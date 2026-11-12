// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Shared colour tokens for every Wyss DBS Localizer interactive window.
//!
//! This lives in `dbsloc-viz` rather than in the application because the scene
//! viewer is *embedded* inside the application: two palettes would put two
//! slightly different greys and ambers in the same window.
//!
//! The system is one surface ladder (canvas → surface → raised) built from the
//! Wyss Center brand greys, hairline borders instead of shadows in dark, **one
//! accent hue** (the brand green, muted), and status colours kept
//! colour-blind-safe (bluish-green / amber / vermillion / blue) so red versus
//! green is never the only channel carrying meaning. Pink and lime from the
//! brand sheet are deliberately unused: this is a clinical tool.
//!
//! Elevation flips direction between the themes, which is the usual convention:
//! in dark, a raised surface is **lighter** than the panel under it; in light,
//! the panel is an off-white canvas and a raised surface is **whiter**.
//!
//! Brand sources (official values) and the derived ones, `docs/BRAND.md`:
//!
//! | token | light | dark | source |
//! | --- | --- | --- | --- |
//! | `canvas` | `#E6E6E6` Grey light | `#0B0B0B` derived from Black | official / derived |
//! | `surface` | `#F8F8F8` Grey lighter | `#161616` derived | official / derived |
//! | `surface_raised` | `#FFFFFF` | `#202020` derived | derived |
//! | `hairline` | `#D8D8D8` Grey | `#2E2E2E` derived | official / derived |
//! | `hairline_strong` | `#949494` Grey mid | `#4A4A4A` derived | official / derived |
//! | `text` | `#111111` near Black | `#F8F8F8` Grey lighter | derived / official |
//! | `text_muted` | `#666666` from Grey dark `#6F6F6F` | `#949494` Grey mid | derived / official |
//! | `accent`, `ok` | `#0A7050` | `#27B386` | muted Green `#0DE69F` |
//! | `accent_fill`, `ok_fill` | `#0A7050` | `#0F6B4C` | muted Green `#0DE69F` |
//! | `warn` / `warn_fill` | `#8A5300` / `#8A5300` | `#D9A04A` / `#7A4D0A` | amber toward Red light `#FFBB99` |
//! | `err` / `err_fill` | `#A83605` / `#A83605` | `#FF8367` / `#7A2F1E` | vermillion toward Red light `#FFBB99` |
//! | `info` | `#36659B` | `#77B3F8` Blue light | derived / official |
//!
//! Light text is `#111111` rather than the official `#000000`: both are far
//! past AA (17.8:1 vs 19.8:1 on `surface`) and the near-black avoids the hard
//! halation of pure black on bright LCDs. Grey dark is darkened to `#666666`
//! because the official value is 4.0:1 on Grey light, under the 4.5:1 the
//! `text_muted` test asks for on the canvas.
//!
//! Status colours come in two forms, and they are not interchangeable. `ok`,
//! `warn`, `err` and `info` are **foregrounds**, measured ≥ 4.8:1 against the
//! surface they are drawn on. `accent_fill`, `warn_fill` and `err_fill` are
//! **backgrounds** that carry white label text at 6.1-9.3:1. Using a foreground
//! token as a fill inverts the contrast relationship and fails WCAG AA.

use eframe::egui::{self, Color32};

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Deepest surface: log background and text-edit wells.
    pub canvas: Color32,
    /// Default panel surface.
    pub surface: Color32,
    /// Cards, windows and popups that sit above a panel.
    pub surface_raised: Color32,
    pub hairline: Color32,
    pub hairline_strong: Color32,
    /// The one accent hue (muted brand green) for text, strokes, selection
    /// and the keyboard focus ring.
    pub accent: Color32,
    /// Accent as a **fill** under white label text.
    pub accent_fill: Color32,
    /// `warn` as a **fill** under white label text.
    pub warn_fill: Color32,
    /// `err` as a **fill** under white label text.
    pub err_fill: Color32,
    /// `ok` as a **fill** under white label text.
    pub ok_fill: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub info: Color32,
}

pub const fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            canvas: Color32::from_rgb(0x0b, 0x0b, 0x0b),
            surface: Color32::from_rgb(0x16, 0x16, 0x16),
            surface_raised: Color32::from_rgb(0x20, 0x20, 0x20),
            hairline: Color32::from_rgb(0x2e, 0x2e, 0x2e),
            hairline_strong: Color32::from_rgb(0x4a, 0x4a, 0x4a),
            accent: Color32::from_rgb(0x27, 0xb3, 0x86),
            accent_fill: Color32::from_rgb(0x0f, 0x6b, 0x4c),
            warn_fill: Color32::from_rgb(0x7a, 0x4d, 0x0a),
            err_fill: Color32::from_rgb(0x7a, 0x2f, 0x1e),
            ok_fill: Color32::from_rgb(0x0f, 0x6b, 0x4c),
            text: Color32::from_rgb(0xf8, 0xf8, 0xf8),
            text_muted: Color32::from_rgb(0x94, 0x94, 0x94),
            ok: Color32::from_rgb(0x27, 0xb3, 0x86),
            warn: Color32::from_rgb(0xd9, 0xa0, 0x4a),
            err: Color32::from_rgb(0xff, 0x83, 0x67),
            info: Color32::from_rgb(0x77, 0xb3, 0xf8),
        }
    } else {
        Palette {
            canvas: Color32::from_rgb(0xe6, 0xe6, 0xe6),
            surface: Color32::from_rgb(0xf8, 0xf8, 0xf8),
            surface_raised: Color32::from_rgb(0xff, 0xff, 0xff),
            hairline: Color32::from_rgb(0xd8, 0xd8, 0xd8),
            hairline_strong: Color32::from_rgb(0x94, 0x94, 0x94),
            accent: Color32::from_rgb(0x0a, 0x70, 0x50),
            accent_fill: Color32::from_rgb(0x0a, 0x70, 0x50),
            warn_fill: Color32::from_rgb(0x8a, 0x53, 0x00),
            err_fill: Color32::from_rgb(0xa8, 0x36, 0x05),
            ok_fill: Color32::from_rgb(0x0a, 0x70, 0x50),
            text: Color32::from_rgb(0x11, 0x11, 0x11),
            text_muted: Color32::from_rgb(0x66, 0x66, 0x66),
            ok: Color32::from_rgb(0x0a, 0x70, 0x50),
            warn: Color32::from_rgb(0x8a, 0x53, 0x00),
            err: Color32::from_rgb(0xa8, 0x36, 0x05),
            info: Color32::from_rgb(0x36, 0x65, 0x9b),
        }
    }
}

impl Palette {
    /// Every token intended as a **background under white label text**.
    ///
    /// The test over this list is what makes the family safe: adding a fill
    /// without adding it here is the only way to escape the contrast check, and
    /// that is a visible omission rather than a silent one.
    pub fn fills(&self) -> [(&'static str, Color32); 4] {
        [
            ("accent_fill", self.accent_fill),
            ("warn_fill", self.warn_fill),
            ("err_fill", self.err_fill),
            ("ok_fill", self.ok_fill),
        ]
    }
}

/// Palette for whatever theme `ui` is currently drawing in.
pub fn palette_for(ui: &egui::Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

/// Paint the token palette onto one theme's visuals.
pub fn apply(style: &mut egui::Style, dark: bool) {
    let p = palette(dark);
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.surface;
    v.window_fill = p.surface_raised;
    v.extreme_bg_color = p.canvas;
    v.faint_bg_color = p.surface_raised;
    v.window_stroke = egui::Stroke::new(1.0_f32, p.hairline_strong);
    v.hyperlink_color = p.info;
    v.selection.bg_fill = p.accent_fill.gamma_multiply(0.55);
    v.selection.stroke = egui::Stroke::new(1.0_f32, p.text);
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.err;
    v.window_shadow = if dark {
        egui::epaint::Shadow::NONE
    } else {
        egui::epaint::Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(38),
        }
    };
    v.popup_shadow = v.window_shadow;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, p.hairline);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, p.text);
    w.inactive.bg_fill = p.surface_raised;
    w.inactive.weak_bg_fill = p.surface_raised;
    w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, p.hairline);
    w.inactive.fg_stroke = egui::Stroke::new(1.0_f32, p.text);
    w.hovered.bg_fill = p.hairline;
    w.hovered.weak_bg_fill = p.hairline;
    w.hovered.bg_stroke = egui::Stroke::new(1.0_f32, p.hairline_strong);
    w.hovered.fg_stroke = egui::Stroke::new(1.5_f32, p.text);
    w.active.bg_fill = p.hairline_strong;
    w.active.weak_bg_fill = p.hairline_strong;
    w.active.bg_stroke = egui::Stroke::new(2.0_f32, p.accent);
    w.active.fg_stroke = egui::Stroke::new(1.5_f32, p.text);
    w.active.expansion = 1.0;
    w.open.bg_fill = p.surface_raised;
    w.open.weak_bg_fill = p.surface_raised;
    w.open.bg_stroke = egui::Stroke::new(1.0_f32, p.hairline_strong);
    w.open.fg_stroke = egui::Stroke::new(1.0_f32, p.text);

    style.visuals = v;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.x relative luminance.
    fn luminance(c: Color32) -> f64 {
        fn channel(v: u8) -> f64 {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Fills carry white label text, so they are measured against white — not
    /// against the surface, which is what the foreground tokens are tuned for.
    /// A round-5 regression shipped `err` (a foreground) as a fill and measured
    /// 2.4:1; this is the guard for that.
    #[test]
    fn status_fills_carry_white_text_at_wcag_aa() {
        for dark in [true, false] {
            let p = palette(dark);
            for (name, fill) in p.fills() {
                let ratio = contrast(fill, Color32::WHITE);
                assert!(
                    ratio >= 4.5,
                    "white on {name} is {ratio:.2}:1 (dark={dark}); WCAG AA needs 4.5:1"
                );
            }
        }
    }

    /// Composite a premultiplied colour over an opaque background, the way
    /// egui paints `selection.bg_fill` over a panel.
    fn over(fg: Color32, bg: Color32) -> Color32 {
        let inv = 255 - fg.a() as u32;
        let ch = |f: u8, b: u8| ((f as u32) + (b as u32 * inv) / 255).min(255) as u8;
        Color32::from_rgb(ch(fg.r(), bg.r()), ch(fg.g(), bg.g()), ch(fg.b(), bg.b()))
    }

    /// Selected text is read against the selection highlight as composited.
    #[test]
    fn selection_highlight_carries_text() {
        for dark in [true, false] {
            let p = palette(dark);
            let highlight = over(p.accent_fill.gamma_multiply(0.55), p.surface);
            let ratio = contrast(p.text, highlight);
            assert!(
                ratio >= 4.5,
                "text on the selection highlight is {ratio:.2}:1 (dark={dark})"
            );
        }
    }

    /// The keyboard focus ring is a non-text indicator: WCAG 2.2 SC 1.4.11 asks
    /// for 3:1, measured against what it actually sits on. The ring is drawn
    /// `StrokeKind::Outside` an expanded rect, so that is the panel surface —
    /// not the button fill it surrounds.
    #[test]
    fn focus_ring_is_perceivable_against_the_panel() {
        for dark in [true, false] {
            let p = palette(dark);
            for (name, surface) in [("surface", p.surface), ("surface_raised", p.surface_raised)] {
                let ratio = contrast(p.accent, surface);
                assert!(
                    ratio >= 3.0,
                    "the focus ring on {name} is {ratio:.2}:1 (dark={dark}); \
                     WCAG 2.2 SC 1.4.11 needs 3:1"
                );
            }
        }
    }

    /// The accent is also drawn as text (links, selected tabs, counts), so it
    /// is held to the text threshold on the surfaces it sits on, not only to
    /// the 3:1 of a non-text indicator.
    #[test]
    fn accent_is_readable_as_text_on_panels() {
        for dark in [true, false] {
            let p = palette(dark);
            for (name, surface) in [("surface", p.surface), ("surface_raised", p.surface_raised)] {
                let ratio = contrast(p.accent, surface);
                assert!(
                    ratio >= 4.5,
                    "accent on {name} is {ratio:.2}:1 (dark={dark}); needs 4.5:1"
                );
            }
        }
    }

    /// Foreground status colours are read on the panel surface they sit on.
    #[test]
    fn status_foregrounds_are_readable_on_their_surface() {
        for dark in [true, false] {
            let p = palette(dark);
            for (name, fg) in [
                ("ok", p.ok),
                ("warn", p.warn),
                ("err", p.err),
                ("info", p.info),
                ("text", p.text),
                ("text_muted", p.text_muted),
            ] {
                for (sname, surface) in [
                    ("surface", p.surface),
                    ("surface_raised", p.surface_raised),
                    ("canvas", p.canvas),
                ] {
                    let ratio = contrast(fg, surface);
                    assert!(
                        ratio >= 4.5,
                        "{name} on {sname} is {ratio:.2}:1 (dark={dark}); needs 4.5:1"
                    );
                }
            }
        }
    }
}
