//! Git through the `git` executable, as den does: hooks, credential helpers,
//! LFS and the user's config all apply. The runner never prompts and never
//! flashes a console window; callers run it off the UI thread.
//!
//! The status parser and the operations are den's frontend logic
//! (`git.svelte.ts`), moved to Rust.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
};

use super::process;

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
        .env_remove("GIT_INDEX_FILE");
    let out = process::output_with_input(cmd, stdin).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound
        } else {
            Error::Failed(e.to_string())
        }
    })?;
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
    /// The path it had before a rename, relative like `rel`.
    pub renamed_from: Option<String>,
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
                let renamed_from = (kind == '2').then(|| tokens.next().unwrap_or_default().to_string());
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

/// `git <base>` on the paths `rels`, handed over on stdin: a command line
/// could not hold thousands of them.
fn with_pathspec(top: &Path, base: &[&str], rels: &[String]) -> Result<(), Error> {
    let mut args = base.to_vec();
    args.extend(["--pathspec-from-file=-", "--pathspec-file-nul"]);
    check(top, &args, Some(&rels.join("\0"))).map(drop)
}

pub fn stage(top: &Path, rels: &[String]) -> Result<(), Error> {
    with_pathspec(top, &["add", "-A"], rels)
}

pub fn unstage(top: &Path, rels: &[String]) -> Result<(), Error> {
    let base: &[&str] = if unborn(top) { &["rm", "--cached", "-r", "-q"] } else { &["restore", "--staged"] };
    with_pathspec(top, base, rels)
}

/// Working-tree changes only: tracked files go back to the index, untracked
/// ones to the Recycle Bin.
pub fn discard(top: &Path, files: &[FileStatus]) -> Result<(), Error> {
    let tracked: Vec<String> = files.iter().filter(|f| !f.untracked()).map(|f| f.rel.clone()).collect();
    let untracked: Vec<PathBuf> = files.iter().filter(|f| f.untracked()).map(|f| f.path.clone()).collect();
    if !tracked.is_empty() {
        with_pathspec(top, &["checkout", "-q"], &tracked)?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

/// A branch or tag at a commit, as `git log` decorates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRef {
    pub kind: RefKind,
    /// `main`, `origin/main`, `v1.0`.
    pub name: String,
    /// HEAD points at this branch.
    pub head: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    /// Unix seconds.
    pub date: i64,
    /// Local time as `2026-10-06 21:30`, for [`long_date`].
    pub when: String,
    pub subject: String,
    /// The message after its subject, trimmed.
    pub body: String,
    pub refs: Vec<CommitRef>,
    /// HEAD is this commit, on no branch.
    pub detached_head: bool,
}

/// `n` commits of `revs` (after the first `skip`), newest first with
/// children before parents, so a graph can be laid out from them.
pub fn log(top: &Path, revs: &[String], n: usize, skip: usize) -> Result<Vec<Commit>, Error> {
    if unborn(top) {
        return Ok(Vec::new());
    }
    let format = format!("--format=%H{UNIT}%h{UNIT}%P{UNIT}%an{UNIT}%ae{UNIT}%at{UNIT}%ad{UNIT}%D{UNIT}%s{UNIT}%b{RECORD}");
    let (n, skip) = (n.to_string(), format!("--skip={skip}"));
    let mut args = vec![
        "log",
        "--topo-order",
        "--decorate=full",
        // Numbers only: Git for Windows' strftime has no `%-d` or `%e`.
        "--date=format-local:%Y-%m-%d %H:%M",
        &format,
        "-n",
        &n,
        &skip,
    ];
    args.extend(revs.iter().map(String::as_str));
    args.push("--");
    let out = check(top, &args, None)?;
    Ok(parse_log(&out.stdout))
}

pub fn parse_log(raw: &str) -> Vec<Commit> {
    raw.split(RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches(['\n', '\r']);
            let parts: Vec<&str> = record.splitn(10, UNIT).collect();
            let [hash, short, parents, author, email, date, when, decorations, subject, body] = parts[..] else { return None };
            let (refs, detached_head) = parse_decorations(decorations);
            Some(Commit {
                hash: hash.into(),
                short: short.into(),
                parents: parents.split_whitespace().map(str::to_string).collect(),
                author: author.into(),
                email: email.into(),
                date: date.parse().unwrap_or(0),
                when: when.into(),
                subject: subject.into(),
                body: body.trim().into(),
                refs,
                detached_head,
            })
        })
        .collect()
}

/// `%D` with `--decorate=full`: `HEAD -> refs/heads/main, tag: refs/tags/v1,
/// refs/remotes/origin/main`. A bare `HEAD` means it is detached.
pub fn parse_decorations(raw: &str) -> (Vec<CommitRef>, bool) {
    let mut refs = Vec::new();
    let mut detached = false;
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (head, full) = match part.strip_prefix("HEAD -> ") {
            Some(rest) => (true, rest),
            None if part == "HEAD" => {
                detached = true;
                continue;
            }
            None => (false, part),
        };
        let (kind, name) = if let Some(name) = full.strip_prefix("refs/heads/") {
            (RefKind::Local, name)
        } else if let Some(name) = full.strip_prefix("refs/remotes/") {
            // `origin/HEAD` only points at another branch.
            if name.ends_with("/HEAD") {
                continue;
            }
            (RefKind::Remote, name)
        } else if let Some(name) = full.strip_prefix("tag: refs/tags/") {
            (RefKind::Tag, name)
        } else {
            continue;
        };
        refs.push(CommitRef { kind, name: name.to_string(), head });
    }
    (refs, detached)
}

/// The branch the remote checks out (`origin/main`, from `origin/HEAD`),
/// else a local `main` or `master`.
pub fn default_branch(top: &Path) -> Option<String> {
    if let Ok(out) = run(top, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"], None)
        && out.ok()
        && let Some(name) = out.stdout.trim().strip_prefix("refs/remotes/")
    {
        return Some(name.to_string());
    }
    ["main", "master"].into_iter().find(|b| is_commit(top, &format!("refs/heads/{b}"))).map(str::to_string)
}

fn is_commit(top: &Path, rev: &str) -> bool {
    run(top, &["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")], None).is_ok_and(|out| out.ok())
}

/// What the commit list shows, as VS Code's graph does: HEAD, its upstream
/// and the default branch, each only when it resolves (an upstream may be
/// gone).
pub fn history_revs(top: &Path, status: &Status) -> Vec<String> {
    let mut revs = vec!["HEAD".to_string()];
    revs.extend(status.branch.upstream.clone());
    revs.extend(default_branch(top));
    let mut seen = HashSet::new();
    revs.retain(|rev| seen.insert(rev.clone()) && is_commit(top, rev));
    revs
}

/// The URL of `origin`, else of the first remote.
pub fn remote_url(top: &Path) -> Option<String> {
    let url = |name: &str| run(top, &["remote", "get-url", name], None).ok().filter(|out| out.ok()).map(|out| out.stdout.trim().to_string()).filter(|u| !u.is_empty());
    url("origin").or_else(|| {
        let out = run(top, &["remote"], None).ok()?;
        let first = out.stdout.lines().map(str::trim).find(|r| !r.is_empty())?;
        url(first)
    })
}

/// `(owner, repo)` of a GitHub remote: `https://github.com/o/r.git`,
/// `git@github.com:o/r.git`, `ssh://git@github.com/o/r`.
pub fn github_repo(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let rest = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else {
        let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
        let without_user = without_scheme.split_once('@').map_or(without_scheme, |(_, rest)| rest);
        without_user.strip_prefix("github.com/")?
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| (owner.to_string(), repo.to_string()))
}

/// `2026-10-06 21:30` (as [`log`] asks git for) as VS Code shows it:
/// `October 6, 2026 at 9:30 PM`.
pub fn long_date(when: &str) -> Option<String> {
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let (date, time) = when.trim().split_once(' ')?;
    let mut date = date.split('-').map(|n| n.parse::<u32>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let (hour, minute) = time.split_once(':')?;
    let (hour, minute) = (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?);
    let month = MONTHS.get(month.checked_sub(1)? as usize)?;
    let (hour12, half) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    Some(format!("{month} {day}, {year} at {hour12}:{minute:02} {half}"))
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
    let out = check(top, &["diff-tree", "--no-commit-id", "-r", "-M", "--root", "--name-status", "-z", hash], None)?;
    Ok((parent, parse_name_status(&out.stdout)))
}

/// `M\0src/a.rs\0` and `R087\0old\0new\0` records (`-z`, so a name with
/// spaces or non-ASCII letters comes through as it is).
pub fn parse_name_status(raw: &str) -> Vec<CommitFile> {
    let mut files = Vec::new();
    let mut parts = raw.split('\0').filter(|p| !p.is_empty());
    while let Some(code) = parts.next() {
        let Some(status) = code.chars().next() else { continue };
        let Some(first) = parts.next() else { break };
        let (old_rel, rel) = match status {
            'R' | 'C' => match parts.next() {
                Some(second) => (first.to_string(), second.to_string()),
                None => break,
            },
            _ => (first.to_string(), first.to_string()),
        };
        files.push(CommitFile { status, rel, old_rel });
    }
    files
}

/// What a commit changed, as `--shortstat` counts it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

/// What `hash` changed against its first parent (everything, for a root).
pub fn commit_stat(top: &Path, hash: &str) -> Result<Stat, Error> {
    let parent = run(top, &["rev-parse", "--verify", "-q", &format!("{hash}^")], None)?;
    let parent = parent.ok().then(|| parent.stdout.trim().to_string()).filter(|p| !p.is_empty());
    let out = match &parent {
        Some(parent) => check(top, &["diff", "--shortstat", "-M", parent, hash], None)?,
        None => check(top, &["diff-tree", "--root", "-r", "-M", "--no-commit-id", "--shortstat", hash], None)?,
    };
    Ok(parse_shortstat(&out.stdout))
}

/// ` 2 files changed, 23 insertions(+), 2 deletions(-)`; parts git leaves
/// out count as zero.
pub fn parse_shortstat(raw: &str) -> Stat {
    let mut stat = Stat::default();
    for part in raw.lines().last().unwrap_or("").split(',').map(str::trim) {
        let n: u32 = part.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if part.contains("file") {
            stat.files = n;
        } else if part.contains("insertion") {
            stat.insertions = n;
        } else if part.contains("deletion") {
            stat.deletions = n;
        }
    }
    stat
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
        let files = parse_name_status("M\0src/a.rs\0R087\0old.rs\0new.rs\0A\0b.txt\0");
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
        assert_eq!(renamed.renamed_from.as_deref(), Some("lib/old.rs"));
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
        let raw = format!(
            "abc{UNIT}ab{UNIT}def 123{UNIT}Ann{UNIT}ann@x.io{UNIT}100{UNIT}2026-10-06 21:30{UNIT}HEAD -> refs/heads/main{UNIT}first{UNIT}A body.\n\nMore.\n{RECORD}\n\
             def{UNIT}de{UNIT}{UNIT}Bob{UNIT}bob@x.io{UNIT}200{UNIT}2026-10-05 09:05{UNIT}{UNIT}second{UNIT}\n{RECORD}\n"
        );
        let commits = parse_log(&raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents, ["def", "123"]);
        assert_eq!(commits[0].body, "A body.\n\nMore.");
        assert_eq!(commits[0].refs, [CommitRef { kind: RefKind::Local, name: "main".into(), head: true }]);
        assert_eq!((commits[1].author.as_str(), commits[1].email.as_str(), commits[1].date), ("Bob", "bob@x.io", 200));
        assert!(commits[1].parents.is_empty() && commits[1].body.is_empty() && commits[1].refs.is_empty());
        assert_eq!(commits[1].when, "2026-10-05 09:05");
    }

    #[test]
    fn parses_decorations() {
        let (refs, detached) = parse_decorations("HEAD -> refs/heads/main, tag: refs/tags/v0.6.0, refs/remotes/origin/main, refs/remotes/origin/HEAD, refs/stash");
        assert!(!detached);
        let names: Vec<(RefKind, &str, bool)> = refs.iter().map(|r| (r.kind, r.name.as_str(), r.head)).collect();
        assert_eq!(names, [(RefKind::Local, "main", true), (RefKind::Tag, "v0.6.0", false), (RefKind::Remote, "origin/main", false)]);
        let (refs, detached) = parse_decorations("HEAD, refs/heads/feature");
        assert!(detached);
        assert_eq!(refs, [CommitRef { kind: RefKind::Local, name: "feature".into(), head: false }]);
        assert_eq!(parse_decorations(""), (Vec::new(), false));
    }

    #[test]
    fn github_repo_forms() {
        let den = Some(("patrickiel".to_string(), "den".to_string()));
        assert_eq!(github_repo("https://github.com/patrickiel/den.git"), den);
        assert_eq!(github_repo("https://github.com/patrickiel/den/"), den);
        assert_eq!(github_repo("git@github.com:patrickiel/den.git"), den);
        assert_eq!(github_repo("ssh://git@github.com/patrickiel/den"), den);
        assert_eq!(github_repo("github.com/patrickiel/den"), den);
        assert_eq!(github_repo("https://gitlab.com/patrickiel/den.git"), None);
        assert_eq!(github_repo("https://github.com/patrickiel"), None);
    }

    #[test]
    fn parses_shortstat() {
        let stat = parse_shortstat(" 2 files changed, 23 insertions(+), 2 deletions(-)\n");
        assert_eq!(stat, Stat { files: 2, insertions: 23, deletions: 2 });
        let stat = parse_shortstat(" 1 file changed, 1 insertion(+)\n");
        assert_eq!(stat, Stat { files: 1, insertions: 1, deletions: 0 });
        assert_eq!(parse_shortstat(""), Stat::default());
    }

    #[test]
    fn long_dates_read_like_vscode() {
        assert_eq!(long_date("2026-10-06 21:30").as_deref(), Some("October 6, 2026 at 9:30 PM"));
        assert_eq!(long_date("2026-01-01 00:05").as_deref(), Some("January 1, 2026 at 12:05 AM"));
        assert_eq!(long_date("2026-12-31 12:00").as_deref(), Some("December 31, 2026 at 12:00 PM"));
        assert_eq!(long_date("2026-13-01 12:00"), None);
        assert_eq!(long_date("garbage"), None);
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
        let status = super::status(&top).unwrap();
        let revs = history_revs(&top, &status);
        assert_eq!(revs, ["HEAD", "main"]);
        let commits = log(&top, &revs, 10, 0).unwrap();
        assert_eq!(commits[0].subject, "first");
        assert_eq!(commits[0].email, "t@example.com");
        assert!(commits[0].parents.is_empty());
        assert_eq!(commits[0].refs, [CommitRef { kind: RefKind::Local, name: "main".into(), head: true }]);
        assert_eq!(commit_stat(&top, &commits[0].hash).unwrap(), Stat { files: 1, insertions: 1, deletions: 0 });

        // A branch with a tag, merged back: the graph's shape and decorations.
        check(&top, &["switch", "-q", "-c", "topic"], None).unwrap();
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        stage(&top, &["b.txt".into()]).unwrap();
        commit(&top, "topic", false).unwrap();
        check(&top, &["tag", "-a", "v1", "-m", "one"], None).unwrap();
        check(&top, &["switch", "-q", "main"], None).unwrap();
        std::fs::write(dir.join("a.txt"), "one more").unwrap();
        stage(&top, &["a.txt".into()]).unwrap();
        commit(&top, "main again", false).unwrap();
        check(&top, &["merge", "--no-ff", "-q", "-m", "merge topic", "topic"], None).unwrap();
        let commits = log(&top, &["HEAD".into()], 10, 0).unwrap();
        let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects[0], "merge topic");
        assert_eq!(commits[0].parents.len(), 2);
        assert_eq!(subjects[3], "first");
        let topic = commits.iter().find(|c| c.subject == "topic").unwrap();
        assert!(topic.refs.iter().any(|r| r.kind == RefKind::Tag && r.name == "v1"));
        assert!(topic.refs.iter().any(|r| r.kind == RefKind::Local && r.name == "topic" && !r.head));
        assert_eq!(commit_stat(&top, &commits[0].hash).unwrap().files, 1);

        // Paths go to git on stdin, as they are: a space, a non-ASCII letter.
        std::fs::write(dir.join("with space.txt"), "s").unwrap();
        std::fs::write(dir.join("ü.txt"), "u").unwrap();
        stage(&top, &["with space.txt".into(), "ü.txt".into()]).unwrap();
        assert_eq!(super::status(&top).unwrap().files.iter().filter(|f| f.staged()).count(), 2);
        commit(&top, "names", false).unwrap();
        let head = log(&top, &["HEAD".into()], 1, 0).unwrap().remove(0);
        let names: Vec<String> = commit_files(&top, &head.hash).unwrap().1.into_iter().map(|f| f.rel).collect();
        assert_eq!(names, ["with space.txt", "ü.txt"]);
        // A staged rename is unstaged as one: the old path comes back deleted,
        // the new one untracked.
        check(&top, &["mv", "with space.txt", "moved.txt"], None).unwrap();
        let status = super::status(&top).unwrap();
        let renamed = status.files.iter().find(|f| f.rel == "moved.txt").unwrap();
        assert_eq!(renamed.renamed_from.as_deref(), Some("with space.txt"));
        unstage(&top, &["moved.txt".into(), "with space.txt".into()]).unwrap();
        let letters: Vec<(String, char)> = super::status(&top).unwrap().files.iter().map(|f| (f.rel.clone(), f.letter())).collect();
        assert!(letters.contains(&("moved.txt".into(), 'U')) && letters.contains(&("with space.txt".into(), 'D')), "{letters:?}");
        std::fs::rename(dir.join("moved.txt"), dir.join("with space.txt")).unwrap();

        std::fs::write(dir.join("a.txt"), "two").unwrap();
        let changed = super::status(&top).unwrap().files;
        discard(&top, &changed).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one more");
        assert_eq!(show(&top, "HEAD", "a.txt").unwrap(), "one more");
        std::fs::remove_dir_all(&dir).ok();
    }
}
