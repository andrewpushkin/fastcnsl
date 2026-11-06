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

use crate::chrome::{self, Caption, Menu, Metrics, Panel, TextStyle};
use crate::highlight::{CodeHighlighter, Span};
use crate::render::glyphs::GlyphStyle;
use crate::render::{Layer, Painter};
use crate::search::Search;
use crate::theme::{Palette, Rgb};
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
    draw_scrollbar(p, term, v);
    if v.search.open {
        draw_search_bar(p, v);
    }
    let m = Metrics { width: p.width, height: p.height, scale: v.scale, cw: p.cell.width, ch: p.cell.height };
    draw_tab_bar(p, v.palette, &m, c);
    if let Some(panel) = c.panel {
        draw_panel(p, v.palette, &m, panel, c.hover);
    }
    if let Some(menu) = c.menu {
        draw_menu(p, v.palette, &m, menu);
    }
}

fn draw_tab_bar(p: &mut Painter<'_>, pal: &Palette, m: &Metrics, c: &ChromeView<'_>) {
    let bar = chrome::tab_bar(m, c.titles.len(), &c.caption);
    let bar_bg = if pal.dark { pal.background.mix(Rgb::hex(0x000000), 0.25) } else { pal.ui_bg };
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
        // Title, cut to fit before the close button.
        let room = ((slot.close.x - t.x - m.cw) / m.cw).floor().max(0.0) as usize;
        let n = title.chars().count();
        let shown: String = if n > room && room > 1 {
            title.chars().take(room - 1).chain(std::iter::once('…')).collect()
        } else {
            title.chars().take(room).collect()
        };
        let color = if active { pal.foreground } else { pal.ui_dim };
        p.text(Layer::UiText, &shown, t.x + m.cw, text_y(&t), color.rgba(1.0));
        if active || hovered {
            let cl = slot.close;
            if c.hover == Hover::TabClose(i) {
                p.rounded(Layer::UiShapes, cl.x, cl.y, cl.w, cl.h, m.scale * 3.0, pal.ui_dim.rgba(0.35));
            }
            p.text(Layer::UiText, "×", cl.x + (cl.w - m.cw) / 2.0, text_y(&cl), pal.ui_dim.rgba(1.0));
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
            p.text(Layer::UiText, "+", rect.x + (rect.w - m.cw) / 2.0, text_y(&rect), color.rgba(1.0));
        }
    }

    draw_window_controls(p, pal, m, &bar, c);
}

/// Minimize, maximize/restore and close, for windows without an OS frame.
fn draw_window_controls(p: &mut Painter<'_>, pal: &Palette, m: &Metrics, bar: &chrome::TabBar, c: &ChromeView<'_>) {
    let (Some(min), Some(max), Some(close)) = (bar.minimize, bar.maximize, bar.close) else { return };
    let t = m.scale.round().max(1.0);
    let s = (10.0 * m.scale).round();
    let center = |r: &ui::Rect| ((r.x + (r.w - s) / 2.0).round(), (r.y + (r.h - s) / 2.0).round());
    for (rect, hover) in [(min, Hover::Minimize), (max, Hover::Maximize), (close, Hover::CloseWindow)] {
        if c.hover == hover {
            let bg = if hover == Hover::CloseWindow { Rgb::hex(0xe81123).rgba(1.0) } else { pal.ui_dim.rgba(0.3) };
            p.rect(Layer::UiShapes, rect.x, rect.y, rect.w, rect.h, bg);
        }
    }
    let fg = |h: Hover| if c.hover == h && h == Hover::CloseWindow { Rgb::hex(0xffffff) } else { pal.foreground };

    let (x, y) = center(&min);
    p.rect(Layer::UiShapes, x, y + (s / 2.0).round(), s, t, fg(Hover::Minimize).rgba(1.0));

    let (x, y) = center(&max);
    let col = fg(Hover::Maximize).rgba(1.0);
    let outline = |p: &mut Painter<'_>, x: f32, y: f32, w: f32| {
        p.rect(Layer::UiShapes, x, y, w, t, col);
        p.rect(Layer::UiShapes, x, y + w - t, w, t, col);
        p.rect(Layer::UiShapes, x, y, t, w, col);
        p.rect(Layer::UiShapes, x + w - t, y, t, w, col);
    };
    if c.maximized {
        // Restore: two overlapping squares.
        let o = (2.0 * m.scale).round();
        let w = s - o;
        p.rect(Layer::UiShapes, x + o, y, w, t, col);
        p.rect(Layer::UiShapes, x + s - t, y, t, w, col);
        outline(p, x, y + o, w);
    } else {
        outline(p, x, y, s);
    }

    let (cx, cy) = (close.x + (close.w - m.cw) / 2.0, (close.y + (close.h - m.ch) / 2.0).round());
    p.text(Layer::UiText, "×", cx.round(), cy, fg(Hover::CloseWindow).rgba(1.0));
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
        if item.action.is_none() {
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
        if item.checked {
            p.text(Layer::OverlayText, "✓", x, y, fg.rgba(1.0));
        }
        p.text(Layer::OverlayText, &item.label, x + 2.0 * m.cw, y, fg.rgba(1.0));
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

    for line in &panel.lines {
        let color = match line.style {
            TextStyle::Title => pal.foreground,
            TextStyle::Heading => pal.ui_accent,
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
        let (bg, fg) = if btn.active {
            (pal.ui_accent, Rgb::hex(0xffffff))
        } else if hovered {
            (pal.ui_dim.mix(pal.ui_bg, 0.5), pal.foreground)
        } else if btn.label == "×" {
            (pal.ui_bg, pal.ui_dim)
        } else {
            (pal.ui_dim.mix(pal.ui_bg, 0.75), pal.ui_fg)
        };
        p.rounded(Layer::OverlayShapes, br.x, br.y, br.w, br.h, m.scale * 5.0, bg.rgba(1.0));
        let tw = btn.label.chars().count() as f32 * m.cw;
        let tx = (br.x + (br.w - tw) / 2.0).round();
        let ty = (br.y + (br.h - m.ch) / 2.0).round();
        p.text(Layer::OverlayText, &btn.label, tx, ty, fg.rgba(1.0));
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
    let matches = &v.search.visible;
    let current = v.search.current.as_ref();
    let thin = (1.0 * v.scale).round().max(1.0);

    let mut mi = 0usize;
    let mut span_line = i32::MIN;
    let mut spans: &[Span] = &[];

    for indexed in content.display_iter {
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let point = indexed.point;
        let row = (point.line.0 + offset) as f32;
        let x = pad + point.column.0 as f32 * cw;
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

        while mi < matches.len() && *matches[mi].end() < point {
            mi += 1;
        }
        if let Some(m) = matches.get(mi).filter(|m| *m.start() <= point) {
            if current.is_some_and(|c| c == m) {
                bg = pal.current_match_bg;
                fg = pal.current_match_fg;
            } else {
                bg = pal.match_bg;
                fg = pal.match_fg;
            }
        }

        let is_cursor = point == cursor.point && cursor.shape != CursorShape::Hidden;
        if is_cursor && v.focused && cursor.shape == CursorShape::Block {
            bg = pal.cursor;
            fg = pal.background;
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
    let Some(g) = ui::scroll_geometry(p.width, v.top, p.height, v.scale, grid.history_size(), grid.screen_lines(), grid.display_offset()) else {
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

fn draw_search_bar(p: &mut Painter<'_>, v: &View<'_>) {
    let pal = v.palette;
    let s = v.search;
    let a = v.search_alpha;
    let (cw, ch) = (p.cell.width, p.cell.height);
    let g = ui::search_geometry(p.width, v.top, v.scale, cw, ch);
    let b = g.bar;
    let r = (6.0 * v.scale).round();
    let border = (1.0 * v.scale).round().max(1.0);

    let edge = if s.invalid {
        pal.ansi[1]
    } else if s.focused {
        pal.ui_accent
    } else {
        pal.ui_dim
    };
    p.rounded(Layer::UiShapes, b.x - border, b.y - border, b.w + 2.0 * border, b.h + 2.0 * border, r + border, edge.rgba(a));
    p.rounded(Layer::UiShapes, b.x, b.y, b.w, b.h, r, pal.ui_bg.rgba(a));

    let pad = (6.0 * v.scale).round();
    let ty = b.y + pad;
    let mut x = b.x + pad;

    // Mode badge: ".*" when regex mode is on, "Aa" otherwise.
    let badge = if s.regex_mode { ".*" } else { "Aa" };
    let badge_color = if s.regex_mode { pal.ui_accent } else { pal.ui_dim };
    x = p.text(Layer::UiText, badge, x, ty, badge_color.rgba(a)) + cw;

    // Status text, right-aligned before the close button.
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
    let status_x = g.close.x - status_w - cw;

    // Query, scrolled to keep its end visible.
    let room = ((status_x - x) / cw).floor().max(1.0) as usize - 1;
    let chars = s.query.chars().count();
    let skip = chars.saturating_sub(room);
    let shown: String = s.query.chars().skip(skip).collect();
    let end = if s.query.is_empty() && !s.focused {
        p.text(Layer::UiText, "search", x, ty, pal.ui_dim.rgba(a))
    } else {
        p.text(Layer::UiText, &shown, x, ty, pal.ui_fg.rgba(a))
    };
    if s.focused {
        p.rect(Layer::UiText, end + 1.0, ty, (2.0 * v.scale).round(), ch, pal.ui_accent.rgba(a));
    }

    p.text(Layer::UiText, &status, status_x, ty, pal.ui_dim.rgba(a));
    p.text(Layer::UiText, "×", g.close.x + cw * 0.5, ty, pal.ui_dim.rgba(a));
}

#[cfg(test)]
mod tests {
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
        // The search bar is drawn too.
        assert!(c.glyph_count(Layer::UiText) >= 3);
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
        let menu = Menu::main(400.0, 30.0, ThemeMode::Dark, false, 2);
        let mut ch = chrome(vec!["Tab 1: vim", "Tab 2: cat"]);
        ch.menu = Some(&menu);
        ch.menu_open = true;
        c.paint(400.0, 300.0, |p| draw(p, &t, &f.view(), &ch));
        assert!(c.glyph_count(Layer::UiText) >= "Tab1:vim".len(), "tab titles");
        assert!(!c.layer(Layer::OverlayShapes).is_empty());
        assert!(c.glyph_count(Layer::OverlayText) > 20, "menu labels");

        let m = Metrics { width: 900.0, height: 700.0, scale: 1.0, cw: 8.0, ch: 17.0 };
        let opts = options_panel(&m, &OptionsState { theme: ThemeMode::System, font_size: 11.0, font_family: "X".into(), code_highlight: true });
        let about = about_panel(&m, "gpu", "/cfg");
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
        // minimize line + 4-sided maximize square + red close hover.
        assert!(shapes.len() >= plain + 6);
        assert!(shapes.iter().any(|i| i.color == Rgb::hex(0xe81123).rgba(1.0)));

        ch.maximized = true;
        c.paint(800.0, 300.0, |p| draw(p, &t, &f.view(), &ch));
        // Restore icon: two squares, more strokes than the maximize square.
        assert!(c.layer(Layer::UiShapes).len() > plain + 6);
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
