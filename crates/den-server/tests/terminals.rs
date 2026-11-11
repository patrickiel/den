//! Terminals on a "server": the built den-server started as ssh starts it,
//! its host started by it, and real shells in real terminals. What matters
//! is what a den that left and came back finds.

#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use den_core::remote::{
    self, Connection, Error, FaultKind, HostId, Options,
    term::{self, Event, Session},
};
use futures::channel::mpsc::UnboundedReceiver;

const SIZE: (u16, u16) = (80, 24);

/// A den-server on this machine and den's connection to it. The host it
/// starts keeps the terminals in `dir`, and goes a second after the last.
struct Server {
    host: HostId,
    dir: PathBuf,
    connection: Arc<Connection>,
}

impl Server {
    fn start(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("den-term-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        Self::again(name, std::fs::canonicalize(dir).unwrap(), 0)
    }

    /// Another den-server for the same user (`dir`), as a den that connects
    /// again gets.
    fn again(name: &str, dir: PathBuf, nth: u32) -> Self {
        let host = HostId::new(&format!("term-{name}-{nth}-{}", std::process::id())).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_den-server"));
        cmd.arg("connect")
            .arg("--home")
            .arg(dir.join("server"))
            .env("DEN_SERVER_LOGIN_PATH", "0")
            .env("DEN_SERVER_HOST_IDLE", "1")
            .env("DEN_TEST_SERVER", env!("CARGO_BIN_EXE_den-server"))
            .env("SHELL", "/bin/sh")
            .env("HOME", dir.join("home"));
        let connection = Connection::start(host.clone(), cmd, Options::default()).unwrap();
        remote::register(connection.clone());
        Server {
            host,
            dir,
            connection,
        }
    }

    /// den's path for the server's folder.
    fn root(&self) -> PathBuf {
        remote::join(&self.host, &self.dir.to_string_lossy())
    }

    /// Start a terminal, running `command` first.
    fn spawn(&self, command: Option<&str>) -> u64 {
        let id = term::new_id();
        term::spawn(&self.root(), &self.root(), id, None, command, SIZE).unwrap();
        id
    }

    fn attach(&self, id: u64, have: Option<u64>) -> (Session, Seen) {
        let (session, events) = term::attach(&self.root(), id, have).unwrap();
        let seen = Seen {
            events,
            text: String::new(),
            offset: 0,
            modes: Vec::new(),
            hooks: Vec::new(),
            resets: 0,
            resumed: false,
            exited: false,
        };
        (session, seen)
    }

    fn hang_up(&self) {
        remote::unregister(&self.host);
        self.connection.close();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // The terminals with it: the host then goes by itself.
        if let Ok(terminals) = term::list(&self.root()) {
            for terminal in terminals {
                let _ = term::kill(&self.root(), terminal.id);
            }
        }
        self.hang_up();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What a terminal has said so far.
struct Seen {
    events: UnboundedReceiver<Event>,
    text: String,
    /// The offset after the last output.
    offset: u64,
    modes: Vec<u8>,
    hooks: Vec<(String, String, String)>,
    resets: usize,
    resumed: bool,
    exited: bool,
}

impl Seen {
    /// Take what has arrived; `ack` says whether den keeps up.
    fn take(&mut self, session: Option<&Session>) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Reset { from, .. } => {
                    self.resets += 1;
                    self.text.clear();
                    self.offset = from;
                }
                Event::Resumed => self.resumed = true,
                Event::Output { offset, bytes } => {
                    assert_eq!(offset, self.offset, "output arrives in order, none missing");
                    self.offset += bytes.len() as u64;
                    self.text.push_str(&String::from_utf8_lossy(&bytes));
                    if let Some(session) = session {
                        session.ack(self.offset);
                    }
                }
                Event::Modes(bytes) => self.modes = bytes,
                Event::Hook { agent, kind, body } => self.hooks.push((agent, kind, body)),
                Event::Exited => self.exited = true,
            }
        }
    }

    /// Wait until `done` says so, keeping up with the output.
    fn until(&mut self, session: &Session, what: &str, done: impl Fn(&Seen) -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            self.take(Some(session));
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < until,
                "waited for {what}; saw {:?}",
                self.text
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "waited for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn not_found(error: &Error) -> bool {
    matches!(error, Error::Failed(fault) if fault.kind == FaultKind::NotFound)
}

fn file(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn a_shell_runs_and_answers() {
    let server = Server::start("echo");
    let id = server.spawn(None);
    let (session, mut seen) = server.attach(id, None);
    session.write(b"echo hel''lo; pwd\n");
    let dir = server.dir.to_string_lossy().into_owned();
    seen.until(&session, "the shell's answer", |seen| {
        seen.text.contains("hello") && seen.text.contains(&dir)
    });
    assert_eq!(seen.resets, 1, "a den that had nothing starts from nothing");
    let listed = term::list(&server.root()).unwrap();
    assert_eq!(listed.iter().map(|t| t.id).collect::<Vec<_>>(), [id]);
    assert_eq!(listed[0].root, dir);
    // The program ends: den is told, and the terminal is no more.
    session.write(b"exit\n");
    seen.until(&session, "the end", |seen| seen.exited);
    assert!(term::list(&server.root()).unwrap().is_empty());
}

#[test]
fn a_den_that_comes_back_gets_what_it_missed() {
    let server = Server::start("back");
    let id = server.spawn(None);
    let (session, mut seen) = server.attach(id, None);
    session.write(b"echo o''ne\n");
    seen.until(&session, "one", |seen| seen.text.contains("one"));
    // Bracketed paste on, then den goes while the shell still works.
    session.write(b"printf '\\033[?2004h'; sleep 0.3; echo t''wo\n");
    seen.until(&session, "the command read", |seen| {
        seen.text.contains("sleep")
    });
    let have = seen.offset;
    drop(session);
    std::thread::sleep(Duration::from_millis(600));
    // Back with what it had: only what is new, nothing dropped.
    let (session, mut again) = server.attach(id, Some(have));
    again.offset = have;
    again.until(&session, "two", |seen| seen.text.contains("two"));
    assert!(again.resumed && again.resets == 0);
    assert!(!again.text.contains("one"));
    drop(session);
    // Back with nothing (a den started anew): all of it, then the modes.
    let (session, mut fresh) = server.attach(id, None);
    fresh.until(&session, "the modes", |seen| !seen.modes.is_empty());
    assert_eq!(fresh.resets, 1);
    assert!(fresh.text.contains("one") && fresh.text.contains("two"));
    let modes = String::from_utf8_lossy(&fresh.modes).into_owned();
    assert!(modes.contains("\x1b[?2004h"), "{modes:?}");
    assert!(modes.contains("\x1b[?1000l"));
}

#[test]
fn a_terminal_outlives_den_server() {
    let server = Server::start("outlives");
    let id = server.spawn(None);
    let (session, mut seen) = server.attach(id, None);
    session.write(b"echo st''ays\n");
    seen.until(&session, "stays", |seen| seen.text.contains("stays"));
    let have = seen.offset;
    // The connection goes, den-server with it.
    drop(session);
    server.hang_up();
    std::thread::sleep(Duration::from_millis(300));
    // den connects again: another den-server, the same host.
    let back = Server::again("outlives", server.dir.clone(), 1);
    let listed = term::list(&back.root()).unwrap();
    assert_eq!(listed.iter().map(|t| t.id).collect::<Vec<_>>(), [id]);
    let (session, mut seen) = back.attach(id, Some(have));
    seen.offset = have;
    session.write(b"echo ba''ck\n");
    seen.until(&session, "back", |seen| seen.text.contains("back"));
    assert!(seen.resumed);
    drop(session);
    drop(back);
    // `server`'s own drop finds its connection closed and only tidies up.
}

#[test]
fn a_flood_waits_for_den() {
    let server = Server::start("flood");
    let id = server.spawn(None);
    let (session, mut seen) = server.attach(id, None);
    session.write(b"yes den\n");
    // den shows nothing: the server stops a little over a megabyte ahead.
    let mut got = 0;
    let mut quiet = Instant::now();
    while quiet.elapsed() < Duration::from_millis(700) {
        seen.take(None);
        if seen.offset != got {
            got = seen.offset;
            quiet = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!((1 << 20..(1 << 20) + (64 << 10)).contains(&got), "{got}");
    // It shows what it has: more comes.
    session.ack(got);
    seen.until(&session, "more", |seen| seen.offset > got + (1 << 20));
    // Meanwhile the oldest output went: a den that lost its place starts
    // from what is kept, on a whole line.
    drop(session);
    let (session, mut late) = server.attach(id, Some(1));
    late.until(&session, "the flood again", |seen| seen.text.len() > 100);
    // (More than once, when the flood passes it again meanwhile.)
    assert!(late.resets >= 1 && !late.resumed);
    assert!(late.text.starts_with("den\r\n"), "{:?}", &late.text[..20]);
    session.kill();
    wait_for("the end of yes", || {
        term::list(&server.root()).unwrap().is_empty()
    });
    // Hung up on, the shell and `yes` with it: nothing keeps the host.
    let socket = server.dir.join("server/data/host.sock");
    wait_for("the host to go", || !socket.exists());
}

#[test]
fn an_agents_hooks_reach_den() {
    let server = Server::start("hooks");
    let id = server.spawn(None);
    let (session, mut seen) = server.attach(id, None);
    let hook = format!("\"$DEN_TEST_SERVER\" hook claude {id}");
    session.write(format!("echo '{{\"n\":1}}' | {hook} done\n").as_bytes());
    seen.until(&session, "the hook", |seen| !seen.hooks.is_empty());
    assert_eq!(seen.hooks[0].0, "claude");
    assert_eq!(seen.hooks[0].1, "done");
    assert_eq!(seen.hooks[0].2.trim(), "{\"n\":1}");
    // One that fires while den is away waits for it.
    let marker = server.dir.join("hooked");
    let later = format!(
        "sleep 0.3; {hook} input --body waiting; touch '{}'\n",
        marker.display()
    );
    session.write(later.as_bytes());
    seen.until(&session, "the command read", |seen| {
        seen.text.contains("sleep")
    });
    let have = seen.offset;
    drop(session);
    wait_for("the hook while away", || marker.exists());
    let (session, mut again) = server.attach(id, Some(have));
    again.offset = have;
    again.until(&session, "the hook kept", |seen| !seen.hooks.is_empty());
    assert_eq!(again.hooks[0].1, "input");
    assert_eq!(again.hooks[0].2, "waiting");
}

#[test]
fn a_question_is_answered_with_no_den_there() {
    let server = Server::start("asks");
    let reply = server.dir.join("reply");
    // Where is the cursor? Nobody is attached; the host's own terminal says.
    let command = format!(
        "stty raw -echo; printf '\\033[6n'; head -c 4 > '{0}.part'; stty sane; mv '{0}.part' '{0}'",
        reply.display()
    );
    server.spawn(Some(&command));
    wait_for("the answer", || reply.exists());
    assert!(file(&reply).starts_with("\x1b["), "{:?}", file(&reply));
}

#[test]
fn a_killed_terminal_is_gone() {
    let server = Server::start("kill");
    let id = server.spawn(None);
    term::kill(&server.root(), id).unwrap();
    assert!(term::list(&server.root()).unwrap().is_empty());
    let Err(error) = term::attach(&server.root(), id, None) else {
        panic!("attached to a terminal that was killed");
    };
    assert!(not_found(&error), "{error:?}");
    // Nor one that never was.
    let Err(error) = term::attach(&server.root(), 12345, Some(7)) else {
        panic!("attached to a terminal that never was");
    };
    assert!(not_found(&error), "{error:?}");
}
