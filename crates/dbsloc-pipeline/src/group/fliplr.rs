// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The non-linear left-right flip transform of a template space (`specs/14` §4.4, decision D5):
//! the template is mirrored about x = 0 and registered to its mirror with the in-process SyN
//! engine; the forward and inverse composites are cached once per template space.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Instant;

use fastwarp_reg::{Grid, Volume};
use serde_json::{json, Map, Value};

use super::flip::flip_x;
use crate::error::PipelineError;
use crate::norm_native::{register_templates, PairJob};
use crate::protocol::RunEvent;
use crate::reg_backend::{choose_for_run, RegBackend, RegEngine};
use crate::sha256::file_sha256_hex;
use crate::stage_impl::load_reg_volume;
use crate::template_catalog;
use crate::templates::select_template_t2_with;

/// Folder name of the transform beside a template space.
pub const DIR_NAME: &str = "fliplr";
/// Record of what a built transform was made from.
pub const BUILD_RECORD: &str = "build.json";
const STAGE: &str = "fliplr";
const WORK_PREFIX: &str = "flip_";

/// Why the flip transform could not be built.
#[derive(Debug, Clone, thiserror::Error)]
pub enum FlipBuildError {
    #[error("cancelled by user")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
}

fn failed(msg: impl Into<String>) -> FlipBuildError {
    FlipBuildError::Failed(msg.into())
}

/// The forward (template to flipped template) and inverse composite of a space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlipPaths {
    pub forward: PathBuf,
    pub inverse: PathBuf,
}

/// `from-<space>_to-<space>Flipped_mode-image_xfm.h5`.
pub fn forward_name(space: &str) -> String {
    format!("from-{space}_to-{space}Flipped_mode-image_xfm.h5")
}

/// `from-<space>Flipped_to-<space>_mode-image_xfm.h5`.
pub fn inverse_name(space: &str) -> String {
    format!("from-{space}Flipped_to-{space}_mode-image_xfm.h5")
}

/// `<root>/templates/space/<space>`.
pub fn space_dir_in(root: &Path, space: &str) -> PathBuf {
    root.join("templates").join("space").join(space)
}

/// The cache folder of `space` under the user data directory `root`.
pub fn cache_dir_in(root: &Path, space: &str) -> PathBuf {
    space_dir_in(root, space).join(DIR_NAME)
}

/// The two composite paths inside a transform folder.
pub fn paths_in(dir: &Path, space: &str) -> FlipPaths {
    FlipPaths {
        forward: dir.join(forward_name(space)),
        inverse: dir.join(inverse_name(space)),
    }
}

fn complete(p: &FlipPaths) -> bool {
    p.forward.is_file() && p.inverse.is_file()
}

/// The inverse composite that belongs to a forward one, by file name: the `from-…_to-…Flipped`
/// pair or an ANTs `…Composite.h5` / `…InverseComposite.h5` pair.
pub fn inverse_of(forward: &Path) -> Option<PathBuf> {
    let name = forward.file_name()?.to_str()?;
    if let Some((space, tail)) = name
        .strip_prefix("from-")
        .and_then(|r| r.split_once("_to-"))
        .and_then(|(a, b)| Some((a, b.strip_prefix(a)?.strip_prefix("Flipped")?)))
    {
        return Some(forward.with_file_name(format!("from-{space}Flipped_to-{space}{tail}")));
    }
    if let Some(stem) = name.strip_suffix("Composite.h5") {
        if !stem.ends_with("Inverse") {
            return Some(forward.with_file_name(format!("{stem}InverseComposite.h5")));
        }
    }
    None
}

/// The template files a flip transform is built from: the T1w and, when it exists, the T2w.
#[derive(Debug, Clone)]
pub struct TemplateFiles {
    pub space: String,
    pub t1: PathBuf,
    pub t2: Option<PathBuf>,
}

impl TemplateFiles {
    /// A T1w template with the T2w beside it (`specs/04` §3).
    pub fn from_t1(space: &str, t1: &Path) -> TemplateFiles {
        TemplateFiles {
            space: space.to_owned(),
            t1: t1.to_path_buf(),
            t2: select_template_t2_with(None, &HashMap::new(), Some(t1)).map(|r| r.path),
        }
    }

    /// A catalogue id or space (installed under `shipped` / `data`) or the path of a T1w file.
    pub fn resolve(
        spec: &str,
        shipped: Option<&Path>,
        data: Option<&Path>,
    ) -> Result<TemplateFiles, String> {
        if let Some(e) = template_catalog::lookup(spec) {
            return template_catalog::entry_t1_path_in(e, shipped, data)
                .map(|p| TemplateFiles::from_t1(e.space, &p))
                .ok_or_else(|| {
                    format!(
                        "the template {} is not installed; download it first (specs/04 §3.1)",
                        e.id
                    )
                });
        }
        let path = Path::new(spec);
        if path.is_file() {
            return Ok(TemplateFiles::from_t1(
                &template_catalog::infer_space(path),
                path,
            ));
        }
        Err(format!(
            "{spec} is neither a template of the catalogue nor a template file"
        ))
    }

    fn roles(&self) -> Vec<(&'static str, &Path)> {
        let mut v = vec![("T1w", self.t1.as_path())];
        if let Some(t2) = &self.t2 {
            v.push(("T2w", t2.as_path()));
        }
        v
    }

    /// The `templates` array of `build.json`: role, file name and SHA-256 of every template.
    pub fn fingerprint(&self) -> Result<Value, String> {
        let mut out = Vec::new();
        for (role, path) in self.roles() {
            let sha = file_sha256_hex(path).map_err(|e| format!("{}: {e}", path.display()))?;
            out.push(json!({
                "role": role,
                "file": path.file_name().map(|n| n.to_string_lossy().into_owned()),
                "sha256": sha,
            }));
        }
        Ok(Value::Array(out))
    }
}

fn read_record(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(BUILD_RECORD)).ok()?).ok()
}

/// Whether the transform in `dir` was built from `files`. A folder without `build.json` was put
/// there by hand and is accepted; without `files` nothing can be compared.
pub fn record_is_current(dir: &Path, files: Option<&TemplateFiles>) -> bool {
    let Some(record) = read_record(dir) else {
        return !dir.join(BUILD_RECORD).exists();
    };
    let Some(files) = files else {
        return true;
    };
    match files.fingerprint() {
        Ok(now) => record.get("templates") == Some(&now),
        Err(_) => false,
    }
}

/// Folders beside the shipped template that may hold a transform.
pub fn shipped_dirs(space: &str, t1: Option<&Path>, shipped_root: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(dir) = t1.and_then(Path::parent) {
        v.push(dir.join(DIR_NAME));
        if let Some(up) = dir.parent() {
            v.push(up.join(DIR_NAME));
        }
    }
    if let Some(root) = shipped_root {
        v.push(cache_dir_in(&root.join("resources"), space));
    }
    v
}

/// The cached transform of `space` (`specs/14` §4.4): the user data directory when it was built
/// from `files`, then a `fliplr` folder beside the shipped template.
pub fn locate(
    space: &str,
    files: Option<&TemplateFiles>,
    user_root: Option<&Path>,
    shipped: &[PathBuf],
) -> Option<FlipPaths> {
    if let Some(root) = user_root {
        let dir = cache_dir_in(root, space);
        let p = paths_in(&dir, space);
        if complete(&p) && record_is_current(&dir, files) {
            return Some(p);
        }
    }
    shipped.iter().map(|d| paths_in(d, space)).find(complete)
}

/// Where transforms are looked up and which engine builds them.
#[derive(Debug, Clone)]
pub struct FlipEnv {
    pub user_root: Option<PathBuf>,
    pub shipped_root: Option<PathBuf>,
    pub backend: RegBackend,
    pub threads: Option<u32>,
}

impl FlipEnv {
    /// The user data directory and the shipped resources of this process.
    pub fn from_environment(backend: RegBackend, threads: Option<u32>) -> FlipEnv {
        FlipEnv {
            user_root: template_catalog::user_data_dir(),
            shipped_root: template_catalog::shipped_root(),
            backend,
            threads,
        }
    }
}

/// What is cached for a space.
#[derive(Debug, Clone)]
pub struct FlipStatus {
    pub space: String,
    pub cache_dir: Option<PathBuf>,
    pub found: Option<FlipPaths>,
    pub stale: bool,
}

/// Looks the transform of `files.space` up without building it.
pub fn describe(files: &TemplateFiles, env: &FlipEnv) -> FlipStatus {
    let space = files.space.as_str();
    let shipped = shipped_dirs(space, Some(&files.t1), env.shipped_root.as_deref());
    let found = locate(space, Some(files), env.user_root.as_deref(), &shipped);
    let cache_dir = env.user_root.as_deref().map(|r| cache_dir_in(r, space));
    let stale = cache_dir
        .as_deref()
        .is_some_and(|d| complete(&paths_in(d, space)) && !record_is_current(d, Some(files)));
    FlipStatus {
        space: space.to_owned(),
        cache_dir,
        found,
        stale,
    }
}

fn write_mirrored(src: &Path, dst: &Path) -> Result<(), FlipBuildError> {
    let vol = load_reg_volume(src, "template", STAGE).map_err(|e| failed(e.to_string()))?;
    let size = vol.grid().size();
    let (data, aff) = flip_x(vol.data(), size, &vol.grid().ras_affine());
    let grid = Grid::from_ras_affine(size, &aff).map_err(|e| failed(e.to_string()))?;
    let data: Vec<f32> = data.iter().map(|&v| v as f32).collect();
    Volume::from_vec(grid, data)
        .and_then(|v| v.save_nifti(dst))
        .map_err(|e| failed(format!("{}: {e}", dst.display())))
}

fn pipeline_error(e: PipelineError) -> FlipBuildError {
    match e {
        PipelineError::Cancelled => FlipBuildError::Cancelled,
        e => failed(e.to_string()),
    }
}

fn io_failed(what: &str, e: std::io::Error) -> FlipBuildError {
    failed(format!("{what}: {e}"))
}

fn build_in_staging(
    engine: &dyn RegEngine,
    files: &TemplateFiles,
    staging: &Path,
    threads: Option<u32>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<Map<String, Value>, FlipBuildError> {
    let mut mirrored = vec![staging.join("mirror_t1.nii.gz")];
    write_mirrored(&files.t1, &mirrored[0])?;
    if let Some(t2) = &files.t2 {
        let dst = staging.join("mirror_t2.nii.gz");
        write_mirrored(t2, &dst)?;
        mirrored.push(dst);
    }
    let roles = files.roles();
    let fixed: Vec<&Path> = roles.iter().map(|(_, p)| *p).collect();
    let moving: Vec<&Path> = mirrored.iter().map(PathBuf::as_path).collect();
    register_templates(
        &PairJob {
            engine,
            stage: STAGE,
            iso_mm: None,
            tmp_prefix: &staging.join(WORK_PREFIX),
            cancel,
        },
        &fixed,
        &moving,
        threads,
        tx,
    )
    .map_err(pipeline_error)
}

/// Builds the transform of `files.space` with `engine` into `dir` (replacing its content). The
/// work happens in a sibling folder that is renamed on success, so a failed or cancelled build
/// keeps the previous transform.
pub(crate) fn build_with(
    engine: &dyn RegEngine,
    files: &TemplateFiles,
    dir: &Path,
    threads: Option<u32>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<FlipPaths, FlipBuildError> {
    let parent = dir
        .parent()
        .ok_or_else(|| failed(format!("{} has no parent folder", dir.display())))?;
    let staging = parent.join(format!(".{DIR_NAME}.partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| io_failed(&staging.display().to_string(), e))?;
    let started = Instant::now();
    let result = build_in_staging(engine, files, &staging, threads, cancel, tx).and_then(|extra| {
        finish(
            engine,
            files,
            &staging,
            dir,
            extra,
            started.elapsed().as_secs_f64(),
        )
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn finish(
    engine: &dyn RegEngine,
    files: &TemplateFiles,
    staging: &Path,
    dir: &Path,
    extra: Map<String, Value>,
    elapsed_s: f64,
) -> Result<FlipPaths, FlipBuildError> {
    let space = files.space.as_str();
    let staged = paths_in(staging, space);
    let work = |suffix: &str| staging.join(format!("{WORK_PREFIX}{suffix}"));
    std::fs::rename(work("Composite.h5"), &staged.forward)
        .map_err(|e| io_failed("the forward composite", e))?;
    std::fs::rename(work("InverseComposite.h5"), &staged.inverse)
        .map_err(|e| io_failed("the inverse composite", e))?;
    let keep = [forward_name(space), inverse_name(space)];
    if let Ok(rd) = std::fs::read_dir(staging) {
        for entry in rd.flatten() {
            if !keep.contains(&entry.file_name().to_string_lossy().into_owned()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let record = json!({
        "software": crate::version::version_string(),
        "space": space,
        "created": crate::space_bridge::now_iso(),
        "engine": engine.describe(),
        "templates": files.fingerprint().map_err(failed)?,
        "elapsed_s": elapsed_s,
        "registration": Value::Object(extra),
    });
    let text = serde_json::to_string_pretty(&record).map_err(|e| failed(e.to_string()))?;
    std::fs::write(staging.join(BUILD_RECORD), text + "\n")
        .map_err(|e| io_failed(BUILD_RECORD, e))?;
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| io_failed(&format!("cannot replace {}", dir.display()), e))?;
    }
    std::fs::rename(staging, dir)
        .map_err(|e| io_failed(&format!("cannot create {}", dir.display()), e))?;
    Ok(paths_in(dir, space))
}

#[cfg(test)]
type BuildHook = Box<dyn Fn(&str, &Path) -> Result<(), FlipBuildError>>;

#[cfg(test)]
thread_local! {
    pub(crate) static BUILD_HOOK: std::cell::RefCell<Option<BuildHook>> =
        const { std::cell::RefCell::new(None) };
}

fn run_build(
    env: &FlipEnv,
    files: &TemplateFiles,
    dir: &Path,
    cancel: Option<&AtomicBool>,
    log: &mut dyn FnMut(String),
) -> Result<FlipPaths, FlipBuildError> {
    let choice = choose_for_run(env.backend).map_err(failed)?;
    let Some(engine) = choice.engine else {
        return Err(failed(
            "the in-process registration engine is not available (no usable GPU adapter, or \
             --reg-backend ants); the flip transform is built only by that engine",
        ));
    };
    let (tx, rx) = mpsc::channel();
    let engine: &dyn RegEngine = engine.as_ref();
    std::thread::scope(|scope| {
        let worker =
            scope.spawn(move || build_with(engine, files, dir, env.threads, cancel, Some(&tx)));
        for ev in rx {
            if let RunEvent::LogLine { line } = ev {
                log(line);
            }
        }
        worker
            .join()
            .unwrap_or_else(|_| Err(failed("the flip transform build panicked")))
    })
}

/// The cached flip transform of `space`, built first when it is missing (or `force`d). The build
/// takes minutes and stops with [`FlipBuildError::Cancelled`] once `cancel` is set. `template` is
/// the T1w file to build from; without it the catalogue template of `space` is used.
pub fn resolve_or_build(
    space: &str,
    template: Option<&Path>,
    env: &FlipEnv,
    force: bool,
    cancel: Option<&AtomicBool>,
    log: &mut dyn FnMut(String),
) -> Result<FlipPaths, FlipBuildError> {
    let files = match template {
        Some(t1) => Some(TemplateFiles::from_t1(space, t1)),
        None => {
            TemplateFiles::resolve(space, env.shipped_root.as_deref(), env.user_root.as_deref())
                .ok()
        }
    };
    if !force {
        let t1 = files.as_ref().map(|f| f.t1.as_path());
        let shipped = shipped_dirs(space, t1, env.shipped_root.as_deref());
        if let Some(p) = locate(space, files.as_ref(), env.user_root.as_deref(), &shipped) {
            return Ok(p);
        }
    }
    let root = env.user_root.as_deref().ok_or_else(|| {
        failed("no user data directory to store the flip transform in (set DBSLOC_DATA_DIR)")
    })?;
    let dir = cache_dir_in(root, space);
    log(format!(
        "dbsloc: building the left-right flip transform of {space} (minutes)..."
    ));
    #[cfg(test)]
    {
        let hooked = BUILD_HOOK.with(|h| h.borrow().as_ref().map(|f| f(space, &dir)));
        if let Some(r) = hooked {
            return r.map(|()| paths_in(&dir, space));
        }
    }
    let files = files.ok_or_else(|| {
        failed(format!(
            "the template of {space} is not installed, so the flip transform cannot be built"
        ))
    })?;
    let paths = run_build(env, &files, &dir, cancel, log)?;
    log(format!(
        "dbsloc: note: flip transform: built {}",
        dir.display()
    ));
    Ok(paths)
}

#[cfg(test)]
#[path = "fliplr_tests.rs"]
mod tests;
