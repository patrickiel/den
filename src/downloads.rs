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
    DOWNLOADS.lock().ok()?.iter().find(|(k, _)| k == key).map(|(_, progress)| progress.clone())
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
    job: impl FnOnce(&crate::backend::http::Cancel, crate::backend::http::Progress) -> Result<(), String> + Send + 'static,
) {
    start_download_then(key, what, window, cx, job, |_, _| {});
}

/// [`start_download`], then `done` once it succeeded.
pub(crate) fn start_download_then(
    key: String,
    what: impl Into<String>,
    window: &mut Window,
    cx: &mut App,
    job: impl FnOnce(&crate::backend::http::Cancel, crate::backend::http::Progress) -> Result<(), String> + Send + 'static,
    done: impl FnOnce(&mut Window, &mut App) + 'static,
) {
    if download_progress(&key).is_some() {
        return;
    }
    set_download_progress(&key, Some("Starting…".into()));
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Option<Result<(), String>>>();
    std::thread::spawn(move || {
        let result = job(&crate::backend::http::Cancel::default(), &mut |stage, done, total| {
            let progress = if total == 0 { format!("{stage}…") } else { format!("{}%", done * 100 / total) };
            set_download_progress(&key, Some(progress));
            _ = tx.unbounded_send(None);
        });
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
                match update {
                    Some(Err(err)) => crate::toast::push(window, format!("Could not download {what}: {err}"), cx),
                    Some(Ok(())) => {
                        if let Some(done) = done.take() {
                            done(window, cx);
                        }
                    }
                    None => {}
                }
                window.refresh();
            });
        }
    })
    .detach();
}

/// Fetch `repo`'s latest release, ask, and download it for the next start.
/// `current` is the installed version when this is an update.
pub(crate) fn get_extension(repo: String, current: Option<String>, window: &mut Window, cx: &mut App) {
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
                        move |_, cx| crate::extensions::installed(done, cx),
                    );
                    true
                })
            });
        });
    })
    .detach();
}
