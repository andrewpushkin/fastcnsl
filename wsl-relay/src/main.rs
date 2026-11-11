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

use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::process::exit;
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
    let argv: Vec<CString> = match (args[2].as_bytes(), &args[3..]) {
        (b"login", []) => login_shell(),
        (b"shell", [line]) => vec![user_shell(), c"-c".to_owned(), cstring(line.as_bytes())],
        (b"exec", [program, rest @ ..]) => {
            std::iter::once(program).chain(rest).map(|a| cstring(a.as_bytes())).collect()
        }
        _ => usage(),
    };
    // A login shell's argv[0] is "-bash"; the program is the shell's path.
    let program = if args[2].as_bytes() == b"login" { user_shell() } else { argv[0].clone() };

    let (master, pid) = match spawn(&program, &argv, &size) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fastcnsl-wsl: {}: {e}", program.to_string_lossy());
            exit(127)
        }
    };

    // Output: pty -> stdout, on its own thread so that input (Ctrl+C!)
    // still reaches the shell while stdout is full.
    // Output: pty -> stdout, on its own thread so that input (Ctrl+C!)
    // still reaches the shell while stdout is full. Plain blocking reads
    // and writes measured fastest: a reader thread feeding a writer thread
    // (0.32 s for a 36 MB cat) or gathering reads into big writes (0.34 s)
    // both lose to this (0.29 s); the kernel's pty is the limit.
    let output = std::thread::spawn(move || {
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                if write_all(1, &buf[..n as usize]).is_err() {
                    break;
                }
            } else if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            } else {
                // 0 or EIO: every process holding the pty has closed it.
                break;
            }
        }
    });

    // Input: stdin -> pty, with control messages taken out.
    std::thread::spawn(move || {
        let mut parser = Input::default();
        let mut buf = vec![0u8; 1 << 14];
        loop {
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if n <= 0 {
                // The tab is gone: hang up the shell's session.
                unsafe {
                    libc::kill(-pid, libc::SIGHUP);
                    libc::kill(pid, libc::SIGHUP);
                }
                exit(0);
            }
            parser.feed(&buf[..n as usize], |item| match item {
                Item::Data(d) => {
                    let _ = write_all(master, d);
                }
                Item::Resize(ws) => unsafe {
                    libc::ioctl(master, libc::TIOCSWINSZ, &ws);
                },
            });
        }
    });

    let _ = output.join();
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    exit(if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    })
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

fn cstring(bytes: &[u8]) -> CString {
    CString::new(bytes).unwrap_or_else(|_| {
        eprintln!("fastcnsl-wsl: argument contains a NUL byte");
        exit(2)
    })
}

/// The user's shell: the passwd entry, else $SHELL, else /bin/sh.
fn user_shell() -> CString {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            let shell = CStr::from_ptr((*pw).pw_shell);
            if !shell.is_empty() {
                return shell.to_owned();
            }
        }
    }
    match std::env::var_os("SHELL") {
        Some(s) if !s.is_empty() => cstring(s.as_bytes()),
        _ => c"/bin/sh".to_owned(),
    }
}

/// argv for a login shell: argv[0] is "-" + the shell's file name.
fn login_shell() -> Vec<CString> {
    let shell = user_shell();
    let name = shell.as_bytes().rsplit(|&b| b == b'/').next().unwrap_or(b"sh");
    let mut argv0 = b"-".to_vec();
    argv0.extend_from_slice(name);
    vec![cstring(&argv0)]
}

/// Starts `program` on a new pty; returns the master fd and the child pid.
fn spawn(program: &CStr, argv: &[CString], size: &libc::winsize) -> io::Result<(i32, libc::pid_t)> {
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(ptr::null());
    let env = [
        (c"TERM", c"xterm-256color"),
        (c"COLORTERM", c"truecolor"),
        (c"TERM_PROGRAM", c"fastcnsl"),
    ];

    let (mut master, mut slave) = (0, 0);
    let size = *size;
    if unsafe { libc::openpty(&mut master, &mut slave, ptr::null_mut(), ptr::null(), &size) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // A pipe that closes on a successful exec: an exec error comes back
    // through it, so a missing program is reported, not a silent exit.
    let mut err_pipe = [0; 2];
    if unsafe { libc::pipe2(err_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // Child: only async-signal-safe calls from here on.
        unsafe {
            libc::close(err_pipe[0]);
            libc::setsid();
            libc::ioctl(slave, libc::TIOCSCTTY, 0);
            libc::dup2(slave, 0);
            libc::dup2(slave, 1);
            libc::dup2(slave, 2);
            if slave > 2 {
                libc::close(slave);
            }
            libc::close(master);
            // Rust ignores SIGPIPE; the shell and its children must not.
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            for (k, v) in env {
                libc::setenv(k.as_ptr(), v.as_ptr(), 1);
            }
            libc::execvp(program.as_ptr(), argv_ptrs.as_ptr());
            let errno = *libc::__errno_location();
            libc::write(err_pipe[1], (&errno as *const i32).cast(), 4);
            libc::_exit(127);
        }
    }

    unsafe {
        libc::close(slave);
        libc::close(err_pipe[1]);
        let mut errno = 0i32;
        let n = libc::read(err_pipe[0], (&mut errno as *mut i32).cast(), 4);
        libc::close(err_pipe[0]);
        if n == 4 {
            libc::waitpid(pid, ptr::null_mut(), 0);
            libc::close(master);
            return Err(io::Error::from_raw_os_error(errno));
        }
    }
    Ok((master, pid))
}

fn write_all(fd: i32, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        buf = &buf[n as usize..];
    }
    Ok(())
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
        assert!(login_shell()[0].as_bytes().starts_with(b"-"));
    }
}
