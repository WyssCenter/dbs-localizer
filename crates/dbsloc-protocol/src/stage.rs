// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Pipeline stage enum — ``specs/00_overview.md`` §5.1.
//!
//! Moved verbatim from `dbsloc-pipeline/src/stages.rs`: the enum, its serde wire shape (`snake_case`
//! with the `coreg` alias for `Coregister`) and the label / CLI-name strings are the wire contract
//! the CLI, GUI and server rely on, so they must not change.

use serde::{Deserialize, Serialize};

/// Ordered pipeline stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Bids,
    Preprocess,
    Precoreg,
    #[serde(alias = "coreg")]
    Coregister,
    Normalize,
    Brainshift,
    Reconstruct,
    Orientation,
    Vat,
    Visualize,
}

const ALL: [Stage; 10] = [
    Stage::Bids,
    Stage::Preprocess,
    Stage::Precoreg,
    Stage::Coregister,
    Stage::Normalize,
    Stage::Brainshift,
    Stage::Reconstruct,
    Stage::Orientation,
    Stage::Vat,
    Stage::Visualize,
];

const WORK: [Stage; 7] = [
    Stage::Preprocess,
    Stage::Coregister,
    Stage::Normalize,
    Stage::Reconstruct,
    Stage::Orientation,
    Stage::Vat,
    Stage::Visualize,
];

impl Stage {
    pub fn default_sequence() -> &'static [Stage] {
        &ALL
    }

    pub fn work_sequence() -> &'static [Stage] {
        &WORK
    }

    pub fn label(self) -> &'static str {
        match self {
            Stage::Bids => "bids",
            Stage::Preprocess => "preprocess",
            Stage::Precoreg => "precoreg",
            Stage::Coregister => "coregister",
            Stage::Normalize => "normalize",
            Stage::Brainshift => "brainshift",
            Stage::Reconstruct => "reconstruct",
            Stage::Orientation => "orientation",
            Stage::Vat => "vat",
            Stage::Visualize => "visualize",
        }
    }

    pub fn cli_name(self) -> &'static str {
        match self {
            Stage::Coregister => "coreg",
            other => other.label(),
        }
    }

    pub fn direct_dependents(self) -> &'static [Stage] {
        match self {
            Stage::Preprocess => &[Stage::Precoreg, Stage::Coregister, Stage::Normalize],
            Stage::Precoreg => &[Stage::Coregister, Stage::Normalize],
            Stage::Coregister => &[Stage::Normalize, Stage::Brainshift, Stage::Reconstruct],
            Stage::Normalize => &[Stage::Brainshift, Stage::Reconstruct],
            Stage::Brainshift => &[Stage::Reconstruct],
            Stage::Reconstruct => &[Stage::Orientation, Stage::Vat, Stage::Visualize],
            Stage::Orientation => &[Stage::Vat, Stage::Visualize],
            Stage::Vat => &[Stage::Visualize],
            Stage::Bids | Stage::Visualize => &[],
        }
    }

    pub fn invalidated_dependents(self) -> Vec<Stage> {
        let mut seen = vec![self];
        let mut i = 0;
        while i < seen.len() {
            for &d in seen[i].direct_dependents() {
                if !seen.contains(&d) {
                    seen.push(d);
                }
            }
            i += 1;
        }
        ALL.iter()
            .copied()
            .filter(|s| *s != self && seen.contains(s))
            .collect()
    }

    pub fn parse(name: &str) -> Option<Stage> {
        let n = name.trim();
        ALL.iter()
            .copied()
            .find(|s| s.cli_name() == n || s.label() == n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_closure_is_every_later_stage_but_bids() {
        let got = Stage::Preprocess.invalidated_dependents();
        let want: Vec<Stage> = ALL[2..].to_vec();
        assert_eq!(got, want);
    }

    #[test]
    fn closure_follows_the_dependents_table() {
        assert_eq!(
            Stage::Coregister.invalidated_dependents(),
            vec![
                Stage::Normalize,
                Stage::Brainshift,
                Stage::Reconstruct,
                Stage::Orientation,
                Stage::Vat,
                Stage::Visualize
            ]
        );
        assert!(Stage::Visualize.invalidated_dependents().is_empty());
        assert!(Stage::Bids.invalidated_dependents().is_empty());
    }
}
