//! Connections to gateways, keyed by profile, each kept alive by whoever
//! still wants it.
//!
//! This replaces the single `TunnelState`: more than one connection can
//! exist at a time, because monitoring several gateways means several
//! tunnels (see `docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md`).
//!
//! A connection lives while at least one [`Holder`] holds it. Removing the
//! last holder removes the entry, which drops its [`Tunnel`] — and the
//! `Tunnel`'s own `Drop` kills the ssh child. Nothing here needs to kill a
//! process itself; it only needs to never leak a `Tunnel` it removed.
//!
//! That is the RUNNING app. Quitting is not this module's job and does not
//! go through here at all: `Builder::run` exits the process without running
//! destructors, and the children are reaped by the OS-level guard they get
//! at spawn (`crate::tunnel::child_guard`).
//!
//! A `Local` profile is an ordinary entry with `tunnel: None`: there is no
//! ssh child, but there is still a connection to hold, so nothing
//! downstream has to special-case the mode.

use std::collections::{HashMap, HashSet};

use tokio::sync::Mutex;

use crate::tunnel::Tunnel;

/// Who wants a connection to stay up.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Holder {
    /// A window is showing this profile's panel, named by its Tauri label:
    /// two windows on one profile are two holds, and closing one must not
    /// take the panel away from the other.
    Window(String),
    /// A watcher is polling this profile's gateway. Nothing sets this yet —
    /// it exists so the lifetime rule is written once, in one place, which
    /// is also why the compiler is told not to mind.
    #[allow(dead_code)]
    Monitor,
}

/// What [`Registry::hold`] tells a caller about a connection it joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    /// Where the panel answers on this machine.
    pub local_port: u16,
    /// The tunnel runs on the built-in ssh client, which cannot exec — see
    /// `crate::pair`. Always false for `Local`.
    pub embedded: bool,
}

/// One live connection to a gateway.
pub struct Connection {
    /// `None` for `Local` profiles, which reach the panel on loopback.
    ///
    /// Never read, and that is the point: the field OWNS the tunnel, and
    /// dropping it is what kills the ssh child.
    #[allow(dead_code)]
    pub tunnel: Option<Tunnel>,
    /// Where the panel answers on this machine.
    pub local_port: u16,
    /// Carried so a REUSED connection still reports how it is served:
    /// pairing and the admin-token read branch on it.
    embedded: bool,
    /// [`crate::connection::Profile::connection_fingerprint`] as it was when
    /// this connection was opened. A profile edited since then is a
    /// different connection, however the same its id.
    fingerprint: String,
    holders: HashSet<Holder>,
    /// Test-only payload, so a test can prove a `Connection` really was
    /// dropped without building a `Tunnel` (which would spawn `ssh`).
    /// Never read, like `tunnel`: being dropped is the whole job.
    #[cfg(test)]
    #[allow(dead_code)]
    probe: Option<std::sync::Arc<()>>,
}

/// Tauri-managed state: every connection this app currently holds open.
///
/// The second mutex serializes `connect`: publishing a connection and
/// releasing the previous one must be one atomic step, or two connects
/// racing each other release what the other just published.
#[derive(Default)]
pub struct Registry(Mutex<HashMap<String, Connection>>, Mutex<()>);

impl Registry {
    /// Add `holder` to an existing connection and return where its panel
    /// answers. `None` means there is no connection for `profile_id` yet and
    /// the caller has to open one.
    pub async fn hold(&self, profile_id: &str, fingerprint: &str, holder: Holder) -> Option<Held> {
        let mut map = self.0.lock().await;
        let conn = map.get_mut(profile_id)?;
        if conn.fingerprint != fingerprint {
            // The profile was edited while this connection was open: it
            // would send traffic to the old host under the new host's name.
            return None;
        }
        conn.holders.insert(holder);
        Some(Held {
            local_port: conn.local_port,
            embedded: conn.embedded,
        })
    }

    /// Hold this for the whole of a `connect`. Two connects that overlap
    /// would otherwise each publish and then release the other's connection,
    /// killing an ssh child the other has already handed to the webview.
    pub async fn connect_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.1.lock().await
    }

    /// Register a connection that is ready to use, held by `holder`. A
    /// previous entry for this profile is replaced — but its OTHER holders
    /// are carried over, or a window connecting to a profile a watcher is
    /// already holding would drop the watcher's claim.
    pub async fn publish(
        &self,
        profile_id: &str,
        tunnel: Option<Tunnel>,
        local_port: u16,
        embedded: bool,
        fingerprint: &str,
        holder: Holder,
    ) {
        let mut map = self.0.lock().await;
        let mut holders = map
            .get(profile_id)
            .map(|c| c.holders.clone())
            .unwrap_or_default();
        holders.insert(holder);
        map.insert(
            profile_id.to_string(),
            Connection {
                tunnel,
                local_port,
                embedded,
                fingerprint: fingerprint.to_string(),
                holders,
                #[cfg(test)]
                probe: None,
            },
        );
    }

    /// Drop `holder`'s claim. Returns whether that was the last one, in
    /// which case the connection itself is gone.
    ///
    /// Unused until a watcher can hold a connection; `release_all` is what
    /// the window uses today.
    #[allow(dead_code)]
    pub async fn release(&self, profile_id: &str, holder: Holder) -> bool {
        let mut map = self.0.lock().await;
        let Some(conn) = map.get_mut(profile_id) else {
            return false;
        };
        conn.holders.remove(&holder);
        if conn.holders.is_empty() {
            map.remove(profile_id);
            return true;
        }
        false
    }

    /// Release one holder's claim everywhere — how a window that is closing
    /// lets go of whatever it was showing.
    pub async fn release_all_for(&self, holder: &Holder) -> Vec<String> {
        self.release_matching(move |h| h == holder, |_| true).await
    }

    /// [`Self::release_all`], except for `keep` — how a window moving from
    /// one profile to another lets go of the first without touching the one
    /// it just arrived at.
    pub async fn release_others(&self, holder: Holder, keep: &str) -> Vec<String> {
        let keep = keep.to_string();
        self.release_matching(move |h| *h == holder, move |id| id != keep)
            .await
    }

    /// Drop every holder matching `holder_matches` from every connection
    /// matching `which`, and remove the connections left with none.
    async fn release_matching(
        &self,
        holder_matches: impl Fn(&Holder) -> bool,
        which: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let mut map = self.0.lock().await;
        let mut dropped = Vec::new();
        map.retain(|id, conn| {
            if !which(id) {
                return true;
            }
            conn.holders.retain(|h| !holder_matches(h));
            if conn.holders.is_empty() {
                dropped.push(id.clone());
                return false;
            }
            true
        });
        dropped
    }

    /// [`Self::publish`] carrying a payload the test can watch die with the
    /// connection.
    #[cfg(test)]
    async fn publish_with_probe(
        &self,
        profile_id: &str,
        local_port: u16,
        holder: Holder,
        probe: std::sync::Arc<()>,
    ) {
        let mut map = self.0.lock().await;
        map.insert(
            profile_id.to_string(),
            Connection {
                tunnel: None,
                local_port,
                embedded: false,
                fingerprint: String::new(),
                holders: HashSet::from([holder]),
                probe: Some(probe),
            },
        );
    }

    /// Where this profile's panel answers, if it is connected. For the
    /// watcher that will poll it.
    #[allow(dead_code)]
    pub async fn local_port(&self, profile_id: &str) -> Option<u16> {
        self.0.lock().await.get(profile_id).map(|c| c.local_port)
    }

    /// How many connections are open.
    #[allow(dead_code)]
    pub async fn len(&self) -> usize {
        self.0.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test publishes `tunnel: None`, so none of them spawns `ssh`:
    /// the holder rules are what is under test, not the tunnel.
    fn window(label: &str) -> Holder {
        Holder::Window(label.to_string())
    }

    async fn registry_with(ids: &[&str]) -> Registry {
        let r = Registry::default();
        for (i, id) in ids.iter().enumerate() {
            r.publish(id, None, 9000 + i as u16, false, "fp", window("w1"))
                .await;
        }
        r
    }

    #[tokio::test]
    async fn hold_on_an_unknown_profile_is_none() {
        let r = Registry::default();
        assert!(r.hold("nope", "fp", window("w1")).await.is_none());
    }

    #[tokio::test]
    async fn publish_then_hold_returns_the_local_port() {
        let r = registry_with(&["a"]).await;
        assert_eq!(
            r.hold("a", "fp", Holder::Monitor)
                .await
                .map(|h| h.local_port),
            Some(9000)
        );
        assert_eq!(r.local_port("a").await, Some(9000));
    }

    #[tokio::test]
    async fn two_holders_keep_the_connection_until_both_let_go() {
        let r = registry_with(&["a"]).await;
        r.hold("a", "fp", Holder::Monitor).await;
        assert!(!r.release("a", window("w1")).await, "monitor still holds");
        assert_eq!(r.len().await, 1);
        assert!(r.release("a", Holder::Monitor).await, "last holder");
        assert_eq!(r.len().await, 0);
    }

    #[tokio::test]
    async fn release_drops_the_connection_when_it_was_the_last_holder() {
        let r = registry_with(&["a"]).await;
        assert!(r.release("a", window("w1")).await);
        assert_eq!(r.local_port("a").await, None);
    }

    #[tokio::test]
    async fn releasing_a_holder_that_is_not_held_is_a_no_op() {
        let r = registry_with(&["a"]).await;
        assert!(!r.release("a", Holder::Monitor).await, "was never held");
        assert_eq!(r.len().await, 1, "and the connection stays");
        assert!(!r.release("ghost", window("w1")).await, "no such profile");
    }

    #[tokio::test]
    async fn releasing_a_window_reports_every_id_it_dropped() {
        let r = registry_with(&["a", "b"]).await;
        let mut dropped = r.release_all_for(&window("w1")).await;
        dropped.sort();
        assert_eq!(dropped, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(r.len().await, 0);
    }

    #[tokio::test]
    async fn release_others_keeps_the_named_profile() {
        let r = registry_with(&["a", "b"]).await;
        let dropped = r.release_others(window("w1"), "b").await;
        assert_eq!(dropped, vec!["a".to_string()]);
        assert_eq!(r.local_port("b").await, Some(9001));
    }

    #[tokio::test]
    async fn two_windows_on_one_profile_both_have_to_let_go() {
        let r = registry_with(&["a"]).await;
        r.hold("a", "fp", window("w2")).await;
        assert!(!r.release("a", window("w1")).await, "w2 still has it");
        assert!(r.release("a", window("w2")).await, "last window");
        assert_eq!(r.len().await, 0);
    }

    #[tokio::test]
    async fn releasing_one_window_leaves_another_windows_profile_alone() {
        let r = Registry::default();
        r.publish("a", None, 9000, false, "fp", window("w1")).await;
        r.publish("b", None, 9001, false, "fp", window("w2")).await;
        assert!(r.release("a", window("w1")).await);
        assert_eq!(r.local_port("b").await, Some(9001), "w2 untouched");
    }

    #[tokio::test]
    async fn release_others_is_scoped_to_one_window() {
        let r = Registry::default();
        r.publish("a", None, 9000, false, "fp", window("w1")).await;
        r.publish("b", None, 9001, false, "fp", window("w1")).await;
        r.hold("b", "fp", window("w2")).await;
        let dropped = r.release_others(window("w1"), "b").await;
        assert_eq!(dropped, vec!["a".to_string()]);
        assert_eq!(r.local_port("b").await, Some(9001), "w2 still holds b");
    }

    #[tokio::test]
    async fn a_closing_window_leaves_a_monitor_hold_standing() {
        let r = Registry::default();
        r.publish("a", None, 9000, false, "fp", window("w1")).await;
        r.hold("a", "fp", Holder::Monitor).await;
        r.publish("b", None, 9001, false, "fp", window("w1")).await;
        let dropped = r.release_all_for(&window("w1")).await;
        assert_eq!(dropped, vec!["b".to_string()], "only b had no other holder");
        assert_eq!(r.local_port("a").await, Some(9000), "the monitor keeps a");
    }

    #[tokio::test]
    async fn a_profile_edited_since_the_connection_opened_is_not_reused() {
        // Same id, different host: reusing would send traffic — and a
        // pairing code minted for it — to the old host under the new name.
        let r = Registry::default();
        r.publish("a", None, 9000, false, "old-host", window("w1"))
            .await;
        assert!(r.hold("a", "new-host", window("w2")).await.is_none());
        assert!(r.hold("a", "old-host", window("w2")).await.is_some());
    }

    #[tokio::test]
    async fn publish_keeps_a_holder_that_is_already_there() {
        // A watcher holding a connection must survive the window connecting
        // to the same profile, or phase 3 kills its own tunnel.
        let r = registry_with(&["a"]).await;
        r.hold("a", "fp", Holder::Monitor).await;
        r.publish("a", None, 9100, false, "fp", window("w1")).await;
        assert!(
            !r.release("a", window("w1")).await,
            "the monitor hold must still be there"
        );
        assert_eq!(r.len().await, 1);
    }

    #[tokio::test]
    async fn hold_reports_whether_the_tunnel_is_embedded() {
        // `embedded` decides whether pairing can exec on the far side
        // (`crate::pair`); a reused connection must not look like a system
        // ssh when it is the built-in client.
        let r = Registry::default();
        r.publish("a", None, 9000, true, "fp", window("w1")).await;
        let held = r.hold("a", "fp", window("w1")).await.expect("held");
        assert_eq!(held.local_port, 9000);
        assert!(held.embedded, "the flag must survive the reuse path");
    }

    #[tokio::test]
    async fn the_connect_lock_serializes_two_callers() {
        // Two connects racing used to release each other's connection; the
        // lock is what makes "publish, then release the others" atomic.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let registry = std::sync::Arc::new(Registry::default());
        let inside = std::sync::Arc::new(AtomicUsize::new(0));
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let (r, inside, peak) = (registry.clone(), inside.clone(), peak.clone());
            tasks.push(tokio::spawn(async move {
                let _guard = r.connect_lock().await;
                let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                inside.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1, "two connects overlapped");
    }

    #[tokio::test]
    async fn dropping_the_registry_drops_every_connection() {
        // Releasing a connection must really drop it, not just forget it in
        // a map that keeps growing — the `Tunnel` inside is what holds an
        // ssh child. A real `Tunnel` cannot be built in a test, so the proof
        // is a payload whose `Weak` dies with its `Connection`. (App EXIT is
        // a different mechanism entirely — see the module docs.)
        let payloads: Vec<std::sync::Arc<()>> = (0..3).map(|_| std::sync::Arc::new(())).collect();
        let weak: Vec<std::sync::Weak<()>> =
            payloads.iter().map(std::sync::Arc::downgrade).collect();
        let registry = Registry::default();
        for (i, payload) in payloads.into_iter().enumerate() {
            registry
                .publish_with_probe(&format!("p{i}"), 9000 + i as u16, window("w1"), payload)
                .await;
        }
        assert_eq!(registry.len().await, 3);
        drop(registry);
        assert!(
            weak.iter().all(|w| w.upgrade().is_none()),
            "a connection outlived the registry"
        );
    }

    #[tokio::test]
    async fn publish_twice_for_one_id_leaves_one_entry() {
        let r = registry_with(&["a"]).await;
        r.publish("a", None, 9100, false, "fp", window("w1")).await;
        assert_eq!(r.len().await, 1);
        assert_eq!(r.local_port("a").await, Some(9100), "the newer one wins");
    }
}
