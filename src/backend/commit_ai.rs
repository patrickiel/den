//! Generated commit messages, as in den: list the changed files (staged, or
//! everything when nothing is staged, like Commit does), leave out lockfiles,
//! generated and binary files, and cap every file's diff. When it all fits
//! the model's context one request writes the message; when it does not, the
//! diffs are summarized in chunks first and the message is written from the
//! notes. The style is the repository's `.den/commit-style.md`, else one
//! derived from its history (and saved there), else the global one, with a
//! handful of recent messages as examples.

use std::{collections::HashSet, path::Path, sync::LazyLock};

use regex::Regex;
use serde_json::{Value, json};

use super::{
    ai::{self, AiConfig},
    git,
    http::{CANCELLED, Cancel},
};

/// Rough size of a token for code and English; low on purpose.
const CHARS_PER_TOKEN: usize = 3;
const OUTPUT_TOKENS: usize = 400;
const MAP_OUTPUT_TOKENS: usize = 180;
const MAX_CHUNKS: usize = 12;
const MAX_FILE_CHARS: usize = 12_000;
const UNTRACKED_PREVIEW: usize = 60;
const MAX_UNTRACKED_PREVIEWS: usize = 150;
const STAT_LINES: usize = 150;
const EXAMPLES: usize = 15;
const EXAMPLE_BODY_LINES: usize = 4;
const HUGE_FILE_LINES: usize = 2000;
pub const STYLE_FILE: &str = ".den/commit-style.md";
const DIFF_SLOT: &str = "{{diff}}";

const MAP_SYSTEM: &str = "You summarize one part of a code change. Reply with 2 to 6 terse bullet points (\"- ...\") saying what changed and, if evident, why. Name concrete features, functions or files. No introduction, no conclusion.";

/// Files whose content says little about the intent of a change.
static NOISE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)(^|/)(pnpm-lock\.yaml|package-lock\.json|yarn\.lock|bun\.lockb?|Cargo\.lock|composer\.lock|Gemfile\.lock|poetry\.lock|go\.sum|Pipfile\.lock|packages\.lock\.json)$",
        r"(?i)\.lock$",
        r"(?i)\.(min\.(js|css)|map|snap)$",
        r"(?i)(^|/)(dist|build|out|target|vendor|node_modules|\.next|coverage)/",
        r"(?i)\.(png|jpe?g|gif|webp|ico|icns|bmp|svg|pdf|zip|gz|7z|woff2?|ttf|otf|eot|mp[34]|wav|exe|dll|so|dylib|bin|gguf)$",
    ]
    .iter()
    .map(|r| Regex::new(r).expect("valid"))
    .collect()
});

/// A past commit message.
#[derive(Clone, Debug)]
pub struct Message {
    pub subject: String,
    pub body: String,
}

/// A changed file.
#[derive(Clone, Debug)]
struct Entry {
    path: String,
    old_path: Option<String>,
    status: char,
    add: usize,
    del: usize,
    binary: bool,
}

struct Changes {
    entries: Vec<Entry>,
    /// (path, diff text), largest changes first.
    diffs: Vec<(String, String)>,
    omitted: HashSet<String>,
}

// -- Style -------------------------------------------------------------------

/// The last `n` commits' messages.
fn recent_messages(top: &Path, n: usize) -> Vec<Message> {
    git::log(top, &["HEAD".into()], n, 0)
        .unwrap_or_default()
        .into_iter()
        .map(|c| Message { subject: c.subject.trim().to_string(), body: c.body })
        .filter(|m| !m.subject.is_empty())
        .collect()
}

fn top_counts(values: impl Iterator<Item = String>, n: usize) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for value in values {
        match counts.iter_mut().find(|(v, _)| *v == value) {
            Some((_, count)) => *count += 1,
            None => counts.push((value, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts.into_iter().take(n).map(|(v, _)| v).collect()
}

/// Plain style rules read off previous messages (deterministic: a small model
/// writes poor style guides, but follows explicit rules well).
pub fn derive_style_prompt(messages: &[Message]) -> String {
    static CONVENTIONAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\w+)(?:\(([^)]+)\))?!?: ").expect("valid"));
    let subjects: Vec<&str> = messages.iter().map(|m| m.subject.as_str()).filter(|s| !s.is_empty()).collect();
    if subjects.len() < 3 {
        return ai::DEFAULT_COMMIT_STYLE.to_string();
    }
    let ratio = |n: usize| n as f32 / subjects.len() as f32;
    let conv: Vec<Option<regex::Captures>> = subjects.iter().map(|s| CONVENTIONAL.captures(s)).collect();
    let conv_count = conv.iter().filter(|c| c.is_some()).count();
    let descriptions: Vec<String> = subjects
        .iter()
        .zip(&conv)
        .map(|(s, c)| {
            let rest = c.as_ref().map_or(*s, |c| &s[c[0].len()..]);
            rest.trim_start_matches(|ch: char| !ch.is_alphanumeric()).to_string()
        })
        .collect();
    let mut rules = Vec::new();
    let mut format = "<summary>".to_string();
    if ratio(conv_count) >= 0.5 {
        let mut types = top_counts(conv.iter().flatten().map(|c| c[1].to_lowercase()), 9);
        for t in ["feat", "fix", "refactor", "perf", "docs", "test", "chore"] {
            if types.len() < 9 && !types.iter().any(|x| x == t) {
                types.push(t.into());
            }
        }
        let scoped_count = conv.iter().flatten().filter(|c| c.get(2).is_some()).count();
        let scopes = top_counts(conv.iter().flatten().filter_map(|c| c.get(2).map(|m| m.as_str().to_string())), 8);
        let scoped = !scopes.is_empty() && scoped_count as f32 / conv_count as f32 >= 0.3;
        format = if scoped { "<type>(<scope>): <summary>".into() } else { "<type>: <summary>".into() };
        rules.push(format!("type: {}", types.join(" | ")));
        if scoped {
            rules.push(format!("scope: the area that changed, e.g. {}", scopes.join(", ")));
        }
    }
    let gitmoji = subjects
        .iter()
        .filter(|s| s.starts_with(':') || s.chars().next().is_some_and(|c| (c as u32) >= 0x1F000 || ('\u{2600}'..='\u{27BF}').contains(&c)))
        .count();
    if ratio(gitmoji) >= 0.3 {
        format = format!("<gitmoji> {format}");
    }
    let mut lengths: Vec<usize> = subjects.iter().map(|s| s.chars().count()).collect();
    lengths.sort();
    let median = lengths[lengths.len() / 2];
    let limit = (((median + 10) as f32 / 5.).round() as usize * 5).clamp(40, 72);
    let lower = descriptions.iter().filter(|d| d.chars().next().is_some_and(char::is_lowercase)).count();
    let upper = descriptions.iter().filter(|d| d.chars().next().is_some_and(char::is_uppercase)).count();
    let first_word = |d: &String| d.split(|c: char| !c.is_alphabetic()).next().unwrap_or("").to_lowercase();
    let past = descriptions.iter().filter(|d| first_word(d).ends_with("ed")).count();
    let gerund = descriptions.iter().filter(|d| first_word(d).ends_with("ing")).count();
    let periods = subjects.iter().filter(|s| s.ends_with('.')).count();
    let mood = if ratio(past) >= 0.4 {
        "past tense"
    } else if ratio(gerund) >= 0.4 {
        "starting with an -ing verb"
    } else {
        "imperative"
    };
    rules.push(format!(
        "summary: ≤ {limit} chars, {mood}, {}, {}",
        if lower > upper { "lowercase" } else { "capitalized" },
        if ratio(periods) >= 0.5 { "ending with a period" } else { "no period" }
    ));
    let bodies: Vec<&Message> = messages.iter().filter(|m| !m.body.trim().is_empty()).collect();
    if (bodies.len() as f32) / (messages.len() as f32) < 0.2 {
        rules.push("Add a body (≤ 2 lines) only if the \"why\" isn't obvious".into());
    } else if bodies.iter().filter(|m| m.body.lines().any(|l| l.trim_start().starts_with("- ") || l.trim_start().starts_with("* "))).count() as f32
        / bodies.len() as f32
        >= 0.5
    {
        rules.push("After a blank line, add a body of short \"- \" bullet points for the notable changes".into());
    } else {
        rules.push("After a blank line, add a short body saying what changed and why".into());
    }
    for rule in [
        "Describe what the change does, not how the code looks",
        "Don't invent context not in the diff",
        "Write in the same language as this repository's recent commit messages",
        "Output only the message, nothing else",
    ] {
        rules.push(rule.into());
    }
    let rules: Vec<String> = rules.iter().map(|r| format!("- {r}")).collect();
    format!("Write ONE commit message for the diff below.\n\nFormat: {format}\n{}\n\nDIFF:\n{DIFF_SLOT}", rules.join("\n"))
}

fn read_repo_style(top: &Path) -> Option<String> {
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->\n?").expect("valid"));
    let text = std::fs::read_to_string(top.join(STYLE_FILE)).ok()?;
    let style = COMMENT.replace_all(&text, "").trim().to_string();
    (!style.is_empty()).then_some(style)
}

/// The repository's style file, made first when it has none: derived from
/// its history (`derive`, or when there is no file yet), else the global
/// style. Its path, to open in a tab.
pub fn style_file(top: &Path, fallback: &str, derive: bool) -> std::path::PathBuf {
    if derive || read_repo_style(top).is_none() {
        let history = recent_messages(top, 50);
        let style = if history.len() >= 3 { derive_style_prompt(&history) } else if fallback.trim().is_empty() { ai::DEFAULT_COMMIT_STYLE.to_string() } else { fallback.to_string() };
        write_repo_style(top, &style);
    }
    top.join(STYLE_FILE)
}

fn write_repo_style(top: &Path, style: &str) {
    let _ = std::fs::create_dir_all(top.join(".den"));
    let header = "<!-- Commit message style for the Generate Commit Message button. {{diff}} marks where the changes go. -->\n";
    let _ = std::fs::write(top.join(STYLE_FILE), format!("{header}{}\n", style.trim()));
}

// -- Changes -----------------------------------------------------------------

fn status_word(status: char) -> &'static str {
    match status {
        'A' | '?' => "added",
        'M' => "modified",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'T' => "type changed",
        'U' => "unmerged",
        _ => "changed",
    }
}

/// The changed files with their line counts (`git diff --numstat` and
/// `--name-status`, `-z` so paths come through as they are).
fn diff_entries(top: &Path, staged: bool) -> Result<Vec<Entry>, String> {
    let mut args = vec!["diff", "-M", "-z"];
    if staged {
        args.push("--cached");
    }
    let run = |extra: &str| {
        let mut args = args.clone();
        args.push(extra);
        git::check(top, &args, None).map(|o| o.stdout).map_err(|e| e.to_string())
    };
    let names = run("--name-status")?;
    let counts = run("--numstat")?;
    let mut entries = Vec::new();
    let mut parts = names.split('\0').filter(|p| !p.is_empty());
    while let Some(code) = parts.next() {
        let status = code.chars().next().unwrap_or('M');
        let first = parts.next().unwrap_or_default().to_string();
        let (old_path, path) = if matches!(status, 'R' | 'C') { (Some(first), parts.next().unwrap_or_default().to_string()) } else { (None, first) };
        entries.push(Entry { path, old_path, status, add: 0, del: 0, binary: false });
    }
    // numstat: "add\tdel\tpath\0", or for a rename "add\tdel\t\0old\0new\0".
    let mut fields = counts.split('\0').peekable();
    while let Some(head) = fields.next() {
        let mut cols = head.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (cols.next(), cols.next(), cols.next()) else { continue };
        let path = if path.is_empty() {
            fields.next();
            fields.next().unwrap_or_default().to_string()
        } else {
            path.to_string()
        };
        if let Some(entry) = entries.iter_mut().find(|e| e.path == path) {
            entry.binary = add == "-";
            entry.add = add.parse().unwrap_or(0);
            entry.del = del.parse().unwrap_or(0);
        }
    }
    Ok(entries)
}

/// Keep the first part of a long text, cut at a line boundary.
fn cap_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = text[..end].rfind('\n').filter(|&c| c > max / 2).unwrap_or(end);
    let kept = &text[..cut];
    let rest = text[cut..].lines().count();
    format!("{kept}\n[... {rest} more lines of this file omitted]\n")
}

/// `git diff` output per file, keyed by the new path.
fn split_diff(text: &str) -> Vec<(String, String)> {
    static INDEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^index [0-9a-f]+\.\.[0-9a-f]+.*\n").expect("valid"));
    let mut out = Vec::new();
    let mut starts: Vec<usize> = text.match_indices("\ndiff --git ").map(|(i, _)| i + 1).collect();
    if text.starts_with("diff --git ") {
        starts.insert(0, 0);
    }
    for (i, &start) in starts.iter().enumerate() {
        let part = &text[start..starts.get(i + 1).copied().unwrap_or(text.len())];
        let path = part
            .lines()
            .find_map(|l| l.strip_prefix("+++ b/"))
            .or_else(|| part.lines().find_map(|l| l.strip_prefix("rename to ")))
            .or_else(|| part.lines().next().and_then(|l| l.rsplit_once(" b/").map(|(_, p)| p)))
            .unwrap_or("")
            .trim()
            .to_string();
        if !path.is_empty() {
            out.push((path, INDEX.replace(part, "").to_string()));
        }
    }
    out
}

fn collect_changes(top: &Path, per_file: usize, max_total: usize, cancel: &Cancel, exclude: Option<&str>) -> Result<Changes, String> {
    let staged = git::run(top, &["diff", "--cached", "--quiet"], None).is_ok_and(|o| !o.ok());
    let mut entries = diff_entries(top, staged)?;
    if !staged && let Ok(out) = git::check(top, &["ls-files", "--others", "--exclude-standard", "-z"], None) {
        for path in out.stdout.split('\0').filter(|p| !p.is_empty()) {
            entries.push(Entry { path: path.to_string(), old_path: None, status: '?', add: 0, del: 0, binary: false });
        }
    }
    cancel.check()?;
    if let Some(exclude) = exclude {
        entries.retain(|e| e.path != exclude);
    }
    let mut omitted = HashSet::new();
    let mut wanted: Vec<Entry> = Vec::new();
    for entry in &entries {
        if entry.binary || NOISE.iter().any(|r| r.is_match(&entry.path)) || entry.add + entry.del > HUGE_FILE_LINES {
            omitted.insert(entry.path.clone());
        } else {
            wanted.push(entry.clone());
        }
    }
    // Biggest changes first: they matter most and go into the budget first.
    wanted.sort_by(|a, b| (b.add + b.del).cmp(&(a.add + a.del)));
    let (untracked, tracked): (Vec<Entry>, Vec<Entry>) = wanted.into_iter().partition(|e| e.status == '?');
    let mut diffs = Vec::new();
    let mut total = 0;
    for slice in tracked.chunks(100) {
        if total >= max_total {
            break;
        }
        let mut args: Vec<String> = vec!["diff".into(), "-M".into()];
        if staged {
            args.push("--cached".into());
        }
        args.push("--".into());
        for entry in slice {
            if let Some(old) = &entry.old_path {
                args.push(old.clone());
            }
            args.push(entry.path.clone());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = git::check(top, &args, None).map(|o| o.stdout).unwrap_or_default();
        let sections = split_diff(&text);
        cancel.check()?;
        for entry in slice {
            match sections.iter().find(|(p, _)| *p == entry.path) {
                Some((_, text)) if total < max_total => {
                    let capped = cap_text(text, per_file);
                    total += capped.len();
                    diffs.push((entry.path.clone(), capped));
                }
                _ => {
                    omitted.insert(entry.path.clone());
                }
            }
        }
    }
    let mut previews = 0;
    for entry in untracked {
        if total >= max_total || previews >= MAX_UNTRACKED_PREVIEWS {
            omitted.insert(entry.path.clone());
            continue;
        }
        previews += 1;
        let Ok(bytes) = std::fs::read(top.join(&entry.path)) else {
            omitted.insert(entry.path.clone());
            continue;
        };
        if bytes.contains(&0) || bytes.is_empty() {
            omitted.insert(entry.path.clone());
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        if let Some(e) = entries.iter_mut().find(|e| e.path == entry.path) {
            e.add = lines.len();
        }
        let mut preview = format!("new file {}\n", entry.path);
        for line in lines.iter().take(UNTRACKED_PREVIEW) {
            preview.push('+');
            preview.push_str(line);
            preview.push('\n');
        }
        if lines.len() > UNTRACKED_PREVIEW {
            preview.push_str("[... rest of the new file omitted]\n");
        }
        let capped = cap_text(&preview, per_file);
        total += capped.len();
        diffs.push((entry.path.clone(), capped));
    }
    Ok(Changes { entries, diffs, omitted })
}

fn stat_block(changes: &Changes) -> String {
    let mut rows: Vec<String> = changes
        .entries
        .iter()
        .take(STAT_LINES)
        .map(|e| {
            let name = e.old_path.as_ref().map_or(e.path.clone(), |old| format!("{old} -> {}", e.path));
            let counts = if e.binary {
                "binary".to_string()
            } else if e.status == '?' {
                "new file".into()
            } else {
                format!("+{} -{}", e.add, e.del)
            };
            let note = if changes.omitted.contains(&e.path) && !e.binary { ", content omitted" } else { "" };
            format!("{} {name} ({counts}{note})", status_word(e.status))
        })
        .collect();
    if changes.entries.len() > STAT_LINES {
        rows.push(format!("... and {} more files", changes.entries.len() - STAT_LINES));
    }
    rows.join("\n")
}

// -- Prompting ---------------------------------------------------------------

fn example_text(message: &Message) -> String {
    let body: Vec<&str> = message.body.lines().take(EXAMPLE_BODY_LINES).collect();
    let body = body.join("\n");
    if body.trim().is_empty() { message.subject.clone() } else { format!("{}\n\n{}", message.subject, body.trim()) }
}

fn final_messages(style: &str, examples: &[String], changes: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    if !examples.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": format!("Recent commit messages in this repository, separated by \"---\". Match their format and tone, not their content:\n\n{}", examples.join("\n---\n")),
        }));
    }
    let prompt = style.trim();
    let content = if prompt.contains(DIFF_SLOT) { prompt.replace(DIFF_SLOT, changes) } else { format!("{prompt}\n\n{changes}") };
    messages.push(json!({
        "role": "user",
        "content": format!("{content}\n\nReply with the commit message only. No quotes, no code fences, no explanations."),
    }));
    messages
}

fn body(messages: Vec<Value>, max_tokens: usize, temperature: f32) -> Value {
    json!({
        "messages": messages,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "top_p": 0.9,
        // Small models loop on long bullet lists otherwise.
        "dry_multiplier": 0.8,
        "dry_base": 1.75,
        "dry_allowed_length": 2,
        "cache_prompt": true,
        // Hybrid-thinking models (Qwen3.x) answer directly.
        "chat_template_kwargs": { "enable_thinking": false },
    })
}

/// Strip what small models like to wrap around the message.
pub fn clean_message(raw: &str) -> String {
    static THINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<think>.*?(</think>|$)\s*").expect("valid"));
    static FENCE_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^```[\w-]*\n?").expect("valid"));
    static FENCE_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n?```\s*$").expect("valid"));
    static LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(\*\*)?(suggested )?commit message:?(\*\*)?:?\s*").expect("valid"));
    static BLANKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").expect("valid"));
    let text = raw.replace("\r\n", "\n");
    let text = THINK.replace_all(text.trim(), "");
    let text = FENCE_OPEN.replace(&text, "");
    let text = FENCE_CLOSE.replace(&text, "");
    let text = LABEL.replace(&text, "").to_string();
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if let Some(first) = lines.first_mut() {
        *first = first.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string();
    }
    let joined = unroll_bullets(lines).join("\n");
    BLANKS.replace_all(&joined, "\n\n").trim().to_string()
}

/// "subject - one - two" on the first line with no real bullet lines: a list
/// the model ran together. Split it into a subject and "- " lines.
fn unroll_bullets(lines: Vec<String>) -> Vec<String> {
    static SEPARATOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+[-•]\s+").expect("valid"));
    let has_bullets = lines.iter().skip(1).any(|l| {
        let l = l.trim_start();
        l.starts_with("- ") || l.starts_with("* ") || l.starts_with("• ")
    });
    if has_bullets || lines.is_empty() {
        return lines;
    }
    let parts: Vec<&str> = SEPARATOR.split(&lines[0]).map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.len() < 3 {
        return lines;
    }
    let mut out = vec![parts[0].to_string(), String::new()];
    out.extend(parts[1..].iter().map(|b| format!("- {b}")));
    out.extend(lines.into_iter().skip(1));
    out
}

/// The request did not fit the model's context. llama-server says so, but
/// may close the connection while saying it (a reset, os error 10054), which
/// counts too: a smaller request is the fix either way.
fn is_context_error(err: &str) -> bool {
    let err = err.to_lowercase();
    ["context", "exceed", "too long", "too large", "n_ctx", "tokens", "forcibly closed", "connection reset", "10054", "10053", "broken pipe"]
        .iter()
        .any(|w| err.contains(w))
}

/// Write a commit message for the repository's changes. `status` says what
/// it is doing; `text` gets the message so far while it streams.
pub fn generate(top: &Path, config: &AiConfig, cancel: &Cancel, status: &mut dyn FnMut(String), text: &mut dyn FnMut(String)) -> Result<String, String> {
    status("Reading changes…".into());
    let history = recent_messages(top, 50);
    let mut style = read_repo_style(top);
    let mut created = false;
    if style.is_none() && config.derive_style && history.len() >= 3 {
        let derived = derive_style_prompt(&history);
        write_repo_style(top, &derived);
        style = Some(derived);
        created = true;
    }
    let style = style.unwrap_or_else(|| if config.style.trim().is_empty() { ai::DEFAULT_COMMIT_STYLE.to_string() } else { config.style.clone() });
    let examples: Vec<String> = history.iter().take(EXAMPLES).map(example_text).collect();
    let exclude = created.then_some(STYLE_FILE);
    // The size estimate can be off for dense content (minified code, data):
    // on a context error, again with half the budget, then a quarter.
    let mut result = Err(String::new());
    for scale in [1.0, 0.5, 0.25] {
        result = attempt(top, config, &style, &examples, exclude, scale, cancel, status, text);
        match &result {
            Err(err) if *err != CANCELLED && is_context_error(err) => status("Too big for the model's context; trying smaller…".into()),
            _ => break,
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn attempt(
    top: &Path,
    config: &AiConfig,
    style: &str,
    examples: &[String],
    exclude: Option<&str>,
    scale: f32,
    cancel: &Cancel,
    status: &mut dyn FnMut(String),
    text: &mut dyn FnMut(String),
) -> Result<String, String> {
    let ctx = config.context as usize;
    let overhead: usize = final_messages(style, examples, "").iter().map(|m| m["content"].as_str().unwrap_or("").len()).sum();
    let system_tokens = overhead.div_ceil(CHARS_PER_TOKEN);
    let scaled = |tokens: usize| ((tokens * CHARS_PER_TOKEN) as f32 * scale) as usize;
    let budget = scaled(ctx.saturating_sub(system_tokens + OUTPUT_TOKENS + 200)).max(2000);
    let map_budget = scaled(ctx.saturating_sub(300 + MAP_OUTPUT_TOKENS)).max(2000);
    let per_file = MAX_FILE_CHARS.min(map_budget * 9 / 10);
    let changes = collect_changes(top, per_file, map_budget * MAX_CHUNKS, cancel, exclude)?;
    if changes.entries.is_empty() {
        return Err("There are no changes to describe.".into());
    }
    let mut stat = stat_block(&changes);
    if stat.len() > budget / 3 {
        stat = cap_text(&stat, budget / 3);
    }
    let diff_chars: usize = changes.diffs.iter().map(|(_, d)| d.len()).sum();

    let user = if stat.len() + diff_chars + 100 <= budget {
        let diff: Vec<&str> = changes.diffs.iter().map(|(_, d)| d.as_str()).collect();
        let diff = diff.join("\n");
        if diff.is_empty() { format!("Files changed:\n{stat}") } else { format!("Files changed:\n{stat}\n\n{diff}") }
    } else {
        // Map: summarize the diffs chunk by chunk, each file's diff whole.
        let mut chunks: Vec<Vec<&(String, String)>> = Vec::new();
        let mut current = Vec::new();
        let mut size = 0;
        for diff in &changes.diffs {
            if size + diff.1.len() > map_budget && !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                size = 0;
            }
            size += diff.1.len();
            current.push(diff);
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        let mut notes = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            status(format!("Summarizing {}/{}…", i + 1, chunks.len()));
            let names: Vec<&str> = chunk.iter().map(|(p, _)| p.as_str()).collect();
            let texts: Vec<&str> = chunk.iter().map(|(_, d)| d.as_str()).collect();
            let content = format!("Part {} of {} of a larger change. Files in this part: {}\n\n{}", i + 1, chunks.len(), names.join(", "), texts.join("\n"));
            let messages = vec![json!({ "role": "system", "content": MAP_SYSTEM }), json!({ "role": "user", "content": content })];
            let summary = ai::chat(config, body(messages, MAP_OUTPUT_TOKENS, 0.1), cancel, |_| {})?;
            notes.push(summary.trim().to_string());
        }
        let mut note_text = notes.join("\n");
        let room = budget.saturating_sub(stat.len() + 200);
        if note_text.len() > room {
            note_text = cap_text(&note_text, room.max(1000));
        }
        format!(
            "Files changed ({}):\n{stat}\n\nThe full diff is too large to show; summary of its parts:\n{note_text}\n\n(Write one message covering the change as a whole; put the main change in the summary line.)",
            changes.entries.len()
        )
    };
    status("Writing…".into());
    let mut raw = String::new();
    let result = ai::chat(config, body(final_messages(style, examples, &user), OUTPUT_TOKENS, 0.2), cancel, |piece| {
        raw.push_str(piece);
        text(clean_message(&raw));
    })?;
    let message = clean_message(&result);
    if message.is_empty() {
        return Err("The model returned an empty message. Try again.".into());
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::{Message, cap_text, clean_message, derive_style_prompt, split_diff};

    #[test]
    fn cleans_what_models_wrap_around() {
        assert_eq!(clean_message("```\nfeat: add x\n```"), "feat: add x");
        assert_eq!(clean_message("<think>hmm</think>\n\"fix: y\""), "fix: y");
        assert_eq!(clean_message("Commit message: chore: z"), "chore: z");
        assert_eq!(clean_message("feat: a - one - two"), "feat: a\n\n- one\n- two");
    }

    #[test]
    fn derives_conventional_style() {
        let history: Vec<Message> = ["feat: add a", "fix(ui): mend b", "chore: release v1", "feat(ui): add c"]
            .iter()
            .map(|s| Message { subject: s.to_string(), body: String::new() })
            .collect();
        let style = derive_style_prompt(&history);
        assert!(style.contains("Format: <type>(<scope>): <summary>"));
        assert!(style.contains("lowercase"));
        assert!(style.contains("{{diff}}"));
    }

    #[test]
    fn splits_a_diff_per_file() {
        let text = "diff --git a/x.rs b/x.rs\nindex 111..222 100644\n--- a/x.rs\n+++ b/x.rs\n@@\n-a\n+b\ndiff --git a/y.rs b/y.rs\n--- a/y.rs\n+++ b/y.rs\n";
        let parts = split_diff(text);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "x.rs");
        assert!(!parts[0].1.contains("index 111"));
    }

    #[test]
    fn caps_long_text_at_a_line() {
        let text = "aaaa\nbbbb\ncccc\ndddd\n";
        assert!(cap_text(text, 12).starts_with("aaaa\nbbbb\n[... "));
    }
}
