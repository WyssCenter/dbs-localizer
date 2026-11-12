// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Load template NIfTI for interactive slices (normalized grey).

use std::path::Path;

#[cfg(feature = "web")]
use nifti::InMemNiftiObject;
use nifti::NiftiHeader;
#[cfg(feature = "scene-viewer")]
use rayon::prelude::*;

use crate::error::{VizError, VizResult};
#[cfg(feature = "web")]
use dbsloc_checkreg::nifti_io::check_header_dims;
#[cfg(feature = "web")]
use dbsloc_checkreg::nifti_io::volume_f32_from_object;

/// True when `bytes` starts with the gzip magic (`1f 8b`).
#[cfg(feature = "web")]
fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

pub struct TemplateVolume {
    pub n0: usize,
    pub n1: usize,
    pub n2: usize,
    pub hdr: NiftiHeader,
    /// Flatten `k * n1 * n0 + j * n0 + i`, values normalized 0..1
    pub normed: Vec<f32>,
}

impl TemplateVolume {
    pub fn load(path: &Path) -> VizResult<Self> {
        Self::from_decoded(dbsloc_checkreg::nifti_io::read_volume_f32(path)?)
    }

    /// Byte-based loader for the browser viewer (`docs/SERVER_CLIENT_PLAN.md` §6.1): parse a NIfTI
    /// buffer already in memory instead of a path. Gzip-aware — a `.nii.gz` payload (gzip magic
    /// `1f 8b`) is decoded through pure-Rust `flate2` (`miniz_oxide`), which runs on `wasm32`.
    /// The header is read and checked before any volume buffer is allocated.
    #[cfg(feature = "web")]
    pub fn from_bytes(bytes: &[u8]) -> VizResult<Self> {
        let gz = is_gzip(bytes);
        let header = if gz {
            NiftiHeader::from_reader(flate2::read::GzDecoder::new(std::io::Cursor::new(bytes)))
        } else {
            NiftiHeader::from_reader(std::io::Cursor::new(bytes))
        }
        .map_err(|e| VizError::NiftiLoad(e.to_string()))?;
        check_header_dims(&header)?;
        let obj = if gz {
            let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
            InMemNiftiObject::from_reader(decoder)
        } else {
            InMemNiftiObject::from_reader(std::io::Cursor::new(bytes))
        }
        .map_err(|e| VizError::NiftiLoad(e.to_string()))?;
        Self::from_inmem(obj)
    }

    /// Shared body of [`load`] / [`from_bytes`]: normalize an in-memory NIfTI object into a
    /// `0..1` grey volume.
    #[cfg(feature = "web")]
    fn from_inmem(obj: nifti::InMemNiftiObject) -> VizResult<Self> {
        Self::from_decoded(volume_f32_from_object(obj)?)
    }

    fn from_decoded(decoded: dbsloc_checkreg::nifti_io::VolumeF32) -> VizResult<Self> {
        let hdr = decoded.header;
        let [n0, n1, n2] = decoded.dims;
        let mut all = decoded.data;
        for v in all.iter_mut() {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        let (lo, hi) = percentile_range(&all);
        let range = (hi - lo).max(1e-9);
        for v in all.iter_mut() {
            *v = (((*v as f64 - lo) / range).clamp(0.0, 1.0)) as f32;
        }
        Ok(Self {
            n0,
            n1,
            n2,
            hdr,
            normed: all,
        })
    }

    #[inline]
    fn at(&self, i: usize, j: usize, k: usize) -> f32 {
        self.normed[k * self.n1 * self.n0 + j * self.n0 + i]
    }

    /// Sagittal slice `i` fixed; width=n1, height=n2. Rows follow **superior-up** (row 0 = high `k`).
    /// Layout matches checkreg-style indexing: each row is one `k`, columns are `j`, with `k` reversed
    /// so WebGPU `v=0` (texture top) maps to superior (head-up in typical MNI templates).
    pub fn sagittal_r8(&self, i: usize) -> (Vec<u8>, u32, u32, u32) {
        let i = i.min(self.n0.saturating_sub(1));
        let w = self.n1 as u32;
        let h = self.n2 as u32;
        let bpr = w.div_ceil(256).max(1) * 256;
        let mut buf = vec![0u8; bpr as usize * h as usize];
        for row in 0..self.n2 {
            let k = self.n2 - 1 - row;
            for j in 0..self.n1 {
                let v = (self.at(i, j, k).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                let dst = row * bpr as usize + j;
                buf[dst] = v;
            }
        }
        (buf, bpr, w, h)
    }

    /// Axial slice `k` fixed; width=n1 (j), height=n0 (i) — same layout as `mni_slice`.
    pub fn axial_r8(&self, k: usize) -> (Vec<u8>, u32, u32, u32) {
        let k = k.min(self.n2.saturating_sub(1));
        let w = self.n1 as u32;
        let h = self.n0 as u32;
        let bpr = w.div_ceil(256).max(1) * 256;
        let mut buf = vec![0u8; bpr as usize * h as usize];
        for i in 0..self.n0 {
            for j in 0..self.n1 {
                let v = (self.at(i, j, k).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                let dst = i * bpr as usize + j;
                buf[dst] = v;
            }
        }
        (buf, bpr, w, h)
    }

    /// Coronal slice `j` fixed; width=n0, height=n2. **Superior-up** row order (like `sagittal_r8`).
    pub fn coronal_r8(&self, j: usize) -> (Vec<u8>, u32, u32, u32) {
        let j = j.min(self.n1.saturating_sub(1));
        let w = self.n0 as u32;
        let h = self.n2 as u32;
        let bpr = w.div_ceil(256).max(1) * 256;
        let mut buf = vec![0u8; bpr as usize * h as usize];
        for row in 0..self.n2 {
            let k = self.n2 - 1 - row;
            for col in 0..self.n0 {
                let v = (self.at(col, j, k).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                let dst = row * bpr as usize + col;
                buf[dst] = v;
            }
        }
        (buf, bpr, w, h)
    }
}

/// Minimum and 99.5th percentile of `values` (finite), using one selection pass over one copy.
fn percentile_range(values: &[f32]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let lo = values.iter().copied().fold(f32::INFINITY, f32::min);
    let idx = ((values.len() as f64 * 0.995) as usize).min(values.len() - 1);
    let mut copy = values.to_vec();
    let (_, hi, _) = copy.select_nth_unstable_by(idx, f32::total_cmp);
    (f64::from(lo), f64::from(*hi))
}

/// Voxel → world affine of a header (`specs/01` §2: sform, then qform, then pixdim diagonal),
/// in single precision for per-voxel and per-vertex use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoxelWorld {
    m: [[f32; 4]; 3],
}

impl VoxelWorld {
    fn from_mat4(a: &fastwarp_core::Mat4) -> Self {
        let mut m = [[0.0_f32; 4]; 3];
        for (dst, src) in m.iter_mut().zip(a.iter()) {
            for (d, s) in dst.iter_mut().zip(src.iter()) {
                *d = *s as f32;
            }
        }
        Self { m }
    }

    /// Voxel indices → world mm.
    pub fn from_header(hdr: &NiftiHeader) -> Self {
        Self::from_mat4(&fastwarp_core::nifti_affine::header_affine(hdr))
    }

    /// World mm → continuous voxel indices. A singular affine falls back to dividing by the
    /// (clamped) pixdim spacing.
    pub fn world_to_voxel(hdr: &NiftiHeader) -> Self {
        let a = fastwarp_core::nifti_affine::header_affine(hdr);
        match fastwarp_core::invert_rowmajor(&a) {
            Some(inv) => Self::from_mat4(&inv),
            None => {
                let s = |v: f32| 1.0 / v.max(1e-6);
                Self {
                    m: [
                        [s(hdr.pixdim[1]), 0.0, 0.0, 0.0],
                        [0.0, s(hdr.pixdim[2]), 0.0, 0.0],
                        [0.0, 0.0, s(hdr.pixdim[3]), 0.0],
                    ],
                }
            }
        }
    }

    /// 3×3 linear part (no translation).
    pub fn linear(&self) -> [[f32; 3]; 3] {
        let m = &self.m;
        [
            [m[0][0], m[0][1], m[0][2]],
            [m[1][0], m[1][1], m[1][2]],
            [m[2][0], m[2][1], m[2][2]],
        ]
    }

    #[inline]
    pub fn apply(&self, x: f32, y: f32, z: f32) -> glam::Vec3 {
        let m = &self.m;
        glam::Vec3::new(
            m[0][0] * x + m[0][1] * y + m[0][2] * z + m[0][3],
            m[1][0] * x + m[1][1] * y + m[1][2] * z + m[1][3],
            m[2][0] * x + m[2][1] * y + m[2][2] * z + m[2][3],
        )
    }
}

/// World mm from voxel indices (continuous).
pub fn ijk_to_world_mm(hdr: &NiftiHeader, i: f32, j: f32, k: f32) -> glam::Vec3 {
    VoxelWorld::from_header(hdr).apply(i, j, k)
}

/// Nearest-neighbor resample binary `src_field` (flat `k * n1 * n0 + j * n0 + i`) onto `template`
/// voxel grid. Each **destination** voxel samples source at template voxel **center** in mm, maps
/// to source ijk, subtracts `source_ijk_subtract` from continuous indices, then rounds to integer.
#[cfg(feature = "scene-viewer")]
pub fn resample_binary_mask_to_template(
    src_hdr: &NiftiHeader,
    src_dims: (usize, usize, usize),
    src_field: &[f32],
    template: &TemplateVolume,
    source_ijk_subtract: f32,
) -> VizResult<Vec<f32>> {
    let (sn0, sn1, sn2) = src_dims;
    let expected = sn0 * sn1 * sn2;
    if src_field.len() != expected {
        return Err(VizError::NiftiLoad(format!(
            "resample: src_field len {} != {}*{}*{}",
            src_field.len(),
            sn0,
            sn1,
            sn2
        )));
    }
    let (tn0, tn1, tn2) = (template.n0, template.n1, template.n2);
    let mut out = vec![0.0_f32; tn0 * tn1 * tn2];

    let trilinear = |fx: f32, fy: f32, fz: f32| -> f32 {
        let i0 = fx.floor() as isize;
        let j0 = fy.floor() as isize;
        let k0 = fz.floor() as isize;
        let tx = fx - fx.floor();
        let ty = fy - fy.floor();
        let tz = fz - fz.floor();

        let mut acc = 0.0_f32;
        for di in 0..2i32 {
            let wi = if di == 0 { 1.0 - tx } else { tx };
            let ii = i0 + di as isize;
            if ii < 0 || ii as usize >= sn0 {
                continue;
            }
            for dj in 0..2i32 {
                let wj = if dj == 0 { 1.0 - ty } else { ty };
                let jj = j0 + dj as isize;
                if jj < 0 || jj as usize >= sn1 {
                    continue;
                }
                for dk in 0..2i32 {
                    let wk = if dk == 0 { 1.0 - tz } else { tz };
                    let kk = k0 + dk as isize;
                    if kk < 0 || kk as usize >= sn2 {
                        continue;
                    }
                    let ix = (kk as usize) * sn1 * sn0 + (jj as usize) * sn0 + (ii as usize);
                    acc += wi * wj * wk * src_field[ix];
                }
            }
        }
        acc
    };

    let template_to_world = VoxelWorld::from_header(&template.hdr);
    let world_to_src = VoxelWorld::world_to_voxel(src_hdr);
    out.par_chunks_mut(tn0 * tn1)
        .enumerate()
        .for_each(|(k, chunk)| {
            let k = k as f32;
            for j in 0..tn1 {
                for i in 0..tn0 {
                    let w = template_to_world.apply(i as f32, j as f32, k);
                    let q = world_to_src.apply(w.x, w.y, w.z);
                    let fx = q.x - source_ijk_subtract;
                    let fy = q.y - source_ijk_subtract;
                    let fz = q.z - source_ijk_subtract;
                    chunk[j * tn0 + i] = trilinear(fx, fy, fz);
                }
            }
        });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_mm_to_ijk(hdr: &NiftiHeader, w: glam::Vec3) -> glam::Vec3 {
        VoxelWorld::world_to_voxel(hdr).apply(w.x, w.y, w.z)
    }

    #[test]
    fn world_ijk_roundtrip_sform() {
        let h = NiftiHeader {
            sform_code: 1,
            srow_x: [2.0_f32, 0.0, 0.0, 10.0],
            srow_y: [0.0_f32, 3.0, 0.0, 20.0],
            srow_z: [0.0_f32, 0.0, 4.0, 30.0],
            ..Default::default()
        };
        let i0 = 3.25_f32;
        let j0 = -1.5_f32;
        let k0 = 7.0_f32;
        let w = ijk_to_world_mm(&h, i0, j0, k0);
        let q = world_mm_to_ijk(&h, w);
        assert!((q.x - i0).abs() < 1e-4, "x {}", q.x);
        assert!((q.y - j0).abs() < 1e-4, "y {}", q.y);
        assert!((q.z - k0).abs() < 1e-4, "z {}", q.z);
    }

    #[test]
    fn qform_only_header_is_not_read_as_pixdim() {
        let h = NiftiHeader {
            sform_code: 0,
            qform_code: 1,
            quatern_d: 1.0,
            quatern_x: 5.0,
            quatern_y: 6.0,
            quatern_z: 7.0,
            pixdim: [1.0, 2.0, 2.0, 2.0, 0.0, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        let w = ijk_to_world_mm(&h, 1.0, 1.0, 1.0);
        assert!(
            (w - glam::Vec3::new(3.0, 4.0, 9.0)).length() < 1e-5,
            "{w:?}"
        );
        let q = world_mm_to_ijk(&h, w);
        assert!((q - glam::Vec3::ONE).length() < 1e-5, "{q:?}");
    }

    #[test]
    fn percentile_range_returns_min_and_the_selected_upper_value() {
        let v: Vec<f32> = (0..1000).rev().map(|i| i as f32).collect();
        let (lo, hi) = percentile_range(&v);
        assert_eq!(lo, 0.0);
        assert_eq!(hi, 995.0);
        assert_eq!(percentile_range(&[]), (0.0, 0.0));
    }

    /// Minimal single-file NIfTI-1 (float32) — same byte layout the checkreg tests use.
    #[cfg(feature = "web")]
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
        wf32!(300, 1.0);
        wf32!(320, 1.0);
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

    #[cfg(feature = "web")]
    #[test]
    fn from_bytes_roundtrips_raw_and_gzip_nifti() {
        let n = 6usize;
        let voxels: Vec<f32> = (0..(n * n * n)).map(|i| i as f32 * 0.5).collect();
        let raw = make_nifti1_bytes(n, n, n, &voxels);

        let v = TemplateVolume::from_bytes(&raw).expect("raw from_bytes");
        assert_eq!((v.n0, v.n1, v.n2), (n, n, n));
        assert_eq!(v.normed.len(), n * n * n);
        assert!(v.normed.iter().all(|x| (0.0..=1.0).contains(x)));

        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw).expect("gzip write");
        let gz = enc.finish().expect("gzip finish");
        assert!(super::is_gzip(&gz));
        let vg = TemplateVolume::from_bytes(&gz).expect("gzip from_bytes");
        assert_eq!(
            (vg.n0, vg.n1, vg.n2),
            (n, n, n),
            "gzip path matches raw dims"
        );
        assert_eq!(vg.normed, v.normed);
    }
}
