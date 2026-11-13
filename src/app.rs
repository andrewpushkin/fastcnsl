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
    /// When the last frame finished, and how long it took to build and
    /// present: output redraws are paced by these.
    last_frame: Instant,
    frame_cost: Duration,
    /// A notice at the bottom of the window, and when it goes.
    toast: Option<(String, Instant)>,
    /// The grid size shown in the bottom right corner until then.
    size_until: Option<Instant>,
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
            (INITIAL_COLS as f32 * cell.width + 2.0 * pad + gutter_px(cfg, cell.width) + sb).ceil() as u32,
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

        let grid = grid_size(&renderer, cfg, scale);
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
            keymap: Keymap::new(&cfg.keys),
            capturing: None,
            pending_keys: None,
            last_frame: Instant::now(),
            frame_cost: Duration::ZERO,
            toast: None,
            size_until: None,
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
        if old.padding != cfg.padding || old.line_numbers != cfg.line_numbers {
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
            Action::Find => st.tab_mut().search_with(|search, term| search.open_in(term)),
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
            Action::OptionsPage(page) => {
                st.options_page = page;
                st.capturing = None;
                if page == OptionsPage::Font {
                    st.open_font_page(&self.cfg);
                }
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

/// Width of the line-number gutter in whole pixels (0 without numbers).
fn gutter_px(cfg: &Config, cell_width: f32) -> f32 {
    if cfg.line_numbers.is_on() { (lines::gutter_cells(cfg.line_numbers) as f32 * cell_width).round() } else { 0.0 }
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

fn grid_size(r: &Renderer, cfg: &Config, scale: f64) -> WindowSize {
    let (w, h) = r.size();
    let cell = r.cell();
    let pad = padding_px(cfg, scale as f32);
    let sb = ui::scrollbar_width(scale as f32);
    let bar = chrome::tab_bar_height(&Metrics { width: w as f32, height: h as f32, scale: scale as f32, cw: cell.width, ch: cell.height });
    let cols = ((w as f32 - 2.0 * pad - gutter_px(cfg, cell.width) - sb) / cell.width).floor().max(2.0);
    let lines = ((h as f32 - 2.0 * pad - bar) / cell.height).floor().max(1.0);
    WindowSize {
        num_cols: cols as u16,
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
        let session = Session::spawn(cfg, self.grid, listener.clone())
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
        log::info!("{text}");
        let until = Instant::now() + TOAST_FOR;
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

    fn resize_grid(&mut self, cfg: &Config) {
        let size = grid_size(&self.renderer, cfg, self.window.scale_factor());
        let g = self.grid;
        if (size.num_cols, size.num_lines, size.cell_width, size.cell_height) == (g.num_cols, g.num_lines, g.cell_width, g.cell_height) {
            return;
        }
        for tab in &mut self.tabs {
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
        }
        let counts_changed = (size.num_cols, size.num_lines) != (g.num_cols, g.num_lines);
        self.grid = size;
        self.resized_at = Some(Instant::now());
        if counts_changed {
            self.show_size();
        }
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
        // Lock through a cloned handle so the guard doesn't borrow `self.tabs`
        // (the tab bar needs every tab's title while the active tab updates).
        let shared = self.tabs[self.active].session.term.clone();
        let t0 = Instant::now();
        {
            let mut live = shared.lock();
            let tab = &mut self.tabs[self.active];
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
                focused: self.focused && matches!(self.overlay, Overlay::None),
                bold_is_bright: cfg.bold_is_bright,
                scale,
                padding: padding_px(cfg, scale),
                top,
                scrollbar_alpha: self.scrollbar.fade.value,
                search_alpha: self.search_fade.value,
                min_contrast: cfg.minimum_contrast,
                gutter: gutter_px(cfg, self.renderer.cell().width),
                line_numbers: &tab.numbers,
                line_number_mode: cfg.line_numbers,
                bookmarks: &tab.bookmarked,
                scroll: Some(scroll),
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

    /// Viewport cell under the pointer: (grid point, side, col, row).
    fn mouse_cell(&self, cfg: &Config, display_offset: usize) -> (Point, Side, usize, usize) {
        let cell = self.renderer.cell();
        let (pad, gutter) = (padding_px(cfg, self.scale()), gutter_px(cfg, cell.width));
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
        let Some(line) = tab.lines.step_bookmark(tab.scroller.shown(&live).grid(), down) else {
            drop(live);
            self.show("No bookmarks in this tab".to_owned());
            return;
        };
        let budget = tab.session.budget();
        tab.scroller.show_line(&mut live, &tab.session.history, &budget, line);
        drop(live);
        self.scrolled();
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
            Overlay::Options | Overlay::About => {
                if matches!(event.logical_key, Key::Named(NamedKey::Escape | NamedKey::Enter)) {
                    self.overlay = Overlay::None;
                    self.window.request_redraw();
                }
                return None;
            }
            Overlay::None => {}
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

        let term = tab.session.term.lock();
        let mode = *term.mode();
        let bytes = input::encode_key(&event.logical_key, event.text.as_deref(), m, mode)?;
        let mut ran = None;
        if matches!(event.logical_key, Key::Named(NamedKey::Enter)) && !mode.contains(TermMode::ALT_SCREEN) {
            let line = highlight::command_line(term.grid());
            tab.code.on_enter(term.grid(), &line);
            tab.lines.on_enter(term.grid());
            ran = detect::program_name(&line).filter(|name| *name != tab.app);
            drop(term);
            // The command line goes into the log as entered.
            if let Err(e) = tab.log_pass(Upto::Cursor) {
                log::warn!("logging: {e}");
            }
        } else {
            drop(term);
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

        let top = self.top();
        let scale = self.scale();
        let tab = &mut self.tabs[self.active];
        let (hist, scrolled) = tab.scroll_metrics();
        let (screen, offset) = tab.shown(|t| (t.grid().screen_lines(), t.grid().display_offset()));
        let geo = ui::scroll_geometry(m.width, top, m.height, scale, hist, screen, scrolled);

        let over_bar = geo.as_ref().is_some_and(|g| g.track.contains(x, y));
        let sg = ui::search_geometry(m.width, top, scale, m.cw, m.ch, tab.search.boxes.len(), tab.search.picker);
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

        let top = self.top();
        let scale = self.scale();
        if pressed && button == WinitButton::Left {
            let tab = &mut self.tabs[self.active];
            if tab.search.open {
                let s = &mut tab.search;
                let g = ui::search_geometry(m.width, top, scale, m.cw, m.ch, s.boxes.len(), s.picker);
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
            if let Some(g) = ui::scroll_geometry(m.width, top, m.height, scale, hist, screen, scrolled)
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
            self.font_scroll = first;
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
            TermEvent::TextAreaSizeRequest(format) => self.tabs[i].session.write(format(self.grid).into_bytes()),
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
                if tab.search.focused {
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
