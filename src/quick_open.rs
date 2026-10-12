//! Quick Open (Ctrl+P) and the command palette (Ctrl+Shift+P), one dialog as
//! in VS Code: a query starting with `>` lists the commands, any other the
//! folder's files, fuzzy matched on the name first and then on the whole
//! path. The files are walked as the search walks them (ignore files apply)
//! each time it opens.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _,
    command::{Command as Palette, CommandItem, CommandState},
    h_flex,
};
use gpui_kit::*;

use crate::workspace::Workspace;

/// The walk stops after this many files.
const MAX_FILES: usize = 200_000;
/// Rows listed at once; typing narrows them.
const MAX_SHOWN: usize = 50;

/// A file under the root.
struct File {
    /// Relative, `/`-separated; the whole path for an open file outside the root.
    rel: String,
    lower: String,
    /// Where the name starts in `lower`.
    name: usize,
}

impl File {
    fn new(rel: String) -> Self {
        let lower = rel.to_lowercase();
        let name = lower.rfind('/').map_or(0, |i| i + 1);
        File { rel, lower, name }
    }
}

/// A command, as the palette lists it.
struct Cmd {
    title: SharedString,
    /// Its first keys, as the platform shows them.
    keys: Option<String>,
    action: Box<dyn Action>,
}

#[derive(Clone)]
enum Row {
    /// A path as in `File::rel`.
    File(String),
    /// An index into `Model::commands`.
    Command(usize),
}

#[derive(Default)]
struct Model {
    /// The open editors' files, in layout order: what an empty query lists first.
    open: Vec<String>,
    /// Sorted by path once the walk is done; empty until then.
    files: Vec<File>,
    /// The files are still being walked.
    walking: bool,
    commands: Vec<Cmd>,
    query: String,
    shown: Vec<Row>,
}

impl Model {
    /// The query asks for commands.
    fn commands_mode(&self) -> bool {
        self.query.starts_with('>')
    }

    fn refilter(&mut self) {
        if let Some(query) = self.query.strip_prefix('>') {
            // As the palette always matched: the title or keys containing it.
            let query = query.trim().to_lowercase();
            self.shown = (self.commands.iter().enumerate())
                .filter(|(_, c)| {
                    c.title.to_lowercase().contains(&query)
                        || c.keys
                            .as_ref()
                            .is_some_and(|k| k.to_lowercase().contains(&query))
                })
                .map(|(ix, _)| Row::Command(ix))
                .collect();
            return;
        }
        let query: Vec<char> = self
            .query
            .to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| if c == '\\' { '/' } else { c })
            .collect();
        self.shown = if query.is_empty() {
            let mut shown = self.open.clone();
            shown.extend(
                self.files
                    .iter()
                    .filter(|f| !self.open.contains(&f.rel))
                    .map(|f| f.rel.clone()),
            );
            shown.truncate(MAX_SHOWN);
            shown.into_iter().map(Row::File).collect()
        } else {
            let mut hits: Vec<(i64, &File)> = self
                .files
                .iter()
                .filter_map(|f| Some((score(&query, f)?, f)))
                .collect();
            // Best first; the shorter path between equals. Stable, so the
            // path order breaks what ties remain.
            hits.sort_by_key(|(score, f)| (-score, f.rel.len()));
            hits.into_iter()
                .take(MAX_SHOWN)
                .map(|(_, f)| Row::File(f.rel.clone()))
                .collect()
        };
    }
}

/// How well `query` (lowercase, no spaces) matches `file`: a match in the name
/// beats one across the path, and a run of the query in the name beats
/// scattered letters. None when its letters are not all there, in order.
fn score(query: &[char], file: &File) -> Option<i64> {
    let name = &file.lower[file.name..];
    if let Some(score) = subsequence(query, name) {
        let typed: String = query.iter().collect();
        let run = if name.starts_with(&typed) {
            200
        } else if name.contains(&typed) {
            100
        } else {
            0
        };
        return Some(1000 + run + score);
    }
    subsequence(query, &file.lower)
}

/// `query`'s letters found in order in `text`: one point each, more when one
/// follows the last or starts a word. None when they are not all there.
fn subsequence(query: &[char], text: &str) -> Option<i64> {
    let mut want = query.iter().peekable();
    let (mut score, mut prev, mut run) = (0, None::<char>, false);
    for c in text.chars() {
        let Some(&&next) = want.peek() else { break };
        if c == next {
            want.next();
            score += if run { 5 } else { 1 };
            if prev.is_none_or(|p| !p.is_alphanumeric()) {
                score += 6;
            }
        }
        run = c == next;
        prev = Some(c);
    }
    want.peek().is_none().then_some(score)
}

/// Open the dialog over `root`'s files, `open` (the open editors' files)
/// first; on the commands when `commands`.
pub fn open(
    root: PathBuf,
    open: Vec<PathBuf>,
    commands: bool,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let model = Rc::new(RefCell::new(Model {
        open: open.iter().map(|p| rel(&root, p)).collect(),
        walking: true,
        commands: crate::keymap::commands(cx)
            .iter()
            .map(|c| Cmd {
                title: c.title.clone().into(),
                keys: c.shown.first().cloned(),
                action: c.action.boxed_clone(),
            })
            .collect(),
        query: if commands { ">" } else { "" }.to_string(),
        ..Model::default()
    }));
    model.borrow_mut().refilter();
    let state = cx.new(|cx| CommandState::new(window, cx));
    state.update(cx, |state, cx| {
        state.set_query(model.borrow().query.clone(), window, cx);
        state.set_loading(!commands, window, cx);
    });

    window
        .spawn(cx, {
            let (root, model, state) = (root.clone(), model.clone(), state.clone());
            async move |cx| {
                let walked = cx
                    .background_executor()
                    .spawn(async move {
                        let mut files = crate::backend::search::files(&root, MAX_FILES);
                        files.sort_unstable();
                        files.into_iter().map(File::new).collect::<Vec<_>>()
                    })
                    .await;
                _ = cx.update(|window, cx| {
                    let mut model = model.borrow_mut();
                    model.files = walked;
                    model.walking = false;
                    model.refilter();
                    state.update(cx, |state, cx| state.set_loading(false, window, cx));
                    window.refresh();
                });
            }
        })
        .detach();

    window.open_dialog(cx, {
        let state = state.clone();
        move |dialog, _, cx| {
            let items = {
                let model = model.borrow();
                let muted = cx.theme().muted_foreground;
                model
                    .shown
                    .iter()
                    .map(|row| match row {
                        Row::File(rel) => file_item(rel),
                        Row::Command(ix) => command_item(&model.commands[*ix], muted),
                    })
                    .collect::<Vec<_>>()
            };
            let shown = model.borrow().shown.clone();
            let (root, workspace, model, on_query) = (
                root.clone(),
                workspace.clone(),
                model.clone(),
                model.clone(),
            );
            let loading = state.clone();
            dialog
                .w(px(640.))
                .margin_top(px(48.))
                .close_button(false)
                .child(
                    Palette::new(&state)
                        .items(items)
                        .filterable(false)
                        .placeholder("Search files by name, or type > for commands")
                        .bordered(false)
                        .on_query(move |query, window, cx| {
                            let mut model = on_query.borrow_mut();
                            model.query = query.to_string();
                            model.refilter();
                            // The spinner is the files'.
                            let spin = model.walking && !model.commands_mode();
                            loading.update(cx, |state, cx| state.set_loading(spin, window, cx));
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let Some(row) = shown.get(ix.row) else {
                                return;
                            };
                            window.close_dialog(cx);
                            match row {
                                Row::File(rel) => {
                                    let (path, workspace) = (root.join(rel), workspace.clone());
                                    // Focused once the dialog has handed focus back.
                                    window.defer(cx, move |window, cx| {
                                        _ = workspace.update(cx, |workspace, cx| {
                                            workspace.open_file(path, false, window, cx)
                                        });
                                    });
                                }
                                Row::Command(ix) => {
                                    let action = model.borrow().commands[*ix].action.boxed_clone();
                                    // Where the keys would have sent it, once the dialog is gone.
                                    window.defer(cx, move |window, cx| {
                                        if let Some(focus) =
                                            crate::workspace::action_target(window, cx)
                                        {
                                            focus.dispatch_action(&*action, window, cx);
                                        }
                                    });
                                }
                            }
                        })
                        .on_cancel(|window, cx| window.close_dialog(cx)),
                )
        }
    });
    window.defer(cx, move |window, cx| {
        state.update(cx, |state, cx| state.focus(window, cx))
    });
}

/// A file's row: its icon, its name, and the folder it is in.
fn file_item(rel: &str) -> CommandItem {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let (dir, name) = (SharedString::from(dir.to_string()), name.to_string());
    CommandItem::new()
        .label(rel.to_string())
        .child(move |_, cx| {
            h_flex()
                .w_full()
                .gap_2()
                .child(crate::file_icon::render(&name, 16., cx))
                .child(div().flex_none().child(name.clone()))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(dir.clone()),
                )
        })
}

/// A command's row: its title, and its keys at the end.
fn command_item(command: &Cmd, muted: Hsla) -> CommandItem {
    let (title, keys) = (command.title.clone(), command.keys.clone());
    CommandItem::new().label(title.clone()).child(move |_, _| {
        h_flex()
            .w_full()
            .gap_4()
            .justify_between()
            .child(div().min_w_0().truncate().child(title.clone()))
            .children(
                keys.clone()
                    .map(|keys| div().flex_none().text_xs().text_color(muted).child(keys)),
            )
    })
}

/// `path` as `File::rel` has it.
fn rel(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{File, Model, Row};

    fn shown(files: &[&str], query: &str) -> Vec<String> {
        let mut model = Model {
            files: files.iter().map(|f| File::new(f.to_string())).collect(),
            query: query.to_string(),
            ..Model::default()
        };
        model.refilter();
        (model.shown.into_iter())
            .filter_map(|row| match row {
                Row::File(rel) => Some(rel),
                Row::Command(_) => None,
            })
            .collect()
    }

    #[test]
    fn names_beat_paths_and_runs_beat_scatter() {
        let files = [
            "src/workspace.rs",
            "src/backend/watch.rs",
            "src/work/space.rs",
            "docs/networking.md",
        ];
        assert_eq!(shown(&files, "ws")[0], "src/workspace.rs");
        assert_eq!(shown(&files, "work")[0], "src/workspace.rs");
        assert_eq!(shown(&files, "back wat")[0], "src/backend/watch.rs");
        assert_eq!(shown(&files, "src\\work/sp")[0], "src/work/space.rs");
        assert!(shown(&files, "zz").is_empty());
    }

    #[test]
    fn a_leading_angle_bracket_lists_the_commands() {
        let command = |title: &str, keys: &str| super::Cmd {
            title: title.to_string().into(),
            keys: Some(keys.to_string()),
            action: Box::new(crate::Quit),
        };
        let mut model = Model {
            files: vec![File::new("src/quit.rs".into())],
            commands: vec![
                command("den: Quit", "Alt+F4"),
                command("File: Save", "Ctrl+S"),
            ],
            query: "> quit".into(),
            ..Model::default()
        };
        model.refilter();
        assert!(matches!(model.shown[..], [Row::Command(0)]));
        model.query = ">ctrl+s".into();
        model.refilter();
        assert!(matches!(model.shown[..], [Row::Command(1)]));
        model.query = "quit".into();
        model.refilter();
        assert!(matches!(&model.shown[..], [Row::File(rel)] if rel == "src/quit.rs"));
    }
}
