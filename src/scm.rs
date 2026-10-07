//! Source Control, as in den: the branch with ahead/behind and its actions, a
//! commit box, the Merge Changes / Staged Changes / Changes groups with their
//! row and group buttons, and the recent commits. Everything runs through
//! `Repo`; errors show at the top of the view with git's message.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use den_extension::view::Graph;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex,
    hover_card::HoverCard,
    input::{Input, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    backend::{
        avatars,
        git::{self, FileStatus, RefKind},
    },
    git_graph,
    repo::{Repo, RepoState},
    ui::menu_action,
};

/// A commit row's height.
const COMMIT_ROW: f32 = 22.;

pub enum ScmEvent {
    /// Open a changed file in an editor tab.
    Open(PathBuf),
    /// Show a change side by side: staged (index against HEAD) or not
    /// (working tree against the index).
    Diff { file: FileStatus, staged: bool },
    /// Show what a commit changed in one file.
    CommitDiff { rel: String, commit: crate::diff::CommitRevs },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Merge,
    Staged,
    Changes,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::Merge => "Merge Changes",
            Group::Staged => "Staged Changes",
            Group::Changes => "Changes",
        }
    }

    fn holds(self, file: &FileStatus) -> bool {
        match self {
            Group::Merge => file.conflict,
            Group::Staged => file.staged(),
            Group::Changes => file.changed(),
        }
    }
}

pub struct ScmView {
    repo: Entity<Repo>,
    message: Entity<InputState>,
    commits_open: bool,
    /// Generate Commit Message while it runs: how to stop it, and what it does.
    generating: Option<(crate::backend::http::Cancel, SharedString)>,
    /// Commits expanded to their files, with the files once loaded.
    expanded: HashMap<String, Option<(Option<String>, Vec<git::CommitFile>)>>,
    /// What a commit changed, for its hover card: `None` while it loads.
    stats: HashMap<String, Option<git::Stat>>,
    /// Authors' avatars by email: `None` when there is none to show.
    avatars: HashMap<String, Option<PathBuf>>,
    /// Emails whose avatar is being fetched.
    avatar_pending: HashSet<String>,
    collapsed: Vec<&'static str>,
    /// Selected rows (by path) in one group, as den selects them: a click
    /// selects one, Ctrl+click adds or removes, Shift+click a range.
    selection: Vec<String>,
    selection_group: Option<Group>,
    /// Where a Shift+click range starts, by position in the group.
    anchor: Option<usize>,
    /// The list's keys: Space stages or unstages, Delete discards.
    list_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ScmEvent> for ScmView {}

impl ScmView {
    pub fn new(repo: Entity<Repo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| InputState::new(window, cx).placeholder("Message (Ctrl+Enter to commit)"));
        let _subscriptions = vec![cx.observe(&repo, |_, _, cx| cx.notify())];
        Self {
            repo,
            message,
            commits_open: true,
            generating: None,
            expanded: Default::default(),
            stats: Default::default(),
            avatars: Default::default(),
            avatar_pending: Default::default(),
            collapsed: Vec::new(),
            selection: Vec::new(),
            selection_group: None,
            anchor: None,
            list_focus: cx.focus_handle(),
            _subscriptions,
        }
    }

    pub fn focus(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let message = this.read(cx).message.clone();
        message.update(cx, |message, cx| message.focus(window, cx));
    }

    /// The draft commit message, for the session.
    pub fn draft(&self, cx: &App) -> String {
        self.message.read(cx).value().to_string()
    }

    pub fn set_draft(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.message.update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Commit (and push after, with `push`). With nothing staged it goes by
    /// Settings ▸ Source Control: ask whether to stage everything and commit
    /// it, always do, or never.
    fn commit(&mut self, amend: bool, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::settings::{Settings, SmartCommit};
        let message = self.message.read(cx).value().to_string();
        let Some(status) = self.repo.read(cx).status() else { return };
        if status.files.iter().any(|f| f.conflict) {
            crate::toast::push(window, "Resolve the merge conflicts before committing: stage each resolved file under Merge Changes.", cx);
            return;
        }
        if message.trim().is_empty() && !amend {
            crate::toast::push(window, "Type a commit message first.", cx);
            return;
        }
        let staged = status.files.iter().any(FileStatus::staged);
        let changed = status.files.iter().any(|f| f.changed() || f.untracked());
        if staged || amend {
            return self.do_commit(message, amend, false, push, window, cx);
        }
        if !changed {
            crate::toast::push(window, "No changes to commit.", cx);
            return;
        }
        match Settings::get(cx).smart_commit {
            SmartCommit::Always => self.do_commit(message, amend, true, push, window, cx),
            SmartCommit::Never => crate::toast::push(window, "There are no staged changes to commit. Stage the changes first, or let Commit stage them: Settings ▸ Source Control.", cx),
            SmartCommit::Ask => {
                let this = cx.weak_entity();
                window.open_alert_dialog(cx, move |dialog, _, cx| {
                    let answer = |id: &'static str, label: &'static str, primary: bool, choice: Option<SmartCommit>| {
                        let this = this.clone();
                        let message = message.clone();
                        Button::new(id).small().label(label).map(|b| if primary { b.primary() } else { b.outline() }).on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            if let Some(choice) = choice {
                                Settings::update(cx, |s| s.smart_commit = choice);
                            }
                            if choice != Some(SmartCommit::Never) {
                                let message = message.clone();
                                _ = this.update(cx, |this, cx| this.do_commit(message, amend, true, push, window, cx));
                            }
                        })
                    };
                    dialog
                        .title("Source Control")
                        .description("There are no staged changes to commit.\n\nWould you like to stage all your changes and commit them directly?")
                        .footer(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(answer("smart-yes", "Yes", true, None))
                                .child(answer("smart-always", "Always", false, Some(SmartCommit::Always)))
                                .child(answer("smart-never", "Never", false, Some(SmartCommit::Never)))
                                .child(
                                    Button::new("smart-cancel")
                                        .small()
                                        .ghost()
                                        .label("Cancel")
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                ),
                        )
                        .map(|d| {
                            let _ = cx;
                            d
                        })
                });
            }
        }
    }

    fn do_commit(&mut self, message: String, amend: bool, stage_all: bool, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.repo.update(cx, |repo, cx| {
            repo.commit(message, amend, stage_all, cx);
            if push {
                repo.push_after_commit(cx);
            }
        });
        self.message.update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// What the action on the row of `rel` applies to: the selection when
    /// the row is part of it, else just the row.
    fn targets(&self, rel: &str, group: Group, cx: &App) -> Vec<FileStatus> {
        if self.selection_group == Some(group) && self.selection.iter().any(|r| r == rel) {
            let status = self.repo.read(cx).status();
            let files: Vec<FileStatus> = status.map(|s| group_files(s, group).filter(|f| self.selection.contains(&f.rel)).cloned().collect()).unwrap_or_default();
            if !files.is_empty() {
                return files;
            }
        }
        self.file(rel, cx).into_iter().collect()
    }

    /// The changed file at `rel`, as the status has it now.
    fn file(&self, rel: &str, cx: &App) -> Option<FileStatus> {
        self.repo.read(cx).status()?.files.iter().find(|f| f.rel == rel).cloned()
    }

    /// The commit `hash`, as the history has it now.
    fn commit_by_hash(&self, hash: &str, cx: &App) -> Option<git::Commit> {
        self.repo.read(cx).commits.iter().find(|c| c.hash == hash).cloned()
    }

    /// The selected files, with their group.
    fn selected_files(&self, cx: &App) -> Option<(Group, Vec<FileStatus>)> {
        let group = self.selection_group?;
        let status = self.repo.read(cx).status()?;
        let files: Vec<FileStatus> = group_files(status, group).filter(|f| self.selection.contains(&f.rel)).cloned().collect();
        (!files.is_empty()).then_some((group, files))
    }

    /// Every file of `group`, for the actions on the whole group.
    fn files_in(&self, group: Group, cx: &App) -> Vec<FileStatus> {
        self.repo.read(cx).status().map(|s| group_files(s, group).cloned().collect()).unwrap_or_default()
    }

    /// A click on row `ix` of `group` (whose paths are `rels`).
    fn click_row(&mut self, group: Group, ix: usize, rels: &[String], modifiers: Modifiers, cx: &mut Context<Self>) -> bool {
        let rel = rels[ix].clone();
        if self.selection_group != Some(group) {
            self.selection.clear();
            self.selection_group = Some(group);
            self.anchor = None;
        }
        let plain = !modifiers.control && !modifiers.shift;
        if modifiers.control {
            if let Some(at) = self.selection.iter().position(|r| *r == rel) {
                self.selection.remove(at);
            } else {
                self.selection.push(rel);
            }
            self.anchor = Some(ix);
        } else if modifiers.shift {
            let from = self.anchor.unwrap_or(ix);
            let (a, b) = (from.min(ix), from.max(ix));
            self.selection = rels[a..=b].to_vec();
        } else {
            self.selection = vec![rel];
            self.anchor = Some(ix);
        }
        cx.notify();
        plain
    }

    fn stage_or_unstage(&mut self, files: Vec<FileStatus>, group: Group, cx: &mut Context<Self>) {
        self.repo.update(cx, |repo, cx| if group == Group::Staged { repo.unstage(files, cx) } else { repo.stage(files, cx) });
    }

    /// Add the files to the repository's .gitignore, as den does.
    fn ignore(&mut self, files: Vec<FileStatus>, cx: &mut Context<Self>) {
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else { return };
        let path = top.join(".gitignore");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        for file in files {
            text.push_str(&format!("/{}\n", file.rel));
        }
        if std::fs::write(&path, text).is_ok() {
            self.repo.update(cx, |repo, cx| repo.refresh(cx));
        }
    }

    /// The right-click menu of a row: on the selection when the row is in it.
    fn row_menu(menu: PopupMenu, this: WeakEntity<Self>, rel: &str, group: Group, cx: &App) -> PopupMenu {
        let Some(view) = this.upgrade() else { return menu };
        let Some(file) = view.read(cx).file(rel, cx) else { return menu };
        let targets = view.read(cx).targets(rel, group, cx);
        let (open, diff) = (file.clone(), file.clone());
        let mut menu = menu
            .item(menu_action(&this, "Open File", move |_, _, cx| cx.emit(ScmEvent::Open(open.path.clone()))))
            .item(menu_action(&this, "Open Changes", move |_, _, cx| cx.emit(ScmEvent::Diff { file: diff.clone(), staged: group == Group::Staged })))
            .separator();
        let (t1, t2, t3) = (targets.clone(), targets.clone(), targets);
        menu = match group {
            Group::Staged => menu.item(menu_action(&this, "Unstage Changes", move |v, _, cx| v.stage_or_unstage(t1.clone(), group, cx))),
            Group::Changes => menu
                .item(menu_action(&this, "Stage Changes", move |v, _, cx| v.stage_or_unstage(t1.clone(), group, cx)))
                .item(menu_action(&this, "Discard Changes", move |v, window, cx| v.confirm_discard(t2.clone(), window, cx)))
                .item(menu_action(&this, "Add to .gitignore", move |v, _, cx| v.ignore(t3.clone(), cx))),
            Group::Merge => menu.item(menu_action(&this, "Stage (accept as resolved)", move |v, _, cx| v.stage_or_unstage(t1.clone(), group, cx))),
        };
        let (copy, copy_rel, show) = (file.clone(), file.clone(), file);
        menu.separator()
            .item(menu_action(&this, "Copy Path", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.path.display().to_string()))))
            .item(menu_action(&this, "Copy Relative Path", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_rel.rel.clone()))))
            .item(menu_action(&this, "Reveal in File Explorer", move |_, _, _| crate::explorer::reveal(&show.path)))
    }

    /// The header's ⋯ menu: the rest of what Source Control does, as den's.
    fn more_menu(menu: PopupMenu, this: WeakEntity<Self>) -> PopupMenu {
        menu.item(menu_action(&this, "Generate Commit Message", |v, window, cx| v.generate_message(window, cx)))
            .item(menu_action(&this, "Edit Commit Message Style", |v, _, cx| v.open_style(false, cx)))
            .item(menu_action(&this, "Derive Commit Message Style from History", |v, _, cx| v.open_style(true, cx)))
            .separator()
            .item(menu_action(&this, "Commit (Amend)", |v, window, cx| v.commit(true, false, window, cx)))
            .item(menu_action(&this, "Commit & Push", |v, window, cx| v.commit(false, true, window, cx)))
            .separator()
            .item(menu_action(&this, "Stage All Changes", |v, _, cx| {
                let files = v.files_in(Group::Changes, cx);
                v.repo.update(cx, |repo, cx| repo.stage(files, cx));
            }))
            .item(menu_action(&this, "Unstage All Changes", |v, _, cx| {
                let files = v.files_in(Group::Staged, cx);
                v.repo.update(cx, |repo, cx| repo.unstage(files, cx));
            }))
            .item(menu_action(&this, "Discard All Changes", |v, window, cx| {
                let files = v.files_in(Group::Changes, cx);
                v.confirm_discard(files, window, cx);
            }))
            .separator()
            .item(menu_action(&this, "Create Branch…", |v, window, cx| v.prompt_branch(window, cx)))
    }

    /// Open the repository's commit message style (made when missing).
    fn open_style(&mut self, derive: bool, cx: &mut Context<Self>) {
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else { return };
        let fallback = crate::settings::Settings::get(cx).ai_system_prompt.clone();
        let path = crate::backend::commit_ai::style_file(&top, &fallback, derive);
        cx.emit(ScmEvent::Open(path));
    }

    /// Space stages or unstages the selection; Delete discards it.
    fn list_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((group, files)) = self.selected_files(cx) else { return };
        match event.keystroke.key.as_str() {
            "space" => self.stage_or_unstage(files, group, cx),
            "delete" if group == Group::Changes => self.confirm_discard(files, window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Discarding asks first: untracked files go to the Recycle Bin, the rest
    /// lose their changes.
    fn confirm_discard(&mut self, files: Vec<FileStatus>, window: &mut Window, cx: &mut Context<Self>) {
        if files.is_empty() {
            return;
        }
        let repo = self.repo.clone();
        let what = match files.as_slice() {
            [file] => format!("Discard the changes in {}?", file.rel),
            files => format!("Discard the changes in {} files?", files.len()),
        };
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let repo = repo.clone();
            let files = files.clone();
            dialog
                .title("Discard Changes")
                .description(what.clone())
                .show_cancel(true)
                .on_ok(move |_, _, cx| {
                    repo.update(cx, |repo, cx| repo.discard(files.clone(), cx));
                    true
                })
        });
    }

    fn prompt_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Branch name"));
        let repo = self.repo.clone();
        window.open_alert_dialog(cx, {
            let input = input.clone();
            move |dialog, _, _| {
                let input = input.clone();
                let repo = repo.clone();
                dialog
                    .title("Create Branch")
                    .show_cancel(true)
                    .child(Input::new(&input))
                    .on_ok(move |_, _, cx| {
                        let name = input.read(cx).value().trim().to_string();
                        if name.is_empty() {
                            return false;
                        }
                        repo.update(cx, |repo, cx| repo.create_branch(name, cx));
                        true
                    })
            }
        });
        window.defer(cx, move |window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let repo = self.repo.read(cx);
        let branch = repo.status().map(|s| s.branch.clone()).unwrap_or_default();
        let top = repo.top().map(|p| p.to_path_buf());
        let busy = repo.busy;
        let handle = self.repo.clone();
        let weak = cx.weak_entity();

        let branch_button = Button::new("branch")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::GitBranch))
            .label(if branch.head.is_empty() { "…".to_string() } else { branch.head.clone() })
            .tooltip("Check out a branch")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.label("BRANCHES").max_h(px(420.)).scrollable(true);
                let branches = top.as_deref().and_then(|top| git::branches(top).ok()).unwrap_or_default();
                for branch in branches {
                    let handle = handle.clone();
                    let label = if branch.remote { format!("{} (remote)", branch.name) } else { branch.name.clone() };
                    menu = menu.item(PopupMenuItem::new(label).checked(branch.current).on_click(move |_, _, cx| {
                        let branch = branch.clone();
                        handle.update(cx, |repo, cx| repo.switch(branch, cx));
                    }));
                }
                let weak = weak.clone();
                menu.separator().item(PopupMenuItem::new("Create Branch…").icon(Icon::new(IconName::Plus)).on_click(move |_, window, cx| {
                    _ = weak.update(cx, |this, cx| this.prompt_branch(window, cx));
                }))
            });

        let action = |id: &'static str, icon: IconName, tooltip: &'static str, f: fn(&mut Repo, &mut Context<Repo>), cx: &mut Context<Self>| {
            let repo = self.repo.clone();
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Icon::new(icon))
                .tooltip(tooltip)
                .on_click(cx.listener(move |_, _, _, cx| repo.update(cx, |repo, cx| f(repo, cx))))
        };
        let push_tooltip = if branch.upstream.is_some() { "Push" } else { "Publish Branch" };

        v_flex()
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
                            .text_color(theme.muted_foreground)
                            .child("SOURCE CONTROL"),
                    )
                    .child(
                        h_flex()
                            .child(action("fetch", IconName::RefreshCcw, "Fetch", |r, cx| r.fetch(cx), cx))
                            .child(action("pull", IconName::ArrowDown, "Pull", |r, cx| r.pull(cx), cx))
                            .child(action("push", IconName::ArrowUp, push_tooltip, |r, cx| r.push(cx), cx))
                            .child(action("refresh", IconName::RotateCw, "Refresh", |r, cx| r.refresh(cx), cx))
                            .child({
                                let weak = cx.weak_entity();
                                Button::new("scm-more")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::Ellipsis))
                                    .tooltip("More Actions")
                                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| ScmView::more_menu(menu, weak.clone()))
                            }),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .pb_1()
                    .gap_1()
                    .text_sm()
                    .child(branch_button)
                    .when(branch.upstream.is_some(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("↓{} ↑{}", branch.behind, branch.ahead)),
                        )
                    })
                    .when_some(busy, |this, busy| {
                        this.child(div().ml_auto().text_xs().text_color(theme.muted_foreground).child(busy))
                    }),
            )
    }

    fn render_group(&self, group: Group, files: &[FileStatus], cx: &mut Context<Self>) -> Option<AnyElement> {
        let rows: Vec<&FileStatus> = files.iter().filter(|f| group.holds(f)).collect();
        if rows.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let title = group.title();
        let collapsed = self.collapsed.contains(&title);
        let header_buttons = h_flex()
            .invisible()
            .group_hover("scm-group", |this| this.visible())
            .when(group == Group::Changes, |this| {
                this.child(
                    Button::new("discard-all")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Undo2))
                        .tooltip("Discard All Changes")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let files = this.files_in(group, cx);
                            this.confirm_discard(files, window, cx)
                        })),
                )
            })
            .child({
                let staged = group == Group::Staged;
                Button::new("stage-all")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(if staged { IconName::Minus } else { IconName::Plus }))
                    .tooltip(if staged { "Unstage All" } else { "Stage All" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let files = this.files_in(group, cx);
                        this.stage_or_unstage(files, group, cx);
                    }))
            });

        let mut list = v_flex().child(
            h_flex()
                .id(title)
                .group("scm-group")
                .px_2()
                .h(px(24.))
                .gap_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .hover(|this| this.bg(theme.list_hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.collapsed.contains(&title) {
                        this.collapsed.push(title);
                    } else {
                        this.collapsed.retain(|t| *t != title);
                    }
                    cx.notify();
                }))
                .child(Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall())
                .child(title.to_uppercase())
                .child(div().flex_1())
                .child(header_buttons)
                .child(div().px_1p5().rounded_full().bg(theme.secondary).child(rows.len().to_string())),
        );
        if collapsed {
            return Some(list.into_any_element());
        }
        let rels: std::rc::Rc<Vec<String>> = std::rc::Rc::new(rows.iter().map(|f| f.rel.clone()).collect());
        let weak = cx.weak_entity();
        for (ix, file) in rows.into_iter().enumerate() {
            let selected = self.selection_group == Some(group) && self.selection.contains(&file.rel);
            let (name, folder) = file.rel.rsplit_once('/').map_or((file.rel.as_str(), ""), |(f, n)| (n, f));
            let (name, folder) = (name.to_string(), folder.to_string());
            let letter = file.letter();
            let color = letter_color(letter, cx);
            let staged_group = group == Group::Staged;
            let id = SharedString::from(format!("{title}-{ix}"));
            // The closures keep the path and look the file up when clicked.
            let (rel, path, conflict) = (file.rel.clone(), file.path.clone(), file.conflict);
            let row_buttons = h_flex()
                .invisible()
                .group_hover(id.clone(), |this| this.visible())
                .child({
                    let path = path.clone();
                    Button::new("open")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::FileText))
                        .tooltip("Open File")
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(ScmEvent::Open(path.clone()))))
                })
                .when(group == Group::Changes, |this| {
                    let rel = rel.clone();
                    this.child(
                        Button::new("discard")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Undo2))
                            .tooltip("Discard Changes")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let files = this.targets(&rel, group, cx);
                                this.confirm_discard(files, window, cx)
                            })),
                    )
                })
                .child({
                    let rel = rel.clone();
                    Button::new("stage")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(if staged_group { IconName::Minus } else { IconName::Plus }))
                        .tooltip(if staged_group { "Unstage Changes" } else { "Stage Changes" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let files = this.targets(&rel, group, cx);
                            this.stage_or_unstage(files, group, cx);
                        }))
                });
            list = list.child(
                h_flex()
                    .id(id.clone())
                    .group(id)
                    .pl(px(24.))
                    .pr_2()
                    .h(px(22.))
                    .gap_1p5()
                    .text_sm()
                    .when(selected, |this| this.bg(theme.list_active))
                    .when(!selected, |this| this.hover(|this| this.bg(theme.list_hover)))
                    .on_click({
                        let rels = rels.clone();
                        let (rel, path) = (rel.clone(), path.clone());
                        cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.list_focus.focus(window, cx);
                            // A plain click also shows the change; Ctrl and Shift only select.
                            if !this.click_row(group, ix, &rels, event.modifiers(), cx) {
                                return;
                            }
                            if conflict {
                                cx.emit(ScmEvent::Open(path.clone()));
                            } else if let Some(file) = this.file(&rel, cx) {
                                cx.emit(ScmEvent::Diff { file, staged: staged_group });
                            }
                        })
                    })
                    .context_menu({
                        let weak = weak.clone();
                        let rel = rel.clone();
                        move |menu, _, cx| ScmView::row_menu(menu, weak.clone(), &rel, group, cx)
                    })
                    .child(crate::file_icon::render(&file.rel, 16., cx))
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.6))
                            .truncate()
                            .text_color(color)
                            .when(letter == 'D', |this| this.line_through())
                            .child(name),
                    )
                    .child(div().flex_1().min_w_0().text_xs().truncate().text_color(theme.muted_foreground).child(folder))
                    .child(row_buttons)
                    .child(div().w(px(12.)).flex_none().text_xs().text_color(color).child(letter.to_string())),
            );
        }
        Some(list.into_any_element())
    }

    /// Generate Commit Message: stop it while it runs; else ask before the
    /// first download, then write the message into the box as it streams.
    fn generate_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::backend::ai;
        if let Some((cancel, _)) = self.generating.take() {
            cancel.cancel();
            return cx.notify();
        }
        let config = crate::settings::Settings::get(cx).ai_config();
        let status = ai::status(&config);
        if status.runtime && status.model {
            return self.run_generate(config, window, cx);
        }
        let preset = ai::MODEL_PRESETS.iter().find(|p| p.url == config.model);
        let model = preset.map_or_else(|| ai::model_name(&config.model), |p| format!("{} ({})", p.name, p.size));
        let mut parts = Vec::new();
        if !status.runtime {
            parts.push("the llama.cpp runtime (about 100 MB)".to_string());
        }
        if !status.model {
            parts.push(model);
        }
        let what = format!("Generating commit messages runs a model on this PC. Download {} into den's data folder?", parts.join(" and "));
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let config = config.clone();
            dialog.title("Local AI").description(what.clone()).ok_text("Download").show_cancel(true).on_ok(move |_, window, cx| {
                let config = config.clone();
                _ = this.update(cx, |this, cx| this.run_generate(config, window, cx));
                true
            })
        });
    }

    fn run_generate(&mut self, config: crate::backend::ai::AiConfig, window: &mut Window, cx: &mut Context<Self>) {
        use crate::backend::{ai, commit_ai};
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else { return };
        enum Update {
            Status(String),
            Text(String),
            Done(Result<String, String>),
        }
        let cancel = crate::backend::http::Cancel::default();
        self.generating = Some((cancel.clone(), "Starting…".into()));
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Update>();
        std::thread::spawn(move || {
            let send = |update| _ = tx.unbounded_send(update);
            let result = (|| {
                let status = ai::status(&config);
                if !(status.runtime && status.model) {
                    let mut last = String::new();
                    ai::install(&config, &cancel, &mut |stage, done, total| {
                        let text = match total {
                            0 => format!("{stage}…"),
                            total => format!("Downloading {stage}: {}%", done * 100 / total.max(1)),
                        };
                        if text != last {
                            last = text.clone();
                            send(Update::Status(text));
                        }
                    })?;
                }
                commit_ai::generate(&top, &config, &cancel, &mut |s| send(Update::Status(s)), &mut |t| send(Update::Text(t)))
            })();
            send(Update::Done(result));
        });
        cx.spawn_in(window, async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(update) = rx.next().await {
                let done = matches!(update, Update::Done(_));
                let alive = this.update_in(cx, |this, window, cx| {
                    match update {
                        Update::Status(text) => {
                            if let Some((_, status)) = &mut this.generating {
                                *status = text.into();
                            }
                        }
                        Update::Text(text) => {
                            if this.generating.is_some() {
                                this.message.update(cx, |input, cx| input.set_value(text, window, cx));
                            }
                        }
                        Update::Done(result) => {
                            let stopped = this.generating.take().is_none();
                            match result {
                                Ok(text) => this.message.update(cx, |input, cx| input.set_value(text, window, cx)),
                                Err(err) if err == crate::backend::http::CANCELLED || stopped => {}
                                Err(err) => crate::toast::push(window, format!("Generate Commit Message: {err}"), cx),
                            }
                        }
                    }
                    cx.notify();
                });
                if done || alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Expand a commit to its files (loaded in the background), or fold it.
    fn toggle_commit(&mut self, hash: String, cx: &mut Context<Self>) {
        if self.expanded.remove(&hash).is_some() {
            return cx.notify();
        }
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else { return };
        self.expanded.insert(hash.clone(), None);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let key = hash.clone();
            let files = cx.background_spawn(async move { git::commit_files(&top, &hash) }).await;
            _ = this.update(cx, |this, cx| {
                if let Some(slot) = this.expanded.get_mut(&key) {
                    *slot = Some(files.unwrap_or_default());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What a commit's hover card shows beyond the commit itself: what it
    /// changed, and its author's avatar. Both load in the background once.
    fn ensure_details(&mut self, hash: &str, cx: &mut Context<Self>) {
        let Some(commit) = self.commit_by_hash(hash, cx) else { return };
        let Some(top) = self.repo.read(cx).top().map(|p| p.to_path_buf()) else { return };
        if !self.stats.contains_key(&commit.hash) {
            self.stats.insert(commit.hash.clone(), None);
            let (key, hash, top) = (commit.hash.clone(), commit.hash.clone(), top.clone());
            cx.spawn(async move |this, cx| {
                let stat = cx.background_spawn(async move { git::commit_stat(&top, &hash) }).await;
                _ = this.update(cx, |this, cx| {
                    match stat {
                        Ok(stat) => _ = this.stats.insert(key, Some(stat)),
                        Err(_) => _ = this.stats.remove(&key),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        let email = commit.email.clone();
        if self.avatars.contains_key(&email) || self.avatar_pending.contains(&email) || email.is_empty() {
            return;
        }
        self.avatar_pending.insert(email.clone());
        let github = self.repo.read(cx).github.clone().map(|(owner, repo)| (owner, repo, commit.hash.clone()));
        cx.spawn(async move |this, cx| {
            let key = email.clone();
            let found = cx.background_spawn(async move { avatars::fetch(&email, github) }).await;
            _ = this.update(cx, |this, cx| {
                this.avatar_pending.remove(&key);
                this.avatars.insert(key, found);
                cx.notify();
            });
        })
        .detach();
    }

    /// A commit's files once expanded, each behind the lanes running on
    /// below the commit.
    fn render_commit_files(&self, hash: &str, below: &Graph, graph_w: Pixels, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let Some(loaded) = self.expanded.get(hash) else { return Vec::new() };
        let lanes = |below: &Graph| div().flex_none().w(graph_w).h_full().child(git_graph::graph_canvas(below.clone(), theme.background));
        let Some((parent, files)) = loaded else {
            return vec![
                h_flex()
                    .h(px(COMMIT_ROW))
                    .child(lanes(below))
                    .child(div().pl_2().text_xs().text_color(theme.muted_foreground).child("Loading…"))
                    .into_any_element(),
            ];
        };
        files
            .iter()
            .enumerate()
            .map(|(ix, file)| {
                let name = file.rel.rsplit('/').next().unwrap_or(&file.rel).to_string();
                let dir = file.rel.strip_suffix(name.as_str()).unwrap_or("").trim_end_matches('/').to_string();
                let event = ScmEvent::CommitDiff {
                    rel: file.rel.clone(),
                    commit: crate::diff::CommitRevs { hash: hash.to_string(), parent: parent.clone(), old_rel: file.old_rel.clone() },
                };
                let event = std::rc::Rc::new(event);
                h_flex()
                    .id(SharedString::from(format!("{hash}-{ix}")))
                    .pr_2()
                    .h(px(COMMIT_ROW))
                    .gap_2()
                    .text_sm()
                    .hover(|this| this.bg(theme.list_hover))
                    .child(lanes(below))
                    .child(crate::file_icon::render(&file.rel, 16., cx))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(div().flex_none().max_w(px(140.)).truncate().text_xs().text_color(theme.muted_foreground).child(dir))
                    .child(div().flex_none().w(px(12.)).text_xs().text_color(letter_color(file.status, cx)).child(file.status.to_string()))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let ScmEvent::CommitDiff { rel, commit } = &*event {
                            cx.emit(ScmEvent::CommitDiff { rel: rel.clone(), commit: commit.clone() });
                        }
                    }))
                    .into_any_element()
            })
            .collect()
    }

    /// A commit's row: its graph, subject, branch and tag badges, author and
    /// age, with a hover card and a menu.
    fn render_commit(&self, ix: usize, commit: &git::Commit, graph_w: Pixels, now: i64, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let repo = self.repo.read(cx);
        let graph = repo.row_graph(ix).cloned().unwrap_or_default();
        let lane_color = git_graph::lane_color(repo.row_lane(ix));
        let open = self.expanded.contains_key(&commit.hash);
        let files = if open { self.render_commit_files(&commit.hash, &git_graph::continuation(&graph), graph_w, cx) } else { Vec::new() };
        let weak = cx.weak_entity();
        let hash = commit.hash.clone();
        let row = h_flex()
            .id(SharedString::from(commit.hash.clone()))
            .pr_2()
            .h(px(COMMIT_ROW))
            .gap_2()
            .text_sm()
            .hover(|this| this.bg(theme.list_hover))
            .when(open, |this| this.bg(theme.list_active))
            .child(div().flex_none().w(graph_w).h_full().child(git_graph::graph_canvas(graph, theme.background)))
            .child(div().flex_1().min_w_0().truncate().child(commit.subject.clone()))
            .children(commit.refs.iter().map(|r| ref_badge(r, lane_color)))
            .when(commit.detached_head, |this| this.child(head_badge(theme.muted_foreground)))
            .child(div().flex_none().max_w(px(90.)).truncate().text_xs().text_color(theme.muted_foreground).child(commit.author.clone()))
            .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(git::age(commit.date, now)))
            .on_click(cx.listener({
                let hash = hash.clone();
                move |this, _, _, cx| this.toggle_commit(hash.clone(), cx)
            }))
            .context_menu({
                let (weak, hash) = (weak.clone(), hash.clone());
                move |menu, _, cx| ScmView::commit_menu(menu, weak.clone(), &hash, cx)
            });
        let card = HoverCard::new(SharedString::from(format!("card-{}", commit.hash)))
            // Beside the row, over the panes next to the panel, as VS Code's.
            .anchor(match crate::settings::Settings::get(cx).sidebar_position {
                crate::settings::SidebarSide::Left => Anchor::LeftCenter,
                crate::settings::SidebarSide::Right => Anchor::RightCenter,
            })
            .open_delay(std::time::Duration::from_millis(500))
            .on_open_change({
                let (weak, hash) = (weak.clone(), hash.clone());
                move |open, _, cx| {
                    if *open {
                        _ = weak.update(cx, |this, cx| this.ensure_details(&hash, cx));
                    }
                }
            })
            .trigger(row)
            .content(move |_, _, cx| render_commit_card(&weak, &hash, now, cx));
        v_flex().child(card).children(files).into_any_element()
    }

    /// A commit's right-click menu.
    fn commit_menu(menu: PopupMenu, this: WeakEntity<Self>, hash: &str, cx: &App) -> PopupMenu {
        let Some(view) = this.upgrade() else { return menu };
        let view = view.read(cx);
        let Some(commit) = view.commit_by_hash(hash, cx) else { return menu };
        let expanded = view.expanded.contains_key(&commit.hash);
        let url = view.repo.read(cx).github_commit_url(&commit.hash);
        let (hash, short, message) = (commit.hash.clone(), commit.short.clone(), commit_message(&commit));
        let toggle = commit.hash.clone();
        menu.item(menu_action(&this, if expanded { "Collapse" } else { "Show Changed Files" }, move |v, _, cx| v.toggle_commit(toggle.clone(), cx)))
            .separator()
            .item(menu_action(&this, "Copy Commit Hash", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))))
            .item(menu_action(&this, "Copy Short Hash", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(short.clone()))))
            .item(menu_action(&this, "Copy Commit Message", move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(message.clone()))))
            .when_some(url, |menu, url| menu.separator().item(menu_action(&this, "Open on GitHub", move |_, _, cx| cx.open_url(&url))))
    }

    fn render_commits(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let repo = self.repo.read(cx);
        let commits = &repo.commits;
        let uncommitted = repo.uncommitted.zip(repo.graph.first().cloned());
        let graph_w = git_graph::column_width(&repo.graph);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let more = !commits.is_empty() && commits.len().is_multiple_of(50);
        v_flex()
            .child(
                h_flex()
                    .id("commits-header")
                    .mt_2()
                    .px_2()
                    .h(px(24.))
                    .gap_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .border_t_1()
                    .border_color(theme.border)
                    .hover(|this| this.bg(theme.list_hover))
                    .child(Icon::new(if self.commits_open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                    .child("COMMITS")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.commits_open = !this.commits_open;
                        cx.notify();
                    })),
            )
            .when(self.commits_open, |this| {
                this.when_some(uncommitted, |this, (count, graph)| {
                    this.child(
                        h_flex()
                            .pr_2()
                            .h(px(COMMIT_ROW))
                            .gap_2()
                            .text_sm()
                            .child(div().flex_none().w(graph_w).h_full().child(git_graph::graph_canvas(graph, theme.background)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .italic()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{count} change{}", if count == 1 { "" } else { "s" })),
                            ),
                    )
                })
                .children(commits.iter().enumerate().map(|(ix, commit)| self.render_commit(ix, commit, graph_w, now, cx)))
                .when(more, |this| {
                    this.child(
                        div()
                            .id("load-more")
                            .pl(graph_w)
                            .h(px(COMMIT_ROW))
                            .text_sm()
                            .italic()
                            .text_color(theme.muted_foreground)
                            .hover(|this| this.bg(theme.list_hover))
                            .child("Load more…")
                            .on_click(cx.listener(|this, _, _, cx| this.repo.update(cx, |repo, cx| repo.load_more(cx)))),
                    )
                })
            })
    }
}

/// The files of `group`, as the status lists them.
fn group_files(status: &git::Status, group: Group) -> impl Iterator<Item = &FileStatus> {
    status.files.iter().filter(move |f| group.holds(f))
}

/// The whole message: the subject, then the body after a blank line.
fn commit_message(commit: &git::Commit) -> String {
    if commit.body.is_empty() { commit.subject.clone() } else { format!("{}\n\n{}", commit.subject, commit.body) }
}

/// A branch or tag at a commit, in its lane's colour: the checked-out
/// branch filled, the others outlined.
fn ref_badge(r: &git::CommitRef, color: Hsla) -> impl IntoElement {
    let icon = match r.kind {
        RefKind::Local => IconName::GitBranch,
        RefKind::Remote => IconName::Cloud,
        RefKind::Tag => IconName::Tag,
    };
    badge(icon, r.name.clone(), color, r.head)
}

/// HEAD on no branch.
fn head_badge(color: Hsla) -> impl IntoElement {
    badge(IconName::GitCommitHorizontal, "HEAD", color, false)
}

fn badge(icon: IconName, name: impl Into<SharedString>, color: Hsla, filled: bool) -> impl IntoElement {
    h_flex()
        .flex_none()
        .h(px(16.))
        .px(px(6.))
        .gap_1()
        .rounded_full()
        .text_xs()
        .border_1()
        .border_color(color)
        .map(|this| if filled { this.bg(color).text_color(gpui_kit::white()) } else { this.text_color(color) })
        .child(Icon::new(icon).xsmall())
        .child(div().max_w(px(110.)).truncate().child(name.into()))
}

/// A commit's hover card: who and when, the message, what it changed, and
/// its hash with a copy button and a link to GitHub.
fn render_commit_card(view: &WeakEntity<ScmView>, hash: &str, now: i64, cx: &App) -> AnyElement {
    let theme = cx.theme().clone();
    let Some(view) = view.upgrade() else { return div().into_any_element() };
    let view = view.read(cx);
    let Some(commit) = view.commit_by_hash(hash, cx) else { return div().into_any_element() };
    let (stat, avatar) = (view.stats.get(&commit.hash).copied(), view.avatars.get(&commit.email).cloned().flatten());
    let url = view.repo.read(cx).github_commit_url(&commit.hash);
    let avatar: AnyElement = match avatar.filter(|path| path.is_file()) {
        Some(path) => img(path).size(px(20.)).flex_none().rounded_full().object_fit(ObjectFit::Cover).into_any_element(),
        None => Avatar::new().name(commit.author.clone()).xsmall().into_any_element(),
    };
    let when = match git::long_date(&commit.when) {
        Some(date) => format!("{} ({date})", git::age(commit.date, now)),
        None => git::age(commit.date, now),
    };
    let divider = || div().h(px(1.)).w_full().bg(theme.border);
    let hash = commit.hash.clone();
    v_flex()
        .w(px(380.))
        .p_3()
        .gap_2()
        .text_sm()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(avatar)
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(commit.author.clone()))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(when)),
                ),
        )
        .child(divider())
        .child(
            v_flex()
                .gap_1()
                .child(div().whitespace_normal().child(commit.subject.clone()))
                .when(!commit.body.is_empty(), |this| this.child(div().whitespace_normal().text_xs().text_color(theme.muted_foreground).child(commit.body.clone()))),
        )
        .child(divider())
        .child(match stat {
            Some(Some(stat)) => {
                let count = |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
                h_flex()
                    .text_xs()
                    .child(count(stat.files, "file changed", "files changed"))
                    .when(stat.insertions > 0, |this| {
                        this.child(", ").child(div().text_color(theme.green).child(format!("{}(+)", count(stat.insertions, "insertion", "insertions"))))
                    })
                    .when(stat.deletions > 0, |this| {
                        this.child(", ").child(div().text_color(theme.red).child(format!("{}(-)", count(stat.deletions, "deletion", "deletions"))))
                    })
                    .into_any_element()
            }
            _ => div().text_xs().text_color(theme.muted_foreground).child("Loading…").into_any_element(),
        })
        .child(divider())
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(Icon::new(IconName::GitCommitHorizontal).small().text_color(theme.muted_foreground))
                .child(div().font_family(crate::settings::mono_font(cx)).text_xs().text_color(theme.muted_foreground).child(commit.short.clone()))
                .child(
                    Button::new("copy-hash")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Copy))
                        .tooltip("Copy Commit Hash")
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()));
                            crate::toast::push(window, "Copied the commit hash", cx);
                        }),
                )
                .when_some(url, |this, url| {
                    this.child(div().px_1().text_color(theme.muted_foreground).child("|")).child(
                        Button::new("open-github")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Github))
                            .label("Open on GitHub")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                }),
        )
        .into_any_element()
}

/// den's git colours: modified yellow, added and untracked green, deleted
/// red, renamed blue, conflicts red.
pub fn letter_color(letter: char, cx: &App) -> Hsla {
    let theme = cx.theme();
    match letter {
        'M' => theme.yellow,
        'U' | 'A' => theme.green,
        'D' | '!' => theme.red,
        'R' => theme.blue,
        _ => theme.foreground,
    }
}

impl Render for ScmView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let state = self.repo.read(cx).state.clone();
        let error = self.repo.read(cx).error.clone();

        let body: AnyElement = match &state {
            RepoState::Loading => div().px_3().text_sm().text_color(theme.muted_foreground).child("Reading the repository…").into_any_element(),
            RepoState::NoGit => div()
                .px_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Git was not found. Install Git for Windows to use Source Control.")
                .into_any_element(),
            RepoState::NoRepo => v_flex()
                .px_3()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("This folder is not in a git repository.")
                .child(
                    Button::new("init")
                        .primary()
                        .small()
                        .label("Initialize Repository")
                        .on_click(cx.listener(|this, _, _, cx| this.repo.update(cx, |repo, cx| repo.init(cx)))),
                )
                .into_any_element(),
            RepoState::Ready { status, .. } => {
                let files = &status.files;
                v_flex()
                    .children(self.render_group(Group::Merge, files, cx))
                    .children(self.render_group(Group::Staged, files, cx))
                    .children(self.render_group(Group::Changes, files, cx))
                    .when(files.is_empty(), |this| {
                        this.child(div().px_3().py_1().text_sm().text_color(theme.muted_foreground).child("No changes."))
                    })
                    .child(self.render_commits(cx))
                    .into_any_element()
            }
        };
        let ready = matches!(state, RepoState::Ready { .. });

        v_flex()
            .size_full()
            .child(self.render_header(cx))
            .when_some(error, |this, error| {
                this.child(
                    h_flex()
                        .mx_2()
                        .mb_1()
                        .p_2()
                        .gap_2()
                        .items_start()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(theme.danger)
                        .bg(theme.danger.opacity(0.12))
                        .text_xs()
                        .child(div().flex_1().min_w_0().child(error))
                        .child(
                            Button::new("dismiss-error")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::X))
                                .on_click(cx.listener(|this, _, _, cx| this.repo.update(cx, |repo, cx| repo.clear_error(cx)))),
                        ),
                )
            })
            .when(ready, |this| {
                this.child(
                    v_flex()
                        .px_2()
                        .gap_1()
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "enter" && event.keystroke.modifiers.control {
                                this.commit(false, false, window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(
                            h_flex()
                                .gap_1()
                                .child(div().flex_1().min_w_0().child(Input::new(&self.message).small()))
                                .child({
                                    let busy = self.generating.is_some();
                                    Button::new("generate-message")
                                        .ghost()
                                        .small()
                                        .icon(Icon::new(if busy { IconName::CircleStop } else { IconName::Sparkles }))
                                        .tooltip(if busy { "Stop" } else { "Generate Commit Message (local AI)" })
                                        .on_click(cx.listener(|this, _, window, cx| this.generate_message(window, cx)))
                                }),
                        )
                        .when_some(self.generating.as_ref().map(|(_, s)| s.clone()), |this, status| {
                            this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(status))
                        })
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("commit")
                                        .primary()
                                        .small()
                                        .flex_1()
                                        .icon(Icon::new(IconName::Check))
                                        .label("Commit")
                                        .on_click(cx.listener(|this, _, window, cx| this.commit(false, false, window, cx))),
                                )
                                .child({
                                    let weak = cx.weak_entity();
                                    Button::new("commit-more")
                                        .primary()
                                        .small()
                                        .icon(Icon::new(IconName::ChevronDown))
                                        .dropdown_menu(move |menu, _, _| {
                                            let amend = weak.clone();
                                            let push = weak.clone();
                                            menu.item(PopupMenuItem::new("Commit (Amend)").on_click(move |_, window, cx| {
                                                _ = amend.update(cx, |this, cx| this.commit(true, false, window, cx));
                                            }))
                                            .item(PopupMenuItem::new("Commit & Push").on_click(move |_, window, cx| {
                                                _ = push.update(cx, |this, cx| this.commit(false, true, window, cx));
                                            }))
                                        })
                                }),
                        ),
                )
            })
            .child(
                div()
                    .id("scm-changes")
                    .track_focus(&self.list_focus)
                    .on_key_down(cx.listener(Self::list_key))
                    .flex_1()
                    .min_h_0()
                    .pt_2()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}
