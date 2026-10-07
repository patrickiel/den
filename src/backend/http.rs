//! HTTP for the backend: one client shared by every request (GitHub
//! releases and its API, the extension index, avatars, updates, the AI
//! downloads), small downloads capped and written through a `.part` file,
//! and large ones resumable with a checksum, progress and cancellation.

use std::{
    fs::File,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

/// The client every request goes through; it keeps connections open.
pub static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .redirects(8)
        .build()
});

pub const CANCELLED: &str = "cancelled";

/// Why a request gave nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The server has no such thing (404).
    NotFound,
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => f.write_str("not found"),
            Error::Other(message) => f.write_str(message),
        }
    }
}

/// The body at `url`, up to `limit` bytes.
pub fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>, Error> {
    let response = match AGENT.get(url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(404, _)) => return Err(Error::NotFound),
        Err(e) => return Err(Error::Other(e.to_string())),
    };
    let mut bytes = Vec::new();
    response.into_reader().take(limit).read_to_end(&mut bytes).map_err(|e| Error::Other(e.to_string()))?;
    Ok(bytes)
}

/// Where a download of `dest` grows until it is complete: `<name>.part`
/// next to it.
pub fn part_path(dest: &Path) -> PathBuf {
    dest.with_file_name(format!("{}.part", dest.file_name().unwrap_or_default().to_string_lossy()))
}

/// Write `bytes` to `path` through its `.part` file, so a reader never sees
/// a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let part = part_path(path);
    std::fs::write(&part, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(part, path).map_err(|e| e.to_string())
}

/// Fetch `url` (up to `limit` bytes; nothing at all is an error) into the
/// file `dest`.
pub fn fetch_to_file(url: &str, dest: &Path, limit: u64) -> Result<(), Error> {
    let bytes = get_bytes(url, limit)?;
    if bytes.is_empty() {
        return Err(Error::Other("empty".into()));
    }
    write_atomic(dest, &bytes).map_err(Error::Other)
}

/// Download progress: what, bytes done, bytes in all (0 when unknown).
pub type Progress<'a> = &'a mut dyn FnMut(&'static str, u64, u64);

/// Stream `url` into `dest` through `dest.part`, resuming a partial download.
pub(crate) fn download(url: &str, dest: &Path, sha256: Option<&str>, stage: &'static str, cancel: &Cancel, progress: Progress) -> Result<(), String> {
    if dest.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(dest.parent().ok_or("bad download path")?).map_err(|e| e.to_string())?;
    let part = part_path(dest);
    let mut offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut request = AGENT.get(url);
    if offset > 0 {
        request = request.set("Range", &format!("bytes={offset}-"));
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(416, _)) => {
            // The partial file is complete or stale: start over.
            let _ = std::fs::remove_file(&part);
            offset = 0;
            AGENT.get(url).call().map_err(|e| format!("Download failed: {e}"))?
        }
        Err(e) => return Err(format!("Download failed: {e}")),
    };
    if response.status() != 206 {
        offset = 0; // the server ignored the range
    }
    let len: u64 = response.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let total = if len > 0 { len + offset } else { 0 };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .open(&part)
        .map_err(|e| e.to_string())?;
    let mut reader = response.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = offset;
    let mut last = Instant::now() - Duration::from_secs(1);
    loop {
        cancel.check()?;
        let n = reader.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(200) {
            last = Instant::now();
            progress(stage, done, total);
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    if total > 0 && done < total {
        return Err("Download interrupted; try again to resume.".into());
    }
    if let Some(want) = sha256 {
        progress("Checking", 0, 0);
        let got = sha256_file(&part, cancel)?;
        if !got.eq_ignore_ascii_case(want) {
            let _ = std::fs::remove_file(&part);
            return Err(format!("The downloaded file is corrupt (checksum mismatch): {url}"));
        }
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())
}

fn sha256_file(path: &Path, cancel: &Cancel) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        cancel.check()?;
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Stops a download or a request: polled between steps, and the socket of a
/// request is shut so a read blocked on prompt processing returns at once.
#[derive(Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    pub(crate) stream: Arc<Mutex<Option<TcpStream>>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(stream) = self.stream.lock().ok().and_then(|s| s.as_ref().and_then(|s| s.try_clone().ok())) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() { Err(CANCELLED.into()) } else { Ok(()) }
    }
}

