// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use dbsloc_reconstruct::native_frame::resolve_fiducials;
use dbsloc_reconstruct::orientation::{
    roll_azimuth_deg, template_marker_directions, FLAG_VERTICAL_MARKER,
};
use dbsloc_reconstruct::tail_fiducial::TailConversion;
use dbsloc_reconstruct::{
    add3, check_side, check_tilt, cross3, electrode_by_name, norm3, scale3, sub3, unit3,
    ElectrodeGeometry, PlausibilityLimits, MAX_TILT_FROM_VERTICAL_DEG,
};
use fastwarp_core::dot3;

use crate::bids::Subject;
use crate::ct_recon::{side_json, warp_sides, SIDE_KEYS};
use crate::derivatives;
use crate::error::{PipelineError, PipelineResult};
use crate::locks::{
    approval_inputs_still_valid_with, default_lock_input_paths, snap_input_paths,
    stage_approval_path, write_approval, write_approval_with, HashMemo,
};
use crate::options::RunOptions;
use crate::orientation_stage::{rebuild_template_markers, Rebuilt, SideUpdate};
use crate::outputs::write_replacing;
use crate::point_warp::PointWarp;
use crate::protocol::RunEvent;
use crate::stage_impl::{opts_with_prefs_model, subject_point_warper, with_space_and_reference};
use crate::stages::Stage;
use crate::template_contacts::{recompute_template_contacts, WarpedContacts};

const MIN_SPAN_MM: f64 = 1e-6;
const METHOD: &str = "manual_correction";
pub const REVIEW_CORRECTED: &str = "corrected";
pub const REVIEW_ACCEPTED: &str = "accepted";
pub const ROLL_SET: &str = "set";
pub const ROLL_ACCEPTED: &str = "accepted";
const DETERMINED_ROLL_STATUSES: [&str; 3] = ["resolved", "ambiguous", "manual"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditSpace {
    Native,
    Mni,
}

impl EditSpace {
    pub fn as_str(self) -> &'static str {
        match self {
            EditSpace::Native => "native",
            EditSpace::Mni => "mni",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextSide {
    pub key: String,
    pub label: String,
    pub elmodel: Option<String>,
    pub head: [f64; 3],
    pub tail: [f64; 3],
    pub x: Option<[f64; 3]>,
    pub y: Option<[f64; 3]>,
    pub roll_deg: Option<f64>,
    pub candidates_deg: Option<[f64; 2]>,
    pub orientation_status: Option<String>,
    pub manually_corrected: bool,
    #[serde(default)]
    pub review: Option<String>,
    #[serde(default)]
    pub roll_review: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManualCorrectionContext {
    pub subj_id: String,
    pub space: EditSpace,
    pub backdrop_nii: PathBuf,
    pub sides: Vec<ContextSide>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SideEdit {
    pub key: String,
    pub head: [f64; 3],
    pub tail: [f64; 3],
    pub roll_deg: Option<f64>,
    #[serde(default)]
    pub accept: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorrectionRequest {
    pub space: EditSpace,
    pub source: String,
    pub sides: Vec<SideEdit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SideReport {
    pub key: String,
    pub space: EditSpace,
    pub review: String,
    pub head_shift_mm: f64,
    pub tail_shift_mm: f64,
    pub roll_deg: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorrectionReport {
    pub path: PathBuf,
    pub sides: Vec<SideReport>,
    pub warnings: Vec<String>,
    pub invalidated: Vec<String>,
    #[serde(default)]
    pub approved: Vec<String>,
}

fn fail(reason: impl std::fmt::Display) -> PipelineError {
    PipelineError::StageFailed {
        stage: "reconstruct".into(),
        message: format!("E_RECON_manual: {reason}"),
    }
}

fn warp_fail(e: impl std::fmt::Display) -> PipelineError {
    let text = e.to_string();
    let message = if text.starts_with("E_RECON_warp") {
        text
    } else {
        format!("E_RECON_warp: {text}")
    };
    PipelineError::StageFailed {
        stage: "reconstruct".into(),
        message,
    }
}

fn as_xyz(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array()?;
    if a.len() < 3 {
        return None;
    }
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

fn finite3(p: [f64; 3]) -> bool {
    p.iter().all(|v| v.is_finite())
}

fn side_label(key: &str) -> &'static str {
    if key == "1" {
        "Right lead"
    } else {
        "Left lead"
    }
}

fn load_reco(sub: &Subject) -> PipelineResult<(PathBuf, Value)> {
    load_reco_converted(sub).map(|(path, reco, _)| (path, reco))
}

fn load_reco_converted(sub: &Subject) -> PipelineResult<(PathBuf, Value, Vec<TailConversion>)> {
    let path = sub.reconstruction_json();
    if !path.is_file() {
        return Err(fail(format!(
            "no reconstruction file at {}",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| fail(format!("cannot read {}: {e}", path.display())))?;
    let mut reco: Value = serde_json::from_str(&text)
        .map_err(|e| fail(format!("cannot parse {}: {e}", path.display())))?;
    if reco.get("version").and_then(Value::as_i64) != Some(2) {
        return Err(fail(format!(
            "{} is not a Reco v2 file (version 2 required)",
            path.display()
        )));
    }
    let conversions = dbsloc_reconstruct::tail_fiducial::convert_legacy_tail(&mut reco, None);
    Ok((path, reco, conversions))
}

fn has_sides(block: Option<&Value>) -> bool {
    block.and_then(Value::as_object).is_some_and(|b| {
        SIDE_KEYS
            .iter()
            .any(|k| b.get(*k).is_some_and(Value::is_object))
    })
}

fn file_space(reco: &Value) -> PipelineResult<EditSpace> {
    if has_sides(reco.get("native")) {
        Ok(EditSpace::Native)
    } else if has_sides(reco.get("mni")) {
        Ok(EditSpace::Mni)
    } else {
        Err(fail("the reconstruction file has no lead sides"))
    }
}

fn block_name(space: EditSpace) -> &'static str {
    match space {
        EditSpace::Native => "native",
        EditSpace::Mni => "mni",
    }
}

pub fn file_edit_space(sub: &Subject) -> PipelineResult<EditSpace> {
    let (_, reco) = load_reco(sub)?;
    file_space(&reco)
}

fn backdrop_for(sub: &Subject, space: EditSpace) -> PipelineResult<PathBuf> {
    match space {
        EditSpace::Native => Ok(derivatives::coreg_ct_image(sub)),
        EditSpace::Mni => sub
            .norm_postop_mr_in_use()
            .or_else(|| {
                let space = sub.norm.template_space.as_deref()?;
                let (key, _) = sub.postop_mr_images().into_iter().next()?;
                Some(derivatives::norm_postop_image(sub, key, space))
            })
            .ok_or_else(|| fail("no normalized post-op MR image to show the template-space edit")),
    }
}

fn model_name(reco: &Value, key: &str, opts: &RunOptions) -> Option<String> {
    reco["props"][key]["elmodel"]
        .as_str()
        .or_else(|| reco["meta"]["elmodel"].as_str())
        .or(opts.el_model.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn candidates_of(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array()?;
    if a.len() != 2 {
        return None;
    }
    Some([a[0].as_f64()?, a[1].as_f64()?])
}

pub fn prepare_manual_correction(
    sub: &Subject,
    opts: &RunOptions,
) -> PipelineResult<ManualCorrectionContext> {
    let ctx = context(sub, opts, true)?;
    if !ctx.backdrop_nii.is_file() {
        return Err(fail(format!(
            "backdrop image missing: {}",
            ctx.backdrop_nii.display()
        )));
    }
    Ok(ctx)
}

pub fn inspect_manual_correction(
    sub: &Subject,
    opts: &RunOptions,
) -> PipelineResult<ManualCorrectionContext> {
    context(sub, opts, false)
}

fn context(
    sub: &Subject,
    opts: &RunOptions,
    need_backdrop: bool,
) -> PipelineResult<ManualCorrectionContext> {
    let prefs = opts_with_prefs_model(sub, opts, None);
    let opts = prefs.as_ref().unwrap_or(opts);
    let (_, reco, conversions) = load_reco_converted(sub)?;
    let warnings = conversions
        .iter()
        .map(|c| {
            format!(
                "{}: the tail marker was moved from the last to the 4th contact level, because the file was written before 2026-11-14. Save to keep the corrected marker.",
                side_label(&c.side)
            )
        })
        .collect();
    let space = file_space(&reco)?;
    let backdrop_nii = match backdrop_for(sub, space) {
        Ok(p) => p,
        Err(e) if need_backdrop => return Err(e),
        Err(_) => PathBuf::new(),
    };
    let block = &reco[block_name(space)];
    let mut sides = Vec::new();
    for key in SIDE_KEYS {
        let Some(side) = block.get(key).filter(|s| s.is_object()) else {
            continue;
        };
        let markers = &side["markers"];
        let (Some(head), Some(tail)) = (as_xyz(&markers["head"]), as_xyz(&markers["tail"])) else {
            return Err(fail(format!(
                "side {key} of the {} block has no head / tail markers",
                block_name(space)
            )));
        };
        let orientation = reco["meta"]["orientation"]
            .get(key)
            .filter(|o| o.is_object());
        let roll_deg = orientation
            .and_then(|o| o["roll_deg"].as_f64())
            .map(|deg| previous_roll(markers).unwrap_or(deg));
        let candidates_deg = orientation.and_then(|o| candidates_of(&o["candidates_deg"]));
        sides.push(ContextSide {
            key: key.to_owned(),
            label: side_label(key).to_owned(),
            elmodel: model_name(&reco, key, opts),
            head,
            tail,
            x: as_xyz(&markers["x"]),
            y: as_xyz(&markers["y"]),
            roll_deg,
            candidates_deg,
            orientation_status: orientation.and_then(|o| o["status"].as_str().map(str::to_owned)),
            manually_corrected: reco["props"][key]["manually_corrected"]
                .as_bool()
                .unwrap_or(false),
            review: side_review(&reco, key).map(str::to_owned),
            roll_review: roll_review(&reco, key).map(str::to_owned),
        });
    }
    Ok(ManualCorrectionContext {
        subj_id: sub.subj_id.clone(),
        space,
        backdrop_nii,
        sides,
        warnings,
    })
}

fn roll_reference(u: [f64; 3]) -> [f64; 3] {
    let anterior = [0.0, 1.0, 0.0];
    let mut reference = sub3(anterior, scale3(u, dot3(anterior, u)));
    if norm3(reference) < 1e-6 {
        reference = sub3([1.0, 0.0, 0.0], scale3(u, u[0]));
    }
    unit3(reference)
}

fn rotate_about(v: [f64; 3], axis: [f64; 3], deg: f64) -> [f64; 3] {
    let (s, c) = deg.to_radians().sin_cos();
    add3(
        add3(scale3(v, c), scale3(cross3(axis, v), s)),
        scale3(axis, dot3(axis, v) * (1.0 - c)),
    )
}

fn roll_frame(head: [f64; 3], tail: [f64; 3], roll_deg: f64) -> ([f64; 3], [f64; 3]) {
    let (s, c) = roll_deg.to_radians().sin_cos();
    template_marker_directions([-s, c, 0.0], head, tail).unwrap_or_else(|| {
        let u = unit3(sub3(tail, head));
        let y_dir = unit3(rotate_about(roll_reference(u), u, roll_deg));
        (cross3(y_dir, u), y_dir)
    })
}

fn previous_roll(markers: &Value) -> Option<f64> {
    let (y, head) = as_xyz(&markers["y"]).zip(as_xyz(&markers["head"]))?;
    roll_azimuth_deg(sub3(y, head))
}

struct Edited {
    key: &'static str,
    geometry: dbsloc_reconstruct::native_frame::SideGeometry,
    y_dir: [f64; 3],
    radius_mm: f64,
    roll_given: Option<f64>,
    previous: Value,
    head_shift_mm: f64,
    tail_shift_mm: f64,
    warnings: Vec<String>,
}

fn compute_side(
    edit: &SideEdit,
    key: &'static str,
    markers: &Value,
    electrode: &ElectrodeGeometry,
) -> PipelineResult<Edited> {
    if !finite3(edit.head) || !finite3(edit.tail) {
        return Err(fail(format!("side {key}: head and tail must be finite")));
    }
    if edit.roll_deg.is_some_and(|r| !r.is_finite()) {
        return Err(fail(format!("side {key}: roll must be finite")));
    }
    let shaft = sub3(edit.tail, edit.head);
    if norm3(shaft) < MIN_SPAN_MM {
        return Err(fail("head and tail coincide"));
    }
    let roll_deg = edit
        .roll_deg
        .or_else(|| previous_roll(markers))
        .unwrap_or(0.0);
    let (x_dir, y_dir) = roll_frame(edit.head, edit.tail, roll_deg);
    let radius_mm = 0.5 * electrode.lead_diameter_mm;
    let fids = [
        edit.head,
        edit.tail,
        add3(edit.head, scale3(x_dir, radius_mm)),
        add3(edit.head, scale3(y_dir, radius_mm)),
    ];
    let geometry =
        resolve_fiducials(fids, electrode).map_err(|e| fail(format!("side {key}: {e}")))?;
    let shift = |name: &str, now: [f64; 3]| {
        as_xyz(&markers[name]).map_or(0.0, |before| norm3(sub3(now, before)))
    };
    let previous = json!({
        "head": markers.get("head").cloned().unwrap_or(Value::Null),
        "tail": markers.get("tail").cloned().unwrap_or(Value::Null),
        "x": markers.get("x").cloned().unwrap_or(Value::Null),
        "y": markers.get("y").cloned().unwrap_or(Value::Null),
    });
    let label = side_label(key);
    let mut warnings = Vec::new();
    if let Some(p) = check_tilt(geometry.head, geometry.tail, MAX_TILT_FROM_VERTICAL_DEG) {
        warnings.push(format!("{label}: {p}"));
    }
    let limits = PlausibilityLimits::for_electrode(electrode);
    let level = Some(electrode.contact_level.as_slice());
    for p in check_side(
        geometry.head,
        geometry.tail,
        &geometry.coords_mm,
        level,
        &limits,
        None,
    ) {
        warnings.push(format!("{label}: {p}"));
    }
    let y_out = unit3(sub3(geometry.y, geometry.head));
    Ok(Edited {
        key,
        head_shift_mm: shift("head", geometry.head),
        tail_shift_mm: shift("tail", geometry.tail),
        geometry,
        y_dir: y_out,
        radius_mm,
        roll_given: edit.roll_deg,
        previous,
        warnings,
    })
}

fn set_geometry(side: &mut Value, g: &dbsloc_reconstruct::native_frame::SideGeometry) {
    let fresh = side_json(g);
    let Some(target) = side.as_object_mut() else {
        return;
    };
    for field in ["coords_mm", "trajectory", "trajvector"] {
        if let Some(v) = fresh.get(field) {
            target.insert(field.into(), v.clone());
        }
    }
    let slot = target
        .entry("markers")
        .or_insert_with(|| Value::Object(Map::new()));
    if let (Some(slot), Some(new)) = (slot.as_object_mut(), fresh["markers"].as_object()) {
        for (k, v) in new {
            slot.insert(k.clone(), v.clone());
        }
    }
}

fn update_of(e: &Edited) -> SideUpdate {
    SideUpdate {
        key: e.key,
        x: e.geometry.x,
        y: e.geometry.y,
        y_dir: e.y_dir,
        radius_mm: e.radius_mm,
    }
}

fn markers_mut<'a>(block: &'a mut Value, key: &str) -> Option<&'a mut Map<String, Value>> {
    block
        .get_mut(key)
        .and_then(|s| s.get_mut("markers"))
        .and_then(Value::as_object_mut)
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

pub(crate) fn manual_orientation(roll_deg: f64) -> Value {
    json!({
        "status": "manual",
        "method": "manual",
        "roll_deg": roll_deg,
        "candidates_deg": null,
        "polar_angle_deg": null,
        "marker_slice_mm": null,
        "harmonic_amplitude": null,
        "com_offset_mm": null,
        "flags": [],
    })
}

fn flag_vertical(meta: &mut Map<String, Value>, key: &str) -> bool {
    let Some(flags) = meta
        .get_mut("orientation")
        .and_then(|o| o.get_mut(key))
        .and_then(|o| o.get_mut("flags"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    if !flags.iter().any(|f| f == FLAG_VERTICAL_MARKER) {
        flags.push(json!(FLAG_VERTICAL_MARKER));
    }
    true
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::SeqCst))
}

fn drain_logs(
    rx: &mpsc::Receiver<RunEvent>,
    log: &mut dyn FnMut(String),
    warnings: &mut Vec<String>,
) {
    for event in rx.try_iter() {
        if let RunEvent::LogLine { line } = event {
            if line.contains("warning:") {
                warnings.push(line.clone());
            }
            log(line);
        }
    }
}

type WarperFactory<'a, 'w> =
    dyn FnMut(&mut dyn FnMut(String)) -> PipelineResult<Box<dyn PointWarp + 'w>> + 'a;

pub fn apply_manual_correction(
    sub: &Subject,
    opts: &RunOptions,
    req: &CorrectionRequest,
    cancel: Option<&AtomicBool>,
    log: &mut dyn FnMut(String),
) -> PipelineResult<CorrectionReport> {
    let resolved = opts.with_resolved_template();
    let prefs = opts_with_prefs_model(sub, &resolved, None);
    let opts = prefs.as_ref().unwrap_or(&resolved);
    let mut factory = |line_log: &mut dyn FnMut(String)| {
        subject_point_warper(sub, opts, cancel, line_log).map_err(warp_fail)
    };
    apply_inner(sub, opts, req, cancel, log, &mut factory)
}

fn apply_inner<'w>(
    sub: &Subject,
    opts: &RunOptions,
    req: &CorrectionRequest,
    cancel: Option<&AtomicBool>,
    log: &mut dyn FnMut(String),
    make_warper: &mut WarperFactory<'_, 'w>,
) -> PipelineResult<CorrectionReport> {
    if req.sides.is_empty() {
        return Err(fail("no sides to correct"));
    }
    if req.source != "gui" && req.source != "cli" {
        return Err(fail(format!(
            "unknown source {:?} (gui or cli)",
            req.source
        )));
    }
    let mut seen: Vec<&'static str> = Vec::new();
    let mut keys: Vec<&'static str> = Vec::new();
    let mut accepts: Vec<&'static str> = Vec::new();
    let mut edits: Vec<&SideEdit> = Vec::new();
    for edit in &req.sides {
        let Some(key) = SIDE_KEYS.iter().copied().find(|k| *k == edit.key) else {
            return Err(fail(format!("unknown side {:?} (1 or 2)", edit.key)));
        };
        if seen.contains(&key) {
            return Err(fail(format!("side {key} given twice")));
        }
        seen.push(key);
        if edit.accept {
            accepts.push(key);
        } else {
            keys.push(key);
            edits.push(edit);
        }
    }
    let (path, reco, tail_warnings) = load_reco_converted(sub)?;
    for c in tail_warnings {
        log(c.warning);
    }
    let space = file_space(&reco)?;
    if space != req.space {
        return Err(fail(format!(
            "the reconstruction file is edited in {} space, not {}",
            space.as_str(),
            req.space.as_str()
        )));
    }
    let block = block_name(space);

    let fingerprint_paths = correction_input_paths(sub);
    let mut memo = HashMemo::default();
    for key in &accepts {
        if !reco[block].get(*key).is_some_and(Value::is_object) {
            return Err(fail(format!("side {key} is absent from the {block} block")));
        }
        match geometry_inputs(&reco, key) {
            None => {
                return Err(fail(format!(
                    "side {key}: its coordinates carry no input fingerprint (the reconstruction predates it); run reconstruct before accepting the lead"
                )))
            }
            Some(fp) if !inputs_match(fp, &fingerprint_paths, &mut memo) => {
                return Err(fail(format!(
                    "side {key}: the inputs changed since the reconstruction (coregistration or normalization re-ran); run reconstruct before accepting the lead"
                )))
            }
            Some(_) => {}
        }
    }
    let mut edited: Vec<Edited> = Vec::new();
    for (edit, key) in edits.iter().zip(&keys) {
        let Some(side) = reco[block].get(*key).filter(|s| s.is_object()) else {
            return Err(fail(format!("side {key} is absent from the {block} block")));
        };
        let name = model_name(&reco, key, opts)
            .ok_or_else(|| fail(format!("no electrode model for side {key}")))?;
        let electrode = electrode_by_name(&name).ok_or_else(|| {
            fail(format!(
                "electrode model {name:?} of side {key} is not in the electrode catalogue"
            ))
        })?;
        edited.push(compute_side(edit, key, &side["markers"], electrode)?);
    }

    let mut warnings: Vec<String> = Vec::new();
    let (mut out, vertical) = if edited.is_empty() {
        (reco.clone(), Vec::new())
    } else {
        rebuild_edited(
            &reco,
            opts,
            space,
            &edited,
            &keys,
            cancel,
            log,
            make_warper,
            &mut warnings,
        )?
    };

    let inputs = match snap_input_paths(&fingerprint_paths) {
        Ok(v) => Value::Array(v),
        Err(e) => {
            let line = format!(
                "dbsloc: warning: manual correction: the input fingerprint could not be recorded ({e}); a later reconstruct run drops this review"
            );
            warnings.push(line.clone());
            log(line);
            Value::Null
        }
    };
    let at = now_secs();
    let Some(root) = out.as_object_mut() else {
        return Err(fail("the reconstruction file is not a JSON object"));
    };
    let props = root
        .entry("props")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(props) = props.as_object_mut() {
        for e in &edited {
            let slot = props
                .entry(e.key.to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(slot) = slot.as_object_mut() {
                slot.insert("manually_corrected".into(), json!(true));
            }
        }
    }
    let meta = root
        .entry("meta")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(meta) = meta.as_object_mut() else {
        return Err(fail("the reconstruction meta is not a JSON object"));
    };
    let before = json!({"meta": Value::Object(meta.clone())});
    let mut record = match meta.get("manual_correction") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    for e in &edited {
        let roll_review = match e.roll_given {
            Some(_) => Some(ROLL_SET),
            None => roll_review(&before, e.key),
        };
        record.insert(
            e.key.to_owned(),
            json!({
                "review": REVIEW_CORRECTED,
                "roll_review": roll_review,
                "at": at,
                "source": req.source,
                "space": space.as_str(),
                "previous": e.previous,
                "head_shift_mm": e.head_shift_mm,
                "tail_shift_mm": e.tail_shift_mm,
                "roll_deg": e.roll_given,
                "inputs": inputs,
                "software": crate::version::software_brief(),
            }),
        );
    }
    for key in &accepts {
        let roll_accepted = (side_is_directed(&reco, key, opts) && roll_determined(&before, key))
            .then_some(ROLL_ACCEPTED);
        let entry = match record.get(*key).and_then(Value::as_object) {
            Some(prev) => {
                let mut prev = prev.clone();
                let review = side_review(&before, key).unwrap_or(REVIEW_CORRECTED);
                let roll = roll_review(&before, key).or(roll_accepted);
                prev.insert("review".into(), json!(review));
                prev.insert("roll_review".into(), json!(roll));
                prev.insert("software".into(), crate::version::software_brief());
                Value::Object(prev)
            }
            None => json!({
                "review": REVIEW_ACCEPTED,
                "roll_review": roll_accepted,
                "at": at,
                "source": req.source,
                "space": space.as_str(),
                "inputs": inputs,
                "software": crate::version::software_brief(),
            }),
        };
        record.insert((*key).to_owned(), entry);
    }
    meta.insert("manual_correction".into(), Value::Object(record));
    if edited.iter().any(|e| e.roll_given.is_some()) {
        let mut orientation = match meta.get("orientation") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        for e in &edited {
            if let Some(roll) = e.roll_given {
                let mut entry = manual_orientation(roll);
                entry["software"] = crate::version::software_brief();
                orientation.insert(e.key.to_owned(), entry);
            }
        }
        meta.insert("orientation".into(), Value::Object(orientation));
    }
    if space == EditSpace::Native && reco.get("mni").is_some_and(Value::is_object) {
        set_reconstruct_flags(meta, &edited, &vertical);
    }
    for key in &vertical {
        if !flag_vertical(meta, key) {
            warnings.push(format!(
                "{}: {FLAG_VERTICAL_MARKER}: the segment marker is vertical, template-space x / y kept",
                side_label(key)
            ));
        }
    }

    dbsloc_reconstruct::tail_fiducial::mark_tail_fiducial(&mut out);
    write_replacing(
        &path,
        (serde_json::to_string_pretty(&out)? + "\n").as_bytes(),
    )?;
    log(format!(
        "[reconstruct] manual correction written: {}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    crate::reco_mat::export_beside_json(&path, &out, log);

    let (invalidated, approved) = settle_locks(
        sub,
        &fingerprint_paths,
        &mut memo,
        &out,
        space,
        opts,
        !edited.is_empty(),
        log,
        &mut warnings,
    );

    let mut sides: Vec<SideReport> = edited
        .iter()
        .map(|e| SideReport {
            key: e.key.to_owned(),
            space,
            review: REVIEW_CORRECTED.to_owned(),
            head_shift_mm: e.head_shift_mm,
            tail_shift_mm: e.tail_shift_mm,
            roll_deg: e.roll_given,
        })
        .collect();
    sides.extend(accepts.iter().map(|key| SideReport {
        key: (*key).to_owned(),
        space,
        review: REVIEW_ACCEPTED.to_owned(),
        head_shift_mm: 0.0,
        tail_shift_mm: 0.0,
        roll_deg: None,
    }));
    Ok(CorrectionReport {
        path,
        sides,
        warnings,
        invalidated,
        approved,
    })
}

#[allow(clippy::too_many_arguments)]
fn rebuild_edited<'w>(
    reco: &Value,
    opts: &RunOptions,
    space: EditSpace,
    edited: &[Edited],
    keys: &[&'static str],
    cancel: Option<&AtomicBool>,
    log: &mut dyn FnMut(String),
    make_warper: &mut WarperFactory<'_, 'w>,
    warnings: &mut Vec<String>,
) -> PipelineResult<(Value, Vec<&'static str>)> {
    let block = block_name(space);
    let mut out = reco.clone();
    for e in edited {
        if let Some(side) = out.get_mut(block).and_then(|b| b.get_mut(e.key)) {
            set_geometry(side, &e.geometry);
        }
        warnings.extend(e.warnings.iter().cloned());
    }

    let mut vertical: Vec<&'static str> = Vec::new();
    if space == EditSpace::Native && out.get("mni").is_some_and(Value::is_object) {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        let mut native_sides = Map::new();
        for e in edited {
            native_sides.insert(e.key.to_owned(), side_json(&e.geometry));
        }
        let warper = make_warper(log)?;
        let warped = warp_sides(&native_sides, warper.as_ref()).map_err(warp_fail)?;
        for e in edited {
            let Some(new) = warped.get(e.key) else {
                continue;
            };
            let Some(mni) = out.get_mut("mni").and_then(Value::as_object_mut) else {
                continue;
            };
            let slot = mni
                .entry(e.key.to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            set_geometry(slot, &geometry_from(new, &e.geometry));
            let new_markers = new["markers"].as_object();
            if let (Some(markers), Some(new_markers)) =
                (markers_mut(&mut out["mni"], e.key), new_markers)
            {
                for name in ["head", "tail"] {
                    if let Some(v) = new_markers.get(name) {
                        markers.insert(name.into(), v.clone());
                    }
                }
                match rebuild_template_markers(markers, &update_of(e)) {
                    Rebuilt::Done => {}
                    Rebuilt::Vertical => vertical.push(e.key),
                    Rebuilt::NoMarkers => {
                        return Err(fail(format!(
                            "the mni markers of side {} lack head / tail",
                            e.key
                        )))
                    }
                }
            }
        }
    }

    let edited_policy = |key: &str| {
        if !keys.contains(&key) {
            WarpedContacts::Keep
        } else if space == EditSpace::Native {
            WarpedContacts::Capture
        } else {
            WarpedContacts::Omit
        }
    };
    recompute_template_contacts(
        &mut out,
        "mni",
        opts.el_model.as_deref(),
        &edited_policy,
        &mut |line| {
            warnings.push(line.clone());
            log(line);
        },
    );

    let had_reference = out.get("reference").is_some_and(Value::is_object);
    if had_reference {
        let old_reference = out["reference"].clone();
        if let Some(obj) = out.as_object_mut() {
            obj.remove("reference");
        }
        let has_template = opts
            .normalization_template
            .as_ref()
            .is_some_and(|t| t.is_file());
        if has_template {
            let (tx, rx) = mpsc::channel();
            let rebuilt = with_space_and_reference(&out, opts, cancel, Some(&tx));
            drain_logs(&rx, log, warnings);
            out = rebuilt?;
        } else {
            let line = "dbsloc: warning: reconstruct: no reference-space coordinates (no template to rebuild the reference block)".to_owned();
            warnings.push(line.clone());
            log(line);
        }
        if !out.get("reference").is_some_and(Value::is_object) {
            let known = warnings
                .iter()
                .rev()
                .find(|w| w.contains("no reference-space coordinates"))
                .cloned();
            warnings.push(match known {
                Some(w) => format!("reference-space coordinates dropped: {w}"),
                None => "reference-space coordinates dropped".to_owned(),
            });
        } else {
            for key in SIDE_KEYS {
                if keys.contains(&key) {
                    continue;
                }
                if let (Some(old), Some(reference)) = (
                    old_reference.get(key),
                    out.get_mut("reference").and_then(Value::as_object_mut),
                ) {
                    reference.insert(key.to_owned(), old.clone());
                }
            }
            if space == EditSpace::Native {
                for e in edited {
                    if let Some(markers) = markers_mut(&mut out["reference"], e.key) {
                        if matches!(
                            rebuild_template_markers(markers, &update_of(e)),
                            Rebuilt::Vertical
                        ) && !vertical.contains(&e.key)
                        {
                            vertical.push(e.key);
                        }
                    }
                }
            }
        }
    }

    if out.get("reference").is_some_and(Value::is_object) {
        recompute_template_contacts(
            &mut out,
            "reference",
            opts.el_model.as_deref(),
            &|_| WarpedContacts::Keep,
            &mut |line| {
                warnings.push(line.clone());
                log(line);
            },
        );
    }
    Ok((out, vertical))
}

fn geometry_from(
    warped: &Value,
    fallback: &dbsloc_reconstruct::native_frame::SideGeometry,
) -> dbsloc_reconstruct::native_frame::SideGeometry {
    let rows = |field: &str| -> Option<Vec<[f64; 3]>> {
        warped[field].as_array()?.iter().map(as_xyz).collect()
    };
    let m = &warped["markers"];
    dbsloc_reconstruct::native_frame::SideGeometry {
        coords_mm: rows("coords_mm").unwrap_or_else(|| fallback.coords_mm.clone()),
        trajectory: rows("trajectory").unwrap_or_else(|| fallback.trajectory.clone()),
        head: as_xyz(&m["head"]).unwrap_or(fallback.head),
        tail: as_xyz(&m["tail"]).unwrap_or(fallback.tail),
        x: as_xyz(&m["x"]).unwrap_or(fallback.x),
        y: as_xyz(&m["y"]).unwrap_or(fallback.y),
        trajvector: as_xyz(&warped["trajvector"]).unwrap_or(fallback.trajvector),
    }
}

fn record_of<'a>(reco: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    reco["meta"]["manual_correction"]
        .get(key)
        .and_then(Value::as_object)
}

pub fn side_review(reco: &Value, key: &str) -> Option<&'static str> {
    match record_of(reco, key) {
        Some(rec) => match rec.get("review").and_then(Value::as_str) {
            Some(REVIEW_ACCEPTED) => Some(REVIEW_ACCEPTED),
            Some(REVIEW_CORRECTED) | None => Some(REVIEW_CORRECTED),
            Some(_) => None,
        },
        None => reco["props"][key]["manually_corrected"]
            .as_bool()
            .unwrap_or(false)
            .then_some(REVIEW_CORRECTED),
    }
}

pub fn roll_review(reco: &Value, key: &str) -> Option<&'static str> {
    let rec = record_of(reco, key)?;
    match rec.get("roll_review").and_then(Value::as_str) {
        Some(ROLL_SET) => Some(ROLL_SET),
        Some(ROLL_ACCEPTED) => Some(ROLL_ACCEPTED),
        Some(_) => None,
        None if rec.get("review").is_none() => rec
            .get("roll_deg")
            .is_some_and(|r| r.is_number())
            .then_some(ROLL_SET),
        None => None,
    }
}

fn roll_determined(reco: &Value, key: &str) -> bool {
    reco["meta"]["orientation"]
        .get(key)
        .and_then(|o| o.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|s| DETERMINED_ROLL_STATUSES.contains(&s))
}

fn side_is_directed(reco: &Value, key: &str, opts: &RunOptions) -> bool {
    model_name(reco, key, opts)
        .and_then(|m| electrode_by_name(&m))
        .is_none_or(|e| e.is_directed)
}

pub fn correction_input_paths(sub: &Subject) -> Vec<PathBuf> {
    let mut paths = default_lock_input_paths(sub)
        .remove(Stage::Reconstruct.cli_name())
        .unwrap_or_default();
    if sub.coreg.anat_postop.contains_key(derivatives::CT_KEY) {
        paths.push(derivatives::coreg_ct_image(sub));
    }
    paths.extend(sub.norm.transform_inverse.clone());
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(crate::paths::absolute(p).to_string_lossy().into_owned()))
        .collect()
}

struct ReviewState {
    reconstruct_complete: bool,
    orientation_complete: bool,
    any_corrected: bool,
}

fn inputs_match(fingerprint: &Value, paths: &[PathBuf], memo: &mut HashMemo) -> bool {
    fingerprint.is_array()
        && approval_inputs_still_valid_with(&json!({"inputs": fingerprint}), paths, memo)
}

fn geometry_inputs<'a>(reco: &'a Value, key: &str) -> Option<&'a Value> {
    match record_of(reco, key) {
        Some(rec) => rec.get("inputs").filter(|v| v.is_array()),
        None => reco["meta"]["reconstruct"]
            .get("inputs")
            .filter(|v| v.is_array()),
    }
}

fn review_current(reco: &Value, key: &str, paths: &[PathBuf], memo: &mut HashMemo) -> bool {
    side_review(reco, key).is_some()
        && record_of(reco, key)
            .and_then(|r| r.get("inputs"))
            .is_some_and(|fp| inputs_match(fp, paths, memo))
}

fn set_reconstruct_flags(meta: &mut Map<String, Value>, edited: &[Edited], vertical: &[&str]) {
    let Some(rec) = meta.get_mut("reconstruct").and_then(Value::as_object_mut) else {
        return;
    };
    let flags = rec
        .entry("flags")
        .or_insert_with(|| Value::Object(Map::new()));
    if !flags.is_object() {
        *flags = Value::Object(Map::new());
    }
    let Some(flags) = flags.as_object_mut() else {
        return;
    };
    for e in edited {
        let mut list: Vec<Value> = flags
            .get(e.key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|f| *f != FLAG_VERTICAL_MARKER)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if vertical.contains(&e.key) {
            list.push(json!(FLAG_VERTICAL_MARKER));
        }
        flags.insert(e.key.to_owned(), Value::Array(list));
    }
}

fn review_state(
    reco: &Value,
    block: &str,
    opts: &RunOptions,
    paths: &[PathBuf],
    memo: &mut HashMemo,
) -> ReviewState {
    let present: Vec<&str> = SIDE_KEYS
        .iter()
        .copied()
        .filter(|k| reco[block].get(*k).is_some_and(Value::is_object))
        .collect();
    let reviewed: Vec<&str> = present
        .iter()
        .copied()
        .filter(|k| review_current(reco, k, paths, memo))
        .collect();
    let directed: Vec<&str> = present
        .iter()
        .copied()
        .filter(|k| side_is_directed(reco, k, opts))
        .collect();
    ReviewState {
        reconstruct_complete: !present.is_empty() && reviewed.len() == present.len(),
        orientation_complete: !directed.is_empty()
            && directed
                .iter()
                .all(|k| reviewed.contains(k) && roll_review(reco, k).is_some()),
        any_corrected: reviewed
            .iter()
            .any(|k| side_review(reco, k) == Some(REVIEW_CORRECTED)),
    }
}

fn approve_reconstruct(sub: &Subject, any_corrected: bool, warnings: &mut Vec<String>) -> bool {
    let reconstruct = stage_approval_path(&sub.locks_dir(), Stage::Reconstruct.cli_name());
    let inputs = default_lock_input_paths(sub)
        .remove(Stage::Reconstruct.cli_name())
        .unwrap_or_default();
    let mut extra = Map::new();
    extra.insert("manually_corrected".into(), json!(any_corrected));
    match write_approval_with(&reconstruct, true, Some(METHOD), &inputs, None, &extra) {
        Ok(()) => true,
        Err(e) => {
            warnings.push(format!("lock: {}: {e}", reconstruct.display()));
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn settle_locks(
    sub: &Subject,
    paths: &[PathBuf],
    memo: &mut HashMemo,
    reco: &Value,
    space: EditSpace,
    opts: &RunOptions,
    geometry_changed: bool,
    log: &mut dyn FnMut(String),
    warnings: &mut Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let state = review_state(reco, block_name(space), opts, paths, memo);
    let mut names: Vec<String> = Vec::new();
    let mut approved: Vec<String> = Vec::new();
    if state.reconstruct_complete {
        if approve_reconstruct(sub, state.any_corrected, warnings) {
            approved.push(Stage::Reconstruct.cli_name().to_owned());
        }
    } else {
        match crate::invalidate::invalidate_stage_lock(sub, Stage::Reconstruct, METHOD) {
            Ok(true) => names.push(Stage::Reconstruct.cli_name().to_owned()),
            Ok(false) => {}
            Err(e) => warnings.push(format!("lock: {e}")),
        }
    }
    if geometry_changed {
        let invalidation =
            crate::invalidate::invalidate_dependents_as(sub, Stage::Reconstruct, METHOD);
        warnings.extend(invalidation.warnings.iter().map(|w| format!("lock: {w}")));
        names.extend(invalidation.changed.iter().map(|s| s.cli_name().to_owned()));
    }
    if state.orientation_complete {
        let orientation = stage_approval_path(&sub.locks_dir(), Stage::Orientation.cli_name());
        let inputs = default_lock_input_paths(sub)
            .remove(Stage::Orientation.cli_name())
            .unwrap_or_default();
        match write_approval(&orientation, true, Some(METHOD), &inputs, None) {
            Ok(()) => {
                names.retain(|n| n != Stage::Orientation.cli_name());
                approved.push(Stage::Orientation.cli_name().to_owned());
            }
            Err(e) => warnings.push(format!("lock: {}: {e}", orientation.display())),
        }
    }
    if !names.is_empty() {
        log(format!(
            "dbsloc: manual correction: approvals invalidated: {}",
            names.join(", ")
        ));
    }
    if !approved.is_empty() {
        log(format!(
            "dbsloc: manual correction: approved: {}",
            approved.join(", ")
        ));
    }
    (names, approved)
}

fn side_phrase(key: &str) -> String {
    format!("the {} lead (side {key})", crate::ct_recon::side_name(key))
}

pub(crate) struct KeptSides {
    pub(crate) old: Value,
    pub(crate) keys: Vec<&'static str>,
}

pub(crate) fn kept_sides(
    sub: &Subject,
    opts: &RunOptions,
    space: EditSpace,
    log: &mut dyn FnMut(String),
) -> Option<KeptSides> {
    let (_, old, tail_warnings) = load_reco_converted(sub).ok()?;
    let reviewed: Vec<&'static str> = SIDE_KEYS
        .iter()
        .copied()
        .filter(|k| side_review(&old, k).is_some())
        .collect();
    if reviewed.is_empty() {
        return None;
    }
    if opts.overwrite_approved {
        let list: Vec<String> = reviewed.iter().map(|k| side_phrase(k)).collect();
        log(format!(
            "dbsloc: warning: reconstruct: Rerun all selected steps re-traces every lead; the manual review of {} is dropped",
            list.join(" and ")
        ));
        return None;
    }
    let paths = correction_input_paths(sub);
    let mut memo = HashMemo::default();
    let block = block_name(space);
    let mut keys = Vec::new();
    for key in reviewed {
        let rec = record_of(&old, key);
        let why = if !old[block].get(key).is_some_and(Value::is_object) {
            Some(format!("the {block} block has no side {key}"))
        } else if rec
            .and_then(|r| r.get("space"))
            .and_then(Value::as_str)
            .is_some_and(|s| s != space.as_str())
        {
            Some(format!(
                "it was made in another space than the {} space this run reconstructs in",
                space.as_str()
            ))
        } else if !rec.is_some_and(|r| r.get("inputs").is_some_and(Value::is_array)) {
            Some("no input fingerprint was recorded with it".to_owned())
        } else if !rec.is_some_and(|r| {
            approval_inputs_still_valid_with(&Value::Object(r.clone()), &paths, &mut memo)
        }) {
            Some(
                "the coregistration or normalization changed since it was made, so its coordinates no longer fit"
                    .to_owned(),
            )
        } else {
            let recorded = old["props"][key]["elmodel"].as_str().map(str::trim);
            let wanted = opts.el_model.as_deref().map(str::trim);
            match (recorded, wanted) {
                (Some(a), Some(b)) if !b.is_empty() && !same_electrode(a, b) => {
                    Some(format!("it was made for the electrode model {a}, not {b}"))
                }
                _ => None,
            }
        };
        match why {
            Some(why) => log(format!(
                "dbsloc: warning: reconstruct: the manual review of {} is dropped: {why}; the lead is traced again",
                side_phrase(key)
            )),
            None => {
                log(format!(
                    "dbsloc: note: reconstruct: {} is kept as reviewed in a manual correction ({})",
                    side_phrase(key),
                    side_review(&old, key).unwrap_or(REVIEW_CORRECTED)
                ));
                keys.push(key);
            }
        }
    }
    if keys.is_empty() {
        return None;
    }
    for c in tail_warnings {
        log(c.warning);
    }
    Some(KeptSides { old, keys })
}

fn same_electrode(a: &str, b: &str) -> bool {
    match (electrode_by_name(a), electrode_by_name(b)) {
        (Some(x), Some(y)) => x.elmodel == y.elmodel,
        _ => a.eq_ignore_ascii_case(b),
    }
}

fn label_of(key: &str) -> &'static str {
    crate::ct_recon::side_name(key)
}

pub(crate) fn merge_kept_sides(new: &mut Value, kept: &KeptSides) {
    let old = &kept.old;
    let Some(root) = new.as_object_mut() else {
        return;
    };
    for block in ["native", "mni"] {
        let has_old = kept
            .keys
            .iter()
            .any(|k| old[block].get(*k).is_some_and(Value::is_object));
        if !has_old {
            continue;
        }
        let slot = root
            .entry(block)
            .or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        if let Some(target) = slot.as_object_mut() {
            for key in &kept.keys {
                if let Some(side) = old[block].get(*key).filter(|s| s.is_object()) {
                    target.insert((*key).to_owned(), side.clone());
                }
            }
        }
    }
    let props = root
        .entry("props")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(props) = props.as_object_mut() {
        for key in &kept.keys {
            if let Some(p) = old["props"].get(*key) {
                props.insert((*key).to_owned(), p.clone());
            }
        }
    }
    let meta = root
        .entry("meta")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(meta) = meta.as_object_mut() else {
        return;
    };
    let mut record = Map::new();
    for key in &kept.keys {
        if let Some(r) = old["meta"]["manual_correction"].get(*key) {
            record.insert((*key).to_owned(), r.clone());
        }
    }
    meta.insert("manual_correction".into(), Value::Object(record));
    let old_orientation: Vec<(&str, Value)> = kept
        .keys
        .iter()
        .filter_map(|k| {
            old["meta"]["orientation"]
                .get(*k)
                .filter(|o| o.is_object())
                .map(|o| (*k, o.clone()))
        })
        .collect();
    if !old_orientation.is_empty() {
        let mut orientation = match meta.get("orientation") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        for (k, o) in old_orientation {
            orientation.insert(k.to_owned(), o);
        }
        meta.insert("orientation".into(), Value::Object(orientation));
    }
    for name in ["side_source", "metal_axis", "side_notes"] {
        for key in &kept.keys {
            let label = label_of(key);
            let previous = old["meta"][name].get(label).cloned();
            match (meta.get_mut(name).and_then(Value::as_object_mut), previous) {
                (Some(m), Some(v)) => {
                    m.insert(label.to_owned(), v);
                }
                (Some(m), None) => {
                    m.remove(label);
                }
                (None, Some(v)) => {
                    let mut m = Map::new();
                    m.insert(label.to_owned(), v);
                    meta.insert(name.to_owned(), Value::Object(m));
                }
                (None, None) => {}
            }
        }
    }
    let entry = meta
        .entry("reconstruct")
        .or_insert_with(|| crate::ct_recon::reconstruct_meta(&[], &[], &[]));
    if !entry.is_object() {
        *entry = crate::ct_recon::reconstruct_meta(&[], &[], &[]);
    }
    if let Some(rec) = entry.as_object_mut() {
        for field in ["status", "flags"] {
            if !rec.get(field).is_some_and(Value::is_object) {
                rec.insert(field.into(), Value::Object(Map::new()));
            }
        }
        for key in &kept.keys {
            if let Some(st) = rec.get_mut("status").and_then(Value::as_object_mut) {
                st.insert((*key).to_owned(), json!(crate::ct_recon::SIDE_KEPT));
            }
            let flags = old["meta"]["reconstruct"]["flags"]
                .get(*key)
                .filter(|f| f.is_array())
                .cloned()
                .unwrap_or_else(|| json!([]));
            if let Some(fl) = rec.get_mut("flags").and_then(Value::as_object_mut) {
                fl.insert((*key).to_owned(), flags);
            }
            if let Some(failed) = rec.get_mut("failed").and_then(Value::as_object_mut) {
                failed.remove(*key);
            }
        }
        if rec
            .get("failed")
            .and_then(Value::as_object)
            .is_some_and(Map::is_empty)
        {
            rec.remove("failed");
        }
    }
}

pub(crate) fn restore_kept_reference(out: &mut Value, kept: &KeptSides) {
    let Some(reference) = out.get_mut("reference").and_then(Value::as_object_mut) else {
        return;
    };
    for key in &kept.keys {
        if let Some(old) = kept.old["reference"].get(*key).filter(|s| s.is_object()) {
            reference.insert((*key).to_owned(), old.clone());
        }
    }
}

pub(crate) fn record_reconstruct_inputs(
    sub: &Subject,
    reco: &mut Value,
    log: &mut dyn FnMut(String),
) {
    let inputs = match snap_input_paths(&correction_input_paths(sub)) {
        Ok(v) => Value::Array(v),
        Err(e) => {
            log(format!(
                "dbsloc: warning: reconstruct: the input fingerprint could not be recorded ({e}); its leads cannot be accepted until reconstruct runs again"
            ));
            Value::Null
        }
    };
    let Some(meta) = reco
        .as_object_mut()
        .map(|o| o.entry("meta").or_insert_with(|| Value::Object(Map::new())))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let entry = meta
        .entry("reconstruct")
        .or_insert_with(|| crate::ct_recon::reconstruct_meta(&[], &[], &[]));
    if !entry.is_object() {
        *entry = crate::ct_recon::reconstruct_meta(&[], &[], &[]);
    }
    if let Some(rec) = entry.as_object_mut() {
        rec.insert("inputs".into(), inputs);
    }
}

pub(crate) fn settle_reconstruct_lock(
    sub: &Subject,
    reco: &Value,
    space: EditSpace,
    opts: &RunOptions,
    log: &mut dyn FnMut(String),
) {
    let paths = correction_input_paths(sub);
    let state = review_state(
        reco,
        block_name(space),
        opts,
        &paths,
        &mut HashMemo::default(),
    );
    let mut warnings = Vec::new();
    if state.reconstruct_complete {
        if approve_reconstruct(sub, state.any_corrected, &mut warnings) {
            log("dbsloc: note: reconstruct: every lead in the file was reviewed in a manual correction; the reconstruct step stays approved".to_owned());
        }
    } else {
        let lock = stage_approval_path(&sub.locks_dir(), Stage::Reconstruct.cli_name());
        let by_correction = crate::locks::read_approval(&lock)
            .ok()
            .flatten()
            .is_some_and(|l| {
                crate::locks::approval_flag(&l) && l["method"].as_str() == Some(METHOD)
            });
        if by_correction {
            match crate::invalidate::invalidate_stage_lock(
                sub,
                Stage::Reconstruct,
                Stage::Reconstruct.cli_name(),
            ) {
                Ok(true) => log("dbsloc: note: reconstruct: not every lead in the file is reviewed any more; the approval from the manual correction is withdrawn".to_owned()),
                Ok(false) => {}
                Err(e) => warnings.push(format!("lock: {e}")),
            }
        }
    }
    for w in warnings {
        log(format!("dbsloc: warning: {w}"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::bids::resolve_subject_with;
    use crate::locks::read_approval;
    use crate::point_warp::PointWarpError;
    use dbsloc_reconstruct::native_frame::SideGeometry;
    use dbsloc_reconstruct::orientation::roll_from_anterior_deg;

    const BOSTON: &str = "Boston Scientific Vercise Directed";

    struct Identity;

    impl PointWarp for Identity {
        fn to_template(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, PointWarpError> {
            Ok(pts.to_vec())
        }
    }

    struct Fixture {
        root: PathBuf,
        sub: Subject,
        opts: RunOptions,
        before: Value,
        geoms: [SideGeometry; 2],
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_manual_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("rawdata/sub-1/ses-preop/anat")).unwrap();
        std::fs::write(
            d.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"),
            b"x",
        )
        .unwrap();
        d
    }

    fn geometry(head: [f64; 3], tilt: [f64; 3]) -> SideGeometry {
        let model = electrode_by_name(BOSTON).unwrap();
        let span = model.fiducial_span_mm();
        let tail = add3(head, scale3(unit3(tilt), span));
        let y = unit3(sub3([0.3, 1.0, 0.1], [0.0; 3]));
        let u = unit3(sub3(tail, head));
        let y = unit3(sub3(y, scale3(u, dot3(y, u))));
        let x = cross3(y, u);
        let r = 0.5 * model.lead_diameter_mm;
        resolve_fiducials(
            [
                head,
                tail,
                add3(head, scale3(x, r)),
                add3(head, scale3(y, r)),
            ],
            model,
        )
        .unwrap()
    }

    fn block(g: &SideGeometry) -> Value {
        side_json(g)
    }

    fn fixture(name: &str, space: EditSpace) -> Fixture {
        let root = scratch(name);
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        std::fs::create_dir_all(&sub.recon_dir).unwrap();
        let g1 = geometry([12.0, -2.0, -8.0], [0.1, 0.2, 1.0]);
        let g2 = geometry([-12.0, -3.0, -9.0], [-0.1, 0.1, 1.0]);
        let sides = |a: &SideGeometry, b: &SideGeometry| json!({"1": block(a), "2": block(b)});
        let mut reco = json!({
            "version": 2,
            "subj_id": "1",
            "method": "trac_core_rust_native",
            "mni": sides(&g1, &g2),
            "props": {
                "1": {"elmodel": BOSTON, "manually_corrected": false},
                "2": {"elmodel": BOSTON, "manually_corrected": false},
            },
            "meta": {
                "orientation": {
                    "1": {"status": "resolved", "method": "diode_marker_v1", "roll_deg": 12.5,
                          "candidates_deg": [12.5, -167.5], "flags": []},
                    "2": "unresolved",
                },
                "tracer": "trac_core_rust",
                "software": {"product": "Wyss DBS Localizer", "version": "0.0.0-fixture",
                             "written_at": "2026-01-01T00:00:00Z"},
                "reconstruct": {
                    "status": {"1": "traced", "2": "traced"},
                    "flags": {"1": [], "2": []},
                    "inputs": snap_input_paths(&correction_input_paths(&sub)).unwrap(),
                },
            },
        });
        if space == EditSpace::Native {
            reco.as_object_mut()
                .unwrap()
                .insert("native".into(), sides(&g1, &g2));
        }
        crate::template_contacts::recompute_template_contacts(
            &mut reco,
            "mni",
            None,
            &|_| WarpedContacts::Omit,
            &mut |_| {},
        );
        let text = serde_json::to_string_pretty(&reco).unwrap() + "\n";
        std::fs::write(sub.reconstruction_json(), text).unwrap();
        let mut opts = RunOptions::new(&root, "1");
        opts.el_model = None;
        Fixture {
            root,
            sub,
            opts,
            before: reco,
            geoms: [g1, g2],
        }
    }

    fn reload(fx: &Fixture) -> Value {
        serde_json::from_str(&std::fs::read_to_string(fx.sub.reconstruction_json()).unwrap())
            .unwrap()
    }

    struct Failing;

    impl PointWarp for Failing {
        fn to_template(&self, _: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, PointWarpError> {
            Err(PointWarpError::NotFinite)
        }
    }

    fn apply(fx: &Fixture, req: &CorrectionRequest) -> PipelineResult<CorrectionReport> {
        let mut factory = |_: &mut dyn FnMut(String)| -> PipelineResult<Box<dyn PointWarp>> {
            Ok(Box::new(Identity))
        };
        apply_inner(&fx.sub, &fx.opts, req, None, &mut |_| {}, &mut factory)
    }

    fn apply_failing(fx: &Fixture, req: &CorrectionRequest) -> PipelineResult<CorrectionReport> {
        let mut factory = |_: &mut dyn FnMut(String)| -> PipelineResult<Box<dyn PointWarp>> {
            Ok(Box::new(Failing))
        };
        apply_inner(&fx.sub, &fx.opts, req, None, &mut |_| {}, &mut factory)
    }

    fn rewrite(fx: &Fixture, edit: impl FnOnce(&mut Value)) {
        let mut v = reload(fx);
        edit(&mut v);
        std::fs::write(
            fx.sub.reconstruction_json(),
            serde_json::to_string_pretty(&v).unwrap() + "\n",
        )
        .unwrap();
    }

    fn request(space: EditSpace, edits: Vec<SideEdit>) -> CorrectionRequest {
        CorrectionRequest {
            space,
            source: "cli".into(),
            sides: edits,
        }
    }

    fn edit(key: &str, g: &SideGeometry, shift: [f64; 3], roll: Option<f64>) -> SideEdit {
        SideEdit {
            key: key.into(),
            head: add3(g.head, shift),
            tail: add3(g.tail, shift),
            roll_deg: roll,
            accept: false,
        }
    }

    fn max_diff(a: &Value, b: &Value) -> f64 {
        match (a, b) {
            (Value::Array(x), Value::Array(y)) => {
                assert_eq!(x.len(), y.len());
                x.iter()
                    .zip(y)
                    .map(|(p, q)| max_diff(p, q))
                    .fold(0.0, f64::max)
            }
            (Value::Object(x), Value::Object(y)) => {
                assert_eq!(x.len(), y.len());
                x.iter()
                    .map(|(k, v)| max_diff(v, &y[k]))
                    .fold(0.0, f64::max)
            }
            (Value::Number(p), Value::Number(q)) => {
                (p.as_f64().unwrap() - q.as_f64().unwrap()).abs()
            }
            _ => {
                assert_eq!(a, b);
                0.0
            }
        }
    }

    fn marker(v: &Value, side: &str, block: &str, name: &str) -> [f64; 3] {
        as_xyz(&v[block][side]["markers"][name]).unwrap()
    }

    fn roll_of(v: &Value, side: &str, block: &str) -> f64 {
        let head = marker(v, side, block, "head");
        let tail = marker(v, side, block, "tail");
        let y = marker(v, side, block, "y");
        roll_from_anterior_deg(unit3(sub3(tail, head)), unit3(sub3(y, head)))
    }

    fn cleanup(fx: &Fixture) {
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    fn azimuth_of(v: &Value, side: &str, block: &str) -> f64 {
        let head = marker(v, side, block, "head");
        roll_azimuth_deg(sub3(marker(v, side, block, "y"), head)).unwrap()
    }

    fn wrapped(deg: f64) -> f64 {
        let d = deg.rem_euclid(360.0);
        if d > 180.0 {
            d - 360.0
        } else {
            d
        }
    }

    const TILT_30: [f64; 3] = [0.5, 0.0, 0.866_025_403_784_438_6];

    #[test]
    fn the_azimuth_frame_round_trips_and_equals_the_axis_rotation_on_a_vertical_lead() {
        let head = [1.0, -2.0, 3.0];
        for t in [
            [0.0, 0.0, 1.0],
            TILT_30,
            unit3([0.2, 0.3, 1.0]),
            unit3([-0.4, 0.1, 1.0]),
        ] {
            let tail = add3(head, scale3(t, 10.0));
            for deg in [
                -170.0, -90.0, -45.0, -30.0, 0.0, 17.5, 30.0, 90.0, 135.0, 179.0,
            ] {
                let (x, y) = roll_frame(head, tail, deg);
                let back = roll_azimuth_deg(y).unwrap();
                assert!(
                    wrapped(back - deg).abs() < 1e-9,
                    "t {t:?} deg {deg} got {back}"
                );
                assert!(dot3(y, t).abs() < 1e-12 && dot3(x, t).abs() < 1e-12);
                assert!((norm3(x) - 1.0).abs() < 1e-12 && (norm3(y) - 1.0).abs() < 1e-12);
                assert!(norm3(sub3(x, cross3(y, t))) < 1e-12);
                if t == [0.0, 0.0, 1.0] {
                    let axis = unit3(rotate_about(roll_reference(t), t, deg));
                    assert!(norm3(sub3(y, axis)) < 1e-12);
                    assert!((wrapped(roll_from_anterior_deg(t, y) - deg)).abs() < 1e-9);
                }
            }
        }
    }

    #[test]
    fn identity_correction_is_idempotent() {
        let fx = fixture("idem", EditSpace::Native);
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], None)],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        assert!(max_diff(&after["native"]["1"], &fx.before["native"]["1"]) < 1e-6);
        for field in ["coords_mm", "markers", "trajectory"] {
            assert!(max_diff(&after["mni"]["1"][field], &fx.before["mni"]["1"][field]) < 1e-6);
        }
        assert!(
            max_diff(
                &after["mni"]["1"]["coords_mm_warped"],
                &fx.before["native"]["1"]["coords_mm"]
            ) < 1e-6
        );
        assert_eq!(after["meta"]["template_contacts"], "from_markers");
        assert_eq!(after["props"]["1"]["manually_corrected"], true);
        assert_eq!(after["props"]["2"]["manually_corrected"], false);
        cleanup(&fx);
    }

    #[test]
    fn shifting_head_and_tail_shifts_every_contact_and_keeps_the_roll() {
        let fx = fixture("shift", EditSpace::Native);
        let shift = [1.0, 2.0, 3.0];
        let before_roll = roll_of(&fx.before, "1", "native");
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], shift, None)],
        );
        let report = apply(&fx, &req).unwrap();
        let after = reload(&fx);
        let old = fx.before["native"]["1"]["coords_mm"].as_array().unwrap();
        let new = after["native"]["1"]["coords_mm"].as_array().unwrap();
        for (o, n) in old.iter().zip(new) {
            let d = sub3(as_xyz(n).unwrap(), as_xyz(o).unwrap());
            assert!(norm3(sub3(d, shift)) < 1e-6, "{d:?}");
        }
        assert!((roll_of(&after, "1", "native") - before_roll).abs() < 1e-6);
        let rec = &after["meta"]["manual_correction"]["1"];
        assert_eq!(rec["source"], "cli");
        assert_eq!(rec["space"], "native");
        assert!((rec["head_shift_mm"].as_f64().unwrap() - 14f64.sqrt()).abs() < 1e-9);
        assert!(rec["roll_deg"].is_null());
        assert_eq!(
            rec["previous"]["head"],
            fx.before["native"]["1"]["markers"]["head"]
        );
        assert!((report.sides[0].tail_shift_mm - 14f64.sqrt()).abs() < 1e-9);
        assert_eq!(
            after["meta"]["orientation"]["1"],
            fx.before["meta"]["orientation"]["1"]
        );
        assert_eq!(after["meta"]["software"], fx.before["meta"]["software"]);
        assert_eq!(rec["software"], crate::version::software_brief());
        cleanup(&fx);
    }

    #[test]
    fn a_given_roll_sets_the_marker_direction_and_radius() {
        let fx = fixture("roll", EditSpace::Native);
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], Some(30.0))],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        let head = marker(&after, "1", "native", "head");
        let x = marker(&after, "1", "native", "x");
        let y = marker(&after, "1", "native", "y");
        assert!((azimuth_of(&after, "1", "native") - 30.0).abs() < 1e-6);
        let r = 0.5 * electrode_by_name(BOSTON).unwrap().lead_diameter_mm;
        assert!((norm3(sub3(x, head)) - r).abs() < 1e-6);
        assert!((norm3(sub3(y, head)) - r).abs() < 1e-6);
        let o = &after["meta"]["orientation"]["1"];
        assert_eq!(o["status"], "manual");
        assert_eq!(o["roll_deg"], 30.0);
        assert!(o["candidates_deg"].is_null());
        assert_eq!(after["meta"]["orientation"]["2"], "unresolved");
        cleanup(&fx);
    }

    fn tilted_edit(g: &SideGeometry, roll: Option<f64>) -> SideEdit {
        let span = norm3(sub3(g.tail, g.head));
        SideEdit {
            key: "1".into(),
            head: g.head,
            tail: add3(g.head, scale3(TILT_30, span)),
            roll_deg: roll,
            accept: false,
        }
    }

    #[test]
    fn a_roll_on_a_thirty_degree_tilted_lead_is_a_horizontal_azimuth() {
        let fx = fixture("tilt30", EditSpace::Native);
        let req = request(
            EditSpace::Native,
            vec![tilted_edit(&fx.geoms[0], Some(45.0))],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        let head = marker(&after, "1", "native", "head");
        let tail = marker(&after, "1", "native", "tail");
        let d = sub3(marker(&after, "1", "native", "y"), head);
        let h = d[0].hypot(d[1]);
        assert!((d[0] / h + 45f64.to_radians().sin()).abs() < 1e-9, "{d:?}");
        assert!((d[1] / h - 45f64.to_radians().cos()).abs() < 1e-9, "{d:?}");
        assert!(d[0] < 0.0);
        let t = unit3(sub3(tail, head));
        assert!(dot3(d, t).abs() < 1e-9);
        let axis = roll_from_anterior_deg(t, unit3(d));
        assert!((axis - 45.0 - 4.1).abs() < 0.05, "axis convention {axis}");
        assert_eq!(after["meta"]["manual_correction"]["1"]["roll_deg"], 45.0);
        let o = &after["meta"]["orientation"]["1"];
        assert_eq!(o["status"], "manual");
        assert_eq!(o["method"], "manual");
        assert_eq!(o["roll_deg"], 45.0);
        assert!((azimuth_of(&after, "1", "mni") - 45.0).abs() < 1e-9);
        cleanup(&fx);
    }

    #[test]
    fn reading_back_an_applied_roll_returns_it() {
        let fx = fixture("readback", EditSpace::Native);
        for deg in [-170.0, -45.0, 0.0, 30.0, 90.0, 179.0] {
            let req = request(
                EditSpace::Native,
                vec![tilted_edit(&fx.geoms[0], Some(deg))],
            );
            apply(&fx, &req).unwrap();
            let after = reload(&fx);
            for block in ["native", "mni"] {
                let back = azimuth_of(&after, "1", block);
                assert!((back - deg).abs() < 1e-9, "{block} {deg} read back {back}");
            }
            let ctx = inspect_manual_correction(&fx.sub, &fx.opts).unwrap();
            let reported = ctx.sides[0].roll_deg.unwrap();
            assert!(
                (reported - deg).abs() < 1e-9,
                "reported {reported} for {deg}"
            );
        }
        cleanup(&fx);
    }

    #[test]
    fn moving_the_head_without_a_roll_keeps_the_azimuth() {
        let fx = fixture("keep_azimuth", EditSpace::Native);
        apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![tilted_edit(&fx.geoms[0], Some(45.0))],
            ),
        )
        .unwrap();
        let start = reload(&fx);
        let head = marker(&start, "1", "native", "head");
        let tail = marker(&start, "1", "native", "tail");
        let axis_before = roll_of(&start, "1", "native");
        let req = request(
            EditSpace::Native,
            vec![SideEdit {
                key: "1".into(),
                head: add3(head, [-4.0, 1.5, 0.5]),
                tail,
                roll_deg: None,
                accept: false,
            }],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        for block in ["native", "mni"] {
            let back = azimuth_of(&after, "1", block);
            assert!((back - 45.0).abs() < 1e-9, "{block}: {back}");
        }
        assert!((roll_of(&after, "1", "native") - axis_before).abs() > 0.1);
        assert!(after["meta"]["manual_correction"]["1"]["roll_deg"].is_null());
        assert_eq!(
            after["meta"]["orientation"]["1"],
            start["meta"]["orientation"]["1"]
        );
        cleanup(&fx);
    }

    #[test]
    fn identity_warp_makes_mni_equal_native_and_the_other_side_is_untouched() {
        let fx = fixture("mni", EditSpace::Native);
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [1.0, -1.0, 2.0], Some(-40.0))],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        for field in ["coords_mm", "markers", "trajectory", "trajvector"] {
            assert!(max_diff(&after["mni"]["1"][field], &after["native"]["1"][field]) < 1e-9);
        }
        assert!(
            max_diff(
                &after["mni"]["1"]["coords_mm_warped"],
                &after["native"]["1"]["coords_mm"]
            ) < 1e-9
        );
        assert!(after["mni"]["2"].get("coords_mm_warped").is_none());
        for block in ["native", "mni"] {
            assert_eq!(
                serde_json::to_string_pretty(&after[block]["2"]).unwrap(),
                serde_json::to_string_pretty(&fx.before[block]["2"]).unwrap()
            );
        }
        assert_eq!(after["props"]["2"], fx.before["props"]["2"]);
        cleanup(&fx);
    }

    #[test]
    fn both_sides_can_be_corrected_in_one_call() {
        let fx = fixture("both", EditSpace::Native);
        let req = request(
            EditSpace::Native,
            vec![
                edit("1", &fx.geoms[0], [0.5, 0.0, 0.0], None),
                edit("2", &fx.geoms[1], [0.0, 0.5, 0.0], Some(10.0)),
            ],
        );
        let report = apply(&fx, &req).unwrap();
        assert_eq!(report.sides.len(), 2);
        let after = reload(&fx);
        assert_eq!(after["props"]["1"]["manually_corrected"], true);
        assert_eq!(after["props"]["2"]["manually_corrected"], true);
        assert_eq!(after["meta"]["orientation"]["2"]["status"], "manual");
        assert!(after["meta"]["orientation"]["1"]["status"] == "resolved");
        cleanup(&fx);
    }

    fn write_lock(fx: &Fixture, stage: &str) {
        write_approval(
            &stage_approval_path(&fx.sub.locks_dir(), stage),
            true,
            Some("fake"),
            &[],
            None,
        )
        .unwrap();
    }

    fn lock(fx: &Fixture, stage: &str) -> Value {
        read_approval(&stage_approval_path(&fx.sub.locks_dir(), stage))
            .unwrap()
            .unwrap()
    }

    fn accept(key: &str) -> SideEdit {
        SideEdit {
            key: key.into(),
            head: [0.0; 3],
            tail: [0.0; 3],
            roll_deg: None,
            accept: true,
        }
    }

    fn with_tracked_input(fx: Fixture) -> (Fixture, PathBuf) {
        let raw = fx
            .root
            .join("rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_CT.nii.gz");
        std::fs::create_dir_all(raw.parent().unwrap()).unwrap();
        std::fs::write(&raw, b"ct").unwrap();
        let sub = resolve_subject_with(&fx.root, "1", false).unwrap();
        let ct = derivatives::coreg_ct_image(&sub);
        std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
        std::fs::write(&ct, b"first coregistration").unwrap();
        let sub = resolve_subject_with(&fx.root, "1", false).unwrap();
        let paths = correction_input_paths(&sub);
        assert!(
            paths.iter().any(|p| p.ends_with(ct.file_name().unwrap())),
            "{paths:?}"
        );
        let fx = Fixture { sub, ..fx };
        let snap = snap_input_paths(&paths).unwrap();
        rewrite(&fx, |v| {
            v["meta"]["reconstruct"]["inputs"] = json!(snap);
        });
        (fx, ct)
    }

    #[test]
    fn accepting_or_counting_a_lead_needs_the_inputs_its_coordinates_were_made_with() {
        let (fx, ct) = with_tracked_input(fixture("stale", EditSpace::Native));
        apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![edit("1", &fx.geoms[0], [0.5, 0.0, 0.0], None)],
            ),
        )
        .unwrap();
        let side1 = reload(&fx)["meta"]["manual_correction"]["1"].clone();
        std::fs::write(&ct, b"the normalization re-ran").unwrap();
        for key in ["1", "2"] {
            let err = apply(&fx, &request(EditSpace::Native, vec![accept(key)]))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("inputs changed since the reconstruction"),
                "{err}"
            );
        }
        let report = apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![edit("2", &fx.geoms[1], [0.0, 0.5, 0.0], None)],
            ),
        )
        .unwrap();
        assert!(report.approved.is_empty());
        assert!(!stage_approval_path(&fx.sub.locks_dir(), "reconstruct").exists());
        let after = reload(&fx);
        assert_eq!(after["meta"]["manual_correction"]["1"], side1);
        assert!(
            kept_sides(&fx.sub, &fx.opts, EditSpace::Native, &mut |_| {})
                .is_some_and(|k| k.keys == ["2"])
        );
        cleanup(&fx);
    }

    #[test]
    fn a_file_without_a_reconstruct_fingerprint_cannot_be_accepted() {
        let fx = fixture("nofp", EditSpace::Native);
        rewrite(&fx, |v| {
            v["meta"]["reconstruct"]
                .as_object_mut()
                .unwrap()
                .remove("inputs");
        });
        let err = apply(&fx, &request(EditSpace::Native, vec![accept("1")]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("run reconstruct before accepting"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn electrode_aliases_are_the_same_model() {
        assert!(same_electrode(BOSTON, &BOSTON.to_ascii_lowercase()));
        assert!(same_electrode(BOSTON, &format!("  {BOSTON} ")));
        assert!(!same_electrode(BOSTON, "Medtronic 3387"));
        let fx = fixture("alias", EditSpace::Native);
        apply(&fx, &request(EditSpace::Native, vec![accept("1")])).unwrap();
        let mut opts = fx.opts.clone();
        opts.el_model = Some(BOSTON.to_ascii_uppercase());
        let kept = kept_sides(&fx.sub, &opts, EditSpace::Native, &mut |_| {}).unwrap();
        assert_eq!(kept.keys, ["1"]);
        opts.el_model = Some("Medtronic 3387".to_owned());
        assert!(kept_sides(&fx.sub, &opts, EditSpace::Native, &mut |_| {}).is_none());
        cleanup(&fx);
    }

    #[test]
    fn a_reconstruct_run_withdraws_only_a_stale_correction_approval() {
        let fx = fixture("withdraw", EditSpace::Native);
        let reco = reload(&fx);
        let path = stage_approval_path(&fx.sub.locks_dir(), "reconstruct");
        write_approval(&path, true, Some(METHOD), &[], None).unwrap();
        let mut lines = Vec::new();
        settle_reconstruct_lock(&fx.sub, &reco, EditSpace::Native, &fx.opts, &mut |l| {
            lines.push(l)
        });
        let l = lock(&fx, "reconstruct");
        assert_eq!(l["approval"], false);
        assert_eq!(l["invalidated_by"], "reconstruct");
        assert!(lines.iter().any(|l| l.contains("withdrawn")), "{lines:?}");
        write_approval(&path, true, Some("user"), &[], None).unwrap();
        settle_reconstruct_lock(&fx.sub, &reco, EditSpace::Native, &fx.opts, &mut |_| {});
        assert_eq!(lock(&fx, "reconstruct")["approval"], true);
        cleanup(&fx);
    }

    #[test]
    fn locks_follow_the_roll() {
        let fx = fixture("locks_roll", EditSpace::Native);
        for stage in ["vat", "visualize", "orientation"] {
            write_lock(&fx, stage);
        }
        let req = request(
            EditSpace::Native,
            vec![
                edit("1", &fx.geoms[0], [0.0; 3], Some(5.0)),
                edit("2", &fx.geoms[1], [0.0; 3], Some(-5.0)),
            ],
        );
        let report = apply(&fx, &req).unwrap();
        let reconstruct = lock(&fx, "reconstruct");
        assert_eq!(reconstruct["approval"], true);
        assert_eq!(reconstruct["method"], "manual_correction");
        assert_eq!(reconstruct["manually_corrected"], true);
        for stage in ["vat", "visualize"] {
            let l = lock(&fx, stage);
            assert_eq!(l["approval"], false, "{stage}");
            assert_eq!(l["invalidated_by"], "manual_correction");
        }
        let orientation = lock(&fx, "orientation");
        assert_eq!(orientation["approval"], true);
        assert_eq!(orientation["method"], "manual_correction");
        assert_eq!(report.invalidated, ["vat", "visualize"]);
        assert_eq!(report.approved, ["reconstruct", "orientation"]);
        let after = reload(&fx);
        for key in ["1", "2"] {
            let rec = &after["meta"]["manual_correction"][key];
            assert_eq!(rec["review"], "corrected");
            assert_eq!(rec["roll_review"], "set");
            assert!(rec["inputs"].is_array());
        }
        cleanup(&fx);
    }

    #[test]
    fn one_corrected_lead_of_two_leaves_the_steps_unapproved() {
        let fx = fixture("locks_one", EditSpace::Native);
        for stage in ["reconstruct", "vat", "orientation"] {
            write_lock(&fx, stage);
        }
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], Some(5.0))],
        );
        let report = apply(&fx, &req).unwrap();
        let reconstruct = lock(&fx, "reconstruct");
        assert_eq!(reconstruct["approval"], false);
        assert_eq!(reconstruct["invalidated_by"], "manual_correction");
        assert_eq!(lock(&fx, "orientation")["approval"], false);
        assert_eq!(report.invalidated, ["reconstruct", "orientation", "vat"]);
        assert!(report.approved.is_empty());
        cleanup(&fx);
    }

    #[test]
    fn locks_without_a_roll_invalidate_the_orientation() {
        let fx = fixture("locks_keep", EditSpace::Native);
        for stage in ["vat", "visualize", "orientation"] {
            write_lock(&fx, stage);
        }
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], None), accept("2")],
        );
        let report = apply(&fx, &req).unwrap();
        assert_eq!(lock(&fx, "reconstruct")["approval"], true);
        assert_eq!(lock(&fx, "orientation")["approval"], false);
        assert_eq!(lock(&fx, "vat")["approval"], false);
        assert_eq!(report.invalidated, ["orientation", "vat", "visualize"]);
        assert_eq!(report.approved, ["reconstruct"]);
        let after = reload(&fx);
        assert!(after["meta"]["manual_correction"]["1"]["roll_review"].is_null());
        assert_eq!(
            after["meta"]["manual_correction"]["2"]["review"],
            "accepted"
        );
        assert!(after["meta"]["manual_correction"]["2"]["roll_review"].is_null());
        assert_eq!(after["props"]["2"]["manually_corrected"], false);
        cleanup(&fx);
    }

    #[test]
    fn accepting_without_an_edit_changes_no_geometry_and_completes_the_review() {
        let fx = fixture("accept", EditSpace::Native);
        write_lock(&fx, "vat");
        let report = apply(&fx, &request(EditSpace::Native, vec![accept("1")])).unwrap();
        assert_eq!(report.sides[0].review, "accepted");
        assert!(report.invalidated.is_empty() && report.approved.is_empty());
        assert_eq!(lock(&fx, "vat")["approval"], true);
        let after = reload(&fx);
        for block in ["native", "mni"] {
            assert_eq!(after[block], fx.before[block], "{block}");
        }
        let rec = &after["meta"]["manual_correction"]["1"];
        assert_eq!(rec["review"], "accepted");
        assert_eq!(rec["roll_review"], "accepted");
        assert_eq!(after["props"]["1"]["manually_corrected"], false);
        let ctx = inspect_manual_correction(&fx.sub, &fx.opts).unwrap();
        assert_eq!(ctx.sides[0].review.as_deref(), Some("accepted"));
        assert!(ctx.sides[1].review.is_none());

        let report = apply(&fx, &request(EditSpace::Native, vec![accept("2")])).unwrap();
        assert_eq!(report.approved, ["reconstruct"]);
        assert_eq!(lock(&fx, "reconstruct")["manually_corrected"], false);
        let err = apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![accept("1"), edit("1", &fx.geoms[0], [0.0; 3], None)],
            ),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("side 1 given twice"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn a_later_correction_keeps_an_earlier_set_roll_review() {
        let fx = fixture("roll_carry", EditSpace::Native);
        apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![edit("1", &fx.geoms[0], [0.0; 3], Some(30.0))],
            ),
        )
        .unwrap();
        apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![edit("1", &fx.geoms[0], [0.5, 0.0, 0.0], None)],
            ),
        )
        .unwrap();
        let after = reload(&fx);
        assert_eq!(
            after["meta"]["manual_correction"]["1"]["roll_review"],
            "set"
        );
        cleanup(&fx);
    }

    #[test]
    fn kept_sides_follow_the_review_the_inputs_and_the_overwrite_flag() {
        let fx = fixture("kept", EditSpace::Native);
        apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![edit("1", &fx.geoms[0], [0.5, 0.0, 0.0], None), accept("2")],
            ),
        )
        .unwrap();
        let mut lines = Vec::new();
        let kept =
            kept_sides(&fx.sub, &fx.opts, EditSpace::Native, &mut |l| lines.push(l)).expect("kept");
        assert_eq!(kept.keys, ["1", "2"]);
        assert!(lines.iter().all(|l| l.contains("is kept as reviewed")));

        let mut overwrite = fx.opts.clone();
        overwrite.overwrite_approved = true;
        let mut lines = Vec::new();
        assert!(
            kept_sides(&fx.sub, &overwrite, EditSpace::Native, &mut |l| lines
                .push(l))
            .is_none()
        );
        assert!(
            lines[0].contains("Rerun all selected steps re-traces every lead"),
            "{lines:?}"
        );

        let mut lines = Vec::new();
        assert!(kept_sides(&fx.sub, &fx.opts, EditSpace::Mni, &mut |l| lines.push(l)).is_none());

        let stale = fx.root.join("stale.nii");
        std::fs::write(&stale, b"a").unwrap();
        let snap = snap_input_paths(std::slice::from_ref(&stale)).unwrap();
        rewrite(&fx, |v| {
            v["meta"]["manual_correction"]["2"]["inputs"] = json!(snap);
        });
        let mut lines = Vec::new();
        let kept = kept_sides(&fx.sub, &fx.opts, EditSpace::Native, &mut |l| lines.push(l))
            .expect("side 1 still kept");
        assert_eq!(kept.keys, ["1"]);
        assert!(
            lines.iter().any(|l| l.contains("left lead (side 2) is dropped")
                && l.contains("no longer fit")),
            "{lines:?}"
        );
        cleanup(&fx);
    }

    #[test]
    fn merging_kept_sides_restores_their_blocks_and_records() {
        let fx = fixture("merge", EditSpace::Native);
        apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![edit("1", &fx.geoms[0], [0.5, 0.0, 0.0], Some(20.0))],
            ),
        )
        .unwrap();
        let kept = kept_sides(&fx.sub, &fx.opts, EditSpace::Native, &mut |_| {}).unwrap();
        let mut fresh = json!({
            "version": 2,
            "native": {"2": fx.before["native"]["2"].clone()},
            "mni": {"2": fx.before["mni"]["2"].clone()},
            "props": {"2": {"elmodel": BOSTON, "manually_corrected": false}},
            "meta": {
                "orientation": crate::ct_recon::ORIENTATION_UNRESOLVED,
                "side_source": {"left": "trac_core"},
                "reconstruct": crate::ct_recon::reconstruct_meta(
                    &[("2", "traced"), ("1", "failed")],
                    &[("1", "right: no seed".to_owned())],
                    &[("2", vec![])],
                ),
            },
        });
        merge_kept_sides(&mut fresh, &kept);
        let old = reload(&fx);
        for block in ["native", "mni", "props"] {
            assert_eq!(fresh[block]["1"], old[block]["1"], "{block}");
            assert!(fresh[block]["2"].is_object(), "{block}");
        }
        let meta = &fresh["meta"];
        assert_eq!(
            meta["manual_correction"]["1"],
            old["meta"]["manual_correction"]["1"]
        );
        assert_eq!(meta["orientation"]["1"]["status"], "manual");
        assert!(meta["orientation"].get("2").is_none());
        assert_eq!(meta["reconstruct"]["status"]["1"], "kept");
        assert_eq!(meta["reconstruct"]["status"]["2"], "traced");
        assert_eq!(meta["reconstruct"]["flags"]["1"], json!([]));
        assert!(meta["reconstruct"].get("failed").is_none());
        let mut kept = kept;
        kept.old["reference"] = json!({"space": "MNI152NLin2009bAsym", "1": {"sentinel": 1}});
        let mut out =
            json!({"reference": {"space": "MNI152NLin2009bAsym", "1": {}, "2": {"x": 2}}});
        restore_kept_reference(&mut out, &kept);
        assert_eq!(out["reference"]["1"], json!({"sentinel": 1}));
        assert_eq!(out["reference"]["2"], json!({"x": 2}));
        cleanup(&fx);
    }

    #[test]
    fn a_reconstruct_run_with_every_lead_reviewed_keeps_the_file_and_approves() {
        let fx = fixture("rerun", EditSpace::Mni);
        let post = fx.root.join("rawdata/sub-1/ses-postop/anat");
        std::fs::create_dir_all(&post).unwrap();
        std::fs::write(post.join("sub-1_ses-postop_acq-ax_T1w.nii.gz"), b"x").unwrap();
        let sub = resolve_subject_with(&fx.root, "1", false).unwrap();
        assert_eq!(sub.postop_modality.as_deref(), Some("MRI"));
        let fx = Fixture { sub, ..fx };
        apply(
            &fx,
            &request(
                EditSpace::Mni,
                vec![edit("1", &fx.geoms[0], [0.0, 0.0, 1.0], None), accept("2")],
            ),
        )
        .unwrap();
        let before = reload(&fx);
        let lock_path = stage_approval_path(&fx.sub.locks_dir(), "reconstruct");
        std::fs::remove_file(&lock_path).unwrap();
        let outcome = crate::stage_impl::run_reconstruct(&fx.sub, &fx.opts, None, None).unwrap();
        assert!(matches!(
            outcome,
            crate::protocol::StageOutcome::Completed(_)
        ));
        let after = reload(&fx);
        for key in ["1", "2"] {
            assert_eq!(after["mni"][key], before["mni"][key]);
            assert_eq!(after["meta"]["reconstruct"]["status"][key], "kept");
        }
        assert_eq!(lock(&fx, "reconstruct")["approval"], true);

        let mut overwrite = fx.opts.clone();
        overwrite.overwrite_approved = true;
        overwrite.el_model = Some(BOSTON.to_owned());
        let err = crate::stage_impl::run_reconstruct(&fx.sub, &overwrite, None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no normalized post-op NIfTIs"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn pipeline_writers_record_the_tail_fiducial_convention() {
        let fx = fixture("tail_field", EditSpace::Mni);
        let post = fx.root.join("rawdata/sub-1/ses-postop/anat");
        std::fs::create_dir_all(&post).unwrap();
        std::fs::write(post.join("sub-1_ses-postop_acq-ax_T1w.nii.gz"), b"x").unwrap();
        let sub = resolve_subject_with(&fx.root, "1", false).unwrap();
        let fx = Fixture { sub, ..fx };
        assert!(reload(&fx)["meta"].get("tail_fiducial").is_none());
        apply(
            &fx,
            &request(
                EditSpace::Mni,
                vec![edit("1", &fx.geoms[0], [0.0, 0.0, 1.0], None), accept("2")],
            ),
        )
        .unwrap();
        assert_eq!(reload(&fx)["meta"]["tail_fiducial"], "fourth_level");

        let mut stripped = reload(&fx);
        stripped["meta"]
            .as_object_mut()
            .unwrap()
            .remove("tail_fiducial");
        std::fs::write(
            fx.sub.reconstruction_json(),
            serde_json::to_string_pretty(&stripped).unwrap(),
        )
        .unwrap();
        let lock_path = stage_approval_path(&fx.sub.locks_dir(), "reconstruct");
        let _ = std::fs::remove_file(&lock_path);
        let outcome = crate::stage_impl::run_reconstruct(&fx.sub, &fx.opts, None, None).unwrap();
        assert!(matches!(
            outcome,
            crate::protocol::StageOutcome::Completed(_)
        ));
        assert_eq!(reload(&fx)["meta"]["tail_fiducial"], "fourth_level");
        cleanup(&fx);
    }

    #[test]
    fn orientation_keeps_every_reviewed_roll_and_an_overwrite_estimates_again() {
        let fx = fixture("orient_keep", EditSpace::Native);
        apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![
                    edit("1", &fx.geoms[0], [0.0; 3], Some(10.0)),
                    edit("2", &fx.geoms[1], [0.0; 3], Some(-10.0)),
                ],
            ),
        )
        .unwrap();
        let outcome =
            crate::orientation_stage::run_orientation(&fx.sub, &fx.opts, None, None).unwrap();
        match outcome {
            crate::protocol::StageOutcome::Skipped { reason, .. } => {
                assert!(
                    reason.contains("set or accepted in a manual correction"),
                    "{reason}"
                );
            }
            other => panic!("{other:?}"),
        }
        let mut overwrite = fx.opts.clone();
        overwrite.overwrite_approved = true;
        let err = crate::orientation_stage::run_orientation(&fx.sub, &overwrite, None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("orientation: missing raw CT"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn a_template_only_file_rejects_a_native_request_and_keeps_the_file() {
        let fx = fixture("space", EditSpace::Mni);
        let bytes = std::fs::read(fx.sub.reconstruction_json()).unwrap();
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], None)],
        );
        let err = apply(&fx, &req).unwrap_err().to_string();
        assert!(err.contains("E_RECON_manual"), "{err}");
        assert_eq!(std::fs::read(fx.sub.reconstruction_json()).unwrap(), bytes);
        assert!(!stage_approval_path(&fx.sub.locks_dir(), "reconstruct").exists());
        cleanup(&fx);
    }

    #[test]
    fn a_template_edit_replaces_the_mni_block_without_creating_a_native_one() {
        let fx = fixture("mni_edit", EditSpace::Mni);
        let req = request(
            EditSpace::Mni,
            vec![edit("2", &fx.geoms[1], [0.0, 0.0, 1.5], Some(60.0))],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        assert!(after.get("native").is_none());
        assert!((azimuth_of(&after, "2", "mni") - 60.0).abs() < 1e-6);
        assert_eq!(after["meta"]["manual_correction"]["2"]["space"], "mni");
        assert!(after["mni"]["2"].get("coords_mm_warped").is_none());
        assert_eq!(after["meta"]["template_contacts"], "from_markers");
        assert_eq!(
            serde_json::to_string_pretty(&after["mni"]["1"]).unwrap(),
            serde_json::to_string_pretty(&fx.before["mni"]["1"]).unwrap()
        );
        cleanup(&fx);
    }

    #[test]
    fn bad_requests_fail_with_the_manual_prefix() {
        let fx = fixture("errors", EditSpace::Native);
        let g = &fx.geoms[0];
        let mut cases: Vec<CorrectionRequest> = Vec::new();
        cases.push(request(EditSpace::Native, vec![]));
        let mut unknown = edit("3", g, [0.0; 3], None);
        unknown.key = "3".into();
        cases.push(request(EditSpace::Native, vec![unknown]));
        cases.push(request(
            EditSpace::Native,
            vec![SideEdit {
                key: "1".into(),
                head: g.head,
                tail: g.head,
                roll_deg: None,
                accept: false,
            }],
        ));
        let mut bad_source = request(EditSpace::Native, vec![edit("1", g, [0.0; 3], None)]);
        bad_source.source = "web".into();
        cases.push(bad_source);
        for req in cases {
            let err = apply(&fx, &req).unwrap_err().to_string();
            assert!(err.contains("E_RECON_manual"), "{err}");
        }
        let mut no_model = fx.before.clone();
        no_model["props"]["1"]["elmodel"] = Value::Null;
        std::fs::write(
            fx.sub.reconstruction_json(),
            serde_json::to_string_pretty(&no_model).unwrap(),
        )
        .unwrap();
        let err = apply(
            &fx,
            &request(EditSpace::Native, vec![edit("1", g, [0.0; 3], None)]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no electrode model for side 1"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn prepare_reports_the_edit_space_sides_and_orientation() {
        let fx = fixture("prepare", EditSpace::Native);
        let ct = derivatives::coreg_ct_image(&fx.sub);
        std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
        std::fs::write(&ct, b"x").unwrap();
        let ctx = prepare_manual_correction(&fx.sub, &fx.opts).unwrap();
        assert_eq!(ctx.space, EditSpace::Native);
        assert_eq!(ctx.backdrop_nii, ct);
        assert_eq!(ctx.sides.len(), 2);
        assert_eq!(ctx.sides[0].key, "1");
        assert_eq!(ctx.sides[0].label, "Right lead");
        assert_eq!(ctx.sides[0].elmodel.as_deref(), Some(BOSTON));
        let from_markers = azimuth_of(&fx.before, "1", "native");
        assert!((ctx.sides[0].roll_deg.unwrap() - from_markers).abs() < 1e-12);
        assert!((from_markers - 12.5).abs() > 0.01);
        assert_eq!(ctx.sides[0].candidates_deg, Some([12.5, -167.5]));
        assert_eq!(ctx.sides[0].orientation_status.as_deref(), Some("resolved"));
        assert!(ctx.sides[1].roll_deg.is_none());
        assert!(!ctx.sides[1].manually_corrected);
        let json = serde_json::to_value(&ctx).unwrap();
        assert_eq!(json["space"], "native");
        let back: ManualCorrectionContext = serde_json::from_value(json).unwrap();
        assert_eq!(back.sides.len(), 2);

        let g = &fx.geoms[0];
        let r = 0.5 * electrode_by_name(BOSTON).unwrap().lead_diameter_mm;
        let (_, diode_y) = roll_frame(g.head, g.tail, 12.5);
        rewrite(&fx, |v| {
            v["native"]["1"]["markers"]["y"] = json!(add3(g.head, scale3(diode_y, r)));
        });
        let ctx = prepare_manual_correction(&fx.sub, &fx.opts).unwrap();
        let roll = ctx.sides[0].roll_deg.unwrap();
        assert!((roll - 12.5).abs() < 1e-9, "{roll}");
        rewrite(&fx, |v| {
            v["native"]["1"]["markers"]
                .as_object_mut()
                .unwrap()
                .remove("y");
        });
        let ctx = prepare_manual_correction(&fx.sub, &fx.opts).unwrap();
        assert_eq!(ctx.sides[0].roll_deg, Some(12.5));
        cleanup(&fx);
    }

    #[test]
    fn prepare_without_a_backdrop_is_an_error() {
        let fx = fixture("nobackdrop", EditSpace::Mni);
        let err = prepare_manual_correction(&fx.sub, &fx.opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("E_RECON_manual"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn an_unresolved_note_string_is_dropped_when_a_roll_is_given() {
        let fx = fixture("note", EditSpace::Native);
        rewrite(&fx, |v| {
            v["meta"]["orientation"] = json!(crate::ct_recon::ORIENTATION_UNRESOLVED);
        });
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], Some(20.0))],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        let o = after["meta"]["orientation"].as_object().unwrap();
        assert_eq!(o.len(), 1);
        assert_eq!(o["1"]["status"], "manual");
        cleanup(&fx);
    }

    #[test]
    fn a_failing_warp_leaves_the_file_and_locks_untouched() {
        let fx = fixture("warpfail", EditSpace::Native);
        let bytes = std::fs::read(fx.sub.reconstruction_json()).unwrap();
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [1.0, 0.0, 0.0], None)],
        );
        let err = apply_failing(&fx, &req).unwrap_err().to_string();
        assert!(
            err.contains("stage reconstruct failed: E_RECON_warp"),
            "{err}"
        );
        assert!(!err.contains("E_RECON_manual"), "{err}");
        assert_eq!(std::fs::read(fx.sub.reconstruction_json()).unwrap(), bytes);
        assert!(!stage_approval_path(&fx.sub.locks_dir(), "reconstruct").exists());
        cleanup(&fx);
    }

    #[test]
    fn lock_failures_become_warnings_after_the_file_is_written() {
        let fx = fixture("lockfail", EditSpace::Native);
        std::fs::write(fx.sub.locks_dir(), b"not a directory").unwrap();
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], Some(5.0)), accept("2")],
        );
        let report = apply(&fx, &req).unwrap();
        assert!(report.warnings.iter().any(|w| w.starts_with("lock:")));
        assert_eq!(reload(&fx)["props"]["1"]["manually_corrected"], true);
        cleanup(&fx);
    }

    #[test]
    fn a_vertical_marker_keeps_the_warped_markers_of_the_new_geometry_and_is_flagged() {
        let fx = fixture("vertical", EditSpace::Native);
        let head = [10.0, 0.0, -5.0];
        let tail = add3(
            head,
            [
                electrode_by_name(BOSTON).unwrap().fiducial_span_mm(),
                0.0,
                0.0,
            ],
        );
        let req = request(
            EditSpace::Native,
            vec![SideEdit {
                key: "1".into(),
                head,
                tail,
                roll_deg: Some(90.0),
                accept: false,
            }],
        );
        let report = apply(&fx, &req).unwrap();
        let after = reload(&fx);
        for name in ["x", "y"] {
            assert_eq!(
                after["mni"]["1"]["markers"][name],
                after["native"]["1"]["markers"][name]
            );
            assert_ne!(
                after["mni"]["1"]["markers"][name],
                fx.before["mni"]["1"]["markers"][name]
            );
        }
        assert_eq!(after["mni"]["1"]["markers"]["head"], json!(head));
        let flags = after["meta"]["orientation"]["1"]["flags"]
            .as_array()
            .unwrap();
        assert!(flags.iter().any(|f| f == FLAG_VERTICAL_MARKER));
        assert_eq!(report.sides.len(), 1);
        assert_eq!(
            after["meta"]["reconstruct"]["flags"]["1"],
            json!([FLAG_VERTICAL_MARKER])
        );
        assert_eq!(after["meta"]["reconstruct"]["flags"]["2"], json!([]));
        apply(
            &fx,
            &request(
                EditSpace::Native,
                vec![edit("1", &fx.geoms[0], [0.0; 3], Some(10.0))],
            ),
        )
        .unwrap();
        assert_eq!(reload(&fx)["meta"]["reconstruct"]["flags"]["1"], json!([]));
        cleanup(&fx);
    }

    #[test]
    fn inspect_needs_no_backdrop_but_prepare_does() {
        let fx = fixture("inspect", EditSpace::Native);
        let ctx = inspect_manual_correction(&fx.sub, &fx.opts).unwrap();
        assert_eq!(ctx.backdrop_nii, derivatives::coreg_ct_image(&fx.sub));
        let err = prepare_manual_correction(&fx.sub, &fx.opts)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("E_RECON_manual: backdrop image missing"),
            "{err}"
        );
        cleanup(&fx);
    }

    #[test]
    fn inspect_shows_a_template_only_file_without_a_normalized_image() {
        let fx = fixture("inspect_mni", EditSpace::Mni);
        let ctx = inspect_manual_correction(&fx.sub, &fx.opts).unwrap();
        assert_eq!(ctx.space, EditSpace::Mni);
        assert_eq!(ctx.sides.len(), 2);
        assert_eq!(ctx.backdrop_nii, PathBuf::new());
        let err = prepare_manual_correction(&fx.sub, &fx.opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no normalized post-op MR image"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn a_template_edit_keeps_the_previous_roll() {
        let fx = fixture("mni_keep", EditSpace::Mni);
        let before = roll_of(&fx.before, "1", "mni");
        let req = request(
            EditSpace::Mni,
            vec![edit("1", &fx.geoms[0], [0.5, -1.0, 2.0], None)],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        assert!((roll_of(&after, "1", "mni") - before).abs() < 1e-6);
        cleanup(&fx);
    }

    #[test]
    fn non_finite_input_is_rejected() {
        let fx = fixture("nonfinite", EditSpace::Native);
        let bytes = std::fs::read(fx.sub.reconstruction_json()).unwrap();
        let g = &fx.geoms[0];
        for bad in [f64::NAN, f64::INFINITY] {
            let mut cases = Vec::new();
            let mut e = edit("1", g, [0.0; 3], None);
            e.head[1] = bad;
            cases.push(e);
            let mut e = edit("1", g, [0.0; 3], None);
            e.tail[2] = bad;
            cases.push(e);
            cases.push(edit("1", g, [0.0; 3], Some(bad)));
            for e in cases {
                let err = apply(&fx, &request(EditSpace::Native, vec![e]))
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("E_RECON_manual"), "{err}");
            }
        }
        assert_eq!(std::fs::read(fx.sub.reconstruction_json()).unwrap(), bytes);
        cleanup(&fx);
    }

    #[test]
    fn a_side_missing_from_the_edited_block_is_an_error() {
        let fx = fixture("noside", EditSpace::Mni);
        rewrite(&fx, |v| {
            v["mni"].as_object_mut().unwrap().remove("2");
        });
        let req = request(
            EditSpace::Mni,
            vec![edit("2", &fx.geoms[1], [0.0; 3], None)],
        );
        let err = apply(&fx, &req).unwrap_err().to_string();
        assert!(err.contains("E_RECON_manual: side 2 is absent"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn a_file_without_version_two_is_rejected() {
        let fx = fixture("version", EditSpace::Native);
        rewrite(&fx, |v| {
            v["version"] = json!(1);
        });
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], None)],
        );
        let err = apply(&fx, &req).unwrap_err().to_string();
        assert!(err.contains("E_RECON_manual"), "{err}");
        cleanup(&fx);
    }

    #[test]
    fn a_file_without_meta_gains_the_correction_record() {
        let fx = fixture("nometa", EditSpace::Native);
        rewrite(&fx, |v| {
            v.as_object_mut().unwrap().remove("meta");
        });
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [0.0; 3], Some(15.0))],
        );
        apply(&fx, &req).unwrap();
        let after = reload(&fx);
        assert_eq!(after["meta"]["manual_correction"]["1"]["roll_deg"], 15.0);
        assert_eq!(after["meta"]["orientation"]["1"]["status"], "manual");
        cleanup(&fx);
    }

    #[test]
    fn a_native_block_without_mni_needs_no_warp() {
        let fx = fixture("nomni", EditSpace::Native);
        rewrite(&fx, |v| {
            v.as_object_mut().unwrap().remove("mni");
        });
        let req = request(
            EditSpace::Native,
            vec![edit("1", &fx.geoms[0], [1.0, 0.0, 0.0], None)],
        );
        apply_failing(&fx, &req).unwrap();
        let after = reload(&fx);
        assert!(after.get("mni").is_none());
        assert_eq!(after["props"]["1"]["manually_corrected"], true);
        cleanup(&fx);
    }
}
