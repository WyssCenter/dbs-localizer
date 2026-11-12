// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Resolution of the SyN registration grid (`specs/04` §5.1 step 2, §5.4): an explicit choice is
//! used as is; the default is the template's own voxel size, stepped to the finest coarser grid
//! the graphics card can fit when the registration runs on the GPU.

use std::path::Path;

pub use dbsloc_protocol::NormGrid;
use fastwarp_gpu::CapacityCaps;

use crate::options::{RunOptions, DEFAULT_NORM_ISOTROPIC_MM};
use crate::paths::py_float;

/// Where the effective grid came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormGridSource {
    /// `--norm-isotropic-mm` / the GUI picker / the API field.
    Explicit,
    /// The template's own voxel size (nothing was chosen).
    Template,
    /// The template's voxel size did not fit the GPU; the finest coarser grid that fits.
    Capacity,
}

impl NormGridSource {
    pub fn as_str(self) -> &'static str {
        match self {
            NormGridSource::Explicit => "explicit",
            NormGridSource::Template => "template",
            NormGridSource::Capacity => "capacity",
        }
    }
}

/// The grid `normalize` registers on: `mm` is `None` for the full template grid.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedNormGrid {
    pub mm: Option<f64>,
    pub source: NormGridSource,
    /// The template's own voxel size the default started from (`Template` and `Capacity`).
    pub template_mm: Option<f64>,
    /// The capacity error of the template's grid when the default was stepped.
    pub reason: Option<String>,
    /// One line on the step, for the plan warnings, the run log and the method log.
    pub note: Option<String>,
}

impl ResolvedNormGrid {
    fn explicit(mm: Option<f64>) -> Self {
        ResolvedNormGrid {
            mm,
            source: NormGridSource::Explicit,
            template_mm: None,
            reason: None,
            note: None,
        }
    }

    fn template(mm: f64) -> Self {
        ResolvedNormGrid {
            mm: Some(mm),
            source: NormGridSource::Template,
            template_mm: Some(mm),
            reason: None,
            note: None,
        }
    }
}

/// The template's finest voxel edge rounded to 0.01 mm, the grid the default registers on.
pub fn template_default_mm(template: &Path) -> Option<f64> {
    crate::resample::template_voxel_mm(template).map(round_mm)
}

fn round_mm(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The grids tried when the template's voxel size does not fit the GPU: the voxel size itself,
/// then 0.1 mm steps up to 1.0 mm, then 1.5, 2.0, 2.5 and 3.0 mm.
pub fn step_candidates(voxel: f64) -> Vec<f64> {
    let mut out = vec![voxel];
    let mut tenths = (voxel * 10.0 + 1e-9).floor() as i64 + 1;
    while tenths <= 10 {
        let mm = tenths as f64 / 10.0;
        if mm > voxel + 1e-9 {
            out.push(mm);
        }
        tenths += 1;
    }
    for mm in [1.5, 2.0, 2.5, 3.0] {
        if mm > voxel + 1e-9 {
            out.push(mm);
        }
    }
    out
}

/// Resolves `grid` for `template` with `channels` SyN channels. `caps` are the device limits when
/// the registration runs on the GPU (`None` on ANTs: no capacity model, the default is the
/// template's voxel size). A template that cannot be read keeps
/// [`DEFAULT_NORM_ISOTROPIC_MM`]; the stage reports the unreadable template itself.
pub fn resolve_for_template(
    grid: NormGrid,
    template: Option<&Path>,
    channels: usize,
    caps: Option<&CapacityCaps>,
) -> ResolvedNormGrid {
    match grid {
        NormGrid::Mm(mm) => return ResolvedNormGrid::explicit(Some(mm)),
        NormGrid::Full => return ResolvedNormGrid::explicit(None),
        NormGrid::Template => {}
    }
    let Some(template) = template.filter(|t| t.is_file()) else {
        return ResolvedNormGrid::template(DEFAULT_NORM_ISOTROPIC_MM);
    };
    let Ok(header) = fastwarp_reg::nifti_io::read_grid(template) else {
        return ResolvedNormGrid::template(DEFAULT_NORM_ISOTROPIC_MM);
    };
    let voxel = header.spacing().into_iter().fold(f64::INFINITY, f64::min);
    if !(voxel.is_finite() && voxel > 0.0) {
        return ResolvedNormGrid::template(DEFAULT_NORM_ISOTROPIC_MM);
    }
    let voxel = round_mm(voxel);
    let Some(caps) = caps else {
        return ResolvedNormGrid::template(voxel);
    };
    let dims_at = |mm: f64| crate::resample::resampled_grid(header.size(), header.spacing(), mm).0;
    let first = match fastwarp_gpu::plan_capacity(dims_at(voxel), channels, caps) {
        Ok(_) => return ResolvedNormGrid::template(voxel),
        Err(e) => e,
    };
    for mm in step_candidates(voxel).into_iter().skip(1) {
        if fastwarp_gpu::plan_capacity(dims_at(mm), channels, caps).is_ok() {
            let reason = first.to_string();
            return ResolvedNormGrid {
                mm: Some(mm),
                source: NormGridSource::Capacity,
                template_mm: Some(voxel),
                note: Some(format!(
                    "registration grid stepped from the template's {} mm to {} mm: the GPU cannot fit the {} mm grid ({reason})",
                    py_float(voxel),
                    py_float(mm),
                    py_float(voxel),
                )),
                reason: Some(reason),
            };
        }
    }
    ResolvedNormGrid::template(voxel)
}

/// The remedy appended to the plan warning and the run log when the default was stepped.
pub const STEP_REMEDY: &str = "choose --norm-isotropic-mm or --reg-backend ants to override";

/// [`resolve_for_template`] on the options' template.
pub fn resolve(
    opts: &RunOptions,
    channels: usize,
    caps: Option<&CapacityCaps>,
) -> ResolvedNormGrid {
    resolve_for_template(
        opts.norm_isotropic_mm,
        opts.normalization_template.as_deref(),
        channels,
        caps,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn template(name: &str, spacing: [f32; 3], dims: [u16; 3]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dbsloc_normgrid_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t1.nii.gz");
        let hdr = nifti::NiftiHeader {
            dim: [3, dims[0], dims[1], dims[2], 1, 1, 1, 1],
            pixdim: [1.0, spacing[0], spacing[1], spacing[2], 0.0, 0.0, 0.0, 0.0],
            sform_code: 1,
            srow_x: [spacing[0], 0.0, 0.0, 0.0],
            srow_y: [0.0, spacing[1], 0.0, 0.0],
            srow_z: [0.0, 0.0, spacing[2], 0.0],
            scl_slope: 1.0,
            ..nifti::NiftiHeader::default()
        };
        nifti::writer::WriterOptions::new(&p)
            .reference_header(&hdr)
            .write_nifti(&ndarray::Array3::<f32>::zeros((
                dims[0] as usize,
                dims[1] as usize,
                dims[2] as usize,
            )))
            .unwrap();
        p
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn candidates_step_by_a_tenth_up_to_one_millimetre_then_coarser() {
        assert_eq!(
            step_candidates(0.5),
            vec![0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.5, 2.0, 2.5, 3.0]
        );
        assert_eq!(step_candidates(1.0), vec![1.0, 1.5, 2.0, 2.5, 3.0]);
        assert_eq!(
            step_candidates(0.75),
            vec![0.75, 0.8, 0.9, 1.0, 1.5, 2.0, 2.5, 3.0]
        );
        assert_eq!(step_candidates(2.0), vec![2.0, 2.5, 3.0]);
        assert_eq!(step_candidates(3.0), vec![3.0]);
    }

    #[test]
    fn explicit_choices_are_never_changed() {
        let caps = CapacityCaps {
            max_storage_buffer_binding_size: 1,
            memory_budget: 1,
        };
        let t = template("explicit", [0.5; 3], [8, 8, 8]);
        let r = resolve_for_template(NormGrid::Mm(0.5), Some(&t), 2, Some(&caps));
        assert_eq!(r, ResolvedNormGrid::explicit(Some(0.5)));
        let r = resolve_for_template(NormGrid::Full, Some(&t), 2, Some(&caps));
        assert_eq!(r, ResolvedNormGrid::explicit(None));
        cleanup(&t);
    }

    #[test]
    fn the_default_is_the_template_voxel_size() {
        let one = template("one", [1.0; 3], [8, 6, 4]);
        assert_eq!(
            resolve_for_template(NormGrid::Template, Some(&one), 1, None),
            ResolvedNormGrid::template(1.0)
        );
        let half = template("half", [0.5; 3], [8, 6, 4]);
        assert_eq!(
            resolve_for_template(NormGrid::Template, Some(&half), 1, None),
            ResolvedNormGrid::template(0.5)
        );
        let aniso = template("aniso", [0.7, 0.7, 1.2], [8, 6, 4]);
        assert_eq!(
            resolve_for_template(NormGrid::Template, Some(&aniso), 1, None),
            ResolvedNormGrid::template(0.7)
        );
        assert_eq!(template_default_mm(&half), Some(0.5));
        cleanup(&one);
        cleanup(&half);
        cleanup(&aniso);
    }

    #[test]
    fn an_unreadable_template_keeps_the_one_millimetre_fallback() {
        let missing = Path::new("/nonexistent/t1.nii.gz");
        assert_eq!(
            resolve_for_template(NormGrid::Template, Some(missing), 1, None),
            ResolvedNormGrid::template(DEFAULT_NORM_ISOTROPIC_MM)
        );
        assert_eq!(
            resolve_for_template(NormGrid::Template, None, 1, None),
            ResolvedNormGrid::template(DEFAULT_NORM_ISOTROPIC_MM)
        );
    }

    #[test]
    fn the_default_steps_to_the_finest_grid_the_device_fits() {
        let t = template("step", [0.5; 3], [40, 40, 40]);
        let fits_all = CapacityCaps {
            max_storage_buffer_binding_size: u64::MAX,
            memory_budget: u64::MAX,
        };
        assert_eq!(
            resolve_for_template(NormGrid::Template, Some(&t), 2, Some(&fits_all)),
            ResolvedNormGrid::template(0.5)
        );
        let voxels = |mm: f64| {
            crate::resample::resampled_grid([40, 40, 40], [0.5; 3], mm)
                .0
                .iter()
                .product::<usize>() as u64
        };
        let tight = CapacityCaps {
            max_storage_buffer_binding_size: voxels(0.8) * 12,
            memory_budget: u64::MAX,
        };
        let r = resolve_for_template(NormGrid::Template, Some(&t), 2, Some(&tight));
        assert_eq!(r.mm, Some(0.8));
        assert_eq!(r.source, NormGridSource::Capacity);
        assert_eq!(r.template_mm, Some(0.5));
        assert!(r.reason.as_deref().unwrap().contains("binds at most"));
        let note = r.note.unwrap();
        assert!(
            note.starts_with("registration grid stepped from the template's 0.5 mm to 0.8 mm")
                && note.contains("binds at most"),
            "{note}"
        );
        let coarse = CapacityCaps {
            max_storage_buffer_binding_size: u64::MAX,
            memory_budget: voxels(1.5) * 188,
        };
        let r = resolve_for_template(NormGrid::Template, Some(&t), 2, Some(&coarse));
        assert_eq!((r.mm, r.source), (Some(1.5), NormGridSource::Capacity));
        let nothing = CapacityCaps {
            max_storage_buffer_binding_size: 1,
            memory_budget: 1,
        };
        assert_eq!(
            resolve_for_template(NormGrid::Template, Some(&t), 2, Some(&nothing)),
            ResolvedNormGrid::template(0.5)
        );
        cleanup(&t);
    }

    #[test]
    fn resolve_reads_the_options_template() {
        let t = template("opts", [0.5; 3], [8, 8, 8]);
        let mut o = RunOptions::new("/nonexistent", "1");
        o.normalization_template = Some(t.clone());
        assert_eq!(resolve(&o, 1, None).mm, Some(0.5));
        o.norm_isotropic_mm = NormGrid::Mm(2.0);
        assert_eq!(resolve(&o, 1, None).mm, Some(2.0));
        cleanup(&t);
    }
}
