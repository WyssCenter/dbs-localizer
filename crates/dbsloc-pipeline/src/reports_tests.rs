#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use dbsloc_reports::model::Vendor;
use serde_json::json;

const PDF: &str = "../../tests/fixtures/reports/synthetic/ring_percept_pc.pdf";
const UIPREFS: &str = "../../fastwarp/crates/fastwarp-matfile/tests/data/uiprefs_contiguous.mat";

fn pdf_bytes() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(PDF);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn subject(root: &Path) -> Subject {
    for (ses, modality) in [("preop", "T1w"), ("postop", "CT")] {
        let p = root
            .join("rawdata")
            .join("sub-1")
            .join(format!("ses-{ses}"))
            .join("anat")
            .join(format!("sub-1_ses-{ses}_{modality}.nii.gz"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
    }
    crate::bids::resolve_subject_in(root, None, "1", false).unwrap()
}

fn contact(label: &str, k: u32, polarity: &str, amplitude: Option<f64>) -> Value {
    json!({"label": label, "k": k, "polarity": polarity, "amplitude": amplitude})
}

fn program(
    index: u32,
    mode: &str,
    amplitude: f64,
    case: &str,
    contacts: Vec<Value>,
    matches: Option<bool>,
) -> Value {
    json!({
        "index": index,
        "mode": mode,
        "amplitude": amplitude,
        "unit": if mode == "voltage" { "V" } else { "mA" },
        "pulse_width_us": 60,
        "frequency_hz": 130,
        "limits": {"lower": null, "upper": null},
        "case": case,
        "contacts": contacts,
        "contact_sum_matches_amplitude": matches,
        "sensing": null,
    })
}

fn group(id: &str, name: &str, left: Vec<Value>, right: Vec<Value>) -> Value {
    json!({
        "id": id,
        "name": name,
        "active": id == "A",
        "hemispheres": {
            "left": {"programs": left},
            "right": {"programs": right},
        },
        "cycling": null,
        "soft_start_stop": null,
    })
}

fn lead(hemisphere: &str, first: u32, catalogue: &str) -> Value {
    json!({
        "hemisphere": hemisphere,
        "vendor_model": "3389",
        "catalogue_model": catalogue,
        "target": "STN",
        "contacts": (0..4).map(|i| json!({
            "label": (first + i).to_string(), "device_number": first + i, "k": i + 1,
        })).collect::<Vec<_>>(),
        "orientation_deg": null,
        "tip_offset": null,
    })
}

fn record_with(catalogue: &str) -> SessionReport {
    let mut r = SessionReport::blank(Vendor::Medtronic, "percept_session_report");
    r.source.filename = "ring_percept_pc.pdf".to_owned();
    r.source.sha256 = sha256_hex(&pdf_bytes());
    r.source.pages = 10;
    r.source.printed_at = Some("2026-10-14T15:02".to_owned());
    r.patient.name = Some("Sample Patient".to_owned());
    r.patient.patient_id = Some("SYN:0001".to_owned());
    r.patient.date_of_birth = Some("1961-03-17".to_owned());
    r.device.serial = Some("SYN_PC_0001".to_owned());
    r.device.implanted = Some("2026-01-20".to_owned());
    r.session.date = Some("2026-10-14T14:25".to_owned());
    let left_a = program(
        1,
        "current",
        4.5,
        "anode",
        vec![
            contact("0", 1, "cathode", Some(1.2)),
            contact("1", 2, "cathode", Some(1.5)),
            contact("2", 3, "cathode", Some(1.0)),
            contact("3", 4, "off", None),
        ],
        Some(false),
    );
    let right_a = program(
        1,
        "current",
        2.0,
        "anode",
        vec![
            contact("8", 1, "off", None),
            contact("9", 2, "cathode", Some(2.0)),
            contact("10", 3, "off", None),
            contact("11", 4, "off", None),
        ],
        None,
    );
    let voltage = program(
        1,
        "voltage",
        2.0,
        "off",
        vec![
            contact("0", 1, "cathode", None),
            contact("1", 2, "anode", None),
        ],
        None,
    );
    let no_currents = program(
        1,
        "current",
        3.0,
        "anode",
        vec![
            contact("0", 1, "cathode", None),
            contact("1", 2, "cathode", None),
            contact("2", 3, "cathode", None),
        ],
        None,
    );
    let inter_1 = program(
        1,
        "current",
        1.0,
        "anode",
        vec![contact("0", 1, "cathode", Some(1.0))],
        None,
    );
    let inter_2 = program(
        2,
        "current",
        2.0,
        "anode",
        vec![contact("2", 3, "cathode", Some(2.0))],
        None,
    );
    let zero = program(
        1,
        "current",
        0.0,
        "anode",
        vec![contact("1", 2, "cathode", Some(0.0))],
        None,
    );
    let leads = json!([lead("left", 0, catalogue), lead("right", 8, catalogue)]);
    r.leads = serde_json::from_value(leads).unwrap();
    r.snapshots.final_.active_group = Some("A".to_owned());
    r.snapshots.final_.groups = serde_json::from_value(json!([
        group("A", "Walking", vec![left_a.clone()], vec![right_a.clone()]),
        group("B", "Volt", vec![voltage], vec![]),
        group("C", "Off", vec![zero], vec![]),
        group("D", "Equal", vec![no_currents], vec![]),
        group("E", "Inter", vec![inter_1, inter_2], vec![]),
    ]))
    .unwrap();
    r.snapshots.initial.active_group = Some("A".to_owned());
    r.snapshots.initial.groups =
        serde_json::from_value(json!([group("A", "Walking", vec![left_a], vec![right_a])]))
            .unwrap();
    r.impedances = serde_json::from_value(json!([
        {"hemisphere": "left", "kind": "monopolar", "a": "case", "b": "0", "ohm": 1180, "status": "ok"},
        {"hemisphere": "left", "kind": "monopolar", "a": "case", "b": "1", "ohm": 2410, "status": "ok"},
        {"hemisphere": "left", "kind": "bipolar", "a": "0", "b": "1", "ohm": 2650, "status": "ok"},
    ]))
    .unwrap();
    r
}

fn record() -> SessionReport {
    record_with("Medtronic 3389")
}

fn only(groups: &[&str]) -> ImportOptions {
    ImportOptions {
        groups: Some(groups.iter().map(|g| (*g).to_owned()).collect()),
        ..ImportOptions::default()
    }
}

fn percents(stim: &Stimulation, side: Side, source: usize) -> Vec<f64> {
    stim.sides.get(side).sources[source - 1]
        .contacts
        .iter()
        .map(|c| c.percent)
        .collect()
}

fn load_label(sub: &Subject, label: &str) -> Stimulation {
    load(&sub.subj_dir, &sub.subj_id, label).unwrap()
}

#[test]
fn shares_are_exact() {
    let v = proportional_shares(&[1.2, 1.5, 1.0]).unwrap();
    assert_eq!(v, vec![32.43, 40.54, 27.03]);
    assert_eq!(
        v.iter().map(|x| (x * 100.0).round() as i64).sum::<i64>(),
        10_000
    );
    assert_eq!(equal_shares(3), vec![33.33, 33.33, 33.34]);
    assert_eq!(equal_shares(2), vec![50.0, 50.0]);
    assert!(proportional_shares(&[0.0, 0.0]).is_none());
}

#[test]
fn import_writes_record_index_text_copy_and_final_labels() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let out = import_record(&sub, record(), &pdf_bytes(), &ImportOptions::default()).unwrap();
    assert!(out.record_path.is_file());
    assert!(out.record_path.ends_with(
        "clinical/reports/medtronic/202610141425_".to_owned()
            + &sha256_hex(&pdf_bytes())[..8]
            + "/sub-1_desc-sessionreport.json"
    ));
    let txt = out.record_path.with_extension("txt");
    assert!(txt.is_file());
    assert!(std::fs::read_to_string(&txt).unwrap().contains('\u{c}'));
    let copy = out.source_copy.clone().unwrap();
    assert_eq!(
        copy,
        sub.sourcedata_dir
            .join("reports/medtronic/ring_percept_pc.pdf")
    );
    assert_eq!(std::fs::read(&copy).unwrap(), pdf_bytes());
    assert_eq!(
        out.labels_written,
        vec!["20261014_A", "20261014_B", "20261014_D", "20261014_E"]
    );
    assert!(out.labels_failed.is_empty(), "{:?}", out.labels_failed);
    assert!(!out.anonymized);

    let listed = list_reports(&sub);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].labels, out.labels_written);
    assert_eq!(listed[0].vendor, "medtronic");
    assert!(!listed[0].anonymized);
    assert!(out.index_path.is_file());
    for label in &out.labels_written {
        assert!(stimulation_json_path(&sub.subj_dir, "1", label).is_file());
        assert!(stimparameters_mat_path(&sub.subj_dir, "1", "native", label).is_file());
    }
    let stim = load_label(&sub, "20261014_A");
    let from = stim.provenance.imported_from.unwrap();
    assert_eq!(from["snapshot"], "final");
    assert_eq!(from["group"], "A");
    assert!(from["report"]
        .as_str()
        .unwrap()
        .starts_with("clinical/reports/medtronic/"));
}

#[test]
fn percentages_case_impedance_and_frequency() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let out = import_record(&sub, record(), &pdf_bytes(), &ImportOptions::default()).unwrap();
    let a = load_label(&sub, "20261014_A");
    assert_eq!(percents(&a, Side::Left, 1), vec![32.43, 40.54, 27.03, 0.0]);
    let left = &a.sides.left.sources[0];
    assert_eq!(left.case.polarity, Polarity::Anode);
    assert_eq!(left.case.percent, 100.0);
    assert_eq!(left.amplitude, 4.5);
    assert_eq!(left.mode, Mode::Current);
    assert_eq!(left.frequency_hz, Some(130.0));
    assert_eq!(left.pulse_width_us, 60.0);
    assert_eq!(left.contacts[0].impedance_kohm, 1.18);
    assert_eq!(left.contacts[1].impedance_kohm, 2.41);
    assert_eq!(left.contacts[2].impedance_kohm, 1.0);
    assert_eq!(percents(&a, Side::Right, 1), vec![0.0, 100.0, 0.0, 0.0]);
    assert!(!a.sides.left.sources[1].is_active());
    for label in &out.labels_written {
        let s = load_label(&sub, label);
        for side in Side::ALL {
            for src in &s.sides.get(side).sources {
                for pol in [Polarity::Cathode, Polarity::Anode] {
                    let members: f64 = src
                        .contacts
                        .iter()
                        .filter(|c| c.polarity == pol)
                        .map(|c| c.percent)
                        .sum::<f64>()
                        + if src.case.polarity == pol {
                            src.case.percent
                        } else {
                            0.0
                        };
                    if src.is_active() && members > 0.0 {
                        assert!((members - 100.0).abs() < 1e-9, "{label} {side} {pol:?}");
                    }
                }
            }
        }
    }
    assert!(out.warnings.iter().any(|w| w.contains(
        "20261014_A: printed contact currents sum to 3.7 mA, amplitude 4.5 mA; percentages follow the printed shares"
    )));
}

#[test]
fn equal_shares_without_printed_currents_and_voltage_marks_usage() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    import_record(&sub, record(), &pdf_bytes(), &ImportOptions::default()).unwrap();
    let d = load_label(&sub, "20261014_D");
    assert_eq!(percents(&d, Side::Left, 1), vec![33.33, 33.33, 33.34, 0.0]);
    let b = load_label(&sub, "20261014_B");
    let src = &b.sides.left.sources[0];
    assert_eq!(src.mode, Mode::Voltage);
    assert_eq!(src.contacts[0].polarity, Polarity::Cathode);
    assert_eq!(src.contacts[0].percent, 100.0);
    assert_eq!(src.contacts[1].polarity, Polarity::Anode);
    assert_eq!(src.contacts[1].percent, 100.0);
    assert_eq!(src.case.polarity, Polarity::Off);
}

#[test]
fn interleaving_maps_to_sources_one_and_two() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let out = import_record(&sub, record(), &pdf_bytes(), &ImportOptions::default()).unwrap();
    let e = load_label(&sub, "20261014_E");
    assert_eq!(e.sides.left.sources[0].amplitude, 1.0);
    assert_eq!(e.sides.left.sources[1].amplitude, 2.0);
    assert_eq!(e.sides.left.sources[0].contacts[0].percent, 100.0);
    assert_eq!(e.sides.left.sources[1].contacts[2].percent, 100.0);
    assert!(out
        .warnings
        .iter()
        .any(|w| w.contains("W_REPORT_interleaving")));
}

#[test]
fn reimport_is_idempotent_and_changed_settings_get_a_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let first = import_record(&sub, record(), &pdf_bytes(), &ImportOptions::default()).unwrap();
    let second = import_record(&sub, record(), &pdf_bytes(), &ImportOptions::default()).unwrap();
    assert!(second.labels_written.is_empty());
    assert_eq!(second.labels_unchanged, first.labels_written);
    assert_eq!(first.record_path, second.record_path);
    assert_eq!(first.source_copy, second.source_copy);
    assert_eq!(list_reports(&sub).len(), 1);

    let mut changed = record();
    changed.snapshots.final_.groups[0]
        .hemispheres
        .left
        .as_mut()
        .unwrap()
        .programs[0]
        .amplitude = Some(6.5);
    let third = import_record(&sub, changed, &pdf_bytes(), &only(&["A"])).unwrap();
    assert_eq!(third.labels_written, vec!["20261014_A_2"]);
    let again = {
        let mut c = record();
        c.snapshots.final_.groups[0]
            .hemispheres
            .left
            .as_mut()
            .unwrap()
            .programs[0]
            .amplitude = Some(6.5);
        import_record(&sub, c, &pdf_bytes(), &only(&["A"])).unwrap()
    };
    assert_eq!(again.labels_unchanged, vec!["20261014_A_2"]);
    assert_eq!(
        load_label(&sub, "20261014_A_2").sides.left.sources[0].amplitude,
        6.5
    );
    assert_eq!(
        load_label(&sub, "20261014_A").sides.left.sources[0].amplitude,
        4.5
    );
}

#[test]
fn same_file_name_with_other_content_gets_a_numbered_copy() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let a = copy_source(&sub, "medtronic", "r.pdf", b"one").unwrap();
    let b = copy_source(&sub, "medtronic", "r.pdf", b"two").unwrap();
    let c = copy_source(&sub, "medtronic", "r.pdf", b"two").unwrap();
    assert!(a.ends_with("r.pdf"));
    assert!(b.ends_with("r-2.pdf"));
    assert_eq!(b, c);
}

#[test]
fn snapshot_groups_and_prefix_options() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let initial = ImportOptions {
        snapshot: SnapshotChoice::Initial,
        label_prefix: Some("pre".to_owned()),
        ..ImportOptions::default()
    };
    let out = import_record(&sub, record(), &pdf_bytes(), &initial).unwrap();
    assert_eq!(out.labels_written, vec!["pre_20261014_A_initial"]);
    let both = ImportOptions {
        snapshot: SnapshotChoice::Both,
        groups: Some(vec!["A".to_owned()]),
        ..ImportOptions::default()
    };
    let out = import_record(&sub, record(), &pdf_bytes(), &both).unwrap();
    assert_eq!(out.labels_written, vec!["20261014_A", "20261014_A_initial"]);
    let none = ImportOptions {
        snapshot: SnapshotChoice::None,
        ..ImportOptions::default()
    };
    let out = import_record(&sub, record(), &pdf_bytes(), &none).unwrap();
    assert!(out.labels_written.is_empty() && out.labels_unchanged.is_empty());
    let out = import_record(&sub, record(), &pdf_bytes(), &only(&["C", "Z"])).unwrap();
    assert_eq!(out.labels_written, vec!["20261014_C"]);
    assert_eq!(out.labels_failed.len(), 1);
    assert!(out.labels_failed[0].1.contains("no group"));
    assert!(!load_label(&sub, "20261014_C").sides.left.sources[0].is_active());
    assert_eq!(SnapshotChoice::parse("Both"), Some(SnapshotChoice::Both));
    assert_eq!(SnapshotChoice::parse("x"), None);
}

#[test]
fn unknown_lead_model_fails_the_label_but_keeps_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let out = import_record(
        &sub,
        record_with("Medtronic X"),
        &pdf_bytes(),
        &only(&["A"]),
    )
    .unwrap();
    assert!(out.record_path.is_file());
    assert!(out.labels_written.is_empty());
    assert_eq!(out.labels_failed.len(), 1);
    assert!(out.labels_failed[0].1.starts_with("E_REPORT_lead_model"));
    assert!(list_reports(&sub)[0].labels.is_empty());
}

#[test]
fn a_differing_subject_model_warns() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(UIPREFS);
    std::fs::create_dir_all(&sub.prefs_dir).unwrap();
    std::fs::copy(&fixture, sub.uiprefs_path()).unwrap();
    let out = import_record(&sub, record(), &pdf_bytes(), &only(&["A"])).unwrap();
    assert!(out
        .warnings
        .iter()
        .any(|w| w.contains("W_REPORT_model_mismatch")));
    assert_eq!(out.labels_written, vec!["20261014_A"]);
}

#[test]
fn anonymised_import_removes_identity_and_reuses_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("keys").join("anon.key");
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, [5u8; 32]).unwrap();
    let opts = ImportOptions {
        anonymize: true,
        key_file: Some(key.clone()),
        ..ImportOptions::default()
    };
    let first = import_record(&sub, record(), &pdf_bytes(), &opts).unwrap();
    assert!(first.anonymized);
    assert!(first.source_copy.is_none());
    assert!(!first.record_path.with_extension("txt").exists());
    assert!(!sub.sourcedata_dir.join("reports").exists());
    let text = std::fs::read_to_string(&first.record_path).unwrap();
    for secret in ["Sample Patient", "SYN:0001", "SYN_PC_0001", "1961-03-17"] {
        assert!(!text.contains(secret), "{secret}");
    }
    assert!(text.contains("\"anonymized\": true"));
    assert!(first
        .warnings
        .iter()
        .any(|w| w.contains("W_REPORT_free_text") && w.contains("Walking")));
    let bytes = std::fs::read(&key).unwrap();
    assert_eq!(bytes.len(), 32);

    let second = import_record(&sub, record(), &pdf_bytes(), &opts).unwrap();
    assert_eq!(std::fs::read(&key).unwrap(), bytes);
    assert_eq!(first.record_path, second.record_path);
    assert!(second.labels_written.is_empty());
    assert!(!second.labels_unchanged.is_empty());
    assert!(second
        .warnings
        .iter()
        .all(|w| !w.contains("W_REPORT_shift_mismatch")));
    let shifted: SessionReport = serde_json::from_str(&text).unwrap();
    let raw = record();
    assert_ne!(shifted.session.date, raw.session.date);
    let entry = &list_reports(&sub)[0];
    assert!(entry.anonymized);
    assert_eq!(entry.session, shifted.session.date);
}

fn old_formula_days(key: &[u8], patient_key: &str) -> i32 {
    let mac = dbsloc_reports::anonymize::hmac_sha256(
        key,
        format!("dbsloc-date-shift:{patient_key}").as_bytes(),
    );
    let mut head = [0u8; 8];
    head.copy_from_slice(&mac[..8]);
    (u64::from_be_bytes(head) % 61) as i32 - 30
}

fn zero_shift_key() -> [u8; 32] {
    (0u32..100_000)
        .map(|i| {
            let mut k = [0u8; 32];
            for chunk in k.chunks_mut(4) {
                chunk.copy_from_slice(&i.to_le_bytes());
            }
            k
        })
        .find(|k| old_formula_days(k, "sub-1") == 0)
        .unwrap()
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.clone());
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    out
}

fn anon_opts(key: PathBuf) -> ImportOptions {
    ImportOptions {
        anonymize: true,
        key_file: Some(key),
        ..ImportOptions::default()
    }
}

fn old_state(sub: &Subject) -> (PathBuf, Vec<String>) {
    let zero = anonymize_preview(&record());
    let opts = ImportOptions {
        source_copy: false,
        ..ImportOptions::default()
    };
    let out = import_record(sub, zero, &pdf_bytes(), &opts).unwrap();
    assert!(!out.labels_written.is_empty());
    (
        out.record_path.parent().unwrap().to_path_buf(),
        out.labels_written,
    )
}

#[test]
fn an_anonymised_import_supersedes_a_record_written_with_a_zero_shift() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let (old_dir, old_labels) = old_state(&sub);
    assert!(old_dir.is_dir());
    let key = dir.path().join("zero.key");
    std::fs::write(&key, zero_shift_key()).unwrap();
    let out = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(out.record_path.is_file());
    assert!(!old_dir.exists());
    let sha = record().source.sha256;
    let entries: Vec<_> = list_reports(&sub)
        .into_iter()
        .filter(|e| e.sha256 == sha)
        .collect();
    assert_eq!(entries.len(), 1);
    assert_ne!(
        Some(entries[0].folder.as_str()),
        old_dir
            .strip_prefix(reports_root(&sub))
            .ok()
            .and_then(|p| p.to_str())
    );
    let warning = out
        .warnings
        .iter()
        .find(|w| w.contains("W_REPORT_superseded"))
        .unwrap();
    assert!(warning.contains(&format!("{} stimulation label folder(s)", old_labels.len())));
    assert!(warning.contains("dbsloc stim list"));
    assert!(warning.contains("delete them"));
    assert!(!warning.contains("rename"));
    let old_name = old_dir.file_name().unwrap().to_string_lossy().into_owned();
    for needle in ["2026-10-14", "20261014", "1961", old_name.as_str()] {
        assert!(!warning.contains(needle), "{needle}");
    }
    for label in &old_labels {
        assert!(!warning.contains(label.as_str()), "{label}");
        assert!(stimulation_json_path(&sub.subj_dir, &sub.subj_id, label).is_file());
    }
    for w in &out.warnings {
        for needle in ["2026-10-14", "20261014", "1961-03-17"] {
            assert!(!w.contains(needle), "{needle} in {w}");
        }
    }
    for p in files_under(&reports_root(&sub)) {
        let name = p.to_string_lossy();
        assert!(!name.contains("20261014"), "{name}");
        if p.is_file() {
            let text = std::fs::read_to_string(&p).unwrap();
            for date in ["1961-03-17", "2026-10-14"] {
                assert!(!text.contains(date), "{date} in {name}");
            }
        }
    }
}

#[test]
fn a_superseded_entry_outside_the_reports_folder_is_kept_and_warns() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let first = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    let outside = reports_root(&sub).join("..").join("outside_copy");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
    let mut entry = list_reports(&sub).remove(0);
    entry.folder = "../outside_copy".to_owned();
    let mut index = read_index(&first.index_path);
    index.reports.push(entry);
    std::fs::write(
        &first.index_path,
        serde_json::to_string_pretty(&index).unwrap(),
    )
    .unwrap();
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(outside.join("keep.txt").is_file());
    assert!(second
        .warnings
        .iter()
        .any(|w| w.contains("W_REPORT_superseded") && w.contains("was not removed")));
    assert!(list_reports(&sub)
        .iter()
        .any(|e| e.folder == "../outside_copy"));
}

#[test]
fn only_a_different_anonymised_folder_is_superseded() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let first = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    let plain = import_record(&sub, record(), &pdf_bytes(), &ImportOptions::default()).unwrap();
    assert!(plain
        .warnings
        .iter()
        .all(|w| !w.contains("W_REPORT_superseded")));
    assert!(first.record_path.is_file());
    assert_eq!(list_reports(&sub).len(), 2);
    let again = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert_eq!(again.record_path, first.record_path);
    assert!(again
        .warnings
        .iter()
        .all(|w| !w.contains("W_REPORT_superseded")));
    assert!(first.record_path.is_file());
    assert_eq!(list_reports(&sub).len(), 2);
}

#[test]
fn labels_of_an_anonymised_import_carry_no_write_time() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let anon = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(!anon.labels_written.is_empty());
    let now = crate::version::utc_now_rfc3339();
    for label in &anon.labels_written {
        let text =
            std::fs::read_to_string(stimulation_json_path(&sub.subj_dir, &sub.subj_id, label))
                .unwrap();
        assert!(!text.contains(&now[..10]), "{label}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["provenance"]["written_at"], "");
        assert!(v["provenance"]["software"].get("written_at").is_none());
    }
    let dir2 = tempfile::tempdir().unwrap();
    let sub2 = subject(dir2.path());
    let plain = import_record(&sub2, record(), &pdf_bytes(), &ImportOptions::default()).unwrap();
    assert!(!plain.labels_written.is_empty());
    for label in &plain.labels_written {
        let stim = load(&sub2.subj_dir, &sub2.subj_id, label).unwrap();
        assert!(!stim.provenance.written_at.is_empty());
        assert!(stim.provenance.software.get("written_at").is_some());
    }
}

#[test]
fn an_anonymised_import_of_a_parsed_report_stores_no_import_time() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let pdf = Path::new(env!("CARGO_MANIFEST_DIR")).join(PDF);
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let out = import_report(&sub, &pdf, &anon_opts(key)).unwrap();
    let text = std::fs::read_to_string(&out.record_path).unwrap();
    let stored: SessionReport = serde_json::from_str(&text).unwrap();
    assert_eq!(stored.source.parsed_at, None);
    let now = crate::version::utc_now_rfc3339();
    assert!(!text.contains(&now[..10]));
    let plain = import_report(&sub, &pdf, &ImportOptions::default()).unwrap();
    let plain_text = std::fs::read_to_string(&plain.record_path).unwrap();
    assert!(plain_text.contains("\"parsed_at\": \""));
}

#[test]
fn a_key_whose_old_shift_was_zero_still_moves_every_date() {
    let key_bytes = zero_shift_key();
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("zero.key");
    std::fs::write(&key, key_bytes).unwrap();
    let opts = ImportOptions {
        anonymize: true,
        key_file: Some(key),
        snapshot: SnapshotChoice::None,
        ..ImportOptions::default()
    };
    let out = import_record(&sub, record(), &pdf_bytes(), &opts).unwrap();
    let text = std::fs::read_to_string(&out.record_path).unwrap();
    for date in ["1961-03-17", "2026-10-14"] {
        assert!(!text.contains(date), "{date}");
    }
    let shifted: SessionReport = serde_json::from_str(&text).unwrap();
    assert_ne!(shifted.session.date, record().session.date);
    assert_ne!(
        shifted.patient.date_of_birth,
        record().patient.date_of_birth
    );
}

#[test]
fn a_different_key_warns_about_the_shift() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let mut keys: Vec<(PathBuf, i32)> = Vec::new();
    for i in 0..40u8 {
        let p = dir.path().join(format!("k{i}.key"));
        std::fs::write(&p, [i + 1; 32]).unwrap();
        keys.push((p, shift_for(&[i + 1; 32], "sub-1").days));
    }
    let first = keys[0].clone();
    let other = keys.iter().find(|k| k.1 != first.1).unwrap().clone();
    let opts = |p: PathBuf| ImportOptions {
        anonymize: true,
        key_file: Some(p),
        snapshot: SnapshotChoice::None,
        ..ImportOptions::default()
    };
    let before = import_record(&sub, record(), &pdf_bytes(), &opts(first.0)).unwrap();
    let out = import_record(&sub, record(), &pdf_bytes(), &opts(other.0)).unwrap();
    assert!(out
        .warnings
        .iter()
        .any(|w| w.contains("W_REPORT_shift_mismatch")));
    assert!(out
        .warnings
        .iter()
        .all(|w| !w.contains("W_REPORT_superseded")));
    assert!(before.record_path.is_file());
    assert!(out.record_path.is_file());
    assert_ne!(before.record_path, out.record_path);
    assert_eq!(list_reports(&sub).len(), 2);
    for w in &out.warnings {
        for needle in ["2026-10-14", "20261014", "1961-03-17"] {
            assert!(!w.contains(needle), "{needle} in {w}");
        }
    }
}

fn add_index_entry(sub: &Subject, folder: &str) {
    let path = index_path(sub);
    let mut entry = list_reports(sub).remove(0);
    entry.folder = folder.to_owned();
    let mut index = read_index(&path);
    index.reports.push(entry);
    std::fs::write(&path, serde_json::to_string_pretty(&index).unwrap()).unwrap();
}

fn short_sha() -> String {
    record().source.sha256[..8].to_owned()
}

#[test]
fn a_vendor_folder_entry_or_a_case_variant_never_deletes_the_new_record() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let first = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    let new_folder = first
        .record_path
        .parent()
        .unwrap()
        .strip_prefix(reports_root(&sub))
        .unwrap()
        .to_string_lossy()
        .replace(char::from(92), "/");
    add_index_entry(&sub, "medtronic");
    add_index_entry(&sub, &new_folder.replacen("medtronic", "MEDTRONIC", 1));
    add_index_entry(&sub, &new_folder.to_uppercase());
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(second.record_path.is_file());
    assert!(reports_root(&sub).join("medtronic").is_dir());
    assert!(second
        .warnings
        .iter()
        .any(|w| w.contains("W_REPORT_superseded")));
}

#[test]
fn the_folder_guard_refuses_the_new_folder_and_its_ancestors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("reports");
    let new = format!("medtronic/202610141425_{}", short_sha());
    std::fs::create_dir_all(root.join(&new)).unwrap();
    assert!(inside_reports(&root, &new, &new).is_none());
    assert!(inside_reports(&root, "medtronic", &new).is_none());
    let other = format!("medtronic/202610011111_{}", short_sha());
    std::fs::create_dir_all(root.join(&other)).unwrap();
    assert!(inside_reports(&root, &other, &new).is_some());
    assert!(is_report_folder_name(&new, &record().source.sha256));
    assert!(!is_report_folder_name("medtronic", &record().source.sha256));
    assert!(!is_report_folder_name(
        &new.to_uppercase(),
        &record().source.sha256
    ));
}

#[test]
fn a_link_inside_the_reports_folder_pointing_elsewhere_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let first = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    let outside = dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let name = format!("sub-{}_desc-sessionreport.json", sub.subj_id);
    std::fs::write(outside.join(&name), canonical_json(&record()).unwrap()).unwrap();
    let rel = format!("medtronic/202610141425_{}", short_sha());
    let link = reports_root(&sub).join(&rel);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, &link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&outside, &link);
    if made.is_err() {
        return;
    }
    add_index_entry(&sub, &rel);
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(outside.join(&name).is_file());
    assert!(first.record_path.is_file());
    assert!(second.record_path.is_file());
    assert!(second
        .warnings
        .iter()
        .any(|w| w.contains("W_REPORT_superseded") && w.contains("not removed")));
}

#[test]
fn a_new_key_is_announced_once() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("fresh").join("new.key");
    let first = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    let hit: Vec<&String> = first
        .warnings
        .iter()
        .filter(|w| w.contains("W_REPORT_key_created"))
        .collect();
    assert_eq!(hit.len(), 1);
    assert!(hit[0].contains(&key.display().to_string()));
    let bytes = std::fs::read(&key).unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert!(!hit[0].contains(&hex));
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(second
        .warnings
        .iter()
        .all(|w| !w.contains("W_REPORT_key_created")));
}

#[test]
fn an_earlier_stamped_label_loses_its_write_time_on_an_anonymised_reimport() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let first = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    assert!(!first.labels_written.is_empty());
    let label = first.labels_written[0].clone();
    let stim = load(&sub.subj_dir, &sub.subj_id, &label).unwrap();
    save(&sub.subj_dir, &stim).unwrap();
    assert!(!load(&sub.subj_dir, &sub.subj_id, &label)
        .unwrap()
        .provenance
        .written_at
        .is_empty());
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    assert!(second.labels_unchanged.contains(&label));
    let back = load(&sub.subj_dir, &sub.subj_id, &label).unwrap();
    assert!(back.provenance.written_at.is_empty());
    assert!(back.provenance.software.get("written_at").is_none());
    assert_eq!(back.sides, stim.sides);

    let mut edited = back.clone();
    edited.sides.get_mut(Side::Right).sources[0].amplitude += 0.1;
    edited.sides.get_mut(Side::Left).sources[0].amplitude += 0.1;
    save(&sub.subj_dir, &edited).unwrap();
    let third = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(!third.labels_unchanged.contains(&label));
    let kept = load(&sub.subj_dir, &sub.subj_id, &label).unwrap();
    assert!(!kept.provenance.written_at.is_empty());
    assert_eq!(kept.sides, edited.sides);
}

#[test]
fn key_file_is_created_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a").join("b.key");
    let one = load_or_create_key(&path).unwrap();
    let two = load_or_create_key(&path).unwrap();
    assert_eq!(one, two);
    assert_eq!(one.len(), 32);
    std::fs::write(&path, b"").unwrap();
    assert!(matches!(
        load_or_create_key(&path),
        Err(ReportImportError::Key(_))
    ));
    assert_ne!(random_key(), random_key());
}

#[test]
fn errors_carry_their_codes() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("x.pdf");
    std::fs::write(&pdf, b"not a pdf").unwrap();
    let err = inspect(&pdf).unwrap_err();
    assert!(err.to_string().starts_with("E_REPORT_"), "{err}");
    let err = inspect(&dir.path().join("missing.pdf")).unwrap_err();
    assert!(err.to_string().starts_with("E_REPORT_io"), "{err}");
    assert!(ReportImportError::Key("k".into())
        .to_string()
        .starts_with("E_REPORT_key"));
}

#[test]
fn anonymize_preview_has_no_identity() {
    let p = anonymize_preview(&record());
    assert!(p.anonymized);
    assert!(p.patient.name.is_none());
    assert_eq!(p.session.date, record().session.date);
}

fn read_vars(path: &Path) -> Vec<(String, fastwarp_matfile::mat5::Mat5Value)> {
    fastwarp_matfile::mat5::read_mat5(&std::fs::read(path).unwrap()).unwrap()
}

fn without_provenance(
    vars: &[(String, fastwarp_matfile::mat5::Mat5Value)],
) -> Vec<(String, fastwarp_matfile::mat5::Mat5Value)> {
    vars.iter()
        .filter(|(n, _)| n != crate::vat::settings::PROVENANCE_VARIABLE)
        .cloned()
        .collect()
}

fn provenance_has_write_time(path: &Path) -> bool {
    read_vars(path)
        .iter()
        .find(|(n, _)| n == crate::vat::settings::PROVENANCE_VARIABLE)
        .and_then(|(_, v)| v.field("written_at", 0).cloned())
        .is_some()
}

fn stamped_label_with_volumes(sub: &Subject, key: &Path) -> (String, Vec<PathBuf>) {
    let first = import_record(sub, record(), &pdf_bytes(), &anon_opts(key.to_path_buf())).unwrap();
    let label = first.labels_written[0].clone();
    let stim = load(&sub.subj_dir, &sub.subj_id, &label).unwrap();
    save(&sub.subj_dir, &stim).unwrap();
    let stamped = load(&sub.subj_dir, &sub.subj_id, &label).unwrap();
    let mut mats = Vec::new();
    for space in ["native", "template"] {
        let path = stimparameters_mat_path(&sub.subj_dir, &sub.subj_id, space, &label);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        crate::vat::settings::write_stimparameters_mat(&path, &stamped, [123.5, 56.25]).unwrap();
        assert!(provenance_has_write_time(&path));
        mats.push(path);
    }
    (label, mats)
}

#[test]
fn a_reimport_keeps_the_volumes_of_a_vat_written_mat_and_only_drops_the_write_time() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let (label, mats) = stamped_label_with_volumes(&sub, &key);
    let before: Vec<_> = mats.iter().map(|m| read_vars(m)).collect();
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(second.labels_unchanged.contains(&label));
    assert!(second
        .warnings
        .iter()
        .all(|w| !w.contains("W_REPORT_stamp_kept")));
    for (mat, old) in mats.iter().zip(&before) {
        let new = read_vars(mat);
        assert!(!provenance_has_write_time(mat));
        let (a, b) = (without_provenance(old), without_provenance(&new));
        assert_eq!(
            fastwarp_matfile::mat5::mat5_bytes(&a, false).unwrap(),
            fastwarp_matfile::mat5::mat5_bytes(&b, false).unwrap()
        );
        let s = &new.iter().find(|(n, _)| n == "S").unwrap().1;
        assert_eq!(
            s.field("volume", 0).and_then(|v| v.as_rows()),
            Some(vec![vec![123.5, 56.25]])
        );
        let old_prov = old
            .iter()
            .find(|(n, _)| n == crate::vat::settings::PROVENANCE_VARIABLE)
            .unwrap();
        let new_prov = new
            .iter()
            .find(|(n, _)| n == crate::vat::settings::PROVENANCE_VARIABLE)
            .unwrap();
        for name in ["product", "version", "git_hash", "source_json_sha256"] {
            assert_eq!(
                old_prov.1.field(name, 0),
                new_prov.1.field(name, 0),
                "{name}"
            );
        }
    }
}

#[test]
fn a_mat_that_cannot_be_cleaned_losslessly_is_kept_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let (label, mats) = stamped_label_with_volumes(&sub, &key);
    let uncompressed = fastwarp_matfile::mat5::mat5_bytes(&read_vars(&mats[0]), false).unwrap();
    std::fs::write(&mats[0], &uncompressed).unwrap();
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(second.labels_unchanged.contains(&label));
    assert_eq!(std::fs::read(&mats[0]).unwrap(), uncompressed);
    assert!(!provenance_has_write_time(&mats[1]));
    let warning = second
        .warnings
        .iter()
        .find(|w| w.contains("W_REPORT_stamp_kept"))
        .unwrap();
    assert!(warning.contains("1 stimulation label(s)"));
    assert!(warning.contains("--overwrite-approved"));
    assert!(warning.contains("for a subject without VTA results"));
    assert!(
        warning.contains("dbsloc stim show <dataset_root> <subj_id> <label> --json > label.json")
    );
    assert!(warning.contains("dbsloc stim set <dataset_root> <subj_id> <label> --from label.json"));
    for needle in [label.as_str(), "2026-10-14", "20261014"] {
        assert!(!warning.contains(needle), "{needle}");
    }
}

#[test]
fn a_hand_deleted_affected_record_still_loses_its_index_row() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let (old_dir, old_labels) = old_state(&sub);
    std::fs::remove_dir_all(&old_dir).unwrap();
    let key = dir.path().join("zero.key");
    std::fs::write(&key, zero_shift_key()).unwrap();
    let out = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    let sha = record().source.sha256;
    let rows: Vec<_> = list_reports(&sub)
        .into_iter()
        .filter(|e| e.sha256 == sha)
        .collect();
    assert_eq!(rows.len(), 1);
    let index_text = std::fs::read_to_string(index_path(&sub)).unwrap();
    for needle in ["2026-10-14", "20261014", "1961-03-17"] {
        assert!(!index_text.contains(needle), "{needle}");
    }
    let warning = out
        .warnings
        .iter()
        .find(|w| w.contains("W_REPORT_superseded"))
        .unwrap();
    assert!(warning.contains("already deleted"));
    assert!(warning.contains(&format!("{} stimulation label folder(s)", old_labels.len())));
    for needle in ["2026-10-14", "20261014"] {
        assert!(!warning.contains(needle), "{needle}");
    }
}

#[test]
fn an_undated_earlier_copy_is_reported_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let first = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    let rel = format!("medtronic/unknown_{}", short_sha());
    let undated = reports_root(&sub).join(&rel);
    std::fs::create_dir_all(&undated).unwrap();
    add_index_entry(&sub, &rel);
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(undated.is_dir());
    assert!(first.record_path.is_file());
    assert!(second.record_path.is_file());
    let warning = second
        .warnings
        .iter()
        .find(|w| w.contains("W_REPORT_superseded"))
        .unwrap();
    assert!(warning.contains("has no session date and could not be checked"));
    assert!(!warning.contains("unknown"));
}

#[test]
fn an_uncleaned_mat_is_tried_again_on_the_next_anonymised_import() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let (label, mats) = stamped_label_with_volumes(&sub, &key);
    let original = std::fs::read(&mats[0]).unwrap();
    let uncompressed = fastwarp_matfile::mat5::mat5_bytes(&read_vars(&mats[0]), false).unwrap();
    std::fs::write(&mats[0], &uncompressed).unwrap();
    let second = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
    assert!(second
        .warnings
        .iter()
        .any(|w| w.contains("W_REPORT_stamp_kept")));
    assert!(load(&sub.subj_dir, &sub.subj_id, &label)
        .unwrap()
        .provenance
        .written_at
        .is_empty());
    assert!(provenance_has_write_time(&mats[0]));
    std::fs::write(&mats[0], &original).unwrap();
    let third = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key)).unwrap();
    assert!(third
        .warnings
        .iter()
        .all(|w| !w.contains("W_REPORT_stamp_kept")));
    assert!(!provenance_has_write_time(&mats[0]));
    assert!(!provenance_has_write_time(&mats[1]));
}

#[test]
fn the_stamp_warning_repeats_while_a_mat_stays_uncleaned() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let key = dir.path().join("k.key");
    std::fs::write(&key, [5u8; 32]).unwrap();
    let (_label, mats) = stamped_label_with_volumes(&sub, &key);
    let uncompressed = fastwarp_matfile::mat5::mat5_bytes(&read_vars(&mats[0]), false).unwrap();
    std::fs::write(&mats[0], &uncompressed).unwrap();
    for _ in 0..2 {
        let out = import_record(&sub, record(), &pdf_bytes(), &anon_opts(key.clone())).unwrap();
        let hits = out
            .warnings
            .iter()
            .filter(|w| w.contains("W_REPORT_stamp_kept"))
            .count();
        assert_eq!(hits, 1);
        assert_eq!(std::fs::read(&mats[0]).unwrap(), uncompressed);
    }
}
