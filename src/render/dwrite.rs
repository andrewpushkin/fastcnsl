//! Windows: glyphs rasterised by DirectWrite, so text looks like it does in
//! other Windows apps (Windows Terminal among them). cosmic-text still
//! picks the font and glyph; DirectWrite only draws it. Colour glyphs
//! (emoji) and fonts DirectWrite doesn't know stay with swash.
// The one unsafe call hands DirectWrite the font face pointer.
#![allow(unsafe_code)]

use std::collections::HashMap;

use cosmic_text::{CacheKey, FontSystem};
use dwrote::{
    DWRITE_GLYPH_RUN, DWRITE_MEASURING_MODE_NATURAL, DWRITE_RENDERING_MODE_ALIASED, DWRITE_RENDERING_MODE_DEFAULT,
    DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC, DWRITE_RENDERING_MODE_OUTLINE, DWRITE_TEXTURE_ALIASED_1x1,
    DWRITE_TEXTURE_CLEARTYPE_3x1, FontCollection, FontFace, FontStretch, FontStyle, FontWeight, GlyphOffset,
    GlyphRunAnalysis,
};

/// A rasterised glyph: an 8-bit coverage mask placed relative to the pen
/// position on the baseline (`top` is upwards).
pub struct Mask {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct Rasterizer {
    collection: Option<FontCollection>,
    /// DirectWrite face per (font, weight, italic); None when DirectWrite
    /// has no matching font.
    faces: HashMap<(cosmic_text::fontdb::ID, u16, bool), Option<FontFace>>,
}

impl Rasterizer {
    /// Draws the glyph `key` (shaped from the single character `c`), or
    /// returns None to leave it to swash.
    pub fn rasterize(&mut self, fs: &mut FontSystem, key: CacheKey, c: char, italic: bool) -> Option<Mask> {
        let face_key = (key.font_id, key.font_weight.0, italic);
        if !self.faces.contains_key(&face_key) {
            let face = self.load_face(fs, key, italic);
            self.faces.insert(face_key, face);
        }
        let face = self.faces.get(&face_key)?.as_ref()?;
        // Same font file? Then the character maps to the glyph cosmic-text chose.
        if face.glyph_indices(&[c as u32]).ok()?.first() != Some(&key.glyph_id) {
            return None;
        }
        draw(face, key.glyph_id, f32::from_bits(key.font_size_bits))
    }

    fn load_face(&mut self, fs: &mut FontSystem, key: CacheKey, italic: bool) -> Option<FontFace> {
        // Emoji and other colour fonts: DirectWrite would draw them as
        // plain outlines.
        let font = fs.get_font(key.font_id, key.font_weight)?;
        let swash = font.as_swash();
        if [*b"COLR", *b"CBDT", *b"sbix", *b"SVG "].iter().any(|t| swash.table(u32::from_be_bytes(*t)).is_some()) {
            return None;
        }
        let info = fs.db().face(key.font_id)?;
        let family = info.families.first().map(|(name, _)| name.clone())?;
        let collection = self.collection.get_or_insert_with(FontCollection::system);
        let found = collection.font_family_by_name(&family).ok()??;
        let style = if italic { FontStyle::Italic } else { FontStyle::Normal };
        let dw_font = found
            .first_matching_font(FontWeight::Unknown(key.font_weight.0 as u32), FontStretch::Normal, style)
            .ok()?;
        log::info!("DirectWrite draws '{family}' weight {}", key.font_weight.0);
        Some(dw_font.create_font_face())
    }
}

fn draw(face: &FontFace, glyph: u16, size: f32) -> Option<Mask> {
    let indices = [glyph];
    let advances = [0.0f32];
    let offsets = [GlyphOffset { advanceOffset: 0.0, ascenderOffset: 0.0 }];
    let run = DWRITE_GLYPH_RUN {
        // SAFETY: `face` outlives `run` and the analysis made from it, and
        // DirectWrite only reads through the pointer.
        fontFace: unsafe { face.as_ptr() },
        fontEmSize: size,
        glyphCount: 1,
        glyphIndices: indices.as_ptr(),
        glyphAdvances: advances.as_ptr(),
        glyphOffsets: offsets.as_ptr(),
        isSideways: 0,
        bidiLevel: 0,
    };
    // The mode Windows recommends for this font and size (from its gasp
    // table and the system's text settings), as DirectWrite apps use.
    let mode = match face.get_recommended_rendering_mode_default_params(size, 1.0, DWRITE_MEASURING_MODE_NATURAL) {
        DWRITE_RENDERING_MODE_DEFAULT | DWRITE_RENDERING_MODE_OUTLINE => DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
        m => m,
    };
    let aliased = mode == DWRITE_RENDERING_MODE_ALIASED;
    let texture = if aliased { DWRITE_TEXTURE_ALIASED_1x1 } else { DWRITE_TEXTURE_CLEARTYPE_3x1 };
    let analysis = GlyphRunAnalysis::create(&run, 1.0, None, mode, DWRITE_MEASURING_MODE_NATURAL, 0.0, 0.0).ok()?;
    let rect = analysis.get_alpha_texture_bounds(texture).ok()?;
    let (width, height) = ((rect.right - rect.left).max(0) as u32, (rect.bottom - rect.top).max(0) as u32);
    if width == 0 || height == 0 {
        return Some(Mask { left: 0, top: 0, width: 0, height: 0, data: Vec::new() });
    }
    let raw = analysis.create_alpha_texture(texture, rect).ok()?;
    // ClearType 3x1 has a value per colour channel; their mean is the
    // grayscale coverage.
    let data = if aliased {
        raw
    } else {
        raw.chunks_exact(3).map(|p| ((p[0] as u16 + p[1] as u16 + p[2] as u16) / 3) as u8).collect()
    };
    Some(Mask { left: rect.left, top: -rect.top, width, height, data })
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
