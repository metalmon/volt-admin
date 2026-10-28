mod commands;
mod connection;
mod logx;
mod pair;
mod profile_doc;
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
        // Remember the window's size and position across runs, so a user who
        // resized it keeps that size. `fit_to_screen` below still caps the
        // restored size to the current screen (the saved one may come from a
        // bigger monitor). Nothing else is restored (no maximized state).
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::SIZE,
                )
                .build(),
        )
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
            logx::log("page_load", &format!("finished url={}", payload.url()));
            let Some(ctx) = webview.state::<pair::PairState>().for_url(payload.url()) else {
                logx::log("page_load", "not panel origin, ignored");
                return;
            };
            logx::log("page_load", "panel origin matched");
            if let Some(token) = &ctx.admin_token {
                let _ = webview.eval(&pair::token_script(token));
            }
            if !pair::is_pair_request(payload.url()) {
                let _ = webview.eval(pair::PROBE_SCRIPT);
                logx::log("page_load", "probe eval'd");
                return;
            }
            let webview = webview.clone();
            tauri::async_runtime::spawn(async move {
                let minted = pair::mint_code(
                    &ctx.profile,
                    ctx.password.as_deref(),
                    ctx.embedded,
                    ctx.lang,
                )
                .await;
                let script = match &minted {
                    Ok(code) => pair::pair_script(code),
                    Err(reason) => {
                        eprintln!("[volt-admin] auto-pair unavailable: {reason}");
                        logx::log("page_load", &format!("mint FAILED: {reason}"));
                        pair::warning_script(&match ctx.lang {
                            tunnel::Lang::Ru => format!("Автосопряжение не выполнено: {reason}"),
                            tunnel::Lang::En => format!("Auto-pairing skipped: {reason}"),
                        })
                    }
                };
                logx::log("page_load", &format!("pair/mint script eval"));
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
                    fit_to_screen(&window);
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Cap the startup window to the monitor's work area (the screen minus the
/// taskbar), re-centering it when it had to shrink. The conf size is picked
/// so the profile form fits without a scrollbar on a normal screen; this is
/// the small-screen fallback. Runs once at startup only — nothing resizes the
/// window while the app is in use.
#[cfg(desktop)]
fn fit_to_screen<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let (Ok(inner), Ok(outer)) = (window.inner_size(), window.outer_size()) else {
        return;
    };
    // The decorations are part of the work area budget but not of `set_size`,
    // which takes the INNER size.
    let area = monitor.work_area().size;
    let max_w = area.width.saturating_sub(outer.width.saturating_sub(inner.width));
    let max_h = area.height.saturating_sub(outer.height.saturating_sub(inner.height));
    if inner.width <= max_w && inner.height <= max_h {
        return;
    }
    let _ = window.set_size(tauri::PhysicalSize::new(
        inner.width.min(max_w),
        inner.height.min(max_h),
    ));
    let _ = window.center();
}
