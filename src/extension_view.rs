//! An extension's view: a tab of its own that it describes as data
//! (`den_extension::view::Content`) and den draws. The content lives in the
//! `Extensions` global, by extension, folder and view, so the tab only reads
//! it; what the user does goes back to the extension as `view_action`.
//!
//! Also the dialogs extensions ask with `prompt`.

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use den_extension::events;
use den_extension::view::{Content, FieldKind, Graph, Line, MenuItem, Prompt, Row, Span, ToolbarItem, ToolbarKind, actions};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    radio::Radio,
    resizable::{resizable_panel, v_resizable},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde_json::{Map, Value, json};

use crate::extensions::{self, Extensions};
use crate::pane::{Pane, PaneEvent};
use crate::git_graph::{GRAPH_PAD, LANE, graph_canvas};
use crate::preset_icon::parse_color;

pub const EXTENSION_VIEW: &str = "ExtensionView";

type Menus = std::collections::BTreeMap<String, Vec<MenuItem>>;

const ROW_HEIGHT: Pixels = px(24.);
/// A graph column is at least this wide, for its title.
const GRAPH_MIN: f32 = 56.;

pub struct ExtensionView {
    id: String,
    view: String,
    root: PathBuf,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// The toolbar's text boxes, by item id.
    searches: Vec<(String, Entity<InputState>)>,
    /// The selection last scrolled to, so a new one is scrolled to once.
    scrolled_to: Option<String>,
    /// What a right-click on a span or a line asked for (its menu and data),
    /// set before the row's menu is built; the row's own menu otherwise.
    menu_target: Rc<RefCell<Option<(String, String)>>>,
    _subscriptions: Vec<Subscription>,
}

impl ExtensionView {
    pub fn new(id: String, view: String, root: PathBuf, cx: &mut Context<Self>) -> Self {
        extensions::view_event(&id, events::VIEW_OPENED, json!({ "root": root, "view": view }), cx);
        let closed = (id.clone(), view.clone(), root.clone());
        cx.on_release(move |_, cx| {
            let (id, view, root) = &closed;
            extensions::view_event(id, events::VIEW_CLOSED, json!({ "root": root, "view": view }), cx);
        })
        .detach();
        ExtensionView {
            id,
            view,
            root,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            searches: Vec::new(),
            scrolled_to: None,
            menu_target: Rc::default(),
            _subscriptions: vec![cx.observe_global::<Extensions>(|_, cx| cx.notify()), cx.observe_global::<crate::settings::Settings>(|_, cx| cx.notify())],
        }
    }

    /// Whether it is extension `id`'s view `view`.
    pub fn is(&self, id: &str, view: &str) -> bool {
        self.id == id && self.view == view
    }

    fn content<'a>(&self, cx: &'a App) -> Option<&'a Content> {
        Extensions::get(cx).view(&self.id, &self.root, &self.view)
    }

    fn rows<'a>(&self, cx: &'a App) -> &'a [Row] {
        self.content(cx).and_then(|c| c.rows.as_deref()).unwrap_or_default()
    }

    fn send(&self, action: &str, row: Option<&str>, data: Option<&str>, value: Option<&str>, cx: &App) {
        let data = json!({ "root": self.root, "view": self.view, "action": action, "row": row, "data": data, "value": value });
        extensions::view_event(&self.id, events::VIEW_ACTION, data, cx);
    }

    /// Select a row (none closes the details) here at once, and tell the extension.
    fn select(&mut self, row: Option<String>, cx: &mut Context<Self>) {
        let content = cx.global_mut::<Extensions>().view_mut(&self.id, &self.root, &self.view);
        content.selected = Some(row.clone().unwrap_or_default());
        if row.is_none() {
            content.detail = Some(Vec::new());
        }
        self.scrolled_to = row.clone();
        self.send(actions::SELECT, row.as_deref(), None, None, cx);
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        if rows.is_empty() {
            return;
        }
        let selected = self.content(cx).and_then(|c| c.selected.as_deref());
        let current = selected.and_then(|s| rows.iter().position(|r| r.id == s));
        let page = 20;
        let last = rows.len() - 1;
        let next = match event.keystroke.key.as_str() {
            "up" => current.map_or(0, |ix| ix.saturating_sub(1)),
            "down" => current.map_or(0, |ix| (ix + 1).min(last)),
            "pageup" => current.map_or(0, |ix| ix.saturating_sub(page)),
            "pagedown" => current.map_or(0, |ix| (ix + page).min(last)),
            "home" => 0,
            "end" => last,
            "enter" => {
                if let Some(ix) = current {
                    let id = rows[ix].id.clone();
                    self.send(actions::OPEN, Some(&id), None, None, cx);
                }
                return cx.stop_propagation();
            }
            "escape" if current.is_some() => {
                self.select(None, cx);
                return cx.stop_propagation();
            }
            _ => return,
        };
        let id = rows[next].id.clone();
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        self.select(Some(id), cx);
        cx.stop_propagation();
    }

    /// A toolbar text box, made the first time it is drawn.
    fn search_input(&mut self, item: &ToolbarItem, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((_, input)) = self.searches.iter().find(|(id, _)| *id == item.id) {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(item.placeholder.clone()).default_value(item.value.clone()));
        let action = item.id.clone();
        self._subscriptions.push(cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                let value = input.read(cx).value().to_string();
                this.send(&action, None, None, Some(&value), cx);
            }
        }));
        self.searches.push((item.id.clone(), input.clone()));
        input
    }

    fn render_toolbar(&mut self, items: &[ToolbarItem], window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut bar = h_flex().flex_none().h(px(36.)).px_2().gap_1().border_b_1().border_color(theme.border);
        for item in items {
            let element = match item.kind {
                ToolbarKind::Button => {
                    let this = cx.weak_entity();
                    let action = item.id.clone();
                    let mut button = Button::new(SharedString::from(format!("tool-{}", item.id))).small().ghost().selected(item.active);
                    if let Some(icon) = crate::ui::lucide_icon(&item.icon, cx) {
                        button = button.icon(icon);
                    }
                    if !item.label.is_empty() {
                        button = button.label(item.label.clone());
                    }
                    let tooltip = if item.tooltip.is_empty() { item.label.clone() } else { item.tooltip.clone() };
                    if !tooltip.is_empty() {
                        button = button.tooltip(tooltip);
                    }
                    button
                        .on_click(move |_, _, cx| _ = this.update(cx, |this, cx| this.send(&action, None, None, None, cx)))
                        .into_any_element()
                }
                ToolbarKind::Select => {
                    let chosen = item.options.iter().find(|o| o.value == item.value).map_or(item.value.clone(), |o| o.label.clone());
                    let label = if item.label.is_empty() { chosen } else { format!("{}: {chosen}", item.label) };
                    let (options, current, action, this) = (item.options.clone(), item.value.clone(), item.id.clone(), cx.weak_entity());
                    let mut button = Button::new(SharedString::from(format!("tool-{}", item.id)))
                        .small()
                        .outline()
                        .label(label)
                        .icon(Icon::new(IconName::ChevronDown));
                    if !item.tooltip.is_empty() {
                        button = button.tooltip(item.tooltip.clone());
                    }
                    button
                        .dropdown_menu(move |mut menu, _, _| {
                            menu = menu.max_h(px(420.)).scrollable(true);
                            for option in &options {
                                let (this, action, value) = (this.clone(), action.clone(), option.value.clone());
                                menu = menu.item(PopupMenuItem::new(option.label.clone()).checked(option.value == current).on_click(move |_, _, cx| {
                                    _ = this.update(cx, |this, cx| this.send(&action, None, None, Some(&value), cx));
                                }));
                            }
                            menu
                        })
                        .into_any_element()
                }
                ToolbarKind::Search => {
                    let input = self.search_input(item, window, cx);
                    div()
                        .w(px(220.))
                        .child(Input::new(&input).small().cleanable(true).prefix(Icon::new(IconName::Search).xsmall().text_color(theme.muted_foreground)))
                        .into_any_element()
                }
                ToolbarKind::Label => div().px_1().text_sm().text_color(theme.muted_foreground).child(item.label.clone()).into_any_element(),
                ToolbarKind::Spacer => div().flex_1().into_any_element(),
                ToolbarKind::Unknown => continue,
            };
            bar = bar.child(element);
        }
        bar
    }

    /// Each column's width: fixed, a graph column's widest graph, or `None` to share the rest.
    fn column_widths(&self, cx: &App) -> Vec<Option<Pixels>> {
        let Some(content) = self.content(cx) else { return Vec::new() };
        let rows = content.rows.as_deref().unwrap_or_default();
        content
            .columns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                column.width.map(px).or_else(|| {
                    let lanes = rows.iter().filter_map(|r| r.cells.get(ix)?.graph.as_ref()).map(Graph::lanes).fold(0., f32::max);
                    (lanes > 0.).then(|| px((GRAPH_PAD * 2. + lanes * LANE).max(GRAPH_MIN)))
                })
            })
            .collect()
    }

    fn render_rows(&mut self, range: std::ops::Range<usize>, widths: &[Option<Pixels>], window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let Some(content) = self.content(cx) else { return Vec::new() };
        let total = content.rows.as_ref().map_or(0, Vec::len);
        // Only the rows on screen are copied out of the global.
        let rows: Vec<Row> = content.rows.as_deref().unwrap_or_default().get(range.start.min(total)..range.end.min(total)).unwrap_or_default().to_vec();
        let selected = content.selected.clone().unwrap_or_default();
        let highlighted = content.highlighted.clone().unwrap_or_default();
        let more = content.more == Some(true);
        let menus = Rc::new(content.menus.clone().unwrap_or_default());
        let focused = self.focus_handle.contains_focused(window, cx);
        let mut elements = Vec::new();
        for ix in range.clone() {
            let Some(row) = rows.get(ix - range.start) else {
                if more && ix == total {
                    elements.push(
                        div()
                            .id("load-more")
                            .h(ROW_HEIGHT)
                            .px_3()
                            .flex()
                            .items_center()
                            .text_sm()
                            .italic()
                            .text_color(theme.muted_foreground)
                            .hover(|this| this.bg(theme.list_hover))
                            .child("Load more…")
                            .on_click(cx.listener(|this, _, _, cx| this.send(actions::MORE, None, None, None, cx)))
                            .into_any_element(),
                    );
                }
                continue;
            };
            let is_selected = !selected.is_empty() && row.id == selected;
            let found = highlighted.contains(&row.id);
            let mut line = h_flex()
                .id(ix)
                .h(ROW_HEIGHT)
                .w_full()
                .relative()
                .text_sm()
                .when(row.bold, |this| this.font_weight(FontWeight::SEMIBOLD))
                .when(found && !is_selected, |this| this.bg(theme.yellow.opacity(0.14)))
                .when(!is_selected, |this| this.hover(|this| this.bg(theme.list_hover)))
                // The focus outline lies over the row: a border would shorten the
                // graph's lines and leave gaps between rows.
                .when(is_selected, |this| {
                    this.bg(theme.list_active)
                        .when(focused, |this| this.child(div().absolute().inset_0().border_1().border_color(theme.list_active_border)))
                });
            for (cell_ix, cell) in row.cells.iter().enumerate() {
                let width = widths.get(cell_ix).copied().flatten();
                let slot = div().h_full().flex().items_center().overflow_hidden().when(row.dim, |this| this.opacity(0.55));
                let slot = match width {
                    Some(width) => slot.flex_none().w(width),
                    None => slot.flex_1().min_w_0(),
                };
                let slot = match &cell.graph {
                    Some(graph) => slot.child(graph_canvas(graph.clone(), theme.background)),
                    None => slot.px_2().gap_1().children(cell.spans.iter().enumerate().map(|(span_ix, span)| self.render_span(span, &row.id, (ix, cell_ix, span_ix), cx))),
                };
                line = line.child(slot);
            }
            let row_id = row.id.clone();
            let open_id = row.id.clone();
            let menu = (!row.menu.is_empty()).then(|| row.menu.clone());
            line = line.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.focus_handle.focus(window, cx);
                if event.click_count() >= 2 {
                    this.send(actions::OPEN, Some(&open_id), None, None, cx);
                } else {
                    this.select(Some(row_id.clone()), cx);
                }
            }));
            let element = self.with_menu(line, menu, row.id.clone(), menus.clone(), cx);
            elements.push(element);
        }
        elements
    }

    /// `element` with a right-click menu: the one a span or line under the
    /// mouse asked for, else `menu`, its picks sent with `row`.
    fn with_menu(&self, element: Stateful<Div>, menu: Option<String>, row: String, menus: Rc<Menus>, cx: &mut Context<Self>) -> AnyElement {
        let target = self.menu_target.clone();
        let this = cx.weak_entity();
        element
            .context_menu(move |popup, window, cx| {
                let (name, data) = match target.borrow_mut().take() {
                    Some((name, data)) => (Some(name), Some(data)),
                    None => (menu.clone(), None),
                };
                let Some(items) = name.and_then(|name| menus.get(&name)) else { return popup };
                build_menu(popup, items, this.clone(), row.clone(), data, window, cx)
            })
            .into_any_element()
    }

    fn render_span(&self, span: &Span, row: &str, key: (usize, usize, usize), cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let color = parse_color(&span.color).unwrap_or(if span.dim { theme.muted_foreground } else { theme.foreground });
        let background = parse_color(&span.background);
        let mut element = h_flex()
            .id(SharedString::from(format!("span-{}-{}-{}", key.0, key.1, key.2)))
            .flex_none()
            .gap_1()
            .items_center()
            .text_color(color)
            .when(span.bold, |this| this.font_weight(FontWeight::SEMIBOLD))
            .when(span.italic, |this| this.italic())
            .when(span.mono, |this| this.font_family(crate::settings::mono_font(cx)))
            .when_some(background, |this, bg| this.px_1p5().h(px(18.)).rounded(px(4.)).text_xs().bg(bg).text_color(parse_color(&span.color).unwrap_or(gpui_kit::white())))
            .when_some(crate::ui::lucide_icon(&span.icon, cx), |this, icon| this.child(icon.xsmall()))
            .when(background.is_none() && !span.text.is_empty(), |this| this.flex_shrink(1.).min_w_0().truncate())
            .child(span.text.clone());
        if !span.tooltip.is_empty() {
            let tip = span.tooltip.clone();
            element = element.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx));
        }
        if !span.action.is_empty() {
            let (action, data, row) = (span.action.clone(), span.data.clone(), row.to_string());
            element = element.cursor_pointer().hover(|this| this.underline()).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.send(&action, Some(&row), Some(&data), None, cx);
            }));
        }
        if !span.menu.is_empty() {
            let target = self.menu_target.clone();
            let (menu, data) = (span.menu.clone(), span.data.clone());
            element = element.on_mouse_down(MouseButton::Right, move |_, _, _| *target.borrow_mut() = Some((menu.clone(), data.clone())));
        }
        element.into_any_element()
    }

    fn render_detail(&self, lines: &[Line], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let menus = Rc::new(self.content(cx).and_then(|c| c.menus.clone()).unwrap_or_default());
        let selected = self.content(cx).and_then(|c| c.selected.clone()).unwrap_or_default();
        v_flex()
            .size_full()
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex().flex_none().justify_end().px_1().pt_1().child(
                    Button::new("close-detail")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::X))
                        .tooltip("Close")
                        .on_click(cx.listener(|this, _, _, cx| this.select(None, cx))),
                ),
            )
            .child(
                div().id("detail").flex_1().min_h_0().overflow_y_scroll().px_4().pb_3().child(v_flex().children(lines.iter().enumerate().map(|(ix, line)| {
                    let mut element = h_flex()
                        .id(("detail-line", ix))
                        .flex_wrap()
                        .gap_x_1()
                        .min_h(px(22.))
                        .pl(px(16. * line.indent as f32))
                        .text_sm()
                        .when(line.heading, |this| this.mt_2().font_weight(FontWeight::SEMIBOLD))
                        .children(line.spans.iter().enumerate().map(|(span_ix, span)| self.render_detail_span(span, &selected, (ix, span_ix), cx)));
                    if !line.action.is_empty() {
                        let (action, data, row) = (line.action.clone(), line.data.clone(), selected.clone());
                        element = element
                            .cursor_pointer()
                            .rounded(px(3.))
                            .hover(|this| this.bg(theme.list_hover))
                            .on_click(cx.listener(move |this, _, _, cx| this.send(&action, Some(&row), Some(&data), None, cx)));
                    }
                    if line.menu.is_empty() {
                        return element.into_any_element();
                    }
                    let target = self.menu_target.clone();
                    let (menu, data) = (line.menu.clone(), line.data.clone());
                    let element = element.on_mouse_down(MouseButton::Right, move |_, _, _| *target.borrow_mut() = Some((menu.clone(), data.clone())));
                    self.with_menu(element, None, selected.clone(), menus.clone(), cx)
                }))),
            )
    }

    /// A span of the details: as in a row, but long text wraps.
    fn render_detail_span(&self, span: &Span, row: &str, key: (usize, usize), cx: &mut Context<Self>) -> AnyElement {
        if !span.background.is_empty() || !span.icon.is_empty() || !span.action.is_empty() || !span.menu.is_empty() {
            return self.render_span(span, row, (usize::MAX, key.0, key.1), cx);
        }
        let theme = cx.theme();
        let color = parse_color(&span.color).unwrap_or(if span.dim { theme.muted_foreground } else { theme.foreground });
        div()
            .text_color(color)
            .when(span.bold, |this| this.font_weight(FontWeight::SEMIBOLD))
            .when(span.italic, |this| this.italic())
            .when(span.mono, |this| this.font_family(crate::settings::mono_font(cx)))
            .when(span.text.contains('\n'), |this| this.w_full().whitespace_normal())
            .child(span.text.clone())
            .into_any_element()
    }
}

/// `items` in `popup`; a pick sends the item's id with `row` and `data`.
fn build_menu(
    mut popup: PopupMenu,
    items: &[MenuItem],
    this: WeakEntity<ExtensionView>,
    row: String,
    data: Option<String>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for item in items {
        if item.separator {
            popup = popup.separator();
        } else if !item.submenu.is_empty() {
            let (sub, this, row, data) = (item.submenu.clone(), this.clone(), row.clone(), data.clone());
            popup = popup.submenu(item.label.clone(), window, cx, move |popup, window, cx| build_menu(popup, &sub, this.clone(), row.clone(), data.clone(), window, cx));
        } else {
            let (this, action, row, data) = (this.clone(), item.id.clone(), row.clone(), data.clone());
            let mut entry = PopupMenuItem::new(item.label.clone()).disabled(item.disabled).on_click(move |_, _, cx| {
                _ = this.update(cx, |this, cx| this.send(&action, Some(&row), data.as_deref(), None, cx));
            });
            if let Some(icon) = crate::ui::lucide_icon(&item.icon, cx) {
                entry = entry.icon(icon);
            }
            popup = popup.item(entry);
        }
    }
    popup
}

impl EventEmitter<PaneEvent> for ExtensionView {}

impl Focusable for ExtensionView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Pane for ExtensionView {
    fn kind(&self) -> &'static str {
        EXTENSION_VIEW
    }

    fn icon(&self, _: &App) -> IconName {
        IconName::Blocks
    }

    fn icon_element(&self, cx: &App) -> Option<AnyElement> {
        let kind = Extensions::get(cx).view_kind(&self.id, &self.view)?;
        Some(crate::ui::lucide_icon(&kind.icon, cx)?.small().into_any_element())
    }

    fn label(&self, cx: &App) -> SharedString {
        let title = self.content(cx).and_then(|c| c.title.clone());
        let title = title.or_else(|| Extensions::get(cx).view_kind(&self.id, &self.view).map(|v| v.title.clone()));
        title.unwrap_or_else(|| self.view.clone()).into()
    }

    fn dump(&self, _: &App) -> Value {
        json!({ "id": self.id, "view": self.view, "root": self.root })
    }
}

impl Render for ExtensionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let base = v_flex().size_full().bg(theme.background).track_focus(&self.focus_handle).key_context("ExtensionView");
        if Extensions::get(cx).view_kind(&self.id, &self.view).is_none() {
            return base
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!("The extension \"{}\" isn't running, or has no view \"{}\".", self.id, self.view));
        }
        let (toolbar, columns, detail, message, count, selected) = match self.content(cx) {
            Some(c) => (
                c.toolbar.clone().unwrap_or_default(),
                c.columns.clone().unwrap_or_default(),
                c.detail.clone().unwrap_or_default(),
                c.message.clone().unwrap_or_default(),
                c.rows.as_ref().map_or(0, Vec::len) + usize::from(c.more == Some(true)),
                c.selected.clone().filter(|s| !s.is_empty()),
            ),
            None => Default::default(),
        };

        // A selection made by the extension (a search's next match) is scrolled to.
        if selected != self.scrolled_to {
            if let Some(ix) = selected.as_ref().and_then(|s| self.rows(cx).iter().position(|r| &r.id == s)) {
                self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
            }
            self.scrolled_to = selected;
        }

        let widths = self.column_widths(cx);
        let header = columns.iter().any(|c| !c.title.is_empty()).then(|| {
            h_flex()
                .flex_none()
                .h(px(26.))
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .border_b_1()
                .border_color(theme.border)
                .children(columns.iter().enumerate().map(|(ix, column)| {
                    let slot = div().px_2().truncate().child(column.title.clone());
                    match widths.get(ix).copied().flatten() {
                        Some(width) => slot.flex_none().w(width),
                        None => slot.flex_1().min_w_0(),
                    }
                }))
        });
        let list = if count == 0 {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(message)
                .into_any_element()
        } else {
            let widths = Rc::new(widths);
            uniform_list(
                "extension-view-rows",
                count,
                cx.processor(move |this, range, window, cx| this.render_rows(range, &widths, window, cx)),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element()
        };
        let list = v_flex()
            .id("extension-view-list")
            .size_full()
            .on_key_down(cx.listener(Self::on_key))
            .children(header)
            .child(div().flex_1().min_h_0().child(list));

        let body = if detail.is_empty() {
            list.into_any_element()
        } else {
            v_resizable(SharedString::from(format!("extension-view-{}-{}", self.id, self.view)))
                .child(resizable_panel().child(list))
                .child(resizable_panel().size(px(260.)).size_range(px(80.)..px(2000.)).child(self.render_detail(&detail, cx)))
                .into_any_element()
        };
        base.when(!toolbar.is_empty(), |this| this.child(self.render_toolbar(&toolbar, window, cx))).child(div().flex_1().min_h_0().child(body))
    }
}

/// Ask what `prompt` asks in a dialog over `window`; the answer goes to
/// extension `id` as `prompt_answered`, with null for a cancel.
pub fn prompt(id: String, root: PathBuf, prompt: Prompt, window: &mut Window, cx: &mut App) {
    let inputs: Vec<(String, Entity<InputState>)> = prompt
        .fields
        .iter()
        .filter(|f| f.kind == FieldKind::Text)
        .map(|f| (f.id.clone(), cx.new(|cx| InputState::new(window, cx).placeholder(f.placeholder.clone()).default_value(f.value.clone()))))
        .collect();
    // The checkboxes' and choices' values, as they are set.
    let picks: Rc<RefCell<Map<String, Value>>> = Rc::new(RefCell::new(
        prompt
            .fields
            .iter()
            .filter_map(|f| match f.kind {
                FieldKind::Checkbox => Some((f.id.clone(), Value::Bool(f.value == "true"))),
                FieldKind::Choice => Some((f.id.clone(), Value::String(f.value.clone()))),
                _ => None,
            })
            .collect(),
    ));
    let answered = Rc::new(std::cell::Cell::new(false));
    let answer = {
        let (id, root, prompt_id, answered) = (id.clone(), root.clone(), prompt.id.clone(), answered.clone());
        Rc::new(move |values: Value, cx: &App| {
            if !answered.replace(true) {
                extensions::view_event(&id, events::PROMPT_ANSWERED, json!({ "root": root, "id": prompt_id, "values": values }), cx);
            }
        })
    };
    let first = inputs.first().map(|(_, input)| input.clone());
    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let fields = v_flex().gap_3().w_full().children(prompt.fields.iter().filter_map(|field| {
            let label = (!field.label.is_empty()).then(|| div().text_sm().child(field.label.clone()));
            Some(match field.kind {
                FieldKind::Text => {
                    let input = inputs.iter().find(|(id, _)| *id == field.id)?.1.clone();
                    v_flex().gap_1().children(label).child(Input::new(&input)).into_any_element()
                }
                FieldKind::Checkbox => {
                    let checked = picks.borrow().get(&field.id).and_then(Value::as_bool).unwrap_or(false);
                    let (picks, key) = (picks.clone(), field.id.clone());
                    Checkbox::new(SharedString::from(format!("prompt-{}", field.id)))
                        .label(field.label.clone())
                        .checked(checked)
                        .on_click(move |checked, window, _| {
                            picks.borrow_mut().insert(key.clone(), Value::Bool(*checked));
                            window.refresh();
                        })
                        .into_any_element()
                }
                FieldKind::Choice => {
                    let current = picks.borrow().get(&field.id).and_then(Value::as_str).unwrap_or_default().to_string();
                    v_flex()
                        .gap_1()
                        .children(label)
                        .children(field.options.iter().map(|option| {
                            let (picks, key, value) = (picks.clone(), field.id.clone(), option.value.clone());
                            Radio::new(SharedString::from(format!("prompt-{}-{}", field.id, option.value)))
                                .label(option.label.clone())
                                .checked(option.value == current)
                                .on_click(move |_, window, _| {
                                    picks.borrow_mut().insert(key.clone(), Value::String(value.clone()));
                                    window.refresh();
                                })
                        }))
                        .into_any_element()
                }
                FieldKind::Unknown => return None,
            })
        }));
        let (ok_inputs, ok_picks, ok_fields, ok_answer) = (inputs.clone(), picks.clone(), prompt.fields.clone(), answer.clone());
        let (cancel_answer, close_answer) = (answer.clone(), answer.clone());
        let mut dialog = dialog
            .title(prompt.title.clone())
            .show_cancel(true)
            .ok_text(if prompt.ok.is_empty() { "OK".to_string() } else { prompt.ok.clone() })
            .when(!prompt.message.is_empty(), |this| this.description(prompt.message.clone()))
            .child(fields)
            .on_ok(move |_, _, cx| {
                let mut values = ok_picks.borrow().clone();
                for (key, input) in &ok_inputs {
                    let text = input.read(cx).value().trim().to_string();
                    if text.is_empty() && ok_fields.iter().any(|f| f.id == *key && f.required) {
                        return false;
                    }
                    values.insert(key.clone(), Value::String(text));
                }
                ok_answer(Value::Object(values), cx);
                true
            })
            .on_cancel(move |_, _, cx| {
                cancel_answer(Value::Null, cx);
                true
            })
            .on_close(move |_, _, cx| close_answer(Value::Null, cx));
        if prompt.danger {
            dialog = dialog.ok_variant(gpui_kit::component::button::ButtonVariant::Danger);
        }
        let _ = cx;
        dialog
    });
    if let Some(input) = first {
        window.defer(cx, move |window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    }
}
