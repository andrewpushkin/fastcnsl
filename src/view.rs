//! Turns terminal state into quads: cells, selection, search matches,
//! code colours, cursor, scrollbar, search bar, and the window chrome
//! (tab bar, menus, panels).
//!
//! Everything is read through borrows of the locked terminal: cells are
//! never copied out of the grid.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

use crate::chrome::{self, ButtonStyle, Caption, Menu, Metrics, Panel, TextStyle};
use crate::config::LineNumbers;
use crate::highlight::{CodeHighlighter, Span};
use crate::render::glyphs::GlyphStyle;
use crate::render::{Layer, Painter};
use crate::search::Search;
use crate::search::MAX_BOXES;
use crate::theme::{MatchColors, Palette, Rgb};
use crate::ui;

pub struct View<'a> {
    pub palette: &'a Palette,
    pub search: &'a Search,
    pub code: &'a CodeHighlighter,
    pub focused: bool,
    pub bold_is_bright: bool,
    pub scale: f32,
    pub padding: f32,
    /// Height of the tab bar; the terminal is drawn below it.
    pub top: f32,
    pub scrollbar_alpha: f32,
    pub search_alpha: f32,
    /// Text on a coloured background is made at least this readable
    /// (contrast ratio, 1 = off).
    pub min_contrast: f32,
    /// Width of the line-number gutter left of the text (0: none).
    pub gutter: f32,
    /// The numbers beside the rows on screen, top to bottom (see
    /// `LineIndex::numbers`).
    pub line_numbers: &'a [Option<u64>],
    /// What `line_numbers` hold: per command seconds to show as times,
    /// otherwise numbers.
    pub line_number_mode: LineNumbers,
    /// The screen rows of bookmarked lines, ascending (see
    /// `LineIndex::bookmarked_rows`).
    pub bookmarks: &'a [usize],
    /// For the scrollbar: the tab's history rows and how far up the view is
    /// (see `Scroller::metrics`); None takes them from the terminal drawn.
    pub scroll: Option<(usize, usize)>,
}

/// What the pointer is over, for hover highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hover {
    None,
    Tab(usize),
    TabClose(usize),
    Plus,
    MenuButton,
    PanelButton(usize),
    /// A part of the search bars.
    Search(ui::SearchHit),
    Minimize,
    Maximize,
    CloseWindow,
}

pub struct ChromeView<'a> {
    pub caption: Caption,
    pub maximized: bool,
    pub titles: Vec<&'a str>,
    pub active: usize,
    pub hover: Hover,
    pub menu_open: bool,
    pub menu: Option<&'a Menu>,
    pub panel: Option<&'a Panel>,
    /// Which tabs are being logged to a file (a dot on the tab).
    pub logging: Vec<bool>,
    /// Which tabs printed something since they were last selected (a bell).
    pub activity: Vec<bool>,
    /// A short notice at the bottom of the window ("Saved to ...").
    pub toast: Option<&'a str>,
}

fn rgb(c: alacritty_terminal::vte::ansi::Rgb) -> Rgb {
    Rgb { r: c.r, g: c.g, b: c.b }
}

pub fn named(n: NamedColor, pal: &Palette) -> Rgb {
    let i = n as usize;
    if i < 16 {
        return pal.ansi[i];
    }
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground => pal.foreground,
        NamedColor::Background => pal.background,
        NamedColor::Cursor => pal.cursor,
        NamedColor::DimForeground => pal.foreground.mix(pal.background, 0.35),
        _ => {
            let base = i - NamedColor::DimBlack as usize;
            pal.ansi[base.min(7)].mix(pal.background, 0.35)
        }
    }
}

fn resolve(c: Color, colors: &Colors, pal: &Palette) -> Rgb {
    match c {
        Color::Spec(v) => rgb(v),
        Color::Indexed(i) => colors[i as usize].map(rgb).unwrap_or_else(|| pal.indexed(i)),
        Color::Named(n) => colors[n].map(rgb).unwrap_or_else(|| named(n, pal)),
    }
}

/// Colour for an index as reported to programs querying it (OSC 4/10/11).
pub fn color_for_index<T>(term: &Term<T>, index: usize, pal: &Palette) -> Rgb {
    if let Some(c) = term.colors()[index] {
        return rgb(c);
    }
    match index {
        0..=255 => pal.indexed(index as u8),
        256 => pal.foreground,
        257 => pal.background,
        _ => pal.cursor,
    }
}

fn span_at(spans: &[Span], col: u16) -> Option<Rgb> {
    let i = spans.partition_point(|s| s.end <= col);
    spans.get(i).filter(|s| s.start <= col).map(|s| s.color)
}

pub fn draw<T: EventListener>(p: &mut Painter<'_>, term: &Term<T>, v: &View<'_>, c: &ChromeView<'_>) {
    draw_grid(p, term, v);
    draw_bookmarks(p, v);
    draw_line_numbers(p, v);
    draw_scrollbar(p, term, v);
    if v.search.open {
        draw_search_bar(p, v, c.hover);
    }
    let m = Metrics { width: p.width, height: p.height, scale: v.scale, cw: p.cell.width, ch: p.cell.height };
    draw_tab_bar(p, v.palette, &m, c);
    if let Some(panel) = c.panel {
        draw_panel(p, v.palette, &m, panel, c.hover);
    }
    if let Some(menu) = c.menu {
        draw_menu(p, v.palette, &m, menu);
    }
    if let Some(text) = c.toast {
        draw_toast(p, v.palette, &m, text);
    }
}

/// A notice centred at the bottom of the window, on top of everything.
fn draw_toast(p: &mut Painter<'_>, pal: &Palette, m: &Metrics, text: &str) {
    let margin = (16.0 * m.scale).round();
    let pad = (12.0 * m.scale).round();
    let room = ((m.width - 2.0 * margin - 2.0 * pad) / m.cw).floor().max(0.0) as usize;
    let count = text.chars().count();
    let shown: String = if count > room { text.chars().take(room.saturating_sub(1)).chain(std::iter::once('…')).collect() } else { text.to_owned() };
    let w = shown.chars().count() as f32 * m.cw + 2.0 * pad;
    let h = m.ch + (16.0 * m.scale).round();
    let x = ((m.width - w) / 2.0).round();
    let y = (m.height - h - margin).round();
    let r = m.scale * 6.0;
    let b = m.scale.round().max(1.0);
    p.rounded(Layer::OverlayShapes, x + m.scale * 3.0, y + m.scale * 3.0, w, h, r, [0.0, 0.0, 0.0, 0.25]);
    p.rounded(Layer::OverlayShapes, x - b, y - b, w + 2.0 * b, h + 2.0 * b, r + b, pal.ui_dim.rgba(0.6));
    p.rounded(Layer::OverlayShapes, x, y, w, h, r, pal.ui_bg.rgba(1.0));
    p.text(Layer::OverlayText, &shown, x + pad, (y + (h - m.ch) / 2.0).round(), pal.ui_fg.rgba(1.0));
}

fn draw_tab_bar(p: &mut Painter<'_>, pal: &Palette, m: &Metrics, c: &ChromeView<'_>) {
    let bar = chrome::tab_bar(m, c.titles.len(), &c.caption);
    let bar_bg = tab_bar_background(pal);
    p.rect(Layer::UiShapes, 0.0, 0.0, m.width, bar.height, bar_bg.rgba(1.0));
    let r = m.scale * 6.0;
    let text_y = |rect: &ui::Rect| (rect.y + (rect.h - m.ch) / 2.0).round();

    for (i, (slot, title)) in bar.tabs.iter().zip(&c.titles).enumerate() {
        let active = i == c.active;
        let hovered = matches!(c.hover, Hover::Tab(h) | Hover::TabClose(h) if h == i);
        let t = slot.rect;
        if active {
            // Active tab merges into the terminal below it.
            p.rounded(Layer::UiShapes, t.x, t.y, t.w, t.h + r, r, pal.background.rgba(1.0));
        } else if hovered {
            p.rounded(Layer::UiShapes, t.x, t.y, t.w, t.h - m.scale * 3.0, r, pal.background.mix(bar_bg, 0.5).rgba(1.0));
        }
        // Title, shortened to fit before the close button.
        // A tab being logged has a red dot before its title.
        let logging = c.logging.get(i).copied().unwrap_or(false);
        let lead = if logging { 2.0 * m.cw } else { m.cw };
        let room = ((slot.close.x - t.x - lead) / m.cw).floor().max(0.0) as usize;
        let shown = fit_tab_title(i, title, room);
        // A lone number is centred: the tab is too narrow for anything else.
        let tx = if shown.chars().count() > room || room <= 3 {
            (t.x + (t.w - shown.chars().count() as f32 * m.cw) / 2.0).round()
        } else {
            t.x + lead
        };
        // The bell takes the place of the close button, which only the
        // selected or hovered tab shows. On a tab so narrow that the place
        // is under the number, the number is coloured instead.
        let activity = !active && c.activity.get(i).copied().unwrap_or(false);
        let bell_fits = tx + shown.chars().count() as f32 * m.cw <= slot.close.x;
        let color = match (active, activity && !bell_fits) {
            (true, _) => pal.foreground,
            (false, true) => pal.ui_accent,
            (false, false) => pal.ui_dim,
        };
        p.text(Layer::UiText, &shown, tx, text_y(&t), color.rgba(1.0));
        if logging {
            let d = (m.cw * 0.5).round().max(4.0);
            let cx = t.x + m.cw - d / 2.0;
            p.rounded(Layer::UiShapes, cx.round(), (t.y + (t.h - d) / 2.0).round(), d, d, d / 2.0, pal.ansi[1].rgba(1.0));
        }
        if active || hovered {
            let cl = slot.close;
            if c.hover == Hover::TabClose(i) {
                p.rounded(Layer::UiShapes, cl.x, cl.y, cl.w, cl.h, m.scale * 3.0, pal.ui_dim.rgba(0.35));
            }
            p.icon(Layer::UiText, '×', cl.x + cl.w / 2.0, cl.y + cl.h / 2.0, pal.ui_dim.rgba(1.0));
        } else if activity && bell_fits {
            draw_bell(p, &slot.close, pal.ui_accent);
        }
    }

    for (rect, hover, is_menu) in [(bar.plus, Hover::Plus, false), (bar.menu, Hover::MenuButton, true)] {
        let lit = c.hover == hover || (is_menu && c.menu_open);
        if lit {
            let inset = m.scale * 5.0;
            p.rounded(Layer::UiShapes, rect.x + inset, rect.y + inset, rect.w - 2.0 * inset, rect.h - 2.0 * inset, r, pal.ui_dim.rgba(0.3));
        }
        let color = if lit { pal.foreground } else { pal.ui_dim };
        if is_menu {
            // Vertical three dots.
            let d = (m.scale * 3.5).round().max(2.0);
            let cx = rect.x + rect.w / 2.0;
            let cy = rect.y + rect.h / 2.0;
            for k in [-1.0, 0.0, 1.0] {
                let y = cy + k * d * 2.0;
                p.rounded(Layer::UiShapes, cx - d / 2.0, y - d / 2.0, d, d, d / 2.0, color.rgba(1.0));
            }
        } else {
            p.icon(Layer::UiText, '+', rect.x + rect.w / 2.0, rect.y + rect.h / 2.0, color.rgba(1.0));
        }
    }

    draw_window_controls(p, pal, m, &bar, c);
}

/// A bell centred in `r`, built from shapes because the font may not have one.
fn draw_bell(p: &mut Painter<'_>, r: &ui::Rect, color: Rgb) {
    let s = (r.w.min(r.h) * 0.8).round();
    let (x, y) = ((r.x + (r.w - s) / 2.0).round(), (r.y + (r.h - s) / 2.0).round());
    let c = color.rgba(1.0);
    let at = |f: f32| (s * f).round();
    // The dome, then straight sides down to the brim.
    let (body_x, body_w) = (x + at(0.16), s - 2.0 * at(0.16));
    let brim_y = y + at(0.68);
    p.rounded(Layer::UiShapes, body_x, y + at(0.06), body_w, at(0.5), body_w / 2.0, c);
    p.rect(Layer::UiShapes, body_x, y + at(0.32), body_w, brim_y - y - at(0.32), c);
    let brim_h = at(0.14).max(1.0);
    p.rounded(Layer::UiShapes, x, brim_y, s, brim_h, brim_h / 2.0, c);
    // The clapper, just clear of the brim.
    let d = at(0.2).max(2.0);
    p.rounded(Layer::UiShapes, x + ((s - d) / 2.0).round(), y + s - d, d, d, d / 2.0, c);
}

/// A tab's title for `room` characters: "Tab 3: vim", then "3: vim" (cut
/// with …), then just "3", so narrow tabs still tell each other apart.
fn fit_tab_title(index: usize, title: &str, room: usize) -> String {
    if title.chars().count() <= room {
        return title.to_owned();
    }
    let number = (index + 1).to_string();
    let name = title.strip_prefix(&format!("Tab {number}: ")).unwrap_or(title);
    let short = format!("{number}: {name}");
    let len = short.chars().count();
    if len <= room {
        return short;
    }
    // The number, ": ", at least one letter and "…".
    if room >= number.len() + 4 {
        return short.chars().take(room - 1).chain(std::iter::once('…')).collect();
    }
    number
}

/// Minimize, maximize/restore and close, for windows without an OS frame.
/// Drawn like Windows 11's caption buttons: 10px icons with 1px lines, a
/// faint hover, red behind close.
fn draw_window_controls(p: &mut Painter<'_>, pal: &Palette, m: &Metrics, bar: &chrome::TabBar, c: &ChromeView<'_>) {
    let (Some(min), Some(max), Some(close)) = (bar.minimize, bar.maximize, bar.close) else { return };
    let bar_bg = tab_bar_background(pal);
    let hover_bg = bar_bg.mix(pal.foreground, if pal.dark { 0.09 } else { 0.06 });
    let t = m.scale.round().max(1.0);
    let s = (10.0 * m.scale).round();
    let center = |r: &ui::Rect| ((r.x + (r.w - s) / 2.0).round(), (r.y + (r.h - s) / 2.0).round());
    let bg = |h: Hover| if c.hover == h { hover_bg } else { bar_bg };
    for (rect, hover) in [(min, Hover::Minimize), (max, Hover::Maximize), (close, Hover::CloseWindow)] {
        if c.hover == hover {
            let color = if hover == Hover::CloseWindow { Rgb::hex(0xc42b1c) } else { hover_bg };
            p.rect(Layer::UiShapes, rect.x, rect.y, rect.w, rect.h, color.rgba(1.0));
        }
    }
    let fg = pal.foreground;

    let (x, y) = center(&min);
    p.rect(Layer::UiShapes, x, y + (s / 2.0).round(), s, t, fg.rgba(1.0));

    // Outlined rounded squares: the line colour, then the background inside.
    let (x, y) = center(&max);
    let r = m.scale * 1.5;
    let square = |p: &mut Painter<'_>, x: f32, y: f32, w: f32, inside: Rgb| {
        p.rounded(Layer::UiShapes, x, y, w, w, r, fg.rgba(1.0));
        p.rounded(Layer::UiShapes, x + t, y + t, w - 2.0 * t, w - 2.0 * t, (r - t).max(0.0), inside.rgba(1.0));
    };
    if c.maximized {
        // Restore: a square with a second one peeking out behind it.
        let o = (2.0 * m.scale).round();
        let w = s - o;
        square(p, x + o, y, w, bg(Hover::Maximize));
        // Clear the back square where the front one covers it.
        p.rect(Layer::UiShapes, x - t, y + o - t, w + 2.0 * t, w + 2.0 * t, bg(Hover::Maximize).rgba(1.0));
        square(p, x, y + o, w, bg(Hover::Maximize));
    } else {
        square(p, x, y, s, bg(Hover::Maximize));
    }

    let (x, y) = center(&close);
    let white = c.hover == Hover::CloseWindow;
    p.cross(Layer::UiShapes, x, y, s, t, if white { Rgb::hex(0xffffff) } else { fg }.rgba(1.0));
}

fn tab_bar_background(pal: &Palette) -> Rgb {
    if pal.dark { pal.background.mix(Rgb::hex(0x000000), 0.25) } else { pal.ui_bg }
}

fn draw_menu(p: &mut Painter<'_>, pal: &Palette, m: &Metrics, menu: &Menu) {
    let l = menu.layout(m);
    let r = m.scale * 6.0;
    let b = m.scale.round().max(1.0);
    let shadow = m.scale * 3.0;
    p.rounded(Layer::OverlayShapes, l.rect.x + shadow, l.rect.y + shadow, l.rect.w, l.rect.h, r, [0.0, 0.0, 0.0, 0.25]);
    p.rounded(Layer::OverlayShapes, l.rect.x - b, l.rect.y - b, l.rect.w + 2.0 * b, l.rect.h + 2.0 * b, r + b, pal.ui_dim.rgba(0.6));
    p.rounded(Layer::OverlayShapes, l.rect.x, l.rect.y, l.rect.w, l.rect.h, r, pal.ui_bg.rgba(1.0));
    for (i, (item, row)) in menu.items.iter().zip(&l.rows).enumerate() {
        if item.is_separator() {
            let y = (row.y + row.h / 2.0).round();
            p.rect(Layer::OverlayShapes, row.x + l.pad, y, row.w - 2.0 * l.pad, b, pal.ui_dim.rgba(0.4));
            continue;
        }
        if menu.hover == Some(i) && item.enabled {
            p.rounded(Layer::OverlayShapes, row.x, row.y, row.w, row.h, m.scale * 4.0, pal.ui_accent.rgba(0.85));
        }
        let fg = if !item.enabled {
            pal.ui_dim.mix(pal.ui_bg, 0.3)
        } else if menu.hover == Some(i) {
            Rgb::hex(0xffffff)
        } else {
            pal.ui_fg
        };
        let y = (row.y + (row.h - m.ch) / 2.0).round();
        let x = l.rect.x + l.pad;
        p.text(Layer::OverlayText, &item.label, x + m.cw, y, fg.rgba(1.0));
        if !item.hint.is_empty() {
            let hint_fg = if menu.hover == Some(i) { fg } else { pal.ui_dim };
            p.text(Layer::OverlayText, &item.hint, l.hint_x, y, hint_fg.rgba(1.0));
        }
    }
}

fn draw_panel(p: &mut Painter<'_>, pal: &Palette, m: &Metrics, panel: &Panel, hover: Hover) {
    p.rect(Layer::OverlayShapes, 0.0, 0.0, m.width, m.height, [0.0, 0.0, 0.0, 0.35]);
    let rc = panel.rect;
    let r = m.scale * 10.0;
    let b = m.scale.round().max(1.0);
    p.rounded(Layer::OverlayShapes, rc.x - b, rc.y - b, rc.w + 2.0 * b, rc.h + 2.0 * b, r + b, pal.ui_dim.rgba(0.6));
    p.rounded(Layer::OverlayShapes, rc.x, rc.y, rc.w, rc.h, r, pal.ui_bg.rgba(1.0));
    for rule in &panel.rules {
        p.rect(Layer::OverlayShapes, rule.x, rule.y, rule.w, rule.h, pal.ui_dim.rgba(0.35));
    }

    for line in &panel.lines {
        let color = match line.style {
            TextStyle::Title => pal.foreground,
            TextStyle::Normal => pal.ui_fg,
            TextStyle::Dim => pal.ui_dim,
            TextStyle::Key => pal.ansi[3],
        };
        // Clip text that would run past the panel's right edge.
        let room = ((rc.x + rc.w - line.x) / m.cw).floor().max(0.0) as usize;
        let text: String = line.text.chars().take(room.saturating_sub(1)).collect();
        p.text(Layer::OverlayText, &text, line.x, line.y, color.rgba(1.0));
        if line.style == TextStyle::Title {
            // Fake bold by drawing twice, one pixel apart.
            p.text(Layer::OverlayText, &text, line.x + b, line.y, color.rgba(1.0));
        }
    }

    for (i, btn) in panel.buttons.iter().enumerate() {
        let br = btn.rect;
        let hovered = hover == Hover::PanelButton(i);
        if btn.style == ButtonStyle::Tab {
            // Text only; the active page is underlined in the accent colour.
            let fg = if btn.active || hovered { pal.foreground } else { pal.ui_dim };
            let tw = btn.label.chars().count() as f32 * m.cw;
            let tx = (br.x + (br.w - tw) / 2.0).round();
            p.text(Layer::OverlayText, &btn.label, tx, (br.y + (br.h - m.ch) / 2.0).round(), fg.rgba(1.0));
            if btn.active {
                let t = (2.0 * m.scale).round().max(2.0);
                p.rounded(Layer::OverlayShapes, br.x + m.cw / 2.0, br.y + br.h - t, br.w - m.cw, t, t / 2.0, pal.ui_accent.rgba(1.0));
            }
            continue;
        }
        if btn.style == ButtonStyle::Icon {
            if hovered {
                p.rounded(Layer::OverlayShapes, br.x, br.y, br.w, br.h, m.scale * 5.0, pal.ui_dim.mix(pal.ui_bg, 0.5).rgba(1.0));
            }
            let fg = if hovered {
                pal.foreground
            } else if btn.active {
                pal.ui_accent
            } else {
                pal.ui_dim.mix(pal.ui_bg, 0.4)
            };
            let c = btn.label.chars().next().unwrap_or(' ');
            p.icon(Layer::OverlayText, c, br.x + br.w / 2.0, br.y + br.h / 2.0, fg.rgba(1.0));
            continue;
        }
        let (bg, fg) = if btn.active {
            (pal.ui_accent, Rgb::hex(0xffffff))
        } else if hovered {
            (pal.ui_dim.mix(pal.ui_bg, 0.5), pal.foreground)
        } else if btn.style == ButtonStyle::Close {
            (pal.ui_bg, pal.ui_dim)
        } else {
            (pal.ui_dim.mix(pal.ui_bg, 0.75), pal.ui_fg)
        };
        p.rounded(Layer::OverlayShapes, br.x, br.y, br.w, br.h, m.scale * 5.0, bg.rgba(1.0));
        if btn.style == ButtonStyle::Close {
            p.icon(Layer::OverlayText, '×', br.x + br.w / 2.0, br.y + br.h / 2.0, fg.rgba(1.0));
            continue;
        }
        let tw = btn.label.chars().count() as f32 * m.cw;
        let tx = (br.x + (br.w - tw) / 2.0).round();
        let ty = (br.y + (br.h - m.ch) / 2.0).round();
        p.text(Layer::OverlayText, &btn.label, tx, ty, fg.rgba(1.0));
    }
}

/// Bookmarked rows: a tint across the window over the cell backgrounds and
/// under the text, and a bar at the left edge. They show with line numbers
/// off and on lines that have no number.
fn draw_bookmarks(p: &mut Painter<'_>, v: &View<'_>) {
    let accent = v.palette.ui_accent;
    let bar = (3.0 * v.scale).round();
    let (w, ch) = (p.width, p.cell.height);
    for &row in v.bookmarks {
        let y = v.top + v.padding + row as f32 * ch;
        p.rect(Layer::Background, 0.0, y, w, ch, accent.rgba(0.18));
        p.rect(Layer::Background, 0.0, y, bar, ch, accent.rgba(1.0));
    }
}

/// Line numbers, right-aligned in the gutter left of the text.
fn draw_line_numbers(p: &mut Painter<'_>, v: &View<'_>) {
    if v.gutter <= 0.0 {
        return;
    }
    let dim = v.palette.ui_dim.rgba(1.0);
    let accent = v.palette.ui_accent.rgba(1.0);
    let mut buf = String::new();
    for (row, n) in v.line_numbers.iter().enumerate() {
        let Some(n) = n else { continue };
        if v.line_number_mode == LineNumbers::Command {
            crate::lines::format_time(*n, &mut buf);
        } else {
            crate::lines::format_number(*n, &mut buf);
        }
        let color = if v.bookmarks.binary_search(&row).is_ok() { accent } else { dim };
        p.text(Layer::Text, &buf, v.padding, v.top + v.padding + row as f32 * p.cell.height, color);
    }
}

fn draw_grid<T: EventListener>(p: &mut Painter<'_>, term: &Term<T>, v: &View<'_>) {
    let pal = v.palette;
    let content = term.renderable_content();
    let (cw, ch) = (p.cell.width, p.cell.height);
    let pad = v.padding;
    let offset = content.display_offset as i32;
    let hist = term.grid().history_size() as i64;
    let code_on = !content.mode.contains(TermMode::ALT_SCREEN);
    let cursor = content.cursor;
    let selection = content.selection;
    // Each search box: its on-screen matches, a cursor into them, its colours.
    let mut boxes: Vec<(&[alacritty_terminal::term::search::Match], usize, MatchColors)> =
        v.search.boxes.iter().map(|b| (b.visible.as_slice(), 0, pal.search_colors(b.color))).collect();
    let active = v.search.active;
    let current = v.search.active_box().current.as_ref();
    let thin = (1.0 * v.scale).round().max(1.0);

    // Last (fg, bg) -> readable fg: neighbouring cells share colours.
    let mut readable: Option<(Rgb, Rgb, Rgb)> = None;
    let mut span_line = i32::MIN;
    let mut spans: &[Span] = &[];

    for indexed in content.display_iter {
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let point = indexed.point;
        let row = (point.line.0 + offset) as f32;
        let x = pad + v.gutter + point.column.0 as f32 * cw;
        let y = v.top + pad + row * ch;

        let mut fg_color = cell.fg;
        if v.bold_is_bright && cell.flags.contains(Flags::BOLD)
            && let Color::Named(n) = fg_color
                && (n as usize) < 8 {
                    fg_color = Color::Indexed(n as u8 + 8);
                }
        let mut fg = resolve(fg_color, content.colors, pal);
        let mut bg = resolve(cell.bg, content.colors, pal);

        if code_on && cell.fg == Color::Named(NamedColor::Foreground) && !cell.flags.contains(Flags::INVERSE) {
            if point.line.0 != span_line {
                span_line = point.line.0;
                spans = v.code.row(hist + point.line.0 as i64);
            }
            if let Some(c) = span_at(spans, point.column.0 as u16) {
                fg = c;
            }
        }
        if cell.flags.contains(Flags::DIM) {
            fg = fg.mix(bg, 0.4);
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }

        if selection.is_some_and(|s| s.contains_cell(&indexed, cursor.point, cursor.shape)) {
            bg = pal.selection_bg;
        }

        // The focused box's current match comes first, then the first box
        // (top to bottom) with a match here.
        let mut mark: Option<(Rgb, Rgb)> = None;
        for (i, (matches, mi, colors)) in boxes.iter_mut().enumerate() {
            while *mi < matches.len() && *matches[*mi].end() < point {
                *mi += 1;
            }
            let Some(m) = matches.get(*mi).filter(|m| *m.start() <= point) else { continue };
            if i == active && current.is_some_and(|c| c == m) {
                mark = Some((colors.current_bg, colors.current_fg));
            } else if mark.is_none() {
                mark = Some((colors.bg, colors.fg));
            }
        }
        if let Some((b, f)) = mark {
            bg = b;
            fg = f;
        }

        let is_cursor = point == cursor.point && cursor.shape != CursorShape::Hidden;
        if is_cursor && v.focused && cursor.shape == CursorShape::Block {
            bg = pal.cursor;
            fg = pal.background;
        }

        // Programs pick colour pairs the palette can make unreadable, e.g.
        // `ls` showing world-writable directories blue on green.
        if bg != pal.background && v.min_contrast > 1.0 {
            fg = match readable {
                Some((f, b, r)) if f == fg && b == bg => r,
                _ => {
                    let r = fg.readable_on(bg, v.min_contrast);
                    readable = Some((fg, bg, r));
                    r
                }
            };
        }

        let w = if cell.flags.contains(Flags::WIDE_CHAR) { cw * 2.0 } else { cw };
        if bg != pal.background {
            p.rect(Layer::Background, x, y, w, ch, bg.rgba(1.0));
        }

        if cell.c != ' ' && !cell.flags.contains(Flags::HIDDEN) {
            let style = GlyphStyle {
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
            };
            let color = fg.rgba(1.0);
            p.glyph(Layer::Text, cell.c, style, x, y, color);
            if let Some(zw) = cell.zerowidth() {
                for &c in zw {
                    p.glyph(Layer::Text, c, style, x, y, color);
                }
            }
        }

        if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            let uy = y + p.cell.baseline + thin.max(p.cell.underline_thickness);
            p.rect(Layer::Text, x, uy, w, p.cell.underline_thickness, fg.rgba(1.0));
            if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
                p.rect(Layer::Text, x, uy + 2.0 * thin, w, p.cell.underline_thickness, fg.rgba(1.0));
            }
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            p.rect(Layer::Text, x, y + (ch / 2.0).round(), w, p.cell.underline_thickness, fg.rgba(1.0));
        }

        if is_cursor && !(v.focused && cursor.shape == CursorShape::Block) {
            let c = pal.cursor.rgba(1.0);
            let shape = if v.focused { cursor.shape } else { CursorShape::HollowBlock };
            match shape {
                CursorShape::Beam => p.rect(Layer::Text, x, y, (2.0 * v.scale).round(), ch, c),
                CursorShape::Underline => p.rect(Layer::Text, x, y + ch - 2.0 * thin, w, 2.0 * thin, c),
                _ => {
                    p.rect(Layer::Text, x, y, w, thin, c);
                    p.rect(Layer::Text, x, y + ch - thin, w, thin, c);
                    p.rect(Layer::Text, x, y, thin, ch, c);
                    p.rect(Layer::Text, x + w - thin, y, thin, ch, c);
                }
            }
        }
    }
}

fn draw_scrollbar<T>(p: &mut Painter<'_>, term: &Term<T>, v: &View<'_>) {
    let grid = term.grid();
    let (hist, offset) = v.scroll.unwrap_or((grid.history_size(), grid.display_offset()));
    let Some(g) = ui::scroll_geometry(p.width, v.top, p.height, v.scale, hist, grid.screen_lines(), offset) else {
        return;
    };
    let a = v.scrollbar_alpha;
    if a <= 0.01 {
        return;
    }
    let pal = v.palette;
    let track = pal.ui_bg.rgba(0.6 * a);
    p.rounded(Layer::UiShapes, g.track.x + g.track.w * 0.25, g.track.y, g.track.w * 0.5, g.track.h, g.track.w * 0.25, track);
    let thumb = pal.ui_dim.rgba(a);
    p.rounded(Layer::UiShapes, g.thumb.x, g.thumb.y, g.thumb.w, g.thumb.h, g.thumb.w * 0.5, thumb);
}

fn draw_search_bar(p: &mut Painter<'_>, v: &View<'_>, hover: Hover) {
    let s = v.search;
    let (cw, ch) = (p.cell.width, p.cell.height);
    let g = ui::search_geometry(p.width, v.top, v.scale, cw, ch, s.boxes.len(), s.picker);
    for (i, b) in s.boxes.iter().enumerate() {
        draw_search_box(p, v, &g, i, b, hover);
    }
}

fn draw_search_box(p: &mut Painter<'_>, v: &View<'_>, g: &ui::SearchGeometry, i: usize, s: &crate::search::SearchBox, hover: Hover) {
    use ui::SearchHit as H;
    let pal = v.palette;
    let a = v.search_alpha;
    let (cw, ch) = (p.cell.width, p.cell.height);
    let bg = &g.boxes[i];
    let b = bg.bar;
    let r = (6.0 * v.scale).round();
    let small_r = (4.0 * v.scale).round();
    let border = (1.0 * v.scale).round().max(1.0);
    let focused = v.search.focused && v.search.active == i;

    let edge = if s.invalid {
        pal.ansi[1]
    } else if focused {
        pal.ui_accent
    } else {
        pal.ui_dim
    };
    p.rounded(Layer::UiShapes, b.x - border, b.y - border, b.w + 2.0 * border, b.h + 2.0 * border, r + border, edge.rgba(a));
    p.rounded(Layer::UiShapes, b.x, b.y, b.w, b.h, r, pal.ui_bg.rgba(a));

    let pad = (6.0 * v.scale).round();
    let ty = b.y + pad;

    // Colour button: a dot in this box's highlight colour.
    let colors = pal.search_colors(s.color);
    let cr = bg.color;
    let picking = g.picker == Some(i);
    if hover == Hover::Search(H::Color(i)) || picking {
        p.rounded(Layer::UiShapes, cr.x, cr.y, cr.w, cr.h, small_r, pal.ui_dim.rgba(0.3 * a));
    }
    let dot = (ch * 0.6).round();
    p.rounded(Layer::UiShapes, cr.x + (cr.w - dot) / 2.0, cr.y + (cr.h - dot) / 2.0, dot, dot, dot / 2.0, colors.current_bg.rgba(a));

    // Status text, right-aligned before ↑.
    let status = if s.query.is_empty() {
        String::new()
    } else if s.invalid {
        "bad regex".to_owned()
    } else if s.total == 0 {
        "no results".to_owned()
    } else {
        let plus = if s.total_capped() { "+" } else { "" };
        match s.index {
            Some(i) => format!("{i}/{}{plus}", s.total),
            None => format!("{}{plus}", s.total),
        }
    };
    let status_w = status.chars().count() as f32 * cw;
    let status_x = bg.prev.x - status_w - cw;

    if picking {
        // Colour choices in place of the query; the chosen one is ringed.
        for (c, sw) in g.swatches.iter().enumerate() {
            if c == s.color || hover == Hover::Search(H::Swatch(i, c)) {
                let ring = if c == s.color { pal.ui_fg } else { pal.ui_dim };
                p.rounded(Layer::UiShapes, sw.x, sw.y, sw.w, sw.h, small_r, ring.rgba(a));
                p.rounded(Layer::UiShapes, sw.x + border, sw.y + border, sw.w - 2.0 * border, sw.h - 2.0 * border, small_r - border, pal.ui_bg.rgba(a));
            }
            let d = sw.h - 6.0 * border;
            let col = pal.search_colors(c).current_bg;
            p.rounded(Layer::UiShapes, sw.x + (sw.w - d) / 2.0, sw.y + (sw.h - d) / 2.0, d, d, small_r - border, col.rgba(a));
        }
    } else {
        let mut x = cr.x + cr.w + pad / 2.0;
        // Mode badge: ".*" when regex mode is on, "Aa" otherwise.
        let badge = if s.regex_mode { ".*" } else { "Aa" };
        let badge_color = if s.regex_mode { pal.ui_accent } else { pal.ui_dim };
        x = p.text(Layer::UiText, badge, x, ty, badge_color.rgba(a)) + cw;

        // Query, scrolled to keep its end visible.
        let room = ((status_x - x) / cw).floor().max(1.0) as usize - 1;
        let chars = s.query.chars().count();
        let skip = chars.saturating_sub(room);
        let shown: String = s.query.chars().skip(skip).collect();
        let end = if s.query.is_empty() && !focused {
            p.text(Layer::UiText, "search", x, ty, pal.ui_dim.rgba(a))
        } else {
            p.text(Layer::UiText, &shown, x, ty, pal.ui_fg.rgba(a))
        };
        if focused {
            p.rect(Layer::UiText, end + 1.0, ty, (2.0 * v.scale).round(), ch, pal.ui_accent.rgba(a));
        }
        p.text(Layer::UiText, &status, status_x, ty, pal.ui_dim.rgba(a));
    }

    // ↑ previous, ↓ next (dimmed with nothing to jump to), + another
    // search, × remove this one.
    let has_matches = s.total > 0;
    let can_add = v.search.boxes.len() < MAX_BOXES;
    for (rect, glyph, h, enabled) in [
        (bg.prev, '↑', H::Prev(i), has_matches),
        (bg.next, '↓', H::Next(i), has_matches),
        (bg.add, '+', H::Add(i), can_add),
        (bg.close, '×', H::Close(i), true),
    ] {
        let hovered = hover == Hover::Search(h);
        if hovered && enabled {
            p.rounded(Layer::UiShapes, rect.x, rect.y, rect.w, rect.h, small_r, pal.ui_dim.rgba(0.3 * a));
        }
        let color = if !enabled {
            pal.ui_dim.mix(pal.ui_bg, 0.5)
        } else if hovered {
            pal.foreground
        } else {
            pal.ui_dim
        };
        p.icon(Layer::UiText, glyph, rect.x + rect.w / 2.0, rect.y + rect.h / 2.0, color.rgba(a));
    }
}

#[cfg(test)]
mod tests {
    use crate::shortcuts::Keymap;
    use super::*;
    use crate::chrome::{OptionsState, about_panel, options_panel};
    use crate::config::ThemeMode;
    use crate::render::testing::Canvas;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

    /// A 40×6 terminal that has parsed `bytes`, like the PTY thread does.
    fn term(bytes: &str) -> Term<VoidListener> {
        let mut t = Term::new(TermConfig::default(), &TermSize::new(40, 6), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut t, bytes.as_bytes());
        t
    }

    struct Fixture {
        palette: Palette,
        search: Search,
        code: CodeHighlighter,
    }

    impl Fixture {
        fn new() -> Self {
            Self { palette: Palette::dark(), search: Search::new(), code: CodeHighlighter::new(true, "base16-ocean.dark") }
        }

        fn view(&self) -> View<'_> {
            View {
                palette: &self.palette,
                search: &self.search,
                code: &self.code,
                focused: true,
                bold_is_bright: false,
                scale: 1.0,
                padding: 0.0,
                top: 30.0,
                scrollbar_alpha: 1.0,
                search_alpha: 1.0,
                min_contrast: 4.5,
                gutter: 0.0,
                line_numbers: &[],
                line_number_mode: LineNumbers::Tab,
                bookmarks: &[],
                scroll: None,
            }
        }
    }

    fn chrome<'a>(titles: Vec<&'a str>) -> ChromeView<'a> {
        ChromeView {
            caption: Caption::NONE,
            maximized: false,
            titles,
            active: 0,
            hover: Hover::None,
            menu_open: false,
            menu: None,
            panel: None,
            logging: Vec::new(),
            activity: Vec::new(),
            toast: None,
        }
    }

    fn glyph_colors(c: &Canvas) -> Vec<[f32; 4]> {
        c.layer(Layer::Text).iter().filter(|i| i.kind != 0).map(|i| i.color).collect()
    }

    #[test]
    fn named_and_indexed_colours() {
        let p = Palette::dark();
        let colors = Colors::default();
        assert_eq!(resolve(Color::Named(NamedColor::Red), &colors, &p), p.ansi[1]);
        assert_eq!(resolve(Color::Named(NamedColor::Foreground), &colors, &p), p.foreground);
        assert_eq!(resolve(Color::Indexed(196), &colors, &p), Rgb::hex(0xff0000));
        let spec = alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 };
        assert_eq!(resolve(Color::Spec(spec), &colors, &p), Rgb { r: 1, g: 2, b: 3 });
        // Dim colours sit between the colour and the background.
        let dim = named(NamedColor::DimRed, &p);
        assert_ne!(dim, p.ansi[1]);
        assert_ne!(dim, p.background);
    }

    #[test]
    fn programs_can_override_palette_entries() {
        let mut t = term("\x1b]4;1;rgb:12/34/56\x07");
        let p = Palette::dark();
        assert_eq!(color_for_index(&t, 1, &p), Rgb { r: 0x12, g: 0x34, b: 0x56 });
        assert_eq!(color_for_index(&t, 2, &p), p.ansi[2]);
        assert_eq!(color_for_index(&t, 256, &p), p.foreground);
        assert_eq!(color_for_index(&t, 257, &p), p.background);
        let _ = &mut t;
    }

    #[test]
    fn span_lookup() {
        let red = Rgb::hex(0xff0000);
        let blue = Rgb::hex(0x0000ff);
        let spans = [Span { start: 2, end: 4, color: red }, Span { start: 6, end: 7, color: blue }];
        assert_eq!(span_at(&spans, 1), None);
        assert_eq!(span_at(&spans, 2), Some(red));
        assert_eq!(span_at(&spans, 3), Some(red));
        assert_eq!(span_at(&spans, 4), None);
        assert_eq!(span_at(&spans, 6), Some(blue));
        assert_eq!(span_at(&[], 0), None);
    }

    #[test]
    fn draws_one_glyph_per_visible_character_and_a_block_cursor() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("ab c\r\nxyz");
        let f = Fixture::new();
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec!["Tab 1: sh"])));
        assert_eq!(c.glyph_count(Layer::Text), 6);
        // Block cursor after "xyz": one cursor-coloured cell background.
        let cursor_bg = f.palette.cursor.rgba(1.0);
        assert_eq!(c.layer(Layer::Background).iter().filter(|i| i.color == cursor_bg).count(), 1);
        // Grid starts below the tab bar.
        let first = c.layer(Layer::Text).iter().map(|i| i.pos[1]).fold(f32::MAX, f32::min);
        assert!(first >= 30.0);
    }

    #[test]
    fn line_numbers_sit_in_a_gutter_left_of_the_text() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("ab\r\ncd");
        let f = Fixture::new();
        let numbers = [Some(1), Some(12)];
        let gutter = 60.0;
        let v = View { gutter, line_numbers: &numbers, ..f.view() };
        c.paint(400.0, 300.0, |p| draw(p, &t, &v, &chrome(vec![])));
        let dim = f.palette.ui_dim.rgba(1.0);
        let glyphs = || c.layer(Layer::Text).iter().filter(|i| i.kind != 0);
        let nums: Vec<_> = glyphs().filter(|i| i.color == dim).collect();
        let text: Vec<_> = glyphs().filter(|i| i.color != dim).collect();
        assert_eq!(nums.len(), 3, "1 and 12");
        assert_eq!(text.len(), 4, "ab and cd");
        assert!(nums.iter().all(|i| i.pos[0] < gutter), "numbers in the gutter");
        assert!(text.iter().all(|i| i.pos[0] >= gutter), "text right of it");
        // Without a gutter nothing is numbered and the text starts at the edge.
        c.paint(400.0, 300.0, |p| draw(p, &t, &View { line_numbers: &numbers, ..f.view() }, &chrome(vec![])));
        assert_eq!(c.glyph_count(Layer::Text), 4);
    }

    #[test]
    fn times_are_drawn_in_the_gutter_with_hours_minutes_and_seconds() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("ab\r\ncd");
        let f = Fixture::new();
        let numbers = [Some(33272), Some(5)];
        let v = View { gutter: 60.0, line_numbers: &numbers, line_number_mode: LineNumbers::Command, ..f.view() };
        c.paint(400.0, 300.0, |p| draw(p, &t, &v, &chrome(vec![])));
        let dim = f.palette.ui_dim.rgba(1.0);
        let shown = c.layer(Layer::Text).iter().filter(|i| i.kind != 0 && i.color == dim).count();
        assert_eq!(shown, 16, "09:14:32 and 00:00:05");
    }

    #[test]
    fn bookmarked_rows_are_marked_across_the_window() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("ab\r\ncd");
        let f = Fixture::new();
        let v = View { padding: 6.0, bookmarks: &[1], ..f.view() };
        let mut ch = 0.0;
        c.paint(400.0, 300.0, |p| {
            ch = p.cell.height;
            draw(p, &t, &v, &chrome(vec![]));
        });
        let accent = f.palette.ui_accent;
        let (r, g, b) = (accent.rgba(1.0)[0], accent.rgba(1.0)[1], accent.rgba(1.0)[2]);
        let marks: Vec<_> = c.layer(Layer::Background).iter().filter(|i| i.color[..3] == [r, g, b]).collect();
        assert!(!marks.is_empty(), "something drawn in the accent colour");
        // Second row only: below the tab bar (30), the padding and row 0.
        assert!(marks.iter().all(|i| i.pos[1] == 30.0 + 6.0 + ch), "{:?}", marks.iter().map(|i| i.pos).collect::<Vec<_>>());
        assert!(marks.iter().any(|i| i.pos[0] == 0.0 && i.size[0] >= 400.0), "a tint across the row");
        assert!(marks.iter().any(|i| i.pos[0] == 0.0 && i.size[0] < 6.0 && i.color[3] == 1.0), "a solid bar in the padding");
    }

    #[test]
    fn a_bookmarked_rows_number_is_in_the_accent_colour() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("ab\r\ncd");
        let f = Fixture::new();
        let numbers = [Some(1), Some(2)];
        let v = View { gutter: 60.0, line_numbers: &numbers, bookmarks: &[1], ..f.view() };
        c.paint(400.0, 300.0, |p| draw(p, &t, &v, &chrome(vec![])));
        let colors: Vec<_> = c.layer(Layer::Text).iter().filter(|i| i.kind != 0 && i.pos[0] < 60.0).map(|i| i.color).collect();
        assert_eq!(colors, [f.palette.ui_dim.rgba(1.0), f.palette.ui_accent.rgba(1.0)]);
    }

    #[test]
    fn logging_dot_and_toast() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("");
        let f = Fixture::new();
        let red = f.palette.ansi[1].rgba(1.0);
        let reds = |c: &Canvas| c.layer(Layer::UiShapes).iter().filter(|i| i.color == red).count();
        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec!["Tab 1: sh", "Tab 2: sh"])));
        assert_eq!(reds(&c), 0);
        let logged = ChromeView { logging: vec![false, true], ..chrome(vec!["Tab 1: sh", "Tab 2: sh"]) };
        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &logged));
        assert_eq!(reds(&c), 1, "one dot, on the logged tab");

        let toast = ChromeView { toast: Some("Saved to ~/x.txt"), ..chrome(vec![]) };
        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &toast));
        assert_eq!(c.glyph_count(Layer::OverlayText), "Savedto~/x.txt".len());
        // A long message is cut to the window.
        let long = "x".repeat(500);
        let toast = ChromeView { toast: Some(&long), ..chrome(vec![]) };
        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &toast));
        assert!(c.layer(Layer::OverlayShapes).iter().all(|i| i.pos[0] >= 0.0 && i.pos[0] < 600.0));
    }

    #[test]
    fn bell_marks_background_tabs_with_activity() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("");
        let f = Fixture::new();
        let accent = f.palette.ui_accent.rgba(1.0);
        let shapes = |c: &Canvas| c.layer(Layer::UiShapes).iter().filter(|i| i.color == accent).count();
        let titles = || vec!["Tab 1: sh", "Tab 2: sh"];

        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(titles())));
        assert_eq!(shapes(&c), 0, "no activity, no bell");

        // The bell is drawn on the unselected tab, and only there.
        let quiet_active = ChromeView { activity: vec![true, false], ..chrome(titles()) };
        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &quiet_active));
        assert_eq!(shapes(&c), 0, "the selected tab never shows one");
        let background = ChromeView { activity: vec![false, true], ..chrome(titles()) };
        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &background));
        assert!(shapes(&c) > 0, "the background tab shows one");

        // Hovering swaps it for the close button.
        let hovered = ChromeView { activity: vec![false, true], hover: Hover::Tab(1), ..chrome(titles()) };
        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &hovered));
        assert_eq!(shapes(&c), 0);
    }

    #[test]
    fn narrow_tab_with_activity_colours_its_number() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("");
        let f = Fixture::new();
        let accent = f.palette.ui_accent.rgba(1.0);
        let titles: Vec<&str> = vec!["Tab 1: sh"; 30];
        let mut activity = vec![false; 30];
        activity[1] = true;
        let v = ChromeView { activity, ..chrome(titles) };
        c.paint(600.0, 300.0, |p| draw(p, &t, &f.view(), &v));
        assert_eq!(c.layer(Layer::UiShapes).iter().filter(|i| i.color == accent).count(), 0);
        assert_eq!(c.layer(Layer::UiText).iter().filter(|i| i.kind != 0 && i.color == accent).count(), 1, "just the number");
    }

    #[test]
    fn sgr_colours_bold_and_underline() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("\x1b[31mR\x1b[0m \x1b[4mU\x1b[0m \x1b[7mI\x1b[0m");
        let f = Fixture::new();
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec![])));
        let colors = glyph_colors(&c);
        assert!(colors.contains(&f.palette.ansi[1].rgba(1.0)), "red R");
        // Underline is a thin rect in the text layer.
        assert_eq!(c.layer(Layer::Text).iter().filter(|i| i.kind == 0).count(), 1);
        // Inverse: background takes the foreground colour.
        let inv_bg = f.palette.foreground.rgba(1.0);
        assert!(c.layer(Layer::Background).iter().any(|i| i.color == inv_bg));
    }

    #[test]
    fn narrow_tabs_keep_their_numbers() {
        assert_eq!(fit_tab_title(2, "Tab 3: powershell", 40), "Tab 3: powershell");
        assert_eq!(fit_tab_title(2, "Tab 3: powershell", 13), "3: powershell");
        assert_eq!(fit_tab_title(2, "Tab 3: powershell", 7), "3: pow…");
        assert_eq!(fit_tab_title(2, "Tab 3: powershell", 5), "3: p…");
        assert_eq!(fit_tab_title(2, "Tab 3: powershell", 4), "3");
        assert_eq!(fit_tab_title(11, "Tab 12: vim", 2), "12");
        assert_eq!(fit_tab_title(11, "Tab 12: vim", 0), "12", "the number shows even without room");
        // A title set by `fastcnsl new-tab NAME` works the same way.
        assert_eq!(fit_tab_title(0, "Tab 1: logs", 8), "1: logs");
    }

    #[test]
    fn text_on_a_clashing_background_is_made_readable() {
        let Some(mut c) = Canvas::new() else { return };
        // How `ls` shows world-writable directories (e.g. on /mnt/c).
        let t = term("\x1b[34;42mDocuments\x1b[0m");
        let f = Fixture::new();
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec![])));
        let green = f.palette.ansi[2];
        let fg = glyph_colors(&c)[0];
        let fg = crate::theme::Rgb { r: (fg[0] * 255.0).round() as u8, g: (fg[1] * 255.0).round() as u8, b: (fg[2] * 255.0).round() as u8 };
        assert!(fg.contrast(green) >= 4.5, "contrast {}", fg.contrast(green));

        // Turned off: the palette's blue as is.
        let mut v = f.view();
        v.min_contrast = 1.0;
        c.paint(400.0, 300.0, |p| draw(p, &t, &v, &chrome(vec![])));
        assert_eq!(glyph_colors(&c)[0], f.palette.ansi[4].rgba(1.0));
    }

    #[test]
    fn search_matches_get_highlight_backgrounds() {
        let Some(mut c) = Canvas::new() else { return };
        let mut t = term("foo bar foo");
        let mut f = Fixture::new();
        f.search.open();
        f.search.set_query("foo".into(), &mut t);
        f.search.refresh(&t);
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec![])));
        let bg = c.layer(Layer::Background);
        let current = bg.iter().filter(|i| i.color == f.palette.current_match_bg.rgba(1.0)).count();
        let other = bg.iter().filter(|i| i.color == f.palette.match_bg.rgba(1.0)).count();
        assert_eq!((current, other), (3, 3), "one current and one other 3-letter match");
        // The search bar is drawn with its ↑ ↓ + × buttons.
        assert!(c.glyph_count(Layer::UiText) >= "Aafoo3/3".len() + 4);
        let hover_bg = f.palette.ui_dim.rgba(0.3);
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &ChromeView { hover: Hover::Search(ui::SearchHit::Next(0)), ..chrome(vec![]) }));
        assert!(c.layer(Layer::UiShapes).iter().any(|i| i.color == hover_bg), "hovered ↓ is highlighted");
    }

    #[test]
    fn each_search_box_highlights_in_its_own_colour() {
        let Some(mut c) = Canvas::new() else { return };
        let mut t = term("foo bar foo bar");
        let mut f = Fixture::new();
        f.search.open();
        f.search.type_text("foo", &mut t);
        f.search.add_box(0);
        f.search.type_text("bar", &mut t);
        f.search.set_color(1, 3);
        f.search.refresh(&t);
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec![])));
        let bg = c.layer(Layer::Background);
        let count = |col: Rgb| bg.iter().filter(|i| i.color == col.rgba(1.0)).count();
        let (first, second) = (f.palette.search_colors(0), f.palette.search_colors(3));
        assert_eq!(count(first.bg), 6, "both foo matches in box 0's colour");
        assert_eq!((count(second.current_bg), count(second.bg)), (3, 3), "focused box 1 has the current bar");
        // Two bars, and with box 1's colour choices open, a dot per colour.
        let shapes = |c: &Canvas| c.layer(Layer::UiShapes).len();
        let before = shapes(&c);
        f.search.toggle_picker(1);
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec![])));
        assert!(shapes(&c) >= before + 4, "colour choices drawn");
    }

    #[test]
    fn printed_code_is_coloured() {
        let Some(mut c) = Canvas::new() else { return };
        crate::highlight::preload_blocking();
        let mut t = term("$ cat a.c");
        let mut f = Fixture::new();
        let line = crate::highlight::command_line(t.grid());
        f.code.on_enter(t.grid(), &line);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut t, b"\r\nint main(void) { return 0; }\r\n$ ");
        f.code.update(t.grid());
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec![])));
        let fg = f.palette.foreground.rgba(1.0);
        let coloured = glyph_colors(&c).iter().filter(|col| **col != fg).count();
        assert!(coloured >= 10, "keywords and literals coloured, got {coloured}");
    }

    #[test]
    fn tab_bar_menu_and_panels_draw_on_their_layers() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("");
        let f = Fixture::new();
        let menu = Menu::main(400.0, 30.0, false, 2, &Keymap::default());
        let mut ch = chrome(vec!["Tab 1: vim", "Tab 2: cat"]);
        ch.menu = Some(&menu);
        ch.menu_open = true;
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &ch));
        assert!(c.glyph_count(Layer::UiText) >= "Tab1:vim".len(), "tab titles");
        assert!(!c.layer(Layer::OverlayShapes).is_empty());
        assert!(c.glyph_count(Layer::OverlayText) > 20, "menu labels");

        let m = Metrics { width: 900.0, height: 700.0, scale: 1.0, cw: 8.0, ch: 17.0 };
        let opts = options_panel(&m, &OptionsState { page: crate::chrome::OptionsPage::General, theme: ThemeMode::System, font_size: 11.0, font_family: "X".into(), code_highlight: true, gpu: crate::config::GpuBackend::Auto, gpu_pending: false, keys: Keymap::default(), capturing: None, ..OptionsState::default() }, (0.0, 0.0));
        let about = about_panel(&m, "gpu", "300 ms", "/cfg", (0.0, 0.0));
        for panel in [&opts, &about] {
            let mut ch = chrome(vec!["Tab 1"]);
            ch.panel = Some(panel);
            c.paint(900.0, 700.0, |p| draw(p, &t, &f.view(), &ch));
            // Dimmed backdrop + panel + border + buttons.
            assert!(c.layer(Layer::OverlayShapes).len() >= 3 + panel.buttons.len());
            assert!(c.glyph_count(Layer::OverlayText) > 20);
        }
    }

    #[test]
    fn window_controls_are_drawn_only_with_a_custom_title_bar() {
        let Some(mut c) = Canvas::new() else { return };
        let t = term("");
        let f = Fixture::new();
        c.paint(800.0, 300.0, |p| draw(p, &t, &f.view(), &chrome(vec!["Tab 1"])));
        let plain = c.layer(Layer::UiShapes).len();

        let mut ch = chrome(vec!["Tab 1"]);
        ch.caption = Caption { buttons: true, left_inset: 0.0 };
        ch.hover = Hover::CloseWindow;
        c.paint(800.0, 300.0, |p| draw(p, &t, &f.view(), &ch));
        let shapes = c.layer(Layer::UiShapes);
        // Red close hover + minimize line + outlined maximize square + white ×.
        assert!(shapes.len() >= plain + 5);
        assert!(shapes.iter().any(|i| i.color == Rgb::hex(0xc42b1c).rgba(1.0)));
        assert!(shapes.iter().any(|i| i.kind == 4 && i.color == Rgb::hex(0xffffff).rgba(1.0)), "white × over red");

        ch.maximized = true;
        c.paint(800.0, 300.0, |p| draw(p, &t, &f.view(), &ch));
        // Restore icon: two squares, more shapes than the maximize square.
        assert!(c.layer(Layer::UiShapes).len() > plain + 5);
    }

    #[test]
    fn scrollbar_appears_with_history() {
        let Some(mut c) = Canvas::new() else { return };
        let f = Fixture::new();
        c.paint(400.0, 300.0, |p| draw(p, &term("x"), &f.view(), &chrome(vec![])));
        assert!(c.layer(Layer::UiShapes).iter().all(|i| i.kind != 3 || i.pos[1] < 30.0), "no scrollbar without history");
        let many: String = (0..20).map(|i| format!("line {i}\r\n")).collect();
        c.paint(400.0, 300.0, |p| draw(p, &term(&many), &f.view(), &chrome(vec![])));
        let x = 400.0 - crate::ui::scrollbar_width(1.0);
        assert!(c.layer(Layer::UiShapes).iter().any(|i| i.kind == 3 && i.pos[0] >= x - 1.0), "scrollbar track/thumb");
    }
}
