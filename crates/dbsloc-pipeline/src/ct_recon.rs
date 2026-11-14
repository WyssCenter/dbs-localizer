// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ndarray::Array3;
use serde_json::{json, Map, Value};
use thiserror::Error;

#[cfg(feature = "pacer")]
use dbsloc_pacer::{ct_volume_from_ijk, pacer_leads, PacerConfig, PacerLead, PacerSide};
use dbsloc_reconstruct::metal_axis::{LEAD_BODY_HU, STEEP_CONTINUITY_MM};
#[cfg(feature = "pacer")]
use dbsloc_reconstruct::native_frame::trajectory_from_anchors;
use dbsloc_reconstruct::native_frame::{
    frame_lattice, implausible_message, rigid_frame_from_points, trace_native_sides,
    world_bounds_corners, ChosenSide, NativeFrame, NativeSides, NativeTraceError,
    NativeTraceParams, SideGeometry, SideSource, FRAME_CENTRAL_FRACTION, FRAME_LATTICE_PER_AXIS,
};
use dbsloc_reconstruct::orientation::FLAG_VERTICAL_MARKER;
use dbsloc_reconstruct::{
    check_in_brain_mask, check_tilt, electrode_by_name, ElectrodeGeometry, TracConfig,
    BRAIN_MASK_MARGIN_MM, MAX_TILT_FROM_VERTICAL_DEG,
};
#[cfg(feature = "pacer")]
use dbsloc_reconstruct::{check_side, PlausibilityLimits, WorldBounds};
use fastwarp_core::Mat4;

use crate::bids::simplify_verbatim;
use crate::bundled_bin::find_binary;
#[cfg(feature = "pacer")]
use crate::nifti_io::read_grid;
use crate::nifti_io::{load_mask, load_volume_f64};
use crate::point_warp::{
    composite_point_transform_tokens, AntsPointWarp, PointWarp, PointWarpError, RustPointWarp,
};
use crate::template_contacts::{recompute_template_contacts, WarpedContacts};

const METHOD_NATIVE_FRAME: &str = "trac_core_rust_native";
pub const TRACER_TRAC_CORE: &str = "trac_core_rust";
pub const SIDE_RIGHT: &str = "1";
pub const SIDE_LEFT: &str = "2";
pub const SIDE_KEYS: [&str; 2] = [SIDE_RIGHT, SIDE_LEFT];
pub const SIDE_KEYS_CONVENTION: &str = "1=right,2=left";
pub const ORIENTATION_UNRESOLVED: &str =
    "unresolved: x/y markers are an arbitrary in-plane basis; \
     segment azimuth needs the orientation stage (specs/06)";
const PACER_IMPORT_ENV: &str = "DBSLOC_PACER_RECO_JSON";
pub const BRAIN_MASK_NAME: &str = "brainmask.nii.gz";
#[cfg(feature = "pacer")]
const MIDLINE_MARGIN_MM: f64 = 2.0;
pub const PACER_AVAILABLE: bool = cfg!(feature = "pacer");
pub const PACER_UNAVAILABLE: &str = "E_RECON_pacer_unavailable: the reconstruction method pacer \
     (--recon-method pacer) needs the optional PaCER component (licensed AGPL-3.0-only), which \
     this build does not include; use auto or trac_core";

pub fn pacer_method_unavailable(method: &str) -> bool {
    method.trim().eq_ignore_ascii_case("pacer") && !PACER_AVAILABLE
}

pub fn pacer_unavailable(opts: &crate::options::RunOptions) -> Option<&'static str> {
    let reconstructs = opts.stages.contains(&crate::stages::Stage::Reconstruct);
    (reconstructs && pacer_method_unavailable(&opts.recon_method)).then_some(PACER_UNAVAILABLE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtMethod {
    Auto,
    TracCore,
    TracCoreNative,
    Pacer,
}

impl CtMethod {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Some(CtMethod::Auto),
            "trac_core" | "trac" | "core" => Some(CtMethod::TracCore),
            "trac_core_native" => Some(CtMethod::TracCoreNative),
            "pacer" => Some(CtMethod::Pacer),
            _ => None,
        }
    }
}

/// How native points reach template space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WarpRoute {
    /// `antsApplyTransforms` on a coordinate image.
    #[default]
    Ants,
    /// `fastwarp-reg` when the inverse composite is readable, otherwise `Ants`.
    Auto,
    /// `fastwarp-reg`; an unreadable inverse composite is an error.
    Rust,
}

#[derive(Clone, Debug)]
pub struct CtReconRequest {
    pub subj_id: String,
    pub method: CtMethod,
    pub ct: Option<PathBuf>,
    pub template: Option<PathBuf>,
    pub norm_inverse_composite: Option<PathBuf>,
    pub el_model: Option<String>,
    pub el_model_error: Option<String>,
    pub metal_dark: bool,
    pub distal_bloom_mm: Option<f64>,
    pub ants_apply: Option<PathBuf>,
    pub ants_threads: Option<u32>,
    pub pacer_import: Option<PathBuf>,
    pub warp_route: WarpRoute,
    pub world_to_reference: Option<Mat4>,
    pub brain_masks: Vec<PathBuf>,
    pub skip_sides: Vec<String>,
}

impl CtReconRequest {
    pub fn new(subj_id: impl Into<String>, method: CtMethod) -> Self {
        CtReconRequest {
            subj_id: subj_id.into(),
            method,
            ct: None,
            template: None,
            norm_inverse_composite: None,
            el_model: None,
            el_model_error: None,
            metal_dark: true,
            distal_bloom_mm: None,
            ants_apply: None,
            ants_threads: None,
            pacer_import: None,
            warp_route: WarpRoute::Ants,
            world_to_reference: None,
            brain_masks: Vec::new(),
            skip_sides: Vec::new(),
        }
    }

    fn skips(&self, key: &str) -> bool {
        self.skip_sides.iter().any(|k| k == key)
    }
}

#[derive(Debug, Error)]
pub enum CtReconError {
    #[error("E_RECON_native: {0}")]
    Blocked(String),
    #[error("{0}")]
    Input(String),
    #[error("{0}")]
    Electrode(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Trace(String),
    #[error("{}", implausible_message(.0))]
    Implausible(Vec<String>),
    #[error("{0}")]
    Warp(String),
}

impl CtReconError {
    pub fn is_os_error(&self) -> bool {
        matches!(
            self,
            CtReconError::Blocked(_) | CtReconError::Input(_) | CtReconError::Warp(_)
        )
    }
}

fn warp_err(stage: &str, e: &PointWarpError) -> CtReconError {
    if matches!(e, PointWarpError::Cancelled) {
        return CtReconError::Warp(format!("E_RECON_warp: {stage}: {e}"));
    }
    CtReconError::Warp(format!("E_RECON_warp: {stage}: {}: {e}", e.python_kind()))
}

pub fn sanitize_path(p: &Path) -> Result<PathBuf, CtReconError> {
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(CtReconError::Input(format!(
            "E_RECON_input: path {} contains a '..' component",
            p.display()
        )));
    }
    if let Ok(c) = std::fs::canonicalize(p) {
        return Ok(simplify_verbatim(c));
    }
    match (p.parent(), p.file_name()) {
        (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => {
            match std::fs::canonicalize(dir) {
                Ok(d) => Ok(simplify_verbatim(d).join(name)),
                Err(_) => Ok(p.to_path_buf()),
            }
        }
        _ => Ok(p.to_path_buf()),
    }
}

fn sanitize_opt(p: &Option<PathBuf>) -> Result<Option<PathBuf>, CtReconError> {
    p.as_deref().map(sanitize_path).transpose()
}

pub fn sanitize_request(req: &CtReconRequest) -> Result<CtReconRequest, CtReconError> {
    Ok(CtReconRequest {
        ct: sanitize_opt(&req.ct)?,
        template: sanitize_opt(&req.template)?,
        norm_inverse_composite: sanitize_opt(&req.norm_inverse_composite)?,
        ants_apply: sanitize_opt(&req.ants_apply)?,
        pacer_import: sanitize_opt(&req.pacer_import)?,
        ..req.clone()
    })
}

fn native_blocker(req: &CtReconRequest) -> Option<String> {
    let Some(tpl) = req.template.as_ref() else {
        return Some("no template given (--template), so points cannot be warped to it".into());
    };
    if !tpl.is_file() {
        return Some(format!(
            "template {} does not exist, so points cannot be warped to it",
            tpl.display()
        ));
    }
    if !req.ct.as_ref().is_some_and(|p| p.is_file()) {
        return Some("no coregistered post-op CT (run coreg first)".into());
    }
    let has_tokens = req
        .norm_inverse_composite
        .as_ref()
        .and_then(|p| composite_point_transform_tokens(p))
        .is_some();
    if !has_tokens {
        return Some("no native->template transform from the normalize stage".into());
    }
    None
}

fn resolve_electrode(req: &CtReconRequest) -> Result<&'static ElectrodeGeometry, CtReconError> {
    let Some(name) = req
        .el_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(CtReconError::Electrode(
            req.el_model_error.clone().unwrap_or_else(|| {
                "E_RECON_elmodel: no electrode model given; pass --el-model '<name>'".to_owned()
            }),
        ));
    };
    electrode_by_name(name).ok_or_else(|| {
        CtReconError::Electrode(format!(
            "E_RECON_elmodel: {name:?} is not in the electrode catalogue ({} known)",
            dbsloc_reconstruct::electrode_model_names().len()
        ))
    })
}

fn ants_point_warp<'a>(
    req: &CtReconRequest,
    cancel: Option<&'a AtomicBool>,
) -> Result<AntsPointWarp<'a>, CtReconError> {
    let template = req.template.clone().ok_or_else(|| {
        CtReconError::Blocked(
            "no template given (--template), so points cannot be warped to it".into(),
        )
    })?;
    let tokens = req
        .norm_inverse_composite
        .as_ref()
        .and_then(|p| composite_point_transform_tokens(p))
        .ok_or_else(|| {
            CtReconError::Blocked("no native->template transform from the normalize stage".into())
        })?;
    Ok(AntsPointWarp {
        exe: req
            .ants_apply
            .clone()
            .filter(|p| p.is_file())
            .or_else(|| find_binary("antsApplyTransforms")),
        template,
        tokens,
        threads: req.ants_threads,
        cancel,
    })
}

pub fn point_warper<'a>(
    req: &CtReconRequest,
    cancel: Option<&'a AtomicBool>,
    log: &mut dyn FnMut(String),
) -> Result<Box<dyn PointWarp + 'a>, CtReconError> {
    if req.warp_route != WarpRoute::Ants {
        if let Some(file) = req.norm_inverse_composite.as_ref() {
            match RustPointWarp::from_file(file) {
                Ok(w) => {
                    log("[reconstruct] point warp: fastwarp-reg through InverseComposite.h5".into());
                    return Ok(Box::new(w));
                }
                Err(e) if req.warp_route == WarpRoute::Rust => {
                    return Err(CtReconError::Warp(format!(
                        "E_RECON_warp: native to template: {e}"
                    )));
                }
                Err(e) => log(format!(
                    "dbsloc: note: reconstruct: InverseComposite.h5 is not readable by fastwarp-reg ({e}); the point warp uses antsApplyTransforms"
                )),
            }
        }
    }
    Ok(Box::new(ants_point_warp(req, cancel)?))
}

pub fn reconstruct_ct(
    req: &CtReconRequest,
    cancel: Option<&AtomicBool>,
    log: &mut dyn FnMut(String),
) -> Result<Value, CtReconError> {
    let mut reco = reconstruct_ct_unstamped(req, cancel, log)?;
    stamp_software(&mut reco);
    Ok(reco)
}

/// `meta.software`: the build that created the Reco v2 file and when.
pub(crate) fn stamp_software(reco: &mut Value) {
    if let Some(meta) = reco
        .as_object_mut()
        .map(|o| o.entry("meta").or_insert_with(|| Value::Object(Map::new())))
        .and_then(Value::as_object_mut)
    {
        meta.insert("software".into(), crate::version::software_stamp());
    }
}

fn reconstruct_ct_unstamped(
    req: &CtReconRequest,
    cancel: Option<&AtomicBool>,
    log: &mut dyn FnMut(String),
) -> Result<Value, CtReconError> {
    let req = &sanitize_request(req)?;
    if req.method == CtMethod::Pacer && !PACER_AVAILABLE {
        return Err(CtReconError::Invalid(PACER_UNAVAILABLE.to_owned()));
    }
    if matches!(req.method, CtMethod::Auto | CtMethod::Pacer) {
        if let Some(v) = pacer_import(req, log)? {
            log("[reconstruct] honouring a pre-exported PaCER Reco v2 JSON".into());
            return Ok(v);
        }
    }
    if let Some(b) = native_blocker(req) {
        return Err(CtReconError::Blocked(b));
    }
    let electrode = resolve_electrode(req)?;
    let warper = point_warper(req, cancel, log)?;
    match req.method {
        #[cfg(feature = "pacer")]
        CtMethod::Pacer => pacer_reco(req, electrode, warper.as_ref(), log),
        #[cfg(not(feature = "pacer"))]
        CtMethod::Pacer => Err(CtReconError::Invalid(PACER_UNAVAILABLE.to_owned())),
        _ => native_frame_reco(req, electrode, warper.as_ref(), log),
    }
}

pub(crate) fn side_json(g: &SideGeometry) -> Value {
    json!({
        "coords_mm": g.coords_mm,
        "trajectory": g.trajectory,
        "markers": {"head": g.head, "tail": g.tail, "x": g.x, "y": g.y},
        "trajvector": g.trajvector,
    })
}

fn electrode_meta(meta: &mut Map<String, Value>, e: &ElectrodeGeometry) {
    meta.insert("lead_diameter_mm".into(), json!(e.lead_diameter_mm));
    meta.insert("eldist_mm".into(), json!(e.eldist_mm));
    meta.insert("elmodel".into(), json!(e.elmodel));
    meta.insert("n_contacts".into(), json!(e.n_contacts));
    meta.insert("level_z_mm".into(), json!(e.level_z_mm));
    meta.insert("contact_level".into(), json!(e.contact_level));
    meta.insert("is_directed".into(), json!(e.is_directed));
}

fn props_json(elmodel: &str, keys: &[&str]) -> Value {
    let mut m = Map::new();
    for k in keys {
        m.insert(
            (*k).to_owned(),
            json!({"elmodel": elmodel, "manually_corrected": false}),
        );
    }
    Value::Object(m)
}

fn mat4_json(m: &Mat4) -> Value {
    json!(m.iter().map(|r| r.to_vec()).collect::<Vec<_>>())
}

fn as_xyz(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array()?;
    if a.len() < 3 {
        return None;
    }
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

enum Slot {
    Row(&'static str, &'static str, usize),
    Marker(&'static str, &'static str),
}

fn xyz_or_err(v: &Value, what: &str) -> Result<[f64; 3], PointWarpError> {
    as_xyz(v)
        .ok_or_else(|| PointWarpError::BadPoint(format!("{what} is not an x, y, z triple: {v}")))
}

pub fn warp_sides(
    sides: &Map<String, Value>,
    warper: &dyn PointWarp,
) -> Result<Map<String, Value>, PointWarpError> {
    warp_sides_reporting(sides, warper).map(|(out, _)| out)
}

pub(crate) fn warp_sides_reporting(
    sides: &Map<String, Value>,
    warper: &dyn PointWarp,
) -> Result<(Map<String, Value>, Vec<&'static str>), PointWarpError> {
    let mut pts: Vec<[f64; 3]> = Vec::new();
    let mut layout: Vec<Slot> = Vec::new();
    for key in SIDE_KEYS {
        let Some(blk) = sides.get(key) else {
            continue;
        };
        let blk = blk
            .as_object()
            .ok_or_else(|| PointWarpError::BadPoint(format!("side {key} is not an object")))?;
        for field in ["coords_mm", "trajectory"] {
            let Some(rows) = blk.get(field) else {
                continue;
            };
            let rows = rows.as_array().ok_or_else(|| {
                PointWarpError::BadPoint(format!("side {key} {field} is not a list"))
            })?;
            for (i, r) in rows.iter().enumerate() {
                pts.push(xyz_or_err(r, &format!("side {key} {field}[{i}]"))?);
                layout.push(Slot::Row(key, field, i));
            }
        }
        if let Some(m) = blk.get("markers") {
            let m = m.as_object().ok_or_else(|| {
                PointWarpError::BadPoint(format!("side {key} markers is not an object"))
            })?;
            for name in ["head", "tail", "x", "y"] {
                if let Some(v) = m.get(name) {
                    pts.push(xyz_or_err(v, &format!("side {key} marker {name}"))?);
                    layout.push(Slot::Marker(key, name));
                }
            }
        }
    }
    let warped = warper.to_template(&pts)?;
    if warped.len() != pts.len() {
        return Err(PointWarpError::Count {
            got: warped.len(),
            expected: pts.len(),
        });
    }
    let mut out = Map::new();
    for key in SIDE_KEYS {
        if let Some(blk) = sides.get(key) {
            out.insert(key.to_owned(), blk.clone());
        }
    }
    for (slot, p) in layout.iter().zip(warped.iter()) {
        let target = match slot {
            Slot::Row(key, field, i) => out
                .get_mut(*key)
                .and_then(|b| b.get_mut(*field))
                .and_then(|r| r.get_mut(*i)),
            Slot::Marker(key, name) => out
                .get_mut(*key)
                .and_then(|b| b.get_mut("markers"))
                .and_then(|m| m.get_mut(*name)),
        };
        match target {
            Some(t) => *t = json!(p),
            None => {
                return Err(PointWarpError::BadPoint(
                    "warped point has no slot to go back to".into(),
                ))
            }
        }
    }
    let vertical = crate::template_contacts::construct_template_markers(&mut out, sides);
    for key in SIDE_KEYS {
        let Some(blk) = out.get_mut(key).and_then(Value::as_object_mut) else {
            continue;
        };
        let ht = blk
            .get("markers")
            .and_then(|m| Some((as_xyz(m.get("head")?)?, as_xyz(m.get("tail")?)?)));
        if let Some((h, t)) = ht {
            let v = [t[0] - h[0], t[1] - h[1], t[2] - h[2]];
            let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if n > 1e-9 {
                blk.insert("trajvector".into(), json!([v[0] / n, v[1] / n, v[2] / n]));
            }
        }
    }
    Ok((out, vertical))
}

fn warp_to_template(
    native: &Map<String, Value>,
    warper: &dyn PointWarp,
    log: &mut dyn FnMut(String),
) -> Result<(Map<String, Value>, Vec<&'static str>), CtReconError> {
    if native.is_empty() {
        return Ok((Map::new(), Vec::new()));
    }
    let (mni, vertical) =
        warp_sides_reporting(native, warper).map_err(|e| warp_err("skipped", &e))?;
    for key in &vertical {
        log(format!(
            "dbsloc: warning: reconstruct: mni side {key}: {FLAG_VERTICAL_MARKER}: the native segment direction has no horizontal part; template x / y kept as individually mapped points"
        ));
    }
    Ok((mni, vertical))
}

pub const SIDE_TRACED: &str = "traced";
pub const SIDE_KEPT: &str = "kept";
pub const SIDE_FAILED: &str = "failed";

pub(crate) fn reconstruct_meta(
    status: &[(&str, &str)],
    failed: &[(&str, String)],
    flags: &[(&str, Vec<&str>)],
) -> Value {
    let mut st = Map::new();
    for key in SIDE_KEYS {
        if let Some((_, v)) = status.iter().find(|(k, _)| *k == key) {
            st.insert(key.to_owned(), json!(v));
        }
    }
    let mut out = Map::new();
    out.insert("status".into(), Value::Object(st));
    if !failed.is_empty() {
        let mut f = Map::new();
        for key in SIDE_KEYS {
            if let Some((_, why)) = failed.iter().find(|(k, _)| *k == key) {
                f.insert(key.to_owned(), json!(why));
            }
        }
        out.insert("failed".into(), Value::Object(f));
    }
    let mut fl = Map::new();
    for key in SIDE_KEYS {
        if let Some((_, v)) = flags.iter().find(|(k, _)| *k == key) {
            fl.insert(key.to_owned(), json!(v));
        }
    }
    out.insert("flags".into(), Value::Object(fl));
    Value::Object(out)
}

fn side_flags<'a>(keys: &[&'a str], vertical: &[&str]) -> Vec<(&'a str, Vec<&'static str>)> {
    keys.iter()
        .map(|k| {
            let flags = if vertical.contains(k) {
                vec![FLAG_VERTICAL_MARKER]
            } else {
                Vec::new()
            };
            (*k, flags)
        })
        .collect()
}

fn label_key(label: &str) -> &'static str {
    if label == "right" {
        SIDE_RIGHT
    } else {
        SIDE_LEFT
    }
}

fn load_brain_mask(template: &Path) -> Result<Option<(Array3<bool>, Mat4)>, CtReconError> {
    let Some(dir) = template.parent() else {
        return Ok(None);
    };
    let p = dir.join(BRAIN_MASK_NAME);
    if !p.is_file() {
        return Ok(None);
    }
    let mask = load_mask(&p, 0.5).map_err(|e| CtReconError::Input(e.to_string()))?;
    Ok(Some(mask))
}

fn template_problems(
    mni: &Map<String, Value>,
    labelled_keys: &[(&str, &str)],
    template: &Path,
) -> Result<Vec<String>, CtReconError> {
    let mask = load_brain_mask(template)?;
    let mut out = Vec::new();
    for (label, key) in labelled_keys {
        let Some(blk) = mni.get(*key) else {
            continue;
        };
        let m = &blk["markers"];
        if let (Some(h), Some(t)) = (as_xyz(&m["head"]), as_xyz(&m["tail"])) {
            if let Some(msg) = check_tilt(h, t, MAX_TILT_FROM_VERTICAL_DEG) {
                out.push(format!("{label} (mni): {msg}"));
            }
        }
        if let Some((mask, aff)) = mask.as_ref() {
            let coords: Vec<[f64; 3]> = blk["coords_mm"]
                .as_array()
                .map(|rows| rows.iter().filter_map(as_xyz).collect())
                .unwrap_or_default();
            for msg in check_in_brain_mask(&coords, mask, aff, BRAIN_MASK_MARGIN_MM) {
                out.push(format!("{label} (mni): {msg}"));
            }
        }
    }
    Ok(out)
}

fn fit_frame(
    aff: &Mat4,
    dims: [usize; 3],
    warper: &dyn PointWarp,
) -> Result<NativeFrame, CtReconError> {
    let (lo, hi) = world_bounds_corners(aff, dims);
    let lattice = frame_lattice(lo, hi, FRAME_LATTICE_PER_AXIS, FRAME_CENTRAL_FRACTION);
    let warped = warper
        .to_template(&lattice)
        .map_err(|e| warp_err("frame fit", &e))?;
    rigid_frame_from_points(&lattice, &warped)
        .map_err(|e| CtReconError::Warp(format!("E_RECON_warp: frame fit: {e}")))
}

fn side_meta(s: &ChosenSide) -> Value {
    json!({
        "continuity": s.continuity,
        "continuity_mm": STEEP_CONTINUITY_MM,
        "threshold_hu": LEAD_BODY_HU,
        "seeds": {"ransac_voxels": s.ransac_voxels},
    })
}

pub fn native_frame_reco(
    req: &CtReconRequest,
    electrode: &ElectrodeGeometry,
    warper: &dyn PointWarp,
    log: &mut dyn FnMut(String),
) -> Result<Value, CtReconError> {
    native_frame_reco_with(req, electrode, warper, None, log)
}

fn native_frame_reco_with(
    req: &CtReconRequest,
    electrode: &ElectrodeGeometry,
    warper: &dyn PointWarp,
    fixed_frame: Option<NativeFrame>,
    log: &mut dyn FnMut(String),
) -> Result<Value, CtReconError> {
    let ct = req.ct.as_ref().ok_or_else(|| {
        CtReconError::Blocked("no coregistered post-op CT (run coreg first)".into())
    })?;
    let template = req.template.as_ref().ok_or_else(|| {
        CtReconError::Blocked(
            "no template given (--template), so points cannot be warped to it".into(),
        )
    })?;
    log(format!(
        "[reconstruct] native TRAC/CORE on {}",
        ct.file_name().unwrap_or_default().to_string_lossy()
    ));
    let (data, aff) =
        load_volume_f64(ct).map_err(|e| CtReconError::Input(format!("E_RECON_input: {e}")))?;
    let (ni, nj, nk) = data.dim();
    let frame = match fixed_frame {
        Some(f) => f,
        None => fit_frame(&aff, [ni, nj, nk], warper)?,
    };
    log(format!(
        "[reconstruct] rigid frame: scale {:.3}, fit residual {:.2} mm",
        frame.scale, frame.fit_rms_mm
    ));
    if !req.metal_dark {
        log("dbsloc: note: reconstruct: the bright metal setting does not apply to a post-op CT in HU; the CT is traced with its metal bright as scanned".into());
    }
    let traced = [!req.skips(SIDE_LEFT), !req.skips(SIDE_RIGHT)];
    let params = NativeTraceParams {
        distal_bloom_mm: req
            .distal_bloom_mm
            .unwrap_or(TracConfig::default().distal_bloom_mm),
        world_to_reference: req.world_to_reference,
        traced,
    };
    let kept_any = traced.iter().any(|t| !t);
    let sides = match trace_native_sides(&data, &aff, &frame, electrode, &params) {
        Ok(s) => s,
        Err(NativeTraceError::SideFailed(failed)) if kept_any => NativeSides {
            left: None,
            right: None,
            failed,
            side_notes: Vec::new(),
        },
        Err(NativeTraceError::Implausible(p)) => return Err(CtReconError::Implausible(p)),
        Err(e @ NativeTraceError::SideFailed(_)) => return Err(CtReconError::Trace(e.to_string())),
        Err(NativeTraceError::Geometry(m)) => return Err(CtReconError::Trace(m)),
    };
    drop(data);
    for s in sides.chosen() {
        log(format!(
            "[reconstruct] {} side from {}",
            s.label,
            s.source.as_str()
        ));
    }
    let mut failed_keys: Vec<(&str, String)> = Vec::new();
    for (label, why) in &sides.failed {
        let key = label_key(label);
        log(format!(
            "dbsloc: warning: reconstruct: the {label} lead (side {key}) was not reconstructed and is left out of the file: {why}"
        ));
        failed_keys.push((key, why.clone()));
    }

    let mut native = Map::new();
    let mut traced_keys: Vec<&str> = Vec::new();
    for s in sides.chosen() {
        let key = label_key(s.label);
        native.insert(key.into(), side_json(&s.geometry));
        traced_keys.push(key);
    }

    let mut meta = Map::new();
    meta.insert("native_input".into(), json!(ct.to_string_lossy()));
    meta.insert("nifti_aff_4x4".into(), mat4_json(&aff));
    meta.insert("frame_rigid_4x4".into(), mat4_json(&frame.matrix()));
    meta.insert("frame_scale".into(), json!(frame.scale));
    meta.insert("frame_fit_rms_mm".into(), json!(frame.fit_rms_mm));
    electrode_meta(&mut meta, electrode);
    meta.insert("tracer".into(), json!(TRACER_TRAC_CORE));
    meta.insert("traced_space".into(), json!("native"));
    meta.insert("side_keys".into(), json!(SIDE_KEYS_CONVENTION));
    let mut source = Map::new();
    for s in sides.chosen() {
        source.insert(s.label.into(), json!(s.source.as_str()));
    }
    meta.insert("side_source".into(), Value::Object(source));
    let mut metal = Map::new();
    for s in sides.chosen().filter(|s| s.source == SideSource::MetalAxis) {
        metal.insert(s.label.into(), side_meta(s));
    }
    if !metal.is_empty() {
        meta.insert("metal_axis".into(), Value::Object(metal));
    }
    if !sides.side_notes.is_empty() {
        let notes: Map<String, Value> = sides
            .side_notes
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!(v)))
            .collect();
        meta.insert("side_notes".into(), Value::Object(notes));
    }
    if electrode.is_directed {
        meta.insert("orientation".into(), json!(ORIENTATION_UNRESOLVED));
    }

    let (mni, vertical) = warp_to_template(&native, warper, log)?;
    let mut status: Vec<(&str, &str)> = traced_keys.iter().map(|k| (*k, SIDE_TRACED)).collect();
    status.extend(failed_keys.iter().map(|(k, _)| (*k, SIDE_FAILED)));
    meta.insert(
        "reconstruct".into(),
        reconstruct_meta(&status, &failed_keys, &side_flags(&traced_keys, &vertical)),
    );
    meta.insert("hybridsave".into(), json!("native -> mni"));

    let mut reco = json!({
        "version": 2,
        "subj_id": req.subj_id,
        "method": METHOD_NATIVE_FRAME,
        "mni": mni,
        "native": native,
        "props": props_json(&electrode.elmodel, &traced_keys),
        "meta": meta,
    });
    recompute_template_contacts(&mut reco, "mni", None, &|_| WarpedContacts::Capture, log);
    let problems = template_problems(
        reco["mni"].as_object().unwrap_or(&Map::new()),
        &[("left", SIDE_LEFT), ("right", SIDE_RIGHT)],
        template,
    )?;
    if !problems.is_empty() {
        return Err(CtReconError::Implausible(problems));
    }
    Ok(reco)
}

pub fn side_name(key: &str) -> &'static str {
    if key == SIDE_RIGHT {
        "right"
    } else {
        "left"
    }
}

#[cfg(feature = "pacer")]
fn key_pacer_leads(
    leads: &[PacerLead],
    midline_x: f64,
) -> Result<Vec<(&'static str, &PacerLead)>, String> {
    if leads.is_empty() {
        return Err("no lead accepted".into());
    }
    if leads.len() > 2 {
        return Err(format!(
            "{} metal components accepted as leads; expected one or two",
            leads.len()
        ));
    }
    let mut out: Vec<(&'static str, &PacerLead)> = Vec::new();
    for lead in leads {
        let dx = lead.centre_mm[0] - midline_x;
        if dx.abs() < MIDLINE_MARGIN_MM {
            return Err(format!(
                "a lead at x = {:.1} mm is within {MIDLINE_MARGIN_MM} mm of the head midline \
                 (x = {midline_x:.1} mm); its hemisphere is ambiguous",
                lead.centre_mm[0]
            ));
        }
        let key = if dx > 0.0 { SIDE_RIGHT } else { SIDE_LEFT };
        if out.iter().any(|(k, _)| *k == key) {
            return Err(format!(
                "both leads lie {} of the head midline (x = {midline_x:.1} mm)",
                side_name(key)
            ));
        }
        out.push((key, lead));
    }
    out.sort_by_key(|(k, _)| *k);
    Ok(out)
}

#[cfg(feature = "pacer")]
fn build_pacer_reco_json(
    subj_id: &str,
    sides: &[(&'static str, &PacerSide)],
    electrode: &ElectrodeGeometry,
    ct: &Path,
    midline_x: f64,
) -> Result<Value, CtReconError> {
    let mut native = Map::new();
    let mut props = Map::new();
    for (key, s) in sides {
        let (traj, tv) = trajectory_from_anchors(s.head, s.tail, electrode)
            .map_err(|e| CtReconError::Trace(format!("PaCER {} lead: {e}", side_name(key))))?;
        native.insert(
            (*key).to_owned(),
            json!({
                "coords_mm": s.level_mm,
                "trajectory": traj,
                "markers": {"head": s.head, "tail": s.tail, "x": s.x, "y": s.y},
                "trajvector": tv,
            }),
        );
        props.insert(
            (*key).to_owned(),
            json!({"elmodel": s.elmodel, "manually_corrected": false}),
        );
    }
    let found: Vec<&str> = sides.iter().map(|(k, _)| side_name(k)).collect();
    let mut meta = Map::new();
    meta.insert("side_keys".into(), json!(SIDE_KEYS_CONVENTION));
    meta.insert("sides_found".into(), json!(found));
    meta.insert("midline_x_mm".into(), json!(midline_x));
    meta.insert("native_input".into(), json!(ct.to_string_lossy()));
    electrode_meta(&mut meta, electrode);
    meta.insert("tracer".into(), json!("pacer_rust"));
    meta.insert("traced_space".into(), json!("native"));
    Ok(json!({
        "version": 2,
        "subj_id": subj_id,
        "method": "pacer_rust",
        "native": native,
        "props": props,
        "meta": meta,
    }))
}

pub const PACER_MASK_TEMPLATE: &str = "template";
pub const PACER_MASK_OWN: &str = "own";
#[cfg(feature = "pacer")]
const MASK_GRID_TOLERANCE_MM: f64 = 1e-3;

#[cfg(feature = "pacer")]
fn same_grid(a: &Mat4, b: &Mat4) -> bool {
    (0..3).all(|r| (0..4).all(|c| (a[r][c] - b[r][c]).abs() <= MASK_GRID_TOLERANCE_MM))
}

#[cfg(feature = "pacer")]
fn pacer_template_mask(
    candidates: &[PathBuf],
    dims: (usize, usize, usize),
    affine: &Mat4,
    log: &mut dyn FnMut(String),
) -> Result<Option<Array3<bool>>, CtReconError> {
    if candidates.is_empty() {
        return Ok(None);
    }
    for path in candidates.iter().filter(|p| p.is_file()) {
        let grid = read_grid(path).map_err(|e| CtReconError::Input(e.to_string()))?;
        if (grid.dims[0], grid.dims[1], grid.dims[2]) != dims || !same_grid(&grid.affine, affine) {
            continue;
        }
        let (mask, aff) = load_mask(path, 0.5).map_err(|e| CtReconError::Input(e.to_string()))?;
        if mask.dim() == dims && same_grid(&aff, affine) {
            log(format!(
                "[reconstruct] PaCER brain mask: template ({})",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            return Ok(Some(mask));
        }
    }
    log("dbsloc: note: reconstruct: no template brain mask lies on the CT grid PaCER segments; PaCER computes its own".into());
    Ok(None)
}

#[cfg(feature = "pacer")]
fn place_pacer_leads<'a>(
    keyed: &[(&'static str, &'a PacerLead)],
    req: &CtReconRequest,
    log: &mut dyn FnMut(String),
) -> Result<Vec<(&'static str, &'a PacerSide)>, CtReconError> {
    let fail = CtReconError::Trace;
    let mut placed: Vec<(&'static str, &PacerSide)> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for (key, lead) in keyed {
        let name = side_name(key);
        if req.skips(key) {
            log(format!(
                "[reconstruct] PaCER {name} lead: kept from the manual correction"
            ));
            continue;
        }
        match &lead.side {
            Ok(side) => {
                log(format!(
                    "[reconstruct] PaCER {name} lead: head ({:.1}, {:.1}, {:.1})",
                    side.head[0], side.head[1], side.head[2]
                ));
                placed.push((*key, side));
            }
            Err(e) => {
                log(format!("[reconstruct] PaCER {name} lead not placed: {e}"));
                failed.push(format!("{name} lead: {e}"));
            }
        }
    }
    if !failed.is_empty() {
        return Err(fail(format!(
            "PaCER could not place {} of {} lead(s): {}",
            failed.len(),
            keyed.len(),
            failed.join(" | ")
        )));
    }
    Ok(placed)
}

#[cfg(feature = "pacer")]
pub fn pacer_reco(
    req: &CtReconRequest,
    electrode: &'static ElectrodeGeometry,
    warper: &dyn PointWarp,
    log: &mut dyn FnMut(String),
) -> Result<Value, CtReconError> {
    let fail = CtReconError::Trace;
    let ct = req.ct.as_ref().ok_or_else(|| {
        CtReconError::Blocked("no coregistered post-op CT (run coreg first)".into())
    })?;
    let template = req.template.as_ref().ok_or_else(|| {
        CtReconError::Blocked(
            "no template given (--template), so points cannot be warped to it".into(),
        )
    })?;
    log(format!(
        "[reconstruct] PaCER on {}",
        ct.file_name().unwrap_or_default().to_string_lossy()
    ));
    let (data, aff) =
        load_volume_f64(ct).map_err(|e| CtReconError::Input(format!("E_RECON_input: {e}")))?;
    let template_mask = pacer_template_mask(&req.brain_masks, data.dim(), &aff, log)?;
    let vol = ct_volume_from_ijk(data.mapv(|v| v as f32), aff);
    drop(data);
    let midline_x = vol
        .head_midline_x()
        .ok_or_else(|| fail("PaCER: no head tissue in the CT to find the midline".into()))?;
    let pacer_mask = if template_mask.is_some() {
        PACER_MASK_TEMPLATE
    } else {
        PACER_MASK_OWN
    };
    let mut cfg = PacerConfig {
        vox_size_mm: fastwarp_core::voxel_spacing_rowmajor(&aff),
        brain_mask: template_mask.map(|m| {
            std::sync::Arc::new(m.permuted_axes([2, 1, 0]).as_standard_layout().to_owned())
        }),
        ..PacerConfig::default()
    };
    if let Some(v) = req.distal_bloom_mm {
        cfg.distal_bloom_mm = v;
    }
    let leads = pacer_leads(&vol, &cfg, Some(&electrode.elmodel))
        .map_err(|e| fail(format!("PaCER: {e}")))?;
    let keyed = key_pacer_leads(&leads, midline_x).map_err(|e| fail(format!("PaCER: {e}")))?;
    let placed = place_pacer_leads(&keyed, req, log)?;
    let limits = PlausibilityLimits::for_electrode(electrode);
    let bounds = WorldBounds::from_grid(&vol.affine, vol.ni, vol.nj, vol.nk, 0.0);
    drop(vol);
    let mut problems: Vec<String> = Vec::new();
    for (key, side) in &placed {
        for msg in check_side(
            side.head,
            side.tail,
            &side.level_mm,
            None,
            &limits,
            Some(&bounds),
        ) {
            problems.push(format!("{} (native): {msg}", side_name(key)));
        }
    }
    if !problems.is_empty() {
        return Err(CtReconError::Implausible(problems));
    }
    let mut reco = build_pacer_reco_json(&req.subj_id, &placed, electrode, ct, midline_x)?;
    let native = reco["native"].as_object().cloned().unwrap_or_default();
    let (mni, vertical) = warp_to_template(&native, warper, log)?;
    let labelled: Vec<(&str, &str)> = placed.iter().map(|(k, _)| (side_name(k), *k)).collect();
    let placed_keys: Vec<&str> = placed.iter().map(|(k, _)| *k).collect();
    if let Some(obj) = reco.as_object_mut() {
        let mut out = Map::new();
        for (k, v) in std::mem::take(obj) {
            if k == "native" {
                out.insert("mni".into(), Value::Object(mni.clone()));
            }
            out.insert(k, v);
        }
        *obj = out;
    }
    recompute_template_contacts(&mut reco, "mni", None, &|_| WarpedContacts::Capture, log);
    let problems = template_problems(
        reco["mni"].as_object().unwrap_or(&Map::new()),
        &labelled,
        template,
    )?;
    if !problems.is_empty() {
        return Err(CtReconError::Implausible(problems));
    }
    if let Some(meta) = reco.get_mut("meta").and_then(Value::as_object_mut) {
        let status: Vec<(&str, &str)> = placed_keys.iter().map(|k| (*k, SIDE_TRACED)).collect();
        meta.insert(
            "reconstruct".into(),
            reconstruct_meta(&status, &[], &side_flags(&placed_keys, &vertical)),
        );
        meta.insert("hybridsave".into(), json!("native -> mni"));
        meta.insert("pacer_mask".into(), json!(pacer_mask));
    }
    Ok(reco)
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn pacer_import_problems(mni: &Map<String, Value>) -> Vec<String> {
    let mut p = Vec::new();
    for (key, label) in [(SIDE_RIGHT, "R"), (SIDE_LEFT, "L")] {
        let Some(s) = mni.get(key) else {
            continue;
        };
        if s.is_null() {
            continue;
        }
        let Some(obj) = s.as_object() else {
            p.push(format!(
                "side {label} ({key}): expected object, got {}",
                json_type_name(s)
            ));
            continue;
        };
        let ok = obj
            .get("markers")
            .and_then(Value::as_object)
            .is_some_and(|m| m.contains_key("head") && m.contains_key("tail"));
        if !ok {
            p.push(format!("side {label} ({key}): markers.head/tail required"));
        }
    }
    p
}

fn bare_subject(id: &str) -> &str {
    let id = id.trim();
    id.strip_prefix("sub-").unwrap_or(id)
}

fn has_side_block(reco: &Value, block: &str) -> bool {
    reco.get(block).and_then(Value::as_object).is_some_and(|b| {
        SIDE_KEYS
            .iter()
            .any(|k| b.get(*k).is_some_and(Value::is_object))
    })
}

fn pacer_import(
    req: &CtReconRequest,
    log: &mut dyn FnMut(String),
) -> Result<Option<Value>, CtReconError> {
    let env = match std::env::var(PACER_IMPORT_ENV)
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty())
    {
        Some(p) => Some(sanitize_path(&p)?).filter(|p| p.is_file()),
        None => None,
    };
    let from_env = env.is_some();
    let Some(path) = env.or_else(|| req.pacer_import.clone().filter(|p| p.is_file())) else {
        return Ok(None);
    };
    let invalid =
        |why: String| CtReconError::Invalid(format!("RecoFileV2 {}: {why}", path.display()));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CtReconError::Input(format!("{}: {e}", path.display())))?;
    let mut v: Value = serde_json::from_str(&text)
        .map_err(|e| CtReconError::Invalid(format!("{}: {e}", path.display())))?;
    if v.get("version").and_then(Value::as_i64).unwrap_or(1) != 2 {
        return Err(CtReconError::Invalid(
            "RecoFileV2: expected version 2".into(),
        ));
    }
    for c in dbsloc_reconstruct::tail_fiducial::convert_legacy_tail(&mut v, None) {
        log(format!("{}: {}", path.display(), c.warning));
    }
    match v.get("subj_id").and_then(Value::as_str) {
        Some(id) if bare_subject(id) != bare_subject(&req.subj_id) => {
            return Err(invalid(format!(
                "the import is for subject {id:?}, not {:?}",
                req.subj_id
            )));
        }
        None if from_env => {
            return Err(invalid(format!(
                "${PACER_IMPORT_ENV} names a file without subj_id, so it cannot be matched to subject {:?}",
                req.subj_id
            )));
        }
        _ => {}
    }
    let Some(mni) = v
        .get("mni")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
    else {
        return Err(invalid("missing or empty mni block".into()));
    };
    let problems = pacer_import_problems(mni);
    if !has_side_block(&v, "native") {
        return Err(invalid(
            "no native block with a lead side; a post-op CT import needs native coordinates".into(),
        ));
    }
    let Some(obj) = v.as_object_mut() else {
        return Err(CtReconError::Invalid(
            "RecoFileV2: expected a JSON object".into(),
        ));
    };
    let method_empty = obj
        .get("method")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty);
    if method_empty {
        obj.insert("method".into(), json!("pacer_import"));
    }
    let meta = obj
        .entry("meta")
        .or_insert_with(|| Value::Object(Map::new()));
    if !meta.is_object() {
        *meta = Value::Object(Map::new());
    }
    if let Some(m) = meta.as_object_mut() {
        m.insert("pacer".into(), json!("import_json"));
        if !problems.is_empty() {
            m.insert("pacer_import_warnings".into(), json!(problems));
        }
    }
    Ok(Some(v))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    #[cfg(feature = "pacer")]
    use dbsloc_pacer::PacerError;
    use dbsloc_reconstruct::{norm3, sub3, unit3};
    use fastwarp_core::dot3;
    use fastwarp_niftiio::WriterOptions;
    use nifti::NiftiHeader;

    struct AffineWarp {
        m: Mat4,
    }

    impl PointWarp for AffineWarp {
        fn to_template(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, PointWarpError> {
            Ok(pts
                .iter()
                .map(|p| fastwarp_core::apply_rowmajor(&self.m, *p))
                .collect())
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_ct_recon_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const OFF: [f64; 3] = [-40.0, -55.0, -35.0];

    fn perp(p: [f64; 3], o: [f64; 3], u: [f64; 3]) -> (f64, f64) {
        let d = sub3(p, o);
        let t = d[0] * u[0] + d[1] * u[1] + d[2] * u[2];
        (
            t,
            norm3([d[0] - t * u[0], d[1] - t * u[1], d[2] - t * u[2]]),
        )
    }

    fn lead_dir(tilt_deg: f64, x_sign: f64) -> [f64; 3] {
        let t = tilt_deg.to_radians();
        let v = [x_sign * 0.15, t.sin(), t.cos()];
        let n = norm3(v);
        [v[0] / n, v[1] / n, v[2] / n]
    }

    fn phantom_ct(e: &ElectrodeGeometry, leads: &[([f64; 3], [f64; 3], f64)]) -> Array3<f64> {
        let first = e.apex_to_first_level_mm();
        Array3::from_shape_fn((81, 111, 81), |(i, j, k)| {
            let w = [i as f64 + OFF[0], j as f64 + OFF[1], k as f64 + OFF[2]];
            let mut v = 30.0;
            for (tip, u, len) in leads {
                let (a, r) = perp(w, *tip, *u);
                if r <= 1.0 && (0.0..=*len).contains(&a) {
                    if !e.tip_is_contact && a < e.tip_length_mm {
                        v = 400.0;
                        continue;
                    }
                    let on_contact = e
                        .level_z_mm
                        .iter()
                        .any(|z| (a - (first + z)).abs() <= 0.5 * e.contact_length_mm);
                    v = if on_contact { 3000.0 } else { 2200.0 };
                }
            }
            v
        })
    }

    fn write_ct(path: &Path, data: &Array3<f64>) {
        let hdr = NiftiHeader {
            sform_code: 1,
            srow_x: [1.0, 0.0, 0.0, OFF[0] as f32],
            srow_y: [0.0, 1.0, 0.0, OFF[1] as f32],
            srow_z: [0.0, 0.0, 1.0, OFF[2] as f32],
            pixdim: [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            ..NiftiHeader::default()
        };
        WriterOptions::new(path)
            .reference_header(&hdr)
            .write_nifti(data)
            .unwrap();
    }

    fn request(dir: &Path, ct: &Path, el: &str) -> CtReconRequest {
        let tpl = dir.join("t1.nii");
        std::fs::write(&tpl, b"").unwrap();
        CtReconRequest {
            ct: Some(ct.to_path_buf()),
            template: Some(tpl),
            el_model: Some(el.into()),
            distal_bloom_mm: Some(0.0),
            ..CtReconRequest::new("01", CtMethod::TracCoreNative)
        }
    }

    fn markers(v: &Value, space: &str, key: &str, name: &str) -> [f64; 3] {
        as_xyz(&v[space][key]["markers"][name]).unwrap()
    }

    #[test]
    fn traced_leads_produce_both_spaces_through_the_point_warp() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let d = tmp("trac");
        let tip_l = [-12.0, -12.0, -12.0];
        let tip_r = [12.0, -12.0, -12.0];
        let (ul, ur) = (lead_dir(30.0, -1.0), lead_dir(30.0, 1.0));
        let ct = d.join("ct.nii.gz");
        write_ct(&ct, &phantom_ct(e, &[(tip_l, ul, 60.0), (tip_r, ur, 60.0)]));
        let req = request(&d, &ct, "Medtronic 3387");
        let warp = AffineWarp {
            m: [
                [1.1, 0.0, 0.0, 2.0],
                [0.0, 1.1, 0.0, -3.0],
                [0.0, 0.0, 1.1, 4.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        };
        let reco =
            native_frame_reco_with(&req, e, &warp, Some(NativeFrame::identity()), &mut |_| {})
                .expect("reconstructed");
        let keys: Vec<&String> = reco.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["version", "subj_id", "method", "mni", "native", "props", "meta"]
        );
        assert_eq!(reco["method"], METHOD_NATIVE_FRAME);
        let meta = &reco["meta"];
        assert_eq!(meta["side_source"]["left"], "trac_core");
        assert_eq!(meta["side_source"]["right"], "trac_core");
        assert_eq!(meta["traced_space"], "native");
        assert_eq!(meta["hybridsave"], "native -> mni");
        assert!(meta.get("metal_axis").is_none() && meta.get("orientation").is_none());
        assert_eq!(meta["reconstruct"]["status"][SIDE_LEFT], SIDE_TRACED);
        assert_eq!(meta["reconstruct"]["status"][SIDE_RIGHT], SIDE_TRACED);
        assert_eq!(meta["reconstruct"]["flags"][SIDE_LEFT], json!([]));
        assert!(meta["reconstruct"].get("failed").is_none());
        let native_keys: Vec<&String> = reco["native"].as_object().unwrap().keys().collect();
        assert_eq!(native_keys, [SIDE_LEFT, SIDE_RIGHT]);
        let head_l = markers(&reco, "native", SIDE_LEFT, "head");
        let head_r = markers(&reco, "native", SIDE_RIGHT, "head");
        let first = e.apex_to_first_level_mm();
        let want_l = [
            tip_l[0] + first * ul[0],
            tip_l[1] + first * ul[1],
            tip_l[2] + first * ul[2],
        ];
        let want_r = [
            tip_r[0] + first * ur[0],
            tip_r[1] + first * ur[1],
            tip_r[2] + first * ur[2],
        ];
        assert!(
            norm3(sub3(head_l, want_l)) < 1.0,
            "{head_l:?} vs {want_l:?}"
        );
        assert!(
            norm3(sub3(head_r, want_r)) < 1.0,
            "{head_r:?} vs {want_r:?}"
        );
        for key in SIDE_KEYS {
            for name in ["head", "tail"] {
                let n = markers(&reco, "native", key, name);
                let m = markers(&reco, "mni", key, name);
                let w = fastwarp_core::apply_rowmajor(&warp.m, n);
                assert!(norm3(sub3(m, w)) < 1e-9);
            }
            let tv = as_xyz(&reco["mni"][key]["trajvector"]).unwrap();
            let tvn = as_xyz(&reco["native"][key]["trajvector"]).unwrap();
            assert!(norm3(sub3(tv, tvn)) < 1e-9);
            let rows = |v: &Value| -> Vec<[f64; 3]> {
                v.as_array()
                    .unwrap()
                    .iter()
                    .map(|r| as_xyz(r).unwrap())
                    .collect()
            };
            let mni_rows = rows(&reco["mni"][key]["coords_mm"]);
            assert_eq!(mni_rows.len(), 4);
            let [h, t, x, y] = ["head", "tail", "x", "y"].map(|n| markers(&reco, "mni", key, n));
            let want =
                dbsloc_reconstruct::native_frame::contacts_from_markers(e, h, t, x, y).unwrap();
            for (got, want) in mni_rows.iter().zip(&want) {
                assert!(norm3(sub3(*got, *want)) < 1e-9);
            }
            let warped = rows(&reco["mni"][key]["coords_mm_warped"]);
            for (w, n) in warped.iter().zip(rows(&reco["native"][key]["coords_mm"])) {
                assert!(norm3(sub3(*w, fastwarp_core::apply_rowmajor(&warp.m, n))) < 1e-9);
            }
            assert_eq!(reco["meta"]["template_contacts"], "from_markers");
            assert_eq!(reco["mni"][key]["trajectory"].as_array().unwrap().len(), 50);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn untraceable_steep_leads_fall_back_to_the_metal_axis_and_still_face_the_tilt_gate() {
        let e = electrode_by_name("Medtronic 3389").unwrap();
        let d = tmp("metal");
        let tip_l = [-14.0, -25.0, -30.0];
        let tip_r = [14.0, -25.0, -30.0];
        let ident = AffineWarp {
            m: fastwarp_core::identity_mat4(),
        };
        let (ul, ur) = (lead_dir(40.0, -1.0), lead_dir(40.0, 1.0));
        let ct = d.join("ct40.nii.gz");
        write_ct(&ct, &phantom_ct(e, &[(tip_l, ul, 36.0), (tip_r, ur, 36.0)]));
        let req = request(&d, &ct, "Medtronic 3389");
        let reco =
            native_frame_reco_with(&req, e, &ident, Some(NativeFrame::identity()), &mut |_| {})
                .expect("metal-axis sides");
        let meta = &reco["meta"];
        assert_eq!(meta["side_source"]["left"], "metal_axis");
        assert_eq!(meta["side_source"]["right"], "metal_axis");
        let notes = meta["side_notes"].as_object().unwrap();
        assert!(notes.contains_key("left") && notes.contains_key("right"));
        assert!(notes["left"]
            .as_str()
            .unwrap()
            .starts_with("TRAC/CORE could not trace this side ("));
        let ma = &meta["metal_axis"]["left"];
        assert!(ma["continuity"].as_f64().unwrap() >= 0.9);
        assert_eq!(ma["continuity_mm"], 30.0);
        assert!(ma["seeds"]["ransac_voxels"].as_u64().unwrap() >= 30);
        assert!(meta.get("tilt_gate").is_none());

        let (sl, sr) = (lead_dir(65.0, -1.0), lead_dir(65.0, 1.0));
        let ct65 = d.join("ct65.nii.gz");
        write_ct(
            &ct65,
            &phantom_ct(e, &[(tip_l, sl, 36.0), (tip_r, sr, 36.0)]),
        );
        let req65 = request(&d, &ct65, "Medtronic 3389");
        let err = native_frame_reco_with(
            &req65,
            e,
            &ident,
            Some(NativeFrame::identity()),
            &mut |_| {},
        )
        .expect_err("tilt gate");
        let msg = err.to_string();
        assert!(msg.starts_with("E_RECON_implausible"), "{msg}");
        assert!(
            msg.contains("left (mni)") && msg.contains("right (mni)"),
            "{msg}"
        );
        assert!(msg.contains("CT→anchor coregistration"), "{msg}");
        assert!(!err.is_os_error());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_side_with_no_metal_names_both_failures() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let d = tmp("none");
        let ct = d.join("empty.nii.gz");
        write_ct(&ct, &phantom_ct(e, &[]));
        let req = request(&d, &ct, "Medtronic 3387");
        let ident = AffineWarp {
            m: fastwarp_core::identity_mat4(),
        };
        let err =
            native_frame_reco_with(&req, e, &ident, Some(NativeFrame::identity()), &mut |_| {})
                .expect_err("nothing to trace");
        let msg = err.to_string();
        assert!(msg.starts_with("left: TRAC/CORE: "), "{msg}");
        assert!(
            msg.contains(
                "; metal axis: fewer than 30 voxels at or above 1800 HU in the hemisphere"
            ),
            "{msg}"
        );
        assert!(msg.contains(" | right: TRAC/CORE: "), "{msg}");
        assert!(!msg.contains("--recon-method pacer"), "{msg}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_single_lead_is_written_and_the_missing_side_is_recorded() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let d = tmp("one_lead");
        let ct = d.join("right_only.nii.gz");
        write_ct(
            &ct,
            &phantom_ct(e, &[([12.0, -12.0, -12.0], lead_dir(30.0, 1.0), 60.0)]),
        );
        let mut req = request(&d, &ct, "Medtronic 3387");
        req.metal_dark = false;
        let ident = AffineWarp {
            m: fastwarp_core::identity_mat4(),
        };
        let mut lines = Vec::new();
        let reco =
            native_frame_reco_with(&req, e, &ident, Some(NativeFrame::identity()), &mut |l| {
                lines.push(l)
            })
            .expect("one lead");
        for block in ["native", "mni", "props"] {
            let keys: Vec<&String> = reco[block].as_object().unwrap().keys().collect();
            assert_eq!(keys, [SIDE_RIGHT], "{block}");
        }
        let meta = &reco["meta"]["reconstruct"];
        assert_eq!(meta["status"][SIDE_RIGHT], SIDE_TRACED);
        assert_eq!(meta["status"][SIDE_LEFT], SIDE_FAILED);
        assert!(meta["failed"][SIDE_LEFT]
            .as_str()
            .unwrap()
            .starts_with("left: TRAC/CORE: "));
        assert_eq!(meta["flags"][SIDE_RIGHT], json!([]));
        assert!(meta["flags"].get(SIDE_LEFT).is_none());
        assert_eq!(reco["meta"]["side_source"].as_object().unwrap().len(), 1);
        assert!(
            lines.iter().any(|l| l.starts_with(
                "dbsloc: warning: reconstruct: the left lead (side 2) was not reconstructed"
            )),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("bright metal setting does not apply")),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_kept_side_survives_a_ct_reconstruct_next_to_a_traced_or_a_failing_side() {
        use crate::manual_correction::{correction_input_paths, kept_sides, EditSpace};
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let d = tmp("kept_ct");
        let anat = d.join("rawdata/sub-1/ses-preop/anat");
        std::fs::create_dir_all(&anat).unwrap();
        std::fs::write(anat.join("sub-1_ses-preop_T1w.nii.gz"), b"x").unwrap();
        let sub = crate::bids::resolve_subject_with(&d, "1", false).unwrap();
        std::fs::create_dir_all(&sub.recon_dir).unwrap();
        let opts = crate::options::RunOptions {
            el_model: Some("Medtronic 3387".into()),
            ..crate::options::RunOptions::new(&d, "1")
        };
        let ident = AffineWarp {
            m: fastwarp_core::identity_mat4(),
        };
        let two = d.join("two.nii.gz");
        write_ct(
            &two,
            &phantom_ct(
                e,
                &[
                    ([-12.0, -12.0, -12.0], lead_dir(30.0, -1.0), 60.0),
                    ([12.0, -12.0, -12.0], lead_dir(30.0, 1.0), 60.0),
                ],
            ),
        );
        let empty = d.join("empty.nii.gz");
        write_ct(&empty, &phantom_ct(e, &[]));
        let first = native_frame_reco_with(
            &request(&d, &two, "Medtronic 3387"),
            e,
            &ident,
            Some(NativeFrame::identity()),
            &mut |_| {},
        )
        .unwrap();
        let snap = crate::locks::snap_input_paths(&correction_input_paths(&sub)).unwrap();
        let mut old = first.clone();
        old["native"][SIDE_RIGHT]["sentinel"] = json!(true);
        old["mni"][SIDE_RIGHT]["sentinel"] = json!(true);
        old["props"][SIDE_RIGHT]["manually_corrected"] = json!(true);
        old["meta"]["manual_correction"] = json!({SIDE_RIGHT: {
            "review": "corrected", "roll_review": null, "space": "native", "inputs": snap,
        }});
        let lock = crate::locks::stage_approval_path(&sub.locks_dir(), "reconstruct");
        for (ct, other) in [(&two, SIDE_TRACED), (&empty, SIDE_FAILED)] {
            std::fs::write(
                sub.reconstruction_json(),
                serde_json::to_string_pretty(&old).unwrap(),
            )
            .unwrap();
            let _ = std::fs::remove_file(&lock);
            let kept = kept_sides(&sub, &opts, EditSpace::Native, &mut |_| {}).expect("kept");
            assert_eq!(kept.keys, [SIDE_RIGHT]);
            let mut req = request(&d, ct, "Medtronic 3387");
            req.skip_sides = vec![SIDE_RIGHT.to_owned()];
            let mut reco =
                native_frame_reco_with(&req, e, &ident, Some(NativeFrame::identity()), &mut |_| {})
                    .unwrap();
            crate::stage_impl::finish_reconstruction(
                &sub,
                &opts,
                &mut reco,
                Some(&kept),
                EditSpace::Native,
                None,
                None,
            )
            .unwrap();
            let after: Value =
                serde_json::from_str(&std::fs::read_to_string(sub.reconstruction_json()).unwrap())
                    .unwrap();
            let rec = &after["meta"]["reconstruct"];
            let meta = &after["meta"];
            assert_eq!(meta["software"]["version"], crate::version::VERSION);
            assert!(meta["software"]["written_at"].is_string());
            assert_eq!(meta["run_options"]["subject"], "1");
            assert_eq!(meta["run_options"]["el_model"], "Medtronic 3387");
            assert!(meta["run_options"].get("el_model_error").is_none());
            assert_eq!(meta["settings"]["recon_method"], "trac_core_native");
            assert_eq!(meta["settings"]["el_model"], "Medtronic 3387");
            assert!(meta["settings"]["reg_backend"]["normalize"].is_null());
            assert_eq!(after["native"][SIDE_RIGHT], old["native"][SIDE_RIGHT]);
            assert_eq!(after["mni"][SIDE_RIGHT], old["mni"][SIDE_RIGHT]);
            assert_eq!(after["props"][SIDE_RIGHT]["manually_corrected"], true);
            assert_eq!(rec["status"][SIDE_RIGHT], SIDE_KEPT);
            assert_eq!(rec["status"][SIDE_LEFT], other);
            assert!(rec["inputs"].is_array());
            let approved = crate::locks::is_approved(&lock).unwrap();
            if other == SIDE_TRACED {
                assert!(after["native"][SIDE_LEFT].is_object());
                assert!(after["meta"]["manual_correction"].get(SIDE_LEFT).is_none());
                assert!(!approved);
            } else {
                assert!(after["native"].get(SIDE_LEFT).is_none());
                assert!(rec["failed"][SIDE_LEFT].is_string());
                assert!(approved);
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(feature = "pacer")]
    #[test]
    fn pacer_placement_skips_kept_sides_and_ignores_their_failures() {
        let leads = vec![lead_at(12.0, false), lead_at(-12.0, true)];
        let keyed = key_pacer_leads(&leads, 0.0).expect("keyed");
        let mut req = CtReconRequest::new("1", CtMethod::Pacer);
        let err = place_pacer_leads(&keyed, &req, &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("could not place 1 of 2"), "{err}");
        req.skip_sides = vec![SIDE_RIGHT.to_owned()];
        let mut lines = Vec::new();
        let placed = place_pacer_leads(&keyed, &req, &mut |l| lines.push(l)).unwrap();
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].0, SIDE_LEFT);
        assert!(lines
            .iter()
            .any(|l| l.contains("kept from the manual correction")));
        req.skip_sides = vec![SIDE_LEFT.to_owned(), SIDE_RIGHT.to_owned()];
        assert!(place_pacer_leads(&keyed, &req, &mut |_| {})
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_reconstruct_meta_records_status_failures_and_vertical_flags_per_side() {
        let keys = [SIDE_RIGHT, SIDE_LEFT];
        let flags = side_flags(&keys, &[SIDE_LEFT]);
        let v = reconstruct_meta(
            &[(SIDE_LEFT, SIDE_TRACED), (SIDE_RIGHT, SIDE_TRACED)],
            &[],
            &flags,
        );
        assert_eq!(v["flags"][SIDE_LEFT], json!([FLAG_VERTICAL_MARKER]));
        assert_eq!(v["flags"][SIDE_RIGHT], json!([]));
        let order: Vec<&String> = v["status"].as_object().unwrap().keys().collect();
        assert_eq!(order, [SIDE_RIGHT, SIDE_LEFT]);
        assert!(v.get("failed").is_none());
        let v = reconstruct_meta(
            &[(SIDE_LEFT, SIDE_FAILED)],
            &[(SIDE_LEFT, "why".to_owned())],
            &[],
        );
        assert_eq!(v["failed"][SIDE_LEFT], "why");
    }

    #[test]
    fn skipped_sides_are_not_traced_and_a_failing_other_side_is_no_error() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let d = tmp("skip");
        let ct = d.join("two.nii.gz");
        write_ct(
            &ct,
            &phantom_ct(
                e,
                &[
                    ([-12.0, -12.0, -12.0], lead_dir(30.0, -1.0), 60.0),
                    ([12.0, -12.0, -12.0], lead_dir(30.0, 1.0), 60.0),
                ],
            ),
        );
        let ident = AffineWarp {
            m: fastwarp_core::identity_mat4(),
        };
        let mut req = request(&d, &ct, "Medtronic 3387");
        req.skip_sides = vec![SIDE_RIGHT.to_owned()];
        let reco =
            native_frame_reco_with(&req, e, &ident, Some(NativeFrame::identity()), &mut |_| {})
                .expect("left traced");
        let keys: Vec<&String> = reco["native"].as_object().unwrap().keys().collect();
        assert_eq!(keys, [SIDE_LEFT]);
        assert!(reco["meta"]["reconstruct"]["status"]
            .get(SIDE_RIGHT)
            .is_none());

        let empty = d.join("empty.nii.gz");
        write_ct(&empty, &phantom_ct(e, &[]));
        let mut req = request(&d, &empty, "Medtronic 3387");
        req.skip_sides = vec![SIDE_RIGHT.to_owned()];
        let reco =
            native_frame_reco_with(&req, e, &ident, Some(NativeFrame::identity()), &mut |_| {})
                .expect("the kept side carries the file");
        assert!(reco["native"].as_object().unwrap().is_empty());
        assert_eq!(
            reco["meta"]["reconstruct"]["status"][SIDE_LEFT],
            SIDE_FAILED
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn blockers_name_the_missing_piece_before_the_electrode() {
        let d = tmp("block");
        let mut req = CtReconRequest::new("01", CtMethod::TracCoreNative);
        let e = reconstruct_ct(&req, None, &mut |_| {}).unwrap_err();
        assert!(e.is_os_error());
        assert!(
            e.to_string()
                .starts_with("E_RECON_native: no template given"),
            "{e}"
        );
        req.template = Some(d.join("t1.nii"));
        assert!(reconstruct_ct(&req, None, &mut |_| {})
            .unwrap_err()
            .to_string()
            .contains("does not exist"));
        std::fs::write(d.join("t1.nii"), b"").unwrap();
        assert!(reconstruct_ct(&req, None, &mut |_| {})
            .unwrap_err()
            .to_string()
            .contains("no coregistered post-op CT"));
        std::fs::write(d.join("ct.nii.gz"), b"").unwrap();
        req.ct = Some(d.join("ct.nii.gz"));
        let inverse = d.join("sub-01_from-MNI_to-anchorNative_mode-image_xfm.h5");
        req.norm_inverse_composite = Some(inverse.clone());
        assert!(reconstruct_ct(&req, None, &mut |_| {})
            .unwrap_err()
            .to_string()
            .contains("no native->template transform"));
        std::fs::write(&inverse, b"").unwrap();
        req.el_model_error = Some("E_RECON_elmodel: uiprefs missing".into());
        let e = reconstruct_ct(&req, None, &mut |_| {}).unwrap_err();
        assert!(matches!(e, CtReconError::Electrode(_)) && !e.is_os_error());
        assert_eq!(e.to_string(), "E_RECON_elmodel: uiprefs missing");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_pacer_import_is_honoured_for_auto_and_pacer_only() {
        let d = tmp("import");
        let p = d.join("sub-1_desc-pacer_import.json");
        let mk = json!({"head": [0, 0, 0], "tail": [0, 0, 5], "x": [1, 0, 0], "y": [0, 1, 0]});
        std::fs::write(
            &p,
            serde_json::to_string(&json!({"version": 2, "subj_id": "1", "method": "pacer",
                "mni": {"1": {"markers": mk, "coords_mm": []}, "2": {"markers": mk}},
                "native": {"1": {"markers": mk}, "2": {"markers": mk}}}))
            .unwrap(),
        )
        .unwrap();
        let mut req = CtReconRequest::new("1", CtMethod::Auto);
        req.pacer_import = Some(p.clone());
        let v = reconstruct_ct(&req, None, &mut |_| {}).unwrap();
        assert_eq!(v["method"], "pacer");
        assert_eq!(v["meta"]["pacer"], "import_json");
        assert!(v["meta"].get("pacer_import_warnings").is_none());
        req.method = CtMethod::TracCoreNative;
        assert!(reconstruct_ct(&req, None, &mut |_| {}).is_err());

        std::fs::write(
            &p,
            serde_json::to_string(&json!({"version": 2, "subj_id": "sub-1", "method": "",
                "mni": {"1": {"coords_mm": []}, "2": [1, 2]}, "native": {"1": {"markers": mk}}}))
            .unwrap(),
        )
        .unwrap();
        req.method = if PACER_AVAILABLE {
            CtMethod::Pacer
        } else {
            CtMethod::Auto
        };
        let v = reconstruct_ct(&req, None, &mut |_| {}).unwrap();
        assert_eq!(v["method"], "pacer_import");
        let w: Vec<&str> = v["meta"]["pacer_import_warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            w,
            [
                "side R (1): markers.head/tail required",
                "side L (2): expected object, got list"
            ]
        );
        std::fs::write(&p, r#"{"version": 1, "mni": {}}"#).unwrap();
        assert!(matches!(
            reconstruct_ct(&req, None, &mut |_| {}),
            Err(CtReconError::Invalid(_))
        ));
        let refused = |doc: Value, needle: &str| {
            std::fs::write(&p, serde_json::to_string(&doc).unwrap()).unwrap();
            match reconstruct_ct(&req, None, &mut |_| {}) {
                Err(CtReconError::Invalid(m)) => assert!(m.contains(needle), "{m}"),
                other => panic!("{needle}: {other:?}"),
            }
        };
        let both = json!({"1": {"markers": mk}});
        refused(
            json!({"version": 2, "subj_id": "2", "mni": both, "native": both}),
            "not \"1\"",
        );
        refused(
            json!({"version": 2, "subj_id": "1", "native": both}),
            "missing or empty mni block",
        );
        refused(
            json!({"version": 2, "subj_id": "1", "mni": both}),
            "no native block",
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    struct ShortWarp;

    impl PointWarp for ShortWarp {
        fn to_template(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, PointWarpError> {
            Ok(pts.iter().skip(1).copied().collect())
        }
    }

    #[test]
    fn warp_sides_refuses_bad_rows_and_short_results() {
        let ident = AffineWarp {
            m: fastwarp_core::identity_mat4(),
        };
        let good = json!({"coords_mm": [[0.0, 0.0, 0.0]], "trajectory": [[0.0, 0.0, 1.0]],
            "markers": {"head": [0.0, 0.0, 0.0], "tail": [0.0, 0.0, 3.0]}});
        let mut sides = Map::new();
        sides.insert(SIDE_RIGHT.into(), good.clone());
        assert!(warp_sides(&sides, &ident).is_ok());
        assert!(matches!(
            warp_sides(&sides, &ShortWarp),
            Err(PointWarpError::Count {
                got: 3,
                expected: 4
            })
        ));
        let mut bad = good.clone();
        bad["coords_mm"][0] = json!([1.0, "x", 2.0]);
        sides.insert(SIDE_RIGHT.into(), bad);
        let e = warp_sides(&sides, &ident).unwrap_err();
        assert!(
            matches!(e, PointWarpError::BadPoint(_)) && e.to_string().contains("coords_mm[0]"),
            "{e}"
        );
        let mut bad = good;
        bad["markers"]["tail"] = json!([1.0, 2.0]);
        sides.insert(SIDE_RIGHT.into(), bad);
        assert!(warp_sides(&sides, &ident)
            .unwrap_err()
            .to_string()
            .contains("marker tail"));
    }

    #[test]
    fn paths_with_parent_components_are_refused() {
        let e = sanitize_path(Path::new("a/../b.nii.gz")).unwrap_err();
        assert!(e.is_os_error() && e.to_string().contains("'..'"), "{e}");
        let d = tmp("sanitize");
        let f = d.join("ct.nii.gz");
        std::fs::write(&f, b"").unwrap();
        let got = sanitize_path(&f).unwrap();
        assert!(
            got.is_absolute() && !got.to_string_lossy().starts_with(r"\\?\"),
            "{got:?}"
        );
        assert!(sanitize_path(&d.join("missing.nii"))
            .unwrap()
            .ends_with("missing.nii"));
        let mut req = CtReconRequest::new("1", CtMethod::Auto);
        req.template = Some(PathBuf::from("x/../t1.nii"));
        assert!(reconstruct_ct(&req, None, &mut |_| {})
            .unwrap_err()
            .to_string()
            .contains("'..'"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn methods_parse_like_the_cli() {
        assert_eq!(CtMethod::parse("AUTO"), Some(CtMethod::Auto));
        assert_eq!(CtMethod::parse(" trac_core "), Some(CtMethod::TracCore));
        assert_eq!(
            CtMethod::parse("trac_core_native"),
            Some(CtMethod::TracCoreNative)
        );
        assert_eq!(CtMethod::parse("pacer"), Some(CtMethod::Pacer));
        assert_eq!(CtMethod::parse("bogus"), None);
    }

    #[cfg(not(feature = "pacer"))]
    #[test]
    fn without_the_pacer_feature_pacer_is_refused_and_auto_still_runs() {
        let d = tmp("no_pacer");
        let p = d.join("sub-1_desc-pacer_import.json");
        let mk = json!({"head": [0, 0, 0], "tail": [0, 0, 5], "x": [1, 0, 0], "y": [0, 1, 0]});
        std::fs::write(
            &p,
            serde_json::to_string(&json!({"version": 2, "subj_id": "1", "method": "pacer",
                "mni": {"1": {"markers": mk, "coords_mm": []}, "2": {"markers": mk}},
                "native": {"1": {"markers": mk}, "2": {"markers": mk}}}))
            .unwrap(),
        )
        .unwrap();
        let mut req = CtReconRequest::new("1", CtMethod::Pacer);
        req.pacer_import = Some(p.clone());
        let e = reconstruct_ct(&req, None, &mut |_| {}).unwrap_err();
        assert!(matches!(e, CtReconError::Invalid(_)) && !e.is_os_error());
        assert_eq!(e.to_string(), PACER_UNAVAILABLE);
        assert!(e.to_string().starts_with("E_RECON_pacer_unavailable: "));
        assert!(e.to_string().contains("needs the optional PaCER component"));
        let mut opts = crate::options::RunOptions {
            recon_method: " PACER ".into(),
            stages: vec![crate::stages::Stage::Reconstruct],
            ..crate::options::RunOptions::new("/data", "1")
        };
        assert_eq!(pacer_unavailable(&opts), Some(PACER_UNAVAILABLE));
        opts.stages = vec![crate::stages::Stage::Coregister];
        assert_eq!(pacer_unavailable(&opts), None);
        opts.stages = vec![crate::stages::Stage::Reconstruct];
        opts.recon_method = "auto".into();
        assert_eq!(pacer_unavailable(&opts), None);
        req.method = CtMethod::Auto;
        let v = reconstruct_ct(&req, None, &mut |_| {}).unwrap();
        assert_eq!(v["meta"]["pacer"], "import_json");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(feature = "pacer")]
    fn lead_at(x: f64, placed: bool) -> PacerLead {
        let side = PacerSide {
            head: [x, -10.0, -8.0],
            tail: [x + 1.0, -6.0, -1.0],
            x: [x + 0.6, -10.0, -8.0],
            y: [x, -9.4, -8.0],
            level_mm: vec![[x, -10.0, -8.0], [x + 1.0, -6.0, -1.0]],
            elmodel: "Medtronic 3387".into(),
            shaft_len_mm: 60.0,
            profile: Vec::new(),
            profile_s_mm: Vec::new(),
            apex_s_mm: 1.0,
        };
        PacerLead {
            centre_mm: [x, -8.0, -4.0],
            side: if placed {
                Ok(side)
            } else {
                Err(PacerError::NoDistalEdge)
            },
        }
    }

    #[test]
    fn a_sheared_warp_still_gives_orthonormal_template_markers_and_recomputed_contacts() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let r = 0.5 * e.lead_diameter_mm;
        let head = [10.0, -5.0, -8.0];
        let tail = [10.0, -5.0, 1.0];
        let native = json!({"1": {
            "coords_mm": [[10.0, -5.0, -8.0], [10.0, -5.0, -5.0]],
            "trajectory": [],
            "markers": {
                "head": head,
                "tail": tail,
                "x": [10.0 + r, -5.0, -8.0],
                "y": [10.0, -5.0 + r, -8.0],
            },
        }});
        let shear = AffineWarp {
            m: [
                [1.0, 0.4, 0.3, 0.0],
                [0.0, 1.3, 0.0, 0.0],
                [0.0, 0.0, 1.1, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        };
        let mni = warp_sides(native.as_object().unwrap(), &shear).unwrap();
        let mut reco =
            json!({"props": {"1": {"elmodel": "Medtronic 3387"}}, "meta": {}, "mni": mni});
        let mut lines = Vec::new();
        recompute_template_contacts(
            &mut reco,
            "mni",
            None,
            &|_| WarpedContacts::Capture,
            &mut |l| lines.push(l),
        );
        assert!(lines.is_empty(), "{lines:?}");
        let m = |n: &str| as_xyz(&reco["mni"]["1"]["markers"][n]).unwrap();
        let (h, t) = (m("head"), m("tail"));
        let u = unit3(sub3(t, h));
        let (xv, yv) = (sub3(m("x"), h), sub3(m("y"), h));
        assert!((norm3(xv) - r).abs() < 1e-9 && (norm3(yv) - r).abs() < 1e-9);
        assert!(dot3(xv, u).abs() < 1e-9 && dot3(yv, u).abs() < 1e-9);
        assert!(dot3(xv, yv).abs() < 1e-9);
        let want = dbsloc_reconstruct::native_frame::contacts_from_markers(e, h, t, m("x"), m("y"))
            .unwrap();
        let rows = reco["mni"]["1"]["coords_mm"].as_array().unwrap();
        assert_eq!(rows.len(), want.len());
        for (row, want) in rows.iter().zip(&want) {
            assert!(norm3(sub3(as_xyz(row).unwrap(), *want)) < 1e-9);
        }
        assert_eq!(
            reco["mni"]["1"]["coords_mm_warped"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[cfg(feature = "pacer")]
    #[test]
    fn a_lone_left_lead_is_side_2() {
        let leads = vec![lead_at(-12.0, true)];
        let keyed = key_pacer_leads(&leads, 0.0).expect("keyed");
        assert_eq!(keyed.len(), 1);
        assert_eq!(keyed[0].0, SIDE_LEFT);
        let side = keyed[0].1.side.as_ref().expect("placed");
        let e = electrode_by_name("Medtronic 3387").expect("catalogued");
        let reco = build_pacer_reco_json("01", &[(keyed[0].0, side)], e, Path::new("ct.nii"), 0.0)
            .unwrap();
        let native = reco["native"].as_object().expect("native");
        assert!(native.contains_key(SIDE_LEFT) && !native.contains_key(SIDE_RIGHT));
        assert_eq!(reco["meta"]["side_keys"], "1=right,2=left");
        assert_eq!(reco["meta"]["sides_found"][0], "left");
        let mni = warp_sides(
            native,
            &AffineWarp {
                m: fastwarp_core::identity_mat4(),
            },
        )
        .unwrap();
        assert_eq!(mni.keys().collect::<Vec<_>>(), [SIDE_LEFT]);
        assert_eq!(mni[SIDE_LEFT]["coords_mm"], native[SIDE_LEFT]["coords_mm"]);
    }

    #[cfg(feature = "pacer")]
    #[test]
    fn pacer_takes_the_first_template_mask_on_its_ct_grid() {
        let d = tmp("pacer_mask_choice");
        let aff = |shift: f64| -> Mat4 {
            [
                [2.0, 0.0, 0.0, shift],
                [0.0, 2.0, 0.0, 0.0],
                [0.0, 0.0, 2.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        };
        let write = |name: &str, dims: (usize, usize, usize), shift: f64| {
            let p = d.join(name);
            let mut m = Array3::<u8>::zeros(dims);
            m[[1, 1, 1]] = 1;
            crate::nifti_io::write_u8_3d(&p, &m, &aff(shift)).unwrap();
            p
        };
        let other_grid = write("raw.nii.gz", (4, 5, 6), 0.0);
        let shifted = write("shifted.nii.gz", (4, 5, 6), 8.0);
        let on_grid = write("anchor.nii.gz", (4, 5, 6), 0.0);
        let lines = std::cell::RefCell::new(Vec::<String>::new());
        let mut log = |l: String| lines.borrow_mut().push(l);
        let got = pacer_template_mask(
            &[shifted.clone(), on_grid.clone()],
            (4, 5, 6),
            &aff(0.0),
            &mut log,
        )
        .unwrap()
        .expect("the second candidate lies on the grid");
        assert_eq!(got.dim(), (4, 5, 6));
        assert!(got[[1, 1, 1]] && got.iter().filter(|v| **v).count() == 1);
        assert!(lines.borrow()[0].contains("anchor.nii.gz"), "{lines:?}");

        let none =
            pacer_template_mask(&[shifted, other_grid], (4, 5, 7), &aff(0.0), &mut log).unwrap();
        assert!(none.is_none());
        assert!(lines
            .borrow()
            .last()
            .unwrap()
            .contains("PaCER computes its own"));
        assert!(pacer_template_mask(&[], (4, 5, 6), &aff(0.0), &mut log)
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(feature = "pacer")]
    #[test]
    fn keys_follow_the_midline_not_the_scanner_origin() {
        let leads = vec![lead_at(30.0, true), lead_at(58.0, true)];
        let keyed = key_pacer_leads(&leads, 44.0).expect("keyed");
        assert_eq!(keyed[0].0, SIDE_RIGHT);
        assert!((keyed[0].1.centre_mm[0] - 58.0).abs() < 1e-9);
        assert_eq!(keyed[1].0, SIDE_LEFT);
    }

    #[cfg(feature = "pacer")]
    #[test]
    fn a_failed_lead_still_occupies_its_hemisphere() {
        let leads = vec![lead_at(12.0, false), lead_at(-12.0, true)];
        let keyed = key_pacer_leads(&leads, 0.0).expect("keyed");
        assert_eq!(keyed[0].0, SIDE_RIGHT);
        assert!(keyed[0].1.side.is_err());
        assert!(keyed[1].1.side.is_ok());
    }

    #[cfg(feature = "pacer")]
    #[test]
    fn spurious_or_ambiguous_leads_are_refused() {
        let three = vec![
            lead_at(12.0, true),
            lead_at(-12.0, true),
            lead_at(30.0, true),
        ];
        assert!(key_pacer_leads(&three, 0.0).is_err());
        let same = vec![lead_at(12.0, true), lead_at(20.0, true)];
        assert!(key_pacer_leads(&same, 0.0).is_err());
        let mid = vec![lead_at(1.0, true)];
        assert!(key_pacer_leads(&mid, 0.0).is_err());
        assert!(key_pacer_leads(&[], 0.0).is_err());
    }

    #[test]
    fn template_gate_reads_the_mask_beside_the_template() {
        let d = tmp("mask");
        let mask = Array3::from_shape_fn((40, 40, 40), |(i, j, k)| {
            u8::from((10..30).contains(&i) && (10..30).contains(&j) && (10..30).contains(&k))
        });
        let aff: Mat4 = [
            [1.0, 0.0, 0.0, -20.0],
            [0.0, 1.0, 0.0, -20.0],
            [0.0, 0.0, 1.0, -20.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        crate::nifti_io::write_u8_3d(&d.join(BRAIN_MASK_NAME), &mask, &aff).unwrap();
        let tpl = d.join("t1.nii");
        let side = json!({"coords_mm": [[0.0, 0.0, 0.0], [0.0, 0.0, 18.0]],
            "markers": {"head": [0.0, 0.0, 0.0], "tail": [0.0, 0.0, 6.0]}});
        let mut mni = Map::new();
        mni.insert(SIDE_LEFT.into(), side);
        let p =
            template_problems(&mni, &[("left", SIDE_LEFT), ("right", SIDE_RIGHT)], &tpl).unwrap();
        assert_eq!(p.len(), 1);
        assert!(
            p[0].starts_with("left (mni): contact 1 at [0.0, 0.0, 18.0] lies outside"),
            "{p:?}"
        );
        let none =
            template_problems(&mni, &[("left", SIDE_LEFT)], &d.join("sub").join("t1.nii")).unwrap();
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn warp_request(prefix: &Path, route: WarpRoute) -> CtReconRequest {
        CtReconRequest {
            template: Some(prefix.with_file_name("t1.nii.gz")),
            norm_inverse_composite: Some(crate::derivatives::suffixed(
                prefix,
                crate::derivatives::ANTS_INVERSE_COMPOSITE_SUFFIX,
            )),
            warp_route: route,
            ..CtReconRequest::new("1", CtMethod::Auto)
        }
    }

    fn write_affine_pair(prefix: &Path) -> fastwarp_reg::TransformChain {
        use fastwarp_reg::transform::{
            write_composite_pair, AffineTransform, CompositePair, TransformChain, TransformElem,
        };
        let a = AffineTransform::from_translation([3.0, -2.0, 5.0]);
        let chain = TransformChain::from_queue(vec![TransformElem::Affine(a)]);
        write_composite_pair(
            prefix,
            &CompositePair {
                forward: chain.clone(),
                inverse: chain.clone(),
            },
        )
        .unwrap();
        chain
    }

    fn warp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("dbsloc_warp_route_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_auto_and_rust_routes_use_fastwarp_reg_when_the_composite_is_readable() {
        let d = warp_dir("rust");
        let prefix = d.join("sub-1_x_");
        let chain = write_affine_pair(&prefix);
        for route in [WarpRoute::Auto, WarpRoute::Rust] {
            let mut lines = Vec::new();
            let warper = point_warper(&warp_request(&prefix, route), None, &mut |l| lines.push(l))
                .ok()
                .unwrap();
            assert!(
                lines.iter().any(|l| l.contains("point warp: fastwarp-reg")),
                "{lines:?}"
            );
            let pts = [[1.0, 2.0, 3.0], [-4.5, 8.25, 0.5]];
            let got = warper.to_template(&pts).unwrap();
            let want = fastwarp_reg::map_points_ras_lattice(&chain, &pts);
            assert_eq!(got, want);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unreadable_composite_falls_back_to_ants_under_auto_and_fails_under_rust() {
        let d = warp_dir("fallback");
        let prefix = d.join("sub-1_x_");
        std::fs::write(d.join("sub-1_x_InverseComposite.h5"), b"not hdf5").unwrap();
        let mut lines = Vec::new();
        assert!(
            point_warper(&warp_request(&prefix, WarpRoute::Auto), None, &mut |l| {
                lines.push(l)
            })
            .is_ok()
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("not readable by fastwarp-reg")
                    && l.contains("antsApplyTransforms")),
            "{lines:?}"
        );
        match point_warper(&warp_request(&prefix, WarpRoute::Rust), None, &mut |_| {}) {
            Err(CtReconError::Warp(m)) => assert!(m.starts_with("E_RECON_warp"), "{m}"),
            other => panic!("{:?}", other.err()),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_ants_route_is_the_default_and_never_reads_the_composite() {
        let d = warp_dir("ants");
        let prefix = d.join("sub-1_x_");
        write_affine_pair(&prefix);
        assert_eq!(
            CtReconRequest::new("1", CtMethod::Auto).warp_route,
            WarpRoute::Ants
        );
        let mut lines = Vec::new();
        assert!(
            point_warper(&warp_request(&prefix, WarpRoute::Ants), None, &mut |l| {
                lines.push(l)
            })
            .is_ok()
        );
        assert!(lines.is_empty(), "{lines:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
