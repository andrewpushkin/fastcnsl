//! A read-only terminal onto a stretch of history: the lines around a
//! position, wrapped at the current width, followed by the live grid's top
//! rows when it reaches them. Drawing, selection, search marks and line
//! numbers work on it as on the live terminal.

use std::io;
use std::ops::ControlFlow;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Grid, Row, Scroll};
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};

use super::{Item, Snapshot, line, lost_text};
use crate::lines;

/// A row of the whole history: row `sub` (at the current width) of global
/// line `line`. Rows of the line whose start already left the grid (the
/// partial line) are numbered on from the rows in history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct At {
    pub line: u64,
    pub sub: u32,
}

/// Cells of the partial line (see `History::partial`).
pub fn partial_cells(snap: &Snapshot) -> Vec<Cell> {
    let mut cells = Vec::new();
    line::decode(snap.partial(), &mut cells);
    cells
}

/// Rows the partial line takes at `columns` in history; 0 if there is none.
pub fn partial_rows(snap: &Snapshot, columns: usize) -> usize {
    if snap.partial().is_empty() { 0 } else { line::row_count(&partial_cells(snap), columns) }
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
        let partial = partial_cells(snap);
        let partial_rows = if snap.partial().is_empty() { 0 } else { line::row_count(&partial, columns) };
        let mut cells = Vec::new();

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
                    Item::Line(bytes) => {
                        cells.clear();
                        line::decode(bytes, &mut cells);
                        line::row_count(&cells, columns)
                    }
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
                    cells.clear();
                    line::decode(bytes, &mut cells);
                    let before = rows.len();
                    line::wrap(&cells, columns, from, from + (max_rows - before), &mut rows);
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
                line::wrap(&partial, columns, from, from + (max_rows - before), &mut rows);
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
        let history = rows.len() - screen;
        let config = TermConfig { scrolling_history: history, ..TermConfig::default() };
        let mut term = Term::new(config, &TermSize::new(columns, screen), listener);
        term.grid_mut().set_content(rows, 0, first_line);
        term.unset_private_mode(PrivateMode::Named(NamedPrivateMode::ShowCursor));
        let mut window = Self { term, rows: ats, reaches_end, generation: snap.generation(), columns, screen };
        let k = window.row_of(top).unwrap_or(0);
        window.set_top(k);
        Ok(window)
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

    /// Absolute row `abs` is followed by a full screen within the window.
    pub fn can_top(&self, abs: usize) -> bool {
        abs <= self.history() || self.reaches_end
    }
}
