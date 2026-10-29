# Gateway monitoring — design

**Spec:** `docs/specs/gateway-monitoring.md` (what and why)
**Status:** proposed, not approved. Nothing here is built yet.

This is one person's architecture, written to be argued with. Every call is
marked as mine; none of it was agreed in conversation.

## Intent

An administrator with several voltd deployments learns that one of them is
in trouble without opening its panel, and without being at the keyboard.

## What the gateway gives us

Checked in the voltd sources rather than assumed:

- `GET /api/logs` — paginated read of the persisted event store.
  Parameters: `severity_min` (a `severity_number`), `hide_internal`,
  `limit`, and a cursor (`until_segment_cursor` + `until_line_offset`).
- Response carries `events`, `at_end`, `incomplete`, `persistence_enabled`
  and `daemon_started_at`.
- `LogEvent`: `id` (UUID v4), `@timestamp` (RFC 3339 UTC, sortable as a
  string), `severity_number`, `severity_text`, `event`, `service`,
  `trace_id`, `span_id`.
- `Severity::number()`: Trace 1, Debug 5, Info 9, **Warn 13, Error 17**.
- Auth: `require_auth` → `Authorization: Bearer <paired session token>`.
- `POST /api/pair` takes `{code, device_name, device_type}` and answers
  `{paired: true, token, …}`.

Three consequences that shape everything below:

1. **`id` makes dedup exact.** No hashing of rendered text.
2. **`daemon_started_at` makes a restart detectable** without another
   endpoint: when it changes, the store rolled and the gateway came back.
3. **`persistence_enabled: false` is a real answer.** A gateway that is not
   persisting must look *unmonitorable*, not healthy.

## The constraint

The pilot's panel binds `127.0.0.1` (`[gateway] host = "127.0.0.1"` in the
kit config, "reachable only from the host itself or through the Volt Admin
SSH tunnel"). So monitoring a remote gateway **requires its tunnel to be
up**. There is no HTTP shortcut. A `Local` profile needs no tunnel.

And today exactly one tunnel can exist: `TunnelState` is
`Mutex<Option<Tunnel>>`, and `connect` drops the previous one.

## The shape I propose

### 1. A connection registry replaces the single tunnel

`TunnelState` becomes `Registry`: profile id → `Connection`, where a
connection owns its tunnel (or nothing, for `Local`), its local port, and
who wants it alive:

```
Connection { profile_id, tunnel: Option<Tunnel>, local_port: u16, held_by: HeldBy }
HeldBy { window: bool, monitor: bool }
```

A connection is torn down when nobody holds it. Monitoring a profile sets
`monitor`; opening its panel sets `window`. Opening the panel of a watched
profile reuses the tunnel that is already up rather than rebuilding it —
which is also why `preferred_port` being stable per profile matters: the
panel's stored session survives.

**My call:** this is the piece that makes everything else possible, and it
is worth doing on its own even if monitoring is dropped — "connect to A,
then to B, then back to A" currently rebuilds tunnels for no reason.

### 2. One process, a tray, and windows that come and go

Monitoring with the window closed means the app outlives the window, which
the current model refuses on purpose ("Multi-copy model: no tray. Closing
the window quits this copy", `lib.rs`).

**My call:** single instance (`tauri-plugin-single-instance`) that owns the
registry, the watchers and the tray; a launch while it is running asks it
for another window instead of starting a second copy. The window stops
being "the app" and becomes a view on one connection. The multi-copy model
survives as *multi-window*: the thing an admin actually wanted was two
panels side by side, not two processes.

Tray menu: each watched profile with its state, "open", and "quit". Quit
must be there or the app becomes unkillable from the UI.

**Rejected:** a separate `--monitor` process in the tray, with the existing
app untouched. It keeps today's model intact and isolates the N-tunnel
work, but then two processes race for the same profile store, the same
stable ports and the same ssh children, and the UI needs cross-process IPC
to show anything. The registry has to exist either way; one process is the
cheaper place to put it.

### 3. A watcher per watched profile

A `tokio` task per profile, started when the watch is switched on, stopped
when it is switched off:

```
loop {
    ensure_connection()      // registry: tunnel up, or Local
    ensure_session_token()   // pair on first run, see §4
    let page = GET /api/logs?severity_min=13&hide_internal=1&limit=50
    classify(page)           // §5
    sleep(interval)          // 60 s, with backoff on failure
}
```

- **Interval:** 60 s, one constant. `tokio::time::interval` with
  `MissedTickBehavior::Delay`, so a laptop that slept fires ONE poll
  instead of one per missed minute. This loses nothing: the events are on
  the gateway, and forty identical requests return the same page. What the
  gap actually costs is covered in §3a.
- **Backoff:** on failure double from 60 s to a 5-minute ceiling, reset on
  the first success.
- **`severity_min=13`** fetches warnings and errors in one request; only
  errors notify (§5). One request, both needs.
- **`limit=50` per page**, one page in steady state. Fifty new events
  inside one minute already means "something is wrong"; the panel is where
  the rest gets read.

### 3a. Coming back after a gap

The store lives on the gateway, so a laptop that slept lost nothing — but
one page of 50 can no longer cover what piled up, and the newest 50 may all
be the same noisy error while the one that matters sits behind them.

So the watcher tracks the `@timestamp` of the newest event it has already
processed (RFC 3339, sortable as a string — no parsing needed to compare).
When a poll comes back **full** (`events.len() == limit`) and its oldest
event is still newer than that mark, there is a backlog: keep paging
backwards with `until_segment_cursor` + `until_line_offset` until one of

- an event older than the mark appears (we have caught up),
- `at_end` is true,
- or **10 pages** have been read.

Hitting the cap, or `incomplete: true` in any page, means the history is
partial — the profile says so instead of pretending the list is complete.

A catch-up never produces a notification per event. It produces one:
"VoltSSH: 137 errors since 14:02, oldest: <text>". The detail is in the
problem list, where it can be read sitting down.

`daemon_started_at` changing during a gap is its own line in that
notification: the gateway restarted while nobody was watching, and the
events before the restart may be gone from the store entirely.

### 4. Pairing as a device, and where the token lives

The app pairs itself exactly as a person would: mint a code with the admin
token (`crate::pair`, already written), then `POST /api/pair` with
`device_name = "Volt Admin — <hostname>"`, `device_type = "monitor"`. The
administrator then sees it in the panel's device list and can revoke it,
which is the property that makes a stored token acceptable at all.

**My call on storage:** the OS keychain (`keyring` crate — Credential
Manager, macOS Keychain, Secret Service), keyed by profile id. Where no
keychain is available, the profile can still be watched but the app says
plainly that its token is stored in a file. Never in `Profile`, never in an
exported document, deleted when the watch is switched off.

A `401` means the token was revoked: stop, mark the profile "re-pair
needed", do not loop trying to pair again.

### 5. What counts as a problem, and when a human is told

| what arrives | what happens |
|---|---|
| event, `severity_number >= 17`, id unseen | counted, and notified |
| event, `severity_number == 13`, id unseen | counted, shown in the app, never notified |
| id already seen | ignored |
| `persistence_enabled: false` | profile shows "cannot be monitored" with the reason, watcher backs off to 10 min |
| `daemon_started_at` changed | one "gateway restarted" notification, seen-ids cleared |
| three consecutive poll failures | one "not answering" notification; one more when it recovers |

**Coalescing:** a notification is raised at most once per minute per
profile. Everything new in that minute becomes one notification: "VoltSSH:
3 errors — <text of the first>". Clicking it opens the window on that
profile's problem list, **not** on the panel: opening the panel means
navigating the webview away from everything else.

**Seen ids:** a bounded ring of the last 500 per profile, persisted with
the watch state so a restart does not re-notify a backlog.

### 6. Modules

```
src-tauri/src/
  registry.rs      Connection, HeldBy, acquire/release, replaces TunnelState
  monitor/
    mod.rs         supervisor: start/stop watchers, own their state
    watcher.rs     one profile's loop (§3)
    client.rs      /api/logs + /api/pair over the connection's local port
    state.rs       counters, seen ids, last error; what the UI reads
    notify.rs      coalescing, the notification itself
```

`client.rs` needs an HTTP client; the app has none today. **My call:**
`reqwest` with `rustls-tls` and `json`, no default TLS stack — one
dependency for something nobody should hand-write. All traffic is to
`127.0.0.1:<local port>` over the tunnel.

The UI learns about changes through Tauri events (`app.emit`), not polling
a command: the watcher already knows when something changed.

### 7. What the user sees

- **Profile card:** a state next to the name — quiet, `N` problems,
  unreachable, or "not monitored". The watch switch lives in the profile
  form.
- **Problem list:** opening a card's state shows the collected events —
  time, severity, text — with "mark read", which clears the count.
- **Tray:** the same states, one line per watched profile.
- **Diagnostics:** the existing section gains the monitor's own last error,
  because "why is it not watching" is the next question after it stops.

## Non-goals, and anti-goals

- Not a monitoring system. No history, no charts, no retention, no
  escalation, no email or webhooks. If the pilot needs those it needs
  Prometheus, not this.
- No acting on a problem — restart, clear, retry. Read-only. The panel is
  one click away and knows how to do all of that properly.
- **Anti-goal:** an app that cries wolf. If an administrator turns
  notifications off, the feature has failed even though it works. That is
  what the warn/error split, the coalescing and the per-minute cap are for,
  and they should be tuned down rather than up.
- **Anti-goal:** monitoring that keeps an SSH connection open for a profile
  whose password lives only in someone's memory. Password-auth profiles
  cannot be watched; the switch is disabled with that reason.

## Build order

Each phase is shippable on its own.

1. **Registry** (§1). Behaviour visible to the user: switching back to a
   profile you already opened no longer rebuilds its tunnel. No monitoring.
2. **Single instance + tray + windows** (§2). Still no monitoring; the app
   simply stops being N processes.
3. **Pairing + client + one watcher, UI only** (§3–§4, §7 without the tray
   and notifications). Problems appear on the cards while the window is
   open.
4. **Notifications** (§5). The feature is now worth having.

If only two phases ever get built, 1 and 3 are the ones with value;
2 and 4 are what make it work unattended.

## Open calls I made that are worth challenging

- A single instance, rather than keeping the multi-copy model and adding a
  separate monitor process.
- The keychain as the default home for session tokens, with a file as the
  visible fallback.
- 60 s, 50 events a page, 10 pages of catch-up, warn-never-notifies. All
  of them are guesses about an administrator's tolerance, and each is one
  constant.
- Clicking a notification opens a problem list rather than the panel.
