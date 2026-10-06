//! The tasks of `.vscode/tasks.json` as buttons in den's title bar, as
//! actboy168's Tasks extension shows them in VS Code's status bar. A click runs
//! the task in a new terminal tab.
//!
//! It reads the same settings, under each task's `options.statusbar`:
//! `hide`, `label` (with `$(icon)` names), `color`, `detail` (the tooltip) and
//! `filePattern` (shown only while the active file's path matches).
//!
//! What it shows of the SDK: `Host::set_buttons` for each folder den opens,
//! `events::BUTTON_CLICKED` back, and `Host::run_in_terminal`; two commands
//! in den's menu (Reload Tasks, and Open tasks.json with `Host::open_file`); a `choice`
//! setting (the package manager for npm tasks) read from `Context::settings`
//! and `events::SETTINGS_CHANGED`; a worker thread that owns the state, takes
//! the events over a channel and notices edits to `tasks.json` by polling
//! between them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use den_extension::{Button, Context, Extension, Host, events, register};
use serde::Deserialize;
use serde_json::Value;

/// How often `tasks.json` is looked at for changes.
const POLL: Duration = Duration::from_secs(2);
/// How deep `dependsOn` is followed, against cycles.
const MAX_DEPTH: usize = 8;
/// The `package_manager` setting's "by the lock file".
const AUTO: &str = "auto";

// -- tasks.json ------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TasksFile {
    tasks: Vec<Task>,
    /// Defaults for every task.
    options: Options,
    windows: Option<Platform>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Task {
    label: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    command: Option<Arg>,
    args: Vec<Arg>,
    /// `npm` tasks: the package.json script.
    script: Option<String>,
    options: Options,
    detail: Option<String>,
    windows: Option<Platform>,
    depends_on: DependsOn,
    /// `sequence`, else the dependencies run side by side.
    depends_order: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Options {
    cwd: Option<String>,
    statusbar: StatusBar,
}

/// vscode-tasks' `options.statusbar`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct StatusBar {
    hide: bool,
    label: Option<String>,
    color: Option<String>,
    detail: Option<String>,
    file_pattern: Option<String>,
}

/// What `windows` overrides on Windows.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Platform {
    command: Option<Arg>,
    args: Option<Vec<Arg>>,
    options: Option<Options>,
}

/// A command or argument: a string, or `{ "value", "quoting" }`.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum Arg {
    Plain(String),
    Quoted { value: String },
}

impl Arg {
    fn value(&self) -> &str {
        match self {
            Arg::Plain(value) | Arg::Quoted { value } => value,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(untagged)]
enum DependsOn {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl DependsOn {
    fn labels(&self) -> Vec<&str> {
        match self {
            DependsOn::None => Vec::new(),
            DependsOn::One(label) => vec![label],
            DependsOn::Many(labels) => labels.iter().map(String::as_str).collect(),
        }
    }
}

impl Task {
    /// Its label; an npm task without one is named as VS Code names it.
    fn name(&self) -> Option<String> {
        self.label.clone().or_else(|| self.script.as_ref().map(|s| format!("npm: {s}")))
    }

    /// With `windows` applied, on Windows.
    fn for_platform(&self) -> Task {
        let mut task = self.clone();
        if cfg!(windows)
            && let Some(platform) = &self.windows
        {
            task.command = platform.command.clone().or(task.command);
            task.args = platform.args.clone().unwrap_or(task.args);
            if let Some(options) = &platform.options {
                task.options.cwd = options.cwd.clone().or(task.options.cwd);
            }
        }
        task
    }
}

/// The tasks of `root`, from `.vscode/tasks.json`.
struct Tasks {
    root: PathBuf,
    file: TasksFile,
    /// The `package_manager` setting: what runs npm tasks, or `auto`.
    package_manager: String,
}

impl Tasks {
    fn parse(root: &Path, text: &str) -> Result<Self, String> {
        let file = serde_json::from_str(&strip_jsonc(text)).map_err(|e| e.to_string())?;
        Ok(Tasks { root: root.to_path_buf(), file, package_manager: AUTO.into() })
    }

    fn find(&self, label: &str) -> Option<&Task> {
        self.file.tasks.iter().find(|t| t.name().as_deref() == Some(label))
    }

    /// One button per task that has a label and isn't hidden.
    fn buttons(&self) -> Vec<Button> {
        self.file
            .tasks
            .iter()
            .filter(|task| !task.options.statusbar.hide)
            .filter_map(|task| {
                let name = task.name()?;
                let bar = &task.options.statusbar;
                let (icon, label) = split_icon(bar.label.as_deref().unwrap_or(&name));
                let tooltip = bar.detail.clone().or_else(|| task.detail.clone()).unwrap_or_else(|| format!("Run Task: {name}"));
                Some(Button {
                    id: name,
                    label,
                    tooltip,
                    icon,
                    color: bar.color.clone().unwrap_or_default(),
                    file_pattern: bar.file_pattern.clone().unwrap_or_default(),
                })
            })
            .collect()
    }

    /// What clicking `label` runs: each a command line and its folder, side
    /// by side in terminals of their own.
    fn runs(&self, label: &str) -> Result<Vec<(String, PathBuf)>, String> {
        let task = self.find(label).ok_or_else(|| format!("no task \"{label}\""))?;
        let mut runs = Vec::new();
        self.collect(task, 0, &mut runs)?;
        if runs.is_empty() {
            return Err(format!("the task \"{label}\" runs nothing"));
        }
        Ok(runs)
    }

    fn collect(&self, task: &Task, depth: usize, runs: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("dependsOn goes too deep (a cycle?)".into());
        }
        let sequence = task.depends_order.as_deref() == Some("sequence");
        let mut steps = Vec::new();
        for dependency in task.depends_on.labels() {
            let dependency = self.find(dependency).ok_or_else(|| format!("dependsOn names no task \"{dependency}\""))?;
            self.collect(dependency, depth + 1, if sequence { &mut steps } else { runs })?;
        }
        steps.extend(self.command(task));
        // A sequence goes to one terminal, one step after another.
        if let Some((_, cwd)) = steps.first().cloned() {
            let mut here = &cwd;
            let mut lines = Vec::new();
            for (line, dir) in &steps {
                if dir != here {
                    lines.push(format!("cd \"{}\"", dir.display()));
                    here = dir;
                }
                lines.push(line.clone());
            }
            runs.push((lines.join("; "), cwd));
        }
        Ok(())
    }

    /// The task's own command line and folder, if it has a command.
    fn command(&self, task: &Task) -> Option<(String, PathBuf)> {
        let task = task.for_platform();
        let mut words = match (task.kind.as_deref(), &task.command, &task.script) {
            (_, Some(command), _) => vec![self.resolve(command.value())],
            (Some("npm"), None, Some(script)) => {
                let pm = if self.package_manager == AUTO { package_manager(&self.root) } else { &self.package_manager };
                vec![pm.to_string(), "run".into(), script.clone()]
            }
            _ => return None,
        };
        words.extend(task.args.iter().map(|arg| quote(&self.resolve(arg.value()))));
        let cwd = task.options.cwd.or_else(|| self.top_options().cwd).map_or_else(|| self.root.clone(), |cwd| self.root.join(self.resolve(&cwd)));
        Some((words.join(" "), cwd))
    }

    fn top_options(&self) -> Options {
        let mut options = self.file.options.clone();
        if cfg!(windows)
            && let Some(cwd) = self.file.windows.as_ref().and_then(|w| w.options.as_ref()).and_then(|o| o.cwd.clone())
        {
            options.cwd = Some(cwd);
        }
        options
    }

    /// VS Code's `${…}` variables that make sense without an editor; the rest
    /// stay as they are.
    fn resolve(&self, text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let Some(end) = rest[start..].find('}') else { break };
            let name = &rest[start + 2..start + end];
            let root = self.root.to_string_lossy();
            let value = match name {
                "workspaceFolder" | "workspaceRoot" | "cwd" => Some(root.into_owned()),
                "workspaceFolderBasename" => self.root.file_name().map(|n| n.to_string_lossy().into_owned()),
                "pathSeparator" | "/" => Some(std::path::MAIN_SEPARATOR.to_string()),
                _ => name.strip_prefix("env:").map(|var| std::env::var(var).unwrap_or_default()),
            };
            out.push_str(value.as_deref().unwrap_or(&rest[start..start + end + 1]));
            rest = &rest[start + end + 1..];
        }
        out.push_str(rest);
        out
    }
}

/// `$(beaker) test` → (`beaker` as a Lucide icon, `test`). Only the first
/// icon is kept; any others are dropped from the label.
fn split_icon(label: &str) -> (String, String) {
    let mut icon = String::new();
    let mut text = String::new();
    let mut rest = label;
    while let Some(start) = rest.find("$(") {
        let Some(end) = rest[start..].find(')') else { break };
        text.push_str(&rest[..start]);
        if icon.is_empty() {
            icon = lucide(&rest[start + 2..start + end]).to_string();
        }
        rest = &rest[start + end + 1..];
    }
    text.push_str(rest);
    (icon, text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// VS Code's codicon names for the Lucide icons den has, where they differ.
/// Others are passed on and show if Lucide has one by that name.
fn lucide(codicon: &str) -> &str {
    let codicon = codicon.split('~').next().unwrap_or(codicon);
    match codicon {
        "run" | "debug-start" | "run-all" => "play",
        "debug-stop" | "stop" | "stop-circle" => "square",
        "debug" | "debug-alt" => "bug",
        "debug-restart" | "sync" | "refresh" => "refresh-cw",
        "gear" | "settings-gear" => "settings",
        "tools" => "wrench",
        "home" => "house",
        "trash" => "trash",
        "watch" => "eye",
        "beaker" => "flask-conical",
        "testing-run-icon" | "test-view-icon" => "list-checks",
        "cloud-upload" => "cloud-upload",
        "check-all" => "check-check",
        other => other,
    }
}

/// npm, or the package manager whose lock file `root` has.
fn package_manager(root: &Path) -> &'static str {
    [("pnpm-lock.yaml", "pnpm"), ("yarn.lock", "yarn"), ("bun.lock", "bun"), ("bun.lockb", "bun")]
        .iter()
        .find(|(lock, _)| root.join(lock).is_file())
        .map_or("npm", |(_, pm)| pm)
}

/// An argument with spaces in double quotes, as both PowerShell and sh read it.
fn quote(arg: &str) -> String {
    if arg.contains(char::is_whitespace) && !arg.contains('"') { format!("\"{arg}\"") } else { arg.to_string() }
}

/// JSON with the comments and trailing commas VS Code allows, made plain.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                while chars.next_if(|&c| c != '\n').is_some() {}
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            _ => out.push(c),
        }
    }
    // Trailing commas: a comma whose next non-space is `}` or `]`, outside
    // strings (comments are gone by now).
    let chars: Vec<char> = out.chars().collect();
    let mut result = String::with_capacity(out.len());
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            result.push(c);
            if c == '\\' && i + 1 < chars.len() {
                i += 1;
                result.push(chars[i]);
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            result.push(c);
        } else if !(c == ',' && chars[i + 1..].iter().find(|c| !c.is_whitespace()).is_some_and(|n| *n == '}' || *n == ']')) {
            result.push(c);
        }
        i += 1;
    }
    result
}

// -- The extension -----------------------------------------------------------------

enum Job {
    Opened(String),
    Clicked { root: String, id: String },
    /// The `package_manager` setting changed.
    PackageManager(String),
    /// One of its commands ran in the window on `root`.
    Command { root: String, id: String },
}

/// The `package_manager` setting from den's values.
fn package_manager_setting(settings: &Value) -> String {
    settings["package_manager"].as_str().unwrap_or(AUTO).to_string()
}

/// A folder den has a window on, with its tasks as last read.
struct Folder {
    /// When `tasks.json` last changed, `None` when there is none.
    stamp: Option<(SystemTime, u64)>,
    tasks: Option<Tasks>,
}

struct TaskButtons {
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Extension for TaskButtons {
    fn activate(host: Host, context: Context) -> Self {
        let (jobs, rx) = mpsc::channel();
        let pm = package_manager_setting(&Value::Object(context.settings));
        match std::thread::Builder::new().name("task-buttons".into()).spawn(move || run(host, pm, rx)) {
            Ok(thread) => TaskButtons { jobs: Some(jobs), thread: Some(thread) },
            Err(err) => {
                host.log(format!("cannot start: {err}"));
                TaskButtons { jobs: None, thread: None }
            }
        }
    }

    fn event(&mut self, name: &str, data: Value) {
        let text = |key: &str| data[key].as_str().map(str::to_string);
        let job = match name {
            events::WORKSPACE_OPENED => text("root").map(Job::Opened),
            events::BUTTON_CLICKED => text("root").zip(text("id")).map(|(root, id)| Job::Clicked { root, id }),
            events::SETTINGS_CHANGED => Some(Job::PackageManager(package_manager_setting(&data["settings"]))),
            events::COMMAND => text("root").zip(text("id")).map(|(root, id)| Job::Command { root, id }),
            _ => None,
        };
        if let (Some(jobs), Some(job)) = (&self.jobs, job) {
            _ = jobs.send(job);
        }
    }

    fn deactivate(&mut self) {
        // Dropping the sender ends the worker at its next wake-up, within `POLL`.
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take() {
            _ = thread.join();
        }
    }
}

register!(TaskButtons);

fn run(host: Host, mut package_manager: String, jobs: Receiver<Job>) {
    // By root, exactly as den named it, which is how den matches it back.
    let mut folders: BTreeMap<String, Folder> = BTreeMap::new();
    loop {
        match jobs.recv_timeout(POLL) {
            Ok(Job::Opened(root)) => {
                folders.entry(root.clone()).or_insert(Folder { stamp: None, tasks: None });
                // Read it now even when nothing changed: a second window on
                // the same folder needs its buttons too.
                refresh(&host, &root, folders.get_mut(&root).unwrap(), true);
            }
            Ok(Job::PackageManager(pm)) => package_manager = pm,
            Ok(Job::Command { root, id }) => match id.as_str() {
                "reload" => {
                    let folder = folders.entry(root.clone()).or_insert(Folder { stamp: None, tasks: None });
                    refresh(&host, &root, folder, true);
                }
                "open-tasks" => {
                    let path = tasks_json(&root);
                    if path.is_file() {
                        _ = host.open_file(&root, &path.to_string_lossy(), None);
                    } else {
                        host.toast(format!("{root} has no .vscode/tasks.json"));
                    }
                }
                _ => {}
            },
            Ok(Job::Clicked { root, id }) => {
                let Some(tasks) = folders.get_mut(&root).and_then(|f| f.tasks.as_mut()) else { continue };
                tasks.package_manager = package_manager.clone();
                match tasks.runs(&id) {
                    Ok(runs) => {
                        for (command, cwd) in runs {
                            host.log(format!("run {command} in {}", cwd.display()));
                            if let Err(err) = host.run_in_terminal(&root, &command, Some(&cwd.to_string_lossy())) {
                                host.toast(format!("Cannot run {id}: {err}"));
                            }
                        }
                    }
                    Err(err) => host.toast(format!("Cannot run {id}: {err}")),
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                for (root, folder) in &mut folders {
                    refresh(&host, root, folder, false);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn tasks_json(root: &str) -> PathBuf {
    Path::new(root).join(".vscode").join("tasks.json")
}

/// Read `root`'s tasks again if `tasks.json` changed (or `force`), and show
/// their buttons.
fn refresh(host: &Host, root: &str, folder: &mut Folder, force: bool) {
    let path = tasks_json(root);
    let stamp = std::fs::metadata(&path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
    if !force && stamp == folder.stamp {
        return;
    }
    folder.stamp = stamp;
    folder.tasks = match std::fs::read_to_string(&path) {
        Err(_) => None,
        Ok(text) => match Tasks::parse(Path::new(root), &text) {
            Ok(tasks) => Some(tasks),
            Err(err) => {
                host.toast(format!(".vscode/tasks.json: {err}"));
                None
            }
        },
    };
    let buttons = folder.tasks.as_ref().map(Tasks::buttons).unwrap_or_default();
    host.log(format!("{root}: {} task buttons", buttons.len()));
    if let Err(err) = host.set_buttons(root, &buttons) {
        host.log(format!("set_buttons: {err}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = if cfg!(windows) { r"C:\code\app" } else { "/code/app" };

    fn tasks(text: &str) -> Tasks {
        Tasks::parse(Path::new(ROOT), text).unwrap()
    }

    #[test]
    fn reads_jsonc() {
        let text = r#"{
            // a comment
            "version": "2.0.0", /* another */
            "tasks": [
                { "label": "a // not a comment", "command": "echo", },
            ],
        }"#;
        assert_eq!(tasks(text).file.tasks[0].name().unwrap(), "a // not a comment");
    }

    #[test]
    fn makes_buttons_as_vscode_tasks_does() {
        let t = tasks(
            r##"{ "tasks": [
                { "label": "build", "command": "cargo build", "detail": "Build it" },
                { "label": "test", "command": "cargo test",
                  "options": { "statusbar": { "label": "$(beaker) ts", "color": "#22C1D6", "detail": "Run the tests", "filePattern": "test_.*" } } },
                { "label": "secret", "command": "x", "options": { "statusbar": { "hide": true } } },
                { "type": "npm", "script": "dev" },
                { "command": "no label" }
            ] }"##,
        );
        let buttons = t.buttons();
        assert_eq!(buttons.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["build", "test", "npm: dev"]);
        assert_eq!((buttons[0].label.as_str(), buttons[0].tooltip.as_str(), buttons[0].icon.as_str()), ("build", "Build it", ""));
        let test = &buttons[1];
        assert_eq!((test.label.as_str(), test.icon.as_str(), test.color.as_str()), ("ts", "flask-conical", "#22C1D6"));
        assert_eq!((test.tooltip.as_str(), test.file_pattern.as_str()), ("Run the tests", "test_.*"));
        assert_eq!(buttons[2].tooltip, "Run Task: npm: dev");
    }

    #[test]
    fn builds_command_lines() {
        let t = tasks(
            r#"{ "options": { "cwd": "${workspaceFolder}/web" }, "tasks": [
                { "label": "a", "command": "cargo", "args": ["run", "--", "two words", { "value": "q", "quoting": "strong" }] },
                { "label": "b", "command": "echo ${workspaceFolderBasename} ${input:x}", "options": { "cwd": "sub" } },
                { "label": "c", "command": "unix", "windows": { "command": "win" } }
            ] }"#,
        );
        let root = Path::new(ROOT);
        assert_eq!(t.runs("a").unwrap(), [("cargo run -- \"two words\" q".to_string(), root.join(format!("{ROOT}/web")))]);
        assert_eq!(t.runs("b").unwrap(), [("echo app ${input:x}".to_string(), root.join("sub"))]);
        assert_eq!(t.runs("c").unwrap()[0].0, if cfg!(windows) { "win" } else { "unix" });
        assert!(t.runs("nope").is_err());
    }

    #[test]
    fn follows_depends_on() {
        let t = tasks(
            r#"{ "tasks": [
                { "label": "fmt", "command": "cargo fmt" },
                { "label": "lint", "command": "cargo clippy", "options": { "cwd": "crate" } },
                { "label": "both", "dependsOn": ["fmt", "lint"] },
                { "label": "seq", "dependsOn": ["fmt", "lint"], "dependsOrder": "sequence", "command": "cargo test" },
                { "label": "loop", "dependsOn": "loop", "command": "x" }
            ] }"#,
        );
        let root = Path::new(ROOT);
        let both = t.runs("both").unwrap();
        assert_eq!(both, [("cargo fmt".to_string(), root.to_path_buf()), ("cargo clippy".to_string(), root.join("crate"))]);
        let seq = t.runs("seq").unwrap();
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].0, format!("cargo fmt; cd \"{}\"; cargo clippy; cd \"{}\"; cargo test", root.join("crate").display(), root.display()));
        assert!(t.runs("loop").unwrap_err().contains("too deep"));
    }

    #[test]
    fn npm_tasks_use_the_projects_package_manager() {
        let dir = std::env::temp_dir().join(format!("task-buttons-pm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(package_manager(&dir), "npm");
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(package_manager(&dir), "pnpm");
        let mut t = Tasks::parse(&dir, r#"{ "tasks": [{ "type": "npm", "script": "dev", "args": ["--port", "3000"] }] }"#).unwrap();
        assert_eq!(t.runs("npm: dev").unwrap()[0].0, "pnpm run dev --port 3000");
        t.package_manager = "bun".into();
        assert_eq!(t.runs("npm: dev").unwrap()[0].0, "bun run dev --port 3000");
        _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn splits_icons_from_labels() {
        assert_eq!(split_icon("$(beaker) ts"), ("flask-conical".into(), "ts".into()));
        assert_eq!(split_icon("go $(rocket)"), ("rocket".into(), "go".into()));
        assert_eq!(split_icon("$(sync~spin) watch $(eye)"), ("refresh-cw".into(), "watch".into()));
        assert_eq!(split_icon("plain"), (String::new(), "plain".into()));
    }
}
