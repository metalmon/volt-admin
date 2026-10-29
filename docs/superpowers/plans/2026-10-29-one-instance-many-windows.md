# One instance, many windows — Implementation Plan (monitoring, phase 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the multi-copy app into one process with several windows and a tray, so a connection can outlive the window that opened it — the precondition for anything watching a gateway in the background.

**Architecture:** A window stops being "the app". `Holder::Window` gains the window's label, so each window holds its own connection and `disconnect` releases only its own. Commands take the calling window instead of looking up `"main"`. `PairState` becomes a map keyed by panel origin, because two connected windows mean two pairing contexts. A second launch no longer starts a second process: `tauri-plugin-single-instance` hands its argv to the running one, which opens another window. A tray keeps the process alive with the last window closed.

**Tech Stack:** Rust, Tauri v2 (`tray-icon` feature, `tauri-plugin-single-instance`), existing `Registry` from phase 1.

**Spec:** `docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md` §2

## Global Constraints

- `Holder::Monitor` stays unused in this phase; nothing polls yet.
- A window label is the registry key for its hold: `Holder::Window(String)`.
- The tray menu must offer "quit", or the app becomes unkillable from the UI (design §2).
- Closing the LAST window must not quit the process once the tray exists — that is the whole point — but quitting from the tray must.
- RU first, EN second for every new string (`src/lib/i18n.tsx`).
- `windows[0]` in `tauri.conf.json` stays the first window's config; new windows are built from it in code.

## Review Focus

- **Two windows on the same profile**: both hold the same connection; closing one must not take the panel away from the other — Task 1.
- **A window closed without `disconnect`** (the X button, not the in-app action): its hold must be released, or the connection leaks until quit — Task 2.
- **Two connected windows, two panels, one pairing context**: the second connect must not overwrite the first window's context, and `PROBE_SCRIPT` on either origin must find its own — Task 3.
- **A second launch while connected**: the new window shows the Connect screen, and the already-connected window keeps its panel — Task 4.
- **Quit from the tray with connections open**: every ssh child dies (the OS guard covers the process ending, `tunnel::child_guard`) — Task 4.

---

### Task 1: A hold belongs to a window, not to "the window"

**Files:**
- Modify: `src-tauri/src/registry.rs` (`Holder`, its tests)
- Modify: `src-tauri/src/commands.rs` (every `Holder::Window` use)

**Interfaces:**
- Produces: `pub enum Holder { Window(String), Monitor }` — the `String` is the Tauri window label.
- Produces: `pub async fn release_all_windows(&self) -> Vec<String>` — releases every `Window(_)` hold, for the app-wide teardown paths that are not about one window.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn two_windows_on_one_profile_both_have_to_let_go()
    // hold("a", Window("w1")), hold("a", Window("w2")); releasing w1 keeps it; releasing w2 drops it.
#[tokio::test] async fn releasing_one_window_leaves_another_windows_profile_alone()
    // w1 holds "a", w2 holds "b"; release(a, Window("w1")) drops only "a".
#[tokio::test] async fn release_others_is_scoped_to_one_window()
    // w1 holds "a" and "b"; release_others(Window("w1"), "b") drops "a" but not w2's hold on "b".
#[tokio::test] async fn release_all_windows_leaves_a_monitor_hold()
    // "a" held by Window("w1") and Monitor; release_all_windows() keeps it alive.
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd src-tauri && cargo test registry`
Expected: FAIL — `Holder::Window` takes no argument.

- [ ] **Step 3: Implement**

`Holder::Window(String)`. `release_all_windows` is `release_where` with a predicate over the holder set rather than a single holder — extract a private `release_matching(pred: impl Fn(&Holder) -> bool, which: impl Fn(&str) -> bool)` and express `release`, `release_all`, `release_others` and `release_all_windows` through it.

- [ ] **Step 4: Run the suite**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(registry): a hold belongs to one window"
```

---

### Task 2: Commands act on the window that called them

**Files:**
- Modify: `src-tauri/src/commands.rs` (`connect`, `disconnect`, `navigate_main_window`)
- Modify: `src-tauri/src/lib.rs` (window-close handling)

**Interfaces:**
- Produces: `connect` and `disconnect` take `window: tauri::WebviewWindow<R>` as their first Tauri-injected parameter and use it for navigation, the title and the hold label. `navigate_main_window` becomes `navigate(window: &WebviewWindow<R>, url)`.

- [ ] **Step 1: Replace the `"main"` lookups**

`commands.rs:53`, `:445`, `:461`, `:492` and the title set in each: all become the passed-in window. `connect` holds with `Holder::Window(window.label().to_string())`; `disconnect` releases exactly that.

- [ ] **Step 2: Release a hold when its window closes**

In `lib.rs`, on `WindowEvent::Destroyed` for a webview window, release `Holder::Window(label)` — a window closed with the X button never calls `disconnect`, and its connection would otherwise live until quit. Use `tauri::async_runtime::block_on` or spawn, whichever keeps the handler non-blocking.

- [ ] **Step 3: Verify**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.
Then `bun run tauri dev`: connect a profile, close the window with X, and confirm in the log that the hold was released (`disconnect`/`window closed` line) and no `ssh` is left (`Get-Process ssh`).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src
git commit -m "feat(windows): every command acts on the window that called it"
```

---

### Task 3: A pairing context per panel origin

**Files:**
- Modify: `src-tauri/src/pair.rs` (`PairState`)
- Modify: `src-tauri/src/commands.rs` (`set`, `clear`)

**Interfaces:**
- Produces: `PairState(Mutex<HashMap<String, PairContext>>)`, keyed by `PairContext::origin`; `set(ctx)` inserts, `clear_origin(&str)` removes one, `clear_all()` empties, `for_url(&Url) -> Option<PairContext>` matches by origin as it does today.

- [ ] **Step 1: Write the failing tests**

In `pair.rs`:

```rust
#[test] fn two_contexts_coexist_and_each_answers_its_own_origin()
#[test] fn clear_origin_leaves_the_other_context()
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd src-tauri && cargo test pair`
Expected: FAIL — `PairState` holds one `Option`.

- [ ] **Step 3: Implement, and update the callers**

`connect` inserts; `disconnect` clears the origin its own window was showing (it knows the profile, so it can rebuild the origin the same way `connect` did — or keep the origin on the registry entry, which is the simpler of the two: add `pub origin: String` to `Connection` and read it back from `hold`).

- [ ] **Step 4: Verify**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(pair): one pairing context per panel origin"
```

---

### Task 4: One process, a tray, and a second launch that opens a window

**Files:**
- Modify: `src-tauri/Cargo.toml` (`tauri` gains `tray-icon`; add `tauri-plugin-single-instance`)
- Modify: `src-tauri/src/lib.rs` (plugin, tray, new-window command registration)
- Create: `src-tauri/src/windows.rs` (`open_window`, the label counter)
- Modify: `src-tauri/capabilities/default.json` (the new window label pattern)
- Modify: `src/lib/i18n.tsx` (tray strings)

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: `windows::open_window(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>>` — builds a window from the same URL and size as the first, with a fresh label `win-<n>`; `#[command] pub fn new_window(app) -> Result<(), String>` for the UI.

- [ ] **Step 1: Write the failing test**

In `windows.rs`:

```rust
#[test] fn labels_never_repeat() {
    // next_label() three times → "win-1", "win-2", "win-3"
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test windows`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement `windows.rs`**

`next_label()` over an `AtomicUsize`; `open_window` uses `WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))` with the title «Вольт Админ» and the conf's size, then `fit_to_screen` (move that helper from `lib.rs` into `windows.rs` so both callers share it).

- [ ] **Step 4: Register the single-instance plugin**

`tauri_plugin_single_instance::init(|app, _argv, _cwd| { let _ = windows::open_window(app); })` — the plugin must be registered FIRST, before any other, per its docs.

- [ ] **Step 5: Add the tray**

`TrayIconBuilder` with the app icon and a menu: "Открыть" (opens a window, or focuses one if any exist) and "Выйти" (`app.exit(0)`). The tray is what keeps the process alive when the last window closes, so also set `app.set_activation_policy` on macOS only if a window-less state is wanted there — out of scope: leave the default.

- [ ] **Step 6: Stop the last window from quitting the app**

In the `WindowEvent::Destroyed` handler from Task 2, do not exit when no windows remain; the tray holds the process. Quitting is the tray's "Выйти" and nothing else.

- [ ] **Step 7: Verify**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: PASS.
Then `bun run tauri dev`: launch the app twice (second launch opens a second window, one process in Task Manager), connect each window to a different profile, close one window and confirm the other keeps its panel, then quit from the tray and confirm no `ssh.exe` survives.

- [ ] **Step 8: Commit**

```bash
git add src-tauri src/lib
git commit -m "feat(app): one instance, a tray, and a window per connection"
```
