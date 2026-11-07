//! Opening a file, folder or web page with the desktop's default app.
//!
//! Linux desktops differ in what they provide, so several programs are
//! tried in turn: under WSL, Windows itself (`wslview`, `explorer.exe`)
//! comes first, since the Linux side often has no file manager or browser
//! and no `xdg-open`; then `xdg-open` and `gio open`.

use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::process::Command;

/// Opens `target` (a path or a URL). The error says what went wrong.
pub fn open(target: impl AsRef<OsStr>) -> Result<(), String> {
    let target = target.as_ref();
    let mut tried = Vec::new();
    for (program, args) in openers(target) {
        match Command::new(&program).args(&args).spawn() {
            Ok(mut child) => {
                // Reap it when it exits, so it doesn't linger as a zombie.
                std::thread::spawn(move || child.wait());
                return Ok(());
            }
            Err(e) if e.kind() == ErrorKind::NotFound => tried.push(program),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    }
    Err(format!("no program found to open it (tried {})", tried.join(", ")))
}

/// The programs to try for `target`, in order, with their arguments.
fn openers(target: &OsStr) -> Vec<(String, Vec<OsString>)> {
    let one = |program: &str, args: &[&OsStr]| (program.to_owned(), args.iter().map(|a| a.to_os_string()).collect());
    if cfg!(target_os = "windows") {
        return vec![one("cmd", &[OsStr::new("/C"), OsStr::new("start"), OsStr::new(""), target])];
    }
    if cfg!(target_os = "macos") {
        return vec![one("open", &[target])];
    }
    let wsl = is_wsl().then(|| windows_path(target));
    linux_openers(target, wsl)
}

/// Linux: `wsl` is Some under WSL, holding `target` as Windows sees it
/// (None inside when it couldn't be converted).
fn linux_openers(target: &OsStr, wsl: Option<Option<OsString>>) -> Vec<(String, Vec<OsString>)> {
    let mut list = Vec::new();
    if let Some(windows) = wsl {
        list.push(("wslview".to_owned(), vec![target.to_os_string()]));
        // explorer.exe opens folders, files with their app, and URLs.
        if let Some(w) = windows {
            list.push(("explorer.exe".to_owned(), vec![w]));
        }
    }
    list.push(("xdg-open".to_owned(), vec![target.to_os_string()]));
    list.push(("gio".to_owned(), vec!["open".into(), target.to_os_string()]));
    list
}

/// Running under Windows Subsystem for Linux.
fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/sys/kernel/osrelease").is_ok_and(|r| r.to_ascii_lowercase().contains("microsoft"))
}

/// `target` for a Windows program: URLs as they are, paths converted by
/// `wslpath -w` (e.g. `\\wsl.localhost\Ubuntu\home\me`).
fn windows_path(target: &OsStr) -> Option<OsString> {
    let text = target.to_string_lossy();
    if text.contains("://") {
        return Some(target.to_os_string());
    }
    let out = Command::new("wslpath").arg("-w").arg(target).output().ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim_end_matches(['\r', '\n']);
    (out.status.success() && !path.is_empty()).then(|| path.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programs(list: &[(String, Vec<OsString>)]) -> Vec<&str> {
        list.iter().map(|(p, _)| p.as_str()).collect()
    }

    #[test]
    fn wsl_tries_windows_first_then_the_linux_desktop() {
        let dir = OsStr::new("/home/me/ragamuffin-logs");
        let win = OsString::from(r"\\wsl.localhost\Ubuntu\home\me\ragamuffin-logs");
        let list = linux_openers(dir, Some(Some(win.clone())));
        assert_eq!(programs(&list), ["wslview", "explorer.exe", "xdg-open", "gio"]);
        assert_eq!(list[1].1, [win], "explorer gets the Windows path");
        assert_eq!(list[3].1, [OsString::from("open"), dir.to_os_string()]);
        // No Windows path: explorer is left out.
        assert_eq!(programs(&linux_openers(dir, Some(None))), ["wslview", "xdg-open", "gio"]);
        // Elsewhere only the desktop's own openers.
        assert_eq!(programs(&linux_openers(dir, None)), ["xdg-open", "gio"]);
    }

    #[test]
    fn urls_go_to_windows_unchanged() {
        let url = OsStr::new("https://github.com/andrewpushkin/ragamuffin");
        assert_eq!(windows_path(url).as_deref(), Some(url));
    }

}
