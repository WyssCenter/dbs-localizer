// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::read::DeflateDecoder;
use flate2::Crc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::connectome_fetch::{user_agent, DownloadLock, ErrorCode, INSTALL_RECORD};
use crate::sha256::{hex, Sha256};
use crate::template_catalog::user_data_dir;
use crate::template_fetch::{download_agent_with, is_cancelled, AgentStyle, CHUNK, PROGRESS_STEP};
use crate::version::utc_now_rfc3339;

pub const ATLAS_DOWNLOAD_URL_ENV: &str = "DBSLOC_ATLAS_DOWNLOAD_URL";
pub const DOWNLOAD_NOTICE: &str = "Lead-DBS atlases are downloaded from the Lead-DBS server to this computer. Lead-DBS states that its standard-space atlases are released under the GNU GPL v3 at their owners' request and should not be used for commercial purposes; for commercial use, contact the atlas's primary coordinator. Cite the atlas publication shown with the atlas.";
pub const PACKAGE_CHANGED: &str =
    "the Lead-DBS atlas package changed; this atlas needs an update of Wyss DBS Localizer";
pub const NO_PARTIAL: &str = "the host does not support partial downloads";
const EMBEDDED_MANIFEST: &str = include_str!("../../../resources/atlases/leaddbs_download.json");
const STAGING_PREFIX: &str = ".staging-";
const REMOVING_PREFIX: &str = ".removing-";
const OLD_COPY_PREFIX: &str = ".partial-old-";
const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 3600);
const SLACK_BYTES: u64 = 100_000_000;
const REQUEST_TIMEOUT_FLOOR: Duration = Duration::from_secs(120);
pub const MAX_REQUEST_BYTES: u64 = 32_000_000;
const MIN_BYTES_PER_SEC: u64 = 50_000;
const RENAME_ATTEMPTS: u32 = 5;
const RENAME_PAUSE: Duration = Duration::from_millis(300);
const LOCAL_HEADER_LEN: u64 = 30;
const EOCD_LEN: usize = 22;
const LOCATOR_LEN: usize = 20;
const ZIP64_EOCD_LEN: usize = 56;
const MAX_COMMENT: usize = 65_535;
const MAX_CENTRAL_DIRECTORY: u64 = 16 * 1024 * 1024;
const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_LOCATOR: u32 = 0x0706_4b50;
const SIG_ZIP64_EOCD: u32 = 0x0606_4b50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasErrorKind {
    UnknownName,
    Bundled,
    Manifest,
    NoDataDir,
    Busy,
    Disk,
    Host,
    NoPartial,
    PackageChanged,
    Archive,
    Checksum,
    Io,
    NotInstalled,
    NotDownloaded,
    ExistingFolder,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlasFetchError {
    pub kind: AtlasErrorKind,
    pub message: String,
}

impl AtlasFetchError {
    pub fn new(kind: AtlasErrorKind, message: impl Into<String>) -> Self {
        AtlasFetchError {
            kind,
            message: message.into(),
        }
    }

    pub fn cancelled() -> Self {
        AtlasFetchError::new(AtlasErrorKind::Cancelled, "cancelled")
    }

    pub fn is_cancelled(&self) -> bool {
        self.kind == AtlasErrorKind::Cancelled
    }

    pub fn is_usage(&self) -> bool {
        matches!(
            self.kind,
            AtlasErrorKind::UnknownName | AtlasErrorKind::Bundled
        )
    }
}

impl std::fmt::Display for AtlasFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AtlasFetchError {}

type Res<T> = Result<T, AtlasFetchError>;

fn io_fail(what: &str, e: impl std::fmt::Display) -> AtlasFetchError {
    AtlasFetchError::new(AtlasErrorKind::Io, format!("{what}: {e}"))
}

fn host_err(message: impl Into<String>) -> AtlasFetchError {
    AtlasFetchError::new(AtlasErrorKind::Host, message)
}

fn archive_err(message: impl Into<String>) -> AtlasFetchError {
    AtlasFetchError::new(AtlasErrorKind::Archive, message)
}

fn changed_err() -> AtlasFetchError {
    AtlasFetchError::new(AtlasErrorKind::PackageChanged, PACKAGE_CHANGED)
}

fn changed_during() -> AtlasFetchError {
    AtlasFetchError::new(
        AtlasErrorKind::PackageChanged,
        "the Lead-DBS data package changed during the download; try again",
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagePin {
    pub file_name: String,
    pub url: String,
    pub bytes: u64,
    pub sha1: String,
    pub last_modified: String,
    pub central_directory_offset: u64,
    pub atlas_prefix: String,
    pub pinned: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestAtlas {
    pub name: String,
    pub bytes: u64,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtlasManifest {
    pub package: PackagePin,
    pub atlases: Vec<ManifestAtlas>,
}

impl AtlasManifest {
    pub fn get(&self, name: &str) -> Option<&ManifestAtlas> {
        self.atlases.iter().find(|a| a.name == name)
    }
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_reserved_device(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        s => {
            let digit = |p: &str| {
                s.strip_prefix(p)
                    .is_some_and(|d| d.len() == 1 && matches!(d.as_bytes()[0], b'1'..=b'9'))
            };
            digit("COM") || digit("LPT")
        }
    }
}

fn bad_component(c: &str) -> bool {
    c.is_empty()
        || c == "."
        || c == ".."
        || c.ends_with('.')
        || c.ends_with(' ')
        || c.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
        || c.chars().any(char::is_control)
        || is_reserved_device(c)
}

pub fn check_pinned_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.split('/').any(bad_component) {
        return Err(format!("unsafe path {path:?}"));
    }
    let last = path.rsplit('/').next().unwrap_or(path);
    if path.eq_ignore_ascii_case(INSTALL_RECORD) || last.eq_ignore_ascii_case(INSTALL_RECORD) {
        return Err(format!("reserved path {path:?}"));
    }
    Ok(())
}

pub fn check_atlas_name(name: &str) -> Result<(), String> {
    let bad = name.starts_with('.') || name.contains('/') || bad_component(name);
    if bad {
        Err(format!("unusable atlas name {name:?}"))
    } else {
        Ok(())
    }
}

fn validate(m: &AtlasManifest) -> Result<(), String> {
    let p = &m.package;
    if !(p.url.starts_with("https://") || p.url.starts_with("http://")) {
        return Err(format!("package url {:?} is not an HTTP address", p.url));
    }
    if !is_lower_hex(&p.sha1, 40) {
        return Err("package sha1 is not 40 lowercase hex digits".to_owned());
    }
    let prefix = p
        .atlas_prefix
        .strip_suffix('/')
        .ok_or_else(|| "atlas_prefix must end with '/'".to_owned())?;
    check_pinned_path(prefix).map_err(|e| format!("atlas_prefix: {e}"))?;
    let mut names = HashSet::new();
    for a in &m.atlases {
        check_atlas_name(&a.name)?;
        if !names.insert(a.name.as_str()) {
            return Err(format!("atlas {:?} is listed twice", a.name));
        }
        let mut paths = HashSet::new();
        let mut sum = 0u64;
        for f in &a.files {
            check_pinned_path(&f.path).map_err(|e| format!("atlas {:?}: {e}", a.name))?;
            if !paths.insert(f.path.to_lowercase()) {
                return Err(format!("atlas {:?}: {:?} is listed twice", a.name, f.path));
            }
            if !is_lower_hex(&f.sha256, 64) {
                return Err(format!(
                    "atlas {:?}: {:?} has no valid sha256",
                    a.name, f.path
                ));
            }
            sum = sum.saturating_add(f.bytes);
        }
        if a.files.is_empty() {
            return Err(format!("atlas {:?} has no files", a.name));
        }
        if sum != a.bytes {
            return Err(format!(
                "atlas {:?}: bytes {} differ from the sum of its files {sum}",
                a.name, a.bytes
            ));
        }
    }
    Ok(())
}

pub fn parse_manifest(text: &str) -> Res<AtlasManifest> {
    let invalid = |e: String| {
        AtlasFetchError::new(
            AtlasErrorKind::Manifest,
            format!("the atlas download list is invalid: {e}"),
        )
    };
    let m: AtlasManifest = serde_json::from_str(text).map_err(|e| invalid(e.to_string()))?;
    validate(&m).map_err(invalid)?;
    Ok(m)
}

pub fn manifest() -> Res<&'static AtlasManifest> {
    static MANIFEST: OnceLock<Res<AtlasManifest>> = OnceLock::new();
    MANIFEST
        .get_or_init(|| parse_manifest(EMBEDDED_MANIFEST))
        .as_ref()
        .map_err(Clone::clone)
}

pub fn default_bundled_dir() -> Option<PathBuf> {
    crate::bundled_bin::find_resources_root()
        .map(|r| r.join("resources").join("atlases").join("leaddbs"))
}

pub fn downloaded_atlases_dir(data_root: &Path) -> PathBuf {
    data_root.join("atlases").join("leaddbs")
}

fn is_bundled(bundled_dir: Option<&Path>, name: &str) -> bool {
    check_atlas_name(name).is_ok() && bundled_dir.is_some_and(|d| d.join(name).is_dir())
}

pub fn resolve_download<'m>(
    manifest: &'m AtlasManifest,
    name: &str,
    bundled_dir: Option<&Path>,
) -> Res<&'m ManifestAtlas> {
    if is_bundled(bundled_dir, name) {
        return Err(AtlasFetchError::new(
            AtlasErrorKind::Bundled,
            format!("'{name}' is already bundled with Wyss DBS Localizer; nothing to download"),
        ));
    }
    manifest.get(name).ok_or_else(|| {
        AtlasFetchError::new(
            AtlasErrorKind::UnknownName,
            format!("unknown atlas '{name}'; list the atlases with: dbsloc atlas list"),
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtlasState {
    Bundled(PathBuf),
    Installed { path: PathBuf, downloaded: bool },
    Available,
}

impl AtlasState {
    pub fn label(&self) -> &'static str {
        match self {
            AtlasState::Bundled(_) => "bundled",
            AtlasState::Installed { .. } => "installed",
            AtlasState::Available => "available",
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            AtlasState::Bundled(p) | AtlasState::Installed { path: p, .. } => Some(p),
            AtlasState::Available => None,
        }
    }

    pub fn removable(&self) -> bool {
        matches!(
            self,
            AtlasState::Installed {
                downloaded: true,
                ..
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlasStatus {
    pub name: String,
    pub bytes: u64,
    pub state: AtlasState,
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

pub fn list_atlases_in(
    manifest: &AtlasManifest,
    bundled_dir: Option<&Path>,
    data_root: Option<&Path>,
) -> Vec<AtlasStatus> {
    let user = data_root.map(downloaded_atlases_dir);
    let mut rows: BTreeMap<Vec<u8>, AtlasStatus> = BTreeMap::new();
    let state_of = |name: &str| -> AtlasState {
        if let Some(d) = bundled_dir.filter(|_| is_bundled(bundled_dir, name)) {
            return AtlasState::Bundled(d.join(name));
        }
        match user.as_ref().map(|u| u.join(name)) {
            Some(p) if check_atlas_name(name).is_ok() && p.is_dir() => AtlasState::Installed {
                downloaded: p.join(INSTALL_RECORD).is_file(),
                path: p,
            },
            _ => AtlasState::Available,
        }
    };
    for a in &manifest.atlases {
        rows.insert(
            a.name.as_bytes().to_vec(),
            AtlasStatus {
                name: a.name.clone(),
                bytes: a.bytes,
                state: state_of(&a.name),
            },
        );
    }
    if let Some(dir) = bundled_dir {
        for name in crate::leaddbs_data::bundled_atlas_names(dir.parent().and_then(Path::parent)) {
            let key = name.as_bytes().to_vec();
            if rows.contains_key(&key) {
                continue;
            }
            rows.insert(
                key,
                AtlasStatus {
                    bytes: dir_bytes(&dir.join(&name)),
                    state: AtlasState::Bundled(dir.join(&name)),
                    name,
                },
            );
        }
    }
    rows.into_values().collect()
}

pub fn list_atlases() -> Res<Vec<AtlasStatus>> {
    Ok(list_atlases_in(
        manifest()?,
        default_bundled_dir().as_deref(),
        user_data_dir().as_deref(),
    ))
}

pub fn remove_atlas(name: &str, data_root: &Path, bundled_dir: Option<&Path>) -> Res<PathBuf> {
    check_atlas_name(name).map_err(|e| AtlasFetchError::new(AtlasErrorKind::NotInstalled, e))?;
    let atlases = data_root.join("atlases");
    let _lock = if atlases.is_dir() {
        let lock = acquire_lock(&atlases, name)?;
        sweep_leftovers(data_root, None, STALE_STAGING_AGE);
        Some(lock)
    } else {
        None
    };
    let root = downloaded_atlases_dir(data_root);
    let dir = root.join(name);
    if !dir.is_dir() {
        if is_bundled(bundled_dir, name) {
            return Err(AtlasFetchError::new(
                AtlasErrorKind::Bundled,
                format!("'{name}' is bundled with Wyss DBS Localizer and cannot be removed"),
            ));
        }
        return Err(AtlasFetchError::new(
            AtlasErrorKind::NotInstalled,
            format!("'{name}' is not installed in {}", root.display()),
        ));
    }
    if !dir.join(INSTALL_RECORD).is_file() {
        return Err(AtlasFetchError::new(
            AtlasErrorKind::NotDownloaded,
            format!(
                "{} was not downloaded by Wyss DBS Localizer (no {INSTALL_RECORD}); remove it by hand if you want it gone",
                dir.display()
            ),
        ));
    }
    let aside = root.join(format!("{REMOVING_PREFIX}{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&aside);
    std::fs::rename(&dir, &aside).map_err(|e| io_fail("cannot remove the atlas folder", e))?;
    std::fs::remove_dir_all(&aside).map_err(|e| io_fail("cannot remove the atlas folder", e))?;
    Ok(dir)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AtlasEvent {
    Started {
        name: String,
        total_bytes: u64,
        files: usize,
    },
    Note {
        message: String,
    },
    Retry {
        attempt: usize,
        wait_secs: u64,
        reason: String,
    },
    Progress {
        downloaded: u64,
        total: u64,
    },
    Installing,
    Done {
        path: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct AtlasFetchSettings {
    pub url: Option<String>,
    pub user_agent: String,
    pub retry_delays: Vec<Duration>,
    pub free_space_override: Option<u64>,
    pub bundled_dir: Option<PathBuf>,
    pub max_request_bytes: u64,
}

impl Default for AtlasFetchSettings {
    fn default() -> Self {
        AtlasFetchSettings {
            url: None,
            user_agent: user_agent(),
            retry_delays: [5, 10, 20, 40, 60]
                .into_iter()
                .map(Duration::from_secs)
                .collect(),
            free_space_override: None,
            bundled_dir: default_bundled_dir(),
            max_request_bytes: MAX_REQUEST_BYTES,
        }
    }
}

fn package_url(settings: &AtlasFetchSettings, manifest: &AtlasManifest) -> String {
    settings
        .url
        .clone()
        .or_else(|| {
            std::env::var(ATLAS_DOWNLOAD_URL_ENV)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| manifest.package.url.clone())
}

pub fn required_free_bytes(atlas: &ManifestAtlas) -> u64 {
    atlas
        .bytes
        .saturating_add(atlas.bytes / 10)
        .saturating_add(SLACK_BYTES)
}

fn check_free_space(atlas: &ManifestAtlas, free: Option<u64>) -> Res<()> {
    let need = required_free_bytes(atlas);
    match free {
        Some(free) if free < need => Err(AtlasFetchError::new(
            AtlasErrorKind::Disk,
            format!(
                "not enough free disk space for {}: need {} MB, {} MB free",
                atlas.name,
                need.div_ceil(1_000_000),
                free / 1_000_000
            ),
        )),
        _ => Ok(()),
    }
}

fn sweep_dirs(root: &Path, prefixes: &[&str], keep: Option<&Path>, max_age: Duration) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if Some(path.as_path()) == keep || !path.is_dir() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().into_owned();
        if !prefixes.iter().any(|p| name.starts_with(p)) {
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

fn sweep_leftovers(data_root: &Path, keep_staging: Option<&Path>, staging_age: Duration) {
    sweep_dirs(
        &data_root.join("atlases"),
        &[STAGING_PREFIX],
        keep_staging,
        staging_age,
    );
    let user = downloaded_atlases_dir(data_root);
    sweep_dirs(&user, &[REMOVING_PREFIX], None, Duration::ZERO);
    recover_old_copies(&user);
}

fn aside_stamp() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let mut prev = LAST.load(Ordering::SeqCst);
    loop {
        let next = now.max(prev.saturating_add(1));
        match LAST.compare_exchange(prev, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return next,
            Err(seen) => prev = seen,
        }
    }
}

fn parse_old_copy(file_name: &str) -> Option<(String, u64)> {
    let rest = file_name.strip_prefix(OLD_COPY_PREFIX)?;
    let mut parts = rest.rsplitn(3, '-');
    let stamp = parts.next()?;
    if stamp.len() != 20 || !stamp.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let stamp = stamp.parse::<u64>().ok()?;
    parts.next()?.parse::<u32>().ok()?;
    let name = parts.next()?;
    check_atlas_name(name).ok()?;
    Some((name.to_owned(), stamp))
}

fn recover_old_copies(user: &Path) {
    let Ok(rd) = std::fs::read_dir(user) else {
        return;
    };
    let mut copies: BTreeMap<String, Vec<(u64, PathBuf)>> = BTreeMap::new();
    for ent in rd.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        if let Some((name, stamp)) = parse_old_copy(&ent.file_name().to_string_lossy()) {
            copies.entry(name).or_default().push((stamp, path));
        }
    }
    for (name, mut list) in copies {
        list.sort_by_key(|(stamp, _)| *stamp);
        let dest = user.join(&name);
        if !dest.exists() {
            if let Some((_, newest)) = list.pop() {
                let restored = rename_with_retries(
                    &newest,
                    &dest,
                    RENAME_ATTEMPTS,
                    RENAME_PAUSE,
                    &|from, to| std::fs::rename(from, to),
                );
                if restored.is_err() {
                    continue;
                }
            }
        }
        if dest.is_dir() {
            for (_, path) in list {
                discard_old_copy(&path, &|from, to| std::fs::rename(from, to));
            }
        }
    }
}

fn lock_err(e: crate::connectome_fetch::ConnectomeError) -> AtlasFetchError {
    if e.code == ErrorCode::Busy {
        AtlasFetchError::new(AtlasErrorKind::Busy, e.message)
    } else {
        AtlasFetchError::new(AtlasErrorKind::Io, e.message)
    }
}

fn acquire_lock(atlases: &Path, name: &str) -> Res<DownloadLock> {
    DownloadLock::acquire_labelled(atlases, name, "atlases", "atlas").map_err(lock_err)
}

fn refuse_foreign_folder(dest: &Path) -> Res<()> {
    if dest.exists() && !dest.join(INSTALL_RECORD).is_file() {
        return Err(AtlasFetchError::new(
            AtlasErrorKind::ExistingFolder,
            format!(
                "{} exists and was not downloaded by Wyss DBS Localizer (no {INSTALL_RECORD}); remove or rename it first",
                dest.display()
            ),
        ));
    }
    Ok(())
}

pub fn fetch_atlas(
    name: &str,
    data_root: &Path,
    settings: &AtlasFetchSettings,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(AtlasEvent),
) -> Res<PathBuf> {
    fetch_atlas_with(manifest()?, name, data_root, settings, cancel, on_event)
}

pub fn fetch_atlas_with(
    manifest: &AtlasManifest,
    name: &str,
    data_root: &Path,
    settings: &AtlasFetchSettings,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(AtlasEvent),
) -> Res<PathBuf> {
    let atlas = resolve_download(manifest, name, settings.bundled_dir.as_deref())?;
    let root = data_root.join("atlases");
    std::fs::create_dir_all(&root).map_err(|e| io_fail("cannot create the atlases folder", e))?;
    let _lock = acquire_lock(&root, &atlas.name)?;
    let staging = root.join(format!(
        "{STAGING_PREFIX}{}-{}",
        atlas.name,
        std::process::id()
    ));
    sweep_leftovers(data_root, Some(&staging), STALE_STAGING_AGE);
    let dest = downloaded_atlases_dir(data_root).join(&atlas.name);
    refuse_foreign_folder(&dest)?;
    let free = settings
        .free_space_override
        .or_else(|| crate::backdrop_fetch::free_space_bytes(&root));
    check_free_space(atlas, free)?;
    if is_cancelled(cancel) {
        return Err(AtlasFetchError::cancelled());
    }
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| io_fail("cannot create the staging folder", e))?;
    let job = Job {
        manifest,
        atlas,
        settings,
        cancel,
    };
    let result = job.run(&staging, &dest, on_event);
    let _ = std::fs::remove_dir_all(&staging);
    if result.is_ok() {
        on_event(AtlasEvent::Done { path: dest.clone() });
    }
    result.map(|()| dest)
}

struct Job<'a> {
    manifest: &'a AtlasManifest,
    atlas: &'a ManifestAtlas,
    settings: &'a AtlasFetchSettings,
    cancel: &'a AtomicBool,
}

struct Head {
    size: u64,
    etag: Option<String>,
    last_modified: Option<String>,
    sha1: Option<String>,
}

fn header(resp: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?']).next().unwrap_or(rest)
}

fn parse_content_range(v: &str) -> Option<(u64, u64)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, _) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?))
}

enum ReqErr {
    Retry(String),
    Fatal(AtlasFetchError),
}

struct Remote<'a> {
    agent: ureq::Agent,
    url: String,
    etag: Option<String>,
    validator: Option<String>,
    delays: &'a [Duration],
    cancel: &'a AtomicBool,
    max_request: u64,
}

impl Remote<'_> {
    fn head(&self) -> Res<Head> {
        let resp = self
            .agent
            .head(&self.url)
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|e| host_err(format!("cannot reach {}: {e}", host_of(&self.url))))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(host_err(format!(
                "{} answered HTTP {status}",
                host_of(&self.url)
            )));
        }
        let size = header(&resp, "content-length")
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| {
                host_err(format!(
                    "{} does not state the size of the data package",
                    host_of(&self.url)
                ))
            })?;
        let sha1 = header(&resp, "oc-checksum").and_then(|oc| {
            oc.split_whitespace().find_map(|t| {
                t.to_ascii_lowercase()
                    .strip_prefix("sha1:")
                    .map(str::to_owned)
            })
        });
        Ok(Head {
            size,
            etag: header(&resp, "etag"),
            last_modified: header(&resp, "last-modified"),
            sha1,
        })
    }

    fn open(
        &self,
        start: u64,
        end: u64,
        conditional: bool,
    ) -> Result<(ureq::BodyReader<'static>, u64), ReqErr> {
        let mut req = self
            .agent
            .get(&self.url)
            .header("Accept-Encoding", "identity")
            .header("Range", format!("bytes={start}-{}", end - 1));
        let validator = self.validator.as_deref().filter(|_| conditional);
        if let Some(v) = validator {
            req = req.header("If-Range", v);
        }
        let resp = req
            .config()
            .timeout_recv_body(Some(request_timeout(end - start)))
            .build()
            .call()
            .map_err(|e| match e {
                ureq::Error::BadUri(_) | ureq::Error::Http(_) => {
                    ReqErr::Fatal(host_err(format!("bad request URL {}: {e}", self.url)))
                }
                e => ReqErr::Retry(e.to_string()),
            })?;
        let host = host_of(&self.url);
        match resp.status().as_u16() {
            206 => {}
            200 if validator.is_some() => return Err(ReqErr::Fatal(changed_during())),
            200 => {
                return Err(ReqErr::Fatal(AtlasFetchError::new(
                    AtlasErrorKind::NoPartial,
                    NO_PARTIAL,
                )))
            }
            s @ (408 | 429 | 500 | 502 | 503 | 504) => {
                return Err(ReqErr::Retry(format!("{host} answered HTTP {s}")))
            }
            s => return Err(ReqErr::Fatal(host_err(format!("{host} answered HTTP {s}")))),
        }
        if let (Some(theirs), Some(ours)) = (header(&resp, "etag"), self.etag.as_deref()) {
            if theirs != ours {
                return Err(ReqErr::Fatal(changed_during()));
            }
        }
        let body_end = match header(&resp, "content-range").and_then(|v| parse_content_range(&v)) {
            Some((s, last)) if s == start && last >= start && last < end => last + 1,
            _ => {
                return Err(ReqErr::Fatal(host_err(format!(
                    "{host} answered a Range request with an unusable Content-Range"
                ))))
            }
        };
        let reader = resp
            .into_body()
            .into_with_config()
            .limit(body_end - start)
            .reader();
        Ok((reader, body_end))
    }
}

pub fn request_timeout(range_bytes: u64) -> Duration {
    REQUEST_TIMEOUT_FLOOR.max(Duration::from_secs(range_bytes / MIN_BYTES_PER_SEC))
}

fn sleep_poll(wait: Duration, cancel: &AtomicBool) -> Res<()> {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if is_cancelled(cancel) {
            return Err(AtlasFetchError::cancelled());
        }
        std::thread::sleep(Duration::from_millis(50).min(end - Instant::now()));
    }
    Ok(())
}

struct Progress<'e> {
    on_event: &'e mut dyn FnMut(AtlasEvent),
    downloaded: u64,
    total: u64,
    last: u64,
}

impl Progress<'_> {
    fn add(&mut self, n: u64) {
        self.downloaded += n;
        if self.downloaded - self.last >= PROGRESS_STEP {
            self.flush();
        }
    }

    fn flush(&mut self) {
        self.last = self.downloaded;
        (self.on_event)(AtlasEvent::Progress {
            downloaded: self.downloaded,
            total: self.total,
        });
    }
}

struct RunReader<'a, 'e> {
    remote: &'a Remote<'a>,
    progress: &'a mut Progress<'e>,
    counted: bool,
    conditional: bool,
    pos: u64,
    end: u64,
    body: Option<ureq::BodyReader<'static>>,
    body_end: u64,
    retries: usize,
    error: Option<AtlasFetchError>,
}

impl<'a, 'e> RunReader<'a, 'e> {
    fn new(
        remote: &'a Remote<'a>,
        progress: &'a mut Progress<'e>,
        start: u64,
        end: u64,
        counted: bool,
        conditional: bool,
    ) -> Self {
        RunReader {
            remote,
            progress,
            counted,
            conditional,
            pos: start,
            end,
            body: None,
            body_end: start,
            retries: 0,
            error: None,
        }
    }

    fn retry(&mut self, reason: String) -> Res<()> {
        self.retries += 1;
        let Some(wait) = self.remote.delays.get(self.retries - 1).copied() else {
            return Err(host_err(format!(
                "the download stopped at byte {} after {} retries: {reason}",
                self.pos,
                self.retries - 1
            )));
        };
        (self.progress.on_event)(AtlasEvent::Retry {
            attempt: self.retries,
            wait_secs: wait.as_secs(),
            reason,
        });
        sleep_poll(wait, self.remote.cancel)
    }

    fn next(&mut self, buf: &mut [u8]) -> Res<usize> {
        loop {
            if is_cancelled(self.remote.cancel) {
                return Err(AtlasFetchError::cancelled());
            }
            if self.pos >= self.end || buf.is_empty() {
                return Ok(0);
            }
            if self.body.is_none() {
                let request_end = self
                    .end
                    .min(self.pos.saturating_add(self.remote.max_request));
                match self.remote.open(self.pos, request_end, self.conditional) {
                    Ok((b, body_end)) => {
                        self.body = Some(b);
                        self.body_end = body_end;
                    }
                    Err(ReqErr::Fatal(e)) => return Err(e),
                    Err(ReqErr::Retry(m)) => {
                        self.retry(m)?;
                        continue;
                    }
                }
            }
            if self.pos >= self.body_end {
                self.body = None;
                continue;
            }
            let Some(body) = self.body.as_mut() else {
                continue;
            };
            let want = (self.body_end - self.pos).min(buf.len() as u64) as usize;
            match body.read(&mut buf[..want]) {
                Ok(0) => {
                    self.body = None;
                    self.retry("the connection closed early".to_owned())?;
                }
                Ok(n) => {
                    self.pos += n as u64;
                    if self.counted {
                        self.progress.add(n as u64);
                    }
                    return Ok(n);
                }
                Err(e) => {
                    self.body = None;
                    self.retry(format!("connection interrupted: {e}"))?;
                }
            }
        }
    }

    fn fill(&mut self, buf: &mut [u8]) -> Res<()> {
        let mut at = 0;
        while at < buf.len() {
            let n = self.next(&mut buf[at..])?;
            if n == 0 {
                return Err(archive_err(
                    "an entry runs past its end in the data package",
                ));
            }
            at += n;
        }
        Ok(())
    }

    fn skip_to(&mut self, target: u64) -> Res<()> {
        if target < self.pos || target > self.end {
            return Err(archive_err("overlapping entries in the data package"));
        }
        let mut scratch = [0u8; 4096];
        while self.pos < target {
            let want = ((target - self.pos) as usize).min(scratch.len());
            self.fill(&mut scratch[..want])?;
        }
        Ok(())
    }
}

impl Read for RunReader<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.next(buf).map_err(|e| {
            self.error = Some(e);
            std::io::Error::other("atlas download failed")
        })
    }
}

#[derive(Debug, Clone)]
struct CdEntry {
    name: String,
    flags: u16,
    method: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    offset: u64,
}

fn bytes_at<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn u16_at(b: &[u8], at: usize) -> Res<u16> {
    bytes_at::<2>(b, at)
        .map(u16::from_le_bytes)
        .ok_or_else(|| archive_err("truncated ZIP record"))
}

fn u32_at(b: &[u8], at: usize) -> Res<u32> {
    bytes_at::<4>(b, at)
        .map(u32::from_le_bytes)
        .ok_or_else(|| archive_err("truncated ZIP record"))
}

fn u64_at(b: &[u8], at: usize) -> Res<u64> {
    bytes_at::<8>(b, at)
        .map(u64::from_le_bytes)
        .ok_or_else(|| archive_err("truncated ZIP record"))
}

struct Eocd {
    at: usize,
    entries: u64,
    cd_size: u64,
    cd_offset: u64,
    needs_zip64: bool,
}

fn find_eocd(tail: &[u8]) -> Option<Eocd> {
    let last = tail.len().checked_sub(EOCD_LEN)?;
    (0..=last).rev().find_map(|i| {
        if u32_at(tail, i).ok()? != SIG_EOCD {
            return None;
        }
        let comment = usize::from(u16_at(tail, i + 20).ok()?);
        if i + EOCD_LEN + comment > tail.len() {
            return None;
        }
        let entries = u16_at(tail, i + 10).ok()?;
        let cd_size = u32_at(tail, i + 12).ok()?;
        let cd_offset = u32_at(tail, i + 16).ok()?;
        Some(Eocd {
            at: i,
            entries: u64::from(entries),
            cd_size: u64::from(cd_size),
            cd_offset: u64::from(cd_offset),
            needs_zip64: entries == u16::MAX || cd_size == u32::MAX || cd_offset == u32::MAX,
        })
    })
}

fn parse_central_directory(cd: &[u8], expected: u64) -> Res<Vec<CdEntry>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < cd.len() {
        if u32_at(cd, p)? != SIG_CENTRAL {
            return Err(archive_err("bad central directory entry"));
        }
        let flags = u16_at(cd, p + 8)?;
        let method = u16_at(cd, p + 10)?;
        let crc = u32_at(cd, p + 16)?;
        let mut csize = u64::from(u32_at(cd, p + 20)?);
        let mut usize_ = u64::from(u32_at(cd, p + 24)?);
        let name_len = usize::from(u16_at(cd, p + 28)?);
        let extra_len = usize::from(u16_at(cd, p + 30)?);
        let comment_len = usize::from(u16_at(cd, p + 32)?);
        let mut offset = u64::from(u32_at(cd, p + 42)?);
        let name_at = p + 46;
        let raw = cd
            .get(name_at..name_at + name_len)
            .ok_or_else(|| archive_err("truncated central directory"))?;
        let name = if flags & 0x800 != 0 || raw.is_ascii() {
            String::from_utf8_lossy(raw).into_owned()
        } else {
            raw.iter().map(|&b| char::from(b)).collect()
        };
        let extra = cd
            .get(name_at + name_len..name_at + name_len + extra_len)
            .ok_or_else(|| archive_err("truncated central directory"))?;
        let mut q = 0usize;
        while q + 4 <= extra.len() {
            let id = u16_at(extra, q)?;
            let size = usize::from(u16_at(extra, q + 2)?);
            if id == 1 {
                let vals = extra
                    .get(q + 4..q + 4 + size)
                    .ok_or_else(|| archive_err("truncated ZIP64 field"))?;
                let mut k = 0usize;
                if usize_ == u64::from(u32::MAX) {
                    usize_ = u64_at(vals, k)?;
                    k += 8;
                }
                if csize == u64::from(u32::MAX) {
                    csize = u64_at(vals, k)?;
                    k += 8;
                }
                if offset == u64::from(u32::MAX) {
                    offset = u64_at(vals, k)?;
                }
            }
            q += 4 + size;
        }
        out.push(CdEntry {
            name,
            flags,
            method,
            crc,
            csize,
            usize: usize_,
            offset,
        });
        p = name_at + name_len + extra_len + comment_len;
    }
    if out.len() as u64 != expected {
        return Err(archive_err(format!(
            "the central directory lists {} entries, its end record says {expected}",
            out.len()
        )));
    }
    Ok(out)
}

struct Run {
    start: u64,
    end: u64,
    members: Vec<usize>,
}

struct Plan {
    entries: Vec<CdEntry>,
    ends: Vec<u64>,
    files: BTreeMap<usize, String>,
    runs: Vec<Run>,
}

impl Plan {
    fn total(&self) -> u64 {
        self.runs.iter().map(|r| r.end - r.start).sum()
    }
}

fn plan_atlas(
    mut entries: Vec<CdEntry>,
    cd_offset: u64,
    prefix: &str,
    atlas: &ManifestAtlas,
) -> Res<Plan> {
    entries.sort_by_key(|e| e.offset);
    let ends: Vec<u64> = (0..entries.len())
        .map(|i| entries.get(i + 1).map_or(cd_offset, |n| n.offset))
        .collect();
    let folder = format!("{prefix}{}/", atlas.name);
    let pinned: BTreeMap<&str, &ManifestFile> =
        atlas.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut files = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut members = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let Some(rel) = e.name.strip_prefix(&folder) else {
            continue;
        };
        let shared = i > 0 && entries[i - 1].offset == e.offset;
        let room = e
            .offset
            .saturating_add(LOCAL_HEADER_LEN)
            .saturating_add(e.csize);
        if shared || ends[i] < room {
            return Err(archive_err(format!(
                "the Lead-DBS data package is broken: {} overlaps the next entry",
                e.name
            )));
        }
        members.push(i);
        if rel.is_empty() || rel.ends_with('/') {
            continue;
        }
        let Some(pin) = pinned.get(rel) else {
            return Err(changed_err());
        };
        if !seen.insert(rel) || e.usize != pin.bytes {
            return Err(changed_err());
        }
        if e.flags & 1 != 0 {
            return Err(archive_err(format!("{rel} is encrypted")));
        }
        if e.method != 0 && e.method != 8 {
            return Err(archive_err(format!(
                "{rel} uses compression method {}, only stored and deflate are supported",
                e.method
            )));
        }
        files.insert(i, rel.to_owned());
    }
    if seen.len() != pinned.len() {
        return Err(changed_err());
    }
    let mut runs: Vec<Run> = Vec::new();
    for i in members {
        match runs.last_mut() {
            Some(r) if r.members.last().is_some_and(|&l| l + 1 == i) => {
                r.end = ends[i];
                r.members.push(i);
            }
            _ => runs.push(Run {
                start: entries[i].offset,
                end: ends[i],
                members: vec![i],
            }),
        }
    }
    runs.retain(|r| r.end > r.start);
    Ok(Plan {
        entries,
        ends,
        files,
        runs,
    })
}

enum Inner {
    Read(std::io::Error),
    Fatal(AtlasFetchError),
}

fn extract_member(
    rd: &mut RunReader<'_, '_>,
    e: &CdEntry,
    pin: &ManifestFile,
    dest: &Path,
    buf: &mut [u8],
) -> Res<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|x| io_fail("cannot create a folder", x))?;
    }
    let file = File::create(dest).map_err(|x| io_fail("cannot write an atlas file", x))?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let mut sha = Sha256::new();
    let mut crc = Crc::new();
    let mut written = 0u64;
    let path = pin.path.as_str();
    let result = {
        let limited = Read::take(&mut *rd, e.csize);
        let mut src: Box<dyn Read + '_> = if e.method == 8 {
            Box::new(DeflateDecoder::new(limited))
        } else {
            Box::new(limited)
        };
        loop {
            let n = match src.read(buf) {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(x) => break Err(Inner::Read(x)),
            };
            written += n as u64;
            if written > pin.bytes {
                break Err(Inner::Fatal(AtlasFetchError::new(
                    AtlasErrorKind::Checksum,
                    format!("{path} is longer than the pinned {} bytes", pin.bytes),
                )));
            }
            sha.update(&buf[..n]);
            crc.update(&buf[..n]);
            if let Err(x) = out.write_all(&buf[..n]) {
                break Err(Inner::Fatal(io_fail("cannot write an atlas file", x)));
            }
        }
    };
    match result {
        Ok(()) => {}
        Err(Inner::Fatal(err)) => return Err(err),
        Err(Inner::Read(x)) => {
            return Err(rd
                .error
                .take()
                .unwrap_or_else(|| archive_err(format!("cannot decompress {path}: {x}"))))
        }
    }
    out.into_inner()
        .map_err(|x| io_fail("cannot write an atlas file", x.error()))?
        .sync_all()
        .map_err(|x| io_fail("cannot write an atlas file", x))?;
    if written != pin.bytes {
        return Err(AtlasFetchError::new(
            AtlasErrorKind::Checksum,
            format!(
                "size mismatch for {path}: expected {} bytes, got {written}",
                pin.bytes
            ),
        ));
    }
    if crc.sum() != e.crc {
        return Err(AtlasFetchError::new(
            AtlasErrorKind::Checksum,
            format!("CRC-32 mismatch for {path}"),
        ));
    }
    let digest = hex(&sha.finish());
    if digest != pin.sha256 {
        return Err(AtlasFetchError::new(
            AtlasErrorKind::Checksum,
            format!(
                "SHA-256 mismatch for {path}: expected {}, got {digest}",
                pin.sha256
            ),
        ));
    }
    Ok(())
}

type Renamer<'a> = &'a dyn Fn(&Path, &Path) -> std::io::Result<()>;

fn rename_with_retries(
    from: &Path,
    to: &Path,
    attempts: u32,
    pause: Duration,
    rename: Renamer<'_>,
) -> std::io::Result<()> {
    let mut last = Ok(());
    for attempt in 0..attempts.max(1) {
        match rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = Err(e),
        }
        if attempt + 1 < attempts {
            std::thread::sleep(pause);
        }
    }
    last
}

fn install_dir(new_dir: &Path, dest: &Path, rename: Renamer<'_>) -> Res<()> {
    let retrying = |from: &Path, to: &Path| {
        rename_with_retries(from, to, RENAME_ATTEMPTS, RENAME_PAUSE, rename)
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| io_fail("cannot create the destination folder", e))?;
    }
    let aside = dest.with_file_name(format!(
        "{OLD_COPY_PREFIX}{}-{}-{:020}",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        aside_stamp()
    ));
    let had_old = dest.exists();
    if had_old {
        retrying(dest, &aside).map_err(|e| io_fail("cannot replace the installed copy", e))?;
    }
    if let Err(e) = retrying(new_dir, dest) {
        if had_old {
            if let Err(back) = retrying(&aside, dest) {
                return Err(io_fail(
                    &format!(
                        "cannot install the download ({e}), and the previous copy could not be put back ({back}); it is kept in {}",
                        aside.display()
                    ),
                    "rename it to its atlas name to restore it",
                ));
            }
        }
        return Err(io_fail("cannot install the download", e));
    }
    if had_old {
        discard_old_copy(&aside, rename);
    }
    Ok(())
}

fn discard_old_copy(copy: &Path, rename: Renamer<'_>) {
    let Some(file_name) = copy.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return;
    };
    let rest = file_name
        .strip_prefix(OLD_COPY_PREFIX)
        .unwrap_or(&file_name)
        .to_owned();
    let doomed = copy.with_file_name(format!("{REMOVING_PREFIX}{rest}"));
    if rename_with_retries(copy, &doomed, RENAME_ATTEMPTS, RENAME_PAUSE, rename).is_ok() {
        let _ = std::fs::remove_dir_all(&doomed);
    }
}

fn staged_path(staging: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(staging.to_path_buf(), |p, c| p.join(c))
}

impl Job<'_> {
    fn run(&self, staging: &Path, dest: &Path, on_event: &mut dyn FnMut(AtlasEvent)) -> Res<()> {
        let url = package_url(self.settings, self.manifest);
        let style = AgentStyle {
            user_agent: Some(self.settings.user_agent.clone()),
            manual_redirects: true,
        };
        let mut remote = Remote {
            agent: download_agent_with(self.atlas.bytes, &style),
            url,
            etag: None,
            validator: None,
            delays: &self.settings.retry_delays,
            cancel: self.cancel,
            max_request: self.settings.max_request_bytes.max(1),
        };
        let head = remote.head()?;
        let pin = &self.manifest.package;
        let sha_differs = head.sha1.as_deref().is_some_and(|s| s != pin.sha1);
        if head.size != pin.bytes || sha_differs {
            on_event(AtlasEvent::Note {
                message: format!(
                    "the Lead-DBS data package changed on {}; files are checked one by one",
                    head.last_modified.as_deref().unwrap_or("an unknown date")
                ),
            });
        }
        remote.etag = head.etag.clone();
        remote.validator = head
            .etag
            .clone()
            .filter(|t| !t.starts_with("W/"))
            .or_else(|| head.last_modified.clone());
        let mut progress = Progress {
            on_event,
            downloaded: 0,
            total: 0,
            last: 0,
        };
        let plan = self.directory(&remote, &mut progress, head.size)?;
        progress.total = plan.total();
        (progress.on_event)(AtlasEvent::Started {
            name: self.atlas.name.clone(),
            total_bytes: progress.total,
            files: self.atlas.files.len(),
        });
        let pins: BTreeMap<&str, &ManifestFile> = self
            .atlas
            .files
            .iter()
            .map(|f| (f.path.as_str(), f))
            .collect();
        let mut buf = vec![0u8; CHUNK];
        let mut extracted = 0usize;
        for run in &plan.runs {
            let mut rd = RunReader::new(&remote, &mut progress, run.start, run.end, true, true);
            for &i in &run.members {
                let e = &plan.entries[i];
                rd.skip_to(e.offset)?;
                let mut lh = [0u8; 30];
                rd.fill(&mut lh)?;
                if u32_at(&lh, 0)? != SIG_LOCAL {
                    return Err(archive_err(format!("bad local header for {}", e.name)));
                }
                let skip = u64::from(u16_at(&lh, 26)?) + u64::from(u16_at(&lh, 28)?);
                let data_start = rd.pos + skip;
                if data_start.saturating_add(e.csize) > plan.ends[i] {
                    return Err(archive_err(format!("{} runs past the next entry", e.name)));
                }
                rd.skip_to(data_start)?;
                let Some(rel) = plan.files.get(&i) else {
                    continue;
                };
                let pin = pins.get(rel.as_str()).copied().ok_or_else(changed_err)?;
                extract_member(&mut rd, e, pin, &staged_path(staging, rel), &mut buf)?;
                extracted += 1;
            }
            rd.skip_to(run.end)?;
        }
        progress.flush();
        let on_event = progress.on_event;
        if is_cancelled(self.cancel) {
            return Err(AtlasFetchError::cancelled());
        }
        if extracted != self.atlas.files.len() {
            return Err(archive_err(format!(
                "the Lead-DBS data package is broken: {extracted} of {} files of {} were found",
                self.atlas.files.len(),
                self.atlas.name
            )));
        }
        on_event(AtlasEvent::Installing);
        let record = json!({
            "atlas": self.atlas.name,
            "source": pin.file_name,
            "package_sha1": pin.sha1,
            "package_last_modified": head.last_modified,
            "files": self.atlas.files.len(),
            "bytes": self.atlas.bytes,
            "installed": utc_now_rfc3339(),
            "version": 1,
        });
        let text = serde_json::to_string_pretty(&record)
            .map_err(|e| io_fail("cannot write the install record", e))?;
        File::create(staging.join(INSTALL_RECORD))
            .and_then(|mut f| {
                f.write_all(text.as_bytes())?;
                f.sync_all()
            })
            .map_err(|e| io_fail("cannot write the install record", e))?;
        if is_cancelled(self.cancel) {
            return Err(AtlasFetchError::cancelled());
        }
        refuse_foreign_folder(dest)?;
        install_dir(staging, dest, &|from, to| std::fs::rename(from, to))
    }

    fn read_range(
        &self,
        remote: &Remote<'_>,
        progress: &mut Progress<'_>,
        start: u64,
        len: u64,
        conditional: bool,
    ) -> Res<Vec<u8>> {
        let n = usize::try_from(len).map_err(|_| archive_err("ZIP record too large"))?;
        let mut buf = vec![0u8; n];
        let mut rd = RunReader::new(remote, progress, start, start + len, false, conditional);
        rd.fill(&mut buf)?;
        Ok(buf)
    }

    fn directory(&self, remote: &Remote<'_>, progress: &mut Progress<'_>, size: u64) -> Res<Plan> {
        let short = size.min((EOCD_LEN + LOCATOR_LEN + ZIP64_EOCD_LEN) as u64);
        let mut tail_start = size - short;
        let mut tail = self.read_range(remote, progress, tail_start, short, false)?;
        let mut eocd = find_eocd(&tail);
        if eocd.is_none() && short < size {
            let long = size.min((EOCD_LEN + MAX_COMMENT + LOCATOR_LEN + ZIP64_EOCD_LEN) as u64);
            tail_start = size - long;
            tail = self.read_range(remote, progress, tail_start, long, true)?;
            eocd = find_eocd(&tail);
        }
        let eocd = eocd.ok_or_else(|| archive_err("the data package has no ZIP end record"))?;
        let locator = eocd
            .at
            .checked_sub(LOCATOR_LEN)
            .filter(|&l| u32_at(&tail, l).ok() == Some(SIG_LOCATOR));
        let (entries, cd_size, cd_offset) = match locator {
            Some(l) => {
                let z = u64_at(&tail, l + 8)?;
                let rec = if z >= tail_start
                    && z - tail_start + ZIP64_EOCD_LEN as u64 <= tail.len() as u64
                {
                    let at = (z - tail_start) as usize;
                    tail[at..at + ZIP64_EOCD_LEN].to_vec()
                } else {
                    if z.saturating_add(ZIP64_EOCD_LEN as u64) > size {
                        return Err(archive_err("bad ZIP64 locator"));
                    }
                    self.read_range(remote, progress, z, ZIP64_EOCD_LEN as u64, true)?
                };
                if u32_at(&rec, 0)? != SIG_ZIP64_EOCD {
                    return Err(archive_err("bad ZIP64 end record"));
                }
                (u64_at(&rec, 32)?, u64_at(&rec, 40)?, u64_at(&rec, 48)?)
            }
            None if eocd.needs_zip64 => {
                return Err(archive_err("the ZIP64 end record is missing"));
            }
            None => (eocd.entries, eocd.cd_size, eocd.cd_offset),
        };
        if cd_size > MAX_CENTRAL_DIRECTORY || cd_offset.saturating_add(cd_size) > size {
            return Err(archive_err(
                "the central directory lies outside the data package",
            ));
        }
        let cd = self.read_range(remote, progress, cd_offset, cd_size, true)?;
        let entries = parse_central_directory(&cd, entries)?;
        plan_atlas(
            entries,
            cd_offset,
            &self.manifest.package.atlas_prefix,
            self.atlas,
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
