// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Anonymisation of a session record (`specs/16` §4): identity removed, every date moved by one patient-constant offset.

use fastwarp_core::sha256::Sha256;
use serde_json::{Map, Value};

use crate::dates::shift_days;
use crate::model::{Anonymization, Group, SessionReport};

pub const SHIFT_RANGE_DAYS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shift {
    pub days: i32,
}

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let mut h = Sha256::new();
        h.update(key);
        k[..32].copy_from_slice(&h.finish());
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(msg);
    let inner_hash = inner.finish();
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_hash);
    outer.finish()
}

pub fn shift_for(key: &[u8], patient_key: &str) -> Shift {
    let message = format!("dbsloc-date-shift:{patient_key}");
    let mac = hmac_sha256(key, message.as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&mac[..8]);
    let value = u64::from_be_bytes(head) % 61;
    Shift {
        days: value as i32 - 30,
    }
}

fn shift_opt(value: &mut Option<String>, days: i32) {
    if let Some(shifted) = value.as_deref().and_then(|v| shift_days(v, days)) {
        *value = Some(shifted);
    }
}

fn shift_value(value: &mut Value, days: i32) {
    match value {
        Value::String(s) => {
            if let Some(shifted) = shift_days(s, days) {
                *s = shifted;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| shift_value(v, days)),
        Value::Object(map) => shift_map(map, days),
        _ => {}
    }
}

fn shift_map(map: &mut Map<String, Value>, days: i32) {
    map.values_mut().for_each(|v| shift_value(v, days));
}

fn shift_opt_value(value: &mut Option<Value>, days: i32) {
    if let Some(v) = value {
        shift_value(v, days);
    }
}

fn shift_group(group: &mut Group, days: i32) {
    shift_opt_value(&mut group.cycling, days);
    shift_opt_value(&mut group.soft_start_stop, days);
    shift_map(&mut group.extra, days);
    for side in [
        group.hemispheres.left.as_mut(),
        group.hemispheres.right.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        for program in &mut side.programs {
            shift_map(&mut program.extra, days);
            if let Some(sensing) = program.sensing.as_mut() {
                shift_map(&mut sensing.extra, days);
            }
        }
    }
}

fn compact_session(session_date: Option<&str>) -> String {
    let digits: String = session_date
        .map(|d| d.chars().filter(char::is_ascii_digit).collect())
        .unwrap_or_default();
    if digits.len() == 12 {
        digits
    } else if digits.len() == 8 {
        format!("{digits}0000")
    } else {
        "unknown".to_owned()
    }
}

pub fn anonymize(report: &SessionReport, shift: Shift) -> SessionReport {
    let days = shift.days;
    let mut r = report.clone();

    r.patient.name = None;
    r.patient.patient_id = None;
    r.patient.clinician_notes = None;
    r.patient.extra.clear();
    r.device.serial = None;
    r.device.extra.clear();
    r.sections.clear();

    shift_opt(&mut r.patient.date_of_birth, days);
    shift_opt(&mut r.device.implanted, days);
    shift_opt(&mut r.session.date, days);
    shift_opt(&mut r.source.printed_at, days);
    shift_map(&mut r.session.extra, days);

    for lead in &mut r.leads {
        shift_map(&mut lead.extra, days);
    }
    for snapshot in [&mut r.snapshots.initial, &mut r.snapshots.final_] {
        for group in &mut snapshot.groups {
            shift_group(group, days);
        }
    }
    for entry in &mut r.history {
        shift_opt(&mut entry.date, days);
        for group in &mut entry.groups {
            shift_group(group, days);
        }
    }
    for imp in &mut r.impedances {
        shift_map(&mut imp.extra, days);
    }
    for event in &mut r.events {
        shift_opt(&mut event.date, days);
        shift_map(&mut event.extra, days);
    }
    for note in &mut r.annotations {
        shift_opt(&mut note.date, days);
        shift_opt_value(&mut note.program, days);
        shift_map(&mut note.extra, days);
    }
    r.sensing
        .surveys
        .iter_mut()
        .for_each(|v| shift_value(v, days));
    r.sensing
        .streaming
        .iter_mut()
        .for_each(|v| shift_value(v, days));
    r.sensing
        .timeline
        .iter_mut()
        .for_each(|v| shift_value(v, days));
    shift_map(&mut r.sensing.extra, days);

    r.anonymized = true;
    r.anonymization = Some(Anonymization {
        method: "date-shift".to_owned(),
        range_days: SHIFT_RANGE_DAYS,
    });
    r.source.filename = format!(
        "{}_{}_{}.pdf",
        r.vendor.as_str(),
        r.report_kind,
        compact_session(r.session.date.as_deref())
    );
    r
}

pub fn free_text_fields(report: &SessionReport) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut group_names = |prefix: &str, groups: &[Group]| {
        for (i, g) in groups.iter().enumerate() {
            if let Some(name) = g.name.as_deref().filter(|n| !n.trim().is_empty()) {
                out.push((format!("{prefix}[{i}].name"), name.to_owned()));
            }
        }
    };
    group_names("snapshots.initial.groups", &report.snapshots.initial.groups);
    group_names("snapshots.final.groups", &report.snapshots.final_.groups);
    for (h, entry) in report.history.iter().enumerate() {
        group_names(&format!("history[{h}].groups"), &entry.groups);
    }
    for (i, e) in report.events.iter().enumerate() {
        if let Some(name) = e.name.as_deref().filter(|n| !n.trim().is_empty()) {
            out.push((format!("events[{i}].name"), name.to_owned()));
        }
    }
    for (i, a) in report.annotations.iter().enumerate() {
        for (j, s) in a.symptoms.iter().enumerate() {
            if !s.name.trim().is_empty() {
                out.push((
                    format!("annotations[{i}].symptoms[{j}].name"),
                    s.name.clone(),
                ));
            }
        }
    }
    out
}

pub fn identifying_strings(report: &SessionReport) -> Vec<String> {
    [
        report.patient.name.as_deref(),
        report.patient.patient_id.as_deref(),
        report.device.serial.as_deref(),
        report.patient.clinician_notes.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::age_years;
    use crate::model::*;
    use fastwarp_core::sha256::hex;
    use serde_json::json;

    fn group(name: &str) -> Group {
        let mut extra = Map::new();
        extra.insert("changed".to_owned(), json!("2026-09-01T10:12"));
        Group {
            id: "A".to_owned(),
            name: Some(name.to_owned()),
            active: true,
            hemispheres: Hemispheres {
                left: Some(HemisphereSettings {
                    programs: vec![Program {
                        index: 1,
                        mode: Some(StimMode::Current),
                        amplitude: Some(2.0),
                        unit: Some("mA".to_owned()),
                        pulse_width_us: Some(60.0),
                        frequency_hz: Some(130.0),
                        limits: Limits::default(),
                        case: Some(Polarity::Anode),
                        contacts: Vec::new(),
                        contact_sum_matches_amplitude: None,
                        sensing: Some(Sensing {
                            extra: {
                                let mut m = Map::new();
                                m.insert("when".to_owned(), json!(["2026-09-02"]));
                                m
                            },
                            ..Sensing::default()
                        }),
                        extra: Map::new(),
                    }],
                }),
                right: None,
            },
            cycling: Some(json!({"since": "2026-08-30T00:00:00Z"})),
            soft_start_stop: None,
            extra,
        }
    }

    fn sample() -> SessionReport {
        let mut r = SessionReport::blank(Vendor::Medtronic, "percept_session_report");
        r.source.filename = "Report_Jane_Roe.pdf".to_owned();
        r.source.printed_at = Some("2026-09-30T09:05".to_owned());
        r.patient.name = Some("Jane Roe".to_owned());
        r.patient.patient_id = Some("MRN:777123".to_owned());
        r.patient.clinician_notes = Some("Roe likes tea".to_owned());
        r.patient.sex = Some(Sex::Female);
        r.patient.diagnosis = Some("Parkinson's disease".to_owned());
        r.patient.date_of_birth = Some("1959-06-25".to_owned());
        r.patient.extra.insert("x".to_owned(), json!("Roe"));
        r.device.serial = Some("SN-ROE-42".to_owned());
        r.device.implanted = Some("2026-03-04".to_owned());
        r.device.extra.insert("eri".to_owned(), json!("2041-02-28"));
        r.session.date = Some("2026-09-30T08:51".to_owned());
        r.session.extra.insert(
            "note".to_owned(),
            json!({"a": ["2026-09-30T08:51", "text"]}),
        );
        r.snapshots.final_.groups.push(group("Walking"));
        r.snapshots.initial.groups.push(group("Start"));
        r.history.push(HistoryEntry {
            date: Some("2026-09-01T10:12".to_owned()),
            groups: vec![group("Old")],
        });
        r.events.push(Event {
            date: Some("2026-09-12T18:30".to_owned()),
            name: Some("Tremor".to_owned()),
            count: Some(3),
            hemisphere: None,
            extra: Map::new(),
        });
        r.annotations.push(Annotation {
            date: Some("2026-03-30".to_owned()),
            hemisphere: Some(Hemisphere::Left),
            frequency_hz: Some(130.0),
            mode: None,
            program: None,
            symptoms: vec![SymptomScore {
                name: "rigidity".to_owned(),
                score: Some(2.0),
            }],
            extra: Map::new(),
        });
        r.sensing
            .surveys
            .push(json!({"date": "2026-09-30T08:00:00Z"}));
        r.sections
            .insert("pi".to_owned(), json!("Jane Roe\nMRN:777123"));
        r
    }

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hmac_long_key_is_hashed_first() {
        let key = [0xaau8; 131];
        let mac = hmac_sha256(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn shift_is_deterministic_and_in_range() {
        let key = [7u8; 32];
        let a = shift_for(&key, "sub-001");
        assert_eq!(a, shift_for(&key, "sub-001"));
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..200 {
            let s = shift_for(&key, &format!("sub-{i:03}"));
            assert!((-30..=30).contains(&s.days));
            seen.insert(s.days);
        }
        assert!(seen.len() > 20);
        assert_ne!(shift_for(&key, "sub-001"), shift_for(&key, "sub-002"));
        assert_ne!(shift_for(&key, "sub-001"), shift_for(&[8u8; 32], "sub-001"));
    }

    #[test]
    fn every_date_moves_by_the_same_offset() {
        let r = sample();
        let a = anonymize(&r, Shift { days: 30 });
        assert_eq!(a.patient.date_of_birth.as_deref(), Some("1959-07-25"));
        assert_eq!(a.device.implanted.as_deref(), Some("2026-04-03"));
        assert_eq!(a.session.date.as_deref(), Some("2026-10-30T08:51"));
        assert_eq!(a.source.printed_at.as_deref(), Some("2026-10-30T09:05"));
        assert_eq!(a.history[0].date.as_deref(), Some("2026-10-01T10:12"));
        assert_eq!(a.events[0].date.as_deref(), Some("2026-10-12T18:30"));
        assert_eq!(a.annotations[0].date.as_deref(), Some("2026-04-29"));
        assert_eq!(a.session.extra["note"]["a"][0], json!("2026-10-30T08:51"));
        assert_eq!(a.session.extra["note"]["a"][1], json!("text"));
        assert_eq!(a.sensing.surveys[0]["date"], json!("2026-10-30T08:00:00Z"));
        let g = &a.snapshots.final_.groups[0];
        assert_eq!(g.extra["changed"], json!("2026-10-01T10:12"));
        assert_eq!(
            g.cycling.as_ref().expect("cycling")["since"],
            json!("2026-09-29T00:00:00Z")
        );
        let sensing = g.hemispheres.left.as_ref().expect("left").programs[0]
            .sensing
            .as_ref()
            .expect("sensing");
        assert_eq!(sensing.extra["when"][0], json!("2026-10-02"));
        assert_eq!(
            a.history[0].groups[0].extra["changed"],
            json!("2026-10-01T10:12")
        );
    }

    #[test]
    fn identity_is_gone_and_age_kept() {
        let r = sample();
        let ids = identifying_strings(&r);
        assert_eq!(ids.len(), 4);
        let a = anonymize(&r, Shift { days: -17 });
        let text = crate::model::canonical_json(&a).expect("json");
        for s in ids.iter().chain(["Roe".to_owned()].iter()) {
            assert!(!text.contains(s.as_str()), "{s}");
        }
        assert!(a.anonymized);
        assert_eq!(
            a.anonymization,
            Some(Anonymization {
                method: "date-shift".to_owned(),
                range_days: 30
            })
        );
        assert!(a.sections.is_empty());
        assert!(a.patient.extra.is_empty() && a.device.extra.is_empty());
        let before = age_years(
            r.patient.date_of_birth.as_deref().expect("dob"),
            r.session.date.as_deref().expect("date"),
        );
        let after = age_years(
            a.patient.date_of_birth.as_deref().expect("dob"),
            a.session.date.as_deref().expect("date"),
        );
        assert_eq!(before, after);
        assert_eq!(a.patient.sex, Some(Sex::Female));
        assert_eq!(a.patient.diagnosis.as_deref(), Some("Parkinson's disease"));
        assert_eq!(
            a.source.filename,
            "medtronic_percept_session_report_202609130851.pdf"
        );
    }

    #[test]
    fn zero_shift_still_removes_identity() {
        let r = sample();
        let a = anonymize(&r, Shift { days: 0 });
        assert_eq!(a.session.date, r.session.date);
        assert!(identifying_strings(&a).is_empty());
        assert!(a.patient.name.is_none() && a.device.serial.is_none());
        assert_eq!(
            a.source.filename,
            "medtronic_percept_session_report_202609300851.pdf"
        );
    }

    #[test]
    fn leap_day_shift() {
        let mut r = sample();
        r.session.date = Some("2028-02-29T10:00".to_owned());
        r.patient.date_of_birth = Some("1990-02-01".to_owned());
        let a = anonymize(&r, Shift { days: 30 });
        assert_eq!(a.session.date.as_deref(), Some("2028-03-30T10:00"));
        assert_eq!(a.patient.date_of_birth.as_deref(), Some("1990-03-03"));
    }

    #[test]
    fn free_text_is_listed() {
        let r = sample();
        let fields = free_text_fields(&r);
        let paths: Vec<&str> = fields.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"snapshots.final.groups[0].name"));
        assert!(paths.contains(&"snapshots.initial.groups[0].name"));
        assert!(paths.contains(&"history[0].groups[0].name"));
        assert!(paths.contains(&"events[0].name"));
        assert!(paths.contains(&"annotations[0].symptoms[0].name"));
        assert_eq!(fields.len(), 5);
    }
}
