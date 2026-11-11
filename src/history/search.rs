//! Searching the lines in history, on threads of their own so the window
//! never waits: the nearest line with a match in a direction, or how many
//! matches there are.


use std::ops::{ControlFlow, Range};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use regex_automata::meta::Regex;
use regex_automata::util::syntax;

use super::{Item, Snapshot, line};

/// A regex for `pattern`, by alacritty_terminal's rule: it ignores case
/// unless the pattern has an upper-case letter.
pub fn compile(pattern: &str) -> Result<Regex, String> {
    let has_upper = pattern.chars().any(char::is_uppercase);
    Regex::builder()
        .syntax(syntax::Config::new().case_insensitive(!has_upper))
        .build(pattern)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Step,
    Count,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The nearest line with a match.
    Line(u64),
    Nothing,
    /// Matches counted, stopping at the cap.
    Count { matches: usize, capped: bool },
}

struct Job {
    owner: u64,
    kind: Kind,
    id: u64,
    cancel: Arc<AtomicBool>,
}

type Done = Arc<Mutex<Vec<(u64, u64, Kind, Found)>>>;

/// One tab's history searches, at most one of each kind per owner (a
/// search box). A new one replaces the one running.
pub struct Searcher {
    jobs: Vec<Job>,
    next_id: u64,
    done: Done,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Searcher {
    /// `wake` is called from the search's thread when it has a result.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { jobs: Vec::new(), next_id: 0, done: Arc::default(), wake: Arc::new(wake) }
    }

    /// Looks for the nearest line in `range` with a match, from its end
    /// (`up`) or its start.
    pub fn step(&mut self, snap: Snapshot, pattern: &str, range: Range<u64>, up: bool, owner: u64) {
        self.spawn(owner, Kind::Step, pattern, move |re, cancel| {
            let mut found = Found::Nothing;
            let mut text = String::new();
            let _ = snap.for_each(range, up, |at, item| {
                if cancel.load(Ordering::Relaxed) {
                    return ControlFlow::Break(());
                }
                if let Item::Line(bytes) = item {
                    text.clear();
                    line::text(bytes, &mut text);
                    if re.is_match(&text) {
                        found = Found::Line(at);
                        return ControlFlow::Break(());
                    }
                }
                ControlFlow::Continue(())
            });
            found
        });
    }

    /// Counts the matches in `range`, up to `cap`.
    pub fn count(&mut self, snap: Snapshot, pattern: &str, range: Range<u64>, cap: usize, owner: u64) {
        self.spawn(owner, Kind::Count, pattern, move |re, cancel| {
            let mut matches = 0;
            let mut text = String::new();
            let _ = snap.for_each(range, false, |_, item| {
                if cancel.load(Ordering::Relaxed) {
                    return ControlFlow::Break(());
                }
                if let Item::Line(bytes) = item {
                    text.clear();
                    line::text(bytes, &mut text);
                    matches += re.find_iter(&text).filter(|m| !m.is_empty()).count();
                    if matches >= cap {
                        return ControlFlow::Break(());
                    }
                }
                ControlFlow::Continue(())
            });
            Found::Count { matches: matches.min(cap), capped: matches >= cap }
        });
    }

    fn spawn(&mut self, owner: u64, kind: Kind, pattern: &str, work: impl FnOnce(Regex, &AtomicBool) -> Found + Send + 'static) {
        self.cancel_kind(owner, kind);
        let Ok(re) = compile(pattern) else { return };
        let id = self.next_id;
        self.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.jobs.push(Job { owner, kind, id, cancel: cancel.clone() });
        let done = self.done.clone();
        let wake = self.wake.clone();
        std::thread::Builder::new()
            .name("history search".into())
            .spawn(move || {
                let found = work(re, &cancel);
                if !cancel.load(Ordering::Relaxed) {
                    done.lock().unwrap_or_else(|e| e.into_inner()).push((id, owner, kind, found));
                    wake();
                }
            })
            .expect("search thread");
    }

    /// Stops `owner`'s step, if one is running.
    pub fn cancel_step(&mut self, owner: u64) {
        self.cancel_kind(owner, Kind::Step);
    }

    fn cancel_kind(&mut self, owner: u64, kind: Kind) {
        self.jobs.retain(|j| {
            let gone = j.owner == owner && j.kind == kind;
            if gone {
                j.cancel.store(true, Ordering::Relaxed);
            }
            !gone
        });
    }

    /// Stops `owner`'s searches; their results never come.
    pub fn cancel(&mut self, owner: u64) {
        self.cancel_kind(owner, Kind::Step);
        self.cancel_kind(owner, Kind::Count);
    }

    pub fn cancel_all(&mut self) {
        for j in self.jobs.drain(..) {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn busy(&self, owner: u64, kind: Kind) -> bool {
        self.jobs.iter().any(|j| j.owner == owner && j.kind == kind)
    }

    /// Results of the searches finished since the last call: (owner, kind,
    /// result). Results of cancelled or replaced searches are left out.
    pub fn poll(&mut self) -> Vec<(u64, Kind, Found)> {
        let finished = std::mem::take(&mut *self.done.lock().unwrap_or_else(|e| e.into_inner()));
        let mut out = Vec::new();
        for (id, owner, kind, found) in finished {
            if let Some(i) = self.jobs.iter().position(|j| j.id == id) {
                self.jobs.remove(i);
                out.push((owner, kind, found));
            }
        }
        out
    }
}

impl Drop for Searcher {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::history::{History, Shared, start, sync};
    use crate::testterm::{TestTerm, temp_dir};

    /// 20 000 lines with "needle" on lines 5, 9000 and 19 990.
    fn needles(tag: &str) -> (TestTerm, Shared) {
        let (history, sink) = start(History::new(4096, temp_dir(tag)).with_chunk_bytes(512));
        let mut t = TestTerm::new(20, 10, 50);
        t.term.grid_mut().set_row_sink(Box::new(sink));
        let text: String = (0..20_000).map(|i| if [5, 9000, 19_990].contains(&i) { format!("a needle {i}\n") } else { format!("hay {i}\n") }).collect();
        t.feed(&text);
        t.term.flush_row_sink();
        sync(&history);
        (t, history)
    }

    /// Waits for the searcher's next result.
    fn next(s: &mut Searcher) -> (u64, Kind, Found) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(r) = s.poll().into_iter().next() {
                return r;
            }
            assert!(Instant::now() < deadline, "no result");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn compile_matches_alacritty_case_rule() {
        assert!(compile("err").unwrap().is_match("ERR here"));
        assert!(!compile("Err").unwrap().is_match("err here"));
        assert!(compile("Err").unwrap().is_match("Err here"));
        assert!(compile("(").is_err());
    }

    #[test]
    fn step_up_finds_disk_line() {
        let (_t, history) = needles("step");
        let snap = crate::history::lock(&history).snapshot();
        let mut s = Searcher::new(|| {});
        s.step(snap.clone(), "needle", snap.first_line()..snap.end_line(), true, 1);
        assert_eq!(next(&mut s), (1, Kind::Step, Found::Line(9000)));
        s.step(snap.clone(), "needle", snap.first_line()..9000, true, 1);
        assert_eq!(next(&mut s), (1, Kind::Step, Found::Line(5)));
        s.step(snap.clone(), "needle", 6..9000, false, 1);
        assert_eq!(next(&mut s), (1, Kind::Step, Found::Nothing));
        assert!(!s.busy(1, Kind::Step));
    }

    #[test]
    fn count_spans_history() {
        let (_t, history) = needles("count");
        let snap = crate::history::lock(&history).snapshot();
        let mut s = Searcher::new(|| {});
        s.count(snap.clone(), "needle", snap.first_line()..snap.end_line(), 9999, 7);
        assert_eq!(next(&mut s), (7, Kind::Count, Found::Count { matches: 2, capped: false }));
    }

    #[test]
    fn count_caps_at_9999() {
        let (_t, history) = needles("cap");
        let snap = crate::history::lock(&history).snapshot();
        let mut s = Searcher::new(|| {});
        s.count(snap.clone(), "a", snap.first_line()..snap.end_line(), 9999, 7);
        assert_eq!(next(&mut s), (7, Kind::Count, Found::Count { matches: 9999, capped: true }));
    }

    #[test]
    fn cancelled_job_reports_nothing() {
        let (_t, history) = needles("cancel");
        let snap = crate::history::lock(&history).snapshot();
        let mut s = Searcher::new(|| {});
        s.count(snap.clone(), "needle", snap.first_line()..snap.end_line(), 9999, 3);
        s.cancel(3);
        assert!(!s.busy(3, Kind::Count));
        std::thread::sleep(Duration::from_millis(200));
        assert!(s.poll().is_empty());
    }

    #[test]
    fn a_new_step_replaces_the_one_running() {
        let (_t, history) = needles("replace");
        let snap = crate::history::lock(&history).snapshot();
        let mut s = Searcher::new(|| {});
        s.step(snap.clone(), "needle", snap.first_line()..snap.end_line(), true, 1);
        s.step(snap.clone(), "needle", snap.first_line()..snap.end_line(), false, 1);
        assert_eq!(next(&mut s), (1, Kind::Step, Found::Line(5)));
        std::thread::sleep(Duration::from_millis(100));
        assert!(s.poll().is_empty(), "the first step's result is dropped");
    }
}
