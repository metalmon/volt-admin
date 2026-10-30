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

// Nothing calls these yet: the loop that does lives in this same file and
// lands with the watcher (`docs/superpowers/plans/2026-10-30-gateway-watch.md`,
// Task 3). Until then the pure half is reachable only from its tests.
#![allow(dead_code)]

use std::collections::HashSet;

use serde::Deserialize;

/// Lowest severity worth waking anyone for: OpenTelemetry's Warn.
pub const SEVERITY_MIN: u8 = 13;
/// ...and Error, which is what a notification is for.
pub const SEVERITY_ERROR: u8 = 17;
/// How many events one poll may bring back.
const LIMIT: u16 = 200;
/// How many ids are remembered for deduplication. A gateway logs forever;
/// this process must not grow with it.
const SEEN_MAX: usize = 1000;

/// One event, as the gateway recorded it.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub id: String,
    #[serde(rename = "@timestamp", default)]
    pub at: String,
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
                at: String::new(),
                severity: 13,
                message: String::new(),
                service: String::new(),
            };
            remember_seen(&mut seen, std::slice::from_ref(&e));
        }
        assert!(seen.len() <= SEEN_MAX, "{} ids kept", seen.len());
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
