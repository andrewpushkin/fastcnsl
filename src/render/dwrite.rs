//! Windows: glyphs rasterised by DirectWrite, so text looks like it does in
//! other Windows apps (Windows Terminal among them): grayscale, symmetric
//! anti-aliasing. cosmic-text still picks the font and
//! glyph; DirectWrite only draws it. Colour glyphs (emoji) and fonts
//! DirectWrite doesn't know stay with swash.
// DirectWrite is a COM API: every call is unsafe. They are all plain
// calls on interfaces this module owns, with buffers sized as asked.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::mem::ManuallyDrop;

use cosmic_text::{CacheKey, FontSystem};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN, DWRITE_GRID_FIT_MODE_DEFAULT,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC, DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE,
    DWRITE_TEXTURE_CLEARTYPE_3x1, DWriteCreateFactory, IDWriteFactory2, IDWriteFontCollection, IDWriteFontFace,
};
use windows::core::{BOOL, HSTRING};

/// A rasterised glyph: an 8-bit coverage mask placed relative to the pen
/// position on the baseline (`top` is upwards).
pub struct Mask {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

struct Api {
    factory: IDWriteFactory2,
    fonts: IDWriteFontCollection,
}

#[derive(Default)]
pub struct Rasterizer {
    /// None until first use; Some(None) if DirectWrite is unavailable.
    api: Option<Option<Api>>,
    /// DirectWrite face per (font, weight, italic); None when DirectWrite
    /// has no matching font.
    faces: HashMap<(cosmic_text::fontdb::ID, u16, bool), Option<IDWriteFontFace>>,
}

impl Rasterizer {
    /// Draws the glyph `key` (shaped from the single character `c`), or
    /// returns None to leave it to swash.
    pub fn rasterize(&mut self, fs: &mut FontSystem, key: CacheKey, c: char, italic: bool) -> Option<Mask> {
        if self.api.is_none() {
            self.api = Some(Api::new().map_err(|e| log::warn!("DirectWrite unavailable: {e}")).ok());
        }
        let api = self.api.as_ref()?.as_ref()?;
        let face_key = (key.font_id, key.font_weight.0, italic);
        let face = self.faces.entry(face_key).or_insert_with(|| load_face(api, fs, key, italic)).as_ref()?;
        // Same font file? Then the character maps to the glyph cosmic-text chose.
        let mut glyph = 0u16;
        unsafe { face.GetGlyphIndices(&(c as u32), 1, &mut glyph) }.ok()?;
        if glyph != key.glyph_id {
            return None;
        }
        draw(api, face, glyph, f32::from_bits(key.font_size_bits))
    }
}

impl Api {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let factory: IDWriteFactory2 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut fonts = None;
            factory.GetSystemFontCollection(&mut fonts, false)?;
            Ok(Self { fonts: fonts.ok_or_else(windows::core::Error::empty)?, factory })
        }
    }
}

fn load_face(api: &Api, fs: &mut FontSystem, key: CacheKey, italic: bool) -> Option<IDWriteFontFace> {
    // Emoji and other colour fonts: DirectWrite would draw them as plain
    // outlines.
    let font = fs.get_font(key.font_id, key.font_weight)?;
    let swash = font.as_swash();
    if [*b"COLR", *b"CBDT", *b"sbix", *b"SVG "].iter().any(|t| swash.table(u32::from_be_bytes(*t)).is_some()) {
        return None;
    }
    let family = fs.db().face(key.font_id)?.families.first().map(|(name, _)| name.clone())?;
    let style = if italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL };
    let face = unsafe {
        let (mut index, mut exists) = (0u32, BOOL(0));
        api.fonts.FindFamilyName(&HSTRING::from(family.as_str()), &mut index, &mut exists).ok()?;
        if !exists.as_bool() {
            return None;
        }
        let font = api
            .fonts
            .GetFontFamily(index)
            .ok()?
            .GetFirstMatchingFont(DWRITE_FONT_WEIGHT(key.font_weight.0 as i32), DWRITE_FONT_STRETCH_NORMAL, style)
            .ok()?;
        font.CreateFontFace().ok()?
    };
    log::info!("DirectWrite draws '{family}' weight {}", key.font_weight.0);
    Some(face)
}

fn draw(api: &Api, face: &IDWriteFontFace, glyph: u16, size: f32) -> Option<Mask> {
    let advance = 0.0f32;
    let offset = DWRITE_GLYPH_OFFSET::default();
    let run = ManuallyDrop::new(DWRITE_GLYPH_RUN {
        fontFace: ManuallyDrop::new(Some(face.clone())),
        fontEmSize: size,
        glyphCount: 1,
        glyphIndices: &glyph,
        glyphAdvances: &advance,
        glyphOffsets: &offset,
        isSideways: BOOL(0),
        bidiLevel: 0,
    });
    let result = unsafe { draw_run(api, &run) };
    // The run holds a reference to the face; give it back.
    drop(ManuallyDrop::into_inner(ManuallyDrop::into_inner(run).fontFace));
    result
}

unsafe fn draw_run(api: &Api, run: &DWRITE_GLYPH_RUN) -> Option<Mask> {
    unsafe {
        // Symmetric anti-aliasing (smooth in x, grid-fitted in y) at
        // ClearType's 3x horizontal resolution, averaged to grayscale.
        // Side by side with Windows Terminal this matches it closely;
        // DirectWrite's own grayscale mode gives only 16 coverage levels
        // and looks harder.
        let analysis = api
            .factory
            .CreateGlyphRunAnalysis(
                run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DEFAULT,
                DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE,
                0.0,
                0.0,
            )
            .ok()?;
        let rect = analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_CLEARTYPE_3x1).ok()?;
        let (width, height) = ((rect.right - rect.left).max(0) as u32, (rect.bottom - rect.top).max(0) as u32);
        if width == 0 || height == 0 {
            return Some(Mask { left: 0, top: 0, width: 0, height: 0, data: Vec::new() });
        }
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        analysis.CreateAlphaTexture(DWRITE_TEXTURE_CLEARTYPE_3x1, &rect, &mut rgb).ok()?;
        let data = rgb.chunks_exact(3).map(|p| ((p[0] as u16 + p[1] as u16 + p[2] as u16) / 3) as u8).collect();
        Some(Mask { left: rect.left, top: -rect.top, width, height, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, SwashCache};

    /// DirectWrite draws a glyph about where and as big as swash does.
    #[test]
    fn directwrite_matches_swash_placement() {
        let mut fs = FontSystem::new();
        let Some(family) = ["Cascadia Mono", "Consolas", "Courier New", "Tahoma", "DejaVu Sans Mono"]
            .into_iter()
            .find(|f| fs.db().faces().any(|face| face.families.iter().any(|(n, _)| n == f)))
        else {
            eprintln!("no known font installed; skipping");
            return;
        };
        let mut buf = Buffer::new(&mut fs, Metrics::new(20.0, 24.0));
        let mut swash = SwashCache::new();
        let mut dw = Rasterizer::default();
        let mut drawn = 0;
        for c in ['A', 'g', 'W', '|'] {
            buf.set_text(&c.to_string(), &Attrs::new().family(Family::Name(family)), Shaping::Advanced, None);
            buf.shape_until_scroll(&mut fs, false);
            let key = buf.layout_runs().next().unwrap().glyphs[0].physical((0.0, 0.0), 1.0).cache_key;
            let Some(m) = dw.rasterize(&mut fs, key, c, false) else { continue };
            let s = swash.get_image_uncached(&mut fs, key).unwrap().placement;
            eprintln!("{family} {c}: dwrite {}x{} at ({}, {}), swash {}x{} at ({}, {})", m.width, m.height, m.left, m.top, s.width, s.height, s.left, s.top);
            assert_eq!(m.data.len(), (m.width * m.height) as usize);
            assert!(m.data.iter().any(|&a| a > 200), "{c} has solid pixels");
            assert!(m.data.iter().any(|&a| a > 30 && a < 225), "{c} is anti-aliased");
            assert!((m.top - s.top).abs() <= 2 && (m.left - s.left).abs() <= 2, "{c} placed like swash");
            assert!((m.width as i32 - s.width as i32).abs() <= 3 && (m.height as i32 - s.height as i32).abs() <= 3, "{c} sized like swash");
            drawn += 1;
        }
        assert!(drawn > 0, "DirectWrite drew none of the glyphs in {family}");

        // Bold (a named instance of the variable Cascadia fonts) is heavier.
        let mut ink = |fs: &mut FontSystem, dw: &mut Rasterizer, weight: cosmic_text::Weight| {
            let attrs = Attrs::new().family(Family::Name(family)).weight(weight);
            buf.set_text("H", &attrs, Shaping::Advanced, None);
            buf.shape_until_scroll(fs, false);
            let key = buf.layout_runs().next().unwrap().glyphs[0].physical((0.0, 0.0), 1.0).cache_key;
            dw.rasterize(fs, key, 'H', false).map(|m| m.data.iter().map(|&a| a as u32).sum::<u32>())
        };
        if let (Some(regular), Some(bold)) = (ink(&mut fs, &mut dw, cosmic_text::Weight::NORMAL), ink(&mut fs, &mut dw, cosmic_text::Weight::BOLD)) {
            eprintln!("{family} H ink: regular {regular}, bold {bold}");
            assert!(bold > regular * 11 / 10, "bold is heavier");
        }
    }
}
