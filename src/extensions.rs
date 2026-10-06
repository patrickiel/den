//! Extensions, as den runs them: loaded at start, each on a thread of its
//! own (`backend/extensions.rs`), their state in the `Extensions` global for
//! Settings and the Extensions view, their toasts shown in den's window, their
//! buttons in the title bar of the windows on the folder they are for.
//!
//! Loading happens once per start. Enabling, disabling, installing, updating
//! and uninstalling are recorded now and take effect at the next start; until
//! then the entry says so.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use den_extension::{Button, Command, Manifest};
use futures::StreamExt as _;
use gpui_kit::*;
use serde_json::Value;

use crate::backend::extensions::{self as backend, Listing, Message, Report};
use crate::settings::Settings;

/// Runs an extension's command: from its keybinding, in the focused window.
#[derive(Clone, PartialEq, Eq, serde::Deserialize, Action)]
#[action(namespace = extensions, no_json)]
pub struct RunCommand {
    pub extension: String,
    pub command: String,
}

/// The change an uninstall leaves; such an entry offers nothing more.
pub const REMOVED: &str = "Removed after a restart.";

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Loading,
    Loaded,
    Disabled,
    Failed(String),
    /// Installed during this session; loads at the next start.
    Installed,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    /// Its folder, with its README.md; `<id>.pending` until an install lands.
    pub dir: PathBuf,
    pub manifest: Option<Manifest>,
    pub status: Status,
    /// What the next start changes, set in this session.
    pub change: Option<String>,
}

impl Entry {
    pub fn name(&self) -> &str {
        self.manifest.as_ref().map_or(&self.id, |m| &m.name)
    }

    /// Whether this start runs it (or tried to).
    pub fn started(&self) -> bool {
        matches!(self.status, Status::Loading | Status::Loaded | Status::Failed(_))
    }
}

/// Fetching the curated index of extensions.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum IndexState {
    #[default]
    Idle,
    Loading,
    Failed(String),
}

/// How many icons are fetched at once.
const ICON_FETCHES: usize = 4;

fn icon_key(listing: &Listing) -> String {
    format!("{}-{}", listing.id, listing.version)
}

/// Fetch the icons of these listings that aren't kept yet, a few at a time.
/// Called for the cards being drawn, so a long index costs only what is seen.
pub fn want_icons(listings: Vec<Listing>, cx: &mut App) {
    let extensions = cx.global_mut::<Extensions>();
    for listing in listings {
        // Each icon is looked at once a session; this runs as cards draw.
        if !extensions.icons_asked.insert(icon_key(&listing)) {
            continue;
        }
        if backend::icon_path(&listing).is_some_and(|path| !path.is_file()) {
            extensions.icon_queue.push_back(listing);
        }
    }
    let start = ICON_FETCHES.saturating_sub(extensions.icon_fetches).min(extensions.icon_queue.len());
    extensions.icon_fetches += start;
    for _ in 0..start {
        cx.spawn(async move |cx| {
            // Each runner takes icons off the queue until it is empty.
            while let Some(listing) = cx.update(|cx| cx.global_mut::<Extensions>().icon_queue.pop_front()) {
                let fetched = cx.background_spawn(async move { backend::fetch_icon(&listing) }).await;
                if fetched.is_ok() {
                    cx.update(|cx| cx.refresh_windows());
                }
            }
            cx.update(|cx| cx.global_mut::<Extensions>().icon_fetches -= 1);
        })
        .detach();
    }
}

/// A cached index younger than this is not fetched again at start.
const INDEX_MAX_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub struct Extensions {
    pub entries: Vec<Entry>,
    /// The curated index, installed ones included.
    pub available: Vec<Listing>,
    pub index: IndexState,
    /// The index was fetched, or found fresh in the cache, in this session.
    index_current: bool,
    /// Icons of listed extensions to fetch, as their cards are drawn.
    icon_queue: std::collections::VecDeque<Listing>,
    /// Icons already looked at this session, by [`icon_key`]: kept, queued,
    /// fetched or failed (a failed one is tried again at the next start).
    icons_asked: std::collections::HashSet<String>,
    /// Icon fetches running, at most [`ICON_FETCHES`].
    icon_fetches: usize,
    running: Vec<(String, mpsc::Sender<Message>)>,
    /// What each extension put in the title bar, by its id and the folder.
    buttons: Vec<Buttons>,
    /// What each extension's views show, by extension, folder and view.
    views: Vec<ViewData>,
    /// The open windows by their folder, for `run_in_terminal`.
    windows: Vec<(PathBuf, WeakEntity<crate::workspace::Workspace>, AnyWindowHandle)>,
}

pub struct Buttons {
    pub id: String,
    pub root: PathBuf,
    pub buttons: Vec<Button>,
}

/// What an extension's view on a folder shows; kept while den runs, so a
/// tab opened later (or moved to another window) shows it at once.
pub struct ViewData {
    pub id: String,
    pub root: PathBuf,
    pub view: String,
    pub content: den_extension::view::Content,
}

impl Global for Extensions {}

impl Extensions {
    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    /// The extensions' buttons for the window on `root`, by extension.
    pub fn buttons_for<'a>(&'a self, root: &'a Path) -> impl Iterator<Item = &'a Buttons> {
        self.buttons.iter().filter(move |b| b.root == root)
    }

    /// What extension `id`'s view `view` on `root` shows, if it said yet.
    pub fn view(&self, id: &str, root: &Path, view: &str) -> Option<&den_extension::view::Content> {
        self.views.iter().find(|v| v.id == id && v.root == root && v.view == view).map(|v| &v.content)
    }

    /// The same, to change it here (a click's selection, before the extension answers).
    pub fn view_mut(&mut self, id: &str, root: &Path, view: &str) -> &mut den_extension::view::Content {
        let ix = match self.views.iter().position(|v| v.id == id && v.root == root && v.view == view) {
            Some(ix) => ix,
            None => {
                self.views.push(ViewData { id: id.to_string(), root: root.to_path_buf(), view: view.to_string(), content: Default::default() });
                self.views.len() - 1
            }
        };
        &mut self.views[ix].content
    }

    /// The manifest's declaration of extension `id`'s view `view`.
    pub fn view_kind(&self, id: &str, view: &str) -> Option<&den_extension::View> {
        self.entries.iter().find(|e| e.id == id)?.manifest.as_ref()?.views.iter().find(|v| v.id == view)
    }
}

/// Start every installed extension that is not disabled.
pub fn init(cx: &mut App) {
    backend::apply_pending();
    let (reports, mut incoming) = futures::channel::mpsc::unbounded();
    let disabled = Settings::get(cx).disabled_extensions.clone();
    let mut extensions = Extensions::default();
    for installed in backend::installed() {
        let (manifest, status) = match installed.manifest {
            Err(err) => (None, Status::Failed(err)),
            Ok(manifest) if disabled.contains(&installed.id) => (Some(manifest), Status::Disabled),
            Ok(manifest) => {
                let settings = manifest.settings_values(&user_settings(&installed.id, cx));
                let tx = backend::start(manifest.clone(), installed.dir.clone(), settings, reports.clone());
                extensions.running.push((installed.id.clone(), tx));
                (Some(manifest), Status::Loading)
            }
        };
        extensions.entries.push(Entry { id: installed.id, dir: installed.dir, manifest, status, change: None });
    }
    bind_commands(&extensions.entries, cx);
    cx.set_global(extensions);

    cx.spawn(async move |cx| {
        while let Some(report) = incoming.next().await {
            _ = cx.update(|cx| on_report(report, cx));
        }
    })
    .detach();

    // den waits a moment for each to deactivate, together, then quits anyway.
    cx.on_app_quit(|cx| {
        let running = std::mem::take(&mut cx.global_mut::<Extensions>().running);
        let done: Vec<_> = running
            .into_iter()
            .filter_map(|(_, tx)| {
                let (done_tx, done_rx) = mpsc::channel();
                tx.send(Message::Deactivate(done_tx)).ok().map(|()| done_rx)
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(1);
        for rx in done {
            _ = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
        async {}
    })
    .detach();
}

fn on_report(report: Report, cx: &mut App) {
    let (id, status, toast) = match report {
        Report::Loaded(id) => (id, Some(Status::Loaded), None),
        Report::Failed(id, err) => (id, Some(Status::Failed(err.clone())), Some(format!("failed to start: {err}"))),
        Report::Toast(id, message) => (id, None, Some(message)),
        Report::Buttons { id, root, buttons } => {
            let extensions = cx.global_mut::<Extensions>();
            let root = PathBuf::from(root);
            extensions.buttons.retain(|b| !(b.id == id && b.root == root));
            if !buttons.is_empty() {
                extensions.buttons.push(Buttons { id, root, buttons });
            }
            cx.refresh_windows();
            return;
        }
        Report::Run { root, command, cwd } => return run_in_terminal(Path::new(&root), command, cwd.map(PathBuf::from), cx),
        Report::OpenFile { root, path, line, column } => return open_file(Path::new(&root), PathBuf::from(path), line, column, cx),
        Report::OpenView { id, root, view } => {
            return in_window(Path::new(&root), "open_view", cx, |workspace, window, cx| workspace.open_extension_view(id, view, window, cx));
        }
        Report::SetView { id, root, view, content } => {
            cx.global_mut::<Extensions>().view_mut(&id, Path::new(&root), &view).merge(*content);
            cx.refresh_windows();
            return;
        }
        Report::Prompt { id, root, prompt } => {
            return in_window(Path::new(&root), "prompt", cx, |_, window, cx| crate::extension_view::prompt(id, PathBuf::from(&root), prompt, window, cx));
        }
        Report::OpenDiff { root, diff } => {
            return in_window(Path::new(&root), "open_diff", cx, |workspace, window, cx| workspace.open_extension_diff(diff, window, cx));
        }
        Report::Copy(text) => return cx.write_to_clipboard(ClipboardItem::new_string(text)),
    };
    let extensions = cx.global_mut::<Extensions>();
    let Some(entry) = extensions.entry_mut(&id) else { return };
    if let Some(status) = status {
        entry.status = status;
    }
    let name = entry.name().to_string();
    if let Some(message) = toast {
        show_toast(name, message, cx);
    }
    cx.refresh_windows();
}

fn show_toast(title: String, message: String, cx: &mut App) {
    use gpui_kit::component::notification::Notification;
    let Some(window) = cx.active_window().or_else(|| cx.windows().first().copied()) else { return };
    _ = window.update(cx, |_, window, cx| crate::toast::push(window, Notification::new().title(title).message(message), cx));
}

/// Read the index: from the cache, then from the web when the cache is old,
/// absent or `force`d (the view's refresh button). Once per session otherwise.
pub fn refresh_index(force: bool, cx: &mut App) {
    let extensions = cx.global_mut::<Extensions>();
    if extensions.index == IndexState::Loading || (extensions.index_current && !force) {
        return;
    }
    if extensions.available.is_empty()
        && let Some((listings, age)) = backend::cached_index()
    {
        extensions.available = listings;
        if age < INDEX_MAX_AGE && !force {
            extensions.index_current = true;
            cx.refresh_windows();
            return;
        }
    }
    extensions.index = IndexState::Loading;
    cx.refresh_windows();
    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async {
                let listings = backend::fetch_index()?;
                Ok::<_, String>(listings)
            })
            .await;
        cx.update(|cx| {
            let extensions = cx.global_mut::<Extensions>();
            extensions.index_current = true;
            match result {
                Ok(listings) => {
                    extensions.available = listings;
                    extensions.index = IndexState::Idle;
                }
                Err(err) => extensions.index = IndexState::Failed(err),
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// The listings not installed, in index order.
pub fn uninstalled<'a>(available: &'a [Listing], entries: &[Entry]) -> Vec<&'a Listing> {
    available.iter().filter(|l| !entries.iter().any(|e| e.id == l.id)).collect()
}

/// The index's listing for `entry` when it has a newer version than installed.
pub fn update_available<'a>(entry: &Entry, available: &'a [Listing]) -> Option<&'a Listing> {
    let installed = &entry.manifest.as_ref()?.version;
    available.iter().find(|l| l.id == entry.id && l.loadable() && crate::update::newer(&l.version, installed))
}

/// A window opened on `root`; it is forgotten once it closes.
pub fn window_opened(root: PathBuf, workspace: WeakEntity<crate::workspace::Workspace>, window: AnyWindowHandle, cx: &mut App) {
    if !cx.has_global::<Extensions>() {
        return;
    }
    let extensions = cx.global_mut::<Extensions>();
    extensions.windows.retain(|(_, workspace, _)| workspace.upgrade().is_some());
    extensions.windows.push((root, workspace, window));
}

/// Run `f` on the workspace of the window on `root`, logged when there is none.
fn in_window(root: &Path, what: &str, cx: &mut App, f: impl FnOnce(&mut crate::workspace::Workspace, &mut Window, &mut Context<crate::workspace::Workspace>)) {
    let window = cx.global::<Extensions>().windows.iter().find(|(r, w, _)| r == root && w.upgrade().is_some()).map(|(_, w, h)| (w.clone(), *h));
    let Some((workspace, handle)) = window else {
        backend::log("den", &format!("{what}: no window is open on {}", root.display()));
        return;
    };
    _ = handle.update(cx, |_, window, cx| {
        _ = workspace.update(cx, |workspace, cx| f(workspace, window, cx));
    });
}

fn run_in_terminal(root: &Path, command: String, cwd: Option<PathBuf>, cx: &mut App) {
    in_window(root, "run_in_terminal", cx, |workspace, window, cx| workspace.run_in_terminal(command, cwd, window, cx));
}

fn open_file(root: &Path, path: PathBuf, line: Option<u32>, column: Option<u32>, cx: &mut App) {
    // Relative to the folder, as an extension may well name it.
    let path = if path.is_relative() { root.join(path) } else { path };
    in_window(root, "open_file", cx, |workspace, window, cx| workspace.open_file_at(path, line, column, window, cx));
}

/// The commands of the extensions this start runs, with the extension's id
/// and name, for den's menu.
pub fn commands(cx: &App) -> Vec<(String, String, Command)> {
    let Some(extensions) = cx.try_global::<Extensions>() else { return Vec::new() };
    extensions
        .entries
        .iter()
        .filter(|e| e.started())
        .filter_map(|e| Some((e, e.manifest.as_ref()?)))
        .flat_map(|(e, m)| m.commands.iter().map(|c| (e.id.clone(), m.name.clone(), c.clone())))
        .collect()
}

/// Bind the keys of the started extensions' commands; a keybinding den
/// can't read is logged and left out.
fn bind_commands(entries: &[Entry], cx: &mut App) {
    let mut bindings = Vec::new();
    for entry in entries.iter().filter(|e| e.started()) {
        let Some(manifest) = &entry.manifest else { continue };
        for command in manifest.commands.iter().filter(|c| !c.keybinding.trim().is_empty()) {
            if let Err(err) = command.keybinding.split_whitespace().try_for_each(|key| Keystroke::parse(key).map(drop)) {
                backend::log(&entry.id, &format!("the keybinding \"{}\" of {} is not one den reads: {err}", command.keybinding, command.id));
                continue;
            }
            bindings.push(KeyBinding::new(&command.keybinding, RunCommand { extension: entry.id.clone(), command: command.id.clone() }, None));
        }
    }
    cx.bind_keys(bindings);
}

/// Run extension `id`'s command `command` for the window on `root`.
pub fn run_command(id: &str, command: &str, root: &Path, cx: &App) {
    send(id, den_extension::events::COMMAND, serde_json::json!({ "root": root, "id": command }), cx);
}

/// `ctrl-alt-h` as menus show keys: `Ctrl+Alt+H`.
pub fn pretty_keys(keys: &str) -> String {
    let key = |key: &str| key.split('-').map(|part| {
        let mut chars = part.chars();
        chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
    }).collect::<Vec<_>>().join("+");
    keys.split_whitespace().map(key).collect::<Vec<_>>().join(" ")
}

/// A click on extension `id`'s button `button` in the window on `root`.
pub fn button_clicked(id: &str, root: &Path, button: &str, cx: &App) {
    send(id, den_extension::events::BUTTON_CLICKED, serde_json::json!({ "root": root, "id": button }), cx);
}

/// Something done in extension `id`'s view (`den_extension::events::VIEW_ACTION`).
pub fn view_event(id: &str, name: &str, data: Value, cx: &App) {
    send(id, name, data, cx);
}

fn send(id: &str, name: &str, data: Value, cx: &App) {
    let Some(extensions) = cx.try_global::<Extensions>() else { return };
    if let Some((_, tx)) = extensions.running.iter().find(|(running, _)| running == id) {
        _ = tx.send(Message::Event { name: name.to_string(), data });
    }
}

/// Send an event (`den_extension::events`) to every running extension.
pub fn broadcast(name: &str, data: Value, cx: &App) {
    let Some(extensions) = cx.try_global::<Extensions>() else { return };
    for (_, tx) in &extensions.running {
        _ = tx.send(Message::Event { name: name.to_string(), data: data.clone() });
    }
}

/// Load `id` from the next start on, or not.
pub fn set_enabled(id: &str, enabled: bool, cx: &mut App) {
    Settings::update(cx, |s| {
        s.disabled_extensions.retain(|d| d != id);
        if !enabled {
            s.disabled_extensions.push(id.to_string());
        }
    });
    if let Some(entry) = cx.global_mut::<Extensions>().entry_mut(id) {
        // A fresh install says so either way; it loads or not by this switch.
        if entry.status != Status::Installed {
            entry.change = match (entry.started(), enabled) {
                (false, true) => Some("Starts after a restart.".into()),
                (true, false) => Some("Stops after a restart.".into()),
                _ => None,
            };
        }
    }
}

/// What the user set of `id`'s settings.
fn user_settings(id: &str, cx: &App) -> serde_json::Map<String, Value> {
    Settings::get(cx).extension_settings.get(id).cloned().unwrap_or_default()
}

/// Every setting's value for `id`, defaults included.
pub fn settings_values(id: &str, cx: &App) -> serde_json::Map<String, Value> {
    let Some(manifest) = Extensions::get(cx).entries.iter().find(|e| e.id == id).and_then(|e| e.manifest.as_ref()) else { return Default::default() };
    manifest.settings_values(&user_settings(id, cx))
}

/// Set `id`'s setting `key` (`None` goes back to the default), and tell the
/// extension when it runs.
pub fn set_setting(id: &str, key: &str, value: Option<Value>, cx: &mut App) {
    Settings::update(cx, |s| {
        let values = s.extension_settings.entry(id.to_string()).or_default();
        match value {
            Some(value) => _ = values.insert(key.to_string(), value),
            None => _ = values.remove(key),
        }
        if values.is_empty() {
            s.extension_settings.remove(id);
        }
    });
    let settings = settings_values(id, cx);
    send(id, den_extension::events::SETTINGS_CHANGED, serde_json::json!({ "settings": settings }), cx);
}

pub fn is_enabled(id: &str, cx: &App) -> bool {
    !Settings::get(cx).disabled_extensions.iter().any(|d| d == id)
}

pub fn uninstall(id: &str, cx: &mut App) -> Result<(), String> {
    backend::uninstall(id)?;
    Settings::update(cx, |s| s.disabled_extensions.retain(|d| d != id));
    if let Some(entry) = cx.global_mut::<Extensions>().entry_mut(id) {
        entry.change = Some(REMOVED.into());
    }
    Ok(())
}

/// After `manifest` was downloaded for the next start: show it as such.
pub fn installed(manifest: Manifest, cx: &mut App) {
    let extensions = cx.global_mut::<Extensions>();
    let change = format!("Version {} installs after a restart.", manifest.version);
    match extensions.entry_mut(&manifest.id) {
        Some(entry) => entry.change = Some(change),
        None => extensions.entries.push(Entry {
            id: manifest.id.clone(),
            dir: backend::dir().join(format!("{}.pending", manifest.id)),
            manifest: Some(manifest),
            status: Status::Installed,
            change: Some(change),
        }),
    }
    cx.refresh_windows();
}

/// Whether a button's `file_pattern` matches `path`; one that doesn't compile
/// never does. Compiled once each, as the title bar asks on every frame.
pub fn pattern_matches(pattern: &str, path: &str) -> bool {
    thread_local! {
        static COMPILED: std::cell::RefCell<std::collections::HashMap<String, Option<regex::Regex>>> = Default::default();
    }
    COMPILED.with_borrow_mut(|compiled| {
        compiled.entry(pattern.to_string()).or_insert_with(|| regex::Regex::new(pattern).ok()).as_ref().is_some_and(|re| re.is_match(path))
    })
}

#[cfg(test)]
mod tests {
    use super::{Entry, Listing, Status, pattern_matches, pretty_keys, uninstalled, update_available};

    #[test]
    fn shows_keys_as_menus_do() {
        assert_eq!(pretty_keys("ctrl-alt-h"), "Ctrl+Alt+H");
        assert_eq!(pretty_keys("ctrl-k ctrl-s"), "Ctrl+K Ctrl+S");
        assert_eq!(pretty_keys("f5"), "F5");
    }

    fn listing(id: &str, version: &str, api: u32) -> Listing {
        Listing {
            repo: format!("me/{id}"),
            id: id.into(),
            name: id.into(),
            version: version.into(),
            api,
            description: String::new(),
            icon_url: String::new(),
            readme_url: String::new(),
        }
    }

    fn entry(id: &str, version: &str) -> Entry {
        let manifest = den_extension::Manifest::parse(&format!(r#"{{"id":"{id}","name":"{id}","version":"{version}","api":1}}"#)).unwrap();
        Entry { id: id.into(), dir: Default::default(), manifest: Some(manifest), status: Status::Loaded, change: None }
    }

    #[test]
    fn the_index_offers_what_is_not_installed_and_newer_versions() {
        let available = [listing("a", "1.2.0", 1), listing("b", "1.0.0", 1), listing("c", "2.0.0", 99)];
        let entries = [entry("a", "1.1.0"), entry("c", "1.0.0")];
        assert_eq!(uninstalled(&available, &entries).iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["b"]);
        assert_eq!(update_available(&entries[0], &available).map(|l| l.version.as_str()), Some("1.2.0"));
        // Not for a den too old to load it.
        assert!(update_available(&entries[1], &available).is_none());
        assert!(update_available(&entry("a", "1.2.0"), &available).is_none());
    }

    #[test]
    fn file_patterns_match_the_path_and_bad_ones_never() {
        assert!(pattern_matches(r"test_.*\.rs$", r"C:\p\src\test_io.rs"));
        assert!(!pattern_matches(r"test_.*\.rs$", r"C:\p\src\io.rs"));
        assert!(!pattern_matches("(unclosed", "(unclosed"));
    }
}
