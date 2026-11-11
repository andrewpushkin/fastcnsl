//! Tests for the vendored vte's runs of printable ASCII and SIMD UTF-8
//! validation, and the vendored terminal's `input_ascii` (fastcnsl
//! patches). They live here because a patched dependency's own tests don't
//! run.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Handler, Processor};
use alacritty_terminal::vte::{Parser, Perform};

use crate::testterm::Quiet;

/// xorshift64*: deterministic input without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn ascii(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| (0x20 + self.below(0x5f) as u8) as char)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Print(char),
    Execute(u8),
}

/// Records what the parser does in ground state, a run as its characters.
#[derive(Default)]
struct Recorder {
    events: Vec<Ev>,
    runs: usize,
}

impl Perform for Recorder {
    fn print(&mut self, c: char) {
        self.events.push(Ev::Print(c));
    }

    fn print_ascii(&mut self, s: &str) {
        assert!(!s.is_empty());
        assert!(
            s.bytes().all(|b| (0x20..0x7f).contains(&b)),
            "not printable ASCII: {s:?}"
        );
        self.runs += 1;
        self.events.extend(s.chars().map(Ev::Print));
    }

    fn execute(&mut self, byte: u8) {
        self.events.push(Ev::Execute(byte));
    }
}

/// What unpatched vte does with `bytes` (no ESC) given in one piece: each
/// character printed or executed, invalid sequences executed (a lone C1
/// byte) or shown as U+FFFD, an incomplete sequence at the end kept back.
fn reference(mut bytes: &[u8]) -> Vec<Ev> {
    fn chars(s: &str, out: &mut Vec<Ev>) {
        for c in s.chars() {
            out.push(match c {
                '\x00'..='\x1f' | '\u{80}'..='\u{9f}' => Ev::Execute(c as u8),
                _ => Ev::Print(c),
            });
        }
    }
    let mut out = Vec::new();
    loop {
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                chars(s, &mut out);
                return out;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                chars(std::str::from_utf8(&bytes[..valid]).unwrap(), &mut out);
                let Some(len) = e.error_len() else { return out };
                out.push(if len == 1 && bytes[valid] <= 0x9f {
                    Ev::Execute(bytes[valid])
                } else {
                    Ev::Print('�')
                });
                bytes = &bytes[valid + len..];
            }
        }
    }
}

/// Plain text (no ESC) mixing every kind of byte the ground state sees.
fn plain_text(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..rng.below(12) {
        match rng.below(9) {
            0..=2 => {
                let len = rng.below(150);
                out.extend(rng.ascii(len).bytes());
            }
            3 => out.extend_from_slice(
                ["\r\n", "\t", "\x07", "\x08", "\x7f", "\0"][rng.below(6)].as_bytes(),
            ),
            4 => out.extend_from_slice(
                ["é", "漢", "😀", "─", "\u{85}", "\u{9f}", "\u{301}"][rng.below(7)].as_bytes(),
            ),
            5 => out.extend_from_slice(
                [
                    [0xff].as_slice(),
                    &[0x80],
                    &[0x9b],
                    &[0xc2],
                    &[0xe6, 0xbc],
                    &[0xf0, 0x9f, 0x98],
                ][rng.below(6)],
            ),
            _ => out.extend("абв漢字 ".repeat(rng.below(20)).bytes()),
        }
    }
    out
}

#[test]
fn ground_state_matches_unpatched_vte() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut runs = 0;
    for _ in 0..5000 {
        let bytes = plain_text(&mut rng);
        let mut rec = Recorder::default();
        Parser::new().advance(&mut rec, &bytes);
        assert_eq!(
            rec.events,
            reference(&bytes),
            "input {:?}",
            String::from_utf8_lossy(&bytes)
        );
        runs += rec.runs;
    }
    assert!(runs > 1000, "printable ASCII went to print_ascii");
}

#[test]
fn ascii_runs_stop_at_controls_and_escapes() {
    let mut rec = Recorder::default();
    Parser::new().advance(&mut rec, b"ab\x1b[31mcd\r\nef\x7fg");
    assert_eq!(rec.runs, 4, "ab, cd, ef and g; DEL is printed on its own");
    let printed: String = rec
        .events
        .iter()
        .filter_map(|e| if let Ev::Print(c) = e { Some(*c) } else { None })
        .collect();
    assert_eq!(printed, "abcdef\x7fg");
}

/// Two terminals: `fast` gets runs of ASCII through `input_ascii`, `slow`
/// one character at a time through `input`; everything else goes to both.
struct Pair {
    fast: Term<Quiet>,
    slow: Term<Quiet>,
    parser: [Processor; 2],
}

impl Pair {
    fn new(cols: usize, lines: usize) -> Self {
        let term = || {
            Term::new(
                TermConfig {
                    scrolling_history: 10,
                    ..TermConfig::default()
                },
                &TermSize::new(cols, lines),
                Quiet,
            )
        };
        Self {
            fast: term(),
            slow: term(),
            parser: [Processor::new(), Processor::new()],
        }
    }

    fn ascii(&mut self, s: &str) {
        self.fast.input_ascii(s);
        for c in s.chars() {
            self.slow.input(c);
        }
    }

    fn input(&mut self, c: char) {
        self.fast.input(c);
        self.slow.input(c);
    }

    /// Escape sequences and controls, through the parser.
    fn bytes(&mut self, b: &str) {
        self.parser[0].advance(&mut self.fast, b.as_bytes());
        self.parser[1].advance(&mut self.slow, b.as_bytes());
    }

    fn assert_same(&self, step: &str) {
        let (f, s) = (self.fast.grid(), self.slow.grid());
        // Row by row: comparing grids needs storage that never rotated.
        assert_eq!(f.history_size(), s.history_size(), "history after {step}");
        assert_eq!(f.cursor, s.cursor, "cursor differs after {step}");
        for l in -(f.history_size() as i32)..f.screen_lines() as i32 {
            assert!(
                f[Line(l)] == s[Line(l)],
                "cells of line {l} differ after {step}"
            );
            assert_eq!(
                f[Line(l)].occupied(),
                s[Line(l)].occupied(),
                "occupied cells of line {l} after {step}"
            );
        }
    }
}

#[test]
fn input_ascii_matches_input_per_character() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for round in 0..300 {
        // At least 2 columns: in 1, a wide character panics in `input` itself.
        let (cols, lines) = (2 + rng.below(30), 1 + rng.below(6));
        let mut t = Pair::new(cols, lines);
        for step in 0..60 {
            let what = match rng.below(14) {
                0..=4 => {
                    let len = 1 + rng.below(3 * cols);
                    let s = rng.ascii(len);
                    t.ascii(&s);
                    format!("ascii {s:?}")
                }
                5 => {
                    let c = ['漢', '😀', 'é', '\u{301}'][rng.below(4)];
                    t.input(c);
                    format!("input {c:?}")
                }
                6 => {
                    let at = format!("\x1b[{};{}H", 1 + rng.below(lines), 1 + rng.below(cols));
                    t.bytes(&at);
                    format!("goto {at:?}")
                }
                _ => {
                    let seq = [
                        "\r\n",
                        "\r",
                        "\n",
                        "\x08",
                        "\t",
                        "\x1b[4h",
                        "\x1b[4l", // insert mode on, off
                        "\x1b[?7l",
                        "\x1b[?7h", // line wrap off, on
                        "\x1b(0",
                        "\x1b(B", // line drawing, ASCII
                        "\x1b[1;31;44m",
                        "\x1b[0m",               // attributes, reset
                        "\x1b[4:3m\x1b[58;5;3m", // underline colour (cell extra)
                        "\x1b]8;id=t;https://example.com\x1b\\",
                        "\x1b]8;;\x1b\\", // hyperlink on, off
                        "\x1b[2K",
                        "\x1b[3@",
                        "\x1b[2P", // erase line, insert, delete
                    ][rng.below(19)];
                    t.bytes(seq);
                    format!("{seq:?}")
                }
            };
            t.assert_same(&format!("round {round} step {step}: {what}"));
        }
    }
}
