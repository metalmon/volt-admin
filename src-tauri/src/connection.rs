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
    pub auth: AuthMethod,
    pub key_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_roundtrips_without_password_field() {
        let p = Profile {
            id: "a".into(),
            name: "srv".into(),
            mode: ConnMode::Remote,
            host: "h".into(),
            user: "u".into(),
            port: 22,
            panel_port: 42627,
            auth: AuthMethod::Agent,
            key_path: None,
        };
        let j = serde_json::to_string(&p).unwrap();
        assert!(!j.contains("password"), "profile must never serialize a password");
        let back: Profile = serde_json::from_str(&j).unwrap();
        assert_eq!(back.host, "h");
    }
}
