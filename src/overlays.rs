//! Two things drawn over a window's body (the sidebar and the groups), under
//! its title bar: a maximized group, and the overview of every tab as tiles.
//!
//! Maximizing a group draws it over everything else in its window (over the
//! groups alone, leaving the sidebar, when Settings say so), a small space
//! in from the edges, as VS Code's maximized editor group. It
//! grows there from its place in the layout and shrinks back when restored.
//! Underneath, the layout stays as it is, with an empty slot where the group
//! came from; making another group of the window the active one (the keys,
//! a tab opening there) restores it. Maximized as tiles, the group shows
//! every one of its tabs at once, live, in a grid (`render_tiles`).
//!
//! The overview lays every tab of the session out as a card: one block per
//! group, the groups in the order Ctrl+1…8 count them, the cards in their
//! tabs' order, three to a row across the window. A card says what the tab
//! is: a terminal's last lines of output, a file's path, a page's address.
//! Clicking one shows that tab; Esc, a click on the backdrop or the button
//! closes the overview.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    browser::BrowserPanel,
    diff::DiffPanel,
    layout::{Node, NodeId, PaneId, Side},
    layout_view::Zone,
    panels::FilePanel,
    terminal::TerminalPanel,
    workspace::Workspace,
};

/// How long a group takes to grow to its maximized bounds, and to shrink back.
const GROW: Duration = Duration::from_millis(180);
/// How long the overview takes to fade in, and out.
const FADE: Duration = Duration::from_millis(150);
/// The space a maximized group leaves to the body's edges.
const SPACING: f32 = 8.;
/// The overview's cards to a row, and a card's height (its width is a third
/// of the window's).
const COLUMNS: usize = 3;
const CARD_HEIGHT: f32 = 240.;
/// How many of a terminal's last lines a card shows.
const TERMINAL_LINES: usize = 12;

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

/// Every tab as a tile, over one window's body.
pub(crate) struct Overview {
    /// The window it is shown in (`None` for the main one).
    pub float: Option<u64>,
    /// Takes the keyboard while it is open, for Esc.
    focus: FocusHandle,
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

/// The colour a kind of tab wears in the overview, from the theme's palette:
/// terminals green, agents magenta, files blue, diffs yellow, pages cyan;
/// the rest in the muted text colour.
fn kind_color(kind: &str, cx: &App) -> Hsla {
    let theme = cx.theme();
    match kind {
        "Agent" => theme.magenta,
        crate::terminal::TERMINAL => theme.green,
        crate::panels::FILE => theme.blue,
        crate::diff::DIFF => theme.yellow,
        crate::browser::BROWSER => theme.cyan,
        _ => theme.muted_foreground,
    }
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
    /// grid, as `render_tiles` lays it out; `None` at the grid's edge.
    pub(crate) fn tile_beside(&self, side: Side) -> Option<PaneId> {
        let group = self.tiled_group()?;
        let tabs = self.tree.tabs(group);
        let active = self.tree.active_tab(group).and_then(|pane| tabs.iter().position(|p| *p == pane))?;
        let columns = crate::layout_view::tile_columns(tabs.len());
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

    /// The transitions that ran their course: what they closed goes. Called
    /// as a window is drawn, before anything looks at the overlays.
    pub(crate) fn settle_overlays(&mut self) {
        if self.maximized.as_ref().is_some_and(|m| m.transition.done()) {
            self.maximized = None;
        }
        if self.overview.as_ref().is_some_and(|o| o.transition.done()) {
            self.overview = None;
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

    /// What lies over a window's body: the maximized group and the overview,
    /// when they are this window's (`None` for the main one).
    pub(crate) fn render_overlays(&self, float: Option<u64>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .child(mark(&self.painted, move |painted, bounds| {
                painted.body.insert(float, bounds);
            }))
            .children(self.render_maximized(float, window, cx))
            .children(self.render_overview(float, window, cx))
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
        let at = body.zip(region).map(|(body, region)| {
            let margin = px(SPACING);
            let full = Bounds::new(region.origin + point(margin, margin), size(region.size.width - margin * 2., region.size.height - margin * 2.));
            match m.from {
                Some(from) => between(Bounds::new(from.origin - body.origin, from.size), full, t),
                None => full,
            }
        });
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

    // -- The overview ------------------------------------------------------------

    /// Whether the overview is open in a window (`None` for the main one).
    pub(crate) fn overview_shown(&self, float: Option<u64>) -> bool {
        self.overview.as_ref().is_some_and(|o| o.float == float && !o.transition.closing)
    }

    /// Show every tab as tiles over `window`'s body, or close the overview
    /// when it is open there.
    pub(crate) fn toggle_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let float = self.float_of_window(window);
        if self.overview_shown(float) {
            self.close_overview(window, cx);
            return;
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.overview = Some(Overview { float, focus, transition: Transition::start(FADE) });
        cx.notify();
    }

    /// Fade the overview out; the keyboard goes back to the tab.
    pub(crate) fn close_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = &mut self.overview else { return };
        if o.transition.closing {
            return;
        }
        o.transition.reverse();
        let float = o.float;
        if let Some(focus) = self.window_focus(float, cx) {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn render_overview(&self, float: Option<u64>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let o = self.overview.as_ref().filter(|o| o.float == float)?;
        let t = o.transition.progress(window);
        let (background, muted) = (cx.theme().background, cx.theme().muted_foreground);
        let groups = self.tree.groups();
        let tabs: usize = groups.iter().map(|group| self.tree.tabs(*group).len()).sum();
        let count = format!(
            "{tabs} {} in {} {}",
            if tabs == 1 { "tab" } else { "tabs" },
            groups.len(),
            if groups.len() == 1 { "group" } else { "groups" }
        );
        let esc = crate::keymap::keys_for(&crate::ShowAllTabs, cx).map_or_else(|| "Esc to close".to_string(), |keys| format!("Esc or {keys} to close"));
        let header = h_flex()
            .flex_none()
            .items_baseline()
            .gap_3()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("All tabs"))
            .child(div().text_sm().text_color(muted).child(count))
            .child(div().flex_1())
            .child(div().text_xs().text_color(muted).child(esc))
            .child(
                Button::new("overview-close")
                    .small()
                    .ghost()
                    .icon(Icon::new(IconName::X))
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_overview(window, cx);
                    })),
            );
        let sections: Vec<AnyElement> = groups.iter().enumerate().map(|(ix, group)| self.overview_section(ix + 1, *group, cx)).collect();
        Some(
            div()
                .id("overview")
                .absolute()
                .inset_0()
                .occlude()
                .track_focus(&o.focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close_overview(window, cx);
                    }
                }))
                // The backdrop closes it; the tiles keep their clicks.
                .on_click(cx.listener(|this, _, window, cx| this.close_overview(window, cx)))
                .bg(background)
                .opacity(t)
                .child(
                    v_flex()
                        .relative()
                        // Settles down as it fades in, lifts off as it goes.
                        .top(px(12. * (1. - t)))
                        .size_full()
                        .px_6()
                        .pt_4()
                        .gap_4()
                        .child(header)
                        .child(
                            v_flex()
                                .id("overview-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .pb_6()
                                .gap_5()
                                .children(sections),
                        ),
                )
                .into_any_element(),
        )
    }

    /// A group's block: its number and name over its tabs' cards, three to
    /// a row, a short last row keeping its cards' width.
    fn overview_section(&self, n: usize, group: NodeId, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let tabs = self.tree.tabs(group).to_vec();
        let shown = self.tree.active_tab(group);
        let active = group == self.active_group;
        let mut notes: Vec<String> = Vec::new();
        if let Some(kind) = self.defaults.get(group) {
            notes.push(format!("default for {}", kind.label()));
        }
        if self.tree.float_of(group).is_some() {
            notes.push("floating window".to_string());
        }
        notes.push(match tabs.len() {
            0 => "empty".to_string(),
            1 => "1 tab".to_string(),
            n => format!("{n} tabs"),
        });
        let badge = h_flex()
            .flex_none()
            .min_w(px(22.))
            .h(px(22.))
            .px_1p5()
            .justify_center()
            .rounded(px(5.))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .map(|this| if active { this.bg(theme.primary).text_color(theme.primary_foreground) } else { this.bg(theme.secondary).text_color(theme.foreground) })
            .child(n.to_string());
        let header = h_flex()
            .items_center()
            .gap_2()
            .child(badge)
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(format!("Group {n}")))
            .child(div().text_xs().text_color(theme.muted_foreground).child(notes.join(" · ")));
        let row = |cards: Vec<AnyElement>| {
            let short = COLUMNS.saturating_sub(cards.len());
            h_flex().gap_3().children(cards).children((0..short).map(|_| div().flex_1()))
        };
        let rows: Vec<_> = if tabs.is_empty() {
            vec![row(vec![self.empty_card(group, cx)])]
        } else {
            tabs.chunks(COLUMNS).map(|chunk| row(chunk.iter().filter_map(|pane| self.card(*pane, shown == Some(*pane), active, cx)).collect())).collect()
        };
        v_flex().gap_2p5().child(header).child(v_flex().gap_3().children(rows)).into_any_element()
    }

    /// An empty group's card: a dashed box that takes you to the group.
    fn empty_card(&self, group: NodeId, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .id(("card-empty", group))
            .flex_1()
            .min_w_0()
            .h(px(CARD_HEIGHT))
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .border_1()
            .border_dashed()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .cursor_pointer()
            .hover(|this| this.bg(theme.secondary_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.set_active_group(group, cx);
                this.close_overview(window, cx);
            }))
            .child("Empty group")
            .into_any_element()
    }

    /// A tab's card: its icon and name, what it holds, and its kind.
    fn card(&self, pane_id: PaneId, shown: bool, active_group: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pane = self.panes.get(&pane_id)?;
        let theme = cx.theme();
        let label = pane.label(cx);
        let dirty = pane.is_dirty(cx);
        let attention = self.attention.contains(&pane_id);
        let icon = pane.icon_element(cx).unwrap_or_else(|| Icon::new(pane.icon(cx)).small().into_any_element());
        let (kind, detail, lines) = self.card_details(pane_id, pane.kind(cx), cx);
        // Each kind of tab has a colour: a strip along the card's top and
        // the kind's name wear it, so the kinds tell apart at a glance.
        let accent = kind_color(&kind, cx);
        // The tab each group shows is ringed; the active group's in full.
        let border = if shown && active_group {
            theme.primary
        } else if shown {
            theme.primary.alpha(0.45)
        } else {
            theme.border
        };
        let muted = theme.muted_foreground;
        let hover = theme.secondary_hover;
        let head = h_flex()
            .flex_none()
            .px_3()
            .pt_2p5()
            .gap_2()
            .items_center()
            .child(div().flex_none().child(icon))
            .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).when(self.preview == Some(pane_id), |this| this.italic()).child(label))
            .when(dirty, |this| this.child(div().flex_none().text_xs().child("●")))
            .when(attention, |this| this.child(div().flex_none().size(px(7.)).rounded_full().bg(theme.warning)))
            .child(
                Button::new(("card-close", pane_id))
                    .icon(IconName::X)
                    .xsmall()
                    .ghost()
                    .tab_stop(false)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.request_close_pane(pane_id, window, cx);
                    })),
            );
        let body = div().flex_1().min_h_0().px_3().pt_1().overflow_hidden().text_xs().text_color(muted).map(|this| match lines {
            // A terminal's last lines, as they stand on its screen.
            Some(lines) => this.font_family(crate::settings::mono_font(cx)).line_height(px(15.)).children(lines.into_iter().map(|line| div().truncate().child(line))),
            None => this.when_some(detail, |this, detail| this.child(div().truncate().child(detail))),
        });
        let foot = h_flex()
            .flex_none()
            .px_3()
            .pb_2()
            .pt_1()
            .items_center()
            .justify_between()
            .text_xs()
            .text_color(muted)
            .child(div().text_color(accent).child(kind))
            .when(shown, |this| {
                this.child(div().px_1p5().rounded(px(4.)).bg(theme.primary.alpha(0.15)).text_color(theme.primary).child("shown"))
            });
        Some(
            v_flex()
                .id(("card", pane_id))
                .flex_1()
                .min_w_0()
                .h(px(CARD_HEIGHT))
                .rounded(px(8.))
                .border_1()
                .border_color(border)
                .bg(theme.tab_bar)
                .overflow_hidden()
                .cursor_pointer()
                .hover(move |this| this.bg(hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.show_pane(pane_id, true, window, cx);
                    this.close_overview(window, cx);
                }))
                // Middle-click closes, as on a tab.
                .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.request_close_pane(pane_id, window, cx);
                }))
                .child(div().flex_none().w_full().h(px(2.)).bg(accent))
                .child(head)
                .child(body)
                .child(foot)
                .into_any_element(),
        )
    }

    /// What a card says of a tab under its name: its kind, a line about it
    /// (a path, an address, a terminal's program and folder), and for a
    /// terminal its last lines of output.
    fn card_details(&self, pane: PaneId, kind: &'static str, cx: &App) -> (String, Option<String>, Option<Vec<String>>) {
        if let Some(file) = self.pane_as::<FilePanel>(pane) {
            return (kind.to_string(), Some(self.relative(file.read(cx).path())), None);
        }
        if let Some(diff) = self.pane_as::<DiffPanel>(pane) {
            return (kind.to_string(), Some(self.relative(diff.read(cx).path())), None);
        }
        if let Some(browser) = self.pane_as::<BrowserPanel>(pane) {
            return (kind.to_string(), Some(browser.read(cx).url().to_string()), None);
        }
        if let Some(terminal) = self.pane_as::<TerminalPanel>(pane) {
            let terminal = terminal.read(cx);
            let kind = if terminal.is_agent() { "Agent" } else { kind };
            let folder = self.relative(terminal.cwd());
            let detail = match terminal.program() {
                Some(program) => format!("{program} · {folder}"),
                None => folder,
            };
            let lines: Vec<String> = terminal.scrollback(TERMINAL_LINES).lines().map(|line| line.trim_end().to_string()).collect();
            let lines = (!lines.is_empty()).then_some(lines);
            return (kind.to_string(), Some(detail), lines);
        }
        (kind.to_string(), None, None)
    }

    /// `path` as the session sees it: inside the folder, from the folder
    /// (the folder's own name for the folder itself); elsewhere whole.
    fn relative(&self, path: &std::path::Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => self.root.file_name().map_or_else(|| self.root.display().to_string(), |name| name.to_string_lossy().to_string()),
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => path.display().to_string(),
        }
    }

    /// The float whose window `window` is; `None` for the main one.
    pub(crate) fn float_of_window(&self, window: &Window) -> Option<u64> {
        let handle = window.window_handle();
        self.float_windows.iter().find(|(_, w)| **w == handle).map(|(id, _)| *id)
    }
}
