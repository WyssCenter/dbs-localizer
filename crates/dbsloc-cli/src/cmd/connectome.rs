// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use clap::{Args, Subcommand};
use dbsloc_pipeline::connectome_cache::{self, CacheTarget, DeleteResult};
use dbsloc_pipeline::connectome_catalog::{
    catalogue, lookup, Access, ConnectomeClass, ConnectomeEntry, Step,
};
use dbsloc_pipeline::connectome_fetch::{
    check_acceptance, discover_connectomes, fetch_connectome, forget_folder, import_archive,
    import_folder, import_only_error, infer_class, licence_record_is_stale, list_catalogue,
    probe_remote, read_dataset_info, read_licence_record, Acceptance, ConnectomeError,
    ConnectomeEvent, ConnectomeRoots, ConnectomeState, ConnectomeStatus, ErrorCode, FetchSettings,
    InstalledConnectome, RootKind, INSTALL_RECORD,
};
use dbsloc_pipeline::connectome_maps::job::resolve_connectome;
use dbsloc_pipeline::connectome_maps::source::cache_target;
use dbsloc_pipeline::connectome_maps::{MapError, Progress};
use serde_json::{json, Value};

use super::interrupt_flag;
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum ConnectomeCmd {
    /// list the downloadable connectomes and every connectome folder found in the connectome roots
    List(ListArgs),
    /// show what is known about a catalogue id, an installed name or a folder
    Info(InfoArgs),
    /// print the full licence or data use terms of a catalogue connectome, verbatim, with the required acknowledgement
    #[command(alias = "license")]
    Licence(LicenceArgs),
    /// download a connectome from its original host after you accept its terms (verified; resumable)
    Fetch(FetchArgs),
    /// use a connectome you obtained yourself: register a folder in place or extract an archive
    Import(ImportArgs),
    /// stop listing a folder that was registered with import (the folder itself is not touched)
    Forget(ForgetArgs),
    /// compute the structural or functional seed map of a stimulation for one or more subjects
    Map(super::connectome_maps::MapArgs),
    /// compute the per-fibre values of a stimulation (input of fibre filtering)
    Fibervals(super::connectome_maps::FibervalsArgs),
    /// build or clear the optional fibre cache of a connectome
    Cache {
        #[command(subcommand)]
        cmd: CacheCmd,
    },
}

#[derive(Subcommand)]
pub enum CacheCmd {
    /// list the caches on this computer with their size and last use
    List(CacheListArgs),
    /// build the cache of a connectome and record that you allow it (about 14 bytes per fibre point of extra disk space)
    Build(CacheArgs),
    /// delete the cache of a connectome (or every cache with --all); the next use asks again
    Clear(CacheClearArgs),
}

#[derive(Args)]
pub struct CacheArgs {
    /// catalogue id, installed name or folder
    pub target: String,
    #[command(flatten)]
    pub root: RootArg,
}

#[derive(Args)]
pub struct CacheClearArgs {
    /// catalogue id, installed name or folder
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub target: Option<String>,
    /// delete every cache
    #[arg(long)]
    pub all: bool,
    #[command(flatten)]
    pub root: RootArg,
}

#[derive(Args)]
pub struct CacheListArgs {
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct RootArg {
    /// primary connectomes folder (default: DBSLOC_CONNECTOMES_DIR, else the per-user data folder)
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,
}

#[derive(Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub root: RootArg,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct InfoArgs {
    /// catalogue id, installed name or folder path
    pub target: String,
    /// ask the original host once and compare size and checksum with the catalogue pins
    #[arg(long)]
    pub remote: bool,
    #[command(flatten)]
    pub root: RootArg,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct LicenceArgs {
    /// catalogue id
    pub connectome: String,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct FetchArgs {
    /// catalogue id
    pub connectome: String,
    /// accept the licence or data use terms shown by this command
    #[arg(long)]
    pub accept_terms: bool,
    /// attest an approved data use agreement (import-only entries are never downloaded; use import)
    #[arg(long)]
    pub attest_data_use_agreement: bool,
    /// cap the download rate in MB/s
    #[arg(long, value_name = "MB/S")]
    pub max_rate: Option<f64>,
    #[command(flatten)]
    pub root: RootArg,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ImportArgs {
    /// connectome folder (registered in place) or a .zip / .tar / .tar.gz archive (extracted)
    pub source: PathBuf,
    /// connectome class when it cannot be told from the files
    #[arg(long, value_name = "CLASS", value_parser = parse_class)]
    pub class: Option<ConnectomeClass>,
    /// record acceptance of the terms of a matching catalogue connectome
    #[arg(long)]
    pub accept_terms: bool,
    /// attest an approved data use agreement (needed with --accept-terms for dua entries)
    #[arg(long)]
    pub attest_data_use_agreement: bool,
    #[command(flatten)]
    pub root: RootArg,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ForgetArgs {
    /// folder registered with import
    pub path: PathBuf,
    #[command(flatten)]
    pub root: RootArg,
}

fn parse_class(text: &str) -> Result<ConnectomeClass, String> {
    ConnectomeClass::parse(text)
        .ok_or_else(|| format!("'{text}': expected dMRI, fMRI or dMRI_MultiTract"))
}

pub fn run(cmd: ConnectomeCmd) -> CmdResult {
    match cmd {
        ConnectomeCmd::List(a) => list(&a),
        ConnectomeCmd::Info(a) => info(&a),
        ConnectomeCmd::Licence(a) => licence(&a),
        ConnectomeCmd::Fetch(a) => fetch(&a),
        ConnectomeCmd::Import(a) => import(&a),
        ConnectomeCmd::Forget(a) => forget(&a),
        ConnectomeCmd::Map(a) => super::connectome_maps::map(&a),
        ConnectomeCmd::Fibervals(a) => super::connectome_maps::fibervals(&a),
        ConnectomeCmd::Cache { cmd } => match cmd {
            CacheCmd::List(a) => cache_list(&a),
            CacheCmd::Build(a) => cache_build(&a),
            CacheCmd::Clear(a) => cache_clear(&a),
        },
    }
}

fn map_fail(e: &MapError) -> Fail {
    match e {
        MapError::Cancelled => Fail::new(
            130,
            "
dbsloc: interrupted",
        ),
        MapError::Usage(_) => Fail::new(2, format!("dbsloc: error: {e}")),
        _ => Fail::new(1, format!("dbsloc: error: {e}")),
    }
}

fn cache_target_of(spec: &str, root: &RootArg) -> Result<CacheTarget, Fail> {
    let roots = roots_of(root)?;
    let conn = resolve_connectome(&roots, spec).map_err(|e| map_fail(&e))?;
    if conn.class != ConnectomeClass::Dmri {
        return Err(Fail::new(
            2,
            format!(
                "dbsloc: error: {} is not a structural dMRI connectome; only those have a cache",
                conn.name
            ),
        ));
    }
    Ok(cache_target(&conn))
}

fn cache_build(a: &CacheArgs) -> CmdResult {
    let target = cache_target_of(&a.target, &a.root)?;
    let estimate = connectome_cache::estimate(&target).map_err(|e| map_fail(&e))?;
    if !estimate.rule_ok() {
        return Err(Fail::new(
            1,
            format!(
                "dbsloc: error: not enough free disk space for the cache of {}: about {} needed twice over plus 5 GB, {} free",
                target.name,
                gb(estimate.estimated_bytes),
                gb(estimate.free_bytes.unwrap_or(0))
            ),
        ));
    }
    let cancel = interrupt_flag();
    let progress = |p: &Progress| err_line(&format!("dbsloc: cache: {}", p.message));
    let (report, _) = connectome_cache::build_and_allow(&target, Some(&cancel), Some(&progress))
        .map_err(|e| map_fail(&e))?;
    out_line(&format!(
        "dbsloc: cache for {} built: {} on disk, {} fibres, {} points, {:.1} s",
        target.name,
        gb(report.bytes),
        report.fibres,
        report.points,
        report.seconds
    ));
    Ok(0)
}

fn report_delete(results: &[DeleteResult]) -> i32 {
    let mut code = 0;
    for r in results {
        match r {
            DeleteResult::Deleted { key, freed_bytes } => {
                out_line(&format!(
                    "dbsloc: cache {key} deleted ({})",
                    gb(*freed_bytes)
                ));
            }
            DeleteResult::InUse { key } => {
                err_line(&format!(
                    "dbsloc: warning: cache {key} is in use; left in place"
                ));
                code = 1;
            }
            DeleteResult::Failed { key, message } => {
                err_line(&format!("dbsloc: error: cache {key}: {message}"));
                code = 1;
            }
        }
    }
    code
}

fn cache_clear(a: &CacheClearArgs) -> CmdResult {
    let keys: Vec<String> = if a.all {
        connectome_cache::list_caches()
            .into_iter()
            .map(|c| c.key)
            .collect()
    } else {
        let spec = a.target.as_deref().unwrap_or_default();
        vec![cache_target_of(spec, &a.root)?.key()]
    };
    if keys.is_empty() {
        out_line("dbsloc: no caches");
        return Ok(0);
    }
    let results = connectome_cache::delete_caches(&keys);
    Ok(report_delete(&results))
}

fn cache_list(a: &CacheListArgs) -> CmdResult {
    let caches = connectome_cache::list_caches();
    let declined = connectome_cache::declined();
    let secs = |t: Option<std::time::SystemTime>| {
        t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    };
    if a.json {
        out_json(&json!({
            "cache_dir": connectome_cache::connectomes_cache_root().map(|p| p.display().to_string()),
            "setting": connectome_cache::CacheSetting::from_env().name(),
            "caches": caches.iter().map(|c| json!({
                "key": c.key,
                "name": c.name,
                "source_path": c.source_path,
                "size_bytes": c.size_bytes,
                "last_used_unix": secs(c.last_used),
                "orphaned": c.orphaned,
                "in_use": c.in_use,
            })).collect::<Vec<_>>(),
            "declined": declined.iter().map(|d| json!({
                "key": d.key, "name": d.name, "decided_at": d.decided_at,
                "estimated_bytes": d.estimated_bytes,
            })).collect::<Vec<_>>(),
        }));
        return Ok(0);
    }
    if caches.is_empty() && declined.is_empty() {
        out_line("dbsloc: no caches");
    }
    for c in &caches {
        out_line(&format!(
            "{}	{}	{}{}",
            c.key,
            gb(c.size_bytes),
            c.source_path,
            if c.orphaned { "	(source gone)" } else { "" }
        ));
    }
    for d in &declined {
        out_line(&format!("declined	{}	{}", d.key, d.name));
    }
    Ok(0)
}

fn fail_of(e: &ConnectomeError) -> Fail {
    match e.code {
        ErrorCode::Cancelled => Fail::new(130, "\ndbsloc: interrupted"),
        ErrorCode::Terms | ErrorCode::Usage => Fail::new(2, format!("dbsloc: error: {e}")),
        _ => Fail::new(1, format!("\ndbsloc: error: {e}")),
    }
}

fn roots_of(arg: &RootArg) -> Result<ConnectomeRoots, Fail> {
    ConnectomeRoots::resolve(arg.root.as_deref()).map_err(|e| fail_of(&e))
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn installed_json(c: &InstalledConnectome) -> Value {
    json!({
        "id": c.id,
        "name": c.name,
        "class": c.class.dir_name(),
        "kind": c.class.kind_label(),
        "source": if c.catalogue_id.is_some() { "catalogue" } else { "discovered" },
        "state": if c.missing { "missing" } else { "installed" },
        "root_kind": c.root_kind.label(),
        "path": c.path.display().to_string(),
        "tag": c.tag,
        "space": c.space,
        "version": c.version,
        "subsets": c.subsets,
        "type": c.fmri_type,
        "duplicate_of": c.duplicate_of.as_ref().map(|p| p.display().to_string()),
        "licence_record": c.licence_record.as_ref().map(|p| p.display().to_string()),
    })
}

fn catalogue_json(s: &ConnectomeStatus, installed: &[InstalledConnectome]) -> Value {
    let e = s.entry;
    let mut v = json!({
        "id": e.id,
        "name": e.name,
        "class": e.class.dir_name(),
        "kind": e.class.kind_label(),
        "source": "catalogue",
        "state": s.state.label(),
        "access": e.access.label(),
        "space": e.space,
        "cohort": e.cohort,
        "version": e.version,
        "size": e.total_bytes(),
        "files": e.files.len(),
        "licence_name": e.licence_name,
        "licence_record": s.licence_record.as_ref().map(|p| p.display().to_string()),
        "path": Value::Null,
        "root_kind": Value::Null,
        "percent": Value::Null,
    });
    match &s.state {
        ConnectomeState::Installed(p) => {
            v["path"] = json!(p.display().to_string());
            v["root_kind"] = installed
                .iter()
                .find(|c| &c.path == p)
                .map_or(Value::Null, |c| json!(c.root_kind.label()));
        }
        ConnectomeState::Partial { percent, .. } => v["percent"] = json!(percent),
        ConnectomeState::Available { .. } => {}
    }
    v
}

pub fn rows_json(statuses: &[ConnectomeStatus], installed: &[InstalledConnectome]) -> Value {
    let used: Vec<&Path> = statuses
        .iter()
        .filter_map(|s| match &s.state {
            ConnectomeState::Installed(p) => Some(p.as_path()),
            _ => None,
        })
        .collect();
    let mut rows: Vec<Value> = statuses
        .iter()
        .map(|s| catalogue_json(s, installed))
        .collect();
    rows.extend(
        installed
            .iter()
            .filter(|c| !used.contains(&c.path.as_path()))
            .map(installed_json),
    );
    Value::Array(rows)
}

fn state_text(row: &Value) -> String {
    match row["state"].as_str().unwrap_or("") {
        "partial" => format!("partial {}%", row["percent"].as_u64().unwrap_or(0)),
        s => s.to_owned(),
    }
}

pub fn table_lines(rows: &Value) -> Vec<String> {
    let list = rows.as_array().cloned().unwrap_or_default();
    let cell = |r: &Value, k: &str| r[k].as_str().unwrap_or("").to_owned();
    let idw = list
        .iter()
        .map(|r| cell(r, "id").len())
        .max()
        .unwrap_or(2)
        .max(2);
    let mut lines = vec![format!(
        "{:<idw$}  {:<10}  {:<12}  {:<14}  {:<9}  name / path",
        "id", "class", "access", "state", "size"
    )];
    for r in &list {
        let size = r["size"].as_u64().map_or_else(String::new, gb);
        let access = r["access"].as_str().unwrap_or("-");
        let tail = match r["path"].as_str() {
            Some(p) => format!("{}  [{}] {p}", cell(r, "name"), cell(r, "root_kind")),
            None => cell(r, "name"),
        };
        lines.push(format!(
            "{:<idw$}  {:<10}  {:<12}  {:<14}  {:<9}  {tail}",
            cell(r, "id"),
            cell(r, "class"),
            access,
            state_text(r),
            size
        ));
    }
    lines
}

fn list(a: &ListArgs) -> CmdResult {
    let roots = roots_of(&a.root)?;
    let installed = discover_connectomes(&roots);
    let statuses = list_catalogue(&roots, &installed);
    let rows = rows_json(&statuses, &installed);
    if a.json {
        out_json(&rows);
    } else {
        for l in table_lines(&rows) {
            out_line(&l);
        }
    }
    Ok(0)
}

fn step_text(s: &Step) -> String {
    match s.url {
        Some(url) => format!("{} Link: {url}", s.text),
        None => s.text.to_owned(),
    }
}

fn steps_json(e: &ConnectomeEntry) -> Value {
    Value::Array(
        e.steps
            .iter()
            .map(|s| json!({"text": s.text, "url": s.url}))
            .collect(),
    )
}

fn steps_lines(e: &ConnectomeEntry) -> Vec<String> {
    e.steps
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, step_text(s)))
        .collect()
}

fn entry_facts(e: &ConnectomeEntry) -> Vec<(String, String)> {
    let mut f = vec![
        ("id".into(), e.id.into()),
        ("name".into(), e.name.into()),
        ("class".into(), e.class.dir_name().into()),
        ("space".into(), e.space.into()),
        ("cohort".into(), e.cohort.into()),
        ("version".into(), e.version.into()),
        (
            "size".into(),
            format!("{} in {} file(s)", gb(e.total_bytes()), e.files.len()),
        ),
        ("access".into(), e.access.label().into()),
        (
            "licence".into(),
            format!("{} ({})", e.licence_name, e.licence_url),
        ),
        ("terms fetched".into(), e.terms_fetched.into()),
        ("dataset website".into(), e.dataset_url.into()),
        ("citation".into(), e.citation.into()),
        ("instructions".into(), e.instructions.into()),
    ];
    for (i, s) in e.steps.iter().enumerate() {
        f.push((format!("step {}", i + 1), step_text(s)));
    }
    if let Some(t) = e.tag {
        f.push(("tag".into(), t.into()));
    }
    f
}

fn folder_facts(c: &InstalledConnectome) -> Vec<(String, String)> {
    let mut f = vec![
        ("name".into(), c.name.clone()),
        ("id".into(), c.id.clone()),
        ("class".into(), c.class.dir_name().into()),
        ("path".into(), c.path.display().to_string()),
        ("root".into(), c.root_kind.label().into()),
        ("space".into(), c.space.clone()),
    ];
    if c.missing {
        f.push(("state".into(), "missing".into()));
    }
    if let Some(t) = &c.tag {
        f.push(("tag".into(), t.clone()));
    }
    if let Some(t) = &c.fmri_type {
        f.push(("type".into(), t.clone()));
    }
    if !c.subsets.is_empty() {
        f.push(("subsets".into(), c.subsets.join(", ")));
    }
    if let Some(v) = &c.version {
        f.push(("version".into(), v.clone()));
    }
    if let Some(d) = &c.duplicate_of {
        f.push(("shadowed by".into(), d.display().to_string()));
    }
    if let Some(e) = c.catalogue_id.and_then(lookup) {
        f.push(("catalogue entry".into(), e.id.into()));
    }
    f
}

fn find_installed(roots: &ConnectomeRoots, target: &str) -> Option<InstalledConnectome> {
    let found = discover_connectomes(roots);
    let as_path = std::fs::canonicalize(target).ok();
    found
        .iter()
        .find(|c| as_path.as_ref().is_some_and(|p| &c.path == p))
        .or_else(|| found.iter().find(|c| c.id == target))
        .or_else(|| found.iter().find(|c| c.name == target))
        .cloned()
}

fn print_facts(facts: &[(String, String)]) {
    let w = facts.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (k, v) in facts {
        out_line(&format!("{k:<w$}  {v}"));
    }
}

fn licence(a: &LicenceArgs) -> CmdResult {
    let id = a.connectome.trim();
    let e = lookup(id).ok_or_else(|| {
        Fail::new(
            1,
            format!(
                "dbsloc: error: {}: {id} is not a catalogue id (see dbsloc connectome list)",
                ErrorCode::Missing.code()
            ),
        )
    })?;
    if a.json {
        out_json(&json!({
            "id": e.id,
            "name": e.name,
            "access": e.access.label(),
            "licence_name": e.licence_name,
            "licence_url": e.licence_url,
            "terms_fetched": e.terms_fetched,
            "licence_text": e.licence_text,
            "licence_text_sha256": dbsloc_pipeline::connectome_fetch::licence_text_sha256(e),
            "acknowledgement": e.acknowledgement,
            "citation": e.citation,
            "steps": steps_json(e),
        }));
    } else {
        out_line(&format!("{} ({})", e.name, e.id));
        out_line(&format!("Licence: {}", e.licence_name));
        out_line(&format!("Source: {}", e.licence_url));
        out_line(&format!("Terms fetched: {}", e.terms_fetched));
        out_line("");
        out_line(e.licence_text.trim_end());
        out_line("");
        out_line("Required acknowledgement:");
        out_line(e.acknowledgement);
        out_line("");
        out_line(&format!("Citation: {}", e.citation));
        if !e.steps.is_empty() {
            out_line("");
            out_line("How to get this dataset (import only):");
            for l in steps_lines(e) {
                out_line(&l);
            }
        }
    }
    Ok(0)
}

fn info(a: &InfoArgs) -> CmdResult {
    let roots = roots_of(&a.root)?;
    let target = a.target.trim();
    let installed = discover_connectomes(&roots);
    let entry = lookup(target);
    let local = find_installed(&roots, target).or_else(|| {
        let p = Path::new(target);
        let class = infer_class(p)?;
        let name = p.file_name()?.to_string_lossy().into_owned();
        let abs = std::fs::canonicalize(p).ok()?;
        Some(InstalledConnectome {
            id: name.clone(),
            tag: read_dataset_info(&abs).and_then(|v| v["tag"].as_str().map(str::to_owned)),
            name,
            class,
            path: abs,
            root_kind: RootKind::Registered,
            catalogue_id: None,
            space: dbsloc_pipeline::connectome_catalog::REFERENCE_SPACE.to_owned(),
            version: None,
            subsets: Vec::new(),
            fmri_type: None,
            missing: false,
            duplicate_of: None,
            licence_record: None,
        })
    });
    if entry.is_none() && local.is_none() {
        return Err(Fail::new(
            1,
            format!(
                "dbsloc: error: {}: {target} is neither a catalogue id, an installed connectome nor a connectome folder",
                ErrorCode::Missing.code()
            ),
        ));
    }
    let entry = entry.or_else(|| local.as_ref().and_then(|c| c.catalogue_id).and_then(lookup));
    let mut facts = Vec::new();
    let mut doc = json!({});
    if let Some(e) = entry {
        facts.extend(entry_facts(e));
        let state = list_catalogue(&roots, &installed)
            .into_iter()
            .find(|s| s.entry.id == e.id);
        if let Some(s) = &state {
            facts.push(("state".into(), s.state.label().into()));
            doc["state"] = json!(s.state.label());
        }
        match read_licence_record(&roots.primary, e.id) {
            Some(rec) => {
                let stale = licence_record_is_stale(&rec, e);
                facts.push((
                    "licence record".into(),
                    format!(
                        "accepted {} via {}{}",
                        rec["accepted_at"].as_str().unwrap_or("?"),
                        rec["accepted_via"].as_str().unwrap_or("?"),
                        if stale {
                            " (stale: the licence text changed; read it with dbsloc connectome licence and accept again)"
                        } else {
                            ""
                        }
                    ),
                ));
                doc["licence_record"] = rec;
            }
            None => facts.push(("licence record".into(), "none".into())),
        }
        doc["entry"] = json!({
            "id": e.id, "name": e.name, "class": e.class.dir_name(), "access": e.access.label(),
            "space": e.space, "size": e.total_bytes(), "version": e.version,
            "citation": e.citation, "licence_url": e.licence_url, "dataset_url": e.dataset_url,
            "steps": steps_json(e),
        });
    }
    if let Some(c) = &local {
        facts.extend(folder_facts(c));
        doc["installed"] = installed_json(c);
        if let Ok(text) = std::fs::read_to_string(c.path.join(INSTALL_RECORD)) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                facts.push((
                    "installed".into(),
                    format!(
                        "{} by {}",
                        v["installed_at"].as_str().unwrap_or("?"),
                        v["installed_by"].as_str().unwrap_or("?")
                    ),
                ));
                doc["install_record"] = v;
            }
        }
    }
    if a.remote {
        let e =
            entry.ok_or_else(|| Fail::new(2, "dbsloc: error: --remote needs a catalogue id"))?;
        let rows = probe_remote(e, &FetchSettings::default()).map_err(|x| fail_of(&x))?;
        let bad = rows.iter().filter(|r| !r.matches).count();
        facts.push((
            "remote".into(),
            if bad == 0 {
                format!("{} file(s) match the catalogue pins", rows.len())
            } else {
                format!(
                    "{bad} of {} file(s) differ from the catalogue pins",
                    rows.len()
                )
            },
        ));
        doc["remote"] = Value::Array(
            rows.iter()
                .map(|r| json!({"file_name": r.file_name, "bytes": r.bytes, "digest": r.digest, "matches": r.matches}))
                .collect(),
        );
    }
    if a.json {
        out_json(&doc);
    } else {
        print_facts(&facts);
    }
    Ok(0)
}

fn refusal(entry: &ConnectomeEntry) -> Fail {
    let mut text = String::new();
    text.push_str(&format!("{} ({})\n", entry.name, entry.id));
    text.push_str(&format!(
        "Licence: {} ({})\n\n",
        entry.licence_name, entry.licence_url
    ));
    text.push_str(entry.licence_text);
    text.push_str(&format!("\nCitation: {}\n", entry.citation));
    text.push_str(&format!("Acknowledgement: {}\n\n", entry.acknowledgement));
    text.push_str("dbsloc: error: pass --accept-terms to accept these terms");
    Fail::new(2, text)
}

fn import_only_refusal(entry: &ConnectomeEntry) -> Fail {
    let mut text = String::new();
    text.push_str(&format!("{} ({})\n", entry.name, entry.id));
    text.push_str(&format!("Citation: {}\n", entry.citation));
    text.push_str(&format!("Dataset website: {}\n\n", entry.dataset_url));
    text.push_str("How to get this dataset:\n");
    for l in steps_lines(entry) {
        text.push_str(&l);
        text.push('\n');
    }
    text.push_str(&format!(
        "\nAfter you have the archive, import it with: dbsloc connectome import <archive> --accept-terms --attest-data-use-agreement\ndbsloc: error: {}",
        import_only_error(entry)
    ));
    Fail::new(2, text)
}

fn fetch(a: &FetchArgs) -> CmdResult {
    let key = a.connectome.trim();
    let Some(entry) = lookup(key) else {
        let valid: Vec<&str> = catalogue().iter().map(|e| e.id).collect();
        return Err(Fail::new(
            2,
            format!(
                "dbsloc: error: unknown connectome '{key}'; valid ids: {}",
                valid.join(", ")
            ),
        ));
    };
    if entry.access == Access::Dua {
        return Err(import_only_refusal(entry));
    }
    let acceptance = Acceptance {
        accept_terms: a.accept_terms,
        attest_data_use_agreement: a.attest_data_use_agreement,
        via: "cli",
    };
    if check_acceptance(entry, &acceptance).is_err() {
        return Err(refusal(entry));
    }
    let roots = roots_of(&a.root)?;
    let mut settings = FetchSettings::default();
    if let Some(r) = a.max_rate {
        if !(r.is_finite() && r > 0.0) {
            return Err(Fail::new(
                2,
                "dbsloc: error: --max-rate must be a positive number",
            ));
        }
        settings.max_rate_bytes_per_sec = Some((r * 1e6) as u64);
    }
    let cancel = interrupt_flag();
    let id = entry.id;
    let result = fetch_connectome(
        entry,
        &roots,
        &acceptance,
        &settings,
        &cancel,
        &mut |ev| match ev {
            ConnectomeEvent::Progress { downloaded, total } => {
                let mut e = std::io::stderr().lock();
                let _ = write!(
                    e,
                    "\rdownloading {id}: {:.0} / {:.0} MB",
                    downloaded as f64 / 1e6,
                    total as f64 / 1e6
                );
                let _ = e.flush();
            }
            ConnectomeEvent::FileStarted {
                index,
                count,
                file_name,
            } if count > 1 => {
                err_line(&format!("\nfile {index} / {count}: {file_name}"));
            }
            ConnectomeEvent::Retry {
                attempt,
                wait_secs,
                reason,
            } => {
                err_line(&format!("\nretry {attempt} in {wait_secs} s: {reason}"));
            }
            ConnectomeEvent::Verifying => err_line("\nverifying checksum"),
            ConnectomeEvent::Extracting => err_line("extracting"),
            ConnectomeEvent::Started { .. }
            | ConnectomeEvent::FileStarted { .. }
            | ConnectomeEvent::Done { .. } => {}
        },
    );
    match result {
        Ok(out) => {
            if a.json {
                out_json(&json!({
                    "id": entry.id,
                    "path": out.path.display().to_string(),
                    "folder": out.folder,
                    "licence_record": out.licence_record.display().to_string(),
                    "sha256": out.files.first().map(|f| f.sha256.clone()),
                    "files": out.files,
                }));
            } else {
                out_line(&out.path.display().to_string());
                for f in &out.files {
                    err_line(&format!("sha256 {}  {}", f.sha256, f.file_name));
                }
            }
            Ok(0)
        }
        Err(e) if cancel.load(Ordering::SeqCst) || e.is_cancelled() => {
            Err(Fail::new(130, "\ndbsloc: interrupted"))
        }
        Err(e) => Err(fail_of(&e)),
    }
}

fn import(a: &ImportArgs) -> CmdResult {
    let roots = roots_of(&a.root)?;
    let acceptance = (a.accept_terms || a.attest_data_use_agreement).then_some(Acceptance {
        accept_terms: a.accept_terms,
        attest_data_use_agreement: a.attest_data_use_agreement,
        via: "import",
    });
    let cancel = interrupt_flag();
    let result = if a.source.is_dir() {
        import_folder(&roots, &a.source, a.class, acceptance.as_ref())
    } else if a.source.is_file() {
        import_archive(&roots, &a.source, a.class, acceptance.as_ref(), &cancel)
    } else {
        Err(ConnectomeError::new(
            ErrorCode::Missing,
            format!("{} does not exist", a.source.display()),
        ))
    };
    let out = result.map_err(|e| fail_of(&e))?;
    if let Some(note) = &out.licence_note {
        err_line(&format!("dbsloc: note: {note}"));
    }
    if a.json {
        out_json(&json!({
            "id": out.id,
            "path": out.path.display().to_string(),
            "class": out.class.dir_name(),
            "registered": out.registered,
            "extracted": out.extracted,
            "catalogue_id": out.catalogue_id,
            "licence_record": out.licence_record.as_ref().map(|p| p.display().to_string()),
            "verified": out.verified,
            "verification_note": out.verification_note(),
        }));
    } else {
        let verb = if out.extracted {
            "extracted"
        } else {
            "registered"
        };
        out_line(&format!(
            "{verb} {} ({}, id {})",
            out.path.display(),
            out.class.dir_name(),
            out.id
        ));
        if let Some(note) = out.verification_note() {
            out_line(&format!("archive: {note}"));
        }
    }
    Ok(0)
}

fn forget(a: &ForgetArgs) -> CmdResult {
    let roots = roots_of(&a.root)?;
    let gone = forget_folder(&roots.primary, &a.path).map_err(|e| fail_of(&e))?;
    out_line(&format!("forgot {}", gone.path));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_header_and_one_line_per_row() {
        let roots =
            ConnectomeRoots::from_primary(std::env::temp_dir().join("dbsloc_no_such_connectomes"));
        let installed = discover_connectomes(&roots);
        let statuses = list_catalogue(&roots, &installed);
        let rows = rows_json(&statuses, &installed);
        let t = table_lines(&rows);
        assert_eq!(t.len(), catalogue().len() + 1);
        assert!(t[0].starts_with("id"));
        assert!(t.iter().any(|l| l.contains("dua")));
    }

    #[test]
    fn class_parser_accepts_the_three_names() {
        assert_eq!(parse_class("fMRI"), Ok(ConnectomeClass::Fmri));
        assert!(parse_class("x").is_err());
    }
}
