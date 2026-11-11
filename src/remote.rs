//! Folders on servers, on den's side: the connection each server needs,
//! made off the UI thread and kept for every folder open on it, and what
//! the windows show of it (connecting, connected, why not).
//!
//! The work itself is in den-core (`backend::remote`): a folder on a server
//! is a path, and `backend::fs`, git, search and the watch go to the server
//! for it.

use std::{
    collections::BTreeMap,
    hash::{Hash as _, Hasher as _},
    path::{Path, PathBuf},
    sync::Once,
};

use futures::channel::oneshot;
use gpui_kit::*;

use crate::backend::remote::{self, HostId, ssh};

/// Set, the remote-folder commands show: the work on them is not done.
pub const ENABLED_ENV: &str = "DEN_REMOTE";
/// The den-server to put on servers, in place of the one built beside den.
const SERVER_ENV: &str = "DEN_SERVER_BIN";

/// Whether folders on servers are offered at all.
pub fn enabled() -> bool {
    std::env::var_os(ENABLED_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Seconds den waits before each try to get a lost connection back; after
/// the last, the banner's Retry is the way.
const RETRY_AFTER: &[u64] = &[1, 3, 10, 30, 60, 60, 120, 300];

/// Terminals whose tab was closed while their server was out of reach: to
/// be ended there once it is back (they would run on unseen for good).
static OVERDUE: std::sync::Mutex<Vec<(PathBuf, u64)>> = std::sync::Mutex::new(Vec::new());

/// End the terminal `id` on the server `at` is on, the next time den is
/// connected to it.
pub fn kill_later(at: &Path, id: u64) {
    OVERDUE.lock().unwrap().push((at.to_path_buf(), id));
}

/// Connected to `host`: end the terminals `kill_later` was told of.
fn kill_overdue(host: &HostId) {
    let here = |at: &Path| remote::split(at).is_some_and(|(on, _)| on == *host);
    let mut overdue = OVERDUE.lock().unwrap();
    let (due, rest): (Vec<_>, Vec<_>) = overdue.drain(..).partition(|(at, _)| here(at));
    *overdue = rest;
    drop(overdue);
    if due.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("den-remote-work".into())
        .spawn(move || {
            for (at, id) in due {
                let _ = remote::term::kill(&at, id);
            }
        });
}

/// How den stands with a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Connected,
    /// Not connected, and why, in words to show.
    Down(SharedString),
}

/// The servers with a folder open, and each one's link.
#[derive(Default)]
pub struct Remotes {
    links: BTreeMap<HostId, Link>,
}

impl Global for Remotes {}

pub fn init(cx: &mut App) {
    cx.set_global(Remotes::default());
}

impl Remotes {
    /// The server `path` is on and den's link to it; `None` for a path on
    /// this machine.
    pub fn link(path: &Path, cx: &App) -> Option<(HostId, Link)> {
        let (host, _) = remote::split(path)?;
        let link = cx
            .global::<Remotes>()
            .links
            .get(&host)
            .cloned()
            .unwrap_or_else(|| Link::Down("Not connected.".into()));
        Some((host, link))
    }

    /// Connect to `host`, unless den is connected or connecting already.
    /// Everything watching `Remotes` hears how it went.
    pub fn connect(host: &HostId, cx: &mut App) {
        Self::attempt(host, None, cx)
    }

    /// `connect`; `again` counts the tries den has made by itself since the
    /// connection was lost (`None`: asked for, by a folder or by Retry).
    fn attempt(host: &HostId, again: Option<usize>, cx: &mut App) {
        if matches!(
            cx.global::<Remotes>().links.get(host),
            Some(Link::Connecting | Link::Connected)
        ) {
            return;
        }
        Self::set(host, Link::Connecting, cx);
        let (tx, rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("den-remote-connect".into())
            .spawn({
                let host = host.clone();
                move || {
                    let result = offer_server().and_then(|()| ssh::connect(&host));
                    let _ = tx.send(result);
                }
            })
            .expect("a thread");
        let host = host.clone();
        cx.spawn(async move |cx| {
            let Ok(result) = rx.await else { return };
            let (lost_tx, lost_rx) = oneshot::channel();
            let link = match result {
                Ok(connection) => {
                    connection.on_close(move || {
                        let _ = lost_tx.send(());
                    });
                    Link::Connected
                }
                Err(error) => Link::Down(explain(&host, &error).into()),
            };
            let connected = link == Link::Connected;
            _ = cx.update(|cx| Self::set(&host, link, cx));
            // Lost, den tries again by itself, a few times and ever later;
            // a first connection that fails waits for Retry (ssh wants
            // something only the user can give, most often).
            let next = match (connected, again) {
                (true, _) => {
                    kill_overdue(&host);
                    if lost_rx.await.is_err() {
                        return;
                    }
                    let lost = Link::Down("The connection to the server was lost.".into());
                    _ = cx.update(|cx| Self::set(&host, lost, cx));
                    0
                }
                (false, Some(tried)) => tried + 1,
                (false, None) => return,
            };
            let Some(wait) = RETRY_AFTER.get(next) else {
                return;
            };
            cx.background_executor()
                .timer(std::time::Duration::from_secs(*wait))
                .await;
            _ = cx.update(|cx| Self::attempt(&host, Some(next), cx));
        })
        .detach();
    }

    fn set(host: &HostId, link: Link, cx: &mut App) {
        cx.global_mut::<Remotes>().links.insert(host.clone(), link);
    }
}

/// Why den is not connected to `host`, with what to do about it when ssh
/// wanted an answer den has no terminal to ask for.
fn explain(host: &HostId, error: &remote::Error) -> String {
    let message = error.to_string();
    if ssh::wants_a_terminal(&message) {
        let (destination, _) = host.destination();
        format!("{message} Connect once from a terminal (`ssh {destination}`), then try again.")
    } else {
        message
    }
}

/// The server and the folder there that `server` and `folder` name, as
/// typed: `me@box` and `~/proj`, or all of it as `ssh://me@box/home/me/proj`.
pub fn destination(server: &str, folder: &str) -> Result<(HostId, String), String> {
    let (server, folder) = (server.trim(), folder.trim());
    if server.starts_with("ssh://") {
        let (host, path) =
            remote::parse_url(server).ok_or_else(|| format!("{server} names no server."))?;
        let path = if path.is_empty() {
            folder.to_string()
        } else {
            path
        };
        return Ok((host, path));
    }
    let host = HostId::new(server).ok_or_else(|| match server {
        "" => "Which server?".to_string(),
        _ => format!("{server} is not a server's name."),
    })?;
    Ok((host, folder.to_string()))
}

/// Find the folder `path` (`~`, `~/proj`, `/srv/app`) on `host`, connecting
/// first: den's path for it, as the server names it with links resolved, or
/// why not, in words to show.
pub fn find_folder(host: &HostId, path: &str, cx: &mut App) -> Task<Result<PathBuf, String>> {
    // The windows on this server hear of the link too.
    Remotes::connect(host, cx);
    let (host, path) = (host.clone(), path.to_string());
    spawn_on(true, cx.background_executor(), move || {
        find_folder_now(&host, &path)
    })
}

/// `find_folder`, waiting for the server: not for the UI thread.
pub fn find_folder_now(host: &HostId, path: &str) -> Result<PathBuf, String> {
    let connection = offer_server()
        .and_then(|()| ssh::connect(host))
        .map_err(|error| explain(host, &error))?;
    let shown = if path.is_empty() { "~" } else { path };
    let root = crate::backend::fs::canonicalize_on(&connection, path)
        .map_err(|error| format!("{host}: {shown}: {error}"))?;
    match crate::backend::fs::stat(&root) {
        Ok(Some(meta)) if meta.is_dir => Ok(root),
        Ok(_) => Err(format!("{} is not a folder.", remote::display(&root))),
        Err(error) => Err(format!("{}: {error}", remote::display(&root))),
    }
}

/// Run `work`, which reads or writes at `path`, where waiting is fine: for a
/// path on this machine the background executor, as ever. For one on a
/// server a thread of its own: a server that stopped answering must not hold
/// the few background threads every folder shares.
pub fn spawn<R: Send + 'static>(
    path: &Path,
    cx: &App,
    work: impl FnOnce() -> R + Send + 'static,
) -> Task<R> {
    spawn_on(is_remote(path), cx.background_executor(), work)
}

/// Whether `path` is on a server.
pub fn is_remote(path: &Path) -> bool {
    remote::split(path).is_some()
}

/// `spawn` from where there is no `App` at hand (inside a task): `remote`
/// says whether the work is for a path on a server.
pub fn spawn_on<R: Send + 'static>(
    remote: bool,
    executor: &BackgroundExecutor,
    work: impl FnOnce() -> R + Send + 'static,
) -> Task<R> {
    if !remote {
        return executor.spawn(async move { work() });
    }
    let (tx, rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("den-remote-work".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .expect("a thread");
    executor.spawn(async move { rx.await.expect("the work on a server ended") })
}

/// Whether `path` is a file, as far as den can say without waiting: one on a
/// server is taken to be (asking would hold the UI thread; opening it says
/// so when it is not).
pub fn is_file_now(path: &Path) -> bool {
    is_remote(path) || path.is_file()
}

/// `is_file_now`, of a folder.
pub fn is_dir_now(path: &Path) -> bool {
    is_remote(path) || path.is_dir()
}

/// A path as its own machine writes it, for the clipboard: `/home/me/a.rs`
/// for a file on a server, to paste into a terminal there.
pub fn plain(path: &Path) -> String {
    match remote::split(path) {
        Some((_, there)) => there,
        None => path.display().to_string(),
    }
}

/// Tell den-core which den-server to put on servers: once, the first time a
/// connection is made (the program is read from disk for it).
fn offer_server() -> Result<(), remote::Error> {
    static OFFERED: Once = Once::new();
    let mut failed = None;
    OFFERED.call_once(|| {
        // What this den was built with, unless another is named.
        if std::env::var_os(SERVER_ENV).is_none()
            && let Some(server) = carried()
        {
            return ssh::set_server(server);
        }
        offer_built_beside(&mut failed)
    });
    match failed {
        // Not offered: `ssh::connect` says there is none, the next time too.
        Some(message) => Err(remote::Error::Start(message)),
        None => Ok(()),
    }
}

/// The den-server programs a release of den carries (see `build.rs`), for
/// Linux on x86_64 and on aarch64.
#[cfg(den_server_embedded)]
fn carried() -> Option<ssh::Server> {
    use std::io::Read as _;
    static X86_64: &[u8] = include_bytes!(concat!(
        env!("DEN_SERVER_EMBEDDED"),
        "/den-server-linux-x86_64.gz"
    ));
    static AARCH64: &[u8] = include_bytes!(concat!(
        env!("DEN_SERVER_EMBEDDED"),
        "/den-server-linux-aarch64.gz"
    ));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (X86_64, AARCH64).hash(&mut hasher);
    Some(ssh::Server {
        id: format!("{:016x}", hasher.finish()),
        binary: Box::new(|machine| {
            let packed = match machine {
                "Linux x86_64" => X86_64,
                "Linux aarch64" | "Linux arm64" => AARCH64,
                _ => {
                    return Err(format!(
                        "den opens folders on Linux servers (x86_64 and aarch64); this one is {machine}."
                    ));
                }
            };
            let mut program = Vec::new();
            flate2::read::GzDecoder::new(packed)
                .read_to_end(&mut program)
                .map_err(|e| format!("den's own den-server does not unpack: {e}"))?;
            Ok(program)
        }),
    })
}

#[cfg(not(den_server_embedded))]
fn carried() -> Option<ssh::Server> {
    None
}

/// A den built with no den-server in it (a development build) puts the one
/// built beside it on servers of its own kind of machine.
fn offer_built_beside(failed: &mut Option<String>) {
    match built_beside() {
        Ok((path, program)) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            program.hash(&mut hasher);
            ssh::set_server(ssh::Server {
                id: format!("{:016x}", hasher.finish()),
                binary: Box::new(move |machine| {
                    let own = format!("Linux {}", std::env::consts::ARCH);
                    if cfg!(target_os = "linux") && machine == own {
                        Ok(program.clone())
                    } else {
                        Err(format!(
                            "The den-server at {} runs on {own} only, and the server is {machine}.",
                            path.display()
                        ))
                    }
                }),
            });
        }
        Err(message) => *failed = Some(message),
    }
}

/// The den-server built with this den: `DEN_SERVER_BIN`, else the one beside
/// den's own program (`target/debug/den-server`).
fn built_beside() -> Result<(PathBuf, Vec<u8>), String> {
    let path = match std::env::var_os(SERVER_ENV) {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("den-server")))
            .ok_or("den does not know where its own program is.")?,
    };
    match std::fs::read(&path) {
        Ok(program) => Ok((path, program)),
        Err(e) => Err(format!(
            "No den-server to put on the server: {} ({e}). Build it with `cargo build -p den-server`.",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::destination;

    #[test]
    fn a_server_and_a_folder_as_typed() {
        let (host, path) = destination(" me@box ", " ~/proj ").unwrap();
        assert_eq!((host.as_str(), path.as_str()), ("me@box", "~/proj"));
        // All of it as a URL: its own folder, or the one typed beside it.
        let (host, path) = destination("ssh://me@box:2222/srv/app", "~").unwrap();
        assert_eq!((host.as_str(), path.as_str()), ("me@box:2222", "/srv/app"));
        let (host, path) = destination("ssh://box", "~/proj").unwrap();
        assert_eq!((host.as_str(), path.as_str()), ("box", "~/proj"));
        assert!(destination("", "~").is_err());
        assert!(destination("-oProxyCommand=x", "~").is_err());
        assert!(destination("two words", "~").is_err());
    }
}
