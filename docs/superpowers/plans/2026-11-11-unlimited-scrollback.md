# Unlimited Scrollback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every tab keeps its whole output, holding at most 500 MB in RAM, with older lines in a per-tab disk file. Every scrollback feature works across all of it.

**Architecture:** The alacritty grid stays a bounded *live* window. A vendored-crate hook hands every row that leaves its top to a per-tab `History`. `History` joins the rows into logical lines and stores them compactly in 1 MB chunks: in RAM up to a limit, then in a file the OS deletes when the process ends. When the user scrolls above the live grid, the tab shows a *window*: a separate read-only `Term` filled with history lines wrapped at the current width (and the top of the live grid when it reaches it). Rendering, selection, on-screen search and line numbers then run unchanged on that `Term`. Positions are carried between live grid, window and history as `At { line, sub }`: a global line index plus a wrapped row within it.

**Tech Stack:** Rust 2024, vendored `alacritty_terminal` 0.26 (patched), `regex-automata` 0.4 (already in the lock file), `syntect`, `wgpu`/`winit` (unchanged).

**Spec:** `docs/superpowers/specs/2026-11-11-unlimited-scrollback-design.md`

## Global Constraints

- Per-tab text RAM ≤ `memory_per_tab_mb` (default **500**, clamp **64..=16384**), applied live.
- Split of the budget: live grid ≤ ¼, window ≤ ¼, history RAM (chunks, open chunk, partial line, disk read cache) ≤ ½.
- Live grid history rows = `min(10_000, (budget/4) / (columns * size_of::<Cell>()))`, at least 100.
- History is never dropped except by clear (`ESC[3J`, `clear`, reset) or a failed disk write.
- History files: `dirs::cache_dir()/fastcnsl/history/` (fallback `std::env::temp_dir()/fastcnsl-history/`). Directory mode 0o700 and file mode 0o600 on Unix. Unix: the file is unlinked right after creation. Windows: `FILE_FLAG_DELETE_ON_CLOSE` (0x04000000) | `FILE_ATTRIBUTE_TEMPORARY` (0x100). Either way no file outlives the process, even after a crash.
- "Copy all text" and selection copy: at most the **last 100 000 lines**.
- Search match count cap stays **9999** (shown "9999+"). While the history part is still being counted, the count shows **"…"**.
- Disk failure keeps one marker line per lost range: `[fastcnsl: N lines lost: disk full]`.
- No UI-thread stall over ~100 ms from history work. Throughput within ~5 % of `docs/BENCHMARK.md` (ascii/color/unicode/lines).
- Each vendored change is marked `fastcnsl patch` and described in `vendor/README.md`.
- The old `scrollback` key in config files still loads (ignored).
- Commit after every task. Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Resize while scrolled into history.** The same line stays at the top. Text rewraps. Code colours disappear, as they do today for reflowed text. → Task 5 test `resize_in_window_keeps_top_line`.
2. **Output streaming while scrolled back.** The view stays on the same lines, the scrollbar grows, and End/typing returns to the live bottom with nothing missing or doubled. → Task 5 test `output_while_in_window_keeps_view_and_bottom_is_complete`.
3. **One enormous line** (a 2 MB line with no newline): it archives, scrolls through row by row, and the window can start in the middle of it. → Task 3 test `giant_line_spans_its_own_chunk` and Task 5 test `window_starts_mid_line`.
4. **Unwritable or full disk**: no panic, output continues, one marker line, everything later still readable. → Task 3 test `unwritable_dir_loses_oldest_with_marker`.
5. **Clear while scrolled back or while a history search runs**: the view returns to the live bottom, the search job is cancelled, bookmarks above the clear are gone. → Task 5 test `clear_leaves_window`, Task 8 test `clear_cancels_history_search`.

---

## File Structure

| File | Responsibility |
|---|---|
| `vendor/alacritty_terminal/src/grid/mod.rs`, `grid/resize.rs` | patch: `RowSink` hook on dropped/cleared rows, `Grid::set_content` |
| `vendor/README.md` | document the patch |
| `src/history/mod.rs` | `History`, `Shared`, `Sink`, `Budget`, `Snapshot`, `Item`, chunk index, spill |
| `src/history/line.rs` | join rows → cells, encode/decode a line, wrap at a width, text |
| `src/history/disk.rs` | `DiskFile`: self-deleting append/positional-read file, read cache |
| `src/history/window.rs` | `At`, `Window` (read-only `Term` over a range of history) |
| `src/history/scroll.rs` | `Scroller`: live/window placement, scroll ops, scrollbar metrics, selection carry |
| `src/history/search.rs` | background history search (step, count), cancellation |
| `src/testterm.rs` | `#[cfg(test)]` `TestTerm` helper shared by new tests |
| `src/session.rs` | owns `history::Shared`, installs sink, live rows from budget |
| `src/config.rs`, `README.md` | `memory_per_tab_mb` replaces `scrollback` |
| `src/lines.rs` | pruning by history first line; bookmark jumps return global lines |
| `src/highlight.rs` | blocks keep global line ranges; `code_ranges`, `highlight_lines`; `CodeRows` trait |
| `src/view.rs` | `View.code: &dyn CodeRows` |
| `src/search.rs` | stepping/count across history |
| `src/logging.rs` | tail text (100 k lines), history-aware logging, streamed save |
| `src/app.rs` | `Tab` gets `scroller`, `searcher`; all term access through displayed/live accessors |

---

### Task 1: Vendored grid hook for dropped rows

**Files:**
- Modify: `vendor/alacritty_terminal/src/grid/mod.rs` (struct `Grid` ~108-150, `update_history` 168, `scroll_up` ~290, `clear_history` 432, `count_dropped` 511)
- Modify: `vendor/alacritty_terminal/src/grid/resize.rs:366-373`
- Modify: `vendor/README.md`
- Test: `vendor/alacritty_terminal/src/grid/tests.rs` (existing test module of the grid)

**Interfaces:**
- Produces:
  ```rust
  // alacritty_terminal::grid
  pub trait RowSink<T>: Send {
      /// `rows` are leaving the top of the grid, oldest first (scrollback full,
      /// history shortened, reflow). Called before they are reused.
      fn dropped(&mut self, rows: &mut dyn Iterator<Item = &Row<T>>);
      /// The history was cleared (ESC[3J, reset). `lines_dropped` is the
      /// grid's `lines_dropped()` after the clear.
      fn cleared(&mut self, lines_dropped: u64);
  }
  impl<T> Grid<T> {
      pub fn set_row_sink(&mut self, sink: Box<dyn RowSink<T>>);
      /// Replaces all rows (top to bottom, `rows.len() >= screen_lines`): the
      /// last `screen_lines` are the screen, the rest history;
      /// max_scroll_limit = history; display_offset = 0; cursor at the last
      /// row, column 0; the dropped counters set to the given values.
      pub fn set_content(&mut self, rows: Vec<Row<T>>, rows_dropped: u64, lines_dropped: u64);
  }
  ```
- The sink lives in a field wrapper `SinkSlot<T>(Option<Box<dyn RowSink<T>>>)`, serde-skipped. Its `Clone` yields `None` and its `Debug` prints `"RowSink"`, so `Grid`'s derives keep working.

- [ ] **Step 1: Write failing tests** in `grid/tests.rs`, with a test sink that records each row's first char, and a `cleared` value:
  - `sink_gets_rows_leaving_a_full_history`: `Grid::<Cell>::new(3, 5, 2)`, write `a`…`h` on successive lines via `scroll_up`. Assert the sink got `a,b,c` in that order, and `grid.rows_dropped() == 3`.
  - `sink_gets_rows_cut_by_update_history`: history of 4 full rows, `update_history(1)` → sink gets the 3 oldest.
  - `sink_gets_rows_pushed_out_by_reflow`: narrowing columns so reflow exceeds `max_scroll_limit + lines`. The sink gets the overflow rows oldest first, already reflowed to the new width.
  - `clear_history_calls_cleared_not_dropped`: `clear_history()`, so `dropped` is never called and `cleared(lines_dropped)` equals `grid.lines_dropped()`.
  - `set_content_installs_rows`: 5 rows, 2 screen lines → `history_size() == 3`, `grid[Line(-3)]` is row 0, and `lines_dropped()` equals the value passed.
- [ ] **Step 2:** `cargo test -p alacritty_terminal sink_ set_content` → FAIL (unresolved `RowSink`).
- [ ] **Step 3: Implement.**
  - `count_dropped(count)` keeps counting and feeds the same `count` rows from `topmost_line()` to the sink.
  - `clear_history` must not call the sink's `dropped`. Split it out with a private `count_dropped_rows(count, archive: bool)`, then call `cleared` after counting.
  - In `resize.rs`, feed `reversed[keep..].iter().rev()` before `truncate`.
  - `set_content` uses `Storage::replace_inner` with the rows reversed (storage keeps bottom first; mirror what `resize.rs` does).
  - Document the patch in `vendor/README.md` under a new "### Row sink" heading.
- [ ] **Step 4:** `cargo test -p alacritty_terminal` → all pass. Then `cargo test` → all pass.
- [ ] **Step 5: Commit** `vendor: hand rows leaving the grid to a sink; Grid::set_content`.

---

### Task 2: Compact line encoding

**Files:**
- Create: `src/history/line.rs`, `src/history/mod.rs` (just `pub mod line;` for now), `src/testterm.rs`
- Modify: `src/main.rs` (`mod history; #[cfg(test)] mod testterm;`)

**Interfaces:**
- Produces (`crate::history::line`):
  ```rust
  /// Appends one logical line's cells: every row but the last loses its
  /// WRAPLINE flag and a LEADING_WIDE_CHAR_SPACER at its end; the last row's
  /// trailing default cells (' ', default fg/bg, no flags, no extra) are dropped.
  pub fn join_rows<'a>(rows: impl IntoIterator<Item = &'a Row<Cell>>, out: &mut Vec<Cell>);
  /// Appends the encoded form of `cells` to `out`. The format is private but
  /// must round-trip every Cell field (c, fg, bg, flags, extra: zerowidth,
  /// underline colour, hyperlink).
  pub fn encode(cells: &[Cell], out: &mut Vec<u8>);
  pub fn decode(bytes: &[u8], out: &mut Vec<Cell>);
  /// Plain text (spacers skipped, zero-width chars kept), like lines::push_row.
  pub fn text(bytes: &[u8], out: &mut String);
  /// Rows at `columns`, as alacritty would reflow them: WRAPLINE on all but the
  /// last row, a wide char that doesn't fit moves to the next row behind a
  /// LEADING_WIDE_CHAR_SPACER, every row padded to `columns`. An empty line is one row.
  /// Only rows `from..to` (clamped) are produced.
  pub fn wrap(cells: &[Cell], columns: usize, from: usize, to: usize, out: &mut Vec<Row<Cell>>);
  pub fn row_count(cells: &[Cell], columns: usize) -> usize;
  ```
- Produces (`crate::testterm`, test only): `TestTerm { pub term: Term<VoidListener>, parser: Processor }`, `TestTerm::new(cols, lines, history)`, `feed(&mut self, s: &str)` (`\n` → `\r\n`, as in `lines.rs` tests), `rows_of_line(&self, first_abs: usize) -> Vec<&Row<Cell>>`.

- [ ] **Step 1: Write failing tests** in `line.rs`:
  - `round_trips_styled_output`: feed a fixed mix into a 20-column `TestTerm`: SGR bold/italic/underline styles, 256 and RGB colours, inverse, dim, a CJK wide char at column 19 so it wraps, a combining mark, an OSC 8 hyperlink and `ESC[41mX\x1b[K` (coloured erase). For every logical line, `join_rows` → `encode` → `decode` → `wrap(.., 20, 0, usize::MAX, ..)` equals the grid's rows cell for cell (flags included).
  - `rewraps_at_other_widths`: the same line wrapped at 7 and at 33 equals what a `TestTerm` of that width shows after `resize` from 20.
  - `text_matches_push_row`: `text()` equals the grid rows' `lines::push_row` output, trimmed.
  - `row_count_matches_wrap` for widths 1, 2, 80 and an empty line (1 row).
  - `wrap_range_returns_only_requested_rows`: from=3, to=5 on a 10-row line gives exactly rows 3 and 4.
- [ ] **Step 2:** `cargo test history::line` → FAIL.
- [ ] **Step 3: Implement.** For speed: plain cells with no `extra` and the same fg/bg/flags as the previous cell cost only their UTF-8 char. Write style changes as a tagged record before the cell. `extra` gets its own record.
- [ ] **Step 4:** `cargo test history::line` → PASS.
- [ ] **Step 5: Commit** `history: compact line encoding and rewrapping`.

---

### Task 3: History store, RAM limit, disk spill

**Files:**
- Modify: `src/history/mod.rs`
- Create: `src/history/disk.rs`

**Interfaces:**
- Consumes: Task 1 `RowSink`. Task 2 `join_rows`, `encode`, `decode`, `text`.
- Produces (`crate::history`):
  ```rust
  pub type Shared = Arc<Mutex<History>>;
  pub struct Budget { pub bytes: usize }               // from memory_per_tab_mb
  impl Budget {
      pub fn from_mb(mb: usize) -> Self;               // clamps 64..=16384
      pub fn live_rows(&self, columns: usize) -> usize;   // Global Constraints formula
      pub fn window_rows(&self, columns: usize) -> usize; // (bytes/4)/(columns*CELL), ≥ 200
      pub fn history_ram(&self) -> usize;               // bytes/2
  }
  pub struct History { .. }
  impl History {
      pub fn new(ram_limit: usize, dir: PathBuf) -> Self;
      #[cfg(test)] pub fn with_chunk_bytes(self, n: usize) -> Self; // default 1 << 20
      pub fn set_ram_limit(&mut self, bytes: usize);   // spills immediately if over
      pub fn first_line(&self) -> u64;   // oldest line still held
      pub fn end_line(&self) -> u64;     // one past the newest complete line == grid.lines_dropped()
      pub fn partial(&self) -> &[u8];    // encoded cells of the incomplete line (rows dropped so far)
      pub fn generation(&self) -> u64;   // +1 on every clear
      pub fn ram_bytes(&self) -> usize;  // chunks in RAM + open chunk + partial + read cache
      pub fn snapshot(&self) -> Snapshot;
  }
  pub struct Sink(pub Shared);   // impl RowSink<Cell>: dropped → push rows; cleared(n) → clear to n
  #[derive(Clone)] pub struct Snapshot { .. }      // Send + Sync; cheap: Arc'd chunks + open chunk copy
  pub enum Item<'a> { Line(&'a [u8]), Lost(u64) } // Lost(count): one marker for that many lines
  impl Snapshot {
      pub fn first_line(&self) -> u64;
      pub fn end_line(&self) -> u64;
      pub fn generation(&self) -> u64;
      pub fn partial(&self) -> &[u8];
      /// Calls f(line, item) for complete lines in `range` (clamped), in order
      /// (reverse order if `rev`). A Lost range is reported once, at its
      /// first line, if that line is in `range`. Returns Err on read failure.
      pub fn for_each(&self, range: Range<u64>, rev: bool, f: impl FnMut(u64, Item<'_>) -> ControlFlow<()>) -> io::Result<()>;
  }
  // disk.rs
  pub struct DiskFile { .. }   // all methods &self; Send + Sync
  impl DiskFile {
      pub fn create(dir: &Path) -> io::Result<Arc<DiskFile>>;
      pub fn append(&self, bytes: &[u8]) -> io::Result<u64>;      // returns offset
      pub fn read(&self, offset: u64, len: u32) -> io::Result<Arc<[u8]>>; // LRU cache of 4 chunks
  }
  ```
- Chunk layout: `[line bytes…][u32 offsets × n][u32 n]`. Index entries `{ first: u64, count: u32, at: Ram(Arc<[u8]>) | Disk { offset, len } | Lost }`. A line larger than the chunk size gets a chunk of its own.
- On `cleared`, drop all entries and the `Arc<DiskFile>` (the file goes when the last snapshot drops), set `first = end = n`, and bump the generation.
- A failed `create` or `append` turns the oldest RAM entries into `Lost` until the limit holds, logs once with `log::warn!`, and retries the disk on the next spill.

- [ ] **Step 1: Write failing tests** in `mod.rs` (`TestTerm` with `Sink` installed through `grid_mut().set_row_sink`, history 5 rows, `with_chunk_bytes(512)`, RAM limit 4 KB, `dir` in a fresh temp dir per test):
  - `every_line_is_kept_in_order`: feed 20 000 numbered lines (`"line {i}"`, some 3× the width so they wrap). Then `end_line() == grid.lines_dropped()`, and reading `first_line()..end_line()` gives `line 0…` exactly in order, text equal.
  - `ram_stays_under_limit`: after every 1000 lines, `ram_bytes() <= 4096 + 512` (limit plus one open chunk).
  - `spilled_lines_come_back_from_disk`: the same as the first test, plus at least one `Disk` entry exists (expose `#[cfg(test)] fn disk_entries()`).
  - `partial_line_completes_later`: a 4-row wrapped line whose first 2 rows dropped. `partial()` decodes to those cells, `end_line()` hasn't moved, and after the rest drops the line reads back whole.
  - `clear_wipes_history`: feed, then `ESC[3J`. `first_line() == end_line() == grid.lines_dropped()`, the generation is +1, and the snapshot reads nothing.
  - `giant_line_spans_its_own_chunk`: one 2 MB line plus newline, at chunk size 512, reads back equal.
  - `unwritable_dir_loses_oldest_with_marker`: `dir` = a path under a regular file. Feed past the limit: no panic, `Item::Lost(n)` with `n > 0` at `first_line()`, the newest lines read back, and `ram_bytes()` is within the limit.
  - `files_never_outlive_history` (disk.rs): after `DiskFile::create` + append, drop → the dir has no entries (on Unix it is empty even before the drop).
  - `snapshot_reads_from_another_thread`: move a `Snapshot` into `std::thread::spawn` and read every line.
- [ ] **Step 2:** `cargo test history::` → FAIL.
- [ ] **Step 3: Implement** `History`, `Sink`, `Snapshot`, `DiskFile`, `Budget`. Positional I/O: `std::os::unix::fs::FileExt::{read_exact_at, write_all_at}` and `std::os::windows::fs::FileExt::{seek_read, seek_write}` (loop until complete). Unix create: `OpenOptions` with `mode(0o600)` and `create_new`, then `remove_file`. Windows: `custom_flags(0x04000000)`, `attributes(0x100)`, `share_mode(FILE_SHARE_READ|WRITE|DELETE = 7)`. Name the file `{pid}-{counter}.hist`.
- [ ] **Step 4:** `cargo test history::` → PASS.
- [ ] **Step 5: Commit** `history: chunked line store with RAM limit and self-deleting disk file`.

---

### Task 4: Wire history into sessions; `memory_per_tab_mb` setting

**Files:**
- Modify: `src/session.rs` (`Session`, `spawn`, `resize`, `apply_config`, `term_config`, tests ~251)
- Modify: `src/config.rs` (field 119-120, default 264, sanitize 387, tests ~446)
- Modify: `README.md` (settings block ~297, plus one sentence in the scrollback/tabs section ~160)

**Interfaces:**
- Consumes: Task 3 `History`, `Sink`, `Budget`.
- Produces:
  - `Config.memory_per_tab_mb: usize` (default 500; `sanitized` clamps 64..=16384). The `scrollback` field is removed.
  - `Session.history: history::Shared`.
  - `fn history_dir() -> PathBuf` in `history/mod.rs` (cache dir rule from Global Constraints, created lazily by `DiskFile::create`).
  - `term_config(cfg, columns)` sets `scrolling_history = Budget::from_mb(cfg.memory_per_tab_mb).live_rows(columns)`.
  - `Session::resize`, when columns change, calls `set_options` with the new live rows. `apply_config` also calls `history.lock().set_ram_limit(budget.history_ram())`.

- [ ] **Step 1: Failing tests:**
  - config: `old_scrollback_key_still_loads` (TOML `scrollback = 5000` parses, other fields default), and `memory_is_clamped` (10 → 64, 10⁶ → 16384).
  - session: `memory_setting_applies_live` replaces `scrollback_setting_applies_live`. A 64 MB budget at 80 columns gives `min(10_000, 16 MB/(80*24))` history rows. The history RAM limit is 32 MB.
- [ ] **Step 2:** `cargo test config:: session::` → FAIL.
- [ ] **Step 3: Implement.** Update the README settings block: `memory_per_tab_mb = 500  # RAM per tab for its text; older lines go to a temporary file`. Remove the `scrollback` line. Add a sentence saying the scrollback is unlimited, kept on disk beyond the RAM limit, and deleted when the tab closes.
- [ ] **Step 4:** `cargo test` → PASS. `cargo build --release` OK.
- [ ] **Step 5: Measure throughput.** Run `make bench BENCH_ARGS="--reps 5 --terminals fastcnsl"` before (on `main`) and after. If any test is more than 5 % slower, profile `Sink::dropped` (`perf record`) and make encoding cheaper before going on. Record the numbers in the commit message.
- [ ] **Step 6: Commit** `Tabs keep all output: history behind the live grid, memory_per_tab_mb`.

---

### Task 5: Window and scrolling through the whole history

**Files:**
- Create: `src/history/window.rs`, `src/history/scroll.rs`
- Modify: `src/app.rs`: `Tab` (75-100); every `session.term.lock()` site (474, 479, 733-803, 1075-1147, 1210, 1234, 1273-1315, 1440-1474, 1620-1657, 1730-1754, 1813, 1916, 1934); `resize_grid` 905-925; the frame code ~1000-1015; theme change.
- Modify: `src/ui.rs` only if `scroll_geometry`'s inputs need a wider type (it takes `usize`, so keep that).

**Interfaces:**
- Consumes: Task 3 `Snapshot`, `Budget`. Task 2 `wrap`, `row_count`, `decode`. Task 1 `set_content`.
- Produces:
  ```rust
  // window.rs
  #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
  pub struct At { pub line: u64, pub sub: u32 }   // global line, wrapped row in it at current width
  pub struct Window<L: EventListener> {
      pub term: Term<L>,      // read-only; cursor hidden; grid via set_content
      rows: Vec<At>,          // At of each window row, top to bottom
      pub first: At, pub last: At,
      pub reaches_live: bool, // ends where the live grid's rows continue
      pub generation: u64,
  }
  impl<L: EventListener> Window<L> {
      /// Rows around `top` (about half the budget above it), at most `max_rows`:
      /// history lines (Lost → one marker row), then the partial line, then live
      /// rows copied from `live` while there is room. Lines larger than the
      /// window are cut to the rows needed (window may start mid-line).
      /// rows_dropped = 0, lines_dropped = first line of the window.
      pub fn build(snap: &Snapshot, live: &Grid<Cell>, top: At, cols: usize, screen: usize, max_rows: usize, listener: L) -> io::Result<Self>;
      pub fn top(&self) -> At;                         // At of the top screen row
      pub fn row_of(&self, at: At) -> Option<usize>;    // absolute window row
      pub fn at_of(&self, abs_row: usize) -> At;
  }
  /// At of absolute live row `abs` (rows of the partial line continue its sub numbering).
  pub fn live_at(live: &Grid<Cell>, snap_partial_rows: usize, abs: usize) -> At;
  pub fn live_row(live: &Grid<Cell>, snap_partial_rows: usize, at: At) -> Option<usize>;

  // scroll.rs
  pub struct Scroller<L: EventListener> { pub window: Option<Window<L>>, .. }
  impl<L: EventListener + Clone> Scroller<L> {
      pub fn new(listener: L) -> Self;
      pub fn in_window(&self) -> bool;
      pub fn top(&self, live: &Term<L>, hist: &Snapshot) -> At;
      /// Moves the view `rows` rows (positive: towards older lines), across
      /// live grid, window and history; rebuilds/enters/leaves the window as needed.
      pub fn scroll(&mut self, live: &mut Term<L>, hist: &Snapshot, budget: &Budget, rows: i64);
      pub fn page(&mut self, live: &mut Term<L>, hist: &Snapshot, budget: &Budget, up: bool);
      pub fn to_top(&mut self, live: &mut Term<L>, hist: &Snapshot, budget: &Budget);
      pub fn to_bottom(&mut self, live: &mut Term<L>);
      /// Puts global line `line` mid-screen (search and bookmark jumps).
      pub fn show_line(&mut self, live: &mut Term<L>, hist: &Snapshot, budget: &Budget, line: u64);
      /// For ui::scroll_geometry: (history rows, display offset) of the whole
      /// tab, counting each history line as one row.
      pub fn metrics(&self, live: &Term<L>, hist: &Snapshot) -> (usize, usize);
      /// Scrollbar drag: `offset` in the units of `metrics`.
      pub fn set_offset(&mut self, live: &mut Term<L>, hist: &Snapshot, budget: &Budget, offset: usize);
      /// Window gone stale: clear (generation changed) → to_bottom; columns
      /// changed → rebuild at the same top line, sub 0.
      pub fn sync(&mut self, live: &mut Term<L>, hist: &Snapshot, budget: &Budget);
  }
  ```
  Placement rule (every op): compute the target top `At`. If `live_row` finds it and `history_size - row >= 0`, go live with that `display_offset` and drop the window. Otherwise use the current window if it holds the target with a full screen below it, else rebuild. Moving between live/window/rebuilt window carries the selection through `At` (`to_range` → `At` → new points → `Selection::new(ty, ..)` + `update`), clipped to what the target holds.
- In `app.rs`, `Tab` gets `scroller: Scroller<EventProxy>` plus two accessors:
  - `fn shown(&mut self) -> Shown<'_>`: deref/deref_mut to `Term<EventProxy>`; the window's term or the live lock guard.
  - `fn live(&self) -> FairMutexGuard<'_, Term<EventProxy>>`.
  - Rule for the call sites: drawing, mouse→cell, selection, copy selection, on-screen search, gutter numbers and bookmarked rows use `shown()`. Writing to the shell, mode checks (alt screen, mouse reporting), `record_times`, `on_enter`, code `update` and logging use `live()`. Every `scroll_display(..)` becomes a `scroller` call. Any key sent to the shell calls `to_bottom` (as `Scroll::Bottom` does today).
  - When the window is shown, `tab.lines.invalidate()` runs on every switch or rebuild (track the window's identity).

- [ ] **Step 1: Failing tests** in `scroll.rs` (TestTerm with history, `Budget` sized so the live grid holds 50 rows and the window 200 rows, chunk 512, RAM 4 KB; helper `screen_text(&Scroller, &Term) -> Vec<String>` reads the shown term's screen rows):
  - `scrolling_up_walks_every_line_once`: 5000 numbered lines, some wrapping. Scroll up one row at a time from the bottom to the top, collecting each new top row. The joined text equals the output exactly.
  - `page_and_top_and_bottom`: `to_top` shows `line 0` on the first row, and `to_bottom` drops the window and sets `display_offset == 0`.
  - `crossing_into_live_drops_window`: at the window's bottom edge, scrolling down 1 → `!in_window()` and the top row is the next line.
  - `show_line_centres_a_disk_line`: `show_line(10)` puts `line 10` at row `screen/2`.
  - `metrics_cover_history`: `metrics().0 == (end-first) + live history_size` and the offset decreases monotonically while scrolling down.
  - `set_offset_round_trips`: for 10 offsets, `set_offset(o)` then `metrics().1 == o` (±1 for wrapped lines).
  - `resize_in_window_keeps_top_line` (Review Focus 1): in the window at `line 1234`, resize the live term to 37 columns, then `sync` → the top is still `line 1234`, sub 0.
  - `output_while_in_window_keeps_view_and_bottom_is_complete` (Review Focus 2): in the window, feed 3000 more lines and `sync` → the same screen text. Then scroll down to the bottom row by row; the collected text is complete and in order.
  - `window_starts_mid_line` (Review Focus 3): a 200 000-character line archived, `show_line` on it, then scroll 5 rows → each screen shows consecutive slices.
  - `clear_leaves_window` (Review Focus 5): in the window, feed `ESC[3J` and `sync` → `!in_window()`.
  - `selection_survives_rebuild`: select 3 rows in the window and scroll far enough to rebuild while the selection stays in range → `selection_to_string()` is unchanged.
- [ ] **Step 2:** `cargo test history::scroll` → FAIL.
- [ ] **Step 3: Implement** `window.rs` and `scroll.rs`, then convert `app.rs` (call sites listed above) and add `tab.scroller.sync(..)` once per frame and after resize or theme change.
- [ ] **Step 4:** `cargo test` → PASS. Then a manual run: `cargo run --release`, then `seq 1 30000000` with `memory_per_tab_mb = 64`. Check: wheel, Shift+PgUp/Home/End and scrollbar drag reach line 1. No visible stall. The selection copies.
- [ ] **Step 5: Commit** `Scroll through the whole history: window over archived lines`.

---

### Task 6: Line numbers, times and bookmarks across history

**Files:**
- Modify: `src/lines.rs` (`record_times_at` 231-254, `on_enter_at` 261-275, `prune_bookmarks` 316-323, `has_bookmarks`, `bookmarked_rows`, `step_bookmark` 352-363, tests)
- Modify: `src/app.rs` (bookmark key/jump handlers, frame code)

**Interfaces:**
- Consumes: Task 5 `Scroller::show_line`, `Tab::shown`/`live`. Task 3 `first_line()`.
- Produces:
  - `LineIndex::forget_before(&mut self, first_line: u64)`. It drops marks, times and bookmarks below `first_line` (keeping the last mark/time at or before it, as the current `partition_point(..) - 1` logic does). It replaces the `grid.lines_dropped()` pruning in `record_times_at`, `on_enter_at` and `prune_bookmarks`.
  - `LineIndex::prune_past_cursor(&mut self, live: &Grid<Cell>)`: the "lines past the cursor after a reset" half of today's `prune_bookmarks`. Live grid only.
  - `has_bookmarks` and `bookmarked_rows` no longer prune.
  - `step_bookmark(&mut self, shown: &Grid<Cell>, down: bool) -> Option<u64>`: returns the global line. The app calls `scroller.show_line`. `first_row_of` and `scroll_to_show` are removed if unused.
  - The frame code calls `forget_before(history.first_line())` and `prune_past_cursor(live)`.

- [ ] **Step 1: Failing tests** in `lines.rs` (TestTerm with history, tiny budget):
  - `bookmark_survives_moving_to_disk`: bookmark `line 7`, push 10 000 lines, `show_line(7)` → `bookmarked_rows(shown)` contains its row.
  - `jump_reaches_bookmark_on_disk`: `step_bookmark` from the live bottom (no bookmarks on screen, going up) returns 7.
  - `tab_numbers_in_window_are_global`: in the window at `line 4000`, `numbers(shown, Tab)` starts with `Some(4001)`.
  - `times_kept_for_archived_lines`: `Command` mode shows a time for an archived line.
  - `clear_forgets_marks_above`: after `ESC[3J` and `forget_before(first_line())`, `has_bookmarks` is false.
  - Update the existing `numbers_hold_when_history_overflows` and `bookmarks_go_with_the_text_they_mark`: bookmarks now survive overflow (that is the new behaviour).
- [ ] **Step 2:** `cargo test lines::` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5: Commit** `Line numbers, times and bookmarks reach archived lines`.

---

### Task 7: Code colours in the window

**Files:**
- Modify: `src/highlight.rs` (`Block` 69-85, `on_enter` 130-160, `update` 164-229, `clear` 122, tests)
- Modify: `src/view.rs` (`View.code` field and `draw_grid` ~523)
- Modify: `src/history/window.rs` (spans per window row)

**Interfaces:**
- Consumes: Task 5 `Window::build` and `At`. Task 3 `Snapshot::for_each`, `line::text`.
- Produces:
  ```rust
  pub trait CodeRows { fn row(&self, abs_row: i64) -> &[Span]; }
  impl CodeRows for CodeHighlighter { .. }   // today's `row`
  impl CodeHighlighter {
      /// Code blocks overlapping `lines` (global), clipped, oldest first.
      pub fn code_ranges(&self, lines: Range<u64>) -> Vec<(Range<u64>, &'static SyntaxReference)>;
      pub fn theme_name(&self) -> &str;
  }
  /// Spans per line, columns counted in cells from the line's start.
  pub fn highlight_lines(syntax: &'static SyntaxReference, theme_name: &str, lines: &[String]) -> Vec<Vec<(u32, u32, Rgb)>>;
  ```
  - `Block` gains `first_line: u64` (global line of the first output line, from `lines::ends_before` on the live grid in `on_enter`) and `end_line: Option<u64>`. Blocks whose language became `Code` are also pushed, when they end, to `finished: Vec<(Range<u64>, &'static SyntaxReference)>` (cap 4096, oldest dropped). `clear()` empties it too, which keeps today's "reflow drops colours" behaviour.
  - `Window::build` gets `code: Option<(&CodeHighlighter)>`. For each code range in the window it re-highlights from `max(range.start, window_first - 1000)` (reading the context lines from history) and keeps `spans: Vec<Vec<Span>>` per window row. `impl CodeRows for Window`.
  - `View.code: &dyn CodeRows`. The app passes the window when it is shown.

- [ ] **Step 1: Failing tests:**
  - highlight: `code_ranges_follow_global_lines` (`cat x.rs` block, ended by a next command → a range starting at the first output line).
  - window: `archived_code_keeps_colours`: `cat a.rs` with 300 lines of Rust, then 5000 filler lines, then `show_line` on the `fn` line → its window row has non-empty spans at the `fn` keyword's columns, equal to the spans the live highlighter produced for the same text before it scrolled out.
  - `theme_switch_recolours_window`: rebuild with another theme → different span colours.
- [ ] **Step 2:** `cargo test highlight:: history::window` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5: Commit** `Code colours for archived lines`.

---

### Task 8: Search across the whole history

**Files:**
- Create: `src/history/search.rs`
- Modify: `src/search.rs` (`SearchBox::step` 96-116, `count` 132-160, `refresh`; `Search::step`; count display), `src/session.rs` (`UserEvent::History(u64)`), `src/app.rs` (event handler, search key handlers 1273/1620/1934, search bar text)
- Modify: `Cargo.toml`: `regex-automata = "0.4.18"` (the version already locked)

**Interfaces:**
- Consumes: Task 3 `Snapshot`, `line::text`. Task 5 `Scroller::show_line`, `Window::row_of`.
- Produces:
  ```rust
  // history/search.rs
  /// Same pattern rules as alacritty's RegexSearch: case-insensitive unless
  /// the pattern has an upper-case letter.
  pub fn compile(pattern: &str) -> Result<regex_automata::meta::Regex, String>;
  pub enum Found { Line(u64), NotFound, Count { matches: usize, capped: bool, before: Option<usize> } }
  pub struct Searcher { .. }    // one per tab; one worker thread per job, cancelled by AtomicBool
  impl Searcher {
      pub fn new(wake: Box<dyn Fn() + Send + Sync>) -> Self;
      /// Nearest line with a match in `range`, scanning from its end (`up`) or start.
      pub fn step(&mut self, snap: Snapshot, pattern: String, range: Range<u64>, up: bool, box_id: usize);
      /// Matches in all history lines, capped at 9999; `before`: matches in lines < `current`.
      pub fn count(&mut self, snap: Snapshot, pattern: String, current: Option<u64>, box_id: usize);
      pub fn cancel(&mut self);
      pub fn poll(&mut self) -> Vec<(usize, Found)>;
      pub fn busy(&self, box_id: usize) -> bool;
  }
  ```
  `wake` sends `UserEvent::History(tab_id)`, which makes the app call `poll` and redraw.
- `SearchBox::step` flow:
  1. Search the shown term from the origin, as today. A match on the correct side of the origin (not wrapped around) is the answer.
  2. Otherwise look beyond the shown term in that direction:
     - Going up from live or from a window: `step(range = first_line..window_or_live_first_line, up)`.
     - Going down from a window that doesn't reach live: `step(window_end..end_line, down)`. If nothing is found, search the live term from its top.
     - If the whole history has no match in that direction, wrap around as today (bottom of live, or top of history).
  3. On `Found::Line(l)`: `show_line(l)`, then in the shown term `search_next` from the start (down) or end (up) of that line's rows. Set `current`.
- Count: `total = history count + live term count`. The live term count uses today's `RegexIter`, over the live term only. The box shows "…" while `busy` and "9999+" when capped. `index = before + (rank of current within its line, from the shown term)` when it is known and below the cap.
- Typing, Esc, a new step, closing the box or a clear (`generation` change) cancels the box's job.

- [ ] **Step 1: Failing tests** in `history/search.rs` and `search.rs` (tiny budget, 20 000 lines, `needle` on lines 5, 9000 and live line 19 990):
  - `compile_matches_alacritty_case_rule`: `"err"` matches `ERR`, while `"Err"` does not match `err`.
  - `step_up_finds_disk_line` → `Found::Line(9000)`, then from 9000 → `Line(5)`.
  - `count_spans_history` → 2 history matches plus 1 live, total 3. The index of the current match at line 9000 is 2.
  - `count_caps_at_9999`: 20 000 matches → capped.
  - `cancelled_job_reports_nothing`.
  - `clear_cancels_history_search` (Review Focus 5): start a count, clear, `sync` → `poll` returns nothing for the old generation and the box count resets.
  - `search.rs` end-to-end: Shift+Enter from the bottom walks 19 990 → 9000 → 5 → wraps to 19 990. Each time the current match is on screen in the shown term.
- [ ] **Step 2:** `cargo test history::search search::` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `cargo test` → PASS. Manually, with a 2 GB history: search a rare word, press Enter repeatedly. The window never freezes, and "…" shows while counting.
- [ ] **Step 5: Commit** `Search through the whole history`.

---

### Task 9: Copy, save and logging over the whole history

**Files:**
- Modify: `src/logging.rs` (`TabLog::write_new` 71-84, `write_lines`, `all_text` 136-143, `save_text` 151-157, tests)
- Modify: `src/app.rs` (`copy_tab_text` 732, `save_tab_text` 744, selection copy 1127, log pass per frame), `src/session.rs` (`UserEvent::Notice(String)`)

**Interfaces:**
- Consumes: Task 3 `Snapshot`, `line::text`, `History::partial`.
- Produces:
  ```rust
  pub const COPY_LINES: usize = 100_000;
  /// The last `max` lines of the tab: history lines then the live grid's
  /// (the partial line's history part joined to its live rows).
  pub fn tail_text(snap: &Snapshot, live: &Grid<Cell>, max: usize) -> (String, usize /*lines*/, bool /*capped*/);
  /// Keeps the last `max` lines of `text`.
  pub fn keep_last_lines(text: String, max: usize) -> (String, bool);
  /// Writes all history lines then the live text to a new file, on its own
  /// thread; `done` gets the result.
  pub fn save_text_in_background(path: PathBuf, snap: Snapshot, live_text: String, done: impl FnOnce(io::Result<()>) + Send + 'static);
  impl TabLog {
      /// As today; lines below the live grid come from `snap`, at most
      /// 50_000 per call. Returns true while still behind.
      pub fn write_new(&mut self, snap: &Snapshot, grid: &Grid<Cell>, upto: Upto) -> io::Result<bool>;
  }
  ```
  - The "scrolled out of the scrollback" notice is written only when `next < snap.first_line()` (after a clear).
  - Copy notice: `Copied the last 100000 lines of {label}` when capped, otherwise today's text.
  - Lost ranges are written as their marker line.

- [ ] **Step 1: Failing tests** in `logging.rs` (tiny budget):
  - `tail_text_takes_last_100k_from_history_and_live`: 150 000 numbered lines → 100 000 lines, the first is `line 50000`, the last is the final line, capped.
  - `tail_text_joins_partial_line`: a wrapped line straddling the history/live seam comes out whole.
  - `selection_copy_is_capped`: `keep_last_lines` on 100 001 lines.
  - `save_writes_every_line`: 20 000 lines spilled to disk → the file equals all lines in order.
  - `log_started_late_catches_up_without_gaps`: start a log after 120 000 lines. Call `write_new` until it returns false; the file has every line once, with no "scrolled out" notice. Update the existing `logging_keeps_up_while_the_scrollback_overflows`: the gap notice is gone.
- [ ] **Step 2:** `cargo test logging::` → FAIL.
- [ ] **Step 3: Implement.** In the app, call `write_new` each frame while it returns true, as well as on output.
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5: Commit** `Copy, save and logging cover the whole history`.

---

### Task 10: Verify budget and throughput, then document

**Files:**
- Modify: `docs/BENCHMARK.md`, `README.md`, `CHANGELOG.md` (through the `update-changelog` skill)
- Test: `src/history/mod.rs` (`#[ignore]` long test)

- [ ] **Step 1: Write the memory test** `history_ram_never_exceeds_budget_with_gigabytes` (`#[ignore]`, run explicitly). Use a 64 MB budget and a 160-column TestTerm, and feed 3 GB of mixed ascii/colour/CJK lines. Every 100 000 lines, assert `history.ram_bytes() <= budget.history_ram()`, live rows × columns × 24 ≤ budget/4, and that the window built at a random line has rows × columns × 24 ≤ budget/4.
- [ ] **Step 2:** `cargo test --release -- --ignored history_ram_never_exceeds` → PASS.
- [ ] **Step 3: Benchmark** with `make bench` (Linux), plus `scripts/bench/windows.sh` if a Windows machine is available. Compare with the tables in `docs/BENCHMARK.md`. All tests must be within 5 %. Update the tables and the date, and add a note on peak RSS with unlimited history.
- [ ] **Step 4: Manual checklist** (release build, `memory_per_tab_mb = 64`):
  - `seq 1 100000000` and watch RSS (`ps -o rss`), which must stay under ~64 MB plus the app's baseline.
  - Scroll to line 1, search `^1234567$`, bookmark it, jump away and back.
  - Resize while scrolled back.
  - Run `clear`; the history file is gone (`ls ~/.cache/fastcnsl/history`). Close the tab; nothing is left.
  - `kill -9` fastcnsl and confirm nothing is left.
- [ ] **Step 5:** Run the `update-changelog` skill. Commit `Benchmark and docs for unlimited scrollback`.
