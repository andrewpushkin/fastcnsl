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
    DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE, DWRITE_TEXTURE_ALIASED_1x1, DWRITE_TEXTURE_CLEARTYPE_3x1, DWriteCreateFactory, IDWriteFactory2, IDWriteFontCollection, IDWriteFontFace,
};
use windows::core::{BOOL, HSTRING};

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

/// Whether Windows draws text with ClearType (the "Adjust ClearType text"
/// setting), and whether the screen's sub-pixels run blue-green-red.
pub fn system_cleartype() -> Option<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETFONTSMOOTHING, SPI_GETFONTSMOOTHINGORIENTATION, SPI_GETFONTSMOOTHINGTYPE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        SystemParametersInfoW,
    };
    let get = |action| {
        let mut v = 0u32;
        // SAFETY: each of these queries writes one UINT into `v`.
        unsafe { SystemParametersInfoW(action, 0, Some(&mut v as *mut u32 as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) }
            .ok()
            .map(|()| v)
    };
    const FE_FONTSMOOTHINGCLEARTYPE: u32 = 2;
    const FE_FONTSMOOTHINGORIENTATIONBGR: u32 = 0;
    let on = get(SPI_GETFONTSMOOTHING)? != 0 && get(SPI_GETFONTSMOOTHINGTYPE)? == FE_FONTSMOOTHINGCLEARTYPE;
    on.then(|| get(SPI_GETFONTSMOOTHINGORIENTATION) == Some(FE_FONTSMOOTHINGORIENTATIONBGR))
}

struct Api {
    factory: IDWriteFactory2,
    fonts: IDWriteFontCollection,
}

#[derive(Default)]
pub struct Rasterizer {
    /// None until first use; Some(None) if DirectWrite is unavailable.
    api: Option<Option<Api>>,
    /// The screen's sub-pixels run blue-green-red (read on first use).
    bgr: bool,
    /// DirectWrite face per (font, weight, italic); None when DirectWrite
    /// has no matching font.
    faces: HashMap<(cosmic_text::fontdb::ID, u16, bool), Option<IDWriteFontFace>>,
}

impl Rasterizer {
    /// Draws the glyph `key` (shaped from the single character `c`), or
    /// returns None to leave it to swash.
    pub fn rasterize(&mut self, fs: &mut FontSystem, key: CacheKey, c: char, italic: bool, subpixel: bool) -> Option<Mask> {
        if self.api.is_none() {
            self.api = Some(Api::new().map_err(|e| log::warn!("DirectWrite unavailable: {e}")).ok());
            self.bgr = system_cleartype() == Some(true);
        }
        let bgr = self.bgr;
        let api = self.api.as_ref()?.as_ref()?;
        let face_key = (key.font_id, key.font_weight.0, italic);
        let face = self.faces.entry(face_key).or_insert_with(|| load_face(api, fs, key, italic)).as_ref()?;
        // Same font file? Then the character maps to the glyph cosmic-text chose.
        let mut glyph = 0u16;
        unsafe { face.GetGlyphIndices(&(c as u32), 1, &mut glyph) }.ok()?;
        if glyph != key.glyph_id {
            return None;
        }
        draw(api, face, glyph, f32::from_bits(key.font_size_bits), subpixel, bgr)
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

fn draw(api: &Api, face: &IDWriteFontFace, glyph: u16, size: f32, subpixel: bool, bgr: bool) -> Option<Mask> {
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
    let result = unsafe { draw_run(api, &run, subpixel, bgr) };
    // The run holds a reference to the face; give it back.
    drop(ManuallyDrop::into_inner(ManuallyDrop::into_inner(run).fontFace));
    result
}

unsafe fn draw_run(api: &Api, run: &DWRITE_GLYPH_RUN, subpixel: bool, bgr: bool) -> Option<Mask> {
    unsafe {
        // Symmetric anti-aliasing (smooth in x, grid-fitted in y). ClearType
        // keeps the coverage of each sub-pixel, as native Windows apps
        // draw text. Grayscale: measured against Windows Terminal (its
        // default mode) this gives the same share of solid and edge pixels;
        // averaging ClearType's channels instead leaves a halo.
        let (mode, texture) = if subpixel {
            (DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE, DWRITE_TEXTURE_CLEARTYPE_3x1)
        } else {
            (DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE, DWRITE_TEXTURE_ALIASED_1x1)
        };
        let analysis = api
            .factory
            .CreateGlyphRunAnalysis(
                run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DEFAULT,
                mode,
                0.0,
                0.0,
            )
            .ok()?;
        let rect = analysis.GetAlphaTextureBounds(texture).ok()?;
        let (width, height) = ((rect.right - rect.left).max(0) as u32, (rect.bottom - rect.top).max(0) as u32);
        if width == 0 || height == 0 {
            return Some(Mask { left: 0, top: 0, width: 0, height: 0, subpixel, data: Vec::new() });
        }
        let per = if subpixel { 3 } else { 1 };
        let mut raw = vec![0u8; (width * height * per) as usize];
        analysis.CreateAlphaTexture(texture, &rect, &mut raw).ok()?;
        // With grayscale anti-aliasing the 1x1 texture holds coverage
        // (0-255), not just on/off.
        let data = if subpixel {
            raw.chunks_exact(3)
                .flat_map(|p| {
                    let (r, b) = if bgr { (p[2], p[0]) } else { (p[0], p[2]) };
                    [r, p[1], b, r.max(p[1]).max(b)]
                })
                .collect()
        } else {
            raw
        };
        Some(Mask { left: rect.left, top: -rect.top, width, height, subpixel, data })
    }
}

/// The system's text settings that DirectWrite applies when it blends
/// text: (gamma, grayscale enhanced contrast). Only checked by a test: the
/// shader's correction was fitted to Windows Terminal's output instead.
#[cfg(test)]
fn text_settings() -> Option<(f32, f32)> {
    use windows::Win32::Graphics::DirectWrite::IDWriteRenderingParams1;
    use windows::core::Interface;
    unsafe {
        let factory: IDWriteFactory2 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
        let params = factory.CreateRenderingParams().ok()?;
        let contrast = params.cast::<IDWriteRenderingParams1>().map(|p| p.GetGrayscaleEnhancedContrast()).unwrap_or(1.0);
        Some((params.GetGamma(), contrast))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, SwashCache};

    /// Ground truth for tuning the shader: draws a line of Cascadia Mono
    /// the way native Windows apps do (DirectWrite into a GDI bitmap with
    /// the system's text settings, ClearType included), in the given
    /// colours, one glyph per 12px cell, and saves it as a PPM image.
    /// Runs only with RAGAMUFFIN_NATIVE_TEXT=<dir>.
    #[test]
    fn native_text_reference() {
        use windows::Win32::Foundation::{COLORREF, RECT};
        use windows::Win32::Graphics::Gdi::{
            BITMAP, CreateSolidBrush, DeleteObject, FillRect, GetCurrentObject, GetObjectW, OBJ_BITMAP,
        };
        let Some(dir) = std::env::var_os("RAGAMUFFIN_NATIVE_TEXT") else { return };
        let api = Api::new().unwrap();
        let text = "Windows PowerShell";
        let (w, h, cell, size, baseline) = (12 * text.len() as i32 + 8, 24, 12.0f32, 20.0f32, 18.0f32);
        let mut index = 0u32;
        let mut exists = BOOL(0);
        unsafe {
            api.fonts.FindFamilyName(&HSTRING::from("Cascadia Mono"), &mut index, &mut exists).unwrap();
            assert!(exists.as_bool(), "Cascadia Mono installed");
            let face = api
                .fonts
                .GetFontFamily(index)
                .unwrap()
                .GetFirstMatchingFont(DWRITE_FONT_WEIGHT(400), DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
                .unwrap()
                .CreateFontFace()
                .unwrap();
            let codes: Vec<u32> = text.chars().map(|c| c as u32).collect();
            let mut glyphs = vec![0u16; codes.len()];
            face.GetGlyphIndices(codes.as_ptr(), codes.len() as u32, glyphs.as_mut_ptr()).unwrap();
            let advances = vec![cell; glyphs.len()];
            let offsets = vec![DWRITE_GLYPH_OFFSET::default(); glyphs.len()];
            let interop = api.factory.GetGdiInterop().unwrap();
            let params = api.factory.CreateRenderingParams().unwrap();
            for (name, fg, bg) in [("dark", [216u8, 222, 233], [30u8, 33, 39]), ("light", [36, 41, 47], [250, 250, 250])] {
                let target = interop.CreateBitmapRenderTarget(None, w as u32, h as u32).unwrap();
                target.SetPixelsPerDip(1.0).unwrap();
                let dc = target.GetMemoryDC();
                let rgb = |c: [u8; 3]| COLORREF(c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16);
                let brush = CreateSolidBrush(rgb(bg));
                FillRect(dc, &RECT { left: 0, top: 0, right: w, bottom: h }, brush);
                let _ = DeleteObject(brush.into());
                let run = ManuallyDrop::new(DWRITE_GLYPH_RUN {
                    fontFace: ManuallyDrop::new(Some(face.clone())),
                    fontEmSize: size,
                    glyphCount: glyphs.len() as u32,
                    glyphIndices: glyphs.as_ptr(),
                    glyphAdvances: advances.as_ptr(),
                    glyphOffsets: offsets.as_ptr(),
                    isSideways: BOOL(0),
                    bidiLevel: 0,
                });
                target.DrawGlyphRun(4.0, baseline, DWRITE_MEASURING_MODE_NATURAL, &*run, &params, rgb(fg), None).unwrap();
                drop(ManuallyDrop::into_inner(ManuallyDrop::into_inner(run).fontFace));
                let mut bm = BITMAP::default();
                let bitmap = GetCurrentObject(dc, OBJ_BITMAP);
                assert!(GetObjectW(bitmap, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut _)) > 0);
                let stride = bm.bmWidthBytes as usize;
                let bits = std::slice::from_raw_parts(bm.bmBits as *const u8, stride * h as usize);
                let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
                for y in 0..h as usize {
                    for x in 0..w as usize {
                        let p = &bits[y * stride + x * 4..];
                        ppm.extend_from_slice(&[p[2], p[1], p[0]]);
                    }
                }
                let path = std::path::Path::new(&dir).join(format!("native_{name}.ppm"));
                std::fs::write(&path, ppm).unwrap();
                eprintln!("wrote {}", path.display());
            }
        }
    }

    #[test]
    fn reads_system_text_settings() {
        let (gamma, contrast) = text_settings().expect("DirectWrite rendering params");
        eprintln!("system text settings: gamma {gamma}, grayscale enhanced contrast {contrast}");
        assert!((1.0..=3.0).contains(&gamma));
        assert!((0.0..=10.0).contains(&contrast));
    }

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
            let Some(m) = dw.rasterize(&mut fs, key, c, false, false) else { continue };
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
            dw.rasterize(fs, key, 'H', false, false).map(|m| m.data.iter().map(|&a| a as u32).sum::<u32>())
        };
        if let (Some(regular), Some(bold)) = (ink(&mut fs, &mut dw, cosmic_text::Weight::NORMAL), ink(&mut fs, &mut dw, cosmic_text::Weight::BOLD)) {
            eprintln!("{family} H ink: regular {regular}, bold {bold}");
            assert!(bold > regular * 11 / 10, "bold is heavier");
        }
    }
}
