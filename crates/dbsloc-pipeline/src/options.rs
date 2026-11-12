// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! ``RunOptions`` — the one per-run configuration for the GUI, `dbsloc_pipeline_runner` and
//! `dbsloc run` (Python builds it as JSON).
//!
//! See ``specs/00_overview.md`` §4.3 for the full field spec.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use crate::bids::PostopModality;
use crate::reg_backend::RegBackend;
use crate::stages::Stage;

pub use dbsloc_protocol::{NormGrid, DEFAULT_NORM_ISOTROPIC_MM};

pub const NORM_METHOD_ANTS_SYN: &str = "ants_syn";

/// Execution engine of the `vat` stage (`specs/07` §7.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VatBackend {
    /// The GPU engine when it fits, else the CPU engine.
    #[default]
    Auto,
    /// The GPU engine only.
    Wgpu,
    /// The f64 CPU engine.
    Cpu,
}

impl VatBackend {
    /// Parses `auto`, `wgpu` or `cpu` (case-insensitive).
    pub fn parse(raw: &str) -> Result<VatBackend, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(VatBackend::Auto),
            "wgpu" => Ok(VatBackend::Wgpu),
            "cpu" => Ok(VatBackend::Cpu),
            _ => Err(format!(
                "unknown vat backend {raw:?} (known: auto, wgpu, cpu)"
            )),
        }
    }

    /// The lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            VatBackend::Auto => "auto",
            VatBackend::Wgpu => "wgpu",
            VatBackend::Cpu => "cpu",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormMethod {
    AntsSyn,
}

impl NormMethod {
    pub fn parse(raw: &str) -> Result<NormMethod, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            NORM_METHOD_ANTS_SYN => Ok(NormMethod::AntsSyn),
            _ => Err(format!(
                "E_NORM_METHOD: unknown normalization method {raw:?} (known: {NORM_METHOD_ANTS_SYN})"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NormMethod::AntsSyn => NORM_METHOD_ANTS_SYN,
        }
    }
}

/// Per-run configuration; serialised as JSON for the Python↔Rust protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOptions {
    /// BIDS dataset root.
    pub bids_dir: PathBuf,
    /// Subject label (without `sub-` prefix).
    pub subject: String,
    /// Ordered list of stages to execute.
    #[serde(default = "default_stages")]
    pub stages: Vec<Stage>,
    /// Normalization template NIfTI path (required for normalize and later stages).
    #[serde(default)]
    pub normalization_template: Option<PathBuf>,
    #[serde(default)]
    pub normalization_template_t2: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub norm_multispectral: bool,
    /// Normalization method (e.g. `"ants_syn"`, `"synthmorph"`).
    #[serde(default = "default_norm_method")]
    pub norm_method: String,
    /// Registration execution backend of the `ants_syn` normalization (`auto`, `wgpu`, `ants`;
    /// `specs/04` §5.4). A missing JSON field means `auto`.
    #[serde(default)]
    pub reg_backend: RegBackend,
    /// Overrides [`RunOptions::reg_backend`] for preprocessing (N4) and precoreg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reg_backend_preprocess: Option<RegBackend>,
    /// Overrides [`RunOptions::reg_backend`] for coregistration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reg_backend_coregister: Option<RegBackend>,
    /// Overrides [`RunOptions::reg_backend`] for normalization, the space bridge and the
    /// normalization warp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reg_backend_normalize: Option<RegBackend>,
    /// Reconstruction method (`auto`, `trac_core`, `pacer`).
    #[serde(default = "default_recon_method")]
    pub recon_method: String,
    /// Use bright-metal segmentation for reconstruction.
    #[serde(default)]
    pub recon_bright_metal: bool,
    /// Electrode catalogue name or `matfname` (CLI `--el-model`). No default: reconstruct
    /// geometry and its tolerances derive from it, so an unset model fails the stage.
    #[serde(default)]
    pub el_model: Option<String>,
    #[serde(default)]
    pub el_model_error: Option<String>,
    /// Maximum ANTs threads; `None` = no cap.
    #[serde(default)]
    pub ants_max_threads: Option<u32>,
    #[serde(default)]
    pub ants_extra: Vec<String>,
    /// Isotropic SyN registration grid (`specs/04` §5.1 step 2). A missing JSON field (or the
    /// string `"template"`) means the template's own voxel size, a number an explicit grid in
    /// millimetres and an explicit `null` the full template grid.
    #[serde(default, skip_serializing_if = "NormGrid::is_template")]
    pub norm_isotropic_mm: NormGrid,
    /// Skip opening the interactive checkreg viewer (still writes PNGs).
    #[serde(default)]
    pub headless: bool,
    #[serde(default = "default_true")]
    pub checkreg: bool,
    /// Skip opening the 3D scene viewer after reconstruct.
    #[serde(default)]
    pub no_scene3d: bool,
    /// Plan/lock only — do not run stage bodies.
    #[serde(default)]
    pub plan_only: bool,
    /// Stop after the first failed stage. `dbsloc run` and the runner fill an unset value from
    /// `DBSLOC_STOP_ON_ERROR`, else from `checkreg` ([`RunOptions::apply_env`]).
    #[serde(default = "default_true")]
    pub stop_on_error: bool,
    /// Ignore existing approved stage locks and rerun selected stages.
    #[serde(default)]
    pub overwrite_approved: bool,
    #[serde(default)]
    pub fingerprint_locks: bool,
    #[serde(default = "default_true")]
    pub use_index: bool,
    /// Output root that holds `sub-<id>/` (locks, derivatives, prefs); `None` =
    /// `<bids_dir>/derivatives/leaddbs` ([`crate::paths::derivatives_root`]).
    #[serde(default)]
    pub output_dir: Option<PathBuf>,
    /// Post-op images to use when a subject has both a CT and an MR (`specs/01` §10 step 5).
    #[serde(default)]
    pub postop_modality: PostopModality,
    /// Directory of further Lead-DBS atlas folders listed in the scene manifest (`specs/11` §1,
    /// a Lead-DBS install root or its `atlases` directory); `None` = only the bundled sets.
    #[serde(default)]
    pub leaddbs_atlases_dir: Option<PathBuf>,
    /// Engine of the `vat` stage (`specs/07` §7.7).
    #[serde(default)]
    pub vat_backend: VatBackend,
    /// Stimulation labels the `vat` stage computes; empty = every label of the subject.
    #[serde(default)]
    pub vat_labels: Vec<String>,
    /// Atlas set of the `vat` overlap statistics; `None` = the default atlas set.
    #[serde(default)]
    pub vat_stats_atlas: Option<String>,
}

impl RunOptions {
    pub fn new(bids_dir: impl Into<PathBuf>, subject: impl Into<String>) -> Self {
        RunOptions {
            bids_dir: bids_dir.into(),
            subject: subject.into(),
            stages: default_stages(),
            normalization_template: None,
            normalization_template_t2: None,
            norm_multispectral: true,
            norm_method: default_norm_method(),
            reg_backend: RegBackend::Auto,
            reg_backend_preprocess: None,
            reg_backend_coregister: None,
            reg_backend_normalize: None,
            recon_method: default_recon_method(),
            recon_bright_metal: false,
            el_model: None,
            el_model_error: None,
            ants_max_threads: None,
            ants_extra: Vec::new(),
            norm_isotropic_mm: NormGrid::Template,
            headless: false,
            checkreg: true,
            no_scene3d: false,
            plan_only: false,
            stop_on_error: true,
            overwrite_approved: false,
            fingerprint_locks: false,
            use_index: true,
            output_dir: None,
            postop_modality: PostopModality::Auto,
            leaddbs_atlases_dir: None,
            vat_backend: VatBackend::Auto,
            vat_labels: Vec::new(),
            vat_stats_atlas: None,
        }
    }

    /// Effective absolute output root of this run.
    pub fn derivatives_root(&self) -> PathBuf {
        crate::paths::derivatives_root(&self.bids_dir, self.output_dir.as_deref())
    }

    pub fn resolve_subject(&self) -> Result<crate::bids::Subject, crate::bids::BidsError> {
        crate::bids::resolve_subject_for(
            &self.bids_dir,
            self.output_dir.as_deref(),
            &self.subject,
            self.use_index,
            self.postop_modality,
        )
    }

    /// Registration engine request of one step: the step's override, else the run-wide default.
    pub fn backend_for(&self, stage: Stage) -> RegBackend {
        let over = match stage {
            Stage::Preprocess | Stage::Precoreg => self.reg_backend_preprocess,
            Stage::Coregister => self.reg_backend_coregister,
            Stage::Normalize => self.reg_backend_normalize,
            _ => None,
        };
        over.unwrap_or(self.reg_backend)
    }

    pub fn with_resolved_template(&self) -> RunOptions {
        let mut o = self.clone();
        o.normalization_template =
            crate::templates::select_template(self.normalization_template.as_deref())
                .map(|r| r.path);
        o.normalization_template_t2 = crate::templates::select_template_t2(
            self.normalization_template_t2.as_deref(),
            o.normalization_template.as_deref(),
        )
        .map(|r| r.path);
        o
    }

    pub fn apply_env(
        &mut self,
        explicit: &dyn Fn(&str) -> bool,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<(), String> {
        let var = |field: &str| -> Option<(&'static str, String)> {
            if explicit(field) {
                return None;
            }
            let name = RUN_OPTION_ENV
                .iter()
                .find(|(_, f)| *f == field)
                .map(|(n, _)| *n)?;
            lookup(name).map(|raw| (name, raw))
        };
        if let Some(v) = var("headless").and_then(|(_, raw)| env_flag(&raw, &ENV_ON)) {
            self.headless = v;
        }
        if let Some(v) = var("checkreg").and_then(|(_, raw)| env_flag(&raw, &ENV_ON)) {
            self.checkreg = v;
        }
        if let Some(v) = var("no_scene3d").and_then(|(_, raw)| env_flag(&raw, &ENV_ON_SCENE3D)) {
            self.no_scene3d = !v;
        }
        if let Some((name, raw)) = var("reg_backend") {
            if let Some(b) = env_backend(name, &raw)? {
                self.reg_backend = b;
            }
        }
        if let Some((name, raw)) = var("reg_backend_preprocess") {
            if let Some(b) = env_backend(name, &raw)? {
                self.reg_backend_preprocess = Some(b);
            }
        }
        if let Some((name, raw)) = var("reg_backend_coregister") {
            if let Some(b) = env_backend(name, &raw)? {
                self.reg_backend_coregister = Some(b);
            }
        }
        if let Some((name, raw)) = var("reg_backend_normalize") {
            if let Some(b) = env_backend(name, &raw)? {
                self.reg_backend_normalize = Some(b);
            }
        }
        if let Some((name, raw)) = var("norm_isotropic_mm") {
            let raw = raw.trim();
            if !raw.is_empty() {
                self.norm_isotropic_mm =
                    NormGrid::parse(raw).map_err(|e| format!("{name}: {e}"))?;
            }
        }
        if let Some((name, raw)) = var("norm_method") {
            if let Some(m) = env_choice(name, &raw, &[NORM_METHOD_ANTS_SYN])? {
                self.norm_method = m;
            }
        }
        if let Some((name, raw)) = var("ants_max_threads") {
            let raw = raw.trim();
            if !raw.is_empty() {
                let n: i64 = raw
                    .parse()
                    .map_err(|_| format!("{name}: expected an integer, got '{raw}'"))?;
                self.ants_max_threads = Some(u32::try_from(n.max(1)).unwrap_or(u32::MAX));
            }
        }
        if let Some((_, raw)) = var("output_dir") {
            let raw = raw.trim();
            if !raw.is_empty() {
                self.output_dir = Some(crate::paths::absolute(std::path::Path::new(raw)));
            }
        }
        if let Some((name, raw)) = var("postop_modality") {
            if !raw.trim().is_empty() {
                self.postop_modality =
                    PostopModality::parse(&raw).map_err(|e| format!("{name}: {e}"))?;
            }
        }
        if let Some((_, raw)) = var("leaddbs_atlases_dir") {
            let raw = raw.trim();
            if !raw.is_empty() {
                self.leaddbs_atlases_dir = Some(crate::paths::absolute(std::path::Path::new(raw)));
            }
        }
        if let Some((name, raw)) = var("vat_backend") {
            if !raw.trim().is_empty() {
                self.vat_backend = VatBackend::parse(&raw).map_err(|e| format!("{name}: {e}"))?;
            }
        }
        if let Some((_, raw)) = var("vat_labels") {
            let labels: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            if !labels.is_empty() {
                self.vat_labels = labels;
            }
        }
        if let Some((_, raw)) = var("vat_stats_atlas") {
            let raw = raw.trim();
            if !raw.is_empty() {
                self.vat_stats_atlas = Some(raw.to_owned());
            }
        }
        if !explicit("stop_on_error") {
            self.stop_on_error = var("stop_on_error")
                .and_then(|(_, raw)| env_flag(&raw, &ENV_ON))
                .unwrap_or(self.checkreg);
        }
        Ok(())
    }

    pub fn from_runner_json(
        text: &str,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<RunOptions, String> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("invalid RunOptions JSON: {e}"))?;
        let keys: Vec<String> = value
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let mut opts: RunOptions =
            serde_json::from_value(value).map_err(|e| format!("invalid RunOptions JSON: {e}"))?;
        opts.apply_env(&|field| keys.iter().any(|k| k == field), lookup)?;
        Ok(opts)
    }
}

pub const RUN_OPTION_ENV: [(&str, &str); 17] = [
    ("DBSLOC_HEADLESS", "headless"),
    ("DBSLOC_OPEN_CHECKREG", "checkreg"),
    ("DBSLOC_OPEN_SCENE3D", "no_scene3d"),
    ("DBSLOC_STOP_ON_ERROR", "stop_on_error"),
    ("DBSLOC_REG_BACKEND", "reg_backend"),
    ("DBSLOC_PREPROCESS_BACKEND", "reg_backend_preprocess"),
    ("DBSLOC_COREGISTER_BACKEND", "reg_backend_coregister"),
    ("DBSLOC_NORMALIZE_BACKEND", "reg_backend_normalize"),
    ("DBSLOC_NORM_ISOTROPIC_MM", "norm_isotropic_mm"),
    ("DBSLOC_NORM_METHOD", "norm_method"),
    ("DBSLOC_ANTS_MAX_THREADS", "ants_max_threads"),
    (crate::paths::OUTPUT_DIR_ENV, "output_dir"),
    ("DBSLOC_POSTOP_MODALITY", "postop_modality"),
    (
        crate::atlas_catalog::LEADDBS_ATLASES_ENV,
        "leaddbs_atlases_dir",
    ),
    ("DBSLOC_VAT_BACKEND", "vat_backend"),
    ("DBSLOC_VAT_LABELS", "vat_labels"),
    ("DBSLOC_VAT_STATS_ATLAS", "vat_stats_atlas"),
];

const ENV_ON: [&str; 4] = ["1", "true", "yes", "on"];
const ENV_ON_SCENE3D: [&str; 5] = ["1", "true", "yes", "on", "always"];
const ENV_OFF: [&str; 4] = ["0", "false", "no", "off"];
const ENV_BACKENDS: [&str; 3] = ["auto", "wgpu", "ants"];

fn env_flag(raw: &str, on: &[&str]) -> Option<bool> {
    let v = raw.trim().to_ascii_lowercase();
    if on.contains(&v.as_str()) {
        Some(true)
    } else if ENV_OFF.contains(&v.as_str()) {
        Some(false)
    } else {
        None
    }
}

fn env_choice(name: &str, raw: &str, allowed: &[&str]) -> Result<Option<String>, String> {
    let v = raw.trim().to_ascii_lowercase();
    if v.is_empty() {
        return Ok(None);
    }
    if allowed.contains(&v.as_str()) {
        Ok(Some(v))
    } else {
        Err(format!(
            "{name}: expected one of {}, got '{v}'",
            allowed.join(", ")
        ))
    }
}

fn env_backend(name: &str, raw: &str) -> Result<Option<RegBackend>, String> {
    match env_choice(name, raw, &ENV_BACKENDS)? {
        None => Ok(None),
        Some(v) => RegBackend::parse(&v)
            .map(Some)
            .map_err(|e| format!("{name}: {e}")),
    }
}

fn default_stages() -> Vec<Stage> {
    Stage::default_sequence().to_vec()
}

fn default_norm_method() -> String {
    "ants_syn".to_owned()
}

fn default_recon_method() -> String {
    "auto".to_owned()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(extra: &str) -> RunOptions {
        let text = format!(r#"{{"bids_dir": "/data", "subject": "01"{extra}}}"#);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn norm_isotropic_defaults_to_the_template_resolution() {
        assert_eq!(
            RunOptions::new("/data", "01").norm_isotropic_mm,
            NormGrid::Template
        );
        assert_eq!(DEFAULT_NORM_ISOTROPIC_MM, 1.0);
    }

    #[test]
    fn missing_norm_isotropic_field_means_the_template_resolution() {
        assert_eq!(parse("").norm_isotropic_mm, NormGrid::Template);
        assert_eq!(
            parse(r#", "norm_isotropic_mm": "template""#).norm_isotropic_mm,
            NormGrid::Template
        );
    }

    #[test]
    fn null_norm_isotropic_means_full_template_grid() {
        assert_eq!(
            parse(r#", "norm_isotropic_mm": null"#).norm_isotropic_mm,
            NormGrid::Full
        );
    }

    #[test]
    fn explicit_norm_isotropic_is_kept() {
        assert_eq!(
            parse(r#", "norm_isotropic_mm": 1.5"#).norm_isotropic_mm,
            NormGrid::Mm(1.5)
        );
    }

    #[test]
    fn the_template_default_is_omitted_from_json_and_the_rest_round_trips() {
        let o = RunOptions::new("/data", "01");
        let text = serde_json::to_string(&o).unwrap_or_default();
        assert!(!text.contains("norm_isotropic_mm"), "{text}");
        for grid in [NormGrid::Mm(0.7), NormGrid::Full, NormGrid::Template] {
            let mut o = RunOptions::new("/data", "01");
            o.norm_isotropic_mm = grid;
            let text = serde_json::to_string(&o).unwrap_or_default();
            let back: RunOptions = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(back.norm_isotropic_mm, grid);
        }
    }

    #[test]
    fn norm_method_defaults_to_ants_syn_and_keeps_an_explicit_value() {
        assert_eq!(RunOptions::new("/data", "01").norm_method, "ants_syn");
        assert_eq!(parse("").norm_method, "ants_syn");
        assert_eq!(
            parse(r#", "norm_method": "ants_syn""#).norm_method,
            "ants_syn"
        );
    }

    #[test]
    fn unknown_norm_method_is_an_e_norm_method_error() {
        let e = NormMethod::parse("fireants").unwrap_err();
        assert!(
            e.starts_with("E_NORM_METHOD") && e.contains("ants_syn"),
            "{e}"
        );
    }

    #[test]
    fn reg_backend_defaults_to_auto_and_parses_the_three_names() {
        assert_eq!(RunOptions::new("/data", "01").reg_backend, RegBackend::Auto);
        assert_eq!(parse("").reg_backend, RegBackend::Auto);
        for (raw, want) in [
            ("auto", RegBackend::Auto),
            ("wgpu", RegBackend::Wgpu),
            ("ants", RegBackend::Ants),
        ] {
            assert_eq!(
                parse(&format!(r#", "reg_backend": "{raw}""#)).reg_backend,
                want
            );
        }
    }

    #[test]
    fn invalid_reg_backend_is_rejected_when_the_options_are_parsed() {
        let text = r#"{"bids_dir": "/data", "subject": "01", "reg_backend": "cuda"}"#;
        let e = serde_json::from_str::<RunOptions>(text).unwrap_err();
        assert!(e.to_string().contains("cuda"), "{e}");
    }

    #[test]
    fn reg_backend_survives_a_json_round_trip() {
        let mut o = RunOptions::new("/data", "01");
        o.reg_backend = RegBackend::Wgpu;
        let text = serde_json::to_string(&o).unwrap_or_default();
        assert!(text.contains(r#""reg_backend":"wgpu""#), "{text}");
        let back: RunOptions = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.reg_backend, RegBackend::Wgpu);
    }

    #[test]
    fn backend_for_uses_the_default_and_each_override() {
        let mut o = RunOptions::new("/data", "01");
        o.reg_backend = RegBackend::Ants;
        for st in [
            Stage::Preprocess,
            Stage::Precoreg,
            Stage::Coregister,
            Stage::Normalize,
        ] {
            assert_eq!(o.backend_for(st), RegBackend::Ants);
        }
        o.reg_backend_preprocess = Some(RegBackend::Wgpu);
        assert_eq!(o.backend_for(Stage::Preprocess), RegBackend::Wgpu);
        assert_eq!(o.backend_for(Stage::Precoreg), RegBackend::Wgpu);
        assert_eq!(o.backend_for(Stage::Coregister), RegBackend::Ants);
        assert_eq!(o.backend_for(Stage::Normalize), RegBackend::Ants);
        o.reg_backend_coregister = Some(RegBackend::Auto);
        assert_eq!(o.backend_for(Stage::Coregister), RegBackend::Auto);
        assert_eq!(o.backend_for(Stage::Normalize), RegBackend::Ants);
        o.reg_backend_normalize = Some(RegBackend::Wgpu);
        assert_eq!(o.backend_for(Stage::Normalize), RegBackend::Wgpu);
        assert_eq!(o.backend_for(Stage::Reconstruct), RegBackend::Ants);
    }

    #[test]
    fn per_step_overrides_are_optional_in_json() {
        let o = parse("");
        assert_eq!(o.reg_backend_preprocess, None);
        assert_eq!(o.reg_backend_coregister, None);
        assert_eq!(o.reg_backend_normalize, None);
        let text = serde_json::to_string(&o).unwrap_or_default();
        assert!(!text.contains("reg_backend_"), "{text}");
        let o = parse(r#", "reg_backend_normalize": "wgpu", "reg_backend_coregister": "ants""#);
        assert_eq!(o.reg_backend_normalize, Some(RegBackend::Wgpu));
        assert_eq!(o.reg_backend_coregister, Some(RegBackend::Ants));
        let text = serde_json::to_string(&o).unwrap_or_default();
        let back: RunOptions = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.reg_backend_normalize, Some(RegBackend::Wgpu));
        assert_eq!(back.reg_backend_preprocess, None);
    }

    #[test]
    fn full_template_grid_survives_a_json_round_trip() {
        let mut o = RunOptions::new("/data", "01");
        o.norm_isotropic_mm = NormGrid::Full;
        let text = serde_json::to_string(&o).unwrap_or_default();
        assert!(text.contains(r#""norm_isotropic_mm":null"#), "{text}");
        let back: RunOptions = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.norm_isotropic_mm, NormGrid::Full);
    }

    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    fn nothing_explicit(_: &str) -> bool {
        false
    }

    #[test]
    fn every_run_option_variable_names_a_serialised_field() {
        let mut o = RunOptions::new("/data", "01");
        o.reg_backend_preprocess = Some(RegBackend::Ants);
        o.reg_backend_coregister = Some(RegBackend::Ants);
        o.reg_backend_normalize = Some(RegBackend::Ants);
        o.norm_isotropic_mm = NormGrid::Full;
        let v = serde_json::to_value(&o).unwrap_or_default();
        for (name, field) in RUN_OPTION_ENV {
            assert!(name.starts_with("DBSLOC_"), "{name}");
            assert!(v.get(field).is_some(), "{field}");
        }
    }

    #[test]
    fn the_environment_fills_unset_fields() {
        let mut o = RunOptions::new("/data", "01");
        let env = env_of(&[
            ("DBSLOC_HEADLESS", "yes"),
            ("DBSLOC_OPEN_CHECKREG", "off"),
            ("DBSLOC_OPEN_SCENE3D", "0"),
            ("DBSLOC_REG_BACKEND", " ANTs "),
            ("DBSLOC_PREPROCESS_BACKEND", "wgpu"),
            ("DBSLOC_COREGISTER_BACKEND", "auto"),
            ("DBSLOC_NORMALIZE_BACKEND", "WGPU"),
            ("DBSLOC_NORM_ISOTROPIC_MM", "full"),
            ("DBSLOC_NORM_METHOD", "ants_syn"),
            ("DBSLOC_ANTS_MAX_THREADS", "-3"),
        ]);
        o.apply_env(&nothing_explicit, &env).unwrap();
        assert!(o.headless);
        assert!(!o.checkreg);
        assert!(o.no_scene3d);
        assert_eq!(o.reg_backend, RegBackend::Ants);
        assert_eq!(o.reg_backend_preprocess, Some(RegBackend::Wgpu));
        assert_eq!(o.reg_backend_coregister, Some(RegBackend::Auto));
        assert_eq!(o.reg_backend_normalize, Some(RegBackend::Wgpu));
        assert_eq!(o.norm_isotropic_mm, NormGrid::Full);
        assert_eq!(o.ants_max_threads, Some(1));
        assert!(
            !o.stop_on_error,
            "stop_on_error follows checkreg when unset"
        );
    }

    #[test]
    fn the_post_op_modality_comes_from_json_flag_or_variable() {
        let mut o = RunOptions::new("/data", "01");
        assert_eq!(o.postop_modality, PostopModality::Auto);
        o.apply_env(
            &nothing_explicit,
            &env_of(&[("DBSLOC_POSTOP_MODALITY", " MRI ")]),
        )
        .unwrap();
        assert_eq!(o.postop_modality, PostopModality::Mri);
        let e = o
            .apply_env(
                &nothing_explicit,
                &env_of(&[("DBSLOC_POSTOP_MODALITY", "pet")]),
            )
            .unwrap_err();
        assert!(e.contains("DBSLOC_POSTOP_MODALITY"), "{e}");
        let o = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01", "postop_modality": "ct"}"#,
            &env_of(&[("DBSLOC_POSTOP_MODALITY", "mri")]),
        )
        .unwrap();
        assert_eq!(o.postop_modality, PostopModality::Ct);
        let o =
            RunOptions::from_runner_json(r#"{"bids_dir": "/data", "subject": "01"}"#, &env_of(&[]))
                .unwrap();
        assert_eq!(o.postop_modality, PostopModality::Auto);
    }

    #[test]
    fn explicit_fields_win_and_their_variables_are_not_parsed() {
        let mut o = RunOptions::new("/data", "01");
        o.reg_backend = RegBackend::Wgpu;
        o.stop_on_error = false;
        let env = env_of(&[
            ("DBSLOC_REG_BACKEND", "cuda"),
            ("DBSLOC_NORM_ISOTROPIC_MM", "auto"),
            ("DBSLOC_STOP_ON_ERROR", "1"),
            ("DBSLOC_OPEN_CHECKREG", "0"),
        ]);
        let explicit = |f: &str| {
            matches!(
                f,
                "reg_backend" | "norm_isotropic_mm" | "stop_on_error" | "checkreg"
            )
        };
        o.apply_env(&explicit, &env).unwrap();
        assert_eq!(o.reg_backend, RegBackend::Wgpu);
        assert_eq!(o.norm_isotropic_mm, NormGrid::Template);
        assert!(!o.stop_on_error);
        assert!(o.checkreg);
    }

    #[test]
    fn stop_on_error_comes_from_the_variable_then_from_checkreg() {
        let run = |vars: &[(&str, &str)], checkreg: bool| {
            let mut o = RunOptions::new("/data", "01");
            o.checkreg = checkreg;
            o.apply_env(&nothing_explicit, &env_of(vars)).unwrap();
            o.stop_on_error
        };
        assert!(run(&[], true));
        assert!(!run(&[], false));
        assert!(run(&[("DBSLOC_STOP_ON_ERROR", "1")], false));
        assert!(!run(&[("DBSLOC_STOP_ON_ERROR", "no")], true));
        assert!(run(&[("DBSLOC_STOP_ON_ERROR", "maybe")], true));
    }

    #[test]
    fn invalid_values_are_errors_naming_the_variable_and_blank_is_unset() {
        for (name, bad) in [
            ("DBSLOC_REG_BACKEND", "cuda"),
            ("DBSLOC_NORMALIZE_BACKEND", "cuda"),
            ("DBSLOC_NORM_ISOTROPIC_MM", "auto"),
            ("DBSLOC_NORM_METHOD", "spm"),
            ("DBSLOC_ANTS_MAX_THREADS", "two"),
        ] {
            let mut o = RunOptions::new("/data", "01");
            let e = o
                .apply_env(&nothing_explicit, &env_of(&[(name, bad)]))
                .unwrap_err();
            assert!(e.starts_with(name), "{e}");
            let mut o = RunOptions::new("/data", "01");
            o.apply_env(&nothing_explicit, &env_of(&[(name, "  ")]))
                .unwrap();
            assert_eq!(o.reg_backend, RegBackend::Auto);
            assert_eq!(o.norm_isotropic_mm, NormGrid::Template);
            assert_eq!(o.ants_max_threads, None);
        }
        let mut o = RunOptions::new("/data", "01");
        let e = o
            .apply_env(
                &nothing_explicit,
                &env_of(&[("DBSLOC_REG_BACKEND", "cuda")]),
            )
            .unwrap_err();
        assert!(e.contains("wgpu"), "{e}");
        let mut o = RunOptions::new("/data", "01");
        o.apply_env(&nothing_explicit, &env_of(&[("DBSLOC_HEADLESS", "maybe")]))
            .unwrap();
        assert!(!o.headless);
    }

    #[test]
    fn output_dir_comes_from_the_variable_unless_set_and_is_made_absolute() {
        let mut o = RunOptions::new("/data", "01");
        o.apply_env(&nothing_explicit, &env_of(&[])).unwrap();
        assert_eq!(o.output_dir, None);
        assert!(o.derivatives_root().ends_with("derivatives/leaddbs"));
        let mut o = RunOptions::new("/data", "01");
        o.apply_env(
            &nothing_explicit,
            &env_of(&[("DBSLOC_OUTPUT_DIR", " rel/out ")]),
        )
        .unwrap();
        let out = o.output_dir.clone().unwrap();
        assert!(out.is_absolute() && out.ends_with("rel/out"), "{out:?}");
        assert_eq!(o.derivatives_root(), out);
        let mut o = RunOptions::new("/data", "01");
        o.output_dir = Some(PathBuf::from("/flag"));
        o.apply_env(
            &|f| f == "output_dir",
            &env_of(&[("DBSLOC_OUTPUT_DIR", "/env")]),
        )
        .unwrap();
        assert_eq!(o.output_dir, Some(PathBuf::from("/flag")));
        let o = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01", "output_dir": null}"#,
            &env_of(&[("DBSLOC_OUTPUT_DIR", "/env")]),
        )
        .unwrap();
        assert_eq!(o.output_dir, None);
        let text = serde_json::to_string(&RunOptions::new("/data", "01")).unwrap();
        assert!(text.contains("\"output_dir\":null"), "{text}");
    }

    #[test]
    fn the_atlases_directory_comes_from_json_or_variable() {
        let mut o = RunOptions::new("/data", "01");
        assert_eq!(o.leaddbs_atlases_dir, None);
        o.apply_env(
            &nothing_explicit,
            &env_of(&[("DBSLOC_LEADDBS_ATLASES_DIR", "  ")]),
        )
        .unwrap();
        assert_eq!(o.leaddbs_atlases_dir, None);
        o.apply_env(
            &nothing_explicit,
            &env_of(&[("DBSLOC_LEADDBS_ATLASES_DIR", "env_atlases")]),
        )
        .unwrap();
        let dir = o.leaddbs_atlases_dir.clone().unwrap_or_default();
        assert!(dir.is_absolute() && dir.ends_with("env_atlases"), "{dir:?}");
        let o = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01", "leaddbs_atlases_dir": "/json/atlases"}"#,
            &env_of(&[("DBSLOC_LEADDBS_ATLASES_DIR", "/env/atlases")]),
        )
        .unwrap();
        assert_eq!(o.leaddbs_atlases_dir, Some(PathBuf::from("/json/atlases")));
        let o = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01", "leaddbs_atlases_dir": null}"#,
            &env_of(&[("DBSLOC_LEADDBS_ATLASES_DIR", "/env/atlases")]),
        )
        .unwrap();
        assert_eq!(o.leaddbs_atlases_dir, None);
        let o =
            RunOptions::from_runner_json(r#"{"bids_dir": "/data", "subject": "01"}"#, &env_of(&[]))
                .unwrap();
        assert_eq!(o.leaddbs_atlases_dir, None);
    }

    #[test]
    fn the_vat_options_follow_flag_then_environment_then_default() {
        let mut o = RunOptions::new("/data", "01");
        assert_eq!(o.vat_backend, VatBackend::Auto);
        assert!(o.vat_labels.is_empty());
        assert_eq!(o.vat_stats_atlas, None);
        o.apply_env(
            &nothing_explicit,
            &env_of(&[
                ("DBSLOC_VAT_BACKEND", " CPU "),
                ("DBSLOC_VAT_LABELS", "a, b,,c"),
                ("DBSLOC_VAT_STATS_ATLAS", "DISTAL Minimal (Ewert 2017)"),
            ]),
        )
        .unwrap();
        assert_eq!(o.vat_backend, VatBackend::Cpu);
        assert_eq!(o.vat_labels, ["a", "b", "c"]);
        assert_eq!(
            o.vat_stats_atlas.as_deref(),
            Some("DISTAL Minimal (Ewert 2017)")
        );
        let o = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01", "vat_backend": "wgpu", "vat_labels": ["x"]}"#,
            &env_of(&[
                ("DBSLOC_VAT_BACKEND", "cpu"),
                ("DBSLOC_VAT_LABELS", "y"),
                ("DBSLOC_VAT_STATS_ATLAS", "env_atlas"),
            ]),
        )
        .unwrap();
        assert_eq!(o.vat_backend, VatBackend::Wgpu);
        assert_eq!(o.vat_labels, ["x"]);
        assert_eq!(o.vat_stats_atlas.as_deref(), Some("env_atlas"));
        let e = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01"}"#,
            &env_of(&[("DBSLOC_VAT_BACKEND", "cuda")]),
        )
        .unwrap_err();
        assert!(e.starts_with("DBSLOC_VAT_BACKEND"), "{e}");
    }

    #[test]
    fn runner_json_keys_are_explicit_and_missing_keys_take_the_environment() {
        let env = env_of(&[
            ("DBSLOC_REG_BACKEND", "ants"),
            ("DBSLOC_NORMALIZE_BACKEND", "wgpu"),
            ("DBSLOC_STOP_ON_ERROR", "0"),
            ("DBSLOC_HEADLESS", "1"),
        ]);
        let o = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01", "reg_backend": "auto", "stop_on_error": true}"#,
            &env,
        )
        .unwrap();
        assert_eq!(o.reg_backend, RegBackend::Auto);
        assert!(o.stop_on_error);
        assert_eq!(o.reg_backend_normalize, Some(RegBackend::Wgpu));
        assert!(o.headless);
        let o = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01", "reg_backend_normalize": null}"#,
            &env,
        )
        .unwrap();
        assert_eq!(o.reg_backend, RegBackend::Ants);
        assert_eq!(o.reg_backend_normalize, None);
        assert!(!o.stop_on_error);
        let e = RunOptions::from_runner_json("{", &env).unwrap_err();
        assert!(e.starts_with("invalid RunOptions JSON"), "{e}");
        let e = RunOptions::from_runner_json(
            r#"{"bids_dir": "/data", "subject": "01"}"#,
            &env_of(&[("DBSLOC_REG_BACKEND", "cuda")]),
        )
        .unwrap_err();
        assert!(e.starts_with("DBSLOC_REG_BACKEND"), "{e}");
    }
}
