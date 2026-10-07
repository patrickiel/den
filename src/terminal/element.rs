//! Painting the terminal grid.
//!
//! Every cell sits at its column times the cell width, whatever the glyph:
//! runs of ASCII text in one style are shaped together, and anything else
//! (box drawing, symbols, wide characters, fallback fonts) is shaped on its
//! own at its cell, so a glyph from another font cannot push the rest of the
//! row out of line.

use alacritty_terminal::{
    index::Point as GridPoint,
    term::cell::Flags,
    vte::ansi::{Color, CursorShape},
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::{TerminalPanel, colors::Palette, glyphs};
use crate::settings::Settings;

pub struct TerminalElement {
    view: Entity<TerminalPanel>,
    focused: bool,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalPanel>, focused: bool) -> Self {
        Self { view, focused }
    }
}

/// Everything to paint, laid out in prepaint.
#[derive(Default)]
pub struct Plan {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    selection: Vec<(Bounds<Pixels>, Hsla)>,
    /// Block elements and box-drawing lines, drawn rather than shaped.
    shapes: Vec<(Bounds<Pixels>, Hsla)>,
    text: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<PaintQuad>,
    cursor_text: Option<(Point<Pixels>, ShapedLine)>,
    underlines: Vec<(Bounds<Pixels>, Hsla)>,
    line_height: Pixels,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[derive(Clone, PartialEq)]
struct Style {
    weight: FontWeight,
    italic: bool,
    color: Hsla,
    underline: bool,
    strike: bool,
}

struct Segment {
    row: usize,
    col: usize,
    cells: usize,
    text: String,
    style: Style,
    /// Only ASCII runs take more cells; anything else stands alone.
    extendable: bool,
}

/// A coordinate on a device pixel, so neighbouring cells meet without a seam.
fn snap(v: Pixels, scale: f32) -> Pixels {
    px((v.as_f32() * scale).round() / scale)
}

/// The part of a cell at `origin` given in fractions, on device pixels.
fn snapped(origin: Point<Pixels>, cell: Size<Pixels>, x: f32, y: f32, w: f32, h: f32, scale: f32) -> Bounds<Pixels> {
    let x0 = snap(origin.x + cell.width * x, scale);
    let y0 = snap(origin.y + cell.height * y, scale);
    let x1 = snap(origin.x + cell.width * (x + w), scale);
    let y1 = snap(origin.y + cell.height * (y + h), scale);
    Bounds::from_corners(point(x0, y0), point(x1, y1))
}

/// A box-drawing character's arms into `shapes`: bars from the cell's centre
/// to its edges (one bar across when both sides have an arm), one device
/// pixel thick (light) or two (heavy), meeting in the middle. A bar continues
/// the one before it, so a line is one rect.
fn push_arms(shapes: &mut Vec<(Bounds<Pixels>, Hsla)>, origin: Point<Pixels>, cell: Size<Pixels>, arms: glyphs::Arms, scale: f32, color: Hsla) {
    let device = scale.round().max(1.) / scale;
    let t = px(if arms.heavy { device * 2. } else { device });
    let left = snap(origin.x, scale);
    let right = snap(origin.x + cell.width, scale);
    let top = snap(origin.y, scale);
    let bottom = snap(origin.y + cell.height, scale);
    let cx = snap(origin.x + cell.width / 2. - t / 2., scale);
    let cy = snap(origin.y + cell.height / 2. - t / 2., scale);
    if arms.left || arms.right {
        let (x0, x1) = (if arms.left { left } else { cx }, if arms.right { right } else { cx + t });
        push_rect(shapes, Bounds::from_corners(point(x0, cy), point(x1, cy + t)), color);
    }
    if arms.up || arms.down {
        let (y0, y1) = (if arms.up { top } else { cy }, if arms.down { bottom } else { cy + t });
        push_rect(shapes, Bounds::from_corners(point(cx, y0), point(cx + t, y1)), color);
    }
}

/// Push a rect, or widen the last one when it continues it in the same
/// colour and height.
fn push_rect(rects: &mut Vec<(Bounds<Pixels>, Hsla)>, bounds: Bounds<Pixels>, color: Hsla) {
    if let Some((last, last_color)) = rects.last_mut()
        && *last_color == color
        && last.origin.y == bounds.origin.y
        && last.size.height == bounds.size.height
        && (last.origin.x + last.size.width - bounds.origin.x).abs() < px(0.5)
    {
        last.size.width += bounds.size.width;
        return;
    }
    rects.push((bounds, color));
}

impl TerminalPanel {
    fn plan(&self, focused: bool, window: &mut Window, cx: &App) -> Plan {
        let theme = cx.theme();
        let palette = Palette::new(theme.foreground, theme.background, theme.selection);
        let font_size = px(Settings::get(cx).editor_font_size);
        let base = font(crate::settings::mono_font(cx));
        let cell = self.cell;
        let origin = self.origin;
        let at = |row: usize, col: usize| point(origin.x + cell.width * col as f32, origin.y + cell.height * row as f32);

        let scale = window.scale_factor();
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let selection = content.selection;
        let cursor = content.cursor;
        let mut plan = Plan {
            line_height: cell.height,
            ..Plan::default()
        };
        let mut selection_rects = Vec::new();
        let mut segments: Vec<Segment> = Vec::new();
        // The cell under the cursor: its character, style, width, and
        // whether it is drawn as a shape rather than text.
        let mut cursor_cell: Option<(char, Style, usize, bool)> = None;
        // Runs of one colour are the norm: a colour is converted once per run.
        type Memo = Option<(Color, Hsla)>;
        let (mut last_fg, mut last_bg): (Memo, Memo) = (None, None);
        let hsla = |memo: &mut Memo, color: Color, foreground: bool| match memo {
            Some((known, hsla)) if *known == color => *hsla,
            _ => {
                let hsla = palette.color(color, foreground);
                *memo = Some((color, hsla));
                hsla
            }
        };

        for item in content.display_iter {
            let row = item.point.line.0 + offset;
            if row < 0 || row as usize >= self.lines {
                continue;
            }
            let (row, col) = (row as usize, item.point.column.0);
            let flags = item.cell.flags;
            let mut fg = hsla(&mut last_fg, item.cell.fg, true);
            let mut bg = hsla(&mut last_bg, item.cell.bg, false);
            if flags.contains(Flags::DIM) {
                fg = fg.opacity(0.66);
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let cells = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let bounds = Bounds::new(at(row, col), size(cell.width * cells as f32, cell.height));
            if bg != palette.background {
                push_rect(&mut plan.backgrounds, bounds, bg);
            }
            if selection.is_some_and(|range| range.contains(item.point)) {
                push_rect(&mut selection_rects, bounds, palette.selection);
            }
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let ch = if flags.contains(Flags::HIDDEN) { ' ' } else { item.cell.c };
            let style = Style {
                weight: if flags.contains(Flags::BOLD) { FontWeight::BOLD } else { FontWeight::NORMAL },
                italic: flags.contains(Flags::ITALIC),
                color: fg,
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strike: flags.contains(Flags::STRIKEOUT),
            };
            let block = glyphs::block(ch);
            let arms = if block.is_none() { glyphs::arms(ch) } else { None };
            if item.point == cursor.point {
                cursor_cell = Some((ch, style.clone(), cells, block.is_some() || arms.is_some()));
            }
            if let Some(rects) = block {
                for r in rects {
                    let bounds = snapped(bounds.origin, cell, r.x, r.y, r.w, r.h, scale);
                    push_rect(&mut plan.shapes, bounds, fg.opacity(fg.a * r.alpha));
                }
                continue;
            }
            if let Some(arms) = arms {
                push_arms(&mut plan.shapes, bounds.origin, cell, arms, scale, fg);
                continue;
            }
            if ch == ' ' && !style.underline && !style.strike {
                continue;
            }
            let ascii = ch.is_ascii();
            match segments.last_mut() {
                Some(seg) if ascii && seg.extendable && seg.row == row && seg.col + seg.cells == col && seg.style == style => {
                    seg.text.push(ch);
                    seg.cells += 1;
                }
                _ => segments.push(Segment {
                    row,
                    col,
                    cells,
                    text: ch.to_string(),
                    style,
                    extendable: ascii,
                }),
            }
        }
        plan.selection = selection_rects;

        let shape = |text: String, style: &Style, color: Hsla, window: &mut Window| {
            let mut font = base.clone();
            font.weight = style.weight;
            if style.italic {
                font.style = FontStyle::Italic;
            }
            let run = TextRun {
                len: text.len(),
                font,
                color,
                background_color: None,
                underline: style.underline.then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: false,
                }),
                strikethrough: style.strike.then(|| StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                }),
            };
            window.text_system().shape_line(text.into(), font_size, &[run], None)
        };
        for seg in segments {
            let color = seg.style.color;
            let line = shape(seg.text, &seg.style, color, window);
            plan.text.push((at(seg.row, seg.col), line));
        }

        // The cursor, when the view is at the bottom.
        if cursor.shape != CursorShape::Hidden && offset == 0 {
            let row = cursor.point.line.0.max(0) as usize;
            let col = cursor.point.column.0;
            let cells = cursor_cell.as_ref().map_or(1, |(_, _, cells, _)| *cells);
            let origin = at(row, col);
            let width = cell.width * cells as f32;
            let color = palette.cursor;
            plan.cursor = Some(match (focused, cursor.shape) {
                (true, CursorShape::Beam) => fill(Bounds::new(origin, size(px(2.), cell.height)), color),
                (true, CursorShape::Underline) => {
                    fill(Bounds::new(point(origin.x, origin.y + cell.height - px(2.)), size(width, px(2.))), color)
                }
                (true, _) => fill(Bounds::new(origin, size(width, cell.height)), color),
                (false, _) => outline(Bounds::new(origin, size(width, cell.height)), color, BorderStyle::Solid),
            });
            // Under a block cursor the character shows in the background colour.
            if focused
                && matches!(cursor.shape, CursorShape::Block | CursorShape::HollowBlock)
                && let Some((ch, style, _, drawn)) = cursor_cell
                && ch != ' '
                && !drawn
            {
                plan.cursor_text = Some((origin, shape(ch.to_string(), &style, palette.background, window)));
            }
        }

        if let Some((row, range)) = &self.hover_link {
            let y = origin.y + cell.height * (*row as f32 + 1.) - px(1.);
            let x = origin.x + cell.width * range.start as f32;
            let width = cell.width * range.len() as f32;
            plan.underlines.push((Bounds::new(point(x, y), size(width, px(1.))), theme.link));
        }
        plan
    }

    /// The grid cell under a window position: its grid point, which half of
    /// the cell, and its screen row and column.
    pub(super) fn cell_at(&self, position: Point<Pixels>) -> (GridPoint, alacritty_terminal::index::Side, usize, usize) {
        use alacritty_terminal::index::{Column, Line, Side};
        let x = (position.x - self.origin.x).as_f32() / self.cell.width.as_f32().max(1.);
        let y = (position.y - self.origin.y).as_f32() / self.cell.height.as_f32().max(1.);
        let col = (x.floor().max(0.) as usize).min(self.columns.saturating_sub(1));
        let row = (y.floor().max(0.) as usize).min(self.lines.saturating_sub(1));
        let side = if x.fract() > 0.5 { Side::Right } else { Side::Left };
        let offset = self.term.grid().display_offset() as i32;
        (GridPoint::new(Line(row as i32 - offset), Column(col)), side, row, col)
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Plan;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = gpui_kit::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.view.update(cx, |this, cx| this.fit(bounds, window, cx));
        let view = self.view.clone();
        let focused = self.focused;
        view.read(cx).plan(focused, window, cx)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        plan: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.read(cx).focus_handle.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
        for (bounds, color) in plan.backgrounds.drain(..) {
            window.paint_quad(fill(bounds, color));
        }
        for (bounds, color) in plan.selection.drain(..) {
            window.paint_quad(fill(bounds, color));
        }
        for (bounds, color) in plan.shapes.drain(..) {
            window.paint_quad(fill(bounds, color));
        }
        for (origin, line) in plan.text.drain(..) {
            _ = line.paint(origin, plan.line_height, TextAlign::Left, None, window, cx);
        }
        if let Some(cursor) = plan.cursor.take() {
            window.paint_quad(cursor);
        }
        if let Some((origin, line)) = plan.cursor_text.take() {
            _ = line.paint(origin, plan.line_height, TextAlign::Left, None, window, cx);
        }
        for (bounds, color) in plan.underlines.drain(..) {
            window.paint_quad(fill(bounds, color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{glyphs, push_arms, push_rect};
    use gpui_kit::{Bounds, Hsla, Pixels, point, px, size};

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn neighbouring_rects_merge_only_at_the_same_height() {
        let color = Hsla::default();
        let mut rects = Vec::new();
        push_rect(&mut rects, rect(0., 0., 8., 16.), color);
        push_rect(&mut rects, rect(8., 0., 8., 16.), color);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].0.size.width, px(16.));
        // A half block beside a full one keeps its own height.
        push_rect(&mut rects, rect(16., 0., 8., 8.), color);
        assert_eq!(rects.len(), 2);
    }

    #[test]
    fn a_line_through_the_cell_is_one_bar() {
        let color = Hsla::default();
        let cell = size(px(8.), px(16.));
        let mut shapes = Vec::new();
        push_arms(&mut shapes, point(px(0.), px(0.)), cell, glyphs::arms('─').unwrap(), 1., color);
        assert_eq!(shapes.len(), 1);
        assert_eq!((shapes[0].0.origin.x, shapes[0].0.size.width), (px(0.), px(8.)));
        // The next cell's bar continues it.
        push_arms(&mut shapes, point(px(8.), px(0.)), cell, glyphs::arms('─').unwrap(), 1., color);
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].0.size.width, px(16.));
        let mut corner = Vec::new();
        push_arms(&mut corner, point(px(0.), px(0.)), cell, glyphs::arms('┌').unwrap(), 1., color);
        assert_eq!(corner.len(), 2);
    }
}
