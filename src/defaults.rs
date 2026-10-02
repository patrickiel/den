//! Default groups: a group or container can be the default for a kind of
//! tab, and new tabs of that kind land there, whichever group is active.
//!
//! A group's own setting wins over its container's; setting a container
//! replaces the settings inside it. Keyed by node ids, which the layout keeps
//! stable, so a setting survives saves and every edit but the removal of its
//! node; a container that collapses into one group hands its setting to it.

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
    #[serde(default)]
    pub container: bool,
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

    /// The setting of exactly this group or container.
    pub fn own(&self, node: NodeId, container: bool) -> Option<Kind> {
        self.list
            .iter()
            .find(|d| d.node == node && d.container == container)
            .map(|d| d.kind)
    }

    /// What a group follows: its own setting, else its nearest container's.
    pub fn effective(&self, tree: &Tree, group: NodeId) -> Option<Kind> {
        self.own(group, false).or_else(|| {
            tree.ancestors(group)
                .into_iter()
                .rev()
                .find_map(|split| self.own(split, true))
        })
    }

    /// Whether a new tab of `kind` may open in `group`: the group is not the
    /// default of another kind, and when `kind` has defaults it is one.
    pub fn allows(&self, tree: &Tree, group: NodeId, kind: Kind) -> bool {
        let own = self.effective(tree, group);
        let has_defaults = self.list.iter().any(|d| d.kind == kind);
        own.is_none_or(|own| own == kind) && (!has_defaults || own == Some(kind))
    }

    /// Where a new tab of `kind` goes: the active group if it may take it,
    /// else the one of those used last, else the first. `None` when no
    /// group may take it.
    pub fn target(&self, tree: &Tree, kind: Kind, active: Option<NodeId>) -> Option<NodeId> {
        let allowed: Vec<NodeId> = tree
            .groups()
            .into_iter()
            .filter(|g| self.allows(tree, *g, kind))
            .collect();
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

    /// Set or clear the default of a group or container.
    pub fn set(&mut self, tree: &Tree, node: NodeId, container: bool, kind: Option<Kind>) {
        self.list.retain(|d| !(d.node == node && d.container == container));
        if let Some(kind) = kind {
            if container {
                self.list
                    .retain(|d| d.node == node || !tree.contains(node, d.node));
            }
            self.list.push(GroupDefault { kind, node, container });
        }
    }

    /// Bring the settings in line with the tree after an edit: follow
    /// collapsed splits, hand a container's setting to the group it became,
    /// and drop settings whose node is gone.
    pub fn prune(&mut self, tree: &Tree, remap: &HashMap<NodeId, NodeId>) {
        let mut kept: Vec<GroupDefault> = Vec::new();
        for mut d in std::mem::take(&mut self.list) {
            d.node = resolve(remap, d.node);
            match tree.find(d.node) {
                None => continue,
                Some(Node::Group { .. }) if d.container => d.container = false,
                Some(Node::Split { .. }) if !d.container => continue,
                _ => {}
            }
            // A group's own setting beats one handed down by a collapse.
            if kept.iter().any(|k| k.node == d.node && k.container == d.container) {
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
    fn a_container_default_routes_to_its_groups() {
        let tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(&tree, 11, true, Some(Kind::Files));
        assert_eq!(defaults.target(&tree, Kind::Files, Some(1)), Some(2));
        assert!(!defaults.allows(&tree, 1, Kind::Files));
        assert!(defaults.allows(&tree, 1, Kind::Terminals));
        assert!(!defaults.allows(&tree, 3, Kind::Terminals));
    }

    #[test]
    fn a_groups_own_setting_beats_its_container() {
        let tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(&tree, 11, true, Some(Kind::Files));
        defaults.set(&tree, 3, false, Some(Kind::Terminals));
        assert_eq!(defaults.effective(&tree, 3), Some(Kind::Terminals));
        assert_eq!(defaults.effective(&tree, 2), Some(Kind::Files));
        // Setting the container again replaces the settings inside it.
        defaults.set(&tree, 11, true, Some(Kind::Agents));
        assert_eq!(defaults.effective(&tree, 3), Some(Kind::Agents));
    }

    #[test]
    fn the_last_used_allowed_group_wins() {
        let tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(&tree, 11, true, Some(Kind::Files));
        defaults.activate(3);
        defaults.activate(1);
        assert_eq!(defaults.target(&tree, Kind::Files, Some(1)), Some(3));
    }

    #[test]
    fn a_collapsed_container_hands_its_setting_to_the_group() {
        let mut tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(&tree, 11, true, Some(Kind::Files));
        let (_, remap) = tree.detach(3).unwrap();
        defaults.prune(&tree, &remap);
        assert_eq!(defaults.own(2, false), Some(Kind::Files));
    }

    #[test]
    fn settings_follow_moved_nodes() {
        let mut tree = tree();
        let mut defaults = Defaults::default();
        defaults.set(&tree, 11, true, Some(Kind::Files));
        let remap = tree.move_node(11, 1, Side::Left).unwrap();
        defaults.prune(&tree, &remap);
        assert_eq!(defaults.own(11, true), Some(Kind::Files));
    }
}
