//! The bundled fonts (ADR 0021): Readex Pro, one variable file whose weights
//! 300 to 600 come from its `wght` axis (`VariationCoords`), and
//! JetBrains Mono Regular. Both are compiled into the program with
//! `include_bytes!`, so nothing is read from disk. Licences and sources:
//! `crates/gui/assets/fonts/README.md`.
//!
//! egui's own fonts (the `default_fonts` feature) stay at the end of each
//! list, for the few glyphs ours lack, such as egui's icons.

use std::sync::{Arc, LazyLock};

use eframe::egui::epaint::text::VariationCoords;
use eframe::egui::{FontData, FontDefinitions, FontFamily, FontTweak};

use super::Face;

const READEX_PRO: &[u8] = include_bytes!("../../assets/fonts/ReadexPro[HEXP,wght].ttf");
const JETBRAINS_MONO: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

/// The variable font's weight axis.
const WEIGHT_AXIS: &str = "wght";
/// The weight the variable font draws with no coordinates set.
const DEFAULT_WEIGHT: f32 = 400.0;

impl Face {
    pub const ALL: [Self; 5] = [
        Self::Light,
        Self::Regular,
        Self::Medium,
        Self::SemiBold,
        Self::Mono,
    ];

    /// The name of its font in [`FontDefinitions`].
    const fn name(self) -> &'static str {
        match self {
            Self::Light => "ReadexPro-Light",
            Self::Regular => "ReadexPro-Regular",
            Self::Medium => "ReadexPro-Medium",
            Self::SemiBold => "ReadexPro-SemiBold",
            Self::Mono => "JetBrainsMono-Regular",
        }
    }

    /// Its Readex Pro weight, or `None` for the mono face.
    const fn weight(self) -> Option<f32> {
        match self {
            Self::Light => Some(300.0),
            Self::Regular => Some(DEFAULT_WEIGHT),
            Self::Medium => Some(500.0),
            Self::SemiBold => Some(600.0),
            Self::Mono => None,
        }
    }

    fn font(self) -> FontData {
        match self.weight() {
            None => FontData::from_static(JETBRAINS_MONO),
            // The file's default: no coordinates, egui's fast path.
            Some(weight) if weight == DEFAULT_WEIGHT => FontData::from_static(READEX_PRO),
            Some(weight) => FontData::from_static(READEX_PRO).tweak(FontTweak {
                coords: VariationCoords::new([(WEIGHT_AXIS, weight)]),
                ..FontTweak::default()
            }),
        }
    }
}

/// The families of the faces egui has no family for, made once so a widget
/// that asks for one on every frame does not allocate.
static NAMED: LazyLock<[FontFamily; 3]> = LazyLock::new(|| {
    [Face::Light, Face::Medium, Face::SemiBold].map(|face| FontFamily::Name(face.name().into()))
});

/// The egui family that draws `face`. Regular is egui's `Proportional` and
/// mono its `Monospace`, so egui's text styles and widgets use them.
pub fn family(face: Face) -> FontFamily {
    match face {
        Face::Regular => FontFamily::Proportional,
        Face::Mono => FontFamily::Monospace,
        Face::Light => NAMED[0].clone(),
        Face::Medium => NAMED[1].clone(),
        Face::SemiBold => NAMED[2].clone(),
    }
}

/// Every face, each first in its family, with egui's fonts after it.
pub fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallbacks = |fonts: &FontDefinitions, family: &FontFamily| {
        fonts.families.get(family).cloned().unwrap_or_default()
    };
    let proportional = fallbacks(&fonts, &FontFamily::Proportional);
    let monospace = fallbacks(&fonts, &FontFamily::Monospace);
    for face in Face::ALL {
        fonts
            .font_data
            .insert(face.name().to_owned(), Arc::new(face.font()));
        let mut list = vec![face.name().to_owned()];
        if face == Face::Mono {
            // Readex Pro draws what JetBrains Mono lacks, such as Arabic in
            // a path or a log line.
            list.push(Face::Regular.name().to_owned());
            list.extend(monospace.iter().cloned());
        } else {
            list.extend(proportional.iter().cloned());
        }
        fonts.families.insert(family(face), list);
    }
    fonts
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, FontId};

    use super::*;

    #[test]
    fn each_face_comes_first_in_its_own_family() {
        let fonts = definitions();
        for face in Face::ALL {
            let list = &fonts.families[&family(face)];
            assert_eq!(list[0], face.name());
            assert!(fonts.font_data.contains_key(face.name()));
        }
        // egui's fonts stay as the last fallbacks.
        let proportional = &fonts.families[&FontFamily::Proportional];
        assert!(proportional.len() > 1);
    }

    #[test]
    fn only_the_non_default_weights_set_coordinates() {
        assert!(Face::Regular.font().tweak.coords.as_ref().is_empty());
        assert!(Face::Mono.font().tweak.coords.as_ref().is_empty());
        for face in [Face::Light, Face::Medium, Face::SemiBold] {
            assert_eq!(face.font().tweak.coords.as_ref().len(), 1, "{face:?}");
        }
    }

    /// The bundled files load: each face draws the window's own characters,
    /// and Arabic, which egui's fonts do not have, so it comes from Readex Pro.
    #[test]
    fn every_face_loads_and_draws_latin_and_arabic() {
        let ctx = egui::Context::default();
        ctx.set_fonts(definitions());
        // Fonts are installed at the start of a pass. No renderer takes the
        // font texture here, so it is dropped on purpose.
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        for face in Face::ALL {
            let font = FontId::new(14.0, family(face));
            let drawn = ctx.fonts_mut(|fonts| fonts.has_glyphs(&font, "Marsa 8.4 – … مرسى"));
            assert!(drawn, "{face:?}");
        }
    }
}
