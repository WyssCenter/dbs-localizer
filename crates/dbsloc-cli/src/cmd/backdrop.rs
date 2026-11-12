// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::sync::atomic::Ordering;

use clap::{Args, Subcommand};
use dbsloc_pipeline::backdrop_catalog::{catalogue, lookup};
use dbsloc_pipeline::backdrop_fetch::{
    fetch_entry, list_backdrops, BackdropEvent, BackdropState, BackdropStatus,
};
use dbsloc_pipeline::template_catalog::{user_data_dir, DATA_DIR_ENV};
use dbsloc_pipeline::template_fetch::CANCELLED;
use serde_json::{json, Value};

use super::interrupt_flag;
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum BackdropCmd {
    /// list the downloadable scene backdrops and whether each is installed
    List(ListArgs),
    /// download a scene backdrop into the per-user data folder (checksum verified; prints the installed path)
    Fetch(FetchArgs),
}

#[derive(Args)]
pub struct ListArgs {
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct FetchArgs {
    /// catalogue id
    pub backdrop: String,
}

pub fn run(cmd: BackdropCmd) -> CmdResult {
    match cmd {
        BackdropCmd::List(a) => list(&a),
        BackdropCmd::Fetch(a) => fetch(&a),
    }
}

fn size_of(r: &BackdropStatus) -> Option<u64> {
    match &r.state {
        BackdropState::Available { size } => Some(*size),
        BackdropState::Installed(_) => None,
    }
}

pub fn rows_json(rows: &[BackdropStatus]) -> Value {
    Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "id": r.entry.id,
                    "space": r.entry.space,
                    "res_mm": r.entry.res_mm,
                    "title": r.entry.title,
                    "status": r.state.label(),
                    "path": r.state.path().map(|p| p.display().to_string()),
                    "size": size_of(r),
                })
            })
            .collect(),
    )
}

pub fn table_lines(rows: &[BackdropStatus]) -> Vec<String> {
    let width = rows.iter().map(|r| r.entry.id.len()).max().unwrap_or(2);
    let mut lines = vec![format!(
        "{:<width$}  {:<22}  {:<10}  size  title",
        "id", "space", "status"
    )];
    for r in rows {
        let size = format!("{:.0} MB", r.entry.size as f64 / 1e6);
        lines.push(format!(
            "{:<width$}  {:<22}  {:<10}  {size}  {}",
            r.entry.id,
            r.entry.space,
            r.state.label(),
            r.entry.title
        ));
    }
    lines
}

fn list(a: &ListArgs) -> CmdResult {
    let rows = list_backdrops(None);
    if a.json {
        out_json(&rows_json(&rows));
    } else {
        for l in table_lines(&rows) {
            out_line(&l);
        }
    }
    Ok(0)
}

fn fetch(a: &FetchArgs) -> CmdResult {
    let key = a.backdrop.trim();
    let Some(entry) = lookup(key) else {
        let valid: Vec<&str> = catalogue().iter().map(|e| e.id).collect();
        return Err(Fail::new(
            2,
            format!(
                "dbsloc: error: unknown backdrop '{key}'; valid ids: {}",
                valid.join(", ")
            ),
        ));
    };
    let data_root = user_data_dir().ok_or_else(|| {
        Fail::new(
            1,
            format!("\ndbsloc: error: no per-user data folder; set {DATA_DIR_ENV}"),
        )
    })?;
    let cancel = interrupt_flag();
    let id = entry.id;
    let result = fetch_entry(entry, &data_root, &cancel, &mut |ev| match ev {
        BackdropEvent::Progress { downloaded, total } => {
            let mut e = std::io::stderr().lock();
            let _ = write!(
                e,
                "\rdownloading {id}: {:.0} / {:.0} MB",
                downloaded as f64 / 1e6,
                total as f64 / 1e6
            );
            let _ = e.flush();
        }
        BackdropEvent::Verifying => err_line("\nverifying checksum"),
        BackdropEvent::Cropping => err_line("cropping to the deep brain region"),
        BackdropEvent::Started { .. } | BackdropEvent::Done { .. } => {}
    });
    match result {
        Ok(p) => {
            out_line(&p.display().to_string());
            Ok(0)
        }
        Err(e) if cancel.load(Ordering::SeqCst) || e == CANCELLED => {
            Err(Fail::new(130, "\ndbsloc: interrupted"))
        }
        Err(e) => Err(Fail::new(1, format!("\ndbsloc: error: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbsloc_pipeline::backdrop_fetch::list_backdrops_in;

    #[test]
    fn table_has_header_and_one_line_per_entry() {
        let rows = list_backdrops_in(None, None);
        let t = table_lines(&rows);
        assert_eq!(t.len(), rows.len() + 1);
        assert!(t[0].starts_with("id"));
        assert!(t.iter().any(|l| l.contains(" MB")));
    }

    #[test]
    fn json_rows_carry_the_documented_keys() {
        let rows = list_backdrops_in(None, None);
        let v = rows_json(&rows);
        for k in ["id", "space", "res_mm", "title", "status", "path", "size"] {
            assert!(v[0].get(k).is_some(), "{k}");
        }
        assert_eq!(v[0]["status"], "available");
    }
}
