//! The Search view, as den's: results come in as you type, with Match Case,
//! Whole Word and Regular Expression (Alt+C / W / R), replace, files to
//! include / exclude, and the ignore files on or off. The search runs on
//! worker threads (`backend::search`) and streams its results in.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{StreamExt as _, channel::mpsc};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::backend::search::{self, FileMatch, Query};

/// Up to this many matches are shown, as in den.
const MAX_RESULTS: usize = 20_000;
const ROW: Pixels = px(22.);

pub enum SearchEvent {
    /// Show a match: in a preview (focus stays in the list) or for good.
    Open { path: PathBuf, line: u32, column: u32, focus: bool },
}

/// Whether a file has unsaved changes in an editor tab; replacing leaves such
/// files alone.
pub type IsDirty = Rc<dyn Fn(&Path, &App) -> bool>;

/// One row of the flattened results list.
#[derive(Clone)]
enum Row {
    File { file: usize },
    Match { file: usize, ix: usize },
}

pub struct SearchView {
    root: PathBuf,
    is_dirty: IsDirty,
    query: Entity<InputState>,
    replace: Entity<InputState>,
    include: Entity<InputState>,
    exclude: Entity<InputState>,
    show_replace: bool,
    show_details: bool,
    match_case: bool,
    whole_word: bool,
    regex: bool,
    use_ignore_files: bool,
    results: Vec<FileMatch>,
    rows: Vec<Row>,
    collapsed: HashSet<usize>,
    selected: Option<usize>,
    limit_hit: bool,
    searching: bool,
    message: Option<SharedString>,
    cancel: Arc<AtomicBool>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _search: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SearchEvent> for SearchView {}

impl SearchView {
    pub fn new(root: PathBuf, is_dirty: IsDirty, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let replace = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
        let include = cx.new(|cx| InputState::new(window, cx).placeholder("files to include"));
        let exclude = cx.new(|cx| InputState::new(window, cx).placeholder("files to exclude"));
        let _subscriptions = vec![
            cx.subscribe(&query, |this, _, event: &InputEvent, cx| this.on_input(event, cx)),
            cx.subscribe(&include, |this, _, event: &InputEvent, cx| this.on_input(event, cx)),
            cx.subscribe(&exclude, |this, _, event: &InputEvent, cx| this.on_input(event, cx)),
            cx.subscribe(&replace, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        Self {
            root,
            is_dirty,
            query,
            replace,
            include,
            exclude,
            show_replace: false,
            show_details: false,
            match_case: false,
            whole_word: false,
            regex: false,
            use_ignore_files: true,
            results: Vec::new(),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            selected: None,
            limit_hit: false,
            searching: false,
            message: None,
            cancel: Arc::new(AtomicBool::new(false)),
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _search: None,
            _subscriptions,
        }
    }

    pub fn focus(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let query = this.read(cx).query.clone();
        query.update(cx, |query, cx| query.focus(window, cx));
    }

    /// Put `text` in the search box (the selection, from Ctrl+Shift+F).
    pub fn set_query(this: &Entity<Self>, text: &str, replace: bool, window: &mut Window, cx: &mut App) {
        this.update(cx, |this, cx| {
            if replace {
                this.show_replace = true;
            }
            if !text.is_empty() {
                this.query.update(cx, |query, cx| query.set_value(text.to_string(), window, cx));
            }
            this.search(cx);
        });
    }

    fn on_input(&mut self, event: &InputEvent, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => self.search(cx),
            InputEvent::PressEnter { .. } => self.search(cx),
            _ => {}
        }
    }

    fn query_of(&self, cx: &App) -> Query {
        Query {
            pattern: self.query.read(cx).value().to_string(),
            is_regex: self.regex,
            case_sensitive: self.match_case,
            whole_word: self.whole_word,
            include: self.include.read(cx).value().to_string(),
            exclude: self.exclude.read(cx).value().to_string(),
            use_ignore_files: self.use_ignore_files,
            paths: None,
            max_results: MAX_RESULTS,
        }
    }

    /// Start a search (a little after the last keystroke), stopping the one
    /// before.
    fn search(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::SeqCst);
        let query = self.query_of(cx);
        if query.pattern.is_empty() {
            self._search = None;
            self.results.clear();
            self.rebuild_rows();
            self.message = None;
            self.searching = false;
            cx.notify();
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let root = self.root.clone();
        self._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            _ = this.update(cx, |this, cx| {
                this.results.clear();
                this.collapsed.clear();
                this.selected = None;
                this.rebuild_rows();
                this.searching = true;
                this.message = None;
                cx.notify();
            });
            let (tx, mut rx) = mpsc::unbounded::<Vec<FileMatch>>();
            let worker_cancel = cancel.clone();
            let worker = cx.background_executor().spawn(async move {
                search::search(&root, &query, &worker_cancel, |batch| tx.unbounded_send(batch).is_ok())
            });
            while let Some(batch) = rx.next().await {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                let alive = this.update(cx, |this, cx| {
                    this.results.extend(batch);
                    this.results.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
                    this.rebuild_rows();
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            let done = worker.await;
            _ = this.update(cx, |this, cx| {
                this.searching = false;
                match done {
                    Ok(done) => this.limit_hit = done.limit_hit,
                    Err(err) => this.message = Some(err.into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (file, found) in self.results.iter().enumerate() {
            self.rows.push(Row::File { file });
            if !self.collapsed.contains(&file) {
                self.rows.extend((0..found.matches.len()).map(|ix| Row::Match { file, ix }));
            }
        }
    }

    fn match_count(&self) -> usize {
        self.results.iter().map(|f| f.matches.len()).sum()
    }

    fn open_row(&mut self, row: usize, focus: bool, cx: &mut Context<Self>) {
        self.selected = Some(row);
        match self.rows.get(row).cloned() {
            Some(Row::Match { file, ix }) => {
                let found = &self.results[file];
                let m = &found.matches[ix];
                cx.emit(SearchEvent::Open {
                    path: PathBuf::from(&found.path),
                    line: m.line as u32,
                    column: m.start as u32 + 1,
                    focus,
                });
            }
            Some(Row::File { file }) => {
                if !self.collapsed.remove(&file) {
                    self.collapsed.insert(file);
                }
                self.rebuild_rows();
            }
            None => {}
        }
        cx.notify();
    }

    /// Dismiss a match or a whole file from the results.
    fn dismiss(&mut self, row: usize, cx: &mut Context<Self>) {
        match self.rows.get(row).cloned() {
            Some(Row::Match { file, ix }) => {
                self.results[file].matches.remove(ix);
                if self.results[file].matches.is_empty() {
                    self.results.remove(file);
                }
            }
            Some(Row::File { file }) => {
                self.results.remove(file);
            }
            None => return,
        }
        self.collapsed.clear();
        self.rebuild_rows();
        cx.notify();
    }

    /// Replace in the files given (every file in the results when `None`),
    /// on disk; files with unsaved changes in a tab are left alone.
    fn replace_in(&mut self, files: Option<Vec<usize>>, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query_of(cx);
        let replacement = self.replace.read(cx).value().to_string();
        let paths: Vec<PathBuf> = match files {
            Some(files) => files.iter().filter_map(|f| self.results.get(*f)).map(|f| PathBuf::from(&f.path)).collect(),
            None => self.results.iter().map(|f| PathBuf::from(&f.path)).collect(),
        };
        let (dirty, clean): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| (self.is_dirty)(p, cx));
        let mut replaced = 0;
        let mut failed = Vec::new();
        for path in &clean {
            let result = std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|text| search::replace(&query, &text, &replacement).map(|done| (text, done)))
                .and_then(|(text, (new, n))| {
                    if n > 0 && new != text {
                        std::fs::write(path, new).map_err(|e| e.to_string())?;
                    }
                    Ok(n)
                });
            match result {
                Ok(n) => replaced += n,
                Err(err) => failed.push(format!("{}: {err}", path.display())),
            }
        }
        let mut note = format!("Replaced {replaced} matches in {} files.", clean.len());
        if !dirty.is_empty() {
            note.push_str(&format!(" {} files with unsaved changes were left alone.", dirty.len()));
        }
        if !failed.is_empty() {
            note.push_str(&format!(" Failed: {}", failed.join("; ")));
        }
        crate::toast::push(window, note, cx);
        self.search(cx);
    }

    fn confirm_replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.results.is_empty() {
            return;
        }
        let count = self.match_count();
        let files = self.results.len();
        let replacement = self.replace.read(cx).value().to_string();
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title("Replace All")
                .description(format!("Replace {count} matches in {files} files with \"{replacement}\"?"))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    _ = this.update(cx, |this, cx| this.replace_in(None, window, cx));
                    true
                })
        });
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let alt = event.keystroke.modifiers.alt;
        match (key, alt) {
            ("c", true) => self.match_case = !self.match_case,
            ("w", true) => self.whole_word = !self.whole_word,
            ("r", true) => self.regex = !self.regex,
            ("escape", false) => {
                self.cancel.store(true, Ordering::SeqCst);
                self.searching = false;
                cx.notify();
                return;
            }
            _ => return,
        }
        cx.stop_propagation();
        self.search(cx);
    }

    fn on_list_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let current = self.selected.unwrap_or(0);
        match event.keystroke.key.as_str() {
            "down" => self.selected = Some((current + 1).min(self.rows.len().saturating_sub(1))),
            "up" => self.selected = Some(current.saturating_sub(1)),
            "enter" => return self.open_row(current, true, cx),
            "delete" => return self.dismiss(current, cx),
            _ => return,
        }
        if let Some(row) = self.selected {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn toggle(&self, id: &'static str, icon: IconName, tooltip: &'static str, on: bool, flip: fn(&mut Self), cx: &mut Context<Self>) -> Button {
        Button::new(id)
            .xsmall()
            .icon(Icon::new(icon))
            .tooltip(tooltip)
            .map(|button| if on { button.primary() } else { button.ghost() })
            .on_click(cx.listener(move |this, _, _, cx| {
                flip(this);
                this.search(cx);
            }))
    }

    fn render_rows(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let replacement = self.replace.read(cx).value().to_string();
        let replacing = self.show_replace;
        range
            .filter_map(|row| {
                let selected = self.selected == Some(row);
                let base = h_flex()
                    .id(row)
                    .group(SharedString::from(format!("search-row-{row}")))
                    .h(ROW)
                    .w_full()
                    .pr_2()
                    .gap_1()
                    .text_sm()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when(selected, |this| this.bg(theme.list_active))
                    .when(!selected, |this| this.hover(|this| this.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.open_row(row, event.click_count() > 1, cx);
                    }));
                let dismiss = Button::new(("dismiss", row))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::X))
                    .tooltip("Dismiss")
                    .on_click(cx.listener(move |this, _, _, cx| this.dismiss(row, cx)));
                let hover_buttons = h_flex()
                    .ml_auto()
                    .flex_none()
                    .invisible()
                    .group_hover(SharedString::from(format!("search-row-{row}")), |this| this.visible());
                Some(match self.rows.get(row)?.clone() {
                    Row::File { file } => {
                        let found = &self.results[file];
                        let (name, folder) = found.rel.rsplit_once('/').map_or((found.rel.as_str(), ""), |(f, n)| (n, f));
                        let collapsed = self.collapsed.contains(&file);
                        base.pl_1()
                            .child(Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall())
                            .child(crate::file_icon::render(name, 16., cx))
                            .child(name.to_string())
                            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(folder.to_string()))
                            .child(
                                hover_buttons
                                    .when(replacing, |this| {
                                        this.child(
                                            Button::new(("replace-file", row))
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::Replace))
                                                .tooltip("Replace in this file")
                                                .on_click(cx.listener(move |this, _, window, cx| this.replace_in(Some(vec![file]), window, cx))),
                                        )
                                    })
                                    .child(dismiss),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .rounded_full()
                                    .text_xs()
                                    .bg(theme.secondary)
                                    .child(found.matches.len().to_string()),
                            )
                            .into_any_element()
                    }
                    Row::Match { file, ix } => {
                        let m = &self.results[file].matches[ix];
                        base.pl(px(36.))
                            .child(div().flex_none().text_color(theme.muted_foreground).child(m.before.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .bg(theme.yellow.opacity(0.35))
                                    .when(replacing, |this| this.line_through())
                                    .child(m.text.clone()),
                            )
                            .when(replacing, |this| this.child(div().flex_none().bg(theme.green.opacity(0.35)).child(replacement.clone())))
                            .child(div().flex_1().min_w_0().truncate().child(m.after.clone()))
                            .child(hover_buttons.child(dismiss))
                            .into_any_element()
                    }
                })
            })
            .collect()
    }
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.match_count();
        let files = self.results.len();
        let has_query = !self.query.read(cx).value().is_empty();
        let summary: Option<SharedString> = if let Some(message) = &self.message {
            Some(message.clone())
        } else if self.searching && count == 0 {
            Some("Searching…".into())
        } else if has_query && !self.searching && count == 0 {
            Some("No results found.".into())
        } else if count > 0 {
            let limit = if self.limit_hit { format!(" (the first {MAX_RESULTS} are shown)") } else { String::new() };
            Some(format!("{count} results in {files} files{limit}").into())
        } else {
            None
        };

        v_flex()
            .size_full()
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                h_flex()
                    .h(px(32.))
                    .flex_none()
                    .px_3()
                    .justify_between()
                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("SEARCH"))
                    .child(
                        h_flex()
                            .child(
                                Button::new("research")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::RotateCw))
                                    .tooltip("Search Again")
                                    .on_click(cx.listener(|this, _, _, cx| this.search(cx))),
                            )
                            .child(
                                Button::new("collapse-results")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::ListCollapse))
                                    .tooltip("Collapse All")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.collapsed = (0..this.results.len()).collect();
                                        this.rebuild_rows();
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .gap_1()
                    .items_start()
                    .child(
                        Button::new("toggle-replace")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(if self.show_replace { IconName::ChevronDown } else { IconName::ChevronRight }))
                            .tooltip("Toggle Replace")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_replace = !this.show_replace;
                                cx.notify();
                            })),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(
                                Input::new(&self.query).small().suffix(
                                    h_flex()
                                        .child(self.toggle("case", IconName::CaseSensitive, "Match Case (Alt+C)", self.match_case, |s| s.match_case = !s.match_case, cx))
                                        .child(self.toggle("word", IconName::WholeWord, "Match Whole Word (Alt+W)", self.whole_word, |s| s.whole_word = !s.whole_word, cx))
                                        .child(self.toggle("regex", IconName::Regex, "Use Regular Expression (Alt+R)", self.regex, |s| s.regex = !s.regex, cx)),
                                ),
                            )
                            .when(self.show_replace, |this| {
                                this.child(
                                    h_flex()
                                        .gap_1()
                                        .child(div().flex_1().child(Input::new(&self.replace).small()))
                                        .child(
                                            Button::new("replace-all")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::ReplaceAll))
                                                .tooltip("Replace All")
                                                .on_click(cx.listener(|this, _, window, cx| this.confirm_replace_all(window, cx))),
                                        ),
                                )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .pt_1()
                    .justify_end()
                    .child(
                        Button::new("details")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Ellipsis))
                            .tooltip("Toggle Search Details")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_details = !this.show_details;
                                cx.notify();
                            })),
                    ),
            )
            .when(self.show_details, |this| {
                this.child(
                    v_flex()
                        .px_2()
                        .pl(px(30.))
                        .gap_1()
                        .child(Input::new(&self.include).small())
                        .child(
                            h_flex()
                                .gap_1()
                                .child(div().flex_1().child(Input::new(&self.exclude).small()))
                                .child(self.toggle(
                                    "ignore-files",
                                    IconName::Settings,
                                    "Use Exclude Settings and Ignore Files",
                                    self.use_ignore_files,
                                    |s| s.use_ignore_files = !s.use_ignore_files,
                                    cx,
                                )),
                        ),
                )
            })
            .when_some(summary, |this, summary| {
                this.child(div().px_3().py_1().text_xs().text_color(theme.muted_foreground).child(summary))
            })
            .child(
                div()
                    .id("search-results")
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(Self::on_list_key))
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("search-rows", self.rows.len(), cx.processor(|this, range, _, cx| this.render_rows(range, cx)))
                            .track_scroll(&self.scroll)
                            .size_full(),
                    ),
            )
    }
}
