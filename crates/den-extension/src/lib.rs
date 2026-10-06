//! den extensions, written in Rust and loaded by den as native DLLs.
//!
//! An extension is a `cdylib` crate that implements [`Extension`] and names
//! its type in [`register!`]. Next to the DLL sits `extension.json`, its
//! [`Manifest`]; den loads every folder under `%APPDATA%\den\extensions`
//! that holds both.
//!
//! Rust has no stable ABI, so nothing Rust-shaped crosses into den: the
//! boundary is two `#[repr(C)]` tables of `extern "C"` functions ([`abi`]),
//! and what goes through them is JSON in C strings. An extension built
//! against this crate keeps working across den updates and compilers as long
//! as [`API_VERSION`] stays the same; new host methods and events come as new
//! names, not as a new ABI.
//!
//! ```ignore
//! struct Hello { host: den_extension::Host }
//!
//! impl den_extension::Extension for Hello {
//!     fn activate(host: den_extension::Host, _: den_extension::Context) -> Self {
//!         host.toast("Hello from an extension");
//!         Hello { host }
//!     }
//! }
//!
//! den_extension::register!(Hello);
//! ```

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The version of the boundary in [`abi`]. den loads extensions built for
/// this version and refuses others.
pub const API_VERSION: u32 = 1;

/// The symbol an extension's DLL exports, NUL-terminated for the loader.
pub const ENTRY: &[u8] = b"den_extension_v1\0";

/// The manifest's file name, next to the DLL.
pub const MANIFEST: &str = "extension.json";

/// The platform part of a release asset's name (`<id>-windows-x86_64.zip`).
pub const PLATFORM: &str = "windows-x86_64";

/// Events den sends to [`Extension::event`], by name.
pub mod events {
    /// A window opened on a folder: `{ "root": "<path>" }`.
    pub const WORKSPACE_OPENED: &str = "workspace_opened";
    /// One of the extension's [`Button`](crate::Button)s was clicked:
    /// `{ "root": "<path>", "id": "<the button's id>" }`.
    pub const BUTTON_CLICKED: &str = "button_clicked";
    /// The user changed one of the extension's [`Setting`](crate::Setting)s:
    /// `{ "settings": { "<key>": <value>, … } }`, every setting's value, as in
    /// [`Context::settings`](crate::Context::settings).
    pub const SETTINGS_CHANGED: &str = "settings_changed";
    /// One of the extension's [`Command`](crate::Command)s was run from den's
    /// menu or its keybinding, in the window on a folder:
    /// `{ "root": "<path>", "id": "<the command's id>" }`.
    pub const COMMAND: &str = "command";
    /// The active tab of the window on a folder changed:
    /// `{ "root": "<path>", "path": "<file>" }`, `path` null when it isn't a file.
    pub const ACTIVE_FILE_CHANGED: &str = "active_file_changed";
    /// A file was saved in the window on a folder: `{ "root": "<path>", "path": "<file>" }`.
    pub const FILE_SAVED: &str = "file_saved";
}

/// `extension.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Lowercase letters, digits and `-`; also the crate's name, which makes
    /// the DLL's name (`-` becomes `_`, as Cargo does it).
    pub id: String,
    pub name: String,
    /// `x.y.z`.
    pub version: String,
    /// The [`API_VERSION`] it was built for.
    pub api: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The GitHub `owner/repo` whose releases carry it, for updates.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repository: String,
    /// The SHA-256 of the release zip, in hex; checked on install when set.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    /// An image in the extension's folder (`icon.png`, `images/icon.svg`),
    /// shown in den's Extensions view and on its page; square, 128 px or more.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// What the user can set, shown on the extension's page in den.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Setting>,
    /// What it adds to den's menu, each with an optional keybinding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Command>,
}

/// A command an extension declares in its manifest. den lists it in its menu
/// (under Extensions) and binds its keys; running it sends
/// [`events::COMMAND`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Command {
    /// The extension's own name for it, sent back when it runs.
    pub id: String,
    pub title: String,
    /// Keys in den's notation, `ctrl-alt-h` or `ctrl-k ctrl-s`. One den
    /// already uses is taken over while the extension runs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub keybinding: String,
}

/// A setting an extension declares in its manifest. den shows it on the
/// extension's page, keeps what the user sets, and hands the values over in
/// [`Context::settings`] and [`events::SETTINGS_CHANGED`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    /// The value's name in [`Context::settings`].
    pub key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    /// The value until the user sets one, of the setting's type.
    pub default: Value,
    /// A `choice`'s values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Boolean,
    Number,
    String,
    /// One of the setting's `options`, as a string.
    Choice,
    /// A type a newer den knows; this one leaves it at its default.
    #[serde(other)]
    Unknown,
}

impl Setting {
    /// Whether `value` is one this setting can take.
    pub fn accepts(&self, value: &Value) -> bool {
        match self.kind {
            SettingKind::Boolean => value.is_boolean(),
            SettingKind::Number => value.is_number(),
            SettingKind::String => value.is_string(),
            SettingKind::Choice => value.as_str().is_some_and(|v| self.options.iter().any(|o| o == v)),
            SettingKind::Unknown => false,
        }
    }
}

impl Manifest {
    /// Read and check `extension.json`'s text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(text).map_err(|e| format!("Bad {MANIFEST}: {e}"))?;
        if !valid_id(&manifest.id) {
            return Err(format!("Bad {MANIFEST}: the id \"{}\" may only have lowercase letters, digits and -", manifest.id));
        }
        if manifest.name.trim().is_empty() {
            return Err(format!("Bad {MANIFEST}: no name"));
        }
        let mut ids = std::collections::HashSet::new();
        if let Some(command) = manifest.commands.iter().find(|c| c.id.is_empty() || c.title.trim().is_empty() || !ids.insert(c.id.as_str())) {
            return Err(format!("Bad {MANIFEST}: each command needs an id of its own and a title (\"{}\")", command.id));
        }
        if !manifest.icon.is_empty() && !inside(&manifest.icon) {
            return Err(format!("Bad {MANIFEST}: the icon \"{}\" must be a path inside the extension's folder", manifest.icon));
        }
        Ok(manifest)
    }

    /// Whether this den can load it.
    pub fn check_api(&self) -> Result<(), String> {
        match self.api {
            API_VERSION => Ok(()),
            api if api > API_VERSION => Err(format!("Needs a newer den (extension API {api}, this den has {API_VERSION}).")),
            api => Err(format!("Built for extension API {api}, which this den no longer loads.")),
        }
    }

    /// The DLL's file name, as Cargo names a `cdylib` crate called `id`.
    pub fn library(&self) -> String {
        format!("{}{}{}", std::env::consts::DLL_PREFIX, self.id.replace('-', "_"), std::env::consts::DLL_SUFFIX)
    }

    /// Every setting's value: the user's where they set one this setting
    /// accepts, else its default.
    pub fn settings_values(&self, user: &Map<String, Value>) -> Map<String, Value> {
        self.settings
            .iter()
            .map(|setting| {
                let value = user.get(&setting.key).filter(|v| setting.accepts(v)).unwrap_or(&setting.default);
                (setting.key.clone(), value.clone())
            })
            .collect()
    }

    /// The release asset with the manifest and the DLL.
    pub fn asset(&self) -> String {
        format!("{}-{PLATFORM}.zip", self.id)
    }
}

/// `path` is relative and stays inside the folder it is relative to.
fn inside(path: &str) -> bool {
    let path = std::path::Path::new(path);
    path.components().all(|c| matches!(c, std::path::Component::Normal(_))) && path.components().next().is_some()
}

/// An id is safe as a folder name and a crate name.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// What den tells an extension when it activates it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Context {
    /// den's version, `x.y.z`.
    pub den_version: String,
    /// A folder of the extension's own that survives updates; created by den.
    pub data_dir: PathBuf,
    /// The values of the manifest's [`Setting`]s by key; later changes come
    /// as [`events::SETTINGS_CHANGED`]. `serde_json::from_value` turns them
    /// into a struct of the extension's own.
    #[serde(default)]
    pub settings: Map<String, Value>,
}

/// A button an extension puts in the title bar of the windows on a folder,
/// with [`Host::set_buttons`]. A click comes back as
/// [`events::BUTTON_CLICKED`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Button {
    /// The extension's own name for it, sent back on a click.
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tooltip: String,
    /// A [Lucide](https://lucide.dev/icons) icon's name (`play`,
    /// `flask-conical`), shown before the label; an unknown name shows none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// The label's colour, `#rrggbb`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
    /// A regular expression: shown only while the active tab is a file whose
    /// path it matches. One that doesn't compile hides the button.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_pattern: String,
}

/// The C boundary. Extensions don't use it directly: [`register!`] and
/// [`Host`] wrap it.
pub mod abi {
    use std::ffi::{c_char, c_void};

    /// den's side, handed to `activate`. It stays valid for as long as the
    /// process runs, and `call` may be used from any thread.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct HostApi {
        pub version: u32,
        pub ctx: *mut c_void,
        /// Run host `method` with JSON `args`. Returns JSON, `{"ok": …}` or
        /// `{"error": "…"}`, which the extension gives back to `free`.
        pub call: unsafe extern "C" fn(ctx: *mut c_void, method: *const c_char, args: *const c_char) -> *mut c_char,
        pub free: unsafe extern "C" fn(text: *mut c_char),
    }

    /// The extension's side, returned by its entry symbol. den calls all
    /// three from one thread of the extension's own.
    #[repr(C)]
    pub struct ExtensionApi {
        pub version: u32,
        /// `context` is a JSON [`Context`](crate::Context). Returns the
        /// extension's state, or null when it failed.
        pub activate: unsafe extern "C" fn(host: *const HostApi, context: *const c_char) -> *mut c_void,
        /// An event by name with its JSON data; the strings live for the call.
        pub event: unsafe extern "C" fn(state: *mut c_void, name: *const c_char, data: *const c_char),
        /// The last call; it frees the state.
        pub deactivate: unsafe extern "C" fn(state: *mut c_void),
    }

    /// The entry symbol, [`ENTRY`](crate::ENTRY).
    pub type Entry = unsafe extern "C" fn() -> *const ExtensionApi;
}

/// den, as an extension sees it. Cheap to copy, and usable from any thread.
#[derive(Clone, Copy)]
pub struct Host {
    api: abi::HostApi,
}

// SAFETY: den guarantees `call` and `free` are thread-safe and `ctx` lives
// for the whole process (see `abi::HostApi`).
unsafe impl Send for Host {}
unsafe impl Sync for Host {}

impl Host {
    /// Run a host method. The methods of this [`API_VERSION`]: `log`
    /// (`{"message"}`), `toast` (`{"message"}`), `info` (no args),
    /// `set_buttons` (`{"root", "buttons"}`), `run_in_terminal`
    /// (`{"root", "command", "cwd"?}`) and `open_file`
    /// (`{"root", "path", "line"?, "column"?}`). A den older than a method answers
    /// with an error naming it.
    pub fn call(&self, method: &str, args: Value) -> Result<Value, String> {
        let method = CString::new(method).map_err(|e| e.to_string())?;
        let args = CString::new(args.to_string()).map_err(|e| e.to_string())?;
        // SAFETY: valid C strings for the call; the result is den's to free.
        let reply = unsafe { (self.api.call)(self.api.ctx, method.as_ptr(), args.as_ptr()) };
        if reply.is_null() {
            return Err("den gave no reply".into());
        }
        // SAFETY: den returns a NUL-terminated string it allocated.
        let text = unsafe { CStr::from_ptr(reply) }.to_string_lossy().into_owned();
        unsafe { (self.api.free)(reply) };
        let mut reply: Value = serde_json::from_str(&text).map_err(|e| format!("Bad reply from den: {e}"))?;
        match reply.get_mut("error") {
            Some(error) => Err(error.as_str().unwrap_or("error").to_string()),
            None => Ok(reply.get_mut("ok").map(Value::take).unwrap_or(Value::Null)),
        }
    }

    /// A line in `%APPDATA%\den\extensions.log`, under the extension's id.
    pub fn log(&self, message: impl Into<String>) {
        _ = self.call("log", serde_json::json!({ "message": message.into() }));
    }

    /// A toast in den's window.
    pub fn toast(&self, message: impl Into<String>) {
        _ = self.call("toast", serde_json::json!({ "message": message.into() }));
    }

    /// Show `buttons` in the title bar of the windows on `root` (as
    /// [`events::WORKSPACE_OPENED`] named it), in place of the ones this
    /// extension set there before; none removes them.
    pub fn set_buttons(&self, root: &str, buttons: &[Button]) -> Result<(), String> {
        self.call("set_buttons", serde_json::json!({ "root": root, "buttons": buttons })).map(drop)
    }

    /// Open a terminal tab in the window on `root` and run `command` in its
    /// shell, in `cwd` (else `root`). Nothing happens when no window is open
    /// on `root`.
    pub fn run_in_terminal(&self, root: &str, command: &str, cwd: Option<&str>) -> Result<(), String> {
        self.call("run_in_terminal", serde_json::json!({ "root": root, "command": command, "cwd": cwd })).map(drop)
    }

    /// Open `path` in an editor tab of the window on `root` (its tab if it is
    /// open), with the cursor at `line` (from 1) when given. Nothing happens
    /// when no window is open on `root`.
    pub fn open_file(&self, root: &str, path: &str, line: Option<u32>) -> Result<(), String> {
        self.call("open_file", serde_json::json!({ "root": root, "path": path, "line": line })).map(drop)
    }
}

/// An extension. den activates it once at start, sends it events on a
/// thread of its own, and deactivates it when den quits.
pub trait Extension: Sized + 'static {
    /// A panic here fails the extension (logged), not den.
    fn activate(host: Host, context: Context) -> Self;

    /// An event from [`events`]; names this extension does not know are fine
    /// to ignore.
    fn event(&mut self, name: &str, data: Value) {
        let _ = (name, data);
    }

    /// den is quitting; it waits only briefly.
    fn deactivate(&mut self) {}
}

/// Export `$ty`, an [`Extension`], as the DLL's entry point.
#[macro_export]
macro_rules! register {
    ($ty:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn den_extension_v1() -> *const $crate::abi::ExtensionApi {
            static API: $crate::abi::ExtensionApi = $crate::abi::ExtensionApi {
                version: $crate::API_VERSION,
                activate: $crate::shim::activate::<$ty>,
                event: $crate::shim::event::<$ty>,
                deactivate: $crate::shim::deactivate::<$ty>,
            };
            &API
        }
    };
}

/// What [`register!`] exports, generic over the extension. Every call catches
/// panics: one must never unwind into den.
#[doc(hidden)]
pub mod shim {
    use super::*;

    /// # Safety
    /// `host` points to a valid `HostApi`; `context` is a C string or null.
    pub unsafe extern "C" fn activate<T: Extension>(host: *const abi::HostApi, context: *const c_char) -> *mut c_void {
        if host.is_null() {
            return std::ptr::null_mut();
        }
        let host = Host { api: unsafe { *host } };
        let context = serde_json::from_str(&unsafe { text(context) }).unwrap_or_default();
        match catch_unwind(AssertUnwindSafe(|| T::activate(host, context))) {
            Ok(extension) => Box::into_raw(Box::new(extension)).cast(),
            Err(panic) => {
                host.log(format!("activate panicked: {}", panic_message(&*panic)));
                std::ptr::null_mut()
            }
        }
    }

    /// # Safety
    /// `state` came from `activate::<T>`; `name` and `data` are C strings or null.
    pub unsafe extern "C" fn event<T: Extension>(state: *mut c_void, name: *const c_char, data: *const c_char) {
        if state.is_null() {
            return;
        }
        let extension = unsafe { &mut *state.cast::<T>() };
        let name = unsafe { text(name) };
        let data = serde_json::from_str(&unsafe { text(data) }).unwrap_or(Value::Null);
        _ = catch_unwind(AssertUnwindSafe(|| extension.event(&name, data)));
    }

    /// # Safety
    /// `state` came from `activate::<T>` and is not used again.
    pub unsafe extern "C" fn deactivate<T: Extension>(state: *mut c_void) {
        if state.is_null() {
            return;
        }
        let mut extension = unsafe { Box::from_raw(state.cast::<T>()) };
        _ = catch_unwind(AssertUnwindSafe(move || {
            extension.deactivate();
            drop(extension);
        }));
    }

    unsafe fn text(ptr: *const c_char) -> String {
        if ptr.is_null() { String::new() } else { unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned() }
    }

    fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
        panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// What the fake host was called with.
    static CALLS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    unsafe extern "C" fn fake_call(_: *mut c_void, method: *const c_char, args: *const c_char) -> *mut c_char {
        let method = unsafe { CStr::from_ptr(method) }.to_string_lossy().into_owned();
        let args = unsafe { CStr::from_ptr(args) }.to_string_lossy().into_owned();
        let reply = if method == "info" { r#"{"ok":{"den_version":"9.9.9"}}"# } else { r#"{"ok":null}"# };
        CALLS.lock().unwrap().push((method, args));
        CString::new(reply).unwrap().into_raw()
    }

    unsafe extern "C" fn fake_free(text: *mut c_char) {
        drop(unsafe { CString::from_raw(text) });
    }

    fn fake_host() -> abi::HostApi {
        abi::HostApi { version: API_VERSION, ctx: std::ptr::null_mut(), call: fake_call, free: fake_free }
    }

    fn calls_with(method: &str) -> Vec<String> {
        CALLS.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, a)| a.clone()).collect()
    }

    struct Counter {
        host: Host,
        events: Vec<String>,
    }

    impl Extension for Counter {
        fn activate(host: Host, context: Context) -> Self {
            host.toast(format!("counter on den {}", context.den_version));
            Counter { host, events: Vec::new() }
        }

        fn event(&mut self, name: &str, data: Value) {
            self.events.push(format!("{name} {data}"));
        }

        fn deactivate(&mut self) {
            self.host.log(format!("counter saw {}", self.events.join(", ")));
        }
    }

    struct Broken;

    impl Extension for Broken {
        fn activate(_: Host, _: Context) -> Self {
            panic!("broken on purpose")
        }
    }

    register!(Counter);

    fn c(text: &str) -> CString {
        CString::new(text).unwrap()
    }

    #[test]
    fn runs_an_extension_through_the_abi() {
        let api = unsafe { &*den_extension_v1() };
        assert_eq!(api.version, API_VERSION);
        let host = fake_host();
        let state = unsafe { (api.activate)(&host, c(r#"{"den_version":"1.2.3","data_dir":"x"}"#).as_ptr()) };
        assert!(!state.is_null());
        assert!(calls_with("toast").iter().any(|a| a.contains("counter on den 1.2.3")));
        unsafe { (api.event)(state, c(events::WORKSPACE_OPENED).as_ptr(), c(r#"{"root":"C:\\p"}"#).as_ptr()) };
        unsafe { (api.deactivate)(state) };
        assert!(calls_with("log").iter().any(|a| a.contains("counter saw workspace_opened") && a.contains("C:")));
    }

    #[test]
    fn a_panicking_activate_fails_and_logs() {
        let host = fake_host();
        let state = unsafe { shim::activate::<Broken>(&host, std::ptr::null()) };
        assert!(state.is_null());
        assert!(calls_with("log").iter().any(|a| a.contains("broken on purpose")));
    }

    #[test]
    fn host_calls_unwrap_ok_and_error() {
        let host = Host { api: fake_host() };
        assert_eq!(host.call("info", Value::Null).unwrap()["den_version"], "9.9.9");
        unsafe extern "C" fn failing(_: *mut c_void, _: *const c_char, _: *const c_char) -> *mut c_char {
            CString::new(r#"{"error":"no such method"}"#).unwrap().into_raw()
        }
        let host = Host { api: abi::HostApi { call: failing, ..fake_host() } };
        assert_eq!(host.call("nope", Value::Null).unwrap_err(), "no such method");
    }

    #[test]
    fn parses_manifests() {
        let m = Manifest::parse(r#"{"id":"ai-usage","name":"AI Usage","version":"0.1.0","api":1,"repository":"me/ai-usage"}"#).unwrap();
        assert_eq!(m.repository, "me/ai-usage");
        assert!(m.check_api().is_ok());
        assert_eq!(m.asset(), "ai-usage-windows-x86_64.zip");
        if cfg!(windows) {
            assert_eq!(m.library(), "ai_usage.dll");
        }
        assert!(Manifest::parse(r#"{"id":"Bad Id","name":"x","version":"1","api":1}"#).is_err());
        assert!(Manifest::parse(r#"{"id":"x","name":" ","version":"1","api":1}"#).is_err());
        assert!(Manifest::parse(r#"{"id":"x","name":"x"}"#).is_err());
        let icon = |icon: &str| Manifest::parse(&format!(r#"{{"id":"x","name":"x","version":"1","api":1,"icon":{icon:?}}}"#));
        assert_eq!(icon("images/icon.png").unwrap().icon, "images/icon.png");
        for bad in ["../icon.png", "/icon.png", "C:\\icon.png", "images/../../x.png"] {
            assert!(icon(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_commands() {
        let m = Manifest::parse(
            r#"{"id":"x","name":"x","version":"1.0.0","api":1,"commands":[
                {"id":"hello","title":"Say Hello","keybinding":"ctrl-alt-h"},{"id":"bye","title":"Bye"}]}"#,
        )
        .unwrap();
        assert_eq!((m.commands[0].keybinding.as_str(), m.commands[1].keybinding.as_str()), ("ctrl-alt-h", ""));
        let bad = |commands: &str| Manifest::parse(&format!(r#"{{"id":"x","name":"x","version":"1.0.0","api":1,"commands":{commands}}}"#)).is_err();
        assert!(bad(r#"[{"id":"a","title":"A"},{"id":"a","title":"B"}]"#));
        assert!(bad(r#"[{"id":"","title":"A"}]"#));
        assert!(bad(r#"[{"id":"a","title":" "}]"#));
    }

    #[test]
    fn resolves_settings_against_their_defaults() {
        let m = Manifest::parse(
            r#"{"id":"x","name":"x","version":"1.0.0","api":1,"settings":[
                {"key":"on","title":"On","type":"boolean","default":true},
                {"key":"n","title":"N","type":"number","default":5},
                {"key":"pm","title":"PM","type":"choice","default":"auto","options":["auto","pnpm"]},
                {"key":"later","title":"Later","type":"colour","default":"red"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(m.settings[3].kind, SettingKind::Unknown);
        let user = serde_json::json!({ "on": false, "n": "not a number", "pm": "pnpm", "gone": 1, "later": "blue" });
        let values = m.settings_values(user.as_object().unwrap());
        assert_eq!(Value::Object(values), serde_json::json!({ "on": false, "n": 5, "pm": "pnpm", "later": "red" }));
        assert!(!m.settings[2].accepts(&serde_json::json!("yarn")));
    }

    #[test]
    fn checks_the_api_version() {
        let mut m = Manifest::parse(r#"{"id":"x","name":"x","version":"1.0.0","api":1}"#).unwrap();
        m.api = API_VERSION + 1;
        assert!(m.check_api().unwrap_err().contains("newer den"));
        m.api = 0;
        assert!(m.check_api().is_err());
    }

    #[test]
    fn validates_ids() {
        assert!(valid_id("hello-extension"));
        assert!(valid_id("x2"));
        for bad in ["", "-x", "x-", "X", "a b", "a/b", "a.b", "..", "a_b"] {
            assert!(!valid_id(bad), "{bad}");
        }
    }
}
