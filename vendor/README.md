# Vendored crates

## vte 0.15.0

The crates.io release (Apache-2.0 OR MIT, see `vte/LICENSE-APACHE` and
`vte/LICENSE-MIT`), without its changelog, examples and test data, plus
two changes (marked "fastcnsl patch"). The first goes together with
`Term::input_ascii` in alacritty_terminal below; its tests are in
`src/vendor_vte_tests.rs`.

### Less unsafe code (`src/lib.rs`)

`osc_dispatch` fills an initialised array instead of a `MaybeUninit` one,
and `advance_partial_utf8` decodes a character split between two reads
with checked calls instead of `from_utf8_unchecked` and
`unwrap_unchecked`. Both run rarely (once per OSC sequence, once per split
character, on at most 4 bytes), so the speed is unchanged. The one
`from_utf8_unchecked` left, on text that `validate_utf8` has just
checked, stays: a checked call would validate every read twice.

### Runs of printable ASCII and SIMD UTF-8 validation (`src/lib.rs`, `src/ansi.rs`)

Plain text between escape sequences used to reach the terminal one
character at a time: `Perform::print`, then `Handler::input`, which works
out the character's width, the wrap, insert mode, the charset and wide
characters it overwrites, all for one cell. That was 25–40 % of the
CPU time of parsing output. Now `ground_dispatch` finds runs of printable
ASCII (U+0020 to U+007E), 16 bytes at a time, and passes each to the new
`Perform::print_ascii` and `Handler::input_ascii`. Both default to calling
`print` and `input` for each character, so other implementations behave
as before.

Text runs of 64 bytes or more are checked as UTF-8 by `simdutf8` (SSE4.2
or AVX2 on x86-64, chosen at run time; NEON on ARM64) instead of the
standard library, which checks non-ASCII text a byte at a time: 32 MiB of
Cyrillic, Greek and CJK took 18.7 ms, now 0.7 ms. Shorter runs, such as
single words between colour escapes, stay with the standard library,
which is faster on them.

Parsing 32 MiB (`scripts/bench/gen.py`, 100×30 grid, no window): ASCII
0.216 → 0.066 s, `seq` lines 0.282 → 0.215 s, Unicode 0.207 → 0.167 s,
colour 0.168 → 0.158 s.

## alacritty_terminal 0.26.0

The crates.io release (Apache-2.0, see `alacritty_terminal/LICENSE-APACHE`),
without its test recordings, plus six changes (marked "fastcnsl patch").

### Dropped-line counters (`src/grid/mod.rs`, `src/grid/resize.rs`)

`Grid::rows_dropped()` and `Grid::lines_dropped()` count the rows that left
the top of the grid (scrollback full, history cleared or shortened, reflow)
and how many lines ended in them. Line numbers (`src/lines.rs`) use them
to keep counting from the start of the tab after old lines are gone.

### Row sink (`src/grid/mod.rs`, `src/grid/resize.rs`)

`Grid::set_row_sink()` installs a `RowSink` that gets every row leaving
the top of the grid (scrollback full, history shortened, reflow) before it
is reused, and is told when the history is cleared. fastcnsl keeps those
rows as the tab's history (`src/history/`), so the grid itself stays small.
The sink may swap the row for one of its own, so the terminal's thread only
swaps a pointer and the history thread does the rest; with a sink, the
prefetch below asks for the sink's rows (`prefetch_row()`) instead of the
grid's. `Row::occupied()` tells how many cells of a row were written. `Grid::top_continues()` tells whether the top row continues a line that
already went to the sink.
`Term::flush_row_sink()` makes the sink pass on rows it holds back.
`Grid::set_content()` fills a grid with given rows: fastcnsl shows archived
history through a read-only terminal built that way. The tests are in
`src/history/vendor_tests.rs`, since a patched dependency's own tests don't
run.

### Windows output stall (`src/tty/windows/blocking.rs`)


On Windows the PTY output reaches the event loop through an in-memory pipe
whose reader only posts a "readable" event when a read finds it empty. The
event loop stops a read pass after 64 KiB to release the terminal lock, so
with large output (`cat` of a big file) it could stop with data left in the
pipe and no event pending: it slept, the pipe filled, and output stopped
until a resize or a key press woke it. Measured on Windows with software
rendering: a 36 MB `cat` took 74–290 s or never finished; with the fix see
the commit message. The fix posts the event again after a read that leaves
data behind.

Drop this fix when alacritty_terminal ships one.

### Public Windows pipe modules (`src/tty/windows/mod.rs`)

`tty::windows::blocking` (the threaded pipe reader/writer) and
`tty::windows::child` (the process exit watcher) are public, so that
fastcnsl's own PTY for WSL tabs (`src/wsl.rs`), which reads a helper's
output through a plain pipe instead of ConPTY, can use them with the
event loop.

### Prefetching reused rows (`src/grid/mod.rs`)

Once the scrollback is full, every line scrolled up reuses the oldest row,
last touched a whole scrollback ago: with 10 000 lines of 120 columns that
is 29 MB back, long out of the CPU cache, and the rows are separate
allocations the hardware prefetcher can't follow. `scroll_up` now asks
for the row it will reuse four lines later (x86-64 only). Parsing a 36 MB
`cat` (vendored or crates.io code alike): 0.33 s before, 0.29 s after; with
3 000 lines of scrollback, which fit in the cache, it takes 0.22 s.

### Printable ASCII a row at a time (`src/term/mod.rs`)

`Term::input_ascii` implements vte's new `Handler::input_ascii` (see vte
above): it writes as much of a run as fits on the cursor's row in one pass
over the row's cells, then wraps. Insert mode, no line wrap, the DEC line
drawing charset, a hyperlink or underline colour in the cursor's style,
and cells that are part of a wide character go through `input` instead,
one character at a time. `src/vendor_vte_tests.rs` (in fastcnsl) checks
it against `input` on random output.
