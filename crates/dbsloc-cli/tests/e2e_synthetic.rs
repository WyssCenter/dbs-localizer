// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dbsloc, result_of, run, run_args, s, Out};
use dbsloc_pipeline::methods_text::parse_entries;

const STAGES: &str = "preprocess,coreg,normalize,reconstruct";

fn run_stages(
    root: &std::ffi::OsStr,
    tpl: &std::ffi::OsStr,
    el_model: &str,
    stages: &str,
    extra: &[&str],
) -> Out {
    let mut args: Vec<String> = [
        "run",
        &s(std::path::Path::new(root)),
        "01",
        "--stages",
        stages,
        "--template",
        &s(std::path::Path::new(tpl)),
        "--el-model",
        el_model,
        "--recon-method",
        "trac_core_native",
        "--norm-isotropic-mm",
        "full",
        "--no-checkreg",
        "--headless",
        "--no-index",
    ]
    .iter()
    .map(|a| (*a).to_owned())
    .collect();
    args.extend(extra.iter().map(|a| (*a).to_owned()));
    run(dbsloc()
        .env("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", "4")
        .args(&args))
}

#[test]
#[ignore = "needs the synthetic dataset (DBSLOC_E2E_DATASET, DBSLOC_E2E_TEMPLATE) and bundled ANTs; about a minute"]
fn synthetic_head_end_to_end() {
    let (Some(root), Some(tpl)) = (
        std::env::var_os("DBSLOC_E2E_DATASET"),
        std::env::var_os("DBSLOC_E2E_TEMPLATE"),
    ) else {
        eprintln!("skipped: set DBSLOC_E2E_DATASET and DBSLOC_E2E_TEMPLATE (tests/synthetic_head.py write_synthetic_dataset)");
        return;
    };
    let el_model =
        std::env::var("DBSLOC_E2E_EL_MODEL").unwrap_or_else(|_| "Medtronic 3387".to_owned());
    let out = run_stages(&root, &tpl, &el_model, STAGES, &[]);
    let v = out.json();
    for st in STAGES.split(',') {
        assert!(
            result_of(&v, st).starts_with("ok:"),
            "{st}: {v}\n{}",
            out.stderr
        );
    }
    assert_eq!(out.code, 0, "{}", out.stderr);

    let located = run_args(&[
        "methods",
        &s(std::path::Path::new(&root)),
        "01",
        "--no-index",
        "--path",
    ]);
    assert_eq!(located.code, 0, "{}", located.stderr);
    let file = std::path::PathBuf::from(located.stdout.trim());
    let first = std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let doc = parse_entries(&String::from_utf8_lossy(&first));
    let steps: Vec<&str> = doc.entries.iter().map(|e| e.step.as_str()).collect();
    assert_eq!(steps, ["preprocess", "coreg", "normalize", "reconstruct"]);
    assert!(doc.preamble.is_empty());
    for e in &doc.entries {
        assert!(e.raw.contains("Wyss DBS Localizer "), "{}", e.raw);
        assert!(e.raw.ends_with("\n\n"), "{}", e.raw);
    }

    let again = run_stages(
        &root,
        &tpl,
        &el_model,
        "preprocess",
        &["--overwrite-approved"],
    );
    let v = again.json();
    assert!(
        result_of(&v, "preprocess").starts_with("ok:"),
        "{v}\n{}",
        again.stderr
    );
    let second = std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    assert!(second.starts_with(&first));
    let doc = parse_entries(&String::from_utf8_lossy(&second));
    let steps: Vec<&str> = doc.entries.iter().map(|e| e.step.as_str()).collect();
    assert_eq!(
        steps,
        [
            "preprocess",
            "coreg",
            "normalize",
            "reconstruct",
            "preprocess"
        ]
    );

    let oriented = run_stages(&root, &tpl, &el_model, "orientation", &[]);
    let v = oriented.json();
    let outcome = result_of(&v, "orientation");
    let third = std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    if outcome.starts_with("ok:") {
        assert!(third.starts_with(&second), "{v}");
        let added = parse_entries(&String::from_utf8_lossy(&third[second.len()..]));
        assert!(added.entries.len() <= 1, "{v}");
        assert!(added.entries.iter().all(|e| e.step == "orientation"), "{v}");
    } else {
        assert_eq!(third, second, "{outcome}\n{}", oriented.stderr);
    }
}
