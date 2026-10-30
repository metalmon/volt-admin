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

/// Russian counts in three forms, and a tray that gets it wrong is the
/// first thing a Russian speaker notices. `forms` is (1, 2, 5).
fn plural_ru(n: usize, forms: (&'static str, &'static str, &'static str)) -> &'static str {
    let (ones, tens) = (n % 10, n % 100);
    if ones == 1 && tens != 11 {
        forms.0
    } else if (2..=4).contains(&ones) && !(12..=14).contains(&tens) {
        forms.1
    } else {
        forms.2
    }
}

/// "3 ошибки" / "3 errors" — shared with the watcher's notification so the
/// two never count differently.
pub fn errors_phrase(lang: Lang, errors: usize) -> String {
    match lang {
        Lang::Ru => format!(
            "{errors} {}",
            plural_ru(errors, ("ошибка", "ошибки", "ошибок"))
        ),
        Lang::En => format!("{errors} error{}", if errors == 1 { "" } else { "s" }),
    }
}

/// What the tray says it has seen. The launcher's own name when there is
/// nothing to report — quiet is the normal state, and the tray must not
/// look alarmed in it.
pub fn watch_tooltip(lang: Lang, errors: usize, warnings: usize) -> String {
    if errors == 0 && warnings == 0 {
        return windows::launcher_title(lang).to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if errors > 0 {
        parts.push(errors_phrase(lang, errors));
    }
    if warnings > 0 {
        parts.push(match lang {
            Lang::Ru => format!(
                "{warnings} {}",
                plural_ru(
                    warnings,
                    ("предупреждение", "предупреждения", "предупреждений")
                )
            ),
            Lang::En => format!("{warnings} warning{}", if warnings == 1 { "" } else { "s" }),
        });
    }
    format!("{} — {}", windows::launcher_title(lang), parts.join(", "))
}

/// Put that on the tray icon.
pub fn set_watch_tooltip<R: Runtime>(
    app: &AppHandle<R>,
    lang: Lang,
    errors: usize,
    warnings: usize,
) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(watch_tooltip(lang, errors, warnings)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_have_their_own_labels() {
        assert_eq!(labels(Lang::Ru), ("Открыть", "Новое окно", "Выйти"));
        assert_eq!(labels(Lang::En), ("Open", "New window", "Quit"));
    }

    #[test]
    fn the_tooltip_says_nothing_when_there_is_nothing_to_say() {
        // Quiet is the normal state; the tray must not look alarmed in it.
        assert_eq!(
            watch_tooltip(Lang::Ru, 0, 0),
            windows::launcher_title(Lang::Ru)
        );
        assert_eq!(
            watch_tooltip(Lang::En, 0, 0),
            windows::launcher_title(Lang::En)
        );
    }

    #[test]
    fn the_tooltip_counts_in_the_language_of_the_app() {
        let ru = watch_tooltip(Lang::Ru, 2, 5);
        assert!(ru.contains("2 ошибки"), "{ru}");
        assert!(ru.contains("5 предупреждений"), "{ru}");
        // Russian counts in three forms, and getting it wrong is the first
        // thing a Russian speaker sees.
        assert!(watch_tooltip(Lang::Ru, 1, 0).contains("1 ошибка"));
        assert!(watch_tooltip(Lang::Ru, 5, 0).contains("5 ошибок"));
        assert!(watch_tooltip(Lang::Ru, 21, 0).contains("21 ошибка"));
        let en = watch_tooltip(Lang::En, 2, 5);
        assert!(en.contains("2 errors") && en.contains("5 warnings"), "{en}");
        assert!(
            watch_tooltip(Lang::En, 1, 1).contains("1 error,"),
            "singular"
        );
    }

    #[test]
    fn warnings_alone_are_still_worth_saying() {
        assert!(watch_tooltip(Lang::Ru, 0, 3).contains("3 предупреждения"));
        assert!(
            !watch_tooltip(Lang::Ru, 0, 3).contains("ошиб"),
            "no zero errors"
        );
    }
}
