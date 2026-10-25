//! Updates, as den gets them: a release on GitHub carries the installer and
//! `latest.json` (version, notes, the installer's URL and its minisign
//! signature, as Tauri's updater writes it). den checks it a little after
//! start and from ☰ ▸ Check for Updates; a newer version is offered in a
//! dialog, downloaded, checked against the public key built into den, and
//! installed silently, after which den starts again. Other dens running
//! from the same exe are asked to quit first (they save their sessions);
//! the installer could not replace an exe they still run. On Linux, where
//! a running program can be replaced, they are left alone.
//!
//! The key is `packaging/updater.pub` (the public half of `~/.keys/den.key`,
//! which scripts/release.ts signs with). While that file is empty, a build
//! does not update.

use std::{path::PathBuf, process::Command};

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

const MANIFEST_URL: &str = "https://github.com/patrickiel/den/releases/latest/download/latest.json";
const PUBLIC_KEY: &str = include_str!("../packaging/updater.pub");
/// This build's entry in the manifest's `platforms`, as Tauri names them.
const PLATFORM: &str = if cfg!(windows) {
    "windows-x86_64"
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    "darwin-aarch64"
} else if cfg!(target_os = "macos") {
    "darwin-x86_64"
} else {
    "linux-x86_64"
};

/// A release newer than this build.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub notes: String,
    url: String,
    signature: String,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Whether this build can update at all (it has a key, and is a release build).
pub fn enabled() -> bool {
    !PUBLIC_KEY.trim().is_empty() && !cfg!(debug_assertions)
}

/// `a` newer than `b`, as x.y.z versions.
pub(crate) fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| {
        v.trim_start_matches('v')
            .split(['.', '-'])
            .take(3)
            .map(|n| n.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(a) > parse(b)
}

/// The newest release if it is newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let manifest: serde_json::Value = crate::backend::http::AGENT
        .get(MANIFEST_URL)
        .call()
        .map_err(|e| format!("Cannot reach the update server: {e}"))?
        .into_json()
        .map_err(|e| format!("Bad update manifest: {e}"))?;
    let version = manifest["version"]
        .as_str()
        .ok_or("Bad update manifest: no version")?
        .to_string();
    if !newer(&version, current_version()) {
        return Ok(None);
    }
    let platform = &manifest["platforms"][PLATFORM];
    if platform.is_null() {
        // A release is published with both platforms' builds; this guards a hand-made one.
        return Err(format!(
            "den {version} is out, but not built for this platform."
        ));
    }
    Ok(Some(Release {
        version,
        notes: manifest["notes"].as_str().unwrap_or_default().to_string(),
        url: platform["url"]
            .as_str()
            .ok_or("Bad update manifest: no download for this platform")?
            .to_string(),
        signature: platform["signature"]
            .as_str()
            .ok_or("Bad update manifest: no signature")?
            .to_string(),
    }))
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    };
    let clean: Vec<u8> = text
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let digits: Vec<u8> = chunk.iter().map(|&c| value(c)).collect::<Option<_>>()?;
        let n = digits
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, d)| n | (*d as u32) << (18 - 6 * i));
        for i in 0..digits.len().saturating_sub(1) {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// Check `bytes` against the signature Tauri's signer wrote (base64 of a
/// minisign signature file) with the key built in (base64 of a minisign
/// public key file).
fn verify(bytes: &[u8], signature: &str) -> Result<(), String> {
    verify_with(PUBLIC_KEY, bytes, signature)
}

fn verify_with(public_key: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    let text = |b64: &str| base64_decode(b64.trim()).and_then(|b| String::from_utf8(b).ok());
    let key = text(public_key).ok_or("Bad update key")?;
    let key =
        minisign_verify::PublicKey::decode(&key).map_err(|e| format!("Bad update key: {e}"))?;
    let signature = text(signature).ok_or("Bad signature")?;
    let signature = minisign_verify::Signature::decode(&signature)
        .map_err(|e| format!("Bad signature: {e}"))?;
    key.verify(bytes, &signature, false)
        .map_err(|_| "The download's signature does not match: not installed.".to_string())
}

/// Download and check the installer (the app archive on macOS and Linux); its path.
/// (A download is some 15 MB; the cap only guards against a wrong manifest.)
fn download(release: &Release) -> Result<PathBuf, String> {
    let bytes = crate::backend::http::get_bytes(&release.url, 200 * 1024 * 1024)
        .map_err(|e| format!("Download failed: {e}"))?;
    verify(&bytes, &release.signature)?;
    let name = if cfg!(windows) {
        format!("den_{}_setup.exe", release.version)
    } else if cfg!(target_os = "linux") {
        format!("den_{}_x86_64.tar.gz", release.version)
    } else {
        format!("den_{}.app.tar.gz", release.version)
    };
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Ask every other den running from this exe to quit, as its window's close
/// button would (each saves its session on the way out), and wait for them.
/// The installer cannot replace an exe another den still runs, and a den
/// left open would go on with the old version. One that stays (asking about
/// unsaved changes) is an error: the update is not staged then.
fn close_other_instances() -> Result<(), String> {
    use std::time::{Duration, Instant};
    const PATIENCE: Duration = Duration::from_secs(20);
    // The new binary goes over the running one there, which the others do
    // not hinder; they go on with the old version until they start again.
    if cfg!(target_os = "linux") {
        return Ok(());
    }
    let me = std::process::id();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let left = |n: usize| {
        Err(format!(
            "{n} other den window{} still open (unsaved changes?): close it, then update again.",
            if n == 1 { " is" } else { "s are" }
        ))
    };
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0},
            System::Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
            },
            UI::WindowsAndMessaging::{
                EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
            },
        };
        use windows::core::{BOOL, PWSTR};

        unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> BOOL {
            // SAFETY: `list` is the Vec below, which outlives the enumeration.
            unsafe { &mut *(list.0 as *mut Vec<HWND>) }.push(hwnd);
            BOOL(1)
        }
        /// The exe `pid` runs, as Windows spells it.
        fn exe_of(process: HANDLE) -> Option<PathBuf> {
            let mut buf = [0u16; 32768];
            let mut len = buf.len() as u32;
            // SAFETY: the buffer and its length go together.
            unsafe {
                QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(buf.as_mut_ptr()),
                    &mut len,
                )
            }
            .ok()?;
            Some(PathBuf::from(String::from_utf16_lossy(
                &buf[..len as usize],
            )))
        }
        let same = |a: &std::path::Path, b: &std::path::Path| {
            a.to_string_lossy()
                .eq_ignore_ascii_case(&b.to_string_lossy())
        };

        let mut windows: Vec<HWND> = Vec::new();
        // SAFETY: the callback only pushes to the Vec it is handed.
        unsafe { EnumWindows(Some(collect), LPARAM(&mut windows as *mut _ as isize)) }
            .map_err(|e| e.to_string())?;
        // Every other den running from this exe, by pid; `Some` keeps a
        // handle to wait on, `None` marks a process that is not den.
        let mut seen: Vec<(u32, Option<HANDLE>)> = Vec::new();
        for hwnd in windows {
            let mut pid = 0;
            // SAFETY: plain queries on a window handle (a stale one fails them).
            let mine = unsafe {
                !IsWindowVisible(hwnd).as_bool()
                    || GetWindowThreadProcessId(hwnd, Some(&mut pid)) == 0
                    || pid == me
            };
            if mine {
                continue;
            }
            let den = match seen.iter().find(|(p, _)| *p == pid) {
                Some((_, process)) => process.is_some(),
                None => {
                    // SAFETY: a query on a pid; the handle is closed below.
                    let process = unsafe {
                        OpenProcess(
                            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                            false,
                            pid,
                        )
                    }
                    .ok()
                    .filter(|&process| {
                        let den = exe_of(process).is_some_and(|path| same(&path, &exe));
                        if !den {
                            // SAFETY: a handle this opened and no longer needs.
                            let _ = unsafe { CloseHandle(process) };
                        }
                        den
                    });
                    seen.push((pid, process));
                    process.is_some()
                }
            };
            if den {
                // SAFETY: a message to a window that may be gone (then it fails).
                let _ = unsafe {
                    PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default())
                };
            }
        }
        let deadline = Instant::now() + PATIENCE;
        let mut open = 0;
        for process in seen.into_iter().filter_map(|(_, process)| process) {
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u32;
            // SAFETY: a handle opened above with PROCESS_SYNCHRONIZE, closed right after.
            unsafe {
                if WaitForSingleObject(process, wait) != WAIT_OBJECT_0 {
                    open += 1;
                }
                let _ = CloseHandle(process);
            }
        }
        if open > 0 {
            return left(open);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        // The other dens running from this exe (`ps` spells it in full).
        let others = || -> Vec<u32> {
            let Ok(out) = Command::new("ps").args(["-axo", "pid=,comm="]).output() else {
                return Vec::new();
            };
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let (pid, comm) = line.trim().split_once(' ')?;
                    let pid: u32 = pid.parse().ok()?;
                    (pid != me && std::path::Path::new(comm.trim()) == exe).then_some(pid)
                })
                .collect()
        };
        let pids = others();
        if pids.is_empty() {
            return Ok(());
        }
        // Quit them as the Dock would: each saves its session on the way out.
        let quit: String = pids
            .iter()
            .map(|pid| format!("$.NSRunningApplication.runningApplicationWithProcessIdentifier({pid}).terminate();"))
            .collect();
        Command::new("osascript")
            .args([
                "-l",
                "JavaScript",
                "-e",
                &format!("ObjC.import('AppKit');{quit}"),
            ])
            .output()
            .map_err(|e| format!("Cannot close the other den windows: {e}"))?;
        let deadline = Instant::now() + PATIENCE;
        loop {
            let open = others().len();
            if open == 0 {
                return Ok(());
            }
            if Instant::now() > deadline {
                return left(open);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Start what brings the new version up once this den has quit: on Windows
/// the installer, silently (it waits for the exe, installs over it and
/// starts den again); on macOS and Linux a shell that waits for this
/// process to end and starts the bundle or binary, swapped for the new one
/// here. Nothing is lost if the start fails: there the swap is undone.
/// `Some` names the folder the new version went to when that is not where
/// this one runs from (macOS only).
fn stage(download: &std::path::Path) -> Result<Option<PathBuf>, String> {
    #[cfg(windows)]
    {
        Command::new(download)
            .args(["/S", "/UPDATE"])
            .spawn()
            .map(|_| None)
            .map_err(|e| format!("Cannot start the installer: {e}"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // The new binary goes over this one (a rename can replace a running
        // program), from a staging folder beside it.
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = exe.parent().ok_or("bad exe path")?;
        if !writable(dir) {
            return Err(format!(
                "den runs from a folder it cannot write to ({}): update it by hand.",
                dir.display()
            ));
        }
        let staging = dir.join(".den-update");
        crate::backend::process::unpack(download, &staging, "the update")?;
        // The archive holds the folder scripts/bundle-linux.sh makes.
        let new = staging.join("den").join("den");
        if !new.is_file() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err("The update archive has no den.".into());
        }
        let old = dir.join(".den.old");
        std::fs::rename(&exe, &old)
            .map_err(|e| format!("Cannot replace {}: {e}", exe.display()))?;
        let undo = || {
            let _ = std::fs::rename(&old, &exe);
            let _ = std::fs::remove_dir_all(&staging);
        };
        if let Err(err) = std::fs::rename(&new, &exe) {
            undo();
            return Err(format!("Cannot install the update: {err}"));
        }
        let started = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("while kill -0 {} 2>/dev/null; do sleep 0.2; done; rm -rf \"$1\" \"$2\"; exec \"$0\"", std::process::id()))
            .arg(&exe)
            .arg(&old)
            .arg(&staging)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Err(err) = started {
            undo();
            return Err(format!("Cannot start the restart helper: {err}"));
        }
        Ok(None)
    }
    #[cfg(target_os = "macos")]
    {
        // The running bundle: `…/den.app/Contents/MacOS/den`.
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let bundle = exe
            .ancestors()
            .nth(3)
            .filter(|dir| dir.extension().is_some_and(|ext| ext == "app"))
            .ok_or("den is not running from an app bundle (a cargo build?); update it by hand.")?
            .to_path_buf();
        let parent = bundle.parent().ok_or("bad bundle path")?;
        // The new bundle goes over this one, from a staging folder next to
        // it (two renames on one volume). Opened from the disk image, or from
        // the read-only copy macOS runs a downloaded app from until the
        // Finder has moved it (App Translocation), nothing can be written
        // there: then it goes to the Applications folder, where a Mac app
        // belongs, and den starts from there.
        let (bundle, moved) = if writable(parent) {
            (bundle, false)
        } else {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let apps = [PathBuf::from("/Applications"), home.join("Applications")]
                .into_iter()
                .find(|dir| std::fs::create_dir_all(dir).is_ok() && writable(dir))
                .ok_or_else(|| {
                    format!(
                        "den is running from a read-only place ({}) and the Applications folder cannot be written to. Drag den.app into it with the Finder, open it from there and update again.",
                        parent.display()
                    )
                })?;
            (apps.join("den.app"), true)
        };
        let parent = bundle.parent().ok_or("bad bundle path")?;
        let staging = parent.join(".den-update");
        crate::backend::process::unpack(download, &staging, "the update")
            .map_err(|err| format!("{err} (is {} writable?)", parent.display()))?;
        let new = staging.join("den.app");
        if !new.join("Contents").join("MacOS").join("den").is_file() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err("The update archive has no den.app.".into());
        }
        let old = parent.join(".den.app.old");
        let _ = std::fs::remove_dir_all(&old);
        // In the Applications folder there may be no den yet.
        let replaced = bundle.exists();
        if replaced {
            std::fs::rename(&bundle, &old)
                .map_err(|e| format!("Cannot replace {}: {e}", bundle.display()))?;
        }
        let undo = |staging: &std::path::Path| {
            let _ = std::fs::rename(&bundle, &new);
            if replaced {
                let _ = std::fs::rename(&old, &bundle);
            }
            let _ = std::fs::remove_dir_all(staging);
        };
        if let Err(err) = std::fs::rename(&new, &bundle) {
            undo(&staging);
            return Err(format!("Cannot install the update: {err}"));
        }
        // The old bundle and the staging folder go once the new one is open.
        let started = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("while kill -0 {} 2>/dev/null; do sleep 0.2; done; open -n \"$0\"; rm -rf \"$1\" \"$2\"", std::process::id()))
            .arg(&bundle)
            .arg(&old)
            .arg(&staging)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Err(err) = started {
            undo(&staging);
            return Err(format!("Cannot start the restart helper: {err}"));
        }
        Ok(moved.then(|| parent.to_path_buf()))
    }
}

/// Whether a folder can be written to (the staging folder's name, so a
/// leftover from an earlier update counts as yes).
#[cfg(not(windows))]
fn writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".den-update");
    match std::fs::create_dir(&probe) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&probe);
            true
        }
        Err(err) => err.kind() == std::io::ErrorKind::AlreadyExists,
    }
}

/// Look for an update; `manual` says so when there is none (or it fails).
pub fn check_in_window(manual: bool, window: &mut Window, cx: &mut App) {
    if !enabled() {
        if manual {
            let why = if cfg!(debug_assertions) {
                "Development builds do not update."
            } else {
                "This build has no update key."
            };
            crate::toast::push(window, why, cx);
        }
        return;
    }
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = cx.background_spawn(async { check() }).await;
        _ = handle.update(cx, |_, window, cx| match result {
            Ok(Some(release)) => offer(release, window, cx),
            Ok(None) if manual => crate::toast::push(
                window,
                format!("den {} is the newest version.", current_version()),
                cx,
            ),
            Err(err) if manual => crate::toast::push(window, err, cx),
            _ => {}
        });
    })
    .detach();
}

/// Ask to install `release`.
fn offer(release: Release, window: &mut Window, cx: &mut App) {
    let notes = if release.notes.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n{}", release.notes.trim())
    };
    let text = format!(
        "den {} is available (this is {}).{notes}",
        release.version,
        current_version()
    );
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let release = release.clone();
        dialog
            .title("Update")
            .description(text.clone())
            .ok_text("Install and Restart")
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                install(release.clone(), window, cx);
                true
            })
    });
}

/// Download, check, stage the new version and quit; it starts again by itself.
fn install(release: Release, window: &mut Window, cx: &mut App) {
    crate::toast::push(window, format!("Downloading den {}…", release.version), cx);
    let handle = window.window_handle();
    let version = release.version.clone();
    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move {
                let file = download(&release)?;
                close_other_instances()?;
                stage(&file)
            })
            .await;
        _ = handle.update(cx, |_, window, cx| match result {
            // Quitting saves every session; the restart waits for this process.
            Ok(None) => cx.quit(),
            Ok(Some(dir)) => {
                let text = format!(
                    "den was running from a read-only place (the disk image, or macOS's copy of a downloaded app), so den {version} went to {}. It opens from there now; this copy can go.",
                    dir.display()
                );
                window.open_alert_dialog(cx, move |dialog, _, _| {
                    dialog.title("Installed to Applications").description(text.clone()).ok_text("Quit and Open").show_cancel(false).on_ok(|_, _, cx| {
                        cx.quit();
                        true
                    })
                });
            }
            Err(err) => crate::toast::push(window, err, cx),
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{base64_decode, newer};

    #[test]
    fn compares_versions() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("v1.0.0", "0.9.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.10.0"));
    }

    #[test]
    fn decodes_base64() {
        assert_eq!(base64_decode("TWFu").unwrap(), b"Man");
        assert_eq!(base64_decode("TWE=").unwrap(), b"Ma");
        assert_eq!(base64_decode("bABzAA==").unwrap(), b"l\0s\0");
    }
}
