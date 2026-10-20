//! The geometric content of a `<draw>`: `<line>`, `<rectangle>` and `<arc>`
//! (XFA 3.3 Chapter 17, "The line element", "The rectangle element", "The
//! arc element").
//!
//! A shape fills its draw's content region. Parsing happens once, at the
//! edge, into [`Shape`]; the geometry below is pure and works in device
//! pixels, so the rasteriser only has to paint what it returns.

use super::{FillKind, PatternKind, Border, Corner, Edge, Fill, GradientDirection, JoinStyle, Num, XfaNode, XfaNodeKind, num};
use rust_decimal::prelude::ToPrimitive;

/// The default `thickness` of an `<edge>` (XFA 3.3 "The edge element").
pub const DEFAULT_EDGE_THICKNESS_PT: f64 = 0.5;

/// Which side of its path a stroke is drawn on (the `hand` attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hand {
    /// Centred on the path.
    #[default]
    Even,
    /// Immediately left of the path, following it from start to end.
    Left,
    /// Immediately right of the path.
    Right,
}

/// A line's orientation within its box (the `slope` attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Slope {
    /// `\`: top-left to bottom-right.
    #[default]
    Down,
    /// `/`: bottom-left to top-right.
    Up,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineShape {
    pub slope: Slope,
    pub hand: Hand,
    pub edge: Edge,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RectangleShape {
    pub hand: Hand,
    /// Top, right, bottom, left; fewer than four reuse the last (the same
    /// rule as a border's). Never empty: a rectangle with no `<edge>` has
    /// the default edge.
    pub edges: Vec<Edge>,
    pub corners: Vec<Corner>,
    pub fill: Option<Fill>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArcShape {
    pub circular: bool,
    pub hand: Hand,
    /// Degrees, counter-clockwise from three o'clock.
    pub start_angle: f32,
    /// Degrees, counter-clockwise.
    pub sweep_angle: f32,
    pub edge: Edge,
    pub fill: Option<Fill>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Line(LineShape),
    Rectangle(RectangleShape),
    Arc(ArcShape),
}

/// The edge an element gets when it declares none: solid, black, visible,
/// 0.5pt.
pub fn default_edge() -> Edge {
    Edge {
        thickness: Some(num(DEFAULT_EDGE_THICKNESS_PT)),
        presence: "visible".to_string(),
        ..Edge::default()
    }
}

/// `edge` with the spec's default thickness when it gives none.
fn with_default_thickness(mut edge: Edge) -> Edge {
    edge.thickness.get_or_insert(num(DEFAULT_EDGE_THICKNESS_PT));
    edge
}

fn element_tag(node: &XfaNode) -> Option<&str> {
    match &node.kind {
        XfaNodeKind::Element { tag_name, .. } => Some(tag_name.as_str()),
        _ => None,
    }
}

fn children_tagged<'a>(node: &'a XfaNode, tag: &'a str) -> impl Iterator<Item = &'a XfaNode> {
    node.children
        .iter()
        .filter(move |c| element_tag(c) == Some(tag))
}

fn parse_hand(node: &XfaNode) -> Hand {
    match node.attributes.get("hand").map(String::as_str) {
        Some("left") => Hand::Left,
        Some("right") => Hand::Right,
        _ => Hand::Even,
    }
}

fn parse_angle(node: &XfaNode, attribute: &str, default: f32) -> Result<f32, String> {
    match node.attributes.get(attribute) {
        None => Ok(default),
        Some(raw) => raw
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|a| a.is_finite())
            .ok_or_else(|| format!("arc {attribute}={raw:?} is not an angle in degrees")),
    }
}

impl Shape {
    /// The shape `value_child` (a child of a `<value>`) describes, or `None`
    /// when it is not a shape element. An angle that is not a number is an
    /// error; an unknown keyword falls back to the attribute's default.
    pub fn parse(value_child: &XfaNode) -> Result<Option<Shape>, String> {
        let first_edge = || {
            children_tagged(value_child, "edge")
                .next()
                .map(|e| with_default_thickness(XfaNode::parse_edge(e)))
                .unwrap_or_else(default_edge)
        };
        let fill = || children_tagged(value_child, "fill").next().map(XfaNode::parse_fill);
        Ok(match element_tag(value_child) {
            Some("line") => Some(Shape::Line(LineShape {
                slope: match value_child.attributes.get("slope").map(String::as_str) {
                    Some("/") => Slope::Up,
                    _ => Slope::Down,
                },
                hand: parse_hand(value_child),
                edge: first_edge(),
            })),
            Some("rectangle") => {
                let mut edges: Vec<Edge> = children_tagged(value_child, "edge")
                    .map(|e| with_default_thickness(XfaNode::parse_edge(e)))
                    .collect();
                if edges.is_empty() {
                    edges.push(default_edge());
                }
                Some(Shape::Rectangle(RectangleShape {
                    hand: parse_hand(value_child),
                    edges,
                    corners: children_tagged(value_child, "corner")
                        .map(XfaNode::parse_corner)
                        .collect(),
                    fill: fill(),
                }))
            }
            Some("arc") => Some(Shape::Arc(ArcShape {
                circular: value_child.attributes.get("circular").map(String::as_str) == Some("1"),
                hand: parse_hand(value_child),
                start_angle: parse_angle(value_child, "startAngle", 0.0)?,
                sweep_angle: parse_angle(value_child, "sweepAngle", 360.0)?,
                edge: first_edge(),
                fill: fill(),
            })),
            _ => None,
        })
    }
}

/// Whether an edge, corner or fill with this `presence` is drawn.
pub fn is_drawn(presence: &str) -> bool {
    !matches!(presence, "hidden" | "inactive" | "invisible")
}

/// A stroke's thickness in device pixels at `scale` pixels per point: at
/// least one pixel, so a hairline stays visible.
pub fn thickness_px(thickness: Option<Num>, scale: f32) -> f32 {
    let pt = thickness
        .and_then(|t| t.to_f32())
        .unwrap_or(DEFAULT_EDGE_THICKNESS_PT as f32);
    (pt * scale).round().max(1.0)
}

/// A box in device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A filled axis-aligned rectangle in whole device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// The centre line of a line's stroke: its path from start to end across
/// `bx`, moved off the path by half the thickness to the side `hand` names.
/// `None` for a box with no extent at all.
pub fn line_centre(slope: Slope, hand: Hand, bx: PixelBox, thickness: f32) -> Option<[(f32, f32); 2]> {
    let (start, end) = match slope {
        Slope::Down => ((bx.x, bx.y), (bx.x + bx.w, bx.y + bx.h)),
        Slope::Up => ((bx.x, bx.y + bx.h), (bx.x + bx.w, bx.y)),
    };
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let length = (dx * dx + dy * dy).sqrt();
    if length == 0.0 {
        return None;
    }
    // Left of the direction of travel, in device space where y grows down.
    let (nx, ny) = (dy / length, -dx / length);
    let offset = match hand {
        Hand::Even => 0.0,
        Hand::Left => thickness / 2.0,
        Hand::Right => -thickness / 2.0,
    };
    let (ox, oy) = (nx * offset, ny * offset);
    Some([(start.0 + ox, start.1 + oy), (end.0 + ox, end.1 + oy)])
}

/// The first whole pixel of a run `t` pixels long centred on `centre`.
fn run_start(centre: f32, t: f32) -> i32 {
    (centre - t / 2.0 + 0.5).floor() as i32
}

/// The pixels of a straight stroke `thickness` wide centred on the segment
/// from `a` to `b`: one rectangle for a horizontal or vertical stroke, one
/// square per step otherwise.
/// The pixel rows inside a polygon (even-odd rule), one rect per row span:
/// a scanline through each pixel row's center, its crossings with the
/// polygon's edges paired up. Degenerate polygons (fewer than three points)
/// fill nothing.
pub fn polygon_fill(points: &[(f32, f32)]) -> Vec<PixelRect> {
    let mut rects = Vec::new();
    if points.len() < 3 {
        return rects;
    }
    let top = points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min).floor() as i32;
    let bottom = points.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max).ceil() as i32;
    for row in top..bottom {
        let y = row as f32 + 0.5;
        let mut crossings: Vec<f32> = Vec::new();
        for i in 0..points.len() {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            if (a.1 <= y) != (b.1 <= y) {
                crossings.push(a.0 + (y - a.1) * (b.0 - a.0) / (b.1 - a.1));
            }
        }
        crossings.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
        for pair in crossings.chunks(2) {
            if let [x0, x1] = pair {
                let (x0, x1) = (x0.round() as i32, x1.round() as i32);
                if x1 > x0 {
                    rects.push(PixelRect { x: x0, y: row, w: x1 - x0, h: 1 });
                }
            }
        }
    }
    rects
}

pub fn stroke_segment(a: (f32, f32), b: (f32, f32), thickness: f32) -> Vec<PixelRect> {
    let t = thickness.max(1.0);
    let side = t.round() as i32;
    if (a.1 - b.1).abs() < 0.5 {
        let x0 = a.0.min(b.0);
        let x1 = a.0.max(b.0);
        return vec![PixelRect {
            x: x0.round() as i32,
            y: run_start(a.1, t),
            w: ((x1 - x0).round() as i32).max(1),
            h: side.max(1),
        }];
    }
    if (a.0 - b.0).abs() < 0.5 {
        let y0 = a.1.min(b.1);
        let y1 = a.1.max(b.1);
        return vec![PixelRect {
            x: run_start(a.0, t),
            y: y0.round() as i32,
            w: side.max(1),
            h: ((y1 - y0).round() as i32).max(1),
        }];
    }
    let steps = (b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil() as i32;
    (0..=steps)
        .map(|i| {
            let f = i as f32 / steps as f32;
            let (cx, cy) = (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
            PixelRect {
                x: run_start(cx, t),
                y: run_start(cy, t),
                w: side.max(1),
                h: side.max(1),
            }
        })
        .collect()
}

/// The box a rectangle's border is drawn inside, given that a border draws
/// each edge inside its box. The rectangle's path runs clockwise from the
/// top left, so a left-handed stroke lies just outside the path and a
/// right-handed one just inside (XFA 3.3 "The rectangle element").
pub fn rectangle_stroke_box(hand: Hand, bx: PixelBox, thickness: f32) -> PixelBox {
    let grow = match hand {
        Hand::Right => 0.0,
        Hand::Even => thickness / 2.0,
        Hand::Left => thickness,
    };
    PixelBox {
        x: bx.x - grow,
        y: bx.y - grow,
        w: bx.w + 2.0 * grow,
        h: bx.h + 2.0 * grow,
    }
}

/// The box whose inscribed ellipse is the centre line of an arc's stroke.
/// An arc's path runs counter-clockwise, so a left-handed stroke lies just
/// inside it and a right-handed one just outside (XFA 3.3 "The arc
/// element"), the reverse of a rectangle's.
pub fn arc_stroke_box(hand: Hand, bx: PixelBox, thickness: f32) -> PixelBox {
    let grow = match hand {
        Hand::Even => 0.0,
        Hand::Left => -thickness / 2.0,
        Hand::Right => thickness / 2.0,
    };
    PixelBox {
        x: bx.x - grow,
        y: bx.y - grow,
        w: (bx.w + 2.0 * grow).max(0.0),
        h: (bx.h + 2.0 * grow).max(0.0),
    }
}

/// Points along an arc's path in `bx`, one per pixel of arc length or so,
/// from `start_angle` through `sweep_angle` degrees counter-clockwise. A
/// circular arc uses the box's smaller dimension, centred.
pub fn arc_points(bx: PixelBox, circular: bool, start_angle: f32, sweep_angle: f32) -> Vec<(f32, f32)> {
    let (mut rx, mut ry) = (bx.w / 2.0, bx.h / 2.0);
    if circular {
        let r = rx.min(ry);
        rx = r;
        ry = r;
    }
    let (cx, cy) = (bx.x + bx.w / 2.0, bx.y + bx.h / 2.0);
    let arc_length = sweep_angle.abs().to_radians() * rx.max(ry);
    let steps = arc_length.ceil().max(1.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = (start_angle + sweep_angle * i as f32 / steps as f32).to_radians();
            (cx + rx * a.cos(), cy - ry * a.sin())
        })
        .collect()
}

/// Whether the pixel centre `(px, py)` lies inside the ellipse that fills
/// `bx` (or the centred circle, for a circular arc).
pub fn inside_ellipse(bx: PixelBox, circular: bool, px: f32, py: f32) -> bool {
    let (mut rx, mut ry) = (bx.w / 2.0, bx.h / 2.0);
    if circular {
        let r = rx.min(ry);
        rx = r;
        ry = r;
    }
    if rx <= 0.0 || ry <= 0.0 {
        return false;
    }
    let (dx, dy) = ((px - bx.x - bx.w / 2.0) / rx, (py - bx.y - bx.h / 2.0) / ry);
    dx * dx + dy * dy <= 1.0
}

/// The color of a `fill` at the pixel centre (`px`, `py`) of a fill box
/// `bx` (XFA 3.3 "The fill element" and its type elements): the start
/// color, blended toward the end color across the box for a `<linear>` fill
/// and outward from the centre for a `<radial>` one; the `<pattern>`'s
/// stroke color on its hatching (strokes one pixel wide every
/// [`PATTERN_INTERVAL_PX`] pixels, an application-defined interval); the
/// `<stipple>`'s color on `rate` percent of the pixels, scattered by a
/// fixed hash so the same box always stipples the same way. A fill with no
/// `<color>` starts from white.
pub fn fill_color_at(fill: &Fill, bx: PixelBox, px: f32, py: f32) -> (u8, u8, u8) {
    let start = fill.color.unwrap_or((255, 255, 255));
    let mix = |end: (u8, u8, u8), t: f32| {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        (m(start.0, end.0), m(start.1, end.1), m(start.2, end.2))
    };
    let along = |pos: f32, origin: f32, extent: f32| {
        if extent <= 1.0 {
            0.0
        } else {
            ((pos - origin) / (extent - 1.0)).clamp(0.0, 1.0)
        }
    };
    match fill.kind {
        FillKind::Solid => start,
        FillKind::Linear(linear) => {
            let t = match linear.direction {
                GradientDirection::ToRight => along(px, bx.x, bx.w),
                GradientDirection::ToLeft => 1.0 - along(px, bx.x, bx.w),
                GradientDirection::ToBottom => along(py, bx.y, bx.h),
                GradientDirection::ToTop => 1.0 - along(py, bx.y, bx.h),
            };
            mix(linear.end, t)
        }
        FillKind::Radial { end, to_center } => {
            let (rx, ry) = ((bx.w / 2.0).max(0.5), (bx.h / 2.0).max(0.5));
            let (dx, dy) = ((px - bx.x - rx) / rx, (py - bx.y - ry) / ry);
            let t = (dx * dx + dy * dy).sqrt().clamp(0.0, 1.0);
            mix(end, if to_center { 1.0 - t } else { t })
        }
        FillKind::Pattern { color, kind } => {
            let (ox, oy) = ((px - bx.x).floor(), (py - bx.y).floor());
            let on_line = |v: f32| v.rem_euclid(PATTERN_INTERVAL_PX) < 1.0;
            let horizontal = on_line(oy);
            let vertical = on_line(ox);
            let down_right = on_line(ox - oy);
            let up_right = on_line(ox + oy);
            let on = match kind {
                PatternKind::Horizontal => horizontal,
                PatternKind::Vertical => vertical,
                PatternKind::CrossHatch => horizontal || vertical,
                PatternKind::DiagonalRight => up_right,
                PatternKind::DiagonalLeft => down_right,
                PatternKind::CrossDiagonal => up_right || down_right,
            };
            if on { color } else { start }
        }
        FillKind::Stipple { color, rate } => {
            let (ix, iy) = ((px - bx.x) as i64, (py - bx.y) as i64);
            // A small integer hash: uniform enough for a dither.
            let h = (ix.wrapping_mul(73_856_093) ^ iy.wrapping_mul(19_349_663)).rem_euclid(100);
            if (h as u8) < rate { color } else { start }
        }
    }
}

/// The interval of a `<pattern>` fill's strokes, in device pixels.
pub const PATTERN_INTERVAL_PX: f32 = 6.0;

/// The trim in pixels each of a border's corners takes off the ends of the
/// edges it joins: its `radius`, "irrespective of whether the corner is
/// round or square, inverted or not" (XFA 3.3 "The edge element"). Capped
/// at half the box's shorter side.
pub fn corner_trims(border: &Border, bx: PixelBox, scale: f32) -> [f32; 4] {
    let cap = (bx.w.min(bx.h) / 2.0).max(0.0);
    let mut trims = [0.0; 4];
    for (i, trim) in trims.iter_mut().enumerate() {
        if let Some(corner) = border.get_corner(i) {
            *trim = (corner.radius.and_then(|r| r.to_f32()).unwrap_or(0.0) * scale).min(cap);
        }
    }
    trims
}

/// The path a corner draws between the trimmed ends of its two edges, as a
/// polyline in pixels, for the corner `index` (top-left, top-right,
/// bottom-right, bottom-left) of the box `bx` whose edges are trimmed by
/// `r` there (XFA 3.3 "The corner element"): a quarter circle for
/// `join="round"`, bulging outward or, `inverted`, inward; two straight
/// segments for `join="square"`, around the vertex or, inverted, notched
/// into the box.
pub fn corner_outline(bx: PixelBox, index: usize, r: f32, round: bool, inverted: bool) -> Vec<(f32, f32)> {
    let (vx, vy) = match index {
        0 => (bx.x, bx.y),
        1 => (bx.x + bx.w, bx.y),
        2 => (bx.x + bx.w, bx.y + bx.h),
        _ => (bx.x, bx.y + bx.h),
    };
    // Unit steps from the vertex along the box's edges (horizontal, vertical).
    let (sx, sy) = match index {
        0 => (1.0, 1.0),
        1 => (-1.0, 1.0),
        2 => (-1.0, -1.0),
        _ => (1.0, -1.0),
    };
    let on_horizontal = (vx + sx * r, vy);
    let on_vertical = (vx, vy + sy * r);
    if !round {
        let middle = if inverted { (vx + sx * r, vy + sy * r) } else { (vx, vy) };
        return vec![on_horizontal, middle, on_vertical];
    }
    let (circle, start) = if inverted {
        // Centred on the vertex: the arc bows into the box.
        let circle = PixelBox { x: vx - r, y: vy - r, w: 2.0 * r, h: 2.0 * r };
        (circle, [270.0, 180.0, 90.0, 0.0][index.min(3)])
    } else {
        corner_arc(bx, index, r).0.pipe(|c| (c, [90.0, 0.0, 270.0, 180.0][index.min(3)]))
    };
    arc_points(circle, false, start, 90.0)
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

/// The radius in pixels of each of a border's corners (top-left, top-right,
/// bottom-right, bottom-left; fewer than four reuse the last) that is
/// rounded: `join="round"` with a radius. A square corner is 0, since its
/// right angle closes the edges' trimmed ends (XFA 3.3 "The corner
/// element"). Each radius is capped at half the box's shorter side.
///
/// A rounded corner shapes the region a fill paints even when the corner
/// itself is hidden: Acrobat draws the UBS add and remove buttons, whose
/// corners are `join="round" presence="hidden"`, as rounded buttons.
pub fn rounded_corner_radii(border: &Border, bx: PixelBox, scale: f32) -> [f32; 4] {
    let cap = (bx.w.min(bx.h) / 2.0).max(0.0);
    let mut radii = [0.0; 4];
    for (i, radius) in radii.iter_mut().enumerate() {
        if let Some(corner) = border.get_corner(i)
            && corner.join == JoinStyle::Round
        {
            *radius = (corner.radius.and_then(|r| r.to_f32()).unwrap_or(0.0) * scale).min(cap);
        }
    }
    radii
}

/// Whether the pixel centre (`px`, `py`) lies inside `bx` with its corners
/// rounded by `radii` (top-left, top-right, bottom-right, bottom-left).
pub fn inside_rounded_box(bx: PixelBox, radii: [f32; 4], px: f32, py: f32) -> bool {
    if px < bx.x || py < bx.y || px > bx.x + bx.w || py > bx.y + bx.h {
        return false;
    }
    let centres = [
        (bx.x + radii[0], bx.y + radii[0]),
        (bx.x + bx.w - radii[1], bx.y + radii[1]),
        (bx.x + bx.w - radii[2], bx.y + bx.h - radii[2]),
        (bx.x + radii[3], bx.y + bx.h - radii[3]),
    ];
    for (i, (cx, cy)) in centres.into_iter().enumerate() {
        let r = radii[i];
        if r <= 0.0 {
            continue;
        }
        let outside_x = if i == 0 || i == 3 { px < cx } else { px > cx };
        let outside_y = if i < 2 { py < cy } else { py > cy };
        if outside_x && outside_y && (px - cx).powi(2) + (py - cy).powi(2) > r * r {
            return false;
        }
    }
    true
}

/// The quarter circle a rounded corner `index` (top-left, top-right,
/// bottom-right, bottom-left) of radius `r` follows in `bx`: the box of its
/// full circle, and its start and sweep angles in the arc convention
/// (degrees, counter-clockwise from three o'clock).
pub fn corner_arc(bx: PixelBox, index: usize, r: f32) -> (PixelBox, f32, f32) {
    let d = 2.0 * r;
    let (x, y, start) = match index {
        0 => (bx.x, bx.y, 90.0),
        1 => (bx.x + bx.w - d, bx.y, 0.0),
        2 => (bx.x + bx.w - d, bx.y + bx.h - d, 270.0),
        _ => (bx.x, bx.y + bx.h - d, 180.0),
    };
    (PixelBox { x, y, w: d, h: d }, start, 90.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `xml` parsed as the content of a `<value>`, as a template carries it.
    fn shape_node(xml: &str) -> XfaNode {
        let nodes = XfaNode::parse(format!("<value>{xml}</value>").as_bytes()).expect("xml");
        nodes[0].children[0].clone()
    }

    fn parse_one(xml: &str) -> Shape {
        Shape::parse(&shape_node(xml)).expect("parse").expect("a shape")
    }

    const BOX: PixelBox = PixelBox {
        x: 10.0,
        y: 20.0,
        w: 100.0,
        h: 0.0,
    };

    #[test]
    fn a_line_without_an_edge_has_the_default_edge() {
        let Shape::Line(line) = parse_one("<line/>") else {
            panic!("a line")
        };
        assert_eq!(line.slope, Slope::Down);
        assert_eq!(line.hand, Hand::Even);
        assert_eq!(line.edge, default_edge());
    }

    #[test]
    fn a_rectangle_keeps_its_edges_corners_and_fill() {
        let Shape::Rectangle(rect) = parse_one(
            r#"<rectangle hand="left"><corner radius="5mm" presence="hidden"/><edge thickness="1pt"/><edge presence="hidden"/><fill><color value="1,2,3"/></fill></rectangle>"#,
        ) else {
            panic!("a rectangle")
        };
        assert_eq!(rect.hand, Hand::Left);
        assert_eq!(rect.edges.len(), 2);
        assert_eq!(rect.edges[0].thickness, Some(num(1.0)));
        assert_eq!(rect.edges[1].thickness, Some(num(DEFAULT_EDGE_THICKNESS_PT)));
        assert!(!is_drawn(&rect.edges[1].presence));
        assert!(!is_drawn(&rect.corners[0].presence));
        assert_eq!(rect.fill.and_then(|f| f.color), Some((1, 2, 3)));
    }

    #[test]
    fn an_arc_defaults_to_a_full_ellipse_and_rejects_a_bad_angle() {
        let Shape::Arc(arc) = parse_one("<arc/>") else {
            panic!("an arc")
        };
        assert_eq!((arc.start_angle, arc.sweep_angle, arc.circular), (0.0, 360.0, false));
        assert!(Shape::parse(&shape_node(r#"<arc sweepAngle="wide"/>"#)).is_err());
    }

    #[test]
    fn a_zero_height_line_is_a_horizontal_rule_and_hand_moves_it() {
        let even = line_centre(Slope::Down, Hand::Even, BOX, 2.0).unwrap();
        assert_eq!(even, [(10.0, 20.0), (110.0, 20.0)]);
        let left = line_centre(Slope::Down, Hand::Left, BOX, 2.0).unwrap();
        assert_eq!(left, [(10.0, 19.0), (110.0, 19.0)], "left of eastward is up");
        let right = line_centre(Slope::Down, Hand::Right, BOX, 2.0).unwrap();
        assert_eq!(right, [(10.0, 21.0), (110.0, 21.0)]);
    }

    #[test]
    fn slopes_run_corner_to_corner() {
        let bx = PixelBox { x: 0.0, y: 0.0, w: 10.0, h: 20.0 };
        assert_eq!(
            line_centre(Slope::Down, Hand::Even, bx, 1.0).unwrap(),
            [(0.0, 0.0), (10.0, 20.0)]
        );
        assert_eq!(
            line_centre(Slope::Up, Hand::Even, bx, 1.0).unwrap(),
            [(0.0, 20.0), (10.0, 0.0)]
        );
        assert_eq!(
            line_centre(Slope::Down, Hand::Even, PixelBox { w: 0.0, h: 0.0, ..bx }, 1.0),
            None
        );
    }

    #[test]
    fn an_axis_aligned_stroke_is_one_rectangle() {
        assert_eq!(
            stroke_segment((10.0, 20.0), (110.0, 20.0), 2.0),
            vec![PixelRect { x: 10, y: 19, w: 100, h: 2 }]
        );
        assert_eq!(
            stroke_segment((5.0, 0.0), (5.0, 30.0), 1.0),
            vec![PixelRect { x: 5, y: 0, w: 1, h: 30 }],
            "a hairline is one pixel wide"
        );
    }

    #[test]
    fn a_diagonal_stroke_covers_both_ends() {
        let rects = stroke_segment((0.0, 0.0), (10.0, 10.0), 1.0);
        assert_eq!(rects.first(), Some(&PixelRect { x: 0, y: 0, w: 1, h: 1 }));
        assert_eq!(rects.last(), Some(&PixelRect { x: 10, y: 10, w: 1, h: 1 }));
    }

    #[test]
    fn a_rectangles_hand_puts_the_stroke_outside_on_or_inside_its_path() {
        let bx = PixelBox { x: 10.0, y: 10.0, w: 20.0, h: 20.0 };
        assert_eq!(rectangle_stroke_box(Hand::Right, bx, 4.0), bx);
        assert_eq!(
            rectangle_stroke_box(Hand::Even, bx, 4.0),
            PixelBox { x: 8.0, y: 8.0, w: 24.0, h: 24.0 }
        );
        assert_eq!(
            rectangle_stroke_box(Hand::Left, bx, 4.0),
            PixelBox { x: 6.0, y: 6.0, w: 28.0, h: 28.0 }
        );
    }

    #[test]
    fn an_arcs_hand_is_the_reverse_of_a_rectangles() {
        let bx = PixelBox { x: 10.0, y: 10.0, w: 20.0, h: 20.0 };
        assert_eq!(arc_stroke_box(Hand::Even, bx, 4.0), bx);
        assert_eq!(
            arc_stroke_box(Hand::Left, bx, 4.0),
            PixelBox { x: 12.0, y: 12.0, w: 16.0, h: 16.0 },
            "left is inside a counter-clockwise path"
        );
        assert_eq!(
            arc_stroke_box(Hand::Right, bx, 4.0),
            PixelBox { x: 8.0, y: 8.0, w: 24.0, h: 24.0 }
        );
    }

    #[test]
    fn arc_points_run_counter_clockwise_from_three_o_clock() {
        let bx = PixelBox { x: 0.0, y: 0.0, w: 20.0, h: 20.0 };
        let points = arc_points(bx, false, 0.0, 90.0);
        let first = points[0];
        let last = *points.last().unwrap();
        assert!((first.0 - 20.0).abs() < 1e-3 && (first.1 - 10.0).abs() < 1e-3, "{first:?}");
        assert!((last.0 - 10.0).abs() < 1e-3 && last.1.abs() < 1e-3, "twelve o'clock is up: {last:?}");
    }

    #[test]
    fn a_circular_arc_uses_the_smaller_dimension() {
        let bx = PixelBox { x: 0.0, y: 0.0, w: 40.0, h: 20.0 };
        assert!(inside_ellipse(bx, true, 20.0, 10.0));
        assert!(!inside_ellipse(bx, true, 2.0, 10.0), "outside the 10px circle");
        assert!(inside_ellipse(bx, false, 2.0, 10.0), "inside the full-width ellipse");
    }

    fn fill(color: (u8, u8, u8), linear: Option<(GradientDirection, (u8, u8, u8))>) -> Fill {
        Fill {
            color: Some(color),
            presence: "visible".to_string(),
            kind: linear
                .map(|(direction, end)| FillKind::Linear(super::super::LinearGradient { direction, end }))
                .unwrap_or_default(),
        }
    }

    #[test]
    fn a_linear_fill_runs_from_its_color_to_the_end_color() {
        let bx = PixelBox { x: 0.0, y: 0.0, w: 11.0, h: 3.0 };
        let down = fill((200, 100, 0), Some((GradientDirection::ToBottom, (0, 0, 0))));
        assert_eq!(fill_color_at(&down, bx, 5.0, 0.0), (200, 100, 0), "start at the top");
        assert_eq!(fill_color_at(&down, bx, 5.0, 2.0), (0, 0, 0), "end at the bottom");
        assert_eq!(fill_color_at(&down, bx, 5.0, 1.0), (100, 50, 0));
        let left = fill((0, 0, 0), Some((GradientDirection::ToLeft, (250, 250, 250))));
        assert_eq!(fill_color_at(&left, bx, 10.0, 1.0), (0, 0, 0), "start at the right");
        assert_eq!(fill_color_at(&fill((1, 2, 3), None), bx, 4.0, 1.0), (1, 2, 3));
    }

    #[test]
    fn only_a_round_join_rounds_a_corner_and_the_last_corner_repeats() {
        let border = Border {
            corners: vec![
                Corner { join: JoinStyle::Round, radius: Some(num(2.0)), ..Corner::default() },
                Corner { join: JoinStyle::Square, radius: Some(num(2.0)), ..Corner::default() },
            ],
            ..Border::default()
        };
        let bx = PixelBox { x: 0.0, y: 0.0, w: 100.0, h: 10.0 };
        assert_eq!(rounded_corner_radii(&border, bx, 2.0), [4.0, 0.0, 0.0, 0.0]);
        let round = Border {
            corners: vec![Corner { join: JoinStyle::Round, radius: Some(num(50.0)), ..Corner::default() }],
            ..Border::default()
        };
        assert_eq!(rounded_corner_radii(&round, bx, 1.0), [5.0; 4], "capped at half the height");
    }

    #[test]
    fn a_rounded_box_excludes_only_its_corner_cut_outs() {
        let bx = PixelBox { x: 0.0, y: 0.0, w: 20.0, h: 20.0 };
        let radii = [5.0, 0.0, 5.0, 0.0];
        assert!(!inside_rounded_box(bx, radii, 0.5, 0.5), "rounded top-left");
        assert!(inside_rounded_box(bx, radii, 19.5, 0.5), "square top-right");
        assert!(!inside_rounded_box(bx, radii, 19.5, 19.5), "rounded bottom-right");
        assert!(inside_rounded_box(bx, radii, 10.0, 10.0));
        assert!(inside_rounded_box(bx, radii, 5.0, 0.5), "the straight part of the top");
    }

    #[test]
    fn corner_arcs_bend_toward_the_box() {
        let bx = PixelBox { x: 0.0, y: 0.0, w: 20.0, h: 10.0 };
        let (circle, start, sweep) = corner_arc(bx, 1, 3.0);
        assert_eq!((circle, start, sweep), (PixelBox { x: 14.0, y: 0.0, w: 6.0, h: 6.0 }, 0.0, 90.0));
        let points = arc_points(circle, false, start, sweep);
        assert!((points[0].0 - 20.0).abs() < 1e-3, "starts on the right edge");
        assert!(points.last().unwrap().1.abs() < 1e-3, "ends on the top edge");
    }

    #[test]
    fn thickness_is_at_least_one_pixel() {
        assert_eq!(thickness_px(Some(num(0.09)), 2.0), 1.0);
        assert_eq!(thickness_px(None, 4.0), 2.0);
    }
}

#[cfg(test)]
mod polygon_tests {
    use super::*;

    #[test]
    fn a_square_fills_its_rows_and_a_diamond_narrows_towards_its_tips() {
        let square = polygon_fill(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        assert_eq!(square.len(), 4);
        assert!(square.iter().all(|r| r.x == 0 && r.w == 4 && r.h == 1));

        let diamond = polygon_fill(&[(4.0, 0.0), (8.0, 4.0), (4.0, 8.0), (0.0, 4.0)]);
        let widths: Vec<i32> = diamond.iter().map(|r| r.w).collect();
        assert_eq!(widths.len(), 8);
        assert!(widths[0] < widths[3] && widths[7] < widths[4], "{widths:?}");

        assert!(polygon_fill(&[(0.0, 0.0), (1.0, 1.0)]).is_empty());
    }
}
