// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! NIfTI-1 voxel → world affine (sform when set, else qform, else the scaled-diagonal fallback).

use crate::Mat4;

/// The NIfTI-1 header fields that determine the voxel → world affine.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NiftiXformFields {
    pub sform_code: i16,
    pub qform_code: i16,
    pub srow_x: [f32; 4],
    pub srow_y: [f32; 4],
    pub srow_z: [f32; 4],
    pub quatern_b: f32,
    pub quatern_c: f32,
    pub quatern_d: f32,
    pub qoffset_x: f32,
    pub qoffset_y: f32,
    pub qoffset_z: f32,
    pub pixdim: [f32; 8],
}

/// Which header method produced an affine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XformSource {
    Sform,
    Qform,
    Pixdim,
}

/// Best voxel → world affine: sform when `sform_code > 0`, else qform (method 2) when
/// `qform_code > 0`, else `diag(pixdim[1..3])` with zero origin (method 1).
pub fn voxel_to_world(f: &NiftiXformFields) -> Mat4 {
    voxel_to_world_with_source(f).0
}

/// [`voxel_to_world`] plus the method that produced it.
pub fn voxel_to_world_with_source(f: &NiftiXformFields) -> (Mat4, XformSource) {
    if f.sform_code > 0 {
        (sform_affine(f), XformSource::Sform)
    } else if f.qform_code > 0 {
        (qform_affine(f), XformSource::Qform)
    } else {
        (pixdim_affine(f), XformSource::Pixdim)
    }
}

fn sform_affine(f: &NiftiXformFields) -> Mat4 {
    let row = |r: [f32; 4]| [r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64];
    [
        row(f.srow_x),
        row(f.srow_y),
        row(f.srow_z),
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn qform_affine(f: &NiftiXformFields) -> Mat4 {
    let b = f.quatern_b as f64;
    let c = f.quatern_c as f64;
    let d = f.quatern_d as f64;
    let a = (1.0 - b * b - c * c - d * d).max(0.0).sqrt();
    let r = [
        [
            a * a + b * b - c * c - d * d,
            2.0 * (b * c - a * d),
            2.0 * (b * d + a * c),
        ],
        [
            2.0 * (b * c + a * d),
            a * a + c * c - b * b - d * d,
            2.0 * (c * d - a * b),
        ],
        [
            2.0 * (b * d - a * c),
            2.0 * (c * d + a * b),
            a * a + d * d - b * b - c * c,
        ],
    ];
    let qfac = if f.pixdim[0] == -1.0 { -1.0 } else { 1.0 };
    let scale = [
        f.pixdim[1] as f64,
        f.pixdim[2] as f64,
        f.pixdim[3] as f64 * qfac,
    ];
    let offset = [f.qoffset_x as f64, f.qoffset_y as f64, f.qoffset_z as f64];
    let mut out = [[0.0f64; 4]; 4];
    for row in 0..3 {
        for col in 0..3 {
            out[row][col] = r[row][col] * scale[col];
        }
        out[row][3] = offset[row];
    }
    out[3][3] = 1.0;
    out
}

fn pixdim_affine(f: &NiftiXformFields) -> Mat4 {
    [
        [f.pixdim[1] as f64, 0.0, 0.0, 0.0],
        [0.0, f.pixdim[2] as f64, 0.0, 0.0],
        [0.0, 0.0, f.pixdim[3] as f64, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

#[cfg(feature = "nifti")]
impl From<&nifti::NiftiHeader> for NiftiXformFields {
    fn from(h: &nifti::NiftiHeader) -> Self {
        Self {
            sform_code: h.sform_code,
            qform_code: h.qform_code,
            srow_x: h.srow_x,
            srow_y: h.srow_y,
            srow_z: h.srow_z,
            quatern_b: h.quatern_b,
            quatern_c: h.quatern_c,
            quatern_d: h.quatern_d,
            qoffset_x: h.quatern_x,
            qoffset_y: h.quatern_y,
            qoffset_z: h.quatern_z,
            pixdim: h.pixdim,
        }
    }
}

/// [`voxel_to_world`] for a parsed `nifti` header.
#[cfg(feature = "nifti")]
pub fn header_affine(h: &nifti::NiftiHeader) -> Mat4 {
    voxel_to_world(&NiftiXformFields::from(h))
}

/// [`voxel_to_world_with_source`] for a parsed `nifti` header.
#[cfg(feature = "nifti")]
pub fn header_affine_with_source(h: &nifti::NiftiHeader) -> (Mat4, XformSource) {
    voxel_to_world_with_source(&NiftiXformFields::from(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_mat_close(got: &Mat4, want: &Mat4, tol: f64) {
        for r in 0..4 {
            for c in 0..4 {
                assert!(
                    (got[r][c] - want[r][c]).abs() < tol,
                    "[{r}][{c}] {} != {}\n got {got:?}\nwant {want:?}",
                    got[r][c],
                    want[r][c]
                );
            }
        }
    }

    fn base() -> NiftiXformFields {
        NiftiXformFields {
            pixdim: [1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0],
            ..Default::default()
        }
    }

    #[test]
    fn sform_only() {
        let f = NiftiXformFields {
            sform_code: 4,
            srow_x: [-1.0, 0.0, 0.0, 90.0],
            srow_y: [0.0, 1.0, 0.0, -126.0],
            srow_z: [0.0, 0.0, 1.0, -72.0],
            ..base()
        };
        let (m, src) = voxel_to_world_with_source(&f);
        assert_eq!(src, XformSource::Sform);
        assert_mat_close(
            &m,
            &[
                [-1.0, 0.0, 0.0, 90.0],
                [0.0, 1.0, 0.0, -126.0],
                [0.0, 0.0, 1.0, -72.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            1e-12,
        );
    }

    #[test]
    fn qform_only_with_rotation_offsets_and_negative_qfac() {
        let f = NiftiXformFields {
            qform_code: 1,
            quatern_b: 0.5,
            quatern_c: 0.5,
            quatern_d: 0.5,
            qoffset_x: 10.0,
            qoffset_y: -20.0,
            qoffset_z: 30.0,
            pixdim: [-1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0],
            srow_x: [9.0, 9.0, 9.0, 9.0],
            ..Default::default()
        };
        let (m, src) = voxel_to_world_with_source(&f);
        assert_eq!(src, XformSource::Qform);
        assert_mat_close(
            &m,
            &[
                [0.0, 0.0, -4.0, 10.0],
                [2.0, 0.0, 0.0, -20.0],
                [0.0, 3.0, 0.0, 30.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            1e-9,
        );
    }

    #[test]
    fn qfac_other_than_minus_one_is_one() {
        let f = NiftiXformFields {
            qform_code: 2,
            pixdim: [0.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        assert_mat_close(
            &voxel_to_world(&f),
            &[
                [2.0, 0.0, 0.0, 0.0],
                [0.0, 3.0, 0.0, 0.0],
                [0.0, 0.0, 4.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            1e-12,
        );
    }

    #[test]
    fn both_codes_zero_uses_pixdim_with_zero_origin() {
        let f = NiftiXformFields {
            srow_x: [5.0, 5.0, 5.0, 5.0],
            quatern_b: 0.5,
            qoffset_x: 7.0,
            ..base()
        };
        let (m, src) = voxel_to_world_with_source(&f);
        assert_eq!(src, XformSource::Pixdim);
        assert_mat_close(
            &m,
            &[
                [2.0, 0.0, 0.0, 0.0],
                [0.0, 3.0, 0.0, 0.0],
                [0.0, 0.0, 4.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            1e-12,
        );
    }

    #[test]
    fn sform_wins_over_qform() {
        let f = NiftiXformFields {
            sform_code: 1,
            qform_code: 1,
            srow_x: [1.5, 0.0, 0.0, -1.0],
            srow_y: [0.0, 1.5, 0.0, -2.0],
            srow_z: [0.0, 0.0, 1.5, -3.0],
            quatern_b: 0.5,
            quatern_c: 0.5,
            quatern_d: 0.5,
            qoffset_x: 10.0,
            ..base()
        };
        let (m, src) = voxel_to_world_with_source(&f);
        assert_eq!(src, XformSource::Sform);
        assert_mat_close(
            &m,
            &[
                [1.5, 0.0, 0.0, -1.0],
                [0.0, 1.5, 0.0, -2.0],
                [0.0, 0.0, 1.5, -3.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            1e-12,
        );
    }

    #[test]
    fn negative_codes_are_not_valid() {
        let f = NiftiXformFields {
            sform_code: -1,
            qform_code: -1,
            srow_x: [5.0, 5.0, 5.0, 5.0],
            ..base()
        };
        assert_eq!(voxel_to_world_with_source(&f).1, XformSource::Pixdim);
    }

    #[cfg(feature = "nifti")]
    #[test]
    fn header_adapter_maps_qoffsets() {
        let h = nifti::NiftiHeader {
            sform_code: 0,
            qform_code: 1,
            quatern_b: 0.0,
            quatern_c: 0.0,
            quatern_d: 1.0,
            quatern_x: 1.0,
            quatern_y: 2.0,
            quatern_z: 3.0,
            pixdim: [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        assert_mat_close(
            &header_affine(&h),
            &[
                [-1.0, 0.0, 0.0, 1.0],
                [0.0, -1.0, 0.0, 2.0],
                [0.0, 0.0, 1.0, 3.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            1e-12,
        );
    }
}
