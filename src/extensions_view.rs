//! The Extensions view, as in VS Code: one box that filters the extensions or,
//! given `owner/repo`, installs one from GitHub; the installed ones, each with
//! how it runs and its update, uninstall and on/off switch; then the ones the
//! curated index offers (Available), each with Install. A card opens the
//! extension's page (`extension_panel.rs`), with its README and settings.
//! Everything it shows is the `Extensions` global; installing and the rest go
//! through `extensions.rs` and the download helpers in `panels.rs`.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    switch::Switch,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::backend::extensions::Listing;
use crate::extensions::{self, Entry, Extensions, IndexState, Status};

pub enum ExtensionsEvent {
    /// Open a file in an editor tab (`extensions.log`).
    Open(PathBuf),
    /// Open the page of the installed extension with this id.
    Show(String),
    /// Open the page of an extension the index offers.
    Preview(Listing),
}

pub struct ExtensionsView {
    query: Entity<InputState>,
    /// The extension whose page was opened last, by id.
    selected: Option<String>,
    /// The index was asked for, on the first draw.
    index_requested: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ExtensionsEvent> for ExtensionsView {}

impl ExtensionsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Filter, or owner/repo to install"));
        let _subscriptions = vec![cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.install(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })];
        Self { query, selected: None, index_requested: false, _subscriptions }
    }

    pub fn focus(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let query = this.read(cx).query.clone();
        query.update(cx, |query, cx| query.focus(window, cx));
    }

    /// The box's text as a GitHub repository, when it reads as one.
    fn repository(&self, cx: &App) -> Option<String> {
        let text = self.query.read(cx).value();
        text.contains('/').then(|| crate::backend::extensions::parse_repository(&text).ok()).flatten()
    }

    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(repo) = self.repository(cx) {
            crate::panels::get_extension(repo, None, window, cx);
        }
    }
}

/// Whether any of `texts` holds the filter `query` (lowercase).
fn passes(texts: &[&str], query: &str) -> bool {
    query.is_empty() || texts.iter().any(|text| text.to_lowercase().contains(query))
}

/// A letter on a colour of the extension's own, in place of an icon.
fn letter(id: &str, name: &str, size: f32) -> AnyElement {
    let hue = id.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    let letter = name.chars().next().unwrap_or('?').to_uppercase().to_string();
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(size / 6.))
        .text_size(px(size * 0.45))
        .bg(hsla(hue as f32 / 360., 0.45, 0.42, 1.))
        .text_color(white())
        .font_weight(FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .child(letter)
        .into_any_element()
}

/// The image at `path`, else the letter.
fn icon_or_letter(path: Option<PathBuf>, id: &str, name: &str, size: f32) -> AnyElement {
    match path.filter(|path| path.is_file()) {
        Some(path) => {
            let (id, name) = (id.to_string(), name.to_string());
            img(path)
                .size(px(size))
                .flex_none()
                .rounded(px(size / 6.))
                .object_fit(ObjectFit::Contain)
                .with_fallback(move || letter(&id, &name, size))
                .into_any_element()
        }
        None => letter(id, name, size),
    }
}

/// An installed extension's icon, else its letter.
pub(crate) fn avatar(entry: &Entry, size: f32) -> AnyElement {
    let path = entry.manifest.as_ref().filter(|m| !m.icon.is_empty()).map(|m| entry.dir.join(&m.icon));
    icon_or_letter(path, &entry.id, entry.name(), size)
}

/// A listed extension's icon, as fetched with the index, else its letter.
pub(crate) fn listing_avatar(listing: &Listing, size: f32) -> AnyElement {
    icon_or_letter(crate::backend::extensions::icon_path(listing), &listing.id, &listing.name, size)
}

/// The dot and words for how `entry` runs, then what the next start changes.
pub(crate) fn status(entry: &Entry, cx: &App) -> (Hsla, String) {
    let theme = cx.theme();
    let (color, text) = match &entry.status {
        Status::Loading => (theme.warning, "Starting…".to_string()),
        Status::Loaded => (theme.success, "Running".to_string()),
        Status::Disabled => (theme.muted_foreground, "Off".to_string()),
        Status::Failed(err) => (theme.danger, format!("Failed: {err}")),
        Status::Installed => (theme.muted_foreground, String::new()),
    };
    let text = [text, entry.change.clone().unwrap_or_default()].join(" · ");
    (color, text.trim_matches(|c| c == ' ' || c == '·').to_string())
}

/// What both kinds of card show above their bottom row.
struct Face {
    id: String,
    icon: AnyElement,
    name: String,
    version: String,
    description: String,
}

/// The frame both kinds of card share: icon, name and version, description,
/// then a bottom row.
fn card_frame(face: Face, bottom: impl IntoElement, selected: bool, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let Face { id, icon, name, version, description } = face;
    h_flex()
        .id(SharedString::from(format!("ext-card-{id}")))
        .px_3()
        .py_2()
        .gap_3()
        .items_start()
        .cursor_pointer()
        .when(selected, |this| this.bg(theme.list_active))
        .hover(|this| this.bg(theme.list_hover))
        .on_click(on_click)
        .child(icon)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .items_baseline()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                        .child(div().text_xs().text_color(theme.muted_foreground).flex_none().child(version)),
                )
                .when(!description.is_empty(), |this| this.child(div().text_xs().text_color(theme.muted_foreground).line_clamp(2).child(description)))
                .child(bottom),
        )
}

fn card(entry: &Entry, update: Option<Listing>, selected: bool, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let id = entry.id.clone();
    let manifest = entry.manifest.as_ref();
    let (dot, status) = status(entry, cx);
    let removed = entry.change.as_deref() == Some(extensions::REMOVED);
    let progress = crate::panels::download_progress(&format!("ext-install-{id}"));
    let check = manifest.filter(|m| !m.repository.is_empty()).map(|m| (m.repository.clone(), m.version.clone()));
    let update = update.zip(manifest).map(|(listing, m)| (listing.repo, m.version.clone(), listing.version));
    let (remove_id, switch_id) = (id.clone(), id.clone());

    let controls = h_flex().gap_0p5().items_center().flex_none();
    let controls = if removed {
        controls
    } else if let Some(progress) = progress {
        controls.child(div().text_xs().text_color(theme.muted_foreground).child(progress))
    } else {
        controls
            .map(|this| match (update, check) {
                // The index knows of a newer version: say so.
                (Some((repo, installed, newer)), _) => this.child(
                    Button::new(SharedString::from(format!("ext-view-update-{id}")))
                        .xsmall()
                        .primary()
                        .label("Update")
                        .tooltip(format!("Version {newer} is out"))
                        .on_click(move |_, window, cx| crate::panels::get_extension(repo.clone(), Some(installed.clone()), window, cx)),
                ),
                (None, Some((repo, version))) => this.child(
                    Button::new(SharedString::from(format!("ext-view-update-{id}")))
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(IconName::RefreshCw))
                        .tooltip("Check for an Update")
                        .on_click(move |_, window, cx| crate::panels::get_extension(repo.clone(), Some(version.clone()), window, cx)),
                ),
                (None, None) => this,
            })
            .child(
                Button::new(SharedString::from(format!("ext-view-remove-{id}")))
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(IconName::Trash))
                    .tooltip("Uninstall")
                    .on_click(move |_, window, cx| {
                        if let Err(err) = extensions::uninstall(&remove_id, cx) {
                            crate::toast::push(window, format!("Could not uninstall: {err}"), cx);
                        }
                        window.refresh();
                    }),
            )
            .child(
                // Not a click on the card as well.
                div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                    Switch::new(SharedString::from(format!("ext-view-on-{id}")))
                        .xsmall()
                        .checked(extensions::is_enabled(&id, cx))
                        .tooltip(if extensions::is_enabled(&id, cx) { "Turn Off" } else { "Turn On" })
                        .on_click(move |checked, window, cx| {
                            extensions::set_enabled(&switch_id, *checked, cx);
                            window.refresh();
                        }),
                ),
            )
    };
    let bottom = h_flex()
        .gap_2()
        .items_center()
        .justify_between()
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .min_w_0()
                .child(div().size(px(7.)).rounded_full().flex_none().bg(dot))
                .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(status)),
        )
        .child(controls);
    let face = Face {
        id: entry.id.clone(),
        icon: avatar(entry, 36.),
        name: entry.name().to_string(),
        version: manifest.map(|m| m.version.clone()).unwrap_or_default(),
        description: manifest.map(|m| m.description.clone()).unwrap_or_default(),
    };
    card_frame(face, bottom, selected, on_click, cx)
}

fn available_card(listing: &Listing, selected: bool, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let progress = crate::panels::download_progress(&format!("ext-install-{}", listing.id));
    let author = listing.repo.split('/').next().unwrap_or_default().to_string();
    let action: AnyElement = if let Some(progress) = progress {
        div().text_xs().text_color(theme.muted_foreground).child(progress).into_any_element()
    } else if !listing.loadable() {
        div().text_xs().text_color(theme.muted_foreground).child("Needs a newer den").into_any_element()
    } else {
        let repo = listing.repo.clone();
        Button::new(SharedString::from(format!("ext-view-install-{}", listing.id)))
            .xsmall()
            .primary()
            .label("Install")
            .on_click(move |_, window, cx| crate::panels::get_extension(repo.clone(), None, window, cx))
            .into_any_element()
    };
    let bottom = h_flex()
        .gap_2()
        .items_center()
        .justify_between()
        .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(author))
        .child(action);
    let face = Face {
        id: listing.id.clone(),
        icon: listing_avatar(listing, 36.),
        name: listing.name.clone(),
        version: listing.version.clone(),
        description: listing.description.clone(),
    };
    card_frame(face, bottom, selected, on_click, cx).when(!listing.loadable(), |this| this.opacity(0.6))
}

impl Render for ExtensionsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.index_requested {
            self.index_requested = true;
            cx.defer(|cx| extensions::refresh_index(false, cx));
        }
        let theme = cx.theme().clone();
        let query = self.query.read(cx).value().trim().to_lowercase();
        let repository = self.repository(cx);
        let state = Extensions::get(cx);
        let entries = state.entries.as_slice();
        let shown: Vec<&Entry> = entries
            .iter()
            .filter(|e| passes(&[&e.id, e.name(), e.manifest.as_ref().map_or("", |m| m.description.as_str())], &query))
            .collect();
        let uninstalled = extensions::uninstalled(&state.available, entries);
        let available: Vec<Listing> = uninstalled
            .iter()
            .filter(|l| passes(&[&l.id, &l.name, &l.description, &l.repo], &query))
            .map(|l| (*l).clone())
            .collect();
        let updates: Vec<Option<Listing>> = shown.iter().map(|e| extensions::update_available(e, &state.available).cloned()).collect();
        let index = state.index.clone();
        let total_available = uninstalled.len();
        let header_button = |id: &'static str, icon: IconName, tooltip: &'static str| Button::new(id).ghost().xsmall().icon(Icon::new(icon)).tooltip(tooltip);
        let section = |title: String| {
            div().px_3().pt_3().pb_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child(title)
        };
        let note = |text: String| div().px_3().py_2().text_xs().text_color(theme.muted_foreground).child(text);

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(32.))
                    .flex_none()
                    .px_3()
                    .justify_between()
                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("EXTENSIONS"))
                    .child(
                        h_flex()
                            .child(
                                header_button("ext-view-refresh", IconName::RefreshCw, "Refresh the Available List")
                                    .loading(index == IndexState::Loading)
                                    .on_click(|_, _, cx| extensions::refresh_index(true, cx)),
                            )
                            .child(header_button("ext-view-log", IconName::FileText, "Show extensions.log").on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ExtensionsEvent::Open(crate::backend::extensions::log_path()));
                            })))
                            .child(header_button("ext-view-folder", IconName::FolderOpen, "Open Extensions Folder").on_click(|_, _, _| crate::panels::open_extensions_folder())),
                    ),
            )
            .child(div().px_2().pb_1().flex_none().child(Input::new(&self.query).small().cleanable(true)))
            .child(
                v_flex()
                    .id("extensions-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when_some(repository, |this, repo| {
                        this.child(section("GITHUB".into())).child(
                            h_flex()
                                .px_3()
                                .py_1()
                                .gap_2()
                                .items_center()
                                .justify_between()
                                .child(div().text_sm().truncate().child(format!("github.com/{repo}")))
                                .child(Button::new("ext-view-install").xsmall().primary().label("Install").on_click(cx.listener(|this, _, window, cx| this.install(window, cx)))),
                        )
                    })
                    .child(section(format!("INSTALLED  {}", entries.len())))
                    .children(shown.iter().zip(updates).map(|(entry, update)| {
                        let id = entry.id.clone();
                        let selected = self.selected.as_ref() == Some(&entry.id);
                        let show = cx.listener(move |this, _, _, cx| {
                            this.selected = Some(id.clone());
                            cx.emit(ExtensionsEvent::Show(id.clone()));
                            cx.notify();
                        });
                        card(entry, update, selected, show, cx)
                    }))
                    .when(shown.is_empty(), |this| {
                        let text = if entries.is_empty() {
                            "No extensions yet. Install one from the list below, type owner/repo above for one that isn't listed, or copy an extension's folder into the extensions folder and restart den."
                        } else {
                            "No installed extension matches."
                        };
                        this.child(note(text.into()))
                    })
                    .child(section(format!("AVAILABLE  {total_available}")))
                    .children(available.iter().map(|listing| {
                        let selected = self.selected.as_ref() == Some(&listing.id);
                        let preview = listing.clone();
                        let show = cx.listener(move |this, _, _, cx| {
                            this.selected = Some(preview.id.clone());
                            cx.emit(ExtensionsEvent::Preview(preview.clone()));
                            cx.notify();
                        });
                        available_card(listing, selected, show, cx)
                    }))
                    .map(|this| match (&index, available.is_empty()) {
                        (IndexState::Failed(err), _) => this.child(note(err.clone())),
                        (IndexState::Loading, true) => this.child(note("Loading…".into())),
                        (_, true) if total_available > 0 => this.child(note("No available extension matches.".into())),
                        (_, true) => this.child(note("Nothing more to install for now.".into())),
                        _ => this,
                    }),
            )
    }
}
