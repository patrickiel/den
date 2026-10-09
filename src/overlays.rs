//! A maximized group, drawn over a window's body (the sidebar and the
//! groups), under its title bar.
//!
//! Maximizing a group draws it over everything else in its window (over the
//! groups alone, leaving the sidebar, when Settings say so), a small space
//! in from the edges, as VS Code's maximized editor group. It
//! grows there from its place in the layout and shrinks back when restored.
//! Underneath, the layout stays as it is, with an empty slot where the group
//! came from; making another group of the window the active one (the keys,
//! a tab opening there) restores it. Maximized as tiles, the group shows
//! every one of its tabs at once, live, in a grid (`render_tiles`).

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    layout::{Node, NodeId, PaneId, Side},
    layout_view::Zone,
    workspace::Workspace,
};

/// How long a group takes to grow to its maximized bounds, and to shrink back.
const GROW: Duration = Duration::from_millis(180);
/// The space a maximized group leaves to the body's edges.
const SPACING: f32 = 8.;

/// A transition of `duration`, run forwards, and back again when closing.
#[derive(Clone, Copy, Debug)]
struct Transition {
    started: Instant,
    closing: bool,
    duration: Duration,
}

impl Transition {
    fn start(duration: Duration) -> Self {
        Self { started: Instant::now(), closing: false, duration }
    }

    /// How much of the duration has passed, 0 to 1.
    fn raw(&self) -> f32 {
        (self.started.elapsed().as_secs_f32() / self.duration.as_secs_f32()).min(1.)
    }

    /// Run back from where it is: reversed halfway, it starts halfway.
    fn reverse(&mut self) {
        let left = self.duration.mul_f32(1. - self.raw());
        self.closing = !self.closing;
        self.started = Instant::now() - left;
    }

    /// How far along it is, eased out: 0 as it starts (and as it ends
    /// closing), 1 fully open. Keeps the frames coming while it moves.
    fn progress(&self, window: &Window) -> f32 {
        let raw = self.raw();
        if raw < 1. {
            window.request_animation_frame();
        }
        let eased = 1. - (1. - raw).powi(3);
        if self.closing { 1. - eased } else { eased }
    }

    /// Closed: ran all the way back.
    fn done(&self) -> bool {
        self.closing && self.raw() >= 1.
    }
}

/// A group drawn over its window's body.
pub(crate) struct Maximized {
    pub group: NodeId,
    /// Every tab of the group at once, in a grid, rather than the shown one.
    pub tiles: bool,
    /// Where the group was in its window (window pixels): the overlay grows
    /// from there and shrinks back to it. Unknown, it is drawn in place.
    from: Option<Bounds<Pixels>>,
    transition: Transition,
}

/// Where parts of the windows were last painted, for the overlays to lie in
/// and grow from.
#[derive(Default)]
pub(crate) struct Painted {
    /// Each window's body, below its title bar (`None` for the main one).
    pub body: HashMap<Option<u64>, Bounds<Pixels>>,
    /// The maximized group's empty slot in the layout.
    pub slot: Option<Bounds<Pixels>>,
}

pub(crate) type PaintedRef = Rc<RefCell<Painted>>;

/// Records where its parent is painted, through `record`.
pub(crate) fn mark(painted: &PaintedRef, record: impl Fn(&mut Painted, Bounds<Pixels>) + 'static) -> impl IntoElement {
    let painted = painted.clone();
    canvas(move |bounds, _, _| record(&mut painted.borrow_mut(), bounds), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// `a` moved and sized towards `b`, `t` of the way.
fn between(a: Bounds<Pixels>, b: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let mix = |a: Pixels, b: Pixels| a + (b - a) * t;
    Bounds::new(
        point(mix(a.origin.x, b.origin.x), mix(a.origin.y, b.origin.y)),
        size(mix(a.size.width, b.size.width), mix(a.size.height, b.size.height)),
    )
}

impl Workspace {
    // -- Maximizing a group ----------------------------------------------------

    /// The group drawn maximized, while one is (shrinking back as well).
    pub(crate) fn maximized_group(&self) -> Option<NodeId> {
        self.maximized.as_ref().map(|m| m.group)
    }

    /// Whether `group` is maximized that way (its shown tab, or as tiles)
    /// and not on its way back.
    pub(crate) fn is_maximized(&self, group: NodeId, tiles: bool) -> bool {
        self.maximized.as_ref().is_some_and(|m| m.group == group && m.tiles == tiles && !m.transition.closing)
    }

    /// The group shown as tiles, while one is (and not on its way back): the
    /// keys that go between groups go between its tiles instead.
    pub(crate) fn tiled_group(&self) -> Option<NodeId> {
        self.maximized.as_ref().filter(|m| m.tiles && !m.transition.closing).map(|m| m.group)
    }

    /// The tile beside the active one towards `side` in the tiled group's
    /// grid, as `render_tiles` last laid it out; `None` at the grid's edge.
    pub(crate) fn tile_beside(&self, side: Side) -> Option<PaneId> {
        let group = self.tiled_group()?;
        let tabs = self.tree.tabs(group);
        let active = self.tree.active_tab(group).and_then(|pane| tabs.iter().position(|p| *p == pane))?;
        let columns = self.tile_columns.get().max(1);
        let (row, col) = (active / columns, active % columns);
        let (row, col) = match side {
            Side::Left => (row, col.checked_sub(1)?),
            Side::Right => (row, col + 1),
            Side::Top => (row.checked_sub(1)?, col),
            Side::Bottom => (row + 1, col),
        };
        (col < columns).then(|| tabs.get(row * columns + col).copied()).flatten()
    }

    /// Maximize `group`, showing its tab or every tab as `tiles`; restore it
    /// when it is maximized that way already, and switch the way otherwise.
    pub(crate) fn toggle_maximize(&mut self, group: NodeId, tiles: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_maximized(group, tiles) {
            self.restore_group(cx);
            return;
        }
        if self.is_maximized(group, !tiles) {
            if let Some(m) = &mut self.maximized {
                m.tiles = tiles;
            }
            cx.notify();
            return;
        }
        if !self.tree.find(group).is_some_and(Node::is_group) {
            return;
        }
        // Where it is drawn now: its strip and its content, as last painted.
        let float = self.tree.float_of(group);
        let from = self.zones.borrow().get(&float).and_then(|zones| {
            zones
                .iter()
                .filter(|(zone, _)| matches!(zone, Zone::Strip(g) | Zone::Group(g) if *g == group))
                .map(|(_, bounds)| *bounds)
                .reduce(|a, b| a.union(&b))
        });
        // The maximized group is the active one. Activating it restores
        // another group maximized in its window, so this comes first.
        self.maximized = None;
        self.set_active_group(group, cx);
        self.maximized = Some(Maximized { group, tiles, from, transition: Transition::start(GROW) });
        if let Some(pane) = self.tree.active_tab(group) {
            self.focus_pane(pane, window, cx);
        }
        cx.notify();
    }

    /// Shrink the maximized group back into its place.
    pub(crate) fn restore_group(&mut self, cx: &mut Context<Self>) {
        let Some(m) = &mut self.maximized else { return };
        if m.transition.closing {
            return;
        }
        // Back to where its slot is now: the layout may have changed under it.
        if let Some(slot) = self.painted.borrow().slot {
            m.from = Some(slot);
        }
        m.transition.reverse();
        cx.notify();
    }

    /// A transition that ran its course: what it closed goes. Called as a
    /// window is drawn, before anything looks at the overlay.
    pub(crate) fn settle_overlays(&mut self) {
        if self.maximized.as_ref().is_some_and(|m| m.transition.done()) {
            self.maximized = None;
        }
    }

    /// The maximized group's place in the layout while it is drawn over it.
    pub(crate) fn render_slot(&self, group: NodeId, cx: &App) -> AnyElement {
        div()
            .id(("group-slot", group))
            .relative()
            .size_full()
            .bg(cx.theme().tab_bar)
            .child(mark(&self.painted, |painted, bounds| painted.slot = Some(bounds)))
            .into_any_element()
    }

    /// What lies over a window's body: the maximized group, when it is this
    /// window's (`None` for the main one).
    pub(crate) fn render_overlays(&self, float: Option<u64>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .child(mark(&self.painted, move |painted, bounds| {
                painted.body.insert(float, bounds);
            }))
            .children(self.render_maximized(float, window, cx))
            .into_any_element()
    }

    /// The maximized group, grown over the body a little in from its edges,
    /// with a dimmed rim showing round it.
    fn render_maximized(&self, float: Option<u64>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.maximized.as_ref()?;
        let group = m.group;
        if self.tree.float_of(group) != float {
            return None;
        }
        let Some(Node::Group { tabs, active, .. }) = self.tree.find(group) else { return None };
        let t = m.transition.progress(window);
        let body = self.painted.borrow().body.get(&float).copied();
        // What it covers, in the body's own pixels: the whole body, or (as
        // Settings may have it) the groups' area alone, leaving the sidebar.
        let region = body.map(|body| {
            let area = (!crate::settings::Settings::get(cx).maximize_covers_sidebar)
                .then(|| self.zones.borrow().get(&float).and_then(|zones| zones.iter().find_map(|(zone, bounds)| matches!(zone, Zone::Area(_)).then_some(*bounds))))
                .flatten();
            match area {
                Some(area) => Bounds::new(area.origin - body.origin, area.size),
                None => Bounds::new(Point::default(), body.size),
            }
        });
        // Where the group lies when full, and where it grows from (drawn
        // full before the body has been painted).
        let full = region.map(|region| {
            let margin = px(SPACING);
            Bounds::new(region.origin + point(margin, margin), size(region.size.width - margin * 2., region.size.height - margin * 2.))
        });
        let at = body.zip(full).map(|(body, full)| match m.from {
            Some(from) => between(Bounds::new(from.origin - body.origin, from.size), full, t),
            None => full,
        });
        // The tiles' grid is laid out for the full size, so it holds still
        // while the group grows to it and shrinks back.
        self.tile_area.set(full.map(|full| full.size));
        let (background, border) = (cx.theme().background, cx.theme().border);
        let frame = div()
            .id(("maximized", group))
            .absolute()
            .map(|this| match at {
                Some(at) => this.left(at.origin.x).top(at.origin.y).w(at.size.width).h(at.size.height),
                None => this.inset(px(SPACING)),
            })
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(background)
            .shadow_lg()
            .overflow_hidden()
            .child(self.render_group(group, tabs, *active, m.tiles, window, cx));
        // The dimmed rim covers what the group does, no more: the sidebar
        // stays usable when the group leaves it out.
        Some(
            div()
                .absolute()
                .map(|this| match region {
                    Some(region) => this.left(region.origin.x).top(region.origin.y).w(region.size.width).h(region.size.height),
                    None => this.inset_0(),
                })
                .occlude()
                .bg(background.alpha(0.6 * t))
                .child(frame)
                .into_any_element(),
        )
    }
}
