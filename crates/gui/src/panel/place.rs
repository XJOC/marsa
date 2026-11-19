//! Where the panel goes: under the menu-bar icon (ADR 0022). The tray knows
//! where its icon is and which monitor holds it, in the physical pixels the
//! system reports; it passes that on the command line ([`Anchor`]). The panel
//! decides the rest, because only it knows how high it is, and that changes
//! when the daemon answers.
//!
//! Pure geometry, so every case is a unit test: the edge of the screen that
//! the icon is nearest (a menu bar on top, a taskbar at the bottom or on a
//! side), the panel on the free side of the icon, kept inside the monitor.
//! Nothing here asks the system for a work area: that needs `unsafe` on
//! Windows. The icon sits in the bar, so the panel next to it, on the side
//! away from the edge, never covers the bar.

use std::fmt;

use eframe::egui::{Align2, Pos2, Rect, Vec2, pos2, vec2};

/// The edge of the screen the icon is nearest: where the bar is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// Why `--icon`, `--monitor`, or `--scale` cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadAnchor(pub &'static str);

impl fmt::Display for BadAnchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for BadAnchor {}

/// The menu-bar icon and its monitor, in physical pixels of the virtual
/// screen, and the scale of that monitor (physical pixels to points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub icon: Rect,
    pub monitor: Rect,
    pub scale: f32,
}

impl Anchor {
    /// The edge of the monitor that the icon's center is nearest. A tie goes
    /// to the top, then the bottom: a menu bar is far more common than a
    /// bar on a side.
    pub fn side(&self) -> Side {
        let at = self.icon.center();
        let monitor = self.monitor;
        [
            (at.y - monitor.top(), Side::Top),
            (monitor.bottom() - at.y, Side::Bottom),
            (at.x - monitor.left(), Side::Left),
            (monitor.right() - at.x, Side::Right),
        ]
        .into_iter()
        .reduce(|best, next| if next.0 < best.0 { next } else { best })
        .map_or(Side::Top, |(_, side)| side)
    }

    /// The most the panel may be high, in physical pixels: the room between
    /// the icon (and `gap`) and the far edge of the monitor (less `edge`), or
    /// the monitor's height less `edge` twice when the bar is on a side.
    pub fn max_height(&self, gap: f32, edge: f32) -> f32 {
        let room = match self.side() {
            Side::Top => self.monitor.bottom() - self.icon.bottom() - gap - edge,
            Side::Bottom => self.icon.top() - self.monitor.top() - gap - edge,
            Side::Left | Side::Right => self.monitor.height() - edge - edge,
        };
        room.max(0.0)
    }

    /// The top-left of a panel `size` big (physical pixels, already no higher
    /// than [`Anchor::max_height`]): on the free side of the icon, `gap` away
    /// from it, and `edge` or more from the edges of the monitor.
    pub fn place(&self, size: Vec2, gap: f32, edge: f32) -> Pos2 {
        let (icon, monitor) = (self.icon, self.monitor);
        // The room the panel takes its place in, and where in it.
        let (room, align) = match self.side() {
            // Under the icon, centered on it.
            Side::Top => (
                Rect::from_min_max(
                    pos2(icon.left(), icon.bottom() + gap),
                    pos2(icon.right(), monitor.bottom()),
                ),
                Align2::CENTER_TOP,
            ),
            // Over the icon, centered on it.
            Side::Bottom => (
                Rect::from_min_max(
                    pos2(icon.left(), monitor.top()),
                    pos2(icon.right(), icon.top() - gap),
                ),
                Align2::CENTER_BOTTOM,
            ),
            // Beside the icon, centered on its middle.
            Side::Left => (
                Rect::from_min_max(
                    pos2(icon.right() + gap, icon.top()),
                    pos2(monitor.right(), icon.bottom()),
                ),
                Align2::LEFT_CENTER,
            ),
            Side::Right => (
                Rect::from_min_max(
                    pos2(monitor.left(), icon.top()),
                    pos2(icon.left() - gap, icon.bottom()),
                ),
                Align2::RIGHT_CENTER,
            ),
        };
        let at = align.align_size_within_rect(size, room).min;
        pos2(
            clamp_within(at.x, monitor.left() + edge, monitor.right() - edge - size.x),
            clamp_within(at.y, monitor.top() + edge, monitor.bottom() - edge - size.y),
        )
    }

    /// The arguments that give this anchor to `marsa-gui --panel`, numbers
    /// only: `--icon x,y,w,h --monitor x,y,w,h --scale s`.
    pub fn to_args(&self) -> [String; 6] {
        let rect = |r: Rect| format!("{},{},{},{}", r.left(), r.top(), r.width(), r.height());
        [
            "--icon".to_owned(),
            rect(self.icon),
            "--monitor".to_owned(),
            rect(self.monitor),
            "--scale".to_owned(),
            self.scale.to_string(),
        ]
    }

    /// The anchor from the text of the three arguments ([`Anchor::to_args`]).
    ///
    /// # Errors
    ///
    /// A number that is missing, not finite, or not text; an icon or a
    /// monitor with no size; a scale that is not between 0 and 16.
    pub fn parse(icon: &str, monitor: &str, scale: &str) -> Result<Self, BadAnchor> {
        let scale: f32 = scale
            .trim()
            .parse()
            .map_err(|_| BadAnchor("--scale is not a number"))?;
        if !(scale.is_finite() && scale > 0.0 && scale <= f32::from(MOST_SCALE)) {
            return Err(BadAnchor("--scale must be more than 0 and at most 16"));
        }
        Ok(Self {
            icon: parse_rect(icon, "--icon needs x,y,width,height, with a size")?,
            monitor: parse_rect(monitor, "--monitor needs x,y,width,height, with a size")?,
            scale,
        })
    }
}

/// The biggest scale a monitor has: more would be a garbage value.
const MOST_SCALE: u8 = 16;

/// `x,y,w,h` as a rect with a size (finite, more than 0 wide and high).
fn parse_rect(text: &str, error: &'static str) -> Result<Rect, BadAnchor> {
    let mut parts = text.split(',').map(|part| part.trim().parse::<f32>());
    let mut next = || parts.next().and_then(Result::ok).filter(|n| n.is_finite());
    let (x, y, width, height) = (next(), next(), next(), next());
    let (Some(x), Some(y), Some(width), Some(height)) = (x, y, width, height) else {
        return Err(BadAnchor(error));
    };
    if parts.next().is_some() || width <= 0.0 || height <= 0.0 {
        return Err(BadAnchor(error));
    }
    Ok(Rect::from_min_size(pos2(x, y), vec2(width, height)))
}

/// `value` between `least` and `most`; `least` when there is no room between
/// them (a panel wider than the monitor starts at the edge).
fn clamp_within(value: f32, least: f32, most: f32) -> f32 {
    if most < least {
        least
    } else {
        value.clamp(least, most)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAP: f32 = 8.0;
    const EDGE: f32 = 16.0;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    /// A 3024 by 1964 monitor at scale 2 (a MacBook), the icon in its menu
    /// bar at `x`.
    fn mac(x: f32) -> Anchor {
        Anchor {
            icon: rect(x, 0.0, 76.0, 68.0),
            monitor: rect(0.0, 0.0, 3024.0, 1964.0),
            scale: 2.0,
        }
    }

    /// A 1920 by 1080 monitor at scale 1 with a taskbar at the bottom.
    fn windows(icon: Rect) -> Anchor {
        Anchor {
            icon,
            monitor: rect(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
        }
    }

    #[test]
    fn under_a_menu_bar_icon_the_panel_is_centered_on_it_just_below() {
        // The spike's measure on macOS: the icon at (2146, 0), 76 by 68 px,
        // scale 2; the panel 360 wide asked for (912, 38) points.
        let anchor = mac(2146.0);
        assert_eq!(anchor.side(), Side::Top);
        let at = anchor.place(vec2(720.0, 1000.0), 8.0, EDGE);
        assert_eq!((at.x / 2.0, at.y / 2.0), (912.0, 38.0));
    }

    #[test]
    fn the_panel_stays_inside_the_monitor_at_the_right_and_left_edges() {
        let size = vec2(720.0, 1000.0);
        // An icon at the far right: the panel ends `EDGE` from the monitor.
        let right = mac(2950.0).place(size, GAP, EDGE);
        assert_eq!(right.x, 3024.0 - EDGE - 720.0);
        // An icon at the far left.
        let left = mac(4.0).place(size, GAP, EDGE);
        assert_eq!(left.x, EDGE);
        // Both still under the icon.
        assert_eq!((right.y, left.y), (76.0, 76.0));
    }

    #[test]
    fn the_monitor_need_not_start_at_the_origin() {
        // A second monitor to the right of a 1920 wide one, and one above.
        let anchor = Anchor {
            icon: rect(3700.0, 0.0, 24.0, 24.0),
            monitor: rect(1920.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
        };
        let at = anchor.place(vec2(360.0, 400.0), 4.0, 8.0);
        assert_eq!(at, pos2(3840.0 - 8.0 - 360.0, 28.0));
        let above = Anchor {
            icon: rect(100.0, -1080.0, 24.0, 24.0),
            monitor: rect(0.0, -1080.0, 1920.0, 1080.0),
            scale: 1.0,
        };
        assert_eq!(
            above.place(vec2(360.0, 400.0), 4.0, 8.0),
            pos2(8.0, -1052.0)
        );
    }

    #[test]
    fn a_taskbar_at_the_bottom_puts_the_panel_above_the_icon() {
        let anchor = windows(rect(1800.0, 1040.0, 24.0, 40.0));
        assert_eq!(anchor.side(), Side::Bottom);
        let at = anchor.place(vec2(360.0, 500.0), 4.0, 8.0);
        // Its bottom is `gap` over the icon's top; centered on the icon,
        // then moved in from the right edge.
        assert_eq!(at.y + 500.0, 1040.0 - 4.0);
        assert_eq!(at.x, 1920.0 - 8.0 - 360.0);
    }

    #[test]
    fn a_taskbar_on_a_side_puts_the_panel_beside_the_icon() {
        let left = windows(rect(0.0, 500.0, 48.0, 48.0));
        assert_eq!(left.side(), Side::Left);
        let at = left.place(vec2(360.0, 400.0), 4.0, 8.0);
        assert_eq!(at.x, 52.0);
        // Centered on the icon's middle (524).
        assert_eq!(at.y, 524.0 - 200.0);
        let right = windows(rect(1872.0, 20.0, 48.0, 48.0));
        assert_eq!(right.side(), Side::Right);
        let at = right.place(vec2(360.0, 400.0), 4.0, 8.0);
        assert_eq!(at.x, 1872.0 - 4.0 - 360.0);
        // An icon near the top of the side bar: the panel is kept on screen.
        assert_eq!(at.y, 8.0);
    }

    #[test]
    fn the_nearest_edge_wins_and_a_tie_goes_to_the_top() {
        // In the middle of the monitor: top and bottom, left and right tie.
        let middle = windows(rect(950.0, 520.0, 20.0, 40.0));
        assert_eq!(middle.side(), Side::Top);
        // A little nearer the bottom edge.
        let low = windows(rect(950.0, 560.0, 20.0, 40.0));
        assert_eq!(low.side(), Side::Bottom);
    }

    #[test]
    fn the_height_is_limited_to_the_room_on_the_free_side() {
        // Under a menu bar: the monitor's height, less the bar, gap, edge.
        assert_eq!(
            mac(2146.0).max_height(8.0, EDGE),
            1964.0 - 68.0 - 8.0 - 16.0
        );
        // Over a taskbar.
        let bottom = windows(rect(1800.0, 1040.0, 24.0, 40.0));
        assert_eq!(bottom.max_height(4.0, 8.0), 1040.0 - 4.0 - 8.0);
        // Beside a side bar: the whole height less the two edges.
        let side = windows(rect(0.0, 500.0, 48.0, 48.0));
        assert_eq!(side.max_height(4.0, 8.0), 1080.0 - 16.0);
        // A panel of exactly that height fits: the top is `edge` from the
        // monitor's, and so is the bottom.
        let at = bottom.place(vec2(360.0, bottom.max_height(4.0, 8.0)), 4.0, 8.0);
        assert_eq!(at.y, 8.0);
    }

    #[test]
    fn a_monitor_too_small_for_the_panel_gives_the_edge_and_no_negative_height() {
        let tiny = Anchor {
            icon: rect(100.0, 0.0, 20.0, 20.0),
            monitor: rect(0.0, 0.0, 200.0, 30.0),
            scale: 1.0,
        };
        assert_eq!(tiny.max_height(4.0, 8.0), 0.0);
        let at = tiny.place(vec2(360.0, 100.0), 4.0, 8.0);
        assert_eq!((at.x, at.y), (8.0, 8.0));
    }

    #[test]
    fn a_scale_between_one_and_two_gives_the_same_points() {
        // The same monitor in points at scale 1.5: 1280 by 720 points.
        let anchor = Anchor {
            icon: rect(1800.0, 0.0, 36.0, 36.0),
            monitor: rect(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.5,
        };
        let at = anchor.place(vec2(360.0 * 1.5, 400.0), 4.0 * 1.5, 8.0 * 1.5);
        // Centered on 1818, moved in from the right edge, `gap` under 36.
        assert_eq!(at.x / 1.5, (1920.0 - 12.0 - 540.0) / 1.5);
        assert_eq!(at.y / 1.5, (36.0 + 6.0) / 1.5);
    }

    #[test]
    fn an_anchor_goes_through_the_command_line_and_back_unchanged() {
        let anchor = Anchor {
            icon: rect(2146.5, 0.0, 76.0, 68.0),
            monitor: rect(-1920.0, 0.0, 1920.0, 1080.0),
            scale: 1.25,
        };
        let args = anchor.to_args();
        assert_eq!(args[0], "--icon");
        assert_eq!(args[2], "--monitor");
        assert_eq!(args[4], "--scale");
        assert_eq!(Anchor::parse(&args[1], &args[3], &args[5]), Ok(anchor));
    }

    #[test]
    fn a_bad_icon_monitor_or_scale_is_refused() {
        let good = ("1,2,3,4", "0,0,100,100", "2");
        assert!(Anchor::parse(good.0, good.1, good.2).is_ok());
        for icon in [
            "",
            "1,2,3",
            "1,2,3,4,5",
            "1,2,0,4",
            "1,2,3,-4",
            "a,2,3,4",
            "NaN,2,3,4",
        ] {
            assert!(
                Anchor::parse(icon, good.1, good.2).is_err(),
                "icon {icon:?}"
            );
        }
        for monitor in ["0,0,100", "0,0,0,100", "inf,0,100,100"] {
            assert!(
                Anchor::parse(good.0, monitor, good.2).is_err(),
                "{monitor:?}"
            );
        }
        for scale in ["", "0", "-1", "17", "NaN", "two"] {
            assert!(
                Anchor::parse(good.0, good.1, scale).is_err(),
                "scale {scale:?}"
            );
        }
        assert_eq!(
            Anchor::parse(good.0, good.1, "0").unwrap_err().to_string(),
            "--scale must be more than 0 and at most 16"
        );
    }
}
