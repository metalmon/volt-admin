# Profile import and export — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move a connection profile between machines as a JSON document — imported from a file or the clipboard, exported to the clipboard — with one validated parse path and an explicit preview before anything is stored.

**Architecture:** Parsing and validation live in Rust (`profile_doc.rs` + `Profile::validate`), exposed as one command, `parse_profile_doc`. The same `validate` guards `save_profile`, so no route can write a malformed profile. The webview only does I/O and presentation: `<input type="file">` + `FileReader`, `navigator.clipboard`, the preview, and `JSON.stringify` for export. Validation errors cross the boundary as `field:<name>` tokens so the existing i18n renders them in the user's language without the Rust side knowing about languages.

**Tech Stack:** Rust (serde, serde_json), React + TypeScript, existing `@/lib/i18n`.

**Spec:** `docs/specs/profile-import-export.md`

## Global Constraints

- A profile document is the camelCase JSON of `connection::Profile` — no wrapper, no new format.
- There is no `password` field on `Profile` and none may be introduced; unknown fields in a document are ignored (serde default).
- `id` travels with the document; `save_profile` upserts by it. A document without `id` gets a fresh `crypto.randomUUID()` on the frontend, where that call already lives.
- Nothing is written to the store without an explicit user confirmation of the preview.
- `paircodeCommand` is shown verbatim in the preview whenever non-empty.
- RU string first, EN second, as in `src/lib/i18n.tsx`.
- No new npm or cargo dependency.

## Review Focus

- A document for a `local` profile carrying `host: "10.0.0.5"` and a `localPort` — the fields that mode ignores must be normalized away, not silently kept (Task 1).
- `port: 0` / `panelPort: 0` — serde accepts them as `u16`, so only explicit validation rejects them (Task 1).
- A document that is a JSON array of profiles — must fail with its own message, not a confusing field error (Task 1).
- A `keyFile` profile with no `keyPath` — connects today and fails at SSH time with an unrelated message (Task 1).
- Clipboard read refused by WebView2 (no user gesture / permission) — the user must be pointed at file import instead of seeing a raw exception (Task 3).

---

### Task 1: Validation and document parsing in Rust

**Files:**
- Modify: `src-tauri/src/connection.rs` (add `Profile::validate`, tests)
- Create: `src-tauri/src/profile_doc.rs` (parse, tests)
- Modify: `src-tauri/src/lib.rs` (`mod profile_doc;`)

**Interfaces:**
- Produces: `Profile::validate(&self) -> Result<(), String>` — `Err` is the bare field name (`"name"`, `"panelPort"`, `"host"`, `"user"`, `"port"`, `"keyPath"`, `"localPort"`). `id` is deliberately not checked here: an
  imported document legitimately has none, and checking it first would mask
  every other field error. `save_profile` owns that rule.
- Produces: `profile_doc::parse(text: &str) -> Result<Profile, String>` — `Err` is `"json"`, `"array"`, or a field name.

- [ ] **Step 1: Write the failing tests**

In `connection.rs`, over a `Profile` built by a local `fn valid() -> Profile` helper:

```rust
#[test] fn validate_accepts_a_complete_remote_profile()      // Ok(())
#[test] fn validate_rejects_a_blank_name()                   // name = "  " -> Err("name")
#[test] fn validate_rejects_a_zero_panel_port()              // Err("panelPort")
#[test] fn validate_rejects_a_remote_without_host_or_user()  // Err("host"), Err("user")
#[test] fn validate_rejects_key_file_auth_without_a_path()   // Err("keyPath")
#[test] fn validate_rejects_a_zero_local_port()              // Some(0) -> Err("localPort")
#[test] fn validate_ignores_remote_fields_for_local()        // local + empty host/user -> Ok(())
```

In `profile_doc.rs`:

```rust
#[test] fn parse_reads_a_full_document()            // every field lands on the Profile
#[test] fn parse_defaults_the_optional_fields()     // no id/localPort/principal/paircodeCommand
#[test] fn parse_leaves_a_missing_id_empty()        // id == "" (the frontend mints one)
#[test] fn parse_ignores_unknown_fields()           // {"password": "hunter2", ...} -> Ok, no panic
#[test] fn parse_normalizes_a_local_profile()       // host -> "127.0.0.1", local_port -> None
#[test] fn parse_rejects_an_array()                 // Err("array")
#[test] fn parse_rejects_malformed_json()           // Err("json")
#[test] fn parse_reports_the_offending_field()      // panelPort: 0 -> Err("panelPort")
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd src-tauri && cargo test profile_doc validate`
Expected: FAIL — `profile_doc` does not exist, `validate` is not a method.

- [ ] **Step 3: Implement `Profile::validate` in `connection.rs`**

Rules, in this order: `name` non-empty after trim; `panel_port != 0`; `local_port != Some(0)`; and for `ConnMode::Remote` only: `host` non-empty, `user` non-empty, `port != 0`, and `key_path` non-empty when `auth == AuthMethod::KeyFile`.

- [ ] **Step 4: Implement `profile_doc::parse` in `profile_doc.rs`**

Deserialize into a private `#[derive(Deserialize)] #[serde(rename_all = "camelCase")] struct Doc` whose `id` is `#[serde(default)] String` and whose other optional fields mirror `Profile`. Map `serde_json` failures to `"json"`, but check for a leading `[` after trimming first so an array reports `"array"`. Convert to `Profile`, then normalize `ConnMode::Local` (host `127.0.0.1`, `local_port: None`), then call `validate`.

- [ ] **Step 5: Run the tests**

Run: `cd src-tauri && cargo test`
Expected: PASS, including the pre-existing suite.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/connection.rs src-tauri/src/profile_doc.rs src-tauri/src/lib.rs
git commit -m "feat(profiles): parse and validate a profile document"
```

---

### Task 2: Expose parsing as a command and guard the store

**Files:**
- Modify: `src-tauri/src/commands.rs` (`parse_profile_doc` command; `save_profile` validates)
- Modify: `src-tauri/src/lib.rs` (`invoke_handler`)

**Interfaces:**
- Consumes: `profile_doc::parse`, `Profile::validate` (Task 1).
- Produces: command `parse_profile_doc(text: String) -> Result<Profile, String>`, erroring with `"json"`, `"array"` or `"field:<name>"`.

- [ ] **Step 1: Implement `parse_profile_doc` in `commands.rs`**

It calls `profile_doc::parse` and maps a field-name error to `format!("field:{name}")`, passing `"json"` and `"array"` through unchanged. Register it in `lib.rs`'s `invoke_handler`.

- [ ] **Step 2: Make `save_profile` reject an invalid profile**

Call `profile.validate()` before loading the store and return `format!("field:{name}")` on `Err`, so a profile that never went through the importer (a hand-edited store, a future caller) cannot be persisted either.

- [ ] **Step 3: Verify the app still builds and the suite is green**

Run: `cd src-tauri && cargo test && cargo check`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(profiles): parse_profile_doc command, validate on save"
```

---

### Task 3: Import and export in the Connect screen

**Files:**
- Modify: `src/screens/Connect.tsx`
- Modify: `src/lib/i18n.tsx`

**Interfaces:**
- Consumes: command `parse_profile_doc` (Task 2), existing `save_profile` and `list_profiles`.

- [ ] **Step 1: Add the i18n keys (RU first, then EN)**

`btn.importFile`, `btn.importClipboard`, `btn.export`, `import.title`, `import.replaces`, `import.paircodeWarning`, `import.confirm`, `export.copied`, `err.import.json`, `err.import.array`, `err.import.field` (takes `{field}`), `err.import.clipboard`.

- [ ] **Step 2: Implement `importText(text: string)`**

`invoke<Profile>('parse_profile_doc', { text })`, then hold the result in a `pending` state that renders the preview. On rejection, map the error string to a message: `field:<name>` → `t('err.import.field', { field: t(fieldLabelKey[name] ?? name) })` reusing the existing `field.*` labels, `json`/`array` → their own keys.

- [ ] **Step 3: Wire the two import sources**

A hidden `<input type="file" accept="application/json,.json">` whose `onChange` reads `file.text()` and calls `importText`; a clipboard button calling `navigator.clipboard.readText()` inside `try/catch`, surfacing `err.import.clipboard` on refusal.

- [ ] **Step 4: Implement the preview and confirmation**

Render the pending profile with the same one-line summary the profile cards use (mode, `user@host:port` or panel port). When a profile with that `id` already exists, show `import.replaces` with its name. When `paircodeCommand` is non-empty, show it verbatim under `import.paircodeWarning`. Confirming calls `save_profile` with `id: pending.id || crypto.randomUUID()`, then `refresh()`; cancelling drops the pending state.

- [ ] **Step 5: Implement export**

A button on each profile card copying `JSON.stringify(profile, null, 2)` via `navigator.clipboard.writeText`, then showing `export.copied` inline for a few seconds.

- [ ] **Step 6: Verify**

Run: `bun run build`
Expected: PASS (tsc + vite clean).
Then, in `bun run tauri dev`: export a profile, delete it, import it back from the clipboard — the profile returns with the same values; import a file with `"panelPort": 0` — the banner names the panel-port field and nothing is added.

- [ ] **Step 7: Commit**

```bash
git add src/screens/Connect.tsx src/lib/i18n.tsx
git commit -m "feat(profiles): import from file or clipboard, export to clipboard"
```
