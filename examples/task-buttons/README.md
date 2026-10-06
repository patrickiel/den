# Task Buttons

The tasks in a folder's `.vscode/tasks.json` as buttons in den's title bar, as [actboy168's Tasks extension](https://github.com/actboy168/vscode-tasks) puts them in VS Code's status bar. A click runs the task in a new terminal tab; editing `tasks.json` updates the buttons within two seconds.

It reads the same settings, under each task's `options.statusbar`:

```jsonc
{
  "version": "2.0.0",
  "tasks": [
    { "label": "build", "command": "cargo build" },
    {
      "label": "test",
      "command": "cargo test",
      "options": {
        "statusbar": {
          "label": "$(beaker) ts",       // the button's text, $(icon) names allowed
          "color": "#22C1D6",            // its colour
          "detail": "Run the tests",     // its tooltip (else the task's detail)
          "filePattern": "test_.*"       // only while the active file's path matches
        }
      }
    },
    { "label": "secret", "command": "x", "options": { "statusbar": { "hide": true } } }
  ]
}
```

Also understood: comments and trailing commas, `args` (quoted when they have spaces), `options.cwd` (per task or for all), `windows` overrides, `npm` tasks (run with pnpm, yarn or bun when their lock file is there), `dependsOn` (side by side in terminals of their own, or one after another in one terminal with `"dependsOrder": "sequence"`), and the variables `${workspaceFolder}`, `${workspaceFolderBasename}`, `${env:NAME}`, `${pathSeparator}`. Variables that need an editor or a prompt (`${file}`, `${input:…}`) stay as they are. `$(icon)` names are VS Code's codicons, mapped to the [Lucide](https://lucide.dev/icons) icons den has where the names differ; any Lucide name works too.

## Settings

**Package manager** (`auto`, `npm`, `pnpm`, `yarn`, `bun`): what runs `npm` tasks. `auto` goes by the folder's lock file.

## What it shows

- `Host::set_buttons(root, &buttons)` for each folder den opens (`events::WORKSPACE_OPENED`), and again whenever `tasks.json` changes. den matches `root` against its windows exactly as it sent it.
- `events::BUTTON_CLICKED` (`{ "root", "id" }`) coming back, and `Host::run_in_terminal(root, command, cwd)`.
- A `choice` setting read from `Context::settings` and kept up to date from `events::SETTINGS_CHANGED`.
- A worker thread that owns all the state and takes the events over a channel, with `recv_timeout` doubling as the poll for changes; `deactivate` drops the channel and joins it.
- Tests for the parsing and command lines: `cargo test -p task-buttons`.

## Try it

```sh
cargo build --release -p task-buttons
```

Copy `extension.json` and `target/release/task_buttons.dll` into `%APPDATA%\den\extensions\task-buttons\` and restart den. Building, side-loading and publishing work as in [`hello-extension`](../hello-extension/README.md).
