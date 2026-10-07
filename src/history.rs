//! Back and forward through the tabs looked at, as with a mouse's side
//! buttons in a browser or VS Code.
//!
//! Each change of the shown tab (in the active group) is a visit. A tab that
//! closes stays in the history as its file, which going back to opens again;
//! a closed tab of another kind (a terminal) drops out.

use std::path::PathBuf;

use crate::layout::PaneId;

/// The most visits kept each way.
const LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Pane(PaneId),
    /// A file whose tab was closed.
    File(PathBuf),
}

#[derive(Default)]
pub struct History {
    back: Vec<Entry>,
    forward: Vec<Entry>,
    current: Option<Entry>,
}

impl History {
    /// `pane` is now the one shown. A new visit drops what was ahead.
    pub fn visit(&mut self, pane: PaneId) {
        let entry = Entry::Pane(pane);
        if self.current.as_ref() == Some(&entry) {
            return;
        }
        if let Some(previous) = self.current.replace(entry) {
            self.back.push(previous);
            if self.back.len() > LIMIT {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// `pane` is now the one shown, arrived at by going back or forward:
    /// where it was in the history is where it stays.
    pub fn arrive(&mut self, pane: PaneId) {
        self.current = Some(Entry::Pane(pane));
    }

    /// Step back (or forward) to the nearest entry `usable` takes, leaving
    /// the current one on the other side. The ones passed over go.
    pub fn step(&mut self, back: bool, usable: impl Fn(&Entry) -> bool) -> Option<Entry> {
        let (from, to) = if back { (&mut self.back, &mut self.forward) } else { (&mut self.forward, &mut self.back) };
        while let Some(entry) = from.pop() {
            if Some(&entry) == self.current.as_ref() || !usable(&entry) {
                continue;
            }
            if let Some(current) = self.current.replace(entry.clone()) {
                to.push(current);
            }
            return Some(entry);
        }
        None
    }

    /// `pane` closed: its entries become `file`, or go without one.
    pub fn closed(&mut self, pane: PaneId, file: Option<PathBuf>) {
        let gone = Entry::Pane(pane);
        let replace = |entries: &mut Vec<Entry>| {
            entries.retain_mut(|entry| {
                if *entry != gone {
                    return true;
                }
                match &file {
                    Some(path) => {
                        *entry = Entry::File(path.clone());
                        true
                    }
                    None => false,
                }
            });
            entries.dedup();
        };
        replace(&mut self.back);
        replace(&mut self.forward);
        if self.current.as_ref() == Some(&gone) {
            self.current = file.map(Entry::File);
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any(_: &Entry) -> bool {
        true
    }

    #[test]
    fn back_and_forward_through_visits() {
        let mut history = History::default();
        history.visit(1);
        history.visit(2);
        history.visit(3);
        assert_eq!(history.step(true, any), Some(Entry::Pane(2)));
        assert_eq!(history.step(true, any), Some(Entry::Pane(1)));
        assert_eq!(history.step(true, any), None);
        assert_eq!(history.step(false, any), Some(Entry::Pane(2)));
        assert_eq!(history.step(false, any), Some(Entry::Pane(3)));
        assert_eq!(history.step(false, any), None);
    }

    #[test]
    fn a_new_visit_drops_what_was_ahead() {
        let mut history = History::default();
        history.visit(1);
        history.visit(2);
        history.step(true, any);
        history.visit(3);
        assert_eq!(history.step(false, any), None);
        assert_eq!(history.step(true, any), Some(Entry::Pane(1)));
    }

    #[test]
    fn showing_the_same_tab_again_is_no_visit() {
        let mut history = History::default();
        history.visit(1);
        history.visit(2);
        history.visit(2);
        assert_eq!(history.step(true, any), Some(Entry::Pane(1)));
        assert_eq!(history.step(true, any), None);
    }

    #[test]
    fn closed_files_stay_and_other_tabs_go() {
        let mut history = History::default();
        history.visit(1);
        history.visit(2);
        history.visit(1);
        history.visit(3);
        history.closed(2, None);
        history.closed(1, Some(PathBuf::from("a.rs")));
        // 1, 2, 1 became a.rs, a.rs: one entry.
        assert_eq!(history.step(true, any), Some(Entry::File(PathBuf::from("a.rs"))));
        assert_eq!(history.step(true, any), None);
    }

    #[test]
    fn unusable_entries_are_passed_over() {
        let mut history = History::default();
        history.visit(1);
        history.visit(2);
        history.visit(3);
        assert_eq!(history.step(true, |entry| *entry != Entry::Pane(2)), Some(Entry::Pane(1)));
        assert_eq!(history.step(false, any), Some(Entry::Pane(3)));
    }

    #[test]
    fn arriving_by_a_step_keeps_the_history() {
        let mut history = History::default();
        history.visit(1);
        history.visit(2);
        history.step(true, any);
        // Going back to a closed file opened it as a new tab.
        history.arrive(4);
        assert_eq!(history.step(false, any), Some(Entry::Pane(2)));
        assert_eq!(history.step(true, any), Some(Entry::Pane(4)));
    }
}
