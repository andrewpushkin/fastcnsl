# Changelog

Notable changes to fastcnsl, newest first. Versions are
`0.1.<number of commits>`, so they are not consecutive here: only the tagged
releases have a section.

## Unreleased

- **A tab's scrollback has no limit any more.** It keeps everything printed
  while the tab is open. Up to 500 MB of a tab's text stays in memory
  (`memory_per_tab_mb`, which replaces `scrollback`); older lines go to a
  temporary file in the cache folder, deleted when the tab closes or
  fastcnsl exits. Scrolling, the scrollbar, search, line numbers, times,
  bookmarks and code colours all reach the oldest lines.
- **Copy all text copies the last 100 000 lines** of a tab, as does copying
  a longer selection. Save text to file and Log to file write the whole
  scrollback.
- The app is now called fastcnsl: its settings are in the `fastcnsl` config
  folder and its environment variables start with `FASTCNSL_`.
- The macOS download is a `.dmg`: open it and drag Fastcnsl onto
  Applications.
- Bug fixes.

## 0.1.84 (2026-11-09)

### Changed

- **The gutter's "Per command" mode, the default, now shows times instead of
  line numbers.** A command's line shows the local time it was entered at,
  and each line of its output how long after that it appeared, both as
  `HH:MM:SS`. For line numbers, choose "Per tab"
  in Options → General (`line_numbers = "tab"`).

### Added

- Bookmarks: right-click a line or its number, or press Ctrl+Shift+B
  (Cmd+B on macOS). Marked lines are tinted and stay marked as they scroll
  and rewrap; Ctrl+Shift+↑ / ↓ (Cmd+↑ / ↓) jump between them.
- Ctrl+1…Ctrl+0 go to tabs 1-10, and the tab menu lists the tabs.
- A bell on a tab means it printed something since you last had it selected.
- `fastcnsl -V` and `--version` print the version.

### Fixed

- With the gutter showing, clicks, selection, mouse reporting and the wheel
  landed about three rows above the pointer.

## 0.1.68 (2026-11-07)

### Added

- Windows: WSL tabs run through the `fastcnsl-wsl` relay instead of the
  console host, so large output comes through faster.
  `wsl_direct = false` goes back to the console host.
- Windows: Microsoft's console host (`conpty.dll`, `OpenConsole.exe`) ships
  with the app and is used for every other tab, about 7 times faster with
  large output than the one built into Windows.

### Changed

- Faster output once the scrollback is full, on x86-64.

## 0.1.65 (2026-11-07)

First tagged release.

### Terminal

- GPU-rendered terminal on wgpu, with a choice of graphics API: auto,
  DirectX, Vulkan, OpenGL, Metal or software.
- Tabs labelled with the last program run in them, numbered when narrow.
- Window buttons in the tab bar, or the OS title bar (`title_bar`).
- Box-drawing and block characters drawn as shapes.
- Text stays readable on coloured backgrounds (`minimum_contrast`).
- Light and dark themes that follow the OS.

### Features

- Syntax highlighting of code printed by `cat`, `head`, `bat`, `git diff`
  and similar commands.
- Search with all matches highlighted, and several search boxes at once,
  each with its own colour.
- Line numbers beside the text that hold as old lines leave the scrollback.
- Tab menu: copy all text, save to a file, or log to a file as output
  arrives (Options → Logging).
- Keyboard shortcuts can be changed in Options.
- Command line control: `fastcnsl new-tab [NAME] [-- COMMAND]`,
  `close-tab`, `quit`.

### Platforms

- Windows x64 and arm64, macOS arm64, Linux `.deb`, `.rpm` and AppImage.
- apt and rpm repositories on GitHub Pages, updated by every release.
- AppImage self-update through zsync.
- Windows: text rasterised with DirectWrite, with ClearType.
- Linux: `display_backend` chooses Wayland or X11.
- Folders, files and links open under WSL and without `xdg-open`.

### Fixed

- Windows: output could stop until a resize or key press, most visibly with
  large output and software rendering.
- Windows: no white window at startup.
