//! Watching a gateway: what one poll of `GET /api/logs` means.
//!
//! The gateway serves structured events — an `id`, a severity, a message —
//! so nothing here parses a text log (`docs/specs/gateway-monitoring.md`,
//! verified against a live gateway). What this module decides is what a
//! poll MEANS: which lines are new, which of them are the same problem
//! repeating, and which are worth interrupting someone for.
//!
//! NOTHING HERE LOGS AN EVENT'S TEXT or the token that fetched it. The log
//! carries counts and outcomes (`docs/specs/diagnostics.md`).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::registry::{Holder, Registry};
use crate::tunnel::Lang;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use serde::{Deserialize, Serialize};

use crate::logx;

/// Lowest severity worth waking anyone for: OpenTelemetry's Warn.
pub const SEVERITY_MIN: u8 = 13;
/// ...and Error, which is what a notification is for.
pub const SEVERITY_ERROR: u8 = 17;
/// How many events one poll may bring back.
const LIMIT: u16 = 200;
/// How long the catch-up fetch may take. The STREAM gets no timeout at
/// all — one that expires every 15 seconds is a poll with extra steps.
const TIMEOUT: Duration = Duration::from_secs(15);
/// How long a silent stream is believed. The gateway sends no heartbeat,
/// so a quiet gateway and a dead connection are the same thing from here;
/// reconnecting costs one catch-up fetch, which is cheap enough to do on
/// a schedule rather than wonder.
const QUIET: Duration = Duration::from_secs(5 * 60);
/// How long a tunnel may take to come up before it counts as refused.
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(30);
/// What to wait before trying again, in order, then the last one forever.
/// A gateway restarting is back in seconds; a server that is down is not.
const LADDER: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(5 * 60),
];
/// How long a problem stays said. The same complaint repeats every 15
/// seconds; a failure that comes back after this is news again.
const SAY_AGAIN: Duration = Duration::from_secs(30 * 60);
/// How many ids are remembered for deduplication. A gateway logs forever;
/// this process must not grow with it.
const SEEN_MAX: usize = 1000;

/// One event, as the gateway recorded it.
#[derive(Debug, Clone, Default)]
pub struct Event {
    /// The gateway's id for this OCCURRENCE. Unique per line, so it is what
    /// says "not seen before" — but not what says "a different problem",
    /// which is the message (see `problem`).
    pub id: String,
    pub severity: u8,
    pub message: String,
    /// `service.name` — on the wire `service` is an OBJECT
    /// (`{"name": "zeroclaw", "version": "0.8.7"}`). A fixture here once
    /// assumed a string, and the first live poll answered
    /// "invalid type: map, expected a string" — for the whole page, not
    /// for that field. Hence `read_event` below.
    pub service: String,
}

/// Read one event out of whatever arrived.
///
/// BY HAND, field by field, because a `Deserialize` derive fails the whole
/// page when one field has a type it did not expect — and a watcher that
/// stops watching because voltd changed a field is worse than one that
/// reads a little less of it. An unreadable field is empty here; an
/// unreadable page is silence.
fn read_event(v: &serde_json::Value) -> Event {
    let text = |at: &str| {
        v.pointer(at)
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Event {
        id: text("/id"),
        severity: v
            .get("severity_number")
            .and_then(|s| s.as_u64())
            .unwrap_or(0)
            .min(u8::MAX as u64) as u8,
        message: text("/message"),
        // Both shapes: the object voltd sends, and a bare string if it
        // ever sends one.
        service: match v.get("service") {
            Some(serde_json::Value::String(name)) => name.clone(),
            _ => text("/service/name"),
        },
    }
}

impl Event {
    /// Error or worse. Both are announced — what a failing cron job logs
    /// is a Warn — but errors are what the notification counts first.
    pub fn is_error(&self) -> bool {
        self.severity >= SEVERITY_ERROR
    }
}

/// What one poll answered.
#[derive(Debug, Clone)]
pub struct Poll {
    pub events: Vec<Event>,
    /// False = this gateway keeps no log. There is nothing to watch here,
    /// and silence must not be read as health.
    pub persistence_enabled: bool,
    /// Part of the window was dropped by the gateway's own retention.
    pub incomplete: bool,
}

/// The wire shape. Everything is optional: a field voltd adds or omits
/// later must not turn a poll into a failure.
#[derive(Deserialize)]
struct Answer {
    /// Raw on purpose — see `read_event`.
    #[serde(default)]
    events: Vec<serde_json::Value>,
    #[serde(default)]
    persistence_enabled: bool,
    #[serde(default)]
    incomplete: bool,
}

/// Read one answer. An error here is a BROKEN answer, never an unwelcome
/// one: "this gateway keeps nothing" is a `Poll`, not an `Err`.
pub fn parse_poll(json: &str) -> Result<Poll, String> {
    let a: Answer = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(Poll {
        events: a.events.iter().map(read_event).collect(),
        persistence_enabled: a.persistence_enabled,
        incomplete: a.incomplete,
    })
}

/// The URL of a poll: always the NEWEST page.
///
/// There is a cursor in this API and it is not for this. `until_*` pages
/// BACKWARDS — asked with the cursor the previous answer returned, the
/// gateway hands back the events before it (checked against a live one:
/// 12:49 first, then 12:48, then 12:47). A watcher built on it walks
/// deeper into the past every minute and never sees today. Newness is
/// decided here instead, by event id.
pub fn logs_url(base: &str) -> String {
    // hide_internal=FALSE, and that is the whole point. Tried it the other
    // way against a live gateway: a cron job that failed logged
    // "Scheduler job '' failed" as a Warn of category `internal`, which
    // `hide_internal=true` drops — a watcher that hides the daemon's own
    // problems watches nothing. The noise that flag exists for (the same
    // warning every 15 seconds) is handled below, by message.
    format!("{base}/api/logs?severity_min={SEVERITY_MIN}&hide_internal=false&limit={LIMIT}")
}

/// The live stream of the same events (`text/event-stream`).
///
/// No query at all. Checked against a live gateway: it IGNORES
/// `severity_min` (asked for 17, sent a 13), and `hide_internal=true` it
/// honours — which would hide the daemon's own failures, the one thing
/// this is here to see. Everything is filtered on this side.
pub fn events_url(base: &str) -> String {
    format!("{base}/api/events")
}

/// Server-Sent Events, chunk by chunk.
///
/// A frame ends at a blank line and a chunk ends wherever the network
/// decided, so the two have nothing to do with each other: what is left
/// over stays in the buffer until the rest of it arrives.
#[derive(Default)]
pub struct Frames {
    buffer: String,
}

impl Frames {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `chunk` and take out every frame that is now complete, as the
    /// text its `data:` lines carry.
    pub fn feed(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut out = Vec::new();
        // A frame boundary is a blank line, in either line ending:
        // nothing says a gateway will not send CRLF.
        while let Some(end) = self
            .buffer
            .find(
                "

",
            )
            .map(|i| (i, 2))
            .or_else(|| {
                self.buffer
                    .find(
                        "

",
                    )
                    .map(|i| (i, 4))
            })
        {
            let frame: String = self.buffer.drain(..end.0 + end.1).collect();
            let payload: Vec<&str> = frame
                .lines()
                // `:` is a comment (keep-alives are sent as one), and every
                // other field — `event:`, `id:`, `retry:` — is not data.
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .collect();
            if !payload.is_empty() {
                out.push(payload.join(
                    "
",
                ));
            }
        }
        out
    }
}

/// What makes two events the same PROBLEM: the text, not the id.
///
/// The same complaint arrives every 15 seconds with a fresh id — 38 of 40
/// events in one real poll were one misconfiguration repeating. By id that
/// is 38 new problems; by message it is one, which is also what a person
/// would say looking at the list.
fn problem(e: &Event) -> String {
    format!("{}|{}", e.severity, e.message)
}

/// The occurrences this watcher has not read yet, newest page or not.
pub fn new_occurrences(events: &[Event], seen_ids: &HashSet<String>) -> Vec<Event> {
    events
        .iter()
        .filter(|e| !seen_ids.contains(&e.id))
        .cloned()
        .collect()
}

/// Which of those are worth saying out loud: one per distinct message, and
/// only if that message has not been said within `window`.
///
/// The window is what makes a watcher bearable AND useful: the same
/// complaint arrives every 15 seconds (157 copies in one real window), so
/// saying it once is enough — but a failure that comes back an hour later
/// is news again, and a watcher that said it once in the morning and never
/// again is a watcher that missed it.
pub fn due_problems(
    events: &[Event],
    said: &HashMap<String, Instant>,
    now: Instant,
    window: Duration,
) -> Vec<Event> {
    let mut within = HashSet::new();
    events
        .iter()
        .filter(|e| {
            let fresh = said
                .get(&problem(e))
                .is_none_or(|last| now.duration_since(*last) >= window);
            fresh && within.insert(problem(e))
        })
        .cloned()
        .collect()
}

/// Remember these occurrences, forgetting the oldest once the set is full.
///
/// "Oldest" is approximate on purpose — a `HashSet` has no order, and the
/// alternative (a queue beside it) buys exactness nobody needs: the point
/// is only not to read the same page twice.
pub fn remember_seen(seen_ids: &mut HashSet<String>, events: &[Event]) {
    for e in events {
        if seen_ids.len() >= SEEN_MAX {
            if let Some(victim) = seen_ids.iter().next().cloned() {
                seen_ids.remove(&victim);
            }
        }
        seen_ids.insert(e.id.clone());
    }
}

/// What the UI shows for one watched profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub profile_id: String,
    pub name: String,
    pub errors: usize,
    pub warnings: usize,
    /// Why this profile is not being watched after all — a gateway that
    /// keeps no log, a token nothing would renew. `None` = watching.
    pub trouble: Option<String>,
}

/// A profile whose notification was clicked, waiting for a window to ask
/// for it.
///
/// Not an event alone: clicking a toast with no window open creates one,
/// and the Connect screen is not listening yet when it does. The screen
/// asks for this on mount, and the event covers a window that was already
/// there.
#[derive(Default)]
pub struct Clicked(pub Mutex<Option<String>>);

impl Clicked {
    /// Take it — one click opens one gateway, once.
    pub fn take(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut c| c.take())
    }
}

/// Every watched profile's last known state, for a window that opens later
/// and missed the events.
#[derive(Default)]
pub struct Board(pub Mutex<HashMap<String, Status>>);

impl Board {
    fn put(&self, status: Status) {
        if let Ok(mut board) = self.0.lock() {
            board.insert(status.profile_id.clone(), status);
        }
    }

    fn drop_profile(&self, profile_id: &str) {
        if let Ok(mut board) = self.0.lock() {
            board.remove(profile_id);
        }
    }

    pub fn all(&self) -> Vec<Status> {
        self.0
            .lock()
            .map(|b| b.values().cloned().collect())
            .unwrap_or_default()
    }
}

/// One running watcher, and the gateway it was started for.
pub struct Watcher {
    /// The profile's `connection_fingerprint` when this task began. A
    /// profile can be edited while it is watched, and a watcher still
    /// tunnelling to the host it was started with is worse than none.
    fingerprint: String,
    task: tauri::async_runtime::JoinHandle<()>,
}

/// The running watchers, one task per profile.
#[derive(Default)]
pub struct Watchers {
    tasks: Mutex<HashMap<String, Watcher>>,
    /// One `sync` at a time. Stopping is abort-then-await-then-release, and
    /// two syncs interleaving there could release a hold the other had just
    /// taken.
    gate: tokio::sync::Mutex<()>,
}

/// Which watchers to start and which to stop, by id and the gateway each
/// one is for. Pure, so the rule is tested without starting anything.
///
/// An id in BOTH lists with a different fingerprint is a profile that was
/// edited: it appears in `stop` and in `start`, and the caller does them in
/// that order.
pub fn to_start_and_stop(
    watched: &[(String, String)],
    running: &[(String, String)],
) -> (Vec<String>, Vec<String>) {
    let same =
        |a: &[(String, String)], id: &str, fp: &str| a.iter().any(|(i, f)| i == id && f == fp);
    let start = watched
        .iter()
        .filter(|(id, fp)| !same(running, id, fp))
        .map(|(id, _)| id.clone())
        .collect();
    let stop = running
        .iter()
        .filter(|(id, fp)| !same(watched, id, fp))
        .map(|(id, _)| id.clone())
        .collect();
    (start, stop)
}

/// Bring the running watchers in line with the saved profiles. Called at
/// startup and after every change to a profile — idempotent, so calling it
/// twice starts nothing twice.
pub fn sync<R: Runtime>(app: &AppHandle<R>) {
    // ALL of it in one task, because stopping a watcher has to be ordered:
    // abort it, wait for it to actually be gone, and only then let go of
    // the connection it was holding. Doing the release beside the abort
    // leaves a window where the dying task publishes a tunnel nobody will
    // ever release — or where the release takes the hold a restarted
    // watcher has just claimed.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let watchers = app.state::<Watchers>();
        let _one_at_a_time = watchers.gate.lock().await;

        let profiles = crate::commands::load_profiles(&app).unwrap_or_default();
        let watched: Vec<(String, String)> = profiles
            .iter()
            .filter(|p| p.watch && crate::connection::watchable(p).is_ok())
            .map(|p| (p.id.clone(), p.connection_fingerprint()))
            .collect();
        let running: Vec<(String, String)> = match watchers.tasks.lock() {
            Ok(map) => map
                .iter()
                .map(|(id, w)| (id.clone(), w.fingerprint.clone()))
                .collect(),
            Err(_) => return,
        };
        let (start, stop) = to_start_and_stop(&watched, &running);

        for id in stop {
            let watcher = watchers.tasks.lock().ok().and_then(|mut m| m.remove(&id));
            if let Some(w) = watcher {
                w.task.abort();
                // Gone for certain before anything else touches its
                // connection. (`abort` only asks.)
                let _ = w.task.await;
            }
            app.state::<Board>().drop_profile(&id);
            release_connection(&app, &id).await;
            logx::log("watch", &format!("stopped watching {id}"));
        }

        for id in start {
            let Some(profile) = profiles.iter().find(|p| p.id == id).cloned() else {
                continue;
            };
            let fingerprint = profile.connection_fingerprint();
            let handle = app.clone();
            let task = tauri::async_runtime::spawn(async move {
                run_one(handle, profile).await;
            });
            if let Ok(mut map) = watchers.tasks.lock() {
                map.insert(id.clone(), Watcher { fingerprint, task });
            }
            logx::log("watch", &format!("watching {id}"));
        }
    });
}

/// Take or raise the connection this profile's gateway answers on, held
/// by the watcher itself, and return the local port.
///
/// The SAME entry a window would use (`crate::registry`): a window that
/// connects later JOINS this one instead of raising a second tunnel to the
/// same host, and a window that disconnects leaves it standing because the
/// watcher is still holding it. That is what `Holder::Monitor` was written
/// for in phase 1.
async fn hold_connection<R: Runtime>(
    app: &AppHandle<R>,
    p: &crate::connection::Profile,
) -> Result<u16, String> {
    let registry = app.state::<Registry>();
    // One connect at a time, app-wide: the window's connect takes this
    // same lock, so the two cannot both decide the entry is missing.
    let _connecting = registry.connect_lock().await;
    let fingerprint = p.connection_fingerprint();
    if let Some(held) = registry.hold(&p.id, &fingerprint, Holder::Monitor).await {
        logx::log(
            "watch",
            &format!("{}: joined connection on port {}", p.id, held.local_port),
        );
        return Ok(held.local_port);
    }
    // Local needs no tunnel: the gateway is already on this machine's
    // loopback. The registry entry is still published, so a window joins
    // it the same way.
    if p.mode == crate::connection::ConnMode::Local {
        registry
            .publish(
                &p.id,
                None,
                p.panel_port,
                false,
                &fingerprint,
                Holder::Monitor,
            )
            .await;
        return Ok(p.panel_port);
    }
    let lang = crate::settings::language(app);
    // No password is ever passed: a watched profile authenticates with an
    // agent or a key (`connection::watchable`).
    let opened =
        tokio::time::timeout(TUNNEL_TIMEOUT, crate::tunnel::open_tunnel(p, None, lang)).await;
    let tun = match opened {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return Err(format!("tunnel refused ({})", e.localize(lang))),
        Err(_) => return Err("tunnel hung".to_string()),
    };
    let local_port = tun.local_port;
    let embedded = tun.is_embedded();
    logx::log(
        "watch",
        &format!(
            "{}: tunnel up on port {local_port} embedded={embedded}",
            p.id
        ),
    );
    registry
        .publish(
            &p.id,
            Some(tun),
            local_port,
            embedded,
            &fingerprint,
            Holder::Monitor,
        )
        .await;
    Ok(local_port)
}

/// Let go of it. A window still holding the same connection keeps it.
async fn release_connection<R: Runtime>(app: &AppHandle<R>, profile_id: &str) {
    if app
        .state::<Registry>()
        .release(profile_id, Holder::Monitor)
        .await
    {
        logx::log("watch", &format!("{profile_id}: connection released"));
    }
}

/// The token to poll with: the one that was kept, or a fresh pairing.
async fn obtain_token<R: Runtime>(
    app: &AppHandle<R>,
    p: &crate::connection::Profile,
    base: &str,
) -> Result<String, String> {
    let fingerprint = p.connection_fingerprint();
    if let Some(token) = crate::settings::watch_token(app, &p.id, &fingerprint) {
        return Ok(token);
    }
    let lang = crate::settings::language(app);
    // `embedded = false`: a watched profile uses the system ssh — the
    // built-in client cannot exec, and `watchable` already refuses the
    // auth that would need one.
    let code = crate::pair::mint_code(p, None, false, lang).await?;
    let origin = crate::pair::origin_of(base).ok_or("bad base url")?;
    let token = crate::pair::redeem_code(&origin, &code).await?;
    crate::settings::remember_watch_token(app, &p.id, &token, &fingerprint)?;
    Ok(token)
}

/// One watched gateway, until the watcher is stopped or gives up.
///
/// Catch up, then listen. The catch-up answers "what is wrong right now",
/// including anything that happened while this was disconnected; the
/// stream answers "what is happening" in seconds. Every reconnection does
/// both, which is what makes a dropped stream — a laptop waking, a daemon
/// restarting — cost nothing but a few seconds of lateness.
async fn run_one<R: Runtime>(app: AppHandle<R>, p: crate::connection::Profile) {
    let client = match reqwest::Client::builder().no_proxy().build() {
        Ok(c) => c,
        Err(e) => {
            logx::log("watch", &format!("{}: no http client ({e})", p.id));
            return give_up(&app, &p, Trouble::Broken);
        }
    };
    // Taken once the connection exists, not before: a remote gateway
    // redeems its pairing code THROUGH the tunnel.
    let mut token: Option<String> = None;
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut said: HashMap<String, Instant> = HashMap::new();
    // A revoked token is re-paired ONCE. Twice would be a watcher minting a
    // pairing code every minute against a gateway that keeps refusing it.
    let mut repaired = false;
    let mut rung = 0usize;
    loop {
        // The connection first: for a Local profile this is bookkeeping,
        // for a Remote one it is the tunnel the stream rides on.
        let base = match hold_connection(&app, &p).await {
            Ok(port) => format!("http://127.0.0.1:{port}"),
            Err(why) => {
                logx::log("watch", &format!("{}: {why}", p.id));
                if rung + 1 >= LADDER.len() {
                    mark(&app, &p, Some(Trouble::NoTunnel));
                }
                rung = (rung + 1).min(LADDER.len() - 1);
                tokio::time::sleep(LADDER[rung]).await;
                continue;
            }
        };
        let have = match &token {
            Some(t) => t.clone(),
            None => match obtain_token(&app, &p, &base).await {
                Ok(t) => {
                    token = Some(t.clone());
                    t
                }
                // The REASON stays out of the log and out of the UI: a
                // failed pairing command's error is built from what the
                // command printed, and what `get-paircode` prints is a
                // pairing code (`docs/specs/diagnostics.md`).
                Err(_) => {
                    release_connection(&app, &p.id).await;
                    return give_up(&app, &p, Trouble::NoPairing);
                }
            },
        };
        match attach(&app, &p, &client, &base, &have, &mut seen_ids, &mut said).await {
            Ok(()) => {
                // The stream ended or went quiet. Both are ordinary; try
                // again from the top of the ladder.
                rung = 0;
            }
            Err(Attach::Unauthorized) => {
                if repaired {
                    release_connection(&app, &p.id).await;
                    return give_up(&app, &p, Trouble::TokenRefused);
                }
                repaired = true;
                crate::settings::forget_watch_token(&app, &p.id);
                token = None; // minted again on the next turn of the loop
                continue;
            }
            Err(Attach::NoLog) => {
                release_connection(&app, &p.id).await;
                return give_up(&app, &p, Trouble::NoLog);
            }
            Err(Attach::Unreachable(why)) => {
                logx::log("watch", &format!("{}: {why}", p.id));
                // Only once the whole ladder has been walked: a gateway
                // that restarts is back before the second rung.
                if rung + 1 >= LADDER.len() {
                    mark(&app, &p, Some(Trouble::StreamLost));
                }
                rung = (rung + 1).min(LADDER.len() - 1);
            }
        }
        tokio::time::sleep(LADDER[rung]).await;
    }
}

/// Why one attempt ended.
enum Attach {
    /// The gateway refused the token — it was revoked, or it never was one.
    Unauthorized,
    /// It keeps no log. Nothing to watch here, ever, until someone changes
    /// that — and silence must not be shown as health.
    NoLog,
    /// It did not answer, or stopped answering. Ordinary; try again.
    Unreachable(String),
}

/// One catch-up and one stream, until the stream ends.
async fn attach<R: Runtime>(
    app: &AppHandle<R>,
    p: &crate::connection::Profile,
    client: &reqwest::Client,
    base: &str,
    token: &str,
    seen_ids: &mut HashSet<String>,
    said: &mut HashMap<String, Instant>,
) -> Result<(), Attach> {
    // 1. What is wrong right now, including what was missed.
    let answer = client
        .get(logs_url(base))
        .bearer_auth(token)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| Attach::Unreachable(format!("catch-up failed ({e})")))?;
    if answer.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Attach::Unauthorized);
    }
    let body = answer
        .text()
        .await
        .map_err(|e| Attach::Unreachable(format!("catch-up unreadable ({e})")))?;
    let poll =
        parse_poll(&body).map_err(|e| Attach::Unreachable(format!("catch-up unreadable ({e})")))?;
    if !poll.persistence_enabled {
        return Err(Attach::NoLog);
    }
    if poll.incomplete {
        // The gateway dropped part of its own window (retention): some
        // events never reach anyone, and that is worth knowing when
        // reading the log.
        logx::log("watch", &format!("{}: window incomplete", p.id));
    }
    digest(app, p, &poll.events, seen_ids, said);

    // 2. What is happening, for as long as it keeps happening.
    let stream = client
        .get(events_url(base))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| Attach::Unreachable(format!("stream refused ({e})")))?;
    if stream.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Attach::Unauthorized);
    }
    if !stream.status().is_success() {
        return Err(Attach::Unreachable(format!("stream {}", stream.status())));
    }
    mark(app, p, None);
    let mut stream = stream;
    let mut frames = Frames::new();
    loop {
        // The silence floor, not a request timeout: there is no heartbeat
        // to tell a quiet gateway from a dead connection.
        match tokio::time::timeout(QUIET, stream.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                let text = String::from_utf8_lossy(&bytes);
                let events: Vec<Event> = frames
                    .feed(&text)
                    .iter()
                    .filter_map(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
                    .map(|v| read_event(&v))
                    // The stream carries everything; `severity_min` is
                    // ignored on it, so the floor is applied here.
                    .filter(|e| e.severity >= SEVERITY_MIN)
                    .collect();
                if !events.is_empty() {
                    digest(app, p, &events, seen_ids, said);
                }
            }
            // Ended, failed, or said nothing for five minutes: all three
            // mean "reconnect and catch up", which costs one fetch.
            _ => return Ok(()),
        }
    }
}

/// Decide what in these events is new, what is worth saying, and say it.
fn digest<R: Runtime>(
    app: &AppHandle<R>,
    p: &crate::connection::Profile,
    events: &[Event],
    seen_ids: &mut HashSet<String>,
    said: &mut HashMap<String, Instant>,
) {
    let now = Instant::now();
    let occurrences = new_occurrences(events, seen_ids);
    remember_seen(seen_ids, events);
    let fresh = due_problems(&occurrences, said, now, SAY_AGAIN);
    for e in &fresh {
        said.insert(problem(e), now);
    }
    said.retain(|_, last| now.duration_since(*last) < SAY_AGAIN);
    report(app, p, &fresh);
}

/// Put what this poll found on the board and tell any open window.
///
/// COUNTS ONLY in the log: an event's text is the gateway's business
/// (`docs/specs/diagnostics.md`).
fn report<R: Runtime>(app: &AppHandle<R>, p: &crate::connection::Profile, fresh: &[Event]) {
    let errors = fresh.iter().filter(|e| e.is_error()).count();
    let warnings = fresh.len() - errors;
    let board = app.state::<Board>();
    let previous = board.0.lock().ok().and_then(|b| b.get(&p.id).cloned());
    let status = Status {
        profile_id: p.id.clone(),
        name: p.name.clone(),
        errors: previous.as_ref().map_or(0, |s| s.errors) + errors,
        warnings: previous.as_ref().map_or(0, |s| s.warnings) + warnings,
        trouble: None,
    };
    if errors + warnings > 0 {
        logx::log(
            "watch",
            &format!("{}: {errors} error(s), {warnings} warning(s)", p.id),
        );
    }
    board.put(status.clone());
    let _ = app.emit("watch", status.clone());
    let lang = crate::settings::language(app);
    crate::tray::set_watch_tooltip(app, lang, status.errors, status.warnings);
    announce(app, lang, &p.name, &p.id, fresh);
}

/// ONE notification per poll, however much the poll brought back.
///
/// A laptop that slept for six hours wakes to a backlog, and six hours of
/// trouble must arrive as "17 problems on VoltLocal", not as seventeen
/// toasts.
///
/// WARNINGS COUNT. What a failing cron job logs is a Warn, not an Error
/// (checked on a live gateway), so an `announce` that interrupted only for
/// errors said nothing at all — which is what the first watched build did.
/// `due_problems` has already collapsed the repeats, so everything here is
/// something this watcher has not mentioned lately.
fn announce<R: Runtime>(
    app: &AppHandle<R>,
    lang: Lang,
    profile_name: &str,
    p_id: &str,
    fresh: &[Event],
) {
    let errors = fresh.iter().filter(|e| e.is_error()).count();
    let Some(newest) = fresh
        .iter()
        .find(|e| e.is_error())
        .or_else(|| fresh.first())
    else {
        return;
    };
    let count = if errors > 0 {
        crate::tray::errors_phrase(lang, errors)
    } else {
        crate::tray::warnings_phrase(lang, fresh.len())
    };
    let title = format!("{profile_name}: {count}");
    // The BODY is the gateway's own message — the thing the administrator
    // needs — and it still never reaches our log (see `report`).
    let body = if newest.service.is_empty() {
        newest.message.clone()
    } else {
        format!("{}: {}", newest.service, newest.message)
    };
    match toast(app, &title, &body, p_id) {
        // Counts only, never the text — but DO say that it went out. The
        // first build logged nothing on success, so "no notification
        // appeared" had no evidence on either side of the glass.
        Ok(()) => logx::log(
            "watch",
            &format!("{profile_name}: notified ({})", fresh.len()),
        ),
        Err(e) => logx::log("watch", &format!("notification refused ({e})")),
    }
}

/// Raise one notification.
///
/// On Windows this bypasses `tauri-plugin-notification` for two reasons
/// found the hard way: its `show()` returned success while nothing appeared
/// — no error to read, nothing to chase — and it cannot ask for a toast
/// that STAYS. A watcher exists to be seen by someone who stepped away, so
/// the toast is a reminder: pre-expanded, on screen until dismissed.
#[cfg(windows)]
fn toast<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    body: &str,
    profile_id: &str,
) -> Result<(), String> {
    use tauri_winrt_notification::{Duration as ToastDuration, Scenario, Toast};
    // Clicking it opens that gateway's panel — the next thing the person
    // reading it wants, and the reason they are reading it at all.
    let opener = app.clone();
    let wanted = profile_id.to_string();
    // The AppUserModelID Windows matches against the Start-menu shortcut
    // the installer stamps (see `installer-hooks.nsi`). Missing or wrong,
    // and the toast is dropped in silence.
    Toast::new(&app.config().identifier)
        .title(title)
        .text1(body)
        .scenario(Scenario::Reminder)
        .duration(ToastDuration::Long)
        .on_activated(move |_action| {
            open_for(&opener, &wanted);
            Ok(())
        })
        .show()
        .map_err(|e| e.to_string())
}

/// Hand a window this profile and bring it forward.
///
/// Windows-only for now, because only the Windows toast carries a click
/// back to us — the plugin's notifications on the other platforms do not,
/// and a function nothing calls is a warning the build refuses.
///
/// Both halves are needed: the state for a window that does not exist yet
/// (the Connect screen asks on mount), the event for one that does and is
/// already past its mount.
#[cfg(windows)]
pub fn open_for<R: Runtime>(app: &AppHandle<R>, profile_id: &str) {
    if let Ok(mut clicked) = app.state::<Clicked>().0.lock() {
        *clicked = Some(profile_id.to_string());
    }
    logx::log("watch", &format!("{profile_id}: notification clicked"));
    crate::windows::show_a_window(app);
    let _ = app.emit("watch:open", profile_id.to_string());
}

#[cfg(not(windows))]
fn toast<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    body: &str,
    _profile_id: &str,
) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

/// Why a watcher stopped. A CATEGORY, never an error's text: this reaches
/// both the log and the UI, and the text a failed pairing carries is built
/// from what `get-paircode` printed.
#[derive(Debug, Clone, Copy)]
enum Trouble {
    /// The gateway keeps no log — there is nothing to watch.
    NoLog,
    /// The event stream will not stay up. Not fatal: the watcher keeps
    /// trying, and the card stops saying it the moment one connects.
    StreamLost,
    /// The tunnel will not come up at all. Same shape: kept trying, said
    /// out loud, cleared when it does.
    NoTunnel,
    /// It refuses the pairing we hold, twice.
    TokenRefused,
    /// We could not obtain a session at all.
    NoPairing,
    /// Something on this side: no HTTP client.
    Broken,
}

impl Trouble {
    /// The key the UI looks up (`watch.<key>` in `src/lib/i18n.tsx`).
    fn key(self) -> &'static str {
        match self {
            Self::NoLog => "no-log",
            Self::StreamLost => "stream-lost",
            Self::NoTunnel => "no-tunnel",
            Self::TokenRefused => "token-refused",
            Self::NoPairing => "no-pairing",
            Self::Broken => "broken",
        }
    }
}

/// Say what is wrong with the watching itself — or that nothing is, which
/// matters just as much: a card that keeps saying "tunnel down" after the
/// gateway came back is lying, and this is the only thing that clears it.
///
/// The counts are kept: what the gateway said does not stop being true
/// because the connection to it dropped.
fn mark<R: Runtime>(app: &AppHandle<R>, p: &crate::connection::Profile, trouble: Option<Trouble>) {
    let board = app.state::<Board>();
    let previous = board.0.lock().ok().and_then(|b| b.get(&p.id).cloned());
    let status = Status {
        profile_id: p.id.clone(),
        name: p.name.clone(),
        errors: previous.as_ref().map_or(0, |s| s.errors),
        warnings: previous.as_ref().map_or(0, |s| s.warnings),
        trouble: trouble.map(|t| t.key().to_string()),
    };
    if previous.as_ref().and_then(|s| s.trouble.clone()) == status.trouble {
        return; // nothing changed; the log and the UI do not need to hear it
    }
    logx::log(
        "watch",
        &format!(
            "{}: {}",
            p.id,
            status.trouble.as_deref().unwrap_or("recovered")
        ),
    );
    board.put(status.clone());
    let _ = app.emit("watch", status);
}

/// Stop watching this profile, with a reason the card can show. The task
/// ends here: whatever is wrong needs a person, not another attempt.
fn give_up<R: Runtime>(app: &AppHandle<R>, p: &crate::connection::Profile, trouble: Trouble) {
    logx::log("watch", &format!("{}: gave up ({})", p.id, trouble.key()));
    mark(app, p, Some(trouble));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape a live gateway answered with (see
    /// `docs/specs/gateway-monitoring.md`), trimmed to two events.
    const ANSWER: &str = r#"{
      "events": [
        {"@timestamp":"2026-10-29T23:37:15Z","attributes":{"_line":645,"claimants":[]},
         "event":{"action":"fail","category":"cron","outcome":"failure"},
         "id":"3910219f-e87c-4c74-833d-fadd5a3ed743","message":"tool timed out",
         "schema_version":2,"service":{"name":"zeroclaw","version":"0.8.7"},
         "severity_number":17,"severity_text":"ERROR","zeroclaw":{}},
        {"@timestamp":"2026-10-29T23:36:44Z","attributes":{},
         "event":{"action":"note","category":"internal"},
         "id":"113fadc1-0000-0000-0000-000000000000","message":"slow response",
         "schema_version":2,"service":{"name":"zeroclaw","version":"0.8.7"},
         "severity_number":13,"severity_text":"WARN","zeroclaw":{}}
      ],
      "next_cursor":"seg-7","next_cursor_line_offset":42,"next_segment_cursor":"seg-7",
      "at_end":false,"incomplete":false,"persistence_enabled":true,
      "daemon_started_at":"2026-10-29T21:03:32Z","attribution_keys":["agent"]
    }"#;

    #[test]
    fn a_real_answer_parses_into_events_and_a_cursor() {
        let poll = parse_poll(ANSWER).expect("parses");
        assert_eq!(poll.events.len(), 2);
        assert_eq!(poll.events[0].message, "tool timed out");
        // The shape is the LIVE one, copied off a running gateway: `event`
        // and `service` are objects, `schema_version` a number. A fixture
        // invented from a field list parsed here and failed there.
        assert_eq!(poll.events[0].service, "zeroclaw");
        assert!(poll.events[0].is_error(), "severity 17 is an error");
        assert!(!poll.events[1].is_error(), "severity 13 is a warning");
        assert!(poll.persistence_enabled);
        assert!(!poll.incomplete);
    }

    #[test]
    fn one_surprising_field_does_not_cost_the_whole_page() {
        // What actually happened: `service` is an object, the derive
        // expected a string, and EVERY poll failed with "invalid type: map,
        // expected a string" — the watcher saw nothing at all for as long
        // as that was true. A field it cannot read is empty; the page still
        // arrives.
        let odd = r#"{"events":[
            {"message":"real trouble","severity_number":17,"service":{"name":"zeroclaw"}},
            {"message":{"not":"a string"},"severity_number":17,"service":"plain"},
            {"severity_number":"thirteen"}
        ],"persistence_enabled":true}"#;
        let poll = parse_poll(odd).expect("the page parses");
        assert_eq!(poll.events.len(), 3);
        assert_eq!(poll.events[0].message, "real trouble");
        assert_eq!(poll.events[0].service, "zeroclaw");
        assert!(poll.events[1].message.is_empty(), "unreadable, not fatal");
        assert_eq!(poll.events[1].service, "plain", "a string shape works too");
        assert_eq!(
            poll.events[2].severity, 0,
            "unreadable severity is not an error"
        );
    }

    #[test]
    fn a_gateway_that_keeps_nothing_says_so() {
        // Not an error to parse: it is an answer, and the one answer that
        // means "there is nothing here to watch".
        let poll = parse_poll(r#"{"events":[],"persistence_enabled":false,"at_end":true}"#)
            .expect("parses");
        assert!(!poll.persistence_enabled);
        assert!(poll.events.is_empty());
    }

    #[test]
    fn unknown_fields_and_a_missing_cursor_are_not_a_failure() {
        // voltd may add fields; a poll must not start failing because it did.
        let poll = parse_poll(
            r#"{"events":[],"persistence_enabled":true,"something_new":{"a":1},"at_end":true}"#,
        )
        .expect("parses");
        assert!(poll.events.is_empty());
    }

    #[test]
    fn the_same_page_is_read_only_once() {
        // Every poll asks for the newest page, so most of what comes back
        // has already been read. The id says which lines are new.
        let poll = parse_poll(ANSWER).expect("parses");
        let mut seen = std::collections::HashSet::new();
        assert_eq!(new_occurrences(&poll.events, &seen).len(), 2);
        remember_seen(&mut seen, &poll.events);
        assert!(new_occurrences(&poll.events, &seen).is_empty());
    }

    #[test]
    fn one_complaint_repeated_is_said_once_until_it_goes_quiet() {
        // What a real gateway does: the same misconfiguration every 15
        // seconds, a fresh id each time — 157 copies in one window. One
        // problem, said once.
        let same = |id: &str| Event {
            id: id.into(),
            severity: 13,
            message: "Cron job has no single owning agent".into(),
            service: String::new(),
        };
        let now = Instant::now();
        let mut said = HashMap::new();
        let window = Duration::from_secs(30 * 60);

        let fresh = due_problems(&[same("a"), same("b"), same("c")], &said, now, window);
        assert_eq!(fresh.len(), 1, "three occurrences, one problem");
        said.insert(problem(&fresh[0]), now);

        // Next poll, same complaint, new ids: nothing to say.
        let later = now + Duration::from_secs(60);
        assert!(due_problems(&[same("d")], &said, later, window).is_empty());

        // A different message is a different problem, right away.
        let other = Event {
            message: "Scheduler job failed".into(),
            ..same("e")
        };
        assert_eq!(due_problems(&[other], &said, later, window).len(), 1);

        // And after the window, the old one is news again — a failure that
        // comes back must not be swallowed because it once was mentioned.
        let much_later = now + window + Duration::from_secs(1);
        assert_eq!(
            due_problems(&[same("f")], &said, much_later, window).len(),
            1
        );
    }

    #[test]
    fn a_frame_split_across_chunks_is_read_once() {
        // The ordinary case on a slow link: a frame arrives in pieces.
        let mut frames = Frames::new();
        assert!(frames.feed("data: {\"message\":\"a\"").is_empty());
        assert_eq!(
            frames.feed(
                "}

"
            ),
            vec![r#"{"message":"a"}"#.to_string()]
        );
    }

    #[test]
    fn several_frames_in_one_chunk_all_arrive() {
        let mut frames = Frames::new();
        let got = frames.feed(
            "data: {\"a\":1}

data: {\"b\":2}

",
        );
        assert_eq!(
            got,
            vec![r#"{"a":1}"#.to_string(), r#"{"b":2}"#.to_string()]
        );
    }

    #[test]
    fn a_comment_or_a_blank_line_is_not_an_event() {
        // SSE keep-alives are comments; a watcher that treated one as an
        // event would report an empty problem every time one arrived.
        let mut frames = Frames::new();
        assert!(frames
            .feed(
                ":keep-alive

"
            )
            .is_empty());
        assert!(frames
            .feed(
                "

"
            )
            .is_empty());
        assert!(frames
            .feed(
                "event: ping

"
            )
            .is_empty());
    }

    #[test]
    fn the_stream_url_asks_for_no_filtering() {
        // Checked against a live gateway: `severity_min` is ignored here,
        // and `hide_internal=true` hides the daemon's own failures — the
        // one thing this exists to see.
        assert_eq!(
            events_url("http://127.0.0.1:42617"),
            "http://127.0.0.1:42617/api/events"
        );
    }

    #[test]
    fn a_poll_always_asks_for_the_newest_page() {
        // The API's cursor pages BACKWARDS (checked against a live gateway:
        // the answer after the cursor was a minute older, not newer). A
        // watcher that followed it walks into the past forever, which is
        // exactly what the first build did. Internal events stay visible:
        // a failed cron job is logged as one.
        let url = logs_url("http://127.0.0.1:42617");
        assert_eq!(
            url,
            "http://127.0.0.1:42617/api/logs?severity_min=13&hide_internal=false&limit=200"
        );
        assert!(!url.contains("until_"), "{url}");
    }

    #[test]
    fn remembering_is_bounded() {
        // A gateway that logs forever must not grow this process forever.
        let mut seen = std::collections::HashSet::new();
        for n in 0..1500 {
            let e = Event {
                id: format!("id-{n}"),
                severity: 13,
                message: String::new(),
                service: String::new(),
            };
            remember_seen(&mut seen, std::slice::from_ref(&e));
        }
        assert!(seen.len() <= SEEN_MAX, "{} ids kept", seen.len());
    }

    #[test]
    fn syncing_starts_what_is_watched_and_stops_what_is_not() {
        let pair = |id: &str, fp: &str| (id.to_string(), fp.to_string());
        let watched = [pair("a", "fp1"), pair("b", "fp1")];
        let running = [pair("b", "fp1"), pair("c", "fp1")];
        let (start, stop) = to_start_and_stop(&watched, &running);
        assert_eq!(start, vec!["a".to_string()]);
        assert_eq!(stop, vec!["c".to_string()], "c is no longer watched");
        // Idempotent: syncing twice starts nothing twice.
        let (start, stop) = to_start_and_stop(&watched, &watched);
        assert!(start.is_empty() && stop.is_empty());
    }

    #[test]
    fn an_edited_profile_is_stopped_and_started_again() {
        // The one this exists for: a watched profile pointed at another
        // host (or port) while it ran. The task carries the old address
        // until it is replaced, and a watcher tunnelling to the host it
        // was started with is worse than no watcher at all.
        let pair = |id: &str, fp: &str| (id.to_string(), fp.to_string());
        let watched = [pair("a", "new")];
        let running = [pair("a", "old")];
        let (start, stop) = to_start_and_stop(&watched, &running);
        assert_eq!(start, vec!["a".to_string()]);
        assert_eq!(stop, vec!["a".to_string()], "and in that order");
    }
}
