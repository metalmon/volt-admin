//! Tauri commands backing the connection-profile store.
//!
//! Profiles are persisted via `tauri-plugin-store` in a single JSON file
//! (`profiles.json`) under one key (`profiles`), as a plain array. No
//! password is ever stored — see `crate::connection` for why.

use crate::connection::{ConnMode, Profile};
use crate::logx;
use crate::pair::{self, PairContext, PairState};
use crate::registry::{Held, Holder, Registry};
use crate::tunnel::{self, Lang};
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

/// Read a profile document (a file the kit ships, or pasted text) into a
/// `Profile`, WITHOUT storing it: the UI shows the result and asks before
/// calling `save_profile`, because `paircode_command` is a command that will
/// later be run on the target machine (see `crate::profile_doc`).
///
/// `Err` is `"json"`, `"array"`, or `"field:<name>"` — the UI maps the field
/// name onto its existing `field.*` labels for the message.
#[command]
pub fn parse_profile_doc(text: String) -> Result<Profile, String> {
    crate::profile_doc::parse(&text).map_err(as_ui_error)
}

/// `"json"` / `"array"` travel as-is; anything else is a field name.
fn as_ui_error(reason: String) -> String {
    match reason.as_str() {
        "json" | "array" => reason,
        field => format!("field:{field}"),
    }
}

/// Write a saved profile to a JSON file in the user's download directory and
/// return the full path. No save dialog: that needs another plugin, and the
/// point here is to hand someone a file — the UI reveals it in the file
/// manager afterwards (the `opener` plugin already allows that).
#[command]
pub fn export_profile_file(app: AppHandle, id: String) -> Result<String, String> {
    let profiles = load_profiles(&app)?;
    let profile = profiles
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| "no such profile".to_string())?;
    let dir = app
        .path()
        .download_dir()
        .or_else(|_| app.path().home_dir())
        .map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(profile).map_err(|e| e.to_string())?;
    let path = free_path(&dir, &file_stem(&profile.name));
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// A profile name reduced to something every filesystem accepts. Keeps
/// letters (Cyrillic included), digits, `-` and `_`; everything else becomes
/// `-`. Empty or all-punctuation names fall back to `profile`.
fn file_stem(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let stem = stem.trim_matches('-').to_string();
    if stem.is_empty() {
        "profile".to_string()
    } else {
        stem.chars().take(64).collect()
    }
}

/// `<dir>/<stem>.json`, or `<stem> (2).json`, `(3)`… — exporting twice must
/// not overwrite the file the user has already sent to someone.
fn free_path(dir: &std::path::Path, stem: &str) -> std::path::PathBuf {
    let first = dir.join(format!("{stem}.json"));
    if !first.exists() {
        return first;
    }
    (2..1000)
        .map(|n| dir.join(format!("{stem} ({n}).json")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// What the "copy diagnostics" button hands to the clipboard, plus where the
/// log lives so the UI can reveal it in the file manager.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub text: String,
    pub log_path: String,
}

/// Everything a support conversation starts with, in one paste: app version,
/// OS, whether an `ssh` client was found, the shape of the saved profiles,
/// and the tail of the log.
///
/// Deliberately absent: passwords, the gateway admin token, pairing codes,
/// and the contents of any key file — only the key's PATH, which the user
/// typed themselves. See `docs/specs/diagnostics.md`.
#[command]
pub fn collect_diagnostics(app: AppHandle) -> Result<Diagnostics, String> {
    let profiles = load_profiles(&app)?;
    let mut text = String::new();
    text.push_str(&format!(
        "Volt Admin {} ({} {})\n",
        app.package_info().version,
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    text.push_str(&format!(
        "ssh: {}\n",
        ssh_binary().unwrap_or_else(|| "NOT FOUND on PATH".into())
    ));
    text.push_str(&format!("profiles: {}\n", profiles.len()));
    for p in &profiles {
        text.push_str(&format!(
            "  - {} [{:?}] {} panel_port={} auth={:?} local_port={:?} principal={:?}\n",
            p.name,
            p.mode,
            match p.mode {
                ConnMode::Local => "127.0.0.1".to_string(),
                ConnMode::Remote => format!("{}@{}:{}", p.user, p.host, p.port),
            },
            p.panel_port,
            p.auth,
            p.local_port,
            p.principal
        ));
    }
    let path = logx::path();
    text.push_str(&format!("log: {}\n\n", path.display()));
    text.push_str(&logx::tail(50));
    Ok(Diagnostics {
        text,
        log_path: path.to_string_lossy().into_owned(),
    })
}

/// The `ssh` the app would spawn for a `Remote` profile, found the way the
/// OS would: the first match on `PATH`. `None` means `Remote` mode cannot
/// work on this machine, which is the single most common support answer.
fn ssh_binary() -> Option<String> {
    let exe = if cfg!(windows) { "ssh.exe" } else { "ssh" };
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(exe))
            .find(|candidate| candidate.is_file())
            .map(|found| found.to_string_lossy().into_owned())
    })
}

/// Create or update a connection profile (matched by `id`).
#[command]
pub fn save_profile(app: AppHandle, profile: Profile) -> Result<(), String> {
    // Guard the store itself, not just the importer: the form, an imported
    // document and any future caller all land here.
    if profile.id.trim().is_empty() {
        return Err("field:id".into());
    }
    profile.validate().map_err(as_ui_error)?;
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
/// - `Remote` mode opens an SSH `-L` tunnel and uses its local URL. The
///   previous connection is released only AFTER the new one is published,
///   so a failed attempt leaves the working one alone (`crate::registry`).
///
/// `password` is only used for `AuthMethod::Password` profiles and is
/// never persisted — see `crate::tunnel`.
#[command]
pub async fn connect<R: Runtime>(
    app: AppHandle<R>,
    profile: Profile,
    password: Option<String>,
    lang: Option<String>,
    registry: State<'_, Registry>,
) -> Result<ConnectResult, String> {
    // UI language for any user-facing tunnel error (RU-first default).
    let lang = Lang::from_code(lang.as_deref());
    logx::log(
        "connect",
        &format!(
            "start mode={:?} host={} user={} port={} panel_port={} auth={:?} principal={:?} paircodeCommand={:?} id={}",
            profile.mode,
            profile.host,
            profile.user,
            profile.port,
            profile.panel_port,
            profile.auth,
            profile.principal,
            profile.paircode_command,
            profile.id
        ),
    );

    // `endpoint` (IP:port) feeds the OS window title so the copy shows which
    // instance it is connected to. Local = the direct 127.0.0.1:panel_port;
    // Remote = the actual remote host:panel_port (not the local tunnel port).
    // One connect at a time: publishing a connection and releasing the
    // previous one is one step, and two overlapping connects would otherwise
    // release what the other just handed to the webview.
    let _connecting = registry.connect_lock().await;

    // Already connected to this profile (another holder, or the window
    // reconnecting): reuse it rather than opening a second tunnel.
    let mut tunnel_opt: Option<tunnel::Tunnel> = None;
    let held = registry.hold(&profile.id, Holder::Window).await;
    // Whether the connection is served by the built-in ssh client decides
    // how pairing reaches the far side — a reused one knows its own answer.
    let mut embedded = held.as_ref().is_some_and(|h: &Held| h.embedded);
    if let Some(h) = &held {
        logx::log(
            "connect",
            &format!(
                "reusing connection on port {} embedded={}",
                h.local_port, h.embedded
            ),
        );
    }
    let (base_url, endpoint) = match (&held, profile.mode) {
        (Some(h), ConnMode::Local) => (
            format!("http://127.0.0.1:{}", h.local_port),
            format!("127.0.0.1:{}", profile.panel_port),
        ),
        (Some(h), ConnMode::Remote) => (
            format!("http://127.0.0.1:{}", h.local_port),
            format!("{}:{}", profile.host, profile.panel_port),
        ),
        (None, ConnMode::Local) => (
            format!("http://127.0.0.1:{}", profile.panel_port),
            format!("127.0.0.1:{}", profile.panel_port),
        ),
        (None, ConnMode::Remote) => {
            let started = std::time::Instant::now();
            let fut = tunnel::open_tunnel(&profile, password.as_deref(), lang);
            // Hard cap: a wedged ssh child must never hold this connect for
            // ever (the frontend already guards at 90 s, this keeps the Rust
            // side clean and releases the process structure either way).
            let tun = match tokio::time::timeout(std::time::Duration::from_secs(30), fut).await {
                Ok(Ok(t)) => t,
                Ok(Err(e)) => {
                    logx::log(
                        "connect",
                        &format!(
                            "open_tunnel FAILED after {} ms: {}",
                            started.elapsed().as_millis(),
                            e
                        ),
                    );
                    return Err(e.localize(lang));
                }
                Err(_) => {
                    logx::log("connect", "open_tunnel HARD TIMEOUT (30 s hard cap hit)");
                    return Err(match lang {
                        Lang::Ru => {
                            "открытие SSH-туннеля зависло (жёсткий таймаут 30 с)".to_string()
                        }
                        Lang::En => "SSH tunnel setup hung (hard 30 s timeout)".to_string(),
                    });
                }
            };
            logx::log(
                "connect",
                &format!(
                    "open_tunnel ok local_port={} embedded={} elapsed_ms={}",
                    tun.local_port,
                    tun.is_embedded(),
                    started.elapsed().as_millis()
                ),
            );
            let url = format!("http://127.0.0.1:{}", tun.local_port);
            embedded = tun.is_embedded();
            tunnel_opt = Some(tun);
            (url, format!("{}:{}", profile.host, profile.panel_port))
        }
    };
    // Publish only once the connection is fully usable, and release the
    // previous one only after that: every `return Err` above happens while
    // the connection the user already had is still up, so a failed attempt
    // no longer costs them the one that worked.
    let reused = held.is_some();
    if !reused {
        let local_port = match &tunnel_opt {
            Some(tun) => tun.local_port,
            None => profile.panel_port,
        };
        registry
            .publish(
                &profile.id,
                tunnel_opt,
                local_port,
                embedded,
                Holder::Window,
            )
            .await;
    }
    let dropped = registry
        .release_others(Holder::Window, &profile.id)
        .await
        .len();
    logx::log(
        "connect",
        &format!(
            "connection {}, {dropped} previous released",
            if reused { "reused" } else { "published" }
        ),
    );

    // The gateway admin token for the panel's own "connect a new device"
    // button (see `crate::pair`). Failure just means no token; the panel then
    // shows its usual CLI hint. The token itself is never logged.
    let admin_token =
        match pair::fetch_admin_token(&profile, password.as_deref(), embedded, lang).await {
            Ok(token) => Some(token),
            Err(reason) => {
                eprintln!("[volt-admin] gateway admin token unavailable: {reason}");
                logx::log("connect", &format!("admin_token none: {reason}"));
                None
            }
        };
    logx::log(
        "connect",
        &format!("admin_token is_some={}", admin_token.is_some()),
    );

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
    logx::log("connect", &format!("navigate to {panel_url}"));
    let url = tauri::Url::parse(&panel_url).map_err(|e| e.to_string())?;
    navigate_main_window(&app, url)?;

    // Connection identity in the OS title bar (per copy/window).
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title(&format!("{} ({}) — Вольт Админ", profile.name, endpoint));
    }

    logx::log("connect", "done");
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
    registry: State<'_, Registry>,
    start_url: State<'_, StartUrl>,
    pair_state: State<'_, PairState>,
) -> Result<(), String> {
    // The window lets go of whatever it was showing; a connection nobody
    // else holds goes down with its ssh child (see `crate::registry`).
    let dropped = registry.release_all(Holder::Window).await;
    logx::log(
        "disconnect",
        &format!("released {} connection(s)", dropped.len()),
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_stem_keeps_letters_and_replaces_the_rest() {
        assert_eq!(file_stem("VoltSSH"), "VoltSSH");
        assert_eq!(file_stem("Вольт — бэкенд"), "Вольт---бэкенд");
        assert_eq!(file_stem(" my/server:2222 "), "my-server-2222");
    }

    #[test]
    fn file_stem_never_yields_an_empty_name() {
        assert_eq!(file_stem("   "), "profile");
        assert_eq!(file_stem("///"), "profile");
    }

    #[test]
    fn free_path_does_not_overwrite_an_existing_export() {
        let dir = std::env::temp_dir().join(format!("volt-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = free_path(&dir, "srv");
        assert_eq!(first.file_name().unwrap(), "srv.json");
        std::fs::write(&first, "{}").unwrap();
        assert_eq!(free_path(&dir, "srv").file_name().unwrap(), "srv (2).json");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
