// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! High-level PaCER pipeline. `specs/10_pacer.md` §2–3, §8.

use std::sync::Arc;

use ndarray::Array3;

use super::catalog;
use super::errors::PacerError;
use super::mask::{build_brain_mask, erode_provided_mask};
use super::metal::find_electrode_clouds;
use super::oor::run_oor;
use super::peaks::{contact_area_fallback, filtfilt_box, find_peaks_mm};
use super::poly3::{fit_param_poly_to_skeleton, inv_poly_arc_length3, polyval3, PolyR3};
use super::skeleton_pass1::{first_pass_poly, pick_stack_axis};
use super::volume::CtVolume;

const Z_RES: f64 = 0.025;
const TIP_DEPTH_MM: f32 = 20.0;

/// Default for [`PacerConfig::distal_bloom_mm`].
pub const PACER_DISTAL_BLOOM_MM: f64 = 0.5;

/// CLI / API options.
#[derive(Clone, Debug)]
pub struct PacerConfig {
    pub no_mask: bool,
    pub reverse_dir: bool,
    pub final_poly_degree: usize,
    pub vox_size_mm: [f64; 3],
    pub prefer_contact_area: bool,
    pub electrode_type: Option<String>,
    pub metal_threshold: f32,
    /// How far (mm) the half-maximum distal edge of the intensity profile lies beyond the
    /// real distal end of the metal, for the distal-edge zero calibration in
    /// `pacer_fiducials` (`specs/10` §8 step 8a). Measured against the Lead-Tutor experts;
    /// a phantom painted without blooming wants `0.0`.
    pub distal_bloom_mm: f64,
    /// Brain mask on the CT grid in the volume's `(k, j, i)` layout (`specs/10` §1); eroded as
    /// in §4 before use. `None` computes the mask from the CT (§4).
    pub brain_mask: Option<Arc<Array3<bool>>>,
}

impl Default for PacerConfig {
    fn default() -> Self {
        PacerConfig {
            no_mask: false,
            reverse_dir: false,
            final_poly_degree: 3,
            vox_size_mm: [1.0, 1.0, 1.0],
            prefer_contact_area: false,
            electrode_type: None,
            metal_threshold: 800.0,
            distal_bloom_mm: PACER_DISTAL_BLOOM_MM,
            brain_mask: None,
        }
    }
}

/// One electrode result.
#[derive(Clone, Debug)]
pub struct PacerElectrode {
    pub poly: PolyR3,
    pub info: catalog::ElectrodeSpec,
    pub use_detected_peaks: bool,
    pub med_profile: Vec<f32>,
    pub s_mm: Vec<f32>,
    pub skel2_refpoly: PolyR3,
}

/// Run PaCER on a loaded CT. Returns one struct per accepted lead.
pub fn run_pacer(vol: &CtVolume, cfg: &PacerConfig) -> Result<Vec<PacerElectrode>, PacerError> {
    run_pacer_per_cloud(vol, cfg)?
        .into_iter()
        .map(|(_, el)| el)
        .collect()
}

pub(crate) type CloudResult = ([f64; 3], Result<PacerElectrode, PacerError>);

pub(crate) fn run_pacer_per_cloud(
    vol: &CtVolume,
    cfg: &PacerConfig,
) -> Result<Vec<CloudResult>, PacerError> {
    let to_world = |i: f64, j: f64, k: f64| fastwarp_core::apply_rowmajor(&vol.affine, [i, j, k]);
    let minv = vol.data.iter().copied().fold(f32::INFINITY, f32::min);
    let base_th = if minv >= 0.0 {
        cfg.metal_threshold + 1024.0
    } else {
        cfg.metal_threshold
    };
    let (nk, nj, ni) = (vol.nk, vol.nj, vol.ni);
    let brain: Array3<bool> = if cfg.no_mask {
        Array3::from_elem((nk, nj, ni), true)
    } else if let Some(given) = cfg.brain_mask.as_deref() {
        if given.dim() != (nk, nj, ni) {
            return Err(PacerError::BrainMaskGrid {
                mask: given.dim(),
                ct: (nk, nj, ni),
            });
        }
        erode_provided_mask(given, cfg.vox_size_mm)
    } else {
        build_brain_mask(&vol.data, cfg.vox_size_mm, &vol.affine)?
    };
    let thick = cfg.vox_size_mm[0]
        .max(cfg.vox_size_mm[1])
        .max(cfg.vox_size_mm[2])
        > 1.0;
    let use_area = cfg.prefer_contact_area || thick || cfg.electrode_type.is_some();

    let clouds = find_electrode_clouds(&vol.data, &brain, cfg.vox_size_mm, base_th, to_world)?;
    if clouds.is_empty() {
        return Err(PacerError::NoElectrode);
    }
    Ok(clouds
        .iter()
        .map(|cloud| {
            (
                cloud_centroid(&cloud.points_mm),
                refine_electrode(vol, cloud, cfg, use_area),
            )
        })
        .collect())
}

fn cloud_centroid(points: &[[f64; 3]]) -> [f64; 3] {
    let n = points.len().max(1) as f64;
    let mut c = [0.0f64; 3];
    for p in points {
        for (ca, pa) in c.iter_mut().zip(p.iter()) {
            *ca += pa / n;
        }
    }
    c
}

/// Area-based fallback when individual contacts are not resolved (`10_pacer` §156): a measured
/// contact-area width below the 3387 contact span (10.5 mm; the 3389 spans 7.5 mm) implies the
/// tighter 3389-class spacing. The threshold is derived from the catalogue rather than written
/// as a literal, so it stays correct if the geometry is amended.
fn pick_electrode_from_width(ca_c: f32, ca_w: f32) -> catalog::ElectrodeSpec {
    let wide = catalog::medtronic_3387();
    if !ca_c.is_finite() || !ca_w.is_finite() || ca_w < catalog::contact_span_mm(&wide) {
        catalog::medtronic_3389()
    } else {
        wide
    }
}

fn sample_poly_arc(p: &PolyR3, t0: f64, n: usize) -> Vec<[f64; 3]> {
    (0..=n)
        .map(|i| {
            let t = t0 + (1.0 - t0) * (i as f64) / (n as f64);
            polyval3(p, t)
        })
        .collect()
}

fn refine_electrode(
    vol: &CtVolume,
    cloud: &super::metal::PointCloud,
    cfg: &PacerConfig,
    use_area: bool,
) -> Result<PacerElectrode, PacerError> {
    let axis = pick_stack_axis(&cloud.points_mm);
    let p0 = first_pass_poly(&cloud.points_mm, &cloud.values, axis, cfg.reverse_dir)?;
    let (refitted2, med, s_mm) = run_oor(&p0, vol);
    if med.is_empty() {
        return Err(PacerError::Interpolation);
    }
    let w = (0.25f32 / Z_RES as f32).round() as usize + 1;
    let smoothed = filtfilt_box(&med, w.max(3));
    let (ca_c, ca_w) = contact_area_fallback(&s_mm, &smoothed, TIP_DEPTH_MM);
    let peak_s: Vec<f32> = if !use_area {
        let (idx, _) = find_peaks_mm(&s_mm, &smoothed, TIP_DEPTH_MM);
        idx.iter().map(|&i| s_mm[i]).collect()
    } else {
        vec![]
    };
    let (auto_info, _d, rms) = if peak_s.len() >= 4 {
        catalog::best_match(peak_s.as_slice())
    } else {
        (None, f64::INFINITY, f64::INFINITY)
    };
    let info: catalog::ElectrodeSpec = if let Some(n) = cfg.electrode_type.as_deref() {
        catalog::by_name(n)?
    } else if use_area {
        pick_electrode_from_width(ca_c, ca_w)
    } else if let Some(e) = auto_info {
        e
    } else {
        pick_electrode_from_width(ca_c, ca_w)
    };
    let use_det = !use_area && peak_s.len() >= 4 && rms < 0.3;
    let t0 = if use_det {
        let first = peak_s[0] as f64;
        let target = (first - info.zero_to_first_peak_mm as f64).max(0.0);
        inv_poly_arc_length3(&refitted2, target)
    } else {
        let mean_c = (info.ring_contact_centers_mm[0]
            + info.ring_contact_centers_mm[1]
            + info.ring_contact_centers_mm[2]
            + info.ring_contact_centers_mm[3]) as f64
            * 0.25;
        let centre = if ca_c.is_finite() {
            ca_c as f64
        } else {
            mean_c
        };
        let target = (centre - mean_c).max(0.0);
        inv_poly_arc_length3(&refitted2, target)
    };
    let sk = sample_poly_arc(&refitted2, t0, 60);
    let dfin = cfg.final_poly_degree.min(sk.len().saturating_sub(1).max(1));
    let poly = fit_param_poly_to_skeleton(&sk, dfin).ok_or(PacerError::EmptySkeleton)?;
    Ok(PacerElectrode {
        poly,
        info,
        use_detected_peaks: use_det,
        med_profile: smoothed,
        s_mm,
        skel2_refpoly: refitted2,
    })
}
