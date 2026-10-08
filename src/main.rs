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
mod layout;
mod layout_file;
mod layout_view;
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
        Quit,
    ]
);

/// A window on the session for `root`, at `bounds` (centred when `None`).
pub fn open_workspace(root: PathBuf, bounds: Option<Bounds<Pixels>>, cx: &mut App) {
    let bounds = bounds.unwrap_or_else(|| Bounds::centered(None, size(px(1400.), px(900.)), cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(640.), px(400.))),
        ..TitleBar::window_options()
    };
    let title = format!("den — {}", root.display());
    extensions::broadcast(den_extension::events::WORKSPACE_OPENED, serde_json::json!({ "root": root }), cx);
    let Ok((window, workspace)) = gpui_kit::open_window(options, cx, {
        let root = root.clone();
        move |window, cx| cx.new(|cx| workspace::Workspace::new(root, window, cx))
    }) else {
        return;
    };
    extensions::window_opened(root, workspace.downgrade(), window, cx);
    window
        .update(cx, |_, window, _| {
            window.activate_window();
            window.set_window_title(&title);
        })
        .ok();
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

/// The menu bar on macOS: the app needs one for Cmd+Q and for the standard
/// look; the keys come from the bindings above.
fn set_menus(cx: &mut App) {
    let menu = |name: &str, items: Vec<MenuItem>| Menu { name: name.to_string().into(), items, disabled: false };
    cx.set_menus(vec![
        menu("den", vec![MenuItem::action("Settings…", OpenSettings), MenuItem::separator(), MenuItem::action("Quit den", Quit)]),
        menu(
            "File",
            vec![
                MenuItem::action("Open Folder…", OpenFolder),
                MenuItem::action("Open Files…", OpenFiles),
                MenuItem::separator(),
                MenuItem::action("New Terminal", NewTerminal),
                MenuItem::action("New Browser", NewBrowser),
                MenuItem::separator(),
                MenuItem::action("Save", SaveFile),
                MenuItem::action("Format Document", FormatDocument),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Close Group", CloseGroup),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Explorer", FocusExplorer),
                MenuItem::action("Search", FocusSearch),
                MenuItem::action("Source Control", FocusScm),
                MenuItem::action("Extensions", FocusExtensions),
                MenuItem::separator(),
                MenuItem::action("Split Right", SplitRight),
                MenuItem::action("Split Down", SplitDown),
                MenuItem::separator(),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
            ],
        ),
    ]);
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

            // Ctrl is the command key on macOS; Ctrl+Tab stays, as in every
            // Mac app with tabs.
            let primary = ui::primary;
            cx.bind_keys([
                KeyBinding::new(&primary("ctrl-s"), SaveFile, None),
                KeyBinding::new("shift-alt-f", FormatDocument, None),
                KeyBinding::new(&primary("ctrl-,"), OpenSettings, None),
                KeyBinding::new(&primary("ctrl-shift-e"), FocusExplorer, None),
                KeyBinding::new(&primary("ctrl-shift-x"), FocusExtensions, None),
                KeyBinding::new(&primary("ctrl-shift-f"), FocusSearch, None),
                KeyBinding::new(&primary("ctrl-shift-g"), FocusScm, None),
                KeyBinding::new(&primary("ctrl-shift-h"), ReplaceInFiles, None),
                KeyBinding::new(&primary("ctrl-shift-d"), SplitRight, None),
                KeyBinding::new(&primary("ctrl-shift--"), SplitDown, None),
                KeyBinding::new(&primary("ctrl-shift-t"), NewTerminal, None),
                KeyBinding::new(&primary("ctrl-shift-b"), NewBrowser, None),
                KeyBinding::new(&primary("ctrl-shift-w"), CloseTab, None),
                KeyBinding::new(&primary("ctrl-w"), CloseTab, None),
                KeyBinding::new(&primary("ctrl-shift-q"), CloseGroup, None),
                KeyBinding::new("ctrl-tab", NextTab, None),
                KeyBinding::new("ctrl-shift-tab", PrevTab, None),
                KeyBinding::new(&primary("ctrl-shift-o"), OpenFolder, None),
                KeyBinding::new(&primary("ctrl-o"), OpenFiles, None),
                KeyBinding::new("alt-left", FocusLeft, None),
                KeyBinding::new("alt-right", FocusRight, None),
                KeyBinding::new("alt-up", FocusUp, None),
                KeyBinding::new("alt-down", FocusDown, None),
                KeyBinding::new(if ui::COMMAND_KEY { "cmd-q" } else { "alt-f4" }, Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
            if ui::COMMAND_KEY {
                set_menus(cx);
            }

            open_workspace(root, None, cx);
            cx.activate(true);
        });
}
