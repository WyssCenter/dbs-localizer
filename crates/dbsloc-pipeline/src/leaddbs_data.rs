// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Import of atlas and connectome folders from an existing Lead-DBS installation
//! (`specs/17`): scan, use in place, copy.

use std::fs::{DirEntry, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Serialize, Serializer};

use crate::atlas_catalog::ATLAS_SPACE;
use crate::atlas_index::ATLAS_INDEX_FILE;
use crate::bids::simplify_verbatim;
use crate::connectome_catalog::{match_installed, Access, ConnectomeClass};
use crate::connectome_fetch::{
    discover_connectomes, folder_has_marker, read_dataset_info, ConnectomeRoots, RootKind,
};
use crate::template_catalog::user_data_dir;

const GB: u64 = 1_000_000_000;
const STAGING_PREFIX: &str = ".partial-copy-";
const STAGING_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
const CHUNK: usize = 1 << 20;
const EVENT_STEP: u64 = 8 << 20;

/// Error codes of `specs/17` §4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadDbsCode {
    Missing,
    Disk,
    Io,
    Cancelled,
}

impl LeadDbsCode {
    pub fn code(self) -> &'static str {
        match self {
            LeadDbsCode::Missing => "E_LDBS_MISSING",
            LeadDbsCode::Disk => "E_LDBS_DISK",
            LeadDbsCode::Io => "E_LDBS_IO",
            LeadDbsCode::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadDbsDataError {
    pub code: LeadDbsCode,
    pub message: String,
    pub partial: Option<Box<CopyOutcome>>,
}

impl LeadDbsDataError {
    pub fn new(code: LeadDbsCode, message: impl Into<String>) -> Self {
        LeadDbsDataError {
            code,
            message: message.into(),
            partial: None,
        }
    }

    pub fn with_partial(mut self, outcome: CopyOutcome) -> Self {
        self.partial = Some(Box::new(outcome));
        self
    }

    pub fn cancelled() -> Self {
        LeadDbsDataError::new(LeadDbsCode::Cancelled, "cancelled")
    }

    pub fn is_cancelled(&self) -> bool {
        self.code == LeadDbsCode::Cancelled
    }
}

impl std::fmt::Display for LeadDbsDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_cancelled() {
            f.write_str("cancelled")
        } else {
            write!(f, "{}: {}", self.code.code(), self.message)
        }
    }
}

impl std::error::Error for LeadDbsDataError {}

type Res<T> = Result<T, LeadDbsDataError>;

fn io_err(path: &Path, e: &std::io::Error) -> LeadDbsDataError {
    LeadDbsDataError::new(LeadDbsCode::Io, format!("{}: {e}", path.display()))
}

fn serialize_class<S: Serializer>(c: &ConnectomeClass, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(c.dir_name())
}

fn serialize_access<S: Serializer>(a: &Option<Access>, s: S) -> Result<S::Ok, S::Error> {
    match a {
        Some(a) => s.serialize_some(a.label()),
        None => s.serialize_none(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AtlasItem {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub unreadable: u64,
    pub has_index: bool,
    pub bundled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectomeItem {
    #[serde(serialize_with = "serialize_class")]
    pub class: ConnectomeClass,
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub unreadable: u64,
    pub catalogue_id: Option<String>,
    #[serde(serialize_with = "serialize_access")]
    pub access: Option<Access>,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Inventory {
    pub source: PathBuf,
    pub atlases_root: Option<PathBuf>,
    pub connectome_root: Option<PathBuf>,
    pub atlases: Vec<AtlasItem>,
    pub connectomes: Vec<ConnectomeItem>,
    pub notes: Vec<String>,
}

impl Inventory {
    pub fn atlas_bytes(&self) -> u64 {
        self.atlases
            .iter()
            .fold(0u64, |acc, a| acc.saturating_add(a.bytes))
    }

    pub fn connectome_bytes(&self) -> u64 {
        self.connectomes
            .iter()
            .fold(0u64, |acc, c| acc.saturating_add(c.bytes))
    }

    pub fn is_empty(&self) -> bool {
        self.atlases.is_empty() && self.connectomes.is_empty()
    }
}

/// What the scan needs to know about this installation.
#[derive(Debug, Clone)]
pub struct ScanContext {
    pub bundled_atlases: Vec<String>,
    pub roots: ConnectomeRoots,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPlan {
    pub atlases_dir: Option<PathBuf>,
    pub connectomes_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selection {
    pub atlases: Vec<String>,
    pub connectomes: Vec<(ConnectomeClass, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destinations {
    pub atlases_dir: PathBuf,
    pub connectomes_root: PathBuf,
    pub free_space_override: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataKind {
    Atlas,
    Connectome(ConnectomeClass),
}

impl DataKind {
    pub fn label(self) -> &'static str {
        match self {
            DataKind::Atlas => "atlas",
            DataKind::Connectome(c) => c.dir_name(),
        }
    }
}

impl Serialize for DataKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CopiedItem {
    pub kind: DataKind,
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub skipped_links: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CopyOutcome {
    pub copied: Vec<CopiedItem>,
    pub skipped: Vec<CopiedItem>,
    pub atlases_dir: Option<PathBuf>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyEvent {
    ItemStarted {
        kind: DataKind,
        name: String,
        index: usize,
        count: usize,
    },
    Bytes {
        copied: u64,
        total: u64,
    },
    ItemDone {
        kind: DataKind,
        name: String,
    },
    ItemSkipped {
        kind: DataKind,
        name: String,
    },
    AllDone,
}

/// Names of the atlas sets that ship under `<resources>/atlases/leaddbs`.
pub fn bundled_atlas_names(resources: Option<&Path>) -> Vec<String> {
    let Some(res) = resources else {
        return Vec::new();
    };
    let mut names: Vec<String> = std::fs::read_dir(res.join("atlases").join("leaddbs"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    names
}

/// `<user data dir>/atlases/leaddbs`, or `None` without a per-user data folder.
pub fn user_atlases_dir_opt() -> Option<PathBuf> {
    user_data_dir().map(|d| d.join("atlases").join("leaddbs"))
}

/// [`user_atlases_dir_opt`], falling back to the relative path `atlases/leaddbs`.
pub fn user_atlases_dir() -> PathBuf {
    user_atlases_dir_opt().unwrap_or_else(|| PathBuf::from("atlases").join("leaddbs"))
}

/// "N linked entries were not copied" for an item, or `None` when no link was skipped.
pub fn linked_entries_text(n: u64) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 linked entry was not copied".to_owned()),
        n => Some(format!("{n} linked entries were not copied")),
    }
}

fn clean_dir(dir: &Path) -> Res<PathBuf> {
    let unreadable = || {
        LeadDbsDataError::new(
            LeadDbsCode::Missing,
            format!("{} is not a folder that can be read", dir.display()),
        )
    };
    if dir.components().any(|c| c == Component::ParentDir) {
        return Err(LeadDbsDataError::new(
            LeadDbsCode::Missing,
            format!(
                "{} contains `..`; give the folder without it",
                dir.display()
            ),
        ));
    }
    let expanded = crate::paths::expand_user(dir);
    let canon = std::fs::canonicalize(&expanded)
        .map(simplify_verbatim)
        .map_err(|_| unreadable())?;
    if std::fs::read_dir(&canon).is_err() {
        return Err(unreadable());
    }
    Ok(canon)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    Link,
    Skip,
}

#[cfg(windows)]
const REPARSE_POINT_ATTRIBUTE: u32 = 0x400;

fn classify_entry(is_symlink: bool, is_file: bool, is_dir: bool, reparse_point: bool) -> Kind {
    if is_symlink {
        Kind::Link
    } else if is_dir {
        if reparse_point {
            Kind::Link
        } else {
            Kind::Dir
        }
    } else if is_file {
        Kind::File
    } else {
        Kind::Skip
    }
}

fn entry_kind(entry: &DirEntry) -> std::io::Result<Kind> {
    let ft = entry.file_type()?;
    #[cfg(windows)]
    let reparse_point = {
        use std::os::windows::fs::MetadataExt;
        ft.is_dir() && entry.metadata()?.file_attributes() & REPARSE_POINT_ATTRIBUTE != 0
    };
    #[cfg(not(windows))]
    let reparse_point = false;
    Ok(classify_entry(
        ft.is_symlink(),
        ft.is_file(),
        ft.is_dir(),
        reparse_point,
    ))
}

#[derive(Default)]
struct Tally {
    bytes: u64,
    files: u64,
    unreadable: u64,
}

fn tally(root: &Path, cancel: &AtomicBool) -> Res<Tally> {
    let mut t = Tally::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::SeqCst) {
            return Err(LeadDbsDataError::cancelled());
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            t.unreadable += 1;
            continue;
        };
        for entry in rd {
            let Ok(entry) = entry else {
                t.unreadable += 1;
                continue;
            };
            match entry_kind(&entry) {
                Ok(Kind::File) => match entry.metadata() {
                    Ok(m) => {
                        t.bytes = t.bytes.saturating_add(m.len());
                        t.files += 1;
                    }
                    Err(_) => t.unreadable += 1,
                },
                Ok(Kind::Dir) => stack.push(entry.path()),
                Ok(Kind::Link | Kind::Skip) => {}
                Err(_) => t.unreadable += 1,
            }
        }
    }
    Ok(t)
}

fn subfolders(root: &Path, notes: &mut Vec<String>) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        match entry_kind(&entry) {
            Ok(Kind::Dir) => out.push((name, entry.path())),
            Ok(Kind::Link) => notes.push(format!(
                "Skipped {} because it is a link; links are never followed.",
                entry.path().display()
            )),
            _ => {}
        }
    }
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    out
}

fn has_class_folder(root: &Path) -> bool {
    ConnectomeClass::ALL
        .iter()
        .any(|c| root.join(c.dir_name()).is_dir())
}

/// Inventory of the atlases and connectomes under `dir` (`specs/17` §2). Writes nothing.
pub fn scan(
    dir: &Path,
    context: &ScanContext,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(usize),
) -> Res<Inventory> {
    let source = clean_dir(dir)?;
    let atlases_root = Some(
        source
            .join("templates")
            .join("space")
            .join(ATLAS_SPACE)
            .join("atlases"),
    )
    .filter(|p| p.is_dir());
    let nested = source.join("connectomes");
    let connectome_root = if has_class_folder(&nested) {
        Some(nested)
    } else if has_class_folder(&source) {
        Some(source.clone())
    } else {
        None
    };
    let mut inv = Inventory {
        source: source.clone(),
        atlases_root: atlases_root.clone(),
        connectome_root: connectome_root.clone(),
        atlases: Vec::new(),
        connectomes: Vec::new(),
        notes: Vec::new(),
    };
    if atlases_root.is_none() && connectome_root.is_none() {
        inv.notes.push(format!(
            "No Lead-DBS atlases or connectomes were found in {}. Choose the Lead-DBS installation folder (it contains `templates`) or the folder that contains `connectomes`.",
            source.display()
        ));
        return Ok(inv);
    }
    let mut done = 0usize;
    if let Some(root) = &atlases_root {
        for (name, path) in subfolders(root, &mut inv.notes) {
            let t = tally(&path, cancel)?;
            done += 1;
            on_progress(done);
            inv.atlases.push(AtlasItem {
                bundled: context.bundled_atlases.contains(&name),
                has_index: path.join(ATLAS_INDEX_FILE).is_file(),
                name,
                path,
                bytes: t.bytes,
                files: t.files,
                unreadable: t.unreadable,
            });
        }
    }
    if let Some(root) = &connectome_root {
        let existing: Vec<(ConnectomeClass, String)> =
            if context.roots.primary.as_os_str().is_empty() {
                Vec::new()
            } else {
                discover_connectomes(&context.roots)
                    .into_iter()
                    .filter(|c| !c.missing && c.root_kind != RootKind::External)
                    .map(|c| (c.class, c.name))
                    .collect()
            };
        for class in ConnectomeClass::ALL {
            for (name, path) in subfolders(&root.join(class.dir_name()), &mut inv.notes) {
                if !folder_has_marker(&path, class) {
                    continue;
                }
                let t = tally(&path, cancel)?;
                done += 1;
                on_progress(done);
                let tag =
                    read_dataset_info(&path).and_then(|v| v["tag"].as_str().map(str::to_owned));
                let entry = match_installed(class, &name, tag.as_deref());
                inv.connectomes.push(ConnectomeItem {
                    class,
                    present: existing.iter().any(|(c, n)| *c == class && *n == name),
                    catalogue_id: entry.map(|e| e.id.to_owned()),
                    access: entry.map(|e| e.access),
                    name,
                    path,
                    bytes: t.bytes,
                    files: t.files,
                    unreadable: t.unreadable,
                });
            }
        }
    }
    Ok(inv)
}

/// The two roots to store as settings when the data are used in place (`specs/17` §3.1).
pub fn link_plan(inv: &Inventory) -> LinkPlan {
    LinkPlan {
        atlases_dir: inv.atlases_root.clone(),
        connectomes_dir: inv.connectome_root.clone(),
    }
}

/// Every atlas that does not ship with the application and every connectome not yet present.
pub fn default_selection(inv: &Inventory) -> Selection {
    Selection {
        atlases: inv
            .atlases
            .iter()
            .filter(|a| !a.bundled)
            .map(|a| a.name.clone())
            .collect(),
        connectomes: inv
            .connectomes
            .iter()
            .filter(|c| !c.present)
            .map(|c| (c.class, c.name.clone()))
            .collect(),
    }
}

struct Job<'a> {
    kind: DataKind,
    name: &'a str,
    source: &'a Path,
    dest: PathBuf,
    bytes: u64,
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

fn volume_key(path: &Path) -> String {
    let anchor = existing_ancestor(&crate::paths::absolute(path));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata(&anchor) {
            return format!("device {}", m.dev());
        }
    }
    #[cfg(windows)]
    {
        if let Some(Component::Prefix(p)) = anchor.components().next() {
            use std::path::Prefix;
            return match p.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                    format!("{}:", char::from(d).to_ascii_lowercase())
                }
                _ => p.as_os_str().to_string_lossy().to_ascii_lowercase(),
            };
        }
    }
    anchor.display().to_string()
}

/// True when both paths are on the same disk volume (the nearest existing folder decides).
pub fn same_volume(a: &Path, b: &Path) -> bool {
    volume_key(a) == volume_key(b)
}

fn check_disk(jobs: &[&Job<'_>], dest: &Destinations) -> Res<Vec<String>> {
    check_disk_with(jobs, dest, &crate::backdrop_fetch::free_space_bytes)
}

fn check_disk_with(
    jobs: &[&Job<'_>],
    dest: &Destinations,
    free_space: &dyn Fn(&Path) -> Option<u64>,
) -> Res<Vec<String>> {
    let mut notes = Vec::new();
    let mut volumes: Vec<(String, PathBuf, u64)> = Vec::new();
    for job in jobs {
        let key = if dest.free_space_override.is_some() {
            "the destination".to_owned()
        } else {
            volume_key(&job.dest)
        };
        match volumes.iter_mut().find(|v| v.0 == key) {
            Some(v) => v.2 = v.2.saturating_add(job.bytes),
            None => volumes.push((key, job.dest.clone(), job.bytes)),
        }
    }
    for (_, sample, bytes) in volumes {
        let need = bytes.saturating_add(GB);
        let free = dest
            .free_space_override
            .or_else(|| free_space(sample.parent().unwrap_or(&sample)));
        let anchor = existing_ancestor(&crate::paths::absolute(&sample));
        match free {
            Some(free) if free < need => {
                return Err(LeadDbsDataError::new(
                    LeadDbsCode::Disk,
                    format!(
                        "not enough free disk space: need {} GB, {} GB free on {}",
                        need.div_ceil(GB),
                        free / GB,
                        anchor.display()
                    ),
                ));
            }
            Some(_) => {}
            None => notes.push(format!(
                "free disk space on {} could not be checked",
                anchor.display()
            )),
        }
    }
    Ok(notes)
}

fn lower_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| {
            let s = c.as_os_str().to_string_lossy().into_owned();
            if cfg!(windows) {
                s.to_lowercase()
            } else {
                s
            }
        })
        .collect()
}

fn is_inside(path: &Path, root: &Path) -> bool {
    let p = lower_components(path);
    let r = lower_components(root);
    p.len() >= r.len() && p[..r.len()] == r[..]
}

fn resolve_existing(path: &Path) -> PathBuf {
    let abs = crate::paths::absolute(path);
    let anchor = existing_ancestor(&abs);
    let Ok(canon) = std::fs::canonicalize(&anchor).map(simplify_verbatim) else {
        return abs;
    };
    match abs.strip_prefix(&anchor) {
        Ok(rest) => canon.join(rest),
        Err(_) => canon,
    }
}

fn guard_destinations(inv: &Inventory, jobs: &[Job<'_>], dest: &Destinations) -> Res<()> {
    let mut protected: Vec<PathBuf> = vec![resolve_existing(&inv.source)];
    protected.extend(inv.atlases_root.iter().map(|p| resolve_existing(p)));
    protected.extend(inv.connectome_root.iter().map(|p| resolve_existing(p)));
    let mut dests: Vec<(PathBuf, bool)> = Vec::new();
    if jobs.iter().any(|j| j.kind == DataKind::Atlas) {
        dests.push((resolve_existing(&dest.atlases_dir), true));
    }
    if jobs.iter().any(|j| j.kind != DataKind::Atlas) {
        dests.push((resolve_existing(&dest.connectomes_root), false));
    }
    for (d, atlas) in &dests {
        if let Some(root) = protected.iter().find(|r| is_inside(d, r)) {
            return Err(LeadDbsDataError::new(
                LeadDbsCode::Io,
                format!(
                    "the destination {} is inside the Lead-DBS folder {}; choose a data directory outside it",
                    d.display(),
                    root.display()
                ),
            ));
        }
        for job in jobs
            .iter()
            .filter(|j| (j.kind == DataKind::Atlas) == *atlas)
        {
            let src = resolve_existing(job.source);
            if is_inside(&src, d) {
                return Err(LeadDbsDataError::new(
                    LeadDbsCode::Io,
                    format!(
                        "the Lead-DBS folder {} is inside the destination {}; choose a data directory outside it",
                        src.display(),
                        d.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

const STAGING_MARKER: &str = ".dbsloc-copy.json";
const STAGING_STALE_SECS: u64 = 600;
const HEARTBEAT_EVERY_SECS: u64 = 30;
const REMOVE_ATTEMPTS: u32 = 5;
const REMOVE_PAUSE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagingState {
    Fresh,
    Stale,
}

fn staging_state(marker_heartbeat: u64, now: u64) -> StagingState {
    if now.saturating_sub(marker_heartbeat) > STAGING_STALE_SECS {
        StagingState::Stale
    } else {
        StagingState::Fresh
    }
}

fn heartbeat_due(last: u64, now: u64) -> bool {
    now.saturating_sub(last) >= HEARTBEAT_EVERY_SECS
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn write_staging_marker(staging: &Path, now: u64) -> Res<()> {
    let marker = staging.join(STAGING_MARKER);
    let body = serde_json::json!({ "pid": std::process::id(), "heartbeat": now });
    std::fs::write(&marker, body.to_string()).map_err(|e| io_err(&marker, &e))
}

fn staging_marker(staging: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(staging.join(STAGING_MARKER)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["heartbeat"].as_u64()
}

fn remove_dir_retrying(
    path: &Path,
    attempts: u32,
    pause: Duration,
    remover: &dyn Fn(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut last = Ok(());
    for attempt in 0..attempts.max(1) {
        match remover(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => last = Err(e),
        }
        if attempt + 1 < attempts {
            std::thread::sleep(pause);
        }
    }
    last
}

fn remove_dir_with_retries(path: &Path) -> std::io::Result<()> {
    remove_dir_retrying(path, REMOVE_ATTEMPTS, REMOVE_PAUSE, &|p| {
        std::fs::remove_dir_all(p)
    })
}

fn removal_note(staging: &Path, name: &str) -> String {
    format!(
        "the temporary folder {} could not be removed; delete it before copying {name} again",
        staging.display()
    )
}

fn sweep_stale_staging(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX)
        {
            continue;
        }
        let Some(heartbeat) = staging_marker(&entry.path()) else {
            continue;
        };
        if unix_now().saturating_sub(heartbeat) > STAGING_MAX_AGE.as_secs() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn prepare_staging(staging: &Path, name: &str, now: u64) -> Res<()> {
    prepare_staging_with(staging, name, now, &write_staging_marker)
}

fn prepare_staging_with(
    staging: &Path,
    name: &str,
    now: u64,
    write_marker: &dyn Fn(&Path, u64) -> Res<()>,
) -> Res<()> {
    if exists_any(staging) {
        let Some(heartbeat) = staging_marker(staging) else {
            return Err(LeadDbsDataError::new(
                LeadDbsCode::Io,
                format!(
                    "{} already exists and was not made by this application; remove it or move it away",
                    staging.display()
                ),
            ));
        };
        if staging_state(heartbeat, now) == StagingState::Fresh {
            return Err(LeadDbsDataError::new(
                LeadDbsCode::Io,
                format!(
                    "another copy of {name} seems to be running (folder {}, last active {} minutes ago). If no other copy is running, delete that folder and try again.",
                    staging.display(),
                    now.saturating_sub(heartbeat) / 60
                ),
            ));
        }
        remove_dir_with_retries(staging).map_err(|e| io_err(staging, &e))?;
    }
    std::fs::create_dir(staging).map_err(|e| io_err(staging, &e))?;
    if let Err(e) = write_marker(staging, now) {
        let _ = remove_dir_with_retries(staging);
        return Err(e);
    }
    Ok(())
}

struct Progress<'a> {
    copied: u64,
    reported: u64,
    total: u64,
    on_event: &'a mut dyn FnMut(CopyEvent),
    staging: Option<PathBuf>,
    last_beat: u64,
}

impl Progress<'_> {
    fn beat(&mut self) {
        let Some(staging) = &self.staging else {
            return;
        };
        let now = unix_now();
        if heartbeat_due(self.last_beat, now) {
            self.last_beat = now;
            let _ = write_staging_marker(staging, now);
        }
    }

    fn add(&mut self, n: u64, force: bool) {
        self.beat();
        self.copied += n;
        if force || self.copied - self.reported >= EVENT_STEP {
            self.reported = self.copied;
            (self.on_event)(CopyEvent::Bytes {
                copied: self.copied,
                total: self.total,
            });
        }
    }
}

fn copy_file(src: &Path, dst: &Path, cancel: &AtomicBool, progress: &mut Progress<'_>) -> Res<()> {
    let mut input = File::open(src).map_err(|e| io_err(src, &e))?;
    let mut output = File::create(dst).map_err(|e| io_err(dst, &e))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(LeadDbsDataError::cancelled());
        }
        let n = input.read(&mut buf).map_err(|e| io_err(src, &e))?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n]).map_err(|e| io_err(dst, &e))?;
        progress.add(n as u64, false);
    }
    output.flush().map_err(|e| io_err(dst, &e))
}

fn copy_tree(src: &Path, dst: &Path, cancel: &AtomicBool, progress: &mut Progress<'_>) -> Res<u64> {
    let mut links = 0u64;
    std::fs::create_dir_all(dst).map_err(|e| io_err(dst, &e))?;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        if cancel.load(Ordering::SeqCst) {
            return Err(LeadDbsDataError::cancelled());
        }
        progress.beat();
        let rd = std::fs::read_dir(&from).map_err(|e| io_err(&from, &e))?;
        for entry in rd {
            let entry = entry.map_err(|e| io_err(&from, &e))?;
            let target = to.join(entry.file_name());
            match entry_kind(&entry).map_err(|e| io_err(&entry.path(), &e))? {
                Kind::File => copy_file(&entry.path(), &target, cancel, progress)?,
                Kind::Dir => {
                    std::fs::create_dir_all(&target).map_err(|e| io_err(&target, &e))?;
                    stack.push((entry.path(), target));
                }
                Kind::Link => links += 1,
                Kind::Skip => {}
            }
        }
    }
    Ok(links)
}

fn exists_any(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Copies the selected items into the destinations (`specs/17` §3.2). Never writes in the source.
pub fn copy(
    inv: &Inventory,
    selection: &Selection,
    dest: &Destinations,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(CopyEvent),
) -> Res<CopyOutcome> {
    let mut jobs: Vec<Job<'_>> = Vec::new();
    let mut atlas_names: Vec<&String> = Vec::new();
    for name in &selection.atlases {
        if !atlas_names.contains(&name) {
            atlas_names.push(name);
        }
    }
    let mut connectome_names: Vec<&(ConnectomeClass, String)> = Vec::new();
    for pair in &selection.connectomes {
        if !connectome_names.contains(&pair) {
            connectome_names.push(pair);
        }
    }
    for name in atlas_names {
        let item = inv
            .atlases
            .iter()
            .find(|a| a.name == *name)
            .ok_or_else(|| {
                LeadDbsDataError::new(
                    LeadDbsCode::Missing,
                    format!("the atlas {name} is not in the inventory"),
                )
            })?;
        jobs.push(Job {
            kind: DataKind::Atlas,
            name: &item.name,
            source: &item.path,
            dest: dest.atlases_dir.join(&item.name),
            bytes: item.bytes,
        });
    }
    for (class, name) in connectome_names {
        let item = inv
            .connectomes
            .iter()
            .find(|c| c.class == *class && c.name == *name)
            .ok_or_else(|| {
                LeadDbsDataError::new(
                    LeadDbsCode::Missing,
                    format!(
                        "the {} connectome {name} is not in the inventory",
                        class.dir_name()
                    ),
                )
            })?;
        jobs.push(Job {
            kind: DataKind::Connectome(*class),
            name: &item.name,
            source: &item.path,
            dest: dest
                .connectomes_root
                .join(class.dir_name())
                .join(&item.name),
            bytes: item.bytes,
        });
    }
    let skip: Vec<bool> = jobs.iter().map(|j| exists_any(&j.dest)).collect();
    let pending: Vec<&Job<'_>> = jobs
        .iter()
        .zip(&skip)
        .filter(|(_, s)| !**s)
        .map(|(j, _)| j)
        .collect();
    guard_destinations(inv, &jobs, dest)?;
    let notes = check_disk(&pending, dest)?;

    sweep_stale_staging(&dest.atlases_dir);
    for class in ConnectomeClass::ALL {
        sweep_stale_staging(&dest.connectomes_root.join(class.dir_name()));
    }

    let count = jobs.len();
    let total: u64 = pending
        .iter()
        .fold(0u64, |acc, j| acc.saturating_add(j.bytes));
    let mut outcome = CopyOutcome {
        copied: Vec::new(),
        skipped: jobs
            .iter()
            .zip(&skip)
            .filter(|(_, s)| **s)
            .map(|(j, _)| CopiedItem {
                kind: j.kind,
                name: j.name.to_owned(),
                path: j.dest.clone(),
                bytes: j.bytes,
                skipped_links: 0,
            })
            .collect(),
        atlases_dir: None,
        notes,
    };
    let mut progress = Progress {
        copied: 0,
        reported: 0,
        total,
        on_event,
        staging: None,
        last_beat: 0,
    };
    let result = copy_jobs(&jobs, &skip, count, cancel, &mut progress, &mut outcome);
    if outcome.copied.iter().any(|c| c.kind == DataKind::Atlas) {
        outcome.atlases_dir = Some(dest.atlases_dir.clone());
    }
    match result {
        Ok(()) => {
            (progress.on_event)(CopyEvent::AllDone);
            Ok(outcome)
        }
        Err(e) => Err(e.with_partial(outcome)),
    }
}

fn copy_jobs(
    jobs: &[Job<'_>],
    skip: &[bool],
    count: usize,
    cancel: &AtomicBool,
    progress: &mut Progress<'_>,
    outcome: &mut CopyOutcome,
) -> Res<()> {
    for (index, job) in jobs.iter().enumerate() {
        if skip[index] {
            (progress.on_event)(CopyEvent::ItemSkipped {
                kind: job.kind,
                name: job.name.to_owned(),
            });
            continue;
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(LeadDbsDataError::cancelled());
        }
        (progress.on_event)(CopyEvent::ItemStarted {
            kind: job.kind,
            name: job.name.to_owned(),
            index,
            count,
        });
        let parent = job.dest.parent().unwrap_or(&job.dest).to_path_buf();
        std::fs::create_dir_all(&parent).map_err(|e| io_err(&parent, &e))?;
        let staging = parent.join(format!("{STAGING_PREFIX}{}", job.name));
        let now = unix_now();
        prepare_staging(&staging, job.name, now)?;
        progress.staging = Some(staging.clone());
        progress.last_beat = now;
        let result = copy_tree(job.source, &staging, cancel, progress).and_then(|links| {
            progress.staging = None;
            let marker = staging.join(STAGING_MARKER);
            std::fs::remove_file(&marker).map_err(|e| io_err(&marker, &e))?;
            std::fs::rename(&staging, &job.dest).map_err(|e| io_err(&job.dest, &e))?;
            Ok(links)
        });
        progress.staging = None;
        let links = match result {
            Ok(links) => links,
            Err(e) => {
                if remove_dir_with_retries(&staging).is_err() {
                    outcome.notes.push(removal_note(&staging, job.name));
                }
                return Err(e);
            }
        };
        progress.add(0, true);
        (progress.on_event)(CopyEvent::ItemDone {
            kind: job.kind,
            name: job.name.to_owned(),
        });
        outcome.copied.push(CopiedItem {
            kind: job.kind,
            name: job.name.to_owned(),
            path: job.dest.clone(),
            bytes: job.bytes,
            skipped_links: links,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
