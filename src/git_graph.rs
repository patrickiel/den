//! The commit graph's lanes, row by row, as vscode-git-graph lays them out,
//! and how a row's graph is drawn.
//!
//! The commits come newest first. Each lane waits for a commit (the parent
//! of a commit above). A commit takes the first lane waiting for it (others
//! waiting for it too join it there), or a free one; its first parent then
//! waits in its lane, and every other parent in a lane of its own unless one
//! already waits for it. So a branch keeps its lane down to the commit it
//! forked from. Each row gets the lines through it: from the lanes
//! above into its dot, from its dot to its parents' lanes below, and
//! straight through for the lanes passing by.

use den_extension::view::{Graph, GraphDot, GraphLine};
use gpui_kit::*;

use crate::preset_icon::parse_color;

/// vscode-git-graph's colours, one per lane.
pub const COLORS: [&str; 12] = [
    "#0085d9", "#d9008f", "#00d90a", "#d98500", "#a300d9", "#ff0000", "#00d9cc", "#e138e8", "#85d900", "#dc5b23", "#6f24d6", "#ffcc00",
];

/// The colour of uncommitted changes.
pub const UNCOMMITTED: &str = "#808080";

/// A graph's lane, in pixels.
pub const LANE: f32 = 14.;
/// Space before a graph's first lane.
pub const GRAPH_PAD: f32 = 4.;

pub fn color(lane: usize) -> &'static str {
    COLORS[lane % COLORS.len()]
}

/// A lane's colour, for the badges of the commits in it.
pub fn lane_color(lane: usize) -> Hsla {
    parse_color(color(lane)).unwrap_or_else(gpui_kit::blue)
}

pub struct Node<'a> {
    pub hash: &'a str,
    pub parents: &'a [String],
    /// Drawn as a ring (HEAD, uncommitted changes).
    pub hollow: bool,
    /// Its lines to its parents are dashed, in grey (uncommitted changes).
    pub uncommitted: bool,
}

#[derive(Clone)]
struct Lane {
    /// The commit it waits for.
    hash: String,
    /// It comes from uncommitted changes.
    uncommitted: bool,
}

#[derive(Default)]
pub struct Layout {
    pub graphs: Vec<Graph>,
    /// Each row's lane.
    pub lanes: Vec<usize>,
}

pub fn layout(nodes: &[Node]) -> Layout {
    let mut lanes: Vec<Option<Lane>> = Vec::new();
    let mut graphs = Vec::with_capacity(nodes.len());
    let mut own_lanes = Vec::with_capacity(nodes.len());
    for node in nodes {
        let before = lanes.clone();
        let waiting: Vec<usize> = before.iter().enumerate().filter(|(_, l)| l.as_ref().is_some_and(|l| l.hash == node.hash)).map(|(ix, _)| ix).collect();
        let lane = waiting.first().copied().unwrap_or_else(|| free(&before));
        let mut after = before.clone();
        for &ix in &waiting {
            after[ix] = None;
        }
        if after.len() <= lane {
            after.resize(lane + 1, None);
        }

        let mut graph = Graph::default();
        let line = |x0: usize, y0: f32, x1: usize, y1: f32, color: &str, dashed: bool| GraphLine {
            x0: x0 as f32,
            y0,
            x1: x1 as f32,
            y1,
            color: color.to_string(),
            dashed,
        };
        let own_color = if node.uncommitted { UNCOMMITTED } else { color(lane) };

        // Into the dot from above, and past it.
        for (ix, waiting_lane) in before.iter().enumerate() {
            let Some(waiting_lane) = waiting_lane else { continue };
            let shade = if waiting_lane.uncommitted { UNCOMMITTED } else { color(ix) };
            if waiting_lane.hash == node.hash {
                graph.lines.push(line(ix, 0., lane, 0.5, shade, waiting_lane.uncommitted));
            } else {
                graph.lines.push(line(ix, 0., ix, 1., shade, waiting_lane.uncommitted));
            }
        }

        // Out of the dot to the parents.
        for (k, parent) in node.parents.iter().enumerate() {
            // The first parent stays in the commit's lane, so a branch runs
            // down to where it forked; another parent joins a lane already
            // waiting for it.
            let waiting = if k == 0 { None } else { after.iter().position(|l| l.as_ref().is_some_and(|l| &l.hash == parent)) };
            let target = match waiting {
                Some(ix) => ix,
                None => {
                    let ix = if k == 0 { lane } else { free(&after) };
                    if after.len() <= ix {
                        after.resize(ix + 1, None);
                    }
                    after[ix] = Some(Lane { hash: parent.clone(), uncommitted: node.uncommitted });
                    ix
                }
            };
            let shade = if node.uncommitted { UNCOMMITTED } else if k == 0 { own_color } else { color(target) };
            graph.lines.push(line(lane, 0.5, target, 1., shade, node.uncommitted));
        }

        graph.dots.push(GraphDot { x: lane as f32, color: own_color.to_string(), hollow: node.hollow });
        while after.last().is_some_and(Option::is_none) {
            after.pop();
        }
        lanes = after;
        graphs.push(graph);
        own_lanes.push(lane);
    }
    Layout { graphs, lanes: own_lanes }
}

/// The first lane that waits for nothing, else a new one.
fn free(lanes: &[Option<Lane>]) -> usize {
    lanes.iter().position(Option::is_none).unwrap_or(lanes.len())
}

/// The lanes running on below a row, as straight lines: drawn through the
/// file rows of an expanded commit.
pub fn continuation(graph: &Graph) -> Graph {
    Graph {
        lines: graph
            .lines
            .iter()
            .filter(|l| l.y1 == 1.)
            .map(|l| GraphLine { x0: l.x1, y0: 0., x1: l.x1, y1: 1., color: l.color.clone(), dashed: l.dashed })
            .collect(),
        dots: Vec::new(),
    }
}

/// The width of a graph column holding every row: at least two lanes.
pub fn column_width(graphs: &[Graph]) -> Pixels {
    let lanes = graphs.iter().map(Graph::lanes).fold(0., f32::max).max(2.);
    px(GRAPH_PAD * 2. + lanes * LANE)
}

/// A row's graph: its lines (a lane change as a curve), then its dots.
pub fn graph_canvas(graph: Graph, background: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let x = |lane: f32| bounds.left() + px(GRAPH_PAD + LANE * lane + LANE / 2.);
            let y = |t: f32| bounds.top() + bounds.size.height * t;
            for line in &graph.lines {
                let Some(color) = parse_color(&line.color) else { continue };
                let mut path = PathBuilder::stroke(px(2.));
                if line.dashed {
                    path = path.dash_array(&[px(3.), px(3.)]);
                }
                let (from, to) = (point(x(line.x0), y(line.y0)), point(x(line.x1), y(line.y1)));
                path.move_to(from);
                if line.x0 == line.x1 {
                    path.line_to(to);
                } else {
                    let middle = (from.y + to.y) / 2.;
                    path.cubic_bezier_to(to, point(from.x, middle), point(to.x, middle));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }
            for dot in &graph.dots {
                let Some(color) = parse_color(&dot.color) else { continue };
                let radius = px(4.);
                let circle = Bounds::centered_at(point(x(dot.x), y(0.5)), size(radius * 2., radius * 2.));
                let fill = if dot.hollow { background } else { color };
                window.paint_quad(quad(circle, radius, fill, px(if dot.hollow { 2. } else { 0. }), color, BorderStyle::default()));
            }
        },
    )
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::{GRAPH_PAD, LANE, Layout, Node, UNCOMMITTED, column_width, continuation, layout};
    use gpui_kit::px;

    fn lay(commits: &[(&str, &[&str])]) -> Layout {
        let parents: Vec<Vec<String>> = commits.iter().map(|(_, p)| p.iter().map(|s| s.to_string()).collect()).collect();
        let nodes: Vec<Node> = commits
            .iter()
            .zip(&parents)
            .map(|((hash, _), parents)| Node { hash, parents, hollow: false, uncommitted: *hash == "*" })
            .collect();
        layout(&nodes)
    }

    #[test]
    fn a_straight_history_stays_in_one_lane() {
        let layout = lay(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(layout.lanes, [0, 0, 0]);
        // The root has a line in from above and none out.
        assert_eq!(layout.graphs[2].lines.len(), 1);
        assert_eq!(layout.graphs[2].lines[0].y1, 0.5);
    }

    #[test]
    fn a_merged_branch_gets_a_lane_of_its_own_and_comes_back() {
        // m merges b into the line of a.
        let layout = lay(&[("m", &["a", "b"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(layout.lanes, [0, 1, 0]);
        let m = &layout.graphs[0];
        assert!(m.lines.iter().any(|l| l.x0 == 0. && l.x1 == 1. && l.y1 == 1.));
        let b = &layout.graphs[1];
        // b keeps its lane, a's passes by; they meet at a.
        assert!(b.lines.iter().any(|l| l.x0 == 1. && l.y0 == 0.5 && l.x1 == 1. && l.y1 == 1.));
        assert!(b.lines.iter().any(|l| l.x0 == 0. && l.y0 == 0. && l.x1 == 0. && l.y1 == 1.));
        assert!(layout.graphs[2].lines.iter().any(|l| l.x0 == 1. && l.y0 == 0. && l.x1 == 0. && l.y1 == 0.5));
    }

    #[test]
    fn two_branches_off_one_commit_meet_at_it() {
        // y's lane comes in beside x's, and both end at a.
        let layout = lay(&[("x", &["a"]), ("y", &["a"]), ("a", &[])]);
        assert_eq!(layout.lanes, [0, 1, 0]);
        assert_eq!(layout.graphs[2].lines.iter().filter(|l| l.y1 == 0.5).count(), 2);
        let layout = lay(&[("x", &["a"]), ("y", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(layout.lanes, [0, 1, 1, 0]);
        // At a, the second lane comes in from above.
        assert!(layout.graphs[3].lines.iter().any(|l| l.x0 == 1. && l.y0 == 0. && l.x1 == 0. && l.y1 == 0.5));
        assert!(layout.graphs[3].lanes() <= 2.);
    }

    #[test]
    fn freed_lanes_are_reused() {
        let layout = lay(&[("m", &["a", "b"]), ("b", &["a"]), ("a", &["z"]), ("n", &["z"]), ("z", &[])]);
        assert_eq!(layout.lanes, [0, 1, 0, 1, 0]);
    }

    #[test]
    fn uncommitted_changes_are_dashed_grey() {
        let layout = lay(&[("*", &["h"]), ("h", &[])]);
        let line = &layout.graphs[0].lines[0];
        assert!(line.dashed);
        assert_eq!(line.color, UNCOMMITTED);
        assert!(layout.graphs[1].lines[0].dashed);
    }

    #[test]
    fn continuation_keeps_only_lines_that_continue() {
        // b, with a's lane passing by: both lanes go on; the line into b's dot does not.
        let layout = lay(&[("m", &["a", "b"]), ("b", &["a"]), ("a", &[])]);
        let below = continuation(&layout.graphs[1]);
        assert!(below.dots.is_empty());
        assert_eq!(below.lines.len(), 2);
        assert!(below.lines.iter().all(|l| l.x0 == l.x1 && l.y0 == 0. && l.y1 == 1.));
        // Nothing continues below the root.
        assert!(continuation(&layout.graphs[2]).lines.is_empty());
    }

    #[test]
    fn the_column_fits_the_widest_row_and_at_least_two_lanes() {
        let layout = lay(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(column_width(&layout.graphs), px(GRAPH_PAD * 2. + 2. * LANE));
        let layout = lay(&[("m", &["a", "b", "c"]), ("c", &["a"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(column_width(&layout.graphs), px(GRAPH_PAD * 2. + 3. * LANE));
    }
}
