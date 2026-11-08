//! Line numbers, and where each line of a tab's text is.
//!
//! A *line* is what a program printed as one line: rows joined while they
//! wrap, so the numbers match `cat -n`. Every line gets a global index
//! counted from the start of the tab. The index holds while old rows drop
//! out of the scrollback (the vendored grid counts the lines that leave)
//! and when the window is resized and text reflows (reflow keeps lines).
//!
//! The count of lines above a row is kept at a known position near the
//! viewport, so a frame only counts the rows that moved since the last
//! one, not the whole scrollback.

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};

use crate::config::LineNumbers;

/// Digits shown in the gutter; bigger numbers show "…" and their last digits.
pub const DIGITS: usize = 5;
/// Width of the gutter in cells: the digits and a gap before the text.
pub const GUTTER_CELLS: usize = DIGITS + 1;

/// `ends` lines ended before the row with global id `row` (rows dropped
/// so far + absolute row). Only rows above it count, so it stays right
/// while those are all in the history, which programs can't change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos {
    row: u64,
    ends: u64,
}

/// Row `abs` (counted from the top of the scrollback) continues on the next row.
pub fn wraps(grid: &Grid<Cell>, abs: usize) -> bool {
    grid[line_of(grid, abs)][grid.last_column()].flags.contains(Flags::WRAPLINE)
}

/// Grid line of absolute row `abs`.
pub fn line_of(grid: &Grid<Cell>, abs: usize) -> Line {
    Line(abs as i32 - grid.history_size() as i32)
}

/// Absolute row of the cursor.
pub fn cursor_row(grid: &Grid<Cell>) -> usize {
    (grid.history_size() as i32 + grid.cursor.point.line.0).max(0) as usize
}

/// First row of the line that row `abs` belongs to.
pub fn line_start(grid: &Grid<Cell>, mut abs: usize) -> usize {
    while abs > 0 && wraps(grid, abs - 1) {
        abs -= 1;
    }
    abs
}

/// Global index of the line row `abs` belongs to: the number of lines that
/// ended above it since the tab started. Counts from `pos` when it is still
/// usable, and moves it to `abs` when the rows above that are all history.
pub fn ends_before(grid: &Grid<Cell>, pos: &mut Option<Pos>, abs: usize) -> u64 {
    let dropped = grid.rows_dropped();
    let from_top = (0, grid.lines_dropped());
    let (mut row, mut ends) = match *pos {
        Some(p) if p.row >= dropped && p.row - dropped <= grid.total_lines() as u64 => {
            let r = (p.row - dropped) as usize;
            // Counting from the top is shorter when abs is near it.
            if abs < r.abs_diff(abs) { from_top } else { (r, p.ends) }
        }
        _ => from_top,
    };
    while row < abs {
        if !wraps(grid, row) {
            ends += 1;
        }
        row += 1;
    }
    while row > abs {
        row -= 1;
        if !wraps(grid, row) {
            ends = ends.saturating_sub(1);
        }
    }
    if abs <= grid.history_size() {
        *pos = Some(Pos { row: dropped + abs as u64, ends });
    }
    ends
}

/// Appends the text of row `abs`, wide-character spacers left out.
pub fn push_row(out: &mut String, grid: &Grid<Cell>, abs: usize) {
    let row = &grid[line_of(grid, abs)];
    for col in 0..grid.columns() {
        let cell = &row[Column(col)];
        if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            out.push(cell.c);
            if let Some(zw) = cell.zerowidth() {
                out.extend(zw);
            }
        }
    }
}

/// How far to scroll the display (for `Scroll::Delta`) to bring row `abs`
/// to the middle of the screen, or as near as the scrollback allows.
pub fn scroll_to_show(grid: &Grid<Cell>, abs: usize) -> i32 {
    let hist = grid.history_size();
    let top = abs.saturating_sub(grid.screen_lines() / 2).min(hist);
    (hist - top) as i32 - grid.display_offset() as i32
}

/// The absolute row the bookmark key acts on: the cursor's at the bottom of
/// the scrollback; when scrolled back, screen row `pointer_row`, the one
/// under the mouse pointer.
pub fn key_bookmark_row(grid: &Grid<Cell>, pointer_row: usize) -> usize {
    match grid.display_offset() {
        0 => cursor_row(grid),
        offset => grid.history_size() - offset + pointer_row,
    }
}

/// First row of global line `line`, if it still is in the grid. Walks the
/// whole scrollback: this is for a key press, not for every frame.
fn first_row_of(grid: &Grid<Cell>, line: u64) -> Option<usize> {
    let mut ends = grid.lines_dropped();
    for abs in 0..=cursor_row(grid) {
        if ends == line {
            return Some(abs);
        }
        if !wraps(grid, abs) {
            ends += 1;
        }
    }
    None
}

/// The bookmark to jump to in `marks` (ascending), wrapping around at the
/// ends. Moves on from `here`, the bookmark last jumped to, while that is
/// on screen; otherwise the first or last line on screen is the start, so
/// bookmarks in view count.
fn next_bookmark(marks: &[u64], here: Option<u64>, first: u64, last: u64, down: bool) -> Option<u64> {
    if down {
        let i = match here {
            Some(h) => marks.partition_point(|&m| m <= h),
            None => marks.partition_point(|&m| m < first),
        };
        marks.get(i).or(marks.first()).copied()
    } else {
        let i = match here {
            Some(h) => marks.partition_point(|&m| m < h),
            None => marks.partition_point(|&m| m <= last),
        };
        i.checked_sub(1).and_then(|i| marks.get(i)).or(marks.last()).copied()
    }
}

/// One tab's command lines, bookmarks and its numbering position.
pub struct LineIndex {
    /// Global indices of the lines commands were entered on, ascending.
    marks: Vec<u64>,
    /// Global indices of the bookmarked lines, ascending.
    bookmarks: Vec<u64>,
    /// The bookmark last jumped to.
    jumped: Option<u64>,
    /// Position at the top of the viewport (see [`Pos`]).
    view: Option<Pos>,
}

impl LineIndex {
    pub fn new() -> Self {
        Self { marks: Vec::new(), bookmarks: Vec::new(), jumped: None, view: None }
    }

    /// Forgets the counting position; call after the grid is resized,
    /// which can move rows between the history and the screen.
    pub fn invalidate(&mut self) {
        self.view = None;
    }

    /// Records the cursor's line as a command. Call when Enter is pressed
    /// on the normal screen, before the key reaches the shell.
    pub fn on_enter(&mut self, grid: &Grid<Cell>) {
        let start = line_start(grid, cursor_row(grid));
        let line = ends_before(grid, &mut self.view, start);
        if self.marks.last().is_none_or(|&m| m < line) {
            self.marks.push(line);
        }
        // Commands whose output is gone entirely aren't needed any more.
        let gone = grid.lines_dropped();
        let unused = self.marks.partition_point(|&m| m <= gone).saturating_sub(1);
        self.marks.drain(..unused);
    }

    /// The numbers to show beside the rows on screen, top to bottom: None
    /// for rows that continue a wrapped line and, counting per command, for
    /// the command line itself. Rows below the cursor have no entry. Pass
    /// the normal screen's grid.
    pub fn numbers(&mut self, grid: &Grid<Cell>, mode: LineNumbers, out: &mut Vec<Option<u64>>) {
        out.clear();
        if !mode.is_on() {
            return;
        }
        let top = grid.history_size() - grid.display_offset();
        let last = cursor_row(grid);
        let mut line = ends_before(grid, &mut self.view, top);
        for abs in top..(top + grid.screen_lines()).min(last + 1) {
            let starts = abs == 0 || !wraps(grid, abs - 1);
            out.push(if starts { self.number(mode, line) } else { None });
            if !wraps(grid, abs) {
                line += 1;
            }
        }
    }

    /// The global line of row `abs` and whether it is bookmarked. None for
    /// rows below the cursor, which hold no line yet.
    pub fn bookmark_target(&mut self, grid: &Grid<Cell>, abs: usize) -> Option<(u64, bool)> {
        if abs > cursor_row(grid) {
            return None;
        }
        let line = ends_before(grid, &mut self.view, abs);
        Some((line, self.bookmarks.binary_search(&line).is_ok()))
    }

    /// Adds a bookmark on global line `line`, or removes the one there.
    pub fn toggle_bookmark(&mut self, line: u64) {
        match self.bookmarks.binary_search(&line) {
            Ok(i) => {
                self.bookmarks.remove(i);
            }
            Err(i) => self.bookmarks.insert(i, line),
        }
    }

    /// Forgets bookmarks whose text is gone: lines that left the scrollback
    /// and, after a terminal reset, lines past the cursor.
    fn prune_bookmarks(&mut self, grid: &Grid<Cell>) {
        if self.bookmarks.is_empty() {
            return;
        }
        let gone = grid.lines_dropped();
        let last = ends_before(grid, &mut self.view, cursor_row(grid));
        self.bookmarks.retain(|&m| m >= gone && m <= last);
    }

    pub fn has_bookmarks(&mut self, grid: &Grid<Cell>) -> bool {
        self.prune_bookmarks(grid);
        !self.bookmarks.is_empty()
    }

    /// The screen rows (top to bottom, from 0) of the bookmarked lines,
    /// every row of a wrapped line. Pass the normal screen's grid.
    pub fn bookmarked_rows(&mut self, grid: &Grid<Cell>, out: &mut Vec<usize>) {
        out.clear();
        self.prune_bookmarks(grid);
        if self.bookmarks.is_empty() {
            return;
        }
        let top = grid.history_size() - grid.display_offset();
        let mut line = ends_before(grid, &mut self.view, top);
        for abs in top..(top + grid.screen_lines()).min(cursor_row(grid) + 1) {
            if self.bookmarks.binary_search(&line).is_ok() {
                out.push(abs - top);
            }
            if !wraps(grid, abs) {
                line += 1;
            }
        }
    }

    /// Moves to the next bookmark down (or up) from what is on screen and
    /// returns its first row, or None if there are no bookmarks.
    pub fn step_bookmark(&mut self, grid: &Grid<Cell>, down: bool) -> Option<usize> {
        self.prune_bookmarks(grid);
        let top = grid.history_size() - grid.display_offset();
        let bottom = (top + grid.screen_lines() - 1).min(cursor_row(grid));
        let first = ends_before(grid, &mut self.view, top);
        let last = ends_before(grid, &mut self.view, bottom);
        let here = self.jumped.filter(|line| (first..=last).contains(line));
        let line = next_bookmark(&self.bookmarks, here, first, last, down)?;
        self.jumped = Some(line);
        first_row_of(grid, line)
    }

    /// The number shown for global line `line`.
    fn number(&self, mode: LineNumbers, line: u64) -> Option<u64> {
        match mode {
            LineNumbers::Off => None,
            LineNumbers::Tab => Some(line + 1),
            LineNumbers::Command => match self.marks.partition_point(|&m| m <= line) {
                0 => Some(line + 1),
                i => {
                    let cmd = self.marks[i - 1];
                    (line > cmd).then(|| line - cmd)
                }
            },
        }
    }
}

/// `n` right-aligned in [`DIGITS`] characters, into `buf`.
pub fn format_number(n: u64, buf: &mut String) {
    use std::fmt::Write;
    buf.clear();
    let s = n.to_string();
    if s.len() > DIGITS {
        buf.push('…');
        buf.push_str(&s[s.len() - (DIGITS - 1)..]);
    } else {
        let _ = write!(buf, "{n:>DIGITS$}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::Processor;

    struct T {
        term: Term<VoidListener>,
        parser: Processor,
        index: LineIndex,
    }

    impl T {
        fn new(cols: usize, lines: usize, history: usize) -> Self {
            let cfg = TermConfig { scrolling_history: history, ..TermConfig::default() };
            Self { term: Term::new(cfg, &TermSize::new(cols, lines), VoidListener), parser: Processor::new(), index: LineIndex::new() }
        }

        fn feed(&mut self, s: &str) {
            self.parser.advance(&mut self.term, s.replace('\n', "\r\n").as_bytes());
        }

        /// Types a command at the prompt and presses Enter.
        fn command(&mut self, cmd: &str) {
            self.feed(&format!("$ {cmd}"));
            self.index.on_enter(self.term.grid());
            self.feed("\n");
        }

        fn numbers(&mut self, mode: LineNumbers) -> Vec<Option<u64>> {
            let mut out = Vec::new();
            self.index.numbers(self.term.grid(), mode, &mut out);
            out
        }

        /// Absolute row whose text is `text`.
        fn row(&self, text: &str) -> usize {
            let grid = self.term.grid();
            (0..grid.total_lines())
                .find(|&abs| {
                    let mut s = String::new();
                    push_row(&mut s, grid, abs);
                    s.trim_end() == text
                })
                .unwrap_or_else(|| panic!("no row {text:?}"))
        }

        /// Right-click on the row showing `text`, then the menu's bookmark item.
        fn toggle(&mut self, text: &str) {
            let abs = self.row(text);
            let (line, _) = self.index.bookmark_target(self.term.grid(), abs).expect("a line to bookmark");
            self.index.toggle_bookmark(line);
        }

        /// The text of the bookmarked rows on screen.
        fn marked(&mut self) -> Vec<String> {
            let mut rows = Vec::new();
            self.index.bookmarked_rows(self.term.grid(), &mut rows);
            let grid = self.term.grid();
            let top = grid.history_size() - grid.display_offset();
            rows.iter()
                .map(|r| {
                    let mut s = String::new();
                    push_row(&mut s, grid, top + r);
                    s.trim_end().to_owned()
                })
                .collect()
        }

        /// Jumps to the next (or previous) bookmark; returns its text.
        fn jump(&mut self, down: bool) -> String {
            let abs = self.index.step_bookmark(self.term.grid(), down).expect("a bookmark");
            let delta = scroll_to_show(self.term.grid(), abs);
            self.term.scroll_display(alacritty_terminal::grid::Scroll::Delta(delta));
            let grid = self.term.grid();
            let top = grid.history_size() - grid.display_offset();
            assert!((top..top + grid.screen_lines()).contains(&abs), "row {abs} on screen after the jump");
            let mut s = String::new();
            push_row(&mut s, grid, abs);
            s.trim_end().to_owned()
        }

        fn numbered(cols: usize, screen: usize, history: usize, count: usize) -> Self {
            let mut t = Self::new(cols, screen, history);
            for i in 0..count {
                t.feed(&format!("l{i}\n"));
            }
            t
        }

        /// Screen rows with their numbers, e.g. "  3 line c".
        fn shown(&mut self, mode: LineNumbers) -> Vec<String> {
            let nums = self.numbers(mode);
            let grid = self.term.grid();
            let top = grid.history_size() - grid.display_offset();
            nums.iter()
                .enumerate()
                .map(|(i, n)| {
                    let mut text = String::new();
                    push_row(&mut text, grid, top + i);
                    let n = n.map_or(String::new(), |n| n.to_string());
                    format!("{n:>3} {}", text.trim_end()).trim_end().to_owned()
                })
                .collect()
        }
    }

    #[test]
    fn output_is_numbered_from_each_command() {
        let mut t = T::new(20, 10, 100);
        t.feed("welcome\n");
        t.command("ls");
        t.feed("a\nb\nc\n");
        t.command("pwd");
        t.feed("/home\n$ ");
        assert_eq!(
            t.shown(LineNumbers::Command),
            ["  1 welcome", "    $ ls", "  1 a", "  2 b", "  3 c", "    $ pwd", "  1 /home", "  2 $"]
        );
        let tab: Vec<_> = t.numbers(LineNumbers::Tab).into_iter().map(Option::unwrap).collect();
        assert_eq!(tab, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(t.numbers(LineNumbers::Off).is_empty());
    }

    #[test]
    fn wrapped_rows_share_their_line_number() {
        let mut t = T::new(10, 8, 100);
        t.command("cat x");
        t.feed("short\n");
        t.feed("0123456789abcdefghijXY\n");
        t.feed("end\n");
        assert_eq!(
            t.shown(LineNumbers::Command),
            ["    $ cat x", "  1 short", "  2 0123456789", "    abcdefghij", "    XY", "  3 end", "  4"]
        );
    }

    #[test]
    fn numbers_hold_when_history_overflows() {
        // 20 lines of history, 5 on screen: most of the output drops out.
        let mut t = T::new(20, 5, 20);
        t.command("seq 100");
        for i in 1..=100 {
            t.feed(&format!("{i}\n"));
            // Draw now and then, as the app does.
            if i % 7 == 0 {
                t.numbers(LineNumbers::Command);
            }
        }
        let grid = t.term.grid();
        assert!(grid.lines_dropped() > 70, "{}", grid.lines_dropped());
        assert_eq!(t.shown(LineNumbers::Command), [" 97 97", " 98 98", " 99 99", "100 100", "101"]);
        assert_eq!(t.numbers(LineNumbers::Tab), [98, 99, 100, 101, 102].map(Some));

        // Scrolled to the top of what is left.
        t.term.scroll_display(alacritty_terminal::grid::Scroll::Top);
        let first = t.shown(LineNumbers::Command)[0].clone();
        let n: u64 = first.split_whitespace().next().unwrap().parse().unwrap();
        assert_eq!(first, format!("{n:>3} {n}"), "numbers still match the text");
    }

    #[test]
    fn numbers_survive_reflow_and_clear() {
        let mut t = T::new(10, 6, 100);
        t.command("cat");
        t.feed("aaaaaaaaaaaaaaa\nb\n");
        // Wider: the long line unwraps; the numbers stay.
        t.term.resize(TermSize::new(20, 6));
        t.index.invalidate();
        assert_eq!(t.shown(LineNumbers::Command), ["    $ cat", "  1 aaaaaaaaaaaaaaa", "  2 b", "  3"]);
        // `clear` empties the screen and the history; counting goes on.
        t.command("clear");
        t.feed("\x1b[H\x1b[2J\x1b[3J");
        t.feed("x\n");
        assert_eq!(t.numbers(LineNumbers::Command), [Some(1), Some(2)]);
        assert_eq!(t.numbers(LineNumbers::Tab), [Some(5), Some(6)]);
    }

    #[test]
    fn counting_moves_with_the_viewport_and_matches_a_full_count() {
        let mut t = T::new(8, 4, 1000);
        for i in 0..300 {
            // Some lines wrap.
            t.feed(&format!("{}\n", "x".repeat(i % 13)));
        }
        let grid = t.term.grid();
        let mut pos = None;
        let total = grid.total_lines();
        assert!(total > 350, "{total}");
        for abs in [0, 50, 10, total - 1, total - 2, grid.history_size(), 3] {
            let fresh = ends_before(grid, &mut None, abs);
            assert_eq!(ends_before(grid, &mut pos, abs), fresh, "row {abs}");
        }
    }

    #[test]
    fn command_marks_are_dropped_with_their_output() {
        let mut t = T::new(20, 3, 5);
        for i in 0..50 {
            t.command(&format!("echo {i}"));
            t.feed(&format!("{i}\n"));
        }
        assert!(t.index.marks.len() <= 6, "{}", t.index.marks.len());
        assert_eq!(t.shown(LineNumbers::Command), ["    $ echo 49", "  1 49", "  2"]);
    }

    #[test]
    fn a_bookmark_stays_on_its_line_as_output_scrolls() {
        let mut t = T::numbered(20, 6, 100, 4);
        t.toggle("l2");
        assert_eq!(t.marked(), ["l2"]);
        // Pushed into the scrollback, and off the screen.
        for i in 4..30 {
            t.feed(&format!("l{i}\n"));
        }
        assert!(t.marked().is_empty());
        t.term.scroll_display(alacritty_terminal::grid::Scroll::Top);
        assert_eq!(t.marked(), ["l2"]);
    }

    #[test]
    fn a_click_on_any_row_of_a_wrapped_line_marks_the_whole_line() {
        let mut t = T::new(10, 8, 100);
        t.feed("short\n0123456789abcdefghijXY\nend\n");
        t.toggle("abcdefghij");
        assert_eq!(t.marked(), ["0123456789", "abcdefghij", "XY"]);
        // Wider: the line unwraps and is still the one marked.
        t.term.resize(TermSize::new(30, 8));
        t.index.invalidate();
        assert_eq!(t.marked(), ["0123456789abcdefghijXY"]);
    }

    #[test]
    fn the_second_toggle_removes_the_bookmark() {
        let mut t = T::numbered(20, 6, 100, 3);
        let abs = t.row("l1");
        assert_eq!(t.index.bookmark_target(t.term.grid(), abs).map(|(_, on)| on), Some(false));
        t.toggle("l1");
        assert_eq!(t.index.bookmark_target(t.term.grid(), abs).map(|(_, on)| on), Some(true));
        t.toggle("l1");
        assert!(t.marked().is_empty());
        assert!(!t.index.has_bookmarks(t.term.grid()));
    }

    #[test]
    fn rows_below_the_cursor_cannot_be_bookmarked() {
        let mut t = T::numbered(20, 8, 100, 2);
        // l0, l1, then the cursor's empty row.
        assert!(t.index.bookmark_target(t.term.grid(), 2).is_some());
        assert!(t.index.bookmark_target(t.term.grid(), 3).is_none());
    }

    #[test]
    fn at_the_bottom_the_bookmark_key_means_the_cursor_line() {
        let mut t = T::numbered(20, 6, 100, 30);
        t.feed("$ ls");
        assert_eq!(key_bookmark_row(t.term.grid(), 2), t.row("$ ls"));
    }

    #[test]
    fn scrolled_back_the_bookmark_key_means_the_row_under_the_pointer() {
        let mut t = T::numbered(20, 6, 100, 30);
        t.feed("$ ls");
        t.term.scroll_display(alacritty_terminal::grid::Scroll::Delta(10));
        let grid = t.term.grid();
        let top = grid.history_size() - grid.display_offset();
        assert_eq!(key_bookmark_row(grid, 2), top + 2);
        t.term.scroll_display(alacritty_terminal::grid::Scroll::Top);
        assert_eq!(key_bookmark_row(t.term.grid(), 2), t.row("l2"));
    }

    #[test]
    fn jumps_walk_the_bookmarks_and_wrap_around() {
        let mut t = T::numbered(20, 10, 100, 40);
        for s in ["l5", "l7", "l30"] {
            t.toggle(s);
        }
        // l5 and l7 share a screen: each press still moves to the next one.
        let ups: Vec<_> = (0..4).map(|_| t.jump(false)).collect();
        assert_eq!(ups, ["l30", "l7", "l5", "l30"]);
        let downs: Vec<_> = (0..3).map(|_| t.jump(true)).collect();
        assert_eq!(downs, ["l5", "l7", "l30"]);
    }

    #[test]
    fn after_scrolling_away_a_jump_starts_from_the_screen() {
        let mut t = T::numbered(20, 10, 100, 40);
        for s in ["l5", "l20", "l30"] {
            t.toggle(s);
        }
        assert_eq!(t.jump(false), "l30");
        t.term.scroll_display(alacritty_terminal::grid::Scroll::Top);
        assert_eq!(t.jump(false), "l5", "the one on screen, not the one before l30");
    }

    #[test]
    fn a_jump_puts_the_bookmark_mid_screen() {
        let mut t = T::numbered(20, 10, 100, 60);
        t.toggle("l20");
        t.jump(false);
        let grid = t.term.grid();
        let top = grid.history_size() - grid.display_offset();
        assert_eq!(t.row("l20") - top, 5);
    }

    #[test]
    fn a_jump_lands_on_the_first_row_of_a_wrapped_line() {
        let mut t = T::new(10, 4, 100);
        t.feed("0123456789abcdefghijXY\n");
        for i in 0..20 {
            t.feed(&format!("l{i}\n"));
        }
        t.toggle("abcdefghij");
        assert_eq!(t.jump(false), "0123456789");
    }

    #[test]
    fn bookmarks_go_with_the_text_they_mark() {
        // 20 rows of history: l0 drops out of it.
        let mut t = T::numbered(20, 5, 20, 3);
        t.toggle("l0");
        assert!(t.index.has_bookmarks(t.term.grid()));
        for i in 3..60 {
            t.feed(&format!("l{i}\n"));
        }
        assert!(!t.index.has_bookmarks(t.term.grid()));
        assert!(t.index.step_bookmark(t.term.grid(), true).is_none());
    }

    #[test]
    fn a_terminal_reset_forgets_the_lines_it_wiped() {
        let mut t = T::numbered(20, 8, 100, 5);
        t.toggle("l3");
        t.feed("\x1bc");
        assert!(!t.index.has_bookmarks(t.term.grid()));
    }

    #[test]
    fn big_numbers_keep_their_last_digits() {
        let mut b = String::new();
        format_number(7, &mut b);
        assert_eq!(b, "    7");
        format_number(99999, &mut b);
        assert_eq!(b, "99999");
        format_number(1234567, &mut b);
        assert_eq!(b, "…4567");
        assert_eq!(b.chars().count(), DIGITS);
    }
}
