//! The window: title bar, sidebar (Explorer / Search / Source Control) and
//! the groups.
//!
//! The workspace owns the layout tree (`layout.rs`), the panes in it and the
//! default groups (`defaults.rs`); `layout_view.rs` draws them. The whole
//! arrangement saves as a `LayoutState`: per session on every change, and
//! as a layout preset under a name.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde::{Deserialize, Serialize};

use crate::pane::AlertKind;
use gpui_kit::component::notification::{Notification, NotificationType};
use crate::{
    CloseGroup, CloseTab, FocusDown, FocusExplorer, FormatDocument, NewBrowser, FocusLeft, FocusRight, FocusScm, FocusSearch, FocusUp, NewTerminal,
    NextTab, OpenFiles, OpenFolder, OpenSettings, PrevTab, ReplaceInFiles, SplitDown, SplitRight,
    defaults::{Defaults, GroupDefault, Kind},
    backend::watch,
    diff::DiffPanel,
    explorer::{Explorer, ExplorerEvent},
    layout::{Node, NodeId, PaneId, Side, Tree},
    layout_view::DropHint,
    pane::{self, Pane as _, PaneRef, PaneState},
    panels::{FilePanel, SettingsPanel},
    repo::Repo,
    scm::{ScmEvent, ScmView},
    search::{IsDirty, SearchEvent, SearchView},
    settings::{AppState, LayoutPreset, Preset, Settings, SidebarSide, SidebarView},
    terminal::{Launch, TerminalPanel},
};

/// The arrangement as saved: the tree, the panes in it and the defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutState {
    pub root: Node,
    #[serde(default)]
    pub panes: HashMap<PaneId, PaneState>,
    #[serde(default)]
    pub defaults: Vec<GroupDefault>,
    #[serde(default)]
    pub active: Option<NodeId>,
}

/// The taskbar's jump list: right-click den on the taskbar
/// for its recent folders, each opening in a new window. Entries removed
/// there leave the title bar's list too.
pub(crate) fn sync_jump_list(cx: &mut App) {
    let entries: Vec<smallvec::SmallVec<[PathBuf; 2]>> =
        AppState::get(cx).recent.iter().take(12).map(|path| smallvec::smallvec![path.clone()]).collect();
    let removed = cx.update_jump_list(Vec::new(), entries);
    cx.spawn(async move |cx| {
        let removed = removed.await;
        if removed.is_empty() {
            return;
        }
        _ = cx.update(|cx| {
            let keys: Vec<String> = removed.iter().flatten().map(|p| crate::repo::key(p)).collect();
            AppState::update(cx, |state| state.recent.retain(|p| !keys.contains(&crate::repo::key(p))));
        });
    })
    .detach();
}

/// A layout preset being dragged to another place in the Layouts menu.
#[derive(Clone)]
struct LayoutPresetDrag {
    ix: usize,
}

/// A row of the Layouts menu: preset `ix` (by position, read as it draws),
/// draggable onto another row, with an x on hover that deletes it.
fn layout_preset_row(ix: usize, cx: &App) -> AnyElement {
    let Some(name) = AppState::get(cx).presets.get(ix).map(|p| p.name.clone()) else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let (hover, drop) = (theme.accent, theme.drop_target);
    let group = SharedString::from(format!("layout-row-{ix}"));
    let label = name.clone();
    h_flex()
        .id(("layout-row", ix))
        .group(group.clone())
        .w_full()
        .min_w(px(180.))
        .gap_2()
        .child(div().flex_1().min_w_0().truncate().child(name))
        .child(
            div()
                .id(("layout-delete", ix))
                .flex_none()
                .rounded(px(3.))
                .invisible()
                .group_hover(group, |this| this.visible())
                .hover(move |this| this.bg(hover))
                .child(Icon::new(IconName::X).xsmall())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    AppState::update(cx, |state| {
                        if ix < state.presets.len() {
                            state.presets.remove(ix);
                        }
                    });
                    window.refresh();
                }),
        )
        .on_drag(LayoutPresetDrag { ix }, move |_, _, _, cx| cx.new(|_| crate::layout_view::DragLabel(label.clone().into())))
        .drag_over::<LayoutPresetDrag>(move |this, _, _, _| this.bg(drop))
        .on_drop(move |drag: &LayoutPresetDrag, window, cx| {
            let from = drag.ix;
            AppState::update(cx, |state| {
                if from != ix && from < state.presets.len() && ix < state.presets.len() {
                    let preset = state.presets.remove(from);
                    state.presets.insert(ix, preset);
                }
            });
            window.refresh();
        })
        .into_any_element()
}

pub struct Workspace {
    pub(crate) root: PathBuf,
    pub(crate) tree: Tree,
    pub(crate) panes: HashMap<PaneId, PaneRef>,
    pane_subscriptions: HashMap<PaneId, Subscription>,
    pub(crate) defaults: Defaults,
    pub(crate) active_group: NodeId,
    /// The preview tab (italic), replaced by the next file opened as a preview.
    pub(crate) preview: Option<PaneId>,
    /// Where a dragged tab, group or container would land.
    pub(crate) drop_hint: Option<DropHint>,
    /// Alt is down: the split buttons split down instead of right.
    pub(crate) alt_held: bool,
    /// Tabs whose program wanted the user while they looked elsewhere.
    pub(crate) attention: std::collections::HashSet<PaneId>,
    explorer: Entity<Explorer>,
    search: Entity<SearchView>,
    scm: Entity<ScmView>,
    repo: Entity<Repo>,
    _watcher: Option<watch::Watcher>,
    _watch_task: Option<Task<()>>,
    _save_task: Option<Task<()>>,
    /// `.den/layout.json` as last read or written, so an unchanged
    /// arrangement is not written again.
    layout_file_text: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        AppState::update(cx, |state| state.opened(&root));
        sync_jump_list(cx);
        // Closing the window saves the session, asking first about unsaved changes.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let Some(this) = weak.upgrade() else { return true };
            let dirty = this.read(cx).dirty_panes(cx);
            if dirty.is_empty() {
                this.update(cx, |this, cx| this.save_session(cx));
                return true;
            }
            this.update(cx, |this, cx| {
                this.confirm_discard(
                    dirty,
                    |this, window, cx| {
                        this.save_session(cx);
                        window.remove_window();
                    },
                    window,
                    cx,
                )
            });
            false
        });
        let session = AppState::get(cx).session(&root);
        let repo = cx.new(|cx| Repo::new(root.clone(), cx));
        let explorer = cx.new(|cx| Explorer::new(root.clone(), session.expanded.clone(), repo.clone(), cx));
        let weak = cx.weak_entity();
        let is_dirty: IsDirty = std::rc::Rc::new(move |path, cx| weak.upgrade().is_some_and(|this| this.read(cx).is_dirty_file(path, cx)));
        let search = cx.new(|cx| SearchView::new(root.clone(), is_dirty, window, cx));
        let scm = cx.new(|cx| ScmView::new(repo.clone(), window, cx));
        if !session.commit_message.is_empty() {
            let draft = session.commit_message.clone();
            scm.update(cx, |scm, cx| scm.set_draft(draft, window, cx));
        }

        let _subscriptions = vec![
            cx.subscribe_in(&explorer, window, |this, _, event: &ExplorerEvent, window, cx| match event {
                ExplorerEvent::Open { path, preview } => this.open_file(path.clone(), *preview, window, cx),
                ExplorerEvent::ExpandedChanged => this.schedule_save(cx),
                ExplorerEvent::OpenTerminal(dir) => this.open_shell_in(dir.clone(), window, cx),
            }),
            cx.subscribe_in(&search, window, |this, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Open { path, line, column, focus } => {
                    this.open_file(path.clone(), !*focus, window, cx);
                    this.go_to(path, *line, *column, *focus, window, cx);
                }
            }),
            cx.subscribe_in(&scm, window, |this, _, event: &ScmEvent, window, cx| match event {
                ScmEvent::Open(path) => this.open_file(path.clone(), false, window, cx),
                ScmEvent::Diff { file, staged } => this.open_diff(file.clone(), *staged, window, cx),
                ScmEvent::CommitDiff { rel, commit } => this.open_commit_diff(rel.clone(), commit.clone(), window, cx),
            }),
            // Coming back to the window, git may have changed underneath.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.repo.update(cx, |repo, cx| repo.refresh(cx));
                    this.seen(this.active_group, cx);
                }
            }),
            cx.observe_global::<Settings>(|_, cx| cx.notify()),
            cx.on_app_quit(|this, cx| {
                crate::backend::ai::stop();
                this.save_session(cx);
                async {}
            }),
        ];

        let tree = Tree::new();
        let active_group = tree.root.id();
        let mut this = Self {
            root,
            tree,
            panes: HashMap::new(),
            pane_subscriptions: HashMap::new(),
            defaults: Defaults::default(),
            active_group,
            preview: None,
            drop_hint: None,
            alt_held: false,
            attention: Default::default(),
            explorer,
            search,
            scm,
            repo,
            _watcher: None,
            _watch_task: None,
            _save_task: None,
            layout_file_text: None,
            _subscriptions,
        };
        this.start_watching(window, cx);
        // Once per run, a little after start: is there a newer den?
        static UPDATE_CHECKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !UPDATE_CHECKED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            cx.spawn_in(window, async move |_, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(10)).await;
                _ = cx.update(|window, cx| crate::update::check_in_window(false, window, cx));
            })
            .detach();
        }
        // The folder's own file first; the session file's tab state merges in.
        // When the folder's file does not load (the Tauri den wrote another
        // format under the same name), the session file's layout is used.
        let from_folder = crate::layout_file::read(&this.root);
        this.layout_file_text = from_folder.as_ref().map(crate::layout_file::text);
        let restored = from_folder.map(|shared| crate::layout_file::restore(shared, &this.root, session.layout.as_ref()));
        let loaded = match restored.map(|layout| this.load_state(layout, window, cx)) {
            Some(Ok(())) => Ok(()),
            _ => session.layout.map_or(Ok(()), |layout| this.load_state(layout, window, cx)),
        };
        if let Err(err) = loaded {
            eprintln!("den: the saved layout could not be read, starting fresh: {err}");
        }
        this
    }

    // -- State ---------------------------------------------------------------

    /// Replace the arrangement with a saved one, making its panes anew.
    pub fn load_state(&mut self, value: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let state: LayoutState = serde_json::from_value(value)?;
        let mut tree = Tree::from_root(state.root);
        let mut panes = HashMap::new();
        for id in tree.panes() {
            match state.panes.get(&id) {
                // Settings are a dialog now; a tab of them in an older layout goes.
                Some(saved) if saved.kind == crate::panels::SETTINGS => {
                    tree.remove_tab(id);
                }
                Some(saved) => {
                    tree.reserve(id);
                    panes.insert(id, pane::build(saved, window, cx));
                }
                // A tab the file names but does not describe has nothing to open.
                None => {
                    tree.remove_tab(id);
                }
            }
        }
        self.panes.clear();
        self.pane_subscriptions.clear();
        for (id, pane) in panes {
            self.track(id, pane, window, cx);
        }
        self.defaults = Defaults::from_saved(state.defaults, &tree);
        self.active_group = state
            .active
            .filter(|id| tree.find(*id).is_some_and(Node::is_group))
            .or_else(|| tree.groups().first().copied())
            .unwrap_or(tree.root.id());
        self.tree = tree;
        cx.notify();
        Ok(())
    }

    pub fn dump_state(&self, cx: &App) -> LayoutState {
        LayoutState {
            root: self.tree.root.clone(),
            panes: self
                .panes
                .iter()
                .map(|(id, pane)| (*id, pane::dump(pane, cx)))
                .collect(),
            defaults: self.defaults.saved(),
            active: Some(self.active_group),
        }
    }

    fn reset_layout(&mut self, cx: &mut Context<Self>) {
        self.panes.clear();
        self.pane_subscriptions.clear();
        self.tree = Tree::new();
        self.defaults.clear();
        self.active_group = self.tree.root.id();
        self.changed(cx);
    }

    fn track(&mut self, id: PaneId, pane: PaneRef, window: &mut Window, cx: &mut Context<Self>) {
        self.pane_subscriptions.insert(id, pane.subscribe(id, window, cx));
        self.panes.insert(id, pane);
    }

    /// After every edit of the arrangement: redraw and save.
    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        self.schedule_save(cx);
    }

    // -- Groups and tabs -----------------------------------------------------

    pub(crate) fn set_active_group(&mut self, group: NodeId, cx: &mut Context<Self>) {
        self.seen(group, cx);
        if self.active_group != group && self.tree.find(group).is_some_and(Node::is_group) {
            self.active_group = group;
            self.defaults.activate(group);
            cx.notify();
        }
    }

    /// Where a new tab of `kind` goes: a default group of that kind, else the
    /// active group.
    pub(crate) fn target_group(&self, kind: Kind) -> NodeId {
        self.defaults
            .target(&self.tree, kind, Some(self.active_group))
            .unwrap_or(self.active_group)
    }

    /// Put a new pane in `group` and show it.
    pub(crate) fn add_pane(&mut self, pane: PaneRef, group: NodeId, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.tree.mint();
        self.track(id, pane.clone(), window, cx);
        self.tree.add_tab(group, id, None, true);
        self.set_active_group(group, cx);
        if focus {
            pane.focus_handle(cx).focus(window, cx);
        }
        self.changed(cx);
    }

    pub(crate) fn show_pane(&mut self, pane: PaneId, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.tree.activate(pane);
        self.attention.remove(&pane);
        if let Some(group) = self.tree.group_of(pane) {
            self.set_active_group(group, cx);
        }
        if focus && let Some(pane) = self.panes.get(&pane) {
            pane.focus_handle(cx).focus(window, cx);
        }
        self.changed(cx);
    }

    /// Close a tab; its group stays, empty or not.
    pub(crate) fn close_pane(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.preview == Some(pane) {
            self.preview = None;
        }
        self.tree.remove_tab(pane);
        self.panes.remove(&pane);
        self.pane_subscriptions.remove(&pane);
        self.changed(cx);
    }

    pub(crate) fn close_group(&mut self, group: NodeId, cx: &mut Context<Self>) {
        for pane in self.tree.tabs(group).to_vec() {
            self.panes.remove(&pane);
            self.pane_subscriptions.remove(&pane);
            self.tree.remove_tab(pane);
        }
        if let Some((_, remap)) = self.tree.detach(group) {
            self.defaults.prune(&self.tree, &remap);
        }
        self.fix_active_group();
        self.changed(cx);
    }

    /// Close a container with every group and tab in it.
    pub(crate) fn close_container(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let groups = self.tree.groups_under(node);
        for &group in &groups {
            for pane in self.tree.tabs(group).to_vec() {
                self.panes.remove(&pane);
                self.pane_subscriptions.remove(&pane);
                self.tree.remove_tab(pane);
            }
        }
        match self.tree.detach(node) {
            Some((_, remap)) => self.defaults.prune(&self.tree, &remap),
            // The root has nowhere to go: close its groups one by one.
            None => groups.into_iter().for_each(|group| self.close_group(group, cx)),
        }
        self.fix_active_group();
        self.changed(cx);
    }

    /// The tab `group` shows has been seen: its attention mark goes.
    fn seen(&mut self, group: NodeId, cx: &mut Context<Self>) {
        if let Some(pane) = self.tree.active_tab(group)
            && self.attention.remove(&pane)
        {
            cx.notify();
        }
    }

    /// A terminal's program wants the user. Unless they look at that tab:
    /// mark the tab, play a sound, toast, and flash the taskbar while the
    /// window is in the background, each as Settings allow.
    pub(crate) fn pane_alert(&mut self, pane: PaneId, kind: AlertKind, message: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let settings = Settings::get(cx).clone();
        if !settings.notifications {
            return;
        }
        let focused = window.is_window_active();
        if focused && self.tree.active_tab(self.active_group) == Some(pane) {
            return;
        }
        if settings.notify_tab {
            self.attention.insert(pane);
            cx.notify();
        }
        if settings.notify_sound {
            crate::sound::play(kind, cx);
        }
        if !focused && settings.notify_taskbar {
            window.request_attention();
        }
        if settings.notify_toast {
            let Some(handle) = self.panes.get(&pane).cloned() else { return };
            let name = handle.label(cx);
            let status = match kind {
                AlertKind::Done => "finished",
                AlertKind::Input => "needs input",
                AlertKind::Attention => "needs attention",
            };
            let title = handle.view().downcast::<crate::terminal::TerminalPanel>().ok().and_then(|t| t.read(cx).title_text());
            let body: Vec<String> = [message.map(|m| m.trim().to_string()), title.filter(|t| t.as_str() != name.as_ref())]
                .into_iter()
                .flatten()
                .filter(|text| !text.is_empty())
                .collect();
            let this = cx.weak_entity();
            let note = Notification::new()
                .title(format!("{name} {status}"))
                .message(body.join(" — "))
                .with_type(if kind == AlertKind::Done { NotificationType::Success } else { NotificationType::Warning })
                .on_click(move |_, window, cx| {
                    _ = this.update(cx, |this, cx| this.show_pane(pane, true, window, cx));
                });
            crate::toast::push(window, note, cx);
        }
    }

    /// Format Document on the active group's file tab.
    pub(crate) fn format_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let file = self
            .tree
            .active_tab(self.active_group)
            .and_then(|pane| self.panes.get(&pane))
            .and_then(|pane| pane.view().downcast::<crate::panels::FilePanel>().ok());
        if let Some(file) = file {
            file.update(cx, |file, cx| file.format(false, window, cx));
        }
    }

    fn fix_active_group(&mut self) {
        if !self.tree.find(self.active_group).is_some_and(Node::is_group) {
            self.active_group = self.tree.groups().first().copied().unwrap_or(self.tree.root.id());
        }
    }

    /// A new empty group beside `node` (a group, a container or the root).
    pub(crate) fn split(&mut self, node: NodeId, side: Side, cx: &mut Context<Self>) {
        if let Some(group) = self.tree.split(node, side) {
            self.set_active_group(group, cx);
        }
        self.changed(cx);
    }

    pub(crate) fn flip(&mut self, split: NodeId, cx: &mut Context<Self>) {
        if self.tree.flip(split) {
            self.changed(cx);
        }
    }

    pub(crate) fn set_default(&mut self, node: NodeId, container: bool, kind: Option<Kind>, cx: &mut Context<Self>) {
        self.defaults.set(&self.tree, node, container, kind);
        self.changed(cx);
    }

    /// Move a group or container beside `target`.
    pub(crate) fn move_node(&mut self, node: NodeId, target: NodeId, side: Side, cx: &mut Context<Self>) {
        if let Some(remap) = self.tree.move_node(node, target, side) {
            self.defaults.prune(&self.tree, &remap);
            if let Some(group) = self.tree.groups_under(node).first() {
                self.set_active_group(*group, cx);
            }
            self.changed(cx);
        }
    }

    /// Move a tab into `group` (at `ix`), or into a new group beside `target`
    /// when `side` is given. A group the tab leaves empty goes with it.
    pub(crate) fn move_tab(&mut self, pane: PaneId, target: NodeId, side: Option<Side>, ix: Option<usize>, cx: &mut Context<Self>) {
        let Some(from) = self.tree.group_of(pane) else { return };
        let group = match side {
            None => target,
            Some(_) if target == from && self.tree.tabs(from).len() == 1 => return,
            Some(side) => match self.tree.split(target, side) {
                Some(group) => group,
                None => return,
            },
        };
        self.tree.move_tab(pane, group, ix);
        if from != group
            && self.tree.tabs(from).is_empty()
            && let Some((_, remap)) = self.tree.detach(from)
        {
            self.defaults.prune(&self.tree, &remap);
        }
        self.fix_active_group();
        self.set_active_group(group, cx);
        self.changed(cx);
    }

    fn cycle_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.tree.tabs(self.active_group).to_vec();
        let Some(current) = self.tree.active_tab(self.active_group) else { return };
        let Some(ix) = tabs.iter().position(|p| *p == current) else { return };
        let next = tabs[(ix as isize + step).rem_euclid(tabs.len() as isize) as usize];
        self.show_pane(next, true, window, cx);
    }

    // -- Opening things ------------------------------------------------------

    fn find_pane(&self, matches: impl Fn(&AnyView, &App) -> bool, cx: &App) -> Option<PaneId> {
        self.tree
            .panes()
            .into_iter()
            .find(|id| self.panes.get(id).is_some_and(|pane| matches(&pane.view(), cx)))
    }

    /// Open a file: as the preview tab (a single click; focus stays where it
    /// is, and the next preview replaces it), or kept, as in den.
    pub fn open_file(&mut self, path: PathBuf, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.find_pane(
            |view, cx| {
                view.clone()
                    .downcast::<FilePanel>()
                    .is_ok_and(|file| file.read(cx).path() == path)
            },
            cx,
        );
        match existing {
            Some(pane) => {
                if !preview && self.preview == Some(pane) {
                    self.preview = None;
                }
                self.show_pane(pane, !preview, window, cx);
            }
            None => {
                let file = cx.new(|cx| FilePanel::new(path, window, cx));
                let group = self.target_group(Kind::Files);
                // The group's preview tab gives way to the new one.
                let replaced = self
                    .preview
                    .filter(|id| preview && self.tree.group_of(*id) == Some(group))
                    .filter(|id| !self.panes.get(id).is_some_and(|p| p.is_dirty(cx)));
                self.add_pane(std::rc::Rc::new(file), group, !preview, window, cx);
                let added = self.tree.active_tab(group);
                if let Some(old) = replaced {
                    // Into the old tab's place, so the strip does not shuffle.
                    if let (Some(added), Some(ix)) = (added, self.tree.tabs(group).iter().position(|p| *p == old)) {
                        self.tree.move_tab(added, group, Some(ix));
                    }
                    self.close_pane(old, cx);
                }
                if preview {
                    self.preview = added;
                }
            }
        }
    }

    /// Keep the preview tab open (editing it, or double-clicking it), or turn
    /// a kept tab back into the preview.
    pub(crate) fn toggle_preview(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.preview == Some(pane) {
            self.preview = None;
        } else if self.panes.get(&pane).is_some_and(|p| p.view().downcast::<FilePanel>().is_ok()) {
            self.preview = Some(pane);
        }
        cx.notify();
    }

    /// A pane changed: an edited preview is kept.
    pub(crate) fn pane_changed(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.preview == Some(pane) && self.panes.get(&pane).is_some_and(|p| p.is_dirty(cx)) {
            self.preview = None;
        }
        cx.notify();
    }

    /// Open a file and put the cursor at `line` and `column` (both from 1).
    pub(crate) fn open_file_at(&mut self, path: PathBuf, line: Option<u32>, column: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file(path.clone(), false, window, cx);
        if let Some(line) = line {
            self.go_to(&path, line, column.unwrap_or(1), true, window, cx);
        }
    }

    fn file_pane(&self, path: &std::path::Path, cx: &App) -> Option<Entity<FilePanel>> {
        self.panes.values().find_map(|pane| {
            let file = pane.view().downcast::<FilePanel>().ok()?;
            (file.read(cx).path() == path).then_some(file)
        })
    }

    /// Move an open file's cursor to `line` and `column` (both from 1).
    pub(crate) fn go_to(&mut self, path: &std::path::Path, line: u32, column: u32, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self.file_pane(path, cx) {
            file.update(cx, |file, cx| file.go_to(line, column, focus, window, cx));
        }
    }

    /// A file with unsaved changes in a tab.
    pub(crate) fn is_dirty_file(&self, path: &std::path::Path, cx: &App) -> bool {
        self.file_pane(path, cx).is_some_and(|file| file.read(cx).is_dirty(cx))
    }

    // -- Following the disk -------------------------------------------------

    /// Watch the session root: the Explorer, git status and open files follow
    /// changes on disk.
    fn start_watching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok((watcher, mut rx)) = watch::watch(&self.root) else { return };
        self._watcher = Some(watcher);
        self._watch_task = Some(cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let timer = move |d| -> futures::future::BoxFuture<'static, ()> { Box::pin(executor.timer(d)) };
            while let Some(batch) = watch::next_batch(&mut rx, Duration::from_millis(150), &timer).await {
                if this.update_in(cx, |this, window, cx| this.on_disk_changed(batch, window, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn on_disk_changed(&mut self, batch: std::collections::BTreeSet<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let outside_git: Vec<PathBuf> = batch.iter().filter(|p| !watch::in_git_dir(p)).cloned().collect();
        if !outside_git.is_empty() {
            self.explorer.update(cx, |explorer, cx| explorer.paths_changed(&outside_git, cx));
            // Open files without unsaved changes take what is on disk now.
            for path in &outside_git {
                if let Some(file) = self.file_pane(path, cx) {
                    file.update(cx, |file, cx| file.reload_from_disk(window, cx));
                }
            }
        }
        if !outside_git.is_empty() || batch.iter().any(|p| watch::is_git_state(p)) {
            self.repo.update(cx, |repo, cx| repo.refresh(cx));
            self.reload_diffs(cx);
        }
    }

    /// A change side by side, in an existing diff tab when there is one.
    fn open_diff(&mut self, file: crate::backend::git::FileStatus, staged: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else { return };
        let existing = self.find_pane(
            |view, cx| {
                view.clone()
                    .downcast::<DiffPanel>()
                    .is_ok_and(|diff| diff.read(cx).path() == file.path && diff.read(cx).staged() == staged && diff.read(cx).commit().is_none())
            },
            cx,
        );
        match existing {
            Some(pane) => {
                if let Some(diff) = self.panes.get(&pane).and_then(|p| p.view().downcast::<DiffPanel>().ok()) {
                    diff.update(cx, |diff, cx| {
                        diff.reload();
                        cx.notify();
                    });
                }
                self.show_pane(pane, false, window, cx);
            }
            None => {
                let diff = cx.new(|cx| DiffPanel::new(file.path.clone(), top, file.rel.clone(), staged, cx));
                let group = self.target_group(Kind::Files);
                self.add_pane(std::rc::Rc::new(diff), group, false, window, cx);
            }
        }
    }

    /// A commit's change to one file; shown again when already open.
    fn open_commit_diff(&mut self, rel: String, commit: crate::diff::CommitRevs, window: &mut Window, cx: &mut Context<Self>) {
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else { return };
        let path = top.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let hash = commit.hash.clone();
        let existing = self.find_pane(
            |view, cx| {
                view.clone().downcast::<DiffPanel>().is_ok_and(|diff| {
                    let diff = diff.read(cx);
                    diff.path() == path && diff.commit().is_some_and(|c| c.hash == hash)
                })
            },
            cx,
        );
        match existing {
            Some(pane) => self.show_pane(pane, false, window, cx),
            None => {
                let diff = cx.new(|cx| DiffPanel::for_commit(path, top, rel, commit, cx));
                let group = self.target_group(Kind::Files);
                self.add_pane(std::rc::Rc::new(diff), group, false, window, cx);
            }
        }
    }

    fn reload_diffs(&mut self, cx: &mut Context<Self>) {
        // The index changed (a stage, a commit, a checkout): new gutter marks.
        for pane in self.panes.values() {
            if let Ok(file) = pane.view().downcast::<FilePanel>() {
                file.update(cx, |file, cx| file.refresh_git_base(cx));
            }
        }
        for pane in self.panes.values() {
            if let Ok(diff) = pane.view().downcast::<DiffPanel>()
                && diff.read(cx).commit().is_none()
            {
                diff.update(cx, |diff, cx| {
                    diff.reload();
                    cx.notify();
                });
            }
        }
    }

    /// Settings in a dialog over the window, as den has them.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| SettingsPanel::new(window, cx));
        let viewport = window.viewport_size();
        let height = (viewport.height - px(140.)).max(px(320.));
        let width = (viewport.width - px(80.)).min(px(860.)).max(px(480.));
        let this = cx.weak_entity();
        let search = view.read(cx).search_input();
        window.open_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title("Settings")
                .w(width)
                .margin_top(px(48.))
                .child(div().h(height).child(view.clone()))
                .on_close(move |_, _, cx| _ = this.update(cx, |_, cx| cx.notify()))
        });
        window.defer(cx, move |window, cx| search.update(cx, |input, cx| input.focus(window, cx)));
        // A browser page hides while the dialog is open.
        cx.notify();
    }

    /// A terminal running a preset (`None` for a plain shell): in `group`
    /// when a group's button asked, else where its kind goes.
    pub(crate) fn open_terminal(&mut self, group: Option<NodeId>, preset: Option<Preset>, window: &mut Window, cx: &mut Context<Self>) {
        let agent = preset.as_ref().is_some_and(|p| p.agent);
        let kind = if agent { Kind::Agents } else { Kind::Terminals };
        let group = group.unwrap_or_else(|| self.target_group(kind));
        let program = preset.map(|p| p.command.trim().to_string()).filter(|c| !c.is_empty());
        let launch = Launch {
            cwd: self.root.clone(),
            program: program.clone(),
            command: program,
            history: None,
            agent,
        };
        let terminal = cx.new(|cx| TerminalPanel::new(launch, window, cx));
        self.add_pane(std::rc::Rc::new(terminal), group, true, window, cx);
    }

    /// A browser tab at `url` (the home page when none), in `group` or where
    /// browsers go.
    pub(crate) fn open_browser(&mut self, group: Option<NodeId>, url: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let group = group.unwrap_or_else(|| self.target_group(Kind::Browsers));
        let browser = cx.new(|cx| crate::browser::BrowserPanel::new(url, window, cx));
        self.add_pane(std::rc::Rc::new(browser), group, true, window, cx);
    }

    /// Show the browser pages whose tab is showing; hide the others, and all
    /// of them while a tab or group is dragged or a dialog is open (a native
    /// page is drawn over everything).
    fn sync_browsers(&self, window: &mut Window, cx: &mut Context<Self>) {
        // A page stays live: only a drag or a dialog hides it (menus and
        // toasts may be drawn under it).
        let covered = cx.has_active_drag() || window.has_active_dialog(cx) || window.has_active_sheet(cx);
        for (id, pane) in &self.panes {
            let Ok(browser) = pane.view().downcast::<crate::browser::BrowserPanel>() else { continue };
            let showing = self.tree.group_of(*id).is_some_and(|group| self.tree.active_tab(group) == Some(*id));
            browser.read(cx).set_shown(showing && !covered);
        }
    }

    /// A shell starting in `dir` (Open Terminal Here).
    fn open_shell_in(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.target_group(Kind::Terminals);
        let launch = Launch {
            cwd: dir,
            program: None,
            command: None,
            history: None,
            agent: false,
        };
        let terminal = cx.new(|cx| TerminalPanel::new(launch, window, cx));
        self.add_pane(std::rc::Rc::new(terminal), group, true, window, cx);
    }

    /// Another tab like those in `group`, as den does: a group of Claude Code
    /// tabs gets another Claude Code; a mix, or a shell among them, a shell.
    pub(crate) fn open_more(&mut self, group: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let programs: Vec<Option<(String, bool)>> = self
            .tree
            .tabs(group)
            .iter()
            .filter_map(|id| self.panes.get(id))
            .map(|pane| {
                let terminal = pane.view().downcast::<TerminalPanel>().ok()?;
                let terminal = terminal.read(cx);
                terminal.program().map(|p| (p.to_string(), terminal.is_agent()))
            })
            .collect();
        let same = programs.first().cloned().flatten().filter(|first| programs.iter().all(|p| p.as_ref() == Some(first)));
        // A group of browsers gets another browser.
        let tabs = self.tree.tabs(group);
        if !tabs.is_empty() && tabs.iter().all(|id| self.panes.get(id).is_some_and(|p| p.kind(cx) == crate::browser::BROWSER)) {
            return self.open_browser(Some(group), None, window, cx);
        }
        let preset = same.map(|(command, agent)| Preset {
            name: command.clone(),
            command,
            agent,
            browser: false,
            pinned: false,
            color: None,
            icon: None,
        });
        self.open_terminal(Some(group), preset, window, cx);
    }

    // -- Sessions and windows ---------------------------------------------

    /// Files with unsaved changes in this window.
    fn dirty_panes(&self, cx: &App) -> Vec<PaneId> {
        self.tree.panes().into_iter().filter(|id| self.panes.get(id).is_some_and(|p| p.is_dirty(cx))).collect()
    }

    /// Run `then` now, or after asking when tabs have unsaved changes.
    fn confirm_discard(&mut self, panes: Vec<PaneId>, then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static, window: &mut Window, cx: &mut Context<Self>) {
        let dirty: Vec<SharedString> = panes
            .iter()
            .filter_map(|id| self.panes.get(id))
            .filter(|p| p.is_dirty(cx))
            .map(|p| p.label(cx))
            .collect();
        if dirty.is_empty() {
            return then(self, window, cx);
        }
        let this = cx.weak_entity();
        let then = std::rc::Rc::new(then);
        let what = match dirty.as_slice() {
            [one] => format!("{one} has unsaved changes. Discard them?"),
            many => format!("{} files have unsaved changes: {}. Discard them?", many.len(), many.join(", ")),
        };
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let then = then.clone();
            dialog
                .title("Unsaved Changes")
                .description(what.clone())
                .ok_text("Discard")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let then = then.clone();
                    _ = this.update(cx, |this, cx| then(this, window, cx));
                    true
                })
        });
    }

    /// Close a tab, asking first when it has unsaved changes.
    pub(crate) fn request_close_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_discard(vec![pane], move |this, _, cx| this.close_pane(pane, cx), window, cx);
    }

    pub(crate) fn request_close_group(&mut self, group: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.tree.tabs(group).to_vec();
        self.confirm_discard(panes, move |this, _, cx| this.close_group(group, cx), window, cx);
    }

    pub(crate) fn request_close_container(&mut self, node: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.tree.groups_under(node).iter().flat_map(|&group| self.tree.tabs(group).to_vec()).collect();
        self.confirm_discard(panes, move |this, _, cx| this.close_container(node, cx), window, cx);
    }

    /// Open `root` as a session: in this window (saving this one first), or a new one.
    fn open_session(&mut self, root: PathBuf, new_window: bool, window: &mut Window, cx: &mut Context<Self>) {
        let root = crate::settings::strip_verbatim(std::fs::canonicalize(&root).unwrap_or(root));
        if new_window {
            crate::open_workspace(root, None, cx);
            return;
        }
        let panes = self.tree.panes();
        self.confirm_discard(
            panes,
            move |this, window, cx| {
                this.save_session(cx);
                crate::open_workspace(root.clone(), Some(window.bounds()), cx);
                window.remove_window();
            },
            window,
            cx,
        );
    }

    fn prompt_open_folder(&mut self, new_window: bool, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = paths.await.ok().and_then(Result::ok).flatten().and_then(|p| p.into_iter().next()) else { return };
            _ = this.update_in(cx, |this, window, cx| this.open_session(path, new_window, window, cx));
        })
        .detach();
    }

    fn prompt_open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Open Files".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(paths) = paths.await.ok().and_then(Result::ok).flatten() else { return };
            _ = this.update_in(cx, |this, window, cx| {
                for path in paths {
                    this.open_file(path, false, window, cx);
                }
            });
        })
        .detach();
    }

    /// Alt+arrows: the group next to the active one, and its shown tab.
    fn focus_group(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.tree.neighbor(self.active_group, side) else { return };
        self.set_active_group(group, cx);
        if let Some(pane) = self.tree.active_tab(group).and_then(|id| self.panes.get(&id)) {
            pane.focus_handle(cx).focus(window, cx);
        }
    }

    // -- Presets -------------------------------------------------------------

    fn load_preset(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = AppState::get(cx).presets.iter().find(|p| p.name == name).cloned() else {
            return;
        };
        if let Err(err) = self.load_state(preset.layout, window, cx) {
            crate::toast::push(window, format!("Could not load layout \"{name}\": {err}"), cx);
            return;
        }
        self.changed(cx);
    }

    fn save_preset(&mut self, name: String, cx: &mut Context<Self>) {
        let Ok(layout) = serde_json::to_value(self.dump_state(cx)) else { return };
        AppState::update(cx, |state| match state.presets.iter_mut().find(|p| p.name == name) {
            Some(preset) => preset.layout = layout,
            None => state.presets.push(LayoutPreset { name, layout }),
        });
        cx.notify();
    }


    fn prompt_save_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Layout name"));
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, {
            let input = input.clone();
            move |dialog, _, cx| {
                let existing: Vec<String> = AppState::get(cx).presets.iter().map(|p| p.name.clone()).collect();
                dialog
                    .title("Save Layout")
                    .show_cancel(true)
                    .child(
                        v_flex()
                            .gap_2()
                            .child(Input::new(&input))
                            .when(!existing.is_empty(), |list| {
                                let theme = cx.theme().clone();
                                list.child(div().pt_1().text_xs().text_color(theme.muted_foreground).child("Or replace a saved layout:"))
                                    .children(existing.iter().enumerate().map(|(ix, name)| {
                                        let this = this.clone();
                                        let name = name.clone();
                                        h_flex()
                                            .id(("replace-layout", ix))
                                            .gap_2()
                                            .px_2()
                                            .py_1()
                                            .rounded(px(4.))
                                            .text_sm()
                                            .hover(|row| row.bg(theme.accent))
                                            .child(Icon::new(IconName::LayoutTemplate).small().text_color(theme.muted_foreground))
                                            .child(name.clone())
                                            .on_click(move |_, window, cx| {
                                                let name = name.clone();
                                                _ = this.update(cx, |this, cx| this.save_preset(name, cx));
                                                window.close_dialog(cx);
                                            })
                                    }))
                            }),
                    )
                    .on_ok({
                        let input = input.clone();
                        let this = this.clone();
                        move |_, _, cx| {
                            let name = input.read(cx).value().trim().to_string();
                            if name.is_empty() {
                                return false;
                            }
                            _ = this.update(cx, |this, cx| this.save_preset(name, cx));
                            true
                        }
                    })
            }
        });
        window.defer(cx, move |window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    }

    // -- Session -------------------------------------------------------------

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self._save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            _ = this.update(cx, |this, cx| this.save_session(cx));
        }));
    }

    fn save_session(&mut self, cx: &mut Context<Self>) {
        let layout = serde_json::to_value(self.dump_state(cx)).ok();
        if let Some(layout) = &layout {
            let text = crate::layout_file::text(&crate::layout_file::shareable(layout, &self.root));
            if self.layout_file_text.as_deref() != Some(text.as_str()) {
                let path = crate::layout_file::path(&self.root);
                let written = path.parent().is_some_and(|dir| std::fs::create_dir_all(dir).is_ok()) && std::fs::write(&path, &text).is_ok();
                if written {
                    self.layout_file_text = Some(text);
                }
            }
        }
        let expanded = self.explorer.read(cx).expanded();
        let draft = self.scm.read(cx).draft(cx);
        let root = self.root.clone();
        AppState::update(cx, |state| {
            let session = state.session_mut(&root);
            session.layout = layout;
            session.expanded = expanded;
            session.commit_message = draft;
        });
    }

    // -- Sidebar -------------------------------------------------------------

    /// The title-bar buttons: show a view, or hide the sidebar when that view
    /// is already showing.
    fn toggle_view(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        AppState::update(cx, |state| {
            let sidebar = &mut state.sidebar;
            if sidebar.visible && sidebar.view == view {
                sidebar.visible = false;
            } else {
                sidebar.visible = true;
                sidebar.view = view;
            }
        });
        cx.notify();
    }

    /// The keys: focus a view, or hide the sidebar when the Explorer already
    /// has focus. (Search and Source Control keep focus in their inputs, so a
    /// second press there just focuses the input again.)
    fn focus_view(&mut self, view: SidebarView, window: &mut Window, cx: &mut Context<Self>) {
        let sidebar = AppState::get(cx).sidebar.clone();
        let focused = sidebar.visible
            && sidebar.view == view
            && view == SidebarView::Explorer
            && self.explorer.focus_handle(cx).contains_focused(window, cx);
        if focused {
            AppState::update(cx, |state| state.sidebar.visible = false);
        } else {
            AppState::update(cx, |state| {
                state.sidebar.visible = true;
                state.sidebar.view = view;
            });
            match view {
                SidebarView::Explorer => self.explorer.focus_handle(cx).focus(window, cx),
                SidebarView::Search => SearchView::focus(&self.search, window, cx),
                SidebarView::Scm => ScmView::focus(&self.scm, window, cx),
            }
        }
        cx.notify();
    }

    // -- Render --------------------------------------------------------------

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = AppState::get(cx).sidebar.clone();
        let presets: Vec<String> = AppState::get(cx).presets.iter().map(|p| p.name.clone()).collect();
        let session_name = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.display().to_string());
        let root_node = self.tree.root.id();
        let root_is_split = !self.tree.root.is_group();
        let this = cx.weak_entity();

        let view_button = |id: &'static str, icon: IconName, tooltip: &'static str, view: SidebarView, cx: &mut Context<Self>| {
            let active = sidebar.visible && sidebar.view == view;
            Button::new(id)
                .small()
                .icon(Icon::new(icon))
                .tooltip(tooltip)
                .map(|button| if active { button.primary() } else { button.ghost() })
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_view(view, cx)))
        };

        let layout_button = Button::new("layouts")
            .small()
            .ghost()
            .icon(Icon::new(IconName::LayoutDashboard))
            .tooltip("Layouts")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.label("LAYOUTS");
                if presets.is_empty() {
                    menu = menu.item(PopupMenuItem::new("No saved layouts").disabled(true));
                }
                // As in den: drag a layout to reorder, its x (on hover) deletes it.
                // Each row reads its layout by position as it draws, so the open
                // menu follows a move or a delete.
                for ix in 0..presets.len() {
                    let this = this.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| layout_preset_row(ix, cx)).icon(Icon::new(IconName::LayoutTemplate)).on_click(move |_, window, cx| {
                            let Some(name) = AppState::get(cx).presets.get(ix).map(|p| p.name.clone()) else { return };
                            _ = this.update(cx, |this, cx| this.load_preset(&name, window, cx));
                        }),
                    );
                }
                let save = this.clone();
                let reset = this.clone();
                menu = menu
                    .separator()
                    .item(PopupMenuItem::new("Save Layout…").icon(Icon::new(IconName::Save)).on_click(move |_, window, cx| {
                        _ = save.update(cx, |this, cx| this.prompt_save_preset(window, cx));
                    }))
                    .item(PopupMenuItem::new("Reset Layout").icon(Icon::new(IconName::RotateCcw)).on_click(move |_, _, cx| {
                        _ = reset.update(cx, |this, cx| this.reset_layout(cx));
                    }));
                menu
            });

        // As in den: the sidebar views on the left, the session in the middle,
        // the layout and the workspace's split and flip, then settings, on the
        // right. Left and right share the space equally so the middle centres.
        let border = cx.theme().border;
        let separator = move || div().w(px(1.)).h(px(16.)).mx_1p5().bg(border);
        TitleBar::new()
            .child(
                h_flex()
                    .flex_1()
                    .gap_1()
                    .child(div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(self.app_menu(cx)))
                    .child(separator())
                    .child(
                        h_flex()
                            .gap_1()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(view_button("view-explorer", IconName::Files, "Explorer (Ctrl+Shift+E)", SidebarView::Explorer, cx))
                            .child(view_button("view-search", IconName::Search, "Search (Ctrl+Shift+F)", SidebarView::Search, cx))
                            .child(view_button("view-scm", IconName::GitBranch, "Source Control (Ctrl+Shift+G)", SidebarView::Scm, cx)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.session_switcher(session_name, cx)),
            )
            .child(
                h_flex().flex_1().justify_end().px_2().child(
                    h_flex()
                        .gap_1()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(layout_button)
                        // The outermost container's actions, called the workspace's.
                        .children(self.container_actions(root_node, "workspace", root_is_split, cx))
                        .child(separator())
                        .child(
                            Button::new("settings")
                                .small()
                                .ghost()
                                .icon(Icon::new(IconName::Settings))
                                .tooltip("Settings (Ctrl+,)")
                                .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
                        ),
                ),
            )
    }

    /// den's menus, under one button: each item runs the action its chord does.
    fn app_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        Button::new("app-menu")
            .ghost()
            .small()
            .icon(Icon::new(IconName::Menu))
            .tooltip("Menu")
            .dropdown_menu(move |menu, _, _| {
                type Run = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);
                let item = |label: &'static str, chord: &'static str, run: Run| {
                    let this = this.clone();
                    let label = if chord.is_empty() { label.to_string() } else { format!("{label}    {chord}") };
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        _ = this.update(cx, |this, cx| run(this, window, cx));
                    })
                };
                menu.max_h(px(640.))
                    .scrollable(true)
                    .label("FILE")
                    .item(item("Open Folder…", "Ctrl+Shift+O", |this, window, cx| this.prompt_open_folder(false, window, cx)))
                    .item(item("Open Folder in New Window…", "", |this, window, cx| this.prompt_open_folder(true, window, cx)))
                    .item(item("Open Files…", "Ctrl+O", |this, window, cx| this.prompt_open_files(window, cx)))
                    .item(item("Close Tab", "Ctrl+Shift+W", |this, window, cx| {
                        if let Some(pane) = this.tree.active_tab(this.active_group) {
                            this.request_close_pane(pane, window, cx);
                        }
                    }))
                    .item(item("Close Group", "Ctrl+Shift+Q", |this, window, cx| this.request_close_group(this.active_group, window, cx)))
                    .separator()
                    .label("EDIT")
                    .item(item("Format Document", "Shift+Alt+F", |this, window, cx| this.format_active(window, cx)))
                    .item(item("Find in Files", "Ctrl+Shift+F", |this, window, cx| this.focus_view(SidebarView::Search, window, cx)))
                    .item(item("Replace in Files", "Ctrl+Shift+H", |_, window, cx| window.dispatch_action(Box::new(ReplaceInFiles), cx)))
                    .separator()
                    .label("VIEW")
                    .item(item("Explorer", "Ctrl+Shift+E", |this, _, cx| this.toggle_view(SidebarView::Explorer, cx)))
                    .item(item("Search", "", |this, _, cx| this.toggle_view(SidebarView::Search, cx)))
                    .item(item("Source Control", "Ctrl+Shift+G", |this, _, cx| this.toggle_view(SidebarView::Scm, cx)))
                    .item(item("Split Right", "Ctrl+Shift+D", |this, _, cx| this.split(this.active_group, Side::Right, cx)))
                    .item(item("Split Down", "Ctrl+Shift+-", |this, _, cx| this.split(this.active_group, Side::Bottom, cx)))
                    .item(item("Save Layout…", "", |this, window, cx| this.prompt_save_preset(window, cx)))
                    .item(item("Reset Layout", "", |this, _, cx| this.reset_layout(cx)))
                    .item(item("Settings", "Ctrl+,", |this, window, cx| this.open_settings(window, cx)))
                    .item(item("Check for Updates…", "", |_, window, cx| crate::update::check_in_window(true, window, cx)))
                    .separator()
                    .label("TERMINAL")
                    .item(item("New Terminal", "Ctrl+Shift+T", |this, window, cx| this.open_terminal(None, None, window, cx)))
                    .item(item("New Browser", "Ctrl+Shift+B", |this, window, cx| this.open_browser(None, None, window, cx)))
                    .item(item("Claude Code", "", |this, window, cx| {
                        let preset = Settings::get(cx).presets.iter().find(|p| p.agent).cloned();
                        this.open_terminal(None, preset, window, cx)
                    }))
                    .separator()
                    .item(item("Exit", "Alt+F4", |_, window, _| window.remove_window()))
            })
    }

    /// The session name in the title bar, as den's: the folders opened before,
    /// and opening another.
    fn session_switcher(&self, name: String, cx: &mut Context<Self>) -> impl IntoElement {
        let recent: Vec<PathBuf> = AppState::get(cx)
            .recent
            .iter()
            .filter(|p| crate::repo::key(p) != crate::repo::key(&self.root))
            .cloned()
            .collect();
        // Each folder by its name, with as many parents as it takes to tell
        // it from the others; the full path in a tooltip.
        let all: Vec<PathBuf> = std::iter::once(self.root.clone()).chain(recent.iter().cloned()).collect();
        let mut names = crate::settings::unique_names(&all);
        let name = if names.is_empty() { name } else { names.remove(0) };
        let this = cx.weak_entity();
        Button::new("session")
            .ghost()
            .small()
            .label(name)
            .dropdown_caret(true)
            .tooltip("Switch session (Ctrl+Shift+O opens a folder)")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.label("RECENT FOLDERS").max_h(px(480.)).scrollable(true);
                if recent.is_empty() {
                    menu = menu.item(PopupMenuItem::new("No other folders yet").disabled(true));
                }
                for (ix, (path, label)) in recent.iter().zip(&names).enumerate() {
                    let this = this.clone();
                    let path = path.clone();
                    let (label, full) = (SharedString::from(label.clone()), SharedString::from(path.display().to_string()));
                    let forget = path.clone();
                    let item = PopupMenuItem::element(move |_, _| {
                        let full = full.clone();
                        let forget = forget.clone();
                        h_flex()
                            .id(("recent", ix))
                            .group(SharedString::from(format!("recent-{ix}")))
                            .w_full()
                            .gap_2()
                            .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                            // The x takes the folder off the list (its saved session stays),
                            // as in den.
                            .child(
                                div()
                                    .id(("forget-recent", ix))
                                    .flex_none()
                                    .invisible()
                                    .group_hover(SharedString::from(format!("recent-{ix}")), |this| this.visible())
                                    .rounded(px(3.))
                                    .hover(|this| this.bg(gpui_kit::hsla(0., 0., 0.5, 0.25)))
                                    .child(Icon::new(IconName::X).xsmall())
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        let forget = forget.clone();
                                        AppState::update(cx, |state| {
                                            let key = crate::repo::key(&forget);
                                            state.recent.retain(|p| crate::repo::key(p) != key);
                                        });
                                        sync_jump_list(cx);
                                        window.refresh();
                                    }),
                            )
                            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx))
                    });
                    menu = menu.item(item.icon(Icon::new(IconName::Folder)).on_click(move |_, window, cx| {
                        _ = this.update(cx, |this, cx| this.open_session(path.clone(), false, window, cx));
                    }));
                }
                let open = this.clone();
                let new_window = this.clone();
                menu.separator()
                    .item(PopupMenuItem::new("Open Folder…").icon(Icon::new(IconName::FolderOpen)).on_click(move |_, window, cx| {
                        _ = open.update(cx, |this, cx| this.prompt_open_folder(false, window, cx));
                    }))
                    .item(PopupMenuItem::new("Open Folder in New Window…").icon(Icon::new(IconName::AppWindow)).on_click(move |_, window, cx| {
                        _ = new_window.update(cx, |this, cx| this.prompt_open_folder(true, window, cx));
                    }))
            })
    }

    fn render_sidebar(&self, view: SidebarView, cx: &App) -> AnyElement {
        let content: AnyView = match view {
            SidebarView::Explorer => self.explorer.clone().into(),
            SidebarView::Search => self.search.clone().into(),
            SidebarView::Scm => self.scm.clone().into(),
        };
        div()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(content)
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_browsers(window, cx);
        let sidebar = AppState::get(cx).sidebar.clone();
        let side = Settings::get(cx).sidebar_position;
        let groups = self.render_layout(window, cx);

        let body = if sidebar.visible {
            let panel = resizable_panel()
                .size(px(sidebar.width))
                .size_range(px(160.)..px(640.))
                .flex_none()
                .child(self.render_sidebar(sidebar.view, cx));
            let left = side == SidebarSide::Left;
            let width_ix = if left { 0 } else { 1 };
            let group = h_resizable(if left { "body-left" } else { "body-right" }).on_resize(move |state, _, cx| {
                if let Some(width) = state.read(cx).sizes().get(width_ix).copied() {
                    AppState::update(cx, |s| s.sidebar.width = width.as_f32());
                }
            });
            if left {
                group.child(panel).child(groups).into_any_element()
            } else {
                group.child(groups).child(panel).into_any_element()
            }
        } else {
            groups
        };

        v_flex()
            .id("workspace")
            // Files dropped from the system's file manager open kept; a
            // folder opens as the session.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                for path in paths.paths() {
                    if path.is_dir() {
                        this.open_session(path.clone(), false, window, cx);
                        return;
                    }
                    this.open_file(path.clone(), false, window, cx);
                }
            }))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                if this.alt_held != event.modifiers.alt {
                    this.alt_held = event.modifiers.alt;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .on_action(cx.listener(|this, _: &FormatDocument, window, cx| this.format_active(window, cx)))
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| this.open_terminal(None, None, window, cx)))
            .on_action(cx.listener(|this, _: &NewBrowser, window, cx| this.open_browser(None, None, window, cx)))
            .on_action(cx.listener(|this, _: &SplitRight, _, cx| this.split(this.active_group, Side::Right, cx)))
            .on_action(cx.listener(|this, _: &SplitDown, _, cx| this.split(this.active_group, Side::Bottom, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(pane) = this.tree.active_tab(this.active_group) {
                    this.request_close_pane(pane, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CloseGroup, window, cx| this.request_close_group(this.active_group, window, cx)))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| this.prompt_open_folder(false, window, cx)))
            .on_action(cx.listener(|this, _: &OpenFiles, window, cx| this.prompt_open_files(window, cx)))
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| this.focus_group(Side::Left, window, cx)))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| this.focus_group(Side::Right, window, cx)))
            .on_action(cx.listener(|this, _: &FocusUp, window, cx| this.focus_group(Side::Top, window, cx)))
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| this.focus_group(Side::Bottom, window, cx)))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(-1, window, cx)))
            .on_action(cx.listener(|this, _: &FocusExplorer, window, cx| this.focus_view(SidebarView::Explorer, window, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.focus_view(SidebarView::Search, window, cx)))
            .on_action(cx.listener(|this, _: &ReplaceInFiles, window, cx| {
                AppState::update(cx, |state| {
                    state.sidebar.visible = true;
                    state.sidebar.view = SidebarView::Search;
                });
                SearchView::set_query(&this.search, "", true, window, cx);
                SearchView::focus(&this.search, window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusScm, window, cx| this.focus_view(SidebarView::Scm, window, cx)))
            .child(self.render_title_bar(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
