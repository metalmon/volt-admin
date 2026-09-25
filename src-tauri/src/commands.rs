//! Tauri commands backing the connection-profile store.
//!
//! Profiles are persisted via `tauri-plugin-store` in a single JSON file
//! (`profiles.json`) under one key (`profiles`), as a plain array. No
//! password is ever stored — see `crate::connection` for why.

use crate::connection::Profile;
use tauri::{command, AppHandle, Runtime};
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
