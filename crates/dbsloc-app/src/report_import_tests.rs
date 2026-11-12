#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::time::{Duration, Instant};

use dbsloc_pipeline::reports::model::{Group, Vendor};
use serde_json::json;

use super::*;

const PDF: &str = "../../tests/fixtures/reports/synthetic/ring_percept_pc.pdf";

fn pdf_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(PDF)
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
    dbsloc_pipeline::bids::resolve_subject_in(root, None, "1", false).unwrap()
}

fn group(id: &str, amplitude: f64, active: bool) -> Group {
    serde_json::from_value(json!({
        "id": id,
        "name": null,
        "active": active,
        "hemispheres": {
            "left": {"programs": [{
                "index": 1, "mode": "current", "amplitude": amplitude, "unit": "mA",
                "pulse_width_us": 60, "frequency_hz": 130,
                "limits": {"lower": null, "upper": null},
                "case": "anode", "contacts": [],
                "contact_sum_matches_amplitude": null, "sensing": null
            }]},
            "right": null
        },
        "cycling": null,
        "soft_start_stop": null
    }))
    .unwrap()
}

fn record() -> SessionReport {
    let mut r = SessionReport::blank(Vendor::Medtronic, "percept_session_report");
    r.snapshots.final_.groups = vec![group("A", 2.5, true), group("B", 0.0, false)];
    r.snapshots.initial.groups = vec![group("C", 1.0, false)];
    r
}

fn dialog(root: &Path) -> ReportImportDialog {
    ReportImportDialog::without_worker(vec![pdf_path()], subject(root))
}

fn entry(session: Option<&str>, vendor: &str, anonymized: bool, labels: &[&str]) -> IndexEntry {
    IndexEntry {
        folder: "f".to_owned(),
        vendor: vendor.to_owned(),
        session: session.map(str::to_owned),
        anonymized,
        sha256: String::new(),
        labels: labels.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn report_dialog_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let d = dialog(dir.path());
    assert_eq!(d.snapshot, SnapshotChoice::Final);
    assert!(!d.anonymize);
    assert!(d.source_copy);
    assert!(d.label_prefix.is_empty());
    assert_eq!(d.stage(), Stage::Review);
    assert!(!d.can_import());
}

#[test]
fn report_anonymise_clears_and_restores_the_copy_tick() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = dialog(dir.path());
    d.set_anonymize(true);
    assert!(!d.source_copy);
    assert!(!d.options().source_copy);
    d.set_anonymize(false);
    assert!(d.source_copy);
    assert!(d.options().source_copy);
}

#[test]
fn report_group_ticks_prefill_from_the_chosen_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = dialog(dir.path());
    d.set_report(0, record());
    let ticks: Vec<(&str, bool)> = d
        .groups()
        .iter()
        .map(|g| (g.id.as_str(), g.ticked))
        .collect();
    assert_eq!(ticks, vec![("A", true), ("B", false)]);
    assert_eq!(d.options().groups, Some(vec!["A".to_owned()]));
    d.set_group_ticked("B", true);
    assert_eq!(
        d.options().groups,
        Some(vec!["A".to_owned(), "B".to_owned()])
    );
    d.set_snapshot(SnapshotChoice::Both);
    let ids: Vec<&str> = d.groups().iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, vec!["A", "B", "C"]);
    d.set_snapshot(SnapshotChoice::None);
    assert!(d.groups().is_empty());
    assert_eq!(d.options().groups, None);
    assert_eq!(d.options().snapshot, SnapshotChoice::None);
}

#[test]
fn report_label_prefix_is_validated() {
    assert_eq!(prefix_error(""), None);
    assert_eq!(prefix_error("visit1"), None);
    assert_eq!(prefix_error("v-1.a_b"), None);
    assert!(prefix_error("bad prefix").is_some());
    assert!(prefix_error("a/b").is_some());
    assert!(prefix_error(&"x".repeat(70)).is_some());
    let dir = tempfile::tempdir().unwrap();
    let mut d = dialog(dir.path());
    d.set_report(0, record());
    d.label_prefix = "visit 1".to_owned();
    assert!(!d.can_import());
    d.label_prefix = " visit1 ".to_owned();
    assert!(d.can_import());
    assert_eq!(d.options().label_prefix.as_deref(), Some("visit1"));
}

#[test]
fn report_summary_lines_are_formatted_and_capped() {
    let one = entry(
        Some("2026-10-14T14:25"),
        "medtronic",
        false,
        &["20261014_A", "20261014_B"],
    );
    assert_eq!(
        reports_summary_lines(std::slice::from_ref(&one)),
        vec!["2026-10-14 14:25 · Medtronic · not anonymised · 20261014_A, 20261014_B"]
    );
    let none = entry(None, "boston_scientific", true, &[]);
    assert_eq!(
        reports_summary_lines(&[none]),
        vec!["unknown · Boston Scientific · anonymised · no labels"]
    );
    let many: Vec<IndexEntry> = (0..7).map(|_| one.clone()).collect();
    let lines = reports_summary_lines(&many);
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[5], "… and 2 more");
    assert!(reports_summary_lines(&[]).is_empty());
}

#[test]
fn report_keys_map_to_actions() {
    assert_eq!(
        key_action(Stage::Review, true, true, false),
        KeyAction::Cancel
    );
    assert_eq!(
        key_action(Stage::Importing, false, true, false),
        KeyAction::StopImport
    );
    assert_eq!(
        key_action(Stage::Done, false, true, false),
        KeyAction::Close
    );
    assert_eq!(
        key_action(Stage::Review, true, false, true),
        KeyAction::Import
    );
    assert_eq!(
        key_action(Stage::Review, false, false, true),
        KeyAction::None
    );
    assert_eq!(
        key_action(Stage::Importing, true, false, true),
        KeyAction::None
    );
    assert_eq!(
        key_action(Stage::Review, true, false, false),
        KeyAction::None
    );
}

#[test]
fn report_import_lines_cover_success_and_failure() {
    let report = ImportReport {
        record_path: PathBuf::from("x/clinical/reports/medtronic/20261014_abcd/r.json"),
        labels_written: vec!["a".to_owned(), "b".to_owned()],
        warnings: vec!["dbsloc: warning: report: W_REPORT_x: y".to_owned()],
        labels_failed: vec![("c".to_owned(), "bad".to_owned())],
        ..ImportReport::default()
    };
    let lines = import_lines("f.pdf", &Ok(report));
    assert_eq!(
        lines[0],
        "dbsloc: note: report: f.pdf: record medtronic/20261014_abcd; labels a, b; unchanged none"
    );
    assert_eq!(lines[1], "dbsloc: warning: report: W_REPORT_x: y");
    assert_eq!(
        lines[2],
        "dbsloc: warning: report: f.pdf: label c not written: bad"
    );
    assert_eq!(
        import_lines("f.pdf", &Err("E_REPORT_pdf: nope".to_owned())),
        vec!["dbsloc: error: report: f.pdf: E_REPORT_pdf: nope"]
    );
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(Instant::now() < limit, "the worker did not finish in time");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn report_worker_round_trip_imports_the_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let ctx = egui::Context::default();
    let mut d = ReportImportDialog::new(vec![pdf_path()], sub.clone(), &ctx);
    wait_until(|| {
        d.poll();
        !d.parsing()
    });
    assert!(d.can_import());
    let ids: Vec<&str> = d.groups().iter().map(|g| g.id.as_str()).collect();
    assert!(ids.contains(&"A") && ids.contains(&"D"), "{ids:?}");
    d.start_import(&ctx);
    assert_eq!(d.stage(), Stage::Importing);
    let mut actions = Vec::new();
    wait_until(|| {
        actions.extend(d.poll());
        d.stage() == Stage::Done
    });
    let imported = actions
        .iter()
        .find_map(|a| match a {
            ReportImportAction::Imported { labels, subject } => {
                Some((labels.clone(), subject.clone()))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        imported.0,
        vec!["20261014_A", "20261014_B", "20261014_C", "20261014_D"]
    );
    assert_eq!(imported.1, sub.subj_id);
    let note = "dbsloc: note: report: ring_percept_pc.pdf: record medtronic/";
    assert!(actions
        .iter()
        .any(|a| matches!(a, ReportImportAction::Log(l) if l[0].starts_with(note))));
    let index = read_entries(&sub.subj_dir);
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].labels.len(), 4);
}

#[test]
fn report_card_cache_reads_the_index_once_per_change() {
    let dir = tempfile::tempdir().unwrap();
    let sub = subject(dir.path());
    let ctx = egui::Context::default();
    assert!(cached_entries(&ctx, &sub.subj_dir).is_empty());
    let index = index_file(&sub.subj_dir);
    std::fs::create_dir_all(index.parent().unwrap()).unwrap();
    let json = r#"{"reports":[{"folder":"medtronic/x","vendor":"medtronic","session":"2026-10-14T14:25","anonymized":false,"sha256":"ab","labels":["a"]}]}"#;
    std::fs::write(&index, json).unwrap();
    invalidate_reports_cache(&ctx);
    let entries = cached_entries(&ctx, &sub.subj_dir);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].labels, vec!["a"]);
}

fn foreign_pdf() -> Vec<u8> {
    let stream = "BT /F1 12 Tf 72 720 Td (Jane Roe) Tj 0 -20 Td (MRN 12345) Tj 0 -20 Td (Some other vendor session summary) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>
stream
{stream}
endstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_owned(),
    ];
    let mut out = b"%PDF-1.4
"
    .to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(
            format!(
                "{} 0 obj
{body}
endobj
",
                i + 1
            )
            .as_bytes(),
        );
    }
    let xref = out.len();
    out.extend_from_slice(
        format!(
            "xref
0 {}
0000000000 65535 f 
",
            objects.len() + 1
        )
        .as_bytes(),
    );
    for o in &offsets {
        out.extend_from_slice(
            format!(
                "{o:010} 00000 n 
"
            )
            .as_bytes(),
        );
    }
    out.extend_from_slice(
        format!(
            "trailer
<< /Size {} /Root 1 0 R >>
startxref
{xref}
%%EOF
",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn parse_failure_keeps_document_text_out_of_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("foreign.pdf");
    std::fs::write(&path, foreign_pdf()).unwrap();
    let err = reports::inspect(&path).expect_err("unknown vendor");
    let failure = ParseFailure::from_error(&err);
    assert!(
        failure.message.starts_with("E_REPORT_unknown_vendor"),
        "{}",
        failure.message
    );
    assert!(!failure.message.contains("Jane"));
    assert!(!failure.message.contains("12345"));
    let debug = format!("{err:?}");
    assert!(
        !debug.contains("Jane") && !debug.contains("12345"),
        "{debug}"
    );
    let lines = failure.first_lines.expect("lines");
    assert!(lines.iter().any(|l| l.contains("Jane")));
    assert!(lines.iter().any(|l| l.contains("12345")));

    let io = ParseFailure::from_error(&reports::ReportImportError::Io("x".to_owned()));
    assert!(io.first_lines.is_none());
}
