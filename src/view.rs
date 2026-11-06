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

use crate::chrome::{self, Menu, Metrics, Panel, TextStyle};
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
}

pub struct ChromeView<'a> {
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
    let bar = chrome::tab_bar(m, c.titles.len());
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
