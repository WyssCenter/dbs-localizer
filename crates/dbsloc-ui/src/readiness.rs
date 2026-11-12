// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! What exists on disk for a subject's pipeline steps, as pure data.
//!
//! The struct itself carries no filesystem dependency: `dbsloc-app` fills it
//! from a background scan (in its own crate) and `dbsloc-web` fills it from the
//! server's subject summary. Everything here is pure and compiles for `wasm32`.

use std::path::PathBuf;

use dbsloc_protocol::stage::Stage;

use crate::settings::PostopModality;

/// Whether a step could run right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageState {
    Ready,
    NeedsUpstream,
    Blocked,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationReview {
    #[default]
    NotReviewed,
    Approved,
    Rejected,
}

impl RegistrationReview {
    pub fn chip_text(self) -> &'static str {
        match self {
            RegistrationReview::NotReviewed => "Not reviewed",
            RegistrationReview::Approved => "Reviewed ✔",
            RegistrationReview::Rejected => "Rejected",
        }
    }

    pub fn chip_hover(self) -> &'static str {
        match self {
            RegistrationReview::NotReviewed => {
                "Nobody has approved or rejected this registration yet. Use Review registration to look at it."
            }
            RegistrationReview::Approved => {
                "You approved this registration in the registration review."
            }
            RegistrationReview::Rejected => {
                "You rejected this registration. Rerun the step to redo it, or review it again."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadReview {
    Corrected,
    Accepted,
    Unchecked,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoSideSummary {
    pub label: &'static str,
    pub review: LeadReview,
    pub reason: Option<String>,
    pub flags: Vec<String>,
}

impl RecoSideSummary {
    pub fn text(&self) -> String {
        let state = match self.review {
            LeadReview::Corrected => "corrected",
            LeadReview::Accepted => "accepted",
            LeadReview::Unchecked => "unchecked",
            LeadReview::NotFound => "not found",
        };
        let mut text = format!("{}: {state}", self.label);
        if self.flags.iter().any(|f| f == "vertical_marker") {
            text.push_str(" · vertical segment marker");
        }
        text
    }

    pub fn hover(&self) -> String {
        let mut text = match self.review {
            LeadReview::Corrected => {
                "Moved or turned by hand in Refine localization.".to_owned()
            }
            LeadReview::Accepted => {
                "Checked in Refine localization and accepted without changes.".to_owned()
            }
            LeadReview::Unchecked => "Not yet checked in Refine localization. The Reconstruct step counts as approved once every lead is corrected or accepted.".to_owned(),
            LeadReview::NotFound => format!(
                "The automatic reconstruction found no lead on this side: {}",
                self.reason.as_deref().unwrap_or("no reason recorded")
            ),
        };
        if self.flags.iter().any(|f| f == "vertical_marker") {
            text.push_str(" The segment direction of this lead points straight up or down, so its template-space x / y markers are the individually mapped points.");
        }
        text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    PreopAnchor,
    PreopOther,
    PostopCt,
    PostopMr,
    Derived,
}

/// One image file of a subject that the pipeline reads or writes, for the *View* buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRow {
    pub kind: ImageKind,
    pub label: String,
    pub path: PathBuf,
    pub is_ct: bool,
}

/// Which per-step outputs exist for one subject, plus the environment facts the
/// step cards judge against. Pure data — the pipeline-facing judgement lives in
/// [`crate::stage_status`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PipelineReadiness {
    pub has_bids_dir: bool,
    pub has_subject: bool,
    pub template_exists: bool,
    pub has_preprocess_nifti: bool,
    pub has_coreg_warped: bool,
    pub has_raw_anat: bool,
    pub has_raw_postop_ct: bool,
    pub has_norm_nifti: bool,
    pub has_norm_postop_ct: bool,
    pub has_raw_postop_mr: bool,
    pub has_norm_postop_mr: bool,
    pub has_reconstruction_json: bool,
    pub reco_manually_corrected: bool,
    pub has_orientation_json: bool,
    pub has_vat_preview: bool,
    pub stimulation_labels: Vec<String>,
    pub stimulation_computed: Vec<String>,
    pub has_scene_manifest: bool,
    pub coreg_review: RegistrationReview,
    pub norm_review: RegistrationReview,
    /// A directory scan hit its entry budget, so "not found" may be wrong.
    pub scan_truncated: bool,
    /// External programs the steps shell out to.
    pub has_ants_registration: bool,
    pub has_n4_bias_correction: bool,
    /// Whether a usable graphics card was found; `None` while the start-up check still runs.
    pub gpu_available: Option<bool>,
    pub reco_sides: Vec<RecoSideSummary>,
    pub images: Vec<ImageRow>,
    pub auto_postop_is_ct: bool,
}

impl PipelineReadiness {
    pub fn anchor_image(&self) -> Option<&ImageRow> {
        self.images
            .iter()
            .find(|r| r.kind == ImageKind::PreopAnchor)
    }

    pub fn postop_images_for(&self, choice: PostopModality) -> Vec<&ImageRow> {
        let ct: Vec<&ImageRow> = self
            .images
            .iter()
            .filter(|r| r.kind == ImageKind::PostopCt)
            .collect();
        let mr: Vec<&ImageRow> = self
            .images
            .iter()
            .filter(|r| r.kind == ImageKind::PostopMr)
            .collect();
        match (choice, ct.is_empty(), mr.is_empty()) {
            (_, true, true) => Vec::new(),
            (_, false, true) => ct,
            (_, true, false) => mr,
            (PostopModality::Ct, _, _) => ct,
            (PostopModality::Mri, _, _) => mr,
            (PostopModality::Auto, _, _) if self.auto_postop_is_ct => ct,
            (PostopModality::Auto, _, _) => mr,
        }
    }

    pub fn registration_review(&self, stage: Stage) -> Option<RegistrationReview> {
        match stage {
            Stage::Coregister => Some(self.coreg_review),
            Stage::Normalize => Some(self.norm_review),
            _ => None,
        }
    }

    pub fn has_raw_postop(&self) -> bool {
        self.has_raw_postop_ct || self.has_raw_postop_mr
    }

    pub fn has_norm_postop(&self) -> bool {
        self.has_norm_postop_ct || self.has_norm_postop_mr
    }

    pub fn stage_output_exists(&self, stage: Stage) -> bool {
        match stage {
            Stage::Preprocess => self.has_preprocess_nifti,
            Stage::Coregister => self.has_coreg_warped,
            Stage::Normalize => self.has_norm_nifti,
            Stage::Reconstruct => self.has_reconstruction_json,
            Stage::Orientation => self.has_orientation_json,
            Stage::Vat => !self.stimulation_computed.is_empty(),
            Stage::Visualize => self.has_scene_manifest,
            Stage::Bids | Stage::Precoreg | Stage::Brainshift => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vat_output_exists_once_a_stimulation_is_computed() {
        let mut r = PipelineReadiness::default();
        assert!(!r.stage_output_exists(Stage::Vat));
        r.stimulation_computed = vec!["a".to_owned()];
        assert!(r.stage_output_exists(Stage::Vat));
    }

    #[test]
    fn only_the_two_registration_steps_have_a_review_state() {
        let r = PipelineReadiness {
            coreg_review: RegistrationReview::Approved,
            norm_review: RegistrationReview::Rejected,
            ..PipelineReadiness::default()
        };
        assert_eq!(
            r.registration_review(Stage::Coregister),
            Some(RegistrationReview::Approved)
        );
        assert_eq!(
            r.registration_review(Stage::Normalize),
            Some(RegistrationReview::Rejected)
        );
        assert_eq!(r.registration_review(Stage::Reconstruct), None);
        assert_eq!(
            PipelineReadiness::default().coreg_review,
            RegistrationReview::NotReviewed
        );
    }

    #[test]
    fn chip_wording_is_distinct_per_state() {
        assert_eq!(RegistrationReview::Approved.chip_text(), "Reviewed ✔");
        assert_eq!(RegistrationReview::Rejected.chip_text(), "Rejected");
        assert_eq!(RegistrationReview::NotReviewed.chip_text(), "Not reviewed");
    }
}
