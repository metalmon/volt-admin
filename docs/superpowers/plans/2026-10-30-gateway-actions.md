# Gateway actions — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an administrator rotate a gateway's pairing code, restart its gateway, and engage, inspect or release its emergency stop, from the profile card that already reaches it.

**Architecture:** One enum of actions, each knowing its `voltd` arguments, whether it needs a confirmation and what that confirmation warns about. One command runs any of them through the path pairing already uses — local shell or `run_ssh_command` — and returns the output as text. The UI is a submenu on the card's `☰` and a result panel; a rotate additionally forgets the pairing markers of every profile pointing at that gateway.

**Tech Stack:** Rust (existing `pair`/`tunnel` exec path, `settings` store), React + TypeScript, existing `@/lib/i18n`.

**Spec:** `docs/specs/gateway-actions.md`

## Global Constraints

- The command's OUTPUT never reaches the log: `--rotate` prints a pairing code (`docs/specs/diagnostics.md` forbids codes in the log). What is logged is the action, the profile, and success or failure.
- The binary is resolved as `pair::paircode_command` resolves it — PATH, then the `bin` of a config directory this user owns, never the container path.
- A `Remote` profile on the built-in SSH client cannot exec: those items are disabled, with the reason auto-pairing already gives.
- `--rotate` and `estop --level kill-all` confirm with what BREAKS, never "are you sure".
- RU first, EN second for every string.
- No new dependency.

## Review Focus

- **A rotate while other profiles point at the same gateway**: every one of their markers must be forgotten, not just the one that ran it — Task 2.
- **A command that fails** (`voltd` missing, gateway down, SSH refused): the user sees what it printed, starting with the reason — Task 1 and Task 4.
- **A `Local` profile whose `voltd` is not on PATH**: resolves through the config directory, as pairing does — Task 1.
- **An action on an embedded-SSH profile**: refused with a reason rather than a silent failure — Task 3.
- **The output window after a rotate**: shows the code, and the log afterwards contains no part of it — Task 4.

---

### Task 1: The actions, and running one

**Files:**
- Create: `src-tauri/src/actions.rs` (with its `#[cfg(test)] mod tests`)
- Modify: `src-tauri/src/lib.rs` (`mod actions;`)

**Interfaces:**
- Produces: `pub enum GatewayAction { RotatePairing, RestartGateway, EmergencyStop, EmergencyStatus, EmergencyResume }`, `Deserialize` with `rename_all = "camelCase"`.
- Produces: `impl GatewayAction { pub fn args(self) -> &'static str; pub fn confirms(self) -> bool; }` — `args` is what follows the binary (`"gateway get-paircode --rotate"`, `"gateway restart"`, `"estop --level kill-all"`, `"estop status"`, `"estop resume"`).
- Produces: `pub async fn run(profile: &Profile, action: GatewayAction, password: Option<&str>, embedded: bool, lang: Lang) -> Result<String, String>` — the command line is the resolved binary plus `args()`; `Local` goes through `pair::run_local`, `Remote` through `tunnel::run_ssh_command`, and an embedded tunnel is refused with the message `pair::mint_code` already uses.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn every_action_names_its_voltd_arguments()
    // RotatePairing → "gateway get-paircode --rotate", RestartGateway → "gateway restart",
    // EmergencyStop → "estop --level kill-all", EmergencyStatus → "estop status",
    // EmergencyResume → "estop resume"
#[test] fn only_the_destructive_two_confirm()
    // confirms() true for RotatePairing and EmergencyStop, false for the other three
#[test] fn a_local_command_line_uses_the_resolved_binary()
    // command_line(&local_profile, RestartGateway) ends with " gateway restart" and
    // its first token is a path or "voltd", quoted when it contains a space
```

The third needs the line without running it: split `run` so the string is built by `pub fn command_line(p: &Profile, action: GatewayAction) -> String`, which is what the test calls.

- [ ] **Step 2: Run them and watch them fail**

Run: `cd src-tauri && cargo test actions`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement `actions.rs`**

`command_line` reuses the binary resolution `pair::paircode_command` performs for `Local` (expose it from `pair` rather than copying it) and the bare name for `Remote`. `run` matches on the mode exactly as `pair::mint_code` does, including the embedded refusal, and logs the action and its outcome — never the output.

- [ ] **Step 4: Run the suite**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(actions): the three gateway actions and how to run one"
```

---

### Task 2: A rotate invalidates every marker for that gateway

**Files:**
- Modify: `src-tauri/src/settings.rs` (`forget_paired_fingerprint`, tests)

**Interfaces:**
- Consumes: `settings::paired_fingerprint`, `settings::forget_paired` (already there).
- Produces: `pub fn forget_paired_fingerprint<R: Runtime>(app: &AppHandle<R>, fingerprint: &str) -> Vec<String>` — forgets every profile whose remembered fingerprint equals this one, returning their ids.
- Produces: `pub fn profiles_with_fingerprint(paired: &serde_json::Map<String, serde_json::Value>, fingerprint: &str) -> Vec<String>` — the pure half, which is what the test exercises.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_rotate_forgets_every_profile_pointing_at_that_gateway() {
    // paired = {"a": "fp1", "b": "fp1", "c": "fp2"}; fingerprint "fp1" → ["a", "b"], never "c"
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test settings`
Expected: FAIL — `profiles_with_fingerprint` does not exist.

- [ ] **Step 3: Implement both**

- [ ] **Step 4: Run the suite**

Run: `cd src-tauri && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/settings.rs
git commit -m "feat(settings): a rotate forgets every marker for that gateway"
```

---

### Task 3: The command the UI calls

**Files:**
- Modify: `src-tauri/src/commands.rs` (`run_gateway_action`)
- Modify: `src-tauri/src/lib.rs` (`invoke_handler`)

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces: `#[command] pub async fn run_gateway_action<R: Runtime>(app: AppHandle<R>, profile: Profile, action: GatewayAction, password: Option<String>, lang: Option<String>) -> Result<String, String>` — returns the command's output; on `RotatePairing` success it first calls `settings::forget_paired_fingerprint` with the profile's `connection_fingerprint`.

- [ ] **Step 1: Implement and register it**

`embedded` comes from the registry's `hold` for a connected profile and is `false` otherwise — an action on a profile that is not connected opens no tunnel, which is why `Remote` actions need its SSH to work on their own.

- [ ] **Step 2: Verify**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src
git commit -m "feat(actions): run_gateway_action, and forget the markers a rotate invalidates"
```

---

### Task 4: The menu and the result

**Files:**
- Modify: `src/screens/Connect.tsx` (the card's `☰` menu, a result panel)
- Modify: `src/lib/i18n.tsx`

**Interfaces:**
- Consumes: command `run_gateway_action` (Task 3).

- [ ] **Step 1: Add the strings (RU first, then EN)**

`gw.title` («Шлюз»), `gw.rotate`, `gw.restart`, `gw.estop`, `gw.estopStatus`, `gw.estopResume`, `gw.confirm.rotate` (names that every paired client must pair again), `gw.confirm.estop` (names that the deployment stops), `gw.running`, `gw.failed`, `btn.close`.

- [ ] **Step 2: Add the submenu**

Under the existing `☰`, below the profile items: «Шлюз» with the five actions. The destructive two show their confirmation first, in the same preview-then-confirm shape the import uses — a question that names the consequence, a cancel and a confirm.

- [ ] **Step 3: Show the result**

A card below the profile list with the output in a `<pre>`, selectable, and a close button. It holds the text until closed: `--rotate` prints the code the administrator came for.

- [ ] **Step 4: Verify**

Run: `bun run build`
Expected: PASS.
Then, in an installed build: run «Состояние остановки» on a Local profile and read the output; run «Обновить код сопряжения» and check that the code appears, that connecting afterwards pairs again on its own, and that the log contains no part of the code.

- [ ] **Step 5: Commit**

```bash
git add src/screens/Connect.tsx src/lib/i18n.tsx
git commit -m "feat(actions): a Шлюз submenu, with the result where it can be read"
```
