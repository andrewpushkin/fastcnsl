//! Scrolling through a tab's whole history: the live grid, and a
//! [`Window`] onto the lines that left it.


use std::ops::ControlFlow;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Grid, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Cell;

pub use super::window::{At, Window};
use super::window::{live_at, live_row, partial_rows};
use super::{Budget, Item, Shared, Snapshot, line, lock, sync};

#[cfg(test)]
use super::CELL_BYTES;

/// The history with everything the grid dropped in it, as a snapshot.
pub fn settled<L: EventListener>(live: &mut Term<L>, history: &Shared) -> Snapshot {
    live.flush_row_sink();
    sync(history);
    lock(history).snapshot()
}

/// Rows of an item at `columns`.
fn item_rows(item: &Item<'_>, columns: usize, cells: &mut Vec<Cell>) -> u64 {
    match item {
        Item::Line(bytes) => {
            cells.clear();
            line::decode(bytes, cells);
            line::row_count(cells, columns) as u64
        }
        Item::Lost(_) => 1,
    }
}

/// `n` rows above `at`, which is in history or the live grid's first row.
fn up(snap: &Snapshot, columns: usize, at: At, n: u64) -> At {
    if u64::from(at.sub) >= n {
        return At { line: at.line, sub: at.sub - n as u32 };
    }
    let mut need = n - u64::from(at.sub);
    let mut result = At { line: at.line, sub: 0 };
    let mut cells = Vec::new();
    let _ = snap.for_each(snap.first_line()..at.line, true, |line, item| {
        let rows = item_rows(&item, columns, &mut cells);
        if rows >= need {
            result = At { line, sub: (rows - need) as u32 };
            return ControlFlow::Break(());
        }
        need -= rows;
        result = At { line, sub: 0 };
        ControlFlow::Continue(())
    });
    result
}

/// `n` rows below `at`, which is in history.
fn down(snap: &Snapshot, live: &Grid<Cell>, partial: usize, at: At, n: u64) -> At {
    let columns = live.columns();
    let end = snap.end_line();
    let mut sub = u64::from(at.sub) + n;
    let mut result = None;
    let mut cells = Vec::new();
    let _ = snap.for_each(at.line..end, false, |line, item| {
        let rows = item_rows(&item, columns, &mut cells);
        if sub < rows {
            result = Some(At { line, sub: sub as u32 });
            return ControlFlow::Break(());
        }
        sub -= rows;
        ControlFlow::Continue(())
    });
    if let Some(at) = result {
        return at;
    }
    if (sub as usize) < partial {
        return At { line: end, sub: sub as u32 };
    }
    let row = (sub as usize - partial).min(live.total_lines() - 1);
    live_at(live, partial, row)
}

/// `rows` rows from `at`: positive is up, towards older lines.
fn step(snap: &Snapshot, live: &Grid<Cell>, partial: usize, at: At, rows: i64) -> At {
    let columns = live.columns();
    if let Some(r) = live_row(live, partial, at) {
        let target = r as i64 - rows;
        if target >= 0 {
            return live_at(live, partial, (target as usize).min(live.total_lines() - 1));
        }
        return up(snap, columns, live_at(live, partial, 0), (-target) as u64);
    }
    if rows >= 0 { up(snap, columns, at, rows as u64) } else { down(snap, live, partial, at, (-rows) as u64) }
}

/// A selection by position in the history: type, start and end (row and
/// column), so it can move between terminals.
type Carried = (SelectionType, (At, usize), (At, usize));

fn take_selection<L: EventListener>(term: &Term<L>, at_of: impl Fn(usize) -> At) -> Option<Carried> {
    let sel = term.selection.as_ref()?;
    let range = sel.to_range(term)?;
    let hist = term.grid().history_size() as i32;
    let abs = |p: Point| (p.line.0 + hist).max(0) as usize;
    Some((sel.ty, (at_of(abs(range.start)), range.start.column.0), (at_of(abs(range.end)), range.end.column.0)))
}

/// Where a carried selection lands in a terminal whose rows go from
/// `first` to `last`: its start and end rows and columns, clipped to them.
fn landing(carried: &Carried, first: At, last: At, total: usize, columns: usize, row_of: impl Fn(At) -> Option<usize>) -> Option<((usize, usize), (usize, usize))> {
    let (_, (start, start_col), (end, end_col)) = *carried;
    if end < first || start > last {
        return None;
    }
    let start = if start < first { (0, 0) } else { (row_of(start).unwrap_or(0), start_col) };
    let end = if end > last { (total - 1, columns - 1) } else { (row_of(end).unwrap_or(total - 1), end_col) };
    Some((start, end))
}

/// Puts a selection of type `ty` from `start` to `end` (rows, columns).
fn put_selection<L: EventListener>(term: &mut Term<L>, ty: SelectionType, start: (usize, usize), end: (usize, usize)) {
    let hist = term.grid().history_size() as i32;
    let last_col = term.grid().columns() - 1;
    let point = |(row, col): (usize, usize)| Point::new(Line(row as i32 - hist), Column(col.min(last_col)));
    let mut sel = Selection::new(ty, point(start), Side::Left);
    sel.update(point(end), Side::Right);
    term.selection = Some(sel);
}

/// What a tab shows: the live terminal, or a window onto older lines.
pub struct Scroller<L: EventListener> {
    pub window: Option<Window<L>>,
    listener: L,
    /// Goes up whenever a window is built or dropped.
    changes: u64,
}

impl<L: EventListener + Clone> Scroller<L> {
    pub fn new(listener: L) -> Self {
        Self { window: None, listener, changes: 0 }
    }

    pub fn in_window(&self) -> bool {
        self.window.is_some()
    }

    /// Changes whenever the terminal shown changes, so positions cached
    /// for it (line numbers) are known to be stale.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    pub fn shown<'a>(&'a self, live: &'a Term<L>) -> &'a Term<L> {
        self.window.as_ref().map_or(live, |w| &w.term)
    }

    pub fn shown_mut<'a>(&'a mut self, live: &'a mut Term<L>) -> &'a mut Term<L> {
        match self.window.as_mut() {
            Some(w) => &mut w.term,
            None => live,
        }
    }

    fn top_in(&self, snap: &Snapshot, live: &Grid<Cell>) -> At {
        match &self.window {
            Some(w) => w.top(),
            None => {
                let abs = live.history_size() - live.display_offset();
                live_at(live, partial_rows(snap, live.columns()), abs)
            }
        }
    }

    /// The position of the top screen row.
    pub fn top(&self, live: &mut Term<L>, history: &Shared) -> At {
        let snap = settled(live, history);
        self.top_in(&snap, live.grid())
    }

    /// Shows `target` as the top screen row (or as near as the history
    /// allows): in the live terminal when it is there, otherwise in a
    /// window, built anew when the current one doesn't hold it.
    fn place(&mut self, live: &mut Term<L>, snap: &Snapshot, budget: &Budget, target: At) {
        let columns = live.grid().columns();
        let screen = live.grid().screen_lines();
        let partial = partial_rows(snap, columns);
        if let Some(row) = live_row(live.grid(), partial, target) {
            if let Some(w) = self.window.take() {
                let g = live.grid();
                let landed = take_selection(&w.term, |abs| w.at_of(abs)).and_then(|c| {
                    let (first, last) = (live_at(g, partial, 0), live_at(g, partial, g.total_lines() - 1));
                    landing(&c, first, last, g.total_lines(), columns, |at| live_row(g, partial, at)).map(|l| (c.0, l))
                });
                if let Some((ty, (start, end))) = landed {
                    put_selection(live, ty, start, end);
                }
                self.changes += 1;
            }
            let offset = live.grid().history_size().saturating_sub(row) as i32;
            let now = live.grid().display_offset() as i32;
            live.scroll_display(Scroll::Delta(offset - now));
            return;
        }
        if let Some(w) = self.window.as_mut()
            && w.generation == snap.generation()
            && w.columns == columns
            && w.screen == screen
            && let Some(k) = w.row_of(target)
            && w.can_top(k)
        {
            w.set_top(k);
            return;
        }
        match Window::build(snap, live.grid(), target, screen, budget.window_rows(columns), self.listener.clone()) {
            Ok(mut new) => {
                let carried = match &self.window {
                    Some(w) => take_selection(&w.term, |abs| w.at_of(abs)),
                    None => {
                        let g = live.grid();
                        take_selection(live, |abs| live_at(g, partial, abs))
                    }
                };
                let total = new.term.grid().total_lines();
                let (first, last) = (new.at_of(0), new.at_of(total - 1));
                if let Some(c) = carried
                    && let Some((start, end)) = landing(&c, first, last, total, columns, |at| new.row_of(at))
                {
                    put_selection(&mut new.term, c.0, start, end);
                }
                if self.window.is_none() {
                    live.selection = None;
                }
                self.window = Some(new);
                self.changes += 1;
            }
            Err(e) => {
                log::warn!("reading history: {e}");
                self.to_bottom(live);
            }
        }
    }

    /// Moves the view `rows` rows: positive is up, towards older lines.
    pub fn scroll(&mut self, live: &mut Term<L>, history: &Shared, budget: &Budget, rows: i64) {
        let snap = settled(live, history);
        let partial = partial_rows(&snap, live.grid().columns());
        let top = self.top_in(&snap, live.grid());
        let target = step(&snap, live.grid(), partial, top, rows);
        self.place(live, &snap, budget, target);
    }

    pub fn page(&mut self, live: &mut Term<L>, history: &Shared, budget: &Budget, up: bool) {
        let rows = live.grid().screen_lines() as i64;
        self.scroll(live, history, budget, if up { rows } else { -rows });
    }

    pub fn to_top(&mut self, live: &mut Term<L>, history: &Shared, budget: &Budget) {
        let snap = settled(live, history);
        if snap.first_line() < snap.end_line() || !snap.partial().is_empty() {
            self.place(live, &snap, budget, At { line: snap.first_line(), sub: 0 });
        } else {
            live.scroll_display(Scroll::Top);
        }
    }

    /// Back to the live terminal, at the bottom.
    pub fn to_bottom(&mut self, live: &mut Term<L>) {
        if self.window.take().is_some() {
            self.changes += 1;
        }
        live.scroll_display(Scroll::Bottom);
    }

    /// Shows `at` as the top screen row, or as near as the history allows.
    pub fn set_top(&mut self, live: &mut Term<L>, history: &Shared, budget: &Budget, at: At) {
        let snap = settled(live, history);
        self.place(live, &snap, budget, at);
    }

    /// Puts global line `line` mid-screen (search and bookmark jumps).
    pub fn show_line(&mut self, live: &mut Term<L>, history: &Shared, budget: &Budget, line: u64) {
        let snap = settled(live, history);
        let partial = partial_rows(&snap, live.grid().columns());
        let half = live.grid().screen_lines() as i64 / 2;
        let target = step(&snap, live.grid(), partial, At { line, sub: 0 }, half);
        self.place(live, &snap, budget, target);
    }

    /// For the scrollbar: rows of history above the screen and how far up
    /// the view is, counting each line in history as one row.
    pub fn metrics(&self, live: &Term<L>, history: &Shared) -> (usize, usize) {
        let (first, end) = {
            let h = lock(history);
            (h.first_line(), h.end_line())
        };
        let hist = live.grid().history_size();
        let total = (end - first) as usize + hist;
        let offset = match &self.window {
            Some(w) => hist + (end.saturating_sub(w.top().line) as usize).max(1),
            None => live.grid().display_offset(),
        };
        (total, offset.min(total))
    }

    /// Scrollbar drag: `offset` in the units of [`Self::metrics`].
    pub fn set_offset(&mut self, live: &mut Term<L>, history: &Shared, budget: &Budget, offset: usize) {
        let snap = settled(live, history);
        let hist = live.grid().history_size();
        if offset <= hist {
            let partial = partial_rows(&snap, live.grid().columns());
            let at = live_at(live.grid(), partial, hist - offset);
            self.place(live, &snap, budget, at);
        } else {
            let line = snap.end_line().saturating_sub((offset - hist) as u64).max(snap.first_line());
            self.place(live, &snap, budget, At { line, sub: 0 });
        }
    }

    /// Once a frame: a window over a cleared history goes, one built for
    /// another size is built again at the same line.
    pub fn sync(&mut self, live: &mut Term<L>, history: &Shared, budget: &Budget) {
        let Some(w) = &self.window else { return };
        let snap = settled(live, history);
        if w.generation != snap.generation() {
            self.to_bottom(live);
        } else if w.columns != live.grid().columns() || w.screen != live.grid().screen_lines() {
            let top = At { line: w.top().line, sub: 0 };
            self.window = None;
            self.changes += 1;
            self.place(live, &snap, budget, top);
        }
    }
}

#[cfg(test)]
mod tests {
        use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point, Side};
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::Term;

    use super::*;
    use crate::history::{History, Shared, start};
    use crate::testterm::{Quiet, TestTerm, temp_dir};

    const COLS: usize = 20;
    const SCREEN: usize = 10;

    struct T {
        t: TestTerm,
        history: Shared,
        budget: Budget,
        s: Scroller<Quiet>,
    }

    fn numbered(i: usize) -> String {
        if i % 7 == 3 { format!("line {i} {}", "x".repeat(45)) } else { format!("line {i}") }
    }

    impl T {
        /// A 20×10 terminal with 50 rows of live history and windows of 200 rows.
        fn new(tag: &str) -> Self {
            let (history, sink) = start(History::new(4096, temp_dir(tag)).with_chunk_bytes(512));
            let mut t = TestTerm::new(COLS, SCREEN, 50);
            t.term.grid_mut().set_row_sink(Box::new(sink));
            Self { t, history, budget: Budget { bytes: 200 * 4 * COLS * CELL_BYTES }, s: Scroller::new(Quiet) }
        }

        fn feed_numbered(&mut self, from: usize, to: usize) {
            let mut s = String::new();
            for i in from..to {
                s.push_str(&numbered(i));
                s.push('\n');
            }
            self.t.feed(&s);
        }

        fn scroll(&mut self, rows: i64) {
            self.s.scroll(&mut self.t.term, &self.history, &self.budget, rows);
        }

        fn top(&mut self) -> At {
            self.s.top(&mut self.t.term, &self.history)
        }

        fn shown(&self) -> &Term<Quiet> {
            self.s.shown(&self.t.term)
        }

        /// Text of screen row `row`.
        fn row_text(&self, row: usize) -> String {
            let term = self.shown();
            let grid = term.grid();
            let line = Line(row as i32 - grid.display_offset() as i32);
            (0..grid.columns()).map(|c| grid[line][Column(c)].c).collect::<String>().trim_end().to_owned()
        }

        fn screen(&self) -> Vec<String> {
            (0..SCREEN).map(|r| self.row_text(r)).collect()
        }
    }

    /// Joins (At, row text) pairs into lines, checking each line's rows
    /// come in order from its first.
    fn join(rows: &[(At, String)]) -> Vec<(u64, String)> {
        let mut out: Vec<(u64, String)> = Vec::new();
        let mut last: Option<At> = None;
        for (at, text) in rows {
            match last {
                Some(l) if l.line == at.line => {
                    assert_eq!(at.sub, l.sub + 1, "rows of line {} in order", at.line);
                    out.last_mut().unwrap().1.push_str(text);
                }
                _ => {
                    assert_eq!(at.sub, 0, "line {} starts at its first row", at.line);
                    out.push((at.line, text.clone()));
                }
            }
            last = Some(*at);
        }
        out
    }

    #[test]
    fn scrolling_up_walks_every_line_once() {
        let mut t = T::new("walk");
        t.feed_numbered(0, 3000);
        let mut rows = vec![(t.top(), t.row_text(0))];
        loop {
            t.scroll(1);
            let top = t.top();
            if top == rows.last().unwrap().0 {
                break;
            }
            rows.push((top, t.row_text(0)));
        }
        rows.reverse();
        let lines = join(&rows);
        assert_eq!(lines[0].0, 0);
        for (i, (line, text)) in lines.iter().enumerate() {
            assert_eq!(*line, i as u64);
            assert_eq!(text.trim_end(), numbered(i), "line {i}");
        }
        assert!(lines.len() > 2900);
    }

    #[test]
    fn page_and_top_and_bottom() {
        let mut t = T::new("page");
        t.feed_numbered(0, 2000);
        t.s.to_top(&mut t.t.term, &t.history, &t.budget);
        assert_eq!(t.row_text(0), "line 0");
        assert!(t.s.in_window());
        t.s.page(&mut t.t.term, &t.history, &t.budget, false);
        assert_eq!(t.top(), At { line: 8, sub: 0 }, "line 3 wraps to 3 rows: a page of 10 rows ends before line 8");
        t.s.to_bottom(&mut t.t.term);
        assert!(!t.s.in_window());
        assert_eq!(t.t.term.grid().display_offset(), 0);
    }

    #[test]
    fn crossing_into_live_drops_window() {
        let mut t = T::new("cross");
        t.feed_numbered(0, 2000);
        while !t.s.in_window() {
            t.scroll(1);
        }
        let shown = t.row_text(1);
        t.scroll(-1);
        assert!(!t.s.in_window());
        assert_eq!(t.row_text(0), shown);
    }

    #[test]
    fn show_line_centres_a_disk_line() {
        let mut t = T::new("show");
        t.feed_numbered(0, 3000);
        t.s.show_line(&mut t.t.term, &t.history, &t.budget, 12);
        assert!(t.s.in_window());
        assert_eq!(t.row_text(SCREEN / 2), "line 12");
    }

    #[test]
    fn metrics_cover_history() {
        let mut t = T::new("metrics");
        t.feed_numbered(0, 3000);
        let (end, first) = {
            t.t.term.flush_row_sink();
            crate::history::sync(&t.history);
            let h = crate::history::lock(&t.history);
            (h.end_line(), h.first_line())
        };
        let (total, offset) = t.s.metrics(&t.t.term, &t.history);
        assert_eq!(total, (end - first) as usize + t.t.term.grid().history_size());
        assert_eq!(offset, 0);
        t.s.to_top(&mut t.t.term, &t.history, &t.budget);
        let mut last = t.s.metrics(&t.t.term, &t.history).1;
        assert_eq!(last, total);
        for _ in 0..500 {
            t.scroll(-7);
            let now = t.s.metrics(&t.t.term, &t.history).1;
            assert!(now <= last, "{now} after {last}");
            last = now;
        }
    }

    #[test]
    fn set_offset_round_trips() {
        let mut t = T::new("offset");
        t.feed_numbered(0, 3000);
        let total = t.s.metrics(&t.t.term, &t.history).0;
        for k in 0..10 {
            let o = total * k / 10;
            t.s.set_offset(&mut t.t.term, &t.history, &t.budget, o);
            let got = t.s.metrics(&t.t.term, &t.history).1;
            assert!(got.abs_diff(o) <= 2, "set {o}, got {got}");
        }
    }

    #[test]
    fn resize_in_window_keeps_top_line() {
        let mut t = T::new("resize");
        t.feed_numbered(0, 3000);
        t.s.set_top(&mut t.t.term, &t.history, &t.budget, At { line: 1234, sub: 0 });
        assert!(t.s.in_window());
        assert_eq!(t.row_text(0), "line 1234");
        t.t.resize(37, SCREEN);
        t.s.sync(&mut t.t.term, &t.history, &t.budget);
        assert_eq!(t.top(), At { line: 1234, sub: 0 });
        assert_eq!(t.row_text(0), "line 1234");
        assert_eq!(t.shown().grid().columns(), 37);
    }

    #[test]
    fn output_while_in_window_keeps_view_and_bottom_is_complete() {
        let mut t = T::new("stream");
        t.feed_numbered(0, 3000);
        t.s.set_top(&mut t.t.term, &t.history, &t.budget, At { line: 2500, sub: 0 });
        let before = t.screen();
        t.feed_numbered(3000, 6000);
        t.s.sync(&mut t.t.term, &t.history, &t.budget);
        assert_eq!(t.screen(), before);
        let mut rows = vec![(t.top(), t.row_text(0))];
        loop {
            t.scroll(-1);
            let top = t.top();
            if top == rows.last().unwrap().0 {
                break;
            }
            rows.push((top, t.row_text(0)));
        }
        assert!(!t.s.in_window());
        let lines = join(&rows);
        for (k, (line, text)) in lines.iter().enumerate() {
            assert_eq!(*line, 2500 + k as u64);
            assert_eq!(text.trim_end(), numbered(2500 + k));
        }
        assert!(lines.last().unwrap().0 > 5980);
    }

    #[test]
    fn window_starts_mid_line() {
        let mut t = T::new("midline");
        let giant: String = (0..200_000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        t.t.feed(&giant);
        t.t.feed("\n");
        t.feed_numbered(0, 500);
        t.s.set_top(&mut t.t.term, &t.history, &t.budget, At { line: 0, sub: 5000 });
        assert_eq!(t.row_text(0), giant[5000 * COLS..5001 * COLS]);
        t.scroll(-5);
        assert_eq!(t.top(), At { line: 0, sub: 5005 });
        for r in 0..SCREEN {
            let at = 5005 + r;
            assert_eq!(t.row_text(r), giant[at * COLS..(at + 1) * COLS], "row {r}");
        }
    }

    #[test]
    fn clear_leaves_window() {
        let mut t = T::new("clear");
        t.feed_numbered(0, 3000);
        t.s.to_top(&mut t.t.term, &t.history, &t.budget);
        assert!(t.s.in_window());
        t.t.feed("\x1b[3J");
        t.s.sync(&mut t.t.term, &t.history, &t.budget);
        assert!(!t.s.in_window());
    }

    #[test]
    fn selection_survives_rebuild() {
        let mut t = T::new("select");
        t.feed_numbered(0, 3000);
        t.s.set_top(&mut t.t.term, &t.history, &t.budget, At { line: 1500, sub: 0 });
        {
            let term = t.s.shown_mut(&mut t.t.term);
            let top = Line(-(term.grid().display_offset() as i32));
            let mut sel = Selection::new(SelectionType::Simple, Point::new(top, Column(0)), Side::Left);
            sel.update(Point::new(top + 2, Column(COLS - 1)), Side::Right);
            term.selection = Some(sel);
        }
        let text = t.shown().selection_to_string().unwrap();
        assert!(text.starts_with("line 1500"), "{text}");
        // Past what the window holds below its top (100 rows, less a
        // screen): it is built again around the new top.
        let changes = t.s.changes();
        t.scroll(-95);
        assert!(t.s.in_window());
        assert_ne!(t.s.changes(), changes, "rebuilt");
        assert_eq!(t.shown().selection_to_string().as_deref(), Some(text.as_str()));
    }
}
