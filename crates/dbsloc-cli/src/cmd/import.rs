// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use clap::Args;
use dbsloc_pipeline::bids;
use dbsloc_pipeline::import::{
    self, Acq, ConvertedSeries, ImportError, ImportItem, ImportPlan, ImportReport, LookupTable,
    Session,
};
use serde_json::{json, Value};

use super::{interrupt_flag, output_dir_of};
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Args)]
pub struct ImportArgs {
    /// dataset root (created when missing)
    pub dataset_root: PathBuf,
    /// subject id, with or without sub- (default: the name of the first input folder with every character other than A-Z, a-z, 0-9 removed)
    #[arg(long)]
    pub subject: Option<String>,
    /// output folder of the pipeline that holds sub-<id>/ (default: <dataset_root>/derivatives/leaddbs); used for the Lead-DBS index and the approvals a replacing import resets. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    /// DICOM folder to convert with dcm2niix; its series are classified by the suggestion rules; repeat
    #[arg(long = "dicom", value_name = "FOLDER", action = clap::ArgAction::Append)]
    pub dicom: Vec<PathBuf>,
    /// NIfTI file (.nii or .nii.gz, JSON sidecar beside it) classified by the suggestion rules; repeat
    #[arg(long = "nifti", value_name = "FILE", action = clap::ArgAction::Append)]
    pub nifti: Vec<PathBuf>,
    /// explicit assignment SESSION:SUFFIX[:ACQ]=SOURCE, e.g. preop:T1w=t1.nii.gz, postop:CT=series:4 or postop:T2w:cor=tse.nii; SOURCE series:<number> names a converted DICOM series; repeat
    #[arg(long = "item", value_name = "SPEC", action = clap::ArgAction::Append)]
    pub item: Vec<String>,
    /// import every suggested image and leave the unassigned ones out (default: stop when an image is unassigned)
    #[arg(long)]
    pub auto: bool,
    /// when the DICOM folders hold two or more study dates, suggest the MR series of the latest study as post-op
    #[arg(long)]
    pub postop_latest_study: bool,
    /// do not copy the DICOM folders to sourcedata/sub-<id>/DICOM/
    #[arg(long)]
    pub no_keep_sourcedata: bool,
    /// replace images the subject already has
    #[arg(long)]
    pub replace_existing: bool,
    /// print the plan and write nothing
    #[arg(long)]
    pub dry_run: bool,
    /// print JSON
    #[arg(long)]
    pub json: bool,
}

enum Target {
    Path(PathBuf),
    Series(u32),
}

struct ItemSpec {
    raw: String,
    session: Session,
    suffix: String,
    acq: Option<Acq>,
    target: Target,
}

struct Row {
    item: ImportItem,
    assigned: bool,
    series: Option<ConvertedSeries>,
    folder: Option<PathBuf>,
}

impl Row {
    fn label(&self) -> String {
        match &self.series {
            Some(s) => format!(
                "series {} {:?} ({})",
                s.series_number
                    .map_or_else(|| "?".to_owned(), |n| n.to_string()),
                s.description,
                file_name(&s.nifti)
            ),
            None => self.item.source.display().to_string(),
        }
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn usage(msg: impl std::fmt::Display) -> Fail {
    Fail::new(2, format!("dbsloc: error: {msg}"))
}

fn import_fail(e: &ImportError) -> Fail {
    let code = match e.code {
        import::E_IMPORT_CANCELLED => 130,
        bids::E_BIDS_PARSE | bids::E_NO_ANCHOR | bids::E_DUPL_RAW | bids::E_NO_SUBJ => 2,
        _ => 1,
    };
    Fail::new(code, format!("dbsloc: error: {e}"))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parse_session(raw: &str) -> Option<Session> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "preop" | "pre" => Some(Session::Preop),
        "postop" | "post" => Some(Session::Postop),
        _ => None,
    }
}

fn parse_acq(raw: &str) -> Option<Acq> {
    Acq::ALL
        .into_iter()
        .find(|a| a.as_str().eq_ignore_ascii_case(raw.trim()))
}

fn parse_item(raw: &str) -> Result<ItemSpec, Fail> {
    let bad = |why: &str| {
        usage(format!(
            "--item {raw:?}: {why} (expected SESSION:SUFFIX[:ACQ]=SOURCE, e.g. preop:T1w=t1.nii.gz)"
        ))
    };
    let (head, source) = raw.split_once('=').ok_or_else(|| bad("no '='"))?;
    let parts: Vec<&str> = head.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return Err(bad("SESSION:SUFFIX or SESSION:SUFFIX:ACQ before '='"));
    }
    let session = parse_session(parts[0]).ok_or_else(|| bad("session must be preop or postop"))?;
    let suffix = import::suffix_choices(session)
        .iter()
        .find(|s| s.eq_ignore_ascii_case(parts[1].trim()))
        .ok_or_else(|| {
            bad(&format!(
                "suffix {:?} is not allowed for {} images (choose one of {})",
                parts[1],
                session.as_str(),
                import::suffix_choices(session).join(", ")
            ))
        })?;
    let acq = match parts.get(2) {
        Some(a) => Some(parse_acq(a).ok_or_else(|| bad("ACQ must be ax, cor, sag or iso"))?),
        None => None,
    };
    let source = source.trim();
    if source.is_empty() {
        return Err(bad("no source after '='"));
    }
    let target = match source.strip_prefix("series:") {
        Some(n) => Target::Series(
            n.trim()
                .parse()
                .map_err(|_| bad("series:<number> needs a series number"))?,
        ),
        None => Target::Path(PathBuf::from(source)),
    };
    Ok(ItemSpec {
        raw: raw.to_owned(),
        session,
        suffix: (*suffix).to_owned(),
        acq,
        target,
    })
}

fn sidecar_of(nifti: &Path) -> Option<PathBuf> {
    let name = file_name(nifti);
    let lower = name.to_lowercase();
    let stem_len = if lower.ends_with(".nii.gz") {
        name.len().checked_sub(7)?
    } else if lower.ends_with(".nii") {
        name.len().checked_sub(4)?
    } else {
        return None;
    };
    let stem = name.get(..stem_len)?;
    Some(nifti.with_file_name(format!("{stem}.json"))).filter(|p| p.is_file())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn subject_folder_name(p: &Path) -> Option<String> {
    let dir = if p.is_dir() { Some(p) } else { p.parent() }?;
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let name = file_name(&dir);
    if name.eq_ignore_ascii_case("dicom") {
        dir.parent().map(file_name)
    } else {
        Some(name)
    }
}

fn temp_dir() -> Result<TempDir, Fail> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("dbsloc-import-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| {
        Fail::new(
            1,
            format!("dbsloc: error: cannot create {}: {e}", dir.display()),
        )
    })?;
    Ok(TempDir(dir))
}

fn convert_all(
    a: &ImportArgs,
    table: &LookupTable,
    tmp: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<Row>, Fail> {
    let mut rows = Vec::new();
    for (i, folder) in a.dicom.iter().enumerate() {
        if !folder.is_dir() {
            return Err(usage(format!("--dicom {}: not a folder", folder.display())));
        }
        if !import::looks_like_dicom(folder) {
            return Err(usage(format!(
                "--dicom {}: no DICOM file found below it",
                folder.display()
            )));
        }
        err_line(&format!(
            "dbsloc: converting {} with dcm2niix",
            folder.display()
        ));
        let series = import::convert_dicom_with(folder, &tmp.join(i.to_string()), cancel, table)
            .map_err(|e| import_fail(&e))?;
        err_line(&format!("dbsloc: {} series", series.len()));
        rows.extend(series.into_iter().map(|s| Row {
            item: s.suggested.clone(),
            assigned: s.assigned,
            series: Some(s),
            folder: Some(folder.clone()),
        }));
    }
    Ok(rows)
}

fn apply_items(rows: &mut Vec<Row>, specs: &[ItemSpec]) -> Result<(), Fail> {
    for spec in specs {
        let idx = match &spec.target {
            Target::Series(n) => {
                let hits: Vec<usize> = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.series.as_ref().and_then(|s| s.series_number) == Some(*n))
                    .map(|(i, _)| i)
                    .collect();
                match hits.as_slice() {
                    [i] => Some(*i),
                    [] => {
                        return Err(usage(format!(
                            "--item {:?}: no converted DICOM series has number {n}",
                            spec.raw
                        )))
                    }
                    _ => {
                        return Err(usage(format!(
                        "--item {:?}: {} converted series have number {n}; give the file instead",
                        spec.raw,
                        hits.len()
                    )))
                    }
                }
            }
            Target::Path(p) => {
                if !p.is_file() {
                    return Err(usage(format!(
                        "--item {:?}: {} is not a file",
                        spec.raw,
                        p.display()
                    )));
                }
                rows.iter().position(|r| same_file(&r.item.source, p))
            }
        };
        let (source, sidecar) = match (idx, &spec.target) {
            (Some(i), _) => (rows[i].item.source.clone(), rows[i].item.sidecar.clone()),
            (None, Target::Path(p)) => (p.clone(), sidecar_of(p)),
            (None, Target::Series(_)) => continue,
        };
        let item = ImportItem {
            source,
            sidecar,
            session: spec.session,
            suffix: spec.suffix.clone(),
            acq: spec.acq,
        };
        match idx {
            Some(i) => {
                rows[i].item = item;
                rows[i].assigned = true;
            }
            None => rows.push(Row {
                item,
                assigned: true,
                series: None,
                folder: None,
            }),
        }
    }
    Ok(())
}

fn default_subject(a: &ImportArgs, specs: &[ItemSpec]) -> Result<String, Fail> {
    let first = a
        .dicom
        .first()
        .cloned()
        .or_else(|| a.nifti.first().cloned())
        .or_else(|| {
            specs.iter().find_map(|s| match &s.target {
                Target::Path(p) => Some(p.clone()),
                Target::Series(_) => None,
            })
        })
        .ok_or_else(|| usage("--subject is required"))?;
    let name = subject_folder_name(&first)
        .ok_or_else(|| usage("cannot derive a subject id from the input; pass --subject"))?;
    let (id, warning) = import::subject_id_from_folder(&name);
    if id.is_empty() {
        return Err(usage(format!(
            "folder name {name:?} gives no subject id; pass --subject"
        )));
    }
    if let Some(w) = warning {
        err_line(&format!("dbsloc: warning: {w}"));
    }
    err_line(&format!("dbsloc: subject sub-{id} (from folder {name:?})"));
    Ok(id)
}

fn item_json(item: &ImportItem) -> Value {
    json!({
        "source": item.source.display().to_string(),
        "sidecar": item.sidecar.as_ref().map(|p| p.display().to_string()),
        "session": item.session.as_str(),
        "suffix": item.suffix,
        "acq": item.acq.map(Acq::as_str),
    })
}

fn rows_json(rows: &[Row]) -> Value {
    Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "folder": r.folder.as_ref().map(|p| p.display().to_string()),
                    "file": r.item.source.display().to_string(),
                    "info": r.series.as_ref().map(|s| serde_json::to_value(&s.info).unwrap_or(Value::Null)),
                    "assigned": r.assigned,
                    "item": r.assigned.then(|| item_json(&r.item)),
                })
            })
            .collect(),
    )
}

fn plan_json(plan: &ImportPlan, rows: &[Row], studies: &[import::Study]) -> Value {
    json!({
        "dataset_root": plan.root.display().to_string(),
        "subject": plan.subject_id,
        "subject_folder": plan.subject_folder(),
        "subject_exists": plan.subject_exists,
        "files": plan.files.iter().map(|f| {
            let mut v = plan.items.get(f.item).map(item_json).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                o.insert("destination".into(), json!(f.relative_path));
                o.insert("sidecar_destination".into(), json!(f.sidecar_relative_path));
                o.insert("conflict".into(), json!(f.exists || f.sidecar_exists || f.index_row_exists));
            }
            v
        }).collect::<Vec<_>>(),
        "sourcedata": plan.sourcedata.iter().map(|s| json!({
            "source": s.source.display().to_string(),
            "destination": s.relative_path,
        })).collect::<Vec<_>>(),
        "warnings": plan.warnings,
        "inputs": rows_json(rows),
        "studies": studies,
    })
}

fn print_plan(plan: &ImportPlan, skipped: &[&Row]) {
    out_line(&format!(
        "import plan for {} ({}) in {}",
        plan.subject_folder(),
        if plan.subject_exists {
            "adds to the existing subject"
        } else {
            "new subject"
        },
        plan.root.display()
    ));
    for f in &plan.files {
        let Some(item) = plan.items.get(f.item) else {
            continue;
        };
        let conflict = if f.exists || f.sidecar_exists || f.index_row_exists {
            "  [replaces an existing image]"
        } else {
            ""
        };
        out_line(&format!(
            "  {:<6} {:<7} {:<4} {} -> {}{conflict}",
            item.session.as_str(),
            item.suffix,
            item.acq.map_or("-", Acq::as_str),
            item.source.display(),
            f.relative_path
        ));
    }
    for s in &plan.sourcedata {
        out_line(&format!(
            "  keep DICOM {} -> {}",
            s.source.display(),
            s.relative_path
        ));
    }
    for r in skipped {
        out_line(&format!("  left out (unassigned): {}", r.label()));
    }
    for w in &plan.warnings {
        out_line(&format!("  warning: {w}"));
    }
}

fn report_json(rep: &ImportReport, plan: &ImportPlan) -> Value {
    json!({
        "subject_folder": rep.subject_folder,
        "written": rep.written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "index": rep.index_path.display().to_string(),
        "invalidated": rep.invalidation.changed.iter().map(|s| s.cli_name()).collect::<Vec<_>>(),
        "warnings": plan.warnings.iter().chain(rep.invalidation.warnings.iter()).collect::<Vec<_>>(),
    })
}

pub fn run(a: &ImportArgs) -> CmdResult {
    if a.dicom.is_empty() && a.nifti.is_empty() && a.item.is_empty() {
        return Err(usage("nothing to import: give --dicom, --nifti or --item"));
    }
    let specs = a
        .item
        .iter()
        .map(|s| parse_item(s))
        .collect::<Result<Vec<_>, _>>()?;
    let table = LookupTable::from_env().map_err(|e| {
        usage(format!(
            "{e} (suggestion table from {})",
            import::LOOKUP_ENV
        ))
    })?;
    for n in &a.nifti {
        if !n.is_file() {
            return Err(usage(format!("--nifti {}: not a file", n.display())));
        }
    }
    let subject = match &a.subject {
        Some(s) => s.clone(),
        None => default_subject(a, &specs)?,
    };
    let cancel = interrupt_flag();
    let tmp = temp_dir()?;
    let mut rows = convert_all(a, &table, &tmp.0, &cancel)?;
    if a.postop_latest_study {
        let mut series: Vec<ConvertedSeries> =
            rows.iter().filter_map(|r| r.series.clone()).collect();
        let changed = import::apply_latest_study_rule(&mut series);
        for (k, r) in rows.iter_mut().filter(|r| r.series.is_some()).enumerate() {
            if let Some(s) = series.get(k).filter(|_| changed.contains(&k)) {
                r.item = s.suggested.clone();
                r.series = Some(s.clone());
                err_line(&format!(
                    "dbsloc: {} is in the latest study, suggested as post-op",
                    r.label()
                ));
            }
        }
    }
    let all_series: Vec<ConvertedSeries> = rows.iter().filter_map(|r| r.series.clone()).collect();
    let studies = import::group_studies(&all_series);
    for n in &a.nifti {
        let sidecar = sidecar_of(n);
        let suggestion = import::suggest(n, sidecar.as_deref(), &table);
        rows.push(Row {
            assigned: suggestion.is_some(),
            item: suggestion.unwrap_or_else(|| import::draft_item(n, sidecar.as_deref())),
            series: None,
            folder: None,
        });
    }
    apply_items(&mut rows, &specs)?;

    let unassigned: Vec<&Row> = rows.iter().filter(|r| !r.assigned).collect();
    if !unassigned.is_empty() && !a.auto {
        let list: Vec<String> = unassigned
            .iter()
            .map(|r| format!("  {}", r.label()))
            .collect();
        return Err(usage(format!(
            "{} image(s) match no suggestion rule:\n{}\nassign them with --item SESSION:SUFFIX[:ACQ]=<file or series:N>, or pass --auto to leave them out",
            unassigned.len(),
            list.join("\n")
        )));
    }
    for r in &unassigned {
        err_line(&format!("dbsloc: left out (unassigned): {}", r.label()));
    }
    let items: Vec<ImportItem> = rows
        .iter()
        .filter(|r| r.assigned)
        .map(|r| r.item.clone())
        .collect();
    if items.is_empty() {
        return Err(usage("no image is assigned, nothing to import"));
    }
    let keep: Vec<PathBuf> = if a.no_keep_sourcedata {
        Vec::new()
    } else {
        a.dicom.clone()
    };
    let root = dbsloc_pipeline::paths::absolute(&a.dataset_root);
    let output_dir = output_dir_of(a.output_dir.as_deref());
    let plan = import::plan_import_with(&root, output_dir.as_deref(), &subject, &items, &keep)
        .map_err(|e| import_fail(&e))?;

    if a.dry_run {
        if a.json {
            let mut v = plan_json(&plan, &rows, &studies);
            if let Some(o) = v.as_object_mut() {
                o.insert("dry_run".into(), json!(true));
            }
            out_json(&v);
        } else {
            print_plan(&plan, &unassigned);
        }
        return Ok(0);
    }
    if !a.json {
        print_plan(&plan, &unassigned);
    }
    let rep = import::run_import(&plan, a.replace_existing, &cancel, &mut |p| {
        if !p.current.is_empty() {
            err_line(&format!(
                "dbsloc: import [{}/{}] {}",
                p.done + 1,
                p.total,
                p.current
            ));
        }
    })
    .map_err(|e| import_fail(&e))?;
    if a.json {
        out_json(&report_json(&rep, &plan));
    } else {
        out_line(&format!(
            "imported {} file(s) into {}; index {}",
            rep.written.len(),
            rep.subject_folder,
            rep.index_path.display()
        ));
        if let Some(line) = rep.invalidation.summary("import replaced raw images") {
            out_line(&line);
        }
        for w in &rep.invalidation.warnings {
            err_line(&format!("dbsloc: warning: {w}"));
        }
    }
    Ok(0)
}
