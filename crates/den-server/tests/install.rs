//! Connecting to a server den-server is not on yet: den puts it there and
//! starts it. The "server" is a folder of this machine as its home, reached
//! through `DEN_REMOTE_COMMAND` in place of ssh. One test, alone in its
//! process: it sets the environment.

#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt as _,
    sync::atomic::{AtomicUsize, Ordering},
};

use den_core::remote::{self, Error, HostId, proto::Request, ssh};

static SENT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn den_server_is_put_on_a_server_once_and_started_there() {
    let home = std::env::temp_dir().join(format!("den-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let home = std::fs::canonicalize(home).unwrap();
    // SAFETY: the only test in this process, before any thread it starts.
    unsafe {
        std::env::set_var(
            ssh::COMMAND_ENV,
            format!(
                "cd '{0}' && HOME='{0}' DEN_SERVER_LOGIN_PATH=0",
                home.display()
            ),
        );
    }
    ssh::set_server(ssh::Server {
        id: "test1234".into(),
        binary: Box::new(|machine| {
            SENT.fetch_add(1, Ordering::Relaxed);
            if machine == "Plan9 mips" {
                return Err("den-server is not built for Plan9 mips.".into());
            }
            assert!(
                machine.starts_with("Linux ") || machine.starts_with("Darwin "),
                "{machine}"
            );
            std::fs::read(env!("CARGO_BIN_EXE_den-server")).map_err(|e| e.to_string())
        }),
    });
    let host = HostId::new("install-test").unwrap();
    let installed = home.join(".den-server/bin/test1234/den-server");
    assert!(!installed.exists());

    let connection = ssh::connect(&host).unwrap();
    assert_eq!(SENT.load(Ordering::Relaxed), 1);
    assert!(installed.is_file());
    assert_eq!(
        installed.metadata().unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::read(&installed).unwrap(),
        std::fs::read(env!("CARGO_BIN_EXE_den-server")).unwrap()
    );
    // Nothing half arrived is left beside it.
    assert_eq!(
        std::fs::read_dir(installed.parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    connection.call(&Request::Ping, Vec::new()).unwrap();
    // `~` there is the server's home.
    assert_eq!(
        den_core::fs::canonicalize_on(&connection, "~").unwrap(),
        remote::join(&host, &home.to_string_lossy())
    );

    // Asked again, it is the same connection; lost, a new one with nothing
    // sent again.
    assert!(std::sync::Arc::ptr_eq(
        &connection,
        &ssh::connect(&host).unwrap()
    ));
    let closed = std::sync::Arc::new(AtomicUsize::new(0));
    connection.on_close({
        let closed = closed.clone();
        move || {
            closed.fetch_add(1, Ordering::Relaxed);
        }
    });
    connection.close();
    assert_eq!(closed.load(Ordering::Relaxed), 1);
    assert!(remote::connection(&host).is_none());
    let again = ssh::connect(&host).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&connection, &again));
    again.call(&Request::Ping, Vec::new()).unwrap();
    assert_eq!(SENT.load(Ordering::Relaxed), 1);
    again.close();

    // What stands in for ssh fails as ssh does: its words, nothing installed.
    unsafe {
        std::env::set_var(
            ssh::COMMAND_ENV,
            "echo 'ssh: connect to host box port 22: No route to host' >&2; exit 255;",
        )
    };
    let other = HostId::new("unreachable").unwrap();
    assert_eq!(
        ssh::connect(&other).err(),
        Some(Error::Start(
            "ssh: connect to host box port 22: No route to host".into()
        ))
    );
    assert_eq!(SENT.load(Ordering::Relaxed), 1);
    assert!(ssh::wants_a_terminal(
        "me@box: Permission denied (publickey,password)."
    ));
    assert!(!ssh::wants_a_terminal(
        "ssh: connect to host box port 22: No route to host"
    ));

    let _ = std::fs::remove_dir_all(&home);
}
