// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use dbsloc_reconstruct::native_frame::contacts_from_markers;
use dbsloc_reconstruct::orientation::{roll_azimuth_deg, roll_from_anterior_deg};
use dbsloc_reconstruct::ElectrodeGeometry;

use crate::refine_volume::{RefineVolume, VolumeGeometry};

pub(crate) type V3 = [f64; 3];

pub(crate) const SAMPLE_STEP_MM: f64 = 0.2;
pub(crate) const LEAD_HALF_WIDTH_MM: f64 = 10.0;
pub(crate) const LEAD_PAST_HEAD_MM: f64 = 5.0;
pub(crate) const CLOSEUP_HALF_MM: f64 = 15.0;
pub(crate) const SLAB_OFFSETS_MM: [f64; 9] = [-4.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0];
const MAX_SAMPLES_PER_SIDE: usize = 4096;
const DEGENERATE: f64 = 1e-6;

pub(crate) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub(crate) fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

pub(crate) fn unit(a: V3) -> Option<V3> {
    let n = norm(a);
    if n.is_finite() && n > 0.0 {
        Some(scale(a, 1.0 / n))
    } else {
        None
    }
}

pub(crate) fn project_unit(y: V3, u: V3) -> Option<V3> {
    let p = sub(y, scale(u, dot(y, u)));
    if norm(p) < DEGENERATE {
        None
    } else {
        unit(p)
    }
}

pub(crate) fn wrap_deg(d: f64) -> f64 {
    let mut r = d % 360.0;
    if r <= -180.0 {
        r += 360.0;
    } else if r > 180.0 {
        r -= 360.0;
    }
    r
}

pub(crate) fn a_ref(u: V3) -> V3 {
    let anterior = [0.0, 1.0, 0.0];
    let mut reference = sub(anterior, scale(u, dot(anterior, u)));
    if norm(reference) < DEGENERATE {
        reference = sub([1.0, 0.0, 0.0], scale(u, u[0]));
    }
    unit(reference).unwrap_or([1.0, 0.0, 0.0])
}

pub(crate) fn rotate_about(v: V3, axis: V3, deg: f64) -> V3 {
    let (s, c) = deg.to_radians().sin_cos();
    add(
        add(scale(v, c), scale(cross(axis, v), s)),
        scale(axis, dot(axis, v) * (1.0 - c)),
    )
}

/// `specs/12` §11.5 / `specs/06` §10.6: x̃ = (cos θ, sin θ, 0) made perpendicular to the lead,
/// y = t × x. `None` when x̃ is parallel to the lead.
pub(crate) fn azimuth_y(u: V3, theta_deg: f64) -> Option<V3> {
    let (s, c) = theta_deg.to_radians().sin_cos();
    let x_dir = project_unit([c, s, 0.0], u)?;
    unit(cross(u, x_dir))
}

/// Read-back of `specs/12` §11.5: angle from anterior to the horizontal part of `y`,
/// negative when `y` points to +x. `None` when `y` has no horizontal part.
pub(crate) fn azimuth_of(y: V3) -> Option<f64> {
    roll_azimuth_deg(y)
}

pub(crate) fn roll_y(u: V3, deg: f64) -> V3 {
    azimuth_y(u, deg).unwrap_or_else(|| rotate_about(a_ref(u), u, deg))
}

pub(crate) fn roll_of(u: V3, y: V3) -> f64 {
    azimuth_of(y).unwrap_or_else(|| roll_from_anterior_deg(u, y))
}

/// Moves `moved` towards `requested` while `|moved − other|` stays fixed: the component of
/// `requested − other` across the current lead is kept, the one along it recomputed on the
/// same side of `other` (`specs/12` §11.2). The flag is true when the request was clamped.
pub(crate) fn pivot_keep_length(moved: V3, other: V3, requested: V3) -> Option<(V3, bool)> {
    let shaft = sub(moved, other);
    let len = norm(shaft);
    let a_hat = unit(shaft)?;
    let d = sub(requested, other);
    let mut across = sub(d, scale(a_hat, dot(d, a_hat)));
    let qn = norm(across);
    let clamped = qn > len;
    if clamped {
        across = scale(across, len / qn);
    }
    let along = (len * len - dot(across, across)).max(0.0).sqrt();
    Some((add(other, add(across, scale(a_hat, along))), clamped))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LeadView {
    Front,
    Left,
}

impl LeadView {
    pub(crate) fn title(self) -> &'static str {
        match self {
            LeadView::Front => "Lead from the front",
            LeadView::Left => "Lead from the left",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlaneFrame {
    pub origin: V3,
    pub right: V3,
    pub up: V3,
    pub normal: V3,
    pub u_range: (f64, f64),
    pub v_range: (f64, f64),
}

impl PlaneFrame {
    pub(crate) fn project(&self, p: V3) -> (f64, f64, f64) {
        let d = sub(p, self.origin);
        (dot(d, self.right), dot(d, self.up), dot(d, self.normal))
    }

    pub(crate) fn world(&self, u: f64, v: f64) -> V3 {
        add(self.origin, add(scale(self.right, u), scale(self.up, v)))
    }

    pub(crate) fn extent(&self) -> (f64, f64) {
        (
            self.u_range.1 - self.u_range.0,
            self.v_range.1 - self.v_range.0,
        )
    }
}

pub(crate) fn trajectory_extension_mm(electrode: &ElectrodeGeometry) -> f64 {
    let k = if electrode.n_contacts > 8 { 1.5 } else { 2.0 };
    k * electrode.contact_span_mm().max(0.0)
}

/// The plane through the lead axis and world x (seen from anterior) or world y (seen from the
/// left), `specs/12` §10: long axis = head → tail (up), head − 5 mm … tail + k · span, ±10 mm.
pub(crate) fn lead_frame(
    view: LeadView,
    head: V3,
    tail: V3,
    electrode: &ElectrodeGeometry,
) -> Option<PlaneFrame> {
    let span = norm(sub(tail, head));
    if !span.is_finite() || span < DEGENERATE {
        return None;
    }
    let up = unit(sub(tail, head))?;
    let (axis, fallback) = match view {
        LeadView::Front => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        LeadView::Left => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    };
    let lateral = project_unit(axis, up).or_else(|| project_unit(fallback, up))?;
    let right = scale(lateral, -1.0);
    let normal = cross(right, up);
    Some(PlaneFrame {
        origin: head,
        right,
        up,
        normal,
        u_range: (-LEAD_HALF_WIDTH_MM, LEAD_HALF_WIDTH_MM),
        v_range: (
            -LEAD_PAST_HEAD_MM,
            span + trajectory_extension_mm(electrode),
        ),
    })
}

/// Axial close-up: the backdrop's own i / j voxel plane through `centre`, ±15 mm.
pub(crate) fn closeup_frame(geometry: &VolumeGeometry, centre: V3) -> PlaneFrame {
    let right = geometry.axis_direction(0);
    let up = project_unit(geometry.axis_direction(1), right)
        .or_else(|| project_unit([0.0, 1.0, 0.0], right))
        .unwrap_or([0.0, 1.0, 0.0]);
    PlaneFrame {
        origin: centre,
        right,
        up,
        normal: cross(right, up),
        u_range: (-CLOSEUP_HALF_MM, CLOSEUP_HALF_MM),
        v_range: (-CLOSEUP_HALF_MM, CLOSEUP_HALF_MM),
    }
}

/// Samples `frame` every `step` mm (row 0 = top, the largest v) as the mean of the planes at
/// `offsets` mm along the frame normal; outside the volume the sample is NaN.
pub(crate) fn sample_frame(
    vol: &RefineVolume,
    frame: &PlaneFrame,
    step: f64,
    offsets: &[f64],
    out: &mut Vec<f32>,
) -> (usize, usize) {
    let (ew, eh) = frame.extent();
    let count = |e: f64| ((e / step.max(1e-6)).round() as usize).clamp(1, MAX_SAMPLES_PER_SIDE);
    let (w, h) = (count(ew), count(eh));
    let du = ew / w as f64;
    let dv = eh / h as f64;
    let g = vol.geometry();
    let di = g.world_vector_to_voxel(scale(frame.right, du));
    let dj = g.world_vector_to_voxel(scale(frame.up, -dv));
    let dn = g.world_vector_to_voxel(frame.normal);
    let start =
        g.world_to_voxel(frame.world(frame.u_range.0 + 0.5 * du, frame.v_range.1 - 0.5 * dv));
    out.clear();
    out.resize(w * h, 0.0);
    let offsets: &[f64] = if offsets.is_empty() { &[0.0] } else { offsets };
    for r in 0..h {
        let row = add(start, scale(dj, r as f64));
        for c in 0..w {
            let p = add(row, scale(di, c as f64));
            let mut acc = 0.0f32;
            let mut n = 0u32;
            for &o in offsets {
                let v = vol.sample_trilinear(add(p, scale(dn, o)));
                if v.is_finite() {
                    acc += v;
                    n += 1;
                }
            }
            out[r * w + c] = if n == 0 { f32::NAN } else { acc / n as f32 };
        }
    }
    (w, h)
}

pub(crate) fn window_rgba(raw: &[f32], center: f32, width: f32, out: &mut Vec<u8>) {
    let width = width.max(1e-3);
    let lo = center - 0.5 * width;
    out.clear();
    out.reserve(raw.len() * 4);
    for &v in raw {
        let g = if v.is_finite() {
            (((v - lo) / width).clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            0
        };
        out.extend_from_slice(&[g, g, g, 255]);
    }
}

pub(crate) fn catalogue_spacing_mm(electrode: &ElectrodeGeometry) -> Option<f64> {
    let n = electrode.n_levels();
    if n < 2 {
        return None;
    }
    let s = electrode.contact_span_mm() / (n - 1) as f64;
    (s.is_finite() && s > 0.0).then_some(s)
}

fn mean_level_spacing(electrode: &ElectrodeGeometry, contacts: &[V3]) -> Option<f64> {
    let n = electrode.n_levels();
    if n < 2 || contacts.len() != electrode.contact_level.len() {
        return None;
    }
    let mut sums = vec![[0.0; 3]; n];
    let mut counts = vec![0usize; n];
    for (c, &level) in contacts.iter().zip(&electrode.contact_level) {
        if level >= n {
            return None;
        }
        sums[level] = add(sums[level], *c);
        counts[level] += 1;
    }
    if counts.contains(&0) {
        return None;
    }
    let centres: Vec<V3> = sums
        .iter()
        .zip(&counts)
        .map(|(s, &k)| scale(*s, 1.0 / k as f64))
        .collect();
    let total: f64 = centres.windows(2).map(|p| norm(sub(p[1], p[0]))).sum();
    Some(total / (n - 1) as f64)
}

/// Mean distance between neighbouring level centres of the contacts the markers imply
/// (`specs/05` §6 marker affine, stretched along the lead), `specs/12` §10 *Electrode Spacing*.
pub(crate) fn live_spacing_mm(
    electrode: &ElectrodeGeometry,
    head: V3,
    tail: V3,
    y_dir: V3,
) -> Option<f64> {
    let u = unit(sub(tail, head))?;
    let r = 0.5 * electrode.lead_diameter_mm;
    let x_dir = cross(y_dir, u);
    let contacts = contacts_from_markers(
        electrode,
        head,
        tail,
        add(head, scale(x_dir, r)),
        add(head, scale(y_dir, r)),
    )?;
    mean_level_spacing(electrode, &contacts)
}

pub(crate) fn caption(dir: V3) -> &'static str {
    let (mut axis, mut best) = (0usize, dir[0].abs());
    for (a, v) in dir.iter().enumerate().skip(1) {
        if v.abs() > best {
            axis = a;
            best = v.abs();
        }
    }
    match (axis, dir[axis] >= 0.0) {
        (0, true) => "R",
        (0, false) => "L",
        (1, true) => "A",
        (1, false) => "P",
        (_, true) => "S",
        (_, false) => "I",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: V3, b: V3, tol: f64) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < tol)
    }

    #[test]
    fn azimuth_round_trip_and_vertical_agreement() {
        let vertical = [0.0, 0.0, 1.0];
        for theta in [-170.0, -45.0, 0.0, 12.5, 90.0, 179.0] {
            let y = azimuth_y(vertical, theta).unwrap();
            assert!((azimuth_of(y).unwrap() - theta).abs() < 1e-9);
            assert!(wrap_deg(roll_from_anterior_deg(vertical, y) - theta).abs() < 1e-9);
        }
        let tilted = unit([0.3, -0.2, 1.0]).unwrap();
        for theta in [-120.0, 0.0, 33.0, 150.0] {
            let y = azimuth_y(tilted, theta).unwrap();
            assert!(dot(y, tilted).abs() < 1e-12);
            assert!((azimuth_of(y).unwrap() - theta).abs() < 1e-9);
        }
        assert!(close(
            azimuth_y(vertical, 90.0).unwrap(),
            [-1.0, 0.0, 0.0],
            1e-12
        ));
    }

    #[test]
    fn pivot_keeps_length_and_across_component() {
        let other = [1.0, -2.0, 10.0];
        let moved = [0.5, -1.0, 0.0];
        let len = norm(sub(moved, other));
        let a_hat = unit(sub(moved, other)).unwrap();
        let requested = add(moved, [0.7, -0.4, 0.3]);
        let (p, clamped) = pivot_keep_length(moved, other, requested).unwrap();
        assert!(!clamped);
        assert!((norm(sub(p, other)) - len).abs() < 1e-12);
        let across = |q: V3| {
            let d = sub(q, other);
            sub(d, scale(a_hat, dot(d, a_hat)))
        };
        assert!(close(across(p), across(requested), 1e-12));
        let (far, clamped) = pivot_keep_length(moved, other, add(moved, [50.0, 0.0, 0.0])).unwrap();
        assert!(clamped);
        assert!((norm(sub(far, other)) - len).abs() < 1e-9);
    }

    #[test]
    fn captions_follow_the_dominant_axis() {
        assert_eq!(caption([0.9, 0.1, 0.0]), "R");
        assert_eq!(caption([-0.9, 0.1, 0.0]), "L");
        assert_eq!(caption([0.1, 0.8, 0.3]), "A");
        assert_eq!(caption([0.1, -0.8, 0.3]), "P");
        assert_eq!(caption([0.1, 0.2, -0.9]), "I");
    }

    #[test]
    fn trilinear_sample_is_exact_on_a_linear_ramp() {
        let dims = [6usize, 5, 4];
        let affine = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut data = Vec::new();
        for k in 0..dims[2] {
            for j in 0..dims[1] {
                for i in 0..dims[0] {
                    data.push((i + 10 * j + 100 * k) as f32);
                }
            }
        }
        let vol = RefineVolume::from_parts(dims, affine, data).unwrap();
        let v = vol.sample_trilinear([1.25, 2.5, 1.75]);
        assert!((v - (1.25 + 25.0 + 175.0)).abs() < 1e-3);
        assert!(vol.sample_trilinear([-0.6, 1.0, 1.0]).is_nan());
        assert!(vol.sample_trilinear([5.4, 1.0, 1.0]).is_finite());
    }
}
