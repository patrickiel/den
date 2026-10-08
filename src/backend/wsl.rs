//! WSL: folders inside a distribution, as Windows sees them
//! (`\\wsl.localhost\Ubuntu\home\me`) and as the distribution does
//! (`/home/me`), and running programs there through `wsl.exe`.

use std::path::{Path, PathBuf};

/// The distribution and Linux path of a folder inside WSL:
/// `\\wsl.localhost\Ubuntu\home\me` (or `\\wsl$\…`, `\\?\UNC\wsl.localhost\…`)
/// → `("Ubuntu", "/home/me")`.
pub fn split(path: &Path) -> Option<(String, String)> {
    let text = path.to_string_lossy();
    let rest = text.strip_prefix(r"\\?\UNC\").or_else(|| text.strip_prefix(r"\\"))?;
    let (server, rest) = rest.split_once('\\')?;
    if !server.eq_ignore_ascii_case("wsl.localhost") && !server.eq_ignore_ascii_case("wsl$") {
        return None;
    }
    let (distro, rest) = rest.split_once('\\').unwrap_or((rest, ""));
    if distro.is_empty() {
        return None;
    }
    let linux = format!("/{}", rest.trim_end_matches('\\').replace('\\', "/"));
    Some((distro.to_string(), linux))
}

/// A Linux path as Windows reaches it: `/mnt/c/x` → `C:\x`, anything else
/// under `\\wsl.localhost\<distro>`; `None` for that with no distro known.
pub fn to_windows(distro: Option<&str>, linux: &str) -> Option<PathBuf> {
    let bytes = linux.as_bytes();
    if linux.starts_with("/mnt/") && bytes.len() >= 6 && bytes[5].is_ascii_alphabetic() && bytes.get(6).is_none_or(|&b| b == b'/') {
        let drive = (bytes[5] as char).to_ascii_uppercase();
        return Some(PathBuf::from(format!(r"{drive}:\{}", linux.get(7..).unwrap_or("").replace('/', "\\"))));
    }
    let distro = distro?;
    Some(PathBuf::from(format!(r"\\wsl.localhost\{distro}{}", linux.trim_end_matches('/').replace('/', "\\"))))
}

/// The shell setting names WSL (`wsl`, `wsl.exe -d Ubuntu`): `Some` with the
/// distribution it picks, if any.
pub fn shell(setting: &str) -> Option<Option<String>> {
    let mut words = setting.split_whitespace();
    let exe = words.next()?;
    let name = exe.rsplit(['\\', '/']).next().unwrap_or(exe).to_lowercase();
    if name != "wsl" && name != "wsl.exe" {
        return None;
    }
    let mut distro = None;
    while let Some(word) = words.next() {
        if word == "-d" || word == "--distribution" {
            distro = words.next().map(str::to_string);
        }
    }
    Some(distro)
}

/// The distribution `wsl.exe` starts when none is named.
#[cfg(windows)]
pub fn default_distro() -> Option<String> {
    let lxss = windows_registry::CURRENT_USER.open(r"Software\Microsoft\Windows\CurrentVersion\Lxss").ok()?;
    let id = lxss.get_string("DefaultDistribution").ok()?;
    lxss.open(&id).and_then(|k| k.get_string("DistributionName")).ok()
}

#[cfg(not(windows))]
pub fn default_distro() -> Option<String> {
    None
}

/// `WSLENV` with `names` added: the variables that cross into WSL (`/p`
/// turns a Windows path into a Linux one).
pub fn wslenv(names: &[&str]) -> String {
    let current = std::env::var("WSLENV").unwrap_or_default();
    current.split(':').filter(|s| !s.is_empty()).chain(names.iter().copied()).collect::<Vec<_>>().join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_wsl_folders() {
        let at = |p: &str| split(Path::new(p));
        assert_eq!(at(r"\\wsl.localhost\Ubuntu\home\me"), Some(("Ubuntu".into(), "/home/me".into())));
        assert_eq!(at(r"\\?\UNC\wsl.localhost\Ubuntu\home\me\"), Some(("Ubuntu".into(), "/home/me".into())));
        assert_eq!(at(r"\\WSL$\Debian"), Some(("Debian".into(), "/".into())));
        assert_eq!(at(r"\\server\share\x"), None);
        assert_eq!(at(r"C:\Users\me"), None);
    }

    #[test]
    fn linux_paths_on_windows() {
        assert_eq!(to_windows(Some("Ubuntu"), "/home/me"), Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\home\me")));
        assert_eq!(to_windows(Some("Ubuntu"), "/"), Some(PathBuf::from(r"\\wsl.localhost\Ubuntu")));
        assert_eq!(to_windows(None, "/mnt/c/Users/me"), Some(PathBuf::from(r"C:\Users\me")));
        assert_eq!(to_windows(None, "/mnt/d"), Some(PathBuf::from(r"D:\")));
        assert_eq!(to_windows(Some("U"), "/mnt/wsl"), Some(PathBuf::from(r"\\wsl.localhost\U\mnt\wsl")));
        assert_eq!(to_windows(None, "/home/me"), None);
    }

    #[test]
    fn wsl_shell_settings() {
        assert_eq!(shell("wsl"), Some(None));
        assert_eq!(shell(r"C:\Windows\System32\WSL.EXE -d Ubuntu"), Some(Some("Ubuntu".into())));
        assert_eq!(shell("wsl --distribution Debian"), Some(Some("Debian".into())));
        assert_eq!(shell("pwsh.exe"), None);
        assert_eq!(shell(""), None);
    }
}
