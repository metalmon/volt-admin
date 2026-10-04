//! Tauri commands backing the connection-profile store.
//!
//! Profiles are persisted via `tauri-plugin-store` in a single JSON file
//! (`profiles.json`) under one key (`profiles`), as a plain array. No
//! password is ever stored — see `crate::connection` for why.

use crate::connection::{ConnMode, Profile};
use crate::pair::{self, PairContext, PairState};
use crate::tunnel::{self, Lang, TunnelState};
use serde::Serialize;
use tauri::{command, AppHandle, Manager, Runtime, State};
use tauri_plugin_store::StoreExt;

const STORE_PATH: &str = "profiles.json";
const PROFILES_KEY: &str = "profiles";

/// The main window's app-entry URL (the React connect screen: the dev-server
/// origin or the bundled `tauri://localhost`/`http://tauri.localhost` entry
/// point, whichever `tauri.conf.json` resolves to on this platform), so
/// `disconnect` can navigate back to it without hardcoding a dev-vs-prod URL.
///
/// Captured LAZILY on the first `connect` — NOT at `setup`, where the webview
/// has not finished loading and `window.url()` still returns `about:blank`
/// (capturing that made disconnect navigate to a blank page). At connect time
/// the webview is showing the loaded Connect screen, so its URL is valid.
#[derive(Default)]
pub struct StartUrl(pub std::sync::Mutex<Option<tauri::Url>>);

impl StartUrl {
    /// Remember the app-entry URL the first time we see a real (non-blank)
    /// one; later calls are ignored so a mid-session capture can't overwrite
    /// it with the panel URL.
    pub fn capture(&self, url: tauri::Url) {
        let mut guard = self.0.lock().expect("StartUrl mutex poisoned");
        if guard.is_none() && url.as_str() != "about:blank" {
            *guard = Some(url);
        }
    }

    pub fn get(&self) -> Option<tauri::Url> {
        self.0.lock().expect("StartUrl mutex poisoned").clone()
    }
}

/// Navigate the main window's webview to `url`, top-level (not an iframe).
/// The daemon panel serves `Content-Security-Policy: frame-ancestors
/// 'none'`, which blocks framing but not top-level navigation — so this is
/// how the connected panel is shown instead of an `<iframe>`.
fn navigate_main_window<R: Runtime>(app: &AppHandle<R>, url: tauri::Url) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    window.navigate(url).map_err(|e| e.to_string())
}

fn load_profiles<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<Profile>, String> {
    let store = app.store(STORE_PATH).map_err(|e| e.to_string())?;
    let profiles = match store.get(PROFILES_KEY) {
        Some(value) => serde_json::from_value(value).map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    Ok(profiles)
}

fn persist_profiles<R: Runtime>(app: &AppHandle<R>, profiles: &[Profile]) -> Result<(), String> {
    let store = app.store(STORE_PATH).map_err(|e| e.to_string())?;
    let value = serde_json::to_value(profiles).map_err(|e| e.to_string())?;
    store.set(PROFILES_KEY, value);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// List all saved connection profiles.
#[command]
pub fn list_profiles(app: AppHandle) -> Result<Vec<Profile>, String> {
    load_profiles(&app)
}

/// Create or update a connection profile (matched by `id`).
#[command]
pub fn save_profile(app: AppHandle, profile: Profile) -> Result<(), String> {
    let mut profiles = load_profiles(&app)?;
    match profiles.iter_mut().find(|p| p.id == profile.id) {
        Some(existing) => *existing = profile,
        None => profiles.push(profile),
    }
    persist_profiles(&app, &profiles)
}

/// Delete a connection profile by `id`.
#[command]
pub fn delete_profile(app: AppHandle, id: String) -> Result<(), String> {
    let mut profiles = load_profiles(&app)?;
    profiles.retain(|p| p.id != id);
    persist_profiles(&app, &profiles)
}

/// What `connect` hands back to the Connect screen.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectResult {
    pub base_url: String,
}

/// Connect to a profile's `voltd` runtime, navigate the main window's
/// webview top-level to the panel (see `navigate_main_window`), and return
/// the panel's base URL. Pairing is not done here: the panel asks for a code
/// on load only if it has no session (see `crate::pair`).
///
/// - `Local` mode never opens a tunnel: it uses the direct panel URL.
/// - `Remote` mode opens an SSH `-L` tunnel (tearing down any previously
///   active tunnel first) and uses the tunnel's local URL.
///
/// `password` is only used for `AuthMethod::Password` profiles and is
/// never persisted — see `crate::tunnel`.
#[command]
pub async fn connect<R: Runtime>(
    app: AppHandle<R>,
    profile: Profile,
    password: Option<String>,
    lang: Option<String>,
    tunnel_state: State<'_, TunnelState>,
) -> Result<ConnectResult, String> {
    // UI language for any user-facing tunnel error (RU-first default).
    let lang = Lang::from_code(lang.as_deref());
    let mut guard = tunnel_state.0.lock().await;
    // A new connect attempt supersedes whatever was active before;
    // dropping the old value (if any) kills its ssh child.
    *guard = None;

    // `endpoint` (IP:port) feeds the OS window title so the copy shows which
    // instance it is connected to. Local = the direct 127.0.0.1:panel_port;
    // Remote = the actual remote host:panel_port (not the local tunnel port).
    let mut embedded = false;
    let (base_url, endpoint) = match profile.mode {
        ConnMode::Local => (
            format!("http://127.0.0.1:{}", profile.panel_port),
            format!("127.0.0.1:{}", profile.panel_port),
        ),
        ConnMode::Remote => {
            let tun = tunnel::open_tunnel(&profile, password.as_deref(), lang)
                .await
                .map_err(|e| e.localize(lang))?;
            let url = format!("http://127.0.0.1:{}", tun.local_port);
            embedded = tun.is_embedded();
            *guard = Some(tun);
            (url, format!("{}:{}", profile.host, profile.panel_port))
        }
    };
    drop(guard);

    // The gateway admin token for the panel's own "connect a new device"
    // button (see `crate::pair`). Failure just means no token; the panel then
    // shows its usual CLI hint. The token itself is never logged.
    let admin_token =
        match pair::fetch_admin_token(&profile, password.as_deref(), embedded, lang).await {
            Ok(token) => Some(token),
            Err(reason) => {
                eprintln!("[volt-admin] gateway admin token unavailable: {reason}");
                None
            }
        };

    // Remember what a pairing code for this connection would need, so the
    // page-load hook can mint one on demand (see `crate::pair::PairContext`
    // for the password's lifetime). Set before navigating: the first page
    // load must already see it.
    if let Some(origin) = pair::origin_of(&base_url) {
        app.state::<PairState>().set(PairContext {
            origin,
            profile: profile.clone(),
            password: password.clone(),
            embedded,
            lang,
            admin_token,
        });
    }

    // Capture the app-entry URL (the Connect screen the webview is currently
    // showing) before we navigate away, so `disconnect` knows where to go
    // back to. Lazy + once — see `StartUrl`.
    if let Some(window) = app.get_webview_window("main") {
        if let Ok(current) = window.url() {
            app.state::<StartUrl>().capture(current);
        }
    }

    // The panel entry point is the daemon's ROOT: its SPA fallback serves
    // index.html there, which itself pulls assets from `/_app/*`. `/_app/`
    // is only the static-asset prefix — requesting it bare returns 400 by
    // design (see the gateway's static_files handler), so navigate to `/`.
    let panel_url = format!("{}/", base_url.trim_end_matches('/'));
    let url = tauri::Url::parse(&panel_url).map_err(|e| e.to_string())?;
    navigate_main_window(&app, url)?;

    // Connection identity in the OS title bar (per copy/window).
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title(&format!("{} ({}) — Вольт Админ", profile.name, endpoint));
    }

    Ok(ConnectResult { base_url })
}

/// Tear down the active tunnel (if any) and navigate the main window's
/// webview back to the app's own entry point (the Connect screen). No-op on
/// the tunnel in `Local` mode or when nothing is connected — the navigate
/// back still happens unconditionally, since the window may be showing the
/// panel regardless of mode.
#[command]
pub async fn disconnect<R: Runtime>(
    app: AppHandle<R>,
    tunnel_state: State<'_, TunnelState>,
    start_url: State<'_, StartUrl>,
    pair_state: State<'_, PairState>,
) -> Result<(), String> {
    let mut guard = tunnel_state.0.lock().await;
    *guard = None; // Drop kills the ssh child.
    drop(guard);
    // Forget the connection's pairing context (and with it the password).
    pair_state.clear();

    // Back to the launcher identity in the title bar.
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title("Вольт Админ");
    }

    match start_url.get() {
        Some(url) => navigate_main_window(&app, url),
        // Never connected this session → nothing to navigate back to.
        None => Ok(()),
    }
}
