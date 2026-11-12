// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui_tokens;

/// A compact circled "i" that explains a control or a step only on hover, so a
/// long explanation does not take permanent space in the panel body. Drawn, not
/// a font glyph: the brand typeface and egui's fallbacks have no circled letter.
pub fn info_icon(ui: &mut egui::Ui, text: &str) {
    let color = ui_tokens::palette_for(ui).text_muted;
    let side = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.circle_stroke(
            rect.center(),
            side * 0.42,
            egui::Stroke::new(1.0_f32, color),
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "i",
            egui::TextStyle::Small.resolve(ui.style()),
            color,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "More information")
    });
    response.on_hover_text(text);
}

pub fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("{} does not exist yet.", path.display()));
    }
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open {}: {e}", path.display()))
}
