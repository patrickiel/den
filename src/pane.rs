//! What a tab is: a pane of some kind (file, terminal, settings).
//!
//! A kind is a name, a builder that makes the pane again from what it saved,
//! and the `Pane` trait: its title, what it saves, which kind of tab it counts
//! as for default groups. The workspace only knows panes through `PaneRef`.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workspace::Workspace;

/// What a pane tells the workspace.
pub enum PaneEvent {
    /// Its title, icon or dirty mark changed: redraw the tab strip.
    Changed,
    /// Open a file (a link clicked in a terminal), at a position if given.
    OpenFile {
        path: std::path::PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// Open a URL in a new browser tab (a page's link to a new window).
    OpenBrowser(String),
    /// The program in it wants the user.
    Alert { kind: AlertKind, message: Option<String> },
}

/// Why a terminal wants the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    /// An agent finished its turn.
    Done,
    /// An agent waits for an answer or a permission.
    Input,
    /// A bell or a notification sequence.
    Attention,
}

pub trait Pane: Render + Focusable + EventEmitter<PaneEvent> {
    /// The kind's name, as saved.
    fn kind(&self) -> &'static str;

    fn icon(&self, _cx: &App) -> IconName;

    /// A richer icon for the tab (a file's type icon); the `icon` otherwise.
    fn icon_element(&self, _cx: &App) -> Option<AnyElement> {
        None
    }

    /// The tab's label.
    fn label(&self, cx: &App) -> SharedString;

    /// Unsaved changes, marked with a dot on the tab.
    fn is_dirty(&self, _cx: &App) -> bool {
        false
    }

    /// What the kind's builder needs to make it again.
    fn dump(&self, cx: &App) -> Value;
}

/// The object-safe face of a pane entity.
pub trait PaneHandle {
    fn view(&self) -> AnyView;
    fn kind(&self, cx: &App) -> &'static str;
    fn icon(&self, cx: &App) -> IconName;
    fn icon_element(&self, cx: &App) -> Option<AnyElement>;
    fn label(&self, cx: &App) -> SharedString;
    fn is_dirty(&self, cx: &App) -> bool;
    fn dump(&self, cx: &App) -> Value;
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    fn subscribe(&self, id: crate::layout::PaneId, window: &mut Window, cx: &mut Context<Workspace>) -> Subscription;
}

impl<T: Pane> PaneHandle for Entity<T> {
    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn kind(&self, cx: &App) -> &'static str {
        self.read(cx).kind()
    }

    fn icon(&self, cx: &App) -> IconName {
        self.read(cx).icon(cx)
    }

    fn icon_element(&self, cx: &App) -> Option<AnyElement> {
        self.read(cx).icon_element(cx)
    }

    fn label(&self, cx: &App) -> SharedString {
        self.read(cx).label(cx)
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.read(cx).is_dirty(cx)
    }

    fn dump(&self, cx: &App) -> Value {
        self.read(cx).dump(cx)
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        Focusable::focus_handle(self.read(cx), cx)
    }

    fn subscribe(&self, id: crate::layout::PaneId, window: &mut Window, cx: &mut Context<Workspace>) -> Subscription {
        cx.subscribe_in(self, window, move |this, _, event: &PaneEvent, window, cx| match event {
            PaneEvent::Changed => this.pane_changed(id, cx),
            PaneEvent::OpenFile { path, line, column } => this.open_file_at(path.clone(), *line, *column, window, cx),
            PaneEvent::OpenBrowser(url) => this.open_browser(None, Some(url.clone()), window, cx),
            PaneEvent::Alert { kind, message } => this.pane_alert(id, *kind, message.clone(), window, cx),
        })
    }
}

pub type PaneRef = Rc<dyn PaneHandle>;

/// A pane as saved: its kind and what it dumped.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneState {
    pub kind: String,
    #[serde(default)]
    pub data: Value,
}

type Builder = Rc<dyn Fn(&Value, &mut Window, &mut App) -> PaneRef>;

#[derive(Default)]
struct Registry(HashMap<&'static str, Builder>);
impl Global for Registry {}

/// Make panes of `kind` buildable from what they saved.
pub fn register(cx: &mut App, kind: &'static str, build: impl Fn(&Value, &mut Window, &mut App) -> PaneRef + 'static) {
    cx.default_global::<Registry>().0.insert(kind, Rc::new(build));
}

/// Make a saved pane again; a kind this build does not have becomes a
/// placeholder that keeps the saved state for the next save.
pub fn build(state: &PaneState, window: &mut Window, cx: &mut App) -> PaneRef {
    let builder = cx
        .try_global::<Registry>()
        .and_then(|registry| registry.0.get(state.kind.as_str()).cloned());
    match builder {
        Some(build) => build(&state.data, window, cx),
        None => {
            let state = state.clone();
            Rc::new(cx.new(|cx| UnknownPane {
                state,
                focus_handle: cx.focus_handle(),
            }))
        }
    }
}

pub fn dump(pane: &PaneRef, cx: &App) -> PaneState {
    match pane.view().downcast::<UnknownPane>() {
        Ok(unknown) => unknown.read(cx).state.clone(),
        Err(_) => PaneState {
            kind: pane.kind(cx).to_string(),
            data: pane.dump(cx),
        },
    }
}

/// Stands in for a pane kind this build does not know.
pub struct UnknownPane {
    state: PaneState,
    focus_handle: FocusHandle,
}

impl EventEmitter<PaneEvent> for UnknownPane {}

impl Focusable for UnknownPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Pane for UnknownPane {
    fn kind(&self) -> &'static str {
        "Unknown"
    }

    fn icon(&self, _: &App) -> IconName {
        IconName::CircleQuestionMark
    }

    fn label(&self, _: &App) -> SharedString {
        format!("Unknown: {}", self.state.kind).into()
    }

    fn dump(&self, _: &App) -> Value {
        self.state.data.clone()
    }
}

impl Render for UnknownPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(
                h_flex()
                    .gap_1()
                    .child(Icon::new(IconName::CircleQuestionMark).small())
                    .child(format!("Unknown pane kind \"{}\"", self.state.kind)),
            )
            .child("Its saved state is kept for a build that knows it.")
    }
}
