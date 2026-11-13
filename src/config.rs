//! User settings, stored as TOML in the platform config directory:
//!
//! * Linux:   `~/.config/fastcnsl/config.toml`
//! * macOS:   `~/Library/Application Support/fastcnsl/config.toml`
//! * Windows: `%APPDATA%\fastcnsl\config.toml`
//!
//! The file is created with defaults on first start and re-read whenever it
//! changes on disk, so edits apply without restarting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follow the operating system's light/dark setting.
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeMode {
    pub fn next(self) -> Self {
        match self {
            ThemeMode::System => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::System,
        }
    }
}

/// Times or line numbers in a gutter left of the text. A line is what a
/// program printed as one line: rows that wrap share its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LineNumbers {
    /// A command's line shows the local time it was entered, its output
    /// how long after that each line appeared.
    #[default]
    #[serde(alias = "time")]
    Command,
    /// Line numbers, counted from the start of the tab.
    Tab,
    Off,
}

impl LineNumbers {
    pub fn is_on(self) -> bool {
        self != LineNumbers::Off
    }
}

/// Which Linux display protocol to use. Ignored on Windows and macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DisplayBackend {
    /// winit's choice: Wayland when available, else X11.
    #[default]
    Auto,
    Wayland,
    X11,
}

impl DisplayBackend {
    /// The backend to force, if any. X11 needs libxkbcommon-x11 (winit
    /// panics without it), so without it the default is kept. Only called
    /// on Linux.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn resolve(self, x11_libs: bool) -> Option<DisplayBackend> {
        match self {
            DisplayBackend::Auto => None,
            DisplayBackend::X11 if !x11_libs => None,
            b => Some(b),
        }
    }
}

/// Where the window buttons (minimize, maximize, close) go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TitleBar {
    /// Buttons in the tab bar (on macOS: the native buttons, moved there).
    /// "auto" was the default in an earlier version; it means the same now.
    #[default]
    #[serde(alias = "auto")]
    Custom,
    /// The operating system's title bar, on its own line. Under WSLg's
    /// Wayland there is none, so the window then has no buttons.
    System,
}

impl TitleBar {
    /// True when the app draws its own title bar.
    pub fn is_custom(self) -> bool {
        self == TitleBar::Custom
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Font family name. Any installed font works; monospace is expected.
    pub font_family: String,
    /// Font size in points (1pt = 4/3 px at 100% scaling).
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// `system`, `dark` or `light`.
    pub theme: ThemeMode,
    /// Shell to start. Empty means the platform default ($SHELL, or
    /// PowerShell/cmd on Windows).
    pub shell: String,
    /// Arguments passed to the shell.
    pub shell_args: Vec<String>,
    /// RAM per tab for its text, in MB. The scrollback is unlimited:
    /// older lines go to a temporary file, deleted when the tab closes.
    pub memory_per_tab_mb: usize,
    /// Opacity (0.0–1.0) of the scrollbar and search bar while they are idle.
    /// They become fully opaque while hovered, dragged or typed into.
    pub ui_idle_opacity: f32,
    /// Inner padding around the terminal grid, in logical pixels.
    pub padding: f32,
    /// Detect source code printed by `cat`, `head`, `tail`, `bat`, `type`,
    /// `Get-Content`, `git diff`... and colour it.
    pub code_highlight: bool,
    /// Gutter: "command" (time since each command started), "tab" (line
    /// numbers from the start of the tab) or "off".
    pub line_numbers: LineNumbers,
    /// syntect theme used for code in dark mode.
    pub code_theme_dark: String,
    /// syntect theme used for code in light mode.
    pub code_theme_light: String,
    /// Render bold text in the bright variant of the 8 basic colours.
    pub bold_is_bright: bool,
    /// "custom" (window buttons in the tab bar) or "system" (the OS title
    /// bar). On macOS a change applies after a restart.
    pub title_bar: TitleBar,
    /// Linux only: "auto", "wayland" or "x11". Applies after a restart.
    pub display_backend: DisplayBackend,
    /// Text smoothing: "auto" (ClearType where Windows uses it, else
    /// grayscale), "subpixel" or "grayscale". Sub-pixel (ClearType) text
    /// is drawn on Windows only.
    pub antialiasing: Antialiasing,
    /// Text on a coloured background is moved towards black or white until
    /// its contrast ratio with the background is at least this (WCAG ratio:
    /// 1 turns it off, 4.5 is the usual "readable" level, 21 is the maximum).
    pub minimum_contrast: f32,
    /// Graphics API: "auto", "directx" (Windows), "vulkan", "opengl",
    /// "metal" (macOS) or "software" (drawn by the CPU). Applies after a
    /// restart; `fastcnsl --gpu NAME` overrides it for one run.
    pub gpu: GpuBackend,
    /// Windows: run WSL tabs (shell `wsl.exe`) through `fastcnsl-wsl`, a
    /// helper next to the exe that gives the shell a Linux pty and relays it
    /// over a pipe, instead of the Windows console (ConPTY). Large output
    /// then comes through faster than in Windows Terminal (36 MB `cat`:
    /// 0.33 s vs 0.40 s; through ConPTY 0.43 s). Off: WSL tabs use ConPTY
    /// like every other tab. Applies to new tabs.
    pub wsl_direct: bool,
    /// Folder for tab text saved or logged from a tab's right-click menu.
    /// Empty: `fastcnsl-logs` in the Documents folder (or the home
    /// folder where there is none). `~` stands for the home folder.
    pub log_folder: String,
    /// Log every new tab to a file from the start.
    pub log_new_tabs: bool,
    /// Start each logged line with the time it was logged.
    pub log_timestamps: bool,
    /// Changed keyboard shortcuts: command name -> keys, e.g.
    /// `new_tab = "Ctrl+T"`, `next_tab = "Ctrl+Tab, Ctrl+PageDown"`;
    /// "" turns a command off. Commands not listed keep their defaults.
    /// (Last: TOML writes tables after plain values.)
    pub keys: BTreeMap<String, String>,
}

/// Which graphics API draws the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuBackend {
    /// DirectX 12 on Windows, Metal on macOS, Vulkan or OpenGL on Linux.
    #[default]
    Auto,
    #[serde(rename = "directx")]
    DirectX,
    Vulkan,
    #[serde(rename = "opengl")]
    OpenGl,
    Metal,
    /// No GPU: a CPU rasteriser (WARP on Windows, llvmpipe/lavapipe on Linux).
    Software,
}

impl GpuBackend {
    /// The choices that exist on this platform.
    pub fn choices() -> &'static [GpuBackend] {
        use GpuBackend::*;
        if cfg!(windows) {
            &[Auto, DirectX, Vulkan, OpenGl, Software]
        } else if cfg!(target_os = "macos") {
            &[Auto, Metal, Software]
        } else {
            &[Auto, Vulkan, OpenGl, Software]
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GpuBackend::Auto => "Auto",
            GpuBackend::DirectX => "DirectX",
            GpuBackend::Vulkan => "Vulkan",
            GpuBackend::OpenGl => "OpenGL",
            GpuBackend::Metal => "Metal",
            GpuBackend::Software => "Software",
        }
    }

    /// Parses a name as written in the settings file or on the command
    /// line (any case; "dx12" and "gl" are accepted too).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "auto" => GpuBackend::Auto,
            "directx" | "dx12" | "d3d12" => GpuBackend::DirectX,
            "vulkan" | "vk" => GpuBackend::Vulkan,
            "opengl" | "gl" | "gles" => GpuBackend::OpenGl,
            "metal" => GpuBackend::Metal,
            "software" | "cpu" => GpuBackend::Software,
            _ => return None,
        })
    }
}

/// How glyph edges are smoothed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Antialiasing {
    #[default]
    Auto,
    /// ClearType: each colour sub-pixel gets its own coverage.
    Subpixel,
    Grayscale,
}

impl Antialiasing {
    /// Whether to draw sub-pixel text, given whether the system does.
    pub fn subpixel(self, system: bool) -> bool {
        match self {
            Antialiasing::Auto => system,
            Antialiasing::Subpixel => true,
            Antialiasing::Grayscale => false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: default_font().to_owned(),
            font_size: default_font_size(),
            line_height: 1.2,
            theme: ThemeMode::System,
            shell: String::new(),
            shell_args: Vec::new(),
            memory_per_tab_mb: 500,
            ui_idle_opacity: 0.4,
            padding: 6.0,
            code_highlight: true,
            line_numbers: LineNumbers::Command,
            code_theme_dark: "base16-ocean.dark".to_owned(),
            code_theme_light: "InspiredGitHub".to_owned(),
            bold_is_bright: false,
            title_bar: TitleBar::Custom,
            display_backend: DisplayBackend::Auto,
            antialiasing: Antialiasing::Auto,
            minimum_contrast: 4.5,
            gpu: GpuBackend::Auto,
            wsl_direct: true,
            log_folder: String::new(),
            log_new_tabs: false,
            log_timestamps: false,
            keys: BTreeMap::new(),
        }
    }
}

/// A setting the Options panel shows, for its ↺ button: shown while the
/// setting differs from its default, it puts back that default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Theme,
    CodeHighlight,
    LineNumbers,
    Gpu,
    FontFamily,
    FontSize,
    LineHeight,
    BoldIsBright,
    Antialiasing,
    LogFolder,
    LogNewTabs,
    LogTimestamps,
}

impl Setting {
    pub const ALL: [Setting; 12] = [
        Setting::Theme,
        Setting::CodeHighlight,
        Setting::LineNumbers,
        Setting::Gpu,
        Setting::FontFamily,
        Setting::FontSize,
        Setting::LineHeight,
        Setting::BoldIsBright,
        Setting::Antialiasing,
        Setting::LogFolder,
        Setting::LogNewTabs,
        Setting::LogTimestamps,
    ];

    /// True if `cfg` has this setting's default value.
    pub fn is_default(self, cfg: &Config) -> bool {
        let d = Config::default();
        match self {
            Setting::Theme => cfg.theme == d.theme,
            Setting::CodeHighlight => cfg.code_highlight == d.code_highlight,
            Setting::LineNumbers => cfg.line_numbers == d.line_numbers,
            Setting::Gpu => cfg.gpu == d.gpu,
            // The font list may spell a family with other capitals.
            Setting::FontFamily => cfg.font_family.eq_ignore_ascii_case(&d.font_family),
            Setting::FontSize => cfg.font_size == d.font_size,
            Setting::LineHeight => (cfg.line_height - d.line_height).abs() < 0.001,
            Setting::BoldIsBright => cfg.bold_is_bright == d.bold_is_bright,
            Setting::Antialiasing => cfg.antialiasing == d.antialiasing,
            Setting::LogFolder => cfg.log_folder == d.log_folder,
            Setting::LogNewTabs => cfg.log_new_tabs == d.log_new_tabs,
            Setting::LogTimestamps => cfg.log_timestamps == d.log_timestamps,
        }
    }

    /// Puts this setting in `cfg` back to its default.
    pub fn reset(self, cfg: &mut Config) {
        let d = Config::default();
        match self {
            Setting::Theme => cfg.theme = d.theme,
            Setting::CodeHighlight => cfg.code_highlight = d.code_highlight,
            Setting::LineNumbers => cfg.line_numbers = d.line_numbers,
            Setting::Gpu => cfg.gpu = d.gpu,
            Setting::FontFamily => cfg.font_family = d.font_family,
            Setting::FontSize => cfg.font_size = d.font_size,
            Setting::LineHeight => cfg.line_height = d.line_height,
            Setting::BoldIsBright => cfg.bold_is_bright = d.bold_is_bright,
            Setting::Antialiasing => cfg.antialiasing = d.antialiasing,
            Setting::LogFolder => cfg.log_folder = d.log_folder,
            Setting::LogNewTabs => cfg.log_new_tabs = d.log_new_tabs,
            Setting::LogTimestamps => cfg.log_timestamps = d.log_timestamps,
        }
    }

    /// The settings in `cfg` that differ from their defaults.
    pub fn changed(cfg: &Config) -> Vec<Setting> {
        Setting::ALL.into_iter().filter(|s| !s.is_default(cfg)).collect()
    }
}

/// Points. 12 on Windows, like Windows Terminal; 11 elsewhere.
pub fn default_font_size() -> f32 {
    if cfg!(target_os = "windows") { 12.0 } else { 11.0 }
}

pub fn default_font() -> &'static str {
    if cfg!(target_os = "windows") {
        "Cascadia Mono"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}

/// A file with this name next to fastcnsl.exe makes a portable copy: its
/// settings are kept in that folder. The Windows zip ships one.
pub const PORTABLE_MARKER: &str = "portable";

/// The settings file, from $FASTCNSL_CONFIG (`env`), the portable copy's
/// folder (`portable_dir`) and the system's config folder, in that order.
/// An empty $FASTCNSL_CONFIG counts as unset.
fn config_path_from(
    env: Option<std::ffi::OsString>,
    portable_dir: Option<&Path>,
    config_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = env.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(dir) = portable_dir {
        return Some(dir.join("config.toml"));
    }
    config_dir.map(|d| d.join("fastcnsl").join("config.toml"))
}

/// On Windows, the exe's folder when the portable marker sits next to it.
fn portable_dir() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    dir.join(PORTABLE_MARKER).is_file().then_some(dir)
}

impl Config {
    /// The settings file: $FASTCNSL_CONFIG if set; else, for a portable
    /// Windows copy (a `portable` file next to the exe), config.toml in the
    /// exe's folder; else <config dir>/fastcnsl/config.toml.
    pub fn path() -> Option<PathBuf> {
        config_path_from(std::env::var_os("FASTCNSL_CONFIG"), portable_dir().as_deref(), dirs::config_dir())
    }

    /// Loads the config file, writing a default one if none exists.
    /// A broken file is reported and the defaults are used instead.
    pub fn load_or_create() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match Self::load(&path) {
            Ok(Some(cfg)) => cfg,
            Ok(None) => {
                let cfg = Self::default();
                if let Err(e) = cfg.write_default(&path) {
                    log::warn!("could not write {}: {e}", path.display());
                }
                cfg
            }
            Err(e) => {
                log::error!("{e}; using defaults");
                Self::default()
            }
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        toml::from_str::<Config>(&text)
            .map(|c| Some(c.sanitized()))
            .map_err(|e| format!("parsing {}: {e}", path.display()))
    }

    /// Writes the settings file (used when settings change from the
    /// Options panel).
    pub fn save(&self) -> std::io::Result<()> {
        match Self::path() {
            Some(path) => self.write_default(&path),
            None => Ok(()),
        }
    }

    fn write_default(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let text = format!(
            "# fastcnsl settings. Saved changes apply immediately.\n\
             # theme: \"system\" | \"dark\" | \"light\"\n\
             # code themes: base16-ocean.dark, base16-eighties.dark, base16-mocha.dark,\n\
             #   base16-ocean.light, InspiredGitHub, Solarized (dark), Solarized (light)\n\n{body}"
        );
        std::fs::write(path, text)
    }

    /// The folder saved and logged tab text goes to.
    pub fn log_dir(&self) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        let folder = self.log_folder.trim();
        if folder.is_empty() {
            return dirs::document_dir().unwrap_or(home).join("fastcnsl-logs");
        }
        match folder.strip_prefix("~/").or_else(|| folder.strip_prefix("~\\")) {
            Some(rest) => home.join(rest),
            None if folder == "~" => home,
            None => PathBuf::from(folder),
        }
    }

    fn sanitized(mut self) -> Self {
        self.font_size = self.font_size.clamp(4.0, 96.0);
        self.line_height = self.line_height.clamp(0.8, 3.0);
        self.ui_idle_opacity = self.ui_idle_opacity.clamp(0.0, 1.0);
        self.padding = self.padding.clamp(0.0, 64.0);
        self.minimum_contrast = self.minimum_contrast.clamp(1.0, 21.0);
        self.memory_per_tab_mb = self.memory_per_tab_mb.clamp(64, 16384);
        self
    }
}

/// Polls the config file's modification time. Cheap enough to call about
/// once a second from the UI thread; avoids a file-watcher dependency.
pub struct ConfigWatcher {
    path: Option<PathBuf>,
    last: Option<SystemTime>,
}

impl ConfigWatcher {
    pub fn new() -> Self {
        Self::with_path(Config::path())
    }

    fn with_path(path: Option<PathBuf>) -> Self {
        let last = path.as_deref().and_then(mtime);
        Self { path, last }
    }

    /// Records the file's current mtime, so a save made by the app itself
    /// isn't reported as an outside change.
    pub fn mark_seen(&mut self) {
        self.last = self.path.as_deref().and_then(mtime);
    }

    /// Returns the new config if the file changed and parsed successfully.
    pub fn poll(&mut self) -> Option<Config> {
        let path = self.path.as_deref()?;
        let now = mtime(path);
        if now == self.last {
            return None;
        }
        self.last = now;
        match Config::load(path) {
            Ok(cfg) => Some(cfg.unwrap_or_default()),
            Err(e) => {
                log::error!("{e}");
                None
            }
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn partial_file_uses_defaults_for_missing_keys() {
        let cfg: Config = toml::from_str("font_size = 20.0\ntheme = \"light\"").unwrap();
        assert_eq!(cfg.font_size, 20.0);
        assert_eq!(cfg.theme, ThemeMode::Light);
        assert_eq!(cfg.memory_per_tab_mb, Config::default().memory_per_tab_mb);
    }

    #[test]
    fn old_scrollback_key_still_loads() {
        let cfg: Config = toml::from_str("scrollback = 5000\nfont_size = 20.0").unwrap();
        assert_eq!(cfg.font_size, 20.0);
        assert_eq!(cfg.memory_per_tab_mb, 500);
    }

    #[test]
    fn memory_is_clamped() {
        let low = Config { memory_per_tab_mb: 10, ..Config::default() }.sanitized();
        let high = Config { memory_per_tab_mb: 1_000_000, ..Config::default() }.sanitized();
        assert_eq!((low.memory_per_tab_mb, high.memory_per_tab_mb), (64, 16384));
    }

    #[test]
    fn settings_reset_to_their_defaults() {
        let mut cfg = Config::default();
        assert!(Setting::changed(&cfg).is_empty());
        cfg.theme = ThemeMode::Light;
        cfg.code_highlight = false;
        cfg.line_numbers = LineNumbers::Off;
        cfg.gpu = GpuBackend::Software;
        cfg.font_family = "Some Mono".into();
        cfg.font_size = 20.0;
        cfg.line_height = 1.5;
        cfg.bold_is_bright = true;
        cfg.antialiasing = Antialiasing::Grayscale;
        cfg.log_folder = "~/logs".into();
        cfg.log_new_tabs = true;
        cfg.log_timestamps = true;
        assert_eq!(Setting::changed(&cfg), Setting::ALL);
        for s in Setting::ALL {
            s.reset(&mut cfg);
            assert!(s.is_default(&cfg), "{s:?}");
        }
        assert_eq!(cfg, Config::default());
        // The font list may give the default family other capitals.
        cfg.font_family = cfg.font_family.to_uppercase();
        assert!(Setting::FontFamily.is_default(&cfg));
        // Line height stepped away and back by tenths.
        cfg.line_height = 13.0 / 10.0;
        cfg.line_height = ((cfg.line_height * 10.0).round() - 1.0) / 10.0;
        assert!(Setting::LineHeight.is_default(&cfg));
    }

    #[test]
    fn default_round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, Config::default());
    }

    /// A fresh directory under the system temp dir for one test.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fastcnsl-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn written_file_loads_back_with_comments() {
        let path = temp_dir("write").join("sub").join("config.toml");
        let cfg = Config { font_size: 15.0, theme: ThemeMode::Light, ..Config::default() };
        cfg.write_default(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# fastcnsl settings"));
        assert_eq!(Config::load(&path).unwrap(), Some(cfg));
    }

    #[test]
    fn missing_file_is_none_and_broken_file_is_an_error() {
        let dir = temp_dir("load");
        assert_eq!(Config::load(&dir.join("absent.toml")).unwrap(), None);
        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "font_size = \"big\"").unwrap();
        let err = Config::load(&bad).unwrap_err();
        assert!(err.contains("bad.toml"), "{err}");
    }

    #[test]
    fn watcher_reports_changes_once_and_skips_own_saves() {
        let path = temp_dir("watch").join("config.toml");
        let mut w = ConfigWatcher::with_path(Some(path.clone()));
        assert_eq!(w.poll(), None, "no file, no change");

        std::fs::write(&path, "font_size = 20.0").unwrap();
        assert_eq!(w.poll().map(|c| c.font_size), Some(20.0));
        assert_eq!(w.poll(), None, "reported once");

        // A broken edit is reported in the log, not applied.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "font_size = [").unwrap();
        assert_eq!(w.poll(), None);

        // The app's own save is marked as seen.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "font_size = 9.0").unwrap();
        w.mark_seen();
        assert_eq!(w.poll(), None);
    }

    #[test]
    fn line_numbers_setting() {
        assert_eq!(Config::default().line_numbers, LineNumbers::Command);
        for (text, want) in [("command", LineNumbers::Command), ("time", LineNumbers::Command), ("tab", LineNumbers::Tab), ("off", LineNumbers::Off)] {
            let cfg: Config = toml::from_str(&format!("line_numbers = \"{text}\"")).unwrap();
            assert_eq!(cfg.line_numbers, want);
        }
        assert!(LineNumbers::Tab.is_on() && !LineNumbers::Off.is_on());
    }

    #[test]
    fn log_folder_setting() {
        let cfg = Config::default();
        assert!(cfg.log_dir().ends_with("fastcnsl-logs"));
        assert!(!cfg.log_new_tabs && !cfg.log_timestamps);
        let at = |f: &str| Config { log_folder: f.into(), ..Config::default() }.log_dir();
        assert_eq!(at("/var/log/term"), PathBuf::from("/var/log/term"));
        if let Some(home) = dirs::home_dir() {
            assert_eq!(at("~/logs"), home.join("logs"));
            assert_eq!(at("~"), home);
        }
        let cfg: Config = toml::from_str("log_folder = \"/tmp/x\"\nlog_new_tabs = true\nlog_timestamps = true").unwrap();
        assert_eq!((cfg.log_folder.as_str(), cfg.log_new_tabs, cfg.log_timestamps), ("/tmp/x", true, true));
    }

    #[test]
    fn display_backend() {
        assert_eq!(DisplayBackend::Auto.resolve(true), None, "auto keeps winit's choice");
        assert_eq!(DisplayBackend::Wayland.resolve(false), Some(DisplayBackend::Wayland));
        assert_eq!(DisplayBackend::X11.resolve(true), Some(DisplayBackend::X11));
        assert_eq!(DisplayBackend::X11.resolve(false), None, "no libxkbcommon-x11: don't crash");
        let cfg: Config = toml::from_str("display_backend = \"x11\"").unwrap();
        assert_eq!(cfg.display_backend, DisplayBackend::X11);
    }

    #[test]
    fn title_bar_setting() {
        assert!(TitleBar::Custom.is_custom());
        assert!(!TitleBar::System.is_custom());
        assert_eq!(Config::default().title_bar, TitleBar::Custom);
        let cfg: Config = toml::from_str("title_bar = \"system\"").unwrap();
        assert_eq!(cfg.title_bar, TitleBar::System);
        // The boolean key from earlier versions is ignored.
        let cfg: Config = toml::from_str("custom_title_bar = false").unwrap();
        assert_eq!(cfg.title_bar, TitleBar::Custom);
    }

    #[test]
    fn theme_mode_cycles_through_all_three() {
        let m = ThemeMode::System;
        assert_eq!(m.next(), ThemeMode::Dark);
        assert_eq!(m.next().next(), ThemeMode::Light);
        assert_eq!(m.next().next().next(), ThemeMode::System);
    }

    #[test]
    fn values_are_clamped() {
        let cfg: Config = toml::from_str("[keys]\nnew_tab = \"Ctrl+T\"\nfind = \"\"").unwrap();
        assert_eq!(cfg.keys.get("new_tab").map(String::as_str), Some("Ctrl+T"));
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap().keys, cfg.keys, "keys round-trip");
        let cfg: Config = toml::from_str("gpu = \"opengl\"").unwrap();
        assert_eq!(cfg.gpu, GpuBackend::OpenGl);
        for g in GpuBackend::choices() {
            let text = toml::to_string(&Config { gpu: *g, ..Config::default() }).unwrap();
            assert_eq!(toml::from_str::<Config>(&text).unwrap().gpu, *g, "{g:?} round-trips");
            assert_eq!(GpuBackend::from_name(g.label()), Some(*g), "{g:?} parses from its label");
        }
        assert_eq!(GpuBackend::from_name("DX12"), Some(GpuBackend::DirectX));
        assert_eq!(GpuBackend::from_name("nope"), None);
        let cfg: Config = toml::from_str("antialiasing = \"grayscale\"").unwrap();
        assert!(!cfg.antialiasing.subpixel(true));
        assert!(Antialiasing::Auto.subpixel(true) && !Antialiasing::Auto.subpixel(false));
        assert!(Antialiasing::Subpixel.subpixel(false));
        let cfg: Config = toml::from_str("title_bar = \"auto\"").unwrap();
        assert_eq!(cfg.title_bar, TitleBar::Custom, "older configs say auto");
        // A whole file written by that version.
        let old = "font_family = \"Cascadia Mono\"\nfont_size = 11.0\nshell_args = []\n\
                   title_bar = \"auto\"\ndisplay_backend = \"auto\"\n";
        let dir = std::env::temp_dir().join(format!("fastcnsl-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, old).unwrap();
        let cfg = Config::load(&path).expect("old file loads").unwrap();
        assert_eq!((cfg.title_bar, cfg.font_size), (TitleBar::Custom, 11.0));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg: Config = toml::from_str("font_size = 1000.0\nui_idle_opacity = 7.0").unwrap();
        let cfg = cfg.sanitized();
        assert_eq!(cfg.font_size, 96.0);
        assert_eq!(cfg.ui_idle_opacity, 1.0);
    }
}
