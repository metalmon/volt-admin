# Spec: watching the gateways, not the launcher

**Status:** draft — the architecture is in
`docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md`
**Owner:** launcher (new `monitor` module, tray, notifications)

## Problem

An administrator runs several voltd deployments. Today Volt Admin shows them
one at a time and only while someone is looking: `connect` opens a tunnel to
one profile, tears down whatever was open before, and points the window at
that panel. Everything the gateway reports — a failing agent, a provider
that stopped answering, a channel that will not bind — is visible only to
whoever opens that panel and scrolls.

So failures are found by someone noticing, which does not scale past a
couple of servers and does not happen at night at all.

This is not about the launcher's own errors. Those are already on screen and
in `docs/specs/diagnostics.md`.

## Why this exists at all, when voltd can already export

voltd ships the usual ways out: `[observability] backend` takes
`none | log | verbose | prometheus | otel`. With `otel` it pushes to any
OTLP collector (`otel_endpoint`, service name, auth headers — all config),
outbound, so no inbound access to the gateway is needed; with `prometheus`
the gateway answers `GET /metrics`.

**Where a collector exists, this feature should not.** A real monitoring
system has history, alert rules, deduplication, routing, escalation and
on-call — and it does not stop working when an administrator closes their
laptop. Pointing `otel_endpoint` at it is config plus documentation, and it
beats anything built here.

This exists for the deployments that have none of that: a laptop, a handful
of containers, a panel bound to loopback, and a person who needs to find
out promptly that something wants their attention. That is the whole scope.
The moment a customer stands up a collector, the answer for them is the
collector.

## Goal

Volt Admin keeps watching every profile the administrator marks, with its
window closed, and raises a system notification when a gateway reports
something that deserves a human — once per problem, not once per log line.

## The source: `GET /api/logs`

The gateway already serves exactly what this needs, and nothing has to be
parsed out of a text log (checked in the voltd sources,
`crates/zeroclaw-gateway`, and then end to end against a live one: mint a
code, `POST /api/pair`, `GET /api/logs?severity_min=13&hide_internal=true`
with the token — answered with `events`, `next_cursor`,
`next_cursor_line_offset`, `next_segment_cursor`, `at_end`, `incomplete`,
`persistence_enabled`, `daemon_started_at`, `attribution_keys`, and each
event carrying `id`, `@timestamp`, `severity_number`, `severity_text`,
`event`, `message`, `service`, `attributes`):

- **`GET /api/logs`** (`lib.rs`, routed to `api_logs::handle_api_logs`) — a
  paginated query over the persisted JSONL event store, with
  `severity_min`, `hide_internal`, `limit` and a cursor
  (`until_segment_cursor` + `until_line_offset`).
- The events are structured (`zeroclaw_log::LogEvent`): an `id`, a
  `Severity`, an `EventCategory`, an `EventOutcome`.
- **The id is per OCCURRENCE, not per problem**, which the first live test
  settled: one misconfiguration repeating every 15 seconds filled 38 of 40
  events in a single poll, each with its own id. What a person would call
  one problem is one MESSAGE, so that is what the watcher deduplicates by.
- **`hide_internal` must be false.** It was `true` to keep that noise out —
  until a deliberately failed cron job logged `Scheduler job '' failed` as a
  Warn of category `internal`, which the flag drops. A watcher that hides
  the daemon's own problems watches nothing; the noise the flag existed for
  is handled by the message rule above.
- **Events are read field by field, not by a derive.** `service` is an
  object (`{"name": "zeroclaw", "version": "0.8.7"}`); a fixture here
  assumed a string and every live poll answered "invalid type: map,
  expected a string" — for the whole page. A watcher that goes silent
  because voltd changed a field is worse than one that reads a little less
  of it, so an unreadable field is empty and the page still arrives.
- **Warnings are worth saying, once.** That failure is a Warn, not an
  Error — so notifying only on Error would have said nothing at all.
- The store is bounded by the daemon's own config
  (`[observability] log_persistence_max_entries`), and a daemon with
  persistence off answers `persistence_enabled: false` instead of serving a
  stale file — so "monitoring is not possible here" is a thing the app can
  say rather than guess.

Which kills the alternatives: no `tail -F` over SSH, no ANSI to strip, no
`voltd` process per poll, and no gRPC — the relay protocol is for other
traffic, this is plain HTTP on the panel port the app already reaches.

### A session token per profile is already overdue

Pairing now happens in `connect`, before the webview navigates (see
`crate::pair::redeem_code`) — which is what made a connect one page load
instead of three. But it mints and redeems a code EVERY time, and every
successful redeem adds a token hash to the gateway's `[gateway]
paired_tokens`. Connecting daily grows that list, and the panel's device
list, without bound.

That is fixed WITHOUT storing a token: the app remembers, per profile, the
`connection_fingerprint` of the gateway it already paired with, and pairs
up front only when that is missing or names somewhere else. The marker is
not a secret — it says a pairing happened, not what it produced — and when
the panel has genuinely lost its session the page-driven fallback still
mints one on demand.

So a STORED SESSION TOKEN is needed only if this feature is built: a
watcher polls `/api/logs` with no window open, and has to present one
itself. Dropping monitoring drops that requirement with it.

### Authentication, and the one new secret

`/api/logs` goes through `require_auth`: `Authorization: Bearer <token>`,
where the token is a **paired session token** — not the gateway admin token.
So the app must pair *itself* as a device, which it is already equipped to
do: mint a code with the admin token (`crate::pair`), redeem it at
`POST /api/pair`, keep the session token.

That is a real change in what this app stores. Today it deliberately
persists no secret at all — `Profile` has no password field, the admin token
lives in memory for the length of a connection. A watched profile needs a
session token that survives restarts, and `tauri-plugin-store` writes plain
JSON next to the profiles.

The token is kept WITH the gateway it was minted for (the profile's
`connection_fingerprint`), and is used only while the profile still names
that gateway: a watched profile whose `panel_port` is edited is a different
gateway on the same machine, and the old token must not be sent to it.

A 401 means the gateway revoked it (any `--rotate` does). The watcher pairs
again ONCE and then stops with a reason — a watcher that re-pairs on every
refusal would mint a pairing code every minute at a gateway that keeps
saying no. Pairing unattended is what ticking "watch" consents to; verifying
that the loopback port is really voltd (rather than a process that took the
port first) is an app-wide question this feature does not answer — `connect`
pairs the same way.

So: **a session token is stored only for profiles the administrator marks as
watched**, it is never written into an exported profile document (it is not
a `Profile` field — a separate store keyed by profile id), the file is
0600 in a 0700 directory on Linux and macOS (the store plugin writes 0644;
on Windows the per-user profile ACL already does this), and the OS keychain
is the right home for it the moment this moves past the pilot.
Un-watching a profile deletes its token.

## The constraint nobody can design around

**One tunnel exists at a time.** `TunnelState` holds a single `Tunnel`, and
`connect` drops the previous one before opening the next (`commands.rs`).
Watching N gateways means N concurrent tunnels, which is a different shape
from "the connection the window is showing":

- the pool is per profile, opened for monitoring, and survives the window;
- the displayed connection is one of them rather than the only one;
- a monitored profile that is not on screen still needs its local port, so
  `preferred_port` already pointing at a stable per-profile port helps here
  (see `tunnel::preferred_port`), as does the per-profile `localPort`.

This is the bulk of the work, and it is what makes the feature a product
change rather than a screen.

No exception in the pilot: the kit binds the panel to `127.0.0.1`
(`[gateway] host = "127.0.0.1"`, "reachable only from the host itself or
through the Volt Admin SSH tunnel"), so a watched remote gateway needs its
tunnel up. Only a `Local` profile is free of one.

## Behavior

**Opting in.** A profile is watched or not — a switch in its own form, off by
default. Nothing is monitored because it exists; an admin with twenty
profiles chooses the handful that matter.

**Polling.** `GET /api/logs?severity_min=<warn>&hide_internal=1&limit=50`
per watched profile per interval (60 s to start; one constant, not a
setting, until someone asks). The newest page is enough: anything older than
the last poll is already known by event id. A poll that fails is itself a
finding — see Down.

**What raises a notification.** An event at error severity, and a gateway
that stops answering. Warnings are collected and shown in the app, never
pushed: an admin who is pushed for warnings turns pushes off, and then
misses the errors too.

**Once per problem.** The event `id` is the key — the store supplies it, so
no hashing of rendered text and no guessing. An id already notified is never
notified again; the app keeps the ids it has seen per profile, bounded by
the same window the gateway keeps. A burst collapses into one notification
saying how many there were.

**Down.** A watched profile whose poll fails three times in a row notifies
once ("VoltSSH is not answering"), and once more when it comes back. Three,
because a laptop lid and a Wi-Fi handover each produce one.

**Clicking a notification** opens the window on that profile's problem list
— not on the panel, which would mean opening a tunnel and navigating away
from everything else.

**In the window.** The Connect screen's profile cards grow a state: quiet,
`N` problems, or unreachable. Opening one shows the collected events with
their time and text, and a "mark as read" that clears the card.

**Tray.** Monitoring with the window closed needs the app to outlive the
window, which the current model refuses by design: "Multi-copy model: no
tray. Closing the window quits this copy" (`lib.rs`). Resolve that first —
see Decide before building.

## Turning the source on

A gateway that records nothing (`observability.backend = none`) or keeps
nothing (`observability.log_persistence = off`) has no log to watch, and
`/api/logs` says so with `persistence_enabled: false`. The profile card's
menu turns on whichever is off (`docs/specs/gateway-actions.md`), the same
way it enables the emergency stop — so a gateway can be made watchable from
here rather than by editing `config.toml` on the far end.

## Decide before building

1. **One app or many copies?** Today each launch is an independent copy with
   its own connection. A tray icon per copy is absurd, so monitoring implies
   a single instance that owns the profiles, the tunnel pool and the tray,
   with a window per connection. That is the multi-window plan the code
   already refers to (`_local/plan-volt-admin-multiwindow.md`), and it has to
   land before or with this.
2. **Does watching require a tunnel?** See the exception above.
3. **Is an unattended tunnel acceptable?** A watched profile behind SSH keeps
   a connection open with no one at the keyboard. For `agent` and `keyFile`
   auth that is fine; for `password` auth it means holding the password for
   as long as monitoring runs, which the app currently refuses to do beyond
   the lifetime of a connection (`PairContext`). Password profiles should
   simply not be watchable.
4. **Where does the session token live?** Plain JSON beside the profiles is
   what exists today; the OS keychain is what this deserves. Decide before
   the first watched profile outside the pilot.

## Out of scope

- Acting on a problem — restarting an agent, clearing a queue. Read-only.
- History, charts, retention. The store is what the gateway already keeps;
  the app shows the recent window and forgets.
- Alerting anyone but the person at this machine: no email, no webhooks, no
  escalation. That is a monitoring system's job, and if the pilot needs one,
  it should get one rather than grow it here.

## Platform notes

- Notifications: `tauri-plugin-notification`. On Windows a toast needs an
  AppUserModelID, which comes from the Start-menu shortcut the NSIS
  installer creates — a portable `.exe` run from a folder may show nothing.
  Verify on an installed build, not a dev run.
- Tray: `tauri`'s own `tray-icon` feature, not a plugin, and it needs a
  menu with at least "show" and "quit" or the app becomes unkillable from
  the UI.
- A poll loop must not wake a sleeping machine, and must notice it slept:
  `tokio::time::interval` with `MissedTickBehavior::Delay`.

## Done when

- A watched profile whose gateway records an error produces one notification
  within a poll interval, and the same event id never produces a second one.
- A gateway with log persistence turned off says so in the app instead of
  looking healthy (`persistence_enabled: false`).
- Killing a watched gateway produces one "not answering" notification after
  three failed polls, and one more when it returns.
- With the window closed, both still happen.
- A profile that is not watched costs nothing: no tunnel, no poll, no
  notification.
- Turning every watch off leaves the app behaving exactly as it does today.
