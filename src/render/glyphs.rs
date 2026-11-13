//! Glyph rasterisation and the GPU texture atlas.
//!
//! Each (character, bold, italic) is shaped once with cosmic-text (which
//! also handles font fallback for emoji, CJK, symbols...), rasterised with
//! FreeType as wezterm does (swash for colour glyphs) and packed into one
//! RGBA texture. After that, drawing a glyph is a
//! table lookup; ASCII skips the hash map entirely.

use std::collections::HashMap;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
};
use etagere::{BucketedAtlasAllocator, size2};

/// Side of the glyph atlas at first: 4 MB of RGBA, room for thousands
/// of glyphs at usual sizes. It doubles when full, up to `ATLAS_MAX`.
const ATLAS_START: u32 = 1024;
/// Largest atlas side: the most the WebGL2-level limits allow (16 MB).
const ATLAS_MAX: u32 = 2048;

/// The font database being loaded in the background (see `preload_fonts`).
static PRELOAD: Mutex<Option<JoinHandle<FontSystem>>> = Mutex::new(None);

/// Starts scanning the installed fonts on a background thread. Windows
/// has hundreds of them and the scan takes a noticeable part of startup,
/// so it runs while the window and GPU are being set up.
pub fn preload_fonts() {
    let handle = std::thread::Builder::new().name("fonts".into()).spawn(FontSystem::new).ok();
    *PRELOAD.lock().unwrap_or_else(|e| e.into_inner()) = handle;
}

/// The preloaded font database (waiting for it if needed), or a fresh
/// one. Also returns how long the caller had to wait.
fn take_font_system() -> (FontSystem, Duration) {
    let start = Instant::now();
    let handle = PRELOAD.lock().unwrap_or_else(|e| e.into_inner()).take();
    let fs = handle.and_then(|h| h.join().ok()).unwrap_or_else(FontSystem::new);
    (fs, start.elapsed())
}

fn atlas_texture(device: &wgpu::Device, size: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph atlas"),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphStyle {
    pub bold: bool,
    pub italic: bool,
}

impl GlyphStyle {
    pub const REGULAR: Self = Self { bold: false, italic: false };

    fn index(self) -> usize {
        self.bold as usize | (self.italic as usize) << 1
    }
}

/// Where a rasterised glyph sits in the atlas and how to place it.
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// Texel rectangle in the atlas: x, y, w, h.
    pub uv: [f32; 4],
    /// Offset from the pen position on the baseline to the bitmap's top-left.
    pub left: f32,
    pub top: f32,
    pub colored: bool,
    /// ClearType: separate coverage per colour channel.
    pub subpixel: bool,
}

impl Glyph {
    /// The shader's kind of quad for this glyph.
    pub fn kind(&self) -> u32 {
        if self.colored {
            super::KIND_COLOR_GLYPH
        } else if self.subpixel {
            super::KIND_SUBPIXEL_GLYPH
        } else {
            super::KIND_MASK_GLYPH
        }
    }
}

#[derive(Clone, Copy)]
enum Slot {
    Unloaded,
    /// Whitespace or a glyph no font has.
    Empty,
    Ready(Glyph),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the top of the cell to the baseline.
    pub baseline: f32,
    pub underline_thickness: f32,
}

/// Which font a glyph is drawn in. Both share one atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// The terminal's font, as the settings choose it.
    Terminal,
    /// The font of the Options and About panels: the platform's default
    /// monospace font at a size set by the window scale only, so the
    /// panels keep their size while the terminal font changes.
    Ui,
}

/// One font of the cache: its size, cell, and the glyphs of it in the atlas.
struct FaceState {
    family: String,
    font_px: f32,
    line_height: f32,
    metrics: CellMetrics,
    /// Shapes one character at a time, at this face's size.
    scratch: Buffer,
    ascii: Vec<Slot>,
    other: HashMap<(char, GlyphStyle), Slot>,
}

impl FaceState {
    fn new(fs: &mut FontSystem, family: &str, font_px: f32, line_height: f32) -> Self {
        let family = resolve_family(fs, family);
        let scratch = Buffer::new(fs, Metrics::new(font_px, (font_px * line_height).round()));
        let mut face = Self {
            family,
            font_px,
            line_height,
            metrics: CellMetrics { width: 1.0, height: 1.0, baseline: 1.0, underline_thickness: 1.0 },
            scratch,
            ascii: vec![Slot::Unloaded; 128 * 4],
            other: HashMap::new(),
        };
        face.measure(fs);
        face
    }

    fn set(&mut self, fs: &mut FontSystem, family: &str, font_px: f32, line_height: f32) {
        self.family = resolve_family(fs, family);
        self.font_px = font_px;
        self.line_height = line_height;
        self.scratch.set_metrics(Metrics::new(font_px, (font_px * line_height).round()));
        self.measure(fs);
    }

    fn clear(&mut self) {
        self.ascii.fill(Slot::Unloaded);
        self.other.clear();
    }

    fn attrs(&self, style: GlyphStyle) -> Attrs<'_> {
        Attrs::new()
            .family(Family::Name(&self.family))
            .weight(if style.bold { Weight::BOLD } else { Weight::NORMAL })
            .style(if style.italic { Style::Italic } else { Style::Normal })
    }

    fn measure(&mut self, fs: &mut FontSystem) {
        let attrs = Attrs::new().family(Family::Name(&self.family));
        self.scratch.set_text("M", &attrs, Shaping::Advanced, None);
        self.scratch.shape_until_scroll(fs, false);
        let height = (self.font_px * self.line_height).round().max(1.0);
        let (width, baseline) = self
            .scratch
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|g| (g.w, run.line_y)))
            .unwrap_or((self.font_px * 0.6, height * 0.8));
        self.metrics = CellMetrics {
            width: width.round().max(1.0),
            height,
            baseline: baseline.round(),
            underline_thickness: (self.font_px / 14.0).round().max(1.0),
        };
    }

    fn slot(&mut self, c: char, style: GlyphStyle) -> &mut Slot {
        if (c as u32) < 128 {
            &mut self.ascii[(c as usize) * 4 + style.index()]
        } else {
            self.other.entry((c, style)).or_insert(Slot::Unloaded)
        }
    }
}

/// Line height of the panels' font.
const UI_LINE_HEIGHT: f32 = 1.2;

pub struct GlyphCache {
    font_system: FontSystem,
    swash: SwashCache,
    /// The terminal's font and the panels' (see `Face`).
    faces: [FaceState; 2],
    alloc: BucketedAtlasAllocator,
    /// RGBA staging for one glyph, reused to avoid per-glyph allocations.
    upload: Vec<u8>,
    device: wgpu::Device,
    texture: wgpu::Texture,
    /// Side of `texture` in texels.
    size: u32,
    /// Changes when `texture` is replaced by a bigger one.
    pub generation: u64,
    /// Set when the atlas filled up and was cleared mid-frame.
    pub overflowed: bool,
    /// How long startup waited for the font database.
    pub font_wait: Duration,
    /// Rasterise with sub-pixel (LCD) coverage per colour channel.
    subpixel: bool,
    /// Apply the fonts' hinting; see `hinted`.
    hinted: bool,
    freetype: super::freetype::Rasterizer,
}

/// Whether glyphs are hinted at this window scale. wezterm hints below
/// 100 DPI (scale 1 at 96 DPI) and leaves high-DPI text unhinted.
pub fn hinted(scale: f64) -> bool {
    96.0 * scale < 100.0
}

impl GlyphCache {
    /// A cache drawing the terminal in `family` at `font_px`. The panels'
    /// font starts at the same size; `set_ui_font` sets it.
    pub fn new(device: &wgpu::Device, family: &str, font_px: f32, line_height: f32, hinted: bool, subpixel: bool) -> Self {
        let (mut font_system, font_wait) = take_font_system();
        let terminal = FaceState::new(&mut font_system, family, font_px, line_height);
        let ui = FaceState::new(&mut font_system, crate::config::default_font(), font_px, UI_LINE_HEIGHT);
        let texture = atlas_texture(device, ATLAS_START);
        Self {
            font_system,
            swash: SwashCache::new(),
            faces: [terminal, ui],
            alloc: BucketedAtlasAllocator::new(size2(ATLAS_START as i32, ATLAS_START as i32)),
            upload: Vec::new(),
            device: device.clone(),
            texture,
            size: ATLAS_START,
            generation: 0,
            overflowed: false,
            font_wait,
            subpixel,
            hinted,
            freetype: super::freetype::Rasterizer::new(super::system_bgr()),
        }
    }

    fn face(&self, face: Face) -> &FaceState {
        &self.faces[face as usize]
    }

    /// The cell of a face's font.
    pub fn metrics(&self, face: Face) -> CellMetrics {
        self.face(face).metrics
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Side of the atlas texture in texels.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Changes the terminal's font family, size or hinting. Clears the atlas.
    pub fn set_font(&mut self, family: &str, font_px: f32, line_height: f32, hinted: bool) {
        self.faces[Face::Terminal as usize].set(&mut self.font_system, family, font_px, line_height);
        self.hinted = hinted;
        self.clear();
    }

    /// Sets the size of the panels' font (it changes with the window
    /// scale only). Clears the atlas if the size changes.
    pub fn set_ui_font(&mut self, font_px: f32) {
        let ui = &mut self.faces[Face::Ui as usize];
        if ui.font_px != font_px {
            let family = ui.family.clone();
            ui.set(&mut self.font_system, &family, font_px, UI_LINE_HEIGHT);
            self.clear();
        }
    }

    /// Switches ClearType on or off; clears the atlas if that changes.
    pub fn set_subpixel(&mut self, subpixel: bool) {
        if subpixel != self.subpixel {
            self.subpixel = subpixel;
            self.clear();
        }
    }

    fn clear(&mut self) {
        for f in &mut self.faces {
            f.clear();
        }
        self.alloc.clear();
    }

    /// Returns the atlas entry for `c`, rasterising and uploading it on
    /// first use.
    #[cfg(test)]
    pub fn get(&mut self, queue: &wgpu::Queue, c: char, style: GlyphStyle) -> Option<Glyph> {
        self.get_in(queue, Face::Terminal, c, style)
    }

    /// `get` in the given face.
    pub fn get_in(&mut self, queue: &wgpu::Queue, face: Face, c: char, style: GlyphStyle) -> Option<Glyph> {
        let f = &mut self.faces[face as usize];
        let known = if (c as u32) < 128 {
            f.ascii[(c as usize) * 4 + style.index()]
        } else {
            f.other.get(&(c, style)).copied().unwrap_or(Slot::Unloaded)
        };
        match known {
            Slot::Ready(g) => return Some(g),
            Slot::Empty => return None,
            Slot::Unloaded => {}
        }
        let slot = self.rasterize(queue, face, c, style);
        *self.faces[face as usize].slot(c, style) = slot;
        match slot {
            Slot::Ready(g) => Some(g),
            _ => None,
        }
    }

    fn rasterize(&mut self, queue: &wgpu::Queue, face: Face, c: char, style: GlyphStyle) -> Slot {
        if c.is_whitespace() || c.is_control() {
            return Slot::Empty;
        }
        let mut utf8 = [0u8; 4];
        let text: &str = c.encode_utf8(&mut utf8);
        let f = &mut self.faces[face as usize];
        // `attrs` borrows f.family; detach before mutably borrowing f.
        let attrs = cosmic_text::AttrsOwned::new(&f.attrs(style));
        f.scratch.set_text(text, &attrs.as_attrs(), Shaping::Advanced, None);
        f.scratch.shape_until_scroll(&mut self.font_system, false);
        let Some(key) = f
            .scratch
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|g| g.physical((0.0, 0.0), 1.0).cache_key))
        else {
            return Slot::Empty;
        };
        let native = self.freetype.rasterize(&mut self.font_system, key, self.hinted, self.subpixel);
        let (left, top, w, h, content, data) = match native {
            Some(m) => {
                let content = if m.subpixel { SwashContent::SubpixelMask } else { SwashContent::Mask };
                (m.left, m.top, m.width, m.height, content, m.data)
            }
            None => {
                let Some(image) = self.swash.get_image_uncached(&mut self.font_system, key) else {
                    return Slot::Empty;
                };
                let p = image.placement;
                (p.left, p.top, p.width, p.height, image.content, image.data)
            }
        };
        if w == 0 || h == 0 {
            return Slot::Empty;
        }

        // 1px gutter so linear sampling can't bleed between glyphs.
        let alloc = match self.alloc.allocate(size2(w as i32 + 1, h as i32 + 1)) {
            Some(a) => a,
            None => {
                if self.size < ATLAS_MAX {
                    // A new texture twice the size; its glyphs are drawn
                    // again as they are used.
                    self.size *= 2;
                    log::info!("glyph atlas full, growing to {0}x{0}", self.size);
                    self.texture = atlas_texture(&self.device, self.size);
                    self.alloc = BucketedAtlasAllocator::new(size2(self.size as i32, self.size as i32));
                    self.generation += 1;
                } else {
                    log::info!("glyph atlas full, clearing");
                }
                self.clear();
                self.overflowed = true;
                match self.alloc.allocate(size2(w as i32 + 1, h as i32 + 1)) {
                    Some(a) => a,
                    None => return Slot::Empty,
                }
            }
        };

        let colored = matches!(content, SwashContent::Color);
        let subpixel = self.subpixel && matches!(content, SwashContent::SubpixelMask);
        self.upload.clear();
        match content {
            SwashContent::Mask => {
                self.upload.extend(data.iter().flat_map(|&a| [255, 255, 255, a]));
            }
            SwashContent::SubpixelMask if subpixel => self.upload.extend_from_slice(&data),
            SwashContent::SubpixelMask => {
                self.upload.extend(data.chunks_exact(4).flat_map(|p| {
                    let a = ((p[0] as u16 + p[1] as u16 + p[2] as u16) / 3) as u8;
                    [255, 255, 255, a]
                }));
            }
            SwashContent::Color => self.upload.extend_from_slice(&data),
        }

        let origin = alloc.rectangle.min;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: origin.x as u32, y: origin.y as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &self.upload,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );

        Slot::Ready(Glyph {
            uv: [origin.x as f32, origin.y as f32, w as f32, h as f32],
            left: left as f32,
            top: top as f32,
            colored,
            subpixel,
        })
    }

    /// Names of the installed monospace font families, in alphabetical
    /// order, with the family in use among them (for Options → Font).
    /// Hidden system fonts (macOS names them with a leading dot) are left out.
    pub fn monospace_families(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .font_system
            .db()
            .faces()
            .filter(|f| f.monospaced)
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .filter(|n| !n.starts_with('.'))
            .collect();
        v.push(self.face(Face::Terminal).family.clone());
        v.sort_by_key(|n| n.to_lowercase());
        v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        v
    }

    /// The family drawn: the one asked for, or the fallback for it.
    pub fn family(&self) -> &str {
        &self.face(Face::Terminal).family
    }
}

/// Uses the requested family if installed, else the system monospace font.
fn resolve_family(fs: &mut FontSystem, wanted: &str) -> String {
    let db = fs.db();
    let has = |name: &str| db.faces().any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)));
    if has(wanted) {
        return db
            .faces()
            .flat_map(|f| f.families.iter())
            .find(|(n, _)| n.eq_ignore_ascii_case(wanted))
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| wanted.to_owned());
    }
    for fallback in ["JetBrains Mono", "DejaVu Sans Mono", "Cascadia Mono", "Consolas", "Menlo", "Monaco", "Liberation Mono", "Noto Sans Mono", "Ubuntu Mono"] {
        if has(fallback) {
            log::warn!("font '{wanted}' not found, using '{fallback}'");
            return fallback.to_owned();
        }
    }
    let any = db.faces().find(|f| f.monospaced).and_then(|f| f.families.first()).map(|(n, _)| n.clone());
    log::warn!("font '{wanted}' not found, using {any:?}");
    any.unwrap_or_else(|| wanted.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::testing;

    fn cache() -> Option<(GlyphCache, wgpu::Queue)> {
        let (device, queue) = testing::device()?;
        Some((GlyphCache::new(&device, "DejaVu Sans Mono", 16.0, 1.2, true, false), queue))
    }

    #[test]
    fn cell_metrics_are_sane() {
        let Some((g, _)) = cache() else { return };
        let m = g.metrics(Face::Terminal);
        assert!(m.width >= 6.0 && m.width <= 14.0, "width {}", m.width);
        assert_eq!(m.height, (16.0f32 * 1.2).round());
        assert!(m.baseline > 0.0 && m.baseline < m.height);
        assert_eq!(m.width.fract(), 0.0, "cell width is whole pixels");
    }

    #[test]
    fn glyphs_are_cached_and_whitespace_is_empty() {
        let Some((mut g, q)) = cache() else { return };
        let a = g.get(&q, 'A', GlyphStyle::REGULAR).expect("A has a glyph");
        assert!(a.uv[2] > 0.0 && a.uv[3] > 0.0);
        assert!(!a.colored);
        let again = g.get(&q, 'A', GlyphStyle::REGULAR).unwrap();
        assert_eq!(a.uv, again.uv, "second lookup hits the cache");
        assert!(g.get(&q, ' ', GlyphStyle::REGULAR).is_none());
        assert!(g.get(&q, '\t', GlyphStyle::REGULAR).is_none());
        // Styles and non-ASCII get their own atlas slots.
        let bold = g.get(&q, 'A', GlyphStyle { bold: true, italic: false }).unwrap();
        assert_ne!(bold.uv[..2], a.uv[..2]);
        let cyr = g.get(&q, 'Ж', GlyphStyle::REGULAR).expect("fallback finds Cyrillic");
        assert_ne!(cyr.uv[..2], a.uv[..2]);
    }

    #[test]
    fn changing_font_size_changes_metrics_and_clears_the_atlas() {
        let Some((mut g, q)) = cache() else { return };
        let small = g.metrics(Face::Terminal);
        g.get(&q, 'x', GlyphStyle::REGULAR);
        g.set_font("DejaVu Sans Mono", 32.0, 1.2, true);
        assert!(g.metrics(Face::Terminal).width > small.width);
        assert!(g.metrics(Face::Terminal).height > small.height);
        for f in &g.faces {
            assert!(f.other.is_empty());
            assert!(f.ascii.iter().all(|s| matches!(s, Slot::Unloaded)));
        }
    }

    #[test]
    fn the_panel_font_keeps_its_size_when_the_terminal_font_changes() {
        let Some((mut g, q)) = cache() else { return };
        g.set_ui_font(16.0);
        let ui = g.metrics(Face::Ui);
        g.set_font("DejaVu Sans Mono", 32.0, 2.0, true);
        assert_eq!(g.metrics(Face::Ui), ui, "terminal font and line height leave it alone");
        assert_ne!(g.metrics(Face::Terminal), ui);
        // Both faces draw from the one atlas, each with its own glyphs.
        let big = g.get_in(&q, Face::Terminal, 'M', GlyphStyle::REGULAR).unwrap();
        let small = g.get_in(&q, Face::Ui, 'M', GlyphStyle::REGULAR).unwrap();
        assert!(big.uv[3] > small.uv[3]);
        // The window scale does change it.
        g.set_ui_font(32.0);
        assert!(g.metrics(Face::Ui).height > ui.height);
    }

    #[test]
    fn a_full_atlas_grows_once_then_clears() {
        let Some((mut g, q)) = cache() else { return };
        assert_eq!(g.size(), ATLAS_START);
        // Big glyphs fill the atlas quickly: ASCII, Cyrillic and Greek in
        // four styles, over and over.
        g.set_font("DejaVu Sans Mono", 200.0, 1.2, true);
        let styles = [(false, false), (true, false), (false, true), (true, true)];
        let mut glyphs = std::iter::repeat_with(|| ('!'..='~').chain('А'..='я').chain('Α'..='ω'))
            .flatten()
            .flat_map(|c| styles.map(|(bold, italic)| (c, GlyphStyle { bold, italic })));
        let mut fill = |g: &mut GlyphCache| {
            for _ in 0..100_000 {
                let (c, style) = glyphs.next().unwrap();
                g.get(&q, c, style);
                if g.overflowed {
                    return;
                }
            }
            panic!("the atlas never filled");
        };
        fill(&mut g);
        assert_eq!((g.size(), g.generation), (ATLAS_MAX, 1), "it grows first");
        assert!(g.faces[0].other.len() <= 1, "and starts empty");
        g.overflowed = false;
        fill(&mut g);
        assert_eq!((g.size(), g.generation), (ATLAS_MAX, 1), "at the largest size it clears instead");
    }

    #[test]
    fn unknown_family_falls_back_to_an_installed_monospace_font() {
        let Some((device, _)) = testing::device() else { return };
        let g = GlyphCache::new(&device, "No Such Font 12345", 16.0, 1.2, true, false);
        assert_ne!(g.family(), "No Such Font 12345");
        assert!(g.metrics(Face::Terminal).width > 0.0);
    }
}
