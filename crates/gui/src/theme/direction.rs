//! Start and end instead of left and right (rtl.md, rules.md 13). Layout
//! code asks a [`Direction`] which side a row starts on, so a right-to-left
//! language later mirrors the window without new layout code. egui has no
//! global direction (spike S-14): each place picks its layout from here.

use eframe::egui::{self, Align, Layout, Rect};

/// The direction of the window's language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    /// Left to right: English, and every language of the window today.
    #[default]
    Ltr,
    /// Right to left: Arabic, after the bidi work S-14 asks for.
    Rtl,
}

impl Direction {
    /// The direction of the window. Always left to right until Arabic ships.
    pub const CURRENT: Self = Self::Ltr;

    pub const fn is_rtl(self) -> bool {
        matches!(self, Self::Rtl)
    }

    /// The alignment of the start of a line.
    pub const fn start(self) -> Align {
        match self {
            Self::Ltr => Align::Min,
            Self::Rtl => Align::Max,
        }
    }

    /// The alignment of the end of a line.
    pub const fn end(self) -> Align {
        match self {
            Self::Ltr => Align::Max,
            Self::Rtl => Align::Min,
        }
    }

    /// A row that lays its widgets out from the start, centered on its height.
    pub fn row(self) -> Layout {
        match self {
            Self::Ltr => Layout::left_to_right(Align::Center),
            Self::Rtl => Layout::right_to_left(Align::Center),
        }
    }

    /// A row that lays its widgets out from the end: the actions of a
    /// screen header, the buttons of a dialog.
    pub fn row_from_end(self) -> Layout {
        match self {
            Self::Ltr => Layout::right_to_left(Align::Center),
            Self::Rtl => Layout::left_to_right(Align::Center),
        }
    }

    /// A column whose lines line up on the start side.
    pub fn column(self) -> Layout {
        Layout::top_down(self.start())
    }

    /// A side panel on the start side: the sidebar, the list of a master and
    /// detail screen.
    pub fn start_panel(self, id_salt: impl egui::AsIdSalt) -> egui::Panel {
        match self {
            Self::Ltr => egui::Panel::left(id_salt),
            Self::Rtl => egui::Panel::right(id_salt),
        }
    }

    /// The x of the start edge of `rect`.
    pub fn start_x(self, rect: Rect) -> f32 {
        match self {
            Self::Ltr => rect.left(),
            Self::Rtl => rect.right(),
        }
    }

    /// The x of the end edge of `rect`.
    pub fn end_x(self, rect: Rect) -> f32 {
        match self {
            Self::Ltr => rect.right(),
            Self::Rtl => rect.left(),
        }
    }

    /// `+1` when "forward" (toward the end) is to the right, `-1` when it is
    /// to the left: multiply an x offset by it, or mirror a direction icon
    /// (a back arrow, a chevron) with it.
    pub const fn forward(self) -> f32 {
        match self {
            Self::Ltr => 1.0,
            Self::Rtl => -1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{pos2, vec2};

    use super::*;

    #[test]
    fn the_window_is_left_to_right_for_now() {
        assert_eq!(Direction::CURRENT, Direction::Ltr);
        assert!(!Direction::CURRENT.is_rtl());
        assert_eq!(Direction::default(), Direction::Ltr);
    }

    #[test]
    fn right_to_left_mirrors_start_and_end() {
        for direction in [Direction::Ltr, Direction::Rtl] {
            assert_ne!(direction.start(), direction.end());
            assert_ne!(direction.row().main_dir, direction.row_from_end().main_dir);
        }
        assert_eq!(Direction::Ltr.start(), Align::Min);
        assert_eq!(Direction::Rtl.start(), Align::Max);
        assert_eq!(Direction::Ltr.row().main_dir, egui::Direction::LeftToRight);
        assert_eq!(Direction::Rtl.row().main_dir, egui::Direction::RightToLeft);
        assert_eq!(Direction::Rtl.column().cross_align, Align::Max);
        assert_eq!(Direction::Ltr.forward(), -Direction::Rtl.forward());
    }

    #[test]
    fn start_and_end_edges_swap_in_right_to_left() {
        let rect = Rect::from_min_size(pos2(10.0, 0.0), vec2(100.0, 20.0));
        assert_eq!(Direction::Ltr.start_x(rect), 10.0);
        assert_eq!(Direction::Ltr.end_x(rect), 110.0);
        assert_eq!(Direction::Rtl.start_x(rect), 110.0);
        assert_eq!(Direction::Rtl.end_x(rect), 10.0);
    }
}
