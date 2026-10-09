//! Default groups: a group can be the default for a kind of tab, and new tabs
//! of that kind land there, whichever group is active.
//!
//! Keyed by node ids, which the layout keeps stable, so a setting survives
//! saves and every edit but the removal of its group.

use std::collections::HashMap;

use gpui_kit::assets::IconName;
use serde::{Deserialize, Serialize};

use crate::layout::{Node, NodeId, Tree, resolve};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Files,
    Terminals,
    Agents,
    Browsers,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Files, Kind::Terminals, Kind::Agents, Kind::Browsers];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Files => "Files",
            Kind::Terminals => "Terminals",
            Kind::Agents => "Agents",
            Kind::Browsers => "Browsers",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Kind::Files => IconName::FileText,
            Kind::Terminals => IconName::SquareTerminal,
            Kind::Agents => IconName::Bot,
            Kind::Browsers => IconName::Globe,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupDefault {
    pub kind: Kind,
    pub node: NodeId,
}

#[derive(Clone, Debug, Default)]
pub struct Defaults {
    list: Vec<GroupDefault>,
    /// Groups that have been active, last active first.
    recent: Vec<NodeId>,
}

impl Defaults {
    pub fn from_saved(list: Vec<GroupDefault>, tree: &Tree) -> Self {
        let mut this = Self { list, recent: Vec::new() };
        this.prune(tree, &HashMap::new());
        this
    }

    pub fn saved(&self) -> Vec<GroupDefault> {
        self.list.clone()
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// The kind `group` is the default for, if any.
    pub fn get(&self, group: NodeId) -> Option<Kind> {
        self.list.iter().find(|d| d.node == group).map(|d| d.kind)
    }

    /// Whether a new tab of `kind` may open in `group`: the group is not the
    /// default of another kind, and when `kind` has defaults it is one.
    pub fn allows(&self, group: NodeId, kind: Kind) -> bool {
        let own = self.get(group);
        let has_defaults = self.list.iter().any(|d| d.kind == kind);
        own.is_none_or(|own| own == kind) && (!has_defaults || own == Some(kind))
    }

    /// Where a new tab of `kind` goes: the active group if it may take it,
    /// else the one of those used last, else the first. `None` when no
    /// group may take it.
    pub fn target(&self, tree: &Tree, kind: Kind, active: Option<NodeId>) -> Option<NodeId> {
        let allowed: Vec<NodeId> = tree.groups().into_iter().filter(|g| self.allows(*g, kind)).collect();
        active
            .into_iter()
            .chain(self.recent.iter().copied())
            .find(|g| allowed.contains(g))
            .or_else(|| allowed.first().copied())
    }

    pub fn activate(&mut self, group: NodeId) {
        if self.recent.first() != Some(&group) {
            self.recent.retain(|g| *g != group);
            self.recent.insert(0, group);
        }
    }

    /// Set or clear the default of a group.
    pub fn set(&mut self, group: NodeId, kind: Option<Kind>) {
        self.list.retain(|d| d.node != group);
        if let Some(kind) = kind {
            self.list.push(GroupDefault { kind, node: group });
        }
    }

    /// Bring the settings in line with the tree after an edit: follow
    /// collapsed splits and drop settings whose group is gone.
    pub fn prune(&mut self, tree: &Tree, remap: &HashMap<NodeId, NodeId>) {
        let mut kept: Vec<GroupDefault> = Vec::new();
        for mut d in std::mem::take(&mut self.list) {
            d.node = resolve(remap, d.node);
            if !tree.find(d.node).is_some_and(Node::is_group) || kept.iter().any(|k| k.node == d.node) {
                continue;
            }
            kept.push(d);
        }
        self.list = kept;
        let groups = tree.groups();
        self.recent = std::mem::take(&mut self.recent)
            .into_iter()
            .map(|g| resolve(remap, g))
            .filter(|g| groups.contains(g))
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Axis, Side};

    fn tree() -> Tree {
        // 10: [1, 11: [2, 3]] side by side, 11 stacked.
        Tree::from_root(Node::Split {
            id: 10,
            axis: Axis::Horizontal,
            sizes: vec![0.5, 0.5],
            children: vec![
                Node::Group { id: 1, tabs: vec![], active: 0 },
                Node::Split {
                    id: 11,
                    axis: Axis::Vertical,
                    sizes: vec![0.5, 0.5],
                    children: vec![
                        Node::Group { id: 2, tabs: vec![], active: 0 },
                        Node::Group { id: 3, tabs: vec![], active: 0 },
                    ],
                },
            ],
        })
    }

    #[test]
    fn a_default_routes_its_kind_to_the_group() {
        let tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(2, Some(Kind::Files));
        assert_eq!(defaults.target(&tree, Kind::Files, Some(1)), Some(2));
        assert!(!defaults.allows(1, Kind::Files));
        assert!(defaults.allows(1, Kind::Terminals));
        assert!(!defaults.allows(2, Kind::Terminals));
    }

    #[test]
    fn setting_a_group_again_replaces_its_kind() {
        let mut defaults = Defaults::default();
        defaults.set(3, Some(Kind::Files));
        defaults.set(3, Some(Kind::Agents));
        assert_eq!(defaults.get(3), Some(Kind::Agents));
        defaults.set(3, None);
        assert_eq!(defaults.get(3), None);
    }

    #[test]
    fn the_last_used_allowed_group_wins() {
        let tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(2, Some(Kind::Files));
        defaults.set(3, Some(Kind::Files));
        defaults.activate(3);
        defaults.activate(1);
        assert_eq!(defaults.target(&tree, Kind::Files, Some(1)), Some(3));
    }

    #[test]
    fn a_closed_groups_setting_goes() {
        let mut tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(3, Some(Kind::Files));
        let (_, remap) = tree.detach(3).unwrap();
        defaults.prune(&tree, &remap);
        assert_eq!(defaults.get(3), None);
    }

    #[test]
    fn settings_follow_moved_groups() {
        let mut tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(3, Some(Kind::Files));
        let remap = tree.move_node(3, 1, Side::Left).unwrap();
        defaults.prune(&tree, &remap);
        assert_eq!(defaults.get(3), Some(Kind::Files));
    }

    #[test]
    fn a_saved_container_setting_is_dropped() {
        // Older layouts could make a container the default; those settings
        // sit on a split now and go.
        let tree = tree();
        let defaults = Defaults::from_saved(vec![GroupDefault { kind: Kind::Files, node: 11 }, GroupDefault { kind: Kind::Agents, node: 1 }], &tree);
        assert_eq!(defaults.saved(), vec![GroupDefault { kind: Kind::Agents, node: 1 }]);
    }
}
