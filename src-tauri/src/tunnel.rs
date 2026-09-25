//! SSH tunnel manager for Remote-mode connections.
//!
//! Spawns `ssh -N -L <localport>:127.0.0.1:<panel_port> ...` to forward a
//! free local port to the `voltd` panel port listening on the *server's*
//! loopback interface (never `0.0.0.0` — the tunnel must not expose the
//! panel on the server's LAN-facing interfaces). `Local` mode never goes
//! through this module: it returns `http://127.0.0.1:<panel_port>` directly.
//!
//! `StrictHostKeyChecking=accept-new` lets a first-ever connection to a
//! profile's host proceed without an interactive prompt (there is no tty)
//! while still rejecting a *changed* key; `ExitOnForwardFailure=yes` makes a
//! refused `-L` bind fail fast instead of waiting out the full connect
//! timeout. `ssh`'s stderr is captured in the background so a failure can
//! be explained instead of surfacing only an exit status or a bare timeout.
//!
//! Passwords (for `AuthMethod::Password`) are never persisted. They are
//! collected transiently by the caller and handed to `open_tunnel`, which
//! feeds them to `ssh` via a per-tunnel `SSH_ASKPASS` helper and an
//! environment variable set only on the spawned child process. The askpass
//! helper script is removed again when the tunnel closes.

use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::connection::{AuthMethod, Profile};

/// How long `open_tunnel` waits for the forwarded port to accept a TCP
/// connection before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Errors that can occur while opening or maintaining an SSH tunnel.
#[derive(Debug)]
pub enum TunnelError {
    Io(io::Error),
    /// The forwarded port never accepted a connection within the timeout.
    /// Carries a (possibly empty) snapshot of ssh's stderr captured up to
    /// that point, so the UI can show *why* (e.g. a stuck auth prompt)
    /// instead of just "timed out".
    Timeout(String),
    /// The `ssh` process exited before the tunnel came up. Carries the
    /// exit status plus a (possibly empty) capture of ssh's stderr, so the
    /// UI can show the real reason (bad host, auth failed, forward
    /// refused) instead of a bare status code.
    ProcessExited(std::process::ExitStatus, String),
    /// `AuthMethod::Password` was requested but no password was supplied.
    MissingPassword,
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TunnelError::Io(e) => write!(f, "tunnel I/O error: {e}"),
            TunnelError::Timeout(detail) => {
                if detail.is_empty() {
                    write!(f, "timed out waiting for the SSH tunnel to come up")
                } else {
                    write!(f, "timed out waiting for the SSH tunnel to come up: {detail}")
                }
            }
            TunnelError::ProcessExited(status, detail) => {
                if detail.is_empty() {
                    write!(f, "ssh exited before the tunnel came up (status: {status})")
                } else {
                    write!(
                        f,
                        "ssh exited before the tunnel came up (status: {status}): {detail}"
                    )
                }
            }
            TunnelError::MissingPassword => {
                write!(f, "password auth selected but no password was supplied")
            }
        }
    }
}

impl std::error::Error for TunnelError {}

impl From<io::Error> for TunnelError {
    fn from(e: io::Error) -> Self {
        TunnelError::Io(e)
    }
}

/// Build the `ssh` argument vector for forwarding `localport` to the
/// server's `127.0.0.1:<panel_port>`. Pure function, no I/O — this is what
/// `ssh_args_forward_loopback_only` pins.
pub fn build_ssh_args(p: &Profile, localport: u16) -> Vec<String> {
    let mut args = vec![
        "-N".to_string(),
        "-L".to_string(),
        format!("{localport}:127.0.0.1:{}", p.panel_port),
        "-p".to_string(),
        p.port.to_string(),
        // Accept and remember a *new* host key without an interactive
        // prompt (there is no tty — stdin is Stdio::null() below), so the
        // first connection to a profile's host doesn't fail closed with no
        // path forward from inside the app. A *changed* key is still
        // rejected — this does not weaken protection against MITM/host
        // spoofing on hosts already known to this client.
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        // If the `-L` forward can't bind (e.g. voltd isn't listening on
        // panel_port yet), exit immediately instead of leaving ssh up and
        // making the caller wait the full CONNECT_TIMEOUT.
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        // Bound the TCP/handshake phase so a dead/firewalled host fails in
        // seconds, not by falling through to CONNECT_TIMEOUT.
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
    ];

    if p.auth == AuthMethod::KeyFile {
        if let Some(key) = &p.key_path {
            args.push("-i".to_string());
            args.push(key.clone());
        }
    }
    // AuthMethod::Agent delegates to ssh-agent: no extra args.
    // AuthMethod::Password is fed via SSH_ASKPASS at spawn time (see
    // open_tunnel), not via argv.

    args.push(format!("{}@{}", p.user, p.host));
    args
}

/// An open SSH tunnel: the `ssh -N -L ...` child process plus the local
/// port it is forwarding. Dropping (or explicitly `close`-ing) a `Tunnel`
/// kills the child — there must be no orphan `ssh` processes.
pub struct Tunnel {
    child: Option<Child>,
    pub local_port: u16,
    askpass_path: Option<PathBuf>,
}

impl Tunnel {
    /// Explicitly tear down the tunnel: kill the `ssh` child and remove the
    /// askpass helper, if any. Safe to call more than once.
    pub fn close(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Best-effort: the child may have already exited on its own.
            let _ = child.start_kill();
        }
        if let Some(path) = self.askpass_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.close();
    }
}

/// Tauri-managed state holding the single active tunnel, if any. Replacing
/// or clearing the `Option` drops (and thus tears down) the previous
/// tunnel automatically.
#[derive(Default)]
pub struct TunnelState(pub Mutex<Option<Tunnel>>);

/// Open an SSH tunnel for a `Remote`-mode profile: pick a free local port,
/// spawn `ssh -N -L ...`, and wait until the forwarded port accepts a TCP
/// connection (or time out and error).
///
/// `password` is only consulted when `p.auth == AuthMethod::Password`; it
/// is never written to disk — it is passed to the `ssh` child only via a
/// process-local environment variable read by a per-tunnel askpass helper.
pub async fn open_tunnel(p: &Profile, password: Option<&str>) -> Result<Tunnel, TunnelError> {
    let local_port = pick_free_port()?;
    let args = build_ssh_args(p, local_port);

    let mut cmd = Command::new("ssh");
    cmd.args(&args);
    cmd.kill_on_drop(true);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    // Piped (not null) so a failure can be explained to the UI instead of
    // just a bare exit status — see `spawn_stderr_reader`.
    cmd.stderr(Stdio::piped());

    let mut askpass_path = None;
    if p.auth == AuthMethod::Password {
        let pass = password.ok_or(TunnelError::MissingPassword)?;
        let script = write_askpass_script(local_port)?;
        // SSH_ASKPASS_REQUIRE=force (OpenSSH 8.4+) makes ssh use the
        // askpass helper even without a DISPLAY / tty, which is what we
        // want since stdin is Stdio::null() above.
        cmd.env("SSH_ASKPASS_REQUIRE", "force");
        cmd.env("SSH_ASKPASS", &script);
        cmd.env("VOLT_SSH_PASSWORD", pass);
        askpass_path = Some(script);
    }

    let mut child = cmd.spawn()?;
    let stderr_buf = child.stderr.take().map(spawn_stderr_reader);

    if let Err(e) = wait_for_forward(local_port, &mut child, stderr_buf.as_ref()).await {
        let _ = child.start_kill();
        if let Some(path) = askpass_path {
            let _ = std::fs::remove_file(path);
        }
        return Err(annotate_windows_password_hint(e, p));
    }

    Ok(Tunnel {
        child: Some(child),
        local_port,
        askpass_path,
    })
}

/// Bind an ephemeral port on the loopback interface to discover a free
/// local port, then release it immediately so `ssh` can bind it. There is
/// an inherent (small) TOCTOU race between the release and `ssh` binding
/// the same port; acceptable for a single-user desktop tool.
fn pick_free_port() -> Result<u16, TunnelError> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Poll the forwarded local port until it accepts a TCP connection, the
/// child process exits (error), or `CONNECT_TIMEOUT` elapses (error).
async fn wait_for_forward(
    local_port: u16,
    child: &mut Child,
    stderr_buf: Option<&StderrBuf>,
) -> Result<(), TunnelError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(TunnelError::ProcessExited(
                status,
                stderr_snapshot(stderr_buf).await,
            ));
        }
        if TcpStream::connect(("127.0.0.1", local_port)).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(TunnelError::Timeout(stderr_snapshot(stderr_buf).await));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Shared buffer that a background task fills with `ssh`'s stderr as it
/// arrives, so a failed connect can be explained (bad host, auth failure,
/// forward refused, ...) instead of surfacing only an exit status or a bare
/// timeout.
type StderrBuf = std::sync::Arc<Mutex<String>>;

/// Spawn a background task that drains `stderr` into a shared buffer.
/// Reading stops (and the task ends) once the pipe is closed, which happens
/// when the child process exits — this never blocks the caller.
fn spawn_stderr_reader(mut stderr: tokio::process::ChildStderr) -> StderrBuf {
    use tokio::io::AsyncReadExt;
    let buf: StderrBuf = std::sync::Arc::new(Mutex::new(String::new()));
    let buf_writer = buf.clone();
    tokio::spawn(async move {
        let mut chunk = [0u8; 4096];
        loop {
            match stderr.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&chunk[..n]);
                    buf_writer.lock().await.push_str(&text);
                }
            }
        }
    });
    buf
}

/// Snapshot whatever `ssh` has printed to stderr so far, trimmed for
/// display. Empty when there is no buffer (shouldn't happen — stderr is
/// always piped) or nothing has been written yet.
async fn stderr_snapshot(buf: Option<&StderrBuf>) -> String {
    match buf {
        Some(b) => b.lock().await.trim().to_string(),
        None => String::new(),
    }
}

/// On Windows, `AuthMethod::Password` relies on `SSH_ASKPASS`, which
/// Win32-OpenSSH has historically not honored — the child can sit waiting
/// on a console prompt it never gets forever, surfacing as a plain
/// `Timeout`. Make that failure mode actionable instead of mysterious.
///
/// NOTE: this path needs live verification against a real Windows OpenSSH
/// client; Key/Agent auth are unaffected and not touched here.
#[cfg(windows)]
fn annotate_windows_password_hint(e: TunnelError, p: &Profile) -> TunnelError {
    if p.auth != AuthMethod::Password {
        return e;
    }
    const HINT: &str = "password auth may not work from a Windows client (Win32-OpenSSH does not reliably support SSH_ASKPASS) — try Key or Agent auth instead";
    match e {
        TunnelError::Timeout(detail) if detail.is_empty() => {
            TunnelError::Timeout(HINT.to_string())
        }
        TunnelError::Timeout(detail) => TunnelError::Timeout(format!("{detail} ({HINT})")),
        other => other,
    }
}

#[cfg(not(windows))]
fn annotate_windows_password_hint(e: TunnelError, _p: &Profile) -> TunnelError {
    e
}

/// Write a per-tunnel askpass helper that prints the password read from
/// `VOLT_SSH_PASSWORD` to stdout (the protocol `ssh` expects of
/// `SSH_ASKPASS` helpers). The password itself is never written into the
/// script — only the env-var name is.
#[cfg(unix)]
fn write_askpass_script(local_port: u16) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path =
        std::env::temp_dir().join(format!("volt-admin-askpass-{}-{}.sh", std::process::id(), local_port));
    std::fs::write(&path, b"#!/bin/sh\nprintf '%s' \"$VOLT_SSH_PASSWORD\"\n")?;
    let mut perms = std::fs::metadata(&path)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(&path, perms)?;
    Ok(path)
}

// NOTE: Win32-OpenSSH has historically not honored SSH_ASKPASS at all (it
// prefers its own console/UI prompt when a tty is available), so the
// password-auth path on a Windows client needs live verification — it may
// simply never invoke this script. See `annotate_windows_password_hint`
// for the user-facing fallback if the tunnel times out under
// `AuthMethod::Password` on Windows. Key and Agent auth do not go through
// this helper and are unaffected.
#[cfg(windows)]
fn write_askpass_script(local_port: u16) -> io::Result<PathBuf> {
    let path =
        std::env::temp_dir().join(format!("volt-admin-askpass-{}-{}.cmd", std::process::id(), local_port));
    // Deliberately NOT `echo %VOLT_SSH_PASSWORD%`: cmd expands the variable
    // and then re-parses the resulting line, so a password containing
    // `&`/`|`/`^`/`<`/`>` would be interpreted as shell syntax rather than
    // emitted literally, and an *empty* value collapses to a bare `echo`
    // that prints "ECHO is on." instead of nothing. Delegating to
    // PowerShell avoids both: `$env:VOLT_SSH_PASSWORD` is resolved by
    // PowerShell against its own environment, never substituted into (and
    // re-tokenized by) the cmd command line, and `Console::Out.Write`
    // writes the value verbatim — including empty.
    std::fs::write(
        &path,
        b"@echo off\r\npowershell -NoProfile -NonInteractive -Command \"[Console]::Out.Write($env:VOLT_SSH_PASSWORD)\"\r\n",
    )?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnMode;

    fn test_profile() -> Profile {
        Profile {
            id: "id".into(),
            name: "name".into(),
            mode: ConnMode::Remote,
            host: "host".into(),
            user: "user".into(),
            port: 22,
            panel_port: 8080,
            auth: AuthMethod::Agent,
            key_path: None,
        }
    }

    #[test]
    fn ssh_args_forward_loopback_only() {
        let p = Profile {
            mode: ConnMode::Remote,
            host: "srv".into(),
            user: "adm".into(),
            port: 2222,
            panel_port: 42627,
            auth: AuthMethod::KeyFile,
            key_path: Some("/k".into()),
            ..test_profile()
        };
        let a = build_ssh_args(&p, 8081);
        assert!(a.iter().any(|s| s == "-N"));
        assert!(a.contains(&"-L".to_string()));
        assert!(a.iter().any(|s| s == "8081:127.0.0.1:42627")); // forward to server LOOPBACK, not 0.0.0.0
        assert!(a.iter().any(|s| s == "-i") && a.iter().any(|s| s == "/k"));
        assert!(a.iter().any(|s| s == "adm@srv"));
        assert!(a.iter().any(|s| s == "-p") && a.iter().any(|s| s == "2222"));
        // I1/M2: new-host-key handling and fast forward-failure detection.
        assert!(a.iter().any(|s| s == "StrictHostKeyChecking=accept-new"));
        assert!(a.iter().any(|s| s == "ExitOnForwardFailure=yes"));
        assert!(a.iter().any(|s| s == "ConnectTimeout=10"));
        assert!(!a.iter().any(|s| s.starts_with("BatchMode"))); // must not disable password-auth prompt path
    }

    #[test]
    fn agent_auth_has_no_key_flag() {
        let p = Profile {
            auth: AuthMethod::Agent,
            key_path: None,
            ..test_profile()
        };
        let a = build_ssh_args(&p, 8081);
        assert!(!a.iter().any(|s| s == "-i"));
    }

    #[test]
    fn password_auth_has_no_key_flag_either() {
        let p = Profile {
            auth: AuthMethod::Password,
            key_path: None,
            ..test_profile()
        };
        let a = build_ssh_args(&p, 8081);
        assert!(!a.iter().any(|s| s == "-i"));
    }
}
