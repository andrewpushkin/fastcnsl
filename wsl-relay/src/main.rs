//! fastcnsl-wsl: runs a shell on a Linux pty inside WSL and relays it over
//! stdin/stdout.
//!
//! fastcnsl on Windows starts `wsl.exe ... -e fastcnsl-wsl ...` with
//! plain pipes instead of a ConPTY pseudoconsole. The output then never goes
//! through the Windows console host, which is the slow part: `cat` of a
//! 36 MB file takes 0.42 s through ConPTY and 0.07 s through a pipe.
//!
//! ```text
//! fastcnsl-wsl COLS ROWS login                 the user's login shell
//! fastcnsl-wsl COLS ROWS shell COMMAND-LINE    $SHELL -c COMMAND-LINE
//! fastcnsl-wsl COLS ROWS exec PROGRAM ARGS...  PROGRAM directly
//! ```
//!
//! stdout carries the pty output unchanged. stdin carries the input for the
//! pty, where 0xFF (a byte UTF-8 never uses) starts a control message:
//!
//! ```text
//! 0xFF 'R' cols rows width height    resize; u16 little-endian each,
//!                                    width/height in pixels
//! ```
//!
//! The shell gets SIGHUP when stdin closes (the tab was closed), and the
//! relay exits with the shell's status once the pty closes.

use std::ffi::{CStr, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio, exit};
use std::ptr;

/// Control-message lead byte on stdin.
const CONTROL: u8 = 0xFF;
const RESIZE: u8 = b'R';
const RESIZE_LEN: usize = 8;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let usage = || -> ! {
        eprintln!("usage: fastcnsl-wsl COLS ROWS login | shell COMMAND-LINE | exec PROGRAM ARGS...");
        exit(2)
    };
    if args.len() < 3 {
        usage();
    }
    let num = |i: usize| args[i].to_str().and_then(|s| s.parse::<u16>().ok()).unwrap_or_else(|| usage());
    let size = winsize(num(0), num(1), 0, 0);
    let mut command = match (args[2].as_bytes(), &args[3..]) {
        (b"login", []) => {
            // A login shell's argv[0] is "-bash"; the program is the shell's path.
            let shell = user_shell();
            let mut command = Command::new(&shell);
            command.arg0(login_argv0(&shell));
            command
        }
        (b"shell", [line]) => {
            let mut command = Command::new(user_shell());
            command.arg("-c").arg(line);
            command
        }
        (b"exec", [program, rest @ ..]) => {
            let mut command = Command::new(program);
            command.args(rest);
            command
        }
        _ => usage(),
    };
    let program = command.get_program().to_owned();

    let (master, mut child) = match spawn(&mut command, &size) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fastcnsl-wsl: {}: {e}", program.to_string_lossy());
            exit(127)
        }
    };
    // Its copies of the pty's slave side would keep the pty open after
    // the shell exits.
    drop(command);
    let pid = child.id() as libc::pid_t;
    let (mut stdin, mut stdout, mut pty_in) = match (dup(io::stdin().as_fd()), dup(io::stdout().as_fd()), master.try_clone()) {
        (Ok(i), Ok(o), Ok(p)) => (i, o, p),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
            eprintln!("fastcnsl-wsl: {e}");
            exit(1)
        }
    };

    // Output: pty -> stdout, on its own thread so that input (Ctrl+C!)
    // still reaches the shell while stdout is full. Plain blocking reads
    // and writes measured fastest: a reader thread feeding a writer thread
    // (0.32 s for a 36 MB cat) or gathering reads into big writes (0.34 s)
    // both lose to this (0.29 s); the kernel's pty is the limit.
    let output = std::thread::spawn(move || {
        let mut master = master;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // EIO: every process holding the pty has closed it.
                Err(_) => break,
            }
        }
    });

    // Input: stdin -> pty, with control messages taken out.
    std::thread::spawn(move || {
        let mut parser = Input::default();
        let mut buf = vec![0u8; 1 << 14];
        loop {
            let n = match stdin.read(&mut buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(n) => n,
                Err(_) => 0,
            };
            if n == 0 {
                // The tab is gone: hang up the shell's session.
                // SAFETY: kill only sends a signal.
                unsafe {
                    libc::kill(-pid, libc::SIGHUP);
                    libc::kill(pid, libc::SIGHUP);
                }
                exit(0);
            }
            parser.feed(&buf[..n], |item| match item {
                Item::Data(d) => {
                    let _ = pty_in.write_all(d);
                }
                // SAFETY: TIOCSWINSZ reads one winsize, from a live reference.
                Item::Resize(ws) => unsafe {
                    libc::ioctl(pty_in.as_raw_fd(), libc::TIOCSWINSZ, &ws);
                },
            });
        }
    });

    let _ = output.join();
    let status = child.wait().ok();
    exit(match (status.and_then(|s| s.code()), status.and_then(|s| s.signal())) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        _ => 1,
    })
}

/// A file of its own on the same open file as `fd` (stdin, stdout): plain
/// reads and writes, without std's locks and line buffering.
fn dup(fd: std::os::fd::BorrowedFd) -> io::Result<File> {
    fd.try_clone_to_owned().map(File::from)
}

/// Splits stdin into pty data and control messages, across reads.
#[derive(Default)]
struct Input {
    /// Bytes of an unfinished control message, lead byte included.
    pending: Vec<u8>,
}

enum Item<'a> {
    Data(&'a [u8]),
    Resize(libc::winsize),
}

impl Input {
    fn feed(&mut self, mut buf: &[u8], mut emit: impl FnMut(Item)) {
        while !buf.is_empty() {
            if self.pending.is_empty() {
                let end = buf.iter().position(|&b| b == CONTROL).unwrap_or(buf.len());
                if end > 0 {
                    emit(Item::Data(&buf[..end]));
                }
                buf = &buf[end..];
                if buf.is_empty() {
                    break;
                }
            }
            // Inside a control message.
            let want = 2 + RESIZE_LEN - self.pending.len();
            let take = want.min(buf.len());
            self.pending.extend_from_slice(&buf[..take]);
            buf = &buf[take..];
            if self.pending.len() >= 2 && self.pending[1] != RESIZE {
                // Unknown message: drop the lead byte, keep the rest as data.
                let rest = self.pending.split_off(1);
                self.pending.clear();
                emit(Item::Data(&rest));
                continue;
            }
            if self.pending.len() == 2 + RESIZE_LEN {
                let p = &self.pending[2..];
                let u = |i: usize| u16::from_le_bytes([p[2 * i], p[2 * i + 1]]);
                emit(Item::Resize(winsize(u(0), u(1), u(2), u(3))));
                self.pending.clear();
            }
        }
    }
}

fn winsize(cols: u16, rows: u16, width: u16, height: u16) -> libc::winsize {
    libc::winsize { ws_col: cols, ws_row: rows, ws_xpixel: width, ws_ypixel: height }
}

/// The user's shell: the passwd entry, else $SHELL, else /bin/sh.
fn user_shell() -> OsString {
    // SAFETY: getpwuid's result is null or points to a static entry, read
    // here before anything else could call it (the threads start later).
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            let shell = CStr::from_ptr((*pw).pw_shell);
            if !shell.is_empty() {
                return OsStr::from_bytes(shell.to_bytes()).to_owned();
            }
        }
    }
    match std::env::var_os("SHELL") {
        Some(s) if !s.is_empty() => s,
        _ => "/bin/sh".into(),
    }
}

/// argv[0] for a login shell: "-" + the shell's file name.
fn login_argv0(shell: &OsStr) -> OsString {
    let name = shell.as_bytes().rsplit(|&b| b == b'/').next().unwrap_or(b"sh");
    let mut argv0 = OsString::from("-");
    argv0.push(OsStr::from_bytes(name));
    argv0
}

/// Starts `command` on a new pty; returns the pty's master side and the
/// child. An exec error (a missing program) comes back as Err.
fn spawn(command: &mut Command, size: &libc::winsize) -> io::Result<(File, Child)> {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty fills in two new file descriptors, owned from here on
    // by the OwnedFds.
    let (master, slave) = unsafe {
        if libc::openpty(&mut master, &mut slave, ptr::null_mut(), ptr::null(), size) != 0 {
            return Err(io::Error::last_os_error());
        }
        (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave))
    };
    // openpty's descriptors aren't close-on-exec; copies are, so the shell
    // inherits the pty only as its stdin, stdout and stderr. The originals
    // are closed here (shadowing them would keep them open).
    let cloexec = |fd: OwnedFd| fd.try_clone();
    let (master, slave) = (cloexec(master)?, cloexec(slave)?);

    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "fastcnsl");
    // std resets SIGPIPE (which Rust ignores) to the default in the child.
    // SAFETY: runs in the child between fork and exec, after the pty is its
    // stdin, and makes only async-signal-safe calls.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY, 0);
            Ok(())
        });
    }
    let child = command.spawn()?;
    Ok((File::from(master), child))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cols, rows, width, height
    type Size = (u16, u16, u16, u16);

    fn parse(chunks: &[&[u8]]) -> (Vec<u8>, Vec<Size>) {
        let mut input = Input::default();
        let (mut data, mut sizes) = (Vec::new(), Vec::new());
        for c in chunks {
            input.feed(c, |item| match item {
                Item::Data(d) => data.extend_from_slice(d),
                Item::Resize(w) => sizes.push((w.ws_col, w.ws_row, w.ws_xpixel, w.ws_ypixel)),
            });
        }
        (data, sizes)
    }

    #[test]
    fn data_passes_through() {
        assert_eq!(parse(&[b"ls -l\r", "é\x1b[A".as_bytes()]).0, "ls -l\ré\x1b[A".as_bytes());
    }

    #[test]
    fn resize_is_taken_out_even_when_split_across_reads() {
        let msg = [CONTROL, RESIZE, 120, 0, 30, 0, 0x80, 0x07, 0x38, 0x04];
        let whole = [b"a".as_slice(), &msg, b"b"].concat();
        assert_eq!(parse(&[&whole]), (b"ab".to_vec(), vec![(120, 30, 1920, 1080)]));
        let chunks: Vec<&[u8]> = whole.chunks(1).collect();
        assert_eq!(parse(&chunks), (b"ab".to_vec(), vec![(120, 30, 1920, 1080)]));
    }

    #[test]
    fn unknown_control_message_keeps_the_data() {
        assert_eq!(parse(&[&[b'x', CONTROL, b'y', b'z']]), (b"xyz".to_vec(), vec![]));
    }

    #[test]
    fn login_shell_argv0_starts_with_a_dash() {
        assert_eq!(login_argv0(OsStr::new("/usr/bin/bash")), "-bash");
        assert!(login_argv0(&user_shell()).as_bytes().starts_with(b"-"));
    }
}
