//! The layout tree: groups of tabs in splits, as plain data.
//!
//! A `Split` lays its children out along its axis and is a *container*; a
//! `Group` holds tabs (pane ids) and may be empty, as in den. Node and pane
//! ids are stable and saved with the layout, so anything keyed by them (the
//! default groups) survives a save and a reload.
//!
//! Splits keep their own axis, so a container can run the same way as the one
//! around it; nothing merges them. Normalising only drops empty splits and
//! collapses a split left with one child into that child.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub type NodeId = u64;
pub type PaneId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    /// Children side by side.
    Horizontal,
    /// Children stacked.
    Vertical,
}

impl Axis {
    pub fn other(self) -> Self {
        match self {
            Axis::Horizontal => Axis::Vertical,
            Axis::Vertical => Axis::Horizontal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    pub fn axis(self) -> Axis {
        match self {
            Side::Left | Side::Right => Axis::Horizontal,
            Side::Top | Side::Bottom => Axis::Vertical,
        }
    }

    /// Whether the new node goes after the target along the axis.
    pub fn after(self) -> bool {
        matches!(self, Side::Right | Side::Bottom)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Node {
    Split {
        id: NodeId,
        axis: Axis,
        children: Vec<Node>,
        /// Each child's share of the split, summing to 1.
        sizes: Vec<f32>,
    },
    Group {
        id: NodeId,
        tabs: Vec<PaneId>,
        active: usize,
    },
}

impl Node {
    pub fn id(&self) -> NodeId {
        match self {
            Node::Split { id, .. } | Node::Group { id, .. } => *id,
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self, Node::Group { .. })
    }

    fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        if let Node::Split { children, .. } = self {
            for child in children {
                child.walk(f);
            }
        }
    }

    fn find(&self, target: NodeId) -> Option<&Node> {
        if self.id() == target {
            return Some(self);
        }
        match self {
            Node::Split { children, .. } => children.iter().find_map(|child| child.find(target)),
            Node::Group { .. } => None,
        }
    }

    fn find_mut(&mut self, target: NodeId) -> Option<&mut Node> {
        if self.id() == target {
            return Some(self);
        }
        match self {
            Node::Split { children, .. } => children.iter_mut().find_map(|child| child.find_mut(target)),
            Node::Group { .. } => None,
        }
    }

    /// The groups under this node, in order.
    pub fn groups(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.walk(&mut |node| {
            if node.is_group() {
                out.push(node.id());
            }
        });
        out
    }

    fn max_id(&self) -> u64 {
        let mut max = 0;
        self.walk(&mut |node| {
            max = max.max(node.id());
            if let Node::Group { tabs, .. } = node {
                max = tabs.iter().copied().fold(max, u64::max);
            }
        });
        max
    }
}

fn even(n: usize) -> Vec<f32> {
    vec![1.0 / n.max(1) as f32; n]
}

fn renormalise(sizes: &mut [f32]) {
    let total: f32 = sizes.iter().sum();
    if total <= f32::EPSILON {
        let share = 1.0 / sizes.len().max(1) as f32;
        sizes.iter_mut().for_each(|s| *s = share);
    } else {
        sizes.iter_mut().for_each(|s| *s /= total);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    pub root: Node,
    next_id: u64,
}

impl Tree {
    /// One empty group.
    pub fn new() -> Self {
        Self {
            root: Node::Group {
                id: 1,
                tabs: Vec::new(),
                active: 0,
            },
            next_id: 2,
        }
    }

    pub fn from_root(root: Node) -> Self {
        let next_id = root.max_id() + 1;
        let mut tree = Self { root, next_id };
        tree.normalize();
        tree
    }

    /// A fresh id, for a node or a pane.
    pub fn mint(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Ids handed out elsewhere (panes restored with their saved ids) must
    /// not be minted again.
    pub fn reserve(&mut self, id: u64) {
        self.next_id = self.next_id.max(id + 1);
    }

    pub fn find(&self, id: NodeId) -> Option<&Node> {
        self.root.find(id)
    }

    pub fn groups(&self) -> Vec<NodeId> {
        self.root.groups()
    }

    /// The groups under `id` (itself, for a group).
    pub fn groups_under(&self, id: NodeId) -> Vec<NodeId> {
        self.find(id).map(Node::groups).unwrap_or_default()
    }

    pub fn tabs(&self, group: NodeId) -> &[PaneId] {
        match self.find(group) {
            Some(Node::Group { tabs, .. }) => tabs,
            _ => &[],
        }
    }

    pub fn active_tab(&self, group: NodeId) -> Option<PaneId> {
        match self.find(group) {
            Some(Node::Group { tabs, active, .. }) => tabs.get(*active).copied(),
            _ => None,
        }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.root.walk(&mut |node| {
            if let Node::Group { tabs, .. } = node {
                out.extend(tabs.iter().copied());
            }
        });
        out
    }

    pub fn group_of(&self, pane: PaneId) -> Option<NodeId> {
        let mut found = None;
        self.root.walk(&mut |node| {
            if let Node::Group { id, tabs, .. } = node
                && tabs.contains(&pane)
            {
                found = Some(*id);
            }
        });
        found
    }

    /// The split holding `id` and its index there.
    pub fn parent_of(&self, id: NodeId) -> Option<(NodeId, usize)> {
        let mut found = None;
        self.root.walk(&mut |node| {
            if let Node::Split { id: split, children, .. } = node
                && let Some(ix) = children.iter().position(|child| child.id() == id)
            {
                found = Some((*split, ix));
            }
        });
        found
    }

    /// The splits from the root down to the one holding `id`, outermost first.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = Vec::new();
        let mut at = id;
        while let Some((parent, _)) = self.parent_of(at) {
            chain.push(parent);
            at = parent;
        }
        chain.reverse();
        chain
    }

    /// Whether `inner` is `outer` or lies inside it.
    pub fn contains(&self, outer: NodeId, inner: NodeId) -> bool {
        self.find(outer).is_some_and(|node| node.find(inner).is_some())
    }

    /// The group next to `group` towards `side` (Alt+arrows): up to the nearest
    /// split along that axis with a sibling that way, then down into it on
    /// the edge facing `group`.
    pub fn neighbor(&self, group: NodeId, side: Side) -> Option<NodeId> {
        let mut at = group;
        loop {
            let (parent, ix) = self.parent_of(at)?;
            if let Some(Node::Split { axis, children, .. }) = self.find(parent)
                && *axis == side.axis()
            {
                let next = if side.after() { ix.checked_add(1).filter(|n| *n < children.len()) } else { ix.checked_sub(1) };
                if let Some(next) = next {
                    return Some(descend(&children[next], side));
                }
            }
            at = parent;
        }
    }

    // -- Edits ---------------------------------------------------------------

    /// Put `node` beside `target`: into the target's split when that runs
    /// along the side's axis, else sharing the target's slot in a new split.
    /// Either way the target's subtree stays whole.
    pub fn insert_beside(&mut self, target: NodeId, side: Side, node: Node) -> bool {
        let axis = side.axis();
        if let Some((parent, ix)) = self.parent_of(target)
            && let Some(Node::Split { axis: parent_axis, children, sizes, .. }) = self.root.find_mut(parent)
            && *parent_axis == axis
        {
            let half = sizes[ix] / 2.0;
            sizes[ix] = half;
            let at = if side.after() { ix + 1 } else { ix };
            children.insert(at, node);
            sizes.insert(at, half);
            return true;
        }
        let split_id = self.mint();
        let Some(slot) = self.root.find_mut(target) else { return false };
        let existing = std::mem::replace(
            slot,
            Node::Group {
                id: 0,
                tabs: Vec::new(),
                active: 0,
            },
        );
        let children = if side.after() { vec![existing, node] } else { vec![node, existing] };
        *slot = Node::Split {
            id: split_id,
            axis,
            children,
            sizes: vec![0.5, 0.5],
        };
        true
    }

    /// A new empty group beside `target`; its id.
    pub fn split(&mut self, target: NodeId, side: Side) -> Option<NodeId> {
        let id = self.mint();
        let group = Node::Group {
            id,
            tabs: Vec::new(),
            active: 0,
        };
        self.insert_beside(target, side, group).then_some(id)
    }

    /// Take `id` out of the tree. The tree is normalised afterwards; the
    /// returned map names, for each split that collapsed, what took its place.
    pub fn detach(&mut self, id: NodeId) -> Option<(Node, HashMap<NodeId, NodeId>)> {
        let (parent, ix) = self.parent_of(id)?;
        let Some(Node::Split { children, sizes, .. }) = self.root.find_mut(parent) else {
            return None;
        };
        let node = children.remove(ix);
        sizes.remove(ix);
        renormalise(sizes);
        let remap = self.normalize();
        Some((node, remap))
    }

    /// Move `src` beside `target`. Refused for a target inside `src` or the
    /// root itself (it has nowhere to go).
    pub fn move_node(&mut self, src: NodeId, target: NodeId, side: Side) -> Option<HashMap<NodeId, NodeId>> {
        if self.contains(src, target) || self.root.id() == src {
            return None;
        }
        let (node, remap) = self.detach(src)?;
        let target = resolve(&remap, target);
        if !self.insert_beside(target, side, node) {
            return None;
        }
        Some(remap)
    }

    /// Lay a container's children out along its other axis.
    pub fn flip(&mut self, split: NodeId) -> bool {
        match self.root.find_mut(split) {
            Some(Node::Split { axis, .. }) => {
                *axis = axis.other();
                true
            }
            _ => false,
        }
    }

    /// Move the boundary after child `ix` of `split` to `at` (a fraction of
    /// the split from its start), keeping each side at least `min`.
    pub fn resize(&mut self, split: NodeId, ix: usize, at: f32, min: f32) {
        let Some(Node::Split { sizes, .. }) = self.root.find_mut(split) else { return };
        if ix + 1 >= sizes.len() {
            return;
        }
        let before: f32 = sizes[..ix].iter().sum();
        let pair = sizes[ix] + sizes[ix + 1];
        let first = (at - before).clamp(min.min(pair / 2.0), pair - min.min(pair / 2.0));
        sizes[ix] = first;
        sizes[ix + 1] = pair - first;
    }

    pub fn add_tab(&mut self, group: NodeId, pane: PaneId, ix: Option<usize>, activate: bool) -> bool {
        let Some(Node::Group { tabs, active, .. }) = self.root.find_mut(group) else { return false };
        let at = ix.unwrap_or(tabs.len()).min(tabs.len());
        tabs.insert(at, pane);
        if activate || tabs.len() == 1 {
            *active = at;
        } else if at <= *active {
            *active += 1;
        }
        true
    }

    /// Take a tab out of its group (which stays, empty or not).
    pub fn remove_tab(&mut self, pane: PaneId) -> Option<NodeId> {
        let group = self.group_of(pane)?;
        let Some(Node::Group { tabs, active, .. }) = self.root.find_mut(group) else { return None };
        let ix = tabs.iter().position(|p| *p == pane)?;
        tabs.remove(ix);
        if ix < *active || (*active >= tabs.len() && *active > 0) {
            *active -= 1;
        }
        Some(group)
    }

    /// Move a tab into `group` at `ix` (the end when `None`), showing it.
    pub fn move_tab(&mut self, pane: PaneId, group: NodeId, ix: Option<usize>) -> bool {
        let Some(from) = self.group_of(pane) else { return false };
        let mut ix = ix;
        if from == group
            && let (Some(at), Some(old)) = (ix, self.tabs(group).iter().position(|p| *p == pane))
            && old < at
        {
            ix = Some(at - 1);
        }
        self.remove_tab(pane);
        self.add_tab(group, pane, ix, true)
    }

    pub fn activate(&mut self, pane: PaneId) {
        let Some(group) = self.group_of(pane) else { return };
        if let Some(Node::Group { tabs, active, .. }) = self.root.find_mut(group)
            && let Some(ix) = tabs.iter().position(|p| *p == pane)
        {
            *active = ix;
        }
    }

    /// Drop empty splits and collapse one-child splits into their child. The
    /// root may become a group; an empty tree becomes one empty group.
    pub fn normalize(&mut self) -> HashMap<NodeId, NodeId> {
        let mut remap = HashMap::new();
        let root = std::mem::replace(
            &mut self.root,
            Node::Group {
                id: 0,
                tabs: Vec::new(),
                active: 0,
            },
        );
        self.root = match tidy(root, &mut remap) {
            Some(root) => root,
            None => Node::Group {
                id: self.mint(),
                tabs: Vec::new(),
                active: 0,
            },
        };
        remap
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

/// The group of `node` nearest the edge a move towards `side` enters by.
fn descend(node: &Node, side: Side) -> NodeId {
    match node {
        Node::Group { id, .. } => *id,
        Node::Split { axis, children, .. } => {
            let child = if *axis == side.axis() && !side.after() { children.last() } else { children.first() };
            child.map_or(node.id(), |child| descend(child, side))
        }
    }
}

/// Follow a collapse map to where `id` ended up.
pub fn resolve(remap: &HashMap<NodeId, NodeId>, mut id: NodeId) -> NodeId {
    while let Some(next) = remap.get(&id) {
        id = *next;
    }
    id
}

fn tidy(node: Node, remap: &mut HashMap<NodeId, NodeId>) -> Option<Node> {
    match node {
        Node::Group { .. } => Some(node),
        Node::Split { id, axis, children, sizes } => {
            let mut kept_children = Vec::new();
            let mut kept_sizes = Vec::new();
            for (child, size) in children.into_iter().zip(sizes.into_iter().chain(std::iter::repeat(0.0))) {
                if let Some(child) = tidy(child, remap) {
                    kept_children.push(child);
                    kept_sizes.push(size);
                }
            }
            match kept_children.len() {
                0 => None,
                1 => {
                    let only = kept_children.pop().expect("one child");
                    remap.insert(id, only.id());
                    Some(only)
                }
                n => {
                    if kept_sizes.len() != n {
                        kept_sizes = even(n);
                    }
                    renormalise(&mut kept_sizes);
                    Some(Node::Split {
                        id,
                        axis,
                        children: kept_children,
                        sizes: kept_sizes,
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: NodeId, tabs: &[PaneId]) -> Node {
        Node::Group {
            id,
            tabs: tabs.to_vec(),
            active: 0,
        }
    }

    fn split(id: NodeId, axis: Axis, children: Vec<Node>) -> Node {
        let n = children.len();
        Node::Split {
            id,
            axis,
            children,
            sizes: even(n),
        }
    }

    #[test]
    fn splitting_a_lone_group_wraps_it() {
        let mut tree = Tree::new();
        let new = tree.split(1, Side::Right).unwrap();
        let Node::Split { axis, children, .. } = &tree.root else { panic!("root is a split") };
        assert_eq!(*axis, Axis::Horizontal);
        assert_eq!(children.iter().map(Node::id).collect::<Vec<_>>(), vec![1, new]);
    }

    #[test]
    fn splitting_along_the_parent_axis_joins_the_parent() {
        let mut tree = Tree::from_root(split(10, Axis::Horizontal, vec![group(1, &[]), group(2, &[])]));
        let new = tree.split(1, Side::Right).unwrap();
        let Node::Split { children, sizes, .. } = &tree.root else { panic!() };
        assert_eq!(children.iter().map(Node::id).collect::<Vec<_>>(), vec![1, new, 2]);
        assert!((sizes.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!((sizes[0] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn a_same_axis_container_stays_a_container() {
        // A container flipped onto its parent's axis keeps its own node.
        let mut tree = Tree::from_root(split(
            10,
            Axis::Horizontal,
            vec![group(1, &[]), split(11, Axis::Vertical, vec![group(2, &[]), group(3, &[])])],
        ));
        assert!(tree.flip(11));
        assert!(matches!(tree.find(11), Some(Node::Split { axis: Axis::Horizontal, .. })));
        assert_eq!(tree.groups_under(11), vec![2, 3]);
    }

    #[test]
    fn detaching_collapses_a_one_child_split() {
        let mut tree = Tree::from_root(split(
            10,
            Axis::Horizontal,
            vec![group(1, &[]), split(11, Axis::Vertical, vec![group(2, &[]), group(3, &[])])],
        ));
        let (node, remap) = tree.detach(3).unwrap();
        assert_eq!(node.id(), 3);
        assert_eq!(remap.get(&11), Some(&2));
        assert_eq!(tree.groups(), vec![1, 2]);
        assert!(tree.find(11).is_none());
    }

    #[test]
    fn moving_beside_a_container_that_collapses_follows_the_remap() {
        // Moving 3 beside its own container (which then collapses into 2).
        let mut tree = Tree::from_root(split(
            10,
            Axis::Horizontal,
            vec![group(1, &[]), split(11, Axis::Vertical, vec![group(2, &[]), group(3, &[])])],
        ));
        assert!(tree.move_node(3, 11, Side::Left).is_some());
        assert_eq!(tree.groups(), vec![1, 3, 2]);
    }

    #[test]
    fn a_node_cannot_move_into_itself() {
        let mut tree = Tree::from_root(split(
            10,
            Axis::Horizontal,
            vec![group(1, &[]), split(11, Axis::Vertical, vec![group(2, &[]), group(3, &[])])],
        ));
        assert!(tree.move_node(11, 2, Side::Left).is_none());
        assert!(tree.move_node(10, 1, Side::Left).is_none());
    }

    #[test]
    fn moving_beside_the_root_wraps_everything() {
        let mut tree = Tree::from_root(split(10, Axis::Vertical, vec![group(1, &[]), group(2, &[])]));
        assert!(tree.move_node(2, 10, Side::Left).is_some());
        // 10 collapsed into 1 after 2 left, so 2 now sits beside 1.
        let Node::Split { axis, children, .. } = &tree.root else { panic!() };
        assert_eq!(*axis, Axis::Horizontal);
        assert_eq!(children.iter().map(Node::id).collect::<Vec<_>>(), vec![2, 1]);
    }

    #[test]
    fn tabs_move_between_groups_and_groups_stay_when_emptied() {
        let mut tree = Tree::from_root(split(10, Axis::Horizontal, vec![group(1, &[100, 101]), group(2, &[])]));
        assert!(tree.move_tab(100, 2, None));
        assert_eq!(tree.tabs(1), &[101]);
        assert_eq!(tree.tabs(2), &[100]);
        tree.remove_tab(101);
        assert_eq!(tree.tabs(1), &[] as &[PaneId]);
        assert_eq!(tree.groups(), vec![1, 2]);
    }

    #[test]
    fn reordering_within_a_group() {
        let mut tree = Tree::from_root(group(1, &[100, 101, 102]));
        assert!(tree.move_tab(100, 1, Some(2)));
        assert_eq!(tree.tabs(1), &[101, 100, 102]);
        assert_eq!(tree.active_tab(1), Some(100));
    }

    #[test]
    fn removing_the_shown_tab_shows_a_neighbour() {
        let mut tree = Tree::from_root(group(1, &[100, 101, 102]));
        tree.activate(102);
        tree.remove_tab(102);
        assert_eq!(tree.active_tab(1), Some(101));
        tree.activate(100);
        tree.remove_tab(100);
        assert_eq!(tree.active_tab(1), Some(101));
    }

    #[test]
    fn resizing_keeps_the_pair_total() {
        let mut tree = Tree::from_root(split(10, Axis::Horizontal, vec![group(1, &[]), group(2, &[]), group(3, &[])]));
        tree.resize(10, 0, 0.5, 0.05);
        let Node::Split { sizes, .. } = &tree.root else { panic!() };
        assert!((sizes[0] - 0.5).abs() < 1e-5);
        assert!((sizes[0] + sizes[1] - 2.0 / 3.0).abs() < 1e-5);
        assert!((sizes[2] - 1.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    fn neighbours_across_containers() {
        // 10: [1, 11: [2, 3] stacked] side by side.
        let tree = Tree::from_root(split(
            10,
            Axis::Horizontal,
            vec![group(1, &[]), split(11, Axis::Vertical, vec![group(2, &[]), group(3, &[])])],
        ));
        assert_eq!(tree.neighbor(1, Side::Right), Some(2));
        assert_eq!(tree.neighbor(3, Side::Left), Some(1));
        assert_eq!(tree.neighbor(2, Side::Bottom), Some(3));
        assert_eq!(tree.neighbor(3, Side::Top), Some(2));
        assert_eq!(tree.neighbor(1, Side::Left), None);
        assert_eq!(tree.neighbor(2, Side::Top), None);
    }

    #[test]
    fn minted_ids_skip_saved_ones() {
        let mut tree = Tree::from_root(group(7, &[42]));
        assert!(tree.mint() > 42);
    }

    #[test]
    fn layouts_round_trip_through_json() {
        let tree = Tree::from_root(split(10, Axis::Horizontal, vec![group(1, &[100]), group(2, &[])]));
        let json = serde_json::to_string(&tree.root).unwrap();
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tree.root);
    }
}
