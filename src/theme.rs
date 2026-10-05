//! Colour themes, as in den: a VS Code color theme (the JSON a theme
//! extension ships) paints the whole window. The workbench colours map onto
//! the component theme, `terminal.*` onto the terminal palette, and the
//! TextMate `tokenColors` onto the editor's syntax colours.
//!
//! den's own Dark and Light are VS Code themes too, built in; an imported
//! theme is folded together (its `include` chain) and kept as one file in the
//! `themes` folder next to `settings.json`. Colours a theme leaves out come
//! from the built-in theme of its kind.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context as _, anyhow, bail};
use gpui_kit::component::{Theme, ThemeConfig};
use gpui_kit::*;
use serde_json::{Map, Value, json};

use crate::settings::{Settings, ThemeChoice};

/// A theme file, folded: no `include` left.
#[derive(Clone, Debug)]
pub struct VsTheme {
    pub name: String,
    pub dark: bool,
    pub colors: HashMap<String, String>,
    pub token_colors: Vec<Value>,
}

/// An imported theme in the `themes` folder.
#[derive(Clone, Debug)]
pub struct Imported {
    /// The file's stem, as stored in Settings.
    pub id: String,
    pub theme: VsTheme,
}

// -- Applying ----------------------------------------------------------------

/// Paint the window with the theme Settings name.
pub fn apply(cx: &mut App) {
    let settings = Settings::get(cx);
    let (choice, color_theme) = (settings.theme, settings.color_theme.clone());
    preview(choice, &color_theme, cx);
}

/// Paint the window with a theme without choosing it (Settings' menu, on
/// hover); `apply` goes back to the chosen one.
pub fn preview(choice: ThemeChoice, color_theme: &str, cx: &mut App) {
    let imported = (!color_theme.is_empty()).then(|| load(color_theme)).flatten();
    let theme = match imported {
        Some(theme) => theme,
        None => builtin(choice == ThemeChoice::Dark),
    };
    let base = builtin(theme.dark);
    crate::terminal::colors::set_palette(terminal_palette(&theme, &base));
    let config = match serde_json::from_value::<ThemeConfig>(to_config(&theme, &base)) {
        Ok(config) => Rc::new(config),
        Err(err) => {
            eprintln!("den: theme {}: {err}", theme.name);
            return;
        }
    };
    Theme::update(cx, |t| t.apply_config(&config));
    cx.refresh_windows();
}

// -- Files -------------------------------------------------------------------

fn themes_dir() -> PathBuf {
    crate::settings::data_dir().join("themes")
}

/// The imported themes, by name.
pub fn imported() -> Vec<Imported> {
    let mut list: Vec<Imported> = std::fs::read_dir(themes_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let id = path.file_stem()?.to_string_lossy().to_string();
            let theme = parse(&std::fs::read_to_string(&path).ok()?, &id).ok()?;
            Some(Imported { id, theme })
        })
        .collect();
    list.sort_by_key(|t| t.theme.name.to_lowercase());
    list
}

fn load(id: &str) -> Option<VsTheme> {
    let text = std::fs::read_to_string(themes_dir().join(format!("{id}.json"))).ok()?;
    parse(&text, id).ok()
}

/// Import a theme file: fold its includes, keep it in the `themes` folder.
/// Returns its id.
pub fn import(path: &Path) -> anyhow::Result<String> {
    let value = resolve(path, 0)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let name = value["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| label_of(&stem));
    let mut value = value;
    value["name"] = json!(name);
    // Checks it is a theme at all.
    parse(&value.to_string(), &stem)?;
    let slug: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let id = format!("user-{}", slug.trim_matches('-').chars().take(60).collect::<String>());
    std::fs::create_dir_all(themes_dir())?;
    std::fs::write(themes_dir().join(format!("{id}.json")), serde_json::to_string_pretty(&value)?)?;
    Ok(id)
}

pub fn remove(id: &str) {
    let _ = std::fs::remove_file(themes_dir().join(format!("{id}.json")));
}

/// A theme file with its `include` chain folded in: the including file's
/// colours and token rules win, as in VS Code.
fn resolve(path: &Path, depth: usize) -> anyhow::Result<Value> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut value: Value = serde_json::from_str(&strip_jsonc(&text)).context("not a JSON file")?;
    if !value.is_object() {
        bail!("not a theme file");
    }
    let Some(include) = value["include"].as_str().map(str::to_string) else { return Ok(value) };
    if depth >= 8 {
        bail!("includes go too deep");
    }
    let base = resolve(&path.parent().unwrap_or(Path::new(".")).join(include), depth + 1)?;
    let mut colors = base["colors"].as_object().cloned().unwrap_or_default();
    colors.extend(value["colors"].as_object().cloned().unwrap_or_default());
    let mut tokens = base["tokenColors"].as_array().cloned().unwrap_or_default();
    tokens.extend(value["tokenColors"].as_array().cloned().unwrap_or_default());
    let object = value.as_object_mut().expect("checked above");
    object.remove("include");
    object.insert("colors".into(), Value::Object(colors));
    object.insert("tokenColors".into(), Value::Array(tokens));
    for key in ["name", "type"] {
        if !object.contains_key(key)
            && let Some(v) = base.get(key)
        {
            object.insert(key.into(), v.clone());
        }
    }
    Ok(value)
}

/// Parse a folded theme file.
pub fn parse(text: &str, id: &str) -> anyhow::Result<VsTheme> {
    let value: Value = serde_json::from_str(&strip_jsonc(text)).context("not a JSON file")?;
    let colors: HashMap<String, String> = value["colors"]
        .as_object()
        .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).filter(|(_, v)| !v.is_empty()).collect())
        .unwrap_or_default();
    let token_colors = value["tokenColors"].as_array().cloned().unwrap_or_default();
    if colors.is_empty() && token_colors.is_empty() {
        return Err(anyhow!("no \"colors\" or \"tokenColors\": not a VS Code color theme"));
    }
    let dark = match value["type"].as_str().map(str::to_lowercase).as_deref() {
        Some("light" | "hclight" | "hc-light") => false,
        Some(_) => true,
        // No type: guess from the editor's background.
        None => colors.get("editor.background").and_then(|bg| luminance(bg)).is_none_or(|l| l <= 128.),
    };
    let name = value["name"].as_str().map(str::to_string).unwrap_or_else(|| label_of(id));
    Ok(VsTheme { name, dark, colors, token_colors })
}

fn luminance(hex: &str) -> Option<f32> {
    let hex = hex.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok().map(f32::from);
    Some((byte(0)? * 299. + byte(2)? * 587. + byte(4)? * 114.) / 1000.)
}

/// `monokai-color-theme` → "Monokai", `dark_modern` → "Dark Modern".
fn label_of(stem: &str) -> String {
    let lower = stem.to_lowercase();
    let stem = lower.strip_suffix("color-theme").or_else(|| lower.strip_suffix("color_theme")).unwrap_or(&lower);
    let words: Vec<String> = stem
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| w[..1].to_uppercase() + &w[1..])
        .collect();
    if words.is_empty() { "Theme".into() } else { words.join(" ") }
}

/// JSON with the comments and trailing commas VS Code allows, made plain.
pub fn strip_jsonc(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut start = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                out.push_str(&text[start..i]);
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                start = i;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                out.push_str(&text[start..i]);
                i = text[i + 2..].find("*/").map_or(bytes.len(), |end| i + 2 + end + 2);
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&text[start.min(text.len())..]);
    // Trailing commas before `}` or `]`, outside strings.
    let mut result = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut in_string = false;
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if in_string {
            result.push(c);
            if c == '\\' && k + 1 < chars.len() {
                result.push(chars[k + 1]);
                k += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            result.push(c);
        } else if c == ',' && chars[k + 1..].iter().find(|c| !c.is_whitespace()).is_some_and(|n| *n == '}' || *n == ']') {
            // dropped
        } else {
            result.push(c);
        }
        k += 1;
    }
    result
}

// -- Mapping -----------------------------------------------------------------

/// The theme's colours over its kind's built-in ones; a colour that does not
/// parse counts as left out.
fn merged<'a>(theme: &'a VsTheme, base: &'a VsTheme) -> impl Fn(&[&str]) -> Option<String> + 'a {
    let valid = |value: &&String| Rgba::try_from(value.as_str()).is_ok();
    move |keys: &[&str]| {
        keys.iter()
            .find_map(|k| theme.colors.get(*k).filter(valid))
            .or_else(|| keys.iter().find_map(|k| base.colors.get(*k).filter(valid)))
            .cloned()
    }
}

/// The component theme for `theme`.
fn to_config(theme: &VsTheme, base: &VsTheme) -> Value {
    let c = merged(theme, base);
    let mut colors = Map::new();
    let mut set = |key: &str, keys: &[&str]| {
        if let Some(value) = c(keys) {
            colors.insert(key.into(), json!(value));
        }
    };
    set("background", &["editor.background"]);
    set("foreground", &["foreground", "editor.foreground"]);
    set("border", &["sideBar.border", "panel.border", "editorGroup.border", "contrastBorder"]);
    set("muted.background", &["editorWidget.background", "input.background"]);
    set("muted.foreground", &["descriptionForeground", "disabledForeground"]);
    set("accent.background", &["toolbar.hoverBackground", "list.hoverBackground"]);
    set("accent.foreground", &["foreground", "editor.foreground"]);
    set("primary.background", &["button.background", "focusBorder"]);
    set("primary.hover.background", &["button.hoverBackground", "button.background"]);
    set("primary.active.background", &["button.hoverBackground", "button.background"]);
    set("primary.foreground", &["button.foreground"]);
    set("secondary.background", &["button.secondaryBackground", "input.background"]);
    set("secondary.hover.background", &["button.secondaryHoverBackground", "toolbar.hoverBackground"]);
    set("secondary.active.background", &["button.secondaryHoverBackground", "toolbar.hoverBackground"]);
    set("secondary.foreground", &["button.secondaryForeground", "foreground"]);
    set("popover.background", &["menu.background", "dropdown.background", "editorWidget.background"]);
    set("popover.foreground", &["menu.foreground", "foreground"]);
    set("input.border", &["input.border", "dropdown.border", "widget.border"]);
    set("ring", &["focusBorder"]);
    set("caret", &["editorCursor.foreground", "foreground"]);
    set("selection.background", &["editor.selectionBackground"]);
    set("link", &["textLink.foreground"]);
    set("link.hover", &["textLink.activeForeground", "textLink.foreground"]);
    set("link.active", &["textLink.activeForeground", "textLink.foreground"]);
    set("list.background", &["sideBar.background", "editor.background"]);
    set("list.even.background", &["sideBar.background", "editor.background"]);
    set("list.hover.background", &["list.hoverBackground"]);
    set("list.active.background", &["list.activeSelectionBackground", "list.inactiveSelectionBackground"]);
    set("list.active.border", &["list.focusOutline", "focusBorder"]);
    set("sidebar.background", &["sideBar.background"]);
    set("sidebar.foreground", &["sideBar.foreground", "foreground"]);
    set("sidebar.border", &["sideBar.border", "panel.border"]);
    set("sidebar.accent.background", &["list.hoverBackground"]);
    set("sidebar.accent.foreground", &["foreground"]);
    set("sidebar.primary.background", &["button.background"]);
    set("sidebar.primary.foreground", &["button.foreground"]);
    set("title_bar.background", &["titleBar.activeBackground", "sideBar.background"]);
    set("title_bar.border", &["titleBar.border", "sideBar.border"]);
    set("status_bar.background", &["statusBar.background"]);
    set("status_bar.border", &["statusBar.border", "sideBar.border"]);
    set("tab_bar.background", &["editorGroupHeader.tabsBackground"]);
    set("tab.background", &["tab.inactiveBackground", "editorGroupHeader.tabsBackground"]);
    set("tab.active.background", &["tab.activeBackground", "editor.background"]);
    set("tab.foreground", &["tab.inactiveForeground"]);
    set("tab.active.foreground", &["tab.activeForeground"]);
    set("scrollbar.thumb.background", &["scrollbarSlider.background"]);
    set("scrollbar.thumb.hover.background", &["scrollbarSlider.hoverBackground"]);
    set("danger.background", &["inputValidation.errorBorder", "errorForeground"]);
    set("danger.hover.background", &["inputValidation.errorBorder", "errorForeground"]);
    set("danger.active.background", &["inputValidation.errorBorder", "errorForeground"]);
    set("warning.background", &["notificationsWarningIcon.foreground", "editorWarning.foreground"]);
    set("info.background", &["notificationsInfoIcon.foreground", "editorInfo.foreground"]);
    set("success.background", &["gitDecoration.addedResourceForeground"]);
    set("drop_target.background", &["editorGroup.dropBackground"]);
    set("drag.border", &["focusBorder"]);
    for (key, ansi) in [("red", "Red"), ("green", "Green"), ("yellow", "Yellow"), ("blue", "Blue"), ("magenta", "Magenta"), ("cyan", "Cyan")] {
        set(&format!("base.{key}"), &[&format!("terminal.ansi{ansi}")]);
        set(&format!("base.{key}.light"), &[&format!("terminal.ansiBright{ansi}")]);
    }

    let mut highlight = Map::new();
    let mut put = |key: &str, keys: &[&str]| {
        if let Some(value) = c(keys) {
            highlight.insert(key.into(), json!(value));
        }
    };
    put("editor.background", &["editor.background"]);
    put("editor.foreground", &["editor.foreground", "foreground"]);
    put("editor.active_line.background", &["editor.lineHighlightBackground"]);
    put("editor.line_number", &["editorLineNumber.foreground"]);
    put("editor.active_line_number", &["editorLineNumber.activeForeground"]);
    put("editor.gutter.background", &["editorGutter.background", "editor.background"]);
    put("error", &["editorError.foreground", "errorForeground"]);
    put("warning", &["editorWarning.foreground"]);
    put("info", &["editorInfo.foreground"]);
    // A theme without token colours (a workbench-only file) keeps its kind's.
    let tokens = if theme.token_colors.is_empty() { &base.token_colors } else { &theme.token_colors };
    highlight.insert("syntax".into(), Value::Object(syntax(tokens)));

    json!({
        "name": theme.name,
        "mode": if theme.dark { "dark" } else { "light" },
        "colors": colors,
        "highlight": highlight,
    })
}

/// Tree-sitter capture → the TextMate scopes that colour it, most specific first.
const CAPTURES: &[(&str, &[&str])] = &[
    ("attribute", &["entity.other.attribute-name"]),
    ("boolean", &["constant.language.boolean", "constant.language"]),
    ("comment", &["comment"]),
    ("comment.doc", &["comment.block.documentation", "comment"]),
    ("constant", &["variable.other.constant", "constant.language", "constant"]),
    ("constructor", &["entity.name.type.class", "entity.name.type"]),
    ("embedded", &["meta.embedded"]),
    ("emphasis", &["markup.italic"]),
    ("emphasis.strong", &["markup.bold"]),
    ("enum", &["entity.name.type.enum", "entity.name.type"]),
    ("function", &["entity.name.function", "support.function"]),
    ("keyword", &["keyword", "storage.type"]),
    ("label", &["entity.name.label"]),
    ("link_text", &["string.other.link", "markup.link"]),
    ("link_uri", &["markup.underline.link"]),
    ("number", &["constant.numeric"]),
    ("operator", &["keyword.operator"]),
    ("preproc", &["meta.preprocessor", "keyword.control.directive"]),
    ("property", &["variable.other.property", "support.type.property-name", "variable.other.object.property"]),
    ("punctuation", &["punctuation"]),
    ("punctuation.bracket", &["punctuation.bracket", "punctuation"]),
    ("punctuation.delimiter", &["punctuation.separator", "punctuation"]),
    ("punctuation.list_marker", &["punctuation.definition.list", "markup.list"]),
    ("punctuation.special", &["punctuation.definition.template-expression", "punctuation"]),
    ("string", &["string"]),
    ("string.escape", &["constant.character.escape"]),
    ("string.regex", &["string.regexp"]),
    ("string.special", &["string.other", "string"]),
    ("string.special.symbol", &["constant.other.symbol", "string"]),
    ("tag", &["entity.name.tag"]),
    ("tag.doctype", &["meta.tag.metadata.doctype", "entity.name.tag"]),
    ("text.code.span", &["markup.inline.raw"]),
    ("text.literal", &["markup.inline.raw", "string"]),
    ("title", &["markup.heading", "entity.name.section"]),
    ("type", &["entity.name.type", "support.type", "support.class"]),
    ("variable", &["variable.other.readwrite", "variable.other", "variable"]),
    ("variable.special", &["variable.language"]),
    ("variant", &["variable.other.enummember", "constant.other"]),
];

/// The syntax colours for the TextMate rules `tokens`.
fn syntax(tokens: &[Value]) -> Map<String, Value> {
    // (scope, foreground, fontStyle) of every rule, in file order.
    let mut rules: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for entry in tokens {
        let settings = &entry["settings"];
        let foreground = settings["foreground"].as_str().filter(|v| Rgba::try_from(*v).is_ok()).map(str::to_string);
        let style = settings["fontStyle"].as_str().map(|s| s.trim().to_string());
        if foreground.is_none() && style.is_none() {
            continue;
        }
        let scopes: Vec<String> = match &entry["scope"] {
            Value::String(s) => s.split(',').map(str::to_string).collect(),
            Value::Array(list) => list.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => continue,
        };
        for raw in scopes {
            // "source.ts meta.class entity.name.type": the last segment is the scope.
            if let Some(scope) = raw.split_whitespace().last() {
                rules.push((scope.to_string(), foreground.clone(), style.clone()));
            }
        }
    }
    let mut out = Map::new();
    for (capture, scopes) in CAPTURES {
        // The most specific rule that covers the scope; the later one on a tie.
        let found = scopes.iter().find_map(|target| {
            rules
                .iter()
                .enumerate()
                .filter(|(_, (scope, ..))| target == scope || target.starts_with(&format!("{scope}.")))
                .max_by_key(|(ix, (scope, ..))| (scope.len(), *ix))
                .map(|(_, rule)| rule.clone())
        });
        let Some((_, foreground, style)) = found else { continue };
        let mut entry = Map::new();
        if let Some(color) = foreground {
            entry.insert("color".into(), json!(color));
        }
        if let Some(style) = style {
            if style.contains("italic") {
                entry.insert("font_style".into(), json!("italic"));
            }
            if style.contains("bold") {
                entry.insert("font_weight".into(), json!(700));
            }
        }
        if !entry.is_empty() {
            out.insert((*capture).into(), Value::Object(entry));
        }
    }
    out
}

/// The terminal palette for `theme`.
fn terminal_palette(theme: &VsTheme, base: &VsTheme) -> crate::terminal::colors::TermColors {
    let c = merged(theme, base);
    let color = |keys: &[&str]| c(keys).and_then(|hex| Rgba::try_from(hex.as_str()).ok()).map(Hsla::from);
    const NAMES: [&str; 16] = [
        "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White", "BrightBlack", "BrightRed", "BrightGreen", "BrightYellow", "BrightBlue",
        "BrightMagenta", "BrightCyan", "BrightWhite",
    ];
    let mut ansi = crate::terminal::colors::TermColors::default().ansi;
    for (slot, name) in ansi.iter_mut().zip(NAMES) {
        if let Some(value) = color(&[&format!("terminal.ansi{name}")]) {
            *slot = value;
        }
    }
    let background = color(&["terminal.background", "panel.background", "editor.background"]);
    let foreground = color(&["terminal.foreground", "foreground"]);
    crate::terminal::colors::TermColors {
        ansi,
        background,
        foreground,
        cursor: color(&["terminalCursor.foreground"]).or(foreground),
        selection: color(&["terminal.selectionBackground", "editor.selectionBackground"]),
    }
}

// -- Built in ----------------------------------------------------------------

/// den's Dark or Light (VS Code's Dark Modern and Light Modern, Dark+ and
/// Light+ token colours).
pub fn builtin(dark: bool) -> VsTheme {
    let value = if dark { dark_theme() } else { light_theme() };
    parse(&value.to_string(), if dark { "dark" } else { "light" }).expect("built-in themes parse")
}

fn rule(scope: &str, foreground: &str) -> Value {
    json!({ "scope": scope, "settings": { "foreground": foreground } })
}

fn dark_theme() -> Value {
    json!({
        "name": "Dark",
        "type": "dark",
        "colors": {
            "editor.background": "#1e1e1e",
            "editor.foreground": "#cccccc",
            "foreground": "#cccccc",
            "descriptionForeground": "#8b8b8b",
            "toolbar.hoverBackground": "#3a3a3a",
            "sideBar.background": "#181818",
            "sideBar.border": "#2b2b2b",
            "titleBar.activeBackground": "#181818",
            "titleBar.border": "#2b2b2b",
            "statusBar.background": "#181818",
            "statusBar.border": "#2b2b2b",
            "menu.background": "#1f1f1f",
            "menu.border": "#454545",
            "editorWidget.background": "#202020",
            "focusBorder": "#0078d4",
            "button.background": "#0078d4",
            "button.hoverBackground": "#026ec1",
            "button.foreground": "#ffffff",
            "button.secondaryBackground": "#313131",
            "button.secondaryHoverBackground": "#3c3c3c",
            "button.secondaryForeground": "#cccccc",
            "input.background": "#313131",
            "input.border": "#3c3c3c",
            "list.hoverBackground": "#2a2d2e",
            "list.inactiveSelectionBackground": "#37373d",
            "list.activeSelectionBackground": "#04395e",
            "errorForeground": "#f14c4c",
            "editorWarning.foreground": "#cca700",
            "editorInfo.foreground": "#3794ff",
            "gitDecoration.addedResourceForeground": "#81b88b",
            "editorGroupHeader.tabsBackground": "#181818",
            "editorGroup.dropBackground": "#53595d80",
            "tab.activeBackground": "#1e1e1e",
            "tab.inactiveBackground": "#181818",
            "tab.activeForeground": "#ffffff",
            "tab.inactiveForeground": "#9d9d9d",
            "editorLineNumber.foreground": "#6e7681",
            "editorLineNumber.activeForeground": "#cccccc",
            "editor.selectionBackground": "#264f78",
            "editor.lineHighlightBackground": "#ffffff08",
            "editorCursor.foreground": "#aeafad",
            "textLink.foreground": "#4daafc",
            "textLink.activeForeground": "#4daafc",
            "scrollbarSlider.background": "#79797966",
            "scrollbarSlider.hoverBackground": "#646464b3",
            "terminal.background": "#1e1e1e",
            "terminal.foreground": "#cccccc",
            "terminalCursor.foreground": "#ffffff",
            "terminal.selectionBackground": "#264f78",
            "terminal.ansiBlack": "#000000",
            "terminal.ansiRed": "#cd3131",
            "terminal.ansiGreen": "#0dbc79",
            "terminal.ansiYellow": "#e5e510",
            "terminal.ansiBlue": "#2472c8",
            "terminal.ansiMagenta": "#bc3fbc",
            "terminal.ansiCyan": "#11a8cd",
            "terminal.ansiWhite": "#e5e5e5",
            "terminal.ansiBrightBlack": "#666666",
            "terminal.ansiBrightRed": "#f14c4c",
            "terminal.ansiBrightGreen": "#23d18b",
            "terminal.ansiBrightYellow": "#f5f543",
            "terminal.ansiBrightBlue": "#3b8eea",
            "terminal.ansiBrightMagenta": "#d670d6",
            "terminal.ansiBrightCyan": "#29b8db",
            "terminal.ansiBrightWhite": "#e5e5e5"
        },
        "tokenColors": [
            rule("comment", "#6a9955"),
            rule("string", "#ce9178"),
            rule("string.regexp", "#d16969"),
            rule("constant.character.escape", "#d7ba7d"),
            rule("constant.numeric", "#b5cea8"),
            rule("constant.language", "#569cd6"),
            rule("variable.other.constant", "#4fc1ff"),
            rule("variable.other.enummember", "#4fc1ff"),
            rule("keyword", "#569cd6"),
            rule("keyword.control", "#c586c0"),
            rule("keyword.operator", "#d4d4d4"),
            rule("storage", "#569cd6"),
            rule("storage.type", "#569cd6"),
            rule("entity.name.function", "#dcdcaa"),
            rule("support.function", "#dcdcaa"),
            rule("entity.name.type", "#4ec9b0"),
            rule("support.type", "#4ec9b0"),
            rule("support.class", "#4ec9b0"),
            rule("variable", "#9cdcfe"),
            rule("variable.language", "#569cd6"),
            rule("variable.other.property", "#9cdcfe"),
            rule("support.type.property-name", "#9cdcfe"),
            rule("entity.name.tag", "#569cd6"),
            rule("entity.other.attribute-name", "#9cdcfe"),
            rule("entity.name.label", "#c8c8c8"),
            rule("punctuation", "#d4d4d4"),
            rule("meta.preprocessor", "#569cd6"),
            rule("markup.heading", "#569cd6"),
            rule("markup.inline.raw", "#ce9178"),
            rule("markup.underline.link", "#3794ff"),
            { "scope": "markup.bold", "settings": { "fontStyle": "bold", "foreground": "#569cd6" } },
            { "scope": "markup.italic", "settings": { "fontStyle": "italic" } }
        ]
    })
}

fn light_theme() -> Value {
    json!({
        "name": "Light",
        "type": "light",
        "colors": {
            "editor.background": "#ffffff",
            "editor.foreground": "#3b3b3b",
            "foreground": "#3b3b3b",
            "descriptionForeground": "#6e6e6e",
            "toolbar.hoverBackground": "#e8e8e8",
            "sideBar.background": "#f8f8f8",
            "sideBar.border": "#e5e5e5",
            "titleBar.activeBackground": "#f8f8f8",
            "titleBar.border": "#e5e5e5",
            "statusBar.background": "#f8f8f8",
            "statusBar.border": "#e5e5e5",
            "menu.background": "#ffffff",
            "menu.border": "#cecece",
            "editorWidget.background": "#f8f8f8",
            "focusBorder": "#005fb8",
            "button.background": "#005fb8",
            "button.hoverBackground": "#0258a8",
            "button.foreground": "#ffffff",
            "button.secondaryBackground": "#e5e5e5",
            "button.secondaryHoverBackground": "#cccccc",
            "button.secondaryForeground": "#3b3b3b",
            "input.background": "#ffffff",
            "input.border": "#cecece",
            "list.hoverBackground": "#f2f2f2",
            "list.inactiveSelectionBackground": "#e4e6f1",
            "list.activeSelectionBackground": "#d5e5f6",
            "errorForeground": "#e51400",
            "editorWarning.foreground": "#bf8803",
            "editorInfo.foreground": "#1a85ff",
            "gitDecoration.addedResourceForeground": "#587c0c",
            "editorGroupHeader.tabsBackground": "#f8f8f8",
            "editorGroup.dropBackground": "#2677cb2d",
            "tab.activeBackground": "#ffffff",
            "tab.inactiveBackground": "#f8f8f8",
            "tab.activeForeground": "#3b3b3b",
            "tab.inactiveForeground": "#868686",
            "editorLineNumber.foreground": "#6e7681",
            "editorLineNumber.activeForeground": "#171184",
            "editor.selectionBackground": "#add6ff",
            "editor.lineHighlightBackground": "#0000000a",
            "editorCursor.foreground": "#000000",
            "textLink.foreground": "#005fb8",
            "textLink.activeForeground": "#005fb8",
            "scrollbarSlider.background": "#64646466",
            "scrollbarSlider.hoverBackground": "#646464b3",
            "terminal.background": "#ffffff",
            "terminal.foreground": "#3b3b3b",
            "terminalCursor.foreground": "#000000",
            "terminal.selectionBackground": "#add6ff",
            "terminal.ansiBlack": "#000000",
            "terminal.ansiRed": "#cd3131",
            "terminal.ansiGreen": "#107c10",
            "terminal.ansiYellow": "#949800",
            "terminal.ansiBlue": "#0451a5",
            "terminal.ansiMagenta": "#bc05bc",
            "terminal.ansiCyan": "#0598bc",
            "terminal.ansiWhite": "#555555",
            "terminal.ansiBrightBlack": "#666666",
            "terminal.ansiBrightRed": "#cd3131",
            "terminal.ansiBrightGreen": "#14ce14",
            "terminal.ansiBrightYellow": "#b5ba00",
            "terminal.ansiBrightBlue": "#0451a5",
            "terminal.ansiBrightMagenta": "#bc05bc",
            "terminal.ansiBrightCyan": "#0598bc",
            "terminal.ansiBrightWhite": "#a5a5a5"
        },
        "tokenColors": [
            rule("comment", "#008000"),
            rule("string", "#a31515"),
            rule("string.regexp", "#811f3f"),
            rule("constant.character.escape", "#ee0000"),
            rule("constant.numeric", "#098658"),
            rule("constant.language", "#0000ff"),
            rule("variable.other.constant", "#0070c1"),
            rule("variable.other.enummember", "#0070c1"),
            rule("keyword", "#0000ff"),
            rule("keyword.control", "#af00db"),
            rule("keyword.operator", "#000000"),
            rule("storage", "#0000ff"),
            rule("storage.type", "#0000ff"),
            rule("entity.name.function", "#795e26"),
            rule("support.function", "#795e26"),
            rule("entity.name.type", "#267f99"),
            rule("support.type", "#267f99"),
            rule("support.class", "#267f99"),
            rule("variable", "#001080"),
            rule("variable.language", "#0000ff"),
            rule("variable.other.property", "#001080"),
            rule("support.type.property-name", "#0451a5"),
            rule("entity.name.tag", "#800000"),
            rule("entity.other.attribute-name", "#e50000"),
            rule("entity.name.label", "#000000"),
            rule("punctuation", "#000000"),
            rule("meta.preprocessor", "#0000ff"),
            rule("markup.heading", "#800000"),
            rule("markup.inline.raw", "#800000"),
            rule("markup.underline.link", "#005fb8"),
            { "scope": "markup.bold", "settings": { "fontStyle": "bold", "foreground": "#000080" } },
            { "scope": "markup.italic", "settings": { "fontStyle": "italic" } }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::{builtin, label_of, parse, strip_jsonc, syntax, to_config};
    use serde_json::json;

    #[test]
    fn jsonc_comments_and_trailing_commas() {
        let text = "{ // c\n \"a\": \"http://x\", /* b */ \"b\": [1, 2,], }";
        let value: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(value, json!({ "a": "http://x", "b": [1, 2] }));
    }

    #[test]
    fn labels_from_file_names() {
        assert_eq!(label_of("monokai-color-theme"), "Monokai");
        assert_eq!(label_of("dark_modern"), "Dark Modern");
    }

    #[test]
    fn kind_from_type_or_background() {
        assert!(!parse(r##"{"type":"light","colors":{"a":"#fff"}}"##, "x").unwrap().dark);
        assert!(!parse(r##"{"colors":{"editor.background":"#fafafa"}}"##, "x").unwrap().dark);
        assert!(parse(r##"{"colors":{"editor.background":"#101010"}}"##, "x").unwrap().dark);
        assert!(parse("{}", "x").is_err());
    }

    #[test]
    fn the_most_specific_rule_wins() {
        let tokens = vec![
            json!({ "scope": "keyword", "settings": { "foreground": "#111111" } }),
            json!({ "scope": ["keyword.operator", "source.rs keyword.operator.math"], "settings": { "foreground": "#222222" } }),
            json!({ "scope": "comment", "settings": { "foreground": "#333333", "fontStyle": "italic" } }),
        ];
        let map = syntax(&tokens);
        assert_eq!(map["keyword"]["color"], "#111111");
        assert_eq!(map["operator"]["color"], "#222222");
        assert_eq!(map["comment"]["font_style"], "italic");
        assert!(map.get("string").is_none());
    }

    #[test]
    fn built_in_themes_become_component_themes() {
        for dark in [true, false] {
            let theme = builtin(dark);
            let config: gpui_kit::component::ThemeConfig = serde_json::from_value(to_config(&theme, &theme)).unwrap();
            assert!(config.highlight.is_some());
        }
    }
}
