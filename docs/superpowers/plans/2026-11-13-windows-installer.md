# Windows Installer and Portable Build Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every CI build produces, for both Windows x86-64 and ARM64, a portable zip (settings kept next to the exe) and an Inno Setup installer (`-setup.exe`). Releases publish both.

**Architecture:** fastcnsl.exe gets an embedded icon (build.rs + windres). `scripts/package/windows.sh` stages the shipped files once. The zip step adds a `portable` marker file. A new arch-independent Docker image (Debian + Wine + Inno Setup 6) compiles `packaging/windows/fastcnsl.iss` against the staged folder. Makefile targets and the CI matrix call it the same way locally and in CI.

**Tech Stack:** Rust 2024 (build.rs, no new crates), Inno Setup 6 under Wine, Docker, GNU make, GitHub Actions, Python 3 (icon script).

**Spec:** `docs/superpowers/specs/2026-11-13-windows-installer-design.md`

## Global Constraints

- Free only: no code signing, no paid services.
- Inno `AppId` is fixed forever: `{4FFEA6F9-A30F-4EF2-9FF2-6CF7D8C247F9}`. The same id for both architectures.
- Version is `scripts/get_version.sh` (`0.1.N`), already exported as `FASTCNSL_VERSION` in every container.
- Per-dist file names: `fastcnsl-<ver>-windows-{x86_64,arm64}.zip` (unchanged) and `fastcnsl-<ver>-windows-{x86_64,arm64}-setup.exe`.
- Release file names: `fastcnsl-windows-x86_64-setup.exe`, `fastcnsl-windows-arm64-setup.exe`, plus a `SHA256SUMS` file covering every release asset.
- Default install: per user, `{autopf}\fastcnsl` with `PrivilegesRequired=lowest` (= `%LOCALAPPDATA%\Programs\fastcnsl`). All-users install offered via `PrivilegesRequiredOverridesAllowed=dialog commandline`.
- Setup tasks (checkboxes): `startmenu` "Add to Start menu" (checked), `desktopicon` "Add a desktop icon" (unchecked), `addtopath` "Add fastcnsl to PATH" (checked).
- `MinVersion=10.0.17763`.
- Uninstall never touches `%APPDATA%\fastcnsl`.
- Every cargo/packaging command runs in Docker via `make` (docs/DEVELOPMENT.md). For quick Linux unit tests you can also run cargo locally with `export PATH="$HOME/.cargo/bin:$PATH"` (system cargo is too old).
- Work on branch `windows-installer` in a worktree. The main working tree has the user's uncommitted edits, so never commit from it. Never push.

## Review Focus

1. **Upgrade over a running fastcnsl**, with tabs open (OpenConsole.exe running from `{app}`). Setup must close them and replace files, not fail with "file in use". Pinned by `CloseApplications=force` and checked in Task 4's smoke test.
2. **Running the wrong-arch setup.** Setup must refuse with a message naming the other download, not install x64 binaries on ARM. Checked in Task 4 by running the arm64 setup on the x64 host and grepping its log.
3. **Unchecking a task on upgrade** (e.g. Start menu unchecked the second time). The old shortcut must go away. Pinned by `[InstallDelete]` entries with `Tasks: not ...`, checked in the Task 4 smoke test.
4. **A portable folder that is read-only, or whose `config.toml` is broken.** Behaviour must match today's for `%APPDATA%`: warn and use defaults, never crash. Covered because `load_or_create` is unchanged and only `path()` moves. The Task 1 test pins that the marker only changes the path.
5. **PATH that already contains the install dir, or a trailing `\`.** Must not be duplicated on reinstall, and uninstall must remove only our entry. Checked in the Task 4 smoke test (install twice, read PATH, uninstall, read PATH).

---

### Task 1: Portable settings location

**Files:**
- Modify: `src/config.rs` (`Config::path`, around line 384; tests module around line 518)
- Modify: `README.md` ("Settings file" section, around line 290)

**Interfaces:**
- Produces: `Config::path()`, which behaves as before unless running on Windows with a file named `portable` next to the exe. Then it returns `<exe dir>/config.toml`.
- Produces (pure, testable): `fn config_path_from(env: Option<std::ffi::OsString>, portable_dir: Option<&Path>, config_dir: Option<PathBuf>) -> Option<PathBuf>`
- Produces: `const PORTABLE_MARKER: &str = "portable";` (Task 3 ships a file with this name)

- [ ] **Step 1: Write the failing tests** in `src/config.rs` `mod tests`:

```rust
#[test]
fn config_path_env_wins_over_portable() {
    let p = config_path_from(Some("/x/my.toml".into()), Some(Path::new("/app")), Some("/cfg".into()));
    assert_eq!(p, Some(PathBuf::from("/x/my.toml")));
}
#[test]
fn config_path_portable_is_next_to_exe() {
    let p = config_path_from(None, Some(Path::new("/app")), Some("/cfg".into()));
    assert_eq!(p, Some(PathBuf::from("/app/config.toml")));
}
#[test]
fn config_path_default_is_config_dir() {
    let p = config_path_from(None, None, Some("/cfg".into()));
    assert_eq!(p, Some(PathBuf::from("/cfg/fastcnsl/config.toml")));
    assert_eq!(config_path_from(Some("".into()), None, None), None);
}
```

- [ ] **Step 2: Run them, expect a compile failure** (`config_path_from` not defined)

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test config_path`

- [ ] **Step 3: Implement.** `config_path_from` holds today's logic, plus the portable branch between env and config dir. An empty env value counts as unset, as today. `Config::path()` calls it with `std::env::var_os("FASTCNSL_CONFIG")`, `portable_dir()` and `dirs::config_dir()`. Add a private `fn portable_dir() -> Option<PathBuf>`. On Windows (`cfg!(windows)`) it returns the exe's directory if `<exe dir>/PORTABLE_MARKER` is a file. Otherwise it returns `None`. Update the `path()` doc comment.

- [ ] **Step 4: Run the tests, expect them to pass.** Then run `make clippy` and `make check-all`. Expected: clean.

- [ ] **Step 5: README.** Under "Settings file", add one line after the Windows path: "Portable zip: `config.toml` next to `fastcnsl.exe`, as long as the file `portable` is there (delete it to use `%APPDATA%`)."

- [ ] **Step 6: Commit** `src/config.rs README.md`: "Portable Windows copy keeps its settings next to the exe"

### Task 2: Application icon in fastcnsl.exe

**Files:**
- Create: `scripts/package/make_ico.py` (same style as `make_icns.py`)
- Create: `packaging/windows/fastcnsl.ico` (generated, committed)
- Create: `packaging/windows/fastcnsl.rc` (one line: `1 ICON "fastcnsl.ico"`)
- Create: `build.rs`
- Modify: `Dockerfile.dev` (windres env for the arm64 check)

**Interfaces:**
- Produces: `packaging/windows/fastcnsl.ico`, which Task 4 uses as `SetupIconFile`. Resource id 1, so `{app}\fastcnsl.exe` works as an icon source for shortcuts and `UninstallDisplayIcon`.

- [ ] **Step 1: `make_ico.py out.ico 16.png 24.png 32.png 48.png 64.png 128.png 256.png`** writes an ICO with PNG-compressed entries: ICONDIR (reserved 0, type 1, count), then 16-byte entries (width/height byte, 0 meaning 256; planes 1; bpp 32; size; offset), then the PNG data. Reject non-square input, as make_icns.py does.

- [ ] **Step 2: Generate the icon** in the dev container (it has ImageMagick). Render each size from `packaging/icons/fastcnsl.svg` with transparent background and 32-bit RGBA, run make_ico.py, and commit the .ico. Verify with `file packaging/windows/fastcnsl.ico`, expected: "MS Windows icon resource - 7 icons".

- [ ] **Step 3: `build.rs`.** It does nothing unless `CARGO_CFG_TARGET_OS == "windows"` and `CARGO_CFG_TARGET_ENV` is `gnu` (covers gnullvm too; `CARGO_CFG_TARGET_ABI` distinguishes them). For MSVC, print a `cargo:warning` and skip. Choose windres from the env var `WINDRES_<target with - → _>` if set, else `<x86_64|aarch64>-w64-mingw32-windres` from PATH. Run `windres packaging/windows/fastcnsl.rc -O coff -o $OUT_DIR/fastcnsl-res.o`, then `cargo:rustc-link-arg-bins=$OUT_DIR/fastcnsl-res.o`. Emit `rerun-if-changed` for the .rc, the .ico and build.rs. If windres fails, panic with a message naming the tool.

- [ ] **Step 4: Dockerfile.dev.** llvm-mingw isn't on PATH there, so add `WINDRES_aarch64_pc_windows_gnullvm=/opt/llvm-mingw/bin/aarch64-w64-mingw32-windres` to the existing `ENV`. The release images already have both windres tools on PATH. Check with `docker run --rm fastcnsl-windows-x86_64 which x86_64-w64-mingw32-windres` and the arm64 equivalent. If one is missing, add its env var to that Dockerfile instead.

- [ ] **Step 5: Verify.** Run `make check-all` (clean) and `make windows-x86_64 windows-arm64`. Then check that each exe has the icon: `x86_64-w64-mingw32-objdump -h <exe> | grep .rsrc` and `llvm-objdump -h <arm exe> | grep .rsrc`, run inside the matching image. Expected: a `.rsrc` section in both. Also run `make linux-x86_64` to confirm build.rs is a no-op there.

- [ ] **Step 6: Commit** the new files and `Dockerfile.dev`: "fastcnsl.exe gets its icon"

### Task 3: Shared staging; zip gets the portable marker

**Files:**
- Modify: `scripts/package/windows.sh`
- Create: `packaging/windows/portable` (text file)

**Interfaces:**
- Consumes: `PORTABLE_MARKER` = `portable` (Task 1).
- Produces: `windows.sh <arch> stage` leaves the installer payload in `$TARGET_DIR/package-windows-$ARCH/fastcnsl/`. Task 4 reads it at `target/windows-<arch>/package-windows-<arch>/fastcnsl/`. That folder does **not** contain `portable`.

- [ ] **Step 1: `packaging/windows/portable`.** Two or three lines: while this file sits next to fastcnsl.exe, settings are kept in this folder (config.toml); delete it to use `%APPDATA%\fastcnsl`.

- [ ] **Step 2: Split `windows.sh`.** Move the current staging code (exe, relay, README, LICENSE, ConPTY and its license, bundled toolchain DLLs) into a `stage` step. `zip` runs `stage`, copies the stage to `$TARGET_DIR/package-windows-$ARCH/zip/fastcnsl/`, adds `packaging/windows/portable`, and zips that. Usage line: `build|stage|zip`.

- [ ] **Step 3: Verify.** Run `make windows-x86_64-zip windows-arm64-zip`, then `unzip -l` each zip. Expected: the same file list as before plus `fastcnsl/portable`. `ls target/windows-x86_64/package-windows-x86_64/fastcnsl/` must not list `portable`.

- [ ] **Step 4: Commit:** "Windows: stage once; the zip is marked portable"

### Task 4: Inno Setup installer

**Files:**
- Create: `docker/windows-setup.Dockerfile`
- Create: `packaging/windows/fastcnsl.iss`
- Create: `scripts/package/windows-setup.sh`
- Modify: `Makefile` (header comment, `PACKAGES`, image rule, two targets, `distclean` image list)
- Modify: `docs/DEVELOPMENT.md` (make target list, around line 21)

**Interfaces:**
- Consumes: the stage folder (Task 3) and `packaging/windows/fastcnsl.ico` (Task 2).
- Produces: `make windows-x86_64-setup` and `make windows-arm64-setup`, which write `dist/fastcnsl-<ver>-windows-<arch>-setup.exe`.

- [ ] **Step 1: Image `docker/windows-setup.Dockerfile`.** Base it on `debian:bookworm-slim`. Add i386, then install `wine wine32 wine64 xvfb xauth ca-certificates curl` (`wine32` because ISCC.exe is 32-bit; drop it if the pinned Inno ships a 64-bit ISCC). Download the latest Inno Setup 6.x installer, pinned `ARG INNO_VERSION` plus `ARG INNO_SHA256`, and check the sha256. Install it at build time with `xvfb-run wine innosetup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR=C:\\inno`. Copy `$WINEPREFIX/drive_c/inno` to `/opt/inno`, then delete the build prefix. Wine refuses a prefix owned by another uid, and containers run as the host user, so the runtime prefix is created fresh under `HOME=/tmp`. Set `ENV WINEDEBUG=-all WINEDLLOVERRIDES=mscoree,mshtml=` so first start doesn't prompt for Mono/Gecko. No `COPY` (the build context is `docker/`). `WORKDIR /work`.

- [ ] **Step 2: `packaging/windows/fastcnsl.iss`.** It takes `/DAppVersion=`, `/DArch=x64|arm64`, `/DSourceDir=` and `/DIconFile=` on the ISCC command line. Required contents:
  - `[Setup]`: `AppId={{4FFEA6F9-A30F-4EF2-9FF2-6CF7D8C247F9}`. `AppName=fastcnsl`, `AppVersion`, `VersionInfoVersion={#AppVersion}.0`. `AppPublisher=Andrei Pushkin`, `AppPublisherURL=https://github.com/andrewpushkin/fastcnsl`. `DefaultDirName={autopf}\fastcnsl`, `DisableProgramGroupPage=yes`. `PrivilegesRequired=lowest`, `PrivilegesRequiredOverridesAllowed=dialog commandline`. `ArchitecturesAllowed` and `ArchitecturesInstallIn64BitMode` are `x64os` when Arch is x64, `arm64` when it's arm64. `MinVersion=10.0.17763`, `CloseApplications=force`, `RestartApplications=no`, `ChangesEnvironment=yes`. `SetupIconFile={#IconFile}`, `UninstallDisplayIcon={app}\fastcnsl.exe`, `UninstallDisplayName=fastcnsl`. `WizardStyle=modern`, `Compression=lzma2/max`, `SolidCompression=yes`. `OutputBaseFilename` and `OutputDir` come from the command line (`/F`, `/O`).
  - `[Messages]` `OnlyOnTheseArchitectures=` "This is the <x64|ARM64> installer. Download fastcnsl-windows-<the other>-setup.exe for this PC." (pick via `#if`).
  - `[Tasks]`: `startmenu` (checked), `desktopicon` (`Flags: unchecked`), `addtopath` (checked), with the exact captions from Global Constraints.
  - `[Files]`: `{#SourceDir}\*` → `{app}`, `recursesubdirs ignoreversion`.
  - `[InstallDelete]`: remove `{autoprograms}\fastcnsl.lnk` when `Tasks: not startmenu`, and `{autodesktop}\fastcnsl.lnk` when `Tasks: not desktopicon`.
  - `[Icons]`: `{autoprograms}\fastcnsl` with `Tasks: startmenu`, and `{autodesktop}\fastcnsl` with `Tasks: desktopicon`.
  - `[Run]`: launch `{app}\fastcnsl.exe` with `postinstall nowait skipifsilent`.
  - `[Code]`: add and remove `{app}` in `Path`. Use `HKCU\Environment` normally and `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` when `IsAdminInstallMode`. Compare case-insensitively, ignoring a trailing `\`, and never add a duplicate. Write `REG_EXPAND_SZ` when the value was expand-sz. Add in `CurStepChanged(ssPostInstall)` if `WizardIsTaskSelected('addtopath')`, otherwise remove (an upgrade that unchecks it). Remove in `CurUninstallStepChanged(usPostUninstall)`. wezterm's `ci/windows-installer.iss` `EnvAddPath`/`EnvRemovePath` is a reference for the string handling.

- [ ] **Step 3: `scripts/package/windows-setup.sh x86_64|arm64`.** Source `common.sh`. Map x86_64 to Arch=x64 and arm64 to arm64. Fail with the `need_binary`-style hint if the stage folder is missing. Convert paths with `winepath -w`. Run `wine /opt/inno/ISCC.exe /Q /DAppVersion=$VERSION /DArch=… /DSourceDir=… /DIconFile=… /O<dist> /Ffastcnsl-$VERSION-windows-$ARCH-setup packaging/windows/fastcnsl.iss`, then print `-> dist/…`.

- [ ] **Step 4: Makefile.** Add `windows-x86_64-setup: windows-x86_64-zip target/.image-windows-setup`, which runs `$(call docker_run,fastcnsl-windows-setup,target/windows-x86_64) scripts/package/windows-setup.sh x86_64`. Add the arm64 twin. The pattern rule `target/.image-%` already builds the image. Add both to `PACKAGES` and the header comment. Add `fastcnsl-windows-setup` to the `distclean` image list. Add the two targets to docs/DEVELOPMENT.md's list.

- [ ] **Step 5: Build.** Run `make windows-x86_64-setup windows-arm64-setup`. Expected: both `dist/fastcnsl-<ver>-windows-{x86_64,arm64}-setup.exe` exist, and `file` reports a "PE32" Windows executable.

- [ ] **Step 6: Smoke test on the Windows host (via WSL interop). Ask the user before running it:** it installs per user on their machine, then uninstalls. Copy the setup to a Windows temp dir. Then:
  1. Run `setup.exe /VERYSILENT /SUPPRESSMSGBOXES /LOG=…\i1.log /DIR=<temp>\fc /TASKS="startmenu,addtopath"`. Check the files, the Start menu .lnk, no desktop .lnk, and that `HKCU\Environment` Path contains the dir exactly once (`reg query`).
  2. Start the installed `fastcnsl.exe`, wait 3 s, then rerun setup with `/TASKS="addtopath"`. Expected: exit code 0, fastcnsl was closed (log shows Restart Manager closing it), the Start menu .lnk is gone, and Path still has the dir once.
  3. Run `"<dir>\unins000.exe" /VERYSILENT`. Expected: the dir is removed, Path no longer has it, and `%APPDATA%\fastcnsl` is untouched.
  4. Run the arm64 setup with `/VERYSILENT /LOG=…\a.log`. Expected: nonzero exit and nothing installed. The log shows the architecture refusal.

- [ ] **Step 7: Commit:** "Windows installer (Inno Setup) for x86-64 and ARM64"

### Task 5: CI, release and README

**Files:**
- Modify: `.github/workflows/build.yml`
- Modify: `README.md` (download table, lines ~16-17; Windows install section, lines ~28-40)

- [ ] **Step 1: Workflow.**
  - Matrix `packages`: `windows-x86_64-zip windows-x86_64-setup`, and the arm64 equivalent.
  - Upload paths: add `dist/*-setup.exe`.
  - "Name the files" step: copy `dist/fastcnsl-*-windows-x86_64-setup.exe` to `upload/fastcnsl-windows-x86_64-setup.exe`, and arm64 the same way. The zip globs `fastcnsl-*-windows-x86_64.zip` don't match `-setup.exe`, so keep them.
  - After the zsync step: `(cd upload && sha256sum * > SHA256SUMS)`.
  - Update the header comment, which mentions zips.

- [ ] **Step 2: README.** The table gets "Windows (x64) — installer: …-setup.exe, portable: ….zip" and the same for ARM. The Windows install section gets:
  - Installer: per user, no admin, Start menu / desktop / PATH checkboxes. A newer setup upgrades in place. Uninstall from Settings → Apps.
  - Portable: unzip anywhere; settings stay in the folder.
  - Unsigned: SmartScreen shows "Windows protected your PC". Click **More info → Run anyway**.
  - Keep the existing notes on the companion files.

- [ ] **Step 3: Verify.** Run `python3 -c 'import yaml,sys;yaml.safe_load(open(".github/workflows/build.yml"))'` (valid YAML) and `make all`. Expected: all 8 packages are listed in `dist/`. CI itself runs when the branch is pushed, and pushing is the user's call.

- [ ] **Step 4: Commit:** "CI: publish Windows installers and SHA256SUMS"
