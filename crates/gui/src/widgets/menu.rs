//! Menus (components.md, Buttons and Version menu): the list that opens
//! under a button, such as "Add site…" and the row menu (⋯). A row is 30
//! high with the text after a check column when the menu marks a setting.
//! [`Menu`] opens when the button is clicked and returns the value of the row
//! the user chose; the screen turns that into an action. The version menu of
//! a table row draws its rows with [`row`] too.

use eframe::egui::{
    Align2, Color32, CornerRadius, Popup, RectAlign, Response, Role, Sense, Ui, WidgetInfo, vec2,
};

use super::icons::Icon;
use super::{Look, layout, paint_focus_ring, quiet_fill, slot, spot, with_disabled_reason};
use crate::theme::{Direction, Face, TextToken, Theme};

/// How a row marks the setting that is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A plain action: no check column.
    None,
    /// One of a group of settings: a check when it is the one that is on.
    Radio(bool),
}

/// The color of a row's text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Normal,
    /// `error`: a destructive action, such as "Unlink…".
    Danger,
}

/// What a row says and how it is marked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowSpec<'a> {
    pub label: &'a str,
    pub mark: Mark,
    pub tone: Tone,
}

fn token(theme: &Theme, face: Face) -> TextToken {
    TextToken {
        size: theme.text.body_sm.size,
        face,
    }
}

/// The fewest points a row needs: the padding, the check column of a marked
/// menu, the text in its heavier face, and the padding.
pub fn row_width(ui: &Ui, theme: &Theme, spec: &RowSpec<'_>) -> f32 {
    // The row is as wide for the heavier face, so a menu keeps its width
    // when the check moves.
    let widest = layout(
        ui.painter(),
        spec.label,
        token(theme, Face::Medium),
        Color32::PLACEHOLDER,
    );
    let check = match spec.mark {
        Mark::None => 0.0,
        Mark::Radio(_) => theme.size.icon_inline + theme.space.s2,
    };
    theme.space.s3 + check + widest.size().x + theme.space.s3
}

/// One row, `width` wide: a check when it marks the setting that is on,
/// then the text. The pointer, the focus ring, and the pressed state are
/// drawn here.
pub fn row(ui: &mut Ui, theme: &Theme, spec: &RowSpec<'_>, width: f32) -> Response {
    let direction = Direction::CURRENT;
    let pad = theme.space.s3;
    let check = theme.size.icon_inline;
    let gap = theme.space.s2;
    let checked = matches!(spec.mark, Mark::Radio(true));
    let face = if checked { Face::Medium } else { Face::Regular };
    let galley = layout(
        ui.painter(),
        spec.label,
        token(theme, face),
        Color32::PLACEHOLDER,
    );
    let size = vec2(width, theme.size.control_in_row);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let look = Look::of(&response);
        let painter = ui.painter();
        let corner = theme.radius.sm;
        if let Some(fill) = quiet_fill(theme, look.touch) {
            painter.rect_filled(rect, CornerRadius::same(corner), fill);
        }
        let row = |offset: f32, width: f32| {
            let x = slot(direction, rect, offset, width);
            spot(x, rect.top(), width, size.y)
        };
        let text_at = match spec.mark {
            Mark::None => pad,
            Mark::Radio(_) => {
                if checked {
                    Icon::Check.paint(painter, theme, row(pad, check), theme.color.accent);
                }
                pad + check + gap
            }
        };
        let color = match spec.tone {
            Tone::Normal => theme.color.ink,
            Tone::Danger => theme.color.error,
        };
        let spot = row(text_at, galley.size().x);
        let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), spot);
        painter.galley(at.min, galley, color);
        if look.focused {
            paint_focus_ring(painter, theme, rect, corner);
        }
    }
    let (enabled, label) = (ui.is_enabled(), spec.label);
    response.widget_info(|| match spec.mark {
        Mark::None => WidgetInfo::labeled(Role::MenuItem, enabled, label),
        Mark::Radio(on) => WidgetInfo::selected(Role::MenuItemRadio, enabled, on, label),
    });
    response
}

/// A row of a [`Menu`]: the value it returns when chosen, and its text.
#[derive(Clone, Copy, Debug)]
pub struct Item<'a, T> {
    value: T,
    label: &'a str,
    tone: Tone,
    hint: Option<&'a str>,
    off: Option<&'a str>,
}

impl<'a, T> Item<'a, T> {
    pub const fn new(value: T, label: &'a str) -> Self {
        Self {
            value,
            label,
            tone: Tone::Normal,
            hint: None,
            off: None,
        }
    }

    /// The text in `error`: for something destructive.
    pub const fn danger(mut self) -> Self {
        self.tone = Tone::Danger;
        self
    }

    /// Shown over the row while the pointer rests on it.
    pub const fn hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }

    /// The row cannot be chosen; hovering it shows `reason`.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.off = Some(reason);
        self
    }

    /// [`Item::disabled`] when there is a reason.
    pub const fn disabled_if(mut self, reason: Option<&'a str>) -> Self {
        self.off = reason;
        self
    }

    pub const fn label(&self) -> &'a str {
        self.label
    }

    /// Why the row cannot be chosen, if it cannot.
    pub const fn reason(&self) -> Option<&'a str> {
        self.off
    }

    const fn spec(&self) -> RowSpec<'a> {
        RowSpec {
            label: self.label,
            mark: Mark::None,
            tone: self.tone,
        }
    }
}

/// The rows that open under a button when it is clicked.
#[derive(Clone, Copy, Debug)]
#[must_use = "a menu is drawn only when it is shown"]
pub struct Menu<'a, T> {
    items: &'a [Item<'a, T>],
    from_end: bool,
}

impl<'a, T: Copy> Menu<'a, T> {
    pub const fn new(items: &'a [Item<'a, T>]) -> Self {
        Self {
            items,
            from_end: false,
        }
    }

    /// The menu lines up with the end of its button, for a button at the end
    /// of a row: the menu then grows toward the start.
    pub const fn from_end(mut self) -> Self {
        self.from_end = true;
        self
    }

    /// The side of the button the menu lines up with.
    pub const fn align(&self, direction: Direction) -> RectAlign {
        match (self.from_end, direction) {
            (false, Direction::Ltr) | (true, Direction::Rtl) => RectAlign::BOTTOM_START,
            (false, Direction::Rtl) | (true, Direction::Ltr) => RectAlign::BOTTOM_END,
        }
    }

    /// Opens the menu under `button` when it was clicked, and keeps it open
    /// until a row is chosen, Escape is pressed, or the user clicks beside
    /// it. `Some` is the value of the row chosen this frame.
    pub fn show(self, button: &Response) -> Option<T> {
        let theme = Theme::of(&button.ctx);
        let popup = Popup::menu(button)
            .gap(theme.space.s1)
            .align(self.align(Direction::CURRENT));
        let least = button.rect.width();
        popup
            .show(|ui| self.list(ui, least))
            .and_then(|shown| shown.inner)
    }

    /// The rows: what the popup holds, and what the gallery shows open. At
    /// least `least` points wide. Returns the value of the row clicked.
    pub fn list(&self, ui: &mut Ui, least: f32) -> Option<T> {
        let theme = Theme::of(ui.ctx());
        // The rows touch: each has its own height and its own hover fill.
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = self
            .items
            .iter()
            .map(|item| row_width(ui, theme, &item.spec()))
            .fold(least, f32::max);
        let mut chosen = None;
        for item in self.items {
            let spec = item.spec();
            let response = match item.off {
                Some(reason) => {
                    let off = ui.add_enabled_ui(false, |ui| row(ui, theme, &spec, width));
                    with_disabled_reason(off.inner, Some(reason))
                }
                None => {
                    let shown = row(ui, theme, &spec, width);
                    match item.hint {
                        Some(hint) => shown.on_hover_text(hint),
                        None => shown,
                    }
                }
            };
            if response.clicked() {
                chosen = Some(item.value);
            }
        }
        chosen
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Id, Key, Pos2};

    use super::*;
    use crate::theme::tests::{TEXT, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::{Harness, Tree, over};
    use eframe::egui::pos2;

    const ITEMS: [Item<'static, u8>; 3] = [
        Item::new(1, "Switch to HTTP"),
        Item::new(2, "Rename"),
        Item::new(3, "Unlink…"),
    ];

    fn items() -> [Item<'static, u8>; 3] {
        [
            ITEMS[0].hint("Serve over plain HTTP"),
            ITEMS[1].disabled("Not yet"),
            ITEMS[2].danger(),
        ]
    }

    fn center(tree: &Tree, label: &str) -> Pos2 {
        let node = tree.node_with_label(label).expect(label);
        let b = node.bounds().expect("bounds");
        pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)
    }

    #[test]
    fn a_menu_has_30_high_rows_as_wide_as_the_widest_or_the_button() {
        let harness = Harness::new(egui::Theme::Light);
        let mut menus = Vec::new();
        let mut widest = 0.0;
        harness.run(|ui| {
            let items = items();
            let theme = Theme::of(ui.ctx());
            widest = items
                .iter()
                .map(|item| row_width(ui, theme, &item.spec()))
                .fold(0.0, f32::max);
            for least in [0.0, 300.0] {
                let shown = ui.scope(|ui| Menu::new(&items).list(ui, least));
                menus.push(shown.response.rect);
            }
        });
        let theme = &LIGHT;
        assert_eq!(theme.size.control_in_row, 30.0);
        for rect in &menus {
            assert_eq!(rect.height(), 3.0 * theme.size.control_in_row);
        }
        // No button to be as wide as: the widest row sets the width.
        assert!(
            (menus[0].width() - widest).abs() < 0.5,
            "{}",
            menus[0].width()
        );
        // A wide button: the menu is as wide as the button.
        assert!(
            (menus[1].width() - 300.0).abs() < 0.5,
            "{}",
            menus[1].width()
        );
    }

    #[test]
    fn a_marked_menu_keeps_a_check_column_and_a_plain_one_does_not() {
        let harness = Harness::new(egui::Theme::Light);
        harness.run(|ui| {
            let theme = Theme::of(ui.ctx());
            let spec = |mark| RowSpec {
                label: "8.4",
                mark,
                tone: Tone::Normal,
            };
            let plain = row_width(ui, theme, &spec(Mark::None));
            let radio = row_width(ui, theme, &spec(Mark::Radio(false)));
            assert!(
                (radio - plain - theme.size.icon_inline - theme.space.s2).abs() < 1e-3,
                "{plain} {radio}"
            );
            // The check moving does not change the width.
            let on = row_width(ui, theme, &spec(Mark::Radio(true)));
            assert_eq!(radio, on);
        });
    }

    #[test]
    fn a_click_opens_the_menu_and_a_row_returns_its_value() {
        let harness = Harness::new(egui::Theme::Light);
        let button = |ui: &mut Ui| {
            let response = ui.add(crate::widgets::Button::secondary("Actions"));
            Menu::new(&items()).from_end().show(&response)
        };
        let tree = harness.accessibility(|ui| {
            button(ui);
        });
        let mut chosen = None;
        harness.click(center(&tree, "Actions"), |ui| chosen = button(ui));
        assert_eq!(chosen, None, "opening a menu chooses nothing");
        assert!(Popup::is_any_open(&harness.ctx));

        let tree = harness.accessibility(|ui| {
            button(ui);
        });
        for label in ["Switch to HTTP", "Rename", "Unlink…"] {
            let node = tree.node_with_label(label).expect(label);
            assert_eq!(node.role(), Role::MenuItem, "{label}");
        }
        let mut chosen = None;
        harness.click(center(&tree, "Unlink…"), |ui| chosen = button(ui));
        assert_eq!(chosen, Some(3));
    }

    #[test]
    fn the_keyboard_opens_the_menu_and_reaches_a_row() {
        let harness = Harness::new(egui::Theme::Light);
        let button = std::cell::Cell::new(Id::NULL);
        let chosen = std::cell::Cell::new(None);
        let draw = |ui: &mut Ui| {
            let response = ui.add(crate::widgets::Button::secondary("Actions"));
            button.set(response.id);
            if let Some(picked) = Menu::new(&items()).show(&response) {
                chosen.set(Some(picked));
            }
        };
        harness.run(draw);
        harness
            .ctx
            .memory_mut(|memory| memory.request_focus(button.get()));
        harness.key(Key::Enter, draw);
        assert!(Popup::is_any_open(&harness.ctx), "Enter opens the menu");
        // Tab from the button lands on the first row; Enter chooses it.
        harness.key(Key::Tab, draw);
        harness.key(Key::Enter, draw);
        assert_eq!(chosen.get(), Some(1));
    }

    #[test]
    fn a_disabled_row_returns_nothing_and_a_menu_of_a_disabled_button_never_opens() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            Menu::new(&items()).list(ui, 0.0);
        });
        let mut chosen = None;
        harness.click(center(&tree, "Rename"), |ui| {
            chosen = Menu::new(&items()).list(ui, 0.0);
        });
        assert_eq!(chosen, None);
        assert_eq!(items()[1].reason(), Some("Not yet"));
        assert_eq!(items()[0].reason(), None);

        // A disabled button does not open the menu.
        let off = |ui: &mut Ui| {
            let response = ui.add(crate::widgets::Button::secondary("Actions").disabled("No"));
            Menu::new(&items()).show(&response)
        };
        let tree = harness.accessibility(|ui| {
            off(ui);
        });
        harness.click(center(&tree, "Actions"), |ui| {
            off(ui);
        });
        assert!(!Popup::is_any_open(&harness.ctx));
    }

    #[test]
    fn the_menu_lines_up_with_the_end_of_a_button_at_the_end_of_a_row() {
        let from_end = Menu::new(&ITEMS).from_end();
        let from_start = Menu::new(&ITEMS);
        assert_eq!(from_end.align(Direction::Ltr), RectAlign::BOTTOM_END);
        assert_eq!(from_end.align(Direction::Rtl), RectAlign::BOTTOM_START);
        assert_eq!(from_start.align(Direction::Ltr), RectAlign::BOTTOM_START);
        assert_eq!(from_start.align(Direction::Rtl), RectAlign::BOTTOM_END);
    }

    #[test]
    fn the_text_of_a_row_reads_at_rest_and_under_the_pointer_in_both_themes() {
        for theme in [&LIGHT, &DARK] {
            let hover = over(
                theme.color.selected.gamma_multiply(theme.state.row_hover),
                theme.color.surface,
            );
            for page in [theme.color.surface, hover] {
                for text in [theme.color.ink, theme.color.error] {
                    assert!(contrast(text, page) >= TEXT);
                }
            }
        }
    }
}
