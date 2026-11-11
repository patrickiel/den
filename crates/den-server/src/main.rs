//! den-server: den's work on a folder, on the server the folder is on. den
//! puts it there and starts it over SSH; nobody runs it by hand.
//!
//! `den-server connect` serves one den on its input and output and ends
//! when den goes.

use std::path::PathBuf;

use den_core::{dirs, remote::worker};

/// Where den-server keeps its files, in the home folder of the user it
/// runs as.
const HOME: &str = ".den-server";
/// Set to `0`, the login shell is not asked for its PATH (tests, which
/// start many).
const LOGIN_PATH_ENV: &str = "DEN_SERVER_LOGIN_PATH";

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") => println!("den-server {}", env!("CARGO_PKG_VERSION")),
        Some("connect") => {
            keep_files(args.next().as_deref(), args.next());
            // The agents in this server's terminals report through this
            // program (`hook`, below).
            #[cfg(unix)]
            if let Ok(program) = std::env::current_exe() {
                den_core::agent::report_through(&program);
            }
            // Started by sshd, not by a login shell: git's hooks and
            // credential helpers need the PATH the user has in a terminal.
            #[cfg(unix)]
            if std::env::var_os(LOGIN_PATH_ENV).is_none_or(|v| v != "0") {
                den_core::login_path::inherit();
            }
            worker::serve(std::io::stdin().lock(), output());
        }
        // Keeps this user's terminals running between den's visits; started
        // by `connect` when one is wanted and there is none.
        #[cfg(unix)]
        Some("host") => {
            keep_files(None, None);
            if let Err(e) = den_core::remote::host::run() {
                eprintln!("den-server: {e}");
                std::process::exit(1);
            }
        }
        // `hook <agent> <terminal> <kind> [--body <json>]`: what an agent's
        // hook runs in a terminal, its JSON on the input or after `--body`.
        #[cfg(unix)]
        Some("hook") => {
            keep_files(None, None);
            let (agent, id, kind) = (args.next(), args.next(), args.next());
            let body = match (args.next().as_deref(), args.next()) {
                (Some("--body"), Some(body)) => body,
                _ => {
                    use std::io::Read as _;
                    let mut body = String::new();
                    let _ = std::io::stdin().take(64 * 1024).read_to_string(&mut body);
                    body
                }
            };
            let id = id.and_then(|id| id.parse().ok());
            if let (Some(agent), Some(id), Some(kind)) = (agent, id, kind) {
                use den_core::remote::proto::Ask;
                // The agent goes on whatever comes of it.
                let _ = den_core::remote::host::ask(&Ask::Hook {
                    id,
                    agent,
                    kind,
                    body,
                });
            }
        }
        _ => {
            eprintln!("den-server is started by den, over SSH.");
            std::process::exit(2);
        }
    }
}

/// Say where den-server keeps its files: the folder after `--home`, else the
/// one a den-server before this one was told (its terminals and their hooks
/// are started with it), else `~/.den-server`.
fn keep_files(flag: Option<&str>, folder: Option<String>) {
    #[cfg(unix)]
    const HOME_ENV: &str = den_core::remote::host::HOME_ENV;
    #[cfg(not(unix))]
    const HOME_ENV: &str = "DEN_SERVER_HOME";
    let home = match (flag, folder) {
        (Some("--home"), Some(folder)) => PathBuf::from(folder),
        _ => match std::env::var_os(HOME_ENV).filter(|home| !home.is_empty()) {
            Some(home) => PathBuf::from(home),
            None => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(HOME),
        },
    };
    // Before any thread starts: the host and the terminals inherit it.
    unsafe { std::env::set_var(HOME_ENV, &home) };
    dirs::set_data_dir(home.join("data"));
}

/// Standard output as it is: Rust's own looks for line ends to flush at,
/// which frames of bytes have no use for.
#[cfg(unix)]
fn output() -> std::fs::File {
    use std::os::fd::AsFd as _;
    std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .expect("standard output")
}

#[cfg(not(unix))]
fn output() -> std::io::Stdout {
    std::io::stdout()
}
