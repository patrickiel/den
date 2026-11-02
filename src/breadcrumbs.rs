//! The breadcrumbs over a file tab, as VS Code's: the file's path, from the
//! session root when the file is inside it, else from the top of the disk.
//!
//! Each part opens a dropdown of the folder it is in, the part selected: a
//! tree whose folders unfold in place, where picking a file opens it.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable, Sizable as _, h_flex, popover::Popover, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::explorer::{Entry, read_dir};

pub enum BreadcrumbsEvent {
    /// A file picked in a dropdown.
    Open(PathBuf),
}

struct Row {
    entry: Entry,
    depth: usize,
}

pub struct Breadcrumbs {
    /// The file; none for an untitled tab, which shows no breadcrumbs.
    path: Option<PathBuf>,
    /// The session root the parts start under.
    root: Option<PathBuf>,
    /// The part whose dropdown is open.
    open: Option<usize>,
    /// Folders unfolded in the open dropdown.
    expanded: HashSet<PathBuf>,
    /// The open dropdown's tree, flattened.
    rows: Vec<Row>,
    selected: Option<usize>,
    focus_handle: FocusHandle,
    scroll: ScrollHandle,
}

impl EventEmitter<BreadcrumbsEvent> for Breadcrumbs {}

/// The parts of `path` shown, each with its own full path: those under
/// `root` when it holds the file, else every one down from the disk's top.
fn segments(path: &Path, root: Option<&Path>) -> Vec<(SharedString, PathBuf)> {
    let (mut at, rest) = match root.and_then(|root| Some((root, path.strip_prefix(root).ok()?))) {
        Some((root, rest)) if !rest.as_os_str().is_empty() => (root.to_path_buf(), rest),
        _ => (PathBuf::new(), path),
    };
    let mut parts = Vec::new();
    for component in rest.components() {
        at.push(component);
        let name = match component {
            Component::Normal(name) => name.to_string_lossy(),
            // A Windows drive (`C:`).
            Component::Prefix(prefix) => prefix.as_os_str().to_string_lossy(),
            _ => continue,
        };
        parts.push((name.to_string().into(), at.clone()));
    }
    parts
}

impl Breadcrumbs {
    pub fn new(path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        Self {
            path,
            root: None,
            open: None,
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            focus_handle: cx.focus_handle(),
            scroll: ScrollHandle::new(),
        }
    }

    pub fn set_path(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.path != path {
            self.path = path;
            self.open = None;
            cx.notify();
        }
    }

    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.root = Some(root);
            self.open = None;
            cx.notify();
        }
    }

    fn segments(&self) -> Vec<(SharedString, PathBuf)> {
        match &self.path {
            Some(path) => segments(path, self.root.as_deref()),
            None => Vec::new(),
        }
    }

    /// Open part `ix`'s dropdown: its folder, the part selected.
    fn open_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some((_, path)) = self.segments().into_iter().nth(ix) else {
            return;
        };
        self.open = Some(ix);
        self.expanded.clear();
        self.rebuild();
        let selected = self.rows.iter().position(|row| row.entry.path == path);
        self.select(selected.unwrap_or(0), cx);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            cx.notify();
        }
    }

    /// The open dropdown's folder.
    fn dir(&self) -> Option<PathBuf> {
        let (_, path) = self.segments().into_iter().nth(self.open?)?;
        path.parent().map(Path::to_path_buf)
    }

    fn rebuild(&mut self) {
        let selected = self
            .selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| row.entry.path.clone());
        let mut rows = Vec::new();
        if let Some(dir) = self.dir() {
            let mut stack: Vec<(Entry, usize)> =
                read_dir(&dir).into_iter().rev().map(|e| (e, 0)).collect();
            while let Some((entry, depth)) = stack.pop() {
                if entry.is_dir && self.expanded.contains(&entry.path) {
                    for child in read_dir(&entry.path).into_iter().rev() {
                        stack.push((child, depth + 1));
                    }
                }
                rows.push(Row { entry, depth });
            }
        }
        self.rows = rows;
        self.selected =
            selected.and_then(|path| self.rows.iter().position(|row| row.entry.path == path));
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            self.selected = None;
        } else {
            let ix = ix.min(self.rows.len() - 1);
            self.selected = Some(ix);
            self.scroll.scroll_to_item(ix);
        }
        cx.notify();
    }

    fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild();
        cx.notify();
    }

    /// A folder unfolds or folds; a file opens, closing the dropdown.
    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.rows.get(ix).map(|row| row.entry.clone()) else {
            return;
        };
        self.selected = Some(ix);
        if entry.is_dir {
            self.toggle(&entry.path, cx);
        } else {
            self.close(cx);
            cx.emit(BreadcrumbsEvent::Open(entry.path));
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let current = self.selected.unwrap_or(0);
        let row = self
            .rows
            .get(current)
            .map(|row| (row.entry.clone(), row.depth));
        match event.keystroke.key.as_str() {
            "down" => self.select(self.selected.map_or(0, |ix| ix + 1), cx),
            "up" => self.select(current.saturating_sub(1), cx),
            "home" => self.select(0, cx),
            "end" => self.select(usize::MAX, cx),
            "pagedown" => self.select(current + 10, cx),
            "pageup" => self.select(current.saturating_sub(10), cx),
            "enter" | "space" => self.activate(current, cx),
            "right" => match row {
                Some((entry, _)) if entry.is_dir && !self.expanded.contains(&entry.path) => {
                    self.toggle(&entry.path, cx)
                }
                Some((entry, _)) if entry.is_dir => self.select(current + 1, cx),
                _ => {}
            },
            "left" => match row {
                Some((entry, _)) if entry.is_dir && self.expanded.contains(&entry.path) => {
                    self.toggle(&entry.path, cx)
                }
                Some((_, depth)) if depth > 0 => {
                    if let Some(parent) = self.rows[..current]
                        .iter()
                        .rposition(|row| row.depth < depth)
                    {
                        self.select(parent, cx);
                    }
                }
                _ => {}
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The open dropdown: the folder's tree.
    fn render_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let rows = self.rows.iter().enumerate().map(|(ix, row)| {
            let entry = &row.entry;
            let selected = self.selected == Some(ix);
            let icon = if entry.is_dir {
                let open = self.expanded.contains(&entry.path);
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small()
                .text_color(theme.muted_foreground)
                .into_any_element()
            } else {
                crate::file_icon::render(&entry.name, 16., cx)
            };
            h_flex()
                .id(ix)
                .flex_none()
                .w_full()
                .h(px(24.))
                .pl(px(6. + 16. * row.depth as f32))
                .pr_3()
                .gap_1p5()
                .rounded(theme.radius)
                .border_1()
                .border_color(transparent_black())
                .when(selected, |this| {
                    this.bg(theme.list_active)
                        .border_color(theme.list_active_border)
                })
                .when(!selected, |this| {
                    this.hover(|this| this.bg(theme.list_hover))
                })
                .child(
                    div()
                        .flex_none()
                        .w(px(16.))
                        .flex()
                        .justify_center()
                        .child(icon),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(entry.name.clone()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.activate(ix, cx)))
        });
        let rows: Vec<_> = rows.collect();
        let empty = rows.is_empty();
        v_flex()
            .id("breadcrumb-list")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .min_w(px(220.))
            .max_w(px(480.))
            .max_h(px(400.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .text_sm()
            .children(rows)
            .when(empty, |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(cx.theme().muted_foreground)
                        .child("Cannot read this folder"),
                )
            })
            .into_any_element()
    }
}

impl Render for Breadcrumbs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let segments = self.segments();
        let last = segments.len().saturating_sub(1);
        let muted = cx.theme().muted_foreground;
        let this = cx.weak_entity();
        h_flex()
            .id("breadcrumbs")
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_sm()
            .children(segments.into_iter().enumerate().map(|(ix, (name, path))| {
                let file = ix == last;
                let crumb = Crumb {
                    id: ("crumb", ix).into(),
                    label: name.clone(),
                    icon: file.then(|| crate::file_icon::render(&name, 14., cx)),
                    open: self.open == Some(ix),
                };
                // The folders give way, the file keeps its name whole.
                let style = if file {
                    StyleRefinement::default().flex_none()
                } else {
                    StyleRefinement::default().min_w_0().flex_shrink_1()
                };
                let part = match path.parent() {
                    Some(_) => Popover::new(("crumb-dropdown", ix))
                        .anchor(Anchor::TopLeft)
                        .p_1()
                        .trigger(crumb)
                        .trigger_style(style)
                        .open(self.open == Some(ix))
                        .on_open_change({
                            let this = this.clone();
                            move |open, _, cx| {
                                _ = this.update(cx, |this, cx| match open {
                                    true => this.open_at(ix, cx),
                                    false => this.close(cx),
                                });
                            }
                        })
                        .track_focus(&self.focus_handle)
                        .content({
                            let this = this.clone();
                            move |_, _, cx| {
                                this.update(cx, |this, cx| this.render_list(cx))
                                    .unwrap_or_else(|_| div().into_any_element())
                            }
                        })
                        .into_any_element(),
                    // A drive: nothing above it to list.
                    None => div().min_w_0().child(crumb).into_any_element(),
                };
                h_flex()
                    .min_w_0()
                    .when(file, |this| this.flex_none())
                    .when(ix > 0, |this| {
                        this.child(
                            Icon::new(IconName::ChevronRight)
                                .xsmall()
                                .flex_none()
                                .text_color(muted),
                        )
                    })
                    .child(part)
            }))
    }
}

/// A part of the path: its name, the file's with its icon, lit while its
/// dropdown is open.
#[derive(IntoElement)]
struct Crumb {
    id: ElementId,
    label: SharedString,
    icon: Option<AnyElement>,
    open: bool,
}

impl Selectable for Crumb {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for Crumb {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (foreground, hover) = (theme.foreground, theme.list_hover);
        h_flex()
            .id(self.id)
            .min_w_0()
            .h(px(20.))
            .px_1()
            .gap_1()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_color(theme.muted_foreground)
            .when(self.open, |this| this.bg(hover).text_color(foreground))
            .hover(move |this| this.text_color(foreground))
            .children(self.icon)
            .child(div().min_w_0().truncate().child(self.label))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` would shadow the built-in `#[test]`.
    use std::path::{Path, PathBuf};

    use gpui_kit::SharedString;

    use super::segments;

    fn names(parts: &[(SharedString, PathBuf)]) -> Vec<&str> {
        parts.iter().map(|(name, _)| name.as_ref()).collect()
    }

    #[cfg(unix)]
    #[test]
    fn parts_start_under_the_root_when_it_holds_the_file() {
        let root = Path::new("/home/me/project");
        let parts = segments(Path::new("/home/me/project/src/main.rs"), Some(root));
        assert_eq!(names(&parts), ["src", "main.rs"]);
        assert_eq!(parts[0].1, Path::new("/home/me/project/src"));
        assert_eq!(parts[0].1.parent(), Some(root));
    }

    #[cfg(unix)]
    #[test]
    fn parts_start_at_the_top_outside_the_root() {
        let parts = segments(
            Path::new("/home/me/.config/app/app.log"),
            Some(Path::new("/home/me/project")),
        );
        assert_eq!(names(&parts), ["home", "me", ".config", "app", "app.log"]);
        assert_eq!(parts[0].1.parent(), Some(Path::new("/")));
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_is_a_part() {
        let parts = segments(Path::new(r"C:\Users\me\notes.txt"), None);
        assert_eq!(names(&parts), ["C:", "Users", "me", "notes.txt"]);
        assert_eq!(parts[1].1.parent(), Some(Path::new(r"C:\")));
    }
}
