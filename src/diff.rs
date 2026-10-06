//! Diff tabs, as in den: a change clicked in Source Control opens read-only,
//! side by side. `name (Working Tree)` compares the file on disk with the
//! index, `name (Index)` the index with HEAD. The header's button (or
//! Ctrl+Enter) opens the file itself.

use std::{
    cell::Cell,
    ops::Range,
    rc::Rc,
    path::{Path, PathBuf},
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::{
    backend::git,
    pane::{Pane, PaneEvent},
    settings::Settings,
};

pub const DIFF: &str = "Diff";

/// The line numbers' column on each side.
const GUTTER: Pixels = px(48.);

actions!(diff, [OpenDiffedFile]);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-enter", OpenDiffedFile, Some("Diff"))]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Removed,
    Added,
    Same,
    /// No line on this side opposite a line on the other.
    Gap,
}

/// One row of the side-by-side view: a line (number, text) on each side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    pub left_kind: Side,
    pub right_kind: Side,
}

/// Line up two texts: equal lines side by side, a run of removed lines next
/// to the run of added lines that replaced it, gaps where one side has more.
pub fn rows(old: &str, new: &str) -> Vec<Row> {
    // git keeps LF where the working copy has CRLF (core.autocrlf): compare
    // the lines, not their endings.
    let (old, new) = (old.replace("\r\n", "\n"), new.replace("\r\n", "\n"));
    let diff = TextDiff::from_lines(&old, &new);
    let mut out = Vec::new();
    let mut removed: Vec<(usize, String)> = Vec::new();
    let mut added: Vec<(usize, String)> = Vec::new();
    let flush = |removed: &mut Vec<(usize, String)>, added: &mut Vec<(usize, String)>, out: &mut Vec<Row>| {
        let n = removed.len().max(added.len());
        let mut r = removed.drain(..);
        let mut a = added.drain(..);
        for _ in 0..n {
            let left = r.next();
            let right = a.next();
            out.push(Row {
                left_kind: if left.is_some() { Side::Removed } else { Side::Gap },
                right_kind: if right.is_some() { Side::Added } else { Side::Gap },
                left,
                right,
            });
        }
    };
    for change in diff.iter_all_changes() {
        let text = change.value().trim_end_matches(['\n', '\r']).to_string();
        match change.tag() {
            ChangeTag::Delete => removed.push((change.old_index().unwrap_or(0) + 1, text)),
            ChangeTag::Insert => added.push((change.new_index().unwrap_or(0) + 1, text)),
            ChangeTag::Equal => {
                flush(&mut removed, &mut added, &mut out);
                out.push(Row {
                    left: Some((change.old_index().unwrap_or(0) + 1, text.clone())),
                    right: Some((change.new_index().unwrap_or(0) + 1, text)),
                    left_kind: Side::Same,
                    right_kind: Side::Same,
                });
            }
        }
    }
    flush(&mut removed, &mut added, &mut out);
    out
}

/// What a commit diff compares.
#[derive(Clone, Debug)]
pub struct CommitRevs {
    pub hash: String,
    /// None for a root commit (everything is added).
    pub parent: Option<String>,
    /// The file's path in the parent (differs after a rename).
    pub old_rel: String,
}

pub struct DiffPanel {
    path: PathBuf,
    top: PathBuf,
    rel: String,
    /// The index against HEAD, else the working tree against the index.
    staged: bool,
    /// A commit's change instead: the commit, its parent, the path before.
    commit: Option<CommitRevs>,
    rows: Vec<Row>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// While the ruler's thumb is dragged: how far below its top it is held.
    ruler_grab: Option<Pixels>,
    /// How far each side (left, right) is scrolled sideways, as in VS Code
    /// where the two editors scroll across on their own.
    h_scroll: [Pixels; 2],
    /// Each side's longest line, in characters.
    widest: [usize; 2],
    /// A character's width in the editor font; set as the view renders.
    char_width: Pixels,
    /// Where the rows are, caught as they paint.
    list_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// While a side's scrollbar thumb is dragged: the side, and how far
    /// right of the thumb's left edge it is held.
    h_grab: Option<(usize, Pixels)>,
}

impl DiffPanel {
    pub fn new(path: PathBuf, top: PathBuf, rel: String, staged: bool, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            path,
            top,
            rel,
            staged,
            commit: None,
            rows: Vec::new(),
            error: None,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            ruler_grab: None,
            h_scroll: [px(0.); 2],
            widest: [0; 2],
            char_width: px(8.),
            list_bounds: Default::default(),
            h_grab: None,
        };
        this.reload();
        this
    }

    /// A commit's change to one file.
    pub fn for_commit(path: PathBuf, top: PathBuf, rel: String, commit: CommitRevs, cx: &mut Context<Self>) -> Self {
        let mut this = Self::new(path, top, rel, false, cx);
        this.commit = Some(commit);
        this.reload();
        this
    }

    pub fn commit(&self) -> Option<&CommitRevs> {
        self.commit.as_ref()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn staged(&self) -> bool {
        self.staged
    }

    /// Read both sides again (after the file or the index changed).
    pub fn reload(&mut self) {
        let side = |rev: &str| git::show(&self.top, rev, &self.rel).unwrap_or_default();
        let (old, new) = if let Some(commit) = &self.commit {
            let old = commit.parent.as_deref().map(|p| git::show(&self.top, p, &commit.old_rel).unwrap_or_default()).unwrap_or_default();
            (old, side(&commit.hash))
        } else if self.staged {
            (side("HEAD"), side(""))
        } else {
            (side(""), std::fs::read_to_string(&self.path).unwrap_or_default())
        };
        self.error = None;
        self.rows = rows(&old, &new);
        let width = |line: &Option<(usize, String)>| line.as_ref().map_or(0, |(_, t)| t.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum());
        self.widest = [
            self.rows.iter().map(|r| width(&r.left)).max().unwrap_or(0),
            self.rows.iter().map(|r| width(&r.right)).max().unwrap_or(0),
        ];
    }

    /// How far `side` can scroll across: its longest line past the room
    /// beside the line numbers.
    fn max_h_scroll(&self, side: usize) -> Pixels {
        let room = self.h_track(side).1;
        (self.char_width * (self.widest[side] + 2) as f32 - room).max(px(0.))
    }

    /// A side's scrollbar track, under its text (not its line numbers): its
    /// left edge and width.
    fn h_track(&self, side: usize) -> (Pixels, Pixels) {
        let bounds = self.list_bounds.get();
        let half = (bounds.size.width - px(1.)) / 2.;
        (bounds.left() + (half + px(1.)) * side as f32 + GUTTER, (half - GUTTER).max(px(0.)))
    }

    /// A side's scrollbar thumb: its left edge and width.
    fn h_thumb(&self, side: usize) -> (Pixels, Pixels) {
        let (left, room) = self.h_track(side);
        let max = self.max_h_scroll(side);
        if max <= px(0.) {
            return (left, room);
        }
        let width = (room * (room / (room + max))).max(px(24.)).min(room);
        (left + (room - width) * (self.h_scroll[side] / max).clamp(0., 1.), width)
    }

    /// Scroll so the held thumb follows the mouse at `x`.
    fn drag_across(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((side, grab)) = self.h_grab else { return };
        let (left, room) = self.h_track(side);
        let width = self.h_thumb(side).1;
        let track = room - width;
        if track <= px(0.) {
            return;
        }
        self.h_scroll[side] = self.max_h_scroll(side) * ((x - grab - left) / track).clamp(0., 1.);
        cx.notify();
    }

    /// The sideways scrollbars, one under each side's text, shown where the
    /// side's lines run past it. Drag the thumb, or press the track to bring
    /// the thumb there.
    fn render_h_scrollbars(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (thumb_color, active_color) = (theme.foreground.opacity(0.2), theme.foreground.opacity(0.35));
        let origin = self.list_bounds.get().left();
        div().children((0..2).filter(|&side| self.max_h_scroll(side) > px(0.)).map(|side| {
            let (left, room) = self.h_track(side);
            let (thumb_left, thumb_width) = self.h_thumb(side);
            let held = matches!(self.h_grab, Some((s, _)) if s == side);
            div()
                .id(("diff-h-scroll", side))
                .absolute()
                .bottom_0()
                .left(left - origin)
                .w(room)
                .h(px(10.))
                .child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .left(thumb_left - left)
                        .w(thumb_width)
                        .h(px(6.))
                        .rounded_full()
                        .bg(if held { active_color } else { thumb_color }),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        let (thumb_left, thumb_width) = this.h_thumb(side);
                        let x = event.position.x;
                        // Held where it was taken; pressed elsewhere, centred there.
                        let grab = if x >= thumb_left && x <= thumb_left + thumb_width { x - thumb_left } else { thumb_width / 2. };
                        this.h_grab = Some((side, grab));
                        this.drag_across(x, cx);
                        cx.stop_propagation();
                    }),
                )
        }))
    }

    fn scroll_across(&mut self, side: usize, delta: Pixels, cx: &mut Context<Self>) {
        let to = (self.h_scroll[side] - delta).clamp(px(0.), self.max_h_scroll(side));
        if to != self.h_scroll[side] {
            self.h_scroll[side] = to;
            cx.notify();
        }
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let line_height = px(Settings::get(cx).editor_font_size * 1.4);
        let h_scroll = self.h_scroll;
        let half = |line: &Option<(usize, String)>, kind: Side, side: usize| {
            let bg = match kind {
                Side::Removed => theme.red.opacity(0.18),
                Side::Added => theme.green.opacity(0.18),
                Side::Gap => theme.muted.opacity(0.3),
                Side::Same => transparent_black(),
            };
            h_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .bg(bg)
                .overflow_hidden()
                .whitespace_nowrap()
                // Sideways (Shift+wheel, a touchpad) moves only this side.
                .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, window, cx| {
                    let delta = event.delta.pixel_delta(window.line_height());
                    if !delta.x.is_zero() {
                        this.scroll_across(side, delta.x, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .w(GUTTER)
                        .flex_none()
                        .pr_2()
                        .text_right()
                        .text_color(theme.muted_foreground)
                        .child(line.as_ref().map(|(n, _)| n.to_string()).unwrap_or_default()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(div().ml(-h_scroll[side]).child(line.as_ref().map(|(_, t)| t.clone()).unwrap_or_default())),
                )
        };
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                Some(
                    h_flex()
                        .h(line_height)
                        .w_full()
                        .child(half(&row.left, row.left_kind, 0))
                        .child(div().w(px(1.)).h_full().bg(theme.border))
                        .child(half(&row.right, row.right_kind, 1))
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl EventEmitter<PaneEvent> for DiffPanel {}

impl Focusable for DiffPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Pane for DiffPanel {
    fn kind(&self) -> &'static str {
        DIFF
    }

    fn icon_element(&self, cx: &App) -> Option<AnyElement> {
        Some(crate::file_icon::render(self.rel.rsplit('/').next().unwrap_or(&self.rel), 16., cx))
    }

    fn icon(&self, _: &App) -> IconName {
        IconName::GitCompare
    }

    fn label(&self, _: &App) -> SharedString {
        let name = self.rel.rsplit('/').next().unwrap_or(&self.rel);
        let side = match &self.commit {
            Some(commit) => commit.hash.chars().take(7).collect(),
            None if self.staged => "Index".to_string(),
            None => "Working Tree".to_string(),
        };
        format!("{name} ({side})").into()
    }

    fn dump(&self, _: &App) -> Value {
        json!({
            "path": self.path.to_string_lossy(),
            "top": self.top.to_string_lossy(),
            "rel": self.rel,
            "staged": self.staged,
            "hash": self.commit.as_ref().map(|c| c.hash.clone()),
            "parent": self.commit.as_ref().and_then(|c| c.parent.clone()),
            "old_rel": self.commit.as_ref().map(|c| c.old_rel.clone()),
        })
    }
}

impl Render for DiffPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let short = |rev: &str| rev.chars().take(7).collect::<String>();
        let (left, right) = match &self.commit {
            Some(commit) => (commit.parent.as_deref().map_or("(none)".to_string(), short), short(&commit.hash)),
            None if self.staged => ("HEAD".into(), "Index".into()),
            None => ("Index".into(), "Working Tree".into()),
        };
        let font_size = px(Settings::get(cx).editor_font_size);
        let text = window.text_system();
        let font_id = text.resolve_font(&font(theme.mono_font_family.clone()));
        self.char_width = text.advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(font_size * 0.6);
        // The longest lines or the view may have changed since last scrolled.
        for side in 0..2 {
            self.h_scroll[side] = self.h_scroll[side].min(self.max_h_scroll(side));
        }
        let list_bounds = self.list_bounds.clone();
        let this = cx.weak_entity();
        let path = self.path.clone();
        v_flex()
            .id("diff")
            .key_context("Diff")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(move |_, _: &OpenDiffedFile, _, cx| {
                cx.emit(PaneEvent::OpenFile {
                    path: path.clone(),
                    line: None,
                    column: None,
                });
            }))
            .child(
                h_flex()
                    .flex_none()
                    .h(px(28.))
                    .px_2()
                    .gap_2()
                    .text_xs()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().child(format!("{} — {left} ↔ {right}", self.rel)))
                    .child(
                        Button::new("open-file")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::FileText))
                            .label("Open File")
                            .tooltip("Open the file (Ctrl+Enter)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(PaneEvent::OpenFile {
                                    path: this.path.clone(),
                                    line: None,
                                    column: None,
                                });
                            })),
                    ),
            )
            .when_some(self.error.clone(), |this, error| this.child(div().p_2().text_color(theme.danger).child(error)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    // The ruler follows the list as it scrolls.
                    .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .font_family(theme.mono_font_family.clone())
                            .text_size(font_size)
                            .child(
                                {
                                    let mut list = uniform_list("diff-rows", self.rows.len(), cx.processor(|this, range, _, cx| this.render_rows(range, cx)))
                                        .track_scroll(&self.scroll)
                                        .size_full();
                                    // Sideways is each side's own, not the list's
                                    // (it would turn into scrolling down).
                                    list.style().restrict_scroll_to_axis = Some(true);
                                    list
                                },
                            )
                            .child(
                                canvas(
                                    move |area, _, _| list_bounds.set(area),
                                    // A drag goes on outside the bar: the
                                    // window keeps the mouse while it's down.
                                    move |_, _, window, _| {
                                        window.on_mouse_event({
                                            let this = this.clone();
                                            move |event: &MouseMoveEvent, phase, _, cx| {
                                                if phase == DispatchPhase::Capture {
                                                    _ = this.update(cx, |this, cx| {
                                                        if event.pressed_button == Some(MouseButton::Left) {
                                                            this.drag_across(event.position.x, cx);
                                                        } else if this.h_grab.take().is_some() {
                                                            cx.notify();
                                                        }
                                                    });
                                                }
                                            }
                                        });
                                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                                                _ = this.update(cx, |this, cx| {
                                                    if this.h_grab.take().is_some() {
                                                        cx.notify();
                                                    }
                                                });
                                            }
                                        });
                                    },
                                )
                                .absolute()
                                .size_full(),
                            )
                            .child(self.render_h_scrollbars(cx)),
                    )
                    .child(self.render_ruler(cx)),
            )
    }
}

/// The ruler's thumb (the part in view) within `area`: its top, its height
/// and the list's furthest scroll offset.
fn thumb(scroll: &UniformListScrollHandle, area: Bounds<Pixels>) -> (Pixels, Pixels, Pixels) {
    use gpui_kit::base::ScrollbarHandle;
    let viewport = ScrollbarHandle::viewport_bounds(scroll).size.height;
    let content = ScrollbarHandle::content_size(scroll).height.max(viewport);
    let max_offset = content - viewport;
    let height = if max_offset > px(0.) { (area.size.height * (viewport / content)).max(px(16.)).min(area.size.height) } else { area.size.height };
    let at = if max_offset > px(0.) { (-ScrollbarHandle::offset(scroll).y / max_offset).clamp(0., 1.) } else { 0. };
    (area.top() + (area.size.height - height) * at, height, max_offset)
}

impl DiffPanel {
    /// The overview ruler, as VS Code's, and the view's scrollbar: where the
    /// changes are in the whole file (removed lines red on the left, added
    /// green on the right), the part in view shaded. Drag the shaded part,
    /// or press elsewhere to bring the view there.
    fn render_ruler(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (red, green, view_color) = (theme.red, theme.green, theme.foreground.opacity(0.12));
        let total = self.rows.len().max(1);
        let marks: Vec<(usize, bool, bool)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(ix, row)| {
                let removed = row.left_kind == Side::Removed;
                let added = row.right_kind == Side::Added;
                (removed || added).then_some((ix, removed, added))
            })
            .collect();
        let scroll = self.scroll.clone();
        let this = cx.weak_entity();
        let bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>> = Default::default();
        div()
            .id("diff-ruler")
            .flex_none()
            .w(px(14.))
            .h_full()
            .border_l_1()
            .border_color(theme.border)
            .child(
                canvas(
                    {
                        let bounds = bounds.clone();
                        move |area, _, _| bounds.set(Some(area))
                    },
                    move |area, _, window, _| {
                        let height = area.size.height;
                        let row_height = (height / total as f32).max(px(2.));
                        let half = area.size.width / 2.;
                        // Read here, not in render: the list has been laid
                        // out by now, so the thumb is where this frame is.
                        let (top, thumb_height, _) = thumb(&scroll, area);
                        window.paint_quad(fill(Bounds::new(point(area.left(), top), size(area.size.width, thumb_height)), view_color));
                        for &(ix, removed, added) in &marks {
                            let y = area.top() + height * (ix as f32 / total as f32);
                            if removed {
                                window.paint_quad(fill(Bounds::new(point(area.left(), y), size(half, row_height)), red));
                            }
                            if added {
                                window.paint_quad(fill(Bounds::new(point(area.left() + half, y), size(half, row_height)), green));
                            }
                        }
                        // A drag goes on outside the ruler: the window keeps
                        // the mouse while the button is down.
                        window.on_mouse_event({
                            let this = this.clone();
                            move |event: &MouseMoveEvent, phase, _, cx| {
                                if phase == DispatchPhase::Capture {
                                    _ = this.update(cx, |this, cx| {
                                        if event.pressed_button == Some(MouseButton::Left) {
                                            this.drag_ruler(area, event.position.y, cx);
                                        } else {
                                            this.ruler_grab = None;
                                        }
                                    });
                                }
                            }
                        });
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                                _ = this.update(cx, |this, _| this.ruler_grab = None);
                            }
                        });
                    },
                )
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let Some(area) = bounds.get() else { return };
                    let (top, height, _) = thumb(&this.scroll, area);
                    let y = event.position.y;
                    // Held where it was taken; pressed elsewhere, centred there.
                    this.ruler_grab = Some(if y >= top && y <= top + height { y - top } else { height / 2. });
                    this.drag_ruler(area, y, cx);
                }),
            )
    }

    /// Scroll so the held thumb follows the mouse at `y`.
    fn drag_ruler(&mut self, area: Bounds<Pixels>, y: Pixels, cx: &mut Context<Self>) {
        use gpui_kit::base::ScrollbarHandle;
        let Some(grab) = self.ruler_grab else { return };
        let (_, height, max_offset) = thumb(&self.scroll, area);
        let track = area.size.height - height;
        if track <= px(0.) {
            return;
        }
        let at = ((y - grab - area.top()) / track).clamp(0., 1.);
        let x = ScrollbarHandle::offset(&self.scroll).x;
        ScrollbarHandle::set_offset(&self.scroll, point(x, -(max_offset * at)));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{Side, rows};

    #[test]
    fn replaced_lines_sit_side_by_side_with_gaps() {
        let rows = rows("a\nb\nc\n", "a\nB\nB2\nc\n");
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].left_kind, Side::Same);
        assert_eq!((rows[1].left_kind, rows[1].right_kind), (Side::Removed, Side::Added));
        assert_eq!(rows[1].left, Some((2, "b".into())));
        assert_eq!(rows[1].right, Some((2, "B".into())));
        assert_eq!((rows[2].left_kind, rows[2].right_kind), (Side::Gap, Side::Added));
        assert_eq!(rows[3].right, Some((4, "c".into())));
    }

    #[test]
    fn line_endings_are_not_changes() {
        let rows = rows("a\nb\n", "a\r\nb\r\n");
        assert!(rows.iter().all(|r| r.left_kind == Side::Same && r.right_kind == Side::Same));
    }

    #[test]
    fn a_new_file_is_all_added() {
        let rows = rows("", "x\ny\n");
        assert!(rows.iter().all(|r| r.left_kind == Side::Gap && r.right_kind == Side::Added));
    }
}
