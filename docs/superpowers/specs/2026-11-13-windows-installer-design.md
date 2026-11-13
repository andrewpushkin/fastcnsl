# Windows installer and portable build

## Why

Windows users get a zip today. We want two ways to get fastcnsl on Windows,
for both x86-64 and ARM64: a portable build and an installer. Everything must be free
(no paid code signing). Updating from inside the app is the next step, not
this one. The installer is designed so that step is easy.

## Goals

- Every CI build (pull requests and every push to main) produces four
  Windows files:
  `fastcnsl-<ver>-windows-{x86_64,arm64}.zip` (portable) and
  `fastcnsl-<ver>-windows-{x86_64,arm64}-setup.exe` (installer).
- Releases publish them under fixed names for `releases/latest/download/`
  links: `fastcnsl-windows-x86_64-setup.exe`, `fastcnsl-windows-arm64-setup.exe`
  (the zips keep their current names).
- `make windows-x86_64-setup` / `make windows-arm64-setup` build the same
  installers locally, using the same Docker setup CI uses.

## Non-goals

- In-app update (next task). Code signing, MSIX, Microsoft Store.
- winget / Scoop manifests (can be added later from the same files).
- An Explorer "Open fastcnsl here" menu item (needs a `--cwd` option the
  app doesn't have).

## Approaches considered

1. **Inno Setup compiled under Wine in Docker (chosen).** Free, supports
   ARM64, can install per user without admin, upgrades in place, and runs silently
   (`/VERYSILENT`). It's what wezterm uses. Keeps the rule that local `make` and
   CI build Windows packages the same way.
2. Inno Setup on GitHub's Windows runner (wezterm's way). It's simpler in CI,
   but the installer could not be built locally from Linux.
3. MSI (WiX / wixl). WiX needs Windows. wixl's ARM64 support is weak, and
   per-user MSI upgrades are awkward.

## Design

### Installer (`packaging/windows/fastcnsl.iss`)

- One setup per architecture, matching the zips. The x64 setup installs
  only on x64 Windows. The ARM64 setup installs only on ARM64 Windows. Each one
  tells the user to download the other build if it's run on the wrong machine.
- Installs per user by default into `%LOCALAPPDATA%\Programs\fastcnsl`
  with no admin prompt. The setup also offers "install for all users"
  (`PrivilegesRequiredOverridesAllowed=dialog`). Installing per user means
  the future updater can replace files without a UAC prompt.
- A fixed `AppId` GUID, used forever, so a newer setup upgrades the existing
  install in place. Restart Manager closes running fastcnsl (and its
  OpenConsole.exe) before replacing files.
- Installs the same files as the zip: fastcnsl.exe, fastcnsl-wsl,
  conpty.dll, OpenConsole.exe, any bundled toolchain DLL, LICENSE,
  ConPTY-LICENSE.txt, README.md.
- Three checkboxes on the setup's "Additional tasks" page:
  "Add to Start menu" (checked), "Add a desktop icon" (unchecked) and
  "Add fastcnsl to PATH" (checked). PATH means the user's PATH, or the
  machine's on an all-users install. Uninstalling removes whichever were added.
  On an upgrade, setup remembers the previous choices.
- Shows up in Settings → Apps with its icon and version, and has an uninstaller.
  Uninstalling leaves the user's settings in `%APPDATA%\fastcnsl` alone.
- Version = `scripts/get_version.sh` (0.1.N), passed as `/DAppVersion=`.
- Minimum Windows 10 1809 (ConPTY), the same as the README says today.

### Icon

- `packaging/windows/fastcnsl.ico` (16–256 px), generated once from
  `packaging/icons/fastcnsl.svg` and committed.
- Embedded into fastcnsl.exe with a small `build.rs` (resource file
  compiled by the mingw / llvm-mingw `windres` already in the images), so
  the exe, shortcuts, taskbar and Apps list all show it. The setup uses it
  as `SetupIconFile`.

### Portable zip

- If a file named `portable` sits next to fastcnsl.exe, settings
  (config.toml) are read from and saved to that folder instead of
  `%APPDATA%\fastcnsl`. The zip ships that file, with a line inside
  saying what it does. Delete it to use `%APPDATA%` again.
  `FASTCNSL_CONFIG` still overrides both. Logs (a user-chosen folder) and the
  scrollback history cache (temporary data in `%LOCALAPPDATA%`) don't
  change.
- The installed copy has no `portable` file, so it behaves as today.

### Build

- New image `docker/windows-setup.Dockerfile`: Debian with Wine (32-bit,
  because ISCC.exe is a 32-bit program) and Inno Setup 6 installed silently,
  with a pinned version and checksum. One image for both architectures:
  it only packages files that are already built.
- `scripts/package/windows.sh` gets a `stage` step (shared by zip and
  setup, so both contain identical files). A new `scripts/package/windows-setup.sh`
  runs ISCC under Wine on the staged folder.
- Makefile: `windows-{x86_64,arm64}-setup` targets, added to `PACKAGES`.
- CI (`.github/workflows/build.yml`): the Windows matrix rows also build
  `-setup`. Upload and release steps include `*-setup.exe`, plus a
  `SHA256SUMS` file for all release assets.
- README: the download table gets installer rows. The Windows install section
  explains installer vs portable and the SmartScreen warning on unsigned
  files ("More info → Run anyway").

## Left to the builder

- Exact Wine/Inno versions and how to keep Wine headless in Docker.
- Wording of setup pages and the `portable` file text.
- Whether README.md is shipped as is or converted to .txt.
