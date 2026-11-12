// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Atlas overlap tables of a group (`specs/14` §6): the cohort VTA table, the contact distance
//! table and the per-group summaries, built from the subjects' statistics files.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::ea_stats::{read_ea_stats, stats_path, EaStats};
use super::record::{subject_id, GroupRecord, PatientEntry};
use super::Note;
use crate::vat::settings::{load as load_stimulation, Polarity, Side, Stimulation};

/// The space folders searched for the statistics, the reference space first.
const PREFERRED_SPACE: &str = "MNI152NLin2009bAsym";

/// One hemisphere's overlap numbers, one value per atlas structure (`None` = not computed).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SideVta {
    pub amplitudes: Vec<f64>,
    pub volume_mm3: Option<f64>,
    pub atlas_volume: Vec<Option<f64>>,
    pub intersection: Vec<Option<f64>>,
    pub n_intersection: Vec<Option<f64>>,
    pub n_within: Vec<Option<f64>>,
    pub efield_volume: Option<f64>,
    pub efield_intersection: Vec<Option<f64>>,
    pub efield_n_intersection: Vec<Option<f64>>,
}

/// The VTA statistics of one patient for the group label.
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectVta {
    pub subject: String,
    pub group: u32,
    pub group_label: String,
    pub label: String,
    pub structures: Vec<String>,
    pub sides: [Option<SideVta>; 2],
}

/// The patients with statistics, the ones without and the notes of the load.
#[derive(Debug, Clone, Default)]
pub struct VtaLoad {
    pub subjects: Vec<SubjectVta>,
    pub missing: Vec<String>,
    pub notes: Vec<Note>,
}

/// One row of the cohort VTA table (`specs/14` §6.1).
#[derive(Debug, Clone, PartialEq)]
pub struct VtaRow {
    pub subject: String,
    pub group: u32,
    pub group_label: String,
    pub side: char,
    pub source: u32,
    pub structure: String,
    pub vta_volume_mm3: Option<f64>,
    pub overlap_mm3: Option<f64>,
    pub frac_of_vta: Option<f64>,
    pub frac_of_structure: Option<f64>,
    pub structure_volume_mm3: Option<f64>,
    pub efield_sum: Option<f64>,
    pub efield_in_structure: Option<f64>,
    pub frac_of_efield: Option<f64>,
    pub amplitude: String,
}

/// One row of the contact distance table (`specs/14` §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ContactRow {
    pub subject: String,
    pub group: u32,
    pub group_label: String,
    pub side: char,
    pub contact: u32,
    pub structure: String,
    pub distance_mm: Option<f64>,
    pub inside_hull: Option<bool>,
    pub inside_voxel: Option<bool>,
    pub active: Option<bool>,
}

/// The measures summarised per group (`specs/14` §6.3).
pub const MEASURES: [&str; 3] = ["overlap_mm3", "frac_of_vta", "frac_of_structure"];

/// The side combinations of a summary (`specs/14` §6.3).
pub const SIDE_COMBINATIONS: [&str; 3] = ["both", "right", "left"];

/// One summary row: statistics of one measure over the patients of a group.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRow {
    pub group: u32,
    pub group_label: String,
    pub side: &'static str,
    pub structure: String,
    pub measure: &'static str,
    pub n: usize,
    pub mean: Option<f64>,
    pub sd: Option<f64>,
    pub median: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// An atlas structure file name without its NIfTI extension.
pub fn structure_name(file: &str) -> String {
    file.strip_suffix(".nii.gz")
        .or_else(|| file.strip_suffix(".nii"))
        .unwrap_or(file)
        .to_owned()
}

/// The `sub-<id>_desc-vtastats.json` of a label: the reference space folder first, then any other template space folder (the native folder never holds statistics).
pub fn vtastats_path(subject_dir: &Path, subject: &str, label: &str) -> Option<PathBuf> {
    let name = format!("sub-{subject}_desc-vtastats.json");
    let stim = subject_dir.join("stimulations");
    let preferred = stim.join(PREFERRED_SPACE).join(label).join(&name);
    if preferred.is_file() {
        return Some(preferred);
    }
    let mut spaces: Vec<PathBuf> = std::fs::read_dir(&stim)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir() && e.file_name() != "native")
        .map(|e| e.path())
        .collect();
    spaces.sort();
    spaces
        .into_iter()
        .map(|s| s.join(label).join(&name))
        .find(|p| p.is_file())
}

fn opt_numbers(v: Option<&Value>) -> Vec<Option<f64>> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(Value::as_f64).collect())
        .unwrap_or_default()
}

fn parse_side(v: &Value) -> SideVta {
    let efield = v.get("efield");
    SideVta {
        amplitudes: v
            .get("amplitudes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default(),
        volume_mm3: v.get("volume_mm3").and_then(Value::as_f64),
        atlas_volume: opt_numbers(v.get("atlas_volume_mm3")),
        intersection: opt_numbers(v.get("intersection_mm3")),
        n_intersection: opt_numbers(v.get("n_intersection")),
        n_within: opt_numbers(v.get("n_within_intersection")),
        efield_volume: efield.and_then(|e| e.get("volume")).and_then(Value::as_f64),
        efield_intersection: opt_numbers(efield.and_then(|e| e.get("intersection"))),
        efield_n_intersection: opt_numbers(efield.and_then(|e| e.get("n_intersection"))),
    }
}

/// Parses a `desc-vtastats.json` (`specs/07` §8.4): the structure names (skipped entries have an empty name) and the two sides. Side keys are `right` / `left` or `1` / `2`.
pub fn parse_vta_stats(json: &Value) -> Option<(String, Vec<String>, [Option<SideVta>; 2])> {
    if json.get("schema").and_then(Value::as_str) != Some("dbsloc-vtastats") {
        return None;
    }
    let label = json.get("label").and_then(Value::as_str)?.to_owned();
    let structures: Vec<String> = json
        .get("atlas")?
        .get("structures")?
        .as_array()?
        .iter()
        .map(|s| {
            let skipped = s.get("skipped").and_then(Value::as_bool).unwrap_or(false);
            match s.get("name").and_then(Value::as_str) {
                Some(n) if !skipped && !n.is_empty() => structure_name(n),
                _ => String::new(),
            }
        })
        .collect();
    let sides = json.get("sides")?;
    let pick = |names: [&str; 2]| names.iter().find_map(|n| sides.get(*n)).map(parse_side);
    Some((
        label,
        structures,
        [pick(["right", "1"]), pick(["left", "2"])],
    ))
}

/// Reads the VTA statistics of every included patient for the group label. A patient without a readable record is listed in `missing` (contact distances are still computed for it).
pub fn load_vta_stats(record: &GroupRecord) -> VtaLoad {
    let label = record.stim_label();
    let mut out = VtaLoad::default();
    for p in record.patients.iter().filter(|p| p.included) {
        let Some(dir) = record.patient_folder(p) else {
            out.missing.push(p.subject.clone());
            continue;
        };
        let Some(path) = vtastats_path(&dir, subject_id(&p.subject), &label) else {
            out.missing.push(p.subject.clone());
            continue;
        };
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
            .map(|v| parse_vta_stats(&v));
        match parsed {
            Ok(Some((l, structures, sides))) if l == label => out.subjects.push(SubjectVta {
                subject: p.subject.clone(),
                group: p.group,
                group_label: record.group_label_of(p.group),
                label: l,
                structures,
                sides,
            }),
            Ok(_) => {
                out.notes.push(Note::plain(format!(
                    "{}: not a statistics record of {label}; the patient is left out",
                    path.display()
                )));
                out.missing.push(p.subject.clone());
            }
            Err(e) => {
                out.notes
                    .push(Note::plain(format!("{}: {e}", path.display())));
                out.missing.push(p.subject.clone());
            }
        }
    }
    out
}

fn at(v: &[Option<f64>], i: usize) -> Option<f64> {
    v.get(i).copied().flatten()
}

fn amplitude_text(amps: &[f64]) -> String {
    amps.iter()
        .filter(|a| a.is_finite() && **a != 0.0)
        .map(|a| format!("{a}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// The cohort VTA table: one row per patient, side and structure (`specs/14` §6.1). E-field columns stay empty for a side without E-field values (`efield.volume` 0 or absent).
pub fn vta_rows(subjects: &[SubjectVta]) -> Vec<VtaRow> {
    let mut rows = Vec::new();
    for s in subjects {
        for side in Side::ALL {
            let Some(v) = &s.sides[side.index()] else {
                continue;
            };
            let has_efield = v.efield_volume.is_some_and(|e| e != 0.0);
            for (a, name) in s.structures.iter().enumerate() {
                if name.is_empty() {
                    continue;
                }
                rows.push(VtaRow {
                    subject: s.subject.clone(),
                    group: s.group,
                    group_label: s.group_label.clone(),
                    side: side.letter(),
                    source: 1,
                    structure: name.clone(),
                    vta_volume_mm3: v.volume_mm3,
                    overlap_mm3: at(&v.intersection, a),
                    frac_of_vta: at(&v.n_intersection, a),
                    frac_of_structure: at(&v.n_within, a),
                    structure_volume_mm3: at(&v.atlas_volume, a),
                    efield_sum: v.efield_volume.filter(|_| has_efield),
                    efield_in_structure: at(&v.efield_intersection, a).filter(|_| has_efield),
                    frac_of_efield: at(&v.efield_n_intersection, a).filter(|_| has_efield),
                    amplitude: amplitude_text(&v.amplitudes),
                });
            }
        }
    }
    rows
}

pub(super) fn contact_active(
    stim: Option<&Stimulation>,
    side: Side,
    contact: usize,
) -> Option<bool> {
    let stim = stim?;
    Some(stim.sides.get(side).sources.iter().any(|src| {
        src.is_active()
            && src
                .contacts
                .get(contact)
                .is_some_and(|c| c.percent > 0.0 && c.polarity != Polarity::Off)
    }))
}

/// The contact distance rows of one patient from its `ea_stats` (`specs/14` §6.2); electrode 1 is used. `names` are the atlas structure names of the statistics.
pub fn contact_rows_of(
    patient: &PatientEntry,
    group_label: &str,
    stats: &EaStats,
    stim: Option<&Stimulation>,
) -> Vec<ContactRow> {
    let mut rows = Vec::new();
    let Some(distances) = stats.conmat.first() else {
        return rows;
    };
    for side in Side::ALL {
        let i = side.index();
        let hull = stats.conmat_inside_hull.first().map(|c| &c[i]);
        let vox = stats.conmat_inside_vox.first().map(|c| &c[i]);
        for (c, row) in distances[i].iter().enumerate() {
            for (a, name) in stats.atlas_names.iter().enumerate() {
                let Some(d) = row.get(a).copied().filter(|d| d.is_finite()) else {
                    continue;
                };
                let zero = |m: Option<&Vec<Vec<f64>>>| {
                    m.and_then(|m| m.get(c))
                        .and_then(|r| r.get(a))
                        .map(|v| *v == 0.0)
                };
                rows.push(ContactRow {
                    subject: patient.subject.clone(),
                    group: patient.group,
                    group_label: group_label.to_owned(),
                    side: side.letter(),
                    contact: c as u32 + 1,
                    structure: structure_name(name),
                    distance_mm: Some(d),
                    inside_hull: zero(hull),
                    inside_voxel: zero(vox),
                    active: contact_active(stim, side, c),
                });
            }
        }
    }
    rows
}

/// The contact distance rows of every included patient that has a readable `ea_stats` with distances; patients without are listed in the notes.
pub fn load_contact_rows(record: &GroupRecord) -> (Vec<ContactRow>, Vec<Note>) {
    let label = record.stim_label();
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    for p in record.patients.iter().filter(|p| p.included) {
        let Some(dir) = record.patient_folder(p) else {
            continue;
        };
        let path = stats_path(&dir, &p.subject);
        let read = match read_ea_stats(&path) {
            Ok(r) => r,
            Err(e) => {
                notes.push(Note::plain(format!(
                    "{}: no contact distances ({e})",
                    p.subject
                )));
                continue;
            }
        };
        if read.stats.conmat.is_empty() {
            notes.push(Note::plain(format!(
                "{}: its statistics file has no contact distances",
                p.subject
            )));
            continue;
        }
        let stim = load_stimulation(&dir, subject_id(&p.subject), &label).ok();
        rows.extend(contact_rows_of(
            p,
            &record.group_label_of(p.group),
            &read.stats,
            stim.as_ref(),
        ));
    }
    (rows, notes)
}

/// The measures a clinical variable can be correlated with (`specs/14` §6.3).
pub const CORRELATION_MEASURES: [&str; 5] = [
    "overlap_mm3",
    "frac_of_vta",
    "frac_of_structure",
    "efield_in_structure",
    "frac_of_efield",
];

/// The value of a measure for structure `a` of one side; the E-field measures are `None` for a side without E-field values.
pub fn measure_value(v: &SideVta, a: usize, measure: &str) -> Option<f64> {
    let has_efield = v.efield_volume.is_some_and(|e| e != 0.0);
    match measure {
        "overlap_mm3" => at(&v.intersection, a),
        "frac_of_vta" => at(&v.n_intersection, a),
        "efield_in_structure" => at(&v.efield_intersection, a).filter(|_| has_efield),
        "frac_of_efield" => at(&v.efield_n_intersection, a).filter(|_| has_efield),
        _ => at(&v.n_within, a),
    }
}

/// Whether a side has a VTA of positive volume.
pub fn has_vta(side: &Option<SideVta>) -> bool {
    side.as_ref()
        .is_some_and(|v| v.volume_mm3.is_some_and(|x| x > 0.0))
}

/// Mean, sample SD (n - 1), median, minimum and maximum of finite values.
pub fn describe(values: &[f64]) -> [Option<f64>; 5] {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return [None; 5];
    }
    v.sort_by(f64::total_cmp);
    let n = v.len() as f64;
    let mean = v.iter().sum::<f64>() / n;
    let sd = (v.len() > 1)
        .then(|| (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt());
    let mid = v.len() / 2;
    let median = if v.len() % 2 == 1 {
        v[mid]
    } else {
        0.5 * (v[mid - 1] + v[mid])
    };
    [
        Some(mean),
        sd,
        Some(median),
        v.first().copied(),
        v.last().copied(),
    ]
}

/// Per group, side combination, structure and measure: the number of patients with a VTA and the statistics of the values (`specs/14` §6.3). `both` adds the two sides, a side without a VTA counting 0; `right` and `left` use the patients that have a VTA on that side.
pub fn summary_rows(subjects: &[SubjectVta]) -> Vec<SummaryRow> {
    let mut groups: Vec<(u32, String)> = Vec::new();
    for s in subjects {
        if !groups.iter().any(|(n, _)| *n == s.group) {
            groups.push((s.group, s.group_label.clone()));
        }
    }
    groups.sort_by_key(|(n, _)| *n);
    let mut structures: Vec<String> = Vec::new();
    for s in subjects {
        for n in &s.structures {
            if !n.is_empty() && !structures.contains(n) {
                structures.push(n.clone());
            }
        }
    }
    let mut out = Vec::new();
    for (number, label) in &groups {
        let members: Vec<&SubjectVta> = subjects.iter().filter(|s| s.group == *number).collect();
        for combination in SIDE_COMBINATIONS {
            for name in &structures {
                for measure in MEASURES {
                    let mut values: Vec<f64> = Vec::new();
                    let mut n = 0usize;
                    for s in &members {
                        let Some(a) = s.structures.iter().position(|x| x == name) else {
                            continue;
                        };
                        let r = s.sides[0].as_ref().filter(|_| has_vta(&s.sides[0]));
                        let l = s.sides[1].as_ref().filter(|_| has_vta(&s.sides[1]));
                        let value = match combination {
                            "right" => r.map(|v| measure_value(v, a, measure)),
                            "left" => l.map(|v| measure_value(v, a, measure)),
                            _ if r.is_none() && l.is_none() => None,
                            _ => {
                                let side = |v: Option<&SideVta>| {
                                    v.map_or(Some(0.0), |v| measure_value(v, a, measure))
                                };
                                Some(side(r).zip(side(l)).map(|(x, y)| x + y))
                            }
                        };
                        if let Some(value) = value {
                            n += 1;
                            if let Some(x) = value {
                                values.push(x);
                            }
                        }
                    }
                    let [mean, sd, median, min, max] = describe(&values);
                    out.push(SummaryRow {
                        group: *number,
                        group_label: label.clone(),
                        side: combination,
                        structure: name.clone(),
                        measure,
                        n,
                        mean,
                        sd,
                        median,
                        min,
                        max,
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "overlap_tests.rs"]
mod tests;
