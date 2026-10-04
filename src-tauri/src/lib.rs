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
        .manage(pair::PairState::default())
        // Lazy pairing on the connected panel origin (see `crate::pair`): every
        // load first gets the gateway admin token (when held) for the panel's
        // own pair-code button; then a normal load gets the probe (asks for a
        // code only without a session); a `/?volt_pair=1` load gets a freshly
        // minted code — or a banner saying why there is none.
        .on_page_load(|webview, payload| {
            if payload.event() != tauri::webview::PageLoadEvent::Finished {
                return;
            }
            let Some(ctx) = webview.state::<pair::PairState>().for_url(payload.url()) else {
                return;
            };
            if let Some(token) = &ctx.admin_token {
                let _ = webview.eval(&pair::token_script(token));
            }
            if !pair::is_pair_request(payload.url()) {
                let _ = webview.eval(pair::PROBE_SCRIPT);
                return;
            }
            let webview = webview.clone();
            tauri::async_runtime::spawn(async move {
                let script = match pair::mint_code(
                    &ctx.profile,
                    ctx.password.as_deref(),
                    ctx.embedded,
                    ctx.lang,
                )
                .await
                {
                    Ok(code) => pair::pair_script(&code),
                    Err(reason) => {
                        eprintln!("[volt-admin] auto-pair unavailable: {reason}");
                        pair::warning_script(&match ctx.lang {
                            tunnel::Lang::Ru => format!("Автосопряжение не выполнено: {reason}"),
                            tunnel::Lang::En => format!("Auto-pairing skipped: {reason}"),
                        })
                    }
                };
                let _ = webview.eval(&script);
            });
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
