// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::checkreg_view::{ReviewAction, ReviewDecision};
use crate::gesture_zoom::{
    adjust_zoom, adjust_zoom_pinch, adjust_zoom_right_drag, wheel_delta_points,
    SliceScrollAccumulator, ZOOM_MAX, ZOOM_MIN,
};
use crate::slice_ui::{
    consume_plain_key, scale_tex_to_panel, upload_texture, SLICE_TEX_UV_FLIPPED_Y,
};
use crate::{ui_tokens, widgets};
use dbsloc_checkreg::checkreg_pair::{
    gencheckregpair_png, load_prepared_checkreg_volumes_cancellable, pick_ijk_from_plane_uv,
    CheckregSlicePlane, GencheckregpairOptions, PreparedCheckregVols, RawWindowStats, SliceLayers,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckregManifest {
    #[serde(default = "default_manifest_version")]
    pub manifest_version: u32,
    pub subj_id: String,
    #[serde(default)]
    pub active: usize,
    pub slides: Vec<CheckregSlide>,
    #[serde(default)]
    pub subj_dir: Option<PathBuf>,
}

fn default_manifest_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckregSlide {
    pub checkreg_png: PathBuf,
    pub moving: PathBuf,
    pub fixed: PathBuf,
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default)]
    pub modality: Option<String>,
    #[serde(default)]
    pub method_log: Option<PathBuf>,
}

fn default_branch() -> String {
    "coreg".to_string()
}

impl CheckregSlide {
    fn preproc_for_refresh(&self) -> bool {
        !matches!(self.branch.to_lowercase().as_str(), "brainshift")
    }

    fn gopt(&self) -> GencheckregpairOptions {
        GencheckregpairOptions {
            preproc: self.preproc_for_refresh(),
            ..Default::default()
        }
    }
}

pub fn registration_qc_kind_label(branch: &str) -> &'static str {
    if branch.eq_ignore_ascii_case("coreg") {
        "Coregistration QC"
    } else if branch.eq_ignore_ascii_case("norm")
        || branch.eq_ignore_ascii_case("normalize")
        || branch.eq_ignore_ascii_case("normalization")
    {
        "Normalization QC"
    } else if branch.eq_ignore_ascii_case("brainshift") {
        "Brain shift QC"
    } else {
        "Registration QC"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    DuringRun,
    AfterRun,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Triple,
    Single,
}

const SHORTCUT_ROWS: &[(&str, &str)] = &[
    ("A", "Approve this image and go to the next"),
    ("D", "Reject this image and go to the next"),
    ("Left / Right", "Previous / next image"),
    ("Page Up / Page Down", "Previous / next slice"),
    ("1 / 2 / 3", "Single pane: axial / coronal / sagittal"),
    ("0", "Show all three planes"),
    ("R", "Reset zoom and pan"),
];

pub fn shortcut_rows() -> &'static [(&'static str, &'static str)] {
    SHORTCUT_ROWS
}

#[inline]
fn screen_uv_to_volume_uv(u_screen: f32, v_screen_top_down: f32) -> (f32, f32) {
    (
        u_screen.clamp(0.0, 1.0),
        1.0 - v_screen_top_down.clamp(0.0, 1.0),
    )
}

const ORTHO_PLANES: [CheckregSlicePlane; 3] = [
    CheckregSlicePlane::Axial,
    CheckregSlicePlane::Sagittal,
    CheckregSlicePlane::Coronal,
];

const ORTHO_NAMES: [&str; 3] = ["ortho_0", "ortho_1", "ortho_2"];

fn plane_index(plane: CheckregSlicePlane, (ci, cj, ck): (usize, usize, usize)) -> usize {
    match plane {
        CheckregSlicePlane::Axial => ck,
        CheckregSlicePlane::Coronal => cj,
        CheckregSlicePlane::Sagittal => ci,
    }
}

fn in_plane_cursor(
    plane: CheckregSlicePlane,
    (ci, cj, ck): (usize, usize, usize),
) -> (usize, usize) {
    match plane {
        CheckregSlicePlane::Axial => (ci, cj),
        CheckregSlicePlane::Coronal => (ci, ck),
        CheckregSlicePlane::Sagittal => (cj, ck),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayersKey {
    plane: CheckregSlicePlane,
    index: usize,
    raw_wl: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImageKey {
    layers: LayersKey,
    window: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TexKey {
    image: ImageKey,
    crosshair: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug)]
struct SliceView {
    cursor: (usize, usize, usize),
    raw_wl: bool,
    center: f32,
    width: f32,
    crosshair_on: bool,
}

impl SliceView {
    fn layers_key(&self, plane: CheckregSlicePlane) -> LayersKey {
        LayersKey {
            plane,
            index: plane_index(plane, self.cursor),
            raw_wl: self.raw_wl,
        }
    }

    fn image_key(&self, plane: CheckregSlicePlane) -> ImageKey {
        ImageKey {
            layers: self.layers_key(plane),
            window: self
                .raw_wl
                .then_some((self.center.to_bits(), self.width.to_bits())),
        }
    }

    fn tex_key(&self, plane: CheckregSlicePlane) -> TexKey {
        TexKey {
            image: self.image_key(plane),
            crosshair: self
                .crosshair_on
                .then_some(in_plane_cursor(plane, self.cursor)),
        }
    }
}

struct DisplayImage {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

#[derive(Default)]
struct PlaneSlot {
    layers: Option<(LayersKey, SliceLayers)>,
    image: Option<(ImageKey, DisplayImage)>,
    tex: Option<egui::TextureHandle>,
    tex_key: Option<TexKey>,
}

impl PlaneSlot {
    fn sync(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        vols: &PreparedCheckregVols,
        plane: CheckregSlicePlane,
        view: SliceView,
    ) {
        let tex_key = view.tex_key(plane);
        if self.tex_key == Some(tex_key) {
            return;
        }
        let image_key = tex_key.image;
        if self.image.as_ref().map(|(k, _)| *k) != Some(image_key) {
            let layers_key = image_key.layers;
            if self.layers.as_ref().map(|(k, _)| *k) != Some(layers_key) {
                self.image = None;
                self.layers = None;
                match vols.slice_layers(plane, layers_key.index, layers_key.raw_wl) {
                    Ok(layers) => self.layers = Some((layers_key, layers)),
                    Err(e) => {
                        eprintln!("checkreg: slice: {e}");
                        return;
                    }
                }
            }
            let Some((_, layers)) = &self.layers else {
                return;
            };
            let (w, h, rgba) = vols.compose_display(layers, view.center, view.width);
            self.image = Some((image_key, DisplayImage { w, h, rgba }));
        }
        let Some((_, img)) = &self.image else {
            return;
        };
        if img.w == 0 || img.h == 0 {
            return;
        }
        let size = [img.w as usize, img.h as usize];
        let cimg = if tex_key.crosshair.is_some() {
            let mut buf = img.rgba.clone();
            vols.draw_display_crosshair(plane, img.w, img.h, &mut buf, view.cursor);
            egui::ColorImage::from_rgba_unmultiplied(size, &buf)
        } else {
            egui::ColorImage::from_rgba_unmultiplied(size, &img.rgba)
        };
        upload_texture(ctx, &mut self.tex, name, cimg);
        self.tex_key = Some(tex_key);
    }
}

type LoadResult = Result<PreparedCheckregVols, String>;

struct PendingLoad {
    generation: u64,
    rx: mpsc::Receiver<LoadResult>,
}

pub struct CheckregReviewPanel {
    man: CheckregManifest,
    mode: ReviewMode,
    slide_states: Vec<Option<bool>>,
    decisions: Vec<ReviewDecision>,
    vols: Option<PreparedCheckregVols>,
    loaded_slide: Option<usize>,
    load_generation: u64,
    latest_generation: Arc<AtomicU64>,
    pending: Option<PendingLoad>,
    regen_on_next_load: bool,
    approve_rect: Option<egui::Rect>,
    load_error: Option<String>,
    fallback_tex: Option<egui::TextureHandle>,
    ci: usize,
    cj: usize,
    ck: usize,
    layout: LayoutMode,
    plane: CheckregSlicePlane,
    single: PlaneSlot,
    ortho: [PlaneSlot; 3],
    raw_wl: bool,
    wl_center: f32,
    wl_width: f32,
    wl_stats: RawWindowStats,
    zoom_ortho: [f32; 3],
    zoom_single: f32,
    pan_ortho: [egui::Vec2; 3],
    pan_single: egui::Vec2,
    crosshair_on: bool,
    slice_scroll: SliceScrollAccumulator,
}

impl Drop for CheckregReviewPanel {
    fn drop(&mut self) {
        self.latest_generation
            .store(self.load_generation.wrapping_add(1), Ordering::SeqCst);
    }
}

impl CheckregReviewPanel {
    pub fn from_manifest(mut man: CheckregManifest) -> Self {
        let n = man.slides.len();
        man.active = man.active.min(n.saturating_sub(1));
        Self {
            man,
            mode: ReviewMode::AfterRun,
            slide_states: vec![None; n],
            decisions: Vec::new(),
            vols: None,
            loaded_slide: None,
            load_generation: 0,
            latest_generation: Arc::new(AtomicU64::new(0)),
            pending: None,
            regen_on_next_load: false,
            approve_rect: None,
            load_error: None,
            fallback_tex: None,
            ci: 0,
            cj: 0,
            ck: 0,
            layout: LayoutMode::Triple,
            plane: CheckregSlicePlane::Axial,
            single: PlaneSlot::default(),
            ortho: Default::default(),
            raw_wl: true,
            wl_center: 40.0,
            wl_width: 80.0,
            wl_stats: RawWindowStats {
                range: (-1000.0, 3000.0),
                window: (40.0, 80.0),
                is_ct: true,
            },
            zoom_ortho: [1.0, 1.0, 1.0],
            zoom_single: 1.0,
            pan_ortho: [egui::Vec2::ZERO; 3],
            pan_single: egui::Vec2::ZERO,
            crosshair_on: true,
            slice_scroll: SliceScrollAccumulator::default(),
        }
    }

    pub fn from_manifest_path(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("cannot read manifest {}: {e}", path.display()))?;
        let man: CheckregManifest = serde_json::from_str(&text)
            .map_err(|e| format!("bad manifest {}: {e}", path.display()))?;
        Ok(Self::from_manifest(man))
    }

    pub fn manifest(&self) -> &CheckregManifest {
        &self.man
    }

    pub fn active_slide(&self) -> usize {
        self.man.active
    }

    pub fn slide_count(&self) -> usize {
        self.man.slides.len()
    }

    pub fn is_last(&self) -> bool {
        self.man.slides.is_empty() || self.man.active + 1 >= self.man.slides.len()
    }

    pub fn set_active(&mut self, slide: usize) {
        let max = self.man.slides.len().saturating_sub(1);
        self.man.active = slide.min(max);
    }

    pub fn set_slide_states(&mut self, states: Vec<Option<bool>>) {
        self.slide_states = states;
        self.slide_states.resize(self.man.slides.len(), None);
    }

    pub fn slide_states(&self) -> &[Option<bool>] {
        &self.slide_states
    }

    pub fn set_mode(&mut self, mode: ReviewMode) {
        self.mode = mode;
    }

    pub fn mode(&self) -> ReviewMode {
        self.mode
    }

    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn has_volumes(&self) -> bool {
        self.vols.is_some()
    }

    pub fn undecided_count(&self) -> usize {
        (0..self.man.slides.len())
            .filter(|i| self.is_undecided(*i))
            .count()
    }

    pub fn all_decided(&self) -> bool {
        self.undecided_count() == 0
    }

    fn is_undecided(&self, slide: usize) -> bool {
        slide < self.man.slides.len() && self.slide_states.get(slide).copied().flatten().is_none()
    }

    fn completes_set(&self) -> bool {
        let others = (0..self.man.slides.len())
            .filter(|i| *i != self.man.active && self.is_undecided(*i))
            .count();
        others == 0
    }

    fn next_in_order_undecided(&self) -> bool {
        self.is_undecided(self.man.active + 1)
    }

    pub(crate) fn approve_label(&self) -> &'static str {
        if self.completes_set() {
            match self.mode {
                ReviewMode::DuringRun => "Approve & continue run",
                ReviewMode::AfterRun => "Approve & close",
            }
        } else if self.next_in_order_undecided() {
            "Approve & next"
        } else {
            "Approve & next undecided"
        }
    }

    pub(crate) fn reject_label(&self) -> &'static str {
        if self.completes_set() {
            match self.mode {
                ReviewMode::DuringRun => "Reject & stop run",
                ReviewMode::AfterRun => "Reject & close",
            }
        } else if self.next_in_order_undecided() {
            "Reject & next"
        } else {
            "Reject & next undecided"
        }
    }

    pub(crate) fn act(&mut self, action: ReviewAction) {
        if self.man.slides.is_empty() {
            return;
        }
        let slide = self.man.active;
        self.decisions.push(ReviewDecision { slide, action });
        if let Some(state) = self.slide_states.get_mut(slide) {
            *state = Some(matches!(action, ReviewAction::Approve));
        }
        if let Some(next) = (0..self.man.slides.len()).find(|i| self.is_undecided(*i)) {
            self.set_active(next);
        }
    }

    fn slide(&self) -> Option<&CheckregSlide> {
        self.man.slides.get(self.man.active)
    }

    fn clear_slices(&mut self) {
        self.single = PlaneSlot::default();
        self.ortho = Default::default();
    }

    fn start_load(&mut self, ctx: &egui::Context) {
        self.load_generation = self.load_generation.wrapping_add(1);
        self.vols = None;
        self.pending = None;
        self.load_error = None;
        self.fallback_tex = None;
        self.clear_slices();
        let active = self.man.active;
        self.loaded_slide = Some(active);
        let Some(slide) = self.slide().cloned() else {
            return;
        };
        let regen = std::mem::take(&mut self.regen_on_next_load);
        let (tx, rx) = mpsc::channel::<LoadResult>();
        let repaint = ctx.clone();
        let generation = self.load_generation;
        self.latest_generation.store(generation, Ordering::SeqCst);
        let latest = Arc::clone(&self.latest_generation);
        let spawned = std::thread::Builder::new()
            .name("checkreg-load".to_string())
            .spawn(move || {
                let live = || latest.load(Ordering::SeqCst) == generation;
                if !live() {
                    return;
                }
                let opt = slide.gopt();
                if regen {
                    if let Err(e) =
                        gencheckregpair_png(&slide.moving, &slide.fixed, &slide.checkreg_png, opt)
                    {
                        eprintln!("checkreg: refresh: {e}");
                    }
                    if !live() {
                        return;
                    }
                }
                let result = load_prepared_checkreg_volumes_cancellable(
                    &slide.moving,
                    &slide.fixed,
                    &slide.gopt(),
                    &live,
                )
                .map_err(|e| e.to_string());
                if !live() {
                    return;
                }
                let _ = tx.send(result);
                repaint.request_repaint();
            });
        match spawned {
            Ok(_) => self.pending = Some(PendingLoad { generation, rx }),
            Err(e) => self.load_error = Some(format!("cannot start loader: {e}")),
        }
    }

    fn ensure_loaded(&mut self, ctx: &egui::Context) {
        if self.man.slides.is_empty() {
            return;
        }
        if self.loaded_slide != Some(self.man.active) {
            self.start_load(ctx);
        }
    }

    fn poll_load(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending else {
            return;
        };
        let generation = pending.generation;
        let received = match pending.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("loader thread stopped unexpectedly".to_string()))
            }
        };
        let Some(result) = received else {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        };
        self.pending = None;
        if generation != self.load_generation {
            return;
        }
        match result {
            Ok(p) => {
                let (n0, n1, n2) = p.dims();
                self.ci = n0 / 2;
                self.cj = n1 / 2;
                self.ck = n2 / 2;
                self.pan_ortho = [egui::Vec2::ZERO; 3];
                self.pan_single = egui::Vec2::ZERO;
                self.wl_stats = p.raw_window_stats();
                (self.wl_center, self.wl_width) = self.wl_stats.window;
                self.clear_slices();
                self.vols = Some(p);
            }
            Err(e) => {
                self.load_error = Some(e);
                self.fallback_tex = self.load_fallback_png(ctx);
            }
        }
    }

    fn load_fallback_png(&self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        let slide = self.slide()?;
        let data = fs::read(&slide.checkreg_png).ok()?;
        let img = image::load_from_memory(&data).ok()?.to_rgba8();
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [img.width() as usize, img.height() as usize],
            img.as_raw(),
        );
        Some(ctx.load_texture("checkreg_fallback_png", color, Default::default()))
    }

    fn clamp_cursor(&mut self) {
        let Some(v) = &self.vols else { return };
        let (n0, n1, n2) = v.dims();
        if n0 > 0 {
            self.ci = self.ci.min(n0 - 1);
        }
        if n1 > 0 {
            self.cj = self.cj.min(n1 - 1);
        }
        if n2 > 0 {
            self.ck = self.ck.min(n2 - 1);
        }
    }

    fn slice_view(&self) -> SliceView {
        SliceView {
            cursor: (self.ci, self.cj, self.ck),
            raw_wl: self.raw_wl,
            center: self.wl_center,
            width: self.wl_width,
            crosshair_on: self.crosshair_on,
        }
    }

    fn sync_triple_textures(&mut self, ctx: &egui::Context) {
        self.clamp_cursor();
        let view = self.slice_view();
        let Some(vols) = self.vols.as_ref() else {
            return;
        };
        for ((slot, plane), name) in self.ortho.iter_mut().zip(ORTHO_PLANES).zip(ORTHO_NAMES) {
            slot.sync(ctx, name, vols, plane, view);
        }
    }

    fn sync_single_texture(&mut self, ctx: &egui::Context) {
        self.clamp_cursor();
        let view = self.slice_view();
        let Some(vols) = self.vols.as_ref() else {
            return;
        };
        self.single
            .sync(ctx, "checkreg_single", vols, self.plane, view);
    }

    fn reset_view(&mut self) {
        self.zoom_ortho = [1.0, 1.0, 1.0];
        self.zoom_single = 1.0;
        self.pan_ortho = [egui::Vec2::ZERO; 3];
        self.pan_single = egui::Vec2::ZERO;
    }

    fn step_slice(&mut self, step: i64) {
        let plane = match self.layout {
            LayoutMode::Triple => CheckregSlicePlane::Axial,
            LayoutMode::Single => self.plane,
        };
        self.step_plane(plane, step);
    }

    fn step_plane(&mut self, plane: CheckregSlicePlane, step: i64) {
        let Some(vols) = self.vols.as_ref() else {
            return;
        };
        let (n0, n1, n2) = vols.dims();
        let bump = |cur: usize, n: usize| {
            (cur as i64 + step).clamp(0, n.saturating_sub(1) as i64) as usize
        };
        match plane {
            CheckregSlicePlane::Axial => self.ck = bump(self.ck, n2),
            CheckregSlicePlane::Sagittal => self.ci = bump(self.ci, n0),
            CheckregSlicePlane::Coronal => self.cj = bump(self.cj, n1),
        }
    }

    fn single_plane(&mut self, plane: CheckregSlicePlane) {
        self.layout = LayoutMode::Single;
        self.plane = plane;
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (
            approve,
            reject,
            prev,
            next,
            page_up,
            page_down,
            axial,
            coronal,
            sagittal,
            triple,
            reset,
        ) = ctx.input_mut(|i| {
            (
                consume_plain_key(i, egui::Key::A),
                consume_plain_key(i, egui::Key::D),
                consume_plain_key(i, egui::Key::ArrowLeft),
                consume_plain_key(i, egui::Key::ArrowRight),
                consume_plain_key(i, egui::Key::PageUp),
                consume_plain_key(i, egui::Key::PageDown),
                consume_plain_key(i, egui::Key::Num1),
                consume_plain_key(i, egui::Key::Num2),
                consume_plain_key(i, egui::Key::Num3),
                consume_plain_key(i, egui::Key::Num0),
                consume_plain_key(i, egui::Key::R),
            )
        });
        if approve {
            self.act(ReviewAction::Approve);
        }
        if reject {
            self.act(ReviewAction::Reject);
        }
        if prev {
            self.set_active(self.man.active.saturating_sub(1));
        }
        if next {
            self.set_active(self.man.active + 1);
        }
        if page_up {
            self.step_slice(1);
        }
        if page_down {
            self.step_slice(-1);
        }
        if axial {
            self.single_plane(CheckregSlicePlane::Axial);
        }
        if coronal {
            self.single_plane(CheckregSlicePlane::Coronal);
        }
        if sagittal {
            self.single_plane(CheckregSlicePlane::Sagittal);
        }
        if triple {
            self.layout = LayoutMode::Triple;
        }
        if reset {
            self.reset_view();
        }
    }

    fn plane_label(pl: CheckregSlicePlane) -> &'static str {
        match pl {
            CheckregSlicePlane::Axial => "Axial (k · dim2)",
            CheckregSlicePlane::Coronal => "Coronal (j · dim1)",
            CheckregSlicePlane::Sagittal => "Sagittal (i · dim0)",
        }
    }

    fn image_scroll_sized(
        ui: &mut egui::Ui,
        t: &egui::TextureHandle,
        tex_dim: egui::Vec2,
        display: egui::Vec2,
    ) {
        let fill = ui.max_rect().size();
        egui::ScrollArea::both()
            .min_scrolled_width(fill.x)
            .min_scrolled_height(fill.y)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_size(display);
                ui.add(egui::Image::new((t.id(), tex_dim)).fit_to_exact_size(display));
            });
    }

    fn interactive_slice_cell(
        &mut self,
        ui: &mut egui::Ui,
        paint_rect: egui::Rect,
        interact_id: egui::Id,
        t: &egui::TextureHandle,
        plane: CheckregSlicePlane,
        ortho_slot: Option<usize>,
    ) {
        let tex_sz = t.size_vec2();
        if paint_rect.width() < 1.0 || paint_rect.height() < 1.0 || tex_sz.x < 1.0 || tex_sz.y < 1.0
        {
            return;
        }

        let response = ui.interact(
            paint_rect,
            interact_id,
            egui::Sense::click_and_drag().union(egui::Sense::hover()),
        );

        if self.raw_wl {
            let shift = ui.ctx().input(|i| i.modifiers.shift);
            let alt = ui.ctx().input(|i| i.modifiers.alt);
            if shift && !alt && response.dragged_by(egui::PointerButton::Primary) {
                let d = response.drag_delta();
                let (min_width, _) = self.width_range();
                let gain = self.drag_gain();
                self.wl_center += d.y * 0.55 * gain;
                self.wl_width = (self.wl_width + d.x * 0.75 * gain).max(min_width);
            }
        }

        let pan_allowed = match ortho_slot {
            Some(i) => self.zoom_ortho[i] > 1.01,
            None => self.zoom_single > 1.01,
        };
        if pan_allowed {
            let mid = response.dragged_by(egui::PointerButton::Middle);
            let alt_pri = response.dragged_by(egui::PointerButton::Primary)
                && ui.ctx().input(|i| i.modifiers.alt);
            if mid || alt_pri {
                let d = response.drag_delta() * 1.35;
                match ortho_slot {
                    Some(i) => self.pan_ortho[i] += d,
                    None => self.pan_single += d,
                }
            }
        }

        if response.dragged_by(egui::PointerButton::Secondary) {
            let dy = response.drag_delta().y;
            match ortho_slot {
                Some(i) => adjust_zoom_right_drag(&mut self.zoom_ortho[i], dy),
                None => adjust_zoom_right_drag(&mut self.zoom_single, dy),
            }
            ui.ctx().request_repaint();
        }

        let (z_for_geom, pan_for_geom) = match ortho_slot {
            Some(i) => (self.zoom_ortho[i], self.pan_ortho[i]),
            None => (self.zoom_single, self.pan_single),
        };

        let picking = self.crosshair_on
            && !ui.ctx().input(|i| i.modifiers.shift)
            && !ui.ctx().input(|i| i.modifiers.alt);
        if picking {
            let drag_pri = response.dragged_by(egui::PointerButton::Primary);
            let click_pri = response.clicked_by(egui::PointerButton::Primary);
            if drag_pri || click_pri {
                if let Some(p) = response.interact_pointer_pos() {
                    let avail = paint_rect.size();
                    let mut disp = scale_tex_to_panel(tex_sz, avail) * z_for_geom;
                    disp.x = disp.x.max(1.0);
                    disp.y = disp.y.max(1.0);
                    let top_left = paint_rect.min + (avail - disp) * 0.5 + pan_for_geom;
                    let img_rect = egui::Rect::from_min_size(top_left, disp);
                    if img_rect.contains(p) {
                        let u_s = (p.x - img_rect.min.x) / img_rect.width().max(1.0);
                        let v_s = (p.y - img_rect.min.y) / img_rect.height().max(1.0);
                        let (u, v) = screen_uv_to_volume_uv(u_s, v_s);
                        if let Some(vols) = self.vols.as_ref() {
                            let (n0, n1, n2) = vols.dims();
                            let (ni, nj, nk) = pick_ijk_from_plane_uv(
                                plane, n0, n1, n2, u, v, self.ci, self.cj, self.ck,
                            );
                            self.ci = ni;
                            self.cj = nj;
                            self.ck = nk;
                            self.clamp_cursor();
                        }
                    }
                }
            }
        }

        if response.hovered() {
            let (sc, pinch) = ui
                .ctx()
                .input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let ctrl = ui.ctx().input(|i| i.modifiers.ctrl);
            let pinch_active = (pinch - 1.0).abs() > 1e-4;
            if pinch_active || (ctrl && sc != 0.0) {
                let zoom = match ortho_slot {
                    Some(i) => &mut self.zoom_ortho[i],
                    None => &mut self.zoom_single,
                };
                if pinch_active {
                    adjust_zoom_pinch(zoom, pinch);
                } else {
                    adjust_zoom(zoom, sc);
                }
                self.clamp_cursor();
            } else {
                let alt = ui.ctx().input(|i| i.modifiers.alt);
                let wheel: Vec<(egui::MouseWheelUnit, egui::Vec2, egui::Modifiers)> =
                    ui.ctx().input(|i| {
                        i.events
                            .iter()
                            .filter_map(|ev| match ev {
                                egui::Event::MouseWheel {
                                    unit,
                                    delta,
                                    modifiers,
                                    ..
                                } => Some((*unit, *delta, *modifiers)),
                                _ => None,
                            })
                            .collect()
                    });
                let mut steps: i64 = 0;
                let mut pan = egui::Vec2::ZERO;
                for (unit, delta, m) in wheel {
                    if m.ctrl || m.command || m.mac_cmd {
                        continue;
                    }
                    if alt && pan_allowed {
                        pan += wheel_delta_points(unit, delta, paint_rect.height());
                    } else {
                        steps += i64::from(self.slice_scroll.feed(unit, delta));
                    }
                }
                if pan != egui::Vec2::ZERO {
                    match ortho_slot {
                        Some(i) => self.pan_ortho[i] += pan,
                        None => self.pan_single += pan,
                    }
                }
                if steps != 0 {
                    self.step_plane(plane, steps);
                    self.clamp_cursor();
                }
            }
        }

        let z = match ortho_slot {
            Some(i) => self.zoom_ortho[i],
            None => self.zoom_single,
        };
        let pan = match ortho_slot {
            Some(i) => self.pan_ortho[i],
            None => self.pan_single,
        };
        let avail = paint_rect.size();
        let mut display = scale_tex_to_panel(tex_sz, avail) * z;
        display.x = display.x.max(1.0);
        display.y = display.y.max(1.0);
        let top_left = paint_rect.min + (avail - display) * 0.5 + pan;
        let img_rect = egui::Rect::from_min_size(top_left, display);
        let clip = paint_rect.intersect(ui.clip_rect());
        ui.painter().with_clip_rect(clip).image(
            t.id(),
            img_rect,
            SLICE_TEX_UV_FLIPPED_Y,
            egui::Color32::WHITE,
        );
    }

    fn draw_ortho_cell(
        &mut self,
        ui: &mut egui::Ui,
        slot: usize,
        title: &str,
        plane: CheckregSlicePlane,
    ) {
        ui.label(egui::RichText::new(title).strong());
        let rest_h = ui.available_height();
        ui.set_min_height(rest_h.max(1.0));
        let Some(texture) = self.ortho[slot].tex.clone() else {
            ui.weak("…");
            return;
        };
        let paint = ui.available_rect_before_wrap();
        let id = ui.id().with("ortho_slice").with(slot);
        self.interactive_slice_cell(ui, paint, id, &texture, plane, Some(slot));
    }

    fn ortho_2x2_layout(&mut self, ui: &mut egui::Ui) {
        let gap = ui.spacing().item_spacing.x;
        let em = ui.text_style_height(&egui::TextStyle::Body);
        let inner = ui.available_size();
        let row_h = ((inner.y - gap) * 0.5).max(em * 6.0);
        let col_w = ((inner.x - gap) * 0.5).max(em * 6.0);
        let cell = egui::vec2(col_w, row_h);

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_size(cell);
                    self.draw_ortho_cell(ui, 0, "Axial (k)", CheckregSlicePlane::Axial);
                });
                ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_size(cell);
                    self.draw_ortho_cell(ui, 1, "Sagittal (i)", CheckregSlicePlane::Sagittal);
                });
            });
            ui.add_space(gap);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_size(cell);
                    ui.label(egui::RichText::new("3D / fourth pane").strong());
                    ui.weak("Not available in this review.");
                });
                ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_size(cell);
                    self.draw_ortho_cell(ui, 2, "Coronal (j)", CheckregSlicePlane::Coronal);
                });
            });
        });
    }

    fn slide_strip(&mut self, ui: &mut egui::Ui) {
        let pal = ui_tokens::palette_for(ui);
        ui.horizontal_wrapped(|ui| {
            for i in 0..self.man.slides.len() {
                let (glyph, color, state) = match self.slide_states.get(i).copied().flatten() {
                    Some(true) => ("✔", pal.ok, "approved"),
                    Some(false) => ("✖", pal.err, "rejected"),
                    None => ("○", pal.text_muted, "not yet reviewed"),
                };
                let label = format!("{glyph} {}", i + 1);
                let mut text = egui::RichText::new(label).color(color);
                if i == self.man.active {
                    text = text.strong();
                }
                let name = self.man.slides[i]
                    .modality
                    .clone()
                    .unwrap_or_else(|| "image".to_string());
                let resp = ui
                    .selectable_label(i == self.man.active, text)
                    .on_hover_text(format!("Image {} ({name}): {state}. Click to view.", i + 1));
                if resp.clicked() {
                    self.set_active(i);
                }
            }
        });
    }

    fn loading_ui(&self, ui: &mut egui::Ui) {
        let n = self.man.slides.len().max(1);
        let name = self
            .slide()
            .and_then(|s| s.moving.file_name())
            .and_then(|x| x.to_str())
            .unwrap_or("image")
            .to_string();
        let label = format!("Loading {name} (slide {} / {n})", self.man.active + 1);
        ui.add(
            egui::ProgressBar::new(self.man.active as f32 / n as f32)
                .text(label)
                .animate(true),
        );
    }

    fn decision_block(&mut self, ui: &mut egui::Ui) {
        let pal = ui_tokens::palette_for(ui);
        let sref = self.slide().cloned();
        if let Some(s) = &sref {
            let dep = s.moving.file_name().and_then(|x| x.to_str()).unwrap_or("?");
            ui.label(
                egui::RichText::new(format!(
                    "Image {} / {}: {}",
                    self.man.active + 1,
                    self.man.slides.len(),
                    s.modality.as_deref().unwrap_or("?")
                ))
                .strong(),
            )
            .on_hover_text(format!("Moving volume: {dep}"));
            ui.label(egui::RichText::new(dep).weak());
        }
        self.slide_strip(ui);
        let undecided = self.undecided_count();
        let current_decided = !self.is_undecided(self.man.active);
        if undecided > 0 && (self.is_last() || current_decided) {
            let text = if undecided == 1 {
                "1 image still needs a decision".to_owned()
            } else {
                format!("{undecided} images still need a decision")
            };
            ui.label(egui::RichText::new(text).color(pal.warn));
        }
        ui.add_space(ui.spacing().item_spacing.y);
        let approve_text = self.approve_label();
        let reject_text = self.reject_label();
        let btn_h = ui.text_style_height(&egui::TextStyle::Button) * 2.0;
        let side_by_side = ui.available_width() >= btn_h * 9.0;
        let width = if side_by_side {
            (ui.available_width() - ui.spacing().item_spacing.x) / 2.0
        } else {
            ui.available_width()
        };
        let size = egui::vec2(width, btn_h);
        let mut approve_clicked = false;
        let mut reject_clicked = false;
        let mut layout = |ui: &mut egui::Ui| {
            let ok = egui::Button::new(
                egui::RichText::new(approve_text)
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .fill(pal.ok_fill)
            .min_size(size);
            let r = ui
                .add(ok)
                .on_hover_text("Approve this registration (key A).");
            self.approve_rect = Some(r.rect);
            approve_clicked = r.clicked();
            let bad = egui::Button::new(
                egui::RichText::new(reject_text)
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .fill(pal.err_fill)
            .min_size(size);
            reject_clicked = ui
                .add(bad)
                .on_hover_text(
                    "Reject this registration (key D). One rejected image stops the run and marks the step for rework.",
                )
                .clicked();
        };
        if side_by_side {
            ui.horizontal(&mut layout);
        } else {
            ui.vertical(&mut layout);
        }
        if approve_clicked {
            self.act(ReviewAction::Approve);
        } else if reject_clicked {
            self.act(ReviewAction::Reject);
        }
        ui.add_space(ui.spacing().item_spacing.y * 0.5);
        ui.horizontal_wrapped(|ui| {
            let can_back = self.man.active > 0;
            if ui
                .add_enabled(can_back, egui::Button::new("Back"))
                .on_hover_text("Show the previous image without deciding (Left arrow).")
                .clicked()
            {
                self.set_active(self.man.active - 1);
            }
            if ui
                .add_enabled(!self.is_last(), egui::Button::new("Next"))
                .on_hover_text("Show the next image without deciding (Right arrow).")
                .clicked()
            {
                self.set_active(self.man.active + 1);
            }
            if ui
                .button("Refresh")
                .on_hover_text("Regenerate the montage image and reload the volumes.")
                .clicked()
            {
                self.regen_on_next_load = true;
                self.loaded_slide = None;
            }
        });
        ui.separator();
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let pal = ui_tokens::palette_for(ui);
        if self.pending.is_some() {
            self.loading_ui(ui);
        } else if let Some(e) = &self.load_error {
            ui.colored_label(pal.err, format!("NIfTI: {e}"));
        } else if let Some(v) = &self.vols {
            let (d0, d1, d2) = v.dims();
            let sp = v.spacing();
            ui.label(format!(
                "Grid: {d0}×{d1}×{d2} · spacing mm {:.3} × {:.3} × {:.3}",
                sp[0], sp[1], sp[2]
            ));
        }
        ui.separator();
        ui.label(egui::RichText::new("Layout").strong());
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.layout, LayoutMode::Triple, "2×2 ortho")
                .on_hover_text("Show axial, sagittal and coronal planes together (key 0).");
            ui.selectable_value(&mut self.layout, LayoutMode::Single, "Single pane")
                .on_hover_text("Show one plane at full size (keys 1, 2, 3).");
        });
        ui.checkbox(&mut self.crosshair_on, "Crosshair (linked i, j, k)")
            .on_hover_text("Draw a crosshair at the cursor and move it by dragging in a plane.");
        ui.separator();
        ui.label(egui::RichText::new("Cursor (i, j, k)").strong());
        if let Some(v) = &self.vols {
            let (n0, n1, n2) = v.dims();
            let i0 = n0.saturating_sub(1);
            let j0 = n1.saturating_sub(1);
            let k0 = n2.saturating_sub(1);
            ui.add(egui::Slider::new(&mut self.ci, 0..=i0).text("i (dim0)"))
                .on_hover_text("Sagittal slice index.");
            ui.add(egui::Slider::new(&mut self.cj, 0..=j0).text("j (dim1)"))
                .on_hover_text("Coronal slice index.");
            ui.add(egui::Slider::new(&mut self.ck, 0..=k0).text("k (dim2)"))
                .on_hover_text("Axial slice index.");
        }
        if self.layout == LayoutMode::Single {
            ui.separator();
            ui.label(egui::RichText::new("Single pane orientation").strong());
            egui::ComboBox::from_id_salt("plane")
                .selected_text(Self::plane_label(self.plane))
                .show_ui(ui, |ui| {
                    for pl in [
                        CheckregSlicePlane::Axial,
                        CheckregSlicePlane::Coronal,
                        CheckregSlicePlane::Sagittal,
                    ] {
                        ui.selectable_value(&mut self.plane, pl, Self::plane_label(pl));
                    }
                })
                .response
                .on_hover_text("Choose the plane shown in single pane layout (keys 1, 2, 3).");
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Window / level").strong());
            widgets::info_icon(
                ui,
                "Window / level maps raw image values to grey. Shift + left drag in a plane changes it.",
            );
        });
        ui.checkbox(&mut self.raw_wl, "Raw NIfTI window and level (linear)")
            .on_hover_text(
                "On: window and level apply to the raw moving image. Off: the grey scale of the montage image (percentile scaling).",
            );
        if self.raw_wl {
            let (lo, hi) = self.center_range();
            let (wmin, wmax) = self.width_range();
            ui.add(egui::Slider::new(&mut self.wl_center, lo..=hi).text("Center"))
                .on_hover_text("Window / level centre (image units).");
            ui.add(
                egui::Slider::new(&mut self.wl_width, wmin..=wmax)
                    .logarithmic(true)
                    .text("Width"),
            )
            .on_hover_text("Window / level width (image units).");
            if ui
                .button("Image default")
                .on_hover_text(
                    "Window / level computed from this image: soft tissue for CT, the 0.5 to 99.5 percentile range otherwise.",
                )
                .clicked()
            {
                (self.wl_center, self.wl_width) = self.wl_stats.window;
            }
            if self.wl_stats.is_ct {
                ui.horizontal(|ui| {
                    if ui
                        .button("Soft tissue")
                        .on_hover_text("Window / level preset: centre 40, width 80.")
                        .clicked()
                    {
                        self.wl_center = 40.0;
                        self.wl_width = 80.0;
                    }
                    if ui
                        .button("Bone")
                        .on_hover_text("Window / level preset: centre 300, width 1500.")
                        .clicked()
                    {
                        self.wl_center = 300.0;
                        self.wl_width = 1500.0;
                    }
                });
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Zoom").strong());
            widgets::info_icon(
                ui,
                "Scroll: change slice. Ctrl + scroll, pinch or right drag: zoom. Middle drag or Alt + left drag: pan. Left drag: move the crosshair.",
            );
        });
        if self.layout == LayoutMode::Triple {
            ui.add(egui::Slider::new(&mut self.zoom_ortho[0], ZOOM_MIN..=ZOOM_MAX).text("Axial"))
                .on_hover_text("Zoom of the axial plane.");
            ui.add(
                egui::Slider::new(&mut self.zoom_ortho[1], ZOOM_MIN..=ZOOM_MAX).text("Sagittal"),
            )
            .on_hover_text("Zoom of the sagittal plane.");
            ui.add(egui::Slider::new(&mut self.zoom_ortho[2], ZOOM_MIN..=ZOOM_MAX).text("Coronal"))
                .on_hover_text("Zoom of the coronal plane.");
        } else {
            ui.add(
                egui::Slider::new(&mut self.zoom_single, ZOOM_MIN..=ZOOM_MAX).text("Single pane"),
            )
            .on_hover_text("Zoom of the single pane.");
        }
        ui.horizontal(|ui| {
            if ui.button("−").on_hover_text("Zoom out.").clicked() {
                self.zoom_by(1.0 / 1.2);
            }
            if ui.button("+").on_hover_text("Zoom in.").clicked() {
                self.zoom_by(1.2);
            }
            if ui
                .button("Reset zoom and pan")
                .on_hover_text("Fit the image to the pane (key R).")
                .clicked()
            {
                self.reset_view();
            }
        });
    }

    fn center_range(&self) -> (f32, f32) {
        let (lo, hi) = self.wl_stats.range;
        let pad = ((hi - lo) * 0.1).max(1e-3);
        (lo - pad, hi + pad)
    }

    fn width_range(&self) -> (f32, f32) {
        let (lo, hi) = self.wl_stats.range;
        let span = (hi - lo).max(1e-3);
        ((span * 1e-3).max(1e-3), span * 2.0)
    }

    fn drag_gain(&self) -> f32 {
        if self.wl_stats.is_ct {
            1.0
        } else {
            let (lo, hi) = self.wl_stats.range;
            ((hi - lo) / 4000.0).max(1e-6)
        }
    }

    fn zoom_by(&mut self, factor: f32) {
        match self.layout {
            LayoutMode::Triple => {
                for z in &mut self.zoom_ortho {
                    *z = (*z * factor).clamp(ZOOM_MIN, ZOOM_MAX);
                }
            }
            LayoutMode::Single => {
                self.zoom_single = (self.zoom_single * factor).clamp(ZOOM_MIN, ZOOM_MAX);
            }
        }
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let sref = self.slide().cloned();
        let kind = sref
            .as_ref()
            .map(|s| registration_qc_kind_label(&s.branch))
            .unwrap_or("Registration QC");
        if self.pending.is_some() {
            ui.heading(kind);
            self.loading_ui(ui);
            return;
        }
        if let Some(err) = self.load_error.clone() {
            let pal = ui_tokens::palette_for(ui);
            ui.heading(kind);
            match self.fallback_tex.clone() {
                Some(t) => {
                    ui.colored_label(
                        pal.warn,
                        "Volumes could not be loaded; showing the montage image. You can still decide.",
                    )
                    .on_hover_text(err);
                    let available = ui.available_size();
                    let sz = t.size_vec2();
                    let d = scale_tex_to_panel(sz, available);
                    if d.x >= 1.0 && d.y >= 1.0 {
                        Self::image_scroll_sized(ui, &t, sz, d);
                    }
                }
                None => {
                    ui.colored_label(
                        pal.err,
                        format!("Failed to load the volumes and no montage image exists: {err}"),
                    );
                }
            }
            return;
        }
        if self.man.slides.is_empty() {
            ui.label("No images to review.");
            return;
        }
        match self.layout {
            LayoutMode::Triple => {
                self.sync_triple_textures(&ctx);
                ui.heading(kind);
                self.ortho_2x2_layout(ui);
            }
            LayoutMode::Single => {
                self.sync_single_texture(&ctx);
                ui.heading(kind);
                ui.label(egui::RichText::new(Self::plane_label(self.plane)).weak());
                let rest_h = ui.available_height();
                ui.set_min_height(rest_h.max(1.0));
                if let Some(tex) = self.single.tex.clone() {
                    let paint = ui.available_rect_before_wrap();
                    let id = ui.id().with("single_slice");
                    self.interactive_slice_cell(ui, paint, id, &tex, self.plane, None);
                }
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Vec<ReviewDecision> {
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        self.ensure_loaded(&ctx);
        self.poll_load(&ctx);

        let em = ui.text_style_height(&egui::TextStyle::Body);
        let base = ui.id().with("checkreg_review");
        egui::Panel::right(base.with("ctrl"))
            .resizable(true)
            .default_size(em * 22.0)
            .show(ui, |ui| {
                self.decision_block(ui);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.side_panel(ui));
            });
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));

        std::mem::take(&mut self.decisions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn nifti_bytes(n: usize, scale: f32) -> Vec<u8> {
        let mut h = [0u8; 348];
        h[0..4].copy_from_slice(&348i32.to_le_bytes());
        h[40..42].copy_from_slice(&3i16.to_le_bytes());
        h[42..44].copy_from_slice(&(n as i16).to_le_bytes());
        h[44..46].copy_from_slice(&(n as i16).to_le_bytes());
        h[46..48].copy_from_slice(&(n as i16).to_le_bytes());
        for off in [48usize, 50, 52, 54] {
            h[off..off + 2].copy_from_slice(&1i16.to_le_bytes());
        }
        h[70..72].copy_from_slice(&16i16.to_le_bytes());
        h[72..74].copy_from_slice(&32i16.to_le_bytes());
        for off in [80usize, 84, 88] {
            h[off..off + 4].copy_from_slice(&1.0f32.to_le_bytes());
        }
        h[108..112].copy_from_slice(&352.0f32.to_le_bytes());
        h[254..256].copy_from_slice(&1i16.to_le_bytes());
        for off in [280usize, 300, 320] {
            h[off..off + 4].copy_from_slice(&1.0f32.to_le_bytes());
        }
        h[344] = b'n';
        h[345] = b'+';
        h[346] = b'1';
        let mut out = Vec::new();
        out.extend_from_slice(&h);
        out.extend_from_slice(&[0u8; 4]);
        for i in 0..n * n * n {
            out.extend_from_slice(&(i as f32 * scale).to_le_bytes());
        }
        out
    }

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("dbsloc_checkreg_app_{tag}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create fixture dir");
            Self { dir }
        }

        fn volume(&self, name: &str) -> PathBuf {
            let p = self.dir.join(name);
            fs::write(&p, nifti_bytes(6, 0.02)).expect("write nifti");
            p
        }

        fn manifest(&self, n: usize) -> CheckregManifest {
            let slides = (0..n)
                .map(|i| CheckregSlide {
                    checkreg_png: self.dir.join(format!("s{i}.png")),
                    moving: self.volume(&format!("mov{i}.nii")),
                    fixed: self.volume(&format!("fix{i}.nii")),
                    branch: "coreg".into(),
                    modality: Some(format!("M{i}")),
                    method_log: None,
                })
                .collect();
            CheckregManifest {
                manifest_version: 1,
                subj_id: "synthetic".into(),
                active: 0,
                slides,
                subj_dir: None,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn frame(
        ctx: &egui::Context,
        panel: &mut CheckregReviewPanel,
        events: Vec<egui::Event>,
    ) -> Vec<ReviewDecision> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = Vec::new();
        ctx.run_ui(input, |ui| {
            out = panel.show(ui);
        })
        .textures_delta
        .clear();
        out
    }

    fn wait_loaded(ctx: &egui::Context, panel: &mut CheckregReviewPanel) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            frame(ctx, panel, Vec::new());
            if !panel.is_loading() {
                return;
            }
            assert!(Instant::now() < deadline, "load timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    #[test]
    fn loads_in_background_and_approve_advances() {
        let fx = Fixture::new("approve");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(2));
        let ctx = egui::Context::default();
        frame(&ctx, &mut panel, Vec::new());
        assert!(panel.is_loading() || panel.has_volumes());
        wait_loaded(&ctx, &mut panel);
        assert!(panel.has_volumes());
        assert!(panel.load_error().is_none());

        panel.act(ReviewAction::Approve);
        let decisions = frame(&ctx, &mut panel, Vec::new());
        assert_eq!(
            decisions,
            vec![ReviewDecision {
                slide: 0,
                action: ReviewAction::Approve
            }]
        );
        assert_eq!(panel.active_slide(), 1);
        assert_eq!(panel.slide_states(), &[Some(true), None]);
        assert!(panel.is_loading() || panel.has_volumes());
        wait_loaded(&ctx, &mut panel);
        assert!(panel.has_volumes());

        panel.act(ReviewAction::Reject);
        let decisions = frame(&ctx, &mut panel, Vec::new());
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].slide, 1);
        assert_eq!(decisions[0].action, ReviewAction::Reject);
        assert_eq!(panel.active_slide(), 1);
        assert_eq!(panel.slide_states(), &[Some(true), Some(false)]);
    }

    #[test]
    fn decision_buttons_stay_on_screen_in_short_window() {
        let fx = Fixture::new("short");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(2));
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 360.0));
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                panel.show(ui);
            })
            .textures_delta
            .clear();
        }
        let r = panel.approve_rect.expect("approve button drawn");
        assert!(
            screen.contains_rect(r),
            "approve button {r:?} outside {screen:?}"
        );
    }

    #[test]
    fn labels_follow_how_many_images_are_undecided() {
        let fx = Fixture::new("labels");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(3));
        assert_eq!(panel.approve_label(), "Approve & next");
        assert_eq!(panel.reject_label(), "Reject & next");
        panel.set_active(2);
        assert_eq!(panel.approve_label(), "Approve & next undecided");
        assert_eq!(panel.reject_label(), "Reject & next undecided");
        panel.set_slide_states(vec![Some(true), None, None]);
        assert_eq!(panel.approve_label(), "Approve & next undecided");
        panel.set_slide_states(vec![Some(true), Some(false), None]);
        panel.set_mode(ReviewMode::DuringRun);
        assert_eq!(panel.approve_label(), "Approve & continue run");
        assert_eq!(panel.reject_label(), "Reject & stop run");
        panel.set_mode(ReviewMode::AfterRun);
        assert_eq!(panel.approve_label(), "Approve & close");
        assert_eq!(panel.reject_label(), "Reject & close");
    }

    #[test]
    fn deciding_moves_to_the_first_undecided_image() {
        let fx = Fixture::new("firstundecided");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(4));
        panel.set_active(2);
        panel.act(ReviewAction::Approve);
        assert_eq!(panel.active_slide(), 0);
        panel.act(ReviewAction::Reject);
        assert_eq!(panel.active_slide(), 1);
        panel.act(ReviewAction::Approve);
        assert_eq!(panel.active_slide(), 3);
        assert_eq!(panel.undecided_count(), 1);
        assert!(!panel.all_decided());
        panel.act(ReviewAction::Approve);
        assert!(panel.all_decided());
        assert_eq!(panel.active_slide(), 3);
    }

    #[test]
    fn modified_letter_keys_do_not_decide() {
        let fx = Fixture::new("modkeys");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(2));
        let ctx = egui::Context::default();
        wait_loaded(&ctx, &mut panel);
        for modifiers in [
            egui::Modifiers::SHIFT,
            egui::Modifiers::CTRL,
            egui::Modifiers::ALT,
        ] {
            let events = vec![egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers,
            }];
            assert!(frame(&ctx, &mut panel, events).is_empty());
        }
        assert_eq!(frame(&ctx, &mut panel, key(egui::Key::A)).len(), 1);
    }

    #[test]
    fn a_newer_load_supersedes_the_running_one() {
        let fx = Fixture::new("supersede");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(2));
        let ctx = egui::Context::default();
        panel.start_load(&ctx);
        let first = panel.load_generation;
        assert_eq!(panel.latest_generation.load(Ordering::SeqCst), first);
        panel.start_load(&ctx);
        assert_eq!(panel.latest_generation.load(Ordering::SeqCst), first + 1);
        let latest = Arc::clone(&panel.latest_generation);
        let stale = move || latest.load(Ordering::SeqCst) == first;
        let slide = panel.man.slides[0].clone();
        let r = load_prepared_checkreg_volumes_cancellable(
            &slide.moving,
            &slide.fixed,
            &slide.gopt(),
            &stale,
        );
        assert!(r.is_err());
        drop(panel);
    }

    #[test]
    fn keyboard_shortcuts_decide_and_navigate() {
        let fx = Fixture::new("keys");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(3));
        let ctx = egui::Context::default();
        wait_loaded(&ctx, &mut panel);

        let d = frame(&ctx, &mut panel, key(egui::Key::A));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].action, ReviewAction::Approve);
        assert_eq!(panel.active_slide(), 1);

        let d = frame(&ctx, &mut panel, key(egui::Key::D));
        assert_eq!(d[0].slide, 1);
        assert_eq!(d[0].action, ReviewAction::Reject);
        assert_eq!(panel.active_slide(), 2);

        frame(&ctx, &mut panel, key(egui::Key::ArrowLeft));
        assert_eq!(panel.active_slide(), 1);
        frame(&ctx, &mut panel, key(egui::Key::ArrowRight));
        assert_eq!(panel.active_slide(), 2);
        frame(&ctx, &mut panel, key(egui::Key::ArrowRight));
        assert_eq!(panel.active_slide(), 2);

        frame(&ctx, &mut panel, key(egui::Key::Num2));
        assert!(panel.layout == LayoutMode::Single);
        assert!(panel.plane == CheckregSlicePlane::Coronal);
        frame(&ctx, &mut panel, key(egui::Key::Num0));
        assert!(panel.layout == LayoutMode::Triple);
    }

    #[test]
    fn slide_states_are_resized_and_applied() {
        let fx = Fixture::new("states");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(3));
        panel.set_slide_states(vec![Some(true)]);
        assert_eq!(panel.slide_states(), &[Some(true), None, None]);
        assert_eq!(panel.slide_count(), 3);
        panel.set_active(99);
        assert_eq!(panel.active_slide(), 2);
        assert!(panel.is_last());
    }

    #[test]
    fn load_error_without_png_still_allows_decisions() {
        let fx = Fixture::new("error");
        let mut man = fx.manifest(1);
        man.slides[0].moving = fx.dir.join("missing.nii");
        let mut panel = CheckregReviewPanel::from_manifest(man);
        let ctx = egui::Context::default();
        wait_loaded(&ctx, &mut panel);
        assert!(!panel.has_volumes());
        assert!(panel.load_error().is_some());
        frame(&ctx, &mut panel, Vec::new());
        let d = frame(&ctx, &mut panel, key(egui::Key::D));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].action, ReviewAction::Reject);
    }

    #[test]
    fn load_error_falls_back_to_the_slide_png() {
        let fx = Fixture::new("fallback");
        let mut man = fx.manifest(1);
        man.slides[0].moving = fx.dir.join("missing.nii");
        let png = image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
        png.save(&man.slides[0].checkreg_png).expect("write png");
        let mut panel = CheckregReviewPanel::from_manifest(man);
        let ctx = egui::Context::default();
        wait_loaded(&ctx, &mut panel);
        assert!(panel.load_error().is_some());
        assert!(panel.fallback_tex.is_some());
    }

    #[test]
    fn manifest_path_round_trip_and_errors() {
        let fx = Fixture::new("manifest");
        let man = fx.manifest(2);
        let path = fx.dir.join("manifest.json");
        fs::write(&path, serde_json::to_string(&man).expect("json")).expect("write");
        let panel = CheckregReviewPanel::from_manifest_path(&path).expect("parse");
        assert_eq!(panel.slide_count(), 2);
        assert!(CheckregReviewPanel::from_manifest_path(&fx.dir.join("none.json")).is_err());
    }

    #[test]
    fn shortcut_rows_are_listed() {
        assert!(shortcut_rows().iter().any(|(k, _)| *k == "A"));
        assert!(shortcut_rows().len() >= 7);
    }

    fn view(cursor: (usize, usize, usize), raw_wl: bool, center: f32, width: f32) -> SliceView {
        SliceView {
            cursor,
            raw_wl,
            center,
            width,
            crosshair_on: true,
        }
    }

    #[test]
    fn sagittal_layers_key_ignores_the_axial_index() {
        let a = view((4, 5, 6), true, 40.0, 80.0);
        let b = view((4, 5, 9), true, 40.0, 80.0);
        let sag = CheckregSlicePlane::Sagittal;
        assert_eq!(a.layers_key(sag), b.layers_key(sag));
        assert_eq!(a.image_key(sag), b.image_key(sag));
        assert_ne!(a.tex_key(sag), b.tex_key(sag));
        let axial = CheckregSlicePlane::Axial;
        assert_ne!(a.layers_key(axial), b.layers_key(axial));
        let c = view((7, 5, 6), true, 40.0, 80.0);
        assert_ne!(a.layers_key(sag), c.layers_key(sag));
        assert_eq!(
            a.layers_key(CheckregSlicePlane::Coronal),
            c.layers_key(CheckregSlicePlane::Coronal)
        );
    }

    #[test]
    fn window_enters_no_key_without_raw_window() {
        let a = view((4, 5, 6), false, 40.0, 80.0);
        let b = view((4, 5, 6), false, 300.0, 1500.0);
        for plane in ORTHO_PLANES {
            assert_eq!(a.layers_key(plane), b.layers_key(plane));
            assert_eq!(a.image_key(plane), b.image_key(plane));
            assert_eq!(a.tex_key(plane), b.tex_key(plane));
            assert_eq!(a.image_key(plane).window, None);
        }
        let ra = view((4, 5, 6), true, 40.0, 80.0);
        let rb = view((4, 5, 6), true, 300.0, 1500.0);
        for plane in ORTHO_PLANES {
            assert_eq!(ra.layers_key(plane), rb.layers_key(plane));
            assert_ne!(ra.image_key(plane), rb.image_key(plane));
            assert_ne!(ra.layers_key(plane), a.layers_key(plane));
        }
    }

    #[test]
    fn crosshair_move_changes_only_the_texture_key() {
        let axial = CheckregSlicePlane::Axial;
        let a = view((4, 5, 6), true, 40.0, 80.0);
        let b = view((8, 2, 6), true, 40.0, 80.0);
        assert_eq!(a.image_key(axial), b.image_key(axial));
        assert_ne!(a.tex_key(axial), b.tex_key(axial));
        let off_a = SliceView {
            crosshair_on: false,
            ..a
        };
        let off_b = SliceView {
            crosshair_on: false,
            ..b
        };
        assert_eq!(off_a.tex_key(axial), off_b.tex_key(axial));
        assert_ne!(off_a.tex_key(axial), a.tex_key(axial));
    }

    fn assert_slots_match_direct_render(panel: &CheckregReviewPanel) {
        let vols = panel.vols.as_ref().expect("volumes");
        let cursor = (panel.ci, panel.cj, panel.ck);
        for (slot, plane) in panel.ortho.iter().zip(ORTHO_PLANES) {
            let (_, img) = slot.image.as_ref().expect("cached image");
            let want = vols
                .rgba_slice_display(
                    plane,
                    plane_index(plane, cursor),
                    panel.raw_wl,
                    panel.wl_center,
                    panel.wl_width,
                    None,
                )
                .expect("direct");
            assert_eq!((img.w, img.h, &img.rgba), (want.0, want.1, &want.2));
            assert_eq!(
                slot.tex_key,
                Some(panel.slice_view().tex_key(plane)),
                "{plane:?}"
            );
            assert!(slot.tex.is_some());
        }
    }

    #[test]
    fn plane_caches_follow_slice_window_and_crosshair_changes() {
        let fx = Fixture::new("caches");
        let mut panel = CheckregReviewPanel::from_manifest(fx.manifest(2));
        let ctx = egui::Context::default();
        wait_loaded(&ctx, &mut panel);
        frame(&ctx, &mut panel, Vec::new());
        assert_slots_match_direct_render(&panel);
        let sag_layers = panel.ortho[1].layers.as_ref().map(|(k, _)| *k);
        let cor_layers = panel.ortho[2].layers.as_ref().map(|(k, _)| *k);

        panel.ck = panel.ck.saturating_sub(1);
        frame(&ctx, &mut panel, Vec::new());
        assert_slots_match_direct_render(&panel);
        assert_eq!(panel.ortho[1].layers.as_ref().map(|(k, _)| *k), sag_layers);
        assert_eq!(panel.ortho[2].layers.as_ref().map(|(k, _)| *k), cor_layers);

        panel.wl_center += 0.01;
        panel.wl_width *= 1.5;
        frame(&ctx, &mut panel, Vec::new());
        assert_slots_match_direct_render(&panel);
        assert_eq!(panel.ortho[1].layers.as_ref().map(|(k, _)| *k), sag_layers);

        panel.raw_wl = false;
        frame(&ctx, &mut panel, Vec::new());
        assert_slots_match_direct_render(&panel);
        panel.wl_center += 5.0;
        frame(&ctx, &mut panel, Vec::new());
        assert_slots_match_direct_render(&panel);

        panel.crosshair_on = false;
        frame(&ctx, &mut panel, Vec::new());
        assert_slots_match_direct_render(&panel);

        panel.layout = LayoutMode::Single;
        panel.plane = CheckregSlicePlane::Coronal;
        frame(&ctx, &mut panel, Vec::new());
        assert_eq!(
            panel.single.tex_key,
            Some(panel.slice_view().tex_key(CheckregSlicePlane::Coronal))
        );

        panel.start_load(&ctx);
        assert!(panel.single.tex_key.is_none() && panel.single.layers.is_none());
        assert!(panel
            .ortho
            .iter()
            .all(|s| s.tex_key.is_none() && s.layers.is_none() && s.image.is_none()));
    }
}
