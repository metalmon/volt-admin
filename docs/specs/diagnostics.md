# Spec: diagnostics — the log a pilot user can actually hand over

**Status:** implemented
**Owner:** launcher (`logx.rs`, Connect screen)

## Problem

`logx::log` already writes an append-only debug log, but it is unusable as a
support tool:

- The path is built from `%APPDATA%` directly
  (`%APPDATA%\ru.metalmon.volt-admin\debug.log`). On Linux and macOS that
  variable is unset, so the whole log silently goes to `./debug.log` next to
  the working directory — or nowhere useful.
- Nothing in the UI mentions the file. A pilot user who cannot connect has no
  way to know it exists, let alone where it is.
- The file grows forever. There is no cap and no rotation.
- Connection errors are shown in a banner that disappears on the next action,
  so by the time the user writes a message, the text is gone.

The result is the loop we keep paying for: "у меня не подключается" → several
rounds of questions → a screenshot of a banner.

## Goal

A user who hits a connection problem can produce the diagnostic text in one
click, without being told where to look.

## Behavior

**Cross-platform log location.** `logx` resolves its directory through Tauri's
`app.path().app_log_dir()` instead of `%APPDATA%`, so the file lands in the
platform's log directory on all three targets. The resolved path is computed
once at startup and held, because `logx::log` is called from places with no
`AppHandle`; resolution failure falls back to the current behavior (the log is
best-effort and must never break the app).

**Size cap.** Before appending, if the file exceeds 1 MB, it is truncated to
its last ~200 lines. No rotation, no second file — this is a support artifact
for the last session, not an audit trail.

**Diagnostics UI.** A small section at the bottom of the Connect screen,
collapsed by default, with:

- **"Открыть папку с журналом"** — opens the log directory with the `opener`
  plugin (already a dependency).
- **"Скопировать диагностику"** — copies to the clipboard: app version, OS and
  architecture, which `ssh` binary was found (or that it was not), the active
  profile's mode/host/panel port (never the key path's contents, never a
  password), and the last 50 log lines.

**Last error is sticky.** The most recent connection error stays visible in
the diagnostics section after the banner is dismissed, so it is still there
when the user goes to copy the diagnostics.

**What gets logged.** Connection lifecycle (tunnel open/close with the local
port, ssh child exit codes and stderr tails), page loads on the panel origin,
pairing attempts and failures.

**What must never be logged or copied:** the gateway admin token, a pairing
code, a password, the contents of a key file. Pairing code minting logs the
outcome, not the code. This is a rule on the call sites; `logx` itself stays
dependency-free and does no redaction.

## Out of scope

- A log viewer inside the app (the folder button plus clipboard is enough).
- Log levels, structured logging, a logging crate.
- Uploading diagnostics anywhere. The user pastes the text into whatever
  channel they already use.

## Copy (i18n keys to add)

`diag.title`, `diag.openFolder`, `diag.copy`, `diag.copied`, `diag.lastError`
— RU and EN.

## Done when

- The log file exists at the platform log directory on Windows, Linux and
  macOS, and the app says where it is.
- "Скопировать диагностику" yields a block that names the app version, the OS,
  the ssh binary, the profile's mode/host/panel port, and the last 50 lines.
- Searching the copied text for a known token, pairing code or password finds
  nothing.
- A log file larger than 1 MB is truncated on the next write instead of
  growing.
