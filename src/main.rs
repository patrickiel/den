//! den: a project terminal for Windows, on GPUI.
//!
//! See README.md for what is in it and how it is built.

// The built-in themes are large `json!` literals.
#![recursion_limit = "512"]

mod assets;
mod backend;
mod browser;
mod defaults;
mod diff;
mod dirty_diff;
mod encoding;
mod explorer;
mod file_icon;
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
    let Ok((window, _)) = gpui_kit::open_window(options, cx, move |window, cx| cx.new(|cx| workspace::Workspace::new(root, window, cx))) else {
        return;
    };
    window
        .update(cx, |_, window, _| {
            window.activate_window();
            window.set_window_title(&title);
        })
        .ok();
}

fn main() {
    // GPUI composites its frame topmost over child windows through
    // DirectComposition, which would hide the browser tabs' WebView2.
    // Presenting to the window directly puts child windows on top. Read once
    // when the platform starts, so set it before anything else.
    // SAFETY: nothing else runs yet, so no thread reads the environment.
    unsafe { std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1") };
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
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

            cx.bind_keys([
                KeyBinding::new("ctrl-s", SaveFile, None),
                KeyBinding::new("shift-alt-f", FormatDocument, None),
                KeyBinding::new("ctrl-,", OpenSettings, None),
                KeyBinding::new("ctrl-shift-e", FocusExplorer, None),
                KeyBinding::new("ctrl-shift-f", FocusSearch, None),
                KeyBinding::new("ctrl-shift-g", FocusScm, None),
                KeyBinding::new("ctrl-shift-h", ReplaceInFiles, None),
                KeyBinding::new("ctrl-shift-d", SplitRight, None),
                KeyBinding::new("ctrl-shift--", SplitDown, None),
                KeyBinding::new("ctrl-shift-t", NewTerminal, None),
                KeyBinding::new("ctrl-shift-b", NewBrowser, None),
                KeyBinding::new("ctrl-shift-w", CloseTab, None),
                KeyBinding::new("ctrl-w", CloseTab, None),
                KeyBinding::new("ctrl-shift-q", CloseGroup, None),
                KeyBinding::new("ctrl-tab", NextTab, None),
                KeyBinding::new("ctrl-shift-tab", PrevTab, None),
                KeyBinding::new("ctrl-shift-o", OpenFolder, None),
                KeyBinding::new("ctrl-o", OpenFiles, None),
                KeyBinding::new("alt-left", FocusLeft, None),
                KeyBinding::new("alt-right", FocusRight, None),
                KeyBinding::new("alt-up", FocusUp, None),
                KeyBinding::new("alt-down", FocusDown, None),
                KeyBinding::new("alt-f4", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx: &mut App| cx.quit());

            open_workspace(root, None, cx);
            cx.activate(true);
        });
}
