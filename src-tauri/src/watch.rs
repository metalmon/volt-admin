//! Watching a gateway: what one poll of `GET /api/logs` means.
//!
//! The gateway serves structured events — an `id`, a severity, a message —
//! so nothing here parses a text log (`docs/specs/gateway-monitoring.md`,
//! verified against a live gateway). What this module decides is what a
//! poll MEANS: which events are new, where the next poll starts, and when
//! the cursor has to be thrown away.
//!
//! NOTHING HERE LOGS AN EVENT'S TEXT or the token that fetched it. The log
//! carries counts and outcomes (`docs/specs/diagnostics.md`).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

use crate::tunnel::Lang;

use serde::{Deserialize, Serialize};

use crate::logx;

/// Lowest severity worth waking anyone for: OpenTelemetry's Warn.
pub const SEVERITY_MIN: u8 = 13;
/// ...and Error, which is what a notification is for.
pub const SEVERITY_ERROR: u8 = 17;
/// How many events one poll may bring back.
const LIMIT: u16 = 200;
/// How long between polls. Slow on purpose: this is "tell me within a
/// minute", not a dashboard.
const EVERY: Duration = Duration::from_secs(60);
/// How long one poll may take before it is a failure.
const TIMEOUT: Duration = Duration::from_secs(15);
/// How many ids are remembered for deduplication. A gateway logs forever;
/// this process must not grow with it.
const SEEN_MAX: usize = 1000;

/// One event, as the gateway recorded it.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub id: String,
    #[serde(rename = "severity_number", default)]
    pub severity: u8,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub service: String,
}

impl Event {
    /// Error or worse. Warnings are counted; errors are announced.
    pub fn is_error(&self) -> bool {
        self.severity >= SEVERITY_ERROR
    }
}

/// Where the next poll starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub segment: String,
    pub line_offset: u64,
}

/// What one poll answered.
#[derive(Debug, Clone)]
pub struct Poll {
    pub events: Vec<Event>,
    pub cursor: Option<Cursor>,
    /// False = this gateway keeps no log. There is nothing to watch here,
    /// and silence must not be read as health.
    pub persistence_enabled: bool,
    /// Part of the window was dropped by the gateway's own retention.
    pub incomplete: bool,
    /// Changes when the daemon restarts, which invalidates the cursor.
    pub daemon_started_at: Option<String>,
}

/// The wire shape. Everything is optional: a field voltd adds or omits
/// later must not turn a poll into a failure.
#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    events: Vec<Event>,
    #[serde(default)]
    next_segment_cursor: Option<String>,
    #[serde(default)]
    next_cursor_line_offset: Option<u64>,
    #[serde(default)]
    persistence_enabled: bool,
    #[serde(default)]
    incomplete: bool,
    #[serde(default)]
    daemon_started_at: Option<String>,
}

/// Read one answer. An error here is a BROKEN answer, never an unwelcome
/// one: "this gateway keeps nothing" is a `Poll`, not an `Err`.
pub fn parse_poll(json: &str) -> Result<Poll, String> {
    let a: Answer = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let cursor = a
        .next_segment_cursor
        .filter(|s| !s.is_empty())
        .map(|segment| Cursor {
            segment,
            line_offset: a.next_cursor_line_offset.unwrap_or(0),
        });
    Ok(Poll {
        events: a.events,
        cursor,
        persistence_enabled: a.persistence_enabled,
        incomplete: a.incomplete,
        daemon_started_at: a.daemon_started_at,
    })
}

/// The URL of the next poll. Without a cursor it asks for the newest page,
/// which is also what a restarted daemon gets.
pub fn logs_url(base: &str, cursor: Option<&Cursor>) -> String {
    let mut url =
        format!("{base}/api/logs?severity_min={SEVERITY_MIN}&hide_internal=true&limit={LIMIT}");
    if let Some(c) = cursor {
        url.push_str(&format!(
            "&until_segment_cursor={}&until_line_offset={}",
            escape(&c.segment),
            c.line_offset
        ));
    }
    url
}

/// Percent-encode a cursor for the query string. The cursor is the
/// GATEWAY's string, not ours, and a space or an `&` in it would otherwise
/// rewrite the request.
fn escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Did the daemon restart between these two polls? Only a CHANGE counts: a
/// first poll has nothing to compare, and an answer that cannot say is not
/// evidence of anything.
pub fn fresh_start(prev: Option<&str>, now: Option<&str>) -> bool {
    matches!((prev, now), (Some(a), Some(b)) if a != b)
}

/// The events not seen before, in the order they arrived.
pub fn unseen(events: &[Event], seen: &HashSet<String>) -> Vec<Event> {
    events
        .iter()
        .filter(|e| !seen.contains(&e.id))
        .cloned()
        .collect()
}

/// Remember these ids, forgetting the oldest once the set is full.
///
/// "Oldest" is approximate on purpose — a `HashSet` has no order, and the
/// alternative (a queue beside it) buys exactness nobody needs: the point
/// is not to notify twice about an event that is still on screen.
pub fn remember_seen(seen: &mut HashSet<String>, events: &[Event]) {
    for e in events {
        if seen.len() >= SEEN_MAX {
            if let Some(victim) = seen.iter().next().cloned() {
                seen.remove(&victim);
            }
        }
        seen.insert(e.id.clone());
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

/// The running watchers, one task per profile.
#[derive(Default)]
pub struct Watchers(pub Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>);

/// Which watchers to start and which to stop. Pure, so the rule is tested
/// without starting anything.
pub fn to_start_and_stop(watched: &[String], running: &[String]) -> (Vec<String>, Vec<String>) {
    let start = watched
        .iter()
        .filter(|id| !running.contains(id))
        .cloned()
        .collect();
    let stop = running
        .iter()
        .filter(|id| !watched.contains(id))
        .cloned()
        .collect();
    (start, stop)
}

/// Bring the running watchers in line with the saved profiles. Called at
/// startup and after every change to a profile — idempotent, so calling it
/// twice starts nothing twice.
pub fn sync<R: Runtime>(app: &AppHandle<R>) {
    let profiles = crate::commands::load_profiles(app).unwrap_or_default();
    let watched: Vec<String> = profiles
        .iter()
        .filter(|p| p.watch && crate::connection::watchable(p).is_ok())
        .map(|p| p.id.clone())
        .collect();
    let watchers = app.state::<Watchers>();
    let running: Vec<String> = match watchers.0.lock() {
        Ok(map) => map.keys().cloned().collect(),
        Err(_) => return,
    };
    let (start, stop) = to_start_and_stop(&watched, &running);
    for id in stop {
        if let Ok(mut map) = watchers.0.lock() {
            if let Some(task) = map.remove(&id) {
                task.abort();
            }
        }
        app.state::<Board>().drop_profile(&id);
        logx::log("watch", &format!("stopped watching {id}"));
    }
    for id in start {
        let Some(profile) = profiles.iter().find(|p| p.id == id).cloned() else {
            continue;
        };
        let handle = app.clone();
        let task = tauri::async_runtime::spawn(async move {
            run_one(handle, profile).await;
        });
        if let Ok(mut map) = watchers.0.lock() {
            map.insert(id.clone(), task);
        }
        logx::log("watch", &format!("watching {id}"));
    }
}

/// Where this profile's gateway answers. `Local` only in this slice — a
/// remote one needs a tunnel with no window open (see `watchable`).
fn base_url(p: &crate::connection::Profile) -> String {
    format!("http://127.0.0.1:{}", p.panel_port)
}

/// The token to poll with: the one that was kept, or a fresh pairing.
async fn obtain_token<R: Runtime>(
    app: &AppHandle<R>,
    p: &crate::connection::Profile,
) -> Result<String, String> {
    if let Some(token) = crate::settings::watch_token(app, &p.id) {
        return Ok(token);
    }
    let lang = crate::settings::language(app);
    let code = crate::pair::mint_code(p, None, false, lang).await?;
    let origin = crate::pair::origin_of(&base_url(p)).ok_or("bad base url")?;
    let token = crate::pair::redeem_code(&origin, &code).await?;
    crate::settings::remember_watch_token(app, &p.id, &token)?;
    Ok(token)
}

/// One watched gateway, until the watcher is stopped or gives up.
async fn run_one<R: Runtime>(app: AppHandle<R>, p: crate::connection::Profile) {
    let base = base_url(&p);
    let client = match reqwest::Client::builder().no_proxy().build() {
        Ok(c) => c,
        Err(e) => return give_up(&app, &p, &format!("http client: {e}")),
    };
    let mut token = match obtain_token(&app, &p).await {
        Ok(t) => t,
        Err(e) => return give_up(&app, &p, &e),
    };
    let mut cursor: Option<Cursor> = None;
    let mut seen: HashSet<String> = HashSet::new();
    let mut started_at: Option<String> = None;
    // A revoked token is re-paired ONCE. Twice would be a watcher minting a
    // pairing code every minute against a gateway that keeps refusing it.
    let mut repaired = false;
    loop {
        let answer = client
            .get(logs_url(&base, cursor.as_ref()))
            .bearer_auth(&token)
            .timeout(TIMEOUT)
            .send()
            .await;
        match answer {
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
                if repaired {
                    return give_up(&app, &p, "token-refused");
                }
                repaired = true;
                crate::settings::forget_watch_token(&app, &p.id);
                match obtain_token(&app, &p).await {
                    Ok(t) => token = t,
                    Err(e) => return give_up(&app, &p, &e),
                }
                continue;
            }
            Ok(r) => {
                match r
                    .text()
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|body| parse_poll(&body))
                {
                    Ok(poll) => {
                        if !poll.persistence_enabled {
                            // Nothing to watch here, and silence must never
                            // be shown as health.
                            return give_up(&app, &p, "no-log");
                        }
                        if fresh_start(started_at.as_deref(), poll.daemon_started_at.as_deref()) {
                            // The gateway restarted: the cursor points into
                            // a store that no longer exists.
                            cursor = None;
                            seen.clear();
                        }
                        if poll.incomplete {
                            // The gateway dropped part of its own window
                            // (retention): some events never reach anyone,
                            // and that is worth knowing when reading the log.
                            logx::log("watch", &format!("{}: window incomplete", p.id));
                        }
                        started_at = poll.daemon_started_at.clone();
                        cursor = poll.cursor.clone().or(cursor);
                        let fresh = unseen(&poll.events, &seen);
                        remember_seen(&mut seen, &poll.events);
                        report(&app, &p, &fresh);
                    }
                    // A gateway that is down, or answering something else:
                    // keep the cursor and try again next minute. Not a
                    // reason to stop watching.
                    Err(e) => logx::log("watch", &format!("{}: poll unreadable ({e})", p.id)),
                }
            }
            Err(e) => logx::log("watch", &format!("{}: poll failed ({e})", p.id)),
        }
        tokio::time::sleep(EVERY).await;
    }
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
    announce(app, lang, &p.name, fresh);
}

/// ONE notification per poll, however much the poll brought back.
///
/// That is the whole point of the cursor: a laptop that slept for six hours
/// wakes to a backlog, and six hours of errors must arrive as "17 errors on
/// VoltLocal", not as seventeen notifications.
fn announce<R: Runtime>(app: &AppHandle<R>, lang: Lang, profile_name: &str, fresh: &[Event]) {
    let errors: Vec<&Event> = fresh.iter().filter(|e| e.is_error()).collect();
    let Some(newest) = errors.first() else {
        return; // warnings go to the tray; only errors interrupt
    };
    let title = match lang {
        Lang::Ru => format!(
            "{profile_name}: {}",
            crate::tray::errors_phrase(lang, errors.len())
        ),
        Lang::En => format!(
            "{profile_name}: {}",
            crate::tray::errors_phrase(lang, errors.len())
        ),
    };
    // The BODY is the gateway's own message — the thing the administrator
    // needs — and it still never reaches our log (see `report`).
    let body = if newest.service.is_empty() {
        newest.message.clone()
    } else {
        format!("{}: {}", newest.service, newest.message)
    };
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        logx::log("watch", &format!("notification refused ({e})"));
    }
}

/// Stop watching this profile, with a reason the card can show. The task
/// ends here: whatever is wrong needs a person, not another poll.
fn give_up<R: Runtime>(app: &AppHandle<R>, p: &crate::connection::Profile, trouble: &str) {
    logx::log("watch", &format!("{}: gave up ({trouble})", p.id));
    let status = Status {
        profile_id: p.id.clone(),
        name: p.name.clone(),
        errors: 0,
        warnings: 0,
        trouble: Some(trouble.to_string()),
    };
    app.state::<Board>().put(status.clone());
    let _ = app.emit("watch", status);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape a live gateway answered with (see
    /// `docs/specs/gateway-monitoring.md`), trimmed to two events.
    const ANSWER: &str = r#"{
      "events": [
        {"@timestamp":"2026-10-29T23:37:15Z","attributes":{"agent":"ops"},"event":"tool.call",
         "id":"e0ef98cd","message":"tool timed out","schema_version":"1",
         "service":"voltd","severity_number":17,"severity_text":"ERROR","zeroclaw":{}},
        {"@timestamp":"2026-10-29T23:36:44Z","attributes":{},"event":"gateway.request",
         "id":"113fadc1","message":"slow response","schema_version":"1",
         "service":"voltd","severity_number":13,"severity_text":"WARN","zeroclaw":{}}
      ],
      "next_cursor":"seg-7","next_cursor_line_offset":42,"next_segment_cursor":"seg-7",
      "at_end":false,"incomplete":false,"persistence_enabled":true,
      "daemon_started_at":"2026-10-29T21:03:32Z","attribution_keys":["agent"]
    }"#;

    #[test]
    fn a_real_answer_parses_into_events_and_a_cursor() {
        let poll = parse_poll(ANSWER).expect("parses");
        assert_eq!(poll.events.len(), 2);
        assert_eq!(poll.events[0].id, "e0ef98cd");
        assert_eq!(poll.events[0].message, "tool timed out");
        assert_eq!(poll.events[0].service, "voltd");
        assert!(poll.events[0].is_error(), "severity 17 is an error");
        assert!(!poll.events[1].is_error(), "severity 13 is a warning");
        assert_eq!(
            poll.cursor,
            Some(Cursor {
                segment: "seg-7".into(),
                line_offset: 42
            })
        );
        assert!(poll.persistence_enabled);
        assert!(!poll.incomplete);
        assert_eq!(
            poll.daemon_started_at.as_deref(),
            Some("2026-10-29T21:03:32Z")
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
        assert_eq!(poll.cursor, None);
    }

    #[test]
    fn unknown_fields_and_a_missing_cursor_are_not_a_failure() {
        // voltd may add fields; a poll must not start failing because it did.
        let poll = parse_poll(
            r#"{"events":[],"persistence_enabled":true,"something_new":{"a":1},"at_end":true}"#,
        )
        .expect("parses");
        assert_eq!(poll.cursor, None);
        assert!(poll.daemon_started_at.is_none());
    }

    #[test]
    fn a_restarted_daemon_invalidates_the_cursor() {
        assert!(fresh_start(Some("a"), Some("b")));
        assert!(!fresh_start(Some("a"), Some("a")));
        assert!(!fresh_start(None, Some("a")), "first poll is not a restart");
        assert!(
            !fresh_start(Some("a"), None),
            "an answer that cannot say is not a restart"
        );
    }

    #[test]
    fn the_same_event_is_new_only_once() {
        let poll = parse_poll(ANSWER).expect("parses");
        let mut seen = std::collections::HashSet::new();
        assert_eq!(unseen(&poll.events, &seen).len(), 2);
        remember_seen(&mut seen, &poll.events);
        assert!(unseen(&poll.events, &seen).is_empty());
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
        let watched = ["a".to_string(), "b".to_string()];
        let running = ["b".to_string(), "c".to_string()];
        let (start, stop) = to_start_and_stop(&watched, &running);
        assert_eq!(start, vec!["a".to_string()]);
        assert_eq!(stop, vec!["c".to_string()], "c is no longer watched");
        // Idempotent: syncing twice starts nothing twice.
        let (start, stop) = to_start_and_stop(&watched, &watched);
        assert!(start.is_empty() && stop.is_empty());
    }

    #[test]
    fn the_url_carries_the_cursor_only_when_there_is_one() {
        let first = logs_url("http://127.0.0.1:42617", None);
        assert_eq!(
            first,
            "http://127.0.0.1:42617/api/logs?severity_min=13&hide_internal=true&limit=200"
        );
        let next = logs_url(
            "http://127.0.0.1:42617",
            Some(&Cursor {
                segment: "seg 7".into(),
                line_offset: 42,
            }),
        );
        assert!(next.contains("until_line_offset=42"), "{next}");
        assert!(
            next.contains("until_segment_cursor=seg%207"),
            "a cursor is the gateway's string, not ours: {next}"
        );
    }
}
