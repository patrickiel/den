//! Terminals on a server, from den's side: started there, kept by the
//! server's host (`remote::host`) whether den is connected or not, and
//! attached to for as long as a tab shows one.

use std::{
    hash::{BuildHasher as _, Hasher as _},
    path::Path,
    sync::{Arc, mpsc as std_mpsc},
    time::Duration,
};

use futures::channel::mpsc;

use super::{
    Connection, Error, Route, Stream,
    proto::{Item, Request, Response, Tell, TermInfo},
    route,
};

/// What an attached terminal says.
#[derive(Debug)]
pub enum Event {
    /// Drop what is shown: output follows from `from`, at this size, old
    /// news up to `live`.
    Reset {
        from: u64,
        live: u64,
        cols: u16,
        rows: u16,
    },
    /// Output follows from where den was.
    Resumed,
    Output {
        offset: u64,
        bytes: Vec<u8>,
    },
    /// Bytes that set the terminal's modes; no part of the output.
    Modes(Vec<u8>),
    Hook {
        agent: String,
        kind: String,
        body: String,
    },
    Exited,
}

/// A name for a new terminal, as good as unique.
pub fn new_id() -> u64 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    // Whole in a JSON number wherever one is read.
    hasher.finish() >> 11 | 1
}

fn connection(at: &Path) -> Result<(Arc<Connection>, String), Error> {
    match route(at) {
        Route::Remote(connection, there) => Ok((connection, there)),
        Route::Offline(host) => Err(Error::Offline(host)),
        Route::Local => Err(Error::Start("The folder is not on a server.".into())),
    }
}

/// Start the terminal `id` in `cwd`, for the folder `root`. Waits for the
/// server.
pub fn spawn(
    root: &Path,
    cwd: &Path,
    id: u64,
    shell: Option<&str>,
    command: Option<&str>,
    (cols, rows): (u16, u16),
) -> Result<(), Error> {
    let (connection, cwd) = connection(cwd)?;
    let root = super::split(root).map(|(_, root)| root).unwrap_or_default();
    let request = Request::TermSpawn {
        id,
        root,
        cwd,
        shell: shell.map(str::to_string),
        command: command.map(str::to_string),
        cols,
        rows,
    };
    connection.call(&request, Vec::new()).map(drop)
}

/// The terminals running on the server for the folder `root`.
pub fn list(root: &Path) -> Result<Vec<TermInfo>, Error> {
    let (connection, root) = connection(root)?;
    match connection.call(&Request::TermList { root }, Vec::new())? {
        (Response::Terms { terms }, _) => Ok(terms),
        _ => Err(Error::Disconnected),
    }
}

/// End the terminal `id` on the server `at` is on.
pub fn kill(at: &Path, id: u64) -> Result<(), Error> {
    let (connection, _) = connection(at)?;
    connection
        .call(&Request::TermKill { id }, Vec::new())
        .map(drop)
}

/// den's hold on a terminal it shows. Dropped, the terminal runs on unseen.
pub struct Session {
    connection: Arc<Connection>,
    id: u64,
    _stream: Stream,
}

/// Attach to the terminal `id`: what it says from now on, after what den
/// missed since the offset `have`. Waits for the server; an error of the
/// kind `NotFound` means the terminal is not running any more. The events
/// end when the terminal does, or the connection.
pub fn attach(
    at: &Path,
    id: u64,
    have: Option<u64>,
) -> Result<(Session, mpsc::UnboundedReceiver<Event>), Error> {
    let (connection, _) = connection(at)?;
    let (tx, rx) = mpsc::unbounded();
    let (first_tx, first_rx) = std_mpsc::channel();
    let mut stream = connection.stream(&Request::TermAttach { id, have }, move |item, blob| {
        let Item::Term { tell } = item else { return };
        let event = match tell {
            Tell::Reset {
                from,
                live,
                cols,
                rows,
            } => Event::Reset {
                from,
                live,
                cols,
                rows,
            },
            Tell::Resumed => Event::Resumed,
            Tell::Output { offset } => Event::Output {
                offset,
                bytes: blob,
            },
            Tell::Modes => Event::Modes(blob),
            Tell::Hook { agent, kind, body } => Event::Hook { agent, kind, body },
            Tell::Exited => Event::Exited,
            _ => return,
        };
        let _ = first_tx.send(());
        let _ = tx.unbounded_send(event);
    })?;
    // Attached once the terminal has said something; refused when the
    // answer comes first.
    loop {
        if let Some(answer) = stream.wait_for(Duration::from_millis(20)) {
            if first_rx.try_recv().is_ok() {
                // Said its all already (a program that had ended).
                break;
            }
            return Err(answer.err().unwrap_or(Error::Disconnected));
        }
        if first_rx.try_recv().is_ok() {
            break;
        }
    }
    let session = Session {
        connection,
        id,
        _stream: stream,
    };
    Ok((session, rx))
}

impl Session {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Type `bytes`. Never waits.
    pub fn write(&self, bytes: &[u8]) {
        let request = Request::TermWrite { id: self.id };
        self.connection.notify(&request, bytes.to_vec());
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let id = self.id;
        let request = Request::TermResize { id, cols, rows };
        self.connection.notify(&request, Vec::new());
    }

    /// The output up to `offset` is shown: the server may send more.
    pub fn ack(&self, offset: u64) {
        let request = Request::TermAck {
            id: self.id,
            offset,
        };
        self.connection.notify(&request, Vec::new());
    }

    /// End the terminal's program. Never waits.
    pub fn kill(&self) {
        let request = Request::TermKill { id: self.id };
        self.connection.notify(&request, Vec::new());
    }
}
