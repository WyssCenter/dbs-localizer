// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Network mapping at group level (`specs/14` §9): voxel-wise maps over per-patient seed maps,
//! similarity prediction, cross-validation and the output files.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use flate2::write::GzEncoder;
use flate2::Compression;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::fibrefilter::{
    cancelled, clinical_covariate, clinical_outcome, commit_staged, create_analysis_dir, evaluate,
    included_patients, lg, permutation_null, prediction_pairs, run_cv, usage, AnalysisError,
    AnalysisOutputs, AnalysisResult, CvScheme, Evaluation,
};
use super::record::{file_stem, subject_id};
use super::stats_common::{
    adjust_p, correlate, covariate_residuals, fisher_z, linear_regression, mean, t_test_one_sample,
    CorrKind, Correction, TestResult,
};
use super::{GroupRecord, Note};
use crate::connectome_catalog::ConnectomeClass;
use crate::connectome_fetch::ConnectomeRoots;
use crate::connectome_maps::job::{
    functional_label, resolve_connectome, stimulation_folder, ResolvedConnectome,
};
use crate::connectome_maps::nii::{encode_nii, read_nii, resample, Affine, Interp, NiiVolume};
use crate::connectome_maps::{report, MapError, ProgressFn};
use crate::outputs::partial_of;
use crate::sha256::file_sha256_hex;
use crate::version::{utc_now_rfc3339, version_string};

/// The map of `specs/14` §9.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MapKind {
    RMap,
    AMap,
    CMap,
    NMap,
    TMap,
}

impl MapKind {
    /// The `stat-` entity value: `rmap`, `amap`, `cmap`, `nmap` or `t`.
    pub fn name(self) -> &'static str {
        match self {
            MapKind::RMap => "rmap",
            MapKind::AMap => "amap",
            MapKind::CMap => "cmap",
            MapKind::NMap => "nmap",
            MapKind::TMap => "t",
        }
    }

    /// Parses the `stat-` value.
    pub fn parse(text: &str) -> Option<MapKind> {
        match text.trim().to_ascii_lowercase().as_str() {
            "rmap" | "r" => Some(MapKind::RMap),
            "amap" | "a" => Some(MapKind::AMap),
            "cmap" | "c" => Some(MapKind::CMap),
            "nmap" | "n" => Some(MapKind::NMap),
            "t" | "tmap" => Some(MapKind::TMap),
            _ => None,
        }
    }
}

/// The mask of the similarity prediction (`specs/14` §9.3).
#[derive(Clone, Debug, PartialEq)]
pub enum MaskChoice {
    All,
    Grey,
    Brain,
    File(PathBuf),
}

/// Settings of a network mapping analysis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetmapSettings {
    pub map: MapKind,
    pub corr: CorrKind,
    pub n_threshold: f64,
    pub show_positive: bool,
    pub show_negative: bool,
    pub significance: Option<Correction>,
    pub alpha: f64,
    pub fisher_z: bool,
}

impl Default for NetmapSettings {
    fn default() -> Self {
        NetmapSettings {
            map: MapKind::RMap,
            corr: CorrKind::Spearman,
            n_threshold: 0.2,
            show_positive: true,
            show_negative: false,
            significance: None,
            alpha: 0.05,
            fisher_z: false,
        }
    }
}

/// The in-memory input of a network mapping analysis.
#[derive(Clone, Debug)]
pub struct NetmapData {
    pub dims: [usize; 3],
    pub affine: Affine,
    pub rows: Vec<Vec<f32>>,
    pub row_patient: Vec<usize>,
    pub flipped: Vec<bool>,
    pub n_patients: usize,
    pub outcome: Vec<f64>,
    pub covariates: Vec<Vec<f64>>,
    pub group_of: Vec<u32>,
    pub mask: Vec<bool>,
}

/// A voxel map with the p values behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct MapOut {
    pub vals: Vec<f32>,
    pub p: Vec<f64>,
}

fn training_y(data: &NetmapData, outcome: &[f64], train: &[bool]) -> (Vec<usize>, Vec<f64>) {
    let rows: Vec<usize> = (0..data.rows.len())
        .filter(|r| train[data.row_patient[*r]])
        .collect();
    let mut y: Vec<f64> = rows.iter().map(|r| outcome[data.row_patient[*r]]).collect();
    if !data.covariates.is_empty() {
        let covs: Vec<Vec<f64>> = data
            .covariates
            .iter()
            .map(|c| rows.iter().map(|r| c[data.row_patient[*r]]).collect())
            .collect();
        y = covariate_residuals(&y, &covs);
    }
    (rows, y)
}

/// The voxel map of `settings.map` over `rows` (one seed map per entry) and the outcome `y`
/// (`specs/14` §9.2). Pairs with a non-finite value are left out voxel by voxel; fewer than 3
/// pairs give NaN for correlation-based maps.
pub fn compute_map(
    rows: &[&[f32]],
    y: &[f64],
    s: &NetmapSettings,
    cancel: &AtomicBool,
) -> AnalysisResult<MapOut> {
    let v = rows.first().map_or(0, |r| r.len());
    let t = s.n_threshold;
    let cells: Vec<(f32, f64)> = (0..v)
        .into_par_iter()
        .map(|i| {
            let mut xs = Vec::with_capacity(rows.len());
            let mut ys = Vec::with_capacity(rows.len());
            for (r, row) in rows.iter().enumerate() {
                let x = f64::from(row[i]);
                if x.is_finite() && y[r].is_finite() {
                    xs.push(x);
                    ys.push(y[r]);
                }
            }
            let nan = (f32::NAN, f64::NAN);
            match s.map {
                MapKind::RMap => {
                    let (r, p) = correlate(s.corr, &xs, &ys);
                    (r as f32, p)
                }
                MapKind::AMap => {
                    if xs.is_empty() {
                        nan
                    } else {
                        (
                            xs.iter().zip(&ys).map(|(a, b)| a * b).sum::<f64>() as f32,
                            f64::NAN,
                        )
                    }
                }
                MapKind::CMap => {
                    if xs.is_empty() {
                        return nan;
                    }
                    let a: f64 = xs.iter().zip(&ys).map(|(a, b)| a * b).sum();
                    let (r, _) = correlate(s.corr, &xs, &ys);
                    if r.is_finite() && r.signum() == a.signum() && a != 0.0 {
                        (a as f32, f64::NAN)
                    } else {
                        nan
                    }
                }
                MapKind::NMap => {
                    if xs.is_empty() {
                        return nan;
                    }
                    let pos = if s.show_positive {
                        xs.iter().filter(|x| **x > t).count()
                    } else {
                        0
                    };
                    let neg = if s.show_negative {
                        xs.iter().filter(|x| **x < -t).count()
                    } else {
                        0
                    };
                    ((pos as f64 - neg as f64) as f32, f64::NAN)
                }
                MapKind::TMap => {
                    let TestResult { stat: tv, p } = t_test_one_sample(&xs, 0.0);
                    (tv as f32, p)
                }
            }
        })
        .collect();
    cancelled(cancel)?;
    let mut out = MapOut {
        vals: cells.iter().map(|c| c.0).collect(),
        p: cells.iter().map(|c| c.1).collect(),
    };
    if let Some(c) = s.significance {
        if matches!(s.map, MapKind::RMap | MapKind::TMap) {
            let adj = adjust_p(&out.p, c);
            for (v, a) in out.vals.iter_mut().zip(&adj) {
                if a.is_finite() && *a > s.alpha {
                    *v = f32::NAN;
                }
            }
        }
    }
    Ok(out)
}

/// The map with the hidden sign set to NaN.
pub fn visible(map: &[f32], show_positive: bool, show_negative: bool) -> Vec<f32> {
    map.iter()
        .map(|v| {
            if (*v > 0.0 && !show_positive) || (*v < 0.0 && !show_negative) {
                f32::NAN
            } else {
                *v
            }
        })
        .collect()
}

/// Similarity of a seed map to a training map inside the mask: the correlation of the chosen
/// kind over voxels where both are finite (Fisher z when requested); NaN below 3 voxels.
pub fn similarity(
    train_map: &[f32],
    seed_map: &[f32],
    mask: &[bool],
    corr: CorrKind,
    fisher: bool,
) -> f64 {
    let mut a = Vec::new();
    let mut b = Vec::new();
    for i in 0..train_map.len() {
        if mask[i] && train_map[i].is_finite() && seed_map[i].is_finite() {
            a.push(f64::from(train_map[i]));
            b.push(f64::from(seed_map[i]));
        }
    }
    let (r, _) = correlate(corr, &a, &b);
    if fisher {
        fisher_z(r)
    } else {
        r
    }
}

fn train_map(
    data: &NetmapData,
    outcome: &[f64],
    train: &[bool],
    s: &NetmapSettings,
    cancel: &AtomicBool,
) -> AnalysisResult<MapOut> {
    let (rows, y) = training_y(data, outcome, train);
    let refs: Vec<&[f32]> = rows.iter().map(|r| data.rows[*r].as_slice()).collect();
    compute_map(&refs, &y, s, cancel)
}

fn predict_patients(
    data: &NetmapData,
    map: &MapOut,
    s: &NetmapSettings,
    test: &[usize],
) -> Vec<[f64; 2]> {
    let vis = visible(&map.vals, s.show_positive, s.show_negative);
    test.iter()
        .map(|j| {
            let row =
                (0..data.rows.len()).find(|r| data.row_patient[*r] == *j && !data.flipped[*r]);
            let sim = row.map_or(f64::NAN, |r| {
                similarity(&vis, &data.rows[r], &data.mask, s.corr, s.fisher_z)
            });
            [sim, f64::NAN]
        })
        .collect()
}

/// The stored model of `specs/14` §9.4.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetmapModel {
    pub connectome_id: String,
    pub mask: String,
    pub settings: NetmapSettings,
    pub beta0: Option<f64>,
    pub beta1: Option<f64>,
    pub n: usize,
    pub r_train: f64,
    pub map_file: String,
}

/// A new patient's prediction.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewPrediction {
    pub similarity: f64,
    pub predicted: f64,
}

/// Applies a model to a new patient's seed map of the same connectome and grid (`specs/14` §9.4).
pub fn predict_new(
    model: &NetmapModel,
    train_map_values: &[f32],
    mask: &[bool],
    connectome_id: &str,
    seed_map: &[f32],
) -> AnalysisResult<NewPrediction> {
    if model.connectome_id != connectome_id {
        return Err(AnalysisError::Map(MapError::Usage(format!(
            "the model was built with connectome {}, not {connectome_id}",
            model.connectome_id
        ))));
    }
    if seed_map.len() != train_map_values.len() || mask.len() != seed_map.len() {
        return Err(AnalysisError::Map(MapError::Space(
            "the seed map is not on the model's grid".into(),
        )));
    }
    let vis = visible(
        train_map_values,
        model.settings.show_positive,
        model.settings.show_negative,
    );
    let sim = similarity(
        &vis,
        seed_map,
        mask,
        model.settings.corr,
        model.settings.fisher_z,
    );
    let predicted = match (model.beta0, model.beta1) {
        (Some(b0), Some(b1)) => b0 + b1 * sim,
        _ => f64::NAN,
    };
    Ok(NewPrediction {
        similarity: sim,
        predicted,
    })
}

/// Everything a network mapping analysis computes.
#[derive(Clone, Debug)]
pub struct NetmapResult {
    pub map: MapOut,
    pub coverage: Vec<f32>,
    pub ihat: Vec<[f64; 2]>,
    pub valid: Vec<usize>,
    pub evaluation: Evaluation,
    pub p_perm: Option<f64>,
    pub null95: Option<f64>,
    pub model: NetmapModel,
    pub scheme: CvScheme,
    pub notes: Vec<Note>,
}

/// Computes the map, the cross-validation and the model (`specs/14` §9.2 to §9.4).
#[allow(clippy::too_many_arguments)]
pub fn compute_netmap(
    data: &NetmapData,
    settings: &NetmapSettings,
    scheme: &CvScheme,
    connectome_id: &str,
    mask_name: &str,
    seed: u64,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> AnalysisResult<NetmapResult> {
    let has_row: Vec<bool> = {
        let mut h = vec![false; data.n_patients];
        for (r, p) in data.row_patient.iter().enumerate() {
            if !data.flipped[r] {
                h[*p] = true;
            }
        }
        h
    };
    let valid: Vec<usize> = (0..data.n_patients)
        .filter(|j| {
            has_row[*j]
                && data.outcome[*j].is_finite()
                && data.covariates.iter().all(|c| c[*j].is_finite())
        })
        .collect();
    if valid.len() < 3 {
        return Err(lg(
            "E_GROUP_TOO_FEW",
            format!(
                "{} patients with an outcome; at least 3 are needed",
                valid.len()
            ),
        ));
    }
    let mut notes = Vec::new();
    let ys: Vec<f64> = valid.iter().map(|j| data.outcome[*j]).collect();
    if ys.windows(2).all(|w| w[0] == w[1]) {
        notes.push(Note::plain("outcome has no variance"));
    }
    let mut all = vec![false; data.n_patients];
    for j in &valid {
        all[*j] = true;
    }
    report(progress, "netmap", 0, 3, "map");
    let map = train_map(data, &data.outcome, &all, settings, cancel)?;
    let coverage: Vec<f32> = (0..map.vals.len())
        .map(|i| {
            data.rows
                .iter()
                .enumerate()
                .filter(|(r, row)| all[data.row_patient[*r]] && row[i].is_finite())
                .count() as f32
        })
        .collect();
    report(progress, "netmap", 1, 3, "cross-validation");
    let predict_with = |outcome: &[f64], train: &[bool], test: &[usize]| {
        let m = train_map(data, outcome, train, settings, cancel)?;
        Ok(predict_patients(data, &m, settings, test))
    };
    let ihat = run_cv(
        &valid,
        &data.group_of,
        scheme,
        seed,
        data.n_patients,
        cancel,
        |train, test| predict_with(&data.outcome, train, test),
    )?;
    let wrap = |o: &[f64]| [o.to_vec(), o.to_vec()];
    let pairs = |ih: &[[f64; 2]], o: &[f64]| prediction_pairs(ih, &wrap(o), false, &valid);
    let (a, b) = pairs(&ihat, &data.outcome);
    let evaluation = evaluate(&a, &b);
    let (mut p_perm, mut null95) = (None, None);
    let lno_pred = |outcome: &[f64]| -> AnalysisResult<Vec<[f64; 2]>> {
        let pred = predict_with(outcome, &all, &valid)?;
        let mut full = vec![[f64::NAN; 2]; data.n_patients];
        for (j, p) in valid.iter().zip(pred) {
            full[*j] = p;
        }
        Ok(full)
    };
    if let CvScheme::LeaveNothingOut { permutations } = scheme {
        report(progress, "netmap", 2, 3, "permutations");
        let (p, q) = permutation_null(
            &valid,
            &wrap(&data.outcome),
            evaluation.r,
            *permutations,
            seed,
            cancel,
            |perm| {
                let full = lno_pred(&perm[0])?;
                let (a, b) = pairs(&full, &perm[0]);
                Ok(evaluate(&a, &b).r)
            },
        )?;
        p_perm = Some(p);
        null95 = Some(q);
    }
    let lno = if matches!(scheme, CvScheme::LeaveNothingOut { .. }) {
        (a, b)
    } else {
        let full = lno_pred(&data.outcome)?;
        pairs(&full, &data.outcome)
    };
    let beta = linear_regression(&lno.0, &lno.1);
    let model = NetmapModel {
        connectome_id: connectome_id.to_owned(),
        mask: mask_name.to_owned(),
        settings: settings.clone(),
        beta0: beta.map(|b| b.0),
        beta1: beta.map(|b| b.1),
        n: valid.len(),
        r_train: evaluate(&lno.0, &lno.1).r,
        map_file: String::new(),
    };
    Ok(NetmapResult {
        map,
        coverage,
        ihat,
        valid,
        evaluation,
        p_perm,
        null95,
        model,
        scheme: *scheme,
        notes,
    })
}

fn gzip_nii(vol: &NiiVolume) -> AnalysisResult<Vec<u8>> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&encode_nii(vol))
        .map_err(|e| usage(e.to_string()))?;
    enc.finish().map_err(|e| usage(e.to_string()))
}

/// The map file name of `specs/14` §9.5.
pub fn map_file_name(dataset_name: &str, guid: &str, id: &str, kind: MapKind) -> String {
    format!(
        "{}_netmap-{id}_stat-{}_map.nii.gz",
        file_stem(dataset_name, guid),
        kind.name()
    )
}

/// The `netmap.json` record.
pub fn netmap_json(result: &NetmapResult, provenance: &Value) -> Value {
    let patients: Vec<Value> = result
        .valid
        .iter()
        .map(|j| json!({"patient": j, "similarity": result.ihat[*j][0]}))
        .collect();
    json!({
        "schema": "dbsloc-netmap/1",
        "software": version_string(),
        "created_at": utc_now_rfc3339(),
        "settings": result.model.settings,
        "results": {
            "scheme": result.scheme,
            "n": result.evaluation.n,
            "r": result.evaluation.r,
            "p": result.evaluation.p,
            "auc": result.evaluation.auc,
            "p_perm": result.p_perm,
            "null_r95": result.null95,
            "predictions": patients,
        },
        "model": result.model,
        "notes": result.notes.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "provenance": provenance,
    })
}

/// Writes `netmap.json` and the map under `<group folder>/networkmapping/<id>/`
/// (`specs/14` §9.5). Nothing is committed when `cancel` is set.
#[allow(clippy::too_many_arguments)]
pub fn write_netmap_outputs(
    group_folder: &Path,
    dataset_name: &str,
    guid: &str,
    result: &mut NetmapResult,
    dims: [usize; 3],
    affine: &Affine,
    provenance: &Value,
    cancel: &AtomicBool,
) -> AnalysisResult<AnalysisOutputs> {
    let (id, dir) = create_analysis_dir(&group_folder.join("networkmapping"), guid)?;
    let name = map_file_name(dataset_name, guid, &id, result.model.settings.map);
    result.model.map_file.clone_from(&name);
    let cleanup = |staged: &[(PathBuf, PathBuf)], dir: &Path| {
        for (tmp, _) in staged {
            let _ = std::fs::remove_file(tmp);
        }
        let _ = std::fs::remove_dir(dir);
    };
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let vol = NiiVolume {
        dims,
        affine: *affine,
        data: result.map.vals.clone(),
    };
    let bytes = match gzip_nii(&vol) {
        Ok(b) => b,
        Err(e) => {
            cleanup(&staged, &dir);
            return Err(e);
        }
    };
    let target = dir.join(&name);
    let tmp = partial_of(&target);
    if let Err(e) = std::fs::write(&tmp, bytes) {
        cleanup(&staged, &dir);
        return Err(usage(e.to_string()));
    }
    staged.push((tmp, target));
    let text = serde_json::to_string_pretty(&netmap_json(result, provenance))
        .map(|t| t + "\n")
        .map_err(|e| usage(e.to_string()))?;
    let target = dir.join("netmap.json");
    let tmp = partial_of(&target);
    if let Err(e) = std::fs::write(&tmp, text) {
        cleanup(&staged, &dir);
        return Err(usage(e.to_string()));
    }
    staged.push((tmp, target));
    let files = commit_staged(&dir, &staged, cancel)?;
    Ok(AnalysisOutputs { id, dir, files })
}

/// Options of [`run_netmap`].
#[derive(Clone, Debug)]
pub struct NetmapRunOptions {
    pub variable: String,
    pub covariates: Vec<String>,
    pub connectome: String,
    pub connectome_root: Option<PathBuf>,
    pub space: String,
    pub model: Option<String>,
    pub settings: NetmapSettings,
    pub mask: MaskChoice,
    pub atlases_dir: Option<PathBuf>,
    pub scheme: CvScheme,
    pub seed: u64,
    pub mirror: bool,
    pub group_folder: PathBuf,
}

/// What [`run_netmap`] did.
#[derive(Clone, Debug)]
pub struct NetmapRun {
    pub outputs: AnalysisOutputs,
    pub result: NetmapResult,
    pub notes: Vec<Note>,
}

/// The seed kind, file-name token and suffix of the per-patient seed maps of `conn`.
pub fn seed_map_naming(conn: &ResolvedConnectome) -> (&'static str, String, &'static str) {
    if conn.class == ConnectomeClass::Fmri {
        (
            "fMRI",
            format!(
                "_conn-{}",
                functional_label(&conn.tag, conn.subset.as_deref())
            ),
            "_desc-AvgRFz_funcmap",
        )
    } else {
        ("dMRI", format!("_conn-{}", conn.tag), "_strucmap")
    }
}

/// The plain and the flipped seed map of subject `id` in `dir`.
pub fn find_seed_maps(
    dir: &Path,
    id: &str,
    seed_kind: &str,
    token: &str,
    suffix: &str,
    model: Option<&str>,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (None, None);
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let want = format!("sub-{id}_");
    let seed = format!("_seed-{seed_kind}");
    let (mut plain, mut flipped) = (None, None);
    for n in names {
        let ext_ok =
            n.ends_with(&format!("{suffix}.nii")) || n.ends_with(&format!("{suffix}.nii.gz"));
        if !n.starts_with(&want)
            || !n.contains(&seed)
            || !n.contains(token)
            || !ext_ok
            || model.is_some_and(|m| !n.contains(&format!("_model-{m}_")))
        {
            continue;
        }
        let slot = if n.contains("hemidesc-flipped") {
            &mut flipped
        } else {
            &mut plain
        };
        if slot.is_none() {
            *slot = Some(dir.join(&n));
        }
    }
    (plain, flipped)
}

fn same_grid(a: &Affine, b: &Affine) -> bool {
    (0..4).all(|r| (0..4).all(|c| (a[r][c] - b[r][c]).abs() < 1e-4))
}

/// Resolves the mask of the similarity prediction onto the map grid (`specs/14` §9.3). The
/// grey-matter mask is the `gm_mask.nii.gz` of the group's atlas set (or the first set that has
/// one); the brain mask is the `brainmask.nii.gz` beside the template.
pub fn resolve_mask(
    choice: &MaskChoice,
    atlas_set: Option<&str>,
    atlases_dir: Option<&Path>,
    dims: [usize; 3],
    affine: &Affine,
) -> AnalysisResult<(Vec<bool>, String)> {
    let (path, name) = match choice {
        MaskChoice::All => return Ok((vec![true; dims[0] * dims[1] * dims[2]], "all".into())),
        MaskChoice::File(p) => (p.clone(), p.display().to_string()),
        MaskChoice::Grey => {
            let sets =
                crate::atlas_catalog::bundled_and_external_atlas_sets(atlases_dir, &mut |_| {});
            let has_gm = |d: &Path| d.join("gm_mask.nii.gz").is_file();
            let chosen = sets
                .iter()
                .filter(|s| has_gm(&s.dir))
                .find(|s| atlas_set.is_some_and(|a| s.id == a || s.name == a))
                .or_else(|| sets.iter().find(|s| has_gm(&s.dir)))
                .ok_or_else(|| {
                    AnalysisError::Map(MapError::Missing(
                        "no atlas set has a gm_mask.nii.gz; pass a mask file".into(),
                    ))
                })?;
            (chosen.dir.join("gm_mask.nii.gz"), "grey".to_owned())
        }
        MaskChoice::Brain => {
            let t = crate::templates::select_template(None).ok_or_else(|| {
                AnalysisError::Map(MapError::Missing(
                    "no template found for the brain mask".into(),
                ))
            })?;
            let p = t
                .path
                .parent()
                .map(|d| d.join("brainmask.nii.gz"))
                .unwrap_or_default();
            if !p.is_file() {
                return Err(AnalysisError::Map(MapError::Missing(format!(
                    "no brain mask at {}",
                    p.display()
                ))));
            }
            (p, "brain".to_owned())
        }
    };
    let vol = read_nii(&path)?;
    let vol = if vol.dims == dims && same_grid(&vol.affine, affine) {
        vol
    } else {
        resample(&vol, dims, affine, Interp::Nearest)?
    };
    Ok((
        vol.data.iter().map(|v| v.is_finite() && *v > 0.5).collect(),
        name,
    ))
}

/// Reads the patients' seed maps, computes the analysis and writes the outputs (`specs/14` §9).
pub fn run_netmap(
    record: &GroupRecord,
    opts: &NetmapRunOptions,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> AnalysisResult<NetmapRun> {
    let idx = included_patients(record);
    let (sides, per_hemi) = clinical_outcome(record, &opts.variable, &idx)?;
    let outcome: Vec<f64> = (0..idx.len())
        .map(|a| {
            let v: Vec<f64> = if per_hemi {
                vec![sides[0][a], sides[1][a]]
                    .into_iter()
                    .filter(|x| x.is_finite())
                    .collect()
            } else {
                vec![sides[0][a]]
                    .into_iter()
                    .filter(|x| x.is_finite())
                    .collect()
            };
            mean(&v)
        })
        .collect();
    let covariates = opts
        .covariates
        .iter()
        .map(|c| clinical_covariate(record, c, &idx))
        .collect::<AnalysisResult<Vec<_>>>()?;
    let roots = ConnectomeRoots::resolve(opts.connectome_root.as_deref())
        .map_err(|e| AnalysisError::Map(MapError::Usage(e.to_string())))?;
    let conn = resolve_connectome(&roots, &opts.connectome)?;
    let (seed_kind, token, suffix) = seed_map_naming(&conn);
    let label = record.stim_label();
    let mut notes: Vec<Note> = conn
        .warnings
        .iter()
        .map(|w| Note::plain(w.clone()))
        .collect();
    let mut rows: Vec<Vec<f32>> = Vec::new();
    let mut row_patient = Vec::new();
    let mut flipped = Vec::new();
    let mut inputs: Vec<Value> = Vec::new();
    let mut grid: Option<([usize; 3], Affine)> = None;
    let mut outcome = outcome;
    for (a, pi) in idx.iter().enumerate() {
        cancelled(cancel)?;
        report(
            progress,
            "netmap",
            a as u64,
            idx.len() as u64,
            "reading seed maps",
        );
        let p = &record.patients[*pi];
        let id = subject_id(&p.subject);
        let found = record.derivatives_root_of(p).map(|root| {
            let dir = stimulation_folder(Path::new(root), id, &opts.space, &label);
            find_seed_maps(&dir, id, seed_kind, &token, suffix, opts.model.as_deref())
        });
        let Some((Some(plain), flip)) = found else {
            notes.push(Note::plain(format!("{}: no seed map, left out", p.subject)));
            outcome[a] = f64::NAN;
            continue;
        };
        let mut files = vec![(plain, false)];
        if opts.mirror {
            match flip {
                Some(f) => files.push((f, true)),
                None => notes.push(Note::plain(format!(
                    "{}: no flipped seed map; mirroring skipped for it",
                    p.subject
                ))),
            }
        }
        for (file, is_flipped) in files {
            let vol = read_nii(&file)?;
            match &grid {
                None => grid = Some((vol.dims, vol.affine)),
                Some((d, af)) => {
                    if *d != vol.dims || !same_grid(af, &vol.affine) {
                        return Err(AnalysisError::Map(MapError::Space(format!(
                            "{} is not on the grid of the other seed maps; the group step does not resample",
                            file.display()
                        ))));
                    }
                }
            }
            let sha = file_sha256_hex(&file).map_err(|e| usage(e.to_string()))?;
            inputs.push(json!({"file": file.display().to_string(), "sha256": sha}));
            rows.push(vol.data);
            row_patient.push(a);
            flipped.push(is_flipped);
        }
    }
    let Some((dims, affine)) = grid else {
        return Err(lg("E_GROUP_TOO_FEW", "no seed map of any patient"));
    };
    let (mask, mask_name) = resolve_mask(
        &opts.mask,
        record.atlas_set.as_deref(),
        opts.atlases_dir.as_deref(),
        dims,
        &affine,
    )?;
    let data = NetmapData {
        dims,
        affine,
        rows,
        row_patient,
        flipped,
        n_patients: idx.len(),
        outcome,
        covariates,
        group_of: idx.iter().map(|i| record.patients[*i].group).collect(),
        mask,
    };
    let mut result = compute_netmap(
        &data,
        &opts.settings,
        &opts.scheme,
        &conn.id,
        &mask_name,
        opts.seed,
        cancel,
        progress,
    )?;
    notes.extend(result.notes.iter().cloned());
    let provenance = json!({
        "group_guid": record.guid,
        "variable": opts.variable,
        "covariates": opts.covariates,
        "connectome": {"id": conn.id, "name": conn.name, "tag": conn.tag, "digest": conn.digest},
        "space": opts.space,
        "mask": mask_name,
        "seed": opts.seed,
        "mirror": opts.mirror,
        "patients": idx.iter().map(|i| record.patients[*i].subject.clone()).collect::<Vec<_>>(),
        "inputs": inputs,
    });
    let outputs = write_netmap_outputs(
        &opts.group_folder,
        &record.dataset_name,
        &record.guid,
        &mut result,
        dims,
        &affine,
        &provenance,
        cancel,
    )?;
    Ok(NetmapRun {
        outputs,
        result,
        notes,
    })
}
