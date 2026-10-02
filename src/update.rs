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

use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

const MANIFEST_URL: &str = "https://github.com/patrickiel/den/releases/latest/download/latest.json";
const PUBLIC_KEY: &str = include_str!("../packaging/updater.pub");
const PLATFORM: &str = "windows-x86_64";

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
fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split(['.', '-']).take(3).map(|n| n.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(a) > parse(b)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(60))
        .redirects(8)
        .build()
}

/// The newest release if it is newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let manifest: serde_json::Value = agent()
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
    Ok(Some(Release {
        version,
        notes: manifest["notes"].as_str().unwrap_or_default().to_string(),
        url: platform["url"].as_str().ok_or("Bad update manifest: no installer for Windows")?.to_string(),
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

/// Download and check the installer; its path.
fn download(release: &Release) -> Result<PathBuf, String> {
    let response = agent().get(&release.url).call().map_err(|e| format!("Download failed: {e}"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes).map_err(|e| format!("Download failed: {e}"))?;
    verify(&bytes, &release.signature)?;
    let path = std::env::temp_dir().join(format!("den_{}_setup.exe", release.version));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
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

/// Download, check, run the installer silently and quit; it starts den again.
fn install(release: Release, window: &mut Window, cx: &mut App) {
    crate::toast::push(window, format!("Downloading den {}…", release.version), cx);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let result = cx.background_spawn(async move { download(&release) }).await;
        _ = handle.update(cx, |_, window, cx| match result {
            Ok(setup) => match std::process::Command::new(&setup).args(["/S", "/UPDATE"]).spawn() {
                // Quitting saves every session; the installer waits for the exe.
                Ok(_) => cx.quit(),
                Err(err) => crate::toast::push(window, format!("Cannot start the installer: {err}"), cx),
            },
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
