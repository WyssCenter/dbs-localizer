// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use ndarray::Array3;

use crate::electrode_catalog::ElectrodeGeometry;
use crate::kabsch::{apply_rigid_r_rows, estimate_rigid_kabsch, KabschError};
use crate::math::{norm3, scale3, sub3, unit3};
use crate::metal_axis::{
    core_window, metal_continuity, metal_voxels, ransac_line, refine_metal_axis, RansacParams,
    RefineParams, CORE_MARGIN_ABOVE_MM, CORE_MARGIN_BELOW_MM, LEAD_BODY_HU, MIN_STEEP_CONTINUITY,
    RANSAC_MIN_POINTS, STEEP_CONTINUITY_MM,
};
use crate::plausibility::{
    check_metal_on_axis, check_side, PlausibilityLimits, WorldBounds, METAL_ON_AXIS_MIN_FRACTION,
};
use crate::ring::ring_in_plane;
use crate::sampling::axis_and_disc_samples_in_bounds;
use crate::trac::{core_on_line, trace_fiducials_flat, TracConfig};
use fastwarp_core::{apply_rowmajor, dot3, mat4_mul, Mat4};

pub const MIN_METAL_ON_AXIS_HU: f64 = 300.0;
pub const FRAME_LATTICE_PER_AXIS: usize = 3;
pub const FRAME_CENTRAL_FRACTION: f64 = 0.5;
pub const SIDE_LABELS: [&str; 2] = ["left", "right"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeFrame {
    pub r: [[f64; 3]; 3],
    pub t: [f64; 3],
    pub scale: f64,
    pub fit_rms_mm: f64,
}

impl NativeFrame {
    pub fn identity() -> Self {
        NativeFrame {
            r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            t: [0.0; 3],
            scale: 1.0,
            fit_rms_mm: 0.0,
        }
    }

    pub fn matrix(&self) -> Mat4 {
        let mut m = fastwarp_core::identity_mat4();
        for (row, (rot, t)) in m.iter_mut().zip(self.r.iter().zip(self.t.iter())) {
            row[..3].copy_from_slice(rot);
            row[3] = *t;
        }
        m
    }

    pub fn to_frame(&self, p: [f64; 3]) -> [f64; 3] {
        apply_rigid_r_rows(&self.r, &self.t, &[p])[0]
    }

    pub fn to_native(&self, p: [f64; 3]) -> [f64; 3] {
        let d = sub3(p, self.t);
        let mut out = [0.0f64; 3];
        for (c, o) in out.iter_mut().enumerate() {
            *o = d[0] * self.r[0][c] + d[1] * self.r[1][c] + d[2] * self.r[2][c];
        }
        out
    }
}

fn mean3(p: &[[f64; 3]]) -> [f64; 3] {
    let mut c = [0.0f64; 3];
    for q in p {
        for a in 0..3 {
            c[a] += q[a];
        }
    }
    let n = p.len().max(1) as f64;
    [c[0] / n, c[1] / n, c[2] / n]
}

pub fn rigid_frame_from_points(
    native_pts: &[[f64; 3]],
    frame_pts: &[[f64; 3]],
) -> Result<NativeFrame, KabschError> {
    let (r, t) = estimate_rigid_kabsch(native_pts, frame_pts)?;
    let c_s = mean3(native_pts);
    let c_d = mean3(frame_pts);
    let var = |p: &[[f64; 3]], c: [f64; 3]| -> f64 {
        p.iter()
            .map(|q| {
                let d = sub3(*q, c);
                d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
            })
            .sum()
    };
    let var_s = var(native_pts, c_s);
    let var_d = var(frame_pts, c_d);
    let scale = if var_s > 1e-12 {
        (var_d / var_s).sqrt()
    } else {
        1.0
    };
    let mut sq = 0.0;
    for (s, d) in native_pts.iter().zip(frame_pts.iter()) {
        let x = sub3(*s, c_s);
        let mut res = [0.0f64; 3];
        for (a, ra) in res.iter_mut().enumerate() {
            *ra = scale * (x[0] * r[a][0] + x[1] * r[a][1] + x[2] * r[a][2]) + c_d[a] - d[a];
        }
        sq += res[0] * res[0] + res[1] * res[1] + res[2] * res[2];
    }
    let rms = if native_pts.is_empty() {
        0.0
    } else {
        (sq / native_pts.len() as f64).sqrt()
    };
    Ok(NativeFrame {
        r,
        t,
        scale,
        fit_rms_mm: rms,
    })
}

pub fn world_bounds_corners(aff: &Mat4, dims: [usize; 3]) -> ([f64; 3], [f64; 3]) {
    let b = WorldBounds::from_grid(aff, dims[0], dims[1], dims[2], 0.0);
    (b.min, b.max)
}

pub fn frame_lattice(
    lo: [f64; 3],
    hi: [f64; 3],
    n_per_axis: usize,
    central_fraction: f64,
) -> Vec<[f64; 3]> {
    let n = n_per_axis.max(2);
    let ticks: Vec<f64> = (0..n)
        .map(|i| -1.0 + 2.0 * i as f64 / (n - 1) as f64)
        .collect();
    let centre = [
        0.5 * (lo[0] + hi[0]),
        0.5 * (lo[1] + hi[1]),
        0.5 * (lo[2] + hi[2]),
    ];
    let half = [
        0.5 * central_fraction * (hi[0] - lo[0]),
        0.5 * central_fraction * (hi[1] - lo[1]),
        0.5 * central_fraction * (hi[2] - lo[2]),
    ];
    let mut out = Vec::with_capacity(n * n * n);
    for &a in &ticks {
        for &b in &ticks {
            for &c in &ticks {
                out.push([
                    centre[0] + half[0] * a,
                    centre[1] + half[1] * b,
                    centre[2] + half[2] * c,
                ]);
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct SideGeometry {
    pub coords_mm: Vec<[f64; 3]>,
    pub trajectory: Vec<[f64; 3]>,
    pub head: [f64; 3],
    pub tail: [f64; 3],
    pub x: [f64; 3],
    pub y: [f64; 3],
    pub trajvector: [f64; 3],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeometryError {
    HeadTailCoincide,
    Kabsch(KabschError),
}

impl std::fmt::Display for GeometryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeometryError::HeadTailCoincide => f.write_str("head and tail coincide"),
            GeometryError::Kabsch(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for GeometryError {}

pub use crate::electrode_catalog::TRAJECTORY_POINTS;

pub fn trajectory_from_anchors(
    head: [f64; 3],
    tail: [f64; 3],
    electrode: &ElectrodeGeometry,
) -> Result<(Vec<[f64; 3]>, [f64; 3]), GeometryError> {
    let traj = electrode
        .trajectory(head, tail)
        .ok_or(GeometryError::HeadTailCoincide)?;
    Ok((traj, unit3(sub3(tail, head))))
}

pub fn resolve_fiducials(
    fids: [[f64; 3]; 4],
    electrode: &ElectrodeGeometry,
) -> Result<SideGeometry, GeometryError> {
    let [head, tail, x_raw, _y_raw] = fids;
    let (x, y) = ring_in_plane(
        head,
        tail,
        Some(sub3(x_raw, head)),
        electrode.lead_diameter_mm,
    );
    let src = electrode.model_fiducials();
    let dst = [head, tail, x, y];
    let (r, t) = estimate_rigid_kabsch(&src, &dst).map_err(GeometryError::Kabsch)?;
    let coords_mm = apply_rigid_r_rows(&r, &t, &electrode.contact_xyz_mm);
    let (trajectory, trajvector) = trajectory_from_anchors(head, tail, electrode)?;
    Ok(SideGeometry {
        coords_mm,
        trajectory,
        head,
        tail,
        x,
        y,
        trajvector,
    })
}

pub const MARKER_FRAME_TOLERANCE: f64 = 0.25;

pub fn contacts_from_markers(
    electrode: &ElectrodeGeometry,
    head: [f64; 3],
    tail: [f64; 3],
    x: [f64; 3],
    y: [f64; 3],
) -> Option<Vec<[f64; 3]>> {
    let r = 0.5 * electrode.lead_diameter_mm;
    let shaft = sub3(tail, head);
    let span = norm3(shaft);
    if !(r.is_finite() && r > 0.0 && span.is_finite() && span > 1e-6) {
        return None;
    }
    let u = scale3(shaft, 1.0 / span);
    let xh = scale3(sub3(x, head), 1.0 / r);
    let yh = scale3(sub3(y, head), 1.0 / r);
    let finite = |v: [f64; 3]| v.iter().all(|c| c.is_finite());
    if !(finite(xh) && finite(yh)) {
        return None;
    }
    let off_axis = [
        dot3(xh, u).abs(),
        dot3(yh, u).abs(),
        dot3(unit3(xh), unit3(yh)).abs(),
    ];
    if off_axis.iter().any(|d| *d > MARKER_FRAME_TOLERANCE)
        || [norm3(xh), norm3(yh)]
            .iter()
            .any(|n| !(0.5..=2.0).contains(n))
    {
        return None;
    }
    let model_span = electrode.fiducial_span_mm();
    let s = if model_span.is_finite() && model_span.abs() > 1e-9 {
        span / model_span
    } else {
        1.0
    };
    let z_head = electrode.model_fiducials()[0][2];
    Some(
        electrode
            .contact_xyz_mm
            .iter()
            .map(|p| {
                let q = [p[0], p[1], s * (p[2] - z_head)];
                [
                    head[0] + xh[0] * q[0] + yh[0] * q[1] + u[0] * q[2],
                    head[1] + xh[1] * q[0] + yh[1] * q[1] + u[1] * q[2],
                    head[2] + xh[2] * q[0] + yh[2] * q[1] + u[2] * q[2],
                ]
            })
            .collect(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideSource {
    TracCore,
    MetalAxis,
}

impl SideSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            SideSource::TracCore => "trac_core",
            SideSource::MetalAxis => "metal_axis",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChosenSide {
    pub label: &'static str,
    pub source: SideSource,
    pub continuity: Option<f64>,
    pub ransac_voxels: Option<usize>,
    pub geometry: SideGeometry,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeSides {
    pub left: Option<ChosenSide>,
    pub right: Option<ChosenSide>,
    pub failed: Vec<(&'static str, String)>,
    pub side_notes: Vec<(&'static str, String)>,
}

impl NativeSides {
    pub fn chosen(&self) -> impl Iterator<Item = &ChosenSide> {
        self.left.iter().chain(self.right.iter())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeTraceError {
    SideFailed(Vec<(&'static str, String)>),
    Implausible(Vec<String>),
    Geometry(String),
}

impl std::fmt::Display for NativeTraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeTraceError::SideFailed(failed) => {
                let parts: Vec<&str> = failed.iter().map(|(_, m)| m.as_str()).collect();
                f.write_str(&parts.join(" | "))
            }
            NativeTraceError::Geometry(m) => f.write_str(m),
            NativeTraceError::Implausible(p) => write!(f, "{}", implausible_message(p)),
        }
    }
}

impl std::error::Error for NativeTraceError {}

pub fn implausible_message(problems: &[String]) -> String {
    format!(
        "E_RECON_implausible: refusing to write {} problem(s): {}",
        problems.len(),
        problems.join(" | ")
    )
}

#[derive(Clone, Copy, Debug)]
pub struct NativeTraceParams {
    pub distal_bloom_mm: f64,
    pub world_to_reference: Option<Mat4>,
    pub traced: [bool; 2],
}

impl Default for NativeTraceParams {
    fn default() -> Self {
        NativeTraceParams {
            distal_bloom_mm: TracConfig::default().distal_bloom_mm,
            world_to_reference: None,
            traced: [true, true],
        }
    }
}

struct Candidate {
    label: &'static str,
    source: SideSource,
    fids_frame: [[f64; 3]; 4],
    continuity: Option<f64>,
    ransac_voxels: Option<usize>,
}

pub fn seed_scaled_trac_config(scale: f64, distal_bloom_mm: f64) -> TracConfig {
    let mut cfg = TracConfig::default();
    let k = 1.0 / scale;
    if (k - 1.0).abs() > 1e-9 {
        cfg.start_z_mm *= k;
        cfg.end_z_mm *= k;
        cfg.seed_abs_x_mm *= k;
        cfg.seed_abs_y_mm *= k;
    }
    cfg.distal_bloom_mm = distal_bloom_mm;
    cfg
}

#[allow(clippy::too_many_arguments)]
fn metal_axis_candidate(
    data: &Array3<f64>,
    aff_frame: &Mat4,
    vol: &Array3<f64>,
    hemi: u8,
    frame: &NativeFrame,
    electrode: &ElectrodeGeometry,
    metal_pts: &[[f64; 3]],
    metal_vals: &[f64],
    params: &NativeTraceParams,
) -> Result<Candidate, String> {
    let k = 1.0 / frame.scale;
    let want_left = hemi == 1;
    let mut pts = Vec::new();
    let mut vals = Vec::new();
    for (p0, v) in metal_pts.iter().zip(metal_vals.iter()) {
        let p = match params.world_to_reference {
            Some(m) => apply_rowmajor(&m, *p0),
            None => *p0,
        };
        let in_box = p[0].abs() < 45.0 * k
            && p[1] > -60.0 * k
            && p[1] < 45.0 * k
            && p[2] > -25.0 * k
            && p[2] < 70.0 * k
            && ((p[0] < 0.0) == want_left);
        if in_box {
            pts.push(*p0);
            vals.push(*v);
        }
    }
    let Some(seed) = ransac_line(&pts, &RansacParams::default()) else {
        return Err(format!(
            "fewer than {RANSAC_MIN_POINTS} voxels at or above {LEAD_BODY_HU:.0} HU in the hemisphere"
        ));
    };
    let Some(axis) = refine_metal_axis(&pts, &vals, seed.deep, seed.u, &RefineParams::default())
    else {
        return Err("metal-axis refinement lost the RANSAC line".to_owned());
    };
    let (deep, shallow) = core_window(&axis, electrode, CORE_MARGIN_BELOW_MM, CORE_MARGIN_ABOVE_MM)
        .map_err(|e| e.to_string())?;
    let cfg = TracConfig {
        distal_bloom_mm: params.distal_bloom_mm,
        ..TracConfig::default()
    };
    let r = core_on_line(vol, aff_frame, deep, shallow, true, electrode, &cfg)
        .map_err(|e| format!("CORE on the metal axis: {e}"))?;
    let cont = metal_continuity(
        data,
        aff_frame,
        r.head,
        axis.u,
        STEEP_CONTINUITY_MM,
        LEAD_BODY_HU,
    );
    Ok(Candidate {
        label: "",
        source: SideSource::MetalAxis,
        fids_frame: [r.head, r.tail, r.x, r.y],
        continuity: Some(cont),
        ransac_voxels: Some(seed.n_voxels),
    })
}

fn refine_trac_fiducials(
    vol: &Array3<f64>,
    aff_frame: &Mat4,
    fids: &[[f64; 3]; 4],
    metal_pts: &[[f64; 3]],
    metal_vals: &[f64],
    electrode: &ElectrodeGeometry,
    cfg: &TracConfig,
) -> Option<[[f64; 3]; 4]> {
    if metal_pts.is_empty() {
        return None;
    }
    let u = unit3(sub3(fids[1], fids[0]));
    let start = sub3(fids[0], scale3(u, 5.0));
    let axis = refine_metal_axis(metal_pts, metal_vals, start, u, &RefineParams::default())?;
    let (deep, shallow) =
        core_window(&axis, electrode, CORE_MARGIN_BELOW_MM, CORE_MARGIN_ABOVE_MM).ok()?;
    let r = core_on_line(vol, aff_frame, deep, shallow, true, electrode, cfg).ok()?;
    Some([r.head, r.tail, r.x, r.y])
}

pub fn metal_axis_corroborated(continuity: Option<f64>) -> Result<(), String> {
    let cont = continuity.unwrap_or(0.0);
    if cont < MIN_STEEP_CONTINUITY {
        return Err(format!(
            "only {:.0}% of the {STEEP_CONTINUITY_MM:.0} mm above contact 0 reads as lead metal \
             (>= {LEAD_BODY_HU:.0} HU; need {:.0}%)",
            cont * 100.0,
            MIN_STEEP_CONTINUITY * 100.0
        ));
    }
    Ok(())
}

pub fn trace_native_sides(
    data: &Array3<f64>,
    aff: &Mat4,
    frame: &NativeFrame,
    electrode: &ElectrodeGeometry,
    params: &NativeTraceParams,
) -> Result<NativeSides, NativeTraceError> {
    let aff_frame = mat4_mul(&frame.matrix(), aff);
    let vol = data.mapv(|v| -v);
    let (metal_pts, metal_vals) = {
        let (w, v) = metal_voxels(data, aff, LEAD_BODY_HU);
        (w.iter().map(|p| frame.to_frame(*p)).collect::<Vec<_>>(), v)
    };
    let mut trac_cfg = seed_scaled_trac_config(frame.scale, params.distal_bloom_mm);
    trac_cfg.world_to_reference = params.world_to_reference;

    let mut chosen: Vec<Candidate> = Vec::with_capacity(2);
    let mut failed: Vec<(&'static str, String)> = Vec::new();
    let mut side_notes: Vec<(&'static str, String)> = Vec::new();
    for (index, (hemi, label)) in [(1u8, SIDE_LABELS[0]), (2u8, SIDE_LABELS[1])]
        .into_iter()
        .enumerate()
    {
        if !params.traced[index] {
            continue;
        }
        let trac_err =
            match trace_fiducials_flat(&vol, &aff_frame, hemi, true, electrode, &trac_cfg) {
                Ok(f) => {
                    let trac_fids = [
                        [f[0], f[1], f[2]],
                        [f[3], f[4], f[5]],
                        [f[6], f[7], f[8]],
                        [f[9], f[10], f[11]],
                    ];
                    let fids_frame = refine_trac_fiducials(
                        &vol,
                        &aff_frame,
                        &trac_fids,
                        &metal_pts,
                        &metal_vals,
                        electrode,
                        &trac_cfg,
                    )
                    .unwrap_or(trac_fids);
                    chosen.push(Candidate {
                        label,
                        source: SideSource::TracCore,
                        fids_frame,
                        continuity: None,
                        ransac_voxels: None,
                    });
                    continue;
                }
                Err(e) => e.to_string(),
            };
        let metal = metal_axis_candidate(
            data,
            &aff_frame,
            &vol,
            hemi,
            frame,
            electrode,
            &metal_pts,
            &metal_vals,
            params,
        );
        let cand = match metal {
            Ok(c) => Candidate { label, ..c },
            Err(metal_err) => {
                failed.push((
                    label,
                    format!("{label}: TRAC/CORE: {trac_err}; metal axis: {metal_err}"),
                ));
                continue;
            }
        };
        if let Err(why) = metal_axis_corroborated(cand.continuity) {
            failed.push((
                label,
                format!("{label}: TRAC/CORE: {trac_err}; metal axis not corroborated: {why}"),
            ));
            continue;
        }
        side_notes.push((
            label,
            format!("TRAC/CORE could not trace this side ({trac_err})"),
        ));
        chosen.push(cand);
    }
    if chosen.is_empty() && !failed.is_empty() {
        return Err(NativeTraceError::SideFailed(failed));
    }

    let mut sides: Vec<ChosenSide> = Vec::with_capacity(2);
    for c in &chosen {
        let native = c.fids_frame.map(|p| frame.to_native(p));
        let geometry = resolve_fiducials(native, electrode)
            .map_err(|e| NativeTraceError::Geometry(e.to_string()))?;
        sides.push(ChosenSide {
            label: c.label,
            source: c.source,
            continuity: c.continuity,
            ransac_voxels: c.ransac_voxels,
            geometry,
        });
    }

    let limits = PlausibilityLimits::for_electrode(electrode);
    let (ni, nj, nk) = data.dim();
    let bounds = WorldBounds::from_grid(aff, ni, nj, nk, 0.0);
    let min_on_axis = Some(MIN_METAL_ON_AXIS_HU);
    let mut problems = Vec::new();
    for s in &sides {
        let g = &s.geometry;
        for msg in check_side(
            g.head,
            g.tail,
            &g.coords_mm,
            Some(&electrode.contact_level),
            &limits,
            Some(&bounds),
        ) {
            problems.push(format!("{} (native): {msg}", s.label));
        }
        let (on_axis, on_disc) =
            axis_and_disc_samples_in_bounds(data, aff, g.head, g.tail, 0.5, 1.5);
        for msg in check_metal_on_axis(&on_axis, &on_disc, METAL_ON_AXIS_MIN_FRACTION, min_on_axis)
        {
            problems.push(format!("{} (native): {msg}", s.label));
        }
    }
    if !problems.is_empty() {
        return Err(NativeTraceError::Implausible(problems));
    }
    let mut left = None;
    let mut right = None;
    for s in sides {
        if s.label == SIDE_LABELS[0] {
            left = Some(s);
        } else {
            right = Some(s);
        }
    }
    Ok(NativeSides {
        left,
        right,
        failed,
        side_notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::electrode_catalog::by_name;
    use crate::math::cross3;

    fn rot_x(deg: f64) -> [[f64; 3]; 3] {
        let a = deg.to_radians();
        [
            [1.0, 0.0, 0.0],
            [0.0, a.cos(), -a.sin()],
            [0.0, a.sin(), a.cos()],
        ]
    }

    #[test]
    fn rigid_frame_recovers_rotation_translation_and_scale() {
        let src = frame_lattice([-60.0, -50.0, -40.0], [60.0, 70.0, 80.0], 3, 1.0);
        let r = rot_x(17.0);
        let t = [3.0, -8.0, 12.0];
        let dst: Vec<[f64; 3]> = src
            .iter()
            .map(|p| {
                let q = apply_rigid_r_rows(&r, &[0.0; 3], &[*p])[0];
                [1.12 * q[0] + t[0], 1.12 * q[1] + t[1], 1.12 * q[2] + t[2]]
            })
            .collect();
        let fr = rigid_frame_from_points(&src, &dst).expect("fit");
        assert!((fr.scale - 1.12).abs() < 1e-9);
        for (got, want) in fr.r.iter().flatten().zip(r.iter().flatten()) {
            assert!((got - want).abs() < 1e-9);
        }
        assert!(fr.fit_rms_mm.abs() < 1e-9);
        for p in &src {
            let back = fr.to_native(fr.to_frame(*p));
            assert!(norm3(sub3(back, *p)) < 1e-9);
        }
        let m = fr.matrix();
        assert_eq!(m[3], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn frame_preserves_distances_but_reports_the_scale() {
        let src = [
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 10.0],
        ];
        let dst: Vec<[f64; 3]> = src
            .iter()
            .map(|p| [1.2 * p[0] + 5.0, 1.2 * p[1] + 5.0, 1.2 * p[2] + 5.0])
            .collect();
        let fr = rigid_frame_from_points(&src, &dst).expect("fit");
        let a = fr.to_frame(src[0]);
        let b = fr.to_frame(src[1]);
        assert!((norm3(sub3(b, a)) - 10.0).abs() < 1e-9);
        assert!((fr.scale - 1.2).abs() < 1e-9);
    }

    #[test]
    fn lattice_spans_the_central_half() {
        let l = frame_lattice([0.0, 0.0, 0.0], [100.0, 200.0, 40.0], 3, 0.5);
        assert_eq!(l.len(), 27);
        assert_eq!(l[0], [25.0, 50.0, 10.0]);
        assert_eq!(l[13], [50.0, 100.0, 20.0]);
        assert_eq!(l[26], [75.0, 150.0, 30.0]);
    }

    #[test]
    fn seed_scale_divides_the_template_seed_geometry() {
        let d = TracConfig::default();
        let c = seed_scaled_trac_config(1.1, 1.0);
        assert!((c.start_z_mm - 8.7 / 1.1).abs() < 1e-12);
        assert!((c.end_z_mm + 15.5 / 1.1).abs() < 1e-12);
        assert!((c.seed_abs_x_mm - 45.0 / 1.1).abs() < 1e-12);
        assert!((c.seed_abs_y_mm - 60.0 / 1.1).abs() < 1e-12);
        let same = seed_scaled_trac_config(1.0, 0.0);
        assert_eq!(same.start_z_mm, d.start_z_mm);
        assert_eq!(same.seed_abs_y_mm, d.seed_abs_y_mm);
        assert_eq!(same.distal_bloom_mm, 0.0);
    }

    #[test]
    fn resolve_places_the_catalogue_contacts_on_the_fiducials() {
        let e = by_name("Medtronic 3387").expect("catalogued");
        let head = [10.0, -2.0, -6.0];
        let tail = [10.0, -2.0, 3.0];
        let g = resolve_fiducials([head, tail, [11.0, -2.0, -6.0], [0.0; 3]], e).expect("geometry");
        assert_eq!(g.coords_mm.len(), 4);
        for (i, c) in g.coords_mm.iter().enumerate() {
            assert!(
                norm3(sub3(*c, [10.0, -2.0, -6.0 + 3.0 * i as f64])) < 1e-9,
                "{c:?}"
            );
        }
        assert!((norm3(sub3(g.x, head)) - 0.5 * e.lead_diameter_mm).abs() < 1e-12);
        assert_eq!(g.trajectory.len(), TRAJECTORY_POINTS);
        assert!(norm3(sub3(g.trajectory[0], [10.0, -2.0, -11.0])) < 1e-9);
        assert!(norm3(sub3(g.trajectory[49], [10.0, -2.0, 24.0])) < 1e-9);
        assert_eq!(g.trajvector, [0.0, 0.0, 1.0]);
        assert_eq!(
            resolve_fiducials([head, head, head, head], e),
            Err(GeometryError::HeadTailCoincide)
        );
    }

    const PH_VOX: f64 = 0.5;
    const PH_N: usize = 96;

    fn ph_affine() -> Mat4 {
        let o = 0.5 * PH_VOX * PH_N as f64;
        [
            [-PH_VOX, 0.0, 0.0, o],
            [0.0, PH_VOX, 0.0, -o],
            [0.0, 0.0, PH_VOX, -o],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn ph_hu_ct(aff: &Mat4, e: &ElectrodeGeometry, head: [f64; 3], u: [f64; 3]) -> Array3<f64> {
        let mut data = Array3::<f64>::zeros((PH_N, PH_N, PH_N));
        let intervals: Vec<(f64, f64)> = e
            .level_z_mm
            .iter()
            .enumerate()
            .map(|(k, &z)| {
                let half = if k == 0 && e.tip_is_contact {
                    0.5 * e.tip_length_mm.max(e.contact_length_mm)
                } else {
                    0.5 * e.contact_length_mm
                };
                (z - half, z + half)
            })
            .collect();
        let z_lo = intervals[0].0 - e.tip_length_mm;
        let r_lead = 0.5 * e.lead_diameter_mm;
        for i in 0..PH_N {
            for j in 0..PH_N {
                for k in 0..PH_N {
                    let w = apply_rowmajor(aff, [i as f64, j as f64, k as f64]);
                    let rel = sub3(w, head);
                    let a = rel[0] * u[0] + rel[1] * u[1] + rel[2] * u[2];
                    let rad = norm3(sub3(rel, scale3(u, a)));
                    let mut v = 40.0 + 0.4 * w[2] + 0.2 * w[0];
                    if rad <= r_lead && a >= z_lo && a <= 40.0 {
                        v = 120.0;
                        if intervals.iter().any(|&(lo, hi)| a >= lo && a <= hi) {
                            v = 2200.0;
                        }
                    }
                    data[[i, j, k]] = v;
                }
            }
        }
        data
    }

    #[test]
    fn refinement_pulls_a_tilted_offset_trac_line_onto_the_metal() {
        let e = by_name("Medtronic 3387").expect("catalogued");
        let aff = ph_affine();
        let head = [-11.0, -3.0, -9.0];
        let u = unit3([0.18, 0.10, 1.0]);
        let data = ph_hu_ct(&aff, e, head, u);
        let vol = data.mapv(|v| -v);
        let (metal_pts, metal_vals) = metal_voxels(&data, &aff, LEAD_BODY_HU);
        assert!(!metal_pts.is_empty());

        let side = unit3(cross3(u, [0.0, 1.0, 0.0]));
        let tilt = 4.0f64.to_radians();
        let u_bad = unit3([
            u[0] + tilt.tan() * side[0],
            u[1] + tilt.tan() * side[1],
            u[2] + tilt.tan() * side[2],
        ]);
        let head_bad = [
            head[0] + 0.7 * side[0],
            head[1] + 0.7 * side[1],
            head[2] + 0.7 * side[2],
        ];
        let span = e.fiducial_span_mm();
        let tail_bad = [
            head_bad[0] + span * u_bad[0],
            head_bad[1] + span * u_bad[1],
            head_bad[2] + span * u_bad[2],
        ];
        let fids = [head_bad, tail_bad, head_bad, head_bad];
        let cfg = TracConfig {
            distal_bloom_mm: 0.0,
            ..TracConfig::default()
        };
        let got = refine_trac_fiducials(&vol, &aff, &fids, &metal_pts, &metal_vals, e, &cfg)
            .expect("refined");

        let head_err = norm3(sub3(got[0], head));
        assert!(head_err < 0.3, "head {:?} off by {head_err:.3} mm", got[0]);
        let u_got = unit3(sub3(got[1], got[0]));
        let cos = (u_got[0] * u[0] + u_got[1] * u[1] + u_got[2] * u[2]).clamp(-1.0, 1.0);
        let angle = cos.acos().to_degrees();
        assert!(angle < 1.0, "direction off by {angle:.3} deg");
    }

    #[test]
    fn refinement_without_metal_voxels_keeps_the_trac_line() {
        let e = by_name("Medtronic 3387").expect("catalogued");
        let aff = ph_affine();
        let vol = Array3::<f64>::zeros((4, 4, 4));
        let fids = [
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 9.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let cfg = TracConfig::default();
        assert!(refine_trac_fiducials(&vol, &aff, &fids, &[], &[], e, &cfg).is_none());
    }

    #[test]
    fn corroboration_is_continuity_alone() {
        assert!(metal_axis_corroborated(Some(0.95)).is_ok());
        assert!(metal_axis_corroborated(Some(MIN_STEEP_CONTINUITY)).is_ok());
        let why = metal_axis_corroborated(Some(0.5)).expect_err("too little metal");
        assert!(
            why.contains("1800 HU") && why.contains("only 50%") && why.contains("need 90%"),
            "{why}"
        );
        assert!(metal_axis_corroborated(Some(0.89)).is_err());
        assert!(metal_axis_corroborated(None).is_err());
    }

    fn marker_set(
        e: &ElectrodeGeometry,
        head: [f64; 3],
        u: [f64; 3],
        x_dir: [f64; 3],
    ) -> [[f64; 3]; 4] {
        let r = 0.5 * e.lead_diameter_mm;
        let y_dir = crate::math::cross3(u, x_dir);
        let tail = [
            head[0] + u[0] * e.fiducial_span_mm(),
            head[1] + u[1] * e.fiducial_span_mm(),
            head[2] + u[2] * e.fiducial_span_mm(),
        ];
        let x = [
            head[0] + r * x_dir[0],
            head[1] + r * x_dir[1],
            head[2] + r * x_dir[2],
        ];
        let y = [
            head[0] + r * y_dir[0],
            head[1] + r * y_dir[1],
            head[2] + r * y_dir[2],
        ];
        [head, tail, x, y]
    }

    #[test]
    fn contacts_from_markers_identity_frame_translates_the_catalogue() {
        let e = by_name("Boston Scientific Vercise Directed").expect("catalogued");
        let head = [4.0, -3.0, 7.0];
        let m = marker_set(e, head, [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let c = contacts_from_markers(e, m[0], m[1], m[2], m[3]).expect("contacts");
        assert_eq!(c.len(), e.n_contacts);
        for (got, model) in c.iter().zip(&e.contact_xyz_mm) {
            let want = [head[0] + model[0], head[1] + model[1], head[2] + model[2]];
            assert!(norm3(sub3(*got, want)) < 1e-9, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn contacts_from_markers_stretches_along_the_lead_only() {
        let e = by_name("Medtronic 3387").expect("catalogued");
        let head = [1.0, 2.0, 3.0];
        let mut m = marker_set(e, head, [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        m[1] = [head[0], head[1], head[2] + 1.3 * e.fiducial_span_mm()];
        let c = contacts_from_markers(e, m[0], m[1], m[2], m[3]).expect("contacts");
        for (got, model) in c.iter().zip(&e.contact_xyz_mm) {
            let want = [
                head[0] + model[0],
                head[1] + model[1],
                head[2] + 1.3 * model[2],
            ];
            assert!(norm3(sub3(*got, want)) < 1e-9, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn contacts_from_markers_follow_the_roll() {
        let e = by_name("Boston Scientific Vercise Directed").expect("catalogued");
        let head = [0.0, 0.0, 0.0];
        let m = marker_set(e, head, [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
        let c = contacts_from_markers(e, m[0], m[1], m[2], m[3]).expect("contacts");
        for (got, model) in c.iter().zip(&e.contact_xyz_mm) {
            let want = [-model[1], model[0], model[2]];
            assert!(norm3(sub3(*got, want)) < 1e-9, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn contacts_from_markers_map_the_four_fiducials_exactly() {
        let mut e = by_name("Boston Scientific Vercise Directed")
            .expect("catalogued")
            .clone();
        let u = unit3([0.2, -0.1, 1.0]);
        let x_dir = unit3(crate::math::cross3(u, [0.0, 1.0, 0.0]));
        let m = marker_set(&e, [5.0, 6.0, -7.0], u, x_dir);
        let tail = [
            m[0][0] + 1.17 * (m[1][0] - m[0][0]),
            m[0][1] + 1.17 * (m[1][1] - m[0][1]),
            m[0][2] + 1.17 * (m[1][2] - m[0][2]),
        ];
        let model = e.model_fiducials();
        e.contact_xyz_mm = model.to_vec();
        let c = contacts_from_markers(&e, m[0], tail, m[2], m[3]).expect("contacts");
        let want = [m[0], tail, m[2], m[3]];
        for (got, want) in c.iter().zip(&want) {
            assert!(norm3(sub3(*got, *want)) < 1e-9, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn contacts_from_markers_refuse_degenerate_frames() {
        let e = by_name("Medtronic 3387").expect("catalogued");
        let m = marker_set(e, [0.0; 3], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        assert!(contacts_from_markers(e, m[0], m[0], m[2], m[3]).is_none());
        assert!(contacts_from_markers(e, m[0], m[1], m[2], m[2]).is_none());
        assert!(contacts_from_markers(e, m[0], m[1], [0.0, 0.0, 0.5], m[3]).is_none());
        assert!(contacts_from_markers(e, m[0], m[1], [f64::NAN, 0.0, 0.0], m[3]).is_none());
    }
}
