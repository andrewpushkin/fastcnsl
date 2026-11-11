# Unlimited scrollback: 500 MB per tab in RAM, the rest on disk

## Why

fastcnsl is used for huge logs: builds and services that print for hours.
Today each tab keeps `scrollback = 10000` lines (at most 1 000 000), and
older lines are lost. The goal is a tab that takes in output of any size,
never loses a line, and never uses more than 500 MB of RAM for its text.

The user must not notice where a line is kept. Every feature that works on
the scrollback today keeps working, unchanged, across the whole history.

## Decided by the user

- **RAM limit: 500 MB per tab.** Once a tab's text reaches that, older
  lines move to a file on disk. (The user first said "about 10 000 000
  lines in RAM", then chose a fixed memory budget once it was clear that
  10 M full grid rows would take ~29 GB.)
- **Unlimited history.** The history only grows while the tab is open.
  No line is ever dropped while there is disk space.
- **Lifetime: the tab.** The disk file belongs to one tab. It is deleted
  when the tab closes or fastcnsl quits. History is not restored after a
  restart.
- **All features, invisibly, across the whole history:**
  - scrolling (wheel, Shift+PgUp/PgDn/Home/End, scrollbar drag and click)
  - search: every match marked on screen, several boxes, regex mode,
    Enter/Shift+Enter/F3 jumping through the whole history, match count
  - code colouring of `cat`ted source
  - line numbers ("tab" mode) and line times ("command" mode)
  - bookmarks and jumping between them
  - mouse selection and copy
  - "Save text" and continuous logging
- **"Copy all text" copies the last 100 000 lines**, not the whole history.

## Goals

1. Per-tab RAM for text stays at or under the budget (default 500 MB),
   however much output arrives.
2. Lines beyond the budget go to disk and stay reachable by every feature.
3. Each feature behaves the same for a line whether it sits in the live
   grid, in compact RAM storage or on disk.
4. No visible slowdown where people spend their time: output throughput,
   and scrolling near the bottom.

## Non-goals

- Keeping history across restarts, or reopening old tabs.
- Encrypting the disk file.
- Sharing history between tabs, or one budget shared by all tabs.
- Any change to full-screen programs (alternate screen: vim, less, htop).
  They don't use the scrollback today and won't after this change.

## Anti-goals (a failure even if it works)

- **Slower output.** The 36 MB `cat` benchmark (`docs/BENCHMARK.md`,
  `scripts/bench/`) gets more than ~5 % slower on Linux or Windows.
- **A frozen window.** Any UI-thread pause over ~100 ms caused by reading
  history: a scroll, a jump to a match or bookmark, a search keystroke,
  a resize.
- **RAM above the budget**, e.g. through an index that grows with every
  line, or caches that are not counted.
- **Files left behind** after a tab closes, fastcnsl quits or crashes.
- **Lost or doubled lines.** Line numbers, bookmarks or logs that drift
  as lines move between tiers.

## What exists today (constraints from the code)

- Text lives in the vendored `alacritty_terminal` grid
  (`vendor/alacritty_terminal`). A `Cell` is 24 bytes, and every row is
  allocated at full window width, so a 120-column row costs ~2.9 KB
  whatever it holds. The grid can't hold the history itself: it has to stay
  a bounded live window.
- Roughly 150 places in `src/` read the grid directly through
  `history_size()`, `display_offset()`, `scroll_display()`, `Grid` indexing,
  `Term::search_next` / `RegexIter`, and `renderable_content()`. The main
  readers:
  - `view.rs`: drawing rows, search marks, code colours, gutter, scrollbar
  - `search.rs`: on-screen matches, stepping, counting (capped at 9999)
  - `highlight.rs`: code blocks addressed by absolute row, colour spans
    cached per row; theme switches re-highlight everything
  - `lines.rs`: global line numbers, command marks, line times, bookmarks
  - `logging.rs`: `all_text`, `save_text`, continuous logging
  - `app.rs`: mouse, scrolling, scrollbar, selection, copy
- A global line index already exists. The vendored grid counts
  `rows_dropped()` and `lines_dropped()` (a fastcnsl patch, see
  `vendor/README.md`), and line numbers, bookmarks and logging are keyed
  by it. The archive should be addressed with the same index.
- Patching the vendored crate is an accepted practice. Each patch is
  marked "fastcnsl patch" and described in `vendor/README.md`.
- Today a bookmark is pruned when its line leaves the scrollback, and a log
  writes `[fastcnsl: N lines scrolled out of the scrollback …]` when it
  falls behind. With unlimited history, neither should happen any more,
  except after a clear.

## Required behaviour

**Storage tiers.** Logically, a tab's history is one sequence of lines by
global index. In practice it has three parts:
1. the live grid: the screen plus a bounded amount of recent history, as
   today
2. older lines in compact form in RAM: text plus colour and attribute runs,
   not 24-byte cells
3. the oldest lines on disk

The budget covers everything per tab that scales with history: grid rows,
the compact RAM tier, any index of the disk file, read caches and
per-row highlight caches. An index with one entry per line doesn't fit
(10⁹ lines × 8 B = 8 GB). Index chunks, not lines.

**Exactness.** A line read back from the compact tier or from disk renders
exactly as it did in the grid: characters (wide characters and combining
marks included), colours (named, indexed, RGB), bold, dim, italic,
underline styles, inverse, strikeout and hyperlinks if the grid keeps them.

**Reflow.** When the window width changes, archived lines rewrap at the
new width, just as grid rows do today. Store logical lines (wrapped rows
joined), not rows, and wrap them when they are read. Line numbers,
bookmarks and the scroll position stay on the same lines across a resize.

**Scrolling.** The scrollbar covers the whole history, and dragging it
reaches any point. The thumb has a minimum size, as it does today. Paging
or wheeling into disk-backed history shows text within ~100 ms. Prefetching
ahead of the scroll direction is the builder's call.

**Search.**
- On-screen marks work on any rows shown, from any tier.
- Enter/F3 stepping walks the whole history in both directions.
- Count keeps today's cap (9999, shown as "9999+") and spans the whole
  history.
- Literal and regex modes behave the same on every tier, and a match can
  span wrapped rows.
- A scan through disk history must not block the UI thread (see
  anti-goals). It runs off the UI thread, can be cancelled by typing,
  Esc or another step, and lands on the match when it finds it.

**Code colouring.** Archived code lines keep their colours and follow theme
switches, the same as live rows. They can be re-highlighted when read, or
stored as syntax scopes rather than final colours. The builder chooses.

**Line numbers, times, bookmarks.** Keep their current meaning by global
line index. Bookmarks on archived lines survive. Jumping to a bookmark on
disk works like any other jump.

**Selection and copy.** Mouse selection can extend into archived history,
including dragging past the top of the screen to auto-scroll.

**Copy all text** copies the last 100 000 lines (user's decision).

**Save text** writes the whole history, streamed from disk, and not held
in RAM all at once.

**Continuous logging** never loses lines, so the "scrolled out" notice
goes away except after a clear.

**Clear.** "Clear scrollback" (`ESC[3J`, `clear`, a terminal reset) wipes
the archive too, RAM and disk. The disk file is truncated or deleted. This
matches what those actions do to the grid today.

**Close/quit.** Closing a tab deletes its file. Quitting deletes all of
them.

## Calls made by Claude (not discussed with the user)

These can be overridden. Each was settled because a sensible builder needs
an answer.

1. **Setting.** A new `memory_per_tab_mb = 500` replaces `scrollback`.
   An old `scrollback` key in a config file is accepted and ignored, so
   existing configs still load. The README settings section is updated.
   Clamp: 64 to 16384 MB. Like other settings it applies live: lowering
   it moves lines to disk until the tab fits, and raising it lets more
   recent lines stay in RAM from then on.
2. **Where the files go.** In the user cache directory
   (`dirs::cache_dir()/fastcnsl/history/`: `~/.cache` on Linux,
   `%LOCALAPPDATA%` on Windows, `~/Library/Caches` on macOS), not the
   system temp dir, because `/tmp` is often in RAM (tmpfs), which would
   defeat the purpose. One file per tab, readable by the owner only.
3. **Crash leftovers.** At startup, fastcnsl deletes history files whose
   owning process is no longer running. A file name that includes the PID
   is enough. Another running fastcnsl's files are left alone.
4. **Selection copy uses the same 100 000-line cap** as "Copy all text".
5. **Disk full or write error.** Output keeps flowing, and nothing freezes
   or crashes. The tab keeps the newest lines within its RAM budget and drops
   the oldest RAM-tier lines. A single line saying
   `[fastcnsl: N lines lost: disk full]` is kept where they were. Disk
   writes resume when space returns. This is the only way lines are lost
   apart from clear.
6. **The budget includes text storage only.** The shell, glyph atlas, GPU
   buffers and window are outside it.
7. **Long-running searches show they are searching** (for example the
   count shows "…" until the scan finishes). This is the only visible
   difference from today. It appears only when a scan takes long enough to
   notice.
8. **Grid size.** How much history stays in the live grid, and how the
   budget is split between the grid and the compact tier, is the
   builder's call. It is probably a fixed share of the budget divided by
   the row cost at the current width, so a narrow window keeps more rows
   live.

## Left to the builder

- The archive's data structures, chunk size, encoding and whether to
  compress (lz4/zstd are fine, given the benchmark).
- Whether lines leave the grid through a hook in the vendored grid (patch,
  documented in `vendor/README.md`) or are copied out from `src/`.
- The read interface the features share. One "history" view, addressed by
  global line index and wrapped to the current width, is the expected shape.
  It means each of `view.rs`, `search.rs`, `highlight.rs`, `lines.rs`,
  `logging.rs` and `app.rs` changes once, not in ~150 places.
- Threading: disk I/O and long scans off the UI thread. Writing to disk
  must not slow the PTY reader.
- Search engine for archived text. Plain-text `regex` over decoded lines
  is fine if literal/regex/smart-case semantics match `RegexSearch`.

## Testing

- Unit tests with a tiny budget (kilobytes) so every test crosses all
  three tiers: render, reflow, search step and count, code colours across
  a theme switch, line numbers, times, bookmarks, selection, copy cap,
  save, logging, clear, close-deletes-file, crash-leftover cleanup,
  disk-full behaviour.
- Exactness test: random styled output round-trips through compact and
  disk tiers identically to the grid.
- Memory test: feed far more than the budget (e.g. a few GB at a 64 MB
  budget) and check the tracked size never exceeds it.
- Benchmark: `scripts/bench` on Linux and Windows before and after, within
  ~5 %. Record results in `docs/BENCHMARK.md` as the repo does today.
- Manual: `yes | head -n 200000000`-scale output, then scroll to the top,
  search, jump, resize, and quit. Check the history file is gone.

## Docs

- README: settings section (`memory_per_tab_mb`), and a sentence that
  history is unlimited and spills to disk, with where the files live.
- `vendor/README.md` for any new vendored patch.
- CHANGELOG via the repo's `update-changelog` skill when this ships.
