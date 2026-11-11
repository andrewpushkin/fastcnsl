//! Saving a tab's text to a file, once or continuously.
//!
//! Text is written as lines (wrapped rows joined, trailing blanks
//! removed), the way programs printed it. A continuous log appends each
//! line once it is complete, i.e. once the cursor has moved past it; the
//! cursor's own line is written when Enter is pressed or logging stops.
//! Lines are told apart by their global index (see `lines.rs`), so none is
//! written twice and the log keeps going as old lines leave the scrollback.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::term::cell::Cell;

use crate::lines::{self, Pos};

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

    /// Writes the lines not written yet, up to `upto`, and flushes them to
    /// the file. Pass the normal screen's grid, except to save a
    /// full-screen program's screen once.
    pub fn write_new(&mut self, grid: &Grid<Cell>, upto: Upto) -> io::Result<()> {
        let dropped = grid.lines_dropped();
        let next = self.next.get_or_insert(dropped);
        if *next < dropped {
            let lost = dropped - *next;
            writeln!(self.out, "[fastcnsl: {lost} lines scrolled out of the scrollback before they could be logged]")?;
            *next = dropped;
        }

        let stamp = if self.timestamps { format!("[{}] ", local_time("%H:%M:%S")) } else { String::new() };
        write_lines(&mut self.out, grid, upto, next, &mut self.pos, &stamp, &mut self.text)?;
        self.out.flush()
    }
}

/// Writes the lines from global index `*next` on, up to `upto`, each after
/// `stamp`, and moves `*next` past them. `pos` is the counting position
/// (see `lines::ends_before`); `text` a reusable buffer.
fn write_lines(
    out: &mut impl Write,
    grid: &Grid<Cell>,
    upto: Upto,
    next: &mut u64,
    pos: &mut Option<Pos>,
    stamp: &str,
    text: &mut String,
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
    let _ = write_lines(&mut out, grid, Upto::All, &mut next, &mut None, "", &mut String::new());
    String::from_utf8(out).unwrap_or_default()
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
        log.write_new(t.term.grid(), Upto::Complete).unwrap();
        assert_eq!(read(&path), "before\n", "what the tab had already; not the prompt yet");

        t.feed("ls");
        log.write_new(t.term.grid(), Upto::Cursor).unwrap(); // Enter
        t.feed("\nlong line wraps here\nb");
        log.write_new(t.term.grid(), Upto::Complete).unwrap();
        log.write_new(t.term.grid(), Upto::Complete).unwrap();
        assert_eq!(read(&path), "before\n$ ls\nlong line wraps here\n", "wrapped rows joined, nothing twice");
        assert_eq!(log.path(), path);

        log.write_new(t.term.grid(), Upto::Cursor).unwrap(); // logging stops
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
                log.write_new(t.term.grid(), Upto::Complete).unwrap();
            }
        }
        log.write_new(t.term.grid(), Upto::Complete).unwrap();
        let want: String = (1..=200).map(|i| format!("{i}\n")).collect();
        assert_eq!(read(&path), want);

        // More output at once than the scrollback holds: the gap is noted.
        for i in 201..=300 {
            t.feed(&format!("{i}\n"));
        }
        log.write_new(t.term.grid(), Upto::Complete).unwrap();
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
        log.write_new(t.term.grid(), Upto::Complete).unwrap();
        t.term.resize(TermSize::new(30, 5));
        log.invalidate();
        t.feed("c\n\x1b[H\x1b[2J\x1b[3Jd\n");
        log.write_new(t.term.grid(), Upto::Complete).unwrap();
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
        log.write_new(t.term.grid(), Upto::Complete).unwrap();
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
}
