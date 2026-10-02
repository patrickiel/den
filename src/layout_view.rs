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

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Side as MenuSide, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    settings::Settings,
    defaults::Kind,
    layout::{Axis, Node, NodeId, PaneId, Side},
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropHint {
    pub target: NodeId,
    pub drop: Drop,
    /// From the edge band: drawn over the whole area, not the target.
    pub edge: bool,
}

/// The side of `bounds` nearest the pointer, relative to its size.
fn nearest_side(bounds: Bounds<Pixels>, at: Point<Pixels>) -> Side {
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
        Drop::Center => base.inset_0(),
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
    /// The groups area: the tree, its edge bands and where drops land.
    pub(crate) fn render_layout(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let edge = self.drop_hint.filter(|hint| hint.edge);
        div()
            .id("groups")
            .relative()
            .size_full()
            .p_1()
            .bg(cx.theme().tab_bar)
            // These run before the zones inside (capture order): the edge
            // bands win, and elsewhere the target starts empty for the zone
            // under the pointer to set.
            .on_drag_move::<TabDrag>(cx.listener(|this, event: &DragMoveEvent<TabDrag>, _, cx| {
                this.edge_hint(event.bounds, event.event.position, cx)
            }))
            .on_drag_move::<GroupDrag>(cx.listener(|this, event: &DragMoveEvent<GroupDrag>, _, cx| {
                this.edge_hint(event.bounds, event.event.position, cx)
            }))
            .on_drop::<TabDrag>(cx.listener(|this, drag: &TabDrag, _, cx| {
                let hint = this.drop_hint.take();
                match hint {
                    Some(DropHint { target, drop: Drop::Center, .. }) => this.move_tab(drag.pane, target, None, None, cx),
                    Some(DropHint { target, drop: Drop::Side(side), .. }) => this.move_tab(drag.pane, target, Some(side), None, cx),
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
            .child(self.render_node(&self.tree.root, window, cx))
            .when_some(edge, |this, hint| this.child(drop_overlay(hint.drop, cx)))
            .into_any_element()
    }

    fn edge_hint(&mut self, bounds: Bounds<Pixels>, at: Point<Pixels>, cx: &mut Context<Self>) {
        let hint = edge_side(bounds, at).map(|side| DropHint {
            target: self.tree.root.id(),
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
            .filter(|hint| !hint.edge && hint.target == node)
            .map(|hint| hint.drop)
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

        if id == self.tree.root.id() {
            // The root holds every group and draws no frame.
            return body.into_any_element();
        }

        let theme = cx.theme();
        let current = self.defaults.own(id, true);
        let count = self.tree.groups_under(id).len();
        // The same tray whether or not the container is a default: its
        // default button says so.
        let tray = theme.muted_foreground.opacity(0.10);
        let layout = if horizontal { "side by side" } else { "stacked" };
        let hint = self.zone_overlay(id);

        v_flex()
            .id(("container", id))
            .relative()
            .size_full()
            .rounded(px(6.))
            .bg(tray)
            .px(px(GUTTER))
            .pb(px(GUTTER))
            .child(
                h_flex()
                    .id(("container-header", id))
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
                    .child(
                        h_flex()
                            .id(("container-grip", id))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .gap_1()
                            .pl_1()
                            .cursor_grab()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .on_drag(GroupDrag { node: id }, |_, _, _, cx| cx.new(|_| DragLabel("Container".into())))
                            .child(Icon::new(IconName::GripHorizontal).xsmall())
                            .child(div().truncate().child(match current {
                                Some(kind) => format!("{count} groups, {layout} · {}", kind.label()),
                                None => format!("{count} groups, {layout}"),
                            })),
                    )
                    .child(self.default_button(id, true, current, false, cx))
                    .children(self.container_actions(id, "container", true, cx))
                    .child(self.container_menu(id, cx)),
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
    /// kind set it shows that kind's icon on a tint.
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
            .map(|button| if current.is_some() { button.primary() } else { button.ghost() })
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

    fn render_tab_strip(&self, group: NodeId, tabs: &[PaneId], active: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let close_buttons = Settings::get(cx).tab_close_button;
        let buttons = Settings::get(cx).group_buttons;
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
                        .child(
                            h_flex()
                                .gap_1p5()
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
            .child(self.default_button(group, false, current, true, cx))
            // The terminal presets, as den's preset buttons; a group that is the
            // default of another kind does not get the buttons for this one.
            .when(buttons.shell && self.defaults.allows(&self.tree, group, Kind::Terminals), |this| this.child(
                Button::new(("group-shell", group))
                    .small()
                    .ghost()
                    .icon(Icon::new(IconName::SquareTerminal))
                    .tooltip("New Terminal (Ctrl+Shift+T)")
                    .on_click(cx.listener(move |this, _, window, cx| this.open_terminal(Some(group), None, window, cx))),
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
                let kind = if preset.browser { Kind::Browsers } else if preset.agent { Kind::Agents } else { Kind::Terminals };
                preset.pinned && self.defaults.allows(&self.tree, group, kind)
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
                                    this.open_terminal(Some(group), Some(launch.clone()), window, cx);
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
            .children(tab_elements)
            .last_empty_space(
                div()
                    .id("tab-bar-empty-space")
                    .h_full()
                    .flex_grow_1()
                    .min_w_16()
                    // Double-clicking the empty strip opens another tab of the kind
                    // the group shows, as in den.
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            this.open_more(group, window, cx);
                        }
                    }))
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
        div()
            .id(("group-grip", group))
            .w_full()
            .flex_none()
            .cursor_grab()
            .on_drag(GroupDrag { node: group }, |_, _, _, cx| cx.new(|_| DragLabel("Group".into())))
            .child(strip)
            .into_any_element()
    }

    /// The container's ⋮ menu.
    fn container_menu(&self, node: NodeId, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
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
                    .item(item("Close Container", |this, node, window, cx| this.request_close_container(node, window, cx)))
            })
    }

    /// The group's ⋮ menu.
    fn group_menu(&self, group: NodeId, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
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
                    .item(item("Close Group", |this, group, window, cx| this.request_close_group(group, window, cx)));
                group_buttons_menu(menu, cx)
            })
    }
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
