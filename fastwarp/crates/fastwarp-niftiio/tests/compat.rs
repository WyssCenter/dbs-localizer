#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::io::Read;
use std::path::{Path, PathBuf};

use bytemuck::Pod;
use fastwarp_niftiio::{decode_f32, decode_map, decode_map_c_order, NiftiFile, WriterOptions};
use ndarray::{Array, ArrayD, Dimension, IxDyn, RemoveAxis, ShapeBuilder};
use nifti::volume::inmem::InMemNiftiVolume;
use nifti::{DataElement, Endianness, IntoNdArray, NiftiHeader, NiftiObject, ReaderOptions};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fastwarp_niftiio_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn read_all_bytes(path: &Path) -> Vec<u8> {
    let raw = std::fs::read(path).unwrap();
    if path.to_string_lossy().ends_with(".gz") {
        let mut d = flate2::read::GzDecoder::new(raw.as_slice());
        let mut out = Vec::new();
        d.read_to_end(&mut out).unwrap();
        assert!(d.into_inner().is_empty(), "more than one gzip member");
        out
    } else {
        raw
    }
}

fn reference_header(endianness: Endianness) -> NiftiHeader {
    NiftiHeader {
        sform_code: 1,
        qform_code: 1,
        srow_x: [-0.9375, 0.0, 0.0, 117.6],
        srow_y: [0.0, 0.9375, 0.0, -103.6],
        srow_z: [0.0, 0.0, 1.5, -112.0],
        pixdim: [-1.0, 0.9375, 0.9375, 1.5, 1.0, 1.0, 1.0, 1.0],
        quatern_b: 0.1,
        quatern_c: -0.2,
        quatern_d: 0.3,
        xyzt_units: 2,
        descrip: {
            let mut d = vec![0u8; 80];
            d[..4].copy_from_slice(b"test");
            d
        },
        endianness,
        ..NiftiHeader::default()
    }
}

fn pattern<T: DataElement + Pod + Default>(shape: &[usize], f: impl Fn(usize) -> T) -> ArrayD<T> {
    let n: usize = shape.iter().product();
    ArrayD::from_shape_vec(IxDyn(shape), (0..n).map(f).collect()).unwrap()
}

fn assert_same_bytes<A, D>(data: &Array<A, D>, endianness: Endianness, tag: &str)
where
    A: DataElement + Pod + Default + Send + Sync,
    D: Dimension + RemoveAxis,
{
    let dir = scratch(tag);
    let hdr = reference_header(endianness);
    for name in ["a.nii", "a.nii.gz"] {
        let old = dir.join(format!("old_{name}"));
        let new = dir.join(format!("new_{name}"));
        nifti::writer::WriterOptions::new(&old)
            .reference_header(&hdr)
            .write_nifti(data)
            .unwrap();
        WriterOptions::new(&new)
            .reference_header(&hdr)
            .write_nifti(data)
            .unwrap();
        let (a, b) = (read_all_bytes(&old), read_all_bytes(&new));
        assert_eq!(a.len(), b.len(), "{tag} {name}: length");
        assert!(a == b, "{tag} {name}: bytes differ");
        let back = NiftiHeader::from_file(&new).unwrap();
        assert_eq!(back.dim[0] as usize, data.ndim());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writer_output_equals_the_nifti_crate_output() {
    let c3 = pattern::<i16>(&[7, 5, 9], |i| (i as i16).wrapping_mul(37));
    let c3 = c3.into_dimensionality::<ndarray::Ix3>().unwrap();
    for e in [Endianness::Little, Endianness::Big] {
        assert_same_bytes(&c3, e, "c3");
    }
    let f3 = Array::from_shape_fn(ndarray::Ix3(6, 5, 4).f(), |(i, j, k)| {
        (i * 100 + j * 10 + k) as f32 * 0.5
    });
    assert_same_bytes(&f3, Endianness::Little, "f3");
    let t3 = f3.t().to_owned();
    assert_same_bytes(&t3, Endianness::Little, "t3");
    let d4 = pattern::<f64>(&[4, 5, 3, 2], |i| i as f64 * 0.37 - 5.0);
    assert_same_bytes(&d4, Endianness::Little, "d4");
    assert_same_bytes(&d4, Endianness::Big, "d4be");
    let d5 = pattern::<f32>(&[3, 4, 5, 1, 3], |i| i as f32);
    assert_same_bytes(&d5, Endianness::Little, "d5");
    let trailing = pattern::<u8>(&[6, 5, 4, 1], |i| (i % 251) as u8);
    assert_same_bytes(&trailing, Endianness::Little, "trailing");
    let one = pattern::<u16>(&[1, 1, 1], |_| 9);
    assert_same_bytes(&one, Endianness::Little, "one");
    let flat = pattern::<i32>(&[1, 6, 1], |i| i as i32 - 3);
    assert_same_bytes(&flat, Endianness::Little, "flat");
    let d1 = pattern::<u32>(&[11], |i| i as u32 * 70000);
    assert_same_bytes(&d1, Endianness::Little, "d1");
    let big = pattern::<f32>(&[130, 129, 131], |i| (i % 1000) as f32);
    assert_same_bytes(&big, Endianness::Little, "big");
}

fn old_f64(path: &Path) -> ArrayD<f64> {
    let obj = ReaderOptions::new().read_file(path).unwrap();
    let hdr = obj.header().clone();
    let raw = obj.into_volume().into_raw_data();
    let dt = hdr.data_type().unwrap();
    let vol =
        InMemNiftiVolume::from_raw_fields(hdr.dim, dt, 0.0, 0.0, raw, hdr.endianness).unwrap();
    vol.into_ndarray::<f64>().unwrap()
}

fn check_read<A>(shape: &[usize], endianness: Endianness, gz: bool, f: impl Fn(usize) -> A)
where
    A: DataElement + Pod + Default + Send + Sync,
{
    let dir = scratch("read");
    let name = if gz { "r.nii.gz" } else { "r.nii" };
    let path = dir.join(name);
    let data = pattern::<A>(shape, f);
    let hdr = reference_header(endianness);
    WriterOptions::new(&path)
        .reference_header(&hdr)
        .write_nifti(&data)
        .unwrap();
    let want = old_f64(&path);
    let want_c: Vec<f64> = want.as_standard_layout().iter().copied().collect();
    let want_f: Vec<f64> = want.t().iter().copied().collect();

    let file = NiftiFile::open(&path).unwrap();
    let dt = file.datatype().unwrap();
    let big = file.header().endianness == Endianness::Big;
    assert_eq!(file.header().dim[0] as usize, shape.len());
    let count = file.voxel_count().unwrap();
    let bytes = file.read_data(None).unwrap();
    assert_eq!(bytes.len(), count * dt.size_of());

    let flat = decode_map(&bytes, dt, big, count, |v| v).unwrap();
    assert_eq!(
        bits(&flat),
        bits(&want_f),
        "F order {shape:?} {endianness:?}"
    );
    let c = decode_map_c_order(&bytes, dt, big, shape, |v| v).unwrap();
    assert_eq!(bits(&c), bits(&want_c), "C order {shape:?} {endianness:?}");
    let f32s = decode_f32(&bytes, dt, big, count, None).unwrap();
    let want32: Vec<u32> = want_f.iter().map(|v| (*v as f32).to_bits()).collect();
    let got32: Vec<u32> = f32s.iter().map(|v| v.to_bits()).collect();
    if !matches!(dt, nifti::NiftiType::Uint64 | nifti::NiftiType::Int64) {
        assert_eq!(got32, want32);
    }

    let scaled = decode_f32(&bytes, dt, big, count, Some((0.5, -3.0))).unwrap();
    for (a, b) in scaled.iter().zip(&f32s) {
        assert_eq!(a.to_bits(), (*b * 0.5 - 3.0).to_bits());
    }

    let part = NiftiFile::open(&path)
        .unwrap()
        .read_data(Some(bytes.len() / 2))
        .unwrap();
    assert_eq!(part, bytes[..bytes.len() / 2]);
    let _ = std::fs::remove_dir_all(&dir);
}

fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn check_all_shapes<A>(f: impl Fn(usize) -> A + Copy)
where
    A: DataElement + Pod + Default + Send + Sync,
{
    for e in [Endianness::Little, Endianness::Big] {
        for gz in [false, true] {
            for shape in [
                &[5usize, 6, 7][..],
                &[17, 3, 33],
                &[4, 5, 6, 3],
                &[3, 4, 2, 2, 2],
                &[8, 9],
                &[40],
            ] {
                check_read::<A>(shape, e, gz, f);
            }
        }
    }
}

#[test]
fn reader_matches_the_nifti_crate_for_every_datatype_and_endianness() {
    check_all_shapes::<u8>(|i| (i * 7) as u8);
    check_all_shapes::<i8>(|i| (i as i64 * 5 - 300) as i8);
    check_all_shapes::<u16>(|i| (i * 131) as u16);
    check_all_shapes::<i16>(|i| (i as i64 * 97 - 20000) as i16);
    check_all_shapes::<u32>(|i| (i as u32).wrapping_mul(2_654_435_761));
    check_all_shapes::<i32>(|i| (i as i32).wrapping_mul(-1_640_531_527));
    check_all_shapes::<u64>(|i| (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
    check_all_shapes::<i64>(|i| (i as i64).wrapping_mul(-0x1234_5678_9abc_def1));
    check_all_shapes::<f32>(|i| i as f32 * 0.37 - 41.5);
    check_all_shapes::<f64>(|i| i as f64 * 1.0e7 / 3.0 - 1e-9 * i as f64);
}

#[test]
fn a_large_volume_round_trips_through_the_parallel_gzip() {
    let dir = scratch("big");
    let path = dir.join("b.nii.gz");
    let data = pattern::<i16>(&[300, 200, 100], |i| ((i * 31) % 4000) as i16 - 1000);
    WriterOptions::new(&path).write_nifti(&data).unwrap();
    let file = NiftiFile::open(&path).unwrap();
    let count = file.voxel_count().unwrap();
    let dt = file.datatype().unwrap();
    let bytes = file.read_data(None).unwrap();
    let c = decode_map_c_order(&bytes, dt, false, &[300, 200, 100], |v| v as i16).unwrap();
    let want: Vec<i16> = data.iter().copied().collect();
    assert_eq!(count, c.len());
    assert_eq!(c, want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn truncated_data_and_headers_fail_cleanly() {
    let dir = scratch("trunc");
    let path = dir.join("t.nii");
    let data = pattern::<f32>(&[4, 4, 4], |i| i as f32);
    WriterOptions::new(&path).write_nifti(&data).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let cut = dir.join("cut.nii");
    std::fs::write(&cut, &bytes[..bytes.len() - 8]).unwrap();
    assert!(NiftiFile::open(&cut).unwrap().read_data(None).is_err());
    let hdr_only = dir.join("h.nii");
    std::fs::write(&hdr_only, &bytes[..100]).unwrap();
    assert!(NiftiFile::open(&hdr_only).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_wrong_bitpix_is_ignored_and_the_datatype_size_decides() {
    let dir = scratch("bitpix");
    let path = dir.join("b.nii");
    let data = pattern::<f32>(&[4, 5, 6], |i| i as f32);
    WriterOptions::new(&path).write_nifti(&data).unwrap();
    let want = NiftiFile::open(&path).unwrap().read_data(None).unwrap();
    for bad in [0i16, 8, 64] {
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[72..74].copy_from_slice(&bad.to_le_bytes());
        let patched = dir.join(format!("bad{bad}.nii"));
        std::fs::write(&patched, &bytes).unwrap();
        let file = NiftiFile::open(&patched).unwrap();
        assert_eq!(file.header().bitpix, bad);
        assert_eq!(file.read_data(None).unwrap(), want);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
