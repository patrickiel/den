//! The changes an extension view's details list (a commit's files), one
//! after another as unified diffs: each file under a header, the unchanged
//! code folded away but for a few lines around each change, fading into the
//! fold. A click on a fold shows what it hides; a header opens its file's
//! change in a tab. A click on a file in the details scrolls to its change.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use den_extension::view::Diff;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::{Scrollbar, ScrollbarMode},
};
use gpui_kit::*;

use crate::diff::{LineStyles, Side, highlight_lines, inline_lines, load_sides, rel_in, rows};
use crate::settings::Settings;

/// Unchanged lines kept on each side of a change.
const CONTEXT: usize = 3;
/// A run of unchanged lines this short is shown rather than folded.
const FOLD_MIN: usize = 4;
/// The context lines' opacity, from the one next to a fold inward: the fog.
const FOG: [f32; CONTEXT] = [0.3, 0.55, 0.8];
/// The files read and shown; a commit touching more lists the rest by name.
const MOST: usize = 100;

struct Line {
    old: Option<usize>,
    new: Option<usize>,
    text: String,
    kind: Side,
    /// The side its colours come from (0 old, 1 new), and its line there (from 1).
    side: usize,
    number: usize,
}

struct File {
    rel: String,
    lines: Vec<Line>,
    styles: [Vec<LineStyles>; 2],
    added: usize,
    removed: usize,
    /// Why there is nothing to show (unreadable, binary, unchanged).
    note: Option<String>,
}

enum Item {
    /// Space before a file's header.
    Gap,
    Header(usize),
    Note(usize),
    /// A line of a file, and how faded it is.
    Line(usize, usize, f32),
    /// A file's unchanged lines `start..end`, folded.
    Fold(usize, usize, usize),
    /// The files not read.
    More(usize),
}

/// A file's change asked for in a tab of its own (its header's button).
pub struct OpenDiff(pub Diff);

impl EventEmitter<OpenDiff> for DiffList {}

pub struct DiffList {
    top: PathBuf,
    diffs: Vec<Diff>,
    files: Vec<File>,
    items: Vec<Item>,
    /// The folds opened, by file and first line.
    opened: HashSet<(usize, usize)>,
    loading: bool,
    /// The file to scroll to once its change is read.
    revealing: Option<usize>,
    scroll: UniformListScrollHandle,
    /// How far the lines are scrolled sideways.
    h_scroll: Pixels,
    /// The longest line, in characters.
    widest: usize,
    generation: u64,
    _load: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl DiffList {
    pub fn new(top: PathBuf, diffs: Vec<Diff>, cx: &mut Context<Self>) -> Self {
        let mut this = DiffList {
            top,
            diffs,
            files: Vec::new(),
            items: Vec::new(),
            opened: HashSet::new(),
            loading: true,
            revealing: None,
            scroll: UniformListScrollHandle::new(),
            h_scroll: px(0.),
            widest: 0,
            generation: 0,
            _load: None,
            _subscriptions: vec![
                cx.observe_global::<Settings>(|_, cx| cx.notify()),
                cx.observe_global::<Theme>(|this, cx| this.reload(cx)),
            ],
        };
        this.reload(cx);
        this
    }

    pub fn diffs(&self) -> &[Diff] {
        &self.diffs
    }

    /// Bring `diff`'s header to the top, once the files are read.
    pub fn reveal(&mut self, diff: &Diff, cx: &mut Context<Self>) {
        self.revealing = self.diffs.iter().position(|d| d == diff);
        self.scroll_to_revealed();
        cx.notify();
    }

    fn scroll_to_revealed(&mut self) {
        if self.loading {
            return;
        }
        let Some(f) = self.revealing.take() else {
            return;
        };
        if let Some(ix) = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Header(h) if *h == f))
        {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        }
    }

    /// Ask for file `f`'s change in a tab of its own.
    fn open(&mut self, f: usize, cx: &mut Context<Self>) {
        if let Some(diff) = self.diffs.get(f) {
            cx.emit(OpenDiff(diff.clone()));
        }
    }

    /// Read the files again, off the UI thread; a later reload wins.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let (top, diffs) = (self.top.clone(), self.diffs.clone());
        let theme = cx.theme().highlight_theme.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let files = cx
                .background_spawn(async move {
                    diffs
                        .iter()
                        .take(MOST)
                        .map(|diff| read(&top, diff, &theme))
                        .collect::<Vec<_>>()
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.widest = files
                    .iter()
                    .flat_map(|f| &f.lines)
                    .map(|l| l.text.chars().count())
                    .max()
                    .unwrap_or(0);
                this.files = files;
                this.loading = false;
                this.lay_out();
                this.scroll_to_revealed();
                cx.notify();
            });
        }));
    }

    /// The items shown: each file's header, then its lines with the long
    /// runs of unchanged ones folded.
    fn lay_out(&mut self) {
        let mut items = Vec::new();
        for (f, file) in self.files.iter().enumerate() {
            if f > 0 {
                items.push(Item::Gap);
            }
            items.push(Item::Header(f));
            if file.note.is_some() {
                items.push(Item::Note(f));
                continue;
            }
            let start = items.len();
            let shown = shown_lines(&file.lines);
            let mut ix = 0;
            while ix < file.lines.len() {
                if shown[ix] {
                    items.push(Item::Line(f, ix, 1.));
                    ix += 1;
                    continue;
                }
                let end = (ix..file.lines.len())
                    .find(|&i| shown[i])
                    .unwrap_or(file.lines.len());
                if end - ix < FOLD_MIN || self.opened.contains(&(f, ix)) {
                    items.extend((ix..end).map(|i| Item::Line(f, i, 1.)));
                } else {
                    items.push(Item::Fold(f, ix, end));
                }
                ix = end;
            }
            fog(&mut items[start..], &file.lines);
        }
        if self.diffs.len() > MOST {
            items.push(Item::Gap);
            items.push(Item::More(self.diffs.len() - MOST));
        }
        self.items = items;
    }

    fn render_items(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let line_height = px(Settings::get(cx).editor_font_size * 1.4);
        let number = |n: Option<usize>| {
            div()
                .w(px(44.))
                .flex_none()
                .pr_2()
                .text_right()
                .text_color(theme.muted_foreground)
                .child(n.map(|n| n.to_string()).unwrap_or_default())
        };
        range
            .filter_map(|ix| {
                let item = self.items.get(ix)?;
                let row = h_flex().id(ix).h(line_height).w_full().whitespace_nowrap();
                Some(match *item {
                    Item::Gap => row.into_any_element(),
                    Item::Header(f) => {
                        let file = &self.files[f];
                        // Its button clear of the scrollbar.
                        row.pl_2()
                            .pr(Scrollbar::width())
                            .gap_2()
                            .bg(theme.secondary)
                            .border_y_1()
                            .border_color(theme.border)
                            .font_family(theme.font_family.clone())
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(file.rel.clone()),
                            )
                            .child(
                                div()
                                    .text_color(theme.green)
                                    .child(format!("+{}", file.added)),
                            )
                            .child(
                                div()
                                    .text_color(theme.red)
                                    .child(format!("−{}", file.removed)),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new(("open-diff", f))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::GitCompare))
                                    .label("Open in Tab")
                                    .on_click(cx.listener(move |this, _, _, cx| this.open(f, cx))),
                            )
                            // A double-click opens it in a tab too.
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                if event.click_count() >= 2 {
                                    this.open(f, cx);
                                }
                            }))
                            .into_any_element()
                    }
                    Item::Note(f) => row
                        .px_2()
                        .italic()
                        .text_color(theme.muted_foreground)
                        .child(self.files[f].note.clone().unwrap_or_default())
                        .into_any_element(),
                    Item::More(n) => row
                        .px_2()
                        .italic()
                        .text_color(theme.muted_foreground)
                        .child(format!("{n} more files; pick one in the list to see it."))
                        .into_any_element(),
                    Item::Fold(f, start, end) => row
                        .px_2()
                        .gap_2()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .bg(theme.muted.opacity(0.25))
                        .hover(|this| this.bg(theme.muted.opacity(0.5)))
                        .child(format!("⋯  {} unchanged lines", end - start))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.opened.insert((f, start));
                            this.lay_out();
                            cx.notify();
                        }))
                        .into_any_element(),
                    Item::Line(f, l, fog) => {
                        let file = &self.files[f];
                        let line = &file.lines[l];
                        let background = match line.kind {
                            Side::Removed => theme.red.opacity(0.18),
                            Side::Added => theme.green.opacity(0.18),
                            _ => transparent_black(),
                        };
                        let highlights = file.styles[line.side]
                            .get(line.number - 1)
                            .cloned()
                            .unwrap_or_default();
                        row.bg(background)
                            .opacity(fog)
                            .child(number(line.old))
                            .child(number(line.new))
                            .child(div().flex_1().min_w_0().overflow_hidden().child(
                                div().ml(-self.h_scroll).child(
                                    StyledText::new(line.text.clone()).with_highlights(highlights),
                                ),
                            ))
                            .into_any_element()
                    }
                })
            })
            .collect()
    }
}

/// Which lines are near enough to a change to show.
fn shown_lines(lines: &[Line]) -> Vec<bool> {
    let mut shown = vec![false; lines.len()];
    let mut last: Option<usize> = None;
    for (ix, line) in lines.iter().enumerate() {
        if line.kind != Side::Same {
            last = Some(ix);
        }
        shown[ix] = last.is_some_and(|c| ix - c <= CONTEXT);
    }
    let mut next: Option<usize> = None;
    for ix in (0..lines.len()).rev() {
        if lines[ix].kind != Side::Same {
            next = Some(ix);
        }
        shown[ix] |= next.is_some_and(|c| c - ix <= CONTEXT);
    }
    shown
}

/// Fade the unchanged lines next to a fold, the nearest the most.
fn fog(items: &mut [Item], lines: &[Line]) {
    let folds: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item, Item::Fold(..)))
        .map(|(ix, _)| ix)
        .collect();
    for (ix, item) in items.iter_mut().enumerate() {
        if let Item::Line(_, l, fade) = item
            && lines[*l].kind == Side::Same
        {
            let away = folds.iter().map(|&f| f.abs_diff(ix) - 1).min();
            if let Some(&opacity) = away.and_then(|d| FOG.get(d)) {
                *fade = opacity;
            }
        }
    }
}

/// One file's change, read and laid out as unified lines.
fn read(top: &Path, diff: &Diff, theme: &gpui_kit::component::highlighter::HighlightTheme) -> File {
    let rel = rel_in(top, &diff.path);
    let path = top.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let commit = diff.hash.clone().map(|hash| crate::diff::CommitRevs {
        hash,
        parent: diff.parent.clone(),
        old_rel: diff.old_path.clone().unwrap_or_else(|| rel.clone()),
    });
    let mut file = File {
        rel,
        lines: Vec::new(),
        styles: Default::default(),
        added: 0,
        removed: 0,
        note: None,
    };
    let (old, new) = match load_sides(top, &file.rel, &path, diff.staged, commit.as_ref()) {
        Ok(sides) => sides,
        Err(err) => {
            file.note = Some(err);
            return file;
        }
    };
    if old.contains('\0') || new.contains('\0') {
        file.note = Some("A binary file.".into());
        return file;
    }
    let rows = rows(&old, &new);
    for (row, side) in inline_lines(&rows) {
        let row = &rows[row];
        let (kind, line) = if side == 0 {
            (row.left_kind, &row.left)
        } else {
            (row.right_kind, &row.right)
        };
        let Some((number, text)) = line else { continue };
        let (old, new) = match kind {
            Side::Same => (row.left.as_ref().map(|l| l.0), Some(*number)),
            Side::Removed => (Some(*number), None),
            _ => (None, Some(*number)),
        };
        match kind {
            Side::Removed => file.removed += 1,
            Side::Added => file.added += 1,
            _ => {}
        }
        file.lines.push(Line {
            old,
            new,
            text: text.replace('\t', "    "),
            kind,
            side,
            number: *number,
        });
    }
    if file.added + file.removed == 0 {
        file.note = Some("No changes to its text.".into());
        return file;
    }
    let language = crate::panels::language_of(&path);
    file.styles =
        [&old, &new].map(|text| highlight_lines(&text.replace("\r\n", "\n"), &language, theme));
    file
}

impl Render for DiffList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let font_size = px(Settings::get(cx).editor_font_size);
        if self.loading && self.files.is_empty() {
            return div()
                .size_full()
                .p_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Loading…")
                .into_any_element();
        }
        let text = window.text_system();
        let char_width = text
            .advance(
                text.resolve_font(&font(theme.mono_font_family.clone())),
                font_size,
                'm',
            )
            .map_or(font_size * 0.6, |s| s.width);
        let widest = char_width * self.widest as f32;
        div()
            .relative()
            .size_full()
            .font_family(theme.mono_font_family.clone())
            .text_size(font_size)
            // Sideways (Shift+wheel, a touchpad) moves the lines across.
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, window, cx| {
                    let delta = event.delta.pixel_delta(window.line_height());
                    if !delta.x.is_zero() {
                        this.h_scroll = (this.h_scroll - delta.x).clamp(px(0.), widest);
                        cx.notify();
                    }
                }),
            )
            .child({
                let mut list = uniform_list(
                    "diff-list",
                    self.items.len(),
                    cx.processor(|this, range, _, cx| this.render_items(range, cx)),
                )
                .track_scroll(&self.scroll)
                .size_full();
                list.style().restrict_scroll_to_axis = Some(true);
                list
            })
            .child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Always))
            .into_any_element()
    }
}
