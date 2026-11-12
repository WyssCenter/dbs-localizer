// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dbsloc, run, s, write};

fn list_json(root: &std::path::Path) -> common::Out {
    run(dbsloc()
        .env("DBSLOC_CONNECTOMES_DIR", s(root))
        .args(["connectome", "list", "--json"]))
}

#[test]
fn list_shows_every_catalogue_entry_with_its_access_class() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = list_json(d.path());
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    let rows = v.as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 11);
    assert!(rows
        .iter()
        .all(|r| r["state"] == "available" && r["source"] == "catalogue"));
    let access = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .map(|r| r["access"].as_str().unwrap_or("").to_owned())
    };
    assert_eq!(access("horn2017-mghusc-hcp32").as_deref(), Some("dua"));
    assert_eq!(
        access("cohen2023-gsp1000v2").as_deref(),
        Some("cohort_terms")
    );
    assert_eq!(access("loh2020-torpd75").as_deref(), Some("open"));
    let gsp = rows
        .iter()
        .find(|r| r["id"] == "cohen2023-gsp1000v2")
        .unwrap_or(&rows[0]);
    assert_eq!(gsp["size"], 218_932_842_997u64);
    assert_eq!(gsp["files"], 38);
}

#[test]
fn licence_prints_the_verbatim_terms_and_the_acknowledgement() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_CONNECTOMES_DIR", s(d.path())).args([
        "connectome",
        "licence",
        "ewert2017-ppmi85",
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("PPMI Data Use Agreement"));
    assert!(out.stdout.contains("Terms fetched: 2026-10-23"));
    assert!(out.stdout.contains("RRID:SCR_006431"));
    assert!(out.stdout.contains("Required acknowledgement:"));
    assert!(out.stdout.len() > 20_000, "{}", out.stdout.len());
    let json = run(dbsloc().args(["connectome", "license", "loh2020-torpd75", "--json"]));
    assert_eq!(json.code, 0, "{}", json.stderr);
    let v = json.json();
    assert_eq!(v["terms_fetched"], "2026-10-23");
    assert!(v["licence_text"]
        .as_str()
        .unwrap_or("")
        .contains("Creative Commons"));
    assert_eq!(v["licence_text_sha256"].as_str().map(str::len), Some(64));
}

#[test]
fn licence_of_an_unknown_id_fails() {
    let out = run(dbsloc().args(["connectome", "licence", "nope"]));
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("not a catalogue id"), "{}", out.stderr);
}

#[test]
fn list_table_has_a_header_and_a_row_per_entry() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc()
        .env("DBSLOC_CONNECTOMES_DIR", s(d.path()))
        .args(["connectome", "list"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 12);
    assert!(lines[0].starts_with("id"));
    assert!(out.stdout.contains("horn2013-gibbs19"));
}

#[test]
fn roots_precedence_flag_over_env_and_external_root_is_read_in_place() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let env_root = d.path().join("env_root");
    let flag_root = d.path().join("flag_root");
    let ext = d.path().join("lead_dbs");
    write(&env_root.join("dMRI/FromEnv/data.mat"), b"x");
    write(&flag_root.join("dMRI/FromFlag/data.mat"), b"x");
    write(&ext.join("dMRI/FromExternal/data.mat"), b"x");
    write(
        &ext.join("fMRI/GSP 1000 v2 (Cohen 2023)/dataset_info.json"),
        b"{\"tag\":\"GSP1000v2\"}",
    );
    let names = |with_flag: bool| {
        let mut cmd = dbsloc();
        cmd.env("DBSLOC_CONNECTOMES_DIR", s(&env_root))
            .env("DBSLOC_LEADDBS_CONNECTOMES_DIR", s(&ext))
            .args(["connectome", "list", "--json"]);
        if with_flag {
            cmd.args(["--root", &s(&flag_root)]);
        }
        let out = run(&mut cmd);
        assert_eq!(out.code, 0, "{}", out.stderr);
        out.json()
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| {
                (
                    r["name"].as_str().unwrap_or("").to_owned(),
                    r["root_kind"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    let env_rows = names(false);
    assert!(env_rows.contains(&("FromEnv".to_owned(), "primary".to_owned())));
    assert!(env_rows.contains(&("FromExternal".to_owned(), "external".to_owned())));
    assert!(env_rows.contains(&("GSP 1000 v2 (Cohen 2023)".to_owned(), "external".to_owned())));
    assert!(!env_rows.iter().any(|(n, _)| n == "FromFlag"));
    let flag_rows = names(true);
    assert!(flag_rows.contains(&("FromFlag".to_owned(), "primary".to_owned())));
    assert!(!flag_rows.iter().any(|(n, _)| n == "FromEnv"));
    assert!(ext.join("dMRI/FromExternal/data.mat").is_file());
    assert!(!ext.join("licenses").exists() && !ext.join("registry.json").exists());
}

#[test]
fn fetch_without_accept_terms_prints_the_licence_and_exits_2() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_CONNECTOMES_DIR", s(d.path())).args([
        "connectome",
        "fetch",
        "loh2020-torpd75",
    ]));
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("CC BY 4.0"), "{}", out.stderr);
    assert!(out.stderr.contains("pass --accept-terms"), "{}", out.stderr);
    assert!(!d.path().join("licenses").exists());
    assert!(!d.path().join("fMRI").exists());
}

#[test]
fn fetch_of_a_dua_entry_is_refused_with_steps_even_with_both_flags() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    for extra in [
        &["--accept-terms"][..],
        &["--accept-terms", "--attest-data-use-agreement"][..],
        &[][..],
    ] {
        let out = run(dbsloc()
            .env("DBSLOC_CONNECTOMES_DIR", s(d.path()))
            .args(["connectome", "fetch", "ewert2017-ppmi85"])
            .args(extra));
        assert_eq!(out.code, 2, "{}", out.stderr);
        assert!(out.stderr.contains("Citation:"), "{}", out.stderr);
        assert!(
            out.stderr.contains("1. Read the PPMI Data Use Agreement."),
            "{}",
            out.stderr
        );
        assert!(
            out.stderr.contains(
                "https://ida.loni.usc.edu/collaboration/access/appApply.jsp?project=PPMI"
            ),
            "{}",
            out.stderr
        );
        assert!(
            out.stderr
                .contains("groupconnectome_ppmi_85_ewert_2017.zip"),
            "{}",
            out.stderr
        );
        assert!(
            out.stderr.contains(
                "dbsloc connectome import <archive> --accept-terms --attest-data-use-agreement"
            ),
            "{}",
            out.stderr
        );
        assert!(
            out.stderr.contains(
                "E_CONN_TERMS: ewert2017-ppmi85 is import-only: obtain it with your own data access and import it"
            ),
            "{}",
            out.stderr
        );
        assert!(!d.path().join("licenses").exists());
        assert!(!d.path().join("dMRI").exists());
    }
}

#[test]
fn info_and_licence_show_the_steps_of_a_dua_entry_only() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let go = |args: &[&str]| {
        run(dbsloc()
            .env("DBSLOC_CONNECTOMES_DIR", s(d.path()))
            .args(args))
    };
    let info = go(&["connectome", "info", "horn2017-mghusc-hcp32", "--json"]);
    assert_eq!(info.code, 0, "{}", info.stderr);
    let steps = info.json()["entry"]["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(steps.len(), 5);
    assert!(steps[0]["url"]
        .as_str()
        .unwrap_or("")
        .starts_with("https://"));
    let text = go(&["connectome", "licence", "horn2017-mghusc-hcp32"]);
    assert!(
        text.stdout
            .contains("How to get this dataset (import only):"),
        "{}",
        text.stdout
    );
    assert!(text.stdout.contains("1. Read the HCP Data Use Agreement"));
    let plain = go(&["connectome", "info", "horn2017-mghusc-hcp32"]);
    assert!(plain.stdout.contains("step 1"), "{}", plain.stdout);
    let open = go(&["connectome", "info", "loh2020-torpd75", "--json"]);
    assert_eq!(
        open.json()["entry"]["steps"].as_array().map(Vec::len),
        Some(0)
    );
    let open_text = go(&["connectome", "licence", "loh2020-torpd75"]);
    assert!(!open_text.stdout.contains("How to get this dataset"));
}

fn tar_with_one_file(name: &str, data: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut out = header.to_vec();
    out.extend_from_slice(data);
    out.resize(512 + data.len().div_ceil(512) * 512, 0);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

#[test]
fn import_of_an_archive_reports_whether_it_was_verified() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = d.path().join("root");
    let zip = d.path().join("mine.tar");
    write(&zip, &tar_with_one_file("Mine/data.mat", b"x"));
    let go = |args: &[&str]| run(dbsloc().env("DBSLOC_CONNECTOMES_DIR", s(&root)).args(args));
    let out = go(&["connectome", "import", &s(&zip)]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("not a known catalogue file; not verified"),
        "{}",
        out.stdout
    );
    let json = go(&["connectome", "import", &s(&zip), "--json"]);
    assert_eq!(json.code, 0, "{}", json.stderr);
    let v = json.json();
    assert_eq!(v["verified"], false);
    assert_eq!(
        v["verification_note"],
        "not a known catalogue file; not verified"
    );
    let named = d.path().join("Pediatric_rsfMRI.zip");
    std::fs::copy(&zip, &named).unwrap_or_else(|e| panic!("{e}"));
    let bad = go(&["connectome", "import", &s(&named)]);
    assert_eq!(bad.code, 1, "{}", bad.stderr);
    assert!(bad.stderr.contains("E_CONN_CHECKSUM"), "{}", bad.stderr);
    assert!(
        bad.stderr
            .contains("is named like the catalogue file Pediatric_rsfMRI.zip"),
        "{}",
        bad.stderr
    );
    assert!(
        bad.stderr.contains("bytes instead of")
            && bad.stderr.contains("rename it and import it again"),
        "{}",
        bad.stderr
    );
    for variant in ["pediatric_rsfmri.ZIP", "Pediatric_rsfMRI (1).zip"] {
        let copy = d.path().join(variant);
        std::fs::copy(&zip, &copy).unwrap_or_else(|e| panic!("{e}"));
        let out = go(&["connectome", "import", &s(&copy)]);
        assert_eq!(out.code, 1, "{variant}: {}", out.stderr);
        assert!(
            out.stderr.contains("E_CONN_CHECKSUM"),
            "{variant}: {}",
            out.stderr
        );
    }
}

#[test]
fn fetch_unknown_id_exits_2_and_lists_valid_ids() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_CONNECTOMES_DIR", s(d.path())).args([
        "connectome",
        "fetch",
        "nope",
        "--accept-terms",
    ]));
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("cohen2023-gsp1000v2"), "{}", out.stderr);
}

#[test]
fn import_registers_a_folder_in_place_and_list_info_forget_follow() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = d.path().join("root");
    let folder = d.path().join("elsewhere").join("My Fibres");
    write(&folder.join("data.mat"), b"placeholder");
    let go = |args: &[&str]| run(dbsloc().env("DBSLOC_CONNECTOMES_DIR", s(&root)).args(args));
    let out = go(&["connectome", "import", &s(&folder), "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["class"], "dMRI");
    assert_eq!(v["registered"], true);
    assert!(folder.join("data.mat").is_file());
    assert!(!root.join("dMRI").exists());
    let list = go(&["connectome", "list", "--json"]);
    let rows = list.json().as_array().cloned().unwrap_or_default();
    let mine = rows.iter().find(|r| r["name"] == "My Fibres");
    assert_eq!(
        mine.map(|r| r["root_kind"].clone()),
        Some("registered".into())
    );
    assert_eq!(rows.len(), 12);
    let info = go(&["connectome", "info", "My Fibres"]);
    assert_eq!(info.code, 0, "{}", info.stderr);
    assert!(info.stdout.contains("registered"), "{}", info.stdout);
    let gone = go(&["connectome", "forget", &s(&folder)]);
    assert_eq!(gone.code, 0, "{}", gone.stderr);
    assert!(folder.join("data.mat").is_file());
    let again = go(&["connectome", "forget", &s(&folder)]);
    assert_eq!(again.code, 1);
    assert!(again.stderr.contains("E_CONN_MISSING"), "{}", again.stderr);
}

#[test]
fn import_of_a_non_connectome_folder_fails() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let folder = d.path().join("plain");
    write(&folder.join("readme.txt"), b"x");
    let out = run(dbsloc()
        .env("DBSLOC_CONNECTOMES_DIR", s(&d.path().join("root")))
        .args(["connectome", "import", &s(&folder)]));
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("E_CONN_FORMAT"), "{}", out.stderr);
}

#[test]
fn import_of_a_catalogue_folder_notes_the_missing_licence_record() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = d.path().join("root");
    let folder = d.path().join("MGH-USC HCP 32 (Horn 2017)");
    write(&folder.join("data.mat"), b"x");
    let out = run(dbsloc().env("DBSLOC_CONNECTOMES_DIR", s(&root)).args([
        "connectome",
        "import",
        &s(&folder),
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains("licence_record: null"),
        "{}",
        out.stderr
    );
    let out = run(dbsloc().env("DBSLOC_CONNECTOMES_DIR", s(&root)).args([
        "connectome",
        "import",
        &s(&folder),
        "--accept-terms",
        "--attest-data-use-agreement",
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    let rec = root.join("licenses").join("horn2017-mghusc-hcp32.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rec).unwrap_or_default())
            .unwrap_or_default();
    assert_eq!(v["accepted_via"], "import");
    assert_eq!(v["access"], "dua");
}

#[test]
fn info_of_a_catalogue_id_shows_licence_and_size() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_CONNECTOMES_DIR", s(d.path())).args([
        "connectome",
        "info",
        "loh2020-torpd75",
        "--json",
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["entry"]["access"], "open");
    assert_eq!(v["entry"]["size"], 25_665_221_855u64);
    assert_eq!(v["state"], "available");
}

#[test]
fn cache_build_list_and_clear_round_trip() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = d.path().join("connectomes");
    let folder = root.join("dMRI").join("Synth");
    std::fs::create_dir_all(&folder).unwrap_or_else(|e| panic!("{e}"));
    let fibres: Vec<Vec<[f32; 3]>> = (0..20)
        .map(|f| {
            (0..6)
                .map(|i| [f as f32 + i as f32, f as f32 * 0.5, i as f32])
                .collect()
        })
        .collect();
    let set = dbsloc_connectome::FibreSet::from_fibres(&fibres).unwrap_or_else(|e| panic!("{e}"));
    dbsloc_connectome::write_data_mat(&folder.join("data.mat"), &set)
        .unwrap_or_else(|e| panic!("{e}"));
    let cache = d.path().join("cache");
    let cmd = |args: &[&str]| {
        run(dbsloc()
            .env("DBSLOC_CONNECTOMES_DIR", s(&root))
            .env("DBSLOC_CACHE_DIR", s(&cache))
            .args(["connectome", "cache"])
            .args(args))
    };
    let out = cmd(&["list", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.json()["caches"].as_array().is_some_and(Vec::is_empty));

    let out = cmd(&["build", "Synth"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("built"), "{}", out.stdout);

    let out = cmd(&["list", "--json"]);
    let v = out.json();
    assert_eq!(v["caches"].as_array().map(Vec::len), Some(1));
    assert_eq!(v["caches"][0]["name"], "Synth");
    assert_eq!(v["caches"][0]["orphaned"], false);

    let out = cmd(&["clear", "Synth"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("deleted"), "{}", out.stdout);
    let out = cmd(&["list", "--json"]);
    assert!(out.json()["caches"].as_array().is_some_and(Vec::is_empty));

    cmd(&["build", "Synth"]);
    let out = cmd(&["clear", "--all"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = cmd(&["clear"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}
