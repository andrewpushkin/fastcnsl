//! Turns terminal state into quads: cells, selection, search matches,
//! code colours, cursor, scrollbar and search bar.
//!
//! Everything is read through borrows of the locked terminal: cells are
//! never copied out of the grid.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

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
    pub scrollbar_alpha: f32,
    pub search_alpha: f32,
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

pub fn draw<T: EventListener>(p: &mut Painter<'_>, term: &Term<T>, v: &View<'_>) {
    draw_grid(p, term, v);
    draw_scrollbar(p, term, v);
    if v.search.open {
        draw_search_bar(p, v);
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
        let y = pad + row * ch;

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
    let Some(g) = ui::scroll_geometry(p.width, p.height, v.scale, grid.history_size(), grid.screen_lines(), grid.display_offset()) else {
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
    let g = ui::search_geometry(p.width, v.scale, cw, ch);
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
