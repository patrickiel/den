//! A floating window: a group or a container moved out of the main window,
//! as VS Code's auxiliary windows. It draws its part of the workspace's tree
//! (the panes stay the workspace's and keep running) under a title bar of its
//! own, without a sidebar. Closing it moves what it holds back into the main
//! window; closing the main window closes it, and it opens again with the
//! session.

use gpui_kit::*;

use crate::workspace::Workspace;

pub struct FloatWindow {
    workspace: WeakEntity<Workspace>,
    id: u64,
    _subscriptions: Vec<Subscription>,
}

impl FloatWindow {
    pub fn new(workspace: Entity<Workspace>, id: u64, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let weak = workspace.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let from = crate::browser::hwnd(window);
            _ = weak.update(cx, |workspace, cx| {
                workspace.dock_float(id, cx);
                if let Some(from) = from {
                    workspace.rehome_browsers(from, cx);
                }
            });
            true
        });
        let _subscriptions = vec![
            cx.observe_window_bounds(window, move |this, window, cx| {
                let bounds = window.bounds();
                _ = this.workspace.update(cx, |workspace, cx| workspace.float_moved(id, bounds, cx));
            }),
            cx.observe_window_activation(window, move |this, window, cx| {
                if window.is_window_active() {
                    _ = this.workspace.update(cx, |workspace, cx| workspace.window_activated(Some(id), cx));
                }
            }),
            // The main window closed, or turned to another folder.
            cx.observe_release_in(&workspace, window, |_, _, window, _| window.remove_window()),
        ];
        // What moved here takes the keyboard.
        let focus = workspace.read(cx).window_focus(Some(id), cx);
        if let Some(focus) = focus {
            window.defer(cx, move |window, cx| focus.focus(window, cx));
        }
        Self {
            workspace: workspace.downgrade(),
            id,
            _subscriptions,
        }
    }
}

impl Render for FloatWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        match self.workspace.upgrade() {
            Some(workspace) => workspace.update(cx, |workspace, cx| workspace.render_float(id, window, cx)),
            None => Empty.into_any_element(),
        }
    }
}
