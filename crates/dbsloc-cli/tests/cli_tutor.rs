#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use common::{dbsloc, run, run_args, s, write};
use serde_json::{json, Value};

fn side(x: f64) -> Value {
    json!({"markers": {
        "head": [x, -8.0, -9.0],
        "tail": [x, -3.0, -1.0],
        "x": [x + 0.65, -8.0, -9.0],
    }})
}

fn write_reco(root: &Path, subj: &str, x: f64) -> PathBuf {
    let p = root
        .join("derivatives/leaddbs")
        .join(format!("sub-{subj}/reconstruction"))
        .join(format!("sub-{subj}_desc-reconstruction.json"));
    let v = json!({
        "version": 2,
        "subj_id": subj,
        "method": "test",
        "mni": {"1": side(x), "2": side(-x)},
        "meta": {"side_keys": "1=right,2=left"},
    });
    write(&p, &serde_json::to_vec_pretty(&v).unwrap());
    p
}

const EXAMPLE_BODY: &str = "{\"pt_15454\":{\"leftElectrode\":{\"head\":[0,0,0],\"tail\":[0,0,-6],\"x\":[1,0,0]},\"rightElectrode\":{\"head\":[1,0,0],\"tail\":[1,0,-6],\"x\":[0,1,0]}}}";

fn serve_once(status_line: &'static str) -> (String, std::thread::JoinHandle<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/my-endpoint", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut split = None;
        let mut want = 0usize;
        loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if split.is_none() {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    split = Some(pos + 4);
                    let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                    want = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                }
            }
            if let Some(start) = split {
                if buf.len() >= start + want {
                    break;
                }
            }
        }
        let start = split.unwrap();
        let head = String::from_utf8_lossy(&buf[..start - 4]).into_owned();
        let body = buf[start..start + want].to_vec();
        let reply = format!("{status_line}\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
        stream.write_all(reply.as_bytes()).unwrap();
        (head, body)
    });
    (url, handle)
}

#[test]
fn print_example_prints_the_template() {
    let out = run_args(&["tutor", "submit-file", "--print-example"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(
        v["pt_15454"]["leftElectrode"]["tail"],
        json!([0.0, 0.0, -6.0])
    );
}

#[test]
fn submit_file_without_a_path_exits_2() {
    let out = run_args(&["tutor", "submit-file"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("json_path required"), "{}", out.stderr);
}

#[test]
fn submit_file_validate_dry_run_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("p.json");
    write(&p, EXAMPLE_BODY.as_bytes());
    let ps = s(&p);
    let out = run_args(&["tutor", "submit-file", &ps, "--validate-only"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        format!("Lead-Tutor JSON OK, payload bytes: {}", EXAMPLE_BODY.len())
    );
    let out = run_args(&[
        "tutor",
        "submit-file",
        &ps,
        "--dry-run",
        "--user-id",
        "482197",
        "--post-url",
        "http://127.0.0.1:1/x",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out
        .stdout
        .contains(&format!("dry-run: JSON OK, bytes: {}", EXAMPLE_BODY.len())));
    assert!(out.stdout.contains("Would POST to: http://127.0.0.1:1/x"));
    assert!(out.stdout.contains("User-ID: 482197"));

    let missing = s(&dir.path().join("none.json"));
    let out = run_args(&["tutor", "submit-file", &missing]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("error: not a file:"));

    let bad = dir.path().join("bad.json");
    write(&bad, b"{\"pt_15454\":{}}");
    let out = run_args(&["tutor", "submit-file", &s(&bad)]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("validation failed"));

    let mut extra: Value = serde_json::from_str(EXAMPLE_BODY).unwrap();
    extra["pt_15454"]["leftElectrode"]["name"] = json!("someone");
    write(&bad, extra.to_string().as_bytes());
    let out = run_args(&["tutor", "submit-file", &s(&bad), "--validate-only"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("unknown field 'name'"),
        "{}",
        out.stderr
    );
}

fn tutor(data_dir: &Path) -> std::process::Command {
    let mut c = dbsloc();
    c.env("DBSLOC_DATA_DIR", data_dir)
        .env_remove("LEAD_TUTOR_USER_ID")
        .stdin(Stdio::null());
    c
}

fn example_file(dir: &Path) -> String {
    let p = dir.join("p.json");
    write(&p, EXAMPLE_BODY.as_bytes());
    s(&p)
}

fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
    })
}

fn is_generated(id: &str) -> bool {
    id.strip_prefix("dbsloc-")
        .is_some_and(|h| h.len() == 12 && h.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
}

#[test]
fn a_dry_run_with_nothing_configured_says_so_and_creates_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let ps = example_file(dir.path());
    let out = run(tutor(&data).args(["tutor", "submit-file", &ps, "--dry-run"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("User-ID: (not set; the upload asks for one or creates a random one)"),
        "{}",
        out.stdout
    );
    assert!(!data.exists(), "dry-run must not create the data folder");
    let out = run(tutor(&data).args(["tutor", "submit-file", &ps, "--validate-only"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!data.exists());
}

#[test]
fn the_environment_id_wins_and_unsafe_ids_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let ps = example_file(dir.path());
    let out = run(tutor(&data).env("LEAD_TUTOR_USER_ID", " run-7 ").args([
        "tutor",
        "submit-file",
        &ps,
        "--dry-run",
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("User-ID: run-7"), "{}", out.stdout);
    let out = run(tutor(&data)
        .env("LEAD_TUTOR_USER_ID", "run-7")
        .args(["tutor", "user-id"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.trim(), "run-7 (from LEAD_TUTOR_USER_ID)");
    let out = run(tutor(&data).env("LEAD_TUTOR_USER_ID", "a b").args([
        "tutor",
        "submit-file",
        &ps,
        "--dry-run",
    ]));
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("invalid User-ID"), "{}", out.stderr);
    for bad in ["a b", "x&userid=1", ""] {
        let out =
            run(tutor(&data).args(["tutor", "submit-file", &ps, "--dry-run", "--user-id", bad]));
        assert_eq!(out.code, 2, "{bad:?}");
        assert!(out.stderr.contains("invalid User-ID"), "{}", out.stderr);
    }
}

#[test]
fn user_id_set_show_and_forget() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let ps = example_file(dir.path());
    let saved = data.join("lead_tutor.json");

    let out = run(tutor(&data).args(["tutor", "user-id"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "not set; the next upload asks for one or creates a random one"
    );

    let out = run(tutor(&data).args(["tutor", "user-id", "--set", "a b"]));
    assert_eq!(out.code, 2);
    assert!(!saved.exists());

    let out = run(tutor(&data).args(["tutor", "user-id", "--set", "run-7"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("Lead-Tutor user id run-7 saved in "),
        "{}",
        out.stdout
    );
    let file: Value = serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
    assert_eq!(file, json!({"user_id": "run-7"}));

    let out = run(tutor(&data).args(["tutor", "submit-file", &ps, "--dry-run"]));
    assert!(out.stdout.contains("User-ID: run-7"), "{}", out.stdout);

    let out = run(tutor(&data).args(["tutor", "user-id"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.starts_with("run-7 (saved in "), "{}", out.stdout);
    assert!(out.stdout.contains("lead_tutor.json"), "{}", out.stdout);

    let out = run(tutor(&data).args(["tutor", "user-id", "--set", "x", "--forget"]));
    assert_eq!(out.code, 2);

    let out = run(tutor(&data).args(["tutor", "user-id", "--forget"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!saved.exists());
    let out = run(tutor(&data).args(["tutor", "user-id", "--forget"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("No saved Lead-Tutor user id"),
        "{}",
        out.stdout
    );
}

#[test]
fn a_broken_saved_file_fails_a_dry_run_with_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let ps = example_file(dir.path());
    let saved = data.join("lead_tutor.json");
    for content in ["{", "{\"other\": 1}", "{\"user_id\": \"a b\"}"] {
        write(&saved, content.as_bytes());
        let out = run(tutor(&data).args(["tutor", "submit-file", &ps, "--dry-run"]));
        assert_eq!(out.code, 2, "{content}: {}", out.stdout);
        assert!(out.stderr.contains("lead_tutor.json"), "{}", out.stderr);
        assert!(
            out.stderr.contains("dbsloc tutor user-id --set"),
            "{}",
            out.stderr
        );
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), content);
    }
}

#[test]
fn an_upload_without_an_id_creates_one_saves_it_and_reuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let ps = example_file(dir.path());
    let mut sent = Vec::new();
    for round in 0..2 {
        let (url, handle) = serve_once("HTTP/1.1 200 OK");
        let out = run(tutor(&data).args(["tutor", "submit-file", &ps, "--post-url", &url]));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let (head, _) = handle.join().unwrap();
        let id = header_value(&head, "user-id").unwrap();
        assert!(is_generated(&id), "{id}");
        if round == 0 {
            assert!(
                out.stderr
                    .contains(&format!("dbsloc: note: Lead-Tutor user id {id} saved in ")),
                "{}",
                out.stderr
            );
        } else {
            assert!(!out.stderr.contains("saved in"), "{}", out.stderr);
        }
        sent.push(id);
    }
    assert_eq!(sent[0], sent[1]);
    let file: Value =
        serde_json::from_slice(&std::fs::read(data.join("lead_tutor.json")).unwrap()).unwrap();
    assert_eq!(file["user_id"], json!(sent[0]));
}

#[test]
fn submit_file_posts_the_exact_body_with_both_headers() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("p.json");
    write(&p, EXAMPLE_BODY.as_bytes());
    let (url, handle) = serve_once("HTTP/1.1 200 OK");
    let out = run(dbsloc()
        .env("LEAD_TUTOR_DASHBOARD_URL", "http://dash.invalid/?userid=")
        .args(["tutor", "submit-file", &s(&p)])
        .args(["--user-id", "482197", "--post-url", &url]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("HTTP 200"));
    assert!(out.stdout.contains("Response (truncated): hello"));
    assert!(out
        .stdout
        .replace("\r\n", "\n")
        .contains("Open in browser (Lead-Tutor feedback):\nhttp://dash.invalid/?userid=482197"));
    let (head, body) = handle.join().unwrap();
    assert!(head.starts_with("POST /my-endpoint HTTP/1.1"), "{head}");
    let lower = head.to_lowercase();
    assert!(lower.contains("content-type: application/json"), "{head}");
    assert!(lower.contains("user-id: 482197"), "{head}");
    assert_eq!(String::from_utf8(body).unwrap(), EXAMPLE_BODY);
}

#[test]
fn http_errors_and_network_errors_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("p.json");
    write(&p, EXAMPLE_BODY.as_bytes());
    let (url, handle) = serve_once("HTTP/1.1 500 Internal Server Error");
    let out = run_args(&[
        "tutor",
        "submit-file",
        &s(&p),
        "--user-id",
        "u",
        "--post-url",
        &url,
    ]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("HTTP 500 Internal Server Error"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("hello"));
    handle.join().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}/x", listener.local_addr().unwrap());
    drop(listener);
    let out = run_args(&[
        "tutor",
        "submit-file",
        &s(&p),
        "--user-id",
        "u",
        "--post-url",
        &dead,
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("Network error:"));
    assert!(out.stderr.contains("curl -sI"));
}

#[test]
fn submit_recon_posts_the_hemisphere_mapped_payload() {
    let dir = tempfile::tempdir().unwrap();
    let rp = write_reco(dir.path(), "15454", 22.0);
    let (url, handle) = serve_once("HTTP/1.1 200 OK");
    let out = run_args(&[
        "tutor",
        "submit-recon",
        "--recon-json",
        &s(&rp),
        "--no-open-browser",
        "--user-id",
        "482197",
        "--post-url",
        &url,
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let (_, body) = handle.join().unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["pt_15454"]["leftElectrode"]["head"][0], json!(22.0));
    assert_eq!(v["pt_15454"]["rightElectrode"]["head"][0], json!(-22.0));
}

#[test]
fn submit_recon_validate_dry_run_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let rp = write_reco(dir.path(), "15454", 22.0);
    let rs = s(&rp);
    let out = run_args(&[
        "tutor",
        "submit-recon",
        "--recon-json",
        &rs,
        "--validate-only",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.starts_with("Lead-Tutor payload OK, bytes: "));
    let out = run_args(&[
        "tutor",
        "submit-recon",
        "--recon-json",
        &rs,
        "--dry-run",
        "--user-id",
        "482197",
        "--no-open-browser",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("dry-run: would POST "));
    assert!(out.stdout.contains("User-ID: 482197"));
    assert!(out.stdout.contains("open browser: no"));
    let out = run_args(&["tutor", "submit-recon"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("use --recon-json PATH"));
    let out = run_args(&[
        "tutor",
        "submit-recon",
        "--recon-json",
        &s(&dir.path().join("nope.json")),
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("error: reconstruction JSON not found:"));
}

#[test]
fn submit_all_merges_validates_and_requires_all() {
    let dir = tempfile::tempdir().unwrap();
    write_reco(dir.path(), "15454", 22.0);
    write_reco(dir.path(), "29781", 21.0);
    write_reco(dir.path(), "77777", 20.0);
    let root = s(dir.path());
    let out = run_args(&["tutor", "submit-all-recon", &root, "--validate-only"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("warning: sub-77777: skipped, not a Lead-Tutor subject"),
        "{}",
        out.stderr
    );
    assert!(out
        .stdout
        .starts_with("Lead-Tutor merged payload OK, patients: 2 bytes: "));
    let out = run_args(&[
        "tutor",
        "submit-all-recon",
        &root,
        "--dry-run",
        "--user-id",
        "7",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("patients ['pt_15454', 'pt_29781']"));
    let out = run_args(&[
        "tutor",
        "submit-all-recon",
        &root,
        "--require-all",
        "--validate-only",
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("missing Lead-Tutor patient"));
    let empty = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(empty.path().join("derivatives/leaddbs")).unwrap();
    let out = run_args(&[
        "tutor",
        "submit-all-recon",
        &s(empty.path()),
        "--validate-only",
    ]);
    assert_eq!(out.code, 1);
    assert!(out
        .stderr
        .contains("no reconstruction JSON found under derivatives/leaddbs"));
}

#[test]
fn submit_reads_reconstructions_from_the_output_folder() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("results");
    let staged = write_reco(dir.path(), "15454", 22.0);
    let target = out_dir.join("sub-15454/reconstruction/sub-15454_desc-reconstruction.json");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::rename(&staged, &target).unwrap();
    std::fs::remove_dir_all(dir.path().join("derivatives")).unwrap();
    write(
        &dir.path()
            .join("rawdata/sub-15454/ses-preop/anat/sub-15454_ses-preop_T1w.nii.gz"),
        b"x",
    );
    let root = s(dir.path());
    let out = run_args(&[
        "tutor",
        "submit-all-recon",
        &root,
        "--validate-only",
        "--output-dir",
        &s(&out_dir),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("patients: 1"), "{}", out.stdout);
    let out = run(dbsloc().env("DBSLOC_OUTPUT_DIR", &out_dir).args([
        "tutor",
        "submit-recon",
        &root,
        "15454",
        "--no-index",
        "--validate-only",
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.starts_with("Lead-Tutor payload OK"));
    let out = run_args(&["tutor", "submit-all-recon", &root, "--validate-only"]);
    assert_eq!(out.code, 1, "{}", out.stdout);
}
