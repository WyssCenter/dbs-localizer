// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::sync::atomic::Ordering;

use clap::{Args, Subcommand};
use dbsloc_pipeline::template_catalog::{lookup, user_data_dir, DATA_DIR_ENV};
use dbsloc_pipeline::template_fetch::{
    fetch_entry, list_entries, EntryState, EntryStatus, FetchEvent, CANCELLED,
};
use serde_json::{json, Value};

use super::interrupt_flag;
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum TemplateCmd {
    /// list catalogue entries and whether each is installed
    List(ListArgs),
    /// download a catalogue template into the per-user data folder (checksum verified; prints the installed T1w path)
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
    /// catalogue id or a unique space name
    pub template: String,
}

pub fn run(cmd: TemplateCmd) -> CmdResult {
    match cmd {
        TemplateCmd::List(a) => list(&a),
        TemplateCmd::Fetch(a) => fetch(&a),
    }
}

fn size_of(r: &EntryStatus) -> Option<u64> {
    match &r.state {
        EntryState::Available { size } => Some(*size),
        _ => None,
    }
}

pub fn rows_json(rows: &[EntryStatus]) -> Value {
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

pub fn table_lines(rows: &[EntryStatus]) -> Vec<String> {
    let width = rows.iter().map(|r| r.entry.id.len()).max().unwrap_or(2);
    let mut lines = vec![format!(
        "{:<width$}  {:<22}  {:<10}  {:<10}  size",
        "id", "space", "resolution", "status"
    )];
    for r in rows {
        let size = match size_of(r) {
            Some(s) if s > 0 => format!("{:.0} MB", s as f64 / 1e6),
            _ => "-".to_owned(),
        };
        let res = format!("{:?} mm", r.entry.res_mm);
        lines.push(format!(
            "{:<width$}  {:<22}  {:<10}  {:<10}  {size}",
            r.entry.id,
            r.entry.space,
            res,
            r.state.label()
        ));
    }
    lines
}

fn list(a: &ListArgs) -> CmdResult {
    let rows = list_entries();
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
    let rows = list_entries();
    let key = a.template.trim();
    let by_id: Vec<&EntryStatus> = rows.iter().filter(|r| r.entry.id == key).collect();
    let hits = if by_id.is_empty() {
        rows.iter().filter(|r| r.entry.space == key).collect()
    } else {
        by_id
    };
    let [row] = hits.as_slice() else {
        let valid: Vec<&str> = rows.iter().map(|r| r.entry.id).collect();
        return Err(Fail::new(
            2,
            format!(
                "dbsloc: error: unknown or ambiguous template '{key}'; valid ids: {}",
                valid.join(", ")
            ),
        ));
    };
    let entry = lookup(row.entry.id).ok_or_else(|| {
        Fail::new(
            2,
            format!("dbsloc: error: unknown template '{}'", row.entry.id),
        )
    })?;
    let data_root = user_data_dir().ok_or_else(|| {
        Fail::new(
            1,
            format!("\ndbsloc: error: no per-user data folder; set {DATA_DIR_ENV}"),
        )
    })?;
    let cancel = interrupt_flag();
    let id = entry.id;
    let result = fetch_entry(entry, &data_root, &cancel, &mut |ev| match ev {
        FetchEvent::Progress { downloaded, total } => {
            let mut e = std::io::stderr().lock();
            let _ = write!(
                e,
                "\rdownloading {id}: {:.0} / {:.0} MB",
                downloaded as f64 / 1e6,
                total as f64 / 1e6
            );
            let _ = e.flush();
        }
        FetchEvent::Verifying => err_line("\nverifying checksum"),
        FetchEvent::Extracting => err_line("extracting"),
        FetchEvent::Started { .. } | FetchEvent::Done { .. } => {}
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
    use dbsloc_pipeline::template_fetch::list_entries_in;

    #[test]
    fn table_has_header_and_one_line_per_entry() {
        let rows = list_entries_in(None, None);
        let t = table_lines(&rows);
        assert_eq!(t.len(), rows.len() + 1);
        assert!(t[0].starts_with("id"));
        assert!(t[0].ends_with("size"));
        assert!(t[1].contains("1.0 mm"));
        assert!(t.iter().any(|l| l.contains(" MB")));
    }

    #[test]
    fn json_rows_carry_the_documented_keys() {
        let rows = list_entries_in(None, None);
        let v = rows_json(&rows);
        let first = &v[0];
        for k in ["id", "space", "res_mm", "title", "status", "path", "size"] {
            assert!(first.get(k).is_some(), "{k}");
        }
        assert_eq!(first["size"], Value::Null);
    }
}
