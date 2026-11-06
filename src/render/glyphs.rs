//! Glyph rasterisation and the GPU texture atlas.
//!
//! Each (character, bold, italic) is shaped once with cosmic-text (which
//! also handles font fallback for emoji, CJK, symbols...), rasterised with
//! swash and packed into one RGBA texture. After that, drawing a glyph is a
//! table lookup; ASCII skips the hash map entirely.

use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
};
use etagere::{BucketedAtlasAllocator, size2};

pub const ATLAS_SIZE: u32 = 2048;

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
}

impl GlyphCache {
    pub fn new(device: &wgpu::Device, family: &str, font_px: f32, line_height: f32) -> Self {
        let mut font_system = FontSystem::new();
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
        let Some(image) = self.swash.get_image_uncached(&mut self.font_system, key) else {
            return Slot::Empty;
        };
        let (w, h) = (image.placement.width, image.placement.height);
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

        let colored = matches!(image.content, SwashContent::Color);
        self.upload.clear();
        match image.content {
            SwashContent::Mask => {
                self.upload.extend(image.data.iter().flat_map(|&a| [255, 255, 255, a]));
            }
            SwashContent::SubpixelMask => {
                self.upload.extend(image.data.chunks_exact(4).flat_map(|p| {
                    let a = ((p[0] as u16 + p[1] as u16 + p[2] as u16) / 3) as u8;
                    [255, 255, 255, a]
                }));
            }
            SwashContent::Color => self.upload.extend_from_slice(&image.data),
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
            left: image.placement.left as f32,
            top: image.placement.top as f32,
            colored,
        })
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
