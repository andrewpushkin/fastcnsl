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

pub struct SearchGeometry {
    pub bar: Rect,
    pub close: Rect,
}

/// Search bar in the top-right corner of the terminal area, sized in cells.
pub fn search_geometry(width: f32, top: f32, scale: f32, cell_w: f32, cell_h: f32) -> SearchGeometry {
    let margin = (8.0 * scale).round();
    let pad = (6.0 * scale).round();
    let w = (cell_w * 44.0 + 2.0 * pad).min(width - 2.0 * margin - scrollbar_width(scale));
    let h = cell_h + 2.0 * pad;
    let bar = Rect { x: width - w - margin - scrollbar_width(scale), y: top + margin, w, h };
    let close = Rect { x: bar.x + bar.w - pad - cell_w * 2.0, y: bar.y, w: cell_w * 2.0 + pad, h };
    SearchGeometry { bar, close }
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
        let g = search_geometry(1000.0, 30.0, 1.0, 8.0, 17.0);
        assert!(g.bar.y >= 30.0);
        assert!(g.bar.x + g.bar.w <= 1000.0 - scrollbar_width(1.0));
        assert!(g.close.x >= g.bar.x && g.close.x + g.close.w <= g.bar.x + g.bar.w + 0.01);
        // Narrow windows shrink the bar instead of overflowing.
        let narrow = search_geometry(200.0, 0.0, 1.0, 8.0, 17.0);
        assert!(narrow.bar.x >= 0.0);
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
