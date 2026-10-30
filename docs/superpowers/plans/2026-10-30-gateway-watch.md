# Gateway watch (monitoring, phase 3 — first slice) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A watched gateway's errors reach the person at this machine within a minute, with no window open.

**Architecture:** A profile can be marked watched. One tokio task per watched profile polls `GET /api/logs` from a cursor with a stored session token, dedupes by event id, and raises what it finds through the tray tooltip and an OS notification. Everything that decides something — parsing, counting, deduping, cursor and restart handling — is a pure function in `watch.rs` with tests; the task is the loop around them.

**Tech Stack:** Rust (reqwest already present, tokio, `tauri-plugin-store`), `tauri-plugin-notification` (new), React + TypeScript.

**Spec:** `docs/specs/gateway-monitoring.md` (and `docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md`)

## Global Constraints

- **This slice watches `Local` profiles only.** A `Remote` gateway needs a tunnel with no window open — the pool is the next slice, not this one. The switch is refused for `Remote` with that reason.
- **No password-auth profile is watchable**, ever: watching would mean holding a password for as long as the app runs (`docs/specs/gateway-monitoring.md`, "Decide before building").
- **The session token is a secret.** It lives in its own store (`watch.json`), never in `profiles.json`, never in an exported profile, never in the log or the diagnostics, and is deleted when the profile stops being watched or is deleted.
- **Nothing in the log carries an event's text, a token or a code** (`docs/specs/diagnostics.md`). What is logged is a profile, a count and an outcome.
- Poll interval 60s. `severity_min=13` (Warn), `hide_internal=true`, `limit=200`.
- RU first, EN second for every string.
- One new dependency (`tauri-plugin-notification`), no others.

## Review Focus

- **A gateway with `persistence_enabled: false`** — it can answer 200 with no events forever. The card must say monitoring is impossible there (and offer the menu item that turns the log on), never show silence as health. — Task 1 + Task 5.
- **The daemon restarted** (`daemon_started_at` changed): the old cursor points into a store that no longer exists. Resume from the newest page rather than replaying the whole log as "new". — Task 1.
- **The laptop woke after hours**: the catch-up poll returns a backlog. It is counted and summarised in ONE notification, never one per event. — Task 3 + Task 4.
- **The token was revoked** (a `--rotate` from the menu, or someone else's): the poll answers 401. Re-pair ONCE, store the new token, and on a second failure stop that watcher with a reason — never a loop that mints a pairing code every minute. — Task 3.
- **The profile was edited or deleted while watched**: its watcher stops and its token is deleted; a profile switched to `Remote` or password auth stops being watchable. — Task 2 + Task 3.

---

### Task 1: What a poll means

**Files:**
- Create: `src-tauri/src/watch.rs` (with its `#[cfg(test)] mod tests`)
- Modify: `src-tauri/src/lib.rs` (`mod watch;`)

**Interfaces:**
- Produces: `pub struct Event { pub id: String, pub at: String, pub severity: u8, pub message: String, pub service: String }`, with `pub fn is_error(&self) -> bool` (`severity >= 17`).
- Produces: `pub struct Cursor { pub segment: String, pub line_offset: u64 }` and `pub struct Poll { pub events: Vec<Event>, pub cursor: Option<Cursor>, pub persistence_enabled: bool, pub incomplete: bool, pub daemon_started_at: Option<String> }`.
- Produces: `pub fn parse_poll(json: &str) -> Result<Poll, String>`.
- Produces: `pub fn logs_url(base: &str, cursor: Option<&Cursor>) -> String` — `{base}/api/logs?severity_min=13&hide_internal=true&limit=200`, plus `&until_segment_cursor=&until_line_offset=` when a cursor is given.
- Produces: `pub fn fresh_start(prev: Option<&str>, now: Option<&str>) -> bool` — true when `daemon_started_at` changed, meaning the cursor must be dropped.
- Produces: `pub fn unseen(events: &[Event], seen: &std::collections::HashSet<String>) -> Vec<Event>` and `pub fn remember_seen(seen: &mut std::collections::HashSet<String>, events: &[Event])`, bounded at 1000 ids (oldest dropped).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_real_answer_parses_into_events_and_a_cursor()
    // the fixture below: 2 events, cursor Some, persistence_enabled true,
    // incomplete false, daemon_started_at Some; severity 17 is_error(), 13 is not
#[test] fn a_gateway_that_keeps_nothing_says_so()
    // {"events":[],"persistence_enabled":false,"at_end":true} → persistence_enabled false, no error
#[test] fn unknown_fields_and_a_missing_cursor_are_not_a_failure()
    // an answer with extra keys and no until_* fields parses, cursor None
#[test] fn a_restarted_daemon_invalidates_the_cursor()
    // fresh_start(Some("a"), Some("b")) == true; equal → false; None on either side → false
#[test] fn the_same_event_is_new_only_once()
    // unseen() drops ids already in the set; remember_seen keeps at most 1000
#[test] fn the_url_carries_the_cursor_only_when_there_is_one()
```

The fixture is the shape the live gateway answered with (`docs/specs/gateway-monitoring.md`): top-level `events`, `next_cursor`, `next_cursor_line_offset`, `next_segment_cursor`, `at_end`, `incomplete`, `persistence_enabled`, `daemon_started_at`, `attribution_keys`; each event `{"@timestamp","attributes","event","id","message","schema_version","service","severity_number","severity_text","zeroclaw"}`.

- [ ] **Step 2: Run them and watch them fail**

Run: `cd src-tauri && cargo test watch`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement `watch.rs`**

`serde::Deserialize` with `#[serde(default)]` on everything optional — a field voltd adds later must not break a poll. `parse_poll` maps the wire shape onto the types above; nothing else in this task does I/O.

- [ ] **Step 4: Run the suite**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(watch): what a /api/logs poll means"
```

---

### Task 2: A watched profile, and where its token lives

**Files:**
- Modify: `src-tauri/src/connection.rs` (`Profile::watch`, `validate`, tests)
- Modify: `src-tauri/src/settings.rs` (token store, tests)
- Modify: `src-tauri/src/commands.rs` (`delete_profile` forgets the token)

**Interfaces:**
- Produces: `Profile { .., #[serde(default)] pub watch: bool }`.
- Produces: `pub fn watchable(p: &Profile) -> Result<(), String>` in `connection.rs` — `Ok` only for `ConnMode::Local` with any auth but `AuthMethod::Password`; the error is `"watch:remote"` or `"watch:password"` (the UI maps those onto its own copy). `validate` returns it when `watch` is set.
- Produces: in `settings.rs`, store file `watch.json` (`WATCH_STORE`), key = profile id: `pub fn watch_token<R: Runtime>(app: &AppHandle<R>, profile_id: &str) -> Option<String>`, `pub fn remember_watch_token<R: Runtime>(app: &AppHandle<R>, profile_id: &str, token: &str) -> Result<(), String>`, `pub fn forget_watch_token<R: Runtime>(app: &AppHandle<R>, profile_id: &str)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_remote_or_password_profile_is_not_watchable()
    // watchable(local+agent) == Ok; local+password → Err("watch:password");
    // remote+agent → Err("watch:remote")
#[test] fn validate_refuses_a_watch_it_cannot_honour()
    // profile{watch:true, mode:Remote}.validate() == Err("watch:remote")
#[test] fn a_profile_document_without_the_field_is_not_watched()
    // serde: JSON with no "watch" key parses with watch == false
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd src-tauri && cargo test connection`
Expected: FAIL — no `watch` field.

- [ ] **Step 3: Implement the field, the rule and the token store**

The token store is a second `tauri-plugin-store` file next to `settings.json`; it is NOT the settings store, so nothing that dumps settings can print a token. `delete_profile` calls `forget_watch_token` beside the `forget_paired` it already does.

- [ ] **Step 4: Run the suite**

Run: `cd src-tauri && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(watch): a watched profile, and a token that lives apart"
```

---

### Task 3: The watcher

**Files:**
- Modify: `src-tauri/src/watch.rs` (the loop, `Watchers`, tests for the diff)
- Modify: `src-tauri/src/lib.rs` (manage `Watchers`, `watch::sync` at setup)
- Modify: `src-tauri/src/commands.rs` (`save_profile`/`delete_profile` call `watch::sync`)

**Interfaces:**
- Consumes: Task 1's pure functions; Task 2's `settings::watch_token` / `remember_watch_token`, `connection::watchable`.
- Produces: `pub struct Watchers(pub Mutex<HashMap<String, tokio::task::JoinHandle<()>>>)`, managed state.
- Produces: `pub fn sync<R: Runtime>(app: &AppHandle<R>)` — starts a task for every watched, watchable profile without one, aborts tasks whose profile is gone or no longer watched.
- Produces: `pub fn to_start_and_stop(watched: &[String], running: &[String]) -> (Vec<String>, Vec<String>)` — the pure half, which is what the test exercises.
- Produces: `pub struct Status { pub profile_id: String, pub errors: usize, pub warnings: usize, pub persistence: bool, pub trouble: Option<String> }` and `pub struct Board(pub Mutex<HashMap<String, Status>>)`, managed state the UI reads.

- [ ] **Step 1: Write the failing test**

```rust
#[test] fn syncing_starts_what_is_watched_and_stops_what_is_not() {
    // watched ["a","b"], running ["b","c"] → start ["a"], stop ["c"]
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test watch`
Expected: FAIL — `to_start_and_stop` does not exist.

- [ ] **Step 3: Implement the loop**

One task per profile: resolve the base URL (`http://127.0.0.1:{panel_port}` — `Local` only in this slice), take the stored token or mint one (`pair::mint_code` + `pair::redeem_code`, then `remember_watch_token`), then every 60s poll `logs_url`, `parse_poll`, drop the cursor when `fresh_start`, keep only `unseen`, update `Board`, and hand the new errors to Task 4. A 401 re-pairs ONCE; a second 401 writes `trouble` and ends that task. `persistence_enabled: false` writes `trouble` and ends it too — there is nothing to poll. The `reqwest::Client` is built with `.no_proxy()`, as `pair::redeem_code` is: the target is loopback.

- [ ] **Step 4: Run the suite**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(watch): one task per watched gateway"
```

---

### Task 4: Saying it out loud

**Files:**
- Modify: `src-tauri/src/tray.rs` (tooltip, test)
- Modify: `src-tauri/src/watch.rs` (call it)
- Modify: `src-tauri/Cargo.toml`, `src-tauri/src/lib.rs`, `src-tauri/capabilities/default.json` (`tauri-plugin-notification`)

**Interfaces:**
- Produces: `pub fn watch_tooltip(lang: Lang, errors: usize, warnings: usize) -> String` in `tray.rs` — the launcher title when both are zero; otherwise the title plus the counts.
- Produces: `pub fn set_watch_tooltip<R: Runtime>(app: &AppHandle<R>, lang: Lang, errors: usize, warnings: usize)`.
- Produces: `pub fn announce<R: Runtime>(app: &AppHandle<R>, lang: Lang, profile_name: &str, new_errors: &[Event])` in `watch.rs` — ONE notification per poll, titled with the profile and the count, bodied with the newest message, and nothing when the slice is empty.

- [ ] **Step 1: Write the failing test**

```rust
#[test] fn the_tooltip_says_nothing_when_there_is_nothing_to_say() {
    assert_eq!(watch_tooltip(Lang::Ru, 0, 0), "Volt Admin");
    assert!(watch_tooltip(Lang::Ru, 2, 5).contains("2"));
    assert!(watch_tooltip(Lang::En, 2, 5).contains("2 errors"));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test tray`
Expected: FAIL — `watch_tooltip` does not exist.

- [ ] **Step 3: Implement the tooltip and the notification**

Add `tauri-plugin-notification = "2"` to `Cargo.toml`, register it in `lib.rs`, add `"notification:default"` to `capabilities/default.json`. A notification body carries the event's `message` — which is the gateway's text, not a secret of ours; the LOG still gets only counts.

- [ ] **Step 4: Run the suite**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri
git commit -m "feat(watch): the tray says what the watcher found, once"
```

---

### Task 5: The switch, and what the card shows

**Files:**
- Modify: `src/screens/Connect.tsx`, `src/lib/i18n.tsx`
- Modify: `src-tauri/src/commands.rs` (`watch_board` command)

**Interfaces:**
- Consumes: Task 3's `Board`.
- Produces: `#[command] pub fn watch_board<R: Runtime>(app: AppHandle<R>) -> Vec<Status>`.
- Produces: in the profile form, a `watch` checkbox — disabled with the reason for a `Remote` or password profile; in the card, a line per watched profile: counts, or `trouble`.

- [ ] **Step 1: Add the strings (RU first, then EN)**

`field.watch` («Следить за ошибками»), `watch.remote` («Пока только для локальных профилей — удалённому нужен туннель без окна»), `watch.password` («Профиль с паролем наблюдать нельзя: пароль пришлось бы хранить»), `watch.quiet` («Ошибок нет»), `watch.counts` («{e} ошибок, {w} предупреждений»), `watch.noLog` («Шлюз не хранит журнал — включите журнал событий в меню»).

- [ ] **Step 2: Add the switch and the card line**

The form's checkbox writes `watch`; `save_profile` already validates, so a refused watch arrives as `field:watch:remote` / `field:watch:password` and is shown under the checkbox. The card polls `watch_board` on mount and on the `watch` event.

- [ ] **Step 3: Verify**

Run: `bun run build`
Expected: PASS.
Then, in an installed build: mark the local profile watched, make the gateway log an error, and see the notification and the tray tooltip without a window open.

- [ ] **Step 4: Commit**

```bash
git add src src-tauri/src
git commit -m "feat(watch): the switch, and what the card says"
```
