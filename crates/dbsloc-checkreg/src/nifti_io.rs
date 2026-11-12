// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use nifti::NiftiHeader;
#[cfg(target_arch = "wasm32")]
use nifti::ReaderOptions;
#[cfg(any(feature = "web", target_arch = "wasm32", test))]
use nifti::{Endianness, NiftiType};
#[cfg(any(feature = "web", target_arch = "wasm32"))]
use nifti::{NiftiObject, NiftiVolume};

use crate::error::{CheckregError, CheckregResult};

const MAX_AXIS: usize = 2048;

const MAX_VOXELS: u64 = 1 << 28;

pub fn check_header_dims(hdr: &NiftiHeader) -> CheckregResult<(usize, usize, usize)> {
    let dims = hdr
        .dim()
        .map_err(|e| CheckregError::NiftiLoad(e.to_string()))?;
    if dims.len() < 3 {
        return Err(CheckregError::NiftiLoad(format!(
            "expected a 3D volume, got {}D",
            dims.len()
        )));
    }
    let (n0, n1, n2) = (dims[0] as usize, dims[1] as usize, dims[2] as usize);
    if n0 > MAX_AXIS || n1 > MAX_AXIS || n2 > MAX_AXIS {
        return Err(CheckregError::NiftiLoad(format!(
            "NIfTI dims too large: {n0}×{n1}×{n2} (max {MAX_AXIS} per axis)"
        )));
    }
    let voxels = dims
        .iter()
        .fold(1_u64, |a, &d| a.saturating_mul(u64::from(d)));
    if voxels > MAX_VOXELS {
        return Err(CheckregError::NiftiLoad(format!(
            "NIfTI has {voxels} voxels (max {MAX_VOXELS})"
        )));
    }
    Ok((n0, n1, n2))
}

#[cfg(target_arch = "wasm32")]
pub fn read_checked(path: &Path) -> CheckregResult<nifti::InMemNiftiObject> {
    let hdr = NiftiHeader::from_file(path)
        .map_err(|e| CheckregError::NiftiLoad(format!("{}: {e}", path.display())))?;
    check_header_dims(&hdr)?;
    ReaderOptions::new()
        .read_file(path)
        .map_err(|e| CheckregError::NiftiLoad(format!("{}: {e}", path.display())))
}

pub struct VolumeF32 {
    pub header: NiftiHeader,
    pub dims: [usize; 3],
    pub data: Vec<f32>,
}

#[cfg(any(feature = "web", target_arch = "wasm32"))]
pub fn volume_f32_from_object(obj: nifti::InMemNiftiObject) -> CheckregResult<VolumeF32> {
    let header = obj.header().clone();
    let (n0, n1, n2) = check_header_dims(&header)?;
    let vol = obj.into_volume();
    let datatype = vol.data_type();
    let raw = vol.into_raw_data();
    let data = decode_voxels(
        &raw,
        datatype,
        header.endianness,
        header.scl_slope,
        header.scl_inter,
        n0 * n1 * n2,
    )?;
    Ok(VolumeF32 {
        header,
        dims: [n0, n1, n2],
        data,
    })
}

#[cfg(target_arch = "wasm32")]
pub fn read_volume_f32(path: &Path) -> CheckregResult<VolumeF32> {
    volume_f32_from_object(read_checked(path)?)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_volume_f32(path: &Path) -> CheckregResult<VolumeF32> {
    let load_err =
        |e: &dyn std::fmt::Display| CheckregError::NiftiLoad(format!("{}: {e}", path.display()));
    let file = fastwarp_niftiio::NiftiFile::open(path).map_err(|e| load_err(&e))?;
    let header = file.header().clone();
    let (n0, n1, n2) = check_header_dims(&header)?;
    let datatype = file.datatype().map_err(|e| load_err(&e))?;
    let count = n0 * n1 * n2;
    let size = fastwarp_niftiio::scalar_size(datatype)
        .ok_or_else(|| CheckregError::NiftiLoad(format!("unsupported voxel type {datatype:?}")))?;
    let bytes = file
        .read_data(Some(count * size))
        .map_err(|e| load_err(&e))?;
    let (slope, inter) = (header.scl_slope, header.scl_inter);
    let scale = (slope != 0.0 && slope.is_finite() && (slope != 1.0 || inter != 0.0))
        .then_some((slope, inter));
    let big = header.endianness == nifti::Endianness::Big;
    let data = fastwarp_niftiio::decode_f32(&bytes, datatype, big, count, scale)
        .map_err(|e| CheckregError::NiftiLoad(e.to_string()))?;
    Ok(VolumeF32 {
        header,
        dims: [n0, n1, n2],
        data,
    })
}

#[cfg(any(feature = "web", target_arch = "wasm32", test))]
pub fn decode_voxels(
    raw: &[u8],
    datatype: NiftiType,
    endianness: Endianness,
    slope: f32,
    inter: f32,
    count: usize,
) -> CheckregResult<Vec<f32>> {
    let size = match datatype {
        NiftiType::Uint8 | NiftiType::Int8 => 1,
        NiftiType::Uint16 | NiftiType::Int16 => 2,
        NiftiType::Uint32 | NiftiType::Int32 | NiftiType::Float32 => 4,
        NiftiType::Uint64 | NiftiType::Int64 | NiftiType::Float64 => 8,
        other => {
            return Err(CheckregError::NiftiLoad(format!(
                "unsupported voxel type {other:?}"
            )))
        }
    };
    if raw.len() < count * size {
        return Err(CheckregError::NiftiLoad(
            "voxel data is shorter than dims".into(),
        ));
    }
    let big = matches!(endianness, Endianness::Big);
    macro_rules! decode {
        ($t:ty) => {
            raw[..count * size]
                .chunks_exact(size)
                .map(|c| {
                    let mut b = [0u8; std::mem::size_of::<$t>()];
                    b.copy_from_slice(c);
                    (if big {
                        <$t>::from_be_bytes(b)
                    } else {
                        <$t>::from_le_bytes(b)
                    }) as f32
                })
                .collect::<Vec<f32>>()
        };
    }
    let mut out = match datatype {
        NiftiType::Uint8 => decode!(u8),
        NiftiType::Int8 => decode!(i8),
        NiftiType::Uint16 => decode!(u16),
        NiftiType::Int16 => decode!(i16),
        NiftiType::Uint32 => decode!(u32),
        NiftiType::Int32 => decode!(i32),
        NiftiType::Uint64 => decode!(u64),
        NiftiType::Int64 => decode!(i64),
        NiftiType::Float32 => decode!(f32),
        _ => decode!(f64),
    };
    if slope != 0.0 && slope.is_finite() && (slope != 1.0 || inter != 0.0) {
        for v in &mut out {
            *v = *v * slope + inter;
        }
    }
    Ok(out)
}

#[cfg(any(test, feature = "test-support"))]
pub fn make_nifti1_bytes(n0: usize, n1: usize, n2: usize, voxels: &[f32]) -> Vec<u8> {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use nifti::{NiftiObject, RandomAccessNiftiVolume, ReaderOptions};

    use super::*;

    #[test]
    fn header_guard_rejects_oversized_axes_and_voxel_counts() {
        let mut h = NiftiHeader {
            dim: [3, 3000, 2, 2, 1, 1, 1, 1],
            ..Default::default()
        };
        assert!(check_header_dims(&h).is_err());
        h.dim = [4, 1024, 1024, 1024, 4, 1, 1, 1];
        assert!(check_header_dims(&h).is_err());
        h.dim = [3, 64, 64, 64, 1, 1, 1, 1];
        assert_eq!(check_header_dims(&h).ok(), Some((64, 64, 64)));
        h.dim = [2, 64, 64, 1, 1, 1, 1, 1];
        assert!(check_header_dims(&h).is_err());
    }

    #[test]
    fn decode_applies_type_endianness_and_scaling() {
        let raw: Vec<u8> = [1i16, -2, 300]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let out = decode_voxels(&raw, NiftiType::Int16, Endianness::Little, 2.0, 10.0, 3).unwrap();
        assert_eq!(out, vec![12.0, 6.0, 610.0]);
        let raw: Vec<u8> = [1.5f32, -2.5]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let out = decode_voxels(&raw, NiftiType::Float32, Endianness::Big, 0.0, 0.0, 2).unwrap();
        assert_eq!(out, vec![1.5, -2.5]);
        assert!(decode_voxels(&raw, NiftiType::Float32, Endianness::Big, 1.0, 0.0, 5).is_err());
    }

    #[test]
    fn bulk_decode_matches_per_voxel_reads() {
        let (n0, n1, n2) = (5usize, 4usize, 3usize);
        let voxels: Vec<f32> = (0..n0 * n1 * n2).map(|i| i as f32 * 0.25 - 3.0).collect();
        let dir = std::env::temp_dir().join(format!("dbsloc_nifti_io_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v.nii");
        std::fs::write(&path, make_nifti1_bytes(n0, n1, n2, &voxels)).unwrap();
        let bulk = read_volume_f32(&path).unwrap();
        assert_eq!(bulk.dims, [n0, n1, n2]);
        let obj = ReaderOptions::new().read_file(&path).unwrap();
        let vol = obj.into_volume();
        for k in 0..n2 {
            for j in 0..n1 {
                for i in 0..n0 {
                    let v = vol.get_f32(&[i as u16, j as u16, k as u16]).unwrap();
                    assert_eq!(bulk.data[k * n1 * n0 + j * n0 + i], v);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_header_is_rejected_before_the_volume_is_read() {
        let dir = std::env::temp_dir().join(format!("dbsloc_nifti_big_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.nii");
        let mut bytes = make_nifti1_bytes(2, 2, 2, &[0.0; 8]);
        bytes[42..44].copy_from_slice(&3000_i16.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let err = read_volume_f32(&path).err().expect("rejected");
        assert!(err.to_string().contains("too large"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
