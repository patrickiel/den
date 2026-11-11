//! den and den-server over a real pipe: the built `den-server` started as a
//! child, as ssh starts it on a server, and den-core's own functions asked
//! the same thing of a folder here and of the same folder "on the server".
//! What they answer must be the same, but for the paths' prefix.

#![cfg(unix)]

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use den_core::{
    fs, git,
    remote::{self, Connection, Error, HostId, Options, Route, proto::Request},
    search, watch,
};

/// A den-server on this machine, a folder of its own to work in, and the
/// connection to it.
struct Server {
    host: HostId,
    dir: PathBuf,
    connection: Arc<Connection>,
}

impl Server {
    fn start(name: &str) -> Self {
        Self::start_with(name, "exec", Options::default())
    }

    /// `before` is what the shell runs in front of den-server's command
    /// line (`exec`, or something that prints first).
    fn start_with(name: &str, before: &str, options: Options) -> Self {
        let dir = scratch(name);
        let host = HostId::new(&format!("test-{name}-{}", std::process::id())).unwrap();
        let connection = Connection::start(host.clone(), command(&dir, before), options).unwrap();
        remote::register(connection.clone());
        Server {
            host,
            dir,
            connection,
        }
    }

    /// den's path for a path of this machine, as on the server.
    fn there(&self, path: &Path) -> PathBuf {
        remote::join(&self.host, &path.to_string_lossy())
    }

    /// The server's process id, written by `command` before it started.
    fn pid(&self) -> String {
        std::fs::read_to_string(self.dir.join("pid"))
            .unwrap()
            .trim()
            .to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        remote::unregister(&self.host);
        self.connection.close();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("den-remote-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    // The name the system gives it (a temp folder may be behind a link).
    std::fs::canonicalize(dir).unwrap()
}

fn command(dir: &Path, before: &str) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(format!(
            "echo $$ > \"$DEN_TEST_DIR/pid\"; {before} \"$DEN_TEST_SERVER\" connect --home \"$DEN_TEST_DIR/server\""
        ))
        .env("DEN_TEST_DIR", dir)
        .env("DEN_TEST_SERVER", env!("CARGO_BIN_EXE_den-server"))
        .env("DEN_SERVER_LOGIN_PATH", "0")
        // Its home and its trash are the test's own.
        .env("HOME", dir.join("home"))
        .env("XDG_DATA_HOME", dir.join("home/.local/share"));
    cmd
}

fn sh(dir: &Path, script: &str) {
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "{script}");
}

/// A repository with one commit, one changed file and one new one.
fn repository(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    sh(
        &repo,
        "git init -q -b main && git config user.name Test && git config user.email t@example.com \
         && git config commit.gpgsign false \
         && printf 'one\\ntwo\\n' > a.txt && printf 'fn main() {}\\n' > src/main.rs \
         && git add -A && git commit -q -m first \
         && printf 'one\\ntwo\\nthree\\n' > a.txt && printf 'new\\n' > src/new.rs",
    );
    repo
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "waited too long for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn files_read_and_written_on_the_server_are_the_files() {
    let server = Server::start("files");
    let (here, there) = (
        server.dir.join("work"),
        server.there(&server.dir.join("work")),
    );
    fs::create_dir_all(&there.join("deep/er")).unwrap();
    assert!(here.join("deep/er").is_dir());

    fs::write(&there.join("a.txt"), b"hello".to_vec()).unwrap();
    assert_eq!(std::fs::read(here.join("a.txt")).unwrap(), b"hello");
    assert_eq!(fs::read(&there.join("a.txt"), 100).unwrap(), b"hello");
    // The same errors as on this machine's disk.
    assert_eq!(
        fs::read(&there.join("a.txt"), 4).unwrap_err().kind(),
        fs::read(&here.join("a.txt"), 4).unwrap_err().kind()
    );
    assert_eq!(
        fs::read(&there.join("none"), 100).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(
        fs::read(&there.join("none"), 100).unwrap_err().to_string(),
        fs::read(&here.join("none"), 100).unwrap_err().to_string()
    );

    fs::create_file(&there.join("b.txt")).unwrap();
    assert_eq!(
        fs::create_file(&there.join("b.txt")).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    fs::rename(&there.join("b.txt"), &there.join("deep/c.txt")).unwrap();
    assert!(here.join("deep/c.txt").is_file() && !here.join("b.txt").exists());
    // Not from one machine to the other.
    assert!(fs::rename(&there.join("a.txt"), &here.join("x.txt")).is_err());
    assert!(here.join("a.txt").is_file());

    assert_eq!(fs::stat(&there).unwrap(), fs::stat(&here).unwrap());
    assert_eq!(
        fs::stat(&there.join("a.txt")).unwrap(),
        fs::stat(&here.join("a.txt")).unwrap()
    );
    assert_eq!(fs::stat(&there.join("a.txt")).unwrap().unwrap().len, 5);
    assert_eq!(fs::stat(&there.join("none")).unwrap(), None);

    let sorted = |dir: &Path| {
        let mut entries = fs::read_dir(dir).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    };
    assert_eq!(sorted(&there), sorted(&here));
    assert_eq!(sorted(&there).len(), 2);

    // `~` is the home folder of the user den-server runs as.
    let home = server.dir.join("home");
    assert_eq!(
        fs::canonicalize_on(&server.connection, "~").unwrap(),
        server.there(&home)
    );
    std::fs::create_dir_all(home.join("proj")).unwrap();
    assert_eq!(
        fs::canonicalize_on(&server.connection, "proj").unwrap(),
        server.there(&home.join("proj"))
    );
    assert_eq!(
        fs::canonicalize(&there.join("deep/er/../c.txt")).unwrap(),
        there.join("deep/c.txt")
    );
}

#[test]
fn a_large_file_goes_whole_while_small_calls_pass_it() {
    let server = Server::start("large");
    let there = server.there(&server.dir.join("big.bin"));
    let bytes: Vec<u8> = (0..24 * 1024 * 1024u32).map(|i| (i % 253) as u8).collect();
    let asked = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            fs::write(&there, bytes.clone()).unwrap();
            assert!(fs::read(&there, u64::MAX).unwrap() == bytes);
            asked.store(true, Ordering::Relaxed);
        });
        // Asked while the file is on its way, answered without waiting for it.
        let mut answered = 0;
        while !asked.load(Ordering::Relaxed) {
            let patience = Duration::from_secs(2);
            server
                .connection
                .call_within(&Request::Ping, Vec::new(), patience)
                .unwrap();
            answered += 1;
        }
        assert!(answered > 0);
    });
    assert!(std::fs::read(server.dir.join("big.bin")).unwrap() == bytes);
}

#[test]
fn git_answers_the_same_on_the_server() {
    let server = Server::start("git");
    let here = repository(&server.dir);
    let there = server.there(&here);
    if git::run(&here, &["--version"], None).is_err() {
        return; // no git here
    }

    assert_eq!(
        git::toplevel(&here.join("src")).unwrap(),
        Some(here.clone())
    );
    assert_eq!(
        git::toplevel(&there.join("src")).unwrap(),
        Some(there.clone())
    );
    assert_eq!(git::toplevel(&server.there(&server.dir)).unwrap(), None);

    // The same status, its paths on the server.
    let (local, mut remote) = (git::status(&here).unwrap(), git::status(&there).unwrap());
    assert_eq!(local.files.len(), 2);
    for file in &mut remote.files {
        assert_eq!(file.path, there.join(&file.rel));
        file.path = here.join(&file.rel);
    }
    remote.ignored = remote
        .ignored
        .iter()
        .map(|path| here.join(path.strip_prefix(&there).unwrap()))
        .collect();
    assert_eq!(local, remote);

    assert_eq!(
        git::log(&here, &[], 10, 0, None).unwrap(),
        git::log(&there, &[], 10, 0, None).unwrap()
    );
    assert_eq!(
        git::branches(&here).unwrap(),
        git::branches(&there).unwrap()
    );
    assert_eq!(
        git::show_in(&there, "HEAD", "a.txt").unwrap().as_deref(),
        Some("one\ntwo\n")
    );
    assert_eq!(git::show_in(&there, "HEAD", "src/new.rs").unwrap(), None);

    // A commit made through the server is in the repository.
    git::stage(&there, &["a.txt".to_string()]).unwrap();
    git::commit(&there, "second, from afar", false).unwrap();
    let log = git::log(&here, &[], 10, 0, None).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].subject, "second, from afar");

    // Discarding a new file moves it to the server user's trash.
    let status = git::status(&there).unwrap();
    assert_eq!(status.files.len(), 1);
    assert!(status.files[0].untracked());
    git::discard(&there, &status.files).unwrap();
    assert!(!here.join("src/new.rs").exists());
    assert!(git::status(&here).unwrap().files.is_empty());
    let trash = server.dir.join("home/.local/share/Trash/files/new.rs");
    assert!(trash.is_file(), "the discarded file is in the trash");

    // git's own failures come back as git's.
    let failed = git::check(&there, &["switch", "no-such-branch"], None).unwrap_err();
    assert_eq!(
        failed,
        git::check(&here, &["switch", "no-such-branch"], None).unwrap_err()
    );
}

#[test]
fn a_search_finds_the_same_on_the_server() {
    let server = Server::start("search");
    let here = repository(&server.dir);
    let there = server.there(&here);
    let query = search::Query {
        pattern: "t[wh]".into(),
        is_regex: true,
        use_ignore_files: true,
        max_results: 100,
        ..Default::default()
    };
    let run = |root: &Path, query: &search::Query| {
        let mut found = Vec::new();
        let done = search::search(root, query, &AtomicBool::new(false), |files| {
            found.extend(files);
            true
        });
        found.sort_by(|a, b| a.rel.cmp(&b.rel));
        (done.map(|done| done.limit_hit), found)
    };
    let (local_done, local) = run(&here, &query);
    let (remote_done, remote) = run(&there, &query);
    assert_eq!(local_done, remote_done);
    assert_eq!(local.len(), 1);
    assert_eq!(remote.len(), 1);
    assert_eq!(local[0].rel, "a.txt");
    assert_eq!(remote[0].rel, "a.txt");
    assert_eq!(remote[0].path, there.join("a.txt").to_string_lossy());
    let lines = |file: &search::FileMatch| -> Vec<(usize, usize, usize, String)> {
        file.matches
            .iter()
            .map(|m| (m.line, m.start, m.end, m.text.clone()))
            .collect()
    };
    assert_eq!(lines(&local[0]), lines(&remote[0]));
    assert_eq!(lines(&remote[0]).len(), 2);

    // Only the files named, by den's paths for them.
    let only = search::Query {
        pattern: "main".into(),
        paths: Some(vec![
            there.join("src/main.rs").to_string_lossy().into_owned(),
        ]),
        max_results: 100,
        ..Default::default()
    };
    let (_, found) = run(&there, &only);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, there.join("src/main.rs").to_string_lossy());

    // A pattern that does not parse says so in the same words.
    let broken = search::Query {
        pattern: "(".into(),
        is_regex: true,
        ..Default::default()
    };
    assert!(run(&here, &broken).0.is_err());
    assert_eq!(run(&here, &broken).0, run(&there, &broken).0);

    // Quick Open's list.
    let files =
        |root: &Path| -> BTreeSet<String> { search::files(root, 1000).into_iter().collect() };
    assert_eq!(files(&here), files(&there));
    assert_eq!(
        files(&there),
        BTreeSet::from(["a.txt", "src/main.rs", "src/new.rs"].map(String::from))
    );
}

#[test]
fn a_search_that_is_stopped_stops_on_the_server() {
    let server = Server::start("stop");
    let here = server.dir.join("many");
    std::fs::create_dir_all(&here).unwrap();
    for n in 0..400 {
        std::fs::write(here.join(format!("f{n}.txt")), "needle\n".repeat(50)).unwrap();
    }
    let there = server.there(&here);
    let query = search::Query {
        pattern: "needle".into(),
        max_results: 1_000_000,
        ..Default::default()
    };
    // The caller says stop at the first batch.
    let mut batches = 0;
    search::search(&there, &query, &AtomicBool::new(false), |_| {
        batches += 1;
        false
    })
    .unwrap();
    assert_eq!(batches, 1);
    // Cancelled before it began: nothing is handed over.
    let mut seen = 0;
    search::search(&there, &query, &AtomicBool::new(true), |files| {
        seen += files.len();
        true
    })
    .unwrap();
    assert_eq!(seen, 0);
    // The connection is as good as before.
    assert!(server.connection.call(&Request::Ping, Vec::new()).is_ok());
}

#[test]
fn changes_on_the_server_are_reported_with_dens_paths() {
    let server = Server::start("watch");
    let here = server.dir.join("watched");
    std::fs::create_dir_all(&here).unwrap();
    let there = server.there(&here);
    let (watcher, mut changes) = watch::watch(&there).unwrap();
    let mut seen = BTreeSet::new();
    // The watch starts on the server a moment after it was asked for.
    let mut n = 0;
    wait_until("a change to be reported", || {
        n += 1;
        std::fs::write(here.join("touched.txt"), format!("{n}")).unwrap();
        while let Ok(path) = changes.try_recv() {
            seen.insert(path);
        }
        seen.contains(&there.join("touched.txt"))
    });
    // Dropped, the watch ends on the server too: its call is over.
    drop(watcher);
    wait_until("the watch to end", || futures_closed(&mut changes));
}

/// Whether the channel is closed and empty.
fn futures_closed(changes: &mut futures::channel::mpsc::UnboundedReceiver<PathBuf>) -> bool {
    use futures::channel::mpsc::TryRecvError;
    loop {
        match changes.try_recv() {
            Ok(_) => {}
            Err(TryRecvError::Closed) => return true,
            Err(TryRecvError::Empty) => return false,
        }
    }
}

#[test]
fn what_a_login_shell_prints_first_is_skipped() {
    let server = Server::start_with(
        "noise",
        "echo 'Welcome to box'; printf 'DEN-SERVE\\nlast login: never\\n'; exec",
        Options::default(),
    );
    assert!(server.connection.call(&Request::Ping, Vec::new()).is_ok());
}

#[test]
fn what_is_not_a_den_server_is_an_error_in_sshs_words() {
    let host = HostId::new("test-not-a-server").unwrap();
    let mut refused = Command::new("/bin/sh");
    refused.args([
        "-c",
        "echo 'me@box: Permission denied (publickey).' >&2; exit 255",
    ]);
    let error = Connection::start(host.clone(), refused, Options::default()).err();
    assert_eq!(
        error,
        Some(Error::Start(
            "me@box: Permission denied (publickey).".into()
        ))
    );

    let mut silent = Command::new("/bin/sh");
    silent.args(["-c", "sleep 30"]);
    let options = Options {
        start: Duration::from_millis(300),
        ..Options::default()
    };
    let began = Instant::now();
    assert!(matches!(
        Connection::start(host.clone(), silent, options),
        Err(Error::Start(_))
    ));
    assert!(began.elapsed() < Duration::from_secs(5));

    let mut old = Command::new("/bin/sh");
    old.args(["-c", "echo 'DEN-SERVER 0'; sleep 30"]);
    let Err(Error::Start(message)) = Connection::start(host, old, Options::default()) else {
        panic!("another version must not connect");
    };
    assert!(message.contains("version 0"), "{message}");
}

#[test]
fn a_server_that_dies_ends_every_call_and_is_never_this_machine() {
    let server = Server::start("dies");
    let here = repository(&server.dir);
    let there = server.there(&here);
    let (_watcher, mut changes) = watch::watch(&there).unwrap();
    let stream = server
        .connection
        .stream(
            &Request::Watch {
                root: here.to_string_lossy().into_owned(),
            },
            |_, _| {},
        )
        .unwrap();
    sh(&server.dir, &format!("kill -9 {}", server.pid()));
    // The call under way ends, and so does the watch's channel.
    assert_eq!(stream.wait().err(), Some(Error::Disconnected));
    wait_until("the watch to end", || futures_closed(&mut changes));
    assert!(!server.connection.alive());
    assert_eq!(
        server.connection.call(&Request::Ping, Vec::new()).err(),
        Some(Error::Disconnected)
    );
    // The folder is out of reach, not a folder of this machine: git is not
    // "missing", the file is not "gone".
    assert!(matches!(remote::route(&there), Route::Offline(_)));
    assert!(matches!(
        git::status(&there),
        Err(git::Error::Failed(message)) if message.contains("Not connected")
    ));
    assert!(fs::stat(&there.join("a.txt")).is_err());
    assert!(watch::watch(&there).is_err());
    assert!(
        search::search(
            &there,
            &search::Query::default(),
            &AtomicBool::new(false),
            |_| true
        )
        .is_err()
    );
}

#[test]
fn a_server_that_stops_answering_is_given_up_on() {
    let options = Options {
        ping: Duration::from_millis(100),
        patience: Duration::from_millis(400),
        ..Options::default()
    };
    let server = Server::start_with("hangs", "exec", options);
    let pid = server.pid();
    assert!(server.connection.call(&Request::Ping, Vec::new()).is_ok());
    sh(&server.dir, &format!("kill -STOP {pid}"));
    let began = Instant::now();
    // A call with all the time in the world still ends.
    assert_eq!(
        server.connection.call(&Request::Ping, Vec::new()).err(),
        Some(Error::Disconnected)
    );
    assert!(began.elapsed() < Duration::from_secs(5));
    assert!(!server.connection.alive());
    sh(&server.dir, &format!("kill -9 {pid} 2>/dev/null; true"));
}

#[test]
fn the_server_ends_when_den_goes() {
    use std::io::{BufRead as _, BufReader, Write as _};
    let dir = scratch("ends");
    let mut child = command(&dir, "exec")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut greeting = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut greeting)
        .unwrap();
    assert_eq!(greeting, "DEN-SERVER 1\n");
    // A watch under way (a request with no end of its own), then den's end
    // of the pipe closes, as when ssh drops: the server does not stay.
    let mut stdin = child.stdin.take().unwrap();
    let json = serde_json::to_vec(&Request::Watch {
        root: dir.to_string_lossy().into_owned(),
    })
    .unwrap();
    let mut frame = ((9 + json.len()) as u32).to_le_bytes().to_vec();
    frame.push(1);
    frame.extend(7u32.to_le_bytes());
    frame.extend((json.len() as u32).to_le_bytes());
    frame.extend(json);
    stdin.write_all(&frame).unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    drop(stdin);
    let mut status = None;
    wait_until("the server to end", || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    assert!(status.unwrap().success());
    let _ = std::fs::remove_dir_all(&dir);
}
