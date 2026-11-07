//! Box-drawing lines (U+2500–257F) and block elements (U+2580–259F) drawn
//! as shapes that fill the cell exactly, like other terminals do. Font
//! glyphs for these are shorter than the cell when the line height is
//! above 1.0, so borders and bars would show gaps between rows.

use super::{Layer, Painter};

/// Line weight towards each side of the cell, two bits per direction:
/// up << 6 | right << 4 | down << 2 | left; 0 none, 1 light, 2 heavy,
/// 3 double. Derived from the Unicode character names; 0 marks the
/// dashed, arc and diagonal characters, which are handled separately (the
/// diagonals by the font).
const LINES: [u8; 128] = [
    0x11, 0x22, 0x44, 0x88, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x14, 0x24, 0x18, 0x28, // U+2500
    0x05, 0x06, 0x09, 0x0a, 0x50, 0x60, 0x90, 0xa0, 0x41, 0x42, 0x81, 0x82, 0x54, 0x64, 0x94, 0x58, // U+2510
    0x98, 0xa4, 0x68, 0xa8, 0x45, 0x46, 0x85, 0x49, 0x89, 0x86, 0x4a, 0x8a, 0x15, 0x16, 0x25, 0x26, // U+2520
    0x19, 0x1a, 0x29, 0x2a, 0x51, 0x52, 0x61, 0x62, 0x91, 0x92, 0xa1, 0xa2, 0x55, 0x56, 0x65, 0x66, // U+2530
    0x95, 0x59, 0x99, 0x96, 0xa5, 0x5a, 0x69, 0xa6, 0x6a, 0x9a, 0xa9, 0xaa, 0x00, 0x00, 0x00, 0x00, // U+2540
    0x33, 0xcc, 0x34, 0x1c, 0x3c, 0x07, 0x0d, 0x0f, 0x70, 0xd0, 0xf0, 0x43, 0xc1, 0xc3, 0x74, 0xdc, // U+2550
    0xfc, 0x47, 0xcd, 0xcf, 0x37, 0x1d, 0x3f, 0x73, 0xd1, 0xf3, 0x77, 0xdd, 0xff, 0x00, 0x00, 0x00, // U+2560
    0x00, 0x00, 0x00, 0x00, 0x01, 0x40, 0x10, 0x04, 0x02, 0x80, 0x20, 0x08, 0x21, 0x48, 0x12, 0x84, // U+2570
];

const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// Draws `c` into the cell at (x, y) if it is one of the characters drawn
/// as shapes; returns false to leave it to the font.
#[allow(clippy::too_many_arguments)]
pub fn draw(p: &mut Painter<'_>, layer: Layer, c: char, x: f32, y: f32, w: f32, h: f32, t: f32, color: [f32; 4]) -> bool {
    let cp = c as u32;
    match cp {
        0x2500..=0x257F => {
            let bits = LINES[(cp - 0x2500) as usize];
            if bits != 0 {
                lines(p, layer, Cell { x, y, w, h, t }, bits, color);
                return true;
            }
            match cp {
                // Dashed: (dashes, heavy, vertical).
                0x2504..=0x250B | 0x254C..=0x254F => {
                    let (n, k) = if cp >= 0x254C { (2, cp - 0x254C) } else if cp >= 0x2508 { (4, cp - 0x2508) } else { (3, cp - 0x2504) };
                    dashes(p, layer, Cell { x, y, w, h, t }, n, k % 2 == 1, k >= 2, color);
                    true
                }
                0x256D..=0x2570 => {
                    arc(p, layer, Cell { x, y, w, h, t }, cp - 0x256D, color);
                    true
                }
                _ => false,
            }
        }
        0x2580..=0x259F => {
            blocks(p, layer, x, y, w, h, cp, color);
            true
        }
        _ => false,
    }
}

#[derive(Clone, Copy)]
struct Cell {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Light line thickness in pixels.
    t: f32,
}

impl Cell {
    fn heavy(&self) -> f32 {
        (self.t * 2.0).max(self.t + 1.0)
    }

    fn thickness(&self, weight: u8) -> f32 {
        match weight {
            LIGHT | DOUBLE => self.t,
            HEAVY => self.heavy(),
            _ => 0.0,
        }
    }

    /// Distance from the centre line to each line of a double pair.
    fn gap(&self) -> f32 {
        self.t.max(1.0)
    }

    /// Centre lines, snapped so a line `thickness` wide covers whole pixels.
    fn snap(pos: f32, thickness: f32) -> f32 {
        (pos - thickness / 2.0).round()
    }
}

/// Horizontal line at centre height `cy`, from x0 to x1.
fn hline(p: &mut Painter<'_>, layer: Layer, x0: f32, x1: f32, cy: f32, th: f32, color: [f32; 4]) {
    if x1 > x0 {
        p.rect(layer, x0, Cell::snap(cy, th), x1 - x0, th, color);
    }
}

/// Vertical line at centre `cx`, from y0 to y1.
fn vline(p: &mut Painter<'_>, layer: Layer, y0: f32, y1: f32, cx: f32, th: f32, color: [f32; 4]) {
    if y1 > y0 {
        p.rect(layer, Cell::snap(cx, th), y0, th, y1 - y0, color);
    }
}

/// How far past the centre a line of a double pair reaches before a
/// crossing line: `side` is the weight of the perpendicular line on this
/// line's side, `other` the one on the far side. Positive is towards the
/// far side.
fn double_reach(c: &Cell, side: u8, other: u8) -> f32 {
    let (g, t) = (c.gap(), c.t);
    match (side, other) {
        (DOUBLE, _) => -g + t / 2.0,
        (NONE, DOUBLE) => g + t / 2.0,
        (NONE, NONE) => 0.0,
        (NONE, o) => c.thickness(o) / 2.0,
        (s, _) => c.thickness(s) / 2.0,
    }
}

fn lines(p: &mut Painter<'_>, layer: Layer, c: Cell, bits: u8, color: [f32; 4]) {
    let (u, r, d, l) = (bits >> 6, (bits >> 4) & 3, (bits >> 2) & 3, bits & 3);
    let (cx, cy) = (c.x + (c.w / 2.0).floor(), c.y + (c.h / 2.0).floor());
    let (g, t) = (c.gap(), c.t);
    let horiz_double = l == DOUBLE || r == DOUBLE;
    let vert_double = u == DOUBLE || d == DOUBLE;
    // Single and heavy lines run through the centre far enough to cover the
    // crossing line; into a double pair they stop at its nearer line.
    let hmax = c.thickness(l).max(c.thickness(r));
    let vmax = c.thickness(u).max(c.thickness(d));
    match u {
        NONE => {}
        DOUBLE => {
            vline(p, layer, c.y, cy + double_reach(&c, l, r), cx - g, t, color);
            vline(p, layer, c.y, cy + double_reach(&c, r, l), cx + g, t, color);
        }
        w => {
            let end = if horiz_double { cy - g + t / 2.0 } else { cy + hmax / 2.0 };
            vline(p, layer, c.y, end, cx, c.thickness(w), color);
        }
    }
    match d {
        NONE => {}
        DOUBLE => {
            vline(p, layer, cy - double_reach(&c, l, r), c.y + c.h, cx - g, t, color);
            vline(p, layer, cy - double_reach(&c, r, l), c.y + c.h, cx + g, t, color);
        }
        w => {
            let start = if horiz_double { cy + g - t / 2.0 } else { cy - hmax / 2.0 };
            vline(p, layer, start, c.y + c.h, cx, c.thickness(w), color);
        }
    }
    match l {
        NONE => {}
        DOUBLE => {
            hline(p, layer, c.x, cx + double_reach(&c, u, d), cy - g, t, color);
            hline(p, layer, c.x, cx + double_reach(&c, d, u), cy + g, t, color);
        }
        w => {
            let end = if vert_double { cx - g + t / 2.0 } else { cx + vmax / 2.0 };
            hline(p, layer, c.x, end, cy, c.thickness(w), color);
        }
    }
    match r {
        NONE => {}
        DOUBLE => {
            hline(p, layer, cx - double_reach(&c, u, d), c.x + c.w, cy - g, t, color);
            hline(p, layer, cx - double_reach(&c, d, u), c.x + c.w, cy + g, t, color);
        }
        w => {
            let start = if vert_double { cx + g - t / 2.0 } else { cx - vmax / 2.0 };
            hline(p, layer, start, c.x + c.w, cy, c.thickness(w), color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn dashes(p: &mut Painter<'_>, layer: Layer, c: Cell, n: u32, heavy: bool, vertical: bool, color: [f32; 4]) {
    let th = if heavy { c.heavy() } else { c.t };
    let (cx, cy) = (c.x + (c.w / 2.0).floor(), c.y + (c.h / 2.0).floor());
    let len = if vertical { c.h } else { c.w };
    let slot = len / n as f32;
    let dash = (slot * 0.6).round().max(1.0);
    for i in 0..n {
        let start = (i as f32 * slot + (slot - dash) / 2.0).round();
        if vertical {
            vline(p, layer, c.y + start, c.y + start + dash, cx, th, color);
        } else {
            hline(p, layer, c.x + start, c.x + start + dash, cy, th, color);
        }
    }
}

/// ╭ ╮ ╯ ╰ (`k` 0–3): a quarter circle joined to the cell edges by
/// straight lines.
fn arc(p: &mut Painter<'_>, layer: Layer, c: Cell, k: u32, color: [f32; 4]) {
    let (cx, cy) = (c.x + (c.w / 2.0).floor(), c.y + (c.h / 2.0).floor());
    let r = (c.w.min(c.h) / 2.0).floor();
    // Which way the two ends go: (right?, down?).
    let (right, down) = match k {
        0 => (true, true),   // ╭
        1 => (false, true),  // ╮
        2 => (false, false), // ╯
        _ => (true, false),  // ╰
    };
    let ox = if right { cx + r } else { cx - r };
    let oy = if down { cy + r } else { cy - r };
    // The visible quarter lies back towards the cell centre: up-left of
    // the circle's centre for ╭.
    p.arc(layer, ox, oy, r, c.t, right, down, color);
    // The rest of the way to the cell edges.
    if down {
        vline(p, layer, oy, c.y + c.h, cx, c.t, color);
    } else {
        vline(p, layer, c.y, oy, cx, c.t, color);
    }
    if right {
        hline(p, layer, ox, c.x + c.w, cy, c.t, color);
    } else {
        hline(p, layer, c.x, ox, cy, c.t, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn blocks(p: &mut Painter<'_>, layer: Layer, x: f32, y: f32, w: f32, h: f32, cp: u32, color: [f32; 4]) {
    let frac = |len: f32, eighths: u32| (len * eighths as f32 / 8.0).round();
    let (hw, hh) = ((w / 2.0).round(), (h / 2.0).round());
    let quad = |p: &mut Painter<'_>, q: u32| {
        if q & 1 != 0 {
            p.rect(layer, x, y, hw, hh, color);
        }
        if q & 2 != 0 {
            p.rect(layer, x + hw, y, w - hw, hh, color);
        }
        if q & 4 != 0 {
            p.rect(layer, x, y + hh, hw, h - hh, color);
        }
        if q & 8 != 0 {
            p.rect(layer, x + hw, y + hh, w - hw, h - hh, color);
        }
    };
    match cp {
        0x2580 => p.rect(layer, x, y, w, hh, color),
        0x2581..=0x2588 => {
            let k = frac(h, cp - 0x2580);
            p.rect(layer, x, y + h - k, w, k, color);
        }
        0x2589..=0x258F => p.rect(layer, x, y, frac(w, 0x2590 - cp), h, color),
        0x2590 => p.rect(layer, x + hw, y, w - hw, h, color),
        0x2591..=0x2593 => {
            let a = (cp - 0x2590) as f32 * 0.25;
            p.rect(layer, x, y, w, h, [color[0], color[1], color[2], color[3] * a]);
        }
        0x2594 => p.rect(layer, x, y, w, frac(h, 1), color),
        0x2595 => {
            let k = frac(w, 1);
            p.rect(layer, x + w - k, y, k, h, color);
        }
        _ => {
            // Quadrants: 1 upper left, 2 upper right, 4 lower left, 8 lower right.
            const Q: [u32; 10] = [4, 8, 1, 1 | 4 | 8, 1 | 8, 1 | 2 | 4, 1 | 2 | 8, 2, 2 | 4, 2 | 4 | 8];
            quad(p, Q[(cp - 0x2596) as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::Canvas;
    use super::super::{Instance, KIND_ARC, KIND_RECT};
    use super::*;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    /// Draws `c` in a 10x20 cell at (100, 200) with 1px lines.
    fn shapes(c: char) -> Option<(bool, Vec<Instance>)> {
        let mut canvas = Canvas::new()?;
        let mut drawn = false;
        canvas.paint(400.0, 400.0, |p| drawn = draw(p, Layer::Text, c, 100.0, 200.0, 10.0, 20.0, 1.0, RED));
        Some((drawn, canvas.layer(Layer::Text).to_vec()))
    }

    fn bounds(v: &[Instance]) -> (f32, f32, f32, f32) {
        let x0 = v.iter().map(|i| i.pos[0]).fold(f32::MAX, f32::min);
        let y0 = v.iter().map(|i| i.pos[1]).fold(f32::MAX, f32::min);
        let x1 = v.iter().map(|i| i.pos[0] + i.size[0]).fold(f32::MIN, f32::max);
        let y1 = v.iter().map(|i| i.pos[1] + i.size[1]).fold(f32::MIN, f32::max);
        (x0, y0, x1, y1)
    }

    #[test]
    fn table_matches_known_characters() {
        let at = |c: char| LINES[(c as u32 - 0x2500) as usize];
        assert_eq!(at('─'), LIGHT << 4 | LIGHT);
        assert_eq!(at('┃'), HEAVY << 6 | HEAVY << 2);
        assert_eq!(at('╔'), DOUBLE << 4 | DOUBLE << 2);
        assert_eq!(at('┞'), HEAVY << 6 | LIGHT << 4 | LIGHT << 2);
        assert_eq!(at('╪'), LIGHT << 6 | DOUBLE << 4 | LIGHT << 2 | DOUBLE);
        assert_eq!(at('┄'), 0, "dashes are drawn separately");
    }

    #[test]
    fn every_line_and_block_character_is_drawn_except_diagonals() {
        if Canvas::new().is_none() {
            return;
        }
        for cp in 0x2500..=0x259F {
            let c = char::from_u32(cp).unwrap();
            let (drawn, v) = shapes(c).unwrap();
            if (0x2571..=0x2573).contains(&cp) {
                assert!(!drawn, "{c} is left to the font");
                continue;
            }
            assert!(drawn && !v.is_empty(), "{c} U+{cp:04X} is drawn");
            let (x0, y0, x1, y1) = bounds(&v);
            assert!(x0 >= 99.0 && y0 >= 199.0 && x1 <= 111.0 && y1 <= 221.0, "{c} stays in its cell: {x0} {y0} {x1} {y1}");
        }
        assert!(!shapes('A').unwrap().0, "letters use the font");
    }

    #[test]
    fn lines_reach_the_cell_edges_so_rows_and_columns_join() {
        let Some((_, v)) = shapes('│') else { return };
        assert_eq!(bounds(&v).1, 200.0);
        assert_eq!(bounds(&v).3, 220.0);
        let (_, v) = shapes('─').unwrap();
        assert_eq!((bounds(&v).0, bounds(&v).2), (100.0, 110.0));
        let (_, v) = shapes('┼').unwrap();
        assert_eq!(bounds(&v), (100.0, 200.0, 110.0, 220.0));
        // Double lines: two of each.
        let (_, v) = shapes('║').unwrap();
        assert_eq!(v.iter().filter(|i| i.size[1] > 5.0).count(), 4, "two lines, each in two halves");
    }

    #[test]
    fn blocks_fill_exact_fractions_of_the_cell() {
        let Some((_, v)) = shapes('█') else { return };
        assert_eq!((v[0].pos, v[0].size), ([100.0, 200.0], [10.0, 20.0]));
        let (_, v) = shapes('▄').unwrap();
        assert_eq!((v[0].pos, v[0].size), ([100.0, 210.0], [10.0, 10.0]));
        let (_, v) = shapes('▌').unwrap();
        assert_eq!(v[0].size, [5.0, 20.0]);
        let (_, v) = shapes('▒').unwrap();
        assert_eq!(v[0].color[3], 0.5);
        let (_, v) = shapes('▚').unwrap();
        assert_eq!(v.len(), 2, "upper left and lower right");
    }

    #[test]
    fn rounded_corners_are_an_arc_and_two_lines() {
        let Some((_, v)) = shapes('╭') else { return };
        assert_eq!(v.iter().filter(|i| i.kind >= KIND_ARC).count(), 1);
        // In a cell twice as tall as wide the arc spans the full width, so
        // only the vertical line remains.
        assert_eq!(v.iter().filter(|i| i.kind == KIND_RECT).count(), 1);
        assert_eq!(bounds(&v).3, 220.0, "continues to the bottom edge");
        assert_eq!(bounds(&v).2, 110.0, "and to the right edge");
    }
}
