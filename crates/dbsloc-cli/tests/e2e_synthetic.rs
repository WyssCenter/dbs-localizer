// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dbsloc, result_of, run, s};

const STAGES: &str = "preprocess,coreg,normalize,reconstruct";

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
    let out = run(dbsloc()
        .env("ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS", "4")
        .args([
            "run",
            &s(std::path::Path::new(&root)),
            "01",
            "--stages",
            STAGES,
            "--template",
            &s(std::path::Path::new(&tpl)),
            "--el-model",
            &el_model,
            "--recon-method",
            "trac_core_native",
            "--norm-isotropic-mm",
            "full",
            "--no-checkreg",
            "--headless",
            "--no-index",
        ]));
    let v = out.json();
    for st in STAGES.split(',') {
        assert!(
            result_of(&v, st).starts_with("ok:"),
            "{st}: {v}\n{}",
            out.stderr
        );
    }
    assert_eq!(out.code, 0, "{}", out.stderr);
}
