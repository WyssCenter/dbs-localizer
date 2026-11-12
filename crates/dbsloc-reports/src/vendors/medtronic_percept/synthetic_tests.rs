// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::model::{canonical_json, FieldStatus, SessionReport, Severity};
use crate::pdf::{read_document, PdfDocument};
use crate::test_support::synthetic_dir;
use crate::vendors::{ParseOptions, VendorModule};

use super::MedtronicPercept;

const SLUGS: [&str; 2] = ["ring_percept_pc", "directional_percept_rc"];
const SECTIONS: [&str; 13] = [
    "pi", "di", "ss", "pps", "gs", "sii", "gh", "gu", "sa", "bss", "mbss", "pe", "es",
];

struct Case {
    slug: &'static str,
    report: SessionReport,
    value: Value,
    expected: Value,
    doc: PdfDocument,
}

fn load(slug: &'static str) -> Case {
    let pdf = synthetic_dir().join(format!("{slug}.pdf"));
    let (report, doc) = crate::parse_report_with_document(&pdf, &ParseOptions::default())
        .unwrap_or_else(|e| panic!("{slug}: {e}"));
    let expected_path = synthetic_dir().join(format!("{slug}.expected.json"));
    let text = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("{}: {e}", expected_path.display()));
    Case {
        slug,
        value: serde_json::to_value(&report).expect("report value"),
        report,
        expected: serde_json::from_str(&text).expect("expected json"),
        doc,
    }
}

fn cases() -> Vec<Case> {
    SLUGS.into_iter().map(load).collect()
}

fn same(expected: &Value, actual: &Value, path: &str) {
    match (expected, actual) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().expect("number"), b.as_f64().expect("number"));
            assert!((a - b).abs() < 1e-9, "{path}: expected {a}, got {b}");
        }
        (Value::Object(a), Value::Object(b)) => {
            let ka: Vec<&String> = a.keys().collect();
            let kb: Vec<&String> = b.keys().collect();
            assert_eq!(
                ka.iter().collect::<std::collections::BTreeSet<_>>(),
                kb.iter().collect::<std::collections::BTreeSet<_>>(),
                "{path}: keys"
            );
            for (k, v) in a {
                same(v, &b[k], &format!("{path}.{k}"));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path}: length");
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                same(x, y, &format!("{path}[{i}]"));
            }
        }
        (a, b) => assert_eq!(a, b, "{path}"),
    }
}

fn at<'v>(v: &'v Value, pointer: &str) -> &'v Value {
    v.pointer(pointer)
        .unwrap_or_else(|| panic!("missing {pointer}"))
}

fn arr<'v>(v: &'v Value, pointer: &str) -> &'v [Value] {
    at(v, pointer).as_array().expect("array")
}

fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut m = Map::new();
    for k in keys {
        m.insert((*k).to_owned(), v.get(*k).cloned().unwrap_or(Value::Null));
    }
    Value::Object(m)
}

#[test]
fn both_reports_are_detected_and_parse_without_errors() {
    for c in cases() {
        assert!(MedtronicPercept.detect(&c.doc), "{}", c.slug);
        assert!(
            !c.report.has_errors(),
            "{}: {:?}",
            c.slug,
            c.report.diagnostics
        );
        assert_eq!(c.report.vendor.as_str(), "medtronic");
        assert_eq!(c.report.report_kind, "percept_session_report");
        assert_eq!(
            u64::from(c.report.source.pages),
            at(&c.expected, "/pages").as_u64().expect("pages"),
            "{}",
            c.slug
        );
        assert_eq!(c.report.source.sha256.len(), 64);
        assert!(c
            .report
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error && !d.code.starts_with("W_REPORT_section")));
        for p in &c.doc.pages {
            assert_eq!(p.unmapped_glyphs, 0, "{} page {}", c.slug, p.number);
        }
    }
}

#[test]
fn diagnostics_follow_the_printed_values() {
    for c in cases() {
        let count = |code: &str| {
            c.report
                .diagnostics
                .iter()
                .filter(|d| d.code == code)
                .count() as u64
        };
        let want = |key: &str| {
            at(&c.expected, &format!("/diagnostics/{key}"))
                .as_u64()
                .expect("count")
        };
        assert_eq!(count("W_REPORT_contact_sum"), want("contact_sum_warnings"));
        assert_eq!(
            count("W_REPORT_interleaving"),
            want("interleaved_hemispheres")
        );
        assert_eq!(count("W_REPORT_history_lead"), 1);
        let ambiguous = c
            .report
            .fields
            .iter()
            .filter(|f| f.status == FieldStatus::Ambiguous)
            .count() as u64;
        assert_eq!(ambiguous, want("voltage_programs"), "{}", c.slug);
        let missing: Vec<&str> = c
            .report
            .fields
            .iter()
            .filter(|f| f.status == FieldStatus::Missing)
            .map(|f| f.path.as_str())
            .collect();
        assert!(missing.is_empty(), "{}: {missing:?}", c.slug);
        for f in &c.report.fields {
            assert!(!f.path.is_empty());
            assert!(f.rule.as_deref().is_some_and(|r| r.starts_with("mdt.")));
        }
    }
}

#[test]
fn canonical_json_round_trips() {
    for c in cases() {
        let text = canonical_json(&c.report).expect("json");
        let back: SessionReport = serde_json::from_str(&text).expect("parse back");
        assert_eq!(back, c.report, "{}", c.slug);
    }
}

#[test]
fn patient_session_and_device_match_the_generator() {
    for c in cases() {
        let v = &c.value;
        let e = &c.expected;
        same(
            at(e, "/patient"),
            &pick(
                at(v, "/patient"),
                &[
                    "name",
                    "patient_id",
                    "diagnosis",
                    "sex",
                    "date_of_birth",
                    "clinician_notes",
                ],
            ),
            &format!("{} patient", c.slug),
        );
        let session = json!({
            "date": at(v, "/session/date"),
            "printed_at": at(v, "/source/printed_at"),
            "programmer_version": at(v, "/device/programmer_version"),
            "stimulation_initial": at(v, "/session/stimulation_initial"),
            "stimulation_final": at(v, "/session/stimulation_final"),
            "battery_reminder": at(v, "/session/battery_reminder"),
            "close_communication_required": at(v, "/session/close_communication_required"),
        });
        same(at(e, "/session"), &session, &format!("{} session", c.slug));
        let mut device = pick(
            at(v, "/device"),
            &[
                "model",
                "rechargeable",
                "serial",
                "model_number",
                "implanted",
                "location",
                "battery_percent",
                "feature_code",
                "firmware",
                "battery_estimate",
            ],
        );
        let extra = at(v, "/device/extra");
        if let Value::Object(m) = &mut device {
            m.insert(
                "device_name".to_owned(),
                extra.get("device_name").cloned().unwrap_or(Value::Null),
            );
            m.insert(
                "eri_date".to_owned(),
                extra.get("eri_date").cloned().unwrap_or(Value::Null),
            );
        }
        same(at(e, "/device"), &device, &format!("{} device", c.slug));
    }
}

#[test]
fn leads_match_the_generator() {
    for c in cases() {
        let actual: Vec<Value> = arr(&c.value, "/leads")
            .iter()
            .map(|l| {
                let labels: Vec<Value> = arr(l, "/contacts")
                    .iter()
                    .map(|k| at(k, "/label").clone())
                    .collect();
                for (i, k) in arr(l, "/contacts").iter().enumerate() {
                    assert_eq!(at(k, "/k").as_u64(), Some(i as u64 + 1));
                }
                let mut v = pick(
                    l,
                    &[
                        "hemisphere",
                        "vendor_model",
                        "target",
                        "tip_offset",
                        "orientation_deg",
                    ],
                );
                if let Value::Object(m) = &mut v {
                    m.insert(
                        "last_contact".to_owned(),
                        at(l, "/extra/last_contact").clone(),
                    );
                    m.insert("contacts".to_owned(), Value::Array(labels));
                }
                v
            })
            .collect();
        same(at(&c.expected, "/leads"), &Value::Array(actual), c.slug);
    }
}

fn program_view(p: &Value, annotation: bool, expected: &Value) -> Value {
    let contacts = arr(p, "/contacts");
    let labels = |polarity: &str| -> Vec<Value> {
        contacts
            .iter()
            .filter(|k| at(k, "/polarity").as_str() == Some(polarity))
            .map(|k| at(k, "/label").clone())
            .collect()
    };
    let mut currents = Map::new();
    for k in contacts {
        if !at(k, "/amplitude").is_null() {
            currents.insert(
                at(k, "/label").as_str().expect("label").to_owned(),
                at(k, "/amplitude").clone(),
            );
        }
    }
    let sensing = p.get("sensing").filter(|s| !s.is_null());
    let mut v = json!({
        "amplitude": at(p, "/amplitude"),
        "unit": at(p, "/unit"),
        "pulse_width_us": at(p, "/pulse_width_us"),
        "frequency_hz": at(p, "/frequency_hz"),
        "cathodes": labels("cathode"),
        "anodes": labels("anode"),
        "currents": currents,
        "sensing": sensing.map_or(json!([]), |s| at(s, "/contacts").clone()),
        "bs_mode": sensing.map_or(Value::Null, |s| at(s, "/mode").clone()),
        "bs_status": sensing.map_or(Value::Null, |s| at(s, "/status").clone()),
    });
    let Value::Object(m) = &mut v else {
        unreachable!()
    };
    if !annotation {
        m.insert("case".to_owned(), at(p, "/case").clone());
    }
    if expected.get("adaptive").is_some() {
        let s = sensing.expect("sensing block");
        let mut a = Map::new();
        a.insert("lfp_hz".to_owned(), at(s, "/lfp_hz").clone());
        a.insert("limits".to_owned(), at(s, "/limits").clone());
        for (k, x) in at(s, "/extra").as_object().expect("extra") {
            a.insert(k.clone(), x.clone());
        }
        m.insert("adaptive".to_owned(), Value::Object(a));
    }
    v
}

fn with_defaults(expected: &Value) -> Value {
    let mut e = expected.clone();
    if let Value::Object(m) = &mut e {
        m.entry("bs_mode").or_insert(Value::Null);
        m.entry("bs_status").or_insert(Value::Null);
    }
    e
}

fn hemispheres_view(g: &Value, expected: &Value) -> Value {
    let mut out = Map::new();
    for side in ["left", "right"] {
        let want = arr(expected, &format!("/hemispheres/{side}"));
        let programs = at(g, &format!("/hemispheres/{side}/programs"))
            .as_array()
            .expect("programs");
        let views: Vec<Value> = programs
            .iter()
            .zip(want)
            .map(|(p, w)| program_view(p, false, w))
            .collect();
        assert_eq!(programs.len(), want.len(), "program count {side}");
        out.insert(side.to_owned(), Value::Array(views));
    }
    Value::Object(out)
}

fn group_view(g: &Value, expected: &Value) -> Value {
    let mut m = Map::new();
    m.insert("id".to_owned(), at(g, "/id").clone());
    m.insert("hemispheres".to_owned(), hemispheres_view(g, expected));
    for key in ["name", "active", "soft_start_stop", "cycling"] {
        if expected.get(key).is_some() {
            m.insert(key.to_owned(), at(g, &format!("/{key}")).clone());
        }
    }
    if expected.get("sensing_settings").is_some() {
        m.insert(
            "sensing_settings".to_owned(),
            at(g, "/extra/sensing_settings").clone(),
        );
    }
    if let Some(want) = expected.get("programmer") {
        let mode = at(want, "/mode");
        let mut programmer = Map::new();
        programmer.insert("mode".to_owned(), mode.clone());
        for side in ["left", "right"] {
            let extra = at(g, &format!("/extra/patient_programmer/{side}"));
            assert_eq!(at(extra, "/mode"), mode, "programmer mode {side}");
            let limits = at(g, &format!("/hemispheres/{side}/programs/0/limits"));
            let Some(side_want) = want.get(side) else {
                assert!(at(limits, "/lower").is_null() && at(limits, "/upper").is_null());
                assert!(extra.get("value_ma").is_none());
                continue;
            };
            let mut got = Map::new();
            for k in side_want.as_object().expect("side").keys() {
                let value = match k.as_str() {
                    "lower" | "upper" => at(limits, &format!("/{k}")).clone(),
                    _ => extra.get(k).cloned().unwrap_or(Value::Null),
                };
                got.insert(k.clone(), value);
            }
            programmer.insert(side.to_owned(), Value::Object(got));
        }
        m.insert("programmer".to_owned(), Value::Object(programmer));
    }
    Value::Object(m)
}

#[test]
fn stimulation_snapshots_match_the_generator() {
    for c in cases() {
        for snap in ["initial", "final"] {
            let want = at(&c.expected, &format!("/snapshots/{snap}"));
            let got = at(&c.value, &format!("/snapshots/{snap}"));
            assert_eq!(
                at(got, "/active_group"),
                at(want, "/active"),
                "{} {snap} active group",
                c.slug
            );
            let want_groups = arr(want, "/groups");
            let got_groups = arr(got, "/groups");
            assert_eq!(got_groups.len(), want_groups.len());
            for (g, w) in got_groups.iter().zip(want_groups) {
                let ctx = format!("{} {snap} group {}", c.slug, at(w, "/id"));
                let normalised = {
                    let mut w = w.clone();
                    for side in ["left", "right"] {
                        if let Some(Value::Array(list)) =
                            w.pointer_mut(&format!("/hemispheres/{side}"))
                        {
                            for p in list.iter_mut() {
                                *p = with_defaults(p);
                            }
                        }
                    }
                    w
                };
                same(&normalised, &group_view(g, w), &ctx);
            }
        }
        let initial = at(&c.expected, "/snapshots/initial/stimulation");
        let fin = at(&c.expected, "/snapshots/final/stimulation");
        assert_eq!(at(&c.value, "/session/stimulation_initial"), initial);
        assert_eq!(at(&c.value, "/session/stimulation_final"), fin);
    }
}

#[test]
fn group_history_matches_the_generator() {
    for c in cases() {
        let want = arr(&c.expected, "/history");
        let got = arr(&c.value, "/history");
        assert_eq!(got.len(), want.len(), "{}", c.slug);
        for (i, (h, w)) in got.iter().zip(want).enumerate() {
            assert_eq!(at(h, "/date"), at(w, "/date"), "{} history {i}", c.slug);
            let want_groups = arr(w, "/groups");
            let got_groups = arr(h, "/groups");
            assert_eq!(got_groups.len(), want_groups.len());
            for (g, wg) in got_groups.iter().zip(want_groups) {
                let mut wg = wg.clone();
                for side in ["left", "right"] {
                    if let Some(Value::Array(list)) =
                        wg.pointer_mut(&format!("/hemispheres/{side}"))
                    {
                        for p in list.iter_mut() {
                            *p = with_defaults(p);
                        }
                    }
                }
                let ctx = format!("{} history {i} group {}", c.slug, at(&wg, "/id"));
                same(&wg, &group_view(g, &wg), &ctx);
            }
        }
    }
}

fn impedance_key(v: &Value) -> String {
    format!(
        "{}/{}/{}/{}",
        at(v, "/hemisphere").as_str().expect("hemisphere"),
        at(v, "/kind").as_str().expect("kind"),
        at(v, "/b").as_str().expect("b"),
        at(v, "/a").as_str().expect("a"),
    )
}

#[test]
fn impedances_match_the_generator() {
    for c in cases() {
        let mut want: BTreeMap<String, Value> = BTreeMap::new();
        for e in arr(&c.expected, "/impedances") {
            want.insert(impedance_key(e), e.clone());
        }
        let mut got: BTreeMap<String, Value> = BTreeMap::new();
        for i in arr(&c.value, "/impedances") {
            assert_eq!(at(i, "/status").as_str(), Some("ok"));
            let mut v = pick(i, &["hemisphere", "kind", "a", "b", "ohm"]);
            if let Value::Object(m) = &mut v {
                m.insert(
                    "bound".to_owned(),
                    i.pointer("/extra/bound").cloned().unwrap_or(Value::Null),
                );
            }
            got.insert(impedance_key(i), v);
        }
        assert_eq!(got.len(), want.len(), "{}", c.slug);
        let lead_cells = if c.slug == "ring_percept_pc" {
            2 * (4 + 6)
        } else {
            2 * (8 + 28)
        };
        assert_eq!(got.len(), lead_cells, "{}", c.slug);
        for (k, w) in &want {
            same(
                w,
                got.get(k)
                    .unwrap_or_else(|| panic!("{} missing {k}", c.slug)),
                k,
            );
        }
    }
}

#[test]
fn annotations_match_the_generator() {
    for c in cases() {
        let want = arr(&c.expected, "/annotations");
        let got = arr(&c.value, "/annotations");
        assert_eq!(got.len(), want.len(), "{}", c.slug);
        for (i, (a, w)) in got.iter().zip(want).enumerate() {
            let mut symptoms = Map::new();
            for s in arr(a, "/symptoms") {
                symptoms.insert(
                    at(s, "/name").as_str().expect("name").to_owned(),
                    at(s, "/score").clone(),
                );
            }
            let view = json!({
                "date": at(a, "/date"),
                "hemisphere": at(a, "/hemisphere"),
                "program": program_view(at(a, "/program"), true, at(w, "/program")),
                "symptoms": symptoms,
            });
            let mut w = w.clone();
            if let Some(p) = w.pointer_mut("/program") {
                *p = with_defaults(p);
            }
            same(&w, &view, &format!("{} annotation {i}", c.slug));
        }
    }
}

#[test]
fn usage_events_survey_recharge_and_summary_match_the_generator() {
    for c in cases() {
        let v = &c.value;
        let e = &c.expected;
        let ids: Vec<Value> = arr(v, "/group_usage")
            .iter()
            .map(|g| at(g, "/id").clone())
            .collect();
        let usage = json!({
            "groups": ids,
            "months": at(v, "/session/extra/group_usage_monthly"),
        });
        same(at(e, "/group_usage"), &usage, &format!("{} usage", c.slug));
        let surveys: Vec<Value> = arr(v, "/sensing/surveys")
            .iter()
            .map(|s| {
                pick(
                    s,
                    &["electrodes", "peak_magnitude_uvp", "peak_frequency_hz"],
                )
            })
            .collect();
        same(
            at(e, "/survey"),
            &Value::Array(surveys),
            &format!("{} survey", c.slug),
        );
        match at(e, "/identifier") {
            Value::Null => assert!(v.pointer("/sensing/extra/identifier").is_none()),
            want => {
                let got = at(v, "/sensing/extra/identifier/0");
                same(
                    want,
                    &pick(
                        got,
                        &["detected_frequency_hz", "selected_frequency_hz", "ranks"],
                    ),
                    &format!("{} identifier", c.slug),
                );
            }
        }
        same(
            at(e, "/patient_events"),
            at(v, "/session/extra/patient_events"),
            &format!("{} patient events", c.slug),
        );
        let events: Vec<Value> = arr(v, "/events")
            .iter()
            .map(|x| pick(x, &["name", "count"]))
            .collect();
        same(
            at(e, "/event_summary/events"),
            &Value::Array(events),
            &format!("{} events", c.slug),
        );
        let mut lfp = Map::new();
        for s in arr(v, "/sensing/extra/lfp_amplitude_summary") {
            assert_eq!(at(s, "/group").as_str(), Some("D"));
            lfp.insert(
                at(s, "/hemisphere").as_str().expect("side").to_owned(),
                json!({
                    "above": at(s, "/above_threshold_percent"),
                    "between": at(s, "/between_thresholds_percent"),
                    "below": at(s, "/below_threshold_percent"),
                    "average": at(s, "/average_amplitude_ma"),
                    "reduction": at(s, "/adaptive_amplitude_reduction_percent"),
                    "running": at(s, "/time_adaptive_running_percent"),
                }),
            );
        }
        same(
            at(e, "/event_summary/lfp"),
            &Value::Object(lfp),
            &format!("{} lfp summary", c.slug),
        );
        match at(e, "/recharge") {
            Value::Null => assert!(v.pointer("/device/extra/recharge").is_none()),
            want => same(
                want,
                at(v, "/device/extra/recharge"),
                &format!("{} recharge", c.slug),
            ),
        }
    }
}

#[test]
fn sections_are_kept_only_on_request() {
    for c in cases() {
        for id in SECTIONS {
            assert!(
                c.report.sections.contains_key(id),
                "{}: section {id}",
                c.slug
            );
        }
        let rc = c.report.device.rechargeable == Some(true);
        assert_eq!(c.report.sections.contains_key("rc"), rc, "{}", c.slug);
        let r = MedtronicPercept
            .parse(
                &c.doc,
                &ParseOptions {
                    keep_sections: false,
                },
            )
            .expect("parse");
        assert!(r.sections.is_empty());
        assert_eq!(r.snapshots, c.report.snapshots);
    }
}

#[test]
fn dropping_any_page_never_panics() {
    let doc = read_document(Path::new(&synthetic_dir().join("ring_percept_pc.pdf"))).expect("pdf");
    for skip in 0..doc.pages.len() {
        let mut pages = doc.pages.clone();
        pages.remove(skip);
        let cut = PdfDocument { pages };
        if let Ok(r) = MedtronicPercept.parse(&cut, &ParseOptions::default()) {
            assert!(canonical_json(&r).is_ok());
        }
    }
    let mut empty_shapes = doc.clone();
    for p in &mut empty_shapes.pages {
        p.shapes.clear();
    }
    assert!(MedtronicPercept
        .parse(&empty_shapes, &ParseOptions::default())
        .is_err());
}
