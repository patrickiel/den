//! An extension's page, as VS Code shows one: what it is and how it runs, with
//! its update, uninstall and on/off switch, then its README.md (Details) and
//! the settings its manifest declares (Settings). A setting changed here is
//! saved in den's settings and sent to the extension at once.
//!
//! An extension the index offers but that isn't installed gets the same page
//! as a preview: its README fetched from its repository, and Install.

use std::path::PathBuf;

use den_extension::{Setting, SettingKind};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde_json::{Value, json};

use crate::backend::extensions::Listing;
use crate::extensions::{self, Entry, Extensions};
use crate::extensions_view::{avatar, listing_avatar, status};
use crate::pane::{Pane, PaneEvent};

pub const EXTENSION: &str = "Extension";

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Details,
    Settings,
}

/// The page's README.md.
enum Readme {
    /// Being fetched from the extension's repository.
    Loading,
    /// The text, and the folder its images are relative to when it is local.
    Text(SharedString, Option<PathBuf>),
    /// Why there is none.
    Missing(String),
}

pub struct ExtensionPanel {
    id: String,
    /// What the index says of it, for the page of one not installed.
    listing: Option<Listing>,
    focus_handle: FocusHandle,
    tab: Tab,
    readme: Readme,
    /// A text box per `number` and `string` setting, by key.
    inputs: Vec<(String, Entity<InputState>)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PaneEvent> for ExtensionPanel {}

impl Focusable for ExtensionPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ExtensionPanel {
    pub fn new(id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let entry = entry(&id, cx).cloned();
        let readme = entry
            .as_ref()
            .and_then(|e| Some(Readme::Text(std::fs::read_to_string(e.dir.join("README.md")).ok()?.into(), Some(e.dir.clone()))))
            .unwrap_or_else(|| Readme::Missing(format!("{} has no README.md.", entry.as_ref().map_or(id.as_str(), |e| e.name()))));
        let settings = entry.as_ref().and_then(|e| e.manifest.as_ref()).map(|m| m.settings.clone()).unwrap_or_default();
        let values = extensions::settings_values(&id, cx);
        let mut inputs = Vec::new();
        let mut _subscriptions = Vec::new();
        for setting in settings.iter().filter(|s| matches!(s.kind, SettingKind::Number | SettingKind::String)) {
            let text = values.get(&setting.key).map(text_of).unwrap_or_default();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(text_of(&setting.default)).default_value(text));
            inputs.push((setting.key.clone(), input.clone()));
            let (id, setting) = (id.clone(), setting.clone());
            _subscriptions.push(cx.subscribe(&input, move |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    // A number box that doesn't hold a number keeps the last good value.
                    if let Some(value) = parse(&setting, &text) {
                        extensions::set_setting(&id, &setting.key, value, cx);
                    }
                }
            }));
        }
        let tab = if matches!(readme, Readme::Missing(_)) && !settings.is_empty() { Tab::Settings } else { Tab::Details };
        Self { id, listing: None, focus_handle: cx.focus_handle(), tab, readme, inputs, _subscriptions }
    }

    /// The page of `listing`, not installed: its README comes from its repository.
    pub fn preview(listing: Listing, cx: &mut Context<Self>) -> Self {
        let readme = if listing.readme_url.is_empty() {
            Readme::Missing(format!("{} has no README.md.", listing.name))
        } else {
            let url = listing.readme_url.clone();
            cx.spawn(async move |this, cx| {
                let result = cx.background_spawn(async move { crate::backend::extensions::fetch_readme(&url) }).await;
                _ = this.update(cx, |this, cx| {
                    this.readme = match result {
                        Ok(text) => Readme::Text(text.into(), None),
                        Err(err) => Readme::Missing(err),
                    };
                    cx.notify();
                });
            })
            .detach();
            Readme::Loading
        };
        Self {
            id: listing.id.clone(),
            listing: Some(listing),
            focus_handle: cx.focus_handle(),
            tab: Tab::Details,
            readme,
            inputs: Vec::new(),
            _subscriptions: Vec::new(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Put `key` back to its default, and its box with it (which sets the
    /// default again as it changes, a value equal to the default).
    fn reset(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let default = entry(&self.id, cx)
            .and_then(|e| e.manifest.as_ref())
            .and_then(|m| m.settings.iter().find(|s| s.key == key))
            .map(|s| text_of(&s.default))
            .unwrap_or_default();
        if let Some((_, input)) = self.inputs.iter().find(|(k, _)| k == key) {
            input.update(cx, |input, cx| input.set_value(default, window, cx));
        }
        extensions::set_setting(&self.id, key, None, cx);
        cx.notify();
    }

    /// The header of a page for an extension not installed.
    fn render_preview_header(&self, listing: &Listing, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let wanted = vec![listing.clone()];
        cx.defer(move |cx| extensions::want_icons(wanted, cx));
        let theme = cx.theme().clone();
        let progress = crate::panels::download_progress(&format!("ext-install-{}", listing.id));
        let url = format!("https://github.com/{}", listing.repo);
        let action: AnyElement = if let Some(progress) = progress {
            div().text_sm().text_color(theme.muted_foreground).child(progress).into_any_element()
        } else if !listing.loadable() {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!("Built for extension API {}; this den has {}. Update den to install it.", listing.api, den_extension::API_VERSION))
                .into_any_element()
        } else {
            let repo = listing.repo.clone();
            Button::new("ext-page-install")
                .small()
                .primary()
                .icon(Icon::new(IconName::Download))
                .label("Install")
                .on_click(move |_, window, cx| crate::panels::get_extension(repo.clone(), None, window, cx))
                .into_any_element()
        };
        h_flex()
            .px_8()
            .pt_6()
            .pb_4()
            .gap_5()
            .items_start()
            .child(listing_avatar(listing, 88.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_baseline()
                            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child(listing.name.clone()))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(format!("v{}", listing.version))),
                    )
                    .child(
                        h_flex().gap_3().text_sm().text_color(theme.muted_foreground).child(listing.id.clone()).child(div().child("·")).child(
                            div()
                                .id("ext-page-repo")
                                .text_color(theme.link)
                                .cursor_pointer()
                                .hover(|this| this.underline())
                                .child(format!("github.com/{}", listing.repo))
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        ),
                    )
                    .when(!listing.description.is_empty(), |this| this.child(div().text_base().child(listing.description.clone())))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("Not installed. Extensions are native code and run with your permissions."))
                    .child(h_flex().pt_1().child(action)),
            )
    }

    fn render_header(&self, entry: &Entry, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let manifest = entry.manifest.as_ref();
        let (dot, status) = status(entry, cx);
        let id = entry.id.clone();
        let enabled = extensions::is_enabled(&id, cx);
        let removed = entry.change.as_deref() == Some(extensions::REMOVED);
        let progress = crate::panels::download_progress(&format!("ext-install-{id}"));
        let repository = manifest.map(|m| m.repository.clone()).filter(|r| !r.is_empty());
        let update = repository.clone().zip(manifest.map(|m| m.version.clone()));
        let (remove_id, switch_id) = (id.clone(), id.clone());

        let actions = h_flex().gap_2().items_center().pt_1();
        let actions = if removed {
            actions
        } else if let Some(progress) = progress {
            actions.child(div().text_sm().text_color(theme.muted_foreground).child(progress))
        } else {
            actions
                .when_some(update, |this, (repo, version)| {
                    this.child(
                        Button::new("ext-page-update")
                            .small()
                            .primary()
                            .icon(Icon::new(IconName::RefreshCw))
                            .label("Check for Update")
                            .on_click(move |_, window, cx| crate::panels::get_extension(repo.clone(), Some(version.clone()), window, cx)),
                    )
                })
                .child(
                    Button::new("ext-page-uninstall")
                        .small()
                        .outline()
                        .icon(Icon::new(IconName::Trash))
                        .label("Uninstall")
                        .on_click(move |_, window, cx| {
                            if let Err(err) = extensions::uninstall(&remove_id, cx) {
                                crate::toast::push(window, format!("Could not uninstall: {err}"), cx);
                            }
                            window.refresh();
                        }),
                )
                .child(
                    h_flex().gap_2().items_center().pl_2().child(
                        Switch::new("ext-page-on")
                            .checked(enabled)
                            .label(if enabled { "On" } else { "Off" })
                            .on_click(move |checked, window, cx| {
                                extensions::set_enabled(&switch_id, *checked, cx);
                                window.refresh();
                            }),
                    ),
                )
        };

        h_flex()
            .px_8()
            .pt_6()
            .pb_4()
            .gap_5()
            .items_start()
            .child(avatar(entry, 88.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_baseline()
                            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child(entry.name().to_string()))
                            .when_some(manifest, |this, m| this.child(div().text_sm().text_color(theme.muted_foreground).child(format!("v{}", m.version)))),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(entry.id.clone())
                            .when_some(repository, |this, repo| {
                                let url = format!("https://github.com/{repo}");
                                this.child(div().child("·")).child(
                                    div()
                                        .id("ext-page-repo")
                                        .text_color(theme.link)
                                        .cursor_pointer()
                                        .hover(|this| this.underline())
                                        .child(format!("github.com/{repo}"))
                                        .on_click(move |_, _, cx| cx.open_url(&url)),
                                )
                            }),
                    )
                    .when_some(manifest.filter(|m| !m.description.is_empty()), |this, m| this.child(div().text_base().child(m.description.clone())))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(div().size(px(8.)).rounded_full().flex_none().bg(dot))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(status)),
                    )
                    .child(actions),
            )
    }

    fn render_tabs(&self, settings: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let tab = |id: &'static str, label: String, which: Tab, cx: &mut Context<Self>| {
            let active = self.tab == which;
            div()
                .id(id)
                .px_1()
                .py_2()
                .text_sm()
                .cursor_pointer()
                .border_b_2()
                .border_color(if active { theme.primary } else { transparent_black() })
                .text_color(if active { theme.foreground } else { theme.muted_foreground })
                .hover(|this| this.text_color(theme.foreground))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = which;
                    cx.notify();
                }))
        };
        h_flex()
            .px_8()
            .gap_6()
            .border_b_1()
            .border_color(theme.border)
            .child(tab("ext-tab-details", "Details".into(), Tab::Details, cx))
            .when(settings > 0, |this| this.child(tab("ext-tab-settings", format!("Settings ({settings})"), Tab::Settings, cx)))
    }

    fn render_details(&self, cx: &mut Context<Self>) -> AnyElement {
        let (text, dir) = match &self.readme {
            Readme::Text(text, dir) => (text.clone(), dir.clone()),
            Readme::Loading => return self.note("Loading the README…", cx),
            Readme::Missing(why) => return self.note(why, cx),
        };
        div()
            .size_full()
            .px_8()
            .py_4()
            .child(
                gpui_kit::base::TextView::markdown(("ext-readme", cx.entity_id()), text)
                    .scrollable(true)
                    .selectable(true)
                    .size_full()
                    .image_source(move |url| {
                        let url = url.to_string();
                        match &dir {
                            Some(dir) if !url.contains("://") && !url.starts_with("data:") => ImageSource::from(dir.join(url.trim_start_matches("./"))),
                            _ => ImageSource::from(url),
                        }
                    }),
            )
            .into_any_element()
    }

    fn render_settings(&self, settings: &[Setting], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let values = extensions::settings_values(&self.id, cx);
        let user = crate::settings::Settings::get(cx).extension_settings.get(&self.id).cloned().unwrap_or_default();
        let rows = settings.iter().filter(|s| s.kind != SettingKind::Unknown).map(|setting| {
            let value = values.get(&setting.key).cloned().unwrap_or(Value::Null);
            let changed = user.get(&setting.key).is_some_and(|v| setting.accepts(v) && *v != setting.default);
            let (id, key) = (self.id.clone(), setting.key.clone());
            let control: AnyElement = match setting.kind {
                SettingKind::Boolean => Switch::new(SharedString::from(format!("ext-set-{key}")))
                    .checked(value.as_bool().unwrap_or(false))
                    .on_click(move |checked, _, cx| extensions::set_setting(&id, &key, Some(json!(*checked)), cx))
                    .into_any_element(),
                SettingKind::Choice => {
                    let current = value.as_str().unwrap_or_default().to_string();
                    let options = setting.options.clone();
                    Button::new(SharedString::from(format!("ext-set-{key}")))
                        .small()
                        .outline()
                        .label(current.clone())
                        .icon(Icon::new(IconName::ChevronDown))
                        .dropdown_menu(move |mut menu, _, _| {
                            for option in &options {
                                let (id, key, option) = (id.clone(), key.clone(), option.clone());
                                menu = menu.item(
                                    PopupMenuItem::new(option.clone())
                                        .checked(option == current)
                                        .on_click(move |_, _, cx| extensions::set_setting(&id, &key, Some(json!(option)), cx)),
                                );
                            }
                            menu
                        })
                        .into_any_element()
                }
                _ => {
                    let input = self.inputs.iter().find(|(k, _)| *k == setting.key).map(|(_, input)| input.clone());
                    let bad = input.as_ref().is_some_and(|input| {
                        let text = input.read(cx).value();
                        setting.kind == SettingKind::Number && !text.trim().is_empty() && parse(setting, &text).is_none()
                    });
                    v_flex()
                        .w(px(260.))
                        .gap_1()
                        .when_some(input, |this, input| this.child(Input::new(&input).small()))
                        .when(bad, |this| this.child(div().text_xs().text_color(theme.danger).child("Not a number; the last good value stays.")))
                        .into_any_element()
                }
            };
            let reset_key = setting.key.clone();
            h_flex()
                .w_full()
                .py_3()
                .gap_6()
                .justify_between()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(200.))
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(setting.title.clone()))
                                .when(changed, |this| {
                                    this.child(
                                        Button::new(SharedString::from(format!("ext-reset-{}", setting.key)))
                                            .xsmall()
                                            .ghost()
                                            .icon(Icon::new(IconName::RotateCcw))
                                            .tooltip(format!("Reset to {}", text_of(&setting.default)))
                                            .on_click(cx.listener(move |this, _, window, cx| this.reset(&reset_key, window, cx))),
                                    )
                                }),
                        )
                        .when(!setting.description.is_empty(), |this| {
                            this.child(div().text_xs().text_color(theme.muted_foreground).child(setting.description.clone()))
                        }),
                )
                .child(control)
        });
        v_flex()
            .id("ext-settings")
            .size_full()
            .overflow_y_scroll()
            .px_8()
            .py_2()
            .children(rows)
            .child(div().pt_3().text_xs().text_color(theme.muted_foreground).child("Changes are saved at once and sent to the extension while it runs."))
            .into_any_element()
    }
}

impl ExtensionPanel {
    fn note(&self, text: &str, cx: &App) -> AnyElement {
        div().px_8().py_6().text_sm().text_color(cx.theme().muted_foreground).child(text.to_string()).into_any_element()
    }
}

fn entry<'a>(id: &str, cx: &'a App) -> Option<&'a Entry> {
    Extensions::get(cx).entries.iter().find(|e| e.id == id)
}

/// A value as its text box shows it.
fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// What a text box's `text` sets: `Some(None)` for the default (an empty
/// number box), `None` when it isn't a value of the setting's type.
fn parse(setting: &Setting, text: &str) -> Option<Option<Value>> {
    match setting.kind {
        SettingKind::Number if text.trim().is_empty() => Some(None),
        SettingKind::Number => {
            let text = text.trim();
            let value = text.parse::<i64>().map(Value::from).ok().or_else(|| text.parse::<f64>().ok().and_then(|n| serde_json::Number::from_f64(n).map(Value::Number)))?;
            Some(Some(value))
        }
        _ => Some(Some(Value::String(text.to_string()))),
    }
}

impl Pane for ExtensionPanel {
    fn kind(&self) -> &'static str {
        EXTENSION
    }

    fn icon(&self, _: &App) -> IconName {
        IconName::Blocks
    }

    fn label(&self, cx: &App) -> SharedString {
        let name = entry(&self.id, cx).map(|e| e.name().to_string()).or_else(|| self.listing.as_ref().map(|l| l.name.clone())).unwrap_or_else(|| self.id.clone());
        format!("Extension: {name}").into()
    }

    fn dump(&self, _: &App) -> Value {
        match &self.listing {
            Some(listing) => json!({ "id": self.id, "listing": listing }),
            None => json!({ "id": self.id }),
        }
    }
}

impl Render for ExtensionPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(entry) = entry(&self.id, cx).cloned() else {
            let Some(listing) = self.listing.clone() else {
                return v_flex()
                    .size_full()
                    .track_focus(&self.focus_handle)
                    .items_center()
                    .justify_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("The extension \"{}\" is not installed.", self.id));
            };
            return v_flex()
                .size_full()
                .track_focus(&self.focus_handle)
                .bg(cx.theme().background)
                .child(self.render_preview_header(&listing, cx))
                .child(self.render_tabs(0, cx))
                .child(div().flex_1().min_h_0().child(self.render_details(cx)));
        };
        let settings = entry.manifest.as_ref().map(|m| m.settings.clone()).unwrap_or_default();
        let shown = settings.iter().filter(|s| s.kind != SettingKind::Unknown).count();
        let tab = if shown == 0 { Tab::Details } else { self.tab };
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(cx.theme().background)
            .child(self.render_header(&entry, cx))
            .child(self.render_tabs(shown, cx))
            .child(div().flex_1().min_h_0().child(match tab {
                Tab::Details => self.render_details(cx),
                Tab::Settings => self.render_settings(&settings, cx),
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Setting, SettingKind, parse, text_of};
    use serde_json::json;

    fn setting(kind: SettingKind) -> Setting {
        Setting { key: "k".into(), title: "K".into(), description: String::new(), kind, default: json!(1), options: Vec::new() }
    }

    #[test]
    fn text_boxes_parse_to_values_of_the_settings_type() {
        let number = setting(SettingKind::Number);
        assert_eq!(parse(&number, " 42 "), Some(Some(json!(42))));
        assert_eq!(parse(&number, "1.5"), Some(Some(json!(1.5))));
        assert_eq!(parse(&number, ""), Some(None));
        assert_eq!(parse(&number, "abc"), None);
        assert_eq!(parse(&setting(SettingKind::String), ""), Some(Some(json!(""))));
        assert_eq!(text_of(&json!("a")), "a");
        assert_eq!(text_of(&json!(90)), "90");
    }
}
