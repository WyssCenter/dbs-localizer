#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::protocol::RunEvent;
use dbsloc_pipeline::run_pipeline_subprocess_cmd;
use dbsloc_pipeline::stages::Stage;

const RUNNER: &str = env!("CARGO_BIN_EXE_dbsloc_pipeline_runner");

fn plan_opts(stages: Vec<Stage>, plan_only: bool) -> RunOptions {
    RunOptions {
        stages,
        headless: true,
        no_scene3d: true,
        checkreg: false,
        plan_only,
        stop_on_error: false,
        ..RunOptions::new("/nonexistent/bids", "01")
    }
}

fn runner() -> Command {
    let mut cmd = Command::new(RUNNER);
    cmd.env_remove("DBSLOC_REG_BACKEND");
    cmd
}

fn finished(events: &[RunEvent]) -> bool {
    matches!(events.last(), Some(RunEvent::PipelineCompleted { .. }))
}

#[test]
fn helper_streams_events_and_reports_success() {
    let (tx, rx) = mpsc::channel();
    let opts = plan_opts(vec![Stage::Preprocess, Stage::Normalize], true);
    let status =
        run_pipeline_subprocess_cmd(&opts, tx, Arc::new(AtomicBool::new(false)), runner()).unwrap();
    assert!(status.success());
    let events: Vec<RunEvent> = rx.into_iter().collect();
    assert!(finished(&events), "{events:?}");
    let started = events
        .iter()
        .filter(|e| matches!(e, RunEvent::StageStarted { .. }))
        .count();
    assert_eq!(started, 2);
}

#[test]
fn helper_reports_a_failing_run_through_the_exit_status() {
    let (tx, rx) = mpsc::channel();
    let opts = plan_opts(vec![Stage::Bids], false);
    let status =
        run_pipeline_subprocess_cmd(&opts, tx, Arc::new(AtomicBool::new(false)), runner()).unwrap();
    assert_eq!(status.code(), Some(1));
    let events: Vec<RunEvent> = rx.into_iter().collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RunEvent::StageFailed { .. })),
        "{events:?}"
    );
    assert!(finished(&events));
}

#[test]
fn helper_with_a_preset_cancel_flag_still_terminates_cleanly() {
    let (tx, rx) = mpsc::channel();
    let opts = plan_opts(vec![Stage::Preprocess], true);
    let status =
        run_pipeline_subprocess_cmd(&opts, tx, Arc::new(AtomicBool::new(true)), runner()).unwrap();
    assert!(status.success());
    let events: Vec<RunEvent> = rx.into_iter().collect();
    assert!(finished(&events), "{events:?}");
}

#[test]
fn helper_returns_an_error_for_a_missing_program() {
    let (tx, _rx) = mpsc::channel();
    let opts = plan_opts(vec![Stage::Preprocess], true);
    let cmd = Command::new("dbsloc_no_such_runner_binary");
    assert!(run_pipeline_subprocess_cmd(&opts, tx, Arc::new(AtomicBool::new(false)), cmd).is_err());
}

fn run_raw(input: &[u8], keep_open_then: Option<&[u8]>) -> (Option<i32>, String) {
    let mut child = runner()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).unwrap();
    if let Some(more) = keep_open_then {
        stdin.flush().unwrap();
        stdin.write_all(more).unwrap();
        stdin.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    drop(stdin);
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    let status = child.wait().unwrap();
    (status.code(), out)
}

#[test]
fn json_without_a_newline_and_closed_stdin_still_works() {
    let json = serde_json::to_vec(&plan_opts(vec![Stage::Preprocess], true)).unwrap();
    let (code, out) = run_raw(&json, None);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("pipeline_completed") || out.contains("PipelineCompleted"));
}

#[test]
fn json_line_then_cancel_line_terminates() {
    let mut json = serde_json::to_vec(&plan_opts(vec![Stage::Preprocess], true)).unwrap();
    json.push(b'\n');
    let (code, out) = run_raw(&json, Some(b"cancel\n"));
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("pipeline_completed") || out.contains("PipelineCompleted"));
}

#[test]
fn invalid_json_exits_2() {
    let (code, _out) = run_raw(b"not json\n", None);
    assert_eq!(code, Some(2));
}
