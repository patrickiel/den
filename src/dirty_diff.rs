//! Quick diff in the editor gutter, as VS Code and den show it: which lines
//! of the buffer (unsaved edits included) differ from the file in the git
//! index. Green for added lines, blue for changed ones, a red mark where
//! lines were deleted.

use std::path::Path;

use similar::{DiffOp, TextDiff};

use crate::backend::git;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Added,
    Modified,
    /// Lines were deleted just above this line.
    Deleted,
}

/// Files above this many bytes get no marks (the diff would cost too much
/// per keystroke).
const MAX_BYTES: usize = 2_000_000;

/// The marked lines (from 0) of `current` against `base`.
pub fn marks(base: &str, current: &str) -> Vec<(usize, Mark)> {
    if base.len() > MAX_BYTES || current.len() > MAX_BYTES {
        return Vec::new();
    }
    // The index has LF where the buffer may have CRLF (core.autocrlf).
    let (base, current) = (base.replace("\r\n", "\n"), current.replace("\r\n", "\n"));
    let diff = TextDiff::from_lines(&base, &current);
    let mut out = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert { new_index, new_len, .. } => out.extend((new_index..new_index + new_len).map(|line| (line, Mark::Added))),
            DiffOp::Delete { new_index, .. } => out.push((new_index, Mark::Deleted)),
            DiffOp::Replace { new_index, new_len, .. } => out.extend((new_index..new_index + new_len).map(|line| (line, Mark::Modified))),
        }
    }
    out
}

/// The file's text in the git index, `None` when it is not in a repository
/// or not tracked (an untracked file gets no marks, as in VS Code).
pub fn index_text(path: &Path) -> Option<String> {
    let dir = path.parent()?;
    let top = git::run(dir, &["rev-parse", "--show-toplevel"], None).ok().filter(|o| o.ok())?.stdout.trim().to_string();
    let top = Path::new(&top);
    let rel = path.strip_prefix(top).ok().or_else(|| {
        // git prints forward slashes and the case it stores; compare loosely.
        let key = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
        let (full, base) = (key(path), key(top));
        full.strip_prefix(&format!("{base}/")).map(|_| Path::new(""))
    })?;
    let rel = if rel.as_os_str().is_empty() {
        let full = path.to_string_lossy().replace('\\', "/");
        full[top.to_string_lossy().len() + 1..].to_string()
    } else {
        rel.to_string_lossy().replace('\\', "/")
    };
    let out = git::run(top, &["show", &format!(":{rel}")], None).ok().filter(|o| o.ok())?;
    Some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::{Mark, marks};

    #[test]
    fn added_changed_and_deleted_lines() {
        let base = "a\nb\nc\nd\n";
        assert_eq!(marks(base, base), vec![]);
        assert_eq!(marks(base, "a\nb\nNEW\nc\nd\n"), vec![(2, Mark::Added)]);
        assert_eq!(marks(base, "a\nB\nc\nd\n"), vec![(1, Mark::Modified)]);
        assert_eq!(marks(base, "a\nc\nd\n"), vec![(1, Mark::Deleted)]);
        assert_eq!(marks(base, "a\r\nb\r\nc\r\nd\r\n"), vec![]);
    }
}
