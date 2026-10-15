//! Downloads started from the views (formatters, AI models, extensions):
//! each runs on a thread of its own with its progress shown where it was
//! started, and a failure becomes a toast.

use gpui_kit::*;

/// The key under which an extension's install shows its progress.
pub(crate) fn install_progress_key(id: &str) -> String {
    format!("ext-install-{id}")
}

/// The downloads running, by key, with their progress.
static DOWNLOADS: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn download_progress(key: &str) -> Option<String> {
    DOWNLOADS
        .lock()
        .ok()?
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, progress)| progress.clone())
}

fn set_download_progress(key: &str, progress: Option<String>) {
    if let Ok(mut downloads) = DOWNLOADS.lock() {
        downloads.retain(|(k, _)| k != key);
        if let Some(progress) = progress {
            downloads.push((key.to_string(), progress));
        }
    }
}

/// Run `job` (a download) on its own thread, its progress shown in the menu
/// entry `key` meanwhile; a failure becomes a toast.
pub(crate) fn start_download(
    key: String,
    what: impl Into<String>,
    window: &mut Window,
    cx: &mut App,
    job: impl FnOnce(
        &crate::backend::http::Cancel,
        crate::backend::http::Progress,
    ) -> Result<(), String>
    + Send
    + 'static,
) {
    start_download_then(key, what, window, cx, job, |_, _, _| {});
}

/// [`start_download`], then `done` with what `job` made, or `None` once it
/// failed (after the toast). Nothing happens while `key` is downloading.
pub(crate) fn start_download_then<T: Send + 'static>(
    key: String,
    what: impl Into<String>,
    window: &mut Window,
    cx: &mut App,
    job: impl FnOnce(&crate::backend::http::Cancel, crate::backend::http::Progress) -> Result<T, String>
    + Send
    + 'static,
    done: impl FnOnce(Option<T>, &mut Window, &mut App) + 'static,
) {
    if download_progress(&key).is_some() {
        return;
    }
    set_download_progress(&key, Some("Starting…".into()));
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Option<Result<T, String>>>();
    std::thread::spawn(move || {
        let result = job(
            &crate::backend::http::Cancel::default(),
            &mut |stage, done, total| {
                let progress = if total == 0 {
                    format!("{stage}…")
                } else {
                    format!("{}%", done * 100 / total)
                };
                set_download_progress(&key, Some(progress));
                _ = tx.unbounded_send(None);
            },
        );
        set_download_progress(&key, None);
        _ = tx.unbounded_send(Some(result));
    });
    let what = what.into();
    let handle = window.window_handle();
    let mut done = Some(done);
    cx.spawn(async move |cx| {
        use futures::StreamExt as _;
        while let Some(update) = rx.next().await {
            _ = handle.update(cx, |_, window, cx| {
                if let Some(result) = update {
                    let made = result
                        .map_err(|err| {
                            crate::toast::push(
                                window,
                                format!("Could not download {what}: {err}"),
                                cx,
                            )
                        })
                        .ok();
                    if let Some(done) = done.take() {
                        done(made, window, cx);
                    }
                }
                window.refresh();
            });
        }
    })
    .detach();
}

/// Fetch `repo`'s latest release, ask, and download it for the next start.
/// `current` is the installed version when this is an update.
pub(crate) fn get_extension(
    repo: String,
    current: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::component::WindowExt as _;
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let fetch = repo.clone();
        let result = cx.background_spawn(async move { crate::backend::extensions::fetch_manifest(&fetch) }).await;
        _ = handle.update(cx, |_, window, cx| {
            let manifest = match result.and_then(|m| m.check_api().map(|()| m)) {
                Ok(manifest) => manifest,
                Err(err) => return crate::toast::push(window, err, cx),
            };
            if let Some(current) = &current
                && !crate::update::newer(&manifest.version, current)
            {
                return crate::toast::push(window, format!("{} {current} is the newest version.", manifest.name), cx);
            }
            let (title, ok) = if current.is_some() { ("Update Extension", "Update") } else { ("Install Extension", "Install") };
            let about = if manifest.description.is_empty() { String::new() } else { format!("{}\n\n", manifest.description) };
            let text = format!(
                "{} {} from github.com/{repo}.\n\n{about}An extension is native code: it runs inside den with your permissions, so install only what you trust. It loads at the next start.",
                manifest.name, manifest.version
            );
            window.open_alert_dialog(cx, move |dialog, _, _| {
                let manifest = manifest.clone();
                dialog.title(title).description(text.clone()).ok_text(ok).show_cancel(true).on_ok(move |_, window, cx| {
                    let (job, done) = (manifest.clone(), manifest.clone());
                    crate::toast::push(window, format!("Downloading {}…", manifest.name), cx);
                    start_download_then(
                        install_progress_key(&manifest.id),
                        manifest.name.clone(),
                        window,
                        cx,
                        move |cancel, progress| crate::backend::extensions::install(&job, cancel, progress),
                        move |made, _, cx| {
                            if made.is_some() {
                                crate::extensions::installed(done, cx)
                            }
                        },
                    );
                    true
                })
            });
        });
    })
    .detach();
}

/// Download the newer versions in `updates` for the next start, together,
/// each with its progress on its card; once all are done, offer to restart.
/// The user chose these already, so there is no dialog for each.
pub(crate) fn update_extensions(
    mut updates: Vec<crate::backend::extensions::Listing>,
    window: &mut Window,
    cx: &mut App,
) {
    use std::{cell::Cell, rc::Rc};
    // One downloading already would never report back here.
    updates.retain(|l| download_progress(&install_progress_key(&l.id)).is_none());
    let left = Rc::new(Cell::new(updates.len()));
    let updated = Rc::new(Cell::new(0));
    for listing in updates {
        let (left, updated) = (left.clone(), updated.clone());
        start_download_then(
            install_progress_key(&listing.id),
            listing.name.clone(),
            window,
            cx,
            move |cancel, progress| {
                let manifest = crate::backend::extensions::fetch_manifest(&listing.repo)?;
                crate::backend::extensions::install(&manifest, cancel, progress)?;
                Ok(manifest)
            },
            move |made, window, cx| {
                if let Some(manifest) = made {
                    updated.set(updated.get() + 1);
                    crate::extensions::installed(manifest, cx);
                }
                left.set(left.get() - 1);
                if left.get() == 0 && updated.get() > 0 {
                    offer_restart(updated.get(), window, cx);
                }
            },
        );
    }
}

/// Updated extensions load at the next start: offer it now.
fn offer_restart(updated: usize, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::button::{Button, ButtonVariants as _};
    use gpui_kit::component::notification::Notification;
    let what = if updated == 1 {
        "The update loads".to_string()
    } else {
        format!("The {updated} updates load")
    };
    let note = Notification::new()
        .title("Extensions Updated")
        .message(format!("{what} when den restarts."))
        .action(|_, _, _| {
            Button::new("ext-restart")
                .primary()
                .label("Restart")
                .on_click(|_, _, cx| crate::restart(cx))
        });
    crate::toast::push(window, note, cx);
}
