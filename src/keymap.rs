//! den's keyboard shortcuts: every command, with VS Code's keys as the
//! default on each platform, the keys the user set in Settings over them
//! (`keybindings` in settings.json, by command id), and the keymap gpui
//! dispatches, rebuilt from both whenever they change.
//!
//! Keys are written in den's notation, `ctrl-shift-t` and chords as
//! `ctrl-k ctrl-s`; VS Code's `ctrl+shift+t` is read as well.
//!
//! In a terminal, a plain Ctrl+letter goes to the program running there
//! (Ctrl+W deletes a word, Ctrl+O is Claude Code's transcript), so a command
//! bound to one works everywhere else. Chords (`ctrl-k …`) still work in a
//! terminal; a lone Ctrl+K reaches the shell with the next key.

use std::{collections::BTreeMap, rc::Rc};

use gpui_kit::*;

use crate::settings::Settings;

/// One command keys can run.
pub struct Command {
    /// Its name in settings.json's `keybindings` (`den.newTerminal`).
    pub id: String,
    /// As the command palette and Keyboard Shortcuts list it (`View: Split Right`).
    pub title: String,
    pub action: Box<dyn Action>,
    /// Where it works: everywhere, or in a key context (`Terminal`).
    pub context: Option<&'static str>,
    /// The keys it has without the user's own, on this platform.
    pub defaults: Vec<String>,
    /// The keys it has now, as the platform shows them (`Ctrl+Shift+T`, `⇧⌘T`).
    pub shown: Vec<String>,
}

/// Where a command's context is, in words, for Keyboard Shortcuts.
pub fn context_label(context: &str) -> &'static str {
    match context {
        crate::terminal::TERMINAL => "In a terminal",
        crate::diff::DIFF => "In a diff",
        _ => "",
    }
}

struct Keymap {
    commands: Vec<Command>,
    /// The bindings den does not manage (text fields, lists, menus), as
    /// they were before den's own went in.
    base: Vec<KeyBinding>,
    /// The user's keys the keymap was last built with.
    applied: Option<BTreeMap<String, Vec<String>>>,
}

impl Global for Keymap {}

/// Build the keymap: after every module that binds fixed keys (gpui's
/// components, the terminal's Tab) and after the extensions started.
pub fn init(cx: &mut App) {
    let base = cx.key_bindings().borrow().bindings().cloned().collect();
    let mut commands = den_commands();
    for (extension, name, command) in crate::extensions::commands(cx) {
        let mut keys = normalize(&command.keybinding);
        if let Err(err) = keys.split_whitespace().try_for_each(|key| Keystroke::parse(key).map(drop)) {
            let message = format!("the keybinding \"{}\" of {} is not one den reads: {err}", command.keybinding, command.id);
            crate::backend::extensions::log(&extension, &message);
            keys.clear();
        }
        commands.push(Command {
            id: format!("{extension}.{}", command.id),
            title: format!("{name}: {}", command.title),
            action: Box::new(crate::extensions::RunCommand { extension, command: command.id }),
            context: None,
            defaults: if keys.is_empty() { Vec::new() } else { vec![keys] },
            shown: Vec::new(),
        });
    }
    cx.set_global(Keymap { commands, base, applied: None });
    apply(cx);
    cx.observe_global::<Settings>(apply).detach();
    // A key such as Ctrl+Shift+` is another key on another layout.
    cx.on_keyboard_layout_change(|cx| {
        cx.global_mut::<Keymap>().applied = None;
        apply(cx);
    })
    .detach();
}

/// Every command, in the order Keyboard Shortcuts lists them.
pub fn commands(cx: &App) -> &[Command] {
    cx.try_global::<Keymap>().map_or(&[], |keymap| &keymap.commands)
}

/// The keys `action` has, as the platform shows them (its first binding).
pub fn keys_for(action: &dyn Action, cx: &App) -> Option<String> {
    commands(cx).iter().find(|c| c.action.partial_eq(action)).and_then(|c| c.shown.first().cloned())
}

/// `text (keys)`, for a tooltip; `text` alone when `action` has no keys.
pub fn with_keys(text: &str, action: &dyn Action, cx: &App) -> String {
    match keys_for(action, cx) {
        Some(keys) => format!("{text} ({keys})"),
        None => text.to_string(),
    }
}

/// `label    keys`, for a menu item.
pub fn menu_label(label: &str, action: &dyn Action, cx: &App) -> String {
    match keys_for(action, cx) {
        Some(keys) => format!("{label}    {keys}"),
        None => label.to_string(),
    }
}

/// The keys of `id` with the user's own: theirs when they set some.
fn keys_of<'a>(command: &'a Command, user: &'a BTreeMap<String, Vec<String>>) -> &'a [String] {
    user.get(&command.id).unwrap_or(&command.defaults)
}

/// Rebuild gpui's keymap when the user's keys changed since the last build.
fn apply(cx: &mut App) {
    let user = &Settings::get(cx).keybindings;
    if cx.global::<Keymap>().applied.as_ref() == Some(user) {
        return;
    }
    let user = user.clone();
    let mapper = cx.keyboard_mapper().clone();
    let keymap = cx.global_mut::<Keymap>();
    let bindings = build(&mut keymap.commands, &user, &keymap.base, mapper.as_ref());
    keymap.applied = Some(user);
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
    // The menu bar shows the keys it had when it was set.
    if crate::ui::COMMAND_KEY {
        crate::set_menus(cx);
    }
}

/// The whole keymap: `base`, then the commands' keys (the user's over the
/// defaults), noting in each command the keys it got.
fn build(commands: &mut [Command], user: &BTreeMap<String, Vec<String>>, base: &[KeyBinding], mapper: &dyn PlatformKeyboardMapper) -> Vec<KeyBinding> {
    let mut bindings = base.to_vec();
    let mut contextual = Vec::new();
    // Everywhere first, then the contexts: of a key bound both ways, the
    // binding added later wins, so a terminal's own keys win in a terminal.
    for pass in [false, true] {
        for command in commands.iter_mut().filter(|c| c.context.is_some() == pass) {
            let predicate = command.context.map(|c| Rc::new(KeyBindingContextPredicate::parse(c).expect("a valid key context")));
            command.shown.clear();
            for keys in keys_of(command, user).to_vec() {
                match KeyBinding::load(&normalize(&keys), command.action.boxed_clone(), predicate.clone(), false, None, mapper) {
                    Ok(binding) => {
                        command.shown.push(display(binding.keystrokes()));
                        if pass {
                            contextual.push(binding.keystrokes()[0].inner().clone());
                        }
                        bindings.push(binding);
                    }
                    Err(err) => eprintln!("den: the keys \"{keys}\" of {} are not ones den reads: {err}", command.id),
                }
            }
        }
    }
    // A plain Ctrl+letter that runs a command reaches a terminal's program
    // instead, unless the terminal binds it itself (Ctrl+V pastes).
    let mut passed: Vec<(Keystroke, String)> = Vec::new();
    for binding in &bindings {
        if binding.predicate().is_some() || binding.keystrokes().len() != 1 {
            continue;
        }
        let stroke = binding.keystrokes()[0].inner();
        let Some(text) = crate::terminal::control_text(stroke) else { continue };
        if !contextual.contains(stroke) && !passed.iter().any(|(s, _)| s == stroke) {
            passed.push((stroke.clone(), text));
        }
    }
    for (stroke, text) in passed {
        bindings.push(KeyBinding::new(&stroke.unparse(), crate::terminal::SendText(text), Some(crate::terminal::TERMINAL)));
    }
    bindings
}

/// Keys in den's notation, from either it or VS Code's (`ctrl+shift+t`,
/// `cmd+k cmd+s`, `ctrl++`).
pub fn normalize(keys: &str) -> String {
    let word = |word: &str| match word.to_ascii_lowercase().as_str() {
        "control" => "ctrl".to_string(),
        "option" | "opt" => "alt".to_string(),
        "command" | "meta" => "cmd".to_string(),
        "esc" => "escape".to_string(),
        "return" => "enter".to_string(),
        other => other.to_string(),
    };
    keys.split_whitespace()
        .map(|stroke| {
            if !stroke.contains('+') || stroke == "+" {
                return stroke.to_string();
            }
            let (body, plus) = match stroke.strip_suffix("++") {
                Some(body) => (body, true),
                None => (stroke, false),
            };
            let mut parts: Vec<String> = body.split('+').filter(|p| !p.is_empty()).map(word).collect();
            if plus {
                parts.push("+".into());
            }
            parts.join("-")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Keys, in either notation, as keycaps, one list per keystroke of a chord; `None` when den
/// can't read them.
pub fn shown_caps(keys: &str, cx: &App) -> Option<Vec<Vec<String>>> {
    let binding = KeyBinding::load(&normalize(keys), Box::new(NoAction), None, false, None, cx.keyboard_mapper().as_ref()).ok()?;
    let strokes = binding.keystrokes();
    (!strokes.is_empty()).then(|| strokes.iter().map(|s| stroke_caps(s.modifiers(), key_name(s.key()))).collect())
}

/// Keystrokes as the platform shows them:`Ctrl+K Ctrl+S` on Windows,
/// `⌘K ⌘S` on macOS, as VS Code writes them.
pub fn display(strokes: &[KeybindingKeystroke]) -> String {
    strokes.iter().map(|s| display_stroke(s.modifiers(), s.key())).collect::<Vec<_>>().join(" ")
}

fn display_stroke(m: &Modifiers, key: &str) -> String {
    stroke_caps(m, key_name(key)).join(if crate::ui::COMMAND_KEY { "" } else { "+" })
}

/// A key as the platform names it: `Enter`, `PageUp`, `T`; ↩ on macOS.
fn key_name(key: &str) -> String {
    let mac = crate::ui::COMMAND_KEY;
    let named = match key {
        "enter" if mac => "↩",
        "tab" if mac => "⇥",
        "escape" if mac => "⎋",
        "backspace" if mac => "⌫",
        "delete" if mac => "⌦",
        "left" if mac => "←",
        "right" if mac => "→",
        "up" if mac => "↑",
        "down" if mac => "↓",
        "enter" => "Enter",
        "tab" => "Tab",
        "escape" => "Esc",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "space" => "Space",
        _ => "",
    };
    if named.is_empty() { key.to_uppercase() } else { named.to_string() }
}

/// A keystroke's keycaps: on macOS one (`⇧⌘P`, as its menus write it), elsewhere
/// one per key in VS Code's order, Ctrl+Shift+Alt+Win (`Ctrl`, `Shift`, `P`).
fn stroke_caps(m: &Modifiers, key: String) -> Vec<String> {
    if crate::ui::COMMAND_KEY {
        // Apple's order: ⌃⌥⇧⌘.
        let mut cap = String::new();
        for (on, symbol) in [(m.control, "⌃"), (m.alt, "⌥"), (m.shift, "⇧"), (m.platform, "⌘"), (m.function, "fn")] {
            if on {
                cap.push_str(symbol);
            }
        }
        vec![cap + &key]
    } else {
        let mut caps: Vec<String> = [(m.control, "Ctrl"), (m.shift, "Shift"), (m.alt, "Alt"), (m.platform, "Win"), (m.function, "Fn")]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, name)| name.to_string())
            .collect();
        caps.push(key);
        caps
    }
}

/// A command of den's own, with its default keys on Windows and on macOS.
fn command(id: &str, title: &str, action: impl Action, windows: &[&str], mac: &[&str]) -> Command {
    let keys = if crate::ui::COMMAND_KEY { mac } else { windows };
    Command {
        id: id.to_string(),
        title: title.to_string(),
        action: Box::new(action),
        context: None,
        defaults: keys.iter().map(|k| k.to_string()).collect(),
        shown: Vec::new(),
    }
}

impl Command {
    fn only_in(mut self, context: &'static str) -> Self {
        self.context = Some(context);
        self
    }
}

/// den's commands with VS Code's keys where VS Code has the command, and
/// den's own (Windows Terminal's) beside them where they do not collide.
fn den_commands() -> Vec<Command> {
    use crate::*;
    let mut list = vec![
        command("den.showCommands", "Show All Commands", ShowCommands, &["ctrl-shift-p", "f1"], &["cmd-shift-p", "f1"]),
        // File
        command("den.openFolder", "File: Open Folder…", OpenFolder, &["ctrl-k ctrl-o", "ctrl-shift-o"], &["cmd-k cmd-o", "cmd-shift-o"]),
        command("den.openFolderInNewWindow", "File: Open Folder in New Window…", OpenFolderInNewWindow, &["ctrl-shift-n"], &["cmd-shift-n"]),
        command("den.openFiles", "File: Open Files…", OpenFiles, &["ctrl-o"], &["cmd-o"]),
        command("den.save", "File: Save", SaveFile, &["ctrl-s"], &["cmd-s"]),
        command("den.closeTab", "View: Close Tab", CloseTab, &["ctrl-w", "ctrl-f4", "ctrl-shift-w"], &["cmd-w"]),
        command("den.closeGroup", "View: Close Group", CloseGroup, &["ctrl-k w", "ctrl-shift-q"], &["cmd-k w"]),
        // Editor
        command("den.formatDocument", "Editor: Format Document", FormatDocument, &["shift-alt-f"], &["shift-alt-f"]),
        command("den.goToLine", "Editor: Go to Line…", GoToLine, &["ctrl-g"], &["ctrl-g"]),
        // Preferences
        command("den.openSettings", "Preferences: Open Settings", OpenSettings, &["ctrl-,"], &["cmd-,"]),
        command("den.openKeyboardShortcuts", "Preferences: Keyboard Shortcuts", OpenKeyboardShortcuts, &["ctrl-k ctrl-s"], &["cmd-k cmd-s"]),
        // Sidebar
        command("den.toggleSidebar", "View: Toggle Sidebar", ToggleSidebar, &["ctrl-b"], &["cmd-b"]),
        command("den.showExplorer", "View: Show Explorer", FocusExplorer, &["ctrl-shift-e"], &["cmd-shift-e"]),
        command("den.showSearch", "View: Search in Files", FocusSearch, &["ctrl-shift-f"], &["cmd-shift-f"]),
        command("den.replaceInFiles", "View: Replace in Files", ReplaceInFiles, &["ctrl-shift-h"], &["cmd-shift-h"]),
        command("den.showSourceControl", "View: Show Source Control", FocusScm, &["ctrl-shift-g"], &["ctrl-shift-g"]),
        command("den.showExtensions", "View: Show Extensions", FocusExtensions, &["ctrl-shift-x"], &["cmd-shift-x"]),
        // Groups and tabs
        command("den.splitRight", "View: Split Right", SplitRight, &["ctrl-\\", "ctrl-shift-d"], &["cmd-\\", "cmd-shift-d"]),
        command("den.splitDown", "View: Split Down", SplitDown, &["ctrl-k ctrl-\\", "ctrl-shift--"], &["cmd-k cmd-\\", "cmd-shift--"]),
        command("den.nextTab", "View: Next Tab", NextTab, &["ctrl-tab", "ctrl-pagedown"], &["ctrl-tab", "cmd-alt-right", "cmd-shift-]", "ctrl-pagedown"]),
        command("den.previousTab", "View: Previous Tab", PrevTab, &["ctrl-shift-tab", "ctrl-pageup"], &["ctrl-shift-tab", "cmd-alt-left", "cmd-shift-[", "ctrl-pageup"]),
    ];
    for n in 1..=9 {
        let (windows, mac) = (format!("alt-{n}"), format!("ctrl-{n}"));
        list.push(command(&format!("den.openTab{n}"), &format!("View: Open Tab {n}"), OpenTab(n), &[&windows], &[&mac]));
    }
    list.push(command("den.openLastTab", "View: Open Last Tab", OpenTab(0), &["alt-0"], &["ctrl-0"]));
    for n in 1..=8 {
        let (windows, mac) = (format!("ctrl-{n}"), format!("cmd-{n}"));
        list.push(command(&format!("den.focusGroup{n}"), &format!("View: Focus Group {n}"), FocusGroup(n), &[&windows], &[&mac]));
    }
    list.extend([
        command("den.focusLeft", "View: Focus Group on the Left", FocusLeft, &["ctrl-k ctrl-left"], &["cmd-k cmd-left"]),
        command("den.focusRight", "View: Focus Group on the Right", FocusRight, &["ctrl-k ctrl-right"], &["cmd-k cmd-right"]),
        command("den.focusUp", "View: Focus Group Above", FocusUp, &["ctrl-k ctrl-up"], &["cmd-k cmd-up"]),
        command("den.focusDown", "View: Focus Group Below", FocusDown, &["ctrl-k ctrl-down"], &["cmd-k cmd-down"]),
        command("den.navigateBack", "Go: Back", NavigateBack, &["alt-left"], &["ctrl--"]),
        command("den.navigateForward", "Go: Forward", NavigateForward, &["alt-right"], &["ctrl-shift--"]),
        command("den.saveLayout", "View: Save Layout…", SaveLayout, &[], &[]),
        command("den.resetLayout", "View: Reset Layout", ResetLayout, &[], &[]),
        // Terminals and browsers
        command("den.newTerminal", "Terminal: New Terminal", NewTerminal, &["ctrl-shift-`", "ctrl-shift-t"], &["ctrl-shift-`", "cmd-shift-t"]),
        command("den.newAgent", "Terminal: New Coding Agent", NewAgent, &[], &[]),
        command("terminal.copy", "Terminal: Copy Selection", crate::terminal::Copy, &["ctrl-shift-c"], &["cmd-c"]).only_in(crate::terminal::TERMINAL),
        command("terminal.paste", "Terminal: Paste", crate::terminal::Paste, &["ctrl-v", "ctrl-shift-v"], &["cmd-v"]).only_in(crate::terminal::TERMINAL),
        command("terminal.scrollPageUp", "Terminal: Scroll Up a Page", crate::terminal::ScrollPageUp, &["shift-pageup"], &["shift-pageup"]).only_in(crate::terminal::TERMINAL),
        command("terminal.scrollPageDown", "Terminal: Scroll Down a Page", crate::terminal::ScrollPageDown, &["shift-pagedown"], &["shift-pagedown"]).only_in(crate::terminal::TERMINAL),
        command("den.newBrowser", "Browser: New Browser", NewBrowser, &["ctrl-shift-b"], &["cmd-shift-b"]),
        command("diff.openFile", "Diff: Open the File", crate::diff::OpenDiffedFile, &["ctrl-enter"], &["cmd-enter"]).only_in(crate::diff::DIFF),
        // The app
        command("den.checkForUpdates", "den: Check for Updates…", CheckForUpdates, &[], &[]),
        command("den.quit", "den: Quit", Quit, &["alt-f4"], &["cmd-q"]),
    ]);
    if crate::ui::COMMAND_KEY {
        list.extend([
            command("den.minimize", "Window: Minimize", Minimize, &[], &["cmd-m"]),
            command("den.hide", "den: Hide den", HideApp, &[], &["cmd-h"]),
            command("den.hideOthers", "den: Hide Others", HideOthers, &[], &["alt-cmd-h"]),
        ]);
    }
    list
}

/// The command palette: every command, by name, with its keys.
pub fn open_palette(window: &mut Window, cx: &mut App) {
    use gpui_kit::component::{
        ActiveTheme as _, WindowExt as _, h_flex,
        command::{Command as Palette, CommandItem, CommandState},
    };
    /// A command's title, keys and action.
    type Entry = (SharedString, Option<String>, Box<dyn Action>);
    let state = cx.new(|cx| CommandState::new(window, cx));
    let entries: Rc<Vec<Entry>> =
        Rc::new(commands(cx).iter().map(|c| (SharedString::from(c.title.clone()), c.shown.first().cloned(), c.action.boxed_clone())).collect());
    window.open_dialog(cx, {
        let state = state.clone();
        move |dialog, _, _| {
            let items = entries.iter().map(|(title, keys, _)| {
                let (title, keys) = (title.clone(), keys.clone());
                CommandItem::new().label(title.clone()).keywords(keys.clone()).child(move |_, cx| {
                    h_flex()
                        .w_full()
                        .gap_4()
                        .justify_between()
                        .child(div().min_w_0().truncate().child(title.clone()))
                        .children(keys.clone().map(|keys| div().flex_none().text_xs().text_color(cx.theme().muted_foreground).child(keys)))
                })
            });
            let entries = entries.clone();
            dialog.w(px(640.)).margin_top(px(48.)).close_button(false).child(
                Palette::new(&state)
                    .items(items)
                    .placeholder("Type the name of a command")
                    .bordered(false)
                    .on_confirm(move |ix, window, cx| {
                        let Some((_, _, action)) = entries.get(ix.row) else { return };
                        let action = action.boxed_clone();
                        window.close_dialog(cx);
                        // Where the keys would have sent it, once the dialog is gone.
                        window.defer(cx, move |window, cx| {
                            if let Some(focus) = crate::workspace::action_target(window, cx) {
                                focus.dispatch_action(&*action, window, cx);
                            }
                        });
                    })
                    .on_cancel(|window, cx| window.close_dialog(cx)),
            )
        }
    });
    window.defer(cx, move |window, cx| state.update(cx, |state, cx| state.focus(window, cx)));
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would come with it.
    use super::{den_commands, display, normalize};
    use gpui_kit::{DummyKeyboardMapper, KeyBinding};

    #[test]
    fn reads_vscode_notation() {
        assert_eq!(normalize("ctrl+shift+t"), "ctrl-shift-t");
        assert_eq!(normalize("Ctrl+K Ctrl+S"), "ctrl-k ctrl-s");
        assert_eq!(normalize("cmd+option+left"), "cmd-alt-left");
        assert_eq!(normalize("ctrl++"), "ctrl-+");
        assert_eq!(normalize("ctrl+-"), "ctrl--");
        assert_eq!(normalize("ctrl-shift--"), "ctrl-shift--");
        assert_eq!(normalize("ctrl-k  ctrl-\\"), "ctrl-k ctrl-\\");
    }

    #[test]
    fn every_default_parses_and_ids_are_unique() {
        let commands = den_commands();
        let mut ids = std::collections::HashSet::new();
        for command in &commands {
            assert!(ids.insert(command.id.clone()), "{} twice", command.id);
            for keys in &command.defaults {
                assert!(KeyBinding::load(keys, command.action.boxed_clone(), None, false, None, &DummyKeyboardMapper).is_ok(), "{keys}");
            }
        }
    }

    #[test]
    fn no_two_defaults_share_keys() {
        let mut seen = std::collections::HashMap::new();
        for command in den_commands() {
            for keys in &command.defaults {
                if let Some(other) = seen.insert((command.context, normalize(keys)), command.id.clone()) {
                    panic!("{keys} is {other}'s and {}'s", command.id);
                }
            }
        }
    }

    /// What `keys` run where `context` (a terminal, `None` elsewhere) has the
    /// keyboard: the winning action, and whether more keys may follow.
    fn run(keymap: &gpui_kit::Keymap, keys: &str, context: Option<&str>) -> (Option<Box<dyn gpui_kit::Action>>, bool) {
        let input: Vec<_> = keys.split(' ').map(|k| gpui_kit::Keystroke::parse(k).unwrap()).collect();
        let stack: Vec<_> = context.into_iter().map(|c| gpui_kit::KeyContext::parse(c).unwrap()).collect();
        let (bindings, pending) = keymap.bindings_for_input(&input, &stack);
        (bindings.first().map(|b| b.action().boxed_clone()), pending)
    }

    fn keymap(user: &[(&str, &[&str])]) -> gpui_kit::Keymap {
        let user = user.iter().map(|(id, keys)| (id.to_string(), keys.iter().map(|k| k.to_string()).collect())).collect();
        gpui_kit::Keymap::new(super::build(&mut den_commands(), &user, &[], &DummyKeyboardMapper))
    }

    #[test]
    fn a_terminal_keeps_ctrl_letters_and_chords_still_work() {
        if crate::ui::COMMAND_KEY {
            return;
        }
        let keymap = keymap(&[]);
        let terminal = Some(crate::terminal::TERMINAL);
        // Claude Code's Ctrl+O, and Ctrl+W deleting a word.
        let (action, _) = run(&keymap, "ctrl-o", terminal);
        assert!(action.unwrap().partial_eq(&crate::terminal::SendText("\x0f".into())));
        let (action, _) = run(&keymap, "ctrl-w", terminal);
        assert!(action.unwrap().partial_eq(&crate::terminal::SendText("\x17".into())));
        // Elsewhere they are den's.
        assert!(run(&keymap, "ctrl-o", None).0.unwrap().partial_eq(&crate::OpenFiles));
        assert!(run(&keymap, "ctrl-w", None).0.unwrap().partial_eq(&crate::CloseTab));
        // The terminal's own Ctrl+V, and keys with Shift, stay commands.
        assert!(run(&keymap, "ctrl-v", terminal).0.unwrap().partial_eq(&crate::terminal::Paste));
        assert!(run(&keymap, "ctrl-shift-w", terminal).0.unwrap().partial_eq(&crate::CloseTab));
        // Ctrl+K waits for the chord's second key.
        assert!(matches!(run(&keymap, "ctrl-k", terminal), (None, true)));
        assert!(run(&keymap, "ctrl-k ctrl-o", terminal).0.unwrap().partial_eq(&crate::OpenFolder));
    }

    #[test]
    fn the_users_keys_replace_the_defaults() {
        let keymap = keymap(&[("den.newTerminal", &["ctrl+alt+t"]), ("den.save", &[])]);
        assert!(run(&keymap, "ctrl-alt-t", None).0.unwrap().partial_eq(&crate::NewTerminal));
        assert!(run(&keymap, if crate::ui::COMMAND_KEY { "cmd-shift-t" } else { "ctrl-shift-t" }, None).0.is_none());
        assert!(run(&keymap, if crate::ui::COMMAND_KEY { "cmd-s" } else { "ctrl-s" }, None).0.is_none());
    }

    #[test]
    fn shows_keys_as_the_platform_does() {
        let binding = KeyBinding::new("ctrl-k ctrl-shift-t", crate::SaveFile, None);
        let shown = display(binding.keystrokes());
        if crate::ui::COMMAND_KEY {
            assert_eq!(shown, "⌃K ⌃⇧T");
        } else {
            assert_eq!(shown, "Ctrl+K Ctrl+Shift+T");
        }
    }
}
