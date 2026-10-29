//! Opening windows, and keeping their labels apart.
//!
//! A window is no longer "the app" (see
//! `docs/superpowers/specs/2026-10-29-gateway-monitoring-design.md` §2): the
//! process owns the connections, and a window is one view on one of them.
//! Launching the app again does not start a second process — the
//! single-instance plugin hands the launch to this one, which opens another
//! window here.

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

/// Cap a window to the monitor's work area (the screen minus the taskbar),
/// re-centering it when it had to shrink. The configured size is picked so
/// the profile form fits without a scrollbar on a normal screen; this is the
/// small-screen fallback. Runs once per window, at creation — nothing
/// resizes a window while it is in use.
#[cfg(desktop)]
pub fn fit_to_screen<R: Runtime>(window: &WebviewWindow<R>) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let (Ok(inner), Ok(outer)) = (window.inner_size(), window.outer_size()) else {
        return;
    };
    // The decorations are part of the work area budget but not of `set_size`,
    // which takes the INNER size.
    let area = monitor.work_area().size;
    let max_w = area
        .width
        .saturating_sub(outer.width.saturating_sub(inner.width));
    let max_h = area
        .height
        .saturating_sub(outer.height.saturating_sub(inner.height));
    if inner.width <= max_w && inner.height <= max_h {
        return;
    }
    let _ = window.set_size(tauri::PhysicalSize::new(
        inner.width.min(max_w),
        inner.height.min(max_h),
    ));
    let _ = window.center();
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
