// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use dbsloc_reconstruct::native_frame::{resolve_fiducials, SideGeometry};
use dbsloc_reconstruct::{electrode_by_name, ElectrodeGeometry};
use serde::{Deserialize, Serialize};

use crate::error::{VizError, VizResult};
use crate::gesture_zoom::{
    adjust_zoom, adjust_zoom_pinch, adjust_zoom_right_drag, wheel_delta_points,
    SliceScrollAccumulator, ZOOM_MAX, ZOOM_MIN,
};
use crate::refine_lead::{
    add, azimuth_of, azimuth_y, caption, catalogue_spacing_mm, closeup_frame, cross, lead_frame,
    live_spacing_mm, norm, pivot_keep_length, project_unit, roll_of, roll_y, sample_frame, scale,
    sub, unit, window_rgba, wrap_deg, LeadView, PlaneFrame, SAMPLE_STEP_MM, SLAB_OFFSETS_MM, V3,
};
use crate::refine_volume::{RefinePlane, RefineVolume, SliceScratch};
use crate::ui_tokens::{self, Palette};
use crate::widgets;

const STEP_NORMAL_MM: f64 = 0.25;
const STEP_LARGE_MM: f64 = 1.0;
const STEP_FINE_MM: f64 = 0.05;
const ROLL_STEP_DEG: f64 = 1.0;
const ROLL_STEP_LARGE_DEG: f64 = 10.0;
const DEFAULT_EM: f32 = 14.0;
const HANDLE_HIT_EM: f32 = 0.85;
const AXIS_HIT_EM: f32 = 0.45;
const HANDLE_MIN_RADIUS_EM: f32 = 0.6;
const HANDLE_RADIUS_FACTOR: f32 = 2.2;
const CONTACT_MIN_RADIUS_EM: f32 = 0.15;
const STROKE_THIN_EM: f32 = 0.07;
const STROKE_MEDIUM_EM: f32 = 0.11;
const STROKE_THICK_EM: f32 = 0.15;
const ARROW_MIN_EM: f32 = 0.15;
const PANE_GAP_EM: f32 = 0.3;
const CLOSEUP_WIDTH_SHARE: f32 = 0.34;
const MODEL_DIAGRAM_MIN_EM: f32 = 8.0;
const MODEL_DIAGRAM_MAX_EM: f32 = 16.0;
const MODEL_DIAGRAM_SHAFT_MM: f64 = 3.0;
const TITLE_INSET_EM: f32 = 0.45;
const TITLE_PAD_EM: f32 = 0.3;
const TITLE_CORNER_EM: f32 = 0.2;
const FIT_EXTENT_MM: f64 = 50.0;
const PAN_DRAG_GAIN: f32 = 1.35;
const NEAR_SLICE_MM: f64 = 2.0;
const MIN_SPAN_MM: f64 = 1e-6;
const ARROW_MM: f64 = 5.0;
const UNDO_CAP: usize = 256;
const ROLL_EPS_DEG: f64 = 1e-9;
const MARKER_EPS_MM: f64 = 1e-9;

const SLICE_TEX_UV_FLIPPED_Y: egui::Rect =
    egui::Rect::from_min_max(egui::pos2(0.0, 1.0), egui::pos2(1.0, 0.0));

const SHORTCUT_ROWS: &[(&str, &str)] = &[
    ("H / T", "Select the head (contact 0) / tail marker"),
    ("< / >", "Previous / next lead side"),
    (
        "Arrow keys",
        "Nudge the selected marker in the hovered pane, 0.25 mm",
    ),
    ("Shift / Ctrl + arrows", "Nudge 1 mm / 0.05 mm"),
    (
        "Page Up / Page Down",
        "Move the selected marker through the pane plane",
    ),
    ("Alt + arrows", "Move the whole lead in the hovered pane"),
    (
        "Alt + Page Up / Page Down",
        "Move the whole lead along its axis, up / down (same steps)",
    ),
    ("K", "Keep lead length on / off"),
    ("[ / ]", "Roll the lead -1 / +1 degree (Shift: 10 degrees)"),
    ("Space", "Accept this side and go to the next"),
    ("Ctrl + Z / Ctrl + Y", "Undo / redo for this side"),
    ("Ctrl + Enter", "Save corrections"),
    ("Esc", "Discard and close"),
    ("F", "Centre the crosshair on the selected marker"),
    ("L", "Lead planes and head / tail close-ups"),
    ("1 / 2 / 3", "Single pane: axial / coronal / sagittal"),
    ("0", "Show all three planes"),
    ("X", "X-ray (slab) view of the lead planes on / off"),
    ("V", "Hide / show the markers, contacts and lead axis"),
    ("R", "Reset zoom and pan"),
    ("Mouse wheel", "Step the hovered pane's slice"),
    (
        "Left drag",
        "Move a marker handle or the lead axis; elsewhere move the crosshair",
    ),
    ("Shift + left drag", "Window and level"),
    ("Right drag, pinch, Ctrl + wheel", "Zoom"),
    ("Middle drag, Alt + left drag", "Pan a zoomed pane"),
];

pub fn shortcut_rows() -> &'static [(&'static str, &'static str)] {
    SHORTCUT_ROWS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditSpace {
    Native,
    Mni,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefineSide {
    pub key: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub elmodel: Option<String>,
    pub head: [f64; 3],
    pub tail: [f64; 3],
    #[serde(default)]
    pub x: Option<[f64; 3]>,
    #[serde(default)]
    pub y: Option<[f64; 3]>,
    #[serde(default)]
    pub roll_deg: Option<f64>,
    #[serde(default)]
    pub candidates_deg: Option<Vec<f64>>,
    #[serde(default)]
    pub orientation_status: Option<String>,
    #[serde(default)]
    pub manually_corrected: bool,
    #[serde(default)]
    pub review: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefineRequest {
    pub subj_id: String,
    pub space: EditSpace,
    pub backdrop_nii: PathBuf,
    pub sides: Vec<RefineSide>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SideEdit {
    pub key: String,
    pub head: [f64; 3],
    pub tail: [f64; 3],
    pub roll_deg: Option<f64>,
    #[serde(default)]
    pub accept: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RefineOutput {
    pub save: Option<Vec<SideEdit>>,
    pub cancelled: bool,
    pub errors: Vec<String>,
}

fn effective_y(u: V3, roll: Option<f64>, file_y: Option<V3>) -> V3 {
    match roll {
        Some(r) => roll_y(u, r),
        None => file_y
            .and_then(|y| {
                azimuth_of(y)
                    .and_then(|az| azimuth_y(u, az))
                    .or_else(|| project_unit(y, u))
            })
            .unwrap_or_else(|| roll_y(u, 0.0)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    head: V3,
    tail: V3,
    roll: Option<f64>,
}

impl Pose {
    fn at(&self, m: Marker) -> V3 {
        match m {
            Marker::Head => self.head,
            Marker::Tail => self.tail,
        }
    }
}

fn pose_valid(p: &Pose) -> bool {
    let span = norm(sub(p.tail, p.head));
    p.head.iter().chain(p.tail.iter()).all(|v| v.is_finite())
        && p.roll.is_none_or(f64::is_finite)
        && span.is_finite()
        && span >= MIN_SPAN_MM
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    Head,
    Tail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneAxis {
    Horizontal,
    Vertical,
    Through,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepSize {
    Normal,
    Large,
    Fine,
}

impl StepSize {
    pub(crate) fn mm(self) -> f64 {
        match self {
            StepSize::Normal => STEP_NORMAL_MM,
            StepSize::Large => STEP_LARGE_MM,
            StepSize::Fine => STEP_FINE_MM,
        }
    }
}

struct SideState {
    key: String,
    label: String,
    electrode: &'static ElectrodeGeometry,
    file_head: V3,
    file_tail: V3,
    file_y: Option<V3>,
    file_roll: f64,
    pose: Pose,
    undo: Vec<Pose>,
    redo: Vec<Pose>,
    accepted: bool,
    candidates: Vec<f64>,
    orientation_status: Option<String>,
    manually_corrected: bool,
    review: Option<String>,
    geom: Option<SideGeometry>,
    geom_error: Option<String>,
    request_roll: Option<f64>,
    y_dir: V3,
}

impl SideState {
    fn new(side: &RefineSide) -> VizResult<Self> {
        let model = side
            .elmodel
            .as_deref()
            .ok_or_else(|| VizError::Json(format!("no electrode model for side {}", side.key)))?;
        let electrode = electrode_by_name(model)
            .ok_or_else(|| VizError::Json(format!("unknown electrode model {model}")))?;
        let u = unit(sub(side.tail, side.head))
            .filter(|_| norm(sub(side.tail, side.head)) >= MIN_SPAN_MM)
            .ok_or_else(|| VizError::Json("head and tail coincide".into()))?;
        if ![side.head, side.tail]
            .iter()
            .flatten()
            .all(|v| v.is_finite())
        {
            return Err(VizError::Json("non-finite marker coordinate".into()));
        }
        let file_y = side.y.and_then(|y| unit(sub(y, side.head)));
        let file_roll = roll_of(u, effective_y(u, None, file_y));
        let label = if side.label.is_empty() {
            format!("Side {}", side.key)
        } else {
            side.label.clone()
        };
        let mut state = Self {
            key: side.key.clone(),
            label,
            electrode,
            file_head: side.head,
            file_tail: side.tail,
            file_y,
            file_roll,
            pose: Pose {
                head: side.head,
                tail: side.tail,
                roll: None,
            },
            undo: Vec::new(),
            redo: Vec::new(),
            accepted: false,
            candidates: side
                .candidates_deg
                .iter()
                .flatten()
                .copied()
                .filter(|c| c.is_finite())
                .map(wrap_deg)
                .collect(),
            orientation_status: side.orientation_status.clone(),
            manually_corrected: side.manually_corrected,
            review: side.review.clone(),
            geom: None,
            geom_error: None,
            request_roll: side.roll_deg.filter(|r| r.is_finite()).map(wrap_deg),
            y_dir: [0.0, 1.0, 0.0],
        };
        state.recompute();
        Ok(state)
    }

    fn recompute(&mut self) {
        let Pose { head, tail, roll } = self.pose;
        let Some(u) = unit(sub(tail, head)) else {
            self.geom = None;
            self.geom_error = Some("head and tail coincide".to_owned());
            return;
        };
        let y = effective_y(u, roll, self.file_y);
        self.y_dir = y;
        let x = cross(y, u);
        let r = 0.5 * self.electrode.lead_diameter_mm;
        match resolve_fiducials(
            [head, tail, add(head, scale(x, r)), add(head, scale(y, r))],
            self.electrode,
        ) {
            Ok(g) => {
                self.geom = Some(g);
                self.geom_error = None;
            }
            Err(e) => {
                self.geom = None;
                self.geom_error = Some(e.to_string());
            }
        }
    }

    fn axis_unit(&self) -> Option<V3> {
        unit(sub(self.pose.tail, self.pose.head))
    }

    fn roll_deg(&self) -> f64 {
        match (self.pose.roll, self.axis_unit()) {
            (Some(r), _) => r,
            (None, Some(u)) => roll_of(u, self.y_dir),
            (None, None) => self.file_roll,
        }
    }

    fn pose_with_roll(&self, base: Pose, deg: f64) -> Pose {
        if !self.electrode.is_directed || !deg.is_finite() {
            return base;
        }
        let r = wrap_deg(deg);
        let Some(u) = unit(sub(base.tail, base.head)) else {
            return base;
        };
        let projected = roll_of(u, effective_y(u, None, self.file_y));
        let roll = if wrap_deg(r - projected).abs() < ROLL_EPS_DEG {
            None
        } else {
            Some(r)
        };
        Pose { roll, ..base }
    }

    fn markers_changed(&self) -> bool {
        let far = |a: V3, b: V3| (0..3).any(|i| (a[i] - b[i]).abs() > MARKER_EPS_MM);
        far(self.pose.head, self.file_head) || far(self.pose.tail, self.file_tail)
    }

    fn roll_changed(&self) -> bool {
        if !self.electrode.is_directed {
            return false;
        }
        let Some(r) = self.pose.roll else {
            return false;
        };
        if let Some(u) = self.axis_unit() {
            let kept = roll_of(u, effective_y(u, None, self.file_y));
            if wrap_deg(r - kept).abs() <= ROLL_EPS_DEG {
                return false;
            }
        }
        wrap_deg(r - self.file_roll).abs() > ROLL_EPS_DEG || self.markers_changed()
    }

    fn changed(&self) -> bool {
        self.markers_changed() || self.roll_changed()
    }

    fn accept_pending(&self) -> bool {
        self.accepted && !self.changed()
    }

    fn review_text(&self) -> Option<&'static str> {
        if self.manually_corrected || self.review.as_deref() == Some("corrected") {
            Some("corrected")
        } else if self.review.as_deref() == Some("accepted") {
            Some("accepted")
        } else {
            None
        }
    }

    fn tilt_from_vertical_deg(&self) -> f64 {
        self.axis_unit()
            .map(|u| u[2].abs().clamp(0.0, 1.0).acos().to_degrees())
            .unwrap_or(0.0)
    }

    fn set_pose(&mut self, pose: Pose) {
        self.pose = pose;
        self.recompute();
    }

    fn marker(&self, m: Marker) -> V3 {
        match m {
            Marker::Head => self.pose.head,
            Marker::Tail => self.pose.tail,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutMode {
    Lead,
    Triple,
    Single,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneKind {
    Ortho(RefinePlane),
    Lead(LeadView),
    Close(Marker),
}

impl From<RefinePlane> for PaneKind {
    fn from(p: RefinePlane) -> Self {
        PaneKind::Ortho(p)
    }
}

const OBLIQUE_PANES: [PaneKind; 4] = [
    PaneKind::Lead(LeadView::Front),
    PaneKind::Lead(LeadView::Left),
    PaneKind::Close(Marker::Head),
    PaneKind::Close(Marker::Tail),
];

#[derive(Clone, Copy)]
enum DragMode {
    Marker(Marker),
    Lead,
    Crosshair,
}

#[derive(Clone, Copy)]
enum DragPane {
    Ortho { plane: RefinePlane, through: f64 },
    Oblique { idx: usize, frame: PlaneFrame },
}

#[derive(Clone, Copy)]
struct DragState {
    mode: DragMode,
    pane: DragPane,
    start: Pose,
    grab: V3,
}

struct ObliquePane {
    sampled: Option<(PlaneFrame, bool)>,
    raw: Vec<f32>,
    size: [usize; 2],
    rgba: Vec<u8>,
    tex: Option<egui::TextureHandle>,
    tex_key: Option<(u64, u32, u32)>,
    generation: u64,
    zoom: f32,
    pan: egui::Vec2,
}

impl Default for ObliquePane {
    fn default() -> Self {
        Self {
            sampled: None,
            raw: Vec::new(),
            size: [0, 0],
            rgba: Vec::new(),
            tex: None,
            tex_key: None,
            generation: 0,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        }
    }
}

#[derive(Clone, Copy)]
struct ObliqueGeo {
    frame: PlaneFrame,
    img: egui::Rect,
    pts_per_mm: f32,
}

impl ObliqueGeo {
    fn new(rect: egui::Rect, frame: PlaneFrame, zoom: f32, pan: egui::Vec2) -> Self {
        let (ew, eh) = frame.extent();
        let fit = (rect.width() / ew.max(1e-6) as f32).min(rect.height() / eh.max(1e-6) as f32);
        let ppm = (fit * zoom).max(1e-3);
        let disp = egui::vec2(ew as f32 * ppm, eh as f32 * ppm);
        let top_left = rect.min + (rect.size() - disp) * 0.5 + pan;
        Self {
            frame,
            img: egui::Rect::from_min_size(top_left, disp),
            pts_per_mm: ppm,
        }
    }

    fn screen(&self, u: f64, v: f64) -> egui::Pos2 {
        egui::pos2(
            self.img.min.x + ((u - self.frame.u_range.0) as f32) * self.pts_per_mm,
            self.img.min.y + ((self.frame.v_range.1 - v) as f32) * self.pts_per_mm,
        )
    }

    fn uv_of(&self, p: egui::Pos2) -> (f64, f64) {
        (
            self.frame.u_range.0 + f64::from((p.x - self.img.min.x) / self.pts_per_mm),
            self.frame.v_range.1 - f64::from((p.y - self.img.min.y) / self.pts_per_mm),
        )
    }

    fn project(&self, w: V3) -> (egui::Pos2, f64) {
        let (u, v, d) = self.frame.project(w);
        (self.screen(u, v), d)
    }
}

const KEEP_LENGTH_ALONG: &str = "Keep lead length is on, so a move along the lead does nothing. Turn it off to stretch the lead, or move the whole lead along its axis with Alt + Page Up / Page Down.";
const KEEP_LENGTH_CLAMPED: &str =
    "Keep lead length: the move was longer than the lead, so the marker stopped at the lead length.";

#[derive(Clone, Copy)]
struct PaneGeo {
    img: egui::Rect,
    pts_per_mm: f32,
}

impl PaneGeo {
    fn new(
        rect: egui::Rect,
        tex: egui::Vec2,
        zoom: f32,
        pan: egui::Vec2,
        extent: (f64, f64),
    ) -> Self {
        let avail = rect.size();
        let fit = if avail.x < 1.0 || avail.y < 1.0 || tex.x < 1.0 || tex.y < 1.0 {
            0.0
        } else {
            (avail.x / tex.x).min(avail.y / tex.y)
        };
        let mut disp = tex * fit * zoom;
        disp.x = disp.x.max(1.0);
        disp.y = disp.y.max(1.0);
        let top_left = rect.min + (avail - disp) * 0.5 + pan;
        Self {
            img: egui::Rect::from_min_size(top_left, disp),
            pts_per_mm: disp.x / extent.0.max(1e-6) as f32,
        }
    }

    fn screen_pos(&self, u: f64, v: f64) -> egui::Pos2 {
        egui::pos2(
            self.img.min.x + u as f32 * self.img.width(),
            self.img.max.y - v as f32 * self.img.height(),
        )
    }

    fn uv_of(&self, p: egui::Pos2) -> (f64, f64) {
        (
            f64::from((p.x - self.img.min.x) / self.img.width().max(1e-6)),
            f64::from((self.img.max.y - p.y) / self.img.height().max(1e-6)),
        )
    }
}

fn fit_zoom_and_pan(
    rect: egui::Rect,
    tex: egui::Vec2,
    extent: (f64, f64),
    centre_uv: (f64, f64),
) -> (f32, egui::Vec2) {
    let base = PaneGeo::new(rect, tex, 1.0, egui::Vec2::ZERO, extent);
    let short = rect.width().min(rect.height());
    let zoom =
        (short / (FIT_EXTENT_MM as f32 * base.pts_per_mm.max(1e-6))).clamp(ZOOM_MIN, ZOOM_MAX);
    let geo = PaneGeo::new(rect, tex, zoom, egui::Vec2::ZERO, extent);
    let c = geo.screen_pos(centre_uv.0, centre_uv.1);
    (zoom, rect.center() - c)
}

fn segment_distance(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 1e-6 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

fn mods_are(m: &egui::Modifiers, alt: bool, shift: bool, ctrl: bool) -> bool {
    m.alt == alt && m.shift == shift && (m.ctrl || m.command) == ctrl
}

fn take_key(
    input: &mut egui::InputState,
    key: egui::Key,
    alt: bool,
    shift: bool,
    ctrl: bool,
) -> bool {
    let mut hit = false;
    input.events.retain(|event| {
        let is_match = matches!(
            event,
            egui::Event::Key { key: k, modifiers, pressed: true, .. }
                if *k == key && mods_are(modifiers, alt, shift, ctrl)
        );
        hit |= is_match;
        !is_match
    });
    hit
}

enum KeyCmd {
    Nudge {
        axis: PaneAxis,
        sign: i32,
        size: StepSize,
        lead: bool,
    },
    AlongAxis {
        sign: i32,
        size: StepSize,
    },
    ToggleKeepLength,
    ToggleSlab,
    ToggleFiducials,
    LeadLayout,
    Roll(f64),
    Select(Marker),
    Side(i32),
    Accept,
    Save,
    Discard,
    Undo,
    Redo,
    Recentre,
    Single(RefinePlane),
    Triple,
    ResetView,
}

pub struct RefinePanel {
    req: RefineRequest,
    vol: RefineVolume,
    sides: Vec<SideState>,
    cur: usize,
    sel: Marker,
    slice: [usize; 3],
    layout: LayoutMode,
    single: RefinePlane,
    wl_center: f32,
    wl_width: f32,
    default_wl: (f32, f32),
    zoom: [f32; 3],
    pan: [egui::Vec2; 3],
    tex: [Option<egui::TextureHandle>; 3],
    tex_key: [Option<(usize, u32, u32)>; 3],
    scratch: SliceScratch,
    em: f32,
    last_pane: PaneKind,
    confirm_discard: bool,
    saved_keys: Vec<String>,
    fit_pending: [bool; 3],
    hover_pane: Option<PaneKind>,
    drag: Option<DragState>,
    keep_length: bool,
    move_whole_numeric: bool,
    slab: bool,
    hide_fiducials: bool,
    oblique: [ObliquePane; 4],
    controls_overflow: bool,
    controls_viewport_h: f32,
    gesture_pushed: Option<bool>,
    scroll: SliceScrollAccumulator,
    status: Option<String>,
    out: RefineOutput,
}

impl RefinePanel {
    pub fn new(req: RefineRequest) -> VizResult<Self> {
        let vol = RefineVolume::load(&req.backdrop_nii)?;
        Self::with_volume(req, vol)
    }

    pub fn from_request_path(path: &Path) -> VizResult<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| VizError::Json(format!("{}: {e}", path.display())))?;
        let req: RefineRequest =
            serde_json::from_str(&text).map_err(|e| VizError::Json(e.to_string()))?;
        Self::new(req)
    }

    pub(crate) fn with_volume(req: RefineRequest, vol: RefineVolume) -> VizResult<Self> {
        if req.sides.is_empty() {
            return Err(VizError::Json("the request has no lead sides".into()));
        }
        let sides = req
            .sides
            .iter()
            .map(SideState::new)
            .collect::<VizResult<Vec<_>>>()?;
        let vol = vol.with_ct_hint(Some(req.space == EditSpace::Native));
        let default_wl = vol.default_window();
        let mut panel = Self {
            req,
            vol,
            sides,
            cur: 0,
            sel: Marker::Head,
            slice: [0; 3],
            layout: LayoutMode::Lead,
            single: RefinePlane::Axial,
            wl_center: default_wl.0,
            wl_width: default_wl.1,
            default_wl,
            zoom: [1.0; 3],
            pan: [egui::Vec2::ZERO; 3],
            tex: Default::default(),
            tex_key: [None; 3],
            scratch: SliceScratch::default(),
            em: DEFAULT_EM,
            last_pane: PaneKind::Ortho(RefinePlane::Axial),
            confirm_discard: false,
            saved_keys: Vec::new(),
            fit_pending: [false; 3],
            hover_pane: None,
            drag: None,
            keep_length: true,
            move_whole_numeric: false,
            slab: false,
            hide_fiducials: false,
            oblique: Default::default(),
            controls_overflow: false,
            controls_viewport_h: 0.0,
            gesture_pushed: None,
            scroll: SliceScrollAccumulator::default(),
            status: None,
            out: RefineOutput::default(),
        };
        panel.centre_on_marker();
        Ok(panel)
    }

    pub fn request(&self) -> &RefineRequest {
        &self.req
    }

    pub fn dirty(&self) -> bool {
        self.sides.iter().any(|s| s.changed() || s.accept_pending())
    }

    pub fn shortcut_rows() -> &'static [(&'static str, &'static str)] {
        SHORTCUT_ROWS
    }

    pub fn mark_saved(&mut self) {
        let keys = std::mem::take(&mut self.saved_keys);
        for side in self.sides.iter_mut().filter(|s| keys.contains(&s.key)) {
            let accept_only = side.accept_pending();
            side.accepted = false;
            if accept_only {
                if side.review.is_none() && !side.manually_corrected {
                    side.review = Some("accepted".to_owned());
                }
                continue;
            }
            side.review = Some("corrected".to_owned());
            side.file_head = side.pose.head;
            side.file_tail = side.pose.tail;
            side.file_y = Some(side.y_dir);
            side.file_roll = side.roll_deg();
            side.request_roll = None;
            side.pose.roll = None;
            side.manually_corrected = true;
            side.recompute();
        }
    }

    pub(crate) fn save_edits(&self) -> Vec<SideEdit> {
        self.sides
            .iter()
            .filter(|s| s.changed() || s.accept_pending())
            .map(|s| SideEdit {
                key: s.key.clone(),
                head: s.pose.head,
                tail: s.pose.tail,
                roll_deg: if s.roll_changed() {
                    Some(s.roll_deg())
                } else {
                    None
                },
                accept: !s.changed(),
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn current_markers(&self) -> (V3, V3) {
        let s = &self.sides[self.cur];
        (s.pose.head, s.pose.tail)
    }

    #[cfg(test)]
    pub(crate) fn current_roll_deg(&self) -> f64 {
        self.sides[self.cur].roll_deg()
    }

    pub(crate) fn select_marker(&mut self, m: Marker) {
        self.sel = m;
        self.recentre();
    }

    pub(crate) fn select_side(&mut self, idx: usize) {
        if idx < self.sides.len() && idx != self.cur {
            self.end_gesture();
            self.cur = idx;
            self.centre_on_marker();
        }
    }

    fn step_side(&mut self, delta: i32) {
        let n = self.sides.len() as i32;
        let next = (self.cur as i32 + delta).rem_euclid(n) as usize;
        self.select_side(next);
    }

    fn centre_on_marker(&mut self) {
        self.recentre();
        self.fit_pending = [true; 3];
    }

    fn recentre(&mut self) {
        let world = self.sides[self.cur].marker(self.sel);
        let ijk = self.vol.world_to_voxel_f(world);
        self.slice = self.vol.geometry().clamp_index(ijk);
    }

    fn begin_gesture(&mut self) {
        if self.gesture_pushed.is_none() {
            self.gesture_pushed = Some(false);
        }
    }

    fn end_gesture(&mut self) {
        self.gesture_pushed = None;
    }

    fn commit(&mut self, new: Pose) -> bool {
        if self.drag.is_none() {
            self.end_gesture();
        }
        self.commit_inner(new)
    }

    fn commit_gesture(&mut self, new: Pose) -> bool {
        self.begin_gesture();
        self.commit_inner(new)
    }

    fn commit_inner(&mut self, new: Pose) -> bool {
        let gesture_pushed = self.gesture_pushed.unwrap_or(false);
        let side = &mut self.sides[self.cur];
        if new == side.pose {
            return false;
        }
        if !pose_valid(&new) {
            self.status =
                Some("Move refused: head and tail must stay finite and apart.".to_owned());
            return false;
        }
        let before = side.pose;
        side.set_pose(new);
        if !gesture_pushed {
            side.undo.push(before);
            if side.undo.len() > UNDO_CAP {
                side.undo.remove(0);
            }
            side.redo.clear();
        }
        side.accepted = false;
        if self.gesture_pushed.is_some() {
            self.gesture_pushed = Some(true);
        }
        self.status = None;
        true
    }

    fn pose_with_marker(&self, marker: Marker, pos: V3) -> Pose {
        let mut pose = self.sides[self.cur].pose;
        match marker {
            Marker::Head => pose.head = pos,
            Marker::Tail => pose.tail = pos,
        }
        pose
    }

    #[cfg(test)]
    pub(crate) fn move_marker_to(&mut self, marker: Marker, pos: V3) -> bool {
        let pose = self.pose_with_marker(marker, pos);
        let changed = self.commit(pose);
        if changed {
            self.recentre();
        }
        changed
    }

    /// *Move whole lead* (`specs/12` §11.4 `m` dialog): `marker` goes to `pos`, the other
    /// marker follows by the same vector.
    pub(crate) fn translate_lead_to(&mut self, marker: Marker, pos: V3, gesture: bool) -> bool {
        let base = self.sides[self.cur].pose;
        let delta = sub(pos, base.at(marker));
        let pose = match marker {
            Marker::Head => Pose {
                head: pos,
                tail: add(base.tail, delta),
                ..base
            },
            Marker::Tail => Pose {
                head: add(base.head, delta),
                tail: pos,
                ..base
            },
        };
        self.commit_noted(pose, None, gesture)
    }

    fn move_marker_gesture(&mut self, marker: Marker, pos: V3) -> bool {
        let pose = self.pose_with_marker(marker, pos);
        let changed = self.commit_gesture(pose);
        if changed {
            self.recentre();
        }
        changed
    }

    fn lead_frame_for(&self, view: LeadView, pose: &Pose) -> Option<PlaneFrame> {
        lead_frame(view, pose.head, pose.tail, self.sides[self.cur].electrode)
    }

    fn pane_dirs(&self, kind: PaneKind) -> Option<(V3, V3, V3)> {
        let g = self.vol.geometry();
        match kind {
            PaneKind::Ortho(plane) => {
                let (ua, va, ta) = plane.axes();
                Some((
                    g.axis_direction(ua),
                    g.axis_direction(va),
                    g.axis_direction(ta),
                ))
            }
            PaneKind::Close(_) => self.pane_dirs(PaneKind::Ortho(RefinePlane::Axial)),
            PaneKind::Lead(view) => {
                let f = self.lead_frame_for(view, &self.sides[self.cur].pose)?;
                Some((f.right, f.up, f.normal))
            }
        }
    }

    /// The pose after asking `marker` to go to `requested`, with *Keep lead length* applied
    /// against `base` (`specs/12` §11.2 pivot rule, along the lead axis of `base`).
    fn marker_move(
        &self,
        base: Pose,
        marker: Marker,
        requested: V3,
    ) -> (Pose, Option<&'static str>) {
        let mut pose = base;
        let (moved, other) = match marker {
            Marker::Head => (base.head, base.tail),
            Marker::Tail => (base.tail, base.head),
        };
        let mut note = None;
        let target = if self.keep_length {
            match pivot_keep_length(moved, other, requested) {
                Some((p, clamped)) => {
                    if clamped {
                        note = Some(KEEP_LENGTH_CLAMPED);
                    } else if norm(sub(p, moved)) < MARKER_EPS_MM
                        && norm(sub(requested, moved)) > MARKER_EPS_MM
                    {
                        note = Some(KEEP_LENGTH_ALONG);
                    }
                    p
                }
                None => requested,
            }
        } else {
            requested
        };
        match marker {
            Marker::Head => pose.head = target,
            Marker::Tail => pose.tail = target,
        }
        (pose, note)
    }

    fn commit_noted(&mut self, pose: Pose, note: Option<&'static str>, gesture: bool) -> bool {
        let changed = if note == Some(KEEP_LENGTH_ALONG) {
            false
        } else if gesture {
            self.commit_gesture(pose)
        } else {
            self.commit(pose)
        };
        if changed {
            self.recentre();
        }
        if let Some(n) = note {
            self.status = Some(n.to_owned());
        }
        changed
    }

    pub(crate) fn nudge(
        &mut self,
        pane: impl Into<PaneKind>,
        axis: PaneAxis,
        steps: i32,
        size: StepSize,
        whole_lead: bool,
    ) -> bool {
        let Some((right, up, through)) = self.pane_dirs(pane.into()) else {
            return false;
        };
        let dir = match axis {
            PaneAxis::Horizontal => right,
            PaneAxis::Vertical => up,
            PaneAxis::Through => through,
        };
        let delta = scale(dir, size.mm() * f64::from(steps));
        let base = self.sides[self.cur].pose;
        if whole_lead {
            let pose = Pose {
                head: add(base.head, delta),
                tail: add(base.tail, delta),
                ..base
            };
            return self.commit_noted(pose, None, false);
        }
        let requested = add(self.sides[self.cur].marker(self.sel), delta);
        let (pose, note) = self.marker_move(base, self.sel, requested);
        self.commit_noted(pose, note, false)
    }

    /// Head and tail (and with them `x` / `y`) move together along the lead; `+` = proximal.
    pub(crate) fn move_along_axis(&mut self, steps: i32, size: StepSize) -> bool {
        let base = self.sides[self.cur].pose;
        let Some(u) = unit(sub(base.tail, base.head)) else {
            return false;
        };
        let delta = scale(u, size.mm() * f64::from(steps));
        let pose = Pose {
            head: add(base.head, delta),
            tail: add(base.tail, delta),
            ..base
        };
        self.commit_noted(pose, None, false)
    }

    pub(crate) fn set_roll(&mut self, deg: f64) -> bool {
        let side = &self.sides[self.cur];
        let pose = side.pose_with_roll(side.pose, deg);
        self.commit(pose)
    }

    fn set_roll_gesture(&mut self, deg: f64) -> bool {
        let side = &self.sides[self.cur];
        let pose = side.pose_with_roll(side.pose, deg);
        self.commit_gesture(pose)
    }

    pub(crate) fn roll_by(&mut self, delta_deg: f64) -> bool {
        let target = self.sides[self.cur].roll_deg() + delta_deg;
        self.set_roll(target)
    }

    pub(crate) fn flip(&mut self) -> bool {
        self.roll_by(180.0)
    }

    pub(crate) fn other_candidate(&mut self) -> bool {
        let side = &self.sides[self.cur];
        if side.candidates.len() < 2 {
            return false;
        }
        let cur = side.roll_deg();
        let far = side
            .candidates
            .iter()
            .copied()
            .max_by(|a, b| wrap_deg(a - cur).abs().total_cmp(&wrap_deg(b - cur).abs()));
        match far {
            Some(c) => self.set_roll(c),
            None => false,
        }
    }

    pub(crate) fn automatic_roll(&mut self) -> bool {
        let side = &self.sides[self.cur];
        let target = side.request_roll.unwrap_or(side.file_roll);
        let pose = side.pose_with_roll(side.pose, target);
        self.commit(pose)
    }

    pub(crate) fn reset_side(&mut self) -> bool {
        let side = &self.sides[self.cur];
        let pose = Pose {
            head: side.file_head,
            tail: side.file_tail,
            roll: None,
        };
        let changed = self.commit(pose);
        if changed {
            self.recentre();
        }
        changed
    }

    pub(crate) fn undo(&mut self) -> bool {
        self.end_gesture();
        let side = &mut self.sides[self.cur];
        let Some(prev) = side.undo.pop() else {
            return false;
        };
        side.redo.push(side.pose);
        side.set_pose(prev);
        self.recentre();
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        self.end_gesture();
        let side = &mut self.sides[self.cur];
        let Some(next) = side.redo.pop() else {
            return false;
        };
        side.undo.push(side.pose);
        side.set_pose(next);
        self.recentre();
        true
    }

    pub(crate) fn accept_side(&mut self) {
        self.end_gesture();
        self.sides[self.cur].accepted = true;
        if self.sides.len() > 1 {
            self.step_side(1);
        }
    }

    pub fn take_save(&mut self) -> Result<Option<Vec<SideEdit>>, Vec<String>> {
        let edits = self.save_edits();
        if edits.is_empty() {
            return Ok(None);
        }
        let errors: Vec<String> = self
            .sides
            .iter()
            .filter(|s| s.changed() && s.geom.is_none())
            .map(|s| {
                format!(
                    "{}: the lead geometry could not be resolved: {}",
                    s.label,
                    s.geom_error.as_deref().unwrap_or("unknown reason")
                )
            })
            .collect();
        if !errors.is_empty() {
            return Err(errors);
        }
        self.saved_keys = edits.iter().map(|e| e.key.clone()).collect();
        Ok(Some(edits))
    }

    pub(crate) fn request_save(&mut self) {
        match self.take_save() {
            Ok(None) => self.status = Some("No changes to save.".to_owned()),
            Ok(Some(edits)) => self.out.save = Some(edits),
            Err(errors) => self.out.errors.extend(errors),
        }
    }

    pub(crate) fn request_discard(&mut self) {
        if self.dirty() {
            self.confirm_discard = true;
        } else {
            self.out.cancelled = true;
        }
    }

    pub(crate) fn resolve_discard(&mut self, discard: bool) {
        self.confirm_discard = false;
        if discard {
            self.out.cancelled = true;
        }
    }

    fn active_pane(&self) -> PaneKind {
        match (self.layout, self.last_pane) {
            (LayoutMode::Single, _) => PaneKind::Ortho(self.single),
            (LayoutMode::Triple, PaneKind::Ortho(p)) => PaneKind::Ortho(p),
            (LayoutMode::Triple, _) => PaneKind::Ortho(RefinePlane::Axial),
            (LayoutMode::Lead, PaneKind::Ortho(_)) => PaneKind::Close(self.sel),
            (LayoutMode::Lead, kind) => kind,
        }
    }

    fn reset_view(&mut self) {
        self.fit_pending = [false; 3];
        self.zoom = [1.0; 3];
        self.pan = [egui::Vec2::ZERO; 3];
        for pane in &mut self.oblique {
            pane.zoom = 1.0;
            pane.pan = egui::Vec2::ZERO;
        }
    }

    fn set_crosshair_from_uv(&mut self, plane: RefinePlane, u: f64, v: f64) {
        let (ua, va, _) = plane.axes();
        let dims = self.vol.dims();
        let pick =
            |t: f64, n: usize| (t * n as f64 - 0.5).round().clamp(0.0, (n - 1) as f64) as usize;
        self.slice[ua] = pick(u, dims[ua]);
        self.slice[va] = pick(v, dims[va]);
    }

    pub(crate) fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
        }
        let widget_focused = ctx.memory(|m| m.focused().is_some());
        let mut cmds: Vec<KeyCmd> = Vec::new();
        ctx.input_mut(|i| {
            if take_key(i, egui::Key::Enter, false, false, true) {
                cmds.push(KeyCmd::Save);
            }
            if take_key(i, egui::Key::Z, false, false, true) {
                cmds.push(KeyCmd::Undo);
            }
            if take_key(i, egui::Key::Y, false, false, true)
                || take_key(i, egui::Key::Z, false, true, true)
            {
                cmds.push(KeyCmd::Redo);
            }
            let moves = [
                (egui::Key::ArrowRight, PaneAxis::Horizontal, 1),
                (egui::Key::ArrowLeft, PaneAxis::Horizontal, -1),
                (egui::Key::ArrowUp, PaneAxis::Vertical, 1),
                (egui::Key::ArrowDown, PaneAxis::Vertical, -1),
                (egui::Key::PageUp, PaneAxis::Through, 1),
                (egui::Key::PageDown, PaneAxis::Through, -1),
            ];
            for (key, axis, sign) in moves {
                for lead in [false, true] {
                    for (shift, ctrl, size) in [
                        (false, false, StepSize::Normal),
                        (true, false, StepSize::Large),
                        (false, true, StepSize::Fine),
                    ] {
                        if take_key(i, key, lead, shift, ctrl) {
                            cmds.push(if lead && axis == PaneAxis::Through {
                                KeyCmd::AlongAxis { sign, size }
                            } else {
                                KeyCmd::Nudge {
                                    axis,
                                    sign,
                                    size,
                                    lead,
                                }
                            });
                        }
                    }
                }
            }
            for (key, small, large) in [
                (egui::Key::OpenBracket, -ROLL_STEP_DEG, -ROLL_STEP_LARGE_DEG),
                (egui::Key::CloseBracket, ROLL_STEP_DEG, ROLL_STEP_LARGE_DEG),
            ] {
                if take_key(i, key, false, false, false) {
                    cmds.push(KeyCmd::Roll(small));
                }
                if take_key(i, key, false, true, false) {
                    cmds.push(KeyCmd::Roll(large));
                }
            }
            if take_key(i, egui::Key::OpenCurlyBracket, false, true, false)
                || take_key(i, egui::Key::OpenCurlyBracket, false, false, false)
            {
                cmds.push(KeyCmd::Roll(-ROLL_STEP_LARGE_DEG));
            }
            if take_key(i, egui::Key::CloseCurlyBracket, false, true, false)
                || take_key(i, egui::Key::CloseCurlyBracket, false, false, false)
            {
                cmds.push(KeyCmd::Roll(ROLL_STEP_LARGE_DEG));
            }
            if take_key(i, egui::Key::H, false, false, false) {
                cmds.push(KeyCmd::Select(Marker::Head));
            }
            if take_key(i, egui::Key::T, false, false, false) {
                cmds.push(KeyCmd::Select(Marker::Tail));
            }
            if take_key(i, egui::Key::Comma, false, true, false)
                || take_key(i, egui::Key::Comma, false, false, false)
            {
                cmds.push(KeyCmd::Side(-1));
            }
            if take_key(i, egui::Key::Period, false, true, false)
                || take_key(i, egui::Key::Period, false, false, false)
            {
                cmds.push(KeyCmd::Side(1));
            }
            if take_key(i, egui::Key::Space, false, false, false) {
                cmds.push(KeyCmd::Accept);
            }
            if !widget_focused && take_key(i, egui::Key::Escape, false, false, false) {
                cmds.push(KeyCmd::Discard);
            }
            if take_key(i, egui::Key::F, false, false, false) {
                cmds.push(KeyCmd::Recentre);
            }
            if take_key(i, egui::Key::Num1, false, false, false) {
                cmds.push(KeyCmd::Single(RefinePlane::Axial));
            }
            if take_key(i, egui::Key::Num2, false, false, false) {
                cmds.push(KeyCmd::Single(RefinePlane::Coronal));
            }
            if take_key(i, egui::Key::Num3, false, false, false) {
                cmds.push(KeyCmd::Single(RefinePlane::Sagittal));
            }
            if take_key(i, egui::Key::Num0, false, false, false) {
                cmds.push(KeyCmd::Triple);
            }
            if take_key(i, egui::Key::R, false, false, false) {
                cmds.push(KeyCmd::ResetView);
            }
            if take_key(i, egui::Key::K, false, false, false) {
                cmds.push(KeyCmd::ToggleKeepLength);
            }
            if take_key(i, egui::Key::X, false, false, false) {
                cmds.push(KeyCmd::ToggleSlab);
            }
            if take_key(i, egui::Key::V, false, false, false) {
                cmds.push(KeyCmd::ToggleFiducials);
            }
            if take_key(i, egui::Key::L, false, false, false) {
                cmds.push(KeyCmd::LeadLayout);
            }
        });
        for cmd in cmds {
            self.apply_key(cmd);
        }
    }

    fn apply_key(&mut self, cmd: KeyCmd) {
        match cmd {
            KeyCmd::Nudge {
                axis,
                sign,
                size,
                lead,
            } => {
                let pane = self.active_pane();
                self.nudge(pane, axis, sign, size, lead);
            }
            KeyCmd::AlongAxis { sign, size } => {
                self.move_along_axis(sign, size);
            }
            KeyCmd::ToggleKeepLength => self.keep_length = !self.keep_length,
            KeyCmd::ToggleSlab => self.slab = !self.slab,
            KeyCmd::ToggleFiducials => self.hide_fiducials = !self.hide_fiducials,
            KeyCmd::LeadLayout => self.layout = LayoutMode::Lead,
            KeyCmd::Roll(d) => {
                self.roll_by(d);
            }
            KeyCmd::Select(m) => self.select_marker(m),
            KeyCmd::Side(d) => self.step_side(d),
            KeyCmd::Accept => self.accept_side(),
            KeyCmd::Save => self.request_save(),
            KeyCmd::Discard => {
                if self.confirm_discard {
                    self.confirm_discard = false;
                } else {
                    self.request_discard();
                }
            }
            KeyCmd::Undo => {
                self.undo();
            }
            KeyCmd::Redo => {
                self.redo();
            }
            KeyCmd::Recentre => self.centre_on_marker(),
            KeyCmd::Single(p) => {
                self.layout = LayoutMode::Single;
                self.single = p;
            }
            KeyCmd::Triple => self.layout = LayoutMode::Triple,
            KeyCmd::ResetView => self.reset_view(),
        }
    }

    fn ensure_texture(&mut self, ctx: &egui::Context, plane: RefinePlane) {
        let pi = plane.index();
        let (_, _, ta) = plane.axes();
        let key = (
            self.slice[ta],
            self.wl_center.to_bits(),
            self.wl_width.to_bits(),
        );
        if self.tex_key[pi] == Some(key) && self.tex[pi].is_some() {
            return;
        }
        let (w, h) = self.vol.slice_rgba_into(
            plane,
            self.slice[ta],
            self.wl_center,
            self.wl_width,
            &mut self.scratch,
        );
        let image =
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], self.scratch.rgba());
        match &mut self.tex[pi] {
            Some(handle) => handle.set(image, Default::default()),
            slot => {
                *slot =
                    Some(ctx.load_texture(format!("refine_pane_{pi}"), image, Default::default()))
            }
        }
        self.tex_key[pi] = Some(key);
    }

    fn marker_screen(&self, geo: PaneGeo, plane: RefinePlane, world: V3) -> egui::Pos2 {
        let (_, _, ta) = plane.axes();
        let (u, v, _) = self
            .vol
            .geometry()
            .plane_uv_for_world(plane, world, self.slice[ta]);
        geo.screen_pos(u, v)
    }

    fn wl_drag(&mut self, resp: &egui::Response, modifiers: egui::Modifiers) {
        if self.drag.is_none()
            && modifiers.shift
            && !modifiers.alt
            && resp.dragged_by(egui::PointerButton::Primary)
        {
            let d = resp.drag_delta();
            self.wl_center += d.y * self.wl_width * 0.002;
            self.wl_width = (self.wl_width + d.x * self.wl_width * 0.004).max(1e-3);
        }
    }

    fn set_crosshair_world(&mut self, world: V3) {
        let ijk = self.vol.world_to_voxel_f(world);
        self.slice = self.vol.geometry().clamp_index(ijk);
    }

    fn pane_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        rect: egui::Rect,
        plane: RefinePlane,
        geo: PaneGeo,
    ) {
        let pi = plane.index();
        let (modifiers, press_origin) = ctx.input(|i| (i.modifiers, i.pointer.press_origin()));
        let plain = !modifiers.shift && !modifiers.alt;
        if resp.hovered() {
            self.hover_pane = Some(PaneKind::Ortho(plane));
            self.last_pane = PaneKind::Ortho(plane);
        }
        self.wl_drag(resp, modifiers);

        let pan_allowed = self.zoom[pi] > 1.01;
        if pan_allowed {
            let mid = resp.dragged_by(egui::PointerButton::Middle);
            let alt_primary = resp.dragged_by(egui::PointerButton::Primary) && modifiers.alt;
            if mid || alt_primary {
                self.pan[pi] += resp.drag_delta() * PAN_DRAG_GAIN;
            }
        }
        if resp.dragged_by(egui::PointerButton::Secondary) {
            adjust_zoom_right_drag(&mut self.zoom[pi], resp.drag_delta().y);
        }

        if resp.drag_started_by(egui::PointerButton::Primary) && plain {
            if let Some(p0) = press_origin {
                self.begin_pane_drag(plane, geo, p0);
            }
        }
        if resp.dragged_by(egui::PointerButton::Primary) {
            if let (Some(state), Some(p)) = (self.drag, resp.interact_pointer_pos()) {
                if let DragPane::Ortho { plane: dp, through } = state.pane {
                    if dp == plane {
                        self.continue_pane_drag(state, plane, through, geo, p);
                    }
                }
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
            self.end_gesture();
        }

        if resp.clicked_by(egui::PointerButton::Primary) && plain {
            if let Some(p) = resp.interact_pointer_pos() {
                self.click_in_pane(plane, geo, p);
            }
        }

        if resp.hovered() {
            let (sc, pinch) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let pinch_active = (pinch - 1.0).abs() > 1e-4;
            if pinch_active || (modifiers.ctrl && sc != 0.0) {
                if pinch_active {
                    adjust_zoom_pinch(&mut self.zoom[pi], pinch);
                } else {
                    adjust_zoom(&mut self.zoom[pi], sc);
                }
            } else {
                let wheel: Vec<(egui::MouseWheelUnit, egui::Vec2, egui::Modifiers)> =
                    ctx.input(|i| {
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
                    if modifiers.alt && pan_allowed {
                        pan += wheel_delta_points(unit, delta, rect.height());
                    } else {
                        steps += i64::from(self.scroll.feed(unit, delta));
                    }
                }
                if pan != egui::Vec2::ZERO {
                    self.pan[pi] += pan;
                }
                if steps != 0 {
                    let (_, _, ta) = plane.axes();
                    let top = (self.vol.dims()[ta] - 1) as i64;
                    self.slice[ta] = (self.slice[ta] as i64 + steps).clamp(0, top) as usize;
                }
            }
        }
        self.zoom[pi] = self.zoom[pi].clamp(ZOOM_MIN, ZOOM_MAX);
    }

    fn hit_mode(
        &mut self,
        p0: egui::Pos2,
        head_s: egui::Pos2,
        tail_s: egui::Pos2,
        only: Option<Marker>,
    ) -> DragMode {
        if self.hide_fiducials {
            return DragMode::Crosshair;
        }
        let hit = HANDLE_HIT_EM * self.em;
        let at = |m: Marker| match m {
            Marker::Head => head_s,
            Marker::Tail => tail_s,
        };
        if let Some(m) = only {
            if p0.distance(at(m)) <= hit {
                self.sel = m;
                return DragMode::Marker(m);
            }
            return DragMode::Crosshair;
        }
        let other = match self.sel {
            Marker::Head => Marker::Tail,
            Marker::Tail => Marker::Head,
        };
        if p0.distance(at(self.sel)) <= hit {
            DragMode::Marker(self.sel)
        } else if p0.distance(at(other)) <= hit {
            self.sel = other;
            DragMode::Marker(other)
        } else if segment_distance(p0, head_s, tail_s) <= AXIS_HIT_EM * self.em {
            DragMode::Lead
        } else {
            DragMode::Crosshair
        }
    }

    fn begin_pane_drag(&mut self, plane: RefinePlane, geo: PaneGeo, p0: egui::Pos2) {
        let pose = self.sides[self.cur].pose;
        let head_s = self.marker_screen(geo, plane, pose.head);
        let tail_s = self.marker_screen(geo, plane, pose.tail);
        let mode = self.hit_mode(p0, head_s, tail_s, None);
        let anchor = match mode {
            DragMode::Marker(m) => pose.at(m),
            DragMode::Lead | DragMode::Crosshair => pose.head,
        };
        let through = self.vol.geometry().through_index(plane, anchor);
        let (u0, v0) = geo.uv_of(p0);
        let grab = self
            .vol
            .geometry()
            .world_for_plane_uv(plane, u0, v0, through);
        self.drag = Some(DragState {
            mode,
            pane: DragPane::Ortho { plane, through },
            start: pose,
            grab,
        });
        if !matches!(mode, DragMode::Crosshair) {
            self.begin_gesture();
        }
    }

    fn drag_to(&mut self, state: DragState, cur: V3) {
        let delta = sub(cur, state.grab);
        match state.mode {
            DragMode::Marker(m) => {
                let requested = add(state.start.at(m), delta);
                let (pose, note) = self.marker_move(state.start, m, requested);
                self.commit_noted(pose, note, true);
            }
            DragMode::Lead => {
                let pose = Pose {
                    head: add(state.start.head, delta),
                    tail: add(state.start.tail, delta),
                    ..state.start
                };
                self.commit_noted(pose, None, true);
            }
            DragMode::Crosshair => {}
        }
    }

    fn continue_pane_drag(
        &mut self,
        state: DragState,
        plane: RefinePlane,
        through: f64,
        geo: PaneGeo,
        p: egui::Pos2,
    ) {
        let (u, v) = geo.uv_of(p);
        if matches!(state.mode, DragMode::Crosshair) {
            self.set_crosshair_from_uv(plane, u, v);
            return;
        }
        let cur = self.vol.geometry().world_for_plane_uv(plane, u, v, through);
        self.drag_to(state, cur);
    }

    fn click_in_pane(&mut self, plane: RefinePlane, geo: PaneGeo, p: egui::Pos2) {
        let pose = self.sides[self.cur].pose;
        let head_d = p.distance(self.marker_screen(geo, plane, pose.head));
        let tail_d = p.distance(self.marker_screen(geo, plane, pose.tail));
        if !self.hide_fiducials && head_d.min(tail_d) <= HANDLE_HIT_EM * self.em {
            let m = if head_d <= tail_d {
                Marker::Head
            } else {
                Marker::Tail
            };
            self.select_marker(m);
        } else {
            let (u, v) = geo.uv_of(p);
            self.set_crosshair_from_uv(plane, u, v);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_side(
        &self,
        painter: &egui::Painter,
        pal: &Palette,
        pts_per_mm: f32,
        project: &dyn Fn(V3) -> (egui::Pos2, f64),
        side: &SideState,
        active: bool,
        only: Option<Marker>,
    ) {
        let em = self.em;
        let dim = if active { 1.0 } else { 0.4 };
        let fade = |c: egui::Color32, d: f64| {
            let near = if d.abs() > NEAR_SLICE_MM { 0.5 } else { 1.0 };
            c.gamma_multiply(dim * near)
        };
        let axis_color = if active { pal.info } else { pal.text_muted };
        if let Some(g) = &side.geom {
            for pair in g.trajectory.windows(2) {
                let (a, da) = project(pair[0]);
                let (b, db) = project(pair[1]);
                painter.line_segment(
                    [a, b],
                    egui::Stroke::new(
                        STROKE_MEDIUM_EM * em,
                        fade(axis_color, da.abs().min(db.abs())),
                    ),
                );
            }
            let radius = ((0.5 * side.electrode.lead_diameter_mm) as f32 * pts_per_mm)
                .max(CONTACT_MIN_RADIUS_EM * em);
            for c in &g.coords_mm {
                let (p, d) = project(*c);
                painter.circle_filled(p, radius, fade(pal.warn, d).gamma_multiply(0.35));
                painter.circle_stroke(
                    p,
                    radius,
                    egui::Stroke::new(STROKE_MEDIUM_EM * em, fade(pal.warn, d)),
                );
            }
        }
        let handle_radius = (((0.5 * side.electrode.lead_diameter_mm) as f32 * pts_per_mm)
            * HANDLE_RADIUS_FACTOR)
            .max(HANDLE_MIN_RADIUS_EM * em);
        for m in [Marker::Head, Marker::Tail] {
            if only.is_some_and(|o| o != m) {
                continue;
            }
            let (p, d) = project(side.marker(m));
            if active && self.sel == m {
                painter.circle_filled(p, handle_radius, fade(pal.accent, d).gamma_multiply(0.7));
                painter.circle_stroke(
                    p,
                    handle_radius,
                    egui::Stroke::new(STROKE_THICK_EM * em, fade(pal.accent, d)),
                );
            } else {
                painter.circle_stroke(
                    p,
                    handle_radius,
                    egui::Stroke::new(STROKE_MEDIUM_EM * em, fade(pal.text, d)),
                );
            }
        }
        let (origin, d0) = project(side.pose.head);
        let (tip, _) = project(add(side.pose.head, scale(side.y_dir, ARROW_MM)));
        let vec = tip - origin;
        if vec.length() > ARROW_MIN_EM * em {
            painter.arrow(
                origin,
                vec,
                egui::Stroke::new(STROKE_MEDIUM_EM * em, fade(pal.accent, d0)),
            );
        }
    }

    fn paint_overlay(
        &self,
        painter: &egui::Painter,
        pal: &Palette,
        geo: PaneGeo,
        plane: RefinePlane,
    ) {
        let (ua, va, ta) = plane.axes();
        let dims = self.vol.dims();
        let cu = (self.slice[ua] as f64 + 0.5) / dims[ua] as f64;
        let cv = (self.slice[va] as f64 + 0.5) / dims[va] as f64;
        let c = geo.screen_pos(cu, cv);
        let stroke = egui::Stroke::new(STROKE_THIN_EM * self.em, pal.info.gamma_multiply(0.55));
        painter.line_segment(
            [
                egui::pos2(geo.img.min.x, c.y),
                egui::pos2(geo.img.max.x, c.y),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(c.x, geo.img.min.y),
                egui::pos2(c.x, geo.img.max.y),
            ],
            stroke,
        );
        if self.hide_fiducials {
            return;
        }
        let slice = self.slice[ta];
        let geometry = self.vol.geometry();
        let project = |w: V3| {
            let (u, v, d) = geometry.plane_uv_for_world(plane, w, slice);
            (geo.screen_pos(u, v), d)
        };
        for (i, side) in self.sides.iter().enumerate() {
            if i != self.cur {
                self.paint_side(painter, pal, geo.pts_per_mm, &project, side, false, None);
            }
        }
        self.paint_side(
            painter,
            pal,
            geo.pts_per_mm,
            &project,
            &self.sides[self.cur],
            true,
            None,
        );
    }

    fn paint_tag(
        &self,
        painter: &egui::Painter,
        ui: &egui::Ui,
        pal: &Palette,
        anchor: egui::Pos2,
        align: egui::Align2,
        text: String,
    ) {
        let galley =
            painter.layout_no_wrap(text, egui::TextStyle::Small.resolve(ui.style()), pal.text);
        let pad = TITLE_PAD_EM * self.em;
        let size = galley.size() + egui::vec2(2.0 * pad, 2.0 * pad);
        let rect = align.anchor_size(anchor, size);
        painter.rect_filled(
            rect,
            TITLE_CORNER_EM * self.em,
            pal.canvas.gamma_multiply(0.75),
        );
        painter.galley(rect.min + egui::vec2(pad, pad), galley, pal.text);
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_frame_labels(
        &self,
        painter: &egui::Painter,
        ui: &egui::Ui,
        pal: &Palette,
        rect: egui::Rect,
        img: egui::Rect,
        title: String,
        right: V3,
        up: Option<V3>,
    ) {
        let inset = TITLE_INSET_EM * self.em;
        self.paint_tag(
            painter,
            ui,
            pal,
            rect.min + egui::vec2(inset, inset),
            egui::Align2::LEFT_TOP,
            title,
        );
        let line = ui.text_style_height(&egui::TextStyle::Small) + 2.0 * TITLE_PAD_EM * self.em;
        let pane_top = rect.min.y + 2.0 * inset + line;
        let inner = img.intersect(rect);
        let rect = if inner.width() > 4.0 * line && inner.height() > 4.0 * line {
            inner
        } else {
            rect
        };
        let top = (rect.min.y + inset).max(pane_top);
        let mid = rect.center();
        let up = up.unwrap_or([0.0; 3]);
        let tags = [
            (
                egui::pos2(rect.min.x + inset, mid.y),
                egui::Align2::LEFT_CENTER,
                caption(scale(right, -1.0)),
            ),
            (
                egui::pos2(rect.max.x - inset, mid.y),
                egui::Align2::RIGHT_CENTER,
                caption(right),
            ),
            (
                egui::pos2(mid.x, top),
                egui::Align2::CENTER_TOP,
                caption(up),
            ),
            (
                egui::pos2(mid.x, rect.max.y - inset),
                egui::Align2::CENTER_BOTTOM,
                caption(scale(up, -1.0)),
            ),
        ];
        for (pos, align, text) in tags {
            if norm(up) > 0.0
                || matches!(
                    align,
                    egui::Align2::LEFT_CENTER | egui::Align2::RIGHT_CENTER
                )
            {
                self.paint_tag(painter, ui, pal, pos, align, text.to_owned());
            }
        }
    }

    fn pane(&mut self, ui: &mut egui::Ui, rect: egui::Rect, plane: RefinePlane) {
        let ctx = ui.ctx().clone();
        self.ensure_texture(&ctx, plane);
        let pal = ui_tokens::palette_for(ui);
        let pi = plane.index();
        let id = ui.id().with(("refine_pane", pi));
        let resp = ui.interact(rect, id, egui::Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        painter.rect_filled(rect, 0.0, pal.canvas);
        let Some(tex) = self.tex[pi].clone() else {
            return;
        };
        let extent = self.vol.geometry().plane_extent_mm(plane);
        if self.fit_pending[pi] {
            self.fit_pending[pi] = false;
            let (ua, va, _) = plane.axes();
            let dims = self.vol.dims();
            let cu = (self.slice[ua] as f64 + 0.5) / dims[ua] as f64;
            let cv = (self.slice[va] as f64 + 0.5) / dims[va] as f64;
            let (zoom, pan) = fit_zoom_and_pan(rect, tex.size_vec2(), extent, (cu, cv));
            self.zoom[pi] = zoom;
            self.pan[pi] = pan;
        }
        let geo0 = PaneGeo::new(rect, tex.size_vec2(), self.zoom[pi], self.pan[pi], extent);
        self.pane_input(&ctx, &resp, rect, plane, geo0);
        if self.tex_key[pi].is_none() || self.tex[pi].is_none() {
            return;
        }
        self.ensure_texture(&ctx, plane);
        let tex = self.tex[pi].clone().unwrap_or(tex);
        let geo = PaneGeo::new(rect, tex.size_vec2(), self.zoom[pi], self.pan[pi], extent);
        painter.image(
            tex.id(),
            geo.img,
            SLICE_TEX_UV_FLIPPED_Y,
            egui::Color32::WHITE,
        );
        self.paint_overlay(&painter, &pal, geo, plane);
        let (ua, va, ta) = plane.axes();
        let title = format!(
            "{} ({}) {} / {}",
            plane.label(),
            ["i", "j", "k"][ta],
            self.slice[ta] + 1,
            self.vol.dims()[ta]
        );
        let g = self.vol.geometry();
        self.paint_frame_labels(
            &painter,
            ui,
            &pal,
            rect,
            geo.img,
            title,
            g.axis_direction(ua),
            Some(g.axis_direction(va)),
        );
    }

    fn oblique_target(&self, idx: usize) -> Option<PlaneFrame> {
        if let Some(DragState {
            pane: DragPane::Oblique { idx: d, frame },
            ..
        }) = self.drag
        {
            if d == idx {
                return Some(frame);
            }
        }
        let pose = self.sides[self.cur].pose;
        match OBLIQUE_PANES.get(idx)? {
            PaneKind::Lead(view) => self.lead_frame_for(*view, &pose),
            PaneKind::Close(m) => Some(closeup_frame(self.vol.geometry(), pose.at(*m))),
            PaneKind::Ortho(_) => None,
        }
    }

    pub(crate) fn resample_oblique(&mut self, idx: usize) -> bool {
        let Some(frame) = self.oblique_target(idx) else {
            self.oblique[idx].sampled = None;
            return false;
        };
        let slab = self.slab && matches!(OBLIQUE_PANES[idx], PaneKind::Lead(_));
        let pane = &mut self.oblique[idx];
        if pane.sampled == Some((frame, slab)) {
            return false;
        }
        let offsets: &[f64] = if slab { &SLAB_OFFSETS_MM } else { &[0.0] };
        let (w, h) = sample_frame(&self.vol, &frame, SAMPLE_STEP_MM, offsets, &mut pane.raw);
        pane.size = [w, h];
        pane.sampled = Some((frame, slab));
        pane.generation += 1;
        true
    }

    fn ensure_oblique_texture(&mut self, ctx: &egui::Context, idx: usize) {
        self.resample_oblique(idx);
        let (center, width) = (self.wl_center, self.wl_width);
        let pane = &mut self.oblique[idx];
        if pane.sampled.is_none() {
            return;
        }
        let key = (pane.generation, center.to_bits(), width.to_bits());
        if pane.tex_key == Some(key) && pane.tex.is_some() {
            return;
        }
        window_rgba(&pane.raw, center, width, &mut pane.rgba);
        let image = egui::ColorImage::from_rgba_unmultiplied(pane.size, &pane.rgba);
        match &mut pane.tex {
            Some(handle) => handle.set(image, Default::default()),
            slot => {
                *slot = Some(ctx.load_texture(
                    format!("refine_oblique_{idx}"),
                    image,
                    Default::default(),
                ))
            }
        }
        pane.tex_key = Some(key);
    }

    fn oblique_only(idx: usize) -> Option<Marker> {
        match OBLIQUE_PANES[idx] {
            PaneKind::Close(m) => Some(m),
            _ => None,
        }
    }

    fn oblique_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        idx: usize,
        geo: ObliqueGeo,
    ) {
        let kind = OBLIQUE_PANES[idx];
        let (modifiers, press_origin) = ctx.input(|i| (i.modifiers, i.pointer.press_origin()));
        let plain = !modifiers.shift && !modifiers.alt;
        if resp.hovered() {
            self.hover_pane = Some(kind);
            self.last_pane = kind;
        }
        self.wl_drag(resp, modifiers);
        {
            let pane = &mut self.oblique[idx];
            if pane.zoom > 1.01 {
                let mid = resp.dragged_by(egui::PointerButton::Middle);
                let alt_primary = resp.dragged_by(egui::PointerButton::Primary) && modifiers.alt;
                if mid || alt_primary {
                    pane.pan += resp.drag_delta() * PAN_DRAG_GAIN;
                }
            }
            if resp.dragged_by(egui::PointerButton::Secondary) {
                adjust_zoom_right_drag(&mut pane.zoom, resp.drag_delta().y);
            }
        }
        if resp.drag_started_by(egui::PointerButton::Primary) && plain {
            if let Some(p0) = press_origin {
                let pose = self.sides[self.cur].pose;
                let (head_s, _) = geo.project(pose.head);
                let (tail_s, _) = geo.project(pose.tail);
                let mode = self.hit_mode(p0, head_s, tail_s, Self::oblique_only(idx));
                let (u0, v0) = geo.uv_of(p0);
                self.drag = Some(DragState {
                    mode,
                    pane: DragPane::Oblique {
                        idx,
                        frame: geo.frame,
                    },
                    start: pose,
                    grab: geo.frame.world(u0, v0),
                });
                if !matches!(mode, DragMode::Crosshair) {
                    self.begin_gesture();
                }
            }
        }
        if resp.dragged_by(egui::PointerButton::Primary) {
            if let (Some(state), Some(p)) = (self.drag, resp.interact_pointer_pos()) {
                if let DragPane::Oblique { idx: d, frame } = state.pane {
                    if d == idx {
                        let (u, v) = geo.uv_of(p);
                        let world = frame.world(u, v);
                        if matches!(state.mode, DragMode::Crosshair) {
                            self.set_crosshair_world(world);
                        } else {
                            self.drag_to(state, world);
                        }
                    }
                }
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
            self.end_gesture();
        }
        if resp.clicked_by(egui::PointerButton::Primary) && plain {
            if let Some(p) = resp.interact_pointer_pos() {
                let pose = self.sides[self.cur].pose;
                let only = Self::oblique_only(idx);
                let hit = |m: Marker| {
                    if only.is_some_and(|o| o != m) {
                        f32::INFINITY
                    } else {
                        p.distance(geo.project(pose.at(m)).0)
                    }
                };
                let (head_d, tail_d) = (hit(Marker::Head), hit(Marker::Tail));
                if !self.hide_fiducials && head_d.min(tail_d) <= HANDLE_HIT_EM * self.em {
                    self.select_marker(if head_d <= tail_d {
                        Marker::Head
                    } else {
                        Marker::Tail
                    });
                } else {
                    let (u, v) = geo.uv_of(p);
                    self.set_crosshair_world(geo.frame.world(u, v));
                }
            }
        }
        if resp.hovered() {
            let (sc, pinch) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let pane = &mut self.oblique[idx];
            if (pinch - 1.0).abs() > 1e-4 {
                adjust_zoom_pinch(&mut pane.zoom, pinch);
            } else if modifiers.ctrl && sc != 0.0 {
                adjust_zoom(&mut pane.zoom, sc);
            }
        }
        let pane = &mut self.oblique[idx];
        pane.zoom = pane.zoom.clamp(ZOOM_MIN, ZOOM_MAX);
    }

    fn oblique_title(&self, idx: usize) -> String {
        match OBLIQUE_PANES[idx] {
            PaneKind::Lead(view) if self.slab => format!("{} · X-ray", view.title()),
            PaneKind::Lead(view) => view.title().to_owned(),
            PaneKind::Close(Marker::Head) => "Head, axial".to_owned(),
            PaneKind::Close(Marker::Tail) => "Tail, axial".to_owned(),
            PaneKind::Ortho(p) => p.label().to_owned(),
        }
    }

    fn oblique_pane(&mut self, ui: &mut egui::Ui, rect: egui::Rect, idx: usize) {
        let ctx = ui.ctx().clone();
        self.ensure_oblique_texture(&ctx, idx);
        let pal = ui_tokens::palette_for(ui);
        let id = ui.id().with(("refine_oblique", idx));
        let resp = ui.interact(rect, id, egui::Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        painter.rect_filled(rect, 0.0, pal.canvas);
        let Some((frame, _)) = self.oblique[idx].sampled else {
            return;
        };
        let geo0 = ObliqueGeo::new(rect, frame, self.oblique[idx].zoom, self.oblique[idx].pan);
        self.oblique_input(&ctx, &resp, idx, geo0);
        self.ensure_oblique_texture(&ctx, idx);
        let pane = &self.oblique[idx];
        let (Some((frame, _)), Some(tex)) = (pane.sampled, pane.tex.clone()) else {
            return;
        };
        let geo = ObliqueGeo::new(rect, frame, pane.zoom, pane.pan);
        painter.image(
            tex.id(),
            geo.img,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        if !self.hide_fiducials {
            let project = |w: V3| geo.project(w);
            self.paint_side(
                &painter,
                &pal,
                geo.pts_per_mm,
                &project,
                &self.sides[self.cur],
                true,
                Self::oblique_only(idx),
            );
        }
        let title = self.oblique_title(idx);
        let up = (!matches!(OBLIQUE_PANES[idx], PaneKind::Lead(_))).then_some(frame.up);
        self.paint_frame_labels(&painter, ui, &pal, rect, geo.img, title, frame.right, up);
    }

    fn lead_layout(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let gap = PANE_GAP_EM * self.em;
        let (w, h) = (rect.width(), rect.height());
        let close_w = ((h - gap) * 0.5).min(w * CLOSEUP_WIDTH_SHARE).max(1.0);
        let lead_w = (w - close_w - gap).max(1.0);
        let (ew, eh) = self
            .oblique_target(0)
            .map(|f| f.extent())
            .unwrap_or((20.0, 30.0));
        let (ew, eh) = (ew.max(1e-3) as f32, eh.max(1e-3) as f32);
        let side_by_side = ((lead_w - gap) * 0.5 / ew).min(h / eh);
        let stacked = (lead_w / ew).min((h - gap) * 0.5 / eh);
        let leads = if side_by_side >= stacked {
            let half = (lead_w - gap) * 0.5;
            [
                egui::Rect::from_min_size(rect.min, egui::vec2(half, h)),
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + half + gap, rect.min.y),
                    egui::vec2(half, h),
                ),
            ]
        } else {
            let half = (h - gap) * 0.5;
            [
                egui::Rect::from_min_size(rect.min, egui::vec2(lead_w, half)),
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x, rect.min.y + half + gap),
                    egui::vec2(lead_w, half),
                ),
            ]
        };
        let cx = rect.min.x + lead_w + gap;
        let ch = (h - gap) * 0.5;
        let head = egui::Rect::from_min_size(egui::pos2(cx, rect.min.y), egui::vec2(close_w, ch));
        let tail = egui::Rect::from_min_size(
            egui::pos2(cx, rect.min.y + ch + gap),
            egui::vec2(close_w, ch),
        );
        self.oblique_pane(ui, leads[0], 0);
        self.oblique_pane(ui, leads[1], 1);
        self.oblique_pane(ui, head, 2);
        self.oblique_pane(ui, tail, 3);
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, egui::Sense::hover());
        self.hover_pane = None;
        let gap = PANE_GAP_EM * self.em;
        match self.layout {
            LayoutMode::Lead => self.lead_layout(ui, rect),
            LayoutMode::Single => {
                let plane = self.single;
                self.pane(ui, rect, plane);
            }
            LayoutMode::Triple => {
                let half_w = (rect.width() - gap) * 0.5;
                let half_h = (rect.height() - gap) * 0.5;
                let left = egui::Rect::from_min_size(rect.min, egui::vec2(half_w, rect.height()));
                let right_x = rect.min.x + half_w + gap;
                let top = egui::Rect::from_min_size(
                    egui::pos2(right_x, rect.min.y),
                    egui::vec2(half_w, half_h),
                );
                let bottom = egui::Rect::from_min_size(
                    egui::pos2(right_x, rect.min.y + half_h + gap),
                    egui::vec2(half_w, half_h),
                );
                self.pane(ui, left, RefinePlane::Axial);
                self.pane(ui, top, RefinePlane::Coronal);
                self.pane(ui, bottom, RefinePlane::Sagittal);
            }
        }
    }

    pub(crate) fn status_text(&self) -> String {
        let s = &self.sides[self.cur];
        let head_shift = norm(sub(s.pose.head, s.file_head));
        let tail_shift = norm(sub(s.pose.tail, s.file_tail));
        let roll = s.roll_deg();
        let delta = wrap_deg(roll - s.file_roll);
        let roll_part = if s.electrode.is_directed {
            format!(" · roll {roll:.1}° (Δ {delta:+.1}°)")
        } else {
            String::new()
        };
        let spacing_part = self
            .spacing_mm()
            .map(|(live, cat)| {
                format!(
                    " · Spacing: {live:.2} mm (catalogue {cat:.2} mm, ratio {:.3})",
                    live / cat
                )
            })
            .unwrap_or_default();
        format!(
            "{} · head shift {head_shift:.2} mm · tail shift {tail_shift:.2} mm{roll_part}{spacing_part} · tilt from vertical {:.1}° · {}",
            s.label,
            s.tilt_from_vertical_deg(),
            s.electrode.elmodel
        )
    }

    /// Live and catalogue mean level spacing of the current side, `specs/12` §12 item 17.
    pub(crate) fn spacing_mm(&self) -> Option<(f64, f64)> {
        let s = &self.sides[self.cur];
        let cat = catalogue_spacing_mm(s.electrode)?;
        let live = live_spacing_mm(s.electrode, s.pose.head, s.pose.tail, s.y_dir)?;
        Some((live, cat))
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let pal = ui_tokens::palette_for(ui);
        ui.horizontal_wrapped(|ui| {
            ui.label(self.status_text());
            if let Some(msg) = &self.status {
                ui.colored_label(pal.warn, msg);
            }
            if let Some(err) = &self.sides[self.cur].geom_error {
                ui.colored_label(pal.warn, format!("Lead geometry: {err}"));
            }
        });
    }

    fn widget_gesture_state(&mut self, r: &egui::Response) {
        let released = r.drag_stopped() || r.lost_focus();
        let discrete = r.changed() && !r.dragged() && !r.has_focus();
        if (released || discrete) && self.drag.is_none() {
            self.end_gesture();
        }
    }

    fn marker_fields(&mut self, ui: &mut egui::Ui, marker: Marker, name: &str) {
        let pos = self.sides[self.cur].marker(marker);
        ui.horizontal_wrapped(|ui| {
            ui.label(name);
            for (a, prefix) in ["x ", "y ", "z "].iter().enumerate() {
                let mut v = pos[a];
                let r = ui
                    .add(
                        egui::DragValue::new(&mut v)
                            .speed(0.01)
                            .min_decimals(2)
                            .max_decimals(4)
                            .prefix(*prefix)
                            .suffix(" mm"),
                    )
                    .on_hover_text(
                        "Coordinate in millimetres of the image space shown. Type a value or drag.",
                    );
                if r.changed() && (v - pos[a]).abs() > 1e-9 {
                    let mut new = self.sides[self.cur].marker(marker);
                    new[a] = v;
                    if self.move_whole_numeric {
                        self.translate_lead_to(marker, new, true);
                    } else {
                        self.move_marker_gesture(marker, new);
                    }
                }
                self.widget_gesture_state(&r);
            }
        });
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let mut pick_side = None;
        ui.horizontal_wrapped(|ui| {
            for (i, s) in self.sides.iter().enumerate() {
                let mut text = s.label.clone();
                if let Some(review) = s.review_text() {
                    text.push_str(" · ");
                    text.push_str(review);
                }
                if s.changed() {
                    text.push_str(" · edited");
                }
                if s.accept_pending() {
                    text.push_str(" · accepted, not saved");
                }
                if ui
                    .selectable_label(i == self.cur, text)
                    .on_hover_text("Choose the lead side to edit (keys < and >).")
                    .clicked()
                {
                    pick_side = Some(i);
                }
            }
        });
        if let Some(i) = pick_side {
            self.select_side(i);
        }
        {
            let s = &self.sides[self.cur];
            let (space, space_long) = match self.req.space {
                EditSpace::Native => ("native space", "coregistered post-op CT"),
                EditSpace::Mni => ("template space", "normalized post-op MR"),
            };
            let mut hover = format!("Edit space: {space} ({space_long}).");
            if let Some(st) = &s.orientation_status {
                hover.push_str(&format!(" Orientation estimate: {st}."));
            }
            match s.review_text() {
                Some("corrected") => hover.push_str(" Already manually corrected."),
                Some(_) => hover.push_str(" Already accepted as checked."),
                None => {}
            }
            ui.label(egui::RichText::new(format!("{} · {space}", s.electrode.elmodel)).weak())
                .on_hover_text(hover);
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Marker");
            let mut sel = self.sel;
            ui.selectable_value(&mut sel, Marker::Head, "Head (H)")
                .on_hover_text("Distal marker, contact 0.");
            ui.selectable_value(&mut sel, Marker::Tail, "Tail (T)")
                .on_hover_text("Proximal marker.");
            if sel != self.sel {
                self.select_marker(sel);
            }
            widgets::info_icon(
                ui,
                "Keys H and T choose the head (distal, contact 0) or the tail (proximal). Arrow keys nudge it in the hovered pane; Alt + arrows move the whole lead, Alt + Page Up / Page Down move it along its axis.",
            );
        });
        self.marker_fields(ui, Marker::Head, "Head");
        self.marker_fields(ui, Marker::Tail, "Tail");
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.keep_length, "Keep lead length")
                .on_hover_text("Moving the head or tail swings it about the other marker, so the distance between them stays the same (key K). Typed coordinates are not changed.");
            ui.checkbox(&mut self.move_whole_numeric, "Move whole lead")
                .on_hover_text("A typed head coordinate moves the tail by the same amount, and the other way round.");
        });

        ui.separator();
        if self.sides[self.cur].electrode.is_directed {
            ui.horizontal(|ui| {
                ui.label("Roll");
                let mut roll = self.sides[self.cur].roll_deg();
                let r = ui
                    .add(
                        egui::Slider::new(&mut roll, -180.0..=180.0)
                            .suffix("°")
                            .fixed_decimals(1),
                    )
                    .on_hover_text("Roll of the lead in degrees.");
                let touched =
                    r.dragged() || r.has_focus() || r.lost_focus() || r.is_pointer_button_down_on();
                let moved = wrap_deg(roll - self.sides[self.cur].roll_deg()).abs() > ROLL_EPS_DEG;
                if r.changed() && touched && moved {
                    self.set_roll_gesture(roll);
                }
                self.widget_gesture_state(&r);
                widgets::info_icon(
                    ui,
                    "Direction of segment A seen from above: 0 degrees = anterior, positive turns it toward the patient's right. Keys [ and ] turn it by 1 degree, with Shift by 10.",
                );
            });
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Flip 180°")
                    .on_hover_text("Turn the lead by half a turn.")
                    .clicked()
                {
                    self.flip();
                }
                let has_two = self.sides[self.cur].candidates.len() >= 2;
                if ui
                    .add_enabled(has_two, egui::Button::new("Other candidate"))
                    .on_hover_text(
                        "Switch to the other of the two orientations the automatic estimate found.",
                    )
                    .clicked()
                {
                    self.other_candidate();
                }
                if ui
                    .button("Automatic")
                    .on_hover_text("Back to the roll stored in the reconstruction file.")
                    .clicked()
                {
                    self.automatic_roll();
                }
            });
        } else {
            ui.label(egui::RichText::new("Ring contacts: roll not applicable").weak());
        }

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let (can_undo, can_redo) = {
                let s = &self.sides[self.cur];
                (!s.undo.is_empty(), !s.redo.is_empty())
            };
            if ui
                .add_enabled(can_undo, egui::Button::new("Undo"))
                .on_hover_text("Undo the last edit of this side (Ctrl + Z).")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(can_redo, egui::Button::new("Redo"))
                .on_hover_text("Redo (Ctrl + Y).")
                .clicked()
            {
                self.redo();
            }
            if ui
                .button("Reset side")
                .on_hover_text("Back to the markers and roll of the reconstruction file.")
                .clicked()
            {
                self.reset_side();
            }
            if ui
                .button("Accept side")
                .on_hover_text("Mark this side as checked without changing it and go to the next (Space). Save corrections records it; once every lead is corrected or accepted, the Reconstruct step counts as approved.")
                .clicked()
            {
                self.accept_side();
            }
        });

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.layout, LayoutMode::Lead, "Lead planes")
                .on_hover_text("Two planes through the lead (seen from the front and from the left) and axial close-ups of the head and tail (key L).");
            ui.selectable_value(&mut self.layout, LayoutMode::Triple, "Three planes")
                .on_hover_text("Show axial, coronal and sagittal planes (key 0).");
            ui.selectable_value(&mut self.layout, LayoutMode::Single, "Single pane")
                .on_hover_text("Show one plane at full size (keys 1, 2, 3).");
            widgets::info_icon(
                ui,
                "Wheel: change slice (three planes). Shift + left drag: window and level. Right drag, pinch or Ctrl + wheel: zoom. Middle drag or Alt + left drag: pan.",
            );
        });
        if self.layout == LayoutMode::Single {
            ui.horizontal(|ui| {
                for p in RefinePlane::ALL {
                    ui.selectable_value(&mut self.single, p, p.label())
                        .on_hover_text("Show this plane at full size.");
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.slab, "X-ray (slab)").on_hover_text(
                "Lead planes show the mean of a 9 mm thick slab (key X). Display only.",
            );
            ui.checkbox(&mut self.hide_fiducials, "Hide markers")
                .on_hover_text("Hide the handles, contacts and lead axis in every pane (key V). Keys still move the selected marker.");
        });
        egui::CollapsingHeader::new("Window / level and zoom")
            .id_salt(ui.id().with("refine_view_more"))
            .default_open(false)
            .show(ui, |ui| {
                let (lo, hi) = self.vol.intensity_range();
                let span = (hi - lo).max(1.0);
                ui.add(egui::Slider::new(&mut self.wl_center, lo..=hi).text("Level"))
                    .on_hover_text("Window / level centre in image units.");
                ui.add(egui::Slider::new(&mut self.wl_width, 1e-3..=span).text("Window"))
                    .on_hover_text("Window / level width in image units.");
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("Automatic window")
                        .on_hover_text("Bone window for CT, 0.5 to 99.5 percentile for MR.")
                        .clicked()
                    {
                        self.wl_center = self.default_wl.0;
                        self.wl_width = self.default_wl.1;
                    }
                    if ui
                        .button("Reset zoom and pan")
                        .on_hover_text("Fit every pane (key R).")
                        .clicked()
                    {
                        self.reset_view();
                    }
                    if ui
                        .button("Centre on marker")
                        .on_hover_text("Move the crosshair onto the selected marker (key F).")
                        .clicked()
                    {
                        self.centre_on_marker();
                    }
                });
            });
        let spare = self.controls_viewport_h - ui.min_rect().height() - 2.0 * self.em;
        if spare > MODEL_DIAGRAM_MIN_EM * self.em {
            ui.separator();
            let h = spare.min(MODEL_DIAGRAM_MAX_EM * self.em);
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
            self.paint_model_diagram(ui, rect);
            resp.on_hover_text("The electrode model: the head marker sits on the deepest contact level (contact 0), the tail marker on the most proximal level.");
        }
    }

    fn paint_model_diagram(&self, ui: &egui::Ui, rect: egui::Rect) {
        let pal = ui_tokens::palette_for(ui);
        let painter = ui.painter_at(rect);
        let e = self.sides[self.cur].electrode;
        let (Some(&z0), Some(&z1)) = (e.level_z_mm.first(), e.level_z_mm.last()) else {
            return;
        };
        let half = 0.5 * e.contact_length_mm.max(0.0);
        let tip = if e.tip_is_contact {
            0.0
        } else {
            e.tip_length_mm.max(0.0)
        };
        let z_bottom = z0 - half - tip;
        let z_top = z1 + half + MODEL_DIAGRAM_SHAFT_MM;
        let line = ui.text_style_height(&egui::TextStyle::Small);
        let usable = (rect.height() - 2.0 * line).max(1.0);
        let scale = usable / (z_top - z_bottom).max(1e-3) as f32;
        let width = (e.lead_diameter_mm as f32 * scale).min(rect.width() * 0.2);
        let cx = rect.min.x + rect.width() * 0.3;
        let y_of = |z: f64| rect.max.y - line - (z - z_bottom) as f32 * scale;
        let shaft = egui::Rect::from_min_max(
            egui::pos2(cx - 0.5 * width, y_of(z_top)),
            egui::pos2(cx + 0.5 * width, y_of(z_bottom)),
        );
        let round = egui::CornerRadius {
            sw: (0.5 * width) as u8,
            se: (0.5 * width) as u8,
            ..Default::default()
        };
        painter.rect_filled(shaft, round, pal.text_muted.gamma_multiply(0.35));
        for (level, &z) in e.level_z_mm.iter().enumerate() {
            let lo = if level == 0 && e.tip_is_contact {
                z_bottom
            } else {
                z - half
            };
            let r = egui::Rect::from_min_max(
                egui::pos2(shaft.min.x, y_of(z + half)),
                egui::pos2(shaft.max.x, y_of(lo)),
            );
            let corner = if level == 0 && e.tip_is_contact {
                round
            } else {
                egui::CornerRadius::ZERO
            };
            painter.rect_filled(r, corner, pal.warn);
            let segments = e.contact_level.iter().filter(|&&l| l == level).count();
            if segments > 1 {
                for k in 1..segments.min(3) {
                    let x = r.min.x + r.width() * k as f32 / segments.min(3) as f32;
                    painter.line_segment(
                        [egui::pos2(x, r.min.y), egui::pos2(x, r.max.y)],
                        egui::Stroke::new(STROKE_MEDIUM_EM * self.em, pal.canvas),
                    );
                }
            }
        }
        let font = egui::TextStyle::Small.resolve(ui.style());
        for (m, z, text) in [
            (Marker::Head, z0, "Head (contact 0)"),
            (Marker::Tail, z1, "Tail"),
        ] {
            let color = if self.sel == m { pal.accent } else { pal.text };
            let y = y_of(z);
            painter.circle_filled(egui::pos2(cx, y), STROKE_THICK_EM * self.em * 2.0, color);
            painter.line_segment(
                [
                    egui::pos2(shaft.max.x, y),
                    egui::pos2(shaft.max.x + 1.5 * self.em, y),
                ],
                egui::Stroke::new(STROKE_THIN_EM * self.em, color),
            );
            painter.text(
                egui::pos2(shaft.max.x + 1.8 * self.em, y),
                egui::Align2::LEFT_CENTER,
                text,
                font.clone(),
                color,
            );
        }
    }

    fn save_row(&mut self, ui: &mut egui::Ui) {
        let pal = ui_tokens::palette_for(ui);
        let dirty = self.dirty();
        let h = ui.text_style_height(&egui::TextStyle::Button) * 1.8;
        let gap = ui.spacing().item_spacing.x;
        let w = ui.available_width();
        ui.horizontal(|ui| {
            let save = egui::Button::new(
                egui::RichText::new("Save corrections")
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .fill(pal.accent_fill)
            .min_size(egui::vec2((w - gap) * 0.62, h));
            if ui
                .add_enabled(dirty, save)
                .on_hover_text(
                    "Write the changed and the accepted sides to the reconstruction (Ctrl + Enter).",
                )
                .clicked()
            {
                self.request_save();
            }
            if ui
                .add(egui::Button::new("Discard").min_size(egui::vec2((w - gap) * 0.38, h)))
                .on_hover_text("Close without saving (Esc).")
                .clicked()
            {
                self.request_discard();
            }
        });
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> RefineOutput {
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        let em = ui.text_style_height(&egui::TextStyle::Body);
        self.em = em;
        let base = ui.id().with("refine_panel");
        egui::Panel::right(base.with("controls"))
            .resizable(true)
            .default_size(em * 24.0)
            .show(ui, |ui| {
                egui::Panel::bottom(base.with("save"))
                    .show_separator_line(false)
                    .show(ui, |ui| self.save_row(ui));
                let out = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.controls(ui));
                self.controls_overflow = out.content_size.y > out.inner_rect.height() + 0.5;
                self.controls_viewport_h = out.inner_rect.height();
            });
        egui::Panel::bottom(base.with("status")).show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
        if self.confirm_discard {
            let mut choice = None;
            egui::Window::new("Discard changes?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label("At least one lead was changed or accepted and not saved.");
                    ui.horizontal(|ui| {
                        if ui
                            .button("Discard changes")
                            .on_hover_text("Close without saving.")
                            .clicked()
                        {
                            choice = Some(true);
                        }
                        if ui
                            .button("Keep editing")
                            .on_hover_text("Go back to the edit view (Esc).")
                            .clicked()
                        {
                            choice = Some(false);
                        }
                    });
                });
            if let Some(discard) = choice {
                self.resolve_discard(discard);
            }
        }
        std::mem::take(&mut self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refine_lead::{a_ref, azimuth_of, dot};
    use crate::refine_volume::Affine;
    use dbsloc_reconstruct::orientation::roll_from_anterior_deg;

    const MODEL: &str = "Boston Scientific Vercise Cartesia HX";
    const RING_MODEL: &str = "Medtronic 3387";

    fn ras_affine() -> Affine {
        [
            [1.0, 0.0, 0.0, -15.0],
            [0.0, 1.0, 0.0, -15.0],
            [0.0, 0.0, 1.0, -15.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn oblique_affine() -> Affine {
        let (s, c) = (0.5_f64.sin(), 0.5_f64.cos());
        [
            [0.8 * c, -1.1 * s, 0.0, -20.0],
            [0.8 * s, 1.1 * c, 0.0, 15.0],
            [0.0, 0.0, 1.5, -30.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn volume(affine: Affine) -> RefineVolume {
        let dims = [30usize, 30, 30];
        let data: Vec<f32> = (0..dims[0] * dims[1] * dims[2])
            .map(|n| (n % 97) as f32)
            .collect();
        RefineVolume::from_parts(dims, affine, data).unwrap()
    }

    fn side(key: &str, head: V3, tail: V3, y_dir: V3) -> RefineSide {
        let u = unit(sub(tail, head)).unwrap();
        let y = project_unit(y_dir, u).unwrap();
        let x = cross(y, u);
        let r = 0.5 * electrode_by_name(MODEL).unwrap().lead_diameter_mm;
        RefineSide {
            key: key.to_owned(),
            label: if key == "1" {
                "Right lead"
            } else {
                "Left lead"
            }
            .to_owned(),
            elmodel: Some(MODEL.to_owned()),
            head,
            tail,
            x: Some(add(head, scale(x, r))),
            y: Some(add(head, scale(y, r))),
            roll_deg: None,
            candidates_deg: Some(vec![20.0, 200.0]),
            orientation_status: Some("resolved".to_owned()),
            manually_corrected: false,
            review: None,
        }
    }

    fn request(affine: Affine) -> (RefineRequest, RefineVolume) {
        let req = RefineRequest {
            subj_id: "sub-test".to_owned(),
            space: EditSpace::Native,
            backdrop_nii: PathBuf::from("unused.nii.gz"),
            sides: vec![
                side("1", [2.0, 3.0, -4.0], [3.0, 5.0, 6.0], [0.0, 1.0, 0.0]),
                side("2", [-8.0, 1.0, -4.0], [-7.0, 2.0, 6.5], [1.0, 1.0, 0.0]),
            ],
        };
        (req, volume(affine))
    }

    fn panel(affine: Affine) -> RefinePanel {
        let (req, vol) = request(affine);
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        p.keep_length = false;
        p
    }

    fn close(a: V3, b: V3, tol: f64) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < tol)
    }

    #[test]
    fn nudge_right_on_axial_follows_voxel_i_axis() {
        let mut p = panel(ras_affine());
        let (h0, t0) = p.current_markers();
        assert!(p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Normal,
            false
        ));
        let (h1, t1) = p.current_markers();
        assert!(close(sub(h1, h0), [0.25, 0.0, 0.0], 1e-12));
        assert_eq!(t1, t0);
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Large,
            false,
        );
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Vertical,
            1,
            StepSize::Fine,
            false,
        );
        let (h2, _) = p.current_markers();
        assert!(close(sub(h2, h1), [1.0, 0.05, 0.0], 1e-12));
        p.nudge(
            RefinePlane::Sagittal,
            PaneAxis::Through,
            -1,
            StepSize::Normal,
            false,
        );
        let (h3, _) = p.current_markers();
        assert!(close(sub(h3, h2), [-0.25, 0.0, 0.0], 1e-12));
    }

    #[test]
    fn nudge_on_oblique_affine_maps_through_header() {
        let mut p = panel(oblique_affine());
        let (h0, _) = p.current_markers();
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            4,
            StepSize::Normal,
            false,
        );
        let (h1, _) = p.current_markers();
        let dir = p.vol.geometry().axis_direction(0);
        assert!(close(sub(h1, h0), scale(dir, 1.0), 1e-12));
        assert!(dir[0] > 0.0 && dir[1] > 0.0);
    }

    #[test]
    fn whole_lead_nudge_moves_both_markers() {
        let mut p = panel(ras_affine());
        let (h0, t0) = p.current_markers();
        p.nudge(
            RefinePlane::Coronal,
            PaneAxis::Vertical,
            1,
            StepSize::Large,
            true,
        );
        let (h1, t1) = p.current_markers();
        assert!(close(sub(h1, h0), [0.0, 0.0, 1.0], 1e-12));
        assert!(close(sub(t1, t0), [0.0, 0.0, 1.0], 1e-12));
    }

    #[test]
    fn roll_is_kept_when_head_moves_along_the_axis() {
        let mut p = panel(ras_affine());
        let before = p.current_roll_deg();
        let (h, t) = p.current_markers();
        let u = unit(sub(t, h)).unwrap();
        assert!(p.move_marker_to(Marker::Head, add(h, scale(u, -3.0))));
        assert!((p.current_roll_deg() - before).abs() < 1e-6);
    }

    #[test]
    fn explicit_roll_survives_lateral_head_move() {
        let mut p = panel(ras_affine());
        p.set_roll(30.0);
        let (h, _) = p.current_markers();
        assert!(p.move_marker_to(Marker::Head, add(h, [3.0, 0.0, 0.0])));
        assert!((p.current_roll_deg() - 30.0).abs() < 1e-6);
        let side = &p.sides[0];
        let u = side.axis_unit().unwrap();
        assert!((roll_of(u, side.y_dir) - 30.0).abs() < 1e-6);
        assert!((azimuth_of(side.y_dir).unwrap() - 30.0).abs() < 1e-6);
    }

    #[test]
    fn panel_roll_matches_pipeline_rebuild_for_kept_roll() {
        let mut p = panel(ras_affine());
        let (h, _) = p.current_markers();
        p.move_marker_to(Marker::Head, add(h, [3.0, 0.0, 0.0]));
        let s = &p.sides[0];
        let u = s.axis_unit().unwrap();
        let kept = azimuth_of(s.file_y.unwrap()).unwrap();
        assert!(close(s.y_dir, azimuth_y(u, kept).unwrap(), 1e-12));
        assert!((azimuth_of(s.y_dir).unwrap() - kept).abs() < 1e-9);
        assert!(dot(s.y_dir, u).abs() < 1e-12);
        let projected = project_unit(s.file_y.unwrap(), u).unwrap();
        assert!((azimuth_of(projected).unwrap() - kept).abs() > 0.01);
        assert!((p.current_roll_deg() - kept).abs() < 1e-9);
        assert!(!s.roll_changed());
    }

    #[test]
    fn ten_bracket_presses_are_ten_degrees_and_flip_twice_is_identity() {
        let mut p = panel(ras_affine());
        let r0 = p.current_roll_deg();
        for _ in 0..10 {
            p.roll_by(ROLL_STEP_DEG);
        }
        assert!(wrap_deg(p.current_roll_deg() - r0 - 10.0).abs() < 1e-9);
        let r1 = p.current_roll_deg();
        p.flip();
        assert!(wrap_deg(p.current_roll_deg() - r1 - 180.0).abs() < 1e-9);
        p.flip();
        assert!(wrap_deg(p.current_roll_deg() - r1).abs() < 1e-9);
        let mut q = panel(ras_affine());
        q.flip();
        q.flip();
        assert!(q.sides[0].pose.roll.is_none());
        assert!(!q.dirty());
    }

    #[test]
    fn save_lists_only_edited_sides_and_roll_only_when_changed() {
        let mut p = panel(ras_affine());
        assert!(p.save_edits().is_empty());
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Normal,
            false,
        );
        let edits = p.save_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].key, "1");
        assert!(edits[0].roll_deg.is_none());
        p.select_side(1);
        p.roll_by(10.0);
        let edits = p.save_edits();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[1].key, "2");
        assert!(edits[1].roll_deg.is_some());
        assert!(edits[0].roll_deg.is_none());
        p.request_save();
        let out = std::mem::take(&mut p.out);
        assert_eq!(out.save.unwrap().len(), 2);
        assert!(!out.cancelled);
    }

    #[test]
    fn undo_restores_markers_exactly_and_redo_reapplies() {
        let mut p = panel(oblique_affine());
        let before = p.current_markers();
        p.nudge(
            RefinePlane::Coronal,
            PaneAxis::Horizontal,
            3,
            StepSize::Fine,
            false,
        );
        let after = p.current_markers();
        assert_ne!(before, after);
        assert!(p.undo());
        assert_eq!(p.current_markers(), before);
        assert!(p.redo());
        assert_eq!(p.current_markers(), after);
        assert!(!p.redo());
    }

    #[test]
    fn coincident_markers_are_refused() {
        let mut p = panel(ras_affine());
        let (h, t) = p.current_markers();
        assert!(!p.move_marker_to(Marker::Tail, h));
        assert_eq!(p.current_markers(), (h, t));
        assert!(!p.sides[0].undo.iter().any(|s| s.tail == h));
        assert!(!p.dirty());
    }

    #[test]
    fn markers_are_never_rounded() {
        let mut p = panel(ras_affine());
        let target = [1.234_567_891_234, -2.000_000_000_123, 3.141_592_000_589];
        assert!(p.move_marker_to(Marker::Head, target));
        assert_eq!(p.current_markers().0, target);
        assert_eq!(p.save_edits()[0].head, target);
    }

    #[test]
    fn reset_automatic_and_candidates() {
        let mut p = panel(ras_affine());
        assert_eq!(p.sides[0].candidates, vec![20.0, -160.0]);
        let r0 = p.current_roll_deg();
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Vertical,
            2,
            StepSize::Large,
            false,
        );
        p.set_roll(77.0);
        assert!(p.other_candidate());
        let r = p.current_roll_deg();
        assert!(p.sides[0]
            .candidates
            .iter()
            .any(|c| wrap_deg(r - c).abs() < 1e-9));
        assert!(p.reset_side());
        assert!(!p.dirty());
        assert!((p.current_roll_deg() - r0).abs() < 1e-6);
        p.set_roll(77.0);
        assert!(p.automatic_roll());
        assert!(!p.dirty());
    }

    #[test]
    fn an_accepted_side_is_saved_as_an_accept_without_an_edit() {
        let mut p = panel(ras_affine());
        assert!(!p.dirty());
        p.accept_side();
        assert!(p.dirty());
        let edits = p.save_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].key, "1");
        assert!(edits[0].accept);
        assert!(edits[0].roll_deg.is_none());
        p.request_save();
        assert_eq!(p.out.save.as_ref().map(Vec::len), Some(1));
        p.mark_saved();
        assert!(!p.dirty());
        assert_eq!(p.sides[0].review.as_deref(), Some("accepted"));
        assert!(!p.sides[0].manually_corrected);
        assert_eq!(p.sides[0].review_text(), Some("accepted"));
    }

    #[test]
    fn an_edited_and_accepted_side_is_saved_as_a_correction() {
        let mut p = panel(ras_affine());
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Normal,
            false,
        );
        p.accept_side();
        let edits = p.save_edits();
        assert_eq!(edits.len(), 1);
        assert!(!edits[0].accept);
        p.request_save();
        assert_eq!(p.out.save.as_ref().map(Vec::len), Some(1));
        p.mark_saved();
        assert!(!p.dirty());
        assert!(p.save_edits().is_empty());
        assert_eq!(p.sides[0].review_text(), Some("corrected"));
    }

    #[test]
    fn accept_goes_to_next_side_and_wraps() {
        let mut p = panel(ras_affine());
        p.accept_side();
        assert_eq!(p.cur, 1);
        assert!(p.sides[0].accepted);
        p.accept_side();
        assert_eq!(p.cur, 0);
    }

    #[test]
    fn selecting_a_marker_moves_the_crosshair_onto_it() {
        let mut p = panel(ras_affine());
        p.select_marker(Marker::Tail);
        let (_, t) = p.current_markers();
        let ijk = p.vol.world_to_voxel_f(t);
        assert_eq!(p.slice, p.vol.geometry().clamp_index(ijk));
    }

    #[test]
    fn mark_saved_rebases_the_file_state() {
        let mut p = panel(ras_affine());
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            2,
            StepSize::Large,
            false,
        );
        p.set_roll(40.0);
        let roll = p.current_roll_deg();
        p.request_save();
        p.mark_saved();
        assert!(!p.dirty());
        assert!((p.current_roll_deg() - roll).abs() < 1e-9);
        assert!(p.sides[0].manually_corrected);
    }

    fn key_event(key: egui::Key, shift: bool, ctrl: bool) -> egui::Event {
        let modifiers = egui::Modifiers {
            shift,
            ctrl,
            command: ctrl,
            ..Default::default()
        };
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn run_keys(p: &mut RefinePanel, events: Vec<egui::Event>) {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            events,
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| p.handle_keys(ui.ctx()))
            .textures_delta
            .clear();
    }

    #[test]
    fn keyboard_drives_nudges_roll_undo_and_shift_is_not_swallowed() {
        let mut p = panel(ras_affine());
        let (h0, _) = p.current_markers();
        run_keys(&mut p, vec![key_event(egui::Key::ArrowRight, true, false)]);
        let (h1, _) = p.current_markers();
        assert!(close(sub(h1, h0), [1.0, 0.0, 0.0], 1e-12));
        run_keys(&mut p, vec![key_event(egui::Key::ArrowRight, false, true)]);
        let (h2, _) = p.current_markers();
        assert!(close(sub(h2, h1), [0.05, 0.0, 0.0], 1e-12));
        run_keys(&mut p, vec![key_event(egui::Key::ArrowUp, false, false)]);
        let (h3, _) = p.current_markers();
        assert!(close(sub(h3, h2), [0.0, 0.25, 0.0], 1e-12));
        let r0 = p.current_roll_deg();
        run_keys(
            &mut p,
            vec![key_event(egui::Key::CloseBracket, true, false)],
        );
        assert!(wrap_deg(p.current_roll_deg() - r0 - 10.0).abs() < 1e-9);
        run_keys(&mut p, vec![key_event(egui::Key::Z, false, true)]);
        assert!((p.current_roll_deg() - r0).abs() < 1e-9);
        run_keys(&mut p, vec![key_event(egui::Key::Z, false, true)]);
        assert_eq!(p.current_markers().0, h2);
        run_keys(&mut p, vec![key_event(egui::Key::Enter, false, true)]);
        assert!(p.out.save.is_some());
        run_keys(&mut p, vec![key_event(egui::Key::Escape, false, false)]);
        assert!(p.confirm_discard);
        assert!(!p.out.cancelled);
        run_keys(&mut p, vec![key_event(egui::Key::Escape, false, false)]);
        assert!(!p.confirm_discard);
        run_keys(&mut p, vec![key_event(egui::Key::Escape, false, false)]);
        p.resolve_discard(true);
        assert!(p.out.cancelled);
    }

    #[test]
    fn escape_cancels_immediately_when_nothing_changed() {
        let mut p = panel(ras_affine());
        run_keys(&mut p, vec![key_event(egui::Key::Escape, false, false)]);
        assert!(p.out.cancelled);
        assert!(!p.confirm_discard);
    }

    #[test]
    fn layout_and_selection_keys() {
        let mut p = panel(ras_affine());
        run_keys(&mut p, vec![key_event(egui::Key::Num2, false, false)]);
        assert_eq!(p.layout, LayoutMode::Single);
        assert_eq!(p.single, RefinePlane::Coronal);
        run_keys(&mut p, vec![key_event(egui::Key::Num0, false, false)]);
        assert_eq!(p.layout, LayoutMode::Triple);
        run_keys(&mut p, vec![key_event(egui::Key::T, false, false)]);
        assert_eq!(p.sel, Marker::Tail);
        run_keys(&mut p, vec![key_event(egui::Key::Period, true, false)]);
        assert_eq!(p.cur, 1);
    }

    #[test]
    fn request_json_round_trips_with_spec_field_names() {
        let json = r#"{
            "subj_id": "sub-01",
            "space": "native",
            "backdrop_nii": "coregistration/anat/sub-01_ct.nii.gz",
            "sides": [{
                "key": "1",
                "label": "Right lead",
                "elmodel": "Medtronic 3387",
                "head": [1.0, 2.0, 3.0],
                "tail": [1.0, 2.0, 13.0],
                "x": [1.6, 2.0, 3.0],
                "y": [1.0, 2.6, 3.0],
                "roll_deg": 12.5,
                "candidates_deg": [12.5, 192.5],
                "orientation_status": "resolved",
                "manually_corrected": false,
                "extra_field": 1
            }, {
                "key": "2",
                "label": "Left lead",
                "elmodel": null,
                "head": [0.0, 0.0, 0.0],
                "tail": [0.0, 0.0, 10.0],
                "x": null,
                "y": null,
                "roll_deg": null,
                "candidates_deg": null,
                "orientation_status": null
            }]
        }"#;
        let req: RefineRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.space, EditSpace::Native);
        assert_eq!(req.sides.len(), 2);
        assert_eq!(req.sides[0].roll_deg, Some(12.5));
        assert_eq!(req.sides[0].candidates_deg, Some(vec![12.5, 192.5]));
        assert!(req.sides[1].elmodel.is_none());
        assert!(!req.sides[1].manually_corrected);
        let back = serde_json::to_string(&req).unwrap();
        let again: RefineRequest = serde_json::from_str(&back).unwrap();
        assert_eq!(again, req);
        let space: EditSpace = serde_json::from_str("\"mni\"").unwrap();
        assert_eq!(space, EditSpace::Mni);

        let out = RefineOutput {
            save: Some(vec![SideEdit {
                key: "1".into(),
                head: [1.0, 2.0, 3.0],
                tail: [1.0, 2.0, 13.0],
                roll_deg: None,
                accept: false,
            }]),
            cancelled: false,
            errors: Vec::new(),
        };
        let text = serde_json::to_string(&out).unwrap();
        assert!(text.contains("\"roll_deg\":null"));
        let parsed: RefineOutput = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, out);
    }

    #[test]
    fn rejects_request_without_model_or_sides() {
        let (mut req, vol) = request(ras_affine());
        req.sides[0].elmodel = None;
        assert!(RefinePanel::with_volume(req, vol).is_err());
        let (mut req, vol) = request(ras_affine());
        req.sides.clear();
        assert!(RefinePanel::with_volume(req, vol).is_err());
        let (mut req, vol) = request(ras_affine());
        req.sides[0].tail = req.sides[0].head;
        assert!(RefinePanel::with_volume(req, vol).is_err());
    }

    #[test]
    fn missing_y_uses_the_anterior_reference_not_x() {
        let (mut req, vol) = request(ras_affine());
        req.sides[0].y = None;
        req.sides[0].roll_deg = Some(77.0);
        let p = RefinePanel::with_volume(req, vol).unwrap();
        let s = &p.sides[0];
        let u = s.axis_unit().unwrap();
        assert!(close(s.y_dir, roll_y(u, 0.0), 1e-12));
        assert!(s.file_roll.abs() < 1e-9);
        assert_eq!(s.request_roll, Some(77.0));
        assert!(dot(a_ref(u), s.y_dir) > 0.99);
    }

    #[test]
    fn inconsistent_request_roll_does_not_change_the_file_roll() {
        let (mut req, vol) = request(ras_affine());
        let probe = RefinePanel::with_volume(req.clone(), volume(ras_affine())).unwrap();
        let from_y = probe.sides[0].file_roll;
        req.sides[0].roll_deg = Some(wrap_deg(from_y + 40.0));
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        assert!(!p.dirty());
        assert!((p.current_roll_deg() - from_y).abs() < 1e-9);
        assert!(p.status_text().contains("Δ +0.0°"));
        assert!(p.automatic_roll());
        assert!(wrap_deg(p.current_roll_deg() - from_y - 40.0).abs() < 1e-9);
        assert!(p.reset_side());
        assert!((p.current_roll_deg() - from_y).abs() < 1e-9);
        assert!(!p.dirty());
    }

    fn frame_input(frame: u32) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1400.0, 800.0),
            )),
            time: Some(f64::from(frame) * 0.016),
            ..Default::default()
        }
    }

    #[test]
    fn thirty_frame_drag_is_one_undo_entry() {
        let mut p = panel(ras_affine());
        let ctx = egui::Context::default();
        let start = p.sides[0].pose;
        p.begin_gesture();
        p.drag = Some(DragState {
            mode: DragMode::Lead,
            pane: DragPane::Ortho {
                plane: RefinePlane::Axial,
                through: 0.0,
            },
            start,
            grab: [0.0; 3],
        });
        for frame in 0..30 {
            ctx.run_ui(frame_input(frame), |ui| {
                let _ = p.show(ui);
            })
            .textures_delta
            .clear();
            let dx = 0.1 * f64::from(frame + 1);
            let mut pose = start;
            pose.head[0] += dx;
            pose.tail[0] += dx;
            assert!(p.commit_gesture(pose));
        }
        p.drag = None;
        p.end_gesture();
        assert_eq!(p.sides[0].undo.len(), 1);
        assert!(p.undo());
        assert_eq!(p.current_markers(), (start.head, start.tail));
    }

    #[test]
    fn key_nudge_after_a_widget_gesture_is_its_own_undo_step() {
        let mut p = panel(ras_affine());
        let start = p.sides[0].pose;
        p.begin_gesture();
        let mut pose = start;
        pose.head[0] += 0.5;
        p.commit_gesture(pose);
        pose.head[0] += 0.5;
        p.commit_gesture(pose);
        assert_eq!(p.sides[0].undo.len(), 1);
        run_keys(&mut p, vec![key_event(egui::Key::ArrowRight, false, false)]);
        assert_eq!(p.sides[0].undo.len(), 2);
        assert!(p.gesture_pushed.is_none());
    }

    #[test]
    fn non_finite_moves_are_refused() {
        let mut p = panel(ras_affine());
        let before = p.current_markers();
        assert!(!p.move_marker_to(Marker::Head, [f64::INFINITY, 0.0, 0.0]));
        assert!(!p.move_marker_to(Marker::Tail, [f64::NAN, 0.0, 0.0]));
        assert_eq!(p.current_markers(), before);
        assert!(p.status.is_some());
    }

    #[test]
    fn save_is_blocked_when_a_changed_side_has_no_geometry() {
        let mut p = panel(ras_affine());
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Normal,
            false,
        );
        p.sides[0].geom = None;
        p.sides[0].geom_error = Some("rigid fit failed".to_owned());
        p.request_save();
        assert!(p.out.save.is_none());
        assert_eq!(p.out.errors.len(), 1);
        assert!(p.out.errors[0].contains("rigid fit failed"));
    }

    #[test]
    fn mark_saved_rebases_only_the_saved_sides() {
        let mut p = panel(ras_affine());
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Normal,
            false,
        );
        p.request_save();
        assert_eq!(p.out.save.as_ref().map(Vec::len), Some(1));
        p.select_side(1);
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Normal,
            false,
        );
        p.mark_saved();
        assert!(!p.sides[0].changed());
        assert!(p.sides[0].manually_corrected);
        assert!(p.sides[1].changed());
        assert!(!p.sides[1].manually_corrected);
        assert_eq!(p.save_edits().len(), 1);
        assert_eq!(p.save_edits()[0].key, "2");
    }

    #[test]
    fn arrow_keys_follow_the_last_hovered_pane() {
        let mut p = panel(ras_affine());
        p.layout = LayoutMode::Triple;
        p.last_pane = PaneKind::Ortho(RefinePlane::Coronal);
        let (h0, _) = p.current_markers();
        run_keys(&mut p, vec![key_event(egui::Key::ArrowUp, false, false)]);
        let (h1, _) = p.current_markers();
        assert!(close(sub(h1, h0), [0.0, 0.0, 0.25], 1e-12));
    }

    #[test]
    fn ring_contacts_have_no_roll() {
        let (mut req, vol) = request(ras_affine());
        req.sides[0].elmodel = Some(RING_MODEL.to_owned());
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        assert!(!p.set_roll(30.0));
        assert!(!p.roll_by(10.0));
        assert!(!p.flip());
        assert!(!p.other_candidate());
        assert!(!p.automatic_roll());
        assert!(!p.status_text().contains("roll"));
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Normal,
            false,
        );
        let edits = p.save_edits();
        assert_eq!(edits.len(), 1);
        assert!(edits[0].roll_deg.is_none());
        let ctx = egui::Context::default();
        ctx.run_ui(frame_input(0), |ui| {
            let _ = p.show(ui);
        })
        .textures_delta
        .clear();
    }

    #[test]
    fn fit_zoom_shows_fifty_millimetres_and_centres_the_crosshair() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(800.0, 600.0));
        let tex = egui::vec2(100.0, 100.0);
        let (zoom, pan) = fit_zoom_and_pan(rect, tex, (150.0, 150.0), (0.3, 0.6));
        assert!((zoom - 3.0).abs() < 1e-4);
        let geo = PaneGeo::new(rect, tex, zoom, pan, (150.0, 150.0));
        let c = geo.screen_pos(0.3, 0.6);
        assert!((c - rect.center()).length() < 1e-3);
        assert!((geo.pts_per_mm * 50.0 - 600.0).abs() < 1e-2);
        let (clamped, _) = fit_zoom_and_pan(rect, tex, (20.0, 20.0), (0.5, 0.5));
        assert!(clamped <= ZOOM_MAX);
    }

    #[test]
    fn open_and_centre_fit_the_panes_and_reset_means_zoom_one() {
        let mut p = panel(ras_affine());
        assert_eq!(p.layout, LayoutMode::Lead);
        p.layout = LayoutMode::Triple;
        assert_eq!(p.fit_pending, [true; 3]);
        let ctx = egui::Context::default();
        ctx.run_ui(frame_input(0), |ui| {
            let _ = p.show(ui);
        })
        .textures_delta
        .clear();
        assert_eq!(p.fit_pending, [false; 3]);
        assert!(p.zoom.iter().all(|z| *z != 1.0));
        p.reset_view();
        assert_eq!(p.zoom, [1.0; 3]);
        p.centre_on_marker();
        assert_eq!(p.fit_pending, [true; 3]);
        p.select_side(1);
        assert_eq!(p.fit_pending, [true; 3]);
    }

    #[test]
    fn corrected_tab_and_no_heading_in_controls() {
        let (mut req, vol) = request(ras_affine());
        req.sides[0].manually_corrected = true;
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        let ctx = egui::Context::default();
        ctx.run_ui(frame_input(0), |ui| {
            let _ = p.show(ui);
        })
        .textures_delta
        .clear();
        assert!(p.sides[0].manually_corrected);
    }

    fn nifti_bytes_with_sform(dims: [usize; 3], rows: [[f32; 4]; 3], voxels: &[f32]) -> Vec<u8> {
        let mut h = [0u8; 348];
        h[0..4].copy_from_slice(&348i32.to_le_bytes());
        h[40..42].copy_from_slice(&3i16.to_le_bytes());
        for (a, n) in dims.iter().enumerate() {
            h[42 + 2 * a..44 + 2 * a].copy_from_slice(&(*n as i16).to_le_bytes());
        }
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
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let off = 280 + 16 * r + 4 * c;
                h[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        h[344] = b'n';
        h[345] = b'+';
        h[346] = b'1';
        let mut out = Vec::new();
        out.extend_from_slice(&h);
        out.extend_from_slice(&[0u8; 4]);
        for v in voxels {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn panel_loads_an_oblique_nifti_from_disk() {
        let dir = std::env::temp_dir().join(format!("dbsloc_refine_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dims = [12usize, 14, 10];
        let rows = [
            [0.8f32, -0.3, 0.0, -9.0],
            [0.3, 0.8, 0.0, -10.0],
            [0.0, 0.0, 1.5, -7.0],
        ];
        let voxels: Vec<f32> = (0..dims[0] * dims[1] * dims[2]).map(|n| n as f32).collect();
        let path = dir.join("backdrop.nii");
        std::fs::write(&path, nifti_bytes_with_sform(dims, rows, &voxels)).unwrap();
        let (mut req, _) = request(ras_affine());
        req.backdrop_nii = path.clone();
        let p = RefinePanel::new(req.clone()).unwrap();
        let g = p.vol.geometry();
        assert_eq!(g.dims(), dims);
        let w = g.voxel_to_world([1.0, 2.0, 3.0]);
        assert!(close(
            w,
            [0.8 - 0.6 - 9.0, 0.3 + 1.6 - 10.0, 4.5 - 7.0],
            1e-5
        ));
        assert_eq!(p.request(), &req);
        assert!(!p.dirty());
        let json = dir.join("request.json");
        std::fs::write(&json, serde_json::to_string(&req).unwrap()).unwrap();
        assert!(RefinePanel::from_request_path(&json).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pane_geo_round_trip_and_flip() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(300.0, 200.0));
        let geo = PaneGeo::new(
            rect,
            egui::vec2(100.0, 100.0),
            1.0,
            egui::Vec2::ZERO,
            (50.0, 50.0),
        );
        let p = geo.screen_pos(0.25, 0.75);
        let (u, v) = geo.uv_of(p);
        assert!((u - 0.25).abs() < 1e-5 && (v - 0.75).abs() < 1e-5);
        assert!(geo.screen_pos(0.5, 0.9).y < geo.screen_pos(0.5, 0.1).y);
        assert!((geo.pts_per_mm - 4.0).abs() < 1e-5);
    }

    #[test]
    fn show_renders_frames_without_panicking() {
        let mut p = panel(oblique_affine());
        p.layout = LayoutMode::Triple;
        let ctx = egui::Context::default();
        for frame in 0..4 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1400.0, 800.0),
                )),
                time: Some(f64::from(frame) * 0.016),
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| {
                let out = p.show(ui);
                assert!(out.save.is_none() && !out.cancelled);
            })
            .textures_delta
            .clear();
        }
        assert!(p.tex.iter().all(Option::is_some));
        p.layout = LayoutMode::Single;
        p.single = RefinePlane::Sagittal;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            let _ = p.show(ui);
        })
        .textures_delta
        .clear();
    }

    #[test]
    fn untouched_directed_side_is_not_edited_after_a_frame() {
        let (req, vol) = request(ras_affine());
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        assert!(!p.sides[0].changed());
        let ctx = egui::Context::default();
        for frame in 0..3 {
            ctx.run_ui(frame_input(frame), |ui| {
                let _ = p.show(ui);
            })
            .textures_delta
            .clear();
        }
        assert!(p.sides.iter().all(|s| !s.changed()));
        assert!(!p.dirty());
        assert!(p.save_edits().is_empty());
        assert!(p.sides[0].pose.roll.is_none());
        let tab = {
            let s = &p.sides[0];
            let mut t = s.label.clone();
            if s.changed() {
                t.push_str(" · edited");
            }
            t
        };
        assert!(!tab.contains("edited"));
    }

    #[test]
    fn roll_equal_to_the_kept_roll_is_not_a_change() {
        let mut p = panel(ras_affine());
        let r0 = p.current_roll_deg();
        p.sides[0].pose.roll = Some(r0 + 1e-12);
        assert!(!p.sides[0].roll_changed());
        assert!(!p.sides[0].changed());
        p.sides[0].pose.head[0] += 1e-12;
        assert!(!p.sides[0].markers_changed());
    }

    #[test]
    fn shortcut_rows_are_exported() {
        assert!(!RefinePanel::shortcut_rows().is_empty());
        assert_eq!(shortcut_rows().len(), RefinePanel::shortcut_rows().len());
    }

    #[test]
    fn status_line_reports_shifts_roll_and_model() {
        let mut p = panel(ras_affine());
        p.nudge(
            RefinePlane::Axial,
            PaneAxis::Horizontal,
            1,
            StepSize::Large,
            false,
        );
        let text = p.status_text();
        assert!(text.contains("head shift 1.00 mm"));
        assert!(text.contains("tail shift 0.00 mm"));
        assert!(text.contains(MODEL));
        assert!(text.contains("tilt from vertical"));
    }

    fn iso_affine(step: f64, half: f64) -> Affine {
        [
            [step, 0.0, 0.0, -half],
            [0.0, step, 0.0, -half],
            [0.0, 0.0, step, -half],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    const CYL_INTENSITY: f32 = 3000.0;

    fn cylinder_volume(point: V3, dir: V3, radius: f64) -> RefineVolume {
        let n = 96usize;
        let step = 0.5;
        let half = 0.5 * step * (n - 1) as f64;
        let affine = iso_affine(step, half);
        let mut data = vec![0.0f32; n * n * n];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let w = [
                        i as f64 * step - half,
                        j as f64 * step - half,
                        k as f64 * step - half,
                    ];
                    let d = sub(w, point);
                    let across = sub(d, scale(dir, dot(d, dir)));
                    if norm(across) <= radius {
                        data[(k * n + j) * n + i] = CYL_INTENSITY;
                    }
                }
            }
        }
        RefineVolume::from_parts([n, n, n], affine, data).unwrap()
    }

    fn one_side_request(model: &str, head: V3, tail: V3) -> RefineRequest {
        let u = unit(sub(tail, head)).unwrap();
        let y = roll_y(u, 0.0);
        let x = cross(y, u);
        let r = 0.5 * electrode_by_name(model).unwrap().lead_diameter_mm;
        RefineRequest {
            subj_id: "sub-test".to_owned(),
            space: EditSpace::Native,
            backdrop_nii: PathBuf::from("unused.nii.gz"),
            sides: vec![RefineSide {
                key: "1".to_owned(),
                label: "Right lead".to_owned(),
                elmodel: Some(model.to_owned()),
                head,
                tail,
                x: Some(add(head, scale(x, r))),
                y: Some(add(head, scale(y, r))),
                roll_deg: None,
                candidates_deg: None,
                orientation_status: None,
                manually_corrected: false,
                review: None,
            }],
        }
    }

    fn tilted_lead(model: &str, tilt_deg: f64) -> (V3, V3, V3) {
        let (s, c) = tilt_deg.to_radians().sin_cos();
        let dir = [s, 0.0, c];
        let head = [-5.0, 0.0, -12.0];
        let span = electrode_by_name(model).unwrap().contact_span_mm();
        (head, add(head, scale(dir, span)), dir)
    }

    fn show_once(p: &mut RefinePanel, size: egui::Vec2) -> usize {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size)),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = p.show(ui);
        });
        out.textures_delta.clear();
        out.shapes.len()
    }

    fn column_mean(raw: &[f32], w: usize, col: usize, rows: std::ops::Range<usize>) -> f32 {
        let n = rows.len().max(1) as f32;
        rows.map(|r| raw[r * w + col]).sum::<f32>() / n
    }

    #[test]
    fn lead_panes_show_an_oblique_cylinder_as_a_straight_band() {
        let (head, tail, dir) = tilted_lead(RING_MODEL, 30.0);
        let vol = cylinder_volume(head, dir, 0.7);
        let req = one_side_request(RING_MODEL, head, tail);
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        assert_eq!(p.layout, LayoutMode::Lead);
        show_once(&mut p, egui::vec2(1400.0, 800.0));
        let span = norm(sub(tail, head));
        for idx in 0..2 {
            let pane = &p.oblique[idx];
            let (frame, slab) = pane.sampled.unwrap();
            assert!(!slab);
            assert!(close(frame.up, dir, 1e-12));
            let [w, h] = pane.size;
            assert_eq!(w, 100);
            let (u0, _) = frame.u_range;
            let v1 = frame.v_range.1;
            let du = (frame.u_range.1 - u0) / w as f64;
            let dv = (v1 - frame.v_range.0) / h as f64;
            let row_of = |v: f64| ((v1 - v) / dv) as usize;
            let rows = row_of(span)..row_of(0.0);
            assert!(rows.len() > 20);
            let col_of = |u: f64| ((u - u0) / du) as usize;
            for col in [col_of(-0.05), col_of(0.05)] {
                let m = column_mean(&pane.raw, w, col, rows.clone());
                assert!(m > 0.8 * CYL_INTENSITY, "pane {idx} col {col}: {m}");
            }
            for u in [-6.0, -3.0, 3.0, 6.0] {
                let m = column_mean(&pane.raw, w, col_of(u), rows.clone());
                assert!(m < 0.02 * CYL_INTENSITY, "pane {idx} u {u}: {m}");
            }
            let g = p.sides[0].geom.as_ref().unwrap();
            for c in &g.coords_mm {
                let (u, v, d) = frame.project(*c);
                assert!(u.abs() < 0.2 && d.abs() < 0.2, "contact off band: {u} {d}");
                assert!(v > -0.2 && v < span + 0.2);
            }
        }
        let gen_before = p.oblique[0].generation;
        p.keep_length = false;
        assert!(p.nudge(
            PaneKind::Lead(LeadView::Front),
            PaneAxis::Horizontal,
            4,
            StepSize::Large,
            false
        ));
        show_once(&mut p, egui::vec2(1400.0, 800.0));
        let pose = p.sides[0].pose;
        for idx in 0..2 {
            let view = [LeadView::Front, LeadView::Left][idx];
            let expected = lead_frame(view, pose.head, pose.tail, p.sides[0].electrode).unwrap();
            assert_eq!(p.oblique[idx].sampled, Some((expected, false)));
        }
        assert_eq!(p.oblique[0].generation, gen_before + 1);
        let gen = p.oblique[0].generation;
        show_once(&mut p, egui::vec2(1400.0, 800.0));
        assert_eq!(
            p.oblique[0].generation, gen,
            "no resampling without a change"
        );
    }

    #[test]
    fn lead_frames_follow_the_reference_views() {
        let (head, tail, _) = tilted_lead(MODEL, 20.0);
        let e = electrode_by_name(MODEL).unwrap();
        let front = lead_frame(LeadView::Front, head, tail, e).unwrap();
        assert!(front.right[0] < -0.9);
        assert!(front.normal[1] > 0.99);
        assert_eq!(caption(front.right), "L");
        let left = lead_frame(LeadView::Left, head, tail, e).unwrap();
        assert!(left.right[1] < -0.99);
        assert!(left.normal[0] < -0.9);
        assert_eq!(caption(left.right), "P");
        let span = norm(sub(tail, head));
        assert!((front.v_range.0 + 5.0).abs() < 1e-12);
        let k = if e.n_contacts > 8 { 1.5 } else { 2.0 };
        assert!((front.v_range.1 - span - k * e.contact_span_mm()).abs() < 1e-12);
        assert_eq!(front.u_range, (-10.0, 10.0));
    }

    #[test]
    fn keep_lead_length_is_the_default_and_survives_random_nudges() {
        let (req, vol) = request(ras_affine());
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        assert!(p.keep_length);
        let other_side = p.sides[1].pose;
        let (h0, t0) = p.current_markers();
        let len = norm(sub(t0, h0));
        let panes = [
            PaneKind::Ortho(RefinePlane::Axial),
            PaneKind::Ortho(RefinePlane::Coronal),
            PaneKind::Ortho(RefinePlane::Sagittal),
            PaneKind::Lead(LeadView::Front),
            PaneKind::Lead(LeadView::Left),
            PaneKind::Close(Marker::Head),
        ];
        let axes = [PaneAxis::Horizontal, PaneAxis::Vertical, PaneAxis::Through];
        let sizes = [StepSize::Normal, StepSize::Large, StepSize::Fine];
        let mut seed: u64 = 0x5eed;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % n
        };
        let mut moved = 0;
        for _ in 0..20 {
            let pane = panes[next(panes.len())];
            let axis = axes[next(axes.len())];
            let size = sizes[next(sizes.len())];
            let sign = if next(2) == 0 { -1 } else { 1 };
            let (h, t) = p.current_markers();
            let (right, up, through) = p.pane_dirs(pane).unwrap();
            let dir = match axis {
                PaneAxis::Horizontal => right,
                PaneAxis::Vertical => up,
                PaneAxis::Through => through,
            };
            let requested = add(h, scale(dir, size.mm() * f64::from(sign)));
            let a_hat = unit(sub(h, t)).unwrap();
            let across = |q: V3| {
                let d = sub(q, t);
                sub(d, scale(a_hat, dot(d, a_hat)))
            };
            if p.nudge(pane, axis, sign, size, false) {
                moved += 1;
                let (h1, t1) = p.current_markers();
                assert_eq!(t1, t);
                assert!(close(across(h1), across(requested), 1e-9));
            }
            let (h1, t1) = p.current_markers();
            assert!((norm(sub(t1, h1)) - len).abs() < 1e-9);
        }
        assert!(moved >= 10, "{moved}");
        assert_eq!(p.sides[1].pose, other_side);
    }

    #[test]
    fn keep_lead_length_refuses_a_pure_along_axis_move_and_clamps() {
        let (head, tail, _) = tilted_lead(MODEL, 30.0);
        let req = one_side_request(MODEL, head, tail);
        let vol = volume(iso_affine(1.0, 15.0));
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        assert!(!p.nudge(
            PaneKind::Lead(LeadView::Front),
            PaneAxis::Vertical,
            1,
            StepSize::Large,
            false
        ));
        assert_eq!(p.status.as_deref(), Some(KEEP_LENGTH_ALONG));
        assert_eq!(p.current_markers(), (head, tail));
        let len = norm(sub(tail, head));
        let (pose, note) =
            p.marker_move(p.sides[0].pose, Marker::Head, add(head, [100.0, 0.0, 0.0]));
        assert_eq!(note, Some(KEEP_LENGTH_CLAMPED));
        assert!((norm(sub(pose.tail, pose.head)) - len).abs() < 1e-9);
    }

    #[test]
    fn along_axis_move_shifts_both_markers_and_keeps_the_roll() {
        let mut p = panel(ras_affine());
        p.set_roll(33.0);
        let r0 = p.current_roll_deg();
        let (h0, t0) = p.current_markers();
        let u = unit(sub(t0, h0)).unwrap();
        assert!(p.move_along_axis(1, StepSize::Normal));
        let (h1, t1) = p.current_markers();
        let dh = sub(h1, h0);
        assert!(close(dh, sub(t1, t0), 1e-12));
        assert!((norm(dh) - STEP_NORMAL_MM).abs() < 1e-9);
        assert!(norm(sub(dh, scale(u, dot(dh, u)))) < 1e-12);
        assert!(dot(dh, u) > 0.0);
        assert!((p.current_roll_deg() - r0).abs() < 1e-6);
        let alt = |key, shift, ctrl| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                alt: true,
                shift,
                ctrl,
                command: ctrl,
                ..Default::default()
            },
        };
        run_keys(&mut p, vec![alt(egui::Key::PageDown, true, false)]);
        let (h2, t2) = p.current_markers();
        assert!(close(sub(h2, h1), scale(u, -STEP_LARGE_MM), 1e-9));
        assert!(close(sub(t2, t1), scale(u, -STEP_LARGE_MM), 1e-9));
        run_keys(&mut p, vec![alt(egui::Key::PageUp, false, true)]);
        let (h3, _) = p.current_markers();
        assert!(close(sub(h3, h2), scale(u, STEP_FINE_MM), 1e-9));
        assert!((p.current_roll_deg() - r0).abs() < 1e-6);
        let (_, untouched) = (p.sides[1].pose.head, p.sides[1].pose.tail);
        assert_eq!(untouched, p.sides[1].file_tail);
    }

    #[test]
    fn spacing_readout_tracks_the_head_tail_distance() {
        let (head, tail, dir) = tilted_lead(MODEL, 30.0);
        let mut req = one_side_request(MODEL, head, tail);
        let (h2, t2) = ([10.0, 2.0, -3.0], [11.0, 3.0, 6.0]);
        req.sides.push(RefineSide {
            key: "2".to_owned(),
            label: "Left lead".to_owned(),
            ..one_side_request(MODEL, h2, t2).sides[0].clone()
        });
        let vol = volume(iso_affine(1.0, 15.0));
        let mut p = RefinePanel::with_volume(req, vol).unwrap();
        p.keep_length = false;
        let e = electrode_by_name(MODEL).unwrap();
        let (live, cat) = p.spacing_mm().unwrap();
        assert!((cat - e.contact_span_mm() / (e.n_levels() - 1) as f64).abs() < 1e-12);
        assert!((live - cat).abs() < 0.01, "{live} {cat}");
        let text = p.status_text();
        assert!(text.contains(&format!(
            "Spacing: {live:.2} mm (catalogue {cat:.2} mm, ratio"
        )));
        let span = norm(sub(tail, head));
        assert!(p.move_marker_to(Marker::Tail, add(head, scale(dir, 1.1 * span))));
        let (live2, _) = p.spacing_mm().unwrap();
        assert!((live2 / live - 1.1).abs() < 1e-6, "{live2} {live}");
        assert_eq!(p.sides[1].pose.head, h2);
        assert_eq!(p.sides[1].pose.tail, t2);
    }

    #[test]
    fn roll_follows_the_azimuth_convention() {
        let vol = || volume(iso_affine(1.0, 15.0));
        let vertical = ([0.0, 0.0, -5.0], [0.0, 0.0, 5.0]);
        let mut p =
            RefinePanel::with_volume(one_side_request(MODEL, vertical.0, vertical.1), vol())
                .unwrap();
        assert!(p.set_roll(37.0));
        let s = &p.sides[0];
        let u = s.axis_unit().unwrap();
        assert!((p.current_roll_deg() - 37.0).abs() < 1e-6);
        assert!((roll_from_anterior_deg(u, s.y_dir) - 37.0).abs() < 1e-6);
        assert!((azimuth_of(s.y_dir).unwrap() - 37.0).abs() < 1e-6);
        assert!(close(s.y_dir, rotate_about_axis(a_ref(u), u, 37.0), 1e-12));

        let (head, tail, _) = tilted_lead(MODEL, 30.0);
        let mut q = RefinePanel::with_volume(one_side_request(MODEL, head, tail), vol()).unwrap();
        assert!(q.set_roll(45.0));
        let s = &q.sides[0];
        let u = s.axis_unit().unwrap();
        assert!((q.current_roll_deg() - 45.0).abs() < 1e-6);
        assert!((azimuth_of(s.y_dir).unwrap() - 45.0).abs() < 1e-6);
        assert!(dot(s.y_dir, u).abs() < 1e-12);
        let axis_convention = roll_from_anterior_deg(u, s.y_dir);
        let diff = (axis_convention - 45.0).abs();
        assert!((diff - 4.1).abs() <= 0.1, "{axis_convention}");
        let saved = q.save_edits();
        assert_eq!(saved[0].roll_deg, Some(45.0));
    }

    fn rotate_about_axis(v: V3, axis: V3, deg: f64) -> V3 {
        let (s, c) = deg.to_radians().sin_cos();
        add(
            add(scale(v, c), scale(cross(axis, v), s)),
            scale(axis, dot(axis, v) * (1.0 - c)),
        )
    }

    #[test]
    fn slab_mode_changes_only_the_display() {
        let (head, tail, dir) = tilted_lead(RING_MODEL, 30.0);
        let vol = cylinder_volume(head, dir, 0.7);
        let mut p =
            RefinePanel::with_volume(one_side_request(RING_MODEL, head, tail), vol).unwrap();
        p.keep_length = false;
        p.nudge(
            PaneKind::Close(Marker::Head),
            PaneAxis::Horizontal,
            1,
            StepSize::Normal,
            false,
        );
        show_once(&mut p, egui::vec2(1400.0, 800.0));
        let before = serde_json::to_string(&p.save_edits()).unwrap();
        let pose = p.sides[0].pose;
        let raw_before = p.oblique[0].raw.clone();
        let close_gen = p.oblique[2].generation;
        run_keys(&mut p, vec![key_event(egui::Key::X, false, false)]);
        assert!(p.slab);
        show_once(&mut p, egui::vec2(1400.0, 800.0));
        assert_eq!(p.oblique[0].sampled.map(|s| s.1), Some(true));
        assert_ne!(p.oblique[0].raw, raw_before);
        assert_eq!(p.oblique[2].generation, close_gen);
        assert_eq!(serde_json::to_string(&p.save_edits()).unwrap(), before);
        let after = p.sides[0].pose;
        for (a, b) in [(pose.head, after.head), (pose.tail, after.tail)] {
            for i in 0..3 {
                assert_eq!(a[i].to_bits(), b[i].to_bits());
            }
        }
        assert_eq!(pose.roll.map(f64::to_bits), after.roll.map(f64::to_bits));
    }

    #[test]
    fn hidden_markers_are_not_drawn_but_keys_still_act() {
        let mut p = panel(ras_affine());
        let shown = show_once(&mut p, egui::vec2(1400.0, 800.0));
        run_keys(&mut p, vec![key_event(egui::Key::V, false, false)]);
        assert!(p.hide_fiducials);
        let hidden = show_once(&mut p, egui::vec2(1400.0, 800.0));
        assert!(hidden + 20 < shown, "{hidden} vs {shown}");
        let (h0, _) = p.current_markers();
        p.last_pane = PaneKind::Close(Marker::Head);
        run_keys(&mut p, vec![key_event(egui::Key::ArrowRight, false, false)]);
        let (h1, _) = p.current_markers();
        assert!(close(sub(h1, h0), [0.25, 0.0, 0.0], 1e-12));
        let head_s = egui::pos2(100.0, 100.0);
        assert!(matches!(
            p.hit_mode(head_s, head_s, egui::pos2(300.0, 300.0), None),
            DragMode::Crosshair
        ));
        p.layout = LayoutMode::Triple;
        let tri_hidden = show_once(&mut p, egui::vec2(1400.0, 800.0));
        p.hide_fiducials = false;
        let tri_shown = show_once(&mut p, egui::vec2(1400.0, 800.0));
        assert!(tri_hidden < tri_shown);
    }

    #[test]
    fn move_whole_lead_from_the_numeric_fields() {
        let mut p = panel(ras_affine());
        let (h0, t0) = p.current_markers();
        let target = [1.5, -2.25, 0.125];
        assert!(p.translate_lead_to(Marker::Head, target, false));
        let (h1, t1) = p.current_markers();
        assert_eq!(h1, target);
        assert!(close(sub(t1, t0), sub(target, h0), 1e-12));
        assert!(p.translate_lead_to(Marker::Tail, t0, false));
        let (h2, t2) = p.current_markers();
        assert_eq!(t2, t0);
        assert!(close(h2, h0, 1e-12));
    }

    #[test]
    fn lead_layout_fits_a_small_window_without_scrolling() {
        let mut p = panel(ras_affine());
        let ctx = egui::Context::default();
        crate::brand::install_fonts(&ctx);
        crate::brand::apply_theme(&ctx);
        let run = |p: &mut RefinePanel, h: f32| {
            for frame in 0..3 {
                let raw = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(1260.0, h),
                    )),
                    time: Some(f64::from(frame) * 0.016),
                    ..Default::default()
                };
                ctx.run_ui(raw, |ui| {
                    let _ = p.show(ui);
                })
                .textures_delta
                .clear();
            }
        };
        run(&mut p, 650.0);
        assert!(!p.controls_overflow);
        assert!(p.oblique.iter().all(|o| o.tex.is_some()));
        run(&mut p, 300.0);
        assert!(p.controls_overflow);
    }

    #[test]
    fn layout_and_view_keys() {
        let mut p = panel(ras_affine());
        run_keys(&mut p, vec![key_event(egui::Key::Num0, false, false)]);
        assert_eq!(p.layout, LayoutMode::Triple);
        run_keys(&mut p, vec![key_event(egui::Key::L, false, false)]);
        assert_eq!(p.layout, LayoutMode::Lead);
        run_keys(&mut p, vec![key_event(egui::Key::K, false, false)]);
        assert!(p.keep_length);
        run_keys(&mut p, vec![key_event(egui::Key::K, false, false)]);
        assert!(!p.keep_length);
        let rows = shortcut_rows();
        for key in ["L", "X", "V", "K", "Alt + Page Up / Page Down"] {
            assert!(rows.iter().any(|(k, _)| *k == key), "{key}");
        }
    }

    #[test]
    #[ignore = "timing on a 512^3 volume (512 MB); run with --ignored --release"]
    fn lead_resampling_time_on_a_512_cube() {
        let n = 512usize;
        let affine = iso_affine(0.5, 128.0);
        let data: Vec<f32> = (0..n * n * n).map(|i| (i % 4093) as f32).collect();
        let vol = RefineVolume::from_parts([n, n, n], affine, data).unwrap();
        let (head, tail, _) = tilted_lead(MODEL, 30.0);
        let mut p = RefinePanel::with_volume(one_side_request(MODEL, head, tail), vol).unwrap();
        p.keep_length = false;
        let mut worst = [0f64; 2];
        for (k, slab) in [false, true].into_iter().enumerate() {
            p.slab = slab;
            for step in 0..10 {
                p.nudge(
                    PaneKind::Close(Marker::Head),
                    PaneAxis::Horizontal,
                    if step % 2 == 0 { 1 } else { -1 },
                    StepSize::Normal,
                    false,
                );
                let t = std::time::Instant::now();
                for idx in 0..4 {
                    p.resample_oblique(idx);
                }
                worst[k] = worst[k].max(t.elapsed().as_secs_f64() * 1e3);
            }
        }
        println!(
            "four panes resampled: worst {:.2} ms plain, {:.2} ms slab",
            worst[0], worst[1]
        );
        let bound_ms = if cfg!(debug_assertions) { 150.0 } else { 30.0 };
        assert!(worst[0] < bound_ms, "{:.2} ms >= {bound_ms} ms", worst[0]);
    }
}
