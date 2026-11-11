//! WSL tabs without ConPTY (Windows).
//!
//! A Windows terminal normally runs `wsl.exe` on a ConPTY pseudoconsole,
//! and every byte of output then goes through the Windows console host,
//! which is the slow part: `cat` of a 36 MB file takes 0.42 s that way
//! (Windows Terminal: 0.41 s), but 0.07 s through a plain pipe. So for a
//! `wsl.exe` shell, fastcnsl starts `wsl.exe ... -e fastcnsl-wsl ...`
//! with plain pipes instead: `fastcnsl-wsl` (`wsl-relay/`, shipped next
//! to fastcnsl.exe) runs the shell on a Linux pty and relays it.
//!
//! Output comes back unchanged on stdout (stderr too). Input goes to its
//! stdin, where 0xFF, which UTF-8 never uses, starts a control message:
//! `0xFF 'R'` and cols, rows, width, height (u16 little-endian) resizes.

use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::windows::blocking::{UnblockedReader, UnblockedWriter};
use alacritty_terminal::tty::windows::child::ChildExitWatcher;
use alacritty_terminal::tty::windows::{PTY_CHILD_EVENT_TOKEN, PTY_READ_WRITE_TOKEN};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};

/// The relay's file name, next to fastcnsl.exe.
pub const RELAY: &str = "fastcnsl-wsl";

/// Control-message lead byte on the relay's stdin.
const CONTROL: u8 = 0xFF;

/// Bytes buffered between the pipes and the event loop: 1 MiB, as
/// alacritty_terminal buffers ConPTY's.
const PIPE_CAPACITY: usize = 0x10_0000;

/// No console window for wsl.exe.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Turns `wsl` arguments into wsl.exe options plus what the relay runs:
/// `["login"]`, `["shell", LINE]` or `["exec", PROGRAM, ARGS...]`.
/// `None` for anything else (`--list`, `--install`, unknown options):
/// those keep running on ConPTY.
pub fn plan(program: &str, args: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    let name = program.rsplit(['\\', '/']).next().unwrap_or(program).to_ascii_lowercase();
    if name != "wsl" && name != "wsl.exe" {
        return None;
    }
    let mut options = Vec::new();
    let mut i = 0;
    if args.first().is_some_and(|a| a == "~") {
        options.extend(["--cd".to_owned(), "~".to_owned()]);
        i = 1;
    }
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--distribution" | "-u" | "--user" | "--cd" => {
                options.extend([arg.to_owned(), args.get(i + 1)?.clone()]);
                i += 2;
            }
            "-e" | "--exec" => {
                let rest = &args[i + 1..];
                if rest.is_empty() {
                    return None;
                }
                let run = ["exec".to_owned()].into_iter().chain(rest.iter().cloned()).collect();
                return Some((options, run));
            }
            "--" => return Some((options, shell_line(&args[i + 1..]))),
            _ if arg.starts_with('-') => return None,
            _ => return Some((options, shell_line(&args[i..]))),
        }
    }
    Some((options, vec!["login".to_owned()]))
}

/// A command line for the user's shell, as wsl.exe runs one (`$SHELL -c`).
fn shell_line(words: &[String]) -> Vec<String> {
    if words.is_empty() {
        return vec!["login".to_owned()];
    }
    vec!["shell".to_owned(), words.join(" ")]
}

/// Finds the relay next to the running exe.
pub fn relay_path() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.with_file_name(RELAY);
    path.is_file().then_some(path)
}

/// A Windows path as `wslpath` takes it: no `\\?\` prefix.
fn plain_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned()
    }
}

/// The relay's PTY: the event loop reads and writes it like ConPTY's.
pub struct WslPty {
    // Fields drop in order: stop watching before `child` closes its handle.
    watcher: ChildExitWatcher,
    output: UnblockedReader<io::PipeReader>,
    input: Input,
    _child: Child,
}

impl WslPty {
    /// Starts `wsl.exe options... -e fastcnsl-wsl COLS ROWS run...`.
    pub fn spawn(
        wsl: &str,
        options: &[String],
        run: &[String],
        relay: &Path,
        cwd: Option<&Path>,
        size: WindowSize,
    ) -> io::Result<Self> {
        // wsl.exe takes a Windows path for the program only through
        // `wslpath`, so a tiny `sh` converts it and execs the relay.
        const LAUNCH: &str = r#"r=$(wslpath -u "$1") || exit 127; shift; exec "$r" "$@""#;
        let (reader, writer) = io::pipe()?;
        let mut cmd = Command::new(wsl);
        cmd.args(options)
            .args(["-e", "sh", "-c", LAUNCH, "sh", &plain_path(relay)])
            .args([size.num_cols.to_string(), size.num_lines.to_string()])
            .args(run)
            .stdin(Stdio::piped())
            .stdout(writer.try_clone()?)
            .stderr(writer)
            .creation_flags(CREATE_NO_WINDOW);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn()?;
        // Our copies of the write end are gone with `cmd`, so the reader
        // sees EOF once wsl.exe exits.
        drop(cmd);
        let stdin = child.stdin.take().expect("piped stdin");
        let watcher = ChildExitWatcher::new(child.as_raw_handle() as _)?;
        let mut input = Input(UnblockedWriter::new(stdin, PIPE_CAPACITY));
        input.resize(size);
        Ok(Self { watcher, output: UnblockedReader::new(reader, PIPE_CAPACITY), input, _child: child })
    }
}

fn with_key(mut event: Event, key: usize) -> Event {
    event.key = key;
    event
}

impl EventedReadWrite for WslPty {
    type Reader = UnblockedReader<io::PipeReader>;
    type Writer = Input;

    // The trait marks it unsafe; nothing unsafe happens here.
    #[allow(unsafe_code)]
    unsafe fn register(&mut self, poll: &Arc<Poller>, interest: Event, mode: PollMode) -> io::Result<()> {
        self.reregister(poll, interest, mode)
    }

    fn reregister(&mut self, poll: &Arc<Poller>, interest: Event, mode: PollMode) -> io::Result<()> {
        self.input.0.register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), mode);
        self.output.register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), mode);
        self.watcher.register(poll, with_key(interest, PTY_CHILD_EVENT_TOKEN));
        Ok(())
    }

    fn deregister(&mut self, _poll: &Arc<Poller>) -> io::Result<()> {
        self.input.0.deregister();
        self.output.deregister();
        self.watcher.deregister();
        Ok(())
    }

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.output
    }

    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.input
    }
}

impl EventedPty for WslPty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        match self.watcher.event_rx().try_recv() {
            Ok(ev) => Some(ev),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(ChildEvent::Exited(None)),
        }
    }
}

impl OnResize for WslPty {
    fn on_resize(&mut self, size: WindowSize) {
        self.input.resize(size);
    }
}

/// The relay's stdin. Keyboard input and pastes are UTF-8 and never hold
/// 0xFF; a stray one is dropped so it can't start a control message.
pub struct Input(UnblockedWriter<ChildStdin>);

impl Input {
    fn resize(&mut self, size: WindowSize) {
        let mut msg = vec![CONTROL, b'R'];
        for v in [size.num_cols, size.num_lines, size.cell_width * size.num_cols, size.cell_height * size.num_lines] {
            msg.extend_from_slice(&v.to_le_bytes());
        }
        // Whole or not at all, or the relay would read the rest as input.
        // The pipe has room unless a megabyte of input is unsent.
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut sent = 0;
        while sent < msg.len() && Instant::now() < deadline {
            sent += self.0.write(&msg[sent..]).unwrap_or(0);
            if sent < msg.len() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

impl Write for Input {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match buf.iter().position(|&b| b == CONTROL) {
            Some(0) => Ok(1),
            Some(end) => self.0.write(&buf[..end]),
            None => self.0.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plans_wsl_command_lines() {
        assert_eq!(plan("wsl.exe", &[]), Some((vec![], s(&["login"]))));
        assert_eq!(plan(r"C:\Windows\System32\WSL.EXE", &[]), Some((vec![], s(&["login"]))));
        assert_eq!(
            plan("wsl", &s(&["-d", "Ubuntu", "--cd", "~", "-e", "htop", "-d", "5"])),
            Some((s(&["-d", "Ubuntu", "--cd", "~"]), s(&["exec", "htop", "-d", "5"])))
        );
        assert_eq!(plan("wsl", &s(&["~"])), Some((s(&["--cd", "~"]), s(&["login"]))));
        assert_eq!(plan("wsl", &s(&["ls", "-la"])), Some((vec![], s(&["shell", "ls -la"]))));
        assert_eq!(plan("wsl", &s(&["-u", "root", "--", "id"])), Some((s(&["-u", "root"]), s(&["shell", "id"]))));
    }

    #[test]
    fn leaves_other_commands_to_conpty() {
        assert_eq!(plan("powershell.exe", &[]), None);
        assert_eq!(plan("wsl.exe", &s(&["--list"])), None);
        assert_eq!(plan("wsl.exe", &s(&["--shutdown"])), None);
        assert_eq!(plan("wsl.exe", &s(&["-d"])), None);
        assert_eq!(plan("wsl.exe", &s(&["-e"])), None);
    }

    #[test]
    fn strips_verbatim_prefixes() {
        assert_eq!(plain_path(Path::new(r"\\?\C:\Apps\fastcnsl-wsl")), r"C:\Apps\fastcnsl-wsl");
        assert_eq!(plain_path(Path::new(r"\\?\UNC\wsl.localhost\Ubuntu\tmp\r")), r"\\wsl.localhost\Ubuntu\tmp\r");
        assert_eq!(plain_path(Path::new(r"D:\r")), r"D:\r");
    }
}
