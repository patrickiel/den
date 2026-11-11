//! den's end of the pipe to a den-server: a program (ssh, or den-server
//! itself in tests) whose input takes den's frames and whose output gives
//! the server's.
//!
//! A call waits for its answer on the thread that made it, as the work on a
//! local folder waits for the disk. One thread writes, one reads and hands
//! each answer to the call that waits for it; when the pipe breaks, every
//! call under way ends with `Disconnected` and no new one starts.

use std::{
    collections::HashMap,
    io::{BufRead as _, BufReader, Read as _},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, Weak,
        mpsc::{self, RecvTimeoutError},
    },
    time::Duration,
};

use super::{
    Error, HostId,
    frame::{self, Assembler},
    proto::{Item, MAGIC, Reply, Request, Response, VERSION},
};
use crate::process;

/// An answer and its blob.
pub type Answer = Result<(Response, Vec<u8>), Error>;
type OnItem = Box<dyn FnMut(Item, Vec<u8>) + Send>;

/// How patient a connection is.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// How long the server may take to say it is there (ssh connecting,
    /// the login shell starting).
    pub start: Duration,
    /// How often den asks whether the server is still there.
    pub ping: Duration,
    /// How long that answer may take before the connection counts as lost:
    /// a laptop that slept, a network that went without a word.
    pub patience: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            start: Duration::from_secs(30),
            ping: Duration::from_secs(10),
            patience: Duration::from_secs(20),
        }
    }
}

pub struct Connection {
    host: HostId,
    out: frame::Sender,
    shared: Arc<Shared>,
    child: Mutex<Child>,
}

struct Shared {
    state: Mutex<State>,
    /// The last of what the program wrote to its standard error.
    stderr: Mutex<String>,
    on_close: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// No den-server answered: what was said instead (ssh's own words, mostly)
/// and the exit code of the program, when it ended by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub message: String,
    pub code: Option<i32>,
}

#[derive(Default)]
struct State {
    last_id: u32,
    waiting: HashMap<u32, Waiting>,
    dead: bool,
}

struct Waiting {
    answer: mpsc::Sender<Answer>,
    on_item: Option<OnItem>,
}

impl Connection {
    /// Start `command` and wait for the den-server behind it to say it is
    /// there. `host` is the server its paths are on.
    pub fn start(host: HostId, command: Command, options: Options) -> Result<Arc<Self>, Error> {
        Self::try_start(host, command, options).map_err(|refused| Error::Start(refused.message))
    }

    /// `start`, telling how the program ended when no den-server answered:
    /// from that the caller knows whether there is one to start at all.
    pub fn try_start(
        host: HostId,
        mut command: Command,
        options: Options,
    ) -> Result<Arc<Self>, Refused> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process::no_window(&mut command);
        let mut child = command.spawn().map_err(|e| Refused {
            message: format!("Could not start {:?}: {e}", command.get_program()),
            code: None,
        })?;
        let (stdin, stdout, stderr) = (
            child.stdin.take().expect("piped"),
            child.stdout.take().expect("piped"),
            child.stderr.take().expect("piped"),
        );
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            stderr: Mutex::new(String::new()),
            on_close: Mutex::new(None),
        });
        spawn("den-remote-stderr", {
            let shared = shared.clone();
            move || keep_stderr(stderr, &shared)
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        spawn("den-remote-reader", {
            let shared = shared.clone();
            move || {
                let mut input = BufReader::new(stdout);
                let greeting = greeting(&mut input);
                let greeted = greeting.is_ok();
                let _ = ready_tx.send(greeting);
                if greeted {
                    read_all(&mut input, &shared);
                }
                shared.die();
            }
        });
        let greeting = match ready_rx.recv_timeout(options.start) {
            Ok(greeting) => greeting,
            Err(_) => Err("The server did not answer in time.".to_string()),
        };
        if let Err(message) = greeting {
            // It ended by itself, mostly: how tells why.
            let mut code = None;
            for _ in 0..20 {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        code = status.code();
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                    Err(_) => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            // What ssh said (no route, a refused key) tells more.
            std::thread::sleep(Duration::from_millis(50));
            let said = shared.stderr.lock().unwrap().trim().to_string();
            return Err(Refused {
                message: if said.is_empty() { message } else { said },
                code,
            });
        }
        let (out, _) = frame::writer(stdin);
        let connection = Arc::new(Connection {
            host,
            out,
            shared,
            child: Mutex::new(child),
        });
        spawn("den-remote-ping", {
            let connection = Arc::downgrade(&connection);
            move || keep_asking(connection, options)
        });
        Ok(connection)
    }

    pub fn host(&self) -> &HostId {
        &self.host
    }

    /// False once the pipe broke; it does not come back.
    pub fn alive(&self) -> bool {
        !self.shared.state.lock().unwrap().dead
    }

    /// den's path for `posix` on this server.
    pub fn path(&self, posix: &str) -> PathBuf {
        super::join(&self.host, posix)
    }

    /// Run `then` once when the connection ends, on whatever thread notices
    /// (at once when it has ended already).
    pub fn on_close(&self, then: impl FnOnce() + Send + 'static) {
        let dead = self.shared.state.lock().unwrap().dead;
        if dead {
            then();
        } else {
            *self.shared.on_close.lock().unwrap() = Some(Box::new(then));
            // It may have ended in between.
            if !self.alive()
                && let Some(then) = self.shared.on_close.lock().unwrap().take()
            {
                then();
            }
        }
    }

    /// What the program behind the pipe last wrote to its standard error.
    pub fn stderr(&self) -> String {
        self.shared.stderr.lock().unwrap().clone()
    }

    /// Ask and wait for the answer, however long it takes (a `git pull`
    /// takes what it takes); a connection that goes ends the wait.
    pub fn call(&self, request: &Request, blob: Vec<u8>) -> Answer {
        let (_, answer) = self.send(request, blob, None)?;
        answer.recv().unwrap_or(Err(Error::Disconnected))
    }

    /// Say something that wants no answer. Never waits: safe from the UI
    /// thread. False when the connection is gone.
    pub fn notify(&self, request: &Request, blob: Vec<u8>) -> bool {
        let json = serde_json::to_vec(request).expect("a request is plain data");
        self.out.send(frame::REQUEST, 0, json, blob)
    }

    /// Like `call`, giving up after `patience`.
    pub fn call_within(&self, request: &Request, blob: Vec<u8>, patience: Duration) -> Answer {
        let (id, answer) = self.send(request, blob, None)?;
        match answer.recv_timeout(patience) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Disconnected) => Err(Error::Disconnected),
            Err(RecvTimeoutError::Timeout) => {
                self.forget(id);
                Err(Error::Timeout)
            }
        }
    }

    /// Ask for something that sends items before its answer. `on_item` runs
    /// on the reading thread: it must be quick and must not call the
    /// connection. Dropping the `Stream` before the answer cancels the call.
    pub fn stream(
        self: &Arc<Self>,
        request: &Request,
        on_item: impl FnMut(Item, Vec<u8>) + Send + 'static,
    ) -> Result<Stream, Error> {
        let (id, answer) = self.send(request, Vec::new(), Some(Box::new(on_item)))?;
        Ok(Stream {
            connection: self.clone(),
            id,
            answer,
            over: false,
        })
    }

    fn send(
        &self,
        request: &Request,
        blob: Vec<u8>,
        on_item: Option<OnItem>,
    ) -> Result<(u32, mpsc::Receiver<Answer>), Error> {
        let json = serde_json::to_vec(request).expect("a request is plain data");
        let (tx, rx) = mpsc::channel();
        let id = {
            let mut state = self.shared.state.lock().unwrap();
            if state.dead {
                return Err(Error::Disconnected);
            }
            // 0 is for requests that want no answer.
            state.last_id = state.last_id.checked_add(1).unwrap_or(1);
            let id = state.last_id;
            state.waiting.insert(
                id,
                Waiting {
                    answer: tx,
                    on_item,
                },
            );
            id
        };
        if !self.out.send(frame::REQUEST, id, json, blob) {
            self.forget(id);
            return Err(Error::Disconnected);
        }
        Ok((id, rx))
    }

    fn forget(&self, id: u32) {
        self.shared.state.lock().unwrap().waiting.remove(&id);
    }

    /// End the connection now: the program is killed, calls under way end
    /// with `Disconnected`.
    pub fn close(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        self.shared.die();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

impl Shared {
    /// No more answers will come: end every wait.
    fn die(&self) {
        let waiting = {
            let mut state = self.state.lock().unwrap();
            state.dead = true;
            std::mem::take(&mut state.waiting)
        };
        // Dropped outside the lock: a callback's own drop may do anything.
        drop(waiting);
        let then = self.on_close.lock().unwrap().take();
        if let Some(then) = then {
            then();
        }
    }
}

/// A call that sends items; see `Connection::stream`.
pub struct Stream {
    connection: Arc<Connection>,
    id: u32,
    answer: mpsc::Receiver<Answer>,
    over: bool,
}

impl Stream {
    /// Wait for the answer that ends the call.
    pub fn wait(mut self) -> Answer {
        self.over = true;
        self.answer.recv().unwrap_or(Err(Error::Disconnected))
    }

    /// The answer, when it comes within `patience`.
    pub fn wait_for(&mut self, patience: Duration) -> Option<Answer> {
        let answer = match self.answer.recv_timeout(patience) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => Err(Error::Disconnected),
        };
        self.over = true;
        Some(answer)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if !self.over {
            self.connection.forget(self.id);
            self.connection
                .out
                .send(frame::CANCEL, self.id, Vec::new(), Vec::new());
        }
    }
}

fn spawn(name: &str, run: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(run)
        .expect("a thread");
}

/// Read up to the line den-server starts with, past whatever a login shell
/// printed before it.
fn greeting(input: &mut impl std::io::BufRead) -> Result<(), String> {
    const MOST_NOISE: usize = 256 * 1024;
    let mut seen = 0;
    loop {
        let mut line = Vec::new();
        let read = input
            .by_ref()
            .take(MOST_NOISE as u64)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("The server closed the connection before den-server started.".into());
        }
        seen += read;
        let line = String::from_utf8_lossy(&line);
        if let Some(version) = line.trim_end().strip_prefix(MAGIC) {
            return if version == VERSION.to_string() {
                Ok(())
            } else {
                Err(format!(
                    "The den-server there speaks version {version}, this den version {VERSION}."
                ))
            };
        }
        if seen > MOST_NOISE {
            return Err("What answered is not den-server.".into());
        }
    }
}

fn read_all(input: &mut impl std::io::Read, shared: &Shared) {
    let mut assembler = Assembler::default();
    while let Ok(Some(frame)) = frame::read_frame(input) {
        let Some(message) = assembler.push(frame) else {
            continue;
        };
        match message.kind {
            frame::RESPONSE => {
                let waiting = shared.state.lock().unwrap().waiting.remove(&message.id);
                let Some(waiting) = waiting else { continue };
                let answer = match serde_json::from_slice::<Reply>(&message.json) {
                    Ok(Reply::Ok(response)) => Ok((response, message.blob)),
                    Ok(Reply::Err(fault)) => Err(Error::Failed(fault)),
                    Err(e) => Err(Error::Failed(super::Fault::other(format!(
                        "The server's answer was not understood: {e}"
                    )))),
                };
                let _ = waiting.answer.send(answer);
            }
            frame::ITEM => {
                let Ok(item) = serde_json::from_slice::<Item>(&message.json) else {
                    continue;
                };
                let mut state = shared.state.lock().unwrap();
                if let Some(on_item) = state
                    .waiting
                    .get_mut(&message.id)
                    .and_then(|waiting| waiting.on_item.as_mut())
                {
                    on_item(item, message.blob);
                }
            }
            _ => {}
        }
    }
}

fn keep_stderr(mut stderr: impl std::io::Read, shared: &Shared) {
    const KEPT: usize = 4 * 1024;
    let mut buffer = [0u8; 1024];
    while let Ok(read) = stderr.read(&mut buffer) {
        if read == 0 {
            break;
        }
        let mut kept = shared.stderr.lock().unwrap();
        kept.push_str(&String::from_utf8_lossy(&buffer[..read]));
        if kept.len() > KEPT {
            let mut cut = kept.len() - KEPT;
            while !kept.is_char_boundary(cut) {
                cut += 1;
            }
            kept.drain(..cut);
        }
    }
}

/// Ask now and then whether the server is there, until the connection is
/// dropped or it is not.
fn keep_asking(connection: Weak<Connection>, options: Options) {
    loop {
        std::thread::sleep(options.ping);
        let Some(connection) = connection.upgrade() else {
            return;
        };
        if !connection.alive() {
            return;
        }
        if connection
            .call_within(&Request::Ping, Vec::new(), options.patience)
            .is_err()
        {
            connection.close();
            return;
        }
    }
}
