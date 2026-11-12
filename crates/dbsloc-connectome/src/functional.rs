// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use fastwarp_matfile::Mat5Value;
use serde::Deserialize;

use crate::error::{format_err, io_err, Error, Result};
use crate::matio;
use crate::Affine;

const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

pub(crate) fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.strip_prefix('\u{feff}').unwrap_or(s).to_owned(),
        Err(_) => bytes
            .iter()
            .map(|b| match *b {
                0x80..=0x9F => char::from_u32(u32::from(CP1252_HIGH[usize::from(*b) - 0x80]))
                    .unwrap_or(char::REPLACEMENT_CHARACTER),
                other => char::from(other),
            })
            .collect(),
    }
}

/// A named subset: 1-based indices into the connectome's subjects.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Subset {
    pub name: String,
    #[serde(default)]
    pub subs: Vec<usize>,
}

#[derive(Deserialize)]
struct InfoJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    tag: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    subsets: Vec<Subset>,
    #[serde(default)]
    citation: Option<serde_json::Value>,
    #[serde(default)]
    formatversion: Option<f64>,
}

/// The voxel grid of the connectome mask: dimensions and the 0-based voxel-to-mm affine.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeGrid {
    pub dims: [usize; 3],
    pub affine: Affine,
}

/// One run file of one subject.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRef {
    /// 0-based index into the subject list.
    pub subject_index: usize,
    pub subject_id: String,
    pub file: String,
    pub path: PathBuf,
}

/// The voxel time courses of one run, voxel-major: voxel `v`, time `t` is `gmtc[v * n_t + t]`.
#[derive(Clone, Debug, PartialEq)]
pub struct RunTimecourses {
    /// 1-based column-major linear voxel indices, in the order of the voxel axis (not sorted).
    pub outidx: Vec<u32>,
    pub n_vox: usize,
    pub n_t: usize,
    pub gmtc: Vec<f32>,
}

/// A functional connectome folder (`specs/15` §3.3); run files are read one at a time.
#[derive(Clone, Debug)]
pub struct FunctionalConnectome {
    pub folder: PathBuf,
    pub name: String,
    pub tag: String,
    pub citation: Option<serde_json::Value>,
    pub format_version: f64,
    pub subsets: Vec<Subset>,
    pub grid: VolumeGrid,
    outidx: Vec<u32>,
    voxels: Vec<[i32; 3]>,
    subjects: Vec<String>,
    runs: Vec<RunRef>,
}

struct VolSurf {
    outidx: Vec<f64>,
    dim: Vec<f64>,
    mat: Vec<f64>,
    sub_ids: Vec<Vec<String>>,
}

fn volsurf(path: &Path, vars: &[(String, Mat5Value)]) -> Result<VolSurf> {
    let vol = vars
        .iter()
        .find(|(n, _)| n == "vol")
        .map(|(_, v)| v)
        .ok_or_else(|| format_err(format!("{}: variable `vol` is missing", path.display())))?;
    let need = |v: Option<&Mat5Value>, what: &str| -> Result<Vec<f64>> {
        match v {
            Some(Mat5Value::Double { data, .. }) => Ok(data.clone()),
            _ => Err(format_err(format!("vol.{what} is missing or not numeric"))),
        }
    };
    let outidx = need(vol.field("outidx", 0), "outidx")?;
    let space = vol
        .field("space", 0)
        .ok_or_else(|| format_err("vol.space is missing"))?;
    let dim = need(space.field("dim", 0), "space.dim")?;
    let mat = need(space.field("mat", 0), "space.mat")?;
    let mut sub_ids = Vec::new();
    if let Some(Mat5Value::Cell { items, .. }) = vol.field("subIDs", 0) {
        for entry in items {
            let Mat5Value::Cell { items: parts, .. } = entry else {
                return Err(format_err("vol.subIDs entries must be cells"));
            };
            let mut row = Vec::new();
            for p in parts {
                match p {
                    Mat5Value::Char(s) => row.push(s.clone()),
                    Mat5Value::Empty => row.push(String::new()),
                    _ => return Err(format_err("vol.subIDs entries must hold text")),
                }
            }
            sub_ids.push(row);
        }
    }
    Ok(VolSurf {
        outidx,
        dim,
        mat,
        sub_ids,
    })
}

fn grid_from(dim: &[f64], mat: &[f64]) -> Result<VolumeGrid> {
    if dim.len() != 3 || mat.len() != 16 {
        return Err(format_err(
            "vol.space needs a 3-element dim and a 4 x 4 mat",
        ));
    }
    let mut dims = [0usize; 3];
    for (d, v) in dims.iter_mut().zip(dim) {
        if !(v.is_finite() && v.fract() == 0.0 && *v >= 1.0 && *v <= 4096.0) {
            return Err(format_err(format!("vol.space.dim entry {v} is not usable")));
        }
        *d = *v as usize;
    }
    let mut m = [[0.0; 4]; 4];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            *v = mat[c * 4 + r];
        }
    }
    if m.iter().flatten().any(|v| !v.is_finite()) {
        return Err(format_err("vol.space.mat is not finite"));
    }
    for row in m.iter_mut().take(3) {
        row[3] += row[0] + row[1] + row[2];
    }
    Ok(VolumeGrid { dims, affine: m })
}

/// The voxel axis of a `gmtc` of MATLAB `dims` for `n` mask voxels.
pub(crate) fn voxel_axis_first(dims: &[usize], n: usize, format_version: f64) -> Result<bool> {
    match dims {
        [a, b] if *a == n && *b == n => Ok(format_version < 1.05),
        [a, _] if *a == n => Ok(true),
        [_, b] if *b == n => Ok(false),
        _ => Err(format_err(format!(
            "gmtc is {} but the mask has {n} voxels",
            dims.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(" x ")
        ))),
    }
}

const GMTC_CHUNK: usize = 1 << 20;

impl FunctionalConnectome {
    /// Opens `folder`: `dataset_info.json`, `dataset_volsurf.mat` and the run list; no run is read.
    pub fn open(folder: &Path) -> Result<Self> {
        let info_path = folder.join("dataset_info.json");
        let bytes = std::fs::read(&info_path).map_err(|e| io_err(&info_path, e))?;
        let info: InfoJson = serde_json::from_str(&decode_text(&bytes))
            .map_err(|e| format_err(format!("{}: {e}", info_path.display())))?;
        if let Some(kind) = &info.kind {
            if kind != "fMRI_timecourses" {
                return Err(Error::Unsupported(format!(
                    "functional connectome type {kind:?}"
                )));
            }
        }
        let format_version = info.formatversion.unwrap_or(1.0);
        let vs_path = folder.join("dataset_volsurf.mat");
        let vs = volsurf(&vs_path, &matio::open(&vs_path)?)?;
        let grid = grid_from(&vs.dim, &vs.mat)?;
        let total = grid.dims.iter().product::<usize>();
        let mut seen = HashSet::with_capacity(vs.outidx.len());
        let mut outidx = Vec::with_capacity(vs.outidx.len());
        let mut voxels = Vec::with_capacity(vs.outidx.len());
        if vs.outidx.is_empty() {
            return Err(format_err("vol.outidx is empty"));
        }
        for v in &vs.outidx {
            if !(v.is_finite() && v.fract() == 0.0 && *v >= 1.0 && *v <= total as f64) {
                return Err(format_err(format!(
                    "vol.outidx entry {v} is outside the grid"
                )));
            }
            let l = *v as usize;
            if !seen.insert(l) {
                return Err(format_err(format!("vol.outidx entry {l} is repeated")));
            }
            let z = l - 1;
            let [nx, ny, _] = grid.dims;
            voxels.push([
                (z % nx) as i32,
                ((z / nx) % ny) as i32,
                (z / (nx * ny)) as i32,
            ]);
            outidx.push(l as u32);
        }
        let mut subjects = Vec::new();
        let mut runs = Vec::new();
        for (s, row) in vs.sub_ids.iter().enumerate() {
            let id = row.first().cloned().unwrap_or_default();
            subjects.push(id.clone());
            for file in row.iter().skip(1) {
                if file.is_empty() || file.contains(['/', '\\']) || file == ".." || file == "." {
                    return Err(format_err(format!("unsafe run file name {file:?}")));
                }
                runs.push(RunRef {
                    subject_index: s,
                    subject_id: id.clone(),
                    file: file.clone(),
                    path: folder.join("vol").join(file),
                });
            }
        }
        for sub in &info.subsets {
            if let Some(bad) = sub.subs.iter().find(|i| **i == 0 || **i > subjects.len()) {
                return Err(format_err(format!(
                    "subset {:?} lists subject {bad} but there are {}",
                    sub.name,
                    subjects.len()
                )));
            }
        }
        Ok(Self {
            folder: folder.to_path_buf(),
            name: info.name,
            tag: info.tag,
            citation: info.citation,
            format_version,
            subsets: info.subsets,
            grid,
            outidx,
            voxels,
            subjects,
            runs,
        })
    }

    /// Every run of every subject in `vol.subIDs` order.
    pub fn runs(&self) -> &[RunRef] {
        &self.runs
    }

    /// The subject ids of `vol.subIDs`.
    pub fn subjects(&self) -> &[String] {
        &self.subjects
    }

    /// The 1-based linear voxel indices of the mask, in time-course order.
    pub fn outidx(&self) -> &[u32] {
        &self.outidx
    }

    /// The 0-based `[i, j, k]` of each mask voxel, in time-course order.
    pub fn voxels(&self) -> &[[i32; 3]] {
        &self.voxels
    }

    /// The indices into [`Self::runs`] of the subjects of subset `name`.
    pub fn runs_of_subset(&self, name: &str) -> Option<Vec<usize>> {
        let sub = self.subsets.iter().find(|s| s.name == name)?;
        Some(
            self.runs
                .iter()
                .enumerate()
                .filter(|(_, r)| sub.subs.contains(&(r.subject_index + 1)))
                .map(|(i, _)| i)
                .collect(),
        )
    }

    /// The size on disk of run `i` in bytes, a proxy for the memory a read of it needs; 0 when
    /// the run or its file is not found.
    pub fn run_size_hint(&self, i: usize) -> u64 {
        self.runs
            .get(i)
            .and_then(|r| std::fs::metadata(&r.path).ok())
            .map_or(0, |m| m.len())
    }

    /// Reads run `i` (`vol/<file>.mat`, MAT v5 / v7 or v7.3) as voxel-major `f32` time courses.
    pub fn read_run(&self, i: usize) -> Result<RunTimecourses> {
        let run = self
            .runs
            .get(i)
            .ok_or_else(|| format_err(format!("run {i} does not exist")))?;
        let src = matio::open_streaming(&run.path)?;
        let dims = src.shape("gmtc")?.ok_or_else(|| {
            format_err(format!(
                "{}: variable `gmtc` is missing",
                run.path.display()
            ))
        })?;
        let n = self.outidx.len();
        let first = voxel_axis_first(&dims, n, self.format_version)
            .map_err(|e| format_err(format!("{}: {e}", run.path.display())))?;
        let [rows, cols] = dims[..] else {
            return Err(format_err(format!(
                "{}: gmtc is not 2-D",
                run.path.display()
            )));
        };
        let n_t = if first { cols } else { rows };
        let mut out = vec![0f32; n * n_t];
        let all_cols: Vec<usize> = (0..cols).collect();
        src.stream_cols("gmtc", &all_cols, GMTC_CHUNK, &mut |col, chunk, row| {
            if first {
                for (i, v) in chunk.iter().enumerate() {
                    out[(row + i) * n_t + col] = *v;
                }
            } else {
                let at = col * n_t + row;
                out[at..at + chunk.len()].copy_from_slice(chunk);
            }
            Ok(())
        })?;
        Ok(RunTimecourses {
            outidx: self.outidx.clone(),
            n_vox: n,
            n_t,
            gmtc: out,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use fastwarp_matfile::mat5::write_mat5_file;

    const OUTIDX: [f64; 10] = [41.0, 42.0, 43.0, 30.0, 31.0, 7.0, 8.0, 55.0, 2.0, 60.0];

    fn id_cell(parts: &[&str]) -> Mat5Value {
        Mat5Value::cell_row(parts.iter().map(|p| Mat5Value::char(p)).collect())
    }

    fn write_volsurf(folder: &Path) {
        let mut mat = vec![0.0; 16];
        mat[0] = 2.0;
        mat[5] = 2.0;
        mat[10] = 2.0;
        mat[15] = 1.0;
        mat[12] = -9.0;
        mat[13] = -13.0;
        mat[14] = -7.0;
        let space = Mat5Value::struct_scalar(vec![
            (
                "dim".into(),
                Mat5Value::Double {
                    dims: vec![3, 1],
                    data: vec![5.0, 4.0, 3.0],
                },
            ),
            (
                "mat".into(),
                Mat5Value::Double {
                    dims: vec![4, 4],
                    data: mat,
                },
            ),
        ]);
        let sub_ids = Mat5Value::Cell {
            dims: vec![2, 1],
            items: vec![
                id_cell(&["0001", "sub-0001_run_1.mat", "sub-0001_run_2.mat"]),
                id_cell(&["0002"]),
            ],
        };
        let vol = Mat5Value::struct_scalar(vec![
            ("outidx".into(), Mat5Value::row(&OUTIDX)),
            ("space".into(), space),
            ("subIDs".into(), sub_ids),
        ]);
        write_mat5_file(
            &folder.join("dataset_volsurf.mat"),
            &[("vol".to_owned(), vol)],
            true,
        )
        .unwrap();
    }

    fn value(v: usize, t: usize) -> f64 {
        (v * 100 + t) as f64
    }

    fn write_runs(folder: &Path) {
        std::fs::create_dir_all(folder.join("vol")).unwrap();
        let mut a = vec![0.0; 60];
        let mut b = vec![0.0; 60];
        for v in 0..10 {
            for t in 0..6 {
                a[t * 10 + v] = value(v, t);
                b[v * 6 + t] = value(v, t) + 0.5;
            }
        }
        for (name, dims, data) in [
            ("sub-0001_run_1.mat", vec![10, 6], a),
            ("sub-0001_run_2.mat", vec![6, 10], b),
        ] {
            write_mat5_file(
                &folder.join("vol").join(name),
                &[("gmtc".to_owned(), Mat5Value::Double { dims, data })],
                true,
            )
            .unwrap();
        }
    }

    fn fixture(info: &[u8]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("dataset_info.json"), info).unwrap();
        write_volsurf(d.path());
        write_runs(d.path());
        d
    }

    const INFO_UTF8: &str = r#"{"name":"Fixture","tag":"FIX","type":"fMRI_timecourses","subsets":[{"name":"all","subs":[1,2]},{"name":"first","subs":[1]}],"citation":{"short":"Köhler – 2020","long":["a","b"]}}"#;

    #[test]
    fn two_runs_read_with_both_orientations() {
        let d = fixture(INFO_UTF8.as_bytes());
        let c = FunctionalConnectome::open(d.path()).unwrap();
        assert_eq!(c.name, "Fixture");
        assert_eq!(c.tag, "FIX");
        assert_eq!(c.runs().len(), 2);
        assert_eq!(c.subjects(), &["0001".to_owned(), "0002".to_owned()]);
        assert_eq!(c.runs()[1].file, "sub-0001_run_2.mat");
        assert_eq!(c.grid.dims, [5, 4, 3]);
        assert_eq!(c.grid.affine[0][3], -7.0);
        assert_eq!(c.grid.affine[1][3], -11.0);
        assert_eq!(c.grid.affine[2][3], -5.0);
        assert_eq!(c.outidx()[0], 41);
        assert_eq!(c.voxels()[0], [0, 0, 2]);
        assert_eq!(c.voxels()[3], [4, 1, 1]);
        assert_eq!(c.voxels()[8], [1, 0, 0]);
        for (i, offset) in [(0usize, 0.0f32), (1, 0.5)] {
            let r = c.read_run(i).unwrap();
            assert_eq!((r.n_vox, r.n_t), (10, 6));
            assert_eq!(r.outidx[3], 30);
            for v in 0..10 {
                for t in 0..6 {
                    assert_eq!(r.gmtc[v * 6 + t], value(v, t) as f32 + offset);
                }
            }
        }
        assert!(c.read_run(2).is_err());
    }

    #[test]
    fn subsets_select_runs() {
        let d = fixture(INFO_UTF8.as_bytes());
        let c = FunctionalConnectome::open(d.path()).unwrap();
        assert_eq!(c.runs_of_subset("all").unwrap(), vec![0, 1]);
        assert_eq!(c.runs_of_subset("first").unwrap(), vec![0, 1]);
        assert!(c.runs_of_subset("none").is_none());
    }

    #[test]
    fn info_decodes_utf8_and_windows_1252() {
        let d = fixture(INFO_UTF8.as_bytes());
        let c = FunctionalConnectome::open(d.path()).unwrap();
        let short = c.citation.as_ref().unwrap()["short"].as_str().unwrap();
        assert_eq!(short, "Köhler – 2020");
        let mut raw = Vec::new();
        raw.extend_from_slice(
            br#"{"name":"Fixture","tag":"FIX","type":"fMRI_timecourses","subsets":[],"citation":{"short":"K"#,
        );
        raw.push(0xF6);
        raw.extend_from_slice(b"hler ");
        raw.push(0x96);
        raw.extend_from_slice(br#" 2020"}}"#);
        assert!(std::str::from_utf8(&raw).is_err());
        let d2 = fixture(&raw);
        let c2 = FunctionalConnectome::open(d2.path()).unwrap();
        let short = c2.citation.as_ref().unwrap()["short"].as_str().unwrap();
        assert_eq!(short, "Köhler – 2020");
    }

    #[test]
    fn orientation_rule_and_validation() {
        assert!(voxel_axis_first(&[10, 6], 10, 1.0).unwrap());
        assert!(!voxel_axis_first(&[6, 10], 10, 1.0).unwrap());
        assert!(voxel_axis_first(&[10, 10], 10, 1.0).unwrap());
        assert!(!voxel_axis_first(&[10, 10], 10, 1.1).unwrap());
        assert!(voxel_axis_first(&[7, 6], 10, 1.0).is_err());
        let d = fixture(br#"{"name":"F","type":"fMRI_matrix"}"#);
        assert!(matches!(
            FunctionalConnectome::open(d.path()),
            Err(Error::Unsupported(_))
        ));
        let d = fixture(br#"{"name":"F","subsets":[{"name":"x","subs":[3]}]}"#);
        assert!(FunctionalConnectome::open(d.path()).is_err());
    }

    #[test]
    fn windows_1252_table_matches_known_characters() {
        assert_eq!(
            decode_text(&[0x80, 0x93, 0x94, 0xE4]),
            "\u{20AC}\u{201C}\u{201D}ä"
        );
    }
}
