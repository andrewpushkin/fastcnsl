//! The text a Fix sends: the last few commands and what they printed.

use std::ops::Range;

/// Output lines kept from the start of a long command's output.
pub const HEAD: usize = 50;
/// Output lines kept from its end, where errors usually are.
pub const TAIL: usize = 150;
/// The most text sent, in bytes.
pub const MAX_CHARS: usize = 30_000;
/// Longer lines (minified code, base64) are cut to this many characters.
const MAX_LINE: usize = 1000;

/// One command: the line it was typed on (with the prompt) and its output.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandText {
    pub line: String,
    /// The first [`HEAD`] lines then the last [`TAIL`], or all of them.
    pub output: Vec<String>,
    /// Output lines not read, between the head and the tail.
    pub left_out: usize,
}

/// Where the commands ran.
pub struct Env {
    pub shell: String,
    pub os: &'static str,
    /// The last program started in the tab.
    pub program: String,
}

/// Reads command `range` (its line, then its output) with `read`, which
/// returns the text of a range of global lines. Long output is read only
/// at its ends, so a command that printed millions of lines costs no more
/// than one that printed two hundred.
pub fn read_command(range: Range<u64>, mut read: impl FnMut(Range<u64>) -> Vec<String>) -> CommandText {
    let output_lines = (range.end - range.start).saturating_sub(1) as usize;
    if output_lines <= HEAD + TAIL {
        let mut all = read(range).into_iter();
        let line = all.next().unwrap_or_default();
        return CommandText { line, output: all.collect(), left_out: 0 };
    }
    let mut head = read(range.start..range.start + 1 + HEAD as u64).into_iter();
    let line = head.next().unwrap_or_default();
    let mut output: Vec<String> = head.collect();
    output.extend(read(range.end - TAIL as u64..range.end));
    CommandText { line, output, left_out: output_lines - HEAD - TAIL }
}

/// The text sent: where the commands ran, then each command and its
/// output. Over [`MAX_CHARS`], the oldest commands go first; the last one
/// always stays, its output cut from the middle if need be.
pub fn build(env: &Env, commands: &[CommandText]) -> String {
    let header = format!("Shell: {} · OS: {} · Program: {}\n", env.shell, env.os, env.program);
    let commands: Vec<CommandText> = commands.iter().map(shorten_lines).collect();
    let mut first = 0;
    loop {
        let shown = &commands[first..];
        let text = render(&header, shown);
        if text.len() <= MAX_CHARS {
            return text;
        }
        if shown.len() <= 1 {
            break;
        }
        first += 1;
    }
    let Some(last) = commands.last() else { return header };
    // The one command left is too long: drop output from the middle.
    // Cut where lines were already left out, else in the middle.
    let split = if last.left_out > 0 { HEAD.min(last.output.len()) } else { last.output.len() / 2 };
    let mut head = last.output[..split].to_vec();
    let mut tail: std::collections::VecDeque<String> = last.output[split..].iter().cloned().collect();
    let mut left_out = last.left_out;
    let mut size: usize = render(&header, std::slice::from_ref(last)).len();
    // Room for the marker line growing a digit or two.
    while size > MAX_CHARS - 40 && !(head.is_empty() && tail.is_empty()) {
        let gone = if head.len() > tail.len() { head.pop() } else { tail.pop_front() };
        size -= gone.map_or(0, |l| l.len() + 1);
        left_out += 1;
    }
    let mut output = head;
    let split = output.len();
    output.extend(tail);
    let cut = CommandText { line: last.line.clone(), output, left_out };
    let mut text = render_with_split(&header, &cut, split);
    // A very long command line alone: cut at a character boundary.
    if text.len() > MAX_CHARS {
        let mut end = MAX_CHARS;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

fn shorten_lines(c: &CommandText) -> CommandText {
    let cut = |l: &String| if l.chars().count() > MAX_LINE { format!("{}…", l.chars().take(MAX_LINE).collect::<String>()) } else { l.clone() };
    CommandText { line: cut(&c.line), output: c.output.iter().map(cut).collect(), left_out: c.left_out }
}

fn render(header: &str, commands: &[CommandText]) -> String {
    let n = commands.len();
    let mut text = header.to_owned();
    for (i, c) in commands.iter().enumerate() {
        let split = if c.left_out > 0 { HEAD.min(c.output.len()) } else { c.output.len() };
        push_command(&mut text, c, i + 1, n, split);
    }
    text
}

fn render_with_split(header: &str, c: &CommandText, split: usize) -> String {
    let mut text = header.to_owned();
    push_command(&mut text, c, 1, 1, split);
    text
}

/// Appends command `i` of `n`, with the "left out" line after output line
/// `split`.
fn push_command(text: &mut String, c: &CommandText, i: usize, n: usize, split: usize) {
    use std::fmt::Write;
    let _ = write!(text, "\n--- command {i} of {n} ---\n{}\n", c.line);
    for (k, l) in c.output.iter().enumerate() {
        if k == split && c.left_out > 0 {
            let _ = writeln!(text, "[{} lines left out]", c.left_out);
        }
        text.push_str(l);
        text.push('\n');
    }
    if split >= c.output.len() && c.left_out > 0 {
        let _ = writeln!(text, "[{} lines left out]", c.left_out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_output_reads_only_head_and_tail() {
        let mut asked = Vec::new();
        let c = read_command(100..1_000_101, |r| {
            asked.push(r.clone());
            r.map(|i| format!("l{i}")).collect()
        });
        assert_eq!(c.line, "l100");
        assert_eq!(c.output.len(), HEAD + TAIL);
        assert_eq!(c.output[HEAD - 1], format!("l{}", 100 + HEAD));
        assert_eq!(c.output[HEAD], format!("l{}", 1_000_101 - TAIL as u64));
        assert_eq!(c.left_out, 1_000_000 - HEAD - TAIL);
        assert!(asked.iter().map(|r| r.end - r.start).sum::<u64>() <= 201, "{asked:?}");
    }

    #[test]
    fn short_output_is_read_whole() {
        let c = read_command(0..4, |r| r.map(|i| format!("l{i}")).collect());
        assert_eq!((c.line.as_str(), c.output.len(), c.left_out), ("l0", 3, 0));
    }

    #[test]
    fn build_keeps_the_last_command_and_the_cap() {
        let env = Env { shell: "bash".into(), os: "linux", program: "tar".into() };
        let big = |s: &str| CommandText { line: format!("$ {s}"), output: vec!["x".repeat(200); 120], left_out: 0 };
        let text = build(&env, &[big("one"), big("two"), big("tar -xf a.tgz -C")]);
        assert!(text.len() <= MAX_CHARS);
        assert!(text.starts_with("Shell: bash · OS: linux · Program: tar"));
        assert!(text.contains("$ tar -xf a.tgz -C"));
        assert!(!text.contains("$ one"));
        let short = build(&env, &[CommandText { line: "$ ls".into(), output: vec!["a".into()], left_out: 7 }]);
        assert!(short.contains("--- command 1 of 1 ---\n$ ls\na\n[7 lines left out]"), "{short}");
    }

    #[test]
    fn one_giant_command_is_cut_from_the_middle() {
        let env = Env { shell: "sh".into(), os: "linux", program: "x".into() };
        let mut output = vec!["first".to_owned()];
        output.extend(std::iter::repeat_n("y".repeat(1000), 100));
        output.push("last".into());
        let text = build(&env, &[CommandText { line: "$ x".into(), output, left_out: 0 }]);
        assert!(text.len() <= MAX_CHARS);
        assert!(text.contains("first") && text.contains("last") && text.contains("lines left out]"), "{}", &text[..200]);
    }
}
