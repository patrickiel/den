//! The login shell's PATH, for a den started without one.

use std::path::PathBuf;

/// Started from the Finder or the Dock, a Mac app gets the system's bare
/// PATH: no Homebrew, no `~/.cargo/bin`, so no `claude`, `pnpm` or `rustfmt`
/// for Source Control, Format Document and the presets. Started from a Linux
/// desktop's launcher, den gets the session's PATH, which lacks what
/// `~/.bashrc` adds (nvm, pnpm, volta). Take the login shell's PATH over, in
/// front of den's own. Run before anything else starts: it writes the
/// environment.
pub fn inherit() {
    use std::{
        io::Read as _,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let current: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    // Started from a terminal, PATH is already the shell's: nothing to do.
    // A Linux session's PATH has more than the system folders in it anyway,
    // so there the terminal itself tells.
    let system = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    let from_terminal = if cfg!(target_os = "macos") {
        current
            .iter()
            .any(|dir| !system.iter().any(|s| dir.as_os_str() == *s))
    } else {
        std::io::IsTerminal::is_terminal(&std::io::stdin())
    };
    if from_terminal {
        return;
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/bash"
            }
            .into()
        });
    // An interactive login shell, as a terminal starts one, so PATH set in
    // .zshrc or .bashrc counts too; markers in case the profile prints something.
    let fish = std::path::Path::new(&shell)
        .file_name()
        .is_some_and(|name| name == "fish");
    let script = if fish {
        "printf '%s' \"<den-path>\"(string join : $PATH)\"</den-path>\""
    } else {
        "printf '%s' \"<den-path>$PATH</den-path>\""
    };
    let Ok(mut child) = Command::new(&shell)
        .args(["-l", "-i", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    // A profile that waits for a terminal must not hold the app up.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
    let mut text = String::new();
    if child
        .stdout
        .take()
        .and_then(|mut out| out.read_to_string(&mut text).ok())
        .is_none()
    {
        return;
    }
    let Some(login) = text
        .split("<den-path>")
        .nth(1)
        .and_then(|rest| rest.split("</den-path>").next())
    else {
        return;
    };
    let mut dirs: Vec<PathBuf> = std::env::split_paths(login).collect();
    for dir in current {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        // SAFETY: called first thing in `main`, before any other thread exists.
        unsafe { std::env::set_var("PATH", joined) };
    }
}
