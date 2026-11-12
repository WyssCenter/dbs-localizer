// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! CPU `gencheckregpair`-class PNG: **moving** (grey) + **fixed** edge overlay (red), axial montage.
//!
//! Spec: `specs/03_coregistration.md` §12.2. Pure Rust; no FSL in CI.

use std::path::Path;

use nifti::NiftiHeader;

use crate::error::{CheckregError, CheckregResult};

/// Orthogonal slice (same axis convention as the volume: dim0 × dim1 × dim2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckregSlicePlane {
    /// Fixed **k** (index along dim2); 2D plane (dim0, dim1) — "axial" in typical RAS stacks.
    Axial,
    /// Fixed **j**; 2D plane (dim0, dim2) — "coronal".
    Coronal,
    /// Fixed **i**; 2D plane (dim1, dim2) — "sagittal".
    Sagittal,
}

/// Options matching Lead-DBS `ea_gencheckregpair` (defaults from reference).
#[derive(Debug, Clone)]
pub struct GencheckregpairOptions {
    /// Intensity pre-window on **moving** (percentile clip), like FSL with `preproc=true`.
    pub preproc: bool,
    /// Lower / upper **percentile** (0–100) on sorted intensities.
    pub lbound_pct: f64,
    pub ubound_pct: f64,
    /// Maximum number of axial slices in the horizontal montage (evenly sampled in `z`).
    pub max_montage_slices: usize,
}

impl Default for GencheckregpairOptions {
    fn default() -> Self {
        Self {
            preproc: true,
            lbound_pct: 0.2,
            ubound_pct: 99.7,
            max_montage_slices: 24,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Vol3 {
    n0: usize,
    n1: usize,
    n2: usize,
    v: Vec<f32>,
}

impl Vol3 {
    #[inline]
    fn get(&self, i: usize, j: usize, k: usize) -> f32 {
        self.v[k * self.n1 * self.n0 + j * self.n0 + i]
    }
}

fn spacing_from_header(hdr: &NiftiHeader) -> [f32; 3] {
    let mut s = [hdr.pixdim[1], hdr.pixdim[2], hdr.pixdim[3]];
    for x in &mut s {
        if !x.is_finite() || *x <= 0.0 {
            *x = 1.0;
        }
    }
    s
}

fn load_vol(path: &Path) -> CheckregResult<(Vol3, [f32; 3])> {
    let vol = crate::nifti_io::read_volume_f32(path)?;
    let sp = spacing_from_header(&vol.header);
    let [n0, n1, n2] = vol.dims;
    Ok((
        Vol3 {
            n0,
            n1,
            n2,
            v: vol.data,
        },
        sp,
    ))
}

type LoadedVol = CheckregResult<(Vol3, [f32; 3])>;

#[cfg(not(target_arch = "wasm32"))]
fn load_pair(moving: &Path, fixed: &Path) -> (LoadedVol, LoadedVol) {
    std::thread::scope(|s| {
        let fixed_job = s.spawn(|| load_vol(fixed));
        let moving_result = load_vol(moving);
        let fixed_result = fixed_job.join().unwrap_or_else(|_| {
            Err(CheckregError::Checkreg(
                "the image loader panicked".to_owned(),
            ))
        });
        (moving_result, fixed_result)
    })
}

#[cfg(target_arch = "wasm32")]
fn load_pair(moving: &Path, fixed: &Path) -> (LoadedVol, LoadedVol) {
    (load_vol(moving), load_vol(fixed))
}

/// Physical extent (mm) of in-plane axes for `plane` (voxel counts × spacing).
fn physical_plane_extent(
    plane: CheckregSlicePlane,
    n0: usize,
    n1: usize,
    n2: usize,
    sp: [f32; 3],
) -> (f32, f32) {
    let f0 = n0 as f32 * sp[0];
    let f1 = n1 as f32 * sp[1];
    let f2 = n2 as f32 * sp[2];
    match plane {
        CheckregSlicePlane::Axial => (f0, f1),
        CheckregSlicePlane::Coronal => (f0, f2),
        CheckregSlicePlane::Sagittal => (f1, f2),
    }
}

const MAX_SLICE_TEX_SIDE: f32 = 768.0;

fn resample_rgba_bilinear(src_w: u32, src_h: u32, src: &[u8], dst_w: u32, dst_h: u32) -> Vec<u8> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; (dst_w * dst_h * 4) as usize];
    let sw = src_w as f32;
    let sh = src_h as f32;
    for y in 0..dst_h {
        let v = (y as f32 + 0.5) / dst_h as f32 * sh - 0.5;
        let y0 = v.floor().clamp(0.0, (src_h - 1) as f32) as u32;
        let y1 = (y0 + 1).min(src_h - 1);
        let fy = v - y0 as f32;
        for x in 0..dst_w {
            let u = (x as f32 + 0.5) / dst_w as f32 * sw - 0.5;
            let x0 = u.floor().clamp(0.0, (src_w - 1) as f32) as u32;
            let x1 = (x0 + 1).min(src_w - 1);
            let fx = u - x0 as f32;
            let mut px = [0f32; 4];
            for (dy, yy, wy) in [(0u32, y0, 1.0 - fy), (1u32, y1, fy)] {
                if dy == 1 && y1 == y0 {
                    continue;
                }
                for (dx, xx, wx) in [(0u32, x0, 1.0 - fx), (1u32, x1, fx)] {
                    if dx == 1 && x1 == x0 {
                        continue;
                    }
                    let w = wx * wy;
                    let o = ((xx + yy * src_w) * 4) as usize;
                    for c in 0..4 {
                        px[c] += src[o + c] as f32 * w;
                    }
                }
            }
            let o = ((x + y * dst_w) * 4) as usize;
            for c in 0..4 {
                out[o + c] = px[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

fn draw_crosshair_rgba(buf: &mut [u8], w: u32, h: u32, cx: i32, cy: i32, rgba: [u8; 4]) {
    if w == 0 || h == 0 {
        return;
    }
    let ww = w as i32;
    let hh = h as i32;
    let thick = 2;
    for t in 0..thick {
        let y = cy - thick / 2 + t;
        if y >= 0 && y < hh {
            for x in 0..ww {
                let o = ((x + y * ww) * 4) as usize;
                buf[o..o + 4].copy_from_slice(&rgba);
            }
        }
    }
    for t in 0..thick {
        let x = cx - thick / 2 + t;
        if x >= 0 && x < ww {
            for y in 0..hh {
                let o = ((x + y * ww) * 4) as usize;
                buf[o..o + 4].copy_from_slice(&rgba);
            }
        }
    }
}

/// `(u, v)` texture coords ∈ [0,1]×[0,1] for crosshair at voxel `(ci,cj,ck)` on `plane`'s displayed slice.
pub fn crosshair_uv_for_plane(
    plane: CheckregSlicePlane,
    n0: usize,
    n1: usize,
    n2: usize,
    ci: usize,
    cj: usize,
    ck: usize,
) -> Option<(f32, f32)> {
    if n0 == 0 || n1 == 0 || n2 == 0 {
        return None;
    }
    let ci = ci.min(n0 - 1);
    let cj = cj.min(n1 - 1);
    let ck = ck.min(n2 - 1);
    Some(match plane {
        CheckregSlicePlane::Axial => {
            let u = (ci as f32 + 0.5) / n0 as f32;
            let v = (cj as f32 + 0.5) / n1 as f32;
            (u, v)
        }
        CheckregSlicePlane::Coronal => {
            let u = (ci as f32 + 0.5) / n0 as f32;
            let v = (ck as f32 + 0.5) / n2 as f32;
            (u, v)
        }
        CheckregSlicePlane::Sagittal => {
            let u = (cj as f32 + 0.5) / n1 as f32;
            let v = (ck as f32 + 0.5) / n2 as f32;
            (u, v)
        }
    })
}

/// Inverse of [`crosshair_uv_for_plane`]: normalized in-plane `(u,v)` ∈ [0,1]² (same convention as
/// texture / voxel slice axes) plus current slice index along the plane normal → full `(ci,cj,ck)`.
#[allow(clippy::too_many_arguments)]
pub fn pick_ijk_from_plane_uv(
    plane: CheckregSlicePlane,
    n0: usize,
    n1: usize,
    n2: usize,
    u: f32,
    v: f32,
    ci: usize,
    cj: usize,
    ck: usize,
) -> (usize, usize, usize) {
    if n0 == 0 || n1 == 0 || n2 == 0 {
        return (ci, cj, ck);
    }
    let uu = u.clamp(0.0, 1.0);
    let vv = v.clamp(0.0, 1.0);
    let pick = |t: f32, n: usize| -> usize {
        if n <= 1 {
            return 0;
        }
        let x = (t * n as f32 - 0.5).round().clamp(0.0, (n - 1) as f32) as usize;
        x.min(n - 1)
    };
    match plane {
        CheckregSlicePlane::Axial => (pick(uu, n0), pick(vv, n1), ck.min(n2 - 1)),
        CheckregSlicePlane::Sagittal => (ci.min(n0 - 1), pick(uu, n1), pick(vv, n2)),
        CheckregSlicePlane::Coronal => (pick(uu, n0), cj.min(n1 - 1), pick(vv, n2)),
    }
}

#[cfg(test)]
mod pick_tests {
    use super::{crosshair_uv_for_plane, pick_ijk_from_plane_uv, CheckregSlicePlane};

    #[test]
    fn pick_roundtrip_axial() {
        let n0 = 64;
        let n1 = 48;
        let n2 = 32;
        for ci in [0usize, 31, 63] {
            for cj in [0usize, 23, 47] {
                let ck = 17;
                let Some((u, v)) =
                    crosshair_uv_for_plane(CheckregSlicePlane::Axial, n0, n1, n2, ci, cj, ck)
                else {
                    continue;
                };
                let (pi, pj, pk) =
                    pick_ijk_from_plane_uv(CheckregSlicePlane::Axial, n0, n1, n2, u, v, ci, cj, ck);
                assert_eq!(pk, ck);
                assert_eq!(pi, ci);
                assert_eq!(pj, cj);
            }
        }
    }

    #[test]
    fn pick_roundtrip_sagittal_coronal() {
        let n0 = 40;
        let n1 = 50;
        let n2 = 60;
        let ci = 12;
        let cj = 25;
        let ck = 33;
        let (u, v) =
            crosshair_uv_for_plane(CheckregSlicePlane::Sagittal, n0, n1, n2, ci, cj, ck).unwrap();
        let (pi, pj, pk) =
            pick_ijk_from_plane_uv(CheckregSlicePlane::Sagittal, n0, n1, n2, u, v, ci, cj, ck);
        assert_eq!(pi, ci);
        assert_eq!(pj, cj);
        assert_eq!(pk, ck);

        let (u2, v2) =
            crosshair_uv_for_plane(CheckregSlicePlane::Coronal, n0, n1, n2, ci, cj, ck).unwrap();
        let (pi2, pj2, pk2) =
            pick_ijk_from_plane_uv(CheckregSlicePlane::Coronal, n0, n1, n2, u2, v2, ci, cj, ck);
        assert_eq!((pi2, pj2, pk2), (ci, cj, ck));
    }
}

fn percentile_clamp(vals: &mut [f32], p_lo: f64, p_hi: f64) -> (f32, f32) {
    if vals.is_empty() {
        return (0.0, 1.0);
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = vals.len();
    let il = ((n as f64 * p_lo / 100.0).floor() as usize).min(n.saturating_sub(1));
    let iu = ((n as f64 * p_hi / 100.0).floor() as usize).min(n.saturating_sub(1));
    (vals[il], vals[iu].max(vals[il]))
}

fn preproc_moving(v: &mut [f32], opt: &GencheckregpairOptions) {
    if !opt.preproc {
        return;
    }
    let mut work: Vec<f32> = v
        .iter()
        .copied()
        .filter(|x| x.is_finite() && *x > 0.0)
        .collect();
    if work.is_empty() {
        work = v.to_vec();
    }
    let (lo, hi) = percentile_clamp(&mut work, opt.lbound_pct, opt.ubound_pct);
    let range = (hi - lo).max(1e-6);
    for x in v.iter_mut() {
        if !x.is_finite() {
            *x = 0.0;
            continue;
        }
        *x = ((*x - lo) / range).clamp(0.0, 1.0);
    }
}

/// Sobel magnitude; `s` is `n0 × n1` row-major `s[i + j * n0]`.
fn sobel_mag(s: &[f32], n0: usize, n1: usize) -> Vec<f32> {
    let w = n0;
    let h = n1;
    let mut out = vec![0.0f32; w * h];
    for j in 1..h - 1 {
        for i in 1..w - 1 {
            let gx = -s[i - 1 + (j - 1) * w] + s[i + 1 + (j - 1) * w] - 2.0 * s[i - 1 + j * w]
                + 2.0 * s[i + 1 + j * w]
                - s[i - 1 + (j + 1) * w]
                + s[i + 1 + (j + 1) * w];
            let gy = -s[i - 1 + (j - 1) * w] - 2.0 * s[i + (j - 1) * w] - s[i + 1 + (j - 1) * w]
                + s[i - 1 + (j + 1) * w]
                + 2.0 * s[i + (j + 1) * w]
                + s[i + 1 + (j + 1) * w];
            out[i + j * w] = (gx * gx + gy * gy).sqrt();
        }
    }
    out
}

fn axial_slice(vol: &Vol3, k: usize) -> Vec<f32> {
    let mut s = vec![0.0f32; vol.n0 * vol.n1];
    for j in 0..vol.n1 {
        for i in 0..vol.n0 {
            s[i + j * vol.n0] = vol.get(i, j, k);
        }
    }
    s
}

/// Sample `count` z-indices in `0..n2` (inclusive ends), evenly.
fn z_indices(n2: usize, count: usize) -> Vec<usize> {
    let count = count.max(1).min(n2);
    if count == 1 {
        return vec![n2 / 2];
    }
    (0..count).map(|i| i * (n2 - 1) / (count - 1)).collect()
}

/// Loaded and windowed **moving** + raw **fixed** for fast slice scrubbing in the checkreg UI.
pub struct PreparedCheckregVols {
    /// Moving after optional percentile preproc (matches batch PNG greyscale).
    mov: Vol3,
    /// Moving **before** preproc — same geometry; used for interactive W/L on raw samples.
    mov_raw: Vol3,
    fix: Vol3,
    /// Voxel spacing (mm) from moving NIfTI `pixdim[1..4]` — used for anatomically correct slice aspect.
    spacing: [f32; 3],
    raw_stats: RawWindowStats,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawWindowStats {
    pub range: (f32, f32),
    pub window: (f32, f32),
    pub is_ct: bool,
}

const CT_SOFT_TISSUE_WINDOW: (f32, f32) = (40.0, 80.0);
const WINDOW_SAMPLES: usize = 1_000_000;

fn raw_window_stats(values: &[f32]) -> RawWindowStats {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for &v in values {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return RawWindowStats {
            range: (0.0, 1.0),
            window: (0.5, 1.0),
            is_ct: false,
        };
    }
    let is_ct = lo < -500.0 && hi > 1000.0;
    let window = if is_ct {
        CT_SOFT_TISSUE_WINDOW
    } else {
        let step = (values.len() / WINDOW_SAMPLES).max(1);
        let mut sample: Vec<f32> = values
            .iter()
            .step_by(step)
            .copied()
            .filter(|v| v.is_finite())
            .collect();
        sample.sort_by(|a, b| a.total_cmp(b));
        let pick = |pct: f64| {
            let pos = ((sample.len() - 1) as f64 * pct / 100.0).round() as usize;
            sample[pos.min(sample.len() - 1)]
        };
        let (p_lo, p_hi) = (pick(0.5), pick(99.5));
        let width = (p_hi - p_lo).max((hi - lo) * 1e-3).max(1e-3);
        (p_lo + 0.5 * width, width)
    };
    RawWindowStats {
        range: (lo, hi),
        window,
        is_ct,
    }
}

impl PreparedCheckregVols {
    /// `(n0, n1, n2)` from the NIfTI.
    pub fn dims(&self) -> (usize, usize, usize) {
        (self.mov.n0, self.mov.n1, self.mov.n2)
    }

    /// Spacing (mm) along dim0, dim1, dim2.
    pub fn spacing(&self) -> [f32; 3] {
        self.spacing
    }

    pub fn raw_window_stats(&self) -> RawWindowStats {
        self.raw_stats
    }

    /// Inclusive max slice index for `plane` (0 when dimension length is 0 or 1).
    pub fn max_index(&self, plane: CheckregSlicePlane) -> usize {
        let (n0, n1, n2) = (self.mov.n0, self.mov.n1, self.mov.n2);
        match plane {
            CheckregSlicePlane::Axial => n2.saturating_sub(1),
            CheckregSlicePlane::Coronal => n1.saturating_sub(1),
            CheckregSlicePlane::Sagittal => n0.saturating_sub(1),
        }
    }

    /// Default slice (middle) per plane.
    pub fn default_index(&self, plane: CheckregSlicePlane) -> usize {
        let (n0, n1, n2) = (self.mov.n0, self.mov.n1, self.mov.n2);
        match plane {
            CheckregSlicePlane::Axial => n2 / 2,
            CheckregSlicePlane::Coronal => n1 / 2,
            CheckregSlicePlane::Sagittal => n0 / 2,
        }
    }

    /// One 2D slice as RGBA8 (same overlay as the batch PNG / montage). Moving = **preprocessed** 0–1.
    pub fn rgba_slice(
        &self,
        plane: CheckregSlicePlane,
        index: usize,
    ) -> CheckregResult<(u32, u32, Vec<u8>)> {
        self.rgba_slice_display(plane, index, false, 0.0, 1.0, None)
    }

    /// Linear W/L on **raw** moving NIfTI samples: `t = clamp((v - (center - width/2)) / width, 0, 1)`.
    /// Edge overlay still from **fixed** (unchanged). `width` clamped to ≥ 1e-3 internally.
    pub fn rgba_slice_windowed(
        &self,
        plane: CheckregSlicePlane,
        index: usize,
        center: f32,
        width: f32,
    ) -> CheckregResult<(u32, u32, Vec<u8>)> {
        self.rgba_slice_display(plane, index, true, center, width, None)
    }

    /// Resampled to physical aspect ratio + optional cyan crosshair at `ijk` (display coords).
    pub fn rgba_slice_display(
        &self,
        plane: CheckregSlicePlane,
        index: usize,
        raw_window: bool,
        center: f32,
        width: f32,
        crosshair_ijk: Option<(usize, usize, usize)>,
    ) -> CheckregResult<(u32, u32, Vec<u8>)> {
        let layers = self.slice_layers(plane, index, raw_window)?;
        let (out_w, out_h, mut out_data) = self.compose_display(&layers, center, width);
        if let Some(ijk) = crosshair_ijk {
            self.draw_display_crosshair(plane, out_w, out_h, &mut out_data, ijk);
        }
        Ok((out_w, out_h, out_data))
    }

    pub fn slice_layers(
        &self,
        plane: CheckregSlicePlane,
        index: usize,
        raw_window: bool,
    ) -> CheckregResult<SliceLayers> {
        let vol_mov = if raw_window { &self.mov_raw } else { &self.mov };
        let fix = &self.fix;
        let (w, h, msl, fsl) = match plane {
            CheckregSlicePlane::Axial => {
                let n0 = vol_mov.n0;
                let n1 = vol_mov.n1;
                let n2 = vol_mov.n2;
                if n2 == 0 {
                    return Err(CheckregError::Checkreg("empty dim2".into()));
                }
                let k = index.min(n2 - 1);
                let msl = axial_slice(vol_mov, k);
                let fsl = axial_slice(fix, k);
                (n0, n1, msl, fsl)
            }
            CheckregSlicePlane::Coronal => {
                let n0 = vol_mov.n0;
                let n1 = vol_mov.n1;
                let n2 = vol_mov.n2;
                if n1 == 0 {
                    return Err(CheckregError::Checkreg("empty dim1".into()));
                }
                let j = index.min(n1 - 1);
                let ww = n0;
                let hh = n2;
                let mut msl = vec![0.0f32; ww * hh];
                let mut fsl = vec![0.0f32; ww * hh];
                for k in 0..n2 {
                    for i in 0..n0 {
                        msl[i + k * ww] = vol_mov.get(i, j, k);
                        fsl[i + k * ww] = fix.get(i, j, k);
                    }
                }
                (ww, hh, msl, fsl)
            }
            CheckregSlicePlane::Sagittal => {
                let n0 = vol_mov.n0;
                let n1 = vol_mov.n1;
                let n2 = vol_mov.n2;
                if n0 == 0 {
                    return Err(CheckregError::Checkreg("empty dim0".into()));
                }
                let i0 = index.min(n0 - 1);
                let ww = n1;
                let hh = n2;
                let mut msl = vec![0.0f32; ww * hh];
                let mut fsl = vec![0.0f32; ww * hh];
                for k in 0..n2 {
                    for j in 0..n1 {
                        msl[j + k * ww] = vol_mov.get(i0, j, k);
                        fsl[j + k * ww] = fix.get(i0, j, k);
                    }
                }
                (ww, hh, msl, fsl)
            }
        };

        let mag = sobel_mag(&fsl, w, h);
        let mmax: f32 = mag.iter().copied().fold(0.0f32, f32::max);
        let edge_th = (0.10 * mmax).max(1e-6);
        let edge = mag.iter().map(|&m| m > edge_th).collect();
        Ok(SliceLayers {
            plane,
            raw_window,
            w,
            h,
            moving: msl,
            edge,
        })
    }

    pub fn compose_display(
        &self,
        layers: &SliceLayers,
        center: f32,
        width: f32,
    ) -> (u32, u32, Vec<u8>) {
        let width = width.max(1e-3);
        let raw_window = layers.raw_window;
        let (w, h) = (layers.w, layers.h);
        let mut full = vec![0u8; w * h * 4];
        let half = width * 0.5;
        let lo_win = center - half;
        for j in 0..h {
            for i in 0..w {
                let rv = layers.moving[i + j * w];
                let m = if raw_window {
                    if rv.is_finite() {
                        ((rv - lo_win) / width).clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                } else {
                    rv.clamp(0.0, 1.0)
                };
                let e = if layers.edge[i + j * w] { 1.0 } else { 0.0 };
                let r = m * (1.0 - 0.75 * e) + 0.95 * e;
                let g = m * (1.0 - 0.4 * e);
                let b = m * (1.0 - 0.4 * e);
                let o = (i + j * w) * 4;
                full[o] = (r * 255.0).min(255.0) as u8;
                full[o + 1] = (g * 255.0).min(255.0) as u8;
                full[o + 2] = (b * 255.0).min(255.0) as u8;
                full[o + 3] = 255;
            }
        }
        let (out_w, out_h) = self.display_size(layers.plane);
        let out_data = if out_w == w as u32 && out_h == h as u32 {
            full
        } else {
            resample_rgba_bilinear(w as u32, h as u32, &full, out_w, out_h)
        };
        (out_w, out_h, out_data)
    }

    fn display_size(&self, plane: CheckregSlicePlane) -> (u32, u32) {
        let (n0, n1, n2) = self.dims();
        let (phys_w, phys_h) = physical_plane_extent(plane, n0, n1, n2, self.spacing);
        let max_phys = phys_w.max(phys_h).max(1e-3);
        let scale = MAX_SLICE_TEX_SIDE / max_phys;
        let out_w = ((phys_w * scale).round() as u32).max(1);
        let out_h = ((phys_h * scale).round() as u32).max(1);
        const CAP: u32 = 2048;
        (out_w.min(CAP), out_h.min(CAP))
    }

    pub fn draw_display_crosshair(
        &self,
        plane: CheckregSlicePlane,
        out_w: u32,
        out_h: u32,
        buf: &mut [u8],
        (ci, cj, ck): (usize, usize, usize),
    ) {
        if buf.len() < out_w as usize * out_h as usize * 4 {
            return;
        }
        let (n0, n1, n2) = self.dims();
        if let Some((u, v)) = crosshair_uv_for_plane(plane, n0, n1, n2, ci, cj, ck) {
            let cx = (u * out_w as f32).round() as i32;
            let cy = (v * out_h as f32).round() as i32;
            draw_crosshair_rgba(buf, out_w, out_h, cx, cy, [0, 255, 200, 255]);
        }
    }
}

pub struct SliceLayers {
    plane: CheckregSlicePlane,
    raw_window: bool,
    w: usize,
    h: usize,
    moving: Vec<f32>,
    edge: Vec<bool>,
}

/// Load moving + fixed, window **moving**; keep **fixed** raw for edge overlay.
pub fn load_prepared_checkreg_volumes(
    moving_path: &Path,
    fixed_path: &Path,
    opt: &GencheckregpairOptions,
) -> CheckregResult<PreparedCheckregVols> {
    load_prepared_checkreg_volumes_cancellable(moving_path, fixed_path, opt, &|| true)
}

pub fn load_prepared_checkreg_volumes_cancellable(
    moving_path: &Path,
    fixed_path: &Path,
    opt: &GencheckregpairOptions,
    keep_going: &dyn Fn() -> bool,
) -> CheckregResult<PreparedCheckregVols> {
    let cancelled = || CheckregError::Checkreg("load cancelled".to_owned());
    if !keep_going() {
        return Err(cancelled());
    }
    let (moving_loaded, fixed_loaded) = load_pair(moving_path, fixed_path);
    let (mut mov, spacing) = moving_loaded?;
    let (fix, _) = fixed_loaded?;
    if !keep_going() {
        return Err(cancelled());
    }
    if (mov.n0, mov.n1, mov.n2) != (fix.n0, fix.n1, fix.n2) {
        return Err(CheckregError::CheckregShape {
            moving: vec![mov.n0, mov.n1, mov.n2],
            fixed: vec![fix.n0, fix.n1, fix.n2],
        });
    }
    if !keep_going() {
        return Err(cancelled());
    }
    let mov_raw = mov.clone();
    let raw_stats = raw_window_stats(&mov_raw.v);
    preproc_moving(&mut mov.v, opt);
    Ok(PreparedCheckregVols {
        mov,
        mov_raw,
        fix,
        spacing,
        raw_stats,
    })
}

/// Write one PNG: axial montage; moving (grey) + red edge overlay from **fixed** per `specs/03_coregistration.md` §12.2.
pub fn gencheckregpair_png(
    moving_path: &Path,
    fixed_path: &Path,
    out_png: &Path,
    opt: GencheckregpairOptions,
) -> CheckregResult<()> {
    if !out_png
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    {
        return Err(CheckregError::Checkreg(format!(
            "output path must end in .png: {}",
            out_png.display()
        )));
    }
    let (mut mov, _) = load_vol(moving_path)?;
    let (fix, _) = load_vol(fixed_path)?;
    if (mov.n0, mov.n1, mov.n2) != (fix.n0, fix.n1, fix.n2) {
        return Err(CheckregError::CheckregShape {
            moving: vec![mov.n0, mov.n1, mov.n2],
            fixed: vec![fix.n0, fix.n1, fix.n2],
        });
    }
    preproc_moving(&mut mov.v, &opt);
    let n0 = mov.n0;
    let n1 = mov.n1;
    let n2 = mov.n2;
    let zs = z_indices(n2, opt.max_montage_slices);
    let row_w = n0 * zs.len();
    let row_h = n1;
    let mut full = vec![0u8; row_w * row_h * 4];
    for (zi, &z) in zs.iter().enumerate() {
        let msl = axial_slice(&mov, z);
        let fsl = axial_slice(&fix, z);
        let mag = sobel_mag(&fsl, n0, n1);
        let mmax: f32 = mag.iter().copied().fold(0.0f32, f32::max);
        let edge_th = (0.10 * mmax).max(1e-6);
        for j in 0..n1 {
            for i in 0..n0 {
                let m = msl[i + j * n0].clamp(0.0, 1.0);
                let e = if mag[i + j * n0] > edge_th { 1.0 } else { 0.0 };
                let r = m * (1.0 - 0.75 * e) + 0.95 * e;
                let g = m * (1.0 - 0.4 * e);
                let b = m * (1.0 - 0.4 * e);
                let x = zi * n0 + i;
                let y = j;
                let o = (x + y * row_w) * 4;
                full[o] = (r * 255.0).min(255.0) as u8;
                full[o + 1] = (g * 255.0).min(255.0) as u8;
                full[o + 2] = (b * 255.0).min(255.0) as u8;
                full[o + 3] = 255;
            }
        }
    }
    if let Some(parent) = out_png.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CheckregError::Checkreg(e.to_string()))?;
    }
    let file =
        std::fs::File::create(out_png).map_err(|e| CheckregError::Checkreg(e.to_string()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), row_w as u32, row_h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| CheckregError::Checkreg(e.to_string()))?;
    writer
        .write_image_data(&full)
        .map_err(|e| CheckregError::Checkreg(e.to_string()))?;
    writer
        .finish()
        .map_err(|e| CheckregError::Checkreg(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal NIfTI-1 (ordering i + j*n0 + k*n0*n1).
    fn make_nifti1_bytes(n0: usize, n1: usize, n2: usize, voxels: &[f32]) -> Vec<u8> {
        assert_eq!(voxels.len(), n0 * n1 * n2);
        let mut h = [0u8; 348];
        macro_rules! wi32 {
            ($off:expr, $v:expr) => {
                h[$off..$off + 4].copy_from_slice(&($v as i32).to_le_bytes())
            };
        }
        macro_rules! wi16 {
            ($off:expr, $v:expr) => {
                h[$off..$off + 2].copy_from_slice(&($v as i16).to_le_bytes())
            };
        }
        macro_rules! wf32 {
            ($off:expr, $v:expr) => {
                h[$off..$off + 4].copy_from_slice(&($v as f32).to_le_bytes())
            };
        }
        wi32!(0, 348);
        wi16!(40, 3);
        wi16!(42, n0);
        wi16!(44, n1);
        wi16!(46, n2);
        wi16!(48, 1);
        wi16!(50, 1);
        wi16!(52, 1);
        wi16!(54, 1);
        wi16!(70, 16);
        wi16!(72, 32);
        wf32!(76, 0.0);
        wf32!(80, 1.0);
        wf32!(84, 1.0);
        wf32!(88, 1.0);
        wf32!(108, 352.0);
        wi16!(254, 1);
        wf32!(280, 1.0);
        wf32!(284, 0.0);
        wf32!(288, 0.0);
        wf32!(292, 0.0);
        wf32!(296, 0.0);
        wf32!(300, 1.0);
        wf32!(304, 0.0);
        wf32!(308, 0.0);
        wf32!(312, 0.0);
        wf32!(316, 0.0);
        wf32!(320, 1.0);
        wf32!(324, 0.0);
        h[344] = b'n';
        h[345] = b'+';
        h[346] = b'1';
        h[347] = 0;
        let mut out = Vec::with_capacity(348 + 4 + voxels.len() * 4);
        out.extend_from_slice(&h);
        out.extend_from_slice(&[0u8; 4]);
        for &v in voxels {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn default_raw_window_follows_the_image_range() {
        let ct: Vec<f32> = (0..1000).map(|i| -1024.0 + i as f32 * 4.0).collect();
        let s = raw_window_stats(&ct);
        assert!(s.is_ct);
        assert_eq!(s.window, CT_SOFT_TISSUE_WINDOW);
        let mr: Vec<f32> = (0..1000).map(|i| i as f32 / 999.0).collect();
        let s = raw_window_stats(&mr);
        assert!(!s.is_ct);
        assert_eq!(s.range, (0.0, 1.0));
        let (c, w) = s.window;
        assert!((c - 0.5).abs() < 0.01 && (w - 0.99).abs() < 0.01, "{c} {w}");
    }

    #[test]
    fn load_prepared_volumes_from_env_if_set() {
        let (Ok(mov_s), Ok(fix_s)) = (
            std::env::var("DBSLOC_CHECKREG_TEST_MOVING"),
            std::env::var("DBSLOC_CHECKREG_TEST_FIXED"),
        ) else {
            return;
        };
        let r = load_prepared_checkreg_volumes(
            std::path::Path::new(&mov_s),
            std::path::Path::new(&fix_s),
            &GencheckregpairOptions::default(),
        );
        assert!(r.is_ok(), "{r:#?}", r = r.map(|_| ()));
    }

    #[test]
    fn gencheckregpair_refuses_a_non_png_output_path() {
        let dir = std::env::temp_dir().join(format!("dbsloc_cr_np_{}", std::process::id()));
        let out = dir.join("sub").join("out.jpg");
        let missing = dir.join("missing.nii");
        let err = gencheckregpair_png(&missing, &missing, &out, GencheckregpairOptions::default())
            .expect_err("non-png path refused");
        assert!(err.to_string().contains("out.jpg"), "{err}");
        assert!(!dir.exists());
    }

    #[test]
    fn gencheckregpair_same_volumes_writes_png() {
        let n = 8usize;
        let voxels: Vec<f32> = (0..(n * n * n))
            .map(|i| (i % 20) as f32 * 0.01 + 0.1)
            .collect();
        let b = make_nifti1_bytes(n, n, n, &voxels);
        let tmp = std::env::temp_dir().join(format!("dbsloc_cr_{}.nii", std::process::id()));
        std::fs::write(&tmp, &b).unwrap();
        let out = tmp.with_extension("checkreg.png");
        let o = GencheckregpairOptions {
            max_montage_slices: 4,
            ..Default::default()
        };
        gencheckregpair_png(&tmp, &tmp, &out, o).expect("gencheckreg");
        let meta = std::fs::metadata(&out).expect("out exists");
        assert!(meta.len() > 200);
        let decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&out).expect("open png"),
        ));
        let mut reader = decoder.read_info().expect("png header");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("png size")];
        let info = reader.next_frame(&mut buf).expect("png frame");
        assert_eq!((info.width, info.height), (4 * n as u32, n as u32));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        let (px, rest) = buf[..info.buffer_size()].as_chunks::<4>();
        assert!(rest.is_empty());
        assert_eq!(px.len(), 4 * n * n);
        assert!(px.iter().all(|p| p[3] == 255 && p[1] == p[2]));
        assert!(px.iter().all(|p| p[0] >= p[1]));
        assert!(px.iter().any(|p| p[1] > 0));
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn rgba_slice_windowed_differs_from_preproc_path() {
        let n = 12usize;
        let voxels: Vec<f32> = (0..(n * n * n)).map(|idx| idx as f32 * 5.0).collect();
        let b = make_nifti1_bytes(n, n, n, &voxels);
        let tmp = std::env::temp_dir().join(format!("dbsloc_wl_{}.nii", std::process::id()));
        std::fs::write(&tmp, &b).unwrap();
        let p = load_prepared_checkreg_volumes(
            &tmp,
            &tmp,
            &GencheckregpairOptions {
                preproc: true,
                ..Default::default()
            },
        )
        .expect("load");
        let a = p
            .rgba_slice(CheckregSlicePlane::Axial, n / 2)
            .expect("rgba");
        let bwl = p
            .rgba_slice_windowed(CheckregSlicePlane::Axial, n / 2, 100.0, 200.0)
            .expect("rgba wl");
        assert_ne!(a.2, bwl.2, "W/L path should change greyscale bytes");
        let _ = std::fs::remove_file(&tmp);
    }

    fn reference_rgba_slice_display(
        p: &PreparedCheckregVols,
        plane: CheckregSlicePlane,
        index: usize,
        raw_window: bool,
        center: f32,
        width: f32,
        crosshair_ijk: Option<(usize, usize, usize)>,
    ) -> CheckregResult<(u32, u32, Vec<u8>)> {
        let width = width.max(1e-3);
        let vol_mov = if raw_window { &p.mov_raw } else { &p.mov };
        let fix = &p.fix;
        let (w, h, msl, fsl) = match plane {
            CheckregSlicePlane::Axial => {
                let n0 = vol_mov.n0;
                let n1 = vol_mov.n1;
                let n2 = vol_mov.n2;
                if n2 == 0 {
                    return Err(CheckregError::Checkreg("empty dim2".into()));
                }
                let k = index.min(n2 - 1);
                let msl = axial_slice(vol_mov, k);
                let fsl = axial_slice(fix, k);
                (n0, n1, msl, fsl)
            }
            CheckregSlicePlane::Coronal => {
                let n0 = vol_mov.n0;
                let n1 = vol_mov.n1;
                let n2 = vol_mov.n2;
                if n1 == 0 {
                    return Err(CheckregError::Checkreg("empty dim1".into()));
                }
                let j = index.min(n1 - 1);
                let ww = n0;
                let hh = n2;
                let mut msl = vec![0.0f32; ww * hh];
                let mut fsl = vec![0.0f32; ww * hh];
                for k in 0..n2 {
                    for i in 0..n0 {
                        msl[i + k * ww] = vol_mov.get(i, j, k);
                        fsl[i + k * ww] = fix.get(i, j, k);
                    }
                }
                (ww, hh, msl, fsl)
            }
            CheckregSlicePlane::Sagittal => {
                let n0 = vol_mov.n0;
                let n1 = vol_mov.n1;
                let n2 = vol_mov.n2;
                if n0 == 0 {
                    return Err(CheckregError::Checkreg("empty dim0".into()));
                }
                let i0 = index.min(n0 - 1);
                let ww = n1;
                let hh = n2;
                let mut msl = vec![0.0f32; ww * hh];
                let mut fsl = vec![0.0f32; ww * hh];
                for k in 0..n2 {
                    for j in 0..n1 {
                        msl[j + k * ww] = vol_mov.get(i0, j, k);
                        fsl[j + k * ww] = fix.get(i0, j, k);
                    }
                }
                (ww, hh, msl, fsl)
            }
        };

        let mag = sobel_mag(&fsl, w, h);
        let mmax: f32 = mag.iter().copied().fold(0.0f32, f32::max);
        let edge_th = (0.10 * mmax).max(1e-6);
        let mut full = vec![0u8; w * h * 4];
        let half = width * 0.5;
        let lo_win = center - half;
        for j in 0..h {
            for i in 0..w {
                let rv = msl[i + j * w];
                let m = if raw_window {
                    if rv.is_finite() {
                        ((rv - lo_win) / width).clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                } else {
                    rv.clamp(0.0, 1.0)
                };
                let e = if mag[i + j * w] > edge_th { 1.0 } else { 0.0 };
                let r = m * (1.0 - 0.75 * e) + 0.95 * e;
                let g = m * (1.0 - 0.4 * e);
                let b = m * (1.0 - 0.4 * e);
                let o = (i + j * w) * 4;
                full[o] = (r * 255.0).min(255.0) as u8;
                full[o + 1] = (g * 255.0).min(255.0) as u8;
                full[o + 2] = (b * 255.0).min(255.0) as u8;
                full[o + 3] = 255;
            }
        }

        let n0 = vol_mov.n0;
        let n1 = vol_mov.n1;
        let n2 = vol_mov.n2;
        let (phys_w, phys_h) = physical_plane_extent(plane, n0, n1, n2, p.spacing);
        let max_phys = phys_w.max(phys_h).max(1e-3);
        let scale = MAX_SLICE_TEX_SIDE / max_phys;
        let mut out_w = ((phys_w * scale).round() as u32).max(1);
        let mut out_h = ((phys_h * scale).round() as u32).max(1);
        const CAP: u32 = 2048;
        out_w = out_w.min(CAP);
        out_h = out_h.min(CAP);

        let mut out_data = if out_w == w as u32 && out_h == h as u32 {
            full
        } else {
            resample_rgba_bilinear(w as u32, h as u32, &full, out_w, out_h)
        };

        if let Some((ci, cj, ck)) = crosshair_ijk {
            if let Some((u, v)) = crosshair_uv_for_plane(plane, n0, n1, n2, ci, cj, ck) {
                let cx = (u * out_w as f32).round() as i32;
                let cy = (v * out_h as f32).round() as i32;
                draw_crosshair_rgba(&mut out_data, out_w, out_h, cx, cy, [0, 255, 200, 255]);
            }
        }

        Ok((out_w, out_h, out_data))
    }

    fn anisotropic_pair(spacing: [f32; 3]) -> PreparedCheckregVols {
        let (n0, n1, n2) = (13usize, 9usize, 7usize);
        let mut raw = Vec::with_capacity(n0 * n1 * n2);
        for k in 0..n2 {
            for j in 0..n1 {
                for i in 0..n0 {
                    raw.push(((i * 7 + j * 13 + k * 29) % 41) as f32 * 25.0 - 200.0);
                }
            }
        }
        raw[17] = f32::NAN;
        raw[400] = f32::INFINITY;
        let fixed: Vec<f32> = raw
            .iter()
            .enumerate()
            .map(|(idx, v)| {
                if idx % 11 == 0 {
                    v * 0.5
                } else {
                    v * 0.9 + 40.0
                }
            })
            .collect();
        let mov_raw = Vol3 { n0, n1, n2, v: raw };
        let mut mov = mov_raw.clone();
        preproc_moving(&mut mov.v, &GencheckregpairOptions::default());
        let raw_stats = raw_window_stats(&mov_raw.v);
        PreparedCheckregVols {
            mov,
            mov_raw,
            fix: Vol3 {
                n0,
                n1,
                n2,
                v: fixed,
            },
            spacing,
            raw_stats,
        }
    }

    #[test]
    fn staged_slice_path_is_byte_identical_to_the_monolithic_reference() {
        let planes = [
            CheckregSlicePlane::Axial,
            CheckregSlicePlane::Coronal,
            CheckregSlicePlane::Sagittal,
        ];
        let windows = [(40.0f32, 80.0f32), (300.0, 1500.0)];
        let crosshair = (3usize, 5usize, 2usize);
        let mut compared = 0usize;
        let mut window_changed_bytes = false;
        let mut crosshair_drawn = false;
        let p = anisotropic_pair([0.8, 1.1, 2.5]);
        let mut case = 0usize;
        for plane in planes {
            let indices = [0usize, p.default_index(plane), 50];
            for raw in [false, true] {
                let mut previous: Option<Vec<u8>> = None;
                for (center, width) in windows {
                    let index = indices[case % indices.len()];
                    case += 1;
                    let layers = p.slice_layers(plane, index, raw).expect("layers");
                    assert_eq!((layers.plane, layers.raw_window), (plane, raw));
                    let base = p.compose_display(&layers, center, width);
                    let plain =
                        reference_rgba_slice_display(&p, plane, index, raw, center, width, None)
                            .expect("reference");
                    assert_eq!(base, plain, "{plane:?} {index} {raw} {center} {width}");
                    let want = reference_rgba_slice_display(
                        &p,
                        plane,
                        index,
                        raw,
                        center,
                        width,
                        Some(crosshair),
                    )
                    .expect("reference");
                    let got = p
                        .rgba_slice_display(plane, index, raw, center, width, Some(crosshair))
                        .expect("wrapper");
                    assert_eq!(got, want, "{plane:?} {index} {raw} {center} {width}");
                    let mut staged = base.2.clone();
                    p.draw_display_crosshair(plane, base.0, base.1, &mut staged, crosshair);
                    crosshair_drawn |= staged != base.2;
                    assert_eq!((base.0, base.1, staged), want);
                    compared += 1;
                    if raw {
                        if let Some(prev) = &previous {
                            window_changed_bytes |= *prev != base.2;
                        }
                        previous = Some(base.2.clone());
                    }
                }
            }
            let far = (99, 0, 99);
            let layers = p.slice_layers(plane, 2, true).expect("layers");
            let (w, h, mut staged) = p.compose_display(&layers, 40.0, 80.0);
            p.draw_display_crosshair(plane, w, h, &mut staged, far);
            assert_eq!(
                (w, h, staged),
                reference_rgba_slice_display(&p, plane, 2, true, 40.0, 80.0, Some(far))
                    .expect("reference")
            );
            let idx = p.default_index(plane);
            assert_eq!(
                p.rgba_slice(plane, idx).expect("rgba"),
                reference_rgba_slice_display(&p, plane, idx, false, 0.0, 1.0, None)
                    .expect("reference")
            );
            assert_eq!(
                p.rgba_slice_windowed(plane, idx, 120.0, 0.0)
                    .expect("rgba windowed"),
                reference_rgba_slice_display(&p, plane, idx, true, 120.0, 0.0, None)
                    .expect("reference")
            );
        }
        assert_eq!(compared, 3 * 2 * 2);
        assert!(window_changed_bytes);
        assert!(crosshair_drawn);
    }
}
