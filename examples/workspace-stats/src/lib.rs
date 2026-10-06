//! A fuller den extension than hello-extension: it remembers the folders den
//! opens, scans each one for the languages in it, and reminds you to take a
//! break. Beyond the basics it shows what a real extension needs:
//!
//! - settings: declared in `extension.json`, set on its page in den, read
//!   from `Context::settings` into a struct and applied live on
//!   `events::SETTINGS_CHANGED`;
//! - state that survives restarts and updates: `history.json` in
//!   `Context::data_dir`;
//! - slow work off the event thread: den hands an extension its events one at
//!   a time, so blocking in `event` holds up the ones after it;
//! - a command (Show Workspace Stats, in den's menu) and `events::FILE_SAVED`,
//!   counted per folder;
//! - `Host` used from another thread, and `call` for a method with no helper;
//! - a quick `deactivate`: den waits only about a second when it quits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use den_extension::{Context, Extension, Host, events, register};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const HISTORY: &str = "history.json";

/// The settings `extension.json` declares, by the same keys. den always sends
/// every one, so the defaults here only matter for a den that sends none.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
struct Config {
    /// Scan each folder den opens.
    scan: bool,
    /// Stop counting after this many files.
    max_files: usize,
    /// Folder names never descended into, separated by commas.
    ignore: String,
    /// A break reminder every this many minutes of den running; 0 turns it off.
    break_minutes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config { scan: true, max_files: 50_000, ignore: ".git, target, node_modules, dist, build, .venv".into(), break_minutes: 90 }
    }
}

impl Config {
    /// From den's values; ones that don't fit (a fraction for `max_files`)
    /// leave all at their defaults, with the reason.
    fn from_settings(settings: Map<String, Value>) -> Result<Self, String> {
        serde_json::from_value(Value::Object(settings)).map_err(|e| e.to_string())
    }

    fn ignored(&self, name: &std::ffi::OsStr) -> bool {
        self.ignore.split(',').map(str::trim).any(|ignored| !ignored.is_empty() && *ignored == *name)
    }
}

enum Job {
    Opened(PathBuf),
    Settings(Config),
    /// A file was saved in the window on this folder.
    Saved(PathBuf),
    /// The Show Workspace Stats command, in the window on this folder.
    Show(PathBuf),
}

/// `history.json`, by folder.
type History = BTreeMap<String, Visit>;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Visit {
    opens: u32,
    /// Unix seconds.
    last_opened: u64,
    /// Files saved in it; missing from a history older than the count.
    #[serde(default)]
    saves: u32,
}

/// What a scan found.
#[derive(Debug, Default, PartialEq)]
struct Scan {
    files: usize,
    /// Bytes per language, for the files whose extension names one.
    languages: BTreeMap<&'static str, u64>,
    /// It stopped early: `max_files`, or den quitting.
    partial: bool,
}

struct Stats {
    host: Host,
    worker: Option<Worker>,
}

fn config(host: &Host, settings: Map<String, Value>) -> Config {
    Config::from_settings(settings).unwrap_or_else(|err| {
        host.toast(format!("Workspace Stats: a setting doesn't fit ({err}); using the defaults"));
        Config::default()
    })
}

impl Extension for Stats {
    fn activate(host: Host, context: Context) -> Self {
        // `info` has no helper on `Host`; `call` reaches any host method by name.
        if let Ok(info) = host.call("info", Value::Null) {
            host.log(format!("activated: {info}"));
        }
        let config = config(&host, context.settings);
        Stats { host, worker: Worker::start(host, config, context.data_dir) }
    }

    fn event(&mut self, name: &str, mut data: Value) {
        let job = match name {
            events::WORKSPACE_OPENED => data["root"].as_str().map(|root| Job::Opened(PathBuf::from(root))),
            events::FILE_SAVED => data["root"].as_str().map(|root| Job::Saved(PathBuf::from(root))),
            events::COMMAND if data["id"] == "show-stats" => data["root"].as_str().map(|root| Job::Show(PathBuf::from(root))),
            events::SETTINGS_CHANGED => match data["settings"].take() {
                Value::Object(settings) => Some(Job::Settings(config(&self.host, settings))),
                _ => None,
            },
            _ => None,
        };
        if let (Some(worker), Some(job)) = (&self.worker, job) {
            _ = worker.jobs.send(job);
        }
    }

    fn deactivate(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.stop();
        }
    }
}

register!(Stats);

// -- The worker ----------------------------------------------------------------

/// The thread that scans, keeps the history and times the breaks.
struct Worker {
    /// Folders den opened and new settings. Dropping it ends the thread.
    jobs: Sender<Job>,
    /// Set on quit, so a scan halfway through a big folder gives up.
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn start(host: Host, config: Config, dir: PathBuf) -> Option<Self> {
        let (jobs, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let spawned = std::thread::Builder::new().name("workspace-stats".into()).spawn({
            let stop = stop.clone();
            move || run(host, config, dir, rx, stop)
        });
        match spawned {
            Ok(thread) => Some(Worker { jobs, stop, thread }),
            Err(err) => {
                host.log(format!("cannot start the worker: {err}"));
                None
            }
        }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.jobs);
        _ = self.thread.join();
    }
}

fn run(host: Host, mut config: Config, dir: PathBuf, jobs: Receiver<Job>, stop: Arc<AtomicBool>) {
    let history_path = dir.join(HISTORY);
    let mut history: History = read_json(&history_path).unwrap_or_default();
    // The last scan of each folder, for Show Workspace Stats.
    let mut scans: BTreeMap<String, Scan> = BTreeMap::new();
    let started = Instant::now();
    let every = |config: &Config| Duration::from_secs(config.break_minutes * 60);
    let mut next_break = (config.break_minutes > 0).then(|| started + every(&config));
    loop {
        // Waiting for the next folder is also waiting for the next break.
        let job = match next_break {
            Some(at) => jobs.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => jobs.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match job {
            Ok(Job::Settings(new)) => {
                // A new interval counts from now.
                next_break = (new.break_minutes > 0).then(|| Instant::now() + every(&new));
                host.log(format!("settings: {new:?}"));
                config = new;
            }
            Ok(Job::Saved(root)) => {
                history.entry(key(&root)).or_default().saves += 1;
                if let Err(err) = write_json(&history_path, &history) {
                    host.log(format!("cannot save {HISTORY}: {err}"));
                }
            }
            Ok(Job::Show(root)) => {
                let visit = history.get(&key(&root)).cloned().unwrap_or_default();
                host.toast(stats(&root, &visit, scans.get(&key(&root)), started.elapsed()));
            }
            Ok(Job::Opened(root)) => {
                let previous = record(&mut history, &root, unix_now());
                if let Err(err) = write_json(&history_path, &history) {
                    host.log(format!("cannot save {HISTORY}: {err}"));
                }
                let scan = config.scan.then(|| {
                    let t = Instant::now();
                    let scan = scan(&root, &config, &stop);
                    host.log(format!("scanned {} in {} ms: {scan:?}", root.display(), t.elapsed().as_millis()));
                    scan
                });
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                host.toast(welcome(&root, history[&key(&root)].opens, previous, scan.as_ref(), unix_now()));
                if let Some(scan) = scan {
                    scans.insert(key(&root), scan);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                host.toast(format!("den has been open for {}. Time for a break?", duration(started.elapsed())));
                next_break = next_break.map(|at| at + every(&config));
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

/// Count an opening of `root` at `now`; returns when it was opened before.
fn record(history: &mut History, root: &Path, now: u64) -> Option<u64> {
    let visit = history.entry(key(root)).or_default();
    let previous = (visit.opens > 0).then_some(visit.last_opened);
    visit.opens += 1;
    visit.last_opened = now;
    previous
}

/// The toast for an opened folder, e.g. `den: 412 files (Rust 81%, Markdown
/// 12%) · opened 7 times, last 2 days ago`.
fn welcome(root: &Path, opens: u32, previous: Option<u64>, scan: Option<&Scan>, now: u64) -> String {
    let name = root.file_name().map_or_else(|| key(root), |n| n.to_string_lossy().into_owned());
    let mut parts = Vec::new();
    if let Some(scan) = scan {
        parts.push(summary(scan));
    }
    parts.push(match previous {
        Some(at) => format!("opened {opens} times, last {}", ago(now.saturating_sub(at))),
        None => "first time here".into(),
    });
    format!("{name}: {}", parts.join(" · "))
}

/// The toast for Show Workspace Stats, e.g. `den: 412 files (Rust 81%) ·
/// opened 7 times · 23 files saved · den open for 1 h 5 min`.
fn stats(root: &Path, visit: &Visit, scan: Option<&Scan>, open_for: Duration) -> String {
    let name = root.file_name().map_or_else(|| key(root), |n| n.to_string_lossy().into_owned());
    let mut parts: Vec<String> = scan.map(summary).into_iter().collect();
    parts.push(format!("opened {} {}", visit.opens, plural(visit.opens.into(), "time")));
    parts.push(format!("{} {} saved", visit.saves, plural(visit.saves.into(), "file")));
    parts.push(format!("den open for {}", duration(open_for)));
    format!("{name}: {}", parts.join(" · "))
}

// -- Scanning ------------------------------------------------------------------

/// Walk `root` without following links (a link is neither a file nor a
/// folder to `file_type`, so loops can't happen), skipping `config.ignore`.
fn scan(root: &Path, config: &Config, stop: &AtomicBool) -> Scan {
    let mut scan = Scan::default();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if scan.files >= config.max_files || stop.load(Ordering::Relaxed) {
                scan.partial = true;
                return scan;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if !config.ignored(&entry.file_name()) {
                    dirs.push(entry.path());
                }
            } else if kind.is_file() {
                scan.files += 1;
                let path = entry.path();
                if let Some(language) = path.extension().and_then(|e| e.to_str()).and_then(language) {
                    *scan.languages.entry(language).or_default() += entry.metadata().map_or(0, |m| m.len());
                }
            }
        }
    }
    scan
}

fn language(extension: &str) -> Option<&'static str> {
    Some(match extension.to_ascii_lowercase().as_str() {
        "rs" => "Rust",
        "md" => "Markdown",
        "ts" | "tsx" | "mts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "svelte" => "Svelte",
        "py" => "Python",
        "go" => "Go",
        "c" | "h" => "C",
        "cpp" | "cc" | "hpp" => "C++",
        "cs" => "C#",
        "java" => "Java",
        "kt" => "Kotlin",
        "swift" => "Swift",
        "html" => "HTML",
        "css" | "scss" => "CSS",
        "ps1" => "PowerShell",
        "sh" => "Shell",
        "toml" => "TOML",
        "json" => "JSON",
        "yml" | "yaml" => "YAML",
        _ => return None,
    })
}

/// `412 files (Rust 81%, Markdown 12%, TOML 4%)`, the top three by size.
fn summary(scan: &Scan) -> String {
    let files = format!("{}{} {}", scan.files, if scan.partial { "+" } else { "" }, plural(scan.files as u64, "file"));
    let total: u64 = scan.languages.values().sum();
    if total == 0 {
        return files;
    }
    let mut top: Vec<_> = scan.languages.iter().filter(|(_, bytes)| **bytes > 0).collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let top: Vec<_> = top.iter().take(3).map(|(language, bytes)| format!("{language} {}%", (*bytes * 100 + total / 2) / total)).collect();
    format!("{files} ({})", top.join(", "))
}

// -- Small things --------------------------------------------------------------

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Through a temporary file, so quitting halfway never leaves half a file.
fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(value)?)?;
    std::fs::rename(tmp, path)
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 { word.to_string() } else { format!("{word}s") }
}

fn ago(secs: u64) -> String {
    let (n, unit) = match secs {
        0..60 => return "just now".into(),
        60..3600 => (secs / 60, "minute"),
        3600..86_400 => (secs / 3600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    format!("{n} {} ago", plural(n, unit))
}

fn duration(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty folder under the system's temp folder.
    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("workspace-stats-{name}-{}", std::process::id()));
        _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file(path: PathBuf, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x".repeat(bytes)).unwrap();
    }

    fn settings(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn reads_its_settings_as_den_sends_them() {
        let manifest = den_extension::Manifest::parse(include_str!("../extension.json")).unwrap();
        let defaults = manifest.settings_values(&Map::new());
        assert_eq!(Config::from_settings(defaults).unwrap(), Config::default());
        let config = Config::from_settings(settings(serde_json::json!({ "break_minutes": 0, "ignore": "out , .cache" }))).unwrap();
        assert_eq!((config.break_minutes, config.max_files), (0, 50_000));
        assert!(config.ignored("out".as_ref()) && config.ignored(".cache".as_ref()) && !config.ignored("src".as_ref()));
        assert!(Config::from_settings(settings(serde_json::json!({ "max_files": 1.5 }))).is_err());
    }

    #[test]
    fn scans_languages_by_size_and_skips_ignored_folders() {
        let dir = temp("scan");
        file(dir.join("src/main.rs"), 300);
        file(dir.join("src/lib.RS"), 500);
        file(dir.join("README.md"), 200);
        file(dir.join("LICENSE"), 1000);
        file(dir.join("target/debug/huge.rs"), 10_000);
        let scan = scan(&dir, &Config::default(), &AtomicBool::new(false));
        assert_eq!(scan.files, 4);
        assert_eq!(scan.languages, BTreeMap::from([("Rust", 800), ("Markdown", 200)]));
        assert!(!scan.partial);
        assert_eq!(summary(&scan), "4 files (Rust 80%, Markdown 20%)");
    }

    #[test]
    fn a_scan_stops_at_max_files_and_on_quit() {
        let dir = temp("partial");
        for i in 0..5 {
            file(dir.join(format!("{i}.py")), 10);
        }
        let config = Config { max_files: 3, ..Config::default() };
        let scan = super::scan(&dir, &config, &AtomicBool::new(false));
        assert_eq!((scan.files, scan.partial), (3, true));
        assert!(summary(&scan).starts_with("3+ files"));
        let scan = super::scan(&dir, &Config::default(), &AtomicBool::new(true));
        assert_eq!((scan.files, scan.partial), (0, true));
    }

    #[test]
    fn keeps_the_history_across_restarts() {
        let dir = temp("history");
        let path = dir.join(HISTORY);
        let root = Path::new(r"C:\code\den");
        let mut history = History::new();
        assert_eq!(record(&mut history, root, 1000), None);
        write_json(&path, &history).unwrap();
        let mut history: History = read_json(&path).unwrap();
        assert_eq!(record(&mut history, root, 2000), Some(1000));
        assert_eq!(history[&key(root)], Visit { opens: 2, last_opened: 2000, saves: 0 });
        // A history from before saves were counted still reads.
        let old: History = serde_json::from_str(r#"{ "C:\\code\\den": { "opens": 3, "last_opened": 5 } }"#).unwrap();
        assert_eq!(old.values().next().unwrap().saves, 0);
    }

    #[test]
    fn words_the_toasts() {
        let root = Path::new(r"C:\code\den");
        assert_eq!(welcome(root, 1, None, None, 0), "den: first time here");
        let scan = Scan { files: 1, languages: BTreeMap::from([("Go", 10)]), partial: false };
        assert_eq!(welcome(root, 3, Some(0), Some(&scan), 2 * 86_400 + 5), "den: 1 file (Go 100%) · opened 3 times, last 2 days ago");
        let visit = Visit { opens: 1, last_opened: 0, saves: 23 };
        assert_eq!(stats(root, &visit, Some(&scan), Duration::from_secs(65 * 60)), "den: 1 file (Go 100%) · opened 1 time · 23 files saved · den open for 1 h 5 min");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(7200), "2 hours ago");
        assert_eq!(duration(Duration::from_secs(50 * 60)), "50 min");
        assert_eq!(duration(Duration::from_secs(120 * 60)), "2 h");
        assert_eq!(duration(Duration::from_secs(95 * 60)), "1 h 35 min");
    }
}
