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
//! A `Local` profile is an ordinary entry with `tunnel: None`: there is no
//! ssh child, but there is still a connection to hold, so nothing
//! downstream has to special-case the mode.

use std::collections::{HashMap, HashSet};

use tokio::sync::Mutex;

use crate::tunnel::Tunnel;

/// Who wants a connection to stay up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Holder {
    /// A window is showing this profile's panel.
    Window,
    /// A watcher is polling this profile's gateway. Nothing sets this yet —
    /// it exists so the lifetime rule is written once, in one place.
    Monitor,
}

/// One live connection to a gateway.
pub struct Connection {
    /// `None` for `Local` profiles, which reach the panel on loopback.
    pub tunnel: Option<Tunnel>,
    /// Where the panel answers on this machine.
    pub local_port: u16,
    holders: HashSet<Holder>,
}

/// Tauri-managed state: every connection this app currently holds open.
#[derive(Default)]
pub struct Registry(Mutex<HashMap<String, Connection>>);

impl Registry {
    /// Add `holder` to an existing connection and return where its panel
    /// answers. `None` means there is no connection for `profile_id` yet and
    /// the caller has to open one.
    pub async fn hold(&self, profile_id: &str, holder: Holder) -> Option<u16> {
        let mut map = self.0.lock().await;
        let conn = map.get_mut(profile_id)?;
        conn.holders.insert(holder);
        Some(conn.local_port)
    }

    /// Register a connection that is ready to use, held by `holder`. Any
    /// previous entry for this profile is replaced, and dropped with it.
    pub async fn publish(
        &self,
        profile_id: &str,
        tunnel: Option<Tunnel>,
        local_port: u16,
        holder: Holder,
    ) {
        let mut map = self.0.lock().await;
        map.insert(
            profile_id.to_string(),
            Connection {
                tunnel,
                local_port,
                holders: HashSet::from([holder]),
            },
        );
    }

    /// Drop `holder`'s claim. Returns whether that was the last one, in
    /// which case the connection itself is gone.
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

    /// [`Self::release`] for every profile. Returns the ids whose connection
    /// was dropped.
    pub async fn release_all(&self, holder: Holder) -> Vec<String> {
        self.release_where(holder, |_| true).await
    }

    /// [`Self::release_all`], except for `keep` — how a window moving from
    /// one profile to another lets go of the first without touching the one
    /// it just arrived at.
    pub async fn release_others(&self, holder: Holder, keep: &str) -> Vec<String> {
        self.release_where(holder, |id| id != keep).await
    }

    async fn release_where(&self, holder: Holder, which: impl Fn(&str) -> bool) -> Vec<String> {
        let mut map = self.0.lock().await;
        let mut dropped = Vec::new();
        map.retain(|id, conn| {
            if !which(id) {
                return true;
            }
            conn.holders.remove(&holder);
            if conn.holders.is_empty() {
                dropped.push(id.clone());
                return false;
            }
            true
        });
        dropped
    }

    /// Where this profile's panel answers, if it is connected.
    pub async fn local_port(&self, profile_id: &str) -> Option<u16> {
        self.0.lock().await.get(profile_id).map(|c| c.local_port)
    }

    /// How many connections are open.
    pub async fn len(&self) -> usize {
        self.0.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test publishes `tunnel: None`, so none of them spawns `ssh`:
    /// the holder rules are what is under test, not the tunnel.
    async fn registry_with(ids: &[&str]) -> Registry {
        let r = Registry::default();
        for (i, id) in ids.iter().enumerate() {
            r.publish(id, None, 9000 + i as u16, Holder::Window).await;
        }
        r
    }

    #[tokio::test]
    async fn hold_on_an_unknown_profile_is_none() {
        let r = Registry::default();
        assert_eq!(r.hold("nope", Holder::Window).await, None);
    }

    #[tokio::test]
    async fn publish_then_hold_returns_the_local_port() {
        let r = registry_with(&["a"]).await;
        assert_eq!(r.hold("a", Holder::Monitor).await, Some(9000));
        assert_eq!(r.local_port("a").await, Some(9000));
    }

    #[tokio::test]
    async fn two_holders_keep_the_connection_until_both_let_go() {
        let r = registry_with(&["a"]).await;
        r.hold("a", Holder::Monitor).await;
        assert!(!r.release("a", Holder::Window).await, "monitor still holds");
        assert_eq!(r.len().await, 1);
        assert!(r.release("a", Holder::Monitor).await, "last holder");
        assert_eq!(r.len().await, 0);
    }

    #[tokio::test]
    async fn release_drops_the_connection_when_it_was_the_last_holder() {
        let r = registry_with(&["a"]).await;
        assert!(r.release("a", Holder::Window).await);
        assert_eq!(r.local_port("a").await, None);
    }

    #[tokio::test]
    async fn releasing_a_holder_that_is_not_held_is_a_no_op() {
        let r = registry_with(&["a"]).await;
        assert!(!r.release("a", Holder::Monitor).await, "was never held");
        assert_eq!(r.len().await, 1, "and the connection stays");
        assert!(!r.release("ghost", Holder::Window).await, "no such profile");
    }

    #[tokio::test]
    async fn release_all_reports_every_id_it_dropped() {
        let r = registry_with(&["a", "b"]).await;
        let mut dropped = r.release_all(Holder::Window).await;
        dropped.sort();
        assert_eq!(dropped, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(r.len().await, 0);
    }

    #[tokio::test]
    async fn release_others_keeps_the_named_profile() {
        let r = registry_with(&["a", "b"]).await;
        let dropped = r.release_others(Holder::Window, "b").await;
        assert_eq!(dropped, vec!["a".to_string()]);
        assert_eq!(r.local_port("b").await, Some(9001));
    }

    #[tokio::test]
    async fn publish_twice_for_one_id_leaves_one_entry() {
        let r = registry_with(&["a"]).await;
        r.publish("a", None, 9100, Holder::Window).await;
        assert_eq!(r.len().await, 1);
        assert_eq!(r.local_port("a").await, Some(9100), "the newer one wins");
    }
}
