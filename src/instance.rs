//! One den per folder. A den started on a folder another den has open hands
//! the folder to that den, which brings its window to the front, and does
//! not open a window of its own.
//!
//! Each den listens on an address of its own while it runs (a Unix socket
//! under the data folder, a named pipe on Windows), and notes that address
//! beside each folder's lock (see `settings::FolderLock`). A den starting on
//! a locked folder reads the address there, sends the folder, and waits for
//! the answer. Without one (the other den is gone, or does not answer) it
//! opens the folder itself, as it would have without this.

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use futures::StreamExt as _;
use gpui_kit::*;

/// How long a den waits for another to take a folder. (A named pipe's
/// client cannot be timed; its den answers or cuts the pipe.)
#[cfg(not(windows))]
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// The answer a den gives once it has the folder.
const TAKEN: &str = "ok";

/// This den's address, for the other dens.
pub fn address() -> String {
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\den-{}", std::process::id())
    }
    #[cfg(not(windows))]
    {
        socket_dir()
            .join(format!("{}.sock", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(not(windows))]
fn socket_dir() -> PathBuf {
    crate::settings::data_dir().join("open")
}

/// The roots among `roots` no other den has open: the others are handed to
/// their den, which shows them. Run before the app starts, so a den with
/// nothing left to open opens no window at all.
pub fn hand_off(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots
        .into_iter()
        .filter(|root| {
            let Some(address) = crate::settings::FolderLock::holder(root) else {
                return true;
            };
            !send(&address, root)
        })
        .collect()
}

/// Ask the den at `address` to show `root`; true once it has.
fn send(address: &str, root: &Path) -> bool {
    let Some(stream) = connect(address) else {
        return false;
    };
    let mut stream = stream;
    let mut line = root.to_string_lossy().into_owned();
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).is_ok() && answer.trim_end() == TAKEN
}

#[cfg(not(windows))]
fn connect(address: &str) -> Option<std::os::unix::net::UnixStream> {
    let stream = std::os::unix::net::UnixStream::connect(address).ok()?;
    stream.set_read_timeout(Some(PATIENCE)).ok()?;
    stream.set_write_timeout(Some(PATIENCE)).ok()?;
    Some(stream)
}

#[cfg(windows)]
fn connect(address: &str) -> Option<std::fs::File> {
    // A pipe no den listens on is simply not there; one whose den is busy
    // with another client refuses, and a window opens here as before.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(address)
        .ok()
}

/// Listen for the folders other dens hand this one, and show each: the
/// window open on it comes to the front; a folder no window has (the lock
/// changed hands meanwhile) opens in a new one.
pub fn serve(cx: &mut App) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<PathBuf>();
    let Some(listener) = Listener::bind() else {
        return;
    };
    std::thread::Builder::new()
        .name("den-instance".into())
        .spawn(move || {
            while let Some(mut client) = listener.accept() {
                let mut line = String::new();
                let read = BufReader::new(&mut client).read_line(&mut line);
                if read.is_err() || line.trim().is_empty() {
                    continue;
                }
                let root = PathBuf::from(line.trim_end_matches(['\r', '\n']));
                if tx.unbounded_send(root).is_err() {
                    break;
                }
                // Answered as soon as this den has it: the other den is not
                // held up by whatever the window is doing.
                let _ = client
                    .write_all(TAKEN.as_bytes())
                    .and_then(|()| client.write_all(b"\n"));
                client.finish();
            }
        })
        .ok();
    cx.spawn(async move |cx| {
        while let Some(root) = rx.next().await {
            _ = cx.update(|cx| crate::open_workspace(root, cx));
        }
    })
    .detach();
    #[cfg(not(windows))]
    cx.on_app_quit(|_| {
        let _ = std::fs::remove_file(address());
        async {}
    })
    .detach();
}

/// This den's end of the line.
struct Listener {
    #[cfg(not(windows))]
    inner: std::os::unix::net::UnixListener,
    #[cfg(windows)]
    name: Vec<u16>,
}

#[cfg(not(windows))]
impl Listener {
    fn bind() -> Option<Self> {
        let path = PathBuf::from(address());
        let dir = path.parent()?;
        std::fs::create_dir_all(dir).ok()?;
        // The sockets of dens gone without quitting (killed, crashed) stay
        // behind; nothing answers on them. A live den's reads an empty
        // line from this and goes on.
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let stale = entry.path();
                if stale.extension().is_some_and(|ext| ext == "sock")
                    && std::os::unix::net::UnixStream::connect(&stale).is_err()
                {
                    let _ = std::fs::remove_file(&stale);
                }
            }
        }
        // Left by a den with this pid that crashed.
        let _ = std::fs::remove_file(&path);
        let inner = std::os::unix::net::UnixListener::bind(&path).ok()?;
        Some(Self { inner })
    }

    fn accept(&self) -> Option<Client> {
        let (stream, _) = self.inner.accept().ok()?;
        let _ = stream.set_read_timeout(Some(PATIENCE));
        let _ = stream.set_write_timeout(Some(PATIENCE));
        Some(Client(stream))
    }
}

#[cfg(not(windows))]
struct Client(std::os::unix::net::UnixStream);

#[cfg(not(windows))]
impl Client {
    fn finish(self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(not(windows))]
impl std::io::Read for Client {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.0, buf)
    }
}

#[cfg(not(windows))]
impl Write for Client {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(windows)]
impl Listener {
    fn bind() -> Option<Self> {
        let name: Vec<u16> = address().encode_utf16().chain([0]).collect();
        Some(Self { name })
    }

    /// A pipe instance for the next client; each is one exchange long.
    fn accept(&self) -> Option<Client> {
        use std::os::windows::io::FromRawHandle as _;
        use windows::Win32::{
            Foundation::{ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE},
            Storage::FileSystem::PIPE_ACCESS_DUPLEX,
            System::Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
                PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
            },
        };
        use windows::core::{HRESULT, PCWSTR};
        // SAFETY: the name is a NUL-terminated UTF-16 string that outlives
        // the call; the handle is owned by the File below from here on.
        let file = unsafe {
            let handle = CreateNamedPipeW(
                PCWSTR(self.name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                None,
            );
            if handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let file = std::fs::File::from_raw_handle(handle.0);
            // A client that opened the pipe before this waits counts as connected.
            if let Err(err) = ConnectNamedPipe(handle, None)
                && err.code() != HRESULT::from_win32(ERROR_PIPE_CONNECTED.0)
            {
                return None;
            }
            file
        };
        Some(Client(file))
    }
}

#[cfg(windows)]
struct Client(std::fs::File);

#[cfg(windows)]
impl Client {
    fn finish(self) {
        use std::os::windows::io::AsRawHandle as _;
        use windows::Win32::{Foundation::HANDLE, System::Pipes::DisconnectNamedPipe};
        // What was written reaches the client before the pipe is cut.
        let _ = self.0.sync_all();
        // SAFETY: the handle is this pipe instance's, open until the File drops.
        let _ = unsafe { DisconnectNamedPipe(HANDLE(self.0.as_raw_handle())) };
    }
}

#[cfg(windows)]
impl std::io::Read for Client {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.0, buf)
    }
}

#[cfg(windows)]
impl Write for Client {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
