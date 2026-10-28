//! The arrangement as den keeps it: groups, splits, sizes, default groups and
//! tabs live in den's session file, one per folder, nothing in the folder
//! itself.
//!
//! A layout preset is shareable across projects: paths inside the folder are
//! stored relative to it, and it keeps no machine state (a terminal's
//! scrollback, where a floating window sits) and no agent conversation,
//! so its agent tabs start anew.
//!
//! Every terminal, Claude Code's too, starts in the folder or a folder inside
//! it: one saved elsewhere (a preset from another project) or whose folder
//! is gone starts in the folder itself.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Keys of a pane's data that are this machine's, not the arrangement's.
const MACHINE_KEYS: &[&str] = &["scrollback"];
/// Keys of a pane's data that hold paths.
const PATH_KEYS: &[&str] = &["path", "cwd", "top"];

/// The layout as a preset keeps it: no machine state, paths relative.
pub fn shareable(layout: &Value, root: &Path) -> Value {
    let mut layout = layout.clone();
    if let Some(panes) = layout.get_mut("panes").and_then(Value::as_object_mut) {
        for pane in panes.values_mut() {
            let Some(data) = pane.get_mut("data").and_then(Value::as_object_mut) else {
                continue;
            };
            for key in MACHINE_KEYS {
                data.remove(*key);
            }
            clear_conversation(data);
            for key in PATH_KEYS {
                if let Some(text) = data.get(*key).and_then(Value::as_str)
                    && let Some(rel) = relative(Path::new(text), root)
                {
                    data.insert((*key).into(), Value::String(rel));
                }
            }
        }
    }
    if let Some(floats) = layout.get_mut("floats").and_then(Value::as_array_mut) {
        for float in floats.iter_mut().filter_map(Value::as_object_mut) {
            float.remove("bounds");
        }
    }
    layout
}

/// A layout copied into another project keeps an agent's flags but never
/// the conversation from its original project, including explicit resumes.
fn clear_conversation(data: &mut serde_json::Map<String, Value>) {
    let fresh = ["program", "resume"]
        .iter()
        .find_map(|key| crate::backend::agent::resume(data.get(*key)?.as_str()?, None, true));
    if let Some(fresh) = fresh {
        data.insert("program".into(), Value::String(fresh.clone()));
        data.insert("resume".into(), Value::String(fresh));
    }
}

/// Start terminal `data` in `root` when its folder is outside `root` or gone:
/// the folder's terminals run in the folder. An agent conversation from
/// elsewhere is not found here, so such a tab starts anew.
fn keep_inside(data: &mut serde_json::Map<String, Value>, root: &Path) {
    let Some(cwd) = data.get("cwd").and_then(Value::as_str).map(PathBuf::from) else {
        return;
    };
    let outside = relative(&cwd, root).is_none();
    if outside || !cwd.is_dir() {
        data.insert(
            "cwd".into(),
            Value::String(root.to_string_lossy().into_owned()),
        );
    }
    if outside {
        clear_conversation(data);
    }
}

/// `layout` with relative paths made absolute in `root` and every terminal
/// started inside `root`.
pub fn restore(layout: Value, root: &Path) -> Value {
    let mut layout = layout;
    if let Some(panes) = layout.get_mut("panes").and_then(Value::as_object_mut) {
        for pane in panes.values_mut() {
            let kind = pane["kind"].clone();
            let Some(data) = pane.get_mut("data").and_then(Value::as_object_mut) else {
                continue;
            };
            for key in PATH_KEYS {
                if let Some(text) = data.get(*key).and_then(Value::as_str) {
                    let path = Path::new(text);
                    if path.is_relative() {
                        let absolute = root.join(text.replace('/', std::path::MAIN_SEPARATOR_STR));
                        data.insert(
                            (*key).into(),
                            Value::String(absolute.to_string_lossy().into_owned()),
                        );
                    }
                }
            }
            if kind == crate::terminal::TERMINAL {
                keep_inside(data, root);
            }
        }
    }
    layout
}

/// A layout preset, saved in another project, made fit
/// for this one: no conversations or machine state, paths inside `root`.
pub fn preset(layout: &Value, root: &Path) -> Value {
    restore(shareable(layout, root), root)
}

/// `path` relative to `root` with forward slashes, when it is inside it.
fn relative(path: &Path, root: &Path) -> Option<String> {
    // Linux tells paths apart by case; Windows and macOS do not.
    let key = |p: &Path| {
        let key = p.to_string_lossy().replace('\\', "/");
        let key = key.trim_end_matches('/');
        if cfg!(target_os = "linux") {
            key.to_string()
        } else {
            key.to_lowercase()
        }
    };
    let (full, base) = (key(path), key(root));
    if full == base {
        return Some(".".into());
    }
    let original = path.to_string_lossy().replace('\\', "/");
    full.strip_prefix(&format!("{base}/"))
        .map(|_| original[base.len() + 1..].to_string())
}

#[cfg(test)]
mod tests {
    use super::{preset, relative, restore, shareable};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn paths_inside_the_folder_become_relative() {
        let root = Path::new(r"C:\repos\den");
        assert_eq!(
            relative(Path::new(r"C:\repos\den\src\main.rs"), root).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            relative(Path::new(r"C:\Repos\Den"), root).as_deref(),
            Some(".")
        );
        assert_eq!(relative(Path::new(r"C:\repos\den2\x"), root), None);
    }

    /// A real folder with a `src` folder in it: this crate's.
    fn crate_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn presets_keep_no_machine_state() {
        let root = crate_root();
        let src = root.join("src").to_string_lossy().into_owned();
        let full = json!({
            "root": {},
            "panes": {
                "1": { "kind": "Terminal", "data": { "cwd": src, "scrollback": "old output" } },
                "2": { "kind": "File", "data": { "path": r"D:\elsewhere\a.txt" } }
            },
            "floats": [{ "id": 9, "root": {}, "bounds": [10.0, 20.0, 800.0, 600.0] }]
        });
        let shared = shareable(&full, root);
        assert_eq!(shared["panes"]["1"]["data"], json!({ "cwd": "src" }));
        assert_eq!(shared["panes"]["2"]["data"]["path"], r"D:\elsewhere\a.txt");
        assert!(shared["floats"][0].get("bounds").is_none());
        let back = restore(shared, root);
        assert_eq!(back["panes"]["1"]["data"]["cwd"], src);
    }

    #[test]
    fn terminals_start_inside_the_folder() {
        let root = crate_root();
        let here = root.to_string_lossy().into_owned();
        let elsewhere = root.parent().unwrap().to_string_lossy().into_owned();
        let layout = json!({
            "root": {},
            "panes": {
                "1": { "kind": "Terminal", "data": { "cwd": elsewhere, "program": "claude", "resume": "claude --resume abc-123" } },
                "2": { "kind": "Terminal", "data": { "cwd": elsewhere, "program": null, "resume": null } },
                "3": { "kind": "Terminal", "data": { "cwd": root.join("gone").to_string_lossy(), "program": null } }
            }
        });
        for back in [restore(layout.clone(), root), preset(&layout, root)] {
            for id in ["1", "2", "3"] {
                assert_eq!(back["panes"][id]["data"]["cwd"], here);
            }
            // Its conversation lives with the other folder: start anew.
            assert_eq!(back["panes"]["1"]["data"]["resume"], "claude");
            assert_eq!(back["panes"]["2"]["data"]["resume"], json!(null));
        }
    }

    #[test]
    fn restored_tabs_resume_their_conversation() {
        let root = crate_root();
        let here = root.to_string_lossy().into_owned();
        let full = json!({
            "root": {},
            "panes": {
                "1": { "kind": "Terminal", "data": { "cwd": here, "program": "claude --resume abc-123", "resume": "claude --resume abc-123" } }
            }
        });
        assert_eq!(
            restore(full.clone(), root)["panes"]["1"]["data"]["resume"],
            "claude --resume abc-123"
        );
        // A preset has no conversation behind it: it starts anew.
        assert_eq!(
            shareable(&full, root)["panes"]["1"]["data"],
            json!({ "cwd": ".", "program": "claude", "resume": "claude" })
        );
        assert_eq!(
            preset(&full, root)["panes"]["1"]["data"]["resume"],
            "claude"
        );
    }

    #[test]
    fn codex_tabs_keep_separate_sessions_and_presets_start_fresh() {
        let root = crate_root();
        let full = json!({ "panes": {
            "1": { "kind": "Terminal", "data": { "cwd": root, "program": "codex --model test", "resume": "codex --model test resume first-id" } },
            "2": { "kind": "Terminal", "data": { "cwd": root, "program": "codex resume old-id", "resume": "codex resume second-id" } },
            "3": { "kind": "Terminal", "data": { "cwd": root, "program": null, "resume": "codex resume typed-id" } },
            "4": { "kind": "Terminal", "data": { "cwd": root, "program": "codex resume explicit-id", "resume": null } }
        }});
        let restored = restore(full.clone(), root);
        assert_eq!(
            restored["panes"]["1"]["data"]["resume"],
            "codex --model test resume first-id"
        );
        assert_eq!(
            restored["panes"]["2"]["data"]["resume"],
            "codex resume second-id"
        );
        assert_eq!(
            restored["panes"]["3"]["data"]["resume"],
            "codex resume typed-id"
        );
        let shared = shareable(&full, root);
        assert_eq!(shared["panes"]["1"]["data"]["resume"], "codex --model test");
        for id in ["2", "3", "4"] {
            assert_eq!(shared["panes"][id]["data"]["program"], "codex");
            assert_eq!(shared["panes"][id]["data"]["resume"], "codex");
        }
        let elsewhere = root.parent().unwrap();
        let moved = restore(full, elsewhere);
        assert_eq!(
            moved["panes"]["1"]["data"]["resume"],
            "codex --model test resume first-id"
        );
        // A tab saved outside this workspace must not bring its old conversation here.
        let outside = json!({ "panes": { "1": { "kind": "Terminal", "data": {
            "cwd": elsewhere, "program": "codex resume other-project", "resume": "codex resume other-project"
        }}}});
        let moved = restore(outside, root);
        assert_eq!(
            moved["panes"]["1"]["data"]["cwd"],
            root.to_string_lossy().as_ref()
        );
        assert_eq!(moved["panes"]["1"]["data"]["program"], "codex");
        assert_eq!(moved["panes"]["1"]["data"]["resume"], "codex");
    }

    #[test]
    fn missing_keys_stay_missing() {
        let old = json!({ "root": {}, "panes": { "1": { "kind": "Settings" } } });
        assert_eq!(preset(&old, Path::new("x")), old);
    }
}
