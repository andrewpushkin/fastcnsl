//! Saving a tab's text to a file, once or continuously.
//!
//! Text is written as lines (wrapped rows joined, trailing blanks
//! removed), the way programs printed it. A continuous log appends each
//! line once it is complete, i.e. once the cursor has moved past it; the
//! cursor's own line is written when Enter is pressed or logging stops.
//! Lines are told apart by their global index (see `lines.rs`), so none is
//! written twice. Lines that left the grid before they were logged come
//! from the tab's history (`crate::history`).

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::term::cell::Cell;

use crate::history::{Item, Snapshot, line, lost_text};
use crate::lines::{self, Pos};

/// "Copy all text" and copying a selection keep at most this many lines.
pub const COPY_LINES: usize = 100_000;
/// Lines a log writes from history in one pass, so catching up on a long
/// history doesn't hold up the window.
const CATCH_UP: u64 = 50_000;

/// Writes history lines `range` of `snap`, each after `stamp`; a range lost
/// to a disk error as its marker line.
fn write_history(out: &mut impl Write, snap: &Snapshot, range: std::ops::Range<u64>, stamp: &str) -> io::Result<()> {
    let mut text = String::new();
    let mut result = Ok(());
    snap.for_each(range, false, |_, item| {
        text.clear();
        match item {
            Item::Line(bytes) => line::text(bytes, &mut text),
            Item::Lost(n) => text.push_str(&lost_text(n)),
        }
        result = writeln!(out, "{stamp}{}", text.trim_end());
        if result.is_err() { std::ops::ControlFlow::Break(()) } else { std::ops::ControlFlow::Continue(()) }
    })?;
    result
}

/// The text of the line whose first rows left the grid while its last are
/// still there (see `History::partial`).
fn partial_text(snap: &Snapshot) -> String {
    let mut text = String::new();
    line::text(snap.partial(), &mut text);
    text
}

/// How far a pass writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upto {
    /// Complete lines: those above the cursor's line.
    Complete,
    /// The cursor's line as well.
    Cursor,
    /// Everything down to the last row with text (what a full-screen
    /// program shows can extend below the cursor).
    All,
}

/// A file a tab's lines are written to.
pub struct TabLog {
    path: PathBuf,
    out: BufWriter<File>,
    /// Global index of the next line to write; set by the first pass to
    /// the first line the tab still has.
    next: Option<u64>,
    /// Counting position (see `lines::ends_before`).
    pos: Option<Pos>,
    timestamps: bool,
    text: String,
}

impl TabLog {
    /// Creates the file; the first pass writes everything the tab still has.
    pub fn create(path: &Path, timestamps: bool) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            path: path.to_owned(),
            out: BufWriter::new(file),
            next: None,
            pos: None,
            timestamps,
            text: String::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Forgets the counting position; call after the grid is resized.
    pub fn invalidate(&mut self) {
        self.pos = None;
    }

    /// Lines that left the grid are waiting to be written: pass the
    /// history to [`Self::write_new`].
    pub fn behind(&self, grid: &Grid<Cell>) -> bool {
        self.next.is_none_or(|next| next < grid.lines_dropped())
    }

    /// Writes the lines not written yet, up to `upto`, and flushes them to
    /// the file. Pass the normal screen's grid, except to save a
    /// full-screen program's screen once, and the tab's history, settled
    /// (see `history::scroll::settled`), when [`Self::behind`]. Writes at
    /// most `CATCH_UP` lines from history at a time; returns true while
    /// there are more.
    pub fn write_new(&mut self, snap: Option<&Snapshot>, grid: &Grid<Cell>, upto: Upto) -> io::Result<bool> {
        let dropped = grid.lines_dropped();
        let stamp = if self.timestamps { format!("[{}] ", local_time("%H:%M:%S")) } else { String::new() };
        let next = self.next.get_or_insert(snap.map_or(dropped, Snapshot::first_line));
        let first = snap.map_or(dropped, |s| s.first_line().min(dropped));
        if *next < first {
            let lost = first - *next;
            writeln!(self.out, "[fastcnsl: {lost} lines scrolled out of the scrollback before they could be logged]")?;
            *next = first;
        }
        if let Some(snap) = snap
            && *next < dropped
        {
            let to = dropped.min(snap.end_line()).min(*next + CATCH_UP);
            write_history(&mut self.out, snap, *next..to, &stamp)?;
            *next = to;
            if *next < dropped {
                self.out.flush()?;
                return Ok(true);
            }
        }
        let lead = snap.map(partial_text).unwrap_or_default();
        write_lines(&mut self.out, grid, upto, next, &mut self.pos, &stamp, &mut self.text, &lead)?;
        self.out.flush()?;
        Ok(false)
    }
}

/// Writes the lines from global index `*next` on, up to `upto`, each after
/// `stamp`, and moves `*next` past them. `pos` is the counting position
/// (see `lines::ends_before`); `text` a reusable buffer; `lead` the text of
/// the grid's first line that already left it (see `History::partial`).
#[allow(clippy::too_many_arguments)]
fn write_lines(
    out: &mut impl Write,
    grid: &Grid<Cell>,
    upto: Upto,
    next: &mut u64,
    pos: &mut Option<Pos>,
    stamp: &str,
    text: &mut String,
    lead: &str,
) -> io::Result<()> {
    let cursor = lines::cursor_row(grid);
    let cursor_line = lines::line_start(grid, cursor);
    let stop = match upto {
        Upto::Complete => cursor_line,
        Upto::Cursor => cursor + 1,
        Upto::All => (last_text_row(grid) + 1).max(cursor + 1),
    };
    // From a row whose line number is known (the rows above it don't
    // change any more), back up to where line `next` starts.
    let anchor = cursor_line.min(grid.history_size());
    let mut line = lines::ends_before(grid, pos, anchor);
    let mut row = anchor;
    while row > 0 && (line > *next || lines::wraps(grid, row - 1)) {
        row -= 1;
        if !lines::wraps(grid, row) {
            line -= 1;
        }
    }

    text.clear();
    for r in row..stop {
        if line >= *next {
            if r == 0 {
                text.push_str(lead);
            }
            lines::push_row(text, grid, r);
        }
        if !lines::wraps(grid, r) || r + 1 == stop {
            if line >= *next {
                writeln!(out, "{stamp}{}", text.trim_end())?;
                *next = line + 1;
            }
            text.clear();
            line += 1;
        }
    }
    Ok(())
}

/// All of a tab's text: every line in the scrollback and on screen.
pub fn all_text(grid: &Grid<Cell>) -> String {
    let mut out = Vec::new();
    let mut next = grid.lines_dropped();
    // Writing to a Vec can't fail.
    let _ = write_lines(&mut out, grid, Upto::All, &mut next, &mut None, "", &mut String::new(), "");
    String::from_utf8(out).unwrap_or_default()
}

/// The last `max` lines of a tab: history lines, then the live grid's (its
/// first line joined to its part in history). Also returns how many lines
/// that is and whether the tab has more.
pub fn tail_text(snap: &Snapshot, live: &Grid<Cell>, max: usize) -> (String, usize, bool) {
    let mut out = Vec::new();
    let mut next = live.lines_dropped();
    let lead = partial_text(snap);
    // Writing to a Vec can't fail.
    let _ = write_lines(&mut out, live, Upto::All, &mut next, &mut None, "", &mut String::new(), &lead);
    let live_text = String::from_utf8(out).unwrap_or_default();
    let live_lines = live_text.lines().count();
    if live_lines >= max {
        let more = live_lines > max || snap.end_line() > snap.first_line();
        let (text, _) = keep_last_lines(live_text, max);
        return (text, max, more);
    }
    let want = (max - live_lines) as u64;
    let end = snap.end_line();
    let from = end.saturating_sub(want).max(snap.first_line());
    let mut out = Vec::new();
    let _ = write_history(&mut out, snap, from..end, "");
    let mut text = String::from_utf8(out).unwrap_or_default();
    text.push_str(&live_text);
    // A lost range is one line standing for many: trim to `max` in lines.
    let (text, trimmed) = keep_last_lines(text, max);
    let lines = text.lines().count();
    (text, lines, trimmed || from > snap.first_line())
}

/// The last `max` lines of `text`, and whether lines were left out.
pub fn keep_last_lines(text: String, max: usize) -> (String, bool) {
    let count = text.lines().count();
    if count <= max {
        return (text, false);
    }
    let skip = count - max;
    let start = text.match_indices('\n').nth(skip - 1).map_or(0, |(i, _)| i + 1);
    (text[start..].to_owned(), true)
}

/// Writes all of a tab's history, then `live_text` (its grid's text, from
/// [`all_text`]), to a new file on a thread of its own; `done` gets the
/// result there.
pub fn save_text_in_background(path: PathBuf, snap: Snapshot, live_text: String, done: impl FnOnce(io::Result<()>) + Send + 'static) {
    std::thread::Builder::new()
        .name("save text".into())
        .spawn(move || {
            let result = (|| {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let file = OpenOptions::new().write(true).create_new(true).open(&path)?;
                let mut out = BufWriter::new(file);
                write_history(&mut out, &snap, snap.first_line()..snap.end_line(), "")?;
                out.write_all(partial_text(&snap).as_bytes())?;
                out.write_all(live_text.as_bytes())?;
                out.flush()
            })();
            done(result);
        })
        .expect("save thread");
}

/// The last row with any text on it, or 0.
fn last_text_row(grid: &Grid<Cell>) -> usize {
    (0..grid.total_lines()).rev().find(|&r| lines::row_has_text(grid, r)).unwrap_or(0)
}

/// Writes all of a tab's text to a new file.
pub fn save_text(path: &Path, grid: &Grid<Cell>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(all_text(grid).as_bytes())
}

/// The current local time, formatted with strftime-style `fmt`.
pub fn local_time(fmt: &str) -> String {
    jiff::Zoned::now().strftime(fmt).to_string()
}

/// A path in `dir` for tab `number` running `app` that doesn't exist yet:
/// `2-vim_2026-11-07_14-03-22.txt`, with `-2`, `-3`... added if needed.
pub fn new_file(dir: &Path, number: usize, app: &str, ext: &str) -> PathBuf {
    let app: String = app
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    let stem = format!("{number}-{app}_{}", local_time("%Y-%m-%d_%H-%M-%S"));
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    path
}

/// `path` with the home folder shown as `~`, for messages.
pub fn display_path(path: &Path) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h).ok().map(Path::to_owned)) {
        Some(rest) => Path::new("~").join(rest).display().to_string(),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::Processor;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fastcnsl-log-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct T {
        term: Term<VoidListener>,
        parser: Processor,
    }

    impl T {
        fn new(cols: usize, lines: usize, history: usize) -> Self {
            let cfg = TermConfig { scrolling_history: history, ..TermConfig::default() };
            Self { term: Term::new(cfg, &TermSize::new(cols, lines), VoidListener), parser: Processor::new() }
        }

        fn feed(&mut self, s: &str) {
            self.parser.advance(&mut self.term, s.replace('\n', "\r\n").as_bytes());
        }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn complete_lines_are_logged_once_and_the_rest_on_enter() {
        let dir = temp_dir("once");
        let mut t = T::new(10, 6, 100);
        t.feed("before\n$ ");
        let path = dir.join("a.log");
        let mut log = TabLog::create(&path, false).unwrap();
        log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
        assert_eq!(read(&path), "before\n", "what the tab had already; not the prompt yet");

        t.feed("ls");
        log.write_new(None, t.term.grid(), Upto::Cursor).unwrap(); // Enter
        t.feed("\nlong line wraps here\nb");
        log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
        log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
        assert_eq!(read(&path), "before\n$ ls\nlong line wraps here\n", "wrapped rows joined, nothing twice");
        assert_eq!(log.path(), path);

        log.write_new(None, t.term.grid(), Upto::Cursor).unwrap(); // logging stops
        assert_eq!(read(&path), "before\n$ ls\nlong line wraps here\nb\n");
        assert!(TabLog::create(&path, false).is_err(), "an existing file isn't overwritten");
    }

    #[test]
    fn logging_keeps_up_while_the_scrollback_overflows() {
        let dir = temp_dir("overflow");
        let mut t = T::new(20, 5, 10);
        let path = dir.join("b.log");
        let mut log = TabLog::create(&path, false).unwrap();
        for i in 1..=200 {
            t.feed(&format!("{i}\n"));
            if i % 9 == 0 {
                log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
            }
        }
        log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
        let want: String = (1..=200).map(|i| format!("{i}\n")).collect();
        assert_eq!(read(&path), want);

        // More output at once than the scrollback holds: the gap is noted.
        for i in 201..=300 {
            t.feed(&format!("{i}\n"));
        }
        log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
        let text = read(&path);
        assert!(text.contains("lines scrolled out of the scrollback"), "{text}");
        assert!(text.ends_with("299\n300\n"), "{text}");
    }

    #[test]
    fn logging_carries_on_after_a_resize_and_clear() {
        let dir = temp_dir("resize");
        let mut t = T::new(10, 5, 100);
        let path = dir.join("c.log");
        let mut log = TabLog::create(&path, false).unwrap();
        t.feed("aaaaaaaaaaaaaaa\nb\n");
        log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
        t.term.resize(TermSize::new(30, 5));
        log.invalidate();
        t.feed("c\n\x1b[H\x1b[2J\x1b[3Jd\n");
        log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
        // "c" was cleared away before this pass saw it.
        let text = read(&path);
        assert!(text.starts_with("aaaaaaaaaaaaaaa\nb\n[fastcnsl: 1 lines"), "{text}");
        assert!(text.ends_with("d\n"), "{text}");
    }

    #[test]
    fn timestamps_start_each_line() {
        let dir = temp_dir("stamp");
        let mut t = T::new(20, 5, 100);
        t.feed("one\ntwo\n");
        let path = dir.join("d.log");
        let mut log = TabLog::create(&path, true).unwrap();
        log.write_new(None, t.term.grid(), Upto::Complete).unwrap();
        for line in read(&path).lines() {
            let (stamp, rest) = line.split_at(11);
            assert!(stamp.starts_with('[') && stamp.ends_with("] ") && stamp.as_bytes()[3] == b':', "{line}");
            assert!(rest == "one" || rest == "two");
        }
    }

    #[test]
    fn saving_writes_all_text_including_below_the_cursor() {
        let dir = temp_dir("save");
        let mut t = T::new(10, 6, 100);
        t.feed("first\nsecond line, wrapped\n\n  indented  \x1b[1;1H");
        let path = dir.join("e.txt");
        save_text(&path, t.term.grid()).unwrap();
        // The cursor went back to the top, as a full-screen program's would.
        assert_eq!(read(&path), "first\nsecond line, wrapped\n\n  indented\n");
        assert_eq!(all_text(t.term.grid()), read(&path), "copied text is the same");
        assert!(save_text(&path, t.term.grid()).is_err(), "an existing file isn't overwritten");
    }

    #[test]
    fn file_names_are_new_and_safe() {
        let dir = temp_dir("names");
        let a = new_file(&dir, 2, "my app/x", "txt");
        let name = a.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("2-my_app_x_20") && name.ends_with(".txt"), "{name}");
        std::fs::write(&a, "").unwrap();
        let b = new_file(&dir, 2, "my app/x", "txt");
        assert_ne!(a, b);
        if let Some(home) = dirs::home_dir() {
            assert_eq!(display_path(&home.join("logs").join("x.log")), Path::new("~").join("logs").join("x.log").display().to_string());
        }
    }

    /// A tab that keeps its history (see `crate::history`).
    mod with_history {
        use std::time::{Duration, Instant};

        use super::*;
        use crate::history::scroll::settled;
        use crate::history::{History, Shared, Snapshot, start};
        use crate::testterm::{TestTerm, temp_dir};

        fn tab(tag: &str) -> (TestTerm, Shared) {
            let (history, sink) = start(History::new(4096, temp_dir(tag)).with_chunk_bytes(512));
            let mut t = TestTerm::new(20, 5, 5);
            t.term.grid_mut().set_row_sink(Box::new(sink));
            (t, history)
        }

        fn numbered(t: &mut TestTerm, from: usize, to: usize) {
            let text: String = (from..to).map(|i| format!("line {i}\n")).collect();
            t.feed(&text);
        }

        fn snap(t: &mut TestTerm, history: &Shared) -> Snapshot {
            settled(&mut t.term, history)
        }

        #[test]
        fn tail_text_takes_last_100k_from_history_and_live() {
            let (mut t, history) = tab("tail");
            numbered(&mut t, 0, 150_000);
            let snap = snap(&mut t, &history);
            let (text, lines, capped) = tail_text(&snap, t.term.grid(), COPY_LINES);
            assert!(capped);
            assert_eq!(lines, COPY_LINES);
            let got: Vec<&str> = text.lines().collect();
            assert_eq!(got.len(), COPY_LINES);
            // The cursor's empty line comes last, after line 149 999.
            assert_eq!(got[got.len() - 2], "line 149999");
            assert_eq!(got[0], "line 50001");
        }

        #[test]
        fn tail_text_joins_partial_line() {
            let (mut t, history) = tab("tail-partial");
            let long = "p".repeat(80);
            t.feed(&format!("{long}\na\nb\nc\nd\ne\nf\ng\n"));
            let snap = snap(&mut t, &history);
            assert!(!snap.partial().is_empty(), "the long line straddles the grid's top");
            let (text, _, capped) = tail_text(&snap, t.term.grid(), COPY_LINES);
            assert!(!capped);
            assert!(text.starts_with(&format!("{long}\na\n")), "{text}");
        }

        #[test]
        fn selection_copy_is_capped() {
            let text: String = (0..100_001).map(|i| format!("{i}\n")).collect();
            let (kept, capped) = keep_last_lines(text, COPY_LINES);
            assert!(capped);
            assert_eq!(kept.lines().count(), COPY_LINES);
            assert!(kept.starts_with("1\n"));
            let (kept, capped) = keep_last_lines("a\nb".to_owned(), COPY_LINES);
            assert_eq!((kept.as_str(), capped), ("a\nb", false));
        }

        #[test]
        fn save_writes_every_line() {
            let (mut t, history) = tab("save");
            numbered(&mut t, 0, 20_000);
            let snap = snap(&mut t, &history);
            let path = temp_dir("save-out").join("all.txt");
            let (tx, rx) = std::sync::mpsc::channel();
            save_text_in_background(path.clone(), snap, all_text(t.term.grid()), move |r| tx.send(r.is_ok()).unwrap());
            assert!(rx.recv_timeout(Duration::from_secs(20)).unwrap());
            let want: String = (0..20_000).map(|i| format!("line {i}\n")).collect::<String>() + "\n";
            assert!(read(&path) == want, "every line once, in order");
        }

        #[test]
        fn log_started_late_catches_up_without_gaps() {
            let (mut t, history) = tab("late-log");
            numbered(&mut t, 0, 120_000);
            let path = temp_dir("late-log-out").join("l.log");
            let mut log = TabLog::create(&path, false).unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut passes = 0;
            loop {
                let snap = log.behind(t.term.grid()).then(|| snap(&mut t, &history));
                passes += 1;
                if !log.write_new(snap.as_ref(), t.term.grid(), Upto::Complete).unwrap() {
                    break;
                }
                assert!(Instant::now() < deadline);
            }
            assert!(passes >= 3, "at most 50 000 lines a pass: {passes}");
            numbered(&mut t, 120_000, 120_010);
            let snap = log.behind(t.term.grid()).then(|| snap(&mut t, &history));
            log.write_new(snap.as_ref(), t.term.grid(), Upto::Complete).unwrap();
            let want: String = (0..120_010).map(|i| format!("line {i}\n")).collect();
            assert!(read(&path) == want, "every line once, no notice");
        }
    }
}

