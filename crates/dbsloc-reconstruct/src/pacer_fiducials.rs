// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! PaCER result → `head` / `tail` / `x` / `y` + contacts (`specs/05` §2, §7; `specs/10` §8).
//!
//! `run_pacer` has been implemented since Phase C but nothing could call it: it returns
//! polynomials and a prototype, and no code turned those into the fiducials every consumer of
//! a reconstruction expects. This is that step.
//!
//! Contact depths come from the shared catalogue rather than PaCER's own prototype table, so a
//! lead PaCER types as a 4-level stack still gets the right per-level depths — including the
//! non-uniform depth leads and the tip-is-contact offset, which the prototype table gets wrong
//! for anything it does not enumerate.

use crate::electrode_catalog::{by_name as electrode_by_name, ElectrodeGeometry};
use crate::pacer::{
    catalog_by_name, inv_poly_arc_length3, poly_arc_length3, polyval3, run_pacer_per_cloud,
    CtVolume, PacerConfig, PacerElectrode, PacerError, PolyR3,
};
use crate::ring::ring_in_plane;
use ndarray::Array3;

/// One lead located in the CT's own (native) world space, in mm.
#[derive(Clone, Debug)]
pub struct PacerSide {
    pub head: [f64; 3],
    pub tail: [f64; 3],
    pub x: [f64; 3],
    pub y: [f64; 3],
    /// One point per **level**, distal first.
    pub level_mm: Vec<[f64; 3]>,
    /// Catalogue name of the geometry used to place them.
    pub elmodel: String,
    /// Arc length of the refined centreline (mm), for reporting.
    pub shaft_len_mm: f64,
    /// Smoothed median intensity profile along the second-pass skeleton, and its arc
    /// positions in mm from the skeleton start (`specs/10` §8), for diagnostics.
    pub profile: Vec<f32>,
    pub profile_s_mm: Vec<f32>,
    /// Arc position (mm, on `profile_s_mm`) of the tip apex, calibrated on the distal metal
    /// edge (`specs/10` §8 step 8a).
    pub apex_s_mm: f64,
}

/// One metal component PaCER accepted as a lead, and what became of it.
#[derive(Clone, Debug)]
pub struct PacerLead {
    /// Mean of the second-pass centreline over its distal 20 mm, world mm. Locates the
    /// lead's hemisphere even when placing its contacts failed.
    pub centre_mm: [f64; 3],
    /// The placed lead, or why it could not be placed.
    pub side: Result<PacerSide, PacerError>,
}

/// Build a [`CtVolume`] from an array indexed `[i, j, k]`, the order a NIfTI reader yields.
///
/// [`CtVolume`] stores `[k, j, i]` and its `trilinear` indexes that way, so handing it an
/// `(i, j, k)` array transposes the volume: every sample comes from the wrong voxel and the
/// world coordinates land somewhere unrelated. Measured on `sub-76325`, that put a lead at
/// `z = 112 mm`, outside the head — and the plausibility gate passed it, because a transposed
/// volume also transposes the field-of-view box it is checked against.
pub fn ct_volume_from_ijk(data_ijk: Array3<f32>, affine: fastwarp_core::Mat4) -> CtVolume {
    let kji = data_ijk.permuted_axes([2, 1, 0]);
    CtVolume::from_array(kji.as_standard_layout().to_owned(), affine)
}

/// Arc position (mm, on `s_mm`) of the half-maximum **distal edge** of the lead metal in a
/// PaCER intensity profile, or `None` when the profile has no clear edge in its first
/// `max_s_mm`.
///
/// The edge is where the profile first rises through half-way between the background (20th
/// percentile of the window) and the metal plateau (median of the samples above the window
/// mid-range), and stays above it for at least `hold_mm`. The profile must start below the
/// threshold, otherwise the tip lies before the traced skeleton and no edge can be measured.
pub fn distal_edge_s_mm(s_mm: &[f32], prof: &[f32], max_s_mm: f32, hold_mm: f32) -> Option<f64> {
    if s_mm.len() != prof.len() || s_mm.len() < 8 {
        return None;
    }
    let idx: Vec<usize> = (0..s_mm.len())
        .filter(|&i| s_mm[i] <= max_s_mm && prof[i].is_finite())
        .collect();
    if idx.len() < 8 {
        return None;
    }
    let mut w: Vec<f64> = idx.iter().map(|&i| prof[i] as f64).collect();
    w.sort_by(|a, b| a.total_cmp(b));
    let bg = w[((w.len() - 1) as f64 * 0.2).round() as usize];
    let pk = w[w.len() - 1];
    if pk - bg < 1e-6 {
        return None;
    }
    let hi: Vec<f64> = w
        .iter()
        .copied()
        .filter(|&v| v > bg + 0.5 * (pk - bg))
        .collect();
    let plateau = hi[hi.len() / 2];
    let thr = bg + 0.5 * (plateau - bg);
    if prof[idx[0]] as f64 >= thr {
        return None;
    }
    for (n, &i) in idx.iter().enumerate().skip(1) {
        if (prof[i] as f64) < thr {
            continue;
        }
        let held = idx[n..]
            .iter()
            .take_while(|&&j| s_mm[j] - s_mm[i] <= hold_mm)
            .all(|&j| prof[j] as f64 >= thr);
        if !held {
            continue;
        }
        let j = idx[n - 1];
        let (lo, hi_v) = (prof[j] as f64, prof[i] as f64);
        let f = if (hi_v - lo).abs() < 1e-12 {
            1.0
        } else {
            ((thr - lo) / (hi_v - lo)).clamp(0.0, 1.0)
        };
        return Some(s_mm[j] as f64 + f * (s_mm[i] - s_mm[j]) as f64);
    }
    None
}

/// Arc from the tip apex to the distal end of the metal: zero when the tip is the contact,
/// the insulating tip length otherwise.
fn apex_to_distal_metal_mm(e: &ElectrodeGeometry) -> f64 {
    if e.tip_is_contact {
        0.0
    } else {
        e.tip_length_mm
    }
}

const TIP_WINDOW_MM: f32 = 20.0;
const EDGE_HOLD_MM: f32 = 1.0;
const APEX_BEFORE_SKELETON_TOL_MM: f64 = 0.5;
const CENTRE_SPAN_MM: f64 = 20.0;

fn hundredths(mm: f64) -> i64 {
    (mm * 100.0).round() as i64
}

/// Check the calibrated apex against the skeleton it is placed on (`specs/10` §8 step 8a).
///
/// The apex may lie at most [`APEX_BEFORE_SKELETON_TOL_MM`] before the skeleton start, and
/// every level centre must fall on the skeleton: [`inv_poly_arc_length3`] clamps arc lengths
/// to `[0, L]`, so a level outside it would silently collapse onto an end point.
fn check_apex_on_skeleton(
    apex_s: f64,
    level_depths: &[f64],
    skeleton_len_mm: f64,
) -> Result<(), PacerError> {
    if apex_s < -APEX_BEFORE_SKELETON_TOL_MM {
        return Err(PacerError::ApexBeforeSkeleton {
            apex_cmm: hundredths(apex_s),
        });
    }
    let first = apex_s + level_depths.first().copied().unwrap_or(0.0);
    if first < 0.0 {
        return Err(PacerError::ApexBeforeSkeleton {
            apex_cmm: hundredths(first),
        });
    }
    let last = apex_s + level_depths.last().copied().unwrap_or(0.0);
    if last > skeleton_len_mm {
        return Err(PacerError::LevelsPastSkeleton {
            need_cmm: hundredths(last),
            have_cmm: hundredths(skeleton_len_mm),
        });
    }
    Ok(())
}

fn distal_centre(poly: &PolyR3) -> [f64; 3] {
    let len = poly_arc_length3(poly, 0.0, 1.0);
    let span = CENTRE_SPAN_MM.min(len).max(0.0);
    let n = 9usize;
    let mut acc = [0.0f64; 3];
    for i in 0..n {
        let arc = span * i as f64 / (n - 1) as f64;
        let p = polyval3(poly, inv_poly_arc_length3(poly, arc));
        for a in 0..3 {
            acc[a] += p[a] / n as f64;
        }
    }
    acc
}

/// Turn one `PacerElectrode` into fiducials, using `electrode` when given and PaCER's own
/// typing otherwise.
///
/// The tip apex is calibrated on the measured **distal metal edge** of the intensity profile
/// (`specs/10` §8 step 8a) whenever the profile shows one: the edge plus `distal_bloom_mm`
/// is the distal end of the metal, and the catalogue says how far that is from the apex.
/// Without an edge the lead is refused (`NoDistalEdge`) rather than placed from PaCER's own
/// `t0` (peaks or contact area): on real post-op CT the contacts bloom into one plateau, so
/// the contact-area run that `t0` keys on is a later bump of that plateau. Measured on
/// `sub-76325` and `sub-78754`, that put `head` 6.6–10.4 mm up the shaft; on `sub-80206` L,
/// where the skeleton starts inside the metal, 7.1 mm. An apex or level that falls off the
/// skeleton is refused too (`ApexBeforeSkeleton`, `LevelsPastSkeleton`).
pub fn side_from_electrode(
    el: &PacerElectrode,
    electrode: Option<&ElectrodeGeometry>,
    distal_bloom_mm: f64,
) -> Result<PacerSide, PacerError> {
    let geom = match electrode {
        Some(g) => g,
        None => electrode_by_name(el.info.name)
            .ok_or_else(|| PacerError::UnknownElectrode(el.info.name.to_owned()))?,
    };
    let edge = distal_edge_s_mm(&el.s_mm, &el.med_profile, TIP_WINDOW_MM, EDGE_HOLD_MM)
        .ok_or(PacerError::NoDistalEdge)?;
    let apex_s = edge + distal_bloom_mm - apex_to_distal_metal_mm(geom);
    let depths = geom.apex_to_level_mm();
    let skel_len = poly_arc_length3(&el.skel2_refpoly, 0.0, 1.0);
    check_apex_on_skeleton(apex_s, &depths, skel_len)?;
    let level_mm: Vec<[f64; 3]> = depths
        .iter()
        .map(|d| {
            polyval3(
                &el.skel2_refpoly,
                inv_poly_arc_length3(&el.skel2_refpoly, apex_s + d),
            )
        })
        .collect();
    if level_mm.len() < 2 {
        return Err(PacerError::EmptySkeleton);
    }
    let head = level_mm[0];
    let tail = level_mm[level_mm.len() - 1];
    let (x, y) = ring_in_plane(head, tail, None, geom.lead_diameter_mm);
    Ok(PacerSide {
        head,
        tail,
        x,
        y,
        level_mm,
        elmodel: geom.elmodel.clone(),
        shaft_len_mm: poly_arc_length3(&el.poly, 0.0, 1.0),
        profile: el.med_profile.clone(),
        profile_s_mm: el.s_mm.clone(),
        apex_s_mm: apex_s,
    })
}

/// Run PaCER on a native-space CT and return one [`PacerLead`] per accepted metal component,
/// each with its own placement result, so one lead that cannot be placed does not discard
/// the others.
///
/// `el_model` pins the geometry; without it PaCER's own typing decides, which is only as good
/// as the intensity profile and cannot distinguish leads that share a level pitch. A pinned
/// name is also handed to PaCER itself when its prototype table knows it, so its width/peak
/// typing cannot disagree with the geometry the levels are then placed from.
pub fn pacer_leads(
    vol: &CtVolume,
    cfg: &PacerConfig,
    el_model: Option<&str>,
) -> Result<Vec<PacerLead>, PacerError> {
    let pinned: Option<&'static ElectrodeGeometry> = match el_model {
        Some(name) => Some(
            electrode_by_name(name).ok_or_else(|| PacerError::UnknownElectrode(name.to_owned()))?,
        ),
        None => None,
    };
    let mut cfg = cfg.clone();
    if let Some(g) = pinned {
        if cfg.electrode_type.is_none() && catalog_by_name(&g.elmodel).is_ok() {
            cfg.electrode_type = Some(g.elmodel.clone());
        }
    }
    let leads = run_pacer_per_cloud(vol, &cfg)?;
    Ok(leads
        .into_iter()
        .map(|(centroid, el)| match el {
            Ok(el) => PacerLead {
                centre_mm: distal_centre(&el.skel2_refpoly),
                side: side_from_electrode(&el, pinned, cfg.distal_bloom_mm),
            },
            Err(e) => PacerLead {
                centre_mm: centroid,
                side: Err(e),
            },
        })
        .collect())
}

/// [`pacer_leads`] for callers that need every lead placed: the first placement error is
/// returned instead of the leads.
pub fn pacer_sides(
    vol: &CtVolume,
    cfg: &PacerConfig,
    el_model: Option<&str>,
) -> Result<Vec<PacerSide>, PacerError> {
    pacer_leads(vol, cfg, el_model)?
        .into_iter()
        .map(|l| l.side)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pacer::fit_param_poly_to_skeleton;

    fn levels_along_poly(poly: &PolyR3, e: &ElectrodeGeometry) -> Vec<[f64; 3]> {
        e.apex_to_level_mm()
            .into_iter()
            .map(|arc| polyval3(poly, inv_poly_arc_length3(poly, arc)))
            .collect()
    }

    fn straight_poly(from: [f64; 3], dir: [f64; 3], len_mm: f64) -> PolyR3 {
        let n = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        let u = [dir[0] / n, dir[1] / n, dir[2] / n];
        let pts: Vec<[f64; 3]> = (0..=40)
            .map(|i| {
                let a = len_mm * i as f64 / 40.0;
                [from[0] + u[0] * a, from[1] + u[1] * a, from[2] + u[2] * a]
            })
            .collect();
        fit_param_poly_to_skeleton(&pts, 3).expect("fit")
    }

    /// Levels must land at the catalogue's apex-relative depths, distal first. A hardcoded
    /// pitch passes a 3387 and fails a 3389, and a wrong apex offset shifts all of them.
    #[test]
    fn levels_sit_at_catalogue_depths() {
        for name in [
            "Medtronic 3387",
            "Medtronic 3389",
            "Boston Scientific Vercise Directed",
        ] {
            let e = electrode_by_name(name).expect("catalogued");
            let apex = [-12.0, -3.0, -9.0];
            let dir = [0.15, 0.1, 1.0];
            let poly = straight_poly(apex, dir, 40.0);
            let got = levels_along_poly(&poly, e);
            assert_eq!(got.len(), e.n_levels(), "{name}");

            let n = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            let u = [dir[0] / n, dir[1] / n, dir[2] / n];
            for (k, want_arc) in e.apex_to_level_mm().into_iter().enumerate() {
                let want = [
                    apex[0] + u[0] * want_arc,
                    apex[1] + u[1] * want_arc,
                    apex[2] + u[2] * want_arc,
                ];
                let d = ((got[k][0] - want[0]).powi(2)
                    + (got[k][1] - want[1]).powi(2)
                    + (got[k][2] - want[2]).powi(2))
                .sqrt();
                assert!(d < 0.05, "{name} level {k}: off by {d:.3} mm");
            }
        }
    }

    /// The tip-is-contact offset is what roadmap 2.5 got wrong: the same traced shaft must put
    /// a Boston level 0 at 0.75 mm from the apex and a Medtronic one at 2.25 mm.
    #[test]
    fn tip_is_contact_changes_the_first_level_depth() {
        let apex = [0.0, 0.0, 0.0];
        let poly = straight_poly(apex, [0.0, 0.0, 1.0], 30.0);
        let boston = electrode_by_name("Boston Scientific Vercise Directed").unwrap();
        let medtronic = electrode_by_name("Medtronic 3389").unwrap();
        let b0 = levels_along_poly(&poly, boston)[0];
        let m0 = levels_along_poly(&poly, medtronic)[0];
        assert!((b0[2] - 0.75).abs() < 0.05, "boston z {}", b0[2]);
        assert!((m0[2] - 2.25).abs() < 0.05, "medtronic z {}", m0[2]);
        assert!(
            (m0[2] - b0[2]).abs() > 1.0,
            "the two must not coincide, or the offset is not being applied"
        );
    }

    /// `head`/`tail` are the outer level centres, and `x`/`y` sit one lead radius off the
    /// shaft, matching what the TRAC/CORE path produces.
    #[test]
    fn fiducials_match_the_reconstruction_contract() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let poly = straight_poly([-12.0, -3.0, -9.0], [0.1, 0.0, 1.0], 30.0);
        let levels = levels_along_poly(&poly, e);
        let (x, y) = ring_in_plane(
            levels[0],
            levels[levels.len() - 1],
            None,
            e.lead_diameter_mm,
        );
        let r = 0.5 * e.lead_diameter_mm;
        for p in [x, y] {
            let d = ((p[0] - levels[0][0]).powi(2)
                + (p[1] - levels[0][1]).powi(2)
                + (p[2] - levels[0][2]).powi(2))
            .sqrt();
            assert!((d - r).abs() < 1e-6, "marker {d} mm from head, want {r}");
        }
        let span = ((levels[levels.len() - 1][2] - levels[0][2]).powi(2)
            + (levels[levels.len() - 1][0] - levels[0][0]).powi(2))
        .sqrt();
        assert!((span - e.contact_span_mm()).abs() < 0.05, "span {span}");
    }

    /// A non-cubic shape with one marked voxel: the value must come back at the *same* index
    /// the NIfTI had it at, and the extents must not be permuted. A cubic fixture cannot fail
    /// this, which is why the transpose went unnoticed.
    #[test]
    fn ijk_conversion_preserves_index_and_extent() {
        let mut data = Array3::<f32>::zeros((3, 4, 5));
        data[[1, 2, 3]] = 99.0;
        let vol = ct_volume_from_ijk(data, fastwarp_core::identity_mat4());
        assert_eq!((vol.ni, vol.nj, vol.nk), (3, 4, 5));
        assert!((vol.trilinear(1.0, 2.0, 3.0) - 99.0).abs() < 1e-6);
        assert!(vol.trilinear(3.0, 2.0, 1.0).abs() < 1e-6 || vol.trilinear(3.0, 2.0, 1.0).is_nan());
    }

    fn bloomed_profile(start_mm: f32) -> (Vec<f32>, Vec<f32>) {
        let s: Vec<f32> = (0..2000).map(|i| start_mm + 0.025 * i as f32).collect();
        let p: Vec<f32> = s
            .iter()
            .map(|&x| {
                if x < 3.0 {
                    0.0
                } else if x < 16.0 {
                    1300.0 + 150.0 * (x * 2.0).sin()
                } else {
                    900.0
                }
            })
            .collect();
        (s, p)
    }

    #[test]
    fn distal_edge_is_the_first_rise_of_the_plateau() {
        let (s, p) = bloomed_profile(-2.0);
        let e = distal_edge_s_mm(&s, &p, 20.0, 1.0).expect("edge");
        assert!((e - 3.0).abs() < 0.1, "edge at {e}");
    }

    #[test]
    fn profile_starting_in_metal_has_no_edge() {
        let (s, p) = bloomed_profile(5.0);
        assert!(distal_edge_s_mm(&s, &p, 20.0, 1.0).is_none());
    }

    fn electrode_on(poly: PolyR3, edge_at_mm: f32) -> PacerElectrode {
        let s: Vec<f32> = (0..1600).map(|i| -2.0 + 0.025 * i as f32).collect();
        let p: Vec<f32> = s
            .iter()
            .map(|&x| {
                if x >= edge_at_mm && x < edge_at_mm + 12.0 {
                    1300.0
                } else {
                    0.0
                }
            })
            .collect();
        PacerElectrode {
            poly: poly.clone(),
            info: catalog_by_name("Medtronic 3387").expect("pacer catalogue"),
            use_detected_peaks: false,
            med_profile: p,
            s_mm: s,
            skel2_refpoly: poly,
        }
    }

    #[test]
    fn apex_before_the_skeleton_is_refused() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let el = electrode_on(straight_poly([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 40.0), 0.2);
        let got = side_from_electrode(&el, Some(e), 0.0);
        assert!(
            matches!(got, Err(PacerError::ApexBeforeSkeleton { .. })),
            "{got:?}"
        );
    }

    #[test]
    fn levels_past_the_skeleton_are_refused() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let el = electrode_on(straight_poly([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 12.0), 3.0);
        let got = side_from_electrode(&el, Some(e), 0.0);
        assert!(
            matches!(got, Err(PacerError::LevelsPastSkeleton { .. })),
            "{got:?}"
        );
    }

    #[test]
    fn apex_on_the_skeleton_places_levels_at_catalogue_depths() {
        let e = electrode_by_name("Medtronic 3387").unwrap();
        let el = electrode_on(straight_poly([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 40.0), 3.0);
        let side = side_from_electrode(&el, Some(e), 0.0).expect("placed");
        let want = 3.0 - e.tip_length_mm + e.apex_to_first_level_mm();
        assert!(
            (side.head[2] - want).abs() < 0.05,
            "head z {}",
            side.head[2]
        );
        assert!((side.apex_s_mm - (3.0 - e.tip_length_mm)).abs() < 0.05);
    }

    #[test]
    fn unknown_pinned_model_is_rejected() {
        let vol = CtVolume::from_array(
            ndarray::Array3::<f32>::zeros((4, 4, 4)),
            fastwarp_core::identity_mat4(),
        );
        let got = pacer_sides(&vol, &PacerConfig::default(), Some("Not A Lead"));
        assert!(
            matches!(got, Err(PacerError::UnknownElectrode(_))),
            "{got:?}"
        );
    }
}
