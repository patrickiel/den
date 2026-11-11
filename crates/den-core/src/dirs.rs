//! Where den keeps its own files.

use std::{path::PathBuf, sync::OnceLock};

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Keep den's files in `dir` instead: den-server has a folder of its own on
/// a server, apart from a den that may be installed there. To be called
/// before anything asks for the folder; later calls change nothing.
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

/// Where den keeps its files: `%APPDATA%\den` on Windows,
/// `~/Library/Application Support/den` on macOS, `~/.config/den` elsewhere.
/// A debug build keeps its own, `den-dev` beside it, so working on den
/// neither reads nor changes the installed app's settings and state.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = DATA_DIR.get() {
        return dir.clone();
    }
    app_dir(if cfg!(debug_assertions) {
        "den-dev"
    } else {
        "den"
    })
}

/// The installed app's folder, for downloads a debug build shares with it
/// (the AI runtime and models, the formatters) rather than fetching again.
pub fn download_dir() -> PathBuf {
    app_dir("den")
}

fn app_dir(name: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|home| home.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|home| home.join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join(name)
}
