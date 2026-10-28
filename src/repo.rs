//! The git repository the session root is in, shared by Source Control and
//! the Explorer: its status (refreshed from the file watcher, on focus and
//! after every operation), recent commits, and the operations, all run off
//! the UI thread. A refresh reads only as much as what changed asks for
//! ([`Refresh`]), and one asked for while another runs follows it.
//!
//! The root may be a folder inside the repository (a monorepo): then the
//! status and the commits can be scoped to it ([`Repo::scoped`]), which is
//! what the rest of den sees.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use den_extension::view::Graph;
use gpui_kit::*;

use crate::backend::git::{self, BranchRef, Commit, Error, FileStatus, Status};
use crate::git_graph;

#[derive(Clone, Debug)]
pub enum RepoState {
    Loading,
    /// No `git` on the PATH.
    NoGit,
    /// The folder is not in a repository.
    NoRepo,
    Ready {
        top: PathBuf,
        status: Arc<Status>,
    },
}

/// A path as a map key: case-insensitive where the file system is.
pub fn key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    } else {
        // macOS file systems are usually case-insensitive too, but git and
        // the shell are not; paths are compared as they are.
        let text = text.trim_end_matches('/');
        if text.is_empty() {
            "/".to_string()
        } else {
            text.to_string()
        }
    }
}

/// How much a refresh reads again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refresh {
    /// The status: files changed on disk. The commits stay.
    Status,
    /// The status and the commits: an operation ran, the window came back.
    Full,
    /// Everything, the remote and the default branch too: the first time,
    /// `git init`, the Refresh button, a change to `.git/config`.
    Reload,
}

pub struct Repo {
    root: PathBuf,
    /// Only what is under the root, when that is inside the repository.
    scoped: bool,
    pub state: RepoState,
    pub commits: Arc<Vec<Commit>>,
    /// The graph through each row: the uncommitted changes first when there
    /// are any, then the commits (see [`Self::row_graph`]).
    pub graph: Vec<Graph>,
    /// Each row's lane, for its badges' colour.
    pub lanes: Vec<usize>,
    /// How many files the working tree changes, when its row leads the graph.
    pub uncommitted: Option<usize>,
    /// The GitHub repository the remote is, as `(owner, repo)`.
    pub github: Option<(String, String)>,
    /// `origin/main`, else a local `main`: the commits are listed from it too.
    default_branch: Option<String>,
    /// The last operation's failure, with git's message.
    pub error: Option<String>,
    /// What runs now ("Committing…"), for the view to show.
    pub busy: Option<&'static str>,
    index: Index,
    commit_count: usize,
    /// Push once the running operation (a commit) is done.
    push_next: bool,
    /// A refresh runs; `again` is the one asked for meanwhile, run after it.
    refreshing: bool,
    again: Option<Refresh>,
    _refresh: Option<Task<()>>,
    _operation: Option<Task<()>>,
}

/// The status as the Explorer asks for it row by row, by [`key`].
#[derive(Default)]
struct Index {
    /// File letters.
    letters: HashMap<String, char>,
    /// Folders holding changes.
    changed_dirs: HashSet<String>,
    ignored: HashSet<String>,
}

/// What a refresh read.
struct Loaded {
    top: PathBuf,
    status: Status,
    default_branch: Option<String>,
    github: Option<(String, String)>,
    commits: Arc<Vec<Commit>>,
    layout: git_graph::Layout,
    uncommitted: Option<usize>,
    index: Index,
}

impl Repo {
    pub fn new(root: PathBuf, scoped: bool, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root,
            scoped,
            state: RepoState::Loading,
            commits: Arc::new(Vec::new()),
            graph: Vec::new(),
            lanes: Vec::new(),
            uncommitted: None,
            github: None,
            default_branch: None,
            error: None,
            busy: None,
            index: Index::default(),
            commit_count: 50,
            push_next: false,
            refreshing: false,
            again: None,
            _refresh: None,
            _operation: None,
        };
        this.reload(cx);
        this
    }

    /// The folder the window is on (the repository's top may be above it).
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn top(&self) -> Option<&Path> {
        match &self.state {
            RepoState::Ready { top, .. } => Some(top),
            _ => None,
        }
    }

    /// The root inside the repository, relative to its top with `/`
    /// separators: `None` when the root is the top (or not in one).
    pub fn subdir(&self) -> Option<String> {
        subdir_of(self.top()?, &self.root)
    }

    /// Whether the status and the commits are limited to the root; it
    /// matters only when the root is a [`Self::subdir`].
    pub fn scoped(&self) -> bool {
        self.scoped
    }

    /// Limit the status and the commits to the root, or show the whole
    /// repository again.
    pub fn set_scoped(&mut self, scoped: bool, cx: &mut Context<Self>) {
        if self.scoped == scoped {
            return;
        }
        self.scoped = scoped;
        cx.notify();
        self.refresh(cx);
    }

    /// The pathspec every git read and write is limited to: the subdir,
    /// while scoped.
    pub fn scope(&self) -> Option<String> {
        self.scoped.then(|| self.subdir()).flatten()
    }

    pub fn status(&self) -> Option<&Status> {
        match &self.state {
            RepoState::Ready { status, .. } => Some(status),
            _ => None,
        }
    }

    /// The graph through the row of commit `ix` (the uncommitted changes
    /// take the row before the first commit).
    pub fn row_graph(&self, ix: usize) -> Option<&Graph> {
        self.graph.get(ix + self.uncommitted.is_some() as usize)
    }

    /// The lane of commit `ix`, for its badges' colour.
    pub fn row_lane(&self, ix: usize) -> usize {
        self.lanes
            .get(ix + self.uncommitted.is_some() as usize)
            .copied()
            .unwrap_or(0)
    }

    /// The commit's page on GitHub, when the remote is there.
    pub fn github_commit_url(&self, hash: &str) -> Option<String> {
        self.github
            .as_ref()
            .map(|(owner, repo)| format!("https://github.com/{owner}/{repo}/commit/{hash}"))
    }

    /// Read the status and the commits again, off the UI thread.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.request(Refresh::Full, cx);
    }

    /// Read everything again, the remote and the default branch too.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.request(Refresh::Reload, cx);
    }

    /// Refresh `level` deep, off the UI thread; while one runs, one of at
    /// least that depth follows it instead.
    pub fn request(&mut self, level: Refresh, cx: &mut Context<Self>) {
        if self.refreshing {
            self.again = Some(self.again.map_or(level, |again| again.max(level)));
            return;
        }
        self.refreshing = true;
        // The top level known from the last time; none means everything anew.
        let known = self
            .top()
            .map(Path::to_path_buf)
            .filter(|_| level < Refresh::Reload);
        let (root, count, scoped) = (self.root.clone(), self.commit_count, self.scoped);
        let (default_branch, github, commits) = (
            self.default_branch.clone(),
            self.github.clone(),
            self.commits.clone(),
        );
        self._refresh = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let reload = known.is_none();
                    let top = match known {
                        Some(top) => top,
                        None => match git::toplevel(&root)? {
                            Some(top) => top,
                            None => return Ok(None),
                        },
                    };
                    let mut status = git::status(&top)?;
                    let scope = scoped.then(|| subdir_of(&top, &root)).flatten();
                    if scope.is_some() {
                        let under = key(&root);
                        status.files.retain(|f| under_key(&under, &f.path));
                    }
                    let (default_branch, github) = if reload {
                        (
                            git::default_branch(&top),
                            git::remote_url(&top).and_then(|url| git::github_repo(&url)),
                        )
                    } else {
                        (default_branch, github)
                    };
                    let commits = if level == Refresh::Status && !reload {
                        commits
                    } else if status.branch.oid == "(initial)" {
                        Arc::new(Vec::new())
                    } else {
                        // The default branch found earlier may be gone: then HEAD alone.
                        let revs = git::history_revs(&status, default_branch.as_deref());
                        let scope = scope.as_deref();
                        let log = git::log(&top, &revs, count, 0, scope)
                            .or_else(|_| git::log(&top, &["HEAD".into()], count, 0, scope));
                        Arc::new(log.unwrap_or_default())
                    };
                    let (layout, uncommitted) = lay_out(&status, &commits);
                    let index = index(&top, &status);
                    Ok::<_, Error>(Some(Loaded {
                        top,
                        status,
                        default_branch,
                        github,
                        commits,
                        layout,
                        uncommitted,
                        index,
                    }))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                match result {
                    Err(Error::NotFound) => this.state = RepoState::NoGit,
                    Err(err) => this.error = Some(err.to_string()),
                    Ok(None) => this.state = RepoState::NoRepo,
                    Ok(Some(loaded)) => this.apply(loaded),
                }
                cx.notify();
                if let Some(level) = this.again.take() {
                    this.request(level, cx);
                }
            });
        }));
    }

    fn apply(&mut self, loaded: Loaded) {
        self.index = loaded.index;
        self.commits = loaded.commits;
        self.graph = loaded.layout.graphs;
        self.lanes = loaded.layout.lanes;
        self.uncommitted = loaded.uncommitted;
        self.default_branch = loaded.default_branch;
        self.github = loaded.github;
        self.state = RepoState::Ready {
            top: loaded.top,
            status: Arc::new(loaded.status),
        };
    }

    /// The letter for a file in the Explorer (M, U, A, D, R, !).
    pub fn letter(&self, path: &Path) -> Option<char> {
        self.index.letters.get(&key(path)).copied()
    }

    pub fn has_changes_under(&self, dir: &Path) -> bool {
        self.index.changed_dirs.contains(&key(dir))
    }

    /// Ignored itself or inside an ignored folder.
    pub fn is_ignored(&self, path: &Path) -> bool {
        ignored_under(&self.index.ignored, self.top(), path)
    }

    /// Run an operation on the repository, then refresh. Its failure shows
    /// in the view with git's message; it never opens a dialog.
    pub fn run(
        &mut self,
        label: &'static str,
        op: impl FnOnce(&Path) -> Result<(), Error> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(top) = self.top().map(Path::to_path_buf) else {
            return;
        };
        self.busy = Some(label);
        self.error = None;
        cx.notify();
        self._operation = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { op(&top) })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Err(err) => {
                        this.error = Some(err.to_string());
                        this.push_next = false;
                    }
                    Ok(()) if std::mem::take(&mut this.push_next) => {
                        this.push(cx);
                        return;
                    }
                    Ok(()) => {}
                }
                this.refresh(cx);
            });
        }));
    }

    pub fn stage(&mut self, files: Vec<FileStatus>, cx: &mut Context<Self>) {
        let rels = paths_of(files);
        self.run("Staging…", move |top| git::stage(top, &rels), cx);
    }

    pub fn unstage(&mut self, files: Vec<FileStatus>, cx: &mut Context<Self>) {
        let rels = paths_of(files);
        self.run("Unstaging…", move |top| git::unstage(top, &rels), cx);
    }

    pub fn discard(&mut self, files: Vec<FileStatus>, cx: &mut Context<Self>) {
        self.run("Discarding…", move |top| git::discard(top, &files), cx);
    }

    /// Commit what is staged; with `stage_all`, every change (in the scope)
    /// is staged first, as den does once "Always" was chosen.
    pub fn commit(
        &mut self,
        message: String,
        amend: bool,
        stage_all: bool,
        cx: &mut Context<Self>,
    ) {
        let scope = self.scope();
        self.run(
            "Committing…",
            move |top| {
                if stage_all {
                    let mut args = vec!["add", "-A", "--"];
                    args.extend(scope.as_deref());
                    git::check(top, &args, None)?;
                }
                git::commit(top, &message, amend)
            },
            cx,
        );
    }

    pub fn fetch(&mut self, cx: &mut Context<Self>) {
        self.run("Fetching…", git::fetch, cx);
    }

    pub fn pull(&mut self, cx: &mut Context<Self>) {
        self.run("Pulling…", git::pull, cx);
    }

    pub fn push(&mut self, cx: &mut Context<Self>) {
        let Some(branch) = self.status().map(|s| s.branch.clone()) else {
            return;
        };
        self.run("Pushing…", move |top| git::push(top, &branch), cx);
    }

    pub fn switch(&mut self, branch: BranchRef, cx: &mut Context<Self>) {
        self.run("Checking out…", move |top| git::switch(top, &branch), cx);
    }

    pub fn create_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.run(
            "Creating branch…",
            move |top| git::create_branch(top, &name),
            cx,
        );
    }

    /// Push after the commit that is running now.
    pub fn push_after_commit(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.push_next = true;
        } else {
            self.push(cx);
        }
    }

    /// `git init` in the session root, for a folder without a repository.
    pub fn init(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.busy = Some("Initializing…");
        cx.notify();
        self._operation = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { git::check(&root, &["init"], None) })
                .await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                if let Err(err) = result {
                    this.error = Some(err.to_string());
                }
                this.reload(cx);
            });
        }));
    }

    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        self.commit_count += 50;
        self.refresh(cx);
    }

    pub fn clear_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }
}

/// `root` relative to `top` with `/` separators, when it is strictly inside
/// it (compared as [`key`]s, so case does not matter where the file system
/// ignores it).
pub fn subdir_of(top: &Path, root: &Path) -> Option<String> {
    let (top_key, root_key) = (key(top), key(root));
    if root_key == top_key || !under_key(&top_key, root) {
        return None;
    }
    let depth = root.components().count() - top.components().count();
    let parts: Vec<String> = root
        .components()
        .rev()
        .take(depth)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.into_iter().rev().collect::<Vec<_>>().join("/"))
}

/// `path` is `dir` (given as its [`key`]) or inside it.
fn under_key(dir: &str, path: &Path) -> bool {
    let path = key(path);
    match path.strip_prefix(dir) {
        Some("") => true,
        Some(rest) => {
            rest.starts_with(std::path::MAIN_SEPARATOR) || dir.ends_with(std::path::MAIN_SEPARATOR)
        }
        None => false,
    }
}

/// The status by path key: each changed file's letter, the folders holding
/// changes (up to `top`), and what is ignored.
fn index(top: &Path, status: &Status) -> Index {
    let mut index = Index::default();
    for file in &status.files {
        index.letters.insert(key(&file.path), file.letter());
        let mut dir = file.path.parent();
        while let Some(d) = dir {
            if !d.starts_with(top) || !index.changed_dirs.insert(key(d)) {
                break;
            }
            dir = d.parent();
        }
    }
    index.ignored = status.ignored.iter().map(|p| key(p)).collect();
    index
}

/// `path` is in `ignored`, or in a folder (up to `top`) that is.
fn ignored_under(ignored: &HashSet<String>, top: Option<&Path>, path: &Path) -> bool {
    let mut at = Some(path);
    while let Some(p) = at {
        if ignored.contains(&key(p)) {
            return true;
        }
        if Some(p) == top {
            return false;
        }
        at = p.parent();
    }
    false
}

/// The paths git takes for `files`: a renamed file's old path too, so the
/// rename moves as one.
fn paths_of(files: Vec<FileStatus>) -> Vec<String> {
    files
        .into_iter()
        .flat_map(|f| [Some(f.rel), f.renamed_from])
        .flatten()
        .collect()
}

/// The graph's lanes through the working tree (when it has changes, as a
/// hollow dot above HEAD) and the commits; HEAD's dot is hollow too.
fn lay_out(status: &Status, commits: &[Commit]) -> (git_graph::Layout, Option<usize>) {
    let head = (status.branch.oid != "(initial)" && !status.branch.oid.is_empty())
        .then(|| status.branch.oid.clone());
    let uncommitted = (!status.files.is_empty() && head.is_some()).then_some(status.files.len());
    let head_parent: Vec<String> = head.iter().cloned().collect();
    let mut nodes = Vec::with_capacity(commits.len() + 1);
    if uncommitted.is_some() {
        nodes.push(git_graph::Node {
            hash: "*",
            parents: &head_parent,
            hollow: true,
            uncommitted: true,
        });
    }
    nodes.extend(commits.iter().map(|c| git_graph::Node {
        hash: &c.hash,
        parents: &c.parents,
        hollow: head.as_deref() == Some(&c.hash),
        uncommitted: false,
    }));
    (git_graph::layout(&nodes), uncommitted)
}

#[cfg(test)]
mod tests {
    use super::{ignored_under, index, key, subdir_of, under_key};
    use crate::backend::git::{FileStatus, Status};
    use std::{collections::HashSet, path::Path};

    #[test]
    fn subdir_is_the_root_inside_the_top() {
        let top = Path::new(if cfg!(windows) { r"C:\mono" } else { "/mono" });
        let apps = top.join("apps").join("assistant");
        assert_eq!(subdir_of(top, &apps).as_deref(), Some("apps/assistant"));
        assert_eq!(subdir_of(top, top), None);
        assert_eq!(subdir_of(top, top.parent().unwrap()), None);
        // A sibling that shares the top's name as a prefix is not inside it.
        assert_eq!(subdir_of(top, &top.with_file_name("monorepo")), None);
        let under = key(&apps);
        assert!(under_key(&under, &apps.join("src").join("a.rs")));
        assert!(under_key(&under, &apps));
        assert!(!under_key(
            &under,
            &top.join("apps").join("assistant2").join("x")
        ));
        assert!(!under_key(&under, &top.join("dotfiles").join("x")));
    }

    #[test]
    fn keys_ignore_case_and_separators_on_windows() {
        if cfg!(windows) {
            assert_eq!(
                key(Path::new(r"C:\Repo\Src\")),
                key(Path::new("c:/repo/src"))
            );
        }
    }

    #[test]
    fn index_keys_files_folders_and_ignored() {
        let top = Path::new(if cfg!(windows) { r"C:\repo" } else { "/repo" });
        let file = FileStatus {
            path: top.join("src").join("a.rs"),
            rel: "src/a.rs".into(),
            index: '.',
            worktree: 'M',
            renamed_from: None,
            conflict: false,
        };
        let status = Status {
            files: vec![file],
            ignored: HashSet::from([top.join("target")]),
            ..Status::default()
        };
        let index = index(top, &status);
        assert_eq!(
            index.letters.get(&key(&top.join("src").join("a.rs"))),
            Some(&'M')
        );
        assert!(
            index.changed_dirs.contains(&key(&top.join("src")))
                && index.changed_dirs.contains(&key(top))
        );
        assert!(!index.changed_dirs.contains(&key(top.parent().unwrap())));
        assert!(ignored_under(
            &index.ignored,
            Some(top),
            &top.join("target").join("debug").join("x.o")
        ));
        assert!(!ignored_under(
            &index.ignored,
            Some(top),
            &top.join("src").join("a.rs")
        ));
    }
}
