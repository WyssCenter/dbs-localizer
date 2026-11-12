// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use clap::{Args, Subcommand};
use dbsloc_pipeline::connectome_catalog::Access;
use dbsloc_pipeline::connectome_fetch::ConnectomeRoots;
use dbsloc_pipeline::leaddbs_data::{
    bundled_atlas_names, copy, default_selection, link_plan, linked_entries_text, scan,
    user_atlases_dir_opt, ConnectomeItem, CopyEvent, CopyOutcome, DataKind, Destinations,
    Inventory, LeadDbsCode, LeadDbsDataError, ScanContext, Selection,
};
use dbsloc_pipeline::template_catalog::DATA_DIR_ENV;
use serde_json::{json, Value};

use super::interrupt_flag;
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

const ATLASES_ENV: &str = "DBSLOC_LEADDBS_ATLASES_DIR";
const CONNECTOMES_ENV: &str = "DBSLOC_LEADDBS_CONNECTOMES_DIR";

#[derive(Subcommand)]
pub enum LeadDbsCmd {
    /// list the atlases and connectomes found in a Lead-DBS installation or data folder (writes nothing)
    Scan(ScanArgs),
    /// copy atlases and connectomes of a Lead-DBS installation into Wyss DBS Localizer (the Lead-DBS folder is never changed)
    Copy(CopyArgs),
}

#[derive(Args)]
pub struct ScanArgs {
    /// Lead-DBS installation folder (contains templates), the folder that contains connectomes, or a connectome folder itself
    pub dir: PathBuf,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct CopyArgs {
    /// Lead-DBS installation folder (contains templates), the folder that contains connectomes, or a connectome folder itself
    pub dir: PathBuf,
    /// copy every atlas and connectome found, also those already included or already there
    #[arg(long, conflicts_with_all = ["atlas", "connectome"])]
    pub all: bool,
    /// copy this atlas (repeat for several); default is every atlas that does not ship with the application
    #[arg(long, value_name = "NAME")]
    pub atlas: Vec<String>,
    /// copy this connectome (repeat for several); default is every connectome not yet present
    #[arg(long, value_name = "NAME")]
    pub connectome: Vec<String>,
    /// primary connectomes folder (default: DBSLOC_CONNECTOMES_DIR, else the per-user data folder)
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

pub fn run(cmd: LeadDbsCmd) -> CmdResult {
    match cmd {
        LeadDbsCmd::Scan(a) => scan_cmd(&a),
        LeadDbsCmd::Copy(a) => copy_cmd(&a),
    }
}

fn fail_of(e: &LeadDbsDataError) -> Fail {
    match e.code {
        LeadDbsCode::Cancelled => Fail::new(130, "\ndbsloc: interrupted"),
        _ => Fail::new(1, format!("dbsloc: error: {e}")),
    }
}

fn size_text(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

fn context_for(roots: ConnectomeRoots) -> ScanContext {
    let resources =
        dbsloc_pipeline::bundled_bin::find_resources_root().map(|r| r.join("resources"));
    ScanContext {
        bundled_atlases: bundled_atlas_names(resources.as_deref()),
        roots,
    }
}

fn resolve_roots(flag: Option<&Path>) -> Result<ConnectomeRoots, Fail> {
    ConnectomeRoots::resolve(flag).map_err(|e| Fail::new(1, format!("dbsloc: error: {e}")))
}

fn scan_context_of<E>(roots: Result<ConnectomeRoots, E>) -> ScanContext {
    context_for(roots.unwrap_or_else(|_| ConnectomeRoots::from_primary(PathBuf::new())))
}

fn json_error_value(message: &str) -> Value {
    let text = message
        .trim()
        .trim_start_matches("dbsloc: error:")
        .trim_start_matches("dbsloc:")
        .trim();
    json!({ "error": text })
}

fn json_fail(json: bool, fail: Fail) -> Fail {
    if json {
        out_json(&json_error_value(
            fail.message.as_deref().unwrap_or("failed"),
        ));
    }
    fail
}

fn run_scan(
    dir: &Path,
    ctx: &ScanContext,
    cancel: &AtomicBool,
    show_progress: bool,
) -> Result<Inventory, Fail> {
    let mut progress = |n: usize| {
        if show_progress {
            let mut e = std::io::stderr().lock();
            let _ = write!(e, "\rsized {n} folders");
            let _ = e.flush();
        }
    };
    let result = scan(dir, ctx, cancel, &mut progress);
    if show_progress {
        err_line("");
    }
    result.map_err(|e| fail_of(&e))
}

fn connectome_note(c: &ConnectomeItem) -> String {
    let mut notes = Vec::new();
    if c.present {
        notes.push("Already there");
    }
    match c.access {
        Some(Access::Dua) => notes.push("Needs your data use agreement"),
        Some(Access::CohortTerms) => notes.push("Cohort data terms"),
        _ => {}
    }
    notes.join("; ")
}

pub fn table_lines(inv: &Inventory) -> Vec<String> {
    let mut lines = Vec::new();
    if !inv.atlases.is_empty() {
        let width = inv
            .atlases
            .iter()
            .map(|a| a.name.len())
            .max()
            .unwrap_or(4)
            .max(4);
        lines.push(format!(
            "Atlases ({}, {})",
            inv.atlases.len(),
            size_text(inv.atlas_bytes())
        ));
        lines.push(format!(
            "{:<width$}  {:>10}  {:>7}  notes",
            "name", "size", "files"
        ));
        for a in &inv.atlases {
            let note = if a.bundled { "Already included" } else { "" };
            lines.push(format!(
                "{:<width$}  {:>10}  {:>7}  {note}",
                a.name,
                size_text(a.bytes),
                a.files
            ));
        }
    }
    if !inv.connectomes.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        let width = inv
            .connectomes
            .iter()
            .map(|c| c.name.len())
            .max()
            .unwrap_or(4)
            .max(4);
        lines.push(format!(
            "Connectomes ({}, {})",
            inv.connectomes.len(),
            size_text(inv.connectome_bytes())
        ));
        lines.push(format!(
            "{:<10}  {:<width$}  {:>10}  {:>7}  notes",
            "kind", "name", "size", "files"
        ));
        for c in &inv.connectomes {
            lines.push(format!(
                "{:<10}  {:<width$}  {:>10}  {:>7}  {}",
                c.class.kind_label(),
                c.name,
                size_text(c.bytes),
                c.files,
                connectome_note(c)
            ));
        }
    }
    lines
}

fn scan_cmd(a: &ScanArgs) -> CmdResult {
    scan_inner(a).map_err(|f| json_fail(a.json, f))
}

fn scan_inner(a: &ScanArgs) -> CmdResult {
    let ctx = scan_context_of(resolve_roots(None));
    let cancel = interrupt_flag();
    let inv = run_scan(&a.dir, &ctx, &cancel, !a.json)?;
    let plan = link_plan(&inv);
    if a.json {
        let mut v = serde_json::to_value(&inv).unwrap_or(Value::Null);
        v["atlas_bytes"] = json!(inv.atlas_bytes());
        v["connectome_bytes"] = json!(inv.connectome_bytes());
        v["link"] = json!({
            "atlases_dir": plan.atlases_dir,
            "connectomes_dir": plan.connectomes_dir,
        });
        out_json(&v);
        return Ok(0);
    }
    out_line(&format!("Source: {}", inv.source.display()));
    for l in table_lines(&inv) {
        out_line(&l);
    }
    for n in &inv.notes {
        out_line(n);
    }
    if plan.atlases_dir.is_some() || plan.connectomes_dir.is_some() {
        out_line("");
        out_line("To use them where they are, set:");
        if let Some(p) = &plan.atlases_dir {
            out_line(&format!("{ATLASES_ENV}={}", p.display()));
        }
        if let Some(p) = &plan.connectomes_dir {
            out_line(&format!("{CONNECTOMES_ENV}={}", p.display()));
        }
    }
    Ok(0)
}

fn pick(kind: &str, wanted: &[String], names: Vec<String>) -> Result<Vec<String>, Fail> {
    let unknown: Vec<&String> = wanted.iter().filter(|w| !names.contains(w)).collect();
    if unknown.is_empty() {
        return Ok(wanted.to_vec());
    }
    Err(Fail::new(
        2,
        format!(
            "dbsloc: error: unknown {kind} name '{}'; found: {}",
            unknown[0],
            if names.is_empty() {
                "none".to_owned()
            } else {
                names.join(", ")
            }
        ),
    ))
}

fn selection_of(a: &CopyArgs, inv: &Inventory) -> Result<Selection, Fail> {
    if a.all {
        return Ok(Selection {
            atlases: inv.atlases.iter().map(|x| x.name.clone()).collect(),
            connectomes: inv
                .connectomes
                .iter()
                .map(|c| (c.class, c.name.clone()))
                .collect(),
        });
    }
    if a.atlas.is_empty() && a.connectome.is_empty() {
        return Ok(default_selection(inv));
    }
    let atlases = pick(
        "atlas",
        &a.atlas,
        inv.atlases.iter().map(|x| x.name.clone()).collect(),
    )?;
    let wanted = pick(
        "connectome",
        &a.connectome,
        inv.connectomes.iter().map(|c| c.name.clone()).collect(),
    )?;
    let connectomes = inv
        .connectomes
        .iter()
        .filter(|c| wanted.contains(&c.name))
        .map(|c| (c.class, c.name.clone()))
        .collect();
    Ok(Selection {
        atlases,
        connectomes,
    })
}

fn report_event(ev: &CopyEvent) {
    match ev {
        CopyEvent::ItemStarted {
            kind,
            name,
            index,
            count,
        } => err_line(&format!(
            "\ncopying {} {name} ({} of {count})",
            kind.label(),
            index + 1
        )),
        CopyEvent::Bytes { copied, total } => {
            let mut e = std::io::stderr().lock();
            let _ = write!(
                e,
                "\r{:.0} / {:.0} MB",
                *copied as f64 / 1e6,
                *total as f64 / 1e6
            );
            let _ = e.flush();
        }
        CopyEvent::ItemSkipped { kind, name } => {
            err_line(&format!("already there, skipped: {} {name}", kind.label()));
        }
        CopyEvent::ItemDone { .. } | CopyEvent::AllDone => {}
    }
}

fn licence_names(inv: &Inventory, outcome: &CopyOutcome) -> Vec<String> {
    inv.connectomes
        .iter()
        .filter(|c| matches!(c.access, Some(Access::Dua | Access::CohortTerms)))
        .filter(|c| {
            outcome
                .copied
                .iter()
                .any(|i| i.kind == DataKind::Connectome(c.class) && i.name == c.name)
        })
        .map(|c| format!("{} ({})", c.name, connectome_note(c)))
        .collect()
}

fn print_outcome(inv: &Inventory, outcome: &CopyOutcome) {
    for i in &outcome.copied {
        out_line(&format!(
            "copied   {:<10} {}  {}",
            i.kind.label(),
            i.name,
            size_text(i.bytes)
        ));
        if let Some(l) = linked_entries_text(i.skipped_links) {
            out_line(&format!("         {:<10} {}: {l}", i.kind.label(), i.name));
        }
    }
    for i in &outcome.skipped {
        out_line(&format!(
            "skipped  {:<10} {}  already there",
            i.kind.label(),
            i.name
        ));
    }
    for n in &outcome.notes {
        out_line(&format!("note: {n}"));
    }
    if let Some(p) = &outcome.atlases_dir {
        out_line("");
        out_line("Atlases are in the folder below; point the Lead-DBS atlases directory at it:");
        out_line(&format!("{ATLASES_ENV}={}", p.display()));
    }
    let needing = licence_names(inv, outcome);
    if !needing.is_empty() {
        out_line("");
        out_line(
            "Record your acceptance of the terms for these connectomes in the Connectomes window:",
        );
        for n in needing {
            out_line(&format!("  {n}"));
        }
    }
}

fn copy_failure(inv: &Inventory, e: &LeadDbsDataError, json: bool) -> Fail {
    if let Some(partial) = &e.partial {
        if json {
            let mut v = json!({
                "error": { "code": e.code.code(), "message": e.message },
                "partial": serde_json::to_value(partial.as_ref()).unwrap_or(Value::Null),
            });
            v["partial"]["needs_licence_record"] = json!(licence_names(inv, partial));
            out_json(&v);
        } else {
            err_line("");
            if !partial.copied.is_empty() {
                out_line("Completed before the stop:");
            }
            print_outcome(inv, partial);
        }
        return fail_of(e);
    }
    json_fail(json, fail_of(e))
}

fn copy_cmd(a: &CopyArgs) -> CmdResult {
    copy_inner(a).map_err(|f| match f {
        CopyFail::Reported(f) => f,
        CopyFail::Plain(f) => json_fail(a.json, f),
    })
}

enum CopyFail {
    Reported(Fail),
    Plain(Fail),
}

impl From<Fail> for CopyFail {
    fn from(f: Fail) -> Self {
        CopyFail::Plain(f)
    }
}

fn copy_inner(a: &CopyArgs) -> Result<i32, CopyFail> {
    let roots = resolve_roots(a.root.as_deref())?;
    let atlases_dir = user_atlases_dir_opt().ok_or_else(|| {
        Fail::new(
            1,
            format!("dbsloc: error: no per-user data folder; set {DATA_DIR_ENV}"),
        )
    })?;
    let primary = roots.primary.clone();
    let ctx = context_for(roots);
    let cancel = interrupt_flag();
    let inv = run_scan(&a.dir, &ctx, &cancel, !a.json)?;
    let selection = selection_of(a, &inv)?;
    let dest = Destinations {
        atlases_dir,
        connectomes_root: primary,
        free_space_override: None,
    };
    let show = !a.json;
    let outcome = copy(&inv, &selection, &dest, &cancel, &mut |ev| {
        if show {
            report_event(&ev);
        }
    })
    .map_err(|e| CopyFail::Reported(copy_failure(&inv, &e, a.json)))?;
    if a.json {
        let mut v = serde_json::to_value(&outcome).unwrap_or(Value::Null);
        v["needs_licence_record"] = json!(licence_names(&inv, &outcome));
        out_json(&v);
        return Ok(0);
    }
    err_line("");
    print_outcome(&inv, &outcome);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_root_lookup_gives_a_context_without_roots() {
        let ctx = scan_context_of::<()>(Err(()));
        assert!(ctx.roots.primary.as_os_str().is_empty());
        assert!(ctx.roots.external.is_none());
    }

    #[test]
    fn a_found_root_lookup_is_kept() {
        let ctx = scan_context_of::<()>(Ok(ConnectomeRoots::from_primary("C:/x")));
        assert_eq!(ctx.roots.primary, PathBuf::from("C:/x"));
    }

    #[test]
    fn the_json_error_is_one_text_with_code_and_message() {
        let v = json_error_value("dbsloc: error: E_LDBS_MISSING: nope");
        assert_eq!(v, json!({ "error": "E_LDBS_MISSING: nope" }));
        let v = json_error_value("\ndbsloc: interrupted");
        assert_eq!(v, json!({ "error": "interrupted" }));
    }
}
