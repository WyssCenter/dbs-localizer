// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The guided step model: the ordered steps and the pure "can this step run"
//! logic. No `egui`, no filesystem — the desktop and browser front ends both
//! render this model.
//!
//! Each stage step is judged as a run of that step alone (`StageSettings::only`).

use dbsloc_protocol::stage::Stage;

use crate::readiness::{PipelineReadiness, StageState};
use crate::settings::{stage_status, StageSettings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuidedStep {
    Subject,
    Preprocess,
    Coregister,
    Normalize,
    Reconstruct,
    Stimulation,
    Review,
}

impl GuidedStep {
    pub const ALL: [GuidedStep; 7] = [
        GuidedStep::Subject,
        GuidedStep::Preprocess,
        GuidedStep::Coregister,
        GuidedStep::Normalize,
        GuidedStep::Reconstruct,
        GuidedStep::Stimulation,
        GuidedStep::Review,
    ];

    /// The first step that does work, where a freshly opened subject starts.
    pub const FIRST_WORK: GuidedStep = GuidedStep::Preprocess;

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn next(self) -> Option<GuidedStep> {
        Self::ALL.get(self.index() + 1).copied()
    }

    pub fn prev(self) -> Option<GuidedStep> {
        self.index().checked_sub(1).map(|i| Self::ALL[i])
    }

    pub fn stage(self) -> Option<Stage> {
        match self {
            GuidedStep::Preprocess => Some(Stage::Preprocess),
            GuidedStep::Coregister => Some(Stage::Coregister),
            GuidedStep::Normalize => Some(Stage::Normalize),
            GuidedStep::Reconstruct => Some(Stage::Reconstruct),
            GuidedStep::Stimulation => Some(Stage::Vat),
            GuidedStep::Subject | GuidedStep::Review => None,
        }
    }

    pub fn from_name(name: &str) -> Option<GuidedStep> {
        Self::ALL
            .into_iter()
            .find(|s| s.title().eq_ignore_ascii_case(name.trim()))
    }

    pub fn title(self) -> &'static str {
        match self {
            GuidedStep::Subject => "Subject",
            GuidedStep::Preprocess => "Preprocess",
            GuidedStep::Coregister => "Coregister",
            GuidedStep::Normalize => "Normalize",
            GuidedStep::Reconstruct => "Reconstruct",
            GuidedStep::Stimulation => "Stimulation",
            GuidedStep::Review => "Review",
        }
    }

    pub fn optional_hint(self) -> Option<&'static str> {
        match self {
            GuidedStep::Stimulation => {
                Some("Optional: compute the volume of activated tissue of a saved stimulation")
            }
            _ => None,
        }
    }
}

pub const NO_STIMULATION_HINT: &str = "No saved stimulation settings";

pub fn chosen_stimulation(readiness: &PipelineReadiness, chosen: Option<&str>) -> Option<String> {
    chosen
        .filter(|c| readiness.stimulation_labels.iter().any(|l| l == c))
        .or_else(|| readiness.stimulation_labels.first().map(String::as_str))
        .map(str::to_owned)
}

pub fn stimulation_computed(readiness: &PipelineReadiness, label: Option<&str>) -> bool {
    label.is_some_and(|l| readiness.stimulation_computed.iter().any(|c| c == l))
}

/// Status, status wording and blockers of a run of exactly `stage`. Shared by
/// the guided screens of both front ends.
pub fn guided_step_check(
    settings: &StageSettings,
    readiness: &PipelineReadiness,
    stage: Stage,
) -> (StageState, &'static str, Vec<String>) {
    let only = settings.only(stage);
    let (status, text) = stage_status(stage, &only, readiness);
    let blockers = only.selection_blockers(readiness, &[stage]);
    (status, text, blockers)
}

/// Whether a run of exactly `stage` could start right now.
pub fn guided_can_run(
    settings: &StageSettings,
    readiness: &PipelineReadiness,
    stage: Stage,
) -> bool {
    let (status, _, blockers) = guided_step_check(settings, readiness, stage);
    if stage == Stage::Vat && readiness.stimulation_labels.is_empty() {
        return false;
    }
    status != StageState::Blocked && blockers.is_empty()
}

/// Why a run of exactly `stage` cannot start, worded for one message.
pub fn guided_refusal(
    settings: &StageSettings,
    readiness: &PipelineReadiness,
    stage: Stage,
) -> String {
    if stage == Stage::Vat && readiness.stimulation_labels.is_empty() {
        return NO_STIMULATION_HINT.to_owned();
    }
    let (_, text, blockers) = guided_step_check(settings, readiness, stage);
    blockers
        .into_iter()
        .next()
        .unwrap_or_else(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> PipelineReadiness {
        PipelineReadiness {
            has_bids_dir: true,
            has_subject: true,
            template_exists: true,
            has_raw_anat: true,
            has_raw_postop_ct: true,
            has_ants_registration: true,
            has_n4_bias_correction: true,
            gpu_available: Some(true),
            ..PipelineReadiness::default()
        }
    }

    #[test]
    fn steps_run_in_pipeline_order_and_stop_at_both_ends() {
        let order = [
            GuidedStep::Subject,
            GuidedStep::Preprocess,
            GuidedStep::Coregister,
            GuidedStep::Normalize,
            GuidedStep::Reconstruct,
            GuidedStep::Stimulation,
            GuidedStep::Review,
        ];
        assert_eq!(GuidedStep::ALL, order);
        for pair in order.windows(2) {
            assert_eq!(pair[0].next(), Some(pair[1]));
            assert_eq!(pair[1].prev(), Some(pair[0]));
        }
        for step in order {
            assert_eq!(
                GuidedStep::from_name(&step.title().to_lowercase()),
                Some(step)
            );
        }
        assert_eq!(GuidedStep::from_name("brainshift"), None);
        assert_eq!(GuidedStep::Subject.prev(), None);
        assert_eq!(GuidedStep::Review.next(), None);
    }

    #[test]
    fn only_the_five_working_steps_have_a_stage() {
        let stages: Vec<Option<Stage>> = GuidedStep::ALL.iter().map(|s| s.stage()).collect();
        assert_eq!(
            stages,
            vec![
                None,
                Some(Stage::Preprocess),
                Some(Stage::Coregister),
                Some(Stage::Normalize),
                Some(Stage::Reconstruct),
                Some(Stage::Vat),
                None,
            ]
        );
        assert!(stages
            .iter()
            .flatten()
            .all(|s| Stage::work_sequence().contains(s)));
        assert_eq!(
            GuidedStep::from_name("stimulation"),
            Some(GuidedStep::Stimulation)
        );
        assert_eq!(
            GuidedStep::Reconstruct.next(),
            Some(GuidedStep::Stimulation)
        );
        assert_eq!(GuidedStep::Stimulation.next(), Some(GuidedStep::Review));
        assert!(GuidedStep::Stimulation.optional_hint().is_some());
        assert!(GuidedStep::Reconstruct.optional_hint().is_none());
        assert_eq!(GuidedStep::FIRST_WORK.stage(), Some(Stage::Preprocess));
    }

    #[test]
    fn a_single_step_run_does_not_count_an_unselected_upstream_step() {
        let settings = StageSettings {
            el_model: Some("any".to_owned()),
            ..StageSettings::default()
        };
        let readiness = ready();
        assert!(!guided_can_run(&settings, &readiness, Stage::Normalize));
        assert!(!guided_can_run(&settings, &readiness, Stage::Reconstruct));
        let coregistered = PipelineReadiness {
            has_coreg_warped: true,
            ..ready()
        };
        assert!(guided_can_run(&settings, &coregistered, Stage::Normalize));
        let normalized = PipelineReadiness {
            has_norm_postop_ct: true,
            has_norm_nifti: true,
            ..readiness
        };
        assert!(guided_can_run(&settings, &normalized, Stage::Reconstruct));
    }

    #[test]
    fn reconstruct_waits_for_an_electrode_model() {
        let readiness = PipelineReadiness {
            has_norm_postop_ct: true,
            has_norm_nifti: true,
            ..ready()
        };
        let mut settings = StageSettings::default();
        assert!(!guided_can_run(&settings, &readiness, Stage::Reconstruct));
        assert!(guided_refusal(&settings, &readiness, Stage::Reconstruct).contains("electrode"));
        settings.el_model = Some("any".to_owned());
        assert!(guided_can_run(&settings, &readiness, Stage::Reconstruct));
    }

    #[test]
    fn a_step_without_its_input_cannot_run() {
        let readiness = PipelineReadiness {
            has_raw_postop_ct: false,
            ..ready()
        };
        let settings = StageSettings::default();
        assert!(!guided_can_run(&settings, &readiness, Stage::Coregister));
        assert_eq!(
            guided_refusal(&settings, &readiness, Stage::Coregister),
            "Coregistration needs a post-operative CT or MRI in this subject."
        );
    }

    #[test]
    fn stimulation_needs_a_saved_label_and_normalized_anatomy() {
        let settings = StageSettings::default();
        let normalized = PipelineReadiness {
            has_norm_nifti: true,
            has_norm_postop_ct: true,
            has_reconstruction_json: true,
            ..ready()
        };
        assert!(!guided_can_run(&settings, &normalized, Stage::Vat));
        assert_eq!(
            guided_refusal(&settings, &normalized, Stage::Vat),
            NO_STIMULATION_HINT
        );
        let saved = PipelineReadiness {
            stimulation_labels: vec!["a".to_owned(), "b".to_owned()],
            ..normalized
        };
        assert!(guided_can_run(&settings, &saved, Stage::Vat));
    }

    #[test]
    fn the_chosen_stimulation_falls_back_to_the_first_saved_label() {
        let r = PipelineReadiness {
            stimulation_labels: vec!["a".to_owned(), "b".to_owned()],
            stimulation_computed: vec!["b".to_owned()],
            ..ready()
        };
        assert_eq!(chosen_stimulation(&r, None).as_deref(), Some("a"));
        assert_eq!(chosen_stimulation(&r, Some("b")).as_deref(), Some("b"));
        assert_eq!(chosen_stimulation(&r, Some("gone")).as_deref(), Some("a"));
        assert_eq!(chosen_stimulation(&ready(), Some("a")), None);
        assert!(stimulation_computed(&r, Some("b")));
        assert!(!stimulation_computed(&r, Some("a")));
        assert!(!stimulation_computed(&r, None));
    }
}
