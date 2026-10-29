//! The tray icon: what keeps the process alive once the last window is
//! closed, and the only way to quit.
//!
//! Its labels are the one piece of user-facing text Rust has to produce
//! before any window exists, so the chosen language cannot come from the
//! webview the way `connect`'s does. It comes from the store instead (see
//! `crate::settings`), and switching the language in the UI rebuilds the
//! menu through [`relabel`].

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Runtime};

use crate::tunnel::Lang;
use crate::windows;

/// Id the tray is registered under, so the language switch can find it.
pub const TRAY_ID: &str = "main";

const OPEN_ID: &str = "open";
const NEW_ID: &str = "new";
const QUIT_ID: &str = "quit";

/// The menu's labels, in `lang`: show a window, open another one, quit.
fn labels(lang: Lang) -> (&'static str, &'static str, &'static str) {
    match lang {
        Lang::Ru => ("Открыть", "Новое окно", "Выйти"),
        Lang::En => ("Open", "New window", "Quit"),
    }
}

fn build_menu<R: Runtime>(app: &AppHandle<R>, lang: Lang) -> tauri::Result<Menu<R>> {
    let (open, new, quit) = labels(lang);
    let open = MenuItem::with_id(app, OPEN_ID, open, true, None::<&str>)?;
    let new = MenuItem::with_id(app, NEW_ID, new, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT_ID, quit, true, None::<&str>)?;
    Menu::with_items(app, &[&open, &new, &quit])
}

/// Put the tray in place. "Quit" must be there, or an app that outlives its
/// windows cannot be closed from the UI at all.
pub fn install<R: Runtime>(app: &AppHandle<R>, lang: Lang) -> tauri::Result<()> {
    let icon = app
        .default_window_icon()
        .cloned()
        .expect("the app always ships an icon");
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(windows::launcher_title(lang))
        .menu(&build_menu(app, lang)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN_ID => windows::show_a_window(app),
            NEW_ID => windows::request_new_window(app),
            QUIT_ID => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Linux emits no click events (Tauri's own note); the menu is
            // the way in there.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                windows::show_a_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Rebuild the menu in `lang` — the UI's language switch reaches the tray
/// through here, so the two never disagree.
pub fn relabel<R: Runtime>(app: &AppHandle<R>, lang: Lang) -> tauri::Result<()> {
    let menu = build_menu(app, lang)?;
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(menu))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_have_their_own_labels() {
        assert_eq!(labels(Lang::Ru), ("Открыть", "Новое окно", "Выйти"));
        assert_eq!(labels(Lang::En), ("Open", "New window", "Quit"));
    }
}
