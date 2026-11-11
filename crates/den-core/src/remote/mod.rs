//! Folders on a server, reached over SSH: den-server does there what den
//! does for a folder on its own machine, and den asks it over one pipe.
//!
//! A folder on a server is a path like any other, as a folder inside WSL is:
//! `/.den-ssh/<host>/home/me/proj` (on Windows
//! `\\.\den-ssh\<host>\home\me\proj`). So everything that keeps a folder,
//! a file or a working directory keeps working, and only the code that
//! touches the disk or starts a program asks `route` where the path lives.
//! No such path exists on den's own machine: reading one from the disk
//! fails, so that code goes through `crate::fs` and the functions here.

pub mod client;
pub mod frame;
#[cfg(unix)]
pub mod host;
pub mod proto;
pub mod ssh;
pub mod term;
pub mod worker;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub use client::{Connection, Options, Stream};
pub use proto::{Fault, FaultKind};

const UNIX_PREFIX: &str = "/.den-ssh/";
/// The device namespace: an absolute path Windows resolves without asking
/// the network, as it would for `\\den-ssh\…`.
const WINDOWS_PREFIX: &str = r"\\.\den-ssh\";
/// A `\` in a file name on the server, in a Windows path (as WSL shows it).
const BACKSLASH: char = '\u{f05c}';

/// A server, as ssh names it: `host`, `me@host`, an alias from
/// `~/.ssh/config`, with `:port` after it for another port.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostId(String);

impl HostId {
    /// `None` for what cannot name a server: nothing, spaces, a path, or a
    /// leading `-`, which ssh would read as an option.
    pub fn new(destination: &str) -> Option<Self> {
        let fine = !destination.is_empty()
            && !destination.starts_with('-')
            && destination.chars().any(|c| c != '.')
            && !destination
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '/' || c == '\\');
        fine.then(|| HostId(destination.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The destination for ssh and the port, when one is named.
    pub fn destination(&self) -> (&str, Option<u16>) {
        match self.0.rsplit_once(':') {
            // Not the last group of a bare IPv6 address.
            Some((host, port)) if !host.contains(':') || host.ends_with(']') => {
                match port.parse() {
                    Ok(port) => (host, Some(port)),
                    Err(_) => (&self.0, None),
                }
            }
            _ => (&self.0, None),
        }
    }

    /// As one folder name in a path.
    fn encoded(&self) -> String {
        let mut out = String::new();
        for byte in self.0.bytes() {
            if byte.is_ascii_alphanumeric() || b"._@+-".contains(&byte) {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    fn decoded(component: &str) -> Option<Self> {
        HostId::new(&percent_decode(component))
    }
}

impl std::fmt::Display for HostId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = (bytes[at] == b'%')
            .then(|| text.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The server and the path there of a folder or file on a server; `None`
/// for a path on this machine.
pub fn split(path: &Path) -> Option<(HostId, String)> {
    split_text(&path.to_string_lossy(), cfg!(windows))
}

fn split_text(text: &str, windows: bool) -> Option<(HostId, String)> {
    if windows {
        let rest = text.strip_prefix(WINDOWS_PREFIX)?;
        let (host, tail) = match rest.find(['\\', '/']) {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => (rest, ""),
        };
        let posix: String = tail
            .chars()
            .map(|c| match c {
                '\\' => '/',
                BACKSLASH => '\\',
                c => c,
            })
            .collect();
        Some((HostId::decoded(host)?, clean(&format!("/{posix}"))))
    } else {
        let rest = text.strip_prefix(UNIX_PREFIX)?;
        let (host, tail) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        };
        Some((HostId::decoded(host)?, clean(tail)))
    }
}

/// The path den uses for `posix` on `host`.
pub fn join(host: &HostId, posix: &str) -> PathBuf {
    PathBuf::from(join_text(host, posix, cfg!(windows)))
}

fn join_text(host: &HostId, posix: &str, windows: bool) -> String {
    let posix = clean(posix);
    let tail = posix.trim_start_matches('/');
    if windows {
        let tail: String = tail
            .chars()
            .map(|c| match c {
                '\\' => BACKSLASH,
                '/' => '\\',
                c => c,
            })
            .collect();
        let host = host.encoded();
        if tail.is_empty() {
            format!("{WINDOWS_PREFIX}{host}")
        } else {
            format!("{WINDOWS_PREFIX}{host}\\{tail}")
        }
    } else if tail.is_empty() {
        format!("{}{}", UNIX_PREFIX, host.encoded())
    } else {
        format!("{}{}/{}", UNIX_PREFIX, host.encoded(), tail)
    }
}

/// An absolute path on the server without `.`, `..`, doubled or trailing
/// slashes: one spelling for one path.
fn clean(posix: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in posix.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The path `text` names, as something running in `base` wrote it (a path in
/// a terminal's output, the folder a shell reports): on `base`'s server when
/// `base` is on one, relative to `base` unless it is absolute.
pub fn resolve(base: &Path, text: &str) -> PathBuf {
    match split(base) {
        Some((host, _)) if text.starts_with('/') => join(&host, text),
        Some((host, dir)) => join(&host, &format!("{dir}/{text}")),
        None => base.join(text),
    }
}

/// A path for people: `host:/home/me/proj` for one on a server.
pub fn display(path: &Path) -> String {
    match split(path) {
        Some((host, posix)) => format!("{host}:{posix}"),
        None => path.display().to_string(),
    }
}

/// `ssh://[user@]host[:port][/path]` as the server and the path there (empty
/// for none, `~/…` for one that starts at the home folder: `ssh://host/~/x`).
pub fn parse_url(arg: &str) -> Option<(HostId, String)> {
    let rest = arg.strip_prefix("ssh://")?;
    let (host, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    let path = percent_decode(path);
    let path = match path.strip_prefix("/~") {
        Some(home) if home.is_empty() || home.starts_with('/') => format!("~{home}"),
        _ => path,
    };
    Some((HostId::new(&percent_decode(host))?, path))
}

/// Where a path's work is done.
pub enum Route {
    /// On this machine.
    Local,
    /// By the den-server on the other end, for this path there.
    Remote(Arc<Connection>, String),
    /// On a server den is not connected to now. Never to be tried on this
    /// machine instead: the path means nothing here.
    Offline(HostId),
}

/// Where the work on `path` is done.
pub fn route(path: &Path) -> Route {
    let Some((host, posix)) = split(path) else {
        return Route::Local;
    };
    match connection(&host) {
        Some(connection) => Route::Remote(connection, posix),
        None => Route::Offline(host),
    }
}

static CONNECTIONS: Mutex<BTreeMap<HostId, Arc<Connection>>> = Mutex::new(BTreeMap::new());

/// Make `connection` the one its server's folders use.
pub fn register(connection: Arc<Connection>) {
    CONNECTIONS
        .lock()
        .unwrap()
        .insert(connection.host().clone(), connection);
}

/// Forget the connection to `host`; calls under way on it end when it does.
pub fn unregister(host: &HostId) {
    CONNECTIONS.lock().unwrap().remove(host);
}

/// The live connection to `host`.
pub fn connection(host: &HostId) -> Option<Arc<Connection>> {
    CONNECTIONS
        .lock()
        .unwrap()
        .get(host)
        .filter(|connection| connection.alive())
        .cloned()
}

/// Why a call to a server did not come back with an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The connection is gone, or went while the call was under way. What
    /// the call was to do may or may not have happened.
    Disconnected,
    /// No answer in the time given.
    Timeout,
    /// den is not connected to this server.
    Offline(HostId),
    /// The connection could not be made: ssh's own words, mostly.
    Start(String),
    /// The server answered that it could not.
    Failed(Fault),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Disconnected => f.write_str("The connection to the server was lost."),
            Error::Timeout => f.write_str("The server did not answer."),
            Error::Offline(host) => write!(f, "Not connected to {host}."),
            Error::Start(message) => f.write_str(message),
            Error::Failed(fault) => f.write_str(&fault.message),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        use std::io::ErrorKind;
        match error {
            Error::Failed(fault) => fault.into(),
            Error::Timeout => std::io::Error::new(ErrorKind::TimedOut, error.to_string()),
            other => std::io::Error::new(ErrorKind::NotConnected, other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> HostId {
        HostId::new(name).unwrap()
    }

    #[test]
    fn a_folder_on_a_server_is_a_path_and_back() {
        let me = host("me@box");
        assert_eq!(
            join_text(&me, "/home/me/proj", false),
            "/.den-ssh/me@box/home/me/proj"
        );
        assert_eq!(
            split_text("/.den-ssh/me@box/home/me/proj", false),
            Some((me.clone(), "/home/me/proj".to_string()))
        );
        assert_eq!(
            join_text(&me, "/home/me/proj", true),
            r"\\.\den-ssh\me@box\home\me\proj"
        );
        assert_eq!(
            split_text(r"\\.\den-ssh\me@box\home\me\proj", true),
            Some((me.clone(), "/home/me/proj".to_string()))
        );
        // The server's root.
        assert_eq!(join_text(&me, "/", false), "/.den-ssh/me@box");
        assert_eq!(
            split_text("/.den-ssh/me@box", false),
            Some((me.clone(), "/".to_string()))
        );
        assert_eq!(join_text(&me, "/", true), r"\\.\den-ssh\me@box");
        assert_eq!(
            split_text(r"\\.\den-ssh\me@box", true),
            Some((me, "/".to_string()))
        );
    }

    #[test]
    fn paths_on_this_machine_are_left_alone() {
        assert_eq!(split_text("/home/me/proj", false), None);
        assert_eq!(split_text("/.den-ssh", false), None);
        assert_eq!(split_text(r"C:\Users\me", true), None);
        assert_eq!(split_text(r"\\wsl.localhost\Ubuntu\home\me", true), None);
        // A Windows path means nothing in the Unix form and the other way.
        assert_eq!(split_text(r"\\.\den-ssh\box\home", false), None);
        assert_eq!(split_text("/.den-ssh/box/home", true), None);
    }

    #[test]
    fn one_path_has_one_spelling() {
        let me = host("box");
        assert_eq!(
            split_text("/.den-ssh/box/home//me/./proj/", false),
            Some((me.clone(), "/home/me/proj".to_string()))
        );
        assert_eq!(
            join_text(&me, "home/me/../you/", false),
            "/.den-ssh/box/home/you"
        );
        assert_eq!(join_text(&me, "/../..", false), "/.den-ssh/box");
    }

    #[test]
    fn a_host_is_one_folder_name_whatever_is_in_it() {
        let odd = host("me@[fe80::1]:2222");
        let text = join_text(&odd, "/srv", false);
        assert_eq!(text, "/.den-ssh/me@%5Bfe80%3A%3A1%5D%3A2222/srv");
        assert_eq!(
            split_text(&text, false),
            Some((odd.clone(), "/srv".to_string()))
        );
        let windows = join_text(&odd, "/srv", true);
        assert!(!windows[WINDOWS_PREFIX.len()..].contains(':'));
        assert_eq!(split_text(&windows, true), Some((odd, "/srv".to_string())));
    }

    #[test]
    fn what_ssh_would_read_as_an_option_names_no_server() {
        assert_eq!(HostId::new("-oProxyCommand=evil"), None);
        assert_eq!(HostId::new(""), None);
        assert_eq!(HostId::new(".."), None);
        assert_eq!(HostId::new("a b"), None);
        assert_eq!(HostId::new("a/b"), None);
        assert_eq!(parse_url("ssh://-oProxyCommand=evil/x"), None);
        // Encoded, it is still refused once decoded.
        assert_eq!(split_text("/.den-ssh/%2DoProxyCommand=evil/x", false), None);
    }

    #[test]
    fn a_backslash_in_a_server_file_name_survives_windows() {
        let me = host("box");
        let text = join_text(&me, r"/tmp/a\b", true);
        assert_eq!(text, "\\\\.\\den-ssh\\box\\tmp\\a\u{f05c}b");
        assert_eq!(split_text(&text, true), Some((me, r"/tmp/a\b".to_string())));
    }

    #[test]
    fn ports_come_off_the_destination() {
        assert_eq!(host("me@box:2222").destination(), ("me@box", Some(2222)));
        assert_eq!(host("box").destination(), ("box", None));
        assert_eq!(host("fe80::1").destination(), ("fe80::1", None));
        assert_eq!(host("[fe80::1]:22").destination(), ("[fe80::1]", Some(22)));
    }

    #[test]
    fn urls_name_a_server_and_a_path() {
        assert_eq!(
            parse_url("ssh://me@box:2222/home/me/my%20proj"),
            Some((host("me@box:2222"), "/home/me/my proj".to_string()))
        );
        assert_eq!(parse_url("ssh://box"), Some((host("box"), String::new())));
        assert_eq!(
            parse_url("ssh://box/~/proj"),
            Some((host("box"), "~/proj".to_string()))
        );
        assert_eq!(
            parse_url("ssh://box/~"),
            Some((host("box"), "~".to_string()))
        );
        assert_eq!(
            parse_url("ssh://box/~me/x"),
            Some((host("box"), "/~me/x".to_string()))
        );
        assert_eq!(parse_url("/home/me"), None);
    }

    #[cfg(unix)]
    #[test]
    fn what_a_program_on_the_server_names_is_on_the_server() {
        let base = Path::new("/.den-ssh/box/home/me/proj");
        assert_eq!(
            resolve(base, "/etc/hosts"),
            PathBuf::from("/.den-ssh/box/etc/hosts")
        );
        assert_eq!(
            resolve(base, "./src/../Cargo.toml"),
            PathBuf::from("/.den-ssh/box/home/me/proj/Cargo.toml")
        );
        assert_eq!(
            resolve(Path::new("/home/me"), "src/a.rs"),
            PathBuf::from("/home/me/src/a.rs")
        );
        assert_eq!(display(base), "box:/home/me/proj");
        assert_eq!(display(Path::new("/home/me")), "/home/me");
    }

    #[test]
    fn a_folder_on_a_server_never_met_is_offline_not_local() {
        let path = join(&host("den-test-nobody"), "/srv");
        assert!(matches!(route(&path), Route::Offline(_)));
        assert!(matches!(route(Path::new("src")), Route::Local));
    }
}
