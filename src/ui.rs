//! Overlay widgets: the scrollbar and the search bar. Both rest at a
//! configurable idle opacity and fade to fully opaque while in use.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Opacity that eases toward a target over ~150 ms.
pub struct Fade {
    pub value: f32,
    last: Instant,
}

const FADE_PER_SEC: f32 = 1.0 / 0.15;

impl Fade {
    pub fn new(value: f32) -> Self {
        Self { value, last: Instant::now() }
    }

    /// Advances toward `target`; returns true while still moving.
    pub fn update(&mut self, target: f32) -> bool {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;
        let step = FADE_PER_SEC * dt;
        if (self.value - target).abs() <= step {
            self.value = target;
            false
        } else {
            self.value += step.copysign(target - self.value);
            true
        }
    }
}

/// How long the scrollbar stays fully visible after scrolling.
pub const SCROLL_ACTIVE_FOR: Duration = Duration::from_millis(900);

pub struct Scrollbar {
    pub hover: bool,
    /// Distance from the thumb's top to the pointer while dragging.
    pub drag: Option<f32>,
    pub last_scroll: Option<Instant>,
    pub fade: Fade,
}

impl Scrollbar {
    pub fn new(idle: f32) -> Self {
        Self { hover: false, drag: None, last_scroll: None, fade: Fade::new(idle) }
    }

    pub fn active(&self) -> bool {
        self.hover || self.drag.is_some() || self.last_scroll.is_some_and(|t| t.elapsed() < SCROLL_ACTIVE_FOR)
    }
}

pub struct ScrollGeometry {
    pub track: Rect,
    pub thumb: Rect,
}

/// Width reserved at the right edge for the scrollbar, in physical pixels.
pub fn scrollbar_width(scale: f32) -> f32 {
    (10.0 * scale).round()
}

/// Scrollbar geometry for the terminal area starting at `top`, or None
/// when there is nothing to scroll.
#[allow(clippy::too_many_arguments)]
pub fn scroll_geometry(width: f32, top: f32, height: f32, scale: f32, history: usize, screen: usize, offset: usize) -> Option<ScrollGeometry> {
    if history == 0 {
        return None;
    }
    let w = scrollbar_width(scale);
    let margin = (2.0 * scale).round();
    let track = Rect { x: width - w, y: top + margin, w, h: height - top - 2.0 * margin };
    let total = (history + screen) as f32;
    let thumb_h = (track.h * screen as f32 / total).max(24.0 * scale).min(track.h);
    let frac = 1.0 - offset as f32 / history as f32;
    let thumb_y = track.y + (track.h - thumb_h) * frac;
    let inset = (2.0 * scale).round();
    let thumb = Rect { x: track.x + inset, y: thumb_y, w: w - 2.0 * inset, h: thumb_h };
    Some(ScrollGeometry { track, thumb })
}

/// Display offset (lines scrolled back) for a thumb top at `thumb_y`.
pub fn offset_for_thumb(g: &ScrollGeometry, thumb_y: f32, history: usize) -> usize {
    let range = (g.track.h - g.thumb.h).max(1.0);
    let frac = ((thumb_y - g.track.y) / range).clamp(0.0, 1.0);
    ((1.0 - frac) * history as f32).round() as usize
}

/// What in the search bars is under the pointer. The numbers are box
/// indices (and for Swatch, the colour).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchHit {
    /// The box itself, away from its buttons.
    Bar(usize),
    /// The colour button at its left end.
    Color(usize),
    /// A colour choice, while the box shows them.
    Swatch(usize, usize),
    Prev(usize),
    Next(usize),
    Add(usize),
    Close(usize),
}

pub struct SearchBoxGeometry {
    pub bar: Rect,
    pub color: Rect,
    /// Previous (older, up) and next (newer, down) match buttons.
    pub prev: Rect,
    pub next: Rect,
    /// Adds a box below this one.
    pub add: Rect,
    pub close: Rect,
}

pub struct SearchGeometry {
    pub boxes: Vec<SearchBoxGeometry>,
    /// The box showing its colour choices, and where they are.
    pub picker: Option<usize>,
    pub swatches: Vec<Rect>,
}

impl SearchGeometry {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.boxes.iter().any(|b| b.bar.contains(x, y))
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<SearchHit> {
        let i = self.boxes.iter().position(|b| b.bar.contains(x, y))?;
        let g = &self.boxes[i];
        if self.picker == Some(i)
            && let Some(c) = self.swatches.iter().position(|r| r.contains(x, y))
        {
            return Some(SearchHit::Swatch(i, c));
        }
        let buttons = [
            (g.color, SearchHit::Color(i)),
            (g.prev, SearchHit::Prev(i)),
            (g.next, SearchHit::Next(i)),
            (g.add, SearchHit::Add(i)),
            (g.close, SearchHit::Close(i)),
        ];
        Some(buttons.iter().find(|(r, _)| r.contains(x, y)).map_or(SearchHit::Bar(i), |(_, h)| *h))
    }
}

/// Search bars in the top-right corner of the terminal area, one under
/// another, sized in cells. `picker` is the box showing its colour
/// choices in place of its query.
pub fn search_geometry(width: f32, top: f32, scale: f32, cell_w: f32, cell_h: f32, count: usize, picker: Option<usize>) -> SearchGeometry {
    let margin = (8.0 * scale).round();
    let pad = (6.0 * scale).round();
    let gap = (4.0 * scale).round();
    let w = (cell_w * 58.0 + 2.0 * pad).min(width - 2.0 * margin - scrollbar_width(scale)).max(0.0);
    let h = cell_h + 2.0 * pad;
    let x = width - w - margin - scrollbar_width(scale);
    // Square-ish buttons: the colour at the left end, ↑ ↓ + × at the right.
    let inset = (3.0 * scale).round();
    let bw = (cell_h + 2.0 * inset).round();
    let boxes: Vec<SearchBoxGeometry> = (0..count.max(1))
        .map(|i| {
            let bar = Rect { x, y: top + margin + i as f32 * (h + gap), w, h };
            let button = |slot: f32| Rect { x: bar.x + bar.w - inset - (4.0 - slot) * bw, y: bar.y + inset, w: bw, h: h - 2.0 * inset };
            let color = Rect { x: bar.x + inset, y: bar.y + inset, w: bw, h: h - 2.0 * inset };
            SearchBoxGeometry { bar, color, prev: button(0.0), next: button(1.0), add: button(2.0), close: button(3.0) }
        })
        .collect();
    let mut swatches = Vec::new();
    if let Some(g) = picker.and_then(|i| boxes.get(i)) {
        let mut sx = g.color.x + g.color.w + inset;
        for _ in 0..crate::theme::SEARCH_COLORS {
            if sx + bw > g.prev.x - inset {
                break;
            }
            swatches.push(Rect { x: sx, y: g.color.y, w: bw, h: g.color.h });
            sx += bw + inset;
        }
    }
    SearchGeometry { boxes, picker: picker.filter(|&i| i < count.max(1)), swatches }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_at_bottom_when_not_scrolled_and_top_when_fully_scrolled() {
        let g = scroll_geometry(800.0, 0.0, 600.0, 1.0, 1000, 30, 0).unwrap();
        assert!((g.thumb.y + g.thumb.h - (g.track.y + g.track.h)).abs() < 0.01);
        let g = scroll_geometry(800.0, 0.0, 600.0, 1.0, 1000, 30, 1000).unwrap();
        assert!((g.thumb.y - g.track.y).abs() < 0.01);
    }

    #[test]
    fn thumb_position_round_trips() {
        let g = scroll_geometry(800.0, 0.0, 600.0, 1.0, 1000, 30, 400).unwrap();
        assert_eq!(offset_for_thumb(&g, g.thumb.y, 1000), 400);
    }

    #[test]
    fn no_scrollbar_without_history() {
        assert!(scroll_geometry(800.0, 0.0, 600.0, 1.0, 0, 30, 0).is_none());
    }

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect { x: 10.0, y: 10.0, w: 5.0, h: 5.0 };
        assert!(r.contains(10.0, 10.0));
        assert!(r.contains(14.9, 14.9));
        assert!(!r.contains(15.0, 12.0));
        assert!(!r.contains(9.9, 12.0));
    }

    #[test]
    fn scrollbar_sits_below_the_tab_bar_and_thumb_has_a_minimum() {
        let g = scroll_geometry(800.0, 30.0, 600.0, 1.0, 1_000_000, 30, 0).unwrap();
        assert!(g.track.y >= 30.0);
        assert!(g.thumb.h >= 24.0, "huge history still leaves a grabbable thumb");
        assert_eq!(g.track.x + g.track.w, 800.0);
        // Dragging past either end clamps.
        assert_eq!(offset_for_thumb(&g, -500.0, 1000), 1000);
        assert_eq!(offset_for_thumb(&g, 5000.0, 1000), 0);
    }

    #[test]
    fn search_bar_is_right_aligned_below_the_tab_bar() {
        let g = search_geometry(1000.0, 30.0, 1.0, 8.0, 17.0, 1, None);
        let b = &g.boxes[0];
        assert!(b.bar.y >= 30.0);
        assert!(b.bar.x + b.bar.w <= 1000.0 - scrollbar_width(1.0));
        for r in [b.color, b.prev, b.next, b.add, b.close] {
            assert!(r.x >= b.bar.x && r.x + r.w <= b.bar.x + b.bar.w + 0.01);
            assert!(r.y >= b.bar.y && r.y + r.h <= b.bar.y + b.bar.h + 0.01);
        }
        assert!(b.color.x + b.color.w < b.prev.x, "colour at the left");
        assert!(b.prev.x + b.prev.w <= b.next.x && b.next.x + b.next.w <= b.add.x && b.add.x + b.add.w <= b.close.x, "↑ ↓ + × in order");
        assert!(g.swatches.is_empty());
        // Narrow windows shrink the bar instead of overflowing.
        let narrow = search_geometry(200.0, 0.0, 1.0, 8.0, 17.0, 1, None);
        assert!(narrow.boxes[0].bar.x >= 0.0);
    }

    #[test]
    fn search_boxes_stack_and_hit_testing_finds_each_part() {
        let g = search_geometry(1000.0, 30.0, 1.0, 8.0, 17.0, 3, Some(1));
        assert_eq!(g.boxes.len(), 3);
        for w in g.boxes.windows(2) {
            assert!(w[0].bar.y + w[0].bar.h < w[1].bar.y, "one under another, apart");
            assert_eq!(w[0].bar.x, w[1].bar.x);
        }
        let mid = |r: Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let hit = |r: Rect| {
            let (x, y) = mid(r);
            g.hit(x, y)
        };
        assert_eq!(hit(g.boxes[2].close), Some(SearchHit::Close(2)));
        assert_eq!(hit(g.boxes[0].add), Some(SearchHit::Add(0)));
        assert_eq!(hit(g.boxes[1].prev), Some(SearchHit::Prev(1)));
        assert_eq!(hit(g.boxes[0].color), Some(SearchHit::Color(0)));
        // Box 1 shows its colour choices, inside it and clear of ↑.
        assert_eq!(g.swatches.len(), 8);
        for (c, r) in g.swatches.iter().enumerate() {
            assert!(r.x > g.boxes[1].color.x && r.x + r.w < g.boxes[1].prev.x && r.y >= g.boxes[1].bar.y);
            assert_eq!(hit(*r), Some(SearchHit::Swatch(1, c)));
        }
        let (x, y) = mid(g.swatches[3]);
        assert_eq!(g.hit(x, y - (g.boxes[1].bar.y - g.boxes[0].bar.y)), Some(SearchHit::Bar(0)), "no choices in box 0");
        assert_eq!(g.hit(5.0, 5.0), None);
        assert!(g.contains(x, y) && !g.contains(5.0, 5.0));
        // Fewer choices fit in a narrow bar, never past ↑.
        let narrow = search_geometry(300.0, 0.0, 1.0, 8.0, 17.0, 1, Some(0));
        assert!(narrow.swatches.len() < 8);
        assert!(narrow.swatches.iter().all(|r| r.x + r.w < narrow.boxes[0].prev.x));
    }

    #[test]
    fn scrollbar_is_active_while_used() {
        let mut s = Scrollbar::new(0.4);
        assert!(!s.active());
        s.hover = true;
        assert!(s.active());
        s.hover = false;
        s.drag = Some(3.0);
        assert!(s.active());
        s.drag = None;
        s.last_scroll = Some(Instant::now());
        assert!(s.active());
        s.last_scroll = Some(Instant::now() - SCROLL_ACTIVE_FOR - Duration::from_millis(1));
        assert!(!s.active());
    }

    #[test]
    fn fade_reaches_target() {
        let mut f = Fade::new(0.0);
        std::thread::sleep(Duration::from_millis(110));
        f.update(1.0);
        std::thread::sleep(Duration::from_millis(110));
        assert!(!f.update(1.0) || f.value > 0.9);
    }
}
