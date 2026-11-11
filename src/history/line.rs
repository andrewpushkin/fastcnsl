//! One logical line in compact form: rows joined while they wrap, stored as
//! UTF-8 text with style changes in between, and wrapped again at any width
//! when read back.
//!
//! The encoding is a sequence of cells. A cell is its character in UTF-8,
//! drawn in the current style. Bytes that can't start a UTF-8 character
//! introduce records:
//!
//! - `STYLE fg bg flags`: the style of the cells that follow.
//! - `EXTRA ...`: zero-width characters, underline colour and hyperlink of
//!   the next cell only.
//! - `WIDE c`: a wide character followed by its spacer cell, both in the
//!   current style.
//!
//! Plain text in the default style costs one byte per ASCII cell.

use std::sync::Arc;

use alacritty_terminal::grid::Row;
use alacritty_terminal::term::cell::{Cell, CellExtra, Flags, Hyperlink};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

const STYLE: u8 = 0xF8;
const EXTRA: u8 = 0xF9;
const WIDE: u8 = 0xFA;

const NAMED: [NamedColor; 29] = [
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::White,
    NamedColor::BrightBlack,
    NamedColor::BrightRed,
    NamedColor::BrightGreen,
    NamedColor::BrightYellow,
    NamedColor::BrightBlue,
    NamedColor::BrightMagenta,
    NamedColor::BrightCyan,
    NamedColor::BrightWhite,
    NamedColor::Foreground,
    NamedColor::Background,
    NamedColor::Cursor,
    NamedColor::DimBlack,
    NamedColor::DimRed,
    NamedColor::DimGreen,
    NamedColor::DimYellow,
    NamedColor::DimBlue,
    NamedColor::DimMagenta,
    NamedColor::DimCyan,
    NamedColor::DimWhite,
    NamedColor::BrightForeground,
    NamedColor::DimForeground,
];

/// Appends one logical line's cells, given its rows top to bottom. A row
/// that wraps loses the WRAPLINE flag and the spacer a wide character left
/// at its end; a row that doesn't loses its trailing default cells.
pub fn join_rows<'a>(rows: impl IntoIterator<Item = &'a Row<Cell>>, out: &mut Vec<Cell>) {
    let blank = Cell::default();
    for row in rows {
        let cells = &row[..];
        let wraps = cells.last().is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
        let start = out.len();
        out.extend_from_slice(cells);
        if wraps {
            if let Some(last) = out.last_mut() {
                last.flags.remove(Flags::WRAPLINE);
                if last.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    out.pop();
                }
            }
        } else {
            while out.len() > start && out.last() == Some(&blank) {
                out.pop();
            }
        }
    }
}

/// The cell a wide character's spacer would be, drawn in `wide`'s style.
fn spacer_of(wide: &Cell) -> Cell {
    Cell {
        c: ' ',
        fg: wide.fg,
        bg: wide.bg,
        flags: (wide.flags - Flags::WIDE_CHAR) | Flags::WIDE_CHAR_SPACER,
        extra: None,
    }
}

/// Appends the encoded form of `cells` to `out`.
pub fn encode(cells: &[Cell], out: &mut Vec<u8>) {
    let (mut fg, mut bg, mut flags) = (Color::Named(NamedColor::Foreground), Color::Named(NamedColor::Background), Flags::empty());
    let mut utf8 = [0u8; 4];
    let mut i = 0;
    while i < cells.len() {
        let cell = &cells[i];
        let wide = cell.flags.contains(Flags::WIDE_CHAR) && cells.get(i + 1) == Some(&spacer_of(cell));
        let cell_flags = if wide { cell.flags - Flags::WIDE_CHAR } else { cell.flags };
        if cell.fg != fg || cell.bg != bg || cell_flags != flags {
            (fg, bg, flags) = (cell.fg, cell.bg, cell_flags);
            out.push(STYLE);
            put_color(out, fg);
            put_color(out, bg);
            out.extend_from_slice(&flags.bits().to_le_bytes());
        }
        if let Some(extra) = &cell.extra {
            put_extra(out, cell, extra);
        }
        if wide {
            out.push(WIDE);
        }
        if cell.c.is_ascii() {
            out.push(cell.c as u8);
        } else {
            out.extend_from_slice(cell.c.encode_utf8(&mut utf8).as_bytes());
        }
        i += if wide { 2 } else { 1 };
    }
}

fn put_color(out: &mut Vec<u8>, color: Color) {
    match color {
        Color::Named(n) => {
            out.push(0);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        Color::Indexed(i) => out.extend_from_slice(&[1, i]),
        Color::Spec(Rgb { r, g, b }) => out.extend_from_slice(&[2, r, g, b]),
    }
}

fn put_varint(out: &mut Vec<u8>, mut n: usize) {
    while n >= 0x80 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

fn put_extra(out: &mut Vec<u8>, cell: &Cell, _extra: &Arc<CellExtra>) {
    out.push(EXTRA);
    let zerowidth = cell.zerowidth().unwrap_or(&[]);
    put_varint(out, zerowidth.len());
    let mut utf8 = [0u8; 4];
    for c in zerowidth {
        out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
    }
    match cell.underline_color() {
        Some(color) => {
            out.push(1);
            put_color(out, color);
        }
        None => out.push(0),
    }
    match cell.hyperlink() {
        Some(link) => {
            out.push(1);
            put_str(out, link.id());
            put_str(out, link.uri());
        }
        None => out.push(0),
    }
}

/// Reads the encoded form back.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> u8 {
        let b = self.bytes[self.at];
        self.at += 1;
        b
    }

    fn char(&mut self) -> char {
        let b = self.bytes[self.at];
        if b < 0x80 {
            self.at += 1;
            return b as char;
        }
        let len = match b {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            _ => 4,
        };
        let s = std::str::from_utf8(&self.bytes[self.at..self.at + len]).expect("encoded as UTF-8");
        self.at += len;
        s.chars().next().expect("one character")
    }

    fn color(&mut self) -> Color {
        match self.byte() {
            0 => {
                let n = u16::from_le_bytes([self.byte(), self.byte()]);
                Color::Named(NAMED.iter().copied().find(|&c| c as u16 == n).unwrap_or(NamedColor::Foreground))
            }
            1 => Color::Indexed(self.byte()),
            _ => Color::Spec(Rgb { r: self.byte(), g: self.byte(), b: self.byte() }),
        }
    }

    fn varint(&mut self) -> usize {
        let mut n = 0usize;
        let mut shift = 0;
        loop {
            let b = self.byte();
            n |= ((b & 0x7F) as usize) << shift;
            if b < 0x80 {
                return n;
            }
            shift += 7;
        }
    }

    fn string(&mut self) -> String {
        let len = self.varint();
        let s = String::from_utf8_lossy(&self.bytes[self.at..self.at + len]).into_owned();
        self.at += len;
        s
    }

    fn extra(&mut self, cell: &mut Cell) {
        cell.extra = Some(Arc::new(CellExtra::default()));
        for _ in 0..self.varint() {
            let c = self.char();
            cell.push_zerowidth(c);
        }
        if self.byte() == 1 {
            let color = self.color();
            cell.set_underline_color(Some(color));
        }
        if self.byte() == 1 {
            let id = self.string();
            let uri = self.string();
            cell.set_hyperlink(Some(Hyperlink::new(Some(id), uri)));
        }
    }
}

/// Calls `f` with each cell of an encoded line, in order.
fn for_each_cell(bytes: &[u8], mut f: impl FnMut(&Cell)) {
    let mut r = Reader { bytes, at: 0 };
    let mut style = Cell::default();
    let mut cell = Cell::default();
    while r.at < bytes.len() {
        match bytes[r.at] {
            STYLE => {
                r.at += 1;
                style.fg = r.color();
                style.bg = r.color();
                style.flags = Flags::from_bits_retain(u16::from_le_bytes([r.byte(), r.byte()]));
            }
            EXTRA => {
                r.at += 1;
                r.extra(&mut cell);
            }
            WIDE => {
                r.at += 1;
                cell.c = r.char();
                cell.fg = style.fg;
                cell.bg = style.bg;
                cell.flags = style.flags | Flags::WIDE_CHAR;
                f(&cell);
                f(&spacer_of(&cell));
                cell.extra = None;
            }
            _ => {
                cell.c = r.char();
                cell.fg = style.fg;
                cell.bg = style.bg;
                cell.flags = style.flags;
                f(&cell);
                cell.extra = None;
            }
        }
    }
}

/// Appends the cells of an encoded line to `out`.
pub fn decode(bytes: &[u8], out: &mut Vec<Cell>) {
    for_each_cell(bytes, |cell| out.push(cell.clone()));
}

/// Appends the text of an encoded line: spacers left out, zero-width
/// characters kept, like `lines::push_row`.
pub fn text(bytes: &[u8], out: &mut String) {
    for_each_cell(bytes, |cell| {
        if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            out.push(cell.c);
            if let Some(zw) = cell.zerowidth() {
                out.extend(zw);
            }
        }
    });
}

/// Lays `cells` out in rows of `columns` the way alacritty reflows a line:
/// calls `cell(row, cell)` for each cell placed and `end(row, wraps)` after
/// each row. Stops early when `cell` or `end` returns false.
fn lay_out(cells: &[Cell], columns: usize, mut cell: impl FnMut(usize, &Cell) -> bool, mut end: impl FnMut(usize, bool) -> bool) {
    let columns = columns.max(1);
    let mut row = 0;
    let mut col = 0;
    let mut leading = Cell::default();
    leading.flags = Flags::LEADING_WIDE_CHAR_SPACER;
    for c in cells {
        if columns < 2 && c.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let width = if c.flags.contains(Flags::WIDE_CHAR) && columns >= 2 { 2 } else { 1 };
        if col + width > columns {
            if col < columns && !cell(row, &leading) {
                return;
            }
            if !end(row, true) {
                return;
            }
            row += 1;
            col = 0;
        }
        if !cell(row, c) {
            return;
        }
        col += 1;
    }
    end(row, false);
}

/// Rows `from..to` of the line at `columns`: WRAPLINE on all but the last,
/// a wide character that doesn't fit moved to the next row behind a
/// LEADING_WIDE_CHAR_SPACER, each row padded to `columns` with default
/// cells. An empty line is one row.
pub fn wrap(cells: &[Cell], columns: usize, from: usize, to: usize, out: &mut Vec<Row<Cell>>) {
    let columns = columns.max(1);
    let mut current: Vec<Cell> = Vec::new();
    let pending = std::cell::RefCell::new(&mut current);
    let rows = std::cell::RefCell::new(out);
    lay_out(
        cells,
        columns,
        |row, cell| {
            if row >= to {
                return false;
            }
            if row >= from {
                pending.borrow_mut().push(cell.clone());
            }
            true
        },
        |row, wraps| {
            if row >= from && row < to {
                let mut cur = pending.borrow_mut();
                let occ = cur.len();
                let mut cells = std::mem::take(&mut **cur);
                cells.resize_with(columns, Cell::default);
                if wraps {
                    cells[columns - 1].flags.insert(Flags::WRAPLINE);
                }
                rows.borrow_mut().push(Row::from_vec(cells, occ));
            }
            row + 1 < to
        },
    );
}

/// How many rows the line takes at `columns`.
pub fn row_count(cells: &[Cell], columns: usize) -> usize {
    let mut rows = 0;
    lay_out(cells, columns, |_, _| true, |_, _| {
        rows += 1;
        true
    });
    rows
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::Column;
    use alacritty_terminal::term::cell::Flags;

    use super::*;
    use crate::lines;
    use crate::testterm::TestTerm;

    /// Styles, colours, a wide character that wraps, a combining mark, a
    /// hyperlink, underline colours and a coloured erase.
    const STYLED: &str = "plain \x1b[1mbold\x1b[0m \x1b[3;4mital-under\x1b[0m\n\
        \x1b[38;5;200mindexed\x1b[0m \x1b[38;2;1;2;3;48;2;9;8;7mrgb\x1b[0m \x1b[7minv\x1b[2mdim\x1b[0m\n\
        0123456789012345678漢字 wraps here\n\
        e\u{301} combining \x1b]8;;http://x.test/\x1b\\link\x1b]8;;\x1b\\ \x1b[4:3;58;5;3mcurly\x1b[0m\n\
        \x1b[41mX\x1b[K\x1b[0m\n\
        \n\
        last line that is long enough to wrap twice at twenty columns\n";

    fn joined(t: &TestTerm, start: usize) -> Vec<Cell> {
        let mut cells = Vec::new();
        join_rows(t.rows_of_line(start), &mut cells);
        cells
    }

    fn round_trip(cells: &[Cell]) -> Vec<Cell> {
        let mut bytes = Vec::new();
        encode(cells, &mut bytes);
        let mut out = Vec::new();
        decode(&bytes, &mut out);
        out
    }

    fn assert_rows_eq(got: &[Row<Cell>], want: &[&Row<Cell>], cols: usize, what: &str) {
        assert_eq!(got.len(), want.len(), "{what}: row count");
        for (r, (g, w)) in got.iter().zip(want).enumerate() {
            for c in 0..cols {
                assert_eq!(g[Column(c)], w[Column(c)], "{what}: row {r} column {c}");
            }
        }
    }

    #[test]
    fn round_trips_styled_output() {
        let mut t = TestTerm::new(20, 30, 100);
        t.feed(STYLED);
        let starts = t.line_starts();
        assert!(starts.len() >= 7);
        for &start in &starts {
            let cells = round_trip(&joined(&t, start));
            assert_eq!(cells, joined(&t, start), "line at row {start}");
            let mut rows = Vec::new();
            wrap(&cells, 20, 0, usize::MAX, &mut rows);
            assert_rows_eq(&rows, &t.rows_of_line(start), 20, &format!("line at row {start}"));
        }
    }

    #[test]
    fn rewraps_at_other_widths() {
        for width in [7, 33] {
            let mut t = TestTerm::new(20, 30, 100);
            t.feed(STYLED);
            let lines: Vec<Vec<Cell>> = t.line_starts().iter().map(|&s| round_trip(&joined(&t, s))).collect();
            t.resize(width, 30);
            let starts = t.line_starts();
            for (i, &start) in starts.iter().enumerate().take(lines.len()) {
                let mut rows = Vec::new();
                wrap(&lines[i], width, 0, usize::MAX, &mut rows);
                assert_rows_eq(&rows, &t.rows_of_line(start), width, &format!("width {width}, line {i}"));
            }
        }
    }

    #[test]
    fn text_matches_push_row() {
        let mut t = TestTerm::new(20, 30, 100);
        t.feed(STYLED);
        let grid = t.term.grid();
        for start in t.line_starts() {
            let mut want = String::new();
            for i in 0..t.rows_of_line(start).len() {
                lines::push_row(&mut want, grid, start + i);
            }
            let mut bytes = Vec::new();
            encode(&joined(&t, start), &mut bytes);
            let mut got = String::new();
            text(&bytes, &mut got);
            assert_eq!(got.trim_end(), want.trim_end(), "line at row {start}");
        }
    }

    #[test]
    fn row_count_matches_wrap() {
        let mut t = TestTerm::new(20, 30, 100);
        t.feed(STYLED);
        let empty: Vec<Cell> = Vec::new();
        for width in [1, 2, 80] {
            for start in t.line_starts() {
                let cells = joined(&t, start);
                let mut rows = Vec::new();
                wrap(&cells, width, 0, usize::MAX, &mut rows);
                assert_eq!(row_count(&cells, width), rows.len(), "width {width}, row {start}");
            }
        }
        assert_eq!(row_count(&empty, 80), 1);
        let mut rows = Vec::new();
        wrap(&empty, 80, 0, usize::MAX, &mut rows);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn wrap_range_returns_only_requested_rows() {
        let cells: Vec<Cell> = "abcdefghijklmnopqrstuvwxyz0123".chars().map(|c| Cell { c, ..Cell::default() }).collect();
        let mut rows = Vec::new();
        wrap(&cells, 3, 3, 5, &mut rows);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][Column(0)].c, 'j');
        assert_eq!(rows[1][Column(0)].c, 'm');
        assert!(rows[1][Column(2)].flags.contains(Flags::WRAPLINE));
    }

    #[test]
    fn plain_ascii_costs_one_byte_per_cell() {
        let cells: Vec<Cell> = "hello world".chars().map(|c| Cell { c, ..Cell::default() }).collect();
        let mut bytes = Vec::new();
        encode(&cells, &mut bytes);
        assert_eq!(bytes.len(), 11);
    }
}
