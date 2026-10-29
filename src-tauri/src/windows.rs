//! Opening windows, and keeping their labels apart.
//!
//! A window is no longer "the app" (see
//! `docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md` §2): the
//! process owns the connections, and a window is one view on one of them.
//! Launching the app again does not start a second process — the
//! single-instance plugin hands the launch to this one, which opens another
//! window here.
//!
//! Per-platform caveats of the tray that holds the process:
//!
//! * **Linux** — Tauri emits no tray CLICK events there, so the icon is
//!   reached through its menu only. On a desktop with no StatusNotifier
//!   host the icon does not appear at all: the process then has no UI, and
//!   the way back is to launch the app again — the single-instance plugin
//!   hands that launch here and a window opens.
//! * **macOS** — closing the last window leaves the app in the Dock;
//!   `RunEvent::Reopen` brings a window back when its icon is clicked.
//!   Cmd+Q goes through the standard menu and ends the process without a
//!   `RunEvent`, so the ssh children are reaped by their own guard rather
//!   than by anything here (`crate::tunnel::child_guard`).

use std::sync::atomic::{AtomicUsize, Ordering};

use tauri::{AppHandle, Manager, Runtime, WebviewWindow, WebviewWindowBuilder};

/// Window title while nothing is connected; `commands::connect` replaces it
/// with the connection's identity.
pub const LAUNCHER_TITLE: &str = "Вольт Админ";

static NEXT: AtomicUsize = AtomicUsize::new(1);

/// A label no other window has. Tauri keys everything on these, and the
/// registry names a connection's holder by them.
fn next_label() -> String {
    format!("win-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Open a window on the app's own entry point — the Connect screen.
///
/// EVERY window comes from here, the first one included: `tauri.conf.json`
/// says `"create": false` so the config window is not built behind our
/// back, which kept the two kinds of window subtly different — the config
/// one never got a webview background, and flashed white on navigation.
///
/// Built from the SAME config as the first window, with only the label
/// changed: hand-rolling the builder meant hand-rolling the URL too, and a
/// window whose webview never loaded anything is just a white rectangle.
/// It also keeps the size in one place — `tauri.conf.json`.
pub fn open_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    let mut config = app
        .config()
        .app
        .windows
        .first()
        .cloned()
        .unwrap_or_default();
    config.label = next_label();
    config.title = LAUNCHER_TITLE.to_string();
    let window = WebviewWindowBuilder::from_config(app, &config)?
        // The config's `backgroundColor` reaches the WINDOW layer only —
        // `WebviewAttributes::from(&WindowConfig)` does not carry it — and
        // the white flash when a page is replaced comes from the WEBVIEW
        // layer. This sets both, at creation, which is the only time the
        // webview takes it for the gap between documents.
        .background_color(BACKGROUND_DARK)
        .build()?;
    // Created dark because the app and the panel are dark by default; a
    // light system is corrected straight away, while the window is still
    // empty.
    paint_for_theme(&window);
    fit_to_screen(&window);
    Ok(window)
}

/// Ask for another window. THE only way to get one — the tray and the UI
/// button both come through here, so there is one behaviour to reason about
/// and one place to debug.
///
/// The work is handed to the thread that runs the event loop. Building a
/// WebView2 anywhere else, or inline inside the handling of the IPC message
/// that asked for it, produced a blank window and then a wedged app;
/// `run_on_main_thread` runs it immediately when the caller is already the
/// event loop (the tray's menu handler) and posts it otherwise (a command).
pub fn request_new_window<R: Runtime>(app: &AppHandle<R>) {
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        if let Err(e) = open_window(&handle) {
            crate::logx::log("window", &format!("open_window failed: {e}"));
        }
    });
    if let Err(e) = queued {
        crate::logx::log("window", &format!("could not reach the event loop: {e}"));
    }
}

/// Bring a window to the user: the first one there, or a new one.
pub fn show_a_window<R: Runtime>(app: &AppHandle<R>) {
    match app.webview_windows().values().next() {
        Some(window) => {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
        None => {
            let _ = open_window(app);
        }
    }
}

/// Let go of whatever this window was showing: its connection (unless
/// another window or a watcher still holds it) and that panel's pairing
/// context, which carries an SSH password and the gateway admin token.
///
/// Called when a window is destroyed AND when the last one is hidden
/// instead — closing means the same thing either way.
pub async fn release_window<R: Runtime>(app: &AppHandle<R>, label: &str) {
    let holder = crate::registry::Holder::Window(label.to_string());
    let dropped = app
        .state::<crate::registry::Registry>()
        .release_all_for(&holder)
        .await;
    let pair_state = app.state::<crate::pair::PairState>();
    for d in &dropped {
        pair_state.clear_origin(&d.origin);
    }
    crate::logx::log(
        "window",
        &format!("{label} closed, released {} connection(s)", dropped.len()),
    );
}

/// Hide the last window rather than destroying it.
///
/// Destroying it meant the tray had to build a new one — a fresh webview
/// booting the whole frontend, which is both slow and somewhere else on
/// screen. Hidden, it comes back instantly and in place.
///
/// It still COUNTS as closed: the connection is released and the webview
/// goes back to the Connect screen, or showing it again would present a
/// panel whose port is long gone.
pub fn hide_as_closed<R: Runtime>(window: &WebviewWindow<R>) {
    let app = window.app_handle().clone();
    let label = window.label().to_string();
    let _ = window.set_title(LAUNCHER_TITLE);
    if let Some(url) = app.state::<crate::commands::StartUrl>().get() {
        let _ = window.navigate(url);
    }
    let _ = window.hide();
    tauri::async_runtime::spawn(async move {
        release_window(&app, &label).await;
    });
}

/// The page's own background, mirroring `--color-background` in
/// `src/index.css` (light and `.dark`).
///
/// The colour every window is CREATED with lives in `tauri.conf.json`
/// (`backgroundColor`), because setting it afterwards leaves the webview
/// layer white between documents — which is what pressing «Подключить»
/// looked like: the launcher page left and the panel arrived through a
/// white flash. These two are for correcting a light system afterwards.
const BACKGROUND_LIGHT: tauri::window::Color = tauri::window::Color(0xf5, 0xf2, 0xee, 0xff);
const BACKGROUND_DARK: tauri::window::Color = tauri::window::Color(0x1b, 0x1e, 0x1f, 0xff);

/// Paint the window in the theme the OS is using, before anything loads in
/// it. The frontend follows `prefers-color-scheme` (see `src/main.tsx`), so
/// this has to follow the same answer.
pub fn paint_for_theme<R: Runtime>(window: &WebviewWindow<R>) {
    let color = match window.theme() {
        Ok(tauri::Theme::Dark) => BACKGROUND_DARK,
        _ => BACKGROUND_LIGHT,
    };
    let _ = window.set_background_color(Some(color));
}

/// Cap a window to the monitor's work area (the screen minus the taskbar).
/// The configured size is picked so the profile form fits without a
/// scrollbar on a normal screen; this is the small-screen fallback. Runs
/// once per window, at creation — nothing resizes a window while it is in
/// use.
///
/// It does NOT re-centre. Centring threw away the position the window-state
/// plugin had just restored, every time the window was a hair taller than
/// the work area — which on a 1080p screen is most of the time. A window
/// that ends up hanging off the edge is nudged back in, and one that fits
/// is left exactly where the user put it.
pub fn fit_to_screen<R: Runtime>(window: &WebviewWindow<R>) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let (Ok(inner), Ok(outer), Ok(position)) = (
        window.inner_size(),
        window.outer_size(),
        window.outer_position(),
    ) else {
        return;
    };
    // The decorations are part of the work area budget but not of
    // `set_size`, which takes the INNER size.
    let area = monitor.work_area();
    let chrome_w = outer.width.saturating_sub(inner.width);
    let chrome_h = outer.height.saturating_sub(inner.height);
    let max_w = area.size.width.saturating_sub(chrome_w);
    let max_h = area.size.height.saturating_sub(chrome_h);
    if inner.width > max_w || inner.height > max_h {
        let _ = window.set_size(tauri::PhysicalSize::new(
            inner.width.min(max_w),
            inner.height.min(max_h),
        ));
    }
    // Nudge back inside, keeping as much of the user's position as fits.
    let outer_w = inner.width.min(max_w) + chrome_w;
    let outer_h = inner.height.min(max_h) + chrome_h;
    let right = area.position.x + area.size.width as i32;
    let bottom = area.position.y + area.size.height as i32;
    let x = position.x.min(right - outer_w as i32).max(area.position.x);
    let y = position.y.min(bottom - outer_h as i32).max(area.position.y);
    if (x, y) != (position.x, position.y) {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_never_repeat() {
        let a = next_label();
        let b = next_label();
        let c = next_label();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert!(a.starts_with("win-"), "{a}");
    }
}
