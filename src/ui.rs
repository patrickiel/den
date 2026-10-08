//! Small pieces the views share.

use gpui_kit::component::{Icon, menu::PopupMenuItem};
use gpui_kit::*;

/// Whether the primary modifier is the command key (macOS) rather than Ctrl.
pub const COMMAND_KEY: bool = cfg!(target_os = "macos");

/// Keys in gpui's notation with `ctrl` as the platform's primary modifier:
/// `ctrl-shift-t` stays on Windows and Linux, and is `cmd-shift-t` on macOS.
pub fn primary(keys: &str) -> String {
    if COMMAND_KEY { keys.replace("ctrl-", "cmd-") } else { keys.to_string() }
}

/// A label such as `New Terminal (Ctrl+Shift+T)` with the platform's primary
/// modifier (`Cmd` on macOS).
pub fn key_label(label: &str) -> String {
    if COMMAND_KEY { label.replace("Ctrl+", "Cmd+").replace("Ctrl-", "Cmd-") } else { label.to_string() }
}

/// A menu item running `run` on the view `this` (nothing once it is gone).
pub fn menu_action<V: 'static>(this: &WeakEntity<V>, label: impl Into<SharedString>, run: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static) -> PopupMenuItem {
    let this = this.clone();
    PopupMenuItem::new(label.into()).on_click(move |_, window, cx| {
        _ = this.update(cx, |view, cx| run(view, window, cx));
    })
}

/// A Lucide icon by name, when den has it (each name is looked up once).
pub fn lucide_icon(name: &str, cx: &App) -> Option<Icon> {
    thread_local! {
        static KNOWN: std::cell::RefCell<std::collections::HashMap<String, bool>> = Default::default();
    }
    if name.is_empty() {
        return None;
    }
    let path = format!("icons/{name}.svg");
    let known = KNOWN.with_borrow_mut(|known| *known.entry(path.clone()).or_insert_with(|| cx.asset_source().load(&path).ok().flatten().is_some()));
    known.then(|| Icon::default().path(path))
}
