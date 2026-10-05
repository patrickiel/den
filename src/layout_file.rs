//! The arrangement kept in the session folder itself, as den does: groups,
//! containers, sizes, default groups and tabs go to `.den/layout.json`
//! whenever they change, and come back from there first, so the layout goes
//! with the folder. (The Tauri den wrote another format to the same file; it
//! does not load, and is replaced on the next change.)
//!
//! Paths inside the folder are stored relative to it. Each tab's machine
//! state (a terminal's scrollback) stays in den's session file and is merged
//! back by tab id, as does where each floating window sits on this machine's
//! screens (by the float's id).

use std::path::{Path, PathBuf};

use serde_json::Value;

pub const FILE: &str = ".den/layout.json";

/// Keys of a pane's data that are this machine's, not the arrangement's.
const MACHINE_KEYS: &[&str] = &["scrollback"];
/// Keys of a pane's data that hold paths.
const PATH_KEYS: &[&str] = &["path", "cwd", "top"];

pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// The layout as the folder keeps it: no machine state, paths relative.
pub fn shareable(layout: &Value, root: &Path) -> Value {
    let mut layout = layout.clone();
    if let Some(panes) = layout["panes"].as_object_mut() {
        for pane in panes.values_mut() {
            let Some(data) = pane["data"].as_object_mut() else { continue };
            for key in MACHINE_KEYS {
                data.remove(*key);
            }
            for key in PATH_KEYS {
                if let Some(text) = data.get(*key).and_then(Value::as_str)
                    && let Some(rel) = relative(Path::new(text), root)
                {
                    data.insert((*key).into(), Value::String(rel));
                }
            }
        }
    }
    if let Some(floats) = layout["floats"].as_array_mut() {
        for float in floats.iter_mut().filter_map(Value::as_object_mut) {
            float.remove("bounds");
        }
    }
    layout
}

/// `layout` from the folder's file with paths made absolute again and the
/// machine state of the same tabs from `machine` (the session file's layout).
pub fn restore(layout: Value, root: &Path, machine: Option<&Value>) -> Value {
    let mut layout = layout;
    if let Some(panes) = layout["panes"].as_object_mut() {
        for (id, pane) in panes.iter_mut() {
            let kind = pane["kind"].clone();
            let Some(data) = pane["data"].as_object_mut() else { continue };
            for key in PATH_KEYS {
                if let Some(text) = data.get(*key).and_then(Value::as_str) {
                    let path = Path::new(text);
                    if path.is_relative() {
                        let absolute = root.join(text.replace('/', std::path::MAIN_SEPARATOR_STR));
                        data.insert((*key).into(), Value::String(absolute.to_string_lossy().into_owned()));
                    }
                }
            }
            let saved = machine.and_then(|m| m["panes"].get(id)).filter(|saved| saved["kind"] == kind);
            if let Some(saved) = saved {
                for key in MACHINE_KEYS {
                    if let Some(value) = saved["data"].get(*key) {
                        data.insert((*key).into(), value.clone());
                    }
                }
            }
        }
    }
    if let Some(floats) = layout["floats"].as_array_mut() {
        let saved = machine.and_then(|m| m["floats"].as_array());
        for float in floats.iter_mut().filter_map(Value::as_object_mut) {
            let bounds = saved
                .and_then(|saved| saved.iter().find(|s| s["id"] == float["id"]))
                .and_then(|s| s.get("bounds"));
            if let Some(bounds) = bounds {
                float.insert("bounds".into(), bounds.clone());
            }
        }
    }
    layout
}

/// `path` relative to `root` with forward slashes, when it is inside it.
fn relative(path: &Path, root: &Path) -> Option<String> {
    let key = |p: &Path| p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase();
    let (full, base) = (key(path), key(root));
    if full == base {
        return Some(".".into());
    }
    let original = path.to_string_lossy().replace('\\', "/");
    full.strip_prefix(&format!("{base}/")).map(|_| original[base.len() + 1..].to_string())
}

/// Read the folder's file.
pub fn read(root: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path(root)).ok()?).ok()
}

/// The file's text: indented, so a change reads as a small diff.
pub fn text(layout: &Value) -> String {
    serde_json::to_string_pretty(layout).unwrap_or_default() + "\n"
}

#[cfg(test)]
mod tests {
    use super::{relative, restore, shareable};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn paths_inside_the_folder_become_relative() {
        let root = Path::new(r"C:\repos\den");
        assert_eq!(relative(Path::new(r"C:\repos\den\src\main.rs"), root).as_deref(), Some("src/main.rs"));
        assert_eq!(relative(Path::new(r"C:\Repos\Den"), root).as_deref(), Some("."));
        assert_eq!(relative(Path::new(r"C:\repos\den2\x"), root), None);
    }

    #[test]
    fn round_trip_keeps_machine_state_apart() {
        let root = Path::new(r"C:\repos\den");
        let full = json!({
            "root": {},
            "panes": {
                "1": { "kind": "Terminal", "data": { "cwd": r"C:\repos\den\src", "scrollback": "old output" } },
                "2": { "kind": "File", "data": { "path": r"D:\elsewhere\a.txt" } }
            }
        });
        let shared = shareable(&full, root);
        assert_eq!(shared["panes"]["1"]["data"], json!({ "cwd": "src" }));
        assert_eq!(shared["panes"]["2"]["data"]["path"], r"D:\elsewhere\a.txt");
        let back = restore(shared, root, Some(&full));
        assert_eq!(back["panes"]["1"]["data"]["cwd"], r"C:\repos\den\src");
        assert_eq!(back["panes"]["1"]["data"]["scrollback"], "old output");
    }

    #[test]
    fn floating_windows_keep_their_place_on_this_machine() {
        let root = Path::new(r"C:\repos\den");
        let full = json!({
            "root": {},
            "panes": {},
            "floats": [{ "id": 9, "root": {}, "bounds": [10.0, 20.0, 800.0, 600.0] }]
        });
        let shared = shareable(&full, root);
        assert!(shared["floats"][0].get("bounds").is_none());
        let back = restore(shared, root, Some(&full));
        assert_eq!(back["floats"][0]["bounds"], json!([10.0, 20.0, 800.0, 600.0]));
    }
}
