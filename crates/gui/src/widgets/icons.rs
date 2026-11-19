//! Stroke icons drawn with the painter (components.md, Section icon tile).
//! There is no icon font: each icon is a few lines, curves, and circles on
//! its own square grid, scaled to the size it is drawn at. The shapes are the
//! ones of the mockups ("Combined"), so a new section adds one entry here,
//! its own shape, and a color in `theme`.
//!
//! The coordinates below are drawing data on the icon's own grid, not sizes
//! of the window: `theme` holds every size of the window and the stroke
//! widths (`Size::icon_stroke`). The literal check of `theme` skips this
//! file's numbers for that reason and still looks for colors in it.

use std::f32::consts::PI;

use eframe::egui::epaint::{CircleShape, PathShape, PathStroke, RectShape};
use eframe::egui::{
    Color32, CornerRadius, Painter, Pos2, Rect, Response, Role, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2, Widget, WidgetInfo, pos2, vec2,
};

use crate::theme::{Direction, Section, Theme};

/// One piece of an icon, on the icon's grid. Lines and curves continue from
/// the current point, like the path of an SVG.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Starts a new line.
    Move(f32, f32),
    /// A straight line to a point.
    Line(f32, f32),
    /// A cubic curve: two control points, then the end point.
    Curve(f32, f32, f32, f32, f32, f32),
    /// An arc of an ellipse, from one angle to another in degrees: 0 is to
    /// the right, 90 is down. Starts a new line when none is open.
    Arc {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        from: f32,
        to: f32,
    },
    /// Draws the line closed.
    Close,
    /// A circle outline: centre, radius.
    Circle(f32, f32, f32),
    /// An ellipse outline: centre, the two radii.
    Ellipse(f32, f32, f32, f32),
    /// A rounded rectangle outline: left, top, width, height, corner.
    RoundRect(f32, f32, f32, f32, f32),
    /// A filled dot: centre, radius.
    Dot(f32, f32, f32),
}

use Step::{Arc, Circle, Close, Curve, Dot, Ellipse, Line, Move, RoundRect};

/// An icon of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    // The sections, each with its own shape (R8).
    Home,
    Sites,
    Runtimes,
    Services,
    Mail,
    Dumps,
    Logs,
    Databases,
    Queues,
    // The sidebar's other items.
    Settings,
    Fold,
    // States: always with a color and a word (R8).
    Check,
    Cross,
    Dash,
    Ring,
    Alert,
    /// A busy mark: a ring with a gap. Still, because nothing animates on its
    /// own (rules.md 10).
    Spinner,
    // Marks of controls and rows.
    Search,
    Plus,
    More,
    ChevronDown,
    Lock,
    LockOpen,
    /// Points to the end of a line: right in a left-to-right language.
    Arrow,
    /// Points to the start of a line: left in a left-to-right language. The
    /// way back.
    Back,
}

impl Icon {
    pub const ALL: [Self; 25] = [
        Self::Home,
        Self::Sites,
        Self::Runtimes,
        Self::Services,
        Self::Mail,
        Self::Dumps,
        Self::Logs,
        Self::Databases,
        Self::Queues,
        Self::Settings,
        Self::Fold,
        Self::Check,
        Self::Cross,
        Self::Dash,
        Self::Ring,
        Self::Alert,
        Self::Spinner,
        Self::Search,
        Self::Plus,
        Self::More,
        Self::ChevronDown,
        Self::Lock,
        Self::LockOpen,
        Self::Arrow,
        Self::Back,
    ];

    /// The icon of a section.
    pub const fn of(section: Section) -> Self {
        match section {
            Section::Home => Self::Home,
            Section::Sites => Self::Sites,
            Section::Runtimes => Self::Runtimes,
            Section::Services => Self::Services,
            Section::Mail => Self::Mail,
            Section::Dumps => Self::Dumps,
            Section::Logs => Self::Logs,
            Section::Databases => Self::Databases,
            Section::Queues => Self::Queues,
        }
    }

    /// The side of the square grid the steps are drawn on.
    pub const fn grid(self) -> f32 {
        match self {
            Self::Check
            | Self::Cross
            | Self::Dash
            | Self::Ring
            | Self::Spinner
            | Self::Arrow
            | Self::Back => 12.0,
            Self::ChevronDown => 10.0,
            Self::More => 14.0,
            Self::Plus => 20.0,
            _ => 16.0,
        }
    }

    /// Status marks are drawn heavier than the other icons.
    const fn bold(self) -> bool {
        matches!(
            self,
            Self::Check | Self::Cross | Self::Dash | Self::Ring | Self::Spinner
        )
    }

    /// An icon that points along the line of text is mirrored in a
    /// right-to-left language (rules.md 13).
    const fn follows_direction(self) -> bool {
        matches!(self, Self::Arrow | Self::Back)
    }

    /// The width of the stroke at `size`, from the tokens.
    pub fn stroke_width(self, theme: &Theme, size: f32) -> f32 {
        let share = if self.bold() {
            theme.size.icon_stroke_bold
        } else {
            theme.size.icon_stroke
        };
        share * size
    }

    pub const fn steps(self) -> &'static [Step] {
        match self {
            Self::Home => &[
                Move(2.5, 7.5),
                Line(8.0, 3.0),
                Line(13.5, 7.5),
                Line(13.5, 13.0),
                Line(10.0, 13.0),
                Line(10.0, 9.5),
                Line(6.0, 9.5),
                Line(6.0, 13.0),
                Line(2.5, 13.0),
                Close,
            ],
            Self::Sites => &[
                Move(2.0, 4.5),
                Line(14.0, 4.5),
                Line(14.0, 12.5),
                Line(2.0, 12.5),
                Close,
                Move(2.0, 7.0),
                Line(14.0, 7.0),
            ],
            Self::Runtimes => &[
                Move(8.0, 2.0),
                Line(14.0, 5.0),
                Line(8.0, 8.0),
                Line(2.0, 5.0),
                Close,
                Move(2.0, 8.0),
                Line(8.0, 11.0),
                Line(14.0, 8.0),
                Move(2.0, 11.0),
                Line(8.0, 14.0),
                Line(14.0, 11.0),
            ],
            Self::Services => &[
                Move(8.0, 2.0),
                Line(8.0, 5.0),
                Move(8.0, 11.0),
                Line(8.0, 14.0),
                Move(2.0, 8.0),
                Line(5.0, 8.0),
                Move(11.0, 8.0),
                Line(14.0, 8.0),
                Circle(8.0, 8.0, 2.5),
            ],
            Self::Mail => &[
                Move(2.0, 4.0),
                Line(14.0, 4.0),
                Line(14.0, 12.0),
                Line(2.0, 12.0),
                Close,
                Move(2.0, 4.0),
                Line(8.0, 9.0),
                Line(14.0, 4.0),
            ],
            // Curly braces: a dump of data.
            Self::Dumps => &[
                Move(5.5, 2.5),
                Curve(4.0, 2.5, 3.5, 3.2, 3.5, 4.5),
                Line(3.5, 6.0),
                Curve(3.5, 6.8, 3.0, 7.5, 2.0, 7.5),
                Line(2.0, 8.5),
                Curve(3.0, 8.5, 3.5, 9.2, 3.5, 10.0),
                Line(3.5, 11.5),
                Curve(3.5, 12.8, 4.0, 13.5, 5.5, 13.5),
                Move(10.5, 2.5),
                Curve(12.0, 2.5, 12.5, 3.2, 12.5, 4.5),
                Line(12.5, 6.0),
                Curve(12.5, 6.8, 13.0, 7.5, 14.0, 7.5),
                Line(14.0, 8.5),
                Curve(13.0, 8.5, 12.5, 9.2, 12.5, 10.0),
                Line(12.5, 11.5),
                Curve(12.5, 12.8, 12.0, 13.5, 10.5, 13.5),
            ],
            Self::Logs => &[
                Move(3.0, 3.0),
                Line(13.0, 3.0),
                Line(13.0, 13.0),
                Line(3.0, 13.0),
                Close,
                Move(5.5, 6.0),
                Line(10.5, 6.0),
                Move(5.5, 8.5),
                Line(10.5, 8.5),
                Move(5.5, 11.0),
                Line(8.5, 11.0),
            ],
            // A cylinder: the top, the two sides with the bottom, and a
            // middle ring.
            Self::Databases => &[
                Ellipse(8.0, 4.0, 5.0, 2.0),
                Move(3.0, 4.0),
                Line(3.0, 12.0),
                Arc {
                    cx: 8.0,
                    cy: 12.0,
                    rx: 5.0,
                    ry: 2.0,
                    from: 180.0,
                    to: 0.0,
                },
                Line(13.0, 4.0),
                Move(3.0, 8.0),
                Arc {
                    cx: 8.0,
                    cy: 8.0,
                    rx: 5.0,
                    ry: 2.0,
                    from: 180.0,
                    to: 0.0,
                },
            ],
            Self::Queues => &[
                Move(3.0, 4.0),
                Line(13.0, 4.0),
                Move(3.0, 8.0),
                Line(13.0, 8.0),
                Move(3.0, 12.0),
                Line(9.0, 12.0),
            ],
            // A cog: a body with 6 square teeth, each 2.4 wide with
            // parallel sides, from radius 4.5 out to 6.6, and a hole of 2.1.
            // Six teeth leave gaps that stay open at 15 px. Not a sun, which
            // would read as the light appearance.
            Self::Settings => &[
                Move(12.36, 9.13),
                Line(14.32, 10.26),
                Line(13.12, 12.34),
                Line(11.16, 11.21),
                Line(9.2, 12.34),
                Line(9.2, 14.6),
                Line(6.8, 14.6),
                Line(6.8, 12.34),
                Line(4.84, 11.21),
                Line(2.88, 12.34),
                Line(1.68, 10.26),
                Line(3.64, 9.13),
                Line(3.64, 6.87),
                Line(1.68, 5.74),
                Line(2.88, 3.66),
                Line(4.84, 4.79),
                Line(6.8, 3.66),
                Line(6.8, 1.4),
                Line(9.2, 1.4),
                Line(9.2, 3.66),
                Line(11.16, 4.79),
                Line(13.12, 3.66),
                Line(14.32, 5.74),
                Line(12.36, 6.87),
                Close,
                Circle(8.0, 8.0, 2.1),
            ],
            // A window with its sidebar: the fold button.
            Self::Fold => &[
                RoundRect(2.0, 3.0, 12.0, 10.0, 2.0),
                Move(6.0, 3.0),
                Line(6.0, 13.0),
            ],
            Self::Check => &[Move(2.8, 6.2), Line(5.0, 8.4), Line(9.2, 3.8)],
            Self::Cross => &[
                Move(3.5, 3.5),
                Line(8.5, 8.5),
                Move(8.5, 3.5),
                Line(3.5, 8.5),
            ],
            Self::Dash => &[Move(3.0, 6.0), Line(9.0, 6.0)],
            Self::Ring => &[Circle(6.0, 6.0, 4.0)],
            // Three quarters of a circle, open at the top right.
            Self::Spinner => &[Arc {
                cx: 6.0,
                cy: 6.0,
                rx: 4.2,
                ry: 4.2,
                from: 0.0,
                to: 270.0,
            }],
            Self::Alert => &[
                Circle(8.0, 8.0, 6.0),
                Move(8.0, 4.8),
                Line(8.0, 8.6),
                Dot(8.0, 11.0, 0.9),
            ],
            Self::Search => &[Circle(7.0, 7.0, 4.5), Move(10.5, 10.5), Line(14.0, 14.0)],
            Self::Plus => &[
                Move(10.0, 4.0),
                Line(10.0, 16.0),
                Move(4.0, 10.0),
                Line(16.0, 10.0),
            ],
            Self::More => &[Dot(3.0, 7.0, 1.2), Dot(7.0, 7.0, 1.2), Dot(11.0, 7.0, 1.2)],
            Self::ChevronDown => &[Move(2.0, 4.0), Line(5.0, 7.0), Line(8.0, 4.0)],
            Self::Arrow => &[
                Move(2.0, 6.0),
                Line(10.0, 6.0),
                Move(6.5, 2.5),
                Line(10.0, 6.0),
                Line(6.5, 9.5),
            ],
            Self::Back => &[
                Move(10.0, 6.0),
                Line(2.0, 6.0),
                Move(5.5, 2.5),
                Line(2.0, 6.0),
                Line(5.5, 9.5),
            ],
            Self::Lock => &[
                RoundRect(3.0, 7.0, 10.0, 7.0, 1.5),
                Move(5.5, 7.0),
                Line(5.5, 5.0),
                Arc {
                    cx: 8.0,
                    cy: 5.0,
                    rx: 2.5,
                    ry: 2.5,
                    from: 180.0,
                    to: 360.0,
                },
                Line(10.5, 7.0),
            ],
            // The shackle's right leg is up and short: the lock is open.
            Self::LockOpen => &[
                RoundRect(3.0, 7.0, 10.0, 7.0, 1.5),
                Move(5.5, 7.0),
                Line(5.5, 5.0),
                Arc {
                    cx: 8.0,
                    cy: 5.0,
                    rx: 2.5,
                    ry: 2.5,
                    from: 180.0,
                    to: 360.0,
                },
            ],
        }
    }

    /// The shapes of this icon in `rect`, scaled to fit its smaller side and
    /// centred. Pure: nothing is painted until [`Icon::paint`].
    pub fn shapes(self, rect: Rect, color: Color32, stroke_width: f32) -> Vec<Shape> {
        self.shapes_in(Direction::CURRENT, rect, color, stroke_width)
    }

    /// [`Icon::shapes`] for a window that reads in `direction`.
    fn shapes_in(
        self,
        direction: Direction,
        rect: Rect,
        color: Color32,
        stroke_width: f32,
    ) -> Vec<Shape> {
        let side = rect.width().min(rect.height());
        let scale = side / self.grid();
        let origin = Rect::from_center_size(rect.center(), Vec2::splat(side)).min;
        let mirror = self.follows_direction() && direction.is_rtl();
        let grid = self.grid();
        let at = |x: f32, y: f32| {
            let x = if mirror { grid - x } else { x };
            origin + vec2(x, y) * scale
        };
        let stroke = Stroke::new(stroke_width, color);
        let mut shapes = Vec::new();
        let mut line: Vec<Pos2> = Vec::new();
        for step in self.steps() {
            match *step {
                Move(x, y) => {
                    flush(&mut shapes, &mut line, stroke, false);
                    line.push(at(x, y));
                }
                Line(x, y) => line.push(at(x, y)),
                Curve(x1, y1, x2, y2, x, y) => {
                    let from = line.last().copied().unwrap_or_else(|| at(x, y));
                    let (c1, c2, to) = (at(x1, y1), at(x2, y2), at(x, y));
                    for index in 1..=CURVE_POINTS {
                        let t = index as f32 / CURVE_POINTS as f32;
                        line.push(cubic(from, c1, c2, to, t));
                    }
                }
                Arc {
                    cx,
                    cy,
                    rx,
                    ry,
                    from,
                    to,
                } => {
                    let count =
                        (((to - from).abs() / ARC_DEGREES_PER_POINT).ceil() as usize).max(2);
                    for index in 0..=count {
                        let t = index as f32 / count as f32;
                        let angle = (from + (to - from) * t) * PI / 180.0;
                        line.push(at(cx + rx * angle.cos(), cy + ry * angle.sin()));
                    }
                }
                Close => flush(&mut shapes, &mut line, stroke, true),
                Circle(cx, cy, r) => {
                    flush(&mut shapes, &mut line, stroke, false);
                    shapes.push(Shape::Circle(CircleShape::stroke(
                        at(cx, cy),
                        r * scale,
                        stroke,
                    )));
                }
                Ellipse(cx, cy, rx, ry) => {
                    flush(&mut shapes, &mut line, stroke, false);
                    let ring = (0..ELLIPSE_POINTS)
                        .map(|index| {
                            let angle = index as f32 / ELLIPSE_POINTS as f32 * 2.0 * PI;
                            at(cx + rx * angle.cos(), cy + ry * angle.sin())
                        })
                        .collect();
                    shapes.push(Shape::closed_line(ring, stroke));
                }
                RoundRect(x, y, w, h, r) => {
                    flush(&mut shapes, &mut line, stroke, false);
                    let rect = Rect::from_min_size(at(x, y), vec2(w, h) * scale);
                    // Rounded to the whole points egui draws corners in.
                    let corner = CornerRadius::same((r * scale).round() as u8);
                    shapes.push(Shape::Rect(RectShape::stroke(
                        rect,
                        corner,
                        stroke,
                        StrokeKind::Middle,
                    )));
                }
                Dot(cx, cy, r) => {
                    flush(&mut shapes, &mut line, stroke, false);
                    shapes.push(Shape::circle_filled(at(cx, cy), r * scale, color));
                }
            }
        }
        flush(&mut shapes, &mut line, stroke, false);
        shapes
    }

    /// Draws the icon in `rect` with its stroke from the tokens.
    pub fn paint(self, painter: &Painter, theme: &Theme, rect: Rect, color: Color32) {
        let width = self.stroke_width(theme, rect.width().min(rect.height()));
        painter.extend(self.shapes(rect, color, width));
    }
}

/// How many straight pieces a curve is drawn with.
const CURVE_POINTS: usize = 10;
/// An arc gets one point for each of these degrees.
const ARC_DEGREES_PER_POINT: f32 = 12.0;
/// How many points an ellipse outline has.
const ELLIPSE_POINTS: usize = 28;

/// Ends the line being built: closed or open. An open line gets round caps,
/// as the mockups' `stroke-linecap: round`.
fn flush(shapes: &mut Vec<Shape>, line: &mut Vec<Pos2>, stroke: Stroke, closed: bool) {
    let points = std::mem::take(line);
    let (Some(first), Some(last)) = (points.first().copied(), points.last().copied()) else {
        return;
    };
    if closed {
        shapes.push(Shape::Path(PathShape {
            points,
            closed: true,
            fill: Color32::TRANSPARENT,
            stroke: PathStroke::from(stroke),
        }));
        return;
    }
    let radius = stroke.width / 2.0;
    shapes.push(Shape::line(points, stroke));
    for end in [first, last] {
        shapes.push(Shape::circle_filled(end, radius, stroke.color));
    }
}

fn cubic(p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2, t: f32) -> Pos2 {
    let u = 1.0 - t;
    let weights = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    let x = weights[0] * p0.x + weights[1] * p1.x + weights[2] * p2.x + weights[3] * p3.x;
    let y = weights[0] * p0.y + weights[1] * p1.y + weights[2] * p2.y + weights[3] * p3.y;
    pos2(x, y)
}

/// An icon on its own, as a widget. With no label it is decoration, and a
/// screen reader skips it; give it one when it is the only thing that says
/// what a control or a state is.
#[derive(Clone, Debug)]
pub struct Glyph<'a> {
    icon: Icon,
    size: Option<f32>,
    color: Option<Color32>,
    label: Option<&'a str>,
}

impl<'a> Glyph<'a> {
    pub const fn new(icon: Icon) -> Self {
        Self {
            icon,
            size: None,
            color: None,
            label: None,
        }
    }

    /// The side of its square; `size.icon_inline` when not set.
    pub const fn size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }

    /// Its color; `ink` when not set.
    pub const fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// What a screen reader says for it.
    pub const fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
}

impl Widget for Glyph<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let size = self.size.unwrap_or(theme.size.icon_inline);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        if ui.is_rect_visible(rect) {
            let color = self.color.unwrap_or(theme.color.ink);
            self.icon.paint(ui.painter(), theme, rect, color);
        }
        let label = self.label;
        response.widget_info(|| match label {
            Some(label) => WidgetInfo::labeled(Role::Image, true, label),
            None => WidgetInfo::new(Role::GenericContainer),
        });
        response
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use eframe::egui;

    use super::*;
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[test]
    fn every_section_has_its_own_icon_shape() {
        // R8: color is never the only clue.
        let icons: HashSet<Icon> = Section::ALL.iter().map(|s| Icon::of(*s)).collect();
        assert_eq!(icons.len(), Section::ALL.len());
    }

    #[test]
    fn no_two_icons_share_their_steps() {
        for (index, a) in Icon::ALL.iter().enumerate() {
            for b in &Icon::ALL[index + 1..] {
                assert_ne!(a.steps(), b.steps(), "{a:?} and {b:?} look the same");
            }
        }
        let all: HashSet<Icon> = Icon::ALL.into_iter().collect();
        assert_eq!(all.len(), Icon::ALL.len());
    }

    /// The farthest coordinate of a step, the stroke aside.
    fn extent(step: &Step) -> f32 {
        match *step {
            Move(x, y) | Line(x, y) => x.max(y),
            Curve(a, b, c, d, e, f) => [a, b, c, d, e, f].into_iter().fold(0.0, f32::max),
            Arc { cx, cy, rx, ry, .. } => (cx + rx).max(cy + ry),
            Circle(cx, cy, r) | Dot(cx, cy, r) => (cx + r).max(cy + r),
            Ellipse(cx, cy, rx, ry) => (cx + rx).max(cy + ry),
            RoundRect(x, y, w, h, _) => (x + w).max(y + h),
            Close => 0.0,
        }
    }

    #[test]
    fn every_icon_fits_its_grid() {
        for icon in Icon::ALL {
            let steps = icon.steps();
            assert!(!steps.is_empty(), "{icon:?} has no steps");
            let farthest = steps.iter().map(extent).fold(0.0, f32::max);
            assert!(
                farthest <= icon.grid(),
                "{icon:?} reaches {farthest} on a grid of {}",
                icon.grid()
            );
        }
    }

    #[test]
    fn shapes_stay_inside_the_rect_they_are_drawn_in() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), Vec2::splat(15.0));
        // The stroke and the round caps reach this far past the geometry.
        let reach = Icon::Check.stroke_width(&LIGHT, 15.0);
        for icon in Icon::ALL {
            let width = icon.stroke_width(&LIGHT, 15.0);
            for shape in icon.shapes(rect, Color32::BLACK, width) {
                let bounds = shape.visual_bounding_rect();
                assert!(
                    rect.expand(reach).contains_rect(bounds),
                    "{icon:?}: {bounds:?} is outside {rect:?}"
                );
            }
        }
    }

    #[test]
    fn an_icon_is_centred_in_a_rect_that_is_not_square() {
        let wide = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 16.0));
        let shapes = Icon::Plus.shapes(wide, Color32::BLACK, 1.0);
        let bounds = shapes
            .iter()
            .map(Shape::visual_bounding_rect)
            .fold(Rect::NOTHING, Rect::union);
        // The plus is symmetric: its centre is the centre of the rect.
        assert!((bounds.center().x - wide.center().x).abs() < 0.01);
        assert!((bounds.center().y - wide.center().y).abs() < 0.01);
        assert!(bounds.height() <= 16.0 + 0.01);
    }

    #[test]
    fn a_closed_icon_has_a_closed_path_and_an_open_one_has_caps() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), Vec2::splat(16.0));
        let home = Icon::Home.shapes(rect, Color32::BLACK, 1.5);
        assert_eq!(home.len(), 1);
        assert!(matches!(&home[0], Shape::Path(path) if path.closed));
        // A line and one round cap at each end.
        let dash = Icon::Dash.shapes(rect, Color32::BLACK, 1.5);
        assert_eq!(dash.len(), 3);
    }

    #[test]
    fn settings_is_a_cog_with_six_square_teeth_and_a_hole() {
        let steps = Icon::Settings.steps();
        let centre = pos2(8.0, 8.0);
        let corners: Vec<Pos2> = steps
            .iter()
            .filter_map(|step| match *step {
                Move(x, y) | Line(x, y) => Some(pos2(x, y)),
                _ => None,
            })
            .collect();
        // Each tooth has two outer corners, past the body's radius.
        let outer: Vec<Pos2> = corners
            .iter()
            .copied()
            .filter(|corner| corner.distance(centre) > 5.5)
            .collect();
        assert_eq!(outer.len(), 2 * 6, "six teeth");
        // Square teeth: the two outer corners of a tooth are as far apart
        // as its two corners on the body, so its sides are parallel.
        for tooth in corners.chunks(4) {
            let base = tooth[0].distance(tooth[3]);
            let tip = tooth[1].distance(tooth[2]);
            assert!((base - tip).abs() < 0.05, "{tooth:?}");
        }
        assert!(steps.contains(&Close), "one closed outline");
        assert!(steps.contains(&Circle(8.0, 8.0, 2.1)), "the hole");
        // The same stroke as the other section and sidebar icons.
        assert_eq!(
            Icon::Settings.stroke_width(&LIGHT, 16.0),
            Icon::Home.stroke_width(&LIGHT, 16.0)
        );
    }

    #[test]
    fn status_icons_are_drawn_heavier_than_section_icons() {
        for theme in [&LIGHT, &DARK] {
            let check = Icon::Check.stroke_width(theme, 12.0);
            let mail = Icon::Mail.stroke_width(theme, 12.0);
            assert!(check > mail);
            // 1.5 on the 16 grid, 1.9 on the 12 grid.
            assert!((Icon::Mail.stroke_width(theme, 16.0) - 1.5).abs() < 1e-4);
            assert!((Icon::Check.stroke_width(theme, 12.0) - 1.9).abs() < 1e-4);
        }
    }

    #[test]
    fn an_arrow_points_to_the_end_of_the_line_and_mirrors_in_right_to_left() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), Vec2::splat(12.0));
        // The first point of the shaft: its tail.
        let tail = |direction| {
            let shapes = Icon::Arrow.shapes_in(direction, rect, Color32::BLACK, 1.0);
            let Some(Shape::Path(path)) = shapes.first() else {
                panic!("the shaft is a path");
            };
            path.points[0].x
        };
        assert!(tail(Direction::Ltr) < rect.center().x);
        assert!(tail(Direction::Rtl) > rect.center().x);
        // The way back points the other way: to the start of the line.
        let back = |direction| {
            let shapes = Icon::Back.shapes_in(direction, rect, Color32::BLACK, 1.0);
            let Some(Shape::Path(path)) = shapes.first() else {
                panic!("the shaft is a path");
            };
            path.points[0].x
        };
        assert!(back(Direction::Ltr) > rect.center().x);
        assert!(back(Direction::Rtl) < rect.center().x);
        // An icon of an object does not mirror.
        let lock = |direction| Icon::Lock.shapes_in(direction, rect, Color32::BLACK, 1.0);
        assert_eq!(lock(Direction::Ltr).len(), lock(Direction::Rtl).len());
        assert_eq!(
            lock(Direction::Ltr)[0].visual_bounding_rect(),
            lock(Direction::Rtl)[0].visual_bounding_rect()
        );
    }

    #[test]
    fn a_glyph_takes_the_size_it_is_given_or_the_inline_size() {
        let mut sizes = Vec::new();
        Harness::new(egui::Theme::Light).run(|ui| {
            sizes.push(ui.add(Glyph::new(Icon::Check)).rect.size());
            sizes.push(ui.add(Glyph::new(Icon::Check).size(20.0)).rect.size());
        });
        assert_eq!(sizes[0], Vec2::splat(LIGHT.size.icon_inline));
        assert_eq!(sizes[1], Vec2::splat(20.0));
    }

    #[test]
    fn a_glyph_with_a_label_is_an_image_for_a_screen_reader() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(Glyph::new(Icon::Check).label("Running"));
        });
        let node = tree.node_with_label("Running").expect("the labeled glyph");
        assert_eq!(node.role(), Role::Image);
    }
}
