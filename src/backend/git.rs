//! Git through the `git` executable, as den does: hooks, credential helpers,
//! LFS and the user's config all apply. The runner never prompts and never
//! flashes a console window; callers run it off the UI thread.
//!
//! The status parser and the operations are den's frontend logic
//! (`git.svelte.ts`), moved to Rust.

use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// A git run that started: its exit code and output.
#[derive(Clone, Debug)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No `git` on the PATH.
    NotFound,
    /// git ran and failed: the command and its message.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "git was not found on the PATH"),
            Error::Failed(message) => f.write_str(message),
        }
    }
}

/// Run `git <args>` in `cwd`, feeding `stdin` if given. Ok even on a non-zero
/// exit (the caller reads `code`); Err only when git could not start.
pub fn run(cwd: &Path, args: &[&str], stdin: Option<&str>) -> Result<Output, Error> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0") // fail instead of waiting for a password
        .env("GIT_OPTIONAL_LOCKS", "0") // status never writes the index (no watcher loop)
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound
        } else {
            Error::Failed(e.to_string())
        }
    })?;
    if let (Some(text), Some(mut pipe)) = (stdin.map(str::to_string), child.stdin.take()) {
        // Its own thread, so a long message cannot deadlock against a full stdout pipe.
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let out = child.wait_with_output().map_err(|e| Error::Failed(e.to_string()))?;
    Ok(Output {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Like `run`, but a non-zero exit is an error carrying git's message.
pub fn check(cwd: &Path, args: &[&str], stdin: Option<&str>) -> Result<Output, Error> {
    let out = run(cwd, args, stdin)?;
    if out.ok() {
        Ok(out)
    } else {
        let message = if out.stderr.trim().is_empty() { out.stdout.trim() } else { out.stderr.trim() };
        Err(Error::Failed(format!("git {}: {}", args.join(" "), message)))
    }
}

// ---------------------------------------------------------------------------
// Status

/// One side of a porcelain XY pair: `.` unchanged, `?` untracked.
pub type Code = char;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    /// Absolute path, native separators.
    pub path: PathBuf,
    /// Relative to the repository top level, `/` separators: what git takes.
    pub rel: String,
    pub index: Code,
    pub worktree: Code,
    pub renamed_from: Option<PathBuf>,
    /// An unmerged entry; `index`/`worktree` hold the conflict letters.
    pub conflict: bool,
}

impl FileStatus {
    pub fn untracked(&self) -> bool {
        self.worktree == '?'
    }

    pub fn staged(&self) -> bool {
        !self.conflict && self.index != '.' && self.index != '?'
    }

    pub fn changed(&self) -> bool {
        !self.conflict && self.worktree != '.'
    }

    /// The letter den shows for the working-tree side (or the index side for
    /// a staged-only change): M, U, A, D, R, or ! for a conflict.
    pub fn letter(&self) -> char {
        if self.conflict {
            return '!';
        }
        let code = if self.worktree != '.' { self.worktree } else { self.index };
        match code {
            '?' => 'U',
            'T' => 'M',
            'C' => 'A',
            other => other,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Branch {
    /// `(initial)` on an unborn branch.
    pub oid: String,
    /// The branch name, or `(detached)`.
    pub head: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Branch,
    pub files: Vec<FileStatus>,
    /// Ignored files and folders (folders without a trailing separator).
    pub ignored: HashSet<PathBuf>,
}

/// The repository `dir` is in: its top level, or `None` outside one.
pub fn toplevel(dir: &Path) -> Result<Option<PathBuf>, Error> {
    let out = run(dir, &["rev-parse", "--show-toplevel"], None)?;
    Ok(out
        .ok()
        .then(|| PathBuf::from(out.stdout.trim().replace('/', std::path::MAIN_SEPARATOR_STR))))
}

pub fn status(top: &Path) -> Result<Status, Error> {
    let out = check(
        top,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
            "--ignored=matching",
            "--renames",
        ],
        None,
    )?;
    Ok(parse_status(&out.stdout, top))
}

/// Parse `git status --porcelain=v2 -z --branch`.
pub fn parse_status(raw: &str, top: &Path) -> Status {
    let abs = |rel: &str| {
        let rel = rel.trim_end_matches('/');
        let mut path = top.to_path_buf();
        for part in rel.split('/') {
            path.push(part);
        }
        path
    };
    let mut status = Status {
        branch: Branch {
            oid: "(initial)".into(),
            ..Branch::default()
        },
        ..Status::default()
    };
    let mut tokens = raw.split('\0');
    while let Some(record) = tokens.next() {
        let Some(kind) = record.chars().next() else { continue };
        match kind {
            '#' => {
                let mut parts = record.splitn(3, ' ');
                parts.next();
                let key = parts.next().unwrap_or_default();
                let value = parts.next().unwrap_or_default();
                match key {
                    "branch.oid" => status.branch.oid = value.into(),
                    "branch.head" => status.branch.head = value.into(),
                    "branch.upstream" => status.branch.upstream = Some(value.into()),
                    "branch.ab" => {
                        let mut ab = value.split(' ');
                        let parse = |s: Option<&str>| s.and_then(|s| s[1..].parse().ok()).unwrap_or(0);
                        status.branch.ahead = parse(ab.next());
                        status.branch.behind = parse(ab.next());
                    }
                    _ => {}
                }
            }
            '1' | '2' | 'u' => {
                let fields = match kind {
                    '1' => 8,
                    '2' => 9,
                    _ => 10,
                };
                let mut parts = record.splitn(fields + 1, ' ');
                parts.next();
                let xy: Vec<char> = parts.next().unwrap_or("..").chars().collect();
                let rel = parts.nth(fields - 2).unwrap_or_default().to_string();
                let renamed_from = (kind == '2').then(|| abs(tokens.next().unwrap_or_default()));
                status.files.push(FileStatus {
                    path: abs(&rel),
                    rel,
                    index: xy.first().copied().unwrap_or('.'),
                    worktree: xy.get(1).copied().unwrap_or('.'),
                    renamed_from,
                    conflict: kind == 'u',
                });
            }
            '?' => {
                let rel = record[2..].to_string();
                status.files.push(FileStatus {
                    path: abs(&rel),
                    rel,
                    index: '.',
                    worktree: '?',
                    renamed_from: None,
                    conflict: false,
                });
            }
            '!' => {
                status.ignored.insert(abs(&record[2..]));
            }
            _ => {}
        }
    }
    status.files.sort_by_key(|f| f.rel.to_lowercase());
    status
}

// ---------------------------------------------------------------------------
// Operations

fn unborn(top: &Path) -> bool {
    run(top, &["rev-parse", "--verify", "-q", "HEAD"], None).is_ok_and(|out| !out.ok())
}

fn with_paths<'a>(base: &[&'a str], rels: &'a [String]) -> Vec<&'a str> {
    let mut args = base.to_vec();
    args.push("--");
    args.extend(rels.iter().map(String::as_str));
    args
}

pub fn stage(top: &Path, rels: &[String]) -> Result<(), Error> {
    check(top, &with_paths(&["add", "-A"], rels), None).map(drop)
}

pub fn unstage(top: &Path, rels: &[String]) -> Result<(), Error> {
    let base: &[&str] = if unborn(top) { &["rm", "--cached", "-r", "-q"] } else { &["restore", "--staged"] };
    check(top, &with_paths(base, rels), None).map(drop)
}

/// Working-tree changes only: tracked files go back to the index, untracked
/// ones to the Recycle Bin.
pub fn discard(top: &Path, files: &[FileStatus]) -> Result<(), Error> {
    let tracked: Vec<String> = files.iter().filter(|f| !f.untracked()).map(|f| f.rel.clone()).collect();
    let untracked: Vec<PathBuf> = files.iter().filter(|f| f.untracked()).map(|f| f.path.clone()).collect();
    if !tracked.is_empty() {
        check(top, &with_paths(&["checkout", "-q"], &tracked), None)?;
    }
    if !untracked.is_empty() {
        trash::delete_all(&untracked).map_err(|e| Error::Failed(format!("Could not move to the Recycle Bin: {e}")))?;
    }
    Ok(())
}

pub fn commit(top: &Path, message: &str, amend: bool) -> Result<(), Error> {
    let mut args = vec!["commit", "-q"];
    if amend {
        args.push("--amend");
    }
    if !message.trim().is_empty() {
        args.extend(["-F", "-"]);
        return check(top, &args, Some(message)).map(drop);
    }
    if amend {
        args.push("--no-edit");
        return check(top, &args, None).map(drop);
    }
    Err(Error::Failed("Provide a commit message.".into()))
}

pub fn fetch(top: &Path) -> Result<(), Error> {
    check(top, &["fetch"], None).map(drop)
}

pub fn pull(top: &Path) -> Result<(), Error> {
    check(top, &["pull"], None).map(drop)
}

/// Push, publishing the branch to the first remote when it has no upstream.
pub fn push(top: &Path, branch: &Branch) -> Result<(), Error> {
    if branch.upstream.is_some() {
        return check(top, &["push"], None).map(drop);
    }
    let remotes = check(top, &["remote"], None)?;
    let remote = remotes
        .stdout
        .lines()
        .map(str::trim)
        .find(|r| !r.is_empty())
        .ok_or_else(|| Error::Failed("This repository has no remote to publish to.".into()))?
        .to_string();
    check(top, &["push", "-u", &remote, &branch.head], None).map(drop)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchRef {
    pub name: String,
    pub remote: bool,
    pub current: bool,
}

const UNIT: char = '\x1f';
const RECORD: char = '\x1e';

pub fn branches(top: &Path) -> Result<Vec<BranchRef>, Error> {
    let format = format!("%(refname){UNIT}%(refname:short){UNIT}%(HEAD){UNIT}%(symref)");
    let out = check(
        top,
        &["for-each-ref", &format!("--format={format}"), "--sort=-committerdate", "refs/heads", "refs/remotes"],
        None,
    )?;
    Ok(out
        .stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(UNIT).collect();
            let [refname, name, head, symref] = parts[..] else { return None };
            (symref.is_empty()).then(|| BranchRef {
                name: name.to_string(),
                remote: refname.starts_with("refs/remotes/"),
                current: head == "*",
            })
        })
        .collect())
}

/// Switch to a local branch, or check out a remote one as a tracking branch.
pub fn switch(top: &Path, branch: &BranchRef) -> Result<(), Error> {
    if !branch.remote {
        return check(top, &["switch", &branch.name], None).map(drop);
    }
    let local = branch.name.split_once('/').map_or(branch.name.as_str(), |(_, rest)| rest);
    let exists = branches(top)?.iter().any(|b| !b.remote && b.name == local);
    if exists {
        check(top, &["switch", local], None).map(drop)
    } else {
        check(top, &["switch", "-c", local, "--track", &branch.name], None).map(drop)
    }
}

pub fn create_branch(top: &Path, name: &str) -> Result<(), Error> {
    check(top, &["switch", "-c", name], None).map(drop)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Unix seconds.
    pub date: i64,
    pub subject: String,
}

pub fn log(top: &Path, n: usize, skip: usize) -> Result<Vec<Commit>, Error> {
    if unborn(top) {
        return Ok(Vec::new());
    }
    let format = format!("%H{UNIT}%h{UNIT}%an{UNIT}%at{UNIT}%s{RECORD}");
    let out = check(
        top,
        &["log", &format!("--format={format}"), "-n", &n.to_string(), &format!("--skip={skip}"), "HEAD"],
        None,
    )?;
    Ok(parse_log(&out.stdout))
}

pub fn parse_log(raw: &str) -> Vec<Commit> {
    raw.split(RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches('\n');
            let parts: Vec<&str> = record.split(UNIT).collect();
            let [hash, short, author, date, subject] = parts[..] else { return None };
            Some(Commit {
                hash: hash.into(),
                short: short.into(),
                author: author.into(),
                date: date.parse().unwrap_or(0),
                subject: subject.into(),
            })
        })
        .collect()
}

/// A file as committed at `rev` (`HEAD`, or `:` for the index), for diffs.
pub fn show(top: &Path, rev: &str, rel: &str) -> Result<String, Error> {
    let spec = format!("{rev}:{rel}");
    check(top, &["show", &spec], None).map(|out| out.stdout)
}

/// A file a commit changed: its status letter, its path, and the path it had
/// before (a rename).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFile {
    pub status: char,
    pub rel: String,
    pub old_rel: String,
}

/// The files `hash` changed and its first parent (none for a root commit).
pub fn commit_files(top: &Path, hash: &str) -> Result<(Option<String>, Vec<CommitFile>), Error> {
    let parent = run(top, &["rev-parse", "--verify", "-q", &format!("{hash}^")], None)?;
    let parent = parent.ok().then(|| parent.stdout.trim().to_string()).filter(|p| !p.is_empty());
    let out = check(top, &["diff-tree", "--no-commit-id", "-r", "-M", "--root", "--name-status", hash], None)?;
    Ok((parent, parse_name_status(&out.stdout)))
}

/// `M\tsrc/a.rs` and `R087\told\tnew` lines.
pub fn parse_name_status(raw: &str) -> Vec<CommitFile> {
    raw.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let status = parts.next()?.chars().next()?;
            let first = parts.next()?.to_string();
            let (old_rel, rel) = match parts.next() {
                Some(second) => (first, second.to_string()),
                None => (first.clone(), first),
            };
            Some(CommitFile { status, rel, old_rel })
        })
        .collect()
}

/// "3 hours ago", for the commit list.
pub fn age(date: i64, now: i64) -> String {
    let seconds = (now - date).max(0);
    let (n, unit) = match seconds {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s if s < 7 * 86_400 => (s / 86_400, "day"),
        s if s < 30 * 86_400 => (s / (7 * 86_400), "week"),
        s if s < 365 * 86_400 => (s / (30 * 86_400), "month"),
        s => (s / (365 * 86_400), "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod commit_file_tests {
    use super::{CommitFile, parse_name_status};

    #[test]
    fn plain_and_renamed_files() {
        let files = parse_name_status("M\tsrc/a.rs\nR087\told.rs\tnew.rs\nA\tb.txt\n");
        assert_eq!(files[0], CommitFile { status: 'M', rel: "src/a.rs".into(), old_rel: "src/a.rs".into() });
        assert_eq!(files[1], CommitFile { status: 'R', rel: "new.rs".into(), old_rel: "old.rs".into() });
        assert_eq!(files[2].status, 'A');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\repo" } else { "/repo" })
    }

    #[test]
    fn parses_branch_and_records() {
        let raw = [
            "# branch.oid 1234abcd",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 aaa bbb src/main.rs",
            "1 A. N... 000000 100644 100644 000 ccc docs/new file.md",
            "2 R. N... 100644 100644 100644 ddd eee R100 lib/new.rs",
            "lib/old.rs",
            "u UU N... 100644 100644 100644 100644 fff ggg hhh conflict.txt",
            "? notes.txt",
            "! target/",
            "",
        ]
        .join("\0");
        let status = parse_status(&raw, &top());
        assert_eq!(status.branch.head, "main");
        assert_eq!(status.branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.branch.ahead, status.branch.behind), (2, 1));
        let rels: Vec<&str> = status.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["conflict.txt", "docs/new file.md", "lib/new.rs", "notes.txt", "src/main.rs"]);

        let main = status.files.iter().find(|f| f.rel == "src/main.rs").unwrap();
        assert!(main.changed() && !main.staged());
        assert_eq!(main.letter(), 'M');
        assert_eq!(main.path, top().join("src").join("main.rs"));

        let added = status.files.iter().find(|f| f.rel == "docs/new file.md").unwrap();
        assert!(added.staged() && !added.changed());
        assert_eq!(added.letter(), 'A');

        let renamed = status.files.iter().find(|f| f.rel == "lib/new.rs").unwrap();
        assert_eq!(renamed.renamed_from, Some(top().join("lib").join("old.rs")));
        assert_eq!(renamed.letter(), 'R');

        let conflict = status.files.iter().find(|f| f.rel == "conflict.txt").unwrap();
        assert!(conflict.conflict && !conflict.staged() && !conflict.changed());
        assert_eq!(conflict.letter(), '!');

        let untracked = status.files.iter().find(|f| f.rel == "notes.txt").unwrap();
        assert!(untracked.untracked() && untracked.changed());
        assert_eq!(untracked.letter(), 'U');

        assert!(status.ignored.contains(&top().join("target")));
    }

    #[test]
    fn an_unborn_branch_has_no_oid() {
        let status = parse_status("# branch.oid (initial)\0# branch.head main\0", &top());
        assert_eq!(status.branch.oid, "(initial)");
        assert_eq!(status.branch.upstream, None);
    }

    #[test]
    fn parses_log_records() {
        let raw = format!("abc{UNIT}ab{UNIT}Ann{UNIT}100{UNIT}first{RECORD}\ndef{UNIT}de{UNIT}Bob{UNIT}200{UNIT}second{RECORD}\n");
        let commits = parse_log(&raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[1].author, "Bob");
        assert_eq!(commits[1].date, 200);
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(0, 30), "just now");
        assert_eq!(age(0, 3600), "1 hour ago");
        assert_eq!(age(0, 3 * 86_400), "3 days ago");
    }

    #[test]
    fn runs_against_a_real_repository() {
        let dir = std::env::temp_dir().join(format!("den-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Ok(out) = run(&dir, &["init", "-q", "-b", "main"], None) else { return }; // no git here
        assert!(out.ok());
        for args in [["config", "user.email", "t@example.com"], ["config", "user.name", "Test"]] {
            check(&dir, &args, None).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        let top = toplevel(&dir).unwrap().unwrap();
        let status = status(&top).unwrap();
        assert_eq!(status.files.len(), 1);
        assert!(status.files[0].untracked());

        stage(&top, &["a.txt".into()]).unwrap();
        assert!(super::status(&top).unwrap().files[0].staged());
        commit(&top, "first", false).unwrap();
        assert!(super::status(&top).unwrap().files.is_empty());
        let commits = log(&top, 10, 0).unwrap();
        assert_eq!(commits[0].subject, "first");

        std::fs::write(dir.join("a.txt"), "two").unwrap();
        let changed = super::status(&top).unwrap().files;
        discard(&top, &changed).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one");
        assert_eq!(show(&top, "HEAD", "a.txt").unwrap(), "one");
        std::fs::remove_dir_all(&dir).ok();
    }
}
