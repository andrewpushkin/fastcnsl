//! ragamuffin: a fast, GPU-accelerated terminal.
#![forbid(unsafe_code)]
// No console window behind the terminal on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod chrome;
mod config;
mod detect;
mod highlight;
mod input;
mod render;
mod search;
mod session;
mod shortcuts;
mod theme;
mod ui;
mod view;

use winit::event_loop::EventLoop;

/// Version shown to users: 0.1.<commits on main>, set by the build
/// (scripts/version.sh through the Makefile or CI), else Cargo's version.
pub const VERSION: &str = match option_env!("RAGAMUFFIN_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// The project's home page.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Number of terminal columns a character occupies (1 or 2).
pub fn grid_width(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(1).max(1)
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    highlight::preload();

    let cfg = config::Config::load_or_create();
    let event_loop = match EventLoop::<session::UserEvent>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("ragamuffin: cannot open a window: {e}");
            std::process::exit(1);
        }
    };
    let mut app = app::App::new(cfg, event_loop.create_proxy());
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("ragamuffin: {e}");
        std::process::exit(1);
    }
    if let Some(e) = app.error {
        eprintln!("ragamuffin: {e}");
        std::process::exit(1);
    }
}
