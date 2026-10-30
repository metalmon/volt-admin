//! The handful of preferences Rust has to know about on its own.
//!
//! Only one so far: the interface language. The webview keeps its own copy
//! in `localStorage` (see `src/lib/i18n.tsx`) because that is what it can
//! read with no round trip and no flash of the wrong language — but the
//! tray is built before any window exists, so Rust needs its own answer.
//! The UI mirrors every change here through `commands::set_language`.

use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

use crate::tunnel::Lang;

const STORE_PATH: &str = "settings.json";
/// The watcher's session tokens, profile id → token. A FILE OF ITS OWN, not
/// a key in the settings: it is the only secret this app keeps on disk, and
/// keeping it apart is what makes "never printed, never exported, deleted
/// with the profile" something a reader can check rather than trust
/// (`docs/specs/gateway-monitoring.md`).
const WATCH_STORE: &str = "watch.json";
const LANG_KEY: &str = "lang";
/// Profile id → the `connection_fingerprint` of the gateway this app has
/// already paired with. NOT a secret: it says that a pairing happened, not
/// what it produced.
const PAIRED_KEY: &str = "paired";

/// The stored language, or the app's RU-first default when nothing is
/// stored yet or the store cannot be opened.
pub fn language<R: Runtime>(app: &AppHandle<R>) -> Lang {
    let Ok(store) = app.store(STORE_PATH) else {
        return Lang::Ru;
    };
    let code = store.get(LANG_KEY);
    Lang::from_code(code.as_ref().and_then(|v| v.as_str()))
}

/// Does this connection still need a pairing of its own?
///
/// `true` when nothing is remembered for the profile, or when what is
/// remembered belongs to a different destination — a profile edited to
/// point at another gateway has never paired with THAT one.
pub fn needs_pairing(remembered: Option<&str>, fingerprint: &str) -> bool {
    remembered != Some(fingerprint)
}

/// The gateway fingerprint this app last paired with for `profile_id`.
pub fn paired_fingerprint<R: Runtime>(app: &AppHandle<R>, profile_id: &str) -> Option<String> {
    let store = app.store(STORE_PATH).ok()?;
    let paired = store.get(PAIRED_KEY)?;
    paired
        .get(profile_id)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Remember that this app paired with the gateway `fingerprint` names.
///
/// Why remember at all: pairing again on every connect adds another hash to
/// the gateway's `[gateway] paired_tokens` and another row to its device
/// list, so a profile used daily grows both without bound. The panel keeps
/// its own session in the webview's storage, so a second pairing buys
/// nothing — and when the panel HAS lost its session, the page-driven
/// fallback still mints one on demand.
pub fn remember_paired<R: Runtime>(
    app: &AppHandle<R>,
    profile_id: &str,
    fingerprint: &str,
) -> Result<(), String> {
    let store = app.store(STORE_PATH).map_err(|e| e.to_string())?;
    let mut paired = store
        .get(PAIRED_KEY)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    paired.insert(profile_id.to_string(), serde_json::Value::from(fingerprint));
    store.set(PAIRED_KEY, serde_json::Value::Object(paired));
    store.save().map_err(|e| e.to_string())
}

/// The profiles remembered as paired with the gateway `fingerprint` names.
/// Pure, so the rule is testable without a store.
pub fn profiles_with_fingerprint(
    paired: &serde_json::Map<String, serde_json::Value>,
    fingerprint: &str,
) -> Vec<String> {
    paired
        .iter()
        .filter(|(_, v)| v.as_str() == Some(fingerprint))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Forget every profile paired with this gateway, returning their ids.
///
/// A rotate revokes every token the gateway had issued, so every marker
/// pointing at it is now a lie — and a lying marker sends the next connect
/// past pairing and onto a pairing form with nothing to fill it.
pub fn forget_paired_fingerprint<R: Runtime>(app: &AppHandle<R>, fingerprint: &str) -> Vec<String> {
    let Ok(store) = app.store(STORE_PATH) else {
        return Vec::new();
    };
    let Some(mut paired) = store.get(PAIRED_KEY).and_then(|v| v.as_object().cloned()) else {
        return Vec::new();
    };
    let forgotten = profiles_with_fingerprint(&paired, fingerprint);
    for id in &forgotten {
        paired.remove(id);
    }
    if !forgotten.is_empty() {
        store.set(PAIRED_KEY, serde_json::Value::Object(paired));
        let _ = store.save();
    }
    forgotten
}

/// Forget a profile's pairing — it is deleted, or it now points elsewhere.
pub fn forget_paired<R: Runtime>(app: &AppHandle<R>, profile_id: &str) {
    let Ok(store) = app.store(STORE_PATH) else {
        return;
    };
    let Some(mut paired) = store.get(PAIRED_KEY).and_then(|v| v.as_object().cloned()) else {
        return;
    };
    if paired.remove(profile_id).is_some() {
        store.set(PAIRED_KEY, serde_json::Value::Object(paired));
        let _ = store.save();
    }
}

/// Remember the language the UI is now showing.
pub fn set_language<R: Runtime>(app: &AppHandle<R>, lang: Lang) -> Result<(), String> {
    let store = app.store(STORE_PATH).map_err(|e| e.to_string())?;
    store.set(LANG_KEY, serde_json::Value::from(lang.code()));
    store.save().map_err(|e| e.to_string())
}

/// The token kept for this profile, but ONLY if it was minted for the
/// gateway the profile names today.
///
/// A profile can be edited while it is watched — a different `panel_port`
/// is a different gateway on this machine — and a token minted for the old
/// one must not be sent to the new one. The fingerprint is the same one
/// `crate::registry` refuses stale connections by.
pub fn watch_token<R: Runtime>(
    app: &AppHandle<R>,
    profile_id: &str,
    fingerprint: &str,
) -> Option<String> {
    let store = app.store(WATCH_STORE).ok()?;
    token_for(&store.get(profile_id)?, fingerprint)
}

/// The pure half: a kept entry is `{"token": …, "fingerprint": …}`, and it
/// answers only for its own gateway.
fn token_for(entry: &serde_json::Value, fingerprint: &str) -> Option<String> {
    if entry.get("fingerprint")?.as_str()? != fingerprint {
        return None;
    }
    entry
        .get("token")?
        .as_str()
        .map(str::to_string)
        .filter(|t| !t.is_empty())
}

/// Make the token file readable by its owner alone.
///
/// `tauri-plugin-store` writes with `fs::write`, which on Linux and macOS
/// means 0644 under the user's data directory — world-readable on a shared
/// machine. On Windows the per-user profile's ACL already restricts it, so
/// there is nothing to tighten there.
fn own_it_alone(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = path.parent() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Where the plugin puts a store file (`BaseDirectory::AppData`).
fn store_path<R: Runtime>(app: &AppHandle<R>, name: &str) -> Option<std::path::PathBuf> {
    use tauri::Manager;
    app.path().app_data_dir().ok().map(|dir| dir.join(name))
}

/// Keep a token for a watched profile, with the gateway it belongs to.
/// Nothing else writes this file.
pub fn remember_watch_token<R: Runtime>(
    app: &AppHandle<R>,
    profile_id: &str,
    token: &str,
    fingerprint: &str,
) -> Result<(), String> {
    let store = app.store(WATCH_STORE).map_err(|e| e.to_string())?;
    store.set(
        profile_id,
        serde_json::json!({ "token": token, "fingerprint": fingerprint }),
    );
    store.save().map_err(|e| e.to_string())?;
    if let Some(path) = store_path(app, WATCH_STORE) {
        own_it_alone(&path);
    }
    Ok(())
}

/// Forget it — the profile stopped being watched, or stopped existing.
pub fn forget_watch_token<R: Runtime>(app: &AppHandle<R>, profile_id: &str) {
    let Ok(store) = app.store(WATCH_STORE) else {
        return;
    };
    if store.delete(profile_id) {
        let _ = store.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_token_answers_only_for_its_own_gateway() {
        // A watched profile can be edited — a different panel port is a
        // different gateway on this machine — and the token minted for the
        // old one must not be handed to the new one.
        let entry = serde_json::json!({"token": "t", "fingerprint": "fp1"});
        assert_eq!(token_for(&entry, "fp1").as_deref(), Some("t"));
        assert_eq!(token_for(&entry, "fp2"), None);
    }

    #[test]
    fn an_entry_without_a_gateway_is_not_trusted() {
        // What an older version wrote (a bare string), and anything else
        // that does not say which gateway it is for: pair again.
        assert_eq!(token_for(&serde_json::json!("t"), "fp1"), None);
        assert_eq!(token_for(&serde_json::json!({"token": "t"}), "fp1"), None);
        assert_eq!(
            token_for(
                &serde_json::json!({"token": "", "fingerprint": "fp1"}),
                "fp1"
            ),
            None
        );
    }

    #[test]
    fn a_rotate_forgets_every_profile_pointing_at_that_gateway() {
        let paired: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({"a": "fp1", "b": "fp1", "c": "fp2"}))
                .unwrap();
        let mut forgotten = profiles_with_fingerprint(&paired, "fp1");
        forgotten.sort();
        assert_eq!(forgotten, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            profiles_with_fingerprint(&paired, "fp3"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_profile_that_never_paired_needs_to() {
        assert!(needs_pairing(None, "fp"));
    }

    #[test]
    fn a_profile_paired_with_this_very_gateway_does_not() {
        assert!(!needs_pairing(Some("fp"), "fp"));
    }

    #[test]
    fn a_profile_pointed_at_another_gateway_pairs_again() {
        // Same profile, edited to another host: that gateway has never seen
        // this app, and reusing the old marker would leave the panel at its
        // pairing form with nobody minting a code.
        assert!(needs_pairing(Some("old-host"), "new-host"));
    }
}
