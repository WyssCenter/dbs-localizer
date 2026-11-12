// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use ndarray::Array3;
use serde_json::{json, Map, Value};

use dbsloc_reconstruct::orientation::{
    estimate_marker_orientation, roll_azimuth_deg, roll_from_anterior_deg, support,
    template_marker_directions, Estimate, MarkerSpec, Support, FLAG_VERTICAL_MARKER, METHOD,
};
use dbsloc_reconstruct::{add3, cross3, electrode_by_name, norm3, scale3, sub3, unit3};
use fastwarp_core::{apply_rowmajor, dot3, invert_rowmajor, Mat4};
use fastwarp_reg::transform::{read_transform_file, AffineTransform, TransformElem};

use crate::bids::Subject;
use crate::ct_recon::{side_name, SIDE_KEYS};
use crate::error::{PipelineError, PipelineResult};
use crate::manual_correction::{manual_orientation, roll_review};
use crate::nifti_io::{load_volume_region_f64, NiftiGrid};
use crate::options::RunOptions;
use crate::outputs::write_replacing;
use crate::protocol::{RunEvent, StageOutcome};
use crate::stage_impl::{
    log, opts_with_prefs_model, subject_point_warper, with_space_and_reference,
};
use crate::template_contacts::{recompute_template_contacts, WarpedContacts};

const HU_FLOOR: f64 = -1024.0;
const SLICE_MARGIN_MM: f64 = 1.0;
const CROP_RADIUS_MM: f64 = 10.0;
const CROP_PAD_VOXELS: f64 = 2.0;

fn flip(p: [f64; 3]) -> [f64; 3] {
    [-p[0], -p[1], p[2]]
}

fn mat_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

pub fn anchor_to_ct_point(anchor_to_ct: &AffineTransform, p: [f64; 3]) -> [f64; 3] {
    flip(anchor_to_ct.apply(flip(p)))
}

pub fn ct_to_anchor_point(ct_to_anchor: &AffineTransform, p: [f64; 3]) -> [f64; 3] {
    flip(ct_to_anchor.apply(flip(p)))
}

pub fn ct_to_anchor_direction(ct_to_anchor: &AffineTransform, d: [f64; 3]) -> [f64; 3] {
    flip(mat_vec(&ct_to_anchor.matrix, flip(d)))
}

fn as_xyz(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array()?;
    if a.len() < 3 {
        return None;
    }
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

fn message(text: impl Into<String>) -> PipelineError {
    PipelineError::Message(text.into())
}

fn missing(what: &str, path: &Path) -> PipelineError {
    message(format!("orientation: missing {what}: {}", path.display()))
}

fn entry(
    status: &str,
    est: Option<&Estimate>,
    roll_deg: Option<f64>,
    candidates_deg: Option<[f64; 2]>,
    failure: Option<&str>,
) -> Value {
    let mut m = Map::new();
    m.insert("status".into(), json!(status));
    m.insert("method".into(), json!(METHOD));
    m.insert("roll_deg".into(), json!(roll_deg));
    m.insert("candidates_deg".into(), json!(candidates_deg));
    m.insert(
        "polar_angle_deg".into(),
        json!(est.map(|e| e.polar_angle_deg)),
    );
    m.insert(
        "marker_slice_mm".into(),
        json!(est.map(|e| e.marker_slice_mm)),
    );
    m.insert(
        "harmonic_amplitude".into(),
        json!(est.map(|e| e.harmonic_amplitude)),
    );
    m.insert(
        "com_offset_mm".into(),
        json!(est.and_then(|e| e.com_offset_mm)),
    );
    let flags: Vec<&str> = est.map(|e| e.flags.clone()).unwrap_or_default();
    m.insert("flags".into(), json!(flags));
    if let Some(text) = failure {
        m.insert("message".into(), json!(text));
    }
    Value::Object(m)
}

enum Plan {
    Run(MarkerSpec),
    Kept,
    NotDirected,
    Unsupported,
    Failed(String),
}

fn side_model<'a>(reco: &'a Value, key: &str, opts: &'a RunOptions) -> Option<&'a str> {
    reco["props"][key]["elmodel"]
        .as_str()
        .or_else(|| reco["meta"]["elmodel"].as_str())
        .or(opts.el_model.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn skip_detail(reco: &Value, opts: &RunOptions, sides: &[(&'static str, Plan, bool)]) -> String {
    if sides.is_empty() {
        return "the reconstruction has no lead sides".to_owned();
    }
    let rings_only = sides.iter().all(|(_, p, _)| matches!(p, Plan::NotDirected));
    if rings_only {
        let mut models: Vec<&str> = Vec::new();
        let mut labelled: Vec<String> = Vec::new();
        for (key, _, _) in sides {
            let model = side_model(reco, key, opts).unwrap_or("unknown model");
            if !models.contains(&model) {
                models.push(model);
            }
            labelled.push(format!("{}: {model}", side_name(key)));
        }
        return match models.as_slice() {
            [one] => format!("{one} has ring contacts only"),
            _ => format!("{}; ring contacts only", labelled.join(", ")),
        };
    }
    sides
        .iter()
        .map(|(key, plan, _)| {
            let label = side_name(key);
            match plan {
                Plan::Failed(why) => format!("{label}: {why}"),
                _ => format!(
                    "{label}: {} has ring contacts only",
                    side_model(reco, key, opts).unwrap_or("unknown model")
                ),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn plan_for(reco: &Value, key: &str, opts: &RunOptions) -> (Plan, bool) {
    let name = side_model(reco, key, opts);
    let Some(name) = name else {
        return (
            Plan::Failed("no electrode model is recorded for this side".into()),
            false,
        );
    };
    let Some(model) = electrode_by_name(name) else {
        return (
            Plan::Failed(format!(
                "electrode model {name:?} is not in the electrode catalogue"
            )),
            false,
        );
    };
    match support(model) {
        Support::NotDirected => (Plan::NotDirected, false),
        Support::UnsupportedModel => (Plan::Unsupported, true),
        Support::Supported => match MarkerSpec::from_model(model) {
            Some(spec) => (Plan::Run(spec), true),
            None => (Plan::Unsupported, true),
        },
    }
}

pub(crate) struct SideUpdate {
    pub(crate) key: &'static str,
    pub(crate) x: [f64; 3],
    pub(crate) y: [f64; 3],
    pub(crate) y_dir: [f64; 3],
    pub(crate) radius_mm: f64,
}

pub(crate) enum Rebuilt {
    Done,
    Vertical,
    NoMarkers,
}

pub(crate) fn rebuild_template_markers(
    markers: &mut Map<String, Value>,
    u: &SideUpdate,
) -> Rebuilt {
    let ht = markers
        .get("head")
        .and_then(as_xyz)
        .zip(markers.get("tail").and_then(as_xyz));
    let Some((head, tail)) = ht else {
        return Rebuilt::NoMarkers;
    };
    let Some((x_dir, y_dir)) = template_marker_directions(u.y_dir, head, tail) else {
        return Rebuilt::Vertical;
    };
    markers.insert("x".into(), json!(add3(head, scale3(x_dir, u.radius_mm))));
    markers.insert("y".into(), json!(add3(head, scale3(y_dir, u.radius_mm))));
    Rebuilt::Done
}

fn kept_entry(reco: &Value, native: &Map<String, Value>, key: &str) -> Value {
    if let Some(previous) = reco["meta"]["orientation"]
        .get(key)
        .filter(|o| o.is_object())
    {
        return previous.clone();
    }
    let m = &native[key]["markers"];
    let roll = m
        .get("head")
        .and_then(as_xyz)
        .zip(m.get("tail").and_then(as_xyz))
        .zip(m.get("y").and_then(as_xyz))
        .map(|((head, tail), y)| {
            let u = unit3(sub3(tail, head));
            roll_of(u, orthonormal_y(sub3(y, head), u))
        });
    match roll {
        Some(r) if r.is_finite() => manual_orientation(r),
        _ => entry("manual", None, None, None, None),
    }
}

fn add_flag(report: &mut Map<String, Value>, key: &str, flag: &str) {
    if let Some(flags) = report
        .get_mut(key)
        .and_then(|v| v.get_mut("flags"))
        .and_then(Value::as_array_mut)
    {
        flags.push(json!(flag));
    }
}

struct CtInput {
    data: Array3<f64>,
    aff: Mat4,
    to_ct: AffineTransform,
    to_anchor: AffineTransform,
}

fn orthonormal_y(y: [f64; 3], u: [f64; 3]) -> [f64; 3] {
    unit3(sub3(y, scale3(u, dot3(y, u))))
}

fn roll_of(u: [f64; 3], y: [f64; 3]) -> f64 {
    roll_azimuth_deg(y).unwrap_or_else(|| roll_from_anterior_deg(u, y))
}

struct SideResult {
    value: Value,
    x: [f64; 3],
    y: [f64; 3],
    y_dir: [f64; 3],
}

fn run_side(
    ct: &CtInput,
    spec: &MarkerSpec,
    head: [f64; 3],
    tail: [f64; 3],
) -> Result<SideResult, String> {
    let u_anchor = unit3(sub3(tail, head));
    let head_ct = anchor_to_ct_point(&ct.to_ct, head);
    let tail_ct = anchor_to_ct_point(&ct.to_ct, tail);
    let est = estimate_marker_orientation(&ct.data, &ct.aff, head_ct, tail_ct, spec)
        .map_err(|e| e.to_string())?;
    let to_anchor = |d: [f64; 3]| orthonormal_y(ct_to_anchor_direction(&ct.to_anchor, d), u_anchor);
    let y_anchor = to_anchor(est.y_dir);
    let x_anchor = cross3(y_anchor, u_anchor);
    let roll = roll_of(u_anchor, y_anchor);
    let candidates = [
        roll_of(u_anchor, to_anchor(est.candidates[0])),
        roll_of(u_anchor, to_anchor(est.candidates[1])),
    ];
    let value = entry(
        est.status.as_str(),
        Some(&est),
        Some(roll),
        Some(candidates),
        None,
    );
    let r = spec.lead_radius_mm;
    Ok(SideResult {
        value,
        x: add3(head, scale3(x_anchor, r)),
        y: add3(head, scale3(y_anchor, r)),
        y_dir: y_anchor,
    })
}

struct LeadSpan<'a> {
    head: [f64; 3],
    tail: [f64; 3],
    spec: &'a MarkerSpec,
}

fn ct_region(grid: &NiftiGrid, segments: &[([f64; 3], [f64; 3])]) -> [Range<usize>; 3] {
    let whole = [0..grid.dims[0], 0..grid.dims[1], 0..grid.dims[2]];
    let Some(inv) = invert_rowmajor(&grid.affine) else {
        return whole;
    };
    if segments.is_empty() {
        return [0..0, 0..0, 0..0];
    }
    let frobenius = (0..3)
        .flat_map(|r| (0..3).map(move |c| (r, c)))
        .map(|(r, c)| inv[r][c] * inv[r][c])
        .sum::<f64>()
        .sqrt();
    let pad = CROP_RADIUS_MM * frobenius + CROP_PAD_VOXELS;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (a, b) in segments {
        for p in [a, b] {
            let ijk = apply_rowmajor(&inv, *p);
            for ax in 0..3 {
                lo[ax] = lo[ax].min(ijk[ax]);
                hi[ax] = hi[ax].max(ijk[ax]);
            }
        }
    }
    let mut out = [0..0, 0..0, 0..0];
    for ax in 0..3 {
        let n = grid.dims[ax] as f64;
        let start = (lo[ax] - pad).floor();
        let end = (hi[ax] + pad).ceil() + 1.0;
        if !(start.is_finite() && end.is_finite()) {
            return whole;
        }
        let (start, end) = (start.clamp(0.0, n) as usize, end.clamp(0.0, n) as usize);
        out[ax] = start..end.max(start);
    }
    out
}

fn load_ct(path: &Path, transform: &Path, leads: &[LeadSpan<'_>]) -> PipelineResult<CtInput> {
    let chain = read_transform_file(transform).map_err(|e| {
        message(format!(
            "orientation: cannot read the coreg transform {}: {e}",
            transform.display()
        ))
    })?;
    let to_ct = match chain.queue().first() {
        Some(TransformElem::Affine(a)) => *a,
        _ => {
            return Err(message(format!(
                "orientation: {} holds no affine transform",
                transform.display()
            )))
        }
    };
    let to_anchor = to_ct.inverse().map_err(|e| {
        message(format!(
            "orientation: the coreg transform {} is not invertible: {e}",
            transform.display()
        ))
    })?;
    let segments: Vec<([f64; 3], [f64; 3])> = leads
        .iter()
        .map(|l| {
            let head = anchor_to_ct_point(&to_ct, l.head);
            let tail = anchor_to_ct_point(&to_ct, l.tail);
            let shaft = sub3(tail, head);
            let u = unit3(shaft);
            let reach = 0.5 * l.spec.marker_len_mm + SLICE_MARGIN_MM;
            let s_lo = (l.spec.marker_pos_mm - reach).min(0.0);
            let s_hi = (l.spec.marker_pos_mm + reach).max(norm3(shaft));
            (add3(head, scale3(u, s_lo)), add3(head, scale3(u, s_hi)))
        })
        .collect();
    let (mut data, grid, offset) = load_volume_region_f64(path, |grid| ct_region(grid, &segments))
        .map_err(|e| {
            message(format!(
                "orientation: cannot read the raw CT {}: {e}",
                path.display()
            ))
        })?;
    data.mapv_inplace(|v| if v.is_finite() { v } else { HU_FLOOR });
    let mut aff = grid.affine;
    let origin = apply_rowmajor(
        &grid.affine,
        [offset[0] as f64, offset[1] as f64, offset[2] as f64],
    );
    for (row, o) in aff.iter_mut().zip(origin) {
        row[3] = o;
    }
    Ok(CtInput {
        data,
        aff,
        to_ct,
        to_anchor,
    })
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::SeqCst))
}

pub fn run_orientation(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    if sub.postop_modality.as_deref() == Some("MRI") {
        return Ok(StageOutcome::skipped(
            "orientation: post-op MRI not supported (specs/06)",
        ));
    }
    let prefs = opts_with_prefs_model(sub, opts, tx);
    let opts = prefs.as_ref().unwrap_or(opts);
    let reco_path = sub.reconstruction_json();
    if !reco_path.is_file() {
        return Err(missing("Reco JSON", &reco_path));
    }
    let text = std::fs::read_to_string(&reco_path).map_err(|e| {
        message(format!(
            "orientation: cannot read the Reco JSON {}: {e}",
            reco_path.display()
        ))
    })?;
    let mut reco: Value = serde_json::from_str(&text).map_err(|e| {
        message(format!(
            "orientation: cannot parse the Reco JSON {}: {e}",
            reco_path.display()
        ))
    })?;
    let Some(native) = reco.get("native").and_then(Value::as_object).cloned() else {
        return Err(missing("native markers in the Reco JSON", &reco_path));
    };

    let mut cleared: Vec<&'static str> = Vec::new();
    let sides: Vec<(&'static str, Plan, bool)> = SIDE_KEYS
        .iter()
        .filter(|k| native.contains_key(**k))
        .map(|k| {
            let (plan, directed) = plan_for(&reco, k, opts);
            let reviewed = directed && roll_review(&reco, k).is_some();
            if reviewed && opts.overwrite_approved {
                cleared.push(*k);
            }
            if reviewed && !opts.overwrite_approved {
                (*k, Plan::Kept, directed)
            } else {
                (*k, plan, directed)
            }
        })
        .collect();
    let n_directed = sides.iter().filter(|(_, _, d)| *d).count();
    if n_directed == 0 {
        return Ok(StageOutcome::skipped(format!(
            "orientation: no directional leads ({})",
            skip_detail(&reco, opts, &sides)
        )));
    }
    let any_kept = sides.iter().any(|(_, p, _)| matches!(p, Plan::Kept));
    let any_run = sides.iter().any(|(_, p, _)| matches!(p, Plan::Run(_)));
    if any_kept && !any_run {
        return Ok(StageOutcome::skipped(
            "orientation: the roll of every directional lead was set or accepted in a manual correction (specs/05 §9a)",
        ));
    }
    if !cleared.is_empty() {
        let names: Vec<&str> = cleared.iter().map(|k| side_name(k)).collect();
        log(
            tx,
            format!(
                "dbsloc: warning: orientation: Rerun all selected steps estimates the roll again; the manually set or accepted roll of the {} lead is replaced",
                names.join(" and ")
            ),
        );
    }

    let ct_path = sub
        .postop_ct_raw
        .clone()
        .filter(|p| p.is_file())
        .ok_or_else(|| {
            let hint = sub
                .postop_ct_raw
                .clone()
                .unwrap_or_else(|| sub.rawdata_dir.join("ses-postop").join("anat"));
            missing("raw CT", &hint)
        })?;
    let transform = crate::derivatives::coreg_ct_affine(sub);
    if !transform.is_file() {
        return Err(missing("coreg transform", &transform));
    }
    log(
        tx,
        format!(
            "[orientation] raw CT {} through {}",
            ct_path.file_name().unwrap_or_default().to_string_lossy(),
            transform.file_name().unwrap_or_default().to_string_lossy()
        ),
    );
    let leads: Vec<LeadSpan<'_>> = sides
        .iter()
        .filter_map(|(key, plan, _)| {
            let Plan::Run(spec) = plan else {
                return None;
            };
            let m = &native[*key]["markers"];
            let head = m.get("head").and_then(as_xyz)?;
            let tail = m.get("tail").and_then(as_xyz)?;
            Some(LeadSpan { head, tail, spec })
        })
        .collect();
    let ct = load_ct(&ct_path, &transform, &leads)?;

    let mut report = Map::new();
    let mut updates: Vec<SideUpdate> = Vec::new();
    let mut n_ok = 0usize;
    for (key, plan, _) in &sides {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        let label = side_name(key);
        let mut value = match plan {
            Plan::Kept => {
                n_ok += 1;
                kept_entry(&reco, &native, key)
            }
            Plan::NotDirected => entry("not_directed", None, None, None, None),
            Plan::Unsupported => entry("unsupported_model", None, None, None, None),
            Plan::Failed(why) => entry("failed", None, None, None, Some(why)),
            Plan::Run(spec) => {
                let markers = &native[*key]["markers"];
                let ht = markers
                    .get("head")
                    .and_then(as_xyz)
                    .zip(markers.get("tail").and_then(as_xyz));
                match ht {
                    None => entry(
                        "failed",
                        None,
                        None,
                        None,
                        Some("no native head / tail markers"),
                    ),
                    Some((head, tail)) => match run_side(&ct, spec, head, tail) {
                        Ok(side) => {
                            n_ok += 1;
                            updates.push(SideUpdate {
                                key,
                                x: side.x,
                                y: side.y,
                                y_dir: side.y_dir,
                                radius_mm: spec.lead_radius_mm,
                            });
                            side.value
                        }
                        Err(why) => entry("failed", None, None, None, Some(&why)),
                    },
                }
            }
        };
        if !matches!(plan, Plan::Kept) {
            if let Some(v) = value.as_object_mut() {
                v.insert("software".into(), crate::version::software_brief());
            }
        }
        log(
            tx,
            format!(
                "[orientation] {label}: {}",
                value["status"].as_str().unwrap_or("")
            ),
        );
        report.insert((*key).to_owned(), value);
    }

    let Some(root) = reco.as_object_mut() else {
        return Err(message("orientation: the Reco JSON is not an object"));
    };
    for u in &updates {
        let slot = root
            .get_mut("native")
            .and_then(|n| n.get_mut(u.key))
            .and_then(|side| side.get_mut("markers"))
            .and_then(Value::as_object_mut);
        let Some(markers) = slot else {
            return Err(message("orientation: the native markers vanished"));
        };
        markers.insert("x".into(), json!(u.x));
        markers.insert("y".into(), json!(u.y));
    }
    let mut rebuilt: Vec<&'static str> = Vec::new();
    for u in &updates {
        match rebuild_native_contacts(root, u.key, opts.el_model.as_deref()) {
            Ok(()) => rebuilt.push(u.key),
            Err(why) => log(
                tx,
                format!(
                    "dbsloc: warning: orientation: native side {}: contacts kept ({why})",
                    u.key
                ),
            ),
        }
    }
    if !updates.is_empty() && root.get("mni").is_some_and(Value::is_object) {
        for u in &updates {
            let target = root
                .get_mut("mni")
                .and_then(|m| m.get_mut(u.key))
                .and_then(|s| s.get_mut("markers"))
                .and_then(Value::as_object_mut);
            let Some(target) = target else {
                return Err(message(format!(
                    "orientation: the Reco JSON has no mni markers for side {}",
                    u.key
                )));
            };
            match rebuild_template_markers(target, u) {
                Rebuilt::Done => {}
                Rebuilt::Vertical => add_flag(&mut report, u.key, FLAG_VERTICAL_MARKER),
                Rebuilt::NoMarkers => {
                    return Err(message(format!(
                        "orientation: the mni markers of side {} lack head / tail",
                        u.key
                    )))
                }
            }
        }
    }
    let meta = root
        .entry("meta")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(meta) = meta.as_object_mut() else {
        return Err(message("orientation: the Reco JSON meta is not an object"));
    };
    meta.insert("orientation".into(), Value::Object(report));
    if let Some(record) = meta
        .get_mut("manual_correction")
        .and_then(Value::as_object_mut)
    {
        for key in &cleared {
            if let Some(r) = record.get_mut(*key).and_then(Value::as_object_mut) {
                r.insert("roll_review".into(), Value::Null);
            }
        }
    }
    let had_reference = root.contains_key("reference");
    let mut out = Value::Object(std::mem::take(root));
    let unwarped = rewarp_native_contacts(&mut out, &rebuilt, sub, opts, cancel, tx)?;
    let mni_policy = |key: &str| {
        if unwarped.contains(&key) {
            WarpedContacts::Omit
        } else {
            WarpedContacts::Keep
        }
    };
    recompute_template_contacts(
        &mut out,
        "mni",
        opts.el_model.as_deref(),
        &mni_policy,
        &mut |line| log(tx, line),
    );
    if had_reference {
        let has_template = opts
            .normalization_template
            .as_ref()
            .is_some_and(|t| t.is_file());
        if has_template {
            if let Some(obj) = out.as_object_mut() {
                obj.remove("reference");
            }
            out = with_space_and_reference(&out, opts, cancel, tx)?;
        } else {
            log(
                tx,
                "dbsloc: warning: orientation: no template to rebuild the reference block; its x / y markers and contacts are updated in place",
            );
            for key in &rebuilt {
                if let Some(side) = out
                    .get_mut("reference")
                    .and_then(|r| r.get_mut(*key))
                    .and_then(Value::as_object_mut)
                {
                    side.remove("coords_mm_warped");
                }
            }
        }
        for u in &updates {
            let target = out
                .get_mut("reference")
                .and_then(|r| r.get_mut(u.key))
                .and_then(|s| s.get_mut("markers"))
                .and_then(Value::as_object_mut);
            if let Some(target) = target {
                rebuild_template_markers(target, u);
            }
        }
        recompute_template_contacts(
            &mut out,
            "reference",
            opts.el_model.as_deref(),
            &|_| WarpedContacts::Keep,
            &mut |line| log(tx, line),
        );
    }
    write_replacing(
        &reco_path,
        (serde_json::to_string_pretty(&out)? + "\n").as_bytes(),
    )?;
    log(
        tx,
        format!(
            "[orientation] written: {}",
            reco_path.file_name().unwrap_or_default().to_string_lossy()
        ),
    );
    crate::reco_mat::export_beside_json(&reco_path, &out, &mut |line| log(tx, line));
    Ok(StageOutcome::done(format!(
        "ok:orientation:{n_ok}/{n_directed}"
    )))
}

fn rewarp_native_contacts(
    out: &mut Value,
    rebuilt: &[&'static str],
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<Vec<&'static str>> {
    let mut unwarped: Vec<&'static str> = rebuilt
        .iter()
        .copied()
        .filter(|key| out["mni"].get(*key).is_some_and(Value::is_object))
        .collect();
    for key in &unwarped {
        if let Some(side) = out
            .get_mut("mni")
            .and_then(|m| m.get_mut(*key))
            .and_then(Value::as_object_mut)
        {
            side.remove("coords_mm_warped");
        }
    }
    let targets: Vec<(&'static str, Vec<[f64; 3]>)> = unwarped
        .iter()
        .filter_map(|key| {
            let rows = out["native"][*key]["coords_mm"].as_array()?;
            let rows: Option<Vec<[f64; 3]>> = rows.iter().map(as_xyz).collect();
            Some((*key, rows?))
        })
        .collect();
    if targets.is_empty() {
        return Ok(unwarped);
    }
    let dropped = |why: &str| {
        log(
            tx,
            format!(
                "dbsloc: warning: orientation: individually mapped template contacts (coords_mm_warped) dropped for the re-oriented side(s) ({why})"
            ),
        );
    };
    let warper = match subject_point_warper(sub, opts, cancel, &mut |line| log(tx, line)) {
        Ok(w) => w,
        Err(e) => {
            dropped(&e.to_string());
            return Ok(unwarped);
        }
    };
    for (key, rows) in targets {
        match warper.to_template(&rows) {
            Ok(warped) if warped.len() == rows.len() => {
                if let Some(side) = out
                    .get_mut("mni")
                    .and_then(|m| m.get_mut(key))
                    .and_then(Value::as_object_mut)
                {
                    side.insert("coords_mm_warped".into(), json!(warped));
                }
                unwarped.retain(|k| *k != key);
            }
            Ok(warped) => dropped(&format!(
                "side {key}: the point warp returned {} point(s) for {}",
                warped.len(),
                rows.len()
            )),
            Err(crate::point_warp::PointWarpError::Cancelled) => {
                return Err(PipelineError::Cancelled)
            }
            Err(e) => dropped(&format!("side {key}: {e}")),
        }
    }
    Ok(unwarped)
}

fn native_model_name(
    root: &Map<String, Value>,
    key: &str,
    fallback: Option<&str>,
) -> Option<String> {
    root.get("props")
        .and_then(|p| p.get(key))
        .and_then(|p| p.get("elmodel"))
        .and_then(Value::as_str)
        .or_else(|| {
            root.get("meta")
                .and_then(|m| m.get("elmodel"))
                .and_then(Value::as_str)
        })
        .or(fallback)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn rebuild_native_contacts(
    root: &mut Map<String, Value>,
    key: &str,
    fallback: Option<&str>,
) -> Result<(), String> {
    let model = native_model_name(root, key, fallback)
        .ok_or_else(|| "no electrode model is recorded for this side".to_owned())?;
    let electrode = electrode_by_name(&model)
        .ok_or_else(|| format!("electrode model {model} is not in the catalogue"))?;
    let side = root
        .get_mut("native")
        .and_then(|n| n.get_mut(key))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "no native side block".to_owned())?;
    let markers = side
        .get("markers")
        .ok_or_else(|| "no native markers".to_owned())?;
    let [head, tail, x, y] =
        ["head", "tail", "x", "y"].map(|name| markers.get(name).and_then(as_xyz));
    let (Some(head), Some(tail), Some(x), Some(y)) = (head, tail, x, y) else {
        return Err("native markers lack head, tail, x or y".to_owned());
    };
    let rows = dbsloc_reconstruct::native_frame::contacts_from_markers(electrode, head, tail, x, y)
        .ok_or_else(|| "the marker frame is degenerate or off the lead axis".to_owned())?;
    side.insert("coords_mm".into(), json!(rows));
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::bids::resolve_subject_with;
    use fastwarp_niftiio::WriterOptions;
    use fastwarp_reg::transform::itk_mat::write_mat_file;
    use nifti::NiftiHeader;
    use std::path::PathBuf;

    const DIMS: (usize, usize, usize) = (40, 36, 30);
    const SPACING: [f64; 3] = [0.45, 0.45, 1.0];
    const ORIGIN: [f64; 3] = [-8.0, -7.0, -12.0];
    const CT_HEAD: [f64; 3] = [0.0, 0.0, -6.0];
    const SHIFT_LPS: [f64; 3] = [2.0, -1.0, 0.5];
    const MNI_SHIFT: [f64; 3] = [10.0, -5.0, 2.0];
    const BOSTON: &str = "Boston Scientific Vercise Directed";

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_orient_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("rawdata/sub-1/ses-preop/anat")).unwrap();
        std::fs::write(
            d.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"),
            b"x",
        )
        .unwrap();
        d
    }

    fn y_truth(roll_deg: f64) -> [f64; 3] {
        let (s, c) = roll_deg.to_radians().sin_cos();
        [-s, c, 0.0]
    }

    fn phantom(roll_deg: f64) -> Array3<f64> {
        let y_true = y_truth(roll_deg);
        let tangent = cross3([0.0, 0.0, 1.0], y_true);
        let model = electrode_by_name(BOSTON).unwrap();
        let (mp, ml) = (model.marker_pos_mm.unwrap(), model.marker_len_mm.unwrap());
        let metal = |p: [f64; 3]| -> bool {
            let q = sub3(p, CT_HEAD);
            let rho_vec = [q[0], q[1], 0.0];
            let rho = dot3(rho_vec, rho_vec).sqrt();
            if !(-3.0..=16.0).contains(&q[2]) {
                return false;
            }
            if rho <= 0.65 {
                return true;
            }
            if (q[2] - mp).abs() <= 0.5 * ml && rho <= 0.80 {
                let cos_a = dot3(rho_vec, y_true) / rho;
                return cos_a.clamp(-1.0, 1.0).acos().to_degrees() < 70.0;
            }
            false
        };
        let offs = [-1.0 / 3.0, 0.0, 1.0 / 3.0];
        Array3::from_shape_fn(DIMS, |(i, j, k)| {
            let p = [
                ORIGIN[0] + i as f64 * SPACING[0],
                ORIGIN[1] + j as f64 * SPACING[1],
                ORIGIN[2] + k as f64 * SPACING[2],
            ];
            let q = sub3(p, CT_HEAD);
            let frac = if (q[0] * q[0] + q[1] * q[1]).sqrt() < 2.5 {
                let mut hits = 0;
                for oi in offs {
                    for oj in offs {
                        for ok in offs {
                            let sp = [
                                p[0] + oi * SPACING[0],
                                p[1] + oj * SPACING[1],
                                p[2] + ok * SPACING[2],
                            ];
                            if metal(sp) {
                                hits += 1;
                            }
                        }
                    }
                }
                hits as f64 / 27.0
            } else {
                0.0
            };
            let streak = if (q[2] - mp).abs() <= 0.5 * ml {
                let across = dot3([q[0], q[1], 0.0], y_true);
                let along = dot3([q[0], q[1], 0.0], tangent);
                -600.0 * (-(across * across) / (2.0 * 0.6 * 0.6)).exp() * (-along.abs() / 5.0).exp()
            } else {
                0.0
            };
            (1.0 - frac) * (30.0 + streak) + frac * 3071.0
        })
    }

    fn write_ct(path: &Path, data: &Array3<f64>, aff: &Mat4) {
        let row = |r: usize| {
            [
                aff[r][0] as f32,
                aff[r][1] as f32,
                aff[r][2] as f32,
                aff[r][3] as f32,
            ]
        };
        let hdr = NiftiHeader {
            sform_code: 1,
            srow_x: row(0),
            srow_y: row(1),
            srow_z: row(2),
            pixdim: [1.0, 0.45, 0.45, 1.0, 1.0, 1.0, 1.0, 1.0],
            ..NiftiHeader::default()
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        WriterOptions::new(path)
            .reference_header(&hdr)
            .write_nifti(data)
            .unwrap();
    }

    fn anchor_head() -> [f64; 3] {
        [
            CT_HEAD[0] + SHIFT_LPS[0],
            CT_HEAD[1] + SHIFT_LPS[1],
            CT_HEAD[2] - SHIFT_LPS[2],
        ]
    }

    fn side_block(head: [f64; 3], shift: [f64; 3], x: [f64; 3], y: [f64; 3]) -> Value {
        let tail = [head[0], head[1], head[2] + 6.0];
        json!({
            "markers": {
                "head": add3(head, shift),
                "tail": add3(tail, shift),
                "x": add3(x, shift),
                "y": add3(y, shift),
            }
        })
    }

    struct Fixture {
        root: PathBuf,
        sub: Subject,
        before: Value,
    }

    fn fixture(name: &str, models: [&str; 2], with_ct: bool, with_mat: bool) -> Fixture {
        let ct = with_ct.then(|| {
            let aff: Mat4 = [
                [SPACING[0], 0.0, 0.0, ORIGIN[0]],
                [0.0, SPACING[1], 0.0, ORIGIN[1]],
                [0.0, 0.0, SPACING[2], ORIGIN[2]],
                [0.0, 0.0, 0.0, 1.0],
            ];
            (phantom(75.0), aff)
        });
        let h = anchor_head();
        let ring_xy = (add3(h, [0.3, 0.0, 0.0]), add3(h, [0.0, 0.3, 0.0]));
        let lead = |shift: [f64; 3]| side_block(h, shift, ring_xy.0, ring_xy.1);
        let native = [lead([0.0; 3]), lead([0.0; 3])];
        let mni = [lead(MNI_SHIFT), lead(MNI_SHIFT)];
        fixture_custom(name, models, ct, native, mni, with_mat)
    }

    fn fixture_custom(
        name: &str,
        models: [&str; 2],
        ct: Option<(Array3<f64>, Mat4)>,
        native: [Value; 2],
        mni: [Value; 2],
        with_mat: bool,
    ) -> Fixture {
        let root = scratch(name);
        if let Some((data, aff)) = &ct {
            write_ct(
                &root.join("rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_CT.nii.gz"),
                data,
                aff,
            );
        }
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        std::fs::create_dir_all(crate::derivatives::coreg_transform_dir(&sub)).unwrap();
        if with_mat {
            write_mat_file(
                &crate::derivatives::coreg_ct_affine(&sub),
                &[TransformElem::Affine(AffineTransform::from_translation(
                    SHIFT_LPS,
                ))],
            )
            .unwrap();
        }
        std::fs::create_dir_all(&sub.recon_dir).unwrap();
        let [native_1, native_2] = native;
        let [mni_1, mni_2] = mni;
        let before = json!({
            "version": 2,
            "subj_id": "1",
            "method": "trac_core_rust_native",
            "mni": {"1": mni_1, "2": mni_2},
            "native": {"1": native_1, "2": native_2},
            "props": {
                "1": {"elmodel": models[0], "manually_corrected": false},
                "2": {"elmodel": models[1], "manually_corrected": false},
            },
            "meta": {
                "orientation": "unresolved",
                "tracer": "trac_core_rust",
                "software": {"version": "0.0.0-fixture", "written_at": "2026-01-01T00:00:00Z"},
            },
        });
        std::fs::write(
            sub.reconstruction_json(),
            serde_json::to_string_pretty(&before).unwrap(),
        )
        .unwrap();
        Fixture { root, sub, before }
    }

    fn run(fx: &Fixture) -> PipelineResult<StageOutcome> {
        let opts = RunOptions::new(&fx.root, "1");
        run_orientation(&fx.sub, &opts, None, None)
    }

    fn reload(fx: &Fixture) -> Value {
        serde_json::from_str(&std::fs::read_to_string(fx.sub.reconstruction_json()).unwrap())
            .unwrap()
    }

    fn token(out: PipelineResult<StageOutcome>) -> String {
        match out.unwrap() {
            StageOutcome::Completed(r) => r,
            StageOutcome::Skipped { result, .. } => result,
        }
    }

    fn len(v: [f64; 3]) -> f64 {
        dot3(v, v).sqrt()
    }

    #[test]
    fn the_cropped_ct_gives_the_same_estimate_as_the_whole_volume() {
        let aff = las_affine(10.0);
        let u = unit3([15.0f64.to_radians().tan(), 20.0f64.to_radians().tan(), 1.0]);
        let lead = truth([12.0, 0.0, -8.0], u, 40.0);
        let data = mirror_phantom(&aff, &[&lead]);
        let head = anchor_of(lead.head);
        let tail = add3(head, scale3(lead.u, 6.0));
        let block = marker_block(head, tail);
        let fx = fixture_custom(
            "crop",
            [BOSTON, BOSTON],
            Some((data, aff)),
            [block.clone(), block.clone()],
            [block.clone(), block],
            true,
        );
        let ct_path = fx.sub.postop_ct_raw.clone().unwrap();
        let transform = crate::derivatives::coreg_ct_affine(&fx.sub);
        let model = electrode_by_name(BOSTON).unwrap();
        let spec = MarkerSpec::from_model(model).unwrap();
        let cropped = load_ct(
            &ct_path,
            &transform,
            &[LeadSpan {
                head,
                tail,
                spec: &spec,
            }],
        )
        .unwrap();
        let (full, aff) = crate::nifti_io::load_volume_f64(&ct_path).unwrap();
        assert!(cropped.data.len() < full.len());
        let h = anchor_to_ct_point(&cropped.to_ct, head);
        let t = anchor_to_ct_point(&cropped.to_ct, tail);
        let a = estimate_marker_orientation(&cropped.data, &cropped.aff, h, t, &spec).unwrap();
        let b = estimate_marker_orientation(&full, &aff, h, t, &spec).unwrap();
        assert_eq!(a.status, b.status);
        assert_eq!(a.flags, b.flags);
        assert!(len(sub3(a.y_dir, b.y_dir)) < 1e-9);
        assert!((a.harmonic_amplitude - b.harmonic_amplitude).abs() < 1e-6);
        assert!((a.marker_slice_mm - b.marker_slice_mm).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn stale_warped_contacts_of_a_reoriented_side_are_dropped_without_a_warp() {
        let fx = fixture("stale_warped", [BOSTON, "Medtronic 3387"], true, true);
        let mut doc = fx.before.clone();
        doc["mni"]["1"]["coords_mm_warped"] = json!([[9.0, 9.0, 9.0]]);
        doc["mni"]["2"]["coords_mm_warped"] = json!([[7.0, 7.0, 7.0]]);
        doc["meta"]["template_contacts"] = json!("from_markers");
        std::fs::write(
            fx.sub.reconstruction_json(),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
        let opts = RunOptions::new(&fx.root, "1");
        let (tx, rx) = mpsc::channel();
        let out = run_orientation(&fx.sub, &opts, None, Some(&tx));
        drop(tx);
        assert_eq!(token(out), "ok:orientation:1/1");
        let after = reload(&fx);
        assert!(after["mni"]["1"].get("coords_mm_warped").is_none());
        assert_eq!(
            after["mni"]["2"]["coords_mm_warped"],
            json!([[7.0, 7.0, 7.0]])
        );
        let lines: Vec<String> = rx
            .iter()
            .filter_map(|e| match e {
                RunEvent::LogLine { line } => Some(line),
                _ => None,
            })
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("coords_mm_warped) dropped")),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn boston_side_is_resolved_and_ring_side_keeps_its_markers() {
        let fx = fixture("boston", [BOSTON, "Medtronic 3387"], true, true);
        assert_eq!(token(run(&fx)), "ok:orientation:1/1");
        let after = reload(&fx);
        let meta = &after["meta"]["orientation"];
        assert_eq!(meta["1"]["status"], "resolved");
        assert_eq!(meta["1"]["method"], "diode_marker_v1");
        assert!(meta["1"]["flags"].as_array().unwrap().is_empty());
        let roll = meta["1"]["roll_deg"].as_f64().unwrap();
        assert!((roll - 75.0).abs() < 3.0, "roll {roll}");
        assert_eq!(meta["1"]["candidates_deg"].as_array().unwrap().len(), 2);
        assert_eq!(meta["2"]["status"], "not_directed");
        assert!(meta["2"]["roll_deg"].is_null());
        assert_eq!(meta["2"]["flags"], json!([]));
        assert_eq!(after["native"]["2"], fx.before["native"]["2"]);
        assert_eq!(after["mni"]["2"], fx.before["mni"]["2"]);
        assert_eq!(after["props"], fx.before["props"]);
        assert_eq!(after["meta"]["tracer"], "trac_core_rust");
        assert_eq!(after["meta"]["software"], fx.before["meta"]["software"]);
        assert_eq!(meta["1"]["software"], crate::version::software_brief());
        assert_eq!(meta["2"]["software"], crate::version::software_brief());

        let m = &after["native"]["1"]["markers"];
        let head = as_xyz(&m["head"]).unwrap();
        let tail = as_xyz(&m["tail"]).unwrap();
        let x = as_xyz(&m["x"]).unwrap();
        let y = as_xyz(&m["y"]).unwrap();
        let u = unit3(sub3(tail, head));
        let (xv, yv) = (sub3(x, head), sub3(y, head));
        assert!(dot3(xv, u).abs() < 1e-9);
        assert!(dot3(yv, u).abs() < 1e-9);
        assert!((len(xv) - 0.65).abs() < 1e-9);
        assert!((len(yv) - 0.65).abs() < 1e-9);
        let expect_x = cross3(unit3(yv), u);
        assert!(len(sub3(unit3(xv), expect_x)) < 1e-9);
        let truth = y_truth(75.0);
        let err = dot3(unit3(yv), truth).clamp(-1.0, 1.0).acos().to_degrees();
        assert!(err < 3.0, "y error {err}");

        let mni = &after["mni"]["1"]["markers"];
        for name in ["x", "y"] {
            let want = add3(as_xyz(&m[name]).unwrap(), MNI_SHIFT);
            let got = as_xyz(&mni[name]).unwrap();
            assert!(len(sub3(got, want)) < 1e-9, "{name}");
        }
        assert_eq!(mni["head"], fx.before["mni"]["1"]["markers"]["head"]);
        let model = dbsloc_reconstruct::electrode_by_name(BOSTON).unwrap();
        let [h, t, x, y] = ["head", "tail", "x", "y"].map(|n| as_xyz(&mni[n]).unwrap());
        let want =
            dbsloc_reconstruct::native_frame::contacts_from_markers(model, h, t, x, y).unwrap();
        let rows = after["mni"]["1"]["coords_mm"].as_array().unwrap();
        assert_eq!(rows.len(), want.len());
        for (row, want) in rows.iter().zip(&want) {
            assert!(len(sub3(as_xyz(row).unwrap(), *want)) < 1e-9);
        }
        assert_eq!(
            after["mni"]["1"]["coords_mm_warped"],
            fx.before["mni"]["1"]["coords_mm"]
        );
        assert_eq!(after["meta"]["template_contacts"], "from_markers");
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    fn seed_native_rows(fx: &Fixture, key: &str, rows: Value) -> Value {
        let mut doc = fx.before.clone();
        doc["native"][key]["coords_mm"] = rows;
        std::fs::write(
            fx.sub.reconstruction_json(),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
        doc
    }

    fn off_axis(p: [f64; 3], head: [f64; 3], u: [f64; 3]) -> f64 {
        let q = sub3(p, head);
        len(sub3(q, scale3(u, dot3(q, u))))
    }

    #[test]
    fn oriented_side_native_contacts_follow_the_rewritten_markers() {
        let fx = fixture("native_rows", [BOSTON, "Medtronic 3387"], true, true);
        let ring_rows = json!([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        let stale = json!(vec![[9.0, 9.0, 9.0]; 8]);
        let seeded = seed_native_rows(&fx, "1", stale);
        let mut doc = seeded.clone();
        doc["native"]["2"]["coords_mm"] = ring_rows.clone();
        std::fs::write(
            fx.sub.reconstruction_json(),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
        assert_eq!(token(run(&fx)), "ok:orientation:1/1");
        let after = reload(&fx);

        assert_eq!(after["native"]["2"]["coords_mm"], ring_rows);
        assert_eq!(after["native"]["2"], doc["native"]["2"]);

        let model = electrode_by_name(BOSTON).unwrap();
        let m = &after["native"]["1"]["markers"];
        let [head, tail, x, y] = ["head", "tail", "x", "y"].map(|n| as_xyz(&m[n]).unwrap());
        let want = dbsloc_reconstruct::native_frame::contacts_from_markers(model, head, tail, x, y)
            .unwrap();
        let rows = after["native"]["1"]["coords_mm"].as_array().unwrap();
        assert_eq!(model.contact_xyz_mm.len(), 8);
        assert_eq!(rows.len(), model.contact_xyz_mm.len());
        for (row, want) in rows.iter().zip(&want) {
            assert!(len(sub3(as_xyz(row).unwrap(), *want)) < 1e-9);
        }
        assert!(after["native"]["1"].get("coords_mm_warped").is_none());
        for name in ["head", "tail"] {
            assert_eq!(
                after["native"]["1"]["markers"][name],
                fx.before["native"]["1"]["markers"][name]
            );
        }

        let r = 0.5 * model.lead_diameter_mm;
        let old = &fx.before["native"]["1"]["markers"];
        let unrolled = |name: &str| {
            let v = sub3(as_xyz(&old[name]).unwrap(), as_xyz(&old["head"]).unwrap());
            add3(head, scale3(unit3(v), r))
        };
        let (unrolled_x, unrolled_y) = (unrolled("x"), unrolled("y"));
        let baseline = dbsloc_reconstruct::native_frame::contacts_from_markers(
            model, head, tail, unrolled_x, unrolled_y,
        )
        .unwrap();
        let u = unit3(sub3(tail, head));
        let moved = rows
            .iter()
            .zip(&baseline)
            .filter(|(row, _)| off_axis(as_xyz(row).unwrap(), head, u) > 0.1)
            .filter(|(row, base)| len(sub3(as_xyz(row).unwrap(), **base)) > 0.1)
            .count();
        assert!(moved > 0);
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn pacer_style_level_rows_become_one_row_per_contact() {
        let fx = fixture("native_levels", [BOSTON, "Medtronic 3387"], true, true);
        let h = anchor_head();
        seed_native_rows(
            &fx,
            "1",
            json!([
                h,
                add3(h, [0.0, 0.0, 2.0]),
                add3(h, [0.0, 0.0, 4.0]),
                add3(h, [0.0, 0.0, 6.0])
            ]),
        );
        assert_eq!(token(run(&fx)), "ok:orientation:1/1");
        let after = reload(&fx);
        let model = electrode_by_name(BOSTON).unwrap();
        assert_eq!(model.contact_xyz_mm.len(), 8);
        assert_eq!(
            after["native"]["1"]["coords_mm"].as_array().unwrap().len(),
            model.contact_xyz_mm.len()
        );
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn sensight_side_is_unsupported_and_keeps_its_markers() {
        let fx = fixture("b33005", [BOSTON, "Medtronic B33005"], true, true);
        assert_eq!(token(run(&fx)), "ok:orientation:1/2");
        let after = reload(&fx);
        assert_eq!(
            after["meta"]["orientation"]["2"]["status"],
            "unsupported_model"
        );
        assert_eq!(after["native"]["2"], fx.before["native"]["2"]);
        assert_eq!(after["mni"]["2"], fx.before["mni"]["2"]);
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn only_ring_leads_skips_without_touching_the_file() {
        let fx = fixture("rings", ["Medtronic 3387", "Medtronic 3389"], false, false);
        let bytes = std::fs::read(fx.sub.reconstruction_json()).unwrap();
        assert_eq!(
            token(run(&fx)),
            "skip:orientation: no directional leads (right: Medtronic 3387, left: Medtronic 3389; ring contacts only)"
        );
        assert_eq!(std::fs::read(fx.sub.reconstruction_json()).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn same_ring_model_on_both_sides_is_named_once() {
        let fx = fixture(
            "rings_same",
            ["Medtronic 3389", "Medtronic 3389"],
            false,
            false,
        );
        assert_eq!(
            token(run(&fx)),
            "skip:orientation: no directional leads (Medtronic 3389 has ring contacts only)"
        );
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn missing_model_is_reported_not_called_ring_only() {
        let fx = fixture("rings_missing", ["", ""], false, false);
        assert_eq!(
            token(run(&fx)),
            "skip:orientation: no directional leads (right: no electrode model is recorded for this side; left: no electrode model is recorded for this side)"
        );
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn missing_model_next_to_a_ring_model_is_worded_per_side() {
        let fx = fixture("rings_mixed", ["Medtronic 3389", ""], false, false);
        let out = token(run(&fx));
        assert!(
            out.starts_with("skip:orientation: no directional leads ("),
            "{out}"
        );
        assert!(
            out.contains("right: Medtronic 3389 has ring contacts only"),
            "{out}"
        );
        assert!(
            out.contains("left: no electrode model is recorded"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn post_op_mri_is_skipped() {
        let root = scratch("mri");
        let anat = root.join("rawdata/sub-1/ses-postop/anat");
        std::fs::create_dir_all(&anat).unwrap();
        std::fs::write(anat.join("sub-1_ses-postop_acq-ax_T1w.nii.gz"), b"x").unwrap();
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        assert_eq!(sub.postop_modality.as_deref(), Some("MRI"));
        let opts = RunOptions::new(&root, "1");
        let out = run_orientation(&sub, &opts, None, None);
        assert_eq!(
            token(out),
            "skip:orientation: post-op MRI not supported (specs/06)"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_inputs_fail_with_the_named_file() {
        let fx = fixture("nomat", [BOSTON, BOSTON], true, false);
        let e = run(&fx).unwrap_err().to_string();
        assert_eq!(
            e,
            format!(
                "orientation: missing coreg transform: {}",
                crate::derivatives::coreg_ct_affine(&fx.sub).display()
            )
        );
        let fx2 = fixture("noct", [BOSTON, BOSTON], false, true);
        let e = run(&fx2).unwrap_err().to_string();
        assert!(e.starts_with("orientation: missing raw CT: "), "{e}");
        std::fs::remove_file(fx2.sub.reconstruction_json()).unwrap();
        let e = run(&fx2).unwrap_err().to_string();
        assert!(e.starts_with("orientation: missing Reco JSON: "), "{e}");
        let _ = std::fs::remove_dir_all(&fx.root);
        let _ = std::fs::remove_dir_all(&fx2.root);
    }

    #[test]
    fn transform_round_trip_and_direction_mapping_agree() {
        let r = {
            let (sa, ca) = 0.7f64.sin_cos();
            let (sb, cb) = (-0.4f64).sin_cos();
            [
                [ca, -sa, 0.0],
                [sa * cb, ca * cb, -sb],
                [sa * sb, ca * sb, cb],
            ]
        };
        let to_ct = AffineTransform::new(r, [3.0, -2.0, 5.0], [10.0, -4.0, 7.0]);
        let to_anchor = to_ct.inverse().unwrap();
        let head = [12.3, -45.6, 7.8];
        let tail = [14.1, -43.0, 18.9];
        for p in [head, tail] {
            let back = ct_to_anchor_point(&to_anchor, anchor_to_ct_point(&to_ct, p));
            assert!(len(sub3(back, p)) < 1e-6);
        }
        let ct_dir = sub3(
            anchor_to_ct_point(&to_ct, tail),
            anchor_to_ct_point(&to_ct, head),
        );
        let mapped = ct_to_anchor_direction(&to_anchor, ct_dir);
        assert!(len(sub3(mapped, sub3(tail, head))) < 1e-9);
    }

    const MIRROR_DIMS: (usize, usize, usize) = (120, 76, 48);

    struct Truth {
        head: [f64; 3],
        u: [f64; 3],
        y: [f64; 3],
    }

    fn rotate_z(v: [f64; 3], deg: f64) -> [f64; 3] {
        let (s, c) = deg.to_radians().sin_cos();
        [c * v[0] - s * v[1], s * v[0] + c * v[1], v[2]]
    }

    fn rodrigues(v: [f64; 3], axis: [f64; 3], deg: f64) -> [f64; 3] {
        let (s, c) = deg.to_radians().sin_cos();
        add3(
            add3(scale3(v, c), scale3(cross3(axis, v), s)),
            scale3(axis, dot3(axis, v) * (1.0 - c)),
        )
    }

    fn truth(head: [f64; 3], u: [f64; 3], roll_deg: f64) -> Truth {
        let anterior = [0.0, 1.0, 0.0];
        let reference = unit3(sub3(anterior, scale3(u, dot3(anterior, u))));
        Truth {
            head,
            u,
            y: rodrigues(reference, u, roll_deg),
        }
    }

    fn las_affine(rotation_deg: f64) -> Mat4 {
        let (s, c) = rotation_deg.to_radians().sin_cos();
        let lin = [
            [-SPACING[0] * c, -SPACING[1] * s, 0.0],
            [-SPACING[0] * s, SPACING[1] * c, 0.0],
            [0.0, 0.0, SPACING[2]],
        ];
        let centre = [
            0.5 * (MIRROR_DIMS.0 - 1) as f64,
            0.5 * (MIRROR_DIMS.1 - 1) as f64,
            0.5 * (MIRROR_DIMS.2 - 1) as f64,
        ];
        let mut aff = [[0.0; 4]; 4];
        for r in 0..3 {
            aff[r][..3].copy_from_slice(&lin[r]);
            aff[r][3] = -(lin[r][0] * centre[0] + lin[r][1] * centre[1] + lin[r][2] * centre[2]);
        }
        aff[3][3] = 1.0;
        aff
    }

    fn mirror_phantom(aff: &Mat4, leads: &[&Truth]) -> Array3<f64> {
        let model = electrode_by_name(BOSTON).unwrap();
        let (mp, ml) = (model.marker_pos_mm.unwrap(), model.marker_len_mm.unwrap());
        let n = unit3([aff[0][2], aff[1][2], aff[2][2]]);
        let world = |i: f64, j: f64, k: f64| {
            [
                aff[0][0] * i + aff[0][1] * j + aff[0][2] * k + aff[0][3],
                aff[1][0] * i + aff[1][1] * j + aff[1][2] * k + aff[1][3],
                aff[2][0] * i + aff[2][1] * j + aff[2][2] * k + aff[2][3],
            ]
        };
        let metal_at = |l: &Truth, p: [f64; 3]| -> bool {
            let q = sub3(p, l.head);
            let z = dot3(q, l.u);
            let rho_vec = sub3(q, scale3(l.u, z));
            let rho = len(rho_vec);
            if !(-3.0..=16.0).contains(&z) {
                return false;
            }
            if rho <= 0.65 {
                return true;
            }
            if (z - mp).abs() <= 0.5 * ml && rho <= 0.80 {
                let cos_a = dot3(rho_vec, l.y) / rho;
                return cos_a.clamp(-1.0, 1.0).acos().to_degrees() < 70.0;
            }
            false
        };
        let offs = [-1.0 / 3.0, 0.0, 1.0 / 3.0];
        Array3::from_shape_fn(MIRROR_DIMS, |(i, j, k)| {
            let (fi, fj, fk) = (i as f64, j as f64, k as f64);
            let p = world(fi, fj, fk);
            let mut near = false;
            let mut streak = 0.0;
            for l in leads {
                let q = sub3(p, l.head);
                let rho = len(sub3(q, scale3(l.u, dot3(q, l.u))));
                near |= rho < 2.5;
                if rho < 8.0 {
                    let proj = sub3(l.y, scale3(l.u, dot3(l.y, n) / dot3(l.u, n)));
                    let d_true = unit3(proj);
                    let tangent = unit3(cross3(n, d_true));
                    let s = dot3(q, n) / dot3(l.u, n);
                    let w = sub3(q, scale3(l.u, s));
                    if (s - mp).abs() <= 0.5 * ml {
                        let across = dot3(w, d_true);
                        let along = dot3(w, tangent);
                        streak -= 600.0
                            * (-(across * across) / (2.0 * 0.6 * 0.6)).exp()
                            * (-along.abs() / 5.0).exp();
                    }
                }
            }
            let frac = if near {
                let mut hits = 0;
                for oi in offs {
                    for oj in offs {
                        for ok in offs {
                            let sp = world(fi + oi, fj + oj, fk + ok);
                            if leads.iter().any(|l| metal_at(l, sp)) {
                                hits += 1;
                            }
                        }
                    }
                }
                hits as f64 / 27.0
            } else {
                0.0
            };
            (1.0 - frac) * (30.0 + streak) + frac * 3071.0
        })
    }

    fn anchor_of(ct_point: [f64; 3]) -> [f64; 3] {
        add3(ct_point, [SHIFT_LPS[0], SHIFT_LPS[1], -SHIFT_LPS[2]])
    }

    fn marker_block(head: [f64; 3], tail: [f64; 3]) -> Value {
        json!({"markers": {
            "head": head,
            "tail": tail,
            "x": add3(head, [0.3, 0.0, 0.0]),
            "y": add3(head, [0.0, 0.3, 0.0]),
        }})
    }

    fn angle_between(a: [f64; 3], b: [f64; 3]) -> f64 {
        dot3(unit3(a), unit3(b))
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    }

    fn roll_gap(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    fn mirror_run(rotation_deg: f64, right_roll: f64, left_roll: f64) {
        let aff = las_affine(rotation_deg);
        let u_right = unit3([15.0f64.to_radians().tan(), 20.0f64.to_radians().tan(), 1.0]);
        let right = truth([12.0, 0.0, -8.0], u_right, right_roll);
        let left = truth(
            [-12.0, 0.0, -8.0],
            [-u_right[0], u_right[1], u_right[2]],
            left_roll,
        );
        let data = mirror_phantom(&aff, &[&right, &left]);
        let mut native = Vec::new();
        let mut mni = Vec::new();
        for lead in [&right, &left] {
            let head = anchor_of(lead.head);
            let tail = add3(head, scale3(lead.u, 6.0));
            native.push(marker_block(head, tail));
            let mni_head = add3(head, MNI_SHIFT);
            let mni_tail = add3(mni_head, rotate_z(sub3(tail, head), 30.0));
            mni.push(marker_block(mni_head, mni_tail));
        }
        let to_pair = |v: Vec<Value>| -> [Value; 2] { [v[0].clone(), v[1].clone()] };
        let fx = fixture_custom(
            "mirror",
            [BOSTON, BOSTON],
            Some((data, aff)),
            to_pair(native),
            to_pair(mni),
            true,
        );
        assert_eq!(token(run(&fx)), "ok:orientation:2/2");
        let after = reload(&fx);
        let model = electrode_by_name(BOSTON).unwrap();
        let radius = 0.5 * model.lead_diameter_mm;
        let mut rolls = Vec::new();
        for (key, lead) in [("1", &right), ("2", &left)] {
            let meta = &after["meta"]["orientation"][key];
            assert_eq!(meta["status"], "resolved", "side {key}");
            let roll = meta["roll_deg"].as_f64().unwrap();
            let want = roll_azimuth_deg(lead.y).unwrap();
            assert!(
                roll_gap(roll, want) <= 3.0,
                "side {key} rotation {rotation_deg}: got {roll:.1} want {want:.1}"
            );
            rolls.push(roll);

            let nm = &after["native"][key]["markers"];
            let head = as_xyz(&nm["head"]).unwrap();
            assert!(len(sub3(head, anchor_of(lead.head))) < 1e-9);
            let y_dir = sub3(as_xyz(&nm["y"]).unwrap(), head);
            assert!(angle_between(y_dir, lead.y) <= 3.0, "side {key} native y");
            assert!((len(y_dir) - radius).abs() < 1e-9);

            let tm = &after["mni"][key]["markers"];
            let (h, t) = (as_xyz(&tm["head"]).unwrap(), as_xyz(&tm["tail"]).unwrap());
            let shaft = unit3(sub3(t, h));
            let x_dir = sub3(as_xyz(&tm["x"]).unwrap(), h);
            let y_tpl = sub3(as_xyz(&tm["y"]).unwrap(), h);
            assert!(dot3(x_dir, shaft).abs() < 1e-9);
            assert!(dot3(y_tpl, shaft).abs() < 1e-9);
            assert!((len(x_dir) - radius).abs() < 1e-9);
            assert!(len(sub3(unit3(y_tpl), cross3(shaft, unit3(x_dir)))) < 1e-9);
            let turned = [y_dir[1], -y_dir[0]];
            let want_az = turned[1].atan2(turned[0]).to_degrees();
            let got_az = x_dir[1].atan2(x_dir[0]).to_degrees();
            assert!(
                roll_gap(got_az, want_az) <= 12.0,
                "side {key} template azimuth {got_az:.1} vs {want_az:.1}"
            );
        }
        assert!(rolls[0] > 0.0 && rolls[1] < 0.0, "{rolls:?}");
        if (right_roll + left_roll).abs() < 1e-9 {
            assert!((rolls[0] + rolls[1]).abs() <= 3.0, "{rolls:?}");
        }
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn mirrored_leads_in_a_las_ct_are_resolved_per_side_through_the_stage() {
        for rotation in [0.0, 10.0] {
            mirror_run(rotation, 40.0, -40.0);
            mirror_run(rotation, 40.0, -110.0);
        }
    }
}
