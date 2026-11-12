// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! One hemisphere of one stimulation: lead frame from the Reco native block, engine input from
//! the settings, basis cache, E-field or analytical sphere (`specs/07` §3, §4 step 1, §6).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use dbsloc_reconstruct::{electrode_by_name, ElectrodeGeometry};
use dbsloc_vat::geometry::GridSpec;
use dbsloc_vat::{
    analytic, apply, field_arrows, rebuild_basis, solve_basis_subset, used_contacts_of,
    AnalyticInput, AnalyticKind, Arrow, Basis, CaseSetting, ConductivityModel, ContactSetting,
    Control, Encapsulation, EngineInput, FieldResult, LeadFrame, Mode, Polarity,
    ResolvedModelSettings, SolverSettings, SourceCombination, SourceSetting, VatError, Volume,
};
use dbsloc_vat_gpu::{solve_basis_choosing, VatBackend as GpuKind, VatRequest};
use fastwarp_core::{apply_rowmajor, invert_rowmajor, Mat4};
use rayon::prelude::*;
use serde_json::{json, Map, Value};

use crate::nifti_io::load_volume_f64;
use crate::options::{RunOptions, VatBackend};
use crate::tutor::side_hemispheres;
use crate::vat::basis_cache::{self, CachePaths};
use crate::vat::settings::{self as cfg, ConductivityMode, ModelKind, ModelSettings, Side, Source};
use crate::vat::warp::SpaceWarp;

/// The engine revision recorded with every result; a change recomputes every label.
pub const ENGINE_VERSION: &str = "dbsloc-vat-cpu-1";
const MAX_ARROWS: usize = 1500;
const DOMAIN_GROWTH: f64 = 1.5;
const GM_BOX_MARGIN_MM: f64 = 25.0;

/// Supplies the shared GPU device of the registration engine; tests substitute it.
pub type DeviceProvider = fn() -> Result<Arc<fastwarp_gpu::WgpuBackend>, String>;

/// A failure with its `E_VAT_*` code, or a cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    Cancelled,
    Code { code: &'static str, message: String },
}

impl Fail {
    pub fn new(code: &'static str, message: impl Into<String>) -> Fail {
        Fail::Code {
            code,
            message: message.into(),
        }
    }
}

impl From<VatError> for Fail {
    fn from(e: VatError) -> Fail {
        match e {
            VatError::Cancelled => Fail::Cancelled,
            VatError::NotConverged { .. }
            | VatError::ContactNotResolved { .. }
            | VatError::UnsolvedContact { .. }
            | VatError::Singular => Fail::new("E_VAT_solver", e.to_string()),
            VatError::GridTooLarge { .. } | VatError::Backend(_) => {
                Fail::new("E_VAT_capacity", e.to_string())
            }
            VatError::InvalidInput(m) => Fail::new("E_VAT_settings", m),
            VatError::Montage(m) => Fail::new("E_VAT_montage", m),
            VatError::ModeNotSupported(m) => Fail::new("E_VAT_mode", m),
        }
    }
}

/// The markers and electrode model of one lead.
#[derive(Debug, Clone)]
pub struct LeadSpec {
    pub key: String,
    pub model: ElectrodeGeometry,
    pub head: [f64; 3],
    pub tail: [f64; 3],
    pub x: Option<[f64; 3]>,
    pub y: Option<[f64; 3]>,
}

fn as_xyz(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array()?;
    if a.len() != 3 {
        return None;
    }
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

fn elmodel_name<'a>(reco: &'a Value, key: &str, opts: &'a RunOptions) -> Option<&'a str> {
    reco["props"][key]["elmodel"]
        .as_str()
        .or_else(|| reco["meta"]["elmodel"].as_str())
        .or(opts.el_model.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The native lead of `side`: `Ok(None)` when the Reco has a native block without this side,
/// `E_VAT_no_native` when it has none, `E_VAT_elmodel` when the model cannot be used.
pub fn find_lead(reco: &Value, opts: &RunOptions, side: Side) -> Result<Option<LeadSpec>, Fail> {
    let Some(block) = reco["native"].as_object() else {
        return Err(Fail::new(
            "E_VAT_no_native",
            format!("the reconstruction has no native block for the {side} lead (run reconstruct)"),
        ));
    };
    let empty = Map::new();
    let meta = reco["meta"].as_object().unwrap_or(&empty);
    let hemis = side_hemispheres(meta, block, "native")
        .map_err(|e| Fail::new("E_VAT_no_native", format!("native block: {e}")))?;
    let Some((key, _)) = hemis.into_iter().find(|(_, h)| *h == side.name()) else {
        return Ok(None);
    };
    let markers = reco["scrf"][&key]["markers"]
        .as_object()
        .or_else(|| block[&key]["markers"].as_object())
        .ok_or_else(|| {
            Fail::new(
                "E_VAT_no_native",
                format!("the native {side} lead has no markers"),
            )
        })?;
    let get = |name: &str| markers.get(name).and_then(as_xyz);
    let (Some(head), Some(tail)) = (get("head"), get("tail")) else {
        return Err(Fail::new(
            "E_VAT_no_native",
            format!("the native {side} lead has no head / tail markers"),
        ));
    };
    let name = elmodel_name(reco, &key, opts).ok_or_else(|| {
        Fail::new(
            "E_VAT_elmodel",
            format!("the {side} lead has no electrode model"),
        )
    })?;
    let model = electrode_by_name(name).cloned().ok_or_else(|| {
        Fail::new(
            "E_VAT_elmodel",
            format!("electrode model {name:?} is not in the catalogue"),
        )
    })?;
    if model.n_levels() < 2 || model.contact_xyz_mm.is_empty() {
        return Err(Fail::new(
            "E_VAT_elmodel",
            format!("electrode model {name:?} has no contact geometry for stimulation"),
        ));
    }
    Ok(Some(LeadSpec {
        key,
        model,
        head,
        tail,
        x: get("x"),
        y: get("y"),
    }))
}

fn polarity(p: cfg::Polarity) -> Polarity {
    match p {
        cfg::Polarity::Off => Polarity::Off,
        cfg::Polarity::Cathode => Polarity::Cathode,
        cfg::Polarity::Anode => Polarity::Anode,
    }
}

/// The engine form of an active source.
pub fn engine_source(s: &Source) -> SourceSetting {
    SourceSetting {
        mode: match s.mode {
            cfg::Mode::Voltage => Mode::Voltage,
            cfg::Mode::Current => Mode::Current,
        },
        amplitude: s.amplitude,
        pulse_width_us: s.pulse_width_us,
        contacts: s
            .contacts
            .iter()
            .filter(|c| c.percent > 0.0 && c.polarity != cfg::Polarity::Off)
            .map(|c| ContactSetting {
                index: c.index as usize,
                polarity: polarity(c.polarity),
                percent: c.percent,
                impedance_kohm: c.impedance_kohm,
            })
            .collect(),
        case: CaseSetting {
            polarity: polarity(s.case.polarity),
            percent: s.case.percent,
        },
    }
}

/// Engine settings of a stimulation with the grey-matter mask of one lead.
pub fn resolved_settings(ms: &ModelSettings, gm: Option<Volume>) -> ResolvedModelSettings {
    let conductivity = match ms.conductivity.mode {
        ConductivityMode::Homogeneous => ConductivityModel::Homogeneous {
            s_per_m: ms.conductivity.homogeneous_s_per_m.unwrap_or(0.1),
        },
        ConductivityMode::GmWm => ConductivityModel::GreyWhite {
            gm_s_per_m: ms.conductivity.gm_s_per_m,
            wm_s_per_m: ms.conductivity.wm_s_per_m,
            gm_mask: gm,
        },
    };
    ResolvedModelSettings {
        threshold_v_per_mm: ms.threshold_v_per_mm,
        conductivity,
        encapsulation: Encapsulation {
            thickness_mm: ms.encapsulation.thickness_mm,
            s_per_m: ms.encapsulation.s_per_m.unwrap_or(0.0),
        },
        remove_electrode: ms.remove_electrode,
        source_combination: match ms.source_combination {
            cfg::SourceCombination::Max => SourceCombination::Max,
            cfg::SourceCombination::Sum => SourceCombination::Sum,
        },
        solver: SolverSettings::default(),
    }
}

fn sample_trilinear(vol: &ndarray::Array3<f64>, inverse: &Mat4, world: [f64; 3]) -> f64 {
    let p = apply_rowmajor(inverse, world);
    let lo = [p[0].floor(), p[1].floor(), p[2].floor()];
    let f = [p[0] - lo[0], p[1] - lo[1], p[2] - lo[2]];
    let (nx, ny, nz) = vol.dim();
    let mut acc = 0.0;
    for c in 0..8usize {
        let d = [c & 1, (c >> 1) & 1, (c >> 2) & 1];
        let mut w = 1.0;
        let mut idx = [0i64; 3];
        for a in 0..3 {
            w *= if d[a] == 1 { f[a] } else { 1.0 - f[a] };
            idx[a] = lo[a] as i64 + d[a] as i64;
        }
        if w <= 0.0 {
            continue;
        }
        if idx[0] < 0
            || idx[1] < 0
            || idx[2] < 0
            || idx[0] as usize >= nx
            || idx[1] as usize >= ny
            || idx[2] as usize >= nz
        {
            continue;
        }
        let v = vol[[idx[0] as usize, idx[1] as usize, idx[2] as usize]];
        if v.is_finite() {
            acc += w * v;
        }
    }
    acc
}

/// The template-space grey-matter mask resampled to a native box around the lead through the
/// inverse normalization (`specs/07` §3.2).
pub fn gm_volume(
    mask: &Path,
    warp: &SpaceWarp,
    lead: &LeadSpec,
    spacing: f64,
) -> Result<Volume, String> {
    let (vol, aff) = load_volume_f64(mask).map_err(|e| e.to_string())?;
    let inverse = invert_rowmajor(&aff).ok_or_else(|| "singular mask affine".to_owned())?;
    let pts = [lead.head, lead.tail];
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in pts {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a] - GM_BOX_MARGIN_MM);
            hi[a] = hi[a].max(p[a] + GM_BOX_MARGIN_MM);
        }
    }
    let mut dims = [0usize; 3];
    let mut origin = [0.0; 3];
    for a in 0..3 {
        origin[a] = (lo[a] / spacing).floor() * spacing;
        dims[a] = (((hi[a] - origin[a]) / spacing).ceil() as usize + 1).max(2);
    }
    let [nx, ny, nz] = dims;
    let affine = [
        [spacing, 0.0, 0.0, origin[0]],
        [0.0, spacing, 0.0, origin[1]],
        [0.0, 0.0, spacing, origin[2]],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let world = |i: usize, j: usize, k: usize| {
        [
            origin[0] + spacing * i as f64,
            origin[1] + spacing * j as f64,
            origin[2] + spacing * k as f64,
        ]
    };
    let data: Vec<f32> = (0..nz)
        .into_par_iter()
        .flat_map_iter(|k| {
            let mut plane = Vec::with_capacity(nx * ny);
            for j in 0..ny {
                for i in 0..nx {
                    let t = warp.point_direct(world(i, j, k));
                    plane.push(sample_trilinear(&vol, &inverse, t) as f32);
                }
            }
            plane
        })
        .collect();
    Ok(Volume { dims, affine, data })
}

/// What one hemisphere's computation produced.
#[derive(Debug, Clone)]
pub struct SideOutcome {
    pub field: FieldResult,
    pub arrows: Vec<Arrow>,
    pub analytic: bool,
    pub basis_cache: &'static str,
    pub backend: &'static str,
    pub adapter: Option<String>,
    pub notes: Vec<String>,
    pub iterations: usize,
    pub rel_residual: f64,
    pub solver_dims: Option<[usize; 3]>,
    pub seconds: f64,
    pub warnings: Vec<String>,
    pub source_indices: Vec<u32>,
}

/// Everything a hemisphere computation needs.
pub struct SideRequest<'a> {
    pub side: Side,
    pub lead: &'a LeadSpec,
    pub kind: ModelKind,
    pub ms: &'a ModelSettings,
    pub sources: Vec<(u32, SourceSetting)>,
    pub gm: Option<Volume>,
    pub gm_identity: Value,
    pub grid: GridSpec,
    pub cache: Option<CachePaths>,
    pub write_cache: bool,
    pub backend: VatBackend,
    pub device: DeviceProvider,
}

fn frame(lead: &LeadSpec) -> Result<LeadFrame, Fail> {
    LeadFrame::from_markers(lead.head, lead.tail, lead.x, lead.y, &lead.model).map_err(|e| {
        Fail::new(
            "E_VAT_no_native",
            format!("the native lead markers are unusable: {e}"),
        )
    })
}

fn cache_key(req: &SideRequest<'_>, grid: &GridSpec, solver: &SolverSettings) -> Value {
    let c = &req.ms.conductivity;
    json!({
        "engine": ENGINE_VERSION,
        "side": req.side.letter().to_string(),
        "markers": {
            "head": req.lead.head,
            "tail": req.lead.tail,
            "x": req.lead.x,
            "y": req.lead.y,
        },
        "elmodel": req.lead.model.elmodel,
        "conductivity": {
            "mode": format!("{:?}", c.mode),
            "gm_s_per_m": c.gm_s_per_m,
            "wm_s_per_m": c.wm_s_per_m,
            "homogeneous_s_per_m": c.homogeneous_s_per_m,
            "gm_mask": req.gm_identity,
        },
        "encapsulation": {
            "thickness_mm": req.ms.encapsulation.thickness_mm,
            "s_per_m": req.ms.encapsulation.s_per_m,
        },
        "grid": {
            "fine_spacing_mm": grid.fine_spacing_mm,
            "fine_half_width_mm": grid.fine_half_width_mm,
            "fine_margin_z_mm": grid.fine_margin_z_mm,
            "growth": grid.growth,
            "domain_margin_mm": grid.domain_margin_mm,
            "max_cells": grid.max_cells,
        },
        "solver": {"rel_tol": solver.rel_tol, "max_iter": solver.max_iter},
    })
}

fn enlarged(grid: &GridSpec) -> GridSpec {
    GridSpec {
        domain_margin_mm: grid.domain_margin_mm * DOMAIN_GROWTH,
        ..grid.clone()
    }
}

struct Solved {
    basis: Basis,
    backend: &'static str,
    adapter: Option<String>,
    notes: Vec<String>,
}

struct Obtained {
    basis: Basis,
    state: &'static str,
    backend: &'static str,
    adapter: Option<String>,
    notes: Vec<String>,
}

fn cpu_solved(
    input: &EngineInput,
    contacts: &[usize],
    ctl: &Control<'_>,
    notes: Vec<String>,
) -> Result<Solved, Fail> {
    Ok(Solved {
        basis: solve_basis_subset(input, contacts, ctl)?,
        backend: "cpu",
        adapter: None,
        notes,
    })
}

fn cpu_note(why: &str) -> String {
    format!("dbsloc: note: vat: {why}; using the CPU engine")
}

fn solve_subset(
    request: VatBackend,
    device: DeviceProvider,
    input: &EngineInput,
    contacts: &[usize],
    ctl: &Control<'_>,
) -> Result<Solved, Fail> {
    let (vat_request, explicit) = match request {
        VatBackend::Cpu => return cpu_solved(input, contacts, ctl, Vec::new()),
        VatBackend::Auto => (VatRequest::Auto, false),
        VatBackend::Wgpu => (VatRequest::Wgpu, true),
    };
    let backend = match device() {
        Ok(b) => b,
        Err(e) if explicit => {
            return Err(Fail::new(
                "E_VAT_capacity",
                format!("vat_backend wgpu requested but no GPU device is available: {e}"),
            ))
        }
        Err(e) => {
            return cpu_solved(
                input,
                contacts,
                ctl,
                vec![cpu_note(&format!("no usable GPU device ({e})"))],
            )
        }
    };
    let ctx = backend.context();
    match solve_basis_choosing(ctx, vat_request, input, contacts, ctl) {
        Ok((basis, chosen)) if chosen.backend == GpuKind::Wgpu => Ok(Solved {
            basis,
            backend: "wgpu",
            adapter: Some(ctx.info.name.clone()),
            notes: Vec::new(),
        }),
        Ok((basis, chosen)) => Ok(Solved {
            basis,
            backend: "cpu",
            adapter: None,
            notes: vec![cpu_note(&chosen.reason)],
        }),
        Err(VatError::Backend(m)) if !explicit => {
            cpu_solved(input, contacts, ctl, vec![cpu_note(&m)])
        }
        Err(e) => Err(e.into()),
    }
}

fn backend_name(built_by: Option<&str>) -> &'static str {
    match built_by {
        Some("wgpu") => "wgpu",
        _ => "cpu",
    }
}

fn obtain_basis(
    req: &SideRequest<'_>,
    input: &EngineInput,
    used: &[usize],
    key: &Value,
    ctl: &Control<'_>,
) -> Result<Obtained, Fail> {
    let cache = req.cache.as_ref();
    let sha = basis_cache::key_sha256(key);
    let loaded = cache.and_then(|p| basis_cache::load(p, &sha));
    let solve = |contacts: &[usize]| solve_subset(req.backend, req.device, input, contacts, ctl);
    let store = |s: &Solved| {
        if req.write_cache {
            if let Some(p) = cache {
                let _ = basis_cache::store(p, key, &s.basis, s.backend, s.adapter.as_deref());
            }
        }
    };
    if let Some(l) = loaded {
        let missing: Vec<usize> = used
            .iter()
            .copied()
            .filter(|&k| l.solutions.get(k).is_none_or(Option::is_none))
            .collect();
        if missing.is_empty() {
            let (built_by, adapter) = (l.built_by.clone(), l.adapter.clone());
            if let Ok(b) = rebuild_basis(input, l.solutions, l.z_kohm, l.stats) {
                return Ok(Obtained {
                    basis: b,
                    state: "hit",
                    backend: backend_name(built_by.as_deref()),
                    adapter,
                    notes: Vec::new(),
                });
            }
        } else {
            let mut solved = solve(&missing)?;
            let b = &mut solved.basis;
            for (k, sol) in l.solutions.into_iter().enumerate() {
                if let (None, Some(sol)) = (&b.solutions[k], sol) {
                    b.solutions[k] = Some(sol);
                    for (j, row) in b.z_kohm.iter_mut().enumerate() {
                        row[k] = l.z_kohm[j][k];
                    }
                }
            }
            let mut stats = l.stats;
            stats.append(&mut b.stats);
            b.stats = stats;
            store(&solved);
            return Ok(Obtained {
                basis: solved.basis,
                state: "built",
                backend: solved.backend,
                adapter: solved.adapter,
                notes: solved.notes,
            });
        }
    }
    let solved = solve(used)?;
    store(&solved);
    Ok(Obtained {
        basis: solved.basis,
        state: "built",
        backend: solved.backend,
        adapter: solved.adapter,
        notes: solved.notes,
    })
}

fn analytic_kind(kind: ModelKind) -> Option<AnalyticKind> {
    match kind {
        ModelKind::Kuncel => Some(AnalyticKind::Kuncel),
        ModelKind::Maedler => Some(AnalyticKind::Maedler),
        ModelKind::Dembek => Some(AnalyticKind::Dembek),
        _ => None,
    }
}

/// Computes one hemisphere in native space.
pub fn compute_side(
    req: &SideRequest<'_>,
    cancel: Option<&AtomicBool>,
) -> Result<SideOutcome, Fail> {
    let t0 = Instant::now();
    let lead_frame = frame(req.lead)?;
    let sources: Vec<SourceSetting> = req.sources.iter().map(|s| s.1.clone()).collect();
    let indices: Vec<u32> = req.sources.iter().map(|s| s.0).collect();
    if let Some(kind) = analytic_kind(req.kind) {
        let field = analytic(&AnalyticInput {
            lead: lead_frame,
            model: req.lead.model.clone(),
            kind,
            threshold_v_per_mm: req.ms.threshold_v_per_mm,
            dembek_reference_pulse_width_us: req.ms.dembek.reference_pulse_width_us,
            sources,
        })?;
        let warnings = field.warnings.clone();
        return Ok(SideOutcome {
            field,
            arrows: Vec::new(),
            analytic: true,
            basis_cache: "none",
            backend: "analytic",
            adapter: None,
            notes: Vec::new(),
            iterations: 0,
            rel_residual: 0.0,
            solver_dims: None,
            seconds: t0.elapsed().as_secs_f64(),
            warnings,
            source_indices: indices,
        });
    }
    if req.kind != ModelKind::Efield {
        return Err(Fail::new(
            "E_VAT_model",
            format!(
                "{} outputs can be imported but not computed; use efield",
                req.kind.slug()
            ),
        ));
    }
    let ctl = Control {
        cancel,
        progress: None,
    };
    let settings = resolved_settings(req.ms, req.gm.clone());
    let used = used_contacts_of(&sources);
    let grids = [req.grid.clone(), enlarged(&req.grid)];
    let start = match &req.cache {
        Some(p) => {
            let second = basis_cache::key_sha256(&cache_key(req, &grids[1], &settings.solver));
            usize::from(basis_cache::matches(p, &second))
        }
        None => 0,
    };
    let mut chosen: Option<(FieldResult, Vec<Arrow>, Obtained)> = None;
    let mut notes: Vec<String> = Vec::new();
    for grid in grids.iter().skip(start) {
        let input = EngineInput {
            lead: lead_frame,
            model: req.lead.model.clone(),
            body: dbsloc_vat::BodyKind::Catalogue,
            settings: settings.clone(),
            sources: sources.clone(),
            grid: grid.clone(),
        };
        let key = cache_key(req, grid, &settings.solver);
        let step = obtain_basis(req, &input, &used, &key, &ctl).and_then(|obtained| {
            let field = apply(&obtained.basis, &sources)?;
            Ok((field, obtained))
        });
        match step {
            Ok((field, obtained)) => {
                let touches = field.touches_boundary;
                let arrows = if req.kind == ModelKind::Efield {
                    field_arrows(&obtained.basis, &sources, MAX_ARROWS).unwrap_or_default()
                } else {
                    Vec::new()
                };
                chosen = Some((field, arrows, obtained));
                if !touches {
                    break;
                }
            }
            Err(Fail::Cancelled) => return Err(Fail::Cancelled),
            Err(e) if chosen.is_some() => {
                if let Fail::Code { message, .. } = e {
                    notes.push(format!(
                        "the domain could not be enlarged ({message}); the first result is kept"
                    ));
                }
                break;
            }
            Err(e) => return Err(e),
        }
    }
    let Some((field, arrows, obtained)) = chosen else {
        return Err(Fail::new("E_VAT_solver", "no solution was computed"));
    };
    let Obtained {
        basis,
        state,
        backend,
        adapter,
        notes: solver_notes,
    } = obtained;
    let iterations = basis
        .stats
        .iter()
        .filter(|s| used.contains(&s.contact))
        .map(|s| s.iterations)
        .max()
        .unwrap_or(0);
    let rel_residual = basis
        .stats
        .iter()
        .filter(|s| used.contains(&s.contact))
        .map(|s| s.rel_residual)
        .fold(0.0, f64::max);
    let mut warnings = field.warnings.clone();
    warnings.extend(notes);
    Ok(SideOutcome {
        field,
        arrows,
        analytic: false,
        basis_cache: state,
        backend,
        adapter,
        notes: solver_notes,
        iterations,
        rel_residual,
        solver_dims: Some(basis.grid.dims),
        seconds: t0.elapsed().as_secs_f64(),
        warnings,
        source_indices: indices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_errors_map_to_their_codes() {
        let code = |e: VatError| match Fail::from(e) {
            Fail::Code { code, .. } => code,
            Fail::Cancelled => "cancelled",
        };
        assert_eq!(code(VatError::Cancelled), "cancelled");
        assert_eq!(code(VatError::Singular), "E_VAT_solver");
        assert_eq!(
            code(VatError::NotConverged {
                iterations: 1,
                rel_residual: 1.0
            }),
            "E_VAT_solver"
        );
        assert_eq!(code(VatError::GridTooLarge { cells: 1 }), "E_VAT_capacity");
        assert_eq!(code(VatError::Montage("m".into())), "E_VAT_montage");
        assert_eq!(code(VatError::ModeNotSupported("m".into())), "E_VAT_mode");
        assert_eq!(code(VatError::InvalidInput("m".into())), "E_VAT_settings");
    }
}
