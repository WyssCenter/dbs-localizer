// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Interactive checkreg review **state**, shared by the desktop binary and the browser viewer.
//!
//! `docs/SERVER_CLIENT_PLAN.md` §6.6: the interactive checkreg review (`dbs-localizer checkreg`) is split
//! so the *state* lives in the library — two volumes per slide, a window / level pair, the current
//! slice index and an approve / reject action enum — while the binary keeps the filesystem and
//! process code (writing the method-log approval) and the web client maps the same action to the
//! approval API. This module holds **no** `std::fs`, no `std::process` and no egui, so it compiles
//! on `wasm32`.
//!
//! Wiring status: the desktop mode `src/checkreg_cli.rs` still owns its own `egui` state
//! and is **not** rewired onto this module (it is large and its widgets are entangled with the
//! texture cache); the module is standalone with tests for now.

use crate::error::VizResult;
use dbsloc_checkreg::checkreg_pair::{
    CheckregSlicePlane, GencheckregpairOptions, PreparedCheckregVols,
};

/// The user's verdict on the current slide's registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    Approve,
    Reject,
}

/// `apply_approval` (`checkreg_cli.rs`) / the server's QC endpoint input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewDecision {
    /// Zero-based slide the decision applies to.
    pub slide: usize,
    pub action: ReviewAction,
}

impl ReviewDecision {
    /// `true` for [`ReviewAction::Approve`] — the `ok` field of the method-log approval.
    pub fn approved(&self) -> bool {
        matches!(self.action, ReviewAction::Approve)
    }
}

/// Review state for a checkreg manifest: which slide, which orientation, which slice, and the
/// window / level applied to the moving volume.
pub struct CheckregReviewState {
    slide_index: usize,
    slide_count: usize,
    plane: CheckregSlicePlane,
    slice: usize,
    /// Window centre / width on **raw** moving samples when `raw_window` is set.
    window_center: f32,
    window_width: f32,
    /// `false` = match the batch PNG greyscale (percentile preprocess on the moving volume).
    raw_window: bool,
    vols: Option<PreparedCheckregVols>,
    rgba: Option<(u32, u32, Vec<u8>)>,
}

impl CheckregReviewState {
    /// Default window / level (matches the binary's raw-NIfTI start).
    pub const DEFAULT_WINDOW_CENTER: f32 = 40.0;
    pub const DEFAULT_WINDOW_WIDTH: f32 = 80.0;

    pub fn new(slide_count: usize) -> Self {
        Self {
            slide_index: 0,
            slide_count,
            plane: CheckregSlicePlane::Axial,
            slice: 0,
            window_center: Self::DEFAULT_WINDOW_CENTER,
            window_width: Self::DEFAULT_WINDOW_WIDTH,
            raw_window: true,
            vols: None,
            rgba: None,
        }
    }

    pub fn slide_index(&self) -> usize {
        self.slide_index
    }

    pub fn slide_count(&self) -> usize {
        self.slide_count
    }

    pub fn plane(&self) -> CheckregSlicePlane {
        self.plane
    }

    pub fn has_volumes(&self) -> bool {
        self.vols.is_some()
    }

    /// Replace the loaded slide volumes (the fixed volume is resampled on the moving grid by
    /// [`PreparedCheckregVols`]). Resets the slice to the plane default.
    pub fn set_volumes(&mut self, vols: PreparedCheckregVols) {
        self.vols = Some(vols);
        self.rgba = None;
        self.slice = self.default_slice();
    }

    /// Build volumes from a windowing preset (`preproc` toggles percentile greyscale).
    pub fn load_volumes(
        &mut self,
        moving_path: &std::path::Path,
        fixed_path: &std::path::Path,
        preproc: bool,
    ) -> VizResult<()> {
        let opt = GencheckregpairOptions {
            preproc,
            ..Default::default()
        };
        let vols = dbsloc_checkreg::checkreg_pair::load_prepared_checkreg_volumes(
            moving_path,
            fixed_path,
            &opt,
        )?;
        self.set_volumes(vols);
        Ok(())
    }

    pub fn raw_window(&self) -> bool {
        self.raw_window
    }

    /// `(center, width)` of the moving window.
    pub fn window_level(&self) -> (f32, f32) {
        (self.window_center, self.window_width)
    }

    /// Set the window / level; width is clamped to `>= 1e-3` (matches the binary's slider).
    pub fn set_window_level(&mut self, center: f32, width: f32) {
        let width = width.max(1e-3);
        if self.window_center != center || self.window_width != width {
            self.window_center = center;
            self.window_width = width;
            self.rgba = None;
        }
    }

    /// Inclusive max slice index for the current plane (0 without loaded volumes).
    pub fn max_slice(&self) -> usize {
        match &self.vols {
            Some(v) => v.max_index(self.plane),
            None => 0,
        }
    }

    /// Middle slice for the current plane (0 without loaded volumes).
    pub fn default_slice(&self) -> usize {
        match &self.vols {
            Some(v) => v.default_index(self.plane),
            None => 0,
        }
    }

    pub fn slice(&self) -> usize {
        self.slice
    }

    /// Clamp-and-set the slice for the current plane.
    pub fn set_slice(&mut self, slice: usize) {
        let slice = slice.min(self.max_slice());
        if self.slice != slice {
            self.slice = slice;
            self.rgba = None;
        }
    }

    pub fn next_slice(&mut self) {
        self.set_slice(self.slice.saturating_add(1));
    }

    /// Switch orientation; the slice index is clamped to the new plane's extent.
    pub fn set_plane(&mut self, plane: CheckregSlicePlane) {
        if self.plane != plane {
            self.plane = plane;
            self.rgba = None;
        }
        self.set_slice(self.slice);
    }

    /// RGBA8 slice for the current state; `Ok(None)` until volumes are loaded. The buffer is
    /// cached and rebuilt only after the slice, plane, window / level or volumes changed.
    pub fn current_rgba(&mut self) -> VizResult<Option<(u32, u32, &[u8])>> {
        let Some(vols) = &self.vols else {
            return Ok(None);
        };
        if self.rgba.is_none() {
            self.rgba = Some(vols.rgba_slice_display(
                self.plane,
                self.slice,
                self.raw_window,
                self.window_center,
                self.window_width,
                None,
            )?);
        }
        Ok(self
            .rgba
            .as_ref()
            .map(|(w, h, data)| (*w, *h, data.as_slice())))
    }

    /// Record the verdict for the current slide. The binary maps the result to
    /// `apply_approval`; the web client maps it to the approval API.
    pub fn decide(&self, action: ReviewAction) -> ReviewDecision {
        ReviewDecision {
            slide: self.slide_index,
            action,
        }
    }

    /// Advance to the next slide and drop the previous slide's volumes; `false` when already on
    /// the last one.
    pub fn goto_next_slide(&mut self) -> bool {
        if self.slide_count == 0 || self.slide_index + 1 >= self.slide_count {
            return false;
        }
        self.slide_index += 1;
        self.vols = None;
        self.rgba = None;
        self.slice = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal single-file NIfTI-1 (float32) — enough for `load_prepared_checkreg_volumes`.
    fn make_nifti1_bytes(n0: usize, n1: usize, n2: usize, voxels: &[f32]) -> Vec<u8> {
        assert_eq!(voxels.len(), n0 * n1 * n2);
        let mut h = [0u8; 348];
        macro_rules! wi32 {
            ($off:expr, $v:expr) => {
                h[$off..$off + 4].copy_from_slice(&($v as i32).to_le_bytes())
            };
        }
        macro_rules! wi16 {
            ($off:expr, $v:expr) => {
                h[$off..$off + 2].copy_from_slice(&($v as i16).to_le_bytes())
            };
        }
        macro_rules! wf32 {
            ($off:expr, $v:expr) => {
                h[$off..$off + 4].copy_from_slice(&($v as f32).to_le_bytes())
            };
        }
        wi32!(0, 348);
        wi16!(40, 3);
        wi16!(42, n0);
        wi16!(44, n1);
        wi16!(46, n2);
        wi16!(48, 1);
        wi16!(50, 1);
        wi16!(52, 1);
        wi16!(54, 1);
        wi16!(70, 16);
        wi16!(72, 32);
        wf32!(76, 0.0);
        wf32!(80, 1.0);
        wf32!(84, 1.0);
        wf32!(88, 1.0);
        wf32!(108, 352.0);
        wi16!(254, 1);
        wf32!(280, 1.0);
        wf32!(300, 1.0);
        wf32!(320, 1.0);
        h[344] = b'n';
        h[345] = b'+';
        h[346] = b'1';
        h[347] = 0;
        let mut out = Vec::with_capacity(348 + 4 + voxels.len() * 4);
        out.extend_from_slice(&h);
        out.extend_from_slice(&[0u8; 4]);
        for &v in voxels {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn defaults_and_slide_navigation() {
        let mut st = CheckregReviewState::new(3);
        assert_eq!(st.slide_index(), 0);
        assert_eq!(st.plane(), CheckregSlicePlane::Axial);
        assert_eq!(st.window_level(), (40.0, 80.0));
        assert!(!st.has_volumes());
        st.next_slice();
        assert_eq!(st.slice(), 0);
        assert!(st.goto_next_slide());
        assert!(st.goto_next_slide());
        assert!(!st.goto_next_slide(), "past the last slide");
        assert_eq!(st.slide_index(), 2);
    }

    #[test]
    fn changing_slide_drops_the_previous_volumes() {
        let n = 4usize;
        let voxels: Vec<f32> = (0..(n * n * n)).map(|i| i as f32).collect();
        let p = std::env::temp_dir().join(format!("dbsloc_crv_slide_{}.nii", std::process::id()));
        std::fs::write(&p, make_nifti1_bytes(n, n, n, &voxels)).expect("write fixture");
        let mut st = CheckregReviewState::new(2);
        st.load_volumes(&p, &p, true).expect("load volumes");
        assert!(st.has_volumes());
        assert!(st.goto_next_slide());
        assert!(!st.has_volumes());
        assert!(st.current_rgba().expect("rgba").is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn window_level_width_is_clamped() {
        let mut st = CheckregReviewState::new(1);
        st.set_window_level(100.0, -5.0);
        assert_eq!(st.window_level(), (100.0, 1e-3));
    }

    #[test]
    fn decision_records_action_and_slide() {
        let mut st = CheckregReviewState::new(4);
        st.goto_next_slide();
        let ok = st.decide(ReviewAction::Approve);
        assert_eq!(ok.slide, 1);
        assert!(ok.approved());
        let no = st.decide(ReviewAction::Reject);
        assert!(!no.approved());
    }

    #[test]
    fn volumes_enable_slices_and_render_rgba() {
        let n = 8usize;
        let voxels: Vec<f32> = (0..(n * n * n)).map(|i| i as f32 * 0.02).collect();
        let bytes = make_nifti1_bytes(n, n, n, &voxels);
        let dir = std::env::temp_dir();
        let p = dir.join(format!("dbsloc_crv_{}.nii", std::process::id()));
        std::fs::write(&p, &bytes).expect("write fixture");

        let mut st = CheckregReviewState::new(2);
        st.load_volumes(&p, &p, true).expect("load volumes");
        assert!(st.has_volumes());
        assert_eq!(st.max_slice(), n - 1);
        assert_eq!(st.slice(), n / 2);
        st.set_slice(999);
        assert_eq!(st.slice(), n - 1);
        let (w, h, len, first) = {
            let (w, h, data) = st.current_rgba().expect("rgba").expect("some");
            (w, h, data.len(), data.as_ptr())
        };
        assert!(w > 0 && h > 0);
        assert_eq!(len as u32, w * h * 4);
        let again = st.current_rgba().expect("rgba").expect("some").2.as_ptr();
        assert_eq!(first, again, "unchanged state reuses the cached buffer");
        st.set_slice(0);
        assert!(st.rgba.is_none(), "a slice change invalidates the cache");
        st.set_plane(CheckregSlicePlane::Sagittal);
        assert_eq!(st.max_slice(), n - 1);
        let _ = std::fs::remove_file(&p);
    }
}
