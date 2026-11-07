# Vendored crates

## alacritty_terminal 0.26.0

The crates.io release (Apache-2.0, see `alacritty_terminal/LICENSE-APACHE`),
without its test recordings, plus two changes (marked "ragamuffin patch").

### Dropped-line counters (`src/grid/mod.rs`, `src/grid/resize.rs`)

`Grid::rows_dropped()` and `Grid::lines_dropped()` count the rows that left
the top of the grid (scrollback full, history cleared or shortened, reflow)
and how many lines ended in them. Line numbers (`src/lines.rs`) use them
to keep counting from the start of the tab after old lines are gone.

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
