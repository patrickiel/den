//! The file and Settings pane kinds.
//!
//! `init` registers the builders that make a saved pane again from what its
//! `dump` saved: the file's here, the others' with their kinds.

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState, Input, InputEvent, InputState, Position, TabSize},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    popover::Popover,
    switch::Switch,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde_json::{Value, json};

use crate::{
    SaveFile,
    defaults::Kind,
    pane::{self, Pane, PaneEvent},
    settings::{Preset, Settings, SidebarSide, ThemeChoice},
};

pub const FILE: &str = "File";
pub const SETTINGS: &str = "Settings";

pub fn init(cx: &mut App) {
    pane::register(cx, FILE, |data, window, cx| {
        let path = PathBuf::from(data["path"].as_str().unwrap_or_default());
        Rc::new(cx.new(|cx| FilePanel::new(path, window, cx)))
    });
    pane::register(cx, crate::browser::BROWSER, |data, window, cx| {
        let url = data["url"].as_str().map(str::to_string);
        Rc::new(cx.new(|cx| crate::browser::BrowserPanel::new(url, window, cx)))
    });
    pane::register(cx, SETTINGS, |_, window, cx| Rc::new(cx.new(|cx| SettingsPanel::new(window, cx))));
    pane::register(cx, crate::extension_panel::EXTENSION, |data, window, cx| {
        let id = data["id"].as_str().unwrap_or_default().to_string();
        let listing = serde_json::from_value::<crate::backend::extensions::Listing>(data["listing"].clone()).ok();
        // A preview whose extension got installed meanwhile opens as its page.
        let installed = crate::extensions::Extensions::get(cx).entry(&id).is_some();
        match listing.filter(|_| !installed) {
            Some(listing) => Rc::new(cx.new(|cx| crate::extension_panel::ExtensionPanel::preview(listing, cx))),
            None => Rc::new(cx.new(|cx| crate::extension_panel::ExtensionPanel::new(id, window, cx))),
        }
    });
    pane::register(cx, crate::extension_view::EXTENSION_VIEW, |data, _, cx| {
        let text = |key: &str| data[key].as_str().unwrap_or_default().to_string();
        let (id, view, root) = (text("id"), text("view"), PathBuf::from(text("root")));
        Rc::new(cx.new(|cx| crate::extension_view::ExtensionView::new(id, view, root, cx)))
    });
}

// ---------------------------------------------------------------------------
// File

/// What a file tab can show besides its text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Text,
    Markdown,
    Svg,
    /// A picture: shown as one, never as text.
    Image,
}

impl Look {
    fn of(path: &Path) -> Self {
        match path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_lowercase().as_str() {
            "md" | "markdown" => Look::Markdown,
            "svg" => Look::Svg,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => Look::Image,
            _ => Look::Text,
        }
    }
}

pub struct FilePanel {
    path: PathBuf,
    look: Look,
    /// Markdown and SVG: the preview shows instead of the source.
    preview: bool,
    /// The last SVG preview and the text it was made from.
    svg: Option<(SharedString, Arc<Image>)>,
    /// The encoding the file was read in and is saved in.
    encoding: String,
    /// Tabs or spaces, and the width: detected, or picked in the status bar.
    indent: (bool, usize),
    /// The highlighter's language.
    language: String,
    /// The file in the git index, for the gutter marks; `None` outside git.
    git_base: Option<String>,
    /// Lines that differ from `git_base`.
    marks: Vec<(usize, crate::dirty_diff::Mark)>,
    editor: Entity<EditorState>,
    /// The text as last read or saved; the tab is dirty while the buffer differs.
    saved: SharedString,
    dirty: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl FilePanel {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let look = Look::of(&path);
        let (text, encoding, error) = match crate::encoding::read(&path, None) {
            _ if look == Look::Image => (String::new(), "utf8".to_string(), (!path.is_file()).then(|| SharedString::from("The file is gone"))),
            Ok((text, encoding)) => (text, encoding, None),
            Err(err) => (String::new(), "utf8".to_string(), Some(SharedString::from(err))),
        };
        let indent = crate::encoding::detect_indent(&text).unwrap_or((false, 4));
        let state = crate::settings::AppState::get(cx);
        let preview = match look {
            Look::Markdown => state.preview_markdown,
            Look::Svg => state.preview_svg,
            _ => false,
        };
        let settings = Settings::get(cx).clone();
        let language = language_of(&path);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language.clone())
                .line_number(settings.line_numbers)
                .soft_wrap(settings.soft_wrap)
                .indent_guides(true)
                .context_menu(false)
                .tab_size(TabSize {
                    tab_size: indent.1,
                    hard_tabs: indent.0,
                })
                .default_value(text.clone())
        });

        let _subscriptions = vec![
            cx.subscribe(&editor, |this, editor, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.update_marks(cx);
                    let dirty = editor.read(cx).value() != this.saved;
                    if dirty != this.dirty {
                        this.dirty = dirty;
                        cx.emit(PaneEvent::Changed);
                        cx.notify();
                    }
                }
            }),
            cx.observe(&editor, |_, _, cx| cx.notify()),
            cx.observe_global_in::<Settings>(window, |this, window, cx| {
                let settings = Settings::get(cx).clone();
                this.editor.update(cx, |editor, cx| {
                    editor.set_line_number(settings.line_numbers, window, cx);
                    editor.set_soft_wrap(settings.soft_wrap, window, cx);
                });
                cx.notify();
            }),
        ];

        cx.defer_in(window, |this, _, cx| this.refresh_git_base(cx));
        Self {
            path,
            look,
            preview,
            svg: None,
            encoding,
            indent,
            language,
            git_base: None,
            marks: Vec::new(),
            editor,
            saved: text.into(),
            dirty: false,
            error,
            _subscriptions,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the file from the git index again (after a save, a stage, a
    /// checkout), in the background, and mark the lines anew.
    pub fn refresh_git_base(&mut self, cx: &mut Context<Self>) {
        if self.look == Look::Image {
            return;
        }
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let base = cx.background_spawn(async move { crate::dirty_diff::index_text(&path) }).await;
            _ = this.update(cx, |this, cx| {
                if this.git_base != base {
                    this.git_base = base;
                    this.update_marks(cx);
                }
            });
        })
        .detach();
    }

    fn update_marks(&mut self, cx: &mut Context<Self>) {
        let marks = match &self.git_base {
            Some(base) => crate::dirty_diff::marks(base, &self.editor.read(cx).value()),
            None => Vec::new(),
        };
        if marks != self.marks {
            self.marks = marks;
            cx.notify();
        }
    }

    /// Put the cursor at `line` and `column` (both from 1), scrolled into view.
    pub fn go_to(&mut self, line: u32, column: u32, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let position = Position::new(line.saturating_sub(1), column.saturating_sub(1));
        self.editor.update(cx, |editor, cx| {
            editor.set_cursor_position(position, window, cx);
            if focus {
                editor.focus(window, cx);
            }
        });
    }

    /// Take what is on disk now (an agent edited the file, `git checkout`),
    /// keeping the cursor. A tab with unsaved changes keeps them.
    pub fn reload_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.look == Look::Image {
            return cx.notify();
        }
        self.refresh_git_base(cx);
        if self.dirty {
            return;
        }
        let Ok((text, _)) = crate::encoding::read(&self.path, Some(&self.encoding)) else { return };
        if text.as_str() == self.saved.as_ref() {
            return;
        }
        self.saved = text.clone().into();
        self.editor.update(cx, |editor, cx| {
            let position = editor.cursor_position();
            editor.set_value(text, window, cx);
            editor.set_cursor_position(position, window, cx);
        });
        cx.notify();
    }

    fn save(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        if self.look == Look::Image {
            return;
        }
        // Format on save, when the file has a formatter; saved either way.
        if Settings::get(cx).format_on_save && crate::backend::format::tool_for(&self.path).is_ok() {
            return self.format(true, window, cx);
        }
        self.write(window, cx);
    }

    /// Format Document: the buffer through the file's formatter, as one step
    /// to undo. With `then_save`, the file is saved after (also on failure).
    pub fn format(&mut self, then_save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let tool = match crate::backend::format::tool_for(&self.path) {
            Ok(tool) => tool,
            Err(err) => {
                if then_save {
                    self.write(window, cx);
                } else if let Some(kit) = crate::backend::format::kit_for(&self.path) {
                    self.offer_formatter(kit, window, cx);
                } else {
                    crate::toast::push(window, format!("Format: {err}"), cx);
                }
                return;
            }
        };
        let text = self.editor.read(cx).value().to_string();
        let path = self.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let input = text.clone();
            let result = cx.background_spawn(async move { crate::backend::format::run(&tool, &path, &input) }).await;
            _ = this.update_in(cx, |this, window, cx| {
                match result {
                    // Edits made while it ran win over the result.
                    Ok(formatted) if formatted != text && this.editor.read(cx).value().as_ref() == text.as_str() => {
                        this.editor.update(cx, |editor, cx| {
                            let position = editor.cursor_position();
                            editor.replace_all(formatted, window, cx);
                            editor.set_cursor_position(position, window, cx);
                        });
                    }
                    Ok(_) => {}
                    Err(err) => crate::toast::push(window, format!("Format failed: {err}"), cx),
                }
                if then_save {
                    this.write(window, cx);
                }
            });
        })
        .detach();
    }

    /// Ask to download the missing formatter, then format with it.
    fn offer_formatter(&mut self, kit: &'static crate::backend::format::Kit, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let what = format!("Format Document uses {name} for {}. Download {name} ({}) into den's data folder?", kit.formats, kit.download_size(), name = kit.name);
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog.title("Format Document").description(what.clone()).ok_text("Download").show_cancel(true).on_ok(move |_, window, cx| {
                _ = this.update(cx, |this, cx| this.install_formatter(kit, window, cx));
                true
            })
        });
    }

    fn install_formatter(&mut self, kit: &'static crate::backend::format::Kit, window: &mut Window, cx: &mut Context<Self>) {
        crate::toast::push(window, format!("Downloading {}…", kit.name), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { crate::backend::format::install(kit, &crate::backend::http::Cancel::default(), &mut |_, _, _| {}) })
                .await;
            _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.format(false, window, cx),
                Err(err) => crate::toast::push(window, format!("Could not download {}: {err}", kit.name), cx),
            });
        })
        .detach();
    }

    fn write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value();
        match crate::encoding::write(&self.path, &text, &self.encoding) {
            Ok(()) => {
                self.saved = text;
                self.dirty = false;
                self.error = None;
                self.refresh_git_base(cx);
                cx.emit(PaneEvent::Changed);
                cx.emit(PaneEvent::Saved(self.path.clone()));
            }
            Err(err) => {
                crate::toast::push(window, format!("Could not save {}: {err}", self.path.display()), cx);
            }
        }
        cx.notify();
    }
}

/// The language modes to pick from: highlighter id, name.
const LANGUAGES: &[(&str, &str)] = &[
    ("text", "Plain Text"),
    ("bash", "Shell"),
    ("css", "CSS"),
    ("html", "HTML"),
    ("javascript", "JavaScript"),
    ("json", "JSON"),
    ("markdown", "Markdown"),
    ("python", "Python"),
    ("rust", "Rust"),
    ("svelte", "Svelte"),
    ("toml", "TOML"),
    ("tsx", "TypeScript JSX"),
    ("typescript", "TypeScript"),
    ("yaml", "YAML"),
];

/// The extension doubles as the highlighter's language name; unknown ones
/// fall back to plain text inside the editor.
fn language_of(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match name.as_str() {
        "makefile" => return "make".into(),
        "cargo.lock" => return "toml".into(),
        _ => {}
    }
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("") {
        "mjs" | "cjs" | "jsx" => "javascript".into(),
        "mts" | "cts" => "typescript".into(),
        "htm" | "svg" | "xml" => "html".into(),
        ext => ext.to_lowercase(),
    }
}

impl EventEmitter<PaneEvent> for FilePanel {}

impl Focusable for FilePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Pane for FilePanel {
    fn kind(&self) -> &'static str {
        FILE
    }

    fn icon_element(&self, cx: &App) -> Option<AnyElement> {
        let name = self.path.file_name()?.to_string_lossy().to_string();
        Some(crate::file_icon::render(&name, 16., cx))
    }

    fn icon(&self, _: &App) -> IconName {
        match self.look {
            Look::Image | Look::Svg => IconName::Image,
            _ => IconName::FileCode,
        }
    }

    fn label(&self, _: &App) -> SharedString {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
            .into()
    }

    fn is_dirty(&self, _: &App) -> bool {
        self.dirty
    }

    fn dump(&self, _: &App) -> Value {
        json!({ "path": self.path.to_string_lossy() })
    }
}

impl FilePanel {
    /// Switch Markdown or SVG between source and preview; the choice is
    /// remembered per type for the next file opened.
    fn set_preview(&mut self, preview: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = preview;
        let look = self.look;
        crate::settings::AppState::update(cx, |state| match look {
            Look::Markdown => state.preview_markdown = preview,
            Look::Svg => state.preview_svg = preview,
            _ => {}
        });
        if !preview {
            self.editor.update(cx, |editor, cx| editor.focus(window, cx));
        }
        cx.notify();
    }

    /// The Source / Preview switch above a Markdown or SVG file.
    fn render_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let preview = self.preview;
        h_flex()
            .flex_none()
            .justify_end()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("source")
                    .xsmall()
                    .label("Source")
                    .map(|b| if preview { b.ghost() } else { b.primary() })
                    .on_click(cx.listener(|this, _, window, cx| this.set_preview(false, window, cx))),
            )
            .child(
                Button::new("preview")
                    .xsmall()
                    .label("Preview")
                    .map(|b| if preview { b.primary() } else { b.ghost() })
                    .on_click(cx.listener(|this, _, window, cx| this.set_preview(true, window, cx))),
            )
    }

    /// The preview of the buffer, unsaved edits included.
    fn render_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.editor.read(cx).value();
        match self.look {
            Look::Markdown => {
                // Images load relative to the document.
                let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
                div()
                    .size_full()
                    .px_6()
                    .py_4()
                    .child(
                        gpui_kit::base::TextView::markdown(("markdown", cx.entity_id()), text)
                            .scrollable(true)
                            .selectable(true)
                            .size_full()
                            .image_source(move |url| {
                                let url = url.to_string();
                                if url.contains("://") || url.starts_with("data:") {
                                    ImageSource::from(url)
                                } else {
                                    ImageSource::from(dir.join(url.trim_start_matches("./")))
                                }
                            }),
                    )
                    .into_any_element()
            }
            Look::Svg => {
                let image = match &self.svg {
                    Some((source, image)) if *source == text => image.clone(),
                    _ => {
                        let image = Arc::new(Image::from_bytes(ImageFormat::Svg, text.as_bytes().to_vec()));
                        self.svg = Some((text, image.clone()));
                        image
                    }
                };
                picture(img(image), cx)
            }
            _ => picture(img(self.path.clone()), cx),
        }
    }
}

impl FilePanel {
    /// Ask for `line` or `line:column` and go there.
    fn prompt_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let lines = self.editor.read(cx).value().lines().count().max(1);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(format!("Line (1–{lines}), or line:column")));
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, {
            let input = input.clone();
            move |dialog, _, _| {
                let input = input.clone();
                let this = this.clone();
                dialog.title("Go to Line").show_cancel(true).child(Input::new(&input)).on_ok(move |_, window, cx| {
                    let text = input.read(cx).value().to_string();
                    let mut parts = text.trim().split([':', ',']);
                    let Some(line) = parts.next().and_then(|l| l.trim().parse::<u32>().ok()) else { return false };
                    let column = parts.next().and_then(|c| c.trim().parse::<u32>().ok()).unwrap_or(1);
                    _ = this.update(cx, |this, cx| this.go_to(line, column, true, window, cx));
                    true
                })
            }
        });
        window.defer(cx, move |window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    }

    fn set_indent(&mut self, hard_tabs: bool, size: usize, cx: &mut Context<Self>) {
        self.indent = (hard_tabs, size);
        self.editor.update(cx, |editor, cx| editor.set_tab_size(TabSize { tab_size: size, hard_tabs }, cx));
        cx.notify();
    }

    fn set_language(&mut self, language: &str, cx: &mut Context<Self>) {
        self.language = language.to_string();
        self.editor.update(cx, |editor, cx| editor.set_highlighter(language.to_string(), cx));
        cx.notify();
    }

    /// Read the file again in `encoding` (unsaved changes would be lost:
    /// refused while there are any).
    fn reopen_with(&mut self, encoding: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            crate::toast::push(window, "Save or undo the changes before reopening the file in another encoding.", cx);
            return;
        }
        match crate::encoding::read(&self.path, Some(encoding)) {
            Ok((text, encoding)) => {
                self.encoding = encoding;
                self.saved = text.clone().into();
                self.editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
                self.update_marks(cx);
            }
            Err(err) => crate::toast::push(window, format!("Could not reopen: {err}"), cx),
        }
        cx.notify();
    }

    /// Save in `encoding` from now on.
    fn save_with(&mut self, encoding: &str, window: &mut Window, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.encoding, encoding.to_string());
        let text = self.editor.read(cx).value();
        if let Err(err) = crate::encoding::write(&self.path, &text, encoding) {
            self.encoding = previous;
            crate::toast::push(window, format!("Could not save in {}: {err}", crate::encoding::short_name(encoding)), cx);
            return cx.notify();
        }
        self.write(window, cx);
    }

    /// Change every line ending, as one step to undo.
    fn set_line_endings(&mut self, crlf: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        let unix = text.replace("\r\n", "\n");
        let converted = if crlf { unix.replace('\n', "\r\n") } else { unix };
        if converted != text {
            self.editor.update(cx, |editor, cx| {
                let position = editor.cursor_position();
                editor.replace_all(converted, window, cx);
                editor.set_cursor_position(position, window, cx);
            });
        }
    }

    /// The bar under the file, as den's: cursor position (Go to Line),
    /// indentation, encoding (reopen or save in another), line endings and
    /// language mode, each a menu.
    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let editor = self.editor.read(cx);
        let position = editor.cursor_position();
        let crlf = editor.value().contains("\r\n");
        let language = LANGUAGES.iter().find(|(id, _)| *id == self.language).map_or_else(
            || {
                let mut chars = self.language.chars();
                chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_else(|| "Plain Text".into())
            },
            |(_, name)| name.to_string(),
        );
        let item = |id: &'static str, label: String| Button::new(id).ghost().xsmall().label(label).text_color(theme.muted_foreground);
        let (hard_tabs, size) = self.indent;
        let encoding = self.encoding.clone();
        let current_language = self.language.clone();
        let this = cx.weak_entity();
        h_flex()
            .flex_none()
            .h(px(22.))
            .px_2()
            .gap_1()
            .justify_end()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.status_bar)
            .text_xs()
            .child(
                item("status-position", format!("Ln {}, Col {}", position.line + 1, position.character + 1))
                    .tooltip("Go to Line")
                    .on_click(cx.listener(|this, _, window, cx| this.prompt_go_to_line(window, cx))),
            )
            .child({
                let this = this.clone();
                item("status-indent", if hard_tabs { format!("Tab Size: {size}") } else { format!("Spaces: {size}") })
                    .tooltip("Indentation")
                    .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                        let mut menu = menu.label("INDENT USING SPACES");
                        for width in [2usize, 4, 8] {
                            let this = this.clone();
                            menu = menu.item(PopupMenuItem::new(format!("{width} spaces")).checked(!hard_tabs && size == width).on_click(move |_, _, cx| {
                                _ = this.update(cx, |this, cx| this.set_indent(false, width, cx));
                            }));
                        }
                        let this = this.clone();
                        menu.separator().item(PopupMenuItem::new("Indent Using Tabs").checked(hard_tabs).on_click(move |_, _, cx| {
                            _ = this.update(cx, |this, cx| this.set_indent(true, 4, cx));
                        }))
                    })
            })
            .child({
                let this = this.clone();
                item("status-encoding", crate::encoding::short_name(&self.encoding).to_string())
                    .tooltip("Encoding")
                    .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, window, cx| {
                        let (reopen, save) = (this.clone(), this.clone());
                        let (reopen_current, save_current) = (encoding.clone(), encoding.clone());
                        menu.submenu("Reopen with Encoding", window, cx, move |menu, _, _| {
                            let mut menu = menu.max_h(px(420.)).scrollable(true);
                            for (id, name, _) in crate::encoding::ENCODINGS {
                                let this = reopen.clone();
                                menu = menu.item(PopupMenuItem::new(*name).checked(*id == reopen_current).on_click(move |_, window, cx| {
                                    _ = this.update(cx, |this, cx| this.reopen_with(id, window, cx));
                                }));
                            }
                            menu
                        })
                        .submenu("Save with Encoding", window, cx, move |menu, _, _| {
                            let mut menu = menu.max_h(px(420.)).scrollable(true);
                            for (id, name, _) in crate::encoding::ENCODINGS {
                                let this = save.clone();
                                menu = menu.item(PopupMenuItem::new(*name).checked(*id == save_current).on_click(move |_, window, cx| {
                                    _ = this.update(cx, |this, cx| this.save_with(id, window, cx));
                                }));
                            }
                            menu
                        })
                    })
            })
            .child({
                let this = this.clone();
                item("status-eol", if crlf { "CRLF".into() } else { "LF".into() })
                    .tooltip("Line endings")
                    .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                        let (lf, crlf_item) = (this.clone(), this.clone());
                        menu.item(PopupMenuItem::new("LF").checked(!crlf).on_click(move |_, window, cx| {
                            _ = lf.update(cx, |this, cx| this.set_line_endings(false, window, cx));
                        }))
                        .item(PopupMenuItem::new("CRLF").checked(crlf).on_click(move |_, window, cx| {
                            _ = crlf_item.update(cx, |this, cx| this.set_line_endings(true, window, cx));
                        }))
                    })
            })
            .child(
                item("status-language", language).tooltip("Language mode").dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                    let mut menu = menu.label("LANGUAGE MODE").max_h(px(420.)).scrollable(true);
                    for (id, name) in LANGUAGES {
                        let this = this.clone();
                        menu = menu.item(PopupMenuItem::new(*name).checked(*id == current_language).on_click(move |_, _, cx| {
                            _ = this.update(cx, |this, cx| this.set_language(id, cx));
                        }));
                    }
                    menu
                }),
            )
    }
}

impl FilePanel {
    /// The gutter marks: a thin strip over the editor's left edge, painted
    /// at the rows the editor shows. Off with soft wrap, where rows are not
    /// lines.
    fn render_marks(&self, cx: &App) -> Option<AnyElement> {
        use crate::dirty_diff::Mark;
        if self.marks.is_empty() || Settings::get(cx).soft_wrap {
            return None;
        }
        let editor = self.editor.read(cx);
        let line_height = editor.line_height()?;
        let scroll = editor.scroll_offset().y;
        let theme = cx.theme();
        let colors = (theme.green, theme.blue, theme.red);
        let marks = self.marks.clone();
        Some(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    for (line, mark) in &marks {
                        let top = bounds.top() + scroll + line_height * *line as f32;
                        if top + line_height < bounds.top() || top > bounds.bottom() {
                            continue;
                        }
                        let (color, rect) = match mark {
                            Mark::Added => (colors.0, Bounds::new(point(bounds.left(), top), size(px(3.), line_height))),
                            Mark::Modified => (colors.1, Bounds::new(point(bounds.left(), top), size(px(3.), line_height))),
                            Mark::Deleted => (colors.2, Bounds::new(point(bounds.left(), top - px(2.)), size(px(6.), px(4.)))),
                        };
                        window.paint_quad(fill(rect, color));
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .h_full()
            .w(px(6.))
            .into_any_element(),
        )
    }
}

impl FilePanel {
    /// The overview of the gutter marks beside the editor, as VS Code's
    /// ruler: where the changed lines are in the whole file. Press or drag
    /// to go there.
    fn render_overview(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::dirty_diff::Mark;
        if self.marks.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let colors = (theme.green, theme.blue, theme.red);
        let lines = self.editor.read(cx).value().lines().count().max(1);
        let marks = self.marks.clone();
        let bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>> = Default::default();
        let seek = {
            let bounds = bounds.clone();
            move |this: &mut Self, y: Pixels, window: &mut Window, cx: &mut Context<Self>| {
                let Some(area) = bounds.get() else { return };
                let t = ((y - area.top()) / area.size.height).clamp(0., 1.);
                let line = (t * lines as f32) as u32 + 1;
                this.go_to(line, 1, false, window, cx);
            }
        };
        let seek_move = seek.clone();
        Some(
            div()
                .id("file-overview")
                .flex_none()
                .w(px(8.))
                .h_full()
                .child(
                    canvas(
                        {
                            let bounds = bounds.clone();
                            move |area, _, _| bounds.set(Some(area))
                        },
                        move |area, _, window, _| {
                            let height = area.size.height;
                            let row = (height / lines as f32).max(px(2.));
                            for (line, mark) in &marks {
                                let y = area.top() + height * (*line as f32 / lines as f32);
                                let color = match mark {
                                    Mark::Added => colors.0,
                                    Mark::Modified => colors.1,
                                    Mark::Deleted => colors.2,
                                };
                                window.paint_quad(fill(Bounds::new(point(area.left() + px(2.), y), size(px(4.), row)), color));
                            }
                        },
                    )
                    .size_full(),
                )
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, window, cx| seek(this, event.position.y, window, cx)))
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        seek_move(this, event.position.y, window, cx);
                    }
                }))
                .into_any_element(),
        )
    }
}

/// The editor's right-click menu, as den's: the edit actions (they go to the
/// editor and show their keys), Find and Replace, Go to Line and Format
/// Document.
fn editor_menu(menu: PopupMenu, focus: FocusHandle, this: WeakEntity<FilePanel>) -> PopupMenu {
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, Replace, Search, SelectAll, Undo};
    menu.action_context(focus)
        .menu("Cut", Box::new(Cut))
        .menu("Copy", Box::new(Copy))
        .menu("Paste", Box::new(Paste))
        .separator()
        .menu("Select All", Box::new(SelectAll))
        .menu("Undo", Box::new(Undo))
        .menu("Redo", Box::new(Redo))
        .separator()
        .menu("Find", Box::new(Search))
        .menu("Replace", Box::new(Replace))
        .item(PopupMenuItem::new("Go to Line…").on_click(move |_, window, cx| {
            _ = this.update(cx, |this, cx| this.prompt_go_to_line(window, cx));
        }))
        .separator()
        .menu("Format Document", Box::new(crate::FormatDocument))
}

/// A picture centred in the pane, at most its size.
fn picture(image: Img, cx: &App) -> AnyElement {
    div()
        .size_full()
        .p_4()
        .flex()
        .items_center()
        .justify_center()
        .bg(cx.theme().muted)
        .child(
            image
                .max_w_full()
                .max_h_full()
                .object_fit(ObjectFit::ScaleDown)
                .with_fallback(|| div().text_sm().child("Cannot show this image").into_any_element()),
        )
        .into_any_element()
}

impl Render for FilePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = Settings::get(cx).editor_font_size;
        let previewable = matches!(self.look, Look::Markdown | Look::Svg);
        let showing_preview = self.look == Look::Image || (previewable && self.preview);
        let preview = showing_preview.then(|| self.render_preview(cx));
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::save))
            .when(previewable, |this| this.child(self.render_switch(cx)))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .bg(cx.theme().danger)
                        .text_color(cx.theme().danger_foreground)
                        .child(error),
                )
            })
            .child(div().flex_1().min_h_0().map(|this| match preview {
                Some(preview) => this.child(preview),
                None => this.child(
                    h_flex()
                        .size_full()
                        .child(
                            div()
                                .id("editor-area")
                                .relative()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .context_menu({
                                    let focus = self.editor.focus_handle(cx);
                                    let this = cx.weak_entity();
                                    move |menu, _, _| editor_menu(menu, focus.clone(), this.clone())
                                })
                                .child(
                                    Editor::new(&self.editor)
                                        .bordered(false)
                                        .p_0()
                                        .h(relative(1.))
                                        .font_family(crate::settings::mono_font(cx))
                                        .text_size(px(font_size)),
                                )
                                .when_some(self.render_marks(cx), |this, marks| this.child(marks)),
                        )
                        .when_some(self.render_overview(cx), |this, ruler| this.child(ruler)),
                ),
            }))
            .when(self.look != Look::Image && !showing_preview, |this| this.child(self.render_status(cx)))
    }
}

// ---------------------------------------------------------------------------
// Settings

pub struct SettingsPanel {
    focus_handle: FocusHandle,
    /// Filters the settings as you type, as den's search does.
    search: Entity<InputState>,
    font_family: Entity<InputState>,
    shell: Entity<InputState>,
    browser_home: Entity<InputState>,
    /// The imported colour themes.
    themes: Vec<crate::theme::Imported>,
    /// One name and one command field per preset, in the order of the list.
    preset_rows: Vec<(Entity<InputState>, Entity<InputState>, Vec<Subscription>)>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::get(cx).clone();
        let font_family = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("The theme's monospace font")
                .default_value(settings.font_family.clone())
        });
        let shell = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Automatic: pwsh, else Windows PowerShell")
                .default_value(settings.shell.clone())
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        let browser_home = cx.new(|cx| InputState::new(window, cx).placeholder("https://www.google.com").default_value(settings.browser_home.clone()));
        let _subscriptions = vec![
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&browser_home, |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().trim().to_string();
                    Settings::update(cx, |s| s.browser_home = value);
                }
            }),
            cx.subscribe(&font_family, |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    Settings::update(cx, |s| s.font_family = value);
                }
            }),
            cx.subscribe(&shell, |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    Settings::update(cx, |s| s.shell = value);
                }
            }),
        ];
        Self {
            focus_handle: cx.focus_handle(),
            search,
            font_family,
            shell,
            browser_home,
            themes: crate::theme::imported(),
            preset_rows: Vec::new(),
            _subscriptions,
        }
    }

    pub fn search_input(&self) -> Entity<InputState> {
        self.search.clone()
    }

    /// Ask for a VS Code theme file, import it and switch to it.
    /// Ask for a custom model's download link.
    fn add_model_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("https://huggingface.co/…/model.gguf"));
        window.open_alert_dialog(cx, {
            let input = input.clone();
            move |dialog, _, _| {
                let input = input.clone();
                dialog
                    .title("Add Model from Link")
                    .description("A link to a .gguf file, such as a Hugging Face file page. It downloads on first use, or with its download button.")
                    .show_cancel(true)
                    .child(Input::new(&input))
                    .on_ok(move |_, window, cx| match crate::backend::ai::custom_model(&input.read(cx).value()) {
                        Ok(model) => {
                            add_custom_model(model, cx);
                            true
                        }
                        Err(err) => {
                            crate::toast::push(window, err, cx);
                            false
                        }
                    })
            }
        });
        window.defer(cx, move |window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    }

    /// Pick a custom model's .gguf file.
    fn add_model_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Add Model".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            _ = this.update_in(cx, |_, window, cx| match crate::backend::ai::custom_model(&path.to_string_lossy()) {
                Ok(model) => add_custom_model(model, cx),
                Err(err) => crate::toast::push(window, err, cx),
            });
        })
        .detach();
    }

    fn import_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import Theme".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            _ = this.update_in(cx, |this, window, cx| match crate::theme::import(&path) {
                Ok(id) => {
                    this.themes = crate::theme::imported();
                    let dark = this.themes.iter().find(|t| t.id == id).is_none_or(|t| t.theme.dark);
                    Settings::update(cx, |s| {
                        s.color_theme = id;
                        s.theme = if dark { ThemeChoice::Dark } else { ThemeChoice::Light };
                    });
                }
                Err(err) => {
                    crate::toast::push(window, format!("Could not import {}: {err:#}", path.display()), cx);
                }
            });
        })
        .detach();
    }

    /// Keep one pair of fields per preset; rebuilt when the list changes
    /// shape (a preset added, removed or moved).
    fn sync_preset_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let presets = Settings::get(cx).presets.clone();
        let same = presets.len() == self.preset_rows.len()
            && presets.iter().zip(&self.preset_rows).all(|(preset, (name, command, _))| {
                name.read(cx).value() == preset.name.as_str() && command.read(cx).value() == preset.command.as_str()
            });
        if same {
            return;
        }
        self.preset_rows = presets
            .iter()
            .enumerate()
            .map(|(ix, preset)| {
                let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name").default_value(preset.name.clone()));
                let command = cx.new(|cx| InputState::new(window, cx).placeholder("Command").default_value(preset.command.clone()));
                let subscriptions = vec![
                    cx.subscribe(&name, move |_, input, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let value = input.read(cx).value().to_string();
                            Settings::update(cx, |s| {
                                if let Some(preset) = s.presets.get_mut(ix) {
                                    preset.name = value;
                                }
                            });
                        }
                    }),
                    cx.subscribe(&command, move |_, input, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let value = input.read(cx).value().to_string();
                            Settings::update(cx, |s| {
                                if let Some(preset) = s.presets.get_mut(ix) {
                                    preset.command = value;
                                }
                            });
                        }
                    }),
                ];
                (name, command, subscriptions)
            })
            .collect();
    }

    /// The presets of one section (agents or terminals), as den lists them:
    /// mark (click for another colour), pin, name, command, order, remove.
    fn render_presets(&self, kind: Kind, cx: &mut Context<Self>) -> Div {
        let agents = kind == Kind::Agents;
        let browsers = kind == Kind::Browsers;
        let theme = cx.theme().clone();
        let presets = Settings::get(cx).presets.clone();
        let indices: Vec<usize> = presets.iter().enumerate().filter(|(_, p)| p.kind() == kind).map(|(ix, _)| ix).collect();
        let section = match kind {
            Kind::Agents => "agent",
            Kind::Browsers => "browser",
            _ => "terminal",
        };
        v_flex()
            .w_full()
            .rounded(px(6.))
            .border_1()
            .border_color(theme.border)
            .children(indices.iter().enumerate().filter_map(|(pos, &ix)| {
                let preset = presets.get(ix)?;
                let (name, command, _) = self.preset_rows.get(ix)?;
                let previous = pos.checked_sub(1).map(|p| indices[p]);
                let next = indices.get(pos + 1).copied();
                let pinned = preset.pinned;
                Some(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            Popover::new(SharedString::from(format!("preset-picker-{ix}")))
                                .anchor(Anchor::TopLeft)
                                .trigger(
                                    Button::new(SharedString::from(format!("preset-mark-{ix}")))
                                        .ghost()
                                        .small()
                                        .child(crate::layout_view::preset_badge(preset, cx))
                                        .tooltip("Icon and colour"),
                                )
                                .content(move |_, _, cx| crate::preset_icon::picker(ix, cx)),
                        )
                        .child(
                            Button::new(SharedString::from(format!("preset-pin-{ix}")))
                                .small()
                                .icon(Icon::new(if pinned { IconName::Pin } else { IconName::PinOff }))
                                .map(|b| if pinned { b.primary() } else { b.ghost() })
                                .tooltip(if pinned { "Pinned: a button on every tab strip" } else { "Pin to the tab strips" })
                                .on_click(move |_, _, cx| {
                                    Settings::update(cx, |s| {
                                        if let Some(preset) = s.presets.get_mut(ix) {
                                            preset.pinned = !preset.pinned;
                                        }
                                    })
                                }),
                        )
                        .child(div().w(px(180.)).flex_none().child(Input::new(name).small()))
                        .child(div().flex_1().min_w_0().child(Input::new(command).small()))
                        .child(
                            Button::new(SharedString::from(format!("preset-up-{ix}")))
                                .ghost()
                                .small()
                                .icon(Icon::new(IconName::ArrowUp))
                                .disabled(previous.is_none())
                                .tooltip("Earlier")
                                .on_click(move |_, _, cx| {
                                    if let Some(previous) = previous {
                                        Settings::update(cx, |s| s.presets.swap(ix, previous));
                                    }
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("preset-down-{ix}")))
                                .ghost()
                                .small()
                                .icon(Icon::new(IconName::ArrowDown))
                                .disabled(next.is_none())
                                .tooltip("Later")
                                .on_click(move |_, _, cx| {
                                    if let Some(next) = next {
                                        Settings::update(cx, |s| s.presets.swap(ix, next));
                                    }
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("preset-remove-{ix}")))
                                .ghost()
                                .small()
                                .icon(Icon::new(IconName::X))
                                .tooltip("Remove")
                                .on_click(move |_, _, cx| {
                                    Settings::update(cx, |s| {
                                        if ix < s.presets.len() {
                                            s.presets.remove(ix);
                                        }
                                    })
                                }),
                        ),
                )
            }))
            .child(
                div().px_3().py_2().child(
                    Button::new(SharedString::from(format!("preset-add-{section}")))
                        .small()
                        .outline()
                        .icon(Icon::new(IconName::Plus))
                        .label("Add Preset")
                        .on_click(move |_, _, cx| {
                            Settings::update(cx, |s| {
                                let name = if agents { "New agent" } else if browsers { "New page" } else { "New preset" };
                                s.presets.push(Preset { agent: agents, browser: browsers, ..Preset::new(name, "") })
                            })
                        }),
                ),
            )
    }
}

impl SettingsPanel {
    /// The theme menu: the built-in and imported themes (hovering one
    /// previews it), Import and Remove.
    fn render_theme_menu(&self, settings: &Settings, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let builtin = settings.color_theme.is_empty();
        let imported: Vec<(String, String, bool)> = self.themes.iter().map(|t| (t.id.clone(), t.theme.name.clone(), t.theme.dark)).collect();
        let current_theme = if builtin {
            if settings.theme == ThemeChoice::Dark { "Dark".to_string() } else { "Light".to_string() }
        } else {
            imported.iter().find(|(id, ..)| *id == settings.color_theme).map_or_else(|| settings.color_theme.clone(), |(_, name, _)| name.clone())
        };
        let (base, color_theme) = (settings.theme, settings.color_theme.clone());
        let this = cx.weak_entity();
        Button::new("theme")
            .small()
            .outline()
            .label(current_theme)
            .icon(Icon::new(IconName::ChevronDown))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                // Hovering a theme previews it; closing the menu goes back to the chosen one.
                cx.subscribe_self(|_, _: &DismissEvent, cx| crate::theme::apply(cx)).detach();
                let item = |label: String, choice: ThemeChoice, id: String, checked: bool| {
                    let hover_id = id.clone();
                    PopupMenuItem::element(move |_, _| {
                        let id = hover_id.clone();
                        div().id(SharedString::from(format!("theme-item-{label}"))).w_full().child(label.clone()).on_hover(move |hovered, _, cx| {
                            if *hovered {
                                crate::theme::preview(choice, &id, cx);
                            }
                        })
                    })
                    .checked(checked)
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        Settings::update(cx, |s| {
                            s.theme = choice;
                            s.color_theme = id;
                        })
                    })
                };
                let mut menu = menu
                    .item(item("Dark".into(), ThemeChoice::Dark, String::new(), builtin && base == ThemeChoice::Dark))
                    .item(item("Light".into(), ThemeChoice::Light, String::new(), builtin && base == ThemeChoice::Light));
                if !imported.is_empty() {
                    menu = menu.separator();
                }
                for (id, name, dark) in &imported {
                    let choice = if *dark { ThemeChoice::Dark } else { ThemeChoice::Light };
                    menu = menu.item(item(name.clone(), choice, id.clone(), color_theme == *id));
                }
                let import = this.clone();
                menu = menu.separator().item(PopupMenuItem::new("Import VS Code Theme…").icon(Icon::new(IconName::FolderOpen)).on_click(move |_, window, cx| {
                    _ = import.update(cx, |this, cx| this.import_theme(window, cx));
                }));
                if !builtin {
                    let remove = this.clone();
                    menu = menu.item(PopupMenuItem::new("Remove This Theme").icon(Icon::new(IconName::Trash)).on_click(move |_, _, cx| {
                        _ = remove.update(cx, |this, cx| {
                            let id = Settings::get(cx).color_theme.clone();
                            crate::theme::remove(&id);
                            this.themes = crate::theme::imported();
                            Settings::update(cx, |s| s.color_theme.clear());
                        });
                    }));
                }
                menu
            })
    }

    /// The AI model menu: den's presets and the added models, each with
    /// its download, and adding one by link or file.
    fn render_ai_model(&self, settings: &Settings, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        use crate::backend::ai;
        let current = settings.ai_model.clone();
        let label = ai::MODEL_PRESETS.iter().find(|p| p.url == current).map_or_else(|| ai::model_name(&current), |p| p.name.to_string());
        // The added models, and the chosen one when it is neither a preset nor on the list.
        let mut customs = settings.ai_custom_models.clone();
        if !ai::MODEL_PRESETS.iter().any(|p| p.url == current) && !customs.contains(&current) {
            customs.push(current.clone());
        }
        let this = cx.weak_entity();
        Button::new("ai-model")
            .small()
            .outline()
            .label(label)
            .icon(Icon::new(IconName::ChevronDown))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                for preset in ai::MODEL_PRESETS {
                    let (url, name, context) = (preset.url, preset.name, preset.context);
                    let detail = format!("{} · {}", preset.size, preset.fits);
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| menu_entry(format!("ai-model-{name}"), name, &detail, vec![model_download(url)], cx))
                            .checked(current == url)
                            .on_click(move |_, _, cx| {
                                Settings::update(cx, |s| {
                                    s.ai_model = url.to_string();
                                    s.ai_context_size = context;
                                })
                            }),
                    );
                }
                if !customs.is_empty() {
                    menu = menu.separator();
                }
                for model in &customs {
                    let (model, pick) = (model.clone(), model.clone());
                    let checked = current == model;
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            let detail = if ai::is_download(&model) { "link" } else { "file" };
                            let forget = model.clone();
                            let forget = Download::Forget(Rc::new(move |_, cx| {
                                Settings::update(cx, |s| {
                                    s.ai_custom_models.retain(|m| *m != forget);
                                    if s.ai_model == forget {
                                        s.ai_model.clear();
                                    }
                                })
                            }));
                            let mut actions = Vec::new();
                            if ai::is_download(&model) {
                                actions.push(model_download(&model));
                            }
                            if !ai::model_downloaded(&model) {
                                actions.push(forget);
                            }
                            menu_entry(format!("ai-model-{model}"), ai::model_name(&model), detail, actions, cx)
                        })
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            let pick = pick.clone();
                            Settings::update(cx, |s| s.ai_model = pick)
                        }),
                    );
                }
                let (by_link, by_file) = (this.clone(), this.clone());
                menu.separator()
                    .item(PopupMenuItem::new("Add Model from Link…").icon(Icon::new(IconName::Globe)).on_click(move |_, window, cx| {
                        _ = by_link.update(cx, |this, cx| this.add_model_link(window, cx));
                    }))
                    .item(PopupMenuItem::new("Add GGUF File…").icon(Icon::new(IconName::FolderOpen)).on_click(move |_, window, cx| {
                        _ = by_file.update(cx, |this, cx| this.add_model_file(window, cx));
                    }))
            })
    }
}

impl EventEmitter<PaneEvent> for SettingsPanel {}

impl Focusable for SettingsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Pane for SettingsPanel {
    fn kind(&self) -> &'static str {
        SETTINGS
    }

    fn icon(&self, _: &App) -> IconName {
        IconName::Settings
    }

    fn label(&self, _: &App) -> SharedString {
        "Settings".into()
    }

    fn dump(&self, _: &App) -> Value {
        json!({})
    }
}

thread_local! {
    /// The search's words while Settings renders, and its current section.
    static FILTER: std::cell::RefCell<(Vec<String>, &'static str, usize)> = const { std::cell::RefCell::new((Vec::new(), "", 0)) };
}

/// Whether a setting with these texts passes the search (and count it).
fn passes(texts: &[&str]) -> bool {
    FILTER.with(|filter| {
        let mut filter = filter.borrow_mut();
        let haystack = format!("{} {}", texts.join(" "), filter.1).to_lowercase();
        let ok = filter.0.iter().all(|word| haystack.contains(word.as_str()));
        if ok {
            filter.2 += 1;
        }
        ok
    })
}

fn filtering() -> bool {
    FILTER.with(|filter| !filter.borrow().0.is_empty())
}

/// One setting: name and description on the left, its control on the right.
fn setting_row(name: impl Into<SharedString>, description: impl Into<SharedString>, control: impl IntoElement, cx: &App) -> Div {
    let (name, description) = (name.into(), description.into());
    if !passes(&[name.as_ref(), description.as_ref()]) {
        return div();
    }
    h_flex()
        .w_full()
        .py_3()
        .gap_6()
        .justify_between()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            // Wraps, so a long description never pushes the control out of view.
            v_flex()
                .flex_1()
                .min_w(px(200.))
                .gap_0p5()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(description)),
        )
        .child(control)
}

type MenuAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// What the end of a download's menu entry offers.
enum Download {
    Get(MenuAction),
    Remove(MenuAction),
    /// Take it off the list (a custom model).
    Forget(MenuAction),
    /// Running: its progress.
    Busy(String),
}

/// A download's menu entry: its name, a muted detail, and at the end its
/// buttons (download, bin, forget), or the progress while it downloads.
fn menu_entry(id: String, name: impl Into<SharedString>, detail: &str, actions: Vec<Download>, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let button = |suffix: &str, icon: IconName, tooltip: &'static str, action: MenuAction| {
        Button::new(SharedString::from(format!("{id}-{suffix}"))).xsmall().ghost().icon(Icon::new(icon)).tooltip(tooltip).on_click(move |_, window, cx| {
            // Not the entry's own click (choosing it).
            cx.stop_propagation();
            action(window, cx);
            window.refresh();
        })
    };
    h_flex()
        .w_full()
        .gap_4()
        .justify_between()
        .child(
            // A long detail ends in an ellipsis rather than pushing the buttons out.
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(div().flex_shrink_0().child(name.into()))
                .child(div().min_w_0().truncate().text_xs().text_color(muted).child(detail.to_string())),
        )
        .child(h_flex().flex_shrink_0().gap_1().children(actions.into_iter().map(|action| match action {
            Download::Get(action) => button("get", IconName::Download, "Download", action).into_any_element(),
            Download::Remove(action) => button("remove", IconName::Trash, "Remove the download", action).into_any_element(),
            Download::Forget(action) => button("forget", IconName::Close, "Take off the list", action).into_any_element(),
            Download::Busy(progress) => div().text_xs().text_color(muted).child(progress).into_any_element(),
        })))
}

/// A model's download button, bin, or progress (the runtime downloads
/// with it when missing).
fn model_download(model: &str) -> Download {
    use crate::backend::ai;
    let key = format!("ai-model-{model}");
    if let Some(progress) = crate::downloads::download_progress(&key) {
        return Download::Busy(progress);
    }
    let model = model.to_string();
    let name = ai::MODEL_PRESETS.iter().find(|p| p.url == model).map_or_else(|| ai::model_name(&model), |p| p.name.to_string());
    if ai::model_downloaded(&model) {
        Download::Remove(Rc::new(move |window, cx| {
            if let Err(err) = ai::remove_model(&model) {
                crate::toast::push(window, format!("Could not remove {name}: {err}"), cx);
            }
        }))
    } else {
        Download::Get(Rc::new(move |window, cx| {
            let mut config = Settings::get(cx).ai_config();
            config.model = model.clone();
            crate::downloads::start_download(key.clone(), name.clone(), window, cx, move |cancel, progress| ai::install(&config, cancel, progress))
        }))
    }
}

/// Put a custom model on the list and choose it.
fn add_custom_model(model: String, cx: &mut App) {
    Settings::update(cx, |s| {
        if !s.ai_custom_models.contains(&model) {
            s.ai_custom_models.push(model.clone());
        }
        s.ai_model = model;
    });
}

/// The downloaded formatters: a menu of every kit with its state.
fn render_formatters() -> impl IntoElement {
    let kits = crate::backend::format::kits();
    let downloaded = kits.iter().filter(|kit| kit.is_installed()).count();
    Button::new("formatters")
        .small()
        .outline()
        .label(format!("{downloaded} of {} downloaded", kits.len()))
        .icon(Icon::new(IconName::ChevronDown))
        .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
            for &kit in crate::backend::format::kits() {
                menu = menu.item(PopupMenuItem::element(move |_, cx| {
                    let key = format!("formatter-{}", kit.name);
                    let installed = kit.is_installed();
                    let detail = format!("{} · {}", kit.formats, if installed { kit.version() } else { kit.size });
                    let download = if let Some(progress) = crate::downloads::download_progress(&key) {
                        Download::Busy(progress)
                    } else if installed {
                        Download::Remove(Rc::new(move |window, cx| {
                            if let Err(err) = crate::backend::format::remove(kit) {
                                crate::toast::push(window, format!("Could not remove {}: {err}", kit.name), cx);
                            }
                        }))
                    } else {
                        let key = key.clone();
                        Download::Get(Rc::new(move |window, cx| {
                            crate::downloads::start_download(key.clone(), kit.name, window, cx, move |cancel, progress| crate::backend::format::install(kit, cancel, progress))
                        }))
                    };
                    menu_entry(key, kit.name, &detail, vec![download], cx)
                }));
            }
            menu
        })
}

fn choice<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    value: T,
    current: T,
    set: fn(&mut Settings, T),
) -> Button {
    Button::new(id)
        .small()
        .label(label)
        .map(|button| if value == current { button.primary() } else { button.outline() })
        .on_click(move |_, _, cx| Settings::update(cx, |settings| set(settings, value)))
}

/// A setting switched on or off.
fn toggle(name: &'static str, description: &'static str, id: &'static str, on: bool, set: fn(&mut Settings, bool), cx: &App) -> Div {
    let switch = Switch::new(id).checked(on).on_click(move |checked, _, cx| {
        let checked = *checked;
        Settings::update(cx, |s| set(s, checked))
    });
    setting_row(name, description, switch, cx)
}

/// A number stepped down or up (`step` with `false` or `true`) by two
/// buttons, shown between them `width` wide; `after` runs on each step.
fn stepper(id: &'static str, value: String, width: Pixels, step: fn(&mut Settings, bool), after: fn(&App)) -> impl IntoElement {
    let button = |suffix: &'static str, icon: IconName, up: bool| {
        Button::new(SharedString::from(format!("{id}-{suffix}"))).small().outline().icon(Icon::new(icon)).on_click(move |_, _, cx| {
            Settings::update(cx, |s| step(s, up));
            after(cx);
        })
    };
    h_flex()
        .gap_2()
        .items_center()
        .child(button("less", IconName::Minus, false))
        .child(div().w(width).text_center().text_sm().child(value))
        .child(button("more", IconName::Plus, true))
}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_preset_rows(window, cx);
        let settings = Settings::get(cx).clone();
        let words: Vec<String> = self.search.read(cx).value().to_lowercase().split_whitespace().map(str::to_string).collect();
        FILTER.with(|filter| *filter.borrow_mut() = (words, "", 0));

        let sidebar = h_flex()
            .gap_1()
            .child(choice("side-left", "Left", SidebarSide::Left, settings.sidebar_position, |s, v| {
                s.sidebar_position = v
            }))
            .child(choice("side-right", "Right", SidebarSide::Right, settings.sidebar_position, |s, v| {
                s.sidebar_position = v
            }));
        let font_size = stepper(
            "font",
            format!("{}", settings.editor_font_size),
            px(32.),
            |s, up| s.editor_font_size = if up { (s.editor_font_size + 1.).min(32.) } else { (s.editor_font_size - 1.).max(8.) },
            |_| {},
        );

        let body = v_flex()
            .id("settings")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .px_2()
                    .pb_4()
                    .child(section_title("Appearance", cx))
                    .child(setting_row("Theme", "Colours of the whole window.", self.render_theme_menu(&settings, cx), cx))
                    .child(setting_row(
                        "Sidebar position",
                        "Which side of the groups the Explorer, Search, Source Control and Extensions sit on.",
                        sidebar,
                        cx,
                    ))
                    .child(section_title("Editor", cx))
                    .child(setting_row("Font family", "Editor and terminal font; empty for the theme's monospace font.", div().w(px(260.)).child(Input::new(&self.font_family).small()), cx))
                    .child(setting_row("Font size", "Text size in file and terminal tabs, in pixels.", font_size, cx))
                    .child(toggle("Line numbers", "Show line numbers in the gutter.", "line-numbers", settings.line_numbers, |s, v| s.line_numbers = v, cx))
                    .child(toggle("Soft wrap", "Wrap long lines at the edge of the editor.", "soft-wrap", settings.soft_wrap, |s, v| s.soft_wrap = v, cx))
                    .child(toggle("Format on save", "Ctrl+S formats the file first, with the formatter Format Document (Shift+Alt+F) uses: an installed one, else one den downloaded.", "format-on-save", settings.format_on_save, |s, v| s.format_on_save = v, cx))
                    .child(setting_row(
                        "Downloaded formatters",
                        "Formatters den downloaded into its data folder because they were not installed. A removed one is offered again the next time Format Document needs it.",
                        render_formatters(),
                        cx,
                    ))
                    .child(section_title("Source Control", cx))
                    .child(setting_row(
                        "Commit with nothing staged",
                        "Ask whether to stage every change and commit it, always do, or never (commit nothing).",
                        h_flex()
                            .gap_1()
                            .child(choice("smart-ask", "Ask", crate::settings::SmartCommit::Ask, settings.smart_commit, |s, v| s.smart_commit = v))
                            .child(choice("smart-always", "Always", crate::settings::SmartCommit::Always, settings.smart_commit, |s, v| s.smart_commit = v))
                            .child(choice("smart-never", "Never", crate::settings::SmartCommit::Never, settings.smart_commit, |s, v| s.smart_commit = v)),
                        cx,
                    ))
                    .child(section_title("AI", cx))
                    .child(setting_row(
                        "Model",
                        "The local model Generate Commit Message runs (llama.cpp, downloaded once on first use into den's data folder). Add your own GGUF model by link or file.",
                        self.render_ai_model(&settings, cx),
                        cx,
                    ))
                    .child(setting_row(
                        "Runtime",
                        "llama.cpp, which runs the model (about 100 MB); downloaded again on next use when removed.",
                        if crate::backend::ai::runtime_downloaded() {
                            Button::new("ai-runtime-remove")
                                .small()
                                .ghost()
                                .icon(Icon::new(IconName::Trash))
                                .label("Remove")
                                .on_click(cx.listener(|_, _, window, cx| {
                                    if let Err(err) = crate::backend::ai::remove_runtime() {
                                        crate::toast::push(window, format!("Could not remove the AI runtime: {err}"), cx);
                                    }
                                    cx.notify();
                                }))
                                .into_any_element()
                        } else {
                            div().text_xs().text_color(cx.theme().muted_foreground).child("not downloaded").into_any_element()
                        },
                        cx,
                    ))
                    .child(setting_row(
                        "Context size",
                        "Tokens the model reads at once; bigger fits larger changes in one go but needs more memory.",
                        stepper(
                            "ai-ctx",
                            format!("{}K", settings.ai_context_size / 1024),
                            px(64.),
                            |s, up| s.ai_context_size = if up { (s.ai_context_size * 2).min(131_072) } else { (s.ai_context_size / 2).max(2048) },
                            |_| {},
                        ),
                        cx,
                    ))
                    .child(toggle("Use the GPU", "Run the model on the graphics card (Vulkan); it falls back to the CPU when the card cannot.", "ai-gpu", settings.ai_gpu, |s, v| s.ai_gpu = v, cx))
                    .child(toggle("Style from history", "On first use in a repository, derive its commit style from the history and save it as .den/commit-style.md (edit it there).", "ai-derive", settings.ai_derive_style, |s, v| s.ai_derive_style = v, cx))
                    .child(section_title("Tabs", cx))
                    .child(toggle("Close buttons", "Show a close button on each tab (a middle-click closes a tab either way).", "tab-close", settings.tab_close_button, |s, v| s.tab_close_button = v, cx))
                    .child(section_title("Notifications", cx))
                    .child(toggle("Notifications", "When a terminal's program wants you (an agent finished or waits for input, a bell) while you look at another tab or window.", "notifications", settings.notifications, |s, v| s.notifications = v, cx))
                    .child(toggle("Toast", "A message in the window's corner; click it to go to the tab.", "notify-toast", settings.notify_toast, |s, v| s.notify_toast = v, cx))
                    .child(toggle("Tab mark", "A dot on the tab until you look at it.", "notify-tab", settings.notify_tab, |s, v| s.notify_tab = v, cx))
                    .child(toggle("Sound", "A sound for a finished turn or a question, picked below.", "notify-sound", settings.notify_sound, |s, v| s.notify_sound = v, cx))
                    .child(toggle("Taskbar", "Flash the taskbar button while the window is in the background.", "notify-taskbar", settings.notify_taskbar, |s, v| s.notify_taskbar = v, cx))
                    .child(setting_row(
                        "Tab strip buttons",
                        "The built-in buttons on each tab strip; presets show by their pin. Also in each group's ⋮ menu.",
                        h_flex()
                            .gap_1()
                            .child(strip_toggle("strip-shell", "Shell", settings.group_buttons.shell, |b| b.shell = !b.shell))
                            .child(strip_toggle("strip-browser", "Browser", settings.group_buttons.browser, |b| b.browser = !b.browser))
                            .child(strip_toggle("strip-split", "Split", settings.group_buttons.split, |b| b.split = !b.split)),
                        cx,
                    ))
                    .child(setting_row(
                        "Sounds",
                        "For a finished turn, and for a question or a permission. Click ▶ to hear one.",
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(sound_picker("sound-done", "Done", settings.notify_sound_done.clone(), |s, v| s.notify_sound_done = v))
                            .child(sound_picker("sound-input", "Input", settings.notify_sound_input.clone(), |s, v| s.notify_sound_input = v)),
                        cx,
                    ))
                    .child(setting_row(
                        "Volume",
                        "Of the notification sounds.",
                        stepper(
                            "volume",
                            format!("{}%", settings.notify_volume),
                            px(48.),
                            |s, up| s.notify_volume = if up { (s.notify_volume + 10).min(100) } else { s.notify_volume.saturating_sub(10) },
                            |cx| {
                                let s = Settings::get(cx);
                                crate::sound::play_named(&s.notify_sound_done, s.notify_volume);
                            },
                        ),
                        cx,
                    ))
                    .child(section_title("Terminal", cx))
                    .child(setting_row("Shell", "For new terminals. TERM_SHELL in the environment overrides the automatic choice.", div().w(px(260.)).child(Input::new(&self.shell).small()), cx))
                    .child(setting_row(
                        "Scrollback",
                        "Lines a terminal keeps above the screen, for new terminals.",
                        stepper(
                            "scrollback",
                            settings.scrollback.to_string(),
                            px(64.),
                            |s, up| s.scrollback = if up { (s.scrollback + 1000).min(200_000) } else { s.scrollback.saturating_sub(1000).max(1000) },
                            |_| {},
                        ),
                        cx,
                    ))
                    .child(preset_section(
                        "Terminal Presets",
                        "Programs a tab-strip button starts in a new terminal: a dev server, a script. The plain shell has its own button.",
                        self.render_presets(Kind::Terminals, cx),
                        cx,
                    ))
                    .child(preset_section(
                        "Agent Presets",
                        "Coding agents, started in a shell like terminal presets but opening where agents go, so a group can be the default for agents apart from terminals. Pinned presets get their own button; click a preset's mark for another colour.",
                        self.render_presets(Kind::Agents, cx),
                        cx,
                    ))
                    .child(section_title("Browser", cx))
                    .child(setting_row("Home page", "The page a new browser tab opens.", div().w(px(260.)).child(Input::new(&self.browser_home).small()), cx))
                    .child(preset_section(
                        "Browser Presets",
                        "Pages a tab-strip button opens in a new browser tab: a dev server, docs. The plain browser has its own button.",
                        self.render_presets(Kind::Browsers, cx),
                        cx,
                    )),
            );
        let empty = FILTER.with(|filter| filter.borrow().2 == 0) && filtering();
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .gap_2()
            .child(div().flex_none().px_2().child(Input::new(&self.search).small()))
            .when(empty, |this| this.child(div().px_2().py_4().text_sm().text_color(cx.theme().muted_foreground).child("No settings match.")))
            .child(body)
    }
}

/// A sound picked from den's list in a menu, with a button to hear it.
fn sound_picker(id: &'static str, label: &'static str, current: String, set: fn(&mut Settings, String)) -> impl IntoElement {
    let name = crate::sound::SOUNDS.iter().find(|(s, ..)| *s == current).map_or("None", |(_, name, _)| *name);
    let hear = current.clone();
    h_flex()
        .gap_1()
        .items_center()
        .child(div().text_xs().child(label))
        .child(Button::new(id).small().outline().label(name).dropdown_menu(move |menu, _, _| {
            let mut menu = menu.item(PopupMenuItem::new("None").checked(current == "none").on_click(move |_, _, cx| Settings::update(cx, |s| set(s, "none".into()))));
            for (sound, name, _) in crate::sound::SOUNDS {
                menu = menu.item(PopupMenuItem::new(*name).checked(current == *sound).on_click(move |_, _, cx| {
                    Settings::update(cx, |s| set(s, sound.to_string()));
                    crate::sound::play_named(sound, Settings::get(cx).notify_volume);
                }));
            }
            menu
        }))
        .child(
            Button::new(SharedString::from(format!("{id}-play")))
                .small()
                .ghost()
                .icon(Icon::new(IconName::Play))
                .tooltip("Hear it")
                .on_click(move |_, _, cx| crate::sound::play_named(&hear, Settings::get(cx).notify_volume)),
        )
}

/// A tab-strip button switched on or off in Settings.
fn strip_toggle(id: &'static str, label: &'static str, on: bool, flip: fn(&mut crate::settings::GroupButtons)) -> Button {
    Button::new(id)
        .small()
        .label(label)
        .map(|b| if on { b.primary() } else { b.outline() })
        .on_click(move |_, _, cx| Settings::update(cx, |s| flip(&mut s.group_buttons)))
}

fn section_title(title: &'static str, cx: &App) -> Div {
    FILTER.with(|filter| filter.borrow_mut().1 = title);
    // While searching, rows show without their section titles.
    if filtering() {
        return div();
    }
    div()
        .pt_5()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(title.to_uppercase())
}

fn preset_section(title: &'static str, description: &'static str, list: Div, cx: &App) -> Div {
    if !passes(&[title, description]) {
        return v_flex();
    }
    v_flex()
        .w_full()
        .py_3()
        .gap_2()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(description))
        .child(list)
}
