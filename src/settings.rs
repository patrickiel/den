//! Settings and the app's own state, both under `%APPDATA%\den\`.
//!
//! `settings.json` holds what the Settings tab edits; `state.json` holds what
//! the app remembers by itself: layout presets, the sidebar, and per session
//! (folder) its layout and expanded Explorer folders.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
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
    /// Ctrl+S formats the file first (Format Document's formatter).
    pub format_on_save: bool,
    pub smart_commit: SmartCommit,
    pub group_buttons: GroupButtons,
    /// den's tab-strip buttons were taken over once.
    pub imported_den_buttons: bool,
    /// den's notification sounds were taken over once.
    pub imported_den_sounds: bool,
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
    /// den's presets and font were taken over once, on first start.
    pub imported_den: bool,
    /// The page a new browser tab opens.
    pub browser_home: String,
    /// den's browser presets were taken over once.
    pub imported_den_browsers: bool,
    /// The icons of den's presets were taken over once (they came later).
    pub imported_den_icons: bool,
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
            smart_commit: SmartCommit::Ask,
            group_buttons: GroupButtons::default(),
            imported_den_buttons: false,
            imported_den_sounds: false,
            ai_model: String::new(),
            ai_custom_models: Vec::new(),
            ai_context_size: 8192,
            ai_threads: 0,
            ai_gpu: true,
            ai_system_prompt: crate::backend::ai::DEFAULT_COMMIT_STYLE.to_string(),
            ai_derive_style: true,
            presets: vec![Preset {
                name: "Claude Code".into(),
                command: "claude".into(),
                agent: true,
                browser: false,
                pinned: true,
                color: None,
                icon: None,
            }],
            notifications: true,
            notify_toast: true,
            notify_tab: true,
            notify_sound: true,
            notify_sound_done: "chime".into(),
            notify_sound_input: "ping".into(),
            notify_volume: 60,
            notify_taskbar: true,
            imported_den: false,
            browser_home: "https://www.google.com".into(),
            imported_den_browsers: false,
            imported_den_icons: false,
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

    /// Change a setting, apply it and save the file. Observers of the global
    /// (open editors, the workspace) pick the change up.
    pub fn update(cx: &mut App, f: impl FnOnce(&mut Self)) {
        f(cx.global_mut::<Self>());
        apply_theme(cx);
        write_json("settings.json", Self::get(cx));
        cx.refresh_windows();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarView {
    #[default]
    Explorer,
    Search,
    Scm,
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
        write_json("state.json", Self::get(cx));
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
    let mut settings: Settings = read_json("settings.json").unwrap_or_default();
    if !settings.imported_den {
        import_den(&mut settings);
        settings.imported_den = true;
        write_json("settings.json", &settings);
    }
    if !settings.imported_den_sounds {
        import_den_sounds(&mut settings);
        settings.imported_den_sounds = true;
        write_json("settings.json", &settings);
    }
    if !settings.imported_den_buttons {
        import_den_buttons(&mut settings);
        settings.imported_den_buttons = true;
        write_json("settings.json", &settings);
    }
    if !settings.imported_den_icons {
        import_den_icons(&mut settings);
        settings.imported_den_icons = true;
        write_json("settings.json", &settings);
    }
    if !settings.imported_den_browsers {
        import_den_browsers(&mut settings);
        settings.imported_den_browsers = true;
        write_json("settings.json", &settings);
    }
    cx.set_global::<Settings>(settings);
    cx.set_global::<AppState>(read_json("state.json").unwrap_or_default());
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
        names.join("\\")
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

/// `canonicalize` on Windows returns `\\?\C:\…`, which is noise in titles and
/// tab tooltips.
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

pub(crate) fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("den")
}

fn read_json<T: for<'de> Deserialize<'de>>(name: &str) -> Option<T> {
    let text = std::fs::read_to_string(data_dir().join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_json<T: Serialize>(name: &str, value: &T) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(dir.join(name), json);
    }
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

/// den's notification sounds and volume.
fn import_den_sounds(settings: &mut Settings) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let path = PathBuf::from(appdata).join("den.workspace").join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(den) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    if let Some(done) = den["notifySoundDone"].as_str() {
        settings.notify_sound_done = done.to_string();
    }
    if let Some(input) = den["notifySoundInput"].as_str() {
        settings.notify_sound_input = input.to_string();
    }
    if let Some(volume) = den["notifyVolume"].as_u64() {
        settings.notify_volume = volume.min(100) as u32;
    }
}

/// den's choice of tab-strip buttons (its plain shell is a preset there).
fn import_den_buttons(settings: &mut Settings) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let path = PathBuf::from(appdata).join("den.workspace").join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(den) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let buttons = &den["groupButtons"];
    if let Some(browser) = buttons["browser"].as_bool() {
        settings.group_buttons.browser = browser;
    }
    if let Some(split) = buttons["split"].as_bool() {
        settings.group_buttons.split = split;
    }
    let shell = den["terminalPresets"].as_array().into_iter().flatten().find(|p| p["command"].as_str().is_some_and(|c| c.trim().is_empty()));
    if let Some(pinned) = shell.and_then(|p| p["pinned"].as_bool()) {
        settings.group_buttons.shell = pinned;
    }
}

/// Give presets taken over from den before icons came their den icon.
fn import_den_icons(settings: &mut Settings) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let path = PathBuf::from(appdata).join("den.workspace").join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(den) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    for key in ["terminalPresets", "agentPresets", "browserPresets"] {
        for preset in den[key].as_array().into_iter().flatten() {
            let Some(icon) = preset["icon"].as_str() else { continue };
            let name = preset["name"].as_str().unwrap_or_default();
            if let Some(own) = settings.presets.iter_mut().find(|p| p.name == name && p.icon.is_none()) {
                own.icon = Some(icon.to_string());
            }
        }
    }
}

/// Take den's browser presets and home page over, once.
fn import_den_browsers(settings: &mut Settings) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let path = PathBuf::from(appdata).join("den.workspace").join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(den) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    if let Some(home) = den["browserHome"].as_str().map(str::trim).filter(|h| !h.is_empty()) {
        settings.browser_home = home.to_string();
    }
    for preset in den["browserPresets"].as_array().into_iter().flatten() {
        let url = preset["url"].as_str().unwrap_or_default().trim().to_string();
        if url.is_empty() {
            continue;
        }
        settings.presets.push(Preset {
            name: preset["name"].as_str().unwrap_or(&url).to_string(),
            command: url,
            agent: false,
            browser: true,
            pinned: preset["pinned"].as_bool().unwrap_or(true),
            color: preset["color"].as_str().map(str::to_string),
            icon: preset["icon"].as_str().map(str::to_string),
        });
    }
}

/// Take the Tauri den's terminal and agent presets and its font over, once
/// (from `%APPDATA%\den.workspace`): den then starts with the buttons it had.
fn import_den(settings: &mut Settings) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let path = PathBuf::from(appdata).join("den.workspace").join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(den) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let mut presets = Vec::new();
    for (key, agent) in [("terminalPresets", false), ("agentPresets", true)] {
        for preset in den[key].as_array().into_iter().flatten() {
            let command = preset["command"].as_str().unwrap_or_default().trim().to_string();
            // The plain shell is the strip's own shell button.
            if command.is_empty() {
                continue;
            }
            presets.push(Preset {
                name: preset["name"].as_str().unwrap_or(&command).to_string(),
                command,
                agent,
                browser: false,
                pinned: preset["pinned"].as_bool().unwrap_or(true),
                color: preset["color"].as_str().map(str::to_string),
                icon: preset["icon"].as_str().map(str::to_string),
            });
        }
    }
    if !presets.is_empty() {
        settings.presets = presets;
    }
    // den's font list is CSS; the first family is the one it uses.
    if let Some(family) = den["fontFamily"].as_str().and_then(|list| list.split(',').next()) {
        let family = family.trim().trim_matches('"').trim_matches('\'');
        if !family.is_empty() && family != "monospace" {
            settings.font_family = family.to_string();
        }
    }
    if let Some(size) = den["fontSize"].as_f64() {
        settings.editor_font_size = size as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::unique_names;
    use std::path::PathBuf;

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
