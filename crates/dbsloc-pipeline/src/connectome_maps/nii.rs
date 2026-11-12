// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Uncompressed float32 NIfTI-1 images and resampling for the connectivity maps (`specs/15` §9.2).

use std::path::Path;

use rayon::prelude::*;

use super::{MapError, MapResult};

/// A 4 x 4 row-major voxel-to-mm affine (0-based voxel indices).
pub type Affine = [[f64; 4]; 4];

/// A 3-D image in memory: `x`-fastest `f32` data and its 0-based voxel-to-mm affine.
#[derive(Clone, Debug, PartialEq)]
pub struct NiiVolume {
    pub dims: [usize; 3],
    pub affine: Affine,
    pub data: Vec<f32>,
}

/// How a source image is sampled at a non-grid position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Nearest,
    Trilinear,
}

/// Maps `v` through `m`.
pub fn apply(m: &Affine, v: [f64; 3]) -> [f64; 3] {
    let row = |r: usize| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2] + m[r][3];
    [row(0), row(1), row(2)]
}

/// The inverse of an affine, or `None` when it is singular.
pub fn invert_affine(m: &Affine) -> Option<Affine> {
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
    let d = 1.0 / det;
    let inv = [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * d,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * d,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * d,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * d,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * d,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * d,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * d,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * d,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * d,
        ],
    ];
    let t = [m[0][3], m[1][3], m[2][3]];
    let mut out = [[0.0; 4]; 4];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = inv[r][c];
        }
        out[r][3] = -(inv[r][0] * t[0] + inv[r][1] * t[1] + inv[r][2] * t[2]);
    }
    out[3][3] = 1.0;
    Some(out)
}

/// An axis-aligned grid affine: `size` mm voxels with 0-based `origin`.
pub fn diagonal_affine(size: [f64; 3], origin: [f64; 3]) -> Affine {
    [
        [size[0], 0.0, 0.0, origin[0]],
        [0.0, size[1], 0.0, origin[1]],
        [0.0, 0.0, size[2], origin[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// The voxel sizes (column norms of the 3 x 3 part).
pub fn voxel_sizes(m: &Affine) -> [f64; 3] {
    let n = |c: usize| (m[0][c] * m[0][c] + m[1][c] * m[1][c] + m[2][c] * m[2][c]).sqrt();
    [n(0), n(1), n(2)]
}

fn translated(affine: &Affine, lo: [usize; 3]) -> Affine {
    let mut out = *affine;
    let origin = apply(affine, [lo[0] as f64, lo[1] as f64, lo[2] as f64]);
    for (r, o) in origin.iter().enumerate() {
        out[r][3] = *o;
    }
    out
}

impl NiiVolume {
    /// An all-zero image.
    pub fn zeros(dims: [usize; 3], affine: Affine) -> Self {
        Self {
            dims,
            affine,
            data: vec![0.0; dims[0] * dims[1] * dims[2]],
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Linear `x`-fastest index of voxel `[i, j, k]`.
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        (k * self.dims[1] + j) * self.dims[0] + i
    }

    /// The `[i, j, k]` of linear index `n`.
    pub fn ijk(&self, n: usize) -> [usize; 3] {
        let nx = self.dims[0];
        let ny = self.dims[1];
        [n % nx, (n / nx) % ny, n / (nx * ny)]
    }

    /// The voxel centre of linear index `n` in mm.
    pub fn centre(&self, n: usize) -> [f64; 3] {
        let [i, j, k] = self.ijk(n);
        apply(&self.affine, [i as f64, j as f64, k as f64])
    }

    /// Samples at the 0-based voxel position `p`; outside the volume is 0.
    pub fn sample(&self, p: [f64; 3], interp: Interp) -> f32 {
        let [nx, ny, nz] = self.dims;
        if p.iter().any(|c| !c.is_finite()) {
            return 0.0;
        }
        match interp {
            Interp::Nearest => {
                let (i, j, k) = (p[0].round(), p[1].round(), p[2].round());
                if i < 0.0 || j < 0.0 || k < 0.0 {
                    return 0.0;
                }
                let (i, j, k) = (i as usize, j as usize, k as usize);
                if i >= nx || j >= ny || k >= nz {
                    return 0.0;
                }
                self.data[self.index(i, j, k)]
            }
            Interp::Trilinear => {
                if p[0] < -0.5
                    || p[1] < -0.5
                    || p[2] < -0.5
                    || p[0] > nx as f64 - 0.5
                    || p[1] > ny as f64 - 0.5
                    || p[2] > nz as f64 - 0.5
                {
                    return 0.0;
                }
                let f = p.map(f64::floor);
                let t = [p[0] - f[0], p[1] - f[1], p[2] - f[2]];
                let mut acc = 0.0f64;
                for dk in 0..2i64 {
                    for dj in 0..2i64 {
                        for di in 0..2i64 {
                            let w = (if di == 1 { t[0] } else { 1.0 - t[0] })
                                * (if dj == 1 { t[1] } else { 1.0 - t[1] })
                                * (if dk == 1 { t[2] } else { 1.0 - t[2] });
                            if w == 0.0 {
                                continue;
                            }
                            let i = f[0] as i64 + di;
                            let j = f[1] as i64 + dj;
                            let k = f[2] as i64 + dk;
                            if i < 0
                                || j < 0
                                || k < 0
                                || i >= nx as i64
                                || j >= ny as i64
                                || k >= nz as i64
                            {
                                continue;
                            }
                            let v = f64::from(
                                self.data[self.index(i as usize, j as usize, k as usize)],
                            );
                            if v.is_finite() {
                                acc += w * v;
                            }
                        }
                    }
                }
                acc as f32
            }
        }
    }
}

/// The voxel box `[lo, hi)` of the grid (`dst_affine`, `dst_dims`) that covers the world bounding
/// box of `src`; `None` when they do not overlap.
pub fn covering_box(
    src: &NiiVolume,
    dst_affine: &Affine,
    dst_dims: [usize; 3],
) -> Option<([usize; 3], [usize; 3])> {
    let inv = invert_affine(dst_affine)?;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for corner in 0..8u8 {
        let edge = |bit: u8, n: usize| {
            if corner & bit == 0 {
                -0.5
            } else {
                n as f64 - 0.5
            }
        };
        let v = [
            edge(1, src.dims[0]),
            edge(2, src.dims[1]),
            edge(4, src.dims[2]),
        ];
        let d = apply(&inv, apply(&src.affine, v));
        for a in 0..3 {
            lo[a] = lo[a].min(d[a]);
            hi[a] = hi[a].max(d[a]);
        }
    }
    let mut blo = [0usize; 3];
    let mut bhi = [0usize; 3];
    for a in 0..3 {
        if !lo[a].is_finite() || !hi[a].is_finite() {
            return None;
        }
        let l = (lo[a].floor() - 1.0).max(0.0);
        let h = (hi[a].ceil() + 2.0).min(dst_dims[a] as f64);
        if h <= l {
            return None;
        }
        blo[a] = l as usize;
        bhi[a] = h as usize;
    }
    Some((blo, bhi))
}

/// Resamples `src` onto the voxels `[lo, hi)` of the grid `dst_affine`; the result has the
/// dimensions `hi - lo` and an affine translated to `lo`. NaN samples become 0.
pub fn resample_box(
    src: &NiiVolume,
    dst_affine: &Affine,
    lo: [usize; 3],
    hi: [usize; 3],
    interp: Interp,
) -> MapResult<NiiVolume> {
    let inv_src = invert_affine(&src.affine)
        .ok_or_else(|| MapError::Format("image affine is singular".into()))?;
    let dims = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let mut out = NiiVolume::zeros(dims, translated(dst_affine, lo));
    let slab = dims[0] * dims[1];
    if slab == 0 {
        return Ok(out);
    }
    out.data
        .par_chunks_mut(slab)
        .enumerate()
        .for_each(|(k, chunk)| {
            for j in 0..dims[1] {
                for i in 0..dims[0] {
                    let w = apply(
                        dst_affine,
                        [(lo[0] + i) as f64, (lo[1] + j) as f64, (lo[2] + k) as f64],
                    );
                    let v = src.sample(apply(&inv_src, w), interp);
                    chunk[j * dims[0] + i] = if v.is_finite() { v } else { 0.0 };
                }
            }
        });
    Ok(out)
}

/// Resamples `src` onto the whole grid `dims` / `affine`.
pub fn resample(
    src: &NiiVolume,
    dims: [usize; 3],
    affine: &Affine,
    interp: Interp,
) -> MapResult<NiiVolume> {
    resample_box(src, affine, [0, 0, 0], dims, interp)
}

/// Crops `vol` to the bounding box of its non-zero finite voxels plus `rim` voxels (clamped to
/// the volume); `None` when it has no such voxel.
pub fn crop_to_content(vol: &NiiVolume, rim: usize) -> Option<NiiVolume> {
    let [nx, ny, nz] = vol.dims;
    let mut lo = [usize::MAX; 3];
    let mut hi = [0usize; 3];
    let mut any = false;
    for (n, v) in vol.data.iter().enumerate() {
        if v.is_finite() && *v != 0.0 {
            let c = vol.ijk(n);
            for a in 0..3 {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
            }
            any = true;
        }
    }
    if !any {
        return None;
    }
    let lo = [
        lo[0].saturating_sub(rim),
        lo[1].saturating_sub(rim),
        lo[2].saturating_sub(rim),
    ];
    let hi = [
        (hi[0] + rim + 1).min(nx),
        (hi[1] + rim + 1).min(ny),
        (hi[2] + rim + 1).min(nz),
    ];
    let dims = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let mut out = NiiVolume::zeros(dims, translated(&vol.affine, lo));
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            let src = vol.index(lo[0], lo[1] + j, lo[2] + k);
            let dst = (k * dims[1] + j) * dims[0];
            out.data[dst..dst + dims[0]].copy_from_slice(&vol.data[src..src + dims[0]]);
        }
    }
    Some(out)
}

/// Reads a `.nii` / `.nii.gz` image (first volume) as `f32`.
pub fn read_nii(path: &Path) -> MapResult<NiiVolume> {
    let (arr, aff) =
        crate::nifti_io::load_volume_f64(path).map_err(|e| MapError::Io(e.to_string()))?;
    let dims = [arr.shape()[0], arr.shape()[1], arr.shape()[2]];
    let mut data = vec![0f32; dims[0] * dims[1] * dims[2]];
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                data[(k * dims[1] + j) * dims[0] + i] = arr[[i, j, k]] as f32;
            }
        }
    }
    Ok(NiiVolume {
        dims,
        affine: aff,
        data,
    })
}

fn put_i16(h: &mut [u8], at: usize, v: i16) {
    h[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(h: &mut [u8], at: usize, v: f32) {
    h[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// The bytes of an uncompressed float32 NIfTI-1 file with an s-form affine.
pub fn encode_nii(vol: &NiiVolume) -> Vec<u8> {
    let mut h = vec![0u8; 352];
    h[0..4].copy_from_slice(&348i32.to_le_bytes());
    put_i16(&mut h, 40, 3);
    for a in 0..3 {
        put_i16(
            &mut h,
            42 + 2 * a,
            vol.dims[a].min(i16::MAX as usize) as i16,
        );
    }
    for a in 4..8 {
        put_i16(&mut h, 40 + 2 * a, 1);
    }
    put_i16(&mut h, 70, 16);
    put_i16(&mut h, 72, 32);
    put_f32(&mut h, 76, 1.0);
    let sizes = voxel_sizes(&vol.affine);
    for (a, s) in sizes.iter().enumerate() {
        put_f32(&mut h, 80 + 4 * a, *s as f32);
    }
    put_f32(&mut h, 108, 352.0);
    put_f32(&mut h, 112, 1.0);
    h[123] = 2;
    put_i16(&mut h, 254, 1);
    for r in 0..3 {
        for c in 0..4 {
            put_f32(&mut h, 280 + 16 * r + 4 * c, vol.affine[r][c] as f32);
        }
    }
    h[344..348].copy_from_slice(b"n+1\0");
    h.reserve(vol.data.len() * 4);
    for v in &vol.data {
        h.extend_from_slice(&v.to_le_bytes());
    }
    h
}

/// Writes `vol` as an uncompressed float32 NIfTI-1 file at exactly `path`.
pub fn write_nii(path: &Path, vol: &NiiVolume) -> MapResult<()> {
    std::fs::write(path, encode_nii(vol))
        .map_err(|e| MapError::Io(format!("{}: {e}", path.display())))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn cube() -> NiiVolume {
        let mut v = NiiVolume::zeros(
            [3, 4, 5],
            diagonal_affine([2.0, 1.0, 0.5], [-1.0, -2.0, 3.0]),
        );
        for (n, x) in v.data.iter_mut().enumerate() {
            *x = n as f32 * 0.5 - 3.0;
        }
        v
    }

    #[test]
    fn nii_file_round_trips_through_the_reader() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.nii");
        let v = cube();
        write_nii(&p, &v).unwrap();
        let back = read_nii(&p).unwrap();
        assert_eq!(back.dims, v.dims);
        assert_eq!(back.data, v.data);
        for r in 0..4 {
            for c in 0..4 {
                assert!((back.affine[r][c] - v.affine[r][c]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn resampling_onto_the_same_grid_is_identity_and_outside_is_zero() {
        let v = cube();
        let same = resample(&v, v.dims, &v.affine, Interp::Trilinear).unwrap();
        assert_eq!(same.data, v.data);
        let near = resample(&v, v.dims, &v.affine, Interp::Nearest).unwrap();
        assert_eq!(near.data, v.data);
        assert_eq!(v.sample([-3.0, 0.0, 0.0], Interp::Nearest), 0.0);
        assert_eq!(
            v.sample([0.5, 0.0, 0.0], Interp::Trilinear),
            (v.data[0] + v.data[1]) / 2.0
        );
    }

    #[test]
    fn affine_inverse_composes_to_identity() {
        let m = cube().affine;
        let inv = invert_affine(&m).unwrap();
        let p = apply(&inv, apply(&m, [1.0, 2.0, 3.0]));
        for (a, b) in p.iter().zip([1.0, 2.0, 3.0]) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn crop_keeps_content_and_rim() {
        let mut v = NiiVolume::zeros([8, 8, 8], diagonal_affine([1.0; 3], [0.0; 3]));
        let n = v.index(3, 4, 5);
        v.data[n] = 2.0;
        let c = crop_to_content(&v, 1).unwrap();
        assert_eq!(c.dims, [3, 3, 3]);
        assert_eq!(c.affine[0][3], 2.0);
        assert_eq!(c.data[c.index(1, 1, 1)], 2.0);
        assert!(crop_to_content(&NiiVolume::zeros([2, 2, 2], v.affine), 1).is_none());
    }
}
