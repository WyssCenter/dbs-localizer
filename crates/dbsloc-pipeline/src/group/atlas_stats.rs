// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Clinical correlations and two-group tests of the atlas overlap (`specs/14` §6.3): Pearson and
//! Spearman correlations with permutation p-values, the pooled two-sample t-test and the box-plot
//! numbers, over the cohort VTA statistics.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::calc::group_fingerprint;
use super::clinical::{ClinicalKind, ClinicalVar};
use super::exports::{inputs_of, write_table, Cell, Table};
use super::overlap::{
    has_vta, load_vta_stats, measure_value, SideVta, SubjectVta, CORRELATION_MEASURES,
    SIDE_COMBINATIONS,
};
use super::record::{AnalysisRef, GroupRecord};
use super::stats_common::{pearson, quantile, ranks_average, t_test_two_sample, Rng};
use super::{GroupError, Note};
use crate::outputs::write_replacing;
use crate::version::{software_stamp, utc_now_rfc3339};

/// Number of label permutations of the Monte-Carlo p-value (`specs/14` §6.3).
pub const DEFAULT_PERMUTATIONS: usize = 5000;

/// Up to this many points all permutations are enumerated.
pub const EXACT_MAX_N: usize = 7;

/// Seed of the permutations when none is given.
pub const DEFAULT_SEED: u64 = 1;

/// Kind of the analysis record written under `atlasstats/<id>/analysis.json`.
pub const ANALYSIS_KIND: &str = "atlasstats";

/// Columns of the correlation table.
pub const CORRELATION_COLUMNS: [&str; 9] = [
    "structure",
    "measure",
    "side",
    "variable",
    "n",
    "pearson_r",
    "pearson_p_perm",
    "spearman_r",
    "spearman_p_perm",
];

/// Columns of the two-group table.
pub const GROUP_TEST_COLUMNS: [&str; 13] = [
    "structure",
    "measure",
    "side",
    "variable",
    "group1_value",
    "group2_value",
    "n1",
    "n2",
    "mean1",
    "mean2",
    "t",
    "df",
    "p",
];

/// What to compute.
#[derive(Debug, Clone)]
pub struct ClinicalStatsOptions {
    pub measure: String,
    pub variable: String,
    pub sides: Vec<&'static str>,
    pub permutations: usize,
    pub seed: u64,
}

impl ClinicalStatsOptions {
    /// Options for `variable` with the default measure, all side combinations and 5 000 permutations.
    pub fn new(variable: &str) -> ClinicalStatsOptions {
        ClinicalStatsOptions {
            measure: CORRELATION_MEASURES[0].to_owned(),
            variable: variable.to_owned(),
            sides: SIDE_COMBINATIONS.to_vec(),
            permutations: DEFAULT_PERMUTATIONS,
            seed: DEFAULT_SEED,
        }
    }
}

/// The side combination named by `text` (`both`, `right`, `left`).
pub fn side_combination(text: &str) -> Option<&'static str> {
    SIDE_COMBINATIONS.iter().copied().find(|s| *s == text)
}

/// One correlation row; `None` = not computable (fewer than 3 points, no variance).
#[derive(Debug, Clone, PartialEq)]
pub struct CorrelationRow {
    pub structure: String,
    pub measure: String,
    pub side: &'static str,
    pub variable: String,
    pub n: usize,
    pub pearson_r: Option<f64>,
    pub pearson_p_perm: Option<f64>,
    pub spearman_r: Option<f64>,
    pub spearman_p_perm: Option<f64>,
}

/// Five-number summary with whiskers at 1.5 times the inter-quartile range and the outliers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoxStats {
    pub n: usize,
    pub min: Option<f64>,
    pub q1: Option<f64>,
    pub median: Option<f64>,
    pub q3: Option<f64>,
    pub max: Option<f64>,
    pub whisker_low: Option<f64>,
    pub whisker_high: Option<f64>,
    pub outliers: Vec<f64>,
}

impl BoxStats {
    fn to_json(&self) -> Value {
        json!({
            "n": self.n,
            "min": self.min,
            "q1": self.q1,
            "median": self.median,
            "q3": self.q3,
            "max": self.max,
            "whisker_low": self.whisker_low,
            "whisker_high": self.whisker_high,
            "outliers": self.outliers,
        })
    }
}

/// One two-group row: Student's t-test with pooled variance of a measure between the patients with
/// the lower (`group1`) and the higher (`group2`) variable value.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupTestRow {
    pub structure: String,
    pub measure: String,
    pub side: &'static str,
    pub variable: String,
    pub group_values: [f64; 2],
    pub n1: usize,
    pub n2: usize,
    pub mean1: Option<f64>,
    pub mean2: Option<f64>,
    pub t: Option<f64>,
    pub df: Option<f64>,
    pub p: Option<f64>,
    pub boxes: [BoxStats; 2],
}

/// The result of a clinical statistics run.
#[derive(Debug, Clone)]
pub struct ClinicalStats {
    pub variable: String,
    pub measure: String,
    pub kind: ClinicalKind,
    pub patients_with_vta: usize,
    pub permutations: usize,
    pub seed: u64,
    pub correlations: Vec<CorrelationRow>,
    pub group_tests: Vec<GroupTestRow>,
    pub notes: Vec<Note>,
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// Visits every permutation of `0..n` (Heap's algorithm), the identity first.
pub fn for_each_permutation(n: usize, mut f: impl FnMut(&[usize])) {
    let mut a: Vec<usize> = (0..n).collect();
    f(&a);
    let mut c = vec![0usize; n];
    let mut i = 1;
    while i < n {
        if c[i] < i {
            if i % 2 == 0 {
                a.swap(0, i);
            } else {
                a.swap(c[i], i);
            }
            f(&a);
            c[i] += 1;
            i = 1;
        } else {
            c[i] = 0;
            i += 1;
        }
    }
}

/// Pearson and Spearman correlation of `x` with `y` and their two-sided permutation p-values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PermutationResult {
    pub pearson_r: f64,
    pub pearson_p: f64,
    pub spearman_r: f64,
    pub spearman_p: f64,
    pub exact: bool,
}

impl PermutationResult {
    const NAN: PermutationResult = PermutationResult {
        pearson_r: f64::NAN,
        pearson_p: f64::NAN,
        spearman_r: f64::NAN,
        spearman_p: f64::NAN,
        exact: false,
    };
}

struct Tally {
    observed: [f64; 2],
    hits: [usize; 2],
    total: usize,
}

impl Tally {
    fn new(x: &[f64], y: &[f64], rx: &[f64], ry: &[f64]) -> Tally {
        Tally {
            observed: [pearson(x, y), pearson(rx, ry)],
            hits: [0, 0],
            total: 0,
        }
    }

    fn add(&mut self, r: [f64; 2]) {
        self.total += 1;
        for ((o, hit), v) in self.observed.iter().zip(self.hits.iter_mut()).zip(r) {
            if o.is_finite() && v.abs() >= o.abs() - 1e-12 {
                *hit += 1;
            }
        }
    }

    fn p(&self, k: usize, exact: bool) -> f64 {
        if !self.observed[k].is_finite() || self.total == 0 {
            f64::NAN
        } else if exact {
            self.hits[k] as f64 / self.total as f64
        } else {
            (self.hits[k] + 1) as f64 / (self.total + 1) as f64
        }
    }

    fn result(&self, exact: bool) -> PermutationResult {
        PermutationResult {
            pearson_r: self.observed[0],
            pearson_p: self.p(0, exact),
            spearman_r: self.observed[1],
            spearman_p: self.p(1, exact),
            exact,
        }
    }
}

fn permuted(v: &[f64], idx: &[usize]) -> Vec<f64> {
    idx.iter().map(|&i| v[i]).collect()
}

/// Permutation correlation test by exact enumeration of all `n!` label permutations (identity
/// included, `p = hits / n!`).
pub fn exact_permutation_test(x: &[f64], y: &[f64]) -> PermutationResult {
    if x.len() != y.len() || x.len() < 3 {
        return PermutationResult::NAN;
    }
    let (rx, ry) = (ranks_average(x), ranks_average(y));
    let mut tally = Tally::new(x, y, &rx, &ry);
    for_each_permutation(y.len(), |idx| {
        tally.add([
            pearson(x, &permuted(y, idx)),
            pearson(&rx, &permuted(&ry, idx)),
        ]);
    });
    tally.result(true)
}

/// Permutation correlation test with `permutations` random label permutations; the observed
/// labelling counts as one of them (`p = (hits + 1) / (permutations + 1)`, never 0).
pub fn monte_carlo_permutation_test(
    x: &[f64],
    y: &[f64],
    permutations: usize,
    rng: &mut Rng,
) -> PermutationResult {
    if x.len() != y.len() || x.len() < 3 {
        return PermutationResult::NAN;
    }
    let (rx, ry) = (ranks_average(x), ranks_average(y));
    let mut tally = Tally::new(x, y, &rx, &ry);
    let mut idx: Vec<usize> = (0..y.len()).collect();
    for _ in 0..permutations {
        rng.shuffle(&mut idx);
        tally.add([
            pearson(x, &permuted(y, &idx)),
            pearson(&rx, &permuted(&ry, &idx)),
        ]);
    }
    tally.result(false)
}

/// The permutation test of `specs/14` §6.3: all `n!` permutations up to [`EXACT_MAX_N`] points,
/// `permutations` random ones above. Fewer than 3 points give NaN.
pub fn permutation_test(
    x: &[f64],
    y: &[f64],
    permutations: usize,
    rng: &mut Rng,
) -> PermutationResult {
    if x.len() <= EXACT_MAX_N {
        exact_permutation_test(x, y)
    } else {
        monte_carlo_permutation_test(x, y, permutations, rng)
    }
}

/// Quartiles (linear interpolation of order statistics), whiskers at the most extreme values within
/// 1.5 inter-quartile ranges of the quartiles, and the values beyond them.
pub fn box_stats(values: &[f64]) -> BoxStats {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return BoxStats::default();
    }
    v.sort_by(f64::total_cmp);
    let q1 = quantile(&v, 0.25);
    let q3 = quantile(&v, 0.75);
    let iqr = q3 - q1;
    let (low, high) = (q1 - 1.5 * iqr, q3 + 1.5 * iqr);
    BoxStats {
        n: v.len(),
        min: v.first().copied(),
        q1: Some(q1),
        median: Some(quantile(&v, 0.5)),
        q3: Some(q3),
        max: v.last().copied(),
        whisker_low: v.iter().copied().find(|x| *x >= low),
        whisker_high: v.iter().copied().rev().find(|x| *x <= high),
        outliers: v
            .iter()
            .copied()
            .filter(|x| *x < low || *x > high)
            .collect(),
    }
}

struct Points {
    x: Vec<f64>,
    y: Vec<f64>,
}

fn side_value(side: &Option<SideVta>, a: usize, measure: &str) -> Option<f64> {
    side.as_ref()
        .filter(|_| has_vta(side))
        .and_then(|v| measure_value(v, a, measure))
        .filter(|x| x.is_finite())
}

fn points_of(
    record: &GroupRecord,
    subjects: &[SubjectVta],
    var: &ClinicalVar,
    structure: &str,
    measure: &str,
    combination: &str,
) -> Points {
    let mut out = Points {
        x: Vec::new(),
        y: Vec::new(),
    };
    let rows: Vec<(usize, &SubjectVta, usize)> = record
        .patients
        .iter()
        .enumerate()
        .filter(|(_, p)| p.included)
        .filter_map(|(i, p)| {
            let s = subjects.iter().find(|s| s.subject == p.subject)?;
            let a = s.structures.iter().position(|n| n == structure)?;
            Some((i, s, a))
        })
        .collect();
    let value_at = |i: usize, col: usize| {
        var.values
            .get(i)
            .and_then(|r| r.get(col))
            .copied()
            .flatten()
            .filter(|y| y.is_finite())
    };
    if var.kind == ClinicalKind::PerHemisphere {
        let sides: &[usize] = match combination {
            "right" => &[0],
            "left" => &[1],
            _ => &[0, 1],
        };
        for &side in sides {
            for &(i, s, a) in &rows {
                if let (Some(x), Some(y)) =
                    (side_value(&s.sides[side], a, measure), value_at(i, side))
                {
                    out.x.push(x);
                    out.y.push(y);
                }
            }
        }
        return out;
    }
    for &(i, s, a) in &rows {
        let Some(y) = value_at(i, 0) else { continue };
        let (has_r, has_l) = (has_vta(&s.sides[0]), has_vta(&s.sides[1]));
        let r = side_value(&s.sides[0], a, measure);
        let l = side_value(&s.sides[1], a, measure);
        let x = match combination {
            "right" => r,
            "left" => l,
            _ if !has_r && !has_l => None,
            _ => {
                let side = |has: bool, v: Option<f64>| if has { v } else { Some(0.0) };
                side(has_r, r).zip(side(has_l, l)).map(|(a, b)| a + b)
            }
        };
        if let Some(x) = x {
            out.x.push(x);
            out.y.push(y);
        }
    }
    out
}

fn two_values(var: &ClinicalVar) -> Option<[f64; 2]> {
    if var.kind != ClinicalKind::PerPatient {
        return None;
    }
    let mut seen: Vec<f64> = Vec::new();
    for v in var
        .values
        .iter()
        .filter_map(|r| r.first().copied().flatten())
    {
        if v.is_finite() && !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.sort_by(f64::total_cmp);
    (seen.len() == 2).then(|| [seen[0], seen[1]])
}

fn group_test(
    structure: &str,
    options: &ClinicalStatsOptions,
    side: &'static str,
    values: [f64; 2],
    p: &Points,
) -> Option<GroupTestRow> {
    let pick = |g: f64| -> Vec<f64> {
        p.x.iter()
            .zip(&p.y)
            .filter(|(_, y)| **y == g)
            .map(|(x, _)| *x)
            .collect()
    };
    let (g1, g2) = (pick(values[0]), pick(values[1]));
    if g1.is_empty() || g2.is_empty() {
        return None;
    }
    let test = t_test_two_sample(&g1, &g2);
    let mean = |g: &[f64]| finite(g.iter().sum::<f64>() / g.len() as f64);
    Some(GroupTestRow {
        structure: structure.to_owned(),
        measure: options.measure.clone(),
        side,
        variable: options.variable.clone(),
        group_values: values,
        n1: g1.len(),
        n2: g2.len(),
        mean1: mean(&g1),
        mean2: mean(&g2),
        t: finite(test.stat),
        df: test
            .stat
            .is_finite()
            .then(|| (g1.len() + g2.len() - 2) as f64),
        p: finite(test.p),
        boxes: [box_stats(&g1), box_stats(&g2)],
    })
}

/// Correlations (and, for a two-valued per-patient variable, two-group tests) of a measure of the
/// cohort VTA table with a clinical variable, per structure and side combination
/// (`specs/14` §6.3). Per-patient variables are used for `both`, `right`, `left`; per-hemisphere
/// variables pair `right` with column 1, `left` with column 2 and stack right then left for `both`.
pub fn clinical_stats(
    record: &GroupRecord,
    subjects: &[SubjectVta],
    options: &ClinicalStatsOptions,
) -> Result<ClinicalStats, GroupError> {
    if !CORRELATION_MEASURES.contains(&options.measure.as_str()) {
        return Err(GroupError::Invalid(format!(
            "unknown measure {:?}; use one of {}",
            options.measure,
            CORRELATION_MEASURES.join(", ")
        )));
    }
    let var = record
        .clinical
        .iter()
        .find(|c| c.name == options.variable)
        .ok_or_else(|| {
            GroupError::Invalid(format!(
                "the group has no clinical variable {:?}",
                options.variable
            ))
        })?;
    if !matches!(
        var.kind,
        ClinicalKind::PerPatient | ClinicalKind::PerHemisphere
    ) {
        return Err(GroupError::Invalid(format!(
            "clinical variable {:?} has one value per contact; pick a per-patient or per-hemisphere variable",
            var.name
        )));
    }
    if subjects.len() < 3 {
        return Err(GroupError::Lg {
            code: "E_GROUP_TOO_FEW",
            message: format!(
                "{} patients have VTA statistics; at least 3 are needed",
                subjects.len()
            ),
        });
    }
    let mut structures: Vec<String> = Vec::new();
    for s in subjects {
        for n in &s.structures {
            if !n.is_empty() && !structures.contains(n) {
                structures.push(n.clone());
            }
        }
    }
    let two = two_values(var);
    let mut out = ClinicalStats {
        variable: var.name.clone(),
        measure: options.measure.clone(),
        kind: var.kind,
        patients_with_vta: subjects.len(),
        permutations: options.permutations,
        seed: options.seed,
        correlations: Vec::new(),
        group_tests: Vec::new(),
        notes: Vec::new(),
    };
    let mut stream = 0u64;
    for structure in &structures {
        for &side in &options.sides {
            let p = points_of(record, subjects, var, structure, &options.measure, side);
            let mut rng = Rng::stream(options.seed, stream);
            stream += 1;
            let r = permutation_test(&p.x, &p.y, options.permutations, &mut rng);
            out.correlations.push(CorrelationRow {
                structure: structure.clone(),
                measure: options.measure.clone(),
                side,
                variable: var.name.clone(),
                n: p.x.len(),
                pearson_r: finite(r.pearson_r),
                pearson_p_perm: finite(r.pearson_p),
                spearman_r: finite(r.spearman_r),
                spearman_p_perm: finite(r.spearman_p),
            });
            if let Some(values) = two {
                out.group_tests
                    .extend(group_test(structure, options, side, values, &p));
            }
        }
    }
    if out.correlations.iter().any(|r| r.n < 3) {
        out.notes.push(Note::plain(
            "some rows have fewer than 3 points and no statistics",
        ));
    }
    Ok(out)
}

fn num(v: Option<f64>) -> Cell {
    Cell::Num(v.filter(|x| x.is_finite()))
}

/// The correlation table.
pub fn correlation_table(rows: &[CorrelationRow]) -> Table {
    Table {
        columns: &CORRELATION_COLUMNS,
        rows: rows
            .iter()
            .map(|r| {
                vec![
                    Cell::Text(r.structure.clone()),
                    Cell::Text(r.measure.clone()),
                    Cell::Text(r.side.to_owned()),
                    Cell::Text(r.variable.clone()),
                    Cell::Int(r.n as u64),
                    num(r.pearson_r),
                    num(r.pearson_p_perm),
                    num(r.spearman_r),
                    num(r.spearman_p_perm),
                ]
            })
            .collect(),
    }
}

/// The two-group table.
pub fn group_test_table(rows: &[GroupTestRow]) -> Table {
    Table {
        columns: &GROUP_TEST_COLUMNS,
        rows: rows
            .iter()
            .map(|r| {
                vec![
                    Cell::Text(r.structure.clone()),
                    Cell::Text(r.measure.clone()),
                    Cell::Text(r.side.to_owned()),
                    Cell::Text(r.variable.clone()),
                    num(Some(r.group_values[0])),
                    num(Some(r.group_values[1])),
                    Cell::Int(r.n1 as u64),
                    Cell::Int(r.n2 as u64),
                    num(r.mean1),
                    num(r.mean2),
                    num(r.t),
                    num(r.df),
                    num(r.p),
                ]
            })
            .collect(),
    }
}

impl ClinicalStats {
    /// The analysis record body: settings, every row and the box-plot numbers.
    pub fn to_json(&self) -> Value {
        json!({
            "variable": self.variable,
            "measure": self.measure,
            "permutations": self.permutations,
            "seed": self.seed,
            "exact_up_to_n": EXACT_MAX_N,
            "patients_with_vta": self.patients_with_vta,
            "correlations": self.correlations.iter().map(|r| json!({
                "structure": r.structure,
                "side": r.side,
                "n": r.n,
                "pearson_r": r.pearson_r,
                "pearson_p_perm": r.pearson_p_perm,
                "spearman_r": r.spearman_r,
                "spearman_p_perm": r.spearman_p_perm,
            })).collect::<Vec<_>>(),
            "group_tests": self.group_tests.iter().map(|r| json!({
                "structure": r.structure,
                "side": r.side,
                "group_values": r.group_values,
                "n1": r.n1,
                "n2": r.n2,
                "mean1": r.mean1,
                "mean2": r.mean2,
                "t": r.t,
                "df": r.df,
                "p": r.p,
                "boxplot": [r.boxes[0].to_json(), r.boxes[1].to_json()],
            })).collect::<Vec<_>>(),
        })
    }
}

/// What [`run_clinical_stats`] wrote.
#[derive(Debug, Clone)]
pub struct ClinicalStatsRun {
    pub stats: ClinicalStats,
    pub files: Vec<PathBuf>,
    pub analysis_record: PathBuf,
    pub missing_vta: Vec<String>,
    pub notes: Vec<Note>,
}

fn analysis_id(variable: &str, measure: &str) -> String {
    let clean: String = variable
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("clinical-{clean}-{}", measure.replace('_', "-"))
}

fn io_error(path: &Path, e: &std::io::Error) -> GroupError {
    GroupError::Io(format!("cannot write {}: {e}", path.display()))
}

/// Reads the VTA statistics of the group, computes [`clinical_stats`] and writes the tables
/// `…_desc-clinicalcorrelation_stats.tsv` (+ `.csv`, + `.json` sidecar) and, when the variable has
/// two values, `…_desc-clinicalgroupcomparison_stats.tsv` under `<group folder>/exports/` (or
/// `out_dir`), plus `atlasstats/<id>/analysis.json` with the inputs fingerprint, the software
/// stamp and the box-plot numbers. The analysis is registered in the record (the caller saves it).
pub fn run_clinical_stats(
    record: &mut GroupRecord,
    folder: &Path,
    options: &ClinicalStatsOptions,
    csv: bool,
    out_dir: Option<&Path>,
) -> Result<ClinicalStatsRun, GroupError> {
    let load = load_vta_stats(record);
    let stats = clinical_stats(record, &load.subjects, options)?;
    let dir = out_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| folder.join("exports"));
    let inputs = inputs_of(record);
    let mut files = Vec::new();
    write_table(
        &dir,
        record,
        "clinicalcorrelation",
        &correlation_table(&stats.correlations),
        csv,
        &inputs,
        &mut files,
    )?;
    if !stats.group_tests.is_empty() {
        write_table(
            &dir,
            record,
            "clinicalgroupcomparison",
            &group_test_table(&stats.group_tests),
            csv,
            &inputs,
            &mut files,
        )?;
    }
    let (fingerprint_inputs, fingerprint) = group_fingerprint(record);
    let id = analysis_id(&stats.variable, &stats.measure);
    let relative = format!("{ANALYSIS_KIND}/{id}/analysis.json");
    let path = folder.join(&relative);
    let mut body = json!({
        "schema": "dbsloc-group-analysis/1",
        "kind": ANALYSIS_KIND,
        "id": id,
        "software": software_stamp(),
        "guid": record.guid,
        "label": record.stim_label(),
        "atlas_set": record.atlas_set,
        "created_at": utc_now_rfc3339(),
        "inputs": fingerprint_inputs,
        "fingerprint": fingerprint,
        "tables": files
            .iter()
            .map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect::<Vec<_>>(),
    });
    if let (Value::Object(b), Value::Object(s)) = (&mut body, stats.to_json()) {
        b.extend(s);
    }
    let text = serde_json::to_string_pretty(&body)
        .map(|t| t + "\n")
        .map_err(|e| GroupError::Json(e.to_string()))?;
    write_replacing(&path, text.as_bytes()).map_err(|e| io_error(&path, &e))?;
    record
        .analyses
        .retain(|a| !(a.kind == ANALYSIS_KIND && a.id == id));
    record.analyses.push(AnalysisRef {
        kind: ANALYSIS_KIND.to_owned(),
        id,
        record: relative,
    });
    let mut notes = load.notes;
    notes.extend(stats.notes.clone());
    for m in &load.missing {
        notes.push(Note::plain(format!(
            "{m} has no VTA statistics for {}; it is left out",
            record.stim_label()
        )));
    }
    Ok(ClinicalStatsRun {
        stats,
        files,
        analysis_record: path,
        missing_vta: load.missing,
        notes,
    })
}

#[cfg(test)]
#[path = "atlas_stats_tests.rs"]
mod tests;
