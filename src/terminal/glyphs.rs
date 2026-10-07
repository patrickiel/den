//! Block elements and box-drawing lines, drawn as rectangles that fill their
//! cell exactly, as xterm.js draws them.
//!
//! A font's glyphs for these neither fill a cell taller than the font nor meet
//! their neighbours exactly, so pixel art (Claude Code's mascot) and frames
//! come out with gaps. Each shape here is a list of rectangles in fractions of
//! the cell, or arms from its centre for lines.

use smallvec::{SmallVec, smallvec};

/// A rectangle in fractions of the cell, and how opaque (shades).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

const fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h, alpha: 1.0 }
}

const UPPER_LEFT: Rect = rect(0.0, 0.0, 0.5, 0.5);
const UPPER_RIGHT: Rect = rect(0.5, 0.0, 0.5, 0.5);
const LOWER_LEFT: Rect = rect(0.0, 0.5, 0.5, 0.5);
const LOWER_RIGHT: Rect = rect(0.5, 0.5, 0.5, 0.5);

/// The rectangles of a block element (U+2580–U+259F).
pub fn block(c: char) -> Option<SmallVec<[Rect; 3]>> {
    let code = c as u32;
    let shape = match code {
        0x2580 => smallvec![rect(0.0, 0.0, 1.0, 0.5)],
        // Lower one eighth … lower seven eighths.
        0x2581..=0x2587 => {
            let n = (code - 0x2580) as f32 / 8.0;
            smallvec![rect(0.0, 1.0 - n, 1.0, n)]
        }
        0x2588 => smallvec![rect(0.0, 0.0, 1.0, 1.0)],
        // Left seven eighths … left one eighth.
        0x2589..=0x258F => {
            let n = (0x2590 - code) as f32 / 8.0;
            smallvec![rect(0.0, 0.0, n, 1.0)]
        }
        0x2590 => smallvec![rect(0.5, 0.0, 0.5, 1.0)],
        0x2591..=0x2593 => {
            let alpha = (code - 0x2590) as f32 * 0.25;
            smallvec![Rect { alpha, ..rect(0.0, 0.0, 1.0, 1.0) }]
        }
        0x2594 => smallvec![rect(0.0, 0.0, 1.0, 0.125)],
        0x2595 => smallvec![rect(0.875, 0.0, 0.125, 1.0)],
        0x2596 => smallvec![LOWER_LEFT],
        0x2597 => smallvec![LOWER_RIGHT],
        0x2598 => smallvec![UPPER_LEFT],
        0x2599 => smallvec![UPPER_LEFT, LOWER_LEFT, LOWER_RIGHT],
        0x259A => smallvec![UPPER_LEFT, LOWER_RIGHT],
        0x259B => smallvec![UPPER_LEFT, UPPER_RIGHT, LOWER_LEFT],
        0x259C => smallvec![UPPER_LEFT, UPPER_RIGHT, LOWER_RIGHT],
        0x259D => smallvec![UPPER_RIGHT],
        0x259E => smallvec![UPPER_RIGHT, LOWER_LEFT],
        0x259F => smallvec![UPPER_RIGHT, LOWER_LEFT, LOWER_RIGHT],
        _ => return None,
    };
    Some(shape)
}

/// Which arms of a box-drawing character reach from the centre to the edges,
/// and whether they are heavy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arms {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub heavy: bool,
}

/// The arms of the light and heavy solid lines, corners, tees and crosses.
/// Rounded corners are drawn square. Dashed and double lines are left to the
/// font.
pub fn arms(c: char) -> Option<Arms> {
    let (up, down, left, right, heavy) = match c {
        '─' => (false, false, true, true, false),
        '━' => (false, false, true, true, true),
        '│' => (true, true, false, false, false),
        '┃' => (true, true, false, false, true),
        '┌' | '╭' => (false, true, false, true, false),
        '┏' => (false, true, false, true, true),
        '┐' | '╮' => (false, true, true, false, false),
        '┓' => (false, true, true, false, true),
        '└' | '╰' => (true, false, false, true, false),
        '┗' => (true, false, false, true, true),
        '┘' | '╯' => (true, false, true, false, false),
        '┛' => (true, false, true, false, true),
        '├' => (true, true, false, true, false),
        '┣' => (true, true, false, true, true),
        '┤' => (true, true, true, false, false),
        '┫' => (true, true, true, false, true),
        '┬' => (false, true, true, true, false),
        '┳' => (false, true, true, true, true),
        '┴' => (true, false, true, true, false),
        '┻' => (true, false, true, true, true),
        '┼' => (true, true, true, true, false),
        '╋' => (true, true, true, true, true),
        '╴' => (false, false, true, false, false),
        '╵' => (true, false, false, false, false),
        '╶' => (false, false, false, true, false),
        '╷' => (false, true, false, false, false),
        _ => return None,
    };
    Some(Arms { up, down, left, right, heavy })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eighths_grow_from_their_edge() {
        assert_eq!(block('▁').as_deref(), Some([rect(0.0, 0.875, 1.0, 0.125)].as_slice()));
        assert_eq!(block('▇').as_deref(), Some([rect(0.0, 0.125, 1.0, 0.875)].as_slice()));
        assert_eq!(block('▉').as_deref(), Some([rect(0.0, 0.0, 0.875, 1.0)].as_slice()));
        assert_eq!(block('▏').as_deref(), Some([rect(0.0, 0.0, 0.125, 1.0)].as_slice()));
    }

    #[test]
    fn halves_and_quadrants() {
        assert_eq!(block('▀').as_deref(), Some([rect(0.0, 0.0, 1.0, 0.5)].as_slice()));
        assert_eq!(block('▄').as_deref(), Some([rect(0.0, 0.5, 1.0, 0.5)].as_slice()));
        assert_eq!(block('▐').as_deref(), Some([rect(0.5, 0.0, 0.5, 1.0)].as_slice()));
        assert_eq!(block('▚').as_deref(), Some([UPPER_LEFT, LOWER_RIGHT].as_slice()));
    }

    #[test]
    fn shades_are_translucent_fills() {
        assert_eq!(block('▒').unwrap()[0].alpha, 0.5);
    }

    #[test]
    fn letters_are_left_to_the_font() {
        assert!(block('a').is_none());
        assert!(arms('a').is_none());
        assert!(arms('═').is_none());
    }

    #[test]
    fn corners_have_two_arms() {
        let corner = arms('╭').unwrap();
        assert!(corner.down && corner.right && !corner.up && !corner.left);
    }
}
