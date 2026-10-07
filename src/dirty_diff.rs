//! Quick diff in the editor gutter, as VS Code and den show it: which lines
//! of the buffer (unsaved edits included) differ from the file in the git
//! index. Green for added lines, blue for changed ones, a red mark where
//! lines were deleted.

use std::{borrow::Cow, path::Path, time::Duration};

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

/// The marked lines (from 0) of `current` against `base`, which has LF line
/// endings (as [`index_text`] gives it) where the buffer may have CRLF.
pub fn marks(base: &str, current: &str) -> Vec<(usize, Mark)> {
    if base.len() > MAX_BYTES || current.len() > MAX_BYTES {
        return Vec::new();
    }
    let current = if current.contains('\r') { Cow::Owned(current.replace("\r\n", "\n")) } else { Cow::Borrowed(current) };
    // This runs on every keystroke: a rougher diff beats a long one.
    let diff = TextDiff::configure().timeout(Duration::from_millis(200)).diff_lines(base, &*current);
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

/// The file's text in the git index, with LF line endings; `None` when it is
/// not in a repository or not tracked (an untracked file gets no marks, as in
/// VS Code).
pub fn index_text(path: &Path) -> Option<String> {
    let (dir, name) = (path.parent()?, path.file_name()?.to_str()?);
    // `:./name` names the index entry of the file in the current folder.
    let text = git::show_in(dir, "", &format!("./{name}")).ok().flatten()?;
    Some(text.replace("\r\n", "\n"))
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
