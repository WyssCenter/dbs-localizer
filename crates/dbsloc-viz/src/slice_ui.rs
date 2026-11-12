// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use eframe::egui;

pub(crate) const SLICE_TEX_UV_FLIPPED_Y: egui::Rect =
    egui::Rect::from_min_max(egui::pos2(0.0, 1.0), egui::pos2(1.0, 0.0));

pub(crate) fn consume_plain_key(input: &mut egui::InputState, key: egui::Key) -> bool {
    let mut hit = false;
    input.events.retain(|event| {
        let is_match = matches!(
            event,
            egui::Event::Key { key: k, modifiers, pressed: true, .. }
                if *k == key && modifiers.matches_exact(egui::Modifiers::NONE)
        );
        hit |= is_match;
        !is_match
    });
    hit
}

pub(crate) fn upload_texture(
    ctx: &egui::Context,
    slot: &mut Option<egui::TextureHandle>,
    name: &str,
    image: egui::ColorImage,
) {
    match slot {
        Some(handle) => handle.set(image, egui::TextureOptions::default()),
        None => *slot = Some(ctx.load_texture(name, image, egui::TextureOptions::default())),
    }
}

pub(crate) fn scale_tex_to_panel(tex_sz: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    if available.x < 1.0 || available.y < 1.0 || tex_sz.x < 1.0 || tex_sz.y < 1.0 {
        return egui::Vec2::ZERO;
    }
    let wide = (tex_sz.x / tex_sz.y) > 2.0;
    let scale = if wide {
        available.y / tex_sz.y
    } else {
        (available.x / tex_sz.x).min(available.y / tex_sz.y)
    };
    tex_sz * scale
}
