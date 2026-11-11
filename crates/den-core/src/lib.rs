//! The work den does on a folder, without a window: git, search, watching
//! the disk, the shells in its terminals and the agents reporting from them.
//! den runs it for the folders on its own machine; den-server runs it on a
//! server, for den to reach over SSH.

pub mod agent;
pub mod dirs;
pub mod fs;
pub mod git;
#[cfg(unix)]
pub mod login_path;
pub mod osc;
pub mod process;
pub mod remote;
pub mod search;
pub mod shell;
pub mod watch;
pub mod wsl;
