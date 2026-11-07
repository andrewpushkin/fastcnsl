//! Glyph rasterisation and the GPU texture atlas.
//!
//! Each (character, bold, italic) is shaped once with cosmic-text (which
//! also handles font fallback for emoji, CJK, symbols...), rasterised with
//! swash and packed into one RGBA texture. After that, drawing a glyph is a
//! table lookup; ASCII skips the hash map entirely.

use std::collections::HashMap;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
};
use etagere::{BucketedAtlasAllocator, size2};

pub const ATLAS_SIZE: u32 = 2048;

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

/// Off Windows there is no native rasteriser; this stands in for its mask.
#[cfg(not(windows))]
struct NativeMask {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    subpixel: bool,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the top of the cell to the baseline.
    pub baseline: f32,
    pub underline_thickness: f32,
}

pub struct GlyphCache {
    font_system: FontSystem,
    swash: SwashCache,
    scratch: Buffer,
    family: String,
    font_px: f32,
    pub metrics: CellMetrics,
    ascii: Vec<Slot>,
    other: HashMap<(char, GlyphStyle), Slot>,
    alloc: BucketedAtlasAllocator,
    /// RGBA staging for one glyph, reused to avoid per-glyph allocations.
    upload: Vec<u8>,
    texture: wgpu::Texture,
    /// Set when the atlas filled up and was cleared mid-frame.
    pub overflowed: bool,
    /// How long startup waited for the font database.
    pub font_wait: Duration,
    /// Rasterise with ClearType (per-channel coverage) where the platform
    /// rasteriser offers it.
    subpixel: bool,
    #[cfg(windows)]
    dwrite: super::dwrite::Rasterizer,
}

impl GlyphCache {
    pub fn new(device: &wgpu::Device, family: &str, font_px: f32, line_height: f32, subpixel: bool) -> Self {
        let (mut font_system, font_wait) = take_font_system();
        let family = resolve_family(&mut font_system, family);
        let metrics_ct = Metrics::new(font_px, (font_px * line_height).round());
        let scratch = Buffer::new(&mut font_system, metrics_ct);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d { width: ATLAS_SIZE, height: ATLAS_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut cache = Self {
            font_system,
            swash: SwashCache::new(),
            scratch,
            family,
            font_px,
            metrics: CellMetrics { width: 1.0, height: 1.0, baseline: 1.0, underline_thickness: 1.0 },
            ascii: vec![Slot::Unloaded; 128 * 4],
            other: HashMap::new(),
            alloc: BucketedAtlasAllocator::new(size2(ATLAS_SIZE as i32, ATLAS_SIZE as i32)),
            upload: Vec::new(),
            texture,
            overflowed: false,
            font_wait,
            subpixel,
            #[cfg(windows)]
            dwrite: Default::default(),
        };
        cache.metrics = cache.measure(line_height);
        cache
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Changes font family or size. Clears the atlas.
    pub fn set_font(&mut self, family: &str, font_px: f32, line_height: f32) {
        self.family = resolve_family(&mut self.font_system, family);
        self.font_px = font_px;
        self.scratch.set_metrics(Metrics::new(font_px, (font_px * line_height).round()));
        self.metrics = self.measure(line_height);
        self.clear();
    }

    /// Switches ClearType on or off; clears the atlas if that changes.
    pub fn set_subpixel(&mut self, subpixel: bool) {
        if subpixel != self.subpixel {
            self.subpixel = subpixel;
            self.clear();
        }
    }

    fn clear(&mut self) {
        self.ascii.fill(Slot::Unloaded);
        self.other.clear();
        self.alloc.clear();
    }

    fn attrs(&self, style: GlyphStyle) -> Attrs<'_> {
        Attrs::new()
            .family(Family::Name(&self.family))
            .weight(if style.bold { Weight::BOLD } else { Weight::NORMAL })
            .style(if style.italic { Style::Italic } else { Style::Normal })
    }

    fn measure(&mut self, line_height: f32) -> CellMetrics {
        let attrs = Attrs::new().family(Family::Name(&self.family));
        self.scratch.set_text("M", &attrs, Shaping::Advanced, None);
        self.scratch.shape_until_scroll(&mut self.font_system, false);
        let height = (self.font_px * line_height).round().max(1.0);
        let (width, baseline) = self
            .scratch
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|g| (g.w, run.line_y)))
            .unwrap_or((self.font_px * 0.6, height * 0.8));
        CellMetrics {
            width: width.round().max(1.0),
            height,
            baseline: baseline.round(),
            underline_thickness: (self.font_px / 14.0).round().max(1.0),
        }
    }

    /// Returns the atlas entry for `c`, rasterising and uploading it on
    /// first use.
    pub fn get(&mut self, queue: &wgpu::Queue, c: char, style: GlyphStyle) -> Option<Glyph> {
        let slot = if (c as u32) < 128 {
            self.ascii[(c as usize) * 4 + style.index()]
        } else {
            self.other.get(&(c, style)).copied().unwrap_or(Slot::Unloaded)
        };
        match slot {
            Slot::Ready(g) => return Some(g),
            Slot::Empty => return None,
            Slot::Unloaded => {}
        }
        let slot = self.rasterize(queue, c, style);
        if (c as u32) < 128 {
            self.ascii[(c as usize) * 4 + style.index()] = slot;
        } else {
            self.other.insert((c, style), slot);
        }
        match slot {
            Slot::Ready(g) => Some(g),
            _ => None,
        }
    }

    fn rasterize(&mut self, queue: &wgpu::Queue, c: char, style: GlyphStyle) -> Slot {
        if c.is_whitespace() || c.is_control() {
            return Slot::Empty;
        }
        let mut utf8 = [0u8; 4];
        let text: &str = c.encode_utf8(&mut utf8);
        let attrs = self.attrs(style);
        // `attrs` borrows self.family; detach before mutably borrowing self.
        let attrs = cosmic_text::AttrsOwned::new(&attrs);
        self.scratch.set_text(text, &attrs.as_attrs(), Shaping::Advanced, None);
        self.scratch.shape_until_scroll(&mut self.font_system, false);
        let Some(key) = self
            .scratch
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|g| g.physical((0.0, 0.0), 1.0).cache_key))
        else {
            return Slot::Empty;
        };
        let (left, top, w, h, content, data) = match self.native(key, c, style) {
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
                log::info!("glyph atlas full, clearing");
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

    /// The platform's own rasteriser, where there is one: DirectWrite on
    /// Windows. None means swash draws the glyph.
    #[cfg(windows)]
    fn native(&mut self, key: cosmic_text::CacheKey, c: char, style: GlyphStyle) -> Option<super::dwrite::Mask> {
        self.dwrite.rasterize(&mut self.font_system, key, c, style.italic, self.subpixel)
    }

    #[cfg(not(windows))]
    fn native(&mut self, _key: cosmic_text::CacheKey, _c: char, _style: GlyphStyle) -> Option<NativeMask> {
        None
    }

    /// Names of installed monospace font families, for the settings help.
    #[allow(dead_code)]
    pub fn monospace_families(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .font_system
            .db()
            .faces()
            .filter(|f| f.monospaced)
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .collect();
        v.sort();
        v.dedup();
        v
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
        Some((GlyphCache::new(&device, "DejaVu Sans Mono", 16.0, 1.2, false), queue))
    }

    #[test]
    fn cell_metrics_are_sane() {
        let Some((g, _)) = cache() else { return };
        let m = g.metrics;
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
        let small = g.metrics;
        g.get(&q, 'x', GlyphStyle::REGULAR);
        g.set_font("DejaVu Sans Mono", 32.0, 1.2);
        assert!(g.metrics.width > small.width);
        assert!(g.metrics.height > small.height);
        assert!(g.other.is_empty());
        assert!(g.ascii.iter().all(|s| matches!(s, Slot::Unloaded)));
    }

    #[test]
    fn unknown_family_falls_back_to_an_installed_monospace_font() {
        let Some((device, _)) = testing::device() else { return };
        let g = GlyphCache::new(&device, "No Such Font 12345", 16.0, 1.2, false);
        assert_ne!(g.family, "No Such Font 12345");
        assert!(g.metrics.width > 0.0);
    }
}
