//! Glyphs rasterised by FreeType, the way wezterm draws them on every
//! platform: the font's own hinting at normal DPI (none on high-DPI
//! screens), grayscale or LCD coverage, synthetic bold and italic where the
//! font has no such face. cosmic-text still picks the font and the glyph;
//! FreeType only draws it. Colour glyphs (emoji) stay with swash.
// FreeType is a C API: every call is unsafe. They are all plain calls on
// the library and faces this module owns, which live as long as it does.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::{CacheKey, CacheKeyFlags, FontSystem, fontdb};
use freetype_sys_nopng as ft;

/// A rasterised glyph placed relative to the pen position on the baseline
/// (`top` is upwards): 8-bit coverage, or with `subpixel` RGBA where RGB is
/// the coverage of each sub-pixel (A their maximum).
pub struct Mask {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub subpixel: bool,
    pub data: Vec<u8>,
}

/// Horizontal skew of synthetic italics, as in wezterm.
const ITALIC_SKEW: f64 = 0.2;

/// 16.16 fixed point.
fn fixed(v: f64) -> ft::FT_Fixed {
    (v * 65536.0).round() as ft::FT_Fixed
}

struct Face {
    face: ft::FT_Face,
    /// The font bytes FreeType reads from; kept alive with the face.
    _font: Arc<cosmic_text::Font>,
    /// `font_size_bits` of the size last set on the face.
    size_bits: u32,
    /// Bold was asked for and the font only has a regular face.
    embolden: bool,
}

pub struct Rasterizer {
    /// Null if FreeType failed to start; swash then draws everything.
    lib: ft::FT_Library,
    /// The screen's sub-pixels run blue-green-red.
    bgr: bool,
    /// FreeType face per (font, requested weight); None when FreeType
    /// can't or shouldn't draw the font.
    faces: HashMap<(fontdb::ID, u16), Option<Face>>,
}

impl Rasterizer {
    pub fn new(bgr: bool) -> Self {
        let mut lib = std::ptr::null_mut();
        unsafe {
            if ft::FT_Init_FreeType(&mut lib) != 0 {
                log::warn!("FreeType failed to start; glyphs drawn by swash");
                lib = std::ptr::null_mut();
            } else {
                // wezterm's choice; FreeType has no filter by default and
                // LCD text would show colour fringes.
                ft::FT_Library_SetLcdFilter(lib, ft::FT_LCD_FILTER_DEFAULT);
            }
        }
        Self { lib, bgr, faces: HashMap::new() }
    }

    /// Draws the glyph `key`, or returns None to leave it to swash.
    /// `hinted`: apply the font's hinting (wezterm's default below 100 DPI).
    pub fn rasterize(&mut self, fs: &mut FontSystem, key: CacheKey, hinted: bool, subpixel: bool) -> Option<Mask> {
        if self.lib.is_null() {
            return None;
        }
        let lib = self.lib;
        let face = self
            .faces
            .entry((key.font_id, key.font_weight.0))
            .or_insert_with(|| unsafe { load_face(lib, fs, key) })
            .as_mut()?;
        unsafe {
            let f = face.face;
            if face.size_bits != key.font_size_bits {
                let px = f32::from_bits(key.font_size_bits) as f64;
                // At 72 DPI a point is a pixel.
                let size = (px * 64.0).round() as ft::FT_F26Dot6;
                if ft::FT_Set_Char_Size(f, 0, size, 72, 72) != 0 {
                    return None;
                }
                face.size_bits = key.font_size_bits;
            }
            let mut skew = ft::FT_Matrix { xx: fixed(1.0), xy: fixed(ITALIC_SKEW), yx: 0, yy: fixed(1.0) };
            let italic = key.flags.contains(CacheKeyFlags::FAKE_ITALIC);
            ft::FT_Set_Transform(f, if italic { &mut skew } else { std::ptr::null_mut() }, std::ptr::null_mut());

            let hinting = if hinted { ft::FT_LOAD_DEFAULT } else { ft::FT_LOAD_NO_HINTING };
            let flags = hinting | ft::FT_LOAD_TARGET_NORMAL | ft::FT_LOAD_COLOR;
            if ft::FT_Load_Glyph(f, key.glyph_id as ft::FT_UInt, flags) != 0 {
                return None;
            }
            let slot = (*f).glyph;
            if (*slot).format != ft::FT_GLYPH_FORMAT_OUTLINE {
                // Bitmap strikes: swash picks the best one and scales it.
                return None;
            }
            if face.embolden {
                ft::FT_GlyphSlot_Embolden(slot);
            }
            let mode = if subpixel { ft::FT_RENDER_MODE_LCD } else { ft::FT_RENDER_MODE_NORMAL };
            if ft::FT_Render_Glyph(slot, mode) != 0 {
                return None;
            }
            convert(&*slot, self.bgr)
        }
    }
}

impl Drop for Rasterizer {
    fn drop(&mut self) {
        unsafe {
            for face in self.faces.drain().filter_map(|(_, f)| f) {
                ft::FT_Done_Face(face.face);
            }
            if !self.lib.is_null() {
                ft::FT_Done_FreeType(self.lib);
            }
        }
    }
}

unsafe fn load_face(lib: ft::FT_Library, fs: &mut FontSystem, key: CacheKey) -> Option<Face> {
    let font = fs.get_font(key.font_id, key.font_weight)?;
    // Emoji and other colour fonts: without its colour renderers FreeType
    // would draw them as plain outlines, if at all.
    let swash = font.as_swash();
    if [*b"COLR", *b"CBDT", *b"sbix", *b"SVG "].iter().any(|t| swash.table(u32::from_be_bytes(*t)).is_some()) {
        return None;
    }
    let info = fs.db().face(key.font_id)?;
    let (index, face_weight) = (info.index, info.weight.0);
    let family = info.families.first().map(|(n, _)| n.clone()).unwrap_or_default();
    let data = font.data();
    let mut face = std::ptr::null_mut();
    unsafe {
        if ft::FT_New_Memory_Face(lib, data.as_ptr(), data.len() as ft::FT_Long, index as ft::FT_Long, &mut face) != 0 {
            log::warn!("FreeType can't open '{family}'; drawn by swash");
            return None;
        }
        if (*face).face_flags & ft::FT_FACE_FLAG_SCALABLE == 0 {
            ft::FT_Done_Face(face);
            return None;
        }
        let variable = (*face).face_flags & ft::FT_FACE_FLAG_MULTIPLE_MASTERS != 0 && set_weight(lib, face, key.font_weight.0);
        // wezterm emboldens when asked for at least demibold and the font
        // it found is regular or lighter.
        let embolden = !variable && key.font_weight.0 >= 600 && face_weight <= 400;
        log::info!(
            "FreeType draws '{family}' weight {}{}",
            key.font_weight.0,
            if embolden { " (synthetic bold)" } else { "" }
        );
        Some(Face { face, _font: font, size_bits: 0, embolden })
    }
}

/// Sets a variable font's weight axis (others at their defaults); false
/// if the font has no weight axis.
unsafe fn set_weight(lib: ft::FT_Library, face: ft::FT_Face, weight: u16) -> bool {
    unsafe {
        let mut mm = std::ptr::null_mut();
        if ft::FT_Get_MM_Var(face, &mut mm) != 0 {
            return false;
        }
        let axes = std::slice::from_raw_parts((*mm).axis, (*mm).num_axis as usize);
        let wght = u32::from_be_bytes(*b"wght") as ft::FT_ULong;
        let has_weight = axes.iter().any(|a| a.tag == wght);
        let mut coords: Vec<ft::FT_Fixed> = axes
            .iter()
            .map(|a| if a.tag == wght { fixed(weight as f64).clamp(a.minimum, a.maximum) } else { a.def })
            .collect();
        if has_weight {
            ft::FT_Set_Var_Design_Coordinates(face, coords.len() as ft::FT_UInt, coords.as_mut_ptr());
        }
        ft::FT_Done_MM_Var(lib, mm);
        has_weight
    }
}

/// The rendered bitmap as coverage (one byte per pixel) or, from LCD
/// rendering, RGBA sub-pixel coverage.
unsafe fn convert(slot: &ft::FT_GlyphSlotRec, bgr: bool) -> Option<Mask> {
    let b = &slot.bitmap;
    let (rows, pitch) = (b.rows.max(0) as usize, b.pitch.unsigned_abs() as usize);
    let src: &[u8] = if b.buffer.is_null() || rows == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(b.buffer, rows * pitch) }
    };
    let row = |y: usize| &src[y * pitch..(y + 1) * pitch];
    let mode = b.pixel_mode as u8 as ft::FT_Pixel_Mode;
    let (width, subpixel, data): (usize, bool, Vec<u8>) = match mode {
        ft::FT_PIXEL_MODE_GRAY => {
            let w = b.width as usize;
            (w, false, (0..rows).flat_map(|y| row(y)[..w].iter().copied()).collect())
        }
        ft::FT_PIXEL_MODE_MONO => {
            let w = b.width as usize;
            let data = (0..rows)
                .flat_map(|y| (0..w).map(move |x| if row(y)[x / 8] & (0x80 >> (x % 8)) != 0 { 255 } else { 0 }))
                .collect();
            (w, false, data)
        }
        ft::FT_PIXEL_MODE_LCD => {
            let w = b.width as usize / 3;
            let data = (0..rows)
                .flat_map(|y| {
                    row(y)[..w * 3].chunks_exact(3).flat_map(|p| {
                        let (r, g, b) = if bgr { (p[2], p[1], p[0]) } else { (p[0], p[1], p[2]) };
                        [r, g, b, r.max(g).max(b)]
                    })
                })
                .collect();
            (w, true, data)
        }
        _ => return None,
    };
    Some(Mask {
        left: slot.bitmap_left,
        top: slot.bitmap_top,
        width: width as u32,
        height: rows as u32,
        subpixel,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, SwashCache, Weight};

    fn key(fs: &mut FontSystem, buf: &mut Buffer, c: char, weight: Weight) -> CacheKey {
        let attrs = Attrs::new().family(Family::Name("DejaVu Sans Mono")).weight(weight);
        buf.set_text(&c.to_string(), &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(fs, false);
        buf.layout_runs().next().unwrap().glyphs[0].physical((0.0, 0.0), 1.0).cache_key
    }

    fn installed(fs: &FontSystem) -> bool {
        let ok = fs.db().faces().any(|f| f.families.iter().any(|(n, _)| n == "DejaVu Sans Mono"));
        if !ok {
            eprintln!("DejaVu Sans Mono not installed; skipping");
        }
        ok
    }

    /// FreeType draws a glyph about where and as big as swash does.
    #[test]
    fn matches_swash_placement() {
        let mut fs = FontSystem::new();
        if !installed(&fs) {
            return;
        }
        let mut buf = Buffer::new(&mut fs, Metrics::new(16.0, 20.0));
        let mut swash = SwashCache::new();
        let mut ft = Rasterizer::new(false);
        for c in ['A', 'g', 'W', '|', 'Ж'] {
            let k = key(&mut fs, &mut buf, c, Weight::NORMAL);
            for hinted in [true, false] {
                let m = ft.rasterize(&mut fs, k, hinted, false).expect("FreeType draws it");
                let s = swash.get_image_uncached(&mut fs, k).unwrap().placement;
                eprintln!("{c} hinted={hinted}: ft {}x{} at ({}, {}), swash {}x{} at ({}, {})", m.width, m.height, m.left, m.top, s.width, s.height, s.left, s.top);
                assert_eq!(m.data.len(), (m.width * m.height) as usize);
                assert!(m.data.iter().any(|&a| a > 200), "{c} has solid pixels");
                assert!((m.top - s.top).abs() <= 2 && (m.left - s.left).abs() <= 2, "{c} placed like swash");
                assert!((m.width as i32 - s.width as i32).abs() <= 3 && (m.height as i32 - s.height as i32).abs() <= 3, "{c} sized like swash");
            }
        }
    }

    #[test]
    fn lcd_rendering_gives_per_channel_coverage() {
        let mut fs = FontSystem::new();
        if !installed(&fs) {
            return;
        }
        let mut buf = Buffer::new(&mut fs, Metrics::new(16.0, 20.0));
        let mut ft = Rasterizer::new(false);
        let k = key(&mut fs, &mut buf, 'W', Weight::NORMAL);
        let m = ft.rasterize(&mut fs, k, true, true).unwrap();
        assert!(m.subpixel);
        assert_eq!(m.data.len(), (m.width * m.height * 4) as usize);
        let fringed = m.data.chunks_exact(4).filter(|p| p[0] != p[2]).count();
        assert!(fringed > 0, "sub-pixels differ at the edges");
        let mut bgr = Rasterizer::new(true);
        let swapped = bgr.rasterize(&mut fs, k, true, true).unwrap();
        assert_eq!((swapped.data[0], swapped.data[2]), (m.data[2], m.data[0]));
    }

    #[test]
    fn bold_is_heavier_and_size_changes_apply() {
        let mut fs = FontSystem::new();
        if !installed(&fs) {
            return;
        }
        let mut buf = Buffer::new(&mut fs, Metrics::new(16.0, 20.0));
        let mut ft = Rasterizer::new(false);
        let ink = |m: &Mask| m.data.iter().map(|&a| a as u32).sum::<u32>();
        let regular = key(&mut fs, &mut buf, 'H', Weight::NORMAL);
        let bold = key(&mut fs, &mut buf, 'H', Weight::BOLD);
        let (r, b) = (ft.rasterize(&mut fs, regular, true, false).unwrap(), ft.rasterize(&mut fs, bold, true, false).unwrap());
        assert!(ink(&b) > ink(&r) * 11 / 10, "bold {} vs regular {}", ink(&b), ink(&r));
        buf.set_metrics(Metrics::new(32.0, 40.0));
        let big = key(&mut fs, &mut buf, 'H', Weight::NORMAL);
        let big = ft.rasterize(&mut fs, big, true, false).unwrap();
        assert!(big.height > r.height * 3 / 2, "{} vs {}", big.height, r.height);
    }
}
