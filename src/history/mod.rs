//! A tab's whole history: the lines that left the live grid.
//!
//! The vendored grid hands every row leaving its top to [`Sink`], which
//! joins rows into lines ([`line`]) and appends them to [`History`]. Lines
//! are kept in chunks of about a megabyte: in RAM while they fit in the
//! tab's budget, then in a file ([`disk`]) that the OS deletes when the
//! process ends. Lines are numbered like `lines.rs` numbers them: from the
//! start of the tab, so the grid's `lines_dropped()` is where history ends.

pub mod disk;
pub mod line;
#[cfg(test)]
mod vendor_tests;

use std::io;
use std::ops::{ControlFlow, Range};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use alacritty_terminal::grid::{Row, RowSink};
use alacritty_terminal::term::cell::Cell;

use self::disk::DiskFile;
use self::line::Encoder;

/// Where history files go: the user's cache folder, not the temp folder,
/// which is often kept in RAM.
pub fn history_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|d| d.join("fastcnsl").join("history"))
        .unwrap_or_else(|| std::env::temp_dir().join("fastcnsl-history"))
}

/// Bytes of one grid cell.
pub const CELL_BYTES: usize = std::mem::size_of::<Cell>();

/// The text of the line that stands for `count` lines lost to a disk error.
pub fn lost_text(count: u64) -> String {
    format!("[fastcnsl: {count} lines lost: disk full]")
}

/// A tab's RAM for text, and how it is shared out: a quarter for the live
/// grid, a quarter for the window onto older lines, half for the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub bytes: usize,
}

impl Budget {
    pub fn from_mb(mb: usize) -> Self {
        Self { bytes: mb.clamp(64, 16384) << 20 }
    }

    /// History rows the live grid keeps at `columns`.
    pub fn live_rows(&self, columns: usize) -> usize {
        ((self.bytes / 4) / (columns.max(1) * CELL_BYTES)).clamp(100, 10_000)
    }

    /// Rows a window onto older lines may have at `columns`.
    pub fn window_rows(&self, columns: usize) -> usize {
        ((self.bytes / 4) / (columns.max(1) * CELL_BYTES)).max(200)
    }

    /// RAM the history may use.
    pub fn history_ram(&self) -> usize {
        self.bytes / 2
    }
}

/// Where a chunk of lines is.
#[derive(Clone)]
enum Place {
    Ram(Arc<[u8]>),
    Disk { offset: u64, len: u32 },
    /// Lost to a disk error: one marker stands for all its lines.
    Lost,
}

/// A sealed chunk: lines `first..first + count`.
#[derive(Clone)]
struct Entry {
    first: u64,
    count: u64,
    place: Place,
}

/// Lines being added, not sealed into a chunk yet.
#[derive(Default)]
struct Open {
    data: Vec<u8>,
    /// Where each line starts in `data`.
    starts: Vec<u32>,
}

impl Open {
    fn bytes(&self) -> usize {
        self.data.len() + self.starts.len() * 4
    }

    /// The chunk layout: line bytes, a u32 start per line, the line count.
    fn seal(&self) -> Arc<[u8]> {
        let mut out = Vec::with_capacity(self.bytes() + 4);
        out.extend_from_slice(&self.data);
        for s in &self.starts {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out.extend_from_slice(&(self.starts.len() as u32).to_le_bytes());
        out.into()
    }
}

/// Line `i` of a sealed chunk.
fn chunk_line(chunk: &[u8], i: usize) -> &[u8] {
    let n = u32::from_le_bytes(chunk[chunk.len() - 4..].try_into().expect("4 bytes")) as usize;
    let table = chunk.len() - 4 - n * 4;
    let start_of = |k: usize| u32::from_le_bytes(chunk[table + k * 4..table + k * 4 + 4].try_into().expect("4 bytes")) as usize;
    let end = if i + 1 < n { start_of(i + 1) } else { table };
    &chunk[start_of(i)..end]
}

/// One tab's archived lines.
pub struct History {
    chunk_bytes: usize,
    ram_limit: usize,
    dir: PathBuf,
    entries: Vec<Entry>,
    open: Open,
    /// The line being dropped row by row, encoded so far.
    partial: Vec<u8>,
    partial_enc: Encoder,
    first: u64,
    end: u64,
    generation: u64,
    /// Bytes of the chunks in RAM.
    ram: usize,
    disk: Option<Arc<DiskFile>>,
    disk_warned: bool,
}

pub type Shared = Arc<Mutex<History>>;

impl History {
    pub fn new(ram_limit: usize, dir: PathBuf) -> Self {
        Self {
            chunk_bytes: 1 << 20,
            ram_limit,
            dir,
            entries: Vec::new(),
            open: Open::default(),
            partial: Vec::new(),
            partial_enc: Encoder::default(),
            first: 0,
            end: 0,
            generation: 0,
            ram: 0,
            disk: None,
            disk_warned: false,
        }
    }

    #[cfg(test)]
    pub fn with_chunk_bytes(mut self, n: usize) -> Self {
        self.chunk_bytes = n;
        self
    }

    pub fn ram_limit(&self) -> usize {
        self.ram_limit
    }

    pub fn set_ram_limit(&mut self, bytes: usize) {
        self.ram_limit = bytes;
        self.spill();
    }

    /// The oldest line still held.
    pub fn first_line(&self) -> u64 {
        self.first
    }

    /// One past the newest complete line: the grid's `lines_dropped()`.
    pub fn end_line(&self) -> u64 {
        self.end
    }

    /// The encoded cells of the line whose first rows left the grid while
    /// its last row is still there.
    pub fn partial(&self) -> &[u8] {
        &self.partial
    }

    /// Goes up by one each time the history is cleared.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// RAM in use: chunks in RAM, the open chunk, the partial line and the
    /// disk read cache.
    pub fn ram_bytes(&self) -> usize {
        self.ram + self.open.bytes() + self.partial.len() + self.disk.as_ref().map_or(0, |d| d.cached_bytes())
    }

    #[cfg(test)]
    pub fn disk_entries(&self) -> usize {
        self.entries.iter().filter(|e| matches!(e.place, Place::Disk { .. })).count()
    }

    /// Appends rows leaving the grid, oldest first.
    pub fn push_rows<'a>(&mut self, rows: impl Iterator<Item = &'a Row<Cell>>) {
        for row in rows {
            if self.partial_enc.encode_row(row, &mut self.partial) {
                self.open.starts.push(self.open.data.len() as u32);
                self.open.data.extend_from_slice(&self.partial);
                self.partial.clear();
                self.partial_enc = Encoder::default();
                self.end += 1;
                if self.open.data.len() >= self.chunk_bytes {
                    self.seal();
                }
            }
        }
    }

    /// Forgets everything: the grid's history was cleared and it has now
    /// dropped `lines_dropped` lines in all.
    pub fn clear(&mut self, lines_dropped: u64) {
        self.entries.clear();
        self.open = Open::default();
        self.partial.clear();
        self.partial_enc = Encoder::default();
        self.first = lines_dropped;
        self.end = lines_dropped;
        self.ram = 0;
        // Snapshots still reading it keep the file until they are done.
        self.disk = None;
        self.generation += 1;
    }

    fn seal(&mut self) {
        if self.open.starts.is_empty() {
            return;
        }
        let count = self.open.starts.len() as u64;
        let chunk = self.open.seal();
        self.ram += chunk.len();
        self.entries.push(Entry { first: self.end - count, count, place: Place::Ram(chunk) });
        self.open = Open::default();
        self.spill();
    }

    /// Moves the oldest chunks in RAM to disk until the RAM limit holds.
    /// Chunks that can't be written are lost.
    fn spill(&mut self) {
        let mut i = 0;
        while self.ram_bytes() > self.ram_limit && i < self.entries.len() {
            let Place::Ram(chunk) = &self.entries[i].place else {
                i += 1;
                continue;
            };
            let chunk = chunk.clone();
            let written = self.disk().and_then(|d| d.append(&chunk).map(|offset| Place::Disk { offset, len: chunk.len() as u32 }));
            let place = match written {
                Ok(place) => place,
                Err(e) => {
                    if !self.disk_warned {
                        log::warn!("history file in {}: {e}; dropping the oldest lines", self.dir.display());
                        self.disk_warned = true;
                    }
                    Place::Lost
                }
            };
            self.ram -= chunk.len();
            self.entries[i].place = place;
            // Lost ranges next to each other become one.
            if i > 0 && matches!(self.entries[i].place, Place::Lost) && matches!(self.entries[i - 1].place, Place::Lost) {
                let gone = self.entries.remove(i);
                self.entries[i - 1].count += gone.count;
            } else {
                i += 1;
            }
        }
    }

    /// The history file, created when it is first needed.
    fn disk(&mut self) -> io::Result<Arc<DiskFile>> {
        if let Some(d) = &self.disk {
            return Ok(d.clone());
        }
        let d = DiskFile::create(&self.dir)?;
        self.disk = Some(d.clone());
        Ok(d)
    }

    /// A copy of the history to read from any thread.
    pub fn snapshot(&self) -> Snapshot {
        let mut entries = self.entries.clone();
        if !self.open.starts.is_empty() {
            let count = self.open.starts.len() as u64;
            entries.push(Entry { first: self.end - count, count, place: Place::Ram(self.open.seal()) });
        }
        Snapshot {
            entries: entries.into(),
            first: self.first,
            end: self.end,
            generation: self.generation,
            partial: self.partial.as_slice().into(),
            disk: self.disk.clone(),
        }
    }
}

/// Hands the rows leaving a grid to a [`History`].
pub struct Sink(pub Shared);

impl RowSink<Cell> for Sink {
    fn dropped(&mut self, rows: &mut dyn Iterator<Item = &Row<Cell>>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push_rows(rows);
    }

    fn cleared(&mut self, lines_dropped: u64) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clear(lines_dropped);
    }
}

/// One line, or a range of lines lost to a disk error.
pub enum Item<'a> {
    /// The encoded line (see [`line`]).
    Line(&'a [u8]),
    /// This many lines are lost; reported once, at the range's first line.
    Lost(u64),
}

/// The history as it was when taken; readable from any thread.
#[derive(Clone)]
pub struct Snapshot {
    entries: Arc<[Entry]>,
    first: u64,
    end: u64,
    generation: u64,
    partial: Arc<[u8]>,
    disk: Option<Arc<DiskFile>>,
}

impl Snapshot {
    pub fn first_line(&self) -> u64 {
        self.first
    }

    pub fn end_line(&self) -> u64 {
        self.end
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn partial(&self) -> &[u8] {
        &self.partial
    }

    /// Calls `f(line, item)` for each line in `range` (clamped to what the
    /// snapshot holds) in order, or in reverse order with `rev`, until it
    /// returns `Break`. A lost range is reported once, at its first line.
    pub fn for_each(&self, range: Range<u64>, rev: bool, mut f: impl FnMut(u64, Item<'_>) -> ControlFlow<()>) -> io::Result<()> {
        let range = range.start.max(self.first)..range.end.min(self.end);
        if range.is_empty() {
            return Ok(());
        }
        let lo = self.entries.partition_point(|e| e.first + e.count <= range.start);
        let hi = self.entries.partition_point(|e| e.first < range.end);
        let mut visit = |e: &Entry| -> io::Result<ControlFlow<()>> {
            let from = range.start.max(e.first);
            let to = range.end.min(e.first + e.count);
            let chunk = match &e.place {
                Place::Lost => {
                    return Ok(if range.contains(&e.first) { f(e.first, Item::Lost(e.count)) } else { ControlFlow::Continue(()) });
                }
                Place::Ram(chunk) => chunk.clone(),
                Place::Disk { offset, len } => match &self.disk {
                    Some(d) => d.read(*offset, *len)?,
                    None => return Err(io::Error::other("history file gone")),
                },
            };
            let mut each = |line: u64| f(line, Item::Line(chunk_line(&chunk, (line - e.first) as usize)));
            if rev {
                for line in (from..to).rev() {
                    if each(line).is_break() {
                        return Ok(ControlFlow::Break(()));
                    }
                }
            } else {
                for line in from..to {
                    if each(line).is_break() {
                        return Ok(ControlFlow::Break(()));
                    }
                }
            }
            Ok(ControlFlow::Continue(()))
        };
        if rev {
            for e in self.entries[lo..hi].iter().rev() {
                if visit(e)?.is_break() {
                    break;
                }
            }
        } else {
            for e in &self.entries[lo..hi] {
                if visit(e)?.is_break() {
                    break;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;


    use super::*;
    use crate::testterm::{TestTerm, temp_dir};

    const CHUNK: usize = 512;
    const RAM: usize = 4096;

    fn setup(tag: &str, ram: usize) -> (TestTerm, Shared, std::path::PathBuf) {
        let dir = temp_dir(tag);
        let history: Shared = Arc::new(Mutex::new(History::new(ram, dir.clone()).with_chunk_bytes(CHUNK)));
        let mut t = TestTerm::new(20, 5, 5);
        t.term.grid_mut().set_row_sink(Box::new(Sink(history.clone())));
        (t, history, dir)
    }

    fn numbered(i: usize) -> String {
        if i % 7 == 3 { format!("line {i} {}", "x".repeat(45)) } else { format!("line {i}") }
    }

    fn feed_numbered(t: &mut TestTerm, from: usize, to: usize) {
        let mut s = String::new();
        for i in from..to {
            s.push_str(&numbered(i));
            s.push('\n');
        }
        t.feed(&s);
    }

    /// Text of every line in the snapshot, markers for lost ones.
    fn texts(snap: &Snapshot) -> Vec<String> {
        let mut out = Vec::new();
        snap.for_each(snap.first_line()..snap.end_line(), false, |_, item| {
            out.push(match item {
                Item::Line(bytes) => {
                    let mut s = String::new();
                    line::text(bytes, &mut s);
                    s.trim_end().to_owned()
                }
                Item::Lost(n) => lost_text(n),
            });
            ControlFlow::Continue(())
        })
        .expect("readable");
        out
    }

    #[test]
    fn every_line_is_kept_in_order() {
        let (mut t, history, _dir) = setup("order", RAM);
        feed_numbered(&mut t, 0, 20_000);
        let h = history.lock().unwrap();
        assert_eq!(h.end_line(), t.term.grid().lines_dropped());
        assert_eq!(h.first_line(), 0);
        let got = texts(&h.snapshot());
        assert_eq!(got.len() as u64, h.end_line());
        for (i, line) in got.iter().enumerate() {
            assert_eq!(line, &numbered(i));
        }
    }

    #[test]
    fn ram_stays_under_limit() {
        let (mut t, history, _dir) = setup("ram", RAM);
        for k in 0..20 {
            feed_numbered(&mut t, k * 1000, (k + 1) * 1000);
            let ram = history.lock().unwrap().ram_bytes();
            assert!(ram <= RAM + CHUNK, "after {}k lines: {ram} bytes", k + 1);
        }
    }

    #[test]
    fn spilled_lines_come_back_from_disk() {
        let (mut t, history, _dir) = setup("disk", RAM);
        feed_numbered(&mut t, 0, 5000);
        let h = history.lock().unwrap();
        assert!(h.disk_entries() > 0);
        let got = texts(&h.snapshot());
        assert_eq!(got[0], numbered(0));
        assert_eq!(got[4321], numbered(4321));
    }

    #[test]
    fn partial_line_completes_later() {
        let (mut t, history, _dir) = setup("partial", RAM);
        // A line of 4 rows, then 2 more lines: 2 of its rows leave the 10
        // row grid (5 history + 5 screen) only after more output.
        t.feed(&format!("{}\n", "p".repeat(80)));
        t.feed("a\nb\nc\nd\ne\nf\ng\n");
        {
            let h = history.lock().unwrap();
            assert_eq!(h.end_line(), 0, "the long line is not complete in history yet");
            let mut cells = Vec::new();
            line::decode(h.partial(), &mut cells);
            assert_eq!(cells.len(), 40, "two rows of it left the grid");
        }
        t.feed("h\ni\n");
        let h = history.lock().unwrap();
        assert_eq!(h.end_line(), t.term.grid().lines_dropped());
        assert_eq!(texts(&h.snapshot())[0], "p".repeat(80));
    }

    #[test]
    fn clear_wipes_history() {
        let (mut t, history, _dir) = setup("clear", RAM);
        feed_numbered(&mut t, 0, 3000);
        let generation = history.lock().unwrap().generation();
        t.feed("\x1b[3J");
        let h = history.lock().unwrap();
        assert_eq!(h.first_line(), t.term.grid().lines_dropped());
        assert_eq!(h.end_line(), h.first_line());
        assert_eq!(h.generation(), generation + 1);
        assert!(texts(&h.snapshot()).is_empty());
        drop(h);
        feed_numbered(&mut t, 3000, 3100);
        let h = history.lock().unwrap();
        assert_eq!(h.end_line(), t.term.grid().lines_dropped());
        // The screen rows at the time of the clear come first, then the new lines.
        let got = texts(&h.snapshot());
        assert_eq!(got.len() as u64, h.end_line() - h.first_line());
        assert_eq!(got.last().unwrap(), &numbered(h.end_line() as usize - 1));
    }

    #[test]
    fn giant_line_spans_its_own_chunk() {
        let (mut t, history, _dir) = setup("giant", RAM);
        let giant: String = (0..2_000_000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        t.feed(&giant);
        t.feed("\n");
        feed_numbered(&mut t, 0, 20);
        let h = history.lock().unwrap();
        let got = texts(&h.snapshot());
        assert_eq!(got[0].len(), giant.len());
        assert!(got[0] == giant);
        assert_eq!(got[1], numbered(0));
    }

    #[test]
    fn unwritable_dir_loses_oldest_with_marker() {
        let base = temp_dir("unwritable");
        let file = base.join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let history: Shared = Arc::new(Mutex::new(History::new(RAM, file.join("sub")).with_chunk_bytes(CHUNK)));
        let mut t = TestTerm::new(20, 5, 5);
        t.term.grid_mut().set_row_sink(Box::new(Sink(history.clone())));
        feed_numbered(&mut t, 0, 5000);
        let h = history.lock().unwrap();
        assert!(h.ram_bytes() <= RAM + CHUNK);
        let snap = h.snapshot();
        let mut first = None;
        snap.for_each(snap.first_line()..snap.end_line(), false, |line, item| {
            first = Some((line, matches!(item, Item::Lost(n) if n > 0)));
            ControlFlow::Break(())
        })
        .unwrap();
        assert_eq!(first, Some((0, true)), "a lost range at the start");
        let got = texts(&snap);
        assert!(got[0].starts_with("[fastcnsl: ") && got[0].ends_with(" lines lost: disk full]"), "{}", got[0]);
        assert_eq!(got.last().unwrap(), &numbered(h.end_line() as usize - 1));
    }

    #[test]
    fn snapshot_reads_from_another_thread() {
        let (mut t, history, _dir) = setup("thread", RAM);
        feed_numbered(&mut t, 0, 3000);
        let snap = history.lock().unwrap().snapshot();
        let got = std::thread::spawn(move || texts(&snap)).join().unwrap();
        assert_eq!(got.len() as u64, t.term.grid().lines_dropped());
        assert_eq!(got[2999.min(got.len() - 1)], numbered(got.len() - 1));
    }

    #[test]
    fn reverse_reading_gives_the_same_lines_backwards() {
        let (mut t, history, _dir) = setup("rev", RAM);
        feed_numbered(&mut t, 0, 2000);
        let snap = history.lock().unwrap().snapshot();
        let mut lines = Vec::new();
        snap.for_each(100..1500, true, |line, _| {
            lines.push(line);
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(lines, (100..1500).rev().collect::<Vec<u64>>());
    }
}
