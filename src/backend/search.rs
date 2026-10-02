//! Text search for the Search view, ported from den's backend. Walks the session root in
//! parallel with ripgrep's walker (the `ignore` crate, so .gitignore and .ignore files apply),
//! matches each file line by line with the `regex` crate and hands the files that have matches to a
//! callback in batches.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Larger files are skipped: the editor refuses them too, so nothing could be replaced in them.
const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// A NUL in the first chunk marks a binary file.
const SNIFF_BYTES: usize = 8 * 1024;
/// Characters kept before a match for the results list; a longer lead-in is cut, with "…" in front.
const CHARS_BEFORE: usize = 30;
/// Characters kept of the match and the text after it, together.
const CHARS_AFTER: usize = 250;
/// VS Code's `files.exclude` and `search.exclude` defaults, applied together with the ignore files.
const DEFAULT_EXCLUDES: &[&str] = &[
    "**/.git", "**/.svn", "**/.hg", "**/CVS", "**/.DS_Store", "**/Thumbs.db", "**/node_modules", "**/bower_components",
    "**/*.code-search",
];
/// Results go out when this many files have matched, or when this long has passed since the last batch.
const BATCH_FILES: usize = 64;
const BATCH_INTERVAL: Duration = Duration::from_millis(60);

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Query {
    pub pattern: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// "files to include" / "files to exclude" as typed: comma-separated globs.
    pub include: String,
    pub exclude: String,
    /// Honour .gitignore and .ignore files, and skip DEFAULT_EXCLUDES.
    pub use_ignore_files: bool,
    /// Search these files instead of walking the root (open editors, files that changed).
    pub paths: Option<Vec<String>>,
    pub max_results: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMatch {
    pub path: String,
    /// Relative to the root with `/` separators; the whole path for a file outside the root.
    pub rel: String,
    pub matches: Vec<LineMatch>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LineMatch {
    /// 1-based.
    pub line: usize,
    /// UTF-16 columns in the line (0-based, end exclusive), as the editor and JavaScript count.
    pub start: usize,
    pub end: usize,
    /// The line for the results list: a shortened lead-in, the match, and the text after it.
    pub before: String,
    pub text: String,
    pub after: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Done {
    /// There were more matches than `max_results`; the search stopped there.
    pub limit_hit: bool,
}

/// Search `root` (or `query.paths`), handing each batch of files with matches to `on_files`
/// (which returns false to stop). Err for a pattern or glob that does not parse.
pub fn search(root: &Path, query: &Query, cancel: &AtomicBool, on_files: impl FnMut(Vec<FileMatch>) -> bool) -> Result<Done, String> {
    let search = Search::new(&root.to_string_lossy(), query)?;
    let budget = Budget::new(query.max_results);
    search.run(query.paths.as_deref(), &budget, cancel, on_files);
    Ok(Done { limit_hit: budget.spent() })
}

/// The matches in `text` (an editor's unsaved buffer), as `search` reports them for a file.
#[allow(dead_code)]
pub fn search_text(query: &Query, text: &str) -> Result<Vec<LineMatch>, String> {
    let matcher = Matcher::new(query)?;
    Ok(matcher.find(text, &Budget::new(query.max_results)))
}

/// Replace every match in `text`, line by line as the search finds them: in
/// regex mode the replacement takes `$1`, `$<name>`, `$0`, `\n` and `\t`;
/// otherwise it is inserted as typed. The new text and how many matches it replaced.
pub fn replace(query: &Query, text: &str, replacement: &str) -> Result<(String, usize), String> {
    let matcher = Matcher::new(query)?;
    let replacement = if query.is_regex {
        replacement.replace("\\n", "\n").replace("\\t", "\t")
    } else {
        replacement.to_string()
    };
    let mut out = String::with_capacity(text.len());
    let mut count = 0;
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let body = body.strip_suffix('\r').unwrap_or(body);
        let ending = &line[body.len()..];
        let mut last = 0;
        for caps in matcher.re.captures_iter(body) {
            let m = caps.get(0).expect("group 0");
            if m.is_empty() || (matcher.whole_word && !word_bounded(body, m.start(), m.end())) {
                continue;
            }
            out.push_str(&body[last..m.start()]);
            if query.is_regex {
                caps.expand(&replacement, &mut out);
            } else {
                out.push_str(&replacement);
            }
            last = m.end();
            count += 1;
        }
        out.push_str(&body[last..]);
        out.push_str(ending);
    }
    Ok((out, count))
}

// ---------- the search ----------

struct Search {
    root: PathBuf,
    matcher: Matcher,
    /// Shared with the walker's entry filter, which must be 'static.
    filter: Arc<Filter>,
    use_ignore_files: bool,
}

impl Search {
    fn new(root: &str, query: &Query) -> Result<Self, String> {
        let root = PathBuf::from(root);
        Ok(Search {
            matcher: Matcher::new(query)?,
            filter: Arc::new(Filter::new(&root, query)?),
            root,
            use_ignore_files: query.use_ignore_files,
        })
    }

    /// Search on worker threads while this one batches what they find.
    fn run(&self, paths: Option<&[String]>, budget: &Budget, cancel: &AtomicBool, mut out: impl FnMut(Vec<FileMatch>) -> bool) {
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(move || match paths {
                Some(paths) => {
                    for p in paths {
                        if cancel.load(Ordering::Relaxed) || budget.spent() {
                            break;
                        }
                        self.visit(Path::new(p), budget, &tx);
                    }
                }
                None => self.walk(budget, cancel, tx),
            });
            let mut batch = Vec::new();
            let mut sent = Instant::now();
            loop {
                let closed = match rx.recv_timeout(BATCH_INTERVAL) {
                    Ok(file) => {
                        batch.push(file);
                        false
                    }
                    Err(RecvTimeoutError::Timeout) => false,
                    Err(RecvTimeoutError::Disconnected) => true,
                };
                let due = closed || batch.len() >= BATCH_FILES || sent.elapsed() >= BATCH_INTERVAL;
                if due && !batch.is_empty() && !cancel.load(Ordering::Relaxed) {
                    // The window went away: stop searching for it.
                    if !out(std::mem::take(&mut batch)) {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    sent = Instant::now();
                }
                if closed {
                    break;
                }
            }
        });
    }

    fn walk(&self, budget: &Budget, cancel: &AtomicBool, tx: mpsc::Sender<FileMatch>) {
        let ignore = self.use_ignore_files;
        let mut builder = WalkBuilder::new(&self.root);
        builder
            .hidden(false) // dotfiles are searched, as in VS Code
            .parents(false) // ignore files above the root do not apply (VS Code's search.useParentIgnoreFiles)
            .git_global(false)
            .ignore(ignore)
            .git_ignore(ignore)
            .git_exclude(ignore)
            .require_git(false) // a .gitignore counts outside a repository too
            .follow_links(true); // the walker detects link loops
        let filter = self.filter.clone();
        builder.filter_entry(move |e| e.depth() == 0 || !filter.excluded(e.path()));
        builder.build_parallel().run(|| {
            let tx = tx.clone();
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) || budget.spent() {
                    return WalkState::Quit;
                }
                if let Ok(entry) = entry {
                    if entry.file_type().is_some_and(|t| t.is_file()) {
                        self.visit(entry.path(), budget, &tx);
                    }
                }
                WalkState::Continue
            })
        });
    }

    fn visit(&self, path: &Path, budget: &Budget, tx: &mpsc::Sender<FileMatch>) {
        let rel = self.filter.rel(path);
        if !self.filter.included(&rel) || self.filter.exclude.is_match(&rel) {
            return;
        }
        let matches = search_file(path, &self.matcher, budget);
        if !matches.is_empty() {
            let _ = tx.send(FileMatch { path: path.to_string_lossy().into_owned(), rel, matches });
        }
    }
}

/// The matches in one file; none for files that are too big, binary or unreadable.
fn search_file(path: &Path, matcher: &Matcher, budget: &Budget) -> Vec<LineMatch> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_BYTES => {}
        _ => return Vec::new(),
    }
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    if bytes.iter().take(SNIFF_BYTES).any(|&b| b == 0) {
        return Vec::new();
    }
    matcher.find(&String::from_utf8_lossy(&bytes), budget)
}

/// Stops the search after `max_results` matches.
struct Budget {
    left: AtomicUsize,
    hit: AtomicBool,
}

impl Budget {
    fn new(max: usize) -> Self {
        Budget { left: AtomicUsize::new(max), hit: AtomicBool::new(false) }
    }

    /// Count one more match; false once the limit is reached, and the match is not reported.
    fn take(&self) -> bool {
        if self.left.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_ok() {
            return true;
        }
        self.hit.store(true, Ordering::Relaxed);
        false
    }

    fn spent(&self) -> bool {
        self.hit.load(Ordering::Relaxed)
    }
}

// ---------- matching ----------

struct Matcher {
    re: Regex,
    whole_word: bool,
}

impl Matcher {
    fn new(query: &Query) -> Result<Self, String> {
        let source = if query.is_regex { query.pattern.clone() } else { regex::escape(&query.pattern) };
        let re = RegexBuilder::new(&source)
            .case_insensitive(!query.case_sensitive)
            // ^ and $ at line ends, CRLF included, so the whole-text check below agrees with the lines.
            .multi_line(true)
            .crlf(true)
            .build()
            .map_err(|e| {
                // The parser's message draws the pattern with a caret; its last line says what is wrong.
                let text = e.to_string();
                let last = text.lines().last().unwrap_or_default();
                format!("Invalid regular expression: {}", last.trim_start_matches("error: "))
            })?;
        Ok(Matcher { re, whole_word: query.whole_word })
    }

    /// Every match, line by line: a match never spans lines, as in the editor.
    fn find(&self, text: &str, budget: &Budget) -> Vec<LineMatch> {
        let mut out = Vec::new();
        // The editor drops a byte order mark, so columns on the first line must not count it.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        // Most files have no match at all: one pass over the whole text rules them out.
        if !self.re.is_match(text) {
            return out;
        }
        for (i, line) in text.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let mut cols = Utf16Cols { line, byte: 0, col: 0 };
            for m in self.re.find_iter(line) {
                if m.is_empty() || (self.whole_word && !word_bounded(line, m.start(), m.end())) {
                    continue;
                }
                if !budget.take() {
                    return out;
                }
                let (before, text, after) = preview(line, m.start(), m.end());
                out.push(LineMatch { line: i + 1, start: cols.at(m.start()), end: cols.at(m.end()), before, text, after });
            }
        }
        out
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whole word, as the editor's find has it: no word character runs on across either end of the match.
fn word_bounded(line: &str, start: usize, end: usize) -> bool {
    let m = &line[start..end];
    let joined = |outside: Option<char>, edge: Option<char>| outside.is_some_and(is_word) && edge.is_some_and(is_word);
    !joined(line[..start].chars().next_back(), m.chars().next()) && !joined(line[end..].chars().next(), m.chars().next_back())
}

/// UTF-16 columns of byte offsets into a line, for offsets that only grow.
struct Utf16Cols<'a> {
    line: &'a str,
    byte: usize,
    col: usize,
}

impl Utf16Cols<'_> {
    fn at(&mut self, byte: usize) -> usize {
        self.col += self.line[self.byte..byte].chars().map(char::len_utf16).sum::<usize>();
        self.byte = byte;
        self.col
    }
}

/// The match with the text around it: the lead-in without its indentation, cut to its last
/// CHARS_BEFORE characters, then the match and the rest of the line up to CHARS_AFTER characters.
fn preview(line: &str, start: usize, end: usize) -> (String, String, String) {
    let lead = line[..start].trim_start();
    let n = lead.chars().count();
    let before = if n > CHARS_BEFORE {
        format!("…{}", lead.chars().skip(n - CHARS_BEFORE).collect::<String>())
    } else {
        lead.to_string()
    };
    let text: String = line[start..end].chars().take(CHARS_AFTER).collect();
    let room = CHARS_AFTER.saturating_sub(text.chars().count());
    let after = line[end..].chars().take(room).collect::<String>().trim_end().to_string();
    (before, text, after)
}

// ---------- files to include / exclude ----------

struct Filter {
    root: PathBuf,
    /// None: every file.
    include: Option<GlobSet>,
    exclude: GlobSet,
}

impl Filter {
    fn new(root: &Path, query: &Query) -> Result<Self, String> {
        let include = expand_globs(&query.include, root);
        let mut exclude = expand_globs(&query.exclude, root);
        if query.use_ignore_files {
            exclude.extend(DEFAULT_EXCLUDES.iter().flat_map(|g| [(g.to_string(), g.to_string()), (g.to_string(), format!("{g}/**"))]));
        }
        Ok(Filter {
            root: root.to_path_buf(),
            include: if include.is_empty() { None } else { Some(glob_set(&include)?) },
            exclude: glob_set(&exclude)?,
        })
    }

    /// `path` relative to the root with `/` separators; a path outside the root stays whole.
    fn rel(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(rel) => rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"),
            Err(_) => path.to_string_lossy().replace('\\', "/"),
        }
    }

    fn excluded(&self, path: &Path) -> bool {
        self.exclude.is_match(self.rel(path))
    }

    fn included(&self, rel: &str) -> bool {
        self.include.as_ref().is_none_or(|set| set.is_match(rel))
    }
}

/// Globs as `(what was typed, glob)`, so an error can name the part at fault.
fn glob_set(globs: &[(String, String)]) -> Result<GlobSet, String> {
    let mut set = GlobSetBuilder::new();
    for (raw, glob) in globs {
        let glob = GlobBuilder::new(glob)
            .literal_separator(true) // `*` stays within a folder; `**` crosses them
            .case_insensitive(cfg!(windows))
            .build()
            .map_err(|e| format!("Invalid pattern '{raw}': {}", e.kind()))?;
        set.add(glob);
    }
    set.build().map_err(|e| e.to_string())
}

/// The "files to include / exclude" boxes read as VS Code reads them: comma-separated globs
/// (commas inside braces belong to the glob). `./src`, or an absolute path inside the root, is a
/// folder of the root; `.ts` means `*.ts`; any other glob matches at any depth, as a file or as a
/// folder with everything in it. Paths outside the root match nothing and are dropped.
fn expand_globs(input: &str, root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in split_globs(input) {
        let raw = raw.trim();
        let p = raw.replace('\\', "/");
        let p = p.trim_end_matches('/');
        if p.is_empty() {
            continue;
        }
        let anchored = if p == "." {
            Some(String::new())
        } else if let Some(rest) = p.strip_prefix("./") {
            Some(rest.trim_start_matches('/').to_string())
        } else if p == ".." || p.starts_with("../") {
            continue;
        } else if Path::new(p).is_absolute() {
            match Path::new(p).strip_prefix(root) {
                Ok(rel) => Some(rel.to_string_lossy().replace('\\', "/")),
                Err(_) => continue,
            }
        } else {
            None
        };
        let globs = match anchored {
            Some(rel) if rel.is_empty() => vec!["**".to_string()],
            Some(rel) => vec![rel.clone(), format!("{rel}/**")],
            None => {
                let p = if p.starts_with('.') { format!("*{p}") } else { p.to_string() };
                vec![format!("**/{p}"), format!("**/{p}/**")].into_iter().map(|g| g.replace("**/**", "**")).collect()
            }
        };
        out.extend(globs.into_iter().map(|g| (raw.to_string(), g)));
    }
    out
}

fn split_globs(input: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut depth = 0usize;
    for c in input.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().unwrap().push(c);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pattern: &str) -> Query {
        Query {
            pattern: pattern.into(),
            is_regex: false,
            case_sensitive: false,
            whole_word: false,
            include: String::new(),
            exclude: String::new(),
            use_ignore_files: true,
            paths: None,
            max_results: 100,
        }
    }

    fn filter(include: &str, exclude: &str) -> Filter {
        let mut q = query("x");
        q.include = include.into();
        q.exclude = exclude.into();
        Filter::new(Path::new("/root"), &q).unwrap()
    }

    #[test]
    fn globs_match_anywhere_as_files_or_folders() {
        let f = filter("*.ts, src/**/include", "");
        assert!(f.included("a.ts"));
        assert!(f.included("deep/down/a.ts"));
        assert!(!f.included("a.tsx"));
        assert!(f.included("pkg/src/x/include/y.rs"));
        assert!(f.included("pkg/src/include"));
        let f = filter("src", "");
        assert!(f.included("src/main.ts"));
        assert!(f.included("app/src/main.ts"));
        assert!(!f.included("source/main.ts"));
    }

    #[test]
    fn dot_prefix_and_anchored_paths() {
        let f = filter(".rs", "");
        assert!(f.included("a/b.rs"));
        let f = filter("./src", "");
        assert!(f.included("src/a.ts"));
        assert!(!f.included("app/src/a.ts"));
        let f = filter("*.{ts,js}", "");
        assert!(f.included("x.js") && f.included("x.ts") && !f.included("x.rs"));
    }

    #[test]
    fn default_and_typed_excludes() {
        let f = filter("", "dist");
        assert!(f.exclude.is_match("node_modules"));
        assert!(f.exclude.is_match("a/node_modules/b/c.js"));
        assert!(f.exclude.is_match(".git"));
        assert!(f.exclude.is_match("dist/a.js"));
        assert!(!f.exclude.is_match("src/a.js"));
    }

    #[test]
    fn columns_are_utf16_and_bom_is_skipped() {
        let m = Matcher::new(&query("b")).unwrap();
        let found = m.find("\u{feff}😀 b\r\nxb", &Budget::new(10));
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].line, found[0].start, found[0].end), (1, 3, 4));
        assert_eq!((found[1].line, found[1].start), (2, 1));
    }

    #[test]
    fn whole_word_and_case() {
        let mut q = query("foo");
        q.whole_word = true;
        let found = Matcher::new(&q).unwrap().find("foofoo foo Foo foo_", &Budget::new(10));
        assert_eq!(found.iter().map(|m| m.start).collect::<Vec<_>>(), vec![7, 11]);
        q.case_sensitive = true;
        let found = Matcher::new(&q).unwrap().find("foofoo foo Foo", &Budget::new(10));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn regex_anchors_work_per_line() {
        let mut q = query("^b$");
        q.is_regex = true;
        let found = Matcher::new(&q).unwrap().find("a\r\nb\r\nbb", &Budget::new(10));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        q.pattern = "(".into();
        assert!(Matcher::new(&q).is_err());
    }

    #[test]
    fn budget_stops_and_reports() {
        let budget = Budget::new(2);
        let found = Matcher::new(&query("a")).unwrap().find("aaa", &budget);
        assert_eq!(found.len(), 2);
        assert!(budget.spent());
    }

    /// The relative paths a search of `root` reports.
    fn found(root: &Path, q: Query) -> Vec<String> {
        let mut files = Vec::new();
        search(root, &q, &AtomicBool::new(false), |batch| {
            files.extend(batch.into_iter().map(|f| f.rel));
            true
        })
        .unwrap();
        files.sort();
        files
    }

    #[test]
    fn walks_the_root_with_ignore_files_and_globs() {
        let root = std::env::temp_dir().join(format!("den-search-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, text) in [
            ("a.txt", "hello world"),
            ("sub/b.rs", "fn hello() {}"),
            (".hidden/c.txt", "hello"),
            ("node_modules/x.js", "hello"),
            ("ignored.log", "hello"),
            (".gitignore", "*.log\n"),
            ("bin.dat", "hello\0binary"),
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let q = || query("hello");
        assert_eq!(found(&root, q()), [".hidden/c.txt", "a.txt", "sub/b.rs"]);
        assert_eq!(found(&root, Query { use_ignore_files: false, ..q() }), [".hidden/c.txt", "a.txt", "ignored.log", "node_modules/x.js", "sub/b.rs"]);
        assert_eq!(found(&root, Query { include: "*.rs".into(), ..q() }), ["sub/b.rs"]);
        assert_eq!(found(&root, Query { exclude: "sub, .hidden".into(), ..q() }), ["a.txt"]);
        let paths = ["a.txt", "ignored.log"].map(|p| root.join(p).to_string_lossy().into_owned()).to_vec();
        assert_eq!(found(&root, Query { paths: Some(paths), ..q() }), ["a.txt", "ignored.log"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replacing_keeps_line_endings_and_expands_groups() {
        let (text, n) = replace(&query("cat"), "a cat\r\nno\ncat cat", "dog").unwrap();
        assert_eq!((text.as_str(), n), ("a dog\r\nno\ndog dog", 3));
        let mut q = query(r"(\w+)@(\w+)");
        q.is_regex = true;
        let (text, n) = replace(&q, "me@home", r"$2:$1\t").unwrap();
        assert_eq!((text.as_str(), n), ("home:me\t", 1));
        let (text, _) = replace(&query("$1"), "x$1", "$2").unwrap();
        assert_eq!(text, "x$2");
        let mut q = query("foo");
        q.whole_word = true;
        let (text, n) = replace(&q, "foofoo foo", "bar").unwrap();
        assert_eq!((text.as_str(), n), ("foofoo bar", 1));
    }

    #[test]
    fn preview_cuts_long_lead_ins() {
        let line = format!("    {}needle tail   ", "x".repeat(40));
        let start = line.find("needle").unwrap();
        let (before, text, after) = preview(&line, start, start + 6);
        assert!(before.starts_with('…'));
        assert_eq!(before.chars().count(), CHARS_BEFORE + 1);
        assert_eq!(text, "needle");
        assert_eq!(after, " tail");
    }
}
