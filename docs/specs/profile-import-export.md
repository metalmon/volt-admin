# Spec: profile import and export

**Status:** implemented (`6a9925e`…`6936abf`)
**Owner:** launcher (Connect screen + `connection.rs`)

## Problem

A pilot administrator types seven fields by hand to reach a server: host, SSH
port, user, auth method, key path, panel port, principal. Nothing validates
the result, and a wrong value fails at connect time with an SSH-level error
that does not name the field. The default panel port was one digit off
(42627 instead of 42617) for several releases and nobody noticed, because
every profile is typed from scratch.

The kit (`C:\pilot-kit\volt-admin`) already ships per-machine helpers
(`ssh-key.ps1`, `ssh-key.sh`). A profile is the same kind of artifact: it is
prepared once by whoever set up the server and consumed verbatim on every
admin machine.

## Goal

Move a connection profile between machines as text: a file the kit ships, or
a string pasted into the app. One parse path, one preview, one confirmation.

## Format

A profile is a JSON object with exactly the field names `save_profile`
already takes (serde `rename_all = "camelCase"` over `connection::Profile`).
No new format, no wrapper envelope:

```json
{
  "id": "pilot-backend",
  "name": "VoltSSH",
  "mode": "remote",
  "host": "backend.volt.oktaplus.ru",
  "user": "volt",
  "port": 2222,
  "panelPort": 42617,
  "localPort": null,
  "auth": "keyFile",
  "keyPath": "~/.ssh/volt_pilot",
  "principal": "admin",
  "paircodeCommand": null
}
```

| field | type | required | notes |
|---|---|---|---|
| `id` | string | no | absent → a fresh UUID is generated |
| `name` | string | yes | non-empty after trim |
| `mode` | `"local"` \| `"remote"` | yes | |
| `host` | string | yes for `remote` | `local` is forced to `127.0.0.1` |
| `user` | string | yes for `remote` | |
| `port` | 1–65535 | yes for `remote` | SSH port |
| `panelPort` | 1–65535 | yes | |
| `localPort` | 1–65535 \| null | no | `remote` only; null = auto-pick |
| `auth` | `"agent"` \| `"keyFile"` \| `"password"` | yes for `remote` | |
| `keyPath` | string \| null | yes for `auth: "keyFile"` | |
| `principal` | string \| null | no | |
| `paircodeCommand` | string \| null | no | **executed** — see Security |

Unknown fields are ignored (serde default). There is no `password` field on
`Profile` and there must never be one in an exported or imported document —
`connection.rs` already has a test asserting a profile never serializes a
password.

`id` travels with the profile. `save_profile` upserts by id, so re-importing
a corrected profile updates it in place instead of creating a duplicate. A
kit that ships `"id": "pilot-backend"` can therefore reissue the file when a
value changes.

A document holds exactly one profile. Arrays are rejected with a clear
message (see Out of scope).

## Behavior

Three entry points, one pipeline: **parse → validate → preview → confirm →
`save_profile` → refresh list**.

**Import from file.** A button in the profile list (and in the empty state,
where it is the primary call to action next to "add profile"). Opens a native
file picker via `<input type="file" accept="application/json,.json">` in the
webview; the content is read with `FileReader`. No `tauri-plugin-dialog`
dependency.

**Import from clipboard.** A button next to it. Reads with
`navigator.clipboard.readText()`. If WebView2 refuses the read (no user
gesture, permission denied), the error message tells the user to use the file
import instead; adding `tauri-plugin-clipboard-manager` is the fallback only
if this proves unreliable in the pilot.

**Export to clipboard.** An action on each existing profile. Serializes that
profile — including its `id` — with `navigator.clipboard.writeText()`, then
confirms inline ("profile copied"). Export to file is not needed: the user
pastes into whatever channel they are already using.

**Preview.** Before anything is written, show what will be imported, as the
profile card renders it, plus the explicit warning case below:

```
Импортировать профиль?
VoltSSH · По сети · volt@backend.volt.oktaplus.ru:2222 · порт панели 42617
Команда кода сопряжения: docker exec voltd voltd gateway get-paircode …
```

When a profile with the same `id` already exists, the preview says the
existing profile will be replaced and names it.

**Validation failures** are reported in the existing error banner, naming the
field: malformed JSON, missing required field, port out of range, unknown
`mode`/`auth` value, array instead of object, empty name.

## Security

`paircodeCommand` is a shell command the app later executes — `pair::mint_code`
runs it through `run_local` for `Local` profiles and `run_ssh_command` for
`Remote` ones. An imported profile is therefore imported code, which fixes
two rules:

1. **No implicit import.** The app never reads a profile that merely happens
   to sit next to the executable, in the Downloads folder, or in the config
   directory. Every import is a user action in the app.
2. **The preview shows everything the import decides**: the pairing-code
   command verbatim whenever non-empty (labelled with *where* it will run —
   for a `local` profile that is this computer), plus the auth method, key
   path, principal and tunnel port. What the dialog hides, it grants
   silently.
3. **Fields that reach a shell or an argv are constrained**, because an
   imported document is untrusted input: `user`/`host` may not start with
   `-` (ssh would read `-oProxyCommand=…` as an option and run it locally)
   or contain whitespace, `@` or control characters; `principal` is
   `[A-Za-z0-9_.-]{1,64}`; `keyPath` may not be a UNC or device path (`ssh
   -i \\host\share\key` leaks an NTLM hash to that host); and neither
   `keyPath` nor `paircodeCommand` may carry newlines, bidi overrides or
   zero-width characters, which would let the preview render as less than
   what runs.

Validation happens on the Rust side in `save_profile`'s existing path where
it can (port ranges, enum values, required fields) so a malformed document
cannot be written to the store by any route.

## Known limitation: profiles share one webview origin

Accepted deliberately, recorded so the next reader does not have to
rediscover it.

A connected panel lives at `http://127.0.0.1:<local port>`, and the webview
keys its storage — the panel's session among it — by that origin. Two
profiles that end up on the same local port therefore share that storage: an
imported profile whose `localPort` is set to another profile's port can,
once the user connects to it, read what the other panel left there. The same
origin is also what `crate::pair` checks before injecting the gateway admin
token into the page (`window.__voltAdminToken`), so a collision hands that
token to whatever is answering on the port.

What holds today: importing is explicit and the preview shows the tunnel
port, and both profiles have to be ones the user chose to connect to.

What would actually fix it, in increasing order of work: refuse a profile
whose explicit `localPort` equals another saved profile's port (partial — an
auto-picked port is derived from the profile id, so a collision can still be
engineered); give each profile its own webview data directory, so no two
panels ever share storage; and stop handing the page a long-lived
credential at all — the panel's "connect a device" button can ask for a
result the way `PROBE_SCRIPT` already does, by navigating to a marker the
app answers with a freshly minted one-time code, leaving the token in Rust.

## Out of scope

- `volt-admin://` deep links (needs protocol registration in the installer).
- Multiple profiles per document, and bulk import.
- Encryption, signing, or any secret in the document (there is nothing secret
  in a profile: passwords are prompted at connect time and never stored).
- Auto-import of a kit-provided file. If zero-click setup is ever wanted, the
  safe shape is a file whose hash is in the kit's `checksums.sha256`, not a
  neighbour-of-the-exe convention.

## Copy (i18n keys to add)

`btn.importFile`, `btn.importClipboard`, `btn.export`, `import.title`,
`import.replaces`, `import.paircodeWarning`, `import.confirm`, `export.copied`,
`err.import.json`, `err.import.field`, `err.import.clipboard`,
`err.import.array` — each with a RU and an EN string, RU first as elsewhere.

## Done when

- A profile exported from one machine and imported on another produces an
  identical profile (same values, same `id`) and connects.
- Re-importing an edited document with the same `id` updates the existing
  profile instead of adding a second one.
- A document with a bad port, a bad `mode`, or broken JSON is refused with a
  message naming the problem, and nothing is written to the store.
- A profile carrying a `paircodeCommand` shows that command in the preview,
  with its key path, principal and auth method, before it can be saved.
- A document whose `host` is `-oProxyCommand=calc.exe`, whose `keyPath` is
  `\\10.0.0.9\share\id`, or whose command hides a second line behind a
  bidi override is refused by name, on import and on save alike.
- The kit can ship `profile.json`, and a README line pointing at "import
  profile" replaces the manual seven-field walkthrough.
