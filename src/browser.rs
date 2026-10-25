//! Pages open in the system's browser: presets, the dev server, and what
//! extensions ask to show. A preset's address can be typed loosely: a host
//! opens as https, `localhost` as http, anything else is a web search.

use gpui_kit::App;

/// Open `url`, as typed, in the system's browser.
pub fn open(url: &str, cx: &App) {
    let url = normalize_url(url);
    if !url.is_empty() {
        cx.open_url(&url);
    }
}

/// Open the file at `path` in the system's browser, as it is saved.
pub fn open_file(path: &std::path::Path, cx: &App) {
    cx.open_url(&file_url(path));
}

/// `/home/x/a b.html` → `file:///home/x/a%20b.html`; `C:\x\a.html` →
/// `file:///C:/x/a.html`.
fn file_url(path: &std::path::Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let path = path.trim_start_matches('/');
    let encoded: Vec<String> = path.split('/').map(encode).collect();
    format!("file:///{}", encoded.join("/"))
}

/// Turn what was typed into something to load: a URL, a host, or a web
/// search.
fn normalize_url(input: &str) -> String {
    let text = input.trim();
    if text.is_empty() {
        return String::new();
    }
    let lower = text.to_lowercase();
    // "localhost:3000" looks like a scheme too; only "x://" and the schemes
    // without slashes count.
    let scheme = lower.split_once("://").is_some_and(|(s, _)| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    if scheme
        || ["about:", "data:", "mailto:", "javascript:"]
            .iter()
            .any(|p| lower.starts_with(p))
    {
        return text.to_string();
    }
    let search = || format!("https://www.google.com/search?q={}", encode(text));
    if text.contains(char::is_whitespace) {
        return search();
    }
    let host = text.split('/').next().unwrap_or("");
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
        .map_or(host, |(h, _)| h);
    let loopback = host.starts_with("127.") && host.split('.').count() == 4;
    if host == "localhost" || loopback {
        return format!("http://{text}");
    }
    if host.split('.').count() >= 2 && host.split('.').all(|part| !part.is_empty()) {
        return format!("https://{text}");
    }
    search()
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            // A Windows drive, "C:", stays as it is.
            b':' => ":".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{file_url, normalize_url};

    #[test]
    fn urls_hosts_and_searches() {
        assert_eq!(normalize_url("https://x.dev/a"), "https://x.dev/a");
        assert_eq!(normalize_url("localhost:5173"), "http://localhost:5173");
        assert_eq!(normalize_url("127.0.0.1:8080/x"), "http://127.0.0.1:8080/x");
        assert_eq!(
            normalize_url("example.com/docs"),
            "https://example.com/docs"
        );
        assert_eq!(
            normalize_url("rust gpui"),
            "https://www.google.com/search?q=rust%20gpui"
        );
        assert_eq!(normalize_url("about:blank"), "about:blank");
        assert_eq!(normalize_url("  "), "");
    }

    #[test]
    fn file_urls() {
        assert_eq!(
            file_url(std::path::Path::new("/home/x/a b.html")),
            "file:///home/x/a%20b.html"
        );
        assert_eq!(
            file_url(std::path::Path::new("C:\\x\\a.html")),
            "file:///C:/x/a.html"
        );
    }
}
