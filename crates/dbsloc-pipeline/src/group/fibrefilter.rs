// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Fibre filtering at group level (`specs/14` §8): per-fibre statistics, cross-validation,
//! prediction and the output files.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use dbsloc_connectome::FibreSet;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::flip::{flipped_is_current, flipped_name};
use super::record::subject_id;
use super::stats_common::{
    adjust_p, auc, chi2_two_proportions_yates, correlate, correlation_p, covariate_residuals,
    linear_regression, mean, median, quantile, spearman, t_test_one_sample, t_test_two_sample,
    weighted_group_t, wilcoxon_rank_sum, wilcoxon_signed_rank, CorrKind, Correction, Rng,
    TestResult,
};
use super::{GroupError, GroupRecord, Note};
use crate::connectome_cache::CacheSetting;
use crate::connectome_fetch::ConnectomeRoots;
use crate::connectome_maps::fibervals::{
    fibre_values, write_subset_mat, FeatureMatrix, FibreValues, BINARY_THRESHOLD,
};
use crate::connectome_maps::job::{
    find_side_files, load_fibres_with, resolve_connectome, stimulation_folder, LoadOptions, Side,
    SideFiles,
};
use crate::connectome_maps::nii::read_nii;
use crate::connectome_maps::structural::SeedKind;
use crate::connectome_maps::{report, MapError, ProgressFn};
use crate::outputs::partial_of;
use crate::sha256::file_sha256_hex;
use crate::vat::settings::Side as VatSide;
use crate::version::{utc_now_rfc3339, version_string};

const SIGMOID_X0: f64 = 206.0;
const SIGMOID_K: f64 = 0.0203;

/// Errors of the group analyses.
#[derive(Debug)]
pub enum AnalysisError {
    Group(GroupError),
    Map(MapError),
    Cancelled,
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisError::Group(e) => write!(f, "{e}"),
            AnalysisError::Map(e) => write!(f, "{e}"),
            AnalysisError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for AnalysisError {}

impl From<GroupError> for AnalysisError {
    fn from(e: GroupError) -> Self {
        AnalysisError::Group(e)
    }
}

impl From<MapError> for AnalysisError {
    fn from(e: MapError) -> Self {
        match e {
            MapError::Cancelled => AnalysisError::Cancelled,
            other => AnalysisError::Map(other),
        }
    }
}

impl AnalysisError {
    /// The error code, or `cancelled`.
    pub fn code(&self) -> String {
        match self {
            AnalysisError::Group(GroupError::Lg { code, .. }) => (*code).to_owned(),
            AnalysisError::Group(_) => "E_GROUP_USAGE".to_owned(),
            AnalysisError::Map(e) => e.code().to_owned(),
            AnalysisError::Cancelled => "cancelled".to_owned(),
        }
    }
}

/// Result alias of the group analyses.
pub type AnalysisResult<T> = Result<T, AnalysisError>;

pub(crate) fn lg(code: &'static str, message: impl Into<String>) -> AnalysisError {
    AnalysisError::Group(GroupError::Lg {
        code,
        message: message.into(),
    })
}

pub(crate) fn usage(message: impl Into<String>) -> AnalysisError {
    AnalysisError::Group(GroupError::Invalid(message.into()))
}

pub(crate) fn cancelled(cancel: &AtomicBool) -> AnalysisResult<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(AnalysisError::Cancelled)
    } else {
        Ok(())
    }
}

/// Value model of the per-fibre input (`specs/14` §8.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FibreMode {
    Vta,
    Efield,
    Sigmoid,
}

impl FibreMode {
    /// Parses `vta`, `efield` or `sigmoid`.
    pub fn parse(text: &str) -> Option<FibreMode> {
        match text.trim().to_ascii_lowercase().as_str() {
            "vta" | "binary" => Some(FibreMode::Vta),
            "efield" | "e-field" => Some(FibreMode::Efield),
            "sigmoid" => Some(FibreMode::Sigmoid),
            _ => None,
        }
    }
}

/// The per-fibre value used for E-field modes (`specs/15` §7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    Bin,
    Sum,
    Mean,
    Peak,
    Peak5,
}

impl Metric {
    /// Parses `bin`, `sum`, `mean`, `peak` or `peak5`.
    pub fn parse(text: &str) -> Option<Metric> {
        match text.trim().to_ascii_lowercase().as_str() {
            "bin" => Some(Metric::Bin),
            "sum" => Some(Metric::Sum),
            "mean" => Some(Metric::Mean),
            "peak" => Some(Metric::Peak),
            "peak5" => Some(Metric::Peak5),
            _ => None,
        }
    }
}

/// The per-fibre test (`specs/14` §8.3 table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FibreTest {
    Spearman,
    Pearson,
    Bend,
    TwoSampleT,
    OneSampleT,
    RankSum,
    SignedRank,
    ReverseT,
    Proportion,
    WeightedTwoSample,
    NMap,
    MeanMap,
}

impl FibreTest {
    /// Parses the kebab-case name.
    pub fn parse(text: &str) -> Option<FibreTest> {
        match text.trim().to_ascii_lowercase().as_str() {
            "spearman" => Some(FibreTest::Spearman),
            "pearson" => Some(FibreTest::Pearson),
            "bend" => Some(FibreTest::Bend),
            "two-sample-t" | "ttest2" => Some(FibreTest::TwoSampleT),
            "one-sample-t" | "ttest1" => Some(FibreTest::OneSampleT),
            "rank-sum" => Some(FibreTest::RankSum),
            "signed-rank" => Some(FibreTest::SignedRank),
            "reverse-t" => Some(FibreTest::ReverseT),
            "proportion" => Some(FibreTest::Proportion),
            "weighted-two-sample" | "wls" => Some(FibreTest::WeightedTwoSample),
            "n-map" => Some(FibreTest::NMap),
            "mean-map" => Some(FibreTest::MeanMap),
            _ => None,
        }
    }
}

/// The null hypothesis of the one-group tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum H0 {
    Zero,
    Average,
}

/// How many of the scored fibres are shown (`specs/14` §8.3 step 6).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "kebab-case")]
pub enum DisplayStrategy {
    RelativeToPeak { percent: f64 },
    PercentOfFibres { percent: f64 },
    FixedCount { count: usize },
    Quantile { q: f64 },
    FixedValue { value: f64 },
}

/// Display selection settings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplaySettings {
    pub strategy: DisplayStrategy,
    pub show_positive: bool,
    pub show_negative: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        DisplaySettings {
            strategy: DisplayStrategy::RelativeToPeak { percent: 25.0 },
            show_positive: true,
            show_negative: false,
        }
    }
}

/// How a test patient's fibres are combined into a predicted score.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PredBasis {
    Mean,
    Sum,
    Peak,
    Peak5,
}

impl PredBasis {
    /// Parses `mean`, `sum`, `peak` or `peak5`.
    pub fn parse(text: &str) -> Option<PredBasis> {
        match text.trim().to_ascii_lowercase().as_str() {
            "mean" => Some(PredBasis::Mean),
            "sum" => Some(PredBasis::Sum),
            "peak" => Some(PredBasis::Peak),
            "peak5" => Some(PredBasis::Peak5),
            _ => None,
        }
    }
}

/// Settings of a fibre filtering analysis (`specs/14` §8).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FibreSettings {
    pub mode: FibreMode,
    pub metric: Metric,
    pub test: FibreTest,
    pub h0: H0,
    pub efield_threshold: f64,
    pub coverage_percent: f64,
    pub significance: Option<Correction>,
    pub alpha: f64,
    pub display: DisplaySettings,
    pub basis: PredBasis,
}

impl Default for FibreSettings {
    fn default() -> Self {
        FibreSettings {
            mode: FibreMode::Efield,
            metric: Metric::Peak,
            test: FibreTest::Spearman,
            h0: H0::Average,
            efield_threshold: 200.0,
            coverage_percent: 20.0,
            significance: None,
            alpha: 0.05,
            display: DisplaySettings::default(),
            basis: PredBasis::Mean,
        }
    }
}

/// Cross-validation strategy (`specs/14` §7.5, §8.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all = "kebab-case")]
pub enum CvScheme {
    LeaveNothingOut { permutations: usize },
    LeaveOneOut,
    LeaveOneCohortOut,
    KFold { k: usize, iterations: usize },
}

impl CvScheme {
    /// The scheme name used on the command line and in records.
    pub fn name(&self) -> &'static str {
        match self {
            CvScheme::LeaveNothingOut { .. } => "leave-nothing-out",
            CvScheme::LeaveOneOut => "leave-one-out",
            CvScheme::LeaveOneCohortOut => "leave-one-cohort-out",
            CvScheme::KFold { .. } => "k-fold",
        }
    }
}

/// One patient-level column set of a side: the feature matrix and where each column comes from.
#[derive(Clone, Debug)]
pub struct SideData {
    pub matrix: FeatureMatrix,
    pub row_patient: Vec<usize>,
    pub flipped: Vec<bool>,
    pub outcome_side: Vec<usize>,
}

impl SideData {
    /// A side without flipped columns; every column uses the outcome of side `side`.
    pub fn plain(matrix: FeatureMatrix, row_patient: Vec<usize>, side: usize) -> SideData {
        let n = row_patient.len();
        SideData {
            matrix,
            row_patient,
            flipped: vec![false; n],
            outcome_side: vec![side; n],
        }
    }
}

/// The in-memory input of a fibre filtering analysis. Side 0 is right, side 1 left.
#[derive(Clone, Debug)]
pub struct FibreData {
    pub n_patients: usize,
    pub outcome: [Vec<f64>; 2],
    pub per_hemisphere: bool,
    pub covariates: Vec<Vec<f64>>,
    pub group_of: Vec<u32>,
    pub sides: [Option<SideData>; 2],
}

fn sigmoid(e: f64) -> f64 {
    1.0 / (1.0 + (-SIGMOID_K * (e - SIGMOID_X0)).exp())
}

fn raw_at(m: &FeatureMatrix, metric: Metric, row: usize, c: usize) -> f64 {
    let at = row * m.n_inputs + c;
    f64::from(match metric {
        Metric::Bin => m.bin[at],
        Metric::Sum => m.sum[at],
        Metric::Mean => m.mean[at],
        Metric::Peak => m.peak[at],
        Metric::Peak5 => m.peak5[at],
    })
}

fn cell_value(m: &FeatureMatrix, s: &FibreSettings, row: usize, c: usize) -> (bool, f64) {
    match s.mode {
        FibreMode::Vta => {
            let on = raw_at(m, Metric::Bin, row, c) > 0.5;
            (on, f64::from(u8::from(on)))
        }
        FibreMode::Efield | FibreMode::Sigmoid => {
            let raw = raw_at(m, s.metric, row, c);
            let on = raw > s.efield_threshold;
            let v = if s.mode == FibreMode::Sigmoid && on {
                sigmoid(raw)
            } else {
                raw
            };
            (on, v)
        }
    }
}

fn value_of_fibre_values(v: &FibreValues, s: &FibreSettings, i: usize) -> (bool, f64) {
    match s.mode {
        FibreMode::Vta => {
            let on = v.bin[i] > 0;
            (on, f64::from(u8::from(on)))
        }
        FibreMode::Efield | FibreMode::Sigmoid => {
            let raw = f64::from(match s.metric {
                Metric::Bin => f32::from(v.bin[i]),
                Metric::Sum => v.sum[i],
                Metric::Mean => v.mean[i],
                Metric::Peak => v.peak[i],
                Metric::Peak5 => v.peak5[i],
            });
            let on = raw > s.efield_threshold;
            let x = if s.mode == FibreMode::Sigmoid && on {
                sigmoid(raw)
            } else {
                raw
            };
            (on, x)
        }
    }
}

/// Scores of the fibres of one side, parallel arrays.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SideScores {
    pub fibre: Vec<u32>,
    pub coverage: Vec<u32>,
    pub value: Vec<f64>,
    pub p: Vec<f64>,
    pub n_rows: usize,
}

struct SideCols {
    cols: Vec<usize>,
    y: Vec<f64>,
}

fn outcome_column(data: &FibreData, side: &SideData, outcome: &[Vec<f64>; 2], c: usize) -> f64 {
    let s = if data.per_hemisphere {
        side.outcome_side[c].min(1)
    } else {
        0
    };
    outcome[s][side.row_patient[c]]
}

fn side_cols(
    data: &FibreData,
    side: &SideData,
    outcome: &[Vec<f64>; 2],
    train: &[bool],
) -> SideCols {
    let cols: Vec<usize> = (0..side.row_patient.len())
        .filter(|c| train[side.row_patient[*c]])
        .collect();
    let mut y: Vec<f64> = cols
        .iter()
        .map(|c| outcome_column(data, side, outcome, *c))
        .collect();
    if !data.covariates.is_empty() {
        let covs: Vec<Vec<f64>> = data
            .covariates
            .iter()
            .map(|cv| cols.iter().map(|c| cv[side.row_patient[*c]]).collect())
            .collect();
        y = covariate_residuals(&y, &covs);
    }
    let keep: Vec<usize> = (0..cols.len()).filter(|i| y[*i].is_finite()).collect();
    SideCols {
        cols: keep.iter().map(|i| cols[*i]).collect(),
        y: keep.iter().map(|i| y[*i]).collect(),
    }
}

fn neg_log10(p: f64, z: f64) -> f64 {
    if !p.is_finite() || !z.is_finite() {
        return f64::NAN;
    }
    z.signum() * -p.max(1e-300).log10()
}

fn pair(r: TestResult) -> (f64, f64) {
    (r.stat, r.p)
}

fn fibre_test(test: FibreTest, x: &[f64], conn: &[bool], y: &[f64], h0: f64) -> (f64, f64) {
    let pick = |on: bool| -> Vec<f64> {
        (0..y.len())
            .filter(|i| conn[*i] == on)
            .map(|i| y[i])
            .collect()
    };
    match test {
        FibreTest::Spearman => correlate(CorrKind::Spearman, x, y),
        FibreTest::Pearson => correlate(CorrKind::Pearson, x, y),
        FibreTest::Bend => correlate(CorrKind::Bend, x, y),
        FibreTest::TwoSampleT => pair(t_test_two_sample(&pick(true), &pick(false))),
        FibreTest::OneSampleT => pair(t_test_one_sample(&pick(true), h0)),
        FibreTest::RankSum => {
            let (a, b) = (pick(true), pick(false));
            let r = wilcoxon_rank_sum(&a, &b);
            let centre = a.len() as f64 * (a.len() + b.len() + 1) as f64 / 2.0;
            (neg_log10(r.p, r.stat - centre), r.p)
        }
        FibreTest::SignedRank => {
            let a = pick(true);
            let d: Vec<f64> = a.iter().map(|v| v - h0).collect();
            let p = wilcoxon_signed_rank(&d).p;
            (neg_log10(p, median(&a) - h0), p)
        }
        FibreTest::ReverseT => {
            let resp: Vec<f64> = (0..y.len()).filter(|i| y[*i] > 0.0).map(|i| x[i]).collect();
            let non: Vec<f64> = (0..y.len())
                .filter(|i| y[*i] <= 0.0)
                .map(|i| x[i])
                .collect();
            if non.is_empty() {
                pair(t_test_one_sample(&resp, 0.0))
            } else {
                pair(t_test_two_sample(&resp, &non))
            }
        }
        FibreTest::Proportion => {
            let n_resp = y.iter().filter(|v| **v > 0.0).count();
            let k1 = (0..y.len()).filter(|i| conn[*i] && y[*i] > 0.0).count();
            let k0 = (0..y.len()).filter(|i| conn[*i] && y[*i] <= 0.0).count();
            pair(chi2_two_proportions_yates(
                k1 as u64,
                n_resp as u64,
                k0 as u64,
                (y.len() - n_resp) as u64,
            ))
        }
        FibreTest::WeightedTwoSample => {
            let lo = x.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = x.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let scale = lo < 0.0 || hi > 1.0;
            let w: Vec<f64> = x
                .iter()
                .map(|v| {
                    if scale && hi > lo {
                        (v - lo) / (hi - lo)
                    } else {
                        v.clamp(0.0, 1.0)
                    }
                })
                .collect();
            let n = y.len();
            let yy: Vec<f64> = y.iter().chain(y).copied().collect();
            let g: Vec<f64> = (0..2 * n).map(|i| f64::from(u8::from(i < n))).collect();
            let ww: Vec<f64> = w.iter().copied().chain(w.iter().map(|v| 1.0 - v)).collect();
            weighted_group_t(&yy, &g, &ww, 2.0 * n as f64 - 2.0)
        }
        FibreTest::NMap => (conn.iter().filter(|b| **b).count() as f64, f64::NAN),
        FibreTest::MeanMap => (mean(&pick(true)), f64::NAN),
    }
}

fn side_scores(
    side: &SideData,
    sc: &SideCols,
    s: &FibreSettings,
    cancel: &AtomicBool,
) -> AnalysisResult<SideScores> {
    let n = sc.cols.len();
    let rows = side.matrix.connected.len();
    let frac = s.coverage_percent / 100.0;
    let h0 = match s.h0 {
        H0::Zero => 0.0,
        H0::Average => mean(&sc.y),
    };
    let results: Vec<Option<(u32, u32, f64, f64)>> = (0..rows)
        .into_par_iter()
        .map(|r| {
            if cancel.load(Ordering::Relaxed) || n < 3 {
                return None;
            }
            let mut x = Vec::with_capacity(n);
            let mut conn = Vec::with_capacity(n);
            for c in &sc.cols {
                let (on, v) = cell_value(&side.matrix, s, r, *c);
                conn.push(on);
                x.push(v);
            }
            let nf = conn.iter().filter(|b| **b).count();
            if (nf as f64) < frac * n as f64 {
                return None;
            }
            if s.mode == FibreMode::Vta && (nf as f64) > (1.0 - frac) * n as f64 {
                return None;
            }
            let (value, p) = fibre_test(s.test, &x, &conn, &sc.y, h0);
            if !value.is_finite() {
                return None;
            }
            Some((side.matrix.connected[r], nf as u32, value, p))
        })
        .collect();
    cancelled(cancel)?;
    let mut out = SideScores {
        n_rows: n,
        ..SideScores::default()
    };
    for (f, nf, v, p) in results.into_iter().flatten() {
        out.fibre.push(f);
        out.coverage.push(nf);
        out.value.push(v);
        out.p.push(p);
    }
    Ok(out)
}

fn apply_significance(scores: &mut [Option<SideScores>; 2], correction: Correction, alpha: f64) {
    let all: Vec<f64> = scores
        .iter()
        .flatten()
        .flat_map(|s| s.p.iter().copied())
        .collect();
    let adj = adjust_p(&all, correction);
    let mut at = 0;
    for s in scores.iter_mut().flatten() {
        for i in 0..s.p.len() {
            if adj[at].is_finite() && adj[at] > alpha {
                s.value[i] = f64::NAN;
            }
            at += 1;
        }
    }
}

/// A fibre of the display set with its score.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shown {
    pub side: usize,
    pub fibre: u32,
    pub value: f64,
}

fn top_per_side(
    items: &[(usize, usize, f64)],
    keep: impl Fn(usize) -> usize,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for side in 0..2 {
        let mut v: Vec<&(usize, usize, f64)> = items.iter().filter(|i| i.0 == side).collect();
        v.sort_by(|a, b| b.2.abs().total_cmp(&a.2.abs()).then(a.1.cmp(&b.1)));
        let k = keep(v.len()).min(v.len());
        out.extend(v[..k].iter().map(|i| (i.0, i.1)));
    }
    out
}

/// Chooses the fibres to show (`specs/14` §8.3 step 6): positive and negative scores separately.
pub fn select_shown(scores: &[Option<SideScores>; 2], d: &DisplaySettings) -> Vec<Shown> {
    let mut out = Vec::new();
    for positive in [true, false] {
        if (positive && !d.show_positive) || (!positive && !d.show_negative) {
            continue;
        }
        let mut items: Vec<(usize, usize, f64)> = Vec::new();
        for (side, s) in scores.iter().enumerate() {
            let Some(s) = s else { continue };
            for (i, v) in s.value.iter().enumerate() {
                if v.is_finite() && ((positive && *v > 0.0) || (!positive && *v < 0.0)) {
                    items.push((side, i, *v));
                }
            }
        }
        if items.is_empty() {
            continue;
        }
        let mags: Vec<f64> = items.iter().map(|i| i.2.abs()).collect();
        let picked: Vec<(usize, usize)> = match d.strategy {
            DisplayStrategy::RelativeToPeak { percent } => {
                let hi = mags.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let lo = mags.iter().copied().fold(f64::INFINITY, f64::min);
                let cut = hi - percent / 100.0 * (hi - lo);
                items
                    .iter()
                    .filter(|i| i.2.abs() >= cut)
                    .map(|i| (i.0, i.1))
                    .collect()
            }
            DisplayStrategy::PercentOfFibres { percent } => {
                top_per_side(&items, |n| (percent / 100.0 * n as f64).ceil() as usize)
            }
            DisplayStrategy::FixedCount { count } => top_per_side(&items, |_| count),
            DisplayStrategy::Quantile { q } => {
                let cut = quantile(&mags, q);
                items
                    .iter()
                    .filter(|i| i.2.abs() >= cut)
                    .map(|i| (i.0, i.1))
                    .collect()
            }
            DisplayStrategy::FixedValue { value } => items
                .iter()
                .filter(|i| i.2.abs() >= value)
                .map(|i| (i.0, i.1))
                .collect(),
        };
        for (side, i) in picked {
            if let Some(s) = &scores[side] {
                out.push(Shown {
                    side,
                    fibre: s.fibre[i],
                    value: s.value[i],
                });
            }
        }
    }
    out.sort_by(|a, b| a.side.cmp(&b.side).then(a.fibre.cmp(&b.fibre)));
    out
}

struct Trained {
    scores: [Option<SideScores>; 2],
    shown: Vec<Shown>,
}

fn train_model(
    data: &FibreData,
    outcome: &[Vec<f64>; 2],
    train: &[bool],
    s: &FibreSettings,
    cancel: &AtomicBool,
) -> AnalysisResult<Trained> {
    let mut scores: [Option<SideScores>; 2] = [None, None];
    for (slot, side) in scores.iter_mut().zip(&data.sides) {
        if let Some(sd) = side {
            let sc = side_cols(data, sd, outcome, train);
            *slot = Some(side_scores(sd, &sc, s, cancel)?);
        }
    }
    if let Some(c) = s.significance {
        apply_significance(&mut scores, c, s.alpha);
    }
    let shown = select_shown(&scores, &s.display);
    Ok(Trained { scores, shown })
}

fn extreme(vals: &[f64], high: bool, k: usize) -> f64 {
    if vals.is_empty() {
        return f64::NAN;
    }
    let mut v = vals.to_vec();
    v.sort_by(|a, b| if high { b.total_cmp(a) } else { a.total_cmp(b) });
    let k = k.clamp(1, v.len());
    mean(&v[..k])
}

fn aggregate(products: &[f64], basis: PredBasis) -> f64 {
    if products.is_empty() {
        return f64::NAN;
    }
    match basis {
        PredBasis::Mean => mean(products),
        PredBasis::Sum => products.iter().sum(),
        PredBasis::Peak | PredBasis::Peak5 => {
            let k = if basis == PredBasis::Peak {
                1
            } else {
                (0.05 * products.len() as f64).ceil() as usize
            };
            let pos: Vec<f64> = products.iter().copied().filter(|v| *v > 0.0).collect();
            let neg: Vec<f64> = products.iter().copied().filter(|v| *v < 0.0).collect();
            let parts: Vec<f64> = [
                (!pos.is_empty()).then(|| extreme(&pos, true, k)),
                (!neg.is_empty()).then(|| extreme(&neg, false, k)),
            ]
            .into_iter()
            .flatten()
            .collect();
            if parts.is_empty() {
                f64::NAN
            } else {
                parts.iter().sum()
            }
        }
    }
}

fn combine_sides(per_side: [Vec<f64>; 2], per_hemisphere: bool, basis: PredBasis) -> [f64; 2] {
    if per_hemisphere {
        [
            aggregate(&per_side[0], basis),
            aggregate(&per_side[1], basis),
        ]
    } else {
        let pooled: Vec<f64> = per_side[0].iter().chain(&per_side[1]).copied().collect();
        [aggregate(&pooled, basis), f64::NAN]
    }
}

fn predict_in_data(
    data: &FibreData,
    shown: &[Shown],
    s: &FibreSettings,
    patient: usize,
) -> [f64; 2] {
    let mut per_side: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    for (side, slot) in per_side.iter_mut().enumerate() {
        let Some(sd) = &data.sides[side] else {
            continue;
        };
        let Some(c) =
            (0..sd.row_patient.len()).find(|c| sd.row_patient[*c] == patient && !sd.flipped[*c])
        else {
            continue;
        };
        for sh in shown.iter().filter(|x| x.side == side) {
            if let Ok(r) = sd.matrix.connected.binary_search(&sh.fibre) {
                let (on, v) = cell_value(&sd.matrix, s, r, c);
                if on && v != 0.0 {
                    slot.push(sh.value * v);
                }
            }
        }
    }
    combine_sides(per_side, data.per_hemisphere, s.basis)
}

/// Correlation of predictions with outcomes over finite pairs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evaluation {
    pub n: usize,
    pub r: f64,
    pub p: f64,
    pub auc: Option<f64>,
}

/// Spearman correlation of `ihat` with `y` over finite pairs, with p; AUC for a binary outcome.
pub fn evaluate(ihat: &[f64], y: &[f64]) -> Evaluation {
    let pairs: Vec<(f64, f64)> = ihat
        .iter()
        .zip(y)
        .filter(|(a, b)| a.is_finite() && b.is_finite())
        .map(|(a, b)| (*a, *b))
        .collect();
    let a: Vec<f64> = pairs.iter().map(|p| p.0).collect();
    let b: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    let r = spearman(&a, &b);
    let binary = !b.is_empty() && b.iter().all(|v| *v == 0.0 || *v == 1.0);
    Evaluation {
        n: pairs.len(),
        r,
        p: correlation_p(r, pairs.len()),
        auc: binary
            .then(|| {
                let raw = auc(&a, &b);
                if r < 0.0 {
                    1.0 - raw
                } else {
                    raw
                }
            })
            .filter(|v| v.is_finite()),
    }
}

/// The (prediction, outcome) pairs of a result vector.
pub fn prediction_pairs(
    ihat: &[[f64; 2]],
    outcome: &[Vec<f64>; 2],
    per_hemisphere: bool,
    valid: &[usize],
) -> (Vec<f64>, Vec<f64>) {
    let mut a = Vec::new();
    let mut b = Vec::new();
    for j in valid {
        a.push(ihat[*j][0]);
        b.push(outcome[0][*j]);
        if per_hemisphere {
            a.push(ihat[*j][1]);
            b.push(outcome[1][*j]);
        }
    }
    (a, b)
}

/// Runs a cross-validation scheme: `predict(train mask, test patients)` returns one prediction
/// pair per test patient. Returns the predictions of every patient (NaN when never tested); a
/// repeated k-fold averages its iterations.
pub fn run_cv<F>(
    valid: &[usize],
    group_of: &[u32],
    scheme: &CvScheme,
    seed: u64,
    n_patients: usize,
    cancel: &AtomicBool,
    predict: F,
) -> AnalysisResult<Vec<[f64; 2]>>
where
    F: Fn(&[bool], &[usize]) -> AnalysisResult<Vec<[f64; 2]>> + Sync,
{
    let mut sum = vec![[0.0f64; 2]; n_patients];
    let mut cnt = vec![[0u32; 2]; n_patients];
    let mut add = |test: &[usize], pred: &[[f64; 2]]| {
        for (j, p) in test.iter().zip(pred) {
            for s in 0..2 {
                if p[s].is_finite() {
                    sum[*j][s] += p[s];
                    cnt[*j][s] += 1;
                }
            }
        }
    };
    let mask_of = |test: &[usize]| -> Vec<bool> {
        let mut m = vec![false; n_patients];
        for j in valid {
            m[*j] = true;
        }
        for j in test {
            m[*j] = false;
        }
        m
    };
    match scheme {
        CvScheme::LeaveNothingOut { .. } => {
            let mut m = vec![false; n_patients];
            for j in valid {
                m[*j] = true;
            }
            let pred = predict(&m, valid)?;
            add(valid, &pred);
        }
        CvScheme::LeaveOneOut => {
            for j in valid {
                cancelled(cancel)?;
                let test = [*j];
                let pred = predict(&mask_of(&test), &test)?;
                add(&test, &pred);
            }
        }
        CvScheme::LeaveOneCohortOut => {
            let mut groups: Vec<u32> = valid.iter().map(|j| group_of[*j]).collect();
            groups.sort_unstable();
            groups.dedup();
            if groups.len() < 2 {
                return Err(usage(
                    "leave-one-cohort-out needs patients from at least two groups",
                ));
            }
            for g in groups {
                cancelled(cancel)?;
                let test: Vec<usize> = valid
                    .iter()
                    .copied()
                    .filter(|j| group_of[*j] == g)
                    .collect();
                let pred = predict(&mask_of(&test), &test)?;
                add(&test, &pred);
            }
        }
        CvScheme::KFold { k, iterations } => {
            let k = (*k).clamp(2, valid.len().max(2));
            for it in 0..(*iterations).max(1) {
                let mut order = valid.to_vec();
                Rng::new(seed ^ (it as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15))
                    .shuffle(&mut order);
                for fold in 0..k {
                    cancelled(cancel)?;
                    let test: Vec<usize> = order
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| i % k == fold)
                        .map(|(_, j)| *j)
                        .collect();
                    if test.is_empty() {
                        continue;
                    }
                    let pred = predict(&mask_of(&test), &test)?;
                    add(&test, &pred);
                }
            }
        }
    }
    Ok((0..n_patients)
        .map(|j| {
            let mut o = [f64::NAN; 2];
            for s in 0..2 {
                if cnt[j][s] > 0 {
                    o[s] = sum[j][s] / f64::from(cnt[j][s]);
                }
            }
            o
        })
        .collect())
}

/// The permutation null of `specs/14` §7.5: `run(permuted outcome)` returns the correlation of
/// a Leave-Nothing-Out analysis; NaN counts as 1e-5. Returns `(p_perm, 95th percentile of |r|)`.
#[allow(clippy::too_many_arguments)]
pub fn permutation_null<F>(
    valid: &[usize],
    outcome: &[Vec<f64>; 2],
    r1: f64,
    permutations: usize,
    seed: u64,
    cancel: &AtomicBool,
    run: F,
) -> AnalysisResult<(f64, f64)>
where
    F: Fn(&[Vec<f64>; 2]) -> AnalysisResult<f64> + Sync,
{
    let fix = |r: f64| if r.is_nan() { 1e-5 } else { r };
    let rs: Vec<AnalysisResult<f64>> = (0..permutations)
        .into_par_iter()
        .map(|k| {
            cancelled(cancel)?;
            let mut order = valid.to_vec();
            Rng::new(seed ^ (k as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03)).shuffle(&mut order);
            let mut perm = outcome.clone();
            for (i, j) in valid.iter().enumerate() {
                perm[0][*j] = outcome[0][order[i]];
                perm[1][*j] = outcome[1][order[i]];
            }
            run(&perm).map(fix)
        })
        .collect();
    let mut null = Vec::with_capacity(rs.len());
    for r in rs {
        null.push(r?.abs());
    }
    let r1 = fix(r1).abs();
    let hits = null.iter().filter(|v| **v >= r1).count();
    let p = if null.is_empty() {
        f64::NAN
    } else {
        hits as f64 / null.len() as f64
    };
    Ok((p, quantile(&null, 0.95)))
}

/// A stored model for new-patient prediction (`specs/14` §8.5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FibreModel {
    pub connectome_id: String,
    pub total_fibers: usize,
    pub settings: FibreSettings,
    pub per_hemisphere: bool,
    pub shown: Vec<Shown>,
    pub beta0: Option<f64>,
    pub beta1: Option<f64>,
    pub n: usize,
    pub r_train: f64,
}

/// A new patient's prediction.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewPrediction {
    pub ihat: [f64; 2],
    pub predicted: [f64; 2],
}

/// Applies a stored model to a new patient's per-fibre values (`specs/14` §8.5); the
/// connectome id and fibre count must match the model.
pub fn predict_new(
    model: &FibreModel,
    connectome_id: &str,
    values: &[Option<FibreValues>; 2],
) -> AnalysisResult<NewPrediction> {
    if model.connectome_id != connectome_id {
        return Err(AnalysisError::Map(MapError::Usage(format!(
            "the model was built with connectome {}, not {connectome_id}",
            model.connectome_id
        ))));
    }
    let mut per_side: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    for side in 0..2 {
        let Some(v) = &values[side] else { continue };
        if v.total_fibers != model.total_fibers {
            return Err(AnalysisError::Map(MapError::Usage(
                "the fibre count differs from the model's connectome".into(),
            )));
        }
        for sh in model.shown.iter().filter(|x| x.side == side) {
            if let Ok(i) = v.fiber_index.binary_search(&sh.fibre) {
                let (on, x) = value_of_fibre_values(v, &model.settings, i);
                if on && x != 0.0 {
                    per_side[side].push(sh.value * x);
                }
            }
        }
    }
    let ihat = combine_sides(per_side, model.per_hemisphere, model.settings.basis);
    let apply = |x: f64| match (model.beta0, model.beta1) {
        (Some(b0), Some(b1)) => b0 + b1 * x,
        _ => f64::NAN,
    };
    Ok(NewPrediction {
        ihat,
        predicted: [apply(ihat[0]), apply(ihat[1])],
    })
}

/// Everything a fibre filtering analysis computes.
#[derive(Clone, Debug)]
pub struct FibreResult {
    pub scores: [Option<SideScores>; 2],
    pub shown: Vec<Shown>,
    pub ihat: Vec<[f64; 2]>,
    pub valid: Vec<usize>,
    pub evaluation: Evaluation,
    pub p_perm: Option<f64>,
    pub null95: Option<f64>,
    pub model: FibreModel,
    pub scheme: CvScheme,
    pub notes: Vec<Note>,
}

fn valid_patients(data: &FibreData) -> Vec<usize> {
    (0..data.n_patients)
        .filter(|j| {
            let has = data.outcome[0][*j].is_finite()
                || (data.per_hemisphere && data.outcome[1][*j].is_finite());
            has && data.covariates.iter().all(|c| c[*j].is_finite())
        })
        .collect()
}

/// Computes the per-fibre statistics, the cross-validation and the model (`specs/14` §8.3 to
/// §8.5). `progress` reports the stages.
pub fn compute_fibrefilter(
    data: &FibreData,
    settings: &FibreSettings,
    scheme: &CvScheme,
    connectome_id: &str,
    seed: u64,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> AnalysisResult<FibreResult> {
    let valid = valid_patients(data);
    if valid.len() < 3 {
        return Err(lg(
            "E_GROUP_TOO_FEW",
            format!(
                "{} patients with an outcome; at least 3 are needed",
                valid.len()
            ),
        ));
    }
    if data.sides.iter().all(Option::is_none) {
        return Err(lg("E_GROUP_TOO_FEW", "no stimulation image of any patient"));
    }
    let mut notes = Vec::new();
    let finite: Vec<f64> = valid.iter().map(|j| data.outcome[0][*j]).collect();
    if finite.windows(2).all(|w| w[0] == w[1]) {
        notes.push(Note::plain("outcome has no variance"));
    }
    let total = data
        .sides
        .iter()
        .flatten()
        .map(|s| s.matrix.total_fibers)
        .max()
        .unwrap_or(0);
    let mut all = vec![false; data.n_patients];
    for j in &valid {
        all[*j] = true;
    }
    report(progress, "fibrefilter", 0, 3, "per-fibre statistics");
    let full = train_model(data, &data.outcome, &all, settings, cancel)?;
    report(progress, "fibrefilter", 1, 3, "cross-validation");
    let predict_with = |outcome: &[Vec<f64>; 2], train: &[bool], test: &[usize]| {
        let m = train_model(data, outcome, train, settings, cancel)?;
        Ok(test
            .iter()
            .map(|j| predict_in_data(data, &m.shown, settings, *j))
            .collect::<Vec<_>>())
    };
    let ihat = run_cv(
        &valid,
        &data.group_of,
        scheme,
        seed,
        data.n_patients,
        cancel,
        |train, test| predict_with(&data.outcome, train, test),
    )?;
    let (a, b) = prediction_pairs(&ihat, &data.outcome, data.per_hemisphere, &valid);
    let evaluation = evaluate(&a, &b);
    let (mut p_perm, mut null95) = (None, None);
    if let CvScheme::LeaveNothingOut { permutations } = scheme {
        report(progress, "fibrefilter", 2, 3, "permutations");
        let (p, q) = permutation_null(
            &valid,
            &data.outcome,
            evaluation.r,
            *permutations,
            seed,
            cancel,
            |perm| {
                let pred = predict_with(perm, &all, &valid)?;
                let mut full_pred = vec![[f64::NAN; 2]; data.n_patients];
                for (j, p) in valid.iter().zip(pred) {
                    full_pred[*j] = p;
                }
                let (a, b) = prediction_pairs(&full_pred, perm, data.per_hemisphere, &valid);
                Ok(evaluate(&a, &b).r)
            },
        )?;
        p_perm = Some(p);
        null95 = Some(q);
    }
    let lno = if matches!(scheme, CvScheme::LeaveNothingOut { .. }) {
        (a, b)
    } else {
        let pred = predict_with(&data.outcome, &all, &valid)?;
        let mut full_pred = vec![[f64::NAN; 2]; data.n_patients];
        for (j, p) in valid.iter().zip(pred) {
            full_pred[*j] = p;
        }
        prediction_pairs(&full_pred, &data.outcome, data.per_hemisphere, &valid)
    };
    let beta = linear_regression(&lno.0, &lno.1);
    let r_train = evaluate(&lno.0, &lno.1).r;
    let model = FibreModel {
        connectome_id: connectome_id.to_owned(),
        total_fibers: total,
        settings: settings.clone(),
        per_hemisphere: data.per_hemisphere,
        shown: full.shown.clone(),
        beta0: beta.map(|b| b.0),
        beta1: beta.map(|b| b.1),
        n: valid.len(),
        r_train,
    };
    Ok(FibreResult {
        scores: full.scores,
        shown: full.shown,
        ihat,
        valid,
        evaluation,
        p_perm,
        null95,
        model,
        scheme: *scheme,
        notes,
    })
}

/// The files of a committed analysis.
#[derive(Clone, Debug)]
pub struct AnalysisOutputs {
    pub id: String,
    pub dir: PathBuf,
    pub files: Vec<PathBuf>,
}

/// Creates `<parent>/<guid>`, `<guid>_1`, ... with the first free name (`specs/14` §7.7).
pub fn create_analysis_dir(parent: &Path, guid: &str) -> AnalysisResult<(String, PathBuf)> {
    std::fs::create_dir_all(parent)
        .map_err(|e| usage(format!("cannot create {}: {e}", parent.display())))?;
    for n in 0..10_000 {
        let id = if n == 0 {
            guid.to_owned()
        } else {
            format!("{guid}_{n}")
        };
        let dir = parent.join(&id);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok((id, dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(usage(format!("cannot create {}: {e}", dir.display()))),
        }
    }
    Err(usage("no free analysis id"))
}

/// Writes the staged `(partial, target)` pairs atomically: renames all unless cancelled.
pub(crate) fn commit_staged(
    dir: &Path,
    staged: &[(PathBuf, PathBuf)],
    cancel: &AtomicBool,
) -> AnalysisResult<Vec<PathBuf>> {
    if cancel.load(Ordering::Relaxed) {
        for (tmp, _) in staged {
            let _ = std::fs::remove_file(tmp);
        }
        let _ = std::fs::remove_dir(dir);
        return Err(AnalysisError::Cancelled);
    }
    let mut files = Vec::new();
    for (tmp, target) in staged {
        let _ = std::fs::remove_file(target);
        std::fs::rename(tmp, target)
            .map_err(|e| usage(format!("cannot write {}: {e}", target.display())))?;
        files.push(target.clone());
    }
    Ok(files)
}

fn side_letter(side: usize) -> &'static str {
    if side == 0 {
        "R"
    } else {
        "L"
    }
}

fn scores_tsv(s: &SideScores) -> String {
    let mut out = String::from("source_fiber_index\tcoverage\tvalue\tp\n");
    for i in 0..s.fibre.len() {
        let p = if s.p[i].is_finite() {
            format!("{}", s.p[i])
        } else {
            String::new()
        };
        let v = if s.value[i].is_finite() {
            format!("{}", s.value[i])
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            s.fibre[i] + 1,
            s.coverage[i],
            v,
            p
        ));
    }
    out
}

/// The `fibrefilter.json` record.
pub fn fibrefilter_json(result: &FibreResult, provenance: &Value) -> Value {
    let scores = |side: usize| -> Value {
        Value::Array(
            result
                .shown
                .iter()
                .filter(|s| s.side == side)
                .map(|s| json!({"source_fiber_index": s.fibre + 1, "value": s.value}))
                .collect(),
        )
    };
    let patients: Vec<Value> = result
        .valid
        .iter()
        .map(|j| json!({"patient": j, "ihat_1": result.ihat[*j][0], "ihat_2": result.ihat[*j][1]}))
        .collect();
    json!({
        "schema": "dbsloc-fibrefilter/1",
        "software": version_string(),
        "created_at": utc_now_rfc3339(),
        "settings": result.model.settings,
        "cross_validation": {
            "scheme": result.scheme,
            "n": result.evaluation.n,
            "r": result.evaluation.r,
            "p": result.evaluation.p,
            "auc": result.evaluation.auc,
            "p_perm": result.p_perm,
            "null_r95": result.null95,
            "predictions": patients,
        },
        "model": result.model,
        "shown": {"R": scores(0), "L": scores(1)},
        "notes": result.notes.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "provenance": provenance,
    })
}

/// Writes `fibrefilter.json`, `fibres_<R|L>.mat` and the per-fibre score tables under
/// `<group folder>/fiberfiltering/<id>/` (`specs/14` §8.6). Nothing is committed when `cancel`
/// is set.
pub fn write_fibrefilter_outputs(
    group_folder: &Path,
    guid: &str,
    result: &FibreResult,
    set: &FibreSet,
    source_tag: &str,
    provenance: &Value,
    cancel: &AtomicBool,
) -> AnalysisResult<AnalysisOutputs> {
    let (id, dir) = create_analysis_dir(&group_folder.join("fiberfiltering"), guid)?;
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let stage = |name: String| -> (PathBuf, PathBuf) {
        let target = dir.join(name);
        (partial_of(&target), target)
    };
    let fail = |dir: &Path, staged: &[(PathBuf, PathBuf)], e: AnalysisError| {
        for (tmp, _) in staged {
            let _ = std::fs::remove_file(tmp);
        }
        let _ = std::fs::remove_dir(dir);
        e
    };
    for side in 0..2 {
        let ids: Vec<u32> = result
            .shown
            .iter()
            .filter(|s| s.side == side)
            .map(|s| s.fibre)
            .collect();
        if !ids.is_empty() {
            let vals: Vec<f32> = result
                .shown
                .iter()
                .filter(|s| s.side == side)
                .map(|s| s.value as f32)
                .collect();
            let pair = stage(format!("fibres_{}.mat", side_letter(side)));
            if let Err(e) = write_subset_mat(&pair.0, set, &ids, &vals, source_tag) {
                return Err(fail(&dir, &staged, e.into()));
            }
            staged.push(pair);
        }
        if let Some(s) = &result.scores[side] {
            let pair = stage(format!("fibrescores_{}.tsv", side_letter(side)));
            if let Err(e) = std::fs::write(&pair.0, scores_tsv(s)) {
                return Err(fail(&dir, &staged, usage(e.to_string())));
            }
            staged.push(pair);
        }
    }
    let pair = stage("fibrefilter.json".to_owned());
    let text = serde_json::to_string_pretty(&fibrefilter_json(result, provenance))
        .map(|t| t + "\n")
        .map_err(|e| usage(e.to_string()))?;
    if let Err(e) = std::fs::write(&pair.0, text) {
        return Err(fail(&dir, &staged, usage(e.to_string())));
    }
    staged.push(pair);
    let files = commit_staged(&dir, &staged, cancel)?;
    Ok(AnalysisOutputs { id, dir, files })
}

/// Options of [`run_fibrefilter`].
#[derive(Clone, Debug)]
pub struct FibreRunOptions {
    pub variable: String,
    pub covariates: Vec<String>,
    pub connectome: String,
    pub connectome_root: Option<PathBuf>,
    pub space: String,
    pub model: Option<String>,
    pub settings: FibreSettings,
    pub scheme: CvScheme,
    pub seed: u64,
    pub mirror: bool,
    pub group_folder: PathBuf,
    pub cache: CacheSetting,
}

/// What [`run_fibrefilter`] did.
#[derive(Clone, Debug)]
pub struct FibreRun {
    pub outputs: AnalysisOutputs,
    pub result: FibreResult,
    pub notes: Vec<Note>,
}

/// The outcome of the named clinical variable per included patient: both sides, and whether it
/// is per hemisphere.
pub(crate) fn clinical_outcome(
    record: &GroupRecord,
    name: &str,
    patients: &[usize],
) -> AnalysisResult<([Vec<f64>; 2], bool)> {
    let var = record
        .clinical
        .iter()
        .find(|v| v.name == name)
        .ok_or_else(|| usage(format!("the group has no clinical variable {name:?}")))?;
    let width = var.width();
    if width > 2 {
        return Err(usage(format!(
            "clinical variable {name:?} has {width} columns; an outcome has one or two"
        )));
    }
    let cell = |j: usize, c: usize| {
        var.values
            .get(j)
            .and_then(|r| r.get(c))
            .copied()
            .flatten()
            .unwrap_or(f64::NAN)
    };
    let right: Vec<f64> = patients.iter().map(|j| cell(*j, 0)).collect();
    let left: Vec<f64> = if width == 2 {
        patients.iter().map(|j| cell(*j, 1)).collect()
    } else {
        right.clone()
    };
    Ok(([right, left], width == 2))
}

/// The first column of a clinical variable per included patient.
pub(crate) fn clinical_covariate(
    record: &GroupRecord,
    name: &str,
    patients: &[usize],
) -> AnalysisResult<Vec<f64>> {
    let (cols, _) = clinical_outcome(record, name, patients)?;
    Ok(cols[0].clone())
}

pub(crate) fn included_patients(record: &GroupRecord) -> Vec<usize> {
    (0..record.patients.len())
        .filter(|i| record.patients[*i].included)
        .collect()
}

/// A patient's stimulation image, its hemisphere column and outcome side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatientImage {
    pub column: usize,
    pub outcome_side: usize,
    pub flipped: bool,
    pub path: PathBuf,
}

/// A patient's images in reading order (`specs/14` §8.1).
pub fn patient_images(
    dir: &Path,
    id: &str,
    kind: SeedKind,
    found: &SideFiles,
    mirror: bool,
    subject: &str,
    notes: &mut Vec<Note>,
) -> Vec<PatientImage> {
    let original = |column: usize| {
        found
            .files
            .iter()
            .find(|(side, _)| usize::from(*side == Side::Left) == column)
            .map(|(_, file)| file.as_path())
    };
    let mut out = Vec::new();
    for column in 0..2 {
        if let Some(file) = original(column) {
            out.push(PatientImage {
                column,
                outcome_side: column,
                flipped: false,
                path: file.to_path_buf(),
            });
        }
        let Some(source) = original(1 - column).filter(|_| mirror) else {
            continue;
        };
        let side = if column == 0 {
            VatSide::Right
        } else {
            VatSide::Left
        };
        let path = dir.join(flipped_name(id, kind.name(), &found.model, side));
        if path.is_file() && flipped_is_current(source, &path) {
            out.push(PatientImage {
                column,
                outcome_side: 1 - column,
                flipped: true,
                path,
            });
        } else {
            notes.push(Note::plain(format!(
                "{subject}: no flipped image {}, or it is out of date; mirroring skipped for it \
                 (run group Calculate with mirroring again)",
                path.display()
            )));
        }
    }
    out
}

/// Reads the patients' stimulation images, computes the analysis and writes the outputs
/// (`specs/14` §8). Patients without a stimulation image are left out and reported.
pub fn run_fibrefilter(
    record: &GroupRecord,
    opts: &FibreRunOptions,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> AnalysisResult<FibreRun> {
    let idx = included_patients(record);
    let (outcome, per_hemisphere) = clinical_outcome(record, &opts.variable, &idx)?;
    let covariates = opts
        .covariates
        .iter()
        .map(|c| clinical_covariate(record, c, &idx))
        .collect::<AnalysisResult<Vec<_>>>()?;
    let roots = ConnectomeRoots::resolve(opts.connectome_root.as_deref())
        .map_err(|e| AnalysisError::Map(MapError::Usage(e.to_string())))?;
    let conn = resolve_connectome(&roots, &opts.connectome)?;
    let (set, load_notes, load_warnings) = load_fibres_with(
        &conn,
        &LoadOptions {
            setting: opts.cache,
            cancel: Some(cancel),
            progress: Some(progress),
        },
    )?;
    let mut notes: Vec<Note> = conn
        .warnings
        .iter()
        .chain(&load_notes)
        .chain(&load_warnings)
        .map(|w| Note::plain(w.clone()))
        .collect();
    let label = record.stim_label();
    let kinds: &[SeedKind] = if opts.settings.mode == FibreMode::Vta {
        &[SeedKind::Binary, SeedKind::Efield]
    } else {
        &[SeedKind::Efield]
    };
    let mut outcome = outcome;
    let mut columns: [Vec<(usize, usize, bool, FibreValues)>; 2] = [Vec::new(), Vec::new()];
    let mut inputs: Vec<Value> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for (a, pi) in idx.iter().enumerate() {
        cancelled(cancel)?;
        report(
            progress,
            "fibrefilter",
            a as u64,
            idx.len() as u64,
            "reading images",
        );
        let p = &record.patients[*pi];
        let Some(root) = record.derivatives_root_of(p) else {
            missing.push(p.subject.clone());
            continue;
        };
        let id = subject_id(&p.subject);
        let dir = stimulation_folder(Path::new(root), id, &opts.space, &label);
        let found = kinds.iter().find_map(|k| {
            find_side_files(&dir, id, *k, opts.model.as_deref())
                .ok()
                .map(|f| (*k, f))
        });
        let Some((kind, found)) = found else {
            missing.push(p.subject.clone());
            continue;
        };
        let threshold = if kind == SeedKind::Binary {
            BINARY_THRESHOLD
        } else {
            opts.settings.efield_threshold
        };
        let images = patient_images(&dir, id, kind, &found, opts.mirror, &p.subject, &mut notes);
        for image in images {
            let e = read_nii(&image.path)?;
            let sha = file_sha256_hex(&image.path).map_err(|er| usage(er.to_string()))?;
            inputs.push(json!({"file": image.path.display().to_string(), "sha256": sha}));
            let values = fibre_values(&set, &e, threshold, cancel)?;
            columns[image.column].push((a, image.outcome_side, image.flipped, values));
        }
    }
    if opts.settings.mode != FibreMode::Vta && !missing.is_empty() {
        return Err(lg(
            "E_SWEETSPOT_NO_EFIELD",
            format!("no E-field image for: {}", missing.join(", ")),
        ));
    }
    for m in &missing {
        notes.push(Note::plain(format!("{m}: no stimulation image, left out")));
    }
    for (a, pi) in idx.iter().enumerate() {
        if missing.contains(&record.patients[*pi].subject) {
            outcome[0][a] = f64::NAN;
            outcome[1][a] = f64::NAN;
        }
    }
    let mut sides: [Option<SideData>; 2] = [None, None];
    for s in 0..2 {
        if columns[s].is_empty() {
            continue;
        }
        let vals: Vec<FibreValues> = columns[s].iter().map(|c| c.3.clone()).collect();
        let matrix = FeatureMatrix::from_columns(&vals)?;
        sides[s] = Some(SideData {
            matrix,
            row_patient: columns[s].iter().map(|c| c.0).collect(),
            flipped: columns[s].iter().map(|c| c.2).collect(),
            outcome_side: columns[s].iter().map(|c| c.1).collect(),
        });
    }
    let data = FibreData {
        n_patients: idx.len(),
        outcome,
        per_hemisphere,
        covariates,
        group_of: idx.iter().map(|i| record.patients[*i].group).collect(),
        sides,
    };
    let result = compute_fibrefilter(
        &data,
        &opts.settings,
        &opts.scheme,
        &conn.id,
        opts.seed,
        cancel,
        progress,
    )?;
    notes.extend(result.notes.iter().cloned());
    let provenance = json!({
        "group_guid": record.guid,
        "variable": opts.variable,
        "covariates": opts.covariates,
        "connectome": {"id": conn.id, "name": conn.name, "tag": conn.tag, "digest": conn.digest},
        "space": opts.space,
        "seed": opts.seed,
        "mirror": opts.mirror,
        "patients": idx.iter().map(|i| record.patients[*i].subject.clone()).collect::<Vec<_>>(),
        "inputs": inputs,
    });
    let outputs = write_fibrefilter_outputs(
        &opts.group_folder,
        &record.guid,
        &result,
        &set,
        &conn.tag,
        &provenance,
        cancel,
    )?;
    Ok(FibreRun {
        outputs,
        result,
        notes,
    })
}
