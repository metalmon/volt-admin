# Spec: in-app updates

**Status:** approved, not implemented — needs a signing key decision first
**Owner:** launcher (`lib.rs`, `tauri.conf.json`, `build-all.yml`)

## Problem

Shipping a fix to the pilot today means: build the installer, copy it into
`C:\pilot-kit\volt-admin`, recompute `checksums.sha256`, edit the kit README,
then get every administrator to run the installer again. We did exactly this
for 0.1.5. It does not scale past a handful of machines, and in practice
means pilots keep running whatever they installed first — the 0.1.4 build sat
there while the repo moved on.

## Goal

An administrator running Volt Admin learns that a newer version exists and
installs it from inside the app, without the kit being re-cut.

## Behavior

**Check.** On startup, once, in the background. A failed check (no network,
corporate proxy, DNS blocked) is silent: it writes a line to the log and
changes nothing on screen. The launcher must stay fully usable offline — an
air-gapped pilot is a normal deployment, not an error.

**Prompt.** When a newer version is found, a dismissible bar on the Connect
screen: the new version number, a short "what changed" line from the release
notes, "Обновить" and "Позже". Never download or install without that click.

**Install.** `tauri-plugin-updater` downloads and verifies the signature, then
installs. On Windows the NSIS updater restarts the app itself. An update is
never applied while a tunnel is open: if a connection is active the bar says
the update will be applied after disconnecting.

**Channel.** GitHub Releases on `metalmon/volt-admin`. The existing `release`
job in `build-all.yml` already builds a draft release from the platform
artifacts on a `v*` tag; it gains the updater manifest (`latest.json`) and the
per-platform signatures. Only published (non-draft) releases are offered —
drafts stay invisible to clients.

**Disabling.** A pilot that must not reach the internet can turn the check off
in the app; the setting lives in the existing `store` plugin's config. An
installation-wide switch is out of scope until someone asks.

## Signing

`tauri-plugin-updater` requires a keypair. The private key lives in a GitHub
Actions secret (`TAURI_SIGNING_PRIVATE_KEY` + password); the public key goes
into `tauri.conf.json` and ships inside the app. Losing the private key means
no existing install can ever be updated again — it must be backed up outside
CI before the first signed release.

This is the one decision that blocks the work, and it is not reversible after
the first shipped release.

## Out of scope

- Delta updates, rollback, staged rollouts, update channels (beta/stable).
- Self-hosted update endpoints. If GitHub is unreachable in a pilot, that
  pilot keeps using the kit installer.
- Updating anything but the launcher: `voltd` itself has its own lifecycle.

## Copy (i18n keys to add)

`update.available`, `update.install`, `update.later`, `update.afterDisconnect`,
`update.failed`, `settings.checkUpdates` — RU and EN.

## Done when

- A build tagged `v0.1.6` and published as a release is offered to a running
  0.1.5 installation, and installs after one click.
- With the network blocked, startup is unchanged and nothing is shown.
- With a connection open, the update is deferred until after disconnect.
- A release whose signature does not match the shipped public key is refused.
