// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The SyN stage of the cohort `antsRegistration` recipe as data.
//!
//! [`SynRecipe`] holds the parameters of the `--transform SyN[...]` stage that the
//! cohort pipeline passes to ANTs, so the in-process engine runs exactly the same recipe.
//! [`SynRecipe::to_ants_args`] renders the stage back to the argument strings; a test can compare them with the
//! argument list a caller builds.

use crate::error::{invalid, Result};
use crate::pyramid::SigmaUnits;

/// Gradient step of `SyN[0.1,3,0]`: the largest voxel step of one update.
pub const COHORT_GRADIENT_STEP: f64 = 0.1;
/// Update-field variance of `SyN[0.1,3,0]`, in squared voxels of the level grid.
pub const COHORT_UPDATE_FIELD_VARIANCE: f64 = 3.0;
/// Total-field variance of `SyN[0.1,3,0]`, in squared voxels of the level grid.
pub const COHORT_TOTAL_FIELD_VARIANCE: f64 = 0.0;
/// Radius of `CC[...,radius]`.
pub const COHORT_CC_RADIUS: usize = 4;
/// Metric weight of each channel of the multispectral recipe.
pub const COHORT_MULTISPECTRAL_WEIGHT: f64 = 0.5;

/// Parameters of one `antsRegistration` SyN stage.
#[derive(Clone, Debug, PartialEq)]
pub struct SynRecipe {
    /// First `SyN[...]` argument: the learning rate of `ScaleUpdateField`.
    pub gradient_step: f64,
    /// Second argument: Gaussian variance of the update field (squared voxels).
    pub update_field_variance: f64,
    /// Third argument: Gaussian variance of the total field (squared voxels, 0 = none).
    pub total_field_variance: f64,
    /// `CC[...,weight,radius]` radius.
    pub cc_radius: usize,
    /// One `CC` weight per channel (ignored by ITK when there is a single metric).
    pub channel_weights: Vec<f64>,
    /// `--convergence [iterations,threshold,window]`: iterations per level.
    pub iterations: Vec<u32>,
    /// Convergence threshold.
    pub convergence_threshold: f64,
    /// Convergence window size.
    pub convergence_window: usize,
    /// `--shrink-factors`.
    pub shrink_factors: Vec<u32>,
    /// `--smoothing-sigmas`.
    pub smoothing_sigmas: Vec<f64>,
    /// Unit suffix of the sigmas (`vox` or `mm`).
    pub sigma_units: SigmaUnits,
}

impl SynRecipe {
    /// The cohort T1-only stage: `SyN[0.1,3,0]`, `CC[...,1,4]`, `[100x70x50x20,1e-6,10]`,
    /// `8x4x2x1`, `3x2x1x0vox`.
    pub fn cohort() -> Self {
        Self {
            gradient_step: COHORT_GRADIENT_STEP,
            update_field_variance: COHORT_UPDATE_FIELD_VARIANCE,
            total_field_variance: COHORT_TOTAL_FIELD_VARIANCE,
            cc_radius: COHORT_CC_RADIUS,
            channel_weights: vec![1.0],
            iterations: vec![100, 70, 50, 20],
            convergence_threshold: 1e-6,
            convergence_window: 10,
            shrink_factors: vec![8, 4, 2, 1],
            smoothing_sigmas: vec![3.0, 2.0, 1.0, 0.0],
            sigma_units: SigmaUnits::Voxels,
        }
    }

    /// The cohort T1w + T2w stage: the same, with two `CC` metrics of weight 0.5.
    pub fn cohort_multispectral() -> Self {
        Self {
            channel_weights: vec![COHORT_MULTISPECTRAL_WEIGHT; 2],
            ..Self::cohort()
        }
    }

    /// Number of resolution levels.
    pub fn levels(&self) -> usize {
        self.iterations.len()
    }

    /// Checks that the per-level vectors agree and every value is usable.
    pub fn validate(&self) -> Result<()> {
        let n = self.iterations.len();
        if n == 0 {
            return Err(invalid("syn recipe", "no levels"));
        }
        if self.shrink_factors.len() != n || self.smoothing_sigmas.len() != n {
            return Err(invalid(
                "syn recipe",
                "iterations, shrink factors and smoothing sigmas must have one entry per level",
            ));
        }
        if self.shrink_factors.contains(&0) {
            return Err(invalid("syn recipe", "shrink factor 0"));
        }
        if self.channel_weights.is_empty()
            || self
                .channel_weights
                .iter()
                .any(|w| !w.is_finite() || *w < 0.0)
            || self.channel_weights.iter().sum::<f64>() <= f64::EPSILON
        {
            return Err(invalid(
                "syn recipe",
                "channel weights must be non-negative with a positive sum",
            ));
        }
        if !(self.gradient_step.is_finite() && self.gradient_step > 0.0) {
            return Err(invalid("syn recipe", "gradient step must be positive"));
        }
        if [self.update_field_variance, self.total_field_variance]
            .iter()
            .chain(&self.smoothing_sigmas)
            .any(|v| !(v.is_finite() && *v >= 0.0))
        {
            return Err(invalid(
                "syn recipe",
                "field variances and smoothing sigmas must be finite and non-negative",
            ));
        }
        if !self.convergence_threshold.is_finite() {
            return Err(invalid(
                "syn recipe",
                "convergence threshold must be finite",
            ));
        }
        if self.convergence_window < 2 {
            return Err(invalid(
                "syn recipe",
                "convergence window must be at least 2",
            ));
        }
        if self.cc_radius == 0 {
            return Err(invalid("syn recipe", "CC radius must be positive"));
        }
        Ok(())
    }

    /// The argument strings of the stage as `antsRegistration` takes them, from
    /// `--transform` through `--smoothing-sigmas`; `channels` are `(fixed, moving)` image
    /// paths, one per weight.
    pub fn to_ants_args(&self, channels: &[(&str, &str)]) -> Vec<String> {
        let mut v = vec![
            "--transform".to_string(),
            format!(
                "SyN[{},{},{}]",
                self.gradient_step, self.update_field_variance, self.total_field_variance
            ),
        ];
        for ((fixed, moving), w) in channels.iter().zip(&self.channel_weights) {
            v.push("--metric".to_string());
            v.push(format!("CC[{fixed},{moving},{w},{}]", self.cc_radius));
        }
        v.push("--convergence".to_string());
        v.push(format!(
            "[{},{:e},{}]",
            join_x(&self.iterations),
            self.convergence_threshold,
            self.convergence_window
        ));
        v.push("--shrink-factors".to_string());
        v.push(join_x(&self.shrink_factors));
        v.push("--smoothing-sigmas".to_string());
        let unit = match self.sigma_units {
            SigmaUnits::Voxels => "vox",
            SigmaUnits::Millimetres => "mm",
        };
        v.push(format!("{}{unit}", join_x(&self.smoothing_sigmas)));
        v
    }
}

fn join_x<T: std::fmt::Display>(values: &[T]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("x")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cohort_args_match_the_pipeline_builder_text() {
        let args = SynRecipe::cohort().to_ants_args(&[("/t.nii.gz", "/m.nii.gz")]);
        let want: Vec<String> = [
            "--transform",
            "SyN[0.1,3,0]",
            "--metric",
            "CC[/t.nii.gz,/m.nii.gz,1,4]",
            "--convergence",
            "[100x70x50x20,1e-6,10]",
            "--shrink-factors",
            "8x4x2x1",
            "--smoothing-sigmas",
            "3x2x1x0vox",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(args, want);
    }

    #[test]
    fn multispectral_args_have_two_half_weight_metrics() {
        let args = SynRecipe::cohort_multispectral()
            .to_ants_args(&[("/t.nii.gz", "/m.nii.gz"), ("/t2.nii.gz", "/m2.nii.gz")]);
        assert_eq!(args[2], "--metric");
        assert_eq!(args[3], "CC[/t.nii.gz,/m.nii.gz,0.5,4]");
        assert_eq!(args[4], "--metric");
        assert_eq!(args[5], "CC[/t2.nii.gz,/m2.nii.gz,0.5,4]");
        assert_eq!(args[6], "--convergence");
        assert_eq!(args.len(), 12);
    }

    #[test]
    fn validation_catches_inconsistent_recipes() {
        assert!(SynRecipe::cohort().validate().is_ok());
        let mut r = SynRecipe::cohort();
        r.shrink_factors.pop();
        assert!(r.validate().is_err());
        let mut r = SynRecipe::cohort();
        r.channel_weights.clear();
        assert!(r.validate().is_err());
        let mut r = SynRecipe::cohort();
        r.convergence_window = 1;
        assert!(r.validate().is_err());
        let mut r = SynRecipe::cohort();
        r.update_field_variance = -1.0;
        assert!(r.validate().is_err());
        let mut r = SynRecipe::cohort();
        r.total_field_variance = f64::NAN;
        assert!(r.validate().is_err());
        let mut r = SynRecipe::cohort();
        r.smoothing_sigmas[1] = f64::INFINITY;
        assert!(r.validate().is_err());
        let mut r = SynRecipe::cohort();
        r.convergence_threshold = f64::NAN;
        assert!(r.validate().is_err());
        let mut r = SynRecipe::cohort();
        r.convergence_threshold = -1.0;
        assert!(r.validate().is_ok());
        let mut r = SynRecipe::cohort();
        r.iterations.clear();
        r.shrink_factors.clear();
        r.smoothing_sigmas.clear();
        assert!(r.validate().is_err());
    }
}
