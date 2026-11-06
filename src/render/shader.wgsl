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

@fragment
fn fs(v: VOut) -> @location(0) vec4<f32> {
    switch v.kind {
        case KIND_MASK_GLYPH: {
            let a = textureSampleLevel(atlas_tex, atlas_smp, v.uv, 0.0).a;
            return vec4<f32>(v.color.rgb, v.color.a * a);
        }
        case KIND_COLOR_GLYPH: {
            let t = textureSampleLevel(atlas_tex, atlas_smp, v.uv, 0.0);
            return vec4<f32>(t.rgb, t.a * v.color.a);
        }
        case KIND_ROUNDED: {
            // Signed distance to a rounded rectangle, 1px anti-aliased edge.
            let half = v.size * 0.5;
            let r = min(v.radius, min(half.x, half.y));
            let q = abs(v.local - half) - (half - vec2<f32>(r, r));
            let d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
            let a = clamp(0.5 - d, 0.0, 1.0);
            return vec4<f32>(v.color.rgb, v.color.a * a);
        }
        default: {
            return v.color;
        }
    }
}
