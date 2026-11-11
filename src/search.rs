//! Find-in-terminal. All matches on screen are highlighted; Enter and
//! Shift+Enter jump between matches through the whole scrollback.
//! Search is literal and smart-case (lower-case query ignores case);
//! Ctrl+R in the search bar switches to regular expressions.
//!
//! Several searches can run at once, one per box, each highlighting its
//! matches in its own colour. Typing and match navigation go to the
//! focused box.
//!
//! Searching works on the terminal shown (the live one, or a window onto
//! older lines). When that has no match in the direction stepped,
//! [`Search::follow_up`] goes on in the rest of the tab's history (see
//! `crate::history::search`), in order, before going round. Counts add the
//! history's matches to the live terminal's.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

use crate::history::scroll::{At, Scroller, settled};
use crate::history::search::{Found, Kind, Searcher};
use crate::history::{Budget, Shared, Snapshot, lock};
use crate::theme::SEARCH_COLORS;

/// Counting every match in a huge scrollback is bounded.
const COUNT_CAP: usize = 9999;
/// Recount at most this often while output is streaming.
const RECOUNT_EVERY: Duration = Duration::from_millis(250);
/// Most search boxes open at once.
pub const MAX_BOXES: usize = SEARCH_COLORS;

/// One search: its query, its matches and its highlight colour.
pub struct SearchBox {
    pub query: String,
    pub regex_mode: bool,
    /// The query is not a valid regex.
    pub invalid: bool,
    regex: Option<RegexSearch>,
    /// Matches in the viewport, in order. Refreshed every frame.
    pub visible: Vec<Match>,
    pub current: Option<Match>,
    pub total: usize,
    /// 1-based index of `current` among all matches.
    pub index: Option<usize>,
    /// Highlight colour, an index into Palette::search_colors.
    pub color: usize,
    count_dirty: bool,
    last_count: Option<Instant>,
    /// Names the box to the history searcher; never changes.
    id: u64,
    /// Goes up when the query changes.
    query_gen: u64,
    /// The last step in the terminal shown: its direction, and whether it
    /// found a match without going round.
    stepped: Option<(Direction, bool)>,
    /// Where that step goes on looking, beyond the terminal shown.
    segments: VecDeque<Segment>,
    step_dir: Direction,
    /// Matches in the live terminal, and the current one's place among them.
    live_total: usize,
    live_index: Option<usize>,
    /// The current match is in the live terminal.
    current_live: bool,
    /// Matches in history: counted for the query `hist_gen`, in history
    /// generation `.0` up to line `.1`.
    hist_total: usize,
    hist_capped: bool,
    hist_counted: Option<(u64, u64)>,
    hist_gen: u64,
    last_hist_count: Option<Instant>,
    /// History is still being counted.
    pub counting: bool,
}

/// Where a step goes on looking: lines in history (`up` from the end of
/// the range, or down from its start), or the live terminal from its
/// bottom (`up`) or top.
#[derive(Debug, Clone, PartialEq)]
enum Segment {
    History(Range<u64>, bool),
    Live(bool),
}

/// The tab's terminals and history, for searching beyond the terminal shown.
pub struct Across<'a, L: EventListener> {
    pub live: &'a mut Term<L>,
    pub scroller: &'a mut Scroller<L>,
    pub history: &'a Shared,
    pub budget: Budget,
    pub searcher: &'a mut Searcher,
}

impl SearchBox {
    fn new(color: usize) -> Self {
        static IDS: AtomicU64 = AtomicU64::new(0);
        Self {
            id: IDS.fetch_add(1, Ordering::Relaxed),
            query_gen: 0,
            stepped: None,
            segments: VecDeque::new(),
            step_dir: Direction::Left,
            live_total: 0,
            live_index: None,
            current_live: true,
            hist_total: 0,
            hist_capped: false,
            hist_counted: None,
            hist_gen: 0,
            last_hist_count: None,
            counting: false,
            query: String::new(),
            regex_mode: false,
            invalid: false,
            regex: None,
            visible: Vec::new(),
            current: None,
            total: 0,
            index: None,
            color,
            count_dirty: false,
            last_count: None,
        }
    }

    pub fn total_capped(&self) -> bool {
        self.total >= COUNT_CAP || self.hist_capped
    }

    /// The pattern searched for, None without a valid query.
    fn pattern(&self) -> Option<String> {
        self.regex.as_ref()?;
        Some(if self.regex_mode { self.query.clone() } else { escape(&self.query) })
    }

    /// Total and position from the history's and the live terminal's counts.
    fn combine(&mut self) {
        self.total = (self.hist_total + self.live_total).min(COUNT_CAP);
        self.index = match self.live_index {
            Some(i) if self.current_live && !self.hist_capped && self.hist_total + i <= COUNT_CAP => Some(self.hist_total + i),
            _ => None,
        };
    }

    fn set_query<T: EventListener>(&mut self, query: String, term: &mut Term<T>) {
        self.query = query;
        self.rebuild();
        self.current = None;
        // Like a browser: jump to the nearest match above the bottom of the screen.
        self.step(term, Direction::Left);
        self.count(term);
    }

    /// Runs the query again without moving to a match.
    fn rerun<T>(&mut self, term: &Term<T>) {
        self.rebuild();
        self.current = None;
        self.count(term);
    }

    fn rebuild(&mut self) {
        self.invalid = false;
        self.regex = None;
        self.query_gen += 1;
        self.hist_total = 0;
        self.hist_capped = false;
        self.hist_counted = None;
        self.segments.clear();
        if self.query.is_empty() {
            return;
        }
        let pattern = if self.regex_mode { self.query.clone() } else { escape(&self.query) };
        match RegexSearch::new(&pattern) {
            Ok(r) => self.regex = Some(r),
            Err(_) => self.invalid = true,
        }
    }

    fn step<T: EventListener>(&mut self, term: &mut Term<T>, dir: Direction) {
        let Some(regex) = self.regex.as_mut() else { return };
        let origin = match (&self.current, dir) {
            (Some(m), Direction::Right) => m.end().add(term, Boundary::None, 1),
            (Some(m), Direction::Left) => m.start().sub(term, Boundary::None, 1),
            (None, _) => {
                let offset = term.grid().display_offset() as i32;
                let bottom = Line(term.screen_lines() as i32 - 1 - offset);
                Point::new(bottom, term.last_column())
            }
        };
        let side = match dir {
            Direction::Right => Side::Left,
            Direction::Left => Side::Right,
        };
        match term.search_next(regex, origin, dir, side, None) {
            Some(m) => {
                // Ahead of the origin, or found by going round the end.
                let ahead = match dir {
                    Direction::Right => *m.start() >= origin,
                    Direction::Left => *m.end() <= origin,
                };
                self.stepped = Some((dir, ahead));
                term.scroll_to_point(*m.start());
                self.current = Some(m);
                self.count(term);
            }
            None => self.stepped = Some((dir, false)),
        }
    }

    /// Matches on the screen of `shown`; the live terminal's count when due.
    fn refresh<T>(&mut self, shown: &Term<T>, live: &Term<T>) {
        self.visible.clear();
        let Some(regex) = self.regex.as_mut() else { return };
        let offset = shown.grid().display_offset() as i32;
        let start = Point::new(Line(-offset), Column(0));
        let end = Point::new(Line(shown.screen_lines() as i32 - 1 - offset), shown.last_column());
        self.visible.extend(RegexIter::new(start, end, Direction::Right, shown, regex));

        let due = self.last_count.is_none_or(|t| t.elapsed() >= RECOUNT_EVERY);
        if self.count_dirty && due {
            self.count(live);
        }
    }

    /// Counts the matches in `term`, the live terminal.
    fn count<T>(&mut self, term: &Term<T>) {
        self.count_dirty = false;
        self.last_count = Some(Instant::now());
        self.live_total = 0;
        self.live_index = None;
        self.combine();
        let Some(regex) = self.regex.as_mut() else { return };
        let start = Point::new(term.topmost_line(), Column(0));
        let end = Point::new(term.bottommost_line(), term.last_column());
        let current_start = self.current.as_ref().map(|m| *m.start());
        for m in RegexIter::new(start, end, Direction::Right, term, regex) {
            self.live_total += 1;
            if Some(*m.start()) == current_start {
                self.live_index = Some(self.live_total);
            }
            if self.live_total >= COUNT_CAP {
                break;
            }
        }
        self.combine();
    }

    /// Goes on looking along the segments left, after the terminal shown
    /// had no match: a history segment starts a search and waits for it.
    fn run_segments<L: EventListener + Clone>(&mut self, a: &mut Across<'_, L>, snap: &Snapshot) {
        let Some(pattern) = self.pattern() else { return };
        while let Some(segment) = self.segments.pop_front() {
            match segment {
                Segment::History(range, up) => {
                    if !range.is_empty() {
                        a.searcher.step(snap.clone(), &pattern, range, a.live.columns(), up, self.id);
                        return;
                    }
                }
                Segment::Live(up) => {
                    let Some(regex) = self.regex.as_mut() else { return };
                    let (origin, dir, side) = if up {
                        (Point::new(a.live.bottommost_line(), a.live.last_column()), Direction::Left, Side::Right)
                    } else {
                        (Point::new(a.live.topmost_line(), Column(0)), Direction::Right, Side::Left)
                    };
                    if let Some(m) = a.live.search_next(regex, origin, dir, side, None) {
                        a.scroller.go_bottom(a.live);
                        a.live.scroll_to_point(*m.start());
                        self.current = Some(m);
                        self.current_live = true;
                        self.count(a.live);
                        return;
                    }
                }
            }
        }
    }

    /// A history search found a match on global line `line`: shows it and
    /// makes the match there the current one.
    fn land<L: EventListener + Clone>(&mut self, a: &mut Across<'_, L>, line: u64) {
        a.scroller.show_line(a.live, a.history, &a.budget, line);
        let up = self.step_dir == Direction::Left;
        let Some(regex) = self.regex.as_mut() else { return };
        let Some(w) = a.scroller.window.as_mut() else { return };
        let Some(row) = w.row_of(At { line, sub: if up { u32::MAX } else { 0 } }) else { return };
        let term = &mut w.term;
        let hist = term.grid().history_size() as i32;
        let (origin, side) = if up {
            (Point::new(Line(row as i32 - hist), term.last_column()), Side::Right)
        } else {
            (Point::new(Line(row as i32 - hist), Column(0)), Side::Left)
        };
        if let Some(m) = term.search_next(regex, origin, self.step_dir, side, None) {
            term.scroll_to_point(*m.start());
            self.current = Some(m);
            self.current_live = false;
            self.combine();
        }
    }
}

pub struct Search {
    pub open: bool,
    pub focused: bool,
    /// Never empty.
    pub boxes: Vec<SearchBox>,
    /// The box that takes typing and Enter/F3.
    pub active: usize,
    /// The box whose colour choices are shown, if any.
    pub picker: Option<usize>,
}

impl Search {
    pub fn new() -> Self {
        Self { open: false, focused: false, boxes: vec![SearchBox::new(0)], active: 0, picker: None }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.focused = true;
    }

    /// Opens (or focuses) the search bar. Queries kept from before are run
    /// again, so the current match and the counts are fresh.
    pub fn open_in<T: EventListener>(&mut self, term: &mut Term<T>) {
        let reopened = !self.open;
        self.open();
        if reopened {
            for (i, b) in self.boxes.iter_mut().enumerate() {
                if b.query.is_empty() {
                    continue;
                }
                if i == self.active {
                    let q = std::mem::take(&mut b.query);
                    b.set_query(q, term);
                } else {
                    b.rerun(term);
                }
            }
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.blur();
        for b in &mut self.boxes {
            b.visible.clear();
            b.current = None;
        }
    }

    /// Typing goes back to the terminal.
    pub fn blur(&mut self) {
        self.focused = false;
        self.picker = None;
    }

    pub fn active_box(&self) -> &SearchBox {
        &self.boxes[self.active]
    }

    /// The focused box's query.
    pub fn query(&self) -> &str {
        &self.active_box().query
    }

    /// Terminal content changed: matches must be recounted.
    pub fn content_changed(&mut self) {
        if self.open {
            for b in self.boxes.iter_mut().filter(|b| b.regex.is_some()) {
                b.count_dirty = true;
            }
        }
    }

    pub fn set_query<T: EventListener>(&mut self, query: String, term: &mut Term<T>) {
        self.boxes[self.active].set_query(query, term);
    }

    /// Adds typed or pasted text to the focused box's query.
    pub fn type_text<T: EventListener>(&mut self, text: &str, term: &mut Term<T>) {
        let q = format!("{}{text}", self.query());
        self.set_query(q, term);
    }

    pub fn toggle_regex<T: EventListener>(&mut self, term: &mut Term<T>) {
        let b = &mut self.boxes[self.active];
        b.regex_mode = !b.regex_mode;
        let q = std::mem::take(&mut b.query);
        b.set_query(q, term);
    }

    /// Moves the focused box to its next match in `dir` and scrolls it into view.
    pub fn step<T: EventListener>(&mut self, term: &mut Term<T>, dir: Direction) {
        self.boxes[self.active].step(term, dir);
    }

    /// Focuses box `i`.
    pub fn focus(&mut self, i: usize) {
        if i < self.boxes.len() {
            self.active = i;
            self.focused = true;
        }
    }

    /// Focuses the next (or previous) box, wrapping around.
    pub fn cycle(&mut self, forward: bool) {
        let n = self.boxes.len();
        self.picker = None;
        self.focus(if forward { (self.active + 1) % n } else { (self.active + n - 1) % n });
    }

    pub fn can_add(&self) -> bool {
        self.boxes.len() < MAX_BOXES
    }

    /// Adds an empty box after box `after` and focuses it. It gets the
    /// first colour no other box uses.
    pub fn add_box(&mut self, after: usize) {
        if !self.can_add() {
            return;
        }
        let color = (0..SEARCH_COLORS).find(|c| !self.boxes.iter().any(|b| b.color == *c)).unwrap_or(0);
        let at = (after + 1).min(self.boxes.len());
        self.boxes.insert(at, SearchBox::new(color));
        self.picker = None;
        self.open();
        self.active = at;
    }

    /// Removes box `i`; removing the last one closes search instead.
    pub fn remove_box(&mut self, i: usize) {
        self.picker = None;
        if self.boxes.len() <= 1 {
            self.close();
            return;
        }
        if i >= self.boxes.len() {
            return;
        }
        self.boxes.remove(i);
        if self.active > i || self.active >= self.boxes.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// Shows or hides box `i`'s colour choices.
    pub fn toggle_picker(&mut self, i: usize) {
        self.focus(i);
        self.picker = if self.picker == Some(i) { None } else { Some(i) };
    }

    /// Gives box `i` highlight colour `color` and hides the choices.
    pub fn set_color(&mut self, i: usize, color: usize) {
        if let Some(b) = self.boxes.get_mut(i) {
            b.color = color % SEARCH_COLORS;
        }
        self.picker = None;
    }

    /// Recomputes on-screen matches in `shown`, and the live terminal's
    /// totals when due.
    pub fn refresh<T>(&mut self, shown: &Term<T>, live: &Term<T>) {
        for b in &mut self.boxes {
            b.refresh(shown, live);
        }
    }

    /// After searching the terminal shown: where a step found no match
    /// ahead, goes on in the rest of the history before going round.
    pub fn follow_up<L: EventListener + Clone>(&mut self, a: &mut Across<'_, L>) {
        for b in &mut self.boxes {
            let Some((dir, ahead)) = b.stepped.take() else { continue };
            a.searcher.cancel_step(b.id);
            b.segments.clear();
            b.current_live = !a.scroller.in_window();
            if a.scroller.in_window() {
                // The step counted the window's matches: count the live terminal's.
                b.count(a.live);
            }
            if ahead || b.regex.is_none() {
                continue;
            }
            let snap = settled(a.live, a.history);
            let (first, end) = (snap.first_line(), snap.end_line());
            let up = dir == Direction::Left;
            b.step_dir = dir;
            b.segments = match &a.scroller.window {
                None => [Segment::History(first..end, up)].into(),
                Some(w) => {
                    let (top, bottom) = (w.first_line(), (w.last_line() + 1).min(end));
                    let above = Segment::History(first..top, up);
                    let below = Segment::History(bottom..end, up);
                    let mut s = VecDeque::new();
                    if up {
                        s.push_back(above);
                        if !w.reaches_end {
                            s.push_back(Segment::Live(true));
                        }
                        s.push_back(below);
                    } else {
                        s.push_back(below);
                        if !w.reaches_end {
                            s.push_back(Segment::Live(false));
                        }
                        s.push_back(above);
                    }
                    s
                }
            };
            b.run_segments(a, &snap);
        }
    }

    /// Takes the history searcher's results: matches found, counts.
    pub fn on_results<L: EventListener + Clone>(&mut self, a: &mut Across<'_, L>) {
        for (owner, kind, found) in a.searcher.poll() {
            let Some(b) = self.boxes.iter_mut().find(|b| b.id == owner) else { continue };
            match (kind, found) {
                (Kind::Step, Found::Line(line)) => b.land(a, line),
                (Kind::Step, Found::Nothing) => {
                    let snap = settled(a.live, a.history);
                    b.run_segments(a, &snap);
                }
                (Kind::Count, Found::Count { matches, capped }) => {
                    b.hist_total += matches;
                    b.hist_capped |= capped || b.hist_total >= COUNT_CAP;
                    b.combine();
                }
                _ => {}
            }
        }
        for b in &mut self.boxes {
            b.counting = a.searcher.busy(b.id, Kind::Count);
        }
    }

    /// Counts matches in history, adding lines as they arrive; a cleared
    /// history is counted again. Call once a frame while search is open.
    pub fn count_history<L: EventListener>(&mut self, a: &mut Across<'_, L>) {
        let (first, end, generation) = {
            let h = lock(a.history);
            (h.first_line(), h.end_line(), h.generation())
        };
        for b in &mut self.boxes {
            if b.hist_counted.is_some_and(|(g, _)| g != generation) || (b.hist_gen != b.query_gen && a.searcher.busy(b.id, Kind::Count)) {
                // Cleared, or counting for a query since changed.
                a.searcher.cancel(b.id);
                b.segments.clear();
                b.hist_total = 0;
                b.hist_capped = false;
                b.hist_counted = None;
            }
            b.counting = a.searcher.busy(b.id, Kind::Count);
            let Some(pattern) = b.pattern() else { continue };
            let from = b.hist_counted.map_or(first, |(_, to)| to.max(first));
            let due = b.last_hist_count.is_none_or(|t| t.elapsed() >= RECOUNT_EVERY) || b.hist_counted.is_none();
            if b.hist_capped || b.counting || end <= from || !due {
                b.combine();
                continue;
            }
            let snap = lock(a.history).snapshot();
            a.searcher.count(snap, &pattern, from..end, a.live.columns(), COUNT_CAP - b.hist_total, b.id);
            b.hist_counted = Some((generation, end));
            b.hist_gen = b.query_gen;
            b.last_hist_count = Some(Instant::now());
            b.counting = true;
        }
    }

    /// A history search or count is running for some box.
    #[cfg(test)]
    pub fn busy(&self, searcher: &Searcher) -> bool {
        self.boxes.iter().any(|b| searcher.busy(b.id, Kind::Step) || searcher.busy(b.id, Kind::Count))
    }

    /// The terminal shown changed under the matches: they no longer point
    /// at the right text.
    pub fn forget_current(&mut self) {
        for b in &mut self.boxes {
            b.current = None;
        }
    }

    /// True if a recount is waiting for its throttle window.
    pub fn recount_pending(&self) -> bool {
        self.boxes.iter().any(|b| b.count_dirty)
    }
}

/// Escapes regex metacharacters for a literal search.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::test::mock_term;

    #[test]
    fn finds_all_visible_matches_case_insensitively() {
        let mut term = mock_term("foo bar Foo\r\nbaz foo.c\r\nnothing");
        let mut s = Search::new();
        s.open();
        s.set_query("foo".into(), &mut term);
        s.refresh(&term, &term);
        assert_eq!(s.active_box().visible.len(), 3);
        assert_eq!(s.active_box().total, 3);
        assert!(s.active_box().current.is_some());
    }

    #[test]
    fn literal_mode_escapes_metacharacters() {
        let mut term = mock_term("a.c abc\r\n");
        let mut s = Search::new();
        s.set_query("a.c".into(), &mut term);
        assert_eq!(s.active_box().total, 1);
        s.toggle_regex(&mut term);
        assert_eq!(s.active_box().total, 2);
    }

    #[test]
    fn upper_case_query_is_case_sensitive() {
        let mut term = mock_term("foo Foo FOO");
        let mut s = Search::new();
        s.set_query("Foo".into(), &mut term);
        assert_eq!(s.active_box().total, 1);
    }

    #[test]
    fn stepping_cycles_through_matches() {
        let mut term = mock_term("x1 x2 x3");
        let mut s = Search::new();
        s.set_query("x".into(), &mut term);
        let first = s.active_box().index;
        s.step(&mut term, Direction::Right);
        assert_ne!(s.active_box().index, first);
        assert_eq!(s.active_box().total, 3);
    }

    #[test]
    fn empty_query_matches_nothing() {
        let mut term = mock_term("abc");
        let mut s = Search::new();
        s.open();
        s.set_query(String::new(), &mut term);
        s.refresh(&term, &term);
        assert!(s.active_box().visible.is_empty());
        assert_eq!((s.active_box().total, s.active_box().index, s.active_box().current.is_some()), (0, None, false));
    }

    #[test]
    fn close_clears_matches_but_keeps_the_query() {
        let mut term = mock_term("foo foo");
        let mut s = Search::new();
        s.open();
        s.set_query("foo".into(), &mut term);
        s.refresh(&term, &term);
        assert_eq!(s.active_box().visible.len(), 2);
        s.close();
        assert!(!s.open && !s.focused);
        assert!(s.active_box().visible.is_empty() && s.active_box().current.is_none());
        assert_eq!(s.query(), "foo");
    }

    #[test]
    fn reopening_restores_the_current_match_and_count() {
        let mut term = mock_term("foo foo foo");
        let mut s = Search::new();
        s.open_in(&mut term);
        s.set_query("foo".into(), &mut term);
        s.close();
        assert!(s.active_box().current.is_none());
        s.open_in(&mut term);
        assert!(s.active_box().current.is_some(), "current match is back");
        assert_eq!((s.active_box().total, s.active_box().index), (3, Some(3)));
        // Focusing an already open bar keeps the position.
        s.step(&mut term, Direction::Left);
        s.open_in(&mut term);
        assert_eq!(s.active_box().index, Some(2));
    }

    #[test]
    fn new_output_triggers_a_recount_only_while_open() {
        let mut term = mock_term("foo");
        let mut s = Search::new();
        s.set_query("foo".into(), &mut term);
        s.content_changed();
        assert!(!s.recount_pending(), "closed search ignores output");
        s.open();
        s.content_changed();
        assert!(s.recount_pending());
        s.refresh(&term, &term);
        assert!(s.recount_pending(), "throttled: set_query counted just now");
        std::thread::sleep(RECOUNT_EVERY);
        s.refresh(&term, &term);
        assert!(!s.recount_pending(), "recounted once the throttle window passed");
    }

    #[test]
    fn index_follows_steps_in_both_directions() {
        let mut term = mock_term("x x x x");
        let mut s = Search::new();
        s.set_query("x".into(), &mut term);
        // Starts at the match nearest the bottom-right: the last one.
        assert_eq!(s.active_box().index, Some(4));
        s.step(&mut term, Direction::Left);
        assert_eq!(s.active_box().index, Some(3));
        s.step(&mut term, Direction::Right);
        s.step(&mut term, Direction::Right);
        assert_eq!(s.active_box().index, Some(1), "wraps around");
    }

    #[test]
    fn matches_across_lines_and_wide_characters() {
        let mut term = mock_term("日本 foo\r\nbar foo");
        let mut s = Search::new();
        s.open();
        s.set_query("foo".into(), &mut term);
        s.refresh(&term, &term);
        assert_eq!(s.active_box().visible.len(), 2);
        assert_eq!(s.active_box().visible[0].start().column.0, 5, "after two double-width chars and a space");
    }

    #[test]
    fn escaping_covers_every_regex_metacharacter() {
        let mut term = mock_term("a+b (c) [d] {e} ^f$ g|h");
        let mut s = Search::new();
        for q in ["a+b", "(c)", "[d]", "{e}", "^f$", "g|h"] {
            s.set_query(q.into(), &mut term);
            assert_eq!(s.active_box().total, 1, "{q}");
            assert!(!s.active_box().invalid);
        }
    }

    #[test]
    fn invalid_regex_is_reported() {
        let mut term = mock_term("abc");
        let mut s = Search::new();
        s.boxes[0].regex_mode = true;
        s.set_query("(".into(), &mut term);
        assert!(s.active_box().invalid);
        assert_eq!(s.active_box().total, 0);
    }

    #[test]
    fn boxes_search_independently_and_typing_goes_to_the_focused_one() {
        let mut term = mock_term("foo bar foo baz\r\nbar");
        let mut s = Search::new();
        s.open_in(&mut term);
        s.type_text("foo", &mut term);
        s.add_box(0);
        assert_eq!((s.boxes.len(), s.active), (2, 1), "new box is focused");
        assert_ne!(s.boxes[1].color, s.boxes[0].color, "with a colour of its own");
        s.type_text("ba", &mut term);
        s.type_text("r", &mut term);
        s.refresh(&term, &term);
        assert_eq!((s.boxes[0].query.as_str(), s.boxes[0].total), ("foo", 2));
        assert_eq!((s.boxes[1].query.as_str(), s.boxes[1].total), ("bar", 2));
        assert_eq!((s.boxes[0].visible.len(), s.boxes[1].visible.len()), (2, 2));
        s.cycle(true);
        assert_eq!(s.active, 0, "Tab wraps around");
        s.cycle(false);
        assert_eq!(s.active, 1);
    }

    #[test]
    fn adding_inserts_after_the_box_and_stops_at_the_limit() {
        let mut s = Search::new();
        s.add_box(0);
        s.add_box(0);
        assert_eq!(s.active, 1, "inserted right after box 0");
        while s.can_add() {
            s.add_box(s.boxes.len() - 1);
        }
        assert_eq!(s.boxes.len(), MAX_BOXES);
        s.add_box(0);
        assert_eq!(s.boxes.len(), MAX_BOXES);
        let mut colors: Vec<usize> = s.boxes.iter().map(|b| b.color).collect();
        colors.sort();
        colors.dedup();
        assert_eq!(colors.len(), MAX_BOXES, "every box starts with a different colour");
    }

    #[test]
    fn removing_boxes_keeps_focus_sensible_and_the_last_one_closes() {
        let mut term = mock_term("a b c");
        let mut s = Search::new();
        s.open_in(&mut term);
        s.type_text("a", &mut term);
        s.add_box(0);
        s.type_text("b", &mut term);
        s.add_box(1);
        s.type_text("c", &mut term);
        s.focus(1);
        s.remove_box(0);
        assert_eq!((s.boxes.len(), s.query()), (2, "b"), "focus stays on the same box");
        s.remove_box(1);
        assert_eq!((s.boxes.len(), s.active, s.query()), (1, 0, "b"));
        s.remove_box(0);
        assert!(!s.open, "removing the only box closes search");
        assert_eq!((s.boxes.len(), s.query()), (1, "b"), "the query is kept for next time");
    }

    #[test]
    fn colour_choice() {
        let mut s = Search::new();
        s.add_box(0);
        s.toggle_picker(0);
        assert_eq!((s.picker, s.active), (Some(0), 0));
        s.set_color(0, 5);
        assert_eq!((s.boxes[0].color, s.picker), (5, None));
        s.toggle_picker(1);
        s.toggle_picker(1);
        assert_eq!(s.picker, None, "clicking again hides the choices");
        s.toggle_picker(1);
        s.blur();
        assert_eq!(s.picker, None);
    }

    #[test]
    fn reopening_reruns_every_box_and_new_output_recounts_them() {
        let mut term = mock_term("x y x y");
        let mut s = Search::new();
        s.open_in(&mut term);
        s.type_text("x", &mut term);
        s.add_box(0);
        s.type_text("y", &mut term);
        s.close();
        s.open_in(&mut term);
        assert_eq!((s.boxes[0].total, s.boxes[1].total), (2, 2));
        assert!(s.boxes[1].current.is_some(), "focused box is at a match");
        assert!(s.boxes[0].current.is_none(), "others don't move the view");
        s.content_changed();
        assert!(s.boxes.iter().all(|b| b.count_dirty));
    }

    /// Search through a tab that keeps its history.
    mod across {
        use std::time::{Duration, Instant};

        use alacritty_terminal::grid::Dimensions;

        use super::super::*;
        use crate::history::scroll::Scroller;
        use crate::history::search::Searcher;
        use crate::history::{Budget, CELL_BYTES, History, Shared, start};
        use crate::testterm::{Quiet, TestTerm, temp_dir};

        struct A {
            t: TestTerm,
            history: Shared,
            budget: Budget,
            scroller: Scroller<Quiet>,
            searcher: Searcher,
            search: Search,
        }

        impl A {
            /// 20 000 lines, "needle" on lines 5, 9000 and 19 990 (live).
            fn new(tag: &str) -> Self {
                let (history, sink) = start(History::new(4096, temp_dir(tag)).with_chunk_bytes(512));
                let mut t = TestTerm::new(20, 10, 50);
                t.term.grid_mut().set_row_sink(Box::new(sink));
                let text: String = (0..20_000).map(|i| if [5, 9000, 19_990].contains(&i) { format!("a needle {i}\n") } else { format!("hay {i}\n") }).collect();
                t.feed(&text);
                let mut search = Search::new();
                search.open();
                Self { t, history, budget: Budget { bytes: 200 * 4 * 20 * CELL_BYTES }, scroller: Scroller::new(Quiet), searcher: Searcher::new(|| {}), search }
            }

            fn with<R>(&mut self, f: impl FnOnce(&mut Search, &mut Across<'_, Quiet>) -> R) -> R {
                let mut a = Across { live: &mut self.t.term, scroller: &mut self.scroller, history: &self.history, budget: self.budget, searcher: &mut self.searcher };
                f(&mut self.search, &mut a)
            }

            /// What the app does around a search action on the terminal shown.
            fn act(&mut self, f: impl FnOnce(&mut Search, &mut Term<Quiet>)) {
                f(&mut self.search, self.scroller.shown_mut(&mut self.t.term));
                self.with(|s, a| s.follow_up(a));
                self.settle();
            }

            /// Takes results until no history search runs.
            fn settle(&mut self) {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    self.with(|s, a| s.on_results(a));
                    if !self.search.busy(&self.searcher) {
                        return;
                    }
                    assert!(Instant::now() < deadline, "search never finished");
                    std::thread::sleep(Duration::from_millis(2));
                }
            }

            /// Text of the current match's row, which must be on screen.
            fn current(&self) -> String {
                let term = self.scroller.shown(&self.t.term);
                let m = self.search.active_box().current.clone().expect("a current match");
                let grid = term.grid();
                let top = -(grid.display_offset() as i32);
                assert!((top..top + grid.screen_lines() as i32).contains(&m.start().line.0), "on screen");
                let row = &grid[m.start().line];
                (0..grid.columns()).map(|c| row[Column(c)].c).collect::<String>().trim_end().to_owned()
            }
        }

        #[test]
        fn shift_enter_walks_history_and_wraps() {
            let mut a = A::new("walk");
            a.act(|s, t| s.set_query("needle".into(), t));
            assert_eq!(a.current(), "a needle 19990");
            assert!(!a.scroller.in_window());
            let mut seen = Vec::new();
            for _ in 0..3 {
                a.act(|s, t| s.step(t, Direction::Left));
                seen.push(a.current());
            }
            assert_eq!(seen, ["a needle 9000", "a needle 5", "a needle 19990"]);
            assert!(!a.scroller.in_window(), "back in the live terminal");
            // And down again.
            a.act(|s, t| s.step(t, Direction::Right));
            assert_eq!(a.current(), "a needle 5");
            a.act(|s, t| s.step(t, Direction::Right));
            assert_eq!(a.current(), "a needle 9000");
        }

        #[test]
        fn count_spans_history() {
            let mut a = A::new("count");
            a.act(|s, t| s.set_query("needle".into(), t));
            a.with(|s, x| s.count_history(x));
            a.settle();
            let b = a.search.active_box();
            assert_eq!((b.total, b.index, b.counting), (3, Some(3), false));
        }

        #[test]
        fn clear_cancels_history_search() {
            let mut a = A::new("clear");
            a.act(|s, t| s.set_query("needle".into(), t));
            a.with(|s, x| s.count_history(x));
            a.t.feed("\x1b[3J");
            a.t.term.flush_row_sink();
            crate::history::sync(&a.history);
            a.with(|s, x| s.count_history(x));
            a.settle();
            a.with(|s, x| s.count_history(x));
            a.settle();
            assert_eq!(a.search.active_box().total, 1, "only the live terminal's match is left");
        }
    }
}

