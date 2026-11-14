//! The AI chat panel's layout: wrapped messages, cards and their buttons,
//! the input box, and what a point in the panel hits. Pure geometry; the
//! app owns the state and `view.rs` draws the result.

use crate::ai::chat::{CardState, Chat, Entry};
use crate::ai::context::Budget;
use crate::grid_width;
use crate::textinput::TextInput;
use crate::ui::Rect;

/// Space around the panel's parts, in pixels at scale 1.
const PAD: f32 = 10.0;
/// Space inside the input box, at scale 1.
pub const INPUT_PAD: f32 = 6.0;
/// The input box grows to this many lines, then scrolls.
const INPUT_LINES: usize = 6;
const PLACEHOLDER: &str = "Message the AI · Enter sends, Shift+Enter new line";

/// A tab's chat: the conversation and the panel showing it.
pub struct ChatPanel {
    pub chat: Chat,
    pub input: TextInput,
    pub open: bool,
    pub focused: bool,
    /// Pixels scrolled up from the bottom; 0 follows new messages.
    pub scroll: f32,
    /// The command the next message asks about (global line), from
    /// "Ask AI in chat".
    pub about: Option<u64>,
    /// What is left of the terminal text the current message may send,
    /// and the bookmarks the AI set while answering it.
    pub budget: Budget,
    pub bookmarked: Vec<u64>,
    /// What the AI is doing while a message is answered ("Asking Claude…").
    pub activity: String,
}

impl ChatPanel {
    pub fn new() -> Self {
        ChatPanel { chat: Chat::new(), input: TextInput::new(), open: false, focused: false, scroll: 0.0, about: None, budget: Budget::new(), bookmarked: vec![], activity: String::new() }
    }

    /// Clears the conversation; the panel stays as it is.
    pub fn new_chat(&mut self) {
        self.chat = Chat::new();
        self.scroll = 0.0;
        self.about = None;
        self.budget = Budget::new();
        self.bookmarked.clear();
    }
}

/// What a point in the panel is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatHit {
    Divider,
    NewChat,
    Close,
    /// Buttons of entry `i`.
    Copy(usize),
    Insert(usize),
    Run(usize),
    Continue(usize),
    Input,
    Messages,
}

/// The font a line is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Ui,
    Term,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub face: Face,
    pub dim: bool,
}

pub struct ChatLayout {
    pub area: Rect,
    pub divider: Rect,
    pub header: Rect,
    pub title: String,
    pub title_y: f32,
    /// Header and per-entry buttons, with their labels.
    pub buttons: Vec<(Rect, ChatHit, &'static str)>,
    /// Behind user messages, code and card commands.
    pub tints: Vec<Rect>,
    /// Messages (clipped to their area) and the status line.
    pub lines: Vec<Line>,
    pub input: Rect,
    pub input_lines: Vec<String>,
    /// Top left of the caret, one UI line tall.
    pub caret: (f32, f32),
    /// `input_lines` is the placeholder: the box is empty.
    pub placeholder: bool,
    /// The most the messages scroll: their height past the space they have.
    pub max_scroll: f32,
}

impl ChatLayout {
    pub fn hit(&self, x: f32, y: f32) -> Option<ChatHit> {
        if self.divider.contains(x, y) {
            return Some(ChatHit::Divider);
        }
        if let Some((_, hit, _)) = self.buttons.iter().find(|(r, _, _)| r.contains(x, y)) {
            return Some(*hit);
        }
        if self.input.contains(x, y) {
            return Some(ChatHit::Input);
        }
        if self.header.contains(x, y) || !self.area.contains(x, y) {
            return None;
        }
        Some(ChatHit::Messages)
    }
}

fn text_width(s: &str) -> usize {
    s.chars().map(grid_width).sum()
}

/// `s` cut to `cols` columns, with `…` when cut.
fn fit(s: &str, cols: usize) -> String {
    if text_width(s) <= cols {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        if w + grid_width(c) + 1 > cols {
            break;
        }
        w += grid_width(c);
        out.push(c);
    }
    out.push('…');
    out
}

/// Breaks each line of `text` into pieces of at most `cols` columns,
/// wherever it falls. A character wider than `cols` gets a line of its own.
pub fn hard_wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut out = vec![];
    for line in text.split('\n') {
        let mut cur = String::new();
        let mut w = 0;
        for c in line.chars() {
            let cw = grid_width(c);
            if w + cw > cols && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(c);
            w += cw;
        }
        out.push(cur);
    }
    out
}

/// Word-wraps `text` to `cols` columns: lines break at spaces, words
/// longer than a line are broken, blank lines are kept.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut out = vec![];
    for line in text.split('\n') {
        let mut cur = String::new();
        for word in line.split(' ').filter(|w| !w.is_empty()) {
            let (ww, cw) = (text_width(word), text_width(&cur));
            if !cur.is_empty() && cw + 1 + ww <= cols {
                cur.push(' ');
                cur.push_str(word);
                continue;
            }
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if ww <= cols {
                cur.push_str(word);
            } else {
                let mut pieces = hard_wrap(word, cols);
                cur = pieces.pop().unwrap_or_default();
                out.extend(pieces);
            }
        }
        out.push(cur);
    }
    out
}

/// Splits a message into prose and fenced code (lines starting with
/// ```), without the fences. An unclosed fence runs to the end.
pub fn blocks(text: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = vec![];
    let mut code = false;
    let mut cur: Vec<&str> = vec![];
    let flush = |code: bool, cur: &mut Vec<&str>, out: &mut Vec<(bool, String)>| {
        if !cur.is_empty() {
            out.push((code, cur.join("\n")));
            cur.clear();
        }
    };
    for line in text.split('\n') {
        if line.trim_start().starts_with("```") {
            flush(code, &mut cur, &mut out);
            code = !code;
        } else {
            cur.push(line);
        }
    }
    flush(code, &mut cur, &mut out);
    out
}

/// Parts of the messages, placed from the top of the content.
enum Item {
    Line(Line, f32),
    Tint(Rect),
    Button(Rect, ChatHit, &'static str),
}

/// Lays out the panel in `area` (the window's right-hand part under the
/// tab bar). `ui` and `term` are the two fonts' cells. `title` is the
/// header text ("Claude · sonnet"); the part before " · " names the AI's
/// messages. `status` shows above the input box.
pub fn layout(area: Rect, ui: (f32, f32), term: (f32, f32), scale: f32, panel: &ChatPanel, title: &str, status: Option<String>) -> ChatLayout {
    let (uw, uh) = ui;
    let (tw, th) = term;
    let pad = (PAD * scale).round();
    let ipad = (INPUT_PAD * scale).round();
    let right = area.x + area.w;
    let bottom = area.y + area.h;
    let divider = Rect { x: area.x - (3.0 * scale).round(), y: area.y, w: (6.0 * scale).round(), h: area.h };

    // Header: the title, New chat and close.
    let header = Rect { x: area.x, y: area.y, w: area.w, h: uh + pad };
    let button = |label: &str, right_x: f32, y: f32| {
        let w = text_width(label) as f32 * uw + pad;
        Rect { x: right_x - w, y, w, h: uh + pad / 2.0 }
    };
    let by = header.y + pad / 4.0;
    let close = button("×", right - pad / 2.0, by);
    let new_chat = button("New chat", close.x - pad / 2.0, by);
    let mut buttons = vec![(new_chat, ChatHit::NewChat, "New chat"), (close, ChatHit::Close, "×")];
    let ai_name = title.split(" · ").next().unwrap_or("AI").to_owned();
    let title_cols = ((new_chat.x - area.x - pad) / uw).floor().max(0.0) as usize;
    let title = fit(title, title_cols);
    let title_y = header.y + pad / 2.0;

    // The input box, at the bottom.
    let inner_cols = ((area.w - 2.0 * pad - 2.0 * ipad) / uw).floor().max(1.0) as usize;
    let text = panel.input.text();
    let placeholder = text.is_empty();
    let (input_lines, caret_at) = if placeholder {
        (vec![fit(PLACEHOLDER, inner_cols)], (0, 0))
    } else {
        let all = hard_wrap(text, inner_cols);
        let before = hard_wrap(&text[..panel.input.caret()], inner_cols);
        let line = before.len() - 1;
        let first = if all.len() <= INPUT_LINES { 0 } else { line.saturating_sub(INPUT_LINES - 1).min(all.len() - INPUT_LINES) };
        let shown = all[first..(first + INPUT_LINES).min(all.len())].to_vec();
        (shown, (line - first, text_width(&before[line])))
    };
    let ih = input_lines.len() as f32 * uh + 2.0 * ipad;
    let input = Rect { x: area.x + pad, y: bottom - pad - ih, w: area.w - 2.0 * pad, h: ih };
    let caret = (input.x + ipad + caret_at.1 as f32 * uw, input.y + ipad + caret_at.0 as f32 * uh);

    // The status line, above the input box.
    let cols = ((area.w - 2.0 * pad) / uw).floor().max(1.0) as usize;
    let mut lines = vec![];
    let mut msg_bottom = input.y - pad / 2.0;
    if let Some(s) = status {
        let y = msg_bottom - uh;
        lines.push(Line { x: area.x + pad, y, text: fit(&s, cols), face: Face::Ui, dim: true });
        msg_bottom = y - pad / 2.0;
    }
    let msg = Rect { x: area.x, y: header.y + header.h + pad / 2.0, w: area.w, h: (msg_bottom - header.y - header.h - pad / 2.0).max(0.0) };

    // The messages, from the top of the content.
    let tcols = ((area.w - 2.0 * pad) / tw).floor().max(1.0) as usize;
    let x = area.x + pad;
    let mut items = vec![];
    let mut y = 0.0;
    let ui_line = |text: String, y: f32, dim: bool| Item::Line(Line { x, y, text, face: Face::Ui, dim }, uh);
    for (i, entry) in panel.chat.entries.iter().enumerate() {
        let label = match entry {
            Entry::User(_) => Some("You"),
            Entry::Ai(_) => Some(ai_name.as_str()),
            Entry::Card(_) => Some("Command"),
            Entry::Note(_) => None,
        };
        if let Some(label) = label {
            items.push(ui_line(fit(label, cols.saturating_sub(5)), y, true));
            let copy = button("Copy", right - pad, y);
            items.push(Item::Button(Rect { h: uh, ..copy }, ChatHit::Copy(i), "Copy"));
            y += uh + pad / 4.0;
        }
        let code = |text: &str, y: &mut f32, items: &mut Vec<Item>| {
            let rows = hard_wrap(text, tcols);
            items.push(Item::Tint(Rect { x: area.x + pad / 2.0, y: *y - pad / 4.0, w: area.w - pad, h: rows.len() as f32 * th + pad / 2.0 }));
            for row in rows {
                items.push(Item::Line(Line { x, y: *y, text: row, face: Face::Term, dim: false }, th));
                *y += th;
            }
            *y += pad / 2.0;
        };
        match entry {
            Entry::User(text) => {
                let rows = wrap(text, cols);
                items.push(Item::Tint(Rect { x: area.x + pad / 2.0, y: y - pad / 4.0, w: area.w - pad, h: rows.len() as f32 * uh + pad / 2.0 }));
                for row in rows {
                    items.push(ui_line(row, y, false));
                    y += uh;
                }
            }
            Entry::Ai(text) => {
                for (is_code, block) in blocks(text) {
                    if is_code {
                        y += pad / 4.0;
                        code(&block, &mut y, &mut items);
                    } else {
                        for row in wrap(&block, cols) {
                            items.push(ui_line(row, y, false));
                            y += uh;
                        }
                    }
                }
            }
            Entry::Note(text) => {
                for row in wrap(text, cols) {
                    items.push(ui_line(row, y, true));
                    y += uh;
                }
            }
            Entry::Card(card) => {
                if !card.explanation.is_empty() {
                    for row in wrap(&card.explanation, cols) {
                        items.push(ui_line(row, y, true));
                        y += uh;
                    }
                }
                y += pad / 4.0;
                code(&card.command, &mut y, &mut items);
                let row_h = uh + pad / 2.0;
                let left = |label: &str, x: f32| Rect { x, y, w: text_width(label) as f32 * uw + pad, h: row_h };
                match card.state {
                    CardState::Waiting => {
                        let insert = left("Insert", x);
                        let run = left("Run", insert.x + insert.w + pad / 2.0);
                        items.push(Item::Button(insert, ChatHit::Insert(i), "Insert"));
                        items.push(Item::Button(run, ChatHit::Run(i), "Run"));
                    }
                    CardState::Ran { .. } => items.push(Item::Button(left("Continue", x), ChatHit::Continue(i), "Continue")),
                    CardState::Inserted => items.push(ui_line("inserted".into(), y + pad / 4.0, true)),
                    CardState::Answered => items.push(ui_line("sent".into(), y + pad / 4.0, true)),
                    CardState::Skipped => items.push(ui_line("skipped".into(), y + pad / 4.0, true)),
                }
                y += row_h;
            }
        }
        y += pad;
    }
    let content_height = y;

    // Short content starts at the top; long content ends at the bottom,
    // shifted by the scroll.
    let max_scroll = (content_height - msg.h).max(0.0);
    let base = if content_height <= msg.h { msg.y } else { msg.y + msg.h - content_height + panel.scroll.clamp(0.0, max_scroll) };
    let visible = |y: f32, h: f32| y >= msg.y && y + h <= msg.y + msg.h;
    let mut tints = vec![];
    for item in items {
        match item {
            Item::Line(mut line, h) => {
                line.y += base;
                if visible(line.y, h) {
                    lines.push(line);
                }
            }
            Item::Tint(mut r) => {
                r.y += base;
                let top = r.y.max(msg.y);
                let end = (r.y + r.h).min(msg.y + msg.h);
                if end > top {
                    tints.push(Rect { y: top, h: end - top, ..r });
                }
            }
            Item::Button(mut r, hit, label) => {
                r.y += base;
                if visible(r.y, r.h) {
                    buttons.push((r, hit, label));
                }
            }
        }
    }
    ChatLayout { area, divider, header, title, title_y, buttons, tints, lines, input, input_lines, caret, placeholder, max_scroll }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{Card, CardState, Entry};

    const UI: (f32, f32) = (8.0, 16.0);
    const TERM: (f32, f32) = (9.0, 18.0);

    fn area(w: f32, h: f32) -> Rect {
        Rect { x: 500.0, y: 30.0, w, h }
    }

    fn card(state: CardState) -> Entry {
        Entry::Card(Card { call_id: "r1".into(), command: "ls -la".into(), explanation: "Lists files.".into(), state })
    }

    fn panel(entries: Vec<Entry>) -> ChatPanel {
        let mut p = ChatPanel::new();
        p.chat.entries = entries;
        p
    }

    fn lay(p: &ChatPanel, a: Rect) -> ChatLayout {
        layout(a, UI, TERM, 1.0, p, "Claude · sonnet", None)
    }

    fn centre(l: &ChatLayout, hit: ChatHit) -> (f32, f32) {
        let (r, _, _) = l.buttons.iter().find(|(_, h, _)| *h == hit).unwrap_or_else(|| panic!("no {hit:?} button"));
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    fn width(line: &Line) -> f32 {
        let cw = if line.face == Face::Term { TERM.0 } else { UI.0 };
        line.text.chars().map(crate::grid_width).sum::<usize>() as f32 * cw
    }

    #[test]
    fn wrap_breaks_at_spaces_and_keeps_blank_lines() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
    }

    #[test]
    fn wrap_hard_breaks_long_words_and_tiny_widths() {
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("ab", 0), ["a", "b"]);
        assert!(wrap("日本語テキスト", 4).iter().all(|l| l.chars().map(crate::grid_width).sum::<usize>() <= 4));
        assert_eq!(wrap("日", 1), ["日"]);
    }

    #[test]
    fn fenced_code_is_its_own_block() {
        assert_eq!(blocks("Run:\n```sh\nls -la\n```\nok"), [(false, "Run:".to_owned()), (true, "ls -la".to_owned()), (false, "ok".to_owned())]);
        // An unclosed fence runs to the end.
        assert_eq!(blocks("```\nx"), [(true, "x".to_owned())]);
    }

    #[test]
    fn a_waiting_card_offers_insert_and_run() {
        let p = panel(vec![Entry::User("hi".into()), card(CardState::Waiting)]);
        let a = area(400.0, 600.0);
        let l = lay(&p, a);
        for hit in [ChatHit::Insert(1), ChatHit::Run(1), ChatHit::NewChat, ChatHit::Close, ChatHit::Copy(0), ChatHit::Copy(1)] {
            let (x, y) = centre(&l, hit);
            assert_eq!(l.hit(x, y), Some(hit));
        }
        assert!(!l.buttons.iter().any(|(_, h, _)| matches!(h, ChatHit::Continue(_))));
        assert_eq!(l.hit(a.x, a.y + 200.0), Some(ChatHit::Divider));
        assert_eq!(l.hit(l.input.x + 4.0, l.input.y + 4.0), Some(ChatHit::Input));
        assert_eq!(l.hit(a.x + 200.0, a.y + 300.0), Some(ChatHit::Messages));
        assert_eq!(l.hit(a.x - 50.0, a.y + 300.0), None);
        assert!(l.placeholder);
    }

    #[test]
    fn a_ran_card_offers_continue_only() {
        let l = lay(&panel(vec![card(CardState::Ran { line: 4 })]), area(400.0, 600.0));
        let (x, y) = centre(&l, ChatHit::Continue(0));
        assert_eq!(l.hit(x, y), Some(ChatHit::Continue(0)));
        assert!(!l.buttons.iter().any(|(_, h, _)| matches!(h, ChatHit::Insert(_) | ChatHit::Run(_))));
        let done = lay(&panel(vec![card(CardState::Skipped)]), area(400.0, 600.0));
        assert!(done.lines.iter().any(|line| line.text == "skipped" && line.dim));
    }

    #[test]
    fn every_line_fits_the_panel() {
        let long = format!("Here is {} and code:\n```\n{}\n```\nend", "averyveryverylongword".repeat(3), "x".repeat(80));
        let p = panel(vec![Entry::User(long.clone()), Entry::Ai(long), card(CardState::Waiting)]);
        let a = area(120.0, 2000.0);
        let l = lay(&p, a);
        assert!(l.lines.len() > 10);
        for line in &l.lines {
            assert!(line.x >= a.x && line.x + width(line) <= a.x + a.w + 0.01, "{line:?}");
            assert!(line.y >= l.header.y + l.header.h && line.y < l.input.y, "{line:?}");
        }
        assert!(l.lines.iter().any(|line| line.face == Face::Term));
    }

    #[test]
    fn scroll_shows_older_lines() {
        let entries: Vec<Entry> = (0..200).map(|i| Entry::User(format!("message {i}"))).collect();
        let mut p = panel(entries);
        let a = area(400.0, 600.0);
        let l = lay(&p, a);
        let shows = |l: &ChatLayout, t: &str| l.lines.iter().any(|line| line.text == t);
        assert!(shows(&l, "message 199") && !shows(&l, "message 0"));
        p.scroll = l.max_scroll;
        let top = lay(&p, a);
        assert!(shows(&top, "message 0") && !shows(&top, "message 199"));
    }

    #[test]
    fn scrolling_stops_at_the_oldest_message() {
        let mut p = panel((0..200).map(|i| Entry::User(format!("message {i}"))).collect());
        let a = area(400.0, 600.0);
        let l = lay(&p, a);
        assert!(l.max_scroll > 0.0);
        p.scroll = l.max_scroll;
        let at_top = lay(&p, a);
        p.scroll = l.max_scroll + 500.0;
        let past = lay(&p, a);
        let first = |l: &ChatLayout| l.lines.iter().map(|x| x.y).fold(f32::MAX, f32::min);
        assert_eq!(first(&at_top), first(&past));
        assert_eq!(lay(&panel(vec![Entry::User("hi".into())]), a).max_scroll, 0.0);
    }

    #[test]
    fn the_input_wraps_and_shows_the_caret() {
        let mut p = panel(vec![]);
        p.input.insert("abcdefghij");
        let a = area(100.0, 600.0);
        let l = lay(&p, a);
        assert!(!l.placeholder && l.input_lines.len() > 1, "{:?}", l.input_lines);
        let last = l.input_lines.last().unwrap();
        // The caret is at the end of the last line.
        assert_eq!(l.caret.0, l.input.x + INPUT_PAD + last.chars().count() as f32 * UI.0);
        // At most six lines show, the caret's among them.
        p.input.insert(&"\nx".repeat(20));
        let tall = lay(&p, a);
        assert_eq!(tall.input_lines.len(), 6);
        assert_eq!(tall.input_lines.last().unwrap(), "x");
    }

    #[test]
    fn status_shows_above_the_input() {
        let p = panel(vec![]);
        let l = layout(area(400.0, 600.0), UI, TERM, 1.0, &p, "t", Some("Asking… Esc stops".into()));
        let s = l.lines.iter().find(|line| line.text.starts_with("Asking")).unwrap();
        assert!(s.y < l.input.y && s.dim);
    }
}
