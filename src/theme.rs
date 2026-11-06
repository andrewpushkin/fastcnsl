//! Colour palettes for dark and light mode.

use crate::config::ThemeMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    /// Linear-ish RGBA for the GPU. The surface is sRGB-unaware (we pick a
    /// non-sRGB format), so values go through unchanged.
    pub fn rgba(self, a: f32) -> [f32; 4] {
        [self.r as f32 / 255.0, self.g as f32 / 255.0, self.b as f32 / 255.0, a]
    }

    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb { r: l(self.r, other.r), g: l(self.g, other.g), b: l(self.b, other.b) }
    }
}

#[derive(Debug, Clone)]
pub struct Palette {
    pub dark: bool,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection_bg: Rgb,
    /// All search matches (Notepad++ "mark" style).
    pub match_bg: Rgb,
    pub match_fg: Rgb,
    /// The match Enter/Shift+Enter is on.
    pub current_match_bg: Rgb,
    pub current_match_fg: Rgb,
    pub ui_bg: Rgb,
    pub ui_fg: Rgb,
    pub ui_accent: Rgb,
    pub ui_dim: Rgb,
    /// 16 ANSI colours: 0–7 normal, 8–15 bright.
    pub ansi: [Rgb; 16],
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            foreground: Rgb::hex(0xd8dee9),
            background: Rgb::hex(0x1e2127),
            cursor: Rgb::hex(0xe5e9f0),
            selection_bg: Rgb::hex(0x3e4f6a),
            match_bg: Rgb::hex(0x6b5a1e),
            match_fg: Rgb::hex(0xffffff),
            current_match_bg: Rgb::hex(0xe8a33d),
            current_match_fg: Rgb::hex(0x1e2127),
            ui_bg: Rgb::hex(0x2c313a),
            ui_fg: Rgb::hex(0xe5e9f0),
            ui_accent: Rgb::hex(0x61afef),
            ui_dim: Rgb::hex(0x7f8794),
            ansi: [
                Rgb::hex(0x282c34),
                Rgb::hex(0xe06c75),
                Rgb::hex(0x98c379),
                Rgb::hex(0xe5c07b),
                Rgb::hex(0x61afef),
                Rgb::hex(0xc678dd),
                Rgb::hex(0x56b6c2),
                Rgb::hex(0xabb2bf),
                Rgb::hex(0x5c6370),
                Rgb::hex(0xff7b86),
                Rgb::hex(0xb5e890),
                Rgb::hex(0xffd68a),
                Rgb::hex(0x82c4ff),
                Rgb::hex(0xde9bf0),
                Rgb::hex(0x7fd4df),
                Rgb::hex(0xffffff),
            ],
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            foreground: Rgb::hex(0x24292f),
            background: Rgb::hex(0xfafafa),
            cursor: Rgb::hex(0x24292f),
            selection_bg: Rgb::hex(0xb6d4fe),
            match_bg: Rgb::hex(0xfff3a3),
            match_fg: Rgb::hex(0x24292f),
            current_match_bg: Rgb::hex(0xff9632),
            current_match_fg: Rgb::hex(0x000000),
            ui_bg: Rgb::hex(0xe8eaed),
            ui_fg: Rgb::hex(0x24292f),
            ui_accent: Rgb::hex(0x0969da),
            ui_dim: Rgb::hex(0x8c959f),
            ansi: [
                Rgb::hex(0x24292f),
                Rgb::hex(0xcf222e),
                Rgb::hex(0x116329),
                Rgb::hex(0x9a6700),
                Rgb::hex(0x0550ae),
                Rgb::hex(0x8250df),
                Rgb::hex(0x1b7c83),
                Rgb::hex(0x6e7781),
                Rgb::hex(0x57606a),
                Rgb::hex(0xa40e26),
                Rgb::hex(0x1a7f37),
                Rgb::hex(0x633c01),
                Rgb::hex(0x218bff),
                Rgb::hex(0xa475f9),
                Rgb::hex(0x3192aa),
                Rgb::hex(0x8c959f),
            ],
        }
    }

    pub fn for_mode(mode: ThemeMode, os_dark: bool) -> Self {
        let dark = match mode {
            ThemeMode::System => os_dark,
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
        };
        if dark { Self::dark() } else { Self::light() }
    }

    /// xterm 256-colour palette: 16 named, 6x6x6 cube, 24 greys.
    pub fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb { r: step(i / 36), g: step((i / 6) % 6), b: step(i % 6) }
            }
            232..=255 => {
                let v = 8 + (i - 232) * 10;
                Rgb { r: v, g: v, b: v }
            }
        }
    }
}

/// Reads the OS light/dark setting where the windowing system doesn't
/// report it: Linux (X11, Wayland) and WSL. Windows and macOS report it
/// through winit, including live changes.
///
/// Order: `GTK_THEME` override, WSL (the Windows setting, via reg.exe), the
/// freedesktop appearance portal (GNOME, KDE, and others), gsettings, KDE's
/// kdeglobals.
pub fn detect_os_dark() -> Option<bool> {
    if let Ok(gtk) = std::env::var("GTK_THEME") {
        return Some(gtk.to_ascii_lowercase().contains("dark"));
    }
    #[cfg(target_os = "linux")]
    {
        linux::detect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// On Linux, polls the OS setting every 2 s on a background thread and
/// sends `UserEvent::OsTheme` when it changes. Elsewhere winit reports
/// changes itself, so this does nothing.
pub fn spawn_os_theme_watcher(proxy: winit::event_loop::EventLoopProxy<crate::session::UserEvent>, initial: Option<bool>) {
    #[cfg(target_os = "linux")]
    std::thread::Builder::new()
        .name("os-theme".into())
        .spawn(move || {
            let mut last = initial;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let now = detect_os_dark();
                if now.is_some() && now != last {
                    last = now;
                    if proxy.send_event(crate::session::UserEvent::OsTheme(now.unwrap_or(true))).is_err() {
                        return;
                    }
                }
            }
        })
        .ok();
    #[cfg(not(target_os = "linux"))]
    let _ = (proxy, initial);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::process::Command;

    pub fn detect() -> Option<bool> {
        if is_wsl()
            && let Some(d) = wsl_windows_dark()
        {
            return Some(d);
        }
        portal().or_else(gsettings).or_else(kde)
    }

    fn run(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn is_wsl() -> bool {
        std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::fs::read_to_string("/proc/version").is_ok_and(|v| v.to_ascii_lowercase().contains("microsoft"))
    }

    /// Windows "app mode": AppsUseLightTheme is 0 for dark, 1 for light.
    fn wsl_windows_dark() -> Option<bool> {
        let out = run(
            "/mnt/c/Windows/System32/reg.exe",
            &["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "/v", "AppsUseLightTheme"],
        )?;
        parse_reg_light_theme(&out).map(|light| !light)
    }

    pub(super) fn parse_reg_light_theme(out: &str) -> Option<bool> {
        let line = out.lines().find(|l| l.contains("AppsUseLightTheme"))?;
        let value = line.split_whitespace().last()?;
        let n = u32::from_str_radix(value.trim_start_matches("0x"), 16).ok()?;
        Some(n != 0)
    }

    /// org.freedesktop.appearance color-scheme: 1 dark, 2 light, 0 none.
    fn portal() -> Option<bool> {
        let out = run(
            "gdbus",
            &[
                "call", "--session", "--timeout", "1",
                "--dest", "org.freedesktop.portal.Desktop",
                "--object-path", "/org/freedesktop/portal/desktop",
                "--method", "org.freedesktop.portal.Settings.Read",
                "org.freedesktop.appearance", "color-scheme",
            ],
        )?;
        parse_portal(&out)
    }

    pub(super) fn parse_portal(out: &str) -> Option<bool> {
        let n: u32 = out.split("uint32").nth(1)?.trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
        match n {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    fn gsettings() -> Option<bool> {
        let scheme = run("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"])?;
        if scheme.contains("prefer-dark") {
            return Some(true);
        }
        if scheme.contains("prefer-light") {
            return Some(false);
        }
        let theme = run("gsettings", &["get", "org.gnome.desktop.interface", "gtk-theme"])?;
        Some(theme.to_ascii_lowercase().contains("dark"))
    }

    fn kde() -> Option<bool> {
        let path = dirs::config_dir()?.join("kdeglobals");
        let text = std::fs::read_to_string(path).ok()?;
        let scheme = text.lines().find_map(|l| l.strip_prefix("ColorScheme="))?;
        Some(scheme.to_ascii_lowercase().contains("dark"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_greys() {
        let p = Palette::dark();
        assert_eq!(p.indexed(16), Rgb::hex(0x000000));
        assert_eq!(p.indexed(231), Rgb::hex(0xffffff));
        assert_eq!(p.indexed(196), Rgb::hex(0xff0000));
        assert_eq!(p.indexed(232), Rgb::hex(0x080808));
        assert_eq!(p.indexed(255), Rgb::hex(0xeeeeee));
    }

    #[test]
    fn mode_picks_the_palette() {
        assert!(Palette::for_mode(ThemeMode::System, true).dark);
        assert!(!Palette::for_mode(ThemeMode::System, false).dark);
        assert!(Palette::for_mode(ThemeMode::Dark, false).dark);
        assert!(!Palette::for_mode(ThemeMode::Light, true).dark);
    }

    #[test]
    fn colour_helpers() {
        assert_eq!(Rgb::hex(0x123456), Rgb { r: 0x12, g: 0x34, b: 0x56 });
        assert_eq!(Rgb::hex(0xff0000).rgba(0.5), [1.0, 0.0, 0.0, 0.5]);
        let (a, b) = (Rgb::hex(0x000000), Rgb::hex(0xffffff));
        assert_eq!(a.mix(b, 0.0), a);
        assert_eq!(a.mix(b, 1.0), b);
        assert_eq!(a.mix(b, 0.5), Rgb::hex(0x808080));
    }

    #[test]
    fn palettes_have_readable_contrast() {
        // Text must differ clearly from the background in both themes.
        for p in [Palette::dark(), Palette::light()] {
            let lum = |c: Rgb| 0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32;
            assert!((lum(p.foreground) - lum(p.background)).abs() > 120.0);
            assert!((lum(p.current_match_fg) - lum(p.current_match_bg)).abs() > 60.0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_os_theme_sources() {
        assert_eq!(linux::parse_portal("(<<uint32 1>>,)\n"), Some(true));
        assert_eq!(linux::parse_portal("(<uint32 2>,)"), Some(false));
        assert_eq!(linux::parse_portal("(<<uint32 0>>,)"), None);
        let reg = "\r\nHKEY_CURRENT_USER\\Software\\...\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x0\r\n";
        assert_eq!(linux::parse_reg_light_theme(reg), Some(false));
        assert_eq!(linux::parse_reg_light_theme("AppsUseLightTheme REG_DWORD 0x1"), Some(true));
    }
}
