//! den-server's end of the pipe: it does what den asks, with the same code
//! den runs for a folder on its own machine, and answers.
//!
//! One worker serves one connection and ends with it. Each request runs on
//! a thread of its own, so a long `git fetch` holds up nothing else.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use futures::StreamExt as _;

use super::{
    frame::{self, Assembler, Sender},
    proto::{
        Ask, DirEntry, Fault, FaultKind, Item, MAGIC, Meta, Reply, Request, Response, Tell, VERSION,
    },
};
use crate::{git, search, watch};

/// Changes on disk are sent this long after the first of a burst.
const WATCH_BATCH: Duration = Duration::from_millis(40);
/// When den is gone, what is under way gets this long to end (a commit must
/// not be cut off with its lock held).
const LINGER: Duration = Duration::from_secs(60);

type Answered = Result<(Response, Vec<u8>), Fault>;
type Cancel = Box<dyn FnOnce() + Send>;

/// Serve den on `input` and `output` until `input` closes.
pub fn serve(mut input: impl Read, mut output: impl Write + Send + 'static) {
    if writeln!(output, "{MAGIC}{VERSION}")
        .and_then(|()| output.flush())
        .is_err()
    {
        return;
    }
    let (out, writer) = frame::writer(output);
    let jobs = Arc::new(Jobs::default());
    let attached = Arc::new(Attached::default());
    let mut assembler = Assembler::default();
    while let Ok(Some(frame)) = frame::read_frame(&mut input) {
        let Some(message) = assembler.push(frame) else {
            continue;
        };
        match message.kind {
            frame::REQUEST => {
                let request = serde_json::from_slice::<Request>(&message.json);
                // What is typed, in the order it was: not from a thread each.
                let typed = match &request {
                    Ok(Request::TermWrite { id }) => Some((*id, Ask::Write)),
                    Ok(Request::TermResize { id, cols, rows }) => {
                        let (cols, rows) = (*cols, *rows);
                        Some((*id, Ask::Resize { cols, rows }))
                    }
                    Ok(Request::TermAck { id, offset }) => {
                        Some((*id, Ask::Ack { offset: *offset }))
                    }
                    _ => None,
                };
                if let Some((terminal, ask)) = typed {
                    attached.pass(terminal, ask, message.blob);
                    continue;
                }
                let (id, out, jobs) = (message.id, out.clone(), jobs.clone());
                let attached = attached.clone();
                jobs.begin(id);
                std::thread::spawn(move || {
                    let answered = match request {
                        Ok(request) => handle(id, request, message.blob, &out, &jobs, &attached),
                        Err(e) => Err(Fault {
                            kind: FaultKind::Invalid,
                            message: format!("This den-server does not know the request: {e}"),
                        }),
                    };
                    if id != 0 {
                        let (reply, blob) = match answered {
                            Ok((response, blob)) => (Reply::Ok(response), blob),
                            Err(fault) => (Reply::Err(fault), Vec::new()),
                        };
                        let json = serde_json::to_vec(&reply).expect("a reply is plain data");
                        out.send(frame::RESPONSE, id, json, blob);
                    }
                    jobs.end(id);
                });
            }
            frame::CANCEL => jobs.cancel(message.id),
            _ => {}
        }
    }
    // den is gone: stop what only it wanted, let the rest end.
    jobs.cancel_all();
    jobs.wait(LINGER);
    drop(out);
    let _ = writer.join();
}

/// The requests under way and how to stop the ones that can be.
#[derive(Default)]
struct Jobs {
    running: Mutex<HashMap<u32, Slot>>,
    changed: Condvar,
}

enum Slot {
    Running,
    /// Cancelled before it could say how.
    Cancelled,
    Stoppable(Cancel),
}

impl Jobs {
    fn begin(&self, id: u32) {
        if id != 0 {
            self.running.lock().unwrap().insert(id, Slot::Running);
        }
    }

    fn end(&self, id: u32) {
        self.running.lock().unwrap().remove(&id);
        self.changed.notify_all();
    }

    /// How to stop the request `id`; run at once when it was cancelled
    /// already.
    fn on_cancel(&self, id: u32, cancel: Cancel) {
        let mut running = self.running.lock().unwrap();
        match running.get(&id) {
            Some(Slot::Running) => {
                running.insert(id, Slot::Stoppable(cancel));
            }
            _ => {
                drop(running);
                cancel();
            }
        }
    }

    fn cancel(&self, id: u32) {
        let slot = {
            let mut running = self.running.lock().unwrap();
            match running.get(&id) {
                Some(_) => running.insert(id, Slot::Cancelled),
                None => None,
            }
        };
        if let Some(Slot::Stoppable(cancel)) = slot {
            cancel();
        }
    }

    fn cancel_all(&self) {
        let ids: Vec<u32> = self.running.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.cancel(id);
        }
    }

    /// Wait until nothing runs, at most `patience`.
    fn wait(&self, patience: Duration) {
        let until = Instant::now() + patience;
        let mut running = self.running.lock().unwrap();
        while !running.is_empty() {
            let Some(left) = until.checked_duration_since(Instant::now()) else {
                return;
            };
            running = self.changed.wait_timeout(running, left).unwrap().0;
        }
    }
}

/// What is typed into one terminal, in order.
type Line = std::sync::mpsc::Sender<(Ask, Vec<u8>)>;

/// The terminals den is attached to through this connection: each takes
/// what is typed from a line of its own, so one whose program reads nothing
/// holds up no other.
#[derive(Default)]
struct Attached {
    lines: Mutex<HashMap<u64, (u64, Line)>>,
}

impl Attached {
    fn pass(&self, terminal: u64, ask: Ask, blob: Vec<u8>) {
        if let Some((_, line)) = self.lines.lock().unwrap().get(&terminal) {
            let _ = line.send((ask, blob));
        }
    }
}

/// Attach den to the terminal `terminal` of this machine's host: what the
/// terminal says goes to den as items of the request `id`, until the
/// terminal ends or den cancels.
#[cfg(unix)]
fn attach(
    id: u32,
    terminal: u64,
    have: Option<u64>,
    out: &Sender,
    jobs: &Jobs,
    attached: &Attached,
) -> Result<(), Fault> {
    use super::host;
    static LINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let mut stream = host::connect()?;
    host::send(&mut stream, &Ask::Attach { id: terminal, have }, &[])?;
    let (line, typed) = std::sync::mpsc::channel::<(Ask, Vec<u8>)>();
    let mine = LINES.fetch_add(1, Ordering::Relaxed);
    attached
        .lines
        .lock()
        .unwrap()
        .insert(terminal, (mine, line));
    let mut input = stream.try_clone()?;
    std::thread::spawn(move || {
        for (ask, blob) in typed {
            // A long paste, in pieces the host takes.
            let mut pieces = blob.chunks(16 * 1024);
            let first = pieces.next().unwrap_or(&[]);
            if host::send(&mut input, &ask, first).is_err()
                || pieces.any(|piece| host::send(&mut input, &ask, piece).is_err())
            {
                return;
            }
        }
    });
    let hang_up = stream.try_clone()?;
    jobs.on_cancel(
        id,
        Box::new(move || {
            let _ = hang_up.shutdown(std::net::Shutdown::Both);
        }),
    );
    let mut result = Ok(());
    loop {
        match host::receive::<Tell>(&mut stream) {
            Ok(Some((Tell::Err { message }, _))) => {
                let kind = FaultKind::NotFound;
                result = Err(Fault { kind, message });
                break;
            }
            Ok(Some((tell, blob))) => {
                let last = tell == Tell::Exited;
                let json = serde_json::to_vec(&Item::Term { tell }).expect("plain data");
                if !out.send(frame::ITEM, id, json, blob) || last {
                    break;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    let mut lines = attached.lines.lock().unwrap();
    if lines.get(&terminal).is_some_and(|(line, _)| *line == mine) {
        lines.remove(&terminal);
    }
    result
}

/// Ask this machine's host for the one thing `ask` asks.
#[cfg(unix)]
fn ask_host(ask: Ask) -> Result<Tell, Fault> {
    Ok(super::host::ask(&ask)?)
}

#[cfg(not(unix))]
fn ask_host(_: Ask) -> Result<Tell, Fault> {
    Err(Fault::other("den-server keeps terminals on Linux only."))
}

fn handle(
    id: u32,
    request: Request,
    blob: Vec<u8>,
    out: &Sender,
    jobs: &Jobs,
    attached: &Attached,
) -> Answered {
    let plain = |response| Ok((response, Vec::new()));
    match request {
        Request::Ping => plain(Response::Ok),
        Request::Git { cwd, args, stdin } => {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match git::run(Path::new(&cwd), &args, stdin.as_deref()) {
                Ok(output) => Ok((
                    Response::Git {
                        code: output.code,
                        stderr: output.stderr,
                    },
                    output.stdout.into_bytes(),
                )),
                Err(git::Error::NotFound) => Err(Fault {
                    kind: FaultKind::GitMissing,
                    message: git::Error::NotFound.to_string(),
                }),
                Err(git::Error::Failed(message)) => Err(Fault::other(message)),
            }
        }
        Request::Read { path, max } => {
            let len = std::fs::metadata(&path)?.len();
            if len > max {
                return Err(Fault {
                    kind: FaultKind::TooLarge,
                    message: format!("The file has {len} bytes, more than the {max} asked for."),
                });
            }
            Ok((Response::Ok, std::fs::read(&path)?))
        }
        Request::Write { path } => {
            std::fs::write(&path, blob)?;
            plain(Response::Ok)
        }
        Request::ReadDir { path } => {
            let entries = std::fs::read_dir(&path)?
                .flatten()
                .map(|entry| DirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_dir: entry.file_type().is_ok_and(|kind| kind.is_dir()),
                })
                .collect();
            plain(Response::Dir { entries })
        }
        Request::Stat { path } => {
            let meta = match std::fs::metadata(&path) {
                Ok(meta) => Some(Meta {
                    is_dir: meta.is_dir(),
                    is_file: meta.is_file(),
                    len: meta.len(),
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            plain(Response::Stat { meta })
        }
        Request::CreateFile { path } => {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            plain(Response::Ok)
        }
        Request::CreateDirAll { path } => {
            std::fs::create_dir_all(&path)?;
            plain(Response::Ok)
        }
        Request::Rename { from, to } => {
            std::fs::rename(&from, &to)?;
            plain(Response::Ok)
        }
        Request::Trash { paths } => {
            let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
            trash::delete_all(&paths).map_err(Fault::other)?;
            plain(Response::Ok)
        }
        Request::Canonicalize { path } => {
            let path = std::fs::canonicalize(expand_home(&path))?;
            plain(Response::Path {
                path: path.to_string_lossy().into_owned(),
            })
        }
        Request::Watch { root } => {
            let (watcher, mut changes) = watch::watch(Path::new(&root)).map_err(Fault::other)?;
            // Dropping the watcher closes `changes`, which ends the loop.
            jobs.on_cancel(id, Box::new(move || drop(watcher)));
            while let Some(first) = futures::executor::block_on(changes.next()) {
                std::thread::sleep(WATCH_BATCH);
                let mut paths = vec![first.to_string_lossy().into_owned()];
                while let Ok(path) = changes.try_recv() {
                    paths.push(path.to_string_lossy().into_owned());
                }
                paths.sort();
                paths.dedup();
                send_item(out, id, &Item::Changed { paths });
            }
            plain(Response::Ok)
        }
        Request::Search { root, query } => {
            let cancelled = Arc::new(AtomicBool::new(false));
            jobs.on_cancel(id, {
                let cancelled = cancelled.clone();
                Box::new(move || cancelled.store(true, Ordering::Relaxed))
            });
            let done = search::search(Path::new(&root), &query, &cancelled, |files| {
                send_item(out, id, &Item::Matches { files })
            })
            .map_err(Fault::other)?;
            plain(Response::Searched {
                limit_hit: done.limit_hit,
            })
        }
        Request::Files { root, max } => {
            let files = search::files(Path::new(&root), max);
            Ok((Response::Ok, files.join("\0").into_bytes()))
        }
        #[cfg(unix)]
        Request::TermSpawn {
            id,
            root,
            cwd,
            shell,
            command,
            cols,
            rows,
        } => {
            let (argv, env) = super::host::command_line(id, shell.as_deref(), command.as_deref());
            ask_host(Ask::Spawn {
                id,
                root,
                cwd,
                argv,
                env,
                cols,
                rows,
            })?;
            plain(Response::Ok)
        }
        #[cfg(not(unix))]
        Request::TermSpawn { .. } | Request::TermAttach { .. } => {
            let _ = attached;
            Err(Fault::other("den-server keeps terminals on Linux only."))
        }
        #[cfg(unix)]
        Request::TermAttach { id: terminal, have } => {
            attach(id, terminal, have, out, jobs, attached)?;
            plain(Response::Ok)
        }
        Request::TermKill { id } => {
            ask_host(Ask::Kill { id })?;
            plain(Response::Ok)
        }
        Request::TermList { root } => match ask_host(Ask::List { root: Some(root) })? {
            Tell::Terms { terms } => plain(Response::Terms { terms }),
            _ => Err(Fault::other("The terminals' host gave no list.")),
        },
        // Taken in order where the requests are read, never here.
        Request::TermWrite { .. } | Request::TermResize { .. } | Request::TermAck { .. } => {
            plain(Response::Ok)
        }
    }
}

/// False once den is gone.
fn send_item(out: &Sender, id: u32, item: &Item) -> bool {
    let json = serde_json::to_vec(item).expect("an item is plain data");
    out.send(frame::ITEM, id, json, Vec::new())
}

/// `~` and `~/…` from the home folder of the user den-server runs as.
fn expand_home(path: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    match path {
        "" | "~" => home().unwrap_or_else(|| PathBuf::from("/")),
        _ => match path
            .strip_prefix("~/")
            .and_then(|rest| Some(home()?.join(rest)))
        {
            Some(expanded) => expanded,
            None => PathBuf::from(path),
        },
    }
}
