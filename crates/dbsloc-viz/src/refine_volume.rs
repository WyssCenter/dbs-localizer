// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use crate::error::{VizError, VizResult};

const MAX_SLICE_TEX_SIDE: f64 = 768.0;
const MAX_SLICE_TEX_CAP: u32 = 2048;
const PERCENTILE_SAMPLES: usize = 1_000_000;

pub type Affine = [[f64; 4]; 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefinePlane {
    Axial,
    Coronal,
    Sagittal,
}

impl RefinePlane {
    pub const ALL: [RefinePlane; 3] = [
        RefinePlane::Axial,
        RefinePlane::Coronal,
        RefinePlane::Sagittal,
    ];

    pub fn axes(self) -> (usize, usize, usize) {
        match self {
            RefinePlane::Axial => (0, 1, 2),
            RefinePlane::Coronal => (0, 2, 1),
            RefinePlane::Sagittal => (1, 2, 0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RefinePlane::Axial => "Axial",
            RefinePlane::Coronal => "Coronal",
            RefinePlane::Sagittal => "Sagittal",
        }
    }

    pub fn index(self) -> usize {
        match self {
            RefinePlane::Axial => 0,
            RefinePlane::Coronal => 1,
            RefinePlane::Sagittal => 2,
        }
    }
}

#[derive(Clone, Debug)]
pub struct VolumeGeometry {
    dims: [usize; 3],
    affine: Affine,
    inverse: Affine,
    spacing: [f64; 3],
}

fn invert_affine(m: &Affine) -> Option<Affine> {
    let a = [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv_det = 1.0 / det;
    let c = [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * inv_det,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * inv_det,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * inv_det,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * inv_det,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * inv_det,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * inv_det,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * inv_det,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * inv_det,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * inv_det,
        ],
    ];
    let t = [m[0][3], m[1][3], m[2][3]];
    let mut out = [[0.0; 4]; 4];
    for r in 0..3 {
        for col in 0..3 {
            out[r][col] = c[r][col];
        }
        out[r][3] = -(c[r][0] * t[0] + c[r][1] * t[1] + c[r][2] * t[2]);
    }
    out[3][3] = 1.0;
    Some(out)
}

fn apply(m: &Affine, p: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2] + m[0][3],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2] + m[1][3],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2] + m[2][3],
    ]
}

impl VolumeGeometry {
    pub fn new(dims: [usize; 3], affine: Affine) -> Option<Self> {
        if dims.contains(&0) || affine.iter().flatten().any(|v| !v.is_finite()) {
            return None;
        }
        let inverse = invert_affine(&affine)?;
        let mut spacing = [1.0; 3];
        for (axis, s) in spacing.iter_mut().enumerate() {
            let n = (affine[0][axis].powi(2) + affine[1][axis].powi(2) + affine[2][axis].powi(2))
                .sqrt();
            if n.is_finite() && n > 1e-9 {
                *s = n;
            }
        }
        Some(Self {
            dims,
            affine,
            inverse,
            spacing,
        })
    }

    pub fn dims(&self) -> [usize; 3] {
        self.dims
    }

    pub fn affine(&self) -> &Affine {
        &self.affine
    }

    pub fn spacing(&self) -> [f64; 3] {
        self.spacing
    }

    pub fn voxel_to_world(&self, ijk: [f64; 3]) -> [f64; 3] {
        apply(&self.affine, ijk)
    }

    pub fn world_to_voxel(&self, world: [f64; 3]) -> [f64; 3] {
        apply(&self.inverse, world)
    }

    pub fn world_vector_to_voxel(&self, v: [f64; 3]) -> [f64; 3] {
        let m = &self.inverse;
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }

    pub fn axis_direction(&self, axis: usize) -> [f64; 3] {
        let col = [
            self.affine[0][axis],
            self.affine[1][axis],
            self.affine[2][axis],
        ];
        let n = (col[0] * col[0] + col[1] * col[1] + col[2] * col[2]).sqrt();
        if n < 1e-12 {
            let mut unit = [0.0; 3];
            unit[axis] = 1.0;
            return unit;
        }
        [col[0] / n, col[1] / n, col[2] / n]
    }

    pub fn plane_extent_mm(&self, plane: RefinePlane) -> (f64, f64) {
        let (u, v, _) = plane.axes();
        (
            self.dims[u] as f64 * self.spacing[u],
            self.dims[v] as f64 * self.spacing[v],
        )
    }

    fn through_gradient_norm(&self, axis: usize) -> f64 {
        let row = &self.inverse[axis];
        (row[0] * row[0] + row[1] * row[1] + row[2] * row[2]).sqrt()
    }

    pub fn plane_uv_for_world(
        &self,
        plane: RefinePlane,
        world: [f64; 3],
        slice_index: usize,
    ) -> (f64, f64, f64) {
        let ijk = self.world_to_voxel(world);
        let (ua, va, ta) = plane.axes();
        let u = (ijk[ua] + 0.5) / self.dims[ua] as f64;
        let v = (ijk[va] + 0.5) / self.dims[va] as f64;
        let grad = self.through_gradient_norm(ta).max(1e-12);
        let dist = (ijk[ta] - slice_index as f64) / grad;
        (u, v, dist)
    }

    pub fn world_for_plane_uv(&self, plane: RefinePlane, u: f64, v: f64, slice: f64) -> [f64; 3] {
        let (ua, va, ta) = plane.axes();
        let mut ijk = [0.0; 3];
        ijk[ua] = u * self.dims[ua] as f64 - 0.5;
        ijk[va] = v * self.dims[va] as f64 - 0.5;
        ijk[ta] = slice;
        self.voxel_to_world(ijk)
    }

    pub fn through_index(&self, plane: RefinePlane, world: [f64; 3]) -> f64 {
        let ijk = self.world_to_voxel(world);
        ijk[plane.axes().2]
    }

    pub fn clamp_index(&self, ijk: [f64; 3]) -> [usize; 3] {
        let mut out = [0usize; 3];
        for axis in 0..3 {
            let top = (self.dims[axis] - 1) as f64;
            out[axis] = ijk[axis].round().clamp(0.0, top) as usize;
        }
        out
    }
}

#[derive(Default)]
pub struct SliceScratch {
    raw: Vec<f32>,
    rgba: Vec<u8>,
}

impl SliceScratch {
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

pub struct RefineVolume {
    geometry: VolumeGeometry,
    data: Vec<f32>,
    range: (f32, f32),
    is_ct: bool,
}

impl RefineVolume {
    pub fn load(path: &Path) -> VizResult<Self> {
        let vol = dbsloc_checkreg::nifti_io::read_volume_f32(path)?;
        let affine = fastwarp_core::nifti_affine::header_affine(&vol.header);
        Self::from_parts(vol.dims, affine, vol.data)
    }

    pub fn with_ct_hint(mut self, is_ct: Option<bool>) -> Self {
        if let Some(ct) = is_ct {
            self.is_ct = ct;
        }
        self
    }

    pub fn from_parts(dims: [usize; 3], affine: Affine, data: Vec<f32>) -> VizResult<Self> {
        if data.len() != dims[0] * dims[1] * dims[2] {
            return Err(VizError::NiftiLoad(
                "voxel count does not match dims".into(),
            ));
        }
        let geometry = VolumeGeometry::new(dims, affine)
            .ok_or_else(|| VizError::NiftiLoad("singular voxel-to-world affine".into()))?;
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for &v in &data {
            if v.is_finite() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if !lo.is_finite() || !hi.is_finite() {
            lo = 0.0;
            hi = 1.0;
        }
        let is_ct = lo < -500.0 && hi > 1000.0;
        Ok(Self {
            geometry,
            data,
            range: (lo, hi),
            is_ct,
        })
    }

    pub fn geometry(&self) -> &VolumeGeometry {
        &self.geometry
    }

    pub fn dims(&self) -> [usize; 3] {
        self.geometry.dims()
    }

    pub fn intensity_range(&self) -> (f32, f32) {
        self.range
    }

    pub fn is_ct(&self) -> bool {
        self.is_ct
    }

    pub fn world_to_voxel_f(&self, world: [f64; 3]) -> [f64; 3] {
        self.geometry.world_to_voxel(world)
    }

    pub fn voxel_to_world_f(&self, ijk: [f64; 3]) -> [f64; 3] {
        self.geometry.voxel_to_world(ijk)
    }

    pub fn default_window(&self) -> (f32, f32) {
        if self.is_ct {
            return (300.0, 1500.0);
        }
        let step = (self.data.len() / PERCENTILE_SAMPLES).max(1);
        let mut sample: Vec<f32> = self
            .data
            .iter()
            .step_by(step)
            .copied()
            .filter(|v| v.is_finite())
            .collect();
        if sample.is_empty() {
            return (0.5, 1.0);
        }
        sample.sort_by(|a, b| a.total_cmp(b));
        let pick = |pct: f64| {
            let pos = ((sample.len() - 1) as f64 * pct / 100.0).round() as usize;
            sample[pos.min(sample.len() - 1)]
        };
        let lo = pick(0.5);
        let hi = pick(99.5);
        let width = (hi - lo).max(1e-3);
        (lo + 0.5 * width, width)
    }

    fn voxel(&self, i: usize, j: usize, k: usize) -> f32 {
        let [n0, n1, _] = self.geometry.dims;
        self.data[k * n1 * n0 + j * n0 + i]
    }

    pub fn sample_trilinear(&self, ijk: [f64; 3]) -> f32 {
        let dims = self.geometry.dims;
        let mut lo = [0usize; 3];
        let mut hi = [0usize; 3];
        let mut t = [0f32; 3];
        for a in 0..3 {
            let n = dims[a];
            let x = ijk[a];
            if !(x >= -0.5 && x <= n as f64 - 0.5) {
                return f32::NAN;
            }
            let x = x.clamp(0.0, (n - 1) as f64);
            let f = x.floor();
            lo[a] = f as usize;
            hi[a] = (lo[a] + 1).min(n - 1);
            t[a] = (x - f) as f32;
        }
        let v = |i: usize, j: usize, k: usize| self.voxel(i, j, k);
        let c00 = v(lo[0], lo[1], lo[2]) * (1.0 - t[0]) + v(hi[0], lo[1], lo[2]) * t[0];
        let c10 = v(lo[0], hi[1], lo[2]) * (1.0 - t[0]) + v(hi[0], hi[1], lo[2]) * t[0];
        let c01 = v(lo[0], lo[1], hi[2]) * (1.0 - t[0]) + v(hi[0], lo[1], hi[2]) * t[0];
        let c11 = v(lo[0], hi[1], hi[2]) * (1.0 - t[0]) + v(hi[0], hi[1], hi[2]) * t[0];
        let c0 = c00 * (1.0 - t[1]) + c10 * t[1];
        let c1 = c01 * (1.0 - t[1]) + c11 * t[1];
        c0 * (1.0 - t[2]) + c1 * t[2]
    }

    fn raw_slice_into(
        &self,
        plane: RefinePlane,
        index: usize,
        out: &mut Vec<f32>,
    ) -> (usize, usize) {
        let [n0, n1, n2] = self.geometry.dims;
        out.clear();
        match plane {
            RefinePlane::Axial => {
                let k = index.min(n2 - 1);
                let start = k * n1 * n0;
                out.extend_from_slice(&self.data[start..start + n0 * n1]);
                (n0, n1)
            }
            RefinePlane::Coronal => {
                let j = index.min(n1 - 1);
                out.resize(n0 * n2, 0.0);
                for k in 0..n2 {
                    for i in 0..n0 {
                        out[i + k * n0] = self.voxel(i, j, k);
                    }
                }
                (n0, n2)
            }
            RefinePlane::Sagittal => {
                let i0 = index.min(n0 - 1);
                out.resize(n1 * n2, 0.0);
                for k in 0..n2 {
                    for j in 0..n1 {
                        out[j + k * n1] = self.voxel(i0, j, k);
                    }
                }
                (n1, n2)
            }
        }
    }

    pub fn slice_rgba(
        &self,
        plane: RefinePlane,
        index: usize,
        center: f32,
        width: f32,
    ) -> (u32, u32, Vec<u8>) {
        let mut scratch = SliceScratch::default();
        let (w, h) = self.slice_rgba_into(plane, index, center, width, &mut scratch);
        (w, h, scratch.rgba)
    }

    pub fn slice_rgba_into(
        &self,
        plane: RefinePlane,
        index: usize,
        center: f32,
        width: f32,
        scratch: &mut SliceScratch,
    ) -> (u32, u32) {
        let (sw, sh) = self.raw_slice_into(plane, index, &mut scratch.raw);
        let src = &scratch.raw;
        let (pw, ph) = self.geometry.plane_extent_mm(plane);
        let scale = MAX_SLICE_TEX_SIDE / pw.max(ph).max(1e-3);
        let out_w = ((pw * scale).round() as u32).clamp(1, MAX_SLICE_TEX_CAP);
        let out_h = ((ph * scale).round() as u32).clamp(1, MAX_SLICE_TEX_CAP);
        let width = width.max(1e-3);
        let lo = center - 0.5 * width;
        let out = &mut scratch.rgba;
        out.clear();
        out.resize((out_w * out_h * 4) as usize, 255u8);
        let sample = |x: usize, y: usize| {
            let v = src[x + y * sw];
            if v.is_finite() {
                v
            } else {
                lo
            }
        };
        for y in 0..out_h {
            let fy =
                ((y as f32 + 0.5) / out_h as f32 * sh as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
            let y0 = fy.floor() as usize;
            let y1 = (y0 + 1).min(sh - 1);
            let ty = fy - y0 as f32;
            for x in 0..out_w {
                let fx =
                    ((x as f32 + 0.5) / out_w as f32 * sw as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
                let x0 = fx.floor() as usize;
                let x1 = (x0 + 1).min(sw - 1);
                let tx = fx - x0 as f32;
                let top = sample(x0, y0) * (1.0 - tx) + sample(x1, y0) * tx;
                let bottom = sample(x0, y1) * (1.0 - tx) + sample(x1, y1) * tx;
                let v = top * (1.0 - ty) + bottom * ty;
                let g = (((v - lo) / width).clamp(0.0, 1.0) * 255.0).round() as u8;
                let o = ((y * out_w + x) * 4) as usize;
                out[o] = g;
                out[o + 1] = g;
                out[o + 2] = g;
            }
        }
        (out_w, out_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn oblique_affine() -> Affine {
        let (s, c) = (0.5_f64.sin(), 0.5_f64.cos());
        [
            [0.8 * c, -1.1 * s, 0.0, -20.0],
            [0.8 * s, 1.1 * c, 0.0, 15.0],
            [0.0, 0.0, 1.5, -30.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn oblique_geometry() -> VolumeGeometry {
        VolumeGeometry::new([20, 24, 16], oblique_affine()).unwrap()
    }

    #[test]
    fn pane_mapping_round_trip_on_oblique_affine() {
        let g = oblique_geometry();
        for plane in RefinePlane::ALL {
            for &(i, j, k) in &[(3usize, 5usize, 7usize), (10, 12, 8), (0, 23, 15)] {
                let world = g.voxel_to_world([i as f64, j as f64, k as f64]);
                let (_, _, ta) = plane.axes();
                let slice = [i, j, k][ta];
                let (u, v, dist) = g.plane_uv_for_world(plane, world, slice);
                assert!(dist.abs() < 1e-9);
                let back = g.world_for_plane_uv(plane, u, v, slice as f64);
                for a in 0..3 {
                    assert!((back[a] - world[a]).abs() < 1e-4);
                }
            }
        }
    }

    #[test]
    fn through_distance_is_perpendicular_millimetres() {
        let g = oblique_geometry();
        let world = g.voxel_to_world([4.0, 6.0, 8.0]);
        let (_, _, dist) = g.plane_uv_for_world(RefinePlane::Axial, world, 6);
        assert!((dist - 2.0 * 1.5).abs() < 1e-9);
        let (_, _, dist) = g.plane_uv_for_world(RefinePlane::Sagittal, world, 2);
        let expected = 2.0 * 0.8;
        assert!((dist - expected).abs() < 1e-9, "{dist}");
    }

    #[test]
    fn v_increases_with_voxel_index() {
        let g = oblique_geometry();
        let a = g.voxel_to_world([4.0, 5.0, 3.0]);
        let b = g.voxel_to_world([4.0, 6.0, 3.0]);
        let (_, va, _) = g.plane_uv_for_world(RefinePlane::Axial, a, 3);
        let (_, vb, _) = g.plane_uv_for_world(RefinePlane::Axial, b, 3);
        assert!(vb > va);
    }

    #[test]
    fn non_finite_affine_is_rejected() {
        let mut a = oblique_affine();
        a[0][3] = f64::NAN;
        assert!(VolumeGeometry::new([4, 4, 4], a).is_none());
        let mut a = oblique_affine();
        a[1][3] = f64::INFINITY;
        assert!(VolumeGeometry::new([4, 4, 4], a).is_none());
    }

    #[test]
    fn ct_hint_overrides_the_intensity_heuristic() {
        let dims = [4usize, 4, 4];
        let affine = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let vol = RefineVolume::from_parts(dims, affine, vec![5.0; 64]).unwrap();
        assert!(!vol.is_ct());
        let vol = vol.with_ct_hint(Some(true));
        assert_eq!(vol.default_window(), (300.0, 1500.0));
        let vol = vol.with_ct_hint(None);
        assert!(vol.is_ct());
    }

    #[test]
    fn singular_affine_is_rejected() {
        let mut a = oblique_affine();
        a[2][2] = 0.0;
        assert!(VolumeGeometry::new([4, 4, 4], a).is_none());
    }

    #[test]
    fn slice_is_physical_aspect_and_windowed() {
        let dims = [10usize, 20, 5];
        let affine = [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 3.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let data: Vec<f32> = (0..dims[0] * dims[1] * dims[2])
            .map(|n| (n % 10) as f32)
            .collect();
        let vol = RefineVolume::from_parts(dims, affine, data).unwrap();
        let (w, h, px) = vol.slice_rgba(RefinePlane::Axial, 2, 4.5, 9.0);
        assert_eq!(w, h);
        assert_eq!(px.len(), (w * h * 4) as usize);
        assert_eq!(px[3], 255);
        let (w, h, _) = vol.slice_rgba(RefinePlane::Coronal, 3, 4.5, 9.0);
        assert!(w > h);
        assert!(!vol.is_ct());
    }

    #[test]
    fn ct_range_selects_bone_window() {
        let dims = [4usize, 4, 4];
        let mut data = vec![0.0f32; 64];
        data[0] = -1000.0;
        data[1] = 2500.0;
        let affine = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let vol = RefineVolume::from_parts(dims, affine, data).unwrap();
        assert!(vol.is_ct());
        assert_eq!(vol.default_window(), (300.0, 1500.0));
    }

    #[test]
    fn mr_window_uses_percentiles() {
        let dims = [10usize, 10, 10];
        let data: Vec<f32> = (0..1000).map(|n| n as f32).collect();
        let affine = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let vol = RefineVolume::from_parts(dims, affine, data).unwrap();
        let (c, w) = vol.default_window();
        assert!((c - 499.5).abs() < 2.0);
        assert!((w - 990.0).abs() < 4.0);
    }
}
