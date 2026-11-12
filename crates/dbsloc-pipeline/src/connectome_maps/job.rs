// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The connectivity job of `specs/15` §9: per-subject structural maps, functional maps and
//! per-fibre values, written atomically under `stimulations/<space>/<label>/` with provenance
//! sidecars. Not a pipeline stage: no stage lock.

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use dbsloc_connectome::{detect_kind, ConnectomeKind, FunctionalConnectome};
use serde_json::{json, Value};

use super::fibervals::{
    fibre_values_from, write_fibervals_mat, write_subset_mat_with_source, FibervalsMeta,
    BINARY_THRESHOLD,
};
use super::functional::{
    functional_maps, output_mask_positions, prepare_functional_seed, seed_weights, to_volume,
    FunctionalOptions,
};
use super::nii::{encode_nii, read_nii, NiiVolume};
use super::source::{Access, FibreSource, INDEX_CELL_MM};
use super::structural::{
    build_seed, seed_search_box, split_hemispheres, structural_map, OutputGrid, SeedKind,
};
use super::{check_cancel, report, MapError, MapResult, ProgressFn};
use crate::connectome_catalog::{lookup, ConnectomeClass, REFERENCE_SPACE};
use crate::connectome_fetch::{
    discover_connectomes, infer_class, read_dataset_info, ConnectomeRoots, RootKind,
};
use crate::outputs::partial_of;
use crate::sha256::{file_sha256_hex, sha256_hex, Sha256};
use crate::version::{utc_now_rfc3339, version_string};
use dbsloc_connectome::{FibreIndex, FibreSet};

const FULL_SET: &str = "Full Set";
const PROBE_BYTES: u64 = 16 * 1024 * 1024;

/// A hemisphere of a stimulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    Right,
    Left,
}

impl Side {
    /// `R` or `L`, as in the `hemi-` entity.
    pub fn letter(self) -> &'static str {
        match self {
            Side::Right => "R",
            Side::Left => "L",
        }
    }
}

/// A connectome located in a root, with the facts the sidecars record.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedConnectome {
    pub name: String,
    pub id: String,
    pub class: ConnectomeClass,
    pub path: PathBuf,
    pub root_kind: RootKind,
    pub tag: String,
    pub subset: Option<String>,
    pub space: String,
    pub version: Option<String>,
    pub licence_record: Option<PathBuf>,
    pub citation: Value,
    pub acknowledgement: Option<String>,
    pub digest: String,
    pub warnings: Vec<String>,
}

fn alnum(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// The functional `<label>` of `specs/15` §9.2: tag, `x`, subset name reduced to letters and digits.
pub fn functional_label(tag: &str, subset: Option<&str>) -> String {
    format!("{tag}x{}", alnum(subset.unwrap_or(FULL_SET)))
}

fn probe_file(path: &Path, hasher: &mut Sha256) -> std::io::Result<()> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    hasher.update(&size.to_le_bytes());
    let mut buf = vec![0u8; 1 << 20];
    let mut copy = |f: &mut std::fs::File, mut left: u64| -> std::io::Result<()> {
        while left > 0 {
            let want = buf.len().min(left as usize);
            let got = f.read(&mut buf[..want])?;
            if got == 0 {
                break;
            }
            hasher.update(&buf[..got]);
            left -= got as u64;
        }
        Ok(())
    };
    copy(&mut f, PROBE_BYTES.min(size))?;
    f.seek(SeekFrom::Start(size.saturating_sub(PROBE_BYTES)))?;
    copy(&mut f, PROBE_BYTES.min(size))
}

/// The connectome digest: SHA-256 of (size as 8 bytes LE, first 16 MiB, last 16 MiB) of the main
/// file (`data.mat` / `data.trk[.gz]`, `dataset_volsurf.mat`); for a multi-tract folder the
/// SHA-256 of the per-file digests in name order.
pub fn connectome_digest(folder: &Path, class: ConnectomeClass) -> MapResult<String> {
    let io = |p: &Path, e: std::io::Error| MapError::Io(format!("{}: {e}", p.display()));
    let one = |p: &Path| -> MapResult<String> {
        let mut h = Sha256::new();
        probe_file(p, &mut h).map_err(|e| io(p, e))?;
        Ok(crate::sha256::hex(&h.finish()))
    };
    match class {
        ConnectomeClass::Fmri => one(&folder.join("dataset_volsurf.mat")),
        ConnectomeClass::Dmri => ["data.mat", "data.trk", "data.trk.gz"]
            .iter()
            .map(|n| folder.join(n))
            .find(|p| p.is_file())
            .map_or_else(
                || {
                    Err(MapError::Missing(format!(
                        "no fibre file in {}",
                        folder.display()
                    )))
                },
                |p| one(&p),
            ),
        ConnectomeClass::DmriMultiTract => {
            let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
                .map_err(|e| io(folder, e))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mat"))
                        && !p.file_name().is_some_and(|n| {
                            n.to_string_lossy()
                                .to_ascii_lowercase()
                                .ends_with("_adj.mat")
                        })
                })
                .collect();
            files.sort();
            let mut all = String::new();
            for f in &files {
                all.push_str(&one(f)?);
            }
            Ok(sha256_hex(all.as_bytes()))
        }
    }
}

/// Resolves `spec` (`<id|name|folder>` with an optional ` > <subset>`) to a connectome.
pub fn resolve_connectome(roots: &ConnectomeRoots, spec: &str) -> MapResult<ResolvedConnectome> {
    let (name, subset) = match spec.split_once('>') {
        Some((a, b)) => (
            a.trim(),
            Some(b.trim().to_owned()).filter(|s| !s.is_empty()),
        ),
        None => (spec.trim(), None),
    };
    if name.is_empty() {
        return Err(MapError::Usage("a connectome name is needed".into()));
    }
    let mut warnings = Vec::new();
    let as_path = Path::new(name);
    let (display, id, class, path, root_kind, tag, space, version, licence, catalogue_id) =
        if as_path.is_dir() {
            let class = infer_class(as_path)
                .or_else(|| {
                    (detect_kind(as_path) == Some(ConnectomeKind::MultiTract))
                        .then_some(ConnectomeClass::DmriMultiTract)
                })
                .ok_or_else(|| MapError::Missing(format!("{name} is not a connectome folder")))?;
            let info = read_dataset_info(as_path);
            let tag = info
                .as_ref()
                .and_then(|v| v["tag"].as_str())
                .map(str::to_owned);
            let folder = as_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (
                folder.clone(),
                crate::connectome_fetch::sanitize_id(&folder),
                class,
                as_path.to_path_buf(),
                RootKind::Registered,
                tag,
                REFERENCE_SPACE.to_owned(),
                None,
                None,
                None,
            )
        } else {
            let found = discover_connectomes(roots).into_iter().find(|c| {
                !c.missing
                    && c.duplicate_of.is_none()
                    && (c.id.eq_ignore_ascii_case(name)
                        || c.name.eq_ignore_ascii_case(name)
                        || c.tag
                            .as_deref()
                            .is_some_and(|t| t.eq_ignore_ascii_case(name)))
            });
            let c = found.ok_or_else(|| {
                MapError::Missing(format!(
                    "connectome {name} not found in {}{}",
                    roots.primary.display(),
                    roots
                        .external
                        .as_ref()
                        .map(|e| format!(" or {}", e.display()))
                        .unwrap_or_default()
                ))
            })?;
            (
                c.name,
                c.id,
                c.class,
                c.path,
                c.root_kind,
                c.tag,
                c.space,
                c.version,
                c.licence_record,
                c.catalogue_id,
            )
        };
    let tag = match tag.filter(|t| !t.is_empty()) {
        Some(t) => t,
        None => {
            warnings.push(format!(
                "{display} has no dataset_info.json tag; the folder name is used"
            ));
            alnum(&display)
        }
    };
    let entry = catalogue_id.and_then(lookup);
    let info = read_dataset_info(&path);
    let citation = match entry {
        Some(e) => Value::String(e.citation.to_owned()),
        None => info.as_ref().map_or(Value::Null, |v| v["citation"].clone()),
    };
    let digest = connectome_digest(&path, class)?;
    Ok(ResolvedConnectome {
        name: display,
        id,
        class,
        path,
        root_kind,
        tag,
        subset,
        space,
        version,
        licence_record: licence,
        citation,
        acknowledgement: entry.map(|e| e.acknowledgement.to_owned()),
        digest,
        warnings,
    })
}

fn connectome_json(c: &ResolvedConnectome) -> Value {
    json!({
        "id": c.id,
        "name": c.name,
        "tag": c.tag,
        "subset": c.subset,
        "class": c.class.dir_name(),
        "version": c.version,
        "digest": c.digest,
        "root_kind": c.root_kind.label(),
        "licence_record": c.licence_record.as_ref().map(|p| p.display().to_string()),
        "citation": c.citation,
        "acknowledgement": c.acknowledgement,
    })
}

/// A set of files written under `.partial-` names and renamed together; dropped without
/// `commit` it removes what it staged.
struct StagedSet {
    pairs: Vec<(PathBuf, PathBuf)>,
    done: bool,
}

impl StagedSet {
    fn new() -> Self {
        Self {
            pairs: Vec::new(),
            done: false,
        }
    }

    fn add(&mut self, target: &Path) -> MapResult<PathBuf> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MapError::Io(format!("{}: {e}", parent.display())))?;
        }
        let tmp = partial_of(target);
        let _ = std::fs::remove_file(&tmp);
        self.pairs.push((tmp.clone(), target.to_path_buf()));
        Ok(tmp)
    }

    fn files(&self) -> Vec<PathBuf> {
        self.pairs.iter().map(|(_, t)| t.clone()).collect()
    }

    fn commit(mut self) -> MapResult<Vec<PathBuf>> {
        for (tmp, target) in &self.pairs {
            let _ = std::fs::remove_file(target);
            std::fs::rename(tmp, target)
                .map_err(|e| MapError::Io(format!("{}: {e}", target.display())))?;
        }
        self.done = true;
        Ok(self.files())
    }
}

impl Drop for StagedSet {
    fn drop(&mut self) {
        if !self.done {
            for (tmp, _) in &self.pairs {
                let _ = std::fs::remove_file(tmp);
            }
        }
    }
}

fn sidecar_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = ["nii.gz", "nii", "mat"]
        .iter()
        .find_map(|e| name.strip_suffix(&format!(".{e}")))
        .unwrap_or(&name)
        .to_owned();
    target.with_file_name(format!("{stem}.json"))
}

fn write_bytes(path: &Path, bytes: &[u8]) -> MapResult<()> {
    std::fs::write(path, bytes).map_err(|e| MapError::Io(format!("{}: {e}", path.display())))
}

fn stage_nii(set: &mut StagedSet, target: &Path, vol: &NiiVolume) -> MapResult<String> {
    let bytes = encode_nii(vol);
    let sha = sha256_hex(&bytes);
    write_bytes(&set.add(target)?, &bytes)?;
    Ok(sha)
}

fn stage_sidecar(set: &mut StagedSet, target: &Path, value: &Value) -> MapResult<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| MapError::Io(e.to_string()))?;
    write_bytes(&set.add(&sidecar_path(target))?, text.as_bytes())
}

fn sidecar(
    conn: &ResolvedConnectome,
    seed: Value,
    params: Value,
    inputs: &[(String, String)],
) -> Value {
    json!({
        "software": version_string(),
        "created_at": utc_now_rfc3339(),
        "connectome": connectome_json(conn),
        "seed": seed,
        "params": params,
        "inputs": inputs
            .iter()
            .map(|(f, s)| json!({"file": f, "sha256": s}))
            .collect::<Vec<_>>(),
    })
}

/// The folder `<derivatives root>/sub-<id>/stimulations/<space>/<label>/`.
pub fn stimulation_folder(root: &Path, subject_id: &str, space: &str, label: &str) -> PathBuf {
    root.join(format!("sub-{subject_id}"))
        .join("stimulations")
        .join(space)
        .join(label)
}

/// The per-side images of a stimulation found in a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct SideFiles {
    pub model: String,
    pub files: Vec<(Side, PathBuf)>,
}

/// Finds `sub-<id>_sim-<kind>_model-<model>_hemi-<R|L>.nii[.gz]` in `dir`. `model` selects one
/// model; without it exactly one must be present.
pub fn find_side_files(
    dir: &Path,
    subject_id: &str,
    kind: SeedKind,
    model: Option<&str>,
) -> MapResult<SideFiles> {
    let prefix = format!("sub-{subject_id}_sim-{}_model-", kind.name());
    let entries = std::fs::read_dir(dir)
        .map_err(|_| MapError::SeedEmpty(format!("no stimulation folder {}", dir.display())))?;
    let mut found: Vec<(String, Side, PathBuf, bool)> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        for (side, tail) in [(Side::Right, "_hemi-R"), (Side::Left, "_hemi-L")] {
            for (ext, gz) in [(".nii", false), (".nii.gz", true)] {
                if let Some(m) = rest.strip_suffix(&format!("{tail}{ext}")) {
                    found.push((m.to_owned(), side, e.path(), gz));
                }
            }
        }
    }
    let models: BTreeSet<&String> = found.iter().map(|f| &f.0).collect();
    let chosen = match model {
        Some(m) => m.to_owned(),
        None => match models.len() {
            0 => String::new(),
            1 => models
                .iter()
                .next()
                .map(|m| (*m).clone())
                .unwrap_or_default(),
            _ => {
                return Err(MapError::Usage(format!(
                    "several stimulation models in {}: pass --model ({})",
                    dir.display(),
                    models
                        .iter()
                        .map(|m| m.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
        },
    };
    let mut files: Vec<(Side, PathBuf)> = Vec::new();
    for side in [Side::Right, Side::Left] {
        let mut candidates: Vec<&(String, Side, PathBuf, bool)> = found
            .iter()
            .filter(|f| f.0 == chosen && f.1 == side)
            .collect();
        candidates.sort_by_key(|f| f.3);
        if let Some(c) = candidates.first() {
            files.push((side, c.2.clone()));
        }
    }
    if files.is_empty() {
        return Err(MapError::SeedEmpty(format!(
            "no {} stimulation image for subject {subject_id} in {}",
            kind.name(),
            dir.display()
        )));
    }
    Ok(SideFiles {
        model: chosen,
        files,
    })
}

fn seed_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    ["nii.gz", "nii"]
        .iter()
        .find_map(|e| name.strip_suffix(&format!(".{e}")))
        .unwrap_or(&name)
        .to_owned()
}

/// Result of one subject: written files, warnings, and why nothing was written when skipped.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub files: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub skipped: Option<String>,
}

/// Status of a subject in a summary (`specs/15` §9.1).
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Ok,
    Skipped(String),
    Error { code: String, message: String },
}

/// One row of the JSON summary.
#[derive(Clone, Debug, PartialEq)]
pub struct SubjectOutcome {
    pub subject_id: String,
    pub status: Status,
    pub files: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

impl SubjectOutcome {
    /// Wraps a job result; a cancel is returned as `Err` for the caller to stop on.
    pub fn from_result(
        subject_id: &str,
        r: MapResult<Outcome>,
    ) -> Result<SubjectOutcome, MapError> {
        match r {
            Ok(o) => Ok(SubjectOutcome {
                subject_id: subject_id.to_owned(),
                status: o.skipped.map_or(Status::Ok, Status::Skipped),
                files: o.files,
                warnings: o.warnings,
            }),
            Err(MapError::Cancelled) => Err(MapError::Cancelled),
            Err(e) => Ok(SubjectOutcome {
                subject_id: subject_id.to_owned(),
                status: Status::Error {
                    code: e.code().to_owned(),
                    message: e.message(),
                },
                files: Vec::new(),
                warnings: Vec::new(),
            }),
        }
    }

    pub fn status_text(&self) -> String {
        match &self.status {
            Status::Ok => "ok".to_owned(),
            Status::Skipped(r) => format!("skipped: {r}"),
            Status::Error { code, message } => format!("error: {code} {message}"),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "subject": self.subject_id,
            "status": self.status_text(),
            "files": self.files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

fn check_space(conn: &ResolvedConnectome, space: &str) -> MapResult<()> {
    if conn.space == space {
        Ok(())
    } else {
        Err(MapError::Space(format!(
            "the stimulation is in {space}, connectome {} is in {}; the template bridge is not applied by this job",
            conn.name, conn.space
        )))
    }
}

pub use super::source::{load_fibres, load_fibres_with, LoadOptions, StructuralEngine};

/// A structural map job for one subject and stimulation.
#[derive(Clone, Debug)]
pub struct StructuralRequest {
    pub derivatives_root: PathBuf,
    pub subject_id: String,
    pub label: String,
    pub space: String,
    pub connectome: ResolvedConnectome,
    pub kind: SeedKind,
    pub model: Option<String>,
    pub grid: OutputGrid,
    pub seed_file: Option<PathBuf>,
    pub write_subset: bool,
}

/// Runs a structural seed map (`specs/15` §5, §9). Nothing is committed unless the job completes.
pub fn run_structural(
    engine: &StructuralEngine,
    req: &StructuralRequest,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> MapResult<Outcome> {
    check_space(&req.connectome, &req.space)?;
    check_cancel(cancel)?;
    let dir = stimulation_folder(
        &req.derivatives_root,
        &req.subject_id,
        &req.space,
        &req.label,
    );
    let mut staged = StagedSet::new();
    let mut warnings = req.connectome.warnings.clone();
    let tag = &req.connectome.tag;
    let (seed, seed_name, base, inputs, map_dir) = match &req.seed_file {
        Some(file) => {
            let seed = read_nii(file)?;
            let sha = file_sha256_hex(file).map_err(|e| MapError::Io(e.to_string()))?;
            let stem = seed_stem(file);
            let parent = file
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            (
                seed,
                file.display().to_string(),
                stem,
                vec![(file.display().to_string(), sha)],
                parent,
            )
        }
        None => {
            let sides = find_side_files(&dir, &req.subject_id, req.kind, req.model.as_deref())?;
            let mut volumes = Vec::new();
            let mut inputs = Vec::new();
            for (_, f) in &sides.files {
                volumes.push(read_nii(f)?);
                inputs.push((
                    f.display().to_string(),
                    file_sha256_hex(f).map_err(|e| MapError::Io(e.to_string()))?,
                ));
            }
            let refs: Vec<&NiiVolume> = volumes.iter().collect();
            let seed = build_seed(&refs, req.kind)?;
            let base = format!(
                "sub-{}_sim-{}_model-{}",
                req.subject_id,
                req.kind.name(),
                sides.model
            );
            let seed_path = dir.join(format!("{base}_seed-dMRI.nii"));
            let sha = stage_nii(&mut staged, &seed_path, &seed)?;
            let params = json!({"stage": "seed", "seed_from": req.kind.name()});
            let seed_json = json!({
                "file": seed_path.display().to_string(),
                "sha256": sha,
                "kind": req.kind.name(),
                "radius_mm": super::structural::search_radius(&seed.affine),
                "grid": "seed grid 0.22 mm",
            });
            stage_sidecar(
                &mut staged,
                &seed_path,
                &sidecar(&req.connectome, seed_json, params, &inputs),
            )?;
            let (right, left) = split_hemispheres(&seed);
            for (vol, suffix) in [(&right, "R"), (&left, "L")] {
                let p = dir.join(format!("{base}_seed-dMRI_hemi-{suffix}.nii"));
                let s = stage_nii(&mut staged, &p, vol)?;
                let seed_json = json!({
                    "file": p.display().to_string(), "sha256": s, "kind": req.kind.name(),
                    "radius_mm": super::structural::search_radius(&vol.affine),
                    "grid": "seed grid 0.22 mm",
                });
                stage_sidecar(
                    &mut staged,
                    &p,
                    &sidecar(
                        &req.connectome,
                        seed_json,
                        json!({"stage": "seed", "hemisphere": suffix}),
                        &inputs,
                    ),
                )?;
            }
            (
                seed,
                seed_path.display().to_string(),
                format!("{base}_seed-dMRI"),
                vec![(seed_path.display().to_string(), sha)],
                dir.clone(),
            )
        }
    };
    check_cancel(cancel)?;
    let (dims, affine) = req.grid.spec();
    let region_holder;
    let (set, index, source_ids): (&FibreSet, &FibreIndex, Option<&[u32]>) = match &engine.access {
        Access::Loaded { set, index } => (&**set, &**index, None),
        Access::Cached(handle) => {
            let (lo, hi) = seed_search_box(&seed).unwrap_or(([0.0; 3], [-1.0; 3]));
            let (sub, ids) = handle.region(lo, hi)?;
            let idx = FibreIndex::build(&sub, INDEX_CELL_MM)?;
            region_holder = (sub, idx, ids);
            (
                &region_holder.0,
                &region_holder.1,
                Some(region_holder.2.as_slice()),
            )
        }
    };
    let result = structural_map(set, index, &seed, dims, &affine, cancel, progress)?;
    let Some(result) = result else {
        let files = staged.commit()?;
        warnings.push(format!("No connected fibres for seed {seed_name}"));
        return Ok(Outcome {
            files,
            warnings,
            skipped: Some(format!("no connected fibres for seed {seed_name}")),
        });
    };
    let seed_sha = inputs.first().map(|i| i.1.clone()).unwrap_or_default();
    let seed_json = json!({
        "file": seed_name,
        "sha256": seed_sha,
        "kind": req.kind.name(),
        "radius_mm": result.selection.radius_mm,
        "grid": req.grid.name(),
    });
    let params = json!({
        "seed_from": req.kind.name(),
        "grid": req.grid.name(),
        "connected_fibres": result.selection.fibre_ids.len(),
        "seed_voxels": result.selection.seed_voxels,
        "total_fibres": engine.n_fibres(),
    });
    let map_path = map_dir.join(format!("{base}_conn-{tag}_strucmap.nii"));
    stage_nii(&mut staged, &map_path, &result.map)?;
    stage_sidecar(
        &mut staged,
        &map_path,
        &sidecar(&req.connectome, seed_json.clone(), params.clone(), &inputs),
    )?;
    if req.write_subset {
        let subset_path = map_dir.join(format!("{base}_conn-{tag}_fibers.mat"));
        let source: Vec<u32> = match source_ids {
            Some(ids) => result
                .selection
                .fibre_ids
                .iter()
                .map(|f| ids[*f as usize])
                .collect(),
            None => result.selection.fibre_ids.clone(),
        };
        match write_subset_mat_with_source(
            &staged.add(&subset_path)?,
            set,
            &result.selection.fibre_ids,
            &source,
            &result.selection.strengths,
            tag,
        ) {
            Ok(()) => stage_sidecar(
                &mut staged,
                &subset_path,
                &sidecar(&req.connectome, seed_json, params, &inputs),
            )?,
            Err(e @ MapError::SubsetTooLarge(_)) => {
                staged.pairs.retain(|(_, t)| t != &subset_path);
                warnings.push(e.to_string());
            }
            Err(e) => return Err(e),
        }
    }
    check_cancel(cancel)?;
    let files = staged.commit()?;
    Ok(Outcome {
        files,
        warnings,
        skipped: None,
    })
}

/// A functional map job for one subject and stimulation.
#[derive(Clone, Debug)]
pub struct FunctionalRequest {
    pub derivatives_root: PathBuf,
    pub subject_id: String,
    pub label: String,
    pub space: String,
    pub connectome: ResolvedConnectome,
    pub kind: SeedKind,
    pub model: Option<String>,
    pub seed_file: Option<PathBuf>,
    pub per_subject_maps: bool,
}

struct Prepared<'a> {
    req: &'a FunctionalRequest,
    staged: StagedSet,
    weights: Vec<f32>,
    base: String,
    out_dir: PathBuf,
    inputs: Vec<(String, String)>,
    seed_json: Value,
    warnings: Vec<String>,
}

fn prepare_functional<'a>(
    conn: &FunctionalConnectome,
    req: &'a FunctionalRequest,
) -> MapResult<Prepared<'a>> {
    check_space(&req.connectome, &req.space)?;
    let dir = stimulation_folder(
        &req.derivatives_root,
        &req.subject_id,
        &req.space,
        &req.label,
    );
    let mut staged = StagedSet::new();
    let mut warnings = req.connectome.warnings.clone();
    let (seed, base, out_dir, inputs, seed_file) = match &req.seed_file {
        Some(file) => {
            let seed = read_nii(file)?;
            let sha = file_sha256_hex(file).map_err(|e| MapError::Io(e.to_string()))?;
            (
                seed,
                seed_stem(file),
                file.parent()
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
                vec![(file.display().to_string(), sha)],
                file.display().to_string(),
            )
        }
        None => {
            let sides = find_side_files(&dir, &req.subject_id, req.kind, req.model.as_deref())?;
            let mut volumes = Vec::new();
            let mut inputs = Vec::new();
            for (_, f) in &sides.files {
                volumes.push(read_nii(f)?);
                inputs.push((
                    f.display().to_string(),
                    file_sha256_hex(f).map_err(|e| MapError::Io(e.to_string()))?,
                ));
            }
            let refs: Vec<&NiiVolume> = volumes.iter().collect();
            let prepared = prepare_functional_seed(&refs, &conn.grid, req.kind)?;
            warnings.extend(prepared.warnings);
            let base = format!(
                "sub-{}_sim-{}_model-{}",
                req.subject_id,
                req.kind.name(),
                sides.model
            );
            let seed_path = dir.join(format!("{base}_seed-fMRI.nii"));
            let sha = stage_nii(&mut staged, &seed_path, &prepared.volume)?;
            let seed_json = json!({
                "file": seed_path.display().to_string(), "sha256": sha,
                "kind": req.kind.name(), "radius_mm": Value::Null, "grid": "connectome grid",
            });
            stage_sidecar(
                &mut staged,
                &seed_path,
                &sidecar(
                    &req.connectome,
                    seed_json,
                    json!({"stage": "seed"}),
                    &inputs,
                ),
            )?;
            let (right, left) = split_hemispheres(&prepared.volume);
            for (vol, suffix) in [(&right, "R"), (&left, "L")] {
                let p = dir.join(format!("{base}_seed-fMRI_hemi-{suffix}.nii"));
                let s = stage_nii(&mut staged, &p, vol)?;
                let seed_json = json!({
                    "file": p.display().to_string(), "sha256": s, "kind": req.kind.name(),
                    "radius_mm": Value::Null, "grid": "connectome grid",
                });
                stage_sidecar(
                    &mut staged,
                    &p,
                    &sidecar(
                        &req.connectome,
                        seed_json,
                        json!({"stage": "seed", "hemisphere": suffix}),
                        &inputs,
                    ),
                )?;
            }
            (
                prepared.volume,
                format!("{base}_seed-fMRI"),
                dir.clone(),
                vec![(seed_path.display().to_string(), sha)],
                seed_path.display().to_string(),
            )
        }
    };
    let weights = seed_weights(&seed, conn)?;
    let seed_json = json!({
        "file": seed_file,
        "sha256": inputs.first().map(|i| i.1.clone()).unwrap_or_default(),
        "kind": req.kind.name(),
        "radius_mm": Value::Null,
        "grid": "connectome grid",
    });
    Ok(Prepared {
        req,
        staged,
        weights,
        base,
        out_dir,
        inputs,
        seed_json,
        warnings,
    })
}

/// Shared settings of one functional pass.
#[derive(Clone, Debug, Default)]
pub struct FunctionalPass {
    pub output_mask: Option<PathBuf>,
}

/// Runs the functional seed maps of several subjects in one pass over the connectome runs
/// (`specs/15` §6.4, §9). All requests must use the same connectome. A subject whose seed cannot
/// be prepared is reported as an error and does not stop the others; a cancel discards everything.
pub fn run_functional_batch(
    conn: &FunctionalConnectome,
    reqs: &[FunctionalRequest],
    pass: &FunctionalPass,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> MapResult<Vec<SubjectOutcome>> {
    let mut outcomes: Vec<Option<SubjectOutcome>> = vec![None; reqs.len()];
    let mut prepared: Vec<(usize, Prepared<'_>)> = Vec::new();
    for (i, req) in reqs.iter().enumerate() {
        check_cancel(cancel)?;
        report(
            progress,
            "seed",
            i as u64,
            reqs.len() as u64,
            &req.subject_id,
        );
        match prepare_functional(conn, req) {
            Ok(p) => prepared.push((i, p)),
            Err(MapError::Cancelled) => return Err(MapError::Cancelled),
            Err(e) => {
                outcomes[i] = Some(SubjectOutcome::from_result(&req.subject_id, Err(e))?);
            }
        }
    }
    if !prepared.is_empty() {
        let subset = reqs.first().and_then(|r| r.connectome.subset.clone());
        let positions = match &pass.output_mask {
            Some(p) => Some(output_mask_positions(&read_nii(p)?, conn)?),
            None => None,
        };
        let options = FunctionalOptions {
            subset,
            output_positions: positions.clone(),
        };
        let seeds: Vec<Vec<f32>> = prepared.iter().map(|(_, p)| p.weights.clone()).collect();
        let mut on_subject = |s: usize, id: &str, r: &[f32]| -> MapResult<()> {
            let p = &mut prepared[s].1;
            if !p.req.per_subject_maps {
                return Ok(());
            }
            let label = functional_label(&p.req.connectome.tag, p.req.connectome.subset.as_deref());
            let safe: String = id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let target = p
                .out_dir
                .join(format!("{}_conn-{label}_id-{safe}_corr.nii", p.base));
            let vol = to_volume(conn, positions.as_deref(), r);
            let bytes = encode_nii(&vol);
            let sha = sha256_hex(&bytes);
            let tmp = p.staged.add(&target)?;
            write_bytes(&tmp, &bytes)?;
            let side = sidecar(
                &p.req.connectome,
                p.seed_json.clone(),
                json!({"map": "corr", "connectome_subject": id, "output_sha256": sha}),
                &p.inputs,
            );
            stage_sidecar(&mut p.staged, &target, &side)
        };
        let stats = functional_maps(conn, &seeds, &options, cancel, progress, &mut on_subject)?;
        for ((i, mut p), st) in prepared.into_iter().zip(stats) {
            let label = functional_label(&p.req.connectome.tag, p.req.connectome.subset.as_deref());
            let params = json!({
                "seed_from": p.req.kind.name(),
                "subset": p.req.connectome.subset.clone().unwrap_or_else(|| FULL_SET.to_owned()),
                "connectome_subjects": st.n_subjects,
                "output_voxels": st.avg_r.len(),
                "output_mask": pass.output_mask.as_ref().map(|m| m.display().to_string()),
            });
            for (desc, values) in [
                ("AvgR", &st.avg_r),
                ("VarR", &st.var_r),
                ("AvgRFz", &st.avg_rfz),
                ("T", &st.t),
            ] {
                let target = p
                    .out_dir
                    .join(format!("{}_conn-{label}_desc-{desc}_funcmap.nii", p.base));
                let vol = to_volume(conn, positions.as_deref(), values);
                stage_nii(&mut p.staged, &target, &vol)?;
                let side = sidecar(
                    &p.req.connectome,
                    p.seed_json.clone(),
                    params.clone(),
                    &p.inputs,
                );
                stage_sidecar(&mut p.staged, &target, &side)?;
            }
            check_cancel(cancel)?;
            let warnings = p.warnings.clone();
            let files = p.staged.commit()?;
            outcomes[i] = Some(SubjectOutcome {
                subject_id: reqs[i].subject_id.clone(),
                status: Status::Ok,
                files,
                warnings,
            });
        }
    }
    Ok(outcomes.into_iter().flatten().collect())
}

/// A per-fibre values job for one subject and stimulation.
#[derive(Clone, Debug)]
pub struct FibervalsRequest {
    pub derivatives_root: PathBuf,
    pub subject_id: String,
    pub label: String,
    pub space: String,
    pub connectome: ResolvedConnectome,
    pub kind: SeedKind,
    pub model: Option<String>,
    pub threshold: f64,
    pub sides: Vec<Side>,
}

/// Runs the per-fibre values of `specs/15` §7 and §9.2 for the requested sides. `Binary` input
/// uses the threshold 0.5 whatever `threshold` says.
pub fn run_fibervals<S: FibreSource + ?Sized>(
    set: &S,
    req: &FibervalsRequest,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> MapResult<Outcome> {
    check_space(&req.connectome, &req.space)?;
    let dir = stimulation_folder(
        &req.derivatives_root,
        &req.subject_id,
        &req.space,
        &req.label,
    );
    let found = find_side_files(&dir, &req.subject_id, req.kind, req.model.as_deref())?;
    let threshold = if req.kind == SeedKind::Binary {
        BINARY_THRESHOLD
    } else {
        req.threshold
    };
    let mut warnings = req.connectome.warnings.clone();
    let mut staged = StagedSet::new();
    let tag = &req.connectome.tag;
    let total = req.sides.len() as u64;
    for (n, side) in req.sides.iter().enumerate() {
        check_cancel(cancel)?;
        report(progress, "fibervals", n as u64, total, side.letter());
        let Some((_, file)) = found.files.iter().find(|(s, _)| s == side) else {
            warnings.push(format!(
                "no {} image for side {}",
                req.kind.name(),
                side.letter()
            ));
            continue;
        };
        let e = read_nii(file)?;
        let sha = file_sha256_hex(file).map_err(|er| MapError::Io(er.to_string()))?;
        let values = fibre_values_from(set, &e, threshold, cancel)?;
        let name = format!(
            "sub-{}_sim-{}_model-{}_hemi-{}_conn-{tag}_fibervals.mat",
            req.subject_id,
            req.kind.name(),
            found.model,
            side.letter()
        );
        let target = dir.join(name);
        let meta = FibervalsMeta {
            connectome_tag: tag.clone(),
            connectome_digest: req.connectome.digest.clone(),
            efield_file: file.display().to_string(),
            efield_sha256: sha.clone(),
        };
        write_fibervals_mat(&staged.add(&target)?, &values, &meta)?;
        let seed_json = json!({
            "file": file.display().to_string(), "sha256": sha, "kind": req.kind.name(),
            "radius_mm": Value::Null, "grid": "stimulation image",
        });
        let params = json!({
            "threshold_vpm": threshold,
            "side": side.letter(),
            "connected_fibres": values.fiber_index.len(),
            "total_fibres": values.total_fibers,
        });
        stage_sidecar(
            &mut staged,
            &target,
            &sidecar(
                &req.connectome,
                seed_json,
                params,
                &[(file.display().to_string(), sha)],
            ),
        )?;
    }
    if staged.pairs.is_empty() {
        return Ok(Outcome {
            files: Vec::new(),
            warnings,
            skipped: Some("no stimulation image for the requested sides".to_owned()),
        });
    }
    check_cancel(cancel)?;
    let files = staged.commit()?;
    Ok(Outcome {
        files,
        warnings,
        skipped: None,
    })
}

#[cfg(test)]
mod tests;
