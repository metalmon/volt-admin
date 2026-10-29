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

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

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
pub fn open_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    let window = WebviewWindowBuilder::new(app, next_label(), WebviewUrl::App("index.html".into()))
        .title(LAUNCHER_TITLE)
        .inner_size(1180.0, 1000.0)
        .min_inner_size(900.0, 640.0)
        .build()?;
    fit_to_screen(&window);
    Ok(window)
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
