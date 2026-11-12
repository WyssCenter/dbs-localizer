#![allow(dead_code)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!("stdout is not JSON ({e}): {}\n{}", self.stdout, self.stderr)
        })
    }
}

pub fn dbsloc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dbsloc"));
    for (k, _) in std::env::vars() {
        if k.starts_with("DBSLOC_") || k.starts_with("HPDBS_") {
            c.env_remove(k);
        }
    }
    c.env("DBSLOC_NO_BRIDGE", "1");
    c.env("DBSLOC_OPEN_SCENE3D", "0");
    c
}

pub fn run(cmd: &mut Command) -> Out {
    let o = cmd
        .output()
        .unwrap_or_else(|e| panic!("cannot run dbsloc: {e}"));
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

pub fn run_args(args: &[&str]) -> Out {
    run(dbsloc().args(args))
}

pub fn write(path: &Path, bytes: &[u8]) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

pub fn dataset(root: &Path, sid: &str) -> PathBuf {
    for (ses, modality) in [("preop", "T1w"), ("postop", "CT")] {
        let p = root
            .join("rawdata")
            .join(format!("sub-{sid}"))
            .join(format!("ses-{ses}"))
            .join("anat")
            .join(format!("sub-{sid}_ses-{ses}_{modality}.nii.gz"));
        write(&p, b"x");
    }
    root.to_path_buf()
}

pub fn t1_of(root: &Path, sid: &str) -> PathBuf {
    root.join("rawdata")
        .join(format!("sub-{sid}"))
        .join("ses-preop")
        .join("anat")
        .join(format!("sub-{sid}_ses-preop_T1w.nii.gz"))
}

pub fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub fn results(v: &Value) -> Vec<(String, String)> {
    v["results"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    (
                        r["stage"].as_str().unwrap_or_default().to_owned(),
                        r["result"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn result_of(v: &Value, stage: &str) -> String {
    results(v)
        .into_iter()
        .find(|(st, _)| st == stage)
        .map(|(_, r)| r)
        .unwrap_or_default()
}
