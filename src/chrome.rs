//! Window chrome drawn by the GPU renderer: tab bar, the ⋮ menu, the
//! right-click menu, and the Options and About panels.
//!
//! Each element is a layout function that returns rectangles. The view
//! draws from the layout and the app hit-tests clicks against the same
//! layout, so they never disagree.

use crate::config::ThemeMode;
use crate::shortcuts;
use crate::ui::Rect;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    NewTab,
    CloseTab,
    Find,
    Copy,
    Paste,
    SelectAll,
    Theme(ThemeMode),
    Options,
    About,
    OpenSettingsFile,
    FontBigger,
    FontSmaller,
    CodeHighlight(bool),
    ClosePanel,
    Quit,
}

/// Sizes shared by every layout.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub cw: f32,
    pub ch: f32,
}

impl Metrics {
    fn px(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }
}

// ------------------------------------------------------------------ tab bar

pub struct TabSlot {
    pub rect: Rect,
    pub close: Rect,
}

pub struct TabBar {
    pub height: f32,
    pub tabs: Vec<TabSlot>,
    pub plus: Rect,
    pub menu: Rect,
}

pub fn tab_bar_height(m: &Metrics) -> f32 {
    (m.ch + m.px(14.0)).round()
}

pub fn tab_bar(m: &Metrics, count: usize) -> TabBar {
    let h = tab_bar_height(m);
    let button = h;
    let menu = Rect { x: m.width - button, y: 0.0, w: button, h };
    let gap = m.px(4.0);
    let avail = (m.width - 2.0 * button - 2.0 * gap).max(0.0);
    let tab_w = (avail / count.max(1) as f32).min(m.px(220.0)).floor();
    let tabs = (0..count)
        .map(|i| {
            let rect = Rect { x: gap + i as f32 * tab_w, y: m.px(4.0), w: tab_w - m.px(2.0), h: h - m.px(4.0) };
            let cs = m.ch;
            let close = Rect { x: rect.x + rect.w - cs - m.px(6.0), y: rect.y + (rect.h - cs) / 2.0, w: cs, h: cs };
            TabSlot { rect, close }
        })
        .collect();
    let plus = Rect { x: gap + count as f32 * tab_w, y: 0.0, w: button, h };
    TabBar { height: h, tabs, plus, menu }
}

// -------------------------------------------------------------------- menus

pub struct MenuItem {
    pub label: String,
    pub hint: String,
    /// None draws a separator.
    pub action: Option<Action>,
    pub enabled: bool,
    pub checked: bool,
}

impl MenuItem {
    pub fn new(label: &str, action: Action) -> Self {
        Self { label: label.to_owned(), hint: shortcuts::hint(action), action: Some(action), enabled: true, checked: false }
    }

    pub fn separator() -> Self {
        Self { label: String::new(), hint: String::new(), action: None, enabled: false, checked: false }
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
}

pub struct Menu {
    pub items: Vec<MenuItem>,
    /// Anchor point; the menu opens below it.
    pub x: f32,
    pub y: f32,
    /// Grow leftwards from the anchor (the ⋮ menu at the window's edge).
    pub align_right: bool,
    pub hover: Option<usize>,
}

pub struct MenuLayout {
    pub rect: Rect,
    pub rows: Vec<Rect>,
    pub pad: f32,
    /// Column where shortcut hints start.
    pub hint_x: f32,
}

impl Menu {
    /// The ⋮ menu.
    pub fn main(x: f32, y: f32, theme: ThemeMode, has_selection: bool, tabs: usize) -> Self {
        let items = vec![
            MenuItem::new("New tab", Action::NewTab),
            MenuItem::new("Close tab", Action::CloseTab).enabled(tabs > 0),
            MenuItem::separator(),
            MenuItem::new("Find…", Action::Find),
            MenuItem::new("Copy", Action::Copy).enabled(has_selection),
            MenuItem::new("Paste", Action::Paste),
            MenuItem::separator(),
            MenuItem::new("System theme", Action::Theme(ThemeMode::System)).checked(theme == ThemeMode::System),
            MenuItem::new("Dark theme", Action::Theme(ThemeMode::Dark)).checked(theme == ThemeMode::Dark),
            MenuItem::new("Light theme", Action::Theme(ThemeMode::Light)).checked(theme == ThemeMode::Light),
            MenuItem::separator(),
            MenuItem::new("Options…", Action::Options),
            MenuItem::new("About", Action::About),
            MenuItem::separator(),
            MenuItem::new("Quit", Action::Quit),
        ];
        Self { items, x, y, align_right: true, hover: None }
    }

    /// The right-click menu.
    pub fn context(x: f32, y: f32, has_selection: bool) -> Self {
        let items = vec![
            MenuItem::new("Copy", Action::Copy).enabled(has_selection),
            MenuItem::new("Paste", Action::Paste),
            MenuItem::new("Select all", Action::SelectAll),
            MenuItem::separator(),
            MenuItem::new("Find…", Action::Find),
        ];
        Self { items, x, y, align_right: false, hover: None }
    }

    pub fn layout(&self, m: &Metrics) -> MenuLayout {
        let pad = m.px(6.0);
        let row_h = m.ch + m.px(8.0);
        let sep_h = m.px(9.0);
        let label = self.items.iter().map(|i| i.label.chars().count()).max().unwrap_or(0) as f32;
        let hint = self.items.iter().map(|i| i.hint.chars().count()).max().unwrap_or(0) as f32;
        // [check] label   hint
        let w = (2.0 + label + if hint > 0.0 { 3.0 + hint } else { 1.0 }) * m.cw + 2.0 * pad;
        let h: f32 = self.items.iter().map(|i| if i.action.is_some() { row_h } else { sep_h }).sum::<f32>() + 2.0 * pad;

        let mut x = if self.align_right { self.x - w } else { self.x };
        x = x.clamp(0.0, (m.width - w).max(0.0));
        let mut y = self.y;
        if y + h > m.height {
            y = (m.height - h).max(0.0);
        }
        let mut rows = Vec::with_capacity(self.items.len());
        let mut ry = y + pad;
        for item in &self.items {
            let rh = if item.action.is_some() { row_h } else { sep_h };
            rows.push(Rect { x: x + pad / 2.0, y: ry, w: w - pad, h: rh });
            ry += rh;
        }
        let hint_x = x + pad + (2.0 + label + 3.0) * m.cw;
        MenuLayout { rect: Rect { x, y, w, h }, rows, pad, hint_x }
    }

    /// Index of the enabled item under (x, y).
    pub fn hit(&self, l: &MenuLayout, x: f32, y: f32) -> Option<usize> {
        l.rows
            .iter()
            .position(|r| r.contains(x, y))
            .filter(|&i| self.items[i].action.is_some() && self.items[i].enabled)
    }

    /// Moves the keyboard highlight to the next enabled item.
    pub fn step(&mut self, down: bool) {
        let n = self.items.len();
        let mut i = self.hover.unwrap_or(if down { n - 1 } else { 0 });
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if self.items[i].action.is_some() && self.items[i].enabled {
                self.hover = Some(i);
                return;
            }
        }
    }
}

// ------------------------------------------------------------------- panels

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    Title,
    Heading,
    Normal,
    Dim,
    Key,
}

pub struct TextLine {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub style: TextStyle,
}

pub struct Button {
    pub rect: Rect,
    pub label: String,
    pub action: Action,
    pub active: bool,
}

pub struct Panel {
    pub rect: Rect,
    pub buttons: Vec<Button>,
    pub lines: Vec<TextLine>,
}

impl Panel {
    pub fn hit(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons.iter().find(|b| b.rect.contains(x, y)).map(|b| b.action)
    }
}

/// Helper that lays out rows of text and buttons on a character grid.
struct PanelBuilder<'a> {
    m: &'a Metrics,
    rect: Rect,
    pad: f32,
    y: f32,
    row_h: f32,
    buttons: Vec<Button>,
    lines: Vec<TextLine>,
}

impl<'a> PanelBuilder<'a> {
    fn new(m: &'a Metrics, cols: f32, rows: f32) -> Self {
        let pad = m.px(18.0);
        let row_h = m.ch + m.px(10.0);
        let w = (cols * m.cw + 2.0 * pad).min(m.width - m.px(16.0));
        let h = (rows * row_h + 2.0 * pad).min(m.height - m.px(16.0));
        let rect = Rect { x: ((m.width - w) / 2.0).round(), y: ((m.height - h) / 2.0).round(), w, h };
        Self { m, rect, pad, y: rect.y + pad, row_h, buttons: Vec::new(), lines: Vec::new() }
    }

    fn left(&self) -> f32 {
        self.rect.x + self.pad
    }

    /// True if `n` more rows fit inside the panel.
    fn fits_rows(&self, n: usize) -> bool {
        self.y + n as f32 * self.row_h <= self.rect.y + self.rect.h - self.pad + 1.0
    }

    fn text(&mut self, col: f32, text: impl Into<String>, style: TextStyle) {
        let x = self.left() + col * self.m.cw;
        let y = self.y + (self.row_h - self.m.ch) / 2.0;
        self.lines.push(TextLine { x, y, text: text.into(), style });
    }

    /// Adds a button at column `col`; returns the column after it.
    fn button(&mut self, col: f32, label: &str, action: Action, active: bool) -> f32 {
        let w = (label.chars().count() as f32 + 2.0) * self.m.cw;
        let rect = Rect { x: self.left() + col * self.m.cw, y: self.y + self.m.px(2.0), w, h: self.row_h - self.m.px(4.0) };
        self.buttons.push(Button { rect, label: label.to_owned(), action, active });
        col + label.chars().count() as f32 + 3.0
    }

    fn close_button(&mut self) {
        let s = self.m.ch + self.m.px(8.0);
        let rect = Rect { x: self.rect.x + self.rect.w - s - self.m.px(8.0), y: self.rect.y + self.m.px(8.0), w: s, h: s };
        self.buttons.push(Button { rect, label: "×".to_owned(), action: Action::ClosePanel, active: false });
    }

    fn next(&mut self) {
        self.y += self.row_h;
    }

    fn gap(&mut self) {
        self.y += self.row_h / 2.0;
    }

    fn finish(self) -> Panel {
        Panel { rect: self.rect, buttons: self.buttons, lines: self.lines }
    }
}

pub struct OptionsState {
    pub theme: ThemeMode,
    pub font_size: f32,
    pub font_family: String,
    pub code_highlight: bool,
}

pub fn options_panel(m: &Metrics, s: &OptionsState) -> Panel {
    let keys = shortcuts::all();
    let key_w = keys.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0) as f32;
    let what_w = keys.iter().map(|(_, w)| w.chars().count()).max().unwrap_or(0) as f32;
    let entry_w = key_w + 2.0 + what_w;
    // Two columns of shortcuts when the window is wide enough.
    let two_cols = (2.0 * entry_w + 4.0) * m.cw + m.px(36.0) <= m.width - m.px(16.0);
    let cols = if two_cols { 2.0 * entry_w + 4.0 } else { 62.0f32.max(entry_w) };
    let key_rows = if two_cols { keys.len().div_ceil(2) } else { keys.len() };
    let mut b = PanelBuilder::new(m, cols, 8.0 + key_rows as f32);
    b.close_button();

    b.text(0.0, "Options", TextStyle::Title);
    b.next();
    b.gap();

    let label_w = 20.0;
    b.text(0.0, "Theme", TextStyle::Normal);
    let mut c = label_w;
    for (name, mode) in [("System", ThemeMode::System), ("Dark", ThemeMode::Dark), ("Light", ThemeMode::Light)] {
        c = b.button(c, name, Action::Theme(mode), s.theme == mode);
    }
    b.next();

    b.text(0.0, "Font size", TextStyle::Normal);
    let c = b.button(label_w, "−", Action::FontSmaller, false);
    b.text(c, format!("{:>4}", s.font_size), TextStyle::Normal);
    let c = b.button(c + 5.0, "+", Action::FontBigger, false);
    b.text(c, &s.font_family, TextStyle::Dim);
    b.next();

    b.text(0.0, "Code highlighting", TextStyle::Normal);
    let c = b.button(label_w, "On", Action::CodeHighlight(true), s.code_highlight);
    b.button(c, "Off", Action::CodeHighlight(false), !s.code_highlight);
    b.next();

    b.text(0.0, "Settings file", TextStyle::Normal);
    b.button(label_w, "Open config.toml", Action::OpenSettingsFile, false);
    b.next();
    b.gap();

    b.text(0.0, "Keyboard shortcuts", TextStyle::Heading);
    b.next();
    let rows: Vec<&[(String, &str)]> = if two_cols { keys.chunks(2).collect() } else { keys.chunks(1).collect() };
    for (n, row) in rows.iter().enumerate() {
        let last_fitting = !b.fits_rows(2) && n + 1 < rows.len();
        if last_fitting {
            b.text(0.0, "… more in the README", TextStyle::Dim);
            break;
        }
        for (i, (k, what)) in row.iter().enumerate() {
            let x = i as f32 * (entry_w + 4.0);
            b.text(x, k.as_str(), TextStyle::Key);
            b.text(x + key_w + 2.0, *what, TextStyle::Normal);
        }
        b.next();
    }
    b.finish()
}

pub fn about_panel(m: &Metrics, gpu: &str, config_path: &str) -> Panel {
    let mut b = PanelBuilder::new(m, 64.0, 9.5);
    b.close_button();
    b.text(0.0, "Ragamuffin", TextStyle::Title);
    b.next();
    b.text(0.0, format!("Version {}", env!("CARGO_PKG_VERSION")), TextStyle::Dim);
    b.next();
    b.gap();
    b.text(0.0, "Fast GPU-accelerated terminal with search and code highlighting.", TextStyle::Normal);
    b.next();
    b.text(0.0, "GPU", TextStyle::Dim);
    b.text(10.0, gpu, TextStyle::Normal);
    b.next();
    b.text(0.0, "Settings", TextStyle::Dim);
    b.text(10.0, config_path, TextStyle::Normal);
    b.next();
    b.text(0.0, "License", TextStyle::Dim);
    b.text(10.0, env!("CARGO_PKG_LICENSE"), TextStyle::Normal);
    b.next();
    b.text(0.0, "Built on", TextStyle::Dim);
    b.text(10.0, "wgpu, winit, alacritty_terminal, cosmic-text, syntect", TextStyle::Normal);
    b.next();
    b.gap();
    b.button(0.0, "Close", Action::ClosePanel, false);
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Metrics {
        Metrics { width: 1000.0, height: 700.0, scale: 1.0, cw: 8.0, ch: 17.0 }
    }

    #[test]
    fn tab_bar_fits_window() {
        let t = tab_bar(&m(), 30);
        let last = t.tabs.last().unwrap().rect;
        assert!(last.x + last.w <= t.menu.x);
        assert!(t.menu.x + t.menu.w <= 1000.0);
    }

    #[test]
    fn menu_stays_inside_window_and_hits_items() {
        let menu = Menu::main(1000.0, 30.0, ThemeMode::System, false, 1);
        let l = menu.layout(&m());
        assert!(l.rect.x >= 0.0 && l.rect.x + l.rect.w <= 1000.0);
        let first = l.rows[0];
        assert_eq!(menu.hit(&l, first.x + 2.0, first.y + 2.0), Some(0));
        // "Copy" is disabled without a selection.
        let copy = l.rows[4];
        assert_eq!(menu.hit(&l, copy.x + 2.0, copy.y + 2.0), None);
    }

    #[test]
    fn keyboard_navigation_skips_separators_and_disabled() {
        let mut menu = Menu::context(10.0, 10.0, false);
        menu.step(true);
        assert_eq!(menu.hover, Some(1)); // Copy disabled → Paste
        menu.step(true);
        assert_eq!(menu.hover, Some(2));
        menu.step(true);
        assert_eq!(menu.hover, Some(4)); // separator skipped
    }

    #[test]
    fn tab_close_buttons_sit_inside_their_tabs() {
        let t = tab_bar(&m(), 3);
        for slot in &t.tabs {
            let (r, c) = (slot.rect, slot.close);
            assert!(c.x >= r.x && c.x + c.w <= r.x + r.w && c.y >= r.y && c.y + c.h <= r.y + r.h);
        }
        // + follows the last tab; the menu button is at the right edge.
        let last = t.tabs[2].rect;
        assert!(t.plus.x >= last.x + last.w);
        assert_eq!(t.menu.x + t.menu.w, 1000.0);
        assert_eq!(t.height, tab_bar_height(&m()));
    }

    #[test]
    fn menus_open_inside_the_window() {
        // Right-click near the bottom-right corner: shifted up and left.
        let menu = Menu::context(995.0, 695.0, true);
        let l = menu.layout(&m());
        assert!(l.rect.x + l.rect.w <= 1000.0);
        assert!(l.rect.y + l.rect.h <= 700.0);
        // The ⋮ menu grows leftwards from its anchor.
        let main = Menu::main(1000.0, 31.0, ThemeMode::Light, true, 1);
        let l = main.layout(&m());
        assert_eq!(l.rect.x + l.rect.w, 1000.0);
        assert_eq!(l.rect.y, 31.0);
        // Theme radio items: exactly the current one is checked.
        let checked: Vec<_> = main.items.iter().filter(|i| i.checked).map(|i| i.action).collect();
        assert_eq!(checked, vec![Some(Action::Theme(ThemeMode::Light))]);
        assert!(l.hint_x > l.rect.x);
    }

    #[test]
    fn copy_is_enabled_only_with_a_selection() {
        for sel in [false, true] {
            let menu = Menu::context(0.0, 0.0, sel);
            let copy = menu.items.iter().find(|i| i.action == Some(Action::Copy)).unwrap();
            assert_eq!(copy.enabled, sel);
        }
    }

    #[test]
    fn about_panel_shows_version_gpu_and_close() {
        let p = about_panel(&m(), "Test GPU (Vulkan)", "/home/x/.config/ragamuffin/config.toml");
        let text: Vec<&str> = p.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(text.iter().any(|t| t.contains(env!("CARGO_PKG_VERSION"))));
        assert!(text.contains(&"Test GPU (Vulkan)"));
        assert!(p.buttons.iter().filter(|b| b.action == Action::ClosePanel).count() == 2, "× and Close");
        assert!(p.rect.x >= 0.0 && p.rect.x + p.rect.w <= 1000.0);
    }

    #[test]
    fn options_panel_fits_small_windows() {
        let small = Metrics { width: 500.0, height: 300.0, ..m() };
        let p = options_panel(&small, &OptionsState { theme: ThemeMode::System, font_size: 11.0, font_family: "X".into(), code_highlight: false });
        assert!(p.rect.w <= 500.0 && p.rect.h <= 300.0);
        assert!(p.lines.iter().all(|l| l.y + small.ch <= p.rect.y + p.rect.h), "no text below the panel");
        let off = p.buttons.iter().find(|b| b.action == Action::CodeHighlight(false)).unwrap();
        assert!(off.active);
    }

    #[test]
    fn options_panel_has_theme_buttons_and_shortcuts() {
        let p = options_panel(&m(), &OptionsState { theme: ThemeMode::Dark, font_size: 11.0, font_family: "X".into(), code_highlight: true });
        let dark = p.buttons.iter().find(|b| b.action == Action::Theme(ThemeMode::Dark)).unwrap();
        assert!(dark.active);
        assert_eq!(p.hit(dark.rect.x + 1.0, dark.rect.y + 1.0), Some(Action::Theme(ThemeMode::Dark)));
        assert!(p.lines.iter().any(|l| l.text == "Keyboard shortcuts"));
    }
}
