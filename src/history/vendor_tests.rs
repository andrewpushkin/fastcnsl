//! Tests for the vendored grid's row sink (fastcnsl patch). They live here
//! because a patched dependency's own tests don't run.

use std::sync::{Arc, Mutex};

use alacritty_terminal::grid::{Dimensions, Grid, Row, RowSink, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::vte::ansi::Color;
#[derive(Default)]
struct Seen {
    firsts: Vec<char>,
    widths: Vec<usize>,
    cleared: Option<u64>,
}

struct TestSink(Arc<Mutex<Seen>>);

impl RowSink<Cell> for TestSink {
    fn dropped(&mut self, row: &mut Row<Cell>) {
        let mut seen = self.0.lock().unwrap();
        seen.firsts.push(row[Column(0)].c);
        seen.widths.push(row.len());
        // Swap in a fresh row, as the history's sink does.
        *row = Row::new(row.len());
    }

    fn cleared(&mut self, lines_dropped: u64) {
        self.0.lock().unwrap().cleared = Some(lines_dropped);
    }
}

fn grid_with_sink(lines: usize, columns: usize, history: usize) -> (Grid<Cell>, Arc<Mutex<Seen>>) {
    let mut grid = Grid::<Cell>::new(lines, columns, history);
    let seen = Arc::new(Mutex::new(Seen::default()));
    grid.set_row_sink(Box::new(TestSink(seen.clone())));
    (grid, seen)
}

/// Writes `c` on the top screen row and scrolls it into the history.
fn push(grid: &mut Grid<Cell>, c: char) {
    let lines = grid.screen_lines() as i32;
    grid[Line(0)][Column(0)].c = c;
    grid.scroll_up::<Color>(&(Line(0)..Line(lines)), 1);
}

#[test]
fn sink_gets_rows_leaving_a_full_history() {
    let (mut grid, seen) = grid_with_sink(3, 5, 2);
    for c in 'a'..='h' {
        push(&mut grid, c);
    }
    assert_eq!(seen.lock().unwrap().firsts, vec!['a', 'b', 'c', 'd', 'e', 'f']);
    assert_eq!(grid.rows_dropped(), 6);
}

#[test]
fn sink_gets_rows_cut_by_update_history() {
    let (mut grid, seen) = grid_with_sink(3, 5, 4);
    for c in 'a'..='d' {
        push(&mut grid, c);
    }
    assert!(seen.lock().unwrap().firsts.is_empty());
    grid.update_history(1);
    assert_eq!(seen.lock().unwrap().firsts, vec!['a', 'b', 'c']);
}

#[test]
fn sink_gets_rows_pushed_out_by_reflow() {
    let (mut grid, seen) = grid_with_sink(2, 4, 2);
    // Two full history rows and two full screen rows: narrowing to 2
    // columns doubles them, more than history + screen can hold.
    for c in ['a', 'b'] {
        for col in 0..4 {
            grid[Line(0)][Column(col)].c = c;
        }
        grid.scroll_up::<Color>(&(Line(0)..Line(2)), 1);
    }
    for (line, c) in [(0, 'c'), (1, 'd')] {
        for col in 0..4 {
            grid[Line(line)][Column(col)].c = c;
        }
    }
    grid.resize::<Color>(true, 2, 2);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.firsts, vec!['a', 'a', 'b', 'b'], "oldest first");
    assert!(seen.widths.iter().all(|&w| w == 2), "already reflowed: {:?}", seen.widths);
    assert_eq!(grid.rows_dropped(), 4);
}

#[test]
fn clear_history_calls_cleared_not_dropped() {
    let (mut grid, seen) = grid_with_sink(3, 5, 4);
    for c in 'a'..='c' {
        push(&mut grid, c);
    }
    grid.clear_history();
    let seen = seen.lock().unwrap();
    assert!(seen.firsts.is_empty());
    assert_eq!(seen.cleared, Some(grid.lines_dropped()));
    assert_eq!(grid.lines_dropped(), 3);
}

#[test]
fn set_content_installs_rows() {
    let mut grid = Grid::<Cell>::new(2, 3, 0);
    let rows: Vec<Row<Cell>> = ('p'..='t')
        .map(|c| {
            let mut row = Row::<Cell>::new(3);
            row[Column(0)].c = c;
            row
        })
        .collect();
    grid.set_content(rows, 7, 11);
    assert_eq!(grid.history_size(), 3);
    assert_eq!(grid[Line(-3)][Column(0)].c, 'p');
    assert_eq!(grid[Line(1)][Column(0)].c, 't');
    assert_eq!(grid.rows_dropped(), 7);
    assert_eq!(grid.lines_dropped(), 11);
    assert_eq!(grid.display_offset(), 0);
    assert_eq!(grid.cursor.point, Point::new(Line(1), Column(0)));
    // Scrolling the display reaches the first row.
    grid.scroll_display(Scroll::Top);
    assert_eq!(grid.display_offset(), 3);
}

#[test]
fn swapped_rows_come_back_clean() {
    let (mut grid, _seen) = grid_with_sink(3, 5, 2);
    for c in 'a'..='h' {
        push(&mut grid, c);
    }
    // The row the sink swapped in last is the new, blank bottom row.
    assert_eq!(grid[Line(2)][Column(0)], Cell::default());
    assert_eq!(grid[Line(-2)][Column(0)].c, 'g');
    assert_eq!(grid[Line(-1)][Column(0)].c, 'h');
}
