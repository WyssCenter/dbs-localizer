// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The vendor-neutral record `dbsloc-session-report` version 1 (`specs/16` §3).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

pub const SCHEMA: &str = "dbsloc-session-report";
pub const SCHEMA_VERSION: u32 = 1;

fn default_schema() -> String {
    SCHEMA.to_owned()
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

fn no_extra(map: &Map<String, Value>) -> bool {
    map.is_empty()
}

fn whole_number<S: Serializer>(value: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match value {
        Some(v) if v.is_finite() && v.fract() == 0.0 && v.abs() < 9.0e15 => {
            s.serialize_i64(*v as i64)
        }
        Some(v) => s.serialize_f64(*v),
        None => s.serialize_none(),
    }
}

fn optional_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Option::<f64>::deserialize(d)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Medtronic,
    BostonScientific,
    Abbott,
}

impl Vendor {
    pub fn as_str(self) -> &'static str {
        match self {
            Vendor::Medtronic => "medtronic",
            Vendor::BostonScientific => "boston_scientific",
            Vendor::Abbott => "abbott",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hemisphere {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    Off,
    Cathode,
    Anode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StimMode {
    Current,
    Voltage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sex {
    Male,
    Female,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpedanceKind {
    Monopolar,
    Bipolar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldStatus {
    Read,
    Derived,
    Missing,
    Ambiguous,
    NotReported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParserStamp {
    pub product: String,
    pub version: String,
    pub git_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub filename: String,
    pub sha256: String,
    pub pages: u32,
    pub printed_at: Option<String>,
    pub parser: ParserStamp,
    pub parsed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anonymization {
    pub method: String,
    pub range_days: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Patient {
    pub name: Option<String>,
    pub patient_id: Option<String>,
    pub sex: Option<Sex>,
    pub date_of_birth: Option<String>,
    pub diagnosis: Option<String>,
    pub clinician_notes: Option<String>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Firmware {
    pub product_version: Option<String>,
    pub core_version: Option<String>,
    pub device_version: Option<String>,
    pub product_application_id: Option<String>,
    pub core_application_id: Option<String>,
    pub device_application_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub model: Option<String>,
    pub model_number: Option<String>,
    pub serial: Option<String>,
    pub implanted: Option<String>,
    pub location: Option<String>,
    pub rechargeable: Option<bool>,
    pub battery_percent: Option<f64>,
    pub battery_estimate: Option<String>,
    pub firmware: Firmware,
    pub feature_code: Option<String>,
    pub programmer_version: Option<String>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub date: Option<String>,
    pub timezone: Option<String>,
    pub stimulation_initial: Option<String>,
    pub stimulation_final: Option<String>,
    pub battery_reminder: Option<String>,
    pub close_communication_required: Option<bool>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeadContact {
    pub label: String,
    pub device_number: Option<u32>,
    pub k: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lead {
    pub hemisphere: Hemisphere,
    pub vendor_model: Option<String>,
    pub catalogue_model: Option<String>,
    pub target: Option<String>,
    pub contacts: Vec<LeadContact>,
    pub orientation_deg: Option<f64>,
    pub tip_offset: Option<f64>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub lower: Option<f64>,
    pub upper: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramContact {
    pub label: String,
    pub k: u32,
    pub polarity: Polarity,
    pub amplitude: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sensing {
    pub mode: Option<String>,
    pub status: Option<String>,
    pub contacts: Vec<String>,
    pub lfp_hz: Option<f64>,
    pub limits: Option<Limits>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub index: u32,
    pub mode: Option<StimMode>,
    pub amplitude: Option<f64>,
    pub unit: Option<String>,
    #[serde(serialize_with = "whole_number", deserialize_with = "optional_number")]
    pub pulse_width_us: Option<f64>,
    #[serde(serialize_with = "whole_number", deserialize_with = "optional_number")]
    pub frequency_hz: Option<f64>,
    pub limits: Limits,
    pub case: Option<Polarity>,
    pub contacts: Vec<ProgramContact>,
    pub contact_sum_matches_amplitude: Option<bool>,
    pub sensing: Option<Sensing>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HemisphereSettings {
    pub programs: Vec<Program>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hemispheres {
    pub left: Option<HemisphereSettings>,
    pub right: Option<HemisphereSettings>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: Option<String>,
    pub active: bool,
    pub hemispheres: Hemispheres,
    pub cycling: Option<Value>,
    pub soft_start_stop: Option<Value>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub active_group: Option<String>,
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshots {
    pub initial: Snapshot,
    #[serde(rename = "final")]
    pub final_: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub date: Option<String>,
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupUsage {
    pub id: String,
    pub percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Impedance {
    pub hemisphere: Hemisphere,
    pub kind: ImpedanceKind,
    pub a: String,
    pub b: String,
    pub ohm: Option<f64>,
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub date: Option<String>,
    pub name: Option<String>,
    pub count: Option<u32>,
    pub hemisphere: Option<Hemisphere>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymptomScore {
    pub name: String,
    pub score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub date: Option<String>,
    pub hemisphere: Option<Hemisphere>,
    #[serde(serialize_with = "whole_number", deserialize_with = "optional_number")]
    pub frequency_hz: Option<f64>,
    pub mode: Option<StimMode>,
    pub program: Option<Value>,
    pub symptoms: Vec<SymptomScore>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SensingData {
    pub surveys: Vec<Value>,
    pub streaming: Vec<Value>,
    pub timeline: Vec<Value>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSource {
    pub path: String,
    pub status: FieldStatus,
    pub page: Option<u32>,
    pub bbox: Option<[f64; 4]>,
    pub rule: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReport {
    #[serde(default = "default_schema")]
    pub schema: String,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub vendor: Vendor,
    pub report_kind: String,
    pub source: Source,
    pub anonymized: bool,
    pub anonymization: Option<Anonymization>,
    pub patient: Patient,
    pub device: Device,
    pub session: Session,
    pub leads: Vec<Lead>,
    pub snapshots: Snapshots,
    pub history: Vec<HistoryEntry>,
    pub group_usage: Vec<GroupUsage>,
    pub impedances: Vec<Impedance>,
    pub events: Vec<Event>,
    pub annotations: Vec<Annotation>,
    pub sensing: SensingData,
    pub fields: Vec<FieldSource>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default, skip_serializing_if = "no_extra")]
    pub sections: Map<String, Value>,
}

impl SessionReport {
    pub fn blank(vendor: Vendor, report_kind: &str) -> Self {
        SessionReport {
            schema: default_schema(),
            schema_version: SCHEMA_VERSION,
            vendor,
            report_kind: report_kind.to_owned(),
            source: Source {
                filename: String::new(),
                sha256: String::new(),
                pages: 0,
                printed_at: None,
                parser: ParserStamp {
                    product: "dbsloc".to_owned(),
                    version: String::new(),
                    git_hash: "unknown".to_owned(),
                },
                parsed_at: None,
            },
            anonymized: false,
            anonymization: None,
            patient: Patient::default(),
            device: Device::default(),
            session: Session::default(),
            leads: Vec::new(),
            snapshots: Snapshots::default(),
            history: Vec::new(),
            group_usage: Vec::new(),
            impedances: Vec::new(),
            events: Vec::new(),
            annotations: Vec::new(),
            sensing: SensingData::default(),
            fields: Vec::new(),
            diagnostics: Vec::new(),
            sections: Map::new(),
        }
    }

    pub fn push_diagnostic(
        &mut self,
        severity: Severity,
        code: &str,
        path: Option<&str>,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(Diagnostic {
            severity,
            code: code.to_owned(),
            path: path.map(str::to_owned),
            message: message.into(),
        });
    }

    pub fn push_field(
        &mut self,
        path: impl Into<String>,
        status: FieldStatus,
        page: Option<u32>,
        bbox: Option<[f64; 4]>,
        rule: Option<&str>,
    ) {
        self.fields.push(FieldSource {
            path: path.into(),
            status,
            page,
            bbox,
            rule: rule.map(str::to_owned),
        });
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

pub fn canonical_json(report: &SessionReport) -> Result<String, serde_json::Error> {
    let mut text = serde_json::to_string_pretty(report)?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_round_trips_and_keeps_nulls() {
        let r = SessionReport::blank(Vendor::Medtronic, "percept_session_report");
        let text = canonical_json(&r).expect("json");
        assert!(text.ends_with("}\n"));
        assert!(text.contains("\"name\": null"));
        assert!(text.contains("\"schema\": \"dbsloc-session-report\""));
        assert!(!text.contains("\"extra\""));
        let back: SessionReport = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, r);
    }

    #[test]
    fn key_order_follows_spec() {
        let r = SessionReport::blank(Vendor::Abbott, "x");
        let text = canonical_json(&r).expect("json");
        let order = [
            "\"schema\"",
            "\"vendor\"",
            "\"source\"",
            "\"anonymized\"",
            "\"patient\"",
            "\"device\"",
            "\"session\"",
            "\"leads\"",
            "\"snapshots\"",
            "\"history\"",
            "\"group_usage\"",
            "\"impedances\"",
            "\"events\"",
            "\"annotations\"",
            "\"sensing\"",
            "\"fields\"",
            "\"diagnostics\"",
        ];
        let mut last = 0;
        for key in order {
            let at = text.find(key).expect(key);
            assert!(at >= last, "{key}");
            last = at;
        }
    }

    #[test]
    fn enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&Vendor::BostonScientific).expect("v"),
            "\"boston_scientific\""
        );
        assert_eq!(
            serde_json::to_string(&FieldStatus::NotReported).expect("f"),
            "\"not_reported\""
        );
        assert_eq!(Vendor::BostonScientific.as_str(), "boston_scientific");
    }

    #[test]
    fn whole_numbers_print_as_integers() {
        let mut r = SessionReport::blank(Vendor::Medtronic, "k");
        r.annotations.push(Annotation {
            date: None,
            hemisphere: None,
            frequency_hz: Some(130.0),
            mode: None,
            program: None,
            symptoms: Vec::new(),
            extra: Map::new(),
        });
        let text = canonical_json(&r).expect("json");
        assert!(text.contains("\"frequency_hz\": 130,"));
    }

    #[test]
    fn diagnostics_and_errors() {
        let mut r = SessionReport::blank(Vendor::Medtronic, "k");
        assert!(!r.has_errors());
        r.push_diagnostic(Severity::Warning, "W_REPORT_x", None, "w");
        assert!(!r.has_errors());
        r.push_diagnostic(Severity::Error, "E_REPORT_x", Some("a.b"), "e");
        assert!(r.has_errors());
        r.push_field("a", FieldStatus::Read, Some(1), Some([0.0; 4]), Some("r"));
        assert_eq!(r.fields.len(), 1);
    }
}
