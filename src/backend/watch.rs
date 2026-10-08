//! Watching the session root, as den does: changes on disk (an agent editing a
//! file, `git checkout`, a build) arrive in batches so the Explorer, Source
//! Control and open files can follow them.

use std::{
    collections::BTreeSet,
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use futures::{
    StreamExt as _,
    channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded},
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use super::{process, wsl};

/// Keeps the watch alive; dropping it stops it.
pub struct Watcher {
    _native: Option<RecommendedWatcher>,
    /// The watch running inside WSL, for a folder there.
    wsl: Option<Child>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(child) = &mut self.wsl {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Run in the distribution with the folder as `$1`: one changed path per
/// line, relative (`./src/main.rs`). inotifywait when it is installed, else
/// a look each second for what changed since the last; dependencies and
/// git's objects are left out.
const WSL_WATCH: &str = r#"cd "$1" || exit 1
if command -v inotifywait >/dev/null 2>&1; then
  exec inotifywait -m -r -q --format %w%f -e modify,create,delete,move,attrib --exclude '/(node_modules|\.git/objects)(/|$)' .
fi
m=$(mktemp) || exit 1
trap 'rm -f "$m" "$n"' EXIT HUP INT TERM
while sleep 1; do
  n=$(mktemp) || exit 1
  find . \( -name node_modules -o -path ./.git/objects \) -prune -o -newer "$m" -print || exit 1
  rm -f "$m"
  m=$n
done
"#;

/// Watch `root` and everything under it. A drive root is not watched (too
/// much churn), as in den.
pub fn watch(root: &Path) -> Result<(Watcher, UnboundedReceiver<PathBuf>), String> {
    if root.parent().is_none() {
        return Err("A drive root is not watched.".into());
    }
    let (tx, rx) = unbounded();
    // Windows reports no changes from inside WSL.
    if cfg!(windows)
        && let Some((distro, linux)) = wsl::split(root)
    {
        return watch_wsl(root, &distro, &linux, tx).map(|watcher| (watcher, rx));
    }
    let mut inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                let _ = tx.unbounded_send(path);
            }
        }
    })
    .map_err(|e| e.to_string())?;
    inner.watch(root, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
    Ok((Watcher { _native: Some(inner), wsl: None }, rx))
}

/// Watch `root`, the folder `linux` in `distro`, from inside WSL.
fn watch_wsl(root: &Path, distro: &str, linux: &str, tx: UnboundedSender<PathBuf>) -> Result<Watcher, String> {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-d", distro, "--exec", "/bin/sh", "-c", WSL_WATCH, "sh", linux])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    process::no_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("The WSL watch has no output.")?;
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let mut path = root.clone();
            path.extend(line.split('/').filter(|part| !part.is_empty() && *part != "."));
            if tx.unbounded_send(path).is_err() {
                break;
            }
        }
    });
    Ok(Watcher { _native: None, wsl: Some(child) })
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

/// Changes git makes inside `.git` that matter (the index, HEAD, refs, the
/// config with its remotes), as opposed to the objects and logs it churns
/// through.
pub fn is_git_state(path: &Path) -> bool {
    let mut components = path.components().map(|c| c.as_os_str().to_string_lossy().to_string());
    if !components.any(|c| c == ".git") {
        return false;
    }
    let rest: Vec<String> = components.collect();
    match rest.first().map(String::as_str) {
        Some("index" | "HEAD" | "MERGE_HEAD" | "FETCH_HEAD" | "ORIG_HEAD" | "config") => true,
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
        assert!(is_git_state(Path::new("/r/.git/config")));
        assert!(!is_git_state(Path::new("/r/.git/objects/ab/cdef")));
        assert!(!is_git_state(Path::new("/r/src/main.rs")));
        assert!(in_git_dir(Path::new("/r/.git/objects/ab")));
        assert!(!in_git_dir(Path::new("/r/src/git.rs")));
    }
}
