// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Sweet-spot mapping of a group (`specs/14` §7): voxel statistics of VTAs / E-fields against a
//! clinical outcome, cross-validation, the stored model and the outputs of §7.7.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::clinical::ClinicalKind;
use super::record::{subject_id, GroupRecord};
use super::stats_common as sc;
use super::{GroupError, Note};
use crate::connectome_maps::nii::{
    apply, diagonal_affine, encode_nii, invert_affine, read_nii, Affine, NiiVolume,
};
use crate::sha256::{file_sha256_hex, sha256_hex};
use crate::vat::settings::load as load_stimulation;

/// Field value above which a voxel counts for the common grid (E-fields, V/m).
pub const GRID_EFIELD_THRESHOLD: f32 = 150.0;
/// Value above which a voxel of a binary VTA counts.
pub const BINARY_CUT: f32 = 0.5;
/// The default random seed.
pub const DEFAULT_SEED: u64 = 20_240_607;

const MAX_GRID_VOXELS: f64 = 3.0e8;
const MAX_DIM: usize = 2048;

fn normalise(text: &str) -> String {
    let mut s: String = text
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    for suffix in ["binaryvar", "tests", "test"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            if !rest.is_empty() {
                s = rest.to_owned();
            }
        }
    }
    s.trim_end_matches(|c: char| c.is_ascii_digit()).to_owned()
}

macro_rules! named_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident => $label:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $( #[serde(rename = $label)] $var ),+ }
        impl $name {
            /// Every value.
            pub const ALL: &'static [$name] = &[$($name::$var),+];
            /// The lower-case name used in records and on the command line.
            pub fn name(self) -> &'static str { match self { $($name::$var => $label),+ } }
            /// Parses a name, also in the reference spelling (case, spaces and dashes ignored).
            pub fn parse(text: &str) -> Option<$name> {
                let key = normalise(text);
                Self::ALL.iter().copied().find(|v| normalise(v.name()) == key)
            }
        }
    };
}

named_enum!(
    /// Statistic level.
    Level { Vtas => "vtas", EFields => "e-fields" }
);
named_enum!(
    /// Voxel test.
    Test {
        MeanImage => "mean-image",
        NImage => "n-image",
        TTest => "t-test",
        Wilcoxon => "wilcoxon-test",
        Proportion => "proportion-test",
        Binomial => "binomial-test",
        Correlations => "correlations",
        ReverseTTests => "reverse-t-tests",
    }
);
named_enum!(
    /// Null hypothesis.
    NullHypothesis { Zero => "zero", Average => "average", Threshold => "threshold" }
);
named_enum!(
    /// Amplitude correction.
    AmplitudeCorrection {
        None => "none",
        Amplitude => "amplitude",
        VtaSize => "vta-size",
        AmplitudeDiscrete => "amplitude-discrete",
        AmplitudeRegression => "amplitude-regression",
    }
);
named_enum!(
    /// Outcome normalisation.
    Normalisation { None => "none", ZScore => "z-score", VanAlbada => "van-albada" }
);
named_enum!(
    /// Correlation type.
    CorrelationType { Spearman => "spearman", Pearson => "pearson" }
);
named_enum!(
    /// Multiple-comparison correction.
    Multiple { Uncorrected => "uncorrected", Fdr => "fdr", Bonferroni => "bonferroni" }
);
named_enum!(
    /// Cross-validation strategy.
    CvStrategy {
        LeaveNothingOut => "leave-nothing-out",
        LeaveOnePatientOut => "leave-one-patient-out",
        LeaveOneCohortOut => "leave-one-cohort-out",
        KFold => "k-fold",
    }
);
named_enum!(
    /// How the predicted score of a patient is formed from the map.
    PredictionBasis {
        MeanOfScores => "mean-of-scores",
        SumOfScores => "sum-of-scores",
        PeakOfScores => "peak-of-scores",
        Peak5OfScores => "peak-5-of-scores",
        ProfileSpearman => "profile-spearman",
        ProfilePearson => "profile-pearson",
    }
);
named_enum!(
    /// A literature preset (`specs/14` §7.1).
    Preset {
        Butson2011 => "butson-2011",
        Cheung2011 => "cheung-2011",
        Eisenstein2014 => "eisenstein-2014",
        Reich2019 => "reich-2019",
        Dembek2019 => "dembek-2019",
        Elias2020 => "elias-2020",
    }
);

impl Test {
    /// The name used in file names (letters only).
    pub fn file_slug(self) -> String {
        self.name().replace('-', "")
    }

    fn has_p(self) -> bool {
        !matches!(self, Test::MeanImage | Test::NImage)
    }
}

/// The settings of a sweet-spot analysis (`specs/14` §7.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SweetspotSettings {
    pub resolution: f64,
    pub level: Level,
    pub efield_threshold: f64,
    pub nan_threshold: f64,
    pub coverage_percent: f64,
    pub test: Test,
    pub null_hypothesis: NullHypothesis,
    pub improvement_threshold: f64,
    pub n_threshold_percent: f64,
    pub amplitude_correction: AmplitudeCorrection,
    pub normalisation: Normalisation,
    pub correlation: CorrelationType,
    pub evaluation_correlation: CorrelationType,
    pub show_significant_only: bool,
    pub alpha: f64,
    pub multiple_comparisons: Multiple,
    pub mirror: bool,
    pub split_by_group: bool,
    pub show_positive: bool,
    pub show_negative: bool,
    pub cv: CvStrategy,
    pub k_folds: usize,
    pub permutations: usize,
    pub basis: PredictionBasis,
    pub seed: u64,
}

impl Default for SweetspotSettings {
    fn default() -> Self {
        SweetspotSettings {
            resolution: 0.5,
            level: Level::Vtas,
            efield_threshold: 200.0,
            nan_threshold: 0.0,
            coverage_percent: 20.0,
            test: Test::TTest,
            null_hypothesis: NullHypothesis::Zero,
            improvement_threshold: 0.0,
            n_threshold_percent: 0.0,
            amplitude_correction: AmplitudeCorrection::None,
            normalisation: Normalisation::None,
            correlation: CorrelationType::Spearman,
            evaluation_correlation: CorrelationType::Spearman,
            show_significant_only: true,
            alpha: 0.05,
            multiple_comparisons: Multiple::Uncorrected,
            mirror: false,
            split_by_group: false,
            show_positive: true,
            show_negative: false,
            cv: CvStrategy::LeaveOnePatientOut,
            k_folds: 5,
            permutations: 1000,
            basis: PredictionBasis::MeanOfScores,
            seed: DEFAULT_SEED,
        }
    }
}

impl SweetspotSettings {
    /// Sets the test, null hypothesis, thresholds and amplitude correction of a preset.
    pub fn apply_preset(&mut self, preset: Preset) {
        let (test, null, imp, nthr, amp) = match preset {
            Preset::Butson2011 => (
                Test::MeanImage,
                NullHypothesis::Threshold,
                0.5,
                0.0,
                AmplitudeCorrection::None,
            ),
            Preset::Cheung2011 => (
                Test::NImage,
                NullHypothesis::Threshold,
                0.75,
                75.0,
                AmplitudeCorrection::None,
            ),
            Preset::Eisenstein2014 => (
                Test::TTest,
                NullHypothesis::Zero,
                0.0,
                0.0,
                AmplitudeCorrection::None,
            ),
            Preset::Reich2019 => (
                Test::TTest,
                NullHypothesis::Average,
                0.0,
                0.0,
                AmplitudeCorrection::None,
            ),
            Preset::Dembek2019 => (
                Test::Wilcoxon,
                NullHypothesis::Average,
                0.0,
                0.0,
                AmplitudeCorrection::AmplitudeDiscrete,
            ),
            Preset::Elias2020 => (
                Test::Wilcoxon,
                NullHypothesis::Average,
                0.0,
                0.0,
                AmplitudeCorrection::VtaSize,
            ),
        };
        self.test = test;
        self.null_hypothesis = null;
        self.improvement_threshold = imp;
        self.n_threshold_percent = nthr;
        self.amplitude_correction = amp;
    }

    /// Checks the combination of settings.
    pub fn validate(&self) -> Result<(), GroupError> {
        let bad = |m: &str| Err(GroupError::Invalid(format!("sweet-spot settings: {m}")));
        if !(self.resolution.is_finite() && self.resolution > 0.0) {
            return bad("the resolution must be above 0");
        }
        if !(0.0..=100.0).contains(&self.coverage_percent) {
            return bad("the coverage threshold must be between 0 and 100");
        }
        if !(0.0..=100.0).contains(&self.n_threshold_percent) {
            return bad("the N threshold must be between 0 and 100");
        }
        let vta_test = matches!(
            self.test,
            Test::MeanImage
                | Test::NImage
                | Test::TTest
                | Test::Wilcoxon
                | Test::Proportion
                | Test::Binomial
        );
        match (self.level, vta_test) {
            (Level::Vtas, false) => {
                return bad("Correlations and Reverse T-Tests need the level E-Fields")
            }
            (Level::EFields, true) => {
                return bad("the chosen test needs the level VTAs; E-Fields allow Correlations and Reverse T-Tests")
            }
            _ => {}
        }
        if self.level == Level::Vtas
            && matches!(
                self.basis,
                PredictionBasis::ProfileSpearman | PredictionBasis::ProfilePearson
            )
        {
            return bad("the Profile of Scores basis needs the level E-Fields");
        }
        if !self.show_positive && !self.show_negative {
            return bad("show at least one sign");
        }
        if self.k_folds < 2 {
            return bad("k must be at least 2");
        }
        if self.permutations == 0 {
            return bad("at least one permutation");
        }
        if !(self.alpha > 0.0 && self.alpha <= 1.0) {
            return bad("alpha must be in (0, 1]");
        }
        Ok(())
    }

    /// The part of the file names that tells which signs the map shows.
    pub fn desc(&self) -> &'static str {
        match (self.show_positive, self.show_negative) {
            (true, true) => "both",
            (true, false) => "pos",
            _ => "neg",
        }
    }
}

/// Number of patients a voxel must be covered by: `round(n * c / 100)`, halves away from zero.
pub fn coverage_count(n: usize, coverage_percent: f64) -> usize {
    (n as f64 * coverage_percent / 100.0).round().max(0.0) as usize
}

/// The common grid of one side: axis-aligned, spacing `resolution`, voxel 0 centred at `origin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    pub dims: [usize; 3],
    pub origin: [f64; 3],
    pub resolution: f64,
}

impl Grid {
    /// The voxel-to-mm affine.
    pub fn affine(&self) -> Affine {
        diagonal_affine([self.resolution; 3], self.origin)
    }

    /// Number of voxels.
    pub fn len(&self) -> usize {
        self.dims[0] * self.dims[1] * self.dims[2]
    }

    /// `true` for a grid without voxels.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// World bounding box (voxel centres) of the voxels of `vol` above `cut`.
pub fn supra_bounds(vol: &NiiVolume, cut: f32) -> Option<[[f64; 3]; 2]> {
    let [nx, ny, _] = vol.dims;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut found = false;
    for (n, x) in vol.data.iter().enumerate() {
        if *x > cut {
            let i = n % nx;
            let j = (n / nx) % ny;
            let k = n / (nx * ny);
            let w = apply(&vol.affine, [i as f64, j as f64, k as f64]);
            for a in 0..3 {
                lo[a] = lo[a].min(w[a]);
                hi[a] = hi[a].max(w[a]);
            }
            found = true;
        }
    }
    found.then_some([lo, hi])
}

/// The common grid of `volumes` (each with the cut that decides its supra-threshold voxels):
/// the union of the boxes extended outward to multiples of `resolution` (`specs/14` §7.2).
pub fn common_grid(volumes: &[(&NiiVolume, f32)], resolution: f64) -> Result<Grid, GroupError> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (vol, cut) in volumes {
        if let Some(b) = supra_bounds(vol, *cut) {
            for a in 0..3 {
                lo[a] = lo[a].min(b[0][a]);
                hi[a] = hi[a].max(b[1][a]);
            }
        }
    }
    if lo.iter().any(|v| !v.is_finite()) {
        return Err(GroupError::Invalid(
            "no field has a voxel above the grid threshold".to_owned(),
        ));
    }
    let mut origin = [0.0; 3];
    let mut dims = [0usize; 3];
    for a in 0..3 {
        let first = (lo[a] / resolution + 1e-6).floor();
        let last = (hi[a] / resolution - 1e-6).ceil();
        origin[a] = first * resolution;
        dims[a] = (last - first).round() as usize + 1;
    }
    if dims.iter().map(|d| *d as f64).product::<f64>() > MAX_GRID_VOXELS {
        return Err(GroupError::Invalid(format!(
            "the common grid would have {}x{}x{} voxels; use a coarser resolution",
            dims[0], dims[1], dims[2]
        )));
    }
    Ok(Grid {
        dims,
        origin,
        resolution,
    })
}

/// Resamples `vol` onto `grid` by nearest neighbour into `out`; outside the image and NaN are 0.
pub fn resample_nearest_into(vol: &NiiVolume, grid: &Grid, out: &mut [f32]) {
    out.fill(0.0);
    let Some(inv) = invert_affine(&vol.affine) else {
        return;
    };
    let [nx, ny, nz] = grid.dims;
    let [sx, sy, sz] = vol.dims;
    let r = grid.resolution;
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let w = [
                    grid.origin[0] + i as f64 * r,
                    grid.origin[1] + j as f64 * r,
                    grid.origin[2] + k as f64 * r,
                ];
                let p = apply(&inv, w);
                let (a, b, c) = (p[0].round(), p[1].round(), p[2].round());
                if a < 0.0 || b < 0.0 || c < 0.0 {
                    continue;
                }
                let (a, b, c) = (a as usize, b as usize, c as usize);
                if a >= sx || b >= sy || c >= sz {
                    continue;
                }
                let x = vol.data[(c * sy + b) * sx + a];
                if x.is_finite() {
                    out[(k * ny + j) * nx + i] = x;
                }
            }
        }
    }
}

/// The values of one side: `rows` images resampled onto the grid (row-major), each row with the
/// value above which a voxel counts as covered.
#[derive(Debug, Clone, PartialEq)]
pub struct SideMatrix {
    pub rows: usize,
    pub v: usize,
    pub data: Vec<f32>,
    pub cut: Vec<f32>,
    pub empty: Vec<bool>,
}

impl SideMatrix {
    /// Builds the matrix; a `None` image is an empty (all-zero) row.
    pub fn build(images: &[Option<&NiiVolume>], cut: &[f32], grid: &Grid) -> SideMatrix {
        let v = grid.len();
        let rows = images.len();
        let mut data = vec![0f32; rows * v];
        if v > 0 {
            data.par_chunks_mut(v)
                .zip(images.par_iter())
                .for_each(|(chunk, img)| {
                    if let Some(vol) = img {
                        resample_nearest_into(vol, grid, chunk);
                    }
                });
        }
        SideMatrix {
            rows,
            v,
            data,
            cut: cut.to_vec(),
            empty: images.iter().map(Option::is_none).collect(),
        }
    }

    /// The values of row `r`.
    pub fn row(&self, r: usize) -> &[f32] {
        &self.data[r * self.v..(r + 1) * self.v]
    }
}

/// For every voxel the rows that cover it (value above the row's cut), and the covered voxel
/// count of every row.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    offsets: Vec<usize>,
    rows: Vec<u32>,
    pub per_row: Vec<usize>,
}

impl Coverage {
    /// Builds the coverage lists of a matrix.
    #[allow(clippy::needless_range_loop)]
    pub fn build(m: &SideMatrix) -> Coverage {
        let mut counts = vec![0usize; m.v + 1];
        let mut per_row = vec![0usize; m.rows];
        for r in 0..m.rows {
            let cut = m.cut[r];
            for (v, x) in m.row(r).iter().enumerate() {
                if *x > cut {
                    counts[v + 1] += 1;
                    per_row[r] += 1;
                }
            }
        }
        for v in 0..m.v {
            counts[v + 1] += counts[v];
        }
        let mut cursor = counts.clone();
        let mut rows = vec![0u32; counts[m.v]];
        for r in 0..m.rows {
            let cut = m.cut[r];
            for (v, x) in m.row(r).iter().enumerate() {
                if *x > cut {
                    rows[cursor[v]] = r as u32;
                    cursor[v] += 1;
                }
            }
        }
        Coverage {
            offsets: counts,
            rows,
            per_row,
        }
    }

    /// The rows covering voxel `v`, ascending.
    pub fn at(&self, v: usize) -> &[u32] {
        &self.rows[self.offsets[v]..self.offsets[v + 1]]
    }
}

enum H0 {
    Scalar(f64),
    PerRow(Vec<f64>),
}

/// The maps of one voxel-statistics run; NaN marks a voxel that is not part of the map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapResult {
    pub values: Vec<f32>,
    pub p: Vec<f32>,
    pub coverage: Vec<f32>,
    pub members: usize,
    pub n_c: usize,
}

fn f32_of(x: f64) -> f32 {
    if x.is_finite() {
        x as f32
    } else {
        f32::NAN
    }
}

/// Voxel statistics of one side (`specs/14` §7.4) over the rows `q`. `outcome` and `amp` hold one
/// value per row of the matrix; rows with a non-finite outcome are left out.
pub fn compute_map(
    m: &SideMatrix,
    cov: &Coverage,
    outcome: &[f64],
    amp: &[f64],
    q: &[usize],
    s: &SweetspotSettings,
    binary_outcome: bool,
) -> MapResult {
    let rows = m.rows;
    let z: Vec<f64> = cov
        .per_row
        .iter()
        .map(|&c| if c == 0 { f64::NAN } else { c as f64 })
        .collect();
    let mut ival = vec![f64::NAN; rows];
    let mut in_q = vec![false; rows];
    let mut members = Vec::new();
    let mut sorted_q = q.to_vec();
    sorted_q.sort_unstable();
    for &r in &sorted_q {
        let o = outcome[r];
        let c = if s.level == Level::Vtas {
            match s.amplitude_correction {
                AmplitudeCorrection::Amplitude => o / amp[r],
                AmplitudeCorrection::VtaSize => o / z[r],
                _ => o,
            }
        } else {
            o
        };
        if c.is_finite() {
            ival[r] = c;
            in_q[r] = true;
            members.push(r);
        }
    }
    let n = members.len();
    let n_c = coverage_count(n, s.coverage_percent);
    let h0 = match s.null_hypothesis {
        NullHypothesis::Zero => H0::Scalar(0.0),
        NullHypothesis::Threshold => H0::Scalar(s.improvement_threshold),
        NullHypothesis::Average => match s.amplitude_correction {
            AmplitudeCorrection::AmplitudeDiscrete if s.level == Level::Vtas => {
                let mut h = vec![f64::NAN; rows];
                for &i in &members {
                    let same: Vec<f64> = members
                        .iter()
                        .filter(|&&j| amp[j].to_bits() == amp[i].to_bits() && amp[i].is_finite())
                        .map(|&j| outcome[j])
                        .collect();
                    h[i] = sc::mean(&same);
                }
                H0::PerRow(h)
            }
            AmplitudeCorrection::AmplitudeRegression if s.level == Level::Vtas => {
                let (mut sao, mut saa) = (0.0, 0.0);
                for &i in &members {
                    if amp[i].is_finite() {
                        sao += amp[i] * outcome[i];
                        saa += amp[i] * amp[i];
                    }
                }
                let b = if saa > 0.0 { sao / saa } else { f64::NAN };
                let mut h = vec![f64::NAN; rows];
                for &i in &members {
                    h[i] = b * amp[i];
                }
                H0::PerRow(h)
            }
            _ => H0::Scalar(sc::mean(
                &members.iter().map(|&i| ival[i]).collect::<Vec<_>>(),
            )),
        },
    };
    let resp_total = members.iter().filter(|&&r| ival[r] > 0.0).count();
    let thr_resp = |r: usize| binary_outcome || ival[r] > s.improvement_threshold;
    let n_prime = members.iter().filter(|&&r| thr_resp(r)).count();
    let n_req = coverage_count(n_prime, s.n_threshold_percent);
    let v_count = m.v;

    let voxel = |buf: &mut (Vec<usize>, Vec<f64>), v: usize| -> (f64, f64, usize) {
        let (idx, vals) = buf;
        idx.clear();
        vals.clear();
        for &r in cov.at(v) {
            let r = r as usize;
            if in_q[r] {
                idx.push(r);
                vals.push(ival[r]);
            }
        }
        let nv = idx.len();
        let nan = (f64::NAN, f64::NAN, nv);
        match s.test {
            Test::MeanImage => {
                if nv == 0 || nv < n_c {
                    nan
                } else {
                    (sc::mean(vals), f64::NAN, nv)
                }
            }
            Test::NImage => {
                if nv == 0 {
                    return nan;
                }
                let np = idx.iter().filter(|&&r| thr_resp(r)).count();
                if np < n_req {
                    nan
                } else {
                    (np as f64, f64::NAN, nv)
                }
            }
            Test::TTest => {
                if nv < n_c.max(2) {
                    return nan;
                }
                let r = match &h0 {
                    H0::Scalar(h) => sc::t_test_one_sample(vals, *h),
                    H0::PerRow(h) => {
                        let hs: Vec<f64> = idx.iter().map(|&r| h[r]).collect();
                        sc::t_test_paired(vals, &hs)
                    }
                };
                (r.stat, r.p, nv)
            }
            Test::Wilcoxon => {
                if nv < n_c.max(1) {
                    return nan;
                }
                let (diffs, shift) = match &h0 {
                    H0::Scalar(h) => (
                        vals.iter().map(|x| x - h).collect::<Vec<_>>(),
                        sc::median(vals) - h,
                    ),
                    H0::PerRow(h) => {
                        let hs: Vec<f64> = idx.iter().map(|&r| h[r]).collect();
                        (
                            vals.iter().zip(&hs).map(|(a, b)| a - b).collect(),
                            sc::median(vals) - sc::median(&hs),
                        )
                    }
                };
                if diffs.iter().any(|d| !d.is_finite()) {
                    return nan;
                }
                let r = sc::wilcoxon_signed_rank(&diffs);
                let mag = -r.p.max(1e-300).log10();
                (if shift < 0.0 { -mag } else { mag }, r.p, nv)
            }
            Test::Proportion => {
                if nv == 0 || nv <= n_c || resp_total == 0 || resp_total == n {
                    return nan;
                }
                let k1 = idx.iter().filter(|&&r| ival[r] > 0.0).count() as u64;
                let k0 = nv as u64 - k1;
                let r = sc::chi2_two_proportions_yates(
                    k1,
                    resp_total as u64,
                    k0,
                    (n - resp_total) as u64,
                );
                (r.stat, r.p, nv)
            }
            Test::Binomial => {
                if nv == 0 || nv <= n_c || n == 0 {
                    return nan;
                }
                let k = idx.iter().filter(|&&r| ival[r] > 0.0).count() as u64;
                let p0 = resp_total as f64 / n as f64;
                let value = k as f64 / nv as f64 - p0;
                (value, sc::binomial_two_sided_p(k, nv as u64, p0), nv)
            }
            Test::Correlations | Test::ReverseTTests => {
                if nv == 0 {
                    return nan;
                }
                let mut e = Vec::with_capacity(members.len());
                let mut o = Vec::with_capacity(members.len());
                let mut resp = Vec::with_capacity(members.len());
                for &r in &members {
                    let x = m.data[r * v_count + v];
                    if x.is_finite() && f64::from(x) >= s.nan_threshold {
                        e.push(f64::from(x));
                        o.push(ival[r]);
                        resp.push(ival[r] > 0.0);
                    }
                }
                if s.test == Test::Correlations {
                    if nv < n_c || e.len() < 3 {
                        return nan;
                    }
                    let r = match s.correlation {
                        CorrelationType::Spearman => sc::spearman(&e, &o),
                        CorrelationType::Pearson => sc::pearson(&e, &o),
                    };
                    if r.is_nan() {
                        return nan;
                    }
                    (r, sc::correlation_p(r, e.len()), nv)
                } else {
                    let total: f64 = e.iter().sum();
                    if total.is_nan() || total <= 0.0 || e.len() < n_c.max(2) {
                        return nan;
                    }
                    let er: Vec<f64> = e
                        .iter()
                        .zip(&resp)
                        .filter(|(_, r)| **r)
                        .map(|(x, _)| *x)
                        .collect();
                    let en: Vec<f64> = e
                        .iter()
                        .zip(&resp)
                        .filter(|(_, r)| !**r)
                        .map(|(x, _)| *x)
                        .collect();
                    let r = if er.is_empty() {
                        sc::TestResult::NAN
                    } else if en.is_empty() {
                        sc::t_test_one_sample(&er, 0.0)
                    } else {
                        sc::t_test_two_sample(&er, &en)
                    };
                    (r.stat, r.p, nv)
                }
            }
        }
    };

    let results: Vec<(f64, f64, usize)> = (0..v_count)
        .into_par_iter()
        .map_init(|| (Vec::new(), Vec::new()), |buf, v| voxel(buf, v))
        .collect();
    let mut values: Vec<f32> = results.iter().map(|r| f32_of(r.0)).collect();
    let p: Vec<f32> = results.iter().map(|r| f32_of(r.1)).collect();
    let coverage: Vec<f32> = results.iter().map(|r| r.2 as f32).collect();

    if s.show_significant_only && s.test.has_p() {
        let live: Vec<usize> = (0..v_count)
            .filter(|&v| values[v].is_finite() && p[v].is_finite())
            .collect();
        let raw: Vec<f64> = live.iter().map(|&v| f64::from(p[v])).collect();
        let adj = match s.multiple_comparisons {
            Multiple::Uncorrected => raw.clone(),
            Multiple::Fdr => sc::fdr_reference(&raw),
            Multiple::Bonferroni => sc::bonferroni(&raw),
        };
        for (k, &v) in live.iter().enumerate() {
            if adj[k] > s.alpha {
                values[v] = f32::NAN;
            }
        }
    }
    MapResult {
        values,
        p,
        coverage,
        members: n,
        n_c,
    }
}

/// Keeps only the visible signs of a map; hidden voxels become NaN.
pub fn apply_signs(map: &[f32], show_positive: bool, show_negative: bool) -> Vec<f32> {
    map.iter()
        .map(|&x| {
            if (x > 0.0 && !show_positive) || (x < 0.0 && !show_negative) {
                f32::NAN
            } else {
                x
            }
        })
        .collect()
}

/// Predicted score of one patient (`specs/14` §7.5) from a map (signs already applied) and the
/// patient's values on the same grid. `cut` is the row's covered threshold (VTAs).
#[allow(clippy::too_many_arguments)]
pub fn predict_row(
    map: &[f32],
    row: &[f32],
    cut: f32,
    level: Level,
    basis: PredictionBasis,
    show_positive: bool,
    show_negative: bool,
) -> f64 {
    let w = |v: usize| -> f64 {
        let x = row[v];
        match level {
            Level::Vtas => f64::from(u8::from(x > cut)),
            Level::EFields => {
                if x.is_finite() {
                    f64::from(x)
                } else {
                    0.0
                }
            }
        }
    };
    match basis {
        PredictionBasis::MeanOfScores | PredictionBasis::SumOfScores => {
            let (mut sum, mut cnt) = (0.0, 0usize);
            for (v, &mv) in map.iter().enumerate() {
                if mv.is_finite() {
                    sum += f64::from(mv) * w(v);
                    cnt += 1;
                }
            }
            if cnt == 0 {
                f64::NAN
            } else if basis == PredictionBasis::MeanOfScores {
                sum / cnt as f64
            } else {
                sum
            }
        }
        PredictionBasis::PeakOfScores | PredictionBasis::Peak5OfScores => {
            let mut vals: Vec<f64> = map
                .iter()
                .enumerate()
                .filter(|(_, mv)| mv.is_finite())
                .map(|(v, mv)| f64::from(*mv) * w(v))
                .collect();
            if vals.is_empty() {
                return f64::NAN;
            }
            vals.sort_by(|a, b| b.total_cmp(a));
            let (top, bottom) = if basis == PredictionBasis::PeakOfScores {
                (vals[0], vals[vals.len() - 1])
            } else {
                let k = ((0.05 * map.len() as f64).ceil() as usize)
                    .max(1)
                    .min(vals.len());
                (
                    vals[..k].iter().sum::<f64>(),
                    vals[vals.len() - k..].iter().sum::<f64>(),
                )
            };
            match (show_positive, show_negative) {
                (true, false) => top,
                (false, true) => bottom,
                (true, true) => top + bottom,
                _ => f64::NAN,
            }
        }
        PredictionBasis::ProfileSpearman | PredictionBasis::ProfilePearson => {
            let mut a = Vec::new();
            let mut b = Vec::new();
            for (v, &mv) in map.iter().enumerate() {
                if mv.is_finite() && row[v].is_finite() {
                    a.push(f64::from(mv));
                    b.push(f64::from(row[v]));
                }
            }
            let r = if basis == PredictionBasis::ProfileSpearman {
                sc::spearman(&a, &b)
            } else {
                sc::pearson(&a, &b)
            };
            sc::fisher_z(r)
        }
    }
}

fn stack_value(values: &[Vec<f64>], p: usize, side: usize) -> f64 {
    let row = &values[p];
    match row.len() {
        0 => f64::NAN,
        1 => row[0],
        _ => row[side.min(row.len() - 1)],
    }
}

/// The outcome of one side stacked over the rows (`specs/14` §7.3 steps 1 and D7): rows
/// `0..N` are the patients' own values for `side`; with mirroring rows `N..2N` hold the score of
/// the hemisphere the flipped copy came from (a per-patient value is repeated).
pub fn stack_outcome(values: &[Vec<f64>], side: usize, mirror: bool) -> Vec<f64> {
    let n = values.len();
    let mut out: Vec<f64> = (0..n).map(|p| stack_value(values, p, side)).collect();
    if mirror {
        out.extend((0..n).map(|p| stack_value(values, p, 1 - side)));
    }
    out
}

/// A field image of a patient with the kind it was read from.
#[derive(Debug, Clone)]
pub struct FieldImage {
    pub volume: NiiVolume,
    pub binary: bool,
}

/// One patient's inputs for the pure analysis; side 0 = right, 1 = left. `flipped[s]` is the
/// field that landed on side `s` by flipping the other hemisphere's field.
#[derive(Debug, Clone)]
pub struct PatientInput {
    pub subject: String,
    pub group: u32,
    pub fields: [Option<FieldImage>; 2],
    pub flipped: [Option<FieldImage>; 2],
    pub amplitude: [f64; 2],
    pub outcome: Vec<f64>,
    pub covariates: Vec<Vec<f64>>,
}

/// Everything of one side the statistics need.
#[derive(Debug, Clone)]
pub struct SideData {
    pub grid: Grid,
    pub matrix: SideMatrix,
    pub coverage: Coverage,
    pub outcome: Vec<f64>,
    pub amp: Vec<f64>,
}

/// The prepared data of an analysis (`specs/14` §7.2, §7.3).
#[derive(Debug, Clone)]
pub struct Dataset {
    pub n: usize,
    pub mirror: bool,
    pub per_hemisphere: bool,
    pub binary_outcome: bool,
    pub subjects: Vec<String>,
    pub groups: Vec<u32>,
    pub sides: [SideData; 2],
}

impl Dataset {
    /// The matrix rows of `patients` (their flipped copies too when mirroring).
    pub fn rows_of(&self, patients: &[usize]) -> Vec<usize> {
        let mut rows: Vec<usize> = patients.to_vec();
        if self.mirror {
            rows.extend(patients.iter().map(|p| p + self.n));
        }
        rows
    }
}

/// Builds the grids, matrices and prepared outcomes of both sides.
pub fn build_dataset(
    patients: &[PatientInput],
    s: &SweetspotSettings,
    per_hemisphere: bool,
) -> Result<Dataset, GroupError> {
    let n = patients.len();
    let outcomes: Vec<Vec<f64>> = patients.iter().map(|p| p.outcome.clone()).collect();
    let ncov = patients.first().map_or(0, |p| p.covariates.len());
    let binary_outcome = {
        let mut any = false;
        let mut all01 = true;
        for v in outcomes.iter().flatten().filter(|v| v.is_finite()) {
            any = true;
            if *v != 0.0 && *v != 1.0 {
                all01 = false;
            }
        }
        any && all01
    };
    let build_side = |side: usize| -> Result<SideData, GroupError> {
        let mut images: Vec<Option<&FieldImage>> =
            patients.iter().map(|p| p.fields[side].as_ref()).collect();
        if s.mirror {
            images.extend(patients.iter().map(|p| p.flipped[side].as_ref()));
        }
        let cut: Vec<f32> = images
            .iter()
            .map(|i| match i {
                Some(f) if f.binary => BINARY_CUT,
                _ => s.efield_threshold as f32,
            })
            .collect();
        let grid_cuts: Vec<(&NiiVolume, f32)> = images
            .iter()
            .flatten()
            .map(|f| {
                (
                    &f.volume,
                    if f.binary {
                        BINARY_CUT
                    } else {
                        GRID_EFIELD_THRESHOLD
                    },
                )
            })
            .collect();
        let grid = common_grid(&grid_cuts, s.resolution).map_err(|e| {
            GroupError::Invalid(format!("side {}: {e}", if side == 0 { "R" } else { "L" }))
        })?;
        let vols: Vec<Option<&NiiVolume>> = images.iter().map(|i| i.map(|f| &f.volume)).collect();
        let matrix = SideMatrix::build(&vols, &cut, &grid);
        let coverage = Coverage::build(&matrix);
        let mut outcome = stack_outcome(&outcomes, side, s.mirror);
        outcome = match s.normalisation {
            Normalisation::None => outcome,
            Normalisation::ZScore => sc::zscore(&outcome),
            Normalisation::VanAlbada => sc::van_albada(&outcome),
        };
        if ncov > 0 {
            let covs: Vec<Vec<f64>> = (0..ncov)
                .map(|c| {
                    let vals: Vec<Vec<f64>> =
                        patients.iter().map(|p| p.covariates[c].clone()).collect();
                    stack_outcome(&vals, side, s.mirror)
                })
                .collect();
            outcome = sc::ols_residuals(&outcome, &covs);
        }
        let mut amp: Vec<f64> = patients.iter().map(|p| p.amplitude[side]).collect();
        if s.mirror {
            amp.extend(patients.iter().map(|p| p.amplitude[1 - side]));
        }
        Ok(SideData {
            grid,
            matrix,
            coverage,
            outcome,
            amp,
        })
    };
    let (r, l) = rayon::join(|| build_side(0), || build_side(1));
    Ok(Dataset {
        n,
        mirror: s.mirror,
        per_hemisphere,
        binary_outcome,
        subjects: patients.iter().map(|p| p.subject.clone()).collect(),
        groups: patients.iter().map(|p| p.group).collect(),
        sides: [r?, l?],
    })
}

/// A progress and cancel hook.
pub struct Ctl<'a> {
    pub cancel: &'a AtomicBool,
    pub progress: &'a (dyn Fn(&str) + Sync),
}

impl Ctl<'_> {
    fn check(&self) -> Result<(), GroupError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(GroupError::Lg {
                code: "cancelled",
                message: "the sweet-spot analysis was cancelled".to_owned(),
            })
        } else {
            Ok(())
        }
    }

    fn say(&self, text: &str) {
        (self.progress)(text);
    }
}

/// Train and test patients of every fold.
pub type Folds = Vec<(Vec<usize>, Vec<usize>)>;

/// The folds of a strategy (`specs/14` §7.5) over patients `0..n`; `groups` holds the cohort
/// number of each patient. Leave-nothing-out has one fold with train = test = all.
pub fn make_folds(
    strategy: CvStrategy,
    n: usize,
    groups: &[u32],
    k: usize,
    seed: u64,
) -> Result<Folds, GroupError> {
    let all: Vec<usize> = (0..n).collect();
    match strategy {
        CvStrategy::LeaveNothingOut => Ok(vec![(all.clone(), all)]),
        CvStrategy::LeaveOnePatientOut => Ok((0..n)
            .map(|p| (all.iter().copied().filter(|&q| q != p).collect(), vec![p]))
            .collect()),
        CvStrategy::LeaveOneCohortOut => {
            let mut numbers: Vec<u32> = groups.to_vec();
            numbers.sort_unstable();
            numbers.dedup();
            if numbers.len() < 2 {
                return Err(GroupError::Invalid(
                    "leave-one-cohort-out needs at least two groups".to_owned(),
                ));
            }
            Ok(numbers
                .iter()
                .map(|g| {
                    let test: Vec<usize> =
                        all.iter().copied().filter(|&p| groups[p] == *g).collect();
                    let train: Vec<usize> =
                        all.iter().copied().filter(|&p| groups[p] != *g).collect();
                    (train, test)
                })
                .collect())
        }
        CvStrategy::KFold => {
            if k < 2 || k > n {
                return Err(GroupError::Invalid(format!(
                    "k-fold needs 2 <= k <= number of patients ({n}), got {k}"
                )));
            }
            let mut order = all.clone();
            sc::Rng::new(seed).shuffle(&mut order);
            Ok((0..k)
                .map(|f| {
                    let test: Vec<usize> = order
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| i % k == f)
                        .map(|(_, p)| *p)
                        .collect();
                    let train: Vec<usize> = order
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| i % k != f)
                        .map(|(_, p)| *p)
                        .collect();
                    (train, test)
                })
                .collect())
        }
    }
}

/// A predicted / observed pair of the evaluation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sample {
    pub patient: usize,
    pub side: Option<char>,
    pub observed: f64,
    pub predicted: f64,
}

/// Result of the cross-validation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CvResult {
    pub strategy: CvStrategy,
    pub n_samples: usize,
    pub r: f64,
    pub p: f64,
    pub p_perm: Option<f64>,
    pub null_abs_r_p95: Option<f64>,
    pub auc: Option<f64>,
    pub samples: Vec<Sample>,
}

fn train_map(
    ds: &Dataset,
    side: usize,
    s: &SweetspotSettings,
    outcome: &[f64],
    patients: &[usize],
) -> Vec<f32> {
    let sd = &ds.sides[side];
    let rows = ds.rows_of(patients);
    let r = compute_map(
        &sd.matrix,
        &sd.coverage,
        outcome,
        &sd.amp,
        &rows,
        s,
        ds.binary_outcome,
    );
    apply_signs(&r.values, s.show_positive, s.show_negative)
}

fn predict_patient(ds: &Dataset, side: usize, s: &SweetspotSettings, map: &[f32], p: usize) -> f64 {
    let sd = &ds.sides[side];
    if sd.coverage.per_row[p] == 0 {
        return f64::NAN;
    }
    predict_row(
        map,
        sd.matrix.row(p),
        sd.matrix.cut[p],
        s.level,
        s.basis,
        s.show_positive,
        s.show_negative,
    )
}

fn fold_predict(
    ds: &Dataset,
    s: &SweetspotSettings,
    outcomes: [&[f64]; 2],
    train: &[usize],
    test: &[usize],
) -> Vec<[f64; 2]> {
    let (a, b) = rayon::join(
        || train_map(ds, 0, s, outcomes[0], train),
        || train_map(ds, 1, s, outcomes[1], train),
    );
    test.iter()
        .map(|&p| {
            [
                predict_patient(ds, 0, s, &a, p),
                predict_patient(ds, 1, s, &b, p),
            ]
        })
        .collect()
}

fn nan_mean2(a: f64, b: f64) -> f64 {
    match (a.is_finite(), b.is_finite()) {
        (true, true) => 0.5 * (a + b),
        (true, false) => a,
        (false, true) => b,
        _ => f64::NAN,
    }
}

fn make_samples(
    ds: &Dataset,
    outcomes: [&[f64]; 2],
    test: &[usize],
    preds: &[[f64; 2]],
) -> Vec<Sample> {
    let mut out = Vec::new();
    for (&p, pr) in test.iter().zip(preds) {
        if ds.per_hemisphere {
            for (side, letter) in [(0usize, 'R'), (1, 'L')] {
                out.push(Sample {
                    patient: p,
                    side: Some(letter),
                    observed: outcomes[side][p],
                    predicted: pr[side],
                });
            }
        } else {
            out.push(Sample {
                patient: p,
                side: None,
                observed: nan_mean2(outcomes[0][p], outcomes[1][p]),
                predicted: nan_mean2(pr[0], pr[1]),
            });
        }
    }
    out
}

/// Correlation of predicted and observed over the finite pairs, with its p and pair count.
pub fn evaluate(samples: &[Sample], kind: CorrelationType) -> (f64, f64, usize) {
    let pairs: Vec<(f64, f64)> = samples
        .iter()
        .filter(|s| s.observed.is_finite() && s.predicted.is_finite())
        .map(|s| (s.predicted, s.observed))
        .collect();
    let a: Vec<f64> = pairs.iter().map(|p| p.0).collect();
    let b: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    let r = match kind {
        CorrelationType::Spearman => sc::spearman(&a, &b),
        CorrelationType::Pearson => sc::pearson(&a, &b),
    };
    (r, sc::correlation_p(r, pairs.len()), pairs.len())
}

fn permuted_outcome(ds: &Dataset, side: usize, perm: &[usize]) -> Vec<f64> {
    let base = &ds.sides[side].outcome;
    let rows = base.len();
    (0..rows)
        .map(|r| {
            let (p, flip) = (r % ds.n, (r / ds.n) * ds.n);
            base[perm[p] + flip]
        })
        .collect()
}

/// Cross-validation and evaluation (`specs/14` §7.5).
pub fn cross_validate(
    ds: &Dataset,
    s: &SweetspotSettings,
    ctl: &Ctl<'_>,
) -> Result<CvResult, GroupError> {
    let folds = make_folds(s.cv, ds.n, &ds.groups, s.k_folds, s.seed)?;
    let outcomes = [&ds.sides[0].outcome[..], &ds.sides[1].outcome[..]];
    let mut samples = Vec::new();
    let mut done = 0usize;
    for (train, test) in &folds {
        ctl.check()?;
        let preds = fold_predict(ds, s, outcomes, train, test);
        samples.extend(make_samples(ds, outcomes, test, &preds));
        done += 1;
        ctl.say(&format!("cross-validation fold {done} of {}", folds.len()));
    }
    let (r, p, n_samples) = evaluate(&samples, s.evaluation_correlation);
    let (mut p_perm, mut p95) = (None, None);
    if s.cv == CvStrategy::LeaveNothingOut {
        ctl.say(&format!("{} permutations", s.permutations));
        let all: Vec<usize> = (0..ds.n).collect();
        let null: Vec<Option<f64>> = (0..s.permutations)
            .into_par_iter()
            .map(|k| {
                if ctl.cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let mut perm = all.clone();
                sc::Rng::stream(s.seed, k as u64 + 1).shuffle(&mut perm);
                let po = [
                    permuted_outcome(ds, 0, &perm),
                    permuted_outcome(ds, 1, &perm),
                ];
                let po_ref = [&po[0][..], &po[1][..]];
                let preds = fold_predict(ds, s, po_ref, &all, &all);
                let sm = make_samples(ds, po_ref, &all, &preds);
                Some(evaluate(&sm, s.evaluation_correlation).0)
            })
            .collect();
        ctl.check()?;
        let null: Vec<f64> = null
            .into_iter()
            .flatten()
            .filter(|v| v.is_finite())
            .collect();
        p_perm = Some(sc::permutation_p(r, &null));
        let abs: Vec<f64> = null.iter().map(|v| v.abs()).collect();
        p95 = Some(sc::quantile(&abs, 0.95));
    }
    let auc = if ds.binary_outcome {
        let pred: Vec<f64> = samples.iter().map(|s| s.predicted).collect();
        let obs: Vec<f64> = samples.iter().map(|s| s.observed).collect();
        let a = sc::logistic_auc(&pred, &obs);
        a.is_finite().then_some(a)
    } else {
        None
    };
    Ok(CvResult {
        strategy: s.cv,
        n_samples,
        r,
        p,
        p_perm,
        null_abs_r_p95: p95,
        auc,
        samples,
    })
}

/// One map of a side (a group map when the analysis is split by group).
#[derive(Debug, Clone)]
pub struct GroupMap {
    pub group: Option<u32>,
    pub values: Vec<f32>,
    pub p: Vec<f32>,
    pub coverage: Vec<f32>,
}

/// The linear model `outcome = beta0 + beta1 * predicted`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelFit {
    pub beta0: f64,
    pub beta1: f64,
    pub train_r: f64,
    pub train_n: usize,
}

/// The result of the statistics, cross-validation and model.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub maps: [Vec<GroupMap>; 2],
    pub cv: CvResult,
    pub model: ModelFit,
}

/// Runs maps, cross-validation and the model fit on a prepared dataset.
pub fn analyse(ds: &Dataset, s: &SweetspotSettings, ctl: &Ctl<'_>) -> Result<Analysis, GroupError> {
    let all: Vec<usize> = (0..ds.n).collect();
    let mut maps: [Vec<GroupMap>; 2] = [Vec::new(), Vec::new()];
    let mut group_numbers: Vec<u32> = ds.groups.clone();
    group_numbers.sort_unstable();
    group_numbers.dedup();
    for side in 0..2 {
        ctl.check()?;
        ctl.say(&format!("voxel statistics, side {}", ["R", "L"][side]));
        let sd = &ds.sides[side];
        let mut sets: Vec<(Option<u32>, Vec<usize>)> = vec![(None, all.clone())];
        if s.split_by_group {
            for g in &group_numbers {
                let members: Vec<usize> = all
                    .iter()
                    .copied()
                    .filter(|&p| ds.groups[p] == *g)
                    .collect();
                sets.push((Some(*g), members));
            }
        }
        for (group, patients) in sets {
            let rows = ds.rows_of(&patients);
            let r = compute_map(
                &sd.matrix,
                &sd.coverage,
                &sd.outcome,
                &sd.amp,
                &rows,
                s,
                ds.binary_outcome,
            );
            maps[side].push(GroupMap {
                group,
                values: r.values,
                p: r.p,
                coverage: r.coverage,
            });
        }
    }
    let cv = cross_validate(ds, s, ctl)?;
    ctl.check()?;
    let outcomes = [&ds.sides[0].outcome[..], &ds.sides[1].outcome[..]];
    let lno_samples = if s.cv == CvStrategy::LeaveNothingOut {
        cv.samples.clone()
    } else {
        let preds = fold_predict(ds, s, outcomes, &all, &all);
        make_samples(ds, outcomes, &all, &preds)
    };
    let (train_r, _, train_n) = evaluate(&lno_samples, s.evaluation_correlation);
    let pairs: Vec<&Sample> = lno_samples
        .iter()
        .filter(|x| x.observed.is_finite() && x.predicted.is_finite())
        .collect();
    let xs: Vec<f64> = pairs.iter().map(|x| x.predicted).collect();
    let ys: Vec<f64> = pairs.iter().map(|x| x.observed).collect();
    let (beta0, beta1) = sc::linear_regression(&xs, &ys).unwrap_or((f64::NAN, f64::NAN));
    Ok(Analysis {
        maps,
        cv,
        model: ModelFit {
            beta0,
            beta1,
            train_r,
            train_n,
        },
    })
}

/// A stored sweet-spot model (`specs/14` §7.6): the shown map of each side on its grid, the
/// settings and the linear model.
#[derive(Debug, Clone)]
pub struct SweetspotModel {
    pub settings: SweetspotSettings,
    pub grids: [Grid; 2],
    pub maps: [Vec<f32>; 2],
    pub fit: ModelFit,
    pub per_hemisphere: bool,
}

/// The prediction for a new patient.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub ihat: [f64; 2],
    pub outcome: [f64; 2],
}

impl SweetspotModel {
    /// Builds the model of an analysis (pooled maps).
    pub fn from_analysis(ds: &Dataset, s: &SweetspotSettings, a: &Analysis) -> SweetspotModel {
        let shown = |side: usize| {
            a.maps[side]
                .iter()
                .find(|m| m.group.is_none())
                .map(|m| apply_signs(&m.values, s.show_positive, s.show_negative))
                .unwrap_or_default()
        };
        SweetspotModel {
            settings: s.clone(),
            grids: [ds.sides[0].grid.clone(), ds.sides[1].grid.clone()],
            maps: [shown(0), shown(1)],
            fit: a.model.clone(),
            per_hemisphere: ds.per_hemisphere,
        }
    }

    /// Predicts the outcome of a patient from its fields (right, left). Each side's predicted
    /// score is NaN when the side has no field.
    pub fn predict(&self, fields: &[Option<FieldImage>; 2]) -> Prediction {
        let mut ihat = [f64::NAN; 2];
        for side in 0..2 {
            let Some(f) = &fields[side] else { continue };
            let mut row = vec![0f32; self.grids[side].len()];
            resample_nearest_into(&f.volume, &self.grids[side], &mut row);
            let cut = if f.binary {
                BINARY_CUT
            } else {
                self.settings.efield_threshold as f32
            };
            if !row.iter().any(|x| *x > cut) {
                continue;
            }
            ihat[side] = predict_row(
                &self.maps[side],
                &row,
                cut,
                self.settings.level,
                self.settings.basis,
                self.settings.show_positive,
                self.settings.show_negative,
            );
        }
        let lin = |x: f64| self.fit.beta0 + self.fit.beta1 * x;
        Prediction {
            ihat,
            outcome: [lin(ihat[0]), lin(ihat[1])],
        }
    }
}

fn gz_bytes(bytes: &[u8]) -> Result<Vec<u8>, GroupError> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes)
        .map_err(|e| GroupError::Io(e.to_string()))?;
    enc.finish().map_err(|e| GroupError::Io(e.to_string()))
}

fn map_volume(grid: &Grid, data: &[f32]) -> NiiVolume {
    NiiVolume {
        dims: grid.dims,
        affine: grid.affine(),
        data: data.to_vec(),
    }
}

fn write_map(path: &Path, grid: &Grid, data: &[f32]) -> Result<(), GroupError> {
    let bytes = gz_bytes(&encode_nii(&map_volume(grid, data)))?;
    std::fs::write(path, bytes).map_err(|e| GroupError::Io(format!("{}: {e}", path.display())))
}

fn check_dims(vol: &NiiVolume, path: &Path) -> Result<(), GroupError> {
    if vol.dims.iter().any(|d| *d > MAX_DIM) {
        return Err(GroupError::Invalid(format!(
            "{}: dims too large: {}x{}x{}",
            path.display(),
            vol.dims[0],
            vol.dims[1],
            vol.dims[2]
        )));
    }
    Ok(())
}

/// Options of [`run_sweetspot`].
#[derive(Debug, Clone)]
pub struct SweetspotOptions {
    pub group_folder: PathBuf,
    pub variable: String,
    pub covariates: Vec<String>,
    pub settings: SweetspotSettings,
    pub space: String,
    pub model: Option<String>,
    pub patients: Option<Vec<String>>,
}

/// What a sweet-spot run produced.
#[derive(Debug, Clone)]
pub struct SweetspotReport {
    pub id: String,
    pub folder: PathBuf,
    pub record_file: PathBuf,
    pub model_file: PathBuf,
    pub files: Vec<PathBuf>,
    pub n_patients: usize,
    pub excluded: Vec<(String, String)>,
    pub cv: CvResult,
    pub model: ModelFit,
    pub notes: Vec<Note>,
    pub summary: Value,
}

struct FoundField {
    path: PathBuf,
    model: String,
}

fn find_field(
    dir: &Path,
    sid: &str,
    kind: &str,
    side: usize,
    flipped_from: Option<usize>,
    model: Option<&str>,
) -> Result<Option<FoundField>, GroupError> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    let letter = ['R', 'L'][side];
    let tail = match flipped_from {
        None => format!("_hemi-{letter}"),
        Some(o) => format!(
            "_hemi-{letter}_hemidesc-FlippedFrom{}",
            ["Right", "Left"][o]
        ),
    };
    let prefix = format!("sub-{sid}_sim-{kind}_model-");
    let mut found: Vec<FoundField> = Vec::new();
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let stem = rest
            .strip_suffix(".nii.gz")
            .or_else(|| rest.strip_suffix(".nii"));
        let Some(stem) = stem else { continue };
        let Some(m) = stem.strip_suffix(&tail) else {
            continue;
        };
        if model.is_some_and(|want| want != m) {
            continue;
        }
        found.push(FoundField {
            path: dir.join(&name),
            model: m.to_owned(),
        });
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => {
            let models: Vec<String> = found.iter().map(|f| f.model.clone()).collect();
            Err(GroupError::Invalid(format!(
                "several stimulation models in {}: pass the model ({})",
                dir.display(),
                models.join(", ")
            )))
        }
    }
}

fn amplitude_of(record: &GroupRecord, p: &super::record::PatientEntry) -> [f64; 2] {
    let mut amp = [f64::NAN; 2];
    let Some(dir) = record.patient_folder(p) else {
        return amp;
    };
    let Ok(stim) = load_stimulation(&dir, subject_id(&p.subject), &record.stim_label()) else {
        return amp;
    };
    for (i, side) in [&stim.sides.right, &stim.sides.left]
        .into_iter()
        .enumerate()
    {
        if let Some(src) = side.sources.iter().find(|s| s.index == 1) {
            amp[i] = src.amplitude;
        }
    }
    amp
}

fn lg_err(code: &'static str, message: String) -> GroupError {
    GroupError::Lg { code, message }
}

fn unique_id(sweetspots: &Path, guid: &str) -> String {
    let mut id = guid.to_owned();
    let mut n = 0;
    while sweetspots.join(&id).exists() {
        n += 1;
        id = format!("{guid}_{n}");
    }
    id
}

#[allow(clippy::needless_range_loop)]
/// Reads the patients' files, runs the analysis and writes the outputs of `specs/14` §7.7 under
/// `<group folder>/sweetspots/<id>/`.
pub fn run_sweetspot(
    record: &GroupRecord,
    options: &SweetspotOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<SweetspotReport, GroupError> {
    let s = &options.settings;
    s.validate()?;
    let ctl = Ctl { cancel, progress };
    let mut notes = Vec::new();

    let var = record
        .clinical
        .iter()
        .find(|v| v.name == options.variable)
        .ok_or_else(|| {
            GroupError::Invalid(format!(
                "the group has no clinical variable {:?}",
                options.variable
            ))
        })?;
    let np = record.patients.len();
    if var.values.len() != np {
        return Err(GroupError::Invalid(format!(
            "clinical variable {:?} has {} rows for {np} patients",
            var.name,
            var.values.len()
        )));
    }
    if !matches!(
        var.kind,
        ClinicalKind::PerPatient | ClinicalKind::PerHemisphere
    ) || var.width() > 2
    {
        return Err(GroupError::Invalid(format!(
            "clinical variable {:?} is not per patient or per hemisphere",
            var.name
        )));
    }
    let per_hemisphere = var.width() == 2;
    let mut cov_vars = Vec::new();
    for name in &options.covariates {
        let c = record
            .clinical
            .iter()
            .find(|v| &v.name == name)
            .ok_or_else(|| GroupError::Invalid(format!("the group has no covariate {name:?}")))?;
        if c.values.len() != np || c.width() > 2 {
            return Err(GroupError::Invalid(format!(
                "covariate {name:?} does not fit the patient list"
            )));
        }
        cov_vars.push(c);
    }

    let label = record.stim_label();
    let wanted: Option<Vec<String>> = options
        .patients
        .as_ref()
        .map(|l| l.iter().map(|x| super::record::subject_name(x)).collect());
    let kind_order: &[&str] = match s.level {
        Level::Vtas => &["efield", "binary"],
        Level::EFields => &["efield"],
    };
    let mut inputs: Vec<PatientInput> = Vec::new();
    let mut files_used: Vec<(String, PathBuf)> = Vec::new();
    let mut excluded: Vec<(String, String)> = Vec::new();
    let mut no_efield: Vec<String> = Vec::new();
    let mut missing_flip: Vec<String> = Vec::new();

    for (index, p) in record.patients.iter().enumerate() {
        if let Some(w) = &wanted {
            if !w.contains(&p.subject) {
                continue;
            }
        } else if !p.included {
            continue;
        }
        ctl.check()?;
        let Some(folder) = record.patient_folder(p) else {
            excluded.push((p.subject.clone(), "dataset not found".to_owned()));
            continue;
        };
        let dir = folder
            .join("stimulations")
            .join(&options.space)
            .join(&label);
        let sid = subject_id(&p.subject);
        let mut fields: [Option<FieldImage>; 2] = [None, None];
        let mut flipped: [Option<FieldImage>; 2] = [None, None];
        let mut kinds: [Option<&str>; 2] = [None, None];
        let mut patient_files: Vec<PathBuf> = Vec::new();
        let mut originals: [Option<PathBuf>; 2] = [None, None];
        for side in 0..2 {
            for kind in kind_order {
                if let Some(f) = find_field(&dir, sid, kind, side, None, options.model.as_deref())?
                {
                    let vol = read_nii(&f.path).map_err(|e| GroupError::Io(e.to_string()))?;
                    check_dims(&vol, &f.path)?;
                    fields[side] = Some(FieldImage {
                        volume: vol,
                        binary: *kind == "binary",
                    });
                    kinds[side] = Some(kind);
                    originals[side] = Some(f.path.clone());
                    patient_files.push(f.path);
                    break;
                }
            }
        }
        if fields.iter().all(Option::is_none) {
            if s.level == Level::EFields {
                no_efield.push(p.subject.clone());
            } else {
                excluded.push((p.subject.clone(), "no VTA for the group label".to_owned()));
            }
            continue;
        }
        if s.level == Level::EFields && fields.iter().flatten().any(|f| f.binary) {
            no_efield.push(p.subject.clone());
            continue;
        }
        if s.mirror {
            for side in 0..2 {
                let from = 1 - side;
                let Some(kind) = kinds[from] else { continue };
                let found =
                    find_field(&dir, sid, kind, side, Some(from), options.model.as_deref())?;
                let current = found.filter(|f| {
                    originals[from]
                        .as_deref()
                        .is_some_and(|o| super::flip::flipped_is_current(o, &f.path))
                });
                match current {
                    Some(f) => {
                        let vol = read_nii(&f.path).map_err(|e| GroupError::Io(e.to_string()))?;
                        check_dims(&vol, &f.path)?;
                        flipped[side] = Some(FieldImage {
                            volume: vol,
                            binary: kind == "binary",
                        });
                        patient_files.push(f.path);
                    }
                    None => missing_flip.push(p.subject.clone()),
                }
            }
        }
        for f in patient_files {
            files_used.push((p.subject.clone(), f));
        }
        let take = |v: &super::clinical::ClinicalVar| -> Vec<f64> {
            v.values[index]
                .iter()
                .map(|x| x.unwrap_or(f64::NAN))
                .collect()
        };
        inputs.push(PatientInput {
            subject: p.subject.clone(),
            group: p.group,
            fields,
            flipped,
            amplitude: amplitude_of(record, p),
            outcome: take(var),
            covariates: cov_vars.iter().map(|c| take(c)).collect(),
        });
    }
    if !no_efield.is_empty() {
        return Err(lg_err(
            "E_SWEETSPOT_NO_EFIELD",
            format!(
                "level E-Fields needs E-field files; missing for: {}",
                no_efield.join(", ")
            ),
        ));
    }
    if !missing_flip.is_empty() {
        missing_flip.sort();
        missing_flip.dedup();
        return Err(GroupError::Invalid(format!(
            "mirroring needs the flipped files (specs/14 §4.4); missing or out of date for: {}; run group Calculate with mirroring again",
            missing_flip.join(", ")
        )));
    }
    let with_outcome = inputs
        .iter()
        .filter(|i| i.outcome.iter().any(|x| x.is_finite()))
        .count();
    if with_outcome < 3 {
        return Err(lg_err(
            "E_GROUP_TOO_FEW",
            format!(
                "only {with_outcome} patients with files and an outcome; at least 3 are needed"
            ),
        ));
    }
    for (subject, reason) in &excluded {
        notes.push(Note::plain(format!("{subject} excluded: {reason}")));
    }
    if s.amplitude_correction != AmplitudeCorrection::None
        && inputs
            .iter()
            .any(|i| i.amplitude.iter().any(|a| !a.is_finite()))
    {
        notes.push(Note::plain(
            "some patients have no source-1 amplitude; the amplitude correction leaves them out",
        ));
    }

    ctl.say(&format!("{} patients; building the grids", inputs.len()));
    let ds = build_dataset(&inputs, s, per_hemisphere)?;
    let analysis = analyse(&ds, s, &ctl)?;
    ctl.check()?;
    let model = SweetspotModel::from_analysis(&ds, s, &analysis);

    let sweetspots = options.group_folder.join("sweetspots");
    std::fs::create_dir_all(&sweetspots)
        .map_err(|e| GroupError::Io(format!("{}: {e}", sweetspots.display())))?;
    let id = unique_id(&sweetspots, &record.guid);
    let final_dir = sweetspots.join(&id);
    let staging = sweetspots.join(format!(".partial-{id}-{}", std::process::id()));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(&staging)
        .map_err(|e| GroupError::Io(format!("{}: {e}", staging.display())))?;
    let written = write_outputs(
        record,
        options,
        &id,
        &staging,
        &ds,
        &analysis,
        &model,
        &inputs,
        &files_used,
        &excluded,
        &notes,
    );
    let (names, record_json) = match written {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    std::fs::rename(&staging, &final_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        GroupError::Io(format!("{}: {e}", final_dir.display()))
    })?;
    let files: Vec<PathBuf> = names.iter().map(|n| final_dir.join(n)).collect();
    let summary = json!({
        "id": id,
        "folder": final_dir,
        "patients": inputs.len(),
        "excluded": excluded.iter().map(|(a, b)| json!({"subject": a, "reason": b})).collect::<Vec<_>>(),
        "cross_validation": record_json["cross_validation"].clone(),
        "model": record_json["model"].clone(),
        "files": names,
    });
    Ok(SweetspotReport {
        id,
        record_file: final_dir.join("sweetspot.json"),
        model_file: final_dir.join("model.json"),
        folder: final_dir,
        files,
        n_patients: inputs.len(),
        excluded,
        cv: analysis.cv,
        model: analysis.model,
        notes,
        summary,
    })
}

fn grid_json(g: &Grid) -> Value {
    json!({"dims": g.dims, "origin": g.origin, "resolution": g.resolution})
}

#[allow(clippy::too_many_arguments)]
fn write_outputs(
    record: &GroupRecord,
    options: &SweetspotOptions,
    id: &str,
    dir: &Path,
    ds: &Dataset,
    a: &Analysis,
    model: &SweetspotModel,
    inputs: &[PatientInput],
    files_used: &[(String, PathBuf)],
    excluded: &[(String, String)],
    notes: &[Note],
) -> Result<(Vec<String>, Value), GroupError> {
    let s = &options.settings;
    let io = |p: &Path, e: std::io::Error| GroupError::Io(format!("{}: {e}", p.display()));
    let mut names: Vec<String> = Vec::new();
    let stem = format!(
        "dataset-{}_analysis-{}_sweetspot-{id}",
        record.dataset_name, record.guid
    );
    let mut map_files = [String::new(), String::new()];
    for side in 0..2 {
        let letter = ['R', 'L'][side];
        for m in &a.maps[side] {
            let group = m.group.map_or(String::new(), |g| format!("_group-{g}"));
            let base = format!("{stem}{group}_hemi-{letter}_stat-{}", s.test.file_slug());
            let shown = apply_signs(&m.values, s.show_positive, s.show_negative);
            let map_name = format!("{base}_desc-{}_map.nii.gz", s.desc());
            write_map(&dir.join(&map_name), &ds.sides[side].grid, &shown)?;
            let cov_name = format!("{base}_desc-coverage_map.nii.gz");
            write_map(&dir.join(&cov_name), &ds.sides[side].grid, &m.coverage)?;
            if m.group.is_none() {
                map_files[side] = map_name.clone();
            }
            names.push(map_name);
            names.push(cov_name);
        }
    }
    let model_json = json!({
        "schema": "dbsloc-sweetspot-model/1",
        "settings": s,
        "per_hemisphere": model.per_hemisphere,
        "grids": {"R": grid_json(&model.grids[0]), "L": grid_json(&model.grids[1])},
        "beta0": model.fit.beta0,
        "beta1": model.fit.beta1,
        "train_r": model.fit.train_r,
        "train_n": model.fit.train_n,
        "maps": {"R": map_files[0], "L": map_files[1]},
        "software": crate::version::build_info(),
        "warning": "Research estimate only.",
    });
    let model_path = dir.join("model.json");
    std::fs::write(&model_path, pretty(&model_json)).map_err(|e| io(&model_path, e))?;
    names.push("model.json".to_owned());

    let mut file_hashes = serde_json::Map::new();
    for (subject, path) in files_used {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let sha = file_sha256_hex(path).map_err(|e| GroupError::Io(e.to_string()))?;
        file_hashes.insert(format!("{subject}/{name}"), json!(sha));
    }
    for p in inputs {
        if let Some(pe) = record.patients.iter().find(|x| x.subject == p.subject) {
            if let Some(folder) = record.patient_folder(pe) {
                let reco = folder
                    .join("reconstruction")
                    .join(format!("{}_desc-reconstruction.json", p.subject));
                if let Ok(sha) = file_sha256_hex(&reco) {
                    file_hashes.insert(
                        format!("{}/{}_desc-reconstruction.json", p.subject, p.subject),
                        json!(sha),
                    );
                }
            }
        }
    }
    let outcome_values: Vec<Value> = inputs
        .iter()
        .map(|p| json!({"subject": p.subject, "outcome": p.outcome, "covariates": p.covariates}))
        .collect();
    let clinical_sha = sha256_hex(pretty(&Value::Array(outcome_values)).as_bytes());
    let settings_json = serde_json::to_value(s).unwrap_or(Value::Null);
    let selection: Vec<&str> = inputs.iter().map(|p| p.subject.as_str()).collect();
    let fingerprint = sha256_hex(
        pretty(&json!({
            "files": file_hashes,
            "clinical": clinical_sha,
            "settings": settings_json,
            "selection": selection,
            "space": options.space,
        }))
        .as_bytes(),
    );
    let cv = &a.cv;
    let record_json = json!({
        "schema": "dbsloc-sweetspot/1",
        "id": id,
        "group_guid": record.guid,
        "dataset_name": record.dataset_name,
        "software": crate::version::build_info(),
        "space": options.space,
        "outcome": {
            "variable": options.variable,
            "covariates": options.covariates,
            "per_hemisphere": ds.per_hemisphere,
            "binary": ds.binary_outcome,
        },
        "settings": settings_json,
        "selection": {
            "patients": selection,
            "excluded": excluded.iter().map(|(a, b)| json!({"subject": a, "reason": b})).collect::<Vec<_>>(),
        },
        "inputs": {
            "files": file_hashes,
            "clinical_sha256": clinical_sha,
            "fingerprint": fingerprint,
        },
        "grids": {"R": grid_json(&ds.sides[0].grid), "L": grid_json(&ds.sides[1].grid)},
        "cross_validation": {
            "strategy": cv.strategy.name(),
            "n": cv.n_samples,
            "r": cv.r,
            "p": cv.p,
            "p_perm": cv.p_perm,
            "null_abs_r_p95": cv.null_abs_r_p95,
            "auc": cv.auc,
            "samples": cv.samples.iter().map(|x| json!({
                "subject": ds.subjects[x.patient],
                "side": x.side.map(|c| c.to_string()),
                "observed": x.observed,
                "predicted": x.predicted,
            })).collect::<Vec<_>>(),
        },
        "model": {
            "file": "model.json",
            "beta0": a.model.beta0,
            "beta1": a.model.beta1,
            "train_r": a.model.train_r,
            "train_n": a.model.train_n,
        },
        "files": names,
        "warnings": notes.iter().map(ToString::to_string).collect::<Vec<_>>(),
    });
    let rec_path = dir.join("sweetspot.json");
    std::fs::write(&rec_path, pretty(&record_json)).map_err(|e| io(&rec_path, e))?;
    names.push("sweetspot.json".to_owned());
    Ok((names, record_json))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v)
        .map(|t| t + "\n")
        .unwrap_or_default()
}

/// Loads a stored model from a `sweetspots/<id>/` folder.
pub fn load_model(dir: &Path) -> Result<SweetspotModel, GroupError> {
    let path = dir.join("model.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| GroupError::Io(format!("{}: {e}", path.display())))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| GroupError::Json(format!("{}: {e}", path.display())))?;
    let settings: SweetspotSettings = serde_json::from_value(v["settings"].clone())
        .map_err(|e| GroupError::Json(format!("{}: settings: {e}", path.display())))?;
    let grid = |k: &str| -> Result<Grid, GroupError> {
        serde_json::from_value(v["grids"][k].clone())
            .map_err(|e| GroupError::Json(format!("{}: grid {k}: {e}", path.display())))
    };
    let map = |k: &str| -> Result<Vec<f32>, GroupError> {
        let name = v["maps"][k].as_str().unwrap_or_default();
        let vol = read_nii(&dir.join(name)).map_err(|e| GroupError::Io(e.to_string()))?;
        Ok(vol.data)
    };
    let num = |k: &str| v[k].as_f64().unwrap_or(f64::NAN);
    Ok(SweetspotModel {
        settings,
        grids: [grid("R")?, grid("L")?],
        maps: [map("R")?, map("L")?],
        fit: ModelFit {
            beta0: num("beta0"),
            beta1: num("beta1"),
            train_r: num("train_r"),
            train_n: v["train_n"].as_u64().unwrap_or(0) as usize,
        },
        per_hemisphere: v["per_hemisphere"].as_bool().unwrap_or(false),
    })
}
