// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Interactive checkreg review (`specs/03_coregistration.md` §12.3; W/L + zoom `specs/08_addendum_interactive_volume_viewer.md`). eframe+egui (wgpu); no Matplotlib.
//!
//! On-screen: **load NIfTIs** → ortho 2×2 (axial / sagittal / coronal) + optional single-pane; slices **resampled to voxel spacing** (anatomical aspect); linked **crosshair**; batch PNG still for `Refresh`.
//!
//! Pipeline opens this viewer after **coregistration**, **normalization** (anchor→template), and **brainshift** when QC is enabled; each manifest slide's `branch` drives the window/heading label.
//!
//! `dbs-localizer checkreg --manifest PATH --headless-check` — validate JSON, no window.
//!
//! Manifest: `subj_id`, `active`, `slides[]` with `checkreg_png`, `moving`, `fixed`, `branch`, `modality`, `method_log`, optional `subj_dir`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::checkreg_app::{
    registration_qc_kind_label, CheckregManifest, CheckregReviewPanel, ReviewMode,
};
use crate::checkreg_view::ReviewDecision;
use clap::Parser;
use eframe::egui;
use serde_json::json;
use serde_json::Value as Json;

#[derive(Parser, Debug)]
#[command(name = "dbs-localizer checkreg")]
struct Args {
    #[arg(long, value_name = "PATH")]
    manifest: PathBuf,
    #[arg(long, action = clap::ArgAction::SetTrue)]
    headless_check: bool,
}

struct CheckregShell {
    panel: CheckregReviewPanel,
    decisions: HashMap<usize, bool>,
    sent_title: Option<String>,
}

impl CheckregShell {
    fn title(&self) -> String {
        let man = self.panel.manifest();
        let kind = man
            .slides
            .get(self.panel.active_slide())
            .map(|s| registration_qc_kind_label(&s.branch))
            .unwrap_or("Registration QC");
        format!("{kind} — {}", man.subj_id)
    }

    fn write_approval(&mut self, decision: &ReviewDecision) {
        self.decisions.insert(decision.slide, decision.approved());
        let slides = &self.panel.manifest().slides;
        let Some(sl) = slides.get(decision.slide) else {
            return;
        };
        let Some(path) = sl.method_log.as_ref() else {
            eprintln!("checkreg: no `method_log` on slide; add to manifest to write JSON approval");
            return;
        };
        let branch = sl.branch.to_lowercase();
        let approved = if is_scalar_branch(&branch) {
            let siblings: Vec<Option<bool>> = slides
                .iter()
                .enumerate()
                .filter(|(_, o)| o.method_log.as_ref() == Some(path))
                .map(|(i, _)| self.decisions.get(&i).copied())
                .collect();
            if siblings.contains(&Some(false)) {
                false
            } else if siblings.iter().all(|d| *d == Some(true)) {
                true
            } else {
                return;
            }
        } else {
            decision.approved()
        };
        if let Err(e) = apply_approval(path, &branch, sl.modality.as_deref(), approved) {
            eprintln!("checkreg: write approval: {e}");
        } else {
            eprintln!("checkreg: wrote approval to {}", path.display());
        }
    }
}

fn is_scalar_branch(branch: &str) -> bool {
    matches!(branch, "norm" | "normalization" | "brainshift")
}

impl eframe::App for CheckregShell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let title = self.title();
        if self.sent_title.as_deref() != Some(title.as_str()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.sent_title = Some(title);
        }
        for decision in self.panel.show(ui) {
            self.write_approval(&decision);
        }
        if self.panel.all_decided() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn apply_approval(
    log_path: &Path,
    branch: &str,
    modality: Option<&str>,
    approved: bool,
) -> Result<(), String> {
    use serde_json::Map;

    let mut root: Json = if log_path.is_file() {
        let s = fs::read_to_string(log_path).map_err(|e| e.to_string())?;
        if s.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&s).map_err(|e| e.to_string())?
        }
    } else {
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        json!({})
    };

    if root.is_null() {
        root = json!({});
    }
    if !root.is_object() {
        return Err(format!(
            "{}: method log is not a JSON object",
            log_path.display()
        ));
    }

    let val = if approved { 1 } else { 0 };
    match branch {
        "norm" | "normalization" | "brainshift" => {
            root["reviewed"] = json!(true);
            root["approval"] = json!(val);
        }
        _ => {
            let m = modality.unwrap_or("UNSPECIFIED");
            let next = match root.get("approval") {
                Some(approval) if approval.is_object() => {
                    let mut o = approval.as_object().cloned().unwrap_or_default();
                    o.insert(m.to_string(), json!(val));
                    Json::Object(o)
                }
                _ => {
                    let mut o = Map::new();
                    o.insert(m.to_string(), json!(val));
                    Json::Object(o)
                }
            };
            let mut keys: Vec<String> = match root.get("reviewed") {
                Some(Json::Array(list)) => list
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                Some(Json::Bool(true)) => next
                    .as_object()
                    .map(|o| o.keys().cloned().collect())
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            if !keys.iter().any(|k| k == m) {
                keys.push(m.to_string());
            }
            root["reviewed"] = json!(keys);
            root["approval"] = next;
        }
    }

    let out = format!(
        "{}\n",
        serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?
    );
    let name = log_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = log_path.with_file_name(format!(".partial-{name}"));
    fs::write(&tmp, out).map_err(|e| e.to_string())?;
    if let Err(e) = fs::rename(&tmp, log_path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

/// Runs the review window with the arguments that follow `checkreg` on the command line and
/// returns the process exit code.
pub fn run_from_args(args: impl IntoIterator<Item = String>) -> i32 {
    let args = Args::parse_from(std::iter::once("dbs-localizer checkreg".to_owned()).chain(args));
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e:?}");
            1
        }
    }
}

fn run(args: Args) -> eframe::Result<()> {
    let s = std::fs::read_to_string(&args.manifest)
        .map_err(|e| eframe::Error::AppCreation(Box::new(e)))?;
    let man: CheckregManifest = serde_json::from_str(&s)
        .map_err(|e: serde_json::Error| eframe::Error::AppCreation(Box::new(e)))?;
    if man.slides.is_empty() {
        eprintln!("checkreg: empty `slides` — close (addendum).");
        return Ok(());
    }
    if args.headless_check {
        println!(
            "manifest v{}: {} slides for {}",
            man.manifest_version,
            man.slides.len(),
            man.subj_id
        );
        return Ok(());
    }

    let man_title = man.subj_id.clone();
    let boot_kind = man
        .slides
        .first()
        .map(|s| registration_qc_kind_label(&s.branch))
        .unwrap_or("Registration QC");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{boot_kind} — {man_title}"))
            .with_icon(crate::brand::window_icon())
            .with_inner_size([1600.0, 960.0]),
        wgpu_options: crate::gpu_setup::native_wgpu_configuration(),
        ..Default::default()
    };
    eframe::run_native(
        "checkreg",
        options,
        Box::new(|cc| {
            crate::brand::install_fonts(&cc.egui_ctx);
            crate::brand::apply_theme(&cc.egui_ctx);
            let mut panel = CheckregReviewPanel::from_manifest(man);
            panel.set_mode(ReviewMode::AfterRun);
            Ok(Box::new(CheckregShell {
                panel,
                decisions: HashMap::new(),
                sent_title: None,
            }) as Box<dyn eframe::App>)
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standalone_writer_sets_the_decision_marker() {
        let dir = std::env::temp_dir().join(format!("dbsloc_checkreg_bin_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let log = dir.join("m.json");
        fs::write(&log, r#"{"method":"ants","approval":0}"#).expect("write");
        apply_approval(&log, "norm", None, true).expect("approve");
        let v: Json = serde_json::from_str(&fs::read_to_string(&log).expect("read")).expect("json");
        assert_eq!(v["approval"], json!(1));
        assert_eq!(v["reviewed"], json!(true));
        assert_eq!(v["method"], json!("ants"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_standalone_writer_refuses_a_non_object_log_without_panicking() {
        let dir =
            std::env::temp_dir().join(format!("dbsloc_checkreg_bin_a_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let log = dir.join("m.json");
        fs::write(&log, "[1,2]").expect("write");
        assert!(apply_approval(&log, "norm", None, true).is_err());
        assert!(apply_approval(&log, "coreg", Some("CT"), true).is_err());
        assert_eq!(fs::read_to_string(&log).expect("read"), "[1,2]");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_standalone_writer_lists_reviewed_coreg_modalities() {
        let dir =
            std::env::temp_dir().join(format!("dbsloc_checkreg_bin_c_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let log = dir.join("m.json");
        fs::write(&log, r#"{"approval":{"CT":0,"T2w":0}}"#).expect("write");
        apply_approval(&log, "coreg", Some("CT"), true).expect("approve");
        let v: Json = serde_json::from_str(&fs::read_to_string(&log).expect("read")).expect("json");
        assert_eq!(v["approval"], json!({"CT": 1, "T2w": 0}));
        assert_eq!(v["reviewed"], json!(["CT"]));
        apply_approval(&log, "coreg", Some("T2w"), false).expect("reject");
        let v: Json = serde_json::from_str(&fs::read_to_string(&log).expect("read")).expect("json");
        assert_eq!(v["reviewed"], json!(["CT", "T2w"]));
        let _ = fs::remove_dir_all(&dir);
    }
}
