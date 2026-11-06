//! Window and event handling.
//!
//! Shortcuts (Cmd instead of Ctrl+Shift on macOS):
//!
//! | Keys                    | Action                                   |
//! |-------------------------|------------------------------------------|
//! | Ctrl+Shift+F            | open / focus search                      |
//! | Enter / Shift+Enter, F3 | previous (older) / next match            |
//! | Ctrl+R (in search)      | toggle regex mode                        |
//! | Esc (in search)         | close search                             |
//! | Ctrl+Shift+C / V        | copy / paste                             |
//! | Ctrl+= / Ctrl+- / Ctrl+0| bigger / smaller / reset font            |
//! | Ctrl+Shift+L            | cycle theme: system → dark → light       |
//! | Ctrl+Shift+,            | open the settings file                   |
//! | Shift+PgUp/PgDn/Home/End| scroll                                   |

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

use crate::config::{Config, ConfigWatcher, ThemeMode};
use crate::highlight::CodeHighlighter;
use crate::input::{self, MouseButton};
use crate::render::Renderer;
use crate::search::Search;
use crate::session::{EventProxy, Session, UserEvent};
use crate::theme::{self, Palette};
use crate::ui::{self, Fade, Scrollbar};
use crate::view::{self, View};

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
}

struct State {
    window: Arc<Window>,
    renderer: Renderer,
    session: Session,
    palette: Palette,
    os_dark: bool,
    theme_mode: ThemeMode,
    search: Search,
    code: CodeHighlighter,
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
}

impl App {
    pub fn new(cfg: Config, proxy: EventLoopProxy<UserEvent>) -> Self {
        Self { cfg, watcher: ConfigWatcher::new(), proxy, state: None, next_poll: Instant::now() + CONFIG_POLL, error: None }
    }

    fn create(&mut self, el: &ActiveEventLoop) -> Result<State, String> {
        let attrs = Window::default_attributes()
            .with_title(TITLE)
            .with_inner_size(winit::dpi::LogicalSize::new(960.0, 600.0));
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
        ))?;

        // Size the window to a comfortable grid now that the cell is known.
        let cell = renderer.cell();
        let pad = cfg.padding * scale as f32;
        let sb = ui::scrollbar_width(scale as f32);
        let want = PhysicalSize::new(
            (INITIAL_COLS as f32 * cell.width + 2.0 * pad + sb).ceil() as u32,
            (INITIAL_LINES as f32 * cell.height + 2.0 * pad).ceil() as u32,
        );
        let _ = window.request_inner_size(want);

        let os_dark = match window.theme() {
            Some(t) => t == Theme::Dark,
            None => theme::os_prefers_dark_fallback().unwrap_or(true),
        };
        let palette = Palette::for_mode(cfg.theme, os_dark);
        let code_theme = if palette.dark { &cfg.code_theme_dark } else { &cfg.code_theme_light };

        let grid = grid_size(&renderer, cfg, scale);
        let session = Session::spawn(cfg, grid, EventProxy::new(self.proxy.clone())).map_err(|e| format!("starting shell: {e}"))?;

        Ok(State {
            window,
            renderer,
            session,
            palette,
            os_dark,
            theme_mode: cfg.theme,
            search: Search::new(),
            code: CodeHighlighter::new(cfg.code_highlight, code_theme),
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
        })
    }

    fn apply_config(&mut self, new: Config) {
        let old = std::mem::replace(&mut self.cfg, new);
        let cfg = &self.cfg;
        let Some(st) = self.state.as_mut() else { return };
        log::info!("settings reloaded");
        if old.font_family != cfg.font_family || old.font_size != cfg.font_size || old.line_height != cfg.line_height {
            st.font_pt = cfg.font_size;
            st.reload_font(cfg);
        }
        if old.padding != cfg.padding {
            st.resize_grid(cfg);
        }
        if old.theme != cfg.theme {
            st.theme_mode = cfg.theme;
        }
        st.code.enabled = cfg.code_highlight;
        st.apply_palette(cfg);
        if old.scrollback != cfg.scrollback {
            st.session.apply_config(cfg);
        }
        if old.shell != cfg.shell || old.shell_args != cfg.shell_args {
            log::warn!("shell changes apply to new windows");
        }
        st.window.request_redraw();
    }
}

fn grid_size(r: &Renderer, cfg: &Config, scale: f64) -> WindowSize {
    let (w, h) = r.size();
    let cell = r.cell();
    let pad = cfg.padding * scale as f32;
    let sb = ui::scrollbar_width(scale as f32);
    let cols = ((w as f32 - 2.0 * pad - sb) / cell.width).floor().max(2.0);
    let lines = ((h as f32 - 2.0 * pad) / cell.height).floor().max(1.0);
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

    fn resize_grid(&mut self, cfg: &Config) {
        let size = grid_size(&self.renderer, cfg, self.window.scale_factor());
        let g = self.grid;
        if (size.num_cols, size.num_lines, size.cell_width, size.cell_height) == (g.num_cols, g.num_lines, g.cell_width, g.cell_height) {
            return;
        }
        if size.num_cols != g.num_cols {
            // Text reflows; recorded row positions no longer hold.
            self.code.clear();
        }
        self.session.resize(size);
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
        self.code.set_theme(code_theme);
    }

    fn write(&self, bytes: impl Into<std::borrow::Cow<'static, [u8]>>) {
        self.session.write(bytes);
    }

    fn redraw(&mut self, cfg: &Config) {
        let idle = cfg.ui_idle_opacity;
        let mut animating = self.scrollbar.fade.update(if self.scrollbar.active() { 1.0 } else { idle });
        let search_target = if self.search.focused || self.search_hover { 1.0 } else { idle };
        animating |= self.search_fade.update(search_target);

        {
            let term = self.session.term.lock();
            if !term.mode().contains(TermMode::ALT_SCREEN) {
                self.code.update(term.grid());
            }
            if self.search.open {
                self.search.refresh(&term);
            }
            let v = View {
                palette: &self.palette,
                search: &self.search,
                code: &self.code,
                focused: self.focused,
                bold_is_bright: cfg.bold_is_bright,
                scale: self.window.scale_factor() as f32,
                padding: cfg.padding * self.window.scale_factor() as f32,
                scrollbar_alpha: self.scrollbar.fade.value,
                search_alpha: self.search_fade.value,
            };
            self.renderer.build(|p| view::draw(p, &term, &v));
        }
        self.renderer.present(self.palette.background);

        if animating || self.renderer.needs_rebuild() || self.search.recount_pending() {
            self.window.request_redraw();
        }
    }

    /// Viewport cell under the pointer: (grid point, side, col, row).
    fn mouse_cell(&self, cfg: &Config, display_offset: usize) -> (Point, Side, usize, usize) {
        let cell = self.renderer.cell();
        let pad = cfg.padding * self.scale();
        let x = (self.mouse.0 - pad).max(0.0);
        let y = (self.mouse.1 - pad).max(0.0);
        let col = ((x / cell.width) as usize).min(self.grid.num_cols as usize - 1);
        let row = ((y / cell.height) as usize).min(self.grid.num_lines as usize - 1);
        let side = if x - col as f32 * cell.width > cell.width / 2.0 { Side::Right } else { Side::Left };
        (Point::new(Line(row as i32 - display_offset as i32), Column(col)), side, col, row)
    }

    fn copy_selection(&mut self) {
        let text = self.session.term.lock().selection_to_string();
        if let (Some(text), Some(cb)) = (text, self.clipboard.as_mut()) {
            if let Err(e) = cb.set_text(text) {
                log::warn!("copy failed: {e}");
            }
        }
    }

    fn clipboard_text(&mut self) -> Option<String> {
        self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
    }

    fn paste(&mut self) {
        let Some(text) = self.clipboard_text() else { return };
        if self.search.focused {
            let q = format!("{}{}", self.search.query, text.lines().next().unwrap_or(""));
            let mut term = self.session.term.lock();
            self.search.set_query(q, &mut term);
            return;
        }
        let mode = *self.session.term.lock().mode();
        self.write(input::paste_bytes(&text, mode));
    }

    fn scrolled(&mut self) {
        self.scrollbar.last_scroll = Some(Instant::now());
        self.wake_at = Some(Instant::now() + ui::SCROLL_ACTIVE_FOR + Duration::from_millis(20));
    }

    fn on_key(&mut self, cfg: &Config, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let m = self.mods;
        let mac = cfg!(target_os = "macos");
        let primary = if mac { m.super_key() } else { m.control_key() && m.shift_key() };
        let zoom = if mac { m.super_key() } else { m.control_key() };

        if let Key::Character(c) = &event.logical_key {
            let c = c.to_lowercase();
            if primary {
                match c.as_str() {
                    "f" => {
                        self.search.open();
                        return self.window.request_redraw();
                    }
                    "c" => return self.copy_selection(),
                    "v" => {
                        self.paste();
                        return self.window.request_redraw();
                    }
                    "," | "<" => return open_settings(),
                    "l" => {
                        self.theme_mode = self.theme_mode.next();
                        log::info!("theme: {:?}", self.theme_mode);
                        self.apply_palette(cfg);
                        return self.window.request_redraw();
                    }
                    _ => {}
                }
            }
            if zoom && !self.search.focused {
                let size = match c.as_str() {
                    "=" | "+" => Some(self.font_pt + 1.0),
                    "-" if !m.shift_key() => Some(self.font_pt - 1.0),
                    "0" => Some(cfg.font_size),
                    _ => None,
                };
                if let Some(size) = size {
                    self.font_pt = size.clamp(4.0, 96.0);
                    self.reload_font(cfg);
                    return self.window.request_redraw();
                }
            }
        }

        if self.search.open && matches!(event.logical_key, Key::Named(NamedKey::F3)) {
            let dir = if m.shift_key() { Direction::Right } else { Direction::Left };
            self.search.step(&mut self.session.term.lock(), dir);
            return self.window.request_redraw();
        }

        if self.search.focused {
            self.on_search_key(&event);
            return self.window.request_redraw();
        }

        if m.shift_key() && !m.control_key() {
            let scroll = match event.logical_key {
                Key::Named(NamedKey::PageUp) => Some(Scroll::PageUp),
                Key::Named(NamedKey::PageDown) => Some(Scroll::PageDown),
                Key::Named(NamedKey::Home) => Some(Scroll::Top),
                Key::Named(NamedKey::End) => Some(Scroll::Bottom),
                _ => None,
            };
            let alt = self.session.term.lock().mode().contains(TermMode::ALT_SCREEN);
            if let (Some(s), false) = (scroll, alt) {
                self.session.term.lock().scroll_display(s);
                self.scrolled();
                return self.window.request_redraw();
            }
        }

        let mut term = self.session.term.lock();
        let mode = *term.mode();
        let Some(bytes) = input::encode_key(&event.logical_key, event.text.as_deref(), m, mode) else { return };
        if matches!(event.logical_key, Key::Named(NamedKey::Enter)) && !mode.contains(TermMode::ALT_SCREEN) {
            self.code.on_enter(term.grid());
        }
        term.scroll_display(Scroll::Bottom);
        term.selection = None;
        drop(term);
        self.write(bytes);
    }

    fn on_search_key(&mut self, event: &KeyEvent) {
        let m = self.mods;
        let mut term = self.session.term.lock();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.search.close(),
            Key::Named(NamedKey::Enter) => {
                let dir = if m.shift_key() { Direction::Right } else { Direction::Left };
                self.search.step(&mut term, dir);
            }
            Key::Named(NamedKey::ArrowUp) => self.search.step(&mut term, Direction::Left),
            Key::Named(NamedKey::ArrowDown) => self.search.step(&mut term, Direction::Right),
            Key::Named(NamedKey::Backspace) => {
                let mut q = self.search.query.clone();
                if m.control_key() {
                    q.clear();
                } else {
                    q.pop();
                }
                self.search.set_query(q, &mut term);
            }
            Key::Character(c) if m.control_key() && c.eq_ignore_ascii_case("r") => self.search.toggle_regex(&mut term),
            Key::Character(c) if m.control_key() && c.eq_ignore_ascii_case("v") => {
                drop(term);
                return self.paste();
            }
            _ => {
                if m.control_key() || m.super_key() {
                    return;
                }
                if let Some(text) = event.text.as_deref().filter(|t| t.chars().all(|c| !c.is_control())) {
                    let q = format!("{}{text}", self.search.query);
                    self.search.set_query(q, &mut term);
                }
            }
        }
    }

    fn on_mouse_move(&mut self, cfg: &Config, x: f32, y: f32) {
        self.mouse = (x, y);
        let (w, h) = self.renderer.size();
        let cell = self.renderer.cell();
        let scale = self.scale();
        let mut term = self.session.term.lock();
        let grid = term.grid();
        let (hist, screen, offset) = (grid.history_size(), grid.screen_lines(), grid.display_offset());
        let geo = ui::scroll_geometry(w as f32, h as f32, scale, hist, screen, offset);

        let over_bar = geo.as_ref().is_some_and(|g| g.track.contains(x, y));
        let search_rect = ui::search_geometry(w as f32, scale, cell.width, cell.height).bar;
        let over_search = self.search.open && search_rect.contains(x, y);
        let redraw = over_bar != self.scrollbar.hover || over_search != self.search_hover;
        self.scrollbar.hover = over_bar;
        self.search_hover = over_search;
        self.window.set_cursor(if over_bar || over_search { CursorIcon::Default } else { CursorIcon::Text });

        if let (Some(grab), Some(g)) = (self.scrollbar.drag, geo.as_ref()) {
            let target = ui::offset_for_thumb(g, y - grab, hist) as i32;
            term.scroll_display(Scroll::Delta(target - offset as i32));
            drop(term);
            self.scrolled();
            return self.window.request_redraw();
        }

        let (point, side, col, row) = self.mouse_cell(cfg, offset);
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
            return self.window.request_redraw();
        }
        drop(term);
        if redraw {
            self.window.request_redraw();
        }
    }

    fn on_mouse_button(&mut self, cfg: &Config, state: ElementState, button: WinitButton) {
        let (w, h) = self.renderer.size();
        let cell = self.renderer.cell();
        let scale = self.scale();
        let (x, y) = self.mouse;
        let pressed = state == ElementState::Pressed;

        if pressed && button == WinitButton::Left {
            if self.search.open {
                let g = ui::search_geometry(w as f32, scale, cell.width, cell.height);
                if g.close.contains(x, y) {
                    self.search.close();
                    return self.window.request_redraw();
                }
                if g.bar.contains(x, y) {
                    self.search.focused = true;
                    return self.window.request_redraw();
                }
            }
            let mut term = self.session.term.lock();
            let grid = term.grid();
            let (hist, screen, offset) = (grid.history_size(), grid.screen_lines(), grid.display_offset());
            if let Some(g) = ui::scroll_geometry(w as f32, h as f32, scale, hist, screen, offset) {
                if g.track.contains(x, y) {
                    if g.thumb.contains(x, y) {
                        self.scrollbar.drag = Some(y - g.thumb.y);
                    } else {
                        term.scroll_display(if y < g.thumb.y { Scroll::PageUp } else { Scroll::PageDown });
                    }
                    drop(term);
                    self.scrolled();
                    return self.window.request_redraw();
                }
            }
        }
        if !pressed && button == WinitButton::Left && self.scrollbar.drag.take().is_some() {
            return self.window.request_redraw();
        }

        let mut term = self.session.term.lock();
        let offset = term.grid().display_offset();
        let (point, side, col, row) = self.mouse_cell(cfg, offset);
        let mode = *term.mode();

        if mode.intersects(TermMode::MOUSE_MODE) && !self.mods.shift_key() {
            let b = match button {
                WinitButton::Left => MouseButton::Left,
                WinitButton::Middle => MouseButton::Middle,
                WinitButton::Right => MouseButton::Right,
                _ => return,
            };
            self.reported_button = pressed.then_some(b);
            self.last_reported_cell = Some((col, row));
            if let Some(bytes) = input::encode_mouse(b, pressed, false, col, row, self.mods, mode) {
                drop(term);
                self.write(bytes);
            }
            return;
        }

        match (button, pressed) {
            (WinitButton::Left, true) => {
                if self.search.focused {
                    self.search.focused = false;
                }
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
                self.selecting = true;
            }
            (WinitButton::Left, false) => {
                self.selecting = false;
                if term.selection.as_ref().is_some_and(Selection::is_empty) {
                    term.selection = None;
                }
            }
            (WinitButton::Middle, true) => {
                drop(term);
                self.paste();
            }
            _ => {}
        }
        self.window.request_redraw();
    }

    fn on_wheel(&mut self, cfg: &Config, delta: MouseScrollDelta) {
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

        let mut term = self.session.term.lock();
        let mode = *term.mode();
        let offset = term.grid().display_offset();
        if mode.intersects(TermMode::MOUSE_MODE) && !self.mods.shift_key() {
            let (_, _, col, row) = self.mouse_cell(cfg, offset);
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

    fn on_term_event(&mut self, el: &ActiveEventLoop, event: TermEvent) {
        match event {
            TermEvent::Wakeup => {
                self.search.content_changed();
                self.window.request_redraw();
            }
            TermEvent::Title(t) => self.window.set_title(&t),
            TermEvent::ResetTitle => self.window.set_title(TITLE),
            TermEvent::ClipboardStore(ClipboardType::Clipboard | ClipboardType::Selection, text) => {
                if let Some(cb) = self.clipboard.as_mut() {
                    let _ = cb.set_text(text);
                }
            }
            TermEvent::ClipboardLoad(_, format) => {
                if let Some(text) = self.clipboard_text() {
                    self.write(format(&text).into_bytes());
                }
            }
            TermEvent::ColorRequest(index, format) => {
                let c = view::color_for_index(&self.session.term.lock(), index, &self.palette);
                let reply = format(alacritty_terminal::vte::ansi::Rgb { r: c.r, g: c.g, b: c.b });
                self.write(reply.into_bytes());
            }
            TermEvent::PtyWrite(s) => self.write(s.into_bytes()),
            TermEvent::TextAreaSizeRequest(format) => self.write(format(self.grid).into_bytes()),
            TermEvent::Bell => {
                if !self.focused {
                    self.window.request_user_attention(Some(UserAttentionType::Informational));
                }
            }
            TermEvent::Exit | TermEvent::ChildExit(_) => el.exit(),
            TermEvent::CursorBlinkingChange | TermEvent::MouseCursorDirty => {}
        }
    }
}

fn open_settings() {
    let Some(path) = Config::path() else { return };
    let result = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").arg("/C").arg("start").arg("").arg(&path).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-t").arg(&path).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&path).spawn()
    };
    if let Err(e) = result {
        log::warn!("could not open {}: {e}", path.display());
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.create(el) {
            Ok(st) => self.state = Some(st),
            Err(e) => {
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        let UserEvent::Term(event) = event;
        if let Some(st) = self.state.as_mut() {
            st.on_term_event(el, event);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let cfg = &self.cfg;
        let Some(st) = self.state.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => st.redraw(cfg),
            WindowEvent::Resized(size) => {
                st.renderer.resize(size.width, size.height);
                st.resize_grid(cfg);
                st.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                st.reload_font(cfg);
                st.window.request_redraw();
            }
            WindowEvent::ThemeChanged(t) => {
                st.os_dark = t == Theme::Dark;
                st.apply_palette(cfg);
                st.window.request_redraw();
            }
            WindowEvent::Focused(f) => {
                st.focused = f;
                let mut term = st.session.term.lock();
                term.is_focused = f;
                let report = term.mode().contains(TermMode::FOCUS_IN_OUT);
                drop(term);
                if report {
                    st.write(if f { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] });
                }
                st.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => st.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => st.on_key(cfg, event),
            WindowEvent::Ime(Ime::Commit(text)) => {
                if st.search.focused {
                    let q = format!("{}{text}", st.search.query);
                    st.search.set_query(q, &mut st.session.term.lock());
                    st.window.request_redraw();
                } else {
                    st.write(text.into_bytes());
                }
            }
            WindowEvent::CursorMoved { position, .. } => st.on_mouse_move(cfg, position.x as f32, position.y as f32),
            WindowEvent::CursorLeft { .. } => {
                st.scrollbar.hover = false;
                st.search_hover = false;
                st.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => st.on_mouse_button(cfg, state, button),
            WindowEvent::MouseWheel { delta, .. } => st.on_wheel(cfg, delta),
            _ => {}
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
        if let Some(st) = self.state.as_mut() {
            if let Some(t) = st.wake_at {
                if now >= t {
                    st.wake_at = None;
                    st.window.request_redraw();
                } else {
                    deadline = deadline.min(t);
                }
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(deadline));
    }
}
