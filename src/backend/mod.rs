//! The work den does off the UI thread: search, git, watching the disk,
//! running extensions, downloads.

pub mod agent;
pub mod ai;
pub mod avatars;
pub mod commit_ai;
pub mod extensions;
pub mod format;
pub mod git;
pub mod http;
pub mod process;
pub mod search;
pub mod watch;

use std::path::{Path, PathBuf};

/// `name` in `dir`, or in a folder up to `depth` levels below it.
pub fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_dir()).find_map(|e| find_file(&e.path(), name, depth - 1))
}
