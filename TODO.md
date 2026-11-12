# To do

- [ ] Remote sessions: open a folder on a server over SSH, the whole of den running there (den-server)
  - [x] den-server: files, git, search, watching, and terminals that outlive the connection (`crates/den-core/src/remote`, `crates/den-server`)
  - [x] den: Open Remote Folder…, `den ssh://host/path`, explorer, editor, source control, search and replace, Quick Open, terminals with agent alerts, reconnecting by itself
  - [x] Releases carry den-server for Linux x86_64 and aarch64 (`build.rs`, `release.yml`)
  - [x] Tried over real SSH from Linux (2026-11-11): install, edit and save, a terminal that outlives den and comes back
  - [x] Drop the `DEN_REMOTE=1` switch (`remote::enabled`)
  - [ ] Try it from Windows (the `\\.\den-ssh\` paths and `ssh.exe` have never run) and macOS
  - [ ] Terminals left running on a server with no tab for them: list them, attach or end them
  - [ ] File paths in a server terminal's output as links; the branch menu asks git on the UI thread
  - [ ] Show `host:/path` wherever a server path is shown (audit `.display()`); a clippy rule against `std::fs` on folder paths
  - [ ] Later: passwords and passphrases (askpass), port forwarding and the dev-server button, Format Document, PDF, pictures and audio, extensions, macOS servers
