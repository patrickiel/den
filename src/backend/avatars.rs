//! Commit authors' GitHub avatars, for the commit list's hover cards.
//!
//! An avatar is looked up by the author's email: GitHub's noreply addresses
//! name the user, any other address is asked of the commit's page on the
//! GitHub API when the repository's remote is there. Images are kept in
//! `%APPDATA%\den\avatars` (den's views show images from disk only), and a
//! miss is remembered for a week so an unknown address is not asked again on
//! every start. Everything here blocks; call it off the UI thread.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

/// Found images, and misses, by email key.
static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
/// After the GitHub API refused (its rate limit), when to ask it again.
static API_BLOCKED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// A remembered miss grows stale after this.
const NEGATIVE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const API_BACKOFF: Duration = Duration::from_secs(3600);
const MAX_BYTES: u64 = 512 * 1024;
/// How big the images are asked for (shown at 20 pixels, on a 2x screen).
const SIZE: u32 = 64;

pub fn dir() -> PathBuf {
    crate::settings::data_dir().join("avatars")
}

fn cache() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    CACHE.get_or_init(Default::default)
}

/// A file name for an email: a hash, so the address stays off the disk.
fn key(email: &str) -> String {
    let digest = Sha256::digest(email.trim().to_lowercase().as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The avatar URL GitHub's own addresses name: `NNN+user@users.noreply.github.com`
/// by id, `user@users.noreply.github.com` by name, and the GitHub Actions bot.
pub fn github_avatar_url(email: &str) -> Option<String> {
    let email = email.trim().to_lowercase();
    if email == "noreply@github.com" {
        // GitHub itself (merges made on the site, GitHub Actions).
        return Some(format!("https://avatars.githubusercontent.com/in/15368?s={SIZE}"));
    }
    let user = email.strip_suffix("@users.noreply.github.com")?;
    let (id, name) = match user.split_once('+') {
        Some((id, name)) => (Some(id), name),
        None => (None, user),
    };
    match id {
        Some(id) if id.chars().all(|c| c.is_ascii_digit()) => Some(format!("https://avatars.githubusercontent.com/u/{id}?s={SIZE}")),
        _ => (!name.is_empty()).then(|| format!("https://avatars.githubusercontent.com/{name}?s={SIZE}")),
    }
}

/// The avatar as already known: the image, a remembered miss, or `None`
/// when it was never looked up (or the miss grew stale).
pub fn cached(email: &str) -> Option<Option<PathBuf>> {
    let key = key(email);
    if let Some(known) = cache().lock().ok().and_then(|cache| cache.get(&key).cloned()) {
        return Some(known);
    }
    let image = dir().join(format!("{key}.img"));
    if image.is_file() {
        remember(&key, Some(image.clone()));
        return Some(Some(image));
    }
    let miss = dir().join(format!("{key}.none"));
    let fresh = std::fs::metadata(&miss).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age < NEGATIVE_TTL));
    if fresh {
        remember(&key, None);
        return Some(None);
    }
    None
}

fn remember(key: &str, found: Option<PathBuf>) {
    if let Ok(mut cache) = cache().lock() {
        cache.insert(key.to_string(), found);
    }
}

/// Look the avatar up and keep it: by the email alone, else through the
/// commit `hash` in the GitHub repository `(owner, repo)`. A miss on the
/// API's rate limit is kept for the session only; any other miss, for a
/// week.
pub fn fetch(email: &str, github: Option<(String, String, String)>) -> Option<PathBuf> {
    if let Some(known) = cached(email) {
        return known;
    }
    let key = key(email);
    let (url, definitive) = match github_avatar_url(email) {
        Some(url) => (Some(url), true),
        None => match github {
            Some((owner, repo, hash)) => match api_avatar_url(&owner, &repo, &hash) {
                Ok(url) => (url, true),
                Err(()) => (None, false),
            },
            None => (None, true),
        },
    };
    let found = url.and_then(|url| download(&url, &dir().join(format!("{key}.img"))).ok());
    if found.is_none() && definitive {
        let _ = std::fs::create_dir_all(dir());
        let _ = std::fs::write(dir().join(format!("{key}.none")), "");
    }
    remember(&key, found.clone());
    found
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(20))
        .redirects(8)
        .build()
}

/// The commit author's avatar URL from the GitHub API: `Ok(None)` when the
/// email is linked to no account, `Err` when the API could not be asked
/// (the rate limit, no network) and the miss must not be remembered.
fn api_avatar_url(owner: &str, repo: &str, hash: &str) -> Result<Option<String>, ()> {
    if API_BLOCKED_UNTIL.lock().ok().and_then(|until| *until).is_some_and(|until| Instant::now() < until) {
        return Err(());
    }
    let url = format!("https://api.github.com/repos/{owner}/{repo}/commits/{hash}");
    let response = agent().get(&url).set("Accept", "application/vnd.github+json").set("User-Agent", "den").call();
    match response {
        Ok(response) => {
            let json: serde_json::Value = response.into_json().map_err(drop)?;
            let avatar = json.get("author").and_then(|a| a.get("avatar_url")).and_then(|u| u.as_str());
            Ok(avatar.map(|u| if u.contains('?') { format!("{u}&s={SIZE}") } else { format!("{u}?s={SIZE}") }))
        }
        Err(ureq::Error::Status(403 | 429, _)) => {
            if let Ok(mut until) = API_BLOCKED_UNTIL.lock() {
                *until = Some(Instant::now() + API_BACKOFF);
            }
            Err(())
        }
        Err(ureq::Error::Status(404 | 422, _)) => Ok(None),
        Err(_) => Err(()),
    }
}

/// Fetch `url` to `path`, through a temporary file so a half-written image
/// never shows.
fn download(url: &str, path: &Path) -> Result<PathBuf, String> {
    let response = agent().get(url).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(response.into_reader(), MAX_BYTES), &mut bytes).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err("empty".into());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let part = path.with_extension("part");
    std::fs::write(&part, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(part, path).map_err(|e| e.to_string())?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::{github_avatar_url, key};

    #[test]
    fn github_addresses_name_their_avatar() {
        assert_eq!(github_avatar_url("12345+octocat@users.noreply.github.com").as_deref(), Some("https://avatars.githubusercontent.com/u/12345?s=64"));
        assert_eq!(github_avatar_url("octocat@users.noreply.github.com").as_deref(), Some("https://avatars.githubusercontent.com/octocat?s=64"));
        assert_eq!(github_avatar_url("41898282+github-actions[bot]@users.noreply.github.com").as_deref(), Some("https://avatars.githubusercontent.com/u/41898282?s=64"));
        assert_eq!(github_avatar_url("noreply@github.com").as_deref(), Some("https://avatars.githubusercontent.com/in/15368?s=64"));
        assert_eq!(github_avatar_url("someone@example.com"), None);
        assert_eq!(github_avatar_url("@users.noreply.github.com"), None);
    }

    #[test]
    fn keys_are_stable_and_safe_file_names() {
        assert_eq!(key("A@b.com"), key(" a@B.com "));
        assert_ne!(key("a@b.com"), key("c@b.com"));
        assert_eq!(key("a@b.com").len(), 16);
        assert!(key("a@b.com").chars().all(|c| c.is_ascii_hexdigit()));
    }
}
