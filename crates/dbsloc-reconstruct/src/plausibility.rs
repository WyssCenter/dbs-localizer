// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Post-conditions on a reconstructed side (`specs/05` §2, §4.2, §11).
//!
//! Reconstruction previously emitted a success result for any geometry the trace happened to
//! return, including centreline extremes tens of millimetres outside the brain. These checks
//! are deliberately **model-derived and assumption-free about anatomy**: they compare the
//! result against the electrode geometry that produced it and against the field of view of
//! the volume that was traced. Anything that passes is not necessarily correct, but anything
//! that fails is certainly not a reconstruction.

use crate::math::{norm3, sub3};

/// World-space axis-aligned bounds of the traced volume, in mm.
#[derive(Clone, Copy, Debug)]
pub struct WorldBounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl WorldBounds {
    /// Bounds of a voxel grid of shape `(ni, nj, nk)` under a row-major index→world affine,
    /// grown by `pad_mm` on every side.
    pub fn from_grid(
        affine: &fastwarp_core::Mat4,
        ni: usize,
        nj: usize,
        nk: usize,
        pad_mm: f64,
    ) -> Self {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        let corners = [
            [0.0, 0.0, 0.0],
            [ni.saturating_sub(1) as f64, 0.0, 0.0],
            [0.0, nj.saturating_sub(1) as f64, 0.0],
            [0.0, 0.0, nk.saturating_sub(1) as f64],
            [
                ni.saturating_sub(1) as f64,
                nj.saturating_sub(1) as f64,
                0.0,
            ],
            [
                ni.saturating_sub(1) as f64,
                0.0,
                nk.saturating_sub(1) as f64,
            ],
            [
                0.0,
                nj.saturating_sub(1) as f64,
                nk.saturating_sub(1) as f64,
            ],
            [
                ni.saturating_sub(1) as f64,
                nj.saturating_sub(1) as f64,
                nk.saturating_sub(1) as f64,
            ],
        ];
        for c in corners {
            let w = fastwarp_core::apply_rowmajor(affine, c);
            for a in 0..3 {
                min[a] = min[a].min(w[a]);
                max[a] = max[a].max(w[a]);
            }
        }
        for a in 0..3 {
            min[a] -= pad_mm;
            max[a] += pad_mm;
        }
        WorldBounds { min, max }
    }

    pub fn contains(&self, p: [f64; 3]) -> bool {
        (0..3).all(|a| p[a] >= self.min[a] && p[a] <= self.max[a])
    }
}

/// Tolerances for [`check_side`]. Derived from the electrode model, not from anatomy.
///
/// Axial distances are measured between **levels** — one ring, or one segmented group of
/// three. The three segments of a directional level sit at the same depth roughly 1.1 mm
/// apart radially, so checking consecutive *contacts* would compare that radial gap against
/// the axial pitch and reject every directional lead.
#[derive(Clone, Debug)]
pub struct PlausibilityLimits {
    /// Expected `‖tail − head‖`, i.e. the distal-to-proximal level span.
    pub expected_span_mm: f64,
    /// Allowed absolute deviation of the measured head→tail span.
    pub span_tol_mm: f64,
    /// Expected centre-to-centre pitch per level gap; length `n_levels − 1`.
    pub expected_pitches_mm: Vec<f64>,
    /// Allowed absolute deviation of each level pitch.
    pub pitch_tol_mm: f64,
    /// Shaft radius; segment centroids sit on it, ring centroids on the axis.
    pub lead_radius_mm: f64,
    /// Allowed absolute deviation of a contact's offset from the shaft axis.
    pub radius_tol_mm: f64,
}

impl PlausibilityLimits {
    #[cfg(test)]
    fn for_levels(n_levels: usize, eldist_mm: f64, lead_diameter_mm: f64) -> Self {
        let z: Vec<f64> = (0..n_levels.max(1)).map(|i| i as f64 * eldist_mm).collect();
        Self::for_level_z(&z, lead_diameter_mm)
    }

    /// Pitch tolerance stays below half the 3387/3389 pitch difference (3.0 vs 2.0 mm), so a
    /// lead reconstructed with the wrong model does not pass as the right one. Leads with
    /// non-uniform gaps carry one expected pitch per gap; the tolerance follows the smallest.
    pub fn for_level_z(level_z_mm: &[f64], lead_diameter_mm: f64) -> Self {
        let pitches: Vec<f64> = level_z_mm.windows(2).map(|w| w[1] - w[0]).collect();
        let span = match (level_z_mm.first(), level_z_mm.last()) {
            (Some(a), Some(b)) => b - a,
            _ => 0.0,
        };
        let pitch_ref = pitches.iter().copied().fold(f64::INFINITY, f64::min);
        let pitch_ref = if pitch_ref.is_finite() {
            pitch_ref
        } else {
            0.0
        };
        let r = 0.5 * lead_diameter_mm;
        PlausibilityLimits {
            expected_span_mm: span,
            span_tol_mm: (0.25 * span).max(2.0),
            expected_pitches_mm: pitches,
            pitch_tol_mm: (0.2 * pitch_ref).max(0.3),
            lead_radius_mm: r,
            radius_tol_mm: (0.25 * r).max(0.3),
        }
    }

    fn expected_pitch_at(&self, gap: usize) -> f64 {
        match self.expected_pitches_mm.len() {
            0 => 0.0,
            n if gap < n => self.expected_pitches_mm[gap],
            n => self.expected_pitches_mm[n - 1],
        }
    }
}

fn all_finite(p: [f64; 3]) -> bool {
    p.iter().all(|v| v.is_finite())
}

/// Steepest trajectory accepted in template space, degrees from the *z* axis.
///
/// Unlike [`check_side`] this is an anatomical assumption, so it is a separate check, and it is
/// only meaningful once the volume is in template space, where "vertical" means something. A
/// DBS lead enters through a frontal burr hole and heads for a deep target: on the Lead-Tutor
/// expert reconstructions that is 36.6–41.2° from vertical. The traces this catches ran at
/// 60.0° and 65.1° — a seed on the frontal skull that walked back to the contacts, which the
/// model-derived checks pass because the contacts it then found were real.
pub const MAX_TILT_FROM_VERTICAL_DEG: f64 = 55.0;

/// Angle between the `head→tail` axis and the *z* axis in degrees; `NaN` for a degenerate axis.
pub fn tilt_from_vertical_deg(head: [f64; 3], tail: [f64; 3]) -> f64 {
    let v = sub3(tail, head);
    let n = norm3(v);
    if n < 1e-9 {
        return f64::NAN;
    }
    (v[2].abs() / n).clamp(0.0, 1.0).acos().to_degrees()
}

/// `Some(problem)` when the trajectory tilts more than `max_deg` from vertical. A degenerate
/// axis is left to the span check in [`check_side`].
pub fn check_tilt(head: [f64; 3], tail: [f64; 3], max_deg: f64) -> Option<String> {
    let tilt = tilt_from_vertical_deg(head, tail);
    if !tilt.is_finite() || tilt <= max_deg {
        return None;
    }
    Some(format!(
        "trajectory tilts {tilt:.1}° from vertical, beyond {max_deg:.0}°; a lead from a frontal \
         burr hole to a deep target does not lie this flat, so either the trace started on \
         something other than the lead or the CT sits rotated against the anchor — check the \
         CT→anchor coregistration"
    ))
}

/// Centroid of each level. `contact_level` maps each contact to its 0-based level; `None`
/// means one contact per level, which is the ring-lead case.
fn level_centroids(contacts: &[[f64; 3]], contact_level: Option<&[usize]>) -> Vec<[f64; 3]> {
    let Some(lv) = contact_level else {
        return contacts.to_vec();
    };
    let n_lev = lv.iter().copied().max().map_or(0, |m| m + 1);
    let mut sums = vec![[0.0f64; 3]; n_lev];
    let mut counts = vec![0usize; n_lev];
    for (c, &k) in contacts.iter().zip(lv.iter()) {
        for a in 0..3 {
            sums[k][a] += c[a];
        }
        counts[k] += 1;
    }
    sums.iter()
        .zip(counts.iter())
        .map(|(s, &n)| {
            if n == 0 {
                [f64::NAN; 3]
            } else {
                [s[0] / n as f64, s[1] / n as f64, s[2] / n as f64]
            }
        })
        .collect()
}

/// Perpendicular distance of `p` from the `head→tail` shaft axis (mm).
fn radial_offset(p: [f64; 3], head: [f64; 3], tail: [f64; 3]) -> f64 {
    let axis = sub3(tail, head);
    let n = norm3(axis);
    let rel = sub3(p, head);
    if n < 1e-9 {
        return norm3(rel);
    }
    let u = [axis[0] / n, axis[1] / n, axis[2] / n];
    let along = rel[0] * u[0] + rel[1] * u[1] + rel[2] * u[2];
    norm3([
        rel[0] - along * u[0],
        rel[1] - along * u[1],
        rel[2] - along * u[2],
    ])
}

/// Check one reconstructed side. Returns the list of violations; empty means plausible.
pub fn check_side(
    head: [f64; 3],
    tail: [f64; 3],
    contacts: &[[f64; 3]],
    contact_level: Option<&[usize]>,
    limits: &PlausibilityLimits,
    bounds: Option<&WorldBounds>,
) -> Vec<String> {
    let mut bad: Vec<String> = Vec::new();

    if !all_finite(head) || !all_finite(tail) {
        bad.push(format!(
            "markers not finite: head={} tail={}",
            py_list(&head, None),
            py_list(&tail, None)
        ));
        return bad;
    }
    if contacts.is_empty() {
        bad.push("no contacts produced".to_owned());
        return bad;
    }
    if contacts.iter().any(|c| !all_finite(*c)) {
        bad.push("contacts contain non-finite values".to_owned());
        return bad;
    }
    if let Some(lv) = contact_level {
        if lv.len() != contacts.len() {
            bad.push(format!(
                "contact_level has {} entries for {} contacts",
                lv.len(),
                contacts.len()
            ));
            return bad;
        }
    }

    let span = norm3(sub3(tail, head));
    let d_span = (span - limits.expected_span_mm).abs();
    if d_span > limits.span_tol_mm {
        bad.push(format!(
            "head→tail span {:.2} mm deviates {:.2} mm from model span {:.2} mm (tol {:.2}); \
             head/tail are probably centreline extremes, not the distal / proximal level centres",
            span, d_span, limits.expected_span_mm, limits.span_tol_mm
        ));
    }

    let lc = level_centroids(contacts, contact_level);
    for i in 1..lc.len() {
        let pitch = norm3(sub3(lc[i], lc[i - 1]));
        let exp = limits.expected_pitch_at(i - 1);
        let d = (pitch - exp).abs();
        if d > limits.pitch_tol_mm {
            bad.push(format!(
                "level {} → {} pitch {:.2} mm deviates {:.2} mm from model pitch {:.2} mm (tol {:.2})",
                i - 1,
                i,
                pitch,
                d,
                exp,
                limits.pitch_tol_mm
            ));
        }
    }

    let r_max = limits.lead_radius_mm + limits.radius_tol_mm;
    for (i, c) in contacts.iter().enumerate() {
        let radial = radial_offset(*c, head, tail);
        if radial > r_max {
            bad.push(format!(
                "contact {i} sits {radial:.2} mm off the shaft axis, beyond the lead radius \
                 {:.2} mm (tol {:.2})",
                limits.lead_radius_mm, limits.radius_tol_mm
            ));
        }
    }

    if let Some(b) = bounds {
        let span = format!(
            "({} … {})",
            py_list(&b.min, Some(1)),
            py_list(&b.max, Some(1))
        );
        for (i, c) in contacts.iter().enumerate() {
            if !b.contains(*c) {
                bad.push(format!(
                    "contact {i} at {} is outside the traced volume {span}",
                    py_list(c, Some(2))
                ));
            }
        }
        for (name, p) in [("head", head), ("tail", tail)] {
            if !b.contains(p) {
                bad.push(format!(
                    "{name} at {} is outside the traced volume {span}",
                    py_list(&p, Some(2))
                ));
            }
        }
    }

    bad
}

pub fn py_float(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 {
            "inf".to_owned()
        } else {
            "-inf".to_owned()
        };
    }
    let s = format!("{x}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{s}.0")
    }
}

pub fn py_list(v: &[f64], decimals: Option<i32>) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|&x| {
            let r = match decimals {
                Some(d) => {
                    let f = 10f64.powi(d);
                    (x * f).round_ties_even() / f
                }
                None => x,
            };
            py_float(r)
        })
        .collect();
    format!("[{}]", items.join(", "))
}

fn median(v: &[f64]) -> f64 {
    if v.iter().any(|x| x.is_nan()) {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        0.5 * (s[n / 2 - 1] + s[n / 2])
    }
}

pub const METAL_ON_AXIS_MIN_FRACTION: f64 = 0.5;

pub fn check_metal_on_axis(
    on_axis: &[f64],
    on_disc: &[f64],
    min_fraction: f64,
    min_on_axis: Option<f64>,
) -> Vec<String> {
    if on_axis.is_empty() || on_disc.is_empty() {
        return vec!["no samples along the axis to check the metal against".to_owned()];
    }
    let med_a = median(on_axis);
    let med_d = median(on_disc);
    if !med_a.is_finite() || !med_d.is_finite() {
        return vec!["non-finite image samples along the axis".to_owned()];
    }
    let mut bad = Vec::new();
    if med_d <= 0.0 || med_a < min_fraction * med_d {
        bad.push(format!(
            "median on-axis value {med_a:.0} is below {:.0}% of the disc maximum {med_d:.0} \
             over the contact span; the axis runs beside the bright structure the trace \
             followed, not through a lead",
            min_fraction * 100.0
        ));
    }
    if let Some(floor) = min_on_axis {
        if med_a < floor {
            bad.push(format!(
                "median on-axis value {med_a:.0} is below {floor:.0}; nothing metallic sits on \
                 the traced axis"
            ));
        }
    }
    bad
}

pub const BRAIN_MASK_MARGIN_MM: f64 = 3.0;

pub fn check_in_brain_mask(
    points: &[[f64; 3]],
    mask: &ndarray::Array3<bool>,
    mask_aff: &fastwarp_core::Mat4,
    margin_mm: f64,
) -> Vec<String> {
    let Some(inv) = fastwarp_core::invert_rowmajor(mask_aff) else {
        return vec!["template brain mask affine is singular".to_owned()];
    };
    let reach: Vec<f64> = (0..3)
        .map(|a| margin_mm.max(0.0) * norm3([inv[a][0], inv[a][1], inv[a][2]]))
        .collect();
    let dims = mask.dim();
    let shape = [dims.0 as i64, dims.1 as i64, dims.2 as i64];
    let in_mask = |ijk: [i64; 3]| -> bool {
        (0..3).all(|a| ijk[a] >= 0 && ijk[a] < shape[a])
            && mask[[ijk[0] as usize, ijk[1] as usize, ijk[2] as usize]]
    };
    let mut bad = Vec::new();
    for (i, p) in points.iter().enumerate() {
        let v = fastwarp_core::apply_rowmajor(&inv, *p);
        let nearest = [
            v[0].round_ties_even() as i64,
            v[1].round_ties_even() as i64,
            v[2].round_ties_even() as i64,
        ];
        let mut inside = in_mask(nearest);
        if !inside {
            let lo: Vec<i64> = (0..3)
                .map(|a| ((v[a] - reach[a]).floor() as i64).max(0))
                .collect();
            let hi: Vec<i64> = (0..3)
                .map(|a| ((v[a] + reach[a]).ceil() as i64 + 1).min(shape[a]))
                .collect();
            'scan: for a in lo[0]..hi[0] {
                for b in lo[1]..hi[1] {
                    for c in lo[2]..hi[2] {
                        if !mask[[a as usize, b as usize, c as usize]] {
                            continue;
                        }
                        let w =
                            fastwarp_core::apply_rowmajor(mask_aff, [a as f64, b as f64, c as f64]);
                        if norm3(sub3(w, *p)) <= margin_mm {
                            inside = true;
                            break 'scan;
                        }
                    }
                }
            }
        }
        if !inside {
            bad.push(format!(
                "contact {i} at {} lies outside the template brain mask (margin {} mm); a DBS \
                 contact is inside the brain, so the trace followed bone or soft tissue, not the \
                 lead",
                py_list(p, Some(1)),
                py_general(margin_mm)
            ));
        }
    }
    bad
}

fn py_general(x: f64) -> String {
    format!("{x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits_3387() -> PlausibilityLimits {
        PlausibilityLimits::for_levels(4, 3.0, 1.27)
    }

    fn tilted(head: [f64; 3], deg: f64) -> [f64; 3] {
        let r = deg.to_radians();
        [head[0] + 6.0 * r.sin(), head[1], head[2] + 6.0 * r.cos()]
    }

    /// Measured on the Lead-Tutor cohort (2026-09-11): experts 36.6–41.2°, the two traces
    /// seeded on the frontal skull 60.0° and 65.1°.
    #[test]
    fn expert_like_tilt_passes_and_skull_seeded_trace_fails() {
        let head = [-12.9, -15.8, -16.6];
        for deg in [0.0, 40.0, 54.0] {
            let tail = tilted(head, deg);
            assert!(
                check_tilt(head, tail, MAX_TILT_FROM_VERTICAL_DEG).is_none(),
                "{deg}"
            );
            assert!(
                check_tilt(tail, head, MAX_TILT_FROM_VERTICAL_DEG).is_none(),
                "{deg}"
            );
        }
        let tail = tilted(head, 65.1);
        assert!((tilt_from_vertical_deg(head, tail) - 65.1).abs() < 1e-6);
        let bad = check_tilt(head, tail, MAX_TILT_FROM_VERTICAL_DEG).expect("65° is rejected");
        assert!(bad.contains("65.1"), "{bad}");
        assert!(bad.contains("coregistration"), "{bad}");
    }

    #[test]
    fn degenerate_axis_is_left_to_the_span_check() {
        let p = [1.0, 2.0, 3.0];
        assert!(tilt_from_vertical_deg(p, p).is_nan());
        assert!(check_tilt(p, p, MAX_TILT_FROM_VERTICAL_DEG).is_none());
    }

    #[test]
    fn ideal_lead_passes() {
        let l = limits_3387();
        let head = [10.0, -2.0, -6.0];
        let contacts: Vec<[f64; 3]> = (0..4)
            .map(|i| [10.0, -2.0, -6.0 + 3.0 * i as f64])
            .collect();
        let tail = contacts[3];
        assert!(check_side(head, tail, &contacts, None, &l, None).is_empty());
    }

    #[test]
    fn centreline_extremes_are_rejected() {
        let l = limits_3387();
        let head = [-68.5, 14.5, 76.0];
        let tail = [-72.1, 17.2, -80.0];
        let contacts: Vec<[f64; 3]> = (0..4)
            .map(|i| [-69.5, 15.2, 38.1 - 1.5 * i as f64])
            .collect();
        let bad = check_side(head, tail, &contacts, None, &l, None);
        assert!(!bad.is_empty(), "156 mm span must be rejected");
        assert!(bad.iter().any(|m| m.contains("span")), "{bad:?}");
    }

    #[test]
    fn wrong_pitch_is_rejected() {
        let l = limits_3387();
        let head = [0.0, 0.0, 0.0];
        let contacts: Vec<[f64; 3]> = (0..4).map(|i| [0.0, 0.0, 1.5 * i as f64]).collect();
        let tail = contacts[3];
        let bad = check_side(head, tail, &contacts, None, &l, None);
        assert!(bad.iter().any(|m| m.contains("pitch")), "{bad:?}");
    }

    /// A 3389 (2.0 mm pitch) must not pass the 3387 (3.0 mm) limits, or the gate would
    /// silently accept a reconstruction built from the wrong electrode model.
    #[test]
    fn adjacent_electrode_model_is_rejected() {
        let l = limits_3387();
        let contacts: Vec<[f64; 3]> = (0..4).map(|i| [0.0, 0.0, 2.0 * i as f64]).collect();
        let bad = check_side(contacts[0], contacts[3], &contacts, None, &l, None);
        assert!(bad.iter().any(|m| m.contains("pitch")), "{bad:?}");
    }

    #[test]
    fn empty_grids_do_not_panic() {
        let b = WorldBounds::from_grid(&fastwarp_core::identity_mat4(), 0, 0, 0, 0.0);
        assert_eq!(b.min, [0.0; 3]);
        assert_eq!(b.max, [0.0; 3]);
    }

    #[test]
    fn contact_outside_fov_is_rejected() {
        let l = limits_3387();
        let aff: fastwarp_core::Mat4 = [
            [1.0, 0.0, 0.0, -20.0],
            [0.0, 1.0, 0.0, -20.0],
            [0.0, 0.0, 1.0, -20.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let b = WorldBounds::from_grid(&aff, 40, 40, 40, 0.0);
        let head = [0.0, 0.0, 0.0];
        let contacts: Vec<[f64; 3]> = (0..4).map(|i| [0.0, 0.0, 3.0 * i as f64]).collect();
        let tail = contacts[3];
        assert!(check_side(head, tail, &contacts, None, &l, Some(&b)).is_empty());

        let far: Vec<[f64; 3]> = (0..4).map(|i| [500.0, 0.0, 3.0 * i as f64]).collect();
        let bad = check_side([500.0, 0.0, 0.0], far[3], &far, None, &l, Some(&b));
        assert!(
            bad.iter().any(|m| m.contains("outside the traced volume")),
            "{bad:?}"
        );
    }

    #[test]
    fn nan_is_rejected() {
        let l = limits_3387();
        let bad = check_side(
            [f64::NAN, 0.0, 0.0],
            [0.0, 0.0, 9.0],
            &[[0.0, 0.0, 0.0]],
            None,
            &l,
            None,
        );
        assert!(bad.iter().any(|m| m.contains("not finite")), "{bad:?}");
    }

    /// The whole point of level awareness: a correct 8-contact directional reconstruction
    /// must pass. Without `contact_level` the 1.13 mm gap between two segments of one level
    /// would be compared against the 2.0 mm axial pitch and rejected.
    #[test]
    fn directional_lead_passes_with_levels_and_fails_without() {
        let e = crate::electrode_by_name("Boston Scientific Vercise Directed").expect("catalogued");
        let l = PlausibilityLimits::for_level_z(&e.level_z_mm, e.lead_diameter_mm);
        let head = [0.0, 0.0, 0.0];
        let tail = [0.0, 0.0, e.contact_span_mm()];
        let contacts: Vec<[f64; 3]> = e.contact_xyz_mm.clone();

        let bad = check_side(head, tail, &contacts, Some(&e.contact_level), &l, None);
        assert!(bad.is_empty(), "{bad:?}");

        let bad = check_side(head, tail, &contacts, None, &l, None);
        assert!(
            bad.iter().any(|m| m.contains("pitch")),
            "contact-wise checking must reject a correct directional lead: {bad:?}"
        );
    }

    /// A segment displaced far outside the shaft is not a contact on this lead.
    #[test]
    fn contact_off_the_shaft_axis_is_rejected() {
        let e = crate::electrode_by_name("Boston Scientific Vercise Directed").expect("catalogued");
        let l = PlausibilityLimits::for_level_z(&e.level_z_mm, e.lead_diameter_mm);
        let mut contacts = e.contact_xyz_mm.clone();
        contacts[1][0] += 4.0;
        let bad = check_side(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, e.contact_span_mm()],
            &contacts,
            Some(&e.contact_level),
            &l,
            None,
        );
        assert!(
            bad.iter().any(|m| m.contains("off the shaft axis")),
            "{bad:?}"
        );
    }

    #[test]
    fn metal_on_axis_passes_a_centred_lead_and_rejects_a_line_beside_bone() {
        let disc = vec![2400.0; 13];
        let axis: Vec<f64> = disc.iter().map(|v| 0.9 * v).collect();
        assert!(check_metal_on_axis(&axis, &disc, METAL_ON_AXIS_MIN_FRACTION, None).is_empty());
        let bad = check_metal_on_axis(&[40.0; 13], &[1300.0; 13], METAL_ON_AXIS_MIN_FRACTION, None);
        assert_eq!(bad.len(), 1);
        assert!(
            bad[0].contains("beside") && bad[0].contains("below 50%"),
            "{bad:?}"
        );
        let faint = check_metal_on_axis(
            &[120.0; 13],
            &[150.0; 13],
            METAL_ON_AXIS_MIN_FRACTION,
            Some(300.0),
        );
        assert_eq!(faint.len(), 1);
        assert!(faint[0].contains("below 300"), "{faint:?}");
        assert_eq!(
            check_metal_on_axis(&[], &[], METAL_ON_AXIS_MIN_FRACTION, None),
            vec!["no samples along the axis to check the metal against".to_owned()]
        );
        assert_eq!(
            check_metal_on_axis(
                &[1.0, f64::NAN],
                &[1.0, 2.0],
                METAL_ON_AXIS_MIN_FRACTION,
                None
            ),
            vec!["non-finite image samples along the axis".to_owned()]
        );
    }

    #[test]
    fn brain_mask_gate_allows_a_margin() {
        let mask = ndarray::Array3::from_shape_fn((40, 40, 40), |(i, j, k)| {
            (10..30).contains(&i) && (10..30).contains(&j) && (10..30).contains(&k)
        });
        let aff: fastwarp_core::Mat4 = [
            [1.0, 0.0, 0.0, -20.0],
            [0.0, 1.0, 0.0, -20.0],
            [0.0, 0.0, 1.0, -20.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert!(
            check_in_brain_mask(&[[0.0, 0.0, 0.0], [-9.0, 9.0, 0.0]], &mask, &aff, 3.0).is_empty()
        );
        let bad = check_in_brain_mask(&[[0.0, 0.0, 0.0], [0.0, 0.0, 18.0]], &mask, &aff, 3.0);
        assert_eq!(bad.len(), 1);
        assert!(
            bad[0].starts_with("contact 1 at [0.0, 0.0, 18.0]"),
            "{bad:?}"
        );
        assert!(bad[0].contains("(margin 3 mm)"), "{bad:?}");
        assert!(check_in_brain_mask(&[[0.0, 0.0, 11.5]], &mask, &aff, 3.0).is_empty());
        assert!(!check_in_brain_mask(&[[0.0, 0.0, 11.5]], &mask, &aff, 0.0).is_empty());
    }

    #[test]
    fn brain_mask_margin_is_a_euclidean_distance() {
        let mask = ndarray::Array3::from_shape_fn((40, 40, 40), |(i, j, k)| {
            (10..30).contains(&i) && (10..30).contains(&j) && (10..30).contains(&k)
        });
        let aff: fastwarp_core::Mat4 = [
            [1.0, 0.0, 0.0, -20.0],
            [0.0, 1.0, 0.0, -20.0],
            [0.0, 0.0, 1.0, -20.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let corner = [9.0, 9.0, 9.0];
        let diagonal = [corner[0] + 2.5, corner[1] + 2.5, corner[2] + 2.5];
        assert_eq!(
            check_in_brain_mask(&[diagonal], &mask, &aff, 3.0).len(),
            1,
            "4.3 mm from the nearest mask voxel is outside a 3 mm margin"
        );
        assert!(check_in_brain_mask(&[[0.0, 11.9, 0.0]], &mask, &aff, 3.0).is_empty());
        let fine: fastwarp_core::Mat4 = [
            [0.5, 0.0, 0.0, -10.0],
            [0.0, 0.5, 0.0, -10.0],
            [0.0, 0.0, 2.0, -40.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert!(check_in_brain_mask(&[[0.0, 0.0, 20.5]], &mask, &fine, 3.0).is_empty());
        assert!(!check_in_brain_mask(&[[0.0, 0.0, 21.5]], &mask, &fine, 3.0).is_empty());
    }

    #[test]
    fn python_style_number_lists() {
        assert_eq!(
            py_list(&[12.345, -4.0, 0.004], Some(2)),
            "[12.34, -4.0, 0.0]"
        );
        assert_eq!(py_list(&[12.35, -0.04, 5.0], Some(1)), "[12.4, -0.0, 5.0]");
        assert_eq!(py_list(&[f64::NAN, 1.5], None), "[nan, 1.5]");
    }

    #[test]
    fn bounds_messages_round_like_numpy() {
        let l = limits_3387();
        let aff: fastwarp_core::Mat4 = [
            [1.0, 0.0, 0.0, -20.0],
            [0.0, 1.0, 0.0, -20.0],
            [0.0, 0.0, 1.0, -20.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let b = WorldBounds::from_grid(&aff, 40, 40, 40, 0.0);
        let far: Vec<[f64; 3]> = (0..4).map(|i| [500.123, 0.0, 3.0 * i as f64]).collect();
        let bad = check_side(far[0], far[3], &far, None, &l, Some(&b));
        assert!(
            bad.iter().any(|m| m
                == "contact 0 at [500.12, 0.0, 0.0] is outside the traced volume ([-20.0, -20.0, -20.0] … [19.0, 19.0, 19.0])"),
            "{bad:?}"
        );
    }

    /// Depth leads have a short first gap and long remaining ones (`specs/09` §2.1 takes
    /// `eldist` from the *second* gap), so one scalar pitch cannot describe them.
    #[test]
    fn non_uniform_gaps_use_per_gap_pitches() {
        let e = crate::electrode_by_name("AdTech BF08R-SP21X").expect("catalogued");
        let l = PlausibilityLimits::for_level_z(&e.level_z_mm, e.lead_diameter_mm);
        assert!(l.expected_pitches_mm.len() >= 2);
        assert!(
            (l.expected_pitches_mm[0] - l.expected_pitches_mm[1]).abs() > 1.0,
            "{:?}",
            l.expected_pitches_mm
        );
        let contacts: Vec<[f64; 3]> = e.contact_xyz_mm.clone();
        let bad = check_side(
            contacts[0],
            contacts[contacts.len() - 1],
            &contacts,
            Some(&e.contact_level),
            &l,
            None,
        );
        assert!(bad.is_empty(), "{bad:?}");
    }
}
