# Connection registry — Implementation Plan (monitoring, phase 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single `TunnelState` with a registry of connections keyed by profile id, each kept alive by named holders, so more than one can exist and a failed connect no longer destroys the one that worked.

**Architecture:** A `Registry` owns `profile id → Connection`, where a connection is a local port plus an optional `Tunnel` (`Local` profiles have none) and the set of holders that want it alive. Holder bookkeeping is pure and unit-tested with `tunnel: None`, so no test spawns `ssh`. `connect` publishes the new connection before releasing the window's hold on the previous one; dropping a `Connection` still tears its ssh child down through the existing `Tunnel::drop`.

**Tech Stack:** Rust, `tokio::sync::Mutex`, existing `tunnel::Tunnel`.

**Spec:** `docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md` §1

## Global Constraints

- No new dependency in this phase.
- `Tunnel` keeps its current teardown contract: dropping it kills the ssh child (`tunnel.rs`, `impl Drop`). The registry must never leak a `Tunnel` it removes.
- A `Local` connection is a first-class registry entry with `tunnel: None` — nothing downstream may special-case `Local` by looking for a missing entry.
- Holder names in this phase: `Holder::Window` and `Holder::Monitor`. `Monitor` is never set yet; it exists so the lifetime rule is written once.
- The app still has exactly one window. `release_all(Holder::Window)` is therefore the correct disconnect semantics for now; per-window holds arrive in phase 2.
- RU/EN user-facing strings: none are added in this phase.

## Review Focus

- **Connecting to a profile that is already connected** (the user clicks Connect twice): the live connection must not be dropped and a second tunnel must not be opened — Task 1 (`hold` on an existing id) and Task 2.
- **A connect that fails**: the previous connection must still be usable, which is the whole point of publishing before releasing — Task 2.
- **Releasing a hold nobody holds** (disconnect twice, disconnect with nothing connected): a no-op, not a panic — Task 1.
- **A `Local` profile**: occupies an entry with `tunnel: None`, and releasing it removes the entry like any other — Task 1.
- **App exit with several connections**: every ssh child dies, which means the registry's map must be dropped, not leaked into a detached task — Task 3.

---

### Task 1: The registry and its holder rules

**Files:**
- Create: `src-tauri/src/registry.rs` (with its `#[cfg(test)] mod tests`)
- Modify: `src-tauri/src/lib.rs` (`mod registry;`)

**Interfaces:**
- Produces:
  - `pub enum Holder { Window, Monitor }` (`Copy`, `PartialEq`, `Eq`, `Hash`, `Debug`)
  - `pub struct Connection { pub tunnel: Option<Tunnel>, pub local_port: u16, holders: HashSet<Holder> }`
  - `pub struct Registry(Mutex<HashMap<String, Connection>>)` — `Default`
  - `pub async fn hold(&self, profile_id: &str, holder: Holder) -> Option<u16>` — adds the holder to an existing connection and returns its local port; `None` when there is no such connection (the caller must open one)
  - `pub async fn publish(&self, profile_id: &str, tunnel: Option<Tunnel>, local_port: u16, holder: Holder)` — inserts, replacing any entry for that id
  - `pub async fn release(&self, profile_id: &str, holder: Holder) -> bool` — removes the holder, and the connection with it when no holders remain; `true` when the connection was dropped
  - `pub async fn release_all(&self, holder: Holder) -> Vec<String>` — the same for every id, returning the ids whose connection was dropped
  - `pub async fn release_others(&self, holder: Holder, keep: &str) -> Vec<String>` — `release_all` except for `keep`
  - `pub async fn local_port(&self, profile_id: &str) -> Option<u16>`
  - `pub async fn len(&self) -> usize`

- [ ] **Step 1: Write the failing tests**

In `registry.rs`. Every test uses `tunnel: None`, so none of them spawns `ssh`:

```rust
#[tokio::test] async fn hold_on_an_unknown_profile_is_none()
#[tokio::test] async fn publish_then_hold_returns_the_local_port()
#[tokio::test] async fn two_holders_keep_the_connection_until_both_let_go()
#[tokio::test] async fn release_drops_the_connection_when_it_was_the_last_holder()
#[tokio::test] async fn releasing_a_holder_that_is_not_held_is_a_no_op()   // false, len unchanged
#[tokio::test] async fn release_all_reports_every_id_it_dropped()
#[tokio::test] async fn release_others_keeps_the_named_profile()
#[tokio::test] async fn publish_twice_for_one_id_leaves_one_entry()        // len() == 1
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd src-tauri && cargo test registry`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement `registry.rs`**

`Registry` wraps one `tokio::sync::Mutex<HashMap<String, Connection>>`; `Connection::holders` is a `HashSet<Holder>`. `release` removes the holder, and when the set is empty removes the map entry — letting the `Connection` (and its `Tunnel`) drop at the end of the statement, outside the lock is not required but the guard must not be held across an `.await`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test registry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/registry.rs src-tauri/src/lib.rs
git commit -m "feat(registry): connections keyed by profile, kept by named holders"
```

---

### Task 2: `connect` publishes before it releases

**Files:**
- Modify: `src-tauri/src/commands.rs` (the `connect` command, currently clearing `tunnel_state` at its start and publishing at line ~360)
- Modify: `src-tauri/src/lib.rs` (`.manage(registry::Registry::default())` instead of `tunnel::TunnelState::default()`)
- Modify: `src-tauri/src/tunnel.rs` (delete `TunnelState`)

**Interfaces:**
- Consumes: everything from Task 1.
- Produces: `connect` takes `registry: State<'_, Registry>` in place of `tunnel_state: State<'_, TunnelState>`.

- [ ] **Step 1: Reorder `connect`**

Delete the "old tunnel cleared" block at the top. Then:

1. `registry.hold(&profile.id, Holder::Window)` — `Some(port)` means a connection is already up for this profile: reuse it, skip `open_tunnel` entirely, and the base URL is `http://127.0.0.1:{port}`.
2. `None` → open as today (`Local`: no tunnel, port = `profile.panel_port`; `Remote`: `open_tunnel` with its 30 s hard cap), then `registry.publish(&profile.id, tunnel, local_port, Holder::Window)`.
3. Only once a connection exists: `registry.release_others(Holder::Window, &profile.id)`.

The early `return Err(...)` paths for a failed or hung `open_tunnel` now leave the previous connection untouched, which is the behaviour change this task ships.

- [ ] **Step 2: Update `disconnect`**

Replace the `*guard = None` body with `registry.release_all(Holder::Window)`. Everything else in `disconnect` (clearing `PairState`, the title, navigating back) is unchanged.

- [ ] **Step 3: Delete `TunnelState`**

Remove the struct from `tunnel.rs` and its `use` in `commands.rs`; `lib.rs` manages `Registry` instead. The doc comment that explains the single-tunnel contract goes with it.

- [ ] **Step 4: Verify the suite and the lints**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: PASS, with no reference to `TunnelState` left (`grep -rn TunnelState src-tauri/src` is empty).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(connect): publish the new connection before releasing the old"
```

---

### Task 3: Everything dies on exit, and the comments stop lying

**Files:**
- Modify: `src-tauri/src/lib.rs` (the `setup` comment about the multi-copy model and `TunnelState`)
- Modify: `src-tauri/src/registry.rs` (module docs)
- Modify: `docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md` (§1's claim about what phase 1 delivers)

**Interfaces:**
- Consumes: Task 1 and Task 2. Produces nothing new.

- [ ] **Step 1: Write the failing test**

In `registry.rs`:

```rust
#[tokio::test]
async fn dropping_the_registry_drops_every_connection() {
    // Two published connections, then drop the Registry; the map must go
    // with it. Proven with an Arc clone count rather than a real Tunnel:
    // `publish` three ids, keep a Weak to a payload in each, drop the
    // registry, assert every Weak is dead.
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test registry::tests::dropping_the_registry`
Expected: FAIL until the test's payload hook exists on `Connection`.

- [ ] **Step 3: Make the test possible without faking a `Tunnel`**

Give `Connection` a `#[cfg(test)] pub(crate) probe: Option<std::sync::Arc<()>>` field, set by a test-only constructor. Nothing in production code reads it.

- [ ] **Step 4: Run the test**

Run: `cd src-tauri && cargo test registry`
Expected: PASS.

- [ ] **Step 5: Fix the comments that now describe the old world**

`lib.rs`'s setup comment says "its TunnelState Drop kills the ssh child" — it is the registry now, and it still holds: closing the window quits this copy, and the `Registry` goes down with it, taking every `Tunnel` with it. The design doc's §1 claim that phase 1 makes "switching back to a profile no longer rebuild its tunnel" is wrong while the window is the only holder; replace it with what phase 1 actually ships (a failed connect no longer costs you the connection you had) and move the reuse claim to the phase that introduces `Holder::Monitor`.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src docs
git commit -m "test(registry): the map dies with the app, and fix what phase 1 claims"
```
