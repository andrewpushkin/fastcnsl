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
    /// A command from `ragamuffin new-tab/close-tab/quit`, and where to
    /// send the answer.
    Remote(crate::remote::Command, std::sync::mpsc::Sender<crate::remote::Reply>),
    /// The operating system switched between dark (true) and light mode.
    /// Only sent on Linux; elsewhere winit reports theme changes itself.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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

/// A shell session. `L` receives the terminal's events: the window's
/// [`EventProxy`] in the app, a channel in tests.
pub struct Session<L: EventListener = EventProxy> {
    pub term: Arc<FairMutex<Term<L>>>,
    notifier: Notifier,
}

impl<L: EventListener + Clone + Send + 'static> Session<L> {
    pub fn spawn(cfg: &Config, size: WindowSize, proxy: L) -> std::io::Result<Self> {
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

impl<L: EventListener> Drop for Session<L> {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(alacritty_terminal::event_loop::Msg::Shutdown);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use alacritty_terminal::grid::Dimensions;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Collects terminal events on a channel.
    #[derive(Clone)]
    struct Events(mpsc::Sender<Event>);

    impl EventListener for Events {
        fn send_event(&self, e: Event) {
            let _ = self.0.send(e);
        }
    }

    fn size(cols: u16, lines: u16) -> WindowSize {
        WindowSize { num_cols: cols, num_lines: lines, cell_width: 8, cell_height: 16 }
    }

    fn spawn(shell: &str, args: &[&str]) -> (Session<Events>, mpsc::Receiver<Event>) {
        let cfg = Config { shell: shell.into(), shell_args: args.iter().map(|s| s.to_string()).collect(), ..Config::default() };
        let (tx, rx) = mpsc::channel();
        (Session::spawn(&cfg, size(60, 10), Events(tx)).expect("spawn shell"), rx)
    }

    fn screen_text<L: EventListener>(s: &Session<L>) -> String {
        let term = s.term.lock();
        let mut out = String::new();
        for line in 0..term.grid().screen_lines() {
            let row = &term.grid()[alacritty_terminal::index::Line(line as i32)];
            for col in 0..term.grid().columns() {
                out.push(row[alacritty_terminal::index::Column(col)].c);
            }
            out.push('\n');
        }
        out
    }

    /// Waits until the screen contains `needle` at least `times` times, or
    /// panics after 5 s.
    fn wait_for_n<L: EventListener>(s: &Session<L>, rx: &mpsc::Receiver<Event>, needle: &str, times: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if screen_text(s).matches(needle).count() >= times {
                return;
            }
            let _ = rx.recv_timeout(Duration::from_millis(50));
        }
        panic!("{needle:?} didn't appear {times}×; screen:\n{}", screen_text(s));
    }

    fn wait_for<L: EventListener>(s: &Session<L>, rx: &mpsc::Receiver<Event>, needle: &str) {
        wait_for_n(s, rx, needle, 1);
    }

    #[test]
    fn shell_output_reaches_the_grid_and_exit_is_reported() {
        let (s, rx) = spawn("/bin/sh", &["-c", "printf 'hello from the pty'; sleep 0.5"]);
        wait_for(&s, &rx, "hello from the pty");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while !exited && Instant::now() < deadline {
            exited = matches!(rx.recv_timeout(Duration::from_millis(100)), Ok(Event::ChildExit(_) | Event::Exit));
        }
        assert!(exited, "child exit is reported");
    }

    #[test]
    fn keystrokes_reach_the_program() {
        let (s, rx) = spawn("/bin/cat", &[]);
        s.write(&b"ping\r"[..]);
        s.write(b"pong\r".to_vec());
        // The terminal echoes each line as it's typed, and cat repeats it
        // later: wait for cat's copy of the last line.
        wait_for_n(&s, &rx, "pong", 2);
        assert!(screen_text(&s).matches("ping").count() >= 2);
    }

    #[test]
    fn resize_changes_the_grid_and_the_pty() {
        let (mut s, rx) = spawn("/bin/sh", &[]);
        s.resize(size(33, 7));
        {
            let term = s.term.lock();
            assert_eq!(term.grid().columns(), 33);
            assert_eq!(term.grid().screen_lines(), 7);
        }
        // The program sees the new size through the PTY.
        s.write(&b"stty size\r"[..]);
        wait_for(&s, &rx, "7 33");
    }

    #[test]
    fn scrollback_setting_applies_live() {
        let (s, rx) = spawn("/bin/sh", &["-c", "for i in $(seq 1 100); do echo line$i; done; sleep 2"]);
        wait_for(&s, &rx, "line100");
        assert!(s.term.lock().grid().history_size() > 50);
        s.apply_config(&Config { scrollback: 10, ..Config::default() });
        assert!(s.term.lock().grid().history_size() <= 10);
    }
}
