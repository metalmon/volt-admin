<div align="center">

<img src="src/assets/logo.svg" alt="Volt Admin" width="96" height="96">

# Volt Admin

**Desktop client for managing the Volt panel — locally or remotely over SSH.**

[![Build](https://github.com/metalmon/volt-admin/actions/workflows/build-all.yml/badge.svg)](https://github.com/metalmon/volt-admin/actions/workflows/build-all.yml)
![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux%20%7C%20macOS-2b2b2b)
![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB)

</div>

---

Volt Admin is a thin desktop client for the Volt admin panel (the ZeroClaw
daemon). It opens the panel as a native window and adds what the browser panel
lacks: saved connection profiles, secure access to a remote daemon through an
SSH tunnel, and running several independent copies at once.

## Features

- **Connection profiles.** Save local and remote connections; the list is
  stored locally and survives restarts.
- **Local mode.** Instant connection to a daemon on the same machine
  (`http://127.0.0.1:<panel port>`).
- **Remote mode over SSH.** Reach a remote daemon's panel through an encrypted
  SSH tunnel. The panel listens only on the server's loopback and is **never**
  exposed to the network — the tunnel forwards it straight to you.
- **Built-in SSH — no OpenSSH required.** The system `ssh` client is used by
  default; if it is missing (a bare Windows install, say), the app
  automatically falls back to a **built-in SSH client** compiled into the
  binary. An unprepared machine needs nothing installed.
- **Three sign-in methods:** key (default — from `ssh-agent` / `~/.ssh`), key
  file, or password. **Passwords are never stored** — they are requested at
  connect time and live only in process memory.
- **Multiple copies.** Launch several windows at once — each with its own
  connection; there is no single-instance lock.
- **Cross-platform.** Installers for Windows (NSIS `.exe` + `.msi`), Linux
  (`.deb` + `.rpm`), and macOS (universal `.dmg`, arm64 + x86_64).

> The panel's user interface is in Russian; Volt Admin is a shell around it.

## Installation

Prebuilt installers are on the [Releases](https://github.com/metalmon/volt-admin/releases) page:

| OS | File |
|----|------|
| Windows | `Volt.Admin_<version>_x64-setup.exe.zip` (or the `.msi` for GPO/SCCM) |
| Linux (Debian/Ubuntu/Astra) | `Volt.Admin_<version>_amd64.deb` |
| Linux (RED OS/Fedora/RPM) | `Volt.Admin-<version>-1.x86_64.rpm` |
| macOS (Apple Silicon + Intel) | `Volt.Admin_<version>_universal.dmg` |

> Installers are not code-signed yet, so the first launch may trigger a
> Windows SmartScreen or macOS Gatekeeper warning — expected until signing
> certificates are in place.

## Usage

1. Launch the app — the profile list opens.
2. **New profile → Local** for a daemon on this machine, or **Remote** for a
   remote one. For remote, set the host, SSH port, user, sign-in method, and
   panel port (default `42617`).
3. Click **Connect**. For password sign-in, the password field appears on the
   profile card at connect time (the password is never written anywhere).

## Building from source

Requires [Rust](https://www.rust-lang.org/tools/install), [Bun](https://bun.sh),
and the [Tauri 2 system prerequisites](https://tauri.app/start/prerequisites/)
for your OS.

```bash
bun install
bun run tauri build      # installers → src-tauri/target/release/bundle/
bun run tauri dev        # run in development mode
```

The built-in SSH client uses [`russh`](https://github.com/eugeny/russh) with the
`ring` crypto backend (no external C dependencies), so the build is identical
across all three platforms.

## Security

- A remote daemon's panel is reachable **only** through the SSH tunnel to the
  server's loopback; it is never exposed on network interfaces.
- Host-key checking follows the *accept-new* policy: a changed key for an
  already-known host is rejected (MITM protection), a new host is accepted on
  first connection.
- Passwords are never written to disk or logged — only held transiently in
  process memory for the duration of the connection.

## Built with

[Tauri 2](https://tauri.app) · Rust · React · TypeScript · Vite ·
[russh](https://github.com/eugeny/russh)

## License

© 2026 metalmon. All rights reserved. The source is published for reference;
usage terms are to be determined — see [`LICENSE`](LICENSE).
