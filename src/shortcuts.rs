//! The keyboard shortcut table, shown in the Options panel and as hints in
//! menus. Key handling itself lives in `app.rs`; keep the two in sync.

use crate::chrome::Action;

/// Modifier prefix for application shortcuts.
pub fn primary() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd+" } else { "Ctrl+Shift+" }
}

/// Modifier prefix for font zoom and tab switching.
fn ctrl() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd+" } else { "Ctrl+" }
}

/// Shortcut shown next to a menu item, if it has one.
pub fn hint(action: Action) -> String {
    let p = primary();
    match action {
        Action::NewTab => format!("{p}T"),
        Action::CloseTab => format!("{p}W"),
        Action::Find => format!("{p}F"),
        Action::Copy => format!("{p}C"),
        Action::Paste => format!("{p}V"),
        Action::Options => format!("{p},"),
        Action::FontBigger => format!("{}=", ctrl()),
        Action::FontSmaller => format!("{}-", ctrl()),
        _ => String::new(),
    }
}

/// (keys, what they do) for the Options panel.
pub fn all() -> Vec<(String, &'static str)> {
    let p = primary();
    let c = ctrl();
    let mut v = vec![
        (format!("{p}T"), "New tab"),
        (format!("{p}W"), "Close tab"),
        (format!("{c}Tab / {c}Shift+Tab"), "Next / previous tab"),
        (format!("{c}PgDn / {c}PgUp"), "Next / previous tab"),
        (format!("{p}F"), "Find"),
        ("Enter / Shift+Enter".to_owned(), "Previous / next match"),
        ("F3 / Shift+F3".to_owned(), "Previous / next match"),
        ("Ctrl+R (in search)".to_owned(), "Toggle regex search"),
        ("Esc".to_owned(), "Close search, menu or panel"),
        (format!("{p}C / {p}V"), "Copy / paste"),
        (format!("{c}= / {c}- / {c}0"), "Font larger / smaller / reset"),
        (format!("{p}L"), "Cycle theme: system, dark, light"),
        (format!("{p},"), "Options"),
        ("Shift+PgUp / PgDn".to_owned(), "Scroll a page"),
        ("Shift+Home / End".to_owned(), "Scroll to top / bottom"),
        ("Right click".to_owned(), "Copy / paste menu"),
        ("Middle click".to_owned(), "Paste; close tab on tab bar"),
        ("Shift+drag".to_owned(), "Select inside mouse apps (vim, htop)"),
    ];
    if cfg!(target_os = "macos") {
        v[2] = ("Cmd+Shift+] / Cmd+Shift+[".to_owned(), "Next / previous tab");
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeMode;

    #[test]
    fn menu_hints_use_the_platform_modifier() {
        for a in [Action::NewTab, Action::CloseTab, Action::Find, Action::Copy, Action::Paste, Action::Options] {
            let h = hint(a);
            assert!(h.starts_with(primary()), "{a:?}: {h}");
        }
        assert!(hint(Action::About).is_empty());
        assert!(hint(Action::Theme(ThemeMode::Dark)).is_empty());
    }

    #[test]
    fn every_menu_hint_is_listed_in_options() {
        let keys: Vec<String> = all().into_iter().map(|(k, _)| k).collect();
        for a in [Action::NewTab, Action::CloseTab, Action::Find, Action::Options] {
            let h = hint(a);
            assert!(keys.iter().any(|k| k.contains(&h)), "{h} missing from the Options list");
        }
        assert!(all().iter().all(|(k, what)| !k.is_empty() && !what.is_empty()));
    }
}
