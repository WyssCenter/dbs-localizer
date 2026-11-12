// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Lead Group compatible group data model (`specs/14` §2, §3.1–§3.2, §10).
//!
//! The group record JSON is primary; the Lead Group `.mat` file is its twin. Reading `M`, writing
//! the twin and importing edits made in Lead Group live in [`lead_group_mat`].

use std::fmt;
use std::path::{Path, PathBuf};

pub mod atlas_stats;
pub mod calc;
pub mod clinical;
pub mod ea_stats;
pub mod exports;
pub mod fibrefilter;
pub mod flip;
pub mod fliplr;
pub mod lead_group_mat;
pub mod matio;
pub mod netmap;
pub mod overlap;
pub mod record;
pub mod stats_common;
pub mod sweetspot;
pub mod view;

#[cfg(test)]
mod fibrefilter_tests;
#[cfg(test)]
mod netmap_tests;
#[cfg(test)]
mod sweetspot_tests;
#[cfg(test)]
mod tests;

pub use clinical::{ClinicalKind, ClinicalVar, ImportOptions, ImportReport};
pub use ea_stats::{read_ea_stats, EaStats, EaStatsRead};
pub use lead_group_mat::{
    apply_imported_stimulations, backup_foreign_twin, import_twin_edits, load_subject_data, read_m,
    write_m_twin, MRead, SubjectData, TwinImport, TwinReport,
};
pub use record::{load_record, save_record, GroupRecord};

use record::{dataset_name_of_folder, find_record_file, is_alphanumeric_id, record_path};

/// Errors of the group data model.
#[derive(Debug, thiserror::Error)]
pub enum GroupError {
    /// An error with a code of `specs/14` §13.
    #[error("{code}: {message}")]
    Lg { code: &'static str, message: String },
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Json(String),
    #[error("{0}")]
    Mat(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Clinical(String),
}

/// A warning or note, with its `specs/14` §13 code when it has one.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub code: Option<&'static str>,
    pub text: String,
}

impl Note {
    /// A note with a code.
    pub fn coded(code: &'static str, text: impl Into<String>) -> Note {
        Note {
            code: Some(code),
            text: text.into(),
        }
    }

    /// A note without a code.
    pub fn plain(text: impl Into<String>) -> Note {
        Note {
            code: None,
            text: text.into(),
        }
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(c) => write!(f, "{c}: {}", self.text),
            None => f.write_str(&self.text),
        }
    }
}

/// Where a group lives: its folder, dataset name and guid.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub folder: PathBuf,
    pub dataset_name: String,
    pub guid: String,
}

impl Location {
    /// The group record JSON file of this location.
    pub fn json_path(&self) -> PathBuf {
        record_path(&self.folder, &self.dataset_name, &self.guid)
    }
}

/// Resolves the group folder (`specs/14` §2.1, §2.3). `input` is a dataset root (the group folder
/// becomes `<input>/derivatives/leadgroup/<guid>`) or an existing `derivatives/leadgroup/<id>`
/// folder; `group_dir` overrides the folder.
pub fn locate(
    input: &Path,
    guid: &str,
    group_dir: Option<&Path>,
    dataset_name: Option<&str>,
) -> Result<Location, GroupError> {
    if !is_alphanumeric_id(guid) {
        return Err(GroupError::Invalid(format!(
            "guid {guid:?} must be made of letters and digits only"
        )));
    }
    let name_of = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let (folder, guid, derived) = match group_dir {
        Some(dir) => (
            dir.to_path_buf(),
            guid.to_owned(),
            dataset_name_of_folder(dir).unwrap_or_else(|| name_of(input)),
        ),
        None => {
            let leadgroup = input.parent().is_some_and(|p| {
                p.file_name().is_some_and(|n| n == "leadgroup")
                    && p.parent()
                        .and_then(Path::file_name)
                        .is_some_and(|n| n == "derivatives")
            });
            if leadgroup {
                (
                    input.to_path_buf(),
                    name_of(input),
                    dataset_name_of_folder(input).unwrap_or_default(),
                )
            } else {
                (
                    input.join("derivatives").join("leadgroup").join(guid),
                    guid.to_owned(),
                    name_of(&crate::paths::absolute(input)),
                )
            }
        }
    };
    let dataset_name = dataset_name
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .unwrap_or(derived);
    if dataset_name.is_empty() {
        return Err(GroupError::Invalid(
            "cannot tell the dataset name; pass it explicitly".to_owned(),
        ));
    }
    if !is_alphanumeric_id(&guid) {
        return Err(GroupError::Invalid(format!(
            "the group id {guid:?} must be made of letters and digits only"
        )));
    }
    Ok(Location {
        folder,
        dataset_name,
        guid,
    })
}

/// A group opened from disk with the notes of the open.
#[derive(Debug, Clone)]
pub struct Opened {
    pub record: GroupRecord,
    pub json_path: PathBuf,
    pub folder: PathBuf,
    pub notes: Vec<Note>,
}

/// Opens a group (the JSON file or its folder). When the twin was changed outside this software
/// its edits are imported and the record saved (`specs/14` §2.7).
pub fn open_group(path: &Path) -> Result<Opened, GroupError> {
    let json_path = find_record_file(path)?;
    let folder = json_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut record = load_record(&json_path)?;
    let before = record.lead_group.mat_sha256.clone();
    let imported = import_twin_edits(&mut record, &folder)?;
    let mut notes = imported.warnings;
    if record.lead_group.mat_sha256 != before {
        if imported.changed.is_empty() {
            notes.push(Note::plain(
                "The Lead Group file was changed outside this software; no modelled field differs"
                    .to_owned(),
            ));
        } else {
            notes.push(Note::plain(format!(
                "Imported changes made in Lead Group: {}",
                imported.changed.join(", ")
            )));
        }
        let written = apply_imported_stimulations(&record, &imported.stimulations)?;
        for subject in written {
            notes.push(Note::plain(format!(
                "Local stimulation settings of {subject} differed from the group and were replaced."
            )));
        }
        save_record(&json_path, &mut record)?;
    }
    Ok(Opened {
        record,
        json_path,
        folder,
        notes,
    })
}

/// Writes the Lead Group twin and the record JSON. The record is saved even when the twin cannot
/// be written; that error is returned afterwards.
pub fn save_group(json_path: &Path, record: &mut GroupRecord) -> Result<Vec<Note>, GroupError> {
    let folder = json_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let (subjects, mut notes) = load_subject_data(record);
    let twin = write_m_twin(record, &subjects, &folder);
    save_record(json_path, record)?;
    notes.extend(twin?.warnings);
    Ok(notes)
}

/// Creates the group record and its twin at `location`; an existing record is refused.
pub fn create_group(
    location: &Location,
    title: &str,
    atlas_set: Option<&str>,
) -> Result<(GroupRecord, PathBuf, Vec<Note>), GroupError> {
    let json_path = location.json_path();
    if json_path.exists() {
        return Err(GroupError::Invalid(format!(
            "{} already exists",
            json_path.display()
        )));
    }
    let mut record = GroupRecord::new(&location.guid, &location.dataset_name);
    record.title = title.to_owned();
    record.atlas_set = atlas_set.map(str::to_owned);
    let notes = save_group(&json_path, &mut record)?;
    Ok((record, json_path, notes))
}
