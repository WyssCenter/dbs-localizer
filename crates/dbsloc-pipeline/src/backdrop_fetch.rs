// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use flate2::read::GzDecoder;
use serde::Serialize;

use crate::backdrop_catalog::{
    catalogue, entry_dir_in, installed_path_in, BackdropEntry, BACKDROP_FILE, BACKDROP_FILE_PLAIN,
};
use crate::sha256::file_sha256_hex;
use crate::template_catalog::user_data_dir;
use crate::template_fetch::{
    io_err, is_cancelled, replace_dir, stream_download, StreamEvent, CANCELLED,
};

const HEADER_LEN: usize = 348;
const OUT_VOX_OFFSET: usize = 352;
const SLACK_BYTES: u64 = 512 * 1_000_000;
const EPS: f64 = 1e-6;
const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 3600);
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BackdropEvent {
    Started { id: String, total_bytes: u64 },
    Progress { downloaded: u64, total: u64 },
    Verifying,
    Cropping,
    Done { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackdropState {
    Installed(PathBuf),
    Available { size: u64 },
}

impl BackdropState {
    pub fn label(&self) -> &'static str {
        match self {
            BackdropState::Installed(_) => "installed",
            BackdropState::Available { .. } => "available",
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            BackdropState::Installed(p) => Some(p),
            BackdropState::Available { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackdropStatus {
    pub entry: &'static BackdropEntry,
    pub state: BackdropState,
}

pub fn list_backdrops_in(data: Option<&Path>, space: Option<&str>) -> Vec<BackdropStatus> {
    catalogue()
        .iter()
        .filter(|e| space.is_none_or(|s| e.space == s))
        .map(|e| {
            let state = match data.and_then(|d| installed_path_in(d, e)) {
                Some(p) => BackdropState::Installed(p),
                None => BackdropState::Available { size: e.size },
            };
            BackdropStatus { entry: e, state }
        })
        .collect()
}

pub fn list_backdrops(space: Option<&str>) -> Vec<BackdropStatus> {
    list_backdrops_in(user_data_dir().as_deref(), space)
}

fn staging_dir(e: &BackdropEntry, data_root: &Path) -> PathBuf {
    let dest = entry_dir_in(data_root, e.space, e.id);
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    let n = STAGING_COUNTER.fetch_add(1, Ordering::SeqCst);
    parent.join(format!(
        "{}{}-{}-{n}",
        crate::outputs::PARTIAL_PREFIX,
        e.id,
        std::process::id()
    ))
}

fn sweep_stale_staging(e: &BackdropEntry, data_root: &Path, keep: &Path, max_age: Duration) {
    let parent = entry_dir_in(data_root, e.space, e.id)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let prefix = format!("{}{}-", crate::outputs::PARTIAL_PREFIX, e.id);
    let Ok(rd) = std::fs::read_dir(&parent) else {
        return;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path == keep || !path.is_dir() {
            continue;
        }
        if !ent.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let old = ent
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if old {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

pub fn expected_output_bytes(e: &BackdropEntry) -> u64 {
    match e.crop_mm {
        Some(c) => {
            let n: f64 = c
                .iter()
                .map(|r| ((r[1] - r[0]) / e.res_mm).floor() + 1.0)
                .product();
            (n * 4.0) as u64
        }
        None => e.size,
    }
}

pub fn required_free_bytes(e: &BackdropEntry) -> u64 {
    e.size + expected_output_bytes(e) + SLACK_BYTES
}

#[cfg(windows)]
pub fn free_space_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = existing_ancestor(path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path and the three out-pointers refer to live locals.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
    (ok != 0).then_some(available)
}

#[cfg(not(windows))]
pub fn free_space_bytes(path: &Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(existing_ancestor(path))
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let kib: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    kib.checked_mul(1024)
}

fn existing_ancestor(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            return PathBuf::from(".");
        }
    }
    p
}

pub fn check_free_space(e: &BackdropEntry, data_root: &Path) -> Result<(), String> {
    check_free_space_with(e, free_space_bytes(data_root))
}

fn check_free_space_with(e: &BackdropEntry, free: Option<u64>) -> Result<(), String> {
    let need = required_free_bytes(e);
    match free {
        Some(free) if free < need => Err(format!(
            "not enough free disk space for {}: need {} MB, {} MB free",
            e.id,
            need.div_ceil(1_000_000),
            free / 1_000_000
        )),
        _ => Ok(()),
    }
}

pub fn fetch_entry(
    e: &BackdropEntry,
    data_root: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(BackdropEvent),
) -> Result<PathBuf, String> {
    let dest = entry_dir_in(data_root, e.space, e.id);
    let staging = staging_dir(e, data_root);
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent).map_err(|x| io_err("cannot create backdrop folder", x))?;
    }
    sweep_stale_staging(e, data_root, &staging, STALE_STAGING_AGE);
    check_free_space(e, data_root)?;
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|x| io_err("cannot create staging directory", x))?;
    let result = download_and_install(e, &staging, &dest, cancel, on_event);
    let _ = std::fs::remove_dir_all(&staging);
    if let Ok(p) = &result {
        on_event(BackdropEvent::Done { path: p.clone() });
    }
    result
}

fn download_and_install(
    e: &BackdropEntry,
    staging: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(BackdropEvent),
) -> Result<PathBuf, String> {
    let download = staging.join("download.nii.gz");
    let digest = stream_download(e.url, e.size, &download, cancel, &mut |ev| match ev {
        StreamEvent::Started(t) => on_event(BackdropEvent::Started {
            id: e.id.to_owned(),
            total_bytes: t,
        }),
        StreamEvent::Progress(downloaded, total) => {
            on_event(BackdropEvent::Progress { downloaded, total })
        }
    })?;
    install_from_download(e, &download, staging, dest, cancel, on_event, Some(digest))
}

pub fn install_from_download(
    e: &BackdropEntry,
    downloaded: &Path,
    staging: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(BackdropEvent),
    known_digest: Option<String>,
) -> Result<PathBuf, String> {
    let result = install_inner(e, downloaded, staging, dest, cancel, on_event, known_digest);
    let _ = std::fs::remove_dir_all(staging);
    result
}

fn install_inner(
    e: &BackdropEntry,
    downloaded: &Path,
    staging: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(BackdropEvent),
    known_digest: Option<String>,
) -> Result<PathBuf, String> {
    if e.sha256.is_empty() {
        return Err(format!("{} has no pinned checksum", e.id));
    }
    on_event(BackdropEvent::Verifying);
    let actual = std::fs::metadata(downloaded)
        .map_err(|x| io_err("cannot read downloaded file", x))?
        .len();
    if actual != e.size {
        return Err(format!(
            "size mismatch for {}: expected {} bytes, got {actual}",
            e.id, e.size
        ));
    }
    let digest = match known_digest {
        Some(d) => d,
        None => {
            file_sha256_hex(downloaded).map_err(|x| io_err("cannot hash downloaded file", x))?
        }
    };
    if digest != e.sha256 {
        return Err(format!(
            "SHA-256 mismatch for {}: expected {}, got {digest}",
            e.id, e.sha256
        ));
    }
    if is_cancelled(cancel) {
        return Err(CANCELLED.to_owned());
    }
    let out_dir = staging.join("out");
    std::fs::create_dir_all(&out_dir).map_err(|x| io_err("cannot create output directory", x))?;
    let file_name = if e.crop_mm.is_some() {
        BACKDROP_FILE_PLAIN
    } else {
        BACKDROP_FILE
    };
    let image = out_dir.join(file_name);
    match e.crop_mm {
        None => {
            std::fs::copy(downloaded, &image).map_err(|x| io_err("cannot copy the download", x))?;
        }
        Some(crop) => {
            on_event(BackdropEvent::Cropping);
            crop_nifti(downloaded, &image, e.expected_dims, crop, cancel)?;
        }
    }
    std::fs::write(out_dir.join("LICENSE"), e.licence_text)
        .map_err(|x| io_err("cannot write LICENSE", x))?;
    let meta = serde_json::json!({
        "id": e.id,
        "space": e.space,
        "title": e.title,
        "res_mm": e.res_mm,
        "source_url": e.url,
        "sha256": e.sha256,
        "crop_mm": e.crop_mm,
        "fetched_by": "dbsloc",
    });
    let text = serde_json::to_string_pretty(&meta).map_err(|x| io_err("backdrop.json", x))?;
    std::fs::write(out_dir.join("backdrop.json"), text)
        .map_err(|x| io_err("cannot write backdrop.json", x))?;
    let _ = std::fs::remove_file(downloaded);
    if is_cancelled(cancel) {
        return Err(CANCELLED.to_owned());
    }
    replace_dir(&out_dir, dest)?;
    Ok(dest.join(file_name))
}

fn rd_i16(h: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([h[at], h[at + 1]])
}

fn rd_f32(h: &[u8], at: usize) -> f64 {
    f64::from(f32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]))
}

fn wr_f32(h: &mut [u8], at: usize, v: f64) {
    h[at..at + 4].copy_from_slice(&(v as f32).to_le_bytes());
}

fn wr_i16(h: &mut [u8], at: usize, v: i16) {
    h[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn voxel_range(
    lo: f64,
    hi: f64,
    origin: f64,
    pix: f64,
    n: usize,
) -> Result<(usize, usize), String> {
    let first = ((lo - origin) / pix - EPS).ceil().max(0.0);
    let last = ((hi - origin) / pix + EPS).floor().min(n as f64 - 1.0);
    if first > last {
        return Err("the crop box does not overlap the image".to_owned());
    }
    Ok((first as usize, last as usize))
}

fn quaternion_rotation(b: f64, c: f64, d: f64) -> [[f64; 3]; 3] {
    let a = (1.0 - (b * b + c * c + d * d)).max(0.0).sqrt();
    [
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
            a * a + d * d - c * c - b * b,
        ],
    ]
}

pub fn crop_nifti(
    src: &Path,
    dst: &Path,
    expected_dims: [usize; 3],
    crop_mm: [[f64; 2]; 3],
    cancel: &AtomicBool,
) -> Result<(), String> {
    let file = File::open(src).map_err(|x| io_err("cannot open the download", x))?;
    let mut rd = GzDecoder::new(BufReader::with_capacity(1 << 20, file));
    let mut h = [0u8; HEADER_LEN];
    rd.read_exact(&mut h)
        .map_err(|x| io_err("cannot read the NIfTI header", x))?;
    if i32::from_le_bytes([h[0], h[1], h[2], h[3]]) != 348 {
        return Err("unsupported NIfTI header (little-endian NIfTI-1 required)".to_owned());
    }
    let dim: Vec<usize> = (1..=3)
        .map(|i| rd_i16(&h, 40 + 2 * i).max(0) as usize)
        .collect();
    if rd_i16(&h, 40) != 3 || dim != expected_dims {
        return Err(format!(
            "unexpected image dimensions {dim:?}, expected {expected_dims:?}"
        ));
    }
    if rd_i16(&h, 70) != 16 || rd_i16(&h, 72) != 32 {
        return Err("unexpected data type: float32 required".to_owned());
    }
    if rd_i16(&h, 254) <= 0 {
        return Err("the image has no sform".to_owned());
    }
    let row = |at: usize| {
        [
            rd_f32(&h, at),
            rd_f32(&h, at + 4),
            rd_f32(&h, at + 8),
            rd_f32(&h, at + 12),
        ]
    };
    let (sx, sy, sz) = (row(280), row(296), row(312));
    let off_ok = sx[1].abs() < EPS
        && sx[2].abs() < EPS
        && sy[0].abs() < EPS
        && sy[2].abs() < EPS
        && sz[0].abs() < EPS
        && sz[1].abs() < EPS;
    if !off_ok || sx[0] <= 0.0 || sy[1] <= 0.0 || sz[2] <= 0.0 {
        return Err("the sform is not an axis-aligned positive diagonal".to_owned());
    }
    let pix = [sx[0], sy[1], sz[2]];
    let origin = [sx[3], sy[3], sz[3]];
    let mut lo = [0usize; 3];
    let mut hi = [0usize; 3];
    for a in 0..3 {
        (lo[a], hi[a]) = voxel_range(crop_mm[a][0], crop_mm[a][1], origin[a], pix[a], dim[a])
            .map_err(|m| format!("axis {a}: {m}"))?;
    }
    let vox_offset = rd_f32(&h, 108);
    if vox_offset < 352.0 || vox_offset.fract() != 0.0 || vox_offset > 1e6 {
        return Err("unsupported vox_offset".to_owned());
    }
    let mut skip = vox_offset as usize - HEADER_LEN;
    let mut buf = vec![0u8; dim[0] * dim[1] * 4];
    while skip > 0 {
        let n = skip.min(buf.len());
        rd.read_exact(&mut buf[..n])
            .map_err(|x| io_err("cannot read the NIfTI extension", x))?;
        skip -= n;
    }
    let out_dim = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1];
    let mut o = h;
    for (a, n) in out_dim.iter().enumerate() {
        wr_i16(&mut o, 42 + 2 * a, *n as i16);
    }
    wr_f32(&mut o, 108, OUT_VOX_OFFSET as f64);
    wr_f32(&mut o, 292, origin[0] + lo[0] as f64 * pix[0]);
    wr_f32(&mut o, 308, origin[1] + lo[1] as f64 * pix[1]);
    wr_f32(&mut o, 324, origin[2] + lo[2] as f64 * pix[2]);
    if rd_i16(&h, 252) > 0 {
        let qfac = if rd_f32(&h, 76) < 0.0 { -1.0 } else { 1.0 };
        let r = quaternion_rotation(rd_f32(&h, 256), rd_f32(&h, 260), rd_f32(&h, 264));
        let step = [
            lo[0] as f64 * rd_f32(&h, 80),
            lo[1] as f64 * rd_f32(&h, 84),
            lo[2] as f64 * qfac * rd_f32(&h, 88),
        ];
        for (a, m) in r.iter().enumerate() {
            let shift: f64 = m.iter().zip(step).map(|(m, s)| m * s).sum();
            wr_f32(&mut o, 268 + 4 * a, rd_f32(&h, 268 + 4 * a) + shift);
        }
    }
    let out = File::create(dst).map_err(|x| io_err("cannot create the cropped file", x))?;
    let mut enc = BufWriter::with_capacity(1 << 20, out);
    enc.write_all(&o)
        .and_then(|()| enc.write_all(&[0u8; OUT_VOX_OFFSET - HEADER_LEN]))
        .map_err(|x| io_err("cannot write the cropped file", x))?;
    let row_bytes = out_dim[0] * 4;
    for k in 0..=hi[2] {
        if is_cancelled(cancel) {
            return Err(CANCELLED.to_owned());
        }
        rd.read_exact(&mut buf)
            .map_err(|x| io_err("the download ends before the last slice", x))?;
        if k < lo[2] {
            continue;
        }
        for j in lo[1]..=hi[1] {
            let start = (j * dim[0] + lo[0]) * 4;
            enc.write_all(&buf[start..start + row_bytes])
                .map_err(|x| io_err("cannot write the cropped file", x))?;
        }
    }
    enc.flush()
        .map_err(|x| io_err("cannot write the cropped file", x))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn value(i: usize, j: usize, k: usize) -> f32 {
        (i + 100 * j + 10_000 * k) as f32
    }

    fn write_nifti(path: &Path, dims: [usize; 3], pix: f32, origin: [f32; 3], datatype: i16) {
        let mut h = [0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&348i32.to_le_bytes());
        wr_i16(&mut h, 40, 3);
        for (a, d) in dims.iter().enumerate() {
            wr_i16(&mut h, 42 + 2 * a, *d as i16);
        }
        wr_i16(&mut h, 70, datatype);
        wr_i16(&mut h, 72, 32);
        wr_f32(&mut h, 76, 1.0);
        for a in 0..3 {
            wr_f32(&mut h, 80 + 4 * a, f64::from(pix));
        }
        wr_f32(&mut h, 108, 352.0);
        wr_f32(&mut h, 112, 1.0);
        wr_i16(&mut h, 252, 1);
        wr_i16(&mut h, 254, 1);
        for (a, o) in origin.iter().enumerate() {
            wr_f32(&mut h, 268 + 4 * a, f64::from(*o));
            wr_f32(&mut h, 280 + 16 * a + 4 * a, f64::from(pix));
            wr_f32(&mut h, 280 + 16 * a + 12, f64::from(*o));
        }
        h[344..348].copy_from_slice(b"n+1\0");
        let mut enc = GzEncoder::new(File::create(path).unwrap(), Compression::default());
        enc.write_all(&h).unwrap();
        enc.write_all(&[0u8; 4]).unwrap();
        for k in 0..dims[2] {
            for j in 0..dims[1] {
                for i in 0..dims[0] {
                    enc.write_all(&value(i, j, k).to_le_bytes()).unwrap();
                }
            }
        }
        enc.finish().unwrap();
    }

    fn read_nifti(path: &Path) -> ([u8; HEADER_LEN], Vec<f32>) {
        let mut all = Vec::new();
        if path.to_string_lossy().ends_with(".gz") {
            GzDecoder::new(File::open(path).unwrap())
                .read_to_end(&mut all)
                .unwrap();
        } else {
            all = std::fs::read(path).unwrap();
        }
        let mut h = [0u8; HEADER_LEN];
        h.copy_from_slice(&all[..HEADER_LEN]);
        let vals = all[352..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        (h, vals)
    }

    const CROP: [[f64; 2]; 3] = [[2.0, 5.0], [11.0, 14.0], [21.0, 23.0]];

    fn test_entry(file: &Path, crop: Option<[[f64; 2]; 3]>, dims: [usize; 3]) -> BackdropEntry {
        BackdropEntry {
            id: "test-backdrop",
            space: "TestSpace",
            res_mm: 1.0,
            title: "Test",
            url: "https://example.invalid/b.nii.gz",
            size: std::fs::metadata(file).unwrap().len(),
            sha256: leak(file_sha256_hex(file).unwrap()),
            crop_mm: crop,
            expected_dims: dims,
            licence_text: "CC0 1.0 Universal test",
        }
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn staged(d: &Path, name: &str, dims: [usize; 3]) -> (PathBuf, PathBuf) {
        let staging = d.join(name);
        std::fs::create_dir_all(&staging).unwrap();
        let src = staging.join("download.nii.gz");
        write_nifti(&src, dims, 1.0, [-0.5, 9.5, 19.5], 16);
        (staging, src)
    }

    #[test]
    fn crop_yields_expected_dims_origin_and_values() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src.nii.gz");
        write_nifti(&src, [10, 12, 14], 1.0, [-0.5, 9.5, 19.5], 16);
        let dst = d.path().join("dst.nii");
        crop_nifti(&src, &dst, [10, 12, 14], CROP, &no_cancel()).unwrap();
        let (h, vals) = read_nifti(&dst);
        let dims: Vec<i16> = (0..4).map(|i| rd_i16(&h, 40 + 2 * i)).collect();
        assert_eq!(dims, vec![3, 3, 3, 2]);
        assert_eq!(vals.len(), 3 * 3 * 2);
        assert!((rd_f32(&h, 292) - 2.5).abs() < 1e-5);
        assert!((rd_f32(&h, 308) - 11.5).abs() < 1e-5);
        assert!((rd_f32(&h, 324) - 21.5).abs() < 1e-5);
        assert!((rd_f32(&h, 268) - 2.5).abs() < 1e-5);
        assert!((rd_f32(&h, 276) - 21.5).abs() < 1e-5);
        assert_eq!(rd_f32(&h, 108), 352.0);
        assert_eq!(rd_f32(&h, 112), 1.0);
        let mut n = 0;
        for k in 0..2 {
            for j in 0..3 {
                for i in 0..3 {
                    assert_eq!(vals[n], value(i + 3, j + 2, k + 2), "{i} {j} {k}");
                    n += 1;
                }
            }
        }
    }

    #[test]
    fn crop_refuses_other_dims_and_datatype() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src.nii.gz");
        write_nifti(&src, [10, 12, 14], 1.0, [0.0, 0.0, 0.0], 16);
        let dst = d.path().join("dst.nii");
        let e = crop_nifti(&src, &dst, [10, 12, 15], CROP, &no_cancel()).unwrap_err();
        assert!(e.contains("dimensions"), "{e}");
        write_nifti(&src, [10, 12, 14], 1.0, [0.0, 0.0, 0.0], 4);
        let e = crop_nifti(&src, &dst, [10, 12, 14], CROP, &no_cancel()).unwrap_err();
        assert!(e.contains("float32"), "{e}");
    }

    #[test]
    fn install_crops_writes_metadata_and_cleans_staging() {
        let d = tempfile::tempdir().unwrap();
        let (staging, src) = staged(d.path(), ".partial-x", [10, 12, 14]);
        let e = test_entry(&src, Some(CROP), [10, 12, 14]);
        let dest = d.path().join("dest");
        let mut events = Vec::new();
        let p = install_from_download(
            &e,
            &src,
            &staging,
            &dest,
            &no_cancel(),
            &mut |ev| events.push(ev),
            None,
        )
        .unwrap();
        assert_eq!(p, dest.join(BACKDROP_FILE_PLAIN));
        assert!(p.is_file());
        let (h, vals) = read_nifti(&p);
        assert_eq!(rd_i16(&h, 42), 3);
        assert_eq!(vals.len(), 3 * 3 * 2);
        assert!(dest.join("LICENSE").is_file());
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dest.join("backdrop.json")).unwrap())
                .unwrap();
        assert_eq!(meta["id"], "test-backdrop");
        assert_eq!(meta["sha256"], e.sha256);
        assert!(!meta["crop_mm"].is_null());
        assert!(!staging.exists());
        assert!(events.contains(&BackdropEvent::Cropping));
    }

    #[test]
    fn install_verbatim_copies_the_file() {
        let d = tempfile::tempdir().unwrap();
        let (staging, src) = staged(d.path(), ".partial-x", [4, 4, 4]);
        let bytes = std::fs::read(&src).unwrap();
        let e = test_entry(&src, None, [4, 4, 4]);
        let dest = d.path().join("dest");
        install_from_download(&e, &src, &staging, &dest, &no_cancel(), &mut |_| {}, None).unwrap();
        assert_eq!(std::fs::read(dest.join(BACKDROP_FILE)).unwrap(), bytes);
        assert!(!staging.exists());
    }

    #[test]
    fn wrong_dims_install_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (staging, src) = staged(d.path(), ".partial-x", [10, 12, 14]);
        let e = test_entry(&src, Some(CROP), [11, 12, 14]);
        let dest = d.path().join("dest");
        let r = install_from_download(&e, &src, &staging, &dest, &no_cancel(), &mut |_| {}, None);
        assert!(r.is_err());
        assert!(!dest.exists());
        assert!(!staging.exists());
    }

    #[test]
    fn size_and_sha_mismatch_are_refused_and_staging_cleaned() {
        let d = tempfile::tempdir().unwrap();
        for case in 0..2 {
            let (staging, src) = staged(d.path(), &format!(".partial-{case}"), [4, 4, 4]);
            let mut e = test_entry(&src, None, [4, 4, 4]);
            let want = if case == 0 {
                e.size += 1;
                "size mismatch"
            } else {
                e.sha256 = leak("0".repeat(64));
                "SHA-256 mismatch"
            };
            let dest = d.path().join(format!("dest{case}"));
            let r =
                install_from_download(&e, &src, &staging, &dest, &no_cancel(), &mut |_| {}, None);
            let msg = r.unwrap_err();
            assert!(msg.contains(want), "{msg}");
            assert!(!dest.exists());
            assert!(!staging.exists());
        }
    }

    #[test]
    fn cancel_during_install_is_reported_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let (staging, src) = staged(d.path(), ".partial-x", [4, 4, 4]);
        let e = test_entry(&src, None, [4, 4, 4]);
        let dest = d.path().join("dest");
        let cancel = AtomicBool::new(true);
        let r = install_from_download(&e, &src, &staging, &dest, &cancel, &mut |_| {}, None);
        assert_eq!(r.unwrap_err(), CANCELLED);
        assert!(!staging.exists() && !dest.exists());
    }

    #[test]
    fn a_replacing_install_keeps_no_old_copy() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("old.txt"), b"old").unwrap();
        let (staging, src) = staged(d.path(), ".partial-x", [4, 4, 4]);
        let e = test_entry(&src, None, [4, 4, 4]);
        install_from_download(&e, &src, &staging, &dest, &no_cancel(), &mut |_| {}, None).unwrap();
        assert!(!dest.join("old.txt").exists());
        let aside: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|x| x.file_name().to_string_lossy().starts_with(".partial-"))
            .collect();
        assert!(aside.is_empty());
    }

    #[test]
    fn staging_names_never_repeat_within_a_process() {
        let e = &catalogue()[0];
        let r = Path::new("root");
        assert_ne!(staging_dir(e, r), staging_dir(e, r));
    }

    #[test]
    fn stale_staging_folders_are_swept_and_the_current_one_kept() {
        let d = tempfile::tempdir().unwrap();
        let e = &catalogue()[0];
        let parent = entry_dir_in(d.path(), e.space, e.id)
            .parent()
            .unwrap()
            .to_path_buf();
        let mk = |n: &str| {
            let p = parent.join(n);
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let mine = mk(&format!(".partial-{}-1-0", e.id));
        let other = mk(&format!(".partial-{}-9-3", e.id));
        let unrelated = mk(".partial-something-else-1-0");
        sweep_stale_staging(e, d.path(), &mine, Duration::from_secs(3600));
        assert!(mine.exists() && other.exists() && unrelated.exists());
        sweep_stale_staging(e, d.path(), &mine, Duration::ZERO);
        assert!(mine.exists());
        assert!(!other.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn free_space_check_message() {
        let e = &catalogue()[0];
        let err = check_free_space_with(e, Some(1_000_000)).unwrap_err();
        assert!(
            err.starts_with("not enough free disk space for edlow2019-7t-exvivo_res-0.5mm: need ")
        );
        assert!(err.ends_with("1 MB free"), "{err}");
        assert!(check_free_space_with(e, Some(u64::MAX)).is_ok());
        assert!(check_free_space_with(e, None).is_ok());
    }

    #[test]
    fn expected_output_for_the_deep_brain_entry_is_the_crop_volume() {
        let e = &catalogue()[1];
        let exact = 500 * 475 * 375 * 4;
        let got = expected_output_bytes(e);
        assert!(got >= exact && got < exact + exact / 50, "{got}");
    }

    #[test]
    fn real_free_space_is_reported() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_space_bytes(d.path()).is_some_and(|b| b > 0));
    }

    #[test]
    fn deep_brain_voxel_ranges_match_the_spec() {
        let e = &catalogue()[1];
        let c = e.crop_mm.unwrap();
        let origin = [
            f64::from(-98.05f32),
            f64::from(-134.05f32),
            f64::from(-72.05f32),
        ];
        let pix = f64::from(0.2f32);
        let r: Vec<(usize, usize)> = (0..3)
            .map(|a| voxel_range(c[a][0], c[a][1], origin[a], pix, e.expected_dims[a]).unwrap())
            .collect();
        assert_eq!(r, vec![(241, 740), (371, 845), (161, 535)]);
    }

    #[test]
    fn list_marks_installed_entries() {
        let d = tempfile::tempdir().unwrap();
        let none = list_backdrops_in(Some(d.path()), Some("MNI152NLin2009bAsym"));
        assert_eq!(none.len(), 2);
        assert!(none
            .iter()
            .all(|s| matches!(s.state, BackdropState::Available { .. })));
        let e = &catalogue()[0];
        let dir = entry_dir_in(d.path(), e.space, e.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(BACKDROP_FILE), b"x").unwrap();
        let some = list_backdrops_in(Some(d.path()), None);
        assert_eq!(some[0].state.label(), "installed");
        assert_eq!(some[1].state.label(), "available");
        assert!(list_backdrops_in(None, Some("Other")).is_empty());
    }
}
