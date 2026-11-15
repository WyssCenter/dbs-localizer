// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::PathBuf;

use common::{dataset, run_args, s, write};

const A1: &str =
    "[2026-01-01T00:00:00Z] preprocess | Wyss DBS Localizer 1.0\nFirst preprocess.\n\n";
const B1: &str = "[2026-01-02T00:00:00Z] coreg | Wyss DBS Localizer 1.0\nCoregistration.\nReferences:\n- Ref.\n\n";
const A2: &str =
    "[2026-01-03T00:00:00Z] preprocess | Wyss DBS Localizer 1.0\nSecond preprocess.\n\n";

fn methods_file(root: &std::path::Path, sid: &str) -> PathBuf {
    let shown = run_args(&["subject", "show", &s(root), sid]);
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    let json = shown.json();
    let dir = PathBuf::from(
        json["subj_dir"]
            .as_str()
            .unwrap_or_else(|| panic!("no subj_dir in {json}")),
    );
    dir.join("log").join(format!("sub-{sid}_desc-methods.txt"))
}

#[test]
fn methods_without_a_text_yet_exits_1_with_a_message() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    for extra in [&[][..], &["--last"][..], &["--path"][..]] {
        let mut args = vec!["methods", "--no-index"];
        let root_text = s(&root);
        args.push(&root_text);
        args.push("22");
        args.extend_from_slice(extra);
        let out = run_args(&args);
        assert_eq!(out.code, 1, "{}", out.stderr);
        assert!(out.stdout.is_empty(), "{}", out.stdout);
        assert!(out.stderr.contains("no methods text yet"), "{}", out.stderr);
    }
}

#[test]
fn methods_for_an_unknown_subject_exits_2() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run_args(&["methods", &s(dir.path()), "7"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("rawdata"), "{}", out.stderr);
}

#[test]
fn methods_prints_the_file_as_it_is() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    let file = methods_file(&root, "22");
    let content = format!("{A1}{B1}{A2}");
    write(&file, content.as_bytes());
    let out = run_args(&["methods", &s(&root), "sub-22"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, content);
    let path = run_args(&["methods", &s(&root), "22", "--path"]);
    assert_eq!(path.code, 0, "{}", path.stderr);
    assert_eq!(PathBuf::from(path.stdout.trim()), file);
}

#[test]
fn methods_last_shows_each_step_once_and_drops_text_before_the_first_entry() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    let file = methods_file(&root, "22");
    let content = format!("My own note.\n\n{A1}{B1}{A2}");
    write(&file, content.as_bytes());
    let plain = run_args(&["methods", &s(&root), "22"]);
    assert_eq!(plain.stdout, content);
    let last = run_args(&["methods", &s(&root), "22", "--last"]);
    assert_eq!(last.code, 0, "{}", last.stderr);
    assert_eq!(last.stdout, format!("{B1}{A2}"));
    assert!(!last.stdout.contains("My own note"));
    assert!(!last.stdout.contains("First preprocess"));
}

#[test]
fn methods_last_on_a_file_without_entries_exits_1() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    let file = methods_file(&root, "22");
    write(&file, b"only a note\n");
    let out = run_args(&["methods", &s(&root), "22", "--last"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("no methods entry"), "{}", out.stderr);
}

#[test]
fn methods_reads_a_file_with_invalid_utf8_and_a_byte_order_mark() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    let file = methods_file(&root, "22");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"note \xFF\xFE\n\n");
    bytes.extend_from_slice(format!("{A1}{B1}").as_bytes());
    write(&file, &bytes);
    let last = run_args(&["methods", &s(&root), "22", "--last"]);
    assert_eq!(last.code, 0, "{}", last.stderr);
    assert_eq!(last.stdout, format!("{A1}{B1}"));
    let plain = run_args(&["methods", &s(&root), "22"]);
    assert_eq!(plain.code, 0, "{}", plain.stderr);
    assert!(plain.stdout.starts_with('\u{feff}'), "{}", plain.stdout);
    assert!(plain.stdout.contains("First preprocess."));
}

#[test]
fn methods_last_adds_no_blank_lines_to_a_file_with_windows_line_ends() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    let file = methods_file(&root, "22");
    let content = format!("{A1}{B1}").replace('\n', "\r\n");
    write(&file, content.as_bytes());
    let last = run_args(&["methods", &s(&root), "22", "--last"]);
    assert_eq!(last.code, 0, "{}", last.stderr);
    assert_eq!(last.stdout, content);
    let plain = run_args(&["methods", &s(&root), "22"]);
    assert_eq!(plain.stdout, content);
}

#[test]
fn methods_refuses_last_together_with_path() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    let out = run_args(&["methods", &s(&root), "22", "--last", "--path"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("cannot be used with"), "{}", out.stderr);
}

#[test]
fn methods_help_uses_full_words() {
    let out = run_args(&["methods", "--help"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("--last"), "{}", out.stdout);
    assert!(out.stdout.contains("--path"), "{}", out.stdout);
    assert!(out
        .stdout
        .contains("most recent entry of each processing step"));
}
