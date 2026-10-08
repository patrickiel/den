//! The icon on a preset's button, as in den: its own pick from a small set
//! (vendor logos, levels, glyphs, its letter boxed, ringed or bare), else the automatic one (the
//! logo of the program its command runs, a local server's port, the plain
//! shell's terminal, else its first letter), in its own colour or the
//! theme's. Picked in Settings from a panel of the same choices.
//!
//! Choices use den's names (`letter`, `logo:claude`, `fourBars`, …), so
//! presets taken over from den keep their icons.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::settings::{Preset, Settings};

/// Vendor marks: key, name, whether there is a version in brand colours.
pub const LOGOS: &[(&str, &str, bool)] = &[
    ("claude", "Claude", true),
    ("openai", "OpenAI", false),
    ("antigravity", "Antigravity", true),
    ("codex", "Codex", true),
    ("githubcopilot", "GitHub Copilot", false),
    ("cursor", "Cursor", false),
    ("amp", "Amp", true),
    ("opencode", "OpenCode", false),
    ("goose", "Goose", false),
    ("kimi", "Kimi", true),
    ("qwen", "Qwen", true),
    ("mistral", "Mistral", true),
    ("cline", "Cline", false),
    ("kilocode", "Kilo Code", false),
    ("kiro", "Kiro", true),
    ("junie", "Junie", true),
    ("openhands", "OpenHands", true),
];

/// The logo of each program that has one, by the command that runs it.
const PROGRAM_LOGOS: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("codex", "openai"),
    ("agy", "antigravity"),
    ("copilot", "githubcopilot"),
    ("cursor-agent", "cursor"),
    ("amp", "amp"),
    ("opencode", "opencode"),
    ("goose", "goose"),
    ("kimi", "kimi"),
    ("qwen", "qwen"),
    ("vibe", "mistral"),
    ("cline", "cline"),
    ("kilocode", "kilocode"),
    ("kilo", "kilocode"),
    ("kiro-cli", "kiro"),
    ("junie", "junie"),
    ("openhands", "openhands"),
];

/// Icons in series, for presets that are levels of one thing.
pub const LEVELS: &[&str] = &["numberOne", "numberTwo", "numberThree", "numberFour", "oneBar", "twoBars", "threeBars", "fourBars"];

/// The other icons a preset can pick.
pub const GLYPHS: &[&str] = &[
    "terminalWindow", "code", "bug", "flask", "rocket", "lightning", "sparkle", "robot", "brain", "magicWand", "lightbulb", "fire", "star", "heart",
    "crown", "ghost", "cpu", "database", "cloud", "globe", "package", "wrench", "gearSix", "gitBranch",
];

/// The swatches; the hue bar reaches the rest.
pub const COLORS: &[&str] = &["#e5484d", "#f76b15", "#e2a336", "#46a758", "#12a594", "#0090ff", "#6e56cf", "#d6409f"];

/// The asset of a level or glyph by den's name.
fn glyph_path(name: &str) -> Option<String> {
    let file = match name {
        "oneBar" => "bars-one".to_string(),
        "twoBars" => "bars-two".into(),
        "threeBars" => "bars-three".into(),
        "fourBars" => "bars-four".into(),
        other if LEVELS.contains(&other) || GLYPHS.contains(&other) => {
            // "magicWand" -> "magic-wand"
            let mut out = String::new();
            for c in other.chars() {
                if c.is_uppercase() {
                    out.push('-');
                    out.extend(c.to_lowercase());
                } else {
                    out.push(c);
                }
            }
            out
        }
        _ => return None,
    };
    Some(format!("presets/glyphs/{file}.svg"))
}

/// "magicWand" -> "Magic wand".
fn words(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else if c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// What a preset's button shows.
enum Look {
    Glyph(String),
    /// A logo: its one-colour mark, and its brand-colour version if any.
    Logo(String, Option<String>),
    Port(String),
    Letter(String, Frame),
}

/// What surrounds a letter icon.
#[derive(Clone, Copy)]
enum Frame {
    Square,
    Circle,
    /// None: the letter fills the icon.
    Bare,
}

/// A local dev server's port (":5173").
fn local_port(url: &str) -> Option<String> {
    let rest = url.trim().trim_start_matches("http://").trim_start_matches("https://");
    let host = rest.split('/').next()?;
    let (name, port) = host.rsplit_once(':')?;
    let local = name == "localhost" || name == "127.0.0.1";
    (local && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit())).then(|| format!(":{port}"))
}

fn logo_look(key: &str) -> Option<Look> {
    let (key, _, color) = LOGOS.iter().find(|(k, ..)| *k == key)?;
    Some(Look::Logo(format!("presets/logos/{key}.svg"), color.then(|| format!("presets/logos/{key}-color.svg"))))
}

/// How `preset` looks with `icon` (`None`: the automatic icon).
fn look(preset: &Preset, icon: Option<&str>) -> Look {
    let letter = || Look::Letter(preset.letter(), Frame::Square);
    match icon {
        Some("letter") => return letter(),
        Some("letterCircle") => return Look::Letter(preset.letter(), Frame::Circle),
        Some("letterBare") => return Look::Letter(preset.letter(), Frame::Bare),
        Some(choice) if choice.starts_with("logo:") => {
            if let Some(look) = logo_look(&choice[5..]) {
                return look;
            }
        }
        Some(choice) => {
            if let Some(path) = glyph_path(choice) {
                return Look::Glyph(path);
            }
        }
        None => {}
    }
    if preset.browser {
        return local_port(&preset.command).map_or_else(letter, Look::Port);
    }
    let command = preset.command.trim();
    if command.is_empty() {
        return Look::Glyph("presets/glyphs/terminal-window.svg".into());
    }
    let program = crate::backend::agent::program_of(command.split_whitespace().next().unwrap_or(command));
    PROGRAM_LOGOS.iter().find(|(p, _)| *p == program).and_then(|(_, key)| logo_look(key)).unwrap_or_else(letter)
}

/// Whether `command` runs a CLI that has a logo (Claude Code, Codex, …).
pub fn has_program_logo(command: &str) -> bool {
    let program = crate::backend::agent::program_of(command.split_whitespace().next().unwrap_or(command));
    PROGRAM_LOGOS.iter().any(|(p, _)| *p == program)
}

/// A colour as `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` (the `#` may be
/// left out); `None` for anything else, an empty string included.
pub fn parse_color(hex: &str) -> Option<Hsla> {
    let hex = hex.trim();
    if hex.is_empty() {
        return None;
    }
    let with_hash;
    let hex = if hex.starts_with('#') {
        hex
    } else {
        with_hash = format!("#{hex}");
        &with_hash
    };
    Rgba::try_from(hex).ok().map(Hsla::from)
}

/// The preset's icon, `size` pixels square.
pub fn render(preset: &Preset, icon: Option<&str>, size: f32, cx: &App) -> AnyElement {
    let own = preset.color.as_deref().and_then(parse_color);
    let color = own.unwrap_or(cx.theme().foreground);
    match look(preset, icon) {
        Look::Glyph(path) => svg().path(path).size(px(size)).flex_none().text_color(color).into_any_element(),
        Look::Logo(mark, brand) => match (brand, own) {
            (Some(brand), None) => img(SharedString::from(brand)).size(px(size)).flex_none().into_any_element(),
            _ => svg().path(mark).size(px(size)).flex_none().text_color(color).into_any_element(),
        },
        Look::Port(text) => div().flex_none().text_color(color).text_size(px(size * 0.62)).font_weight(FontWeight::BOLD).child(text).into_any_element(),
        Look::Letter(text, frame) => div()
            .size(px(size))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .text_color(color)
            .font_weight(FontWeight::BOLD)
            .map(|this| match frame {
                Frame::Square => this.rounded(px(3.)).border(px(1.5)).border_color(color).text_size(px(size * 0.62)),
                Frame::Circle => this.rounded_full().border(px(1.5)).border_color(color).text_size(px(size * 0.62)),
                Frame::Bare => this.text_size(px(size * 1.1)).line_height(px(size)),
            })
            .child(text)
            .into_any_element(),
    }
}

/// The colour at `hue` degrees, saturated and light enough to read on dark
/// and light themes (den's `hueColor`).
pub fn hue_color(hue: f32) -> String {
    let (s, l) = (0.7_f32, 0.58_f32);
    let f = |n: f32| {
        let k = (n + hue / 30.) % 12.;
        let c = l - s * l.min(1. - l) * (k - 3.).min(9. - k).clamp(-1., 1.);
        (c * 255.).round() as u8
    };
    format!("#{:02x}{:02x}{:02x}", f(0.), f(8.), f(4.))
}

/// The hue of a "#rrggbb" colour in degrees; 0 for greys.
pub fn color_hue(hex: &str) -> f32 {
    let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f32 / 255.;
    let (r, g, b) = (channel(1), channel(3), channel(5));
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    if d == 0. {
        return 0.;
    }
    let h = if max == r {
        ((g - b) / d) % 6.
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    ((h * 60. + 360.) % 360.).round()
}

/// Change preset `ix` in Settings.
fn edit(ix: usize, cx: &mut App, f: impl FnOnce(&mut Preset)) {
    Settings::update(cx, |s| {
        if let Some(preset) = s.presets.get_mut(ix) {
            f(preset);
        }
    });
}

/// The picker panel for preset `ix`: its automatic icon, its letters and the
/// logos; the levels; the glyphs; the theme colour, swatches and a hue bar.
/// Each pick applies at once.
pub fn picker(ix: usize, cx: &App) -> AnyElement {
    let Some(preset) = Settings::get(cx).presets.get(ix).cloned() else { return div().into_any_element() };
    let theme = cx.theme().clone();
    let current = preset.icon.clone();
    let cell = |choice: Option<String>, title: String, preset: &Preset| {
        let selected = current == choice;
        let icon = choice.clone();
        div()
            .id(SharedString::from(format!("pick-{ix}-{}", choice.as_deref().unwrap_or("auto"))))
            .size(px(34.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .border_1()
            .border_color(if selected { theme.ring } else { transparent_black() })
            .when(selected, |this| this.bg(theme.accent))
            .hover(|this| this.bg(theme.accent))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(title.clone()).build(window, cx))
            .child(render(preset, icon.as_deref(), 18., cx))
            .on_click(move |_, _, cx| {
                let choice = choice.clone();
                edit(ix, cx, |p| p.icon = choice);
            })
    };
    let grid = |cells: Vec<Stateful<Div>>| h_flex().flex_wrap().w(px(8. * 36.)).gap(px(2.)).children(cells);
    let mut own = vec![
        cell(None, "Automatic".into(), &preset),
        cell(Some("letter".into()), "First letter".into(), &preset),
        cell(Some("letterCircle".into()), "First letter in a circle".into(), &preset),
        cell(Some("letterBare".into()), "First letter, large".into(), &preset),
    ];
    own.extend(LOGOS.iter().map(|(key, name, _)| cell(Some(format!("logo:{key}")), (*name).into(), &preset)));
    let levels = LEVELS.iter().map(|name| cell(Some((*name).into()), words(name), &preset)).collect();
    let glyphs = GLYPHS.iter().map(|name| cell(Some((*name).into()), words(name), &preset)).collect();

    let swatch = |id: String, color: Option<&'static str>| {
        let selected = preset.color.as_deref().map(str::to_lowercase) == color.map(str::to_string);
        let fill = color.and_then(parse_color).unwrap_or(theme.foreground);
        div()
            .id(SharedString::from(id))
            .size(px(26.))
            .rounded_full()
            .border_2()
            .border_color(if selected { theme.foreground } else { transparent_black() })
            .p(px(3.))
            .child(div().size_full().rounded_full().bg(fill))
            .on_click(move |_, _, cx| edit(ix, cx, |p| p.color = color.map(str::to_string)))
    };
    let mut swatches = vec![swatch(format!("swatch-{ix}-theme"), None)];
    swatches.extend(COLORS.iter().map(|color| swatch(format!("swatch-{ix}-{color}"), Some(color))));
    // The hue bar, as den's slider: press or drag anywhere along it.
    const BAR: f32 = 8. * 36.;
    let picked_hue = preset.color.as_deref().map(color_hue);
    let bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>> = Default::default();
    let pick = {
        let bounds = bounds.clone();
        move |x: Pixels, cx: &mut App| {
            let Some(area) = bounds.get() else { return };
            let t = ((x - area.left()) / area.size.width).clamp(0., 1.);
            let color = hue_color(t * 359.);
            edit(ix, cx, |p| p.color = Some(color));
        }
    };
    let (pick_down, pick_move) = (pick.clone(), pick);
    let hue_bar = div()
        .id(SharedString::from(format!("hue-{ix}")))
        .relative()
        .w(px(BAR))
        .h(px(16.))
        .child(
            h_flex()
                .absolute()
                .top(px(2.))
                .left_0()
                .w_full()
                .h(px(12.))
                .rounded_full()
                .overflow_hidden()
                .children((0..72).map(|step| {
                    let fill = parse_color(&hue_color(step as f32 * 5.)).unwrap_or(theme.foreground);
                    div().flex_1().h_full().bg(fill)
                })),
        )
        .when_some(picked_hue, |this, hue| {
            this.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(hue / 359. * BAR - 8.))
                    .size(px(16.))
                    .rounded_full()
                    .border_2()
                    .border_color(gpui_kit::white())
                    .bg(parse_color(preset.color.as_deref().unwrap_or("#000000")).unwrap_or(theme.foreground))
                    .shadow_sm(),
            )
        })
        .child(
            canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
        .on_mouse_down(MouseButton::Left, move |event, _, cx| pick_down(event.position.x, cx))
        .on_mouse_move(move |event, _, cx| {
            if event.pressed_button == Some(MouseButton::Left) {
                pick_move(event.position.x, cx);
            }
        });
    v_flex()
        .gap_3()
        .p_1()
        .child(grid(own))
        .child(grid(levels))
        .child(grid(glyphs))
        .child(h_flex().gap_1().children(swatches))
        .child(hue_bar)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{color_hue, glyph_path, hue_color, local_port, parse_color};

    #[test]
    fn glyph_files_by_den_names() {
        assert_eq!(glyph_path("magicWand").as_deref(), Some("presets/glyphs/magic-wand.svg"));
        assert_eq!(glyph_path("threeBars").as_deref(), Some("presets/glyphs/bars-three.svg"));
        assert_eq!(glyph_path("nope"), None);
    }

    #[test]
    fn hues_round_trip() {
        for hue in [0., 40., 120., 200., 300.] {
            assert!((color_hue(&hue_color(hue)) - hue).abs() <= 2., "{hue}");
        }
    }

    #[test]
    fn ports_of_local_servers() {
        assert_eq!(local_port("localhost:5173").as_deref(), Some(":5173"));
        assert_eq!(local_port("https://example.com:8080"), None);
    }

    #[test]
    fn parses_short_and_alpha_hex() {
        let white = parse_color("#ffffff").unwrap();
        assert_eq!(parse_color("#fff"), Some(white));
        assert_eq!(parse_color(" fff "), Some(white));
        assert_eq!(parse_color("#ffffff80").map(|c| (c.a * 100.).round()), Some(50.));
        assert_eq!(parse_color(""), None);
        assert_eq!(parse_color("#zzz"), None);
    }
}
