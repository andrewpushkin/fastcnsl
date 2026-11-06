# ragamuffin

A fast, GPU-accelerated terminal written in Rust, for Linux, Windows and macOS.

![ragamuffin showing cat hello.c with C highlighting and a search for "greet"](docs/screenshot.png)

- **GPU rendering** with [wgpu] (Vulkan, Metal, DirectX 12, OpenGL): one draw
  call per frame, glyphs cached in a texture atlas, redraws only when something
  changed.
- **Search highlighting like Notepad++ or Konsole**: every match on screen is
  marked, the current one stands out, and `3/17` shows where you are in the
  whole scrollback.
- **Code highlighting**: `cat hello.c`, `head main.rs`, `bat`, `type`
  (Windows), `Get-Content` (PowerShell) and `git diff` output is detected and
  coloured. The language comes from the file name, or from the content (`#!`
  lines, `#include`, `fn main`...) when there is no extension. Text a program
  colours itself is never touched.
- **Light and dark themes** that follow the operating system, or a fixed choice.
- **Settings file**: font, size, theme, shell, opacity and more. Saved changes
  apply immediately.
- **Quiet UI**: the scrollbar and search bar sit at 40 % opacity and become
  fully visible while you use them.
- **No `unsafe`** in ragamuffin's own code (`#![forbid(unsafe_code)]`).

## Install

Download a package for your platform from the releases page:

| Platform                | File                                    |
|-------------------------|-----------------------------------------|
| Linux x86-64 (Debian, Ubuntu) | `ragamuffin_<ver>_amd64.deb`      |
| Linux x86-64 (any distro)     | `Ragamuffin-<ver>-x86_64.AppImage`|
| Windows x86-64          | `ragamuffin-<ver>-windows-x86_64.zip`   |
| Windows ARM64           | `ragamuffin-<ver>-windows-arm64.zip`    |
| macOS Apple Silicon     | `ragamuffin-<ver>-macos-arm64.zip`      |

- `.deb`: `sudo apt install ./ragamuffin_<ver>_amd64.deb`
- AppImage: `chmod +x Ragamuffin-*.AppImage && ./Ragamuffin-*.AppImage`
- Windows: unzip and run `ragamuffin.exe` (Windows 10 1809 or newer).
- macOS: unzip and move `Ragamuffin.app` to Applications. The app is ad-hoc
  signed, not notarized, so the first launch needs right-click → Open.

Linux builds target glibc 2.31, so they run on Ubuntu 20.04, Debian 11,
Fedora 32 and anything newer.

## Shortcuts

On macOS, use Cmd where the table says Ctrl+Shift.

| Keys                        | Action                                   |
|-----------------------------|------------------------------------------|
| Ctrl+Shift+F                | open or focus search                     |
| Enter / Shift+Enter, F3     | previous (older) / next match            |
| ↑ / ↓ in search             | previous / next match                    |
| Ctrl+R in search            | toggle regular expressions               |
| Esc in search               | close search                             |
| Ctrl+Shift+C / Ctrl+Shift+V | copy / paste                             |
| Ctrl+= / Ctrl+- / Ctrl+0    | larger / smaller / default font          |
| Ctrl+Shift+L                | cycle theme: system → dark → light       |
| Ctrl+Shift+,                | open the settings file                   |
| Shift+PgUp / PgDn / Home / End | scroll the scrollback                 |

Mouse: drag to select, double-click selects a word, triple-click a line, and
middle-click pastes. Drag the scrollbar, or click the track to page. Hold
Shift to select text in programs that capture the mouse (vim, htop).

Search ignores case unless the query contains a capital letter.

## Settings

The settings file is created on first start:

- Linux: `~/.config/ragamuffin/config.toml`
- macOS: `~/Library/Application Support/ragamuffin/config.toml`
- Windows: `%APPDATA%\ragamuffin\config.toml`

```toml
font_family = "JetBrains Mono"   # any installed font
font_size = 11.0                 # points
line_height = 1.2
theme = "system"                 # "system" | "dark" | "light"
shell = ""                       # empty: $SHELL, or PowerShell/cmd on Windows
shell_args = []
scrollback = 10000
ui_idle_opacity = 0.4            # scrollbar/search bar opacity when idle
padding = 6.0
code_highlight = true
code_theme_dark = "base16-ocean.dark"
code_theme_light = "InspiredGitHub"
bold_is_bright = false
```

Code themes: `base16-ocean.dark`, `base16-eighties.dark`, `base16-mocha.dark`,
`base16-ocean.light`, `InspiredGitHub`, `Solarized (dark)`, `Solarized (light)`.

## Building

Everything builds in Docker, so the only requirements are Docker and `make`.
Each platform has its own image in `docker/`, and output goes to `dist/`,
owned by your user.

```sh
make all              # every release package
make linux-x86_64     # .deb + AppImage
make windows-x86_64   # zip
make windows-arm64    # zip
make macos-arm64      # zip, needs a macOS SDK (see docker/macos-sdk/README.md)

make build            # quick build for this machine -> dist/ragamuffin
make run              # build and start it
make test             # unit tests
make clippy           # lints
make check-all        # type-check every target platform
make screenshot       # headless smoke test -> dist/screenshot.png
make clean
```

Apple doesn't allow redistributing the macOS SDK, so `make all` skips macOS
until you add one. You can also build natively on a Mac with
`cargo build --release`.

## How it stays fast

```
PTY ──read──▶ I/O thread ──parse in place──▶ grid (shared, locked)
                                                │ borrowed &Cell
                         UI thread ──▶ instance buffer ──▶ GPU, 1 draw call
```

- The terminal core ([alacritty_terminal]) parses PTY bytes straight from its
  read buffer into the grid. The renderer borrows cells under a short lock
  and never copies them out.
- The lock is released before presenting, so waiting for vsync never blocks
  the shell's output.
- Each frame is four reusable instance arrays (backgrounds, text, UI shapes,
  UI text). They keep their capacity between frames and are uploaded straight
  from their slices, so a steady-state frame allocates nothing.
- Glyphs are shaped and rasterised once, then looked up in a table. ASCII
  skips hashing entirely.
- Fixed key sequences (Enter, arrows, Ctrl+C...) go to the shell as
  `&'static [u8]` without allocating.
- Code highlighting runs once per line as output arrives, and its colours are
  cached per row.
- Nothing is drawn unless the terminal changed, the window changed, or an
  overlay is fading.

## Project layout

| Path                | What it does                                      |
|---------------------|---------------------------------------------------|
| `src/app.rs`        | window, input, shortcuts, mouse, clipboard        |
| `src/session.rs`    | shell process, PTY and I/O thread                 |
| `src/view.rs`       | terminal state → quads                            |
| `src/render/`       | wgpu pipeline, glyph atlas, WGSL shader           |
| `src/search.rs`     | find, match navigation and counting               |
| `src/highlight.rs`  | code blocks in the grid, syntect highlighting     |
| `src/detect.rs`     | command-line and content language detection       |
| `src/input.rs`      | keyboard, mouse and paste encoding                |
| `src/config.rs`     | settings file and hot reload                      |
| `src/theme.rs`      | dark and light palettes                           |
| `docker/`, `scripts/package/`, `packaging/` | release builds    |

## License

Dual-licensed under MIT or Apache-2.0, at your option.

[wgpu]: https://wgpu.rs
[alacritty_terminal]: https://crates.io/crates/alacritty_terminal
