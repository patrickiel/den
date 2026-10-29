//! A window's workspace tabs: several folders open in one window, each a
//! `Workspace` of its own that keeps running (its terminals, agents, watcher
//! and git) while another is in front. Only the one in front is drawn. The
//! tabs sit in the middle of the title bar, as many as fit; the rest, and
//! the recent folders, are in the menu at their end.
//!
//! The frame is the window's root view and owns what is the window's rather
//! than a workspace's: closing it (asking about every tab's unsaved files),
//! which tab has the keyboard, and the window's title.

use std::{cell::RefCell, collections::HashMap, path::PathBuf, time::Duration};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    tooltip::Tooltip,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    layout_view::DragLabel,
    settings::{AppState, OpenWindow, Settings, WorkspaceColor},
    workspace::{self, Workspace},
};

/// A tab's height in the title bar.
const TAB_HEIGHT: f32 = 26.;
/// The most a label gets before it is cut short.
const LABEL_MAX: f32 = 200.;
const LABEL_SIZE: f32 = 13.;
/// A tab's room besides its label: its padding and outline, and the close
/// button when Settings show one.
const TAB_PADDING: f32 = 16. + 2.;
const CLOSE_ROOM: f32 = 20.;
const TAB_GAP: f32 = 4.;
/// The menu button at the end of the tabs, and the gap before it.
const MENU_ROOM: f32 = 28. + 4.;

pub struct Frame {
    /// The tabs, in their order, and each one's folder (kept here too: the
    /// workspace in front is being drawn when the tabs are, so it cannot be
    /// read then).
    workspaces: Vec<Entity<Workspace>>,
    roots: Vec<PathBuf>,
    /// The tab in front (out of range while none is).
    active: usize,
    /// Each label's width as drawn, measured once (until the font changes).
    label_widths: RefCell<HashMap<String, Pixels>>,
    /// The tabs the last paint had no room for, listed in the menu.
    overflowed: Vec<usize>,
    /// The window's title as last set.
    title: String,
    _subscriptions: Vec<Subscription>,
}

/// A workspace tab being dragged along the strip.
#[derive(Clone)]
struct WorkspaceTabDrag(EntityId);

/// The tab row as an element: it learns the width of the title bar's middle
/// as it is laid out and fits the tabs to it in the same frame, so a change
/// at either side of the bar (a button more or less) never shows a frame of
/// tabs fitted to the room there was before.
struct TabRow {
    frame: WeakEntity<Frame>,
    tabs: Vec<Tab>,
    widths: Vec<Pixels>,
    /// Still to build its row (once).
    build: bool,
}

impl IntoElement for TabRow {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TabRow {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if !std::mem::take(&mut self.build) {
            return None;
        }
        // The tabs sit on the window's middle, not the bar's: the sides are
        // not the same width. The room is what is left once they have moved.
        let window_mid = window.viewport_size().width / 2.;
        let shift = window_mid - bounds.center().x;
        let available = (bounds.size.width - shift.abs() * 2. - px(MENU_ROOM)).max(px(0.));
        let mut row = self
            .frame
            .update(cx, |frame, cx| {
                frame.render_row(&self.tabs, &self.widths, available, cx)
            })
            .ok()?;
        let size = row.layout_as_root(
            size(
                AvailableSpace::MinContent,
                AvailableSpace::Definite(bounds.size.height),
            ),
            window,
            cx,
        );
        let x = (window_mid - size.width / 2.)
            .min(bounds.right() - size.width)
            .max(bounds.left());
        let y = bounds.top() + (bounds.size.height - size.height) / 2.;
        row.prepaint_at(point(x, y), window, cx);
        Some(row)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        row: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(row) = row {
            row.paint(window, cx);
        }
    }
}

/// What one tab in the row shows.
#[derive(Clone)]
struct Tab {
    root: PathBuf,
    label: String,
    color: Option<Hsla>,
    /// A program in it wanted the user.
    attention: bool,
    close_button: bool,
}

impl Frame {
    /// A window on `roots`, each a tab, with the one at `active` in front.
    pub fn new(
        roots: Vec<PathBuf>,
        active: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Closing the window asks first about unsaved changes in any tab,
        // then saves every session.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak.upgrade().is_none_or(|this| {
                this.update(cx, |this, cx| this.request_close_window(window, cx))
            })
        });
        let _subscriptions = vec![
            // Coming back to the window: it is the one new tabs go to, and
            // the tab in front catches up (git may have changed underneath).
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    Frames::bump(window.window_handle(), cx);
                    if let Some(workspace) = this.active_workspace() {
                        workspace.update(cx, |workspace, cx| workspace.came_forward(cx));
                    }
                }
            }),
            // The recent folders decide how much of each path the tabs show.
            cx.observe_global::<AppState>(|_, cx| cx.notify()),
            cx.observe_global::<Settings>(|this, cx| {
                this.label_widths.borrow_mut().clear();
                cx.notify();
            }),
        ];
        let mut this = Self {
            workspaces: Vec::new(),
            roots: Vec::new(),
            active: 0,
            label_widths: RefCell::default(),
            overflowed: Vec::new(),
            title: String::new(),
            _subscriptions,
        };
        for root in roots {
            this.push(root, window, cx);
        }
        let active = active.min(this.workspaces.len().saturating_sub(1));
        this.active = this.workspaces.len();
        this.activate(active, window, cx);
        this
    }

    /// The frame that is `window`'s root view, if a frame is.
    pub fn of(window: &Window, cx: &App) -> Option<Entity<Self>> {
        let view = window
            .root::<gpui_kit::base::Root>()
            .flatten()?
            .read(cx)
            .view()
            .clone();
        view.downcast::<Self>().ok()
    }

    /// The tab in front.
    pub fn active_workspace(&self) -> Option<Entity<Workspace>> {
        self.workspaces.get(self.active).cloned()
    }

    fn index_of(&self, id: EntityId) -> Option<usize> {
        self.workspaces.iter().position(|w| w.entity_id() == id)
    }

    /// A new tab on `root`, hidden until it is activated.
    fn push(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        crate::extensions::broadcast(
            den_extension::events::WORKSPACE_OPENED,
            serde_json::json!({ "root": root }),
            cx,
        );
        let frame = cx.weak_entity();
        let workspace = cx.new(|cx| Workspace::new(root.clone(), frame, window, cx));
        crate::extensions::window_opened(
            root.clone(),
            workspace.downgrade(),
            window.window_handle(),
            cx,
        );
        self.workspaces.push(workspace);
        self.roots.push(root);
    }

    /// A tab on `root`, in front.
    pub fn add(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.push(root, window, cx);
        self.activate(self.workspaces.len() - 1, window, cx);
    }

    /// Bring tab `ix` to the front: the one there goes on running hidden,
    /// and the keyboard goes to this one's active pane.
    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix == self.active {
            return;
        }
        let Some(entering) = self.workspaces.get(ix).cloned() else {
            return;
        };
        if let Some(leaving) = self.active_workspace() {
            let held = leaving.update(cx, |workspace, cx| {
                // Mid-drag, the strip stays as it is until the drop.
                if workspace.is_dragging() {
                    return true;
                }
                workspace.leave_full_screen(window, cx);
                workspace.set_hidden(true, cx);
                false
            });
            if held {
                return;
            }
        }
        self.active = ix;
        entering.update(cx, |workspace, cx| {
            workspace.set_hidden(false, cx);
            workspace.came_forward(cx);
        });
        // Set now, not deferred: the next draw then finds something focused
        // and no input is told it lost the focus for nothing.
        let focus = entering.read(cx).action_target(cx);
        focus.focus(window, cx);
        self.note(window, cx);
        cx.notify();
    }

    pub(crate) fn activate_entity(
        &mut self,
        id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.index_of(id) {
            self.activate(ix, window, cx);
        }
    }

    fn cycle(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.workspaces.len() as isize;
        if len == 0 {
            return;
        }
        let ix = (self.active as isize + by).rem_euclid(len) as usize;
        self.activate(ix, window, cx);
    }

    /// Bring the tab at `n` (counted from 1) to the front.
    fn open_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if n >= 1 {
            self.activate(n - 1, window, cx);
        }
    }

    /// Close tab `ix`, asking first about its unsaved files; with `reopen`,
    /// its folder then opens in a window of its own.
    fn close(
        &mut self,
        ix: usize,
        reopen: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspaces.get(ix).cloned() else {
            return;
        };
        // Out of this update: closing comes back to the frame.
        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.request_close_tab(reopen, window, cx)
            });
        });
    }

    fn close_entity(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.index_of(id) {
            self.close(ix, None, window, cx);
        }
    }

    fn close_others(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let others: Vec<usize> = (0..self.workspaces.len())
            .filter(|&ix| self.workspaces[ix].entity_id() != id)
            .collect();
        for ix in others {
            self.close(ix, None, window, cx);
        }
    }

    /// Close tab `id` and open its folder in a window of its own. A
    /// workspace is bound to the window it was made in, so it is made anew
    /// there: its terminals start again, and its agents resume.
    fn move_to_new_window(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else {
            return;
        };
        if self.workspaces.len() == 1 {
            return;
        }
        let root = self.roots[ix].clone();
        self.close(ix, Some(root), window, cx);
    }

    /// Tab `id` closed, its session saved: the next one comes to the front,
    /// or the window goes with its last tab. With `reopen`, the folder opens
    /// in a new window once this workspace is gone (a frame after: its lock
    /// on the folder goes with it).
    pub(crate) fn remove(
        &mut self,
        id: EntityId,
        reopen: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.index_of(id) else {
            return;
        };
        let removed = self.workspaces.remove(ix);
        self.roots.remove(ix);
        if let Some(root) = reopen {
            cx.observe_release(&removed, move |_, _, cx| {
                cx.defer(move |cx| crate::open_in_new_window(vec![root], 0, cx));
            })
            .detach();
        }
        drop(removed);
        if self.workspaces.is_empty() {
            Frames::forget(window.window_handle(), cx);
            window.remove_window();
            return;
        }
        if ix < self.active {
            self.active -= 1;
        } else if ix == self.active {
            self.active = self.workspaces.len();
            self.activate(ix.min(self.workspaces.len() - 1), window, cx);
            return;
        }
        self.note(window, cx);
        cx.notify();
    }

    /// Tab `from` dropped on tab `to`: it takes that place.
    fn reorder(
        &mut self,
        from: EntityId,
        to: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(from), Some(to)) = (self.index_of(from), self.index_of(to)) else {
            return;
        };
        if from == to {
            return;
        }
        let active = self.active_workspace().map(|w| w.entity_id());
        let workspace = self.workspaces.remove(from);
        self.workspaces.insert(to, workspace);
        let root = self.roots.remove(from);
        self.roots.insert(to, root);
        if let Some(active) = active.and_then(|id| self.index_of(id)) {
            self.active = active;
        }
        self.note(window, cx);
        cx.notify();
    }

    /// Close the window: at once, saving every session, when nothing is
    /// unsaved; else after asking (false then: the window stays for now).
    pub fn request_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let dirty: Vec<SharedString> = self
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.read(cx).dirty_labels(cx))
            .collect();
        if dirty.is_empty() {
            self.close_window(window, cx);
            return true;
        }
        let what = workspace::discard_question(&dirty);
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title("Unsaved Changes")
                .description(what.clone())
                .ok_text("Discard")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    _ = this.update(cx, |this, cx| this.close_window(window, cx));
                    true
                })
        });
        false
    }

    fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for workspace in &self.workspaces {
            workspace.update(cx, |workspace, cx| workspace.save_session(cx));
        }
        Frames::closing(window.window_handle(), cx);
        window.remove_window();
    }

    /// Note the tabs as they are now, for the next start.
    fn note(&self, window: &Window, cx: &mut App) {
        Frames::note(
            window.window_handle(),
            OpenWindow {
                roots: self.roots.clone(),
                active: self.active,
            },
            cx,
        );
    }

    /// The window's title names the tab in front.
    fn sync_title(&mut self, window: &mut Window, cx: &App) {
        let Some(root) = self.roots.get(self.active) else {
            return;
        };
        let title = workspace::window_title(root, cx);
        if self.title != title {
            window.set_window_title(&title);
            self.title = title;
        }
    }

    // -- The tabs --------------------------------------------------------------

    /// The middle of the title bar: the tabs that fit, centred on the
    /// window, and the menu with the rest and the recent folders. Fills the
    /// room the title bar gives it, and notes that room for the next paint.
    /// Drawn by the workspace in front, which says whether a tab of its own
    /// wants attention (it cannot be read meanwhile).
    pub(crate) fn render_tabs(
        &mut self,
        active_attention: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let roots = self.roots.clone();
        let attention: Vec<bool> = (0..roots.len())
            .map(|ix| {
                if ix == self.active {
                    active_attention
                } else {
                    self.workspaces[ix].read(cx).has_attention()
                }
            })
            .collect();
        let (labels, _) = names(&roots, cx);
        let dark = cx.theme().is_dark();
        let colors: Vec<Option<Hsla>> = roots
            .iter()
            .map(|root| {
                AppState::get(cx)
                    .workspace_color(root)
                    .map(|color| color.foreground(dark))
            })
            .collect();
        let font = Font {
            family: cx.theme().font_family.clone(),
            ..Font::default()
        };
        // As the panes' tabs: a close button on each when Settings say so
        // (a middle-click closes either way).
        let close_buttons = Settings::get(cx).tab_close_button;
        let bold = Font {
            weight: FontWeight::BOLD,
            ..font.clone()
        };
        let widths: Vec<Pixels> = labels
            .iter()
            .enumerate()
            .map(|(ix, label)| {
                let font = if ix == self.active { &bold } else { &font };
                let text = self.label_width(label, font, window).min(px(LABEL_MAX));
                text + px(TAB_PADDING) + px(if close_buttons { CLOSE_ROOM } else { 0. })
            })
            .collect();
        let frame = cx.weak_entity();
        let tabs = roots
            .into_iter()
            .zip(labels)
            .zip(colors)
            .zip(attention)
            .map(|(((root, label), color), attention)| Tab {
                root,
                label,
                color,
                attention,
                close_button: close_buttons,
            })
            .collect();
        TabRow {
            frame,
            tabs,
            widths,
            build: true,
        }
        .into_any_element()
    }

    /// The row of the tabs that fit `available`, and the menu button.
    fn render_row(
        &mut self,
        tabs: &[Tab],
        widths: &[Pixels],
        available: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = fit(widths, self.active, available);
        self.overflowed = (0..tabs.len()).filter(|ix| !shown.contains(ix)).collect();
        let overflow = self.overflowed.len();
        let frame = cx.weak_entity();
        let menu_button = Button::new("workspaces")
            .ghost()
            .small()
            .map(|button| {
                if overflow == 0 {
                    button.icon(Icon::new(IconName::ChevronDown))
                } else {
                    button.label(format!("+{overflow}")).dropdown_caret(true)
                }
            })
            .tooltip(match crate::keymap::keys_for(&crate::OpenFolder, cx) {
                Some(keys) => format!("Workspaces ({keys} opens a folder)"),
                None => "Workspaces".to_string(),
            })
            .dropdown_menu(move |menu, _, cx| tabs_menu(frame.clone(), None, menu, cx));
        let mut elements = Vec::with_capacity(shown.len());
        for &ix in &shown {
            elements.push(self.render_tab(ix, tabs[ix].clone(), cx));
        }
        h_flex()
            .flex_none()
            .gap(px(TAB_GAP))
            // Not a place to drag the window from.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(elements)
            .child(menu_button)
            .into_any_element()
    }

    fn label_width(&self, label: &str, font: &Font, window: &Window) -> Pixels {
        let key = format!("{label}\u{0}{:?}", font.weight);
        if let Some(width) = self.label_widths.borrow().get(&key) {
            return *width;
        }
        let mut seen = HashMap::new();
        let width = crate::ui::text_width(
            window.text_system(),
            &mut seen,
            label,
            font.clone(),
            px(LABEL_SIZE),
            false,
        );
        self.label_widths.borrow_mut().insert(key, width);
        width
    }

    fn render_tab(&self, ix: usize, tab: Tab, cx: &mut Context<Self>) -> AnyElement {
        let Tab {
            root,
            label,
            color,
            attention,
            close_button,
        } = tab;
        let id = self.workspaces[ix].entity_id();
        let active = ix == self.active;
        let theme = cx.theme();
        let (radius, tab_active_fg, tab_fg, hover, warning) = (
            theme.radius,
            theme.tab_active_foreground,
            theme.tab_foreground,
            theme.secondary_hover,
            theme.warning,
        );
        let label = SharedString::from(label);
        let full = SharedString::from(root.display().to_string());
        let group = SharedString::from(format!("workspace-tab-{ix}"));
        let frame = cx.weak_entity();
        let drag_label = label.clone();
        let menu_root = root.clone();
        let tab = div()
            .id(("workspace-tab", ix))
            .group(group.clone())
            .flex_none()
            .h(px(TAB_HEIGHT))
            .px_2()
            .rounded(radius)
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(px(LABEL_SIZE))
            .cursor_default()
            .border_1()
            .border_color(transparent_black())
            // In its colour, when it has one: filled with it in front, named
            // in it behind.
            .map(|tab| match (active, color) {
                (true, Some(color)) => tab.bg(color).text_color(on(color)),
                // The theme's active-tab colour is near the title bar's:
                // a tint of the text colour stands out on it in both modes.
                (true, None) => tab
                    .bg(tab_active_fg.opacity(0.18))
                    .text_color(tab_active_fg),
                (false, Some(color)) => tab
                    .text_color(color.opacity(0.85))
                    .hover(move |tab| tab.bg(color.opacity(0.12))),
                (false, None) => tab.text_color(tab_fg).hover(move |tab| tab.bg(hover)),
            })
            .child(
                div()
                    .max_w(px(LABEL_MAX))
                    .truncate()
                    .when(active, |name| name.font_weight(FontWeight::BOLD))
                    .child(label.clone()),
            )
            .when(close_button, |tab| {
                tab.child(
                    Button::new(("close-workspace", ix))
                        .icon(IconName::X)
                        .xsmall()
                        .ghost()
                        .mr(-px(4.))
                        .tab_stop(false)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_entity(id, window, cx);
                        })),
                )
            })
            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.activate_entity(id, window, cx)))
            // Middle-click closes, as the panes' tabs do.
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_entity(id, window, cx);
                }),
            )
            .on_drag(WorkspaceTabDrag(id), move |_, _, _, cx| {
                cx.new(|_| DragLabel(drag_label.clone()))
            })
            .drag_over::<WorkspaceTabDrag>(|this, _, _, cx| {
                this.border_l_2().border_color(cx.theme().drag_border)
            })
            .on_drop(
                cx.listener(move |this, drag: &WorkspaceTabDrag, window, cx| {
                    this.reorder(drag.0, id, window, cx)
                }),
            )
            .context_menu(move |menu, window, cx| {
                tab_menu(frame.clone(), id, menu_root.clone(), menu, window, cx)
            });
        if !attention || active {
            return tab.into_any_element();
        }
        // A program in it wanted the user while they looked elsewhere: the
        // tab breathes in its colour until they do.
        let pulse = color.unwrap_or(warning);
        tab.with_animation(
            ("workspace-tab-pulse", ix),
            Animation::new(Duration::from_millis(1600))
                .repeat()
                .with_easing(pulsating_between(0.08, 0.5)),
            move |tab, alpha| tab.bg(pulse.opacity(alpha)),
        )
        .into_any_element()
    }
}

impl Render for Frame {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_title(window, cx);
        div()
            .id("frame")
            .size_full()
            .on_action(
                cx.listener(|this, _: &crate::NextWorkspace, window, cx| this.cycle(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::PrevWorkspace, window, cx| {
                    this.cycle(-1, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &crate::CloseWorkspace, window, cx| {
                this.close(this.active, None, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &crate::MoveWorkspaceToNewWindow, window, cx| {
                    if let Some(workspace) = this.active_workspace() {
                        this.move_to_new_window(workspace.entity_id(), window, cx)
                    }
                }),
            )
            .on_action(
                cx.listener(|this, action: &crate::OpenWorkspace, window, cx| {
                    this.open_nth(action.0, window, cx)
                }),
            )
            .children(self.active_workspace())
    }
}

/// Text that reads on a fill of `color`: near black on a light colour
/// (yellow), near white on a dark one.
fn on(color: Hsla) -> Hsla {
    let rgb = color.to_rgb();
    let channel = |c: f32| {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b);
    if luminance > 0.4 {
        gpui_kit::hsla(0., 0., 0.1, 1.)
    } else {
        gpui_kit::hsla(0., 0., 0.98, 1.)
    }
}

/// Which tabs fit in `available`: the active one, then its neighbours on
/// either side in turn, as far as they go without a gap in the strip.
fn fit(widths: &[Pixels], active: usize, available: Pixels) -> Vec<usize> {
    let Some(&first) = widths.get(active) else {
        return Vec::new();
    };
    let gap = px(TAB_GAP);
    let mut shown = vec![active];
    let mut used = first;
    let (mut left, mut right) = (active, active + 1);
    let (mut left_done, mut right_done) = (left == 0, right >= widths.len());
    while !(left_done && right_done) {
        if !right_done {
            if used + gap + widths[right] <= available {
                used += gap + widths[right];
                shown.push(right);
                right += 1;
                right_done = right >= widths.len();
            } else {
                right_done = true;
            }
        }
        if !left_done {
            let next = left - 1;
            if used + gap + widths[next] <= available {
                used += gap + widths[next];
                shown.insert(0, next);
                left = next;
                left_done = left == 0;
            } else {
                left_done = true;
            }
        }
    }
    shown
}

/// The open folders' names, each with as many parents as it takes to tell
/// it from the others and from the recent folders; and the recent folders
/// not open, with theirs.
fn names(roots: &[PathBuf], cx: &App) -> (Vec<String>, Vec<(PathBuf, String)>) {
    let keys: Vec<String> = roots.iter().map(|p| crate::repo::key(p)).collect();
    let recent: Vec<PathBuf> = AppState::get(cx)
        .recent
        .iter()
        .filter(|p| !keys.contains(&crate::repo::key(p)))
        .cloned()
        .collect();
    let all: Vec<PathBuf> = roots.iter().chain(recent.iter()).cloned().collect();
    let mut names = crate::settings::unique_names(&all);
    let rest = names.split_off(roots.len());
    (names, recent.into_iter().zip(rest).collect())
}

/// A tab's right-click menu.
fn tab_menu(
    frame: WeakEntity<Frame>,
    id: EntityId,
    root: PathBuf,
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    type Run = fn(&mut Frame, EntityId, &mut Window, &mut Context<Frame>);
    let item = |label: &str, run: Run| {
        let frame = frame.clone();
        PopupMenuItem::new(SharedString::from(label.to_string())).on_click(move |_, window, cx| {
            _ = frame.update(cx, |frame, cx| run(frame, id, window, cx));
        })
    };
    let color_root = root.clone();
    menu.item(item("Close", |frame, id, window, cx| {
        frame.close_entity(id, window, cx)
    }))
    .item(item("Close Others", |frame, id, window, cx| {
        frame.close_others(id, window, cx)
    }))
    .item(item("Move to New Window", |frame, id, window, cx| {
        frame.move_to_new_window(id, window, cx)
    }))
    .separator()
    .submenu("Workspace Color", window, cx, move |menu, _, cx| {
        let selected = AppState::get(cx).workspace_color(&color_root);
        std::iter::once(None)
            .chain(WorkspaceColor::ALL.map(Some))
            .fold(menu, |menu, color| {
                let root = color_root.clone();
                menu.item(
                    PopupMenuItem::new(color.map_or("Default", WorkspaceColor::name))
                        .checked(color == selected)
                        .on_click(move |_, window, cx| {
                            AppState::update(cx, |state| state.session_mut(&root).color = color);
                            window.refresh();
                        }),
                )
            })
    })
    .separator()
    .item(
        PopupMenuItem::new("Copy Path")
            .icon(Icon::new(IconName::Copy))
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(root.display().to_string()));
            }),
    )
}

/// The menu at the end of the tabs: the open folders the strip had no
/// room for, the recent folders (each with its colour, and an x to forget
/// it), and opening another.
fn tabs_menu(
    frame: WeakEntity<Frame>,
    color_for: Option<PathBuf>,
    menu: PopupMenu,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(strong) = frame.upgrade() else {
        return menu;
    };
    let (open, overflowed) = {
        let frame = strong.read(cx);
        let open: Vec<(EntityId, PathBuf, bool)> = frame
            .workspaces
            .iter()
            .zip(&frame.roots)
            .map(|(workspace, root)| {
                (
                    workspace.entity_id(),
                    root.clone(),
                    workspace.read(cx).has_attention(),
                )
            })
            .collect();
        (open, frame.overflowed.clone())
    };
    let roots: Vec<PathBuf> = open.iter().map(|(_, root, _)| root.clone()).collect();
    let (labels, recent) = names(&roots, cx);
    let dark = cx.theme().is_dark();
    let own = cx.weak_entity();
    let mut menu = menu.max_h(px(480.)).scrollable(true);
    if !overflowed.is_empty() {
        menu = menu.label("OPEN");
        for ix in overflowed {
            let (id, root, attention) = open[ix].clone();
            let label = SharedString::from(labels[ix].clone());
            let color = AppState::get(cx)
                .workspace_color(&root)
                .map(|color| color.foreground(dark));
            let frame = frame.clone();
            let item = PopupMenuItem::element(move |_, cx| {
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when_some(color, |this, color| this.text_color(color))
                            .child(label.clone()),
                    )
                    .when(attention, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .size(px(7.))
                                .rounded_full()
                                .bg(cx.theme().warning),
                        )
                    })
            });
            menu = menu.item(item.icon(Icon::new(IconName::AppWindow)).on_click(
                move |_, window, cx| {
                    _ = frame.update(cx, |frame, cx| frame.activate_entity(id, window, cx));
                },
            ));
        }
        menu = menu.separator();
    }
    menu = menu.label("RECENT FOLDERS");
    if recent.is_empty() {
        menu = menu.item(PopupMenuItem::new("No other folders yet").disabled(true));
    }
    for (ix, (path, label)) in recent.into_iter().enumerate() {
        let (label, full) = (
            SharedString::from(label),
            SharedString::from(path.display().to_string()),
        );
        let picking_color = color_for.as_ref() == Some(&path);
        let rebuild = (frame.clone(), own.clone(), path.clone());
        let item = PopupMenuItem::element(move |_, cx| {
            let full = full.clone();
            let (frame, own, path) = rebuild.clone();
            let color = AppState::get(cx)
                .workspace_color(&path)
                .map(|color| color.foreground(cx.theme().is_dark()));
            let (color_frame, color_own, color_path) = (frame.clone(), own.clone(), path.clone());
            h_flex()
                .id(("recent", ix))
                .group(SharedString::from(format!("recent-{ix}")))
                .w_full()
                .gap_2()
                .child(
                    div()
                        .id(("recent-name", ix))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when_some(color, |this, color| this.text_color(color))
                        .child(label.clone())
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx)),
                )
                .child(
                    h_flex()
                        .id(("color-recent", ix))
                        .flex_none()
                        .size(px(16.))
                        .justify_center()
                        .when(!picking_color, |this| {
                            this.invisible()
                                .group_hover(SharedString::from(format!("recent-{ix}")), |this| {
                                    this.visible()
                                })
                        })
                        .rounded(px(3.))
                        .cursor_pointer()
                        .text_color(color.unwrap_or(cx.theme().muted_foreground))
                        .hover(|this| this.bg(gpui_kit::hsla(0., 0., 0.5, 0.25)))
                        .child(Icon::new(IconName::Palette).xsmall())
                        .tooltip(|window, cx| Tooltip::new("Workspace color").build(window, cx))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let color_for = (!picking_color).then(|| color_path.clone());
                            let frame = color_frame.clone();
                            _ = color_own.update(cx, |menu, cx| {
                                menu.rebuild(window, cx, |menu, _, cx| {
                                    tabs_menu(frame, color_for, menu, cx)
                                })
                            });
                        }),
                )
                // The x takes the folder off the list (its saved session stays).
                .child(
                    div()
                        .id(("forget-recent", ix))
                        .flex_none()
                        .invisible()
                        .group_hover(SharedString::from(format!("recent-{ix}")), |this| {
                            this.visible()
                        })
                        .rounded(px(3.))
                        .hover(|this| this.bg(gpui_kit::hsla(0., 0., 0.5, 0.25)))
                        .child(Icon::new(IconName::X).xsmall())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            AppState::update(cx, |state| {
                                let key = crate::repo::key(&path);
                                state.recent.retain(|p| crate::repo::key(p) != key);
                            });
                            workspace::sync_jump_list(cx);
                            let frame = frame.clone();
                            _ = own.update(cx, |menu, cx| {
                                menu.rebuild(window, cx, |menu, _, cx| {
                                    tabs_menu(frame, None, menu, cx)
                                })
                            });
                            window.refresh();
                        }),
                )
        });
        let open_path = path.clone();
        // Opens the folder as a tab here.
        menu = menu.item(
            item.icon(Icon::new(IconName::Folder))
                .on_click(move |_, _, cx| crate::open_workspace(open_path.clone(), cx)),
        );
        if picking_color {
            let (frame, own) = (frame.clone(), own.clone());
            menu = menu.item(PopupMenuItem::element(move |_, cx| {
                color_picker(path.clone(), frame.clone(), own.clone(), cx)
            }));
        }
    }
    let prompt = |new_window: bool| {
        let frame = frame.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let workspace = frame
                .upgrade()
                .and_then(|frame| frame.read(cx).active_workspace());
            if let Some(workspace) = workspace {
                workspace.update(cx, |workspace, cx| {
                    workspace.prompt_open_folder(new_window, window, cx)
                });
            }
        }
    };
    menu.separator()
        .item(
            PopupMenuItem::new("Open Folder…")
                .icon(Icon::new(IconName::FolderOpen))
                .on_click(prompt(false)),
        )
        .item(
            PopupMenuItem::new("Open Folder in New Window…")
                .icon(Icon::new(IconName::AppWindow))
                .on_click(prompt(true)),
        )
}

/// A single row of distinct swatches under the folder being edited. Keeping
/// it inside the menu also keeps choosing a color from opening the folder.
fn color_picker(
    path: PathBuf,
    frame: WeakEntity<Frame>,
    menu: WeakEntity<PopupMenu>,
    cx: &App,
) -> AnyElement {
    let selected = AppState::get(cx).workspace_color(&path);
    let theme = cx.theme();
    let (dark, foreground, hover) = (theme.is_dark(), theme.foreground, theme.accent);
    h_flex()
        .id("workspace-color-picker")
        .w_full()
        .gap(px(2.))
        .py_1()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, _, cx| cx.stop_propagation())
        .children(
            std::iter::once(None)
                .chain(WorkspaceColor::ALL.map(Some))
                .enumerate()
                .map(move |(ix, color)| {
                    let (path, frame, menu) = (path.clone(), frame.clone(), menu.clone());
                    div()
                        .id(("workspace-color", ix))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(24.))
                        .p(px(3.))
                        .rounded_full()
                        .border_1()
                        .border_color(if color == selected {
                            foreground
                        } else {
                            transparent_black()
                        })
                        .cursor_pointer()
                        .hover(move |this| this.bg(hover))
                        .when_some(color, |this, color| {
                            this.child(div().size_full().rounded_full().bg(color.foreground(dark)))
                        })
                        .when(color.is_none(), |this| {
                            this.child(Icon::new(IconName::Ban).xsmall())
                        })
                        .tooltip(move |window, cx| {
                            Tooltip::new(color.map_or("Default color", WorkspaceColor::name))
                                .build(window, cx)
                        })
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            AppState::update(cx, |state| state.session_mut(&path).color = color);
                            let frame = frame.clone();
                            _ = menu.update(cx, |menu, cx| {
                                menu.rebuild(window, cx, |menu, _, cx| {
                                    tabs_menu(frame, None, menu, cx)
                                })
                            });
                            window.refresh();
                        })
                }),
        )
        .into_any_element()
}

// -- The windows ---------------------------------------------------------------

/// The frame windows, the one last in front first, each with its tabs as
/// last noted: for which window a folder opens in, and for the next start.
#[derive(Default)]
pub struct Frames {
    windows: Vec<(AnyWindowHandle, OpenWindow)>,
}

impl Global for Frames {}

impl Frames {
    pub fn init(cx: &mut App) {
        cx.set_global(Frames::default());
        cx.on_app_quit(|cx| {
            Frames::persist(cx);
            async {}
        })
        .detach();
    }

    /// The frame window last in front, among those still open.
    pub fn front(cx: &App) -> Option<AnyWindowHandle> {
        let live = cx.windows();
        cx.try_global::<Frames>()?
            .windows
            .iter()
            .map(|(window, _)| *window)
            .find(|window| live.contains(window))
    }

    fn note(window: AnyWindowHandle, open: OpenWindow, cx: &mut App) {
        let frames = cx.default_global::<Frames>();
        match frames.windows.iter_mut().find(|(w, _)| *w == window) {
            Some(entry) => entry.1 = open,
            None => frames.windows.insert(0, (window, open)),
        }
        Self::persist(cx);
    }

    fn bump(window: AnyWindowHandle, cx: &mut App) {
        let frames = cx.default_global::<Frames>();
        if let Some(ix) = frames.windows.iter().position(|(w, _)| *w == window)
            && ix != 0
        {
            let entry = frames.windows.remove(ix);
            frames.windows.insert(0, entry);
        }
    }

    fn forget(window: AnyWindowHandle, cx: &mut App) {
        cx.default_global::<Frames>()
            .windows
            .retain(|(w, _)| *w != window);
        Self::persist(cx);
    }

    /// A window closing by hand: forgotten while others stay open; the last
    /// one's tabs are kept, as it is den quitting (on Windows and Linux),
    /// and they come back with the next start.
    fn closing(window: AnyWindowHandle, cx: &mut App) {
        let others = cx
            .default_global::<Frames>()
            .windows
            .iter()
            .any(|(w, _)| *w != window);
        if others {
            Self::forget(window, cx);
        } else {
            Self::persist(cx);
        }
    }

    fn persist(cx: &mut App) {
        let open: Vec<OpenWindow> = cx
            .default_global::<Frames>()
            .windows
            .iter()
            .map(|(_, open)| open.clone())
            .collect();
        if AppState::get(cx).windows != open {
            AppState::update(cx, |state| state.windows = open);
        }
    }
}

/// The window a folder opens in as a tab: the one in front (the main window
/// of a floating one), else the frame window last in front.
pub fn target_window(cx: &mut App) -> Option<AnyWindowHandle> {
    if let Some(active) = cx.active_window() {
        let main = active
            .update(cx, |_, window, cx| {
                if Frame::of(window, cx).is_some() {
                    return Some(window.window_handle());
                }
                crate::float::FloatWindow::of(window, cx)
                    .and_then(|float| float.read(cx).main_window(cx))
            })
            .ok()
            .flatten();
        if let Some(main) = main {
            return Some(main);
        }
    }
    Frames::front(cx)
}
