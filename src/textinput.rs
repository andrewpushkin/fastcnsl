//! A text box's contents and caret: the AI chat's input.

/// Text being typed, with a caret (a byte index on a `char` boundary).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextInput {
    text: String,
    caret: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Inserts `s` at the caret. Line ends become `\n`; other control
    /// characters (tabs, escapes) are dropped.
    pub fn insert(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let clean: String = s.chars().filter(|&c| c == '\n' || !c.is_control()).collect();
        self.text.insert_str(self.caret, &clean);
        self.caret += clean.len();
    }

    pub fn newline(&mut self) {
        self.insert("\n");
    }

    pub fn backspace(&mut self) {
        if let Some(c) = self.text[..self.caret].chars().next_back() {
            self.caret -= c.len_utf8();
            self.text.remove(self.caret);
        }
    }

    pub fn delete(&mut self) {
        if self.caret < self.text.len() {
            self.text.remove(self.caret);
        }
    }

    pub fn left(&mut self) {
        if let Some(c) = self.text[..self.caret].chars().next_back() {
            self.caret -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.caret..].chars().next() {
            self.caret += c.len_utf8();
        }
    }

    /// To the start of the caret's line.
    pub fn home(&mut self) {
        self.caret = self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
    }

    /// To the end of the caret's line.
    pub fn end(&mut self) {
        self.caret += self.text[self.caret..].find('\n').unwrap_or(self.text.len() - self.caret);
    }

    /// The text, trimmed, leaving the box empty.
    pub fn take(&mut self) -> String {
        self.caret = 0;
        std::mem::take(&mut self.text).trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_editing_at_the_caret() {
        let mut t = TextInput::new();
        t.insert("hllo");
        t.home();
        t.right();
        t.insert("e");
        assert_eq!((t.text(), t.caret()), ("hello", 2));
        t.end();
        t.backspace();
        t.left();
        t.delete();
        assert_eq!(t.text(), "hel");
    }

    #[test]
    fn multibyte_and_lines() {
        let mut t = TextInput::new();
        t.insert("añb");
        t.left();
        t.left();
        t.backspace();
        assert_eq!(t.text(), "ñb");
        t.end();
        t.newline();
        t.insert("x\r\ny\tz\x1b");
        assert_eq!(t.text(), "ñb\nx\nyz");
        t.home();
        assert_eq!(t.caret(), "ñb\nx\n".len());
    }

    #[test]
    fn take_trims_and_empties() {
        let mut t = TextInput::new();
        t.insert("  hi \n");
        assert_eq!(t.take(), "hi");
        assert!(t.is_empty() && t.caret() == 0);
    }

    #[test]
    fn edges_do_nothing() {
        let mut t = TextInput::new();
        t.backspace();
        t.delete();
        t.left();
        t.right();
        assert!(t.is_empty());
        t.insert("ab");
        t.right();
        t.delete();
        assert_eq!((t.text(), t.caret()), ("ab", 2));
    }
}
