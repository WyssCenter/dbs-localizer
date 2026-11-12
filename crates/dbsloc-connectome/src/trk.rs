// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Read;
use std::path::Path;

use crate::error::{format_err, io_err, Result};
use crate::fibres::FibreSet;
use crate::Affine;

const HEADER_LEN: usize = 1000;

struct Header {
    big_endian: bool,
    voxel_size: [f32; 3],
    n_scalars: usize,
    n_properties: usize,
    vox_to_ras: [[f32; 4]; 4],
    n_count: i32,
}

struct Reader<'a> {
    bytes: &'a [u8],
    big: bool,
}

impl Reader<'_> {
    fn u32_at(&self, pos: usize) -> Option<u32> {
        let b: [u8; 4] = self.bytes.get(pos..pos + 4)?.try_into().ok()?;
        Some(if self.big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }

    fn i32_at(&self, pos: usize) -> Option<i32> {
        self.u32_at(pos).map(|v| v as i32)
    }

    fn f32_at(&self, pos: usize) -> Option<f32> {
        self.u32_at(pos).map(f32::from_bits)
    }

    fn i16_at(&self, pos: usize) -> Option<i16> {
        let b: [u8; 2] = self.bytes.get(pos..pos + 2)?.try_into().ok()?;
        Some(if self.big {
            i16::from_be_bytes(b)
        } else {
            i16::from_le_bytes(b)
        })
    }
}

fn parse_header(bytes: &[u8]) -> Result<Header> {
    if bytes.len() < HEADER_LEN {
        return Err(format_err(
            "TrackVis file shorter than its 1000-byte header",
        ));
    }
    if &bytes[..5] != b"TRACK" {
        return Err(format_err("not a TrackVis file (id_string is not TRACK)"));
    }
    let le = Reader { bytes, big: false };
    let big = match le.i32_at(996) {
        Some(1000) => false,
        _ => {
            let be = Reader { bytes, big: true };
            if be.i32_at(996) == Some(1000) {
                true
            } else {
                return Err(format_err("TrackVis hdr_size is not 1000"));
            }
        }
    };
    let r = Reader { bytes, big };
    let field = |v: Option<i16>| -> Result<usize> {
        usize::try_from(v.ok_or_else(|| format_err("truncated TrackVis header"))?)
            .map_err(|_| format_err("negative scalar or property count"))
    };
    let mut voxel_size = [0f32; 3];
    for (i, v) in voxel_size.iter_mut().enumerate() {
        *v = r
            .f32_at(12 + 4 * i)
            .ok_or_else(|| format_err("truncated TrackVis header"))?;
    }
    let mut vox_to_ras = [[0f32; 4]; 4];
    for (i, row) in vox_to_ras.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = r
                .f32_at(440 + 4 * (4 * i + j))
                .ok_or_else(|| format_err("truncated TrackVis header"))?;
        }
    }
    Ok(Header {
        big_endian: big,
        voxel_size,
        n_scalars: field(r.i16_at(36))?,
        n_properties: field(r.i16_at(238))?,
        vox_to_ras,
        n_count: r
            .i32_at(988)
            .ok_or_else(|| format_err("truncated TrackVis header"))?,
    })
}

/// Reads TrackVis `.trk` bytes into a [`FibreSet`] in the millimetres of the connectome's space.
pub fn read_trk_bytes(bytes: &[u8], reference: Option<&Affine>) -> Result<FibreSet> {
    let h = parse_header(bytes)?;
    let mut m: Affine = [[0.0; 4]; 4];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = f64::from(h.vox_to_ras[i][j]);
        }
    }
    if m.iter().flatten().all(|v| *v == 0.0) {
        m = *reference.ok_or_else(|| {
            format_err("TrackVis vox_to_ras is all zero and no reference image was given")
        })?;
    }
    if h.voxel_size.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(format_err("TrackVis voxel_size is not positive"));
    }
    let r = Reader {
        bytes,
        big: h.big_endian,
    };
    let per_point = 3 + h.n_scalars;
    let mut pos = HEADER_LEN;
    let mut points: Vec<[f32; 3]> = Vec::new();
    let mut offsets = vec![0u32];
    let mut tracks = 0i64;
    while pos < bytes.len() && (h.n_count <= 0 || tracks < i64::from(h.n_count)) {
        let n = r
            .i32_at(pos)
            .ok_or_else(|| format_err("truncated TrackVis track header"))?;
        let n = usize::try_from(n).map_err(|_| format_err("negative TrackVis point count"))?;
        pos += 4;
        let need = n
            .checked_mul(per_point)
            .and_then(|v| v.checked_add(h.n_properties))
            .and_then(|v| v.checked_mul(4))
            .ok_or_else(|| format_err("TrackVis track too large"))?;
        if bytes.len() - pos < need {
            return Err(format_err("truncated TrackVis track data"));
        }
        for i in 0..n {
            let base = pos + 4 * i * per_point;
            let mut p = [0f64; 3];
            for (a, v) in p.iter_mut().enumerate() {
                let raw = r
                    .f32_at(base + 4 * a)
                    .ok_or_else(|| format_err("truncated TrackVis track data"))?;
                if !raw.is_finite() {
                    return Err(format_err("non-finite TrackVis coordinate"));
                }
                *v = f64::from(raw) / f64::from(h.voxel_size[a]) - 0.5;
            }
            let row = |k: usize| m[k][0] * p[0] + m[k][1] * p[1] + m[k][2] * p[2] + m[k][3];
            points.push([row(0) as f32, row(1) as f32, row(2) as f32]);
        }
        pos += need;
        tracks += 1;
        if n > 0 {
            offsets.push(
                u32::try_from(points.len())
                    .map_err(|_| format_err("more than 2^32 - 1 points in one connectome"))?,
            );
        }
    }
    if h.n_count > 0 && tracks < i64::from(h.n_count) {
        return Err(format_err("TrackVis file has fewer tracks than n_count"));
    }
    Ok(FibreSet {
        points,
        fibre_offsets: offsets,
        space: crate::fibres::FibreSpace::Mm,
        vals: Vec::new(),
        mirrored: None,
    })
}

/// Reads a `.trk` or `.trk.gz` file (gzip detected by its magic bytes).
pub fn read_trk(path: &Path, reference: Option<&Affine>) -> Result<FibreSet> {
    let raw = std::fs::read(path).map_err(|e| io_err(path, e))?;
    let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(raw.as_slice())
            .read_to_end(&mut out)
            .map_err(|e| io_err(path, e))?;
        out
    } else {
        raw
    };
    read_trk_bytes(&bytes, reference)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build(
        tracks: &[Vec<[f32; 3]>],
        voxel_size: [f32; 3],
        m: [[f32; 4]; 4],
        big: bool,
    ) -> Vec<u8> {
        let put32 = |b: &mut Vec<u8>, v: u32| {
            b.extend_from_slice(&if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            })
        };
        let mut h = vec![0u8; HEADER_LEN];
        h[..6].copy_from_slice(b"TRACK\0");
        let mut tmp = Vec::new();
        for (i, v) in voxel_size.iter().enumerate() {
            tmp.clear();
            put32(&mut tmp, v.to_bits());
            h[12 + 4 * i..16 + 4 * i].copy_from_slice(&tmp);
        }
        for (i, row) in m.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                tmp.clear();
                put32(&mut tmp, v.to_bits());
                let at = 440 + 4 * (4 * i + j);
                h[at..at + 4].copy_from_slice(&tmp);
            }
        }
        tmp.clear();
        put32(&mut tmp, tracks.len() as u32);
        h[988..992].copy_from_slice(&tmp);
        tmp.clear();
        put32(&mut tmp, 2);
        h[992..996].copy_from_slice(&tmp);
        tmp.clear();
        put32(&mut tmp, 1000);
        h[996..1000].copy_from_slice(&tmp);
        for t in tracks {
            put32(&mut h, t.len() as u32);
            for p in t {
                for c in p {
                    put32(&mut h, c.to_bits());
                }
            }
        }
        h
    }

    fn affine() -> [[f32; 4]; 4] {
        [
            [-2.0, 0.0, 0.0, 90.0],
            [0.0, 2.0, 0.0, -126.0],
            [0.0, 0.0, 2.0, -72.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    #[test]
    fn voxel_centre_convention_is_applied() {
        let tracks = vec![
            vec![[1.0, 3.0, 5.0], [3.0, 3.0, 5.0]],
            vec![[0.0, 0.0, 0.0]],
        ];
        let bytes = build(&tracks, [2.0, 2.0, 2.0], affine(), false);
        let set = read_trk_bytes(&bytes, None).unwrap();
        assert_eq!(set.n_fibres(), 2);
        assert_eq!(set.fibre_offsets, vec![0, 2, 3]);
        let p = set.points[0];
        assert!((p[0] - 90.0).abs() < 1e-5, "{p:?}");
        assert!((p[1] - (-126.0 + 2.0)).abs() < 1e-5, "{p:?}");
        assert!((p[2] - (-72.0 + 4.0)).abs() < 1e-5, "{p:?}");
        let q = set.points[2];
        assert!(
            (q[0] - 91.0).abs() < 1e-5 && (q[1] - -127.0).abs() < 1e-5,
            "{q:?}"
        );
        assert!((q[2] - -73.0).abs() < 1e-5, "{q:?}");
    }

    #[test]
    fn big_endian_and_gzip_read_the_same() {
        let tracks = vec![vec![[1.0, 3.0, 5.0], [3.0, 3.0, 5.0]]];
        let le = read_trk_bytes(&build(&tracks, [2.0; 3], affine(), false), None).unwrap();
        let be = read_trk_bytes(&build(&tracks, [2.0; 3], affine(), true), None).unwrap();
        assert_eq!(le, be);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.trk.gz");
        let mut enc = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        enc.write_all(&build(&tracks, [2.0; 3], affine(), false))
            .unwrap();
        enc.finish().unwrap();
        assert_eq!(read_trk(&path, None).unwrap(), le);
        assert_eq!(crate::read_fibres(&path).unwrap(), le);
    }

    #[test]
    fn zero_matrix_needs_a_reference() {
        let tracks = vec![vec![[2.0, 2.0, 2.0]]];
        let bytes = build(&tracks, [2.0; 3], [[0.0; 4]; 4], false);
        assert!(read_trk_bytes(&bytes, None).is_err());
        let reference = [
            [1.0, 0.0, 0.0, 10.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let set = read_trk_bytes(&bytes, Some(&reference)).unwrap();
        assert!((set.points[0][0] - 10.5).abs() < 1e-5);
    }

    #[test]
    fn truncated_and_bad_files_are_errors() {
        let tracks = vec![vec![[1.0, 3.0, 5.0], [3.0, 3.0, 5.0]]];
        let bytes = build(&tracks, [2.0; 3], affine(), false);
        assert!(read_trk_bytes(&bytes[..bytes.len() - 3], None).is_err());
        assert!(read_trk_bytes(&bytes[..500], None).is_err());
        let mut bad = bytes.clone();
        bad[0] = b'X';
        assert!(read_trk_bytes(&bad, None).is_err());
    }
}
