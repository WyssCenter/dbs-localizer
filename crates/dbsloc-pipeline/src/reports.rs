// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Import of vendor session reports into a subject (`specs/16` §5): record, index, source copy, stimulation labels.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dbsloc_reports::anonymize::{anonymize, free_text_fields, shift_for};
use dbsloc_reports::dates::shift_days;
use dbsloc_reports::model::{
    canonical_json, Group, Hemisphere, ImpedanceKind, Polarity as RecPolarity, Program,
    ProgramContact, Severity, Snapshot, StimMode, Vendor,
};
use dbsloc_reports::pdf::PdfDocument;
use dbsloc_reports::vendors::ParseOptions;
use dbsloc_reports::{parse_report_with_document, pdf, ReportError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::bids::Subject;
use crate::outputs::write_replacing;
use crate::sha256::{sha256_hex, Sha256};
use crate::vat::settings::{
    check, label_is_valid, label_mat_paths, load, normalise, save, save_unstamped,
    stimparameters_mat_path, stimulation_json_path, strip_stimparameters_write_time,
    write_stimparameters_mat, write_stimparameters_mat_unstamped, CaseSpec, Contact, Mode,
    ModelKind, Polarity, Side, Source, StimError, Stimulation, SOURCES_PER_SIDE,
};
use crate::version::{GIT_HASH, VERSION};

pub use dbsloc_reports::anonymize::Shift;
pub use dbsloc_reports::model::{self, SessionReport};

/// Environment variable that overrides the path of the anonymisation key file.
pub const KEY_FILE_ENV: &str = "DBSLOC_ANONYMIZATION_KEY_FILE";
const INDEX_REL: &str = "clinical/reports/index.json";

/// Failure of a report import.
#[derive(Debug, thiserror::Error)]
pub enum ReportImportError {
    /// Failure of the PDF layer or the vendor module (`E_REPORT_*`).
    #[error("{0}")]
    Report(#[from] ReportError),
    /// File system or serialisation failure.
    #[error("E_REPORT_io: {0}")]
    Io(String),
    /// The anonymisation key could not be read or created.
    #[error("E_REPORT_key: {0}")]
    Key(String),
    /// Stimulation settings failure.
    #[error("E_REPORT_settings: {0}")]
    Settings(#[from] StimError),
}

/// Which snapshot of the report becomes stimulation labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapshotChoice {
    /// The final snapshot (default).
    #[default]
    Final,
    /// The initial snapshot.
    Initial,
    /// Both snapshots.
    Both,
    /// No labels.
    None,
}

impl SnapshotChoice {
    /// Parses `final`, `initial`, `both` or `none`.
    pub fn parse(text: &str) -> Option<SnapshotChoice> {
        match text.trim().to_ascii_lowercase().as_str() {
            "final" => Some(SnapshotChoice::Final),
            "initial" => Some(SnapshotChoice::Initial),
            "both" => Some(SnapshotChoice::Both),
            "none" => Some(SnapshotChoice::None),
            _ => None,
        }
    }
}

/// Options of one import.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Remove identity and shift dates (`specs/16` §4).
    pub anonymize: bool,
    /// Snapshot that yields stimulation labels.
    pub snapshot: SnapshotChoice,
    /// Group ids to import; `None` = every group with a program above 0 amplitude.
    pub groups: Option<Vec<String>>,
    /// Prefix of every label.
    pub label_prefix: Option<String>,
    /// Keep a copy of the PDF in `sourcedata` (never when anonymising).
    pub source_copy: bool,
    /// Anonymisation key file; default `DBSLOC_ANONYMIZATION_KEY_FILE`, else `<user data dir>/anonymization.key`.
    pub key_file: Option<PathBuf>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            anonymize: false,
            snapshot: SnapshotChoice::Final,
            groups: None,
            label_prefix: None,
            source_copy: true,
            key_file: None,
        }
    }
}

/// Outcome of one import.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    /// The written record JSON.
    pub record_path: PathBuf,
    /// `clinical/reports/index.json`.
    pub index_path: PathBuf,
    /// Labels written.
    pub labels_written: Vec<String>,
    /// Labels that already existed with identical settings.
    pub labels_unchanged: Vec<String>,
    /// Labels that could not be derived, with the reason.
    pub labels_failed: Vec<(String, String)>,
    /// The archived PDF, when one is kept.
    pub source_copy: Option<PathBuf>,
    /// `dbsloc: warning: report: …` lines.
    pub warnings: Vec<String>,
    /// Whether the record is anonymised.
    pub anonymized: bool,
}

/// One row of `clinical/reports/index.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Record folder relative to `clinical/reports`.
    pub folder: String,
    /// Vendor id.
    pub vendor: String,
    /// Session date-time as in the record.
    pub session: Option<String>,
    /// Whether the record is anonymised.
    pub anonymized: bool,
    /// SHA-256 of the source PDF.
    pub sha256: String,
    /// Stimulation labels derived from the report.
    pub labels: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    reports: Vec<IndexEntry>,
}

fn io_err(path: &Path, e: impl std::fmt::Display) -> ReportImportError {
    ReportImportError::Io(format!("{}: {e}", path.display()))
}

fn reports_root(sub: &Subject) -> PathBuf {
    sub.subj_dir.join("clinical").join("reports")
}

fn index_path(sub: &Subject) -> PathBuf {
    sub.subj_dir.join(INDEX_REL)
}

fn stamp_parser(record: &mut SessionReport) {
    record.source.parser.product = "dbsloc".to_owned();
    record.source.parser.version = VERSION.to_owned();
    record.source.parser.git_hash = GIT_HASH.to_owned();
}

fn digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

fn session_compact(date: Option<&str>) -> String {
    let d = digits(date.unwrap_or_default());
    if d.len() >= 12 {
        d[..12].to_owned()
    } else if d.len() >= 8 {
        format!("{}0000", &d[..8])
    } else {
        "unknown".to_owned()
    }
}

pub(crate) fn random_key() -> [u8; 32] {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let pid = std::process::id();
    let mut seed: Vec<u8> = Vec::new();
    for _ in 0..2 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u128(nanos);
        h.write_u32(pid);
        seed.extend_from_slice(&h.finish().to_le_bytes());
    }
    seed.extend_from_slice(&nanos.to_le_bytes());
    seed.extend_from_slice(&pid.to_le_bytes());
    let mut hasher = Sha256::new();
    hasher.update(&seed);
    hasher.finish()
}

/// Path of the anonymisation key: `explicit`, else `DBSLOC_ANONYMIZATION_KEY_FILE`, else `<user data dir>/anonymization.key`.
pub fn anonymization_key_path(explicit: Option<&Path>) -> Result<PathBuf, ReportImportError> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Some(v) = std::env::var_os(KEY_FILE_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(v));
    }
    crate::template_catalog::user_data_dir()
        .map(|d| d.join("anonymization.key"))
        .ok_or_else(|| {
            ReportImportError::Key(
                "no user data directory; pass --key-file or set DBSLOC_ANONYMIZATION_KEY_FILE"
                    .to_owned(),
            )
        })
}

pub fn key_created_warning(path: &Path) -> String {
    format!(
        "dbsloc: warning: report: W_REPORT_key_created: a new anonymisation key was created at {}; reports of subjects anonymised with another key will no longer line up",
        path.display()
    )
}

/// Reads the key file, creating 32 random bytes on first use.
pub fn load_or_create_key(path: &Path) -> Result<Vec<u8>, ReportImportError> {
    load_or_create_key_noting(path).map(|(key, _)| key)
}

pub fn load_or_create_key_noting(path: &Path) -> Result<(Vec<u8>, bool), ReportImportError> {
    let read = |p: &Path| -> Result<Vec<u8>, ReportImportError> {
        let bytes = std::fs::read(p)
            .map_err(|e| ReportImportError::Key(format!("{}: {e}", p.display())))?;
        if bytes.is_empty() {
            return Err(ReportImportError::Key(format!("{} is empty", p.display())));
        }
        Ok(bytes)
    };
    if path.is_file() {
        return read(path).map(|k| (k, false));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| ReportImportError::Key(format!("{}: {e}", parent.display())))?;
    }
    let key = random_key();
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path);
    match created {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&key)
                .map_err(|e| ReportImportError::Key(format!("{}: {e}", path.display())))?;
            drop(file);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| ReportImportError::Key(format!("{}: {e}", path.display())))?;
            }
            Ok((key.to_vec(), true))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read(path).map(|k| (k, false)),
        Err(e) => Err(ReportImportError::Key(format!("{}: {e}", path.display()))),
    }
}

fn parse_and_stamp_document(
    pdf: &Path,
    keep_sections: bool,
) -> Result<(SessionReport, PdfDocument), ReportImportError> {
    let (mut record, doc) = parse_report_with_document(pdf, &ParseOptions { keep_sections })?;
    stamp_parser(&mut record);
    Ok((record, doc))
}

fn parse_and_stamp(pdf: &Path, keep_sections: bool) -> Result<SessionReport, ReportImportError> {
    parse_and_stamp_document(pdf, keep_sections).map(|(record, _)| record)
}

/// Parses a report without writing anything.
pub fn inspect(pdf: &Path) -> Result<SessionReport, ReportImportError> {
    parse_and_stamp(pdf, true)
}

/// The record as it would be anonymised with a zero shift (no key needed).
pub fn anonymize_preview(record: &SessionReport) -> SessionReport {
    anonymize(record, Shift { days: 0 })
}

/// The `W_REPORT_free_text` warning line for the free-text fields a record still carries.
pub fn free_text_warning(record: &SessionReport) -> Option<String> {
    let fields = free_text_fields(record);
    if fields.is_empty() {
        return None;
    }
    let list = fields
        .iter()
        .map(|(path, text)| format!("{path} = {text:?}"))
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "dbsloc: warning: report: W_REPORT_free_text: free-text fields are kept, review them for names: {list}"
    ))
}

pub struct AnonymizedFile {
    pub key_created: Option<PathBuf>,
    pub result: Result<SessionReport, ReportImportError>,
}

/// Parses `pdf`, anonymises it for `patient_key` (`sub-<id>`) and writes the record to `out`.
pub fn anonymize_file(
    pdf: &Path,
    patient_key: &str,
    key_file: Option<&Path>,
    out: &Path,
) -> AnonymizedFile {
    let mut key_created = None;
    let result = (|| {
        let record = parse_and_stamp(pdf, false)?;
        let key_path = anonymization_key_path(key_file)?;
        let (key, created) = load_or_create_key_noting(&key_path)?;
        if created {
            key_created = Some(key_path);
        }
        let shifted = anonymize(&record, shift_for(&key, patient_key));
        let text = canonical_json(&shifted).map_err(|e| ReportImportError::Io(e.to_string()))?;
        write_replacing(out, text.as_bytes()).map_err(|e| io_err(out, e))?;
        Ok(shifted)
    })();
    AnonymizedFile {
        key_created,
        result,
    }
}

/// The reports imported into a subject (`clinical/reports/index.json`); empty when there are none.
pub fn list_reports(subject: &Subject) -> Vec<IndexEntry> {
    read_index(&index_path(subject)).reports
}

fn read_index(path: &Path) -> IndexFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Parses `pdf` and imports it into `subject` (`specs/16` §5).
pub fn import_report(
    subject: &Subject,
    pdf: &Path,
    opts: &ImportOptions,
) -> Result<ImportReport, ReportImportError> {
    let bytes = std::fs::read(pdf).map_err(|e| io_err(pdf, e))?;
    let (record, doc) = parse_and_stamp_document(pdf, !opts.anonymize)?;
    import_parsed(subject, record, &bytes, Some(&doc), opts)
}

struct TrueDates {
    session: Option<String>,
    birth: Option<String>,
}

fn is_vendor_folder(name: &str) -> bool {
    Vendor::ALL.iter().any(|v| v.as_str() == name)
}

fn report_folder_parts<'a>(rel: &'a str, sha256: &str) -> Option<(&'a str, &'a str)> {
    let (vendor, leaf) = rel.split_once('/')?;
    let (session, hash) = leaf.split_once('_')?;
    (is_vendor_folder(vendor) && hash.len() == 8 && sha256.get(..8) == Some(hash))
        .then_some((vendor, session))
}

fn is_report_folder_name(rel: &str, sha256: &str) -> bool {
    report_folder_parts(rel, sha256).is_some_and(|(_, session)| {
        session.len() == 12 && session.chars().all(|c| c.is_ascii_digit())
    })
}

fn is_undated_folder_name(rel: &str, sha256: &str) -> bool {
    report_folder_parts(rel, sha256).is_some_and(|(_, session)| session == "unknown")
}

fn old_record(sub: &Subject, entry: &IndexEntry) -> Option<SessionReport> {
    let path = reports_root(sub)
        .join(&entry.folder)
        .join(format!("sub-{}_desc-sessionreport.json", sub.subj_id));
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn kept_true_dates(old: &SessionReport, true_dates: &TrueDates) -> bool {
    let session_equal = old.session.date.is_some() && old.session.date == true_dates.session;
    let birth_equal = match (&old.patient.date_of_birth, &true_dates.birth) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    session_equal && birth_equal
}

fn supersedable(sub: &Subject, entry: &IndexEntry, sha256: &str, true_dates: &TrueDates) -> bool {
    is_report_folder_name(&entry.folder, sha256)
        && old_record(sub, entry).is_some_and(|old| kept_true_dates(&old, true_dates))
}

fn check_shift_mismatch(
    sub: &Subject,
    sha256: &str,
    true_dates: &TrueDates,
    shift: Shift,
    folder: &str,
    warnings: &mut Vec<String>,
) {
    let Some(dob) = true_dates.birth.as_deref() else {
        return;
    };
    let Some(expected) = shift_days(dob, shift.days) else {
        return;
    };
    for entry in list_reports(sub).into_iter().filter(|e| {
        e.anonymized
            && e.folder != folder
            && !(e.sha256 == sha256 && supersedable(sub, e, sha256, true_dates))
    }) {
        if !is_vendor_folder(entry.folder.split('/').next().unwrap_or_default()) {
            continue;
        }
        let existing = old_record(sub, &entry).and_then(|r| r.patient.date_of_birth);
        if let Some(existing) = existing {
            if existing != expected {
                warnings.push(
                    "dbsloc: warning: report: W_REPORT_shift_mismatch: the stored date of birth of an earlier anonymised report of this subject does not match the one this key gives: either the anonymisation key differs from the one used before, or that report was written by version 0.1.0 or a build before the zero-shift fix and kept its true dates. Import this subject's reports again with the key you want to keep (the original PDFs are needed)"
                        .to_owned(),
                );
            }
            return;
        }
    }
}

fn record_folder(record: &SessionReport) -> String {
    format!(
        "{}/{}_{}",
        record.vendor.as_str(),
        session_compact(record.session.date.as_deref()),
        record
            .source
            .sha256
            .get(..8)
            .unwrap_or(&record.source.sha256)
    )
}

/// Imports a parsed record; `pdf_bytes` is the source file (page text, source copy).
pub fn import_record(
    subject: &Subject,
    record: SessionReport,
    pdf_bytes: &[u8],
    opts: &ImportOptions,
) -> Result<ImportReport, ReportImportError> {
    import_parsed(subject, record, pdf_bytes, None, opts)
}

fn import_parsed(
    subject: &Subject,
    mut record: SessionReport,
    pdf_bytes: &[u8],
    document: Option<&PdfDocument>,
    opts: &ImportOptions,
) -> Result<ImportReport, ReportImportError> {
    stamp_parser(&mut record);
    let mut warnings: Vec<String> = Vec::new();
    let mut original_dates: Option<TrueDates> = None;
    let patient_key = format!("sub-{}", subject.subj_id);
    if record.source.sha256.is_empty() {
        record.source.sha256 = sha256_hex(pdf_bytes);
    }
    if opts.anonymize {
        let key_path = anonymization_key_path(opts.key_file.as_deref())?;
        let (key, created) = load_or_create_key_noting(&key_path)?;
        if created {
            warnings.push(key_created_warning(&key_path));
        }
        let shift = shift_for(&key, &patient_key);
        let shifted = anonymize(&record, shift);
        let folder = record_folder(&shifted);
        let true_dates = TrueDates {
            session: record.session.date.clone(),
            birth: record.patient.date_of_birth.clone(),
        };
        check_shift_mismatch(
            subject,
            &record.source.sha256,
            &true_dates,
            shift,
            &folder,
            &mut warnings,
        );
        original_dates = Some(true_dates);
        record = shifted;
        if let Some(w) = free_text_warning(&record) {
            warnings.push(w);
        }
    }
    for d in &record.diagnostics {
        if d.code == "W_REPORT_contact_sum" {
            continue;
        }
        let word = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        warnings.push(format!("dbsloc: {word}: report: {}: {}", d.code, d.message));
    }

    let folder = record_folder(&record);
    let vendor = record.vendor.as_str();
    let dir = reports_root(subject).join(&folder);
    let name = format!("sub-{}_desc-sessionreport", subject.subj_id);
    let record_path = dir.join(format!("{name}.json"));
    let text = canonical_json(&record).map_err(|e| ReportImportError::Io(e.to_string()))?;
    write_replacing(&record_path, text.as_bytes()).map_err(|e| io_err(&record_path, e))?;

    let txt_path = dir.join(format!("{name}.txt"));
    if opts.anonymize {
        let _ = std::fs::remove_file(&txt_path);
    } else {
        let parsed;
        let doc = match document {
            Some(doc) => Ok(doc),
            None => {
                parsed = pdf::read_bytes(pdf_bytes);
                parsed.as_ref()
            }
        };
        match doc {
            Ok(doc) => {
                let pages: Vec<String> = doc
                    .pages
                    .iter()
                    .map(|p| pdf::layout::text_in(p, None))
                    .collect();
                write_replacing(&txt_path, pages.join("\u{c}").as_bytes())
                    .map_err(|e| io_err(&txt_path, e))?;
            }
            Err(e) => warnings.push(format!(
                "dbsloc: warning: report: the page text could not be extracted: {e}"
            )),
        }
    }

    let source_copy = if opts.source_copy && !opts.anonymize {
        Some(copy_source(
            subject,
            vendor,
            &record.source.filename,
            pdf_bytes,
        )?)
    } else {
        None
    };

    let record_rel = format!("clinical/reports/{folder}/{name}.json");
    let mut out = ImportReport {
        record_path,
        index_path: index_path(subject),
        source_copy,
        anonymized: record.anonymized,
        ..ImportReport::default()
    };

    if opts.snapshot != SnapshotChoice::None {
        derive_labels(subject, &record, &record_rel, opts, &mut out, &mut warnings);
    }

    let removed = match &original_dates {
        Some(true_dates) => {
            remove_superseded(subject, &record, &folder, true_dates, &out, &mut warnings)
        }
        None => Vec::new(),
    };
    update_index(subject, &record, &folder, &out, &removed)?;
    out.warnings = warnings;
    Ok(out)
}

fn inside_reports(root: &Path, rel: &str, new_folder: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let old = root.join(rel).canonicalize().ok()?;
    let new = root.join(new_folder).canonicalize().ok()?;
    let inside = old.starts_with(&root) && old != root;
    let overlaps = old == new || new.starts_with(&old);
    (inside && !overlaps).then_some(old)
}

fn remove_superseded(
    sub: &Subject,
    record: &SessionReport,
    folder: &str,
    true_dates: &TrueDates,
    out: &ImportReport,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let root = reports_root(sub);
    let sha = &record.source.sha256;
    let short = sha.get(..8).unwrap_or(sha);
    let mut removed = Vec::new();
    let old: Vec<IndexEntry> = list_reports(sub)
        .into_iter()
        .filter(|e| e.anonymized && &e.sha256 == sha && e.folder != folder)
        .collect();
    for entry in old {
        if is_undated_folder_name(&entry.folder, sha) {
            warnings.push(format!(
                "dbsloc: warning: report: W_REPORT_superseded: an earlier anonymised copy of this report (source checksum {short}) has no session date and could not be checked; remove it yourself if it kept its true dates (clinical/reports/index.json lists it)"
            ));
            continue;
        }
        if !is_report_folder_name(&entry.folder, sha) {
            warnings.push(format!(
                "dbsloc: warning: report: W_REPORT_superseded: an earlier anonymised copy of this report (source checksum {short}) was not removed because its entry in clinical/reports/index.json does not name a report folder of this subject; check that file and remove the copy yourself if it kept its true dates"
            ));
            continue;
        }
        let remaining = || {
            let stale = entry
                .labels
                .iter()
                .filter(|l| !out.labels_written.contains(l) && !out.labels_unchanged.contains(l))
                .filter(|l| stimulation_json_path(&sub.subj_dir, &sub.subj_id, l).is_file())
                .count();
            if stale == 0 {
                String::new()
            } else {
                format!(
                    "; {stale} stimulation label folder(s) created from it remain and carry the earlier date in their names and inside their files: list them with `dbsloc stim list <dataset_root> {}`, delete them, and create again under a new name the settings you still need (`dbsloc stim set` or the stimulation panel)",
                    sub.subj_id
                )
            }
        };
        if !root.join(&entry.folder).exists() {
            if entry.session.is_some() && entry.session == true_dates.session {
                warnings.push(format!(
                    "dbsloc: warning: report: W_REPORT_superseded: an earlier anonymised copy of this report (source checksum {short}) had kept its true dates and its record folder was already deleted; its index entry was removed{}",
                    remaining()
                ));
                removed.push(entry.folder);
            }
            continue;
        }
        if !supersedable(sub, &entry, sha, true_dates) {
            continue;
        }
        let Some(old_dir) = inside_reports(&root, &entry.folder, folder) else {
            warnings.push(format!(
                "dbsloc: warning: report: W_REPORT_superseded: an earlier anonymised copy of this report (source checksum {short}) kept its true dates but was not removed because it does not lie safely inside clinical/reports; check clinical/reports/index.json and remove it yourself"
            ));
            continue;
        };
        if std::fs::remove_dir_all(&old_dir).is_err() {
            warnings.push(format!(
                "dbsloc: warning: report: W_REPORT_superseded: an earlier anonymised copy of this report (source checksum {short}) kept its true dates but could not be removed; check clinical/reports/index.json and remove it yourself"
            ));
            continue;
        }
        warnings.push(format!(
            "dbsloc: warning: report: W_REPORT_superseded: an earlier anonymised copy of this report (source checksum {short}) had kept its true dates and was removed{}",
            remaining()
        ));
        removed.push(entry.folder);
    }
    removed
}

fn safe_file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if base.is_empty() {
        "report.pdf".to_owned()
    } else {
        base
    }
}

fn copy_source(
    sub: &Subject,
    vendor: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<PathBuf, ReportImportError> {
    let dir = sub.sourcedata_dir.join("reports").join(vendor);
    let name = safe_file_name(filename);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let sha = sha256_hex(bytes);
    let mut n = 1u32;
    loop {
        let candidate = if n == 1 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem}-{n}{ext}"))
        };
        if !candidate.exists() {
            write_replacing(&candidate, bytes).map_err(|e| io_err(&candidate, e))?;
            return Ok(candidate);
        }
        if std::fs::read(&candidate).is_ok_and(|b| sha256_hex(&b) == sha) {
            return Ok(candidate);
        }
        n += 1;
    }
}

fn update_index(
    sub: &Subject,
    record: &SessionReport,
    folder: &str,
    out: &ImportReport,
    removed: &[String],
) -> Result<(), ReportImportError> {
    let path = index_path(sub);
    let mut index = read_index(&path);
    let previous: Vec<String> = index
        .reports
        .iter()
        .find(|e| e.folder == folder)
        .map(|e| e.labels.clone())
        .unwrap_or_default();
    let mut labels: Vec<String> = previous
        .into_iter()
        .filter(|l| stimulation_json_path(&sub.subj_dir, &sub.subj_id, l).is_file())
        .collect();
    for l in out.labels_written.iter().chain(&out.labels_unchanged) {
        if !labels.contains(l) {
            labels.push(l.clone());
        }
    }
    let entry = IndexEntry {
        folder: folder.to_owned(),
        vendor: record.vendor.as_str().to_owned(),
        session: record.session.date.clone(),
        anonymized: record.anonymized,
        sha256: record.source.sha256.clone(),
        labels,
    };
    index
        .reports
        .retain(|e| e.folder != folder && !removed.contains(&e.folder));
    index.reports.push(entry);
    index.reports.sort_by(|a, b| a.folder.cmp(&b.folder));
    let mut text =
        serde_json::to_string_pretty(&index).map_err(|e| ReportImportError::Io(e.to_string()))?;
    text.push('\n');
    write_replacing(&path, text.as_bytes()).map_err(|e| io_err(&path, e))
}

fn hemisphere_of(side: Side) -> Hemisphere {
    match side {
        Side::Right => Hemisphere::Right,
        Side::Left => Hemisphere::Left,
    }
}

fn lead_counts(record: &SessionReport) -> [Result<usize, String>; 2] {
    Side::ALL.map(|side| {
        let Some(lead) = record
            .leads
            .iter()
            .find(|l| l.hemisphere == hemisphere_of(side))
        else {
            return Err(format!("the report has no {side} lead"));
        };
        let Some(model) = lead.catalogue_model.as_deref() else {
            return Err(format!(
                "the {side} lead model {:?} has no catalogue model",
                lead.vendor_model
            ));
        };
        dbsloc_reconstruct::electrode_by_name(model)
            .map(|e| e.n_contacts)
            .ok_or_else(|| {
                format!("the {side} lead model {model:?} is not in the electrode catalogue")
            })
    })
}

fn warn_model_mismatch(sub: &Subject, record: &SessionReport, warnings: &mut Vec<String>) {
    let Ok(Some(model)) = crate::uiprefs::subject_electrode_model(sub) else {
        return;
    };
    let same = |a: &str, b: &str| match (
        dbsloc_reconstruct::electrode_by_name(a),
        dbsloc_reconstruct::electrode_by_name(b),
    ) {
        (Some(x), Some(y)) => x.elmodel == y.elmodel,
        _ => a.trim().eq_ignore_ascii_case(b.trim()),
    };
    for lead in &record.leads {
        if let Some(m) = lead.catalogue_model.as_deref() {
            if !same(m, &model) {
                let side = match lead.hemisphere {
                    Hemisphere::Left => "left",
                    Hemisphere::Right => "right",
                };
                warnings.push(format!(
                    "dbsloc: warning: report: W_REPORT_model_mismatch: the {side} lead model {m:?} differs from the subject's electrode model {model:?}"
                ));
            }
        }
    }
}

fn group_has_stimulation(group: &Group) -> bool {
    [&group.hemispheres.left, &group.hemispheres.right]
        .into_iter()
        .flatten()
        .any(|h| h.programs.iter().any(|p| p.amplitude.unwrap_or(0.0) > 0.0))
}

fn to_percent(hundredths: &[i64]) -> Vec<f64> {
    hundredths.iter().map(|v| *v as f64 / 100.0).collect()
}

fn equal_shares(n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let base = 10_000 / n as i64;
    let mut v = vec![base; n];
    v[n - 1] = 10_000 - base * (n as i64 - 1);
    to_percent(&v)
}

fn proportional_shares(amps: &[f64]) -> Option<Vec<f64>> {
    let sum: f64 = amps.iter().sum();
    if amps.is_empty() || !(sum.is_finite() && sum > 0.0) || amps.iter().any(|a| *a < 0.0) {
        return None;
    }
    let mut v: Vec<i64> = amps
        .iter()
        .map(|a| (a / sum * 10_000.0).round() as i64)
        .collect();
    let n = v.len();
    let head: i64 = v[..n - 1].iter().sum();
    let last = 10_000 - head;
    if last < 0 {
        return None;
    }
    v[n - 1] = last;
    Some(to_percent(&v))
}

fn polarity_of(p: RecPolarity) -> Polarity {
    match p {
        RecPolarity::Off => Polarity::Off,
        RecPolarity::Cathode => Polarity::Cathode,
        RecPolarity::Anode => Polarity::Anode,
    }
}

fn impedance_kohm(record: &SessionReport, hemisphere: Hemisphere, label: &str) -> f64 {
    record
        .impedances
        .iter()
        .filter(|i| i.kind == ImpedanceKind::Monopolar && i.hemisphere == hemisphere)
        .find(|i| {
            (i.a.eq_ignore_ascii_case("case") && i.b.eq_ignore_ascii_case(label))
                || (i.b.eq_ignore_ascii_case("case") && i.a.eq_ignore_ascii_case(label))
        })
        .and_then(|i| i.ohm)
        .filter(|o| o.is_finite() && *o > 0.0)
        .map_or(1.0, |o| o / 1000.0)
}

fn build_source(
    record: &SessionReport,
    hemisphere: Hemisphere,
    program: &Program,
    n: usize,
    label: &str,
    side: Side,
    warnings: &mut Vec<String>,
) -> Source {
    let mode = match (program.mode, program.unit.as_deref()) {
        (Some(StimMode::Voltage), _) | (None, Some("V")) => Mode::Voltage,
        _ => Mode::Current,
    };
    let mut src = Source::blank(program.index, n);
    src.mode = mode;
    src.amplitude = program.amplitude.unwrap_or(0.0);
    if let Some(pw) = program.pulse_width_us {
        src.pulse_width_us = pw;
    }
    src.frequency_hz = program.frequency_hz;

    let used: Vec<&ProgramContact> = program
        .contacts
        .iter()
        .filter(|c| c.polarity != RecPolarity::Off)
        .collect();
    let mut percents: Vec<(usize, f64)> = Vec::new();
    for pol in [RecPolarity::Cathode, RecPolarity::Anode] {
        let members: Vec<usize> = used
            .iter()
            .enumerate()
            .filter(|(_, c)| c.polarity == pol)
            .map(|(i, _)| i)
            .collect();
        let printed: Option<Vec<f64>> = members.iter().map(|i| used[*i].amplitude).collect();
        let proportional = match printed {
            Some(amps) if pol == RecPolarity::Cathode && mode == Mode::Current => {
                proportional_shares(&amps)
            }
            _ => None,
        };
        let shares = proportional.unwrap_or_else(|| equal_shares(members.len()));
        percents.extend(members.into_iter().zip(shares));
    }
    for (i, percent) in percents {
        let c = used[i];
        let contact = Contact {
            index: c.k,
            polarity: if percent > 0.0 {
                polarity_of(c.polarity)
            } else {
                Polarity::Off
            },
            percent,
            impedance_kohm: impedance_kohm(record, hemisphere, &c.label),
        };
        match src.contacts.iter_mut().find(|x| x.index == c.k) {
            Some(slot) => *slot = contact,
            None => src.contacts.push(contact),
        }
    }

    let case_polarity = program.case.map_or(Polarity::Off, polarity_of);
    src.case = if case_polarity == Polarity::Off {
        CaseSpec {
            polarity: Polarity::Off,
            percent: 0.0,
        }
    } else if used
        .iter()
        .any(|c| polarity_of(c.polarity) == case_polarity)
    {
        warnings.push(format!(
            "dbsloc: warning: report: W_REPORT_case_share: {label}: {side} program {}: the case shares its polarity with lead contacts and its share is not printed; the lead contacts take equal shares and the case 0",
            program.index
        ));
        CaseSpec {
            polarity: Polarity::Off,
            percent: 0.0,
        }
    } else {
        CaseSpec {
            polarity: case_polarity,
            percent: 100.0,
        }
    };

    if mode == Mode::Current && program.contact_sum_matches_amplitude == Some(false) {
        let sum: f64 = used
            .iter()
            .filter(|c| c.polarity == RecPolarity::Cathode)
            .filter_map(|c| c.amplitude)
            .sum();
        warnings.push(format!(
            "dbsloc: warning: report: {label}: printed contact currents sum to {sum} mA, amplitude {} mA; percentages follow the printed shares",
            src.amplitude
        ));
    }
    src
}

fn content_sha(stim: &Stimulation) -> String {
    let mut value = serde_json::to_value(stim).unwrap_or(Value::Null);
    if let Some(o) = value.as_object_mut() {
        o.remove("provenance");
        o.remove("label");
    }
    sha256_hex(value.to_string().as_bytes())
}

fn has_write_time(stim: &Stimulation) -> bool {
    !stim.provenance.written_at.is_empty()
        || stim
            .provenance
            .software
            .get("written_at")
            .is_some_and(|v| !v.is_null())
}

enum Outcome {
    Written(String),
    Unchanged(String),
    UnchangedStampKept(String),
}

struct LabelJob<'a> {
    snapshot: &'static str,
    group: &'a Group,
    base: String,
}

fn derive_label(
    sub: &Subject,
    record: &SessionReport,
    record_rel: &str,
    job: &LabelJob<'_>,
    include_zero: bool,
    counts: &[Result<usize, String>; 2],
    warnings: &mut Vec<String>,
) -> Result<Outcome, String> {
    let base = job.base.as_str();
    if !label_is_valid(base) {
        return Err(format!(
            "E_REPORT_label: label {base:?} must be 1-64 characters of [A-Za-z0-9._-] and not start with '.'"
        ));
    }
    let mut n_per_side = [0usize; 2];
    let mut mapped: Vec<(Side, Hemisphere, &Program)> = Vec::new();
    for side in Side::ALL {
        let hemisphere = hemisphere_of(side);
        let settings = match hemisphere {
            Hemisphere::Right => &job.group.hemispheres.right,
            Hemisphere::Left => &job.group.hemispheres.left,
        };
        let programs: Vec<&Program> = settings
            .iter()
            .flat_map(|h| h.programs.iter())
            .filter(|p| (1..=SOURCES_PER_SIDE).contains(&p.index))
            .filter(|p| include_zero || p.amplitude.unwrap_or(0.0) > 0.0)
            .collect();
        match &counts[side.index()] {
            Ok(n) => n_per_side[side.index()] = *n,
            Err(reason) if !programs.is_empty() => {
                return Err(format!("E_REPORT_lead_model: {reason}"));
            }
            Err(_) => {}
        }
        if programs.len() > 1 {
            warnings.push(format!(
                "dbsloc: note: report: W_REPORT_interleaving: {base}: {side} has {} programs, imported as sources 1 and 2",
                programs.len()
            ));
        }
        mapped.extend(programs.into_iter().map(|p| (side, hemisphere, p)));
    }

    let mut stim = Stimulation::blank(base, &sub.subj_id, ModelKind::Efield, n_per_side);
    for (side, hemisphere, program) in mapped {
        let n = n_per_side[side.index()];
        let src = build_source(record, hemisphere, program, n, base, side, warnings);
        let slot = (src.index - 1) as usize;
        stim.sides.get_mut(side).sources[slot] = src;
    }
    stim.provenance.imported_from = Some(json!({
        "report": record_rel,
        "sha256": record.source.sha256,
        "snapshot": job.snapshot,
        "group": job.group.id,
    }));
    warnings.extend(normalise(&mut stim, n_per_side));
    let report = check(&stim, n_per_side);
    warnings.extend(report.warnings);
    if !report.errors.is_empty() {
        return Err(report
            .errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "));
    }

    let wanted = content_sha(&stim);
    let mut suffix = 1u32;
    let label = loop {
        let candidate = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base}_{suffix}")
        };
        if !label_is_valid(&candidate) || suffix > 999 {
            return Err(format!("E_REPORT_label: no free label for {base:?}"));
        }
        let path = stimulation_json_path(&sub.subj_dir, &sub.subj_id, &candidate);
        if !path.exists() {
            break candidate;
        }
        if let Ok(existing) = load(&sub.subj_dir, &sub.subj_id, &candidate) {
            if content_sha(&existing) == wanted {
                if record.anonymized {
                    let failed = label_mat_paths(&sub.subj_dir, &sub.subj_id, &candidate)
                        .iter()
                        .filter(|mat| strip_stimparameters_write_time(mat).is_err())
                        .count();
                    if has_write_time(&existing) {
                        save_unstamped(&sub.subj_dir, &existing).map_err(|e| e.to_string())?;
                    }
                    if failed > 0 {
                        return Ok(Outcome::UnchangedStampKept(candidate));
                    }
                }
                return Ok(Outcome::Unchanged(candidate));
            }
        }
        suffix += 1;
    };
    stim.label.clone_from(&label);
    if record.anonymized {
        save_unstamped(&sub.subj_dir, &stim)
    } else {
        save(&sub.subj_dir, &stim)
    }
    .map_err(|e| e.to_string())?;
    let saved = load(&sub.subj_dir, &sub.subj_id, &label).map_err(|e| e.to_string())?;
    let mat = stimparameters_mat_path(&sub.subj_dir, &sub.subj_id, "native", &label);
    let mat_warnings = if record.anonymized {
        write_stimparameters_mat_unstamped(&mat, &saved, [0.0, 0.0])
    } else {
        write_stimparameters_mat(&mat, &saved, [0.0, 0.0])
    }
    .map_err(|e| e.to_string())?;
    warnings.extend(mat_warnings);
    Ok(Outcome::Written(label))
}

fn derive_labels(
    sub: &Subject,
    record: &SessionReport,
    record_rel: &str,
    opts: &ImportOptions,
    out: &mut ImportReport,
    warnings: &mut Vec<String>,
) {
    warn_model_mismatch(sub, record, warnings);
    let counts = lead_counts(record);
    let date = digits(record.session.date.as_deref().unwrap_or_default());
    let prefix = opts
        .label_prefix
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let mut snapshots: Vec<(&'static str, &Snapshot)> = Vec::new();
    if matches!(opts.snapshot, SnapshotChoice::Final | SnapshotChoice::Both) {
        snapshots.push(("final", &record.snapshots.final_));
    }
    if matches!(
        opts.snapshot,
        SnapshotChoice::Initial | SnapshotChoice::Both
    ) {
        snapshots.push(("initial", &record.snapshots.initial));
    }
    let mut jobs: Vec<LabelJob<'_>> = Vec::new();
    for (name, snapshot) in snapshots {
        let suffix = if name == "initial" { "_initial" } else { "" };
        let label_of = |id: &str| {
            let head = date.get(..8).unwrap_or("unknown");
            match prefix {
                Some(p) => format!("{p}_{head}_{id}{suffix}"),
                None => format!("{head}_{id}{suffix}"),
            }
        };
        match &opts.groups {
            Some(wanted) => {
                for id in wanted {
                    let id = id.trim();
                    match snapshot
                        .groups
                        .iter()
                        .find(|g| g.id.eq_ignore_ascii_case(id))
                    {
                        Some(group) => jobs.push(LabelJob {
                            snapshot: name,
                            group,
                            base: label_of(&group.id),
                        }),
                        None => out.labels_failed.push((
                            label_of(id),
                            format!("the {name} snapshot has no group {id:?}"),
                        )),
                    }
                }
            }
            None => {
                for group in snapshot.groups.iter().filter(|g| group_has_stimulation(g)) {
                    jobs.push(LabelJob {
                        snapshot: name,
                        group,
                        base: label_of(&group.id),
                    });
                }
            }
        }
    }
    let include_zero = opts.groups.is_some();
    let mut stamp_kept = 0usize;
    for job in jobs {
        if date.len() < 8 {
            out.labels_failed
                .push((job.base, "the session date is unknown".to_owned()));
            continue;
        }
        match derive_label(
            sub,
            record,
            record_rel,
            &job,
            include_zero,
            &counts,
            warnings,
        ) {
            Ok(Outcome::Written(l)) => out.labels_written.push(l),
            Ok(Outcome::Unchanged(l)) => out.labels_unchanged.push(l),
            Ok(Outcome::UnchangedStampKept(l)) => {
                stamp_kept += 1;
                out.labels_unchanged.push(l);
            }
            Err(reason) => out.labels_failed.push((job.base, reason)),
        }
    }
    if stamp_kept > 0 {
        warnings.push(format!(
            "dbsloc: warning: report: W_REPORT_stamp_kept: {stamp_kept} stimulation label(s) have a stimparameters file whose provenance still records when an earlier import ran and could not be cleaned safely; running the vat step again with `dbsloc run <dataset_root> <subj_id> --stages vat --overwrite-approved` (GUI: Rerun all selected steps) rewrites those files without it; for a subject without VTA results, `dbsloc stim show <dataset_root> <subj_id> <label> --json > label.json` followed by `dbsloc stim set <dataset_root> <subj_id> <label> --from label.json` does the same for the native file (it writes zero volumes there); this warning repeats on every anonymised import while a file stays uncleaned"
        ));
    }
}

#[cfg(test)]
#[path = "reports_tests.rs"]
mod tests;
