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

#[cfg(test)]
mod tests {
    use super::*;

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
