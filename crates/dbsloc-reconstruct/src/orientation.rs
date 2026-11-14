// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use fastwarp_core::{apply_rowmajor, dot3, invert_rowmajor, Mat4};
use ndarray::Array3;
use thiserror::Error;

use crate::electrode_catalog::ElectrodeGeometry;
use crate::math::{add3, cross3, norm3, scale3, sub3, unit3};
use crate::sampling::trilinear_sample_in_bounds;

pub const METHOD: &str = "diode_marker_v1";
pub const FLAG_HIGH_POLAR_ANGLE: &str = "high_polar_angle";
pub const FLAG_COM_UNAVAILABLE: &str = "com_unavailable";
pub const FLAG_AXIS_REFIT_UNAVAILABLE: &str = "axis_refit_unavailable";
pub const AXIS_REFIT_SPAN_MM: f64 = 20.0;

const HIGH_POLAR_ANGLE_DEG: f64 = 55.0;
const HU_MIN: f64 = -1024.0;
const HU_MAX: f64 = 3071.0;
const SLICE_MARGIN_MM: f64 = 1.0;
const RING_R_MIN_MM: f64 = 2.0;
const RING_R_STEP_MM: f64 = 0.25;
const RING_R_COUNT: usize = 17;
const ANGLE_STEPS: usize = 360;
const MIN_HARMONIC: f64 = 1e-9;
const COM_HU: f64 = 2000.0;
const COM_HALF_WIDTH_STEPS: i32 = 50;
const COM_GRID_STEP_MM: f64 = 0.1;
const COM_MIN_OFFSET_MM: f64 = 0.05;
const AXIS_REFIT_STEP_MM: f64 = 0.5;
const AXIS_REFIT_RADIUS_MM: f64 = 3.0;
const AXIS_REFIT_HU: f64 = 2000.0;
const AXIS_REFIT_FALLBACK_HU: f64 = 1000.0;
const AXIS_REFIT_MIN_CENTROIDS: usize = 5;
const AXIS_REFIT_MAX_ANGLE_DEG: f64 = 10.0;
const AXIS_REFIT_POWER_ITERATIONS: usize = 200;
const RESOLVED_MAX_ANGLE_DEG: f64 = 60.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeadFamily {
    Boston,
    Abbott,
    Medtronic,
    Other,
}

pub fn lead_family(model: &ElectrodeGeometry) -> LeadFamily {
    let name = model.elmodel.trim().to_ascii_lowercase();
    if name.starts_with("boston") {
        LeadFamily::Boston
    } else if name.starts_with("abbott") {
        LeadFamily::Abbott
    } else if name.starts_with("medtronic") {
        LeadFamily::Medtronic
    } else {
        LeadFamily::Other
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Supported,
    NotDirected,
    UnsupportedModel,
}

pub fn support(model: &ElectrodeGeometry) -> Support {
    if !model.is_directed {
        return Support::NotDirected;
    }
    let family = lead_family(model);
    let has_marker = model.marker_pos_mm.is_some() && model.marker_len_mm.is_some();
    if matches!(family, LeadFamily::Boston | LeadFamily::Abbott) && has_marker {
        Support::Supported
    } else {
        Support::UnsupportedModel
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerSpec {
    pub marker_offset_mm: f64,
    pub marker_len_mm: f64,
    pub lead_radius_mm: f64,
}

impl MarkerSpec {
    pub fn from_model(model: &ElectrodeGeometry) -> Option<MarkerSpec> {
        Some(MarkerSpec {
            marker_offset_mm: model.marker_offset_mm()?,
            marker_len_mm: model.marker_len_mm?,
            lead_radius_mm: 0.5 * model.lead_diameter_mm,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Resolved,
    Ambiguous,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::Resolved => "resolved",
            Resolution::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    pub status: Resolution,
    pub axis: [f64; 3],
    pub y_dir: [f64; 3],
    pub x_dir: [f64; 3],
    pub candidates: [[f64; 3]; 2],
    pub polar_angle_deg: f64,
    pub marker_slice_mm: f64,
    pub harmonic_amplitude: f64,
    pub com_offset_mm: Option<f64>,
    pub flags: Vec<&'static str>,
}

#[derive(Debug, Error, PartialEq)]
pub enum OrientationError {
    #[error("head and tail coincide")]
    DegenerateLead,
    #[error("the CT voxel-to-world affine is singular")]
    SingularAffine,
    #[error("no CT slice through the marker region lies inside the volume")]
    NoSlice,
    #[error("the angular profile of the marker slices has no two-fold component")]
    FlatProfile,
    #[error("the marker axis is parallel to the lead")]
    ParallelAxis,
}

pub fn roll_from_anterior_deg(u: [f64; 3], y: [f64; 3]) -> f64 {
    let anterior = [0.0, 1.0, 0.0];
    let mut reference = sub3(anterior, scale3(u, dot3(anterior, u)));
    if norm3(reference) < 1e-6 {
        reference = sub3([1.0, 0.0, 0.0], scale3(u, u[0]));
    }
    let reference = unit3(reference);
    let along = dot3(u, cross3(reference, y));
    let across = dot3(reference, y);
    let deg = along.atan2(across).to_degrees();
    if deg <= -180.0 {
        deg + 360.0
    } else {
        deg
    }
}

pub fn roll_azimuth_deg(y: [f64; 3]) -> Option<f64> {
    let horizontal = y[0].hypot(y[1]);
    if !horizontal.is_finite() || horizontal < MIN_HORIZONTAL {
        return None;
    }
    let deg = (-y[0]).atan2(y[1]).to_degrees();
    Some(if deg <= -180.0 {
        deg + 360.0
    } else {
        deg + 0.0
    })
}

pub const FLAG_VERTICAL_MARKER: &str = "vertical_marker";

const MIN_HORIZONTAL: f64 = 1e-6;

pub fn template_marker_directions(
    y_native: [f64; 3],
    head: [f64; 3],
    tail: [f64; 3],
) -> Option<([f64; 3], [f64; 3])> {
    let horizontal = [y_native[0], y_native[1], 0.0];
    let len = norm3(horizontal);
    if len < MIN_HORIZONTAL {
        return None;
    }
    let y_hat = scale3(horizontal, 1.0 / len);
    let shaft = sub3(tail, head);
    if norm3(shaft) < MIN_HORIZONTAL {
        return None;
    }
    let t = unit3(shaft);
    let turned = [y_hat[1], -y_hat[0], 0.0];
    let across = sub3(turned, scale3(t, dot3(turned, t)));
    if norm3(across) < MIN_HORIZONTAL {
        return None;
    }
    let x_dir = unit3(across);
    Some((x_dir, cross3(t, x_dir)))
}

fn column(aff: &Mat4, c: usize) -> [f64; 3] {
    [aff[0][c], aff[1][c], aff[2][c]]
}

fn perpendicular_basis(u: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let helper = if u[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let a = unit3(cross3(u, helper));
    let b = cross3(u, a);
    (a, b)
}

fn slice_centroid(
    data: &Array3<f64>,
    aff: &Mat4,
    inv: &Mat4,
    head: [f64; 3],
    u: [f64; 3],
    k: f64,
    threshold: f64,
) -> Option<[f64; 3]> {
    let dims = data.dim();
    let k_head = apply_rowmajor(inv, head)[2];
    let dk = inv[2][0] * u[0] + inv[2][1] * u[1] + inv[2][2] * u[2];
    if dk.abs() < 1e-9 {
        return None;
    }
    let q = add3(head, scale3(u, (k - k_head) / dk));
    let q_ijk = apply_rowmajor(inv, q);
    let reach = |c: usize| {
        let len = norm3(column(aff, c));
        if len > 1e-9 {
            (AXIS_REFIT_RADIUS_MM / len).ceil() + 1.0
        } else {
            0.0
        }
    };
    let (ri, rj) = (reach(0), reach(1));
    let lo_i = (q_ijk[0] - ri).floor().max(0.0) as usize;
    let hi_i = (q_ijk[0] + ri).ceil().min(dims.0 as f64 - 1.0);
    let lo_j = (q_ijk[1] - rj).floor().max(0.0) as usize;
    let hi_j = (q_ijk[1] + rj).ceil().min(dims.1 as f64 - 1.0);
    if !(hi_i >= 0.0 && hi_j >= 0.0) {
        return None;
    }
    let (hi_i, hi_j, kk) = (hi_i as usize, hi_j as usize, k as usize);
    let mut sum = [0.0f64; 3];
    let mut count = 0usize;
    for i in lo_i..=hi_i {
        for j in lo_j..=hi_j {
            if data[[i, j, kk]] >= threshold {
                let w = apply_rowmajor(aff, [i as f64, j as f64, k]);
                if norm3(sub3(w, q)) <= AXIS_REFIT_RADIUS_MM {
                    sum = add3(sum, w);
                    count += 1;
                }
            }
        }
    }
    (count > 0).then(|| scale3(sum, 1.0 / count as f64))
}

pub fn refit_axis(
    data: &Array3<f64>,
    aff: &Mat4,
    head: [f64; 3],
    u: [f64; 3],
) -> Option<([f64; 3], [f64; 3])> {
    let inv = invert_rowmajor(aff)?;
    let dk = inv[2][0] * u[0] + inv[2][1] * u[1] + inv[2][2] * u[2];
    if dk.abs() < 1e-9 {
        return None;
    }
    let n_slices = data.dim().2 as f64;
    let n_steps = (AXIS_REFIT_SPAN_MM / AXIS_REFIT_STEP_MM).round() as usize;
    let mut slices: Vec<f64> = Vec::new();
    for i in 0..=n_steps {
        let s = i as f64 * AXIS_REFIT_STEP_MM;
        let k = apply_rowmajor(&inv, add3(head, scale3(u, s)))[2].round();
        if k >= 0.0 && k < n_slices && !slices.contains(&k) {
            slices.push(k);
        }
    }
    let at = |threshold: f64| -> Vec<Option<[f64; 3]>> {
        slices
            .iter()
            .map(|&k| slice_centroid(data, aff, &inv, head, u, k, threshold))
            .collect()
    };
    let mut centroids = at(AXIS_REFIT_HU);
    let empty = centroids.iter().filter(|c| c.is_none()).count();
    if 2 * empty > centroids.len() {
        centroids = at(AXIS_REFIT_FALLBACK_HU);
    }
    let points: Vec<[f64; 3]> = centroids.into_iter().flatten().collect();
    if points.len() < AXIS_REFIT_MIN_CENTROIDS {
        return None;
    }
    let mean = scale3(
        points.iter().fold([0.0; 3], |acc, p| add3(acc, *p)),
        1.0 / points.len() as f64,
    );
    let mut cov = [[0.0f64; 3]; 3];
    for p in &points {
        let d = sub3(*p, mean);
        for (r, row) in cov.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell += d[r] * d[c];
            }
        }
    }
    let mut v = u;
    for _ in 0..AXIS_REFIT_POWER_ITERATIONS {
        let next = [dot3(cov[0], v), dot3(cov[1], v), dot3(cov[2], v)];
        let len = norm3(next);
        if !(len.is_finite() && len > 1e-12) {
            return None;
        }
        v = scale3(next, 1.0 / len);
    }
    if dot3(v, u) < 0.0 {
        v = scale3(v, -1.0);
    }
    let angle = dot3(v, u).clamp(-1.0, 1.0).acos().to_degrees();
    if angle.is_nan() || angle > AXIS_REFIT_MAX_ANGLE_DEG {
        return None;
    }
    let h = add3(mean, scale3(v, dot3(sub3(head, mean), v)));
    Some((h, v))
}

fn perpendicular_to(v: [f64; 3], axis: [f64; 3]) -> [f64; 3] {
    let p = sub3(v, scale3(axis, dot3(v, axis)));
    if norm3(p) > 1e-9 {
        unit3(p)
    } else {
        v
    }
}

pub fn estimate_marker_orientation(
    data: &Array3<f64>,
    aff: &Mat4,
    head_in: [f64; 3],
    tail: [f64; 3],
    spec: &MarkerSpec,
) -> Result<Estimate, OrientationError> {
    let axis = sub3(tail, head_in);
    if norm3(axis) < 1e-6 {
        return Err(OrientationError::DegenerateLead);
    }
    let u0 = unit3(axis);
    let inv = invert_rowmajor(aff).ok_or(OrientationError::SingularAffine)?;
    let e1 = unit3(column(aff, 0));
    let n = unit3(column(aff, 2));
    let e2 = cross3(n, e1);
    let polar_angle_deg = dot3(u0, n).abs().min(1.0).acos().to_degrees();
    let mut flags: Vec<&'static str> = Vec::new();
    if polar_angle_deg > HIGH_POLAR_ANGLE_DEG {
        flags.push(FLAG_HIGH_POLAR_ANGLE);
    }
    let (head, u) = match refit_axis(data, aff, head_in, u0) {
        Some(fit) => fit,
        None => {
            flags.push(FLAG_AXIS_REFIT_UNAVAILABLE);
            (head_in, u0)
        }
    };

    let dims = data.dim();
    let sample = |p: [f64; 3]| -> f64 {
        let v = trilinear_sample_in_bounds(data, apply_rowmajor(&inv, p));
        if v.is_nan() {
            HU_MIN
        } else {
            v.clamp(HU_MIN, HU_MAX)
        }
    };

    let k_head = apply_rowmajor(&inv, head)[2];
    let dk = inv[2][0] * u[0] + inv[2][1] * u[1] + inv[2][2] * u[2];
    if dk.abs() < 1e-9 {
        return Err(OrientationError::NoSlice);
    }
    let s_lo = spec.marker_offset_mm - 0.5 * spec.marker_len_mm - SLICE_MARGIN_MM;
    let s_hi = spec.marker_offset_mm + 0.5 * spec.marker_len_mm + SLICE_MARGIN_MM;
    let k_a = k_head + s_lo * dk;
    let k_b = k_head + s_hi * dk;
    let k_first = (k_a.min(k_b) - 1e-9).ceil() as i64;
    let k_last = (k_a.max(k_b) + 1e-9).floor() as i64;

    let trig: Vec<(f64, f64)> = (0..ANGLE_STEPS)
        .map(|j| (j as f64).to_radians().sin_cos())
        .collect();
    let radii: Vec<f64> = (0..RING_R_COUNT)
        .map(|i| RING_R_MIN_MM + RING_R_STEP_MM * i as f64)
        .collect();

    let mut best: Option<(f64, f64, f64)> = None;
    for k in k_first..=k_last {
        if k < 0 || k >= dims.2 as i64 {
            continue;
        }
        let s = (k as f64 - k_head) / dk;
        let centre = add3(head, scale3(u, s));
        let c_ijk = apply_rowmajor(&inv, centre);
        let inside = c_ijk[0] >= 0.0
            && c_ijk[1] >= 0.0
            && c_ijk[0] <= (dims.0 - 1) as f64
            && c_ijk[1] <= (dims.1 - 1) as f64;
        if !inside {
            continue;
        }
        let profile: Vec<f64> = trig
            .iter()
            .map(|&(sin_p, cos_p)| {
                let dir = add3(scale3(e1, cos_p), scale3(e2, sin_p));
                let total: f64 = radii
                    .iter()
                    .map(|&r| sample(add3(centre, scale3(dir, r))))
                    .sum();
                total / radii.len() as f64
            })
            .collect();
        let mean = profile.iter().sum::<f64>() / profile.len() as f64;
        let mut re = 0.0;
        let mut im = 0.0;
        for (j, p) in profile.iter().enumerate() {
            let phi2 = 2.0 * (j as f64).to_radians();
            let dp = p - mean;
            re += dp * phi2.cos();
            im -= dp * phi2.sin();
        }
        let amplitude = 2.0 * re.hypot(im) / ANGLE_STEPS as f64;
        if best.is_none_or(|(a, _, _)| amplitude > a) {
            best = Some((amplitude, im.atan2(re), s));
        }
    }
    let (amplitude, phase, marker_slice_mm) = best.ok_or(OrientationError::NoSlice)?;
    if !amplitude.is_finite() || amplitude < MIN_HARMONIC {
        return Err(OrientationError::FlatProfile);
    }

    let beta = -0.5 * phase;
    let d = add3(scale3(e1, beta.cos()), scale3(e2, beta.sin()));
    let m_raw = sub3(d, scale3(u, dot3(d, u)));
    if norm3(m_raw) < 1e-6 {
        return Err(OrientationError::ParallelAxis);
    }
    let m = unit3(m_raw);

    let c_m = add3(head, scale3(u, marker_slice_mm));
    let (a, b) = perpendicular_basis(u);
    let mut kept = 0usize;
    let mut sum = [0.0f64; 3];
    for i1 in -COM_HALF_WIDTH_STEPS..=COM_HALF_WIDTH_STEPS {
        for i2 in -COM_HALF_WIDTH_STEPS..=COM_HALF_WIDTH_STEPS {
            let off = add3(
                scale3(a, i1 as f64 * COM_GRID_STEP_MM),
                scale3(b, i2 as f64 * COM_GRID_STEP_MM),
            );
            if sample(add3(c_m, off)) >= COM_HU {
                kept += 1;
                sum = add3(sum, off);
            }
        }
    }

    let mut com_offset_mm = None;
    let mut status = Resolution::Ambiguous;
    let mut y_dir = m;
    if kept == 0 {
        flags.push(FLAG_COM_UNAVAILABLE);
    } else {
        let mean = scale3(sum, 1.0 / kept as f64);
        let v = sub3(mean, scale3(u, dot3(mean, u)));
        let v_len = norm3(v);
        com_offset_mm = Some(v_len);
        if v_len < COM_MIN_OFFSET_MM {
            flags.push(FLAG_COM_UNAVAILABLE);
        } else {
            if dot3(m, v) < 0.0 {
                y_dir = scale3(m, -1.0);
            }
            let cos_angle = (dot3(v, y_dir) / v_len).clamp(-1.0, 1.0);
            if cos_angle.acos().to_degrees() < RESOLVED_MAX_ANGLE_DEG {
                status = Resolution::Resolved;
            }
        }
    }
    let y_dir = perpendicular_to(y_dir, u0);
    let x_dir = cross3(y_dir, u0);
    let m0 = perpendicular_to(m, u0);

    Ok(Estimate {
        status,
        axis: u0,
        y_dir,
        x_dir,
        candidates: [m0, scale3(m0, -1.0)],
        polar_angle_deg,
        marker_slice_mm,
        harmonic_amplitude: amplitude,
        com_offset_mm,
        flags,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::electrode_by_name;

    const DIMS: (usize, usize, usize) = (64, 56, 40);
    const SPACING: [f64; 3] = [0.45, 0.45, 1.0];
    const ORIGIN: [f64; 3] = [-14.0, -11.0, -20.0];
    const HEAD: [f64; 3] = [-5.0, -4.0, -8.0];
    const MARKER_OFFSET: f64 = 8.0;
    const MARKER_LEN: f64 = 3.0;

    struct Phantom {
        data: Array3<f64>,
        aff: Mat4,
        head: [f64; 3],
        tail: [f64; 3],
        y_true: [f64; 3],
        u: [f64; 3],
    }

    fn affine() -> Mat4 {
        [
            [SPACING[0], 0.0, 0.0, ORIGIN[0]],
            [0.0, SPACING[1], 0.0, ORIGIN[1]],
            [0.0, 0.0, SPACING[2], ORIGIN[2]],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn rotate_about(v: [f64; 3], axis: [f64; 3], deg: f64) -> [f64; 3] {
        let (s, c) = deg.to_radians().sin_cos();
        add3(
            add3(scale3(v, c), scale3(cross3(axis, v), s)),
            scale3(axis, dot3(axis, v) * (1.0 - c)),
        )
    }

    fn lead_frame(polar_deg: f64, roll_deg: f64) -> ([f64; 3], [f64; 3]) {
        let alpha = polar_deg.to_radians();
        let psi = 35.0f64.to_radians();
        let u = [
            alpha.sin() * psi.cos(),
            alpha.sin() * psi.sin(),
            alpha.cos(),
        ];
        let anterior = [0.0, 1.0, 0.0];
        let reference = unit3(sub3(anterior, scale3(u, dot3(anterior, u))));
        (u, rotate_about(reference, u, roll_deg))
    }

    fn phantom(polar_deg: f64, roll_deg: f64, metal_hu: f64, band: bool) -> Phantom {
        let (u, y_true) = lead_frame(polar_deg, roll_deg);
        let n = [0.0, 0.0, 1.0];
        let proj = sub3(y_true, scale3(u, dot3(y_true, n) / dot3(u, n)));
        let d_true = unit3(proj);
        let tangent = unit3(cross3(n, d_true));
        let aff = affine();
        let metal_at = |p: [f64; 3]| -> bool {
            let q = sub3(p, HEAD);
            let z = dot3(q, u);
            let rho_vec = sub3(q, scale3(u, z));
            let rho = norm3(rho_vec);
            if !(-3.0..=25.0).contains(&z) {
                return false;
            }
            if rho <= 0.65 {
                return true;
            }
            let in_band = (z - MARKER_OFFSET).abs() <= 0.5 * MARKER_LEN;
            if band && in_band && rho <= 0.95 {
                let cos_a = dot3(rho_vec, y_true) / rho;
                return cos_a.clamp(-1.0, 1.0).acos().to_degrees() < 70.0;
            }
            false
        };
        let mut data = Array3::<f64>::zeros(DIMS);
        let offsets = [-1.0 / 3.0, 0.0, 1.0 / 3.0];
        for i in 0..DIMS.0 {
            for j in 0..DIMS.1 {
                for k in 0..DIMS.2 {
                    let p = apply_rowmajor(&aff, [i as f64, j as f64, k as f64]);
                    let q = sub3(p, HEAD);
                    let z = dot3(q, u);
                    let rho = norm3(sub3(q, scale3(u, z)));
                    let frac = if rho < 2.5 {
                        let mut hits = 0;
                        for &oi in &offsets {
                            for &oj in &offsets {
                                for &ok in &offsets {
                                    let sp = [
                                        p[0] + oi * SPACING[0],
                                        p[1] + oj * SPACING[1],
                                        p[2] + ok * SPACING[2],
                                    ];
                                    if metal_at(sp) {
                                        hits += 1;
                                    }
                                }
                            }
                        }
                        hits as f64 / 27.0
                    } else {
                        0.0
                    };
                    let s = (p[2] - HEAD[2]) / u[2];
                    let centre = add3(HEAD, scale3(u, s));
                    let w = sub3(p, centre);
                    let streak = if (s - MARKER_OFFSET).abs() <= 0.5 * MARKER_LEN {
                        let across = dot3(w, d_true);
                        let along = dot3(w, tangent);
                        -600.0
                            * (-(across * across) / (2.0 * 0.6 * 0.6)).exp()
                            * (-along.abs() / 5.0).exp()
                            * (-((s - MARKER_OFFSET) / 0.75).powi(2)).exp()
                    } else {
                        0.0
                    };
                    data[[i, j, k]] = (1.0 - frac) * (30.0 + streak) + frac * metal_hu;
                }
            }
        }
        let tail = add3(HEAD, scale3(u, 6.0));
        Phantom {
            data,
            aff,
            head: HEAD,
            tail,
            y_true,
            u,
        }
    }

    fn spec() -> MarkerSpec {
        MarkerSpec {
            marker_offset_mm: MARKER_OFFSET,
            marker_len_mm: MARKER_LEN,
            lead_radius_mm: 0.65,
        }
    }

    fn angle_diff(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    #[test]
    fn phantom_roll_is_recovered_within_three_degrees() {
        for polar in [0.0, 30.0, 50.0] {
            for roll in [0.0, 75.0, -130.0] {
                let ph = phantom(polar, roll, 3071.0, true);
                let est = estimate_marker_orientation(&ph.data, &ph.aff, ph.head, ph.tail, &spec())
                    .unwrap();
                let got = roll_from_anterior_deg(ph.u, est.y_dir);
                let want = roll_from_anterior_deg(ph.u, ph.y_true);
                let err = angle_diff(got, want);
                assert!(
                    err <= 3.0,
                    "polar {polar} roll {roll}: got {got:.1} want {want:.1} ({:?}, offset {:?})",
                    est.status,
                    est.com_offset_mm
                );
                assert!((est.polar_angle_deg - polar).abs() < 1e-6);
                assert!(est.flags.is_empty(), "{:?}", est.flags);
                let c0 = roll_from_anterior_deg(ph.u, est.candidates[0]);
                let c1 = roll_from_anterior_deg(ph.u, est.candidates[1]);
                assert!((angle_diff(c0, c1) - 180.0).abs() < 1e-6);
                assert!(dot3(est.x_dir, ph.u).abs() < 1e-9);
                assert!(norm3(sub3(est.x_dir, cross3(est.y_dir, ph.u))) < 1e-9);
            }
        }
    }

    const MIRROR_DIMS: (usize, usize, usize) = (120, 76, 48);

    struct LeadTruth {
        head: [f64; 3],
        u: [f64; 3],
        y_true: [f64; 3],
    }

    fn mirror_x(v: [f64; 3]) -> [f64; 3] {
        [-v[0], v[1], v[2]]
    }

    fn lead_truth(head: [f64; 3], u: [f64; 3], roll_deg: f64) -> LeadTruth {
        let anterior = [0.0, 1.0, 0.0];
        let reference = unit3(sub3(anterior, scale3(u, dot3(anterior, u))));
        LeadTruth {
            head,
            u,
            y_true: rotate_about(reference, u, roll_deg),
        }
    }

    fn las_affine(rotation_deg: f64) -> Mat4 {
        let (s, c) = rotation_deg.to_radians().sin_cos();
        let lin = [
            [-SPACING[0] * c, -SPACING[1] * s, 0.0],
            [-SPACING[0] * s, SPACING[1] * c, 0.0],
            [0.0, 0.0, SPACING[2]],
        ];
        let centre = [
            0.5 * (MIRROR_DIMS.0 - 1) as f64,
            0.5 * (MIRROR_DIMS.1 - 1) as f64,
            0.5 * (MIRROR_DIMS.2 - 1) as f64,
        ];
        let mut aff = [[0.0; 4]; 4];
        for r in 0..3 {
            for k in 0..3 {
                aff[r][k] = lin[r][k];
            }
            aff[r][3] = -(lin[r][0] * centre[0] + lin[r][1] * centre[1] + lin[r][2] * centre[2]);
        }
        aff[3][3] = 1.0;
        aff
    }

    fn multi_phantom(aff: &Mat4, leads: &[&LeadTruth]) -> Array3<f64> {
        let n = unit3(column(aff, 2));
        let metal_at = |lead: &LeadTruth, p: [f64; 3]| -> bool {
            let q = sub3(p, lead.head);
            let z = dot3(q, lead.u);
            let rho_vec = sub3(q, scale3(lead.u, z));
            let rho = norm3(rho_vec);
            if !(-3.0..=25.0).contains(&z) {
                return false;
            }
            if rho <= 0.65 {
                return true;
            }
            if (z - MARKER_OFFSET).abs() <= 0.5 * MARKER_LEN && rho <= 0.95 {
                let cos_a = dot3(rho_vec, lead.y_true) / rho;
                return cos_a.clamp(-1.0, 1.0).acos().to_degrees() < 70.0;
            }
            false
        };
        let offsets = [-1.0 / 3.0, 0.0, 1.0 / 3.0];
        Array3::from_shape_fn(MIRROR_DIMS, |(i, j, k)| {
            let p = apply_rowmajor(aff, [i as f64, j as f64, k as f64]);
            let mut near = false;
            let mut streak = 0.0;
            for lead in leads {
                let q = sub3(p, lead.head);
                let rho = norm3(sub3(q, scale3(lead.u, dot3(q, lead.u))));
                near |= rho < 2.5;
                if rho < 8.0 {
                    let proj = sub3(
                        lead.y_true,
                        scale3(lead.u, dot3(lead.y_true, n) / dot3(lead.u, n)),
                    );
                    let d_true = unit3(proj);
                    let tangent = unit3(cross3(n, d_true));
                    let s = dot3(q, n) / dot3(lead.u, n);
                    let w = sub3(q, scale3(lead.u, s));
                    if (s - MARKER_OFFSET).abs() <= 0.5 * MARKER_LEN {
                        let across = dot3(w, d_true);
                        let along = dot3(w, tangent);
                        streak -= 600.0
                            * (-(across * across) / (2.0 * 0.6 * 0.6)).exp()
                            * (-along.abs() / 5.0).exp()
                            * (-((s - MARKER_OFFSET) / 0.75).powi(2)).exp();
                    }
                }
            }
            let frac = if near {
                let mut hits = 0;
                for &oi in &offsets {
                    for &oj in &offsets {
                        for &ok in &offsets {
                            let sp =
                                apply_rowmajor(aff, [i as f64 + oi, j as f64 + oj, k as f64 + ok]);
                            if leads.iter().any(|l| metal_at(l, sp)) {
                                hits += 1;
                            }
                        }
                    }
                }
                hits as f64 / 27.0
            } else {
                0.0
            };
            (1.0 - frac) * (30.0 + streak) + frac * 3071.0
        })
    }

    fn mirror_pair_rolls(rotation_deg: f64, right_roll: f64, left_roll: f64) -> (f64, f64) {
        let aff = las_affine(rotation_deg);
        assert!(column(&aff, 0)[0] < 0.0);
        let u_right = unit3([15.0f64.to_radians().tan(), 20.0f64.to_radians().tan(), 1.0]);
        let right = lead_truth([12.0, 0.0, -8.0], u_right, right_roll);
        let left = lead_truth([-12.0, 0.0, -8.0], mirror_x(u_right), left_roll);
        let data = multi_phantom(&aff, &[&right, &left]);
        let mut got = Vec::new();
        for lead in [&right, &left] {
            let tail = add3(lead.head, scale3(lead.u, 6.0));
            let est = estimate_marker_orientation(&data, &aff, lead.head, tail, &spec()).unwrap();
            let roll = roll_from_anterior_deg(lead.u, est.y_dir);
            let want = roll_from_anterior_deg(lead.u, lead.y_true);
            assert!(
                angle_diff(roll, want) <= 3.0,
                "head x {} rotation {rotation_deg}: got {roll:.1} want {want:.1} ({:?}, offset {:?})",
                lead.head[0],
                est.status,
                est.com_offset_mm
            );
            assert_eq!(est.status, Resolution::Resolved);
            got.push(roll);
        }
        (got[0], got[1])
    }

    #[test]
    fn mirrored_leads_in_a_las_volume_have_opposite_rolls() {
        for rotation in [0.0, 10.0] {
            let (r, l) = mirror_pair_rolls(rotation, 40.0, -40.0);
            assert!(r > 0.0 && l < 0.0, "rotation {rotation}: {r} {l}");
            assert!((r + l).abs() <= 3.0, "rotation {rotation}: {r} {l}");
        }
    }

    #[test]
    fn asymmetric_mirrored_pair_is_recovered_on_both_sides() {
        for rotation in [0.0, 10.0] {
            let (r, l) = mirror_pair_rolls(rotation, 40.0, -110.0);
            assert!(r > 0.0 && l < 0.0, "rotation {rotation}: {r} {l}");
        }
    }

    #[test]
    fn template_markers_equal_native_for_a_vertical_lead() {
        let head = [3.0, -2.0, -5.0];
        let tail = [3.0, -2.0, 1.0];
        let u = [0.0, 0.0, 1.0];
        for roll in [0.0, 40.0, -110.0, 170.0] {
            let y = lead_truth(head, u, roll).y_true;
            let native_x = cross3(y, u);
            let (x_dir, y_dir) = template_marker_directions(y, head, tail).unwrap();
            assert!(norm3(sub3(x_dir, native_x)) < 1e-9, "roll {roll}");
            assert!(norm3(sub3(y_dir, y)) < 1e-9, "roll {roll}");
        }
    }

    #[test]
    fn template_markers_keep_the_native_azimuth_under_a_rotation_about_s() {
        let head = [0.0, 0.0, 0.0];
        let tail_native = scale3(unit3([0.1, 0.15, 1.0]), 6.0);
        let y_native = lead_truth(head, unit3(tail_native), 25.0).y_true;
        let tail_template = rotate_about(tail_native, [0.0, 0.0, 1.0], 30.0);
        let (x_dir, y_dir) = template_marker_directions(y_native, head, tail_template).unwrap();
        let t = unit3(tail_template);
        assert!(dot3(x_dir, t).abs() < 1e-9);
        assert!(norm3(sub3(y_dir, cross3(t, x_dir))) < 1e-9);
        assert!(dot3(cross3(x_dir, y_dir), t) > 0.0);
        let azimuth = |v: [f64; 3]| v[1].atan2(v[0]).to_degrees();
        let kept = azimuth([y_native[1], -y_native[0], 0.0]);
        assert!(angle_diff(azimuth(x_dir), kept) < 3.0);
        assert!(angle_diff(azimuth(x_dir), kept + 30.0) > 20.0);
    }

    #[test]
    fn vertical_marker_or_degenerate_lead_has_no_template_markers() {
        let head = [0.0, 0.0, 0.0];
        let tail = [0.0, 0.0, 6.0];
        assert!(template_marker_directions([0.0, 0.0, 1.0], head, tail).is_none());
        assert!(template_marker_directions([1.0, 0.0, 0.0], head, head).is_none());
    }

    #[test]
    fn missing_centre_of_mass_is_ambiguous_with_a_flag() {
        let ph = phantom(30.0, 75.0, 1500.0, true);
        let est =
            estimate_marker_orientation(&ph.data, &ph.aff, ph.head, ph.tail, &spec()).unwrap();
        assert_eq!(est.status, Resolution::Ambiguous);
        assert!(est.flags.contains(&FLAG_COM_UNAVAILABLE));
        assert!(est.com_offset_mm.is_none());
        let got = roll_from_anterior_deg(ph.u, est.y_dir);
        let want = roll_from_anterior_deg(ph.u, ph.y_true);
        let err = angle_diff(got, want).min(180.0 - angle_diff(got, want));
        assert!(err <= 3.0, "axis error {err}");
    }

    fn rod(head: [f64; 3], u: [f64; 3]) -> Array3<f64> {
        let aff = affine();
        Array3::from_shape_fn(DIMS, |(i, j, k)| {
            let p = apply_rowmajor(&aff, [i as f64, j as f64, k as f64]);
            let q = sub3(p, head);
            let z = dot3(q, u);
            let rho = norm3(sub3(q, scale3(u, z)));
            if rho <= 0.7 && (-3.0..=25.0).contains(&z) {
                3000.0
            } else {
                30.0
            }
        })
    }

    fn distance_to_line(p: [f64; 3], origin: [f64; 3], u: [f64; 3]) -> f64 {
        let q = sub3(p, origin);
        norm3(sub3(q, scale3(u, dot3(q, u))))
    }

    #[test]
    fn axis_refit_recovers_a_tilted_offset_rod() {
        let (u_true, _) = lead_frame(20.0, 0.0);
        let data = rod(HEAD, u_true);
        let side = unit3(cross3(u_true, [0.0, 1.0, 0.0]));
        let u_bad = rotate_about(u_true, unit3(cross3(u_true, side)), 5.0);
        let head_bad = add3(HEAD, scale3(side, 0.8));
        let (h, v) = refit_axis(&data, &affine(), head_bad, u_bad).expect("refit");
        let angle = dot3(v, u_true).clamp(-1.0, 1.0).acos().to_degrees();
        assert!(angle < 1.0, "axis off by {angle:.2} deg");
        let off = distance_to_line(h, HEAD, u_true);
        assert!(off < 0.3, "head {off:.2} mm off the rod axis");
        assert!(dot3(v, u_bad) > 0.0);
        let along = dot3(sub3(h, head_bad), v).abs();
        assert!(along < 1e-9, "head not the nearest point: {along}");
    }

    #[test]
    fn axis_refit_falls_back_without_metal() {
        let empty = Array3::<f64>::zeros(DIMS);
        assert!(refit_axis(&empty, &affine(), HEAD, [0.0, 0.0, 1.0]).is_none());
        let ph = phantom(30.0, 75.0, 900.0, true);
        let est =
            estimate_marker_orientation(&ph.data, &ph.aff, ph.head, ph.tail, &spec()).unwrap();
        assert!(
            est.flags.contains(&FLAG_AXIS_REFIT_UNAVAILABLE),
            "{:?}",
            est.flags
        );
        assert!(est.flags.contains(&FLAG_COM_UNAVAILABLE), "{:?}", est.flags);
        let ok = phantom(30.0, 75.0, 3071.0, true);
        let est =
            estimate_marker_orientation(&ok.data, &ok.aff, ok.head, ok.tail, &spec()).unwrap();
        assert!(
            !est.flags.contains(&FLAG_AXIS_REFIT_UNAVAILABLE),
            "{:?}",
            est.flags
        );
    }

    #[test]
    fn axis_refit_falls_back_to_one_thousand_hounsfield_units() {
        let (u_true, _) = lead_frame(20.0, 0.0);
        let data = rod(HEAD, u_true).mapv(|v| if v > 1000.0 { 1500.0 } else { v });
        let (h, v) = refit_axis(&data, &affine(), HEAD, u_true).expect("1000 HU fallback");
        let angle = dot3(v, u_true).clamp(-1.0, 1.0).acos().to_degrees();
        assert!(angle < 1.0, "axis off by {angle:.2} deg");
        assert!(distance_to_line(h, HEAD, u_true) < 0.3);
        let faint = rod(HEAD, u_true).mapv(|v| if v > 1000.0 { 900.0 } else { v });
        assert!(refit_axis(&faint, &affine(), HEAD, u_true).is_none());
    }

    #[test]
    fn axis_refit_rejects_more_than_ten_degrees() {
        let (u_true, _) = lead_frame(20.0, 0.0);
        let data = rod(HEAD, u_true);
        let side = unit3(cross3(u_true, [0.0, 1.0, 0.0]));
        let u_bad = rotate_about(u_true, unit3(cross3(u_true, side)), 20.0);
        assert!(refit_axis(&data, &affine(), HEAD, u_bad).is_none());
        let (_, v) = refit_axis(&data, &affine(), HEAD, u_true).expect("correct start refits");
        assert!(dot3(v, u_true) > 0.9998);
    }

    fn paint_lump(ph: &mut Phantom, dir: [f64; 3], radius_mm: f64, z_lo: f64, z_hi: f64) {
        let aff = ph.aff;
        for ((i, j, k), v) in ph.data.indexed_iter_mut() {
            let p = apply_rowmajor(&aff, [i as f64, j as f64, k as f64]);
            let q = sub3(p, ph.head);
            let z = dot3(q, ph.u);
            let centre = add3(scale3(ph.u, z), scale3(dir, radius_mm));
            if (z_lo..=z_hi).contains(&z) && norm3(sub3(q, centre)) <= 0.35 {
                *v = 3071.0;
            }
        }
    }

    #[test]
    fn centre_of_mass_uses_one_plane_five_millimetres_wide() {
        let mut ph = phantom(0.0, 45.0, 3071.0, true);
        let band_only = estimate_marker_orientation(&ph.data, &ph.aff, ph.head, ph.tail, &spec())
            .unwrap()
            .com_offset_mm
            .expect("band centre of mass");
        let (a, b) = perpendicular_basis(ph.u);
        let diagonal = unit3(add3(a, b));
        assert!(dot3(diagonal, ph.y_true) > 0.999);
        let band = (
            MARKER_OFFSET - 0.5 * MARKER_LEN,
            MARKER_OFFSET + 0.5 * MARKER_LEN,
        );
        paint_lump(&mut ph, diagonal, 6.5, band.0, band.1);
        let est =
            estimate_marker_orientation(&ph.data, &ph.aff, ph.head, ph.tail, &spec()).unwrap();
        assert_eq!(est.status, Resolution::Resolved, "{:?}", est.flags);
        let offset = est.com_offset_mm.expect("centre of mass");
        assert!(
            offset > band_only + 0.15,
            "offset {offset:.2} mm, band alone {band_only:.2} mm"
        );
        let got = roll_from_anterior_deg(ph.u, est.y_dir);
        let want = roll_from_anterior_deg(ph.u, ph.y_true);
        assert!(angle_diff(got, want) <= 3.0, "got {got:.1} want {want:.1}");
        let axis = roll_from_anterior_deg(ph.u, est.candidates[0]);
        let axis_err = angle_diff(axis, want).min(180.0 - angle_diff(axis, want));
        assert!(axis_err <= 3.0, "harmonic axis off by {axis_err:.1}");

        let s_k = est.marker_slice_mm;
        let off_plane = if s_k + 1.0 <= band.1 {
            s_k + 1.0
        } else {
            s_k - 1.0
        };
        assert!((off_plane - s_k).abs() > 0.5);
        assert!((off_plane - s_k).abs() <= 0.5 * MARKER_LEN);
        let mut decoy = Phantom {
            data: ph.data.clone(),
            aff: ph.aff,
            head: ph.head,
            tail: ph.tail,
            y_true: ph.y_true,
            u: ph.u,
        };
        paint_lump(
            &mut decoy,
            scale3(diagonal, -1.0),
            4.0,
            off_plane - 0.3,
            off_plane + 0.3,
        );
        assert!(decoy.data != ph.data);
        let with_decoy =
            estimate_marker_orientation(&decoy.data, &decoy.aff, decoy.head, decoy.tail, &spec())
                .unwrap();
        assert!((with_decoy.marker_slice_mm - s_k).abs() < 1e-9);
        assert_eq!(with_decoy.status, Resolution::Resolved);
        assert_eq!(with_decoy.flags, est.flags);
        let decoy_offset = with_decoy.com_offset_mm.expect("centre of mass");
        assert!(
            (decoy_offset - offset).abs() < 1e-9,
            "{decoy_offset} vs {offset}"
        );
        assert!(norm3(sub3(with_decoy.candidates[0], est.candidates[0])) < 1e-9);
        assert!(norm3(sub3(with_decoy.y_dir, est.y_dir)) < 1e-9);
    }

    #[test]
    fn steep_polar_angle_adds_a_flag_and_still_computes() {
        let ph = phantom(60.0, 75.0, 3071.0, true);
        let est =
            estimate_marker_orientation(&ph.data, &ph.aff, ph.head, ph.tail, &spec()).unwrap();
        assert!(est.flags.contains(&FLAG_HIGH_POLAR_ANGLE));
        assert!(est.polar_angle_deg > 55.0);
    }

    #[test]
    fn a_lead_inside_the_slice_plane_has_no_slice() {
        let ph = phantom(30.0, 0.0, 3071.0, true);
        let tail = add3(ph.head, [6.0, 0.0, 0.0]);
        assert_eq!(
            estimate_marker_orientation(&ph.data, &ph.aff, ph.head, tail, &spec()).err(),
            Some(OrientationError::NoSlice)
        );
    }

    #[test]
    fn coincident_head_and_tail_are_rejected() {
        let ph = phantom(0.0, 0.0, 3071.0, true);
        assert_eq!(
            estimate_marker_orientation(&ph.data, &ph.aff, ph.head, ph.head, &spec()).err(),
            Some(OrientationError::DegenerateLead)
        );
    }

    #[test]
    fn roll_follows_the_right_hand_rule_about_the_lead() {
        let u = [0.0, 0.0, 1.0];
        assert!(roll_from_anterior_deg(u, [0.0, 1.0, 0.0]).abs() < 1e-9);
        assert!((roll_from_anterior_deg(u, [-1.0, 0.0, 0.0]) - 90.0).abs() < 1e-9);
        assert!((roll_from_anterior_deg(u, [1.0, 0.0, 0.0]) + 90.0).abs() < 1e-9);
        assert!((roll_from_anterior_deg(u, [0.0, -1.0, 0.0]) - 180.0).abs() < 1e-9);
    }

    #[test]
    fn roll_azimuth_is_measured_from_anterior_toward_minus_x() {
        assert_eq!(roll_azimuth_deg([0.0, 1.0, 0.3]), Some(0.0));
        assert_eq!(roll_azimuth_deg([-1.0, 0.0, 0.0]), Some(90.0));
        assert_eq!(roll_azimuth_deg([1.0, 0.0, 0.0]), Some(-90.0));
        assert_eq!(roll_azimuth_deg([0.0, -1.0, 0.0]), Some(180.0));
        assert!(roll_azimuth_deg([0.0, 0.0, 1.0]).is_none());
        assert!(roll_azimuth_deg([f64::NAN, 1.0, 0.0]).is_none());
        let u = [0.0, 0.0, 1.0];
        for deg in [-170.0, -45.0, 0.0, 30.0, 90.0, 179.0] {
            let (s, c) = f64::to_radians(deg).sin_cos();
            let y = [-s, c, 0.0];
            let az = roll_azimuth_deg(y).unwrap();
            assert!((az - roll_from_anterior_deg(u, y)).abs() < 1e-9);
            assert!((az - deg).abs() < 1e-9);
        }
    }

    #[test]
    fn support_follows_the_family_table() {
        let get = |n: &str| electrode_by_name(n).unwrap();
        assert_eq!(
            support(get("Boston Scientific Vercise Directed")),
            Support::Supported
        );
        assert_eq!(
            support(get("Boston Scientific Vercise Cartesia HX")),
            Support::Supported
        );
        assert_eq!(
            support(get("Boston Scientific Vercise Cartesia X")),
            Support::Supported
        );
        assert_eq!(
            support(get("Abbott Directed 6172 (short)")),
            Support::Supported
        );
        assert_eq!(
            support(get("Abbott Directed 6173 (long)")),
            Support::Supported
        );
        assert_eq!(support(get("Medtronic B33005")), Support::UnsupportedModel);
        assert_eq!(support(get("Medtronic B33015")), Support::UnsupportedModel);
        assert_eq!(
            support(get("Aleva directSTIM Directed")),
            Support::UnsupportedModel
        );
        assert_eq!(support(get("Medtronic 3387")), Support::NotDirected);
        assert!(MarkerSpec::from_model(get("Boston Scientific Vercise Directed")).is_some());
    }
}
