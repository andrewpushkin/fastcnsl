//! Keyboard shortcuts: the commands that can be bound to keys, their
//! default keys, the user's changes from the `[keys]` table of the
//! settings file, and matching key presses against them. The Options panel
//! lists and edits them; menus show them as hints.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::chrome::Action;

/// Something a key can be bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Command {
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    Find,
    Copy,
    Paste,
    Options,
    CycleTheme,
    FontBigger,
    FontSmaller,
    FontReset,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
}

impl Command {
    /// Every command, in the order the Options panel lists them.
    pub const ALL: [Command; 16] = [
        Command::NewTab,
        Command::CloseTab,
        Command::NextTab,
        Command::PreviousTab,
        Command::Find,
        Command::Copy,
        Command::Paste,
        Command::Options,
        Command::CycleTheme,
        Command::FontBigger,
        Command::FontSmaller,
        Command::FontReset,
        Command::ScrollPageUp,
        Command::ScrollPageDown,
        Command::ScrollTop,
        Command::ScrollBottom,
    ];

    /// The name in the settings file's `[keys]` table.
    pub fn name(self) -> &'static str {
        match self {
            Command::NewTab => "new_tab",
            Command::CloseTab => "close_tab",
            Command::NextTab => "next_tab",
            Command::PreviousTab => "previous_tab",
            Command::Find => "find",
            Command::Copy => "copy",
            Command::Paste => "paste",
            Command::Options => "options",
            Command::CycleTheme => "cycle_theme",
            Command::FontBigger => "font_bigger",
            Command::FontSmaller => "font_smaller",
            Command::FontReset => "font_reset",
            Command::ScrollPageUp => "scroll_page_up",
            Command::ScrollPageDown => "scroll_page_down",
            Command::ScrollTop => "scroll_top",
            Command::ScrollBottom => "scroll_bottom",
        }
    }

    /// What it does, for the Options panel.
    pub fn label(self) -> &'static str {
        match self {
            Command::NewTab => "New tab",
            Command::CloseTab => "Close tab",
            Command::NextTab => "Next tab",
            Command::PreviousTab => "Previous tab",
            Command::Find => "Find",
            Command::Copy => "Copy",
            Command::Paste => "Paste",
            Command::Options => "Options",
            Command::CycleTheme => "Cycle theme",
            Command::FontBigger => "Font larger",
            Command::FontSmaller => "Font smaller",
            Command::FontReset => "Font size reset",
            Command::ScrollPageUp => "Scroll a page up",
            Command::ScrollPageDown => "Scroll a page down",
            Command::ScrollTop => "Scroll to top",
            Command::ScrollBottom => "Scroll to bottom",
        }
    }

    fn from_name(name: &str) -> Option<Command> {
        Command::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The keys it has unless the settings file says otherwise.
    fn defaults(self) -> &'static [&'static str] {
        let mac = cfg!(target_os = "macos");
        match self {
            Command::NewTab if mac => &["Cmd+T"],
            Command::NewTab => &["Ctrl+Shift+T"],
            Command::CloseTab if mac => &["Cmd+W"],
            Command::CloseTab => &["Ctrl+Shift+W"],
            Command::NextTab if mac => &["Cmd+Shift+]", "Ctrl+Tab"],
            Command::NextTab => &["Ctrl+Tab", "Ctrl+PageDown"],
            Command::PreviousTab if mac => &["Cmd+Shift+[", "Ctrl+Shift+Tab"],
            Command::PreviousTab => &["Ctrl+Shift+Tab", "Ctrl+PageUp"],
            Command::Find if mac => &["Cmd+F"],
            Command::Find => &["Ctrl+Shift+F"],
            Command::Copy if mac => &["Cmd+C"],
            Command::Copy => &["Ctrl+Shift+C"],
            Command::Paste if mac => &["Cmd+V"],
            Command::Paste => &["Ctrl+Shift+V"],
            Command::Options if mac => &["Cmd+,"],
            Command::Options => &["Ctrl+Shift+,"],
            Command::CycleTheme if mac => &["Cmd+Shift+L"],
            Command::CycleTheme => &["Ctrl+Shift+L"],
            Command::FontBigger if mac => &["Cmd+=", "Cmd+Shift+="],
            Command::FontBigger => &["Ctrl+=", "Ctrl+Shift+=", "Ctrl++"],
            Command::FontSmaller if mac => &["Cmd+-"],
            Command::FontSmaller => &["Ctrl+-"],
            Command::FontReset if mac => &["Cmd+0"],
            Command::FontReset => &["Ctrl+0"],
            Command::ScrollPageUp => &["Shift+PageUp"],
            Command::ScrollPageDown => &["Shift+PageDown"],
            Command::ScrollTop => &["Shift+Home"],
            Command::ScrollBottom => &["Shift+End"],
        }
    }

    /// The menu action with the same effect, for menu hints.
    fn action(self) -> Option<Action> {
        Some(match self {
            Command::NewTab => Action::NewTab,
            Command::CloseTab => Action::CloseTab,
            Command::Find => Action::Find,
            Command::Copy => Action::Copy,
            Command::Paste => Action::Paste,
            Command::Options => Action::Options,
            _ => return None,
        })
    }
}

/// A key with modifiers, e.g. Ctrl+Shift+T.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Combo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Cmd on macOS, the Windows key elsewhere.
    pub logo: bool,
    /// Lowercase character ("t", ",") or key name ("Tab", "PageDown", "F3").
    pub key: String,
}

/// Named keys that can be part of a shortcut, as written in settings.
const NAMED: [(NamedKey, &str); 26] = [
    (NamedKey::Tab, "Tab"),
    (NamedKey::Enter, "Enter"),
    (NamedKey::Space, "Space"),
    (NamedKey::Backspace, "Backspace"),
    (NamedKey::Delete, "Delete"),
    (NamedKey::Insert, "Insert"),
    (NamedKey::Home, "Home"),
    (NamedKey::End, "End"),
    (NamedKey::PageUp, "PageUp"),
    (NamedKey::PageDown, "PageDown"),
    (NamedKey::ArrowUp, "Up"),
    (NamedKey::ArrowDown, "Down"),
    (NamedKey::ArrowLeft, "Left"),
    (NamedKey::ArrowRight, "Right"),
    (NamedKey::F1, "F1"),
    (NamedKey::F2, "F2"),
    (NamedKey::F3, "F3"),
    (NamedKey::F4, "F4"),
    (NamedKey::F5, "F5"),
    (NamedKey::F6, "F6"),
    (NamedKey::F7, "F7"),
    (NamedKey::F8, "F8"),
    (NamedKey::F9, "F9"),
    (NamedKey::F10, "F10"),
    (NamedKey::F11, "F11"),
    (NamedKey::F12, "F12"),
];

impl Combo {
    /// The combo a key press makes, or None for modifier keys alone and
    /// keys that can't be bound. `key` is the key without modifiers applied
    /// (Shift+, is ",", not "<").
    pub fn from_key(key: &Key, mods: ModifiersState) -> Option<Combo> {
        let key = match key {
            Key::Character(c) => c.to_lowercase(),
            Key::Named(n) => NAMED.iter().find(|(k, _)| k == n)?.1.to_owned(),
            _ => return None,
        };
        Some(Combo { ctrl: mods.control_key(), shift: mods.shift_key(), alt: mods.alt_key(), logo: mods.super_key(), key })
    }

    /// The combo of a key event.
    pub fn from_event(event: &KeyEvent, mods: ModifiersState) -> Option<Combo> {
        use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
        Combo::from_key(&event.key_without_modifiers(), mods)
    }

    /// Parses "Ctrl+Shift+T", "Cmd+,", "Ctrl++" (any case).
    pub fn parse(text: &str) -> Option<Combo> {
        let text = text.trim();
        let (mods, key) = if let Some(m) = text.strip_suffix("++") {
            (m, "+")
        } else if text == "+" {
            ("", "+")
        } else {
            match text.rsplit_once('+') {
                Some((m, k)) => (m, k),
                None => ("", text),
            }
        };
        let mut c = Combo { ctrl: false, shift: false, alt: false, logo: false, key: String::new() };
        for m in mods.split('+').map(str::trim).filter(|m| !m.is_empty()) {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" | "option" | "opt" => c.alt = true,
                "cmd" | "command" | "super" | "win" | "meta" | "logo" => c.logo = true,
                _ => return None,
            }
        }
        let key = key.trim();
        c.key = match NAMED.iter().find(|(_, name)| name.eq_ignore_ascii_case(key)) {
            Some((_, name)) => (*name).to_owned(),
            None if key.eq_ignore_ascii_case("pgup") => "PageUp".to_owned(),
            None if key.eq_ignore_ascii_case("pgdn") => "PageDown".to_owned(),
            None if key.chars().count() == 1 => key.to_lowercase(),
            None => return None,
        };
        Some(c)
    }
}

impl fmt::Display for Combo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let logo = if cfg!(target_os = "macos") { "Cmd" } else { "Win" };
        for (on, name) in [(self.ctrl, "Ctrl"), (self.alt, "Alt"), (self.logo, logo), (self.shift, "Shift")] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        match self.key.as_str() {
            "PageUp" => write!(f, "PgUp"),
            "PageDown" => write!(f, "PgDn"),
            k if k.chars().count() == 1 => write!(f, "{}", k.to_uppercase()),
            k => write!(f, "{k}"),
        }
    }
}

/// Writes combos as the settings file has them: "Ctrl+Tab, Ctrl+PageDown".
fn settings_text(combos: &[Combo]) -> String {
    combos
        .iter()
        .map(|c| {
            let shown = c.to_string();
            // The file uses full key names; the panel abbreviates.
            shown.replace("PgUp", "PageUp").replace("PgDn", "PageDown")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The shortcuts in effect: defaults with the settings file's changes.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<(Command, Vec<Combo>)>,
    lookup: HashMap<Combo, Command>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::new(&BTreeMap::new())
    }
}

impl Keymap {
    /// From the `[keys]` table: command name -> comma-separated combos
    /// ("" turns a command off). Unknown names and keys are logged and
    /// ignored.
    pub fn new(overrides: &BTreeMap<String, String>) -> Self {
        for name in overrides.keys() {
            if Command::from_name(name).is_none() {
                log::warn!("[keys]: unknown command {name:?}");
            }
        }
        let bindings: Vec<(Command, Vec<Combo>)> = Command::ALL
            .into_iter()
            .map(|cmd| {
                let combos = match overrides.get(cmd.name()) {
                    Some(text) => text
                        .split(',')
                        .map(str::trim)
                        .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("none"))
                        .filter_map(|t| {
                            let c = Combo::parse(t);
                            if c.is_none() {
                                log::warn!("[keys] {}: can't read {t:?}", cmd.name());
                            }
                            c
                        })
                        .collect(),
                    None => cmd.defaults().iter().filter_map(|t| Combo::parse(t)).collect(),
                };
                (cmd, combos)
            })
            .collect();
        let mut lookup = HashMap::new();
        for (cmd, combos) in &bindings {
            for c in combos {
                lookup.entry(c.clone()).or_insert(*cmd);
            }
        }
        Keymap { bindings, lookup }
    }

    /// The command a key press runs, if any.
    pub fn command(&self, combo: &Combo) -> Option<Command> {
        self.lookup.get(combo).copied()
    }

    pub fn combos(&self, cmd: Command) -> &[Combo] {
        self.bindings.iter().find(|(c, _)| *c == cmd).map(|(_, v)| v.as_slice()).unwrap_or(&[])
    }

    /// "Ctrl+Tab, Ctrl+PgDn", or "—" for none.
    pub fn keys_text(&self, cmd: Command) -> String {
        let combos = self.combos(cmd);
        if combos.is_empty() {
            "—".to_owned()
        } else {
            combos.iter().map(Combo::to_string).collect::<Vec<_>>().join(", ")
        }
    }

    /// The first key of a menu action's command, as a menu hint.
    pub fn hint(&self, action: Action) -> String {
        Command::ALL
            .into_iter()
            .find(|c| c.action() == Some(action))
            .and_then(|c| self.combos(c).first())
            .map(Combo::to_string)
            .unwrap_or_default()
    }

    /// The `[keys]` table after binding `cmd` to just `combo` (or to
    /// nothing): other commands lose that combo, and entries equal to the
    /// defaults are dropped.
    pub fn rebind(&self, cmd: Command, combo: Option<Combo>) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (c, combos) in &self.bindings {
            let new: Vec<Combo> = if *c == cmd {
                combo.iter().cloned().collect()
            } else {
                combos.iter().filter(|k| Some(*k) != combo.as_ref()).cloned().collect()
            };
            let defaults: Vec<Combo> = c.defaults().iter().filter_map(|t| Combo::parse(t)).collect();
            if new != defaults {
                out.insert(c.name().to_owned(), settings_text(&new));
            }
        }
        out
    }
}

/// Keys that can't be changed, for the Options panel: they only mean
/// something in one place (search) or are mouse actions.
pub fn fixed() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Enter / Shift+Enter", "Previous / next match"),
        ("F3 / Shift+F3", "Previous / next match"),
        ("Ctrl+R (in search)", "Toggle regex search"),
        ("Esc", "Close search, menu or panel"),
        ("Right click", "Copy / paste menu"),
        ("Middle click", "Paste; close tab on tab bar"),
        ("Shift+drag", "Select inside mouse apps (vim, htop)"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(s: &str) -> Combo {
        Combo::parse(s).unwrap_or_else(|| panic!("parses {s}"))
    }

    #[test]
    fn combos_parse_and_print() {
        let c = combo("ctrl+shift+t");
        assert!(c.ctrl && c.shift && !c.alt && !c.logo && c.key == "t");
        assert_eq!(c.to_string(), "Ctrl+Shift+T");
        assert_eq!(combo("Ctrl++").key, "+");
        assert_eq!(combo("Ctrl+,").key, ",");
        assert_eq!(combo("Shift+PgDn"), combo("Shift+PageDown"));
        assert_eq!(combo("Shift+PageDown").to_string(), "Shift+PgDn");
        assert_eq!(combo("cmd+f3"), Combo { ctrl: false, shift: false, alt: false, logo: true, key: "F3".into() });
        assert!(Combo::parse("Ctrl+Hyper+T").is_none());
        assert!(Combo::parse("Ctrl+NoSuchKey").is_none());
    }

    #[test]
    fn key_presses_match_with_shift_ignored_in_the_key() {
        let k = Keymap::default();
        let shift_ctrl = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let ctrl = if cfg!(target_os = "macos") { ModifiersState::SUPER } else { ModifiersState::CONTROL };
        if !cfg!(target_os = "macos") {
            // Ctrl+Shift+T arrives as "t" from key_without_modifiers.
            let c = Combo::from_key(&Key::Character("t".into()), shift_ctrl).unwrap();
            assert_eq!(k.command(&c), Some(Command::NewTab));
        }
        let c = Combo::from_key(&Key::Character("0".into()), ctrl).unwrap();
        assert_eq!(k.command(&c), Some(Command::FontReset));
        assert!(Combo::from_key(&Key::Named(NamedKey::Shift), ModifiersState::SHIFT).is_none(), "modifiers alone");
    }

    #[test]
    fn settings_change_and_turn_off_keys() {
        let mut o = BTreeMap::new();
        o.insert("new_tab".to_owned(), "Ctrl+T, Alt+N".to_owned());
        o.insert("find".to_owned(), "".to_owned());
        o.insert("no_such_thing".to_owned(), "Ctrl+X".to_owned());
        let k = Keymap::new(&o);
        assert_eq!(k.command(&combo("Ctrl+T")), Some(Command::NewTab));
        assert_eq!(k.command(&combo("Alt+N")), Some(Command::NewTab));
        assert!(k.combos(Command::Find).is_empty());
        assert_eq!(k.keys_text(Command::Find), "—");
        assert_eq!(k.hint(Action::NewTab), "Ctrl+T");
    }

    #[test]
    fn rebinding_moves_a_key_and_keeps_only_changes() {
        let k = Keymap::default();
        let next = k.combos(Command::NextTab)[0].clone();
        // Give Next tab's first key to New tab.
        let table = k.rebind(Command::NewTab, Some(next.clone()));
        assert_eq!(table.get("new_tab"), Some(&next.to_string()));
        assert!(!table["next_tab"].contains(&next.to_string()), "taken from Next tab");
        assert_eq!(table.len(), 2, "untouched commands stay at their defaults");
        let k2 = Keymap::new(&table);
        assert_eq!(k2.command(&next), Some(Command::NewTab));
        // Clearing.
        let cleared = k.rebind(Command::Find, None);
        assert_eq!(cleared.get("find").map(String::as_str), Some(""));
        // Next tab keeps its other keys.
        assert_eq!(k2.combos(Command::NextTab), &k.combos(Command::NextTab)[1..]);
    }

    #[test]
    fn every_default_parses_and_no_two_commands_share_a_key() {
        let k = Keymap::default();
        let mut seen = HashMap::new();
        for cmd in Command::ALL {
            assert_eq!(k.combos(cmd).len(), cmd.defaults().len(), "{cmd:?} defaults parse");
            for c in k.combos(cmd) {
                assert!(seen.insert(c.clone(), cmd).is_none(), "{c} bound twice");
            }
        }
    }
}
