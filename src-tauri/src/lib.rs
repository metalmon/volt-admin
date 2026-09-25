mod commands;
mod connection;
mod tunnel;

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
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

/// Bring the main window to the foreground: unminimize, show, focus.
///
/// This is the fix for the upstream bug where a tray left-click did nothing
/// (or only toggled visibility) when the window was minimized rather than
/// hidden. All three calls are needed: `show` alone does not un-minimize on
/// Windows, and neither raises focus on its own.
#[cfg(desktop)]
fn restore_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // Single-instance must be registered before other plugins (see plugin
    // docs). A second launch focuses the existing window instead of opening
    // a new process.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            restore_main_window(app);
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(tunnel::TunnelState::default())
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
                let show_i = MenuItem::with_id(app, "show", "Показать", true, None::<&str>)?;
                let disconnect_i =
                    MenuItem::with_id(app, "disconnect", "Отключиться", true, None::<&str>)?;
                let quit_i = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_i, &disconnect_i, &quit_i])?;

                // Capture the main window's entry URL (the React Connect
                // screen: dev-server origin or the bundled
                // tauri://localhost / http://tauri.localhost entry point,
                // whichever this platform resolves to) *before* anything
                // navigates it away, so `commands::disconnect` and the tray
                // "Отключиться" handler below can navigate back to it
                // without hardcoding a dev-vs-prod URL.
                // StartUrl is captured LAZILY on the first `connect` (the
                // webview's URL here at setup is still `about:blank`), so just
                // register the empty holder now.
                app.manage(commands::StartUrl::default());

                let icon = app
                    .default_window_icon()
                    .cloned()
                    .expect("bundle configures a default window icon");

                TrayIconBuilder::new()
                    .icon(icon)
                    .menu(&menu)
                    // Left-click restores the window (see below); the menu
                    // itself only opens on right-click.
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        "show" => restore_main_window(app),
                        "disconnect" => {
                            // Tray is Rust-side and independent of webview
                            // content, so this works the same whether the
                            // window is currently showing the app shell or
                            // the remote panel.
                            let app_handle = app.clone();
                            tauri::async_runtime::spawn(async move {
                                {
                                    let state = app_handle.state::<tunnel::TunnelState>();
                                    let mut guard = state.0.lock().await;
                                    *guard = None; // Drop kills the ssh child, if any.
                                }
                                // Navigate back to the captured app entry on
                                // the UI thread (WebView2 requires navigation
                                // to run there, and this body is a spawned
                                // task). No-op if nothing was ever connected.
                                let nav = app_handle.clone();
                                let _ = app_handle.run_on_main_thread(move || {
                                    if let (Some(window), Some(url)) = (
                                        nav.get_webview_window("main"),
                                        nav.state::<commands::StartUrl>().get(),
                                    ) {
                                        let _ = window.navigate(url);
                                    }
                                });
                                let _ = app_handle.emit("voltadmin-disconnected", ());
                            });
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    // THE bug fix: upstream only handled the menu, or toggled
                    // show/hide, so a minimized window never came back on
                    // left-click. Restore explicitly on tray left-click release.
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            restore_main_window(tray.app_handle());
                        }
                    })
                    .build(app)?;

                // Close-to-tray: hide instead of quitting so the tray icon
                // (and any active tunnel) stays alive; "Выход" is the real
                // exit path.
                if let Some(window) = app.get_webview_window("main") {
                    let window_handle = window.clone();
                    window.on_window_event(move |event| {
                        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                            api.prevent_close();
                            let _ = window_handle.hide();
                        }
                    });
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
