//! A running shell: PTY, the terminal state machine, and the I/O thread.
//!
//! Data path (no copies between the PTY and the renderer):
//!
//! ```text
//!  PTY fd ──read()──▶ I/O thread buffer ──vte parser──▶ Term grid (Arc<FairMutex>)
//!                                                              │ &Cell borrowed
//!                                     UI thread: lock ─▶ build GPU instances ─▶ GPU
//! ```
//!
//! The I/O thread (alacritty_terminal's `EventLoop`) parses bytes straight out
//! of its read buffer into the shared grid. The UI thread only ever borrows
//! cells under the lock; keystrokes go the other way as `Cow<'static, [u8]>`,
//! so fixed escape sequences are sent without allocating.

use std::borrow::Cow;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Notifier};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty;
use winit::event_loop::EventLoopProxy;

use crate::config::Config;

/// Events delivered to the winit event loop from other threads.
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// An event from the terminal of the tab with this id.
    Term(u64, Event),
    /// The operating system switched between dark (true) and light mode.
    OsTheme(bool),
}

/// Forwards one tab's terminal events to the UI thread.
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<UserEvent>,
    tab: u64,
}

impl EventProxy {
    pub fn new(proxy: EventLoopProxy<UserEvent>, tab: u64) -> Self {
        Self { proxy, tab }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        // The loop is only closed while the app is exiting.
        let _ = self.proxy.send_event(UserEvent::Term(self.tab, event));
    }
}

pub type SharedTerm = Arc<FairMutex<Term<EventProxy>>>;

pub struct Session {
    pub term: SharedTerm,
    notifier: Notifier,
}

impl Session {
    pub fn spawn(cfg: &Config, size: WindowSize, proxy: EventProxy) -> std::io::Result<Self> {
        static ENV: std::sync::Once = std::sync::Once::new();
        ENV.call_once(tty::setup_env);

        let term_cfg = term_config(cfg);
        let dims = TermSize::new(size.num_cols as usize, size.num_lines as usize);
        let term = Arc::new(FairMutex::new(Term::new(term_cfg, &dims, proxy.clone())));

        let shell = (!cfg.shell.is_empty())
            .then(|| tty::Shell::new(cfg.shell.clone(), cfg.shell_args.clone()));
        let options = tty::Options {
            shell,
            working_directory: dirs::home_dir(),
            drain_on_exit: true,
            env: [("TERM_PROGRAM".to_owned(), "ragamuffin".to_owned())].into(),
            #[cfg(target_os = "windows")]
            escape_args: true,
        };

        let pty = tty::new(&options, size, 0)?;
        let event_loop = EventLoop::new(term.clone(), proxy, pty, options.drain_on_exit, false)?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();

        Ok(Self { term, notifier })
    }

    /// Sends bytes to the shell. Pass `&'static [u8]` for fixed sequences to
    /// avoid an allocation.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        self.notifier.notify(bytes);
    }

    pub fn resize(&mut self, size: WindowSize) {
        let dims = TermSize::new(size.num_cols as usize, size.num_lines as usize);
        self.term.lock().resize(dims);
        self.notifier.on_resize(size);
    }

    pub fn apply_config(&self, cfg: &Config) {
        self.term.lock().set_options(term_config(cfg));
    }
}

fn term_config(cfg: &Config) -> term::Config {
    term::Config { scrolling_history: cfg.scrollback, ..Default::default() }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(alacritty_terminal::event_loop::Msg::Shutdown);
    }
}
