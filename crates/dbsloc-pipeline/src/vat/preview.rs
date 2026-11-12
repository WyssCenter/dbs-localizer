// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Interactive preview of unsaved settings (`specs/07` §7.8): the same engine and basis cache as
//! the stage, native space only, no result files.

use std::sync::atomic::AtomicBool;

use crate::bids::Subject;
use crate::options::RunOptions;
use crate::vat::engine::{Fail, SideOutcome};
use crate::vat::settings::{ModelKind, Side, Stimulation};
use crate::vat::stage::{Run, VatParams};

/// The native fields of the stimulated hemispheres.
#[derive(Debug, Clone, Default)]
pub struct Preview {
    pub sides: Vec<(Side, SideOutcome)>,
    pub warnings: Vec<String>,
}

/// Computes `stim` for the subject without writing result files; `write_cache` lets a missing or
/// partial basis be stored for the next preview.
pub fn preview(
    sub: &Subject,
    opts: &RunOptions,
    stim: &Stimulation,
    params: &VatParams,
    write_cache: bool,
    cancel: Option<&AtomicBool>,
) -> Result<Preview, Fail> {
    if matches!(stim.model, ModelKind::Fastfield | ModelKind::Ossdbs) {
        return Err(Fail::new(
            "E_VAT_model",
            format!(
                "{} outputs can be imported but not computed; use efield",
                stim.model.slug()
            ),
        ));
    }
    let mut run = Run::open(sub, opts, params, cancel, None)
        .map_err(|e| Fail::new("E_VAT_no_native", e.to_string()))?;
    let (stim, leads) = run.prepare(stim, &stim.label)?;
    let runs = run.compute_runs(&stim, &leads, write_cache)?;
    let mut out = Preview::default();
    for side in Side::ALL {
        let r = &runs[side.index()];
        if let Some(f) = &r.fallback {
            out.warnings.push(format!(
                "no grey-matter mask ({f}); homogeneous white matter"
            ));
        }
        if let Some(o) = &r.outcome {
            out.warnings
                .extend(o.warnings.iter().map(|w| format!("{side}: {w}")));
        }
    }
    for (side, run) in Side::ALL.into_iter().zip(runs) {
        if let Some(o) = run.outcome {
            out.sides.push((side, o));
        }
    }
    Ok(out)
}
