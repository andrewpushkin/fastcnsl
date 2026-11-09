//! ragamuffin: a fast, GPU-accelerated terminal.
// Denied rather than forbidden so that render::dwrite (Windows) can make
// its one DirectWrite call.
#![deny(unsafe_code)]
// No console window behind the terminal on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod chrome;
mod config;
mod detect;
mod highlight;
mod input;
mod lines;
mod logging;
mod open;
mod remote;
mod render;
mod search;
mod session;
mod shortcuts;
mod theme;
mod ui;
mod view;
#[cfg(windows)]
mod wsl;

use winit::event_loop::EventLoop;

/// Version shown to users: 0.1.<commits on main>, set by the build
/// (scripts/get_version.sh through the Makefile or CI), else Cargo's version.
pub const VERSION: &str = match option_env!("RAGAMUFFIN_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// The project's home page.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// When the process started, for the startup time in the About panel.
pub static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Set once the window exists; read by the startup watchdog.
pub static WINDOW_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long to wait for a window before explaining the hang.
const STARTUP_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(10);

/// The message printed when no window appeared in time.
fn startup_hang_message() -> String {
    let display = std::env::var("WAYLAND_DISPLAY")
        .map(|w| format!("Wayland display {w}"))
        .or_else(|_| std::env::var("DISPLAY").map(|d| format!("X display {d}")))
        .unwrap_or_else(|_| "display server".to_owned());
    let mut msg = format!(
        "ragamuffin: still waiting for the {display} after {}s; it is not responding.",
        STARTUP_WATCHDOG.as_secs()
    );
    if theme::is_wsl() {
        msg.push_str(" WSLg seems stuck: run `wsl --shutdown` in Windows, then start WSL again.");
    }
    msg
}

/// Number of terminal columns a character occupies (1 or 2).
pub fn grid_width(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(1).max(1)
}

/// winit's X11 backend loads libxkbcommon-x11 at startup and panics if it
/// is missing; check first so a missing library doesn't crash the app.
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

/// Runs `ragamuffin new-tab/close-tab/quit` against a running window.
/// Returns the first tab to open when there is no window to send a
/// new-tab to, so that this process becomes the window.
fn run_command(inv: remote::Invocation) -> (Option<String>, Vec<String>) {
    match remote::send(inv.pid, &inv.command) {
        Ok(Ok(msg)) => {
            println!("{msg}");
            std::process::exit(0);
        }
        Ok(Err(msg)) => {
            eprintln!("ragamuffin: {msg}");
            std::process::exit(1);
        }
        Err(remote::SendError::NoInstance) if inv.pid.is_none() => match inv.command {
            remote::Command::NewTab { name, command } => (name, command),
            _ => {
                eprintln!("ragamuffin: no window is running");
                std::process::exit(1);
            }
        },
        Err(remote::SendError::NoInstance) => {
            eprintln!("ragamuffin: no window with process id {}", inv.pid.unwrap_or_default());
            std::process::exit(1);
        }
        Err(remote::SendError::Failed(e)) => {
            eprintln!("ragamuffin: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    STARTED.get_or_init(std::time::Instant::now);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match remote::parse_args(&args) {
        Ok(a) => a,
        Err(msg) if msg == remote::USAGE || msg == VERSION => {
            remote::attach_console();
            println!("{msg}");
            std::process::exit(0);
        }
        Err(msg) => {
            remote::attach_console();
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let first_tab = match parsed.invocation() {
        None => (None, Vec::new()),
        Some(inv) => {
            remote::attach_console();
            run_command(inv)
        }
    };
    render::glyphs::preload_fonts();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    highlight::preload();

    // A hung display server blocks window creation forever without any
    // error; say so instead of sitting there silently.
    std::thread::spawn(|| {
        std::thread::sleep(STARTUP_WATCHDOG);
        if !WINDOW_UP.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("{}", startup_hang_message());
        }
    });

    let cfg = config::Config::load_or_create();
    let mut builder = EventLoop::<session::UserEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        use config::DisplayBackend;
        use winit::platform::wayland::EventLoopBuilderExtWayland;
        use winit::platform::x11::EventLoopBuilderExtX11;
        let x11_libs = x11_keyboard_library_installed();
        if cfg.display_backend == DisplayBackend::X11 && !x11_libs {
            log::warn!("display_backend = \"x11\" needs libxkbcommon-x11 (e.g. sudo apt install libxkbcommon-x11-0); using the default");
        }
        match cfg.display_backend.resolve(x11_libs) {
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
    let mut app = app::App::new(cfg, event_loop.create_proxy(), first_tab, parsed.gpu);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("ragamuffin: {e}");
        std::process::exit(1);
    }
    if let Some(e) = app.error {
        eprintln!("ragamuffin: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hang_message_names_the_display_and_the_timeout() {
        let m = super::startup_hang_message();
        assert!(m.contains("not responding"));
        assert!(m.contains("10s"));
    }
}
