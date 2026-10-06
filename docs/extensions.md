# Writing den extensions

This guide covers writing, testing, publishing and listing an extension for den. The examples in [`examples/`](../examples) are complete, working extensions, so it's worth reading them alongside this.

| Example | What it shows |
| --- | --- |
| [`hello-extension`](../examples/hello-extension) | The minimum: a toast, a setting, and a command with a keybinding that uses the active file |
| [`workspace-stats`](../examples/workspace-stats) | A worker thread, settings applied live, state across restarts, saves counted, a clean shutdown |
| [`task-buttons`](../examples/task-buttons) | Title-bar buttons, running commands in a terminal, opening files, a `choice` setting |

## Contents

1. [How extensions work](#how-extensions-work)
2. [Quick start](#quick-start)
3. [The manifest](#the-manifest-extensionjson)
4. [The extension](#the-extension)
5. [Host methods](#host-methods)
6. [Events](#events)
7. [Settings](#settings)
8. [Commands and keybindings](#commands-and-keybindings)
9. [Title-bar buttons](#title-bar-buttons)
10. [Threads, state and shutdown](#threads-state-and-shutdown)
11. [Testing and debugging](#testing-and-debugging)
12. [Publishing](#publishing)
13. [Getting listed](#getting-listed)
14. [Compatibility](#compatibility)
15. [What extensions can't do yet](#what-extensions-cant-do-yet)

## How extensions work

An extension is a **Rust `cdylib`** (a DLL), built against the [`den-extension`](../crates/den-extension) crate, plus an **`extension.json`** manifest. den loads every folder in `%APPDATA%\den\extensions\<id>\` that holds both.

```
%APPDATA%\den\
  extensions\
    my-extension\
      extension.json      the manifest
      my_extension.dll    the code
      README.md           shown on the extension's page (optional)
      icon.svg            shown in the Extensions view (optional)
  extensions-data\
    my-extension\         the extension's own folder (Context::data_dir)
  extensions.log          every extension's log lines, and how each started
```

How den runs it:
- **Its own thread.** den loads the DLL at start and runs each extension on a thread of its own. It calls `activate`, then hands over events one at a time, and calls `deactivate` when it quits.
- **JSON over a C ABI.** Rust has no stable ABI, so nothing Rust-shaped crosses into den. The boundary is two small tables of `extern "C"` functions, carrying JSON in C strings. `den-extension` wraps all of it: you implement a trait and call methods.
- **UI is described, not drawn.** An extension can't draw den's UI. It describes what it wants (a button, a setting, a command) as data, and den draws it. That is also why an extension keeps working across den updates.
- **Panics are caught.** A panic in `activate` fails the extension, logged with the reason. A panic in `event` or `deactivate` is ignored. Neither ever reaches den.
- **Changes need a restart.** A loaded DLL is never unloaded. Installing, updating, turning on or off, and uninstalling all take effect at den's next start.
- **Trust.** It's native code running with the user's permissions. den asks before installing one from GitHub.

## Quick start

**1. Create the crate.** The crate's name is the extension's id.

```sh
cargo new --lib my-extension
```

```toml
# Cargo.toml
[package]
name = "my-extension"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
den-extension = { git = "https://github.com/patrickiel/den" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

**2. Write the manifest** next to `Cargo.toml`:

```json
{
  "id": "my-extension",
  "name": "My Extension",
  "version": "0.1.0",
  "api": 1,
  "description": "What it does, in one sentence.",
  "commands": [
    { "id": "greet", "title": "Greet", "keybinding": "ctrl-alt-g" }
  ]
}
```

**3. Implement it** in `src/lib.rs`:

```rust
use den_extension::{Context, Extension, Host, events, register};
use serde_json::Value;

struct MyExtension {
    host: Host,
}

impl Extension for MyExtension {
    fn activate(host: Host, context: Context) -> Self {
        host.log(format!("started on den {}", context.den_version));
        MyExtension { host }
    }

    fn event(&mut self, name: &str, data: Value) {
        if name == events::COMMAND && data["id"] == "greet" {
            self.host.toast("Hello from my extension");
        }
    }
}

register!(MyExtension);
```

**4. Build it and side-load it.**

```sh
cargo build --release
```

Copy `extension.json` and `target\release\my_extension.dll` into `%APPDATA%\den\extensions\my-extension\`; the folder button in the Extensions view opens that folder. Then restart den. In den's repository, `.\scripts\sideload.ps1 <folder>` does the build and copy for you.

**5. Try it.** Open the Extensions view (Ctrl+Shift+X): your extension shows as Running, or Failed with the reason. Press Ctrl+Alt+G, or pick *My Extension: Greet* from den's menu.

## The manifest: `extension.json`

| Field | Required | |
| --- | --- | --- |
| `id` | yes | Lowercase letters, digits and `-`, at most 64 characters, not starting or ending with `-`. It must equal the crate's name; the DLL is the id with `-` turned into `_` (`my_extension.dll`). It also names the extension's folders. |
| `name` | yes | Shown everywhere. |
| `version` | yes | `x.y.z`. den compares versions to offer updates. |
| `api` | yes | The extension API it was built for: `den_extension::API_VERSION`, currently `1`. |
| `description` | for listing | One sentence, shown in the Extensions view. The index requires one. |
| `repository` | no | The GitHub `owner/repo` its releases come from. den fills it in when installing from GitHub, and uses it for updates. |
| `icon` | no | A path inside the extension's folder (`icon.svg`, `images/icon.png`). It should be square, PNG or SVG, 128 px or more. Without one, den shows the name's initial on a colour. |
| `settings` | no | See [Settings](#settings). |
| `commands` | no | See [Commands and keybindings](#commands-and-keybindings). |
| `sha256` | no | The SHA-256 of the release zip. `release.yml` adds it to the released copy, and den checks it on install. Leave it out of your source copy. |

A `README.md` next to the manifest is shown on the extension's page, both installed and before installing (from the repository). Write it for users: what the extension does, its settings, and its commands.

## The extension

Implement `den_extension::Extension` and export the type with `register!`:

```rust
pub trait Extension: Sized + 'static {
    fn activate(host: Host, context: Context) -> Self;  // once, at start
    fn event(&mut self, name: &str, data: Value) {}     // each event, in order
    fn deactivate(&mut self) {}                         // den is quitting
}
```

`Context` holds:

| Field | |
| --- | --- |
| `den_version` | den's version, `x.y.z` |
| `data_dir` | `%APPDATA%\den\extensions-data\<id>`, created by den. It survives updates and uninstalls, so keep the extension's state here. |
| `settings` | Every setting's value: the user's where they set one, else its default |

All three methods run on the extension's own thread, one at a time. While `event` runs, the extension's next events wait. Keep it quick and hand slow work to a thread of your own (see [Threads, state and shutdown](#threads-state-and-shutdown)).

## Host methods

`Host` is den, as the extension sees it. It's `Copy`, `Send` and `Sync`, so you can clone it into any thread and call it from there.

| Method | What it does |
| --- | --- |
| `log(message)` | Writes a line to `extensions.log`, under the extension's id |
| `toast(message)` | Shows a notification in den's window, titled with the extension's name |
| `set_buttons(root, &[Button])` | Sets the extension's title-bar buttons for the windows on `root`; see [Title-bar buttons](#title-bar-buttons) |
| `run_in_terminal(root, command, cwd)` | Opens a terminal tab in the window on `root` and runs `command` in its shell (PowerShell by default), in `cwd` or else the root |
| `open_file(root, path, line)` | Opens `path` in an editor tab of the window on `root` (or switches to its tab), at `line` (from 1) if given. A relative path counts from `root`. |
| `call(method, args)` | Calls any host method by name with JSON arguments, such as `info` (no arguments), which returns `{ id, den_version, data_dir, api }` |

Every method takes a **`root`**: the folder of a den window, exactly as den named it in `workspace_opened` (or in any event's `root`). Pass the string back unchanged; den finds the window by it. If no window is open on that root, the call does nothing (it's logged).

The typed methods return `Result<(), String>`. `log` and `toast` ignore errors.

## Events

`event(name, data)` receives these. The names are constants in `den_extension::events`. Ignore names you don't know: newer dens may send more.

| Constant | `name` | `data` | When |
| --- | --- | --- | --- |
| `WORKSPACE_OPENED` | `workspace_opened` | `{ root }` | A window opened on a folder, including the ones restored at start |
| `ACTIVE_FILE_CHANGED` | `active_file_changed` | `{ root, path }` | The active tab of a window changed; `path` is `null` when it isn't a file tab |
| `FILE_SAVED` | `file_saved` | `{ root, path }` | A file was saved in a window |
| `COMMAND` | `command` | `{ root, id }` | One of the extension's commands ran, from den's menu or its keybinding, in the window on `root` |
| `BUTTON_CLICKED` | `button_clicked` | `{ root, id }` | One of its title-bar buttons was clicked |
| `SETTINGS_CHANGED` | `settings_changed` | `{ settings }` | The user changed one of its settings; `settings` holds every value, like `Context::settings` |

## Settings

Declare settings in the manifest. den shows them on the extension's page (click the extension in the Extensions view, then **Settings**), stores the user's values in its own `settings.json`, and hands them to the extension. You write no settings UI and no config file.

```json
"settings": [
  { "key": "enabled",  "title": "Enabled",  "description": "Do the thing.", "type": "boolean", "default": true },
  { "key": "limit",    "title": "Limit",    "description": "At most this many.", "type": "number", "default": 100 },
  { "key": "exclude",  "title": "Excluded", "description": "Names, separated by commas.", "type": "string", "default": "target, node_modules" },
  { "key": "mode",     "title": "Mode",     "description": "How to do it.", "type": "choice", "default": "auto", "options": ["auto", "fast", "thorough"] }
]
```

| `type` | Shown as | Value |
| --- | --- | --- |
| `boolean` | a switch | `true` / `false` |
| `number` | a text box (a non-number keeps the last good value) | a JSON number |
| `string` | a text box | a string |
| `choice` | a dropdown of `options` | one of `options`, as a string |

Every value is in `Context::settings` at start, and again in `settings_changed` whenever one changes. A user value that doesn't fit its setting (the wrong type, or an option that's gone) is replaced by the default before the extension sees it. The simplest way to read them is to deserialize into a struct:

```rust
#[derive(serde::Deserialize)]
#[serde(default)]
struct Config { enabled: bool, limit: u64, exclude: String, mode: String }

impl Default for Config { /* the same defaults as the manifest */ }

fn config(settings: serde_json::Map<String, Value>) -> Config {
    serde_json::from_value(Value::Object(settings)).unwrap_or_default()
}

// In activate:  let config = config(context.settings);
// In event:     if name == events::SETTINGS_CHANGED { if let Value::Object(s) = data["settings"].take() { self.config = config(s); } }
```

Apply a change straight away; the user expects it without a restart. `workspace-stats` shows how to pass a new config to a worker thread.

## Commands and keybindings

```json
"commands": [
  { "id": "show-stats", "title": "Show Workspace Stats" },
  { "id": "say-hello", "title": "Say Hello", "keybinding": "ctrl-alt-h" }
]
```

- den lists each command in its menu under **Extensions** as *Name: Title*, with its keys. A command runs in the window it was picked from, and the extension gets `command { root, id }`.
- `keybinding` uses den's notation: modifiers `ctrl`, `alt`, `shift` and `win`/`cmd` joined to the key with `-` (`ctrl-alt-h`, `shift-f5`), and chords separated by a space (`ctrl-k ctrl-s`).
- A keybinding den can't read is skipped and logged. One that den already uses is taken over while the extension runs, so pick keys den leaves free (`ctrl-alt-…` combinations mostly are).
- `id`s must be unique within the extension, and every command needs a `title`.
- Only running extensions' commands are listed and bound. Commands added in a new version appear after the restart that installs it.

## Title-bar buttons

```rust
use den_extension::Button;

let result = host.set_buttons(&root, &[
    Button { id: "build".into(), label: "Build".into(), icon: "hammer".into(), ..Default::default() },
    Button {
        id: "test".into(),
        label: "Test".into(),
        icon: "flask-conical".into(),
        color: "#22C1D6".into(),
        tooltip: "Run the tests".into(),
        file_pattern: r"test_.*\.rs$".into(),
    },
]);
```

| Field | |
| --- | --- |
| `id` | Comes back in `button_clicked { root, id }` |
| `label` | The text |
| `icon` | A [Lucide](https://lucide.dev/icons) icon's name, shown before the label; an unknown name shows none |
| `color` | `#rrggbb` for the label and icon |
| `tooltip` | Shown on hover; defaults to the label |
| `file_pattern` | A regular expression: the button shows only while the active file's path matches. One that doesn't compile hides the button. |

How they behave:
- Buttons belong to a **folder**: set them per `root`, usually in response to `workspace_opened`. Every window on that folder shows them, to the left of den's layout button.
- Each call **replaces** the extension's buttons for that root, and an empty list removes them. Set them again whenever your data changes; `task-buttons` does this when `tasks.json` changes.

## Threads, state and shutdown

**Keep `event` fast.** Hand anything that takes time (scanning files, network, waiting) to a thread of your own, over a channel:

```rust
struct MyExtension { jobs: Option<Sender<Job>>, thread: Option<JoinHandle<()>> }

fn activate(host: Host, context: Context) -> Self {
    let (jobs, rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new().name("my-extension".into()).spawn(move || run(host, rx)).ok();
    MyExtension { jobs: Some(jobs), thread }
}

fn event(&mut self, name: &str, data: Value) {
    if let (Some(jobs), Some(job)) = (&self.jobs, Job::from_event(name, data)) {
        _ = jobs.send(job);
    }
}

fn deactivate(&mut self) {
    drop(self.jobs.take());           // the worker's recv() ends
    if let Some(thread) = self.thread.take() { _ = thread.join(); }
}
```

- **Timers and polling:** `recv_timeout` on the worker's channel does double duty. It waits for the next job and for the next tick. `workspace-stats` uses it for break reminders, `task-buttons` to notice edits to `tasks.json`.
- **Shutdown is short:** den waits about **one second** for all extensions to deactivate, then quits anyway. Make long work check a stop flag (an `Arc<AtomicBool>`) so `deactivate` can join quickly. `workspace-stats` stops a half-done scan this way.
- **State:** keep it in `Context::data_dir`, which survives updates. Write through a temporary file and rename it, so quitting halfway never leaves half a file.
- **Settings vs state:** what the user chooses belongs in `settings` (den stores it). What the extension learns belongs in `data_dir`.

## Testing and debugging

**Unit tests.** Keep the logic out of the `Extension` impl, in plain functions you can test without den: parsing, deciding what to show, building command lines. `cargo test` works on a `cdylib`. A test can read your manifest with `den_extension::Manifest::parse(include_str!("../extension.json"))` to check that your defaults agree with it (`workspace-stats` does this).

**Side-loading.** Build in release and copy the folder in (see the [Quick start](#quick-start)), or in den's repository run:

```powershell
.\scripts\sideload.ps1 examples\hello-extension   # or a path to your extension
```

This stages it as `<id>.pending`, the same way an install does. den can stay open; restart it to load the new build. A running den keeps its DLL locked, so you can't overwrite it in place.

**Logs.** `extensions.log` (the log button in the Extensions view opens it) records:
- each start: loaded, or failed with the reason (a missing DLL, a wrong API version, a panic in `activate`);
- every `host.log` line;
- den's notes, such as a keybinding it couldn't read or a call for a window that isn't open.

**A separate den for testing.** den keeps everything under `%APPDATA%\den`. Set `APPDATA` to another folder to run a den with its own extensions, settings and state, without touching your own:

```powershell
$env:APPDATA = "C:\temp\den-test"; & "path\to\den.exe" C:\some\project
```

**Testing the Available list.** Set `DEN_EXTENSION_INDEX` to a local `index.json` (or another URL). In a local index, `icon_url` and `readme_url` may be file paths.

## Publishing

Publish each extension from its **own public GitHub repository**, with `extension.json` at its root. Copy [`release.yml`](../examples/hello-extension/.github/workflows/release.yml) into `.github/workflows/`, then:

1. Set `version` in `extension.json` (and in `Cargo.toml`) to the new version.
2. Push a tag `v<version>`, matching `extension.json`, or the workflow fails:
   ```sh
   git tag v0.2.0; git push origin v0.2.0
   ```
3. The workflow builds on Windows and makes a release with:
   - `<id>-windows-x86_64.zip`: `extension.json`, the DLL, and `README.md` and the icon if present;
   - `extension.json` with the zip's `sha256` added. den reads this first.

Anyone can then install the extension by typing `owner/repo` in the Extensions view. Users who installed it get the new release from its update button.

## Getting listed

To appear under **Available** in everyone's den, open a pull request to [`patrickiel/den-extensions`](https://github.com/patrickiel/den-extensions) that adds one line to `extensions.json` and changes nothing else:

```json
{ "repo": "you/my-extension" }
```

CI checks your latest release the way den installs it:
- the manifest is valid and has a description;
- it's built for an API den has;
- the zip is there and `sha256` is set;
- the id isn't taken;
- `repository`, if set, names your repository;
- the icon, if any, is in the repository.

Once a maintainer merges it, the index is rebuilt and den shows your extension within the hour. New releases appear as updates on their own, because the index is rebuilt daily.

Being listed isn't a security audit. Keep your source public, and have it build the released DLL.

## Compatibility

- **The API version** (`api` in the manifest, `den_extension::API_VERSION` in code) changes only when the boundary itself breaks. den loads extensions built for its version and refuses others, with the reason.
- **New features arrive under the same version**, as new host methods, events and manifest fields. An extension built against an older `den-extension` keeps working: it never calls the new methods, and ignores events it doesn't know.
- **An extension that uses a new method** fails in an older den only at that call: `call` returns an error naming the method. Handle that error if you want to support older dens.
- **Only Windows x86-64 for now**: the release asset is `<id>-windows-x86_64.zip`.

## What extensions can't do yet

These are planned, but not available today:
- add a sidebar view or a tab of its own;
- ask the user something (a quick pick or an input box);
- add items to the Explorer's right-click menu;
- decorate the editor (gutter marks, inline text);
- read or edit a file's unsaved contents.

If your extension needs one of these, open an issue on den describing the use case.
