#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::process::{Command, Stdio};

use dbsloc_pipeline::protocol::RunEvent;

const RUNNER: &str = env!("CARGO_BIN_EXE_dbsloc_pipeline_runner");

struct Ran {
    code: Option<i32>,
    events: Vec<RunEvent>,
    stderr: String,
}

fn run_runner(options: &str, env: &[(&str, &str)]) -> Ran {
    let mut cmd = Command::new(RUNNER);
    for (k, _) in std::env::vars() {
        if k.starts_with("DBSLOC_") {
            cmd.env_remove(k);
        }
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(options.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let events = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    Ran {
        code: out.status.code(),
        events,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn started(ran: &Ran) -> usize {
    ran.events
        .iter()
        .filter(|e| matches!(e, RunEvent::StageStarted { .. }))
        .count()
}

const FAILING: &str = r#""bids_dir": "/nonexistent/bids", "subject": "01", "stages": ["preprocess", "normalize"], "headless": true, "no_scene3d": true"#;

#[test]
fn an_unset_stop_on_error_follows_the_variable_then_checkreg() {
    let no_key = format!(r#"{{{FAILING}, "checkreg": false}}"#);
    assert_eq!(started(&run_runner(&no_key, &[])), 2);
    assert_eq!(
        started(&run_runner(&no_key, &[("DBSLOC_STOP_ON_ERROR", "1")])),
        1
    );
    let gated = format!(r#"{{{FAILING}, "checkreg": true}}"#);
    assert_eq!(started(&run_runner(&gated, &[])), 1);
    assert_eq!(
        started(&run_runner(&gated, &[("DBSLOC_STOP_ON_ERROR", "0")])),
        2
    );
}

#[test]
fn explicit_json_fields_win_over_the_environment() {
    let explicit = format!(r#"{{{FAILING}, "checkreg": true, "stop_on_error": false}}"#);
    assert_eq!(
        started(&run_runner(&explicit, &[("DBSLOC_STOP_ON_ERROR", "1")])),
        2
    );
    let checkreg_off = format!(r#"{{{FAILING}, "checkreg": false}}"#);
    assert_eq!(
        started(&run_runner(&checkreg_off, &[("DBSLOC_OPEN_CHECKREG", "1")])),
        2,
        "an explicit checkreg = false keeps the derived stop_on_error off"
    );
}

#[test]
fn an_invalid_variable_matters_only_for_a_field_the_json_leaves_unset() {
    let plan = r#""bids_dir": "/nonexistent/bids", "subject": "01", "stages": ["preprocess"], "plan_only": true, "checkreg": false"#;
    let ran = run_runner(&format!("{{{plan}}}"), &[("DBSLOC_REG_BACKEND", "cuda")]);
    assert_eq!(ran.code, Some(2));
    assert!(ran.stderr.contains("DBSLOC_REG_BACKEND"), "{}", ran.stderr);
    let ran = run_runner(
        &format!(r#"{{{plan}, "reg_backend": "ants"}}"#),
        &[("DBSLOC_REG_BACKEND", "cuda")],
    );
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let ran = run_runner("{not json", &[]);
    assert_eq!(ran.code, Some(2));
    assert!(
        ran.stderr.contains("invalid RunOptions JSON"),
        "{}",
        ran.stderr
    );
}
