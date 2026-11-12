// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;

use crate::gesture_zoom::{
    adjust_zoom, adjust_zoom_pinch, TOUCHPAD_POINTS_PER_SLICE, ZOOM_MAX, ZOOM_MIN,
};
use crate::refine_volume::{RefinePlane, RefineVolume, SliceScratch};
use crate::slice_ui::{
    consume_plain_key, scale_tex_to_panel, upload_texture, SLICE_TEX_UV_FLIPPED_Y,
};

const SHORTCUT_ROWS: &[(&str, &str)] = &[
    ("1 / 2 / 3", "Single pane: axial / coronal / sagittal"),
    ("0", "Show all three planes"),
    (
        "Page Up / Page Down",
        "Previous / next slice in the single pane",
    ),
    ("R", "Reset zoom, pan and window"),
];

pub fn shortcut_rows() -> &'static [(&'static str, &'static str)] {
    SHORTCUT_ROWS
}

type LoadResult = Result<RefineVolume, String>;

enum State {
    Loading { rx: mpsc::Receiver<LoadResult> },
    Ready(Box<Ready>),
    Failed(String),
}

pub struct VolumePreview {
    title: String,
    path: PathBuf,
    file: String,
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    Quad,
    Single(RefinePlane),
}

type TexKey = (usize, u32, u32);

struct PlaneView {
    scratch: SliceScratch,
    tex: Option<egui::TextureHandle>,
    key: Option<TexKey>,
    zoom: f32,
    pan: egui::Vec2,
}

impl PlaneView {
    fn new() -> Self {
        Self {
            scratch: SliceScratch::default(),
            tex: None,
            key: None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        }
    }
}

struct Ready {
    volume: RefineVolume,
    world: [f64; 3],
    default_window: (f32, f32),
    center: f32,
    width: f32,
    layout: Layout,
    planes: [PlaneView; 3],
    scroll_acc: f32,
}

fn world_axis(plane: RefinePlane) -> usize {
    match plane {
        RefinePlane::Axial => 2,
        RefinePlane::Coronal => 1,
        RefinePlane::Sagittal => 0,
    }
}

fn world_axis_letter(plane: RefinePlane) -> &'static str {
    match plane {
        RefinePlane::Axial => "z",
        RefinePlane::Coronal => "y",
        RefinePlane::Sagittal => "x",
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl Ready {
    fn new(volume: RefineVolume) -> Self {
        let dims = volume.dims();
        let mid = [
            (dims[0] as f64 - 1.0) * 0.5,
            (dims[1] as f64 - 1.0) * 0.5,
            (dims[2] as f64 - 1.0) * 0.5,
        ];
        let world = volume.voxel_to_world_f(mid);
        let default_window = volume.default_window();
        Self {
            volume,
            world,
            default_window,
            center: default_window.0,
            width: default_window.1,
            layout: Layout::Quad,
            planes: [PlaneView::new(), PlaneView::new(), PlaneView::new()],
            scroll_acc: 0.0,
        }
    }

    fn slice_count(&self, plane: RefinePlane) -> usize {
        self.volume.dims()[plane.axes().2]
    }

    fn slice_index(&self, plane: RefinePlane) -> usize {
        let top = self.slice_count(plane).saturating_sub(1);
        let ijk = self.volume.world_to_voxel_f(self.world);
        let t = ijk[plane.axes().2];
        if t.is_finite() {
            (t.round().max(0.0) as usize).min(top)
        } else {
            0
        }
    }

    fn set_slice(&mut self, plane: RefinePlane, index: usize) {
        let top = self.slice_count(plane).saturating_sub(1);
        let mut ijk = self.volume.world_to_voxel_f(self.world);
        ijk[plane.axes().2] = index.min(top) as f64;
        self.world = self.volume.voxel_to_world_f(ijk);
    }

    fn step_slice(&mut self, plane: RefinePlane, step: i64) {
        let top = self.slice_count(plane).saturating_sub(1) as i64;
        let next = (self.slice_index(plane) as i64 + step).clamp(0, top);
        self.set_slice(plane, next as usize);
    }

    fn slice_mm(&self, plane: RefinePlane) -> f64 {
        let mut ijk = self.volume.world_to_voxel_f(self.world);
        ijk[plane.axes().2] = self.slice_index(plane) as f64;
        self.volume.voxel_to_world_f(ijk)[world_axis(plane)]
    }

    fn pick(&mut self, plane: RefinePlane, u: f64, v: f64) {
        let through = self.volume.world_to_voxel_f(self.world)[plane.axes().2];
        self.world = self
            .volume
            .geometry()
            .world_for_plane_uv(plane, u, v, through);
    }

    fn reset_window(&mut self) {
        self.center = self.default_window.0;
        self.width = self.default_window.1;
    }

    fn reset_view(&mut self) {
        for p in &mut self.planes {
            p.zoom = 1.0;
            p.pan = egui::Vec2::ZERO;
        }
        self.reset_window();
    }

    fn visible_planes(&self) -> Vec<RefinePlane> {
        match self.layout {
            Layout::Quad => RefinePlane::ALL.to_vec(),
            Layout::Single(p) => vec![p],
        }
    }

    fn render(&mut self, plane: RefinePlane, ctx: &egui::Context) {
        let index = self.slice_index(plane);
        let key = (index, self.center.to_bits(), self.width.to_bits());
        let pv = &mut self.planes[plane.index()];
        if pv.key == Some(key) && pv.tex.is_some() {
            return;
        }
        let (w, h) =
            self.volume
                .slice_rgba_into(plane, index, self.center, self.width, &mut pv.scratch);
        let image =
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], pv.scratch.rgba());
        upload_texture(
            ctx,
            &mut pv.tex,
            &format!("volume_preview_{}", plane.label()),
            image,
        );
        pv.key = Some(key);
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (axial, coronal, sagittal, quad, reset, up, down) = ctx.input_mut(|i| {
            (
                consume_plain_key(i, egui::Key::Num1),
                consume_plain_key(i, egui::Key::Num2),
                consume_plain_key(i, egui::Key::Num3),
                consume_plain_key(i, egui::Key::Num0),
                consume_plain_key(i, egui::Key::R),
                consume_plain_key(i, egui::Key::PageUp),
                consume_plain_key(i, egui::Key::PageDown),
            )
        });
        if axial {
            self.layout = Layout::Single(RefinePlane::Axial);
        }
        if coronal {
            self.layout = Layout::Single(RefinePlane::Coronal);
        }
        if sagittal {
            self.layout = Layout::Single(RefinePlane::Sagittal);
        }
        if quad {
            self.layout = Layout::Quad;
        }
        if reset {
            self.reset_view();
        }
        if let Layout::Single(p) = self.layout {
            if up {
                self.step_slice(p, 1);
            }
            if down {
                self.step_slice(p, -1);
            }
        }
    }

    fn header(&self, ui: &mut egui::Ui, file: &str) {
        let d = self.volume.dims();
        let s = self.volume.geometry().spacing();
        let (lo, hi) = self.volume.intensity_range();
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(file).strong())
                .on_hover_text("File name of the image being shown");
            ui.label(format!(
                "{} × {} × {} voxels · {:.2} × {:.2} × {:.2} mm · intensity {:.1} to {:.1}",
                d[0], d[1], d[2], s[0], s[1], s[2], lo, hi
            ))
            .on_hover_text("Matrix size, voxel size in millimetres and intensity range");
        });
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let quad = ui
                .selectable_label(self.layout == Layout::Quad, "2×2")
                .on_hover_text("Show all three planes (0)");
            if quad.clicked() {
                self.layout = Layout::Quad;
            }
            for (plane, key) in RefinePlane::ALL.into_iter().zip(["1", "2", "3"]) {
                let on = self.layout == Layout::Single(plane);
                let r = ui
                    .selectable_label(on, plane.label())
                    .on_hover_text(format!("Show only the {} plane ({key})", plane.label()));
                if r.clicked() {
                    self.layout = Layout::Single(plane);
                }
            }
            if ui
                .button("Reset view")
                .on_hover_text("Reset zoom, pan and window / level (R)")
                .clicked()
            {
                self.reset_view();
            }
        });
        let (lo, hi) = self.volume.intensity_range();
        let span = (hi - lo).max(1e-3);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Slider::new(&mut self.width, (span * 0.001).max(1e-3)..=span * 2.0)
                    .text("Window"),
            )
            .on_hover_text("Window: width of the intensity range mapped from black to white");
            ui.add(egui::Slider::new(&mut self.center, lo..=hi.max(lo + 1e-3)).text("Level"))
                .on_hover_text("Level: intensity shown as mid grey");
            if ui
                .button("Reset window")
                .on_hover_text("Back to the automatic window / level for this image")
                .clicked()
            {
                self.reset_window();
            }
        });
    }

    fn info_cell(&self, ui: &mut egui::Ui, file: &str) {
        let d = self.volume.dims();
        let s = self.volume.geometry().spacing();
        let (lo, hi) = self.volume.intensity_range();
        ui.label(egui::RichText::new("Image").strong());
        ui.label(file);
        ui.label(format!("Matrix {} × {} × {}", d[0], d[1], d[2]));
        ui.label(format!("Voxel {:.2} × {:.2} × {:.2} mm", s[0], s[1], s[2]));
        ui.label(format!("Intensity {lo:.1} to {hi:.1}"));
        ui.label(if self.volume.is_ct() {
            "Looks like CT (bone window)"
        } else {
            "Automatic window from percentiles"
        });
        ui.label(format!(
            "Cursor x {:+.1}, y {:+.1}, z {:+.1} mm",
            self.world[0], self.world[1], self.world[2]
        ));
    }

    fn slice_row(&mut self, ui: &mut egui::Ui, plane: RefinePlane, width: f32) {
        let n = self.slice_count(plane);
        let mut idx = self.slice_index(plane);
        let em = ui.text_style_height(&egui::TextStyle::Body);
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = (width * 0.35).max(em * 5.0);
            let r = ui
                .add(
                    egui::Slider::new(&mut idx, 0..=n.saturating_sub(1))
                        .text("Index")
                        .show_value(false),
                )
                .on_hover_text(format!(
                    "Slice index along the {} axis; the mouse wheel over the image steps through slices",
                    plane.label()
                ));
            if r.changed() {
                self.set_slice(plane, idx);
            }
            ui.label(format!(
                "{} · {} {:+.1} mm",
                self.slice_index(plane),
                world_axis_letter(plane),
                self.slice_mm(plane)
            ));
        });
    }

    fn pane(&mut self, ui: &mut egui::Ui, plane: RefinePlane, rect: egui::Rect) {
        let id = ui.id().with(("volume_preview_pane", plane.index()));
        let response = ui.interact(
            rect,
            id,
            egui::Sense::click_and_drag().union(egui::Sense::hover()),
        );
        let pi = plane.index();
        let Some(tex) = self.planes[pi].tex.clone() else {
            ui.painter()
                .rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            return;
        };
        let tex_sz = tex.size_vec2();
        let avail = rect.size();

        let shift = ui.ctx().input(|i| i.modifiers.shift);
        let panning = response.dragged_by(egui::PointerButton::Secondary)
            || (shift && response.dragged_by(egui::PointerButton::Primary));
        if panning {
            self.planes[pi].pan += response.drag_delta();
        }

        if response.hovered() {
            let (sc, pinch, ctrl) = ui
                .ctx()
                .input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.modifiers.ctrl));
            let pinch_active = (pinch - 1.0).abs() > 1e-4;
            if pinch_active {
                adjust_zoom_pinch(&mut self.planes[pi].zoom, pinch);
            } else if ctrl && sc != 0.0 {
                adjust_zoom(&mut self.planes[pi].zoom, sc);
            } else if sc != 0.0 {
                self.scroll_acc += sc;
                let steps = (self.scroll_acc / TOUCHPAD_POINTS_PER_SLICE).trunc();
                if steps != 0.0 {
                    self.scroll_acc -= steps * TOUCHPAD_POINTS_PER_SLICE;
                    self.step_slice(plane, steps as i64);
                }
            }
        }

        let zoom = self.planes[pi].zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        let mut display = scale_tex_to_panel(tex_sz, avail) * zoom;
        display.x = display.x.max(1.0);
        display.y = display.y.max(1.0);
        let top_left = rect.min + (avail - display) * 0.5 + self.planes[pi].pan;
        let img_rect = egui::Rect::from_min_size(top_left, display);

        let picking = !shift
            && (response.dragged_by(egui::PointerButton::Primary)
                || response.clicked_by(egui::PointerButton::Primary));
        if picking {
            if let Some(p) = response.interact_pointer_pos() {
                if img_rect.contains(p) {
                    let u = ((p.x - img_rect.min.x) / img_rect.width().max(1.0)).clamp(0.0, 1.0);
                    let v = ((p.y - img_rect.min.y) / img_rect.height().max(1.0)).clamp(0.0, 1.0);
                    self.pick(plane, f64::from(u), f64::from(1.0 - v));
                }
            }
        }

        let clip = rect.intersect(ui.clip_rect());
        let painter = ui.painter().with_clip_rect(clip);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        painter.image(
            tex.id(),
            img_rect,
            SLICE_TEX_UV_FLIPPED_Y,
            egui::Color32::WHITE,
        );
        let (u, v, _) =
            self.volume
                .geometry()
                .plane_uv_for_world(plane, self.world, self.slice_index(plane));
        let x = img_rect.min.x + u as f32 * img_rect.width();
        let y = img_rect.min.y + (1.0 - v as f32) * img_rect.height();
        let stroke = egui::Stroke::new(1.0_f32, ui.visuals().warn_fg_color);
        painter.line_segment(
            [egui::pos2(x, img_rect.min.y), egui::pos2(x, img_rect.max.y)],
            stroke,
        );
        painter.line_segment(
            [egui::pos2(img_rect.min.x, y), egui::pos2(img_rect.max.x, y)],
            stroke,
        );
    }

    fn plane_cell(&mut self, ui: &mut egui::Ui, plane: RefinePlane, cell: egui::Vec2) {
        ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_size(cell);
            ui.set_max_size(cell);
            ui.label(egui::RichText::new(plane.label()).strong());
            let em = ui.text_style_height(&egui::TextStyle::Body);
            let row_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
            let pane_h = (ui.available_height() - row_h).max(em * 4.0);
            let size = egui::vec2(ui.available_width().max(1.0), pane_h);
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            self.pane(ui, plane, rect);
            self.slice_row(ui, plane, cell.x);
        });
    }

    fn show(&mut self, ui: &mut egui::Ui, file: &str) {
        let outer = ui.max_rect();
        if ui.rect_contains_pointer(outer) {
            self.handle_keys(ui.ctx());
        }
        self.header(ui, file);
        self.controls(ui);
        ui.separator();
        for plane in self.visible_planes() {
            self.render(plane, ui.ctx());
        }
        let gap = ui.spacing().item_spacing;
        let em = ui.text_style_height(&egui::TextStyle::Body);
        let inner = ui.available_size();
        match self.layout {
            Layout::Single(plane) => {
                let cell = egui::vec2(inner.x.max(em * 6.0), inner.y.max(em * 10.0));
                self.plane_cell(ui, plane, cell);
            }
            Layout::Quad => {
                let cell = egui::vec2(
                    ((inner.x - gap.x) * 0.5).max(em * 8.0),
                    ((inner.y - gap.y) * 0.5).max(em * 10.0),
                );
                ui.horizontal(|ui| {
                    self.plane_cell(ui, RefinePlane::Axial, cell);
                    self.plane_cell(ui, RefinePlane::Sagittal, cell);
                });
                ui.horizontal(|ui| {
                    self.plane_cell(ui, RefinePlane::Coronal, cell);
                    ui.allocate_ui_with_layout(
                        cell,
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_size(cell);
                            self.info_cell(ui, file);
                        },
                    );
                });
            }
        }
    }
}

impl VolumePreview {
    pub fn open(path: PathBuf, title: String, ct_hint: Option<bool>, ctx: &egui::Context) -> Self {
        let file = file_name_of(&path);
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        let worker_path = path.clone();
        let spawned = std::thread::Builder::new()
            .name("volume-preview-load".into())
            .spawn(move || {
                let result = RefineVolume::load(&worker_path)
                    .map(|v| v.with_ct_hint(ct_hint))
                    .map_err(|e| e.to_string());
                let _ = tx.send(result);
                repaint.request_repaint();
            });
        let state = match spawned {
            Ok(_) => State::Loading { rx },
            Err(e) => State::Failed(format!("could not start the loader: {e}")),
        };
        Self {
            title,
            path,
            file,
            state,
        }
    }

    pub fn from_volume(path: PathBuf, title: String, volume: RefineVolume) -> Self {
        let file = file_name_of(&path);
        Self {
            title,
            path,
            file,
            state: State::Ready(Box::new(Ready::new(volume))),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_name(&self) -> &str {
        &self.file
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.state, State::Loading { .. })
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready(_))
    }

    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            State::Failed(m) => Some(m),
            _ => None,
        }
    }

    fn apply_result(&mut self, result: LoadResult) {
        self.state = match result {
            Ok(volume) => State::Ready(Box::new(Ready::new(volume))),
            Err(message) => State::Failed(message),
        };
    }

    fn poll(&mut self) {
        let outcome = match &self.state {
            State::Loading { rx } => match rx.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("loading stopped unexpectedly".to_string()))
                }
            },
            _ => None,
        };
        if let Some(result) = outcome {
            self.apply_result(result);
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.poll();
        let file = self.file.clone();
        match &mut self.state {
            State::Loading { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Loading {file}"));
                });
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            State::Failed(message) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Could not open {file}"),
                );
                ui.label(message.as_str());
                if ui
                    .button("Copy message")
                    .on_hover_text("Copy the error message to the clipboard")
                    .clicked()
                {
                    let text = format!("{file}: {message}");
                    ui.ctx().copy_text(text);
                }
            }
            State::Ready(ready) => ready.show(ui, &file),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refine_volume::Affine;

    fn oblique_affine() -> Affine {
        let (s, c) = (0.5_f64.sin(), 0.5_f64.cos());
        [
            [0.8 * c, -1.1 * s, 0.0, -20.0],
            [0.8 * s, 1.1 * c, 0.0, 15.0],
            [0.0, 0.0, 1.5, -30.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn oblique_volume() -> RefineVolume {
        let dims = [20usize, 24, 16];
        let data: Vec<f32> = (0..dims[0] * dims[1] * dims[2])
            .map(|n| (n % 97) as f32)
            .collect();
        RefineVolume::from_parts(dims, oblique_affine(), data).unwrap()
    }

    fn preview() -> VolumePreview {
        VolumePreview::from_volume(
            PathBuf::from("a/sub-1_T1w.nii.gz"),
            "t".into(),
            oblique_volume(),
        )
    }

    fn ready(p: &mut VolumePreview) -> &mut Ready {
        match &mut p.state {
            State::Ready(r) => r,
            _ => panic!("not ready"),
        }
    }

    #[test]
    fn state_machine_moves_from_loading_to_ready_or_failed() {
        let (_tx, rx) = mpsc::channel();
        let mut p = VolumePreview {
            title: "t".into(),
            path: PathBuf::from("x/y.nii"),
            file: "y.nii".into(),
            state: State::Loading { rx },
        };
        assert!(p.is_loading());
        p.poll();
        assert!(p.is_loading());
        p.apply_result(Ok(oblique_volume()));
        assert!(p.is_ready());

        let (tx, rx) = mpsc::channel::<LoadResult>();
        drop(tx);
        let mut p = VolumePreview {
            title: "t".into(),
            path: PathBuf::from("x/y.nii"),
            file: "y.nii".into(),
            state: State::Loading { rx },
        };
        p.poll();
        assert_eq!(p.failure(), Some("loading stopped unexpectedly"));

        p.apply_result(Err("bad header".into()));
        assert_eq!(p.failure(), Some("bad header"));
        assert_eq!(p.file_name(), "y.nii");
    }

    #[test]
    fn slice_index_is_clamped() {
        let mut p = preview();
        let r = ready(&mut p);
        r.set_slice(RefinePlane::Axial, 9999);
        assert_eq!(r.slice_index(RefinePlane::Axial), 15);
        r.step_slice(RefinePlane::Axial, 50);
        assert_eq!(r.slice_index(RefinePlane::Axial), 15);
        r.step_slice(RefinePlane::Axial, -500);
        assert_eq!(r.slice_index(RefinePlane::Axial), 0);
        r.set_slice(RefinePlane::Sagittal, 7);
        assert_eq!(r.slice_index(RefinePlane::Sagittal), 7);
        assert_eq!(r.slice_index(RefinePlane::Axial), 0);
    }

    #[test]
    fn picking_moves_the_other_planes_on_an_oblique_affine() {
        let mut p = preview();
        let r = ready(&mut p);
        let target = r.volume.voxel_to_world_f([5.0, 9.0, 11.0]);
        let (u, v, _) = r
            .volume
            .geometry()
            .plane_uv_for_world(RefinePlane::Axial, target, 11);
        r.set_slice(RefinePlane::Axial, 11);
        r.pick(RefinePlane::Axial, u, v);
        assert_eq!(r.slice_index(RefinePlane::Axial), 11);
        assert_eq!(r.slice_index(RefinePlane::Sagittal), 5);
        assert_eq!(r.slice_index(RefinePlane::Coronal), 9);
        for (w, t) in r.world.iter().zip(target) {
            assert!((w - t).abs() < 1e-6);
        }
    }

    #[test]
    fn window_reset_restores_the_automatic_window() {
        let mut p = preview();
        let r = ready(&mut p);
        let (c, w) = r.default_window;
        r.center += 10.0;
        r.width *= 3.0;
        r.reset_window();
        assert_eq!((r.center, r.width), (c, w));
        r.planes[0].zoom = 3.0;
        r.center = 1.0;
        r.reset_view();
        assert_eq!(r.planes[0].zoom, 1.0);
        assert_eq!(r.center, c);
    }

    #[test]
    fn renders_a_frame_without_a_gpu() {
        let ctx = egui::Context::default();
        let mut p = preview();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| p.show(ui)).textures_delta.clear();
        }
        let r = ready(&mut p);
        assert!(r.planes.iter().all(|pl| pl.tex.is_some()));
        r.layout = Layout::Single(RefinePlane::Coronal);
        assert_eq!(r.visible_planes(), vec![RefinePlane::Coronal]);
    }

    #[test]
    fn shortcut_rows_are_listed() {
        assert!(shortcut_rows().iter().any(|(k, _)| *k == "R"));
    }
}
