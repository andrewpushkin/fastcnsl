//! A terminal without a shell for tests: feed it text, look at its grid.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Row};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Processor;

use crate::lines;

pub struct TestTerm {
    pub term: Term<VoidListener>,
    parser: Processor,
}

impl TestTerm {
    pub fn new(cols: usize, lines: usize, history: usize) -> Self {
        let cfg = TermConfig { scrolling_history: history, ..TermConfig::default() };
        Self { term: Term::new(cfg, &TermSize::new(cols, lines), VoidListener), parser: Processor::new() }
    }

    /// Feeds `s` as a program's output, `\n` as `\r\n`.
    pub fn feed(&mut self, s: &str) {
        self.parser.advance(&mut self.term, s.replace('\n', "\r\n").as_bytes());
    }

    pub fn resize(&mut self, cols: usize, lines: usize) {
        self.term.resize(TermSize::new(cols, lines));
    }

    /// The rows of the line that starts at absolute row `first_abs`.
    pub fn rows_of_line(&self, first_abs: usize) -> Vec<&Row<Cell>> {
        let grid = self.term.grid();
        let mut out = Vec::new();
        let mut abs = first_abs;
        loop {
            out.push(&grid[lines::line_of(grid, abs)]);
            if !lines::wraps(grid, abs) || abs + 1 >= grid.total_lines() {
                return out;
            }
            abs += 1;
        }
    }

    /// The first absolute row of every line up to the cursor's.
    pub fn line_starts(&self) -> Vec<usize> {
        let grid = self.term.grid();
        let last = lines::cursor_row(grid);
        (0..=last).filter(|&abs| abs == 0 || !lines::wraps(grid, abs - 1)).collect()
    }
}

/// A new empty directory for one test, under the system temp dir.
pub fn temp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("fastcnsl-test-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}
