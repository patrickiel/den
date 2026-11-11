//! The pipe to a server: the system's own ssh, so `~/.ssh/config`, the
//! agent, jump hosts and known hosts all apply as they do in a terminal.
//! It never asks (no password, no passphrase, no "are you sure"): den has
//! no terminal to ask in, and says to connect once from one instead.

use std::{
    collections::BTreeMap,
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
};

use super::{Connection, Error, HostId, Options};
use crate::process;

/// Set, it is run by the shell in place of ssh, with the server's command
/// line after it. For working on den with no server at hand, a folder of
/// this machine as the server's home:
/// `DEN_REMOTE_COMMAND='cd /tmp/home && HOME=/tmp/home'`.
pub const COMMAND_ENV: &str = "DEN_REMOTE_COMMAND";

/// The command that runs `remote` (a command line for the server's shell)
/// on `host`, its input and output den's.
pub fn command(host: &HostId, remote: &str) -> Command {
    if let Some(own) = std::env::var_os(COMMAND_ENV).filter(|own| !own.is_empty()) {
        let mut line = own;
        line.push(" ");
        line.push(remote);
        let mut cmd = if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C");
            cmd
        } else {
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-c");
            cmd
        };
        cmd.arg(line);
        return cmd;
    }
    let mut cmd = Command::new(program());
    cmd.args(options(host)).arg(remote);
    cmd
}

/// ssh's arguments up to and including the destination.
fn options(host: &HostId) -> Vec<String> {
    let (destination, port) = host.destination();
    let mut args: Vec<String> = [
        // No terminal on the far side: the bytes go through as they are.
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        // A server that went without a word is noticed within 15 seconds.
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=3",
    ]
    .map(String::from)
    .to_vec();
    if let Some(port) = port {
        args.extend(["-p".to_string(), port.to_string()]);
    }
    // After `--` nothing is an option, whatever the destination looks like.
    args.extend(["--".to_string(), destination.to_string()]);
    args
}

/// Windows ships OpenSSH in System32; a den started from a 32-bit or odd
/// PATH still finds it there. Elsewhere, and failing that, the PATH's.
fn program() -> PathBuf {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        let own = PathBuf::from(root)
            .join("System32")
            .join("OpenSSH")
            .join("ssh.exe");
        if own.is_file() {
            return own;
        }
    }
    PathBuf::from("ssh")
}

/// The program for a machine, or why there is none for it.
pub type Binary = dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync;

/// The den-server this den puts on servers.
pub struct Server {
    /// Names this build of it: letters and digits, part of the path it is
    /// installed at, so each den finds its own and never another's.
    pub id: String,
    /// The program for a machine, by what `uname -sm` says there
    /// (`Linux x86_64`); an error to show for a machine it is not built for.
    pub binary: Box<Binary>,
}

static SERVER: OnceLock<Server> = OnceLock::new();

/// Say which den-server `connect` installs; once, when den starts.
pub fn set_server(server: Server) {
    assert!(
        !server.id.is_empty() && server.id.chars().all(|c| c.is_ascii_alphanumeric()),
        "the id goes into a command line as it is"
    );
    let _ = SERVER.set(server);
}

/// Where den-server lives on a server, from the home folder.
fn folder(id: &str) -> String {
    format!(".den-server/bin/{id}")
}

/// The connection to `host`: the one there is, or a new one, with den-server
/// put on the server first when it is not there yet. Waits for ssh; not for
/// the UI thread.
pub fn connect(host: &HostId) -> Result<Arc<Connection>, Error> {
    // One at a time for a server: two folders opening on it share the one.
    static CONNECTING: Mutex<BTreeMap<HostId, Arc<Mutex<()>>>> = Mutex::new(BTreeMap::new());
    let gate = CONNECTING
        .lock()
        .unwrap()
        .entry(host.clone())
        .or_default()
        .clone();
    let _one = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(connection) = super::connection(host) {
        return Ok(connection);
    }
    let Some(server) = SERVER.get() else {
        return Err(Error::Start(
            "This den has no den-server to put on a server.".into(),
        ));
    };
    let line = format!("{}/den-server connect", folder(&server.id));
    let start = || Connection::try_start(host.clone(), command(host, &line), Options::default());
    let connection = match start() {
        Ok(connection) => connection,
        // ssh's own failure (no route, no key), or no answer at all.
        Err(refused) if matches!(refused.code, Some(255) | None) => {
            return Err(Error::Start(refused.message));
        }
        // ssh got through and the server's shell found no den-server.
        Err(_) => {
            install(host, server)?;
            start().map_err(|refused| Error::Start(refused.message))?
        }
    };
    super::register(connection.clone());
    Ok(connection)
}

/// Whether ssh failed for want of an answer only a person can give (a
/// password, a passphrase, yes to a new host key): den has no terminal to
/// ask in, and `ssh <host>` in one settles it.
pub fn wants_a_terminal(message: &str) -> bool {
    [
        "Permission denied",
        "Host key verification failed",
        "passphrase",
        "password",
    ]
    .iter()
    .any(|sign| message.contains(sign))
}

/// The servers `~/.ssh/config` names, to offer when one is asked for.
pub fn hosts() -> Vec<String> {
    std::env::home_dir()
        .and_then(|home| std::fs::read_to_string(home.join(".ssh").join("config")).ok())
        .map(|text| config_hosts(&text))
        .unwrap_or_default()
}

/// The names an ssh config's `Host` lines give, patterns left out, in the
/// file's order.
fn config_hosts(text: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // `Host a b`, `Host=a`, any case; `HostName` is another keyword.
        let Some((keyword, names)) = line.split_once(|c: char| c.is_whitespace() || c == '=')
        else {
            continue;
        };
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        for name in names.split(|c: char| c.is_whitespace() || c == '=') {
            let name = name.trim_matches('"');
            let pattern = name.contains(['*', '?', '!']);
            if !name.is_empty() && !pattern && !hosts.iter().any(|host| host == name) {
                hosts.push(name.to_string());
            }
        }
    }
    hosts
}

/// Put den-server on `host`, in two visits: one to ask what machine it is
/// and make the folder, one to send the program.
fn install(host: &HostId, server: &Server) -> Result<(), Error> {
    let dir = folder(&server.id);
    // The script on sh's input, so the login shell (fish, csh) reads none
    // of it.
    let probe = run(
        host,
        "sh",
        format!("uname -sm\nmkdir -p {dir}\n").into_bytes(),
    )?;
    let machine = probe
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string();
    let program = (server.binary)(&machine).map_err(Error::Start)?;
    // Under another name first: a den-server half arrived is never run.
    let (tmp, done) = (
        format!("{dir}/den-server.{}.tmp", std::process::id()),
        format!("{dir}/den-server"),
    );
    // Single quotes mean the same to every login shell.
    let line = format!("sh -c 'cat > {tmp} && chmod 755 {tmp} && mv {tmp} {done}'");
    run(host, &line, program).map(drop)
}

/// Run `remote` on `host` with `input` on its standard input; its output,
/// or what it (or ssh) said when it failed.
fn run(host: &HostId, remote: &str, input: Vec<u8>) -> Result<String, Error> {
    let mut cmd = command(host, remote);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::no_window(&mut cmd);
    let failed = |e: std::io::Error| Error::Start(format!("Could not run ssh: {e}"));
    let mut child = cmd.spawn().map_err(failed)?;
    let mut stdin = child.stdin.take().expect("piped");
    // From a thread of its own: a long input cannot wait on a full output.
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output().map_err(failed)?;
    let _ = writer.join();
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Error::Start(if said.is_empty() {
            format!("ssh ended with {}", output.status)
        } else {
            said
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_destination_comes_last_and_cannot_be_an_option() {
        let args = options(&HostId::new("me@box:2222").unwrap());
        assert_eq!(args[args.len() - 4..], ["-p", "2222", "--", "me@box"]);
        assert!(args.contains(&"BatchMode=yes".to_string()));
        let args = options(&HostId::new("box").unwrap());
        assert_eq!(args[args.len() - 2..], ["--", "box"]);
        assert!(!args.contains(&"-p".to_string()));
    }

    #[test]
    fn the_config_names_its_servers() {
        let config = "# mine\nHost box  dev.example.org\n  HostName 10.0.0.2\n\
            host=\"pi\"\nHost *.lan !bad\nHost box\nMatch host x\n";
        assert_eq!(config_hosts(config), ["box", "dev.example.org", "pi"]);
    }
}
