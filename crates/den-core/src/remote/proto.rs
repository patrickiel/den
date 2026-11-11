//! What den asks den-server and what it answers, as the JSON of a frame.
//! Paths are the server's own (`/home/me/proj`); den turns them into its
//! paths for the folder and back (`remote::join`, `remote::split`).

use serde::{Deserialize, Serialize};

use crate::search::{FileMatch, Query};

/// The line den-server prints before its first frame, with `VERSION` after
/// it; what a login shell prints before it is skipped.
pub const MAGIC: &str = "DEN-SERVER ";
/// den and the den-server it installs are built together and speak the same
/// version; another one is a den-server left over, to be replaced.
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// `git <args>` in `cwd`. Answer: `Git`, git's standard output as the
    /// blob.
    Git {
        cwd: String,
        args: Vec<String>,
        stdin: Option<String>,
    },
    /// A file's bytes as the blob, when it has no more than `max`.
    Read {
        path: String,
        max: u64,
    },
    /// The blob becomes the file.
    Write {
        path: String,
    },
    ReadDir {
        path: String,
    },
    Stat {
        path: String,
    },
    /// A new, empty file; fails when there is one.
    CreateFile {
        path: String,
    },
    CreateDirAll {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    /// To the trash of the user den-server runs as.
    Trash {
        paths: Vec<String>,
    },
    /// The path with `~`, links and `..` resolved. Answer: `Path`.
    Canonicalize {
        path: String,
    },
    /// Changes under `root`, as `Item::Changed`, until cancelled.
    Watch {
        root: String,
    },
    /// A search, its results as `Item::Matches`. Answer: `Searched`.
    Search {
        root: String,
        query: Query,
    },
    /// The files a search would look at, relative to `root` and separated by
    /// NUL in the blob.
    Files {
        root: String,
        max: usize,
    },
    /// Start a terminal in `cwd`, for the folder `root`: the user's shell
    /// (or `shell`), running `command` first. den picks `id`. It runs on,
    /// den here or not, until its program ends or `TermKill`.
    TermSpawn {
        id: u64,
        root: String,
        cwd: String,
        shell: Option<String>,
        command: Option<String>,
        cols: u16,
        rows: u16,
    },
    /// The terminal's output from now on, as `Item::Term`, after what den
    /// missed: from the offset `have` when the server still has that much,
    /// else its last screens. Cancelled, the terminal runs on unseen.
    TermAttach {
        id: u64,
        have: Option<u64>,
    },
    /// The blob is typed into the terminal den is attached to. No answer.
    TermWrite {
        id: u64,
    },
    /// No answer.
    TermResize {
        id: u64,
        cols: u16,
        rows: u16,
    },
    /// den has shown the output up to `offset`: more may come. No answer.
    TermAck {
        id: u64,
        offset: u64,
    },
    TermKill {
        id: u64,
    },
    /// The terminals running for `root`. Answer: `Terms`.
    TermList {
        root: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "re", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Git { code: i32, stderr: String },
    Dir { entries: Vec<DirEntry> },
    Stat { meta: Option<Meta> },
    Path { path: String },
    Searched { limit_hit: bool },
    Terms { terms: Vec<TermInfo> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum Item {
    Changed {
        paths: Vec<String>,
    },
    Matches {
        files: Vec<FileMatch>,
    },
    /// What the terminal den is attached to says, bytes in the blob.
    Term {
        tell: Tell,
    },
}

/// A terminal that runs on the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermInfo {
    pub id: u64,
    pub root: String,
    pub cwd: String,
}

/// What den-server asks the host, the program that keeps a server's
/// terminals running between den's visits (`remote::host`). A host outlives
/// the den-server that started it, and den's updates: what is here stays as
/// it is, and what is added must be something an older host can refuse.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Ask {
    /// Run `argv` in a terminal, with `env` and nothing else as its
    /// environment.
    Spawn {
        id: u64,
        root: String,
        cwd: String,
        argv: Vec<String>,
        env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
    },
    /// The asker becomes the one the terminal talks to (`Tell`s follow, and
    /// `Write`, `Resize` and `Ack` are taken from it) until it hangs up.
    Attach {
        id: u64,
        have: Option<u64>,
    },
    /// The blob, typed.
    Write,
    Resize {
        cols: u16,
        rows: u16,
    },
    Ack {
        offset: u64,
    },
    Kill {
        id: u64,
    },
    List {
        root: Option<String>,
    },
    /// An agent in the terminal `id` reports (`den-server hook`).
    Hook {
        id: u64,
        agent: String,
        kind: String,
        body: String,
    },
}

/// What the host says back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Tell {
    Ok,
    Err {
        message: String,
    },
    /// What the asker has shown of this terminal is to be dropped: output
    /// follows from the offset `from`, at the terminal's size, and is old
    /// news up to `live`.
    Reset {
        from: u64,
        live: u64,
        cols: u16,
        rows: u16,
    },
    /// The asker missed nothing it cannot have: output follows from the
    /// offset it has.
    Resumed,
    /// Output, in the blob; `offset` is that of its first byte.
    Output {
        offset: u64,
    },
    /// The blob sets the terminal's modes (bracketed paste, the mouse, the
    /// other screen) as the program left them; no part of the output.
    Modes,
    Hook {
        agent: String,
        kind: String,
        body: String,
    },
    /// The program ended; nothing follows.
    Exited,
    Terms {
        terms: Vec<TermInfo>,
    },
    /// Something a newer host says.
    #[serde(other)]
    Unknown,
}

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// What `Stat` tells of a path, links followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub is_dir: bool,
    pub is_file: bool,
    pub len: u64,
}

/// The JSON of a `RESPONSE` frame.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok(Response),
    Err(Fault),
}

/// Why the server could not do what was asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fault {
    pub kind: FaultKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    /// A file larger than the `max` asked for.
    TooLarge,
    /// No `git` on the server's PATH.
    GitMissing,
    /// A request this den-server does not know.
    Invalid,
    #[serde(other)]
    Other,
}

impl Fault {
    pub fn other(message: impl std::fmt::Display) -> Self {
        Fault {
            kind: FaultKind::Other,
            message: message.to_string(),
        }
    }
}

impl From<std::io::Error> for Fault {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        let kind = match error.kind() {
            ErrorKind::NotFound => FaultKind::NotFound,
            ErrorKind::PermissionDenied => FaultKind::PermissionDenied,
            ErrorKind::AlreadyExists => FaultKind::AlreadyExists,
            ErrorKind::FileTooLarge => FaultKind::TooLarge,
            _ => FaultKind::Other,
        };
        Fault {
            kind,
            message: error.to_string(),
        }
    }
}

impl From<Fault> for std::io::Error {
    fn from(fault: Fault) -> Self {
        use std::io::ErrorKind;
        let kind = match fault.kind {
            FaultKind::NotFound => ErrorKind::NotFound,
            FaultKind::PermissionDenied => ErrorKind::PermissionDenied,
            FaultKind::AlreadyExists => ErrorKind::AlreadyExists,
            FaultKind::TooLarge => ErrorKind::FileTooLarge,
            FaultKind::GitMissing | FaultKind::Invalid | FaultKind::Other => ErrorKind::Other,
        };
        std::io::Error::new(kind, fault.message)
    }
}
