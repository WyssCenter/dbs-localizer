// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Stimulation settings of `specs/07` §2: canonical JSON, validation and the Lead-DBS `S` mapping.

use std::fmt;
use std::path::{Path, PathBuf};

use dbsloc_reconstruct::electrode_by_name;
use fastwarp_matfile::mat5::{mat5_bytes, Mat5Value};
use fastwarp_matfile::{read_any_with, MatError, ReadOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bids::Subject;
use crate::outputs::write_replacing;
use crate::sha256::sha256_hex;
use crate::tutor::side_hemispheres;
use crate::version::{software_stamp, utc_now_rfc3339, version_string, GIT_HASH, PRODUCT};

pub const SCHEMA: &str = "dbsloc-stimulation";
pub const SCHEMA_VERSION: u32 = 1;
pub const LEAD_DBS_VER: &str = "2.0";
pub const SOURCES_PER_SIDE: u32 = 4;
pub const PROVENANCE_VARIABLE: &str = "dbsloc_provenance";
pub const DEFAULT_GM_MASK: &str = "atlas:DISTAL Minimal (Ewert 2017)";
const MAX_DEPTH: usize = 64;

const S_KNOWN_FIELDS: [&str; 10] = [
    "label",
    "active",
    "model",
    "monopolarmodel",
    "amplitude",
    "activecontacts",
    "numContacts",
    "sources",
    "volume",
    "ver",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Right,
    Left,
}

impl Side {
    pub const ALL: [Side; 2] = [Side::Right, Side::Left];

    pub fn index(self) -> usize {
        match self {
            Side::Right => 0,
            Side::Left => 1,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Side::Right => "right",
            Side::Left => "left",
        }
    }

    pub fn letter(self) -> char {
        match self {
            Side::Right => 'R',
            Side::Left => 'L',
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Polarity {
    Off,
    Cathode,
    Anode,
}

impl Polarity {
    pub fn code(self) -> f64 {
        match self {
            Polarity::Off => 0.0,
            Polarity::Cathode => 1.0,
            Polarity::Anode => 2.0,
        }
    }

    pub fn from_code(v: f64) -> Option<Polarity> {
        if v == 0.0 {
            Some(Polarity::Off)
        } else if v == 1.0 {
            Some(Polarity::Cathode)
        } else if v == 2.0 {
            Some(Polarity::Anode)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Voltage,
    Current,
}

impl Mode {
    pub fn code(self) -> f64 {
        match self {
            Mode::Voltage => 1.0,
            Mode::Current => 2.0,
        }
    }

    pub fn from_code(v: f64) -> Option<Mode> {
        if v == 1.0 {
            Some(Mode::Voltage)
        } else if v == 2.0 {
            Some(Mode::Current)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Efield,
    Kuncel,
    Maedler,
    Dembek,
    Fastfield,
    Ossdbs,
}

impl ModelKind {
    pub const ALL: [ModelKind; 6] = [
        ModelKind::Efield,
        ModelKind::Kuncel,
        ModelKind::Maedler,
        ModelKind::Dembek,
        ModelKind::Fastfield,
        ModelKind::Ossdbs,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            ModelKind::Efield => "SimBio/FieldTrip (see Horn 2017)",
            ModelKind::Kuncel => "Kuncel 2008",
            ModelKind::Maedler => "Maedler 2012",
            ModelKind::Dembek => "Dembek 2017",
            ModelKind::Fastfield => "Fastfield (Baniasadi 2020)",
            ModelKind::Ossdbs => "OSS-DBS (Butenko 2020)",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            ModelKind::Efield => "simbio",
            ModelKind::Kuncel => "kuncel",
            ModelKind::Maedler => "maedler",
            ModelKind::Dembek => "dembek",
            ModelKind::Fastfield => "fastfield",
            ModelKind::Ossdbs => "ossdbs",
        }
    }

    pub fn from_display_name(name: &str) -> Option<ModelKind> {
        ModelKind::ALL
            .into_iter()
            .find(|m| m.display_name() == name.trim())
    }

    pub fn is_analytical(self) -> bool {
        matches!(
            self,
            ModelKind::Kuncel | ModelKind::Maedler | ModelKind::Dembek
        )
    }
}

fn one() -> f64 {
    1.0
}

fn off() -> Polarity {
    Polarity::Off
}

fn current() -> Mode {
    Mode::Current
}

fn sixty() -> f64 {
    60.0
}

fn first() -> u32 {
    1
}

fn schema_name() -> String {
    SCHEMA.to_owned()
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
}

fn lead_ver() -> String {
    LEAD_DBS_VER.to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub index: u32,
    #[serde(default = "off")]
    pub polarity: Polarity,
    #[serde(default)]
    pub percent: f64,
    #[serde(default = "one")]
    pub impedance_kohm: f64,
}

impl Contact {
    pub fn unused(index: u32) -> Contact {
        Contact {
            index,
            polarity: Polarity::Off,
            percent: 0.0,
            impedance_kohm: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseSpec {
    pub polarity: Polarity,
    pub percent: f64,
}

impl Default for CaseSpec {
    fn default() -> Self {
        CaseSpec {
            polarity: Polarity::Anode,
            percent: 100.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub index: u32,
    #[serde(default = "current")]
    pub mode: Mode,
    #[serde(default)]
    pub amplitude: f64,
    #[serde(default = "sixty")]
    pub pulse_width_us: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_hz: Option<f64>,
    #[serde(default)]
    pub case: CaseSpec,
    #[serde(default)]
    pub contacts: Vec<Contact>,
}

impl Source {
    pub fn blank(index: u32, contacts: usize) -> Source {
        Source {
            index,
            mode: Mode::Current,
            amplitude: 0.0,
            pulse_width_us: 60.0,
            frequency_hz: None,
            case: CaseSpec::default(),
            contacts: (1..=contacts as u32).map(Contact::unused).collect(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.amplitude.is_finite()
            && self.amplitude > 0.0
            && self
                .contacts
                .iter()
                .any(|c| c.percent > 0.0 && c.polarity != Polarity::Off)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SideSettings {
    #[serde(default = "first")]
    pub active_source: u32,
    #[serde(default)]
    pub sources: Vec<Source>,
}

impl Default for SideSettings {
    fn default() -> Self {
        SideSettings {
            active_source: 1,
            sources: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sides {
    #[serde(default)]
    pub right: SideSettings,
    #[serde(default)]
    pub left: SideSettings,
}

impl Sides {
    pub fn get(&self, side: Side) -> &SideSettings {
        match side {
            Side::Right => &self.right,
            Side::Left => &self.left,
        }
    }

    pub fn get_mut(&mut self, side: Side) -> &mut SideSettings {
        match side {
            Side::Right => &mut self.right,
            Side::Left => &mut self.left,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConductivityMode {
    GmWm,
    Homogeneous,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Conductivity {
    pub mode: ConductivityMode,
    pub gm_s_per_m: f64,
    pub wm_s_per_m: f64,
    pub homogeneous_s_per_m: Option<f64>,
    pub gm_mask: Option<String>,
}

impl Default for Conductivity {
    fn default() -> Self {
        Conductivity {
            mode: ConductivityMode::GmWm,
            gm_s_per_m: 0.33,
            wm_s_per_m: 0.14,
            homogeneous_s_per_m: None,
            gm_mask: Some(DEFAULT_GM_MASK.to_owned()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Encapsulation {
    pub thickness_mm: f64,
    pub s_per_m: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceCombination {
    Max,
    Sum,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DembekSettings {
    pub reference_pulse_width_us: f64,
}

impl Default for DembekSettings {
    fn default() -> Self {
        DembekSettings {
            reference_pulse_width_us: 60.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSettings {
    pub threshold_v_per_mm: f64,
    pub conductivity: Conductivity,
    pub encapsulation: Encapsulation,
    pub remove_electrode: bool,
    pub source_combination: SourceCombination,
    pub dembek: DembekSettings,
}

impl Default for ModelSettings {
    fn default() -> Self {
        ModelSettings {
            threshold_v_per_mm: 0.2,
            conductivity: Conductivity::default(),
            encapsulation: Encapsulation::default(),
            remove_electrode: true,
            source_combination: SourceCombination::Max,
            dembek: DembekSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeadDbs {
    #[serde(default = "lead_ver")]
    pub ver: String,
    #[serde(default)]
    pub extra_fields: Map<String, Value>,
}

impl Default for LeadDbs {
    fn default() -> Self {
        LeadDbs {
            ver: lead_ver(),
            extra_fields: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    #[serde(default)]
    pub software: Value,
    #[serde(default)]
    pub written_at: String,
    #[serde(default)]
    pub imported_from: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stimulation {
    #[serde(default = "schema_name")]
    pub schema: String,
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub label: String,
    #[serde(default)]
    pub subject: String,
    pub model: ModelKind,
    #[serde(default)]
    pub model_settings: ModelSettings,
    #[serde(default)]
    pub sides: Sides,
    #[serde(default)]
    pub lead_dbs: LeadDbs,
    #[serde(default)]
    pub provenance: Provenance,
}

impl Stimulation {
    pub fn blank(label: &str, subject: &str, model: ModelKind, contacts: [usize; 2]) -> Self {
        let mut s = Stimulation {
            schema: schema_name(),
            schema_version: SCHEMA_VERSION,
            label: label.to_owned(),
            subject: subject.to_owned(),
            model,
            model_settings: ModelSettings::default(),
            sides: Sides::default(),
            lead_dbs: LeadDbs::default(),
            provenance: Provenance::default(),
        };
        normalise(&mut s, contacts);
        s
    }

    pub fn contact_counts(&self) -> [usize; 2] {
        Side::ALL.map(|side| {
            self.sides
                .get(side)
                .sources
                .iter()
                .map(|s| s.contacts.len())
                .max()
                .unwrap_or(0)
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub code: &'static str,
    pub side: Option<Side>,
    pub source: Option<u32>,
    pub reason: String,
}

impl ValidationError {
    fn settings(reason: impl Into<String>) -> Self {
        ValidationError {
            code: "E_VAT_settings",
            side: None,
            source: None,
            reason: reason.into(),
        }
    }

    fn at(code: &'static str, side: Side, source: u32, reason: impl Into<String>) -> Self {
        ValidationError {
            code,
            side: Some(side),
            source: Some(source),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.side, self.source) {
            (Some(side), Some(j)) => {
                write!(f, "vat: {}: {side} source {j}: {}", self.code, self.reason)
            }
            (Some(side), None) => write!(f, "vat: {}: {side}: {}", self.code, self.reason),
            _ => write!(f, "vat: {}: {}", self.code, self.reason),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StimError {
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Json(String),
    #[error("{0}")]
    Mat(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{}", join_errors(.0))]
    Invalid(Vec<ValidationError>),
}

fn join_errors(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Debug, Default)]
pub struct Report {
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<String>,
}

pub fn label_is_valid(label: &str) -> bool {
    !label.is_empty()
        && label.chars().count() <= 64
        && !label.starts_with('.')
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn validate(
    stim: &Stimulation,
    contacts_per_side: [usize; 2],
) -> Result<(), Vec<ValidationError>> {
    let report = check(stim, contacts_per_side);
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors)
    }
}

pub fn check(stim: &Stimulation, contacts_per_side: [usize; 2]) -> Report {
    let mut r = Report::default();
    check_stimulation(stim, &mut r);
    for side in Side::ALL {
        check_side(stim, side, contacts_per_side[side.index()], &mut r);
    }
    r
}

fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

fn check_stimulation(stim: &Stimulation, r: &mut Report) {
    if stim.schema != SCHEMA || stim.schema_version != SCHEMA_VERSION {
        r.errors.push(ValidationError::settings(format!(
            "schema {:?} version {} is not {SCHEMA} version {SCHEMA_VERSION}",
            stim.schema, stim.schema_version
        )));
    }
    if !label_is_valid(&stim.label) {
        r.errors.push(ValidationError::settings(format!(
            "label {:?} must be 1-64 characters of [A-Za-z0-9._-] and not start with '.'",
            stim.label
        )));
    } else if stim.label.starts_with("gs_") {
        r.errors.push(ValidationError::settings(format!(
            "label {:?}: the prefix gs_ is reserved for group stimulations",
            stim.label
        )));
    }
    let m = &stim.model_settings;
    let th = m.threshold_v_per_mm;
    if !(th.is_finite() && th > 0.0 && th <= 10.0) {
        r.errors.push(ValidationError::settings(format!(
            "threshold_v_per_mm {th} must be in (0, 10]"
        )));
    }
    if !positive(m.conductivity.gm_s_per_m) || !positive(m.conductivity.wm_s_per_m) {
        r.errors.push(ValidationError::settings(
            "grey and white matter conductivities must be greater than 0",
        ));
    }
    if m.conductivity.mode == ConductivityMode::Homogeneous
        && !m.conductivity.homogeneous_s_per_m.is_some_and(positive)
    {
        r.errors.push(ValidationError::settings(
            "homogeneous_s_per_m must be greater than 0 in homogeneous mode",
        ));
    }
    let t = m.encapsulation.thickness_mm;
    if !(t.is_finite() && (0.0..=1.0).contains(&t)) {
        r.errors.push(ValidationError::settings(format!(
            "encapsulation.thickness_mm {t} must be in [0, 1]"
        )));
    } else if t > 0.0 && !m.encapsulation.s_per_m.is_some_and(positive) {
        r.errors.push(ValidationError::settings(
            "encapsulation.s_per_m is required and must be greater than 0 when the thickness is above 0",
        ));
    }
    if !positive(m.dembek.reference_pulse_width_us) {
        r.errors.push(ValidationError::settings(
            "dembek.reference_pulse_width_us must be greater than 0",
        ));
    }
}

fn check_side(stim: &Stimulation, side: Side, n: usize, r: &mut Report) {
    let s = stim.sides.get(side);
    if !(1..=SOURCES_PER_SIDE).contains(&s.active_source) {
        r.errors.push(ValidationError {
            code: "E_VAT_settings",
            side: Some(side),
            source: None,
            reason: format!("active_source {} must be 1 to 4", s.active_source),
        });
    }
    let mut seen = Vec::new();
    for src in &s.sources {
        if !(1..=SOURCES_PER_SIDE).contains(&src.index) || seen.contains(&src.index) {
            r.errors.push(ValidationError::at(
                "E_VAT_settings",
                side,
                src.index,
                "source index must be unique and 1 to 4",
            ));
            continue;
        }
        seen.push(src.index);
        check_source(stim, side, n, src, r);
    }
    check_impedances(stim, side, r);
}

fn check_source(stim: &Stimulation, side: Side, n: usize, src: &Source, r: &mut Report) {
    let j = src.index;
    let label = &stim.label;
    let err = |r: &mut Report, reason: String| {
        r.errors
            .push(ValidationError::at("E_VAT_settings", side, j, reason));
    };
    let warn = |r: &mut Report, text: String| {
        r.warnings.push(format!(
            "dbsloc: warning: vat: {label}: {side} source {j}: {text}"
        ));
    };
    if !src.amplitude.is_finite() || src.amplitude < 0.0 {
        err(
            r,
            format!(
                "amplitude {} must be finite and not negative",
                src.amplitude
            ),
        );
        return;
    }
    if !positive(src.pulse_width_us) {
        err(
            r,
            format!(
                "pulse_width_us {} must be greater than 0",
                src.pulse_width_us
            ),
        );
    }
    let (limit, unit) = match src.mode {
        Mode::Voltage => (10.0, "V"),
        Mode::Current => (20.0, "mA"),
    };
    if src.amplitude > limit {
        warn(
            r,
            format!("amplitude {} {unit} is above {limit} {unit}", src.amplitude),
        );
    }
    if !src.is_active() {
        return;
    }
    let mut indices = Vec::new();
    for c in &src.contacts {
        if c.index == 0 || indices.contains(&c.index) {
            err(r, format!("contact index {} is zero or repeated", c.index));
        }
        indices.push(c.index);
        if !c.percent.is_finite() || !(0.0..=100.0).contains(&c.percent) {
            err(
                r,
                format!(
                    "contact {} percent {} must be in [0, 100]",
                    c.index, c.percent
                ),
            );
            continue;
        }
        if c.index as usize > n && c.percent > 0.0 {
            err(
                r,
                format!(
                    "contact {} does not exist on the {n}-contact electrode",
                    c.index
                ),
            );
        }
        if c.percent > 0.0 && c.polarity == Polarity::Off {
            err(
                r,
                format!(
                    "contact {} has percent {} but polarity off",
                    c.index, c.percent
                ),
            );
        }
        if c.percent == 0.0 && c.polarity != Polarity::Off {
            err(
                r,
                format!("contact {} has a polarity but percent 0", c.index),
            );
        }
        if !positive(c.impedance_kohm) {
            warn(
                r,
                format!(
                    "contact {} impedance {} kOhm is not positive; 1 kOhm is used",
                    c.index, c.impedance_kohm
                ),
            );
        }
    }
    let used: Vec<&Contact> = src.contacts.iter().filter(|c| c.percent > 0.0).collect();
    let case = &src.case;
    if !case.percent.is_finite() || !(0.0..=100.0).contains(&case.percent) {
        err(
            r,
            format!("case percent {} must be in [0, 100]", case.percent),
        );
        return;
    }
    let case_part = case.polarity != Polarity::Off && case.percent > 0.0;
    if stim.model.is_analytical() {
        check_analytical(stim, side, src, &used, r);
        return;
    }
    for pol in [Polarity::Cathode, Polarity::Anode] {
        let members: Vec<&Contact> = src.contacts.iter().filter(|c| c.polarity == pol).collect();
        let case_in = case_part && case.polarity == pol;
        if members.is_empty() && !case_in {
            continue;
        }
        let sum: f64 = members.iter().map(|c| c.percent).sum::<f64>()
            + if case_in { case.percent } else { 0.0 };
        if (sum - 100.0).abs() > 0.01 {
            let text = format!(
                "{pol:?} percentages sum to {sum}, not 100 (the case counts in its polarity group)"
            );
            match src.mode {
                Mode::Current => err(r, text),
                Mode::Voltage => warn(r, text),
            }
        }
    }
    let has = |p: Polarity| {
        src.contacts
            .iter()
            .any(|c| c.polarity == p && c.percent > 0.0)
    };
    if !((has(Polarity::Cathode) && has(Polarity::Anode)) || case_part) {
        r.warnings.push(format!(
            "dbsloc: warning: vat: {label}: {side} source {j} has no return electrode; the case is used as return"
        ));
    }
}

fn check_analytical(
    stim: &Stimulation,
    side: Side,
    src: &Source,
    used: &[&Contact],
    r: &mut Report,
) {
    let j = src.index;
    let name = stim.model.display_name();
    if used.len() != 1 {
        r.errors.push(ValidationError::at(
            "E_VAT_montage",
            side,
            j,
            format!(
                "model {name} needs exactly one used contact per active source, found {}",
                used.len()
            ),
        ));
    }
    if matches!(stim.model, ModelKind::Kuncel | ModelKind::Maedler) && src.mode != Mode::Voltage {
        r.errors.push(ValidationError::at(
            "E_VAT_mode",
            side,
            j,
            format!("model {name} requires voltage mode"),
        ));
    }
    if stim.model == ModelKind::Maedler {
        let label = &stim.label;
        if src.mode == Mode::Voltage && src.amplitude > 10.0 {
            r.warnings.push(format!(
                "dbsloc: warning: vat: {label}: {side} source {j}: amplitude {} V is outside the 0-10 V validity range of Maedler 2012",
                src.amplitude
            ));
        }
        if let Some(c) = used.first() {
            if c.impedance_kohm.is_finite() && !(0.5..=2.0).contains(&c.impedance_kohm) {
                r.warnings.push(format!(
                    "dbsloc: warning: vat: {label}: {side} source {j}: impedance {} kOhm is outside the 500-2000 Ohm validity range of Maedler 2012",
                    c.impedance_kohm
                ));
            }
        }
    }
}

fn check_impedances(stim: &Stimulation, side: Side, r: &mut Report) {
    let sources: Vec<&Source> = stim
        .sides
        .get(side)
        .sources
        .iter()
        .filter(|s| s.is_active())
        .collect();
    let mut flagged = Vec::new();
    for a in &sources {
        for c in &a.contacts {
            if c.percent <= 0.0 || flagged.contains(&c.index) {
                continue;
            }
            let differs = sources.iter().any(|b| {
                b.contacts.iter().any(|d| {
                    d.index == c.index && d.percent > 0.0 && d.impedance_kohm != c.impedance_kohm
                })
            });
            if differs {
                flagged.push(c.index);
                r.warnings.push(format!(
                    "dbsloc: warning: vat: {}: {side} contact {} has different impedances in different sources",
                    stim.label, c.index
                ));
            }
        }
    }
}

pub fn normalise(stim: &mut Stimulation, contacts_per_side: [usize; 2]) -> Vec<String> {
    let mut notes = Vec::new();
    stim.schema = schema_name();
    stim.schema_version = SCHEMA_VERSION;
    if stim.lead_dbs.ver.is_empty() {
        stim.lead_dbs.ver = lead_ver();
    }
    let c = &mut stim.model_settings.conductivity;
    if c.mode == ConductivityMode::Homogeneous && c.homogeneous_s_per_m.is_none() {
        c.homogeneous_s_per_m = Some(0.1);
    }
    for side in Side::ALL {
        let n = contacts_per_side[side.index()];
        let existing = std::mem::take(&mut stim.sides.get_mut(side).sources);
        let mut sources: Vec<Source> = Vec::new();
        for j in 1..=SOURCES_PER_SIDE {
            match existing.iter().find(|s| s.index == j) {
                Some(s) => {
                    let mut s = s.clone();
                    normalise_contacts(&mut s, n, side, &stim.label, &mut notes);
                    sources.push(s);
                }
                None => sources.push(Source::blank(j, n)),
            }
        }
        for s in &existing {
            if !(1..=SOURCES_PER_SIDE).contains(&s.index) {
                notes.push(format!(
                    "dbsloc: note: vat: {}: {side} source {} is outside 1 to 4 and was dropped",
                    stim.label, s.index
                ));
            }
        }
        stim.sides.get_mut(side).sources = sources;
    }
    notes
}

fn normalise_contacts(
    src: &mut Source,
    n: usize,
    side: Side,
    label: &str,
    notes: &mut Vec<String>,
) {
    let mut out: Vec<Contact> = (1..=n as u32)
        .map(|i| {
            src.contacts
                .iter()
                .find(|c| c.index == i)
                .cloned()
                .unwrap_or_else(|| Contact::unused(i))
        })
        .collect();
    for c in &src.contacts {
        if c.index as usize > n || c.index == 0 {
            if c.percent > 0.0 {
                out.push(c.clone());
            } else {
                notes.push(format!(
                    "dbsloc: note: vat: {label}: {side} source {}: contact {} is not on the electrode and was dropped",
                    src.index, c.index
                ));
            }
        }
    }
    for c in &mut out {
        if !positive(c.impedance_kohm) {
            notes.push(format!(
                "dbsloc: warning: vat: {label}: {side} source {} contact {}: impedance {} kOhm is not positive; using 1",
                src.index, c.index, c.impedance_kohm
            ));
            c.impedance_kohm = 1.0;
        }
    }
    src.contacts = out;
}

pub fn contact_counts_from_reco(reco: &Value) -> Result<[Option<usize>; 2], String> {
    let empty = Map::new();
    let meta = reco["meta"].as_object().unwrap_or(&empty);
    let mut counts: [Option<usize>; 2] = [None, None];
    for block_name in ["mni", "native"] {
        let Some(block) = reco[block_name].as_object() else {
            continue;
        };
        for (key, hemi) in side_hemispheres(meta, block, block_name)? {
            let index = usize::from(hemi != "right");
            if counts[index].is_some() {
                continue;
            }
            let name = reco["props"][key.as_str()]["elmodel"]
                .as_str()
                .or_else(|| reco["meta"]["elmodel"].as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("the {hemi} lead has no electrode model"))?;
            let e = electrode_by_name(name)
                .ok_or_else(|| format!("electrode model {name:?} is not in the catalogue"))?;
            counts[index] = Some(e.n_contacts);
        }
    }
    Ok(counts)
}

pub fn contact_counts(sub: &Subject) -> Result<Option<[usize; 2]>, StimError> {
    let path = sub.reconstruction_json();
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| StimError::Io(format!("cannot read {}: {e}", path.display())))?;
    let reco: Value = serde_json::from_str(&text)
        .map_err(|e| StimError::Json(format!("{}: {e}", path.display())))?;
    let counts = contact_counts_from_reco(&reco)
        .map_err(|e| StimError::Json(format!("{}: {e}", path.display())))?;
    Ok(Some(counts.map(|c| c.unwrap_or(0))))
}

pub fn stimulations_dir(subject_dir: &Path) -> PathBuf {
    subject_dir.join("stimulations")
}

pub fn label_dir(subject_dir: &Path, space: &str, label: &str) -> PathBuf {
    stimulations_dir(subject_dir).join(space).join(label)
}

pub fn stimulation_json_path(subject_dir: &Path, subject: &str, label: &str) -> PathBuf {
    label_dir(subject_dir, "native", label).join(format!("sub-{subject}_desc-stimulation.json"))
}

pub fn stimparameters_mat_path(
    subject_dir: &Path,
    subject: &str,
    space: &str,
    label: &str,
) -> PathBuf {
    label_dir(subject_dir, space, label).join(format!("sub-{subject}_desc-stimparameters.mat"))
}

fn check_label_path(label: &str) -> Result<(), StimError> {
    if label_is_valid(label) {
        Ok(())
    } else {
        Err(StimError::Invalid(vec![ValidationError::settings(
            format!(
                "label {label:?} must be 1-64 characters of [A-Za-z0-9._-] and not start with '.'"
            ),
        )]))
    }
}

pub fn canonical_json(stim: &Stimulation) -> Result<String, StimError> {
    serde_json::to_string_pretty(stim)
        .map(|t| t + "\n")
        .map_err(|e| StimError::Json(e.to_string()))
}

pub fn load(subject_dir: &Path, subject: &str, label: &str) -> Result<Stimulation, StimError> {
    check_label_path(label)?;
    let path = stimulation_json_path(subject_dir, subject, label);
    if !path.is_file() {
        return Err(StimError::NotFound(format!(
            "no stimulation {label:?}: {} does not exist",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| StimError::Io(format!("cannot read {}: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| StimError::Json(format!("{}: {e}", path.display())))
}

pub fn save(subject_dir: &Path, stim: &Stimulation) -> Result<PathBuf, StimError> {
    check_label_path(&stim.label)?;
    let mut stamped = stim.clone();
    stamped.provenance.software = software_stamp();
    stamped.provenance.written_at = utc_now_rfc3339();
    let text = canonical_json(&stamped)?;
    let path = stimulation_json_path(subject_dir, &stim.subject, &stim.label);
    write_replacing(&path, text.as_bytes())
        .map_err(|e| StimError::Io(format!("cannot write {}: {e}", path.display())))?;
    Ok(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelState {
    Canonical,
    Importable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LabelEntry {
    pub label: String,
    pub space: String,
    pub state: LabelState,
}

fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    out.sort();
    out
}

pub fn list_labels(subject_dir: &Path, subject: &str) -> Vec<LabelEntry> {
    let mut out: Vec<LabelEntry> = Vec::new();
    let mut spaces = subdirs(&stimulations_dir(subject_dir));
    spaces.sort_by_key(|(name, _)| name != "native");
    for (space, space_dir) in spaces {
        if space.starts_with('.') {
            continue;
        }
        for (label, dir) in subdirs(&space_dir) {
            if label.starts_with('.') || label.starts_with("gs_") {
                continue;
            }
            let canonical = space == "native"
                && dir
                    .join(format!("sub-{subject}_desc-stimulation.json"))
                    .is_file();
            let importable = dir
                .join(format!("sub-{subject}_desc-stimparameters.mat"))
                .is_file();
            if canonical {
                out.retain(|e| e.label != label);
                out.push(LabelEntry {
                    label,
                    space: space.clone(),
                    state: LabelState::Canonical,
                });
            } else if importable && !out.iter().any(|e| e.label == label) {
                out.push(LabelEntry {
                    label,
                    space: space.clone(),
                    state: LabelState::Importable,
                });
            }
        }
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

fn num_of(v: &Mat5Value) -> Option<f64> {
    match v {
        Mat5Value::Double { data, .. } => data.first().copied(),
        Mat5Value::Logical { data, .. } => data.first().map(|b| f64::from(u8::from(*b))),
        _ => None,
    }
}

fn text_of(v: &Mat5Value) -> Option<&str> {
    match v {
        Mat5Value::Char(s) => Some(s),
        _ => None,
    }
}

fn json_number(v: f64) -> Value {
    serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
}

fn json_to_mat(v: &Value, depth: usize) -> Mat5Value {
    if depth > MAX_DEPTH {
        return Mat5Value::Empty;
    }
    match v {
        Value::Null => Mat5Value::Empty,
        Value::Bool(b) => Mat5Value::logical(*b),
        Value::Number(n) => Mat5Value::scalar(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => Mat5Value::char(s),
        Value::Array(items) if items.is_empty() => Mat5Value::Empty,
        Value::Array(items) if items.iter().all(Value::is_number) => {
            let data: Vec<f64> = items.iter().filter_map(Value::as_f64).collect();
            Mat5Value::row(&data)
        }
        Value::Array(items) => {
            Mat5Value::cell_row(items.iter().map(|i| json_to_mat(i, depth + 1)).collect())
        }
        Value::Object(map) => Mat5Value::struct_scalar(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_mat(v, depth + 1)))
                .collect(),
        ),
    }
}

fn mat_to_json(v: &Mat5Value, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return Value::Null;
    }
    match v {
        Mat5Value::Empty | Mat5Value::Unsupported { .. } => Value::Null,
        Mat5Value::Char(s) => Value::String(s.clone()),
        Mat5Value::CharMatrix { rows, .. } => {
            Value::Array(rows.iter().map(|r| Value::String(r.clone())).collect())
        }
        Mat5Value::Double { data, .. } => match data.as_slice() {
            [] => Value::Null,
            [x] => json_number(*x),
            xs => Value::Array(xs.iter().map(|x| json_number(*x)).collect()),
        },
        Mat5Value::Logical { data, .. } => match data.as_slice() {
            [] => Value::Null,
            [b] => Value::Bool(*b),
            bs => Value::Array(bs.iter().map(|b| Value::Bool(*b)).collect()),
        },
        Mat5Value::Cell { items, .. } => {
            Value::Array(items.iter().map(|i| mat_to_json(i, depth + 1)).collect())
        }
        Mat5Value::Struct {
            fields, elements, ..
        } => {
            let object = |el: &Vec<Mat5Value>| {
                Value::Object(
                    fields
                        .iter()
                        .zip(el)
                        .map(|(k, v)| (k.clone(), mat_to_json(v, depth + 1)))
                        .collect(),
                )
            };
            match elements.as_slice() {
                [one] => object(one),
                many => Value::Array(many.iter().map(object).collect()),
            }
        }
    }
}

fn complete_sources(side: &SideSettings, n: usize) -> Vec<Source> {
    (1..=SOURCES_PER_SIDE)
        .map(|j| {
            let mut s = side
                .sources
                .iter()
                .find(|s| s.index == j)
                .cloned()
                .unwrap_or_else(|| Source::blank(j, n));
            while s.contacts.len() < n {
                let index = s.contacts.len() as u32 + 1;
                s.contacts.push(Contact::unused(index));
            }
            s
        })
        .collect()
}

fn field(name: &str, value: Mat5Value) -> (String, Mat5Value) {
    (name.to_owned(), value)
}

fn contact_struct(c: &Contact) -> Mat5Value {
    Mat5Value::struct_scalar(vec![
        field("perc", Mat5Value::scalar(c.percent)),
        field("pol", Mat5Value::scalar(c.polarity.code())),
        field("imp", Mat5Value::scalar(c.impedance_kohm)),
    ])
}

fn source_struct(s: &Source) -> Mat5Value {
    let mut fields: Vec<(String, Mat5Value)> = s
        .contacts
        .iter()
        .enumerate()
        .map(|(i, c)| (format!("k{}", i + 1), contact_struct(c)))
        .collect();
    fields.push(field(
        "case",
        Mat5Value::struct_scalar(vec![
            field("perc", Mat5Value::scalar(s.case.percent)),
            field("pol", Mat5Value::scalar(s.case.polarity.code())),
        ]),
    ));
    fields.push(field("amp", Mat5Value::scalar(s.amplitude)));
    fields.push(field("va", Mat5Value::scalar(s.mode.code())));
    fields.push(field("pulseWidth", Mat5Value::scalar(s.pulse_width_us)));
    Mat5Value::struct_scalar(fields)
}

pub fn export_s(stim: &Stimulation, volume: [f64; 2]) -> (Mat5Value, Vec<String>) {
    let mut warnings = Vec::new();
    let counts = stim.contact_counts();
    let n = counts[0].max(counts[1]);
    if counts[0] != counts[1] && counts[0] > 0 && counts[1] > 0 {
        warnings.push(format!(
            "dbsloc: warning: vat: {}: the two sides use electrode models with different contact counts; the shorter side is padded",
            stim.label
        ));
    }
    let sides: Vec<Vec<Source>> = Side::ALL
        .iter()
        .map(|side| complete_sources(stim.sides.get(*side), n))
        .collect();
    let mut fields = vec![field("label", Mat5Value::char(&stim.label))];
    for (side, sources) in Side::ALL.iter().zip(&sides) {
        for s in sources {
            fields.push((format!("{}s{}", side.letter(), s.index), source_struct(s)));
        }
    }
    fields.push(field(
        "active",
        Mat5Value::row(&[
            f64::from(stim.sides.right.active_source),
            f64::from(stim.sides.left.active_source),
        ]),
    ));
    fields.push(field("model", Mat5Value::char(stim.model.display_name())));
    fields.push(field(
        "monopolarmodel",
        Mat5Value::scalar(f64::from(u8::from(stim.model.is_analytical()))),
    ));
    let amplitudes = sides
        .iter()
        .map(|srcs| Mat5Value::row(&srcs.iter().map(|s| s.amplitude).collect::<Vec<_>>()))
        .collect();
    fields.push(field("amplitude", Mat5Value::cell_row(amplitudes)));
    let active_contacts = sides
        .iter()
        .map(|srcs| {
            let used: Vec<f64> = (0..n)
                .map(|k| {
                    let on = srcs.iter().any(|s| {
                        s.amplitude > 0.0 && s.contacts.get(k).is_some_and(|c| c.percent > 0.0)
                    });
                    f64::from(u8::from(on))
                })
                .collect();
            Mat5Value::row(&used)
        })
        .collect();
    fields.push(field(
        "activecontacts",
        Mat5Value::cell_row(active_contacts),
    ));
    fields.push(field("numContacts", Mat5Value::scalar(n as f64)));
    fields.push(field("sources", Mat5Value::row(&[1.0, 2.0, 3.0, 4.0])));
    fields.push(field("volume", Mat5Value::row(&volume)));
    fields.push(field("ver", Mat5Value::char(&stim.lead_dbs.ver)));
    for (k, v) in &stim.lead_dbs.extra_fields {
        if fields.iter().all(|(name, _)| name != k) {
            fields.push((k.clone(), json_to_mat(v, 0)));
        }
    }
    (Mat5Value::struct_scalar(fields), warnings)
}

pub fn to_lead_dbs_s(stim: &Stimulation) -> Mat5Value {
    export_s(stim, [0.0, 0.0]).0
}

fn field_names(v: &Mat5Value) -> &[String] {
    match v {
        Mat5Value::Struct { fields, .. } => fields,
        _ => &[],
    }
}

fn get<'a>(v: &'a Mat5Value, name: &str) -> Option<&'a Mat5Value> {
    v.field(name, 0)
}

fn bad_value(label: &str, what: &str, text: String) -> StimError {
    StimError::Invalid(vec![ValidationError::settings(format!(
        "{label}: {what}: {text}"
    ))])
}

struct ReadCtx<'a> {
    label: &'a str,
    notes: Vec<String>,
}

impl ReadCtx<'_> {
    fn note(&mut self, text: String) {
        self.notes
            .push(format!("dbsloc: note: vat: {}: {text}", self.label));
    }

    fn polarity(
        &self,
        holder: &Mat5Value,
        what: &str,
        default: Polarity,
    ) -> Result<Polarity, StimError> {
        match get(holder, "pol").and_then(num_of) {
            Some(v) => Polarity::from_code(v).ok_or_else(|| {
                bad_value(self.label, what, format!("polarity {v} is not 0, 1 or 2"))
            }),
            None => Ok(default),
        }
    }
}

fn read_contact(
    ctx: &mut ReadCtx<'_>,
    holders: &[&Mat5Value],
    name: &str,
    index: u32,
    what: &str,
) -> Result<Contact, StimError> {
    let Some(k) = holders.iter().find_map(|h| get(h, name)) else {
        return Ok(Contact::unused(index));
    };
    let polarity = ctx.polarity(k, what, Polarity::Off)?;
    let impedance = match get(k, "imp").and_then(num_of) {
        Some(v) => v,
        None => {
            ctx.note(format!("{what}: {name}.imp is missing; using 1"));
            1.0
        }
    };
    Ok(Contact {
        index,
        polarity,
        percent: get(k, "perc").and_then(num_of).unwrap_or(0.0),
        impedance_kohm: impedance,
    })
}

fn count_k_fields(v: &Mat5Value) -> usize {
    field_names(v)
        .iter()
        .filter_map(|n| n.strip_prefix('k')?.parse::<usize>().ok())
        .filter(|i| *i >= 1)
        .max()
        .unwrap_or(0)
}

fn read_source(
    ctx: &mut ReadCtx<'_>,
    side: Side,
    j: u32,
    own: Option<&Mat5Value>,
    other: Option<&Mat5Value>,
    n: usize,
    legacy: bool,
) -> Result<Source, StimError> {
    let Some(primary) = own.or(other) else {
        return Ok(Source::blank(j, n));
    };
    let what = format!("{}s{j}", side.letter());
    let holders: Vec<&Mat5Value> = std::iter::once(primary)
        .chain(other.filter(|_| legacy))
        .collect();
    let mut contacts = Vec::with_capacity(n);
    for i in 1..=n {
        let name = match (legacy, side) {
            (false, _) => format!("k{i}"),
            (true, _) if i > 8 => {
                contacts.push(Contact::unused(i as u32));
                continue;
            }
            (true, Side::Right) => format!("k{}", i - 1),
            (true, Side::Left) => format!("k{}", i + 7),
        };
        contacts.push(read_contact(ctx, &holders, &name, i as u32, &what)?);
    }
    let case = match get(primary, "case") {
        Some(c) => CaseSpec {
            polarity: ctx.polarity(c, &what, Polarity::Anode)?,
            percent: get(c, "perc").and_then(num_of).unwrap_or(100.0),
        },
        None => CaseSpec::default(),
    };
    let mode = match get(primary, "va").and_then(num_of) {
        Some(v) => Mode::from_code(v)
            .ok_or_else(|| bad_value(ctx.label, &what, format!("va {v} is not 1 or 2")))?,
        None => Mode::Current,
    };
    let pulse_width_us = match get(primary, "pulseWidth").and_then(num_of) {
        Some(v) => v,
        None => {
            ctx.note(format!("{what}: pulseWidth is missing; using 60"));
            60.0
        }
    };
    Ok(Source {
        index: j,
        mode,
        amplitude: get(primary, "amp").and_then(num_of).unwrap_or(0.0),
        pulse_width_us,
        frequency_hz: None,
        case,
        contacts,
    })
}

fn is_source_field(name: &str) -> bool {
    name.strip_prefix(['R', 'L'])
        .and_then(|r| r.strip_prefix('s'))
        .is_some_and(|d| matches!(d, "1" | "2" | "3" | "4"))
}

pub fn from_lead_dbs_s(
    s: &Mat5Value,
    subject: &str,
    fallback_contacts: usize,
) -> Result<(Stimulation, Vec<String>), StimError> {
    if !matches!(s, Mat5Value::Struct { .. }) {
        return Err(StimError::Mat("variable S is not a struct".to_owned()));
    }
    let label = get(s, "label").and_then(text_of).unwrap_or("").to_owned();
    let mut ctx = ReadCtx {
        label: &label,
        notes: Vec::new(),
    };
    let legacy = get(s, "ver").and_then(text_of).is_none();
    let model = match get(s, "model").and_then(text_of) {
        Some(name) => ModelKind::from_display_name(name).ok_or_else(|| {
            StimError::Invalid(vec![ValidationError {
                code: "E_VAT_model",
                side: None,
                source: None,
                reason: format!("unknown model {name:?}"),
            }])
        })?,
        None => ModelKind::Efield,
    };
    let declared = get(s, "numContacts")
        .and_then(num_of)
        .map(|v| v as usize)
        .filter(|d| *d > 0);
    let mut n = 0;
    if !legacy {
        for side in Side::ALL {
            for j in 1..=SOURCES_PER_SIDE {
                if let Some(src) = get(s, &format!("{}s{j}", side.letter())) {
                    n = n.max(count_k_fields(src));
                }
            }
        }
    }
    if n == 0 {
        n = declared.unwrap_or(fallback_contacts);
    }
    let mut stim = Stimulation::blank(&label, subject, model, [0, 0]);
    for side in Side::ALL {
        let other_letter = if side == Side::Right { 'L' } else { 'R' };
        let mut sources = Vec::new();
        for j in 1..=SOURCES_PER_SIDE {
            let own = get(s, &format!("{}s{j}", side.letter()));
            let other = get(s, &format!("{other_letter}s{j}"));
            sources.push(read_source(&mut ctx, side, j, own, other, n, legacy)?);
        }
        stim.sides.get_mut(side).sources = sources;
    }
    if let Some(Mat5Value::Double { data, .. }) = get(s, "active") {
        for (side, v) in Side::ALL.iter().zip(data) {
            if (1.0..=4.0).contains(v) {
                stim.sides.get_mut(*side).active_source = *v as u32;
            }
        }
    }
    if let Some(cell) = get(s, "amplitude") {
        for (si, side) in Side::ALL.iter().enumerate() {
            let Some(Mat5Value::Double { data, .. }) = cell.item(si) else {
                continue;
            };
            for (jj, a) in data.iter().enumerate() {
                let own = stim.sides.get(*side).sources.get(jj).map(|x| x.amplitude);
                if own.is_some_and(|o| (o - a).abs() > 1e-12) {
                    ctx.notes.push(format!(
                        "dbsloc: warning: vat: {label}: S.amplitude disagrees with {}s{}.amp; using amp",
                        side.letter(),
                        jj + 1
                    ));
                }
            }
        }
    }
    if let Mat5Value::Struct {
        fields, elements, ..
    } = s
    {
        for (name, value) in fields.iter().zip(elements.first().into_iter().flatten()) {
            if !S_KNOWN_FIELDS.contains(&name.as_str()) && !is_source_field(name) {
                stim.lead_dbs
                    .extra_fields
                    .insert(name.clone(), mat_to_json(value, 0));
            }
        }
    }
    stim.lead_dbs.ver = lead_ver();
    Ok((stim, ctx.notes))
}

pub fn write_stimparameters_mat(
    path: &Path,
    stim: &Stimulation,
    volume: [f64; 2],
) -> Result<Vec<String>, StimError> {
    let (s, warnings) = export_s(stim, volume);
    let sha = sha256_hex(canonical_json(stim)?.as_bytes());
    let written_at = if stim.provenance.written_at.is_empty() {
        utc_now_rfc3339()
    } else {
        stim.provenance.written_at.clone()
    };
    let provenance = Mat5Value::struct_scalar(vec![
        field("product", Mat5Value::char(PRODUCT)),
        field("version", Mat5Value::char(&version_string())),
        field("git_hash", Mat5Value::char(GIT_HASH)),
        field("source_json_sha256", Mat5Value::char(&sha)),
        field("written_at", Mat5Value::char(&written_at)),
    ]);
    let vars = vec![
        ("S".to_owned(), s),
        (PROVENANCE_VARIABLE.to_owned(), provenance),
    ];
    let bytes = mat5_bytes(&vars, true).map_err(|e| StimError::Mat(e.to_string()))?;
    write_replacing(path, &bytes)
        .map_err(|e| StimError::Io(format!("cannot write {}: {e}", path.display())))?;
    Ok(warnings)
}

pub fn read_s_variable(path: &Path) -> Result<Mat5Value, StimError> {
    let mat = |e: String| StimError::Mat(format!("{}: {e}", path.display()));
    let vars = read_any_with(
        path,
        ReadOptions {
            only: Some("S"),
            skip_fields: &[],
        },
    )
    .map_err(|e| match e {
        MatError::Io { source, .. } => {
            StimError::Io(format!("cannot read {}: {source}", path.display()))
        }
        other => mat(other.to_string()),
    })?;
    vars.into_iter()
        .next()
        .map(|(_, v)| v)
        .ok_or_else(|| mat("no variable S".to_owned()))
}

pub fn read_stimparameters_mat(
    path: &Path,
    subject: &str,
    fallback_contacts: usize,
) -> Result<(Stimulation, Vec<String>), StimError> {
    from_lead_dbs_s(&read_s_variable(path)?, subject, fallback_contacts)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
