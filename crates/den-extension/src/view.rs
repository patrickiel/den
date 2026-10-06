//! Views: tabs of an extension's own, described as data and drawn by den.
//!
//! A view is declared in the manifest ([`View`](crate::View)) and opened in
//! a window with [`Host::open_view`](crate::Host::open_view). What it shows
//! is a [`Content`]: a toolbar, a list of rows in columns (a cell holds
//! styled text, badges, or a [`Graph`] of lanes), right-click menus, and a
//! details pane under the list. The extension sends it with
//! [`Host::set_view`](crate::Host::set_view); every field it sets replaces
//! that part, the ones it leaves out stay as they are. Clicks, menu picks and
//! typing come back as [`events::VIEW_ACTION`](crate::events::VIEW_ACTION).
//!
//! den keeps the list virtual, so thousands of rows are fine; send them again
//! only when they change, and the details pane or the selection on their own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The actions den sends by itself, in [`events::VIEW_ACTION`](crate::events::VIEW_ACTION)'s
/// `action`. They start with `:`, so they never clash with an extension's ids.
pub mod actions {
    /// A row was clicked or picked with the arrow keys: `row` is its id, or
    /// null when the details pane was closed.
    pub const SELECT: &str = ":select";
    /// A row was double-clicked, or Enter pressed on it: `row`.
    pub const OPEN: &str = ":open";
    /// The "Load more" row at the end of the list ([`Content::more`](super::Content::more)).
    pub const MORE: &str = ":more";
}

/// What a view shows. Every field is optional: [`Host::set_view`](crate::Host::set_view)
/// replaces the parts that are set and keeps the rest.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Content {
    /// The tab's label; the manifest's title until set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Above the list, left to right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolbar: Option<Vec<ToolbarItem>>,
    /// The list's columns; a row's cells go in them in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<Column>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<Row>>,
    /// Right-click menus by name, for [`Row::menu`] and [`Span::menu`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menus: Option<BTreeMap<String, Vec<MenuItem>>>,
    /// The selected row's id; empty for none. den scrolls to it when it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    /// Rows marked as found (a search's matches), by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlighted: Option<Vec<String>>,
    /// The details pane under the list, line by line; empty closes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Vec<Line>>,
    /// Shown in place of the list while it has no rows (loading, an error,
    /// nothing found); empty for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Whether the list ends in a "Load more" row, which sends [`actions::MORE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more: Option<bool>,
}

impl Content {
    /// Lay `patch` over this: what it sets replaces what is here.
    pub fn merge(&mut self, patch: Content) {
        macro_rules! take {
            ($($field:ident),*) => { $( if patch.$field.is_some() { self.$field = patch.$field; } )* };
        }
        take!(title, toolbar, columns, rows, menus, selected, highlighted, detail, message, more);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Column {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Its width in pixels; none shares the space left with the other such
    /// columns. A column of graphs without one is as wide as its widest graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// The extension's own name for it, sent back with its actions.
    pub id: String,
    pub cells: Vec<Cell>,
    /// The name of its right-click menu in [`Content::menus`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub menu: String,
    /// Drawn faded, as for something not in the current branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
    /// Drawn bold, as for the current commit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
}

/// A cell: a [`Graph`] when set, else its spans one after another.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<Graph>,
}

impl From<Vec<Span>> for Cell {
    fn from(spans: Vec<Span>) -> Self {
        Cell { spans, graph: None }
    }
}

impl From<Span> for Cell {
    fn from(span: Span) -> Self {
        Cell { spans: vec![span], graph: None }
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Span::text(text).into()
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Span::text(text).into()
    }
}

impl From<Graph> for Cell {
    fn from(graph: Graph) -> Self {
        Cell { spans: Vec::new(), graph: Some(graph) }
    }
}

/// A run of text, or a badge when it has a `background`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Span {
    #[serde(default)]
    pub text: String,
    /// A [Lucide](https://lucide.dev/icons) icon's name, before the text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// `#rrggbb`; the theme's text colour when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
    /// `#rrggbb`: drawn as a rounded badge on this colour.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub background: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    /// In the editor's font.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mono: bool,
    /// In the theme's muted colour.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tooltip: String,
    /// A click sends this action, with `data`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    /// The name of its own right-click menu in [`Content::menus`], in place of the row's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub menu: String,
    /// Sent with its action and its menu's picks (a branch's name).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
}

impl Span {
    pub fn text(text: impl Into<String>) -> Self {
        Span { text: text.into(), ..Default::default() }
    }

    pub fn color(mut self, color: impl Into<String>) -> Self {
        self.color = color.into();
        self
    }

    pub fn background(mut self, color: impl Into<String>) -> Self {
        self.background = color.into();
        self
    }

    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = tooltip.into();
        self
    }

    pub fn action(mut self, action: impl Into<String>, data: impl Into<String>) -> Self {
        self.action = action.into();
        self.data = data.into();
        self
    }

    pub fn menu(mut self, menu: impl Into<String>, data: impl Into<String>) -> Self {
        self.menu = menu.into();
        self.data = data.into();
        self
    }
}

/// Lines and dots drawn across a row, in lanes: `x` counts lanes from the
/// left (a lane's middle is at `x`), `y` runs from the row's top (0) to its
/// bottom (1). A git graph draws each commit as a dot at `y` 0.5 and its
/// branches as lines through the rows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<GraphLine>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dots: Vec<GraphDot>,
}

impl Graph {
    /// How many lanes it reaches into.
    pub fn lanes(&self) -> f32 {
        let lines = self.lines.iter().map(|l| l.x0.max(l.x1));
        let dots = self.dots.iter().map(|d| d.x);
        lines.chain(dots).fold(-1.0, f32::max) + 1.0
    }
}

/// From (`x0`, `y0`) to (`x1`, `y1`); a line that changes lanes is drawn as
/// a curve.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphLine {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// `#rrggbb`.
    pub color: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dashed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphDot {
    pub x: f32,
    /// `#rrggbb`.
    pub color: String,
    /// A ring rather than a filled dot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hollow: bool,
}

/// A line of the details pane: spans that wrap.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Line {
    #[serde(default)]
    pub spans: Vec<Span>,
    /// Indented this many steps.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub indent: u32,
    /// A click on the line (not on one of its spans' own actions) sends this, with `data`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    /// The name of its right-click menu in [`Content::menus`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub menu: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
    /// A heading, set apart from the line above.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub heading: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Line {
    pub fn new(spans: Vec<Span>) -> Self {
        Line { spans, ..Default::default() }
    }

    pub fn heading(text: impl Into<String>) -> Self {
        Line { spans: vec![Span::text(text)], heading: true, ..Default::default() }
    }

    pub fn action(mut self, action: impl Into<String>, data: impl Into<String>) -> Self {
        self.action = action.into();
        self.data = data.into();
        self
    }

    pub fn menu(mut self, menu: impl Into<String>) -> Self {
        self.menu = menu.into();
        self
    }

    pub fn indent(mut self, indent: u32) -> Self {
        self.indent = indent;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolbarKind {
    /// Sends its `id` when clicked.
    #[default]
    Button,
    /// A dropdown of `options`; picking one sends its `id` with the option's
    /// value as `value`.
    Select,
    /// A text box; each change sends its `id` with the text as `value`.
    Search,
    /// Text, nothing to click.
    Label,
    /// Pushes what follows to the right.
    Spacer,
    /// A kind a newer den knows; this one leaves it out.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolbarItem {
    #[serde(default)]
    pub kind: ToolbarKind,
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// A [Lucide](https://lucide.dev/icons) icon's name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tooltip: String,
    /// A select's chosen option, or a search's text when den first shows it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// A select's options.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SelectOption>,
    /// A search's placeholder.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    /// A button drawn as switched on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub active: bool,
}

impl ToolbarItem {
    pub fn button(id: impl Into<String>, label: impl Into<String>, icon: impl Into<String>) -> Self {
        ToolbarItem { kind: ToolbarKind::Button, id: id.into(), label: label.into(), icon: icon.into(), ..Default::default() }
    }

    pub fn select(id: impl Into<String>, value: impl Into<String>, options: Vec<SelectOption>) -> Self {
        ToolbarItem { kind: ToolbarKind::Select, id: id.into(), value: value.into(), options, ..Default::default() }
    }

    pub fn search(id: impl Into<String>, placeholder: impl Into<String>) -> Self {
        ToolbarItem { kind: ToolbarKind::Search, id: id.into(), placeholder: placeholder.into(), ..Default::default() }
    }

    pub fn label(text: impl Into<String>) -> Self {
        ToolbarItem { kind: ToolbarKind::Label, label: text.into(), ..Default::default() }
    }

    pub fn spacer() -> Self {
        ToolbarItem { kind: ToolbarKind::Spacer, ..Default::default() }
    }

    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = tooltip.into();
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// An item of a right-click menu: one that sends `id` (with the row's id
/// and the span's or line's `data`), a separator, or a submenu.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MenuItem {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub separator: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submenu: Vec<MenuItem>,
}

impl MenuItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        MenuItem { id: id.into(), label: label.into(), ..Default::default() }
    }

    pub fn separator() -> Self {
        MenuItem { separator: true, ..Default::default() }
    }

    pub fn submenu(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        MenuItem { label: label.into(), submenu: items, ..Default::default() }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// A question den asks in a dialog, with [`Host::prompt`](crate::Host::prompt).
/// The answer comes back as [`events::PROMPT_ANSWERED`](crate::events::PROMPT_ANSWERED).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Prompt {
    /// The extension's own name for it, sent back with the answer.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// The OK button's text; "OK" when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ok: String,
    /// The OK button in the danger colour, for what can't be undone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub danger: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    /// A text box; its value is the text.
    #[default]
    Text,
    /// A checkbox; its value is `true` or `false`.
    Checkbox,
    /// One of `options` (as radio buttons); its value is the option's value.
    Choice,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Its key in the answer's `values`.
    pub id: String,
    #[serde(default)]
    pub kind: FieldKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// What it starts as: text, `"true"`/`"false"`, or an option's value.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SelectOption>,
    /// A text box that can't be left empty: OK does nothing until it is filled.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

impl Field {
    pub fn text(id: impl Into<String>, label: impl Into<String>, value: impl Into<String>) -> Self {
        Field { id: id.into(), kind: FieldKind::Text, label: label.into(), value: value.into(), ..Default::default() }
    }

    pub fn checkbox(id: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Field { id: id.into(), kind: FieldKind::Checkbox, label: label.into(), value: checked.to_string(), ..Default::default() }
    }

    pub fn choice(id: impl Into<String>, label: impl Into<String>, value: impl Into<String>, options: Vec<SelectOption>) -> Self {
        Field { id: id.into(), kind: FieldKind::Choice, label: label.into(), value: value.into(), options, ..Default::default() }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }
}

/// A change to show side by side, with [`Host::open_diff`](crate::Host::open_diff).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Diff {
    /// The file, relative to the repository's top or absolute.
    pub path: String,
    /// A commit's change to it; none for the uncommitted change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// The commit's parent to compare with; none for a root commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The file's path in the parent, when it was renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Without `hash`: the staged change (index against HEAD) rather than
    /// the working tree's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staged: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_replaces_only_what_is_set() {
        let mut content = Content { title: Some("A".into()), rows: Some(vec![Row { id: "1".into(), ..Default::default() }]), ..Default::default() };
        content.merge(Content { selected: Some("1".into()), ..Default::default() });
        assert_eq!(content.title.as_deref(), Some("A"));
        assert_eq!(content.rows.as_ref().map(Vec::len), Some(1));
        assert_eq!(content.selected.as_deref(), Some("1"));
        content.merge(Content { rows: Some(Vec::new()), ..Default::default() });
        assert_eq!(content.rows.as_ref().map(Vec::len), Some(0));
    }

    #[test]
    fn reads_terse_json() {
        let content: Content = serde_json::from_str(
            r##"{"rows":[{"id":"a","cells":[{"spans":[{"text":"main","background":"#0e639c"}]},{"graph":{"dots":[{"x":2,"color":"#fff"}]}}]}],
                "toolbar":[{"kind":"search","id":"find"},{"kind":"from-the-future"}]}"##,
        )
        .unwrap();
        let rows = content.rows.unwrap();
        assert_eq!(rows[0].cells[0].spans[0].background, "#0e639c");
        assert_eq!(rows[0].cells[1].graph.as_ref().unwrap().lanes(), 3.0);
        let toolbar = content.toolbar.unwrap();
        assert_eq!((toolbar[0].kind, toolbar[1].kind), (ToolbarKind::Search, ToolbarKind::Unknown));
    }
}
