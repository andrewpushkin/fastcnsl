//! User settings, stored as TOML in the platform config directory:
//!
//! * Linux:   `~/.config/ragamuffin/config.toml`
//! * macOS:   `~/Library/Application Support/ragamuffin/config.toml`
//! * Windows: `%APPDATA%\ragamuffin\config.toml`
//!
//! The file is created with defaults on first start and re-read whenever it
//! changes on disk, so edits apply without restarting.

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
    /// Lines of scrollback history.
    pub scrollback: usize,
    /// Opacity (0.0–1.0) of the scrollbar and search bar while they are idle.
    /// They become fully opaque while hovered, dragged or typed into.
    pub ui_idle_opacity: f32,
    /// Inner padding around the terminal grid, in logical pixels.
    pub padding: f32,
    /// Detect source code printed by `cat`, `head`, `tail`, `bat`, `type`,
    /// `Get-Content`, `git diff`... and colour it.
    pub code_highlight: bool,
    /// syntect theme used for code in dark mode.
    pub code_theme_dark: String,
    /// syntect theme used for code in light mode.
    pub code_theme_light: String,
    /// Render bold text in the bright variant of the 8 basic colours.
    pub bold_is_bright: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: default_font().to_owned(),
            font_size: 11.0,
            line_height: 1.2,
            theme: ThemeMode::System,
            shell: String::new(),
            shell_args: Vec::new(),
            scrollback: 10_000,
            ui_idle_opacity: 0.4,
            padding: 6.0,
            code_highlight: true,
            code_theme_dark: "base16-ocean.dark".to_owned(),
            code_theme_light: "InspiredGitHub".to_owned(),
            bold_is_bright: false,
        }
    }
}

fn default_font() -> &'static str {
    if cfg!(target_os = "windows") {
        "Cascadia Mono"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("ragamuffin").join("config.toml"))
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
            "# ragamuffin settings. Saved changes apply immediately.\n\
             # theme: \"system\" | \"dark\" | \"light\"\n\
             # code themes: base16-ocean.dark, base16-eighties.dark, base16-mocha.dark,\n\
             #   base16-ocean.light, InspiredGitHub, Solarized (dark), Solarized (light)\n\n{body}"
        );
        std::fs::write(path, text)
    }

    fn sanitized(mut self) -> Self {
        self.font_size = self.font_size.clamp(4.0, 96.0);
        self.line_height = self.line_height.clamp(0.8, 3.0);
        self.ui_idle_opacity = self.ui_idle_opacity.clamp(0.0, 1.0);
        self.padding = self.padding.clamp(0.0, 64.0);
        self.scrollback = self.scrollback.min(1_000_000);
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
    fn partial_file_uses_defaults_for_missing_keys() {
        let cfg: Config = toml::from_str("font_size = 20.0\ntheme = \"light\"").unwrap();
        assert_eq!(cfg.font_size, 20.0);
        assert_eq!(cfg.theme, ThemeMode::Light);
        assert_eq!(cfg.scrollback, Config::default().scrollback);
    }

    #[test]
    fn default_round_trips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, Config::default());
    }

    /// A fresh directory under the system temp dir for one test.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ragamuffin-test-{}-{name}", std::process::id()));
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
        assert!(text.starts_with("# ragamuffin settings"));
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
    fn theme_mode_cycles_through_all_three() {
        let m = ThemeMode::System;
        assert_eq!(m.next(), ThemeMode::Dark);
        assert_eq!(m.next().next(), ThemeMode::Light);
        assert_eq!(m.next().next().next(), ThemeMode::System);
    }

    #[test]
    fn values_are_clamped() {
        let cfg: Config = toml::from_str("font_size = 1000.0\nui_idle_opacity = 7.0").unwrap();
        let cfg = cfg.sanitized();
        assert_eq!(cfg.font_size, 96.0);
        assert_eq!(cfg.ui_idle_opacity, 1.0);
    }
}
