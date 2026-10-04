mod commands;
mod connection;
mod pair;
mod tunnel;
mod tunnel_embedded;

// NOTE (was M1 TODO): the panel is no longer loaded in an `<iframe>` — the
// daemon serves `Content-Security-Policy: frame-ancestors 'none'`
// (anti-clickjacking), which blocks framing outright. The panel is now
// loaded as the main window's TOP-LEVEL content instead (`commands::connect`
// navigates the webview there directly; `frame-ancestors` does not apply to
// top-level navigation). `app.security.csp` stays `null`: Tauri's CSP
// applies to the app's OWN bundled pages, not to a page the webview has
// navigated to externally, so it has no bearing on the panel's CSP either
// way.

#[cfg(desktop)]
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Multi-copy model: no single-instance lock — launching the app again
    // opens another independent copy (another connection). See
    // _local/plan-volt-admin-multiwindow.md.
    let builder = tauri::Builder::default();

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(tunnel::TunnelState::default())
        .manage(pair::PendingPair::default())
        // Once the panel origin a `connect` navigated to has finished
        // loading, run the queued pairing (or warning) script in it.
        .on_page_load(|webview, payload| {
            if payload.event() != tauri::webview::PageLoadEvent::Finished {
                return;
            }
            if let Some(script) = webview.state::<pair::PendingPair>().take_for(payload.url()) {
                let _ = webview.eval(&script);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::save_profile,
            commands::delete_profile,
            commands::connect,
            commands::disconnect
        ])
        .setup(|app| {
            #[cfg(desktop)]
            {
                // Multi-copy model: no tray. Closing the window quits this
                // copy (its TunnelState Drop kills the ssh child). The only
                // state we still register is the StartUrl holder so the
                // `disconnect` command can navigate back to the launcher
                // (captured lazily on the first connect — see commands::StartUrl).
                app.manage(commands::StartUrl::default());

                // Launcher identity in the title bar (RU brand «Вольт Админ»),
                // overriding the conf default so it matches the connected title.
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_title("Вольт Админ");
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
