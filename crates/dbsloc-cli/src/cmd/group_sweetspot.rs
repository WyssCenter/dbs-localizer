// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use clap::Args;
use dbsloc_pipeline::group::record::save_record;
use dbsloc_pipeline::group::sweetspot::{
    run_sweetspot, AmplitudeCorrection, CorrelationType, CvStrategy, Level, Multiple,
    Normalisation, NullHypothesis, PredictionBasis, Preset, SweetspotOptions, SweetspotSettings,
    Test, DEFAULT_SEED,
};
use dbsloc_pipeline::group::{open_group, GroupError};
use serde_json::json;

use super::interrupt_flag;
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Args)]
pub struct SweetspotArgs {
    /// group record file or its folder
    pub group_file: PathBuf,
    /// clinical variable of the group used as the outcome (per patient, or per hemisphere with two columns)
    #[arg(long, value_name = "NAME")]
    pub variable: String,
    /// clinical variable regressed out of the outcome (repeatable)
    #[arg(long, value_name = "NAME")]
    pub covariate: Vec<String>,
    /// literature preset: Butson 2011, Cheung 2011, Eisenstein 2014, Reich 2019, Dembek 2019, Elias 2020 (explicit options override it)
    #[arg(long, value_name = "NAME")]
    pub preset: Option<String>,
    /// level: vtas (default) or e-fields
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<String>,
    /// test: t-test (default), mean-image, n-image, wilcoxon-test, proportion-test, binomial-test; at level e-fields: correlations, reverse-t-tests
    #[arg(long, value_name = "TEST")]
    pub test: Option<String>,
    /// null hypothesis: zero (default), average, threshold
    #[arg(long, value_name = "H0")]
    pub h0: Option<String>,
    /// improvement threshold of the threshold null hypothesis and of the n-image test
    #[arg(long, value_name = "VALUE")]
    pub improvement_threshold: Option<f64>,
    /// share of patients (percent) of the n-image test
    #[arg(long, value_name = "PERCENT")]
    pub n_threshold: Option<f64>,
    /// amplitude correction: none (default), amplitude, vta-size, amplitude-discrete, amplitude-regression
    #[arg(long, value_name = "MODE")]
    pub amplitude_correction: Option<String>,
    /// outcome normalisation: none (default), z-score, van-albada
    #[arg(long, value_name = "MODE")]
    pub normalisation: Option<String>,
    /// correlation of the correlations test: spearman (default) or pearson
    #[arg(long, value_name = "TYPE")]
    pub correlation: Option<String>,
    /// E-field threshold in V/m that counts a voxel as covered (default 200)
    #[arg(long, value_name = "V/M")]
    pub threshold: Option<f64>,
    /// E-field values below this become missing at level e-fields (default 0)
    #[arg(long, value_name = "V/M")]
    pub nan_threshold: Option<f64>,
    /// share of patients (percent) that must cover a voxel (default 20)
    #[arg(long, value_name = "PERCENT")]
    pub coverage: Option<f64>,
    /// significance level (default 0.05)
    #[arg(long, value_name = "ALPHA")]
    pub alpha: Option<f64>,
    /// multiple-comparison correction: uncorrected (default), fdr, bonferroni
    #[arg(long, value_name = "MODE")]
    pub multiple_comparisons: Option<String>,
    /// keep voxels that are not significant
    #[arg(long)]
    pub all_voxels: bool,
    /// show the negative (sour spot) part of the map as well
    #[arg(long)]
    pub show_negative: bool,
    /// hide the positive (sweet spot) part of the map
    #[arg(long)]
    pub hide_positive: bool,
    /// use the flipped copies of both hemispheres as additional patients
    #[arg(long)]
    pub mirror: bool,
    /// also write one map per group
    #[arg(long)]
    pub split_by_group: bool,
    /// cross-validation: leave-one-patient-out (default), leave-nothing-out, leave-one-cohort-out, k-fold
    #[arg(long, value_name = "STRATEGY")]
    pub cv: Option<String>,
    /// number of folds of k-fold (default 5)
    #[arg(long, value_name = "K")]
    pub k: Option<usize>,
    /// number of permutations of leave-nothing-out (default 1000)
    #[arg(long, value_name = "N")]
    pub permutations: Option<usize>,
    /// prediction basis: mean-of-scores (default), sum-of-scores, peak-of-scores, peak-5-of-scores, profile-spearman, profile-pearson
    #[arg(long, value_name = "BASIS")]
    pub basis: Option<String>,
    /// correlation of the evaluation: spearman (default) or pearson
    #[arg(long, value_name = "TYPE")]
    pub evaluation_correlation: Option<String>,
    /// random seed of the permutations and folds
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,
    /// voxel size of the common grid in mm (default 0.5)
    #[arg(long, value_name = "MM")]
    pub resolution: Option<f64>,
    /// template space folder of the stimulations (default MNI152NLin2009bAsym)
    #[arg(long, value_name = "SPACE", default_value = "MNI152NLin2009bAsym")]
    pub space: String,
    /// stimulation model slug when a patient has several (for example simbio)
    #[arg(long, value_name = "SLUG")]
    pub model: Option<String>,
    /// use only these patients (ids with or without sub-); default: every included patient
    #[arg(long, num_args = 1.., value_name = "ID")]
    pub subjects: Vec<String>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

fn fail(message: String) -> Fail {
    Fail::new(1, format!("dbsloc: error: {message}"))
}

fn pick<T>(
    name: &str,
    text: Option<&str>,
    parse: fn(&str) -> Option<T>,
    all: &[&str],
) -> Result<Option<T>, Fail> {
    match text {
        None => Ok(None),
        Some(t) => parse(t)
            .map(Some)
            .ok_or_else(|| fail(format!("--{name} {t:?} is not one of: {}", all.join(", ")))),
    }
}

macro_rules! choice {
    ($flag:literal, $text:expr, $ty:ty) => {{
        let names: Vec<&str> = <$ty>::ALL.iter().map(|v| v.name()).collect();
        pick($flag, $text.as_deref(), <$ty>::parse, &names)
    }};
}

fn settings_of(a: &SweetspotArgs) -> Result<SweetspotSettings, Fail> {
    let mut s = SweetspotSettings::default();
    if let Some(p) = choice!("preset", a.preset, Preset)? {
        s.apply_preset(p);
    }
    if let Some(v) = choice!("level", a.level, Level)? {
        s.level = v;
        if v == Level::EFields && a.test.is_none() && a.preset.is_none() {
            s.test = Test::Correlations;
        }
    }
    if let Some(v) = choice!("test", a.test, Test)? {
        s.test = v;
    }
    if let Some(v) = choice!("h0", a.h0, NullHypothesis)? {
        s.null_hypothesis = v;
    }
    if let Some(v) = a.improvement_threshold {
        s.improvement_threshold = v;
    }
    if let Some(v) = a.n_threshold {
        s.n_threshold_percent = v;
    }
    if let Some(v) = choice!(
        "amplitude-correction",
        a.amplitude_correction,
        AmplitudeCorrection
    )? {
        s.amplitude_correction = v;
    }
    if let Some(v) = choice!("normalisation", a.normalisation, Normalisation)? {
        s.normalisation = v;
    }
    if let Some(v) = choice!("correlation", a.correlation, CorrelationType)? {
        s.correlation = v;
    }
    if let Some(v) = a.threshold {
        s.efield_threshold = v;
    }
    if let Some(v) = a.nan_threshold {
        s.nan_threshold = v;
    }
    if let Some(v) = a.coverage {
        s.coverage_percent = v;
    }
    if let Some(v) = a.alpha {
        s.alpha = v;
    }
    if let Some(v) = choice!("multiple-comparisons", a.multiple_comparisons, Multiple)? {
        s.multiple_comparisons = v;
    }
    s.show_significant_only = !a.all_voxels;
    s.show_negative = a.show_negative;
    s.show_positive = !a.hide_positive;
    s.mirror = a.mirror;
    s.split_by_group = a.split_by_group;
    if let Some(v) = choice!("cv", a.cv, CvStrategy)? {
        s.cv = v;
    }
    if let Some(v) = a.k {
        s.k_folds = v;
    }
    if let Some(v) = a.permutations {
        s.permutations = v;
    }
    if let Some(v) = choice!("basis", a.basis, PredictionBasis)? {
        s.basis = v;
    }
    if let Some(v) = choice!(
        "evaluation-correlation",
        a.evaluation_correlation,
        CorrelationType
    )? {
        s.evaluation_correlation = v;
    }
    s.seed = a.seed.unwrap_or(DEFAULT_SEED);
    if let Some(v) = a.resolution {
        s.resolution = v;
    }
    s.validate().map_err(|e| fail(e.to_string()))?;
    Ok(s)
}

fn group_fail(e: &GroupError) -> Fail {
    let code = match e {
        GroupError::Lg {
            code: "cancelled", ..
        } => 130,
        _ => 1,
    };
    Fail::new(code, format!("dbsloc: error: {e}"))
}

pub fn sweetspot(a: &SweetspotArgs) -> CmdResult {
    let settings = settings_of(a)?;
    let mut opened = open_group(&a.group_file).map_err(|e| group_fail(&e))?;
    for n in &opened.notes {
        err_line(&format!("dbsloc: warning: {n}"));
    }
    let options = SweetspotOptions {
        group_folder: opened.folder.clone(),
        variable: a.variable.clone(),
        covariates: a.covariate.clone(),
        settings,
        space: a.space.clone(),
        model: a.model.clone(),
        patients: (!a.subjects.is_empty()).then(|| a.subjects.clone()),
    };
    let cancel = interrupt_flag();
    let as_json = a.json;
    let progress = move |text: &str| {
        if !as_json {
            err_line(&format!("dbsloc: {text}"));
        }
    };
    let report =
        run_sweetspot(&opened.record, &options, &cancel, &progress).map_err(|e| group_fail(&e))?;
    for n in &report.notes {
        err_line(&format!("dbsloc: warning: {n}"));
    }
    opened
        .record
        .register_analysis("sweetspot", "sweetspots", &report.id, "sweetspot.json");
    save_record(&opened.json_path, &mut opened.record).map_err(|e| group_fail(&e))?;
    if a.json {
        out_json(&json!({
            "id": report.id,
            "folder": report.folder,
            "record": report.record_file,
            "model": report.model_file,
            "patients": report.n_patients,
            "excluded": report.excluded.iter().map(|(s, r)| json!({"subject": s, "reason": r})).collect::<Vec<_>>(),
            "cross_validation": report.summary["cross_validation"],
            "model_fit": report.summary["model"],
            "files": report.files,
            "warnings": report.notes.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }));
    } else {
        out_line(&format!(
            "sweet-spot analysis {} on {} patients: {} r = {:.3} (p = {:.4}){}",
            report.id,
            report.n_patients,
            options.settings.cv.name(),
            report.cv.r,
            report.cv.p,
            report
                .cv
                .p_perm
                .map_or(String::new(), |p| format!(", permutation p = {p:.4}"))
        ));
        out_line(&format!("wrote {}", report.folder.display()));
    }
    Ok(0)
}
