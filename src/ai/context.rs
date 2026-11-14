//! What a Fix sends, the way skills work: a list up front (the tab's recent
//! commands, its bookmarked lines, the last command's output) and other
//! output when the AI asks for it with `get_output`. Lines are named by
//! the numbers the gutter shows in "Per tab" mode: global index + 1.

use std::fmt::Write;
use std::ops::Range;

use serde_json::Value;

/// Output lines sent from the start of a long command's output.
pub const HEAD: usize = 50;
/// Output lines sent from its end, where errors usually are.
pub const TAIL: usize = 150;
/// The most lines one `get_output` range returns.
pub const MAX_RANGE: u64 = 300;
/// Bookmarked lines listed: the most recent ones.
pub const MAX_BOOKMARKS: usize = 20;
/// Terminal text sent in one Fix, in bytes, over all its requests.
pub const BUDGET: usize = 120_000;
/// The most the first request's text may take of [`BUDGET`].
pub const FIRST_MAX: usize = 60_000;
/// Longer lines (minified code, base64) are cut to this many characters.
const MAX_LINE: usize = 1000;

/// The tab, as the AI's tools see it. Line numbers are global indices.
pub trait TerminalText {
    /// The text of lines `r`, from the first one the tab still has.
    fn lines(&mut self, r: Range<u64>) -> Vec<String>;
    /// The prompt's line: output runs up to it.
    fn end(&self) -> u64;
    /// The commands: `(line, local time of day in seconds)`, ascending.
    fn commands(&self) -> Vec<(u64, u64)>;
    /// The bookmarked lines, ascending.
    fn bookmarks(&self) -> Vec<u64>;
    fn add_bookmark(&mut self, line: u64);
    /// The oldest line the tab still has.
    fn first_line(&self) -> u64;
}

/// Where the commands ran.
pub struct Env {
    pub shell: String,
    pub os: &'static str,
    /// The last program started in the tab.
    pub program: String,
}

/// The index of the command global line `line` belongs to: the last one
/// entered on or before it.
pub fn command_of(commands: &[(u64, u64)], line: u64) -> Option<usize> {
    commands.partition_point(|&(c, _)| c <= line).checked_sub(1)
}

/// Global lines of command `i`'s output: up to the next command, or the prompt.
fn output_lines(t: &impl TerminalText, commands: &[(u64, u64)], i: usize) -> Range<u64> {
    let start = commands[i].0 + 1;
    let end = commands.get(i + 1).map_or(t.end(), |&(c, _)| c);
    start..end.max(start)
}

fn cut(line: &str) -> String {
    let line = line.trim_end();
    if line.chars().count() > MAX_LINE { format!("{}…", line.chars().take(MAX_LINE).collect::<String>()) } else { line.to_owned() }
}

/// Appends lines `r` numbered as shown, `*` after a bookmarked line's number.
fn push_numbered(out: &mut String, t: &mut impl TerminalText, r: Range<u64>, bookmarks: &[u64]) {
    let first = r.start.max(t.first_line());
    for (k, text) in t.lines(r).iter().enumerate() {
        let line = first + k as u64;
        let mark = if bookmarks.binary_search(&line).is_ok() { "*" } else { " " };
        let _ = writeln!(out, "{}{mark}| {}", line + 1, cut(text));
    }
}

/// Command `i`'s output, numbered: all of it up to `HEAD + TAIL` lines,
/// else its first `HEAD` and last `TAIL` with how many were left out.
pub fn output(t: &mut impl TerminalText, i: usize) -> String {
    let commands = t.commands();
    let bookmarks = t.bookmarks();
    let r = output_lines(t, &commands, i);
    let mut out = format!("--- output of line {} ---\n", commands[i].0 + 1);
    let count = r.end - r.start;
    if count == 0 {
        out.push_str("(no output)\n");
    } else if count <= (HEAD + TAIL) as u64 {
        push_numbered(&mut out, t, r, &bookmarks);
    } else {
        push_numbered(&mut out, t, r.start..r.start + HEAD as u64, &bookmarks);
        let _ = writeln!(out, "[{} lines left out]", count - (HEAD + TAIL) as u64);
        push_numbered(&mut out, t, r.end - TAIL as u64..r.end, &bookmarks);
    }
    out
}

/// Lines `from..=to` as shown (numbered from 1), at most `MAX_RANGE`.
pub fn range(t: &mut impl TerminalText, from: u64, to: u64) -> String {
    let start = from.saturating_sub(1).max(t.first_line());
    let end = to.min(t.end() + 1).min(start + MAX_RANGE);
    if start >= end {
        return "no such line".to_owned();
    }
    let bookmarks = t.bookmarks();
    let mut out = format!("--- lines {}-{} ---\n", start + 1, end);
    push_numbered(&mut out, t, start..end, &bookmarks);
    out
}

fn clock(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 3600 % 24, secs % 3600 / 60)
}

/// The first request's text: where the commands ran, the last `listed`
/// commands, the bookmarked lines, the output of the command to fix (the
/// one global line `target` belongs to, else the last) and always the last
/// command's. None when there is no command to fix.
pub fn first_message(t: &mut impl TerminalText, env: &Env, listed: usize, target: Option<u64>) -> Option<String> {
    let commands = t.commands();
    let last = commands.len().checked_sub(1)?;
    let fix = match target {
        Some(line) => command_of(&commands, line)?,
        None => last,
    };
    Some(listing(t, env, listed, Some((fix, "<- fix this one"))))
}

/// What a chat message carries, as a Fix's first request does: the
/// commands, the bookmarked lines and the last output, with the command
/// global line `about` belongs to marked and its output too.
#[allow(dead_code)] // used once the chat panel is wired in
pub fn chat_context(t: &mut impl TerminalText, env: &Env, listed: usize, about: Option<u64>) -> String {
    let commands = t.commands();
    if commands.is_empty() {
        return format!("{}\n\nNo commands yet.\n", env_line(env));
    }
    let mark = about.and_then(|line| command_of(&commands, line)).map(|i| (i, "<- the user asks about this one"));
    listing(t, env, listed, mark)
}

fn env_line(env: &Env) -> String {
    format!("Shell: {} · OS: {} · Program: {}", env.shell, env.os, env.program)
}

/// The env line, the last `listed` commands (command `mark.0` marked with
/// `mark.1`), the bookmarked lines, the marked command's output and the
/// last command's. The tab must have a command.
fn listing(t: &mut impl TerminalText, env: &Env, listed: usize, mark: Option<(usize, &str)>) -> String {
    let commands = t.commands();
    let last = commands.len() - 1;
    let bookmarks = t.bookmarks();
    let mut out = format!("{}\n\nCommands in this tab (newest last):\n", env_line(env));
    for i in commands.len().saturating_sub(listed.max(1))..commands.len() {
        let (line, secs) = commands[i];
        let text = t.lines(line..line + 1).into_iter().next().unwrap_or_default();
        let r = output_lines(t, &commands, i);
        let count = r.end - r.start;
        let _ = write!(out, "  line {} {}  {}  {count} line{}", line + 1, clock(secs), cut(&text), if count == 1 { "" } else { "s" });
        let marked: Vec<String> = bookmarks.iter().filter(|&&b| b >= line && b < r.end).map(|b| (b + 1).to_string()).collect();
        if !marked.is_empty() {
            let _ = write!(out, "  bookmarked: {}", marked.join(", "));
        }
        match mark {
            Some((m, text)) if m == i => {
                let _ = writeln!(out, "  {text}");
            }
            _ => out.push('\n'),
        }
    }
    let kept: Vec<u64> = bookmarks.iter().copied().filter(|&b| b >= t.first_line()).collect();
    if !kept.is_empty() {
        out.push_str("\nBookmarked lines:\n");
        for &b in &kept[kept.len().saturating_sub(MAX_BOOKMARKS)..] {
            let text = t.lines(b..b + 1).into_iter().next().unwrap_or_default();
            match command_of(&commands, b) {
                Some(c) => {
                    let _ = writeln!(out, "  {} (in line {}): {}", b + 1, commands[c].0 + 1, cut(&text));
                }
                None => {
                    let _ = writeln!(out, "  {}: {}", b + 1, cut(&text));
                }
            }
        }
    }
    if let Some((m, _)) = mark.filter(|&(m, _)| m != last) {
        out.push('\n');
        out.push_str(&output(t, m));
    }
    out.push('\n');
    out.push_str(&output(t, last));
    out
}

/// Runs one of the terminal tools; the text is the tool's result. Lines
/// in `input` are as shown (from 1). `added` gets the global lines of
/// bookmarks set.
pub fn run_tool(t: &mut impl TerminalText, name: &str, input: &Value, added: &mut Vec<u64>) -> String {
    let no_line = || "no such line".to_owned();
    // The line as a global index, if the tab has it (up to the prompt's).
    let line = input["line"].as_i64().filter(|&n| n >= 1).map(|n| n as u64 - 1).filter(|&l| l >= t.first_line() && l <= t.end());
    match name {
        "get_output" => {
            if let (Some(from), Some(to)) = (input["from"].as_u64(), input["to"].as_u64()) {
                return range(t, from, to);
            }
            let Some(line) = line else { return no_line() };
            match command_of(&t.commands(), line) {
                Some(i) => output(t, i),
                None => no_line(),
            }
        }
        "add_bookmark" => {
            let Some(line) = line else { return no_line() };
            if t.bookmarks().binary_search(&line).is_ok() {
                return format!("line {} is already bookmarked", line + 1);
            }
            t.add_bookmark(line);
            added.push(line);
            format!("bookmarked line {}", line + 1)
        }
        _ => format!("unknown tool {name}"),
    }
}

/// What is left of the terminal text one Fix may send.
pub struct Budget(usize);

impl Budget {
    pub fn new() -> Budget {
        Budget(BUDGET)
    }

    /// `text`, cut to at most `max` bytes and what is left, which it uses
    /// up; for the first request, which must go out whole or cut.
    pub fn take_up_to(&mut self, mut text: String, max: usize) -> String {
        const NOTE: &str = "\n[cut: limit reached]";
        let room = max.min(self.0);
        if text.len() > room {
            let mut end = room.saturating_sub(NOTE.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str(NOTE);
        }
        self.0 -= text.len();
        text
    }

    /// `text` if it fits in what is left (which it then uses up), else a
    /// note saying the limit is reached.
    pub fn take(&mut self, text: String) -> String {
        if text.len() <= self.0 {
            self.0 -= text.len();
            text
        } else {
            "limit reached".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A tab whose line i reads "l{i}", with the given commands and
    /// bookmarks, the prompt on line `end`, and lines before `first` gone.
    struct Fake {
        end: u64,
        commands: Vec<(u64, u64)>,
        marks: Vec<u64>,
        first: u64,
        reads: Vec<Range<u64>>,
    }

    impl TerminalText for Fake {
        fn lines(&mut self, r: Range<u64>) -> Vec<String> {
            self.reads.push(r.clone());
            (r.start.max(self.first)..r.end.min(self.end + 1)).map(|i| format!("l{i}")).collect()
        }
        fn end(&self) -> u64 {
            self.end
        }
        fn commands(&self) -> Vec<(u64, u64)> {
            self.commands.clone()
        }
        fn bookmarks(&self) -> Vec<u64> {
            self.marks.clone()
        }
        fn add_bookmark(&mut self, line: u64) {
            self.marks.push(line);
            self.marks.sort();
        }
        fn first_line(&self) -> u64 {
            self.first
        }
    }

    fn fake() -> Fake {
        Fake { end: 2000, commands: vec![(0, 3600), (1900, 7260), (1990, 7265)], marks: vec![5, 1995], first: 0, reads: vec![] }
    }

    fn env() -> Env {
        Env { shell: "bash".into(), os: "linux", program: "tar".into() }
    }

    #[test]
    fn first_message_lists_commands_and_sends_the_last_output() {
        let mut t = fake();
        let m = first_message(&mut t, &env(), 50, None).unwrap();
        assert!(m.starts_with("Shell: bash · OS: linux · Program: tar"));
        assert!(m.contains("  line 1 01:00  l0  1899 lines  bookmarked: 6"), "{m}");
        assert!(m.contains("  line 1991 02:01  l1990  9 lines  bookmarked: 1996  <- fix this one"), "{m}");
        assert!(m.contains("Bookmarked lines:\n  6 (in line 1): l5\n  1996 (in line 1991): l1995"), "{m}");
        assert!(m.contains("--- output of line 1991 ---\n1992 | l1991"), "{m}");
        assert!(m.contains("1996*| l1995"));
        assert!(!m.contains("--- output of line 1 ---"));
    }

    #[test]
    fn an_older_target_sends_its_output_too() {
        let mut t = fake();
        let m = first_message(&mut t, &env(), 50, Some(1950)).unwrap();
        assert!(m.contains("  line 1901 02:01  l1900  89 lines  <- fix this one"), "{m}");
        assert!(m.contains("--- output of line 1901 ---") && m.contains("--- output of line 1991 ---"));
        assert!(first_message(&mut Fake { commands: vec![], ..fake() }, &env(), 50, None).is_none());
    }

    #[test]
    fn chat_context_marks_the_command_asked_about() {
        let mut t = fake();
        let m = chat_context(&mut t, &env(), 50, Some(5));
        assert!(m.contains("  line 1 01:00  l0  1899 lines  bookmarked: 6  <- the user asks about this one"), "{m}");
        assert!(m.contains("--- output of line 1 ---") && m.contains("--- output of line 1991 ---"), "{m}");
        let plain = chat_context(&mut fake(), &env(), 50, None);
        assert!(!plain.contains("<-") && plain.contains("--- output of line 1991 ---"), "{plain}");
        let empty = chat_context(&mut Fake { commands: vec![], ..fake() }, &env(), 50, None);
        assert_eq!(empty, "Shell: bash · OS: linux · Program: tar\n\nNo commands yet.\n");
    }

    #[test]
    fn only_the_last_commands_are_listed() {
        let mut t = Fake { commands: (0..100).map(|i| (i * 10, 0)).collect(), marks: vec![], end: 1000, ..fake() };
        let m = first_message(&mut t, &env(), 3, None).unwrap();
        let rows: Vec<&str> = m.lines().filter(|l| l.starts_with("  line ")).collect();
        assert_eq!(rows.len(), 3, "{m}");
        assert!(rows[0].starts_with("  line 971 "));
    }

    #[test]
    fn long_output_comes_as_head_and_tail() {
        let mut t = fake();
        let o = output(&mut t, 0);
        assert!(o.contains("51 | l50") && !o.contains("52 | l51"), "head ends at line 51");
        assert!(o.contains("[1699 lines left out]") && o.contains("1900 | l1899"), "{o}");
        assert!(t.reads.iter().map(|r| r.end - r.start).sum::<u64>() <= 201);
    }

    #[test]
    fn ranges_are_capped() {
        let mut t = fake();
        let r = range(&mut t, 1, 1_000_000_000);
        assert_eq!(r.lines().filter(|l| l.contains(" | ") || l.contains("*| ")).count(), MAX_RANGE as usize);
        assert_eq!(range(&mut t, 5000, 6000), "no such line");
    }

    #[test]
    fn tools_refuse_lines_the_tab_lacks() {
        let mut t = Fake { first: 100, ..fake() };
        let mut added = vec![];
        for bad in [json!({"line": 0}), json!({"line": -3}), json!({"line": 5000}), json!({"line": 50})] {
            assert_eq!(run_tool(&mut t, "add_bookmark", &bad, &mut added), "no such line", "{bad}");
        }
        assert_eq!(run_tool(&mut t, "get_output", &json!({"line": "x"}), &mut added), "no such line");
        assert_eq!(run_tool(&mut t, "add_bookmark", &json!({"line": 1993}), &mut added), "bookmarked line 1993");
        assert_eq!(added, vec![1992]);
        assert_eq!(run_tool(&mut t, "add_bookmark", &json!({"line": 1996}), &mut added), "line 1996 is already bookmarked");
        assert!(run_tool(&mut t, "get_output", &json!({"line": 1950}), &mut added).starts_with("--- output of line 1901 ---"));
        assert!(run_tool(&mut t, "get_output", &json!({"line": 1, "from": 1950, "to": 1952}), &mut added).contains("1952 | l1951"));
        assert_eq!(run_tool(&mut t, "rm", &json!({}), &mut added), "unknown tool rm");
    }

    #[test]
    fn a_long_first_message_is_cut_to_fit() {
        let mut b = Budget::new();
        let t = b.take_up_to("é".repeat(BUDGET), FIRST_MAX);
        assert!(t.len() <= FIRST_MAX && t.ends_with("[cut: limit reached]"), "{}", t.len());
        assert_eq!(b.0, BUDGET - t.len());
        let short = b.take_up_to("ok".into(), FIRST_MAX);
        assert_eq!(short, "ok");
    }

    #[test]
    fn the_budget_stops_at_its_limit() {
        let mut b = Budget::new();
        assert_eq!(b.take("x".repeat(BUDGET - 10)).len(), BUDGET - 10);
        assert_eq!(b.take("y".repeat(20)), "limit reached");
        assert_eq!(b.take("z".repeat(10)).len(), 10);
    }
}
