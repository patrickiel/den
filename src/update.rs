//! Updates, as den gets them: a release on GitHub carries the installer and
//! `latest.json` (version, notes, the installer's URL and its minisign
//! signature, as Tauri's updater writes it). den checks it a little after
//! start and from ☰ ▸ Check for Updates; a newer version is offered in a
//! dialog, downloaded, checked against the public key built into den, and
//! installed silently, after which den starts again.
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
    let parse = |v: &str| v.trim_start_matches('v').split(['.', '-']).take(3).map(|n| n.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
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
    let version = manifest["version"].as_str().ok_or("Bad update manifest: no version")?.to_string();
    if !newer(&version, current_version()) {
        return Ok(None);
    }
    let platform = &manifest["platforms"][PLATFORM];
    if platform.is_null() {
        // The Mac build joins a release after the Windows one (scripts/release.ts --attach).
        return Err(format!("den {version} is out, but not built for this platform yet."));
    }
    Ok(Some(Release {
        version,
        notes: manifest["notes"].as_str().unwrap_or_default().to_string(),
        url: platform["url"].as_str().ok_or("Bad update manifest: no download for this platform")?.to_string(),
        signature: platform["signature"].as_str().ok_or("Bad update manifest: no signature")?.to_string(),
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
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let digits: Vec<u8> = chunk.iter().map(|&c| value(c)).collect::<Option<_>>()?;
        let n = digits.iter().enumerate().fold(0u32, |n, (i, d)| n | (*d as u32) << (18 - 6 * i));
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
    let key = minisign_verify::PublicKey::decode(&key).map_err(|e| format!("Bad update key: {e}"))?;
    let signature = text(signature).ok_or("Bad signature")?;
    let signature = minisign_verify::Signature::decode(&signature).map_err(|e| format!("Bad signature: {e}"))?;
    key.verify(bytes, &signature, false).map_err(|_| "The download's signature does not match: not installed.".to_string())
}

/// Download and check the installer (the app archive on macOS); its path.
/// (A download is some 15 MB; the cap only guards against a wrong manifest.)
fn download(release: &Release) -> Result<PathBuf, String> {
    let bytes = crate::backend::http::get_bytes(&release.url, 200 * 1024 * 1024).map_err(|e| format!("Download failed: {e}"))?;
    verify(&bytes, &release.signature)?;
    let name = if cfg!(windows) { format!("den_{}_setup.exe", release.version) } else { format!("den_{}.app.tar.gz", release.version) };
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Start what brings the new version up once this den has quit: on Windows
/// the installer, silently (it waits for the exe, installs over it and
/// starts den again); on macOS a shell that waits for this process to end
/// and opens the bundle, swapped for the new one here. Nothing is lost if
/// the start fails: on macOS the swap is undone.
fn stage(download: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        Command::new(download).args(["/S", "/UPDATE"]).spawn().map(|_| ()).map_err(|e| format!("Cannot start the installer: {e}"))
    }
    #[cfg(not(windows))]
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
        // Next to the bundle, so the swap is two renames on one volume.
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
        std::fs::rename(&bundle, &old).map_err(|e| format!("Cannot replace {}: {e}", bundle.display()))?;
        if let Err(err) = std::fs::rename(&new, &bundle) {
            let _ = std::fs::rename(&old, &bundle);
            let _ = std::fs::remove_dir_all(&staging);
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
            let _ = std::fs::rename(&bundle, &new);
            let _ = std::fs::rename(&old, &bundle);
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("Cannot start the restart helper: {err}"));
        }
        Ok(())
    }
}

/// Look for an update; `manual` says so when there is none (or it fails).
pub fn check_in_window(manual: bool, window: &mut Window, cx: &mut App) {
    if !enabled() {
        if manual {
            let why = if cfg!(debug_assertions) { "Development builds do not update." } else { "This build has no update key." };
            crate::toast::push(window, why, cx);
        }
        return;
    }
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = cx.background_spawn(async { check() }).await;
        _ = handle.update(cx, |_, window, cx| match result {
            Ok(Some(release)) => offer(release, window, cx),
            Ok(None) if manual => crate::toast::push(window, format!("den {} is the newest version.", current_version()), cx),
            Err(err) if manual => crate::toast::push(window, err, cx),
            _ => {}
        });
    })
    .detach();
}

/// Ask to install `release`.
fn offer(release: Release, window: &mut Window, cx: &mut App) {
    let notes = if release.notes.trim().is_empty() { String::new() } else { format!("\n\n{}", release.notes.trim()) };
    let text = format!("den {} is available (this is {}).{notes}", release.version, current_version());
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let release = release.clone();
        dialog.title("Update").description(text.clone()).ok_text("Install and Restart").show_cancel(true).on_ok(move |_, window, cx| {
            install(release.clone(), window, cx);
            true
        })
    });
}

/// Download, check, stage the new version and quit; it starts again by itself.
fn install(release: Release, window: &mut Window, cx: &mut App) {
    crate::toast::push(window, format!("Downloading den {}…", release.version), cx);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = cx.background_spawn(async move { download(&release).and_then(|file| stage(&file)) }).await;
        _ = handle.update(cx, |_, window, cx| match result {
            // Quitting saves every session; the restart waits for this process.
            Ok(()) => cx.quit(),
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
