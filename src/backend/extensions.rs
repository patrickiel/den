//! Extensions off the UI thread: their folders, loading a DLL and running it
//! on a thread of its own, and installing one from a GitHub release.
//!
//! An extension is a folder `%APPDATA%\den\extensions\<id>` with its
//! `extension.json` and DLL (see the `den-extension` crate). Nothing is
//! replaced or removed while den runs, because Windows locks a loaded DLL and
//! den never unloads one: an install or update unpacks into `<id>.pending`,
//! an uninstall leaves an `<id>.remove` file, and [`apply_pending`] carries
//! both out at the next start, before anything loads.
//!
//! The extensions den lists as Available come from a curated index, one
//! generated `index.json` in the `patrickiel/den-extensions` repository,
//! cached in `%APPDATA%\den\extensions-index.json`.

use std::ffi::{CStr, CString, c_char, c_void};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};

use den_extension::{API_VERSION, Button, Context, MANIFEST, Manifest, abi, valid_id, view};
use futures::channel::mpsc::UnboundedSender;
use serde_json::{Value, json};

use crate::backend::{
    http::{self, Cancel, Progress},
    process,
};

const PENDING: &str = ".pending";
const REMOVE: &str = ".remove";
/// The log starts over at the next start once it is this big.
const LOG_LIMIT: u64 = 1024 * 1024;

pub fn dir() -> PathBuf {
    crate::settings::data_dir().join("extensions")
}

/// An extension's own folder, kept across updates and uninstalls.
fn data_dir(id: &str) -> PathBuf {
    crate::settings::data_dir().join("extensions-data").join(id)
}

pub fn log_path() -> PathBuf {
    crate::settings::data_dir().join("extensions.log")
}

/// Carry out the installs, updates and uninstalls Settings left for this start.
pub fn apply_pending() {
    apply_pending_in(&dir());
    if std::fs::metadata(log_path()).is_ok_and(|m| m.len() > LOG_LIMIT) {
        let _ = std::fs::remove_file(log_path());
    }
}

fn apply_pending_in(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(id) = name.strip_suffix(REMOVE).filter(|id| valid_id(id)) {
            let _ = std::fs::remove_dir_all(dir.join(id));
            let _ = std::fs::remove_file(entry.path());
        } else if let Some(id) = name.strip_suffix(PENDING).filter(|id| valid_id(id)) {
            let _ = std::fs::remove_dir_all(dir.join(id));
            if let Err(err) = std::fs::rename(entry.path(), dir.join(id)) {
                log(id, &format!("could not finish installing: {err}"));
            }
        }
    }
}

/// An extension's folder, with its manifest or why that is unreadable.
pub struct Installed {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Result<Manifest, String>,
}

pub fn installed() -> Vec<Installed> {
    installed_in(&dir())
}

fn installed_in(dir: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut installed: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            valid_id(&id).then(|| {
                let dir = e.path();
                let manifest = read_manifest(&dir).and_then(|m| {
                    if m.id == id { Ok(m) } else { Err(format!("Its {MANIFEST} has the id \"{}\", but the folder is \"{id}\".", m.id)) }
                });
                Installed { id, dir, manifest }
            })
        })
        .collect();
    installed.sort_by(|a, b| a.id.cmp(&b.id));
    installed
}

fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).map_err(|_| format!("No {MANIFEST}."))?;
    Manifest::parse(&text)
}

// -- Running -------------------------------------------------------------------

/// From den to an extension's thread.
pub enum Message {
    Event { name: String, data: Value },
    /// The last message; answered once the extension has deactivated.
    Deactivate(mpsc::Sender<()>),
}

/// From an extension's thread to den, by the extension's id.
pub enum Report {
    Loaded(String),
    Failed(String, String),
    Toast(String, String),
    /// Its buttons for the windows on a folder (`root`, as den sent it).
    Buttons { id: String, root: String, buttons: Vec<Button> },
    /// A command to run in a new terminal of the window on `root`.
    Run { root: String, command: String, cwd: Option<String> },
    /// A file to open in the window on `root`, at a position if given.
    OpenFile { root: String, path: String, line: Option<u32>, column: Option<u32> },
    /// A tab of its view `view` in the window on `root`.
    OpenView { id: String, root: String, view: String },
    /// What its view `view` on `root` shows, laid over what it showed.
    SetView { id: String, root: String, view: String, content: Box<view::Content> },
    /// A question for the user in the window on `root`.
    Prompt { id: String, root: String, prompt: view::Prompt },
    /// A change to show side by side in the window on `root`.
    OpenDiff { root: String, diff: view::Diff },
    Copy(String),
}

/// Start `manifest`'s extension from `dir` on a thread of its own: load the
/// DLL, activate it, then hand it the messages sent here. How that goes
/// comes back as [`Report`]s.
/// `settings` are its settings' values, for its `Context`.
pub fn start(manifest: Manifest, dir: PathBuf, settings: serde_json::Map<String, Value>, reports: UnboundedSender<Report>) -> mpsc::Sender<Message> {
    let (tx, rx) = mpsc::channel();
    let name = format!("extension {}", manifest.id);
    let spawned = std::thread::Builder::new().name(name).spawn({
        let reports = reports.clone();
        let manifest = manifest.clone();
        move || run(manifest, dir, settings, rx, reports)
    });
    if let Err(err) = spawned {
        _ = reports.unbounded_send(Report::Failed(manifest.id, err.to_string()));
    }
    tx
}

fn run(manifest: Manifest, dir: PathBuf, settings: serde_json::Map<String, Value>, messages: mpsc::Receiver<Message>, reports: UnboundedSender<Report>) {
    let id = manifest.id.clone();
    let (api, state) = match activate(&manifest, &dir, settings, reports.clone()) {
        Ok(running) => running,
        Err(err) => {
            log(&id, &format!("failed: {err}"));
            _ = reports.unbounded_send(Report::Failed(id, err));
            return;
        }
    };
    log(&id, &format!("loaded {}", manifest.version));
    _ = reports.unbounded_send(Report::Loaded(id));
    for message in messages {
        match message {
            Message::Event { name, data } => {
                let (Ok(name), Ok(data)) = (CString::new(name), CString::new(data.to_string())) else { continue };
                // SAFETY: `state` came from this extension's `activate`; the strings live for the call.
                unsafe { (api.event)(state, name.as_ptr(), data.as_ptr()) };
            }
            Message::Deactivate(done) => {
                // SAFETY: as above, and `state` is not used again.
                unsafe { (api.deactivate)(state) };
                _ = done.send(());
                return;
            }
        }
    }
}

fn activate(
    manifest: &Manifest,
    dir: &Path,
    settings: serde_json::Map<String, Value>,
    reports: UnboundedSender<Report>,
) -> Result<(&'static abi::ExtensionApi, *mut c_void), String> {
    manifest.check_api()?;
    let file = manifest.library();
    let path = dir.join(&file);
    if !path.is_file() {
        return Err(format!("{file} is missing."));
    }
    // SAFETY: loading runs the DLL's initialisers. Installing it was the
    // user's decision to trust it; den can't check native code.
    let library = unsafe { libloading::Library::new(&path) }.map_err(|e| format!("Cannot load {file}: {e}"))?;
    // SAFETY: the symbol has the type `register!` gives it.
    let entry: abi::Entry = *unsafe { library.get::<abi::Entry>(den_extension::ENTRY) }.map_err(|_| "Not a den extension: it exports no den_extension_v1.".to_string())?;
    // Never unloaded: code of it may run until den exits (its threads, statics).
    std::mem::forget(library);
    // SAFETY: the entry returns a pointer to a static table, or null.
    let api = unsafe { entry() };
    if api.is_null() {
        return Err("Its entry point returned nothing.".into());
    }
    let api: &'static abi::ExtensionApi = unsafe { &*api };
    if api.version != API_VERSION {
        return Err(format!("Built for extension API {}, this den has {API_VERSION}.", api.version));
    }

    let data_dir = data_dir(&manifest.id);
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("Cannot create {}: {e}", data_dir.display()))?;
    let context = Context { den_version: crate::update::current_version().into(), data_dir, settings };
    let info = json!({ "id": manifest.id, "den_version": context.den_version, "data_dir": context.data_dir, "api": API_VERSION });
    // Both live as long as the process, as `HostApi` promises.
    let views = manifest.views.iter().map(|v| v.id.clone()).collect();
    let ctx: &'static HostCtx = Box::leak(Box::new(HostCtx { id: manifest.id.clone(), info, views, reports }));
    let host: &'static abi::HostApi = Box::leak(Box::new(abi::HostApi {
        version: API_VERSION,
        ctx: (ctx as *const HostCtx).cast_mut().cast(),
        call: host_call,
        free: host_free,
    }));
    let context = CString::new(serde_json::to_string(&context).unwrap_or_default()).unwrap_or_default();
    // SAFETY: valid pointers; the extension catches its own panics.
    let state = unsafe { (api.activate)(host, context.as_ptr()) };
    if state.is_null() {
        return Err(format!("It failed to start; see {}.", log_path().display()));
    }
    Ok((api, state))
}

/// den's side of an extension's `HostApi`.
struct HostCtx {
    id: String,
    info: Value,
    /// The ids of the views its manifest declares.
    views: Vec<String>,
    reports: UnboundedSender<Report>,
}

impl HostCtx {
    /// `args`' `view`, one the manifest declares.
    fn view(&self, args: &Value, method: &str) -> Result<String, String> {
        let view = string(args, "view", method)?;
        if !self.views.contains(&view) {
            return Err(format!("{method}: the manifest declares no view \"{view}\""));
        }
        Ok(view)
    }

    /// The host methods of this API version.
    fn call(&self, method: &str, args: &Value) -> Result<Value, String> {
        let message = || args["message"].as_str().map(str::to_string).ok_or_else(|| format!("{method} needs a \"message\""));
        match method {
            "log" => {
                log(&self.id, &message()?);
                Ok(Value::Null)
            }
            "toast" => self.report(Report::Toast(self.id.clone(), message()?)),
            "info" => Ok(self.info.clone()),
            "set_buttons" => {
                let root = string(args, "root", method)?;
                let buttons: Vec<Button> = serde_json::from_value(args["buttons"].clone()).map_err(|e| format!("set_buttons: bad \"buttons\": {e}"))?;
                self.report(Report::Buttons { id: self.id.clone(), root, buttons })
            }
            "run_in_terminal" => {
                let (root, command) = (string(args, "root", method)?, string(args, "command", method)?);
                let cwd = args["cwd"].as_str().map(str::to_string);
                self.report(Report::Run { root, command, cwd })
            }
            "open_file" => {
                let (root, path) = (string(args, "root", method)?, string(args, "path", method)?);
                let number = |key: &str| args[key].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
                self.report(Report::OpenFile { root, path, line: number("line"), column: number("column") })
            }
            "open_view" => {
                let (root, view) = (string(args, "root", method)?, self.view(args, method)?);
                self.report(Report::OpenView { id: self.id.clone(), root, view })
            }
            "set_view" => {
                let (root, view) = (string(args, "root", method)?, self.view(args, method)?);
                let content = serde_json::from_value(args["content"].clone()).map_err(|e| format!("set_view: bad \"content\": {e}"))?;
                self.report(Report::SetView { id: self.id.clone(), root, view, content: Box::new(content) })
            }
            "prompt" => {
                let root = string(args, "root", method)?;
                let prompt = serde_json::from_value(args["prompt"].clone()).map_err(|e| format!("prompt: bad \"prompt\": {e}"))?;
                self.report(Report::Prompt { id: self.id.clone(), root, prompt })
            }
            "open_diff" => {
                let root = string(args, "root", method)?;
                let diff = serde_json::from_value(args["diff"].clone()).map_err(|e| format!("open_diff: bad \"diff\": {e}"))?;
                self.report(Report::OpenDiff { root, diff })
            }
            "copy" => self.report(Report::Copy(string(args, "text", method)?)),
            _ => Err(format!("den has no method \"{method}\" (extension API {API_VERSION})")),
        }
    }
}

impl HostCtx {
    /// Hand `report` to den's UI thread.
    fn report(&self, report: Report) -> Result<Value, String> {
        self.reports.unbounded_send(report).map_err(|_| "den is closing".to_string())?;
        Ok(Value::Null)
    }
}

/// `args[key]`, which `method` needs.
fn string(args: &Value, key: &str, method: &str) -> Result<String, String> {
    args[key].as_str().map(str::to_string).ok_or_else(|| format!("{method} needs a \"{key}\""))
}

unsafe extern "C" fn host_call(ctx: *mut c_void, method: *const c_char, args: *const c_char) -> *mut c_char {
    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `ctx` is the leaked `HostCtx` of this extension; the strings are C strings or null.
        let ctx = unsafe { &*ctx.cast::<HostCtx>() };
        let method = unsafe { text(method) };
        let args = serde_json::from_str(&unsafe { text(args) }).unwrap_or(Value::Null);
        ctx.call(&method, &args)
    }))
    .unwrap_or_else(|_| Err("den failed on this call".into()));
    let reply = match reply {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error }),
    };
    // JSON escapes NUL, so this never fails.
    CString::new(reply.to_string()).unwrap_or_default().into_raw()
}

unsafe extern "C" fn host_free(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: `host_call` made it with `CString::into_raw`.
        drop(unsafe { CString::from_raw(text) });
    }
}

unsafe fn text(ptr: *const c_char) -> String {
    if ptr.is_null() { String::new() } else { unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned() }
}

static LOG: Mutex<()> = Mutex::new(());

/// A line in `extensions.log`: UTC time, the extension's id, the message.
pub fn log(id: &str, message: &str) {
    let _guard = LOG.lock();
    let path = log_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let _ = writeln!(file, "{} [{id}] {message}", utc(secs));
    }
}

/// `secs` since 1970 as `YYYY-MM-DD HH:MM:SS` (Howard Hinnant's civil_from_days).
fn utc(secs: u64) -> String {
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rest / 3600, rest / 60 % 60, rest % 60)
}

// -- Installing from GitHub ------------------------------------------------------

/// `owner/repo` from what was typed: that, or a link into the repository.
pub fn parse_repository(text: &str) -> Result<String, String> {
    let text = text.trim().trim_end_matches('/').trim_end_matches(".git");
    let path = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .map(|rest| rest.strip_prefix("www.").unwrap_or(rest))
        .map(|rest| rest.strip_prefix("github.com/").ok_or("Only GitHub repositories can be installed from."))
        .transpose()?
        .unwrap_or(text);
    let mut parts = path.split('/');
    let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
        return Err("Type the repository as owner/repo.".into());
    };
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && !s.starts_with('.');
    if !ok(owner) || !ok(repo) {
        return Err("Type the repository as owner/repo.".into());
    }
    Ok(format!("{owner}/{repo}"))
}

fn release_url(repo: &str, file: &str) -> String {
    format!("https://github.com/{repo}/releases/latest/download/{file}")
}

/// The manifest of `repo`'s latest release.
pub fn fetch_manifest(repo: &str) -> Result<Manifest, String> {
    let text = match http::AGENT.get(&release_url(repo, MANIFEST)).call() {
        Ok(response) => response.into_string().map_err(|e| format!("Cannot read {repo}'s {MANIFEST}: {e}"))?,
        Err(ureq::Error::Status(404, _)) => return Err(format!("{repo} has no release with an {MANIFEST}.")),
        Err(e) => return Err(format!("Cannot reach GitHub: {e}")),
    };
    let mut manifest = Manifest::parse(&text)?;
    if manifest.repository.is_empty() {
        manifest.repository = repo.to_string();
    }
    Ok(manifest)
}

// -- The index ---------------------------------------------------------------

/// Where den reads the curated list of extensions.
const INDEX_URL: &str = "https://raw.githubusercontent.com/patrickiel/den-extensions/main/index.json";
/// Overrides [`INDEX_URL`] with another URL or a local file, for testing an index.
const INDEX_ENV: &str = "DEN_EXTENSION_INDEX";

/// An extension in the index, as its latest release's manifest has it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Listing {
    pub repo: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: u32,
    #[serde(default)]
    pub description: String,
    /// The icon in the repository, empty when it has none.
    #[serde(default)]
    pub icon_url: String,
    #[serde(default)]
    pub readme_url: String,
}

impl Listing {
    /// Whether this den can load it.
    pub fn loadable(&self) -> bool {
        self.api == API_VERSION
    }
}

/// `index.json`: what den reads of it.
#[derive(serde::Deserialize)]
struct Index {
    extensions: Vec<Listing>,
}

fn index_cache() -> PathBuf {
    crate::settings::data_dir().join("extensions-index.json")
}

fn parse_index(text: &str) -> Result<Vec<Listing>, String> {
    let index: Index = serde_json::from_str(text).map_err(|e| format!("Bad extension index: {e}"))?;
    // One per id, and only ids that make safe folder names.
    let mut seen = std::collections::HashSet::new();
    Ok(index.extensions.into_iter().filter(|l| valid_id(&l.id) && seen.insert(l.id.clone())).collect())
}

/// The index from the web (or [`INDEX_ENV`]), cached for [`cached_index`].
pub fn fetch_index() -> Result<Vec<Listing>, String> {
    let source = std::env::var(INDEX_ENV).unwrap_or_else(|_| INDEX_URL.to_string());
    let text = if source.contains("://") {
        http::AGENT.get(&source).call().map_err(|e| format!("Cannot reach the extension index: {e}"))?.into_string().map_err(|e| e.to_string())?
    } else {
        std::fs::read_to_string(&source).map_err(|e| format!("Cannot read {source}: {e}"))?
    };
    let listings = parse_index(&text)?;
    let _ = std::fs::create_dir_all(crate::settings::data_dir());
    let _ = std::fs::write(index_cache(), text);
    Ok(listings)
}

/// Where `listing`'s icon is kept once fetched; by version, so a new one
/// replaces it.
pub fn icon_path(listing: &Listing) -> Option<PathBuf> {
    if listing.icon_url.is_empty() {
        return None;
    }
    let ext = if listing.icon_url.to_lowercase().ends_with(".svg") { "svg" } else { "png" };
    Some(crate::settings::data_dir().join("extensions-icons").join(format!("{}-{}.{ext}", listing.id, listing.version)))
}

/// The bytes at `source`, a URL or (for a local test index) a file, up to `limit`.
fn read_source(source: &str, limit: u64) -> Result<Vec<u8>, http::Error> {
    if source.contains("://") {
        return http::get_bytes(source, limit);
    }
    let file = std::fs::File::open(source).map_err(|_| http::Error::NotFound)?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, limit), &mut bytes).map_err(|e| http::Error::Other(e.to_string()))?;
    Ok(bytes)
}

/// Fetch `listing`'s icon to [`icon_path`] (den's views don't load images
/// from the web, so they show it from disk).
pub fn fetch_icon(listing: &Listing) -> Result<(), String> {
    let path = icon_path(listing).ok_or("no icon")?;
    fetch_icon_to(&listing.icon_url, &path)
}

fn fetch_icon_to(source: &str, path: &Path) -> Result<(), String> {
    let bytes = read_source(source, 1024 * 1024).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err("empty".into());
    }
    http::write_atomic(path, &bytes)
}

/// A listed extension's README.md, for its page before it is installed.
pub fn fetch_readme(url: &str) -> Result<String, String> {
    match read_source(url, 4 * 1024 * 1024) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(http::Error::NotFound) => Err("It has no README.md.".into()),
        Err(err) => Err(format!("Cannot fetch its README: {err}")),
    }
}

/// The index as last fetched, and how old that is; nothing before the first fetch.
pub fn cached_index() -> Option<(Vec<Listing>, std::time::Duration)> {
    let path = index_cache();
    let age = std::fs::metadata(&path).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    Some((parse_index(&std::fs::read_to_string(path).ok()?).ok()?, age))
}

/// Download `manifest`'s release zip from its repository and unpack it as
/// `<id>.pending`, to be installed at the next start.
pub fn install(manifest: &Manifest, cancel: &Cancel, progress: Progress) -> Result<(), String> {
    manifest.check_api()?;
    let dir = dir();
    let archive = dir.join(".downloads").join(manifest.asset());
    // Always a fresh copy: `download` keeps a file that is already there.
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::remove_file(http::part_path(&archive));
    let sha256 = Some(manifest.sha256.as_str()).filter(|s| !s.is_empty());
    http::download(&release_url(&manifest.repository, &manifest.asset()), &archive, sha256, "Downloading", cancel, progress)?;
    let pending = dir.join(format!("{}{PENDING}", manifest.id));
    let unpacked = process::unpack(&archive, &pending, "the extension").and_then(|()| check_unpacked(manifest, &pending));
    let _ = std::fs::remove_file(&archive);
    if let Err(err) = unpacked {
        let _ = std::fs::remove_dir_all(&pending);
        return Err(err);
    }
    let _ = std::fs::remove_file(dir.join(format!("{}{REMOVE}", manifest.id)));
    Ok(())
}

/// The zip holds the same extension, with its DLL; it keeps the repository for updates.
fn check_unpacked(manifest: &Manifest, pending: &Path) -> Result<(), String> {
    let mut unpacked = read_manifest(pending)?;
    if unpacked.id != manifest.id {
        return Err(format!("The release holds \"{}\", not \"{}\".", unpacked.id, manifest.id));
    }
    unpacked.check_api()?;
    if !pending.join(unpacked.library()).is_file() {
        return Err(format!("The release has no {}.", unpacked.library()));
    }
    if unpacked.repository.is_empty() {
        unpacked.repository = manifest.repository.clone();
        let json = serde_json::to_string_pretty(&unpacked).map_err(|e| e.to_string())?;
        std::fs::write(pending.join(MANIFEST), json).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Remove `id` at the next start (and drop an install waiting for it).
pub fn uninstall(id: &str) -> Result<(), String> {
    let dir = dir();
    let _ = std::fs::remove_dir_all(dir.join(format!("{id}{PENDING}")));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{id}{REMOVE}")), "").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("den-ext-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(dir: &Path, id: &str, manifest_id: &str) {
        std::fs::create_dir_all(dir.join(id)).unwrap();
        let manifest = format!(r#"{{"id":"{manifest_id}","name":"N","version":"1.0.0","api":1}}"#);
        std::fs::write(dir.join(id).join(MANIFEST), manifest).unwrap();
    }

    #[test]
    fn lists_extension_folders() {
        let dir = temp("list");
        put(&dir, "b-ext", "b-ext");
        put(&dir, "a-ext", "other");
        std::fs::create_dir_all(dir.join("no-manifest")).unwrap();
        std::fs::create_dir_all(dir.join(".downloads")).unwrap();
        std::fs::create_dir_all(dir.join("c.pending")).unwrap();
        let found = installed_in(&dir);
        let ids: Vec<_> = found.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["a-ext", "b-ext", "no-manifest"]);
        assert!(found[0].manifest.as_ref().unwrap_err().contains("\"other\""));
        assert!(found[1].manifest.is_ok());
        assert!(found[2].manifest.as_ref().unwrap_err().contains("No extension.json"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn applies_pending_installs_and_removals() {
        let dir = temp("pending");
        put(&dir, "old", "old");
        std::fs::write(dir.join("old.remove"), "").unwrap();
        put(&dir, "up", "up");
        std::fs::create_dir_all(dir.join("up.pending")).unwrap();
        std::fs::write(dir.join("up.pending").join("new.txt"), "").unwrap();
        put(&dir, "fresh.pending", "fresh");
        apply_pending_in(&dir);
        assert!(!dir.join("old").exists() && !dir.join("old.remove").exists());
        assert!(dir.join("up").join("new.txt").is_file() && !dir.join("up").join(MANIFEST).exists());
        assert!(dir.join("fresh").join(MANIFEST).is_file() && !dir.join("fresh.pending").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_repositories() {
        assert_eq!(parse_repository("me/ai-usage").unwrap(), "me/ai-usage");
        assert_eq!(parse_repository(" https://github.com/me/ai-usage/ ").unwrap(), "me/ai-usage");
        assert_eq!(parse_repository("https://github.com/me/ai.usage.git").unwrap(), "me/ai.usage");
        assert_eq!(parse_repository("https://github.com/me/x/releases/tag/v1").unwrap(), "me/x");
        assert!(parse_repository("https://gitlab.com/me/x").is_err());
        assert!(parse_repository("justaname").is_err());
        assert!(parse_repository("me/../x").is_err());
        assert!(parse_repository("me/x y").is_err());
    }

    #[test]
    fn reads_the_index_and_drops_bad_or_repeated_ids() {
        let text = r#"{ "generated": "2026-10-06", "extensions": [
            { "repo": "me/a", "id": "a", "name": "A", "version": "1.0.0", "api": 1, "icon_url": "https://x/icon.svg", "later": true },
            { "repo": "other/a", "id": "a", "name": "A again", "version": "9.0.0", "api": 1 },
            { "repo": "me/bad", "id": "Bad Id", "name": "B", "version": "1.0.0", "api": 1 },
            { "repo": "me/future", "id": "future", "name": "F", "version": "1.0.0", "api": 99 }
        ] }"#;
        let listings = parse_index(text).unwrap();
        assert_eq!(listings.iter().map(|l| l.repo.as_str()).collect::<Vec<_>>(), ["me/a", "me/future"]);
        assert_eq!((listings[0].icon_url.as_str(), listings[0].readme_url.as_str()), ("https://x/icon.svg", ""));
        assert!(listings[0].loadable() && !listings[1].loadable());
        assert!(parse_index("{ nope").is_err());
    }

    #[test]
    fn fetches_an_icon_once_whole() {
        let dir = temp("icon");
        let source = dir.join("icon.svg");
        std::fs::write(&source, "<svg/>").unwrap();
        let target = dir.join("cache").join("x-1.0.0.svg");
        fetch_icon_to(&source.to_string_lossy(), &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "<svg/>");
        assert!(!target.with_extension("part").exists());
        assert!(fetch_icon_to(&dir.join("gone.svg").to_string_lossy(), &dir.join("cache").join("y.svg")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn host_methods_answer_or_refuse() {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let ctx = HostCtx { id: "t".into(), info: json!({ "id": "t" }), views: vec!["graph".into()], reports: tx };
        assert_eq!(ctx.call("info", &Value::Null).unwrap()["id"], "t");
        assert!(ctx.call("toast", &json!({ "message": "hi" })).is_ok());
        assert!(ctx.call("toast", &json!({})).is_err());
        assert!(ctx.call("nope", &Value::Null).unwrap_err().contains("no method"));
    }

    #[test]
    fn views_are_only_the_manifests() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let ctx = HostCtx { id: "t".into(), info: Value::Null, views: vec!["graph".into()], reports: tx };
        ctx.call("open_view", &json!({ "root": "C:/p", "view": "graph" })).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Report::OpenView { view, .. } if view == "graph"));
        assert!(ctx.call("open_view", &json!({ "root": "C:/p", "view": "other" })).unwrap_err().contains("no view"));
        ctx.call("set_view", &json!({ "root": "C:/p", "view": "graph", "content": { "selected": "a" } })).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Report::SetView { content, .. } if content.selected.as_deref() == Some("a") && content.rows.is_none()));
        assert!(ctx.call("set_view", &json!({ "root": "C:/p", "view": "graph", "content": { "rows": 3 } })).is_err());
        ctx.call("prompt", &json!({ "root": "C:/p", "prompt": { "id": "q", "title": "Name?", "fields": [{ "id": "n" }] } })).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Report::Prompt { prompt, .. } if prompt.fields[0].kind == den_extension::view::FieldKind::Text));
        ctx.call("open_diff", &json!({ "root": "C:/p", "diff": { "path": "a.rs", "hash": "abc" } })).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Report::OpenDiff { diff, .. } if diff.hash.as_deref() == Some("abc")));
    }

    #[test]
    fn buttons_and_terminals_go_to_the_ui_thread() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let ctx = HostCtx { id: "t".into(), info: Value::Null, views: Vec::new(), reports: tx };
        let buttons = json!([{ "id": "build", "label": "Build", "icon": "hammer" }]);
        ctx.call("set_buttons", &json!({ "root": "C:/p", "buttons": buttons })).unwrap();
        match rx.try_recv().unwrap() {
            Report::Buttons { id, root, buttons } => {
                assert_eq!((id.as_str(), root.as_str()), ("t", "C:/p"));
                assert_eq!((buttons[0].label.as_str(), buttons[0].icon.as_str(), buttons[0].tooltip.as_str()), ("Build", "hammer", ""));
            }
            _ => panic!("not buttons"),
        }
        assert!(ctx.call("set_buttons", &json!({ "root": "C:/p", "buttons": [{ "label": "no id" }] })).is_err());
        assert!(ctx.call("set_buttons", &json!({ "buttons": [] })).unwrap_err().contains("\"root\""));
        ctx.call("run_in_terminal", &json!({ "root": "C:/p", "command": "cargo build" })).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Report::Run { command, cwd: None, .. } if command == "cargo build"));
        assert!(ctx.call("run_in_terminal", &json!({ "root": "C:/p" })).is_err());
        ctx.call("open_file", &json!({ "root": "C:/p", "path": "C:/p/a.rs", "line": 12 })).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Report::OpenFile { line: Some(12), column: None, .. }));
        assert!(ctx.call("open_file", &json!({ "root": "C:/p" })).unwrap_err().contains("\"path\""));
    }

    /// Loads a real DLL: `cargo build -p hello-extension`, then
    /// `cargo test -- --ignored loads_the_hello_extension`.
    #[test]
    #[ignore]
    fn loads_the_hello_extension() {
        let dll = std::env::current_dir().unwrap().join("target/debug/hello_extension.dll");
        assert!(dll.is_file(), "build it first: cargo build -p hello-extension");
        let appdata = temp("appdata");
        // SAFETY: only this ignored test reads APPDATA while it runs.
        unsafe { std::env::set_var("APPDATA", &appdata) };
        let ext = dir().join("hello-extension");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::copy(&dll, ext.join("hello_extension.dll")).unwrap();
        std::fs::copy("examples/hello-extension/extension.json", ext.join(MANIFEST)).unwrap();

        let found = installed();
        let manifest = found[0].manifest.clone().unwrap();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let messages = start(manifest, found[0].dir.clone(), Default::default(), tx);
        let mut next = || futures::executor::block_on(futures::StreamExt::next(&mut rx)).unwrap();
        assert!(matches!(next(), Report::Toast(id, message) if id == "hello-extension" && message.contains("Hello from an extension")));
        assert!(matches!(next(), Report::Loaded(id) if id == "hello-extension"));
        messages.send(Message::Event { name: den_extension::events::WORKSPACE_OPENED.into(), data: json!({ "root": "C:\\project" }) }).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        messages.send(Message::Deactivate(done_tx)).unwrap();
        done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let log = std::fs::read_to_string(log_path()).unwrap();
        assert!(log.contains("[hello-extension] loaded 0.1.0"), "{log}");
        assert!(log.contains("[hello-extension] opened C:\\project"), "{log}");
        assert!(log.contains("[hello-extension] goodbye"), "{log}");
        assert!(crate::settings::data_dir().join("extensions-data").join("hello-extension").is_dir());
    }

    /// Installs task-buttons from the live index as the Extensions view does,
    /// then loads it: `cargo test -- --ignored installs_from_the_live_index`.
    #[test]
    #[ignore]
    fn installs_from_the_live_index() {
        let appdata = temp("live-index");
        // SAFETY: only this ignored test reads APPDATA while it runs.
        unsafe { std::env::set_var("APPDATA", &appdata) };
        let listings = fetch_index().unwrap();
        let listing = listings.iter().find(|l| l.id == "task-buttons").expect("task-buttons is in the index");
        assert!(listing.loadable());
        assert!(cached_index().is_some_and(|(cached, _)| cached == listings));
        fetch_icon(listing).unwrap();
        assert!(fetch_readme(&listing.readme_url).unwrap().contains("Task Buttons"));

        let manifest = fetch_manifest(&listing.repo).unwrap();
        assert_eq!((manifest.version.as_str(), manifest.repository.as_str()), (listing.version.as_str(), "patrickiel/task-buttons"));
        assert_eq!(manifest.sha256.len(), 64, "the release's extension.json carries the zip's sha256");
        install(&manifest, &Cancel::default(), &mut |_, _, _| {}).unwrap();
        assert!(dir().join("task-buttons.pending").join(manifest.library()).is_file());

        apply_pending();
        let found = installed();
        let ext = found.iter().find(|i| i.id == "task-buttons").unwrap();
        let installed = ext.manifest.clone().unwrap();
        assert_eq!(installed.repository, "patrickiel/task-buttons");
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let messages = start(installed, ext.dir.clone(), Default::default(), tx);
        loop {
            match futures::executor::block_on(futures::StreamExt::next(&mut rx)).unwrap() {
                Report::Loaded(id) if id == "task-buttons" => break,
                Report::Failed(_, err) => panic!("failed to load: {err}"),
                _ => {}
            }
        }
        let (done_tx, done_rx) = mpsc::channel();
        messages.send(Message::Deactivate(done_tx)).unwrap();
        done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(std::fs::read_to_string(log_path()).unwrap().contains("[task-buttons] loaded 0.1.0"));
    }

    #[test]
    fn formats_utc_times() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(utc(1_790_000_000), "2026-09-21 14:13:20");
    }
}
