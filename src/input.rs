//! Keyboard and mouse encoding: winit events → bytes for the shell.
//!
//! Fixed sequences are `&'static [u8]` and are sent as `Cow::Borrowed`,
//! so ordinary keys (Enter, arrows, Ctrl+C...) never allocate.

use std::borrow::Cow;

use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub type Bytes = Cow<'static, [u8]>;

/// xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn mod_param(m: ModifiersState) -> u8 {
    1 + m.shift_key() as u8 + 2 * m.alt_key() as u8 + 4 * m.control_key() as u8
}

fn has_mods(m: ModifiersState) -> bool {
    m.shift_key() || m.alt_key() || m.control_key()
}

/// Cursor-style keys: `ESC [ X` / `ESC O X` (application mode), or
/// `ESC [ 1 ; m X` with modifiers.
fn cursor_key(
    normal: &'static [u8],
    app: &'static [u8],
    letter: char,
    m: ModifiersState,
    mode: TermMode,
) -> Bytes {
    if has_mods(m) {
        Cow::Owned(format!("\x1b[1;{}{letter}", mod_param(m)).into_bytes())
    } else if mode.contains(TermMode::APP_CURSOR) {
        Cow::Borrowed(app)
    } else {
        Cow::Borrowed(normal)
    }
}

/// `ESC [ n ~` keys, with `ESC [ n ; m ~` when modified.
fn tilde_key(plain: &'static [u8], n: u8, m: ModifiersState) -> Bytes {
    if has_mods(m) {
        Cow::Owned(format!("\x1b[{n};{}~", mod_param(m)).into_bytes())
    } else {
        Cow::Borrowed(plain)
    }
}

fn ss3_key(plain: &'static [u8], letter: char, m: ModifiersState) -> Bytes {
    if has_mods(m) {
        Cow::Owned(format!("\x1b[1;{}{letter}", mod_param(m)).into_bytes())
    } else {
        Cow::Borrowed(plain)
    }
}

/// Encodes a key press. `text` is what the layout produced (with Shift
/// applied); `None` means nothing should be sent.
pub fn encode_key(key: &Key, text: Option<&str>, m: ModifiersState, mode: TermMode) -> Option<Bytes> {
    let alt_prefix = m.alt_key() && !cfg!(target_os = "macos");
    let b = |s: &'static [u8]| Some(Cow::Borrowed(s));
    match key {
        Key::Named(named) => match named {
            NamedKey::Enter => if alt_prefix { b(b"\x1b\r") } else { b(b"\r") },
            NamedKey::Backspace => {
                if m.control_key() {
                    b(b"\x08")
                } else if alt_prefix {
                    b(b"\x1b\x7f")
                } else {
                    b(b"\x7f")
                }
            }
            NamedKey::Tab => if m.shift_key() { b(b"\x1b[Z") } else { b(b"\t") },
            NamedKey::Escape => b(b"\x1b"),
            NamedKey::Space => {
                if m.control_key() {
                    b(b"\0")
                } else if alt_prefix {
                    b(b"\x1b ")
                } else {
                    b(b" ")
                }
            }
            NamedKey::ArrowUp => Some(cursor_key(b"\x1b[A", b"\x1bOA", 'A', m, mode)),
            NamedKey::ArrowDown => Some(cursor_key(b"\x1b[B", b"\x1bOB", 'B', m, mode)),
            NamedKey::ArrowRight => Some(cursor_key(b"\x1b[C", b"\x1bOC", 'C', m, mode)),
            NamedKey::ArrowLeft => Some(cursor_key(b"\x1b[D", b"\x1bOD", 'D', m, mode)),
            NamedKey::Home => Some(cursor_key(b"\x1b[H", b"\x1bOH", 'H', m, mode)),
            NamedKey::End => Some(cursor_key(b"\x1b[F", b"\x1bOF", 'F', m, mode)),
            NamedKey::Insert => Some(tilde_key(b"\x1b[2~", 2, m)),
            NamedKey::Delete => Some(tilde_key(b"\x1b[3~", 3, m)),
            NamedKey::PageUp => Some(tilde_key(b"\x1b[5~", 5, m)),
            NamedKey::PageDown => Some(tilde_key(b"\x1b[6~", 6, m)),
            NamedKey::F1 => Some(ss3_key(b"\x1bOP", 'P', m)),
            NamedKey::F2 => Some(ss3_key(b"\x1bOQ", 'Q', m)),
            NamedKey::F3 => Some(ss3_key(b"\x1bOR", 'R', m)),
            NamedKey::F4 => Some(ss3_key(b"\x1bOS", 'S', m)),
            NamedKey::F5 => Some(tilde_key(b"\x1b[15~", 15, m)),
            NamedKey::F6 => Some(tilde_key(b"\x1b[17~", 17, m)),
            NamedKey::F7 => Some(tilde_key(b"\x1b[18~", 18, m)),
            NamedKey::F8 => Some(tilde_key(b"\x1b[19~", 19, m)),
            NamedKey::F9 => Some(tilde_key(b"\x1b[20~", 20, m)),
            NamedKey::F10 => Some(tilde_key(b"\x1b[21~", 21, m)),
            NamedKey::F11 => Some(tilde_key(b"\x1b[23~", 23, m)),
            NamedKey::F12 => Some(tilde_key(b"\x1b[24~", 24, m)),
            _ => text.map(|t| owned_text(t, alt_prefix)),
        },
        Key::Character(ch) if m.control_key() => {
            let mut chars = ch.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return text.map(|t| owned_text(t, alt_prefix));
            };
            let Some(code) = ctrl_code(c) else {
                return text.map(|t| owned_text(t, alt_prefix));
            };
            Some(if alt_prefix { Cow::Owned(vec![0x1b, code]) } else { Cow::Owned(vec![code]) })
        }
        _ => text.filter(|t| !t.is_empty()).map(|t| owned_text(t, alt_prefix)),
    }
}

fn owned_text(t: &str, alt_prefix: bool) -> Bytes {
    let mut v = Vec::with_capacity(t.len() + 1);
    if alt_prefix {
        v.push(0x1b);
    }
    v.extend_from_slice(t.as_bytes());
    Cow::Owned(v)
}

/// Control character produced by Ctrl+`c`.
fn ctrl_code(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

/// Encodes a mouse event for applications that enabled mouse reporting
/// (vim, htop, tmux...). `col`/`row` are 0-based viewport cells.
pub fn encode_mouse(
    button: MouseButton,
    pressed: bool,
    motion: bool,
    col: usize,
    row: usize,
    m: ModifiersState,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let mut code: u8 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if motion {
        code += 32;
    }
    if m.shift_key() {
        code += 4;
    }
    if m.alt_key() {
        code += 8;
    }
    if m.control_key() {
        code += 16;
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes());
    }
    // Legacy X10 encoding: one byte per value, limited to 223 columns/rows.
    if col >= 223 || row >= 223 {
        return None;
    }
    let code = if pressed || matches!(button, MouseButton::WheelUp | MouseButton::WheelDown) {
        code
    } else {
        3 + (code & !3)
    };
    Some(vec![0x1b, b'[', b'M', 32 + code, 32 + col as u8 + 1, 32 + row as u8 + 1])
}

/// Prepares clipboard text for pasting: newlines become CR, and in
/// bracketed-paste mode the text is wrapped and any embedded end marker
/// (or ESC) is stripped so the paste can't inject commands.
pub fn paste_bytes(text: &str, mode: TermMode) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let clean = text.replace('\x1b', "");
        let mut v = Vec::with_capacity(clean.len() + 12);
        v.extend_from_slice(b"\x1b[200~");
        v.extend_from_slice(clean.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key, text: Option<&str>, m: ModifiersState) -> Vec<u8> {
        encode_key(&key, text, m, TermMode::empty()).unwrap().into_owned()
    }

    #[test]
    fn fixed_keys_do_not_allocate() {
        let r = encode_key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(), TermMode::empty());
        assert!(matches!(r, Some(Cow::Borrowed(b"\r"))));
        let r = encode_key(&Key::Named(NamedKey::ArrowUp), None, ModifiersState::empty(), TermMode::empty());
        assert!(matches!(r, Some(Cow::Borrowed(_))));
    }

    #[test]
    fn arrows_respect_app_cursor_mode_and_modifiers() {
        let up = Key::Named(NamedKey::ArrowUp);
        let none = ModifiersState::empty();
        assert_eq!(encode_key(&up, None, none, TermMode::APP_CURSOR).unwrap().as_ref(), b"\x1bOA");
        assert_eq!(enc(up.clone(), None, none), b"\x1b[A");
        assert_eq!(enc(up, None, ModifiersState::CONTROL), b"\x1b[1;5A");
    }

    #[test]
    fn control_letters() {
        let c = Key::Character("c".into());
        assert_eq!(enc(c, Some("c"), ModifiersState::CONTROL), vec![3]);
        let br = Key::Character("[".into());
        assert_eq!(enc(br, None, ModifiersState::CONTROL), vec![0x1b]);
    }

    #[test]
    fn text_and_function_keys() {
        assert_eq!(enc(Key::Character("é".into()), Some("é"), ModifiersState::empty()), "é".as_bytes());
        assert_eq!(enc(Key::Named(NamedKey::F5), None, ModifiersState::empty()), b"\x1b[15~");
        assert_eq!(enc(Key::Named(NamedKey::Delete), None, ModifiersState::SHIFT), b"\x1b[3;2~");
        assert_eq!(enc(Key::Named(NamedKey::Tab), None, ModifiersState::SHIFT), b"\x1b[Z");
    }

    #[test]
    fn mouse_sgr_and_legacy() {
        let none = ModifiersState::empty();
        let sgr = encode_mouse(MouseButton::Left, true, false, 4, 9, none, TermMode::SGR_MOUSE).unwrap();
        assert_eq!(sgr, b"\x1b[<0;5;10M");
        let rel = encode_mouse(MouseButton::Left, false, false, 4, 9, none, TermMode::SGR_MOUSE).unwrap();
        assert_eq!(rel, b"\x1b[<0;5;10m");
        let x10 = encode_mouse(MouseButton::Left, true, false, 0, 0, none, TermMode::empty()).unwrap();
        assert_eq!(x10, vec![0x1b, b'[', b'M', 32, 33, 33]);
    }

    #[test]
    fn alt_prefixes_escape_except_on_macos() {
        let r = enc(Key::Character("x".into()), Some("x"), ModifiersState::ALT);
        let want: &[u8] = if cfg!(target_os = "macos") { b"x" } else { b"\x1bx" };
        assert_eq!(r, want);
    }

    #[test]
    fn special_control_keys() {
        assert_eq!(enc(Key::Named(NamedKey::Space), None, ModifiersState::CONTROL), vec![0]);
        assert_eq!(enc(Key::Named(NamedKey::Backspace), None, ModifiersState::CONTROL), vec![8]);
        assert_eq!(enc(Key::Named(NamedKey::Backspace), None, ModifiersState::empty()), vec![0x7f]);
        assert_eq!(enc(Key::Named(NamedKey::Escape), None, ModifiersState::empty()), vec![0x1b]);
        assert_eq!(enc(Key::Character("?".into()), None, ModifiersState::CONTROL), vec![0x7f]);
        // Ctrl with a key that has no control code falls back to the text.
        assert_eq!(enc(Key::Character("é".into()), Some("é"), ModifiersState::CONTROL), "é".as_bytes());
    }

    #[test]
    fn navigation_keys() {
        let none = ModifiersState::empty();
        assert_eq!(enc(Key::Named(NamedKey::Home), None, none), b"\x1b[H");
        assert_eq!(encode_key(&Key::Named(NamedKey::End), None, none, TermMode::APP_CURSOR).unwrap().as_ref(), b"\x1bOF");
        assert_eq!(enc(Key::Named(NamedKey::PageUp), None, none), b"\x1b[5~");
        assert_eq!(enc(Key::Named(NamedKey::Insert), None, none), b"\x1b[2~");
        assert_eq!(enc(Key::Named(NamedKey::F1), None, none), b"\x1bOP");
        assert_eq!(enc(Key::Named(NamedKey::F1), None, ModifiersState::SHIFT), b"\x1b[1;2P");
        assert_eq!(enc(Key::Named(NamedKey::F12), None, none), b"\x1b[24~");
        assert_eq!(enc(Key::Named(NamedKey::ArrowLeft), None, ModifiersState::ALT | ModifiersState::SHIFT), b"\x1b[1;4D");
    }

    #[test]
    fn keys_without_text_send_nothing() {
        let r = encode_key(&Key::Named(NamedKey::Shift), None, ModifiersState::SHIFT, TermMode::empty());
        assert!(r.is_none());
        let r = encode_key(&Key::Character("a".into()), Some(""), ModifiersState::empty(), TermMode::empty());
        assert!(r.is_none());
    }

    #[test]
    fn mouse_wheel_motion_and_modifiers() {
        let sgr = TermMode::SGR_MOUSE;
        let up = encode_mouse(MouseButton::WheelUp, true, false, 0, 0, ModifiersState::empty(), sgr).unwrap();
        assert_eq!(up, b"\x1b[<64;1;1M");
        let drag = encode_mouse(MouseButton::Left, true, true, 2, 3, ModifiersState::CONTROL, sgr).unwrap();
        assert_eq!(drag, b"\x1b[<48;3;4M");
        // Legacy release reports button 3; far cells can't be encoded.
        let rel = encode_mouse(MouseButton::Right, false, false, 0, 0, ModifiersState::empty(), TermMode::empty()).unwrap();
        assert_eq!(rel[3], 32 + 3);
        assert!(encode_mouse(MouseButton::Left, true, false, 300, 0, ModifiersState::empty(), TermMode::empty()).is_none());
    }

    #[test]
    fn bracketed_paste_strips_escapes() {
        let p = paste_bytes("ls\n\x1b[201~rm -rf /", TermMode::BRACKETED_PASTE);
        assert_eq!(p, b"\x1b[200~ls\r[201~rm -rf /\x1b[201~");
        assert_eq!(paste_bytes("a\r\nb", TermMode::empty()), b"a\rb");
    }
}
