// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

use common::{dbsloc, run, s, write};

const DISTAL: &str = "DISTAL Minimal (Ewert 2017)";

fn in_data_dir(data: &Path, args: &[&str]) -> common::Out {
    run(dbsloc().env("DBSLOC_DATA_DIR", s(data)).args(args))
}

fn temp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"))
}

fn available_names(data: &Path) -> Vec<String> {
    let out = in_data_dir(data, &["atlas", "list", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    out.json()
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|r| r["status"] == "available")
        .filter_map(|r| r["name"].as_str().map(str::to_owned))
        .collect()
}

fn failing_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = listener.local_addr().map(|a| a.port()).unwrap_or_default();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    format!("http://127.0.0.1:{port}/leaddbs_data.zip")
}

#[test]
fn list_json_has_the_documented_keys_and_the_bundled_default() {
    let d = temp();
    let out = in_data_dir(d.path(), &["atlas", "list", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    let rows = v.as_array().cloned().unwrap_or_default();
    let distal = rows
        .iter()
        .find(|r| r["name"] == DISTAL)
        .unwrap_or_else(|| panic!("{DISTAL} missing: {}", out.stdout));
    assert_eq!(distal["status"], "bundled");
    for k in ["name", "bytes", "status", "path"] {
        assert!(distal.get(k).is_some(), "{k}");
    }
    let table = in_data_dir(d.path(), &["atlas", "list"]);
    assert_eq!(table.code, 0, "{}", table.stderr);
    assert!(table.stdout.starts_with("name"));
    assert!(table.stdout.contains(DISTAL));
}

#[test]
fn fetch_of_an_unknown_name_exits_2_and_downloads_nothing() {
    let d = temp();
    let out = in_data_dir(d.path(), &["atlas", "fetch", "No Such Atlas (Nobody 1900)"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("dbsloc atlas list"), "{}", out.stderr);
    assert!(!d.path().join("atlases").exists());
}

#[test]
fn fetch_of_a_bundled_name_exits_2() {
    let d = temp();
    let out = in_data_dir(d.path(), &["atlas", "fetch", DISTAL]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("already bundled"), "{}", out.stderr);
    assert!(!d.path().join("atlases").exists());
}

#[test]
fn fetch_with_one_bad_name_downloads_none_of_the_others() {
    let d = temp();
    let names = available_names(d.path());
    let Some(first) = names.first() else {
        eprintln!("skipped: the embedded manifest lists no downloadable atlas");
        return;
    };
    let url = failing_server();
    let out = run(dbsloc()
        .env("DBSLOC_DATA_DIR", s(d.path()))
        .env("DBSLOC_ATLAS_DOWNLOAD_URL", &url)
        .args(["atlas", "fetch", first, "No Such Atlas (Nobody 1900)"]));
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(!d.path().join("atlases").exists());
}

#[test]
fn a_failed_download_exits_1_after_trying_every_name() {
    let d = temp();
    let names = available_names(d.path());
    if names.len() < 2 {
        eprintln!("skipped: the embedded manifest lists fewer than two downloadable atlases");
        return;
    }
    let url = failing_server();
    let out = run(dbsloc()
        .env("DBSLOC_DATA_DIR", s(d.path()))
        .env("DBSLOC_ATLAS_DOWNLOAD_URL", &url)
        .args(["atlas", "fetch", &names[0], &names[1]]));
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("GNU GPL v3"), "{}", out.stderr);
    for n in &names[..2] {
        assert!(
            out.stderr.contains(&format!("error: {n}:")),
            "{}",
            out.stderr
        );
    }
    assert_eq!(
        out.stderr.matches("answered HTTP 500").count(),
        2,
        "{}",
        out.stderr
    );
    assert!(out.stdout.trim().is_empty(), "{}", out.stdout);
    assert!(!d.path().join("atlases").join("leaddbs").exists());
}

#[test]
fn remove_refuses_a_folder_without_the_install_record() {
    let d = temp();
    let dir = d.path().join("atlases").join("leaddbs");
    write(
        &dir.join("Copied (X 2020)").join("lh").join("a.nii.gz"),
        b"x",
    );
    let out = in_data_dir(d.path(), &["atlas", "remove", "Copied (X 2020)"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains(".dbsloc-install.json"),
        "{}",
        out.stderr
    );
    assert!(dir.join("Copied (X 2020)").is_dir());

    write(
        &dir.join("Fetched (Y 2021)").join(".dbsloc-install.json"),
        b"{}",
    );
    let out = in_data_dir(d.path(), &["atlas", "remove", "Fetched (Y 2021)"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("Fetched (Y 2021)"));
    assert!(!dir.join("Fetched (Y 2021)").exists());

    let out = in_data_dir(d.path(), &["atlas", "remove", DISTAL]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("bundled"), "{}", out.stderr);
}

#[test]
fn without_a_data_folder_the_error_names_the_variable() {
    let out = run(dbsloc()
        .env_remove("APPDATA")
        .env_remove("HOME")
        .env_remove("XDG_DATA_HOME")
        .args(["atlas", "remove", "Anything"]));
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("DBSLOC_DATA_DIR"), "{}", out.stderr);
}
