# Writing a den extension

The full guide is [docs/extensions.md](../../docs/extensions.md): the manifest, every host method and event, settings, commands, buttons, threads, testing, publishing and getting listed. This page is the short version.

An extension is a Rust `cdylib` built against `den-extension`, plus an `extension.json`. This one shows a toast when den starts and logs each folder den opens.

## Make one

```toml
# Cargo.toml — the crate's name is the extension's id
[package]
name = "my-extension"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
den-extension = { git = "https://github.com/patrickiel/den" }
serde_json = "1"
```

```json
// extension.json
{ "id": "my-extension", "name": "My Extension", "version": "0.1.0", "api": 1,
  "description": "What it does.", "repository": "owner/my-extension", "icon": "icon.svg",
  "settings": [
    { "key": "greeting", "title": "Greeting", "description": "What the toast says.",
      "type": "string", "default": "Hello" }
  ],
  "commands": [
    { "id": "say-hello", "title": "Say Hello", "keybinding": "ctrl-alt-h" }
  ] }
```

Implement `den_extension::Extension` (`activate`, and optionally `event` and `deactivate`) and export it with `den_extension::register!(MyExtension);`, as `src/lib.rs` here does.

## What an extension can do

It runs on a thread of its own inside den, with your permissions, so on its own side it can do anything Rust can (files, network, processes, threads). In den it works through these.

**Declared in `extension.json`**, and shown by den without any UI code:

| Field | What den does with it |
| --- | --- |
| `icon` | A square image in the extension's folder (PNG or SVG, 128 px or more), shown in the Extensions view and on its page |
| `README.md` (next to it) | Shown on the extension's page (click it in the Extensions view) |
| `settings` | Each `boolean`, `number`, `string` or `choice` (with `options`) with a `default`, set on the extension's page and kept in den's `settings.json` |
| `commands` | Listed in den's menu under Extensions, as "Name: Title"; `keybinding` (den's notation, `ctrl-alt-h`, `ctrl-k ctrl-s`) binds keys to it |

**Calls to den**, through the `Host` that `activate` receives, from any thread:

| Method | |
| --- | --- |
| `log(message)` | A line in `%APPDATA%\den\extensions.log` |
| `toast(message)` | A notification in den's window |
| `set_buttons(root, &buttons)` | Buttons in the title bar of the windows on a folder: label, Lucide icon, colour, tooltip, and a `file_pattern` that shows one only for matching files |
| `run_in_terminal(root, command, cwd)` | A terminal tab running a command |
| `open_file(root, path, line)` | A file in an editor tab, at a line |
| `call(method, args)` | Any host method by name, such as `info` (den's version, the extension's id and `data_dir`) |

**Events from den**, to `Extension::event`:

| Event | Data |
| --- | --- |
| `workspace_opened` | `{ root }`: a window opened on a folder |
| `active_file_changed` | `{ root, path }`: the active tab changed; `path` is null when it isn't a file |
| `file_saved` | `{ root, path }` |
| `command` | `{ root, id }`: one of its commands ran (the menu or its keys), in the window on `root` |
| `button_clicked` | `{ root, id }` |
| `settings_changed` | `{ settings }`: every setting's value, after one changed |

`Context` carries den's version, `data_dir` (a folder of the extension's own that survives updates) and `settings`, the values at start. `root` is always a folder exactly as `workspace_opened` named it, which is how den finds the window again.

For more than this, see [`workspace-stats`](../workspace-stats) (settings applied live, a command, saves counted, state across restarts, a worker thread, a clean shutdown) and [`task-buttons`](../task-buttons) (VS Code's `tasks.json` as title-bar buttons that run in a terminal, commands that reload and open it).

## Try it

```sh
cargo build --release
```

Copy `extension.json` and `target/release/my_extension.dll` into `%APPDATA%\den\extensions\my-extension\` (the folder button in the Extensions view opens it), then restart den. The Extensions view lists it as Running or Failed with the reason, and its log button opens `%APPDATA%\den\extensions.log` with its log lines.

## Publish it

Put the extension in its own GitHub repository with `.github/workflows/release.yml` from here. Pushing a tag `v<version>` (matching `extension.json`) creates a release with `<id>-windows-x86_64.zip` and an `extension.json` carrying the zip's SHA-256. Anyone can then install it in den by typing `owner/repo` in the Extensions view (Ctrl+Shift+X), and get new releases from its update button there. To appear under **Available** in everyone's den, open a pull request adding your repository to [patrickiel/den-extensions](https://github.com/patrickiel/den-extensions); its CI checks your release the way den installs it.
