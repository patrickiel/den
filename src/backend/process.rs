//! Running programs from the backend: without a console window, and with
//! their input written from a thread of its own.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

/// No console window for a console program started from a GUI app.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Run `cmd` to its end with `stdin` on its input (none when `None`), both
/// outputs captured. The input goes from its own thread, so a long one
/// cannot deadlock against a full output pipe.
pub fn output_with_input(mut cmd: Command, stdin: Option<&str>) -> std::io::Result<Output> {
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    no_window(&mut cmd);
    let mut child = cmd.spawn()?;
    if let (Some(text), Some(mut pipe)) = (stdin.map(str::to_string), child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    child.wait_with_output()
}

/// Unpack `archive` into the new folder `into` with the system tar (bsdtar on
/// Windows 10+ reads zip too); `what` names it in the error.
pub(crate) fn unpack(archive: &Path, into: &Path, what: &str) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(into);
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let tar = if cfg!(windows) {
        let sysroot = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(sysroot).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    };
    let mut cmd = Command::new(tar);
    cmd.arg(if cfg!(windows) { "-xf" } else { "-xzf" }).arg(archive).arg("-C").arg(into);
    no_window(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Cannot run tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("Unpacking {what} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}
