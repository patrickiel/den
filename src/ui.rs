//! Small pieces the views share.

use gpui_kit::component::{Icon, menu::PopupMenuItem};
use gpui_kit::*;

/// Whether the primary modifier is the command key (macOS) rather than Ctrl.
pub const COMMAND_KEY: bool = cfg!(target_os = "macos");

/// A label such as `New Terminal (Ctrl+Shift+T)` with its keys as the
/// platform shows them: on macOS as its menus do, `New Terminal (⇧⌘T)`.
pub fn key_label(label: &str) -> String {
    if COMMAND_KEY { mac_keys(label) } else { label.to_string() }
}

/// The keys in `label` as macOS writes them: Ctrl (the primary modifier) as
/// ⌘, the modifiers in Apple's order (⌥⇧⌘) and named keys as symbols.
fn mac_keys(label: &str) -> String {
    const MODIFIERS: [(&str, char); 3] = [("Alt+", '⌥'), ("Shift+", '⇧'), ("Ctrl+", '⌘')];
    let mut out = String::new();
    let mut rest = label;
    while let Some(c) = rest.chars().next() {
        let mut held = [false; 3];
        let mut chord = rest;
        while let Some(ix) = MODIFIERS.iter().position(|(name, _)| chord.starts_with(name)) {
            held[ix] = true;
            chord = &chord[MODIFIERS[ix].0.len()..];
        }
        if chord.len() == rest.len() {
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        out.extend(MODIFIERS.iter().zip(held).filter(|(_, on)| *on).map(|((_, symbol), _)| symbol));
        // The key: a word (`T`, `Enter`, `F5`) or one sign (`,`, `-`).
        let len = chord.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(chord.len()).max(chord.chars().next().map_or(0, char::len_utf8));
        let (key, tail) = chord.split_at(len);
        out.push_str(match key {
            "Enter" => "↩",
            "Tab" => "⇥",
            "Esc" => "⎋",
            "Backspace" => "⌫",
            "Delete" => "⌦",
            "Left" => "←",
            "Right" => "→",
            "Up" => "↑",
            "Down" => "↓",
            key => key,
        });
        rest = tail;
    }
    out
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

#[cfg(test)]
mod tests {
    use super::mac_keys;

    #[test]
    fn writes_keys_as_macos_does() {
        assert_eq!(mac_keys("New Terminal (Ctrl+Shift+T)"), "New Terminal (⇧⌘T)");
        assert_eq!(mac_keys("Format Document (Shift+Alt+F)"), "Format Document (⌥⇧F)");
        assert_eq!(mac_keys("Settings (Ctrl+,)"), "Settings (⌘,)");
        assert_eq!(mac_keys("Split down (Ctrl+Shift+-)"), "Split down (⇧⌘-)");
        assert_eq!(mac_keys("Message (Ctrl+Enter to commit)"), "Message (⌘↩ to commit)");
        assert_eq!(mac_keys("Ctrl+S formats the file"), "⌘S formats the file");
        assert_eq!(mac_keys("Match Case (Alt+C)"), "Match Case (⌥C)");
        assert_eq!(mac_keys("No keys — here"), "No keys — here");
    }
}
