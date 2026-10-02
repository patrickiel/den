//! Browser tabs, as in den: a real Chromium browser (WebView2, through wry)
//! inside a group. The toolbar has back / forward / reload and a URL bar (a
//! host opens as https, `localhost` as http, anything else is a web search);
//! links that open a new window open as a new browser tab. Tabs come back at
//! their last URL; logins persist in den's own browser profile.
//!
//! The page is a native child window painted over the app: it follows its
//! pane's bounds every frame it is drawn, and the workspace hides it while
//! its tab is not showing or a tab is being dragged. While the page has the
//! keyboard, keys go to the page; a click on the toolbar takes them back.

use std::{cell::Cell, path::PathBuf, rc::Rc};

use futures::{StreamExt as _, channel::mpsc};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde_json::{Value, json};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use wry::{
    NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize},
};

use crate::{
    pane::{Pane, PaneEvent},
    settings::Settings,
};

pub const BROWSER: &str = "Browser";

/// Turn what was typed in the URL bar into something to load: a URL, a host,
/// or a web search.
pub fn normalize_url(input: &str) -> String {
    let text = input.trim();
    if text.is_empty() {
        return String::new();
    }
    let lower = text.to_lowercase();
    // "localhost:3000" looks like a scheme too; only "x://" and the schemes
    // without slashes count.
    let scheme = lower.split_once("://").is_some_and(|(s, _)| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)));
    if scheme || ["about:", "data:", "mailto:", "javascript:"].iter().any(|p| lower.starts_with(p)) {
        return text.to_string();
    }
    let search = || format!("https://www.google.com/search?q={}", encode(text));
    if text.contains(char::is_whitespace) {
        return search();
    }
    let host = text.split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').filter(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit())).map_or(host, |(h, _)| h);
    let loopback = host.starts_with("127.") && host.split('.').count() == 4;
    if host == "localhost" || loopback {
        return format!("http://{text}");
    }
    if host.split('.').count() >= 2 && host.split('.').all(|part| !part.is_empty()) {
        return format!("https://{text}");
    }
    search()
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// What the page reports, from wry's handlers to the pane.
enum PageEvent {
    Title(String),
    Loading(String),
    Loaded(String),
    NewWindow(String),
}

pub struct BrowserPanel {
    focus_handle: FocusHandle,
    webview: Option<Rc<WebView>>,
    error: Option<SharedString>,
    url: String,
    title: String,
    loading: bool,
    url_input: Entity<InputState>,
    /// Where the page sits in the window, from the last paint.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Shown by the workspace; hidden while another tab shows or a drag runs.
    shown: Rc<Cell<bool>>,
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// The window the page is a child of, by its native handle: the webview is
/// built outside GPUI's update (see `BrowserPanel::new`), where the `Window`
/// itself is not at hand.
struct Parent(RawWindowHandle);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: the handle is the GPUI window's, which outlives its panes.
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

/// den's browser profile: logins and cookies stay between sessions.
fn profile_dir() -> PathBuf {
    crate::settings::data_dir().join("webview")
}

impl BrowserPanel {
    pub fn new(url: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = url.filter(|u| !u.trim().is_empty()).unwrap_or_else(|| Settings::get(cx).browser_home.clone());
        let url = normalize_url(&url);
        let url_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search or type a URL").default_value(url.clone()));
        let (tx, mut rx) = mpsc::unbounded::<PageEvent>();
        let (title_tx, load_tx, window_tx) = (tx.clone(), tx.clone(), tx);
        // WebView2 is created with a nested message loop (wry waits for its
        // controller), which runs GPUI's queued tasks too. Inside an update
        // (here) those would find the app borrowed and panic, so the page is
        // built from a task of its own, outside any update, with the window's
        // native handle taken now.
        let parent = HasWindowHandle::window_handle(window).map(|handle| handle.as_raw());
        let start_url = url.clone();
        let _events = cx.spawn(async move |this, cx| {
            let built = match parent {
                Ok(raw) => {
                    let mut context = WebContext::new(Some(profile_dir()));
                    WebViewBuilder::new_with_web_context(&mut context)
                        .with_url(&start_url)
                        .with_bounds(Rect { position: LogicalPosition::new(0, 0).into(), size: LogicalSize::new(1, 1).into() })
                        .with_visible(false)
                        .with_focused(false)
                        .with_document_title_changed_handler(move |title| _ = title_tx.unbounded_send(PageEvent::Title(title)))
                        .with_on_page_load_handler(move |event, url| {
                            let event = match event {
                                PageLoadEvent::Started => PageEvent::Loading(url),
                                PageLoadEvent::Finished => PageEvent::Loaded(url),
                            };
                            _ = load_tx.unbounded_send(event);
                        })
                        .with_new_window_req_handler(move |url, _| {
                            _ = window_tx.unbounded_send(PageEvent::NewWindow(url));
                            NewWindowResponse::Deny
                        })
                        .build_as_child(&Parent(raw))
                        .map_err(|err| err.to_string())
                }
                Err(err) => Err(err.to_string()),
            };
            let alive = this.update(cx, |this, cx| {
                match built {
                    Ok(webview) => this.webview = Some(Rc::new(webview)),
                    Err(err) => this.error = Some(format!("Cannot start the browser (WebView2): {err}").into()),
                }
                cx.notify();
            });
            if alive.is_err() {
                return;
            }
            while let Some(event) = rx.next().await {
                if this.update(cx, |this, cx| this.page_event(event, cx)).is_err() {
                    break;
                }
            }
        });
        let _subscriptions = vec![cx.subscribe_in(&url_input, window, |this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let text = input.read(cx).value().to_string();
                this.navigate(&text, window, cx);
            }
        })];
        Self {
            focus_handle: cx.focus_handle(),
            webview: None,
            error: None,
            title: String::new(),
            loading: true,
            url,
            url_input,
            bounds: Rc::default(),
            shown: Rc::new(Cell::new(false)),
            _events,
            _subscriptions,
        }
    }

    fn page_event(&mut self, event: PageEvent, cx: &mut Context<Self>) {
        match event {
            PageEvent::Title(title) => self.title = title,
            PageEvent::Loading(url) => {
                self.loading = true;
                self.url = url;
            }
            PageEvent::Loaded(url) => {
                self.loading = false;
                self.url = url;
            }
            PageEvent::NewWindow(url) => cx.emit(PaneEvent::OpenBrowser(url)),
        }
        // The URL bar follows the page, unless the user is typing in it.
        let url = self.url.clone();
        let input = self.url_input.clone();
        cx.defer(move |cx| {
            let Some(window) = cx.active_window() else { return };
            _ = window.update(cx, |_, window, cx| {
                if !input.read(cx).focus_handle(cx).is_focused(window) {
                    input.update(cx, |input, cx| input.set_value(url, window, cx));
                }
            });
        });
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn navigate(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = normalize_url(text);
        if url.is_empty() {
            return;
        }
        if let Some(webview) = &self.webview {
            _ = webview.load_url(&url);
            _ = webview.focus();
        }
        self.url = url;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn script(&self, js: &str) {
        if let Some(webview) = &self.webview {
            _ = webview.evaluate_script(js);
        }
    }

    /// The workspace shows or hides the page (its tab showing, no drag).
    pub fn set_shown(&self, shown: bool) {
        if self.shown.replace(shown) != shown
            && let Some(webview) = &self.webview
        {
            _ = webview.set_visible(shown && self.bounds.get().is_some());
        }
    }


    /// Take the keyboard back from the page.
    fn take_focus(&self) {
        if let Some(webview) = &self.webview {
            _ = webview.focus_parent();
        }
    }
}

impl EventEmitter<PaneEvent> for BrowserPanel {}

impl Focusable for BrowserPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Pane for BrowserPanel {
    fn kind(&self) -> &'static str {
        BROWSER
    }

    fn icon(&self, _: &App) -> IconName {
        IconName::Globe
    }

    fn label(&self, _: &App) -> SharedString {
        let title = self.title.trim();
        if !title.is_empty() {
            return title.chars().take(40).collect::<String>().into();
        }
        let host = self.url.split("://").nth(1).unwrap_or(&self.url).split('/').next().unwrap_or("");
        if host.is_empty() { "Browser".into() } else { host.to_string().into() }
    }

    fn dump(&self, _: &App) -> Value {
        json!({ "url": self.url })
    }
}

impl Render for BrowserPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tool = |id: &'static str, icon: IconName, tip: &'static str| Button::new(id).ghost().small().icon(Icon::new(icon)).tooltip(tip);
        let (bounds, shown, webview) = (self.bounds.clone(), self.shown.clone(), self.webview.clone());
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .px_1()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.take_focus()))
                    .child(tool("back", IconName::ArrowLeft, "Back").on_click(cx.listener(|this, _, _, _| this.script("history.back()"))))
                    .child(tool("forward", IconName::ArrowRight, "Forward").on_click(cx.listener(|this, _, _, _| this.script("history.forward()"))))
                    .child(
                        tool("reload", if self.loading { IconName::X } else { IconName::RotateCw }, if self.loading { "Stop" } else { "Reload" })
                            .on_click(cx.listener(|this, _, _, _| this.script(if this.loading { "window.stop()" } else { "location.reload()" }))),
                    )
                    .child(div().flex_1().min_w_0().child(Input::new(&self.url_input).small())),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .when_some(self.error.clone(), |this, error| this.child(div().p_4().text_color(theme.danger).child(error)))
                    .child(
                        // Puts the native page where this area is drawn.
                        canvas(
                            move |area, _, _| area,
                            move |_, area, _, _| {
                                let Some(webview) = &webview else { return };
                                if bounds.get() != Some(area) {
                                    bounds.set(Some(area));
                                    _ = webview.set_bounds(Rect {
                                        position: LogicalPosition::new(area.left().as_f32(), area.top().as_f32()).into(),
                                        size: LogicalSize::new(area.size.width.as_f32().max(1.), area.size.height.as_f32().max(1.)).into(),
                                    });
                                }
                                if shown.get() {
                                    _ = webview.set_visible(true);
                                }
                            },
                        )
                        .size_full(),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_url;

    #[test]
    fn urls_hosts_and_searches() {
        assert_eq!(normalize_url("https://x.dev/a"), "https://x.dev/a");
        assert_eq!(normalize_url("localhost:5173"), "http://localhost:5173");
        assert_eq!(normalize_url("127.0.0.1:8080/x"), "http://127.0.0.1:8080/x");
        assert_eq!(normalize_url("example.com/docs"), "https://example.com/docs");
        assert_eq!(normalize_url("rust gpui"), "https://www.google.com/search?q=rust%20gpui");
        assert_eq!(normalize_url("about:blank"), "about:blank");
        assert_eq!(normalize_url("  "), "");
    }
}
