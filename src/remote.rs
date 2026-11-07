//! Command-line control of a running ragamuffin:
//!
//! ```text
//! ragamuffin [--gpu API]
//! ragamuffin [--pid N] new-tab [NAME] [-- COMMAND ARGS...]
//! ragamuffin [--pid N] close-tab [NAME | NUMBER]
//! ragamuffin [--pid N] quit
//! ```
//!
//! Each window listens on a loopback TCP port. It writes the port and a
//! random secret to `<local data dir>/ragamuffin/instances/<pid>` (readable
//! only by the user); a request must carry the secret. Without `--pid` the
//! most recently started window is used.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::config::GpuBackend;
use crate::session::UserEvent;

/// What a window is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Open a tab, optionally named and running `command` instead of the
    /// shell.
    NewTab { name: Option<String>, command: Vec<String> },
    /// Close the tab with this name or 1-based number; the active tab if None.
    CloseTab(Option<String>),
    Quit,
}

/// The answer sent back to the command line.
pub type Reply = Result<String, String>;

/// A command for a running window, and which window.
#[derive(Debug, PartialEq)]
pub struct Invocation {
    pub pid: Option<u32>,
    pub command: Command,
}

/// A parsed command line.
#[derive(Debug, PartialEq, Default)]
pub struct Args {
    pub pid: Option<u32>,
    /// Graphics API for a window this process opens (not saved).
    pub gpu: Option<GpuBackend>,
    /// None: start the terminal.
    pub command: Option<Command>,
}

impl Args {
    pub fn invocation(&self) -> Option<Invocation> {
        self.command.clone().map(|command| Invocation { pid: self.pid, command })
    }
}

pub const USAGE: &str = "usage:
  ragamuffin [--gpu API]                           start the terminal; API: auto,
                                                   directx, vulkan, opengl, metal,
                                                   software (this run only)
  ragamuffin [--pid N] new-tab [NAME] [-- COMMAND ARGS...]
  ragamuffin [--pid N] close-tab [NAME | NUMBER]   (default: the active tab)
  ragamuffin [--pid N] quit
Without --pid the most recently started window is used.";

/// Parses the arguments after the program name.
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut rest = args;
    let mut out = Args::default();
    loop {
        match rest.first().map(String::as_str) {
            Some("--pid") => {
                let n = rest.get(1).ok_or("--pid needs a process id")?;
                out.pid = Some(n.parse().map_err(|_| format!("--pid: not a process id: {n}"))?);
                rest = &rest[2..];
            }
            Some("--gpu") => {
                let n = rest.get(1).ok_or("--gpu needs an API: auto, directx, vulkan, opengl, metal or software")?;
                out.gpu = Some(GpuBackend::from_name(n).ok_or_else(|| format!("--gpu: unknown API {n}\n{USAGE}"))?);
                rest = &rest[2..];
            }
            _ => break,
        }
    }
    let Some((verb, rest)) = rest.split_first() else {
        return if out.pid.is_some() { Err("--pid needs a command".into()) } else { Ok(out) };
    };
    out.command = Some(match verb.as_str() {
        "new-tab" => {
            let (before, command) = match rest.iter().position(|a| a == "--") {
                Some(i) => (&rest[..i], rest[i + 1..].to_vec()),
                None => (rest, Vec::new()),
            };
            if before.len() > 1 {
                return Err(format!("new-tab takes one name; put the command after --: {}", before.join(" ")));
            }
            Command::NewTab { name: before.first().cloned(), command }
        }
        "close-tab" if rest.len() <= 1 => Command::CloseTab(rest.first().cloned()),
        "quit" if rest.is_empty() => Command::Quit,
        "-h" | "--help" | "help" => return Err(USAGE.into()),
        _ => return Err(format!("unknown command: {}\n{USAGE}", args.join(" "))),
    });
    Ok(out)
}

/// On the wire: one TOML document per connection, answered with "ok\n..."
/// or "error\n...".
#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    action: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    command: Vec<String>,
}

impl Request {
    fn new(token: String, c: &Command) -> Self {
        let (action, name, command) = match c {
            Command::NewTab { name, command } => ("new-tab", name.clone(), command.clone()),
            Command::CloseTab(tab) => ("close-tab", tab.clone(), Vec::new()),
            Command::Quit => ("quit", None, Vec::new()),
        };
        Self { token, action: action.into(), name, command }
    }

    fn command(self) -> Result<Command, String> {
        match self.action.as_str() {
            "new-tab" => Ok(Command::NewTab { name: self.name, command: self.command }),
            "close-tab" => Ok(Command::CloseTab(self.name)),
            "quit" => Ok(Command::Quit),
            other => Err(format!("unknown action {other}")),
        }
    }
}

fn instances_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("ragamuffin").join("instances"))
}

/// A fresh random secret (RandomState is seeded from the OS).
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        h.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

/// The listening side, owned by the window. Dropping it removes the
/// instance file.
pub struct Server {
    file: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}

/// Starts listening for commands; None (logged) if that isn't possible.
pub fn serve(proxy: EventLoopProxy<UserEvent>) -> Option<Server> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| log::warn!("remote control: {e}")).ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = new_token();
    let dir = instances_dir()?;
    let file = dir.join(std::process::id().to_string());
    if let Err(e) = write_private(&dir, &file, &format!("{port} {token}\n")) {
        log::warn!("remote control: writing {}: {e}", file.display());
        return None;
    }
    std::thread::Builder::new()
        .name("remote".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Err(e) = handle(stream, &token, &proxy) {
                    log::warn!("remote control: {e}");
                }
            }
        })
        .ok()?;
    log::info!("remote control on 127.0.0.1:{port}");
    Some(Server { file })
}

fn write_private(dir: &std::path::Path, file: &std::path::Path, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(file)?.write_all(text.as_bytes())
}

fn handle(mut stream: TcpStream, token: &str, proxy: &EventLoopProxy<UserEvent>) -> Result<(), String> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    let mut text = String::new();
    (&mut stream).take(64 * 1024).read_to_string(&mut text).map_err(|e| e.to_string())?;
    let reply = match toml::from_str::<Request>(&text) {
        Ok(req) if req.token == token => match req.command() {
            Ok(cmd) => {
                let (tx, rx) = mpsc::channel();
                if proxy.send_event(UserEvent::Remote(cmd, tx)).is_err() {
                    Err("the window is closing".to_owned())
                } else {
                    rx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| Err("no answer from the window".into()))
                }
            }
            Err(e) => Err(e),
        },
        Ok(_) => Err("wrong token".to_owned()),
        Err(e) => Err(format!("bad request: {e}")),
    };
    let out = match reply {
        Ok(msg) => format!("ok\n{msg}"),
        Err(msg) => format!("error\n{msg}"),
    };
    stream.write_all(out.as_bytes()).map_err(|e| e.to_string())
}

/// Windows: the release build is a GUI program, so it has no console and
/// its replies would vanish when typed in cmd or PowerShell. Borrow the
/// console of the shell it was started from, unless output already goes
/// somewhere (a pipe or file).
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn attach_console() {
    use std::os::windows::io::AsRawHandle;
    if std::io::stdout().as_raw_handle().is_null() {
        // SAFETY: a plain Win32 call with a constant argument.
        let _ = unsafe { windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS) };
    }
}

#[cfg(not(windows))]
pub fn attach_console() {}

/// Why a command couldn't be delivered.
#[derive(Debug)]
pub enum SendError {
    /// No window is running (or the one asked for isn't).
    NoInstance,
    Failed(String),
}

/// Sends `command` to a running window and returns its reply.
pub fn send(pid: Option<u32>, command: &Command) -> Result<Reply, SendError> {
    let dir = instances_dir().ok_or(SendError::NoInstance)?;
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = match pid {
        Some(p) => vec![(std::time::UNIX_EPOCH, dir.join(p.to_string()))],
        None => std::fs::read_dir(&dir)
            .map_err(|_| SendError::NoInstance)?
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect(),
    };
    // Newest first.
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, file) in candidates {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let mut parts = text.split_whitespace();
        let (Some(port), Some(token)) = (parts.next().and_then(|p| p.parse::<u16>().ok()), parts.next()) else { continue };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
            // That window is gone without cleaning up (crash, kill).
            let _ = std::fs::remove_file(&file);
            continue;
        };
        let req = toml::to_string(&Request::new(token.to_owned(), command)).map_err(|e| SendError::Failed(e.to_string()))?;
        let io = |e: std::io::Error| SendError::Failed(e.to_string());
        stream.write_all(req.as_bytes()).map_err(io)?;
        stream.shutdown(std::net::Shutdown::Write).map_err(io)?;
        stream.set_read_timeout(Some(Duration::from_secs(15))).map_err(io)?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).map_err(io)?;
        return Ok(match reply.split_once('\n') {
            Some(("ok", msg)) => Ok(msg.to_owned()),
            Some(("error", msg)) => Err(msg.to_owned()),
            _ => Err(format!("unexpected reply: {reply}")),
        });
    }
    Err(SendError::NoInstance)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_commands() {
        let cmd = |s: &str| parse_args(&args(s)).unwrap().command;
        assert_eq!(parse_args(&[]).unwrap(), Args::default());
        assert_eq!(cmd("new-tab"), Some(Command::NewTab { name: None, command: vec![] }));
        assert_eq!(
            cmd("new-tab logs -- tail -f /var/log/syslog"),
            Some(Command::NewTab { name: Some("logs".into()), command: args("tail -f /var/log/syslog") })
        );
        assert_eq!(cmd("new-tab -- htop"), Some(Command::NewTab { name: None, command: args("htop") }));
        let a = parse_args(&args("--pid 42 close-tab 2")).unwrap();
        assert_eq!(a.invocation(), Some(Invocation { pid: Some(42), command: Command::CloseTab(Some("2".into())) }));
        assert_eq!(cmd("close-tab"), Some(Command::CloseTab(None)));
        assert_eq!(cmd("quit"), Some(Command::Quit));
        assert!(parse_args(&args("new-tab a b")).is_err(), "one name only");
        assert!(parse_args(&args("frobnicate")).is_err());
        assert!(parse_args(&args("--pid x quit")).is_err());
        assert!(parse_args(&args("--help")).unwrap_err().contains("new-tab"));
    }

    #[test]
    fn gpu_option_starts_a_window_with_that_api() {
        let a = parse_args(&args("--gpu software")).unwrap();
        assert_eq!((a.gpu, a.command), (Some(GpuBackend::Software), None));
        let a = parse_args(&args("--gpu Vulkan new-tab x")).unwrap();
        assert_eq!(a.gpu, Some(GpuBackend::Vulkan));
        assert!(a.command.is_some());
        assert!(parse_args(&args("--gpu")).is_err());
        assert!(parse_args(&args("--gpu glide")).unwrap_err().contains("unknown API"));
    }

    #[test]
    fn requests_round_trip_through_toml() {
        for c in [
            Command::NewTab { name: Some("a \"b\"".into()), command: args("echo hi") },
            Command::NewTab { name: None, command: vec![] },
            Command::CloseTab(Some("3".into())),
            Command::CloseTab(None),
            Command::Quit,
        ] {
            let text = toml::to_string(&Request::new("t".into(), &c)).unwrap();
            let back: Request = toml::from_str(&text).unwrap();
            assert_eq!(back.token, "t");
            assert_eq!(back.command().unwrap(), c);
        }
    }

    #[test]
    fn tokens_are_random() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
