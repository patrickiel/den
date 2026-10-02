//! File-type icons, as den shows them: VS Code's default "Seti" icon theme
//! (MIT, from microsoft/vscode extensions/theme-seti). `seti.ttf` is the icon
//! font (den's seti.woff unpacked; GPUI loads TrueType) and `seti.json` maps
//! file names, extensions and language ids to a glyph and a colour, with
//! lighter-theme colours of its own.

use std::{borrow::Cow, collections::HashMap, sync::LazyLock};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use serde_json::Value;

const FONT: &[u8] = include_bytes!("../assets/fileicons/seti.ttf");
const FAMILY: &str = "seti";

struct Seti {
    theme: Value,
    cache: std::sync::Mutex<HashMap<(String, bool), (String, Option<String>)>>,
}

static SETI: LazyLock<Seti> = LazyLock::new(|| Seti {
    theme: serde_json::from_str(include_str!("../assets/fileicons/seti.json")).unwrap_or_default(),
    cache: Default::default(),
});

/// Load the icon font; once, at start.
pub fn init(cx: &mut App) {
    if let Err(err) = cx.text_system().add_fonts(vec![Cow::Borrowed(FONT)]) {
        eprintln!("den: the file icon font did not load: {err}");
    }
}

/// The language id VS Code would give a file by its extension, for the
/// languages Seti lists only by id (TypeScript, Rust, Python, …).
fn language_of(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "rs" => "rust",
        "py" | "pyi" | "pyw" => "python",
        "md" | "markdown" | "mdown" => "markdown",
        "json" => "json",
        "jsonc" => "jsonc",
        "jsonl" => "jsonl",
        "css" => "css",
        "scss" => "scss",
        "sass" => "sass",
        "less" => "less",
        "html" | "htm" => "html",
        "xml" | "xaml" | "csproj" | "fsproj" | "props" | "targets" => "xml",
        "yml" | "yaml" => "yaml",
        "sh" | "bash" | "zsh" => "shellscript",
        "ps1" | "psm1" | "psd1" => "powershell",
        "bat" | "cmd" => "bat",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "fs" | "fsx" | "fsi" => "fsharp",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "sql" => "sql",
        "swift" => "swift",
        "dart" => "dart",
        "vue" => "vue",
        "pl" | "pm" => "perl",
        "r" => "r",
        "tex" => "latex",
        "ini" | "properties" => "properties",
        _ => return None,
    })
}

/// The glyph and colour for a file name: the exact name, then the longest
/// extension ("foo.spec.ts" tries "spec.ts", then "ts"), then its language.
fn lookup(name: &str, light: bool) -> (String, Option<String>) {
    let name = name.to_lowercase();
    let key = (name.clone(), light);
    if let Some(hit) = SETI.cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let theme = &SETI.theme;
    // The light theme overrides some mappings with "_light" definitions.
    let section = |field: &str, key: &str| -> Option<String> {
        let own = light.then(|| theme["light"][field][key].as_str()).flatten();
        own.or_else(|| theme[field][key].as_str()).map(str::to_string)
    };
    let mut id = section("fileNames", &name);
    let mut at = name.find('.');
    while id.is_none()
        && let Some(i) = at
    {
        id = section("fileExtensions", &name[i + 1..]);
        at = name[i + 1..].find('.').map(|j| i + 1 + j);
    }
    if id.is_none()
        && let Some(language) = name.rsplit_once('.').and_then(|(_, ext)| language_of(ext))
    {
        id = section("languageIds", language);
    }
    let default = if light { theme["light"]["file"].as_str().or(theme["file"].as_str()) } else { theme["file"].as_str() };
    let def = id
        .as_deref()
        .map(|id| &theme["iconDefinitions"][id])
        .filter(|d| d.is_object())
        .unwrap_or(&theme["iconDefinitions"][default.unwrap_or("_default")]);
    let glyph = def["fontCharacter"]
        .as_str()
        .and_then(|c| u32::from_str_radix(c.trim_start_matches('\\'), 16).ok())
        .and_then(char::from_u32)
        .map(String::from)
        .unwrap_or_default();
    let found = (glyph, def["fontColor"].as_str().map(str::to_string));
    if let Ok(mut cache) = SETI.cache.lock() {
        cache.insert(key, found.clone());
    }
    found
}

/// The icon of the file `name`, about `size` pixels.
pub fn render(name: &str, size: f32, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (glyph, color) = lookup(name, !theme.is_dark());
    let color = color.and_then(|hex| crate::preset_icon::parse_color(&hex)).unwrap_or(theme.muted_foreground);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(size))
        .h(px(size))
        .font_family(FAMILY)
        // Seti's glyphs sit small in their em box, as in VS Code.
        .text_size(px(size * 1.25))
        .line_height(px(size))
        .text_color(color)
        .child(glyph)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::lookup;

    #[test]
    fn by_name_extension_and_language() {
        let ts = lookup("actions.ts", false);
        let svelte = lookup("SettingsForm.svelte", false);
        let css = lookup("style.css", false);
        let readme = lookup("README.md", false);
        let unknown = lookup("notes.unknownext", false);
        assert!(!ts.0.is_empty() && ts.1.is_some());
        assert_ne!(ts.0, unknown.0, "TypeScript has its own glyph");
        assert_ne!(svelte.0, unknown.0);
        assert_ne!(css.0, ts.0);
        assert!(!readme.0.is_empty());
    }
}
