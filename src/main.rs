//! den: a project terminal for Windows and macOS, on GPUI.
//!
//! See README.md for what is in it and how it is built.

// The built-in themes are large `json!` literals.
#![recursion_limit = "512"]
// A GUI app: no console window opens with it. Dev builds keep theirs for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod backend;
mod browser;
mod defaults;
mod diff;
mod dirty_diff;
mod downloads;
mod encoding;
mod explorer;
mod extension_panel;
mod extension_view;
mod extensions;
mod extensions_view;
mod file_icon;
mod git_graph;
mod float;
mod history;
mod keymap;
mod layout;
mod layout_file;
mod layout_view;
mod overlays;
mod pane;
mod preset_icon;
mod repo;
mod panels;
mod scm;
mod search;
mod settings;
mod sound;
mod terminal;
mod theme;
mod toast;
mod ui;
mod update;
mod workspace;

use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

actions!(
    den,
    [
        SaveFile,
        FormatDocument,
        OpenSettings,
        FocusExplorer,
        FocusExtensions,
        FocusSearch,
        FocusScm,
        SplitRight,
        SplitDown,
        NewTerminal,
        NewBrowser,
        ReplaceInFiles,
        CloseTab,
        CloseGroup,
        NextTab,
        PrevTab,
        OpenFolder,
        OpenFiles,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        OpenFolderInNewWindow,
        SaveLayout,
        ResetLayout,
        CheckForUpdates,
        NewAgent,
        Minimize,
        Zoom,
        HideApp,
        HideOthers,
        ShowAll,
        Quit,
        ShowCommands,
        OpenKeyboardShortcuts,
        ToggleSidebar,
        ToggleMaximizeGroup,
        ToggleGroupTiles,
        ShowAllTabs,
        GoToLine,
        NavigateBack,
        NavigateForward,
    ]
);

/// Show the active group's tab at this place, counted from 1; 0 for its last.
#[derive(Clone, PartialEq, Eq, serde::Deserialize, Action)]
#[action(namespace = den, no_json)]
pub struct OpenTab(pub usize);

/// Focus the group at this place in the layout, counted from 1.
#[derive(Clone, PartialEq, Eq, serde::Deserialize, Action)]
#[action(namespace = den, no_json)]
pub struct FocusGroup(pub usize);

/// Open the pinned preset at this place on the tab strips, counted from 1.
#[derive(Clone, PartialEq, Eq, serde::Deserialize, Action)]
#[action(namespace = den, no_json)]
pub struct OpenPreset(pub usize);

/// A window on the session for `root`, at `bounds` (centred when `None`).
pub fn open_workspace(root: PathBuf, bounds: Option<Bounds<Pixels>>, cx: &mut App) {
    let bounds = bounds.unwrap_or_else(|| Bounds::centered(None, size(px(1400.), px(900.)), cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(640.), px(400.))),
        ..TitleBar::window_options()
    };
    extensions::broadcast(den_extension::events::WORKSPACE_OPENED, serde_json::json!({ "root": root }), cx);
    let Ok((window, workspace)) = gpui_kit::open_window(options, cx, {
        let root = root.clone();
        move |window, cx| cx.new(|cx| workspace::Workspace::new(root, window, cx))
    }) else {
        return;
    };
    window
        .update(cx, |_, window, cx| {
            window.activate_window();
            workspace_opened(root, workspace, window, cx);
        })
        .ok();
}

/// Turn `window` to the session for `root` in its place: the window stays as
/// it is (its bounds, maximised or not), only its content changes.
pub fn switch_workspace(root: PathBuf, window: &mut Window, cx: &mut App) {
    extensions::broadcast(den_extension::events::WORKSPACE_OPENED, serde_json::json!({ "root": root }), cx);
    let mut workspace = None;
    window.replace_root(cx, |window, cx| {
        let view = cx.new(|cx| workspace::Workspace::new(root.clone(), window, cx));
        workspace = Some(view.clone());
        gpui_kit::base::Root::new(view, window, cx)
    });
    if let Some(workspace) = workspace {
        workspace_opened(root, workspace, window, cx);
    }
}

/// A workspace now in `window`: named in the title, known to the extensions.
fn workspace_opened(root: PathBuf, workspace: Entity<workspace::Workspace>, window: &mut Window, cx: &mut App) {
    window.set_window_title(&format!("den — {}", root.display()));
    extensions::window_opened(root, workspace.downgrade(), window.window_handle(), cx);
}

/// The folder to open with none given. Started from a shell, that shell's
/// folder; from the Start menu or taskbar (whose working folder is den's own
/// install folder), the folder last open.
fn start_folder() -> PathBuf {
    let cwd = std::env::current_dir().ok();
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from));
    let from_shell = match (&cwd, &exe_dir) {
        (Some(cwd), Some(exe_dir)) => repo::key(cwd) != repo::key(exe_dir),
        (cwd, _) => cwd.is_some(),
    };
    if !from_shell && let Some(last) = settings::last_folder() {
        return last;
    }
    cwd.unwrap_or_else(|| PathBuf::from("."))
}

/// The menu bar on macOS, which stands for the title bar's menu button
/// there: everything that menu has, and what every Mac app's has. The keys
/// shown come from the keymap, which sets the menus again when it changes.
pub(crate) fn set_menus(cx: &mut App) {
    use gpui_kit::component::input;
    let menu = |name: &str, items: Vec<MenuItem>| Menu { name: name.to_string().into(), items, disabled: false };
    let mut menus = vec![
        menu(
            "den",
            vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Keyboard Shortcuts", OpenKeyboardShortcuts),
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide den", HideApp),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit den", Quit),
            ],
        ),
        menu(
            "File",
            vec![
                MenuItem::action("Open Folder…", OpenFolder),
                MenuItem::action("Open Folder in New Window…", OpenFolderInNewWindow),
                MenuItem::action("Open Files…", OpenFiles),
                MenuItem::separator(),
                MenuItem::action("Save", SaveFile),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Close Group", CloseGroup),
            ],
        ),
        // The system's actions where a native view (a browser tab, a file
        // dialog) has the focus; den's text fields and terminals otherwise.
        menu(
            "Edit",
            vec![
                MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("Format Document", FormatDocument),
                MenuItem::action("Find in Files", FocusSearch),
                MenuItem::action("Replace in Files", ReplaceInFiles),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Show All Commands", ShowCommands),
                MenuItem::separator(),
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Explorer", FocusExplorer),
                MenuItem::action("Search", FocusSearch),
                MenuItem::action("Source Control", FocusScm),
                MenuItem::action("Extensions", FocusExtensions),
                MenuItem::separator(),
                MenuItem::action("Split Right", SplitRight),
                MenuItem::action("Split Down", SplitDown),
                MenuItem::action("Toggle Maximize Group", ToggleMaximizeGroup),
                MenuItem::action("Toggle Group Tiles", ToggleGroupTiles),
                MenuItem::action("Show All Tabs", ShowAllTabs),
                MenuItem::action("Save Layout…", SaveLayout),
                MenuItem::action("Reset Layout", ResetLayout),
                MenuItem::separator(),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
                MenuItem::action("Back", NavigateBack),
                MenuItem::action("Forward", NavigateForward),
            ],
        ),
        menu(
            "Terminal",
            vec![
                MenuItem::action("New Terminal", NewTerminal),
                MenuItem::action("New Browser", NewBrowser),
                MenuItem::action("Claude Code", NewAgent),
            ],
        ),
    ];
    // The commands of the extensions this start runs, each by its extension.
    let commands = extensions::commands(cx);
    if !commands.is_empty() {
        let items = commands
            .into_iter()
            .map(|(extension, name, command)| MenuItem::action(format!("{name}: {}", command.title), extensions::RunCommand { extension, command: command.id }))
            .collect();
        menus.push(menu("Extensions", items));
    }
    // Named "Window", AppKit lists the open windows in it.
    menus.push(menu("Window", vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)]));
    cx.set_menus(menus);
}

/// The menu bar's actions on the window in front.
fn on_window_actions(cx: &mut App) {
    fn in_front(cx: &mut App, run: fn(&mut Window)) {
        if let Some(window) = cx.active_window() {
            _ = window.update(cx, |_, window, _| run(window));
        }
    }
    cx.on_action(|_: &Minimize, cx: &mut App| in_front(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx: &mut App| in_front(cx, |window| window.zoom_window()));
    cx.on_action(|_: &HideApp, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx: &mut App| cx.unhide_other_apps());
}

/// The workspace's actions when nothing in the window has the focus (the
/// focused tab closed, a dialog gone). An action goes along the path from
/// the focused element to the root, and the workspace's handlers hang below
/// the root: with no focus they are out of reach, and macOS shows the menu
/// bar's items disabled. These run only once nothing in the window took the
/// action, and hand it to a handle under the handlers.
fn on_unfocused_actions(cx: &mut App) {
    fn forward<A: Action>(cx: &mut App) {
        cx.on_action(|action: &A, cx: &mut App| {
            let Some(window) = cx.active_window() else { return };
            let action = action.boxed_clone();
            // This runs inside the window's update: reach it afterwards.
            cx.defer(move |cx| {
                _ = window.update(cx, |_, window, cx| {
                    if let Some(focus) = workspace::action_target(window, cx)
                        && window.is_action_available_in(&*action, &focus)
                    {
                        focus.dispatch_action(&*action, window, cx);
                    }
                });
            });
        });
    }
    let forwarded: &[fn(&mut App)] = &[
        forward::<SaveFile>,
        forward::<FormatDocument>,
        forward::<OpenSettings>,
        forward::<FocusExplorer>,
        forward::<FocusExtensions>,
        forward::<FocusSearch>,
        forward::<FocusScm>,
        forward::<SplitRight>,
        forward::<SplitDown>,
        forward::<NewTerminal>,
        forward::<NewBrowser>,
        forward::<ReplaceInFiles>,
        forward::<CloseTab>,
        forward::<CloseGroup>,
        forward::<NextTab>,
        forward::<PrevTab>,
        forward::<OpenFolder>,
        forward::<OpenFiles>,
        forward::<FocusLeft>,
        forward::<FocusRight>,
        forward::<FocusUp>,
        forward::<FocusDown>,
        forward::<OpenFolderInNewWindow>,
        forward::<SaveLayout>,
        forward::<ResetLayout>,
        forward::<CheckForUpdates>,
        forward::<NewAgent>,
        forward::<ShowCommands>,
        forward::<OpenKeyboardShortcuts>,
        forward::<ToggleSidebar>,
        forward::<ToggleMaximizeGroup>,
        forward::<ToggleGroupTiles>,
        forward::<ShowAllTabs>,
        forward::<NavigateBack>,
        forward::<NavigateForward>,
        forward::<OpenTab>,
        forward::<FocusGroup>,
        forward::<OpenPreset>,
        forward::<extensions::RunCommand>,
    ];
    for register in forwarded {
        register(cx);
    }
}

/// Started from the Finder or the Dock, a Mac app gets the system's bare
/// PATH: no Homebrew, no `~/.cargo/bin`, so no `claude`, `pnpm` or `rustfmt`
/// for Source Control, Format Document and the presets. Take the login
/// shell's PATH over, in front of den's own. Run before anything else
/// starts: it writes the environment.
#[cfg(target_os = "macos")]
fn inherit_login_path() {
    use std::{
        io::Read as _,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let current: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    // Started from a terminal, PATH is already the shell's: nothing to do.
    let system = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    if current.iter().any(|dir| !system.iter().any(|s| dir.as_os_str() == *s)) {
        return;
    }
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    // An interactive login shell, as a terminal starts one, so PATH set in
    // .zshrc counts too; markers in case the profile prints something.
    let fish = std::path::Path::new(&shell).file_name().is_some_and(|name| name == "fish");
    let script = if fish { "printf '%s' \"<den-path>\"(string join : $PATH)\"</den-path>\"" } else { "printf '%s' \"<den-path>$PATH</den-path>\"" };
    let Ok(mut child) = Command::new(&shell).args(["-l", "-i", "-c", script]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
    else {
        return;
    };
    // A profile that waits for a terminal must not hold the app up.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
    let mut text = String::new();
    if child.stdout.take().and_then(|mut out| out.read_to_string(&mut text).ok()).is_none() {
        return;
    }
    let Some(login) = text.split("<den-path>").nth(1).and_then(|rest| rest.split("</den-path>").next()) else { return };
    let mut dirs: Vec<PathBuf> = std::env::split_paths(login).collect();
    for dir in current {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        // SAFETY: called first thing in `main`, before any other thread exists.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    inherit_login_path();
    // GPUI composites its frame topmost over child windows through
    // DirectComposition, which would hide the browser tabs' WebView2.
    // Presenting to the window directly puts child windows on top. Read once
    // when the platform starts, so set it before anything else.
    // SAFETY: nothing else runs yet, so no thread reads the environment.
    unsafe { std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1") };
    let root = match std::env::args_os().nth(1) {
        // The jump list's New Window task (see `workspace::sync_jump_list`).
        Some(arg) if arg == "--dock-action" => settings::last_folder().unwrap_or_else(start_folder),
        Some(arg) => PathBuf::from(arg),
        None => start_folder(),
    };
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let root = settings::strip_verbatim(root);

    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            settings::init(cx);
            file_icon::init(cx);
            panels::init(cx);
            terminal::init(cx);
            diff::init(cx);
            extensions::init(cx);
            // Last: it keeps the keys bound so far and adds every command's
            // (and sets the menu bar on macOS).
            keymap::init(cx);

            cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
            on_unfocused_actions(cx);
            if ui::COMMAND_KEY {
                on_window_actions(cx);
            }

            open_workspace(root, None, cx);
            cx.activate(true);
        });
}
