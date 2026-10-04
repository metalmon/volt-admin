//! Embedded SSH tunnel — the self-contained fallback used when the system
//! `ssh` client is not installed.
//!
//! Remote mode normally drives the system `ssh` binary (see `tunnel.rs`). On a
//! machine with no OpenSSH client installed that path fails with
//! `TunnelError::SshNotFound`; rather than dead-end an unprepared user, we fall
//! back to this `russh`-based client compiled into the binary. It mirrors what
//! the system path does: forward a free local port to the server's
//! `127.0.0.1:<panel_port>` through an SSH `direct-tcpip` channel.
//!
//! Scope: **Password** and **KeyFile** auth — the cases an unprepared user
//! actually hits. **Agent** auth is intentionally left on the system-ssh path:
//! a user with a loaded ssh-agent already has SSH infrastructure installed.
//!
//! Host-key policy mirrors the system path's `StrictHostKeyChecking=accept-new`:
//! a host already in `known_hosts` must match (a changed key is rejected —
//! MITM protection), while an unknown host is accepted and recorded on first
//! use.

use std::sync::Arc;

use russh::client::{self, Handle};
use russh::keys::{load_secret_key, HashAlg, PrivateKeyWithHashAlg};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::connection::{AuthMethod, Profile};
use crate::tunnel::{Lang, TunnelError};

/// Build a localized `Embedded` error. Both messages are formatted eagerly
/// (they are cheap) and the one for `lang` is kept.
fn emb(lang: Lang, ru: String, en: String) -> TunnelError {
    TunnelError::Embedded(match lang {
        Lang::Ru => ru,
        Lang::En => en,
    })
}

/// A running embedded tunnel: the local listener's accept loop plus the live
/// `russh` session it forwards through. Aborting the task (on `close`/drop)
/// drops the session and closes the listener — no orphaned connections.
#[derive(Debug)]
pub struct EmbeddedTunnel {
    pub local_port: u16,
    accept_task: JoinHandle<()>,
}

impl EmbeddedTunnel {
    /// Tear down the tunnel: stop accepting and drop the SSH session. Safe to
    /// call more than once.
    pub fn close(&mut self) {
        self.accept_task.abort();
    }
}

impl Drop for EmbeddedTunnel {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// `russh` client handler carrying the target host/port so host-key decisions
/// can be recorded against the right `known_hosts` entry.
struct Handler {
    host: String,
    port: u16,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        // accept-new (russh has no known_hosts *writer*, so we cannot persist
        // a first-seen key): a host already pinned in known_hosts — e.g. by the
        // system ssh client — must still match; a *changed* key is rejected
        // (MITM protection); an unknown host is accepted on first use.
        match russh::keys::check_known_hosts(&self.host, self.port, &key) {
            Ok(true) => Ok(true),
            Ok(false) => Ok(true),
            Err(russh::keys::Error::KeyChanged { .. }) => Ok(false),
            // No known_hosts file yet / unreadable — treat as a fresh host.
            Err(_) => Ok(true),
        }
    }
}

/// Open an embedded tunnel for a `Remote`-mode profile: connect, authenticate,
/// verify the panel port is reachable, then start forwarding a free local port.
///
/// `password` is consulted only for `AuthMethod::Password`; it is never
/// persisted.
pub async fn open_embedded_tunnel(
    p: &Profile,
    password: Option<&str>,
    lang: Lang,
) -> Result<EmbeddedTunnel, TunnelError> {
    if p.auth == AuthMethod::Agent {
        return Err(emb(
            lang,
            "вход через ssh-agent не поддерживается встроенным SSH-клиентом — установите системный клиент OpenSSH либо используйте файл ключа или пароль".to_string(),
            "ssh-agent auth is not supported by the built-in SSH client — install the system OpenSSH client, or use a key file or password".to_string(),
        ));
    }

    let config = Arc::new(client::Config::default());
    let handler = Handler {
        host: p.host.clone(),
        port: p.port,
    };

    let tcp = TcpStream::connect((p.host.as_str(), p.port))
        .await
        .map_err(|e| {
            emb(
                lang,
                format!("не удалось подключиться к {}:{} — {e}", p.host, p.port),
                format!("could not connect to {}:{} — {e}", p.host, p.port),
            )
        })?;

    let mut session = client::connect_stream(config, tcp, handler)
        .await
        .map_err(|e| {
            emb(
                lang,
                format!("сбой SSH-рукопожатия: {e}"),
                format!("SSH handshake failed: {e}"),
            )
        })?;

    authenticate(&mut session, p, password, lang).await?;

    let panel_port = p.panel_port;

    // Fast-fail parity with `ExitOnForwardFailure=yes`: probe that the server
    // can actually reach the panel on its loopback before we report success.
    let probe = session
        .channel_open_direct_tcpip("127.0.0.1", panel_port as u32, "127.0.0.1", 0)
        .await
        .map_err(|e| {
            emb(
                lang,
                format!("порт панели {panel_port} недоступен через туннель — {e}"),
                format!("panel port {panel_port} is not reachable through the tunnel — {e}"),
            )
        })?;
    drop(probe);

    // Bind the local end (the profile's stable port, so the panel origin and
    // its stored session survive reconnects) and start forwarding. The
    // listener is kept, so there is no pick-then-bind race here.
    let listener = crate::tunnel::bind_stable_listener(&p.id)
        .and_then(|l| {
            l.set_nonblocking(true)?;
            TcpListener::from_std(l)
        })
        .map_err(|e| {
            emb(
                lang,
                format!("не удалось занять локальный порт — {e}"),
                format!("could not bind a local port — {e}"),
            )
        })?;
    let local_port = listener
        .local_addr()
        .map_err(|e| {
            emb(
                lang,
                format!("не удалось определить локальный порт — {e}"),
                format!("could not read local port — {e}"),
            )
        })?
        .port();

    let accept_task = tokio::spawn(accept_loop(listener, session, panel_port));

    Ok(EmbeddedTunnel {
        local_port,
        accept_task,
    })
}

/// Authenticate the session per the profile's auth method.
async fn authenticate(
    session: &mut Handle<Handler>,
    p: &Profile,
    password: Option<&str>,
    lang: Lang,
) -> Result<(), TunnelError> {
    let ok = match p.auth {
        AuthMethod::Password => {
            let pass = password.ok_or(TunnelError::MissingPassword)?;
            session
                .authenticate_password(&p.user, pass)
                .await
                .map_err(|e| {
                    emb(
                        lang,
                        format!("ошибка входа по паролю — {e}"),
                        format!("password auth error — {e}"),
                    )
                })?
                .success()
        }
        AuthMethod::KeyFile => {
            let key_path = p.key_path.as_deref().ok_or_else(|| {
                emb(
                    lang,
                    "выбран вход по файлу ключа, но путь к ключу не задан".to_string(),
                    "key-file auth selected but no key path is set".to_string(),
                )
            })?;
            let key = load_secret_key(key_path, None).map_err(|e| {
                emb(
                    lang,
                    format!("не удалось загрузить приватный ключ {key_path} — {e}"),
                    format!("could not load private key {key_path} — {e}"),
                )
            })?;
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), Some(HashAlg::Sha256));
            session
                .authenticate_publickey(&p.user, key)
                .await
                .map_err(|e| {
                    emb(
                        lang,
                        format!("ошибка входа по ключу — {e}"),
                        format!("key auth error — {e}"),
                    )
                })?
                .success()
        }
        AuthMethod::Agent => unreachable!("agent auth is rejected before authenticate()"),
    };
    if ok {
        Ok(())
    } else {
        Err(emb(
            lang,
            "аутентификация не удалась — проверьте пользователя, пароль или ключ".to_string(),
            "authentication failed — check the user, password, or key".to_string(),
        ))
    }
}

/// Accept local connections and forward each through its own `direct-tcpip`
/// channel to the server's `127.0.0.1:<panel_port>`. Ends when the task is
/// aborted (tunnel closed) or the listener errors.
async fn accept_loop(listener: TcpListener, session: Handle<Handler>, panel_port: u16) {
    loop {
        let inbound = match listener.accept().await {
            Ok((sock, _peer)) => sock,
            Err(_) => break,
        };
        let channel = match session
            .channel_open_direct_tcpip("127.0.0.1", panel_port as u32, "127.0.0.1", 0)
            .await
        {
            Ok(c) => c,
            // A single failed channel drops just this connection; the tunnel
            // stays up for the next one.
            Err(_) => continue,
        };
        tokio::spawn(pump(inbound, channel));
    }
}

/// Copy bytes in both directions between the local socket and the SSH channel
/// until either side closes.
async fn pump(mut inbound: TcpStream, channel: russh::Channel<client::Msg>) {
    let mut stream = channel.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut stream).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnMode;

    fn profile(auth: AuthMethod) -> Profile {
        Profile {
            id: "id".into(),
            name: "name".into(),
            mode: ConnMode::Remote,
            host: "127.0.0.1".into(),
            user: "volt".into(),
            port: 2222,
            panel_port: 42617,
            auth,
            key_path: None,
            principal: None,
            paircode_command: None,
        }
    }

    /// Agent auth is out of scope for the built-in client and must be rejected
    /// before any network I/O, with a message that names the cause.
    #[tokio::test]
    async fn embedded_rejects_agent_auth() {
        let e = open_embedded_tunnel(&profile(AuthMethod::Agent), None, Lang::En)
            .await
            .unwrap_err();
        assert!(matches!(e, TunnelError::Embedded(_)));
        assert!(e.to_string().contains("agent"), "got: {e}");
    }

    /// Live end-to-end test against the docker sshd sidecar (see
    /// _local/deploy-f4-docker/compose.ssh-test.yaml). Ignored by default —
    /// run with the sidecar up:
    ///   cargo test --lib embedded_tunnel_live -- --ignored --nocapture
    /// Overridable via VOLT_TEST_SSH_{HOST,PORT,USER,PASSWORD,PANEL_PORT}.
    #[tokio::test]
    #[ignore = "requires a live SSH server; run explicitly with the sidecar up"]
    async fn embedded_tunnel_live() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        // VOLT_TEST_SSH_AUTH = "password" (default) or "key" (+ VOLT_TEST_SSH_KEY).
        let use_key = env("VOLT_TEST_SSH_AUTH", "password") == "key";
        let password = env("VOLT_TEST_SSH_PASSWORD", "voltpass123");
        let p = Profile {
            host: env("VOLT_TEST_SSH_HOST", "127.0.0.1"),
            user: env("VOLT_TEST_SSH_USER", "volt"),
            port: env("VOLT_TEST_SSH_PORT", "2222").parse().unwrap(),
            panel_port: env("VOLT_TEST_SSH_PANEL_PORT", "42617").parse().unwrap(),
            auth: if use_key {
                AuthMethod::KeyFile
            } else {
                AuthMethod::Password
            },
            key_path: use_key.then(|| env("VOLT_TEST_SSH_KEY", "")),
            ..profile(AuthMethod::Password)
        };

        let tunnel = open_embedded_tunnel(&p, (!use_key).then_some(password.as_str()), Lang::En)
            .await
            .expect("embedded tunnel should open");

        let mut sock = TcpStream::connect(("127.0.0.1", tunnel.local_port))
            .await
            .expect("connect to forwarded local port");
        sock.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        sock.read_to_end(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf);
        assert!(
            resp.contains("200"),
            "expected HTTP 200 through the tunnel, got: {}",
            &resp[..resp.len().min(120)]
        );
    }
}
