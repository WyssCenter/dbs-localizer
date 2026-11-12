// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Plain, serializable description of a subject's pipeline state.
//!
//! The server and the browser client need the same view of a subject that the desktop app's
//! readiness scan builds, but without any UI types. This module extracts that data gathering into
//! the pipeline library: [`subject_summary`] resolves the subject and reports, per stage, whether
//! it is approved, has outputs, is a known stub, or is missing, plus the two manifests the client
//! can fetch. No paths are UI objects: every field is a `String` so the DTO can travel as JSON
//! (`docs/SERVER_CLIENT_PLAN.md` §4.1, WP1).
//!
//! ## Stage state rule
//!
//! For each stage in [`Stage::default_sequence`], in order:
//!
//! 1. `"approved"` — the stage lock exists and its approval flag is true
//!    ([`crate::locks::is_approved`]).
//! 2. `"outputs"` — the stage's primary output exists on disk (see [`stage_output_exists`]).
//! 3. `"stub"` — the stage is `vat`, which has no body yet (`specs/07`).
//! 4. `"missing"` — otherwise.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bids::{resolve_subject_in, BidsError, Subject};
use crate::checkreg;
use crate::locks;
use crate::options::RunOptions;
use crate::stages::Stage;

/// One stage's state in a [`SubjectSummaryData`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSummaryData {
    /// CLI stage name (`specs/00`): `bids`, `preprocess`, `coreg`, …
    pub stage: String,
    /// One of `"approved"`, `"outputs"`, `"stub"`, `"missing"`.
    pub state: String,
    /// Reserved for a stage result token; currently always `null`.
    pub result: Option<String>,
}

/// Plain, serializable subject description for the server API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectSummaryData {
    pub subj_id: String,
    pub anchor_modality: String,
    /// Pre-op modalities present, in resolution order (anchor first as resolved).
    pub modalities: Vec<String>,
    #[serde(default)]
    pub postop_modalities: Vec<String>,
    pub stages: Vec<StageSummaryData>,
    /// `locks/checkreg_manifest.json` when it exists.
    pub checkreg_manifest: Option<String>,
    /// `reconstruction/viz/scene_manifest.json` when it exists.
    pub scene_manifest: Option<String>,
}

/// Resolve `subject` under `bids_dir` and describe its pipeline state.
///
/// `opts` supplies the index-resolution choice ([`RunOptions::use_index`]) and any resolved
/// template; the scan itself needs no template. A missing or unanchorable subject returns the
/// underlying [`BidsError`].
pub fn subject_summary(
    bids_dir: &Path,
    subject: &str,
    opts: &RunOptions,
) -> Result<SubjectSummaryData, BidsError> {
    let sub = resolve_subject_in(
        bids_dir,
        opts.output_dir.as_deref(),
        subject,
        opts.use_index,
    )?;
    let modalities: Vec<String> = sub.preop_raw.keys().cloned().collect();
    let postop_modalities: Vec<String> = sub.postop_raw.keys().cloned().collect();
    let stages = Stage::default_sequence()
        .iter()
        .map(|s| stage_summary(&sub, *s))
        .collect();
    Ok(SubjectSummaryData {
        subj_id: sub.subj_id.clone(),
        anchor_modality: sub.anchor_modality.clone(),
        modalities,
        postop_modalities,
        stages,
        checkreg_manifest: file_string(&checkreg::manifest_path(&sub)),
        scene_manifest: file_string(&scene_manifest_path(&sub)),
    })
}

fn scene_manifest_path(sub: &Subject) -> PathBuf {
    crate::derivatives::scene_manifest(sub)
}

fn file_string(p: &Path) -> Option<String> {
    p.is_file().then(|| p.to_string_lossy().into_owned())
}

fn stage_summary(sub: &Subject, stage: Stage) -> StageSummaryData {
    let lock = locks::stage_approval_path(&sub.locks_dir(), stage.cli_name());
    let approved = locks::is_approved(&lock).unwrap_or(false);
    let state = if approved {
        "approved"
    } else if stage_output_exists(sub, stage) {
        "outputs"
    } else if matches!(stage, Stage::Vat) {
        "stub"
    } else {
        "missing"
    };
    StageSummaryData {
        stage: stage.cli_name().to_owned(),
        state: state.to_owned(),
        result: None,
    }
}

/// The stage's primary output, matching the desktop scan (`dbsloc-app` `panels.rs`).
///
/// `bids`/`precoreg`/`brainshift`/`orientation` have no primary output of their own; the image
/// outputs are read from the `Subject` bundles (`specs/01` §7a).
fn stage_output_exists(sub: &Subject, stage: Stage) -> bool {
    match stage {
        Stage::Preprocess => sub.preop_anchor_preproc.is_some(),
        Stage::Coregister => {
            !sub.coreg.anat_postop.is_empty() || sub.coreg.anat_preop.contains_key("T2w")
        }
        Stage::Normalize => !sub.norm.anat_preop.is_empty() || !sub.norm.anat_postop.is_empty(),
        Stage::Reconstruct => sub.reconstruction_json().is_file(),
        Stage::Visualize => scene_manifest_path(sub).is_file(),
        Stage::Bids | Stage::Precoreg | Stage::Brainshift | Stage::Orientation | Stage::Vat => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bids::resolve_subject_with;
    use serde_json::json;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_summary_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    #[test]
    fn reports_states_and_manifests() {
        let root = scratch("states");
        touch(&root.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"));
        touch(&root.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T2w.nii.gz"));
        let opts = RunOptions::new(&root, "1");
        touch(&root.join("rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_CT.nii.gz"));
        let sub = resolve_subject_with(&root, "1", opts.use_index).unwrap();

        touch(&crate::derivatives::preproc_image(&sub));
        touch(&crate::derivatives::coreg_ct_image(&sub));
        let coreg_lock = locks::stage_approval_path(&sub.locks_dir(), "coreg");
        locks::write_approval(&coreg_lock, true, Some("ants"), &[], None).unwrap();
        touch(&crate::derivatives::norm_anchor_image(&sub, "MNI"));
        std::fs::create_dir_all(crate::derivatives::norm_log_dir(&sub)).unwrap();
        std::fs::write(
            crate::derivatives::norm_method_log(&sub),
            r#"{"template_space": "MNI"}"#,
        )
        .unwrap();
        touch(&sub.reconstruction_json());
        touch(&scene_manifest_path(&sub));
        touch(&checkreg::manifest_path(&sub));

        let s = subject_summary(&root, "1", &opts).unwrap();
        assert_eq!(s.subj_id, "1");
        assert_eq!(s.anchor_modality, "T1w");
        assert_eq!(s.modalities, vec!["T1w".to_owned(), "T2w".to_owned()]);
        let state = |name: &str| {
            s.stages
                .iter()
                .find(|st| st.stage == name)
                .unwrap()
                .state
                .clone()
        };
        assert_eq!(state("bids"), "missing");
        assert_eq!(state("preprocess"), "outputs");
        assert_eq!(state("precoreg"), "missing");
        assert_eq!(state("coreg"), "approved");
        assert_eq!(state("normalize"), "outputs");
        assert_eq!(state("brainshift"), "missing");
        assert_eq!(state("reconstruct"), "outputs");
        assert_eq!(state("orientation"), "missing");
        assert_eq!(state("vat"), "stub");
        assert_eq!(state("visualize"), "outputs");
        assert!(s.checkreg_manifest.as_deref().is_some_and(|p| {
            p.ends_with(&format!(
                "locks{}checkreg_manifest.json",
                std::path::MAIN_SEPARATOR
            ))
        }));
        assert!(s.scene_manifest.is_some());
        assert!(s.stages.iter().all(|st| st.result.is_none()));
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["subj_id"], json!("1"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_subject_is_a_bids_error() {
        let root = scratch("nosub");
        let opts = RunOptions::new(&root, "7");
        let e = subject_summary(&root, "7", &opts).unwrap_err();
        assert_eq!(e.code, crate::bids::E_NO_SUBJ);
        let _ = std::fs::remove_dir_all(&root);
    }
}
