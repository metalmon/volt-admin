//! Reading a connection profile that came from outside the app — a file the
//! kit ships, or text pasted from the clipboard.
//!
//! The document is just the camelCase JSON of a [`Profile`] (see
//! `docs/specs/profile-import-export.md`); there is no envelope and no
//! versioning. Everything the importer accepts goes through `parse`, and
//! `parse` is the only place that turns untrusted text into a `Profile`.
//!
//! An imported profile is imported *code*: `paircode_command` is run on the
//! target machine by `crate::pair`. Parsing therefore never touches the
//! store — the caller shows the user what was parsed and asks first.

use serde::Deserialize;

use crate::connection::{AuthMethod, ConnMode, Profile};

/// A profile as it arrives from outside: `id` may be missing (the UI mints
/// one), and the fields with a `Profile`-side default may be absent too.
/// Unknown fields are ignored, so a document from a newer build still loads.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Doc {
    #[serde(default)]
    id: String,
    name: String,
    mode: ConnMode,
    #[serde(default)]
    host: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    port: u16,
    panel_port: u16,
    #[serde(default)]
    local_port: Option<u16>,
    #[serde(default = "default_auth")]
    auth: AuthMethod,
    #[serde(default)]
    key_path: Option<String>,
    #[serde(default)]
    principal: Option<String>,
    #[serde(default)]
    paircode_command: Option<String>,
}

fn default_auth() -> AuthMethod {
    AuthMethod::Agent
}

/// Parse a profile document. `Err` is `"json"` (not valid JSON, or missing a
/// required field), `"array"` (a list of profiles — not supported), or the
/// bare name of the field that failed [`Profile::validate`].
///
/// `id` is allowed to be empty here: the UI fills it with a fresh UUID when
/// the document carries none. `save_profile` refuses an empty one.
pub fn parse(text: &str) -> Result<Profile, String> {
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.starts_with('[') {
        return Err("array".into());
    }
    let doc: Doc = serde_json::from_str(trimmed).map_err(|_| "json".to_string())?;
    let local = doc.mode == ConnMode::Local;
    let profile = Profile {
        id: doc.id.trim().to_string(),
        name: doc.name,
        mode: doc.mode,
        // Local always means this machine's loopback, and it opens no tunnel,
        // so a host or a forwarded port in the document is meaningless there.
        host: if local {
            "127.0.0.1".into()
        } else {
            doc.host
        },
        user: doc.user,
        port: doc.port,
        panel_port: doc.panel_port,
        local_port: if local { None } else { doc.local_port },
        auth: doc.auth,
        key_path: doc.key_path,
        principal: doc.principal,
        paircode_command: doc.paircode_command,
    };
    profile.validate()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
        "id": "pilot-backend",
        "name": "VoltSSH",
        "mode": "remote",
        "host": "backend.volt.oktaplus.ru",
        "user": "volt",
        "port": 2222,
        "panelPort": 42617,
        "localPort": 8081,
        "auth": "keyFile",
        "keyPath": "~/.ssh/volt_pilot",
        "principal": "admin",
        "paircodeCommand": "voltd gateway get-paircode --new"
    }"#;

    #[test]
    fn parse_reads_a_full_document() {
        let p = parse(FULL).unwrap();
        assert_eq!(p.id, "pilot-backend");
        assert_eq!(p.name, "VoltSSH");
        assert_eq!(p.mode, ConnMode::Remote);
        assert_eq!(p.host, "backend.volt.oktaplus.ru");
        assert_eq!(p.user, "volt");
        assert_eq!(p.port, 2222);
        assert_eq!(p.panel_port, 42617);
        assert_eq!(p.local_port, Some(8081));
        assert_eq!(p.auth, AuthMethod::KeyFile);
        assert_eq!(p.key_path.as_deref(), Some("~/.ssh/volt_pilot"));
        assert_eq!(p.principal.as_deref(), Some("admin"));
        assert_eq!(
            p.paircode_command.as_deref(),
            Some("voltd gateway get-paircode --new")
        );
    }

    #[test]
    fn parse_defaults_the_optional_fields() {
        let p = parse(r#"{"name":"L","mode":"local","panelPort":42617}"#).unwrap();
        assert_eq!(p.auth, AuthMethod::Agent);
        assert_eq!(p.local_port, None);
        assert!(p.principal.is_none());
        assert!(p.paircode_command.is_none());
    }

    #[test]
    fn parse_leaves_a_missing_id_empty() {
        let p = parse(r#"{"name":"L","mode":"local","panelPort":42617}"#).unwrap();
        assert_eq!(p.id, "", "the UI mints the id, not the parser");
    }

    #[test]
    fn parse_ignores_unknown_fields() {
        let p = parse(
            r#"{"name":"L","mode":"local","panelPort":42617,
                 "password":"hunter2","somethingNewer":1}"#,
        )
        .unwrap();
        assert_eq!(p.name, "L");
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("hunter2"), "an unknown field must not stick");
    }

    #[test]
    fn parse_normalizes_a_local_profile() {
        let p = parse(
            r#"{"name":"L","mode":"local","host":"10.0.0.5","localPort":9000,"panelPort":42617}"#,
        )
        .unwrap();
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.local_port, None, "local opens no tunnel");
    }

    #[test]
    fn parse_rejects_an_array() {
        let err = parse(r#"[{"name":"L","mode":"local","panelPort":42617}]"#).unwrap_err();
        assert_eq!(err, "array");
    }

    #[test]
    fn parse_rejects_malformed_json() {
        assert_eq!(parse("not json at all").unwrap_err(), "json");
        assert_eq!(parse(r#"{"mode":"local"}"#).unwrap_err(), "json");
    }

    #[test]
    fn parse_reports_the_offending_field() {
        let doc = r#"{"name":"L","mode":"local","panelPort":0}"#;
        assert_eq!(parse(doc).unwrap_err(), "panelPort");
        let doc = r#"{"name":"R","mode":"remote","user":"u","port":22,"panelPort":42617}"#;
        assert_eq!(parse(doc).unwrap_err(), "host");
    }
}
