//! Drawing the layout tree, and dragging things around in it.
//!
//! Splits lay their children out with a handle between each pair to resize
//! them; every split but the outermost is a container, drawn as a tray with a
//! header holding its actions (default menu, split, flip), and dragged by it.
//! A group is a tab strip over the shown tab's content; the strip ends in the
//! group's actions (default, terminal presets, split, menu), and grabbing it
//! anywhere but on a tab drags the whole group.
//!
//! Drops: a tab onto a group's middle joins it, onto a side starts a group
//! beside it; a group or container onto a group's side or a container's
//! header goes beside that; anything into the band along the edge of the
//! whole area goes beside everything.
//!
//! Each window (the main one, and each floating one) draws its own part of the
//! tree this way. A tab, group or container let go outside the window moves
//! into a new floating window there, or into the session's window under the
//! pointer, as VS Code's editors do.
//!
//! The window a drag starts in keeps the mouse until it is let go, so the
//! others never see it pass. Each window records where its drop zones were
//! drawn (`Zones`); a drag over another window of the session works out its
//! drop from those, and that window shows the hint and the dragged label.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Side as MenuSide, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    scroll::{Scrollbar, ScrollbarMode, ScrollbarThumbStyle},
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    browser::BrowserPanel,
    diff::DiffPanel,
    panels::FilePanel,
    settings::Settings,
    terminal::TerminalPanel,
    defaults::Kind,
    layout::{Axis, Node, NodeId, PaneId, Side},
    ui::menu_action,
    workspace::Workspace,
};

const HEADER: f32 = 24.;
const GUTTER: f32 = 4.;
const HANDLE: f32 = 4.;
/// How far in from the area's edges a drop goes beside everything.
const EDGE_BAND: f32 = 28.;
/// Below this share a split's child cannot be dragged smaller.
const MIN_SHARE: f32 = 0.06;

/// A tab being dragged.
#[derive(Clone)]
pub struct TabDrag {
    pub pane: PaneId,
}

/// A group (by its tab strip) or a container (by its header) being dragged.
#[derive(Clone)]
pub struct GroupDrag {
    pub node: NodeId,
}

/// What a drag carries, as the workspace keeps it for a drop outside the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dragged {
    Tab(PaneId),
    Node(NodeId),
}

/// A drop zone as its window drew it, for a drag from another window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Zone {
    /// A window's whole groups area, for its edge bands.
    Area(NodeId),
    /// A group's content, below its tab strip.
    Group(NodeId),
    /// A container's header.
    Header(NodeId),
    /// A group's tab strip.
    Strip(NodeId),
    /// Tab `ix` on a group's strip.
    Tab(NodeId, usize),
}

/// Each window's drop zones (`None` for the main one), in its own pixels,
/// from its last paint.
pub(crate) type Zones = Rc<RefCell<HashMap<Option<u64>, Vec<(Zone, Bounds<Pixels>)>>>>;

/// Records where `zone` is drawn: an empty box over its parent.
fn zone_marker(zones: &Zones, float: Option<u64>, zone: Zone) -> impl IntoElement {
    let zones = zones.clone();
    canvas(move |bounds, _, _| zones.borrow_mut().entry(float).or_default().push((zone, bounds)), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// The drop a drag at `at` would make among a window's `zones`, as the zones
/// work it out for a drag inside that window: the edge bands first, then a
/// tab strip (a tab goes before the tab it is over, or last), a group (a tab
/// may join it), then a container's header (groups and containers only).
pub(crate) fn hint_at(zones: &[(Zone, Bounds<Pixels>)], dragged: Dragged, at: Point<Pixels>) -> Option<DropHint> {
    let edge = zones.iter().find_map(|(zone, bounds)| match zone {
        Zone::Area(root) => edge_side(*bounds, at).map(|side| DropHint {
            target: *root,
            drop: Drop::Side(side),
            edge: true,
        }),
        _ => None,
    });
    edge.or_else(|| {
        zones.iter().find_map(|&(zone, bounds)| {
            if !bounds.contains(&at) {
                return None;
            }
            let (target, drop) = match (zone, dragged) {
                (Zone::Strip(group), Dragged::Tab(_)) => {
                    // Before the first tab the pointer is left of the middle of.
                    let ix = zones
                        .iter()
                        .filter_map(|&(zone, tab)| match zone {
                            Zone::Tab(g, ix) if g == group && at.x < tab.center().x => Some(ix),
                            _ => None,
                        })
                        .min();
                    (group, Drop::Tab(ix))
                }
                (Zone::Group(group), Dragged::Tab(_)) => (group, center_or_side(bounds, at)),
                (Zone::Group(group), Dragged::Node(_)) => (group, Drop::Side(nearest_side(bounds, at))),
                (Zone::Header(node), Dragged::Node(_)) => {
                    // The side is taken against the whole container, as below.
                    let tray = Bounds::new(bounds.origin, size(bounds.size.width, bounds.size.height * 6.));
                    (node, Drop::Side(nearest_side(tray, at)))
                }
                _ => return None,
            };
            Some(DropHint { target, drop, edge: false })
        })
    })
}

/// A pinned preset's button being dragged to another place on the strip.
#[derive(Clone)]
pub struct PresetDrag {
    pub ix: usize,
}

/// The handle after child `ix` of `split` being dragged.
#[derive(Clone)]
pub struct ResizeDrag {
    pub split: NodeId,
    pub ix: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    /// Into the group (tabs only).
    Center,
    Side(Side),
    /// Onto the group's strip, before tab `ix` or last (tabs only; set for
    /// a drag from another window, the strip's own drops do it inside one).
    Tab(Option<usize>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropHint {
    pub target: NodeId,
    pub drop: Drop,
    /// From the edge band: drawn over the whole area, not the target.
    pub edge: bool,
}

/// The side of `bounds` nearest the pointer, relative to its size.
pub(crate) fn nearest_side(bounds: Bounds<Pixels>, at: Point<Pixels>) -> Side {
    let u = (at.x - bounds.origin.x).as_f32() / bounds.size.width.as_f32().max(1.);
    let v = (at.y - bounds.origin.y).as_f32() / bounds.size.height.as_f32().max(1.);
    [(Side::Left, u), (Side::Right, 1. - u), (Side::Top, v), (Side::Bottom, 1. - v)]
        .into_iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(side, _)| side)
        .unwrap_or(Side::Right)
}

/// The middle of a box joins, the rest goes beside.
fn center_or_side(bounds: Bounds<Pixels>, at: Point<Pixels>) -> Drop {
    let u = (at.x - bounds.origin.x).as_f32() / bounds.size.width.as_f32().max(1.);
    let v = (at.y - bounds.origin.y).as_f32() / bounds.size.height.as_f32().max(1.);
    if (0.25..0.75).contains(&u) && (0.25..0.75).contains(&v) {
        Drop::Center
    } else {
        Drop::Side(nearest_side(bounds, at))
    }
}

/// The edge of `bounds` whose band the pointer is in, if any.
fn edge_side(bounds: Bounds<Pixels>, at: Point<Pixels>) -> Option<Side> {
    if !bounds.contains(&at) {
        return None;
    }
    let band = px(EDGE_BAND);
    [
        (Side::Left, at.x - bounds.origin.x),
        (Side::Right, bounds.origin.x + bounds.size.width - at.x),
        (Side::Top, at.y - bounds.origin.y),
        (Side::Bottom, bounds.origin.y + bounds.size.height - at.y),
    ]
    .into_iter()
    .filter(|(_, distance)| *distance < band)
    .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    .map(|(side, _)| side)
}

/// The part of a box a drop would take, tinted.
fn drop_overlay(drop: Drop, cx: &App) -> impl IntoElement {
    let half = relative(0.5);
    let base = div().absolute().bg(cx.theme().drop_target).rounded(px(4.));
    deferred(match drop {
        Drop::Center | Drop::Tab(_) => base.inset_0(),
        Drop::Side(Side::Left) => base.left_0().top_0().bottom_0().w(half),
        Drop::Side(Side::Right) => base.right_0().top_0().bottom_0().w(half),
        Drop::Side(Side::Top) => base.left_0().right_0().top_0().h(half),
        Drop::Side(Side::Bottom) => base.left_0().right_0().bottom_0().h(half),
    })
    .with_priority(2)
}

/// A flex child taking `share` of the space along the split.
fn grow(element: Div, share: f32) -> Div {
    let mut element = element.flex_basis(px(0.)).flex_shrink_0();
    element.style().flex_grow = Some(share.max(0.001));
    element
}

pub struct DragLabel(pub SharedString);

impl Render for DragLabel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .py_1()
            .px_3()
            .rounded(cx.theme().radius)
            .bg(cx.theme().primary)
            .text_color(cx.theme().primary_foreground)
            .text_sm()
            .opacity(0.85)
            .child(self.0.clone())
    }
}

struct Nothing;

impl Render for Nothing {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl Workspace {
    /// A window's groups area (`root` is the main tree, or float `float`'s):
    /// the tree, its edge bands and where drops land.
    pub(crate) fn render_layout(&self, root: &Node, float: Option<u64>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let root_id = root.id();
        let edge = self.drop_hint.filter(|hint| hint.edge && hint.target == root_id);
        let this = cx.weak_entity();
        // Drawn again from here on: the zones are recorded as they paint.
        self.zones.borrow_mut().insert(float, Vec::new());
        // A drag from another window, over this one: its label, as there.
        let remote = self.remote_drag.filter(|(over, _)| *over == float).map(|(_, at)| at);
        let remote_label = remote.and_then(|_| self.dragging).map(|dragged| match dragged {
            Dragged::Tab(pane) => self.panes.get(&pane).map_or_else(|| SharedString::from("Tab"), |pane| pane.label(cx)),
            Dragged::Node(node) if self.tree.find(node).is_some_and(Node::is_group) => "Group".into(),
            Dragged::Node(_) => "Container".into(),
        });
        div()
            .id("groups")
            .relative()
            .size_full()
            .p_1()
            // The window's chrome, not the strip: tab bars stand out from it.
            .bg(cx.theme().sidebar)
            // These run before the zones inside (capture order): the edge
            // bands win, and elsewhere the target starts empty for the zone
            // under the pointer to set.
            .on_drag_move::<TabDrag>(cx.listener(move |this, event: &DragMoveEvent<TabDrag>, window, cx| {
                let dragged = Dragged::Tab(event.drag(cx).pane);
                this.dragging = Some(dragged);
                this.edge_hint(root_id, event.bounds, event.event.position, cx);
                this.forward_drag(float, dragged, event.event.position, window, cx);
            }))
            .on_drag_move::<GroupDrag>(cx.listener(move |this, event: &DragMoveEvent<GroupDrag>, window, cx| {
                let dragged = Dragged::Node(event.drag(cx).node);
                this.dragging = Some(dragged);
                this.edge_hint(root_id, event.bounds, event.event.position, cx);
                this.forward_drag(float, dragged, event.event.position, window, cx);
            }))
            .on_drop::<TabDrag>(cx.listener(|this, drag: &TabDrag, _, cx| {
                let hint = this.drop_hint.take();
                match hint {
                    Some(DropHint { target, drop: Drop::Center, .. }) => this.move_tab(drag.pane, target, None, None, cx),
                    Some(DropHint { target, drop: Drop::Side(side), .. }) => this.move_tab(drag.pane, target, Some(side), None, cx),
                    Some(DropHint { target, drop: Drop::Tab(ix), .. }) => this.move_tab(drag.pane, target, None, ix, cx),
                    None => cx.notify(),
                }
            }))
            .on_drop::<GroupDrag>(cx.listener(|this, drag: &GroupDrag, _, cx| {
                let hint = this.drop_hint.take();
                match hint {
                    Some(DropHint { target, drop: Drop::Side(side), .. }) => this.move_node(drag.node, target, side, cx),
                    _ => cx.notify(),
                }
            }))
            .child(zone_marker(&self.zones, float, Zone::Area(root_id)))
            .child(self.render_node(root, window, cx))
            .when_some(edge, |this, hint| this.child(drop_overlay(hint.drop, cx)))
            .when_some(remote.zip(remote_label), |this, (at, label)| {
                this.child(deferred(anchored().position(at).child(cx.new(|_| DragLabel(label)))).with_priority(3))
            })
            // A drag let go outside the window gets no drop event: the
            // window keeps the mouse while a button is down, so the release
            // still comes here, outside every hitbox.
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                                _ = this.update(cx, |this, cx| this.drag_released(float, event.position, window, cx));
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            )
            .into_any_element()
    }

    /// A drag outside the window it started in (float `from`'s, or the main
    /// one): over another window of the session, the drop it would make
    /// there, from the zones that window drew.
    fn forward_drag(&mut self, from: Option<u64>, dragged: Dragged, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let inside = Bounds::new(Point::default(), window.viewport_size()).contains(&position);
        let remote = if inside { None } else { self.window_under(from, crate::workspace::on_screen(window, position), cx) };
        if let Some((float, local)) = remote {
            let hint = self.zones.borrow().get(&float).and_then(|zones| hint_at(zones, dragged, local));
            self.set_hint(hint, cx);
        }
        if self.remote_drag != remote {
            self.remote_drag = remote;
            cx.notify();
        }
    }

    fn edge_hint(&mut self, root: NodeId, bounds: Bounds<Pixels>, at: Point<Pixels>, cx: &mut Context<Self>) {
        let hint = edge_side(bounds, at).map(|side| DropHint {
            target: root,
            drop: Drop::Side(side),
            edge: true,
        });
        self.set_hint(hint, cx);
    }

    fn set_hint(&mut self, hint: Option<DropHint>, cx: &mut Context<Self>) {
        if self.drop_hint != hint {
            self.drop_hint = hint;
            cx.notify();
        }
    }

    /// A zone's hint, unless the pointer is in an edge band.
    fn zone_hint(&mut self, target: NodeId, drop: Drop, cx: &mut Context<Self>) {
        if self.drop_hint.is_some_and(|hint| hint.edge) {
            return;
        }
        self.set_hint(Some(DropHint { target, drop, edge: false }), cx);
    }

    fn zone_overlay(&self, node: NodeId) -> Option<Drop> {
        self.drop_hint
            .filter(|hint| !hint.edge && hint.target == node && !matches!(hint.drop, Drop::Tab(_)))
            .map(|hint| hint.drop)
    }

    /// Where on `group`'s strip a drag from another window would put its tab.
    fn strip_hint(&self, group: NodeId) -> Option<Option<usize>> {
        match self.drop_hint {
            Some(DropHint { target, drop: Drop::Tab(ix), .. }) if target == group => Some(ix),
            _ => None,
        }
    }

    fn render_node(&self, node: &Node, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Group { id, tabs, active } => self.render_group(*id, tabs, *active, window, cx),
            Node::Split { id, axis, children, sizes } => self.render_split(*id, *axis, children, sizes, window, cx),
        }
    }

    // -- Splits and containers -----------------------------------------------

    fn render_split(&self, id: NodeId, axis: Axis, children: &[Node], sizes: &[f32], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let horizontal = axis == Axis::Horizontal;
        let mut body = if horizontal { h_flex() } else { v_flex() }
            .id(("split", id))
            .size_full()
            .min_w_0()
            .min_h_0()
            .items_stretch()
            .on_drag_move::<ResizeDrag>(cx.listener(move |this, event: &DragMoveEvent<ResizeDrag>, _, cx| {
                let drag = event.drag(cx).clone();
                if drag.split != id {
                    return;
                }
                let bounds = event.bounds;
                let at = event.event.position;
                let share = if horizontal {
                    (at.x - bounds.origin.x).as_f32() / bounds.size.width.as_f32().max(1.)
                } else {
                    (at.y - bounds.origin.y).as_f32() / bounds.size.height.as_f32().max(1.)
                };
                this.tree.resize(id, drag.ix, share, MIN_SHARE);
                this.changed(cx);
            }));
        for (ix, child) in children.iter().enumerate() {
            if ix > 0 {
                body = body.child(self.render_handle(id, ix - 1, horizontal, cx));
            }
            let share = sizes.get(ix).copied().unwrap_or(1.0 / children.len() as f32);
            body = body.child(
                grow(div(), share)
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_node(child, window, cx)),
            );
        }

        if self.tree.is_root(id) {
            // A window's root holds every group in it and draws no frame.
            return body.into_any_element();
        }

        let theme = cx.theme();
        let current = self.defaults.own(id, true);
        // A frame around its groups, not another surface: a faint line and
        // barely a tint, so nesting shows as nested frames.
        let frame = theme.muted_foreground.opacity(0.20);
        let tray = theme.muted_foreground.opacity(0.04);
        let hint = self.zone_overlay(id);
        let hover_group = SharedString::from(format!("container-header-{id}"));

        v_flex()
            .id(("container", id))
            .relative()
            .size_full()
            .rounded(px(6.))
            .border_1()
            .border_color(frame)
            .bg(tray)
            .px(px(GUTTER))
            .pb(px(GUTTER))
            .child(
                h_flex()
                    .id(("container-header", id))
                    // The header only: hovering a group inside must not
                    // reveal the container's actions.
                    .group(hover_group.clone())
                    .relative()
                    .flex_none()
                    .h(px(HEADER))
                    .gap_0p5()
                    // The header is where a group or container drops beside
                    // this container; the side is taken against the whole box.
                    .on_drag_move::<GroupDrag>(cx.listener(move |this, event: &DragMoveEvent<GroupDrag>, _, cx| {
                        if event.bounds.contains(&event.event.position) {
                            let tray = Bounds::new(event.bounds.origin, size(event.bounds.size.width, event.bounds.size.height * 6.));
                            this.zone_hint(id, Drop::Side(nearest_side(tray, event.event.position)), cx);
                        }
                    }))
                    // The default button leads: its kind badge is state, so it
                    // stays; with no kind it hides until the header is hovered.
                    .child(
                        div()
                            .flex_none()
                            .when(current.is_none(), |this| this.invisible().group_hover(hover_group.clone(), |this| this.visible()))
                            .child(self.default_button(id, true, current, false, cx)),
                    )
                    // The rest of the header is the grip.
                    .child(
                        div()
                            .id(("container-grip", id))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .cursor_grab()
                            .on_drag(GroupDrag { node: id }, |_, _, _, cx| cx.new(|_| DragLabel("Container".into()))),
                    )
                    .child(
                        h_flex()
                            .gap_0p5()
                            .invisible()
                            .group_hover(hover_group, |this| this.visible())
                            .children(self.container_actions(id, "container", true, cx))
                            .child(self.container_menu(id, cx)),
                    )
                    .child(zone_marker(&self.zones, self.tree.float_of(id), Zone::Header(id))),
            )
            .child(div().flex_1().min_h_0().child(body))
            .when_some(hint, |this, drop| this.child(drop_overlay(drop, cx)))
            .into_any_element()
    }

    fn render_handle(&self, split: NodeId, ix: usize, horizontal: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = cx.theme().primary.opacity(0.5);
        div()
            .id(SharedString::from(format!("handle-{split}-{ix}")))
            .flex_none()
            .when(horizontal, |this| this.w(px(HANDLE)).h_full().cursor_col_resize())
            .when(!horizontal, |this| this.h(px(HANDLE)).w_full().cursor_row_resize())
            .hover(move |this| this.bg(hover))
            .on_drag(ResizeDrag { split, ix }, |_, _, _, cx| cx.new(|_| Nothing))
    }

    /// Split and flip for a container, or the workspace when `node` is the
    /// root (whose flip is offered only once it is a split).
    pub(crate) fn container_actions(&self, node: NodeId, scope: &'static str, can_flip: bool, cx: &mut Context<Self>) -> Vec<Button> {
        let mut buttons = vec![self.split_button(node, scope, false, cx)];
        if can_flip {
            buttons.push(
                Button::new(("flip", node))
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(IconName::RotateCw))
                    .tooltip(format!("Flip {scope} layout (side by side / stacked)"))
                    .on_click(cx.listener(move |this, _, _, cx| this.flip(node, cx))),
            );
        }
        buttons
    }

    /// One split button, as den's (and VS Code's): splits right, or down while
    /// Alt is held, its icon and tooltip following the key.
    pub(crate) fn split_button(&self, node: NodeId, scope: &'static str, small: bool, cx: &mut Context<Self>) -> Button {
        let down = self.alt_held;
        Button::new(("split", node))
            .map(|button| if small { button.small() } else { button.xsmall() })
            .ghost()
            .icon(Icon::new(if down { IconName::Rows2 } else { IconName::Columns2 }))
            .tooltip(if down {
                format!("Split {scope} down (Ctrl+Shift+-)")
            } else {
                format!("Split {scope} right (Ctrl+Shift+D)
[Alt] Split {scope} down")
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let side = if event.modifiers().alt { Side::Bottom } else { Side::Right };
                this.split(node, side, cx)
            }))
    }

    /// The button that opens the default menu of a group or container. With a
    /// kind set it shows that kind's icon, highlighted.
    fn default_button(&self, node: NodeId, container: bool, current: Option<Kind>, small: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let scope = if container { "container" } else { "group" };
        let tooltip = match current {
            Some(kind) => format!("Default {scope} for new {}", kind.label()),
            None => format!("Make this {scope} the default for a kind of tab"),
        };
        let this = cx.weak_entity();
        Button::new((if container { "container-default" } else { "group-default" }, node))
            .map(|button| if small { button.small() } else { button.xsmall() })
            .icon(Icon::new(current.map_or(IconName::SquareArrowDownRight, Kind::icon)))
            // Highlighted, not filled, as the sidebar's view buttons.
            .ghost()
            .selected(current.is_some())
            .tooltip(tooltip)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu
                    .check_side(MenuSide::Right)
                    .label(format!("DEFAULT {} FOR", scope.to_uppercase()));
                for kind in Kind::ALL {
                    let this = this.clone();
                    menu = menu.item(
                        PopupMenuItem::new(kind.label())
                            .icon(Icon::new(kind.icon()))
                            .checked(current == Some(kind))
                            .on_click(move |_, _, cx| {
                                _ = this.update(cx, |this, cx| this.set_default(node, container, Some(kind), cx));
                            }),
                    );
                }
                let this = this.clone();
                menu.separator().item(
                    PopupMenuItem::new("None")
                        .icon(Icon::new(IconName::Minus))
                        .checked(current.is_none())
                        .on_click(move |_, _, cx| {
                            _ = this.update(cx, |this, cx| this.set_default(node, container, None, cx));
                        }),
                )
            })
    }

    // -- Groups ----------------------------------------------------------------

    fn render_group(&self, id: NodeId, tabs: &[PaneId], active: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let is_active = id == self.active_group;
        let shown = tabs.get(active).and_then(|pane| self.panes.get(pane)).cloned();
        let hint = self.zone_overlay(id);
        let border = if is_active { theme.primary.opacity(0.7) } else { theme.border };
        let muted = theme.muted_foreground;

        let content = div()
            .id(("group-body", id))
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(theme.background)
            .on_drag_move::<TabDrag>(cx.listener(move |this, event: &DragMoveEvent<TabDrag>, _, cx| {
                if event.bounds.contains(&event.event.position) {
                    this.zone_hint(id, center_or_side(event.bounds, event.event.position), cx);
                }
            }))
            .on_drag_move::<GroupDrag>(cx.listener(move |this, event: &DragMoveEvent<GroupDrag>, _, cx| {
                if event.bounds.contains(&event.event.position) {
                    this.zone_hint(id, Drop::Side(nearest_side(event.bounds, event.event.position)), cx);
                }
            }))
            .map(|this| match &shown {
                Some(pane) => this.child(pane.view().cached(StyleRefinement::default().absolute().size_full())),
                None => this.child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .text_sm()
                        .text_color(muted)
                        .child("Empty group")
                        .child("Open a file or a terminal here, or drag a tab in."),
                ),
            })
            .child(zone_marker(&self.zones, self.tree.float_of(id), Zone::Group(id)))
            .when_some(hint, |this, drop| this.child(drop_overlay(drop, cx)));

        v_flex()
            .id(("group", id))
            .size_full()
            .min_w_0()
            .min_h_0()
            .rounded(px(4.))
            .border_1()
            .border_color(border)
            .overflow_hidden()
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| this.set_active_group(id, cx)))
            .child(self.render_tab_strip(id, tabs, active, window, cx))
            .child(content)
            .into_any_element()
    }

    fn render_tab_strip(&self, group: NodeId, tabs: &[PaneId], active: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // The strip's scroll, and the tab it last showed: a tab that becomes
        // the active one is scrolled into view, as in VS Code.
        let scroll = window.use_keyed_state(("tab-strip-scroll", group), cx, |_, _| (ScrollHandle::new(), None::<PaneId>));
        let (handle, shown) = scroll.read(cx).clone();
        let active_pane = tabs.get(active).copied();
        if active_pane != shown {
            if active_pane.is_some() {
                handle.scroll_to_item(active);
            }
            scroll.update(cx, |state, _| state.1 = active_pane);
        }

        let theme = cx.theme();
        let close_buttons = Settings::get(cx).tab_close_button;
        let buttons = Settings::get(cx).group_buttons;
        // A drag from another window over this strip (see `hint_at`).
        let float = self.tree.float_of(group);
        let strip_hint = self.strip_hint(group);
        let tab_elements: Vec<Tab> = tabs
            .iter()
            .enumerate()
            .filter_map(|(ix, pane_id)| {
                let pane = self.panes.get(pane_id)?;
                let pane_id = *pane_id;
                let label = pane.label(cx);
                let dirty = pane.is_dirty(cx);
                let preview = self.preview == Some(pane_id);
                Some(
                    Tab::new()
                        .when(strip_hint == Some(Some(ix)), |tab| tab.border_l_2().border_color(cx.theme().drag_border))
                        .child(
                            h_flex()
                                .relative()
                                .gap_1p5()
                                .child(zone_marker(&self.zones, float, Zone::Tab(group, ix)))
                                .child(pane.icon_element(cx).unwrap_or_else(|| Icon::new(pane.icon(cx)).small().into_any_element()))
                                .when(self.attention.contains(&pane_id), |this| {
                                    this.child(div().size(px(7.)).rounded_full().flex_none().bg(cx.theme().warning))
                                })
                                .child(div().max_w(px(220.)).truncate().when(preview, |this| this.italic()).child(label.clone()))
                                .when(dirty, |this| this.child("●")),
                        )
                        .when(close_buttons, |tab| tab.suffix(
                            Button::new(("close-tab", pane_id))
                                .icon(IconName::X)
                                .xsmall()
                                .ghost()
                                .ml(-px(8.))
                                .mr_2()
                                .tab_stop(false)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.request_close_pane(pane_id, window, cx);
                                })),
                        ))
                        .selected(ix == active)
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            // Double-clicking a tab keeps the preview, or makes it the preview again.
                            if event.click_count() == 2 {
                                this.toggle_preview(pane_id, cx);
                            }
                            this.show_pane(pane_id, true, window, cx)
                        }))
                        // Right-click: the strip's menu is for this tab.
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.tab_menu = Some(pane_id)))
                        // Middle-click closes, as in den.
                        .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.request_close_pane(pane_id, window, cx);
                        }))
                        .on_drag(TabDrag { pane: pane_id }, move |_, _, _, cx| cx.new(|_| DragLabel(label.clone())))
                        .drag_over::<TabDrag>(|this, _, _, cx| this.border_l_2().border_color(cx.theme().drag_border))
                        .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| {
                            this.drop_hint = None;
                            this.move_tab(drag.pane, group, None, Some(ix), cx);
                        })),
                )
            })
            .collect();

        let current = self.defaults.own(group, false);
        let actions = h_flex()
            .h_full()
            .flex_none()
            .px_1()
            .gap_0p5()
            .bg(theme.tab_bar)
            // The terminal presets, as den's preset buttons; a group that is the
            // default of another kind does not get the buttons for this one.
            .when(buttons.shell && self.defaults.allows(&self.tree, group, Kind::Terminals), |this| this.child(
                Button::new(("group-shell", group))
                    .small()
                    .ghost()
                    .icon(Icon::new(IconName::SquareTerminal))
                    .tooltip("New Terminal (Ctrl+Shift+T)")
                    .on_click(cx.listener(move |this, _, window, cx| this.open_terminal(Some(group), None, false, window, cx))),
            ))
            .when(buttons.browser && self.defaults.allows(&self.tree, group, Kind::Browsers), |this| this.child(
                Button::new(("group-browser", group))
                    .small()
                    .ghost()
                    .icon(Icon::new(IconName::Globe))
                    .tooltip("New Browser (Ctrl+Shift+B)")
                    .on_click(cx.listener(move |this, _, window, cx| this.open_browser(Some(group), None, window, cx))),
            ))
            .children(Settings::get(cx).presets.iter().enumerate().filter(|(_, preset)| {
                preset.pinned && self.defaults.allows(&self.tree, group, preset.kind())
            }).map(|(ix, preset)| {
                let launch = preset.clone();
                let name = preset.name.clone();
                // Drag a preset's button onto another to put it there, as in den.
                div()
                    .id(SharedString::from(format!("preset-slot-{group}-{ix}")))
                    .rounded(px(4.))
                    .on_drag(PresetDrag { ix }, move |_, _, _, cx| cx.new(|_| DragLabel(name.clone().into())))
                    .drag_over::<PresetDrag>(|this, _, _, cx| this.bg(cx.theme().drop_target))
                    .on_drop(cx.listener(move |_, drag: &PresetDrag, _, cx| {
                        let from = drag.ix;
                        Settings::update(cx, |s| {
                            if from != ix && from < s.presets.len() && ix < s.presets.len() {
                                let preset = s.presets.remove(from);
                                s.presets.insert(ix, preset);
                            }
                        });
                        cx.stop_propagation();
                    }))
                    .child(
                        Button::new(SharedString::from(format!("preset-{group}-{ix}")))
                            .small()
                            .ghost()
                            .child(preset_badge(preset, cx))
                            .tooltip(format!("{} ({})", preset.name, preset.command))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if launch.browser {
                                    this.open_browser(Some(group), Some(launch.command.clone()), window, cx);
                                } else {
                                    this.open_terminal(Some(group), Some(launch.command.clone()), launch.agent, window, cx);
                                }
                            })),
                    )
            }))
            .when(buttons.split, |this| this.child(self.split_button(group, "group", true, cx)))
            // An empty group closes with its own X, as in den (not the last one).
            .when(tabs.is_empty() && self.tree.groups().len() > 1, |this| {
                this.child(
                    Button::new(("group-close", group))
                        .small()
                        .ghost()
                        .icon(Icon::new(IconName::X))
                        .tooltip("Close Group (Ctrl+Shift+Q)")
                        .on_click(cx.listener(move |this, _, window, cx| this.request_close_group(group, window, cx))),
                )
            })
            .child(self.group_menu(group, cx));

        let strip = TabBar::new(("tab-bar", group))
            // A tab's height (gpui-kit's default size), so an empty group's
            // strip does not grow when its first tab opens.
            .min_h(px(32.))
            .track_scroll(&handle)
            // The group's default leads the strip, as the container's does its header.
            .prefix(h_flex().h_full().flex_none().px_1().bg(cx.theme().tab_bar).child(self.default_button(group, false, current, true, cx)))
            .children(tab_elements)
            .last_empty_space(
                div()
                    .id("tab-bar-empty-space")
                    .h_full()
                    // With no tab beside it, `h_full` alone leaves it no height
                    // (the strip's scroller sizes to its content).
                    .min_h(px(32.))
                    .flex_grow_1()
                    .min_w_16()
                    // Double-clicking the empty strip opens another tab of the kind
                    // the group is the default for, else of the kind it shows.
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            this.open_more(group, window, cx);
                        }
                    }))
                    .when(strip_hint == Some(None), |this| this.bg(cx.theme().drop_target))
                    .drag_over::<TabDrag>(|this, _, _, cx| this.bg(cx.theme().drop_target))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| {
                        this.drop_hint = None;
                        this.move_tab(drag.pane, group, None, None, cx);
                    })),
            )
            .suffix(actions);

        // Grabbing the strip anywhere but on a tab (its empty part, the gaps
        // between the buttons) moves the whole group, as in den. A tab's own
        // drag starts first, so pressing a tab still drags just that tab.
        //
        // A tab's right-click menu is the strip's: the tab records itself as it
        // is pressed (after the press clears it here) and the menu, built once
        // the press is over, is for that tab; elsewhere on the strip it is
        // empty and does not open.
        let this = cx.weak_entity();
        div()
            .id(("group-grip", group))
            .relative()
            .w_full()
            .flex_none()
            .cursor_grab()
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.tab_menu = None))
            .on_drag(GroupDrag { node: group }, |_, _, _, cx| cx.new(|_| DragLabel("Group".into())))
            // The wheel scrolls the tabs sideways, as in VS Code (the strip
            // itself only takes horizontal deltas).
            .on_scroll_wheel({
                let handle = handle.clone();
                cx.listener(move |_, event: &ScrollWheelEvent, window, cx| {
                    let delta = event.delta.pixel_delta(window.line_height());
                    if delta.y.abs() <= delta.x.abs() {
                        return;
                    }
                    let max = handle.max_offset().x;
                    let offset = handle.offset();
                    let x = (offset.x + delta.y).clamp(-max, px(0.));
                    if x != offset.x {
                        handle.set_offset(point(x, offset.y));
                        cx.notify();
                    }
                })
            })
            .child(strip)
            // A thin bar flush with the strip's bottom, as VS Code's.
            .child(
                Scrollbar::horizontal(&handle)
                    .id(("tab-strip-scrollbar", group))
                    .mode(ScrollbarMode::Hover)
                    .styles(|styles| {
                        let thin = |thumb: ScrollbarThumbStyle| thumb.width(px(3.)).inset(px(0.)).radius(px(0.));
                        // No track: it would cover the bottom of the tabs.
                        styles
                            .track(|track| track.width(px(3.)).bg(transparent_black()))
                            .thumb(thin)
                            .thumb_hover(thin)
                            .thumb_active(thin)
                    }),
            )
            .child(zone_marker(&self.zones, float, Zone::Strip(group)))
            .context_menu(move |menu, _, cx| {
                let Some(workspace) = this.upgrade() else { return menu };
                let workspace = workspace.read(cx);
                match workspace.tab_menu {
                    Some(pane) => workspace.tab_menu(menu, pane, &this, cx),
                    None => menu,
                }
            })
            .into_any_element()
    }

    /// A tab's right-click menu, as VS Code's: closing it and the tabs beside
    /// it, what its kind offers (its file's path, the page's address, another
    /// terminal like it), and moving it.
    fn tab_menu(&self, menu: PopupMenu, pane: PaneId, this: &WeakEntity<Self>, cx: &App) -> PopupMenu {
        let (Some(group), Some(view)) = (self.tree.group_of(pane), self.panes.get(&pane).map(|p| p.view())) else { return menu };
        let tabs = self.tree.tabs(group).to_vec();
        let ix = tabs.iter().position(|p| *p == pane).unwrap_or(0);
        let others: Vec<PaneId> = tabs.iter().copied().filter(|p| *p != pane).collect();
        let right = tabs[ix + 1..].to_vec();
        let (alone, last) = (others.is_empty(), right.is_empty());
        let mut menu = menu
            .item(menu_action(this, "Close", move |ws, window, cx| ws.request_close_pane(pane, window, cx)).icon(Icon::new(IconName::X)))
            .item(menu_action(this, "Close Others", move |ws, window, cx| ws.request_close_panes(others.clone(), window, cx)).disabled(alone))
            .item(menu_action(this, "Close to the Right", move |ws, window, cx| ws.request_close_panes(right.clone(), window, cx)).disabled(last))
            .item(menu_action(this, "Close All", move |ws, window, cx| ws.request_close_panes(tabs.clone(), window, cx)).icon(Icon::new(IconName::ListX)));

        if let Ok(file) = view.clone().downcast::<FilePanel>() {
            let path = file.read(cx).path().to_path_buf();
            menu = menu.separator();
            if self.preview == Some(pane) {
                menu = menu.item(menu_action(this, "Keep Open", move |ws, _, cx| ws.toggle_preview(pane, cx)).icon(Icon::new(IconName::Pin)));
            }
            menu = path_items(menu, this, &path, &self.root);
            let dir = path.parent().map(|dir| dir.to_path_buf()).unwrap_or_else(|| self.root.clone());
            menu = menu.item(
                menu_action(this, "Open Terminal Here", move |ws, window, cx| ws.open_shell_in(dir.clone(), window, cx)).icon(Icon::new(IconName::SquareTerminal)),
            );
        } else if let Ok(diff) = view.clone().downcast::<DiffPanel>() {
            let path = diff.read(cx).path().to_path_buf();
            let open = path.clone();
            menu = menu.separator().item(
                menu_action(this, "Open File", move |ws, window, cx| ws.open_file(open.clone(), false, window, cx))
                    .icon(Icon::new(IconName::File))
                    .disabled(!path.is_file()),
            );
            menu = path_items(menu, this, &path, &self.root);
        } else if let Ok(browser) = view.clone().downcast::<BrowserPanel>() {
            let url = browser.read(cx).url().to_string();
            let open = url.clone();
            menu = menu
                .separator()
                .item(menu_action(this, "Copy URL", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))).icon(Icon::new(IconName::Link)))
                .item(menu_action(this, "Open in Default Browser", move |_, _, cx| cx.open_url(&open)).icon(Icon::new(IconName::SquareArrowOutUpRight)))
                .item(menu_action(this, "Reload", move |_, _, cx| browser.read(cx).reload()).icon(Icon::new(IconName::RotateCw)));
        } else if let Ok(terminal) = view.clone().downcast::<TerminalPanel>() {
            let cwd = terminal.read(cx).cwd().to_string_lossy().to_string();
            menu = menu
                .separator()
                .item(menu_action(this, "Duplicate", move |ws, window, cx| ws.duplicate_terminal(pane, window, cx)).icon(Icon::new(IconName::CopyPlus)))
                .item(menu_action(this, "Copy Working Directory", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(cwd.clone()))).icon(Icon::new(IconName::Copy)));
        }

        // Splitting moves the tab into a new group, as dragging it to a side
        // does; a group's only tab has nowhere to split from.
        let floating = self.tree.float_of(group).is_some();
        let own_window = alone && floating && self.tree.is_root(group);
        let menu = menu
            .separator()
            .item(menu_action(this, "Split Right", move |ws, _, cx| ws.move_tab(pane, group, Some(Side::Right), None, cx)).icon(Icon::new(IconName::Columns2)).disabled(alone))
            .item(menu_action(this, "Split Down", move |ws, _, cx| ws.move_tab(pane, group, Some(Side::Bottom), None, cx)).icon(Icon::new(IconName::Rows2)).disabled(alone))
            .separator()
            .item(menu_action(this, "Move into New Window", move |ws, _, cx| ws.float_tab(pane, None, cx)).icon(Icon::new(IconName::ExternalLink)).disabled(own_window));
        if !floating {
            return menu;
        }
        menu.item(menu_action(this, "Move into Main Window", move |ws, _, cx| ws.dock_tab(pane, cx)).icon(Icon::new(IconName::Minimize)))
    }

    /// The container's ⋮ menu.
    fn container_menu(&self, node: NodeId, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        let floating = self.tree.float_of(node).is_some();
        Button::new(("container-menu", node))
            .xsmall()
            .ghost()
            .icon(Icon::new(IconName::EllipsisVertical))
            .tooltip("Container")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let item = |label: &'static str, action: fn(&mut Workspace, NodeId, &mut Window, &mut Context<Workspace>)| {
                    let this = this.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        _ = this.update(cx, |this, cx| action(this, node, window, cx));
                    })
                };
                menu.item(item("Add Group Right", |this, node, _, cx| this.split(node, Side::Right, cx)).icon(Icon::new(IconName::Columns2)))
                    .item(item("Add Group Below", |this, node, _, cx| this.split(node, Side::Bottom, cx)).icon(Icon::new(IconName::Rows2)))
                    .item(item("Flip Layout", |this, node, _, cx| this.flip(node, cx)).icon(Icon::new(IconName::RotateCw)))
                    .separator()
                    .map(|menu| window_items(menu, &this, node, floating))
                    .separator()
                    .item(item("Close Container", |this, node, window, cx| this.request_close_container(node, window, cx)))
            })
    }

    /// The group's ⋮ menu.
    fn group_menu(&self, group: NodeId, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        let floating = self.tree.float_of(group).is_some();
        Button::new(("group-menu", group))
            .small()
            .ghost()
            .icon(Icon::new(IconName::EllipsisVertical))
            .tooltip("Group")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                let item = |label: &'static str, action: fn(&mut Workspace, NodeId, &mut Window, &mut Context<Workspace>)| {
                    let this = this.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        _ = this.update(cx, |this, cx| action(this, group, window, cx));
                    })
                };
                let menu = menu
                    .item(item("Split Right", |this, group, _, cx| this.split(group, Side::Right, cx)).icon(Icon::new(IconName::Columns2)))
                    .item(item("Split Down", |this, group, _, cx| this.split(group, Side::Bottom, cx)).icon(Icon::new(IconName::Rows2)))
                    .separator()
                    .map(|menu| window_items(menu, &this, group, floating))
                    .separator()
                    .item(item("Close Group", |this, group, window, cx| this.request_close_group(group, window, cx)));
                group_buttons_menu(menu, cx)
                    .separator()
                    .item(item("Edit Presets…", |this, _, window, cx| this.open_settings(true, window, cx)).icon(Icon::new(IconName::Settings)))
            })
    }
}

/// A file's paths, and showing it, as the Explorer's menu has them.
fn path_items(menu: PopupMenu, this: &WeakEntity<Workspace>, path: &std::path::Path, root: &std::path::Path) -> PopupMenu {
    let full = path.display().to_string();
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
    let (on_disk, inside) = (path.exists(), path.starts_with(root));
    let (reveal, show) = (path.to_path_buf(), path.to_path_buf());
    menu.item(menu_action(this, "Copy Path", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(full.clone()))).icon(Icon::new(IconName::Copy)))
        .item(menu_action(this, "Copy Relative Path", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(rel.clone()))).icon(Icon::new(IconName::Copy)))
        .item(menu_action(this, "Reveal in File Explorer", move |_, _, _| crate::explorer::reveal(&reveal)).icon(Icon::new(IconName::FolderOpen)).disabled(!on_disk))
        .item(
            menu_action(this, "Reveal in Explorer View", move |ws, window, cx| ws.reveal_in_sidebar(show.clone(), window, cx))
                .icon(Icon::new(IconName::Files))
                .disabled(!on_disk || !inside),
        )
}

/// Moving a group or container into a window of its own, or (in a floating
/// window) back into the main one, as VS Code's editor groups.
fn window_items(menu: PopupMenu, this: &WeakEntity<Workspace>, node: NodeId, floating: bool) -> PopupMenu {
    let new_window = this.clone();
    let menu = menu.item(PopupMenuItem::new("Move into New Window").icon(Icon::new(IconName::ExternalLink)).on_click(move |_, _, cx| {
        _ = new_window.update(cx, |this, cx| this.float_node(node, None, cx));
    }));
    if !floating {
        return menu;
    }
    let main = this.clone();
    menu.item(PopupMenuItem::new("Move into Main Window").icon(Icon::new(IconName::Minimize)).on_click(move |_, _, cx| {
        _ = main.update(cx, |this, cx| {
            let root = this.tree.root.id();
            this.move_node(node, root, Side::Right, cx)
        });
    }))
}

/// The strip's buttons, checked when shown, as den's ⋮ menu lists them:
/// the built-in ones and each preset (its pin).
fn group_buttons_menu(menu: PopupMenu, cx: &App) -> PopupMenu {
    use crate::settings::GroupButtons;
    let settings = Settings::get(cx);
    let buttons = settings.group_buttons;
    let toggle = |label: &'static str, on: bool, flip: fn(&mut GroupButtons)| {
        PopupMenuItem::new(label).checked(on).on_click(move |_, _, cx| Settings::update(cx, |s| flip(&mut s.group_buttons)))
    };
    let mut menu = menu
        .separator()
        .label("BUTTONS")
        .item(toggle("Shell", buttons.shell, |b| b.shell = !b.shell))
        .item(toggle("Browser", buttons.browser, |b| b.browser = !b.browser))
        .item(toggle("Split", buttons.split, |b| b.split = !b.split));
    for (ix, preset) in settings.presets.iter().enumerate() {
        menu = menu.item(PopupMenuItem::new(preset.name.clone()).checked(preset.pinned).on_click(move |_, _, cx| {
            Settings::update(cx, |s| {
                if let Some(preset) = s.presets.get_mut(ix) {
                    preset.pinned = !preset.pinned;
                }
            })
        }));
    }
    menu
}

/// A preset's mark, as den draws it: its picked icon, else the logo of the
/// program it runs, a local server's port, or its letter.
pub fn preset_badge(preset: &crate::settings::Preset, cx: &App) -> AnyElement {
    crate::preset_icon::render(preset, preset.icon.as_deref(), 16., cx)
}

#[cfg(test)]
mod tests {
    use super::{Drop, DropHint, Dragged, Zone, hint_at};
    use crate::layout::Side;
    use gpui_kit::{Bounds, Pixels, point, px, size};

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn drops_from_another_window_follow_its_zones() {
        // Area 1 holds container 10 (header at the top) with groups 2 and 3 side by side.
        let zones = [
            (Zone::Area(1), rect(0., 0., 1000., 600.)),
            (Zone::Header(10), rect(40., 40., 920., 24.)),
            (Zone::Group(2), rect(40., 64., 460., 500.)),
            (Zone::Group(3), rect(500., 64., 460., 500.)),
        ];
        let tab = Dragged::Tab(100);
        let group = Dragged::Node(4);
        let hint = |dragged, x, y| hint_at(&zones, dragged, point(px(x), px(y)));
        // The edge bands go beside everything.
        assert_eq!(hint(tab, 5., 300.), Some(DropHint { target: 1, drop: Drop::Side(Side::Left), edge: true }));
        // A tab joins a group in its middle, or starts one beside it.
        assert_eq!(hint(tab, 270., 300.), Some(DropHint { target: 2, drop: Drop::Center, edge: false }));
        assert_eq!(hint(tab, 950., 300.), Some(DropHint { target: 3, drop: Drop::Side(Side::Right), edge: false }));
        // A group never joins: beside the group, or the container by its header.
        assert!(matches!(hint(group, 270., 300.), Some(DropHint { target: 2, drop: Drop::Side(_), edge: false })));
        assert_eq!(hint(group, 60., 50.), Some(DropHint { target: 10, drop: Drop::Side(Side::Left), edge: false }));
        assert_eq!(hint(tab, 500., 50.), None);
    }

    #[test]
    fn tabs_from_another_window_go_onto_a_strip() {
        let zones = [
            (Zone::Area(1), rect(0., 0., 1000., 600.)),
            (Zone::Strip(2), rect(100., 100., 400., 30.)),
            (Zone::Tab(2, 0), rect(110., 105., 80., 20.)),
            (Zone::Tab(2, 1), rect(200., 105., 80., 20.)),
            (Zone::Group(2), rect(100., 130., 400., 300.)),
        ];
        let hint = |dragged, x| hint_at(&zones, dragged, point(px(x), px(115.)));
        let onto = |ix| Some(DropHint { target: 2, drop: Drop::Tab(ix), edge: false });
        // Before the tab whose middle is right of the pointer, else last.
        assert_eq!(hint(Dragged::Tab(100), 120.), onto(Some(0)));
        assert_eq!(hint(Dragged::Tab(100), 180.), onto(Some(1)));
        assert_eq!(hint(Dragged::Tab(100), 400.), onto(None));
        // A group does not go onto a strip.
        assert_eq!(hint(Dragged::Node(4), 400.), None);
    }
}
