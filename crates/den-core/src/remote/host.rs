//! The host: the program that keeps a server's terminals running. den-server
//! comes and goes with den's connection; the shells and agents started from
//! den must not, so one host per user holds them, started by the first
//! den-server that needs one and gone again a while after its last terminal.
//!
//! It listens on a socket only its user can reach, in den-server's folder.
//! Every visit is a connection of its own: one `Ask`, then what it calls
//! for. An attached den-server stays, taking the terminal's output and
//! sending what is typed.
//!
//! For each terminal the host keeps the last of its output, to send a den
//! that comes back what it missed, and follows the output with a terminal
//! of its own that nobody sees: that one knows which modes the program has
//! set (a returning den is told), and answers the program's questions
//! (where is the cursor?) while no den is there to.

use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt as _,
        net::{UnixListener, UnixStream},
        process::CommandExt as _,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    grid::Dimensions,
    term::{Config, TermMode},
    vte::ansi::Processor,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Serialize, de::DeserializeOwned};

use super::{
    frame,
    proto::{Ask, Tell, TermInfo},
};
use crate::{agent, osc, shell};

/// How much of a terminal's output is kept for a den that comes back.
const KEPT: usize = 2 << 20;
/// How far the host sends ahead of what den says it has shown: a program
/// that prints without end fills this much of the way to den, no more.
const AHEAD: u64 = 1 << 20;
/// The most output in one message.
const PIECE: usize = 32 * 1024;
/// With no terminal and no visitor for this long, the host ends.
const IDLE: Duration = Duration::from_secs(30);
/// Seconds, in place of `IDLE` (tests).
const IDLE_ENV: &str = "DEN_SERVER_HOST_IDLE";
/// Where den-server keeps its files, when not `~/.den-server`: set by
/// `--home`, and handed to all it starts, the terminals' hooks included.
pub const HOME_ENV: &str = "DEN_SERVER_HOME";

/// The host's socket.
fn socket() -> PathBuf {
    crate::dirs::data_dir().join("host.sock")
}

/// A way to name `path` that fits a socket's address (about a hundred
/// bytes): the folder held open and named by its number. The file keeps the
/// name good.
fn short(path: &Path) -> io::Result<(PathBuf, Option<std::fs::File>)> {
    if path.as_os_str().len() < 100 || !cfg!(target_os = "linux") {
        return Ok((path.to_path_buf(), None));
    }
    use std::os::fd::AsRawFd as _;
    let (dir, name) = (
        path.parent().unwrap_or(Path::new("/")),
        path.file_name().unwrap_or_default(),
    );
    let dir = std::fs::File::open(dir)?;
    let short = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(name);
    Ok((short, Some(dir)))
}

// -- Messages ---------------------------------------------------------------

/// Send `message` and `blob` (`PIECE` at most) as one frame.
pub fn send(out: &mut impl Write, message: &impl Serialize, blob: &[u8]) -> io::Result<()> {
    let json = serde_json::to_vec(message).expect("plain data");
    frame::write_frame(out, frame::ITEM, 0, &json, blob)?;
    out.flush()
}

/// The next message; `None` when the other side hung up.
pub fn receive<T: DeserializeOwned>(input: &mut impl Read) -> io::Result<Option<(T, Vec<u8>)>> {
    let Some(message) = frame::read_frame(input)? else {
        return Ok(None);
    };
    let value = serde_json::from_slice(&message.json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some((value, message.blob)))
}

// -- den-server's side ------------------------------------------------------

/// A connection to the host, which is started when there is none.
pub fn connect() -> io::Result<UnixStream> {
    let path = socket();
    let (short, _dir) = short(&path)?;
    if let Ok(stream) = UnixStream::connect(&short) {
        return Ok(stream);
    }
    let mut host = Command::new(std::env::current_exe()?);
    host.arg("host")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A session of its own: what ends den-server's (ssh hanging up) is not
    // its business.
    unsafe {
        host.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = host.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(&short) {
            Ok(stream) => return Ok(stream),
            Err(e) if Instant::now() > until => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Ask the host one thing and take its one answer.
pub fn ask(ask: &Ask) -> io::Result<Tell> {
    let mut stream = connect()?;
    send(&mut stream, ask, &[])?;
    match receive::<Tell>(&mut stream)? {
        Some((Tell::Err { message }, _)) => Err(io::Error::other(message)),
        Some((tell, _)) => Ok(tell),
        None => Err(io::Error::other("The terminals' host hung up.")),
    }
}

/// What starts the terminal `id` on this machine: the user's shell (or
/// `shell`) with den's files for it, `command` run first, and the
/// environment it gets.
pub fn command_line(
    id: u64,
    shell: Option<&str>,
    command: Option<&str>,
) -> (Vec<String>, Vec<(String, String)>) {
    let shell = shell
        .map(str::to_string)
        .or_else(|| {
            std::env::var("SHELL")
                .ok()
                .filter(|shell| !shell.is_empty())
        })
        .unwrap_or_else(|| "/bin/sh".into());
    let mut cmd = CommandBuilder::new(&shell);
    if !shell::configure(&mut cmd, &shell, id, command) {
        cmd.env("PROMPT_COMMAND", osc::BASH_HOOK);
        if let Some(command) = command {
            let command = agent::inject(id, command, false);
            cmd.args(["-c", &format!("{command}; exec {shell}")]);
        }
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let argv = cmd
        .get_argv()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let env = cmd
        .iter_full_env_as_str()
        // Not a Claude Code session's own markers: Claude Code in the
        // terminal would take itself for a child of that session.
        .filter(|(key, _)| !key.starts_with("CLAUDE_CODE") && *key != "CLAUDECODE")
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    (argv, env)
}

// -- The host ---------------------------------------------------------------

#[derive(Default)]
struct Host {
    terminals: Mutex<HashMap<u64, Arc<Terminal>>>,
    visitors: AtomicUsize,
}

/// Serve until there is nothing left to keep. Returns at once when another
/// host serves already.
pub fn run() -> io::Result<()> {
    let path = socket();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        // Nobody else gets to the socket, whatever the umask.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let (short, _dir) = short(&path)?;
    let listener = match UnixListener::bind(&short) {
        Ok(listener) => listener,
        Err(_) => {
            if UnixStream::connect(&short).is_ok() {
                return Ok(());
            }
            // Left by a host that was killed.
            std::fs::remove_file(&short)?;
            UnixListener::bind(&short)?
        }
    };
    // The terminals get their own default back when they start.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let host = Arc::new(Host::default());
    std::thread::spawn({
        let (host, short) = (host.clone(), short.clone());
        let idle = std::env::var(IDLE_ENV)
            .ok()
            .and_then(|seconds| seconds.parse().ok())
            .map_or(IDLE, Duration::from_secs);
        move || {
            let mut quiet: Option<Instant> = None;
            loop {
                std::thread::sleep(Duration::from_millis(500).min(idle));
                let running = host
                    .terminals
                    .lock()
                    .unwrap()
                    .values()
                    .any(|terminal| !terminal.state.lock().unwrap().exited);
                if running || host.visitors.load(Ordering::SeqCst) > 0 {
                    quiet = None;
                } else if quiet.get_or_insert_with(Instant::now).elapsed() >= idle {
                    let _ = std::fs::remove_file(&short);
                    std::process::exit(0);
                }
            }
        }
    });
    for stream in listener.incoming().flatten() {
        let host = host.clone();
        host.visitors.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            visit(&host, stream);
            host.visitors.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

fn visit(host: &Host, mut stream: UnixStream) {
    let ask = match receive::<Ask>(&mut stream) {
        Ok(Some((ask, _))) => ask,
        Ok(None) => return,
        Err(e) => {
            let message = format!("This host does not know what is asked: {e}");
            let _ = send(&mut stream, &Tell::Err { message }, &[]);
            return;
        }
    };
    let find = |id: u64| host.terminals.lock().unwrap().get(&id).cloned();
    let gone = || Tell::Err {
        message: "The terminal is not running any more.".into(),
    };
    let tell = match ask {
        Ask::Spawn {
            id,
            root,
            cwd,
            argv,
            env,
            cols,
            rows,
        } => match Terminal::start(id, root, cwd, argv, env, cols, rows) {
            Ok(terminal) => {
                host.terminals.lock().unwrap().insert(id, terminal);
                Tell::Ok
            }
            Err(e) => Tell::Err {
                message: format!("Could not start the shell: {e}"),
            },
        },
        Ask::Attach { id, have } => match find(id) {
            Some(terminal) => return terminal.attach(stream, have),
            None => gone(),
        },
        Ask::Kill { id } => {
            if let Some(terminal) = host.terminals.lock().unwrap().remove(&id) {
                terminal.kill();
            }
            Tell::Ok
        }
        Ask::List { root } => {
            let terminals = host.terminals.lock().unwrap();
            let mut terms: Vec<TermInfo> = terminals
                .values()
                .filter(|terminal| root.as_ref().is_none_or(|root| *root == terminal.root))
                .filter(|terminal| !terminal.state.lock().unwrap().exited)
                .map(|terminal| TermInfo {
                    id: terminal.id,
                    root: terminal.root.clone(),
                    cwd: terminal.cwd.clone(),
                })
                .collect();
            terms.sort_by_key(|term| term.id);
            Tell::Terms { terms }
        }
        Ask::Hook {
            id,
            agent,
            kind,
            body,
        } => match find(id) {
            Some(terminal) => {
                let mut state = terminal.state.lock().unwrap();
                // With no den there, the last few wait for one.
                if state.hooks.len() >= 16 {
                    state.hooks.pop_front();
                }
                state.hooks.push_back((agent, kind, body));
                terminal.changed.notify_all();
                Tell::Ok
            }
            None => gone(),
        },
        // For an attached den-server only.
        Ask::Write | Ask::Resize { .. } | Ask::Ack { .. } => Tell::Err {
            message: "Not attached to a terminal.".into(),
        },
    };
    let _ = send(&mut stream, &tell, &[]);
}

/// What the unseen terminal wants written back to the program.
#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<String>>>);

impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            self.0.lock().unwrap().push(text);
        }
    }
}

struct Size {
    cols: u16,
    rows: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

struct Terminal {
    id: u64,
    root: String,
    cwd: String,
    state: Mutex<State>,
    /// Output, an ack, a hook, a den that came or went.
    changed: Condvar,
    /// Apart from `state`: a program that reads nothing must not hold up
    /// the reading of what it prints.
    input: Mutex<Box<dyn Write + Send>>,
}

struct State {
    /// The last of the output; `start` is the offset of its first byte.
    kept: VecDeque<u8>,
    start: u64,
    seen: Term<Replies>,
    parser: Processor,
    replies: Replies,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    size: (u16, u16),
    exited: bool,
    /// Killed: whoever is attached is told it ended, shown all or not.
    killed: bool,
    /// Which attachment the terminal talks to; 0 for none.
    attached: u64,
    /// What that one has shown.
    acked: u64,
    hooks: VecDeque<(String, String, String)>,
}

impl State {
    fn end(&self) -> u64 {
        self.start + self.kept.len() as u64
    }

    /// Where to start a den that has nothing: the start of what is kept,
    /// or, when the oldest output is gone, of the first whole line.
    fn fresh(&self) -> u64 {
        if self.start == 0 {
            return 0;
        }
        match self.kept.iter().take(64 * 1024).position(|b| *b == b'\n') {
            Some(at) => self.start + at as u64 + 1,
            None => self.start,
        }
    }
}

enum Step {
    Tell(Tell, Vec<u8>),
    /// The modes, then a nudge for a program on the other screen to paint.
    Modes(Vec<u8>, bool),
    Over,
}

impl Terminal {
    fn start(
        id: u64,
        root: String,
        cwd: String,
        argv: Vec<String>,
        env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Self>, String> {
        let failed = |e: &dyn std::fmt::Display| e.to_string();
        let (cols, rows) = (cols.max(2), rows.max(1));
        let pair = native_pty_system()
            .openpty(pty_size(cols, rows))
            .map_err(|e| failed(&e))?;
        if argv.is_empty() {
            return Err("nothing to run".into());
        }
        let mut cmd = CommandBuilder::from_argv(argv.into_iter().map(Into::into).collect());
        cmd.env_clear();
        for (key, value) in &env {
            cmd.env(key, value);
        }
        // A folder that went meanwhile: the home folder, as a login does.
        let home = env.iter().find(|(key, _)| key == "HOME");
        match (Path::new(&cwd).is_dir(), home) {
            (true, _) => cmd.cwd(&cwd),
            (false, Some((_, home))) => cmd.cwd(home),
            (false, None) => cmd.cwd("/"),
        }
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| failed(&e))?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| failed(&e))?;
        let input = pair.master.take_writer().map_err(|e| failed(&e))?;
        let killer = child.clone_killer();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let replies = Replies::default();
        let config = Config {
            scrolling_history: 0,
            ..Config::default()
        };
        let terminal = Arc::new(Terminal {
            id,
            root,
            cwd,
            state: Mutex::new(State {
                kept: VecDeque::new(),
                start: 0,
                seen: Term::new(config, &Size { cols, rows }, replies.clone()),
                parser: Processor::new(),
                replies,
                master: pair.master,
                killer,
                size: (cols, rows),
                exited: false,
                killed: false,
                attached: 0,
                acked: 0,
                hooks: VecDeque::new(),
            }),
            changed: Condvar::new(),
            input: Mutex::new(input),
        });
        std::thread::spawn({
            let terminal = terminal.clone();
            move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => terminal.printed(&buf[..n]),
                    }
                }
                terminal.state.lock().unwrap().exited = true;
                terminal.changed.notify_all();
            }
        });
        Ok(terminal)
    }

    /// The program printed `bytes`.
    fn printed(&self, bytes: &[u8]) {
        let replies = {
            let mut state = self.state.lock().unwrap();
            state.kept.extend(bytes);
            let over = state.kept.len().saturating_sub(KEPT);
            if over > 0 {
                state.kept.drain(..over);
                state.start += over as u64;
            }
            let state = &mut *state;
            state.parser.advance(&mut state.seen, bytes);
            let replies = std::mem::take(&mut *state.replies.0.lock().unwrap());
            // With a den attached, its terminal answers.
            if state.attached == 0 {
                replies
            } else {
                Vec::new()
            }
        };
        self.changed.notify_all();
        for reply in replies {
            self.typed(reply.as_bytes());
        }
    }

    fn typed(&self, bytes: &[u8]) {
        let mut input = self.input.lock().unwrap();
        let _ = input.write_all(bytes).and_then(|()| input.flush());
    }

    fn resize(&self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        let mut state = self.state.lock().unwrap();
        if state.size != (cols, rows) {
            state.size = (cols, rows);
            let _ = state.master.resize(pty_size(cols, rows));
            state.seen.resize(Size { cols, rows });
        }
    }

    /// Hang up on what runs in the terminal, as closing a terminal's
    /// window does: the program in front, and the shell.
    fn kill(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(group) = state.master.process_group_leader() {
            unsafe { libc::killpg(group, libc::SIGHUP) };
        }
        let _ = state.killer.kill();
        state.killed = true;
        self.changed.notify_all();
    }

    /// `stream` is a den-server whose den shows this terminal: until it
    /// hangs up, or another one attaches.
    fn attach(self: Arc<Self>, stream: UnixStream, have: Option<u64>) {
        static ATTACHMENTS: AtomicU64 = AtomicU64::new(1);
        let me = ATTACHMENTS.fetch_add(1, Ordering::Relaxed);
        let Ok(mut out) = stream.try_clone() else {
            return;
        };
        let (first, mut cursor, mut modes_at) = {
            let mut state = self.state.lock().unwrap();
            state.attached = me;
            let end = state.end();
            match have {
                Some(have) if (state.start..=end).contains(&have) => {
                    state.acked = have;
                    (Tell::Resumed, have, None)
                }
                _ => {
                    let from = match state.seen.mode().contains(TermMode::ALT_SCREEN) {
                        // A program that owns the screen paints it again.
                        true => end,
                        false => state.fresh(),
                    };
                    state.acked = from;
                    let (cols, rows) = state.size;
                    let reset = Tell::Reset {
                        from,
                        live: end,
                        cols,
                        rows,
                    };
                    (reset, from, Some(end))
                }
            }
        };
        // Whoever was attached before sees it is not any more.
        self.changed.notify_all();
        if send(&mut out, &first, &[]).is_err() {
            return self.detach(me);
        }
        std::thread::spawn({
            let (terminal, mut input) = (self.clone(), stream);
            move || {
                while let Ok(Some((ask, blob))) = receive::<Ask>(&mut input) {
                    match ask {
                        Ask::Write => terminal.typed(&blob),
                        Ask::Resize { cols, rows } => terminal.resize(cols, rows),
                        Ask::Ack { offset } => {
                            let mut state = terminal.state.lock().unwrap();
                            if state.attached == me {
                                state.acked = state.acked.max(offset);
                                terminal.changed.notify_all();
                            }
                        }
                        _ => {}
                    }
                }
                terminal.detach(me);
            }
        });
        loop {
            let step = {
                let mut state = self.state.lock().unwrap();
                loop {
                    if state.attached != me {
                        break Step::Over;
                    }
                    if state.killed {
                        break Step::Tell(Tell::Exited, Vec::new());
                    }
                    if let Some((agent, kind, body)) = state.hooks.pop_front() {
                        break Step::Tell(Tell::Hook { agent, kind, body }, Vec::new());
                    }
                    let end = state.end();
                    // Nothing more until den has shown what it was sent: a
                    // program that prints without end is not sent whole.
                    let room = cursor.saturating_sub(state.acked) < AHEAD;
                    if room && cursor < state.start {
                        // den was so far behind that what comes next is gone.
                        cursor = state.fresh();
                        state.acked = cursor;
                        modes_at = Some(end);
                        let (cols, rows) = state.size;
                        let reset = Tell::Reset {
                            from: cursor,
                            live: end,
                            cols,
                            rows,
                        };
                        break Step::Tell(reset, Vec::new());
                    }
                    if room && modes_at.is_some_and(|at| cursor >= at) {
                        modes_at = None;
                        let mode = *state.seen.mode();
                        break Step::Modes(modes(mode), mode.contains(TermMode::ALT_SCREEN));
                    }
                    if room && cursor < end {
                        let at = (cursor - state.start) as usize;
                        let len = PIECE.min((end - cursor) as usize);
                        let piece: Vec<u8> = state.kept.range(at..at + len).copied().collect();
                        let offset = cursor;
                        cursor += len as u64;
                        break Step::Tell(Tell::Output { offset }, piece);
                    }
                    if state.exited && cursor == end {
                        break Step::Tell(Tell::Exited, Vec::new());
                    }
                    state = self.changed.wait(state).unwrap();
                }
            };
            let sent = match step {
                Step::Over => break,
                Step::Tell(tell, blob) => {
                    let last = tell == Tell::Exited;
                    send(&mut out, &tell, &blob).is_ok() && !last
                }
                Step::Modes(bytes, repaint) => {
                    let sent = send(&mut out, &Tell::Modes, &bytes).is_ok();
                    if sent && repaint {
                        self.nudge();
                    }
                    sent
                }
            };
            if !sent {
                break;
            }
        }
        self.detach(me);
        let _ = out.shutdown(std::net::Shutdown::Both);
    }

    fn detach(&self, me: u64) {
        let mut state = self.state.lock().unwrap();
        if state.attached == me {
            state.attached = 0;
        }
        self.changed.notify_all();
    }

    /// Make the program paint its screen again: a size that changes and
    /// changes back is what it repaints for.
    fn nudge(&self) {
        let state = self.state.lock().unwrap();
        let (cols, rows) = state.size;
        let _ = state
            .master
            .resize(pty_size(cols, rows.saturating_sub(1).max(1)));
        std::thread::sleep(Duration::from_millis(30));
        let _ = state.master.resize(pty_size(cols, rows));
    }
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// The bytes that put a terminal in `mode`, each mode said outright: what
/// the terminal was sent before may have set any of them either way.
fn modes(mode: TermMode) -> Vec<u8> {
    let mut out = String::new();
    // The other screen first: the rest is said of the screen shown.
    if mode.contains(TermMode::ALT_SCREEN) {
        out.push_str("\x1b[?1049h");
    }
    for (code, flag) in [
        ("?1", TermMode::APP_CURSOR),
        ("?7", TermMode::LINE_WRAP),
        ("?25", TermMode::SHOW_CURSOR),
        ("?1000", TermMode::MOUSE_REPORT_CLICK),
        ("?1002", TermMode::MOUSE_DRAG),
        ("?1003", TermMode::MOUSE_MOTION),
        ("?1004", TermMode::FOCUS_IN_OUT),
        ("?1006", TermMode::SGR_MOUSE),
        ("?1007", TermMode::ALTERNATE_SCROLL),
        ("?2004", TermMode::BRACKETED_PASTE),
        ("4", TermMode::INSERT),
    ] {
        let set = if mode.contains(flag) { 'h' } else { 'l' };
        out.push_str(&format!("\x1b[{code}{set}"));
    }
    out.push_str(if mode.contains(TermMode::APP_KEYPAD) {
        "\x1b="
    } else {
        "\x1b>"
    });
    let keyboard = [
        TermMode::DISAMBIGUATE_ESC_CODES,
        TermMode::REPORT_EVENT_TYPES,
        TermMode::REPORT_ALTERNATE_KEYS,
        TermMode::REPORT_ALL_KEYS_AS_ESC,
        TermMode::REPORT_ASSOCIATED_TEXT,
    ]
    .iter()
    .enumerate()
    .filter(|(_, flag)| mode.contains(**flag))
    .fold(0, |bits, (ix, _)| bits | 1 << ix);
    out.push_str(&format!("\x1b[={keyboard};1u"));
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_are_said_outright() {
        let said = String::from_utf8(modes(
            TermMode::BRACKETED_PASTE | TermMode::SHOW_CURSOR | TermMode::DISAMBIGUATE_ESC_CODES,
        ))
        .unwrap();
        assert!(said.contains("\x1b[?2004h") && said.contains("\x1b[?25h"));
        assert!(said.contains("\x1b[?1000l") && said.contains("\x1b[?7l"));
        assert!(!said.contains("1049"));
        assert!(said.ends_with("\x1b>\x1b[=1;1u"));
        let other = String::from_utf8(modes(TermMode::ALT_SCREEN)).unwrap();
        assert!(other.starts_with("\x1b[?1049h"));
    }
}
