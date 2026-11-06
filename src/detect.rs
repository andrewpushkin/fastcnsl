//! Reads command lines the user enters:
//!
//! * which program they start, for tab labels (`Tab 1: vim`);
//! * whether they print source code, and in which language: `cat hello.c` →
//!   C, `head -n 20 main.rs` → Rust, `git diff` → diff. When the file name
//!   doesn't say, the first lines of output are sniffed.

/// What we learned about the language from the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hint {
    /// The file being printed, e.g. `src/main.rs` or `Makefile`.
    File(String),
    /// Output is a unified diff.
    Diff,
    /// The command prints a file but we could not see its name (e.g. `cat < x`).
    Unknown,
}

/// Commands that print a file to stdout without changing it.
const VIEWERS: &[&str] = &[
    "cat", "bat", "batcat", "head", "tail", "tac", "type", "gc", "get-content", "more",
];

/// Inspects the text of the line the user just pressed Enter on, prompt
/// included, and returns a hint if the command will print code.
pub fn command_hint(line: &str) -> Option<Hint> {
    let tokens = tokenize(line);
    for (i, tok) in tokens.iter().enumerate() {
        let name = base_name(tok);
        if VIEWERS.contains(&name.as_str()) {
            let file = tokens[i + 1..]
                .iter()
                .take_while(|t| !is_separator(t))
                .filter(|t| !t.starts_with('-') && !t.chars().all(|c| c.is_ascii_digit()))
                .last();
            return Some(match file {
                Some(f) if looks_like_diff_file(f) => Hint::Diff,
                Some(f) => Hint::File(f.clone()),
                None => Hint::Unknown,
            });
        }
        if name == "diff" || name == "colordiff" {
            return Some(Hint::Diff);
        }
        if name == "git" {
            let sub = tokens.get(i + 1).map(String::as_str);
            if matches!(sub, Some("diff" | "show" | "format-patch")) {
                return Some(Hint::Diff);
            }
        }
    }
    None
}

fn looks_like_diff_file(f: &str) -> bool {
    let f = f.to_ascii_lowercase();
    f.ends_with(".diff") || f.ends_with(".patch")
}

fn is_separator(t: &str) -> bool {
    matches!(t, "|" | "||" | "&&" | ";" | ">" | ">>" | "2>" | "&")
}

/// Commands that run another program; the tab shows that program.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "time", "nohup", "env", "exec", "command", "builtin", "nice", "ionice", "stdbuf", "caffeinate", "noglob",
];

/// Shell builtins that change directory or environment rather than run an
/// application; they leave the tab label alone.
const NOT_APPS: &[&str] = &[
    "cd", "pushd", "popd", "dirs", "export", "alias", "unalias", "unset", "set", "setopt", "source", ".",
    "history", "exit", "logout", "z", "set-location", "sl", "chdir",
];

/// Prompt endings, for finding where the typed command starts.
const PROMPT_ENDS: &[&str] = &["$ ", "# ", "% ", "> ", "❯ ", "λ ", "» "];

/// Removes the shell prompt from the line the user pressed Enter on.
fn strip_prompt(line: &str) -> &str {
    // cmd.exe: "C:\Users\me>dir" has no space after '>'.
    let b = line.as_bytes();
    if b.len() > 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'\\'
        && let Some(i) = line.find('>')
    {
        return &line[i + 1..];
    }
    PROMPT_ENDS
        .iter()
        .filter_map(|e| line.find(e).map(|i| i + e.len()))
        .min()
        .map_or(line, |i| &line[i..])
}

/// The program a command line runs, for the tab label: `vim` for
/// `user@host:~$ sudo vim /etc/hosts`. None for an empty line, a builtin
/// like `cd`, or text that doesn't look like a shell command (e.g. typed
/// into a REPL).
pub fn program_name(line: &str) -> Option<String> {
    let tokens = tokenize(strip_prompt(line.trim_end()));
    let mut after_wrapper = false;
    for (i, tok) in tokens.iter().enumerate() {
        if is_separator(tok) {
            return None;
        }
        // VAR=value prefixes, wrapper flags and numbers (nice -n 10).
        let assignment = tok.find('=').is_some_and(|eq| eq > 0 && tok[..eq].chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if assignment || (after_wrapper && (tok.starts_with('-') || tok.chars().all(|c| c.is_ascii_digit()))) {
            continue;
        }
        let base = tok.rsplit(['/', '\\']).next().unwrap_or(tok);
        let name = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(base);
        if WRAPPERS.contains(&name.to_ascii_lowercase().as_str()) {
            after_wrapper = true;
            continue;
        }
        let looks_like_program = !name.is_empty()
            && !name.starts_with('-')
            && name.chars().all(|c| c.is_alphanumeric() || "._+-".contains(c));
        let next = tokens.get(i + 1).map(String::as_str);
        let repl_syntax = matches!(next, Some(n) if n == "=" || n.starts_with('(') || n.starts_with("+=") || n.starts_with("=="));
        let builtin = NOT_APPS.contains(&name.to_ascii_lowercase().as_str());
        return (looks_like_program && !repl_syntax && !builtin).then(|| name.to_owned());
    }
    None
}

/// Lower-cased base name without directory or `.exe`.
fn base_name(tok: &str) -> String {
    let base = tok.rsplit(['/', '\\']).next().unwrap_or(tok).to_ascii_lowercase();
    match base.strip_suffix(".exe") {
        Some(b) => b.to_owned(),
        None => base,
    }
}

/// Whitespace split that keeps quoted strings together and splits off
/// shell operators glued to words (`cat a.c|less`).
fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let push = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None => match c {
                '"' | '\'' => quote = Some(c),
                c if c.is_whitespace() => push(&mut cur, &mut out),
                '|' | ';' | '&' => {
                    push(&mut cur, &mut out);
                    out.push(c.to_string());
                }
                _ => cur.push(c),
            },
        }
    }
    push(&mut cur, &mut out);
    out
}

/// Guesses a language from the first lines of output. Returns a file
/// extension that syntect knows. Used when the file name had no usable
/// extension (`cat Dockerfile.old`, `cat script`).
pub fn sniff(lines: &[&str]) -> Option<&'static str> {
    let first = lines.iter().map(|l| l.trim()).find(|l| !l.is_empty())?;
    if let Some(rest) = first.strip_prefix("#!") {
        let interp = rest.split_whitespace().collect::<Vec<_>>();
        let prog = match interp.as_slice() {
            [env, prog, ..] if env.ends_with("/env") => *prog,
            [prog, ..] => prog.rsplit('/').next().unwrap_or(prog),
            [] => return None,
        };
        return Some(match prog {
            p if p.starts_with("python") => "py",
            "bash" | "sh" | "zsh" | "dash" | "ksh" => "sh",
            "node" | "deno" | "bun" => "js",
            "ruby" => "rb",
            "perl" => "pl",
            "php" => "php",
            "lua" => "lua",
            _ => return None,
        });
    }
    if first.starts_with("<?xml") || first.starts_with("<svg") {
        return Some("xml");
    }
    if first.to_ascii_lowercase().starts_with("<!doctype html") || first.starts_with("<html") {
        return Some("html");
    }
    if first.starts_with("diff --git") || first.starts_with("--- ") && lines.len() > 1 {
        return Some("diff");
    }
    if (first.starts_with('{') || first.starts_with('['))
        && lines.iter().take(20).any(|l| l.trim_start().starts_with('"'))
    {
        return Some("json");
    }

    // Keyword scoring over the first screenful.
    let mut scores: [(&'static str, i32); 7] =
        [("c", 0), ("cpp", 0), ("rs", 0), ("py", 0), ("go", 0), ("js", 0), ("java", 0)];
    let mut add = |ext: &str, n: i32| {
        if let Some(s) = scores.iter_mut().find(|(e, _)| *e == ext) {
            s.1 += n;
        }
    };
    for l in lines.iter().take(60) {
        let t = l.trim_start();
        if t.starts_with("#include") {
            add("c", 3);
            add("cpp", 2);
        }
        if t.starts_with("int main(") || t.contains("printf(") || t.contains("malloc(") {
            add("c", 2);
        }
        if t.contains("std::") || t.starts_with("template<") || t.starts_with("class ") && t.ends_with('{') || t.starts_with("namespace ") {
            add("cpp", 3);
        }
        if t.starts_with("fn ") || t.starts_with("pub fn ") || t.starts_with("impl ") || t.starts_with("use ") && t.contains("::") || t.contains("let mut ") {
            add("rs", 3);
        }
        if t.starts_with("def ") || t.starts_with("import ") && !t.ends_with(';') || t.starts_with("from ") && t.contains(" import ") || t.contains("self.") && !t.ends_with(';') {
            add("py", 3);
        }
        if t.starts_with("package ") && !t.ends_with(';') || t.starts_with("func ") || t.contains(":= ") {
            add("go", 3);
        }
        if t.starts_with("const ") && t.contains(" = ") && t.ends_with(';') || t.starts_with("function ") || t.contains("=> {") || t.starts_with("export ") {
            add("js", 2);
        }
        if t.starts_with("public class ") || t.starts_with("package ") && t.ends_with(';') || t.contains("System.out.") {
            add("java", 3);
        }
    }
    scores
        .iter()
        .filter(|(_, s)| *s >= 3)
        .max_by_key(|(_, s)| *s)
        .map(|(e, _)| *e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(s: &str) -> Option<Hint> {
        Some(Hint::File(s.to_owned()))
    }

    #[test]
    fn cat_with_prompt() {
        assert_eq!(command_hint("andrei@box:~/src$ cat hello.c"), file("hello.c"));
        assert_eq!(command_hint("root@box:/# cat /etc/nginx/nginx.conf"), file("/etc/nginx/nginx.conf"));
        assert_eq!(command_hint("❯ cat src/main.rs"), file("src/main.rs"));
        assert_eq!(command_hint("PS C:\\code> Get-Content .\\app.py"), file(".\\app.py"));
        assert_eq!(command_hint("C:\\code> type main.go"), file("main.go"));
    }

    #[test]
    fn flags_and_counts_are_skipped() {
        assert_eq!(command_hint("$ head -n 20 lib.rs"), file("lib.rs"));
        assert_eq!(command_hint("$ tail -f -n 100 server.log"), file("server.log"));
        assert_eq!(command_hint("$ /usr/bin/cat -A 'my file.py'"), file("my file.py"));
    }

    #[test]
    fn pipes_stop_the_argument_list() {
        assert_eq!(command_hint("$ cat hello.c | grep main"), file("hello.c"));
        assert_eq!(command_hint("$ cat hello.c|less"), file("hello.c"));
    }

    #[test]
    fn diffs() {
        assert_eq!(command_hint("$ git diff HEAD~1"), Some(Hint::Diff));
        assert_eq!(command_hint("$ diff -u a b"), Some(Hint::Diff));
        assert_eq!(command_hint("$ cat fix.patch"), Some(Hint::Diff));
    }

    #[test]
    fn other_commands_are_ignored() {
        assert_eq!(command_hint("$ ls -la"), None);
        assert_eq!(command_hint("cat@box:~$ make"), None);
        assert_eq!(command_hint("$ cat"), Some(Hint::Unknown));
    }

    fn prog(line: &str) -> Option<String> {
        program_name(line)
    }

    #[test]
    fn program_names_for_tab_labels() {
        assert_eq!(prog("andrei@box:~/src$ cat hello.c").as_deref(), Some("cat"));
        assert_eq!(prog("root@box:/# apt update").as_deref(), Some("apt"));
        assert_eq!(prog("user@mac ~ % ls -la").as_deref(), Some("ls"));
        assert_eq!(prog("~/src> git status").as_deref(), Some("git"));
        assert_eq!(prog("❯ cargo build --release").as_deref(), Some("cargo"));
        assert_eq!(prog("PS C:\\code> Get-ChildItem").as_deref(), Some("Get-ChildItem"));
        assert_eq!(prog("C:\\Users\\me>notepad.exe file.txt").as_deref(), Some("notepad"));
        assert_eq!(prog("$ /usr/bin/python3 script.py").as_deref(), Some("python3"));
        assert_eq!(prog("$ ./build.sh").as_deref(), Some("build.sh"));
    }

    #[test]
    fn wrappers_and_assignments_are_skipped() {
        assert_eq!(prog("$ sudo vim /etc/hosts").as_deref(), Some("vim"));
        assert_eq!(prog("$ RUST_LOG=debug cargo run").as_deref(), Some("cargo"));
        assert_eq!(prog("$ nice -n 10 make -j8").as_deref(), Some("make"));
        assert_eq!(prog("$ time env FOO=1 htop").as_deref(), Some("htop"));
    }

    #[test]
    fn non_commands_give_no_name() {
        assert_eq!(prog("dev@box:~$ "), None);
        assert_eq!(prog(">>> x = 5"), None);
        assert_eq!(prog(">>> print(1)"), None);
        assert_eq!(prog("$ | grep x"), None);
        assert_eq!(prog("$ cd /tmp"), None);
        assert_eq!(prog("$ export FOO=1"), None);
        assert_eq!(prog("PS C:\\> Set-Location D:\\"), None);
    }

    #[test]
    fn every_viewer_is_recognised() {
        for v in ["cat", "bat", "batcat", "head", "tail", "tac", "type", "gc", "Get-Content", "more"] {
            assert_eq!(command_hint(&format!("$ {v} x.rs")), file("x.rs"), "{v}");
        }
        assert_eq!(command_hint("$ git show HEAD"), Some(Hint::Diff));
        assert_eq!(command_hint("$ git log"), None);
        assert_eq!(command_hint("$ CAT.EXE a.py"), file("a.py"));
    }

    #[test]
    fn more_sniffing() {
        assert_eq!(sniff(&["<?xml version=\"1.0\"?>"]), Some("xml"));
        assert_eq!(sniff(&["<!DOCTYPE html>", "<html>"]), Some("html"));
        assert_eq!(sniff(&["diff --git a/x b/x"]), Some("diff"));
        assert_eq!(sniff(&["package main", "", "func main() {", "  x := 1", "}"]), Some("go"));
        assert_eq!(sniff(&["#!/usr/bin/env node"]), Some("js"));
        assert_eq!(sniff(&["#!/usr/bin/ruby"]), Some("rb"));
        assert_eq!(sniff(&["#!/usr/bin/whatever"]), None);
        assert_eq!(sniff(&["", "   ", ""]), None);
        assert_eq!(sniff(&[]), None);
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(&["#!/usr/bin/env python3", "print(1)"]), Some("py"));
        assert_eq!(sniff(&["#!/bin/bash"]), Some("sh"));
        assert_eq!(sniff(&["#include <stdio.h>", "", "int main(void) {", "  printf(\"hi\");", "}"]), Some("c"));
        assert_eq!(sniff(&["use std::io;", "", "fn main() {", "    let mut x = 1;", "}"]), Some("rs"));
        assert_eq!(sniff(&["{", "  \"a\": 1", "}"]), Some("json"));
        assert_eq!(sniff(&["hello world", "this is prose"]), None);
    }
}
