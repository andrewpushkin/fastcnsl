//! A read-only terminal onto a stretch of history: the lines around a
//! position, wrapped at the current width, followed by the live grid's top
//! rows when it reaches them. Drawing, selection, search marks and line
//! numbers work on it as on the live terminal.

use std::io;
use std::ops::{ControlFlow, Range};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Grid, Row, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};

use super::{Item, Snapshot, line, lost_text};
use crate::highlight::{CodeHighlighter, CodeRows, LineHighlighter, Span};
use crate::lines;

/// Lines read before the first coloured one, for the state they leave the
/// highlighter in (comments, strings).
const CONTEXT: u64 = 200;

/// A row of the whole history: row `sub` (at the current width) of global
/// line `line`. Rows of the line whose start already left the grid (the
/// partial line) are numbered on from the rows in history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct At {
    pub line: u64,
    pub sub: u32,
}

/// Rows the partial line takes at `columns` in history; 0 if there is none.
pub fn partial_rows(snap: &Snapshot, columns: usize) -> usize {
    snap.partial_rows(columns)
}

/// The row of absolute live row `abs`. `partial` is [`partial_rows`].
pub fn live_at(live: &Grid<Cell>, partial: usize, abs: usize) -> At {
    let start = lines::line_start(live, abs);
    let line = lines::ends_before(live, &mut None, abs);
    let lead = if start == 0 && line == live.lines_dropped() { partial } else { 0 };
    At { line, sub: (abs - start + lead) as u32 }
}

/// The absolute live row of `at`, None if that row is in history. Rows
/// past the end of a line or of the grid give the last one.
pub fn live_row(live: &Grid<Cell>, partial: usize, at: At) -> Option<usize> {
    let dropped = live.lines_dropped();
    if at.line < dropped {
        return None;
    }
    let lead = if at.line == dropped { partial } else { 0 };
    let sub = (at.sub as usize).checked_sub(lead)?;
    let total = live.total_lines();
    let mut line = dropped;
    let mut row = 0;
    while line < at.line {
        if row + 1 >= total {
            return Some(row);
        }
        if !lines::wraps(live, row) {
            line += 1;
        }
        row += 1;
    }
    for _ in 0..sub {
        if !lines::wraps(live, row) || row + 1 >= total {
            break;
        }
        row += 1;
    }
    Some(row)
}

/// The row standing for `count` lines lost to a disk error.
fn marker_row(count: u64, columns: usize) -> Row<Cell> {
    let mut row = Row::<Cell>::new(columns);
    for (i, c) in lost_text(count).chars().take(columns).enumerate() {
        row[Column(i)].c = c;
        row[Column(i)].flags = Flags::DIM;
    }
    row
}

/// A read-only terminal onto the history around a position.
pub struct Window<L: EventListener> {
    pub term: Term<L>,
    /// The position of each row, top to bottom.
    rows: Vec<At>,
    /// It holds the live grid's rows down to its last.
    pub reaches_end: bool,
    /// The history's generation it was built from.
    pub generation: u64,
    pub columns: usize,
    pub screen: usize,
    /// The lines just before it (global line, encoded), for the state
    /// they leave a code highlighter in. Not a snapshot: that would keep
    /// history chunks in RAM after they move to disk.
    before: Vec<(u64, Vec<u8>)>,
    /// Code colours per row, `coloured` once worked out with `code_theme`.
    spans: Vec<Vec<Span>>,
    coloured: Vec<bool>,
    code_theme: Option<String>,
}

impl<L: EventListener> CodeRows for Window<L> {
    fn row(&self, abs_row: i64) -> &[Span] {
        usize::try_from(abs_row).ok().and_then(|r| self.spans.get(r)).map_or(&[], Vec::as_slice)
    }
}

impl<L: EventListener> Window<L> {
    /// Rows around `top`, about half of `max_rows` above it: history lines
    /// (a lost range as one marker row), the partial line, then the live
    /// grid's rows while there is room. `top` ends up as the top screen row,
    /// or as near the bottom as the rows allow.
    pub fn build(snap: &Snapshot, live: &Grid<Cell>, top: At, screen: usize, max_rows: usize, listener: L) -> io::Result<Self> {
        let columns = live.columns();
        let max_rows = max_rows.max(screen * 3);
        let end = snap.end_line();
        let partial_rows = partial_rows(snap, columns);

        // Where to start: up to half the window above `top`.
        let want_above = max_rows / 2;
        let (mut start_line, mut start_sub) = (top.line.min(end), top.sub as usize);
        if start_sub >= want_above {
            start_sub -= want_above;
        } else {
            let mut need = want_above - start_sub;
            start_sub = 0;
            snap.for_each(snap.first_line()..start_line, true, |line, item| {
                let rows = match item {
                    Item::Line(bytes) => line::row_count_encoded(bytes, columns),
                    Item::Lost(_) => 1,
                };
                start_line = line;
                if rows >= need {
                    start_sub = rows - need;
                    return ControlFlow::Break(());
                }
                need -= rows;
                ControlFlow::Continue(())
            })?;
        }

        // History lines from there.
        let mut rows: Vec<Row<Cell>> = Vec::with_capacity(max_rows);
        let mut ats: Vec<At> = Vec::with_capacity(max_rows);
        snap.for_each(start_line..end, false, |at_line, item| {
            let from = if at_line == start_line { start_sub } else { 0 };
            match item {
                Item::Line(bytes) => {
                    let before = rows.len();
                    line::wrap_encoded(bytes, columns, from, from + (max_rows - before), &mut rows);
                    ats.extend((0..rows.len() - before).map(|i| At { line: at_line, sub: (from + i) as u32 }));
                }
                Item::Lost(count) => {
                    rows.push(marker_row(count, columns));
                    ats.push(At { line: at_line, sub: 0 });
                }
            }
            if rows.len() >= max_rows { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })?;

        // The partial line's rows, then the live grid's.
        let mut reaches_end = false;
        if rows.len() < max_rows {
            let from = if start_line == end { start_sub.min(partial_rows) } else { 0 };
            let before = rows.len();
            if partial_rows > 0 {
                line::wrap_encoded(snap.partial(), columns, from, from + (max_rows - before), &mut rows);
                // It goes on in the live grid.
                if let Some(last) = rows.last_mut() {
                    last[Column(columns - 1)].flags.insert(Flags::WRAPLINE);
                }
            }
            ats.extend((0..rows.len() - before).map(|i| At { line: end, sub: (from + i) as u32 }));
            let total = live.total_lines();
            let (mut line, mut sub) = (end, partial_rows as u32);
            let mut abs = 0;
            while abs < total && rows.len() < max_rows {
                rows.push(live[lines::line_of(live, abs)].clone());
                ats.push(At { line, sub });
                if lines::wraps(live, abs) {
                    sub += 1;
                } else {
                    line += 1;
                    sub = 0;
                }
                abs += 1;
            }
            reaches_end = abs == total;
        }
        while rows.len() < screen {
            rows.push(Row::new(columns));
            let next = ats.last().map_or(At { line: end, sub: 0 }, |a| At { line: a.line + 1, sub: 0 });
            ats.push(next);
        }

        let first_line = ats.first().map_or(end, |a| a.line);
        let mut before = Vec::new();
        snap.for_each(first_line.saturating_sub(CONTEXT)..first_line, false, |at_line, item| {
            if let Item::Line(bytes) = item {
                before.push((at_line, bytes.to_vec()));
            }
            ControlFlow::Continue(())
        })?;
        let history = rows.len() - screen;
        let config = TermConfig { scrolling_history: history, ..TermConfig::default() };
        let mut term = Term::new(config, &TermSize::new(columns, screen), listener);
        term.grid_mut().set_content(rows, 0, first_line);
        term.unset_private_mode(PrivateMode::Named(NamedPrivateMode::ShowCursor));
        let count = ats.len();
        let mut window = Self {
            term,
            rows: ats,
            reaches_end,
            generation: snap.generation(),
            columns,
            screen,
            before,
            spans: vec![Vec::new(); count],
            coloured: vec![false; count],
            code_theme: None,
        };
        let k = window.row_of(top).unwrap_or(0);
        window.set_top(k);
        Ok(window)
    }

    /// The global line of the first row.
    pub fn first_line(&self) -> u64 {
        self.rows.first().map_or(0, |a| a.line)
    }

    /// The global line of the last row.
    pub fn last_line(&self) -> u64 {
        self.rows.last().map_or(0, |a| a.line)
    }

    /// Absolute rows of history above the screen.
    pub fn history(&self) -> usize {
        self.rows.len() - self.screen
    }

    /// The position of the top screen row.
    pub fn top(&self) -> At {
        let grid = self.term.grid();
        self.rows[grid.history_size() - grid.display_offset()]
    }

    /// The absolute row of `at` (or of the nearest row before it), None if
    /// it is outside the window.
    pub fn row_of(&self, at: At) -> Option<usize> {
        let first = *self.rows.first()?;
        let last = *self.rows.last()?;
        if at < first || (at > last && !(self.reaches_end && at.line == last.line)) {
            return None;
        }
        Some(self.rows.partition_point(|a| *a <= at).saturating_sub(1))
    }

    pub fn at_of(&self, abs: usize) -> At {
        self.rows[abs.min(self.rows.len() - 1)]
    }

    /// Scrolls so that absolute row `abs` is the top screen row, or as near
    /// as the window allows.
    pub fn set_top(&mut self, abs: usize) {
        let offset = self.history().saturating_sub(abs) as i32;
        let now = self.term.grid().display_offset() as i32;
        self.term.scroll_display(Scroll::Delta(offset - now));
    }

    /// Works out code colours for absolute rows `rows`, if not done yet
    /// with `code`'s theme.
    pub fn colour(&mut self, rows: Range<usize>, code: &CodeHighlighter) {
        if self.code_theme.as_deref() != Some(code.theme_name()) {
            self.spans.iter_mut().for_each(Vec::clear);
            self.coloured.fill(false);
            self.code_theme = Some(code.theme_name().to_owned());
        }
        let rows = rows.start.min(self.rows.len())..rows.end.min(self.rows.len());
        if rows.is_empty() || self.coloured[rows.clone()].iter().all(|&c| c) {
            return;
        }
        let first = self.rows[rows.start].line;
        let last = self.rows[rows.end - 1].line;
        let window_first = self.rows[0].line;
        let hist = self.term.grid().history_size() as i32;
        for (range, syntax) in code.code_ranges(first.saturating_sub(CONTEXT)..last + 1) {
            let Some(mut hl) = LineHighlighter::new(syntax, code.theme_name()) else { return };
            let from = range.start.max(first.saturating_sub(CONTEXT));
            // Lines before the window: their text, for context only.
            if from < window_first {
                let mut text = String::new();
                for (_, bytes) in self.before.iter().filter(|(l, _)| (from..window_first.min(range.end)).contains(l)) {
                    text.clear();
                    line::text(bytes, &mut text);
                    hl.context(&text);
                }
            }
            for at_line in from.max(window_first)..range.end.min(last + 1) {
                let lo = self.rows.partition_point(|a| a.line < at_line);
                let hi = self.rows.partition_point(|a| a.line <= at_line);
                if lo == hi {
                    continue;
                }
                let grid = self.term.grid();
                let line_rows: Vec<&Row<Cell>> = (lo..hi).map(|r| &grid[Line(r as i32 - hist)]).collect();
                for (r, spans) in (lo..hi).zip(hl.rows(&line_rows)) {
                    self.spans[r] = spans;
                    self.coloured[r] = true;
                }
            }
        }
        self.coloured[rows].fill(true);
    }

    /// Absolute row `abs` is followed by a full screen within the window.
    pub fn can_top(&self, abs: usize) -> bool {
        abs <= self.history() || self.reaches_end
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Dimensions;

    use crate::highlight::{self, CodeHighlighter, CodeRows};
    use crate::history::scroll::{At, Scroller};
    use crate::history::{Budget, CELL_BYTES, History, start};
    use crate::testterm::{Quiet, TestTerm, temp_dir};

    /// `cat a.rs` with 300 lines of Rust, line 150 a function; then filler.
    fn archived_code() -> (TestTerm, crate::history::Shared, CodeHighlighter, Vec<highlight::Span>) {
        highlight::preload_blocking();
        let (history, sink) = start(History::new(4096, temp_dir("code")).with_chunk_bytes(512));
        let mut t = TestTerm::new(40, 10, 400);
        t.term.grid_mut().set_row_sink(Box::new(sink));
        let mut code = CodeHighlighter::new(true, "base16-ocean.dark");
        t.feed("$ cat a.rs");
        let line = highlight::command_line(t.term.grid());
        code.on_enter(t.term.grid(), &line);
        t.feed("\n");
        let file: String = (0..300).map(|k| if k == 150 { "fn special() { return; }\n".to_owned() } else { format!("let x{k} = {k};\n") }).collect();
        t.feed(&file);
        code.update(t.term.grid());
        // The live highlighter's colours for the function's row.
        let grid = t.term.grid();
        let abs = (0..grid.total_lines()).find(|&a| {
            let mut s = String::new();
            crate::lines::push_row(&mut s, grid, a);
            s.starts_with("fn special")
        }).expect("on the grid");
        let live = CodeRows::row(&code, abs as i64).to_vec();
        assert!(!live.is_empty());
        t.feed("$ ls");
        let line = highlight::command_line(t.term.grid());
        code.on_enter(t.term.grid(), &line);
        t.feed("\n");
        let filler: String = (0..5000).map(|k| format!("filler {k}\n")).collect();
        t.feed(&filler);
        (t, history, code, live)
    }

    fn window_spans(t: &mut TestTerm, history: &crate::history::Shared, code: &CodeHighlighter) -> Vec<highlight::Span> {
        let budget = Budget { bytes: 300 * 4 * 40 * CELL_BYTES };
        let mut s = Scroller::new(Quiet);
        // `fn special` is line 151: the command is line 0.
        s.set_top(&mut t.term, history, &budget, At { line: 151, sub: 0 });
        let w = s.window.as_mut().expect("a window");
        let top = w.history() - w.term.grid().display_offset();
        w.colour(top..top + 10, code);
        CodeRows::row(&*w, top as i64).to_vec()
    }

    #[test]
    fn archived_code_keeps_colours() {
        let (mut t, history, code, live) = archived_code();
        assert_eq!(window_spans(&mut t, &history, &code), live);
    }

    #[test]
    fn theme_switch_recolours_window() {
        let (mut t, history, mut code, live) = archived_code();
        code.set_theme("InspiredGitHub");
        let spans = window_spans(&mut t, &history, &code);
        assert!(!spans.is_empty());
        assert_ne!(spans, live);
    }
}
