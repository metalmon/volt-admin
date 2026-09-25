//! Tauri commands backing the connection-profile store.
//!
//! Profiles are persisted via `tauri-plugin-store` in a single JSON file
//! (`profiles.json`) under one key (`profiles`), as a plain array. No
//! password is ever stored — see `crate::connection` for why.

use crate::connection::{ConnMode, Profile};
use crate::tunnel::{self, TunnelState};
use tauri::{command, AppHandle, Runtime, State};
use tauri_plugin_store::StoreExt;

const STORE_PATH: &str = "profiles.json";
const PROFILES_KEY: &str = "profiles";

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

/// Connect to a profile's `voltd` runtime and return the local URL to load
/// in the UI.
///
/// - `Local` mode never opens a tunnel: it returns the direct panel URL.
/// - `Remote` mode opens an SSH `-L` tunnel (tearing down any previously
///   active tunnel first) and returns the tunnel's local URL.
///
/// `password` is only used for `AuthMethod::Password` profiles and is
/// never persisted — see `crate::tunnel`.
#[command]
pub async fn connect(
    profile: Profile,
    password: Option<String>,
    tunnel_state: State<'_, TunnelState>,
) -> Result<String, String> {
    let mut guard = tunnel_state.0.lock().await;
    // A new connect attempt supersedes whatever was active before;
    // dropping the old value (if any) kills its ssh child.
    *guard = None;

    match profile.mode {
        ConnMode::Local => Ok(format!("http://127.0.0.1:{}", profile.panel_port)),
        ConnMode::Remote => {
            let tun = tunnel::open_tunnel(&profile, password.as_deref())
                .await
                .map_err(|e| e.to_string())?;
            let url = format!("http://127.0.0.1:{}", tun.local_port);
            *guard = Some(tun);
            Ok(url)
        }
    }
}

/// Tear down the active tunnel (if any). No-op in `Local` mode or when
/// nothing is connected.
#[command]
pub async fn disconnect(tunnel_state: State<'_, TunnelState>) -> Result<(), String> {
    let mut guard = tunnel_state.0.lock().await;
    *guard = None; // Drop kills the ssh child.
    Ok(())
}
