//! Find-in-terminal. All matches on screen are highlighted; Enter and
//! Shift+Enter jump between matches through the whole scrollback.
//! Search is literal and smart-case (lower-case query ignores case);
//! Ctrl+R in the search bar switches to regular expressions.

use std::time::{Duration, Instant};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

/// Counting every match in a huge scrollback is bounded.
const COUNT_CAP: usize = 9999;
/// Recount at most this often while output is streaming.
const RECOUNT_EVERY: Duration = Duration::from_millis(250);

pub struct Search {
    pub open: bool,
    pub focused: bool,
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
    count_dirty: bool,
    last_count: Option<Instant>,
}

impl Search {
    pub fn new() -> Self {
        Self {
            open: false,
            focused: false,
            query: String::new(),
            regex_mode: false,
            invalid: false,
            regex: None,
            visible: Vec::new(),
            current: None,
            total: 0,
            index: None,
            count_dirty: false,
            last_count: None,
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.focused = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.focused = false;
        self.visible.clear();
        self.current = None;
    }

    pub fn total_capped(&self) -> bool {
        self.total >= COUNT_CAP
    }

    /// Terminal content changed: matches must be recounted.
    pub fn content_changed(&mut self) {
        if self.open && self.regex.is_some() {
            self.count_dirty = true;
        }
    }

    pub fn set_query<T: EventListener>(&mut self, query: String, term: &mut Term<T>) {
        self.query = query;
        self.rebuild();
        self.current = None;
        // Like a browser: jump to the nearest match above the bottom of the screen.
        self.step(term, Direction::Left);
        self.count(term);
    }

    pub fn toggle_regex<T: EventListener>(&mut self, term: &mut Term<T>) {
        self.regex_mode = !self.regex_mode;
        let q = std::mem::take(&mut self.query);
        self.set_query(q, term);
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

    /// Moves to the next match in `dir` and scrolls it into view.
    pub fn step<T: EventListener>(&mut self, term: &mut Term<T>, dir: Direction) {
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

    /// Recomputes on-screen matches, and the total when due.
    pub fn refresh<T>(&mut self, term: &Term<T>) {
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

    /// True if a recount is waiting for its throttle window.
    pub fn recount_pending(&self) -> bool {
        self.count_dirty
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
        assert_eq!(s.visible.len(), 3);
        assert_eq!(s.total, 3);
        assert!(s.current.is_some());
    }

    #[test]
    fn literal_mode_escapes_metacharacters() {
        let mut term = mock_term("a.c abc\r\n");
        let mut s = Search::new();
        s.set_query("a.c".into(), &mut term);
        assert_eq!(s.total, 1);
        s.toggle_regex(&mut term);
        assert_eq!(s.total, 2);
    }

    #[test]
    fn upper_case_query_is_case_sensitive() {
        let mut term = mock_term("foo Foo FOO");
        let mut s = Search::new();
        s.set_query("Foo".into(), &mut term);
        assert_eq!(s.total, 1);
    }

    #[test]
    fn stepping_cycles_through_matches() {
        let mut term = mock_term("x1 x2 x3");
        let mut s = Search::new();
        s.set_query("x".into(), &mut term);
        let first = s.index;
        s.step(&mut term, Direction::Right);
        assert_ne!(s.index, first);
        assert_eq!(s.total, 3);
    }

    #[test]
    fn empty_query_matches_nothing() {
        let mut term = mock_term("abc");
        let mut s = Search::new();
        s.open();
        s.set_query(String::new(), &mut term);
        s.refresh(&term);
        assert!(s.visible.is_empty());
        assert_eq!((s.total, s.index, s.current.is_some()), (0, None, false));
    }

    #[test]
    fn close_clears_matches_but_keeps_the_query() {
        let mut term = mock_term("foo foo");
        let mut s = Search::new();
        s.open();
        s.set_query("foo".into(), &mut term);
        s.refresh(&term);
        assert_eq!(s.visible.len(), 2);
        s.close();
        assert!(!s.open && !s.focused);
        assert!(s.visible.is_empty() && s.current.is_none());
        assert_eq!(s.query, "foo");
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
        assert_eq!(s.index, Some(4));
        s.step(&mut term, Direction::Left);
        assert_eq!(s.index, Some(3));
        s.step(&mut term, Direction::Right);
        s.step(&mut term, Direction::Right);
        assert_eq!(s.index, Some(1), "wraps around");
    }

    #[test]
    fn matches_across_lines_and_wide_characters() {
        let mut term = mock_term("日本 foo\r\nbar foo");
        let mut s = Search::new();
        s.open();
        s.set_query("foo".into(), &mut term);
        s.refresh(&term);
        assert_eq!(s.visible.len(), 2);
        assert_eq!(s.visible[0].start().column.0, 5, "after two double-width chars and a space");
    }

    #[test]
    fn escaping_covers_every_regex_metacharacter() {
        let mut term = mock_term("a+b (c) [d] {e} ^f$ g|h");
        let mut s = Search::new();
        for q in ["a+b", "(c)", "[d]", "{e}", "^f$", "g|h"] {
            s.set_query(q.into(), &mut term);
            assert_eq!(s.total, 1, "{q}");
            assert!(!s.invalid);
        }
    }

    #[test]
    fn invalid_regex_is_reported() {
        let mut term = mock_term("abc");
        let mut s = Search::new();
        s.regex_mode = true;
        s.set_query("(".into(), &mut term);
        assert!(s.invalid);
        assert_eq!(s.total, 0);
    }
}
