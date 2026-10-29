//! den: a project terminal for Windows, macOS and Linux, on GPUI.
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
mod diff_list;
mod dirty_diff;
mod downloads;
mod encoding;
mod explorer;
mod extension_panel;
mod extension_view;
mod extensions;
mod extensions_view;
mod file_icon;
mod float;
mod frame;
mod git_graph;
mod history;
mod instance;
mod keymap;
mod layout;
mod layout_view;
mod overlays;
mod pane;
mod panels;
mod pdf;
mod preset_icon;
mod quick_open;
mod repo;
mod saved_layout;
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

use gpui_kit::*;

actions!(
    den,
    [
        NewFile,
        SaveFile,
        FormatDocument,
        ToggleSoftWrap,
        OpenSettings,
        FocusExplorer,
        FocusExtensions,
        FocusSearch,
        FocusScm,
        SplitRight,
        SplitDown,
        NewTerminal,
        OpenDevServer,
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
        QuickOpen,
        OpenKeyboardShortcuts,
        ToggleSidebar,
        ToggleMaximizeGroup,
        ToggleGroupTiles,
        GoToLine,
        ToggleFullScreen,
        NavigateBack,
        NavigateForward,
        NextWorkspace,
        PrevWorkspace,
        CloseWorkspace,
        MoveWorkspaceToNewWindow,
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

/// Bring the workspace tab at this place to the front, counted from 1.
#[derive(Clone, PartialEq, Eq, serde::Deserialize, Action)]
#[action(namespace = den, no_json)]
pub struct OpenWorkspace(pub usize);

/// Show the session for `root`: its tab, wherever one is open on it; else
/// a new tab in the window in front (the one last in front, when none is),
/// or a window of its own when there is none.
pub fn open_workspace(root: PathBuf, cx: &mut App) {
    // Out of whatever update this is called from (a menu, a key, a folder
    // dropped): the window is taken for the tab.
    cx.defer(move |cx| {
        if show_workspace(&root, cx) {
            return;
        }
        match frame::target_window(cx) {
            Some(window) => {
                _ = window.update(cx, |_, window, cx| {
                    if let Some(frame) = frame::Frame::of(window, cx) {
                        frame.update(cx, |frame, cx| frame.add(root, window, cx));
                    }
                    window.activate_window();
                });
            }
            None => open_in_new_window(vec![root], 0, cx),
        }
    });
}

/// Bring the tab on `root` to the front, and its window with it, when one
/// is open; false when none is.
fn show_workspace(root: &std::path::Path, cx: &mut App) -> bool {
    let Some((window, workspace)) = extensions::workspace_on(root, cx) else {
        return false;
    };
    cx.activate(true);
    _ = window.update(cx, |_, window, cx| {
        window.activate_window();
        if let Some(frame) = frame::Frame::of(window, cx) {
            frame.update(cx, |frame, cx| {
                frame.activate_entity(workspace.entity_id(), window, cx)
            });
        }
    });
    true
}

/// A window with a tab on each of `roots`, the one at `active` in front,
/// placed as the first folder's window was when it was last open.
pub fn open_in_new_window(roots: Vec<PathBuf>, active: usize, cx: &mut App) {
    let Some(first) = roots.first() else {
        return;
    };
    let min = size(px(640.), px(400.));
    let (bounds, display_id) = workspace::main_window_bounds(first, min, cx);
    let options = WindowOptions {
        window_bounds: Some(bounds),
        window_min_size: Some(min),
        display_id,
        ..crate::ui::window_options()
    };
    let Ok((window, _)) = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| frame::Frame::new(roots, active, window, cx))
    }) else {
        return;
    };
    _ = window.update(cx, |_, window, _| window.activate_window());
}

/// The folder to open with none given. Started from a shell, that shell's
/// folder; from the Start menu or taskbar (whose working folder is den's own
/// install folder), or a desktop launcher or the Dock (which start it in the
/// home folder or `/`, with no terminal), the folder last open.
fn start_folder() -> PathBuf {
    if !started_from_shell()
        && let Some(last) = settings::last_folder()
    {
        return last;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Whether den was started from a shell (see [`start_folder`]).
fn started_from_shell() -> bool {
    let cwd = std::env::current_dir().ok();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from));
    let own_folder = match (&cwd, &exe_dir) {
        (Some(cwd), Some(exe_dir)) => repo::key(cwd) != repo::key(exe_dir),
        (cwd, _) => cwd.is_some(),
    };
    own_folder && (cfg!(windows) || std::io::IsTerminal::is_terminal(&std::io::stdin()))
}

/// The menu bar on macOS, which stands for the title bar's menu button
/// there: everything that menu has, and what every Mac app's has. The keys
/// shown come from the keymap, which sets the menus again when it changes.
pub(crate) fn set_menus(cx: &mut App) {
    use gpui_kit::component::input;
    let menu = |name: &str, items: Vec<MenuItem>| Menu {
        name: name.to_string().into(),
        items,
        disabled: false,
    };
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
                MenuItem::action("New File", NewFile),
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
        // The system's actions where a native view (a file dialog) has the
        // focus; den's text fields and terminals otherwise.
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
                MenuItem::action("Go to File…", QuickOpen),
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
                MenuItem::action("Save Layout…", SaveLayout),
                MenuItem::action("Reset Layout", ResetLayout),
                MenuItem::separator(),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
                MenuItem::action("Back", NavigateBack),
                MenuItem::action("Forward", NavigateForward),
                MenuItem::separator(),
                MenuItem::action("Next Workspace", NextWorkspace),
                MenuItem::action("Previous Workspace", PrevWorkspace),
                MenuItem::action("Close Workspace", CloseWorkspace),
                MenuItem::separator(),
                MenuItem::action("Word Wrap", ToggleSoftWrap),
            ],
        ),
        menu(
            "Terminal",
            vec![
                MenuItem::action("New Terminal", NewTerminal),
                MenuItem::action("Open Dev Server", OpenDevServer),
                MenuItem::action("Claude Code", NewAgent),
            ],
        ),
    ];
    // The commands of the extensions this start runs, each by its extension.
    let commands = extensions::commands(cx);
    if !commands.is_empty() {
        let items = commands
            .into_iter()
            .map(|(extension, name, command)| {
                MenuItem::action(
                    format!("{name}: {}", command.title),
                    extensions::RunCommand {
                        extension,
                        command: command.id,
                    },
                )
            })
            .collect();
        menus.push(menu("Extensions", items));
    }
    // Named "Window", AppKit lists the open windows in it.
    menus.push(menu(
        "Window",
        vec![
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Move Workspace to New Window", MoveWorkspaceToNewWindow),
        ],
    ));
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
            let Some(window) = cx.active_window() else {
                return;
            };
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
        forward::<NewFile>,
        forward::<SaveFile>,
        forward::<FormatDocument>,
        forward::<ToggleSoftWrap>,
        forward::<OpenSettings>,
        forward::<FocusExplorer>,
        forward::<FocusExtensions>,
        forward::<FocusSearch>,
        forward::<FocusScm>,
        forward::<SplitRight>,
        forward::<SplitDown>,
        forward::<NewTerminal>,
        forward::<OpenDevServer>,
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
        forward::<QuickOpen>,
        forward::<OpenKeyboardShortcuts>,
        forward::<ToggleSidebar>,
        forward::<ToggleMaximizeGroup>,
        forward::<ToggleGroupTiles>,
        forward::<ToggleFullScreen>,
        forward::<NavigateBack>,
        forward::<NavigateForward>,
        forward::<OpenTab>,
        forward::<FocusGroup>,
        forward::<OpenPreset>,
        forward::<NextWorkspace>,
        forward::<PrevWorkspace>,
        forward::<CloseWorkspace>,
        forward::<MoveWorkspaceToNewWindow>,
        forward::<OpenWorkspace>,
        forward::<extensions::RunCommand>,
    ];
    for register in forwarded {
        register(cx);
    }
}

/// Started from the Finder or the Dock, a Mac app gets the system's bare
/// PATH: no Homebrew, no `~/.cargo/bin`, so no `claude`, `pnpm` or `rustfmt`
/// for Source Control, Format Document and the presets. Started from a Linux
/// desktop's launcher, den gets the session's PATH, which lacks what
/// `~/.bashrc` adds (nvm, pnpm, volta). Take the login shell's PATH over, in
/// front of den's own. Run before anything else starts: it writes the
/// environment.
#[cfg(unix)]
fn inherit_login_path() {
    use std::{
        io::Read as _,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let current: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    // Started from a terminal, PATH is already the shell's: nothing to do.
    // A Linux session's PATH has more than the system folders in it anyway,
    // so there the terminal itself tells.
    let system = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    let from_terminal = if cfg!(target_os = "macos") {
        current
            .iter()
            .any(|dir| !system.iter().any(|s| dir.as_os_str() == *s))
    } else {
        std::io::IsTerminal::is_terminal(&std::io::stdin())
    };
    if from_terminal {
        return;
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/bash"
            }
            .into()
        });
    // An interactive login shell, as a terminal starts one, so PATH set in
    // .zshrc or .bashrc counts too; markers in case the profile prints something.
    let fish = std::path::Path::new(&shell)
        .file_name()
        .is_some_and(|name| name == "fish");
    let script = if fish {
        "printf '%s' \"<den-path>\"(string join : $PATH)\"</den-path>\""
    } else {
        "printf '%s' \"<den-path>$PATH</den-path>\""
    };
    let Ok(mut child) = Command::new(&shell)
        .args(["-l", "-i", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
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
    if child
        .stdout
        .take()
        .and_then(|mut out| out.read_to_string(&mut text).ok())
        .is_none()
    {
        return;
    }
    let Some(login) = text
        .split("<den-path>")
        .nth(1)
        .and_then(|rest| rest.split("</den-path>").next())
    else {
        return;
    };
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

/// Quit and start again on the windows open now, as updated extensions need.
/// The new den waits for this one to be gone, sessions and windows saved
/// and extensions unloaded, before it applies the updates.
pub fn restart(cx: &mut App) {
    let started = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg("--wait-for")
            .arg(std::process::id().to_string())
            .arg("--restore")
            .spawn()
    });
    match started {
        Ok(_) => cx.quit(),
        Err(err) => {
            if let Some(window) = cx.active_window() {
                _ = window.update(cx, |_, window, cx| {
                    toast::push(window, format!("Could not restart den: {err}"), cx)
                });
            }
        }
    }
}

/// Wait a while for process `pid` to end; false when it still runs (its quit
/// was called off).
fn wait_for_exit(pid: u32) -> bool {
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // SAFETY: a wait on a pid's process; the handle is closed right after.
        unsafe {
            // It cannot be opened when it is gone already.
            let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
                return true;
            };
            let gone = WaitForSingleObject(process, PATIENCE.as_millis() as u32) != WAIT_TIMEOUT;
            let _ = CloseHandle(process);
            gone
        }
    }
    #[cfg(not(windows))]
    {
        let deadline = std::time::Instant::now() + PATIENCE;
        let running = || {
            std::process::Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        while running() {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        true
    }
}

fn main() {
    #[cfg(unix)]
    inherit_login_path();
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    // A restart (see `restart`): once the den before has quit.
    if args.len() >= 2 && args[0] == "--wait-for" {
        let pid = args[1].to_str().and_then(|pid| pid.parse().ok());
        if pid.is_some_and(|pid| !wait_for_exit(pid)) {
            return;
        }
        args.drain(..2);
    }
    // A restart comes back to the windows as they were.
    let restore = args.first().is_some_and(|arg| arg == "--restore");
    if restore {
        args.remove(0);
    }
    // What opens: each window's folders, as its tabs, and the one in front.
    let saved = || -> Vec<(Vec<PathBuf>, usize)> {
        settings::saved_windows()
            .into_iter()
            .map(|window| (window.roots, window.active))
            .collect()
    };
    let windows: Vec<(Vec<PathBuf>, usize)> = match args.first() {
        // The jump list's New Window task (see `workspace::sync_jump_list`).
        Some(arg) if arg == "--dock-action" => {
            vec![(
                vec![settings::last_folder().unwrap_or_else(start_folder)],
                0,
            )]
        }
        Some(_) => {
            let roots = args
                .into_iter()
                // Explorer's "Open with den" on a drive passes "C:\", whose trailing
                // backslash escapes the closing quote: the folder arrives as C:".
                .map(
                    |arg| match arg.to_str().and_then(|arg| arg.strip_suffix('"')) {
                        Some(drive) => PathBuf::from(format!("{drive}\\")),
                        None => PathBuf::from(arg),
                    },
                )
                .collect();
            vec![(roots, 0)]
        }
        // Started from a launcher, or again after an update: the windows
        // last open; from a shell, that shell's folder.
        None if restore || !started_from_shell() => match saved() {
            windows if windows.is_empty() => vec![(vec![start_folder()], 0)],
            windows => windows,
        },
        None => vec![(vec![start_folder()], 0)],
    };
    // A folder another den has open shows there; with every one of them
    // handed over, this den has nothing to show.
    let windows: Vec<(Vec<PathBuf>, usize)> = windows
        .into_iter()
        .filter_map(|(roots, active)| {
            let roots: Vec<PathBuf> = roots
                .into_iter()
                .map(|root| settings::strip_verbatim(std::fs::canonicalize(&root).unwrap_or(root)))
                .collect();
            let front = roots.get(active).cloned();
            let roots = instance::hand_off(roots);
            if roots.is_empty() {
                return None;
            }
            let active = front
                .and_then(|front| roots.iter().position(|root| *root == front))
                .unwrap_or(0);
            Some((roots, active))
        })
        .collect();
    if windows.is_empty() {
        return;
    }

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
            // Before any window takes a folder's lock: a den started on it
            // meanwhile finds this one listening.
            instance::serve(cx);
            frame::Frames::init(cx);

            // The window last in front last, so it is in front again.
            for (roots, active) in windows.into_iter().rev() {
                open_in_new_window(roots, active, cx);
            }
            cx.activate(true);
        });
}
