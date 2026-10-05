//! The Explorer: the session root as a lazily read file tree.
//!
//! Folders are read when first expanded and cached until Refresh. Which
//! folders are expanded is remembered per session by the workspace.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{repo::Repo, scm::letter_color};

pub enum ExplorerEvent {
    /// `preview` is a single click: open it, keep focus in the tree.
    Open { path: PathBuf, preview: bool },
    ExpandedChanged,
    /// A terminal starting in this folder.
    OpenTerminal(PathBuf),
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: SharedString,
    is_dir: bool,
}

struct Row {
    entry: Entry,
    depth: usize,
}

pub struct Explorer {
    root: PathBuf,
    repo: Entity<Repo>,
    expanded: HashSet<PathBuf>,
    children: HashMap<PathBuf, Vec<Entry>>,
    rows: Vec<Row>,
    selected: Option<usize>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// The last file operation's failure.
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ExplorerEvent> for Explorer {}

impl Focusable for Explorer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Explorer {
    pub fn new(root: PathBuf, expanded: Vec<PathBuf>, repo: Entity<Repo>, cx: &mut Context<Self>) -> Self {
        let _subscriptions = vec![cx.observe(&repo, |_, _, cx| cx.notify())];
        let mut this = Self {
            root,
            repo,
            _subscriptions,
            expanded: expanded.into_iter().collect(),
            children: HashMap::new(),
            rows: Vec::new(),
            selected: None,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            error: None,
        };
        this.rebuild();
        this
    }

    pub fn expanded(&self) -> Vec<PathBuf> {
        let mut expanded: Vec<_> = self.expanded.iter().cloned().collect();
        expanded.sort();
        expanded
    }

    fn read_children(&mut self, dir: &Path) -> Vec<Entry> {
        if let Some(entries) = self.children.get(dir) {
            return entries.clone();
        }
        let mut entries: Vec<Entry> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|item| {
                let name = item.file_name().to_string_lossy().to_string();
                if name == ".git" {
                    return None;
                }
                let is_dir = item.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
                Some(Entry {
                    path: item.path(),
                    name: name.into(),
                    is_dir,
                })
            })
            .collect();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.children.insert(dir.to_path_buf(), entries.clone());
        entries
    }

    /// Flatten the expanded part of the tree into the rows the list draws.
    fn rebuild(&mut self) {
        let selected = self.selected_path();
        let mut rows = Vec::new();
        let mut stack: Vec<(Entry, usize)> = self
            .read_children(&self.root.clone())
            .into_iter()
            .rev()
            .map(|entry| (entry, 0))
            .collect();
        while let Some((entry, depth)) = stack.pop() {
            if entry.is_dir && self.expanded.contains(&entry.path) {
                for child in self.read_children(&entry.path).into_iter().rev() {
                    stack.push((child, depth + 1));
                }
            }
            rows.push(Row { entry, depth });
        }
        self.rows = rows;
        self.selected = selected.and_then(|path| self.rows.iter().position(|row| row.entry.path == path));
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| row.entry.path.clone())
    }

    fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild();
        cx.emit(ExplorerEvent::ExpandedChanged);
        cx.notify();
    }

    fn activate(&mut self, ix: usize, preview: bool, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        let Some(row) = self.rows.get(ix) else { return };
        let entry = row.entry.clone();
        if entry.is_dir {
            self.toggle(&entry.path, cx);
        } else {
            cx.emit(ExplorerEvent::Open {
                path: entry.path,
                preview,
            });
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.children.clear();
        self.expanded.retain(|path| path.is_dir());
        self.rebuild();
        cx.notify();
    }

    /// Files and folders changed on disk: read their folders again.
    pub fn paths_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let mut stale = false;
        for path in paths {
            for dir in [Some(path.as_path()), path.parent()].into_iter().flatten() {
                stale |= self.children.remove(dir).is_some();
            }
        }
        if stale {
            self.expanded.retain(|path| path.is_dir());
            self.rebuild();
            cx.notify();
        }
    }

    /// Show `path` in the tree: the folders down to it expanded, its row
    /// selected and scrolled to.
    pub fn show_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !path.starts_with(&self.root) {
            return;
        }
        for dir in path.ancestors().skip(1).take_while(|dir| *dir != self.root) {
            self.expanded.insert(dir.to_path_buf());
        }
        self.rebuild();
        if let Some(ix) = self.rows.iter().position(|row| row.entry.path == path) {
            self.select(ix, cx);
        }
        cx.emit(ExplorerEvent::ExpandedChanged);
        cx.notify();
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        self.rebuild();
        cx.emit(ExplorerEvent::ExpandedChanged);
        cx.notify();
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let ix = ix.min(self.rows.len() - 1);
        self.selected = Some(ix);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.selected.unwrap_or(0);
        let row = self.rows.get(current).map(|row| (row.entry.clone(), row.depth));
        match event.keystroke.key.as_str() {
            "down" => self.select(self.selected.map_or(0, |ix| ix + 1), cx),
            "up" => self.select(current.saturating_sub(1), cx),
            "home" => self.select(0, cx),
            "end" => self.select(usize::MAX, cx),
            "enter" => self.activate(current, false, cx),
            "f2" => {
                if let Some((entry, _)) = row {
                    self.prompt_rename(entry.path, window, cx);
                }
            }
            "delete" => {
                if let Some((entry, _)) = row {
                    self.confirm_delete(entry.path, window, cx);
                }
            }
            "right" => {
                if let Some((entry, _)) = row
                    && entry.is_dir
                    && !self.expanded.contains(&entry.path)
                {
                    self.toggle(&entry.path, cx);
                }
            }
            "left" => match row {
                Some((entry, _)) if entry.is_dir && self.expanded.contains(&entry.path) => {
                    self.toggle(&entry.path, cx)
                }
                Some((_, depth)) if depth > 0 => {
                    let parent = self.rows[..current].iter().rposition(|row| row.depth < depth);
                    if let Some(parent) = parent {
                        self.select(parent, cx);
                    }
                }
                _ => {}
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    // -- File operations ---------------------------------------------------

    /// Where New File / New Folder create: the selected folder, or the
    /// selected file's folder, else the root.
    fn target_dir(&self) -> PathBuf {
        match self.selected.and_then(|ix| self.rows.get(ix)) {
            Some(row) if row.entry.is_dir => row.entry.path.clone(),
            Some(row) => row.entry.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        }
    }

    /// The right-click menu of a row.
    fn row_menu(menu: PopupMenu, this: WeakEntity<Self>, path: PathBuf, is_dir: bool) -> PopupMenu {
        type Run = Box<dyn Fn(&mut Explorer, &mut Window, &mut Context<Explorer>)>;
        let dir = if is_dir { path.clone() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
        let act = |label: &'static str, icon: IconName, run: Run| {
            let this = this.clone();
            let run = std::rc::Rc::new(run);
            PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(move |_, window, cx| {
                let run = run.clone();
                _ = this.update(cx, |explorer, cx| run(explorer, window, cx));
            })
        };
        let (d1, d2, d3) = (dir.clone(), dir.clone(), dir);
        let (p1, p2, p3, p4, p5) = (path.clone(), path.clone(), path.clone(), path.clone(), path);
        menu.item(act("New File…", IconName::FilePlus, Box::new(move |e, w, cx| e.prompt_new(d1.clone(), false, w, cx))))
            .item(act("New Folder…", IconName::FolderPlus, Box::new(move |e, w, cx| e.prompt_new(d2.clone(), true, w, cx))))
            .separator()
            .item(act(
                "Open Terminal Here",
                IconName::SquareTerminal,
                Box::new(move |_, _, cx| cx.emit(ExplorerEvent::OpenTerminal(d3.clone()))),
            ))
            .item(act("Reveal in File Explorer", IconName::FolderOpen, Box::new(move |_, _, _| reveal(&p1))))
            .separator()
            .item(act(
                "Copy Path",
                IconName::Copy,
                Box::new(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(p2.display().to_string()))),
            ))
            .item(act(
                "Copy Relative Path",
                IconName::Copy,
                Box::new(move |e, _, cx| {
                    let rel = p3.strip_prefix(&e.root).unwrap_or(&p3).to_string_lossy().replace('\\', "/");
                    cx.write_to_clipboard(ClipboardItem::new_string(rel))
                }),
            ))
            .separator()
            .item(act("Rename…", IconName::Pencil, Box::new(move |e, w, cx| e.prompt_rename(p4.clone(), w, cx))))
            .item(act("Delete", IconName::Trash, Box::new(move |e, w, cx| e.confirm_delete(p5.clone(), w, cx))))
    }

    /// Ask for a name, then run `then` with it.
    fn prompt_name(
        &self,
        title: &'static str,
        initial: String,
        then: impl Fn(&mut Explorer, String, &mut Context<Explorer>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let this = cx.weak_entity();
        let then = std::rc::Rc::new(then);
        window.open_alert_dialog(cx, {
            let input = input.clone();
            move |dialog, _, _| {
                let input = input.clone();
                let this = this.clone();
                let then = then.clone();
                dialog.title(title).show_cancel(true).child(Input::new(&input)).on_ok(move |_, _, cx| {
                    let name = input.read(cx).value().trim().to_string();
                    if name.is_empty() {
                        return false;
                    }
                    let then = then.clone();
                    _ = this.update(cx, |explorer, cx| then(explorer, name, cx));
                    true
                })
            }
        });
        window.defer(cx, move |window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    }

    fn prompt_new(&mut self, dir: PathBuf, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let title = if folder { "New Folder" } else { "New File" };
        self.prompt_name(
            title,
            String::new(),
            move |explorer, name, cx| {
                let path = dir.join(&name);
                let result = if folder {
                    std::fs::create_dir_all(&path)
                } else {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(drop)
                };
                match result {
                    Ok(()) => {
                        explorer.expanded.insert(dir.clone());
                        explorer.paths_changed(&[path.clone()], cx);
                        if !folder {
                            cx.emit(ExplorerEvent::Open { path, preview: false });
                        }
                    }
                    Err(err) => explorer.error = Some(format!("Could not create {name}: {err}").into()),
                }
                cx.notify();
            },
            window,
            cx,
        );
    }

    fn prompt_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let initial = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.prompt_name(
            "Rename",
            initial,
            move |explorer, name, cx| {
                let target = path.with_file_name(&name);
                match std::fs::rename(&path, &target) {
                    Ok(()) => explorer.paths_changed(&[path.clone(), target], cx),
                    Err(err) => explorer.error = Some(format!("Could not rename: {err}").into()),
                }
                cx.notify();
            },
            window,
            cx,
        );
    }

    /// Delete moves to the Recycle Bin, after asking.
    fn confirm_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let path = path.clone();
            dialog
                .title("Delete")
                .description(format!("Move {name} to the Recycle Bin?"))
                .ok_text("Delete")
                .show_cancel(true)
                .on_ok(move |_, _, cx| {
                    let path = path.clone();
                    _ = this.update(cx, |explorer, cx| {
                        match trash::delete(&path) {
                            Ok(()) => explorer.paths_changed(&[path], cx),
                            Err(err) => explorer.error = Some(format!("Could not delete: {err}").into()),
                        }
                        cx.notify();
                    });
                    true
                })
        });
    }

    fn render_rows(&mut self, range: Range<usize>, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let this = cx.weak_entity();
        let focused = self.focus_handle.contains_focused(window, cx);
        let repo = self.repo.read(cx);
        let decorations: Vec<(Option<char>, bool, bool)> = range
            .clone()
            .map(|ix| match self.rows.get(ix) {
                Some(row) => (
                    repo.letter(&row.entry.path),
                    row.entry.is_dir && repo.has_changes_under(&row.entry.path),
                    repo.is_ignored(&row.entry.path),
                ),
                None => (None, false, false),
            })
            .collect();
        let first = range.start;
        let colors: Vec<Option<Hsla>> = decorations.iter().map(|(letter, _, _)| letter.map(|l| letter_color(l, cx))).collect();
        let theme = cx.theme();
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                let selected = self.selected == Some(ix);
                let entry = &row.entry;
                let (letter, changes_inside, ignored) = decorations[ix - first];
                let color = colors[ix - first];
                let label_color = color.unwrap_or(if ignored { theme.muted_foreground } else { theme.foreground });
                let (chevron, icon) = if entry.is_dir {
                    let open = self.expanded.contains(&entry.path);
                    (
                        Some(if open { IconName::ChevronDown } else { IconName::ChevronRight }),
                        Some(if open { IconName::FolderOpen } else { IconName::Folder }),
                    )
                } else {
                    (None, None)
                };
                Some(
                    h_flex()
                        .id(ix)
                        .w_full()
                        .h(px(22.))
                        .pl(px(8. + 12. * row.depth as f32))
                        .pr_2()
                        .gap_1()
                        .text_sm()
                        .border_1()
                        .border_color(transparent_black())
                        .when(selected, |this| {
                            this.bg(theme.list_active).when(focused, |this| {
                                this.border_color(theme.list_active_border)
                            })
                        })
                        .when(!selected, |this| this.hover(|this| this.bg(theme.list_hover)))
                        .child(
                            div().w(px(16.)).flex_none().when_some(chevron, |this, chevron| {
                                this.child(Icon::new(chevron).xsmall().text_color(theme.muted_foreground))
                            }),
                        )
                        .child(match icon {
                            Some(icon) => Icon::new(icon).small().text_color(theme.muted_foreground).into_any_element(),
                            None => crate::file_icon::render(&entry.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), 16., cx),
                        })
                        .child(div().flex_1().min_w_0().truncate().text_color(label_color).when(ignored, |this| this.opacity(0.75)).child(entry.name.clone()))
                        // Git status as in den: a letter for a changed file, a dot for a
                        // folder holding changes.
                        .when_some(letter.zip(color), |this, (letter, color)| {
                            this.child(div().flex_none().text_xs().text_color(color).child(letter.to_string()))
                        })
                        .when(letter.is_none() && changes_inside, |this| {
                            this.child(div().flex_none().size(px(6.)).rounded_full().bg(theme.yellow.opacity(0.8)))
                        })
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.focus_handle.focus(window, cx);
                            this.activate(ix, event.click_count() < 2, cx);
                        }))
                        .context_menu({
                            let this = this.clone();
                            let path = entry.path.clone();
                            let is_dir = entry.is_dir;
                            move |menu, _, _| Explorer::row_menu(menu, this.clone(), path.clone(), is_dir)
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for Explorer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root_name: SharedString = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.display().to_string())
            .into();

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(32.))
                    .flex_none()
                    .px_3()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("EXPLORER · {}", root_name.to_uppercase())),
                    )
                    .child(
                        h_flex()
                            .child(
                                Button::new("new-file")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::FilePlus))
                                    .tooltip("New File")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let dir = this.target_dir();
                                        this.prompt_new(dir, false, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("new-folder")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::FolderPlus))
                                    .tooltip("New Folder")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let dir = this.target_dir();
                                        this.prompt_new(dir, true, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("refresh")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::RefreshCw))
                                    .tooltip("Refresh")
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            )
                            .child(
                                Button::new("collapse")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::ListCollapse))
                                    .tooltip("Collapse All")
                                    .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
                            ),
                    ),
            )
            .child(
                div().when_some(self.error.clone(), |this, error| {
                    this.child(div().px_3().py_1().text_xs().text_color(cx.theme().danger).child(error))
                }),
            )
            .child(
                div()
                    .id("explorer-tree")
                    .key_context("Explorer")
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(Self::on_key_down))
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list(
                            "explorer-rows",
                            self.rows.len(),
                            cx.processor(|this, range, window, cx| this.render_rows(range, window, cx)),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
            )
    }
}

/// Show `path` selected in Windows Explorer.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}
