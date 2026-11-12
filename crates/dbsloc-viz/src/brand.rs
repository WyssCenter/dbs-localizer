// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Wyss Center brand assets for every interactive Wyss DBS Localizer window: the IBM Plex
//! Sans typeface and the monochrome avatar. Colours live in [`crate::ui_tokens`];
//! the mapping from brand colours to tokens is documented in `docs/BRAND.md`.
//!
//! The font and logo bytes are compiled in here, so this module is only built
//! with the `interactive-eframe` feature: headless and CI library builds do not
//! carry them.
//!
//! IBM Plex Sans is © 2017 IBM Corp., SIL Open Font License 1.1
//! (`assets/fonts/OFL.txt`). The avatar is a trademark of the Wyss Center for
//! Bio and Neuroengineering, used in its own software (see `NOTICE`).

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

/// Font-data key and `FontFamily::Proportional` primary face.
pub const FAMILY_REGULAR: &str = "IBMPlexSans";
/// `FontFamily::Name` for headings and emphasised labels.
pub const FAMILY_MEDIUM: &str = "IBMPlexSans-Medium";
/// `FontFamily::Name` for the strongest weight the brand uses in UI.
pub const FAMILY_SEMIBOLD: &str = "IBMPlexSans-SemiBold";

static PLEX_REGULAR: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf");
static PLEX_MEDIUM: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf");
static PLEX_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf");
static ICON_INVERSE_PNG: &[u8] = include_bytes!("../assets/brand/wyss-center-icon-inverse-256.png");
static APP_ICON_PNG: &[u8] = include_bytes!("../assets/brand/dbs-loc-icon-256.png");

/// egui's default fonts with IBM Plex Sans in front of the proportional chain.
///
/// egui's own faces stay behind Plex as glyph fallbacks, so symbols the UI uses
/// (✔ ✖ ⓘ → …) and emoji keep rendering. Monospace is left as egui's default:
/// the brand specifies no monospace face.
pub fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        (FAMILY_REGULAR, PLEX_REGULAR),
        (FAMILY_MEDIUM, PLEX_MEDIUM),
        (FAMILY_SEMIBOLD, PLEX_SEMIBOLD),
    ] {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let chain = |primary: &str| -> Vec<String> {
        std::iter::once(primary.to_owned())
            .chain(fallbacks.iter().filter(|f| f.as_str() != primary).cloned())
            .collect()
    };
    fonts
        .families
        .insert(FontFamily::Proportional, chain(FAMILY_REGULAR));
    for name in [FAMILY_MEDIUM, FAMILY_SEMIBOLD] {
        fonts
            .families
            .insert(FontFamily::Name(name.into()), chain(name));
    }
    fonts
}

/// Install the brand fonts on `ctx`. Call once at creation, before any style
/// that names a `FontFamily::Name` family is applied.
pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

pub fn medium_family() -> FontFamily {
    FontFamily::Name(FAMILY_MEDIUM.into())
}

/// Heading `FontId` in the Medium weight.
pub fn heading_font_id(size: f32) -> egui::FontId {
    egui::FontId::new(size, medium_family())
}

/// The application's text-size ramp and control metrics (12 / 14 / 15 / 20, integer sizes so
/// glyphs land on whole pixels), so a standalone viewer looks the same as the embedded one.
pub fn apply_text_ramp(style: &mut egui::Style) {
    style
        .text_styles
        .insert(egui::TextStyle::Heading, heading_font_id(20.0));
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(15.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(15.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new(14.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(12.0, FontFamily::Proportional),
    );
    style.spacing.interact_size = egui::vec2(26.0, 26.0);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(9.0, 4.0);
    style.spacing.slider_width = 15.0 * 11.0;
}

/// Token palette on both themes plus the application's text ramp and Medium heading face, for
/// the standalone viewers. The application does the same inside its own style setup.
pub fn apply_theme(ctx: &egui::Context) {
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        crate::ui_tokens::apply(style, true)
    });
    ctx.style_mut_of(egui::Theme::Light, |style| {
        crate::ui_tokens::apply(style, false)
    });
    ctx.all_styles_mut(apply_text_ramp);
}

fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

/// The avatar as straight-alpha RGBA (white glyph on transparent): tint it with
/// the current text colour to draw it. Falls back to a 4×4 transparent image if
/// the embedded PNG cannot be decoded, which cannot happen for the committed
/// asset but keeps the window from failing to open.
pub fn brand_icon_rgba() -> (Vec<u8>, u32, u32) {
    decode_png(ICON_INVERSE_PNG).unwrap_or_else(|| (vec![0; 4 * 4 * 4], 4, 4))
}

/// The avatar as an `egui::ColorImage`, ready for `Context::load_texture`.
pub fn brand_logo_image() -> egui::ColorImage {
    let (rgba, w, h) = brand_icon_rgba();
    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba)
}

/// Texture options for drawing the 256 px avatar at UI size.
pub fn logo_texture_options() -> egui::TextureOptions {
    egui::TextureOptions {
        magnification: egui::TextureFilter::Linear,
        minification: egui::TextureFilter::Linear,
        wrap_mode: egui::TextureWrapMode::ClampToEdge,
        mipmap_mode: Some(egui::TextureFilter::Linear),
    }
}

/// The OS window icon: the application tile (`assets/brand/dbs-loc-icon-256.png`,
/// built from the square master by `scripts/build_app_icon.py`), which reads on both
/// light and dark task bars. Details in `docs/BRAND.md`.
pub fn window_icon() -> egui::IconData {
    let (rgba, width, height) = decode_png(APP_ICON_PNG).unwrap_or_else(brand_icon_rgba);
    egui::IconData {
        rgba,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plex_leads_the_proportional_chain_and_keeps_the_fallbacks() {
        let fonts = font_definitions();
        let prop = &fonts.families[&FontFamily::Proportional];
        assert_eq!(prop[0], FAMILY_REGULAR);
        assert!(prop.len() > 1, "egui's symbol / emoji fallbacks must stay");
        for name in [FAMILY_MEDIUM, FAMILY_SEMIBOLD] {
            let chain = &fonts.families[&FontFamily::Name(name.into())];
            assert_eq!(chain[0], name);
            assert_eq!(chain.len(), prop.len());
            assert!(fonts.font_data.contains_key(name));
        }
        assert!(fonts.families[&FontFamily::Monospace]
            .iter()
            .all(|f| !f.starts_with("IBMPlex")));
    }

    const UI_GLYPHS: &str = "\u{27f3}\u{25b6}\u{2716}\u{2714}\u{26a0}\u{25cb}\u{23f9}\u{1f50d}\u{203a}\u{2022}\u{2013}\u{b7}\u{2026}\u{2014}\u{b0}\u{2192}";

    fn missing_glyphs(family: FontFamily) -> Vec<char> {
        missing_glyphs_in(family, UI_GLYPHS)
    }

    fn missing_glyphs_in(family: FontFamily, glyphs: &str) -> Vec<char> {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        let id = egui::FontId::new(14.0, family);
        let missing = ctx.fonts_mut(|f| {
            let mut uv = |c: char| {
                f.layout_no_wrap(c.to_string(), id.clone(), egui::Color32::WHITE)
                    .rows[0]
                    .glyphs[0]
                    .uv_rect
            };
            let replacement = uv('\u{25fb}');
            glyphs
                .chars()
                .filter(|c| uv(*c) == replacement)
                .collect::<Vec<_>>()
        });
        ctx.end_pass().textures_delta.clear();
        missing
    }

    #[test]
    fn every_glyph_the_ui_draws_is_covered_by_the_font_chain() {
        for family in [FontFamily::Proportional, medium_family()] {
            assert_eq!(missing_glyphs(family), Vec::<char>::new());
        }
    }

    #[test]
    fn the_circled_i_and_the_thin_cross_are_not_covered() {
        let missing = missing_glyphs_in(FontFamily::Proportional, "\u{24d8}\u{2715}");
        assert_eq!(missing, vec!['\u{24d8}', '\u{2715}']);
    }

    #[test]
    fn the_avatar_decodes_as_a_256_px_alpha_mask() {
        let (rgba, w, h) = brand_icon_rgba();
        assert_eq!((w, h), (256, 256));
        assert_eq!(rgba.len(), 256 * 256 * 4);
        let opaque = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 255)
            .count();
        let clear = rgba.as_chunks::<4>().0.iter().filter(|p| p[3] == 0).count();
        assert!(
            opaque > 10_000 && clear > 10_000,
            "opaque={opaque} clear={clear}"
        );
        assert!(rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 255)
            .all(|p| p[0] == 255 && p[1] == 255 && p[2] == 255));
        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        let opaque = |p: &[u8; 4]| p[3] == 255;
        let black = icon
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| opaque(p) && p[0] < 16 && p[1] < 16 && p[2] < 16)
            .count();
        let light = icon
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| opaque(p) && p[0].max(p[1]).max(p[2]) > 200)
            .count();
        let corner_clear = icon.rgba[3] == 0;
        assert!(
            black > 10_000 && light > 5_000 && corner_clear,
            "black={black} light={light}"
        );
    }
}
