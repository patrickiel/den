//! Files and folders, where they are: on this machine's disk, or on a
//! server through its den-server (`remote`). What opens, lists, saves or
//! deletes for a folder goes through here, so it works for both.
//!
//! These wait for the disk or the server: call them off the UI thread.

use std::{
    io,
    path::{Path, PathBuf},
};

pub use crate::remote::proto::{DirEntry, Meta};
use crate::remote::{
    self, Connection, Route,
    proto::{Request, Response},
};

/// The connection and the path there for a path on a server; `None` for one
/// on this machine. An error for a server den is not connected to: such a
/// path is never looked for on this machine's disk.
fn there(path: &Path) -> io::Result<Option<(std::sync::Arc<Connection>, String)>> {
    match remote::route(path) {
        Route::Local => Ok(None),
        Route::Remote(connection, posix) => Ok(Some((connection, posix))),
        Route::Offline(host) => Err(remote::Error::Offline(host).into()),
    }
}

fn unexpected() -> io::Error {
    io::Error::other("The server answered something else than was asked.")
}

/// A file's bytes. `FileTooLarge` when it has more than `max`.
pub fn read(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let Some((connection, path)) = there(path)? else {
        let len = std::fs::metadata(path)?.len();
        if len > max {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("The file has {len} bytes, more than the {max} asked for."),
            ));
        }
        return std::fs::read(path);
    };
    let (_, bytes) = connection.call(&Request::Read { path, max }, Vec::new())?;
    Ok(bytes)
}

/// Make `bytes` the file's contents; the file is new when there was none.
pub fn write(path: &Path, bytes: Vec<u8>) -> io::Result<()> {
    let Some((connection, path)) = there(path)? else {
        return std::fs::write(path, bytes);
    };
    connection.call(&Request::Write { path }, bytes)?;
    Ok(())
}

/// A folder's entries, in no order.
pub fn read_dir(path: &Path) -> io::Result<Vec<DirEntry>> {
    let Some((connection, path)) = there(path)? else {
        return Ok(std::fs::read_dir(path)?
            .flatten()
            .map(|entry| DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: entry.file_type().is_ok_and(|kind| kind.is_dir()),
            })
            .collect());
    };
    match connection.call(&Request::ReadDir { path }, Vec::new())? {
        (Response::Dir { entries }, _) => Ok(entries),
        _ => Err(unexpected()),
    }
}

/// What is at `path`: `Ok(None)` for nothing. An error means it could not
/// be told (a server out of reach), which is not the same: what is kept
/// about the path must then be kept.
pub fn stat(path: &Path) -> io::Result<Option<Meta>> {
    let Some((connection, path)) = there(path)? else {
        return match std::fs::metadata(path) {
            Ok(meta) => Ok(Some(Meta {
                is_dir: meta.is_dir(),
                is_file: meta.is_file(),
                len: meta.len(),
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        };
    };
    match connection.call(&Request::Stat { path }, Vec::new())? {
        (Response::Stat { meta }, _) => Ok(meta),
        _ => Err(unexpected()),
    }
}

/// A new, empty file; `AlreadyExists` when there is one.
pub fn create_file(path: &Path) -> io::Result<()> {
    let Some((connection, path)) = there(path)? else {
        return std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(drop);
    };
    connection.call(&Request::CreateFile { path }, Vec::new())?;
    Ok(())
}

pub fn create_dir_all(path: &Path) -> io::Result<()> {
    let Some((connection, path)) = there(path)? else {
        return std::fs::create_dir_all(path);
    };
    connection.call(&Request::CreateDirAll { path }, Vec::new())?;
    Ok(())
}

/// Rename or move; both paths on the same machine.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    match (there(from)?, there(to)?) {
        (None, None) => std::fs::rename(from, to),
        (Some((connection, from)), Some((other, to))) if connection.host() == other.host() => {
            connection.call(&Request::Rename { from, to }, Vec::new())?;
            Ok(())
        }
        _ => Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            "A file cannot be moved from one machine to another.",
        )),
    }
}

/// Move to the trash: this machine's, or for files on a server that of the
/// user den-server runs as there.
pub fn trash(paths: &[PathBuf]) -> Result<(), String> {
    let mut here: Vec<&PathBuf> = Vec::new();
    let mut theirs: Vec<(std::sync::Arc<Connection>, Vec<String>)> = Vec::new();
    for path in paths {
        match there(path).map_err(|e| e.to_string())? {
            None => here.push(path),
            Some((connection, posix)) => {
                match theirs
                    .iter_mut()
                    .find(|(known, _)| known.host() == connection.host())
                {
                    Some((_, list)) => list.push(posix),
                    None => theirs.push((connection, vec![posix])),
                }
            }
        }
    }
    if !here.is_empty() {
        trash::delete_all(here).map_err(|e| e.to_string())?;
    }
    for (connection, paths) in theirs {
        connection
            .call(&Request::Trash { paths }, Vec::new())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The path with links and `..` resolved, as the machine it is on names it.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    let Some((connection, posix)) = there(path)? else {
        return std::fs::canonicalize(path);
    };
    canonicalize_on(&connection, &posix)
}

/// A file's text, wherever the file is; an error for one that is not UTF-8,
/// as `std::fs::read_to_string` gives.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    String::from_utf8(read(path, u64::MAX)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "The file is not UTF-8 text."))
}

/// den's path for what `text` names on a server: `~`, `~/proj`, a relative
/// path from the home folder, or an absolute one.
pub fn canonicalize_on(connection: &Connection, text: &str) -> io::Result<PathBuf> {
    let path = if text.is_empty() || text.starts_with('/') || text.starts_with('~') {
        text.to_string()
    } else {
        format!("~/{text}")
    };
    match connection.call(&Request::Canonicalize { path }, Vec::new())? {
        (Response::Path { path }, _) => Ok(connection.path(&path)),
        _ => Err(unexpected()),
    }
}
