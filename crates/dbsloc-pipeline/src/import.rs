// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Import of loose NIfTI files and DICOM folders into a subject (`specs/01_bids.md` §8.4).

use std::collections::HashSet;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fastwarp_niftiio::ParGzWriter;
use flate2::bufread::GzDecoder as BufGzDecoder;
use flate2::read::GzDecoder;
use serde_json::{json, Value};

use crate::bids::{self, BidsError, RawImageEntry};
use crate::invalidate::{invalidate_from, Invalidation};
use crate::outputs::{partial_of, PARTIAL_PREFIX};
use crate::stages::Stage;

/// An import item is missing, not a NIfTI, or has an unusable header.
pub const E_IMPORT_FILE: &str = "E_IMPORT_FILE";
/// Import items break the session, suffix or orientation rules.
pub const E_IMPORT_RULES: &str = "E_IMPORT_RULES";
/// DICOM conversion failed: converter missing, failed, or no series produced.
pub const E_DCM2NIIX: &str = "E_DCM2NIIX";
/// The user cancelled an import or conversion.
pub const E_IMPORT_CANCELLED: &str = "E_IMPORT_CANCELLED";
/// Environment variable that names the `dcm2niix` executable.
pub const DCM2NIIX_ENV: &str = "DBSLOC_DCM2NIIX_BIN";
/// Environment variable that names a JSON suggestion table ([`LookupTable`]).
pub const LOOKUP_ENV: &str = "DBSLOC_IMPORT_LOOKUP_JSON";

/// Acquisition session of an imported image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Session {
    Preop,
    Postop,
}

impl Session {
    /// Both sessions, pre-op first.
    pub const ALL: [Session; 2] = [Session::Preop, Session::Postop];

    /// BIDS session label.
    pub fn as_str(self) -> &'static str {
        match self {
            Session::Preop => "preop",
            Session::Postop => "postop",
        }
    }

    /// Label for user interfaces.
    pub fn label(self) -> &'static str {
        match self {
            Session::Preop => "Pre-op",
            Session::Postop => "Post-op",
        }
    }
}

/// Acquisition orientation of an imported image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Acq {
    Ax,
    Cor,
    Sag,
    Iso,
}

impl Acq {
    /// All orientations.
    pub const ALL: [Acq; 4] = [Acq::Ax, Acq::Cor, Acq::Sag, Acq::Iso];

    /// BIDS `acq` label.
    pub fn as_str(self) -> &'static str {
        match self {
            Acq::Ax => "ax",
            Acq::Cor => "cor",
            Acq::Sag => "sag",
            Acq::Iso => "iso",
        }
    }

    /// Label for user interfaces.
    pub fn label(self) -> &'static str {
        match self {
            Acq::Ax => "Axial",
            Acq::Cor => "Coronal",
            Acq::Sag => "Sagittal",
            Acq::Iso => "Isotropic",
        }
    }
}

/// Pre-op suffixes, in anchor priority order.
pub const PREOP_SUFFIXES: &[&str] = &["T1w", "T2w", "PDw", "T2starw", "FLAIR", "FGATIR"];
/// Post-op suffixes: CT or an MR suffix.
pub const POSTOP_SUFFIXES: &[&str] = &["CT", "T1w", "T2w", "PDw", "T2starw", "FLAIR", "FGATIR"];

/// Suffixes accepted for a session.
pub fn suffix_choices(session: Session) -> &'static [&'static str] {
    match session {
        Session::Preop => PREOP_SUFFIXES,
        Session::Postop => POSTOP_SUFFIXES,
    }
}

/// One image to import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportItem {
    pub source: PathBuf,
    pub sidecar: Option<PathBuf>,
    pub session: Session,
    pub suffix: String,
    pub acq: Option<Acq>,
}

/// Import failure with a stable code (`specs/01` §11).
#[derive(Clone, Debug)]
pub struct ImportError {
    pub code: &'static str,
    pub message: String,
}

impl ImportError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        ImportError {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for ImportError {}

impl From<BidsError> for ImportError {
    fn from(e: BidsError) -> Self {
        ImportError {
            code: e.code,
            message: e.message,
        }
    }
}

/// Result of an import operation.
pub type ImportResult<T> = Result<T, ImportError>;

/// A file the import will write.
#[derive(Clone, Debug)]
pub struct PlannedFile {
    pub item: usize,
    pub relative_path: String,
    pub sidecar_relative_path: Option<String>,
    pub exists: bool,
    pub sidecar_exists: bool,
    pub index_row_exists: bool,
}

/// A source DICOM folder the import copies to `sourcedata/sub-<id>/DICOM/<name>/`.
#[derive(Clone, Debug)]
pub struct PlannedArchive {
    pub source: PathBuf,
    pub relative_path: String,
}

/// Validated import: destinations, index rows and warnings.
#[derive(Clone, Debug)]
pub struct ImportPlan {
    pub root: PathBuf,
    /// Chosen pipeline output root; `None` = `<root>/derivatives/leaddbs`.
    pub output_dir: Option<PathBuf>,
    pub subject_id: String,
    pub subject_exists: bool,
    pub items: Vec<ImportItem>,
    pub files: Vec<PlannedFile>,
    pub rows: Vec<RawImageEntry>,
    pub sourcedata: Vec<PlannedArchive>,
    pub warnings: Vec<String>,
}

impl ImportPlan {
    /// Planned images whose destination exists already.
    pub fn conflicts(&self) -> Vec<&PlannedFile> {
        self.files
            .iter()
            .filter(|f| f.exists || f.sidecar_exists || f.index_row_exists)
            .collect()
    }

    /// Subject folder name, `sub-<id>`.
    pub fn subject_folder(&self) -> String {
        format!("sub-{}", self.subject_id)
    }
}

/// Progress of a running import.
#[derive(Clone, Debug, Default)]
pub struct ImportProgress {
    pub done: usize,
    pub total: usize,
    pub current: String,
}

/// What a finished import wrote.
#[derive(Clone, Debug)]
pub struct ImportReport {
    pub subject_folder: String,
    pub written: Vec<PathBuf>,
    pub index_path: PathBuf,
    pub invalidation: Invalidation,
}

fn file_err(path: &Path, what: impl std::fmt::Display) -> ImportError {
    ImportError::new(E_IMPORT_FILE, format!("{}: {what}", path.display()))
}

fn folder_sub(subject: &str) -> String {
    let s = subject.trim();
    if s.starts_with("sub-") {
        s.to_owned()
    } else {
        format!("sub-{s}")
    }
}

struct Header {
    dim: [i16; 8],
    datatype: i16,
    bitpix: i16,
    vox_offset: f32,
    pixdim: [f32; 8],
    qform_code: i16,
    sform_code: i16,
    quat: [f32; 3],
    srow: [[f32; 4]; 3],
}

fn is_gz(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().ends_with(".gz")
}

fn is_nifti_name(path: &Path) -> bool {
    let n = path.to_string_lossy().to_lowercase();
    n.ends_with(".nii") || n.ends_with(".nii.gz")
}

fn read_header(path: &Path) -> ImportResult<Header> {
    if !is_nifti_name(path) {
        return Err(file_err(path, "not a NIfTI file (.nii or .nii.gz)"));
    }
    let f = std::fs::File::open(path).map_err(|e| file_err(path, format!("cannot open: {e}")))?;
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err(file_err(path, "not a file"));
    }
    let mut reader: Box<dyn Read> = if is_gz(path) {
        Box::new(GzDecoder::new(f))
    } else {
        Box::new(f)
    };
    let mut b = [0u8; 348];
    reader
        .read_exact(&mut b)
        .map_err(|e| file_err(path, format!("header unreadable or file too short: {e}")))?;
    let hdr = [b[0], b[1], b[2], b[3]];
    if i32::from_le_bytes(hdr) == 540 || i32::from_be_bytes(hdr) == 540 {
        return Err(file_err(path, "NIfTI-2 is not supported"));
    }
    let le = i32::from_le_bytes([b[0], b[1], b[2], b[3]]) == 348;
    let be = i32::from_be_bytes([b[0], b[1], b[2], b[3]]) == 348;
    if !le && !be {
        return Err(file_err(
            path,
            "not a NIfTI-1 header (sizeof_hdr is not 348)",
        ));
    }
    if &b[344..347] != b"n+1" {
        return Err(file_err(path, "not a NIfTI-1 header (bad magic)"));
    }
    let i16_at = |o: usize| {
        let a = [b[o], b[o + 1]];
        if le {
            i16::from_le_bytes(a)
        } else {
            i16::from_be_bytes(a)
        }
    };
    let f32_at = |o: usize| {
        let a = [b[o], b[o + 1], b[o + 2], b[o + 3]];
        if le {
            f32::from_le_bytes(a)
        } else {
            f32::from_be_bytes(a)
        }
    };
    let mut dim = [0i16; 8];
    let mut pixdim = [0f32; 8];
    for i in 0..8 {
        dim[i] = i16_at(40 + 2 * i);
        pixdim[i] = f32_at(76 + 4 * i);
    }
    let mut srow = [[0f32; 4]; 3];
    for (r, row) in srow.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            *v = f32_at(280 + 16 * r + 4 * c);
        }
    }
    Ok(Header {
        dim,
        datatype: i16_at(70),
        bitpix: i16_at(72),
        vox_offset: f32_at(108),
        pixdim,
        qform_code: i16_at(252),
        sform_code: i16_at(254),
        quat: [f32_at(256), f32_at(260), f32_at(264)],
        srow,
    })
}

fn validate_header(path: &Path, h: &Header) -> ImportResult<()> {
    let bad = |why: String| Err(file_err(path, format!("unusable NIfTI header: {why}")));
    if !(3..=7).contains(&h.dim[0]) {
        return bad(format!("dim[0] is {}, expected 3 to 7", h.dim[0]));
    }
    if h.dim[1..=3].iter().any(|&d| d <= 0) {
        return bad("a spatial dimension is zero".to_owned());
    }
    if h.datatype == 0 {
        return bad("datatype is unknown (0)".to_owned());
    }
    if h.bitpix <= 0 {
        return bad(format!("bitpix is {}", h.bitpix));
    }
    if h.qform_code <= 0 && h.sform_code <= 0 {
        return bad("neither qform nor sform is set, so the image has no orientation".to_owned());
    }
    Ok(())
}

fn expected_len(h: &Header) -> u64 {
    let offset = if h.vox_offset.is_finite() && h.vox_offset > 352.0 {
        h.vox_offset as u64
    } else {
        352
    };
    let count = h.dim[1..=(h.dim[0].clamp(1, 7) as usize)]
        .iter()
        .fold(1u64, |a, &d| a.saturating_mul(d.max(1) as u64));
    let bits = count.saturating_mul(h.bitpix.max(0) as u64);
    offset.saturating_add(bits.saturating_add(7) / 8)
}

fn directions(h: &Header) -> [[f32; 3]; 3] {
    if h.sform_code > 0 {
        let mut d = [[0f32; 3]; 3];
        for (c, col) in d.iter_mut().enumerate() {
            for (r, v) in col.iter_mut().enumerate() {
                *v = h.srow[r][c];
            }
        }
        if d.iter().flatten().any(|v| v.abs() > 1e-6) {
            return d;
        }
    }
    let [b, c, d] = h.quat;
    let a = (1.0 - b * b - c * c - d * d).max(0.0).sqrt();
    let r = [
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
            a * a + d * d - b * b - c * c,
        ],
    ];
    let mut out = [[0f32; 3]; 3];
    for (col, o) in out.iter_mut().enumerate() {
        for (row, v) in o.iter_mut().enumerate() {
            *v = r[row][col];
        }
    }
    out
}

fn guess_acq(voxel: [f32; 3], dirs: [[f32; 3]; 3]) -> Option<Acq> {
    let max = voxel.iter().copied().fold(f32::MIN, f32::max);
    let min = voxel.iter().copied().fold(f32::MAX, f32::min);
    if max <= 0.0 || max - min <= 0.1 * max {
        return None;
    }
    let mut sorted = voxel;
    sorted.sort_by(f32::total_cmp);
    let median = sorted[1];
    let thick = (0..3).fold(0, |best, i| {
        let (di, db) = ((voxel[i] - median).abs(), (voxel[best] - median).abs());
        if di > db || (di == db && voxel[i] > voxel[best]) {
            i
        } else {
            best
        }
    });
    let col = dirs[thick];
    let world = (0..3).fold(0, |best, i| {
        if col[i].abs() > col[best].abs() {
            i
        } else {
            best
        }
    });
    Some(match world {
        0 => Acq::Sag,
        1 => Acq::Cor,
        _ => Acq::Ax,
    })
}

/// Dimensions and voxel size (mm) of a NIfTI file, after validating the header (`specs/01` §8.4).
pub fn nifti_summary(path: &Path) -> ImportResult<([u32; 3], [f32; 3])> {
    let h = read_header(path)?;
    validate_header(path, &h)?;
    if !is_gz(path) {
        let need = expected_len(&h);
        let have = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if have < need {
            return Err(file_err(
                path,
                format!("image data is truncated: {have} bytes, header needs {need}"),
            ));
        }
    }
    let dims = [h.dim[1] as u32, h.dim[2] as u32, h.dim[3] as u32];
    let vox = [h.pixdim[1].abs(), h.pixdim[2].abs(), h.pixdim[3].abs()];
    Ok((dims, vox))
}

fn dest_name(sid: &str, item: &ImportItem) -> String {
    let acq = item
        .acq
        .map(|a| format!("_acq-{}", a.as_str()))
        .unwrap_or_default();
    format!(
        "sub-{sid}_ses-{}{acq}_{}",
        item.session.as_str(),
        item.suffix
    )
}

fn is_postop_mr(item: &ImportItem) -> bool {
    item.session == Session::Postop && item.suffix != "CT"
}

fn row_for(item: &ImportItem, relative: &str) -> RawImageEntry {
    let mut entities = serde_json::Map::new();
    entities.insert("suffix".to_owned(), json!(item.suffix));
    if let Some(a) = item.acq {
        entities.insert("acq".to_owned(), json!(a.as_str()));
    }
    if item.session == Session::Postop {
        let modality = match (item.suffix.as_str(), item.acq) {
            ("CT", _) => "CT".to_owned(),
            (_, Some(a)) => format!("{}_MRI", a.as_str()),
            (_, None) => "ax_MRI".to_owned(),
        };
        entities.insert("modality".to_owned(), json!(modality));
    }
    RawImageEntry {
        path: relative.to_owned(),
        session: item.session.as_str().to_owned(),
        datatype: "anat".to_owned(),
        entities,
    }
}

fn same_key(a: &RawImageEntry, b: &RawImageEntry) -> bool {
    a.session == b.session && a.datatype == b.datatype && a.entities == b.entities
}

fn norm_index_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let mut t = s.trim();
    while let Some(rest) = t.strip_prefix("./") {
        t = rest;
    }
    t.to_owned()
}

fn row_clash(old: &RawImageEntry, new: &RawImageEntry) -> bool {
    same_key(old, new) || norm_index_path(&old.path) == norm_index_path(&new.path)
}

fn index_file(root: &Path, folder: &str) -> PathBuf {
    root.join("rawdata").join(folder).join("rawimages.json")
}

/// [`plan_import_in`] with the default output root.
pub fn plan_import(root: &Path, subject: &str, items: &[ImportItem]) -> ImportResult<ImportPlan> {
    plan_import_in(root, None, subject, items)
}

/// Validate `items` for `subject` and compute destinations and index rows; writes nothing.
/// `output_dir` is the pipeline output root ([`crate::paths::derivatives_root`]) holding the
/// Lead-DBS prefs index and the locks an import invalidates.
pub fn plan_import_in(
    root: &Path,
    output_dir: Option<&Path>,
    subject: &str,
    items: &[ImportItem],
) -> ImportResult<ImportPlan> {
    plan_import_with(root, output_dir, subject, items, &[])
}

/// [`plan_import_in`] that also archives the DICOM folders `sourcedata_dirs` to
/// `sourcedata/sub-<id>/DICOM/<folder name>/` (a counter is appended when the name is taken).
pub fn plan_import_with(
    root: &Path,
    output_dir: Option<&Path>,
    subject: &str,
    items: &[ImportItem],
    sourcedata_dirs: &[PathBuf],
) -> ImportResult<ImportPlan> {
    let sid = bids::parse_subj_id(&folder_sub(subject))?;
    let folder = format!("sub-{sid}");
    let subject_exists = root.join("rawdata").join(&folder).is_dir();
    if items.is_empty() {
        return Err(ImportError::new(E_IMPORT_RULES, "no images to import"));
    }

    let index_path = index_file(root, &folder);
    let existing_rows = if index_path.is_file() {
        bids::load_rawimages(&index_path).map_err(|e| ImportError::new(E_IMPORT_FILE, e.message))?
    } else {
        Vec::new()
    };
    let mut warnings = Vec::new();
    let mut seen: HashSet<(Session, String, Option<Acq>)> = HashSet::new();
    let mut files = Vec::with_capacity(items.len());
    let mut rows = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        nifti_summary(&item.source)?;
        if let Some(sc) = &item.sidecar {
            if !sc.is_file() {
                return Err(file_err(sc, "sidecar is not a file"));
            }
        }
        if !suffix_choices(item.session).contains(&item.suffix.as_str()) {
            return Err(ImportError::new(
                E_IMPORT_RULES,
                format!(
                    "{}: suffix {:?} is not allowed for {} images (choose one of {})",
                    item.source.display(),
                    item.suffix,
                    item.session.label(),
                    suffix_choices(item.session).join(", ")
                ),
            ));
        }
        if item.suffix == "CT" && item.acq.is_some() {
            return Err(ImportError::new(
                E_IMPORT_RULES,
                format!("{}: CT images carry no orientation", item.source.display()),
            ));
        }
        if is_postop_mr(item) && !matches!(item.acq, Some(Acq::Ax | Acq::Cor | Acq::Sag)) {
            return Err(ImportError::new(
                E_IMPORT_RULES,
                format!(
                    "{}: a post-op MR image needs an orientation (axial, coronal or sagittal)",
                    item.source.display()
                ),
            ));
        }
        if !seen.insert((item.session, item.suffix.clone(), item.acq)) {
            return Err(ImportError::new(
                E_IMPORT_RULES,
                format!(
                    "duplicate image: {} {}{} appears more than once",
                    item.session.label(),
                    item.suffix,
                    item.acq
                        .map(|a| format!(" ({})", a.label().to_lowercase()))
                        .unwrap_or_default()
                ),
            ));
        }
        let stem = dest_name(&sid, item);
        let rel = format!(
            "rawdata/{folder}/ses-{}/anat/{stem}.nii.gz",
            item.session.as_str()
        );
        let sidecar_rel = item.sidecar.as_ref().map(|_| {
            format!(
                "rawdata/{folder}/ses-{}/anat/{stem}.json",
                item.session.as_str()
            )
        });
        let norm_rel = norm_index_path(&rel);
        for r in std::iter::once(&rel).chain(sidecar_rel.iter()) {
            let abs = std::path::absolute(root.join(r)).unwrap_or_else(|_| root.join(r));
            if abs.to_string_lossy().chars().count() > 240 {
                warnings.push(format!(
                    "the destination path of {} is longer than 240 characters and may fail on Windows",
                    abs.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        files.push(PlannedFile {
            item: i,
            exists: root.join(&rel).is_file(),
            sidecar_exists: sidecar_rel.as_ref().is_some_and(|s| root.join(s).is_file()),
            index_row_exists: existing_rows
                .iter()
                .any(|r| norm_index_path(&r.path) == norm_rel),
            relative_path: rel.clone(),
            sidecar_relative_path: sidecar_rel,
        });
        rows.push(row_for(item, &rel));
    }

    if !items.iter().any(|i| i.session == Session::Preop) {
        let has_anchor = subject_exists
            && !matches!(
                bids::resolve_subject_in(root, output_dir, &folder, true),
                Err(ref e) if e.code == bids::E_NO_ANCHOR
            );
        if !has_anchor {
            return Err(ImportError::new(
                bids::E_NO_ANCHOR,
                format!(
                    "no pre-op image: {folder} would have no pre-op anchor, add a pre-op \
                     anatomical image (e.g. T1w)"
                ),
            ));
        }
    }
    let planned: Vec<PathBuf> = files.iter().map(|f| root.join(&f.relative_path)).collect();
    let mut merged: Vec<RawImageEntry> = existing_rows
        .iter()
        .filter(|old| !rows.iter().any(|n| row_clash(old, n)))
        .cloned()
        .collect();
    merged.extend(rows.iter().cloned());
    let prefs = bids::prefs_rawimages_path(
        &crate::paths::derivatives_root(root, output_dir).join(&folder),
        &sid,
    );
    if prefs.is_file() {
        merged.extend(bids::load_leaddbs_prefs_index_with(
            &prefs, root, &sid, &planned,
        )?);
    }
    if let Err(e) = bids::check_duplicates(&merged, &sid) {
        return Err(ImportError::new(
            bids::E_DUPL_RAW,
            format!(
                "{}: after the import the subject would not resolve, rows of {} or of the \
                 Lead-DBS index {} share a session, type and orientation; remove the duplicate \
                 row or leave the image out",
                e.message,
                index_path.display(),
                prefs.display()
            ),
        ));
    }
    for old in &existing_rows {
        if rows.iter().any(|n| row_clash(old, n)) {
            warnings.push(format!(
                "the existing index row for {} will be replaced",
                old.path
            ));
        }
    }

    let sourcedata = plan_archives(root, &folder, sourcedata_dirs, &mut warnings)?;

    Ok(ImportPlan {
        root: root.to_path_buf(),
        output_dir: output_dir.map(Path::to_path_buf),
        subject_id: sid,
        subject_exists,
        items: items.to_vec(),
        files,
        rows,
        sourcedata,
        warnings,
    })
}

fn sanitize_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(128)
        .collect::<String>()
        .trim_start_matches('.')
        .to_owned()
}

fn plan_archives(
    root: &Path,
    folder: &str,
    dirs: &[PathBuf],
    warnings: &mut Vec<String>,
) -> ImportResult<Vec<PlannedArchive>> {
    let subject_src = root.join("sourcedata").join(folder);
    let subject_src_canon = std::fs::canonicalize(&subject_src).ok();
    let mut out: Vec<PlannedArchive> = Vec::new();
    for dir in dirs {
        if !dir.is_dir() {
            return Err(file_err(dir, "DICOM folder to keep is not a folder"));
        }
        let canon = std::fs::canonicalize(dir).map_err(|e| file_err(dir, e))?;
        if subject_src_canon
            .as_ref()
            .is_some_and(|s| canon.starts_with(s))
        {
            warnings.push(format!(
                "{} is already under {}, it is not copied again",
                dir.display(),
                subject_src.display()
            ));
            continue;
        }
        let mut base = sanitize_name(
            &canon
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        if base.is_empty() {
            base = "dicom".to_owned();
        }
        let rel_of = |n: usize| {
            let name = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            format!("sourcedata/{folder}/DICOM/{name}")
        };
        let mut n = 1;
        while root.join(rel_of(n)).exists() || out.iter().any(|a| a.relative_path == rel_of(n)) {
            n += 1;
        }
        if n > 1 && root.join(rel_of(1)).exists() {
            warnings.push(format!(
                "{} exists already, the DICOM folder is kept as {}",
                rel_of(1),
                rel_of(n)
            ));
        }
        out.push(PlannedArchive {
            source: canon,
            relative_path: rel_of(n),
        });
    }
    Ok(out)
}

#[derive(Default)]
struct Journal {
    created: Vec<PathBuf>,
    replaced: Vec<(PathBuf, PathBuf)>,
    trees: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    index: Option<(PathBuf, Option<Vec<u8>>)>,
}

impl Journal {
    fn rollback(&self) -> Vec<String> {
        let mut failures = Vec::new();
        let mut note = |what: &str, path: &Path, r: std::io::Result<()>| {
            if let Err(e) = r {
                if e.kind() != std::io::ErrorKind::NotFound {
                    failures.push(format!("{what} {}: {e}", path.display()));
                }
            }
        };
        if let Some((path, previous)) = &self.index {
            match previous {
                Some(bytes) => {
                    let tmp = partial_of(path);
                    let r = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
                    if r.is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                    note("cannot restore", path, r);
                }
                None => note("cannot remove", path, std::fs::remove_file(path)),
            }
        }
        for p in self.created.iter().rev() {
            note("cannot remove", p, std::fs::remove_file(p));
        }
        for (dest, backup) in self.replaced.iter().rev() {
            note("cannot remove", dest, std::fs::remove_file(dest));
            note("cannot restore", dest, std::fs::rename(backup, dest));
        }
        for t in self.trees.iter().rev() {
            note("cannot remove folder", t, std::fs::remove_dir_all(t));
        }
        for d in self.dirs.iter().rev() {
            note("cannot remove folder", d, std::fs::remove_dir(d));
        }
        failures
    }

    fn commit(&self) {
        for (_, backup) in &self.replaced {
            let _ = std::fs::remove_file(backup);
        }
    }

    fn ensure_dir(&mut self, dir: &Path) -> ImportResult<()> {
        let mut missing: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|a| !a.as_os_str().is_empty() && !a.exists())
            .map(Path::to_path_buf)
            .collect();
        missing.reverse();
        for d in missing {
            match std::fs::create_dir(&d) {
                Ok(()) => self.dirs.push(d),
                Err(e) if d.is_dir() && e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(file_err(&d, format!("cannot create folder: {e}"))),
            }
        }
        Ok(())
    }

    fn stage(
        &mut self,
        dest: &Path,
        write: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> ImportResult<()> {
        let tmp = partial_of(dest);
        if let Err(e) = write(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Err(file_err(dest, format!("write failed: {e}")));
        }
        let existed = dest.exists();
        if existed {
            let name = dest.file_name().unwrap_or_default().to_string_lossy();
            let backup = dest.with_file_name(format!("{PARTIAL_PREFIX}backup-{name}"));
            let _ = std::fs::remove_file(&backup);
            if let Err(e) = std::fs::rename(dest, &backup) {
                let _ = std::fs::remove_file(&tmp);
                return Err(file_err(dest, format!("cannot back up existing file: {e}")));
            }
            self.replaced.push((dest.to_path_buf(), backup));
        }
        if let Err(e) = std::fs::rename(&tmp, dest) {
            let _ = std::fs::remove_file(&tmp);
            return Err(file_err(dest, format!("cannot move into place: {e}")));
        }
        if !existed {
            self.created.push(dest.to_path_buf());
        }
        Ok(())
    }
}

const CHUNK: usize = 1 << 20;

const PLAIN_COMPRESSION_LEVEL: u32 = 3;

struct Tee<R, W> {
    reader: R,
    writer: W,
}

impl<R: Read, W: Write> Read for Tee<R, W> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(buf)?;
        self.writer.write_all(&buf[..n])?;
        Ok(n)
    }
}

fn cancelled_io() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")
}

fn copy_verified_gz(
    src: std::fs::File,
    out: BufWriter<std::fs::File>,
    cancel: &AtomicBool,
) -> std::io::Result<u64> {
    let tee = Tee {
        reader: src,
        writer: out,
    };
    let mut decoder = BufGzDecoder::new(BufReader::with_capacity(CHUNK, tee));
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled_io());
        }
        let n = decoder.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
    }
    let mut rest = decoder.into_inner();
    std::io::copy(&mut rest, &mut std::io::sink())?;
    rest.into_inner().writer.flush()?;
    Ok(total)
}

fn compress_plain(
    mut src: std::fs::File,
    out: BufWriter<std::fs::File>,
    cancel: &AtomicBool,
) -> std::io::Result<u64> {
    let mut enc = ParGzWriter::new(out, PLAIN_COMPRESSION_LEVEL);
    let mut buf = vec![0u8; 4 * CHUNK];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled_io());
        }
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        enc.write_all(&buf[..n])?;
        total += n as u64;
    }
    enc.finish()?.flush()?;
    Ok(total)
}

fn write_image(source: &Path, tmp: &Path, needed: u64, cancel: &AtomicBool) -> std::io::Result<()> {
    let src = std::fs::File::open(source)?;
    let out = BufWriter::with_capacity(CHUNK, std::fs::File::create(tmp)?);
    let total = if is_gz(source) {
        copy_verified_gz(src, out, cancel)?
    } else {
        compress_plain(src, out, cancel)?
    };
    if total < needed {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("image data is truncated: {total} bytes, header needs {needed}"),
        ));
    }
    Ok(())
}

fn check_cancel(cancel: &AtomicBool) -> ImportResult<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(ImportError::new(
            E_IMPORT_CANCELLED,
            "import cancelled, nothing was kept",
        ))
    } else {
        Ok(())
    }
}

fn rows_to_json(rows: &[RawImageEntry]) -> Value {
    Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "path": r.path,
                    "session": r.session,
                    "datatype": r.datatype,
                    "entities": Value::Object(r.entities.clone()),
                })
            })
            .collect(),
    )
}

fn run_inner(
    plan: &ImportPlan,
    replace_existing: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportProgress),
    j: &mut Journal,
) -> ImportResult<ImportReport> {
    check_cancel(cancel)?;
    let folder = plan.subject_folder();
    let root = &plan.root;
    if !replace_existing {
        let clash: Vec<&str> = plan
            .files
            .iter()
            .filter(|f| {
                f.exists
                    || f.sidecar_exists
                    || f.index_row_exists
                    || root.join(&f.relative_path).exists()
                    || f.sidecar_relative_path
                        .as_ref()
                        .is_some_and(|s| root.join(s).exists())
            })
            .map(|f| f.relative_path.as_str())
            .collect();
        if !clash.is_empty() {
            return Err(ImportError::new(
                E_IMPORT_RULES,
                format!("files already exist: {}", clash.join(", ")),
            ));
        }
    }
    let index_path = index_file(root, &folder);
    let previous = match std::fs::read(&index_path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(file_err(&index_path, format!("cannot read index: {e}"))),
    };
    let mut rows = if previous.is_some() {
        bids::load_rawimages(&index_path).map_err(|e| ImportError::new(E_IMPORT_FILE, e.message))?
    } else {
        Vec::new()
    };

    let total = plan.files.len()
        + plan
            .files
            .iter()
            .filter(|f| f.sidecar_relative_path.is_some())
            .count()
        + plan.sourcedata.len()
        + 1;
    let mut done = 0usize;
    let mut written = Vec::new();
    let mut replaced: HashSet<Session> = HashSet::new();

    j.ensure_dir(root)?;
    write_dataset_description(root, j)?;

    for a in &plan.sourcedata {
        check_cancel(cancel)?;
        let dest = root.join(&a.relative_path);
        progress(ImportProgress {
            done,
            total,
            current: a.relative_path.clone(),
        });
        if let Some(parent) = dest.parent() {
            j.ensure_dir(parent)?;
        }
        if dest.exists() {
            return Err(file_err(&dest, "exists already"));
        }
        let staged = partial_of(&dest);
        if staged.exists() {
            std::fs::remove_dir_all(&staged)
                .map_err(|e| file_err(&staged, format!("cannot remove a stale copy: {e}")))?;
        }
        j.trees.push(staged.clone());
        copy_tree(&a.source, &staged, cancel).map_err(|e| {
            if cancel.load(Ordering::Relaxed) {
                ImportError::new(E_IMPORT_CANCELLED, "import cancelled, nothing was kept")
            } else {
                file_err(&a.source, format!("cannot copy to {}: {e}", dest.display()))
            }
        })?;
        std::fs::rename(&staged, &dest)
            .map_err(|e| file_err(&dest, format!("cannot move into place: {e}")))?;
        if let Some(t) = j.trees.last_mut() {
            t.clone_from(&dest);
        }
        written.push(dest);
        done += 1;
    }

    for pf in &plan.files {
        let item = plan
            .items
            .get(pf.item)
            .ok_or_else(|| ImportError::new(E_IMPORT_RULES, "plan refers to a missing item"))?;
        check_cancel(cancel)?;
        let dest = root.join(&pf.relative_path);
        if let Some(parent) = dest.parent() {
            j.ensure_dir(parent)?;
        }
        progress(ImportProgress {
            done,
            total,
            current: dest
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        });
        let needed = expected_len(&read_header(&item.source)?);
        if dest.is_file() {
            replaced.insert(item.session);
        }
        j.stage(&dest, |tmp| write_image(&item.source, tmp, needed, cancel))
            .map_err(|e| {
                if cancel.load(Ordering::Relaxed) {
                    ImportError::new(E_IMPORT_CANCELLED, "import cancelled, nothing was kept")
                } else if item.source.is_file() {
                    e
                } else {
                    file_err(&item.source, "source file is missing")
                }
            })?;
        written.push(dest);
        done += 1;
        if let (Some(rel), Some(sc)) = (&pf.sidecar_relative_path, &item.sidecar) {
            check_cancel(cancel)?;
            let sdest = root.join(rel);
            progress(ImportProgress {
                done,
                total,
                current: sdest
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            });
            j.stage(&sdest, |tmp| std::fs::write(tmp, std::fs::read(sc)?))?;
            written.push(sdest);
            done += 1;
        }
    }

    check_cancel(cancel)?;
    progress(ImportProgress {
        done,
        total,
        current: "rawimages.json".to_owned(),
    });
    rows.retain(|old| {
        let clash = plan.rows.iter().any(|n| row_clash(old, n));
        if clash {
            replaced.extend(
                Session::ALL
                    .into_iter()
                    .filter(|s| s.as_str() == old.session),
            );
        }
        !clash
    });
    rows.extend(plan.rows.iter().cloned());
    let body =
        serde_json::to_string_pretty(&rows_to_json(&rows)).map_err(|e| file_err(&index_path, e))?;
    j.index = Some((index_path.clone(), previous));
    let tmp = partial_of(&index_path);
    if let Err(e) = std::fs::write(&tmp, body.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(file_err(&index_path, format!("write failed: {e}")));
    }
    if let Err(e) = std::fs::rename(&tmp, &index_path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(file_err(
            &index_path,
            format!("cannot move into place: {e}"),
        ));
    }

    let sub = bids::resolve_subject_in(root, plan.output_dir.as_deref(), &folder, true)?;
    let first: &[Stage] = if replaced.contains(&Session::Preop) {
        &[Stage::Bids, Stage::Preprocess]
    } else if replaced.contains(&Session::Postop) {
        &[Stage::Coregister]
    } else {
        &[]
    };
    let invalidation = invalidate_from(&sub, first, "import");
    done += 1;
    progress(ImportProgress {
        done,
        total,
        current: String::new(),
    });
    Ok(ImportReport {
        subject_folder: folder,
        written,
        index_path,
        invalidation,
    })
}

fn write_dataset_description(root: &Path, j: &mut Journal) -> ImportResult<()> {
    let desc = root.join("dataset_description.json");
    let generated_by = json!([{
        "Name": crate::version::PRODUCT,
        "Version": crate::version::version_string(),
    }]);
    let body = if desc.exists() {
        let Some(mut obj) = std::fs::read(&desc)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            })
        else {
            return Ok(());
        };
        if obj.contains_key("GeneratedBy") {
            return Ok(());
        }
        obj.insert("GeneratedBy".to_owned(), generated_by);
        Value::Object(obj)
    } else {
        let name = std::path::absolute(root)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "dataset".to_owned());
        json!({
            "Name": name,
            "BIDSVersion": "1.9.0",
            "DatasetType": "raw",
            "GeneratedBy": generated_by,
        })
    };
    let text = serde_json::to_string_pretty(&body).map_err(|e| file_err(&desc, e))?;
    j.stage(&desc, |tmp| std::fs::write(tmp, text.as_bytes()))
}

fn copy_tree(src: &Path, dst: &Path, cancel: &AtomicBool) -> std::io::Result<()> {
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf(), 0usize)];
    while let Some((from, to, depth)) = stack.pop() {
        std::fs::create_dir(&to)?;
        for e in std::fs::read_dir(&from)? {
            if cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "cancelled",
                ));
            }
            let e = e?;
            let ft = e.file_type()?;
            let target = to.join(e.file_name());
            if ft.is_dir() && !ft.is_symlink() {
                if depth < 32 {
                    stack.push((e.path(), target, depth + 1));
                }
            } else if ft.is_file() {
                std::fs::copy(e.path(), &target)?;
            }
        }
    }
    Ok(())
}

/// Write the planned files and merge the index; the subject must resolve afterwards.
///
/// Every created file is removed and every replaced file and the previous index restored on
/// error or cancel (`specs/01` §8.4).
pub fn run_import(
    plan: &ImportPlan,
    replace_existing: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportProgress),
) -> ImportResult<ImportReport> {
    let mut journal = Journal::default();
    match run_inner(plan, replace_existing, cancel, progress, &mut journal) {
        Ok(r) => {
            journal.commit();
            Ok(r)
        }
        Err(mut e) => {
            let failures = journal.rollback();
            if !failures.is_empty() {
                e.message
                    .push_str(&format!(" (rollback incomplete: {})", failures.join("; ")));
            }
            Err(e)
        }
    }
}

/// One subject of an [`ImportBatch`].
#[derive(Clone, Debug)]
pub struct SubjectImport {
    pub subject_id: String,
    pub items: Vec<ImportItem>,
    pub replace_existing: bool,
    /// Copy `sourcedata_dirs` to `sourcedata/sub-<id>/DICOM/` (default `true`).
    pub keep_sourcedata: bool,
    /// DICOM folders the items were converted from.
    pub sourcedata_dirs: Vec<PathBuf>,
}

impl SubjectImport {
    pub fn new(subject_id: impl Into<String>, items: Vec<ImportItem>) -> Self {
        SubjectImport {
            subject_id: subject_id.into(),
            items,
            replace_existing: false,
            keep_sourcedata: true,
            sourcedata_dirs: Vec::new(),
        }
    }
}

/// Several subjects imported one after another into one dataset.
#[derive(Clone, Debug)]
pub struct ImportBatch {
    pub root: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub subjects: Vec<SubjectImport>,
}

/// Progress of a running batch: subject `subject` of `subjects` (0-based) and its files.
#[derive(Clone, Debug, Default)]
pub struct BatchProgress {
    pub subject: usize,
    pub subjects: usize,
    pub subject_id: String,
    pub file: ImportProgress,
}

fn plan_subject(
    batch: &ImportBatch,
    s: &SubjectImport,
    seen: &mut HashSet<String>,
) -> ImportResult<ImportPlan> {
    let dirs: &[PathBuf] = if s.keep_sourcedata {
        &s.sourcedata_dirs
    } else {
        &[]
    };
    let plan = plan_import_with(
        &batch.root,
        batch.output_dir.as_deref(),
        &s.subject_id,
        &s.items,
        dirs,
    )?;
    if !seen.insert(plan.subject_id.clone()) {
        return Err(ImportError::new(
            E_IMPORT_RULES,
            format!(
                "subject {} appears more than once in the batch",
                plan.subject_folder()
            ),
        ));
    }
    Ok(plan)
}

/// Plan every subject of `batch`; one result per subject, in order. A subject id that occurs
/// twice fails for its second occurrence (`E_IMPORT_RULES`).
pub fn plan_import_batch(batch: &ImportBatch) -> Vec<ImportResult<ImportPlan>> {
    let mut seen = HashSet::new();
    batch
        .subjects
        .iter()
        .map(|s| plan_subject(batch, s, &mut seen))
        .collect()
}

/// Plan and import the subjects of `batch` one after another, each with its own journal: a
/// failed subject is rolled back and the next one runs, a cancel rolls back the current subject
/// and skips the rest (`E_IMPORT_CANCELLED`). One result per subject, in order.
pub fn run_import_batch(
    batch: &ImportBatch,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&BatchProgress),
) -> Vec<ImportResult<ImportReport>> {
    let mut seen = HashSet::new();
    let subjects = batch.subjects.len();
    let mut out = Vec::with_capacity(subjects);
    for (i, s) in batch.subjects.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            out.push(Err(ImportError::new(
                E_IMPORT_CANCELLED,
                format!("import cancelled before sub-{}", s.subject_id),
            )));
            continue;
        }
        let result = plan_subject(batch, s, &mut seen).and_then(|plan| {
            let subject_id = plan.subject_id.clone();
            run_import(&plan, s.replace_existing, cancel, &mut |file| {
                progress(&BatchProgress {
                    subject: i,
                    subjects,
                    subject_id: subject_id.clone(),
                    file,
                });
            })
        });
        out.push(result);
    }
    out
}

/// Locate `dcm2niix`: `DBSLOC_DCM2NIIX_BIN` (a file), the bundled copy, then `PATH`.
pub fn find_dcm2niix() -> Option<PathBuf> {
    if let Ok(v) = std::env::var(DCM2NIIX_ENV) {
        let p = PathBuf::from(v.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    crate::bundled_bin::find_bundled_in("dcm2niix", "dcm2niix")
        .or_else(|| crate::bundled_bin::find_on_path("dcm2niix"))
}

/// Whether any file below `dir` looks like DICOM (`.dcm` / `.ima` extension or `DICM` magic).
pub fn looks_like_dicom(dir: &Path) -> bool {
    fn is_dicom_file(p: &Path) -> bool {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if ext == "dcm" || ext == "ima" {
            return true;
        }
        let Ok(mut f) = std::fs::File::open(p) else {
            return false;
        };
        let mut b = [0u8; 132];
        f.read_exact(&mut b).is_ok() && &b[128..132] == b"DICM"
    }
    fn walk(dir: &Path, depth: usize) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return false;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if depth < 32 && walk(&e.path(), depth + 1) {
                    return true;
                }
            } else if ft.is_file() && is_dicom_file(&e.path()) {
                return true;
            }
        }
        false
    }
    walk(dir, 0)
}

/// DICOM metadata of one converted series, read from its sidecar and header, for display.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct SeriesInfo {
    pub series_number: Option<u32>,
    pub description: String,
    pub protocol: String,
    /// DICOM `Modality` (`MR`, `CT`, …); empty when the sidecar has none.
    pub modality: String,
    /// `YYYY-MM-DDTHH:MM:SS` (or `YYYY-MM-DD` when no time is known).
    pub acquisition_datetime: Option<String>,
    /// `YYYY-MM-DD`: `StudyDate`, else the acquisition date.
    pub study_date: Option<String>,
    pub image_type: Vec<String>,
    pub manufacturer: Option<String>,
    pub magnetic_field_strength: Option<f64>,
    pub echo_time: Option<f64>,
    pub repetition_time: Option<f64>,
    pub inversion_time: Option<f64>,
    pub slice_thickness: Option<f64>,
    /// `[0; 3]` when the header fails validation.
    pub dims: [u32; 3],
    /// `[0.0; 3]` when the header fails validation.
    pub voxel_mm: [f32; 3],
}

/// One NIfTI series produced by the DICOM converter.
#[derive(Clone, Debug)]
pub struct ConvertedSeries {
    pub nifti: PathBuf,
    pub sidecar: Option<PathBuf>,
    pub series_number: Option<u32>,
    pub description: String,
    pub protocol: String,
    pub modality: String,
    /// `[0; 3]` when the header fails validation (the series is still returned).
    pub dims: [u32; 3],
    /// `[0.0; 3]` when the header fails validation.
    pub voxel_mm: [f32; 3],
    pub info: SeriesInfo,
    /// Starting values of the row: the suggestion when [`ConvertedSeries::assigned`], else a
    /// pre-op `T1w` placeholder that is not imported until the user assigns it.
    pub suggested: ImportItem,
    /// A suggestion rule matched; `false` = unassigned (`specs/01` §8.4).
    pub assigned: bool,
}

impl ConvertedSeries {
    /// The suggestion, `None` for an unassigned series.
    pub fn suggestion(&self) -> Option<&ImportItem> {
        self.assigned.then_some(&self.suggested)
    }
}

/// One suggestion rule: a keyword hit assigns `session` and `suffix`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupRule {
    pub session: Session,
    pub suffix: String,
    pub keywords: Vec<String>,
}

/// Ordered suggestion rules; the last rule with a keyword hit wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupTable {
    pub rules: Vec<LookupRule>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    session: String,
    suffix: String,
    keywords: Vec<String>,
}

/// The built-in suggestion rules (`specs/13` §4.2, adapted).
pub fn default_lookup_table() -> LookupTable {
    let r = |session, suffix: &str, keywords: &[&str]| LookupRule {
        session,
        suffix: suffix.to_owned(),
        keywords: keywords.iter().map(|k| (*k).to_owned()).collect(),
    };
    LookupTable {
        rules: vec![
            r(Session::Preop, "T1w", &["t1", "mprage"]),
            r(Session::Preop, "T2w", &["t2", "tse"]),
            r(Session::Preop, "FGATIR", &["fgatir"]),
            r(Session::Preop, "PDw", &["pd"]),
            r(Session::Preop, "T2starw", &["t2star", "t2*", "swi"]),
            r(Session::Preop, "FLAIR", &["flair"]),
            r(Session::Postop, "CT", &["ct"]),
        ],
    }
}

impl LookupTable {
    /// Parse the JSON form: an array of `{"session", "suffix", "keywords"}` objects.
    pub fn from_json(text: &str) -> Result<LookupTable, String> {
        let raw: Vec<RawRule> = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut rules = Vec::with_capacity(raw.len());
        for (i, r) in raw.into_iter().enumerate() {
            let session = match r.session.trim().to_ascii_lowercase().as_str() {
                "preop" => Session::Preop,
                "postop" => Session::Postop,
                other => {
                    return Err(format!(
                        "rule {}: session {other:?} is not preop or postop",
                        i + 1
                    ))
                }
            };
            let suffix = suffix_choices(session)
                .iter()
                .find(|s| s.eq_ignore_ascii_case(r.suffix.trim()))
                .ok_or_else(|| {
                    format!(
                        "rule {}: suffix {:?} is not allowed for {} images (choose one of {})",
                        i + 1,
                        r.suffix,
                        session.as_str(),
                        suffix_choices(session).join(", ")
                    )
                })?;
            rules.push(LookupRule {
                session,
                suffix: (*suffix).to_owned(),
                keywords: r
                    .keywords
                    .into_iter()
                    .map(|k| k.trim().to_owned())
                    .filter(|k| !k.is_empty())
                    .collect(),
            });
        }
        Ok(LookupTable { rules })
    }

    /// The JSON form read by [`LookupTable::from_json`].
    pub fn to_json(&self) -> Value {
        Value::Array(
            self.rules
                .iter()
                .map(|r| {
                    json!({
                        "session": r.session.as_str(),
                        "suffix": r.suffix,
                        "keywords": r.keywords,
                    })
                })
                .collect(),
        )
    }

    pub fn load(path: &Path) -> ImportResult<LookupTable> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| file_err(path, format!("cannot read the lookup table: {e}")))?;
        LookupTable::from_json(&text).map_err(|e| file_err(path, format!("bad lookup table: {e}")))
    }

    /// The table named by `DBSLOC_IMPORT_LOOKUP_JSON`, else [`default_lookup_table`].
    pub fn from_env() -> ImportResult<LookupTable> {
        match std::env::var_os(LOOKUP_ENV).filter(|v| !v.is_empty()) {
            Some(p) => LookupTable::load(Path::new(&p)),
            None => Ok(default_lookup_table()),
        }
    }
}

impl Default for LookupTable {
    fn default() -> Self {
        default_lookup_table()
    }
}

fn read_sidecar(path: Option<&Path>) -> Option<Value> {
    let text = std::fs::read_to_string(path?).ok()?;
    serde_json::from_str(&text).ok()
}

fn json_str(v: &Option<Value>, key: &str) -> String {
    v.as_ref()
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn json_opt_str(v: &Option<Value>, key: &str) -> Option<String> {
    Some(json_str(v, key)).filter(|s| !s.is_empty())
}

fn json_f64(v: &Option<Value>, key: &str) -> Option<f64> {
    let x = v.as_ref()?.get(key)?;
    x.as_f64()
        .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|f: &f64| f.is_finite())
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

fn digits(s: &str) -> Option<u32> {
    s.chars()
        .all(|c| c.is_ascii_digit())
        .then(|| s.parse().ok())
        .flatten()
}

/// `YYYY-MM-DD` from `YYYY-MM-DD…` or `YYYYMMDD…`.
fn iso_date(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (y, m, d) = if raw.get(4..5) == Some("-") {
        (raw.get(0..4)?, raw.get(5..7)?, raw.get(8..10)?)
    } else {
        (raw.get(0..4)?, raw.get(4..6)?, raw.get(6..8)?)
    };
    let (yy, mm, dd) = (digits(y)?, digits(m)?, digits(d)?);
    ((1900..=2200).contains(&yy) && (1..=12).contains(&mm) && (1..=31).contains(&dd))
        .then(|| format!("{y}-{m}-{d}"))
}

/// `HH:MM:SS` from `HH:MM:SS(.f)` or `HHMMSS(.f)`.
fn iso_time(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (h, m, s) = if raw.get(2..3) == Some(":") {
        (raw.get(0..2)?, raw.get(3..5)?, raw.get(6..8)?)
    } else {
        (raw.get(0..2)?, raw.get(2..4)?, raw.get(4..6)?)
    };
    let (hh, mm, ss) = (digits(h)?, digits(m)?, digits(s)?);
    (hh < 24 && mm < 60 && ss < 61).then(|| format!("{h}:{m}:{s}"))
}

fn date_time(date: String, time: Option<String>) -> String {
    match time {
        Some(t) => format!("{date}T{t}"),
        None => date,
    }
}

fn datetime_from_stem(stem: &str) -> Option<(String, String)> {
    stem.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() == 14 && t.chars().all(|c| c.is_ascii_digit()))
        .find_map(|t| {
            let d = iso_date(&t[..8])?;
            let time = iso_time(&t[8..])?;
            Some((d.clone(), format!("{d}T{time}")))
        })
}

fn nifti_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_lowercase();
    let cut = if lower.ends_with(".nii.gz") {
        name.len() - 7
    } else if lower.ends_with(".nii") {
        name.len() - 4
    } else {
        name.len()
    };
    name.get(..cut).unwrap_or(&name).to_owned()
}

/// Display metadata of a NIfTI series from its sidecar, header and file name.
pub fn series_info(nifti: &Path, sidecar: Option<&Path>) -> SeriesInfo {
    let side = read_sidecar(sidecar);
    let series_number = side
        .as_ref()
        .and_then(|v| v.get("SeriesNumber"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .and_then(|n| u32::try_from(n).ok());
    let time_of = |keys: &[&str]| keys.iter().find_map(|k| iso_time(&json_str(&side, k)));
    let from_sidecar = {
        let adt = json_str(&side, "AcquisitionDateTime");
        iso_date(&adt)
            .map(|d| {
                let t = adt
                    .split_once('T')
                    .and_then(|(_, t)| iso_time(t))
                    .or_else(|| time_of(&["AcquisitionTime"]));
                (d.clone(), date_time(d, t))
            })
            .or_else(|| {
                [
                    ("AcquisitionDate", "AcquisitionTime"),
                    ("SeriesDate", "SeriesTime"),
                    ("ContentDate", "ContentTime"),
                    ("StudyDate", "StudyTime"),
                ]
                .iter()
                .find_map(|(dk, tk)| {
                    let d = iso_date(&json_str(&side, dk))?;
                    Some((d.clone(), date_time(d, time_of(&["AcquisitionTime", tk]))))
                })
            })
    };
    let acquired = from_sidecar.or_else(|| datetime_from_stem(&nifti_stem(nifti)));
    let study_date = iso_date(&json_str(&side, "StudyDate"))
        .or_else(|| acquired.as_ref().map(|(d, _)| d.clone()));
    let image_type = match side.as_ref().and_then(|v| v.get("ImageType")) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) => s
            .split(['\\', ','])
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let (dims, voxel_mm) = nifti_summary(nifti).unwrap_or(([0; 3], [0.0; 3]));
    SeriesInfo {
        series_number,
        description: json_str(&side, "SeriesDescription"),
        protocol: json_str(&side, "ProtocolName"),
        modality: json_str(&side, "Modality"),
        acquisition_datetime: acquired.map(|(_, dt)| dt),
        study_date,
        image_type,
        manufacturer: json_opt_str(&side, "Manufacturer"),
        magnetic_field_strength: json_f64(&side, "MagneticFieldStrength"),
        echo_time: json_f64(&side, "EchoTime"),
        repetition_time: json_f64(&side, "RepetitionTime"),
        inversion_time: json_f64(&side, "InversionTime"),
        slice_thickness: json_f64(&side, "SliceThickness"),
        dims,
        voxel_mm,
    }
}

fn keyword_hits(lower: &str, toks: &[String], keyword: &str) -> bool {
    let k = keyword.trim().to_lowercase();
    if k.is_empty() {
        return false;
    }
    if k.len() <= 3 && k.chars().all(|c| c.is_ascii_alphanumeric()) {
        toks.iter()
            .any(|t| *t == k || t.strip_suffix('w') == Some(k.as_str()))
    } else {
        lower.contains(&k)
    }
}

fn acq_from_name_or_voxels(nifti: &Path) -> Option<Acq> {
    let name = nifti
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    Acq::ALL
        .into_iter()
        .find(|a| name.contains(&format!("acq-{}", a.as_str())))
        .or_else(|| {
            let h = read_header(nifti).ok()?;
            guess_acq(
                [h.pixdim[1].abs(), h.pixdim[2].abs(), h.pixdim[3].abs()],
                directions(&h),
            )
        })
}

fn postop_mr_acq(acq: Option<Acq>) -> Acq {
    match acq {
        Some(a @ (Acq::Ax | Acq::Cor | Acq::Sag)) => a,
        _ => Acq::Ax,
    }
}

fn suggest_with(
    nifti: &Path,
    sidecar: Option<&Path>,
    table: &LookupTable,
    ignore_prefix: Option<&str>,
) -> Option<ImportItem> {
    let side = read_sidecar(sidecar);
    let modality = json_str(&side, "Modality");
    let mut stem = nifti_stem(nifti).to_lowercase();
    if let Some(prefix) = ignore_prefix
        .map(str::to_lowercase)
        .filter(|p| !p.is_empty())
    {
        let cleaned: String = prefix
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        for p in [prefix, cleaned] {
            if let Some(rest) = stem.strip_prefix(&p) {
                stem = rest.to_owned();
                break;
            }
        }
    }
    let text = format!(
        "{} {} {stem}",
        json_str(&side, "SeriesDescription").to_lowercase(),
        json_str(&side, "ProtocolName").to_lowercase()
    );
    let toks = tokens(&text);
    let item = |session, suffix: &str, acq| ImportItem {
        source: nifti.to_path_buf(),
        sidecar: sidecar.map(Path::to_path_buf),
        session,
        suffix: suffix.to_owned(),
        acq,
    };
    if modality.eq_ignore_ascii_case("CT") {
        return Some(item(Session::Postop, "CT", None));
    }
    let tagged_not_ct = !modality.is_empty();
    let rule = table.rules.iter().rev().find(|r| {
        !(r.suffix == "CT" && tagged_not_ct)
            && r.keywords.iter().any(|k| keyword_hits(&text, &toks, k))
    })?;
    if rule.suffix == "CT" {
        return Some(item(Session::Postop, "CT", None));
    }
    let acq = acq_from_name_or_voxels(nifti);
    Some(match rule.session {
        Session::Preop => item(Session::Preop, &rule.suffix, acq),
        Session::Postop => item(Session::Postop, &rule.suffix, Some(postop_mr_acq(acq))),
    })
}

/// Suggest session, suffix and orientation from `table` (`specs/01` §8.4); `None` = unassigned.
///
/// DICOM `Modality == CT` in the sidecar always means post-op CT, and a sidecar naming another
/// modality disables the CT rules. Keywords of up to three letters and digits match whole words
/// (optionally followed by `w`, so `t1` matches `T1w` but `ct` does not match `direct`); longer
/// keywords match anywhere. The text searched is `SeriesDescription`, `ProtocolName` and the
/// file stem; the last rule with a hit wins.
pub fn suggest(nifti: &Path, sidecar: Option<&Path>, table: &LookupTable) -> Option<ImportItem> {
    suggest_with(nifti, sidecar, table, None)
}

/// Placeholder row for an unassigned image: pre-op `T1w` with the orientation guess.
pub fn draft_item(nifti: &Path, sidecar: Option<&Path>) -> ImportItem {
    ImportItem {
        source: nifti.to_path_buf(),
        sidecar: sidecar.map(Path::to_path_buf),
        session: Session::Preop,
        suffix: "T1w".to_owned(),
        acq: acq_from_name_or_voxels(nifti),
    }
}

/// [`suggest`] with the active table ([`LookupTable::from_env`], the default table when that
/// fails), falling back to [`draft_item`] for an unassigned image.
pub fn suggest_item(nifti: &Path, sidecar: Option<&Path>) -> ImportItem {
    let table = LookupTable::from_env().unwrap_or_default();
    suggest(nifti, sidecar, &table).unwrap_or_else(|| draft_item(nifti, sidecar))
}

/// Series grouped by study date.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Study {
    /// `YYYY-MM-DD`; `None` for the series without a date.
    pub date: Option<String>,
    /// Indices into the series list.
    pub series: Vec<usize>,
}

/// Studies of `series` by date, oldest first; series without a date form a last study.
pub fn group_studies(series: &[ConvertedSeries]) -> Vec<Study> {
    let mut by_date: std::collections::BTreeMap<Option<String>, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, s) in series.iter().enumerate() {
        by_date
            .entry(s.info.study_date.clone())
            .or_default()
            .push(i);
    }
    let mut out: Vec<Study> = by_date
        .into_iter()
        .map(|(date, series)| Study { date, series })
        .collect();
    out.sort_by_key(|s| s.date.is_none());
    out
}

/// Date rule: with two or more dated studies, every assigned pre-op MR series of the latest
/// study becomes post-op MR (orientation from the voxels, axial when they are isotropic);
/// returns the indices changed. Post-op CT and unassigned series are left alone.
pub fn apply_latest_study_rule(series: &mut [ConvertedSeries]) -> Vec<usize> {
    let studies = group_studies(series);
    let dated: Vec<&Study> = studies.iter().filter(|s| s.date.is_some()).collect();
    let [.., latest] = dated.as_slice() else {
        return Vec::new();
    };
    if dated.len() < 2 {
        return Vec::new();
    }
    let mut changed = Vec::new();
    for &i in &latest.series {
        let Some(s) = series.get_mut(i) else {
            continue;
        };
        if s.assigned && s.suggested.session == Session::Preop && s.suggested.suffix != "CT" {
            s.suggested.session = Session::Postop;
            s.suggested.acq = Some(postop_mr_acq(s.suggested.acq));
            changed.push(i);
        }
    }
    changed
}

/// Remove every character but `[A-Za-z0-9]` (after a leading `sub-`) from a folder name to
/// propose a subject id (`specs/13` §2.2); the message says what was removed.
pub fn subject_id_from_folder(name: &str) -> (String, Option<String>) {
    let trimmed = name.trim();
    let body = trimmed.strip_prefix("sub-").unwrap_or(trimmed);
    let id: String = body.chars().filter(char::is_ascii_alphanumeric).collect();
    let warning = (id != body).then(|| {
        format!("subject id {id:?} from folder {name:?}: characters other than A-Z, a-z, 0-9 were removed")
    });
    (id, warning)
}

/// `dcm2niix` options before `-o`: gzip, BIDS sidecar, skip derived / localizer / 2-D images,
/// and the name pattern folder_protocol_sequence_datetime_series_description.
pub const DCM2NIIX_ARGS: [&str; 8] = ["-z", "y", "-b", "y", "-i", "y", "-f", "%f_%p_%z_%t_%s_%d"];

fn format_tail(stdout: &str, stderr: &str) -> String {
    let lines: Vec<&str> = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|l| !l.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(20);
    lines[start..].join("\n")
}

fn parse_series(
    out_dir: &Path,
    folder_name: Option<&str>,
    table: &LookupTable,
) -> ImportResult<Vec<ConvertedSeries>> {
    let rd = std::fs::read_dir(out_dir).map_err(|e| {
        ImportError::new(
            E_DCM2NIIX,
            format!("cannot read {}: {e}", out_dir.display()),
        )
    })?;
    let mut niftis: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_nifti_name(p))
        .collect();
    niftis.sort();
    let mut out = Vec::with_capacity(niftis.len());
    for nifti in niftis {
        let stem = nifti_stem(&nifti);
        let sidecar = Some(out_dir.join(format!("{stem}.json"))).filter(|p| p.is_file());
        let info = series_info(&nifti, sidecar.as_deref());
        let suggestion = suggest_with(&nifti, sidecar.as_deref(), table, folder_name);
        let assigned = suggestion.is_some();
        let suggested = suggestion.unwrap_or_else(|| draft_item(&nifti, sidecar.as_deref()));
        out.push(ConvertedSeries {
            series_number: info.series_number,
            description: info.description.clone(),
            protocol: info.protocol.clone(),
            modality: info.modality.clone(),
            dims: info.dims,
            voxel_mm: info.voxel_mm,
            nifti,
            sidecar,
            info,
            suggested,
            assigned,
        });
    }
    out.sort_by(|a, b| {
        (a.series_number.is_none(), a.series_number, &a.nifti).cmp(&(
            b.series_number.is_none(),
            b.series_number,
            &b.nifti,
        ))
    });
    if out.is_empty() {
        return Err(ImportError::new(E_DCM2NIIX, "no NIfTI series produced"));
    }
    Ok(out)
}

fn read_all(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = r.read_to_end(&mut b);
        String::from_utf8_lossy(&b).into_owned()
    })
}

/// Convert a DICOM folder to NIfTI series in `out_dir` with `dcm2niix`; cancellable. Suggestions
/// come from [`LookupTable::from_env`].
pub fn convert_dicom(
    folder: &Path,
    out_dir: &Path,
    cancel: &AtomicBool,
) -> ImportResult<Vec<ConvertedSeries>> {
    convert_dicom_with(folder, out_dir, cancel, &LookupTable::from_env()?)
}

/// [`convert_dicom`] with an explicit suggestion table.
pub fn convert_dicom_with(
    folder: &Path,
    out_dir: &Path,
    cancel: &AtomicBool,
    table: &LookupTable,
) -> ImportResult<Vec<ConvertedSeries>> {
    let bin = find_dcm2niix().ok_or_else(|| {
        ImportError::new(
            E_DCM2NIIX,
            "dcm2niix was not found: set DBSLOC_DCM2NIIX_BIN or install dcm2niix on PATH",
        )
    })?;
    if !folder.is_dir() {
        return Err(file_err(folder, "not a folder"));
    }
    if out_dir.exists()
        && std::fs::read_dir(out_dir)
            .map(|mut r| r.next().is_some())
            .unwrap_or(true)
    {
        return Err(ImportError::new(
            E_DCM2NIIX,
            format!("output folder {} is not empty", out_dir.display()),
        ));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| {
        ImportError::new(
            E_DCM2NIIX,
            format!("cannot create {}: {e}", out_dir.display()),
        )
    })?;
    let result = run_dcm2niix(&bin, folder, out_dir, cancel, table);
    if result.is_err() {
        clean_dir(out_dir);
    }
    result
}

fn clean_dir(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(&p);
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
}

fn run_dcm2niix(
    bin: &Path,
    folder: &Path,
    out_dir: &Path,
    cancel: &AtomicBool,
    table: &LookupTable,
) -> ImportResult<Vec<ConvertedSeries>> {
    check_cancel(cancel)?;

    let mut cmd = Command::new(bin);
    crate::outputs::hide_console_window(&mut cmd)
        .args(DCM2NIIX_ARGS)
        .arg("-o")
        .arg(out_dir)
        .arg(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        ImportError::new(E_DCM2NIIX, format!("cannot start {}: {e}", bin.display()))
    })?;
    let out_h = child.stdout.take().map(read_all);
    let err_h = child.stderr.take().map(read_all);

    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ImportError::new(
                E_IMPORT_CANCELLED,
                "DICOM conversion cancelled",
            ));
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ImportError::new(
                    E_DCM2NIIX,
                    format!("cannot wait for dcm2niix: {e}"),
                ));
            }
        }
    };
    let stdout = out_h.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err_h.and_then(|h| h.join().ok()).unwrap_or_default();
    if !status.success() {
        return Err(ImportError::new(
            E_DCM2NIIX,
            format!(
                "dcm2niix failed ({status}):\n{}",
                format_tail(&stdout, &stderr)
            ),
        ));
    }
    let folder_name = folder.file_name().map(|n| n.to_string_lossy().into_owned());
    parse_series(out_dir, folder_name.as_deref(), table)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct Spec {
        datatype: i16,
        dim0: i16,
        dims: [i16; 3],
        pixdim: [f32; 3],
        qform: i16,
        sform: i16,
        srow: [[f32; 4]; 3],
    }

    impl Default for Spec {
        fn default() -> Self {
            Spec {
                datatype: 2,
                dim0: 3,
                dims: [2, 2, 2],
                pixdim: [1.0, 1.0, 1.0],
                qform: 1,
                sform: 0,
                srow: [[0.0; 4]; 3],
            }
        }
    }

    fn nifti(s: &Spec) -> Vec<u8> {
        let data: usize = s.dims.iter().map(|&d| d.max(1) as usize).product();
        let mut b = vec![0u8; 352 + data];
        b[0..4].copy_from_slice(&348i32.to_le_bytes());
        b[40..42].copy_from_slice(&s.dim0.to_le_bytes());
        for (i, d) in s.dims.iter().enumerate() {
            b[42 + 2 * i..44 + 2 * i].copy_from_slice(&d.to_le_bytes());
        }
        for i in 4..8 {
            b[40 + 2 * i..42 + 2 * i].copy_from_slice(&1i16.to_le_bytes());
        }
        b[70..72].copy_from_slice(&s.datatype.to_le_bytes());
        b[72..74].copy_from_slice(&8i16.to_le_bytes());
        for (i, p) in s.pixdim.iter().enumerate() {
            b[80 + 4 * i..84 + 4 * i].copy_from_slice(&p.to_le_bytes());
        }
        b[108..112].copy_from_slice(&352.0f32.to_le_bytes());
        b[252..254].copy_from_slice(&s.qform.to_le_bytes());
        b[254..256].copy_from_slice(&s.sform.to_le_bytes());
        for (r, row) in s.srow.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let o = 280 + 16 * r + 4 * c;
                b[o..o + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        b[344..348].copy_from_slice(b"n+1\0");
        b
    }

    fn ident() -> [[f32; 4]; 3] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ]
    }

    fn write_nii(dir: &Path, name: &str, s: &Spec) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, nifti(s)).unwrap();
        p
    }

    fn item(source: PathBuf, session: Session, suffix: &str, acq: Option<Acq>) -> ImportItem {
        ImportItem {
            source,
            sidecar: None,
            session,
            suffix: suffix.to_owned(),
            acq,
        }
    }

    fn code(r: ImportResult<impl std::fmt::Debug>) -> &'static str {
        r.unwrap_err().code
    }

    fn gunzip(p: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        GzDecoder::new(std::fs::File::open(p).unwrap())
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    fn no_progress(_: ImportProgress) {}

    #[test]
    fn plan_rejects_bad_input_and_accepts_valid() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let root = d.path().join("ds");
        let good = write_nii(&src, "a.nii", &Spec::default());
        let t1 = |p: &Path| item(p.to_path_buf(), Session::Preop, "T1w", None);

        assert_eq!(
            code(plan_import(&root, "bad_id", &[t1(&good)])),
            bids::E_BIDS_PARSE
        );
        assert_eq!(
            code(plan_import(&root, "01", &[t1(&src.join("missing.nii"))])),
            E_IMPORT_FILE
        );
        let zero = write_nii(
            &src,
            "zero.nii",
            &Spec {
                dims: [2, 0, 2],
                ..Spec::default()
            },
        );
        assert_eq!(code(plan_import(&root, "01", &[t1(&zero)])), E_IMPORT_FILE);
        let noor = write_nii(
            &src,
            "noor.nii",
            &Spec {
                qform: 0,
                sform: 0,
                ..Spec::default()
            },
        );
        assert_eq!(code(plan_import(&root, "01", &[t1(&noor)])), E_IMPORT_FILE);
        let txt = src.join("x.txt");
        std::fs::write(&txt, b"x").unwrap();
        assert_eq!(code(plan_import(&root, "01", &[t1(&txt)])), E_IMPORT_FILE);

        let mr_no_acq = [t1(&good), item(good.clone(), Session::Postop, "T1w", None)];
        assert_eq!(code(plan_import(&root, "01", &mr_no_acq)), E_IMPORT_RULES);
        let mr_iso = [
            t1(&good),
            item(good.clone(), Session::Postop, "T1w", Some(Acq::Iso)),
        ];
        assert_eq!(code(plan_import(&root, "01", &mr_iso)), E_IMPORT_RULES);
        let dup = [t1(&good), t1(&good)];
        assert_eq!(code(plan_import(&root, "01", &dup)), E_IMPORT_RULES);
        let bad_suffix = [item(good.clone(), Session::Preop, "CT", None)];
        assert_eq!(code(plan_import(&root, "01", &bad_suffix)), E_IMPORT_RULES);

        let ok = [t1(&good), item(good.clone(), Session::Postop, "CT", None)];
        let plan = plan_import(&root, "sub-01", &ok).unwrap();
        assert_eq!(plan.subject_id, "01");
        assert_eq!(plan.subject_folder(), "sub-01");
        assert!(!plan.subject_exists);
        assert!(plan.conflicts().is_empty());
        assert!(plan.warnings.is_empty());
        assert_eq!(
            plan.files[0].relative_path,
            "rawdata/sub-01/ses-preop/anat/sub-01_ses-preop_T1w.nii.gz"
        );
        assert_eq!(
            plan.files[1].relative_path,
            "rawdata/sub-01/ses-postop/anat/sub-01_ses-postop_CT.nii.gz"
        );
        assert_eq!(plan.rows[0].path, plan.files[0].relative_path);
        assert_eq!(plan.rows[0].session, "preop");
        assert_eq!(plan.rows[0].entities.get("suffix"), Some(&json!("T1w")));
        assert!(!plan.rows[0].entities.contains_key("modality"));
        assert_eq!(plan.rows[1].entities.get("modality"), Some(&json!("CT")));

        let mr = [
            t1(&good),
            item(good.clone(), Session::Postop, "T1w", Some(Acq::Ax)),
        ];
        let plan = plan_import(&root, "01", &mr).unwrap();
        assert!(plan.files[1]
            .relative_path
            .ends_with("sub-01_ses-postop_acq-ax_T1w.nii.gz"));
        assert_eq!(
            plan.rows[1].entities.get("modality"),
            Some(&json!("ax_MRI"))
        );

        let only_post = [item(good.clone(), Session::Postop, "CT", None)];
        assert_eq!(
            code(plan_import(&root, "01", &only_post)),
            bids::E_NO_ANCHOR
        );
    }

    #[test]
    fn run_import_fresh_root() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let root = d.path().join("ds");
        let t1 = write_nii(&src, "t1.nii", &Spec::default());
        let ct_bytes = nifti(&Spec {
            dims: [3, 3, 3],
            ..Spec::default()
        });
        let ct = src.join("ct.nii.gz");
        {
            let mut e = GzEncoder::new(std::fs::File::create(&ct).unwrap(), Compression::default());
            e.write_all(&ct_bytes).unwrap();
            e.finish().unwrap();
        }
        let sc = src.join("t1.json");
        std::fs::write(&sc, b"{\"A\":1}").unwrap();
        let mut a = item(t1.clone(), Session::Preop, "T1w", None);
        a.sidecar = Some(sc);
        let b = item(ct.clone(), Session::Postop, "CT", None);
        let plan = plan_import(&root, "01", &[a, b]).unwrap();

        let mut last = ImportProgress::default();
        let mut seen = Vec::new();
        let rep = run_import(&plan, false, &AtomicBool::new(false), &mut |p| {
            seen.push(p.current.clone());
            last = p;
        })
        .unwrap();
        assert_eq!(last.done, last.total);
        assert_eq!(last.total, 4);
        assert!(seen.contains(&"sub-01_ses-preop_T1w.nii.gz".to_owned()));

        let anat = root.join("rawdata/sub-01/ses-preop/anat");
        assert_eq!(
            gunzip(&anat.join("sub-01_ses-preop_T1w.nii.gz")),
            std::fs::read(&t1).unwrap()
        );
        assert_eq!(
            std::fs::read(anat.join("sub-01_ses-preop_T1w.json")).unwrap(),
            b"{\"A\":1}"
        );
        assert_eq!(
            gunzip(&root.join("rawdata/sub-01/ses-postop/anat/sub-01_ses-postop_CT.nii.gz")),
            ct_bytes
        );
        let desc: Value =
            serde_json::from_slice(&std::fs::read(root.join("dataset_description.json")).unwrap())
                .unwrap();
        assert_eq!(desc["Name"], "ds");
        assert_eq!(desc["BIDSVersion"], "1.9.0");
        assert_eq!(desc["DatasetType"], "raw");
        let rows = bids::load_rawimages(&rep.index_path).unwrap();
        assert_eq!(rows, plan.rows);
        assert_eq!(rep.written.len(), 3);
        let sub = bids::resolve_subject_with(&root, "sub-01", true).unwrap();
        assert_eq!(sub.anchor_modality, "T1w");
        assert_eq!(sub.postop_modality.as_deref(), Some("CT"));
        let left: Vec<_> = std::fs::read_dir(&anat)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".partial-"))
            .collect();
        assert!(left.is_empty());
    }

    #[test]
    fn postop_only_new_subject_keeps_nothing() {
        let d = tempfile::tempdir().unwrap();
        let ct = write_nii(d.path(), "ct.nii", &Spec::default());
        let root = d.path().join("ds");
        let e = plan_import(
            &root,
            "02",
            &[item(ct.clone(), Session::Postop, "CT", None)],
        )
        .unwrap_err();
        assert_eq!(e.code, bids::E_NO_ANCHOR);
        assert!(!root.join("rawdata/sub-02").exists());
        std::fs::create_dir_all(root.join("rawdata/sub-02/ses-preop/anat")).unwrap();
        let e = plan_import(&root, "02", &[item(ct, Session::Postop, "CT", None)]).unwrap_err();
        assert_eq!(e.code, bids::E_NO_ANCHOR);
        assert!(!root.join("rawdata/sub-02/rawimages.json").exists());
        assert!(!root.join("dataset_description.json").exists());
    }

    #[test]
    fn conflict_replace_and_rollback() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let root = d.path().join("ds");
        let first = write_nii(&src, "first.nii", &Spec::default());
        let t1 = |p: &Path| item(p.to_path_buf(), Session::Preop, "T1w", None);
        let plan = plan_import(&root, "01", &[t1(&first)]).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let dest = root.join(&plan.files[0].relative_path);
        let original = std::fs::read(&dest).unwrap();
        let index_before = std::fs::read(root.join("rawdata/sub-01/rawimages.json")).unwrap();

        let second = write_nii(
            &src,
            "second.nii",
            &Spec {
                dims: [4, 4, 4],
                ..Spec::default()
            },
        );
        let plan2 = plan_import(&root, "01", &[t1(&second)]).unwrap();
        assert_eq!(plan2.conflicts().len(), 1);
        assert!(plan2.subject_exists);
        assert!(plan2
            .warnings
            .iter()
            .any(|w| w.contains("will be replaced")));
        let e = run_import(&plan2, false, &AtomicBool::new(false), &mut no_progress).unwrap_err();
        assert_eq!(e.code, E_IMPORT_RULES);
        assert_eq!(std::fs::read(&dest).unwrap(), original);

        run_import(&plan2, true, &AtomicBool::new(false), &mut no_progress).unwrap();
        assert_eq!(gunzip(&dest), std::fs::read(&second).unwrap());
        let anat = dest.parent().unwrap();
        assert!(std::fs::read_dir(anat)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with(".partial-")));

        std::fs::write(&dest, &original).unwrap();
        let t2 = write_nii(&src, "t2.nii", &Spec::default());
        let plan3 = plan_import(
            &root,
            "01",
            &[t1(&second), item(t2.clone(), Session::Preop, "T2w", None)],
        )
        .unwrap();
        std::fs::remove_file(&t2).unwrap();
        let e = run_import(&plan3, true, &AtomicBool::new(false), &mut no_progress).unwrap_err();
        assert_eq!(e.code, E_IMPORT_FILE);
        assert_eq!(std::fs::read(&dest).unwrap(), original);
        assert_eq!(
            std::fs::read(root.join("rawdata/sub-01/rawimages.json")).unwrap(),
            index_before
        );
        assert!(!anat.join("sub-01_ses-preop_T2w.nii.gz").exists());
        assert!(std::fs::read_dir(anat)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with(".partial-")));
    }

    #[test]
    fn a_corrupt_index_fails_at_plan_time() {
        let (_d, src, root) = setup();
        let t1 = write_nii(&src, "t1.nii", &Spec::default());
        let a = item(t1, Session::Preop, "T1w", None);
        let plan = plan_import(&root, "01", std::slice::from_ref(&a)).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        std::fs::write(root.join("rawdata/sub-01/rawimages.json"), b"{not json").unwrap();
        assert_eq!(code(plan_import(&root, "01", &[a])), E_IMPORT_FILE);
    }

    #[test]
    fn a_ct_listed_in_the_lead_dbs_prefs_index_is_a_plan_error() {
        let (_d, src, root) = setup();
        let n = write_nii(&src, "a.nii", &Spec::default());
        let plan =
            plan_import(&root, "01", &[item(n.clone(), Session::Preop, "T1w", None)]).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let prefs = root.join("derivatives/leaddbs/sub-01/prefs/sub-01_desc-rawimages.json");
        std::fs::create_dir_all(prefs.parent().unwrap()).unwrap();
        let ct = [item(n, Session::Postop, "CT", None)];
        std::fs::write(
            &prefs,
            br#"{"postop": {"anat": {"CT": "sub-01_ses-postop_other_CT"}}}"#,
        )
        .unwrap();
        plan_import(&root, "01", &ct).unwrap();
        std::fs::write(
            &prefs,
            br#"{"postop": {"anat": {"CT": "sub-01_ses-postop_CT"}}}"#,
        )
        .unwrap();
        let e = plan_import(&root, "01", &ct).unwrap_err();
        assert_eq!(e.code, bids::E_DUPL_RAW);
        assert!(e.message.contains("E_DUPL_RAW"), "{}", e.message);
        assert!(!root.join("rawdata/sub-01/ses-postop").exists());
    }

    #[test]
    fn a_chosen_output_root_holds_the_prefs_index_and_the_invalidated_locks() {
        let (d, src, root) = setup();
        let out = d.path().join("elsewhere");
        let n = write_nii(&src, "a.nii", &Spec::default());
        let items = [
            item(n.clone(), Session::Preop, "T1w", None),
            item(n.clone(), Session::Postop, "CT", None),
        ];
        let plan = plan_import_in(&root, Some(&out), "01", &items[..1]).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let prefs = out.join("sub-01/prefs/sub-01_desc-rawimages.json");
        std::fs::create_dir_all(prefs.parent().unwrap()).unwrap();
        std::fs::write(
            &prefs,
            br#"{"postop": {"anat": {"CT": "sub-01_ses-postop_CT"}}}"#,
        )
        .unwrap();
        let e = plan_import_in(&root, Some(&out), "01", &items[1..]).unwrap_err();
        assert_eq!(e.code, bids::E_DUPL_RAW);
        plan_import(&root, "01", &items[1..]).unwrap();
        std::fs::remove_file(&prefs).unwrap();

        let plan = plan_import_in(&root, Some(&out), "01", &items).unwrap();
        run_import(&plan, true, &AtomicBool::new(false), &mut no_progress).unwrap();
        let locks = out.join("sub-01/locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(locks.join("coreg.json"), r#"{"approval": true}"#).unwrap();
        let post = plan_import_in(&root, Some(&out), "01", &items[1..]).unwrap();
        let rep = run_import(&post, true, &AtomicBool::new(false), &mut no_progress).unwrap();
        assert_eq!(rep.invalidation.changed, [Stage::Coregister]);
        assert!(!root.join("derivatives").exists());
    }

    #[test]
    fn replacing_raw_images_invalidates_the_stages_that_read_them() {
        let (_d, src, root) = setup();
        let n = write_nii(&src, "a.nii", &Spec::default());
        let items = [
            item(n.clone(), Session::Preop, "T1w", None),
            item(n.clone(), Session::Postop, "CT", None),
        ];
        let plan = plan_import(&root, "01", &items).unwrap();
        let first = run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        assert_eq!(first.invalidation, Invalidation::default());
        let locks = root.join("derivatives/leaddbs/sub-01/locks");
        std::fs::create_dir_all(&locks).unwrap();
        let approve = || {
            for stage in ["preprocess", "coreg", "normalize"] {
                std::fs::write(locks.join(format!("{stage}.json")), r#"{"approval": true}"#)
                    .unwrap();
            }
        };
        let approved = |stage: &str| {
            let v: Value = serde_json::from_slice(
                &std::fs::read(locks.join(format!("{stage}.json"))).unwrap(),
            )
            .unwrap();
            v["approval"] == json!(true)
        };

        approve();
        let post = plan_import(&root, "01", &items[1..]).unwrap();
        let rep = run_import(&post, true, &AtomicBool::new(false), &mut no_progress).unwrap();
        assert!(approved("preprocess"));
        assert!(!approved("coreg") && !approved("normalize"));
        assert_eq!(
            rep.invalidation.changed,
            [Stage::Coregister, Stage::Normalize]
        );
        assert_eq!(
            rep.invalidation
                .summary("import replaced raw images")
                .as_deref(),
            Some("dbsloc: import replaced raw images; approvals invalidated: coreg, normalize")
        );

        approve();
        let pre = plan_import(&root, "01", &items[..1]).unwrap();
        let rep = run_import(&pre, true, &AtomicBool::new(false), &mut no_progress).unwrap();
        assert!(!approved("preprocess") && !approved("coreg") && !approved("normalize"));
        let v: Value =
            serde_json::from_slice(&std::fs::read(locks.join("preprocess.json")).unwrap()).unwrap();
        assert_eq!(v["invalidated_by"], json!("import"));
        assert_eq!(
            rep.invalidation.changed,
            [Stage::Preprocess, Stage::Coregister, Stage::Normalize]
        );
    }

    #[test]
    fn cancel_before_run_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let t1 = write_nii(d.path(), "t1.nii", &Spec::default());
        let root = d.path().join("ds");
        let plan = plan_import(&root, "01", &[item(t1, Session::Preop, "T1w", None)]).unwrap();
        let e = run_import(&plan, false, &AtomicBool::new(true), &mut no_progress).unwrap_err();
        assert_eq!(e.code, E_IMPORT_CANCELLED);
        assert!(!root.exists());
    }

    #[test]
    fn find_dcm2niix_env_must_be_a_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let d = tempfile::tempdir().unwrap();
        let fake = d.path().join("dcm2niix-fake");
        std::fs::write(&fake, b"x").unwrap();
        let old = std::env::var_os(DCM2NIIX_ENV);
        std::env::set_var(DCM2NIIX_ENV, &fake);
        assert_eq!(find_dcm2niix(), Some(fake.clone()));
        std::env::set_var(DCM2NIIX_ENV, d.path().join("missing"));
        assert_ne!(find_dcm2niix(), Some(d.path().join("missing")));
        std::env::set_var(DCM2NIIX_ENV, d.path());
        assert_ne!(find_dcm2niix(), Some(d.path().to_path_buf()));
        match old {
            Some(v) => std::env::set_var(DCM2NIIX_ENV, v),
            None => std::env::remove_var(DCM2NIIX_ENV),
        }
    }

    #[test]
    fn bundled_dcm2niix_is_found_without_env_or_path() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let old = std::env::var_os(DCM2NIIX_ENV);
        std::env::remove_var(DCM2NIIX_ENV);
        let found = find_dcm2niix();
        if let Some(v) = old {
            std::env::set_var(DCM2NIIX_ENV, v);
        }
        let bundled = crate::bundled_bin::find_bundled_in("dcm2niix", "dcm2niix")
            .expect("resources/dcm2niix/<platform>/dcm2niix ships with the repository");
        assert!(bundled.is_file(), "{}", bundled.display());
        assert_eq!(found, Some(bundled));
    }

    #[test]
    fn parse_series_and_suggestions() {
        let d = tempfile::tempdir().unwrap();
        let o = d.path();
        write_nii(
            o,
            "5_t2_tse_cor_x.nii",
            &Spec {
                pixdim: [0.5, 0.5, 3.0],
                ..Spec::default()
            },
        );
        std::fs::write(
            o.join("5_t2_tse_cor_x.json"),
            br#"{"SeriesNumber": 5, "SeriesDescription": "t2_tse_cor", "ProtocolName": "t2", "Modality": "MR"}"#,
        )
        .unwrap();
        let mut perm = ident();
        perm[1] = [0.0, 0.0, 1.0, 0.0];
        perm[2] = [0.0, 1.0, 0.0, 0.0];
        write_nii(
            o,
            "6_flair.nii",
            &Spec {
                pixdim: [0.5, 0.5, 3.0],
                sform: 1,
                srow: perm,
                ..Spec::default()
            },
        );
        write_nii(o, "2_head_ct.nii", &Spec::default());
        std::fs::write(
            o.join("2_head_ct.json"),
            br#"{"SeriesNumber": "2", "Modality": "CT"}"#,
        )
        .unwrap();

        let s = parse_series(o, None, &default_lookup_table()).unwrap();
        assert_eq!(s.len(), 3);
        let by = |n: &str| {
            s.iter()
                .find(|c| c.nifti.file_name().unwrap().to_string_lossy() == n)
                .unwrap()
        };
        let ct = by("2_head_ct.nii");
        assert_eq!(ct.modality, "CT");
        assert_eq!(ct.suggested.session, Session::Postop);
        assert_eq!(ct.suggested.suffix, "CT");
        assert_eq!(ct.suggested.acq, None);

        let t2 = by("5_t2_tse_cor_x.nii");
        assert_eq!(t2.series_number, Some(5));
        assert_eq!(t2.description, "t2_tse_cor");
        assert_eq!(t2.protocol, "t2");
        assert_eq!(t2.dims, [2, 2, 2]);
        assert_eq!(t2.voxel_mm, [0.5, 0.5, 3.0]);
        assert_eq!(t2.suggested.session, Session::Preop);
        assert_eq!(t2.suggested.suffix, "T2w");
        assert_eq!(t2.suggested.acq, Some(Acq::Ax));

        let fl = by("6_flair.nii");
        assert_eq!(fl.series_number, None);
        assert_eq!(fl.suggested.suffix, "FLAIR");
        assert_eq!(fl.suggested.acq, Some(Acq::Cor));

        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            parse_series(empty.path(), None, &default_lookup_table())
                .unwrap_err()
                .code,
            E_DCM2NIIX
        );

        let tail = format_tail(
            &(0..30).map(|i| format!("o{i}\n")).collect::<String>(),
            "e1\ne2\n",
        );
        assert_eq!(tail.lines().count(), 20);
        assert!(tail.ends_with("e2"));
    }

    #[test]
    fn looks_like_dicom_by_magic_and_extension() {
        let d = tempfile::tempdir().unwrap();
        let with_magic = d.path().join("a/b");
        std::fs::create_dir_all(&with_magic).unwrap();
        let mut b = vec![0u8; 132];
        b[128..132].copy_from_slice(b"DICM");
        std::fs::write(with_magic.join("IM0001"), &b).unwrap();
        assert!(looks_like_dicom(&d.path().join("a")));

        let by_ext = d.path().join("c");
        std::fs::create_dir(&by_ext).unwrap();
        std::fs::write(by_ext.join("x.DCM"), b"junk").unwrap();
        assert!(looks_like_dicom(&by_ext));

        let nii = d.path().join("n");
        std::fs::create_dir(&nii).unwrap();
        write_nii(&nii, "a.nii", &Spec::default());
        assert!(!looks_like_dicom(&nii));
    }

    #[test]
    fn suggest_item_from_names() {
        let d = tempfile::tempdir().unwrap();
        let n = |name: &str| write_nii(d.path(), name, &Spec::default());
        let s = suggest_item(&n("sub-01_T2w.nii.gz"), None);
        assert_eq!(
            (s.session, s.suffix.as_str(), s.acq),
            (Session::Preop, "T2w", None)
        );
        let s = suggest_item(&n("post_ct.nii.gz"), None);
        assert_eq!((s.session, s.suffix.as_str()), (Session::Postop, "CT"));
        let s = suggest_item(&n("anything.nii"), None);
        assert_eq!(
            (s.session, s.suffix.as_str(), s.acq),
            (Session::Preop, "T1w", None)
        );
        let s = suggest_item(&n("sub-01_acq-sag_T1w.nii"), None);
        assert_eq!(s.acq, Some(Acq::Sag));
        let s = suggest_item(&d.path().join("missing.nii"), None);
        assert_eq!(
            (s.session, s.suffix.as_str(), s.acq),
            (Session::Preop, "T1w", None)
        );
    }

    fn big_nii(dir: &Path, name: &str, extra: usize) -> PathBuf {
        let mut b = nifti(&Spec::default());
        b.extend(std::iter::repeat_n(7u8, extra));
        let p = dir.join(name);
        std::fs::write(&p, b).unwrap();
        p
    }

    fn gz_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut e = GzEncoder::new(std::fs::File::create(&p).unwrap(), Compression::default());
        e.write_all(bytes).unwrap();
        e.finish().unwrap();
        p
    }

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let root = d.path().join("ds");
        (d, src, root)
    }

    fn no_partials(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .all(|e| !e.file_name().to_string_lossy().starts_with(".partial-"))
            })
            .unwrap_or(true)
    }

    #[test]
    fn sidecar_only_conflict_blocks_without_replace() {
        let (_d, src, root) = setup();
        let t1 = write_nii(&src, "t1.nii", &Spec::default());
        let sc = src.join("t1.json");
        std::fs::write(&sc, b"{}").unwrap();
        let mut a = item(t1, Session::Preop, "T1w", None);
        a.sidecar = Some(sc);
        let plan = plan_import(&root, "01", &[a.clone()]).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        std::fs::remove_file(root.join(&plan.files[0].relative_path)).unwrap();

        let plan2 = plan_import(&root, "01", &[a]).unwrap();
        assert!(!plan2.files[0].exists);
        assert!(plan2.files[0].sidecar_exists);
        assert_eq!(plan2.conflicts().len(), 1);
        let e = run_import(&plan2, false, &AtomicBool::new(false), &mut no_progress).unwrap_err();
        assert_eq!(e.code, E_IMPORT_RULES);
        run_import(&plan2, true, &AtomicBool::new(false), &mut no_progress).unwrap();
    }

    #[test]
    fn same_path_index_row_is_replaced_not_duplicated() {
        let (_d, src, root) = setup();
        let t1 = write_nii(&src, "t1.nii", &Spec::default());
        let a = item(t1, Session::Preop, "T1w", None);
        let plan = plan_import(&root, "01", std::slice::from_ref(&a)).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let idx = root.join("rawdata/sub-01/rawimages.json");
        let rel = &plan.files[0].relative_path;
        let rows = json!([{
            "path": format!("./{}", rel.replace('/', "\\")),
            "session": "preop",
            "datatype": "anat",
            "entities": {"suffix": "T1w", "run": 1}
        }]);
        std::fs::write(&idx, serde_json::to_vec(&rows).unwrap()).unwrap();
        std::fs::remove_file(root.join(rel)).unwrap();

        let plan2 = plan_import(&root, "01", &[a]).unwrap();
        assert!(plan2.files[0].index_row_exists);
        assert_eq!(plan2.conflicts().len(), 1);
        assert!(plan2
            .warnings
            .iter()
            .any(|w| w.contains("will be replaced")));
        assert_eq!(
            run_import(&plan2, false, &AtomicBool::new(false), &mut no_progress)
                .unwrap_err()
                .code,
            E_IMPORT_RULES
        );
        run_import(&plan2, true, &AtomicBool::new(false), &mut no_progress).unwrap();
        let after = bids::load_rawimages(&idx).unwrap();
        assert_eq!(after, plan2.rows);
    }

    #[test]
    fn ct_with_acq_is_rejected() {
        let (_d, src, root) = setup();
        let n = write_nii(&src, "a.nii", &Spec::default());
        let bad = [
            item(n.clone(), Session::Preop, "T1w", None),
            item(n, Session::Postop, "CT", Some(Acq::Ax)),
        ];
        assert_eq!(code(plan_import(&root, "01", &bad)), E_IMPORT_RULES);
    }

    #[test]
    fn a_coronal_or_sagittal_post_op_mr_alone_is_accepted() {
        for acq in [Acq::Cor, Acq::Sag] {
            let (_d, src, root) = setup();
            let n = write_nii(&src, "a.nii", &Spec::default());
            let set = [
                item(n.clone(), Session::Preop, "T1w", None),
                item(n.clone(), Session::Postop, "T2w", Some(acq)),
            ];
            let plan = plan_import(&root, "01", &set).unwrap();
            let key = format!("{}_MRI", acq.as_str());
            assert_eq!(plan.rows[1].entities.get("modality"), Some(&json!(key)));
            run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
            let sub = bids::resolve_subject_with(&root, "01", true).unwrap();
            assert_eq!(sub.postop_modality.as_deref(), Some("MRI"), "{key}");
            let keys: Vec<&str> = sub.postop_mr_images().iter().map(|(k, _)| *k).collect();
            assert_eq!(keys, [key.as_str()]);
        }
    }

    #[test]
    fn a_post_op_mr_onto_an_existing_subject_needs_no_axial_image() {
        let (_d, src, root) = setup();
        let n = write_nii(&src, "a.nii", &Spec::default());
        let first = [item(n.clone(), Session::Preop, "T1w", None)];
        let plan = plan_import(&root, "01", &first).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();

        let sag = [item(n.clone(), Session::Postop, "FLAIR", Some(Acq::Sag))];
        let plan2 = plan_import(&root, "01", &sag).unwrap();
        run_import(&plan2, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let cor = [item(n.clone(), Session::Postop, "T2w", Some(Acq::Cor))];
        assert!(plan_import(&root, "01", &cor).is_ok());

        let (_d2, _src2, root2) = setup();
        assert_eq!(code(plan_import(&root2, "01", &cor)), bids::E_NO_ANCHOR);
    }

    #[test]
    fn truncated_nii_is_rejected_and_leaves_nothing() {
        let (_d, src, root) = setup();
        let p = write_nii(&src, "t.nii", &Spec::default());
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(355).unwrap();
        drop(f);
        let a = item(p.clone(), Session::Preop, "T1w", None);
        assert_eq!(code(plan_import(&root, "01", &[a])), E_IMPORT_FILE);

        let q = write_nii(&src, "u.nii", &Spec::default());
        let b = item(q.clone(), Session::Preop, "T1w", None);
        let plan = plan_import(&root, "01", &[b]).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&q).unwrap();
        f.set_len(355).unwrap();
        drop(f);
        let e = run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap_err();
        assert_eq!(e.code, E_IMPORT_FILE);
        assert!(!root.exists());
    }

    #[test]
    fn corrupt_gz_is_rejected_and_leaves_nothing() {
        let (_d, src, root) = setup();
        let mut bytes = nifti(&Spec::default());
        bytes.extend(std::iter::repeat_n(0u8, 3 << 20));
        let p = gz_file(&src, "c.nii.gz", &bytes);
        let mut raw = std::fs::read(&p).unwrap();
        raw.truncate(raw.len() - 8);
        std::fs::write(&p, &raw).unwrap();
        let a = item(p, Session::Preop, "T1w", None);
        match plan_import(&root, "01", &[a]) {
            Err(e) => assert_eq!(e.code, E_IMPORT_FILE),
            Ok(plan) => {
                let e = run_import(&plan, false, &AtomicBool::new(false), &mut no_progress)
                    .unwrap_err();
                assert_eq!(e.code, E_IMPORT_FILE);
            }
        }
        assert!(!root.exists());
    }

    #[test]
    fn cancel_during_copy_rolls_back() {
        let (_d, src, root) = setup();
        let big = big_nii(&src, "big.nii", 3 << 20);
        let plan = plan_import(&root, "01", &[item(big, Session::Preop, "T1w", None)]).unwrap();
        let cancel = AtomicBool::new(false);
        let e = run_import(&plan, false, &cancel, &mut |p| {
            if p.current.ends_with(".nii.gz") {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert_eq!(e.code, E_IMPORT_CANCELLED);
        assert!(!root.exists());
    }

    #[test]
    fn big_copy_roundtrips_in_chunks() {
        let (_d, src, root) = setup();
        let big = big_nii(&src, "big.nii", 3 << 20);
        let plan = plan_import(
            &root,
            "01",
            &[item(big.clone(), Session::Preop, "T1w", None)],
        )
        .unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        assert_eq!(
            gunzip(&root.join(&plan.files[0].relative_path)),
            std::fs::read(&big).unwrap()
        );
        assert!(no_partials(&root.join("rawdata/sub-01/ses-preop/anat")));
    }

    #[test]
    fn gz_source_is_copied_unchanged_and_plain_source_becomes_one_gzip_member() {
        let (_d, src, root) = setup();
        let mut bytes = nifti(&Spec::default());
        bytes.extend((0..(3 << 20)).map(|i: usize| (i * 31 % 251) as u8));
        let gz = gz_file(&src, "a.nii.gz", &bytes);
        let plain = src.join("b.nii");
        std::fs::write(&plain, &bytes).unwrap();
        let items = [
            item(gz.clone(), Session::Preop, "T1w", None),
            item(plain, Session::Preop, "T2w", None),
        ];
        let plan = plan_import(&root, "01", &items).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let copied = root.join(&plan.files[0].relative_path);
        assert_eq!(std::fs::read(&copied).unwrap(), std::fs::read(&gz).unwrap());
        let packed = std::fs::read(root.join(&plan.files[1].relative_path)).unwrap();
        let mut d = GzDecoder::new(packed.as_slice());
        let mut out = Vec::new();
        d.read_to_end(&mut out).unwrap();
        assert!(d.into_inner().is_empty());
        assert_eq!(out, bytes);
    }

    #[test]
    fn nifti2_and_two_file_magic_are_rejected() {
        let (_d, src, _root) = setup();
        let p = write_nii(&src, "n2.nii", &Spec::default());
        let mut b = std::fs::read(&p).unwrap();
        b[0..4].copy_from_slice(&540i32.to_le_bytes());
        std::fs::write(&p, &b).unwrap();
        let e = nifti_summary(&p).unwrap_err();
        assert_eq!(e.code, E_IMPORT_FILE);
        assert!(e.message.contains("NIfTI-2 is not supported"));

        let q = write_nii(&src, "ni1.nii", &Spec::default());
        let mut b = std::fs::read(&q).unwrap();
        b[344..347].copy_from_slice(b"ni1");
        std::fs::write(&q, &b).unwrap();
        assert_eq!(nifti_summary(&q).unwrap_err().code, E_IMPORT_FILE);
    }

    #[test]
    fn long_destination_path_warns() {
        let (_d, src, root) = setup();
        let n = write_nii(&src, "a.nii", &Spec::default());
        let deep = root.join("x".repeat(120)).join("y".repeat(120));
        let plan = plan_import(&deep, "01", &[item(n, Session::Preop, "T1w", None)]).unwrap();
        assert!(plan.warnings.iter().any(|w| w.contains("240 characters")));
    }

    #[test]
    fn guess_acq_odd_axis_and_sform_fallback() {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(guess_acq([3.0, 3.0, 0.5], id), Some(Acq::Ax));
        assert_eq!(guess_acq([0.5, 0.5, 3.0], id), Some(Acq::Ax));
        assert_eq!(guess_acq([3.0, 0.5, 3.0], id), Some(Acq::Cor));
        assert_eq!(guess_acq([0.5, 3.0, 0.5], id), Some(Acq::Cor));
        assert_eq!(guess_acq([3.0, 0.5, 0.5], id), Some(Acq::Sag));
        assert_eq!(guess_acq([1.0, 1.05, 0.95], id), None);

        let h = |sform: i16, srow| Header {
            dim: [3, 2, 2, 2, 1, 1, 1, 1],
            datatype: 2,
            bitpix: 8,
            vox_offset: 352.0,
            pixdim: [1.0; 8],
            qform_code: 1,
            sform_code: sform,
            quat: [0.0; 3],
            srow,
        };
        let zero = h(1, [[0.0; 4]; 3]);
        assert_eq!(directions(&zero), id);
        let real = h(1, ident());
        assert_eq!(directions(&real), id);
    }

    #[test]
    fn series_sort_by_number_then_name() {
        let d = tempfile::tempdir().unwrap();
        let o = d.path();
        for (name, num) in [
            ("a_x", Some(10)),
            ("b_x", Some(2)),
            ("c_x", None),
            ("0_x", None),
        ] {
            write_nii(o, &format!("{name}.nii"), &Spec::default());
            if let Some(n) = num {
                std::fs::write(
                    o.join(format!("{name}.json")),
                    format!("{{\"SeriesNumber\": {n}}}"),
                )
                .unwrap();
            }
        }
        let names: Vec<String> = parse_series(o, None, &default_lookup_table())
            .unwrap()
            .iter()
            .map(|s| s.nifti.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["b_x.nii", "a_x.nii", "0_x.nii", "c_x.nii"]);
    }

    #[test]
    fn suffix_classification_uses_tokens() {
        let d = tempfile::tempdir().unwrap();
        let suffix = |name: &str| {
            let p = write_nii(d.path(), name, &Spec::default());
            suggest_item(&p, None).suffix
        };
        assert_eq!(suffix("ST2_x.nii"), "T1w");
        assert_eq!(suffix("t2_tse.nii"), "T2w");
        assert_eq!(suffix("swiss.nii"), "T1w");
        assert_eq!(suffix("swi_x.nii"), "T2starw");
        assert_eq!(suffix("t2star_x.nii"), "T2starw");
        assert_eq!(suffix("pd_x.nii"), "PDw");
    }

    #[test]
    fn convert_dicom_failure_empties_output_dir() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let d = tempfile::tempdir().unwrap();
        let dicom = d.path().join("dicom");
        std::fs::create_dir(&dicom).unwrap();
        let out = d.path().join("out");
        let fake = d
            .path()
            .join(if cfg!(windows) { "fake.exe" } else { "fake" });
        std::fs::write(&fake, b"x").unwrap();
        let old = std::env::var_os(DCM2NIIX_ENV);
        std::env::set_var(DCM2NIIX_ENV, &fake);
        let cancel = AtomicBool::new(true);
        let r = convert_dicom(&dicom, &out, &cancel);
        match old {
            Some(v) => std::env::set_var(DCM2NIIX_ENV, v),
            None => std::env::remove_var(DCM2NIIX_ENV),
        }
        assert_eq!(r.unwrap_err().code, E_IMPORT_CANCELLED);
        assert!(out.is_dir());
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
    }

    fn suggested(
        d: &Path,
        name: &str,
        sidecar: Option<&str>,
    ) -> Option<(Session, String, Option<Acq>)> {
        let p = write_nii(d, name, &Spec::default());
        let sc = sidecar.map(|text| {
            let s = d.join(format!("{}.json", nifti_stem(&p)));
            std::fs::write(&s, text).unwrap();
            s
        });
        suggest(&p, sc.as_deref(), &default_lookup_table()).map(|i| (i.session, i.suffix, i.acq))
    }

    fn suffix_of(d: &Path, name: &str) -> Option<String> {
        suggested(d, name, None).map(|(_, s, _)| s)
    }

    #[test]
    fn lookup_rules_last_match_wins_and_short_keys_are_words() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        assert_eq!(suffix_of(d, "t1_mprage_sag.nii").as_deref(), Some("T1w"));
        assert_eq!(suffix_of(d, "MPRAGE_iso.nii").as_deref(), Some("T1w"));
        assert_eq!(suffix_of(d, "sub-01_T1w.nii").as_deref(), Some("T1w"));
        assert_eq!(suffix_of(d, "t2_tse_ax.nii").as_deref(), Some("T2w"));
        assert_eq!(suffix_of(d, "t2_flair.nii").as_deref(), Some("FLAIR"));
        assert_eq!(suffix_of(d, "t2star_ax.nii").as_deref(), Some("T2starw"));
        assert_eq!(suffix_of(d, "t1_fgatir.nii").as_deref(), Some("FGATIR"));
        assert_eq!(suffix_of(d, "pd_t2_tse.nii").as_deref(), Some("PDw"));
        assert_eq!(suffix_of(d, "swi_mip.nii").as_deref(), Some("T2starw"));
        assert_eq!(suffix_of(d, "t1_head_ct.nii").as_deref(), Some("CT"));
        assert_eq!(suffix_of(d, "direct_survey.nii"), None);
        assert_eq!(suffix_of(d, "swiss.nii"), None);
        assert_eq!(suffix_of(d, "ST2_x.nii"), None);
        assert_eq!(suffix_of(d, "localizer.nii"), None);
        assert_eq!(
            suggested(d, "head_ct.nii", None),
            Some((Session::Postop, "CT".to_owned(), None))
        );
    }

    #[test]
    fn the_dicom_modality_tag_beats_the_name() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        assert_eq!(
            suggested(d, "a_t1.nii", Some(r#"{"Modality": "CT"}"#)),
            Some((Session::Postop, "CT".to_owned(), None))
        );
        assert_eq!(
            suggested(
                d,
                "b_ct.nii",
                Some(r#"{"Modality": "MR", "SeriesDescription": "head"}"#)
            ),
            None
        );
        assert_eq!(
            suggested(
                d,
                "c_ct.nii",
                Some(r#"{"Modality": "MR", "ProtocolName": "T2_TSE"}"#)
            )
            .map(|(_, s, _)| s)
            .as_deref(),
            Some("T2w")
        );
        let (session, suffix, acq) = suggested(
            d,
            "x.nii",
            Some(r#"{"Modality": "MR", "SeriesDescription": "t1_mprage"}"#),
        )
        .unwrap();
        assert_eq!(
            (session, suffix.as_str(), acq),
            (Session::Preop, "T1w", None)
        );
    }

    #[test]
    fn a_lookup_table_loads_from_json_and_rejects_bad_rules() {
        let table = default_lookup_table();
        let back = LookupTable::from_json(&table.to_json().to_string()).unwrap();
        assert_eq!(back, table);
        let custom = LookupTable::from_json(
            r#"[{"session": "preop", "suffix": "t1w", "keywords": ["anat"]},
                {"session": "postop", "suffix": "T2w", "keywords": ["postop_tse", " "]}]"#,
        )
        .unwrap();
        assert_eq!(custom.rules[0].suffix, "T1w");
        assert_eq!(custom.rules[1].keywords, ["postop_tse"]);
        let d = tempfile::tempdir().unwrap();
        let p = write_nii(
            d.path(),
            "postop_tse.nii",
            &Spec {
                pixdim: [0.5, 0.5, 3.0],
                ..Spec::default()
            },
        );
        let item = suggest(&p, None, &custom).unwrap();
        assert_eq!(
            (item.session, item.suffix.as_str(), item.acq),
            (Session::Postop, "T2w", Some(Acq::Ax))
        );
        let iso = write_nii(d.path(), "postop_tse_iso.nii", &Spec::default());
        assert_eq!(suggest(&iso, None, &custom).unwrap().acq, Some(Acq::Ax));
        assert!(
            LookupTable::from_json(r#"[{"session": "x", "suffix": "T1w", "keywords": []}]"#)
                .is_err()
        );
        assert!(LookupTable::from_json(
            r#"[{"session": "preop", "suffix": "CT", "keywords": []}]"#
        )
        .is_err());
        assert!(LookupTable::from_json(r#"{"rules": []}"#).is_err());
        let file = d.path().join("table.json");
        std::fs::write(&file, b"[oops").unwrap();
        assert_eq!(LookupTable::load(&file).unwrap_err().code, E_IMPORT_FILE);
    }

    #[test]
    fn series_info_reads_the_new_sidecar_fields_and_dates() {
        let d = tempfile::tempdir().unwrap();
        let o = d.path();
        let p = write_nii(
            o,
            "pat_t1_mprage_tfl3d1_20190502101112_7_t1_mprage.nii",
            &Spec::default(),
        );
        std::fs::write(
            o.join("pat_t1_mprage_tfl3d1_20190502101112_7_t1_mprage.json"),
            br#"{"SeriesNumber": "7", "SeriesDescription": "t1_mprage", "ProtocolName": "t1",
                "Modality": "MR", "Manufacturer": "Siemens", "MagneticFieldStrength": 3,
                "EchoTime": 0.00226, "RepetitionTime": 2.3, "InversionTime": 0.9,
                "SliceThickness": 1, "ImageType": ["ORIGINAL", "PRIMARY", "M", "ND"],
                "AcquisitionDateTime": "2021-03-04T08:09:10.250000", "StudyDate": "20210303"}"#,
        )
        .unwrap();
        let info = series_info(
            &p,
            Some(&o.join("pat_t1_mprage_tfl3d1_20190502101112_7_t1_mprage.json")),
        );
        assert_eq!(info.series_number, Some(7));
        assert_eq!(info.manufacturer.as_deref(), Some("Siemens"));
        assert_eq!(info.magnetic_field_strength, Some(3.0));
        assert_eq!(info.echo_time, Some(0.00226));
        assert_eq!(info.repetition_time, Some(2.3));
        assert_eq!(info.inversion_time, Some(0.9));
        assert_eq!(info.slice_thickness, Some(1.0));
        assert_eq!(info.image_type, ["ORIGINAL", "PRIMARY", "M", "ND"]);
        assert_eq!(
            info.acquisition_datetime.as_deref(),
            Some("2021-03-04T08:09:10")
        );
        assert_eq!(info.study_date.as_deref(), Some("2021-03-03"));
        assert_eq!(info.dims, [2, 2, 2]);

        let info = series_info(&p, None);
        assert_eq!(
            info.acquisition_datetime.as_deref(),
            Some("2019-05-02T10:11:12")
        );
        assert_eq!(info.study_date.as_deref(), Some("2019-05-02"));
        assert_eq!(info.manufacturer, None);

        let q = write_nii(o, "plain_name.nii", &Spec::default());
        let sc = o.join("plain_name.json");
        std::fs::write(
            &sc,
            br#"{"AcquisitionDate": "2020-01-02", "AcquisitionTime": "131415.5", "ImageType": "DERIVED\\SECONDARY"}"#,
        )
        .unwrap();
        let info = series_info(&q, Some(&sc));
        assert_eq!(
            info.acquisition_datetime.as_deref(),
            Some("2020-01-02T13:14:15")
        );
        assert_eq!(info.study_date.as_deref(), Some("2020-01-02"));
        assert_eq!(info.image_type, ["DERIVED", "SECONDARY"]);
        let none = series_info(&write_nii(o, "nodate_123.nii", &Spec::default()), None);
        assert_eq!(none.study_date, None);
        assert_eq!(none.acquisition_datetime, None);
        assert_eq!(none.series_number, None);
    }

    fn series_dir(o: &Path, folder: &str, rows: &[(&str, &str)]) -> Vec<ConvertedSeries> {
        for (name, sidecar) in rows {
            write_nii(
                o,
                &format!("{name}.nii"),
                &Spec {
                    pixdim: [0.5, 0.5, 2.0],
                    ..Spec::default()
                },
            );
            std::fs::write(o.join(format!("{name}.json")), sidecar).unwrap();
        }
        parse_series(o, Some(folder), &default_lookup_table()).unwrap()
    }

    #[test]
    fn the_latest_of_two_studies_is_suggested_as_post_op() {
        let d = tempfile::tempdir().unwrap();
        let mut s = series_dir(
            d.path(),
            "PatientCT_01",
            &[
                (
                    "PatientCT_01_t1_a",
                    r#"{"SeriesNumber": 1, "StudyDate": "20200101", "SeriesDescription": "t1_mprage", "Modality": "MR"}"#,
                ),
                (
                    "PatientCT_01_t2_b",
                    r#"{"SeriesNumber": 2, "StudyDate": "20200101", "SeriesDescription": "t2_tse", "Modality": "MR"}"#,
                ),
                (
                    "PatientCT_01_t1_c",
                    r#"{"SeriesNumber": 3, "StudyDate": "20200301", "SeriesDescription": "t1_post", "Modality": "MR"}"#,
                ),
                (
                    "PatientCT_01_ct_d",
                    r#"{"SeriesNumber": 4, "StudyDate": "20200301", "SeriesDescription": "head", "Modality": "CT"}"#,
                ),
                (
                    "PatientCT_01_sv_e",
                    r#"{"SeriesNumber": 5, "StudyDate": "20200301", "SeriesDescription": "survey", "Modality": "MR"}"#,
                ),
                (
                    "PatientCT_01_un_f",
                    r#"{"SeriesNumber": 6, "SeriesDescription": "t2_other", "Modality": "MR"}"#,
                ),
            ],
        );
        assert!(s
            .iter()
            .filter(|c| c.series_number != Some(5))
            .all(|c| c.assigned));
        assert!(s[4].suggestion().is_none());
        assert_eq!(s[4].suggested.suffix, "T1w");
        let studies = group_studies(&s);
        assert_eq!(
            studies,
            [
                Study {
                    date: Some("2020-01-01".to_owned()),
                    series: vec![0, 1]
                },
                Study {
                    date: Some("2020-03-01".to_owned()),
                    series: vec![2, 3, 4]
                },
                Study {
                    date: None,
                    series: vec![5]
                },
            ]
        );
        assert_eq!(apply_latest_study_rule(&mut s), [2]);
        let row = |i: usize| {
            let it = &s[i].suggested;
            (it.session, it.suffix.clone(), it.acq)
        };
        assert_eq!(row(0), (Session::Preop, "T1w".to_owned(), Some(Acq::Ax)));
        assert_eq!(row(2), (Session::Postop, "T1w".to_owned(), Some(Acq::Ax)));
        assert_eq!(row(3), (Session::Postop, "CT".to_owned(), None));
        assert!(s[4].suggestion().is_none());
        assert_eq!(row(5).0, Session::Preop);

        let d2 = tempfile::tempdir().unwrap();
        let mut one = series_dir(
            d2.path(),
            "x",
            &[
                (
                    "a",
                    r#"{"StudyDate": "20200101", "SeriesDescription": "t1"}"#,
                ),
                (
                    "b",
                    r#"{"StudyDate": "20200101", "SeriesDescription": "t2"}"#,
                ),
            ],
        );
        assert!(apply_latest_study_rule(&mut one).is_empty());
        assert!(one.iter().all(|c| c.suggested.session == Session::Preop));
    }

    #[test]
    fn the_converter_folder_name_does_not_take_part_in_matching() {
        let d = tempfile::tempdir().unwrap();
        let s = series_dir(
            d.path(),
            "CT Study",
            &[("CT_Study_survey_x", r#"{"SeriesDescription": "survey"}"#)],
        );
        assert!(!s[0].assigned);
        let p = d.path().join("CT_Study_survey_x.nii");
        assert_eq!(
            suggest(&p, None, &default_lookup_table()).unwrap().suffix,
            "CT"
        );
    }

    #[test]
    fn subject_ids_from_folder_names() {
        assert_eq!(subject_id_from_folder("Pat01"), ("Pat01".to_owned(), None));
        let (id, w) = subject_id_from_folder("Pat_01-a");
        assert_eq!(id, "Pat01a");
        assert!(w.unwrap().contains("removed"));
        assert_eq!(subject_id_from_folder("sub-07").0, "07");
        assert_eq!(subject_id_from_folder("ü x").0, "x");
    }

    fn dicom_folder(dir: &Path, name: &str) -> PathBuf {
        let f = dir.join(name);
        std::fs::create_dir_all(f.join("series1")).unwrap();
        let mut b = vec![0u8; 132];
        b[128..132].copy_from_slice(b"DICM");
        std::fs::write(f.join("series1/IM0001"), &b).unwrap();
        std::fs::write(f.join("DICOMDIR"), b"dir").unwrap();
        f
    }

    #[test]
    fn the_source_dicom_folder_is_kept_and_rolled_back() {
        let (d, src, root) = setup();
        let n = write_nii(&src, "t1.nii", &Spec::default());
        let dicom = dicom_folder(d.path(), "Pat 01");
        let items = [item(n.clone(), Session::Preop, "T1w", None)];
        let plan =
            plan_import_with(&root, None, "01", &items, std::slice::from_ref(&dicom)).unwrap();
        assert_eq!(
            plan.sourcedata[0].relative_path,
            "sourcedata/sub-01/DICOM/Pat01"
        );
        let rep = run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let kept = root.join("sourcedata/sub-01/DICOM/Pat01");
        assert_eq!(
            std::fs::read(kept.join("series1/IM0001")).unwrap().len(),
            132
        );
        assert_eq!(std::fs::read(kept.join("DICOMDIR")).unwrap(), b"dir");
        assert!(rep.written.contains(&kept));
        assert!(no_partials(&root.join("sourcedata/sub-01/DICOM")));

        let t2 = write_nii(&src, "t2.nii", &Spec::default());
        let plan2 = plan_import_with(
            &root,
            None,
            "01",
            &[item(t2.clone(), Session::Preop, "T2w", None)],
            std::slice::from_ref(&dicom),
        )
        .unwrap();
        assert_eq!(
            plan2.sourcedata[0].relative_path,
            "sourcedata/sub-01/DICOM/Pat01-2"
        );
        assert!(plan2.warnings.iter().any(|w| w.contains("Pat01-2")));
        std::fs::remove_file(&t2).unwrap();
        let e = run_import(&plan2, false, &AtomicBool::new(false), &mut no_progress).unwrap_err();
        assert_eq!(e.code, E_IMPORT_FILE);
        assert!(!root.join("sourcedata/sub-01/DICOM/Pat01-2").exists());
        assert!(kept.join("series1/IM0001").is_file());
        assert!(no_partials(&root.join("sourcedata/sub-01/DICOM")));

        let again =
            plan_import_with(&root, None, "01", &items, std::slice::from_ref(&kept)).unwrap();
        assert!(again.sourcedata.is_empty());
        assert!(again
            .warnings
            .iter()
            .any(|w| w.contains("not copied again")));

        let (_d2, _src2, root2) = setup();
        let cancel = AtomicBool::new(false);
        let plan3 =
            plan_import_with(&root2, None, "02", &items, std::slice::from_ref(&dicom)).unwrap();
        let e = run_import(&plan3, false, &cancel, &mut |p| {
            if p.current.starts_with("sourcedata/") {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert_eq!(e.code, E_IMPORT_CANCELLED);
        assert!(!root2.exists());
    }

    #[test]
    fn dataset_description_records_the_generating_software() {
        let (_d, src, root) = setup();
        let n = write_nii(&src, "t1.nii", &Spec::default());
        let items = [item(n, Session::Preop, "T1w", None)];
        let desc = |root: &Path| -> Value {
            serde_json::from_slice(&std::fs::read(root.join("dataset_description.json")).unwrap())
                .unwrap()
        };
        let plan = plan_import(&root, "01", &items).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let v = desc(&root);
        assert_eq!(v["GeneratedBy"][0]["Name"], "Wyss DBS Localizer");
        assert_eq!(
            v["GeneratedBy"][0]["Version"],
            json!(crate::version::version_string())
        );
        assert_eq!(v["BIDSVersion"], "1.9.0");

        let (_d2, _s2, root2) = setup();
        std::fs::create_dir_all(&root2).unwrap();
        std::fs::write(
            root2.join("dataset_description.json"),
            br#"{"Name": "Mine", "BIDSVersion": "1.6.0", "LeadDBSVersion": "3"}"#,
        )
        .unwrap();
        let plan = plan_import(&root2, "01", &items).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        let v = desc(&root2);
        assert_eq!(v["Name"], "Mine");
        assert_eq!(v["LeadDBSVersion"], "3");
        assert_eq!(v["GeneratedBy"][0]["Name"], "Wyss DBS Localizer");
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["Name", "BIDSVersion", "LeadDBSVersion", "GeneratedBy"]
        );

        std::fs::write(
            root2.join("dataset_description.json"),
            br#"{"Name": "Mine", "GeneratedBy": [{"Name": "Other"}]}"#,
        )
        .unwrap();
        let t2 = write_nii(&src, "t2.nii", &Spec::default());
        let plan = plan_import(&root2, "01", &[item(t2, Session::Preop, "T2w", None)]).unwrap();
        run_import(&plan, false, &AtomicBool::new(false), &mut no_progress).unwrap();
        assert_eq!(desc(&root2)["GeneratedBy"][0]["Name"], "Other");
    }

    #[test]
    fn a_failed_subject_of_a_batch_leaves_the_others_imported() {
        let (d, src, root) = setup();
        let n = write_nii(&src, "t1.nii", &Spec::default());
        let ct = write_nii(&src, "ct.nii", &Spec::default());
        let dicom = dicom_folder(d.path(), "PatA");
        let mut first = SubjectImport::new(
            "A1",
            vec![
                item(n.clone(), Session::Preop, "T1w", None),
                item(ct.clone(), Session::Postop, "CT", None),
            ],
        );
        first.sourcedata_dirs = vec![dicom.clone()];
        let mut second = SubjectImport::new(
            "B2",
            vec![item(src.join("missing.nii"), Session::Preop, "T1w", None)],
        );
        second.sourcedata_dirs = vec![dicom.clone()];
        let mut third =
            SubjectImport::new("C3", vec![item(n.clone(), Session::Preop, "T1w", None)]);
        third.keep_sourcedata = false;
        third.sourcedata_dirs = vec![dicom.clone()];
        let dup = SubjectImport::new("sub-A1", vec![item(n.clone(), Session::Preop, "T1w", None)]);
        let batch = ImportBatch {
            root: root.clone(),
            output_dir: None,
            subjects: vec![first, second, third, dup],
        };

        let plans = plan_import_batch(&batch);
        assert_eq!(plans.len(), 4);
        assert!(plans[0].is_ok() && plans[2].is_ok());
        assert_eq!(plans[1].as_ref().unwrap_err().code, E_IMPORT_FILE);
        assert_eq!(plans[3].as_ref().unwrap_err().code, E_IMPORT_RULES);
        assert!(!root.exists());

        let mut seen = Vec::new();
        let results = run_import_batch(&batch, &AtomicBool::new(false), &mut |p| {
            seen.push((p.subject, p.subjects, p.subject_id.clone()));
        });
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].as_ref().unwrap().subject_folder, "sub-A1");
        assert_eq!(results[1].as_ref().unwrap_err().code, E_IMPORT_FILE);
        assert!(results[2].is_ok());
        assert_eq!(results[3].as_ref().unwrap_err().code, E_IMPORT_RULES);
        assert!(seen.contains(&(0, 4, "A1".to_owned())));
        assert!(seen.contains(&(2, 4, "C3".to_owned())));
        assert!(bids::resolve_subject_with(&root, "A1", true).is_ok());
        assert!(root
            .join("sourcedata/sub-A1/DICOM/PatA/series1/IM0001")
            .is_file());
        assert!(!root.join("rawdata/sub-B2").exists());
        assert!(!root.join("sourcedata/sub-B2").exists());
        assert!(bids::resolve_subject_with(&root, "C3", true).is_ok());
        assert!(!root.join("sourcedata/sub-C3").exists());

        let (_d2, _s2, root2) = setup();
        let batch2 = ImportBatch {
            root: root2.clone(),
            output_dir: None,
            subjects: batch.subjects[..1].to_vec(),
        };
        let results = run_import_batch(&batch2, &AtomicBool::new(true), &mut |_| {});
        assert_eq!(results[0].as_ref().unwrap_err().code, E_IMPORT_CANCELLED);
        assert!(!root2.exists());
    }
}
