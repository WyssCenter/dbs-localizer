// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::{Path, PathBuf};

use common::{dataset, run_args, s, write};

const PDF: &str = "../../tests/fixtures/reports/synthetic/ring_percept_pc.pdf";

fn fixture() -> String {
    s(&Path::new(env!("CARGO_MANIFEST_DIR")).join(PDF))
}

fn record_files(root: &Path) -> Vec<PathBuf> {
    let base = root
        .join("derivatives")
        .join("leaddbs")
        .join("sub-1")
        .join("clinical")
        .join("reports")
        .join("medtronic");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            out.push(e.path().join("sub-1_desc-sessionreport.json"));
        }
    }
    out
}

#[test]
fn inspect_prints_the_table_and_the_record() {
    let out = run_args(&["report", "inspect", &fixture()]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("vendor: medtronic"), "{}", out.stdout);
    assert!(out.stdout.contains("final snapshot"));
    let out = run_args(&["report", "inspect", &fixture(), "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["schema"], "dbsloc-session-report");
    assert_eq!(v["patient"]["name"], "Sample Patient");
    assert_eq!(v["device"]["serial"], "SYN_PC_0001");
    assert_eq!(v["session"]["date"], "2026-10-14T14:25");
    assert_eq!(v["snapshots"]["final"]["active_group"], "D");
    let out = run_args(&[
        "report",
        "inspect",
        &fixture(),
        "--json",
        "--anonymize-preview",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["anonymized"], true);
    assert!(v["patient"]["name"].is_null());
}

#[test]
fn import_list_and_stim_list_round_trip() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    dataset(dir.path(), "1");
    let root = s(dir.path());
    let out = run_args(&[
        "report",
        "import",
        &root,
        "1",
        &fixture(),
        "--no-index",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    let first = &v["results"][0];
    assert_eq!(first["ok"], true);
    let written: Vec<String> = first["labels_written"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert!(!written.is_empty(), "{first}");
    let files = record_files(dir.path());
    assert_eq!(files.len(), 1);
    assert!(files[0].is_file());
    assert!(dir
        .path()
        .join("sourcedata")
        .join("sub-1")
        .join("reports")
        .join("medtronic")
        .join("ring_percept_pc.pdf")
        .is_file());

    let out = run_args(&["report", "list", &root, "1", "--no-index", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["reports"].as_array().map(Vec::len), Some(1));
    assert_eq!(v["reports"][0]["vendor"], "medtronic");
    let out = run_args(&["report", "list", &root, "1", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("medtronic/"));

    let out = run_args(&["stim", "list", &root, "1", "--no-index", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let listed = out.stdout;
    for label in &written {
        assert!(listed.contains(label.as_str()), "{label} in {listed}");
    }

    let again = run_args(&["report", "import", &root, "1", &fixture(), "--no-index"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(again.stdout.contains("labels unchanged"));
}

#[test]
fn anonymised_import_uses_the_key_file_and_keeps_no_copy() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    dataset(dir.path(), "1");
    let root = s(dir.path());
    let key = dir.path().join("anon.key");
    let out = run_args(&[
        "report",
        "import",
        &root,
        "1",
        &fixture(),
        "--no-index",
        "--anonymize",
        "--key-file",
        &s(&key),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(std::fs::read(&key).map(|b| b.len()).unwrap_or(0), 32);
    assert!(!dir
        .path()
        .join("sourcedata")
        .join("sub-1")
        .join("reports")
        .exists());
    let files = record_files(dir.path());
    assert_eq!(files.len(), 1);
    assert!(!files[0].with_extension("txt").exists());
    let text = std::fs::read_to_string(&files[0]).unwrap_or_default();
    assert!(text.contains("\"anonymized\": true"));
    assert!(!text.contains("Sample Patient"));

    let shared = dir.path().join("shared.json");
    let out = run_args(&[
        "report",
        "anonymize",
        &fixture(),
        "--subject",
        "sub-1",
        "--out",
        &s(&shared),
        "--key-file",
        &s(&key),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(shared.is_file());
}

#[test]
fn unknown_files_fail_with_a_report_code() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    dataset(dir.path(), "1");
    let root = s(dir.path());
    let bad = dir.path().join("notes.pdf");
    write(&bad, b"this is not a pdf");
    let out = run_args(&["report", "import", &root, "1", &s(&bad), "--no-index"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("E_REPORT_"), "{}", out.stderr);
    let out = run_args(&["report", "inspect", &s(&bad)]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("dbsloc: error: report: E_REPORT_"));
    let out = run_args(&[
        "report",
        "import",
        &root,
        "1",
        &s(&bad),
        &fixture(),
        "--no-index",
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stdout.contains("labels written"), "{}", out.stdout);
}

fn foreign_pdf() -> Vec<u8> {
    let stream = "BT /F1 12 Tf 72 720 Td (Jane Roe) Tj 0 -20 Td (MRN 12345) Tj 0 -20 Td (Some other vendor session summary) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn unknown_vendor_text_stays_out_of_import_and_anonymize_output() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    dataset(dir.path(), "1");
    let root = s(dir.path());
    let foreign = dir.path().join("foreign.pdf");
    write(&foreign, &foreign_pdf());
    let out = run_args(&[
        "report",
        "import",
        &root,
        "1",
        &s(&foreign),
        "--no-index",
        "--anonymize",
        "--json",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("E_REPORT_unknown_vendor"),
        "{}",
        out.stderr
    );
    for text in [&out.stdout, &out.stderr] {
        assert!(!text.contains("Jane"), "{text}");
        assert!(!text.contains("12345"), "{text}");
    }
    assert!(
        out.json()["results"][0]["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("E_REPORT_unknown_vendor")),
        "{}",
        out.stdout
    );

    let shared = dir.path().join("shared.json");
    let key = dir.path().join("anon.key");
    let out = run_args(&[
        "report",
        "anonymize",
        &s(&foreign),
        "--subject",
        "sub-1",
        "--out",
        &s(&shared),
        "--key-file",
        &s(&key),
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("E_REPORT_unknown_vendor"),
        "{}",
        out.stderr
    );
    for text in [&out.stdout, &out.stderr] {
        assert!(!text.contains("Jane"), "{text}");
        assert!(!text.contains("12345"), "{text}");
    }

    let out = run_args(&["report", "inspect", &s(&foreign)]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("dbsloc: note: report: first lines of page 1"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("Jane"), "{}", out.stderr);
    assert!(!out.stdout.contains("Jane"), "{}", out.stdout);

    let out = run_args(&["report", "inspect", &s(&foreign), "--anonymize-preview"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("E_REPORT_unknown_vendor"),
        "{}",
        out.stderr
    );
    for text in [&out.stdout, &out.stderr] {
        assert!(!text.contains("Jane"), "{text}");
        assert!(!text.contains("12345"), "{text}");
        assert!(!text.contains("first lines"), "{text}");
    }
}

#[test]
fn a_missing_subject_is_exit_two() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = s(dir.path());
    let out = run_args(&["report", "import", &root, "99", &fixture(), "--no-index"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    let out = run_args(&["report", "list", &root, "99", "--no-index"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}
