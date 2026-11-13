// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dataset, dbsloc, run, s};

fn run_plan(root: &std::path::Path, env: &[(&str, &str)], extra: &[&str]) -> common::Out {
    let rs = s(root);
    let mut c = dbsloc();
    for (k, v) in env {
        c.env(k, v);
    }
    c.args([
        "run",
        &rs,
        "1",
        "--plan",
        "--log",
        "quiet",
        "--no-index",
        "--stages",
        "normalize",
    ]);
    c.args(extra);
    run(&mut c)
}

fn root() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "1");
    (dir, root)
}

#[test]
fn norm_isotropic_values_are_accepted() {
    let (_d, root) = root();
    for v in ["1.5", "2", " 0.8 ", "full", "FULL", "template"] {
        let out = run_plan(&root, &[], &["--norm-isotropic-mm", v]);
        assert_eq!(out.code, 0, "{v}: {}", out.stderr);
    }
}

#[test]
fn norm_isotropic_invalid_flag_exits_2() {
    let (_d, root) = root();
    for v in ["0", "-1", "0.01", "3.5", "abc", "nan", "inf", ""] {
        let out = run_plan(&root, &[], &["--norm-isotropic-mm", v]);
        assert_eq!(out.code, 2, "{v:?}");
        assert!(out.stderr.contains("--norm-isotropic-mm"), "{}", out.stderr);
        assert!(
            out.stderr
                .contains("millimetres from 0.3 to 3.0, 'full' or 'template'"),
            "{}",
            out.stderr
        );
    }
}

#[test]
fn norm_isotropic_environment() {
    let (_d, root) = root();
    for v in ["1.5", "Full", "  "] {
        let out = run_plan(&root, &[("DBSLOC_NORM_ISOTROPIC_MM", v)], &[]);
        assert_eq!(out.code, 0, "{v}: {}", out.stderr);
    }
    for v in ["0", "auto", "-0.5", "0.01"] {
        let out = run_plan(&root, &[("DBSLOC_NORM_ISOTROPIC_MM", v)], &[]);
        assert_eq!(out.code, 2, "{v}");
        assert!(out.stderr.contains("DBSLOC_NORM_ISOTROPIC_MM") && out.stderr.contains("'full'"));
    }
    let out = run_plan(
        &root,
        &[("DBSLOC_NORM_ISOTROPIC_MM", "bogus")],
        &["--norm-isotropic-mm", "2"],
    );
    assert_eq!(
        out.code, 0,
        "flag wins over the environment: {}",
        out.stderr
    );
}

#[test]
fn norm_method() {
    let (_d, root) = root();
    assert_eq!(run_plan(&root, &[], &["--norm-method", "ants_syn"]).code, 0);
    let out = run_plan(&root, &[], &["--norm-method", "synthmorph"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--norm-method"), "{}", out.stderr);
    assert_eq!(
        run_plan(&root, &[("DBSLOC_NORM_METHOD", "ants_syn")], &[]).code,
        0
    );
    assert_eq!(
        run_plan(&root, &[("DBSLOC_NORM_METHOD", "  ")], &[]).code,
        0
    );
    assert_eq!(
        run_plan(
            &root,
            &[("DBSLOC_NORM_METHOD", "spm")],
            &["--norm-method", "ants_syn"]
        )
        .code,
        0
    );
    let out = run_plan(&root, &[("DBSLOC_NORM_METHOD", "spm")], &[]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("DBSLOC_NORM_METHOD"), "{}", out.stderr);
}

#[test]
fn reg_backend() {
    let (_d, root) = root();
    for v in ["auto", "wgpu", "ants"] {
        assert_eq!(run_plan(&root, &[], &["--reg-backend", v]).code, 0, "{v}");
    }
    let out = run_plan(&root, &[], &["--reg-backend", "cuda"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--reg-backend"), "{}", out.stderr);
    for v in ["wgpu", " ANTs ", "auto", "  "] {
        let out = run_plan(&root, &[("DBSLOC_REG_BACKEND", v)], &[]);
        assert_eq!(out.code, 0, "{v}: {}", out.stderr);
    }
    assert_eq!(
        run_plan(
            &root,
            &[("DBSLOC_REG_BACKEND", "wgpu")],
            &["--reg-backend", "ants"]
        )
        .code,
        0
    );
    let out = run_plan(&root, &[("DBSLOC_REG_BACKEND", "cuda")], &[]);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("DBSLOC_REG_BACKEND") && out.stderr.contains("wgpu"),
        "{}",
        out.stderr
    );
}

#[test]
fn per_step_backends() {
    let (_d, root) = root();
    for (flag, var) in [
        ("--preprocess-backend", "DBSLOC_PREPROCESS_BACKEND"),
        ("--coregister-backend", "DBSLOC_COREGISTER_BACKEND"),
        ("--normalize-backend", "DBSLOC_NORMALIZE_BACKEND"),
    ] {
        for v in ["auto", "wgpu", "ants"] {
            assert_eq!(run_plan(&root, &[], &[flag, v]).code, 0, "{flag} {v}");
        }
        let out = run_plan(&root, &[], &[flag, "cuda"]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains(flag), "{}", out.stderr);
        assert_eq!(run_plan(&root, &[(var, " WGPU ")], &[]).code, 0);
        assert_eq!(run_plan(&root, &[(var, "wgpu")], &[flag, "ants"]).code, 0);
        let out = run_plan(&root, &[(var, "cuda")], &[]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains(var), "{}", out.stderr);
    }
    assert_eq!(
        run_plan(
            &root,
            &[],
            &["--reg-backend", "ants", "--normalize-backend", "wgpu"]
        )
        .code,
        0
    );
}

#[test]
fn recon_method_and_log_choices_are_validated() {
    let (_d, root) = root();
    let pacer = run_plan(&root, &[], &["--recon-method", "pacer"]);
    if cfg!(feature = "pacer") {
        assert_eq!(pacer.code, 0, "{}", pacer.stderr);
    } else {
        assert_eq!(pacer.code, 2, "{}", pacer.stderr);
        assert!(
            pacer.stderr.contains("E_RECON_pacer_unavailable"),
            "{}",
            pacer.stderr
        );
    }
    assert_eq!(run_plan(&root, &[], &["--recon-method", "magic"]).code, 2);
    assert_eq!(run_plan(&root, &[], &["--log", "loud"]).code, 2);
}

#[test]
fn ants_thread_cap_environment_must_be_an_integer() {
    let (_d, root) = root();
    assert_eq!(
        run_plan(&root, &[("DBSLOC_ANTS_MAX_THREADS", "2")], &[]).code,
        0
    );
    assert_eq!(
        run_plan(&root, &[("DBSLOC_ANTS_MAX_THREADS", "two")], &[]).code,
        2
    );
}

fn stub_run(root: &std::path::Path, env: &[(&str, &str)], extra: &[&str]) -> serde_json::Value {
    let rs = s(root);
    let mut c = dbsloc();
    for (k, v) in env {
        c.env(k, v);
    }
    c.args([
        "run",
        &rs,
        "1",
        "--log",
        "quiet",
        "--headless",
        "--no-index",
        "--stages",
        "orientation,vat",
    ]);
    c.args(extra);
    let out = run(&mut c);
    assert_eq!(out.code, 1, "{}", out.stderr);
    out.json()
}

fn vat_ran(v: &serde_json::Value) -> bool {
    common::result_of(v, "vat") == "skip:vat: no stimulation settings"
}

#[test]
fn stop_on_error_follows_the_environment_then_checkreg() {
    let (_d, root) = root();
    assert!(!vat_ran(&stub_run(&root, &[], &[])));
    assert!(vat_ran(&stub_run(&root, &[], &["--no-checkreg"])));
    assert!(!vat_ran(&stub_run(
        &root,
        &[("DBSLOC_STOP_ON_ERROR", "1")],
        &["--no-checkreg"]
    )));
    assert!(vat_ran(&stub_run(
        &root,
        &[("DBSLOC_STOP_ON_ERROR", "0")],
        &[]
    )));
}

#[test]
fn a_checkreg_flag_wins_over_the_checkreg_environment() {
    let (_d, root) = root();
    let v = stub_run(&root, &[("DBSLOC_OPEN_CHECKREG", "1")], &["--no-checkreg"]);
    assert!(vat_ran(&v), "--no-checkreg must not be overridden: {v}");
    let v = stub_run(&root, &[("DBSLOC_OPEN_CHECKREG", "0")], &[]);
    assert!(vat_ran(&v), "the variable fills an unset checkreg: {v}");
}

#[test]
fn an_explicit_thread_flag_skips_an_invalid_environment_value() {
    let (_d, root) = root();
    assert_eq!(
        run_plan(
            &root,
            &[("DBSLOC_ANTS_MAX_THREADS", "two")],
            &["--ants-max-threads", "2"]
        )
        .code,
        0
    );
}

fn bids_plan(root: &std::path::Path, env: &[(&str, &str)], extra: &[&str]) -> serde_json::Value {
    let rs = s(root);
    let mut c = dbsloc();
    for (k, v) in env {
        c.env(k, v);
    }
    c.args([
        "run",
        &rs,
        "1",
        "--plan",
        "--log",
        "quiet",
        "--no-index",
        "--fingerprint-locks",
        "--stages",
        "bids",
    ]);
    c.args(extra);
    let out = run(&mut c);
    assert_eq!(out.code, 0, "{}", out.stderr);
    out.json()
}

#[test]
fn output_dir_flag_and_variable_choose_where_locks_are_read() {
    let (d, root) = root();
    let rs = s(&root);
    let flag_dir = d.path().join("flag_out");
    let env_dir = d.path().join("env_out");
    let flag_abs = dbsloc_pipeline::paths::absolute(&flag_dir);
    let env_abs = dbsloc_pipeline::paths::absolute(&env_dir);
    let t1 = s(&common::t1_of(&root, "1"));
    let approve = run(dbsloc().args([
        "lock",
        "approve",
        &rs,
        "1",
        "bids",
        "--input",
        &t1,
        "--no-index",
        "--output-dir",
        &s(&flag_dir),
    ]));
    assert_eq!(approve.code, 0, "{}", approve.stderr);

    let v = bids_plan(&root, &[], &["--output-dir", &s(&flag_dir)]);
    assert_eq!(v["output_dir"], s(&flag_abs));
    assert_eq!(v["locks_dir"], s(&flag_abs.join("sub-1").join("locks")));
    assert_eq!(common::result_of(&v, "bids"), "skip:approved");

    let v = bids_plan(&root, &[], &[]);
    let default = dbsloc_pipeline::paths::absolute(&root)
        .join("derivatives")
        .join("leaddbs");
    assert_eq!(v["output_dir"], s(&default));
    assert_eq!(common::result_of(&v, "bids"), "plan:bids");

    let env_s = s(&env_dir);
    let flag_s = s(&flag_dir);
    let v = bids_plan(&root, &[("DBSLOC_OUTPUT_DIR", &flag_s)], &[]);
    assert_eq!(v["output_dir"], s(&flag_abs));
    assert_eq!(common::result_of(&v, "bids"), "skip:approved");

    let v = bids_plan(
        &root,
        &[("DBSLOC_OUTPUT_DIR", &flag_s)],
        &["--output-dir", &env_s],
    );
    assert_eq!(v["output_dir"], s(&env_abs), "the flag beats the variable");
    assert_eq!(common::result_of(&v, "bids"), "plan:bids");
    assert!(!root.join("derivatives").exists());
}

#[test]
fn an_executed_run_reads_derivatives_from_the_output_dir() {
    let (d, root) = root();
    let out_dir = d.path().join("results");
    let out = run(dbsloc().args([
        "run",
        &s(&root),
        "1",
        "--log",
        "quiet",
        "--no-index",
        "--no-checkreg",
        "--headless",
        "--stages",
        "bids,orientation",
        "--output-dir",
        &s(&out_dir),
    ]));
    let v = out.json();
    let orientation = common::result_of(&v, "orientation");
    let expected = dbsloc_pipeline::paths::absolute(&out_dir).join("sub-1");
    assert!(
        orientation.starts_with("error: orientation: missing Reco JSON: ")
            && orientation.contains(&s(&expected)),
        "{orientation}"
    );
    assert!(!root.join("derivatives").exists());
}

#[test]
fn the_postop_modality_flag_is_validated() {
    let (_d, root) = root();
    let out = run_plan(&root, &[], &["--postop-modality", "ct"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = run_plan(&root, &[], &["--postop-modality", "pet"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}

#[test]
fn the_atlases_directory_flag_is_accepted() {
    let (d, root) = root();
    let dir = s(&d.path().join("atlases"));
    let out = run_plan(&root, &[], &["--leaddbs-atlases-dir", &dir]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = run_plan(&root, &[("DBSLOC_LEADDBS_ATLASES_DIR", &dir)], &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = run_plan(&root, &[], &["--leaddbs-atlases-dir"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}

#[test]
fn the_vat_flags_are_validated_and_beat_the_variables() {
    let (_d, root) = root();
    for v in ["auto", "wgpu", "cpu"] {
        let out = run_plan(&root, &[], &["--vat-backend", v]);
        assert_eq!(out.code, 0, "{v}: {}", out.stderr);
    }
    let out = run_plan(&root, &[], &["--vat-backend", "cuda"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    let out = run_plan(&root, &[("DBSLOC_VAT_BACKEND", "cuda")], &[]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("DBSLOC_VAT_BACKEND"), "{}", out.stderr);
    let out = run_plan(
        &root,
        &[("DBSLOC_VAT_BACKEND", "cuda")],
        &["--vat-backend", "cpu"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = run_plan(
        &root,
        &[
            ("DBSLOC_VAT_LABELS", "a,b"),
            ("DBSLOC_VAT_STATS_ATLAS", "x"),
        ],
        &[
            "--vat-labels",
            "c",
            "--vat-labels",
            "d,e",
            "--vat-stats-atlas",
            "y",
        ],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = run_plan(&root, &[], &["--vat-labels"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}
