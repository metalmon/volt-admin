mod actions;
mod commands;
mod connection;
mod logx;
mod pair;
mod profile_doc;
mod registry;
mod settings;
mod tray;
mod tunnel;
mod tunnel_embedded;
mod windows;

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
    // One process, several windows: a second launch is handed to this
    // instance by the single-instance plugin below, and a window is one view
    // on one connection rather than the app itself. See
    // `docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md` §2.
    let builder = tauri::Builder::default();

    builder
        // FIRST, as the plugin requires: a second launch is handed to this
        // process, which opens another window instead of starting a second
        // copy of the app.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            logx::log("app", "second launch — opening a window");
            let _ = windows::open_window(app);
        }))
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
                // Geometry is remembered per LABEL, and labels are handed out
                // per run (`main`, `win-1`, …) — so a window opened from the
                // tray would be "a different window" and land at the default
                // position. Every window shares one remembered geometry
                // instead: what the user wants is "it opens where I left it",
                // not a different memory per slot.
                .map_label(|_| "window")
                .build(),
        )
        .manage(registry::Registry::default())
        .manage(pair::PairState::default())
        // Lazy pairing on the connected panel origin (see `crate::pair`): every
        // load first gets the gateway admin token (when held) for the panel's
        // own pair-code button; then a normal load gets the probe (asks for a
        // code only without a session); a `/?volt_pair=1` load gets a freshly
        // minted code — or a banner saying why there is none.
        .on_page_load(|webview, payload| {
            // STARTED is WebView2's `ContentLoading`: early enough to hand
            // the panel its session before its own scripts look for one.
            if payload.event() == tauri::webview::PageLoadEvent::Started {
                if let Some(ctx) = webview.state::<pair::PairState>().for_url(payload.url()) {
                    if let Some(token) = &ctx.session_token {
                        let _ = webview.eval(pair::session_script(&ctx.origin, token));
                        logx::log("page_load", "session handed over before scripts");
                    }
                }
                return;
            }
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
                let _ = webview.eval(pair::token_script(token));
            }
            if ctx.session_token.is_some() {
                // Already signed in by `connect`; the probe exists only for
                // the fallback path and would cost a navigation.
                return;
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
                logx::log("page_load", "pair/mint script eval");
                let _ = webview.eval(&script);
            });
        })
        // A window closed with the X button never calls `disconnect`, so its
        // hold would keep an ssh child alive until the app quits. Releasing
        // here is what makes a connection outlive only the windows that
        // still want it.
        .on_window_event(|window, event| match event {
            // The LAST window hides instead of closing: rebuilding it from
            // the tray means booting a whole webview, slowly and somewhere
            // else on screen. It still counts as closed — see
            // `windows::hide_as_closed`.
            tauri::WindowEvent::CloseRequested { api, .. }
                if window.app_handle().webview_windows().len() == 1 =>
            {
                api.prevent_close();
                if let Some(w) = window.app_handle().get_webview_window(window.label()) {
                    windows::hide_as_closed(&w);
                }
            }
            // Any other window really closes, and never calls `disconnect`
            // on its way out — so its hold is released here.
            tauri::WindowEvent::Destroyed => {
                let app = window.app_handle().clone();
                let label = window.label().to_string();
                tauri::async_runtime::spawn(async move {
                    windows::release_window(&app, &label).await;
                });
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::save_profile,
            commands::parse_profile_doc,
            commands::export_profile_file,
            commands::collect_diagnostics,
            commands::new_window,
            commands::set_language,
            commands::run_gateway_action,
            commands::delete_profile,
            commands::connect,
            commands::disconnect
        ])
        .setup(|app| {
            #[cfg(desktop)]
            {
                // Closing a window no longer quits the app. Its ssh children die with it, but NOT through Rust's
                // Drop — `Builder::run` ends the process with
                // `std::process::exit`, which runs no destructor. What reaps
                // them is the OS-level guard each child is given at spawn: a
                // Job Object on Windows, `PR_SET_PDEATHSIG` on Linux, a
                // watchdog process on macOS (`crate::tunnel::child_guard`).
                // The `Registry`'s own Drop matters while the app RUNS —
                // releasing a connection is what tears its tunnel down. The
                // only
                // state we still register is the StartUrl holder so the
                // `disconnect` command can navigate back to the launcher
                // (captured lazily on the first connect — see commands::StartUrl).
                app.manage(commands::StartUrl::default());

                // Point the logger at the platform's log directory before
                // anything can log (it resolves nothing on its own — see
                // `crate::logx`).
                if let Ok(dir) = app.path().app_log_dir() {
                    logx::init(dir.join("debug.log"));
                }

                // Launcher identity in the title bar (RU brand «Вольт Админ»),
                // overriding the conf default so it matches the connected title.
                // The first window, by the same path as every other one
                // (`"create": false` in the config, see `crate::windows`).
                windows::request_new_window(app.handle());

                // The tray is what keeps the process alive once the last
                // window is closed — which is the point: a connection, and
                // later a watcher, outlives the window that opened it. Its
                // language comes from the store, because no window exists
                // yet to ask (see `crate::settings`).
                tray::install(app.handle(), settings::language(app.handle()))?;
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Closing the last window must NOT quit: the tray holds the
            // process, and a connection — later a watcher — outlives the
            // window that opened it. An explicit exit (the tray's «Выйти»,
            // which calls `exit(0)`) carries a code and is let through.
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                api.prevent_exit();
            }
        });
}
