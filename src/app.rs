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
use alacritty_terminal::term::{ClipboardType, TermMode};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton as WinitButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Theme, UserAttentionType, Window, WindowId};

use crate::chrome::{self, Action, Caption, Menu, Metrics, OptionsPage, OptionsState, Panel};
use crate::config::{Config, ConfigWatcher, ThemeMode};
use crate::detect;
use crate::highlight::{self, CodeHighlighter};
use crate::input::{self, MouseButton};
use crate::remote;
use crate::render::Renderer;
use crate::search::Search;
use crate::session::{EventProxy, Session, UserEvent};
use crate::theme::{self, Palette};
use crate::ui::{self, Fade, Scrollbar};
use crate::view::{self, ChromeView, Hover, View};

const TITLE: &str = "ragamuffin";
const INITIAL_COLS: u32 = 100;
const INITIAL_LINES: u32 = 30;
const CONFIG_POLL: Duration = Duration::from_secs(1);
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// Font size in points → pixels at the given DPI scale.
fn font_px(points: f32, scale: f64) -> f32 {
    points * scale as f32 * 96.0 / 72.0
}

pub struct App {
    cfg: Config,
    watcher: ConfigWatcher,
    proxy: EventLoopProxy<UserEvent>,
    state: Option<State>,
    next_poll: Instant,
    pub error: Option<String>,
    /// Name and command of the first tab (from `ragamuffin new-tab`).
    first_tab: (Option<String>, Vec<String>),
    /// Listens for `ragamuffin new-tab/close-tab/quit`.
    _remote: Option<remote::Server>,
}

/// One shell with its own scrollback, search and code highlighting.
struct Tab {
    id: u64,
    session: Session,
    search: Search,
    code: CodeHighlighter,
    /// Last program started in this tab (the shell's name until then).
    app: String,
    /// Name given with `ragamuffin new-tab NAME`; shown instead of `app`.
    name: Option<String>,
    /// "Tab N: app", rebuilt when `app` or the tab's position changes.
    label: String,
    /// Title the shell set with an escape sequence, used for the window.
    shell_title: Option<String>,
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
    /// For double-click on the empty tab bar (maximize).
    last_bar_click: Option<Instant>,
    /// Cursor last sent to the window system.
    cursor: CursorIcon,
    /// Time from launch to the first frame, for the About panel.
    startup: String,
    /// The page the Options panel shows.
    options_page: OptionsPage,
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
    pub fn new(cfg: Config, proxy: EventLoopProxy<UserEvent>, first_tab: (Option<String>, Vec<String>)) -> Self {
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
        let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, "ragamuffin", "ragamuffin");
        #[cfg(all(unix, not(target_os = "macos")))]
        let attrs = winit::platform::x11::WindowAttributesExtX11::with_name(attrs, "ragamuffin", "ragamuffin");
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
        let renderer = pollster::block_on(Renderer::new(
            window.clone(),
            el.owned_display_handle(),
            &cfg.font_family,
            font_px(cfg.font_size, scale),
            cfg.line_height,
            cfg.antialiasing.subpixel(crate::render::system_subpixel()),
        ))?;

        // Size the window to a comfortable grid now that the cell is known.
        let cell = renderer.cell();
        let pad = padding_px(cfg, scale as f32);
        let sb = ui::scrollbar_width(scale as f32);
        let bar = chrome::tab_bar_height(&Metrics { width: 0.0, height: 0.0, scale: scale as f32, cw: cell.width, ch: cell.height });
        let want = PhysicalSize::new(
            (INITIAL_COLS as f32 * cell.width + 2.0 * pad + sb).ceil() as u32,
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
            last_bar_click: None,
            cursor: CursorIcon::Text,
            startup: String::new(),
            options_page: OptionsPage::General,
        };
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
        if old.antialiasing != cfg.antialiasing {
            st.renderer.set_subpixel(cfg.antialiasing.subpixel(crate::render::system_subpixel()));
        }
        if old.padding != cfg.padding {
            st.resize_grid(cfg);
        }
        if old.theme != cfg.theme {
            st.theme_mode = cfg.theme;
        }
        for tab in &mut st.tabs {
            tab.code.enabled = cfg.code_highlight;
            if old.scrollback != cfg.scrollback {
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

    fn perform(&mut self, el: &ActiveEventLoop, action: Action) {
        let Some(st) = self.state.as_mut() else { return };
        if !matches!(
            action,
            Action::FontBigger | Action::FontSmaller | Action::CodeHighlight(_) | Action::Theme(_) | Action::OptionsPage(_)
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
                let tab = st.tab_mut();
                tab.search.open_in(&mut tab.session.term.lock());
            }
            Action::Copy => st.copy_selection(),
            Action::Paste => st.paste(),
            Action::SelectAll => {
                let mut term = st.tab().session.term.lock();
                let mut sel = Selection::new(SelectionType::Simple, Point::new(term.topmost_line(), Column(0)), Side::Left);
                sel.include_all();
                term.selection = Some(sel);
            }
            Action::Theme(mode) => self.change_config(|c| c.theme = mode),
            Action::FontBigger => {
                let size = (self.cfg.font_size + 1.0).min(96.0);
                self.change_config(|c| c.font_size = size);
            }
            Action::FontSmaller => {
                let size = (self.cfg.font_size - 1.0).max(4.0);
                self.change_config(|c| c.font_size = size);
            }
            Action::CodeHighlight(on) => self.change_config(|c| c.code_highlight = on),
            Action::Options => {
                st.overlay = Overlay::Options;
                st.options_page = OptionsPage::General;
            }
            Action::About => st.overlay = Overlay::About,
            Action::OpenSettingsFile => open_settings(),
            Action::OpenProjectPage => open_in_os(crate::REPOSITORY),
            Action::OpenReleases => open_in_os(format!("{}/releases", crate::REPOSITORY)),
            Action::ClosePanel => {}
            Action::Quit => el.exit(),
            Action::OptionsPage(page) => st.options_page = page,
        }
    }
}

/// The padding around the grid in whole pixels: at fractional scales
/// (125%: 6 -> 7.5) the grid would otherwise sit between pixels, and so
/// would every cell background and glyph.
fn padding_px(cfg: &Config, scale: f32) -> f32 {
    (cfg.padding * scale).round()
}

fn grid_size(r: &Renderer, cfg: &Config, scale: f64) -> WindowSize {
    let (w, h) = r.size();
    let cell = r.cell();
    let pad = padding_px(cfg, scale as f32);
    let sb = ui::scrollbar_width(scale as f32);
    let bar = chrome::tab_bar_height(&Metrics { width: w as f32, height: h as f32, scale: scale as f32, cw: cell.width, ch: cell.height });
    let cols = ((w as f32 - 2.0 * pad - sb) / cell.width).floor().max(2.0);
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
        let session = Session::spawn(cfg, self.grid, EventProxy::new(self.proxy.clone(), id))
            .map_err(|e| format!("starting {}: {e}", if cfg.shell.is_empty() { "shell" } else { &cfg.shell }))?;
        let code_theme = if self.palette.dark { &cfg.code_theme_dark } else { &cfg.code_theme_light };
        self.tabs.push(Tab {
            id,
            session,
            search: Search::new(),
            code: CodeHighlighter::new(cfg.code_highlight, code_theme),
            app: shell_name(cfg),
            name,
            label: String::new(),
            shell_title: None,
        });
        self.relabel();
        self.select_tab(self.tabs.len() - 1);
        Ok(())
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

    /// Carries out a command from `ragamuffin new-tab/close-tab/quit`.
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
        self.selecting = false;
        self.reported_button = None;
        self.update_window_title();
        self.window.request_redraw();
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
        }
        self.grid = size;
    }

    fn reload_font(&mut self, cfg: &Config) {
        let px = font_px(self.font_pt, self.window.scale_factor());
        self.renderer.glyphs.set_font(&cfg.font_family, px, cfg.line_height);
        self.resize_grid(cfg);
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
        let m = self.metrics();
        match self.overlay {
            Overlay::Options => Some(chrome::options_panel(
                &m,
                &OptionsState {
                    page: self.options_page,
                    theme: self.theme_mode,
                    font_size: cfg.font_size,
                    font_family: cfg.font_family.clone(),
                    code_highlight: cfg.code_highlight,
                },
            )),
            Overlay::About => {
                let path = Config::path().map(|p| p.display().to_string()).unwrap_or_default();
                Some(chrome::about_panel(&m, &self.renderer.gpu, &self.startup, &path))
            }
            _ => None,
        }
    }

    fn redraw(&mut self, cfg: &Config) {
        let idle = cfg.ui_idle_opacity;
        let mut animating = self.scrollbar.fade.update(if self.scrollbar.active() { 1.0 } else { idle });
        let search_target = if self.tab().search.focused || self.search_hover { 1.0 } else { idle };
        animating |= self.search_fade.update(search_target);

        let panel = self.panel(cfg);
        let top = self.top();
        let scale = self.scale();
        // Lock through a cloned handle so the guard doesn't borrow `self.tabs`
        // (the tab bar needs every tab's title while the active tab updates).
        let shared = self.tabs[self.active].session.term.clone();
        {
            let term = shared.lock();
            let tab = &mut self.tabs[self.active];
            if !term.mode().contains(TermMode::ALT_SCREEN) {
                tab.code.update(term.grid());
            }
            if tab.search.open {
                tab.search.refresh(&term);
            }
            let tab = &self.tabs[self.active];
            let v = View {
                palette: &self.palette,
                search: &tab.search,
                code: &tab.code,
                focused: self.focused && matches!(self.overlay, Overlay::None),
                bold_is_bright: cfg.bold_is_bright,
                scale,
                padding: padding_px(cfg, scale),
                top,
                scrollbar_alpha: self.scrollbar.fade.value,
                search_alpha: self.search_fade.value,
                min_contrast: cfg.minimum_contrast,
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
            };
            self.renderer.build(|p| view::draw(p, &term, &v, &c));
        }
        self.renderer.present(self.palette.background);

        let tab = &self.tabs[self.active];
        if animating || self.renderer.needs_rebuild() || tab.search.recount_pending() {
            self.window.request_redraw();
        }
    }

    /// Viewport cell under the pointer: (grid point, side, col, row).
    fn mouse_cell(&self, cfg: &Config, display_offset: usize) -> (Point, Side, usize, usize) {
        let cell = self.renderer.cell();
        let pad = padding_px(cfg, self.scale());
        let x = (self.mouse.0 - pad).max(0.0);
        let y = (self.mouse.1 - self.top() - pad).max(0.0);
        let col = ((x / cell.width) as usize).min(self.grid.num_cols as usize - 1);
        let row = ((y / cell.height) as usize).min(self.grid.num_lines as usize - 1);
        let side = if x - col as f32 * cell.width > cell.width / 2.0 { Side::Right } else { Side::Left };
        (Point::new(Line(row as i32 - display_offset as i32), Column(col)), side, col, row)
    }

    fn has_selection(&self) -> bool {
        self.tab().session.term.lock().selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    fn copy_selection(&mut self) {
        let text = self.tab().session.term.lock().selection_to_string();
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
            let q = format!("{}{}", tab.search.query, text.lines().next().unwrap_or(""));
            let mut term = tab.session.term.lock();
            tab.search.set_query(q, &mut term);
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
        let mac = cfg!(target_os = "macos");
        let primary = if mac { m.super_key() } else { m.control_key() && m.shift_key() };
        let ctrl = if mac { m.super_key() } else { m.control_key() };

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
            Overlay::Options | Overlay::About => {
                if matches!(event.logical_key, Key::Named(NamedKey::Escape | NamedKey::Enter)) {
                    self.overlay = Overlay::None;
                    self.window.request_redraw();
                }
                return None;
            }
            Overlay::None => {}
        }

        // Tab switching.
        let tab_switch = match &event.logical_key {
            Key::Named(NamedKey::Tab) if m.control_key() => Some(!m.shift_key()),
            Key::Named(NamedKey::PageDown) if ctrl && !m.shift_key() => Some(true),
            Key::Named(NamedKey::PageUp) if ctrl && !m.shift_key() => Some(false),
            Key::Character(c) if mac && m.super_key() && m.shift_key() && (c == "]" || c == "}") => Some(true),
            Key::Character(c) if mac && m.super_key() && m.shift_key() && (c == "[" || c == "{") => Some(false),
            _ => None,
        };
        if let Some(forward) = tab_switch {
            self.cycle_tab(forward);
            return None;
        }

        if let Key::Character(c) = &event.logical_key {
            let c = c.to_lowercase();
            if primary {
                let action = match c.as_str() {
                    "t" => Some(Action::NewTab),
                    "w" => Some(Action::CloseTab),
                    "f" => Some(Action::Find),
                    "c" => Some(Action::Copy),
                    "v" => Some(Action::Paste),
                    "," | "<" => Some(Action::Options),
                    "l" => Some(Action::Theme(self.theme_mode.next())),
                    _ => None,
                };
                if action.is_some() {
                    return action;
                }
            }
            if ctrl && !self.tab().search.focused {
                let size = match c.as_str() {
                    "=" | "+" => Some(self.font_pt + 1.0),
                    "-" if !m.shift_key() => Some(self.font_pt - 1.0),
                    "0" => Some(cfg.font_size),
                    _ => None,
                };
                if let Some(size) = size {
                    // Temporary zoom, like a browser; Options changes the saved size.
                    self.font_pt = size.clamp(4.0, 96.0);
                    self.reload_font(cfg);
                    self.window.request_redraw();
                    return None;
                }
            }
        }

        let tab = &mut self.tabs[self.active];
        if tab.search.open && matches!(event.logical_key, Key::Named(NamedKey::F3)) {
            let dir = if m.shift_key() { Direction::Right } else { Direction::Left };
            tab.search.step(&mut tab.session.term.lock(), dir);
            self.window.request_redraw();
            return None;
        }

        if tab.search.focused {
            self.on_search_key(&event);
            self.window.request_redraw();
            return None;
        }

        if m.shift_key() && !m.control_key() {
            let scroll = match event.logical_key {
                Key::Named(NamedKey::PageUp) => Some(Scroll::PageUp),
                Key::Named(NamedKey::PageDown) => Some(Scroll::PageDown),
                Key::Named(NamedKey::Home) => Some(Scroll::Top),
                Key::Named(NamedKey::End) => Some(Scroll::Bottom),
                _ => None,
            };
            let mut term = tab.session.term.lock();
            if let (Some(s), false) = (scroll, term.mode().contains(TermMode::ALT_SCREEN)) {
                term.scroll_display(s);
                drop(term);
                self.scrolled();
                self.window.request_redraw();
                return None;
            }
        }

        let mut term = tab.session.term.lock();
        let mode = *term.mode();
        let bytes = input::encode_key(&event.logical_key, event.text.as_deref(), m, mode)?;
        let mut ran = None;
        if matches!(event.logical_key, Key::Named(NamedKey::Enter)) && !mode.contains(TermMode::ALT_SCREEN) {
            let line = highlight::command_line(term.grid());
            tab.code.on_enter(term.grid(), &line);
            ran = detect::program_name(&line).filter(|name| *name != tab.app);
        }
        term.scroll_display(Scroll::Bottom);
        term.selection = None;
        drop(term);
        self.write(bytes);
        if let Some(name) = ran {
            self.tabs[self.active].app = name;
            self.relabel();
            self.window.request_redraw();
        }
        None
    }

    fn on_search_key(&mut self, event: &KeyEvent) {
        let m = self.mods;
        let tab = &mut self.tabs[self.active];
        let mut term = tab.session.term.lock();
        let search = &mut tab.search;
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => search.close(),
            Key::Named(NamedKey::Enter) => {
                let dir = if m.shift_key() { Direction::Right } else { Direction::Left };
                search.step(&mut term, dir);
            }
            Key::Named(NamedKey::ArrowUp) => search.step(&mut term, Direction::Left),
            Key::Named(NamedKey::ArrowDown) => search.step(&mut term, Direction::Right),
            Key::Named(NamedKey::Backspace) => {
                let mut q = search.query.clone();
                if m.control_key() {
                    q.clear();
                } else {
                    q.pop();
                }
                search.set_query(q, &mut term);
            }
            Key::Character(c) if m.control_key() && c.eq_ignore_ascii_case("r") => search.toggle_regex(&mut term),
            Key::Character(c) if m.control_key() && c.eq_ignore_ascii_case("v") => {
                drop(term);
                self.paste()
            }
            _ => {
                if m.control_key() || m.super_key() {
                    return;
                }
                if let Some(text) = event.text.as_deref().filter(|t| t.chars().all(|c| !c.is_control())) {
                    let q = format!("{}{text}", search.query);
                    search.set_query(q, &mut term);
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
            self.set_cursor(CursorIcon::Default);
            return;
        }

        let top = self.top();
        let scale = self.scale();
        let tab = &mut self.tabs[self.active];
        let mut term = tab.session.term.lock();
        let grid = term.grid();
        let (hist, screen, offset) = (grid.history_size(), grid.screen_lines(), grid.display_offset());
        let geo = ui::scroll_geometry(m.width, top, m.height, scale, hist, screen, offset);

        let over_bar = geo.as_ref().is_some_and(|g| g.track.contains(x, y));
        let sg = ui::search_geometry(m.width, top, scale, m.cw, m.ch);
        let over_search = tab.search.open && sg.bar.contains(x, y);
        let search_button = if !over_search {
            Hover::None
        } else if sg.prev.contains(x, y) {
            Hover::SearchPrev
        } else if sg.next.contains(x, y) {
            Hover::SearchNext
        } else if sg.close.contains(x, y) {
            Hover::SearchClose
        } else {
            Hover::None
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
            let target = ui::offset_for_thumb(g, y - grab, hist) as i32;
            term.scroll_display(Scroll::Delta(target - offset as i32));
            drop(term);
            self.scrolled();
            self.window.request_redraw();
            return;
        }

        drop(term);
        let (point, side, col, row) = self.mouse_cell(cfg, offset);
        let mut term = self.tabs[self.active].session.term.lock();
        let mode = *term.mode();
        if let Some(button) = self.reported_button {
            if mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG) && self.last_reported_cell != Some((col, row)) {
                self.last_reported_cell = Some((col, row));
                if let Some(b) = input::encode_mouse(button, true, true, col, row, self.mods, mode) {
                    drop(term);
                    self.write(b);
                }
            }
            return;
        }
        if self.selecting {
            if let Some(sel) = term.selection.as_mut() {
                sel.update(point, side);
            }
            drop(term);
            self.window.request_redraw();
            return;
        }
        drop(term);
        if redraw {
            self.window.request_redraw();
        }
    }

    /// Handles a click; returns an action for the app to perform.
    fn on_mouse_button(&mut self, cfg: &Config, state: ElementState, button: WinitButton) -> Option<Action> {
        let m = self.metrics();
        let (x, y) = self.mouse;
        let pressed = state == ElementState::Pressed;

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
                // Right-click elsewhere reopens the context menu there.
                let sel = self.has_selection();
                self.overlay = Overlay::Menu(Menu::context(x, y, sel));
            }
            return picked;
        }
        // Panels are modal.
        if let Some(panel) = self.panel(cfg) {
            if pressed && button == WinitButton::Left {
                if let Some(a) = panel.hit(x, y) {
                    return Some(a);
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
                    self.overlay = Overlay::Menu(Menu::main(bar.menu.x + bar.menu.w, bar.height, sel, self.tabs.len()));
                    self.window.request_redraw();
                }
                (Hover::Plus, WinitButton::Left) => return Some(Action::NewTab),
                (Hover::TabClose(i), WinitButton::Left) | (Hover::Tab(i) | Hover::TabClose(i), WinitButton::Middle) => {
                    self.select_tab(i);
                    return Some(Action::CloseTab);
                }
                (Hover::Tab(i), WinitButton::Left) => self.select_tab(i),
                _ => {}
            }
            return None;
        }

        let top = self.top();
        let scale = self.scale();
        if pressed && button == WinitButton::Left {
            let tab = &mut self.tabs[self.active];
            if tab.search.open {
                let g = ui::search_geometry(m.width, top, scale, m.cw, m.ch);
                if g.close.contains(x, y) {
                    tab.search.close();
                    self.window.request_redraw();
                    return None;
                }
                for (rect, dir) in [(g.prev, Direction::Left), (g.next, Direction::Right)] {
                    if rect.contains(x, y) {
                        tab.search.focused = true;
                        tab.search.step(&mut tab.session.term.lock(), dir);
                        self.window.request_redraw();
                        return None;
                    }
                }
                if g.bar.contains(x, y) {
                    tab.search.focused = true;
                    self.window.request_redraw();
                    return None;
                }
            }
            let mut term = tab.session.term.lock();
            let grid = term.grid();
            let (hist, screen, offset) = (grid.history_size(), grid.screen_lines(), grid.display_offset());
            if let Some(g) = ui::scroll_geometry(m.width, top, m.height, scale, hist, screen, offset)
                && g.track.contains(x, y)
            {
                if g.thumb.contains(x, y) {
                    self.scrollbar.drag = Some(y - g.thumb.y);
                } else {
                    term.scroll_display(if y < g.thumb.y { Scroll::PageUp } else { Scroll::PageDown });
                }
                drop(term);
                self.scrolled();
                self.window.request_redraw();
                return None;
            }
        }
        if !pressed && button == WinitButton::Left && self.scrollbar.drag.take().is_some() {
            self.window.request_redraw();
            return None;
        }

        let offset = self.tab().session.term.lock().grid().display_offset();
        let (point, side, col, row) = self.mouse_cell(cfg, offset);
        let mut term = self.tabs[self.active].session.term.lock();
        let mode = *term.mode();

        if mode.intersects(TermMode::MOUSE_MODE) && !self.mods.shift_key() {
            let b = match button {
                WinitButton::Left => MouseButton::Left,
                WinitButton::Middle => MouseButton::Middle,
                WinitButton::Right => MouseButton::Right,
                _ => return None,
            };
            self.reported_button = pressed.then_some(b);
            self.last_reported_cell = Some((col, row));
            if let Some(bytes) = input::encode_mouse(b, pressed, false, col, row, self.mods, mode) {
                drop(term);
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
                drop(term);
                self.selecting = true;
                self.tab_mut().search.focused = false;
            }
            (WinitButton::Left, false) => {
                self.selecting = false;
                if term.selection.as_ref().is_some_and(Selection::is_empty) {
                    term.selection = None;
                }
            }
            (WinitButton::Right, true) => {
                let sel = term.selection.as_ref().is_some_and(|s| !s.is_empty());
                drop(term);
                self.overlay = Overlay::Menu(Menu::context(x, y, sel));
            }
            (WinitButton::Middle, true) => {
                drop(term);
                self.paste();
            }
            _ => {}
        }
        self.window.request_redraw();
        None
    }

    fn on_wheel(&mut self, cfg: &Config, delta: MouseScrollDelta) {
        if !matches!(self.overlay, Overlay::None) || self.mouse.1 < self.top() {
            return;
        }
        let cell_h = self.renderer.cell().height;
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 3.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32 / cell_h,
        };
        self.wheel_accum += lines;
        let whole = self.wheel_accum.trunc() as i32;
        if whole == 0 {
            return;
        }
        self.wheel_accum -= whole as f32;

        let offset = self.tab().session.term.lock().grid().display_offset();
        let (_, _, col, row) = self.mouse_cell(cfg, offset);
        let mut term = self.tabs[self.active].session.term.lock();
        let mode = *term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !self.mods.shift_key() {
            let b = if whole > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
            drop(term);
            for _ in 0..whole.unsigned_abs() {
                if let Some(bytes) = input::encode_mouse(b, true, false, col, row, self.mods, mode) {
                    self.write(bytes);
                }
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            drop(term);
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
            term.scroll_display(Scroll::Delta(whole));
            drop(term);
            self.scrolled();
            self.window.request_redraw();
        }
    }

    /// Handles an event from tab `id`'s terminal. Returns true when the
    /// last tab closed and the app should quit.
    fn on_term_event(&mut self, id: u64, event: TermEvent) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return false };
        let active = i == self.active;
        match event {
            TermEvent::Wakeup => {
                self.tabs[i].search.content_changed();
                if active {
                    self.window.request_redraw();
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
            TermEvent::PtyWrite(s) => self.tabs[i].session.write(s.into_bytes()),
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

fn open_settings() {
    if let Some(path) = Config::path() {
        open_in_os(path);
    }
}

/// Opens a file or URL with the OS's default application.
fn open_in_os(target: impl AsRef<std::ffi::OsStr>) {
    let target = target.as_ref();
    let result = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").arg("/C").arg("start").arg("").arg(target).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(target).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(target).spawn()
    };
    if let Err(e) = result {
        log::warn!("could not open {}: {e}", target.to_string_lossy());
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
        let cfg = &self.cfg;
        let Some(st) = self.state.as_mut() else { return };
        match event {
            UserEvent::Term(id, event) => {
                if st.on_term_event(id, event) {
                    el.exit();
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
        let cfg = &self.cfg;
        let Some(st) = self.state.as_mut() else { return };
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
                    let q = format!("{}{text}", tab.search.query);
                    tab.search.set_query(q, &mut tab.session.term.lock());
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

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
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
