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

/// Best-effort OS dark-mode detection for platforms where the windowing
/// system does not report it (X11, some Wayland compositors, WSLg).
pub fn os_prefers_dark_fallback() -> Option<bool> {
    if let Ok(gtk) = std::env::var("GTK_THEME") {
        return Some(gtk.to_ascii_lowercase().contains("dark"));
    }
    #[cfg(target_os = "linux")]
    {
        let out = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
            .ok()?;
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if s.contains("dark") {
                return Some(true);
            }
            if s.contains("light") || s.contains("default") {
                return Some(false);
            }
        }
    }
    None
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
}
