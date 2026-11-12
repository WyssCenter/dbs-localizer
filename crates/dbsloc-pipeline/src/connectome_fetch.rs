// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Connectome roots, discovery, polite download, install, licence records and import
//! (`specs/15` §3.1, §4.3 to §4.7, §4.9, §11).

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::connectome_archive::{
    archive_kind, archive_stem, extract_tar, extract_tar_file, extract_zip, map_io, safe_relative,
    zip_uncompressed_bytes, ArchiveKind,
};
use crate::connectome_catalog::{
    catalogue, lookup, match_installed, Access, ConnectomeClass, ConnectomeEntry, ConnectomeFile,
    Install, Source, DATAVERSE_BASE_URL, FIGSHARE_BASE_URL, GSP_DOI, LEADDBS_RELEASE_URL,
    REFERENCE_SPACE,
};
use crate::md5::Md5;
use crate::sha1::Sha1;
use crate::sha256::{hex, sha256_hex, Sha256};
use crate::template_catalog::user_data_dir;
use crate::template_fetch::{
    download_agent_with, is_cancelled, replace_dir, AgentStyle, CHUNK, PROGRESS_STEP,
};
use crate::version::{utc_now_rfc3339, VERSION};

pub const CONNECTOMES_DIR_ENV: &str = "DBSLOC_CONNECTOMES_DIR";
pub const LEADDBS_CONNECTOMES_ENV: &str = "DBSLOC_LEADDBS_CONNECTOMES_DIR";
pub const LOCK_FILE: &str = ".download.lock";
pub const INSTALL_RECORD: &str = ".dbsloc-install.json";
pub const REGISTRY_FILE: &str = "registry.json";
const PARTIAL: &str = ".partial-";
const PARTIAL_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
const RETRY_AFTER_CAP: Duration = Duration::from_secs(3600);
const UNREADABLE_LOCK_GRACE: Duration = Duration::from_secs(10);
const GB: u64 = 1_000_000_000;
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Error codes of `specs/15` §11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Missing,
    Format,
    Terms,
    Busy,
    Host,
    Size,
    Changed,
    Checksum,
    Disk,
    Archive,
    Io,
    Usage,
    Cancelled,
}

impl ErrorCode {
    pub fn code(self) -> &'static str {
        match self {
            ErrorCode::Missing => "E_CONN_MISSING",
            ErrorCode::Format => "E_CONN_FORMAT",
            ErrorCode::Terms => "E_CONN_TERMS",
            ErrorCode::Busy => "E_CONN_BUSY",
            ErrorCode::Host => "E_CONN_HOST",
            ErrorCode::Size => "E_CONN_SIZE",
            ErrorCode::Changed => "E_CONN_CHANGED",
            ErrorCode::Checksum => "E_CONN_CHECKSUM",
            ErrorCode::Disk => "E_CONN_DISK",
            ErrorCode::Archive => "E_CONN_ARCHIVE",
            ErrorCode::Io => "E_CONN_IO",
            ErrorCode::Usage => "E_CONN_USAGE",
            ErrorCode::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectomeError {
    pub code: ErrorCode,
    pub message: String,
}

impl ConnectomeError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ConnectomeError {
            code,
            message: message.into(),
        }
    }

    pub fn cancelled() -> Self {
        ConnectomeError::new(ErrorCode::Cancelled, "cancelled")
    }

    pub fn is_cancelled(&self) -> bool {
        self.code == ErrorCode::Cancelled
    }
}

impl std::fmt::Display for ConnectomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_cancelled() {
            f.write_str("cancelled")
        } else {
            write!(f, "{}: {}", self.code.code(), self.message)
        }
    }
}

impl std::error::Error for ConnectomeError {}

type Res<T> = Result<T, ConnectomeError>;

fn host_err(message: impl Into<String>) -> ConnectomeError {
    ConnectomeError::new(ErrorCode::Host, message)
}

fn io_fail(what: &str, e: &std::io::Error) -> ConnectomeError {
    map_io(what, e)
}

static EXTERNAL_ROOT_SETTING: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Remembers the *Lead-DBS connectomes folder* setting; [`ConnectomeRoots::resolve`] prefers it
/// over `DBSLOC_LEADDBS_CONNECTOMES_DIR`. `None` or an empty path clears it.
pub fn set_external_root_setting(dir: Option<PathBuf>) {
    let dir = dir.filter(|p| !p.as_os_str().is_empty());
    let mut guard = EXTERNAL_ROOT_SETTING
        .write()
        .unwrap_or_else(|p| p.into_inner());
    if *guard != dir {
        *guard = dir;
    }
}

fn external_root_setting() -> Option<PathBuf> {
    EXTERNAL_ROOT_SETTING
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// The connectome folders: one writable primary root and an optional read-only Lead-DBS root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectomeRoots {
    pub primary: PathBuf,
    pub external: Option<PathBuf>,
}

impl ConnectomeRoots {
    pub fn from_primary(primary: impl Into<PathBuf>) -> Self {
        ConnectomeRoots {
            primary: primary.into(),
            external: None,
        }
    }

    /// Primary root: `flag`, else `DBSLOC_CONNECTOMES_DIR`, else `<data_dir>/connectomes`;
    /// external root: `external_setting`, else `DBSLOC_LEADDBS_CONNECTOMES_DIR`.
    /// Empty values count as unset.
    pub fn resolve_with(
        flag: Option<&Path>,
        lookup_env: &dyn Fn(&str) -> Option<String>,
        data_dir: Option<PathBuf>,
        external_setting: Option<&Path>,
    ) -> Res<Self> {
        let env = |name: &str| {
            lookup_env(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let primary = flag
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .or_else(|| env(CONNECTOMES_DIR_ENV))
            .or_else(|| data_dir.map(|d| d.join("connectomes")))
            .ok_or_else(|| {
                ConnectomeError::new(
                    ErrorCode::Usage,
                    format!("no connectomes folder; pass --root or set {CONNECTOMES_DIR_ENV}"),
                )
            })?;
        Ok(ConnectomeRoots {
            primary: crate::paths::absolute(&primary),
            external: external_setting
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .or_else(|| env(LEADDBS_CONNECTOMES_ENV))
                .map(|p| crate::paths::absolute(&p)),
        })
    }

    /// [`resolve_with`](Self::resolve_with) over the process environment, the user data folder
    /// and the setting given to [`set_external_root_setting`].
    pub fn resolve(flag: Option<&Path>) -> Res<Self> {
        Self::resolve_with(
            flag,
            &|n| std::env::var(n).ok(),
            user_data_dir(),
            external_root_setting().as_deref(),
        )
    }

    pub fn class_dir(&self, class: ConnectomeClass) -> PathBuf {
        self.primary.join(class.dir_name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    Primary,
    Registered,
    External,
}

impl RootKind {
    pub fn label(self) -> &'static str {
        match self {
            RootKind::Primary => "primary",
            RootKind::Registered => "registered",
            RootKind::External => "external",
        }
    }
}

/// One entry of `<primary>/registry.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub path: String,
    pub class: String,
    pub id: String,
    pub registered_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

pub fn read_registry(primary: &Path) -> Res<Vec<RegistryEntry>> {
    let path = primary.join(REGISTRY_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_fail("cannot read registry.json", &e)),
    };
    serde_json::from_str(&text).map_err(|e| {
        ConnectomeError::new(
            ErrorCode::Format,
            format!("{} is not a valid registry: {e}", path.display()),
        )
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Res<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_fail("cannot create a folder", &e))?;
    }
    let n = STAGING_COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| io_fail("cannot write a file", &e))?;
    std::fs::rename(&tmp, path).map_err(|e| io_fail("cannot write a file", &e))
}

fn write_json(path: &Path, value: &Value) -> Res<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| ConnectomeError::new(ErrorCode::Io, e.to_string()))?;
    write_atomic(path, text.as_bytes())
}

fn write_registry(primary: &Path, rows: &[RegistryEntry]) -> Res<()> {
    let value = serde_json::to_value(rows)
        .map_err(|e| ConnectomeError::new(ErrorCode::Io, e.to_string()))?;
    write_json(&primary.join(REGISTRY_FILE), &value)
}

fn same_path(a: &str, b: &Path) -> bool {
    let a = Path::new(a);
    a == b
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

/// Adds (or replaces) a registered folder.
pub fn register_folder(primary: &Path, entry: RegistryEntry) -> Res<()> {
    let mut rows = read_registry(primary)?;
    rows.retain(|r| !same_path(&r.path, Path::new(&entry.path)));
    rows.push(entry);
    write_registry(primary, &rows)
}

/// Removes a registered folder; the folder itself is never touched.
pub fn forget_folder(primary: &Path, path: &Path) -> Res<RegistryEntry> {
    let mut rows = read_registry(primary)?;
    let Some(i) = rows.iter().position(|r| same_path(&r.path, path)) else {
        return Err(ConnectomeError::new(
            ErrorCode::Missing,
            format!("{} is not a registered connectome folder", path.display()),
        ));
    };
    let gone = rows.remove(i);
    write_registry(primary, &rows)?;
    Ok(gone)
}

/// A connectome folder found in a root.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledConnectome {
    pub name: String,
    pub id: String,
    pub class: ConnectomeClass,
    pub path: PathBuf,
    pub root_kind: RootKind,
    pub catalogue_id: Option<&'static str>,
    pub tag: Option<String>,
    pub space: String,
    pub version: Option<String>,
    pub subsets: Vec<String>,
    pub fmri_type: Option<String>,
    pub missing: bool,
    pub duplicate_of: Option<PathBuf>,
    pub licence_record: Option<PathBuf>,
}

fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|b| char::from(*b)).collect(),
    }
}

/// `dataset_info.json` of a folder, decoded as UTF-8 with a Windows-1252 style fallback.
pub fn read_dataset_info(folder: &Path) -> Option<Value> {
    let bytes = std::fs::read(folder.join("dataset_info.json")).ok()?;
    let text = decode_text(&bytes);
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn read_install_record(folder: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(folder.join(INSTALL_RECORD)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether a folder holds a connectome of `class` (`specs/15` §3.1).
pub fn folder_has_marker(folder: &Path, class: ConnectomeClass) -> bool {
    match class {
        ConnectomeClass::Dmri => ["data.mat", "data.trk", "data.trk.gz"]
            .iter()
            .any(|f| folder.join(f).is_file()),
        ConnectomeClass::Fmri => folder.join("dataset_info.json").is_file(),
        ConnectomeClass::DmriMultiTract => std::fs::read_dir(folder).is_ok_and(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                n.ends_with(".mat") && e.path().is_file()
            })
        }),
    }
}

/// Class of a single connectome folder, from the files it holds.
pub fn infer_class(folder: &Path) -> Option<ConnectomeClass> {
    [ConnectomeClass::Dmri, ConnectomeClass::Fmri]
        .into_iter()
        .find(|c| folder_has_marker(folder, *c))
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn describe(
    path: &Path,
    class: ConnectomeClass,
    kind: RootKind,
    primary: &Path,
    registry_space: Option<&str>,
) -> InstalledConnectome {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let info = if class == ConnectomeClass::Fmri {
        read_dataset_info(path)
    } else {
        read_dataset_info(path).filter(|v| v.is_object())
    };
    let tag = info
        .as_ref()
        .and_then(|v| v["tag"].as_str())
        .map(str::to_owned);
    let subsets = info
        .as_ref()
        .and_then(|v| v["subsets"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let fmri_type = info
        .as_ref()
        .and_then(|v| v["type"].as_str())
        .map(str::to_owned);
    let record = read_install_record(path);
    let is_variant = record.as_ref().is_some_and(|r| r["variant_of"].is_string());
    let entry = if is_variant {
        None
    } else {
        record
            .as_ref()
            .and_then(|r| r["id"].as_str())
            .and_then(lookup)
            .or_else(|| match_installed(class, &name, tag.as_deref()))
    };
    let space = record
        .as_ref()
        .and_then(|r| r["space"].as_str())
        .or(registry_space)
        .or(entry.map(|e| e.space))
        .unwrap_or(REFERENCE_SPACE)
        .to_owned();
    let id = entry.map_or_else(|| sanitize_id(&name), |e| e.id.to_owned());
    let licence = licence_record_path(primary, &id);
    InstalledConnectome {
        name,
        id,
        class,
        path: path.to_path_buf(),
        root_kind: kind,
        catalogue_id: entry.map(|e| e.id),
        tag,
        space,
        version: record
            .as_ref()
            .and_then(|r| r["version"].as_str())
            .map(str::to_owned),
        subsets,
        fmri_type,
        missing: false,
        duplicate_of: None,
        licence_record: licence.is_file().then_some(licence),
    }
}

/// Folder name reduced to `[A-Za-z0-9._-]` for ids and file names.
pub fn sanitize_id(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.trim_matches('_').chars().take(128).collect()
}

fn scan_root(root: &Path, kind: RootKind, primary: &Path, out: &mut Vec<InstalledConnectome>) {
    for class in ConnectomeClass::ALL {
        let Ok(rd) = std::fs::read_dir(root.join(class.dir_name())) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| !is_hidden(&e.file_name().to_string_lossy()) && e.path().is_dir())
            .map(|e| e.path())
            .collect();
        dirs.sort();
        for dir in dirs {
            if folder_has_marker(&dir, class) {
                out.push(describe(&dir, class, kind, primary, None));
            }
        }
    }
}

/// Every connectome in the primary root, the registered folders and the external root, in that
/// priority order; a later duplicate of a (class, name) pair names the copy that wins.
pub fn discover_connectomes(roots: &ConnectomeRoots) -> Vec<InstalledConnectome> {
    let mut out = Vec::new();
    scan_root(&roots.primary, RootKind::Primary, &roots.primary, &mut out);
    for row in read_registry(&roots.primary).unwrap_or_default() {
        let Some(class) = ConnectomeClass::parse(&row.class) else {
            continue;
        };
        let path = PathBuf::from(&row.path);
        if path.is_dir() {
            out.push(describe(
                &path,
                class,
                RootKind::Registered,
                &roots.primary,
                row.space.as_deref(),
            ));
        } else {
            let mut gone = describe(&path, class, RootKind::Registered, &roots.primary, None);
            gone.missing = true;
            gone.id = row.id.clone();
            out.push(gone);
        }
    }
    if let Some(ext) = &roots.external {
        scan_root(ext, RootKind::External, &roots.primary, &mut out);
    }
    let mut seen: Vec<(ConnectomeClass, String, PathBuf)> = Vec::new();
    for c in &mut out {
        if c.missing {
            continue;
        }
        match seen.iter().find(|(k, n, _)| *k == c.class && *n == c.name) {
            Some((_, _, winner)) => c.duplicate_of = Some(winner.clone()),
            None => seen.push((c.class, c.name.clone(), c.path.clone())),
        }
    }
    out
}

/// Where a catalogue entry stands for the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectomeState {
    Installed(PathBuf),
    Partial { percent: u8, bytes: u64 },
    Available { size: u64 },
}

impl ConnectomeState {
    pub fn label(&self) -> &'static str {
        match self {
            ConnectomeState::Installed(_) => "installed",
            ConnectomeState::Partial { .. } => "partial",
            ConnectomeState::Available { .. } => "available",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectomeStatus {
    pub entry: &'static ConnectomeEntry,
    pub state: ConnectomeState,
    pub licence_record: Option<PathBuf>,
}

fn partial_dir(primary: &Path, entry: &ConnectomeEntry) -> PathBuf {
    primary
        .join(entry.class.dir_name())
        .join(format!("{PARTIAL}{}", entry.id))
}

fn progress_path(dir: &Path) -> PathBuf {
    dir.join("progress.json")
}

fn read_progress(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(progress_path(dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["done"].as_array().cloned())
        .unwrap_or_default()
}

/// Bytes already on disk for an interrupted fetch of `entry` (partial files plus finished files).
pub fn partial_bytes(primary: &Path, entry: &ConnectomeEntry) -> u64 {
    let dir = partial_dir(primary, entry);
    let done: u64 = read_progress(&dir)
        .iter()
        .filter_map(|v| v["bytes"].as_u64())
        .sum();
    let parts: u64 = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    done + parts
}

/// One row per catalogue entry: installed, partially downloaded or available.
pub fn list_catalogue(
    roots: &ConnectomeRoots,
    installed: &[InstalledConnectome],
) -> Vec<ConnectomeStatus> {
    catalogue()
        .iter()
        .map(|e| {
            let found = installed
                .iter()
                .find(|c| c.catalogue_id == Some(e.id) && !c.missing && c.duplicate_of.is_none());
            let bytes = partial_bytes(&roots.primary, e);
            let state = match found {
                Some(c) => ConnectomeState::Installed(c.path.clone()),
                None if bytes > 0 => ConnectomeState::Partial {
                    percent: u8::try_from(
                        (bytes.saturating_mul(100) / e.total_bytes().max(1)).min(100),
                    )
                    .unwrap_or(100),
                    bytes,
                },
                None => ConnectomeState::Available {
                    size: e.total_bytes(),
                },
            };
            let rec = licence_record_path(&roots.primary, e.id);
            ConnectomeStatus {
                entry: e,
                state,
                licence_record: rec.is_file().then_some(rec),
            }
        })
        .collect()
}

pub fn licence_record_path(primary: &Path, id: &str) -> PathBuf {
    primary.join("licenses").join(format!("{id}.json"))
}

/// SHA-256 of the licence text shown for `entry`, with line endings normalised to LF so a
/// build from a CRLF checkout hashes the same text.
pub fn licence_text_sha256(entry: &ConnectomeEntry) -> String {
    sha256_hex(entry.licence_text.replace("\r\n", "\n").as_bytes())
}

fn os_user() -> String {
    ["USERNAME", "USER", "LOGNAME"]
        .iter()
        .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Writes `<primary>/licenses/<id>.json` (`specs/15` §4.7); an earlier record is kept as
/// `<id>.<timestamp>.json`.
pub fn write_licence_record(
    primary: &Path,
    entry: &ConnectomeEntry,
    accepted_via: &str,
    attestation: Option<&str>,
) -> Res<PathBuf> {
    let path = licence_record_path(primary, entry.id);
    let now = utc_now_rfc3339();
    if path.is_file() {
        let stamp: String = now.chars().filter(char::is_ascii_alphanumeric).collect();
        let old = path.with_file_name(format!("{}.{stamp}.json", entry.id));
        std::fs::rename(&path, &old).map_err(|e| io_fail("cannot archive the old record", &e))?;
    }
    let record = json!({
        "id": entry.id,
        "dataset": entry.name,
        "version": entry.version,
        "access": entry.access.label(),
        "licence_name": entry.licence_name,
        "licence_url": entry.licence_url,
        "licence_text_sha256": licence_text_sha256(entry),
        "terms_fetched": entry.terms_fetched,
        "attestation": attestation,
        "accepted_by": os_user(),
        "accepted_at": now,
        "accepted_via": accepted_via,
        "product_version": crate::version::version_string(),
    });
    write_json(&path, &record)?;
    Ok(path)
}

pub fn read_licence_record(primary: &Path, id: &str) -> Option<Value> {
    let text = std::fs::read_to_string(licence_record_path(primary, id)).ok()?;
    serde_json::from_str(&text).ok()
}

/// A record is stale when its licence text hash differs from the current catalogue text.
pub fn licence_record_is_stale(record: &Value, entry: &ConnectomeEntry) -> bool {
    record["licence_text_sha256"].as_str() != Some(licence_text_sha256(entry).as_str())
}

/// What the user accepted: the terms, and for `dua` entries the attestation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acceptance {
    pub accept_terms: bool,
    pub attest_data_use_agreement: bool,
    pub via: &'static str,
}

/// Refuses a fetch or import record without acceptance, before any request is made.
pub fn check_acceptance(entry: &ConnectomeEntry, a: &Acceptance) -> Res<()> {
    if !a.accept_terms {
        return Err(ConnectomeError::new(
            ErrorCode::Terms,
            format!("the terms of {} are not accepted", entry.id),
        ));
    }
    if entry.access == Access::Dua && !a.attest_data_use_agreement {
        return Err(ConnectomeError::new(
            ErrorCode::Terms,
            format!(
                "{} needs your own approved data use agreement; attest it or import a folder you obtained",
                entry.id
            ),
        ));
    }
    Ok(())
}

/// The refusal of a download for an import-only (`dua`) entry (`specs/15` §4.3).
pub fn import_only_error(entry: &ConnectomeEntry) -> ConnectomeError {
    ConnectomeError::new(
        ErrorCode::Terms,
        format!(
            "{} is import-only: obtain it with your own data access and import it",
            entry.id
        ),
    )
}

fn attestation_for(entry: &ConnectomeEntry, a: &Acceptance) -> Option<String> {
    (entry.access == Access::Dua && a.attest_data_use_agreement)
        .then(|| entry.attestation_sentence())
}

/// `dbsloc/<version> (Wyss DBS Localizer; research software)`, sent on every request.
pub fn user_agent() -> String {
    format!("dbsloc/{VERSION} (Wyss DBS Localizer; research software)")
}

#[derive(Debug, Clone)]
pub struct FetchSettings {
    pub leaddbs_release_url: String,
    pub dataverse_base_url: String,
    pub figshare_base_url: String,
    pub max_rate_bytes_per_sec: Option<u64>,
    pub retry_delays: Vec<Duration>,
    pub user_agent: String,
    pub disk_slack_bytes: u64,
    pub free_space_override: Option<u64>,
}

impl Default for FetchSettings {
    fn default() -> Self {
        FetchSettings {
            leaddbs_release_url: LEADDBS_RELEASE_URL.to_owned(),
            dataverse_base_url: DATAVERSE_BASE_URL.to_owned(),
            figshare_base_url: FIGSHARE_BASE_URL.to_owned(),
            max_rate_bytes_per_sec: None,
            retry_delays: vec![Duration::from_secs(60), Duration::from_secs(300)],
            user_agent: user_agent(),
            disk_slack_bytes: GB,
            free_space_override: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ConnectomeEvent {
    Started {
        id: String,
        total_bytes: u64,
        files: usize,
    },
    FileStarted {
        index: usize,
        count: usize,
        file_name: String,
    },
    Progress {
        downloaded: u64,
        total: u64,
    },
    Retry {
        attempt: usize,
        wait_secs: u64,
        reason: String,
    },
    Verifying,
    Extracting,
    Done {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    pub path: PathBuf,
    pub folder: String,
    pub files: Vec<FileRecord>,
    pub licence_record: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub file_name: String,
    pub bytes: u64,
    pub source_digest: String,
    pub sha256: String,
}

fn hostname() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(windows)]
fn pid_running(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .creation_flags(0x0800_0000)
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
}

#[cfg(not(windows))]
fn pid_running(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The one-transfer-at-a-time lock of a connectome root; removed on drop.
#[derive(Debug)]
pub struct DownloadLock {
    path: PathBuf,
}

impl DownloadLock {
    pub fn acquire(primary: &Path, id: &str) -> Res<DownloadLock> {
        Self::acquire_labelled(primary, id, "connectomes", "connectome")
    }

    pub fn acquire_labelled(
        primary: &Path,
        id: &str,
        folder: &str,
        item: &str,
    ) -> Res<DownloadLock> {
        std::fs::create_dir_all(primary)
            .map_err(|e| io_fail(&format!("cannot create the {folder} folder"), &e))?;
        let path = primary.join(LOCK_FILE);
        let me = hostname();
        for _ in 0..3 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let body = json!({
                        "pid": std::process::id(),
                        "host": me,
                        "started_at": utc_now_rfc3339(),
                        "id": id,
                    });
                    f.write_all(body.to_string().as_bytes())
                        .map_err(|e| io_fail("cannot write the download lock", &e))?;
                    return Ok(DownloadLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let held: Option<Value> = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|t| serde_json::from_str(&t).ok());
                    let stale = match &held {
                        None => std::fs::metadata(&path)
                            .and_then(|m| m.modified())
                            .ok()
                            .is_none_or(|t| match t.elapsed() {
                                Ok(age) => age >= UNREADABLE_LOCK_GRACE,
                                Err(ahead) => ahead.duration() > UNREADABLE_LOCK_GRACE,
                            }),
                        Some(v) => {
                            v["host"].as_str() == Some(me.as_str())
                                && v["pid"]
                                    .as_u64()
                                    .and_then(|p| u32::try_from(p).ok())
                                    .is_some_and(|p| !pid_running(p))
                        }
                    };
                    if !stale {
                        let what = held
                            .as_ref()
                            .and_then(|v| v["id"].as_str())
                            .map_or_else(|| format!("another {item}"), str::to_owned);
                        return Err(ConnectomeError::new(
                            ErrorCode::Busy,
                            format!("another download is running ({what}); wait for it to finish"),
                        ));
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(io_fail("cannot take the download lock", &e)),
            }
        }
        Err(ConnectomeError::new(
            ErrorCode::Busy,
            "cannot take the download lock",
        ))
    }
}

impl Drop for DownloadLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn newest_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut best = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                best = Some(best.map_or(t, |b| b.max(t)));
            }
        }
    }
    best
}

/// Removes `.partial-<id>` folders untouched for `max_age` (30 days by default in a fetch).
pub fn sweep_old_partials(primary: &Path, max_age: Duration) {
    for class in ConnectomeClass::ALL {
        let Ok(rd) = std::fs::read_dir(primary.join(class.dir_name())) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(PARTIAL) || !e.path().is_dir() {
                continue;
            }
            let old = newest_mtime(&e.path())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= max_age);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

enum SourceHasher {
    Sha1(Sha1),
    Md5(Md5),
}

struct Hashers {
    sha256: Sha256,
    source: SourceHasher,
}

struct Digests {
    sha256: String,
    source: String,
}

fn split_digest(pin: &str) -> (&str, &str) {
    pin.split_once(':').unwrap_or(("", pin))
}

impl Hashers {
    fn new(pin: &str) -> Hashers {
        let source = if split_digest(pin).0 == "md5" {
            SourceHasher::Md5(Md5::new())
        } else {
            SourceHasher::Sha1(Sha1::new())
        };
        Hashers {
            sha256: Sha256::new(),
            source,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.sha256.update(data);
        match &mut self.source {
            SourceHasher::Sha1(h) => h.update(data),
            SourceHasher::Md5(h) => h.update(data),
        }
    }

    fn finish(self) -> Digests {
        let source = match self.source {
            SourceHasher::Sha1(h) => hex(&h.finish()),
            SourceHasher::Md5(h) => hex(&h.finish()),
        };
        Digests {
            sha256: hex(&self.sha256.finish()),
            source,
        }
    }
}

enum DlError {
    Cancelled,
    Restart,
    Gone(String),
    Retryable {
        message: String,
        retry_after: Option<Duration>,
    },
    Fatal(ConnectomeError),
}

fn net_err(e: &ureq::Error, url: &str) -> DlError {
    match e {
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => {
            DlError::Fatal(host_err(format!("bad request URL {url}: {e}")))
        }
        _ => DlError::Retryable {
            message: e.to_string(),
            retry_after: None,
        },
    }
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?']).next().unwrap_or(rest)
}

fn absolute_location(base: &str, location: &str) -> Result<String, DlError> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Ok(location.to_owned());
    }
    if location.starts_with('/') {
        let scheme = base.split_once("://").map_or("https", |(s, _)| s);
        return Ok(format!("{scheme}://{}{location}", host_of(base)));
    }
    Err(DlError::Fatal(host_err(format!(
        "unusable redirect target {location:?} from {base}"
    ))))
}

fn header(resp: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
}

fn retry_after(resp: &ureq::http::Response<ureq::Body>) -> Option<Duration> {
    header(resp, "retry-after")
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn style(settings: &FetchSettings) -> AgentStyle {
    AgentStyle {
        user_agent: Some(settings.user_agent.clone()),
        manual_redirects: true,
    }
}

fn classify_status(
    status: u16,
    resp: &ureq::http::Response<ureq::Body>,
    url: &str,
) -> Option<DlError> {
    match status {
        404 | 410 => Some(DlError::Gone(format!("{url} answered HTTP {status}"))),
        408 | 429 | 500 | 502 | 503 | 504 => Some(DlError::Retryable {
            message: format!("{} answered HTTP {status}", host_of(url)),
            retry_after: retry_after(resp),
        }),
        s if !(200..300).contains(&s) && !(300..400).contains(&s) => Some(DlError::Fatal(
            host_err(format!("{} answered HTTP {s}", host_of(url))),
        )),
        _ => None,
    }
}

fn resolve_url(settings: &FetchSettings, source: &Source) -> Result<String, DlError> {
    let url = match source {
        Source::LeadDbs { id } => format!("{}?id={id}", settings.leaddbs_release_url),
        Source::Dataverse { file_id } => format!(
            "{}/api/access/datafile/{file_id}",
            settings.dataverse_base_url.trim_end_matches('/')
        ),
        Source::Figshare { file_id } => format!(
            "{}/{file_id}",
            settings.figshare_base_url.trim_end_matches('/')
        ),
    };
    let resp = download_agent_with(0, &style(settings))
        .get(&url)
        .header("Accept-Encoding", "identity")
        .call()
        .map_err(|e| net_err(&e, &url))?;
    let status = resp.status().as_u16();
    if let Some(err) = classify_status(status, &resp, &url) {
        return Err(err);
    }
    if (300..400).contains(&status) {
        let loc = header(&resp, "location").ok_or_else(|| {
            DlError::Fatal(host_err(format!("{url} redirected without a Location")))
        })?;
        return absolute_location(&url, &loc);
    }
    Ok(url)
}

struct Job<'a> {
    settings: &'a FetchSettings,
    cancel: &'a AtomicBool,
    on_event: &'a mut dyn FnMut(ConnectomeEvent),
    base_done: u64,
    total: u64,
}

fn sleep_poll(wait: Duration, cancel: &AtomicBool) -> Res<()> {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if is_cancelled(cancel) {
            return Err(ConnectomeError::cancelled());
        }
        std::thread::sleep(Duration::from_millis(50).min(end - Instant::now()));
    }
    Ok(())
}

fn remove_part(part: &Path, meta: &Path) {
    let _ = std::fs::remove_file(part);
    let _ = std::fs::remove_file(meta);
}

fn write_sidecar(
    meta: &Path,
    url: &str,
    etag: Option<&str>,
    modified: Option<&str>,
    bytes: u64,
    pin: &str,
) {
    let v = json!({
        "url": url,
        "etag": etag,
        "last_modified": modified,
        "bytes": bytes,
        "source_digest": pin,
    });
    let _ = std::fs::write(meta, v.to_string());
}

fn sidecar_validator(meta: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(meta).ok()?).ok()?;
    v["etag"]
        .as_str()
        .or_else(|| v["last_modified"].as_str())
        .map(str::to_owned)
}

fn hash_existing(part: &Path, hashers: &mut Hashers, cancel: &AtomicBool) -> Result<(), DlError> {
    let mut f = File::open(part)
        .map_err(|e| DlError::Fatal(io_fail("cannot read the partial file", &e)))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if is_cancelled(cancel) {
            return Err(DlError::Cancelled);
        }
        let n = f
            .read(&mut buf)
            .map_err(|e| DlError::Fatal(io_fail("cannot read the partial file", &e)))?;
        if n == 0 {
            return Ok(());
        }
        hashers.update(&buf[..n]);
    }
}

fn parse_content_range(v: &str) -> Option<(u64, u64)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()?))
}

fn changed(file: &ConnectomeFile, what: &str) -> DlError {
    DlError::Fatal(ConnectomeError::new(
        ErrorCode::Changed,
        format!(
            "the server copy of {} changed ({what}); update Wyss DBS Localizer",
            file.file_name
        ),
    ))
}

fn attempt(
    job: &mut Job<'_>,
    file: &ConnectomeFile,
    url: &str,
    part: &Path,
    meta: &Path,
) -> Result<Digests, DlError> {
    let mut hashers = Hashers::new(file.source_digest);
    let mut existing = std::fs::metadata(part).map_or(0, |m| m.len());
    if existing > file.bytes {
        remove_part(part, meta);
        existing = 0;
    }
    if existing > 0 {
        hash_existing(part, &mut hashers, job.cancel)?;
        if existing == file.bytes {
            return Ok(hashers.finish());
        }
    }
    let agent = download_agent_with(file.bytes, &style(job.settings));
    let mut req = agent.get(url).header("Accept-Encoding", "identity");
    if existing > 0 {
        req = req.header("Range", format!("bytes={existing}-"));
        if let Some(v) = sidecar_validator(meta) {
            req = req.header("If-Range", v);
        }
    }
    let resp = req.call().map_err(|e| net_err(&e, url))?;
    let status = resp.status().as_u16();
    if status == 416 {
        remove_part(part, meta);
        return Err(DlError::Restart);
    }
    if let Some(err) = classify_status(status, &resp, url) {
        return Err(err);
    }
    if status != 200 && status != 206 {
        return Err(DlError::Fatal(host_err(format!(
            "{} answered HTTP {status}",
            host_of(url)
        ))));
    }
    if let Some(oc) = header(&resp, "oc-checksum") {
        let pin = split_digest(file.source_digest);
        if pin.0 == "sha1" {
            let theirs = oc.split_whitespace().find_map(|t| {
                t.to_ascii_lowercase()
                    .strip_prefix("sha1:")
                    .map(str::to_owned)
            });
            if theirs.is_some_and(|t| t != pin.1) {
                return Err(changed(file, "its checksum differs from the pinned one"));
            }
        }
    }
    if status == 200 {
        if existing > 0 {
            existing = 0;
            hashers = Hashers::new(file.source_digest);
        }
        if let Some(len) = resp.body().content_length() {
            if len != file.bytes {
                return Err(changed(
                    file,
                    &format!("{len} bytes instead of {}", file.bytes),
                ));
            }
        }
    } else {
        let range = header(&resp, "content-range").and_then(|v| parse_content_range(&v));
        match range {
            Some((_, total)) if total != file.bytes => {
                return Err(changed(
                    file,
                    &format!("{total} bytes instead of {}", file.bytes),
                ));
            }
            Some((start, _)) if start == existing => {}
            _ => {
                return Err(DlError::Fatal(host_err(format!(
                    "{} answered a Range request with an unusable Content-Range",
                    host_of(url)
                ))))
            }
        }
    }
    let etag = header(&resp, "etag");
    let modified = header(&resp, "last-modified");
    write_sidecar(
        meta,
        url,
        etag.as_deref(),
        modified.as_deref(),
        existing,
        file.source_digest,
    );
    let mut out = if existing > 0 {
        std::fs::OpenOptions::new().append(true).open(part)
    } else {
        File::create(part)
    }
    .map_err(|e| DlError::Fatal(io_fail("cannot write the partial file", &e)))?;
    let mut reader = resp.into_body().into_with_config().limit(u64::MAX).reader();
    let mut buf = vec![0u8; CHUNK];
    let mut have = existing;
    let mut last_report = have;
    let started = Instant::now();
    let session_start = have;
    let outcome = loop {
        if is_cancelled(job.cancel) {
            break Err(DlError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) => {
                break Err(DlError::Retryable {
                    message: format!("connection interrupted: {e}"),
                    retry_after: None,
                })
            }
        };
        if have + n as u64 > file.bytes {
            let _ = out.flush();
            drop(out);
            remove_part(part, meta);
            return Err(DlError::Fatal(ConnectomeError::new(
                ErrorCode::Size,
                format!(
                    "{} is longer than the pinned {} bytes",
                    file.file_name, file.bytes
                ),
            )));
        }
        hashers.update(&buf[..n]);
        if let Err(e) = out.write_all(&buf[..n]) {
            break Err(DlError::Fatal(io_fail("cannot write the partial file", &e)));
        }
        have += n as u64;
        if have - last_report >= PROGRESS_STEP {
            last_report = have;
            (job.on_event)(ConnectomeEvent::Progress {
                downloaded: job.base_done + have,
                total: job.total,
            });
        }
        if let Some(rate) = job.settings.max_rate_bytes_per_sec.filter(|r| *r > 0) {
            let due = Duration::from_secs_f64((have - session_start) as f64 / rate as f64);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    };
    let _ = out.flush();
    drop(out);
    write_sidecar(
        meta,
        url,
        etag.as_deref(),
        modified.as_deref(),
        have,
        file.source_digest,
    );
    outcome?;
    (job.on_event)(ConnectomeEvent::Progress {
        downloaded: job.base_done + have,
        total: job.total,
    });
    if have < file.bytes {
        return Err(DlError::Retryable {
            message: format!("the connection closed after {have} of {} bytes", file.bytes),
            retry_after: None,
        });
    }
    Ok(hashers.finish())
}

fn fetch_file(job: &mut Job<'_>, file: &ConnectomeFile, dir: &Path) -> Res<FileRecord> {
    std::fs::create_dir_all(dir).map_err(|e| io_fail("cannot create the partial folder", &e))?;
    let part = dir.join(format!("{}.part", file.file_name));
    let meta = dir.join(format!("{}.part.json", file.file_name));
    let mut url: Option<String> = None;
    let mut retries = 0usize;
    let mut restarts = 0usize;
    let mut re_resolved = false;
    loop {
        if is_cancelled(job.cancel) {
            return Err(ConnectomeError::cancelled());
        }
        let step = match &url {
            Some(u) => attempt(job, file, u, &part, &meta),
            None => match resolve_url(job.settings, &file.source) {
                Ok(u) => {
                    url = Some(u);
                    continue;
                }
                Err(e) => Err(e),
            },
        };
        match step {
            Ok(d) => {
                (job.on_event)(ConnectomeEvent::Verifying);
                return verify_file(file, &part, &meta, &d);
            }
            Err(DlError::Cancelled) => return Err(ConnectomeError::cancelled()),
            Err(DlError::Restart) => {
                restarts += 1;
                if restarts > 2 {
                    return Err(host_err("the server keeps refusing the Range request"));
                }
            }
            Err(DlError::Gone(m)) => {
                if re_resolved {
                    return Err(host_err(m));
                }
                re_resolved = true;
                url = None;
            }
            Err(DlError::Fatal(e)) => return Err(e),
            Err(DlError::Retryable {
                message,
                retry_after,
            }) => {
                let Some(base) = job.settings.retry_delays.get(retries) else {
                    let host = url.as_deref().map_or_else(
                        || host_of(&job.settings.leaddbs_release_url).to_owned(),
                        |u| host_of(u).to_owned(),
                    );
                    return Err(host_err(format!("cannot reach {host}: {message}")));
                };
                let wait = (*base)
                    .max(retry_after.unwrap_or_default())
                    .min(RETRY_AFTER_CAP);
                (job.on_event)(ConnectomeEvent::Retry {
                    attempt: retries + 1,
                    wait_secs: wait.as_secs(),
                    reason: message,
                });
                sleep_poll(wait, job.cancel)?;
                retries += 1;
            }
        }
    }
}

fn verify_file(file: &ConnectomeFile, part: &Path, meta: &Path, d: &Digests) -> Res<FileRecord> {
    let size = std::fs::metadata(part).map_or(0, |m| m.len());
    if size != file.bytes {
        remove_part(part, meta);
        return Err(ConnectomeError::new(
            ErrorCode::Size,
            format!(
                "{} has {size} bytes, expected {}",
                file.file_name, file.bytes
            ),
        ));
    }
    let (_, want_source) = split_digest(file.source_digest);
    if d.source != want_source {
        remove_part(part, meta);
        return Err(ConnectomeError::new(
            ErrorCode::Checksum,
            format!(
                "{}: {} expected {want_source}, got {}",
                file.file_name,
                split_digest(file.source_digest).0.to_ascii_uppercase(),
                d.source
            ),
        ));
    }
    if !file.sha256.is_empty() && d.sha256 != file.sha256 {
        remove_part(part, meta);
        return Err(ConnectomeError::new(
            ErrorCode::Checksum,
            format!(
                "{}: SHA-256 expected {}, got {}",
                file.file_name, file.sha256, d.sha256
            ),
        ));
    }
    Ok(FileRecord {
        file_name: file.file_name.to_owned(),
        bytes: file.bytes,
        source_digest: file.source_digest.to_owned(),
        sha256: d.sha256.clone(),
    })
}

/// Free bytes the fetch of `entry` needs before the first byte (`specs/15` §4.6).
pub fn required_free_bytes(entry: &ConnectomeEntry, slack: u64) -> u64 {
    let total = entry.total_bytes();
    match entry.install {
        Install::ZipFolder { .. } => total
            .saturating_add(total + total / 50)
            .saturating_add(slack),
        Install::GspTars { .. } => {
            let largest = entry.files.iter().map(|f| f.bytes).max().unwrap_or(0);
            total.saturating_add(largest).saturating_add(slack)
        }
    }
}

fn check_disk(
    entry: &ConnectomeEntry,
    primary: &Path,
    settings: &FetchSettings,
    need: u64,
) -> Res<()> {
    let free = settings
        .free_space_override
        .or_else(|| crate::backdrop_fetch::free_space_bytes(primary));
    match free {
        Some(free) if free < need => Err(ConnectomeError::new(
            ErrorCode::Disk,
            format!(
                "not enough free disk space for {}: need {} GB, {} GB free",
                entry.id,
                need.div_ceil(GB),
                free / GB
            ),
        )),
        _ => Ok(()),
    }
}

fn staging_name(id: &str) -> String {
    let n = STAGING_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{PARTIAL}{id}-{}-{n}", std::process::id())
}

fn install_record(
    entry: &ConnectomeEntry,
    folder: &str,
    files: &[FileRecord],
    urls: &[String],
) -> Value {
    json!({
        "id": entry.id,
        "name": folder,
        "version": entry.version,
        "space": entry.space,
        "files": files,
        "source_urls": urls,
        "installed_at": utc_now_rfc3339(),
        "installed_by": crate::version::version_string(),
    })
}

fn single_top_folder(dir: &Path) -> Res<String> {
    let mut names = Vec::new();
    for e in std::fs::read_dir(dir)
        .map_err(|e| io_fail("cannot read the extracted folder", &e))?
        .flatten()
    {
        if !e.path().is_dir() {
            return Err(ConnectomeError::new(
                ErrorCode::Archive,
                "the archive must contain exactly one top-level folder",
            ));
        }
        names.push(e.file_name().to_string_lossy().into_owned());
    }
    match names.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(ConnectomeError::new(
            ErrorCode::Archive,
            "the archive must contain exactly one top-level folder",
        )),
    }
}

fn top_level_folders(dir: &Path) -> Res<Vec<String>> {
    let mut names = Vec::new();
    for e in std::fs::read_dir(dir)
        .map_err(|e| io_fail("cannot read the extracted folder", &e))?
        .flatten()
    {
        if !e.path().is_dir() {
            return Err(ConnectomeError::new(
                ErrorCode::Archive,
                "the archive must contain only top-level folders",
            ));
        }
        names.push(e.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

fn pick_entry_folder(entry: &ConnectomeEntry, names: &[String]) -> Res<String> {
    if names.iter().any(|n| n == entry.folder()) {
        return Ok(entry.folder().to_owned());
    }
    match names {
        [one] if !entry.folder_verified => Ok(one.clone()),
        _ => Err(ConnectomeError::new(
            ErrorCode::Archive,
            format!(
                "the archive has no folder {:?}; it holds {names:?}",
                entry.folder()
            ),
        )),
    }
}

fn install_zip(
    entry: &ConnectomeEntry,
    archive: &Path,
    class_dir: &Path,
    record: &[FileRecord],
    urls: &[String],
    job: &mut Job<'_>,
) -> Res<(PathBuf, String)> {
    let uncompressed = zip_uncompressed_bytes(archive)?;
    check_disk(
        entry,
        class_dir,
        job.settings,
        uncompressed.saturating_add(job.settings.disk_slack_bytes),
    )?;
    let staging = class_dir.join(staging_name(entry.id));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| io_fail("cannot create the staging folder", &e))?;
    let result = (|| {
        (job.on_event)(ConnectomeEvent::Extracting);
        extract_zip(archive, &staging, job.cancel)?;
        let names = top_level_folders(&staging)?;
        let top = pick_entry_folder(entry, &names)?;
        let top_dir = staging.join(&top);
        if !top_dir.join(entry.marker).is_file() {
            return Err(ConnectomeError::new(
                ErrorCode::Archive,
                format!(
                    "{} is missing from the archive folder {top:?}",
                    entry.marker
                ),
            ));
        }
        write_json(
            &top_dir.join(INSTALL_RECORD),
            &install_record(entry, &top, record, urls),
        )?;
        if is_cancelled(job.cancel) {
            return Err(ConnectomeError::cancelled());
        }
        let dest = class_dir.join(&top);
        replace_dir(&top_dir, &dest).map_err(|m| ConnectomeError::new(ErrorCode::Io, m))?;
        for name in names.iter().filter(|n| **n != top) {
            let dir = staging.join(name);
            if !dir.join(entry.marker).is_file() {
                continue;
            }
            let mut rec = install_record(entry, name, record, urls);
            rec["variant_of"] = json!(entry.id);
            write_json(&dir.join(INSTALL_RECORD), &rec)?;
            if is_cancelled(job.cancel) {
                return Err(ConnectomeError::cancelled());
            }
            replace_dir(&dir, &class_dir.join(name))
                .map_err(|m| ConnectomeError::new(ErrorCode::Io, m))?;
        }
        Ok((dest, top))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn is_gsp_run_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("sub-") else {
        return false;
    };
    let Some((id, tail)) = rest.split_once("_run_") else {
        return false;
    };
    let Some(n) = tail.strip_suffix(".mat") else {
        return false;
    };
    id.len() == 4
        && id.bytes().all(|b| b.is_ascii_digit())
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
}

fn gsp_map(name: &str, is_dir: bool) -> Res<PathBuf> {
    let flat = name.trim_start_matches("./");
    if is_dir {
        return Err(ConnectomeError::new(
            ErrorCode::Archive,
            format!("unexpected folder member {name:?} in a GSP archive"),
        ));
    }
    if flat == "GSP1000.txt" {
        Ok(PathBuf::from("GSP1000.txt"))
    } else if is_gsp_run_name(flat) {
        Ok(Path::new("vol").join(flat))
    } else {
        Err(ConnectomeError::new(
            ErrorCode::Archive,
            format!("unexpected member {name:?} in a GSP archive"),
        ))
    }
}

fn write_progress(dir: &Path, done: &[FileRecord]) -> Res<()> {
    write_json(&progress_path(dir), &json!({ "done": done }))
}

fn install_gsp(
    entry: &ConnectomeEntry,
    dir: &Path,
    class_dir: &Path,
    folder: &str,
    job: &mut Job<'_>,
) -> Res<(PathBuf, String, Vec<FileRecord>)> {
    let stage = dir.join("stage").join(folder);
    std::fs::create_dir_all(stage.join("vol"))
        .map_err(|e| io_fail("cannot create the staging folder", &e))?;
    let mut done: Vec<FileRecord> = read_progress(dir)
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    let names: HashSet<String> = done.iter().map(|d| d.file_name.clone()).collect();
    let count = entry.files.len();
    job.base_done = done.iter().map(|d| d.bytes).sum();
    for (i, file) in entry.files.iter().enumerate() {
        if names.contains(file.file_name) {
            continue;
        }
        (job.on_event)(ConnectomeEvent::FileStarted {
            index: i + 1,
            count,
            file_name: file.file_name.to_owned(),
        });
        let rec = fetch_file(job, file, dir)?;
        let part = dir.join(format!("{}.part", file.file_name));
        let meta = dir.join(format!("{}.part.json", file.file_name));
        if file.file_name.ends_with(".tar") {
            (job.on_event)(ConnectomeEvent::Extracting);
            let f = File::open(&part).map_err(|e| io_fail("cannot open the download", &e))?;
            extract_tar(
                std::io::BufReader::with_capacity(1 << 20, f),
                &stage,
                &mut gsp_map,
                job.cancel,
            )?;
        } else {
            let rel = safe_relative(file.file_name)?;
            std::fs::copy(&part, stage.join(rel))
                .map_err(|e| io_fail("cannot copy the download", &e))?;
        }
        remove_part(&part, &meta);
        job.base_done += file.bytes;
        done.push(rec);
        write_progress(dir, &done)?;
    }
    if !stage.join(entry.marker).is_file() {
        return Err(ConnectomeError::new(
            ErrorCode::Archive,
            format!("{} is missing after the download", entry.marker),
        ));
    }
    write_json(
        &stage.join(INSTALL_RECORD),
        &install_record(
            entry,
            folder,
            &done,
            &[
                format!(
                    "{}/api/access/datafile/*",
                    job.settings.dataverse_base_url.trim_end_matches('/')
                ),
                GSP_DOI.to_owned(),
            ],
        ),
    )?;
    let dest = class_dir.join(folder);
    replace_dir(&stage, &dest).map_err(|m| ConnectomeError::new(ErrorCode::Io, m))?;
    Ok((dest, folder.to_owned(), done))
}

/// Downloads, verifies and installs `entry` into `<primary>/<class>/` (`specs/15` §4.4 to §4.7).
/// Acceptance is checked first and the disk before any request; a partial file stays on disk
/// for resume after a cancel, a network failure or a changed server copy.
pub fn fetch_connectome(
    entry: &ConnectomeEntry,
    roots: &ConnectomeRoots,
    acceptance: &Acceptance,
    settings: &FetchSettings,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(ConnectomeEvent),
) -> Res<FetchOutcome> {
    if entry.access == Access::Dua {
        return Err(import_only_error(entry));
    }
    check_acceptance(entry, acceptance)?;
    let primary = &roots.primary;
    let _lock = DownloadLock::acquire(primary, entry.id)?;
    sweep_old_partials(primary, PARTIAL_MAX_AGE);
    let dir = partial_dir(primary, entry);
    let existing = partial_bytes(primary, entry);
    check_disk(
        entry,
        primary,
        settings,
        required_free_bytes(entry, settings.disk_slack_bytes).saturating_sub(existing),
    )?;
    let record_path = write_licence_record(
        primary,
        entry,
        acceptance.via,
        attestation_for(entry, acceptance).as_deref(),
    )?;
    let class_dir = roots.class_dir(entry.class);
    std::fs::create_dir_all(&dir).map_err(|e| io_fail("cannot create the partial folder", &e))?;
    on_event(ConnectomeEvent::Started {
        id: entry.id.to_owned(),
        total_bytes: entry.total_bytes(),
        files: entry.files.len(),
    });
    let mut job = Job {
        settings,
        cancel,
        on_event,
        base_done: 0,
        total: entry.total_bytes(),
    };
    let (path, folder, files) = match entry.install {
        Install::ZipFolder { .. } => {
            let file = entry
                .files
                .first()
                .ok_or_else(|| ConnectomeError::new(ErrorCode::Usage, "entry has no files"))?;
            (job.on_event)(ConnectomeEvent::FileStarted {
                index: 1,
                count: 1,
                file_name: file.file_name.to_owned(),
            });
            let rec = fetch_file(&mut job, file, &dir)?;
            let archive = dir.join(format!("{}.part", file.file_name));
            let urls = vec![match file.source {
                Source::LeadDbs { id } => format!("{}?id={id}", settings.leaddbs_release_url),
                Source::Dataverse { file_id } => format!("dataverse:{file_id}"),
                Source::Figshare { file_id } => format!(
                    "{}/{file_id}",
                    settings.figshare_base_url.trim_end_matches('/')
                ),
            }];
            let (path, folder) = install_zip(
                entry,
                &archive,
                &class_dir,
                std::slice::from_ref(&rec),
                &urls,
                &mut job,
            )?;
            (path, folder, vec![rec])
        }
        Install::GspTars { folder } => {
            let (path, folder, files) = install_gsp(entry, &dir, &class_dir, folder, &mut job)?;
            (path, folder, files)
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    (job.on_event)(ConnectomeEvent::Done { path: path.clone() });
    Ok(FetchOutcome {
        path,
        folder,
        files,
        licence_record: record_path,
    })
}

/// What `import` did.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportOutcome {
    pub path: PathBuf,
    pub class: ConnectomeClass,
    pub id: String,
    pub registered: bool,
    pub extracted: bool,
    pub catalogue_id: Option<&'static str>,
    pub licence_record: Option<PathBuf>,
    pub licence_note: Option<String>,
    pub verified: Option<bool>,
}

/// Printed beside an imported archive whose name is not a catalogue file name.
pub const UNVERIFIED_ARCHIVE_NOTE: &str = "not a known catalogue file; not verified";

impl ImportOutcome {
    /// One line on the verification of an imported archive; `None` for a registered folder.
    pub fn verification_note(&self) -> Option<&'static str> {
        match self.verified {
            Some(true) => Some("verified against the catalogue size and checksums"),
            Some(false) => Some(UNVERIFIED_ARCHIVE_NOTE),
            None => None,
        }
    }
}

fn catalogue_files() -> Vec<&'static ConnectomeFile> {
    catalogue().iter().flat_map(|e| e.files.iter()).collect()
}

fn split_extension(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    let at = if lower.ends_with(".tar.gz") {
        Some(name.len() - ".tar.gz".len())
    } else {
        name.rfind('.')
    };
    match at {
        Some(i) => name.split_at(i),
        None => (name, ""),
    }
}

/// Whether `candidate` is the catalogue file name (ASCII case-insensitive) or that name with a
/// browser's duplicate counter, such as `name (1).zip` or `name(2).zip`.
fn names_catalogue_file(candidate: &str, catalogue_name: &str) -> bool {
    if candidate.eq_ignore_ascii_case(catalogue_name) {
        return true;
    }
    let (stem, ext) = split_extension(catalogue_name);
    let Some(rest) = candidate.get(stem.len()..).filter(|_| {
        candidate
            .get(..stem.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(stem))
    }) else {
        return false;
    };
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let Some(rest) = rest.strip_prefix('(') else {
        return false;
    };
    let Some((digits, tail)) = rest.split_once(')') else {
        return false;
    };
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && tail.eq_ignore_ascii_case(ext)
}

fn verify_archive(archive: &Path, file: &ConnectomeFile, cancel: &AtomicBool) -> Res<()> {
    let entry_name = catalogue()
        .iter()
        .find(|e| e.files.iter().any(|f| f.file_name == file.file_name))
        .map_or_else(String::new, |e| format!(" of {}", e.name));
    let mismatch = |what: String| {
        ConnectomeError::new(
            ErrorCode::Checksum,
            format!(
                "{} is named like the catalogue file {}{entry_name} but is not that file: {what}. If the download was interrupted, download it again. If this is your own archive, rename it and import it again; it will then be imported without verification.",
                archive.display(),
                file.file_name
            ),
        )
    };
    let size = std::fs::metadata(archive)
        .map_err(|e| io_fail("cannot read the archive", &e))?
        .len();
    if size != file.bytes {
        return Err(mismatch(format!("{size} bytes instead of {}", file.bytes)));
    }
    let mut hashers = Hashers::new(file.source_digest);
    let mut input = File::open(archive).map_err(|e| io_fail("cannot open the archive", &e))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if is_cancelled(cancel) {
            return Err(ConnectomeError::cancelled());
        }
        let n = input
            .read(&mut buf)
            .map_err(|e| io_fail("cannot read the archive", &e))?;
        if n == 0 {
            break;
        }
        hashers.update(&buf[..n]);
    }
    let d = hashers.finish();
    let (kind, want) = split_digest(file.source_digest);
    if d.source != want {
        let label = match kind {
            "sha1" => "SHA-1".to_owned(),
            other => other.to_ascii_uppercase(),
        };
        return Err(mismatch(format!("{label} {} instead of {want}", d.source)));
    }
    if !file.sha256.is_empty() && d.sha256 != file.sha256 {
        return Err(mismatch(format!(
            "SHA-256 {} instead of {}",
            d.sha256, file.sha256
        )));
    }
    Ok(())
}

fn classify_for_import(folder: &Path, class: Option<ConnectomeClass>) -> Res<ConnectomeClass> {
    match class {
        Some(c) if folder_has_marker(folder, c) => Ok(c),
        Some(c) => Err(ConnectomeError::new(
            ErrorCode::Format,
            format!(
                "{} does not look like a {} connectome folder",
                folder.display(),
                c.dir_name()
            ),
        )),
        None => infer_class(folder).ok_or_else(|| {
            let hint = if ["dMRI", "fMRI", "dMRI_MultiTract"]
                .iter()
                .any(|d| folder.join(d).is_dir())
            {
                "; it looks like a connectome root, point at one connectome folder inside it or set DBSLOC_LEADDBS_CONNECTOMES_DIR"
            } else {
                "; pass --class for a multi-tract folder"
            };
            ConnectomeError::new(
                ErrorCode::Format,
                format!(
                    "{} is not recognised as a connectome folder (no data.mat, data.trk or dataset_info.json){hint}",
                    folder.display()
                ),
            )
        }),
    }
}

fn import_licence(
    roots: &ConnectomeRoots,
    folder: &Path,
    class: ConnectomeClass,
    acceptance: Option<&Acceptance>,
) -> Res<(Option<&'static str>, Option<PathBuf>, Option<String>)> {
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tag = read_dataset_info(folder).and_then(|v| v["tag"].as_str().map(str::to_owned));
    let Some(entry) = match_installed(class, &name, tag.as_deref()) else {
        return Ok((None, None, None));
    };
    if entry.access == Access::Open {
        return Ok((Some(entry.id), None, None));
    }
    let existing = licence_record_path(&roots.primary, entry.id);
    if existing.is_file() {
        return Ok((Some(entry.id), Some(existing), None));
    }
    match acceptance {
        Some(a) if check_acceptance(entry, a).is_ok() => {
            let p = write_licence_record(
                &roots.primary,
                entry,
                "import",
                attestation_for(entry, a).as_deref(),
            )?;
            Ok((Some(entry.id), Some(p), None))
        }
        _ => Ok((
            Some(entry.id),
            None,
            Some(format!(
                "no licence record for {}: pass --accept-terms{} to record it; until then every output notes licence_record: null",
                entry.id,
                if entry.access == Access::Dua { " --attest-data-use-agreement" } else { "" }
            )),
        )),
    }
}

/// Registers a connectome folder in place (never copies it).
pub fn import_folder(
    roots: &ConnectomeRoots,
    folder: &Path,
    class: Option<ConnectomeClass>,
    acceptance: Option<&Acceptance>,
) -> Res<ImportOutcome> {
    if !folder.is_dir() {
        return Err(ConnectomeError::new(
            ErrorCode::Missing,
            format!("{} is not a folder", folder.display()),
        ));
    }
    let path =
        std::fs::canonicalize(folder).map_err(|e| io_fail("cannot resolve the folder", &e))?;
    let class = classify_for_import(&path, class)?;
    let (catalogue_id, licence_record, licence_note) =
        import_licence(roots, &path, class, acceptance)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = catalogue_id.map_or_else(|| sanitize_id(&name), str::to_owned);
    register_folder(
        &roots.primary,
        RegistryEntry {
            path: path.display().to_string(),
            class: class.dir_name().to_owned(),
            id: id.clone(),
            registered_at: utc_now_rfc3339(),
            space: None,
        },
    )?;
    Ok(ImportOutcome {
        path,
        class,
        id,
        registered: true,
        extracted: false,
        catalogue_id,
        licence_record,
        licence_note,
        verified: None,
    })
}

/// Extracts a zip / tar / tar.gz archive into the primary root and validates the folder.
/// An archive whose name equals a catalogue file name must match that file's size and checksums
/// before anything is extracted (`specs/15` §4.3).
pub fn import_archive(
    roots: &ConnectomeRoots,
    archive: &Path,
    class: Option<ConnectomeClass>,
    acceptance: Option<&Acceptance>,
    cancel: &AtomicBool,
) -> Res<ImportOutcome> {
    import_archive_against(
        roots,
        archive,
        class,
        acceptance,
        cancel,
        &catalogue_files(),
    )
}

fn import_archive_against(
    roots: &ConnectomeRoots,
    archive: &Path,
    class: Option<ConnectomeClass>,
    acceptance: Option<&Acceptance>,
    cancel: &AtomicBool,
    pins: &[&ConnectomeFile],
) -> Res<ImportOutcome> {
    let kind = archive_kind(archive).ok_or_else(|| {
        ConnectomeError::new(
            ErrorCode::Usage,
            format!(
                "{} is neither a folder nor a .zip, .tar or .tar.gz file",
                archive.display()
            ),
        )
    })?;
    let known = archive
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|name| {
            pins.iter()
                .copied()
                .find(|f| names_catalogue_file(name, f.file_name))
        });
    if let Some(file) = known {
        verify_archive(archive, file, cancel)?;
    }
    let primary = &roots.primary;
    std::fs::create_dir_all(primary)
        .map_err(|e| io_fail("cannot create the connectomes folder", &e))?;
    let staging = primary.join(staging_name("import"));
    let extract_dir = staging.join("x");
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| io_fail("cannot create the staging folder", &e))?;
    let result = (|| {
        match kind {
            ArchiveKind::Zip => extract_zip(archive, &extract_dir, cancel)?,
            _ => {
                extract_tar_file(archive, kind, &extract_dir, cancel)?;
            }
        }
        let (top_dir, folder_name) = match single_top_folder(&extract_dir) {
            Ok(top) => (extract_dir.join(&top), top),
            Err(_) => (extract_dir.clone(), archive_stem(archive)),
        };
        let folder_name = safe_relative(&folder_name)?.to_string_lossy().into_owned();
        let class = classify_for_import(&top_dir, class)?;
        let dest = roots.class_dir(class).join(&folder_name);
        let tag = read_dataset_info(&top_dir).and_then(|v| v["tag"].as_str().map(str::to_owned));
        let entry = match_installed(class, &folder_name, tag.as_deref());
        let id = entry.map_or_else(|| sanitize_id(&folder_name), |e| e.id.to_owned());
        write_json(
            &top_dir.join(INSTALL_RECORD),
            &json!({
                "id": id,
                "name": folder_name,
                "space": entry.map_or(REFERENCE_SPACE, |e| e.space),
                "imported_from": archive.display().to_string(),
                "installed_at": utc_now_rfc3339(),
                "installed_by": crate::version::version_string(),
            }),
        )?;
        replace_dir(&top_dir, &dest).map_err(|m| ConnectomeError::new(ErrorCode::Io, m))?;
        Ok((dest, class))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    let (dest, class) = result?;
    let (catalogue_id, licence_record, licence_note) =
        import_licence(roots, &dest, class, acceptance)?;
    let id = catalogue_id.map_or_else(
        || {
            sanitize_id(
                &dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        },
        str::to_owned,
    );
    Ok(ImportOutcome {
        path: dest,
        class,
        id,
        registered: false,
        extracted: true,
        catalogue_id,
        licence_record,
        licence_note,
        verified: Some(known.is_some()),
    })
}

/// Server copy of one file compared with its pin (`info --remote`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFact {
    pub file_name: String,
    pub bytes: Option<u64>,
    pub digest: Option<String>,
    pub matches: bool,
}

fn fact(file: &ConnectomeFile, bytes: Option<u64>, digest: Option<String>) -> RemoteFact {
    let (_, pin) = split_digest(file.source_digest);
    let matches = bytes == Some(file.bytes) && digest.as_deref().is_none_or(|d| d == pin);
    RemoteFact {
        file_name: file.file_name.to_owned(),
        bytes,
        digest,
        matches,
    }
}

/// Compares the file list of a Dataverse dataset metadata answer with the catalogue pins.
pub fn compare_dataverse_metadata(entry: &ConnectomeEntry, meta: &Value) -> Vec<RemoteFact> {
    let files = meta["data"]["latestVersion"]["files"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    entry
        .files
        .iter()
        .map(|f| {
            let Source::Dataverse { file_id } = f.source else {
                return fact(f, None, None);
            };
            let found = files
                .iter()
                .find(|v| v["dataFile"]["id"].as_u64() == Some(file_id));
            let bytes = found.and_then(|v| v["dataFile"]["filesize"].as_u64());
            let md5 = found
                .and_then(|v| v["dataFile"]["md5"].as_str())
                .map(str::to_owned);
            fact(f, bytes, md5)
        })
        .collect()
}

/// One user-initiated metadata request: a HEAD on the resolved Lead-DBS file, or the Dataverse
/// dataset metadata.
pub fn probe_remote(entry: &ConnectomeEntry, settings: &FetchSettings) -> Res<Vec<RemoteFact>> {
    let to_err = |e: DlError| match e {
        DlError::Fatal(c) => c,
        DlError::Gone(m) | DlError::Retryable { message: m, .. } => host_err(m),
        DlError::Cancelled | DlError::Restart => ConnectomeError::cancelled(),
    };
    let first = entry
        .files
        .first()
        .ok_or_else(|| ConnectomeError::new(ErrorCode::Usage, "entry has no files"))?;
    match first.source {
        Source::Dataverse { .. } => {
            let url = format!(
                "{}/api/datasets/:persistentId/?persistentId={GSP_DOI}",
                settings.dataverse_base_url.trim_end_matches('/')
            );
            let agent = download_agent_with(0, &style(settings));
            let mut resp = agent
                .get(&url)
                .call()
                .map_err(|e| to_err(net_err(&e, &url)))?;
            let status = resp.status().as_u16();
            if let Some(err) = classify_status(status, &resp, &url) {
                return Err(to_err(err));
            }
            let text = resp
                .body_mut()
                .read_to_string()
                .map_err(|e| host_err(format!("cannot read {url}: {e}")))?;
            let meta: Value = serde_json::from_str(&text)
                .map_err(|e| host_err(format!("{url} did not answer JSON: {e}")))?;
            Ok(compare_dataverse_metadata(entry, &meta))
        }
        Source::LeadDbs { .. } | Source::Figshare { .. } => {
            let resolved = resolve_url(settings, &first.source).map_err(to_err)?;
            let agent = download_agent_with(0, &style(settings));
            let resp = agent
                .head(&resolved)
                .call()
                .map_err(|e| to_err(net_err(&e, &resolved)))?;
            let status = resp.status().as_u16();
            if let Some(err) = classify_status(status, &resp, &resolved) {
                return Err(to_err(err));
            }
            let bytes = header(&resp, "content-length").and_then(|v| v.parse().ok());
            let sha1 = header(&resp, "oc-checksum")
                .filter(|_| matches!(first.source, Source::LeadDbs { .. }))
                .and_then(|oc| {
                    oc.split_whitespace().find_map(|t| {
                        t.to_ascii_lowercase()
                            .strip_prefix("sha1:")
                            .map(str::to_owned)
                    })
                });
            Ok(vec![fact(first, bytes, sha1)])
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
