# Watch: the gateway's stream, and a tunnel of its own — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A watched gateway's trouble reaches the person within seconds, remote gateways included.

**Architecture:** The watcher stops polling. It catches up once with `GET /api/logs`, then holds `GET /api/events` (Server-Sent Events) open and reads frames as they arrive; a dropped or silent stream reconnects with backoff, and every reconnection catches up again. A watched `Remote` profile gets a tunnel of its own, held in the registry as `Holder::Monitor` on the port a window would use, so a window opened later joins it instead of raising a second one.

**Tech Stack:** Rust (`reqwest` streaming via `Response::chunk`, the existing `tunnel`/`registry` modules), React + TypeScript.

**Spec:** `docs/specs/gateway-monitoring.md` (sections "The gateway pushes, too" and "Watching through a tunnel")

## Global Constraints

- **No server-side filtering.** `severity_min` is ignored by the stream and `hide_internal=true` hides the daemon's own failures. Everything is filtered here, by the rules already in `watch.rs`.
- **No request timeout on the stream.** The existing 15s timeout belongs to the catch-up fetch only; a stream that times out every 15 seconds is a poll with extra steps.
- **Five minutes of silence means reconnect.** The gateway sends no heartbeat, so an idle stream and a dead one are identical from here.
- Agent or key auth only for a watched profile; password profiles stay unwatchable.
- A tunnel the watcher opened is released when the profile stops being watched — and never torn down while a window still holds the connection.
- The log keeps counts, outcomes and categories. No event text, no token (`docs/specs/diagnostics.md`).
- RU first, EN second. No new dependency.

## Review Focus

- **A frame split across two chunks** (the common case on a slow link): the reader must buffer, not drop or double-report — Task 1.
- **A gateway that restarts under an open stream**: the read ends, the watcher reconnects and catches up; nothing is reported twice (the `said` window and `seen_ids` already decide that) — Task 2.
- **A laptop that sleeps**: the stream is dead on wake with no error until something is read; the silence floor is what notices, and the catch-up reports what was missed as ONE notification — Task 2.
- **A window connecting to a profile the watcher already tunnelled**: it must reuse that connection, not open a second tunnel on a second port — Task 3.
- **A tunnel that cannot come up at all** (host down, key refused): backoff, and after it is exhausted the card says the tunnel is down rather than showing a quiet gateway — Task 3 and Task 4.

---

### Task 1: Reading a Server-Sent Events stream

**Files:**
- Modify: `src-tauri/src/watch.rs` (the reader and its tests)

**Interfaces:**
- Produces: `pub struct Frames { buffer: String }` with `pub fn new() -> Self`, `pub fn feed(&mut self, chunk: &str) -> Vec<String>` — appends `chunk` and returns the complete `data:` payloads it now holds, leaving a partial frame in the buffer.
- Produces: `pub fn events_url(base: &str) -> String` — `{base}/api/events`, with no query: the gateway ignores `severity_min` and `hide_internal` hides what matters.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_frame_split_across_chunks_is_read_once()
    // feed("data: {\"message\":\"a\"") → []; feed("}\n\n") → one payload, {"message":"a"}
#[test] fn several_frames_in_one_chunk_all_arrive()
    // two "data: {...}\n\n" in one feed → two payloads, in order
#[test] fn a_comment_or_a_blank_line_is_not_an_event()
    // ":keep-alive\n\n" and "\n\n" → []
#[test] fn the_stream_url_asks_for_no_filtering()
    // events_url("http://h") == "http://h/api/events"
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd src-tauri && cargo test watch`
Expected: FAIL — `Frames` does not exist.

- [ ] **Step 3: Implement `Frames` and `events_url`**

One frame is the lines up to a blank line; the payload is what follows `data:` on each of them, joined by newline (the SSE rule). Lines starting with `:` are comments. Non-`data:` fields (`event:`, `id:`, `retry:`) are ignored — this gateway sends none.

- [ ] **Step 4: Run the suite**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/watch.rs
git commit -m "feat(watch): read a Server-Sent Events stream"
```

---

### Task 2: Catch up, then listen

**Files:**
- Modify: `src-tauri/src/watch.rs` (`run_one`, `Trouble`)

**Interfaces:**
- Consumes: Task 1's `Frames`/`events_url`; the existing `parse_poll`, `new_occurrences`, `due_problems`, `report`.
- Produces: `Trouble::StreamLost` (key `"stream-lost"`), reported when the stream cannot be re-established after the backoff is exhausted.

- [ ] **Step 1: Replace the poll loop**

Per connection: fetch `logs_url` once (the 15s timeout stays here) and report what it finds; then `GET events_url` with no timeout and read `chunk()` in a loop, feeding `Frames` and reporting each payload through the same `read_event` → `new_occurrences` → `due_problems` → `report` path. A chunk that does not arrive within `QUIET` (5 min) ends the read.

The reconnect ladder is `5s, 30s, 5min` and then repeats at 5min; `Trouble::StreamLost` is reported once the first full ladder is exhausted, and cleared by the next successful connection — the card must stop lying as soon as the gateway comes back.

- [ ] **Step 2: Verify**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.
Then, in an installed build: watch the local profile, make the gateway log something (a one-shot cron job that fails), and see the notification arrive in seconds rather than within a minute; stop the daemon and watch the card say the stream is lost; start it and watch the card recover.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/watch.rs
git commit -m "feat(watch): catch up once, then listen"
```

---

### Task 3: A tunnel the watcher holds

**Files:**
- Modify: `src-tauri/src/watch.rs` (the tunnel before the stream)
- Modify: `src-tauri/src/connection.rs` (`watchable` allows `Remote`)

**Interfaces:**
- Consumes: `registry::Registry::{hold, publish, release, peek}`, `tunnel::open` (whatever `commands::connect` calls to raise one), `Holder::Monitor`.
- Produces: `watchable` returns `Ok` for a `Remote` profile whose auth is `Agent` or `KeyFile`; `"watch:password"` stays.
- Produces: in `run_one`, the base URL for a `Remote` profile is `http://127.0.0.1:{local_port}` — the port the registry published for that connection, not the profile's `panel_port`.

- [ ] **Step 1: Write the failing test**

```rust
#[test] fn a_remote_profile_with_a_key_is_watchable() {
    // watchable(remote+agent) == Ok, watchable(remote+keyFile) == Ok,
    // watchable(remote+password) == Err("watch:password")
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test connection`
Expected: FAIL — `watch:remote` is still returned.

- [ ] **Step 3: Raise and hold the tunnel**

Before the first catch-up: take or create the connection through the registry with `Holder::Monitor` — the same path `connect` uses, so a window that is already connected is JOINED rather than duplicated. Hold it for as long as the watcher runs; release it when `sync` stops this watcher. A tunnel that will not come up uses the same ladder as the stream and ends in `Trouble::NoTunnel` (`"no-tunnel"`).

- [ ] **Step 4: Verify**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.
Then, in an installed build: watch the remote profile with no window open, confirm one `ssh` child exists, open a window and connect to the same profile, and confirm no second tunnel appears (the app log says the connection was reused).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(watch): a tunnel the watcher holds, shared with the window"
```

---

### Task 4: What the card says now

**Files:**
- Modify: `src/screens/Connect.tsx`, `src/lib/i18n.tsx`

**Interfaces:**
- Consumes: `Status.trouble` values `"stream-lost"` and `"no-tunnel"`.

- [ ] **Step 1: Add the strings (RU first, then EN)**

`watch.stream-lost` («Шлюз не отвечает — поток событий потерян»), `watch.no-tunnel` («Туннель не поднимается — проверьте доступ по SSH»), and the form's hint for a remote profile, which no longer refuses: `field.watch.remoteHint` («Туннель будет держаться, пока включено наблюдение»).

- [ ] **Step 2: Let a remote profile be watched**

The checkbox is disabled only for password auth now; the hint says what a watched remote profile keeps open.

- [ ] **Step 3: Verify**

Run: `bun run build`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src src-tauri
git commit -m "feat(watch): a remote profile can be watched, and says when it cannot"
```
