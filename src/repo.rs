//! The git repository the session root is in, shared by Source Control and
//! the Explorer: its status (refreshed from the file watcher, on focus and
//! after every operation), recent commits, and the operations, all run off
//! the UI thread.

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
    Ready { top: PathBuf, status: Arc<Status> },
}

/// A path as a map key: case-insensitive where the file system is.
pub fn key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = text.trim_end_matches('\\');
    if cfg!(windows) { text.to_lowercase() } else { text.to_string() }
}

pub struct Repo {
    root: PathBuf,
    pub state: RepoState,
    pub commits: Vec<Commit>,
    /// The graph through each row: the uncommitted changes first when there
    /// are any, then the commits (see [`Self::row_graph`]).
    pub graph: Vec<Graph>,
    /// Each row's lane, for its badges' colour.
    pub lanes: Vec<usize>,
    /// How many files the working tree changes, when its row leads the graph.
    pub uncommitted: Option<usize>,
    /// The GitHub repository the remote is, as `(owner, repo)`.
    pub github: Option<(String, String)>,
    /// The last operation's failure, with git's message.
    pub error: Option<String>,
    /// What runs now ("Committing…"), for the view to show.
    pub busy: Option<&'static str>,
    /// File letters by path key, for the Explorer.
    letters: HashMap<String, char>,
    /// Folders holding changes.
    changed_dirs: HashSet<String>,
    ignored: HashSet<String>,
    commit_count: usize,
    /// Push once the running operation (a commit) is done.
    push_next: bool,
    _refresh: Option<Task<()>>,
    _operation: Option<Task<()>>,
}

impl Repo {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root,
            state: RepoState::Loading,
            commits: Vec::new(),
            graph: Vec::new(),
            lanes: Vec::new(),
            uncommitted: None,
            github: None,
            error: None,
            busy: None,
            letters: HashMap::new(),
            changed_dirs: HashSet::new(),
            ignored: HashSet::new(),
            commit_count: 50,
            push_next: false,
            _refresh: None,
            _operation: None,
        };
        this.refresh(cx);
        this
    }

    pub fn top(&self) -> Option<&Path> {
        match &self.state {
            RepoState::Ready { top, .. } => Some(top),
            _ => None,
        }
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
        self.lanes.get(ix + self.uncommitted.is_some() as usize).copied().unwrap_or(0)
    }

    /// The commit's page on GitHub, when the remote is there.
    pub fn github_commit_url(&self, hash: &str) -> Option<String> {
        self.github.as_ref().map(|(owner, repo)| format!("https://github.com/{owner}/{repo}/commit/{hash}"))
    }

    /// Read status and history again, off the UI thread.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let count = self.commit_count;
        self._refresh = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let top = git::toplevel(&root)?;
                    let Some(top) = top else { return Ok(None) };
                    let status = git::status(&top)?;
                    let revs = git::history_revs(&top, &status);
                    let commits = git::log(&top, &revs, count, 0).unwrap_or_default();
                    let github = git::remote_url(&top).and_then(|url| git::github_repo(&url));
                    let (layout, uncommitted) = lay_out(&status, &commits);
                    Ok::<_, Error>(Some((top, status, commits, layout, uncommitted, github)))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                match result {
                    Err(Error::NotFound) => this.state = RepoState::NoGit,
                    Err(err) => this.error = Some(err.to_string()),
                    Ok(None) => this.state = RepoState::NoRepo,
                    Ok(Some((top, status, commits, layout, uncommitted, github))) => {
                        this.index(&top, &status);
                        this.commits = commits;
                        this.graph = layout.graphs;
                        this.lanes = layout.lanes;
                        this.uncommitted = uncommitted;
                        this.github = github;
                        this.state = RepoState::Ready {
                            top,
                            status: Arc::new(status),
                        };
                    }
                }
                cx.notify();
            });
        }));
    }

    fn index(&mut self, top: &Path, status: &Status) {
        self.letters.clear();
        self.changed_dirs.clear();
        for file in &status.files {
            self.letters.insert(key(&file.path), file.letter());
            let mut dir = file.path.parent();
            while let Some(d) = dir {
                if !d.starts_with(top) || !self.changed_dirs.insert(key(d)) {
                    break;
                }
                dir = d.parent();
            }
        }
        self.ignored = status.ignored.iter().map(|p| key(p)).collect();
    }

    /// The letter for a file in the Explorer (M, U, A, D, R, !).
    pub fn letter(&self, path: &Path) -> Option<char> {
        self.letters.get(&key(path)).copied()
    }

    pub fn has_changes_under(&self, dir: &Path) -> bool {
        self.changed_dirs.contains(&key(dir))
    }

    /// Ignored itself or inside an ignored folder.
    pub fn is_ignored(&self, path: &Path) -> bool {
        let top = self.top();
        let mut at = Some(path);
        while let Some(p) = at {
            if self.ignored.contains(&key(p)) {
                return true;
            }
            if Some(p) == top {
                return false;
            }
            at = p.parent();
        }
        false
    }

    /// Run an operation on the repository, then refresh. Its failure shows
    /// in the view with git's message; it never opens a dialog.
    pub fn run(&mut self, label: &'static str, op: impl FnOnce(&Path) -> Result<(), Error> + Send + 'static, cx: &mut Context<Self>) {
        let Some(top) = self.top().map(Path::to_path_buf) else { return };
        self.busy = Some(label);
        self.error = None;
        cx.notify();
        self._operation = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { op(&top) }).await;
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

    /// Commit what is staged; with nothing staged, everything (as den does
    /// once "Always" was chosen).
    /// Commit, staging every change first with `stage_all`.
    pub fn commit(&mut self, message: String, amend: bool, stage_all: bool, cx: &mut Context<Self>) {
        self.run(
            "Committing…",
            move |top| {
                if stage_all {
                    git::check(top, &["add", "-A"], None)?;
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
        let Some(branch) = self.status().map(|s| s.branch.clone()) else { return };
        self.run("Pushing…", move |top| git::push(top, &branch), cx);
    }

    pub fn switch(&mut self, branch: BranchRef, cx: &mut Context<Self>) {
        self.run("Checking out…", move |top| git::switch(top, &branch), cx);
    }

    pub fn create_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.run("Creating branch…", move |top| git::create_branch(top, &name), cx);
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
            let result = cx.background_executor().spawn(async move { git::check(&root, &["init"], None) }).await;
            _ = this.update(cx, |this, cx| {
                this.busy = None;
                if let Err(err) = result {
                    this.error = Some(err.to_string());
                }
                this.refresh(cx);
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

/// The paths git takes for `files`: a renamed file's old path too, so the
/// rename moves as one.
fn paths_of(files: Vec<FileStatus>) -> Vec<String> {
    files.into_iter().flat_map(|f| [Some(f.rel), f.renamed_from]).flatten().collect()
}

/// The graph's lanes through the working tree (when it has changes, as a
/// hollow dot above HEAD) and the commits; HEAD's dot is hollow too.
fn lay_out(status: &Status, commits: &[Commit]) -> (git_graph::Layout, Option<usize>) {
    let head = (status.branch.oid != "(initial)" && !status.branch.oid.is_empty()).then(|| status.branch.oid.clone());
    let uncommitted = (!status.files.is_empty() && head.is_some()).then_some(status.files.len());
    let head_parent: Vec<String> = head.iter().cloned().collect();
    let mut nodes = Vec::with_capacity(commits.len() + 1);
    if uncommitted.is_some() {
        nodes.push(git_graph::Node { hash: "*", parents: &head_parent, hollow: true, uncommitted: true });
    }
    nodes.extend(commits.iter().map(|c| git_graph::Node { hash: &c.hash, parents: &c.parents, hollow: head.as_deref() == Some(&c.hash), uncommitted: false }));
    (git_graph::layout(&nodes), uncommitted)
}

#[cfg(test)]
mod tests {
    use super::key;
    use std::path::Path;

    #[test]
    fn keys_ignore_case_and_separators_on_windows() {
        if cfg!(windows) {
            assert_eq!(key(Path::new(r"C:\Repo\Src\")), key(Path::new("c:/repo/src")));
        }
    }
}
