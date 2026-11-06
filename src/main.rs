#![forbid(unsafe_code)]

mod config;
mod detect;
mod highlight;
mod input;
mod render;
mod search;
mod session;
mod theme;

/// Number of terminal columns a character occupies (0, 1 or 2).
pub fn grid_width(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(1).max(1)
}

fn main() {}
