// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The `vat` pipeline stage (`specs/07` §7): every stimulation label of the subject is computed
//! in native space, warped into the template space, written in the Lead-DBS names and measured
//! against an atlas.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use dbsloc_vat::geometry::GridSpec;
use dbsloc_vat::Volume;
use serde_json::{json, Value};

use crate::atlas_catalog::{bundled_and_external_atlas_sets, AtlasSetEntry, ATLAS_SPACE};
use crate::bids::Subject;
use crate::error::{PipelineError, PipelineResult};
use crate::locks::{
    approval_inputs_still_valid, snap_input_paths, stage_approval_path, write_approval,
};
use crate::options::RunOptions;
use crate::outputs::{write_replacing, Staged};
use crate::protocol::{RunEvent, StageOutcome};
use crate::sha256::{file_sha256_hex, sha256_hex};
use crate::stage_impl::{log, opts_with_prefs_model};
use crate::template_catalog::REFERENCE_SPACE;
use crate::vat::basis_cache::CachePaths;
use crate::vat::engine::{
    compute_side, engine_source, find_lead, gm_volume, DeviceProvider, Fail, LeadSpec, SideOutcome,
    SideRequest, ENGINE_VERSION,
};
use crate::vat::mesh;
use crate::vat::outputs::{
    efield_gauss, hemi_mat_bytes, write_binary_nii, write_bytes, write_f32_nii, Codes, Gradient,
    Names,
};
use crate::vat::settings::{
    self as cfg, canonical_json, check, label_dir, list_labels, normalise, stimulation_json_path,
    write_stimparameters_mat, LabelState, ModelKind, Side, Stimulation,
};
use crate::vat::stats::{ea_stats_bytes, side_stats, stats_json, SideStats, StatsAtlas};
use crate::vat::warp::{self, native_gradient, Field, SpaceWarp};

/// The stage name used in locks and messages.
pub const STAGE: &str = "vat";
const GM_FILE: &str = "gm_mask.nii.gz";
const ARROW_LENGTH_MM: f64 = 0.2;
const RESERVED_GS: &str = "reserved for group stimulations";

/// Tunables that tests and the interactive preview override; `Default` is the production setup.
#[derive(Debug, Clone)]
pub struct VatParams {
    pub grid: GridSpec,
    pub gm_spacing_mm: f64,
    pub device: DeviceProvider,
}

impl Default for VatParams {
    fn default() -> Self {
        VatParams {
            grid: GridSpec::default(),
            gm_spacing_mm: 0.5,
            device: crate::reg_backend::shared_backend,
        }
    }
}

struct ReleaseGpu(bool);

impl Drop for ReleaseGpu {
    fn drop(&mut self) {
        if self.0 {
            crate::reg_backend::release_idle_gpu_memory();
        }
    }
}

enum Done {
    Computed { native_only: bool },
    Kept,
}

struct LabelError {
    label: String,
    fail: Fail,
}

fn label_backend(kind: ModelKind, runs: &[SideRun; 2]) -> &'static str {
    if kind.is_analytical() {
        return "analytic";
    }
    let used: Vec<&'static str> = runs
        .iter()
        .filter_map(|r| r.outcome.as_ref().map(|o| o.backend))
        .collect();
    if !used.is_empty() && used.iter().all(|b| *b == "wgpu") {
        "wgpu"
    } else {
        "cpu"
    }
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::SeqCst))
}

fn stim_fail(e: cfg::StimError) -> Fail {
    match e {
        cfg::StimError::Invalid(errors) => {
            let code = errors.first().map_or("E_VAT_settings", |e| e.code);
            let text = errors
                .iter()
                .map(|e| match (e.side, e.source) {
                    (Some(s), Some(j)) => format!("{s} source {j}: {}", e.reason),
                    (Some(s), None) => format!("{s}: {}", e.reason),
                    _ => e.reason.clone(),
                })
                .collect::<Vec<_>>()
                .join("; ");
            Fail::new(code, text)
        }
        other => Fail::new("E_VAT_settings", other.to_string()),
    }
}

/// The inputs a label's results depend on and that the lock fingerprints: the Reco JSON, every
/// canonical stimulation file and the normalization composites.
pub fn lock_input_paths(sub: &Subject) -> Vec<PathBuf> {
    let mut v = vec![sub.reconstruction_json()];
    for e in list_labels(&sub.subj_dir, &sub.subj_id) {
        if e.state == LabelState::Canonical && e.space == "native" {
            v.push(stimulation_json_path(&sub.subj_dir, &sub.subj_id, &e.label));
        }
    }
    v.extend(sub.norm.transform_forward.clone());
    v.extend(sub.norm.transform_inverse.clone());
    v
}

fn discover_labels(sub: &Subject, opts: &RunOptions) -> Result<Vec<String>, PipelineError> {
    if opts.vat_labels.is_empty() {
        return Ok(list_labels(&sub.subj_dir, &sub.subj_id)
            .into_iter()
            .filter(|e| e.state == LabelState::Canonical && e.space == "native")
            .map(|e| e.label)
            .collect());
    }
    let mut out: Vec<String> = Vec::new();
    for l in &opts.vat_labels {
        if !stimulation_json_path(&sub.subj_dir, &sub.subj_id, l).is_file() {
            return Err(PipelineError::Message(format!(
                "vat: E_VAT_settings: no stimulation {l:?} (no sub-{}_desc-stimulation.json in stimulations/native/{l}/)",
                sub.subj_id
            )));
        }
        if !out.contains(l) {
            out.push(l.clone());
        }
    }
    Ok(out)
}

fn settings_hash(stim: &Stimulation) -> Result<String, Fail> {
    let mut bare = stim.clone();
    bare.provenance = cfg::Provenance::default();
    canonical_json(&bare)
        .map(|t| sha256_hex(t.as_bytes()))
        .map_err(stim_fail)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub(crate) struct Run<'a> {
    sub: &'a Subject,
    opts: &'a RunOptions,
    params: &'a VatParams,
    reco: Value,
    warp: Result<SpaceWarp, String>,
    atlas_sets: Option<Vec<AtlasSetEntry>>,
    warp_sha: Option<String>,
    cancel: Option<&'a AtomicBool>,
    tx: Option<&'a mpsc::Sender<RunEvent>>,
}

impl Run<'_> {
    fn say(&self, line: String) {
        log(self.tx, line);
    }

    fn sets(&mut self) -> &[AtlasSetEntry] {
        if self.atlas_sets.is_none() {
            let tx = self.tx;
            let sets = bundled_and_external_atlas_sets(
                self.opts.leaddbs_atlases_dir.as_deref(),
                &mut |l| log(tx, format!("dbsloc: note: vat: {l}")),
            );
            self.atlas_sets = Some(sets);
        }
        self.atlas_sets.as_deref().unwrap_or_default()
    }

    fn gm_path(&mut self, spec: Option<&str>) -> Result<PathBuf, String> {
        let spec = spec.ok_or_else(|| "no grey-matter mask is configured".to_owned())?;
        match spec.strip_prefix("atlas:") {
            Some(name) => {
                let set = self
                    .sets()
                    .iter()
                    .find(|e| e.name == name || e.id == name)
                    .cloned()
                    .ok_or_else(|| format!("atlas set {name:?} is not available"))?;
                let p = set.dir.join(GM_FILE);
                if p.is_file() {
                    Ok(p)
                } else {
                    Err(format!("{} has no {GM_FILE}", set.name))
                }
            }
            None => {
                let p = PathBuf::from(spec);
                if p.is_file() {
                    Ok(p)
                } else {
                    Err(format!("{spec} does not exist"))
                }
            }
        }
    }

    fn warp_hash(&mut self) -> String {
        if self.warp_sha.is_none() {
            let h = self
                .sub
                .norm
                .transform_inverse
                .as_deref()
                .and_then(|p| file_sha256_hex(p).ok())
                .unwrap_or_default();
            self.warp_sha = Some(h);
        }
        self.warp_sha.clone().unwrap_or_default()
    }

    fn resolve_gm(
        &mut self,
        stim: &Stimulation,
        lead: &LeadSpec,
    ) -> (Option<Volume>, Value, Option<String>) {
        let c = &stim.model_settings.conductivity;
        if c.mode != cfg::ConductivityMode::GmWm {
            return (None, json!("none"), None);
        }
        let attempt = (|| -> Result<(Volume, Value), String> {
            let path = self.gm_path(c.gm_mask.as_deref())?;
            let warp = self.warp.as_ref().map_err(|e| e.clone())?;
            let vol = gm_volume(&path, warp, lead, self.params.gm_spacing_mm)?;
            let mask_sha = file_sha256_hex(&path).map_err(|e| e.to_string())?;
            Ok((
                vol,
                json!({
                    "file": path.to_string_lossy(),
                    "sha256": mask_sha,
                    "spacing_mm": self.params.gm_spacing_mm,
                }),
            ))
        })();
        match attempt {
            Ok((vol, mut identity)) => {
                let w = self.warp_hash();
                identity["warp_sha256"] = json!(w);
                (Some(vol), identity, None)
            }
            Err(why) => (None, json!("none"), Some(why)),
        }
    }

    fn input_paths(&mut self, stim: &Stimulation) -> Vec<PathBuf> {
        let mut v = vec![self.sub.reconstruction_json()];
        v.extend(self.sub.norm.transform_forward.clone());
        v.extend(self.sub.norm.transform_inverse.clone());
        if stim.model == ModelKind::Efield
            && stim.model_settings.conductivity.mode == cfg::ConductivityMode::GmWm
        {
            if let Ok(p) = self.gm_path(stim.model_settings.conductivity.gm_mask.as_deref()) {
                v.push(p);
            }
        }
        v
    }

    fn up_to_date(&mut self, stim: &Stimulation, settings_sha: &str, ms_json: &Value) -> bool {
        let names = Names::new(&self.sub.subj_id, stim.model.slug());
        let sidecar = label_dir(&self.sub.subj_dir, "native", &stim.label).join(names.vta_json());
        let Some(rec) = read_json(&sidecar) else {
            return false;
        };
        let want_atlas = self.opts.vat_stats_atlas.clone().unwrap_or_default();
        let paths = self.input_paths(stim);
        rec["engine_version"] == ENGINE_VERSION
            && rec["settings_sha256"] == settings_sha
            && rec["model_settings"] == *ms_json
            && rec["stats_atlas_request"] == want_atlas
            && approval_inputs_still_valid(&json!({"inputs": rec["inputs"]}), &paths)
    }
}

pub(crate) struct SideRun {
    pub(crate) outcome: Option<SideOutcome>,
    pub(crate) status: &'static str,
    pub(crate) reason: Option<String>,
    pub(crate) fallback: Option<String>,
}

struct SideFields {
    native: Option<Field>,
    template: Option<Field>,
    reference: Option<Field>,
    gradient_native: Option<Gradient>,
    gradient_template: Option<Gradient>,
}

fn has_image(f: &Field) -> bool {
    f.dims.iter().all(|&d| d > 0)
}

fn affine_json(a: &[[f64; 4]; 4]) -> Value {
    json!(a.iter().map(|r| r.to_vec()).collect::<Vec<_>>())
}

fn amplitudes(stim: &Stimulation, side: Side) -> [f64; 4] {
    let mut out = [0.0; 4];
    for s in &stim.sides.get(side).sources {
        if (1..=4).contains(&s.index) && s.is_active() {
            out[s.index as usize - 1] = s.amplitude;
        }
    }
    out
}

fn side_volume(run: &SideRun, field: Option<&Field>, native: bool) -> f64 {
    let Some(o) = &run.outcome else {
        return 0.0;
    };
    if o.analytic {
        return o.field.volumes.sphere_mm3.unwrap_or(0.0);
    }
    if native {
        o.field.volumes.raw_mm3
    } else {
        field.map_or(0.0, |f| f.raw_mm3)
    }
}

impl Run<'_> {
    pub(crate) fn prepare(
        &mut self,
        stim_raw: &Stimulation,
        label: &str,
    ) -> Result<(Stimulation, [Option<LeadSpec>; 2]), Fail> {
        let raw_active: [bool; 2] = Side::ALL.map(|s| {
            stim_raw
                .sides
                .get(s)
                .sources
                .iter()
                .any(cfg::Source::is_active)
        });

        let mut leads: [Option<LeadSpec>; 2] = [None, None];
        for side in Side::ALL {
            let found = find_lead(&self.reco, self.opts, side);
            leads[side.index()] = if raw_active[side.index()] {
                found?
            } else {
                found.ok().flatten()
            };
        }
        let counts = Side::ALL.map(|s| {
            leads[s.index()]
                .as_ref()
                .map_or(stim_raw.contact_counts()[s.index()], |l| l.model.n_contacts)
        });
        let mut stim = stim_raw.clone();
        for note in normalise(&mut stim, counts) {
            self.say(note);
        }
        let report = check(&stim, counts);
        for w in &report.warnings {
            self.say(w.clone());
        }
        let errors: Vec<_> = report
            .errors
            .into_iter()
            .filter(|e| !(label.starts_with("gs_") && e.reason.contains(RESERVED_GS)))
            .collect();
        if !errors.is_empty() {
            return Err(stim_fail(cfg::StimError::Invalid(errors)));
        }
        Ok((stim, leads))
    }

    pub(crate) fn compute_runs(
        &mut self,
        stim: &Stimulation,
        leads: &[Option<LeadSpec>; 2],
        write_cache: bool,
    ) -> Result<[SideRun; 2], Fail> {
        let kind = stim.model;
        let label = stim.label.as_str();
        let mut runs: [SideRun; 2] = [
            SideRun {
                outcome: None,
                status: "no_stimulation",
                reason: None,
                fallback: None,
            },
            SideRun {
                outcome: None,
                status: "no_stimulation",
                reason: None,
                fallback: None,
            },
        ];
        for side in Side::ALL {
            if cancelled(self.cancel) {
                return Err(Fail::Cancelled);
            }
            let active: Vec<(u32, dbsloc_vat::SourceSetting)> = stim
                .sides
                .get(side)
                .sources
                .iter()
                .filter(|s| s.is_active())
                .map(|s| (s.index, engine_source(s)))
                .collect();
            if active.is_empty() {
                continue;
            }
            let Some(lead) = leads[side.index()].clone() else {
                self.say(format!(
                    "dbsloc: warning: vat: {label}: the {side} lead is not in the reconstruction; the {side} side is skipped"
                ));
                runs[side.index()].status = "no_lead";
                runs[side.index()].reason = Some("no reconstructed lead".to_owned());
                continue;
            };
            let efield = kind == ModelKind::Efield;
            let (gm, gm_identity, fallback) = if efield {
                self.resolve_gm(stim, &lead)
            } else {
                (None, json!("none"), None)
            };
            if let Some(why) = &fallback {
                self.say(format!(
                    "dbsloc: warning: vat: {label}: no grey-matter mask ({why}); homogeneous \u{3c3} = {} S/m",
                    stim.model_settings.conductivity.wm_s_per_m
                ));
            }
            self.say(format!("[vat] {label}: {side} lead, {}", kind.slug()));
            let req = SideRequest {
                side,
                lead: &lead,
                kind,
                ms: &stim.model_settings,
                sources: active,
                gm,
                gm_identity,
                grid: self.params.grid.clone(),
                cache: Some(CachePaths::new(&self.sub.subj_dir, &self.sub.subj_id, side)),
                write_cache,
                backend: self.opts.vat_backend,
                device: self.params.device,
            };
            let outcome = compute_side(&req, self.cancel)?;
            for n in &outcome.notes {
                self.say(n.clone());
            }
            for w in &outcome.warnings {
                if fallback.is_some() && w.contains("grey-matter mask") {
                    continue;
                }
                self.say(format!("dbsloc: warning: vat: {label}: {side}: {w}"));
            }
            runs[side.index()] = SideRun {
                outcome: Some(outcome),
                status: "computed",
                reason: None,
                fallback,
            };
        }

        Ok(runs)
    }

    fn process_label(&mut self, label: &str) -> Result<Done, Fail> {
        let stim_raw =
            cfg::load(&self.sub.subj_dir, &self.sub.subj_id, label).map_err(stim_fail)?;
        if matches!(stim_raw.model, ModelKind::Fastfield | ModelKind::Ossdbs) {
            return Err(Fail::new(
                "E_VAT_model",
                format!(
                    "{} outputs can be imported but not computed; use efield",
                    stim_raw.model.slug()
                ),
            ));
        }
        let kind = stim_raw.model;
        let settings_sha = settings_hash(&stim_raw)?;
        let ms_json = serde_json::to_value(&stim_raw.model_settings)
            .map_err(|e| Fail::new("E_VAT_settings", e.to_string()))?;
        let (stim, leads) = self.prepare(&stim_raw, label)?;

        if !self.opts.overwrite_approved && self.up_to_date(&stim_raw, &settings_sha, &ms_json) {
            self.say(format!("dbsloc: note: vat: {label} unchanged; kept"));
            return Ok(Done::Kept);
        }

        let t0 = Instant::now();
        let runs = self.compute_runs(&stim, &leads, true)?;
        let thr = (1000.0 * stim.model_settings.threshold_v_per_mm) as f32;
        let spacing = self.params.grid.output_spacing_mm;
        let mut warnings: Vec<String> = Vec::new();
        let any_computed = runs.iter().any(|r| r.outcome.is_some());
        let mut native_only = false;
        if any_computed && self.warp.is_err() {
            let why = self.warp.as_ref().err().cloned().unwrap_or_default();
            self.say(format!(
                "dbsloc: warning: vat: {label}: no normalization transform ({why}); template-space outputs skipped"
            ));
            warnings.push(format!(
                "no normalization transform ({why}); template-space outputs skipped"
            ));
            native_only = true;
        }
        let mut fields: [SideFields; 2] = [
            SideFields {
                native: None,
                template: None,
                reference: None,
                gradient_native: None,
                gradient_template: None,
            },
            SideFields {
                native: None,
                template: None,
                reference: None,
                gradient_native: None,
                gradient_template: None,
            },
        ];
        for side in Side::ALL {
            let i = side.index();
            let Some(o) = &runs[i].outcome else {
                continue;
            };
            let native = Field::from_native(&o.field);
            fields[i].gradient_native =
                (!o.arrows.is_empty()).then(|| native_gradient(&o.arrows, ARROW_LENGTH_MM));
            if has_image(&native) {
                if let Ok(w) = &self.warp {
                    let mut wl = Vec::new();
                    match warp::to_template(&native, thr, spacing, w, &mut wl) {
                        Ok(t) => {
                            fields[i].template = t;
                            if !o.arrows.is_empty() {
                                fields[i].gradient_template =
                                    w.gradient(&o.arrows, ARROW_LENGTH_MM).ok();
                            }
                        }
                        Err(e) => {
                            return Err(Fail::new(
                                "E_VAT_settings",
                                format!("the {side} field could not be warped: {e}"),
                            ));
                        }
                    }
                    for x in wl {
                        warnings.push(format!("{side}: {x}"));
                    }
                }
            } else {
                warnings.push(format!("{side}: no voxel exceeds the activation threshold"));
                self.say(format!(
                    "dbsloc: warning: vat: {label}: {side}: no voxel exceeds the activation threshold"
                ));
            }
            fields[i].native = Some(native);
        }

        let mut reference_chain: Option<(
            fastwarp_reg::transform::TransformChain,
            crate::space_bridge::Bridge,
        )> = None;
        let mut reference_note: Option<String> = None;
        if let Ok(w) = &self.warp {
            if w.space != REFERENCE_SPACE && any_computed {
                match self.opts.normalization_template.as_deref() {
                    Some(t) if t.is_file() => {
                        match crate::space_bridge::ensure_bridge(t, self.opts, self.cancel, self.tx)
                        {
                            Ok(Some(b)) => {
                                match fastwarp_reg::transform::read_transform_file(
                                    &b.inverse_composite_path(),
                                ) {
                                    Ok(chain) => reference_chain = Some((chain, b)),
                                    Err(e) => reference_note = Some(e.to_string()),
                                }
                            }
                            Ok(None) => reference_note = Some("the bridge is disabled".to_owned()),
                            Err(PipelineError::Cancelled) => return Err(Fail::Cancelled),
                            Err(e) => reference_note = Some(e.to_string()),
                        }
                    }
                    _ => reference_note = Some("no template image".to_owned()),
                }
                if let Some(why) = &reference_note {
                    self.say(format!(
                        "dbsloc: warning: vat: no reference-space copy ({why})"
                    ));
                    warnings.push(format!("no reference-space copy ({why})"));
                }
            }
        }
        if let Some((chain, bridge)) = &reference_chain {
            for side in Side::ALL {
                let i = side.index();
                let Some(t) = fields[i].template.clone() else {
                    continue;
                };
                let mut wl = Vec::new();
                let points = |p: &[[f64; 3]]| bridge.points_to_reference(p);
                match warp::to_reference(&t, thr, spacing, chain, &points, &mut wl) {
                    Ok(r) => fields[i].reference = r,
                    Err(e) => {
                        self.say(format!(
                            "dbsloc: warning: vat: {label}: {side}: no reference-space copy ({e})"
                        ));
                        warnings.push(format!("{side}: no reference-space copy ({e})"));
                    }
                }
            }
        }

        let atlas_in_template = self.warp.as_ref().is_ok_and(|w| w.space == ATLAS_SPACE);
        let stats_choice = self.opts.vat_stats_atlas.clone();
        let atlas: Option<StatsAtlas> = if any_computed {
            let sets = self.sets().to_vec();
            match StatsAtlas::resolve(&sets, stats_choice.as_deref()) {
                Ok(a) => Some(a),
                Err(why) => {
                    self.say(format!("dbsloc: note: vat: no statistics ({why})"));
                    warnings.push(format!("no statistics ({why})"));
                    None
                }
            }
        } else {
            None
        };
        let mut stats_template: Option<[SideStats; 2]> = None;
        let mut stats_reference: Option<[SideStats; 2]> = None;
        if let Some(a) = &atlas {
            let n = a.entries.len();
            let quiet = |_: String| {};
            let build = |pick: &dyn Fn(&SideFields) -> Option<&Field>| -> [SideStats; 2] {
                Side::ALL.map(|side| {
                    let amps = amplitudes(&stim, side);
                    match pick(&fields[side.index()]).filter(|f| has_image(f)) {
                        Some(f) => side_stats(a, side, f, thr, amps, &quiet),
                        None => SideStats::empty(side, n, amps),
                    }
                })
            };
            if atlas_in_template {
                stats_template = Some(build(&|f| f.template.as_ref()));
            } else if reference_chain.is_some() {
                stats_reference = Some(build(&|f| f.reference.as_ref()));
            }
        }

        self.write_label(
            &stim,
            &stim_raw,
            &runs,
            &fields,
            &WriteCtx {
                settings_sha: &settings_sha,
                ms_json: &ms_json,
                atlas: atlas.as_ref(),
                stats_template: stats_template.as_ref(),
                stats_reference: stats_reference.as_ref(),
                warnings: &warnings,
                reference_chain: reference_chain.is_some(),
            },
        )?;

        if let Some(a) = &atlas {
            if let Some(s) = stats_template.as_ref().or(stats_reference.as_ref()) {
                self.update_ea_stats(label, a, s);
            }
        }
        let vol = |i: usize| side_volume(&runs[i], fields[i].native.as_ref(), true);
        let backend = label_backend(kind, &runs);
        self.say(format!(
            "dbsloc: vat: {label}: {}, right {:.1} mm\u{b3}, left {:.1} mm\u{b3}, {backend}, {:.1} s",
            kind.slug(),
            vol(0),
            vol(1),
            t0.elapsed().as_secs_f64()
        ));
        Ok(Done::Computed { native_only })
    }
}

struct WriteCtx<'a> {
    settings_sha: &'a str,
    ms_json: &'a Value,
    atlas: Option<&'a StatsAtlas>,
    stats_template: Option<&'a [SideStats; 2]>,
    stats_reference: Option<&'a [SideStats; 2]>,
    warnings: &'a [String],
    reference_chain: bool,
}

impl Run<'_> {
    #[allow(clippy::too_many_arguments)]
    fn write_label(
        &mut self,
        stim: &Stimulation,
        stim_raw: &Stimulation,
        runs: &[SideRun; 2],
        fields: &[SideFields; 2],
        ctx: &WriteCtx<'_>,
    ) -> Result<(), Fail> {
        let kind = stim.model;
        let names = Names::new(&self.sub.subj_id, kind.slug());
        let root = cfg::stimulations_dir(&self.sub.subj_dir);
        let template_space = self.warp.as_ref().ok().map(|w| w.space.clone());
        let ref_folder = template_space
            .as_ref()
            .filter(|s| s.as_str() != REFERENCE_SPACE)
            .map(|_| REFERENCE_SPACE.to_owned());

        let mut folders = vec![Folder {
            space: "native".to_owned(),
            kind: "native",
            dir: root.join("native").join(&stim.label),
        }];
        if let Some(s) = &template_space {
            folders.push(Folder {
                space: s.clone(),
                kind: "template",
                dir: root.join(s).join(&stim.label),
            });
        }
        if let Some(s) = &ref_folder {
            folders.push(Folder {
                space: s.clone(),
                kind: "reference",
                dir: root.join(s).join(&stim.label),
            });
        }
        let mut targets: Vec<PathBuf> = Vec::new();
        for f in &folders {
            let with_stats = f.kind != "native";
            for n in names.owned_files(with_stats) {
                targets.push(f.dir.join(n));
            }
        }
        let staged =
            Staged::new(targets).map_err(|e| Fail::new("E_VAT_settings", e.to_string()))?;
        let result = self.write_into(&folders, &staged, stim, stim_raw, runs, fields, ctx, &names);
        match result {
            Ok(()) => staged
                .commit(None)
                .map_err(|e| Fail::new("E_VAT_settings", e.to_string())),
            Err(e) => {
                staged.discard();
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_into(
        &mut self,
        folders: &[Folder],
        staged: &Staged,
        stim: &Stimulation,
        stim_raw: &Stimulation,
        runs: &[SideRun; 2],
        fields: &[SideFields; 2],
        ctx: &WriteCtx<'_>,
        names: &Names,
    ) -> Result<(), Fail> {
        let kind = stim.model;
        let io = |e: PipelineError| Fail::new("E_VAT_settings", e.to_string());
        let native_codes = Codes::of_image(self.sub.anchor_in_use().as_deref());
        let template_codes = Codes::of_image(self.opts.normalization_template.as_deref());
        let inputs_paths = self.input_paths(stim_raw);
        let inputs = snap_input_paths(&inputs_paths).map_err(|e| Fail::new("E_VAT_settings", e))?;
        let threshold_v_per_m = 1000.0 * stim.model_settings.threshold_v_per_mm;
        for Folder {
            space,
            kind: space_kind,
            dir,
        } in folders
        {
            let (codes, native) = match *space_kind {
                "native" => (native_codes, true),
                _ => (template_codes, false),
            };
            let mut volumes = [0.0; 2];
            let mut sides_json = serde_json::Map::new();
            let mut grid_json = serde_json::Map::new();
            for side in Side::ALL {
                let i = side.index();
                let run = &runs[i];
                let field: Option<&Field> = match *space_kind {
                    "native" => fields[i].native.as_ref(),
                    "template" => fields[i].template.as_ref(),
                    _ => fields[i].reference.as_ref(),
                };
                volumes[i] = side_volume(run, field, native);
                let mut entry = serde_json::Map::new();
                entry.insert("status".into(), json!(run.status));
                if let Some(r) = &run.reason {
                    entry.insert("reason".into(), json!(r));
                }
                if let Some(f) = &run.fallback {
                    entry.insert("conductivity_fallback".into(), json!(f));
                }
                if let (Some(o), Some(f)) = (&run.outcome, field) {
                    entry.insert("volume_raw_mm3".into(), json!(volumes[i]));
                    entry.insert("volume_final_mm3".into(), json!(f.final_mm3));
                    let mut srcs = Vec::new();
                    for (k, idx) in o.source_indices.iter().enumerate() {
                        let src = stim
                            .sides
                            .get(side)
                            .sources
                            .iter()
                            .find(|s| s.index == *idx);
                        let mut v = json!({
                            "index": idx,
                            "mode": src.map(|s| format!("{:?}", s.mode).to_lowercase()),
                            "amplitude": src.map(|s| s.amplitude),
                            "volume_raw_mm3": if native {
                                o.field.volumes.per_source_raw_mm3.get(k).copied()
                            } else {
                                None
                            },
                        });
                        if native && !o.analytic {
                            v["contact_currents_ma"] = json!(o.field.contact_currents_ma.get(k));
                            v["contact_potentials_v"] = json!(o.field.contact_potentials_v.get(k));
                        }
                        srcs.push(v);
                    }
                    entry.insert("sources".into(), Value::Array(srcs));
                    grid_json.insert(
                        side.name().to_owned(),
                        json!({
                            "spacing_mm": self.params.grid.output_spacing_mm,
                            "dims": f.dims,
                            "affine_4x4": affine_json(&f.affine),
                        }),
                    );
                    if has_image(f) {
                        let bin = dir.join(names.binary_nii(side));
                        write_binary_nii(&staged.tmp(&bin), f.dims, &f.binary, &f.affine, codes)
                            .map_err(io)?;
                        if !o.analytic {
                            let ef = dir.join(names.efield_nii(side));
                            write_f32_nii(&staged.tmp(&ef), f.dims, &f.e_mag, &f.affine, codes)
                                .map_err(io)?;
                            let gauss = efield_gauss(&f.e_mag, &f.binary_raw);
                            let eg = dir.join(names.efieldgauss_nii(side));
                            write_f32_nii(&staged.tmp(&eg), f.dims, &gauss, &f.affine, codes)
                                .map_err(io)?;
                        }
                        let surface = mesh::surface(&f.binary, f.dims, &f.affine);
                        let gradient = if o.analytic || *space_kind == "reference" {
                            None
                        } else if native {
                            fields[i].gradient_native.as_ref()
                        } else {
                            fields[i].gradient_template.as_ref()
                        };
                        let bytes = hemi_mat_bytes(&surface, volumes[i], gradient).map_err(io)?;
                        write_bytes(&staged.tmp(&dir.join(names.hemi_mat(side))), &bytes)
                            .map_err(io)?;
                    }
                }
                sides_json.insert(side.name().to_owned(), Value::Object(entry));
            }
            let mat = dir.join(names.stimparameters_mat());
            write_stimparameters_mat(&staged.tmp(&mat), stim, volumes).map_err(stim_fail)?;

            let backend = label_backend(kind, runs);
            let adapter = runs
                .iter()
                .filter_map(|r| r.outcome.as_ref()?.adapter.clone())
                .next();
            let mut solver_dims = serde_json::Map::new();
            let mut iterations = 0usize;
            let mut residual = 0.0f64;
            let mut cache_state = "none";
            for side in Side::ALL {
                if let Some(o) = &runs[side.index()].outcome {
                    if let Some(d) = o.solver_dims {
                        solver_dims.insert(side.name().to_owned(), json!(d));
                    }
                    iterations = iterations.max(o.iterations);
                    residual = residual.max(o.rel_residual);
                    if o.basis_cache == "built" || cache_state == "none" {
                        cache_state = o.basis_cache;
                    }
                }
            }
            let seconds: f64 = runs
                .iter()
                .filter_map(|r| r.outcome.as_ref().map(|o| o.seconds))
                .sum();
            let sidecar = json!({
                "schema": "dbsloc-vta",
                "schema_version": 1,
                "label": stim.label,
                "model": kind.slug(),
                "model_id": format!("{kind:?}").to_lowercase(),
                "space": space,
                "software": crate::version::software_stamp(),
                "run_options": serde_json::to_value(self.opts).unwrap_or(Value::Null),
                "settings_sha256": ctx.settings_sha,
                "model_settings": ctx.ms_json,
                "threshold_v_per_m": threshold_v_per_m,
                "grid": Value::Object(grid_json),
                "engine": {
                    "backend": backend,
                    "adapter": adapter,
                    "solver_grids": Value::Object(solver_dims),
                    "iterations": iterations,
                    "relative_residual": residual,
                    "basis_cache": cache_state,
                    "seconds": seconds,
                },
                "engine_version": ENGINE_VERSION,
                "stats_atlas_request": self.opts.vat_stats_atlas.clone().unwrap_or_default(),
                "sides": Value::Object(sides_json),
                "inputs": inputs,
                "warnings": ctx.warnings,
                "lead_dbs": {
                    "model_name": kind.display_name(),
                    "slug": kind.slug(),
                },
                "reference_space_copy": ctx.reference_chain,
            });
            let vta = dir.join(names.vta_json());
            write_bytes(
                &staged.tmp(&vta),
                (serde_json::to_string_pretty(&sidecar)
                    .map_err(|e| Fail::new("E_VAT_settings", e.to_string()))?
                    + "\n")
                    .as_bytes(),
            )
            .map_err(io)?;

            let stats = match *space_kind {
                "template" => ctx.stats_template,
                "reference" => ctx.stats_reference,
                _ => None,
            };
            if let (Some(a), Some(s)) = (ctx.atlas, stats) {
                let record = stats_json(a, &stim.label, kind.slug(), ctx.settings_sha, s);
                let path = dir.join(names.vtastats_json());
                write_bytes(
                    &staged.tmp(&path),
                    (serde_json::to_string_pretty(&record)
                        .map_err(|e| Fail::new("E_VAT_settings", e.to_string()))?
                        + "\n")
                        .as_bytes(),
                )
                .map_err(io)?;
            }
        }
        Ok(())
    }

    fn update_ea_stats(&self, label: &str, atlas: &StatsAtlas, sides: &[SideStats; 2]) {
        let path = self
            .sub
            .subj_dir
            .join(format!("sub-{}_desc-stats.mat", self.sub.subj_id));
        match ea_stats_bytes(&path, atlas, label, sides) {
            Ok(bytes) => {
                if let Err(e) = write_replacing(&path, &bytes) {
                    self.say(format!(
                        "dbsloc: note: vat: sub-{}_desc-stats.mat not updated ({e}); see sub-{}_desc-vtastats.json",
                        self.sub.subj_id, self.sub.subj_id
                    ));
                }
            }
            Err(why) => self.say(format!(
                "dbsloc: note: vat: sub-{}_desc-stats.mat not updated ({why}); see sub-{}_desc-vtastats.json",
                self.sub.subj_id, self.sub.subj_id
            )),
        }
    }
}

struct Folder {
    space: String,
    kind: &'static str,
    dir: PathBuf,
}

impl<'a> Run<'a> {
    pub(crate) fn open(
        sub: &'a Subject,
        opts: &'a RunOptions,
        params: &'a VatParams,
        cancel: Option<&'a AtomicBool>,
        tx: Option<&'a mpsc::Sender<RunEvent>>,
    ) -> PipelineResult<Run<'a>> {
        let reco_path = sub.reconstruction_json();
        if !reco_path.is_file() {
            return Err(PipelineError::Prereq(
                "vat: no reconstruction (run reconstruct)".to_owned(),
            ));
        }
        let text = std::fs::read_to_string(&reco_path).map_err(|e| {
            PipelineError::Message(format!(
                "vat: cannot read the Reco JSON {}: {e}",
                reco_path.display()
            ))
        })?;
        let reco: Value = serde_json::from_str(&text).map_err(|e| {
            PipelineError::Message(format!(
                "vat: cannot parse the Reco JSON {}: {e}",
                reco_path.display()
            ))
        })?;
        Ok(Run {
            sub,
            opts,
            params,
            reco,
            warp: SpaceWarp::load(sub).map_err(|e| e.0),
            atlas_sets: None,
            warp_sha: None,
            cancel,
            tx,
        })
    }
}

/// Runs the stage with the production parameters.
pub fn run_vat(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    run_vat_with(sub, opts, &VatParams::default(), cancel, tx)
}

/// Runs the stage with explicit parameters.
pub fn run_vat_with(
    sub: &Subject,
    opts: &RunOptions,
    params: &VatParams,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    let labels = discover_labels(sub, opts)?;
    if labels.is_empty() {
        return Ok(StageOutcome::skipped("vat: no stimulation settings"));
    }
    let prefs = opts_with_prefs_model(sub, opts, tx);
    let eff = prefs.as_ref().unwrap_or(opts);
    let mut run = Run::open(sub, eff, params, cancel, tx)?;
    let released = ReleaseGpu(eff.vat_backend != crate::options::VatBackend::Cpu);
    let mut computed = 0usize;
    let mut native_only = false;
    let mut failures: Vec<LabelError> = Vec::new();
    for label in &labels {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        match run.process_label(label) {
            Ok(Done::Computed { native_only: n }) => {
                computed += 1;
                native_only |= n;
            }
            Ok(Done::Kept) => {}
            Err(Fail::Cancelled) => return Err(PipelineError::Cancelled),
            Err(fail) => failures.push(LabelError {
                label: label.clone(),
                fail,
            }),
        }
    }
    if !failures.is_empty() {
        let text = failures
            .iter()
            .map(|f| match &f.fail {
                Fail::Code { code, message } => format!("vat: {code}: {}: {message}", f.label),
                Fail::Cancelled => String::new(),
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(PipelineError::Message(text));
    }
    if computed > 0 {
        let lock = stage_approval_path(&sub.locks_dir(), STAGE);
        if let Err(e) = write_approval(
            &lock,
            false,
            Some(ENGINE_VERSION),
            &lock_input_paths(sub),
            None,
        ) {
            log(
                tx,
                format!("dbsloc: warning: vat: the lock was not written: {e}"),
            );
        }
    }
    drop(released);
    let suffix = if native_only { " (native only)" } else { "" };
    Ok(StageOutcome::done(format!(
        "ok:vat:{computed}/{}{suffix}",
        labels.len()
    )))
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
