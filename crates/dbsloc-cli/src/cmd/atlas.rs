// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Args, Subcommand};
use dbsloc_pipeline::atlas_fetch::{
    default_bundled_dir, fetch_atlas_with, list_atlases_in, manifest, remove_atlas,
    resolve_download, AtlasEvent, AtlasFetchError, AtlasFetchSettings, AtlasStatus,
    DOWNLOAD_NOTICE,
};
use dbsloc_pipeline::template_catalog::{user_data_dir, DATA_DIR_ENV};
use serde_json::{json, Value};

use super::interrupt_flag;
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum AtlasCmd {
    #[command(
        about = "list the Lead-DBS atlases that can be downloaded and the bundled ones, with their status"
    )]
    List(ListArgs),
    #[command(
        about = "download Lead-DBS atlases into the per-user data folder (every file checked against its pinned SHA-256; prints each installed folder)"
    )]
    Fetch(FetchArgs),
    #[command(about = "delete an atlas downloaded with 'dbsloc atlas fetch'")]
    Remove(RemoveArgs),
}

#[derive(Args)]
pub struct ListArgs {
    #[arg(long, help = "print JSON")]
    pub json: bool,
}

#[derive(Args)]
pub struct FetchArgs {
    #[arg(
        required = true,
        help = "atlas names exactly as 'dbsloc atlas list' shows them"
    )]
    pub names: Vec<String>,
}

#[derive(Args)]
pub struct RemoveArgs {
    #[arg(help = "atlas name exactly as 'dbsloc atlas list' shows it")]
    pub name: String,
}

pub fn run(cmd: AtlasCmd) -> CmdResult {
    match cmd {
        AtlasCmd::List(a) => list(&a),
        AtlasCmd::Fetch(a) => fetch(&a),
        AtlasCmd::Remove(a) => remove(&a),
    }
}

fn manifest_fail(e: &AtlasFetchError) -> Fail {
    Fail::new(1, format!("dbsloc: error: {e}"))
}

fn no_data_dir() -> Fail {
    Fail::new(
        1,
        format!("dbsloc: error: no per-user data folder; set {DATA_DIR_ENV}"),
    )
}

pub fn rows_json(rows: &[AtlasStatus]) -> Value {
    Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "bytes": r.bytes,
                    "status": r.state.label(),
                    "path": r.state.path().map(|p| p.display().to_string()),
                })
            })
            .collect(),
    )
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

pub fn table_lines(rows: &[AtlasStatus]) -> Vec<String> {
    let width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let mut lines = vec![format!(
        "{:<width$}  {:>10}  {:<9}  path",
        "name", "size", "status"
    )];
    for r in rows {
        lines.push(
            format!(
                "{:<width$}  {:>10}  {:<9}  {}",
                r.name,
                megabytes(r.bytes),
                r.state.label(),
                r.state
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            )
            .trim_end()
            .to_owned(),
        );
    }
    lines
}

fn list(a: &ListArgs) -> CmdResult {
    let m = manifest().map_err(|e| manifest_fail(&e))?;
    let rows = list_atlases_in(
        m,
        default_bundled_dir().as_deref(),
        user_data_dir().as_deref(),
    );
    if a.json {
        out_json(&rows_json(&rows));
    } else {
        for l in table_lines(&rows) {
            out_line(&l);
        }
    }
    Ok(0)
}

fn print_event(name: &str, ev: AtlasEvent) {
    match ev {
        AtlasEvent::Started {
            total_bytes, files, ..
        } => err_line(&format!(
            "downloading {name}: {files} files, {}",
            megabytes(total_bytes)
        )),
        AtlasEvent::Note { message } => err_line(&message),
        AtlasEvent::Retry {
            attempt,
            wait_secs,
            reason,
        } => err_line(&format!("\n{reason}; retry {attempt} in {wait_secs} s")),
        AtlasEvent::Progress { downloaded, total } => {
            let mut e = std::io::stderr().lock();
            let _ = write!(
                e,
                "\rdownloading {name}: {:.1} / {:.1} MB",
                downloaded as f64 / 1e6,
                total as f64 / 1e6
            );
            let _ = e.flush();
        }
        AtlasEvent::Installing => err_line(""),
        AtlasEvent::Done { .. } => {}
    }
}

pub fn run_fetches(
    names: &[String],
    cancel: &AtomicBool,
    fetch_one: &mut dyn FnMut(&str) -> Result<PathBuf, AtlasFetchError>,
) -> CmdResult {
    let mut failed = false;
    for name in names {
        match fetch_one(name) {
            Ok(p) => out_line(&p.display().to_string()),
            Err(e) if e.is_cancelled() || cancel.load(Ordering::SeqCst) => {
                return Err(Fail::new(130, "\ndbsloc: interrupted"));
            }
            Err(e) => {
                err_line(&format!("\ndbsloc: error: {name}: {e}"));
                failed = true;
            }
        }
    }
    Ok(i32::from(failed))
}

pub fn unique_names(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names.iter().map(|n| n.trim()) {
        if !out.iter().any(|o| o == n) {
            out.push(n.to_owned());
        }
    }
    out
}

fn fetch(a: &FetchArgs) -> CmdResult {
    let m = manifest().map_err(|e| manifest_fail(&e))?;
    let settings = AtlasFetchSettings::default();
    let names = unique_names(&a.names);
    let refused: Vec<String> = names
        .iter()
        .filter_map(|n| {
            resolve_download(m, n, settings.bundled_dir.as_deref())
                .err()
                .map(|e| format!("dbsloc: error: {e}"))
        })
        .collect();
    if !refused.is_empty() {
        return Err(Fail::new(2, refused.join("\n")));
    }
    let data_root = user_data_dir().ok_or_else(no_data_dir)?;
    err_line(DOWNLOAD_NOTICE);
    let cancel = interrupt_flag();
    run_fetches(&names, &cancel, &mut |name| {
        fetch_atlas_with(m, name, &data_root, &settings, &cancel, &mut |ev| {
            print_event(name, ev)
        })
    })
}

fn remove(a: &RemoveArgs) -> CmdResult {
    let data_root = user_data_dir().ok_or_else(no_data_dir)?;
    match remove_atlas(a.name.trim(), &data_root, default_bundled_dir().as_deref()) {
        Ok(p) => {
            out_line(&p.display().to_string());
            Ok(0)
        }
        Err(e) => Err(Fail::new(1, format!("dbsloc: error: {e}"))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use dbsloc_pipeline::atlas_fetch::{AtlasErrorKind, AtlasState};

    fn rows() -> Vec<AtlasStatus> {
        vec![
            AtlasStatus {
                name: "A".to_owned(),
                bytes: 1_500_000,
                state: AtlasState::Available,
            },
            AtlasStatus {
                name: "Bundled one".to_owned(),
                bytes: 28_000_000,
                state: AtlasState::Bundled(PathBuf::from("res/Bundled one")),
            },
        ]
    }

    #[test]
    fn table_and_json_carry_the_documented_columns() {
        let t = table_lines(&rows());
        assert_eq!(t.len(), 3);
        assert!(t[0].starts_with("name"));
        assert!(t[1].contains("1.5 MB") && t[1].ends_with("available"));
        assert!(t[2].contains("bundled"));
        let v = rows_json(&rows());
        for k in ["name", "bytes", "status", "path"] {
            assert!(v[0].get(k).is_some(), "{k}");
        }
        assert!(v[0]["path"].is_null());
        assert_eq!(v[1]["status"], "bundled");
        assert_eq!(v[1]["bytes"], 28_000_000);
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn every_name_is_tried_and_a_failure_exits_1() {
        let cancel = AtomicBool::new(false);
        let mut tried = Vec::new();
        let code = run_fetches(&names(&["a", "b", "c"]), &cancel, &mut |n| {
            tried.push(n.to_owned());
            if n == "b" {
                Err(AtlasFetchError::new(AtlasErrorKind::Host, "down"))
            } else {
                Ok(PathBuf::from(n))
            }
        })
        .unwrap();
        assert_eq!(code, 1);
        assert_eq!(tried, ["a", "b", "c"]);
        let ok = run_fetches(&names(&["a"]), &cancel, &mut |n| Ok(PathBuf::from(n))).unwrap();
        assert_eq!(ok, 0);
    }

    #[test]
    fn a_name_given_twice_is_fetched_once_in_order() {
        assert_eq!(
            unique_names(&names(&["b", " a", "b", "a ", "c"])),
            ["b", "a", "c"]
        );
    }

    #[test]
    fn an_interrupt_exits_130_and_stops() {
        let cancel = AtomicBool::new(false);
        let mut tried = 0;
        let r = run_fetches(&names(&["a", "b"]), &cancel, &mut |_| {
            tried += 1;
            Err(AtlasFetchError::cancelled())
        });
        assert_eq!(r.unwrap_err().code, 130);
        assert_eq!(tried, 1);
    }
}
