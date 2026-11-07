// One instanced quad per cell background, glyph or UI shape.

struct Globals {
    screen: vec2<f32>,
    atlas: vec2<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_smp: sampler;

struct InstanceIn {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) kind: u32,
    @location(5) radius: f32,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) local: vec2<f32>,
    @location(4) @interpolate(flat) size: vec2<f32>,
    @location(5) @interpolate(flat) radius: f32,
};

const KIND_RECT: u32 = 0u;
const KIND_MASK_GLYPH: u32 = 1u;
const KIND_COLOR_GLYPH: u32 = 2u;
const KIND_ROUNDED: u32 = 3u;
const KIND_CROSS: u32 = 4u;
const KIND_SUBPIXEL_GLYPH: u32 = 5u;
// 8-11: quarter circle; bit 0: centre at the right edge, bit 1: at the bottom.
const KIND_ARC: u32 = 8u;

@vertex
fn vs(@builtin(vertex_index) vi: u32, i: InstanceIn) -> VOut {
    // Triangle strip: (0,0) (1,0) (0,1) (1,1)
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = i.pos + corner * i.size;
    var out: VOut;
    out.clip = vec4<f32>(p.x / g.screen.x * 2.0 - 1.0, 1.0 - p.y / g.screen.y * 2.0, 0.0, 1.0);
    out.uv = (i.uv.xy + corner * i.uv.zw) / g.atlas;
    out.color = i.color;
    out.kind = i.kind;
    out.local = corner * i.size;
    out.size = i.size;
    out.radius = i.radius;
    return out;
}

// Glyph coverage is blended in gamma space; uncorrected, light text on a
// dark background looks thin and dark text on light looks heavy. Correct it
// with DirectWrite's formulas: enhanced contrast, then the alpha correction
// for gamma 1.8 (the Windows default). The contrast, 0.4 for every text
// colour, was fitted to Windows Terminal's pixels for light-on-dark and
// dark-on-light Cascadia Mono; it leaves no measurable bias either way.
fn text_coverage(a: f32, rgb: vec3<f32>) -> f32 {
    let k = 0.4;
    let c = a * (k + 1.0) / (a * k + 1.0);
    let f = dot(rgb, vec3<f32>(0.25, 0.5, 0.25));
    let g = vec4<f32>(0.1469, -0.8911, 1.4644, -0.3234) / 4.0;
    return clamp(c + c * (1.0 - c) * ((g.x * f + g.y) * c + (g.z * f + g.w)), 0.0, 1.0);
}

// ClearType: DirectWrite's correction per sub-pixel, each channel weighted
// by the text colour's own channel. Fitted to native Windows ClearType
// (DirectWrite drawing into a GDI bitmap with the system settings) at
// gamma 1.8: light text needs enhanced contrast 0.6, dark text none;
// colours in between are interpolated by brightness. Both then match the
// native pixels to within 0.01 on average.
fn cleartype_coverage(t: vec3<f32>, rgb: vec3<f32>) -> vec3<f32> {
    let luma = dot(rgb, vec3<f32>(0.30, 0.59, 0.11));
    let k = 0.6 * clamp((luma - 0.15) / 0.7, 0.0, 1.0);
    let c = t * (k + 1.0) / (t * k + 1.0);
    let g = vec4<f32>(0.1469, -0.8911, 1.4644, -0.3234) / 4.0;
    return clamp(c + c * (1.0 - c) * ((g.x * rgb + g.y) * c + (g.z * rgb + g.w)), vec3<f32>(0.0), vec3<f32>(1.0));
}

// What a fragment contributes: its colour, and how much of it covers the
// pixel, per channel. Grayscale shapes cover all channels alike; ClearType
// text covers the red, green and blue sub-pixels separately. The entry
// point that hands this to the blender is appended by the renderer: with
// dual-source blending the weights blend each channel on its own,
// otherwise their alpha is used.
struct Shaded {
    color: vec3<f32>,
    weights: vec4<f32>,
};

fn plain(rgb: vec3<f32>, a: f32) -> Shaded {
    return Shaded(rgb, vec4<f32>(a, a, a, a));
}

fn shade(v: VOut) -> Shaded {
    if v.kind >= KIND_ARC {
        // `radius` is the line width; the circle touches the quad's far
        // edges with a 1px margin for anti-aliasing.
        let k = v.kind - KIND_ARC;
        let centre = vec2<f32>(f32(k & 1u), f32((k >> 1u) & 1u)) * v.size;
        let r = v.size.x - v.radius * 0.5 - 1.0;
        let d = abs(length(v.local - centre) - r);
        let a = clamp(v.radius * 0.5 + 0.5 - d, 0.0, 1.0);
        return plain(v.color.rgb, v.color.a * a);
    }
    switch v.kind {
        case KIND_MASK_GLYPH: {
            let a = textureSampleLevel(atlas_tex, atlas_smp, v.uv, 0.0).a;
            return plain(v.color.rgb, v.color.a * text_coverage(a, v.color.rgb));
        }
        case KIND_SUBPIXEL_GLYPH: {
            let t = textureSampleLevel(atlas_tex, atlas_smp, v.uv, 0.0);
            let w = cleartype_coverage(t.rgb, v.color.rgb) * v.color.a;
            return Shaded(v.color.rgb, vec4<f32>(w, max(w.r, max(w.g, w.b))));
        }
        case KIND_COLOR_GLYPH: {
            let t = textureSampleLevel(atlas_tex, atlas_smp, v.uv, 0.0);
            return plain(t.rgb, t.a * v.color.a);
        }
        case KIND_ROUNDED: {
            // Signed distance to a rounded rectangle, 1px anti-aliased edge.
            let half = v.size * 0.5;
            let r = min(v.radius, min(half.x, half.y));
            let q = abs(v.local - half) - (half - vec2<f32>(r, r));
            let d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
            let a = clamp(0.5 - d, 0.0, 1.0);
            return plain(v.color.rgb, v.color.a * a);
        }
        case KIND_CROSS: {
            // Both diagonals of the square; `radius` is the line width.
            let s = v.size.x;
            let d = min(abs(v.local.x - v.local.y), abs(v.local.x + v.local.y - s)) * 0.70710678;
            let a = clamp(v.radius * 0.5 + 0.5 - d, 0.0, 1.0);
            return plain(v.color.rgb, v.color.a * a);
        }
        default: {
            return plain(v.color.rgb, v.color.a);
        }
    }
}
