// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::Value;

use crate::dates;
use crate::model::{
    canonical_json, Group, Hemisphere, ImpedanceKind, Polarity, Program, SessionReport, Severity,
    Sex,
};
use crate::pdf::{Glyph, PageContent, PdfDocument};
use crate::test_support::real_fixture_dir;
use crate::vendors::{ParseOptions, VendorModule};

use super::vendor_json::{
    self as vj, arr, def, electrode_label, local_date, local_minute, num, st, JsonProgram,
};
use super::MedtronicPercept;

struct Fixture {
    pdf: String,
    report: SessionReport,
    doc: PdfDocument,
    json: Value,
}

fn load_fixtures() -> Vec<Fixture> {
    vj::pairing()
        .into_iter()
        .map(|(pdf, json)| {
            let path = vj::fixture_dir().join(&pdf);
            let (report, doc) = crate::parse_report_with_document(&path, &ParseOptions::default())
                .unwrap_or_else(|e| panic!("{pdf}: {e}"));
            Fixture {
                pdf,
                report,
                doc,
                json: vj::load(&json),
            }
        })
        .collect()
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(p) => p
            .downcast_ref::<&str>()
            .map_or_else(|| "panic".to_owned(), |s| (*s).to_owned()),
    }
}

fn close(a: Option<f64>, b: Option<f64>, tol: f64) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => (x - y).abs() <= tol,
        (None, None) => true,
        _ => false,
    }
}

fn hemisphere_word(h: Hemisphere) -> &'static str {
    match h {
        Hemisphere::Left => "Left",
        Hemisphere::Right => "Right",
    }
}

fn first_contact(report: &SessionReport, h: Hemisphere) -> u32 {
    report
        .leads
        .iter()
        .find(|l| l.hemisphere == h)
        .and_then(|l| l.tip_offset)
        .expect("lead tip offset") as u32
}

fn group_letter(id: &str) -> String {
    def(id).trim_start_matches("GROUP_").to_owned()
}

fn level_of(label: &str, first: u32) -> u32 {
    let digits: String = label.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u32>().expect("label digits") - first
}

#[derive(Default)]
struct Tally {
    programs: usize,
    brainsense: usize,
    errors: Vec<String>,
}

impl Tally {
    fn check(&mut self, ok: bool, what: impl FnOnce() -> String) {
        if !ok {
            self.errors.push(what());
        }
    }

    fn finish(self, what: &str) {
        assert!(
            self.errors.is_empty(),
            "{what}: {} mismatches:\n{}",
            self.errors.len(),
            self.errors.join("\n")
        );
    }
}

fn compare_program(
    t: &mut Tally,
    ctx: &str,
    ours: &Program,
    theirs: &JsonProgram,
    first: u32,
    currents: bool,
) {
    t.programs += 1;
    t.check(close(ours.amplitude, theirs.amplitude, 1e-6), || {
        format!(
            "{ctx}: amplitude {:?} vs {:?}",
            ours.amplitude, theirs.amplitude
        )
    });
    t.check(close(ours.pulse_width_us, theirs.pulse_width, 1e-6), || {
        format!(
            "{ctx}: pulse width {:?} vs {:?}",
            ours.pulse_width_us, theirs.pulse_width
        )
    });
    t.check(close(ours.frequency_hz, theirs.rate, 1e-6), || {
        format!("{ctx}: rate {:?} vs {:?}", ours.frequency_hz, theirs.rate)
    });
    for (label, _, _) in &theirs.states {
        t.check(ours.contacts.iter().any(|c| &c.label == label), || {
            format!("{ctx}: vendor contact {label} not on the parsed lead")
        });
    }
    for c in &ours.contacts {
        let state = theirs
            .states
            .iter()
            .find(|s| s.0.eq_ignore_ascii_case(&c.label));
        let expected = match state.map(|s| s.1.as_str()) {
            Some("Negative") => Polarity::Cathode,
            Some("Positive") => Polarity::Anode,
            _ => Polarity::Off,
        };
        t.check(c.polarity == expected, || {
            format!(
                "{ctx}: contact {} polarity {:?} vs {:?}",
                c.label, c.polarity, expected
            )
        });
        if currents {
            if let Some(a) = c.amplitude {
                t.check(close(Some(a), state.and_then(|s| s.2), 1e-6), || {
                    format!(
                        "{ctx}: contact {} current {a} vs {:?}",
                        c.label,
                        state.and_then(|s| s.2)
                    )
                });
            }
        }
    }
    if currents {
        for (label, state, current) in &theirs.states {
            if state == "Negative" && current.is_some() {
                let ours_current = ours
                    .contacts
                    .iter()
                    .find(|c| &c.label == label)
                    .and_then(|c| c.amplitude);
                t.check(close(ours_current, *current, 1e-6), || {
                    format!("{ctx}: cathode {label} current {ours_current:?} vs {current:?}")
                });
            }
        }
    }
    t.check(
        (ours.case == Some(Polarity::Anode)) == theirs.case_positive,
        || {
            format!(
                "{ctx}: case {:?} vs vendor positive {}",
                ours.case, theirs.case_positive
            )
        },
    );
    let sensing_levels: Option<Vec<u32>> = ours.sensing.as_ref().map(|s| {
        let mut v: Vec<u32> = s.contacts.iter().map(|l| level_of(l, first)).collect();
        v.sort_unstable();
        v.dedup();
        v
    });
    match &theirs.channel {
        Some(levels) => t.check(sensing_levels.as_ref() == Some(levels), || {
            format!("{ctx}: sensing levels {sensing_levels:?} vs {levels:?}")
        }),
        None => t.check(sensing_levels.as_ref().is_none_or(|v| v.is_empty()), || {
            format!("{ctx}: sensing {sensing_levels:?} without a vendor channel")
        }),
    }
}

fn compare_brainsense(t: &mut Tally, ctx: &str, ours: &Program, channel: &Value) {
    t.brainsense += 1;
    let Some(s) = ours.sensing.as_ref() else {
        t.errors.push(format!("{ctx}: no sensing block"));
        return;
    };
    let status = def(st(channel, "AdaptiveTherapyStatus"));
    let mode = if status == "NOT_CONFIGURED" {
        "sensing_only"
    } else {
        "adaptive"
    };
    t.check(s.mode.as_deref() == Some(mode), || {
        format!("{ctx}: mode {:?} vs {mode}", s.mode)
    });
    let expected_status = match status {
        "RUNNING" => Some("adapting"),
        "SUSPENDED" => Some("paused"),
        _ => None,
    };
    t.check(s.status.as_deref() == expected_status, || {
        format!("{ctx}: status {:?} vs {expected_status:?}", s.status)
    });
    let setup = channel.get("SensingSetup").cloned().unwrap_or(Value::Null);
    t.check(
        close(s.lfp_hz, num(&setup, "FrequencyInHertz"), 1e-6),
        || format!("{ctx}: LFP frequency {:?}", s.lfp_hz),
    );
    let extra = |k: &str| s.extra.get(k).and_then(Value::as_f64);
    for (key, json_key) in [
        ("upper_lfp_threshold", "UpperLfpThreshold"),
        ("lower_lfp_threshold", "LowerLfpThreshold"),
        ("upper_lfp_captured", "MeasuredUpperLfp"),
        ("lower_lfp_captured", "MeasuredLowerLfp"),
        ("paused_amplitude_ma", "SuspendAmplitudeInMilliAmps"),
    ] {
        if mode == "sensing_only" && key == "paused_amplitude_ma" {
            continue;
        }
        t.check(close(extra(key), num(channel, json_key), 1e-6), || {
            format!(
                "{ctx}: {key} {:?} vs {:?}",
                extra(key),
                num(channel, json_key)
            )
        });
    }
    if mode == "adaptive" {
        for (key, json_key) in [
            ("transition_up_min", "TransitionUpInMilliSeconds"),
            ("transition_down_min", "TransitionDownInMilliSeconds"),
        ] {
            let expected = num(channel, json_key).map(|ms| (ms / 6000.0).round() / 10.0);
            t.check(close(extra(key), expected, 1e-6), || {
                format!("{ctx}: {key} {:?} vs {expected:?}", extra(key))
            });
        }
        let limits = s.limits.clone().unwrap_or_default();
        t.check(
            close(limits.upper, num(channel, "UpperLimitInMilliAmps"), 1e-6),
            || format!("{ctx}: upper limit {:?}", limits.upper),
        );
        t.check(
            close(limits.lower, num(channel, "LowerLimitInMilliAmps"), 1e-6),
            || format!("{ctx}: lower limit {:?}", limits.lower),
        );
    }
    if channel.get("Mode").is_some() {
        let printed = s.extra.get("threshold_mode").and_then(Value::as_str);
        t.check(
            def(st(channel, "Mode")) == "DUAL_THRESHOLD_DIRECT"
                && printed == Some("Dual Threshold"),
            || format!("{ctx}: threshold mode {printed:?}"),
        );
    }
}

fn hemisphere_programs(g: &Group, h: Hemisphere) -> &[Program] {
    match h {
        Hemisphere::Left => g.hemispheres.left.as_ref(),
        Hemisphere::Right => g.hemispheres.right.as_ref(),
    }
    .map_or(&[], |s| s.programs.as_slice())
}

fn compare_group(
    t: &mut Tally,
    ctx: &str,
    report: &SessionReport,
    ours: &Group,
    theirs: &Value,
    snapshot: bool,
) {
    for h in [Hemisphere::Left, Hemisphere::Right] {
        let first = first_contact(report, h);
        let jp = vj::programs(theirs, hemisphere_word(h), first);
        let op = hemisphere_programs(ours, h);
        t.check(jp.len() == op.len(), || {
            format!("{ctx} {h:?}: {} programs vs {} vendor", op.len(), jp.len())
        });
        let channels: Vec<&Value> = theirs
            .get("ProgramSettings")
            .map(|p| arr(p, "SensingChannel"))
            .unwrap_or(&[])
            .iter()
            .filter(|c| def(st(c, "HemisphereLocation")) == hemisphere_word(h))
            .collect();
        for (i, (o, j)) in op.iter().zip(jp.iter()).enumerate() {
            let pctx = format!("{ctx} {h:?} program {}", i + 1);
            compare_program(t, &pctx, o, j, first, snapshot);
            if snapshot {
                if let Some(ch) = channels.get(i) {
                    compare_brainsense(t, &pctx, o, ch);
                }
                if j.sensing.is_none() {
                    let key = format!("{}Hemisphere", hemisphere_word(h));
                    let vendor = theirs
                        .get("ProgramSettings")
                        .and_then(|p| p.get(&key))
                        .map(|x| arr(x, "Programs"))
                        .and_then(|p| p.get(i));
                    let lower = vendor.and_then(|v| num(v, "LowerLimitInMilliAmps"));
                    let upper = vendor.and_then(|v| num(v, "UpperLimitInMilliAmps"));
                    if o.limits.lower.is_some() || o.limits.upper.is_some() {
                        t.check(
                            close(o.limits.lower, lower, 1e-6)
                                && close(o.limits.upper, upper, 1e-6),
                            || format!("{pctx}: limits {:?} vs {lower:?}..{upper:?}", o.limits),
                        );
                    }
                }
            }
        }
    }
}

fn every_fixture_parses_without_errors(all: &[Fixture]) {
    assert_eq!(all.len(), 4);
    for f in all {
        assert!(
            !f.report.has_errors(),
            "{}: {:?}",
            f.pdf,
            f.report.diagnostics
        );
        assert_eq!(f.report.vendor.as_str(), "medtronic");
        assert_eq!(f.report.report_kind, "percept_session_report");
        assert!(!f.report.source.sha256.is_empty());
        let history_lead = f
            .report
            .diagnostics
            .iter()
            .filter(|d| d.code == "W_REPORT_history_lead")
            .count();
        assert_eq!(history_lead, 1, "{}", f.pdf);
        assert!(f
            .report
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error && !d.code.starts_with("W_REPORT_section")));
    }
    let sums: usize = all
        .iter()
        .map(|f| {
            f.report
                .diagnostics
                .iter()
                .filter(|d| d.code == "W_REPORT_contact_sum")
                .count()
        })
        .sum();
    assert_eq!(
        sums, 38,
        "16a §3.7: 38 multi-cathode programs do not sum to the amplitude"
    );
}

fn canonical_json_round_trips(all: &[Fixture]) {
    for f in all {
        let text = canonical_json(&f.report).expect("json");
        let back: SessionReport = serde_json::from_str(&text).expect("parse back");
        assert_eq!(back, f.report, "{}", f.pdf);
    }
}

fn walk_dates(v: &Value, key: &str, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(m) => m.iter().for_each(|(k, x)| walk_dates(x, k, out)),
        Value::Array(a) => a.iter().for_each(|x| walk_dates(x, key, out)),
        Value::String(s) => {
            let dated = [
                "date",
                "date_of_birth",
                "implanted",
                "printed_at",
                "eri_date",
                "period_start",
                "period_end",
                "month_start",
            ];
            if dated.contains(&key) {
                out.push((key.to_owned(), s.clone()));
            }
        }
        _ => {}
    }
}

fn fields_have_paths_and_dates_parse(all: &[Fixture]) {
    for f in all {
        assert!(!f.report.fields.is_empty());
        for field in &f.report.fields {
            assert!(!field.path.is_empty(), "{}: empty field path", f.pdf);
            assert!(
                field.rule.as_deref().is_some_and(|r| r.starts_with("mdt.")),
                "{}: {field:?}",
                f.pdf
            );
        }
        let value = serde_json::to_value(&f.report).expect("value");
        let mut found = Vec::new();
        walk_dates(&value, "", &mut found);
        assert!(found.len() > 50, "{}: {} dates", f.pdf, found.len());
        for (key, s) in found {
            let ok =
                dates::parse_local_date(&s).is_some() || dates::parse_local_datetime(&s).is_some();
            assert!(ok, "{}: {key} = {s:?}", f.pdf);
        }
        let parsed_at = f.report.source.parsed_at.as_deref().expect("parsed_at");
        assert!(dates::parse_instant(parsed_at).is_some());
        assert!(
            dates::parse_local_datetime(f.report.session.date.as_deref().expect("session"))
                .is_some()
        );
    }
}

fn patient_device_and_session_match_vendor_json(all: &[Fixture]) {
    for f in all {
        let ctx = &f.pdf;
        let j = &f.json;
        let pat = j.pointer("/PatientInformation/Final").expect("patient");
        let p = &f.report.patient;
        let name = format!(
            "{} {}",
            st(pat, "PatientFirstName"),
            st(pat, "PatientLastName")
        );
        assert_eq!(p.name.as_deref(), Some(name.as_str()), "{ctx}");
        assert_eq!(p.patient_id.as_deref(), Some(st(pat, "PatientId")), "{ctx}");
        let sex = match def(st(pat, "PatientGender")) {
            "MALE" => Some(Sex::Male),
            "FEMALE" => Some(Sex::Female),
            _ => None,
        };
        assert_eq!(p.sex, sex, "{ctx}");
        assert_eq!(
            p.date_of_birth.as_deref(),
            Some(local_date(st(pat, "PatientDateOfBirth")).as_str()),
            "{ctx}"
        );
        let squash = |s: &str| {
            s.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_lowercase()
        };
        assert_eq!(
            squash(p.diagnosis.as_deref().unwrap_or("")),
            squash(def(st(pat, "Diagnosis"))),
            "{ctx}"
        );
        assert_eq!(
            p.clinician_notes.as_deref(),
            Some(st(pat, "ClinicianNotes")),
            "{ctx}"
        );

        let dev = j.pointer("/DeviceInformation/Final").expect("device");
        let d = &f.report.device;
        assert_eq!(
            d.model.as_deref(),
            Some(st(dev, "Neurostimulator")),
            "{ctx}"
        );
        assert_eq!(
            d.serial.as_deref(),
            Some(st(dev, "NeurostimulatorSerialNumber")),
            "{ctx}"
        );
        assert_eq!(
            d.model_number.as_deref(),
            Some(st(dev, "NeurostimulatorModel")),
            "{ctx}"
        );
        assert_eq!(
            d.implanted.as_deref(),
            Some(local_date(st(dev, "ImplantDate")).as_str()),
            "{ctx}"
        );
        assert_eq!(
            d.firmware.product_version.as_deref(),
            Some(st(dev, "ProductVersion")),
            "{ctx}"
        );
        assert_eq!(
            d.firmware.core_version.as_deref(),
            Some(st(dev, "CoreVersion")),
            "{ctx}"
        );
        assert_eq!(
            d.firmware.device_version.as_deref(),
            Some(st(dev, "DeviceVersion")),
            "{ctx}"
        );
        for (ours, key) in [
            (&d.firmware.product_application_id, "ProductApplicationId"),
            (&d.firmware.core_application_id, "CoreApplicationId"),
            (&d.firmware.device_application_id, "DeviceApplicationId"),
        ] {
            assert_eq!(
                ours.as_deref(),
                Some(st(dev, key).replace('.', "").as_str()),
                "{ctx} {key}"
            );
        }
        let mut words: Vec<String> = def(st(dev, "NeurostimulatorLocation"))
            .split('_')
            .map(str::to_lowercase)
            .collect();
        words.sort();
        let mut ours: Vec<String> = d
            .location
            .as_deref()
            .unwrap_or("")
            .split(' ')
            .map(str::to_owned)
            .collect();
        ours.sort();
        assert_eq!(ours, words, "{ctx}");
        let battery = j.get("BatteryInformation").expect("battery");
        assert_eq!(
            d.battery_percent,
            num(battery, "BatteryPercentage"),
            "{ctx}"
        );
        match num(battery, "EstimatedBatteryLifeMonths") {
            Some(m) => assert_eq!(
                d.extra
                    .get("battery_estimate_months")
                    .and_then(Value::as_f64),
                Some(m),
                "{ctx}"
            ),
            None => assert!(d.battery_estimate.is_none(), "{ctx}"),
        }
        if let Some(eri) = battery.get("ERIDate").and_then(Value::as_str) {
            assert_eq!(
                d.extra.get("eri_date").and_then(Value::as_str),
                Some(local_date(eri).as_str()),
                "{ctx}"
            );
        }
        assert_eq!(
            d.rechargeable,
            Some(st(dev, "Neurostimulator") == "Percept RC"),
            "{ctx}"
        );
        assert_eq!(
            d.feature_code.as_deref(),
            Some(st(j, "FeatureInformationCode")),
            "{ctx}"
        );
        assert_eq!(
            d.programmer_version.as_deref(),
            Some(st(j, "ProgrammerVersion")),
            "{ctx}"
        );

        let s = &f.report.session;
        assert_eq!(
            s.date.as_deref(),
            Some(local_minute(st(j, "SessionDate")).as_str()),
            "{ctx}"
        );
        assert!(s.timezone.is_none());
        let stim = j.get("Stimulation").expect("stimulation");
        let on_off = |k: &str| def(st(stim, k)).to_lowercase();
        assert_eq!(
            s.stimulation_initial.as_deref(),
            Some(on_off("InitialStimStatus").as_str()),
            "{ctx}"
        );
        assert_eq!(
            s.stimulation_final.as_deref(),
            Some(on_off("FinalStimStatus").as_str()),
            "{ctx}"
        );
        let reminder = j.get("BatteryReminder").expect("reminder");
        assert_eq!(
            s.battery_reminder.as_deref(),
            Some(st(reminder, "ReminderTime")),
            "{ctx}"
        );
        assert_eq!(s.close_communication_required, Some(true), "{ctx}");
        assert!(f
            .report
            .source
            .printed_at
            .as_deref()
            .is_some_and(|p| p.starts_with("2026-09-30T09:0")));
    }
}

fn leads_match_vendor_json(all: &[Fixture]) {
    for f in all {
        let configs = arr(
            f.json.pointer("/LeadConfiguration").expect("leads"),
            "Final",
        );
        assert_eq!(f.report.leads.len(), configs.len(), "{}", f.pdf);
        for c in configs {
            let h = match def(st(c, "Hemisphere")) {
                "Left" => Hemisphere::Left,
                _ => Hemisphere::Right,
            };
            let lead = f
                .report
                .leads
                .iter()
                .find(|l| l.hemisphere == h)
                .expect("lead");
            let model = def(st(c, "Model")).trim_start_matches("LEAD_");
            assert_eq!(lead.vendor_model.as_deref(), Some(model), "{}", f.pdf);
            assert_eq!(
                lead.catalogue_model.as_deref(),
                Some(format!("Medtronic {model}").as_str())
            );
            assert_eq!(
                lead.target.as_deref().map(str::to_lowercase),
                Some(def(st(c, "LeadLocation")).to_lowercase())
            );
            let tip = match def(st(c, "TipOffset")) {
                "ZERO" => 0.0,
                "EIGHT" => 8.0,
                other => panic!("tip offset {other}"),
            };
            assert_eq!(lead.tip_offset, Some(tip), "{}", f.pdf);
            let range: Vec<u32> = def(st(c, "ElectrodeNumber"))
                .split('_')
                .map(|w| match w {
                    "ZERO" => 0,
                    "THREE" => 3,
                    "SEVEN" => 7,
                    "EIGHT" => 8,
                    "ELEVEN" => 11,
                    "FIFTEEN" => 15,
                    other => panic!("port {other}"),
                })
                .collect();
            let numbers: Vec<u32> = lead
                .contacts
                .iter()
                .filter_map(|c| c.device_number)
                .collect();
            assert_eq!(numbers.first(), range.first(), "{}", f.pdf);
            assert_eq!(numbers.last(), range.last(), "{}", f.pdf);
            let ks: Vec<u32> = lead.contacts.iter().map(|c| c.k).collect();
            let n = if model.starts_with('B') { 8 } else { 4 };
            assert_eq!(ks, (1..=n).collect::<Vec<u32>>(), "{}", f.pdf);
            assert_eq!(
                lead.orientation_deg,
                num(c, "OrientationInDegrees"),
                "{}",
                f.pdf
            );
        }
    }
}

fn stimulation_snapshots_match_vendor_json(all: &[Fixture]) {
    let mut t = Tally::default();
    let mut programs_per_pdf = Vec::new();
    for f in all {
        let before = t.programs;
        for (snap, ours) in [
            ("Initial", &f.report.snapshots.initial),
            ("Final", &f.report.snapshots.final_),
        ] {
            let groups = arr(f.json.get("Groups").expect("groups"), snap);
            t.check(ours.groups.len() == groups.len(), || {
                format!("{} {snap}: group count", f.pdf)
            });
            for jg in groups {
                let id = group_letter(st(jg, "GroupId"));
                let ctx = format!("{} {snap} group {id}", f.pdf);
                let Some(og) = ours.groups.iter().find(|g| g.id == id) else {
                    t.errors.push(format!("{ctx}: missing"));
                    continue;
                };
                t.check(og.name.as_deref() == Some(st(jg, "GroupName")), || {
                    format!("{ctx}: name {:?}", og.name)
                });
                let active = jg
                    .get("ActiveGroup")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                t.check(og.active == active, || {
                    format!("{ctx}: active {} vs {active}", og.active)
                });
                if active {
                    t.check(ours.active_group.as_deref() == Some(id.as_str()), || {
                        format!("{ctx}: active_group")
                    });
                }
                compare_group(&mut t, &ctx, &f.report, og, jg, true);
                let settings = jg.get("GroupSettings").cloned().unwrap_or(Value::Null);
                let soft = settings
                    .get("SoftStartStop")
                    .cloned()
                    .unwrap_or(Value::Null);
                let expected_soft = if soft.get("Enabled").and_then(Value::as_bool) == Some(true) {
                    serde_json::json!({ "enabled": true, "duration_s": num(&soft, "DurationInSeconds") })
                } else {
                    serde_json::json!({ "enabled": false })
                };
                t.check(og.soft_start_stop.as_ref() == Some(&expected_soft), || {
                    format!(
                        "{ctx}: soft start {:?} vs {expected_soft}",
                        og.soft_start_stop
                    )
                });
                t.check(
                    og.cycling == Some(serde_json::json!({ "enabled": false })),
                    || format!("{ctx}: cycling {:?}", og.cycling),
                );
                for c in jg
                    .get("ProgramSettings")
                    .map(|p| arr(p, "SensingChannel"))
                    .unwrap_or(&[])
                {
                    let side = def(st(c, "HemisphereLocation")).to_lowercase();
                    let got = |k: &str| {
                        og.extra
                            .get("sensing_settings")
                            .and_then(|v| v.pointer(&format!("/{side}/{k}")))
                            .and_then(Value::as_f64)
                    };
                    let setup = c.get("SensingSetup").cloned().unwrap_or(Value::Null);
                    let adaptive = c.get("AdaptiveTherapy").cloned().unwrap_or(Value::Null);
                    let ms = |v: Option<f64>| v.map(|x| x / 1000.0);
                    let blank = num(&settings, "SensingBlankingDurationInMicroseconds")
                        .map(|us| (us / 100.0).round() / 10.0);
                    for (key, expected) in [
                        (
                            "high_pass_filter_hz",
                            num(&settings, "HighPassFilterInHertz"),
                        ),
                        (
                            "averaging_duration_s",
                            ms(num(&setup, "AveragingDurationInMilliSeconds")),
                        ),
                        ("sensing_blanking_ms", blank),
                        (
                            "onset_upper_s",
                            ms(num(&adaptive, "UpperThresholdOnsetInMilliSeconds")),
                        ),
                        (
                            "onset_lower_s",
                            ms(num(&adaptive, "LowerThresholdOnsetInMilliSeconds")),
                        ),
                        (
                            "detection_blanking_s",
                            ms(num(&adaptive, "DetectionBlankingDurationInMilliSeconds")),
                        ),
                        (
                            "adaptive_startup_delay_s",
                            ms(num(&adaptive, "AdaptiveStartupDelayInMilliSeconds")),
                        ),
                    ] {
                        let adaptive_key = key.starts_with("onset")
                            || key.starts_with("detection")
                            || key.starts_with("adaptive");
                        if adaptive_key
                            && def(st(c, "AdaptiveTherapyStatus")) == "NOT_CONFIGURED"
                            && got(key).is_none()
                        {
                            continue;
                        }
                        t.check(close(got(key), expected, 1e-6), || {
                            format!("{ctx} {side}: {key} {:?} vs {expected:?}", got(key))
                        });
                    }
                }
            }
        }
        programs_per_pdf.push((f.pdf.clone(), t.programs - before));
    }
    eprintln!(
        "stimulation programs compared: {programs_per_pdf:?}, BrainSense settings: {}",
        t.brainsense
    );
    assert_eq!(t.programs, 72, "16a §3.8: 72 stimulation programs");
    assert_eq!(t.brainsense, 16, "16a §3.8: 16 BrainSense settings");
    t.finish("stimulation programs");
}

fn group_history_matches_vendor_json(all: &[Fixture]) {
    let mut t = Tally::default();
    let mut per_pdf = Vec::new();
    for f in all {
        let before = t.programs;
        let sessions = arr(&f.json, "GroupHistory");
        t.check(f.report.history.len() == sessions.len(), || {
            format!(
                "{}: {} sessions vs {}",
                f.pdf,
                f.report.history.len(),
                sessions.len()
            )
        });
        for (n, (ours, theirs)) in f.report.history.iter().zip(sessions).enumerate() {
            let date = local_date(st(theirs, "SessionDate"));
            let ctx = format!("{} history {n} ({date})", f.pdf);
            t.check(ours.date.as_deref() == Some(date.as_str()), || {
                format!("{ctx}: date {:?}", ours.date)
            });
            let groups = arr(theirs, "Groups");
            t.check(ours.groups.len() == groups.len(), || {
                format!("{ctx}: group count")
            });
            for jg in groups {
                let id = group_letter(st(jg, "GroupId"));
                let Some(og) = ours.groups.iter().find(|g| g.id == id) else {
                    t.errors.push(format!("{ctx}: group {id} missing"));
                    continue;
                };
                compare_group(
                    &mut t,
                    &format!("{ctx} group {id}"),
                    &f.report,
                    og,
                    jg,
                    false,
                );
            }
        }
        per_pdf.push((f.pdf.clone(), t.programs - before));
    }
    eprintln!("history programs compared: {per_pdf:?}");
    assert_eq!(t.programs, 180, "16a §3.8: 180 group-history programs");
    t.finish("group history");
}

fn impedances_match_vendor_json(all: &[Fixture]) {
    let mut total = 0;
    let mut per_pdf = Vec::new();
    for f in all {
        let mut expected: BTreeMap<String, f64> = BTreeMap::new();
        let imp = arr(&f.json, "Impedance");
        for h in imp.first().map(|i| arr(i, "Hemisphere")).unwrap_or(&[]) {
            let side = def(st(h, "Hemisphere"));
            let hemi = if side == "Left" {
                Hemisphere::Left
            } else {
                Hemisphere::Right
            };
            let first = first_contact(&f.report, hemi);
            let session = h.get("SessionImpedance").cloned().unwrap_or(Value::Null);
            for (kind, key) in [("monopolar", "Monopolar"), ("bipolar", "Bipolar")] {
                for cell in arr(&session, key) {
                    let a = electrode_label(st(cell, "Electrode1"), first)
                        .unwrap_or_else(|| "case".to_owned());
                    let b = electrode_label(st(cell, "Electrode2"), first).expect("electrode 2");
                    expected.insert(
                        format!("{side}/{kind}/{}/{}", a.to_lowercase(), b.to_lowercase()),
                        num(cell, "ResultValue").expect("ohm"),
                    );
                }
            }
        }
        let mut ours: BTreeMap<String, f64> = BTreeMap::new();
        for i in &f.report.impedances {
            let kind = match i.kind {
                ImpedanceKind::Monopolar => "monopolar",
                ImpedanceKind::Bipolar => "bipolar",
            };
            ours.insert(
                format!("{}/{kind}/{}/{}", hemisphere_word(i.hemisphere), i.a, i.b),
                i.ohm.expect("ohm"),
            );
            assert_eq!(i.status.as_deref(), Some("ok"), "{}", f.pdf);
        }
        assert_eq!(ours, expected, "{}", f.pdf);
        assert_eq!(
            f.report.impedances.len(),
            ours.len(),
            "{}: duplicate cells",
            f.pdf
        );
        total += ours.len();
        per_pdf.push((f.pdf.clone(), ours.len()));
    }
    eprintln!("impedance cells compared: {per_pdf:?}");
    assert_eq!(total, 184, "16a §3.8: 184 impedance cells");
}

fn annotation_key(
    hemi: &str,
    date: &str,
    amp: f64,
    pw: f64,
    rate: f64,
    mut cathodes: Vec<String>,
    mut scores: Vec<String>,
) -> String {
    cathodes.sort();
    scores.sort();
    format!(
        "{hemi}|{date}|{}|{pw}|{rate}|{}|{}",
        (amp * 100.0).round() as i64,
        cathodes.join(","),
        scores.join(",")
    )
}

fn annotations_match_vendor_json_as_multiset(all: &[Fixture]) {
    let mut total = 0;
    let mut per_pdf = Vec::new();
    for f in all {
        let mut expected = Vec::new();
        for a in arr(&f.json, "Annotations") {
            let side = def(st(a, "Hemisphere"));
            let hemi = if side == "Left" {
                Hemisphere::Left
            } else {
                Hemisphere::Right
            };
            let first = first_contact(&f.report, hemi);
            let program = arr(a, "Program").first().cloned().unwrap_or(Value::Null);
            let cathodes: Vec<String> = arr(&program, "ElectrodeState")
                .iter()
                .filter(|s| def(st(s, "ElectrodeStateResult")) == "Negative")
                .filter_map(|s| electrode_label(st(s, "Electrode"), first))
                .map(|l| l.to_lowercase())
                .collect();
            let mut scores = Vec::new();
            for (list, name_key, rating_key) in [
                ("Symptoms", "SymptomName", "SymptomRating"),
                ("SideEffects", "SideEffectName", "SideEffectRating"),
            ] {
                for s in arr(a, list) {
                    let col = vj::symptom_column(st(s, name_key)).expect("known symptom");
                    let r = vj::rating(st(s, rating_key)).expect("rating");
                    scores.push(format!("{col}={r}"));
                }
            }
            expected.push(annotation_key(
                side,
                &local_date(st(a, "Date")),
                num(&program, "AmplitudeInMilliAmps").expect("amp"),
                num(&program, "PulseWidthInMicroSecond").expect("pw"),
                num(a, "RateInHertz").expect("rate"),
                cathodes,
                scores,
            ));
        }
        let mut ours = Vec::new();
        for a in &f.report.annotations {
            let program: Program =
                serde_json::from_value(a.program.clone().expect("program")).expect("program shape");
            let cathodes: Vec<String> = program
                .contacts
                .iter()
                .filter(|c| c.polarity == Polarity::Cathode)
                .map(|c| c.label.clone())
                .collect();
            let scores: Vec<String> = a
                .symptoms
                .iter()
                .filter_map(|s| s.score.map(|v| format!("{}={}", s.name, v as u32)))
                .collect();
            ours.push(annotation_key(
                hemisphere_word(a.hemisphere.expect("hemisphere")),
                a.date.as_deref().expect("date"),
                program.amplitude.expect("amp"),
                program.pulse_width_us.expect("pw"),
                a.frequency_hz.expect("rate"),
                cathodes,
                scores,
            ));
        }
        expected.sort();
        ours.sort();
        assert_eq!(ours, expected, "{}", f.pdf);
        total += ours.len();
        per_pdf.push((f.pdf.clone(), ours.len()));
    }
    eprintln!("annotations compared: {per_pdf:?}");
    assert_eq!(total, 168, "16a §3.8: 168 annotations");
}

fn usage_events_and_summary_match_vendor_json(all: &[Fixture]) {
    for f in all {
        let usage = arr(&f.json, "GroupUsagePercentage");
        let ours: Vec<String> = f.report.group_usage.iter().map(|g| g.id.clone()).collect();
        assert_eq!(ours, ["A", "B", "C", "D"], "{}", f.pdf);
        assert_eq!(ours.len(), usage.len(), "{}", f.pdf);
        for g in usage.iter().filter(|g| g.get("GroupId").is_some()) {
            assert!(ours.contains(&group_letter(st(g, "GroupId"))), "{}", f.pdf);
        }
        assert!(f.report.group_usage.iter().all(|g| g.percent.is_none()));
        let months = f
            .report
            .session
            .extra
            .get("group_usage_monthly")
            .and_then(Value::as_array)
            .expect("months");
        assert_eq!(months.len(), 3, "{}", f.pdf);

        let summary = f.json.get("EventSummary").expect("event summary");
        let mut expected: Vec<(String, u32)> = arr(summary, "Events")
            .iter()
            .map(|e| {
                (
                    st(e, "EventName").to_owned(),
                    num(e, "EventCount").expect("count") as u32,
                )
            })
            .collect();
        let mut ours: Vec<(String, u32)> = f
            .report
            .events
            .iter()
            .map(|e| (e.name.clone().expect("name"), e.count.expect("count")))
            .collect();
        expected.sort();
        ours.sort();
        assert_eq!(ours, expected, "{}", f.pdf);
        let start = local_date(st(summary, "SessionStartDate"));
        let end = local_date(st(summary, "SessionEndDate"));
        for e in &f.report.events {
            assert_eq!(
                e.extra.get("period_start").and_then(Value::as_str),
                Some(start.as_str())
            );
            assert_eq!(
                e.extra.get("period_end").and_then(Value::as_str),
                Some(end.as_str())
            );
        }
        let lfp = f
            .report
            .sensing
            .extra
            .get("lfp_amplitude_summary")
            .and_then(Value::as_array)
            .expect("lfp");
        let vendor = arr(summary, "LfpAndAmplitudeSummary");
        assert_eq!(lfp.len(), vendor.len(), "{}", f.pdf);
        for v in vendor {
            let side = def(st(v, "Hemisphere")).to_lowercase();
            let o = lfp
                .iter()
                .find(|x| x.get("hemisphere").and_then(Value::as_str) == Some(side.as_str()))
                .expect("hemisphere summary");
            assert_eq!(
                o.get("group").and_then(Value::as_str),
                Some(group_letter(st(v, "GroupId")).as_str())
            );
            assert_eq!(
                o.get("period_start").and_then(Value::as_str),
                Some(start.as_str())
            );
            for (key, json_key, tol) in [
                ("above_threshold_percent", "AboveThresholdPercent", 1e-6),
                (
                    "between_thresholds_percent",
                    "BetweenThresholdPercent",
                    1e-6,
                ),
                ("below_threshold_percent", "BelowThresholdPercent", 1e-6),
                ("average_amplitude_ma", "AverageAmplitude", 0.05 + 1e-9),
                (
                    "time_adaptive_running_percent",
                    "TimeAdaptiveRunningPercent",
                    1e-6,
                ),
                (
                    "adaptive_amplitude_reduction_percent",
                    "AdaptiveAmplitudeReductionPercent",
                    1e-6,
                ),
            ] {
                assert!(
                    close(o.get(key).and_then(Value::as_f64), num(v, json_key), tol),
                    "{} {side}: {key} {:?} vs {:?}",
                    f.pdf,
                    o.get(key),
                    num(v, json_key)
                );
            }
        }

        let events = f
            .report
            .session
            .extra
            .get("patient_events")
            .expect("patient events");
        for snap in ["Initial", "Final"] {
            let expected: Vec<(String, Option<String>)> =
                arr(f.json.get("PatientEvents").expect("pe"), snap)
                    .iter()
                    .map(|e| {
                        (
                            st(e, "EventName").to_owned(),
                            e.get("Additional Behavior")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        )
                    })
                    .collect();
            let ours: Vec<(String, Option<String>)> = events
                .get(snap.to_lowercase())
                .and_then(Value::as_array)
                .expect("snapshot events")
                .iter()
                .map(|e| {
                    (
                        e.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        e.get("additional_behavior")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    )
                })
                .collect();
            assert_eq!(ours, expected, "{} {snap}", f.pdf);
        }
    }
}

fn recharge_and_sensing_summaries_match_vendor_json(all: &[Fixture]) {
    for f in all {
        match f.json.get("RechargeCount").and_then(Value::as_array) {
            Some(sessions) => {
                let ours = f
                    .report
                    .device
                    .extra
                    .get("recharge")
                    .and_then(|v| v.pointer("/sessions"))
                    .and_then(Value::as_array)
                    .expect("recharge");
                assert_eq!(ours.len(), 10, "{}", f.pdf);
                for o in ours {
                    let date = o.get("date").and_then(Value::as_str).expect("date");
                    let hit = sessions.iter().any(|s| {
                        local_date(st(s, "SessionStartDate")) == date
                            && num(s, "StartingPercentage")
                                == o.get("start_percent").and_then(Value::as_f64)
                            && num(s, "EndingPercentage")
                                == o.get("end_percent").and_then(Value::as_f64)
                    });
                    assert!(hit, "{}: recharge {o}", f.pdf);
                }
            }
            None => assert!(f.report.device.extra.get("recharge").is_none(), "{}", f.pdf),
        }
        let montage: Vec<&Value> = arr(&f.json, "LFPMontage")
            .iter()
            .filter(|m| num(m, "PeakMagnitudeInMicroVolt").unwrap_or(0.0) > 0.0)
            .collect();
        assert_eq!(f.report.sensing.surveys.len(), montage.len(), "{}", f.pdf);
        for m in montage {
            let side = def(st(m, "Hemisphere"));
            let hemi = if side == "Left" {
                Hemisphere::Left
            } else {
                Hemisphere::Right
            };
            let first = first_contact(&f.report, hemi);
            let mut levels = vj::channel_levels(st(m, "SensingElectrodes"));
            levels.reverse();
            let labels: Vec<String> = levels.iter().map(|l| (l + first).to_string()).collect();
            let hit = f.report.sensing.surveys.iter().any(|s| {
                s.get("hemisphere").and_then(Value::as_str) == Some(side.to_lowercase().as_str())
                    && s.get("electrodes") == Some(&serde_json::json!(labels))
                    && close(
                        s.get("peak_frequency_hz").and_then(Value::as_f64),
                        num(m, "PeakFrequencyInHertz"),
                        1e-9,
                    )
                    && close(
                        s.get("peak_magnitude_uvp").and_then(Value::as_f64),
                        num(m, "PeakMagnitudeInMicroVolt"),
                        1e-8,
                    )
            });
            assert!(hit, "{}: survey {labels:?}", f.pdf);
        }
        let identifier: Vec<&Value> = arr(&f.json, "BrainSenseSurveys")
            .iter()
            .filter(|s| st(s, "SurveyMode") == "ElectrodeIdentifier")
            .flat_map(|s| arr(s, "ElectrodeIdentifier"))
            .filter(|e| num(e, "PeakFrequencyInHertz").unwrap_or(0.0) > 0.0)
            .collect();
        let ours = f
            .report
            .sensing
            .extra
            .get("identifier")
            .and_then(Value::as_array);
        if identifier.is_empty() {
            assert!(ours.is_none(), "{}", f.pdf);
            continue;
        }
        let ours = ours.expect("identifier");
        for e in identifier {
            let side = st(e, "Hemisphere").to_lowercase();
            let block = ours
                .iter()
                .find(|b| b.get("hemisphere").and_then(Value::as_str) == Some(side.as_str()))
                .expect("identifier hemisphere");
            assert_eq!(
                block.get("detected_frequency_hz").and_then(Value::as_f64),
                num(e, "PeakFrequencyInHertz")
            );
            assert_eq!(
                block.get("selected_frequency_hz").and_then(Value::as_f64),
                num(e, "SelectedFrequencyInHertz")
            );
            let level = st(e, "SensingElectrodes")
                .strip_prefix("ELECTRODE_")
                .and_then(|w| w.split('_').next())
                .and_then(vj::word_level)
                .expect("ring level");
            let contact = (level + num(e, "TipOffset").expect("tip") as u32).to_string();
            let rank = match st(e, "RankingatSelectedFrequency") {
                "HIGHEST_RANK" => "most",
                "LOWEST_RANK" => "least",
                _ => "medium",
            };
            let ranks = block.get("ranks").and_then(Value::as_array).expect("ranks");
            assert!(
                ranks.contains(&serde_json::json!({ "contact": contact, "rank": rank })),
                "{}: identifier {contact} {rank}",
                f.pdf
            );
        }
    }
}

fn sections_are_kept_only_on_request(all: &[Fixture]) {
    for f in all {
        for id in [
            "pi", "di", "ss", "pps", "gs", "sii", "gh", "gu", "sa", "bss", "mbss", "pe", "es",
        ] {
            assert!(
                f.report.sections.contains_key(id),
                "{}: section {id}",
                f.pdf
            );
        }
        let rc = f.report.device.rechargeable == Some(true);
        assert_eq!(f.report.sections.contains_key("rc"), rc, "{}", f.pdf);
    }
    let first = all.first().expect("fixture");
    let r = MedtronicPercept
        .parse(
            &first.doc,
            &ParseOptions {
                keep_sections: false,
            },
        )
        .expect("parse");
    assert!(r.sections.is_empty());
    assert_eq!(r.snapshots, first.report.snapshots);
}

type Section = (&'static str, fn(&[Fixture]));

#[test]
fn fixtures_match_vendor_json_and_structure() {
    if real_fixture_dir("fixtures_match_vendor_json_and_structure").is_none() {
        return;
    }
    let all = load_fixtures();
    let sections: [Section; 12] = [
        (
            "every_fixture_parses_without_errors",
            every_fixture_parses_without_errors,
        ),
        ("canonical_json_round_trips", canonical_json_round_trips),
        (
            "fields_have_paths_and_dates_parse",
            fields_have_paths_and_dates_parse,
        ),
        (
            "patient_device_and_session",
            patient_device_and_session_match_vendor_json,
        ),
        ("leads", leads_match_vendor_json),
        (
            "stimulation_snapshots",
            stimulation_snapshots_match_vendor_json,
        ),
        ("group_history", group_history_matches_vendor_json),
        ("impedances", impedances_match_vendor_json),
        ("annotations", annotations_match_vendor_json_as_multiset),
        (
            "usage_events_and_summary",
            usage_events_and_summary_match_vendor_json,
        ),
        (
            "recharge_and_sensing_summaries",
            recharge_and_sensing_summaries_match_vendor_json,
        ),
        (
            "sections_are_kept_only_on_request",
            sections_are_kept_only_on_request,
        ),
    ];
    let failures: Vec<String> = sections
        .iter()
        .filter_map(|(name, run)| {
            catch_unwind(AssertUnwindSafe(|| run(&all)))
                .err()
                .map(|p| format!("[{name}] {}", panic_text(p)))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} sections failed:\n{}",
        failures.len(),
        sections.len(),
        failures.join("\n")
    );
}

fn glyph(text: &str, x0: f64, baseline: f64, white: bool) -> Glyph {
    Glyph {
        text: text.to_owned(),
        x0,
        top: baseline - 12.0,
        x1: x0 + 6.0,
        bottom: baseline,
        baseline,
        size: 12.0,
        font: "Roboto".to_owned(),
        fill_rgb: if white { [1.0; 3] } else { [0.0; 3] },
        alpha: 1.0,
        render_mode: 0,
    }
}

fn words(text: &str, x0: f64, baseline: f64, white: bool) -> Vec<Glyph> {
    text.chars()
        .enumerate()
        .map(|(i, c)| glyph(&c.to_string(), x0 + 6.0 * i as f64, baseline, white))
        .collect()
}

fn page(glyphs: Vec<Glyph>) -> PageContent {
    PageContent {
        number: 1,
        width: 612.0,
        height: 792.0,
        glyphs,
        shapes: Vec::new(),
        unmapped_glyphs: 0,
    }
}

#[test]
fn detect_refuses_empty_and_foreign_documents() {
    assert!(!MedtronicPercept.detect(&PdfDocument { pages: Vec::new() }));
    assert!(!MedtronicPercept.detect(&PdfDocument {
        pages: vec![page(Vec::new())]
    }));
    let mut glyphs = words("Session Report", 54.0, 35.0, false);
    glyphs.extend(words("Kinetra 7428 X1", 54.0, 71.0, false));
    glyphs.extend(words("<s-pi>", 54.0, 97.0, true));
    glyphs.extend(words("Page 1", 500.0, 784.0, false));
    assert!(!MedtronicPercept.detect(&PdfDocument {
        pages: vec![page(glyphs)]
    }));
}

#[test]
fn a_header_only_document_fails_with_a_section_error() {
    let mut glyphs = words("Session Report", 54.0, 35.0, false);
    glyphs.extend(words("Demo Patient MRN:1", 54.0, 53.0, false));
    glyphs.extend(words("Percept PC SERIAL", 54.0, 71.0, false));
    glyphs.extend(words("<s-pi>", 54.0, 97.0, true));
    glyphs.extend(words("Page 1", 500.0, 784.0, false));
    let doc = PdfDocument {
        pages: vec![page(glyphs)],
    };
    assert!(MedtronicPercept.detect(&doc));
    match MedtronicPercept.parse(&doc, &ParseOptions::default()) {
        Err(crate::ReportError::Section { id, .. }) => {
            assert!(["pi", "di", "ss"].contains(&id.as_str()))
        }
        other => panic!("expected a section error, got {other:?}"),
    }
}
