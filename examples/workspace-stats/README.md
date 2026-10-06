# Workspace Stats

A fuller example than `hello-extension`, showing what a real extension needs beyond `activate`. For each folder den opens it toasts something like

> den: 412 files (Rust 81%, Markdown 12%, TOML 4%) · opened 7 times, last 2 days ago

and every 90 minutes it suggests a break.

What it shows, all in `src/lib.rs`:

- **Settings**: declared in `extension.json` and set on the extension's page in den (click it in the Extensions view). `Context::settings` becomes a `Config` struct through `serde_json::from_value`; `settings_changed` sends a new one to the worker, so a change applies at once (a new break interval counts from then).
- **State across restarts and updates**: `history.json` in `Context::data_dir` (`%APPDATA%\den\extensions-data\workspace-stats`), written through a temporary file so quitting never leaves half of one.
- **Slow work off the event thread**: den hands an extension its events one at a time on the extension's thread, so `event` only queues the folder for a worker thread, which scans it, keeps the history and times the breaks (`recv_timeout` doubles as the timer).
- **`Host` from another thread**: it is `Send + Sync` and `Copy`, so the worker toasts and logs directly. `host.call("info", …)` reaches a host method that has no helper.
- **A quick `deactivate`**: den waits about a second when it quits, so a stop flag makes a scan halfway through a big folder give up, and the worker is joined.
- **Tests** for everything that doesn't need den: `cargo test -p workspace-stats`.

## Settings

| Setting | Default | |
| --- | --- | --- |
| Scan folders | on | Count each opened folder's files and languages. |
| File limit | 50000 | Stop counting after this many files. |
| Skipped folders | `.git, target, node_modules, dist, build, .venv` | Folder names never looked into. |
| Break reminder | 90 | Minutes between reminders; 0 turns them off. |

## Try it

```sh
cargo build --release -p workspace-stats
```

Copy `extension.json` and `target/release/workspace_stats.dll` into `%APPDATA%\den\extensions\workspace-stats\` and restart den. Building, side-loading and publishing work as in [`hello-extension`](../hello-extension/README.md).
