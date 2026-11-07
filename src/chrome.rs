//! Window chrome drawn by the GPU renderer: tab bar, the ⋮ menu, the
//! right-click menu, and the Options and About panels.
//!
//! Each element is a layout function that returns rectangles. The view
//! draws from the layout and the app hit-tests clicks against the same
//! layout, so they never disagree.

use crate::config::{GpuBackend, ThemeMode};
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
    /// Open the GitHub project page / its Releases page in the browser.
    OpenProjectPage,
    OpenReleases,
    FontBigger,
    FontSmaller,
    CodeHighlight(bool),
    /// Switch the Options panel to this page.
    OptionsPage(OptionsPage),
    /// Graphics API for the next start.
    Gpu(GpuBackend),
    ClosePanel,
    Quit,
}

/// The pages of the Options panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptionsPage {
    #[default]
    General,
    Shortcuts,
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

/// Window controls drawn in the tab bar instead of an OS title bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caption {
    /// Draw minimize / maximize / close at the right (Windows, Linux).
    pub buttons: bool,
    /// Space kept free at the left for macOS's traffic-light buttons.
    pub left_inset: f32,
}

impl Caption {
    /// The OS draws the title bar; the tab bar has no window controls.
    pub const NONE: Self = Self { buttons: false, left_inset: 0.0 };
}

pub struct TabBar {
    pub height: f32,
    pub tabs: Vec<TabSlot>,
    pub plus: Rect,
    pub menu: Rect,
    /// Window controls, when the app draws its own title bar.
    pub minimize: Option<Rect>,
    pub maximize: Option<Rect>,
    pub close: Option<Rect>,
}

pub fn tab_bar_height(m: &Metrics) -> f32 {
    (m.ch + m.px(14.0)).round()
}

/// Width of one window-control button.
fn caption_button_width(m: &Metrics) -> f32 {
    m.px(46.0)
}

pub fn tab_bar(m: &Metrics, count: usize, caption: &Caption) -> TabBar {
    let h = tab_bar_height(m);
    let button = h;
    let (mut right, mut minimize, mut maximize, mut close) = (m.width, None, None, None);
    if caption.buttons {
        let bw = caption_button_width(m);
        close = Some(Rect { x: right - bw, y: 0.0, w: bw, h });
        maximize = Some(Rect { x: right - 2.0 * bw, y: 0.0, w: bw, h });
        minimize = Some(Rect { x: right - 3.0 * bw, y: 0.0, w: bw, h });
        right -= 3.0 * bw;
    }
    let menu = Rect { x: right - button, y: 0.0, w: button, h };
    let gap = m.px(4.0);
    let left = caption.left_inset + gap;
    // Keep a strip of empty bar after "+" so there is always somewhere to
    // grab the window.
    let grab = if caption.buttons || caption.left_inset > 0.0 { m.px(48.0) } else { 0.0 };
    let avail = (menu.x - left - button - gap - grab).max(0.0);
    let tab_w = (avail / count.max(1) as f32).min(m.px(220.0)).floor();
    let tabs = (0..count)
        .map(|i| {
            let rect = Rect { x: left + i as f32 * tab_w, y: m.px(4.0), w: tab_w - m.px(2.0), h: h - m.px(4.0) };
            let cs = m.ch;
            let close = Rect { x: rect.x + rect.w - cs - m.px(6.0), y: rect.y + (rect.h - cs) / 2.0, w: cs, h: cs };
            TabSlot { rect, close }
        })
        .collect();
    let plus = Rect { x: left + count as f32 * tab_w, y: 0.0, w: button, h };
    TabBar { height: h, tabs, plus, menu, minimize, maximize, close }
}

/// Which window edge or corner (x, y) is on, for resizing a window without
/// an OS frame. `border` is the grab width in pixels.
pub fn resize_edge(x: f32, y: f32, width: f32, height: f32, border: f32) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection as D;
    let (l, r, t, b) = (x < border, x >= width - border, y < border, y >= height - border);
    Some(match (l, r, t, b) {
        (true, _, true, _) => D::NorthWest,
        (_, true, true, _) => D::NorthEast,
        (true, _, _, true) => D::SouthWest,
        (_, true, _, true) => D::SouthEast,
        (true, ..) => D::West,
        (_, true, ..) => D::East,
        (_, _, true, _) => D::North,
        (_, _, _, true) => D::South,
        _ => return None,
    })
}

// -------------------------------------------------------------------- menus

pub struct MenuItem {
    pub label: String,
    pub hint: String,
    /// None draws a separator.
    pub action: Option<Action>,
    pub enabled: bool,
}

impl MenuItem {
    pub fn new(label: &str, action: Action) -> Self {
        Self { label: label.to_owned(), hint: shortcuts::hint(action), action: Some(action), enabled: true }
    }

    pub fn separator() -> Self {
        Self { label: String::new(), hint: String::new(), action: None, enabled: false }
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
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
    pub fn main(x: f32, y: f32, has_selection: bool, tabs: usize) -> Self {
        let items = vec![
            MenuItem::new("New tab", Action::NewTab),
            MenuItem::new("Close tab", Action::CloseTab).enabled(tabs > 0),
            MenuItem::separator(),
            MenuItem::new("Find…", Action::Find),
            MenuItem::new("Copy", Action::Copy).enabled(has_selection),
            MenuItem::new("Paste", Action::Paste),
            MenuItem::separator(),
            MenuItem::new("Options…", Action::Options),
            MenuItem::new("About", Action::About),
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
        // margin, label, hint, margin
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
        let hint_x = x + pad + (1.0 + label + 3.0) * m.cw;
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

    /// Like `finish`, but ends the panel below its last row; the top stays
    /// where the size given to `new` put it.
    fn finish_fitted(mut self) -> Panel {
        self.rect.h = self.rect.h.min(self.y - self.rect.y + self.pad);
        self.finish()
    }
}

#[derive(Clone)]
pub struct OptionsState {
    pub page: OptionsPage,
    pub theme: ThemeMode,
    pub font_size: f32,
    pub font_family: String,
    pub code_highlight: bool,
    pub gpu: GpuBackend,
    /// `gpu` differs from what this run started with.
    pub gpu_pending: bool,
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
    // Placed for the taller page and cut to each page's length, so the
    // page buttons stay put when switching.
    const GENERAL_ROWS: usize = 6;
    let mut b = PanelBuilder::new(m, cols, 4.0 + key_rows.max(GENERAL_ROWS) as f32);
    b.close_button();

    b.text(0.0, "Options", TextStyle::Title);
    let c = b.button(12.0, "General", Action::OptionsPage(OptionsPage::General), s.page == OptionsPage::General);
    b.button(c, "Keyboard shortcuts", Action::OptionsPage(OptionsPage::Shortcuts), s.page == OptionsPage::Shortcuts);
    b.next();
    b.gap();

    match s.page {
        OptionsPage::General => {
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

            b.text(0.0, "Graphics", TextStyle::Normal);
            let mut c = label_w;
            for &g in GpuBackend::choices() {
                c = b.button(c, g.label(), Action::Gpu(g), s.gpu == g);
            }
            b.next();
            if s.gpu_pending {
                b.text(label_w, "Takes effect when ragamuffin restarts", TextStyle::Dim);
                b.next();
            }

            b.text(0.0, "Settings file", TextStyle::Normal);
            b.button(label_w, "Open config.toml", Action::OpenSettingsFile, false);
            b.next();
        }
        OptionsPage::Shortcuts => {
            let rows: Vec<&[(String, &str)]> = if two_cols { keys.chunks(2).collect() } else { keys.chunks(1).collect() };
            for (n, row) in rows.iter().enumerate() {
                let last_fitting = !b.fits_rows(2) && n + 1 < rows.len();
                if last_fitting {
                    b.text(0.0, "… more in the README", TextStyle::Dim);
                    b.next();
                    break;
                }
                for (i, (k, what)) in row.iter().enumerate() {
                    let x = i as f32 * (entry_w + 4.0);
                    b.text(x, k.as_str(), TextStyle::Key);
                    b.text(x + key_w + 2.0, *what, TextStyle::Normal);
                }
                b.next();
            }
        }
    }
    b.finish_fitted()
}

pub fn about_panel(m: &Metrics, gpu: &str, startup: &str, config_path: &str) -> Panel {
    let mut b = PanelBuilder::new(m, 66.0, 11.5);
    b.close_button();
    b.text(0.0, "Ragamuffin", TextStyle::Title);
    b.next();
    let build = crate::VERSION.rsplit('.').next().unwrap_or("0");
    b.text(0.0, format!("Version {} (build {build})", crate::VERSION), TextStyle::Dim);
    b.next();
    b.gap();
    b.text(0.0, "Fast GPU-accelerated terminal with search and code highlighting.", TextStyle::Normal);
    b.next();
    b.text(0.0, "GitHub", TextStyle::Dim);
    b.text(10.0, crate::REPOSITORY, TextStyle::Normal);
    b.next();
    b.text(0.0, "GPU", TextStyle::Dim);
    b.text(10.0, gpu, TextStyle::Normal);
    b.next();
    b.text(0.0, "Startup", TextStyle::Dim);
    b.text(10.0, startup, TextStyle::Normal);
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
    let c = b.button(0.0, "GitHub page", Action::OpenProjectPage, false);
    let c = b.button(c, "Download updates", Action::OpenReleases, false);
    b.button(c, "Close", Action::ClosePanel, false);
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
        let t = tab_bar(&m(), 30, &Caption::NONE);
        let last = t.tabs.last().unwrap().rect;
        assert!(last.x + last.w <= t.menu.x);
        assert!(t.menu.x + t.menu.w <= 1000.0);
    }

    #[test]
    fn menu_stays_inside_window_and_hits_items() {
        let menu = Menu::main(1000.0, 30.0, false, 1);
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
        let t = tab_bar(&m(), 3, &Caption::NONE);
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
    fn window_controls_sit_right_of_the_menu_button() {
        let cap = Caption { buttons: true, left_inset: 0.0 };
        let t = tab_bar(&m(), 4, &cap);
        let (min, max, close) = (t.minimize.unwrap(), t.maximize.unwrap(), t.close.unwrap());
        assert_eq!(close.x + close.w, 1000.0, "close at the right edge");
        assert!(min.x + min.w <= max.x && max.x + max.w <= close.x);
        assert!(t.menu.x + t.menu.w <= min.x, "⋮ before the window buttons");
        assert!(t.plus.x + t.plus.w < t.menu.x, "free strip to drag the window");
        assert!(tab_bar(&m(), 4, &Caption::NONE).close.is_none());
    }

    #[test]
    fn macos_inset_moves_tabs_right() {
        let cap = Caption { buttons: false, left_inset: 78.0 };
        let t = tab_bar(&m(), 2, &cap);
        assert!(t.tabs[0].rect.x >= 78.0);
        assert!(t.close.is_none());
    }

    #[test]
    fn resize_edges_and_corners() {
        use winit::window::ResizeDirection as D;
        let e = |x, y| resize_edge(x, y, 800.0, 600.0, 6.0);
        assert_eq!(e(400.0, 300.0), None);
        assert_eq!(e(2.0, 300.0), Some(D::West));
        assert_eq!(e(798.0, 300.0), Some(D::East));
        assert_eq!(e(400.0, 1.0), Some(D::North));
        assert_eq!(e(400.0, 599.0), Some(D::South));
        assert_eq!(e(1.0, 1.0), Some(D::NorthWest));
        assert_eq!(e(799.0, 599.0), Some(D::SouthEast));
    }

    #[test]
    fn menus_open_inside_the_window() {
        // Right-click near the bottom-right corner: shifted up and left.
        let menu = Menu::context(995.0, 695.0, true);
        let l = menu.layout(&m());
        assert!(l.rect.x + l.rect.w <= 1000.0);
        assert!(l.rect.y + l.rect.h <= 700.0);
        // The ⋮ menu grows leftwards from its anchor.
        let main = Menu::main(1000.0, 31.0, true, 1);
        let l = main.layout(&m());
        assert_eq!(l.rect.x + l.rect.w, 1000.0);
        assert_eq!(l.rect.y, 31.0);
        // No theme choices or Quit: those are in Options and the window's ×.
        assert!(!main.items.iter().any(|i| matches!(i.action, Some(Action::Theme(_) | Action::Quit))));
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
        let p = about_panel(&m(), "Test GPU (Vulkan)", "300 ms (GPU 100 ms, fonts 0 ms)", "/home/x/.config/ragamuffin/config.toml");
        let text: Vec<&str> = p.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(text.iter().any(|t| t.contains(crate::VERSION) && t.contains("build")));
        assert!(text.contains(&crate::REPOSITORY));
        let actions: Vec<Action> = p.buttons.iter().map(|b| b.action).collect();
        assert!(actions.contains(&Action::OpenProjectPage) && actions.contains(&Action::OpenReleases));
        assert!(text.contains(&"Test GPU (Vulkan)"));
        assert!(text.contains(&"300 ms (GPU 100 ms, fonts 0 ms)"));
        assert!(p.buttons.iter().filter(|b| b.action == Action::ClosePanel).count() == 2, "× and Close");
        assert!(p.rect.x >= 0.0 && p.rect.x + p.rect.w <= 1000.0);
    }

    #[test]
    fn options_panel_fits_small_windows() {
        let small = Metrics { width: 500.0, height: 300.0, ..m() };
        let p = options_panel(&small, &OptionsState { page: OptionsPage::General, theme: ThemeMode::System, font_size: 11.0, font_family: "X".into(), code_highlight: false, gpu: GpuBackend::Auto, gpu_pending: false });
        assert!(p.rect.w <= 500.0 && p.rect.h <= 300.0);
        assert!(p.lines.iter().all(|l| l.y + small.ch <= p.rect.y + p.rect.h), "no text below the panel");
        let off = p.buttons.iter().find(|b| b.action == Action::CodeHighlight(false)).unwrap();
        assert!(off.active);
    }

    #[test]
    fn options_panel_has_a_general_page_and_a_shortcuts_page() {
        let mut s = OptionsState { page: OptionsPage::General, theme: ThemeMode::Dark, font_size: 11.0, font_family: "X".into(), code_highlight: true, gpu: GpuBackend::Auto, gpu_pending: false };
        let p = options_panel(&m(), &s);
        let dark = p.buttons.iter().find(|b| b.action == Action::Theme(ThemeMode::Dark)).unwrap();
        assert!(dark.active);
        assert_eq!(p.hit(dark.rect.x + 1.0, dark.rect.y + 1.0), Some(Action::Theme(ThemeMode::Dark)));
        let tab = p.buttons.iter().find(|b| b.action == Action::OptionsPage(OptionsPage::Shortcuts)).unwrap();
        assert!(!tab.active);
        assert!(!p.lines.iter().any(|l| l.text == "New tab"), "shortcuts are on their own page");

        // Graphics: every API of this platform, the chosen one lit; a
        // change waiting for a restart says so.
        let gpus: Vec<_> = p.buttons.iter().filter(|b| matches!(b.action, Action::Gpu(_))).collect();
        assert_eq!(gpus.len(), GpuBackend::choices().len());
        assert!(gpus.iter().any(|b| b.active && b.action == Action::Gpu(GpuBackend::Auto)));
        assert!(!p.lines.iter().any(|l| l.text.contains("restarts")));
        let pending = options_panel(&m(), &OptionsState { gpu: GpuBackend::Software, gpu_pending: true, ..s.clone() });
        assert!(pending.lines.iter().any(|l| l.text.contains("restarts")));

        s.page = OptionsPage::Shortcuts;
        // In a small window the list is cut, and says so inside the panel.
        let small = Metrics { width: 600.0, height: 300.0, ..m() };
        let cut = options_panel(&small, &s);
        let more = cut.lines.iter().find(|l| l.text.starts_with("… more")).expect("cut list says so");
        assert!(more.y + small.ch <= cut.rect.y + cut.rect.h, "the note is inside the panel");
        let q = options_panel(&m(), &s);
        assert!(q.lines.iter().any(|l| l.text == "New tab"));
        assert!(!q.buttons.iter().any(|b| matches!(b.action, Action::Theme(_))));
        assert_eq!((p.rect.x, p.rect.y, p.rect.w), (q.rect.x, q.rect.y, q.rect.w), "page buttons stay put");
        assert!(p.rect.h < q.rect.h, "the short page isn't padded to the long one");
    }
}
