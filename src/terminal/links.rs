//! Links in terminal output: URLs, and paths to files that exist, with an
//! optional position (`path:line:col`, `path(line,col)`), as in den.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Url(String),
    File { path: PathBuf, line: Option<u32>, column: Option<u32> },
}

/// Characters that end a token; parentheses stay, for `path(12,3)`.
fn is_break(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|')
}

const LEADING: &[char] = &['(', '[', '{', '"', '\''];
const TRAILING: &[char] = &['.', ',', ';', ':', ')', ']', '}', '"', '\'', '!', '?'];

static POSITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<path>.+?)(?::(?P<line>\d+)(?::(?P<col>\d+))?|\((?P<pline>\d+)(?:,\s*(?P<pcol>\d+))?\))?$").expect("valid regex")
});

/// The link in `line` (one character per cell) that covers column `col`,
/// with the columns it spans. Relative paths resolve against `cwd`; a path
/// counts only when `exists` says so.
pub fn find(line: &[char], col: usize, cwd: &Path, exists: impl Fn(&Path) -> bool) -> Option<(Range<usize>, Link)> {
    if col >= line.len() || is_break(line[col]) {
        return None;
    }
    let mut start = col;
    while start > 0 && !is_break(line[start - 1]) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < line.len() && !is_break(line[end]) {
        end += 1;
    }
    while start < end && LEADING.contains(&line[start]) {
        start += 1;
    }
    if start >= end || col < start {
        return None;
    }

    let token: String = line[start..end].iter().collect();
    if token.starts_with("http://") || token.starts_with("https://") {
        let trimmed = token.trim_end_matches(TRAILING);
        let len = trimmed.chars().count();
        return (col < start + len).then(|| (start..start + len, Link::Url(trimmed.to_string())));
    }

    // As written first, then without trailing punctuation.
    let mut candidate = token.as_str();
    loop {
        if let Some(link) = file_link(candidate, cwd, &exists) {
            let len = candidate.chars().count();
            return (col < start + len).then(|| (start..start + len, link));
        }
        let shorter = candidate.strip_suffix(TRAILING);
        match shorter {
            Some(shorter) if !shorter.is_empty() => candidate = shorter,
            _ => return None,
        }
    }
}

fn file_link(token: &str, cwd: &Path, exists: &impl Fn(&Path) -> bool) -> Option<Link> {
    let caps = POSITION.captures(token)?;
    let raw = caps.name("path")?.as_str();
    let number = |name: &str| caps.name(name).and_then(|m| m.as_str().parse::<u32>().ok());
    let line = number("line").or_else(|| number("pline"));
    let column = number("col").or_else(|| number("pcol"));
    let path = Path::new(raw);
    let path = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
    exists(&path).then_some(Link::File { path, line, column })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    fn at(text: &str, needle: &str) -> usize {
        text.find(needle).expect("needle in text")
    }

    fn cwd() -> PathBuf {
        PathBuf::from("C:/work")
    }

    fn exists(known: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |path| known.iter().any(|k| path == Path::new(k))
    }

    #[test]
    fn urls_lose_trailing_punctuation() {
        let text = "see https://example.com/a?b=1, then";
        let (range, link) = find(&chars(text), at(text, "example"), &cwd(), |_| false).unwrap();
        assert_eq!(link, Link::Url("https://example.com/a?b=1".into()));
        assert_eq!(range, at(text, "https")..at(text, ","));
    }

    #[test]
    fn a_relative_path_with_a_position() {
        let text = "error at src/main.rs:12:5: oops";
        let (range, link) = find(&chars(text), at(text, "main"), &cwd(), exists(&["C:/work/src/main.rs"])).unwrap();
        assert_eq!(
            link,
            Link::File { path: "C:/work/src/main.rs".into(), line: Some(12), column: Some(5) }
        );
        assert_eq!(range, at(text, "src")..at(text, ": oops"));
    }

    #[test]
    fn the_parenthesised_position_form() {
        let text = "src/app.ts(3,14): error TS2304";
        let (_, link) = find(&chars(text), 2, &cwd(), exists(&["C:/work/src/app.ts"])).unwrap();
        assert_eq!(link, Link::File { path: "C:/work/src/app.ts".into(), line: Some(3), column: Some(14) });
    }

    #[test]
    fn an_absolute_windows_path_keeps_its_drive() {
        let text = r"at C:\code\x.rs:7";
        let (_, link) = find(&chars(text), at(text, "code"), &cwd(), exists(&[r"C:\code\x.rs"])).unwrap();
        assert_eq!(link, Link::File { path: r"C:\code\x.rs".into(), line: Some(7), column: None });
    }

    #[test]
    fn a_path_in_parentheses() {
        let text = "(see docs/guide.md)";
        let (_, link) = find(&chars(text), at(text, "guide"), &cwd(), exists(&["C:/work/docs/guide.md"])).unwrap();
        assert_eq!(link, Link::File { path: "C:/work/docs/guide.md".into(), line: None, column: None });
    }

    #[test]
    fn missing_files_are_not_links() {
        let text = "nothing/here.rs:3";
        assert!(find(&chars(text), 2, &cwd(), |_| false).is_none());
    }

    #[test]
    fn blank_cells_are_not_links() {
        let text = "a  b";
        assert!(find(&chars(text), 1, &cwd(), |_| true).is_none());
    }
}
