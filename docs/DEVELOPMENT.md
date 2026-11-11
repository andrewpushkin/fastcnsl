# Developing fastcnsl

Notes for building fastcnsl from source and finding your way around the
code. For using it, see the [README](../README.md).

## Building

Everything builds in Docker, so the only requirements are Docker and `make`.
Each platform has its own image in `docker/`. All generated files are normal
files in the project, owned by your user: packages in `dist/`, build output
in `target/`, downloaded crates in `.cache/`. `make clean` is a plain
`rm -rf target dist` and needs no Docker.

```sh
make all                     # every release package below

make linux-x86_64            # compile only
make linux-x86_64-deb        # .deb
make linux-x86_64-rpm        # .rpm
make linux-x86_64-appimage   # AppImage
make windows-x86_64          # compile only
make windows-x86_64-zip      # zip
make windows-arm64           # compile only
make windows-arm64-zip       # zip
make macos-arm64             # compile only (the image downloads the macOS SDK, see below)
make macos-arm64-dmg         # Fastcnsl.app in a disk image

make build            # quick build for this machine -> dist/fastcnsl
make run              # build and start it
make tests            # unit tests (not run by GitHub Actions)
make clippy           # lints
make check-all        # type-check every target platform
make screenshot       # headless UI run -> dist/screenshots/*.png
make clean
```

The README's screenshots in `docs/` come from `make screenshot`.

GitHub Actions (`.github/workflows/build.yml`) lints and builds every
package on each pull request and push to `main`, using the same Makefile
targets. It doesn't run the unit tests; run `make tests` before pushing.
macOS is built on GitHub's Apple Silicon runner with Apple's own SDK.

Every push to `main` becomes a release. The version is
`<major>.<minor>.<commits on main>` (`scripts/get_version.sh`): `0.1` comes from
`Cargo.toml`, so bump it there for a new minor version. Local builds use
the same numbering, and `make VERSION=1.2.3 ...` overrides it.

The macOS image downloads Apple's macOS SDK from a third-party mirror while
it is built. Apple's SDK license only allows use on Apple hardware and
forbids redistribution, so check that you may use it, and don't publish that
image. `docker/macos-arm64.Dockerfile` explains how to point it at an SDK you
packaged yourself. You can also build natively on a Mac with
`scripts/package/macos-arm64.sh`.

fastcnsl's own code has no `unsafe` (`#![forbid(unsafe_code)]`).

## How it stays fast

```
PTY ──read──▶ I/O thread ──parse in place──▶ grid (shared, locked)
                                                │ borrowed &Cell
                         UI thread ──▶ instance buffer ──▶ GPU, 1 draw call
```

- Rendering uses [wgpu] (Vulkan, Metal, DirectX 12, OpenGL): one draw call
  per frame, glyphs cached in a texture atlas.
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

On Windows, WSL tabs go through `fastcnsl-wsl`, which runs the shell on a
Linux pty and passes its output to fastcnsl through a plain pipe instead
of the Windows console host. Other tabs use the console host from Windows
Terminal (`conpty.dll`, `OpenConsole.exe`), which is much faster with large
output than the one built into Windows.

## Remote control

`fastcnsl new-tab/close-tab/quit` talk to a running window: each window
listens on a random port on 127.0.0.1 and writes the port and a random
secret to `instances/<pid>` in the local data directory
(`%LOCALAPPDATA%\fastcnsl` on Windows, `~/.local/share/fastcnsl` on
Linux, `~/Library/Application Support/fastcnsl` on macOS), readable only
by the user. A command must present that secret. The file is removed when
the window closes.

## Project layout

| Path                | What it does                                      |
|---------------------|---------------------------------------------------|
| `src/app.rs`        | window, tabs, input, mouse, clipboard, actions    |
| `src/chrome.rs`     | tab bar, menus, Options and About panels          |
| `src/shortcuts.rs`  | shortcut table shown in Options and menus         |
| `src/session.rs`    | shell process, PTY and I/O thread                 |
| `src/view.rs`       | terminal state → quads                            |
| `src/render/`       | wgpu pipeline, glyph atlas, WGSL shader           |
| `vendor/`           | alacritty_terminal with two patches (see its README) |
| `src/search.rs`     | find, match navigation and counting               |
| `src/highlight.rs`  | code blocks in the grid, syntect highlighting     |
| `src/detect.rs`     | command-line and content language detection       |
| `src/lines.rs`      | logical lines and their numbers                   |
| `src/logging.rs`    | copying, saving and logging a tab's text          |
| `src/input.rs`      | keyboard, mouse and paste encoding                |
| `src/config.rs`     | settings file and hot reload                      |
| `src/remote.rs`     | `fastcnsl new-tab/close-tab/quit` control       |
| `src/theme.rs`      | dark and light palettes, OS theme detection       |
| `wsl-relay/`        | `fastcnsl-wsl`, the WSL tab relay on Windows    |
| `docker/`, `scripts/package/`, `packaging/` | release builds    |

[wgpu]: https://wgpu.rs
[alacritty_terminal]: https://crates.io/crates/alacritty_terminal
