//! Window, tabs and event handling.
//!
//! Every user command (shortcut, ⋮ menu, right-click menu, Options panel
//! button) becomes an [`Action`] and goes through [`App::perform`]. The
//! shortcut table shown to users lives in `shortcuts.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{ClipboardType, Term, TermMode};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton as WinitButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Theme, UserAttentionType, Window, WindowId};

use crate::ai::{self, AiEvent};
use crate::chat_ui::{self, ChatPanel};
use crate::chrome::{self, Action, BookmarkMenu, Caption, Menu, Metrics, OptionsPage, OptionsState, Panel};
use crate::config::{Config, ConfigWatcher, GpuBackend, Setting, ThemeMode};
use crate::detect;
use crate::highlight::{self, CodeHighlighter};
use crate::history::scroll::Scroller;
use crate::history::search::Searcher;
use crate::input::{self, MouseButton};
use crate::lines::{self, LineIndex};
use crate::logging::{self, TabLog, Upto};
use crate::remote;
use crate::render::Renderer;
use crate::search::{Across, Search};
use crate::shortcuts::{Combo, Command, Keymap};
use crate::session::{EventProxy, Session, UserEvent};
use crate::theme::{self, Palette};
use crate::ui::{self, Fade, Scrollbar, SearchHit};
use crate::view::{self, ChromeView, Hover, View};

const TITLE: &str = "fastcnsl";
const INITIAL_COLS: u32 = 100;
const INITIAL_LINES: u32 = 30;
const CONFIG_POLL: Duration = Duration::from_secs(1);
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// How long a notice ("Saved to ...") stays up.
const TOAST_FOR: Duration = Duration::from_secs(5);
/// How long the grid size ("100x30") stays up after launch or a resize.
const SIZE_FOR: Duration = Duration::from_secs(5);
/// How long after a resize output is taken for shells repainting their
/// prompts for the new size, so it doesn't mark background tabs as active.
const RESIZE_REPAINT: Duration = Duration::from_secs(1);

/// Font size in points → pixels at the given DPI scale.
fn font_px(points: f32, scale: f64) -> f32 {
    points * scale as f32 * 96.0 / 72.0
}

/// The panels' font size in pixels: the default terminal font size, so it
/// changes with the window scale only, not with the font settings.
fn ui_font_px(scale: f64) -> f32 {
    font_px(crate::config::default_font_size(), scale)
}

pub struct App {
    cfg: Config,
    watcher: ConfigWatcher,
    proxy: EventLoopProxy<UserEvent>,
    state: Option<State>,
    next_poll: Instant,
    pub error: Option<String>,
    /// Name and command of the first tab (from `fastcnsl new-tab`).
    first_tab: (Option<String>, Vec<String>),
    /// Listens for `fastcnsl new-tab/close-tab/quit`.
    _remote: Option<remote::Server>,
    /// Graphics API from `--gpu`, for this run only.
    gpu_override: Option<GpuBackend>,
}

/// Whether output at `now` is still shells repainting after the resize at
/// `resized_at`, rather than something a program printed.
fn repainting_after_resize(resized_at: Option<Instant>, now: Instant) -> bool {
    resized_at.is_some_and(|t| now.duration_since(t) < RESIZE_REPAINT)
}

/// A tab's text and bookmarks as the AI's tools see them (see
/// `ai::context`): a settled snapshot of its history, its live grid, and
/// its line index. `end` is the prompt's line.
struct TabText<'a> {
    snap: &'a crate::history::Snapshot,
    grid: &'a alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>,
    lines: &'a mut LineIndex,
    end: u64,
}

impl ai::context::TerminalText for TabText<'_> {
    fn lines(&mut self, r: std::ops::Range<u64>) -> Vec<String> {
        logging::lines_text(self.snap, self.grid, r)
    }
    fn end(&self) -> u64 {
        self.end
    }
    fn commands(&self) -> Vec<(u64, u64)> {
        self.lines.commands().collect()
    }
    fn bookmarks(&self) -> Vec<u64> {
        self.lines.bookmarks().to_vec()
    }
    fn add_bookmark(&mut self, line: u64) {
        if self.lines.bookmarks().binary_search(&line).is_err() {
            self.lines.toggle_bookmark(line);
        }
    }
    fn first_line(&self) -> u64 {
        self.snap.first_line().min(self.grid.lines_dropped())
    }
}

/// One shell with its own scrollback, search and code highlighting.
struct Tab {
    id: u64,
    session: Session,
    search: Search,
    code: CodeHighlighter,
    /// Command lines, bookmarks and line times, for the gutter and jumps.
    lines: LineIndex,
    /// The gutter's numbers or times on screen, rebuilt each frame.
    numbers: Vec<Option<u64>>,
    /// The screen rows of bookmarked lines, rebuilt each frame.
    bookmarked: Vec<usize>,
    /// The line a bookmark jump landed on and when, highlighted for a moment.
    flash: Option<(u64, Instant)>,
    /// A bookmark jump's scroll while it is under way.
    glide: Option<Glide>,
    /// Last program started in this tab (the shell's name until then).
    app: String,
    /// Name given with `fastcnsl new-tab NAME`; shown instead of `app`.
    name: Option<String>,
    /// "Tab N: app", rebuilt when `app` or the tab's position changes.
    label: String,
    /// Title the shell set with an escape sequence, used for the window.
    shell_title: Option<String>,
    /// The file this tab is being logged to.
    log: Option<TabLog>,
    /// Printed something since it was last the selected tab (a bell on it).
    activity: bool,
    /// What is shown: the live terminal or a window onto older lines.
    scroller: Scroller<EventProxy>,
    /// `scroller.changes()` when line positions were last computed.
    shown_changes: u64,
    /// Searches through the history, on their own threads.
    searcher: Searcher,
    /// `scroller.changes()` when search matches were last placed.
    search_view: u64,
    /// The log is still writing lines from history (see `TabLog::write_new`).
    log_behind: bool,
    /// This tab's grid: narrower than the window while its chat is open.
    grid: WindowSize,
    /// The AI chat beside this tab.
    chat: ChatPanel,
}

impl Tab {
    /// Runs `f` on the terminal shown: the live one, or the window onto
    /// older lines when scrolled back that far.
    fn shown<R>(&mut self, f: impl FnOnce(&mut Term<EventProxy>) -> R) -> R {
        let mut live = self.session.term.lock();
        f(self.scroller.shown_mut(&mut live))
    }

    /// Runs `f` on the search and the terminal shown, then goes on into
    /// the history where that found nothing.
    fn search_with<R>(&mut self, f: impl FnOnce(&mut Search, &mut Term<EventProxy>) -> R) -> R {
        if self.scroller.changes() != self.search_view {
            self.search.forget_current();
        }
        let r = {
            let mut live = self.session.term.lock();
            f(&mut self.search, self.scroller.shown_mut(&mut live))
        };
        self.search_follow_up();
        r
    }

    /// See `Search::follow_up`.
    fn search_follow_up(&mut self) {
        let mut live = self.session.term.lock();
        let mut a = Across {
            live: &mut live,
            scroller: &mut self.scroller,
            history: &self.session.history,
            budget: self.session.budget(),
            searcher: &mut self.searcher,
        };
        self.search.follow_up(&mut a);
        self.search_view = self.scroller.changes();
    }

    /// The history searcher has results.
    fn search_results(&mut self) {
        let mut live = self.session.term.lock();
        let mut a = Across {
            live: &mut live,
            scroller: &mut self.scroller,
            history: &self.session.history,
            budget: self.session.budget(),
            searcher: &mut self.searcher,
        };
        self.search.on_results(&mut a);
        self.search_view = self.scroller.changes();
    }

    /// Scrolls like `Term::scroll_display`, through the whole history.
    fn scroll(&mut self, s: Scroll) {
        let mut live = self.session.term.lock();
        let budget = self.session.budget();
        let history = &self.session.history;
        match s {
            Scroll::Delta(n) => self.scroller.scroll(&mut live, history, &budget, i64::from(n)),
            Scroll::PageUp => self.scroller.page(&mut live, history, &budget, true),
            Scroll::PageDown => self.scroller.page(&mut live, history, &budget, false),
            Scroll::Top => self.scroller.go_top(&mut live, history, &budget),
            Scroll::Bottom => self.scroller.go_bottom(&mut live),
        }
    }

    /// For the scrollbar: history rows and how far up the view is.
    fn scroll_metrics(&self) -> (usize, usize) {
        self.scroller.metrics(&self.session.term.lock(), &self.session.history)
    }

    /// Moves a bookmark jump's scroll on a frame; true while it goes on.
    /// It stops where it is if something else scrolled the view.
    fn glide_step(&mut self, live: &mut Term<EventProxy>) -> bool {
        let Some(g) = self.glide.as_mut() else { return false };
        let history = &self.session.history;
        let (total, offset) = self.scroller.metrics(live, history);
        if total - offset != g.seen {
            self.glide = None;
            return false;
        }
        let budget = self.session.budget();
        let t = g.start.elapsed().as_secs_f32() / GLIDE_FOR.as_secs_f32();
        if t >= 1.0 {
            let line = g.line;
            self.glide = None;
            self.scroller.show_line(live, history, &budget, line);
            return false;
        }
        let at = g.from as f32 + (g.to as f32 - g.from as f32) * ease_out(t);
        self.scroller.set_offset(live, history, &budget, total - (at.round() as usize).min(total));
        let (total, offset) = self.scroller.metrics(live, history);
        g.seen = total - offset;
        true
    }

    /// Scrollbar drag to `offset` (in the units of `scroll_metrics`).
    fn set_scroll_offset(&mut self, offset: usize) {
        let mut live = self.session.term.lock();
        let budget = self.session.budget();
        self.scroller.set_offset(&mut live, &self.session.history, &budget, offset);
    }

    /// One pass of the log, if there is one: from history while it is
    /// behind. Returns true while there is more to write from history.
    fn log_pass(&mut self, upto: Upto) -> std::io::Result<bool> {
        let Some(log) = self.log.as_mut() else { return Ok(false) };
        let mut term = self.session.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return Ok(false);
        }
        let snap = log.needs_history(term.grid(), upto).then(|| crate::history::scroll::settled(&mut term, &self.session.history));
        log.write_new(snap.as_ref(), term.grid(), upto)
    }

    /// A settled snapshot of the history and the live grid's text.
    fn text_snapshot(&self) -> (crate::history::Snapshot, String) {
        let mut term = self.session.term.lock();
        let snap = crate::history::scroll::settled(&mut term, &self.session.history);
        (snap, logging::all_text(term.grid()))
    }

    /// Drops a stale window (history cleared, size changed).
    fn sync_scroller(&mut self) {
        let mut live = self.session.term.lock();
        let budget = self.session.budget();
        self.scroller.sync(&mut live, &self.session.history, &budget);
    }

    /// Forgets line positions cached for a terminal other than the one
    /// shown now (a window built or dropped since): they don't hold in it.
    /// Enter pressed at the prompt, with `typed` (if any) still on its way
    /// to the shell: records the command for the gutter, code colouring
    /// and the log. Returns the global line Enter was on, and the program
    /// the command starts if it is another one.
    fn note_enter(&mut self, typed: &str) -> (u64, Option<String>) {
        let term = self.session.term.lock();
        let mut line = highlight::command_line(term.grid());
        if !typed.is_empty() && !line.trim_end().ends_with(typed) {
            line = format!("{} {typed}", line.trim_end());
        }
        self.code.on_enter(term.grid(), &line);
        let at = self.lines.cursor_line(term.grid());
        self.lines.on_enter(term.grid());
        let ran = detect::program_name(&line).filter(|name| *name != self.app);
        drop(term);
        // The command line goes into the log as entered.
        if let Err(e) = self.log_pass(Upto::Cursor) {
            log::warn!("logging: {e}");
        }
        (at, ran)
    }

    fn sync_lines(&mut self) {
        if self.scroller.changes() != self.shown_changes {
            self.shown_changes = self.scroller.changes();
            self.lines.invalidate();
        }
    }
}

enum Overlay {
    None,
    Menu(Menu),
    Options,
    About,
    /// Asks before a Fix or a chat message first sends terminal text to
    /// the provider.
    AiConfirm(Confirm),
    /// "You're using your ChatGPT plan", once after the first sign-in.
    AiWelcome,
}

/// What the AI confirm panel lets go on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Confirm {
    /// A Fix of the command on this global line, or the last one.
    Fix(Option<u64>),
    /// The chat message in the active tab's input box.
    Chat,
}

/// A sign-in under way: its job, provider, the flag that cancels it, and
/// (Grok) the code to enter and xAI's page with it filled in.
struct SignIn {
    job: u64,
    provider: ai::Provider,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    code: Option<(String, String)>,
}

/// Who answers, for notices: the provider, or the plan it spends.
fn ai_label(client: &ai::client::Client, provider: ai::Provider) -> &'static str {
    match (client.on_plan(), provider) {
        (true, ai::Provider::Xai) => "Grok (your plan)",
        (true, _) => "ChatGPT (your plan)",
        (false, p) => p.label(),
    }
}

/// The email signed in to ChatGPT, if signed in.
fn chatgpt_account() -> Option<String> {
    ai::login::Login::path().map(|p| ai::login::Login::load(&p)).filter(|l| l.signed_in()).map(|l| l.email)
}

/// The email signed in to Grok, if signed in.
fn grok_account() -> Option<String> {
    ai::grok::GrokLogin::path().map(|p| ai::grok::GrokLogin::load(&p)).filter(|l| l.signed_in()).map(|l| l.email)
}

struct State {
    window: Arc<Window>,
    renderer: Renderer,
    proxy: EventLoopProxy<UserEvent>,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
    overlay: Overlay,
    hover: Hover,
    palette: Palette,
    os_dark: bool,
    theme_mode: ThemeMode,
    scrollbar: Scrollbar,
    /// When the grid last changed size, see `RESIZE_REPAINT`.
    resized_at: Option<Instant>,
    search_fade: Fade,
    search_hover: bool,
    mods: ModifiersState,
    mouse: (f32, f32),
    selecting: bool,
    last_click: Option<(Instant, Point)>,
    click_count: u8,
    reported_button: Option<MouseButton>,
    last_reported_cell: Option<(usize, usize)>,
    wheel_accum: f32,
    clipboard: Option<arboard::Clipboard>,
    focused: bool,
    font_pt: f32,
    grid: WindowSize,
    /// Redraw again at this time (scrollbar fade-out).
    wake_at: Option<Instant>,
    /// Window buttons are in the tab bar rather than an OS title bar.
    custom_title_bar: bool,
    /// Logs being finished from history on their own threads.
    finishing_logs: Vec<std::thread::JoinHandle<()>>,
    /// For double-click on the empty tab bar (maximize).
    last_bar_click: Option<Instant>,
    /// Cursor last sent to the window system.
    cursor: CursorIcon,
    /// Time from launch to the first frame, for the About panel.
    startup: String,
    /// The page the Options panel shows.
    options_page: OptionsPage,
    /// Options → Font: the installed monospace families (read when the
    /// page opens) and the first one its list shows.
    font_families: std::sync::Arc<[String]>,
    font_scroll: usize,
    /// Graphics API asked for at startup (it changes only on a restart).
    gpu_started: GpuBackend,
    frames: FrameStats,
    /// Keyboard shortcuts in effect.
    keymap: Keymap,
    /// Options → shortcuts: the command waiting for new keys.
    capturing: Option<Command>,
    /// New `[keys]` table from a key press while capturing, applied by
    /// Action::ApplyKeys (actions are Copy; the table isn't).
    pending_keys: Option<std::collections::BTreeMap<String, String>>,
    /// How far the open panel has been dragged from the centre.
    panel_offset: (f32, f32),
    /// While dragging a panel by its header: the pointer and offset at the
    /// start of the drag.
    panel_drag: Option<((f32, f32), (f32, f32))>,
    /// While dragging the chat panel's divider: the panel's share of the
    /// window, saved to `[ai] panel_width` on release.
    chat_drag: Option<f32>,
    /// When the last frame finished, and how long it took to build and
    /// present: output redraws are paced by these.
    last_frame: Instant,
    frame_cost: Duration,
    /// A notice at the bottom of the window, and when it goes.
    toast: Option<(String, Instant)>,
    /// The grid size shown in the bottom right corner until then.
    size_until: Option<Instant>,
    /// The Fix being waited for: its job number and tab id.
    ai_job: Option<(u64, u64)>,
    /// That Fix's provider (for the notice), what is left of the terminal
    /// text it may send, and the bookmarks (global lines) the AI set.
    ai_label: &'static str,
    ai_budget: ai::context::Budget,
    ai_bookmarked: Vec<u64>,
    /// Job numbers for AI requests; answers to older ones are dropped.
    next_ai_job: u64,
    /// API keys (`ai-keys.toml`).
    ai_keys: ai::Keys,
    /// Options → AI: the models the key can use, the first one its list
    /// shows, the line about the last request, and the model-list and
    /// Test requests being waited for.
    ai_models: std::sync::Arc<[String]>,
    ai_scroll: usize,
    ai_status: String,
    ai_models_job: Option<u64>,
    ai_test_job: Option<u64>,
    /// The model chosen when the list was asked for, to scroll to it.
    ai_model_shown: String,
    /// The emails signed in to ChatGPT and to Grok ("" when the provider
    /// didn't say), and a sign-in under way.
    ai_account: Option<String>,
    ai_grok_account: Option<String>,
    ai_signin: Option<SignIn>,
    /// The sign-in changed: the model (named differently on a plan) must be
    /// chosen again. Done by `App`, which owns the settings.
    ai_reset_model: bool,
}

/// Fastest frame rate while output streams in (120/s).
const MIN_FRAME_GAP: Duration = Duration::from_millis(8);
/// While output streams in, frames start no sooner than this many times
/// the last frame's cost after it: drawing then takes at most a third of
/// the time, leaving the CPU to the program and the terminal. Matters most
/// for software rendering, which competes with them for the same cores.
const FRAME_COST_FACTOR: u32 = 3;

/// Frame timings, logged every 2 s at debug level (RUST_LOG=fastcnsl=debug).
struct FrameStats {
    since: Instant,
    count: u32,
    /// Time the terminal was locked to build frames: the reading thread waits.
    locked: Duration,
    present: Duration,
    /// Output notifications and replies to the program, since `since`.
    wakeups: u32,
    replies: u32,
}

impl FrameStats {
    fn new() -> Self {
        Self { since: Instant::now(), count: 0, locked: Duration::ZERO, present: Duration::ZERO, wakeups: 0, replies: 0 }
    }

    fn add(&mut self, locked: Duration, present: Duration) {
        self.count += 1;
        self.locked += locked;
        self.present += present;
        self.report();
    }

    /// Output arrived (whether or not a frame follows).
    fn wakeup(&mut self) {
        self.wakeups += 1;
        self.report();
    }

    fn report(&mut self) {
        let span = self.since.elapsed();
        if span >= Duration::from_secs(2) {
            if log::log_enabled!(log::Level::Debug) {
                let ms = |d: Duration| d.as_secs_f64() * 1000.0 / self.count.max(1) as f64;
                log::debug!(
                    "{:.1} frames/s, per frame: {:.2} ms building (terminal locked), {:.2} ms presenting; {} output wakeups, {} replies",
                    self.count as f64 / span.as_secs_f64(),
                    ms(self.locked),
                    ms(self.present),
                    self.wakeups,
                    self.replies
                );
            }
            *self = Self::new();
        }
    }
}

/// Writes "Tab N: app" into `buf`, reusing its allocation.
fn write_label(buf: &mut String, index: usize, app: &str) {
    use std::fmt::Write;
    buf.clear();
    let _ = write!(buf, "Tab {}: {}", index + 1, app);
}

fn shell_name(cfg: &Config) -> String {
    let shell = if cfg.shell.is_empty() { std::env::var("SHELL").unwrap_or_default() } else { cfg.shell.clone() };
    let name = shell.rsplit(['/', '\\']).next().unwrap_or("").trim_end_matches(".exe");
    if name.is_empty() {
        if cfg!(windows) { "PowerShell".to_owned() } else { "shell".to_owned() }
    } else {
        name.to_owned()
    }
}

impl App {
    pub fn new(
        cfg: Config,
        proxy: EventLoopProxy<UserEvent>,
        first_tab: (Option<String>, Vec<String>),
        gpu_override: Option<GpuBackend>,
    ) -> Self {
        let remote = remote::serve(proxy.clone());
        Self {
            cfg,
            watcher: ConfigWatcher::new(),
            proxy,
            state: None,
            next_poll: Instant::now() + CONFIG_POLL,
            error: None,
            first_tab,
            _remote: remote,
            gpu_override,
        }
    }

    fn create(&mut self, el: &ActiveEventLoop) -> Result<State, String> {
        let attrs = Window::default_attributes()
            .with_title(TITLE)
            .with_inner_size(winit::dpi::LogicalSize::new(960.0, 600.0));
        // Windows paints a new window white until the first frame arrives;
        // keep it hidden until then (see `resumed`).
        #[cfg(windows)]
        let attrs = attrs.with_visible(false);
        // The icon build.rs embeds (resource 1). winit registers its window
        // class without one, so the taskbar and Alt+Tab would show a blank one.
        #[cfg(windows)]
        let attrs = {
            use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
            use winit::window::Icon;
            attrs.with_window_icon(Icon::from_resource(1, None).ok()).with_taskbar_icon(Icon::from_resource(1, None).ok())
        };
        // App id / WM_CLASS, matching StartupWMClass in the .desktop file.
        #[cfg(all(unix, not(target_os = "macos")))]
        let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, "fastcnsl", "fastcnsl");
        #[cfg(all(unix, not(target_os = "macos")))]
        let attrs = winit::platform::x11::WindowAttributesExtX11::with_name(attrs, "fastcnsl", "fastcnsl");
        // Window buttons in the tab bar: no OS frame on Windows/Linux; on
        // macOS the native traffic lights float over the tab bar instead.
        let custom = self.cfg.title_bar.is_custom();
        #[cfg(not(target_os = "macos"))]
        let attrs = attrs.with_decorations(!custom);
        #[cfg(target_os = "macos")]
        let attrs = if custom {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs.with_titlebar_transparent(true).with_title_hidden(true).with_fullsize_content_view(true)
        } else {
            attrs
        };
        let window = Arc::new(el.create_window(attrs).map_err(|e| e.to_string())?);
        window.set_ime_allowed(true);
        window.set_cursor(CursorIcon::Text);
        let scale = window.scale_factor();

        let cfg = &self.cfg;
        let mut renderer = pollster::block_on(Renderer::new(
            window.clone(),
            el.owned_display_handle(),
            &cfg.font_family,
            font_px(cfg.font_size, scale),
            cfg.line_height,
            cfg.antialiasing.subpixel(crate::render::system_subpixel()),
            self.gpu_override.unwrap_or(cfg.gpu),
        ))?;
        renderer.glyphs.set_ui_font(ui_font_px(scale));

        // Size the window to a comfortable grid now that the cell is known.
        let cell = renderer.cell();
        let pad = padding_px(cfg, scale as f32);
        let sb = ui::scrollbar_width(scale as f32);
        let bar = chrome::tab_bar_height(&Metrics { width: 0.0, height: 0.0, scale: scale as f32, cw: cell.width, ch: cell.height });
        let want = PhysicalSize::new(
            (INITIAL_COLS as f32 * cell.width + 2.0 * pad + gutter_px(cfg, cell.width, pad) + sb).ceil() as u32,
            (INITIAL_LINES as f32 * cell.height + 2.0 * pad + bar).ceil() as u32,
        );
        let _ = window.request_inner_size(want);
        // Where the new size applies at once (Windows), draw the first
        // frame at it rather than at the creation size.
        let now = window.inner_size();
        let mut renderer = renderer;
        renderer.resize(now.width, now.height);

        // Linux desktops (and WSL) are asked directly: winit can't see their
        // setting. Windows and macOS report it through winit.
        let detected = if cfg!(target_os = "linux") { theme::detect_os_dark() } else { None };
        let os_dark = detected.or(window.theme().map(|t| t == Theme::Dark)).unwrap_or(true);
        theme::spawn_os_theme_watcher(self.proxy.clone(), Some(os_dark));
        let palette = Palette::for_mode(cfg.theme, os_dark);

        let grid = grid_size(&renderer, cfg, scale, 0.0);
        let mut st = State {
            window,
            renderer,
            proxy: self.proxy.clone(),
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            overlay: Overlay::None,
            hover: Hover::None,
            palette,
            os_dark,
            theme_mode: cfg.theme,
            scrollbar: Scrollbar::new(cfg.ui_idle_opacity),
            resized_at: None,
            search_fade: Fade::new(1.0),
            search_hover: false,
            mods: ModifiersState::empty(),
            mouse: (0.0, 0.0),
            selecting: false,
            last_click: None,
            click_count: 0,
            reported_button: None,
            last_reported_cell: None,
            wheel_accum: 0.0,
            clipboard: arboard::Clipboard::new().map_err(|e| log::warn!("clipboard unavailable: {e}")).ok(),
            focused: true,
            font_pt: cfg.font_size,
            grid,
            wake_at: None,
            custom_title_bar: custom,
            finishing_logs: Vec::new(),
            last_bar_click: None,
            cursor: CursorIcon::Text,
            startup: String::new(),
            options_page: OptionsPage::General,
            font_families: Vec::new().into(),
            font_scroll: 0,
            gpu_started: self.gpu_override.unwrap_or(cfg.gpu),
            frames: FrameStats::new(),
            panel_offset: (0.0, 0.0),
            panel_drag: None,
            chat_drag: None,
            keymap: Keymap::new(&cfg.keys),
            capturing: None,
            pending_keys: None,
            last_frame: Instant::now(),
            frame_cost: Duration::ZERO,
            toast: None,
            size_until: None,
            ai_job: None,
            ai_label: "",
            ai_budget: ai::context::Budget::new(),
            ai_bookmarked: Vec::new(),
            next_ai_job: 0,
            ai_keys: ai::Keys::path().map(|p| ai::Keys::load(&p)).unwrap_or_default(),
            ai_models: Vec::new().into(),
            ai_scroll: 0,
            ai_status: String::new(),
            ai_models_job: None,
            ai_test_job: None,
            ai_model_shown: String::new(),
            ai_account: chatgpt_account(),
            ai_grok_account: grok_account(),
            ai_signin: None,
            ai_reset_model: false,
        };
        st.show_size();
        let (name, command) = self.first_tab.clone();
        st.new_tab_with(cfg, name, command)?;
        Ok(st)
    }

    fn apply_config(&mut self, new: Config) {
        let old = std::mem::replace(&mut self.cfg, new);
        let cfg = &self.cfg;
        let Some(st) = self.state.as_mut() else { return };
        log::info!("settings applied");
        if old.font_family != cfg.font_family || old.font_size != cfg.font_size || old.line_height != cfg.line_height {
            st.font_pt = cfg.font_size;
            st.reload_font(cfg);
        }
        if old.keys != cfg.keys {
            st.keymap = Keymap::new(&cfg.keys);
        }
        if old.antialiasing != cfg.antialiasing {
            st.renderer.set_subpixel(cfg.antialiasing.subpixel(crate::render::system_subpixel()));
        }
        if old.padding != cfg.padding || old.line_numbers != cfg.line_numbers || old.ai.panel_width != cfg.ai.panel_width {
            st.resize_grid(cfg);
        }
        if old.theme != cfg.theme {
            st.theme_mode = cfg.theme;
        }
        for tab in &mut st.tabs {
            tab.code.enabled = cfg.code_highlight;
            if old.memory_per_tab_mb != cfg.memory_per_tab_mb {
                tab.session.apply_config(cfg);
            }
        }
        st.apply_palette(cfg);
        if old.shell != cfg.shell || old.shell_args != cfg.shell_args {
            log::info!("shell change applies to new tabs");
        }
        let custom = cfg.title_bar.is_custom();
        if custom != st.custom_title_bar {
            if cfg!(target_os = "macos") {
                log::info!("title bar change applies after a restart on macOS");
            } else {
                st.custom_title_bar = custom;
                st.window.set_decorations(!custom);
            }
        }
        st.window.request_redraw();
    }

    /// Changes settings from the UI: applies them and saves the file.
    fn change_config(&mut self, change: impl FnOnce(&mut Config)) {
        let mut new = self.cfg.clone();
        change(&mut new);
        if let Err(e) = new.save() {
            log::warn!("could not save settings: {e}");
        }
        self.watcher.mark_seen();
        self.apply_config(new);
    }

    /// The settings and the window's state, while the app runs. After
    /// `exit()` winit still delivers the events queued, and with the last
    /// tab closed there is nothing to handle them for (`tab()` would panic).
    fn live(&mut self, el: &ActiveEventLoop) -> Option<(&Config, &mut State)> {
        if el.exiting() {
            return None;
        }
        Some((&self.cfg, self.state.as_mut()?))
    }

    fn perform(&mut self, el: &ActiveEventLoop, action: Action) {
        let Some(st) = self.state.as_mut() else { return };
        let confirming = if let Overlay::AiConfirm(at) = st.overlay { Some(at) } else { None };
        if !matches!(
            action,
            Action::FontBigger
                | Action::FontSmaller
                | Action::FontFamily(_)
                | Action::LineHeight(_)
                | Action::BoldIsBright(_)
                | Action::Antialiasing(_)
                | Action::CodeHighlight(_)
                | Action::LineNumbers(_)
                | Action::LogNewTabs(_)
                | Action::LogTimestamps(_)
                | Action::Theme(_)
                | Action::OptionsPage(_)
                | Action::Gpu(_)
                | Action::Rebind(_)
                | Action::ResetKey(_)
                | Action::ResetKeys
                | Action::ResetSetting(_)
                | Action::ApplyKeys
                | Action::AiProvider(_)
                | Action::AiPasteKey
                | Action::AiRemoveKey
                | Action::AiModel(_)
                | Action::AiTest
                | Action::AiSignIn
                | Action::AiSignOut
                | Action::AiManageUsage
                | Action::AiOpenSignInPage
        )
            || matches!(st.overlay, Overlay::Menu(_))
        {
            st.overlay = Overlay::None;
        }
        st.hover = Hover::None;
        st.window.request_redraw();
        match action {
            Action::NewTab => {
                if let Err(e) = st.new_tab(&self.cfg) {
                    log::error!("{e}");
                }
            }
            Action::CloseTab => {
                let i = st.active;
                if st.close_tab(i) {
                    el.exit();
                }
            }
            Action::Find => {
                st.tab_mut().chat.focused = false;
                st.tab_mut().search_with(|search, term| search.open_in(term));
            }
            Action::Copy => st.copy_selection(),
            Action::Paste => st.paste(),
            Action::SelectAll => st.tab_mut().shown(|term| {
                let mut sel = Selection::new(SelectionType::Simple, Point::new(term.topmost_line(), Column(0)), Side::Left);
                sel.include_all();
                term.selection = Some(sel);
            }),
            Action::Theme(mode) => self.change_config(|c| c.theme = mode),
            Action::FontBigger => {
                let size = (self.cfg.font_size + 1.0).min(96.0);
                self.change_config(|c| c.font_size = size);
            }
            Action::FontSmaller => {
                let size = (self.cfg.font_size - 1.0).max(4.0);
                self.change_config(|c| c.font_size = size);
            }
            Action::FontFamily(i) => {
                let Some(name) = st.font_families.get(i).cloned() else { return };
                self.change_config(|c| c.font_family = name);
                if let Some(st) = self.state.as_mut() {
                    st.reveal_font(&self.cfg, i);
                }
            }
            Action::LineHeight(taller) => {
                let tenths = (self.cfg.line_height * 10.0).round() + if taller { 1.0 } else { -1.0 };
                let height = (tenths / 10.0).clamp(0.8, 3.0);
                self.change_config(|c| c.line_height = height);
            }
            Action::BoldIsBright(on) => self.change_config(|c| c.bold_is_bright = on),
            Action::Antialiasing(aa) => self.change_config(|c| c.antialiasing = aa),
            Action::CodeHighlight(on) => self.change_config(|c| c.code_highlight = on),
            Action::LineNumbers(mode) => self.change_config(|c| c.line_numbers = mode),
            Action::LogNewTabs(on) => self.change_config(|c| c.log_new_tabs = on),
            Action::LogTimestamps(on) => self.change_config(|c| c.log_timestamps = on),
            Action::CopyTabText(i) if i < st.tabs.len() => st.copy_tab_text(i),
            Action::SaveTabText(i) if i < st.tabs.len() => st.save_tab_text(&self.cfg, i),
            Action::ToggleTabLog(i) if i < st.tabs.len() => {
                if st.tabs[i].log.is_some() {
                    st.stop_log(i);
                } else {
                    st.start_log(&self.cfg, i, false);
                }
            }
            Action::NextTab | Action::PreviousTab => st.cycle_tab(action == Action::NextTab),
            Action::CloseTabAt(i) => {
                if st.close_tab(i) {
                    el.exit();
                }
            }
            Action::OpenLogFolder => {
                let dir = self.cfg.log_dir();
                let shown = logging::display_path(&dir);
                match std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|()| crate::open::open(&dir)) {
                    Ok(()) => {}
                    Err(e) => st.show(format!("Could not open {shown}: {e}")),
                }
            }
            Action::CopyTabText(_) | Action::SaveTabText(_) | Action::ToggleTabLog(_) => {}
            Action::ToggleBookmark(line) => st.tab_mut().lines.toggle_bookmark(line),
            Action::PreviousBookmark => st.jump_to_bookmark(false),
            Action::NextBookmark => st.jump_to_bookmark(true),
            Action::Gpu(gpu) => self.change_config(|c| c.gpu = gpu),
            Action::Rebind(cmd) => st.capturing = if st.capturing == Some(cmd) { None } else { Some(cmd) },
            Action::ResetKey(cmd) => {
                st.capturing = None;
                let keys = st.keymap.reset(cmd);
                self.change_config(|c| c.keys = keys);
            }
            Action::ResetKeys => {
                st.capturing = None;
                self.change_config(|c| c.keys.clear());
            }
            Action::ResetSetting(setting) => self.change_config(|c| setting.reset(c)),
            Action::ApplyKeys => {
                if let Some(keys) = st.pending_keys.take() {
                    self.change_config(|c| c.keys = keys);
                }
            }
            Action::Options => {
                st.overlay = Overlay::Options;
                st.options_page = OptionsPage::General;
                st.panel_offset = (0.0, 0.0);
                st.capturing = None;
            }
            Action::About => {
                st.overlay = Overlay::About;
                st.panel_offset = (0.0, 0.0);
            }
            Action::OpenSettingsFile => {
                if let Some(path) = Config::path() {
                    st.open(path);
                }
            }
            Action::OpenProjectPage => st.open(crate::REPOSITORY),
            Action::OpenReleases => st.open(format!("{}/releases", crate::REPOSITORY)),
            Action::ClosePanel => {}
            Action::Quit => el.exit(),
            Action::AiFix(at) => st.ai_fix(&self.cfg, at),
            Action::AiConfirm(go) => {
                let provider = self.cfg.ai.provider;
                if go && let Some(what) = confirming {
                    if !self.cfg.ai.confirmed.contains(&provider) {
                        self.change_config(|c| c.ai.confirmed.push(provider));
                    }
                    if let Some(st) = self.state.as_mut() {
                        match what {
                            Confirm::Fix(at) => st.ai_fix(&self.cfg, at),
                            Confirm::Chat => st.send_chat(&self.cfg, true),
                        }
                    }
                }
            }
            Action::AiChat => st.toggle_chat(&self.cfg),
            Action::AiChatAt(i) => {
                if i < st.tabs.len() {
                    st.select_tab(i);
                    st.open_chat(&self.cfg, None);
                }
            }
            Action::AskAiInChat(line) => st.open_chat(&self.cfg, Some(line)),
            Action::AiChatWidth(share) => {
                self.change_config(|c| c.ai.panel_width = share);
                if let Some(st) = self.state.as_mut() {
                    st.resize_grid(&self.cfg);
                }
            }
            Action::OptionsPage(page) => {
                st.options_page = page;
                st.capturing = None;
                if page == OptionsPage::Font {
                    st.open_font_page(&self.cfg);
                }
                if page == OptionsPage::Ai {
                    st.load_ai_models(&self.cfg);
                }
            }
            Action::AiProvider(p) => {
                if p != self.cfg.ai.provider {
                    // Model names differ between providers.
                    self.change_config(|c| {
                        c.ai.provider = p;
                        c.ai.model.clear();
                    });
                }
                if let Some(st) = self.state.as_mut() {
                    st.load_ai_models(&self.cfg);
                }
            }
            Action::AiPasteKey => {
                st.paste_ai_key(&self.cfg);
            }
            Action::AiRemoveKey => {
                st.set_ai_key(&self.cfg, None);
                st.ai_models = Vec::new().into();
                st.ai_status = "Key removed".to_owned();
            }
            Action::AiModel(i) => {
                if let Some(model) = st.ai_models.get(i).cloned() {
                    st.ai_status.clear();
                    self.change_config(|c| c.ai.model = model);
                }
            }
            Action::AiTest => st.test_ai(&self.cfg),
            Action::AiSignIn => st.sign_in_or_cancel(self.cfg.ai.provider),
            Action::AiOpenSignInPage => {
                if let Some(page) = st.ai_signin.as_ref().and_then(|s| s.code.as_ref()).map(|(_, page)| page.clone()) {
                    st.open(page);
                }
            }
            Action::AiSignOut => {
                st.sign_out(self.cfg.ai.provider);
                self.change_config(|c| c.ai.model.clear());
                if let Some(st) = self.state.as_mut() {
                    st.load_ai_models(&self.cfg);
                }
            }
            Action::AiManageUsage => st.open("https://chatgpt.com/settings/usage"),
            Action::AiWelcomeDone => {
                if let Some(path) = ai::login::Login::path() {
                    let _one = ai::login::LOCK.lock().unwrap_or_else(|e| e.into_inner());
                    let mut l = ai::login::Login::load(&path);
                    l.welcomed = true;
                    if let Err(e) = l.save(&path) {
                        log::warn!("could not save the ChatGPT sign-in: {e}");
                    }
                }
                st.overlay = Overlay::Options;
                st.options_page = OptionsPage::Ai;
            }
        }
    }
}

/// The padding around the grid in whole pixels: at fractional scales
/// (125%: 6 -> 7.5) the grid would otherwise sit between pixels, and so
/// would every cell background and glyph.
fn padding_px(cfg: &Config, scale: f32) -> f32 {
    (cfg.padding * scale).round()
}

/// A bookmark jump's scroll: the view's distance from the top of the
/// history (in `Scroller::metrics` units, which output arriving below
/// doesn't change) eases from `from` to `to`, then lands exactly on `line`.
struct Glide {
    from: usize,
    to: usize,
    line: u64,
    start: Instant,
    /// The distance after the last step, to notice other scrolling.
    seen: usize,
}

/// How long a bookmark jump's scroll takes.
const GLIDE_FOR: Duration = Duration::from_millis(220);

/// Fast at first, slowing to a stop: 0 at 0, 1 at 1.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// How long a line a bookmark jump lands on stays highlighted.
const FLASH_FOR: Duration = Duration::from_millis(1200);
/// How long of that it stays at full strength before fading out.
const FLASH_HOLD: Duration = Duration::from_millis(500);

/// Strength of the jump highlight `elapsed` after the jump, 1 down to 0.
fn flash_alpha(elapsed: Duration) -> f32 {
    let fade = (FLASH_FOR - FLASH_HOLD).as_secs_f32();
    (1.0 - elapsed.saturating_sub(FLASH_HOLD).as_secs_f32() / fade).clamp(0.0, 1.0)
}

/// Width of the line-number gutter in whole pixels (0 without numbers):
/// from the left padding to the text. The times start left of the padding's
/// edge (see `lines::gutter_inset`) and end a small gap before the text.
fn gutter_px(cfg: &Config, cell_width: f32, pad: f32) -> f32 {
    if !cfg.line_numbers.is_on() {
        return 0.0;
    }
    let chars = lines::gutter_cells(cfg.line_numbers) as f32 + lines::GUTTER_GAP;
    ((lines::gutter_inset(pad) + chars * cell_width).round() - pad).max(0.0)
}

/// Viewport cell at a window position: (col, row, side of the cell). The
/// gutter is left of the text, so it moves columns and not rows.
fn cell_at(pos: (f32, f32), top: f32, pad: f32, gutter: f32, cell: (f32, f32), grid: WindowSize) -> (usize, usize, Side) {
    let (cw, ch) = cell;
    let x = (pos.0 - pad - gutter).max(0.0);
    let y = (pos.1 - top - pad).max(0.0);
    let col = ((x / cw) as usize).min(grid.num_cols as usize - 1);
    let row = ((y / ch) as usize).min(grid.num_lines as usize - 1);
    let side = if x - col as f32 * cw > cw / 2.0 { Side::Right } else { Side::Left };
    (col, row, side)
}

/// The button as reported to programs; None for the extra buttons.
fn mouse_button(b: WinitButton) -> Option<MouseButton> {
    Some(match b {
        WinitButton::Left => MouseButton::Left,
        WinitButton::Middle => MouseButton::Middle,
        WinitButton::Right => MouseButton::Right,
        _ => return None,
    })
}

/// The chat panel's share of a window `width` wide with its divider
/// dragged to `x`: rounded to hundredths (it is saved), 0.2 to 0.8.
fn chat_share(width: f32, x: f32) -> f32 {
    (((width - x) / width).clamp(0.2, 0.8) * 100.0).round() / 100.0
}

/// Columns that fit in a window `width` wide with `pad` on both sides,
/// the gutter, the scrollbar and a chat panel `panel` wide; at least 2.
fn columns(width: f32, pad: f32, gutter: f32, scrollbar: f32, cell_w: f32, panel: f32) -> u16 {
    ((width - panel - 2.0 * pad - gutter - scrollbar) / cell_w).floor().max(2.0) as u16
}

/// The grid that fits the window, less a chat panel `panel` px wide.
fn grid_size(r: &Renderer, cfg: &Config, scale: f64, panel: f32) -> WindowSize {
    let (w, h) = r.size();
    let cell = r.cell();
    let pad = padding_px(cfg, scale as f32);
    let sb = ui::scrollbar_width(scale as f32);
    let bar = chrome::tab_bar_height(&Metrics { width: w as f32, height: h as f32, scale: scale as f32, cw: cell.width, ch: cell.height });
    let cols = columns(w as f32, pad, gutter_px(cfg, cell.width, pad), sb, cell.width, panel);
    let lines = ((h as f32 - 2.0 * pad - bar) / cell.height).floor().max(1.0);
    WindowSize {
        num_cols: cols,
        num_lines: lines as u16,
        cell_width: cell.width as u16,
        cell_height: cell.height as u16,
    }
}

impl State {
    fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    /// Changes the mouse cursor only when it differs from the current one.
    /// Setting it on every mouse move re-uploads the cursor image on
    /// Wayland, which some compositors (WSLg) answer by hiding the cursor.
    fn set_cursor(&mut self, icon: CursorIcon) {
        if icon != self.cursor {
            self.cursor = icon;
            self.window.set_cursor(icon);
        }
    }

    fn metrics(&self) -> Metrics {
        let (w, h) = self.renderer.size();
        let cell = self.renderer.cell();
        Metrics { width: w as f32, height: h as f32, scale: self.scale(), cw: cell.width, ch: cell.height }
    }

    fn top(&self) -> f32 {
        chrome::tab_bar_height(&self.metrics())
    }

    /// Window controls in the tab bar for this platform.
    fn caption(&self) -> Caption {
        if !self.custom_title_bar {
            Caption::NONE
        } else if cfg!(target_os = "macos") {
            Caption { buttons: false, left_inset: (78.0 * self.scale()).round() }
        } else {
            Caption { buttons: true, left_inset: 0.0 }
        }
    }

    fn tab_bar(&self) -> chrome::TabBar {
        chrome::tab_bar(&self.metrics(), self.tabs.len(), &self.caption())
    }

    /// Window edge under (x, y) when the app draws its own frame.
    fn resize_edge(&self, x: f32, y: f32) -> Option<winit::window::ResizeDirection> {
        if !self.caption().buttons || self.window.is_maximized() {
            return None;
        }
        let m = self.metrics();
        chrome::resize_edge(x, y, m.width, m.height, (5.0 * m.scale).round())
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn new_tab(&mut self, cfg: &Config) -> Result<(), String> {
        self.new_tab_with(cfg, None, Vec::new())
    }

    /// Opens a tab named `name` running `command` (program and
    /// arguments), or the configured shell if `command` is empty.
    fn new_tab_with(&mut self, cfg: &Config, name: Option<String>, command: Vec<String>) -> Result<(), String> {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let run;
        let cfg = match command.split_first() {
            Some((program, args)) => {
                run = Config { shell: program.clone(), shell_args: args.to_vec(), ..cfg.clone() };
                &run
            }
            None => cfg,
        };
        let listener = EventProxy::new(self.proxy.clone(), id);
        let proxy = self.proxy.clone();
        let searcher = Searcher::new(move || {
            let _ = proxy.send_event(UserEvent::History(id));
        });
        // New tabs have no chat open: the window's whole width.
        let grid = grid_size(&self.renderer, cfg, self.window.scale_factor(), 0.0);
        let session = Session::spawn(cfg, grid, listener.clone())
            .map_err(|e| format!("starting {}: {e}", if cfg.shell.is_empty() { "shell" } else { &cfg.shell }))?;
        let code_theme = if self.palette.dark { &cfg.code_theme_dark } else { &cfg.code_theme_light };
        self.tabs.push(Tab {
            id,
            session,
            search: Search::new(),
            code: CodeHighlighter::new(cfg.code_highlight, code_theme),
            lines: LineIndex::new(),
            numbers: Vec::new(),
            bookmarked: Vec::new(),
            flash: None,
            glide: None,
            app: shell_name(cfg),
            name,
            label: String::new(),
            shell_title: None,
            log: None,
            activity: false,
            scroller: Scroller::new(listener),
            shown_changes: 0,
            searcher,
            search_view: 0,
            log_behind: false,
            grid,
            chat: ChatPanel::new(),
        });
        self.relabel();
        self.select_tab(self.tabs.len() - 1);
        if cfg.log_new_tabs {
            self.start_log(cfg, self.tabs.len() - 1, true);
        }
        Ok(())
    }

    /// Shows a notice at the bottom of the window for a few seconds.
    fn show(&mut self, text: String) {
        self.show_for(text, TOAST_FOR);
    }

    /// Shows a notice for `time`, or until the next one.
    fn show_for(&mut self, text: String, time: Duration) {
        log::info!("{text}");
        let until = Instant::now() + time;
        self.toast = Some((text, until));
        let wake = until + Duration::from_millis(20);
        self.wake_at = Some(self.wake_at.map_or(wake, |t| t.min(wake)));
        self.window.request_redraw();
    }

    /// Shows the grid size ("100x30", columns by lines) for a few seconds.
    fn show_size(&mut self) {
        let until = Instant::now() + SIZE_FOR;
        self.size_until = Some(until);
        let wake = until + Duration::from_millis(20);
        self.wake_at = Some(self.wake_at.map_or(wake, |t| t.min(wake)));
        self.window.request_redraw();
    }

    /// Opens a file or URL with the desktop's default app; says so if it can't.
    fn open(&mut self, target: impl AsRef<std::ffi::OsStr>) {
        let target = target.as_ref();
        if let Err(e) = crate::open::open(target) {
            self.show(format!("Could not open {}: {e}", logging::display_path(std::path::Path::new(target))));
        }
    }

    /// The name used in file names for tab `i`: its given name or its program.
    fn tab_file_name(&self, i: usize) -> &str {
        let tab = &self.tabs[i];
        tab.name.as_deref().unwrap_or(&tab.app)
    }

    /// Copies tab `i`'s text to the clipboard: its last `COPY_LINES` lines.
    fn copy_tab_text(&mut self, i: usize) {
        let (text, lines, capped) = {
            let tab = &self.tabs[i];
            let mut term = tab.session.term.lock();
            let snap = crate::history::scroll::settled(&mut term, &tab.session.history);
            logging::tail_text(&snap, term.grid(), logging::COPY_LINES)
        };
        let label = self.tabs[i].label.clone();
        let copied = if capped { format!("Copied the last {lines} lines of {label}") } else { format!("Copied {lines} lines of {label}") };
        match self.clipboard.as_mut().map(|cb| cb.set_text(text)) {
            Some(Ok(())) => self.show(copied),
            Some(Err(e)) => self.show(format!("Could not copy: {e}")),
            None => self.show("No clipboard available".to_owned()),
        }
    }

    /// Saves all of tab `i`'s text to a new file in the logs folder, on a
    /// thread of its own: a long history takes a while.
    fn save_tab_text(&mut self, cfg: &Config, i: usize) {
        let path = logging::new_file(&cfg.log_dir(), i + 1, self.tab_file_name(i), "txt");
        let (snap, live_text) = self.tabs[i].text_snapshot();
        let shown = logging::display_path(&path);
        let label = self.tabs[i].label.clone();
        let proxy = self.proxy.clone();
        logging::save_text_in_background(path, snap, live_text, move |result| {
            let message = match result {
                Ok(()) => format!("Saved {label} to {shown}"),
                Err(e) => format!("Could not save {shown}: {e}"),
            };
            let _ = proxy.send_event(UserEvent::Notice(message));
        });
    }

    /// Starts logging tab `i` to a new file, beginning with the text it
    /// already has. `quiet`: no notice unless it fails (new tabs).
    fn start_log(&mut self, cfg: &Config, i: usize, quiet: bool) {
        let path = logging::new_file(&cfg.log_dir(), i + 1, self.tab_file_name(i), "log");
        let shown = logging::display_path(&path);
        match TabLog::create(&path, cfg.log_timestamps) {
            Ok(log) => {
                self.tabs[i].log = Some(log);
                self.write_log(i);
                if !quiet && self.tabs[i].log.is_some() {
                    self.show(format!("Logging {} to {shown}", self.tabs[i].label));
                }
            }
            Err(e) => self.show(format!("Could not log to {shown}: {e}")),
        }
        self.window.request_redraw();
    }

    /// Notes when tab `i`'s new lines appeared. Always on, so switching to
    /// times shows them for what is already in the tab.
    fn record_line_times(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        let term = tab.session.term.lock();
        if !term.mode().contains(TermMode::ALT_SCREEN) {
            tab.lines.record_times(term.grid());
        }
    }

    /// Writes tab `i`'s new complete lines to its log, if it has one. A
    /// full-screen program's screen isn't logged; its output is, after it.
    fn write_log(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        match tab.log_pass(Upto::Complete) {
            Ok(behind) => tab.log_behind = behind,
            Err(e) => {
                let shown = tab.log.as_ref().map(|l| logging::display_path(l.path())).unwrap_or_default();
                tab.log = None;
                tab.log_behind = false;
                self.show(format!("Stopped logging to {shown}: {e}"));
            }
        }
    }

    /// Stops logging tab `i`, writing the line the cursor is on too.
    fn finish_log(&mut self, i: usize) -> Option<std::path::PathBuf> {
        let tab = &mut self.tabs[i];
        let path = tab.log.as_ref()?.path().to_owned();
        tab.log_behind = false;
        let mut term = tab.session.term.lock();
        let needs_history = !term.mode().contains(TermMode::ALT_SCREEN)
            && tab.log.as_ref().is_some_and(|l| l.needs_history(term.grid(), Upto::Cursor));
        if needs_history {
            // Lines still waiting in history: written on a thread of their own.
            let snap = crate::history::scroll::settled(&mut term, &tab.session.history);
            let log = tab.log.take()?;
            let proxy = self.proxy.clone();
            let shown = logging::display_path(&path);
            let worker = log.finish_in_background(snap, term.grid(), move |result| {
                if let Err(e) = result {
                    let _ = proxy.send_event(UserEvent::Notice(format!("Could not finish {shown}: {e}")));
                }
            });
            drop(term);
            self.finishing_logs.retain(|t| !t.is_finished());
            self.finishing_logs.push(worker);
            return Some(path);
        }
        drop(term);
        if let Err(e) = tab.log_pass(Upto::Cursor) {
            log::warn!("logging to {}: {e}", path.display());
        }
        tab.log = None;
        Some(path)
    }

    fn stop_log(&mut self, i: usize) {
        if let Some(path) = self.finish_log(i) {
            self.show(format!("Stopped logging {} to {}", self.tabs[i].label, logging::display_path(&path)));
        }
    }

    /// Rebuilds the "Tab N: app" labels after tabs open, close or run
    /// something new.
    fn relabel(&mut self) {
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            write_label(&mut tab.label, i, tab.name.as_deref().unwrap_or(&tab.app));
        }
        self.update_window_title();
    }

    fn update_window_title(&self) {
        if let Some(tab) = self.tabs.get(self.active) {
            self.window.set_title(tab.shell_title.as_deref().unwrap_or(&tab.label));
        }
    }

    /// Index of the tab `which` names: its 1-based number or its name
    /// (the active tab if None).
    fn find_tab(&self, which: Option<&str>) -> Result<usize, String> {
        let Some(w) = which else { return Ok(self.active) };
        if let Ok(n) = w.parse::<usize>() {
            return (1..=self.tabs.len()).contains(&n).then(|| n - 1).ok_or_else(|| format!("no tab {n}; there are {}", self.tabs.len()));
        }
        self.tabs
            .iter()
            .position(|t| t.name.as_deref() == Some(w))
            .or_else(|| self.tabs.iter().position(|t| t.app == w))
            .ok_or_else(|| format!("no tab named {w}"))
    }

    /// Carries out a command from `fastcnsl new-tab/close-tab/quit`.
    /// Returns the reply and whether the app should exit.
    fn on_remote(&mut self, cfg: &Config, command: remote::Command) -> (remote::Reply, bool) {
        self.window.request_redraw();
        match command {
            remote::Command::NewTab { name, command } => {
                let r = self.new_tab_with(cfg, name, command).map(|()| format!("opened tab {}", self.tabs.len()));
                (r, false)
            }
            remote::Command::CloseTab(which) => match self.find_tab(which.as_deref()) {
                Ok(i) => {
                    let label = self.tabs[i].label.clone();
                    let last = self.close_tab(i);
                    (Ok(format!("closed {label}")), last)
                }
                Err(e) => (Err(e), false),
            },
            remote::Command::Quit => (Ok("quitting".to_owned()), true),
        }
    }

    /// Closes a tab; returns true when it was the last one.
    fn close_tab(&mut self, i: usize) -> bool {
        if i >= self.tabs.len() {
            return false;
        }
        self.finish_log(i);
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            return true;
        }
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.relabel();
        self.select_tab(self.active);
        false
    }

    fn select_tab(&mut self, i: usize) {
        self.active = i.min(self.tabs.len() - 1);
        self.grid = self.tabs[self.active].grid;
        self.tabs[self.active].activity = false;
        self.selecting = false;
        self.reported_button = None;
        self.update_window_title();
        self.window.request_redraw();
    }

    /// The right-click menu of tab `i`.
    fn tab_menu(&self, x: f32, y: f32, i: usize) -> Menu {
        Menu::tab(x, y, i, &self.tabs[i].label, self.tabs[i].log.is_some(), self.tabs.len(), &self.keymap)
    }

    fn cycle_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        let i = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
        self.select_tab(i);
    }

    /// Sizes each tab's grid to the window, less its chat panel; tabs
    /// whose size changed are resized (the shell is told).
    fn resize_grid(&mut self, cfg: &Config) {
        let scale = self.window.scale_factor();
        let same = |a: WindowSize, b: WindowSize| (a.num_cols, a.num_lines, a.cell_width, a.cell_height) == (b.num_cols, b.num_lines, b.cell_width, b.cell_height);
        let mut changed = false;
        for i in 0..self.tabs.len() {
            let size = grid_size(&self.renderer, cfg, scale, self.chat_px(cfg, &self.tabs[i]));
            let tab = &mut self.tabs[i];
            let g = tab.grid;
            if same(size, g) {
                continue;
            }
            if size.num_cols != g.num_cols {
                // Text reflows; recorded row positions no longer hold.
                tab.code.clear();
            }
            tab.session.resize(size);
            tab.sync_scroller();
            tab.lines.invalidate();
            if let Some(log) = tab.log.as_mut() {
                log.invalidate();
            }
            tab.grid = size;
            changed = true;
        }
        let g = self.grid;
        let size = match self.tabs.get(self.active) {
            Some(tab) => tab.grid,
            None => grid_size(&self.renderer, cfg, scale, 0.0),
        };
        if changed {
            self.resized_at = Some(Instant::now());
        }
        if !same(size, g) {
            self.grid = size;
            if (size.num_cols, size.num_lines) != (g.num_cols, g.num_lines) {
                self.show_size();
            }
        }
    }

    /// Width of tab's chat panel in pixels; 0 while it is closed.
    fn chat_px(&self, cfg: &Config, tab: &Tab) -> f32 {
        if !tab.chat.open {
            return 0.0;
        }
        let active = self.tabs.get(self.active).is_some_and(|t| t.id == tab.id);
        let share = self.chat_drag.filter(|_| active).unwrap_or(cfg.ai.panel_width);
        (self.renderer.size().0 as f32 * share).round()
    }

    /// Width of the active tab's terminal: the window less its chat panel.
    fn term_width(&self, cfg: &Config) -> f32 {
        let w = self.renderer.size().0 as f32;
        self.tabs.get(self.active).map_or(w, |t| w - self.chat_px(cfg, t))
    }

    /// Background tab `i` printed something: puts a bell on it.
    fn mark_activity(&mut self, i: usize) {
        if self.tabs[i].activity || repainting_after_resize(self.resized_at, Instant::now()) {
            return;
        }
        self.tabs[i].activity = true;
        self.window.request_redraw();
    }

    fn reload_font(&mut self, cfg: &Config) {
        let px = font_px(self.font_pt, self.window.scale_factor());
        let hinted = crate::render::glyphs::hinted(self.window.scale_factor());
        self.renderer.glyphs.set_font(&cfg.font_family, px, cfg.line_height, hinted);
        self.renderer.glyphs.set_ui_font(ui_font_px(self.window.scale_factor()));
        self.resize_grid(cfg);
    }

    /// Options → Font opens: reads the installed families and scrolls the
    /// list to the one in use, a few rows from the top.
    fn open_font_page(&mut self, cfg: &Config) {
        self.font_families = self.renderer.glyphs.monospace_families().into();
        let i = self.options_state(cfg).font_index().unwrap_or(0);
        self.font_scroll = i.saturating_sub(3);
    }

    /// Scrolls the font list so family `i` is in view.
    fn reveal_font(&mut self, cfg: &Config, i: usize) {
        if let Some(list) = self.panel(cfg).and_then(|p| p.list) {
            self.font_scroll = list.revealing(i);
        }
    }

    fn options_state(&self, cfg: &Config) -> OptionsState {
        OptionsState {
            page: self.options_page,
            theme: self.theme_mode,
            font_size: cfg.font_size,
            font_family: cfg.font_family.clone(),
            code_highlight: cfg.code_highlight,
            gpu: cfg.gpu,
            gpu_pending: cfg.gpu != self.gpu_started,
            keys: self.keymap.clone(),
            capturing: self.capturing,
            line_numbers: cfg.line_numbers,
            log_folder: logging::display_path(&cfg.log_dir()),
            log_new_tabs: cfg.log_new_tabs,
            log_timestamps: cfg.log_timestamps,
            font_families: self.font_families.clone(),
            font_scroll: self.font_scroll,
            font_in_use: self.renderer.glyphs.family().to_owned(),
            line_height: cfg.line_height,
            bold_is_bright: cfg.bold_is_bright,
            antialiasing: cfg.antialiasing,
            changed: Setting::changed(cfg),
            ai_provider: cfg.ai.provider,
            ai_key: self.ai_keys.in_file(cfg.ai.provider).map(ai::keys::mask),
            ai_key_env: self.ai_keys.in_env(cfg.ai.provider).and(cfg.ai.provider.env_var()),
            ai_model: cfg.ai.model_or_default(),
            ai_base_url: cfg.ai.base_url.clone(),
            ai_models: self.ai_models.clone(),
            ai_scroll: self.ai_scroll,
            ai_status: self.ai_status.clone(),
            ai_account: match cfg.ai.provider {
                ai::Provider::OpenAi => self.ai_account.clone(),
                ai::Provider::Xai => self.ai_grok_account.clone(),
                _ => None,
            },
            ai_signing_in: self.ai_signin.as_ref().is_some_and(|s| s.provider == cfg.ai.provider),
            ai_sign_in_code: self.ai_signin.as_ref().filter(|s| s.provider == cfg.ai.provider).and_then(|s| s.code.as_ref()).map(|(code, _)| code.clone()),
        }
    }

    fn apply_palette(&mut self, cfg: &Config) {
        self.palette = Palette::for_mode(self.theme_mode, self.os_dark);
        let code_theme = if self.palette.dark { &cfg.code_theme_dark } else { &cfg.code_theme_light };
        for tab in &mut self.tabs {
            tab.code.set_theme(code_theme);
        }
    }

    fn write(&self, bytes: impl Into<std::borrow::Cow<'static, [u8]>>) {
        self.tab().session.write(bytes);
    }

    fn panel(&self, cfg: &Config) -> Option<Panel> {
        // Laid out in the panels' own font, so a font change doesn't move them.
        let ui = self.renderer.ui_cell();
        let m = Metrics { cw: ui.width, ch: ui.height, ..self.metrics() };
        match self.overlay {
            Overlay::Options => Some(chrome::options_panel(&m, &self.options_state(cfg), self.panel_offset)),
            Overlay::About => {
                let path = Config::path().map(|p| p.display().to_string()).unwrap_or_default();
                Some(chrome::about_panel(&m, &self.renderer.gpu, &self.startup, &path, self.panel_offset))
            }
            Overlay::AiConfirm(_) => Some(chrome::ai_confirm_panel(&m, cfg.ai.provider.label(), self.panel_offset)),
            Overlay::AiWelcome => Some(chrome::ai_welcome_panel(&m, self.panel_offset)),
            _ => None,
        }
    }

    fn redraw(&mut self, cfg: &Config) {
        // Logs started on a long history catch up a pass a frame.
        for i in 0..self.tabs.len() {
            if self.tabs[i].log_behind {
                self.write_log(i);
            }
        }
        let idle = cfg.ui_idle_opacity;
        let mut animating = self.scrollbar.fade.update(if self.scrollbar.active() { 1.0 } else { idle });
        animating |= self.tabs.iter().any(|t| t.log_behind);
        let search_target = if self.tab().search.focused || self.search_hover { 1.0 } else { idle };
        animating |= self.search_fade.update(search_target);

        if self.toast.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
            self.toast = None;
        }
        if self.size_until.is_some_and(|until| Instant::now() >= until) {
            self.size_until = None;
        }
        let size_text = self.size_until.map(|_| format!("{}x{}", self.grid.num_cols, self.grid.num_lines));
        let panel = self.panel(cfg);
        let top = self.top();
        let scale = self.scale();
        let term_w = self.tab().chat.open.then(|| self.term_width(cfg));
        let chat = self.chat_layout(cfg);
        let chat_focused = self.tab().chat.focused && self.focused && matches!(self.overlay, Overlay::None);
        // Lock through a cloned handle so the guard doesn't borrow `self.tabs`
        // (the tab bar needs every tab's title while the active tab updates).
        let shared = self.tabs[self.active].session.term.clone();
        let t0 = Instant::now();
        {
            let mut live = shared.lock();
            let tab = &mut self.tabs[self.active];
            let mut flash = None;
            animating |= tab.glide_step(&mut live);
            let budget = tab.session.budget();
            tab.scroller.sync(&mut live, &tab.session.history, &budget);
            tab.sync_lines();
            let scroll = tab.scroller.metrics(&live, &tab.session.history);
            if !live.mode().contains(TermMode::ALT_SCREEN) {
                let first = crate::history::lock(&tab.session.history).first_line();
                tab.lines.forget_before(first);
                tab.lines.prune_past_cursor(live.grid());
                tab.code.update(live.grid());
                let term = tab.scroller.shown(&live);
                tab.lines.numbers(term.grid(), cfg.line_numbers, &mut tab.numbers);
                tab.lines.bookmarked_rows(term.grid(), &mut tab.bookmarked);
                tab.flash = tab.flash.filter(|(_, at)| at.elapsed() < FLASH_FOR);
                flash = tab.flash.and_then(|(line, at)| {
                    let rows = tab.lines.rows_of(term.grid(), line)?;
                    Some((rows, flash_alpha(at.elapsed())))
                });
                animating |= tab.flash.is_some();
            } else {
                // Full-screen programs (vim, less) number their own lines.
                tab.numbers.clear();
                tab.bookmarked.clear();
            }
            if tab.search.open {
                if tab.scroller.changes() != tab.search_view {
                    tab.search.forget_current();
                    tab.search_view = tab.scroller.changes();
                }
                tab.search.refresh(tab.scroller.shown(&live), &live);
                let mut a = Across {
                    live: &mut live,
                    scroller: &mut tab.scroller,
                    history: &tab.session.history,
                    budget,
                    searcher: &mut tab.searcher,
                };
                tab.search.count_history(&mut a);
            }
            // Code in a window onto older lines: colour what is in view.
            if let Some(w) = tab.scroller.window.as_mut() {
                let grid = w.term.grid();
                let top = grid.history_size() - grid.display_offset();
                let screen = grid.screen_lines();
                w.colour(top.saturating_sub(screen)..top + 2 * screen, &tab.code);
            }
            let tab = &self.tabs[self.active];
            let term = tab.scroller.shown(&live);
            let code: &dyn highlight::CodeRows = match &tab.scroller.window {
                Some(w) => w,
                None => &tab.code,
            };
            let v = View {
                palette: &self.palette,
                search: &tab.search,
                code,
                focused: self.focused && matches!(self.overlay, Overlay::None) && !(tab.chat.open && tab.chat.focused),
                bold_is_bright: cfg.bold_is_bright,
                scale,
                padding: padding_px(cfg, scale),
                top,
                scrollbar_alpha: self.scrollbar.fade.value,
                search_alpha: self.search_fade.value,
                min_contrast: cfg.minimum_contrast,
                gutter: gutter_px(cfg, self.renderer.cell().width, padding_px(cfg, scale)),
                line_numbers: &tab.numbers,
                line_number_mode: cfg.line_numbers,
                bookmarks: &tab.bookmarked,
                flash,
                scroll: Some(scroll),
                width: term_w,
            };
            let c = ChromeView {
                caption: self.caption(),
                maximized: self.window.is_maximized(),
                titles: self.tabs.iter().map(|t| t.label.as_str()).collect(),
                active: self.active,
                hover: self.hover,
                menu_open: matches!(self.overlay, Overlay::Menu(_)),
                menu: match &self.overlay {
                    Overlay::Menu(m) => Some(m),
                    _ => None,
                },
                panel: panel.as_ref(),
                logging: self.tabs.iter().map(|t| t.log.is_some()).collect(),
                activity: self.tabs.iter().map(|t| t.activity).collect(),
                toast: self.toast.as_ref().map(|(t, _)| t.as_str()),
                size: size_text.as_deref(),
                chat: chat.as_ref(),
                chat_focused,
            };
            self.renderer.build(|p| view::draw(p, term, &v, &c));
        }
        let t1 = Instant::now();
        let shown = self.renderer.present(self.palette.background);
        let end = Instant::now();
        self.frames.add(t1 - t0, end - t1);
        self.frame_cost = end - t0;
        self.last_frame = end;
        if !shown {
            // The surface was lost and set up again: draw again soon, not
            // at once, in case it stays lost for a while.
            let retry = end + Duration::from_millis(50);
            self.wake_at = Some(self.wake_at.map_or(retry, |t| t.min(retry)));
        }

        let tab = &self.tabs[self.active];
        if animating || self.renderer.needs_rebuild() || tab.search.recount_pending() || tab.search.boxes.iter().any(|b| b.counting) {
            self.window.request_redraw();
        }
    }

    /// x (in pixels) is over the gutter's times or numbers.
    fn in_gutter(&self, cfg: &Config, x: f32) -> bool {
        let pad = padding_px(cfg, self.scale());
        let gutter = gutter_px(cfg, self.renderer.cell().width, pad);
        gutter > 0.0 && x < pad + gutter
    }

    /// The client and model the settings describe; else says what is
    /// missing, opening Options → AI when something is not set up yet.
    fn ai_client(&mut self, cfg: &Config) -> Option<(ai::client::Client, String)> {
        let model = cfg.ai.model_or_default();
        let client = match ai::client::Client::new(&cfg.ai, &self.ai_keys) {
            Ok(_) if model.is_empty() => Err(ai::AiError::NoModel),
            r => r,
        };
        match client {
            Ok(c) => Some((c, model)),
            // Not set up yet: the page where it is.
            Err(e @ (ai::AiError::NoProvider | ai::AiError::NoKey | ai::AiError::NoModel | ai::AiError::NoAddress)) => {
                self.open_ai_options(cfg);
                self.show(format!("AI: {e}"));
                None
            }
            Err(e) => {
                self.show(format!("AI: {e}"));
                None
            }
        }
    }

    /// Asks the AI to fix the command on global line `at` (None: the last
    /// command), with the commands before it as context.
    fn ai_fix(&mut self, cfg: &Config, at: Option<u64>) {
        let provider = cfg.ai.provider;
        let Some((client, model)) = self.ai_client(cfg) else { return };
        if !cfg.ai.confirmed.contains(&provider) {
            self.overlay = Overlay::AiConfirm(Confirm::Fix(at));
            self.panel_offset = (0.0, 0.0);
            self.window.request_redraw();
            return;
        }
        let shell = shell_name(cfg);
        let tab = &mut self.tabs[self.active];
        let mut live = tab.session.term.lock();
        if live.mode().contains(TermMode::ALT_SCREEN) {
            drop(live);
            return self.show("Fix works on the normal screen, not in full-screen programs".to_owned());
        }
        let snap = crate::history::scroll::settled(&mut live, &tab.session.history);
        let grid = live.grid();
        let end = tab.lines.cursor_line(grid);
        let env = ai::context::Env { shell, os: std::env::consts::OS, program: tab.app.clone() };
        let mut text = TabText { snap: &snap, grid, lines: &mut tab.lines, end };
        let first = ai::context::first_message(&mut text, &env, cfg.ai.listed_commands, at);
        drop(live);
        let Some(context) = first else {
            return self.show("No command to fix here".to_owned());
        };
        self.ai_budget = ai::context::Budget::new();
        let context = self.ai_budget.take_up_to(context, ai::context::FIRST_MAX);
        self.ai_bookmarked.clear();
        self.ai_label = ai_label(&client, provider);
        let tab_id = tab.id;
        let job = self.ai_job_number();
        self.ai_job = Some((job, tab_id));
        ai::fix::start(job, tab_id, client, model, context, self.proxy.clone());
        self.show_for(format!("Asking {}… Esc cancels", provider.label()), Duration::from_secs(120));
    }

    /// The active tab's chat panel, laid out; None while it is closed.
    fn chat_layout(&self, cfg: &Config) -> Option<chat_ui::ChatLayout> {
        let tab = self.tabs.get(self.active)?;
        if !tab.chat.open {
            return None;
        }
        let (w, h) = self.renderer.size();
        let px = self.chat_px(cfg, tab);
        let top = self.top();
        let area = ui::Rect { x: w as f32 - px, y: top, w: px, h: h as f32 - top };
        let (ui, term) = (self.renderer.ui_cell(), self.renderer.cell());
        let title = format!("{} · {}", cfg.ai.provider.label(), cfg.ai.model_or_default());
        let chat = &tab.chat;
        let status = if chat.chat.job.is_some() {
            Some(chat.activity.clone())
        } else if chat.chat.near_limit() {
            Some("This chat is long; start a New chat".to_owned())
        } else {
            chat.about.map(|line| format!("Asking about the command on line {}", line + 1))
        };
        Some(chat_ui::layout(area, (ui.width, ui.height), (term.width, term.height), self.scale(), chat, &title, status))
    }

    /// Opens the active tab's chat with the focus in it; `about` is the
    /// command (global line) the next message asks about.
    fn open_chat(&mut self, cfg: &Config, about: Option<u64>) {
        if self.ai_client(cfg).is_none() {
            return;
        }
        let tab = self.tab_mut();
        tab.search.blur();
        tab.chat.focused = true;
        if about.is_some() {
            tab.chat.about = about;
        }
        if !tab.chat.open {
            tab.chat.open = true;
            self.resize_grid(cfg);
        }
        self.window.request_redraw();
    }

    /// `ai_chat`: opens the chat, or moves the focus between it and the terminal.
    fn toggle_chat(&mut self, cfg: &Config) {
        let chat = &self.tab().chat;
        if chat.open && chat.focused {
            self.tab_mut().chat.focused = false;
            self.window.request_redraw();
        } else {
            self.open_chat(cfg, None);
        }
    }

    fn close_chat(&mut self, cfg: &Config) {
        let chat = &mut self.tab_mut().chat;
        chat.open = false;
        chat.focused = false;
        self.resize_grid(cfg);
        self.window.request_redraw();
    }

    /// Sends the active tab's message (`typed`; else Continue: only the
    /// results of the run commands) with the tab's commands and output.
    fn send_chat(&mut self, cfg: &Config, typed: bool) {
        let chat = &self.tab().chat;
        if chat.chat.job.is_some() || (typed && chat.input.is_empty()) {
            return;
        }
        let provider = cfg.ai.provider;
        let Some((client, model)) = self.ai_client(cfg) else { return };
        if !cfg.ai.confirmed.contains(&provider) {
            self.overlay = Overlay::AiConfirm(Confirm::Chat);
            self.panel_offset = (0.0, 0.0);
            self.window.request_redraw();
            return;
        }
        let shell = shell_name(cfg);
        let tab = &mut self.tabs[self.active];
        let text = typed.then(|| tab.chat.input.take());
        let format = client.format();
        let mut budget = ai::context::Budget::new();
        let mut live = tab.session.term.lock();
        let history = if live.mode().contains(TermMode::ALT_SCREEN) {
            const FULL: &str = "The terminal shows a full-screen program; its lines are not sent.";
            tab.chat.chat.ask(format, text.as_deref(), FULL, |_| FULL.to_owned())
        } else {
            let snap = crate::history::scroll::settled(&mut live, &tab.session.history);
            let grid = live.grid();
            let end = tab.lines.cursor_line(grid);
            let env = ai::context::Env { shell, os: std::env::consts::OS, program: tab.app.clone() };
            let mut t = TabText { snap: &snap, grid, lines: &mut tab.lines, end };
            let context = ai::context::chat_context(&mut t, &env, cfg.ai.listed_commands, tab.chat.about);
            let context = budget.take_up_to(context, ai::chat::CONTEXT_MAX);
            tab.chat.chat.ask(format, text.as_deref(), &context, |line| {
                use ai::context::TerminalText;
                match ai::context::command_of(&t.commands(), line) {
                    Some(i) => budget.take(ai::context::output(&mut t, i)),
                    None => "(no output)".to_owned(),
                }
            })
        };
        drop(live);
        let label = ai_label(&client, provider);
        let job = self.ai_job_number();
        let tab = &mut self.tabs[self.active];
        tab.chat.budget = budget;
        tab.chat.bookmarked.clear();
        tab.chat.about = None;
        tab.chat.scroll = 0.0;
        tab.chat.chat.job = Some(job);
        tab.chat.activity = format!("Asking {label}… Esc stops");
        ai::chat::start(job, tab.id, client, model, history, self.proxy.clone());
        self.window.request_redraw();
    }

    /// A click on a chat button.
    fn on_chat_hit(&mut self, cfg: &Config, hit: chat_ui::ChatHit) {
        use ai::chat::{CardState, Entry};
        use chat_ui::ChatHit as H;
        let tab_id = self.tab().id;
        let card_command = |st: &Self, i: usize| match st.tab().chat.chat.entries.get(i) {
            Some(Entry::Card(c)) => Some(c.command.clone()),
            _ => None,
        };
        let set_state = |st: &mut Self, i: usize, state: CardState| {
            if let Some(Entry::Card(c)) = st.tab_mut().chat.chat.entries.get_mut(i) {
                c.state = state;
            }
        };
        const NOT_TYPED: &str = "Nothing typed: a full-screen program is open";
        match hit {
            H::Divider | H::Input | H::Messages => {}
            H::NewChat => self.tab_mut().chat.new_chat(),
            H::Close => return self.close_chat(cfg),
            H::Copy(i) => {
                let text = match self.tab().chat.chat.entries.get(i) {
                    Some(Entry::User(t) | Entry::Ai(t) | Entry::Note(t)) => t.clone(),
                    Some(Entry::Card(c)) => c.command.clone(),
                    None => return,
                };
                if let Some(cb) = self.clipboard.as_mut()
                    && let Err(e) = cb.set_text(text)
                {
                    log::warn!("copy failed: {e}");
                }
            }
            H::Insert(i) => {
                let Some(command) = card_command(self, i) else { return };
                if self.insert_command(tab_id, &command) {
                    set_state(self, i, CardState::Inserted);
                } else {
                    self.tab_mut().chat.chat.entries.push(Entry::Note(NOT_TYPED.to_owned()));
                }
            }
            H::Run(i) => {
                let Some(command) = card_command(self, i) else { return };
                if !self.insert_command(tab_id, &command) {
                    self.tab_mut().chat.chat.entries.push(Entry::Note(NOT_TYPED.to_owned()));
                    return self.window.request_redraw();
                }
                let (line, ran) = self.tab_mut().note_enter(&ai::fix::insert_text(&command));
                self.write(b"\r".as_slice());
                if let Some(name) = ran {
                    self.tab_mut().app = name;
                    self.relabel();
                }
                set_state(self, i, CardState::Ran { line });
            }
            H::Continue(_) => self.send_chat(cfg, false),
        }
        self.window.request_redraw();
    }

    /// A key while the chat has the focus. Nothing reaches the shell.
    fn on_chat_key(&mut self, cfg: &Config, event: &KeyEvent) {
        let m = self.mods;
        let chat = &mut self.tabs[self.active].chat;
        let input = &mut chat.input;
        match &event.logical_key {
            Key::Named(NamedKey::Enter) if m.shift_key() => input.newline(),
            Key::Named(NamedKey::Enter) => self.send_chat(cfg, true),
            Key::Named(NamedKey::Escape) if chat.chat.job.is_some() => {
                chat.chat.job = None;
                chat.chat.entries.push(ai::chat::Entry::Note("Stopped".to_owned()));
            }
            Key::Named(NamedKey::Escape) => chat.focused = false,
            Key::Named(NamedKey::Backspace) => input.backspace(),
            Key::Named(NamedKey::Delete) => input.delete(),
            Key::Named(NamedKey::ArrowLeft) => input.left(),
            Key::Named(NamedKey::ArrowRight) => input.right(),
            Key::Named(NamedKey::Home) => input.home(),
            Key::Named(NamedKey::End) => input.end(),
            Key::Character(c) if (m.control_key() || m.super_key()) && c.eq_ignore_ascii_case("v") => self.paste(),
            _ => {
                if m.control_key() || m.super_key() {
                    return;
                }
                if let Some(text) = event.text.as_deref() {
                    input.insert(text);
                }
            }
        }
        self.window.request_redraw();
    }

    /// Mouse over the chat panel (or its divider): hover, cursor, and the
    /// divider's drag. False when the pointer is elsewhere.
    fn on_chat_mouse_move(&mut self, cfg: &Config, x: f32, y: f32) -> bool {
        let Some(l) = self.chat_layout(cfg) else { return false };
        if self.chat_drag.is_some() {
            let w = self.renderer.size().0 as f32;
            self.chat_drag = Some(chat_share(w, x));
            self.resize_grid(cfg);
            self.set_cursor(CursorIcon::ColResize);
            self.window.request_redraw();
            return true;
        }
        if self.selecting || !(l.area.contains(x, y) || l.divider.contains(x, y)) {
            return false;
        }
        let hit = l.hit(x, y);
        let hover = match hit {
            Some(chat_ui::ChatHit::Input | chat_ui::ChatHit::Messages | chat_ui::ChatHit::Divider) | None => Hover::None,
            Some(h) => Hover::Chat(h),
        };
        if hover != self.hover || self.scrollbar.hover || self.search_hover {
            self.hover = hover;
            self.scrollbar.hover = false;
            self.search_hover = false;
            self.window.request_redraw();
        }
        self.set_cursor(match hit {
            Some(chat_ui::ChatHit::Divider) => CursorIcon::ColResize,
            Some(chat_ui::ChatHit::Input) => CursorIcon::Text,
            Some(chat_ui::ChatHit::Messages) | None => CursorIcon::Default,
            Some(_) => CursorIcon::Pointer,
        });
        true
    }

    /// A press on the chat panel; false when it was elsewhere.
    fn on_chat_press(&mut self, cfg: &Config, button: WinitButton) -> bool {
        let (x, y) = self.mouse;
        let Some(l) = self.chat_layout(cfg) else { return false };
        if !(l.area.contains(x, y) || l.divider.contains(x, y)) {
            return false;
        }
        if button != WinitButton::Left {
            return true;
        }
        let hit = l.hit(x, y);
        if hit == Some(chat_ui::ChatHit::Divider) {
            self.chat_drag = Some(cfg.ai.panel_width);
            return true;
        }
        let tab = self.tab_mut();
        tab.search.blur();
        tab.chat.focused = true;
        if let Some(hit) = hit {
            self.on_chat_hit(cfg, hit);
        }
        self.window.request_redraw();
        true
    }

    /// The wheel over the chat panel scrolls its messages; false elsewhere.
    fn on_chat_wheel(&mut self, cfg: &Config, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let Some(l) = self.chat_layout(cfg) else { return false };
        if !l.area.contains(x, y) {
            return false;
        }
        let step = 3.0 * self.renderer.ui_cell().height;
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, dy) => dy * step,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        let chat = &mut self.tab_mut().chat;
        chat.scroll = (chat.scroll + dy).clamp(0.0, l.content_height);
        self.window.request_redraw();
        true
    }

    /// An answer from an AI request thread.
    fn on_ai(&mut self, event: AiEvent) {
        let signed_out = matches!(
            &event,
            AiEvent::Fixed { result: Err(ai::AiError::SignInAgain), .. }
                | AiEvent::ChatTurn { result: Err(ai::AiError::SignInAgain), .. }
                | AiEvent::Models { result: Err(ai::AiError::SignInAgain), .. }
                | AiEvent::Tested { result: Err(ai::AiError::SignInAgain), .. }
        );
        if signed_out {
            self.ai_account = chatgpt_account();
            self.ai_grok_account = grok_account();
        }
        match event {
            AiEvent::ChatTurn { job, tab, result } => {
                let Some(i) = self.tabs.iter().position(|t| t.id == tab) else { return };
                let chat = &mut self.tabs[i].chat;
                if chat.chat.job != Some(job) {
                    return;
                }
                chat.chat.job = None;
                match result {
                    Ok(turn) => chat.chat.on_turn(turn),
                    Err(e) => chat.chat.entries.push(ai::chat::Entry::Note(format!("AI: {e}"))),
                }
                if !chat.bookmarked.is_empty() {
                    let lines: Vec<String> = chat.bookmarked.drain(..).map(|l| (l + 1).to_string()).collect();
                    let s = if lines.len() == 1 { "" } else { "s" };
                    chat.chat.entries.push(ai::chat::Entry::Note(format!("Bookmarked line{s} {}", lines.join(", "))));
                }
                chat.scroll = 0.0;
                self.window.request_redraw();
            }
            AiEvent::Fixed { job, tab, result } => {
                if self.ai_job.map(|(j, _)| j) != Some(job) {
                    return;
                }
                self.ai_job = None;
                let mut bookmarks: String = self.ai_bookmarked.iter().map(|l| format!(" · Bookmarked line {}", l + 1)).collect();
                if !bookmarks.is_empty() {
                    self.window.request_redraw();
                }
                match result {
                    Ok(fix) => {
                        if let Some(command) = fix.command {
                            self.insert_command(tab, &command);
                        }
                        bookmarks.insert_str(0, &fix.explanation);
                        self.show(bookmarks);
                    }
                    Err(e) => self.show(format!("AI: {e}{bookmarks}")),
                }
            }
            AiEvent::Tools { job, tab, calls, reply } => {
                // A cancelled Fix or chat message, or its tab closed:
                // dropping `reply` ends it.
                let Some(i) = self.tabs.iter().position(|t| t.id == tab) else { return };
                let fix = self.ai_job == Some((job, tab));
                if !fix && self.tabs[i].chat.chat.job != Some(job) {
                    return;
                }
                let t = &mut self.tabs[i];
                // The Fix's budget and bookmarks live on the state, a chat's on its tab.
                let (mut budget, mut bookmarked) = if fix {
                    (std::mem::replace(&mut self.ai_budget, ai::context::Budget::new()), std::mem::take(&mut self.ai_bookmarked))
                } else {
                    (std::mem::replace(&mut t.chat.budget, ai::context::Budget::new()), std::mem::take(&mut t.chat.bookmarked))
                };
                let mut live = t.session.term.lock();
                let results: Vec<String> = if live.mode().contains(TermMode::ALT_SCREEN) {
                    calls.iter().map(|_| "the terminal shows a full-screen program".to_owned()).collect()
                } else {
                    let snap = crate::history::scroll::settled(&mut live, &t.session.history);
                    let grid = live.grid();
                    let end = t.lines.cursor_line(grid);
                    let mut text = TabText { snap: &snap, grid, lines: &mut t.lines, end };
                    calls.iter().map(|c| budget.take(ai::context::run_tool(&mut text, &c.name, &c.input, &mut bookmarked))).collect()
                };
                drop(live);
                let _ = reply.send(results);
                if fix {
                    self.ai_budget = budget;
                    self.ai_bookmarked = bookmarked;
                } else {
                    t.chat.budget = budget;
                    t.chat.bookmarked = bookmarked;
                }
                if let Some(line) = calls.iter().find(|c| c.name == "get_output").and_then(|c| c.input["line"].as_i64()) {
                    let reading = format!("reading the output of line {line}");
                    if fix {
                        self.show_for(format!("Asking {}… {reading}", self.ai_label), Duration::from_secs(120));
                    } else {
                        let a = &mut self.tabs[i].chat.activity;
                        let asking = a.split('…').next().unwrap_or("Asking").to_owned();
                        *a = format!("{asking}… {reading} · Esc stops");
                    }
                }
                self.window.request_redraw();
            }
            AiEvent::Models { job, result } => {
                if self.ai_models_job != Some(job) {
                    return;
                }
                self.ai_models_job = None;
                match result {
                    Ok(models) => {
                        self.ai_status = if models.is_empty() { "The provider lists no models".to_owned() } else { String::new() };
                        self.ai_models = models.into();
                    }
                    Err(e) => self.ai_status = e.to_string(),
                }
                self.ai_scroll = self.ai_models.iter().position(|m| m == &self.ai_model_shown).unwrap_or(0).saturating_sub(3);
                self.window.request_redraw();
            }
            AiEvent::DeviceCode { job, code, page } => {
                let Some(signin) = self.ai_signin.as_mut().filter(|s| s.job == job) else { return };
                signin.code = Some((code.clone(), page));
                // Said where it is seen even with Options closed.
                self.show_for(format!("Grok sign-in: enter code {code} on xAI's page (opened in your browser)"), Duration::from_secs(300));
                self.window.request_redraw();
            }
            AiEvent::SignedIn { job, provider, result } => {
                if self.ai_signin.as_ref().map(|s| s.job) != Some(job) {
                    return;
                }
                self.ai_signin = None;
                self.window.request_redraw();
                // Said where it is seen even with Options closed.
                self.show(ai::login::outcome_text(provider.label(), &result));
                match result {
                    Ok(_) if provider == ai::Provider::OpenRouter => {
                        // The sign-in saved the key it got in the keys file.
                        self.ai_keys = ai::Keys::path().map(|p| ai::Keys::load(&p)).unwrap_or_default();
                        self.ai_status = String::new();
                        self.ai_reset_model = true;
                    }
                    Ok(email) if provider == ai::Provider::Xai => {
                        self.ai_status = String::new();
                        self.ai_grok_account = Some(email);
                        self.ai_reset_model = true;
                    }
                    Ok(email) => {
                        self.ai_status = String::new();
                        self.ai_account = Some(email);
                        let welcomed = ai::login::Login::path().is_some_and(|p| ai::login::Login::load(&p).welcomed);
                        if !welcomed {
                            self.overlay = Overlay::AiWelcome;
                            self.panel_offset = (0.0, 0.0);
                        }
                        self.ai_reset_model = true;
                    }
                    Err(e) => self.ai_status = e.to_string(),
                }
            }
            AiEvent::Tested { job, result } => {
                if self.ai_test_job != Some(job) {
                    return;
                }
                self.ai_test_job = None;
                self.ai_status = match result {
                    Ok(()) => "Works".to_owned(),
                    Err(e) => e.to_string(),
                };
                self.window.request_redraw();
            }
        }
    }

    /// Options → AI → Continue with ChatGPT (or Grok, or Sign in with
    /// OpenRouter): opens the browser to
    /// sign in; pressed again while waiting (Cancel), stops waiting.
    fn sign_in_or_cancel(&mut self, provider: ai::Provider) {
        if let Some(signin) = self.ai_signin.take() {
            signin.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.ai_status = "Sign-in cancelled".to_owned();
            return;
        }
        let job = self.ai_job_number();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.ai_signin = Some(SignIn { job, provider, cancel: cancel.clone(), code: None });
        self.ai_status.clear();
        match provider {
            ai::Provider::Xai => ai::grok::sign_in(job, self.proxy.clone(), cancel),
            ai::Provider::OpenRouter => ai::openrouter::sign_in(job, self.proxy.clone(), cancel),
            _ => ai::login::sign_in(job, self.proxy.clone(), cancel),
        }
    }

    /// Options → AI → Sign out, for `provider`.
    fn sign_out(&mut self, provider: ai::Provider) {
        if provider == ai::Provider::Xai {
            self.sign_out_grok();
        } else {
            self.sign_out_chatgpt();
        }
    }

    /// Forgets the Grok tokens here and asks xAI to revoke them (in the
    /// background).
    fn sign_out_grok(&mut self) {
        self.ai_grok_account = None;
        let Some(path) = ai::grok::GrokLogin::path() else { return };
        let _one = ai::login::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = ai::grok::GrokLogin::load(&path);
        let _ = std::thread::Builder::new().name("ai-logout".into()).spawn(move || ai::grok::revoke(&ai::login::agent(), &ai::grok::auth_base(), &old));
        match ai::grok::GrokLogin::default().save(&path) {
            Ok(()) => self.ai_status = "Signed out".to_owned(),
            Err(e) => self.ai_status = format!("could not save the sign-out: {e}"),
        }
    }

    /// Forgets the ChatGPT tokens here and asks OpenAI to revoke them (in
    /// the background).
    fn sign_out_chatgpt(&mut self) {
        self.ai_account = None;
        let Some(path) = ai::login::Login::path() else { return };
        let mut login = {
            let _one = ai::login::LOCK.lock().unwrap_or_else(|e| e.into_inner());
            ai::login::Login::load(&path)
        };
        let old = login.clone();
        let _ = std::thread::Builder::new().name("ai-logout".into()).spawn(move || ai::login::revoke(&ai::login::agent(), &ai::login::auth_base(), &old));
        login.sign_out();
        let _one = ai::login::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        match login.save(&path) {
            Ok(()) => self.ai_status = "Signed out".to_owned(),
            Err(e) => self.ai_status = format!("could not save the sign-out: {e}"),
        }
    }

    /// Opens Options on the AI page.
    fn open_ai_options(&mut self, cfg: &Config) {
        self.overlay = Overlay::Options;
        self.options_page = OptionsPage::Ai;
        self.panel_offset = (0.0, 0.0);
        self.capturing = None;
        self.load_ai_models(cfg);
        self.window.request_redraw();
    }

    /// A new job number for an AI request.
    fn ai_job_number(&mut self) -> u64 {
        self.next_ai_job += 1;
        self.next_ai_job
    }

    /// Asks the provider for its models, for Options → AI.
    fn load_ai_models(&mut self, cfg: &Config) {
        self.ai_models = Vec::new().into();
        self.ai_model_shown = cfg.ai.model_or_default();
        match ai::client::Client::new(&cfg.ai, &self.ai_keys) {
            Ok(client) => {
                let job = self.ai_job_number();
                self.ai_models_job = Some(job);
                self.ai_status = "Loading models…".to_owned();
                ai::fix::list_models(job, client, self.proxy.clone());
            }
            Err(ai::AiError::NoProvider) => self.ai_status.clear(),
            Err(e) => self.ai_status = e.to_string(),
        }
    }

    /// Options → AI → Paste key: the clipboard's text, if it looks like a key.
    fn paste_ai_key(&mut self, cfg: &Config) {
        let text = self.clipboard_text().unwrap_or_default();
        let key = text.trim();
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            self.ai_status = "Copy the key first: the clipboard has no key".to_owned();
            return;
        }
        let key = key.to_owned();
        if self.set_ai_key(cfg, Some(key)) {
            self.load_ai_models(cfg);
        }
    }

    /// Saves (or removes) the provider's key; false if it couldn't be saved.
    fn set_ai_key(&mut self, cfg: &Config, key: Option<String>) -> bool {
        self.ai_keys.set(cfg.ai.provider, key);
        let saved = match ai::Keys::path() {
            Some(path) => self.ai_keys.save(&path).map_err(|e| format!("could not save the key: {e}")),
            None => Err("no settings folder to keep the key in".to_owned()),
        };
        match saved {
            Ok(()) => true,
            Err(e) => {
                self.ai_status = e;
                false
            }
        }
    }

    /// Options → AI → Test: one short request with the key and model.
    fn test_ai(&mut self, cfg: &Config) {
        let model = cfg.ai.model_or_default();
        match ai::client::Client::new(&cfg.ai, &self.ai_keys) {
            Ok(_) if model.is_empty() => self.ai_status = ai::AiError::NoModel.to_string(),
            Ok(client) => {
                let job = self.ai_job_number();
                self.ai_test_job = Some(job);
                self.ai_status = "Testing…".to_owned();
                ai::fix::test(job, client, model, self.proxy.clone());
            }
            Err(e) => self.ai_status = e.to_string(),
        }
    }

    /// Types `command` at the prompt of the tab with id `tab`, without
    /// running it.
    /// Types `command` at the prompt of the tab with id `tab`, without
    /// running it; false if nothing was typed (a full-screen program is
    /// open, or the command is empty).
    fn insert_command(&mut self, tab: u64, command: &str) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.id == tab) else { return false };
        let text = ai::fix::insert_text(command);
        if text.is_empty() {
            return false;
        }
        let tab = &mut self.tabs[i];
        let mut live = tab.session.term.lock();
        let mode = *live.mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            return false;
        }
        tab.scroller.go_bottom(&mut live);
        live.selection = None;
        drop(live);
        tab.session.write(input::paste_bytes(&text, mode));
        self.window.request_redraw();
        true
    }

    /// Viewport cell under the pointer: (grid point, side, col, row).
    fn mouse_cell(&self, cfg: &Config, display_offset: usize) -> (Point, Side, usize, usize) {
        let cell = self.renderer.cell();
        let pad = padding_px(cfg, self.scale());
        let gutter = gutter_px(cfg, cell.width, pad);
        let (col, row, side) = cell_at(self.mouse, self.top(), pad, gutter, (cell.width, cell.height), self.grid);
        (Point::new(Line(row as i32 - display_offset as i32), Column(col)), side, col, row)
    }

    /// What the right-click menu at the pointer offers for bookmarks.
    /// Lines are only numbered on the normal screen.
    fn bookmark_menu(&mut self, cfg: &Config) -> BookmarkMenu {
        let offset = self.tab_mut().shown(|t| t.grid().display_offset());
        let (_, _, _, row) = self.mouse_cell(cfg, offset);
        let over_text = self.mouse.1 >= self.top();
        let tab = &mut self.tabs[self.active];
        tab.sync_lines();
        let live = tab.session.term.lock();
        if live.mode().contains(TermMode::ALT_SCREEN) {
            return BookmarkMenu::default();
        }
        let grid = tab.scroller.shown(&live).grid();
        let abs = grid.history_size() - offset + row;
        let target = if over_text { tab.lines.bookmark_target(grid, abs) } else { None };
        BookmarkMenu { target, any: tab.lines.has_bookmarks() }
    }

    /// The bookmark key: adds or removes a bookmark without a clicked row
    /// (see `lines::key_bookmark_row`).
    fn toggle_bookmark_by_key(&mut self, cfg: &Config) {
        let offset = self.tab_mut().shown(|t| t.grid().display_offset());
        let (_, _, _, pointer_row) = self.mouse_cell(cfg, offset);
        let tab = &mut self.tabs[self.active];
        tab.sync_lines();
        let live = tab.session.term.lock();
        let grid = tab.scroller.shown(&live).grid();
        // In a window onto older lines there is no cursor: the pointer's row.
        let row = if tab.scroller.in_window() {
            grid.history_size() - grid.display_offset() + pointer_row
        } else {
            lines::key_bookmark_row(grid, pointer_row)
        };
        if let Some((line, _)) = tab.lines.bookmark_target(grid, row) {
            tab.lines.toggle_bookmark(line);
        }
        drop(live);
        self.window.request_redraw();
    }

    /// Scrolls to the next bookmark down (or up) in the active tab.
    fn jump_to_bookmark(&mut self, down: bool) {
        let tab = &mut self.tabs[self.active];
        tab.sync_lines();
        let mut live = tab.session.term.lock();
        if live.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        // A jump while the last one still glides goes on from where that
        // one lands.
        if let Some(g) = tab.glide.take() {
            let budget = tab.session.budget();
            tab.scroller.show_line(&mut live, &tab.session.history, &budget, g.line);
        }
        let Some(line) = tab.lines.step_bookmark(tab.scroller.shown(&live).grid(), down) else {
            drop(live);
            self.show("No bookmarks in this tab".to_owned());
            return;
        };
        let budget = tab.session.budget();
        let history = &tab.session.history;
        let before = tab.scroller.metrics(&live, history);
        tab.scroller.show_line(&mut live, history, &budget, line);
        let (total, offset) = tab.scroller.metrics(&live, history);
        // Glide there, over at most a couple of screens: from further away
        // the view jumps that close first.
        let to = total - offset;
        let reach = live.grid().screen_lines() * 2;
        let from = (before.0 - before.1).clamp(to.saturating_sub(reach), to + reach);
        tab.glide = None;
        if from != to {
            tab.scroller.set_offset(&mut live, history, &budget, total - from.min(total));
            let (total, offset) = tab.scroller.metrics(&live, history);
            tab.glide = Some(Glide { from, to, line, start: Instant::now(), seen: total - offset });
        }
        tab.flash = Some((line, Instant::now()));
        drop(live);
        self.scrolled();
        self.window.request_redraw();
    }

    fn has_selection(&mut self) -> bool {
        self.tab_mut().shown(|t| t.selection.as_ref().is_some_and(|s| !s.is_empty()))
    }

    fn copy_selection(&mut self) {
        let text = self.tab_mut().shown(|t| t.selection_to_string()).map(|t| logging::keep_last_lines(t, logging::COPY_LINES).0);
        if let (Some(text), Some(cb)) = (text, self.clipboard.as_mut())
            && let Err(e) = cb.set_text(text)
        {
            log::warn!("copy failed: {e}");
        }
    }

    fn clipboard_text(&mut self) -> Option<String> {
        self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
    }

    fn paste(&mut self) {
        let Some(text) = self.clipboard_text() else { return };
        let tab = &mut self.tabs[self.active];
        if tab.chat.open && tab.chat.focused {
            tab.chat.input.insert(&text);
            self.window.request_redraw();
            return;
        }
        if tab.search.focused {
            tab.search_with(|search, term| search.type_text(text.lines().next().unwrap_or(""), term));
            return;
        }
        let mode = *tab.session.term.lock().mode();
        self.write(input::paste_bytes(&text, mode));
    }

    fn scrolled(&mut self) {
        self.scrollbar.last_scroll = Some(Instant::now());
        self.wake_at = Some(Instant::now() + ui::SCROLL_ACTIVE_FOR + Duration::from_millis(20));
    }

    /// Handles a key press; returns an action for the app to perform.
    fn on_key(&mut self, cfg: &Config, event: KeyEvent) -> Option<Action> {
        if event.state != ElementState::Pressed {
            return None;
        }
        let m = self.mods;

        // Menus and panels take the keyboard while open.
        match &mut self.overlay {
            Overlay::Menu(menu) => {
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => self.overlay = Overlay::None,
                    Key::Named(NamedKey::ArrowDown) => menu.step(true),
                    Key::Named(NamedKey::ArrowUp) => menu.step(false),
                    Key::Named(NamedKey::Enter) => {
                        let action = menu.hover.and_then(|i| menu.items[i].action);
                        self.overlay = Overlay::None;
                        self.window.request_redraw();
                        return action;
                    }
                    _ => {}
                }
                self.window.request_redraw();
                return None;
            }
            Overlay::Options if self.capturing.is_some() => {
                let cmd = self.capturing.unwrap_or(Command::NewTab);
                self.window.request_redraw();
                let table = match event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        self.capturing = None;
                        return None;
                    }
                    Key::Named(NamedKey::Backspace | NamedKey::Delete) if m.is_empty() => self.keymap.rebind(cmd, None),
                    // Modifier keys alone wait for the rest of the combination.
                    _ => self.keymap.rebind(cmd, Some(Combo::from_event(&event, m)?)),
                };
                self.capturing = None;
                self.pending_keys = Some(table);
                return Some(Action::ApplyKeys);
            }
            Overlay::Options if self.options_page == OptionsPage::Font => {
                // ↑ ↓ pick the previous or next font; Esc and Enter close.
                let down = match event.logical_key {
                    Key::Named(NamedKey::ArrowUp) => false,
                    Key::Named(NamedKey::ArrowDown) => true,
                    Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                        self.overlay = Overlay::None;
                        self.window.request_redraw();
                        return None;
                    }
                    _ => return None,
                };
                let len = self.font_families.len();
                let next = match self.options_state(cfg).font_index() {
                    Some(i) if down => (i + 1).min(len.saturating_sub(1)),
                    Some(i) => i.saturating_sub(1),
                    None => 0,
                };
                return (len > 0).then_some(Action::FontFamily(next));
            }
            Overlay::AiWelcome => {
                return matches!(event.logical_key, Key::Named(NamedKey::Enter | NamedKey::Escape)).then_some(Action::AiWelcomeDone);
            }
            Overlay::AiConfirm(_) => {
                return match event.logical_key {
                    Key::Named(NamedKey::Enter) => Some(Action::AiConfirm(true)),
                    Key::Named(NamedKey::Escape) => Some(Action::AiConfirm(false)),
                    _ => None,
                };
            }
            Overlay::Options | Overlay::About => {
                if matches!(event.logical_key, Key::Named(NamedKey::Escape | NamedKey::Enter)) {
                    self.overlay = Overlay::None;
                    self.window.request_redraw();
                }
                return None;
            }
            Overlay::None => {}
        }

        // Esc while a Fix is on its way cancels it; the shell doesn't get it.
        if self.ai_job.is_some() && m.is_empty() && matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            self.ai_job = None;
            self.show("Cancelled".to_owned());
            return None;
        }

        // Shortcuts (Options → Keyboard shortcuts; [keys] in settings).
        if let Some(cmd) = Combo::from_event(&event, m).and_then(|c| self.keymap.command(&c)) {
            let searching = self.tab().search.focused;
            let alt_screen = self.tab().session.term.lock().mode().contains(TermMode::ALT_SCREEN);
            let font = |size: f32| Some(size.clamp(4.0, 96.0));
            let zoom = match cmd {
                Command::FontBigger if !searching => font(self.font_pt + 1.0),
                Command::FontSmaller if !searching => font(self.font_pt - 1.0),
                Command::FontReset if !searching => font(cfg.font_size),
                _ => None,
            };
            if let Some(size) = zoom {
                // Temporary zoom, like a browser; Options changes the saved size.
                self.font_pt = size;
                self.reload_font(cfg);
                self.window.request_redraw();
                return None;
            }
            let scroll = match cmd {
                Command::ScrollPageUp => Some(Scroll::PageUp),
                Command::ScrollPageDown => Some(Scroll::PageDown),
                Command::ScrollTop => Some(Scroll::Top),
                Command::ScrollBottom => Some(Scroll::Bottom),
                _ => None,
            };
            // Full-screen programs (vim, less) get these keys themselves.
            if let Some(s) = scroll.filter(|_| !alt_screen) {
                self.tab_mut().scroll(s);
                self.scrolled();
                self.window.request_redraw();
                return None;
            }
            match cmd {
                Command::NewTab => return Some(Action::NewTab),
                Command::CloseTab => return Some(Action::CloseTab),
                Command::Find => return Some(Action::Find),
                Command::Copy => return Some(Action::Copy),
                Command::Paste => return Some(Action::Paste),
                Command::Options => return Some(Action::Options),
                Command::ToggleBookmark if !alt_screen => {
                    self.toggle_bookmark_by_key(cfg);
                    return None;
                }
                Command::PreviousBookmark if !alt_screen => return Some(Action::PreviousBookmark),
                Command::NextBookmark if !alt_screen => return Some(Action::NextBookmark),
                Command::CycleTheme => return Some(Action::Theme(self.theme_mode.next())),
                Command::AiFix => return Some(Action::AiFix(None)),
                Command::AiChat => return Some(Action::AiChat),
                Command::NextTab | Command::PreviousTab => {
                    self.cycle_tab(cmd == Command::NextTab);
                    return None;
                }
                _ => {}
            }
        }

        // Ctrl+1 … Ctrl+0. Without such a tab the key does nothing, rather
        // than reaching the shell only while there are few tabs.
        if let Some(i) = Combo::from_event(&event, m).and_then(|c| self.keymap.tab_number(&c)) {
            if i < self.tabs.len() {
                self.select_tab(i);
            }
            return None;
        }

        if self.tab().chat.open && self.tab().chat.focused {
            self.on_chat_key(cfg, &event);
            return None;
        }

        let tab = &mut self.tabs[self.active];
        if tab.search.open && matches!(event.logical_key, Key::Named(NamedKey::F3)) {
            let dir = if m.shift_key() { Direction::Right } else { Direction::Left };
            tab.search_with(|search, term| search.step(term, dir));
            self.window.request_redraw();
            return None;
        }

        if tab.search.focused {
            self.on_search_key(&event);
            self.window.request_redraw();
            return None;
        }

        let mode = *tab.session.term.lock().mode();
        let bytes = input::encode_key(&event.logical_key, event.text.as_deref(), m, mode)?;
        let mut ran = None;
        if matches!(event.logical_key, Key::Named(NamedKey::Enter)) && !mode.contains(TermMode::ALT_SCREEN) {
            ran = tab.note_enter("").1;
        }
        // Typing goes back to the live terminal, at the bottom.
        let mut live = tab.session.term.lock();
        tab.scroller.go_bottom(&mut live);
        live.selection = None;
        drop(live);
        self.write(bytes);
        if let Some(name) = ran {
            self.tabs[self.active].app = name;
            self.relabel();
            self.window.request_redraw();
        }
        None
    }

    fn on_search_key(&mut self, event: &KeyEvent) {
        let tab = &mut self.tabs[self.active];
        if tab.scroller.changes() != tab.search_view {
            tab.search.forget_current();
        }
        self.on_search_key_in_view(event);
        self.tabs[self.active].search_follow_up();
    }

    fn on_search_key_in_view(&mut self, event: &KeyEvent) {
        let m = self.mods;
        let tab = &mut self.tabs[self.active];
        let mut live = tab.session.term.lock();
        let term = tab.scroller.shown_mut(&mut live);
        let search = &mut tab.search;
        match &event.logical_key {
            Key::Named(NamedKey::Escape) if search.picker.is_some() => search.picker = None,
            Key::Named(NamedKey::Escape) => search.close(),
            Key::Named(NamedKey::Tab) => search.cycle(!m.shift_key()),
            Key::Named(NamedKey::Enter) => {
                let dir = if m.shift_key() { Direction::Right } else { Direction::Left };
                search.step(term, dir);
            }
            Key::Named(NamedKey::ArrowUp) => search.step(term, Direction::Left),
            Key::Named(NamedKey::ArrowDown) => search.step(term, Direction::Right),
            Key::Named(NamedKey::Backspace) => {
                let mut q = search.query().to_owned();
                if m.control_key() {
                    q.clear();
                } else {
                    q.pop();
                }
                search.set_query(q, term);
            }
            Key::Character(c) if m.control_key() && c.eq_ignore_ascii_case("r") => search.toggle_regex(term),
            Key::Character(c) if m.control_key() && c.eq_ignore_ascii_case("v") => {
                drop(live);
                self.paste()
            }
            _ => {
                if m.control_key() || m.super_key() {
                    return;
                }
                if let Some(text) = event.text.as_deref().filter(|t| t.chars().all(|c| !c.is_control())) {
                    search.type_text(text, term);
                }
            }
        }
    }

    /// What chrome element is under (x, y).
    fn chrome_hover(&self, cfg: &Config, x: f32, y: f32) -> Hover {
        if let Some(panel) = self.panel(cfg) {
            return panel.buttons.iter().position(|b| b.rect.contains(x, y)).map_or(Hover::None, Hover::PanelButton);
        }
        let bar = self.tab_bar();
        if y >= bar.height {
            return Hover::None;
        }
        let over = |r: Option<ui::Rect>| r.is_some_and(|r| r.contains(x, y));
        if over(bar.close) {
            return Hover::CloseWindow;
        }
        if over(bar.maximize) {
            return Hover::Maximize;
        }
        if over(bar.minimize) {
            return Hover::Minimize;
        }
        if bar.menu.contains(x, y) {
            return Hover::MenuButton;
        }
        if bar.plus.contains(x, y) {
            return Hover::Plus;
        }
        for (i, slot) in bar.tabs.iter().enumerate() {
            if slot.close.contains(x, y) {
                return Hover::TabClose(i);
            }
            if slot.rect.contains(x, y) {
                return Hover::Tab(i);
            }
        }
        Hover::None
    }

    fn on_mouse_move(&mut self, cfg: &Config, x: f32, y: f32) {
        self.mouse = (x, y);
        let m = self.metrics();

        if let Some(((sx, sy), (ox, oy))) = self.panel_drag {
            match self.panel(cfg) {
                Some(panel) => {
                    // Keep the offset to where the panel can actually go,
                    // so dragging back from past an edge moves it at once.
                    self.panel_offset = (ox + x - sx, oy + y - sy);
                    let (cx, cy) = ((m.width - panel.rect.w) / 2.0, (m.height - panel.rect.h) / 2.0);
                    self.panel_offset = (panel.rect.x - cx, panel.rect.y - cy);
                    self.set_cursor(CursorIcon::Grabbing);
                    self.window.request_redraw();
                    return;
                }
                // The panel closed under the drag (Esc): the drag is over.
                None => self.panel_drag = None,
            }
        }

        if let Overlay::Menu(menu) = &mut self.overlay {
            let l = menu.layout(&m);
            let hover = menu.hit(&l, x, y);
            if hover != menu.hover && (hover.is_some() || !l.rect.contains(x, y)) {
                menu.hover = hover;
                self.window.request_redraw();
            }
            self.set_cursor(CursorIcon::Default);
            return;
        }
        if matches!(self.overlay, Overlay::None)
            && let Some(dir) = self.resize_edge(x, y)
        {
            self.set_cursor(CursorIcon::from(dir));
            if self.hover != Hover::None {
                self.hover = Hover::None;
                self.window.request_redraw();
            }
            return;
        }
        let hover = self.chrome_hover(cfg, x, y);
        if hover != self.hover {
            self.hover = hover;
            self.window.request_redraw();
        }
        if !matches!(self.overlay, Overlay::None) || y < self.top() {
            // A panel's title strip can be dragged.
            let grab = hover == Hover::None && self.panel(cfg).is_some_and(|p| p.header.contains(x, y));
            self.set_cursor(if grab { CursorIcon::Grab } else { CursorIcon::Default });
            return;
        }
        if self.on_chat_mouse_move(cfg, x, y) {
            return;
        }

        let top = self.top();
        let scale = self.scale();
        let term_w = self.term_width(cfg);
        let tab = &mut self.tabs[self.active];
        let (hist, scrolled) = tab.scroll_metrics();
        let (screen, offset) = tab.shown(|t| (t.grid().screen_lines(), t.grid().display_offset()));
        let geo = ui::scroll_geometry(term_w, top, m.height, scale, hist, screen, scrolled);

        let over_bar = geo.as_ref().is_some_and(|g| g.track.contains(x, y));
        let sg = ui::search_geometry(term_w, top, scale, m.cw, m.ch, tab.search.boxes.len(), tab.search.picker);
        let over_search = tab.search.open && sg.contains(x, y);
        let search_button = match sg.hit(x, y).filter(|_| tab.search.open) {
            None | Some(SearchHit::Bar(_)) => Hover::None,
            Some(h) => Hover::Search(h),
        };
        let redraw = over_bar != self.scrollbar.hover || over_search != self.search_hover || search_button != self.hover;
        self.hover = search_button;
        self.scrollbar.hover = over_bar;
        self.search_hover = over_search;
        // (Inline set_cursor: the terminal lock still borrows `self.tabs`.)
        let icon = if over_bar || over_search { CursorIcon::Default } else { CursorIcon::Text };
        if icon != self.cursor {
            self.cursor = icon;
            self.window.set_cursor(icon);
        }

        if let (Some(grab), Some(g)) = (self.scrollbar.drag, geo.as_ref()) {
            let target = ui::offset_for_thumb(g, y - grab, hist);
            self.tabs[self.active].set_scroll_offset(target);
            self.scrolled();
            self.window.request_redraw();
            return;
        }

        let (point, side, col, row) = self.mouse_cell(cfg, offset);
        let mode = *self.tabs[self.active].session.term.lock().mode();
        if let Some(button) = self.reported_button {
            if mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG) && self.last_reported_cell != Some((col, row)) {
                self.last_reported_cell = Some((col, row));
                if let Some(b) = input::encode_mouse(button, true, true, col, row, self.mods, mode) {
                    self.write(b);
                }
            }
            return;
        }
        if self.selecting {
            self.tab_mut().shown(|term| {
                if let Some(sel) = term.selection.as_mut() {
                    sel.update(point, side);
                }
            });
            self.window.request_redraw();
            return;
        }
        if redraw {
            self.window.request_redraw();
        }
    }

    /// Handles a click; returns an action for the app to perform.
    fn on_mouse_button(&mut self, cfg: &Config, state: ElementState, button: WinitButton) -> Option<Action> {
        let m = self.metrics();
        let (x, y) = self.mouse;
        let pressed = state == ElementState::Pressed;

        // A release ends what the press started, wherever the pointer has
        // got to since: the tab bar, a menu, a panel. Otherwise the drag
        // or selection would go on without a button held.
        if !pressed {
            if button == WinitButton::Left {
                self.selecting = false;
                if let Some(share) = self.chat_drag.take() {
                    self.window.request_redraw();
                    return Some(Action::AiChatWidth(share));
                }
                let dragged = self.panel_drag.take().is_some() | self.scrollbar.drag.take().is_some();
                if dragged {
                    self.window.request_redraw();
                    return None;
                }
            }
            if let Some(b) = self.reported_button.filter(|b| Some(*b) == mouse_button(button)) {
                // A program reporting the mouse gets its release too.
                self.reported_button = None;
                let offset = self.tab_mut().shown(|t| t.grid().display_offset());
                let (_, _, col, row) = self.mouse_cell(cfg, offset);
                let mode = *self.tab().session.term.lock().mode();
                if mode.intersects(TermMode::MOUSE_MODE)
                    && let Some(bytes) = input::encode_mouse(b, false, false, col, row, self.mods, mode)
                {
                    self.write(bytes);
                }
                return None;
            }
        }

        // Open menu: a click picks an item or dismisses the menu.
        if let Overlay::Menu(menu) = &self.overlay {
            if !pressed {
                return None;
            }
            let l = menu.layout(&m);
            let picked = menu.hit(&l, x, y).and_then(|i| menu.items[i].action);
            let on_menu_button = self.tab_bar().menu.contains(x, y);
            self.overlay = Overlay::None;
            self.window.request_redraw();
            if picked.is_none() && !on_menu_button && !l.rect.contains(x, y) && button == WinitButton::Right {
                // Right-click elsewhere reopens the menu of what is there.
                self.overlay = Overlay::Menu(match self.chrome_hover(cfg, x, y) {
                    Hover::Tab(i) | Hover::TabClose(i) => self.tab_menu(x, y, i),
                    _ => {
                        let marks = self.bookmark_menu(cfg);
                        Menu::context(x, y, self.has_selection(), marks, &self.keymap)
                    }
                });
            }
            return picked;
        }
        // Panels are modal.
        if let Some(panel) = self.panel(cfg) {
            if pressed && button == WinitButton::Left {
                if let Some(a) = panel.hit(x, y) {
                    return Some(a);
                }
                if panel.header.contains(x, y) {
                    self.panel_drag = Some(((x, y), self.panel_offset));
                    return None;
                }
                if !panel.rect.contains(x, y) {
                    self.overlay = Overlay::None;
                    self.window.request_redraw();
                }
            }
            return None;
        }

        // Window edges, when the app draws its own frame.
        if pressed
            && button == WinitButton::Left
            && let Some(dir) = self.resize_edge(x, y)
        {
            if let Err(e) = self.window.drag_resize_window(dir) {
                log::warn!("resize: {e}");
            }
            return None;
        }

        // Tab bar.
        if y < self.top() {
            if !pressed {
                return None;
            }
            match (self.chrome_hover(cfg, x, y), button) {
                (Hover::Minimize, WinitButton::Left) => self.window.set_minimized(true),
                (Hover::Maximize, WinitButton::Left) => self.window.set_maximized(!self.window.is_maximized()),
                (Hover::CloseWindow, WinitButton::Left) => return Some(Action::Quit),
                (Hover::None, WinitButton::Left) if self.custom_title_bar => {
                    // Empty bar: drag the window; double-click maximizes.
                    let now = Instant::now();
                    if self.last_bar_click.is_some_and(|t| now - t < MULTI_CLICK) {
                        self.last_bar_click = None;
                        self.window.set_maximized(!self.window.is_maximized());
                    } else {
                        self.last_bar_click = Some(now);
                        if let Err(e) = self.window.drag_window() {
                            log::warn!("move window: {e}");
                        }
                    }
                }
                (Hover::MenuButton, WinitButton::Left) => {
                    let bar = self.tab_bar();
                    let sel = self.has_selection();
                    self.overlay = Overlay::Menu(Menu::main(bar.menu.x + bar.menu.w, bar.height, sel, self.tabs.len(), &self.keymap));
                    self.window.request_redraw();
                }
                (Hover::Plus, WinitButton::Left) => return Some(Action::NewTab),
                (Hover::TabClose(i), WinitButton::Left) | (Hover::Tab(i) | Hover::TabClose(i), WinitButton::Middle) => {
                    self.select_tab(i);
                    return Some(Action::CloseTab);
                }
                (Hover::Tab(i), WinitButton::Left) => self.select_tab(i),
                (Hover::Tab(i) | Hover::TabClose(i), WinitButton::Right) => {
                    self.overlay = Overlay::Menu(self.tab_menu(x, y, i));
                    self.window.request_redraw();
                }
                _ => {}
            }
            return None;
        }

        if pressed && self.on_chat_press(cfg, button) {
            return None;
        }
        if pressed && button == WinitButton::Left {
            // A click in the terminal gives it the keyboard.
            self.tab_mut().chat.focused = false;
        }

        let top = self.top();
        let scale = self.scale();
        let term_w = self.term_width(cfg);
        if pressed && button == WinitButton::Left {
            let tab = &mut self.tabs[self.active];
            if tab.search.open {
                let s = &mut tab.search;
                let g = ui::search_geometry(term_w, top, scale, m.cw, m.ch, s.boxes.len(), s.picker);
                if let Some(hit) = g.hit(x, y) {
                    match hit {
                        SearchHit::Close(i) => s.remove_box(i),
                        SearchHit::Add(i) => s.add_box(i),
                        SearchHit::Color(i) => s.toggle_picker(i),
                        SearchHit::Swatch(i, c) => s.set_color(i, c),
                        SearchHit::Prev(i) | SearchHit::Next(i) => {
                            s.focus(i);
                            s.picker = None;
                            let dir = if hit == SearchHit::Prev(i) { Direction::Left } else { Direction::Right };
                            tab.search_with(|search, term| search.step(term, dir));
                        }
                        SearchHit::Bar(i) => {
                            s.focus(i);
                            if s.picker != Some(i) {
                                s.picker = None;
                            }
                        }
                    }
                    self.window.request_redraw();
                    return None;
                }
            }
            let (hist, scrolled) = tab.scroll_metrics();
            let screen = tab.shown(|t| t.grid().screen_lines());
            if let Some(g) = ui::scroll_geometry(term_w, top, m.height, scale, hist, screen, scrolled)
                && g.track.contains(x, y)
            {
                if g.thumb.contains(x, y) {
                    self.scrollbar.drag = Some(y - g.thumb.y);
                } else {
                    tab.scroll(if y < g.thumb.y { Scroll::PageUp } else { Scroll::PageDown });
                }
                self.scrolled();
                self.window.request_redraw();
                return None;
            }
        }
        // The gutter: a menu for the command beside the click.
        if pressed
            && button == WinitButton::Left
            && self.in_gutter(cfg, x)
            && let Some((line, marked)) = self.bookmark_menu(cfg).target
        {
            self.overlay = Overlay::Menu(Menu::gutter(x, y, line, marked, &self.keymap));
            self.window.request_redraw();
            return None;
        }
        let offset = self.tab_mut().shown(|t| t.grid().display_offset());
        let (point, side, col, row) = self.mouse_cell(cfg, offset);
        let tab = &mut self.tabs[self.active];
        let mut live = tab.session.term.lock();
        let mode = *live.mode();
        let term = tab.scroller.shown_mut(&mut live);

        if mode.intersects(TermMode::MOUSE_MODE) && !self.mods.shift_key() {
            let b = mouse_button(button)?;
            self.reported_button = pressed.then_some(b);
            self.last_reported_cell = Some((col, row));
            if let Some(bytes) = input::encode_mouse(b, pressed, false, col, row, self.mods, mode) {
                drop(live);
                self.write(bytes);
            }
            return None;
        }

        match (button, pressed) {
            (WinitButton::Left, true) => {
                let now = Instant::now();
                let repeat = self.last_click.is_some_and(|(t, p)| now - t < MULTI_CLICK && p == point);
                self.click_count = if repeat { self.click_count % 3 + 1 } else { 1 };
                self.last_click = Some((now, point));
                let ty = match self.click_count {
                    1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                term.selection = Some(Selection::new(ty, point, side));
                drop(live);
                self.selecting = true;
                self.tab_mut().search.blur();
            }
            (WinitButton::Left, false) => {
                self.selecting = false;
                if term.selection.as_ref().is_some_and(Selection::is_empty) {
                    term.selection = None;
                }
            }
            (WinitButton::Right, true) => {
                let sel = term.selection.as_ref().is_some_and(|s| !s.is_empty());
                drop(live);
                let marks = self.bookmark_menu(cfg);
                self.overlay = Overlay::Menu(Menu::context(x, y, sel, marks, &self.keymap));
            }
            (WinitButton::Middle, true) => {
                drop(live);
                self.paste();
            }
            _ => {}
        }
        self.window.request_redraw();
        None
    }

    /// Whole lines the wheel has turned, keeping the fraction for next time.
    fn wheel_lines(&mut self, delta: MouseScrollDelta) -> Option<i32> {
        let cell_h = self.renderer.cell().height;
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 3.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32 / cell_h,
        };
        self.wheel_accum += lines;
        let whole = self.wheel_accum.trunc() as i32;
        if whole == 0 {
            return None;
        }
        self.wheel_accum -= whole as f32;
        Some(whole)
    }

    /// The wheel over the Options panel scrolls its list (Font page).
    fn wheel_panel_list(&mut self, cfg: &Config, delta: MouseScrollDelta) {
        let Some(list) = self.panel(cfg).and_then(|p| p.list) else { return };
        let Some(whole) = self.wheel_lines(delta) else { return };
        let first = list.scrolled(-i64::from(whole));
        if first != list.first {
            if self.options_page == OptionsPage::Ai {
                self.ai_scroll = first;
            } else {
                self.font_scroll = first;
            }
            // The rows moved under the pointer.
            self.hover = self.chrome_hover(cfg, self.mouse.0, self.mouse.1);
            self.window.request_redraw();
        }
    }

    fn on_wheel(&mut self, cfg: &Config, delta: MouseScrollDelta) {
        if matches!(self.overlay, Overlay::Options) {
            self.wheel_panel_list(cfg, delta);
            return;
        }
        if !matches!(self.overlay, Overlay::None) || self.mouse.1 < self.top() {
            return;
        }
        if self.on_chat_wheel(cfg, delta) {
            return;
        }
        let Some(whole) = self.wheel_lines(delta) else { return };

        let offset = self.tab_mut().shown(|t| t.grid().display_offset());
        let (_, _, col, row) = self.mouse_cell(cfg, offset);
        let mode = *self.tabs[self.active].session.term.lock().mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !self.mods.shift_key() {
            let b = if whole > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
            for _ in 0..whole.unsigned_abs() {
                if let Some(bytes) = input::encode_mouse(b, true, false, col, row, self.mods, mode) {
                    self.write(bytes);
                }
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let seq: &'static [u8] = match (whole > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            for _ in 0..whole.unsigned_abs() {
                self.write(seq);
            }
        } else {
            self.tab_mut().scroll(Scroll::Delta(whole));
            self.scrolled();
            self.window.request_redraw();
        }
    }

    /// New output: draw it now, or once enough time has passed since the
    /// last frame (see FRAME_COST_FACTOR). Typing after a pause draws at
    /// once; a stream of output is shown at a pace the machine can afford.
    fn redraw_for_output(&mut self) {
        let next = self.last_frame + MIN_FRAME_GAP.max(self.frame_cost * FRAME_COST_FACTOR);
        if Instant::now() >= next {
            self.window.request_redraw();
        } else {
            self.wake_at = Some(self.wake_at.map_or(next, |t| t.min(next)));
        }
    }

    /// Handles an event from tab `id`'s terminal. Returns true when the
    /// last tab closed and the app should quit.
    fn on_term_event(&mut self, id: u64, event: TermEvent) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return false };
        let active = i == self.active;
        match event {
            TermEvent::Wakeup => {
                self.frames.wakeup();
                self.tabs[i].search.content_changed();
                self.record_line_times(i);
                self.write_log(i);
                if active {
                    self.redraw_for_output();
                } else {
                    self.mark_activity(i);
                }
            }
            TermEvent::Title(t) => {
                self.tabs[i].shell_title = Some(t);
                if active {
                    self.update_window_title();
                }
            }
            TermEvent::ResetTitle => {
                self.tabs[i].shell_title = None;
                if active {
                    self.update_window_title();
                }
            }
            TermEvent::ClipboardStore(ClipboardType::Clipboard | ClipboardType::Selection, text) => {
                if let Some(cb) = self.clipboard.as_mut() {
                    let _ = cb.set_text(text);
                }
            }
            TermEvent::ClipboardLoad(_, format) => {
                if let Some(text) = self.clipboard_text() {
                    self.tabs[i].session.write(format(&text).into_bytes());
                }
            }
            TermEvent::ColorRequest(index, format) => {
                let c = view::color_for_index(&self.tabs[i].session.term.lock(), index, &self.palette);
                let reply = format(alacritty_terminal::vte::ansi::Rgb { r: c.r, g: c.g, b: c.b });
                self.tabs[i].session.write(reply.into_bytes());
            }
            TermEvent::PtyWrite(s) => {
                self.frames.replies += 1;
                self.tabs[i].session.write(s.into_bytes());
            }
            TermEvent::TextAreaSizeRequest(format) => self.tabs[i].session.write(format(self.tabs[i].grid).into_bytes()),
            TermEvent::Bell => {
                if !self.focused {
                    self.window.request_user_attention(Some(UserAttentionType::Informational));
                }
            }
            TermEvent::Exit | TermEvent::ChildExit(_) => return self.close_tab(i),
            TermEvent::CursorBlinkingChange | TermEvent::MouseCursorDirty => {}
        }
        false
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.create(el) {
            Ok(mut st) => {
                crate::WINDOW_UP.store(true, std::sync::atomic::Ordering::Relaxed);
                // Draw the first frame before showing the window, so it
                // never appears blank.
                st.redraw(&self.cfg);
                st.window.set_visible(true);
                let total = crate::STARTED.get().map(|t| t.elapsed().as_millis()).unwrap_or(0);
                st.startup = format!("{total} ms ({})", st.renderer.startup);
                log::info!("first frame after {}", st.startup);
                self.state = Some(st);
            }
            Err(e) => {
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        let Some((cfg, st)) = self.live(el) else {
            // The command line still gets its answer (see `remote::handle`).
            if let UserEvent::Remote(_, reply) = event {
                let _ = reply.send(Err("the window is closing".to_owned()));
            }
            return;
        };
        match event {
            UserEvent::Term(id, event) => {
                if st.on_term_event(id, event) {
                    el.exit();
                }
            }
            UserEvent::Notice(message) => st.show(message),
            UserEvent::Ai(event) => {
                st.on_ai(event);
                if std::mem::take(&mut st.ai_reset_model) {
                    self.change_config(|c| c.ai.model.clear());
                    if let Some(st) = self.state.as_mut() {
                        st.load_ai_models(&self.cfg);
                    }
                }
            }
            UserEvent::History(id) => {
                if let Some(i) = st.tabs.iter().position(|t| t.id == id) {
                    st.tabs[i].search_results();
                    if i == st.active {
                        st.scrolled();
                        st.window.request_redraw();
                    }
                }
            }
            UserEvent::Remote(command, reply) => {
                let (r, exit) = st.on_remote(cfg, command);
                let _ = reply.send(r);
                if exit {
                    el.exit();
                }
            }
            UserEvent::OsTheme(dark) => {
                if dark != st.os_dark {
                    log::info!("OS theme changed: {}", if dark { "dark" } else { "light" });
                    st.os_dark = dark;
                    st.apply_palette(cfg);
                    st.window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some((cfg, st)) = self.live(el) else { return };
        let action = match event {
            WindowEvent::CloseRequested => {
                el.exit();
                None
            }
            WindowEvent::RedrawRequested => {
                st.redraw(cfg);
                None
            }
            WindowEvent::Resized(size) => {
                st.renderer.resize(size.width, size.height);
                st.resize_grid(cfg);
                st.window.request_redraw();
                None
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                st.reload_font(cfg);
                st.window.request_redraw();
                None
            }
            WindowEvent::ThemeChanged(t) => {
                st.os_dark = t == Theme::Dark;
                st.apply_palette(cfg);
                st.window.request_redraw();
                None
            }
            WindowEvent::Focused(f) => {
                st.focused = f;
                let mut term = st.tab().session.term.lock();
                term.is_focused = f;
                let report = term.mode().contains(TermMode::FOCUS_IN_OUT);
                drop(term);
                if report {
                    st.write(if f { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] });
                }
                st.window.request_redraw();
                None
            }
            WindowEvent::ModifiersChanged(m) => {
                st.mods = m.state();
                None
            }
            WindowEvent::KeyboardInput { event, .. } => st.on_key(cfg, event),
            WindowEvent::Ime(Ime::Commit(text)) => {
                let tab = &mut st.tabs[st.active];
                if tab.chat.open && tab.chat.focused && matches!(st.overlay, Overlay::None) {
                    tab.chat.input.insert(&text);
                    st.window.request_redraw();
                } else if tab.search.focused {
                    tab.search_with(|search, term| search.type_text(&text, term));
                    st.window.request_redraw();
                } else if matches!(st.overlay, Overlay::None) {
                    st.write(text.into_bytes());
                }
                None
            }
            WindowEvent::CursorMoved { position, .. } => {
                st.on_mouse_move(cfg, position.x as f32, position.y as f32);
                None
            }
            WindowEvent::CursorLeft { .. } => {
                st.scrollbar.hover = false;
                st.search_hover = false;
                st.hover = Hover::None;
                st.window.request_redraw();
                None
            }
            WindowEvent::MouseInput { state, button, .. } => st.on_mouse_button(cfg, state, button),
            WindowEvent::MouseWheel { delta, .. } => {
                st.on_wheel(cfg, delta);
                None
            }
            _ => None,
        };
        if let Some(action) = action {
            self.perform(el, action);
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Finish every log with the line its cursor is on.
        if let Some(st) = self.state.as_mut() {
            for i in 0..st.tabs.len() {
                st.finish_log(i);
            }
            // Logs finishing from history are written to the end.
            for worker in st.finishing_logs.drain(..) {
                let _ = worker.join();
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if el.exiting() {
            return;
        }
        let now = Instant::now();
        if now >= self.next_poll {
            self.next_poll = now + CONFIG_POLL;
            if let Some(cfg) = self.watcher.poll() {
                self.apply_config(cfg);
            }
        }
        let mut deadline = self.next_poll;
        if let Some(st) = self.state.as_mut()
            && let Some(t) = st.wake_at
        {
            if now >= t {
                st.wake_at = None;
                st.window.request_redraw();
            } else {
                deadline = deadline.min(t);
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(deadline));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_right_after_a_resize_is_not_activity() {
        let t = Instant::now();
        assert!(!repainting_after_resize(None, t));
        assert!(repainting_after_resize(Some(t), t + RESIZE_REPAINT / 2));
        assert!(!repainting_after_resize(Some(t), t + RESIZE_REPAINT));
    }

    #[test]
    fn a_glide_eases_out() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "more than half the way at half the time");
        let mut last = 0.0;
        for i in 1..=10 {
            let now = ease_out(i as f32 / 10.0);
            assert!(now > last);
            last = now;
        }
    }

    #[test]
    fn the_jump_highlight_holds_then_fades_out() {
        assert_eq!(flash_alpha(Duration::ZERO), 1.0);
        assert_eq!(flash_alpha(FLASH_HOLD), 1.0);
        let mid = flash_alpha(FLASH_HOLD + (FLASH_FOR - FLASH_HOLD) / 2);
        assert!((mid - 0.5).abs() < 0.01, "{mid}");
        assert_eq!(flash_alpha(FLASH_FOR), 0.0);
        assert_eq!(flash_alpha(FLASH_FOR * 2), 0.0);
    }

    #[test]
    fn dragging_the_divider_gives_a_rounded_share() {
        assert_eq!(chat_share(900.0, 400.0), 0.56);
        assert_eq!(chat_share(900.0, 10.0), 0.8);
        assert_eq!(chat_share(900.0, 890.0), 0.2);
    }

    #[test]
    fn grid_size_leaves_room_for_the_panel() {
        assert_eq!(columns(1000.0, 10.0, 0.0, 10.0, 10.0, 0.0), 97);
        assert_eq!(columns(1000.0, 10.0, 0.0, 10.0, 10.0, 400.0), 57);
        assert_eq!(columns(300.0, 10.0, 0.0, 10.0, 10.0, 290.0), 2); // never below 2
    }

    #[test]
    fn the_gutter_only_shifts_columns() {
        let grid = WindowSize { num_cols: 80, num_lines: 24, cell_width: 9, cell_height: 18 };
        let (top, pad, gutter, cell) = (32.0, 6.0, 54.0, (9.0, 18.0));
        // Middle of the left half of column 12 in row 3, where draw_grid puts it.
        let pos = (pad + gutter + 12.0 * 9.0 + 2.0, top + pad + 3.0 * 18.0 + 9.0);
        assert_eq!(cell_at(pos, top, pad, gutter, cell, grid), (12, 3, Side::Left));
        // Right half of the same cell.
        assert_eq!(cell_at((pos.0 + 5.0, pos.1), top, pad, gutter, cell, grid), (12, 3, Side::Right));
        // The first row starts right under the padding, gutter or not.
        assert_eq!(cell_at((pos.0, top + pad + 1.0), top, pad, gutter, cell, grid), (12, 0, Side::Left));
        assert_eq!(cell_at((pos.0 - gutter, top + pad + 1.0), top, pad, 0.0, cell, grid), (12, 0, Side::Left));
        // Off the edges: clamped into the grid.
        assert_eq!(cell_at((0.0, 0.0), top, pad, gutter, cell, grid), (0, 0, Side::Left));
        assert_eq!(cell_at((5000.0, 5000.0), top, pad, gutter, cell, grid).0, 79);
        assert_eq!(cell_at((5000.0, 5000.0), top, pad, gutter, cell, grid).1, 23);
    }

    #[test]
    fn font_points_to_pixels() {
        assert_eq!(font_px(12.0, 1.0), 16.0);
        assert_eq!(font_px(12.0, 2.0), 32.0);
        assert_eq!(font_px(9.0, 1.5), 18.0);
    }

    #[test]
    fn tab_labels() {
        let mut buf = String::from("old");
        write_label(&mut buf, 0, "vim");
        assert_eq!(buf, "Tab 1: vim");
        write_label(&mut buf, 11, "cargo");
        assert_eq!(buf, "Tab 12: cargo");
    }

    #[test]
    fn shell_name_from_settings() {
        let cfg = |shell: &str| Config { shell: shell.into(), ..Config::default() };
        assert_eq!(shell_name(&cfg("/usr/bin/zsh")), "zsh");
        assert_eq!(shell_name(&cfg("C:\\Windows\\System32\\cmd.exe")), "cmd");
        assert_eq!(shell_name(&cfg("pwsh.exe")), "pwsh");
    }
}
