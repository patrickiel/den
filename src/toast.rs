//! Toasts, through one door, so how den shows them can change in one place.

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

/// Show a toast in `window`.
pub fn push(window: &mut Window, note: impl Into<Notification>, cx: &mut App) {
    window.push_notification(note, cx);
}
