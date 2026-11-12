// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
// SPDX-FileCopyrightText: ANTs developers
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0
//
// Copyright NumFOCUS (Insight Software Consortium)
// Copyright 2026 Wyss Center for Bio and Neuroengineering
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// A fit is one stage of fastwarp-reg's `run_linear_chain_cached` (itself after ANTs'
// `--initial-moving-transform <start> --transform Rigid[...]` or `Affine[...]`: the start in the
// moving initial composite, a fresh transform optimised on top, both collapsed into one affine),
// with its smoothing cache kept across calls instead of made per call. See `docs/THIRD_PARTY_PORTS.md`.

//! Several linear (rigid or affine) fits of one image pair on one device: the images are
//! converted, winsorized and uploaded once, and their smoothed copies are reused by every fit that
//! asks for the same sigma -- the shape of a multi-start, where five coarse fits and two full ones
//! register the same CT to the same anchor, or of a rigid fit followed by an affine one.

use std::sync::atomic::AtomicBool;

use fastwarp_gpu::WgpuBackend;
use fastwarp_reg::linalg::IDENTITY3;
use fastwarp_reg::linear::{
    run_linear_stage_cached, with_center, LinearKind, LinearRecipe, SmoothCache,
};
use fastwarp_reg::mask::MaskVolume;
use fastwarp_reg::transform::{AffineTransform, ParametricTransform};
use fastwarp_reg::{Result, Volume};

/// The prepared pair and everything reused between fits.
pub struct Session {
    pub gpu: WgpuBackend,
    pub fixed: Volume<f64>,
    pub moving: Volume<f64>,
    pub mask: Option<MaskVolume>,
    pub cache: SmoothCache,
}

/// One fit's result: the collapsed affine (start then the fitted transform) and the final MI value.
pub struct Fit {
    pub total: AffineTransform,
    pub metric: Option<f64>,
    pub iterations: Vec<u32>,
}

impl Session {
    /// One `Rigid` or `Affine` stage (`recipe.kind`) from `initial`, its metric sampling only the
    /// fixed points inside `mask` (`None`: everywhere). The fitted transform starts as the
    /// identity centred where `initial` is, as ANTs centres a stage after the moving initial
    /// transform (`run_linear_chain_cached`).
    pub fn fit(
        &mut self,
        recipe: &LinearRecipe,
        initial: AffineTransform,
        mask: Option<MaskVolume>,
        cancel: Option<&AtomicBool>,
    ) -> Result<Fit> {
        let mut recipe = recipe.clone();
        recipe.mask = mask;
        let start = match recipe.kind {
            LinearKind::Rigid => ParametricTransform::euler3d([0.0; 6], initial.center),
            LinearKind::Affine => ParametricTransform::affine(&AffineTransform::new(
                IDENTITY3,
                [0.0; 3],
                initial.center,
            )),
        };
        let fitted = run_linear_stage_cached(
            &self.gpu,
            &recipe,
            &self.fixed,
            &self.moving,
            initial,
            start,
            &mut self.cache,
            cancel,
        )?;
        let collapsed = AffineTransform::compose(&initial, &fitted.affine);
        Ok(Fit {
            total: with_center(&collapsed, fitted.affine.center),
            metric: fitted.final_metric(),
            iterations: fitted.levels.iter().map(|l| l.cg.iterations).collect(),
        })
    }
}
