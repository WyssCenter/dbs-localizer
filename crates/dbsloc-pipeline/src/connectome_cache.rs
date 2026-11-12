// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The optional on-disk cache of a structural connectome (`specs/15` §3.5): Morton-ordered
//! fibres, a dense 4 mm cell index, consent per connectome, listing and clearing.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dbsloc_connectome::{
    cache_key, probe_point_count, read_fibres_cancellable, Error as ConnError, FibreIndex,
    FibreSet, FibreSpace,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connectome_catalog::ConnectomeClass;
use crate::connectome_maps::{MapError, MapResult, Progress};
use crate::sha256::{hex, sha256_hex, Sha256};

pub const CACHE_VERSION: u32 = 1;
pub const READER_VERSION: u32 = 1;
pub const BYTES_PER_POINT: f64 = 14.4;
pub const CACHE_ENV: &str = "DBSLOC_CONNECTOME_CACHE";
pub const CELL_MM: u32 = 4;
const MORTON_MM: f64 = 2.0;
const MORTON_BITS: i64 = 1023;
const PROBE_BYTES: u64 = 16 * 1024 * 1024;
const GB: u64 = 1_000_000_000;
const WRITE_BLOCK: usize = 1 << 15;
const WRITE_GROUP: usize = 16;
const BUILD_STEPS: u64 = 5;

static BUILD_COUNTER: AtomicU64 = AtomicU64::new(0);
static IN_USE: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
static BUILDING: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
static CHOICES_LOCK: Mutex<()> = Mutex::new(());
const STALE_PARTIAL_SECS: u64 = 3600;
const LOCK_STALE_SECS: u64 = 30;
const LOCK_WAIT_MS: u64 = 5000;

pub type ProgressSink<'a> = &'a (dyn Fn(&Progress) + Sync);

/// The setting *Connectome cache* (`DBSLOC_CONNECTOME_CACHE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CacheSetting {
    #[default]
    Ask,
    Always,
    Never,
}

impl CacheSetting {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "ask" | "" => Some(Self::Ask),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    pub fn from_env() -> Self {
        std::env::var(CACHE_ENV)
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or_default()
    }
}

/// The recorded consent of one connectome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Choice {
    Allow,
    Decline,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChoiceRecord {
    pub choice: Choice,
    pub decided_at: String,
    pub estimated_bytes: u64,
    #[serde(default)]
    pub name: String,
}

/// What a job should do about the cache of one connectome (`specs/15` §3.5 consent rules).
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    UseCache,
    Build,
    Plain {
        note: Option<String>,
        warning: Option<String>,
    },
}

/// The estimated cache size of a connectome and the free disk space where it would go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    pub points: u64,
    pub estimated_bytes: u64,
    pub free_bytes: Option<u64>,
}

impl Estimate {
    pub fn rule_ok(&self) -> bool {
        free_rule_ok(self.estimated_bytes, self.free_bytes)
    }

    pub fn dialog_ok(&self) -> bool {
        self.free_bytes
            .is_none_or(|f| f >= self.estimated_bytes.saturating_add(5 * GB))
    }
}

pub fn free_rule_ok(estimated: u64, free: Option<u64>) -> bool {
    free.is_none_or(|f| f >= estimated.saturating_mul(2).saturating_add(5 * GB))
}

/// A connectome the cache can describe: a structural folder with a `data.mat` / `data.trk[.gz]`.
#[derive(Clone, Debug)]
pub struct CacheTarget {
    pub class: ConnectomeClass,
    pub name: String,
    pub id: String,
    pub folder: PathBuf,
}

impl CacheTarget {
    pub fn source_file(&self) -> Option<PathBuf> {
        if self.class != ConnectomeClass::Dmri {
            return None;
        }
        ["data.mat", "data.trk", "data.trk.gz"]
            .iter()
            .map(|n| self.folder.join(n))
            .find(|p| p.is_file())
    }

    pub fn key(&self) -> String {
        let canon = std::fs::canonicalize(&self.folder).unwrap_or_else(|_| self.folder.clone());
        cache_key(
            "dMRI",
            &folder_name(&self.folder),
            &sha256_hex(canon.to_string_lossy().as_bytes()),
        )
    }
}

fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn connectomes_cache_root() -> Option<PathBuf> {
    crate::template_catalog::cache_dir().map(|d| d.join("connectomes"))
}

fn choices_path() -> Option<PathBuf> {
    connectomes_cache_root().map(|d| d.join("choices.json"))
}

fn io_err(path: &Path, e: impl std::fmt::Display) -> MapError {
    MapError::Io(format!("{}: {e}", path.display()))
}

fn now_rfc3339() -> String {
    crate::version::utc_now_rfc3339()
}

fn read_choices() -> HashMap<String, ChoiceRecord> {
    choices_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn read_choices_for_update(path: &Path) -> MapResult<HashMap<String, ChoiceRecord>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(io_err(path, e)),
    };
    match serde_json::from_str(&text) {
        Ok(map) => Ok(map),
        Err(e) => {
            let bak = path.with_extension("json.bak");
            let _ = std::fs::remove_file(&bak);
            let kept = std::fs::rename(path, &bak).is_ok();
            Err(MapError::Format(format!(
                "{} is unreadable ({e}); {}",
                path.display(),
                if kept {
                    format!("kept as {}, the choices start over", bak.display())
                } else {
                    "left untouched".to_owned()
                }
            )))
        }
    }
}

struct ChoicesFileLock(PathBuf);

impl Drop for ChoicesFileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn lock_choices_file(path: &Path) -> MapResult<ChoicesFileLock> {
    let lock = path.with_extension("json.lock");
    let started = Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => return Ok(ChoicesFileLock(lock)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&lock)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age.as_secs() >= LOCK_STALE_SECS);
                if stale {
                    let _ = std::fs::remove_file(&lock);
                    continue;
                }
                if started.elapsed().as_millis() as u64 >= LOCK_WAIT_MS {
                    return Err(MapError::Io(format!(
                        "{} is locked by another process",
                        path.display()
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(io_err(&lock, e)),
        }
    }
}

fn update_choices(f: impl FnOnce(&mut HashMap<String, ChoiceRecord>) -> bool) -> MapResult<()> {
    let path = choices_path().ok_or_else(|| MapError::Io("no cache folder".into()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    }
    let _guard = CHOICES_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _file = lock_choices_file(&path)?;
    let mut all = read_choices_for_update(&path)?;
    if !f(&mut all) {
        return Ok(());
    }
    let sorted: std::collections::BTreeMap<_, _> = all.iter().collect();
    let text = serde_json::to_string_pretty(&sorted).map_err(|e| MapError::Io(e.to_string()))?;
    let tmp = path.with_extension(format!(
        "json.partial-{}-{}",
        std::process::id(),
        BUILD_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    write_synced(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err(&path, e)
    })
}

fn write_synced(path: &Path, bytes: &[u8]) -> MapResult<()> {
    let mut f = File::create(path).map_err(|e| io_err(path, e))?;
    f.write_all(bytes).map_err(|e| io_err(path, e))?;
    f.sync_all().map_err(|e| io_err(path, e))
}

pub fn choice(key: &str) -> Option<ChoiceRecord> {
    read_choices().remove(key)
}

pub fn set_choice(key: &str, name: &str, choice: Choice, estimated_bytes: u64) -> MapResult<()> {
    update_choices(|all| {
        all.insert(
            key.to_owned(),
            ChoiceRecord {
                choice,
                decided_at: now_rfc3339(),
                estimated_bytes,
                name: name.to_owned(),
            },
        );
        true
    })
}

pub fn reset_choice(key: &str) -> MapResult<()> {
    update_choices(|all| all.remove(key).is_some())
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeclinedEntry {
    pub key: String,
    pub name: String,
    pub decided_at: String,
    pub estimated_bytes: u64,
}

pub fn declined() -> Vec<DeclinedEntry> {
    let mut out: Vec<DeclinedEntry> = read_choices()
        .into_iter()
        .filter(|(_, r)| r.choice == Choice::Decline)
        .map(|(key, r)| DeclinedEntry {
            key,
            name: r.name,
            decided_at: r.decided_at,
            estimated_bytes: r.estimated_bytes,
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// The pure consent policy of `specs/15` §3.5.
pub fn decide(
    setting: CacheSetting,
    choice: Option<Choice>,
    cache_valid: bool,
    estimate: &Estimate,
    name: &str,
    id: &str,
) -> Decision {
    if setting == CacheSetting::Never {
        return Decision::Plain {
            note: None,
            warning: None,
        };
    }
    if cache_valid {
        return Decision::UseCache;
    }
    let may_build = setting == CacheSetting::Always || choice == Some(Choice::Allow);
    if may_build {
        return if estimate.rule_ok() {
            Decision::Build
        } else {
            Decision::Plain {
                note: None,
                warning: Some(format!(
                    "connectome cache for {name} not built: {} free, {} needed",
                    gb_text(estimate.free_bytes.unwrap_or(0)),
                    gb_text(
                        estimate
                            .estimated_bytes
                            .saturating_mul(2)
                            .saturating_add(5 * GB)
                    )
                )),
            }
        };
    }
    Decision::Plain {
        note: choice.is_none().then(|| {
            format!(
                "connectome cache for {name} not built (about {}); build it with 'dbsloc connectome cache build {id}'",
                gb_text(estimate.estimated_bytes)
            )
        }),
        warning: None,
    }
}

pub fn gb_text(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GB as f64)
}

/// The estimated size (`14.4 x P`, `P` from the header) and the free space of the cache folder.
pub fn estimate(target: &CacheTarget) -> MapResult<Estimate> {
    let file = target.source_file().ok_or_else(|| {
        MapError::Missing(format!(
            "no cacheable fibre file in {}",
            target.folder.display()
        ))
    })?;
    let points = probe_point_count(&file)?;
    let free_bytes = connectomes_cache_root()
        .as_deref()
        .and_then(crate::backdrop_fetch::free_space_bytes);
    Ok(Estimate {
        points,
        estimated_bytes: (points as f64 * BYTES_PER_POINT) as u64,
        free_bytes,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub source_path: String,
    pub source_size: u64,
    pub source_mtime: i64,
    pub source_probe_sha256: String,
    pub format: String,
    pub p: u64,
    pub k: u64,
    pub bbox_mm: [[f64; 3]; 2],
    pub space: String,
    #[serde(default)]
    pub space_affine: Option<[[f64; 4]; 4]>,
    #[serde(default)]
    pub mirrored: Option<bool>,
    pub has_vals: bool,
    pub name: String,
    pub class: String,
    pub built_by: String,
    pub built_at: String,
    pub reader_version: u32,
    pub cache_version: u32,
    pub order: String,
    pub cell_mm: u32,
    pub grid_origin_mm: [f64; 3],
    pub grid_dims: [usize; 3],
}

fn mtime_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

fn probe_sha(path: &Path) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut h = Sha256::new();
    h.update(&size.to_le_bytes());
    let mut buf = vec![0u8; 1 << 20];
    for start in [0, size.saturating_sub(PROBE_BYTES)] {
        f.seek(SeekFrom::Start(start))?;
        let mut left = PROBE_BYTES.min(size);
        while left > 0 {
            let want = buf.len().min(left as usize);
            let got = f.read(&mut buf[..want])?;
            if got == 0 {
                break;
            }
            h.update(&buf[..got]);
            left -= got as u64;
        }
    }
    Ok(hex(&h.finish()))
}

fn version_dir(key: &str) -> Option<PathBuf> {
    connectomes_cache_root().map(|r| r.join(key).join("v1"))
}

fn read_meta(dir: &Path) -> Option<Meta> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).ok()?).ok()
}

fn meta_matches(meta: &Meta, source: &Path) -> bool {
    let Ok(md) = std::fs::metadata(source) else {
        return false;
    };
    meta.cache_version == CACHE_VERSION
        && meta.reader_version == READER_VERSION
        && meta.source_size == md.len()
        && meta.source_mtime == mtime_ms(source)
        && probe_sha(source).is_ok_and(|s| s == meta.source_probe_sha256)
}

fn file_lens_ok(dir: &Path, meta: &Meta) -> bool {
    let len = |n: &str| std::fs::metadata(dir.join(n)).map(|m| m.len()).ok();
    let Some(cells) = meta
        .grid_dims
        .iter()
        .try_fold(1usize, |a, d| a.checked_mul(*d))
        .and_then(|c| c.checked_add(1))
        .and_then(|c| (c as u64).checked_mul(8))
    else {
        return false;
    };
    let (Some(p12), Some(k8), Some(k4)) = (
        meta.p.checked_mul(12),
        meta.k.checked_add(1).and_then(|k| k.checked_mul(8)),
        meta.k.checked_mul(4),
    ) else {
        return false;
    };
    len("points.f32") == Some(p12)
        && len("offsets.u64") == Some(k8)
        && len("order.u32") == Some(k4)
        && len("vals.f32") == Some(if meta.has_vals { k4 } else { 0 })
        && len("cells.u64") == Some(cells)
        && len("cells.u32").is_some()
}

fn structure_ok(offsets: &Map, cells64: &Map, cells32: &Map, meta: &Meta) -> bool {
    let k = meta.k as usize;
    let n_cells = meta.grid_dims.iter().product::<usize>();
    if offsets.bytes().len() != (k + 1) * 8 || cells64.bytes().len() != (n_cells + 1) * 8 {
        return false;
    }
    let non_decreasing = |m: &Map, n: usize| {
        let mut prev = 0u64;
        (0..n).all(|i| {
            let v = m.u64_at(i);
            let ok = v >= prev;
            prev = v;
            ok
        })
    };
    offsets.u64_at(0) == 0
        && non_decreasing(offsets, k + 1)
        && offsets.u64_at(k) == meta.p
        && cells64.u64_at(0) == 0
        && non_decreasing(cells64, n_cells + 1)
        && cells64.u64_at(n_cells).checked_mul(4) == Some(cells32.bytes().len() as u64)
}

/// Whether a valid cache of `target` exists (size, mtime, probe hash, versions, structure).
pub fn is_valid(target: &CacheTarget) -> bool {
    let (Some(src), Some(dir)) = (target.source_file(), version_dir(&target.key())) else {
        return false;
    };
    let Some(meta) = read_meta(&dir) else {
        return false;
    };
    if !meta_matches(&meta, &src) || !file_lens_ok(&dir, &meta) {
        return false;
    }
    let (Ok(offsets), Ok(cells64), Ok(cells32)) = (
        Map::open(&dir.join("offsets.u64")),
        Map::open(&dir.join("cells.u64")),
        Map::open(&dir.join("cells.u32")),
    ) else {
        return false;
    };
    structure_ok(&offsets, &cells64, &cells32, &meta)
}

struct Map(Option<memmap2::Mmap>);

impl Map {
    fn open(path: &Path) -> MapResult<Self> {
        let f = File::open(path).map_err(|e| io_err(path, e))?;
        if f.metadata().map_err(|e| io_err(path, e))?.len() == 0 {
            return Ok(Self(None));
        }
        // SAFETY: the cache files are written once under a `.partial-*` name, renamed, and never modified afterwards.
        let m = unsafe { memmap2::Mmap::map(&f) }.map_err(|e| io_err(path, e))?;
        Ok(Self(Some(m)))
    }

    fn bytes(&self) -> &[u8] {
        self.0.as_deref().unwrap_or(&[])
    }

    fn u64_at(&self, i: usize) -> u64 {
        let mut a = [0u8; 8];
        a.copy_from_slice(&self.bytes()[i * 8..i * 8 + 8]);
        u64::from_le_bytes(a)
    }

    fn u32_at(&self, i: usize) -> u32 {
        let mut a = [0u8; 4];
        a.copy_from_slice(&self.bytes()[i * 4..i * 4 + 4]);
        u32::from_le_bytes(a)
    }
}

fn triple(c: &[u8; 12]) -> [f32; 3] {
    let f = |i: usize| f32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
    [f(0), f(4), f(8)]
}

fn f32_triples(bytes: &[u8]) -> Vec<[f32; 3]> {
    bytes
        .as_chunks::<12>()
        .0
        .iter()
        .map(|c| {
            let f = |i: usize| f32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
            [f(0), f(4), f(8)]
        })
        .collect()
}

/// A cache opened for reading; the files stay mapped while the handle lives.
pub struct CacheHandle {
    key: String,
    pub meta: Meta,
    points: Map,
    offsets: Map,
    order: Map,
    vals: Map,
    cells64: Map,
    cells32: Map,
}

impl Drop for CacheHandle {
    fn drop(&mut self) {
        let mut g = IN_USE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(m) = g.as_mut() {
            if let Some(n) = m.get_mut(&self.key) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.remove(&self.key);
                }
            }
        }
    }
}

fn in_use(key: &str) -> bool {
    IN_USE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|m| m.contains_key(key))
}

struct BuildingGuard(String);

impl BuildingGuard {
    fn new(key: &str) -> Self {
        *BUILDING
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(HashMap::new)
            .entry(key.to_owned())
            .or_insert(0) += 1;
        Self(key.to_owned())
    }
}

impl Drop for BuildingGuard {
    fn drop(&mut self) {
        let mut g = BUILDING.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(m) = g.as_mut() {
            if let Some(n) = m.get_mut(&self.0) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.remove(&self.0);
                }
            }
        }
    }
}

fn build_active(key: &str) -> bool {
    BUILDING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|m| m.contains_key(key))
}

fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(dir).and_then(|m| m.modified()).ok()?;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                newest = newest.max(t);
            }
        }
    }
    Some(newest)
}

fn partial_is_stale(dir: &Path) -> bool {
    newest_mtime(dir)
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() >= STALE_PARTIAL_SECS)
}

fn cancelled(cancel: Option<&AtomicBool>) -> MapResult<()> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        Err(MapError::Cancelled)
    } else {
        Ok(())
    }
}

fn map_conn_err(e: ConnError) -> MapError {
    match e {
        ConnError::Cancelled => MapError::Cancelled,
        other => other.into(),
    }
}

fn emit(progress: Option<ProgressSink<'_>>, stage: &str, done: u64, total: u64, msg: &str) {
    if let Some(p) = progress {
        p(&Progress {
            stage: stage.to_owned(),
            done,
            total,
            message: msg.to_owned(),
        });
    }
}

impl CacheHandle {
    /// Opens the valid cache of `target`; `None` when there is none.
    pub fn open(target: &CacheTarget) -> MapResult<Option<Arc<Self>>> {
        let (Some(src), Some(dir)) = (target.source_file(), version_dir(&target.key())) else {
            return Ok(None);
        };
        let Some(meta) = read_meta(&dir) else {
            return Ok(None);
        };
        if !meta_matches(&meta, &src) || !file_lens_ok(&dir, &meta) {
            return Ok(None);
        }
        let key = target.key();
        let h = Self {
            key: key.clone(),
            points: Map::open(&dir.join("points.f32"))?,
            offsets: Map::open(&dir.join("offsets.u64"))?,
            order: Map::open(&dir.join("order.u32"))?,
            vals: Map::open(&dir.join("vals.f32"))?,
            cells64: Map::open(&dir.join("cells.u64"))?,
            cells32: Map::open(&dir.join("cells.u32"))?,
            meta,
        };
        IN_USE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(HashMap::new)
            .entry(key)
            .and_modify(|n| *n += 1)
            .or_insert(1);
        if !structure_ok(&h.offsets, &h.cells64, &h.cells32, &h.meta) {
            return Ok(None);
        }
        let _ = std::fs::write(dir.join("last_used"), now_rfc3339());
        Ok(Some(Arc::new(h)))
    }

    pub fn n_fibres(&self) -> usize {
        self.meta.k as usize
    }

    fn space(&self) -> FibreSpace {
        match self.meta.space_affine {
            Some(a) if self.meta.space != "mm" => FibreSpace::ConvertedFromVoxels(a),
            _ => FibreSpace::Mm,
        }
    }

    fn off(&self, c: usize) -> usize {
        self.offsets.u64_at(c) as usize
    }

    fn fibre_bytes(&self, c: usize) -> &[u8] {
        &self.points.bytes()[self.off(c) * 12..self.off(c + 1) * 12]
    }

    /// The whole set in source order, identical to reading the source file.
    pub fn load_all(&self, cancel: Option<&AtomicBool>) -> MapResult<(FibreSet, FibreIndex)> {
        let k = self.n_fibres();
        let order: Vec<u32> = (0..k).map(|c| self.order.u32_at(c)).collect();
        let mut rank = vec![0u32; k];
        let mut seen = vec![false; k];
        for (c, s) in order.iter().enumerate() {
            let slot = seen
                .get_mut(*s as usize)
                .ok_or_else(|| MapError::Format("cache order names a fibre out of range".into()))?;
            if std::mem::replace(slot, true) {
                return Err(MapError::Format("cache order names a fibre twice".into()));
            }
            rank[*s as usize] = c as u32;
        }
        cancelled(cancel)?;
        let mut offsets = Vec::with_capacity(k + 1);
        offsets.push(0u32);
        let mut total = 0u64;
        for c in rank.iter().map(|c| *c as usize) {
            total += (self.off(c + 1) - self.off(c)) as u64;
            offsets.push(
                u32::try_from(total)
                    .map_err(|_| MapError::Format("more than 2^32 - 1 points".into()))?,
            );
        }
        let mut points = vec![[0f32; 3]; total as usize];
        let block = 1 << 14;
        let mut slices: Vec<(usize, &mut [[f32; 3]])> = Vec::new();
        let mut rest = points.as_mut_slice();
        let mut s = 0;
        while s < k {
            let e = (s + block).min(k);
            let n = (offsets[e] - offsets[s]) as usize;
            let (head, tail) = rest.split_at_mut(n);
            slices.push((s, head));
            rest = tail;
            s = e;
        }
        let stop = AtomicBool::new(false);
        slices.into_par_iter().for_each(|(first, out)| {
            if stop.load(Ordering::Relaxed) || cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                stop.store(true, Ordering::Relaxed);
                return;
            }
            let end = (first + block).min(k);
            let mut at = 0;
            for c in &rank[first..end] {
                let bytes = self.fibre_bytes(*c as usize);
                for (dst, v) in out[at..].iter_mut().zip(bytes.as_chunks::<12>().0) {
                    *dst = triple(v);
                }
                at += bytes.len() / 12;
            }
        });
        cancelled(cancel)?;
        let vals = if self.meta.has_vals {
            (0..k)
                .map(|s| {
                    let c = rank[s] as usize;
                    f32::from_bits(self.vals.u32_at(c))
                })
                .collect()
        } else {
            Vec::new()
        };
        let n_cells = self.meta.grid_dims.iter().product::<usize>();
        let cells: Vec<u64> = (0..=n_cells).map(|i| self.cells64.u64_at(i)).collect();
        let n_ids = cells.last().copied().unwrap_or(0) as usize;
        let ids: Vec<u32> = (0..n_ids).map(|i| self.cells32.u32_at(i)).collect();
        let origin = self
            .meta
            .grid_origin_mm
            .map(|v| (v / f64::from(CELL_MM)) as i64);
        let index = FibreIndex::from_cache(
            CELL_MM as f32,
            origin,
            self.meta.grid_dims,
            cells,
            ids,
            &order,
        )?;
        cancelled(cancel)?;
        Ok((
            FibreSet {
                points,
                fibre_offsets: offsets,
                space: self.space(),
                vals,
                mirrored: self.meta.mirrored,
            },
            index,
        ))
    }

    /// The fibres with a point in a 4 mm cell overlapping the box `lo..=hi` (mm), as a subset in
    /// ascending source id order with those ids.
    pub fn region(&self, lo: [f64; 3], hi: [f64; 3]) -> MapResult<(FibreSet, Vec<u32>)> {
        let cell = f64::from(CELL_MM);
        let dims = self.meta.grid_dims;
        let origin = self.meta.grid_origin_mm;
        if dims.contains(&0) {
            return Ok((FibreSet::from_fibres(&[])?, Vec::new()));
        }
        let mut from = [0usize; 3];
        let mut to = [0usize; 3];
        for a in 0..3 {
            let l = ((lo[a] / cell).floor() * cell - origin[a]) / cell;
            let h = ((hi[a] / cell).floor() * cell - origin[a]) / cell;
            if h < 0.0 || l >= dims[a] as f64 || hi[a] < lo[a] {
                return Ok((FibreSet::from_fibres(&[])?, Vec::new()));
            }
            from[a] = l.max(0.0) as usize;
            to[a] = (h as usize).min(dims[a] - 1);
        }
        let mut found: Vec<u32> = Vec::new();
        for z in from[2]..=to[2] {
            for y in from[1]..=to[1] {
                for x in from[0]..=to[0] {
                    let id = (z * dims[1] + y) * dims[0] + x;
                    for i in self.cells64.u64_at(id)..self.cells64.u64_at(id + 1) {
                        let c = self.cells32.u32_at(i as usize);
                        if u64::from(c) >= self.meta.k {
                            return Err(MapError::Format(
                                "cache index names a fibre out of range".into(),
                            ));
                        }
                        found.push(c);
                    }
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        let mut pairs: Vec<(u32, u32)> = found
            .into_iter()
            .map(|c| (self.order.u32_at(c as usize), c))
            .collect();
        pairs.sort_unstable();
        let mut points = Vec::new();
        let mut offsets = vec![0u32];
        let mut vals = Vec::new();
        for (_, c) in &pairs {
            points.extend(f32_triples(self.fibre_bytes(*c as usize)));
            offsets.push(
                u32::try_from(points.len())
                    .map_err(|_| MapError::Memory("region holds too many points".into()))?,
            );
            if self.meta.has_vals {
                vals.push(f32::from_bits(self.vals.u32_at(*c as usize)));
            }
        }
        Ok((
            FibreSet {
                points,
                fibre_offsets: offsets,
                space: self.space(),
                vals,
                mirrored: self.meta.mirrored,
            },
            pairs.into_iter().map(|p| p.0).collect(),
        ))
    }
}

fn morton(p: &[f32; 3], origin: [f64; 3]) -> u32 {
    let mut code = 0u32;
    for a in 0..3 {
        let q = (((f64::from(p[a]) - origin[a]) / MORTON_MM).floor() as i64).clamp(0, MORTON_BITS);
        let mut v = q as u32;
        v = (v | (v << 16)) & 0x030000FF;
        v = (v | (v << 8)) & 0x0300F00F;
        v = (v | (v << 4)) & 0x030C30C3;
        v = (v | (v << 2)) & 0x09249249;
        code |= v << a;
    }
    code
}

fn bbox_of(set: &FibreSet) -> [[f64; 3]; 2] {
    let (lo, hi) = set
        .points
        .par_chunks(1 << 16)
        .map(|chunk| {
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for p in chunk {
                for a in 0..3 {
                    lo[a] = lo[a].min(f64::from(p[a]));
                    hi[a] = hi[a].max(f64::from(p[a]));
                }
            }
            (lo, hi)
        })
        .reduce(
            || ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]),
            |a, b| {
                let mut lo = a.0;
                let mut hi = a.1;
                for i in 0..3 {
                    lo[i] = lo[i].min(b.0[i]);
                    hi[i] = hi[i].max(b.1[i]);
                }
                (lo, hi)
            },
        );
    if lo[0].is_finite() {
        [lo, hi]
    } else {
        [[0.0; 3]; 2]
    }
}

fn write_file(
    path: &Path,
    bytes: impl FnOnce(&mut BufWriter<File>) -> MapResult<()>,
) -> MapResult<()> {
    let f = File::create(path).map_err(|e| io_err(path, e))?;
    let mut w = BufWriter::with_capacity(8 << 20, f);
    bytes(&mut w)?;
    let f = w.into_inner().map_err(|e| io_err(path, e.error()))?;
    f.sync_all().map_err(|e| io_err(path, e))
}

fn sync_dir(dir: &Path) {
    let _ = File::open(dir).and_then(|d| d.sync_all());
}

/// What a finished build reports.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildReport {
    pub key: String,
    pub bytes: u64,
    pub seconds: f64,
    pub points: u64,
    pub fibres: u64,
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.metadata().map_or(0, |m| m.len()))
                .sum()
        })
        .unwrap_or(0)
}

/// Removes `<root>/<key>/v1` after renaming it away; `false` (and nothing changed) when a file is in use.
fn remove_version_dir(key: &str) -> MapResult<bool> {
    let Some(v1) = version_dir(key) else {
        return Ok(true);
    };
    if !v1.exists() {
        return Ok(true);
    }
    if in_use(key) {
        return Ok(false);
    }
    let trash = v1.with_file_name(format!(
        "v1.deleting-{}-{}",
        std::process::id(),
        BUILD_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::rename(&v1, &trash).is_err() {
        return Ok(false);
    }
    let _ = std::fs::remove_dir_all(&trash);
    Ok(true)
}

/// Builds the cache of `target` from the source file. Returns the report and the set that was read, in source order.
pub fn build(
    target: &CacheTarget,
    cancel: Option<&AtomicBool>,
    progress: Option<ProgressSink<'_>>,
) -> MapResult<(BuildReport, FibreSet)> {
    let started = Instant::now();
    let src = target.source_file().ok_or_else(|| {
        MapError::Missing(format!(
            "no cacheable fibre file in {}",
            target.folder.display()
        ))
    })?;
    let key = target.key();
    let _building = BuildingGuard::new(&key);
    let root = connectomes_cache_root().ok_or_else(|| MapError::Io("no cache folder".into()))?;
    let keydir = root.join(&key);
    std::fs::create_dir_all(&keydir).map_err(|e| io_err(&keydir, e))?;
    let size = std::fs::metadata(&src).map_err(|e| io_err(&src, e))?.len();
    let mtime = mtime_ms(&src);
    let probe = probe_sha(&src).map_err(|e| io_err(&src, e))?;
    emit(progress, "cache", 0, BUILD_STEPS, "reading the connectome");
    let set = read_fibres_cancellable(&src, cancel).map_err(map_conn_err)?;
    cancelled(cancel)?;
    let k = set.n_fibres();
    let p = set.points.len();
    emit(progress, "cache", 1, BUILD_STEPS, "ordering the fibres");
    let bbox = bbox_of(&set);
    let morton_origin = bbox[0].map(|v| (v / MORTON_MM).floor() * MORTON_MM);
    let mut keyed: Vec<(u32, u32)> = (0..k)
        .into_par_iter()
        .map(|f| {
            let pts = set.fibre(f);
            let code = pts
                .get(pts.len() / 2)
                .map_or(0, |q| morton(q, morton_origin));
            (code, f as u32)
        })
        .collect();
    keyed.par_sort_unstable();
    let order: Vec<u32> = keyed.iter().map(|k| k.1).collect();
    drop(keyed);
    let mut rank = vec![0u32; k];
    for (c, s) in order.iter().enumerate() {
        rank[*s as usize] = c as u32;
    }
    cancelled(cancel)?;
    emit(progress, "cache", 2, BUILD_STEPS, "indexing");
    let index = FibreIndex::build(&set, CELL_MM as f32)?;
    let (origin_cell, dims, cells, ids_src) = index.parts();
    let mut ids = ids_src.to_vec();
    {
        let mut slices: Vec<&mut [u32]> = Vec::with_capacity(cells.len());
        let mut rest = ids.as_mut_slice();
        for w in cells.windows(2) {
            let (head, tail) = rest.split_at_mut((w[1] - w[0]) as usize);
            slices.push(head);
            rest = tail;
        }
        slices.into_par_iter().for_each(|cell| {
            for id in cell.iter_mut() {
                *id = rank[*id as usize];
            }
            cell.sort_unstable();
        });
    }
    cancelled(cancel)?;
    let n = BUILD_COUNTER.fetch_add(1, Ordering::Relaxed);
    let partial = keydir.join(format!(".partial-{key}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|e| io_err(&partial, e))?;
    let result = (|| -> MapResult<()> {
        emit(progress, "cache", 3, BUILD_STEPS, "writing the fibres");
        write_file(&partial.join("points.f32"), |w| {
            let blocks: Vec<&[u32]> = order.chunks(WRITE_BLOCK).collect();
            for group in blocks.chunks(WRITE_GROUP) {
                cancelled(cancel)?;
                let bufs: Vec<Vec<u8>> = group
                    .par_iter()
                    .map(|b| {
                        let mut out = Vec::new();
                        for s in *b {
                            for q in set.fibre(*s as usize) {
                                for v in q {
                                    out.extend_from_slice(&v.to_le_bytes());
                                }
                            }
                        }
                        out
                    })
                    .collect();
                for b in bufs {
                    w.write_all(&b)
                        .map_err(|e| io_err(&partial.join("points.f32"), e))?;
                }
            }
            Ok(())
        })?;
        write_file(&partial.join("offsets.u64"), |w| {
            let mut at = 0u64;
            let put = |w: &mut BufWriter<File>, v: u64| {
                w.write_all(&v.to_le_bytes())
                    .map_err(|e| io_err(Path::new("offsets.u64"), e))
            };
            put(w, 0)?;
            for s in &order {
                at += set.fibre(*s as usize).len() as u64;
                put(w, at)?;
            }
            Ok(())
        })?;
        let u32s = |name: &str, data: &mut dyn Iterator<Item = u32>| {
            write_file(&partial.join(name), |w| {
                for v in data {
                    w.write_all(&v.to_le_bytes())
                        .map_err(|e| io_err(Path::new(name), e))?;
                }
                Ok(())
            })
        };
        u32s("order.u32", &mut order.iter().copied())?;
        if set.vals.len() == k {
            u32s(
                "vals.f32",
                &mut order.iter().map(|s| set.vals[*s as usize].to_bits()),
            )?;
        } else {
            write_synced(&partial.join("vals.f32"), &[])?;
        }
        emit(progress, "cache", 4, BUILD_STEPS, "writing the index");
        write_file(&partial.join("cells.u64"), |w| {
            for v in cells {
                w.write_all(&v.to_le_bytes())
                    .map_err(|e| io_err(Path::new("cells.u64"), e))?;
            }
            Ok(())
        })?;
        u32s("cells.u32", &mut ids.iter().copied())?;
        let (space, space_affine) = match &set.space {
            FibreSpace::Mm => ("mm".to_owned(), None),
            FibreSpace::ConvertedFromVoxels(a) => ("vox".to_owned(), Some(*a)),
        };
        let cell = f64::from(CELL_MM);
        let meta = Meta {
            source_path: src.display().to_string(),
            source_size: size,
            source_mtime: mtime,
            source_probe_sha256: probe.clone(),
            format: src
                .extension()
                .map_or_else(String::new, |e| e.to_string_lossy().into_owned()),
            p: p as u64,
            k: k as u64,
            bbox_mm: bbox,
            space,
            space_affine,
            mirrored: set.mirrored,
            has_vals: set.vals.len() == k,
            name: target.name.clone(),
            class: "dMRI".to_owned(),
            built_by: crate::version::version_string(),
            built_at: now_rfc3339(),
            reader_version: READER_VERSION,
            cache_version: CACHE_VERSION,
            order: "morton-mid-2mm".to_owned(),
            cell_mm: CELL_MM,
            grid_origin_mm: origin_cell.map(|c| c as f64 * cell),
            grid_dims: dims,
        };
        let text = serde_json::to_string_pretty(&meta).map_err(|e| MapError::Io(e.to_string()))?;
        write_synced(&partial.join("meta.json"), text.as_bytes())?;
        write_synced(&partial.join("last_used"), now_rfc3339().as_bytes())?;
        sync_dir(&partial);
        cancelled(cancel)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    if !remove_version_dir(&key)? {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(MapError::Io(format!(
            "the cache of {} is in use; close what uses it and try again",
            target.name
        )));
    }
    let v1 = keydir.join("v1");
    if let Err(e) = std::fs::rename(&partial, &v1) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(io_err(&v1, e));
    }
    sync_dir(&keydir);
    emit(progress, "cache", BUILD_STEPS, BUILD_STEPS, "cache ready");
    Ok((
        BuildReport {
            key,
            bytes: dir_size(&v1),
            seconds: started.elapsed().as_secs_f64(),
            points: p as u64,
            fibres: k as u64,
        },
        set,
    ))
}

/// One cache found under the cache folder.
#[derive(Clone, Debug, PartialEq)]
pub struct CacheInfo {
    pub key: String,
    pub name: String,
    pub source_path: String,
    pub size_bytes: u64,
    pub last_used: Option<SystemTime>,
    pub orphaned: bool,
    pub in_use: bool,
}

pub fn list_caches() -> Vec<CacheInfo> {
    let Some(root) = connectomes_cache_root() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let v1 = e.path().join("v1");
        let Some(meta) = read_meta(&v1) else { continue };
        let key = e.file_name().to_string_lossy().into_owned();
        out.push(CacheInfo {
            name: meta.name.clone(),
            orphaned: !Path::new(&meta.source_path).exists(),
            source_path: meta.source_path,
            size_bytes: dir_size(&v1),
            last_used: std::fs::metadata(v1.join("last_used"))
                .and_then(|m| m.modified())
                .ok(),
            in_use: in_use(&key),
            key,
        });
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeleteResult {
    Deleted { key: String, freed_bytes: u64 },
    InUse { key: String },
    Failed { key: String, message: String },
}

/// Deletes the caches named by `keys`; one in use (mapped or held by a running job) is skipped
/// and reported, every deleted one gets its consent reset to undecided.
pub fn delete_caches(keys: &[String]) -> Vec<DeleteResult> {
    let Some(root) = connectomes_cache_root() else {
        return Vec::new();
    };
    keys.iter()
        .map(|key| {
            let safe = !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                && !key.starts_with('.');
            if !safe {
                return DeleteResult::Failed {
                    key: key.clone(),
                    message: "not a cache key".into(),
                };
            }
            if build_active(key) {
                return DeleteResult::InUse { key: key.clone() };
            }
            let size = version_dir(key).map_or(0, |d| dir_size(&d));
            match remove_version_dir(key) {
                Ok(true) => {
                    if let Ok(rd) = std::fs::read_dir(root.join(key)) {
                        for e in rd.flatten() {
                            let n = e.file_name().to_string_lossy().into_owned();
                            let sweep = n.starts_with("v1.deleting-")
                                || (n.starts_with(".partial-") && partial_is_stale(&e.path()));
                            if sweep {
                                let _ = std::fs::remove_dir_all(e.path());
                            }
                        }
                    }
                    let _ = std::fs::remove_dir(root.join(key));
                    if !build_active(key) {
                        let _ = reset_choice(key);
                    }
                    DeleteResult::Deleted {
                        key: key.clone(),
                        freed_bytes: size,
                    }
                }
                Ok(false) => DeleteResult::InUse { key: key.clone() },
                Err(e) => DeleteResult::Failed {
                    key: key.clone(),
                    message: e.to_string(),
                },
            }
        })
        .collect()
}

pub fn clear(target: &CacheTarget) -> DeleteResult {
    delete_caches(&[target.key()])
        .into_iter()
        .next()
        .unwrap_or(DeleteResult::Failed {
            key: target.key(),
            message: "no cache folder".into(),
        })
}

/// Builds the cache and, only after a successful build, records `allow` (the GUI *Keep a copy*,
/// `dbsloc connectome cache build`; `specs/15` §3.5).
pub fn build_and_allow(
    target: &CacheTarget,
    cancel: Option<&AtomicBool>,
    progress: Option<ProgressSink<'_>>,
) -> MapResult<(BuildReport, FibreSet)> {
    let est = estimate(target)?;
    let built = build(target, cancel, progress)?;
    set_choice(
        &target.key(),
        &target.name,
        Choice::Allow,
        est.estimated_bytes,
    )?;
    Ok(built)
}

fn open_or_warn(target: &CacheTarget, warnings: &mut Vec<String>) -> Option<Arc<CacheHandle>> {
    match CacheHandle::open(target) {
        Ok(h) => h,
        Err(e) => {
            warnings.push(format!(
                "connectome cache for {} not used: {e}",
                target.name
            ));
            None
        }
    }
}

/// What [`prepare`] did about the cache of one connectome.
pub enum Prepared {
    Cached(Arc<CacheHandle>),
    Built(Arc<CacheHandle>, Box<FibreSet>),
    Plain,
}

/// Applies the consent policy: opens a valid cache, builds one the user allowed, else `Plain`.
/// Notes and warnings (`dbsloc: note: …` texts without the prefix) are appended to `notes` / `warnings`.
pub fn prepare(
    target: &CacheTarget,
    setting: CacheSetting,
    cancel: Option<&AtomicBool>,
    progress: Option<ProgressSink<'_>>,
    notes: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> MapResult<Prepared> {
    if setting == CacheSetting::Never || target.source_file().is_none() {
        return Ok(Prepared::Plain);
    }
    let est = match estimate(target) {
        Ok(e) => e,
        Err(_) => return Ok(Prepared::Plain),
    };
    let valid = is_valid(target);
    let recorded = choice(&target.key()).map(|r| r.choice);
    match decide(setting, recorded, valid, &est, &target.name, &target.id) {
        Decision::UseCache => match open_or_warn(target, warnings) {
            Some(h) => Ok(Prepared::Cached(h)),
            None => Ok(Prepared::Plain),
        },
        Decision::Build => match build(target, cancel, progress) {
            Ok((_, set)) => match open_or_warn(target, warnings) {
                Some(h) => Ok(Prepared::Built(h, Box::new(set))),
                None => Ok(Prepared::Plain),
            },
            Err(MapError::Cancelled) => Err(MapError::Cancelled),
            Err(e) => {
                warnings.push(format!(
                    "connectome cache for {} not built: {e}",
                    target.name
                ));
                Ok(Prepared::Plain)
            }
        },
        Decision::Plain { note, warning } => {
            notes.extend(note);
            warnings.extend(warning);
            Ok(Prepared::Plain)
        }
    }
}

#[cfg(test)]
pub(crate) fn with_cache_dir<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var(crate::template_catalog::CACHE_DIR_ENV, dir);
    let out = f();
    std::env::remove_var(crate::template_catalog::CACHE_DIR_ENV);
    out
}

#[cfg(test)]
mod tests;
