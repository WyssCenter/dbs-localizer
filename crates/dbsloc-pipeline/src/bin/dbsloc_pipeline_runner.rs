// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Subprocess entry point for the Wyss DBS Localizer pipeline runner.
//!
//! Protocol: reads one JSON ``RunOptions`` object from **stdin** (up to the first newline or EOF),
//! runs the requested stages, writes ``RunEvent`` NDJSON lines to **stdout**.  While it runs,
//! further stdin lines are read: the line `cancel` stops the running tools and skips the remaining
//! stages; EOF is not a cancel.  Exit code 0 = all stages ok; non-zero = one or more stages failed.
//!
//! A first line `{"group_job": {"kind": …, "group_file": …, "options": {…}}}` instead of
//! `RunOptions` runs one cohort analysis of the group record (`group_calc`, `group_stats`,
//! `sweetspot`, `fibrefilter`, `netmap`, `export`; `docs/SERVER_CLIENT_PLAN.md` §12) with the same
//! events and cancel line; the exit code is 0 on success, 1 when a subject or the analysis failed,
//! 130 when cancelled.
//!
//! Designed to be called from ``dbsloc-app`` (Rust GUI) or any subprocess wrapper.

use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use dbsloc_pipeline::protocol::RunEvent;
use dbsloc_pipeline::{options::RunOptions, run_pipeline_channel};

fn main() {
    if let Some(note) = dbsloc_pipeline::legacy_env::legacy_env_note(std::env::vars()) {
        eprintln!("{note}");
    }
    let mut input = String::new();
    if io::stdin().lock().read_line(&mut input).is_err() {
        eprintln!("dbsloc-pipeline-runner: failed to read stdin");
        std::process::exit(2);
    }
    let group_spec = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v.get("group_job").cloned());
    let opts = match group_spec {
        Some(_) => None,
        None => match RunOptions::from_runner_json(&input, &|name| std::env::var(name).ok()) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("dbsloc-pipeline-runner: {e}");
                std::process::exit(2);
            }
        },
    };

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let stdin = io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match stdin.lock().read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if line.trim() == "cancel" {
                            cancel.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
        });
    }

    let Some(opts) = opts else {
        let spec = group_spec.unwrap_or_default();
        std::process::exit(group_job::run(&spec, cancel.as_ref()));
    };

    let (tx, rx) = mpsc::channel::<RunEvent>();
    let writer = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let mut closed = false;
            for ev in rx {
                if !closed && dbsloc_protocol::protocol::try_emit(&ev).is_err() {
                    closed = true;
                    cancel.store(true, Ordering::SeqCst);
                }
            }
        })
    };
    let failed = run_pipeline_channel(&opts, &tx, Some(cancel.as_ref()));
    drop(tx);
    let _ = writer.join();
    if failed > 0 {
        std::process::exit(1);
    }
}

mod group_job {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    use dbsloc_pipeline::atlas_catalog::external_atlases_dir_from_env;
    use dbsloc_pipeline::connectome_maps::Progress;
    use dbsloc_pipeline::group::calc::{
        run_calculate_job, run_contact_distances, CalcEvent, CalcOptions, CalcPhase, CalcReport,
        FlipTransformChoice,
    };
    use dbsloc_pipeline::group::clinical::write_clinical_export;
    use dbsloc_pipeline::group::exports::{write_stats_exports, ExportOptions};
    use dbsloc_pipeline::group::fibrefilter::{
        run_fibrefilter, AnalysisError, CvScheme, FibreRunOptions, FibreSettings,
    };
    use dbsloc_pipeline::group::netmap::{
        run_netmap, MaskChoice, NetmapRunOptions, NetmapSettings,
    };
    use dbsloc_pipeline::group::record::save_record;
    use dbsloc_pipeline::group::sweetspot::{
        run_sweetspot, Preset, SweetspotOptions, SweetspotSettings,
    };
    use dbsloc_pipeline::group::{open_group, save_group, GroupError, Note, Opened};
    use dbsloc_pipeline::options::{RunOptions, VatBackend};
    use dbsloc_pipeline::protocol::RunEvent;
    use serde::de::DeserializeOwned;
    use serde::Serialize;
    use serde_json::{json, Value};

    enum Failure {
        Cancelled,
        Failed(String),
    }

    type Done = Result<Value, Failure>;

    fn emit(ev: &RunEvent, cancel: &AtomicBool) {
        if dbsloc_protocol::protocol::try_emit(ev).is_err() {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    fn log(line: String, cancel: &AtomicBool) {
        emit(&RunEvent::LogLine { line }, cancel);
    }

    fn group_failure(e: &GroupError) -> Failure {
        match e {
            GroupError::Lg {
                code: "cancelled", ..
            } => Failure::Cancelled,
            other => Failure::Failed(other.to_string()),
        }
    }

    fn analysis_failure(e: &AnalysisError) -> Failure {
        match e {
            AnalysisError::Cancelled => Failure::Cancelled,
            other => Failure::Failed(other.to_string()),
        }
    }

    fn warn_notes(notes: &[Note], cancel: &AtomicBool) {
        for n in notes {
            log(format!("warning: {n}"), cancel);
        }
    }

    fn rel(folder: &Path, p: &Path) -> String {
        p.strip_prefix(folder)
            .unwrap_or_else(|_| Path::new(p.file_name().unwrap_or_default()))
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn rels(folder: &Path, files: &[PathBuf]) -> Vec<String> {
        files.iter().map(|f| rel(folder, f)).collect()
    }

    pub(super) fn merged<T: Serialize + DeserializeOwned + Default>(
        overlay: Option<&Value>,
    ) -> Result<T, String> {
        merged_over(T::default(), overlay)
    }

    fn merged_over<T: Serialize + DeserializeOwned>(
        base: T,
        overlay: Option<&Value>,
    ) -> Result<T, String> {
        let mut base = serde_json::to_value(base).map_err(|e| e.to_string())?;
        if let (Some(Value::Object(over)), Value::Object(target)) = (overlay, &mut base) {
            for (k, v) in over {
                target.insert(k.clone(), v.clone());
            }
        }
        serde_json::from_value(base).map_err(|e| format!("settings: {e}"))
    }

    fn flag(options: &Value, key: &str, default: bool) -> bool {
        options.get(key).and_then(Value::as_bool).unwrap_or(default)
    }

    fn texts(options: &Value, key: &str) -> Vec<String> {
        options
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn text(options: &Value, key: &str) -> Option<String> {
        options.get(key).and_then(Value::as_str).map(str::to_owned)
    }

    fn calc_options(options: &Value) -> Result<CalcOptions, Failure> {
        let mut run = RunOptions::new(PathBuf::new(), "");
        run.headless = true;
        run.checkreg = false;
        run.no_scene3d = true;
        run.leaddbs_atlases_dir = external_atlases_dir_from_env();
        if let Some(b) = text(options, "vat_backend") {
            run.vat_backend = VatBackend::parse(&b).map_err(Failure::Failed)?;
        }
        let mut c = CalcOptions::new(run);
        let subjects = texts(options, "subjects");
        c.subjects = (!subjects.is_empty()).then_some(subjects);
        c.stop_on_first_failure = flag(options, "stop_on_first_failure", false);
        c.contact_distances = flag(options, "contact_distances", true);
        c.mirror = flag(options, "mirror", false);
        c.flip_transform = match text(options, "flip_transform").as_deref() {
            Some("plain") => FlipTransformChoice::PlainFlip,
            _ => FlipTransformChoice::Auto,
        };
        c.copy_settings_from = text(options, "copy_settings_from");
        c.sync_settings = flag(options, "sync_settings", false);
        Ok(c)
    }

    fn counts(report: &CalcReport) -> (u32, u32, u32) {
        let n = |p: CalcPhase| u32::try_from(report.count(p)).unwrap_or(u32::MAX);
        (
            n(CalcPhase::Computed),
            n(CalcPhase::Failed) + n(CalcPhase::NotRun),
            n(CalcPhase::Kept) + n(CalcPhase::Cancelled),
        )
    }

    fn calc_event(ev: &CalcEvent, cancel: &AtomicBool) {
        match ev {
            CalcEvent::SubjectStarted { subject, .. } => emit(
                &RunEvent::StageStarted {
                    stage: subject.clone(),
                },
                cancel,
            ),
            CalcEvent::Log { line, .. } => log(line.clone(), cancel),
            CalcEvent::SubjectFinished(o) => {
                let stage = o.subject.clone();
                let message = o.message.clone().unwrap_or_default();
                let event = match o.phase {
                    CalcPhase::Computed => RunEvent::StageCompleted {
                        stage,
                        result: Some("computed".to_owned()),
                    },
                    CalcPhase::Kept => RunEvent::StageSkipped {
                        stage,
                        reason: "up to date".to_owned(),
                        result: Some("kept".to_owned()),
                    },
                    CalcPhase::Cancelled => RunEvent::StageSkipped {
                        stage,
                        reason: "cancelled".to_owned(),
                        result: None,
                    },
                    CalcPhase::Failed | CalcPhase::NotRun => RunEvent::StageFailed {
                        stage,
                        message: if message.is_empty() {
                            o.phase.as_str().to_owned()
                        } else {
                            message
                        },
                    },
                };
                emit(&event, cancel);
            }
        }
    }

    type Tally = Option<(u32, u32, u32)>;

    fn group_calc(file: &Path, options: &Value, cancel: &AtomicBool) -> (Done, Tally) {
        let c = match calc_options(options) {
            Ok(c) => c,
            Err(f) => return (Err(f), None),
        };
        let report = run_calculate_job(file, &c, Some(cancel), &mut |ev| calc_event(ev, cancel));
        match report {
            Ok(r) => {
                warn_notes(&r.notes, cancel);
                let tally = counts(&r);
                let result = if r.cancelled() {
                    Err(Failure::Cancelled)
                } else {
                    Ok(r.to_json())
                };
                (result, Some(tally))
            }
            Err(e) => (Err(group_failure(&e)), None),
        }
    }

    fn group_stats(file: &Path, options: &Value, cancel: &AtomicBool) -> Done {
        let mut opened = open_group(file).map_err(|e| group_failure(&e))?;
        warn_notes(&opened.notes, cancel);
        let c = calc_options(options)?;
        let distances = if flag(options, "contact_distances", true) {
            let report = run_contact_distances(
                &mut opened.record,
                &opened.folder,
                &c,
                Some(cancel),
                &mut |ev| calc_event(ev, cancel),
            )
            .map_err(|e| group_failure(&e))?;
            Some(report)
        } else {
            None
        };
        if cancel.load(Ordering::SeqCst) {
            return Err(Failure::Cancelled);
        }
        let exported = write_stats_exports(
            &opened.record,
            &opened.folder,
            &ExportOptions {
                csv: flag(options, "csv", false),
                lead_dbs_mat: flag(options, "lead_dbs", false),
                out_dir: None,
            },
        )
        .map_err(|e| group_failure(&e))?;
        let mut notes = exported.notes.clone();
        match save_group(&opened.json_path, &mut opened.record) {
            Ok(n) => notes.extend(n),
            Err(e) => notes.push(Note::plain(format!(
                "the group file was not fully saved: {e}"
            ))),
        }
        warn_notes(&notes, cancel);
        Ok(json!({
            "contact_distances": distances.as_ref().map(CalcReport::to_json),
            "files": rels(&opened.folder, &exported.files),
            "vta_rows": exported.vta_rows,
            "contact_rows": exported.contact_rows,
            "summary_rows": exported.summary_rows,
            "patients_with_vta": exported.patients_with_vta,
            "missing_vta": exported.missing_vta,
        }))
    }

    fn register(opened: &mut Opened, kind: &str, folder: &str, id: &str, json_name: &str) {
        opened.record.register_analysis(kind, folder, id, json_name);
    }

    fn sweetspot(file: &Path, options: &Value, cancel: &AtomicBool) -> Done {
        let mut opened = open_group(file).map_err(|e| group_failure(&e))?;
        warn_notes(&opened.notes, cancel);
        let mut base = SweetspotSettings::default();
        if let Some(name) = text(options, "preset") {
            let preset = Preset::parse(&name)
                .ok_or_else(|| Failure::Failed(format!("unknown preset {name:?}")))?;
            base.apply_preset(preset);
        }
        let settings: SweetspotSettings =
            merged_over(base, options.get("settings")).map_err(Failure::Failed)?;
        settings.validate().map_err(|e| group_failure(&e))?;
        let subjects = texts(options, "subjects");
        let run = SweetspotOptions {
            group_folder: opened.folder.clone(),
            variable: text(options, "variable").unwrap_or_default(),
            covariates: texts(options, "covariates"),
            settings,
            space: text(options, "space").unwrap_or_else(|| "MNI152NLin2009bAsym".to_owned()),
            model: text(options, "model"),
            patients: (!subjects.is_empty()).then_some(subjects),
        };
        let progress = |t: &str| log(t.to_owned(), cancel);
        let report = run_sweetspot(&opened.record, &run, cancel, &progress)
            .map_err(|e| group_failure(&e))?;
        warn_notes(&report.notes, cancel);
        let folder = opened.folder.clone();
        register(
            &mut opened,
            "sweetspot",
            "sweetspots",
            &report.id,
            "sweetspot.json",
        );
        save_record(&opened.json_path, &mut opened.record).map_err(|e| group_failure(&e))?;
        Ok(json!({
            "id": report.id,
            "folder": rel(&folder, &report.folder),
            "record": rel(&folder, &report.record_file),
            "patients": report.n_patients,
            "excluded": report.excluded.iter().map(|(s, r)| json!({"subject": s, "reason": r})).collect::<Vec<_>>(),
            "cross_validation": report.summary["cross_validation"],
            "model_fit": report.summary["model"],
            "files": rels(&folder, &report.files),
        }))
    }

    fn scheme_of(options: &Value) -> Result<CvScheme, Failure> {
        match options.get("scheme") {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| Failure::Failed(format!("scheme: {e}"))),
            None => Ok(CvScheme::LeaveOneOut),
        }
    }

    fn analysis_progress(cancel: &AtomicBool) -> impl Fn(&Progress) + Sync + '_ {
        move |p: &Progress| {
            if p.total > 0 {
                log(
                    format!("{} {}/{} {}", p.stage, p.done, p.total, p.message),
                    cancel,
                );
            }
        }
    }

    fn fibrefilter(file: &Path, options: &Value, cancel: &AtomicBool) -> Done {
        let mut opened = open_group(file).map_err(|e| group_failure(&e))?;
        warn_notes(&opened.notes, cancel);
        let settings: FibreSettings = merged(options.get("settings")).map_err(Failure::Failed)?;
        let run = FibreRunOptions {
            variable: text(options, "variable").unwrap_or_default(),
            covariates: texts(options, "covariates"),
            connectome: text(options, "connectome").unwrap_or_default(),
            connectome_root: None,
            space: text(options, "space")
                .unwrap_or_else(|| dbsloc_pipeline::connectome_catalog::REFERENCE_SPACE.to_owned()),
            model: text(options, "model"),
            settings,
            scheme: scheme_of(options)?,
            seed: options.get("seed").and_then(Value::as_u64).unwrap_or(1),
            mirror: flag(options, "mirror", false),
            group_folder: opened.folder.clone(),
            cache: dbsloc_pipeline::connectome_cache::CacheSetting::from_env(),
        };
        let progress = analysis_progress(cancel);
        let out = run_fibrefilter(&opened.record, &run, cancel, &progress)
            .map_err(|e| analysis_failure(&e))?;
        register(
            &mut opened,
            "fiberfiltering",
            "fiberfiltering",
            &out.outputs.id,
            "fibrefilter.json",
        );
        save_record(&opened.json_path, &mut opened.record).map_err(|e| group_failure(&e))?;
        warn_notes(&out.notes, cancel);
        let ev = out.result.evaluation;
        Ok(json!({
            "id": out.outputs.id,
            "folder": rel(&opened.folder, &out.outputs.dir),
            "files": rels(&opened.folder, &out.outputs.files),
            "shown_fibres": out.result.shown.len(),
            "r": ev.r, "p": ev.p, "auc": ev.auc, "n": ev.n,
            "p_perm": out.result.p_perm, "null_r95": out.result.null95,
        }))
    }

    fn netmap(file: &Path, options: &Value, cancel: &AtomicBool) -> Done {
        let mut opened = open_group(file).map_err(|e| group_failure(&e))?;
        warn_notes(&opened.notes, cancel);
        let settings: NetmapSettings = merged(options.get("settings")).map_err(Failure::Failed)?;
        let mask = match text(options, "mask").as_deref() {
            Some("brain") => MaskChoice::Brain,
            Some("all") => MaskChoice::All,
            _ => MaskChoice::Grey,
        };
        let run = NetmapRunOptions {
            variable: text(options, "variable").unwrap_or_default(),
            covariates: texts(options, "covariates"),
            connectome: text(options, "connectome").unwrap_or_default(),
            connectome_root: None,
            space: text(options, "space")
                .unwrap_or_else(|| dbsloc_pipeline::connectome_catalog::REFERENCE_SPACE.to_owned()),
            model: text(options, "model"),
            settings,
            mask,
            atlases_dir: external_atlases_dir_from_env(),
            scheme: scheme_of(options)?,
            seed: options.get("seed").and_then(Value::as_u64).unwrap_or(1),
            mirror: flag(options, "mirror", false),
            group_folder: opened.folder.clone(),
        };
        let progress = analysis_progress(cancel);
        let out = run_netmap(&opened.record, &run, cancel, &progress)
            .map_err(|e| analysis_failure(&e))?;
        register(
            &mut opened,
            "networkmapping",
            "networkmapping",
            &out.outputs.id,
            "netmap.json",
        );
        save_record(&opened.json_path, &mut opened.record).map_err(|e| group_failure(&e))?;
        warn_notes(&out.notes, cancel);
        let ev = out.result.evaluation;
        Ok(json!({
            "id": out.outputs.id,
            "folder": rel(&opened.folder, &out.outputs.dir),
            "files": rels(&opened.folder, &out.outputs.files),
            "r": ev.r, "p": ev.p, "auc": ev.auc, "n": ev.n,
            "p_perm": out.result.p_perm, "null_r95": out.result.null95,
        }))
    }

    fn export(file: &Path, options: &Value, cancel: &AtomicBool) -> Done {
        let mut opened = open_group(file).map_err(|e| group_failure(&e))?;
        warn_notes(&opened.notes, cancel);
        let only = text(options, "only");
        let mut out = serde_json::Map::new();
        if only.as_deref() != Some("clinical") {
            let notes =
                save_group(&opened.json_path, &mut opened.record).map_err(|e| group_failure(&e))?;
            warn_notes(&notes, cancel);
            let twin = dbsloc_pipeline::group::record::twin_path(
                &opened.folder,
                &opened.record.dataset_name,
                &opened.record.guid,
            );
            out.insert(
                "lead_group_file".to_owned(),
                json!(rel(&opened.folder, &twin)),
            );
        }
        if only.as_deref() != Some("twin") {
            let (tsv, sidecar) = write_clinical_export(&opened.record, &opened.folder)
                .map_err(|e| group_failure(&e))?;
            out.insert(
                "clinical_table".to_owned(),
                json!(rel(&opened.folder, &tsv)),
            );
            out.insert(
                "clinical_description".to_owned(),
                json!(rel(&opened.folder, &sidecar)),
            );
        }
        Ok(Value::Object(out))
    }

    pub(super) fn run(spec: &Value, cancel: &AtomicBool) -> i32 {
        let kind = spec["kind"].as_str().unwrap_or_default().to_owned();
        let options = spec.get("options").cloned().unwrap_or(Value::Null);
        let Some(file) = spec["group_file"].as_str().map(PathBuf::from) else {
            eprintln!("dbsloc-pipeline-runner: the group job has no group_file");
            return 2;
        };
        if kind != "group_calc" {
            emit(
                &RunEvent::StageStarted {
                    stage: kind.clone(),
                },
                cancel,
            );
        }
        let (done, tally) = match kind.as_str() {
            "group_calc" => group_calc(&file, &options, cancel),
            "group_stats" => (group_stats(&file, &options, cancel), None),
            "sweetspot" => (sweetspot(&file, &options, cancel), None),
            "fibrefilter" => (fibrefilter(&file, &options, cancel), None),
            "netmap" => (netmap(&file, &options, cancel), None),
            "export" => (export(&file, &options, cancel), None),
            other => (
                Err(Failure::Failed(format!("unknown group job kind {other:?}"))),
                None,
            ),
        };
        let (mut ok, mut failed, mut skipped) = tally.unwrap_or((0, 0, 0));
        let mut code = 0;
        match done {
            Ok(summary) => {
                if tally.is_none() {
                    ok = 1;
                    emit(
                        &RunEvent::StageCompleted {
                            stage: kind.clone(),
                            result: Some(summary.to_string()),
                        },
                        cancel,
                    );
                } else {
                    log(summary.to_string(), cancel);
                    if failed > 0 {
                        code = 1;
                    }
                }
            }
            Err(Failure::Cancelled) => {
                code = 130;
                if tally.is_none() {
                    skipped = 1;
                    emit(
                        &RunEvent::StageSkipped {
                            stage: kind.clone(),
                            reason: "cancelled".to_owned(),
                            result: None,
                        },
                        cancel,
                    );
                }
            }
            Err(Failure::Failed(message)) => {
                code = 1;
                failed += 1;
                emit(
                    &RunEvent::StageFailed {
                        stage: kind.clone(),
                        message,
                    },
                    cancel,
                );
            }
        }
        emit(
            &RunEvent::PipelineCompleted {
                stages_ok: ok,
                stages_failed: failed,
                stages_skipped: skipped,
            },
            cancel,
        );
        code
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::group_job;
    use dbsloc_pipeline::group::fibrefilter::FibreSettings;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn settings_overlay_replaces_only_the_named_fields() {
        let s: FibreSettings =
            group_job::merged(Some(&json!({"alpha": 0.01, "efield_threshold": 150.0}))).unwrap();
        assert!((s.alpha - 0.01).abs() < 1e-12);
        assert!((s.efield_threshold - 150.0).abs() < 1e-12);
        assert!((s.coverage_percent - 20.0).abs() < 1e-12);
        let bad = group_job::merged::<FibreSettings>(Some(&json!({"alpha": "x"})));
        assert!(bad.is_err());
        let d: FibreSettings = group_job::merged(None).unwrap();
        assert_eq!(d, FibreSettings::default());
    }

    #[test]
    fn an_export_job_writes_the_clinical_table_of_a_new_group() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ds");
        std::fs::create_dir_all(root.join("derivatives")).unwrap();
        let location = dbsloc_pipeline::group::locate(&root, "g1", None, Some("ds")).unwrap();
        let (_, json_path, _) = dbsloc_pipeline::group::create_group(&location, "t", None).unwrap();
        let spec = json!({
            "kind": "export",
            "group_file": json_path.to_string_lossy(),
            "options": {"only": "clinical"},
        });
        let cancel = AtomicBool::new(false);
        assert_eq!(group_job::run(&spec, &cancel), 0);
        let table = location
            .folder
            .join("exports")
            .join("dataset-ds_analysis-g1_desc-clinical.tsv");
        assert!(table.is_file());
        let missing = json!({
            "kind": "export",
            "group_file": dir.path().join("none.json").to_string_lossy(),
        });
        assert_eq!(group_job::run(&missing, &cancel), 1);
        let unknown = json!({"kind": "nope", "group_file": json_path.to_string_lossy()});
        assert_eq!(group_job::run(&unknown, &cancel), 1);
    }
}
