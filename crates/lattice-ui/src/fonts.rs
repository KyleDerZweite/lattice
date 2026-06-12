//! Bundled fonts (Adwaita Sans / Adwaita Mono, OFL — see `assets/fonts/LICENSE`).
//!
//! Adwaita Sans is a variable font (Inter); ab_glyph renders its default instance,
//! so we ship a separate static **SemiBold (wght 600)** instance for headings and
//! bold runs — egui has no font-weight axis. The mono substitute is Adwaita Mono
//! (the reference asks for JetBrains Mono, which is unavailable offline).

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily, FontId, TextStyle};

use crate::theme::fs;

const ADWAITA_SANS: &[u8] = include_bytes!("../../../assets/fonts/AdwaitaSans-Regular.ttf");
const ADWAITA_SANS_SEMIBOLD: &[u8] =
    include_bytes!("../../../assets/fonts/AdwaitaSans-SemiBold.ttf");
const ADWAITA_MONO: &[u8] = include_bytes!("../../../assets/fonts/AdwaitaMono-Regular.ttf");

const UI: &str = "AdwaitaSans";
const UI_BOLD: &str = "AdwaitaSansSemiBold";
const MONO: &str = "AdwaitaMono";

/// Custom family used for headings and `**bold**` runs (true 600 weight).
pub fn bold_family() -> FontFamily {
    FontFamily::Name("ui-bold".into())
}

/// Register bundled fonts and the text-style scale. Call once at startup.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts
        .font_data
        .insert(UI.to_owned(), Arc::new(FontData::from_static(ADWAITA_SANS)));
    fonts.font_data.insert(
        UI_BOLD.to_owned(),
        Arc::new(FontData::from_static(ADWAITA_SANS_SEMIBOLD)),
    );
    fonts
        .font_data
        .insert(MONO.to_owned(), Arc::new(FontData::from_static(ADWAITA_MONO)));

    // Prepend ours; keep egui's bundled emoji/icon fonts as trailing fallbacks
    // so Unicode glyph icons still resolve instead of rendering as tofu.
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, UI.to_owned());

    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace.insert(0, MONO.to_owned());
    monospace.insert(1, UI.to_owned()); // glyph coverage fallback

    let mut bold = vec![UI_BOLD.to_owned(), UI.to_owned()];
    bold.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .into_iter()
            .flatten()
            .filter(|name| name.as_str() != UI)
            .cloned(),
    );
    fonts.families.insert(bold_family(), bold);

    ctx.set_fonts(fonts);

    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(fs::META, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(fs::UI, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(fs::MENU, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(fs::PRE, FontFamily::Monospace)),
            (TextStyle::Heading, FontId::new(fs::H2, bold_family())),
        ]
        .into();
    });
}
