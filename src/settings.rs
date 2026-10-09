//! Settings and the app's own state, both under `%APPDATA%\den\`.
//!
//! `settings.json` holds what the Settings tab edits; `state.json` holds what
//! the app remembers by itself: layout presets, the sidebar, and per session
//! (folder) its layout and expanded Explorer folders.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Dark,
    Light,
}

/// Which built-in buttons a tab strip shows (the presets have their pins).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupButtons {
    pub shell: bool,
    pub browser: bool,
    pub split: bool,
}

impl Default for GroupButtons {
    fn default() -> Self {
        Self { shell: true, browser: true, split: true }
    }
}

/// How a diff lays its sides out, as VS Code's Diff View menu: inline,
/// side by side, or side by side until the view is too narrow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffLayout {
    #[default]
    Automatic,
    Inline,
    SideBySide,
}

/// Commit with nothing staged: ask whether to stage everything and commit
/// it, always do, or never (commit nothing), as VS Code and den do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmartCommit {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarSide {
    Left,
    Right,
}

/// A program a tab-strip button runs in a new terminal: an agent (Claude Code,
/// Codex) or anything else (a dev server, a script).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub agent: bool,
    /// A browser preset: `command` is the URL it opens.
    #[serde(default)]
    pub browser: bool,
    /// Pinned presets get their own button on every tab strip.
    #[serde(default = "yes")]
    pub pinned: bool,
    /// The badge colour (`#f76b15`); none for the strip's text colour.
    #[serde(default)]
    pub color: Option<String>,
    /// The picked icon, by den's names (`letter`, `logo:claude`,
    /// `fourBars`); none for the automatic one.
    #[serde(default)]
    pub icon: Option<String>,
}

fn yes() -> bool {
    true
}

impl Preset {
    /// The badge letter: the first letter of the name.
    pub fn letter(&self) -> String {
        self.name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// An imported VS Code theme by its id; empty for den's Dark or Light.
    pub color_theme: String,
    pub sidebar_position: SidebarSide,
    /// Editor and terminal font; empty for the theme's monospace font.
    pub font_family: String,
    pub editor_font_size: f32,
    pub line_numbers: bool,
    pub soft_wrap: bool,
    /// The shell for new terminals; empty for the automatic choice.
    pub shell: String,
    /// Lines a terminal keeps above the screen.
    pub scrollback: usize,
    pub tab_close_button: bool,
    /// Saving formats the file first (Format Document's formatter).
    pub format_on_save: bool,
    pub diff_layout: DiffLayout,
    pub smart_commit: SmartCommit,
    pub group_buttons: GroupButtons,
    /// Generate Commit Message: empty for the default model, a download URL
    /// (a preset's or a custom one) or a local .gguf file.
    pub ai_model: String,
    /// Custom models added in Settings (URLs or .gguf files), listed after
    /// the presets.
    pub ai_custom_models: Vec<String>,
    pub ai_context_size: u32,
    /// CPU threads; 0 for llama.cpp's choice.
    pub ai_threads: u32,
    pub ai_gpu: bool,
    /// The commit style for repositories without `.den/commit-style.md`.
    pub ai_system_prompt: String,
    /// Derive a repository's style from its history on first use.
    pub ai_derive_style: bool,
    pub presets: Vec<Preset>,
    /// A terminal wants you (an agent finished or waits for input, a bell)
    /// while you look elsewhere: these say how you hear of it.
    pub notifications: bool,
    pub notify_toast: bool,
    pub notify_tab: bool,
    pub notify_sound: bool,
    /// den's sounds by id ("chime", …; "none" for silence).
    pub notify_sound_done: String,
    pub notify_sound_input: String,
    /// 0..100.
    pub notify_volume: u32,
    pub notify_taskbar: bool,
    /// The page a new browser tab opens.
    pub browser_home: String,
    /// Installed extensions that den does not load, by id.
    pub disabled_extensions: Vec<String>,
    /// What the user set of each extension's settings, by id and key.
    pub extension_settings: std::collections::BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
    /// The keys of the commands the user changed, by command id
    /// (`den.newTerminal`), in place of the defaults; none for unbound.
    pub keybindings: std::collections::BTreeMap<String, Vec<String>>,
}

impl Preset {
    /// A terminal preset running `command`, pinned to the tab strips.
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self { name: name.into(), command: command.into(), agent: false, browser: false, pinned: true, color: None, icon: None }
    }

    /// Which kind of tab it opens, for the default groups.
    pub fn kind(&self) -> crate::defaults::Kind {
        use crate::defaults::Kind;
        if self.browser {
            Kind::Browsers
        } else if self.agent {
            Kind::Agents
        } else {
            Kind::Terminals
        }
    }
}

/// Claude Code and Codex; on Windows with WSL installed, a shell in its
/// default distribution too.
pub fn default_presets() -> Vec<Preset> {
    let mut presets = vec![
        Preset { agent: true, ..Preset::new("Claude Code", "claude") },
        Preset { agent: true, ..Preset::new("Codex", "codex") },
    ];
    if crate::backend::wsl::default_distro().is_some() {
        presets.push(Preset::new("WSL", "wsl"));
    }
    presets
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            color_theme: String::new(),
            sidebar_position: SidebarSide::Left,
            font_family: String::new(),
            editor_font_size: 14.,
            line_numbers: true,
            soft_wrap: false,
            shell: String::new(),
            scrollback: 10_000,
            tab_close_button: true,
            format_on_save: false,
            diff_layout: DiffLayout::Automatic,
            smart_commit: SmartCommit::Ask,
            group_buttons: GroupButtons::default(),
            ai_model: String::new(),
            ai_custom_models: Vec::new(),
            ai_context_size: 8192,
            ai_threads: 0,
            ai_gpu: true,
            ai_system_prompt: crate::backend::ai::DEFAULT_COMMIT_STYLE.to_string(),
            ai_derive_style: true,
            presets: default_presets(),
            notifications: true,
            notify_toast: true,
            notify_tab: true,
            notify_sound: true,
            notify_sound_done: "chime".into(),
            notify_sound_input: "ping".into(),
            notify_volume: 60,
            notify_taskbar: true,
            browser_home: "https://www.google.com".into(),
            disabled_extensions: Vec::new(),
            extension_settings: Default::default(),
            keybindings: Default::default(),
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// What Generate Commit Message runs with.
    pub fn ai_config(&self) -> crate::backend::ai::AiConfig {
        crate::backend::ai::AiConfig {
            model: self.ai_model.clone(),
            context: self.ai_context_size.clamp(1024, 131_072),
            threads: self.ai_threads,
            gpu: self.ai_gpu,
            style: self.ai_system_prompt.clone(),
            derive_style: self.ai_derive_style,
        }
    }
}

impl Settings {
    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Change a setting: the theme follows when that is what changed, the
    /// file is written a moment later, and observers of the global (the
    /// panes, the workspace) pick the change up.
    pub fn update(cx: &mut App, f: impl FnOnce(&mut Self)) {
        let (theme, color_theme) = (Self::get(cx).theme, Self::get(cx).color_theme.clone());
        f(cx.global_mut::<Self>());
        if Self::get(cx).theme != theme || Self::get(cx).color_theme != color_theme {
            apply_theme(cx);
        }
        write_later(File::Settings, cx);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarView {
    #[default]
    Explorer,
    Search,
    Scm,
    Extensions,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarState {
    pub visible: bool,
    pub view: SidebarView,
    pub width: f32,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            visible: true,
            view: SidebarView::Explorer,
            width: 260.,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// A `LayoutState`, kept as JSON so a layout an older build wrote cannot
    /// spoil the rest of the file.
    pub layout: Option<serde_json::Value>,
    pub expanded: Vec<PathBuf>,
    /// The commit message being written, kept with the session.
    pub commit_message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutPreset {
    pub name: String,
    pub layout: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub sidebar: SidebarState,
    pub presets: Vec<LayoutPreset>,
    /// Folders opened before, last first, for the session switcher.
    pub recent: Vec<PathBuf>,
    /// Keyed by a hash of the session folder.
    pub sessions: HashMap<String, Session>,
    /// The view last picked for Markdown and SVG files: preview, not source.
    pub preview_markdown: bool,
    pub preview_svg: bool,
}

impl Global for AppState {}

impl AppState {
    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub fn update<R>(cx: &mut App, f: impl FnOnce(&mut Self) -> R) -> R {
        let result = f(cx.global_mut::<Self>());
        write_later(File::State, cx);
        result
    }

    /// Put `root` first in the recent folders.
    pub fn opened(&mut self, root: &Path) {
        let k = session_key(root);
        self.recent.retain(|p| session_key(p) != k);
        self.recent.insert(0, root.to_path_buf());
        self.recent.truncate(20);
    }

    pub fn session(&self, root: &Path) -> Session {
        self.sessions.get(&session_key(root)).cloned().unwrap_or_default()
    }

    pub fn session_mut(&mut self, root: &Path) -> &mut Session {
        self.sessions.entry(session_key(root)).or_default()
    }
}

pub fn init(cx: &mut App) {
    let settings = read_settings(&data_dir().join("settings.json"));
    cx.set_global::<Settings>(settings);
    cx.set_global::<AppState>(read_json("state.json").unwrap_or_default());
    cx.set_global(Pending::default());
    // A write still waiting would be cut off with the app.
    cx.on_app_quit(|cx| {
        flush(cx);
        async {}
    })
    .detach();
    apply_theme(cx);
}

fn apply_theme(cx: &mut App) {
    crate::theme::apply(cx);
}

fn session_key(root: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    root.to_string_lossy().to_lowercase().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Short names for `paths`: each its folder name, with as many parent folders
/// in front as it takes to tell it from the others (`a\debug`, `b\debug`).
pub fn unique_names(paths: &[PathBuf]) -> Vec<String> {
    let parts: Vec<Vec<String>> = paths
        .iter()
        .map(|path| path.components().rev().map(|c| c.as_os_str().to_string_lossy().trim_end_matches(['\\', '/']).to_string()).filter(|s| !s.is_empty()).collect())
        .collect();
    let tail = |ix: usize, depth: usize| -> String {
        let mut names: Vec<&str> = parts[ix].iter().take(depth).map(String::as_str).collect();
        names.reverse();
        names.join(std::path::MAIN_SEPARATOR_STR)
    };
    let mut depth = vec![1; paths.len()];
    // Lengthen every name that clashes with another until none do (or a path
    // has no parents left).
    loop {
        let names: Vec<String> = (0..paths.len()).map(|ix| tail(ix, depth[ix]).to_lowercase()).collect();
        let mut grew = false;
        for ix in 0..paths.len() {
            let clash = names.iter().enumerate().any(|(other, name)| other != ix && *name == names[ix]);
            if clash && depth[ix] < parts[ix].len() {
                depth[ix] += 1;
                grew = true;
            }
        }
        if !grew {
            return (0..paths.len()).map(|ix| tail(ix, depth[ix])).collect();
        }
    }
}

/// `canonicalize` on Windows returns `\\?\C:\…` and `\\?\UNC\server\…`,
/// which are noise in titles and tab tooltips. A folder inside WSL comes out
/// as `\\wsl.localhost\…`, the form den builds from git's paths there.
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let plain = match path.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest.strip_prefix(r"UNC\").map_or_else(|| rest.to_string(), |unc| format!(r"\\{unc}"))),
        None => path.clone(),
    };
    match crate::backend::wsl::split(&plain) {
        Some((distro, linux)) => crate::backend::wsl::to_windows(Some(&distro), &linux).unwrap_or(plain),
        None => plain,
    }
}

/// Where den keeps its files: `%APPDATA%\den` on Windows,
/// `~/Library/Application Support/den` on macOS, `~/.config/den` elsewhere.
/// A debug build keeps its own, `den-dev` beside it, so working on den
/// neither reads nor changes the installed app's settings and state.
pub(crate) fn data_dir() -> PathBuf {
    app_dir(if cfg!(debug_assertions) { "den-dev" } else { "den" })
}

/// The installed app's folder, for downloads a debug build shares with it
/// (the AI runtime and models, the formatters) rather than fetching again.
pub(crate) fn download_dir() -> PathBuf {
    app_dir("den")
}

fn app_dir(name: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|home| home.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home().map(|home| home.join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join(name)
}

/// The most recent folder that still exists, read before the app starts.
pub fn last_folder() -> Option<PathBuf> {
    read_json::<AppState>("state.json")?.recent.into_iter().find(|p| p.is_dir())
}

/// The settings file: the defaults when there is none. One that cannot be
/// read is kept beside as `settings.json.bad`, and den starts with the
/// defaults, so the file stays as it is until a setting changes.
fn read_settings(path: &Path) -> Settings {
    let Ok(text) = std::fs::read_to_string(path) else { return Settings::default() };
    match serde_json::from_str(&text) {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("den: {} cannot be read ({err}); starting with the defaults", path.display());
            let _ = std::fs::copy(path, path.with_extension("json.bad"));
            Settings::default()
        }
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(name: &str) -> Option<T> {
    let text = std::fs::read_to_string(data_dir().join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

/// den's two files under [`data_dir`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum File {
    Settings,
    State,
}

impl File {
    fn name(self) -> &'static str {
        match self {
            File::Settings => "settings.json",
            File::State => "state.json",
        }
    }

    /// The file's content, from the global it holds.
    fn text(self, cx: &App) -> String {
        match self {
            File::Settings => pretty(Settings::get(cx)),
            File::State => pretty(AppState::get(cx)),
        }
    }
}

fn pretty<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// The writes waiting for a quiet moment, one per file.
#[derive(Default)]
struct Pending {
    settings: Option<Task<()>>,
    state: Option<Task<()>>,
}

impl Global for Pending {}

/// Write `file` half a second after the last change to it (each change
/// starts the wait over), off the UI thread.
fn write_later(file: File, cx: &mut App) {
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(500)).await;
        let text = cx.update(|cx| file.text(cx));
        cx.background_spawn(async move { write_file(file, &text) }).await;
    });
    let pending = cx.default_global::<Pending>();
    match file {
        File::Settings => pending.settings = Some(task),
        File::State => pending.state = Some(task),
    }
}

/// Write what waits, now (on quit, where a wait would be cut off).
pub fn flush(cx: &mut App) {
    *cx.default_global::<Pending>() = Pending::default();
    for file in [File::Settings, File::State] {
        write_file(file, &file.text(cx));
    }
}

/// Write `text` to `file` unless that is what it holds already. The first
/// failure is reported; den runs on without the file.
fn write_file(file: File, text: &str) {
    static WRITTEN: Mutex<[u64; 2]> = Mutex::new([0; 2]);
    static WARNED: AtomicBool = AtomicBool::new(false);
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();
    let Ok(mut written) = WRITTEN.lock() else { return };
    if written[file as usize] == hash {
        return;
    }
    match write_text(&data_dir().join(file.name()), text) {
        Ok(()) => written[file as usize] = hash,
        Err(err) => {
            if !WARNED.swap(true, Ordering::Relaxed) {
                eprintln!("den: could not write {}: {err}", file.name());
            }
        }
    }
}

/// Write `text` through a temporary file beside `path`, so a crash never
/// leaves it half written.
fn write_text(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The editor and terminal font: the one set in Settings, else the theme's.
pub fn mono_font(cx: &App) -> SharedString {
    let family = &Settings::get(cx).font_family;
    if family.trim().is_empty() {
        gpui_kit::component::ActiveTheme::theme(cx).mono_font_family.clone()
    } else {
        family.trim().to_string().into()
    }
}

#[cfg(test)]
mod tests {
    use super::{read_settings, session_key, strip_verbatim, unique_names, write_text};
    use std::path::{Path, PathBuf};

    #[test]
    fn plain_paths_from_canonicalize() {
        let plain = |p: &str| strip_verbatim(PathBuf::from(p));
        assert_eq!(plain(r"\\?\C:\Users\me"), PathBuf::from(r"C:\Users\me"));
        assert_eq!(plain(r"\\?\UNC\server\share\x"), PathBuf::from(r"\\server\share\x"));
        assert_eq!(plain(r"\\?\UNC\wsl.localhost\Ubuntu\home\me"), PathBuf::from(r"\\wsl.localhost\Ubuntu\home\me"));
        assert_eq!(plain(r"\\wsl$\Ubuntu\home\me"), PathBuf::from(r"\\wsl.localhost\Ubuntu\home\me"));
        assert_eq!(plain("/Users/me"), PathBuf::from("/Users/me"));
    }

    #[test]
    fn a_broken_settings_file_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("den-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        assert_eq!(read_settings(&path).editor_font_size, 14.);
        std::fs::write(&path, "{ \"editor_font_size\": 20 }").unwrap();
        assert_eq!(read_settings(&path).editor_font_size, 20.);
        std::fs::write(&path, "{ not json").unwrap();
        let settings = read_settings(&path);
        assert_eq!(settings.editor_font_size, 14.);
        assert_eq!(std::fs::read_to_string(dir.join("settings.json.bad")).unwrap(), "{ not json");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn session_keys_are_stable() {
        // The keys are in state.json: a change would orphan every saved session.
        assert_eq!(session_key(Path::new(r"C:\Users\me\project")), "d6959baf11742046");
        assert_eq!(session_key(Path::new(r"c:\users\ME\project")), session_key(Path::new(r"C:\Users\me\project")));
    }

    #[test]
    fn writes_through_a_temporary_file() {
        let dir = std::env::temp_dir().join(format!("den-write-test-{}", std::process::id()));
        let path = dir.join("state.json");
        write_text(&path, "one").unwrap();
        write_text(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn names(paths: &[&str]) -> Vec<String> {
        unique_names(&paths.iter().map(PathBuf::from).collect::<Vec<_>>())
    }

    #[test]
    fn just_the_name_when_it_is_unique() {
        assert_eq!(names(&[r"C:\repos\den", r"C:\repos\den2"]), vec!["den", "den2"]);
    }

    #[test]
    fn parents_until_unique() {
        assert_eq!(
            names(&[r"C:\a\x\debug", r"C:\b\x\debug", r"C:\c\target"]),
            vec![r"a\x\debug", r"b\x\debug", "target"]
        );
        assert_eq!(names(&[r"C:\a\debug", r"C:\b\Debug"]), vec![r"a\debug", r"b\Debug"]);
    }
}
