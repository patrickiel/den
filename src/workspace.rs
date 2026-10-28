//! The window: title bar, sidebar (Explorer / Search / Source Control) and
//! the groups.
//!
//! The workspace owns the layout tree (`layout.rs`), the panes in it and the
//! default groups (`defaults.rs`); `layout_view.rs` draws them. The whole
//! arrangement saves as a `LayoutState`: per session on every change, and
//! as a layout preset under a name.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::base::resize_handle;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    resizable::resize_handle_appearance,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde::{Deserialize, Serialize};

use crate::pane::AlertKind;
use crate::{
    CheckForUpdates, CloseGroup, CloseTab, FocusDown, FocusExplorer, FocusExtensions, FocusLeft,
    FocusRight, FocusScm, FocusSearch, FocusUp, FormatDocument, NewAgent, NewTerminal, NextTab,
    OpenDevServer, OpenFiles, OpenFolder, OpenFolderInNewWindow, OpenSettings, PrevTab,
    ReplaceInFiles, ResetLayout, SaveLayout, SplitDown, SplitRight, ToggleSoftWrap,
    backend::watch,
    defaults::{Defaults, GroupDefault, Kind},
    diff::DiffPanel,
    explorer::{Explorer, ExplorerEvent},
    extension_panel::ExtensionPanel,
    extension_view::ExtensionView,
    extensions_view::{ExtensionsEvent, ExtensionsView},
    float::FloatWindow,
    history::{Entry, History},
    layout::{Float, Node, NodeId, PaneId, Side, Tree},
    layout_view::{Dragged, Drop, DropHint, Flash, Hints, Nothing, Zones},
    overlays::{Maximized, PaintedRef},
    pane::{self, Pane, PaneRef, PaneState},
    panels::{FilePanel, SettingsPanel},
    repo::{Refresh, Repo},
    scm::{ScmEvent, ScmView},
    search::{IsDirty, SearchEvent, SearchView},
    settings::{
        AppState, LayoutPreset, Settings, SidebarSide, SidebarState, SidebarView, WindowState,
    },
    terminal::{Launch, TerminalPanel},
};
use gpui_kit::component::notification::{Notification, NotificationType};

/// The arrangement as saved: the tree, the panes in it and the defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutState {
    pub root: Node,
    #[serde(default)]
    pub panes: HashMap<PaneId, PaneState>,
    #[serde(default, deserialize_with = "crate::defaults::known")]
    pub defaults: Vec<GroupDefault>,
    #[serde(default)]
    pub active: Option<NodeId>,
    /// What the floating windows hold, and where they were.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub floats: Vec<Float>,
}

/// The taskbar's jump list: right-click den on the taskbar
/// for its recent folders, each opening in a new window. Entries removed
/// there leave the title bar's list too. GPUI fails the whole list without
/// a task, so there is New Window too, which starts den as `--dock-action 0`.
pub(crate) fn sync_jump_list(cx: &mut App) {
    let entries: Vec<smallvec::SmallVec<[PathBuf; 2]>> = AppState::get(cx)
        .recent
        .iter()
        .take(12)
        .map(|path| smallvec::smallvec![path.clone()])
        .collect();
    let removed = cx.update_jump_list(
        vec![MenuItem::action("New Window", crate::OpenFolder)],
        entries,
    );
    cx.spawn(async move |cx| {
        let removed = removed.await;
        if removed.is_empty() {
            return;
        }
        _ = cx.update(|cx| {
            let keys: Vec<String> = removed
                .iter()
                .flatten()
                .map(|p| crate::repo::key(p))
                .collect();
            AppState::update(cx, |state| {
                state
                    .recent
                    .retain(|p| !keys.contains(&crate::repo::key(p)))
            });
        });
    })
    .detach();
}

/// Each folder by its name, with as many parents as it takes to tell it from
/// the others: `root` first, then the recent folders other than it.
fn recent_names(root: &std::path::Path, cx: &App) -> (Vec<String>, Vec<PathBuf>) {
    let recent: Vec<PathBuf> = AppState::get(cx)
        .recent
        .iter()
        .filter(|p| crate::repo::key(p) != crate::repo::key(root))
        .cloned()
        .collect();
    let all: Vec<PathBuf> = std::iter::once(root.to_path_buf())
        .chain(recent.iter().cloned())
        .collect();
    (crate::settings::unique_names(&all), recent)
}

/// `root`'s name for the title bar: with parents enough to tell it from the
/// other recent folders, else the folder itself.
fn session_name(root: &std::path::Path, cx: &App) -> String {
    recent_names(root, cx)
        .0
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            root.file_name().map_or_else(
                || root.display().to_string(),
                |name| name.to_string_lossy().to_string(),
            )
        })
}

/// The OS window title for `root`, as the taskbar and window switcher show it.
pub fn window_title(root: &std::path::Path, cx: &App) -> String {
    format!("den - {}", session_name(root, cx))
}

/// The session switcher's menu, from the recent folders as they are now.
fn recent_menu(
    this: WeakEntity<Workspace>,
    root: PathBuf,
    menu: PopupMenu,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let (names, recent) = recent_names(&root, cx);
    let own = cx.weak_entity();
    let mut menu = menu
        .label("RECENT FOLDERS")
        .max_h(px(480.))
        .scrollable(true);
    if recent.is_empty() {
        menu = menu.item(PopupMenuItem::new("No other folders yet").disabled(true));
    }
    for (ix, (path, label)) in recent.iter().zip(names.iter().skip(1)).enumerate() {
        let this = this.clone();
        let path = path.clone();
        let (label, full) = (
            SharedString::from(label.clone()),
            SharedString::from(path.display().to_string()),
        );
        let forget = path.clone();
        let rebuild = (this.clone(), root.clone(), own.clone());
        let item = PopupMenuItem::element(move |_, _| {
            let full = full.clone();
            let forget = forget.clone();
            let (this, root, own) = rebuild.clone();
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
                        .group_hover(SharedString::from(format!("recent-{ix}")), |this| {
                            this.visible()
                        })
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
                            let (this, root) = (this.clone(), root.clone());
                            _ = own.update(cx, |menu, cx| {
                                menu.rebuild(window, cx, |menu, _, cx| {
                                    recent_menu(this, root, menu, cx)
                                })
                            });
                            window.refresh();
                        }),
                )
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
                })
        });
        // Opens the folder in a window of its own, leaving this one be.
        menu = menu.item(
            item.icon(Icon::new(IconName::Folder))
                .on_click(move |_, window, cx| {
                    _ = this.update(cx, |this, cx| {
                        this.open_session(path.clone(), true, window, cx)
                    });
                }),
        );
    }
    let open = this.clone();
    let new_window = this;
    menu.separator()
        .item(
            PopupMenuItem::new("Open Folder…")
                .icon(Icon::new(IconName::FolderOpen))
                .on_click(move |_, window, cx| {
                    _ = open.update(cx, |this, cx| this.prompt_open_folder(false, window, cx));
                }),
        )
        .item(
            PopupMenuItem::new("Open Folder in New Window…")
                .icon(Icon::new(IconName::AppWindow))
                .on_click(move |_, window, cx| {
                    _ = new_window.update(cx, |this, cx| this.prompt_open_folder(true, window, cx));
                }),
        )
}

/// A layout preset being dragged to another place in the Layouts menu.
#[derive(Clone)]
struct LayoutPresetDrag {
    ix: usize,
}

/// The Layouts menu, from the presets as they are now.
fn layouts_menu(
    this: WeakEntity<Workspace>,
    mut menu: PopupMenu,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let count = AppState::get(cx).presets.len();
    let own = cx.weak_entity();
    menu = menu.label("LAYOUTS");
    if count == 0 {
        menu = menu.item(PopupMenuItem::new("No saved layouts").disabled(true));
    }
    // As in den: drag a layout to reorder, its x (on hover) deletes it.
    // Each row reads its layout by position as it draws, so the open
    // menu follows a move; a delete builds the menu again.
    for ix in 0..count {
        let this = this.clone();
        let (row_this, row_menu) = (this.clone(), own.clone());
        menu = menu.item(
            PopupMenuItem::element(move |_, cx| {
                layout_preset_row(ix, row_this.clone(), row_menu.clone(), cx)
            })
            .icon(Icon::new(IconName::LayoutTemplate))
            .on_click(move |_, window, cx| {
                let Some(name) = AppState::get(cx).presets.get(ix).map(|p| p.name.clone()) else {
                    return;
                };
                _ = this.update(cx, |this, cx| this.load_preset(&name, window, cx));
            }),
        );
    }
    let save = this.clone();
    let reset = this;
    menu.separator()
        .item(
            PopupMenuItem::new("Save Layout…")
                .icon(Icon::new(IconName::Save))
                .on_click(move |_, window, cx| {
                    _ = save.update(cx, |this, cx| this.prompt_save_preset(window, cx));
                }),
        )
        .item(
            PopupMenuItem::new("Reset Layout")
                .icon(Icon::new(IconName::RotateCcw))
                .on_click(move |_, _, cx| {
                    _ = reset.update(cx, |this, cx| this.reset_layout(cx));
                }),
        )
}

/// A row of the Layouts menu: preset `ix` (by position, read as it draws),
/// draggable onto another row, with an x on hover that deletes it.
fn layout_preset_row(
    ix: usize,
    this: WeakEntity<Workspace>,
    menu: WeakEntity<PopupMenu>,
    cx: &App,
) -> AnyElement {
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
                    let this = this.clone();
                    _ = menu.update(cx, |menu, cx| {
                        menu.rebuild(window, cx, |menu, _, cx| layouts_menu(this, menu, cx))
                    });
                    window.refresh();
                }),
        )
        .on_drag(LayoutPresetDrag { ix }, move |_, _, _, cx| {
            cx.new(|_| crate::layout_view::DragLabel(label.clone().into()))
        })
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
    /// Under the workspace's action handlers, for actions with no tab to
    /// take them (see `action_target`).
    focus_handle: FocusHandle,
    pub(crate) tree: Tree,
    pub(crate) panes: HashMap<PaneId, PaneRef>,
    /// Each pane's events and its focus.
    pane_subscriptions: HashMap<PaneId, [Subscription; 2]>,
    pub(crate) defaults: Defaults,
    pub(crate) active_group: NodeId,
    /// The preview tab (italic), replaced by the next file opened as a preview.
    pub(crate) preview: Option<PaneId>,
    /// Where a dragged tab or group would land.
    pub(crate) drop_hint: Option<DropHint>,
    /// The modifiers down: Alt turns the split buttons to splitting down.
    pub(crate) held: Modifiers,
    /// The numbers shown over the groups or the active group's tabs while
    /// the modifiers of their commands are held (Ctrl+1…8 numbers the groups).
    pub(crate) hints: Option<Hints>,
    /// When `hints` last changed: the group numbers fade in from then, or
    /// out, over `HINT_FADE`.
    hints_at: std::time::Instant,
    /// The numbers fading out, once their modifiers are let go.
    hints_gone: Option<Hints>,
    /// Counts the changes of the modifiers: the numbers come after a short
    /// hold, when the modifiers did not change meanwhile.
    hint_seq: u64,
    /// The group or tab the keys last went to, and when: its border flashes.
    flash: Option<(Flash, std::time::Instant)>,
    /// A group drawn over its window's body, sidebar and all (`overlays.rs`).
    pub(crate) maximized: Option<Maximized>,
    /// Toggle Full Screen (F11): this group maximized in its window, which is
    /// full screen without its title bar; and whether that window was full
    /// screen before.
    full_screen: Option<(NodeId, bool)>,
    /// Where the windows' bodies and the maximized group's slot were painted.
    pub(crate) painted: PaintedRef,
    /// Tabs whose program wanted the user while they looked elsewhere.
    pub(crate) attention: std::collections::HashSet<PaneId>,
    /// The main window, and each floating window by its float's id.
    window: AnyWindowHandle,
    pub(crate) float_windows: HashMap<u64, AnyWindowHandle>,
    /// The group last active in each window (`None` for the main one), for
    /// coming back to that window.
    last_active: HashMap<Option<u64>, NodeId>,
    /// The tab or group being dragged, for a drop outside every window.
    pub(crate) dragging: Option<Dragged>,
    /// Where each window drew its drop zones, for a drag from another one.
    pub(crate) zones: Zones,
    /// The size the maximized group has when full, which the tiles' grid is
    /// laid out for (see `tile_columns`), and how many then go across.
    pub(crate) tile_area: std::cell::Cell<Option<Size<Pixels>>>,
    pub(crate) tile_columns: std::cell::Cell<usize>,
    /// A drag from another window over this float's window (`None` for the
    /// main one): where the pointer is in it.
    pub(crate) remote_drag: Option<(Option<u64>, Point<Pixels>)>,
    /// The tab last right-clicked, for its strip's context menu.
    pub(crate) tab_menu: Option<PaneId>,
    /// The float just made, whose window comes to the front when it opens
    /// (those coming back with the session do not).
    activate_float: Option<u64>,
    /// The tabs looked at, for the mouse's back and forward buttons.
    history: History,
    /// Going back or forward: what is shown now is no new visit.
    navigating: bool,
    /// The tabs by when they were opened or last had focus, the latest last,
    /// for closing the stalest file past the tab limit.
    shown: Vec<PaneId>,
    explorer: Entity<Explorer>,
    search: Entity<SearchView>,
    scm: Entity<ScmView>,
    extensions: Entity<ExtensionsView>,
    /// The extension views opened in the sidebar (each made the first time
    /// it shows), and which of them the sidebar shows: extension and view id.
    sidebar_views: Vec<Entity<ExtensionView>>,
    sidebar_shown: Option<(String, String)>,
    /// This window's sidebar: shown or not, which view, how wide. Its own,
    /// not shared with other windows; kept with the session.
    sidebar: SidebarState,
    repo: Entity<Repo>,
    /// The project's dev servers, likeliest first (`backend::dev_server`);
    /// the first is the tab strips' button.
    pub(crate) dev_servers: Vec<crate::backend::dev_server::DevServer>,
    _dev_server_task: Option<Task<()>>,
    _watcher: Option<watch::Watcher>,
    _watch_task: Option<Task<()>>,
    _save_task: Option<Task<()>>,
    /// The main window as last seen settled, kept with the session.
    window_state: Option<WindowState>,
    _window_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    /// Tells other dens this folder is open here (see `settings::FolderLock`).
    _folder_lock: Option<crate::settings::FolderLock>,
    /// The active file as last told to the extensions.
    active_file: Option<PathBuf>,
    /// The folder's name in the title bar (see [`session_name`]), kept up
    /// with the recent folders.
    session_name: String,
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        AppState::update(cx, |state| state.opened(&root));
        sync_jump_list(cx);
        // Closing the window saves the session, asking first about unsaved changes.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak.upgrade().is_none_or(|this| {
                this.update(cx, |this, cx| this.request_close_window(window, cx))
            })
        });
        let session = AppState::get(cx).session(&root);
        let repo = cx.new(|cx| Repo::new(root.clone(), !session.scm_whole_repo, cx));
        let explorer =
            cx.new(|cx| Explorer::new(root.clone(), session.expanded.clone(), repo.clone(), cx));
        let weak = cx.weak_entity();
        let is_dirty: IsDirty = std::rc::Rc::new(move |path, cx| {
            weak.upgrade()
                .is_some_and(|this| this.read(cx).is_dirty_file(path, cx))
        });
        let search = cx.new(|cx| SearchView::new(root.clone(), is_dirty, window, cx));
        let scm = cx.new(|cx| ScmView::new(repo.clone(), window, cx));
        let extensions = cx.new(|cx| ExtensionsView::new(window, cx));
        let sidebar_views = session
            .sidebar_views
            .iter()
            .map(|data| {
                let mut data = data.clone();
                data["root"] = serde_json::json!(root);
                cx.new(|cx| {
                    let mut view = ExtensionView::restore(&data, cx);
                    view.in_sidebar = true;
                    view
                })
            })
            .collect();
        if !session.commit_message.is_empty() {
            let draft = session.commit_message.clone();
            scm.update(cx, |scm, cx| scm.set_draft(draft, window, cx));
        }

        let _subscriptions = vec![
            cx.subscribe_in(
                &extensions,
                window,
                |this, _, event: &ExtensionsEvent, window, cx| match event {
                    ExtensionsEvent::Open(path) => this.open_file(path.clone(), false, window, cx),
                    ExtensionsEvent::Show(id) => this.open_extension(id.clone(), None, window, cx),
                    ExtensionsEvent::Preview(listing) => {
                        this.open_extension(listing.id.clone(), Some(listing.clone()), window, cx)
                    }
                },
            ),
            cx.subscribe_in(
                &explorer,
                window,
                |this, _, event: &ExplorerEvent, window, cx| match event {
                    ExplorerEvent::Open { path, preview } => {
                        this.open_file(path.clone(), *preview, window, cx)
                    }
                    ExplorerEvent::ExpandedChanged => this.schedule_save(cx),
                    ExplorerEvent::OpenTerminal(dir) => this.open_shell_in(dir.clone(), window, cx),
                },
            ),
            cx.subscribe_in(
                &search,
                window,
                |this, _, event: &SearchEvent, window, cx| match event {
                    SearchEvent::Open {
                        path,
                        line,
                        column,
                        focus,
                    } => {
                        this.open_file(path.clone(), !*focus, window, cx);
                        this.go_to(path, *line, *column, *focus, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &scm,
                window,
                |this, _, event: &ScmEvent, window, cx| match event {
                    ScmEvent::Open(path) => this.open_file(path.clone(), false, window, cx),
                    ScmEvent::Diff { file, staged } => {
                        this.open_diff(file.path.clone(), file.rel.clone(), *staged, window, cx)
                    }
                    ScmEvent::CommitDiff { rel, commit } => {
                        this.open_commit_diff(rel.clone(), commit.clone(), window, cx)
                    }
                },
            ),
            // Coming back to the window, git may have changed underneath.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.repo.update(cx, |repo, cx| repo.refresh(cx));
                    this.window_activated(None, cx);
                }
            }),
            cx.observe_window_bounds(window, |this, window, cx| this.window_changed(window, cx)),
            cx.observe_global::<Settings>(|_, cx| cx.notify()),
            // The recent folders decide how much of the path the title shows.
            cx.observe_global::<AppState>(|this, cx| {
                this.session_name = session_name(&this.root, cx);
                cx.notify();
            }),
            // The title bar shows the extensions' buttons.
            cx.observe_global::<crate::extensions::Extensions>(|_, cx| cx.notify()),
            // The Source Control button counts the changes.
            cx.observe(&repo, |_, _, cx| cx.notify()),
            cx.on_app_quit(|this, cx| {
                crate::backend::ai::stop();
                this.save_session(cx);
                crate::settings::flush(cx);
                async {}
            }),
        ];

        let tree = Tree::new();
        let active_group = tree.root.id();
        let session_name = session_name(&root, cx);
        let mut this = Self {
            _folder_lock: crate::settings::FolderLock::new(&root),
            root,
            tree,
            panes: HashMap::new(),
            pane_subscriptions: HashMap::new(),
            defaults: Defaults::default(),
            active_group,
            preview: None,
            drop_hint: None,
            held: Modifiers::none(),
            hints: None,
            hints_at: std::time::Instant::now(),
            hints_gone: None,
            hint_seq: 0,
            flash: None,
            maximized: None,
            full_screen: None,
            painted: PaintedRef::default(),
            attention: Default::default(),
            window: window.window_handle(),
            float_windows: HashMap::new(),
            focus_handle: cx.focus_handle(),
            last_active: HashMap::new(),
            dragging: None,
            zones: Zones::default(),
            tile_area: Default::default(),
            tile_columns: std::cell::Cell::new(1),
            remote_drag: None,
            tab_menu: None,
            activate_float: None,
            history: History::default(),
            navigating: false,
            shown: Vec::new(),
            explorer,
            search,
            scm,
            extensions,
            sidebar_views,
            sidebar_shown: session.sidebar_shown.clone(),
            sidebar: session
                .sidebar
                .clone()
                .unwrap_or_else(|| AppState::get(cx).sidebar.clone()),
            repo,
            dev_servers: Vec::new(),
            _dev_server_task: None,
            _watcher: None,
            _watch_task: None,
            _save_task: None,
            window_state: session.window,
            _window_task: None,
            _subscriptions,
            active_file: None,
            session_name,
        };
        this.start_watching(window, cx);
        this.detect_dev_servers(cx);
        // A window turned to this folder keeps its bounds: note them.
        this.window_changed(window, cx);
        // Once per run, a little after start: is there a newer den, or a
        // newer version of an installed extension?
        static UPDATE_CHECKED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !UPDATE_CHECKED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            cx.spawn_in(window, async move |_, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(10))
                    .await;
                _ = cx.update(|window, cx| {
                    crate::update::check_in_window(false, window, cx);
                    crate::extensions::check_for_updates(cx);
                });
            })
            .detach();
        }
        let loaded = session.layout.map_or(Ok(()), |layout| {
            this.load_state(crate::saved_layout::restore(layout, &this.root), window, cx)
        });
        if let Err(err) = loaded {
            eprintln!("den: the saved layout could not be read, starting fresh: {err}");
        }
        // The active tab takes the keyboard from the start: the keys and the
        // menu bar reach the workspace through the focused element (nothing
        // focused, macOS shows the menu bar's items disabled).
        let focus = this.action_target(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        this
    }

    /// Where the main window's actions go when nothing in it has the focus:
    /// the active tab, or with no tab the workspace's own handle, both under
    /// its handlers.
    pub(crate) fn action_target(&self, cx: &App) -> FocusHandle {
        self.window_focus(None, cx)
            .unwrap_or_else(|| self.focus_handle.clone())
    }

    // -- State ---------------------------------------------------------------

    /// Replace the arrangement with a saved one, making its panes anew.
    pub fn load_state(
        &mut self,
        value: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let state: LayoutState = serde_json::from_value(value)?;
        let mut tree = Tree::with_floats(state.root, state.floats);
        let mut panes = HashMap::new();
        for id in tree.panes() {
            match state.panes.get(&id) {
                // Settings are a dialog now, and pages open in the system's
                // browser; such a tab in an older layout goes.
                Some(saved) if saved.kind == crate::panels::SETTINGS || saved.kind == "Browser" => {
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
        self.maximized = None;
        self.last_active.clear();
        self.history.clear();
        self.note_visit();
        self.sync_floats(cx);
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
            floats: self.tree.floats.clone(),
        }
    }

    fn reset_layout(&mut self, cx: &mut Context<Self>) {
        self.panes.clear();
        self.pane_subscriptions.clear();
        self.tree = Tree::new();
        self.defaults.clear();
        self.active_group = self.tree.root.id();
        self.history.clear();
        self.changed(cx);
    }

    fn track(&mut self, id: PaneId, pane: PaneRef, window: &mut Window, cx: &mut Context<Self>) {
        let focused = cx.on_focus_in(&pane.focus_handle(cx), window, move |this, _, _| {
            this.shown.retain(|p| *p != id);
            this.shown.push(id);
        });
        self.pane_subscriptions
            .insert(id, [pane.subscribe(id, window, cx), focused]);
        self.panes.insert(id, pane);
    }

    /// After every edit of the arrangement: redraw, open or close floating
    /// windows to match, and save.
    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
        // The maximized group closed: nothing is maximized.
        if self
            .maximized_group()
            .is_some_and(|group| !self.tree.find(group).is_some_and(Node::is_group))
        {
            self.maximized = None;
        }
        self.note_visit();
        cx.notify();
        self.sync_floats(cx);
        self.schedule_save(cx);
    }

    // -- Groups and tabs -----------------------------------------------------

    pub(crate) fn set_active_group(&mut self, group: NodeId, cx: &mut Context<Self>) {
        self.seen(group, cx);
        if self.tree.find(group).is_some_and(Node::is_group) {
            self.last_active.insert(self.tree.float_of(group), group);
            // Another group of the maximized group's window takes over: the
            // maximized one shrinks back, so what is active can be seen.
            if let Some(maximized) = self.maximized_group()
                && maximized != group
                && self.tree.float_of(maximized) == self.tree.float_of(group)
            {
                self.restore_group(cx);
            }
        }
        if self.active_group != group && self.tree.find(group).is_some_and(Node::is_group) {
            self.active_group = group;
            self.defaults.activate(group);
            cx.notify();
        }
        self.note_visit();
    }

    /// The tab shown in the active group, for going back and forward.
    fn note_visit(&mut self) {
        let Some(pane) = self.tree.active_tab(self.active_group) else {
            return;
        };
        if self.navigating {
            self.history.arrive(pane);
        } else {
            self.history.visit(pane);
        }
    }

    /// Show the tab looked at before (`back`) or after, as the mouse's side
    /// buttons do. A closed file opens again, as the preview.
    fn navigate(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.history.step(back, |entry| match entry {
            Entry::Pane(pane) => self.panes.contains_key(pane),
            Entry::File(path) => path.is_file(),
        }) else {
            return;
        };
        self.navigating = true;
        match entry {
            Entry::Pane(pane) => self.show_pane(pane, true, window, cx),
            Entry::File(path) => {
                self.open_file(path, true, window, cx);
                if let Some(pane) = self.tree.active_tab(self.active_group) {
                    self.focus_pane(pane, window, cx);
                }
            }
        }
        self.navigating = false;
    }

    /// Where a new tab of `kind` goes: a default group of that kind, else the
    /// active group.
    pub(crate) fn target_group(&self, kind: Kind) -> NodeId {
        self.defaults
            .target(&self.tree, kind, Some(self.active_group))
            .unwrap_or(self.active_group)
    }

    /// Put a new pane in `group` and show it.
    pub(crate) fn add_pane<T: Pane>(
        &mut self,
        pane: Entity<T>,
        group: NodeId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.tree.mint();
        let pane: PaneRef = std::rc::Rc::new(pane);
        self.track(id, pane, window, cx);
        self.shown.push(id);
        self.tree.add_tab(group, id, None, true);
        self.set_active_group(group, cx);
        if focus {
            self.focus_pane(id, window, cx);
        }
        self.changed(cx);
    }

    pub(crate) fn show_pane(
        &mut self,
        pane: PaneId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tree.activate(pane);
        self.attention.remove(&pane);
        if let Some(group) = self.tree.group_of(pane) {
            self.set_active_group(group, cx);
        }
        if focus {
            self.focus_pane(pane, window, cx);
        }
        self.changed(cx);
    }

    /// Focus a pane in the window it is in: `window` when that is it, else
    /// its own, brought to the front.
    pub(crate) fn focus_pane(&self, pane: PaneId, window: &mut Window, cx: &mut App) {
        let Some(focus) = self.panes.get(&pane).map(|p| p.focus_handle(cx)) else {
            return;
        };
        let target = self.window_of(
            self.tree
                .group_of(pane)
                .and_then(|group| self.tree.float_of(group)),
        );
        if target == window.window_handle() {
            focus.focus(window, cx);
        } else {
            // After this update: that window may be the one being updated.
            cx.defer(move |cx| {
                _ = target.update(cx, |_, window, cx| {
                    window.activate_window();
                    focus.focus(window, cx);
                });
            });
        }
    }

    /// The window of the main tree (`None`) or of a float.
    fn window_of(&self, float: Option<u64>) -> AnyWindowHandle {
        float
            .and_then(|id| self.float_windows.get(&id).copied())
            .unwrap_or(self.window)
    }

    /// Close a tab; its group stays, empty or not.
    pub(crate) fn close_pane(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.preview == Some(pane) {
            self.preview = None;
        }
        self.forget_pane(pane, cx);
        self.changed(cx);
    }

    /// A tab goes: the history keeps it as its file, if it has one.
    fn forget_pane(&mut self, pane: PaneId, cx: &App) {
        let file = self
            .pane_as::<FilePanel>(pane)
            .and_then(|file| file.read(cx).path().map(std::path::Path::to_path_buf));
        self.panes.remove(&pane);
        self.pane_subscriptions.remove(&pane);
        self.shown.retain(|p| *p != pane);
        self.tree.remove_tab(pane);
        self.history.closed(pane, file);
    }

    pub(crate) fn close_group(&mut self, group: NodeId, cx: &mut Context<Self>) {
        for pane in self.tree.tabs(group).to_vec() {
            self.forget_pane(pane, cx);
        }
        self.detach(group);
        self.changed(cx);
    }

    /// Take `node` out of the tree, with what follows from that: the
    /// defaults follow collapsed splits, and the active group stays a group.
    /// False for the root, which has nowhere to go.
    fn detach(&mut self, node: NodeId) -> bool {
        let detached = self.tree.detach(node);
        if let Some((_, remap)) = &detached {
            self.defaults.prune(&self.tree, remap);
        }
        self.fix_active_group();
        detached.is_some()
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
    pub(crate) fn pane_alert(
        &mut self,
        pane: PaneId,
        kind: AlertKind,
        message: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = Settings::get(cx).clone();
        if !settings.notifications {
            return;
        }
        // The window the tab is in: this one, or a floating one.
        let float = self
            .tree
            .group_of(pane)
            .and_then(|group| self.tree.float_of(group));
        let target = self.window_of(float);
        let focused = if target == window.window_handle() {
            window.is_window_active()
        } else {
            target
                .update(cx, |_, window, _| window.is_window_active())
                .unwrap_or(false)
        };
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
            if target == window.window_handle() {
                window.request_attention();
            } else {
                _ = target.update(cx, |_, window, _| window.request_attention());
            }
        }
        if settings.notify_toast {
            let Some(handle) = self.panes.get(&pane).cloned() else {
                return;
            };
            let name = handle.label(cx);
            let status = match kind {
                AlertKind::Done => "finished",
                AlertKind::Input => "needs input",
                AlertKind::Attention => "needs attention",
            };
            let title = self
                .pane_as::<TerminalPanel>(pane)
                .and_then(|t| t.read(cx).title_text());
            let body: Vec<String> = [
                message.map(|m| m.trim().to_string()),
                title.filter(|t| t.as_str() != name.as_ref()),
            ]
            .into_iter()
            .flatten()
            .filter(|text| !text.is_empty())
            .collect();
            let this = cx.weak_entity();
            let note = Notification::new()
                .title(format!("{name} {status}"))
                .message(body.join(" — "))
                .with_type(if kind == AlertKind::Done {
                    NotificationType::Success
                } else {
                    NotificationType::Warning
                })
                .on_click(move |_, window, cx| {
                    _ = this.update(cx, |this, cx| this.show_pane(pane, true, window, cx));
                });
            crate::toast::push(window, note, cx);
        }
    }

    /// Format Document on the active group's file tab.
    pub(crate) fn format_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self
            .tree
            .active_tab(self.active_group)
            .and_then(|pane| self.pane_as::<FilePanel>(pane))
        {
            file.update(cx, |file, cx| file.format(false, window, cx));
        }
    }

    fn fix_active_group(&mut self) {
        if !self
            .tree
            .find(self.active_group)
            .is_some_and(Node::is_group)
        {
            self.active_group = self
                .tree
                .groups()
                .first()
                .copied()
                .unwrap_or(self.tree.root.id());
        }
    }

    /// A new empty group beside `node` (a group or the root).
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

    pub(crate) fn set_default(
        &mut self,
        group: NodeId,
        kind: Option<Kind>,
        cx: &mut Context<Self>,
    ) {
        self.defaults.set(group, kind);
        self.changed(cx);
    }

    /// Move a group beside `target`.
    pub(crate) fn move_node(
        &mut self,
        node: NodeId,
        target: NodeId,
        side: Side,
        cx: &mut Context<Self>,
    ) {
        if let Some(remap) = self.tree.move_node(node, target, side) {
            self.defaults.prune(&self.tree, &remap);
            if let Some(group) = self.tree.groups_under(node).first() {
                self.set_active_group(*group, cx);
            }
            self.changed(cx);
        }
    }

    /// Move a tab into `group` (at `ix`), or into a new group beside `target`
    /// when `side` is given. A group the tab leaves empty goes with it,
    /// unless the tab was split off beside it: a group's only tab dropped on
    /// its own side splits it, leaving it empty.
    pub(crate) fn move_tab(
        &mut self,
        pane: PaneId,
        target: NodeId,
        side: Option<Side>,
        ix: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.tree.group_of(pane) else {
            return;
        };
        let group = match side {
            None => target,
            Some(side) => match self.tree.split(target, side) {
                Some(group) => group,
                None => return,
            },
        };
        self.tree.move_tab(pane, group, ix);
        if from != group && from != target && self.tree.tabs(from).is_empty() {
            self.detach(from);
        }
        self.set_active_group(group, cx);
        self.changed(cx);
    }

    // -- Floating windows ----------------------------------------------------

    /// Open a window for each float without one and close those whose float
    /// is gone, once the current update is over (the window to close may be
    /// the one being updated).
    fn sync_floats(&mut self, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let Some(this) = this.upgrade() else { return };
            let (gone, missing) = this.update(cx, |this, _| {
                let wanted: Vec<u64> = this.tree.floats.iter().map(|f| f.id).collect();
                let gone: Vec<u64> = this
                    .float_windows
                    .keys()
                    .copied()
                    .filter(|id| !wanted.contains(id))
                    .collect();
                let gone: Vec<AnyWindowHandle> = gone
                    .iter()
                    .filter_map(|id| this.float_windows.remove(id))
                    .collect();
                let missing: Vec<(u64, Option<[f32; 4]>, bool)> = this
                    .tree
                    .floats
                    .iter()
                    .filter(|f| !this.float_windows.contains_key(&f.id))
                    .map(|f| (f.id, f.bounds, this.activate_float == Some(f.id)))
                    .collect();
                (gone, missing)
            });
            for (id, bounds, activate) in missing {
                if let Some(window) = open_float_window(this.clone(), id, bounds, activate, cx) {
                    this.update(cx, |this, _| {
                        this.float_windows.insert(id, window);
                        if activate {
                            this.activate_float = None;
                        }
                    });
                }
            }
            // Then the windows to close.
            for window in gone {
                _ = window.update(cx, |_, window, _| window.remove_window());
            }
        });
    }

    /// Move a group (or a window's whole root) into a new window: at `bounds`
    /// (on screen), or centred.
    pub(crate) fn float_node(
        &mut self,
        node: NodeId,
        bounds: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        // The main window's groups, all empty, have nothing to take along.
        if node == self.tree.root.id()
            && self
                .tree
                .root
                .groups()
                .iter()
                .all(|group| self.tree.tabs(*group).is_empty())
        {
            return;
        }
        let Some((float, remap)) = self.tree.float_out(node, bounds.map(screen_rect)) else {
            return;
        };
        self.defaults.prune(&self.tree, &remap);
        self.activate_float = Some(float);
        self.fix_active_group();
        if let Some(group) = self.tree.groups_under(node).first() {
            self.set_active_group(*group, cx);
        }
        self.changed(cx);
    }

    /// Move a tab into a new window, in a group of its own.
    pub(crate) fn float_tab(
        &mut self,
        pane: PaneId,
        bounds: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.tree.group_of(pane) else {
            return;
        };
        // Alone in a window of its own already.
        if self.tree.tabs(from).len() == 1
            && self.tree.float_of(from).is_some()
            && self.tree.is_root(from)
        {
            return;
        }
        let (float, group) = self.tree.new_float(bounds.map(screen_rect));
        self.activate_float = Some(float);
        self.move_tab(pane, group, None, None, cx);
    }

    /// Float `id`'s window closes: what it holds goes back into the main
    /// window, beside everything (in place of a lone empty group there).
    /// Closing it drops empty groups, which have nothing to bring back;
    /// `keep_empty` (its Move into Main Window button) moves them all the same.
    pub(crate) fn dock_float(&mut self, id: u64, keep_empty: bool, cx: &mut Context<Self>) {
        let Some(root) = self.tree.window_root(Some(id)) else {
            return;
        };
        let root_id = root.id();
        if !keep_empty
            && root
                .groups()
                .iter()
                .all(|group| self.tree.tabs(*group).is_empty())
        {
            self.detach(root_id);
            return self.changed(cx);
        }
        let main = self.tree.root.id();
        let empty_main = matches!(&self.tree.root, Node::Group { tabs, .. } if tabs.is_empty());
        self.move_node(root_id, main, Side::Right, cx);
        if empty_main {
            self.close_group(main, cx);
        }
    }

    /// A floating window moved or changed size: kept for the next session.
    pub(crate) fn float_moved(&mut self, id: u64, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.tree.set_float_bounds(id, screen_rect(bounds));
        self.schedule_save(cx);
    }

    /// A window of the session came to the front: its group is the active
    /// one, as it was when the user left it.
    pub(crate) fn window_activated(&mut self, float: Option<u64>, cx: &mut Context<Self>) {
        if self.tree.float_of(self.active_group) != float {
            let remembered = self.last_active.get(&float).copied().filter(|group| {
                self.tree.find(*group).is_some_and(Node::is_group)
                    && self.tree.float_of(*group) == float
            });
            let group = remembered.or_else(|| {
                self.tree
                    .window_root(float)
                    .and_then(|root| root.groups().first().copied())
            });
            if let Some(group) = group {
                self.set_active_group(group, cx);
            }
        }
        self.seen(self.active_group, cx);
    }

    /// What takes the keyboard in a window: its active group's shown tab.
    pub(crate) fn window_focus(&self, float: Option<u64>, cx: &App) -> Option<FocusHandle> {
        let group = if self.tree.float_of(self.active_group) == float {
            self.active_group
        } else {
            *self.tree.window_root(float)?.groups().first()?
        };
        let pane = self.tree.active_tab(group)?;
        Some(self.panes.get(&pane)?.focus_handle(cx))
    }

    /// The session's window under `at` (on screen, see `on_screen`), other
    /// than float `from`'s (or the main one): its float, and where `at` is in
    /// it. Floating windows first: they tend to be over the main one.
    pub(crate) fn window_under(
        &self,
        from: Option<u64>,
        at: Point<f32>,
        cx: &mut App,
    ) -> Option<(Option<u64>, Point<Pixels>)> {
        let others: Vec<Option<u64>> = self
            .float_windows
            .keys()
            .copied()
            .map(Some)
            .chain([None])
            .filter(|w| *w != from)
            .collect();
        others.into_iter().find_map(|float| {
            let (origin, scale, size) = self
                .window_of(float)
                .update(cx, |_, window, _| {
                    (
                        on_screen(window, Point::default()),
                        window.scale_factor(),
                        window.viewport_size(),
                    )
                })
                .ok()?;
            let local = point(px((at.x - origin.x) / scale), px((at.y - origin.y) / scale));
            Bounds::new(Point::default(), size)
                .contains(&local)
                .then_some((float, local))
        })
    }

    /// A drag let go outside the window it started in (float `from`'s, or
    /// the main one): over another window of the session, what it carries
    /// lands where that window showed it would; elsewhere, it moves into a
    /// new window where it was let go.
    pub(crate) fn drag_released(
        &mut self,
        from: Option<u64>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dragged) = self.dragging.take() else {
            return;
        };
        if self.remote_drag.take().is_some() {
            cx.notify();
        }
        if Bounds::new(Point::default(), window.viewport_size()).contains(&position) {
            return;
        }
        let hint = self.drop_hint.take();
        let at = on_screen(window, position);
        match self.window_under(from, at, cx) {
            Some((float, _)) => {
                let Some(hint) = hint.filter(|hint| self.tree.float_of(hint.target) == float)
                else {
                    return cx.notify();
                };
                match (dragged, hint.drop) {
                    (Dragged::Tab(pane), Drop::Center) => {
                        self.move_tab(pane, hint.target, None, None, cx)
                    }
                    (Dragged::Tab(pane), Drop::Side(side)) => {
                        self.move_tab(pane, hint.target, Some(side), None, cx)
                    }
                    (Dragged::Node(node), Drop::Side(side)) => {
                        self.move_node(node, hint.target, side, cx)
                    }
                    (Dragged::Tab(pane), Drop::Tab(ix)) => {
                        self.move_tab(pane, hint.target, None, ix, cx)
                    }
                    (Dragged::Node(_), Drop::Center | Drop::Tab(_)) => cx.notify(),
                }
            }
            None => {
                // The new window opens with its tab strip under the pointer,
                // placed in the pixels of the monitor there (each monitor's
                // windows are placed by its own scale).
                let scale = monitor_scale(at).unwrap_or_else(|| window.scale_factor());
                let pointer = point(px(at.x / scale), px(at.y / scale));
                let current = window.bounds().size;
                let size = size(
                    px(960f32.min(current.width.as_f32())),
                    px(640f32.min(current.height.as_f32())),
                );
                let bounds = Bounds::new(pointer - FLOAT_GRAB, size);
                match dragged {
                    Dragged::Node(node) => self.float_node(node, Some(bounds), cx),
                    Dragged::Tab(pane) => self.float_tab(pane, Some(bounds), cx),
                }
            }
        }
    }

    /// A floating window's content: a title bar and its groups.
    pub(crate) fn render_float(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.settle_overlays();
        self.settle_full_screen(Some(id), window);
        let Some(root) = self.tree.window_root(Some(id)) else {
            return Empty.into_any_element();
        };
        let root_id = root.id();
        let groups = self.render_layout(root, Some(id), window, cx);
        let name = self.root.file_name().map_or_else(
            || self.root.display().to_string(),
            |name| name.to_string_lossy().to_string(),
        );
        let title_bar = TitleBar::new()
            // Linux's close button (den draws it there): as the window's own close.
            .on_close_window(cx.listener(move |this, _, _, cx| this.dock_float(id, false, cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().truncate().child(name)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(self.root_actions(root_id, "window", !root.is_group(), cx))
                    .child(
                        Button::new("float-dock")
                            .small()
                            .ghost()
                            .icon(Icon::new(IconName::Minimize))
                            .tooltip("Move into Main Window")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.dock_float(id, true, cx)),
                            ),
                    ),
            );
        self.layout_actions(v_flex().id("float"), cx)
            // Alt+F4 closes this window, not den.
            .on_action(
                cx.listener(move |this, _: &crate::Quit, _, cx| this.dock_float(id, false, cx)),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(!self.full_screen_in(Some(id)), |this| this.child(title_bar))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(groups)
                    .child(self.render_overlays(Some(id), window, cx)),
            )
            .when(cfg!(windows) && window.has_active_dialog(cx), |this| {
                this.child(caption_buttons(window, cx))
            })
            .into_any_element()
    }

    fn cycle_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.tree.tabs(self.active_group).to_vec();
        let Some(current) = self.tree.active_tab(self.active_group) else {
            return;
        };
        let Some(ix) = tabs.iter().position(|p| *p == current) else {
            return;
        };
        let next = tabs[(ix as isize + step).rem_euclid(tabs.len() as isize) as usize];
        self.show_pane(next, true, window, cx);
        self.flash(Flash::Tab(next), cx);
    }

    /// Alt+1…9 (Ctrl on macOS): the active group's tab there, counted from
    /// 1; 0 for the last.
    fn open_tab_at(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.tree.tabs(self.active_group);
        let pane = if n == 0 { tabs.last() } else { tabs.get(n - 1) };
        if let Some(pane) = pane.copied() {
            self.show_pane(pane, true, window, cx);
            self.flash(Flash::Tab(pane), cx);
        }
    }

    /// Ctrl+1…8 (Cmd on macOS): the group there in the layout, counted from 1.
    fn focus_group_at(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        // With a group shown as tiles, the numbers are its tiles'.
        if let Some(tiled) = self.tiled_group() {
            if let Some(pane) = n
                .checked_sub(1)
                .and_then(|ix| self.tree.tabs(tiled).get(ix).copied())
            {
                self.show_pane(pane, true, window, cx);
                self.flash(Flash::Tab(pane), cx);
            }
            return;
        }
        let Some(group) = n
            .checked_sub(1)
            .and_then(|ix| self.tree.groups().get(ix).copied())
        else {
            return;
        };
        self.set_active_group(group, cx);
        if let Some(pane) = self.tree.active_tab(group) {
            self.focus_pane(pane, window, cx);
        }
        self.flash(Flash::Group(group), cx);
    }

    /// Flash the border of the group or tab the keys went to, so the eye
    /// finds it.
    fn flash(&mut self, target: Flash, cx: &mut Context<Self>) {
        self.flash = Some((target, std::time::Instant::now()));
        cx.notify();
    }

    /// How strong `target`'s flash is now, `None` when it is not flashing;
    /// keeps the frames coming while it is.
    pub(crate) fn flash_alpha(&self, target: Flash, window: &Window) -> Option<f32> {
        let (_, started) = self.flash.filter(|(flashed, _)| *flashed == target)?;
        let t = started.elapsed().as_secs_f32() / FLASH.as_secs_f32();
        if t >= 1. {
            return None;
        }
        window.request_animation_frame();
        // Full at first, fading faster towards the end.
        Some(1. - t * t)
    }

    /// Shows `wanted`'s numbers, or takes the shown ones away, fading: a
    /// change mid-fade carries on from where the fade stands.
    fn set_hints(&mut self, wanted: Option<Hints>) {
        if wanted == self.hints {
            return;
        }
        let fade = HINT_FADE.as_secs_f32();
        let t = (self.hints_at.elapsed().as_secs_f32() / fade).min(1.);
        let (shown, alpha) = match (self.hints, self.hints_gone) {
            (Some(hints), _) => (Some(hints), t),
            (None, Some(hints)) => (Some(hints), (1. - t).max(0.)),
            (None, None) => (None, 0.),
        };
        let from = match wanted {
            Some(_) if wanted == shown => alpha,
            Some(_) => 0.,
            None => 1. - alpha,
        };
        self.hints_gone = self.hints.filter(|_| wanted.is_none());
        self.hints = wanted;
        self.hints_at = std::time::Instant::now() - Duration::from_secs_f32(from * fade);
    }

    /// The numbers shown and how far in they are, `None` when there are
    /// none; keeps the frames coming while they fade.
    pub(crate) fn hints_alpha(&self, window: &Window) -> Option<(Hints, f32)> {
        let t = self.hints_at.elapsed().as_secs_f32() / HINT_FADE.as_secs_f32();
        let (hints, alpha) = match (self.hints, self.hints_gone) {
            (Some(hints), _) => (hints, t.min(1.)),
            (None, Some(hints)) => (hints, 1. - t),
            (None, None) => return None,
        };
        if alpha <= 0. {
            return None;
        }
        if t < 1. {
            window.request_animation_frame();
        }
        Some((hints, alpha))
    }

    /// The modifiers changed: Ctrl (Cmd on macOS) held a moment numbers the
    /// groups, Alt (Ctrl) the active group's tabs, as their commands' keys
    /// are set. A moment, so a quick Ctrl+S does not flash the numbers.
    fn modifiers_changed(&mut self, modifiers: Modifiers, cx: &mut Context<Self>) {
        if self.held == modifiers {
            return;
        }
        self.held = modifiers;
        self.hint_seq += 1;
        let wanted = if !modifiers.modified() {
            None
        } else if Some(modifiers) == crate::keymap::modifiers_of(&crate::FocusGroup(1), cx) {
            Some(Hints::Groups)
        } else if Some(modifiers) == crate::keymap::modifiers_of(&crate::OpenTab(1), cx) {
            Some(Hints::Tabs)
        } else {
            None
        };
        if wanted != self.hints {
            self.set_hints(None);
            if wanted.is_some() {
                let seq = self.hint_seq;
                cx.spawn(async move |this, cx| {
                    let delay = if wanted == Some(Hints::Groups) {
                        GROUP_HINT_DELAY
                    } else {
                        HINT_DELAY
                    };
                    cx.background_executor().timer(delay).await;
                    _ = this.update(cx, |this, cx| {
                        if this.hint_seq == seq {
                            this.set_hints(wanted);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    // -- Opening things ------------------------------------------------------

    /// The pane as its kind, when it is of that kind.
    pub(crate) fn pane_as<T: 'static>(&self, id: PaneId) -> Option<Entity<T>> {
        self.panes
            .get(&id)
            .and_then(|pane| pane.view().downcast::<T>().ok())
    }

    /// The panes of one kind.
    pub(crate) fn panes_of<T: 'static>(&self) -> impl Iterator<Item = (PaneId, Entity<T>)> + '_ {
        self.panes
            .iter()
            .filter_map(|(id, pane)| Some((*id, pane.view().downcast::<T>().ok()?)))
    }

    /// The first tab, in layout order, of kind `T` that `matches`.
    fn find_pane<T: 'static>(&self, matches: impl Fn(&T) -> bool, cx: &App) -> Option<PaneId> {
        self.tree.panes().into_iter().find(|id| {
            self.pane_as::<T>(*id)
                .is_some_and(|view| matches(view.read(cx)))
        })
    }

    /// Show the tab `existing`, else a new pane from `make` where tabs of
    /// `kind` go.
    fn show_or_add<T: Pane>(
        &mut self,
        existing: Option<PaneId>,
        kind: Kind,
        focus: bool,
        make: impl FnOnce(&mut Window, &mut Context<Self>) -> Entity<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match existing {
            Some(pane) => self.show_pane(pane, focus, window, cx),
            None => {
                let pane = make(window, cx);
                let group = self.target_group(kind);
                self.add_pane(pane, group, focus, window, cx);
                if kind == Kind::Files
                    && let Some(added) = self.tree.active_tab(group)
                {
                    self.limit_file_tabs(group, added, cx);
                }
            }
        }
    }

    /// Open a file: as the preview tab (a single click; focus stays where it
    /// is, and the next preview replaces it), or kept, as in den.
    pub fn open_file(
        &mut self,
        path: PathBuf,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.find_pane::<FilePanel>(|file| file.path() == Some(path.as_path()), cx);
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
                self.add_pane(file, group, !preview, window, cx);
                let added = self.tree.active_tab(group);
                if let Some(old) = replaced {
                    // Into the old tab's place, so the strip does not shuffle.
                    if let (Some(added), Some(ix)) =
                        (added, self.tree.tabs(group).iter().position(|p| *p == old))
                    {
                        self.tree.move_tab(added, group, Some(ix));
                    }
                    self.close_pane(old, cx);
                }
                if preview {
                    self.preview = added;
                }
                if let Some(added) = added {
                    self.limit_file_tabs(group, added, cx);
                }
            }
        }
    }

    /// Past the tab limit (Settings > Tabs), close `group`'s file and diff
    /// tabs opened or focused longest ago, never `keep` or one with unsaved
    /// changes.
    fn limit_file_tabs(&mut self, group: NodeId, keep: PaneId, cx: &mut Context<Self>) {
        let limit = Settings::get(cx).max_file_tabs;
        if limit == 0 {
            return;
        }
        let mut files: Vec<PaneId> = self
            .tree
            .tabs(group)
            .iter()
            .copied()
            .filter(|id| {
                self.pane_as::<FilePanel>(*id).is_some() || self.pane_as::<DiffPanel>(*id).is_some()
            })
            .collect();
        while files.len() > limit {
            // Untouched since coming back with the session counts as oldest.
            let Some(stalest) = files
                .iter()
                .copied()
                .filter(|id| *id != keep && !self.panes.get(id).is_some_and(|p| p.is_dirty(cx)))
                .min_by_key(|id| self.shown.iter().position(|p| p == id))
            else {
                break;
            };
            files.retain(|id| *id != stalest);
            self.close_pane(stalest, cx);
        }
    }

    /// Quick Open over the folder, the open editors' files listed first; the
    /// command palette when `commands` (its query starts `>`).
    fn quick_open(&mut self, commands: bool, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .tree
            .panes()
            .into_iter()
            .filter_map(|id| self.pane_as::<FilePanel>(id))
            .filter_map(|file| file.read(cx).path().map(Path::to_path_buf))
            .collect();
        // Ctrl's release goes to the dialog, not here: let it go now, or the
        // group numbers would come up behind the dialog and stay.
        self.modifiers_changed(Modifiers::default(), cx);
        let workspace = cx.entity().downgrade();
        crate::quick_open::open(self.root.clone(), open, commands, workspace, window, cx);
    }

    /// A new untitled file, as VS Code's Ctrl+N: an empty tab named by the
    /// lowest free number, saved where Save As says. In `group`, else where
    /// files go.
    pub(crate) fn new_untitled(
        &mut self,
        group: Option<NodeId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let taken: HashSet<String> = self
            .panes_of::<FilePanel>()
            .filter_map(|(_, file)| file.read(cx).untitled_name())
            .collect();
        let name = (1..)
            .map(|n| format!("Untitled-{n}"))
            .find(|name| !taken.contains(name))
            .unwrap_or_default();
        let path = self.root.join(name);
        let file = cx.new(|cx| FilePanel::untitled(path, window, cx));
        let group = group.unwrap_or_else(|| self.target_group(Kind::Files));
        self.add_pane(file, group, true, window, cx);
        if let Some(added) = self.tree.active_tab(group) {
            self.limit_file_tabs(group, added, cx);
        }
    }

    /// Keep the preview tab open (editing it, or double-clicking it), or turn
    /// a kept tab back into the preview.
    pub(crate) fn toggle_preview(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.preview == Some(pane) {
            self.preview = None;
        } else if self.pane_as::<FilePanel>(pane).is_some() {
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

    /// Open a file and put the cursor at `line` and `column` (both from 1),
    /// that line at the top.
    pub(crate) fn open_file_at(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(path.clone(), false, window, cx);
        if let Some(line) = line {
            self.go_to(&path, line, column.unwrap_or(1), true, window, cx);
            if let Some(file) = self.file_pane(&path, cx) {
                file.update(cx, |file, cx| file.scroll_to_top(line, window, cx));
            }
        }
    }

    fn file_pane(&self, path: &std::path::Path, cx: &App) -> Option<Entity<FilePanel>> {
        self.panes_of::<FilePanel>()
            .map(|(_, file)| file)
            .find(|file| file.read(cx).path() == Some(path))
    }

    /// Move an open file's cursor to `line` and `column` (both from 1).
    pub(crate) fn go_to(
        &mut self,
        path: &std::path::Path,
        line: u32,
        column: u32,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(file) = self.file_pane(path, cx) {
            file.update(cx, |file, cx| file.go_to(line, column, focus, window, cx));
        }
    }

    /// A file with unsaved changes in a tab.
    pub(crate) fn is_dirty_file(&self, path: &std::path::Path, cx: &App) -> bool {
        self.file_pane(path, cx)
            .is_some_and(|file| file.read(cx).is_dirty(cx))
    }

    // -- Following the disk -------------------------------------------------

    /// Watch the session root: the Explorer, git status and open files follow
    /// changes on disk.
    fn start_watching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok((watcher, mut rx)) = watch::watch(&self.root) else {
            return;
        };
        self._watcher = Some(watcher);
        self._watch_task = Some(cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let timer =
                move |d| -> futures::future::BoxFuture<'static, ()> { Box::pin(executor.timer(d)) };
            while let Some(batch) =
                watch::next_batch(&mut rx, Duration::from_millis(150), &timer).await
            {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.on_disk_changed(batch, window, cx)
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn on_disk_changed(
        &mut self,
        batch: BTreeSet<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let outside_git: Vec<PathBuf> = batch
            .iter()
            .filter(|p| !watch::in_git_dir(p))
            .cloned()
            .collect();
        if outside_git.iter().any(|p| {
            crate::backend::dev_server::is_project_file(p.strip_prefix(&self.root).unwrap_or(p))
        }) {
            self.detect_dev_servers(cx);
        }
        if !outside_git.is_empty() {
            self.explorer
                .update(cx, |explorer, cx| explorer.paths_changed(&outside_git, cx));
            // Open files without unsaved changes take what is on disk now.
            for path in &outside_git {
                if let Some(file) = self.file_pane(path, cx) {
                    file.update(cx, |file, cx| file.reload_from_disk(window, cx));
                }
            }
        }
        // What git keeps changed (a commit, a checkout, a fetch): the commits
        // too, and everything when a remote may have. Files changed: the
        // status, unless they are all ignored (a build writing its output).
        let git_state: Vec<&PathBuf> = batch.iter().filter(|p| watch::is_git_state(p)).collect();
        let level = if git_state
            .iter()
            .any(|p| p.file_name().is_some_and(|name| name == "config"))
        {
            Refresh::Reload
        } else if !git_state.is_empty() {
            Refresh::Full
        } else if outside_git.is_empty()
            || outside_git.iter().all(|p| self.repo.read(cx).is_ignored(p))
        {
            return;
        } else {
            Refresh::Status
        };
        self.repo.update(cx, |repo, cx| repo.request(level, cx));
        self.reload_diffs((level == Refresh::Status).then_some(&batch), cx);
    }

    /// The change to `path` (`rel` in the repository) side by side, in an
    /// existing diff tab when there is one.
    fn open_diff(
        &mut self,
        path: PathBuf,
        rel: String,
        staged: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else {
            return;
        };
        let existing = self.find_pane::<DiffPanel>(
            |diff| diff.path() == path && diff.staged() == staged && diff.commit().is_none(),
            cx,
        );
        if let Some(diff) = existing.and_then(|pane| self.pane_as::<DiffPanel>(pane)) {
            diff.update(cx, |diff, cx| diff.reload(cx));
        }
        self.show_or_add(
            existing,
            Kind::Files,
            false,
            |_, cx| cx.new(|cx| DiffPanel::new(path, top, rel, staged, cx)),
            window,
            cx,
        );
    }

    /// A commit's change to one file; shown again when already open.
    fn open_commit_diff(
        &mut self,
        rel: String,
        commit: crate::diff::CommitRevs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else {
            return;
        };
        let path = top.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let existing = self.find_pane::<DiffPanel>(
            |diff| diff.path() == path && diff.commit().is_some_and(|c| c.hash == commit.hash),
            cx,
        );
        self.show_or_add(
            existing,
            Kind::Files,
            false,
            |_, cx| cx.new(|cx| DiffPanel::for_commit(path, top, rel, commit, cx)),
            window,
            cx,
        );
    }

    /// After git's own state changed (`None`: a stage, a commit, a checkout),
    /// every file's gutter marks and every working-tree diff read again;
    /// after files changed, only the diff tabs of those files (an open file
    /// reloads itself).
    fn reload_diffs(&mut self, batch: Option<&BTreeSet<PathBuf>>, cx: &mut Context<Self>) {
        if batch.is_none() {
            for (_, file) in self.panes_of::<FilePanel>() {
                file.update(cx, |file, cx| file.refresh_git_base(cx));
            }
        }
        let changed: Option<HashSet<String>> =
            batch.map(|batch| batch.iter().map(|p| crate::repo::key(p)).collect());
        for (_, diff) in self.panes_of::<DiffPanel>() {
            let wanted = {
                let diff = diff.read(cx);
                diff.commit().is_none()
                    && changed
                        .as_ref()
                        .is_none_or(|changed| changed.contains(&crate::repo::key(diff.path())))
            };
            if wanted {
                diff.update(cx, |diff, cx| diff.reload(cx));
            }
        }
    }

    /// Settings in a dialog over the window, as den has them; scrolled to
    /// the presets with `presets`.
    pub(crate) fn open_settings(
        &mut self,
        presets: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_at(presets.then_some(1), window, cx);
    }

    /// Settings scrolled to its `section`th part (1 the presets, 2 Keyboard
    /// Shortcuts); the top with none.
    pub(crate) fn open_settings_at(
        &mut self,
        section: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| SettingsPanel::new(window, cx));
        if let Some(section) = section {
            view.update(cx, |view, _| view.show_section(section));
        }
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
        window.defer(cx, move |window, cx| {
            search.update(cx, |input, cx| input.focus(window, cx))
        });
    }

    /// A terminal running the coding agent's preset (a plain shell without one).
    fn open_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let program = Settings::get(cx)
            .presets
            .iter()
            .find(|p| p.agent)
            .map(|p| p.command.clone());
        let agent = program.is_some();
        self.open_terminal(None, program, agent, window, cx)
    }

    /// Open Preset 1…9 (no keys by default): the `n`th pinned preset, as the
    /// tab strips show them, in the group its kind goes to.
    fn open_preset(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = n.checked_sub(1).and_then(|ix| {
            Settings::get(cx)
                .presets
                .iter()
                .filter(|p| p.pinned)
                .nth(ix)
                .cloned()
        }) else {
            return;
        };
        if preset.browser {
            crate::browser::open(&preset.command, cx);
        } else {
            self.open_terminal(None, Some(preset.command), preset.agent, window, cx);
        }
    }

    /// A terminal running `program` (`None` for a plain shell), a coding
    /// agent's or not: in `group` when a group's button asked, else where
    /// its kind goes.
    pub(crate) fn open_terminal(
        &mut self,
        group: Option<NodeId>,
        program: Option<String>,
        agent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = group.unwrap_or_else(|| {
            self.target_group(if agent { Kind::Agents } else { Kind::Terminals })
        });
        let program = program
            .map(|p| p.trim().to_string())
            .filter(|c| !c.is_empty());
        let launch = Launch {
            cwd: self.root.clone(),
            program: program.clone(),
            command: program,
            history: None,
            agent,
        };
        self.add_terminal(launch, group, window, cx);
    }

    fn add_terminal(
        &mut self,
        launch: Launch,
        group: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal = cx.new(|cx| TerminalPanel::new(launch, window, cx));
        self.add_pane(terminal, group, true, window, cx);
    }

    /// Find the project's dev servers again (at start, and when a file that
    /// decides them changed).
    fn detect_dev_servers(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self._dev_server_task = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { crate::backend::dev_server::detect(&root) })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.dev_servers != found {
                    this.dev_servers = found;
                    cx.notify();
                }
            });
        }));
    }

    /// Open Dev Server: the project's dev server in the system's browser.
    /// The one answering now when several were found; a toast when none was.
    pub(crate) fn open_dev_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let found = if self.dev_servers.is_empty() {
            crate::backend::dev_server::detect(&self.root)
        } else {
            self.dev_servers.clone()
        };
        if found.is_empty() {
            crate::toast::push(window, "No dev server found in this project.", cx);
            return;
        }
        cx.spawn(async move |_, cx| {
            let url = cx
                .background_executor()
                .spawn(async move { crate::backend::dev_server::pick(&found) })
                .await;
            if let Some(url) = url {
                cx.update(|cx| crate::browser::open(&url, cx));
            }
        })
        .detach();
    }

    /// The page of extension `id` (a preview of `listing` when it is not
    /// installed): its tab if open, else a new one where files go.
    pub(crate) fn open_extension(
        &mut self,
        id: String,
        listing: Option<crate::backend::extensions::Listing>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.find_pane::<ExtensionPanel>(|page| page.id() == id, cx);
        let make = |window: &mut Window, cx: &mut Context<Self>| match listing {
            Some(listing) => cx.new(|cx| ExtensionPanel::preview(listing, cx)),
            None => cx.new(|cx| ExtensionPanel::new(id, window, cx)),
        };
        self.show_or_add(existing, Kind::Files, true, make, window, cx);
    }

    /// Extension `id`'s view `view`: its tab if open, else a new one where
    /// files go; or the sidebar, for a view the manifest puts there.
    pub(crate) fn open_extension_view(
        &mut self,
        id: String,
        view: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let location = crate::extensions::Extensions::get(cx)
            .view_kind(&id, &view)
            .map(|kind| kind.location);
        if location == Some(den_extension::ViewLocation::Sidebar) {
            self.show_sidebar_view(id, view, window, cx);
            return;
        }
        let existing = self.find_pane::<ExtensionView>(|v| v.is(&id, &view), cx);
        let root = self.root.clone();
        self.show_or_add(
            existing,
            Kind::Files,
            true,
            |_, cx| cx.new(|cx| ExtensionView::new(id, view, root, cx)),
            window,
            cx,
        );
    }

    /// Whether extension `id` runs and puts its view `view` in the sidebar.
    fn is_sidebar_view(id: &str, view: &str, cx: &App) -> bool {
        crate::extensions::Extensions::get(cx)
            .sidebar_views()
            .any(|(i, v)| i == id && v.id == view)
    }

    /// The extension view the sidebar shows, when it shows one.
    fn shown_sidebar_view(&self, cx: &App) -> Option<Entity<ExtensionView>> {
        let (id, view) = self.sidebar_shown.as_ref()?;
        if !Self::is_sidebar_view(id, view, cx) {
            return None;
        }
        self.sidebar_views
            .iter()
            .find(|v| v.read(cx).is(id, view))
            .cloned()
    }

    /// What the sidebar shows here: the Explorer in place of an extension
    /// view this window doesn't have.
    fn sidebar_view(&self, cx: &App) -> SidebarView {
        match self.sidebar.view {
            SidebarView::Extension if self.shown_sidebar_view(cx).is_none() => {
                SidebarView::Explorer
            }
            view => view,
        }
    }

    /// Show extension `id`'s view `view` in the sidebar, with the keyboard in
    /// it; opened now if it wasn't.
    fn show_sidebar_view(
        &mut self,
        id: String,
        view: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.sidebar_views.iter().any(|v| v.read(cx).is(&id, &view)) {
            // A tab of it from before it moved to the sidebar moves there,
            // so the extension doesn't see it close.
            let existing = self.find_pane::<ExtensionView>(|v| v.is(&id, &view), cx);
            let tab = existing.and_then(|pane| {
                let entity = self.pane_as::<ExtensionView>(pane);
                self.close_pane(pane, cx);
                entity
            });
            let root = self.root.clone();
            let (id, view) = (id.clone(), view.clone());
            let entity = tab.unwrap_or_else(|| cx.new(|cx| ExtensionView::new(id, view, root, cx)));
            entity.update(cx, |view, _| view.in_sidebar = true);
            self.sidebar_views.push(entity);
        }
        self.sidebar_shown = Some((id, view));
        self.sidebar.visible = true;
        self.sidebar.view = SidebarView::Extension;
        self.schedule_save(cx);
        if let Some(view) = self.shown_sidebar_view(cx) {
            view.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Its title-bar button: show extension `id`'s view `view`, or hide the
    /// sidebar when it already shows.
    fn toggle_sidebar_view(
        &mut self,
        id: String,
        view: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let showing = self.sidebar.visible
            && self.sidebar_view(cx) == SidebarView::Extension
            && self.sidebar_shown.as_ref() == Some(&(id.clone(), view.clone()));
        if showing {
            self.sidebar.visible = false;
            self.schedule_save(cx);
            cx.notify();
        } else {
            self.show_sidebar_view(id, view, window, cx);
        }
    }

    /// An extension's `open_diff`: a commit's change to a file, or its
    /// uncommitted change, in the window's repository.
    pub(crate) fn open_extension_diff(
        &mut self,
        diff: den_extension::view::Diff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else {
            return;
        };
        let rel = crate::diff::rel_in(&top, &diff.path);
        match diff.hash {
            Some(hash) => {
                let old_rel = diff.old_path.unwrap_or_else(|| rel.clone());
                self.open_commit_diff(
                    rel,
                    crate::diff::CommitRevs {
                        hash,
                        parent: diff.parent,
                        old_rel,
                    },
                    window,
                    cx,
                );
            }
            None => {
                let path = top.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                self.open_diff(path, rel, diff.staged, window, cx);
            }
        }
    }

    /// An extension's `run_in_terminal`: a shell in `cwd` (else the root)
    /// running `command`, where terminals go.
    pub(crate) fn run_in_terminal(
        &mut self,
        command: String,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self.target_group(Kind::Terminals);
        // A plain shell, so the session brings it back as one rather than
        // running the command again.
        let launch = Launch {
            cwd: cwd.unwrap_or_else(|| self.root.clone()),
            program: None,
            command: Some(command),
            history: None,
            agent: false,
        };
        self.add_terminal(launch, group, window, cx);
    }

    /// A shell starting in `dir` (Open Terminal Here).
    pub(crate) fn open_shell_in(
        &mut self,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self.target_group(Kind::Terminals);
        let launch = Launch {
            cwd: dir,
            program: None,
            command: None,
            history: None,
            agent: false,
        };
        self.add_terminal(launch, group, window, cx);
    }

    /// Another terminal beside `pane` running what it runs, in the folder its
    /// shell is in now.
    pub(crate) fn duplicate_terminal(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.pane_as::<TerminalPanel>(pane) else {
            return;
        };
        let Some(group) = self.tree.group_of(pane) else {
            return;
        };
        let terminal = terminal.read(cx);
        let program = terminal.program().map(str::to_string);
        let launch = Launch {
            cwd: terminal.cwd().to_path_buf(),
            program: program.clone(),
            command: program,
            history: None,
            agent: terminal.is_agent(),
        };
        self.add_terminal(launch, group, window, cx);
    }

    /// Show `path` in the Explorer view: in the main window's sidebar, which
    /// comes to the front.
    pub(crate) fn reveal_in_sidebar(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.visible = true;
        self.sidebar.view = SidebarView::Explorer;
        self.schedule_save(cx);
        self.explorer
            .update(cx, |explorer, cx| explorer.show_path(&path, cx));
        let focus = self.explorer.focus_handle(cx);
        if self.window == window.window_handle() {
            focus.focus(window, cx);
        } else {
            let target = self.window;
            cx.defer(move |cx| {
                _ = target.update(cx, |_, window, cx| {
                    window.activate_window();
                    focus.focus(window, cx);
                });
            });
        }
        cx.notify();
    }

    /// Move a tab from a floating window into the main one: its group last
    /// active, else its first.
    pub(crate) fn dock_tab(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let groups = self.tree.root.groups();
        let target = self
            .last_active
            .get(&None)
            .filter(|group| groups.contains(group))
            .or(groups.first())
            .copied();
        if let Some(target) = target {
            self.move_tab(pane, target, None, None, cx);
        }
    }

    /// Another tab for `group`: what it is the default for (a new file, a
    /// shell, the first agent preset, the home page); else one like its
    /// tabs, as den does: a group of Claude Code tabs gets another Claude
    /// Code; a mix, or a shell among them, a shell.
    pub(crate) fn open_more(&mut self, group: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        match self.defaults.get(group) {
            Some(Kind::Files) => return self.new_untitled(Some(group), window, cx),
            Some(Kind::Terminals) => {
                return self.open_terminal(Some(group), None, false, window, cx);
            }
            Some(Kind::Agents) => {
                let preset = Settings::get(cx)
                    .presets
                    .iter()
                    .find(|p| p.kind() == Kind::Agents)
                    .map(|p| p.command.clone());
                if preset.is_some() {
                    return self.open_terminal(Some(group), preset, true, window, cx);
                }
            }
            None => {}
        }
        let programs: Vec<Option<(String, bool)>> = self
            .tree
            .tabs(group)
            .iter()
            .filter(|id| self.panes.contains_key(id))
            .map(|id| {
                let terminal = self.pane_as::<TerminalPanel>(*id)?;
                let terminal = terminal.read(cx);
                terminal
                    .program()
                    .map(|p| (p.to_string(), terminal.is_agent()))
            })
            .collect();
        let same = programs
            .first()
            .cloned()
            .flatten()
            .filter(|first| programs.iter().all(|p| p.as_ref() == Some(first)));
        let (program, agent) =
            same.map_or((None, false), |(command, agent)| (Some(command), agent));
        self.open_terminal(Some(group), program, agent, window, cx);
    }

    // -- Sessions and windows ---------------------------------------------

    /// Files with unsaved changes in this window.
    /// Close this window: at once, saving the session, when nothing is
    /// unsaved; else after asking (false then: the window stays for now).
    pub(crate) fn request_close_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let dirty = self.dirty_panes(cx);
        if dirty.is_empty() {
            self.save_session(cx);
            return true;
        }
        self.confirm_discard(
            dirty,
            |this, window, cx| {
                this.save_session(cx);
                window.remove_window();
            },
            window,
            cx,
        );
        false
    }

    fn dirty_panes(&self, cx: &App) -> Vec<PaneId> {
        self.tree
            .panes()
            .into_iter()
            .filter(|id| self.panes.get(id).is_some_and(|p| p.is_dirty(cx)))
            .collect()
    }

    /// Run `then` now, or after asking when tabs have unsaved changes.
    fn confirm_discard(
        &mut self,
        panes: Vec<PaneId>,
        then: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            many => format!(
                "{} files have unsaved changes: {}. Discard them?",
                many.len(),
                many.join(", ")
            ),
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
    pub(crate) fn request_close_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_discard(
            vec![pane],
            move |this, _, cx| this.close_pane(pane, cx),
            window,
            cx,
        );
    }

    /// Close several tabs, asking first when any has unsaved changes.
    pub(crate) fn request_close_panes(
        &mut self,
        panes: Vec<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let close = panes.clone();
        self.confirm_discard(
            panes,
            move |this, _, cx| close.iter().for_each(|&pane| this.close_pane(pane, cx)),
            window,
            cx,
        );
    }

    pub(crate) fn request_close_group(
        &mut self,
        group: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panes = self.tree.tabs(group).to_vec();
        self.confirm_discard(
            panes,
            move |this, _, cx| this.close_group(group, cx),
            window,
            cx,
        );
    }

    /// Open `root` as a session: in this window (saving this one first), or a new one.
    fn open_session(
        &mut self,
        root: PathBuf,
        new_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = crate::settings::strip_verbatim(std::fs::canonicalize(&root).unwrap_or(root));
        // Open in another window already: that window comes to the front,
        // and this one stays as it is.
        if let Some(other) = crate::extensions::window_on(&root, cx)
            && other != window.window_handle()
        {
            crate::focus_window(other, cx);
            return;
        }
        if new_window {
            crate::open_workspace(root, cx);
            return;
        }
        let panes = self.tree.panes();
        self.confirm_discard(
            panes,
            move |this, window, cx| {
                this.save_session(cx);
                // Once this workspace is out of its update: the switch drops it.
                let root = root.clone();
                window.defer(cx, move |window, cx| {
                    crate::switch_workspace(root, window, cx)
                });
            },
            window,
            cx,
        );
    }

    fn prompt_open_folder(
        &mut self,
        new_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = paths
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|p| p.into_iter().next())
            else {
                return;
            };
            _ = this.update_in(cx, |this, window, cx| {
                this.open_session(path, new_window, window, cx)
            });
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
            let Some(paths) = paths.await.ok().and_then(Result::ok).flatten() else {
                return;
            };
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
        // With a group shown as tiles, the keys go between its tiles.
        if self.tiled_group().is_some() {
            if let Some(pane) = self.tile_beside(side) {
                self.show_pane(pane, true, window, cx);
                self.flash(Flash::Tab(pane), cx);
            }
            return;
        }
        let Some(group) = self.tree.neighbor(self.active_group, side) else {
            return;
        };
        self.set_active_group(group, cx);
        if let Some(pane) = self.tree.active_tab(group) {
            self.focus_pane(pane, window, cx);
        }
        self.flash(Flash::Group(group), cx);
    }

    // -- Presets -------------------------------------------------------------

    fn load_preset(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = AppState::get(cx)
            .presets
            .iter()
            .find(|p| p.name == name)
            .cloned()
        else {
            return;
        };
        if let Err(err) = self.load_state(
            crate::saved_layout::preset(&preset.layout, &self.root),
            window,
            cx,
        ) {
            crate::toast::push(
                window,
                format!("Could not load layout \"{name}\": {err}"),
                cx,
            );
            return;
        }
        self.changed(cx);
    }

    /// Keep the arrangement as a named preset: the groups, splits, terminals
    /// and defaults, not the files that happened to be open in it.
    fn save_preset(&mut self, name: String, cx: &mut Context<Self>) {
        let mut state = self.dump_state(cx);
        let mut tree = Tree::with_floats(state.root, state.floats);
        state.panes.retain(|id, pane| {
            let file = pane.kind == crate::panels::FILE || pane.kind == crate::diff::DIFF;
            if file {
                tree.remove_tab(*id);
            }
            !file
        });
        state.root = tree.root;
        state.floats = tree.floats;
        let Ok(layout) = serde_json::to_value(state) else {
            return;
        };
        let layout = crate::saved_layout::shareable(&layout, &self.root);
        AppState::update(cx, |state| {
            match state.presets.iter_mut().find(|p| p.name == name) {
                Some(preset) => preset.layout = layout,
                None => state.presets.push(LayoutPreset { name, layout }),
            }
        });
        cx.notify();
    }

    fn prompt_save_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Layout name"));
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, {
            let input = input.clone();
            move |dialog, _, cx| {
                let existing: Vec<String> = AppState::get(cx)
                    .presets
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                dialog
                    .title("Save Layout")
                    .show_cancel(true)
                    .child(v_flex().gap_2().child(Input::new(&input)).when(
                        !existing.is_empty(),
                        |list| {
                            let theme = cx.theme().clone();
                            list.child(
                                div()
                                    .pt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Or replace a saved layout:"),
                            )
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
                                    .child(
                                        Icon::new(IconName::LayoutTemplate)
                                            .small()
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(name.clone())
                                    .on_click(move |_, window, cx| {
                                        let name = name.clone();
                                        _ = this.update(cx, |this, cx| this.save_preset(name, cx));
                                        window.close_dialog(cx);
                                    })
                            }))
                        },
                    ))
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
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx))
        });
    }

    // -- Session -------------------------------------------------------------

    pub(crate) fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self._save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            _ = this.update(cx, |this, cx| this.save_session(cx));
        }));
    }

    fn save_session(&mut self, cx: &mut Context<Self>) {
        let layout = serde_json::to_value(self.dump_state(cx)).ok();
        let expanded = self.explorer.read(cx).expanded();
        let draft = self.scm.read(cx).draft(cx);
        let scm_whole_repo = !self.repo.read(cx).scoped();
        let sidebar_views = self
            .sidebar_views
            .iter()
            .map(|view| view.read(cx).dump(cx))
            .collect();
        let sidebar_shown = self.sidebar_shown.clone();
        let sidebar = self.sidebar.clone();
        let window_state = self.window_state;
        let root = self.root.clone();
        AppState::update(cx, |state| {
            // The next folder opened for the first time starts like this one.
            state.sidebar = sidebar.clone();
            let session = state.session_mut(&root);
            session.sidebar = Some(sidebar);
            session.layout = layout;
            session.expanded = expanded;
            session.commit_message = draft;
            session.scm_whole_repo = scm_whole_repo;
            session.sidebar_views = sidebar_views;
            session.sidebar_shown = sidebar_shown;
            if window_state.is_some() {
                session.window = window_state;
            }
        });
    }

    /// The main window moved, changed size or was maximized: noted once it
    /// settles, as X11 tells it is maximized only after its new size. Full
    /// screen is not noted; the window opens as it was before it.
    fn window_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._window_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                let state = match window.window_bounds() {
                    WindowBounds::Windowed(bounds) => WindowState {
                        bounds: screen_rect(bounds),
                        maximized: false,
                    },
                    // The size it had before, as noted: on X11 the bounds
                    // given are the maximized ones.
                    WindowBounds::Maximized(bounds) => WindowState {
                        bounds: this
                            .window_state
                            .map_or_else(|| screen_rect(bounds), |state| state.bounds),
                        maximized: true,
                    },
                    WindowBounds::Fullscreen(_) => return,
                };
                if this.window_state != Some(state) {
                    this.window_state = Some(state);
                    this.schedule_save(cx);
                }
            });
        }));
    }

    // -- Sidebar -------------------------------------------------------------

    /// The title-bar buttons: show a view, or hide the sidebar when that view
    /// is already showing.
    fn toggle_view(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        let shown = self.sidebar_view(cx);
        let sidebar = &mut self.sidebar;
        if sidebar.visible && shown == view {
            sidebar.visible = false;
        } else {
            sidebar.visible = true;
            sidebar.view = view;
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Ctrl+B: show or hide the sidebar; hidden, the keyboard goes back to
    /// the active tab.
    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.visible = !self.sidebar.visible;
        let visible = self.sidebar.visible;
        self.schedule_save(cx);
        if !visible && let Some(pane) = self.tree.active_tab(self.active_group) {
            self.focus_pane(pane, window, cx);
        }
        cx.notify();
    }

    /// The keys: focus a view, or hide the sidebar when the Explorer already
    /// has focus. (Search and Source Control keep focus in their inputs, so a
    /// second press there just focuses the input again.)
    fn focus_view(&mut self, view: SidebarView, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.sidebar.visible
            && self.sidebar_view(cx) == view
            && view == SidebarView::Explorer
            && self.explorer.focus_handle(cx).contains_focused(window, cx);
        if focused {
            self.sidebar.visible = false;
        } else {
            self.sidebar.visible = true;
            self.sidebar.view = view;
            match view {
                SidebarView::Explorer => self.explorer.focus_handle(cx).focus(window, cx),
                SidebarView::Search => SearchView::focus(&self.search, window, cx),
                SidebarView::Scm => ScmView::focus(&self.scm, window, cx),
                SidebarView::Extensions => ExtensionsView::focus(&self.extensions, window, cx),
                SidebarView::Extension => match self.shown_sidebar_view(cx) {
                    Some(view) => view.focus_handle(cx).focus(window, cx),
                    None => self.explorer.focus_handle(cx).focus(window, cx),
                },
            }
        }
        self.schedule_save(cx);
        cx.notify();
    }

    // -- Render --------------------------------------------------------------

    /// A title-bar button for each sidebar view of the extensions running,
    /// after den's own.
    fn sidebar_view_buttons(&self, cx: &mut Context<Self>) -> Vec<Button> {
        let shown = (self.sidebar.visible && self.sidebar_view(cx) == SidebarView::Extension)
            .then(|| self.sidebar_shown.clone())
            .flatten();
        let views: Vec<(String, den_extension::View)> = crate::extensions::Extensions::get(cx)
            .sidebar_views()
            .map(|(id, view)| (id.to_string(), view.clone()))
            .collect();
        views
            .into_iter()
            .enumerate()
            .map(|(ix, (id, view))| {
                let icon = crate::ui::lucide_icon(&view.icon, cx)
                    .unwrap_or_else(|| Icon::new(IconName::Blocks));
                let key = (id, view.id);
                let active = shown.as_ref() == Some(&key);
                Button::new(("view-extension", ix))
                    .small()
                    .icon(icon)
                    .tooltip(view.title)
                    .ghost()
                    .selected(active)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let (id, view) = key.clone();
                        this.toggle_sidebar_view(id, view, window, cx)
                    }))
            })
            .collect()
    }

    fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self.sidebar.clone();
        let shown = self.sidebar_view(cx);
        let root_node = self.tree.root.id();
        let root_is_split = !self.tree.root.is_group();
        let this = cx.weak_entity();

        let view_button = |id: &'static str,
                           icon: IconName,
                           tooltip: &'static str,
                           keys: &dyn Action,
                           view: SidebarView,
                           cx: &mut Context<Self>| {
            let active = sidebar.visible && shown == view;
            Button::new(id)
                .small()
                .icon(Icon::new(icon))
                .tooltip(crate::keymap::with_keys(tooltip, keys, cx))
                // Highlighted, not filled: the primary colour is the change count's.
                .ghost()
                .selected(active)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_view(view, cx)))
        };

        let layout_button = Button::new("layouts")
            .small()
            .ghost()
            .icon(Icon::new(IconName::LayoutDashboard))
            .tooltip("Layouts")
            .dropdown_menu(move |menu, _, cx| layouts_menu(this.clone(), menu, cx));

        // Account for the space TitleBar keeps outside its children: the
        // traffic lights / padding on the left and caption buttons on the right.
        let left_padding = if crate::ui::COMMAND_KEY {
            if window.is_fullscreen() {
                px(0.)
            } else {
                px(80.)
            }
        } else {
            px(12.)
        };
        let left_inset = left_padding
            + if window.is_fullscreen() {
                px(12.)
            } else {
                px(0.)
            };
        let right_inset = if cfg!(windows)
            || cfg!(target_os = "linux")
                && matches!(window.window_decorations(), Decorations::Client { .. })
        {
            let controls = window.window_controls();
            gpui_kit::component::TITLE_BAR_HEIGHT
                * (1 + u8::from(controls.minimize) + u8::from(controls.maximize)) as f32
        } else {
            px(0.)
        };

        // Each side starts with the opposite inset, then shares the remaining
        // space equally. The session centres on the whole window when it fits;
        // the buttons' minimum widths take priority in a narrow window.
        let border = cx.theme().border;
        let separator = move || div().w(px(1.)).h(px(16.)).mx_1p5().bg(border);
        TitleBar::new()
            // Linux's close button (den draws it there): as the window's own
            // close, which asks about unsaved changes and saves the session.
            .on_close_window(cx.listener(|this, _, window, cx| {
                if this.request_close_window(window, cx) {
                    window.remove_window();
                }
            }))
            // The title bar keeps room for the traffic lights on macOS; in
            // full screen they are gone with the menu bar.
            .pl(left_padding)
            .child(
                h_flex()
                    .flex_1()
                    .flex_basis(right_inset)
                    .gap_1()
                    // On macOS these menus are in the menu bar (`set_menus`).
                    .when(!crate::ui::COMMAND_KEY, |this| {
                        this.child(
                            div()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(self.app_menu(cx)),
                        )
                        .child(separator())
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(view_button(
                                "view-explorer",
                                IconName::Files,
                                "Explorer",
                                &FocusExplorer,
                                SidebarView::Explorer,
                                cx,
                            ))
                            .child(view_button(
                                "view-search",
                                IconName::Search,
                                "Search",
                                &FocusSearch,
                                SidebarView::Search,
                                cx,
                            ))
                            .child(
                                // As in VS Code: how many files have changed, in the
                                // bottom corner, ringed to stand off the icon.
                                div()
                                    .relative()
                                    .child(view_button(
                                        "view-scm",
                                        IconName::GitBranch,
                                        "Source Control",
                                        &FocusScm,
                                        SidebarView::Scm,
                                        cx,
                                    ))
                                    .map(|this| {
                                        let changed = self
                                            .repo
                                            .read(cx)
                                            .status()
                                            .map_or(0, |status| status.files.len());
                                        if changed == 0 {
                                            return this;
                                        }
                                        let count = if changed > 999 {
                                            "999+".to_string()
                                        } else {
                                            changed.to_string()
                                        };
                                        this.child(
                                            h_flex()
                                                .absolute()
                                                .bottom(-px(4.))
                                                .right(-px(4.))
                                                .min_w(px(14.))
                                                .h(px(14.))
                                                .px(px(3.))
                                                .justify_center()
                                                .rounded_full()
                                                .border_1()
                                                .border_color(cx.theme().title_bar)
                                                .bg(cx.theme().primary)
                                                .text_color(cx.theme().primary_foreground)
                                                .text_size(px(9.))
                                                .line_height(relative(1.))
                                                .child(count),
                                        )
                                    }),
                            )
                            .child(view_button(
                                "view-extensions",
                                IconName::Blocks,
                                "Extensions",
                                &FocusExtensions,
                                SidebarView::Extensions,
                                cx,
                            ))
                            .children(self.sidebar_view_buttons(cx)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.session_switcher(cx)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .flex_basis(left_inset)
                    .justify_end()
                    .child(
                        h_flex()
                            .px_2()
                            .gap_1()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .children(self.extension_buttons(cx))
                            .child(layout_button)
                            // The root's actions, called the workspace's.
                            .children(self.root_actions(root_node, "workspace", root_is_split, cx))
                            .child(separator())
                            .child(
                                Button::new("settings")
                                    .small()
                                    .ghost()
                                    .icon(Icon::new(IconName::Settings))
                                    .tooltip(crate::keymap::with_keys(
                                        "Settings",
                                        &OpenSettings,
                                        cx,
                                    ))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_settings(false, window, cx)
                                    })),
                            ),
                    ),
            )
    }

    /// The buttons extensions put in this window's title bar; one with a
    /// `file_pattern` only while the active tab is a file it matches.
    /// The file in the active group's active tab, if it is a file tab.
    fn active_file(&self, cx: &App) -> Option<PathBuf> {
        self.tree
            .active_tab(self.active_group)
            .and_then(|pane| self.pane_as::<FilePanel>(pane))
            .and_then(|file| file.read(cx).path().map(std::path::Path::to_path_buf))
    }

    /// Toggle Full Screen: `group` maximized in its window, which goes full
    /// screen without its title bar; and all back again.
    pub(crate) fn toggle_full_screen(
        &mut self,
        group: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((group, was_full)) = self.full_screen.take() {
            if self.maximized_group() == Some(group) {
                self.restore_group(cx);
            }
            if !was_full && window.is_fullscreen() {
                window.toggle_fullscreen();
            }
            return cx.notify();
        }
        if !self.is_maximized(group, false) && !self.is_maximized(group, true) {
            self.toggle_maximize(group, false, window, cx);
        }
        if self.maximized_group() != Some(group) {
            return;
        }
        let was_full = window.is_fullscreen();
        if !was_full {
            window.toggle_fullscreen();
        }
        self.full_screen = Some((group, was_full));
        cx.notify();
    }

    /// Leave full screen once its group is no longer maximized (restored,
    /// another group made active, the group closed), when it is in `float`.
    fn settle_full_screen(&mut self, float: Option<u64>, window: &mut Window) {
        let Some((group, was_full)) = self.full_screen else {
            return;
        };
        let maximized = self.is_maximized(group, false) || self.is_maximized(group, true);
        if maximized || self.tree.float_of(group) != float && self.tree.find(group).is_some() {
            return;
        }
        self.full_screen = None;
        if !was_full && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
    }

    /// Whether `group` is full screen (Toggle Full Screen).
    pub(crate) fn is_full_screen(&self, group: NodeId) -> bool {
        self.full_screen.is_some_and(|(g, _)| g == group)
    }

    /// Whether the window of `float` is full screen for Toggle Full Screen.
    fn full_screen_in(&self, float: Option<u64>) -> bool {
        self.full_screen
            .is_some_and(|(group, _)| self.tree.float_of(group) == float)
    }

    /// Tell the extensions when the active file changed since they last heard.
    fn report_active_file(&mut self, cx: &App) {
        let active = self.active_file(cx);
        if active != self.active_file {
            crate::extensions::broadcast(
                den_extension::events::ACTIVE_FILE_CHANGED,
                serde_json::json!({ "root": self.root, "path": active }),
                cx,
            );
            self.active_file = active;
        }
    }

    fn extension_buttons(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(extensions) = cx.try_global::<crate::extensions::Extensions>() else {
            return Vec::new();
        };
        let active_file = self
            .active_file
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let mut elements = Vec::new();
        for set in extensions.buttons_for(&self.root) {
            for button in &set.buttons {
                if button.location != den_extension::ButtonLocation::TitleBar {
                    continue;
                }
                if !button.file_pattern.is_empty()
                    && !active_file.as_deref().is_some_and(|path| {
                        crate::extensions::pattern_matches(&button.file_pattern, path)
                    })
                {
                    continue;
                }
                let icon = crate::ui::lucide_icon(&button.icon, cx);
                let color = crate::preset_icon::parse_color(&button.color);
                let (id, root, name) = (set.id.clone(), self.root.clone(), button.id.clone());
                let tooltip = if button.tooltip.is_empty() {
                    button.label.clone()
                } else {
                    button.tooltip.clone()
                };
                elements.push(
                    Button::new(SharedString::from(format!("ext-{}-{}", set.id, button.id)))
                        .small()
                        .ghost()
                        .tooltip(tooltip)
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .when_some(color, |this, color| this.text_color(color))
                                .when_some(icon, |this, icon| this.child(icon.small()))
                                .child(button.label.clone()),
                        )
                        .on_click(move |_, _, cx| {
                            crate::extensions::button_clicked(&id, &root, &name, cx)
                        })
                        .into_any_element(),
                );
            }
        }
        elements
    }

    /// den's menus, under one button: each item runs the action its chord does.
    fn app_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        Button::new("app-menu")
            .ghost()
            .small()
            .icon(Icon::new(IconName::Menu))
            .tooltip("Menu")
            .dropdown_menu(move |menu, window, cx| {
                let commands = crate::extensions::commands(cx);
                type Run = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);
                // Each with the keys of the command it runs.
                fn item(
                    this: &WeakEntity<Workspace>,
                    label: &'static str,
                    keys: &dyn Action,
                    run: Run,
                    cx: &App,
                ) -> PopupMenuItem {
                    let this = this.clone();
                    PopupMenuItem::new(crate::keymap::menu_label(label, keys, cx)).on_click(
                        move |_, window, cx| {
                            _ = this.update(cx, |this, cx| run(this, window, cx));
                        },
                    )
                }
                let (file, edit, view, terminal, extensions) = (
                    this.clone(),
                    this.clone(),
                    this.clone(),
                    this.clone(),
                    this.clone(),
                );
                menu.item(item(
                    &this,
                    "Show All Commands",
                    &crate::ShowCommands,
                    |this, window, cx| this.quick_open(true, window, cx),
                    cx,
                ))
                .item(item(
                    &this,
                    "Go to File…",
                    &crate::QuickOpen,
                    |this, window, cx| this.quick_open(false, window, cx),
                    cx,
                ))
                .separator()
                .submenu("File", window, cx, move |menu, _, cx| {
                    let this = &file;
                    menu.item(item(
                        this,
                        "New File",
                        &crate::NewFile,
                        |this, window, cx| this.new_untitled(None, window, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Open Folder…",
                        &OpenFolder,
                        |this, window, cx| this.prompt_open_folder(false, window, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Open Folder in New Window…",
                        &OpenFolderInNewWindow,
                        |this, window, cx| this.prompt_open_folder(true, window, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Open Files…",
                        &OpenFiles,
                        |this, window, cx| this.prompt_open_files(window, cx),
                        cx,
                    ))
                    .separator()
                    .item(item(
                        this,
                        "Close Tab",
                        &CloseTab,
                        |this, window, cx| {
                            if let Some(pane) = this.tree.active_tab(this.active_group) {
                                this.request_close_pane(pane, window, cx);
                            }
                        },
                        cx,
                    ))
                    .item(item(
                        this,
                        "Close Group",
                        &CloseGroup,
                        |this, window, cx| this.request_close_group(this.active_group, window, cx),
                        cx,
                    ))
                })
                .submenu("Edit", window, cx, move |menu, _, cx| {
                    let this = &edit;
                    menu.item(item(
                        this,
                        "Format Document",
                        &FormatDocument,
                        |this, window, cx| this.format_active(window, cx),
                        cx,
                    ))
                    .separator()
                    .item(item(
                        this,
                        "Find in Files",
                        &FocusSearch,
                        |this, window, cx| this.focus_view(SidebarView::Search, window, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Replace in Files",
                        &ReplaceInFiles,
                        |_, window, cx| window.dispatch_action(Box::new(ReplaceInFiles), cx),
                        cx,
                    ))
                })
                .submenu("View", window, cx, move |menu, _, cx| {
                    let this = &view;
                    menu.item(item(
                        this,
                        "Toggle Sidebar",
                        &crate::ToggleSidebar,
                        |this, window, cx| this.toggle_sidebar(window, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Explorer",
                        &FocusExplorer,
                        |this, _, cx| this.toggle_view(SidebarView::Explorer, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Search",
                        &FocusSearch,
                        |this, _, cx| this.toggle_view(SidebarView::Search, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Source Control",
                        &FocusScm,
                        |this, _, cx| this.toggle_view(SidebarView::Scm, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Extensions",
                        &FocusExtensions,
                        |this, _, cx| this.toggle_view(SidebarView::Extensions, cx),
                        cx,
                    ))
                    .separator()
                    .item(item(
                        this,
                        "Split Right",
                        &SplitRight,
                        |this, _, cx| this.split(this.active_group, Side::Right, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Split Down",
                        &SplitDown,
                        |this, _, cx| this.split(this.active_group, Side::Bottom, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Save Layout…",
                        &SaveLayout,
                        |this, window, cx| this.prompt_save_preset(window, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Reset Layout",
                        &ResetLayout,
                        |this, _, cx| this.reset_layout(cx),
                        cx,
                    ))
                    .separator()
                    .item(
                        item(
                            this,
                            "Word Wrap",
                            &ToggleSoftWrap,
                            |_, _, cx| Settings::update(cx, |s| s.soft_wrap = !s.soft_wrap),
                            cx,
                        )
                        .checked(Settings::get(cx).soft_wrap),
                    )
                })
                .submenu("Terminal", window, cx, move |menu, _, cx| {
                    let this = &terminal;
                    menu.item(item(
                        this,
                        "New Terminal",
                        &NewTerminal,
                        |this, window, cx| this.open_terminal(None, None, false, window, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Open Dev Server",
                        &OpenDevServer,
                        |this, window, cx| this.open_dev_server(window, cx),
                        cx,
                    ))
                    .item(item(
                        this,
                        "Claude Code",
                        &NewAgent,
                        Self::open_agent,
                        cx,
                    ))
                })
                // The running extensions' commands, each by its extension.
                .when(!commands.is_empty(), |menu| {
                    menu.submenu("Extensions", window, cx, move |mut menu, _, cx| {
                        for (id, name, command) in &commands {
                            let action = crate::extensions::RunCommand {
                                extension: id.clone(),
                                command: command.id.clone(),
                            };
                            let label = crate::keymap::menu_label(
                                &format!("{name}: {}", command.title),
                                &action,
                                cx,
                            );
                            let (this, id, command_id) =
                                (extensions.clone(), id.clone(), command.id.clone());
                            menu =
                                menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                    _ = this.update(cx, |this, cx| {
                                        crate::extensions::run_command(
                                            &id,
                                            &command_id,
                                            &this.root,
                                            cx,
                                        )
                                    });
                                }));
                        }
                        menu
                    })
                })
                .separator()
                .item(item(
                    &this,
                    "Settings",
                    &OpenSettings,
                    |this, window, cx| this.open_settings(false, window, cx),
                    cx,
                ))
                .item(item(
                    &this,
                    "Keyboard Shortcuts",
                    &crate::OpenKeyboardShortcuts,
                    |this, window, cx| this.open_settings_at(Some(2), window, cx),
                    cx,
                ))
                .item(item(
                    &this,
                    "Check for Updates…",
                    &CheckForUpdates,
                    |_, window, cx| crate::update::check_in_window(true, window, cx),
                    cx,
                ))
                .separator()
                .item(item(
                    &this,
                    "Exit",
                    &crate::Quit,
                    |this, window, cx| {
                        if this.request_close_window(window, cx) {
                            window.remove_window();
                        }
                    },
                    cx,
                ))
            })
    }

    /// The session name in the title bar, as den's: the folders opened before,
    /// and opening another.
    fn session_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.session_name.clone();
        let this = cx.weak_entity();
        let root = self.root.clone();
        Button::new("session")
            .ghost()
            .small()
            .label(name)
            .dropdown_caret(true)
            .tooltip(match crate::keymap::keys_for(&OpenFolder, cx) {
                Some(keys) => format!("Switch session ({keys} opens a folder)"),
                None => "Switch session".to_string(),
            })
            .dropdown_menu(move |menu, _, cx| recent_menu(this.clone(), root.clone(), menu, cx))
    }

    fn render_sidebar(&self, cx: &App) -> AnyElement {
        let content: AnyView = match self.sidebar_view(cx) {
            SidebarView::Explorer => self.explorer.clone().into(),
            SidebarView::Search => self.search.clone().into(),
            SidebarView::Scm => self.scm.clone().into(),
            SidebarView::Extensions => self.extensions.clone().into(),
            SidebarView::Extension => match self.shown_sidebar_view(cx) {
                Some(view) => view.into(),
                None => self.explorer.clone().into(),
            },
        };
        div()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(content)
            .into_any_element()
    }
}

impl Workspace {
    /// What every window of the session does with the groups: dropped files,
    /// Alt for the split buttons, and the actions on groups and tabs.
    fn layout_actions(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root
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
            // The mouse's side buttons go back and forward through the tabs
            // looked at, wherever the pointer is (before a group takes the
            // press as a click into it).
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                // A click is not the keys' way about: the numbers go.
                if this.hints.is_some() {
                    this.set_hints(None);
                    cx.notify();
                }
                if let MouseButton::Navigate(direction) = event.button {
                    cx.stop_propagation();
                    this.navigate(direction == NavigationDirection::Back, window, cx);
                }
            }))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                this.modifiers_changed(event.modifiers, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FormatDocument, window, cx| this.format_active(window, cx)),
            )
            .on_action(cx.listener(|_, _: &ToggleSoftWrap, _, cx| {
                Settings::update(cx, |s| s.soft_wrap = !s.soft_wrap)
            }))
            .on_action(
                cx.listener(|this, _: &crate::ToggleFullScreen, window, cx| {
                    this.toggle_full_screen(this.active_group, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &crate::NewFile, window, cx| {
                this.new_untitled(None, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
                this.open_terminal(None, None, false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenDevServer, window, cx| this.open_dev_server(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewAgent, window, cx| this.open_agent(window, cx)))
            .on_action(cx.listener(|this, _: &SplitRight, _, cx| {
                this.split(this.active_group, Side::Right, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, _, cx| {
                this.split(this.active_group, Side::Bottom, cx)
            }))
            .on_action(
                cx.listener(|this, _: &crate::ToggleMaximizeGroup, window, cx| {
                    this.toggle_maximize(this.active_group, false, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ToggleGroupTiles, window, cx| {
                    this.toggle_maximize(this.active_group, true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(pane) = this.tree.active_tab(this.active_group) {
                    this.request_close_pane(pane, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CloseGroup, window, cx| {
                this.request_close_group(this.active_group, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenFiles, window, cx| this.prompt_open_files(window, cx)),
            )
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
                this.focus_group(Side::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
                this.focus_group(Side::Right, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FocusUp, window, cx| {
                    this.focus_group(Side::Top, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
                this.focus_group(Side::Bottom, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(-1, window, cx)))
            .on_action(cx.listener(|this, action: &crate::OpenTab, window, cx| {
                this.open_tab_at(action.0, window, cx)
            }))
            .on_action(cx.listener(|this, action: &crate::FocusGroup, window, cx| {
                this.focus_group_at(action.0, window, cx)
            }))
            .on_action(cx.listener(|this, action: &crate::OpenPreset, window, cx| {
                this.open_preset(action.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::NavigateBack, window, cx| {
                this.navigate(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::NavigateForward, window, cx| {
                this.navigate(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::ShowCommands, window, cx| {
                this.quick_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::QuickOpen, window, cx| {
                this.quick_open(false, window, cx)
            }))
    }
}

/// A rectangle on screen as a float keeps it: x, y, width, height.
fn screen_rect(bounds: Bounds<Pixels>) -> [f32; 4] {
    [
        bounds.origin.x.as_f32(),
        bounds.origin.y.as_f32(),
        bounds.size.width.as_f32(),
        bounds.size.height.as_f32(),
    ]
}

/// Where `at` in `window` is on screen, in physical pixels: the one space all
/// windows and monitors share. (A window's own pixels are the physical ones
/// over the scale of the monitor it is on, so two windows on monitors of
/// different scales count differently.)
pub(crate) fn on_screen(window: &Window, at: Point<Pixels>) -> Point<f32> {
    let scale = window.scale_factor();
    let at = window.bounds().origin + at;
    point(at.x.as_f32() * scale, at.y.as_f32() * scale)
}

/// The monitor under `at` (physical pixels), as gpui names it (its handle).
#[cfg(windows)]
fn monitor_at(at: Point<f32>) -> Option<DisplayId> {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint},
    };
    // SAFETY: a plain query.
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: at.x as i32,
                y: at.y as i32,
            },
            MONITOR_DEFAULTTONULL,
        )
    };
    (!monitor.is_invalid()).then(|| DisplayId::from(monitor.0 as u64))
}

/// A monitor's scale (its DPI over 96).
#[cfg(windows)]
fn display_scale(display: DisplayId) -> Option<f32> {
    use windows::Win32::{
        Graphics::Gdi::HMONITOR,
        UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
    };
    let (mut x, mut y) = (0, 0);
    // SAFETY: a plain query; an unknown handle fails it.
    unsafe {
        GetDpiForMonitor(
            HMONITOR(u64::from(display) as _),
            MDT_EFFECTIVE_DPI,
            &mut x,
            &mut y,
        )
    }
    .ok()?;
    Some(x as f32 / 96.)
}

#[cfg(not(windows))]
fn monitor_at(_: Point<f32>) -> Option<DisplayId> {
    None
}

#[cfg(not(windows))]
fn display_scale(_: DisplayId) -> Option<f32> {
    None
}

/// The scale of the monitor under `at` (physical pixels), if any.
fn monitor_scale(at: Point<f32>) -> Option<f32> {
    monitor_at(at).and_then(display_scale)
}

/// The display a window placed with `grab` (in a display's own pixels) is
/// on. Each display counts by its own scale, so on monitors of different
/// scales two displays' ranges can overlap: the one whose monitor really is
/// under the point wins.
fn display_at(grab: Point<Pixels>, cx: &App) -> Option<std::rc::Rc<dyn PlatformDisplay>> {
    let displays = cx.displays();
    let exact = displays.iter().find(|display| {
        display.bounds().contains(&grab)
            && display_scale(display.id()).is_some_and(|scale| {
                monitor_at(point(grab.x.as_f32() * scale, grab.y.as_f32() * scale))
                    == Some(display.id())
            })
    });
    exact
        .or_else(|| {
            displays
                .iter()
                .find(|display| display.bounds().contains(&grab))
        })
        .cloned()
}

/// Where the pointer is in a window popped out by a drag: on its tab strip,
/// as if it had been picked up there.
const FLOAT_GRAB: Point<Pixels> = Point {
    x: px(80.),
    y: px(52.),
};
/// How long the border of a group or tab the keys went to flashes.
const FLASH: Duration = Duration::from_millis(700);
/// How long the modifiers are held before the numbers show.
const HINT_DELAY: Duration = Duration::from_millis(150);
/// The groups' numbers wait longer: Ctrl is held for many a shortcut.
const GROUP_HINT_DELAY: Duration = Duration::from_millis(400);
/// How long the group numbers take to fade in or out.
const HINT_FADE: Duration = Duration::from_millis(180);

/// `bounds` moved (and shrunk if need be) to lie inside `area`.
fn fit(bounds: Bounds<Pixels>, area: Bounds<Pixels>) -> Bounds<Pixels> {
    let width = bounds.size.width.as_f32().min(area.size.width.as_f32());
    let height = bounds.size.height.as_f32().min(area.size.height.as_f32());
    let x = bounds
        .origin
        .x
        .as_f32()
        .clamp(area.left().as_f32(), area.right().as_f32() - width);
    let y = bounds
        .origin
        .y
        .as_f32()
        .clamp(area.top().as_f32(), area.bottom().as_f32() - height);
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

/// A window's rectangle as kept (see `screen_rect`), at least `min` in size,
/// on the display under its grab point (on its tab strip) and kept inside
/// it: the window is placed on the display it names, and one whose middle
/// is off that display would open centred on it instead. `None` when it is
/// on no display now.
fn place(
    [x, y, w, h]: [f32; 4],
    min: Size<Pixels>,
    cx: &App,
) -> Option<(DisplayId, Bounds<Pixels>)> {
    let bounds = Bounds::new(
        point(px(x), px(y)),
        size(px(w).max(min.width), px(h).max(min.height)),
    );
    let display = display_at(bounds.origin + FLOAT_GRAB, cx).or_else(|| {
        cx.displays()
            .into_iter()
            .find(|display| display.bounds().intersects(&bounds))
    })?;
    Some((display.id(), fit(bounds, display.visible_bounds())))
}

/// How the main window of `root` opens: as it was when the folder was last
/// open (where that is still on a screen, maximized or not), else centred.
pub(crate) fn main_window_bounds(
    root: &Path,
    min: Size<Pixels>,
    cx: &App,
) -> (WindowBounds, Option<DisplayId>) {
    let saved = AppState::get(cx).session(root).window;
    let placed = saved.and_then(|state| place(state.bounds, min, cx));
    let bounds = placed.map_or_else(
        || {
            // Off every screen now: centred, the size it had.
            let size = saved.map_or(size(px(1400.), px(900.)), |state| {
                size(px(state.bounds[2]), px(state.bounds[3]))
            });
            Bounds::centered(None, size.max(&min), cx)
        },
        |(_, bounds)| bounds,
    );
    let bounds = if saved.is_some_and(|state| state.maximized) {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    (bounds, placed.map(|(display, _)| display))
}

/// The window of float `id`, where it was (when that is still on a screen)
/// or centred.
fn open_float_window(
    workspace: Entity<Workspace>,
    id: u64,
    bounds: Option<[f32; 4]>,
    activate: bool,
    cx: &mut App,
) -> Option<AnyWindowHandle> {
    let placed = bounds.and_then(|rect| place(rect, size(px(320.), px(240.)), cx));
    let bounds = placed.map_or_else(
        || Bounds::centered(None, size(px(960.), px(640.)), cx),
        |(_, bounds)| bounds,
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(320.), px(240.))),
        display_id: placed.map(|(display, _)| display),
        focus: activate,
        ..crate::ui::window_options()
    };
    let title = window_title(&workspace.read(cx).root, cx);
    let (window, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| FloatWindow::new(workspace, id, window, cx))
    })
    .ok()?;
    _ = window.update(cx, |_, window, _| {
        if activate {
            window.activate_window();
        }
        window.set_window_title(&title);
    });
    Some(window)
}

/// The handle between the sidebar and the groups being dragged.
struct SidebarResize;

/// How narrow and how wide the handle takes the sidebar.
const SIDEBAR_MIN: f32 = 160.;
const SIDEBAR_MAX: f32 = 640.;

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.settle_overlays();
        self.settle_full_screen(None, window);
        self.report_active_file(cx);
        let sidebar = self.sidebar.clone();
        let side = Settings::get(cx).sidebar_position;
        let groups = self.render_layout(&self.tree.root, None, window, cx);

        let body = if sidebar.visible {
            // A fixed width rather than the kit's resizable group, which
            // rescales every panel with the window: only the handle moves it.
            let left = side == SidebarSide::Left;
            let panel = div()
                .flex_none()
                .h_full()
                .relative()
                .w(px(sidebar.width.clamp(SIDEBAR_MIN, SIDEBAR_MAX)))
                .child(self.render_sidebar(cx));
            let groups = div().flex_1().min_w_0().h_full().relative().child(groups);
            // The kit's handle, straddling the boundary from whichever side
            // comes second.
            let handle = resize_handle("sidebar-handle", gpui::Axis::Horizontal)
                .with_appearance(resize_handle_appearance())
                .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| Nothing));
            let body = h_flex()
                .id("body")
                .size_full()
                .on_drag_move::<std::rc::Rc<SidebarResize>>(cx.listener(
                    move |this, event: &DragMoveEvent<std::rc::Rc<SidebarResize>>, _, cx| {
                        let (bounds, at) = (event.bounds, event.event.position.x);
                        let width = if left {
                            at - bounds.left()
                        } else {
                            bounds.right() - at
                        };
                        let width = width.as_f32().clamp(SIDEBAR_MIN, SIDEBAR_MAX);
                        this.sidebar.width = width;
                        this.schedule_save(cx);
                        cx.notify();
                    },
                ));
            if left {
                body.child(panel).child(groups.child(handle))
            } else {
                body.child(groups).child(panel.child(handle))
            }
            .into_any_element()
        } else {
            groups
        };

        self.layout_actions(v_flex().id("workspace"), cx)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Without size, so no click lands on it: a focus target only.
            .child(div().track_focus(&self.focus_handle))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| {
                this.prompt_open_folder(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenFolderInNewWindow, window, cx| {
                this.prompt_open_folder(true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &SaveLayout, window, cx| this.prompt_save_preset(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ResetLayout, _, cx| this.reset_layout(cx)))
            .on_action(cx.listener(|_, _: &CheckForUpdates, window, cx| {
                crate::update::check_in_window(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusExplorer, window, cx| {
                this.focus_view(SidebarView::Explorer, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.focus_view(SidebarView::Search, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReplaceInFiles, window, cx| {
                this.sidebar.visible = true;
                this.sidebar.view = SidebarView::Search;
                this.schedule_save(cx);
                SearchView::set_query(&this.search, "", true, window, cx);
                SearchView::focus(&this.search, window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusScm, window, cx| {
                this.focus_view(SidebarView::Scm, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusExtensions, window, cx| {
                this.focus_view(SidebarView::Extensions, window, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::ToggleSidebar, window, cx| {
                this.toggle_sidebar(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &crate::OpenKeyboardShortcuts, window, cx| {
                    this.open_settings_at(Some(2), window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, action: &crate::extensions::RunCommand, _, cx| {
                    crate::extensions::run_command(
                        &action.extension,
                        &action.command,
                        &this.root,
                        cx,
                    )
                }),
            )
            .when(!self.full_screen_in(None), |this| {
                this.child(self.render_title_bar(window, cx))
            })
            // The overlays (a maximized group, the tiles) lie over the whole
            // body, sidebar and all, under the title bar.
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(body)
                    .child(self.render_overlays(None, window, cx)),
            )
            .when(cfg!(windows) && window.has_active_dialog(cx), |this| {
                this.child(caption_buttons(window, cx))
            })
    }
}

/// Where `window`'s actions go when nothing in it has the focus: a handle
/// under the workspace's action handlers, in the main window or a floating
/// one (see `Workspace::action_target`).
pub(crate) fn action_target(window: &Window, cx: &App) -> Option<FocusHandle> {
    let view = window
        .root::<gpui_kit::base::Root>()
        .flatten()?
        .read(cx)
        .view()
        .clone();
    match view.downcast::<Workspace>() {
        Ok(workspace) => Some(workspace.read(cx).action_target(cx)),
        Err(view) => view
            .downcast::<crate::float::FloatWindow>()
            .ok()?
            .read(cx)
            .focus(cx),
    }
}

/// The title bar's minimize, maximize and close buttons, over an open
/// dialog: its overlay makes the whole title bar a drag area, so they would
/// not take clicks.
fn caption_buttons(window: &Window, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let button = |id: &'static str,
                  icon: IconName,
                  area: WindowControlArea,
                  hover_bg: Hsla,
                  hover_fg: Hsla| {
        div()
            .id(id)
            .flex()
            .w(gpui_kit::component::TITLE_BAR_HEIGHT)
            .h_full()
            .items_center()
            .justify_center()
            .text_color(theme.foreground)
            .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
            .window_control_area(area)
            .child(Icon::new(icon).small())
    };
    let maximize = if window.is_maximized() {
        IconName::WindowRestore
    } else {
        IconName::WindowMaximize
    };
    deferred(
        h_flex()
            .absolute()
            .top_0()
            .right_0()
            .h(gpui_kit::component::TITLE_BAR_HEIGHT)
            .child(button(
                "caption-min",
                IconName::WindowMinimize,
                WindowControlArea::Min,
                theme.secondary_hover,
                theme.secondary_foreground,
            ))
            .child(button(
                "caption-max",
                maximize,
                WindowControlArea::Max,
                theme.secondary_hover,
                theme.secondary_foreground,
            ))
            .child(button(
                "caption-close",
                IconName::WindowClose,
                WindowControlArea::Close,
                theme.danger,
                theme.danger_foreground,
            )),
    )
    .with_priority(10)
}
