// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The group 3D view manifest (`specs/14` §5.1 to §5.4): one version-2 scene manifest per group
//! view, written to `<group folder>/viz/scene_manifest.json`.

use std::path::{Path, PathBuf};

use dbsloc_reconstruct::electrode_by_name;
use dbsloc_reconstruct::native_frame::resolve_fiducials;
use serde_json::{json, Map, Value};

use super::calc::FlipTransformChoice;
use super::flip::{flip_points, flip_points_through};
use super::fliplr::{describe, FlipEnv, TemplateFiles};
use super::overlap::contact_active;
use super::record::{default_color, subject_id, GroupRecord, PatientEntry};
use super::stats_common::{van_albada, zscore};
use super::sweetspot::Normalisation;
use super::{ClinicalVar, GroupError, Note};
use crate::atlas_catalog::{atlas_lead_sets_json, bundled_and_external_atlas_sets};
use crate::outputs::write_replacing;
use crate::paths::absolute;
use crate::reg_backend::RegBackend;
use crate::template_catalog::REFERENCE_SPACE;
use crate::vat::settings::{label_dir, load as load_stimulation, Side, Stimulation};

/// `manifest_version` of a group scene.
pub const MANIFEST_VERSION: u32 = 2;
/// Suffix of the name of a mirrored copy of a patient (`specs/14` §5.4).
pub const MIRRORED_SUFFIX: &str = "_mirrored";
/// Colour map name of the manifest.
pub const COLORMAP: &str = "blue_white_red";

/// What the contacts and leads are coloured by.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ColourBy {
    #[default]
    Group,
    Clinical(String),
}

/// The display toggles of `group.show`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowFlags {
    pub active_contacts: bool,
    pub passive_contacts: bool,
    pub highlight_active: bool,
    pub vtas: bool,
    pub point_cloud: bool,
    pub isovolume: bool,
}

impl ShowFlags {
    /// The flags the group record stores, with VTAs shown and the point cloud on when a clinical variable colours.
    pub fn from_record(record: &GroupRecord, clinical: bool) -> ShowFlags {
        ShowFlags {
            active_contacts: record.view.show_active,
            passive_contacts: record.view.show_passive,
            highlight_active: record.view.highlight_active,
            vtas: true,
            point_cloud: clinical,
            isovolume: record.view.show_isovolume,
        }
    }

    fn to_json(self) -> Value {
        json!({
            "active_contacts": self.active_contacts,
            "passive_contacts": self.passive_contacts,
            "highlight_active": self.highlight_active,
            "vtas": self.vtas,
            "point_cloud": self.point_cloud,
            "isovolume": self.isovolume,
        })
    }
}

/// Options of [`write_group_view`].
#[derive(Debug, Clone)]
pub struct ViewOptions {
    pub colour_by: ColourBy,
    pub normalisation: Normalisation,
    pub mirror: bool,
    pub show: Option<ShowFlags>,
    /// Template catalogue id, space or T1w file of the group space; default the reference space.
    pub template: Option<String>,
    pub leaddbs_atlases_dir: Option<PathBuf>,
    pub flip_transform: FlipTransformChoice,
    pub env: FlipEnv,
    /// Patients to show (ids with or without `sub-`); `None` = every included patient.
    pub subjects: Option<Vec<String>>,
}

impl ViewOptions {
    /// Group colours, no mirroring, the cached flip transform of the process environment.
    pub fn new() -> ViewOptions {
        ViewOptions {
            colour_by: ColourBy::Group,
            normalisation: Normalisation::None,
            mirror: false,
            show: None,
            template: None,
            leaddbs_atlases_dir: None,
            flip_transform: FlipTransformChoice::Auto,
            env: FlipEnv::from_environment(RegBackend::Auto, None),
            subjects: None,
        }
    }
}

impl Default for ViewOptions {
    fn default() -> Self {
        ViewOptions::new()
    }
}

/// What was written.
#[derive(Debug, Clone)]
pub struct ViewReport {
    pub manifest: PathBuf,
    pub notes: Vec<Note>,
    pub patients: usize,
    pub leads: usize,
    pub skipped: Vec<(String, String)>,
    pub range: Option<[f64; 2]>,
    pub space: String,
}

impl ViewReport {
    /// The JSON summary of the view.
    pub fn to_json(&self) -> Value {
        json!({
            "manifest": self.manifest,
            "space": self.space,
            "patients": self.patients,
            "leads": self.leads,
            "range": self.range,
            "skipped": self.skipped.iter().map(|(s, why)| json!({"subject": s, "reason": why})).collect::<Vec<_>>(),
            "warnings": self.notes.iter().map(|n| json!(n.to_string())).collect::<Vec<_>>(),
        })
    }
}

/// Whether a value array has one entry per contact or per contact pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Contact,
    Pair,
}

impl ValueKind {
    fn name(self) -> &'static str {
        match self {
            ValueKind::Contact => "contact",
            ValueKind::Pair => "pair",
        }
    }
}

/// The values of one patient: per side (right, left) one entry per contact or contact pair; NaN is missing.
#[derive(Debug, Clone, PartialEq)]
pub struct PatientValues {
    pub kind: ValueKind,
    pub sides: [Vec<f64>; 2],
}

fn isomatrix_error(message: String) -> GroupError {
    GroupError::Lg {
        code: "E_GROUP_ISOMATRIX",
        message,
    }
}

/// Reshapes a clinical variable into per-contact values per side (`specs/14` §5.3). `counts[i]` is the contact count of patient `i` on the right and left side (0 = no lead); row `i` of the variable belongs to patient `i`. Normalisation is applied per side over all finite values.
pub fn contact_values(
    var: &ClinicalVar,
    counts: &[[usize; 2]],
    normalisation: Normalisation,
) -> Result<Vec<PatientValues>, GroupError> {
    if var.values.len() != counts.len() {
        return Err(isomatrix_error(format!(
            "{}: {} rows for {} patients",
            var.name,
            var.values.len(),
            counts.len()
        )));
    }
    let w = var.width();
    let kmax = counts.iter().flatten().copied().max().unwrap_or(0);
    #[derive(Clone, Copy, PartialEq)]
    enum Shape {
        One,
        Hemisphere,
        Pairs,
        Contacts,
    }
    let shape = if w == 1 {
        Shape::One
    } else if w == 2 {
        Shape::Hemisphere
    } else if kmax >= 2 && w == 2 * (kmax - 1) {
        Shape::Pairs
    } else if kmax >= 1 && w == 2 * kmax {
        Shape::Contacts
    } else {
        return Err(isomatrix_error(format!(
            "{} has {w} columns; expected 1, 2, {} (contact pairs) or {} (contacts) for contact count {kmax}",
            var.name,
            2 * kmax.saturating_sub(1),
            2 * kmax
        )));
    };
    let kind = if shape == Shape::Pairs {
        ValueKind::Pair
    } else {
        ValueKind::Contact
    };
    let cell = |row: usize, col: usize| {
        var.values
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .flatten()
            .filter(|v| v.is_finite())
            .unwrap_or(f64::NAN)
    };
    let length = |count: usize| match kind {
        ValueKind::Contact => count,
        ValueKind::Pair => count.saturating_sub(1),
    };
    let mut raw: Vec<[Vec<f64>; 2]> = Vec::with_capacity(counts.len());
    for (i, c) in counts.iter().enumerate() {
        let mut sides = [Vec::new(), Vec::new()];
        for (s, side) in sides.iter_mut().enumerate() {
            let n = length(c[s]);
            *side = (0..n)
                .map(|k| match shape {
                    Shape::One => cell(i, 0),
                    Shape::Hemisphere => cell(i, s),
                    Shape::Pairs => cell(i, s * (kmax - 1) + k),
                    Shape::Contacts => cell(i, s * kmax + k),
                })
                .collect();
        }
        raw.push(sides);
    }
    if normalisation != Normalisation::None {
        for s in 0..2 {
            let stacked: Vec<f64> = raw.iter().flat_map(|p| p[s].iter().copied()).collect();
            let normed = match normalisation {
                Normalisation::ZScore => zscore(&stacked),
                _ => van_albada(&stacked),
            };
            let mut at = 0;
            for p in &mut raw {
                let n = p[s].len();
                p[s].copy_from_slice(&normed[at..at + n]);
                at += n;
            }
        }
    }
    Ok(raw
        .into_iter()
        .map(|sides| PatientValues { kind, sides })
        .collect())
}

/// The contact values of the mirrored copy of a patient: the sides swap (`specs/14` §5.4).
pub fn mirror_values(values: &PatientValues) -> PatientValues {
    PatientValues {
        kind: values.kind,
        sides: [values.sides[1].clone(), values.sides[0].clone()],
    }
}

fn norm_text(n: Normalisation) -> &'static str {
    match n {
        Normalisation::None => "none",
        Normalisation::ZScore => "zscore",
        Normalisation::VanAlbada => "van_albada",
    }
}

/// Parses `none`, `zscore` / `z-score`, `van_albada` / `van-albada`.
pub fn parse_normalisation(text: &str) -> Option<Normalisation> {
    let key: String = text
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    match key.as_str() {
        "none" => Some(Normalisation::None),
        "zscore" => Some(Normalisation::ZScore),
        "vanalbada" => Some(Normalisation::VanAlbada),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Lead {
    head: [f64; 3],
    tail: [f64; 3],
    x: Option<[f64; 3]>,
    y: Option<[f64; 3]>,
    contacts: Vec<[f64; 3]>,
    trajectory: Vec<[f64; 3]>,
}

fn point(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array()?;
    (a.len() == 3)
        .then(|| [a[0].as_f64(), a[1].as_f64(), a[2].as_f64()])
        .and_then(|[x, y, z]| Some([x?, y?, z?]))
}

fn points(v: Option<&Value>) -> Vec<[f64; 3]> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(point).collect())
        .unwrap_or_default()
}

fn lead_of(side: &Value) -> Option<Lead> {
    let m = side.get("markers")?;
    Some(Lead {
        head: point(m.get("head")?)?,
        tail: point(m.get("tail")?)?,
        x: m.get("x").and_then(point),
        y: m.get("y").and_then(point),
        contacts: points(side.get("coords_mm")),
        trajectory: points(side.get("trajectory")),
    })
}

fn mean_x(lead: &Lead) -> f64 {
    0.5 * (lead.head[0] + lead.tail[0])
}

fn json_points(p: &[[f64; 3]]) -> Value {
    Value::Array(p.iter().map(|q| json!([q[0], q[1], q[2]])).collect())
}

fn lead_json(lead: &Lead) -> Value {
    let mut markers = json!({"head": lead.head, "tail": lead.tail});
    if let Some(x) = lead.x {
        markers["x"] = json!(x);
    }
    if let Some(y) = lead.y {
        markers["y"] = json!(y);
    }
    json!({
        "markers": markers,
        "coords_mm": json_points(&lead.contacts),
        "trajectory": json_points(&lead.trajectory),
    })
}

fn side_leads(block: &Value, declared: bool) -> [Option<Lead>; 2] {
    let obj = block.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k)).and_then(lead_of);
    if declared {
        return [get("1"), get("2")];
    }
    let mut leads: Vec<Lead> = ["1", "2"].iter().filter_map(|k| get(k)).collect();
    match leads.len() {
        1 => {
            let l = leads.remove(0);
            if mean_x(&l) > 0.0 {
                [Some(l), None]
            } else {
                [None, Some(l)]
            }
        }
        2 => {
            if mean_x(&leads[0]) >= mean_x(&leads[1]) {
                let l = leads.remove(1);
                [Some(leads.remove(0)), Some(l)]
            } else {
                let r = leads.remove(1);
                [Some(r), Some(leads.remove(0))]
            }
        }
        _ => [None, None],
    }
}

struct Reco {
    path: PathBuf,
    leads: [Option<Lead>; 2],
    block: &'static str,
    model: Option<String>,
    tail_warnings: Vec<String>,
}

fn read_reco(path: &Path, group_space: &str) -> Result<Reco, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("no reconstruction ({e})"))?;
    let mut root: Value = serde_json::from_str(&text)
        .map_err(|e| format!("reconstruction {}: {e}", path.display()))?;
    let tail_warnings = dbsloc_reconstruct::tail_fiducial::convert_legacy_tail(&mut root, None)
        .into_iter()
        .map(|c| format!("{}: {}", path.display(), c.warning))
        .collect();
    let declared = root
        .pointer("/meta/side_keys")
        .and_then(Value::as_str)
        .is_some_and(|s| s == "1=right,2=left");
    let own_space = root
        .pointer("/meta/template_space")
        .and_then(Value::as_str)
        .unwrap_or(REFERENCE_SPACE);
    let block = if own_space == group_space {
        "mni"
    } else if group_space == REFERENCE_SPACE && root.get("reference").is_some_and(Value::is_object)
    {
        "reference"
    } else {
        return Err(format!(
            "W_GROUP_SPACE: the reconstruction is in {own_space}, the group space is {group_space} and the file has no reference block"
        ));
    };
    let leads = side_leads(root.get(block).unwrap_or(&Value::Null), declared);
    if leads.iter().all(Option::is_none) {
        return Err("the reconstruction has no lead with markers".to_owned());
    }
    let model = ["1", "2"]
        .iter()
        .find_map(|k| root.pointer(&format!("/props/{k}/elmodel")))
        .or_else(|| root.pointer("/meta/elmodel"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(Reco {
        path: path.to_path_buf(),
        leads,
        block,
        model,
        tail_warnings,
    })
}

fn reco_file(folder: &Path, subject: &str) -> PathBuf {
    folder
        .join("reconstruction")
        .join(format!("{subject}_desc-reconstruction.json"))
}

fn map_points(
    points: &[[f64; 3]],
    transform: Option<&Path>,
    notes: &mut Vec<Note>,
    warned: &mut bool,
) -> Vec<[f64; 3]> {
    if let Some(t) = transform {
        match flip_points_through(points, t) {
            Ok(p) => return p,
            Err(e) => {
                if !*warned {
                    notes.push(Note::plain(format!(
                        "the flip transform could not be applied ({e}); mirrored leads use the plain x flip"
                    )));
                    *warned = true;
                }
            }
        }
    }
    flip_points(points)
}

fn mirror_lead(
    lead: &Lead,
    model: Option<&str>,
    transform: Option<&Path>,
    notes: &mut Vec<Note>,
    warned: &mut bool,
) -> Lead {
    let electrode = model.and_then(electrode_by_name);
    if let (Some(x), Some(y), Some(el)) = (lead.x, lead.y, electrode) {
        let mapped = map_points(&[lead.head, lead.tail, x, y], transform, notes, warned);
        if let Ok(side) = resolve_fiducials([mapped[0], mapped[1], mapped[2], mapped[3]], el) {
            return Lead {
                head: side.head,
                tail: side.tail,
                x: Some(side.x),
                y: Some(side.y),
                contacts: side.coords_mm,
                trajectory: side.trajectory,
            };
        }
        notes.push(Note::plain(
            "the flipped markers do not resolve against the electrode catalogue; contacts are flipped directly".to_owned(),
        ));
    }
    let mut all = vec![lead.head, lead.tail];
    all.extend(lead.x);
    all.extend(lead.y);
    let n_fid = all.len();
    all.extend(&lead.contacts);
    all.extend(&lead.trajectory);
    let m = map_points(&all, transform, notes, warned);
    let mut at = 2;
    let take_opt = |present: bool, at: &mut usize| {
        present.then(|| {
            let p = m[*at];
            *at += 1;
            p
        })
    };
    let x = take_opt(lead.x.is_some(), &mut at);
    let y = take_opt(lead.y.is_some(), &mut at);
    Lead {
        head: m[0],
        tail: m[1],
        x,
        y,
        contacts: m[n_fid..n_fid + lead.contacts.len()].to_vec(),
        trajectory: m[n_fid + lead.contacts.len()..].to_vec(),
    }
}

fn mirrored_reco_path(viz: &Path, subject: &str) -> PathBuf {
    viz.join("mirrored").join(format!(
        "{subject}{MIRRORED_SUFFIX}_desc-reconstruction.json"
    ))
}

fn write_mirrored_reco(
    path: &Path,
    subject: &str,
    leads: &[Option<Lead>; 2],
    model: Option<&str>,
) -> Result<(), GroupError> {
    let mut mni = Map::new();
    for (k, lead) in leads.iter().enumerate() {
        if let Some(l) = lead {
            mni.insert((k + 1).to_string(), lead_json(l));
        }
    }
    let mut meta = json!({"side_keys": "1=right,2=left", "template_space": "group"});
    if let Some(m) = model {
        meta["elmodel"] = json!(m);
    }
    let mut doc = json!({
        "version": 2,
        "subj_id": format!("{subject}{MIRRORED_SUFFIX}"),
        "method": "group_mirror",
        "mni": Value::Object(mni),
        "meta": meta,
    });
    dbsloc_reconstruct::tail_fiducial::mark_tail_fiducial(&mut doc);
    let text = serde_json::to_string_pretty(&doc).map_err(|e| GroupError::Json(e.to_string()))?;
    write_replacing(path, text.as_bytes())
        .map_err(|e| GroupError::Io(format!("cannot write {}: {e}", path.display())))
}

type VtaFiles = ([Option<PathBuf>; 2], [Option<PathBuf>; 2], Vec<PathBuf>);

fn vta_files(dir: &Path, subject_id: &str) -> VtaFiles {
    let mut original = [None, None];
    let mut flipped = [None, None];
    let mut stale = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (original, flipped, stale);
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let prefix = format!("sub-{subject_id}_sim-binary_model-");
    for name in names {
        if !name.starts_with(&prefix) {
            continue;
        }
        for side in Side::ALL {
            let i = side.index();
            let l = side.letter();
            if name.ends_with(&format!("_hemi-{l}.nii")) && original[i].is_none() {
                original[i] = Some(dir.join(&name));
            }
            if name.ends_with(&format!(
                "_hemi-{l}_hemidesc-FlippedFrom{}.nii",
                other_side(side)
            )) && flipped[i].is_none()
            {
                flipped[i] = Some(dir.join(&name));
            }
        }
    }
    for i in 0..2 {
        let current = match (&original[1 - i], &flipped[i]) {
            (Some(o), Some(f)) => super::flip::flipped_is_current(o, f),
            _ => false,
        };
        if !current {
            stale.extend(flipped[i].take());
        }
    }
    (original, flipped, stale)
}

fn other_side(side: Side) -> &'static str {
    match side {
        Side::Right => "Left",
        Side::Left => "Right",
    }
}

fn path_json(p: &Option<PathBuf>) -> Value {
    p.as_ref()
        .map_or(Value::Null, |p| json!(absolute(p).to_string_lossy()))
}

fn sides_json<T>(pair: &[T; 2], f: impl Fn(&T) -> Value) -> Value {
    json!({"1": f(&pair[0]), "2": f(&pair[1])})
}

fn num_or_null(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else {
        Value::Null
    }
}

fn values_json(v: &PatientValues) -> Value {
    json!({
        "1": v.sides[0].iter().copied().map(num_or_null).collect::<Vec<_>>(),
        "2": v.sides[1].iter().copied().map(num_or_null).collect::<Vec<_>>(),
    })
}

fn active_of(stim: Option<&Stimulation>, counts: [usize; 2]) -> [Vec<bool>; 2] {
    let mut out = [Vec::new(), Vec::new()];
    for side in Side::ALL {
        let i = side.index();
        out[i] = (0..counts[i])
            .map(|c| contact_active(stim, side, c).unwrap_or(false))
            .collect();
    }
    out
}

struct Shown<'a> {
    index: usize,
    patient: &'a PatientEntry,
    reco: Reco,
    stim: Option<Stimulation>,
    counts: [usize; 2],
    original_vta: [Option<PathBuf>; 2],
    flipped_vta: [Option<PathBuf>; 2],
    stale_flipped: Vec<PathBuf>,
}

/// Builds the version-2 scene manifest of a group view and writes it to `<folder>/viz/scene_manifest.json` (`specs/14` §5.1). Patients without a usable reconstruction are skipped and listed in the report.
pub fn write_group_view(
    record: &GroupRecord,
    folder: &Path,
    options: &ViewOptions,
) -> Result<ViewReport, GroupError> {
    let mut notes: Vec<Note> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let spec = options
        .template
        .clone()
        .unwrap_or_else(|| REFERENCE_SPACE.to_owned());
    let files = TemplateFiles::resolve(
        &spec,
        options.env.shipped_root.as_deref(),
        options.env.user_root.as_deref(),
    )
    .map_err(GroupError::Invalid)?;
    let space = files.space.clone();
    let wanted: Option<Vec<String>> = options
        .subjects
        .as_ref()
        .map(|v| v.iter().map(|s| super::record::subject_name(s)).collect());
    let label = record.stim_label();

    let mut shown: Vec<Shown<'_>> = Vec::new();
    for (index, patient) in record.patients.iter().enumerate() {
        if !patient.included {
            continue;
        }
        if let Some(w) = &wanted {
            if !w.contains(&super::record::subject_name(&patient.subject)) {
                continue;
            }
        }
        let Some(dir) = record.patient_folder(patient) else {
            skipped.push((
                patient.subject.clone(),
                "its dataset is not in the group".to_owned(),
            ));
            continue;
        };
        let reco = match read_reco(&reco_file(&dir, &patient.subject), &space) {
            Ok(r) => r,
            Err(why) => {
                skipped.push((patient.subject.clone(), why));
                continue;
            }
        };
        notes.extend(reco.tail_warnings.iter().cloned().map(Note::plain));
        let sid = subject_id(&patient.subject);
        let stim = load_stimulation(&dir, sid, &label).ok();
        let counts = counts_of(&reco, patient, &mut notes);
        let (original_vta, flipped_vta, stale_flipped) =
            vta_files(&label_dir(&dir, &space, &label), sid);
        shown.push(Shown {
            index,
            patient,
            reco,
            stim,
            counts,
            original_vta,
            flipped_vta,
            stale_flipped,
        });
    }
    for (s, why) in &skipped {
        if why.starts_with("W_GROUP_SPACE") {
            notes.push(Note::coded("W_GROUP_SPACE", format!("{s}: {why}")));
        }
    }
    let kmax_counts: Vec<[usize; 2]> = record
        .patients
        .iter()
        .enumerate()
        .map(|(i, _)| {
            shown
                .iter()
                .find(|s| s.index == i)
                .map_or([0, 0], |s| s.counts)
        })
        .collect();

    let (colour_name, values): (Option<String>, Option<Vec<PatientValues>>) =
        match &options.colour_by {
            ColourBy::Group => (None, None),
            ColourBy::Clinical(name) => {
                let var = record
                    .clinical
                    .iter()
                    .find(|v| &v.name == name)
                    .ok_or_else(|| {
                        GroupError::Invalid(format!("the group has no clinical variable {name:?}"))
                    })?;
                (
                    Some(name.clone()),
                    Some(contact_values(var, &kmax_counts, options.normalisation)?),
                )
            }
        };

    let transform: Option<PathBuf> = if options.mirror {
        match &options.flip_transform {
            FlipTransformChoice::Path(p) => Some(p.clone()),
            FlipTransformChoice::PlainFlip => {
                notes.push(Note::plain(
                    "mirroring with the plain x flip only: the non-linear part of the flip transform is not applied",
                ));
                None
            }
            FlipTransformChoice::Auto => {
                let found = describe(&files, &options.env).found.map(|f| f.forward);
                if found.is_none() {
                    notes.push(Note::plain(format!(
                        "no flip transform of {space} is cached; mirrored leads use the plain x flip (run `dbsloc group fliplr build`)"
                    )));
                }
                found
            }
        }
    } else {
        None
    };

    let viz = folder.join("viz");
    let mut subjects: Vec<Value> = Vec::new();
    let mut warned = false;
    let mut all_values: Vec<f64> = Vec::new();
    let mut missing_flipped: Vec<String> = Vec::new();
    let group_label = |n: u32| record.group_label_of(n);
    for s in &shown {
        let p = s.patient;
        let model = p.elmodel_override.clone().or_else(|| s.reco.model.clone());
        let active = active_of(s.stim.as_ref(), s.counts);
        let value = values.as_ref().map(|v| &v[s.index]);
        if let Some(v) = value {
            all_values.extend(v.sides.iter().flatten().copied().filter(|x| x.is_finite()));
        }
        let mut entry = json!({
            "subject": p.subject,
            "label": group_label(p.group),
            "group": p.group,
            "reco_json": absolute(&s.reco.path).to_string_lossy(),
            "block": s.reco.block,
            "elmodel": model,
            "active_contacts": sides_json(&active, |a| json!(a)),
            "vta": sides_json(&s.original_vta, path_json),
            "vta_mesh": {},
            "flipped": sides_json(&s.flipped_vta, path_json),
            "mirrored": false,
        });
        if let Some(v) = value {
            entry["value"] = values_json(v);
            entry["value_kind"] = json!(v.kind.name());
        }
        subjects.push(entry);
        if options.mirror {
            let mut leads = [None, None];
            for (k, slot) in leads.iter_mut().enumerate() {
                if let Some(l) = &s.reco.leads[1 - k] {
                    *slot = Some(mirror_lead(
                        l,
                        model.as_deref(),
                        transform.as_deref(),
                        &mut notes,
                        &mut warned,
                    ));
                }
            }
            let copy_name = format!("{}{MIRRORED_SUFFIX}", p.subject);
            let path = mirrored_reco_path(&viz, &p.subject);
            write_mirrored_reco(&path, &p.subject, &leads, model.as_deref())?;
            let mirrored_active = [active[1].clone(), active[0].clone()];
            if s.flipped_vta.iter().all(Option::is_none)
                && s.original_vta.iter().any(Option::is_some)
            {
                missing_flipped.push(p.subject.clone());
            }
            for stale in &s.stale_flipped {
                notes.push(Note::plain(format!(
                    "{}: the flipped VTA {} is out of date and is not drawn; run Calculate with \
                     mirroring again",
                    p.subject,
                    stale.display()
                )));
            }
            let mut copy = json!({
                "subject": copy_name,
                "label": group_label(p.group),
                "group": p.group,
                "reco_json": absolute(&path).to_string_lossy(),
                "block": "mni",
                "elmodel": model,
                "active_contacts": sides_json(&mirrored_active, |a| json!(a)),
                "vta": sides_json(&s.flipped_vta, path_json),
                "vta_mesh": {},
                "flipped": sides_json(&s.original_vta, path_json),
                "mirrored": true,
                "source_subject": p.subject,
            });
            if let Some(v) = value {
                let m = mirror_values(v);
                all_values.extend(m.sides.iter().flatten().copied().filter(|x| x.is_finite()));
                copy["value"] = values_json(&m);
                copy["value_kind"] = json!(m.kind.name());
            }
            subjects.push(copy);
        }
    }
    if shown.is_empty() {
        return Err(GroupError::Invalid(
            "no patient of the group has a usable reconstruction in the group space".to_owned(),
        ));
    }
    if !missing_flipped.is_empty() {
        notes.push(Note::plain(format!(
            "no flipped VTA files, or only out-of-date ones, for {}; run Calculate with mirroring again to draw the mirrored VTAs",
            missing_flipped.join(", ")
        )));
    }

    let range = if all_values.is_empty() {
        None
    } else {
        let lo = all_values.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = all_values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Some([lo, hi])
    };
    let colour_by = match &colour_name {
        None => json!("group"),
        Some(name) => json!({
            "clinical": name,
            "normalisation": norm_text(options.normalisation),
            "colormap": COLORMAP,
            "range": range,
        }),
    };
    let show = options
        .show
        .unwrap_or_else(|| ShowFlags::from_record(record, colour_name.is_some()));
    let groups: Vec<Value> = record
        .groups
        .iter()
        .map(|g| {
            json!({
                "number": g.number,
                "label": g.label,
                "color": g.color.unwrap_or_else(|| default_color(g.number)),
            })
        })
        .collect();

    let mut man = Map::new();
    man.insert("manifest_version".into(), json!(MANIFEST_VERSION));
    man.insert("subj_id".into(), json!(format!("group-{}", record.guid)));
    man.insert("space".into(), json!(space));
    man.insert(
        "template_nii".into(),
        json!(absolute(&files.t1).to_string_lossy()),
    );
    man.insert("backdrops".into(), json!([]));
    add_atlas(&mut man, record, options, &space, &mut notes);
    man.insert(
        "group".into(),
        json!({
            "guid": record.guid,
            "title": record.title,
            "subjects": subjects,
            "groups": groups,
            "colour_by": colour_by,
            "mirror": options.mirror,
            "show": show.to_json(),
        }),
    );
    let manifest = viz.join("scene_manifest.json");
    let text = serde_json::to_string_pretty(&Value::Object(man))
        .map_err(|e| GroupError::Json(e.to_string()))?;
    write_replacing(&manifest, text.as_bytes())
        .map_err(|e| GroupError::Io(format!("cannot write {}: {e}", manifest.display())))?;
    let leads = shown
        .iter()
        .map(|s| s.counts.iter().filter(|c| **c > 0).count())
        .sum::<usize>()
        * if options.mirror { 2 } else { 1 };
    Ok(ViewReport {
        manifest,
        notes,
        patients: shown.len(),
        leads,
        skipped,
        range,
        space,
    })
}

fn counts_of(reco: &Reco, patient: &PatientEntry, notes: &mut Vec<Note>) -> [usize; 2] {
    let catalogue = patient
        .elmodel_override
        .as_deref()
        .or(reco.model.as_deref())
        .and_then(electrode_by_name)
        .map(|e| e.n_contacts);
    let mut counts = [0, 0];
    for (k, lead) in reco.leads.iter().enumerate() {
        if let Some(l) = lead {
            counts[k] = if l.contacts.is_empty() {
                catalogue.unwrap_or(0)
            } else {
                l.contacts.len()
            };
        }
    }
    if counts.contains(&0) && counts.iter().any(|c| *c > 0) {
        notes.push(Note::plain(format!(
            "{}: a unilateral lead",
            patient.subject
        )));
    }
    counts
}

fn add_atlas(
    man: &mut Map<String, Value>,
    record: &GroupRecord,
    options: &ViewOptions,
    space: &str,
    notes: &mut Vec<Note>,
) {
    if space != REFERENCE_SPACE {
        notes.push(Note::plain(format!(
            "no atlas is drawn in {space}; atlas sets are available in {REFERENCE_SPACE} only"
        )));
        return;
    }
    let mut logs = Vec::new();
    let sets = bundled_and_external_atlas_sets(options.leaddbs_atlases_dir.as_deref(), &mut |m| {
        logs.push(m);
    });
    for m in logs {
        notes.push(Note::plain(m));
    }
    let sets: Vec<_> = sets.into_iter().filter(|s| s.dir.is_dir()).collect();
    if sets.is_empty() {
        return;
    }
    let chosen = record
        .atlas_set
        .as_deref()
        .and_then(|id| sets.iter().find(|s| s.id == id || s.name == id))
        .or_else(|| crate::atlas_catalog::default_atlas(&sets));
    if let Some(d) = chosen {
        man.insert("atlas_default_id".into(), json!(d.id));
    }
    let placed: Vec<_> = sets
        .into_iter()
        .map(|mut s| {
            s.dir = absolute(&s.dir);
            s
        })
        .collect();
    man.insert(
        "atlas_lead_sets".into(),
        json!(atlas_lead_sets_json(&placed)),
    );
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
