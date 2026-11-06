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

/// winit's X11 backend loads libxkbcommon-x11 at startup and panics if it
/// is missing; check first so `auto` can fall back to Wayland.
#[cfg(target_os = "linux")]
fn x11_keyboard_library_installed() -> bool {
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("LD_LIBRARY_PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(
        ["/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib64", "/usr/lib", "/lib/x86_64-linux-gnu", "/lib64", "/lib"]
            .map(std::path::PathBuf::from),
    );
    dirs.iter().any(|d| d.join("libxkbcommon-x11.so.0").exists() || d.join("libxkbcommon-x11.so").exists())
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    highlight::preload();

    let cfg = config::Config::load_or_create();
    let mut builder = EventLoop::<session::UserEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        use config::DisplayBackend;
        use winit::platform::wayland::EventLoopBuilderExtWayland;
        use winit::platform::x11::EventLoopBuilderExtX11;
        let wsl = theme::is_wsl();
        let x11_libs = x11_keyboard_library_installed();
        if wsl && !x11_libs && cfg.display_backend == DisplayBackend::Auto {
            log::warn!(
                "WSL: libxkbcommon-x11 is not installed, so using Wayland, where WSLg hides the mouse \
                 cursor until a click. Fix: sudo apt install libxkbcommon-x11-0"
            );
        }
        match cfg.display_backend.resolve(wsl, x11_libs) {
            Some(DisplayBackend::X11) => {
                builder.with_x11();
            }
            Some(DisplayBackend::Wayland) => {
                builder.with_wayland();
            }
            _ => {}
        }
    }
    let event_loop = match builder.build() {
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
