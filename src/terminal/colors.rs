//! Terminal colours: the ANSI palette and its mapping onto the theme.

use std::sync::RwLock;

use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui_kit::{Hsla, Rgba};

/// The theme's terminal colours; `None` follows the app's own colour.
#[derive(Clone, Copy, Debug)]
pub struct TermColors {
    pub ansi: [Hsla; 16],
    pub foreground: Option<Hsla>,
    pub background: Option<Hsla>,
    pub cursor: Option<Hsla>,
    pub selection: Option<Hsla>,
}

impl Default for TermColors {
    /// VS Code's terminal palette (den's dark theme).
    fn default() -> Self {
        const ANSI: [u32; 16] = [
            0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, //
            0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
        ];
        Self {
            ansi: ANSI.map(|v| gpui_kit::rgb(v).into()),
            foreground: None,
            background: None,
            cursor: None,
            selection: None,
        }
    }
}

static PALETTE: RwLock<Option<TermColors>> = RwLock::new(None);

/// Set by the theme; every terminal paints with it.
pub fn set_palette(colors: TermColors) {
    if let Ok(mut palette) = PALETTE.write() {
        *palette = Some(colors);
    }
}

pub fn palette() -> TermColors {
    PALETTE.read().ok().and_then(|p| *p).unwrap_or_default()
}

fn hsla_rgb(color: Hsla) -> Rgb {
    let rgba = Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    Rgb {
        r: byte(rgba.r),
        g: byte(rgba.g),
        b: byte(rgba.b),
    }
}

pub fn rgb_hsla(rgb: Rgb) -> Hsla {
    gpui_kit::rgb((rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32).into()
}

pub struct Palette {
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
    pub ansi: [Hsla; 16],
}

impl Palette {
    /// The theme's terminal colours, the app's where it has none.
    pub fn new(foreground: Hsla, background: Hsla, selection: Hsla) -> Self {
        let colors = palette();
        let foreground = colors.foreground.unwrap_or(foreground);
        Self {
            foreground,
            background: colors.background.unwrap_or(background),
            cursor: colors.cursor.unwrap_or(foreground),
            selection: colors.selection.unwrap_or(selection),
            ansi: colors.ansi,
        }
    }

    /// The colours painted by the terminal, for OSC 4/10/11/12 replies.
    pub fn rgb(&self, index: usize) -> Rgb {
        let value = match index {
            0..=15 => return hsla_rgb(self.ansi[index]),
            16..=231 => {
                let i = index - 16;
                let level = |v: usize| if v == 0 { 0 } else { 55 + v as u32 * 40 };
                (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
            }
            232..=255 => {
                let v = 8 + (index as u32 - 232) * 10;
                (v << 16) | (v << 8) | v
            }
            index if index == NamedColor::Foreground as usize => return hsla_rgb(self.foreground),
            index if index == NamedColor::Background as usize => return hsla_rgb(self.background),
            index if index == NamedColor::Cursor as usize => return hsla_rgb(self.cursor),
            _ => 0xcccccc,
        };
        Rgb {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        }
    }

    fn indexed(&self, index: usize) -> Hsla {
        if index < 16 {
            self.ansi[index]
        } else {
            rgb_hsla(self.rgb(index))
        }
    }

    pub fn color(&self, color: Color, foreground: bool) -> Hsla {
        match color {
            Color::Spec(rgb) => rgb_hsla(rgb),
            Color::Indexed(index) => self.indexed(index as usize),
            Color::Named(named) => match named {
                NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
                NamedColor::DimForeground => self.foreground.opacity(0.66),
                NamedColor::Background => self.background,
                NamedColor::Cursor => self.cursor,
                named => {
                    let index = named as usize;
                    if index < 16 {
                        self.ansi[index]
                    } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize)
                        .contains(&index)
                    {
                        self.ansi[index - NamedColor::DimBlack as usize].opacity(0.66)
                    } else if foreground {
                        self.foreground
                    } else {
                        self.background
                    }
                }
            },
        }
    }
}
