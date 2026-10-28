//! Connection profile model.
//!
//! A `Profile` describes how the Volt Admin UI reaches a `voltd` runtime:
//! either `Local` (attach to a daemon already running on this machine) or
//! `Remote` (SSH-tunnel to a server; the tunnel itself is built in a later
//! task).
//!
//! `Profile` deliberately has **no `password` field**. Passwords are
//! collected transiently at connect time (a later task) and are never
//! persisted to disk alongside the rest of the profile.

use serde::{Deserialize, Serialize};

/// How the UI reaches the target `voltd` runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnMode {
    /// Attach to a daemon already running on this machine.
    Local,
    /// Reach the daemon over an SSH tunnel to a remote host.
    Remote,
}

/// How a remote connection authenticates over SSH.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthMethod {
    /// Use the local SSH agent's loaded keys.
    Agent,
    /// Use a specific private key file (see `Profile::key_path`).
    KeyFile,
    /// Prompt for a password at connect time (never persisted).
    Password,
}

/// A saved connection profile.
///
/// NOTE: no `password` field, by design — see the module docs. Passwords are
/// collected transiently at connect time and never written to the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub mode: ConnMode,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub panel_port: u16,
    /// Local loopback port the SSH tunnel listens on when this profile is
    /// connected (`-L <local_port>:127.0.0.1:<panel_port>`). None = pick a
    /// stable per-profile port automatically (see
    /// `crate::tunnel::requested_local_listener`). Independent from the
    /// remote `panel_port`.
    #[serde(default)]
    pub local_port: Option<u16>,
    pub auth: AuthMethod,
    pub key_path: Option<String>,
    /// `[[authz.principals]]` id the auto-pairing code is bound to (an
    /// administrator profile). Empty = `admin`.
    #[serde(default)]
    pub principal: Option<String>,
    /// Command run on the machine with voltd that prints a fresh pairing
    /// code (`{panel_port}` / `{principal}` placeholders). Empty = the voltd
    /// CLI default, see `crate::pair::paircode_command`.
    #[serde(default)]
    pub paircode_command: Option<String>,
}

/// Is `word` safe to put in the `user@host` argument of an `ssh` command
/// line? Rejects a leading `-` (ssh would read the whole destination as an
/// option), whitespace, a `@`, and control characters.
fn is_safe_ssh_word(word: &str) -> bool {
    let word = word.trim();
    !word.starts_with('-')
        && !word
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '@')
}

/// Characters that let a string render differently from how it runs:
/// newlines and other control characters (a preview collapses them), the
/// bidi overrides behind "Trojan Source", and zero-width joiners/spaces.
/// None of them belong in a path or a one-line command.
fn has_hidden_characters(s: &str) -> bool {
    s.chars().any(|c| {
        c.is_control()
            || matches!(c,
                '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
    })
}

/// A UNC or device path (`\\host\share`, `//host/share`, `\\?\…`, `\\.\…`).
/// Handing one to `ssh -i` makes Windows authenticate to that host to read
/// the "key" — which leaks an NTLM hash to whoever sent the profile.
fn is_network_or_device_path(path: &str) -> bool {
    let p = path.trim_start();
    p.starts_with("\\\\") || p.starts_with("//")
}

impl Profile {
    /// Check the fields that must hold for this profile to be usable, in the
    /// order a reader of the form would meet them. `Err` carries the bare
    /// camelCase field name — the UI turns it into a localized message (see
    /// `commands::parse_profile_doc`), so nothing here knows about languages.
    ///
    /// Remote-only fields are not checked for a `Local` profile: that mode
    /// talks to `127.0.0.1:<panel_port>` directly and never reads them.
    /// `id` is not checked here either — an imported document legitimately
    /// arrives without one, and it is `save_profile` that must not store a
    /// profile it cannot address.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("name".into());
        }
        if self.panel_port == 0 {
            return Err("panelPort".into());
        }
        if self.local_port == Some(0) {
            return Err("localPort".into());
        }
        // The import preview is a consent dialog: what it shows must be what
        // runs. A command carrying a newline or a bidi override renders as
        // one harmless line and executes as something else.
        if let Some(command) = self.paircode_command.as_deref() {
            if has_hidden_characters(command) {
                return Err("paircodeCommand".into());
            }
        }
        if let Some(key) = self.key_path.as_deref() {
            if has_hidden_characters(key) || is_network_or_device_path(key) {
                return Err("keyPath".into());
            }
        }
        // `principal` is interpolated into the pairing-code command, which
        // runs through `sh -c` / `cmd /C` (see `crate::pair`), so it must not
        // be able to carry shell syntax. An id from `[[authz.principals]]`
        // never needs more than this alphabet.
        if let Some(principal) = self.principal.as_deref() {
            let principal = principal.trim();
            if !principal.is_empty()
                && (principal.len() > 64
                    || !principal
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
            {
                return Err("principal".into());
            }
        }
        if self.mode == ConnMode::Remote {
            if self.host.trim().is_empty() {
                return Err("host".into());
            }
            if self.user.trim().is_empty() {
                return Err("user".into());
            }
            // `user` and `host` are passed to ssh as one `user@host` argv
            // entry. A value starting with `-` would be read as an OPTION
            // instead — `-oProxyCommand=…` runs a command on THIS machine —
            // and whitespace or a second `@` splits the destination. Neither
            // is legal in a real username or hostname.
            if !is_safe_ssh_word(&self.host) {
                return Err("host".into());
            }
            if !is_safe_ssh_word(&self.user) {
                return Err("user".into());
            }
            if self.port == 0 {
                return Err("port".into());
            }
            if self.auth == AuthMethod::KeyFile
                && self.key_path.as_deref().unwrap_or("").trim().is_empty()
            {
                return Err("keyPath".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile that passes `validate`; each test breaks one thing.
    fn valid() -> Profile {
        Profile {
            id: "pilot-backend".into(),
            name: "VoltSSH".into(),
            mode: ConnMode::Remote,
            host: "backend.volt.oktaplus.ru".into(),
            user: "volt".into(),
            port: 2222,
            panel_port: 42617,
            local_port: None,
            auth: AuthMethod::Agent,
            key_path: None,
            principal: None,
            paircode_command: None,
        }
    }

    #[test]
    fn validate_accepts_a_complete_remote_profile() {
        assert_eq!(valid().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_a_blank_name() {
        let p = Profile {
            name: "  ".into(),
            ..valid()
        };
        assert_eq!(p.validate(), Err("name".into()));
    }

    #[test]
    fn validate_rejects_a_zero_panel_port() {
        let p = Profile {
            panel_port: 0,
            ..valid()
        };
        assert_eq!(p.validate(), Err("panelPort".into()));
    }

    #[test]
    fn validate_rejects_a_remote_without_host_or_user() {
        let p = Profile {
            host: "".into(),
            ..valid()
        };
        assert_eq!(p.validate(), Err("host".into()));
        let p = Profile {
            user: "".into(),
            ..valid()
        };
        assert_eq!(p.validate(), Err("user".into()));
    }

    #[test]
    fn validate_rejects_key_file_auth_without_a_path() {
        let p = Profile {
            auth: AuthMethod::KeyFile,
            key_path: None,
            ..valid()
        };
        assert_eq!(p.validate(), Err("keyPath".into()));
    }

    #[test]
    fn validate_rejects_a_zero_local_port() {
        let p = Profile {
            local_port: Some(0),
            ..valid()
        };
        assert_eq!(p.validate(), Err("localPort".into()));
    }

    #[test]
    fn validate_rejects_an_ssh_option_as_host_or_user() {
        // An imported profile is untrusted input: `ssh … -oProxyCommand=…`
        // would run a command on THIS machine.
        for host in ["-oProxyCommand=calc.exe", "a host", "h@h"] {
            let p = Profile {
                host: host.into(),
                ..valid()
            };
            assert_eq!(p.validate(), Err("host".into()), "host: {host:?}");
        }
        for user in ["-oProxyCommand=calc.exe", "ro ot"] {
            let p = Profile {
                user: user.into(),
                ..valid()
            };
            assert_eq!(p.validate(), Err("user".into()), "user: {user:?}");
        }
    }

    #[test]
    fn validate_rejects_shell_syntax_in_the_principal() {
        // It is interpolated into the pairing-code command, which is run by a
        // shell on the target machine.
        for principal in ["admin; rm -rf /", "$(id)", "a`b`", "admin\nops"] {
            let p = Profile {
                principal: Some(principal.into()),
                ..valid()
            };
            assert_eq!(
                p.validate(),
                Err("principal".into()),
                "principal: {principal:?}"
            );
        }
        let p = Profile {
            principal: Some("ops.admin_1-x".into()),
            ..valid()
        };
        assert_eq!(p.validate(), Ok(()), "a real principal id must pass");
    }

    #[test]
    fn validate_rejects_a_command_that_hides_what_it_runs() {
        // The import preview is what the user consents to, so a command must
        // not be able to render as less than it is.
        for command in [
            "echo ok\nrm -rf /",
            "echo ok\u{202e}rm -rf /",
            "echo\u{200b}ok; rm -rf /",
        ] {
            let p = Profile {
                paircode_command: Some(command.into()),
                ..valid()
            };
            assert_eq!(p.validate(), Err("paircodeCommand".into()), "{command:?}");
        }
    }

    #[test]
    fn validate_rejects_a_network_key_path() {
        // `ssh -i \\host\share\key` makes Windows authenticate to `host`,
        // handing an NTLM hash to whoever wrote the profile.
        for key in ["\\\\10.0.0.9\\share\\id_ed25519", "//10.0.0.9/share/id"] {
            let p = Profile {
                auth: AuthMethod::KeyFile,
                key_path: Some(key.into()),
                ..valid()
            };
            assert_eq!(p.validate(), Err("keyPath".into()), "{key:?}");
        }
        let p = Profile {
            auth: AuthMethod::KeyFile,
            key_path: Some("~/.ssh/id_ed25519".into()),
            ..valid()
        };
        assert_eq!(p.validate(), Ok(()), "a normal key path must pass");
    }

    #[test]
    fn validate_ignores_remote_fields_for_local() {
        // A local profile reaches the panel on loopback: host, user, SSH port
        // and the key are never read, so blanks there must not block it.
        let p = Profile {
            mode: ConnMode::Local,
            host: "".into(),
            user: "".into(),
            port: 0,
            auth: AuthMethod::KeyFile,
            key_path: None,
            ..valid()
        };
        assert_eq!(p.validate(), Ok(()));
    }

    #[test]
    fn profile_roundtrips_without_password_field() {
        let p = Profile {
            id: "a".into(),
            name: "srv".into(),
            mode: ConnMode::Remote,
            host: "h".into(),
            user: "u".into(),
            port: 22,
            panel_port: 42617,
            local_port: None,
            auth: AuthMethod::Agent,
            key_path: None,
            principal: None,
            paircode_command: None,
        };
        let j = serde_json::to_string(&p).unwrap();
        assert!(
            !j.contains("password"),
            "profile must never serialize a password"
        );
        let back: Profile = serde_json::from_str(&j).unwrap();
        assert_eq!(back.host, "h");
    }

    #[test]
    fn older_profiles_without_pairing_fields_still_load() {
        let j = r#"{"id":"a","name":"srv","mode":"local","host":"127.0.0.1","user":"","port":22,"panelPort":42617,"auth":"agent","keyPath":null}"#;
        let p: Profile = serde_json::from_str(j).unwrap();
        assert!(p.principal.is_none());
        assert!(p.paircode_command.is_none());
    }
}
