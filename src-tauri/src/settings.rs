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

/// The stored language, or the app's RU-first default when nothing is
/// stored yet or the store cannot be opened.
pub fn language<R: Runtime>(app: &AppHandle<R>) -> Lang {
    let Ok(store) = app.store(STORE_PATH) else {
        return Lang::Ru;
    };
    let code = store.get(LANG_KEY);
    Lang::from_code(code.as_ref().and_then(|v| v.as_str()))
}

/// Remember the language the UI is now showing.
pub fn set_language<R: Runtime>(app: &AppHandle<R>, lang: Lang) -> Result<(), String> {
    let store = app.store(STORE_PATH).map_err(|e| e.to_string())?;
    store.set(LANG_KEY, serde_json::Value::from(lang.code()));
    store.save().map_err(|e| e.to_string())
}
