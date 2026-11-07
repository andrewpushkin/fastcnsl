//! Find-in-terminal. All matches on screen are highlighted; Enter and
//! Shift+Enter jump between matches through the whole scrollback.
//! Search is literal and smart-case (lower-case query ignores case);
//! Ctrl+R in the search bar switches to regular expressions.
//!
//! Several searches can run at once, one per box, each highlighting its
//! matches in its own colour. Typing and match navigation go to the
//! focused box.

use std::time::{Duration, Instant};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

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
}

impl SearchBox {
    fn new(color: usize) -> Self {
        Self {
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
        self.total >= COUNT_CAP
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
        if let Some(m) = term.search_next(regex, origin, dir, side, None) {
            term.scroll_to_point(*m.start());
            self.current = Some(m);
            self.count(term);
        }
    }

    fn refresh<T>(&mut self, term: &Term<T>) {
        self.visible.clear();
        let Some(regex) = self.regex.as_mut() else { return };
        let offset = term.grid().display_offset() as i32;
        let start = Point::new(Line(-offset), Column(0));
        let end = Point::new(Line(term.screen_lines() as i32 - 1 - offset), term.last_column());
        self.visible.extend(RegexIter::new(start, end, Direction::Right, term, regex));

        let due = self.last_count.is_none_or(|t| t.elapsed() >= RECOUNT_EVERY);
        if self.count_dirty && due {
            self.count(term);
        }
    }

    fn count<T>(&mut self, term: &Term<T>) {
        self.count_dirty = false;
        self.last_count = Some(Instant::now());
        self.total = 0;
        self.index = None;
        let Some(regex) = self.regex.as_mut() else { return };
        let start = Point::new(term.topmost_line(), Column(0));
        let end = Point::new(term.bottommost_line(), term.last_column());
        let current_start = self.current.as_ref().map(|m| *m.start());
        for m in RegexIter::new(start, end, Direction::Right, term, regex) {
            self.total += 1;
            if Some(*m.start()) == current_start {
                self.index = Some(self.total);
            }
            if self.total >= COUNT_CAP {
                break;
            }
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

    /// Recomputes on-screen matches, and the totals when due.
    pub fn refresh<T>(&mut self, term: &Term<T>) {
        for b in &mut self.boxes {
            b.refresh(term);
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
        s.refresh(&term);
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
        s.refresh(&term);
        assert!(s.active_box().visible.is_empty());
        assert_eq!((s.active_box().total, s.active_box().index, s.active_box().current.is_some()), (0, None, false));
    }

    #[test]
    fn close_clears_matches_but_keeps_the_query() {
        let mut term = mock_term("foo foo");
        let mut s = Search::new();
        s.open();
        s.set_query("foo".into(), &mut term);
        s.refresh(&term);
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
        s.refresh(&term);
        assert!(s.recount_pending(), "throttled: set_query counted just now");
        std::thread::sleep(RECOUNT_EVERY);
        s.refresh(&term);
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
        s.refresh(&term);
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
        s.refresh(&term);
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
}
