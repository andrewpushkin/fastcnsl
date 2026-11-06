//! Syntax highlighting for code printed into the terminal.
//!
//! When Enter is pressed on a command line like `cat hello.c`, the rows the
//! command prints become a *block*. Each complete logical line of the block
//! (wrapped rows joined) is highlighted once with syntect, incrementally as
//! output arrives; the result is cached as colour spans per row. Only cells
//! the program left in the default foreground colour are recoloured, so
//! programs that colour their own output (`bat`, `git diff --color`) win.
//!
//! Rows are addressed by an *absolute* index counted from the top of the
//! scrollback (`history_size + line`), which stays put as output scrolls.
//! Once scrollback is full, the oldest lines drop off and indices shift;
//! that is detected by checking that each block's command line is still
//! where we left it.

use std::sync::OnceLock;

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::detect::{self, Hint};
use crate::theme::Rgb;

struct Assets {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

static ASSETS: OnceLock<Assets> = OnceLock::new();

/// Loads syntax definitions on a background thread (~30 ms) so startup
/// isn't delayed. Highlighting starts as soon as they're ready.
/// Loads syntax definitions on the calling thread (tests).
#[cfg(test)]
pub fn preload_blocking() {
    ASSETS.get_or_init(|| Assets {
        syntaxes: SyntaxSet::load_defaults_nonewlines(),
        themes: ThemeSet::load_defaults(),
    });
}

pub fn preload() {
    std::thread::spawn(|| {
        ASSETS.get_or_init(|| Assets {
            syntaxes: SyntaxSet::load_defaults_nonewlines(),
            themes: ThemeSet::load_defaults(),
        });
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u16,
    pub end: u16,
    pub color: Rgb,
}

enum Lang {
    /// Not decided yet: waiting for output to sniff.
    Pending,
    Plain,
    Code(&'static SyntaxReference),
}

struct Block {
    /// Absolute row of the command line (its last row).
    cmd_row: i64,
    /// Text of that row, used to detect when rows shift.
    anchor: String,
    /// First output row.
    start: i64,
    /// One past the last output row, once the next command was entered.
    end: Option<i64>,
    hint: Hint,
    lang: Lang,
    highlighter: Option<HighlightLines<'static>>,
    /// Next row to highlight.
    next_row: i64,
    /// Spans for rows `start..next_row`.
    rows: Vec<Vec<Span>>,
}

const MAX_BLOCKS: usize = 64;
/// Rows to wait for before sniffing a language from content.
const SNIFF_ROWS: i64 = 8;

pub struct CodeHighlighter {
    pub enabled: bool,
    blocks: Vec<Block>,
    theme_name: String,
    /// Reused per-line buffers.
    text: String,
    map: Vec<(usize, u16, u16)>,
}

impl CodeHighlighter {
    pub fn new(enabled: bool, theme_name: &str) -> Self {
        Self {
            enabled,
            blocks: Vec::new(),
            theme_name: theme_name.to_owned(),
            text: String::new(),
            map: Vec::new(),
        }
    }

    /// Switches the colour theme; everything is re-highlighted.
    pub fn set_theme(&mut self, name: &str) {
        if name == self.theme_name {
            return;
        }
        self.theme_name = name.to_owned();
        for b in &mut self.blocks {
            b.highlighter = None;
            b.next_row = b.start;
            b.rows.clear();
        }
    }

    /// Forget all blocks (screen cleared, window resized and text reflowed).
    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    /// Call when the user presses Enter, before the key reaches the shell,
    /// with the line from [`command_line`].
    pub fn on_enter(&mut self, grid: &Grid<Cell>, line: &str) {
        if !self.enabled {
            return;
        }
        let hist = grid.history_size() as i64;
        let cursor = grid.cursor.point.line;
        let first = logical_line_start(grid, cursor);
        if let Some(open) = self.blocks.last_mut().filter(|b| b.end.is_none()) {
            open.end = Some(hist + first.0 as i64);
        }
        let Some(hint) = detect::command_hint(line.trim_end()) else { return };

        let cmd_row = hist + cursor.0 as i64;
        let mut anchor = String::new();
        append_row(&mut anchor, grid, cursor);
        if self.blocks.len() == MAX_BLOCKS {
            self.blocks.remove(0);
        }
        self.blocks.push(Block {
            cmd_row,
            anchor,
            start: cmd_row + 1,
            end: None,
            hint,
            lang: Lang::Pending,
            highlighter: None,
            next_row: cmd_row + 1,
            rows: Vec::new(),
        });
    }

    /// Re-anchors blocks and highlights newly completed lines. Call once
    /// per frame with the primary (non-alternate) screen grid.
    pub fn update(&mut self, grid: &Grid<Cell>) {
        if !self.enabled || self.blocks.is_empty() {
            return;
        }
        let Some(assets) = ASSETS.get() else { return };
        self.reanchor(grid);

        let hist = grid.history_size() as i64;
        let cursor_abs = hist + grid.cursor.point.line.0 as i64;
        let theme = theme(assets, &self.theme_name);
        let default_fg = theme.settings.foreground;

        for b in &mut self.blocks {
            let limit = b.end.unwrap_or(cursor_abs);
            if matches!(b.lang, Lang::Pending) {
                b.lang = decide_lang(assets, b, grid, limit, &mut self.text);
            }
            let Lang::Code(syntax) = b.lang else { continue };
            let hl = b.highlighter.get_or_insert_with(|| HighlightLines::new(syntax, theme));

            while b.next_row < limit {
                // Gather one logical line: rows joined while they wrap.
                let first = b.next_row;
                let mut last = first;
                while last + 1 < limit && row_wraps(grid, abs_line(last, hist)) {
                    last += 1;
                }
                if b.end.is_none() && last + 1 == limit && row_wraps(grid, abs_line(last, hist)) {
                    break; // still being written
                }

                self.text.clear();
                self.map.clear();
                for (off, r) in (first..=last).enumerate() {
                    collect_row(&mut self.text, &mut self.map, grid, abs_line(r, hist), off as u16);
                }
                let trimmed = self.text.trim_end().len();
                self.text.truncate(trimmed);

                let mut row_spans: Vec<Vec<Span>> = vec![Vec::new(); (last - first + 1) as usize];
                if let Ok(ranges) = hl.highlight_line(&self.text, &assets.syntaxes) {
                    let mut byte = 0usize;
                    let mut mi = 0usize;
                    for (style, piece) in ranges {
                        let end = byte + piece.len();
                        let fg = style.foreground;
                        let colored = Some(fg) != default_fg;
                        while mi < self.map.len() && self.map[mi].0 < end {
                            let (_, row_off, col) = self.map[mi];
                            if colored {
                                push_span(&mut row_spans[row_off as usize], col, Rgb { r: fg.r, g: fg.g, b: fg.b });
                            }
                            mi += 1;
                        }
                        byte = end;
                    }
                }
                b.rows.extend(row_spans);
                b.next_row = last + 1;
            }
        }
    }

    /// Colour spans for an absolute row, sorted by column.
    pub fn row(&self, abs_row: i64) -> &[Span] {
        let i = self.blocks.partition_point(|b| b.start <= abs_row);
        if i == 0 {
            return &[];
        }
        let b = &self.blocks[i - 1];
        if abs_row >= b.next_row {
            return &[];
        }
        b.rows.get((abs_row - b.start) as usize).map_or(&[], Vec::as_slice)
    }

    /// Detects rows shifting (scrollback full) or being cleared, using the
    /// newest block's command line as the reference.
    fn reanchor(&mut self, grid: &Grid<Cell>) {
        let hist = grid.history_size() as i64;
        let Some(newest) = self.blocks.last() else { return };
        let total = grid.total_lines() as i64;
        let at = |abs: i64| abs >= 0 && abs < total && row_equals(grid, abs_line(abs, hist), &newest.anchor);
        if at(newest.cmd_row) {
            return;
        }
        let max_shift = grid.screen_lines() as i64 * 4;
        let Some(shift) = (1..=max_shift).find(|k| at(newest.cmd_row - k)) else {
            self.blocks.clear();
            return;
        };
        for b in &mut self.blocks {
            b.cmd_row -= shift;
            b.start -= shift;
            b.next_row -= shift;
            if let Some(e) = &mut b.end {
                *e -= shift;
            }
        }
        // Drop blocks that scrolled out of history completely.
        self.blocks.retain(|b| b.end.is_none_or(|e| e > 0));
        for b in &mut self.blocks {
            if b.start < 0 {
                let cut = (-b.start) as usize;
                b.rows.drain(..cut.min(b.rows.len()));
                b.start = 0;
                b.next_row = b.next_row.max(0);
            }
        }
    }
}

fn theme<'a>(assets: &'a Assets, name: &str) -> &'a Theme {
    assets
        .themes
        .themes
        .get(name)
        .or_else(|| assets.themes.themes.get("base16-ocean.dark"))
        .or_else(|| assets.themes.themes.values().next())
        .expect("syntect ships default themes")
}

fn decide_lang(assets: &'static Assets, b: &Block, grid: &Grid<Cell>, limit: i64, buf: &mut String) -> Lang {
    let ss = &assets.syntaxes;
    let by_ext = |ext: &str| ss.find_syntax_by_extension(ext).filter(|s| s.name != "Plain Text");
    match &b.hint {
        Hint::Diff => {
            if let Some(s) = by_ext("diff") {
                return Lang::Code(s);
            }
        }
        Hint::File(path) => {
            let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
            let found = by_ext(name).or_else(|| name.rsplit_once('.').and_then(|(_, ext)| by_ext(ext)));
            if let Some(s) = found {
                return Lang::Code(s);
            }
        }
        Hint::Unknown => {}
    }
    // Sniff from content once enough rows are in (or the command finished).
    let available = limit - b.start;
    if available < SNIFF_ROWS && b.end.is_none() {
        return Lang::Pending;
    }
    let hist = grid.history_size() as i64;
    buf.clear();
    for r in b.start..limit.min(b.start + 60) {
        append_row(buf, grid, abs_line(r, hist));
        buf.push('\n');
    }
    let lines: Vec<&str> = buf.lines().collect();
    if let Some(s) = lines.first().and_then(|l| ss.find_syntax_by_first_line(l)) {
        return Lang::Code(s);
    }
    match detect::sniff(&lines).and_then(by_ext) {
        Some(s) => Lang::Code(s),
        None => Lang::Plain,
    }
}

/// Text of the (possibly wrapped) line the cursor is on, prompt included:
/// the command line when the user presses Enter.
pub fn command_line(grid: &Grid<Cell>) -> String {
    let cursor = grid.cursor.point.line;
    let mut text = String::new();
    for l in logical_line_start(grid, cursor).0..cursor.0 {
        // Wrapped rows are full; keep their trailing blanks (spaces at the wrap).
        let row = &grid[Line(l)];
        text.extend((0..grid.columns()).map(|c| &row[Column(c)]).filter(|c| !is_spacer(c)).map(|c| c.c));
    }
    append_row(&mut text, grid, cursor);
    text
}

fn logical_line_start(grid: &Grid<Cell>, mut line: Line) -> Line {
    while line > grid.topmost_line() && row_wraps(grid, line - 1) {
        line -= 1;
    }
    line
}

fn abs_line(abs: i64, hist: i64) -> Line {
    Line((abs - hist) as i32)
}

fn row_wraps(grid: &Grid<Cell>, line: Line) -> bool {
    grid[line][grid.last_column()].flags.contains(Flags::WRAPLINE)
}

fn is_spacer(cell: &Cell) -> bool {
    cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
}

fn append_row(out: &mut String, grid: &Grid<Cell>, line: Line) {
    let row = &grid[line];
    let start = out.len();
    for col in 0..grid.columns() {
        let cell = &row[Column(col)];
        if !is_spacer(cell) {
            out.push(cell.c);
        }
    }
    let trimmed = out[start..].trim_end().len();
    out.truncate(start + trimmed);
}

/// Compares a row's text (trailing blanks ignored) without allocating.
fn row_equals(grid: &Grid<Cell>, line: Line, text: &str) -> bool {
    let row = &grid[line];
    let mut want = text.chars();
    let mut col = 0;
    let cols = grid.columns();
    while col < cols {
        let cell = &row[Column(col)];
        col += 1;
        if is_spacer(cell) {
            continue;
        }
        match want.next() {
            Some(c) if c == cell.c => {}
            Some(_) => return false,
            None => {
                // Rest of the row must be blank.
                return cell.c == ' ' && (col..cols).all(|c| row[Column(c)].c == ' ');
            }
        }
    }
    want.next().is_none()
}

/// Appends a row's characters to `text` and records, for each character,
/// (byte offset, row offset within the logical line, column).
fn collect_row(text: &mut String, map: &mut Vec<(usize, u16, u16)>, grid: &Grid<Cell>, line: Line, row_off: u16) {
    let row = &grid[line];
    for col in 0..grid.columns() {
        let cell = &row[Column(col)];
        if is_spacer(cell) {
            continue;
        }
        map.push((text.len(), row_off, col as u16));
        text.push(cell.c);
    }
}

fn push_span(spans: &mut Vec<Span>, col: u16, color: Rgb) {
    if let Some(last) = spans.last_mut()
        && last.end == col && last.color == color {
            last.end = col + 1;
            return;
        }
    spans.push(Span { start: col, end: col + 1, color });
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::test::mock_term;

    fn ready() {
        ASSETS.get_or_init(|| Assets {
            syntaxes: SyntaxSet::load_defaults_nonewlines(),
            themes: ThemeSet::load_defaults(),
        });
    }

    #[test]
    fn highlights_cat_output_and_not_the_prompt() {
        ready();
        // Row 0: prompt + command, rows 1-3: file, row 4: next prompt.
        let mut term = mock_term("$ cat hello.c\r\n#include <stdio.h>\r\nint main(void) {\r\n}\r\n$ ");
        let mut hl = CodeHighlighter::new(true, "base16-ocean.dark");
        term.grid_mut().cursor.point.line = Line(0);
        hl.on_enter(term.grid(), &command_line(term.grid()));
        term.grid_mut().cursor.point.line = Line(4);
        hl.update(term.grid());

        assert!(hl.row(0).is_empty(), "command line must not be coloured");
        assert!(!hl.row(1).is_empty(), "#include line should be coloured");
        assert!(!hl.row(2).is_empty(), "int main should be coloured");
        assert!(hl.row(4).is_empty(), "prompt after output must not be coloured");
        // `int` keyword starts at column 0 and differs from `main`.
        let spans = hl.row(2);
        assert_eq!(spans[0].start, 0);
    }

    #[test]
    fn other_commands_are_left_alone() {
        ready();
        let mut term = mock_term("$ ls\r\nhello.c\r\n$ ");
        let mut hl = CodeHighlighter::new(true, "base16-ocean.dark");
        term.grid_mut().cursor.point.line = Line(0);
        hl.on_enter(term.grid(), &command_line(term.grid()));
        term.grid_mut().cursor.point.line = Line(2);
        hl.update(term.grid());
        assert!(hl.row(1).is_empty());
    }

    #[test]
    fn unknown_extension_is_sniffed() {
        ready();
        let mut term = mock_term("$ cat script\r\n#!/usr/bin/env python3\r\ndef f(x):\r\n    return x\r\n$ ");
        let mut hl = CodeHighlighter::new(true, "base16-ocean.dark");
        term.grid_mut().cursor.point.line = Line(0);
        hl.on_enter(term.grid(), &command_line(term.grid()));
        // Next command closes the block, which allows sniffing short output.
        term.grid_mut().cursor.point.line = Line(4);
        hl.on_enter(term.grid(), &command_line(term.grid()));
        hl.update(term.grid());
        assert!(!hl.row(2).is_empty(), "python def line should be coloured");
    }

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::Processor;

    /// A real terminal fed through the escape-sequence parser.
    struct Tty {
        term: Term<VoidListener>,
        parser: Processor,
    }

    impl Tty {
        fn new(cols: usize, lines: usize, history: usize) -> Self {
            let cfg = TermConfig { scrolling_history: history, ..TermConfig::default() };
            Self { term: Term::new(cfg, &TermSize::new(cols, lines), VoidListener), parser: Processor::new() }
        }

        fn feed(&mut self, s: &str) {
            self.parser.advance(&mut self.term, s.as_bytes());
        }

        /// Types a command and presses Enter, as the app does.
        fn run(&mut self, hl: &mut CodeHighlighter, cmd: &str) {
            self.feed(cmd);
            let line = command_line(self.term.grid());
            hl.on_enter(self.term.grid(), &line);
            self.feed("\r\n");
        }

        /// Absolute row of the first screen/history row starting with `prefix`.
        fn find(&self, prefix: &str) -> i64 {
            let g = self.term.grid();
            let hist = g.history_size() as i64;
            (g.topmost_line().0..=g.bottommost_line().0)
                .find(|&l| {
                    let mut t = String::new();
                    append_row(&mut t, g, Line(l));
                    t.starts_with(prefix)
                })
                .map(|l| hist + l as i64)
                .unwrap_or_else(|| panic!("{prefix:?} not on screen"))
        }
    }

    #[test]
    fn colours_follow_their_rows_when_full_scrollback_drops_lines() {
        ready();
        let mut tty = Tty::new(40, 5, 10);
        let mut hl = CodeHighlighter::new(true, "base16-ocean.dark");
        for i in 0..12 {
            tty.feed(&format!("filler {i}\r\n"));
        }
        tty.run(&mut hl, "$ cat a.rs");
        tty.feed("fn main() {\r\n    let x = 1;\r\n}\r\n$ ");
        hl.update(tty.term.grid());
        let before = tty.find("fn main");
        assert!(!hl.row(before).is_empty());

        // Three more lines push three old ones out of the full history.
        tty.feed("a\r\nb\r\nc\r\n");
        hl.update(tty.term.grid());
        let after = tty.find("fn main");
        assert_eq!(after, before - 3, "rows shifted");
        assert!(!hl.row(after).is_empty(), "colours moved with the code");
        assert!(hl.row(tty.find("$ cat")).is_empty());
    }

    #[test]
    fn blocks_drop_when_their_text_scrolls_away_or_is_cleared() {
        ready();
        let mut tty = Tty::new(40, 5, 10);
        let mut hl = CodeHighlighter::new(true, "base16-ocean.dark");
        tty.run(&mut hl, "$ cat a.c");
        tty.feed("int x;\r\n$ ");
        hl.update(tty.term.grid());
        assert!(!hl.blocks.is_empty());
        // Clear screen and scrollback, like `clear`.
        tty.feed("\x1b[H\x1b[2J\x1b[3J");
        hl.update(tty.term.grid());
        assert!(hl.blocks.is_empty());
    }

    #[test]
    fn wrapped_lines_are_highlighted_as_one_line() {
        ready();
        let mut tty = Tty::new(20, 6, 100);
        let mut hl = CodeHighlighter::new(true, "base16-ocean.dark");
        tty.run(&mut hl, "$ cat a.c");
        // 30 characters: wraps onto a second row in the middle of a string.
        tty.feed("char *s = \"abcdefghijklmnop\";\r\n$ ");
        hl.update(tty.term.grid());
        let first = tty.find("char");
        let second = hl.row(first + 1);
        assert!(!second.is_empty(), "continuation row is coloured");
        // The continuation is inside the string, so it gets the string colour.
        let string_color = hl.row(first).last().unwrap().color;
        assert_eq!(second[0].color, string_color);
    }

    #[test]
    fn command_line_joins_wrapped_rows() {
        let mut tty = Tty::new(10, 4, 10);
        tty.feed("$ cat some/long/path.rs");
        assert_eq!(command_line(tty.term.grid()), "$ cat some/long/path.rs");
    }

    #[test]
    fn theme_change_rehighlights_and_disabled_does_nothing() {
        ready();
        let mut tty = Tty::new(40, 6, 100);
        let mut hl = CodeHighlighter::new(true, "base16-ocean.dark");
        tty.run(&mut hl, "$ cat a.py");
        tty.feed("def f(): return 1\r\n$ ");
        hl.update(tty.term.grid());
        let row = tty.find("def");
        let dark = hl.row(row).to_vec();
        hl.set_theme("InspiredGitHub");
        assert!(hl.row(row).is_empty(), "old colours dropped");
        hl.update(tty.term.grid());
        assert!(!hl.row(row).is_empty());
        assert_ne!(hl.row(row), dark.as_slice(), "new theme, new colours");

        let mut off = CodeHighlighter::new(false, "base16-ocean.dark");
        tty.run(&mut off, "$ cat b.py");
        tty.feed("def g(): pass\r\n$ ");
        off.update(tty.term.grid());
        assert!(off.blocks.is_empty());
    }

    #[test]
    fn output_still_streaming_waits_for_complete_lines() {
        ready();
        let mut tty = Tty::new(40, 6, 100);
        let mut hl = CodeHighlighter::new(true, "base16-ocean.dark");
        tty.run(&mut hl, "$ cat a.rs");
        tty.feed("fn main() {}");
        hl.update(tty.term.grid());
        // The cursor is still on that line: not highlighted yet.
        assert!(hl.row(tty.find("fn main")).is_empty());
        tty.feed("\r\n");
        hl.update(tty.term.grid());
        assert!(!hl.row(tty.find("fn main")).is_empty());
    }

    #[test]
    fn row_equals_ignores_trailing_blanks() {
        let term = mock_term("abc   \r\nx");
        assert!(row_equals(term.grid(), Line(0), "abc"));
        assert!(!row_equals(term.grid(), Line(0), "abd"));
        assert!(!row_equals(term.grid(), Line(0), "ab"));
    }
}
