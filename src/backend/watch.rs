//! Watching the session root, as den does: changes on disk (an agent editing a
//! file, `git checkout`, a build) arrive in batches so the Explorer, Source
//! Control and open files can follow them.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

use futures::{
    StreamExt as _,
    channel::mpsc::{UnboundedReceiver, unbounded},
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

/// Keeps the watch alive; dropping it stops it.
pub struct Watcher {
    _inner: RecommendedWatcher,
}

/// Watch `root` and everything under it. A drive root is not watched (too
/// much churn), as in den.
pub fn watch(root: &Path) -> Result<(Watcher, UnboundedReceiver<PathBuf>), String> {
    if root.parent().is_none() {
        return Err("A drive root is not watched.".into());
    }
    let (tx, rx) = unbounded();
    let mut inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                let _ = tx.unbounded_send(path);
            }
        }
    })
    .map_err(|e| e.to_string())?;
    inner.watch(root, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
    Ok((Watcher { _inner: inner }, rx))
}

/// The next batch: the first change, then whatever else arrives within
/// `quiet` of the last one. `None` once the watch is gone.
pub async fn next_batch(rx: &mut UnboundedReceiver<PathBuf>, quiet: Duration, timer: impl Fn(Duration) -> futures::future::BoxFuture<'static, ()>) -> Option<BTreeSet<PathBuf>> {
    let first = rx.next().await?;
    let mut batch = BTreeSet::from([first]);
    loop {
        timer(quiet).await;
        let mut more = false;
        while let Ok(path) = rx.try_recv() {
            batch.insert(path);
            more = true;
        }
        if !more {
            return Some(batch);
        }
    }
}

/// Changes git makes inside `.git` that matter (the index, HEAD, refs), as
/// opposed to the objects and logs it churns through.
pub fn is_git_state(path: &Path) -> bool {
    let mut components = path.components().map(|c| c.as_os_str().to_string_lossy().to_string());
    if !components.any(|c| c == ".git") {
        return false;
    }
    let rest: Vec<String> = components.collect();
    match rest.first().map(String::as_str) {
        Some("index" | "HEAD" | "MERGE_HEAD" | "FETCH_HEAD" | "ORIG_HEAD") => true,
        Some("refs") => true,
        _ => false,
    }
}

/// Inside a `.git` folder.
pub fn in_git_dir(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_state_files_count_and_objects_do_not() {
        assert!(is_git_state(Path::new("/r/.git/index")));
        assert!(is_git_state(Path::new("/r/.git/HEAD")));
        assert!(is_git_state(Path::new("/r/.git/refs/heads/main")));
        assert!(!is_git_state(Path::new("/r/.git/objects/ab/cdef")));
        assert!(!is_git_state(Path::new("/r/src/main.rs")));
        assert!(in_git_dir(Path::new("/r/.git/objects/ab")));
        assert!(!in_git_dir(Path::new("/r/src/git.rs")));
    }
}
