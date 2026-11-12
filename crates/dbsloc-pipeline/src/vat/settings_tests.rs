#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use fastwarp_matfile::mat5::read_mat5;

const N: usize = 8;

fn base() -> Stimulation {
    let mut s = Stimulation::blank("lab1", "01", ModelKind::Efield, [N, N]);
    let src = &mut s.sides.right.sources[0];
    src.amplitude = 2.5;
    src.frequency_hz = Some(130.0);
    src.contacts[1] = Contact {
        index: 2,
        polarity: Polarity::Cathode,
        percent: 100.0,
        impedance_kohm: 1.2,
    };
    s
}

fn bipolar() -> Stimulation {
    let mut s = base();
    let src = &mut s.sides.right.sources[0];
    src.case.polarity = Polarity::Off;
    src.case.percent = 0.0;
    src.contacts[1].percent = 60.0;
    src.contacts[2] = Contact {
        index: 3,
        polarity: Polarity::Cathode,
        percent: 40.0,
        impedance_kohm: 1.0,
    };
    src.contacts[4] = Contact {
        index: 5,
        polarity: Polarity::Anode,
        percent: 100.0,
        impedance_kohm: 1.0,
    };
    s
}

fn codes(s: &Stimulation) -> Vec<(&'static str, String)> {
    match validate(s, [N, N]) {
        Ok(()) => Vec::new(),
        Err(e) => e.into_iter().map(|e| (e.code, e.to_string())).collect(),
    }
}

fn fails(s: &Stimulation, code: &str, needle: &str) {
    let e = codes(s);
    assert!(
        e.iter().any(|(c, m)| *c == code && m.contains(needle)),
        "{code} {needle}: {e:?}"
    );
}

fn warnings(s: &Stimulation) -> Vec<String> {
    check(s, [N, N]).warnings
}

#[test]
fn json_round_trip_is_identical_and_defaults_fill() {
    let s = base();
    let text = canonical_json(&s).unwrap();
    let back: Stimulation = serde_json::from_str(&text).unwrap();
    assert_eq!(back, s);
    let minimal: Stimulation = serde_json::from_str(r#"{"label":"x","model":"kuncel"}"#).unwrap();
    assert_eq!(minimal.model_settings, ModelSettings::default());
    assert_eq!(minimal.schema, SCHEMA);
    assert_eq!(minimal.lead_dbs.ver, "2.0");
}

#[test]
fn valid_settings_pass_and_normalise_fills_four_sources() {
    assert!(validate(&base(), [N, N]).is_ok());
    assert!(validate(&bipolar(), [N, N]).is_ok());
    let mut s: Stimulation = serde_json::from_str(r#"{"label":"x","model":"efield"}"#).unwrap();
    let notes = normalise(&mut s, [4, 2]);
    assert!(notes.is_empty());
    assert_eq!(s.sides.right.sources.len(), 4);
    assert_eq!(s.sides.left.sources[3].contacts.len(), 2);
    assert_eq!(s.sides.right.sources[0].contacts.len(), 4);
}

#[test]
fn normalise_drops_unused_extra_contacts_and_repairs_impedance() {
    let mut s = base();
    s.sides.right.sources[0].contacts.push(Contact::unused(12));
    s.sides.right.sources[0].contacts[0].impedance_kohm = 0.0;
    let notes = normalise(&mut s, [N, N]);
    assert_eq!(s.sides.right.sources[0].contacts.len(), N);
    assert_eq!(s.sides.right.sources[0].contacts[0].impedance_kohm, 1.0);
    assert_eq!(notes.len(), 2);
}

#[test]
fn rule_1_inactive_sources_are_ignored() {
    let mut s = base();
    s.sides.left.sources[0].amplitude = 0.0;
    s.sides.left.sources[0].contacts[0].percent = 250.0;
    s.sides.right.sources[0].amplitude = 0.0;
    s.sides.right.sources[0].contacts[1].percent = 55.0;
    assert!(validate(&s, [N, N]).is_ok());
}

#[test]
fn rule_2_amplitude() {
    let mut s = base();
    s.sides.right.sources[0].amplitude = -1.0;
    fails(&s, "E_VAT_settings", "right source 1: amplitude");
    s.sides.right.sources[0].amplitude = f64::NAN;
    fails(&s, "E_VAT_settings", "amplitude");
    let mut w = base();
    w.sides.right.sources[0].amplitude = 25.0;
    assert!(warnings(&w).iter().any(|m| m.contains("above 20 mA")));
}

#[test]
fn rule_3_percent_and_polarity_consistency() {
    let mut s = base();
    s.sides.right.sources[0].contacts[1].percent = 120.0;
    fails(&s, "E_VAT_settings", "percent 120");
    let mut s = base();
    s.sides.right.sources[0].contacts[3].percent = 10.0;
    fails(&s, "E_VAT_settings", "polarity off");
    let mut s = base();
    s.sides.right.sources[0].contacts[3].polarity = Polarity::Anode;
    fails(&s, "E_VAT_settings", "percent 0");
}

#[test]
fn rule_4_current_mode_sums_include_the_case() {
    let mut s = bipolar();
    s.sides.right.sources[0].contacts[1].percent = 50.0;
    fails(&s, "E_VAT_settings", "not 100");
    let mut s = bipolar();
    s.sides.right.sources[0].case = CaseSpec {
        polarity: Polarity::Anode,
        percent: 100.0,
    };
    fails(&s, "E_VAT_settings", "Anode percentages sum to 200");
    let mut s = bipolar();
    let src = &mut s.sides.right.sources[0];
    src.contacts[4].percent = 70.0;
    src.case = CaseSpec {
        polarity: Polarity::Anode,
        percent: 30.0,
    };
    assert!(validate(&s, [N, N]).is_ok());
}

#[test]
fn rule_5_voltage_mode_sum_is_a_warning() {
    let mut s = bipolar();
    let src = &mut s.sides.right.sources[0];
    src.mode = Mode::Voltage;
    src.amplitude = 3.0;
    src.contacts[1].percent = 50.0;
    assert!(validate(&s, [N, N]).is_ok());
    assert!(warnings(&s).iter().any(|m| m.contains("not 100")));
}

#[test]
fn rule_6_missing_return_is_a_warning_naming_the_case() {
    let mut s = base();
    s.sides.right.sources[0].case = CaseSpec {
        polarity: Polarity::Off,
        percent: 0.0,
    };
    assert!(validate(&s, [N, N]).is_ok());
    assert!(warnings(&s).iter().any(|m| m
        == "dbsloc: warning: vat: lab1: right source 1 has no return electrode; the case is used as return"));
}

#[test]
fn rule_7_contacts_must_exist_on_the_model() {
    let mut s = base();
    s.sides.right.sources[0].contacts.push(Contact {
        index: 9,
        polarity: Polarity::Cathode,
        percent: 10.0,
        impedance_kohm: 1.0,
    });
    fails(&s, "E_VAT_settings", "contact 9 does not exist");
    let mut s = base();
    s.sides.right.sources[0].contacts.push(Contact::unused(9));
    assert!(validate(&s, [N, N]).is_ok());
}

#[test]
fn rule_8_impedance_warns() {
    let mut s = base();
    s.sides.right.sources[0].contacts[1].impedance_kohm = -3.0;
    assert!(validate(&s, [N, N]).is_ok());
    assert!(warnings(&s).iter().any(|m| m.contains("impedance")));
}

#[test]
fn rule_9_analytical_models() {
    let mut s = bipolar();
    s.model = ModelKind::Dembek;
    fails(&s, "E_VAT_montage", "exactly one used contact");
    let mut s = base();
    s.model = ModelKind::Kuncel;
    fails(&s, "E_VAT_mode", "voltage mode");
    s.sides.right.sources[0].mode = Mode::Voltage;
    assert!(validate(&s, [N, N]).is_ok());
    s.model = ModelKind::Dembek;
    s.sides.right.sources[0].mode = Mode::Current;
    assert!(validate(&s, [N, N]).is_ok());
    let mut m = base();
    m.model = ModelKind::Maedler;
    let src = &mut m.sides.right.sources[0];
    src.mode = Mode::Voltage;
    src.amplitude = 12.0;
    assert!(warnings(&m).iter().any(|w| w.contains("0-10 V")));
}

#[test]
fn rule_10_label() {
    for bad in ["", ".hidden", "a b", "gs_abc", &"x".repeat(65)] {
        let mut s = base();
        s.label = bad.to_owned();
        fails(&s, "E_VAT_settings", "label");
    }
}

#[test]
fn rule_11_model_settings() {
    let mut s = base();
    s.model_settings.threshold_v_per_mm = 0.0;
    fails(&s, "E_VAT_settings", "threshold_v_per_mm");
    let mut s = base();
    s.model_settings.conductivity.wm_s_per_m = -1.0;
    fails(&s, "E_VAT_settings", "conductivities");
    let mut s = base();
    s.model_settings.encapsulation.thickness_mm = 2.0;
    fails(&s, "E_VAT_settings", "thickness_mm");
    let mut s = base();
    s.model_settings.encapsulation.thickness_mm = 0.1;
    fails(&s, "E_VAT_settings", "s_per_m is required");
}

#[test]
fn s_export_has_the_layout_of_the_spec() {
    let s = bipolar();
    let (v, warns) = export_s(&s, [1.5, 0.0]);
    assert!(warns.is_empty());
    let names = field_names(&v);
    assert_eq!(names.first().map(String::as_str), Some("label"));
    assert!(names.iter().any(|n| n == "Rs4") && names.iter().any(|n| n == "Ls1"));
    assert_eq!(get(&v, "ver"), Some(&Mat5Value::char("2.0")));
    assert_eq!(get(&v, "numContacts").and_then(num_of), Some(8.0));
    assert_eq!(get(&v, "monopolarmodel").and_then(num_of), Some(0.0));
    let rs1 = get(&v, "Rs1").unwrap();
    assert_eq!(count_k_fields(rs1), 8);
    assert_eq!(
        get(get(rs1, "k2").unwrap(), "pol").and_then(num_of),
        Some(1.0)
    );
    assert_eq!(get(rs1, "va").and_then(num_of), Some(2.0));
    let active = get(&v, "activecontacts").and_then(|c| c.item(0)).unwrap();
    assert_eq!(
        active,
        &Mat5Value::row(&[0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0])
    );
    assert_eq!(get(&v, "volume").unwrap(), &Mat5Value::row(&[1.5, 0.0]));
}

#[test]
fn s_round_trips_through_the_mat_writer_and_reader() {
    let mut s = bipolar();
    let mut src = Source::blank(2, N);
    src.mode = Mode::Voltage;
    src.amplitude = 3.0;
    src.pulse_width_us = 90.0;
    src.contacts[0] = Contact {
        index: 1,
        polarity: Polarity::Cathode,
        percent: 100.0,
        impedance_kohm: 0.8,
    };
    s.sides.left.sources[1] = src;
    s.sides.left.active_source = 2;
    s.lead_dbs
        .extra_fields
        .insert("template".into(), json_number(1.0));
    s.lead_dbs
        .extra_fields
        .insert("note".into(), Value::String("hello".into()));
    s.sides.right.sources[0].frequency_hz = None;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub-01_desc-stimparameters.mat");
    write_stimparameters_mat(&path, &s, [0.0, 0.0]).unwrap();
    let vars = read_mat5(&std::fs::read(&path).unwrap()).unwrap();
    assert!(vars.iter().any(|(n, _)| n == PROVENANCE_VARIABLE));
    let (back, notes) = read_stimparameters_mat(&path, "01", 8).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(back.sides, s.sides);
    assert_eq!(back.label, s.label);
    assert_eq!(back.model, s.model);
    assert_eq!(back.lead_dbs, s.lead_dbs);
    assert_eq!(to_lead_dbs_s(&back), to_lead_dbs_s(&s));
}

#[test]
fn json_to_s_to_json_is_identical_for_every_model() {
    for model in ModelKind::ALL {
        let mut s = bipolar();
        s.model = model;
        s.sides.right.sources[0].frequency_hz = None;
        let (back, _) = from_lead_dbs_s(&to_lead_dbs_s(&s), "01", 8).unwrap();
        assert_eq!(back.model, model);
        assert_eq!(back.sides, s.sides);
    }
}

fn set_field(v: &mut Mat5Value, name: &str, value: Mat5Value) {
    if let Mat5Value::Struct {
        fields, elements, ..
    } = v
    {
        let i = fields.iter().position(|f| f == name).unwrap();
        elements[0][i] = value;
    }
}

#[test]
fn unknown_model_and_bad_polarity_are_refused() {
    let mut v = to_lead_dbs_s(&base());
    set_field(&mut v, "model", Mat5Value::char("Nonsense 1999"));
    let e = from_lead_dbs_s(&v, "01", 8).unwrap_err();
    assert!(e.to_string().contains("E_VAT_model"), "{e}");

    let mut v = to_lead_dbs_s(&base());
    let mut rs1 = get(&v, "Rs1").unwrap().clone();
    let mut k2 = get(&rs1, "k2").unwrap().clone();
    set_field(&mut k2, "pol", Mat5Value::scalar(7.0));
    set_field(&mut rs1, "k2", k2);
    set_field(&mut v, "Rs1", rs1);
    let e = from_lead_dbs_s(&v, "01", 8).unwrap_err();
    assert!(e.to_string().contains("E_VAT_settings"), "{e}");
}

#[test]
fn disagreeing_amplitude_is_reported_and_amp_wins() {
    let mut v = to_lead_dbs_s(&base());
    set_field(
        &mut v,
        "amplitude",
        Mat5Value::cell_row(vec![
            Mat5Value::row(&[9.0, 0.0, 0.0, 0.0]),
            Mat5Value::row(&[0.0; 4]),
        ]),
    );
    let (s, notes) = from_lead_dbs_s(&v, "01", 8).unwrap();
    assert_eq!(s.sides.right.sources[0].amplitude, 2.5);
    assert!(notes
        .iter()
        .any(|n| n == "dbsloc: warning: vat: lab1: S.amplitude disagrees with Rs1.amp; using amp"));
}

fn legacy_source(right: &[(usize, f64, f64)], left: &[(usize, f64, f64)]) -> Mat5Value {
    let k = |perc: f64, pol: f64| {
        Mat5Value::struct_scalar(vec![
            field("perc", Mat5Value::scalar(perc)),
            field("pol", Mat5Value::scalar(pol)),
            field("imp", Mat5Value::scalar(1.0)),
        ])
    };
    let mut fields: Vec<(String, Mat5Value)> = (0..16)
        .map(|i| {
            let hit = right
                .iter()
                .find(|(n, _, _)| *n == i + 1)
                .or_else(|| left.iter().find(|(n, _, _)| *n + 8 == i + 1));
            match hit {
                Some((_, p, pol)) => (format!("k{i}"), k(*p, *pol)),
                None => (format!("k{i}"), k(0.0, 0.0)),
            }
        })
        .collect();
    fields.push(field(
        "case",
        Mat5Value::struct_scalar(vec![
            field("perc", Mat5Value::scalar(100.0)),
            field("pol", Mat5Value::scalar(2.0)),
        ]),
    ));
    fields.push(field("amp", Mat5Value::scalar(3.0)));
    fields.push(field("va", Mat5Value::scalar(1.0)));
    fields.push(field("pulseWidth", Mat5Value::scalar(60.0)));
    Mat5Value::struct_scalar(fields)
}

#[test]
fn legacy_layout_without_ver_is_upgraded() {
    let blank = legacy_source(&[], &[]);
    let s = Mat5Value::struct_scalar(vec![
        field("label", Mat5Value::char("old")),
        field("Rs1", legacy_source(&[(2, 100.0, 1.0)], &[(3, 100.0, 1.0)])),
        field("Rs2", blank.clone()),
        field("Rs3", blank.clone()),
        field("Rs4", blank),
        field("model", Mat5Value::char("Kuncel 2008")),
        field("numContacts", Mat5Value::scalar(8.0)),
    ]);
    let (st, notes) = from_lead_dbs_s(&s, "05", 8).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(st.lead_dbs.ver, "2.0");
    assert_eq!(st.model, ModelKind::Kuncel);
    let right = &st.sides.right.sources[0];
    assert_eq!(right.contacts.len(), 8);
    assert_eq!(right.contacts[1].polarity, Polarity::Cathode);
    assert_eq!(right.contacts[2].polarity, Polarity::Off);
    assert_eq!(right.mode, Mode::Voltage);
    let left = &st.sides.left.sources[0];
    assert_eq!(left.contacts[2].polarity, Polarity::Cathode);
    assert_eq!(left.contacts[1].polarity, Polarity::Off);
    assert_eq!(left.amplitude, 3.0);
}

#[test]
fn counts_come_from_a_reco_file() {
    let reco = serde_json::json!({
        "mni": {"1": {"coords_mm": [[12.0, 0.0, 0.0]]}, "2": {"coords_mm": [[-12.0, 0.0, 0.0]]}},
        "props": {
            "1": {"elmodel": "Boston Scientific Vercise Directed"},
            "2": {"elmodel": "Medtronic 3387"}
        },
        "meta": {"side_keys": "1=right,2=left"}
    });
    assert_eq!(contact_counts_from_reco(&reco).unwrap(), [Some(8), Some(4)]);
}

#[test]
fn save_load_and_list_labels() {
    let dir = tempfile::tempdir().unwrap();
    let s = base();
    let path = save(dir.path(), &s).unwrap();
    assert!(path.ends_with("stimulations/native/lab1/sub-01_desc-stimulation.json"));
    let back = load(dir.path(), "01", "lab1").unwrap();
    assert_eq!(back.sides, s.sides);
    assert_eq!(back.provenance.software["product"], PRODUCT);
    assert!(!back.provenance.written_at.is_empty());
    let other = stimparameters_mat_path(dir.path(), "01", "MNI152NLin2009bAsym", "imp1");
    write_stimparameters_mat(&other, &s, [0.0; 2]).unwrap();
    std::fs::create_dir_all(label_dir(dir.path(), "native", "gs_hidden")).unwrap();
    std::fs::create_dir_all(label_dir(dir.path(), "native", ".partial-x")).unwrap();
    let labels = list_labels(dir.path(), "01");
    assert_eq!(
        labels,
        vec![
            LabelEntry {
                label: "imp1".into(),
                space: "MNI152NLin2009bAsym".into(),
                state: LabelState::Importable
            },
            LabelEntry {
                label: "lab1".into(),
                space: "native".into(),
                state: LabelState::Canonical
            },
        ]
    );
    assert!(matches!(
        load(dir.path(), "01", "nope"),
        Err(StimError::NotFound(_))
    ));
    assert!(matches!(
        load(dir.path(), "01", "../x"),
        Err(StimError::Invalid(_))
    ));
}
