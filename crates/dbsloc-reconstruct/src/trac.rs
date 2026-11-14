// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! TRAC/CORE — trajectory trace plus contact localisation (`specs/05` §4.2, §5).
//!
//! Two stages, matching the spec's split:
//!
//! * **TRAC** walks axial slices from a superior start plane toward the target, taking a damped
//!   centroid of the thresholded artifact in a search window that follows a robust line fit.
//!   This yields the shaft *direction* and a centreline, but says nothing about where along
//!   that line the contacts sit.
//! * **CORE** answers that by matching the intensity profile sampled along the fitted line
//!   against the electrode's own metal comb, so `head` is contact 0 and `tail` is the tail
//!   fiducial level (`specs/09` §2.2) — not the two ends of whatever the trace happened to cover.
//!
//! The previous implementation had neither: it split hemispheres at the median *x* of a `> p10`
//! mask, took one weighted centroid per slice over the whole hemisphere, fitted a line, and
//! reported the centreline *extremes* as `head`/`tail`. On real data that produced a 180 mm
//! head→tail for a 9 mm lead.
//!
//! `specs/05` §4.2 keys the search template off `templates/electrode_contacts/{ct,mr}`; that is
//! Lead-DBS data and is not used here. The comb is derived from the catalogue instead
//! (`ElectrodeGeometry`), which also makes it correct for leads the fixed 4-contact template
//! does not describe.

use ndarray::Array3;

use crate::electrode_catalog::ElectrodeGeometry;
use crate::percentile::percentile_linear;
use crate::ring::ring_in_plane;
use fastwarp_core::{apply_rowmajor, invert_rowmajor, Mat4};

/// Tunables from `specs/05` §4.3. Defaults are the spec's STN/GPi/ViM-class values.
#[derive(Clone, Copy, Debug)]
pub struct TracConfig {
    /// Superior plane where the trace starts, world *z* mm.
    pub start_z_mm: f64,
    /// Inferior plane below which no DBS target lies, world *z* mm.
    pub end_z_mm: f64,
    /// Guided passes after the first; each re-runs the walk against the previous line.
    pub refine_steps: u32,
    /// Half-width in **mm** of the square search window carried between slices.
    ///
    /// `specs/05` §4.3 gives `maskwindow` in voxels, which silently changes meaning with
    /// resolution: 10 voxels is 10 mm on a 1 mm template and 5 mm on the 0.5 mm
    /// `MNI152NLin2009bAsym`. Held in mm here and converted per grid.
    pub mask_window_mm: f64,
    /// Foreground is `mean ∓ tra_std_factor · std` inside the window.
    pub tra_std_factor: f64,
    /// How far in mm a slice's centroid may sit from the predicted point and still be accepted.
    pub plausibility_radius_mm: f64,
    /// Consecutive implausible slices tolerated before the walk stops.
    pub end_tolerance: u32,
    /// Transverse disc radius (mm) the CORE profile searches for metal at each depth.
    pub profile_radius_mm: f64,
    /// Allowed deviation of the measured metal extent from the model's, as a fraction.
    pub metal_extent_tol_frac: f64,
    /// Floor for that allowance in mm, so short leads keep a workable margin.
    pub metal_extent_tol_min_mm: f64,
    /// Semi-axis in world *x* (mm) of the elliptical seed search region. Bone is the trap here:
    /// `lpost` inverts CT (`specs/05` §4.1) so metal reads dark, and that also turns the skull
    /// into a large very dark ring far more extensive than any lead. Keeping the seed inside
    /// plausible brain coordinates excludes it. Measured on `sub-76325`: without this the seed
    /// landed at x ≈ ±72 mm, outside the head.
    ///
    /// The region is an **ellipse**, not a box with these half-extents. At the +8.7 mm start
    /// plane the frontal bone reaches (±44, +58) in template space, which is inside a 45 × 60
    /// box but outside the inscribed ellipse. Measured 2026-09-11 on `sub-15454`, `sub-78754`,
    /// `sub-80206` and `sub-93127`: the box seeded every left lead on that sliver of skull.
    pub seed_abs_x_mm: f64,
    /// Semi-axis in world *y* (mm) of the seed search region.
    pub seed_abs_y_mm: f64,
    /// Largest cross-section in mm² a seed component may have. Anything much larger is anatomy,
    /// not a lead. The artifact is far wider than the 1.3 mm lead, though: on `sub-78754`,
    /// averaged over 5 slices at 0.5 mm, both leads measured 17 mm². A 12 mm² limit rejected
    /// them and left only the skull sliver, which at 11 mm² was compact.
    pub seed_max_area_mm2: f64,
    /// Depth in mm of the slab averaged along the slice axis to find the entry seed. Held in mm
    /// so the seed sees the same stretch of lead whatever the slice spacing.
    pub seed_slab_mm: f64,
    /// How far (mm) the half-maximum distal metal edge lies **beyond** the most-distal contact's
    /// real edge, so `head` is placed that much further up the shaft than the model offset alone.
    ///
    /// Blooming: the metal reads wider and longer than it is, and the CORE profile takes the
    /// brightest voxel on a transverse disc, so the visible tip runs past the contact.
    ///
    /// `0.5` (roadmap 2.25, `specs/05` §4.2). The earlier `1.0` (roadmap 2.21, 2026-09-11) was
    /// calibrated through the old affine CT coreg and a SyN registered on a template the
    /// isotropic resample had shrunk ~0.7 % toward its corner. Re-measured 2026-10-05: on
    /// `sub-76325`, whose contacts resolve as separate peaks in the raw CT, the true level
    /// centres sit 0.6–1.0 mm deeper than the 1.0 mm markers (bloom 0.0–0.6 mm); against the
    /// Lead-Tutor experts the 10-subject mean error per lead is 1.651 / 1.526 / 1.499 /
    /// 1.489 mm at 1.0 / 0.65 / 0.5 / 0.35 mm. A phantom painted without blooming wants `0.0`.
    pub distal_bloom_mm: f64,
    /// Maps this volume's world millimetres to the reference-space millimetres in which
    /// `start_z_mm`, `end_z_mm` and the seed ellipse are defined (`specs/05` §4.3). `None` means
    /// the volume is already in that space.
    pub world_to_reference: Option<Mat4>,
}

impl TracConfig {
    fn reference_point(&self, w: [f64; 3]) -> [f64; 3] {
        match &self.world_to_reference {
            Some(m) => apply_rowmajor(m, w),
            None => w,
        }
    }
}

impl Default for TracConfig {
    fn default() -> Self {
        TracConfig {
            start_z_mm: 8.7,
            end_z_mm: -15.5,
            refine_steps: 1,
            mask_window_mm: 5.0,
            tra_std_factor: 3.0,
            plausibility_radius_mm: 7.5,
            end_tolerance: 10,
            profile_radius_mm: 1.5,
            metal_extent_tol_frac: 0.25,
            metal_extent_tol_min_mm: 2.0,
            seed_abs_x_mm: 45.0,
            seed_abs_y_mm: 60.0,
            seed_max_area_mm2: 40.0,
            seed_slab_mm: 5.0,
            distal_bloom_mm: 0.5,
            world_to_reference: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TracError {
    Hemi,
    /// The z window does not intersect the volume.
    ZWindow,
    /// No artifact found in the start plane for this hemisphere.
    NoEntry,
    /// Fewer accepted slices than a line fit needs.
    ShortCentreline,
    /// The traced line is shorter than the electrode's metal extent.
    ShorterThanElectrode,
    /// No metal region of plausible length along the traced line.
    NoContactMatch,
    /// A metal region was found but it is too short to be this electrode.
    MetalExtentMismatch {
        /// Hundredths of a mm, so the error stays comparable by value.
        measured_mm: i64,
        model_mm: i64,
    },
    Affine,
    SingleLevelElectrode(String),
}

impl std::fmt::Display for TracError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TracError::Hemi => write!(f, "hemi must be 1 (left) or 2 (right)"),
            TracError::ZWindow => write!(
                f,
                "start/end z window does not intersect the volume; check the template space"
            ),
            TracError::NoEntry => write!(
                f,
                "no electrode artifact in the start plane for this hemisphere; if the volume \
                 has bright un-inverted metal, metal_dark is the wrong way round"
            ),
            TracError::ShortCentreline => write!(f, "too few accepted slices to fit a line"),
            TracError::ShorterThanElectrode => write!(
                f,
                "traced centreline is shorter than the electrode metal extent"
            ),
            TracError::NoContactMatch => write!(
                f,
                "no metal region of plausible length along the traced line (CORE)"
            ),
            TracError::MetalExtentMismatch {
                measured_mm,
                model_mm,
            } => write!(
                f,
                "metal extends only {:.2} mm along the traced line, too short for the \
                 {:.2} mm this electrode should show (CORE)",
                *measured_mm as f64 / 100.0,
                *model_mm as f64 / 100.0
            ),
            TracError::Affine => write!(f, "affine is singular"),
            TracError::SingleLevelElectrode(name) => write!(
                f,
                "{name} has a single contact level, so TRAC/CORE cannot place a head and a \
                 distinct tail on it"
            ),
        }
    }
}

impl std::error::Error for TracError {}

/// One side's result. `head` is contact 0, `tail` the tail fiducial level centre (`specs/09` §2.2).
#[derive(Clone, Debug)]
pub struct TracResult {
    pub head: [f64; 3],
    pub tail: [f64; 3],
    pub x: [f64; 3],
    pub y: [f64; 3],
    /// Accepted centreline points in world mm, deepest first.
    pub centreline: Vec<[f64; 3]>,
    /// Metal extent measured along the traced line, mm. A lower bound when the artifact runs
    /// past the end of the trace window, which is the usual case on real post-op CT.
    pub metal_extent_mm: f64,
    /// What the electrode model says that extent should be, mm.
    pub model_metal_extent_mm: f64,
    /// Seed point of the walk in world mm, for diagnostics when a trace goes astray.
    pub entry: [f64; 3],
}

/// Flat `[head(3), tail(3), x(3), y(3)]`, the `FiducialSet` row order.
pub fn trace_fiducials_flat(
    data: &Array3<f64>,
    aff: &Mat4,
    hemi: u8,
    metal_dark: bool,
    electrode: &ElectrodeGeometry,
    cfg: &TracConfig,
) -> Result<[f64; 12], TracError> {
    let r = trace_side(data, aff, hemi, metal_dark, electrode, cfg)?;
    Ok([
        r.head[0], r.head[1], r.head[2], r.tail[0], r.tail[1], r.tail[2], r.x[0], r.x[1], r.x[2],
        r.y[0], r.y[1], r.y[2],
    ])
}

/// TRAC then CORE for one hemisphere.
pub fn trace_side(
    data: &Array3<f64>,
    aff: &Mat4,
    hemi: u8,
    metal_dark: bool,
    electrode: &ElectrodeGeometry,
    cfg: &TracConfig,
) -> Result<TracResult, TracError> {
    if hemi != 1 && hemi != 2 {
        return Err(TracError::Hemi);
    }
    require_levels(electrode)?;
    let grid = Grid::new(data, aff)?;
    let (centreline_vx, entry_vx) = trac_walk(&grid, hemi, metal_dark, cfg)?;
    let centreline: Vec<[f64; 3]> = centreline_vx.iter().map(|p| grid.to_world(*p)).collect();
    let entry = grid.to_world(entry_vx);
    let placed = core_place(&grid, &centreline, metal_dark, electrode, cfg)?;
    let (x, y) = ring_in_plane(placed.head, placed.tail, None, electrode.lead_diameter_mm);
    Ok(TracResult {
        head: placed.head,
        tail: placed.tail,
        x,
        y,
        centreline,
        metal_extent_mm: placed.metal_extent_mm,
        model_metal_extent_mm: placed.model_metal_extent_mm,
        entry,
    })
}

/// CORE alone along a line supplied by the caller, `deep` below the expected tip and `shallow`
/// above the most-proximal level (`specs/05` §5).
///
/// For an axis that did not come from the TRAC walk: a metal-axis refinement of it, or a line
/// fitted to the metal itself when the walk could not seed. Placement, the metal-extent check
/// and `distal_bloom_mm` are exactly those of [`trace_side`].
///
/// The line must be at least as long as the lead's apex-to-proximal-metal length, otherwise
/// `shallow` cannot lie above the most-proximal level and the result is
/// [`TracError::ShorterThanElectrode`].
pub fn core_on_line(
    data: &Array3<f64>,
    aff: &Mat4,
    deep: [f64; 3],
    shallow: [f64; 3],
    metal_dark: bool,
    electrode: &ElectrodeGeometry,
    cfg: &TracConfig,
) -> Result<TracResult, TracError> {
    let len = ((shallow[0] - deep[0]).powi(2)
        + (shallow[1] - deep[1]).powi(2)
        + (shallow[2] - deep[2]).powi(2))
    .sqrt();
    require_levels(electrode)?;
    if len < apex_to_proximal_metal_mm(electrode) {
        return Err(TracError::ShorterThanElectrode);
    }
    let grid = Grid::new(data, aff)?;
    let n = 8usize;
    let centreline: Vec<[f64; 3]> = (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            [
                deep[0] + t * (shallow[0] - deep[0]),
                deep[1] + t * (shallow[1] - deep[1]),
                deep[2] + t * (shallow[2] - deep[2]),
            ]
        })
        .collect();
    let placed = core_place(&grid, &centreline, metal_dark, electrode, cfg)?;
    let (x, y) = ring_in_plane(placed.head, placed.tail, None, electrode.lead_diameter_mm);
    Ok(TracResult {
        head: placed.head,
        tail: placed.tail,
        x,
        y,
        centreline,
        metal_extent_mm: placed.metal_extent_mm,
        model_metal_extent_mm: placed.model_metal_extent_mm,
        entry: shallow,
    })
}

fn require_levels(e: &ElectrodeGeometry) -> Result<(), TracError> {
    if e.n_levels() < 2 {
        return Err(TracError::SingleLevelElectrode(e.elmodel.clone()));
    }
    Ok(())
}

/// Apex to the proximal edge of the most-proximal level along the shaft (`specs/09` §3.1).
pub fn apex_to_proximal_metal_mm(e: &ElectrodeGeometry) -> f64 {
    e.apex_to_level_mm().last().copied().unwrap_or(0.0) + 0.5 * e.contact_length_mm
}

/// Voxel grid with the slice axis resolved, so the walk can be written in `(u, v, s)` terms
/// regardless of which stored axis happens to run superior-inferior.
struct Grid<'a> {
    data: &'a Array3<f64>,
    aff: Mat4,
    inv: Mat4,
    /// Stored axis most parallel to world *z*.
    s_ax: usize,
    /// The two in-plane stored axes.
    u_ax: usize,
    v_ax: usize,
    dims: [usize; 3],
    /// World mm per voxel step along `u`, `v`, `s`.
    spacing: [f64; 3],
}

impl<'a> Grid<'a> {
    fn new(data: &'a Array3<f64>, aff: &Mat4) -> Result<Self, TracError> {
        let (d0, d1, d2) = data.dim();
        let col = |a: usize| {
            (aff[0][a] * aff[0][a] + aff[1][a] * aff[1][a] + aff[2][a] * aff[2][a])
                .sqrt()
                .max(1e-6)
        };
        let mut s_ax = 0usize;
        let mut best = f64::NEG_INFINITY;
        for (a, z) in aff[2][..3].iter().enumerate() {
            let m = z.abs() / col(a);
            if m > best {
                best = m;
                s_ax = a;
            }
        }
        let (u_ax, v_ax) = match s_ax {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        Ok(Grid {
            data,
            aff: *aff,
            inv: invert_rowmajor(aff).ok_or(TracError::Affine)?,
            s_ax,
            u_ax,
            v_ax,
            dims: [d0, d1, d2],
            spacing: [col(u_ax), col(v_ax), col(s_ax)],
        })
    }

    fn n_slices(&self) -> usize {
        self.dims[self.s_ax]
    }

    fn n_u(&self) -> usize {
        self.dims[self.u_ax]
    }

    fn n_v(&self) -> usize {
        self.dims[self.v_ax]
    }

    fn ijk(&self, u: usize, v: usize, s: usize) -> [usize; 3] {
        let mut p = [0usize; 3];
        p[self.u_ax] = u;
        p[self.v_ax] = v;
        p[self.s_ax] = s;
        p
    }

    fn at(&self, u: usize, v: usize, s: usize) -> f64 {
        let p = self.ijk(u, v, s);
        self.data[[p[0], p[1], p[2]]]
    }

    /// `(u, v, s)` in voxel units (may be fractional) → world mm.
    fn to_world(&self, p: [f64; 3]) -> [f64; 3] {
        let mut ijk = [0.0f64; 3];
        ijk[self.u_ax] = p[0];
        ijk[self.v_ax] = p[1];
        ijk[self.s_ax] = p[2];
        apply_rowmajor(&self.aff, ijk)
    }

    fn to_voxel(&self, w: [f64; 3]) -> [f64; 3] {
        apply_rowmajor(&self.inv, w)
    }

    /// Trilinear sample in world space; `None` outside the grid.
    fn sample_world(&self, w: [f64; 3]) -> Option<f64> {
        let p = self.to_voxel(w);
        let mut lo = [0usize; 3];
        let mut fr = [0.0f64; 3];
        for a in 0..3 {
            if !p[a].is_finite() {
                return None;
            }
            let f = p[a].floor();
            if f < 0.0 || f as usize + 1 >= self.dims[a] {
                return None;
            }
            lo[a] = f as usize;
            fr[a] = p[a] - f;
        }
        let mut acc = 0.0;
        for c in 0..8 {
            let mut idx = [0usize; 3];
            let mut w8 = 1.0;
            for a in 0..3 {
                let bit = (c >> a) & 1;
                idx[a] = lo[a] + bit;
                w8 *= if bit == 1 { fr[a] } else { 1.0 - fr[a] };
            }
            acc += w8 * self.data[[idx[0], idx[1], idx[2]]];
        }
        Some(acc)
    }

    /// World *z* at the centre of slice `s`.
    fn slice_world_z(&self, s: usize) -> f64 {
        self.to_world([
            0.5 * (self.n_u() as f64 - 1.0),
            0.5 * (self.n_v() as f64 - 1.0),
            s as f64,
        ])[2]
    }

    /// Fractional slice index at which the reference-space line `(0, 0, z)` crosses the volume,
    /// and whether increasing index moves superiorly in reference space. `None` when the
    /// mapping is singular or the slice axis does not move along reference *z*.
    fn slice_for_reference_z(&self, z: f64, to_ref: &Mat4) -> Option<(f64, bool)> {
        let inv = invert_rowmajor(to_ref)?;
        let at = |zr: f64| self.to_voxel(apply_rowmajor(&inv, [0.0, 0.0, zr]))[self.s_ax];
        let (s0, s1) = (at(0.0), at(1.0));
        let step = s1 - s0;
        if !step.is_finite() || step.abs() < 1e-9 {
            return None;
        }
        Some((at(z), step > 0.0))
    }

    /// Slice index for a start or end plane of `cfg`, in the space `cfg` is defined in.
    fn slice_for_plane(&self, z: f64, cfg: &TracConfig) -> Result<(f64, bool), TracError> {
        match &cfg.world_to_reference {
            None => Ok(self.slice_for_z(z)),
            Some(m) => self.slice_for_reference_z(z, m).ok_or(TracError::Affine),
        }
    }

    /// Slice index nearest world *z*, and whether increasing index moves superiorly.
    fn slice_for_z(&self, z: f64) -> (f64, bool) {
        let z0 = self.slice_world_z(0);
        let z1 = self.slice_world_z(self.n_slices().saturating_sub(1));
        let n = (self.n_slices() as f64 - 1.0).max(1.0);
        let step = (z1 - z0) / n;
        if step.abs() < 1e-9 {
            return (0.0, true);
        }
        ((z - z0) / step, step > 0.0)
    }
}

/// Least-median-squares mean: median, then the mean of values within `3 σ` of it, with `σ`
/// from the MAD. Rejects the interpolated slices the walk inserts when a slice fails.
fn robust_mean(xs: &[f64]) -> f64 {
    const MAD_TO_SIGMA: f64 = 1.4826;
    let v: Vec<f64> = xs.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return f64::NAN;
    }
    if v.len() < 4 {
        return v.iter().sum::<f64>() / v.len() as f64;
    }
    let med = median_of(&v);
    let mut dev: Vec<f64> = v.iter().map(|x| (x - med).abs()).collect();
    let sigma = MAD_TO_SIGMA * median_of_mut(&mut dev);
    if sigma < 1e-12 {
        return med;
    }
    let inl: Vec<f64> = v
        .iter()
        .copied()
        .filter(|x| (x - med).abs() <= 3.0 * sigma)
        .collect();
    if inl.is_empty() {
        med
    } else {
        inl.iter().sum::<f64>() / inl.len() as f64
    }
}

fn median_of(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    median_of_mut(&mut v)
}

fn median_of_mut(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Line through the centreline as `u = u0 + t·du`, `v = v0 + t·dv` with `t` the slice step.
#[derive(Clone, Copy, Debug)]
struct LineFit {
    u0: f64,
    du: f64,
    v0: f64,
    dv: f64,
}

impl LineFit {
    fn at(&self, t: f64) -> (f64, f64) {
        (self.u0 + t * self.du, self.v0 + t * self.dv)
    }
}

/// `specs/05` §4.2 `fit_line`: robust per-step offset, then robust intercept.
fn fit_centreline(pts: &[(f64, f64)]) -> Option<LineFit> {
    if pts.len() < 2 {
        return None;
    }
    let du = robust_mean(
        &pts.windows(2)
            .map(|w| w[1].0 - w[0].0)
            .collect::<Vec<f64>>(),
    );
    let dv = robust_mean(
        &pts.windows(2)
            .map(|w| w[1].1 - w[0].1)
            .collect::<Vec<f64>>(),
    );
    if !du.is_finite() || !dv.is_finite() {
        return None;
    }
    let u0 = robust_mean(
        &pts.iter()
            .enumerate()
            .map(|(t, p)| p.0 - t as f64 * du)
            .collect::<Vec<f64>>(),
    );
    let v0 = robust_mean(
        &pts.iter()
            .enumerate()
            .map(|(t, p)| p.1 - t as f64 * dv)
            .collect::<Vec<f64>>(),
    );
    if !u0.is_finite() || !v0.is_finite() {
        return None;
    }
    Some(LineFit { u0, du, v0, dv })
}

#[derive(Clone, Copy, Debug)]
struct Window {
    u_lo: usize,
    u_hi: usize,
    v_lo: usize,
    v_hi: usize,
}

impl Window {
    fn around(u: f64, v: f64, half: usize, grid: &Grid) -> Window {
        let cu = u.round().max(0.0) as usize;
        let cv = v.round().max(0.0) as usize;
        Window {
            u_lo: cu.saturating_sub(half),
            u_hi: (cu + half).min(grid.n_u().saturating_sub(1)),
            v_lo: cv.saturating_sub(half),
            v_hi: (cv + half).min(grid.n_v().saturating_sub(1)),
        }
    }

    fn contains(&self, u: usize, v: usize) -> bool {
        u >= self.u_lo && u <= self.u_hi && v >= self.v_lo && v <= self.v_hi
    }
}

/// Foreground pixels in `win` on slice `s`, thresholded at `mean ∓ k·std`.
///
/// `k` is relaxed toward zero when nothing crosses the threshold, which is the spec's
/// `add_to_tra_stdfactor`: a window that happens to sit on uniform tissue must still yield the
/// artifact rather than an empty slice.
fn foreground(
    grid: &Grid,
    s: usize,
    win: &Window,
    metal_dark: bool,
    k0: f64,
    slice_avg: usize,
) -> Vec<(usize, usize)> {
    let val = |u: usize, v: usize| -> f64 {
        let n = slice_avg.max(1);
        let mut acc = 0.0;
        let mut cnt = 0.0;
        for d in 0..n {
            let ss = s + d;
            if ss < grid.n_slices() {
                acc += grid.at(u, v, ss);
                cnt += 1.0;
            }
        }
        if cnt == 0.0 {
            f64::NAN
        } else {
            acc / cnt
        }
    };
    let mut cells: Vec<(usize, usize, f64)> = Vec::new();
    for u in win.u_lo..=win.u_hi {
        for v in win.v_lo..=win.v_hi {
            let x = val(u, v);
            if x.is_finite() {
                cells.push((u, v, x));
            }
        }
    }
    let vals: Vec<f64> = cells.iter().map(|c| c.2).collect();
    if vals.len() < 4 {
        return Vec::new();
    }
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let var = vals.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / vals.len() as f64;
    let sd = var.sqrt();
    if sd < 1e-12 {
        return Vec::new();
    }
    let mut k = k0;
    while k >= 0.5 {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for &(u, v, x) in &cells {
            let hit = if metal_dark {
                x < mean - k * sd
            } else {
                x > mean + k * sd
            };
            if hit {
                out.push((u, v));
            }
        }
        if !out.is_empty() {
            return out;
        }
        k -= 0.5;
    }
    Vec::new()
}

/// 8-connected components of `px`, each as (centroid, size).
fn components(px: &[(usize, usize)], win: &Window) -> Vec<((f64, f64), usize)> {
    let nu = win.u_hi + 1 - win.u_lo;
    let nv = win.v_hi + 1 - win.v_lo;
    if nu == 0 || nv == 0 {
        return Vec::new();
    }
    let idx = |u: usize, v: usize| (u - win.u_lo) * nv + (v - win.v_lo);
    let mut is_fg = vec![false; nu * nv];
    for &(u, v) in px {
        if win.contains(u, v) {
            is_fg[idx(u, v)] = true;
        }
    }
    let mut seen = vec![false; nu * nv];
    let mut out = Vec::new();
    for &(u0, v0) in px {
        if !win.contains(u0, v0) || seen[idx(u0, v0)] {
            continue;
        }
        let mut stack = vec![(u0, v0)];
        seen[idx(u0, v0)] = true;
        let mut su = 0.0;
        let mut sv = 0.0;
        let mut n = 0usize;
        while let Some((u, v)) = stack.pop() {
            su += u as f64;
            sv += v as f64;
            n += 1;
            for du in -1i64..=1 {
                for dv in -1i64..=1 {
                    if du == 0 && dv == 0 {
                        continue;
                    }
                    let nu2 = u as i64 + du;
                    let nv2 = v as i64 + dv;
                    if nu2 < win.u_lo as i64
                        || nu2 > win.u_hi as i64
                        || nv2 < win.v_lo as i64
                        || nv2 > win.v_hi as i64
                    {
                        continue;
                    }
                    let (a, b) = (nu2 as usize, nv2 as usize);
                    if is_fg[idx(a, b)] && !seen[idx(a, b)] {
                        seen[idx(a, b)] = true;
                        stack.push((a, b));
                    }
                }
            }
        }
        out.push(((su / n as f64, sv / n as f64), n));
    }
    out
}

/// `specs/05` §4.2 `findonemidpoint`: pick the component nearest the prediction, then move
/// 80% of the way from the prediction to its centroid rather than jumping to it.
const DAMPING: f64 = 0.8;

fn one_midpoint(
    grid: &Grid,
    s: usize,
    win: &Window,
    metal_dark: bool,
    cfg: &TracConfig,
    predict: (f64, f64),
    slice_avg: usize,
) -> Option<(f64, f64)> {
    let px = foreground(grid, s, win, metal_dark, cfg.tra_std_factor, slice_avg);
    if px.is_empty() {
        return None;
    }
    let comps = components(&px, win);
    let (c, _n) = comps.into_iter().min_by(|a, b| {
        let da = (a.0 .0 - predict.0).powi(2) + (a.0 .1 - predict.1).powi(2);
        let db = (b.0 .0 - predict.0).powi(2) + (b.0 .1 - predict.1).powi(2);
        da.total_cmp(&db)
    })?;
    Some((
        predict.0 + (c.0 - predict.0) * DAMPING,
        predict.1 + (c.1 - predict.1) * DAMPING,
    ))
}

fn seed_slab_slices(spacing_mm: f64, slab_mm: f64) -> usize {
    ((slab_mm / spacing_mm).round() as usize).max(1)
}

/// Seed for the walk: the most electrode-shaped artifact in this hemisphere's half of the
/// start plane.
///
/// `specs/05` §4.2 offers manual, auto and a template `masksz`; only auto is meaningful without
/// a UI or per-space guide data. Two things do the discriminating, and both are needed:
/// plausible brain coordinates (bone is dark once CT is inverted, and the skull is enormous),
/// and cross-sectional area (a lead is ~1.3 mm across). Averaging a few slices lifts the shaft
/// out of noise the way the spec's 4-slice mean does. Among the candidates that survive, the
/// one with the most extreme mean intensity wins — metal is denser than anything else present.
///
/// Components are labelled over the whole hemisphere window and filtered afterwards, so a bone
/// sliver cut by the region boundary cannot pass as a compact artifact. When nothing compact
/// lies inside the region there is no seed: the earlier fallback to "any component" is how
/// four of the cohort's left leads came to be seeded on the frontal skull.
fn entry_seed(
    grid: &Grid,
    s: usize,
    hemi: u8,
    metal_dark: bool,
    cfg: &TracConfig,
) -> Option<(f64, f64)> {
    let full = seed_window(grid, s, cfg)?;
    let slices = seed_slab_slices(grid.spacing[2], cfg.seed_slab_mm);
    let px = foreground(grid, s, &full, metal_dark, cfg.tra_std_factor, slices);
    if px.is_empty() {
        return None;
    }
    let want_left = hemi == 1;
    let side_px: Vec<(usize, usize)> = px
        .into_iter()
        .filter(|&(u, v)| {
            let x = cfg.reference_point(grid.to_world([u as f64, v as f64, s as f64]))[0];
            (x < 0.0) == want_left
        })
        .collect();
    if side_px.is_empty() {
        return None;
    }
    let px_area = grid.spacing[0] * grid.spacing[1];
    let max_px = ((cfg.seed_max_area_mm2 / px_area).round() as usize).max(2);
    let score = |c: &(f64, f64)| -> f64 {
        let v = grid.at(
            (c.0.round().max(0.0) as usize).min(grid.n_u() - 1),
            (c.1.round().max(0.0) as usize).min(grid.n_v() - 1),
            s,
        );
        if metal_dark {
            -v
        } else {
            v
        }
    };
    components(&side_px, &full)
        .into_iter()
        .filter(|(c, n)| {
            let w = grid.to_world([c.0, c.1, s as f64]);
            *n <= max_px && in_seed_region(w, cfg)
        })
        .map(|(c, _)| c)
        .max_by(|a, b| score(a).total_cmp(&score(b)))
}

/// True when world `(x, y)` lies inside the elliptical seed region (see `TracConfig`).
fn in_seed_region(w: [f64; 3], cfg: &TracConfig) -> bool {
    let r = cfg.reference_point(w);
    let a = r[0] / cfg.seed_abs_x_mm;
    let b = r[1] / cfg.seed_abs_y_mm;
    a * a + b * b <= 1.0
}

/// Voxel window covering the plausible seed region of slice `s`.
fn seed_window(grid: &Grid, s: usize, cfg: &TracConfig) -> Option<Window> {
    let mut u_lo = usize::MAX;
    let mut u_hi = 0usize;
    let mut v_lo = usize::MAX;
    let mut v_hi = 0usize;
    for u in 0..grid.n_u() {
        for v in 0..grid.n_v() {
            let w = grid.to_world([u as f64, v as f64, s as f64]);
            if in_seed_region(w, cfg) {
                u_lo = u_lo.min(u);
                u_hi = u_hi.max(u);
                v_lo = v_lo.min(v);
                v_hi = v_hi.max(v);
            }
        }
    }
    if u_lo == usize::MAX || u_hi <= u_lo || v_hi <= v_lo {
        return None;
    }
    Some(Window {
        u_lo,
        u_hi,
        v_lo,
        v_hi,
    })
}

/// Walk from the start plane toward the target, returning accepted centreline points as
/// `(u, v, s)` voxel coordinates ordered **deepest first**.
#[allow(clippy::type_complexity)]
fn trac_walk(
    grid: &Grid,
    hemi: u8,
    metal_dark: bool,
    cfg: &TracConfig,
) -> Result<(Vec<[f64; 3]>, [f64; 3]), TracError> {
    let (start_f, index_is_superior) = grid.slice_for_plane(cfg.start_z_mm, cfg)?;
    let (end_f, _) = grid.slice_for_plane(cfg.end_z_mm, cfg)?;
    let n = grid.n_slices();
    if n < 4 {
        return Err(TracError::ZWindow);
    }
    let clamp = |x: f64| x.round().clamp(0.0, n as f64 - 1.0) as usize;
    let start = clamp(start_f);
    let end = clamp(end_f);
    let step: i64 = if index_is_superior { -1 } else { 1 };
    let n_steps = (start as i64 - end as i64).abs();
    if n_steps < 4 {
        return Err(TracError::ZWindow);
    }

    let mut prior: Option<Vec<(f64, f64)>> = None;
    let mut accepted: Vec<[f64; 3]> = Vec::new();
    let plane_mm = 0.5 * (grid.spacing[0] + grid.spacing[1]);
    let half = ((cfg.mask_window_mm / plane_mm).round() as usize).max(2);
    let mut entry = [0.0f64; 3];

    for refine in 0..=cfg.refine_steps {
        let radius =
            (cfg.plausibility_radius_mm / plane_mm) * (1.0 - 0.1 * (refine as f64).min(3.0));
        let seed = match prior.as_ref() {
            Some(p) => p[0],
            None => entry_seed(grid, start, hemi, metal_dark, cfg).ok_or(TracError::NoEntry)?,
        };
        if refine == 0 {
            entry = [seed.0, seed.1, start as f64];
        }
        let mut centre: Vec<(f64, f64)> = vec![seed];
        let mut pts: Vec<[f64; 3]> = vec![[seed.0, seed.1, start as f64]];
        let mut fit: Option<LineFit> = None;
        let mut fails = 0u32;

        for t in 1..=n_steps {
            let s_i = start as i64 + step * t;
            if s_i < 0 || s_i >= n as i64 {
                break;
            }
            let s = s_i as usize;
            let predict = match (prior.as_ref(), fit) {
                (Some(p), _) if (t as usize) < p.len() => p[t as usize],
                (_, Some(f)) => f.at(t as f64),
                _ => centre.last().copied().unwrap_or(seed),
            };
            let win = Window::around(predict.0, predict.1, half, grid);
            let found = one_midpoint(grid, s, &win, metal_dark, cfg, predict, 1);
            let taken = match found {
                Some(p) => {
                    let d = ((p.0 - predict.0).powi(2) + (p.1 - predict.1).powi(2)).sqrt();
                    if d < radius {
                        fails = 0;
                        p
                    } else {
                        fails += 1;
                        predict
                    }
                }
                None => {
                    fails += 1;
                    predict
                }
            };
            centre.push(taken);
            pts.push([taken.0, taken.1, s as f64]);
            fit = fit_centreline(&centre);
            if fails >= cfg.end_tolerance {
                break;
            }
        }

        if centre.len() < 4 {
            return Err(TracError::ShortCentreline);
        }
        let f = fit_centreline(&centre).ok_or(TracError::ShortCentreline)?;
        let smoothed: Vec<[f64; 3]> = (0..centre.len())
            .map(|t| {
                let (u, v) = f.at(t as f64);
                [u, v, pts[t][2]]
            })
            .collect();
        prior = Some((0..centre.len()).map(|t| f.at(t as f64)).collect());
        accepted = smoothed;
    }

    accepted.reverse();
    Ok((accepted, entry))
}

/// Axial extent of metal in the model frame, one interval per level.
///
/// The most-distal level is widened to the tip length when the tip *is* the contact
/// (`specs/09` §2, `tipiscontact`), because then the metal reaches the apex.
fn metal_intervals(e: &ElectrodeGeometry) -> Vec<(f64, f64)> {
    let cl = e.contact_length_mm;
    e.level_z_mm
        .iter()
        .enumerate()
        .map(|(k, &z)| {
            if k == 0 && e.tip_is_contact {
                let half = 0.5 * e.tip_length_mm.max(cl);
                let apex = z - 0.5 * e.tip_length_mm;
                ((z - half).max(apex), z + half)
            } else {
                (z - 0.5 * cl, z + 0.5 * cl)
            }
        })
        .collect()
}

/// Model-frame `z` of the distal edge of the most-distal level's metal, and the full metal
/// extent from that edge to the proximal edge of the last level.
fn metal_bounds(e: &ElectrodeGeometry) -> (f64, f64) {
    let iv = metal_intervals(e);
    let lo = iv.first().map(|i| i.0).unwrap_or(0.0);
    let hi = iv.last().map(|i| i.1).unwrap_or(0.0);
    (lo, hi - lo)
}

/// Widest insulator gap between adjacent levels; the closing width that lets a resolved comb
/// read as one metal region.
fn max_level_pitch(e: &ElectrodeGeometry) -> f64 {
    e.level_z_mm
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(0.0f64, f64::max)
        .max(e.eldist_mm)
}

/// Most metal-like intensity on a transverse disc of radius `radius_mm`. `sign` is `-1` when
/// metal reads dark, so the returned value always has metal as its maximum.
///
/// Max, not mean, and over a disc wider than the lead. The fitted line is only constrained
/// where there is signal, so it extrapolates past the tip with a growing lateral error —
/// measured at 1.0–1.7 mm on the 8-ring phantom, more than the 0.68 mm shaft radius. A mean
/// over a narrow disc then samples tissue beside the lead and reads the metal as absent, which
/// shortened the measured extent to 9.5 of 15.5 mm and put `head` 3.2 mm too deep. Taking the
/// extremum over a disc that covers the plausible offset is insensitive to it, and matches
/// what is being asked: whether there is metal at this depth at all.
fn profile_signed(
    grid: &Grid,
    p: [f64; 3],
    u_hat: [f64; 3],
    radius_mm: f64,
    sign: f64,
) -> Option<f64> {
    let (e1, e2) = perp_basis(u_hat);
    let mut best = f64::NEG_INFINITY;
    let probe = |w: [f64; 3], best: &mut f64| {
        if let Some(v) = grid.sample_world(w) {
            let f = sign * v;
            if f > *best {
                *best = f;
            }
        }
    };
    probe(p, &mut best);
    const RINGS: usize = 3;
    const SPOKES: usize = 8;
    for ring in 1..=RINGS {
        let r = radius_mm * ring as f64 / RINGS as f64;
        for spoke in 0..SPOKES {
            let th = std::f64::consts::TAU * spoke as f64 / SPOKES as f64;
            let (c, s) = (th.cos(), th.sin());
            let w = [
                p[0] + r * (c * e1[0] + s * e2[0]),
                p[1] + r * (c * e1[1] + s * e2[1]),
                p[2] + r * (c * e1[2] + s * e2[2]),
            ];
            probe(w, &mut best);
        }
    }
    if best.is_finite() {
        Some(best)
    } else {
        None
    }
}

fn perp_basis(u: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let aux = if u[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let d = u[0] * aux[0] + u[1] * aux[1] + u[2] * aux[2];
    let mut e1 = [aux[0] - d * u[0], aux[1] - d * u[1], aux[2] - d * u[2]];
    let n1 = (e1[0] * e1[0] + e1[1] * e1[1] + e1[2] * e1[2]).sqrt();
    for c in e1.iter_mut() {
        *c /= n1.max(1e-12);
    }
    let e2 = [
        u[1] * e1[2] - u[2] * e1[1],
        u[2] * e1[0] - u[0] * e1[2],
        u[0] * e1[1] - u[1] * e1[0],
    ];
    (e1, e2)
}

/// Half-maximum crossing between samples `i-1` and `i`, in sample units.
fn crossing(prof: &[f64], i: usize, thr: f64) -> f64 {
    if i == 0 || prof.is_empty() {
        return 0.0;
    }
    if i >= prof.len() {
        return (prof.len() - 1) as f64;
    }
    let (a, b) = (prof[i - 1], prof[i]);
    if (b - a).abs() < 1e-12 {
        return i as f64;
    }
    (i - 1) as f64 + ((thr - a) / (b - a)).clamp(0.0, 1.0)
}

/// Distal and proximal ends of the lead's metal along the profile, in sample units, after
/// closing insulator gaps up to `bridge` samples. The proximal end may be the final sample,
/// which means the metal continues past the traced line.
fn metal_edges(prof: &[f64], thr: f64, bridge: usize, min_len: usize) -> Option<(usize, usize)> {
    let above: Vec<bool> = prof.iter().map(|&v| v > thr).collect();
    let n = above.len();
    let mut i = 0usize;
    while i < n {
        if !above[i] {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut j = i;
        while j < n {
            if above[j] {
                end = j;
                j += 1;
            } else {
                let mut gap = j;
                while gap < n && !above[gap] {
                    gap += 1;
                }
                if gap < n && gap - j <= bridge {
                    j = gap;
                } else {
                    break;
                }
            }
        }
        if end > start && end - start + 1 >= min_len && start > 0 {
            return Some((start, end));
        }
        i = end + 1;
    }
    None
}

/// Locate the lead along the fitted line and place `head`/`tail` from the catalogue.
///
/// `specs/05` §4.2 cross-correlates against `templates/electrode_contacts/{ct,mr}` — Lead-DBS
/// data, not used here. The first attempt built the equivalent template from the catalogue as a
/// comb of contact boxcars, which is right in principle and wrong in practice: measured on
/// `sub-76325`, a real normalised post-op CT samples the lead as **one continuous 11 mm plateau**
/// of 1200–2000 HU, not four resolvable contacts, because blooming around metal is wider than
/// the 1.5 mm insulator gaps. Correlating a comb against a plateau maximises where the comb
/// straddles the plateau's *edge*, which put contact 0 about a full span too deep.
///
/// What survives in both regimes is the metal's **distal edge** and its **length**, so that is
/// what this measures: half-maximum crossings of the metal region (insulator gaps closed, so a
/// resolved comb still reads as one region), then contact 0 placed at its known offset from that
/// edge. The measured length is checked against the model's, which is a far stronger correctness
/// signal than a correlation coefficient — on `sub-76325` it reads 11.0 mm against a model 10.5 mm.
fn core_place(
    grid: &Grid,
    centreline: &[[f64; 3]],
    metal_dark: bool,
    e: &ElectrodeGeometry,
    cfg: &TracConfig,
) -> Result<CorePlacement, TracError> {
    if centreline.len() < 4 {
        return Err(TracError::ShortCentreline);
    }
    let deep = centreline[0];
    let shallow = centreline[centreline.len() - 1];
    let mut u_hat = [
        shallow[0] - deep[0],
        shallow[1] - deep[1],
        shallow[2] - deep[2],
    ];
    let len = (u_hat[0] * u_hat[0] + u_hat[1] * u_hat[1] + u_hat[2] * u_hat[2]).sqrt();
    if len < 1e-6 {
        return Err(TracError::ShortCentreline);
    }
    for c in u_hat.iter_mut() {
        *c /= len;
    }

    let (edge_z, metal_mm) = metal_bounds(e);
    let step = 0.1f64;
    let pre_mm = 6.0;
    let total_mm = len + pre_mm;
    if total_mm < metal_mm {
        return Err(TracError::ShorterThanElectrode);
    }
    let n_prof = ((total_mm / step).round() as usize).max(8);
    let radius = cfg.profile_radius_mm.max(0.5 * e.lead_diameter_mm);
    let sign = if metal_dark { -1.0 } else { 1.0 };
    let mut prof: Vec<f64> = Vec::with_capacity(n_prof);
    for i in 0..n_prof {
        let a = -pre_mm + i as f64 * step;
        let p = [
            deep[0] + u_hat[0] * a,
            deep[1] + u_hat[1] * a,
            deep[2] + u_hat[2] * a,
        ];
        prof.push(profile_signed(grid, p, u_hat, radius, sign).unwrap_or(f64::NAN));
    }
    let finite: Vec<f64> = prof.iter().copied().filter(|x| x.is_finite()).collect();
    if finite.len() < 8 {
        return Err(TracError::NoContactMatch);
    }
    let bg = percentile_linear(&finite, 20.0);
    for x in prof.iter_mut() {
        if !x.is_finite() {
            *x = bg;
        }
    }
    let pk = prof.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !(pk - bg).is_finite() || pk - bg < 1e-6 {
        return Err(TracError::NoContactMatch);
    }
    let plateau = {
        let hi: Vec<f64> = prof
            .iter()
            .copied()
            .filter(|&v| v > bg + 0.5 * (pk - bg))
            .collect();
        if hi.len() < 4 {
            pk
        } else {
            median_of(&hi)
        }
    };
    let thr = bg + 0.5 * (plateau - bg);
    let bridge = (((max_level_pitch(e) + e.contact_length_mm) / step).round() as usize).max(1);
    let min_len = ((0.5 * metal_mm / step).round() as usize).max(2);
    let (i_lo, i_hi) = metal_edges(&prof, thr, bridge, min_len).ok_or(TracError::NoContactMatch)?;

    let a_lo = -pre_mm + crossing(&prof, i_lo, thr) * step;
    let a_hi = -pre_mm + crossing(&prof, i_hi + 1, thr) * step;
    let measured = a_hi - a_lo;
    let tol = (cfg.metal_extent_tol_frac * metal_mm).max(cfg.metal_extent_tol_min_mm);
    if measured < metal_mm - tol {
        return Err(TracError::MetalExtentMismatch {
            measured_mm: (measured * 100.0).round() as i64,
            model_mm: (metal_mm * 100.0).round() as i64,
        });
    }

    let a_head = a_lo - edge_z + cfg.distal_bloom_mm;
    let head = [
        deep[0] + u_hat[0] * a_head,
        deep[1] + u_hat[1] * a_head,
        deep[2] + u_hat[2] * a_head,
    ];
    let span = e.fiducial_span_mm();
    let tail = [
        head[0] + u_hat[0] * span,
        head[1] + u_hat[1] * span,
        head[2] + u_hat[2] * span,
    ];
    Ok(CorePlacement {
        head,
        tail,
        metal_extent_mm: measured,
        model_metal_extent_mm: metal_mm,
    })
}

struct CorePlacement {
    head: [f64; 3],
    tail: [f64; 3],
    metal_extent_mm: f64,
    model_metal_extent_mm: f64,
}

#[cfg(test)]
mod tests {
    use super::{metal_intervals, seed_slab_slices, Grid};
    use crate::electrode_catalog::{by_name, model_names};

    #[test]
    fn tip_contact_metal_starts_at_the_apex() {
        for name in model_names() {
            let e = by_name(name).expect("catalogued");
            let iv = metal_intervals(e);
            let apex = -e.apex_to_first_level_mm();
            assert!(
                iv[0].0 >= apex - 1e-12,
                "{name}: {:?} below apex {apex}",
                iv[0]
            );
        }
        let boston = by_name("Boston Scientific Vercise Directed").expect("catalogued");
        let iv = metal_intervals(boston);
        let h = 0.5 * boston.tip_length_mm.max(boston.contact_length_mm);
        assert_eq!(iv[0], (-h, h));
    }

    #[test]
    fn single_level_electrodes_are_refused() {
        let e = by_name("FHC WU Rat Electrode").expect("catalogued");
        let data = ndarray::Array3::<f64>::zeros((8, 8, 8));
        let aff = fastwarp_core::identity_mat4();
        let got = super::core_on_line(
            &data,
            &aff,
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 7.0],
            false,
            e,
            &super::TracConfig::default(),
        );
        assert!(
            matches!(got, Err(super::TracError::SingleLevelElectrode(_))),
            "{got:?}"
        );
    }

    #[test]
    fn slice_axis_is_chosen_on_direction_not_raw_spacing() {
        let data = ndarray::Array3::<f64>::zeros((4, 4, 4));
        let (c, s) = (20f64.to_radians().cos(), 20f64.to_radians().sin());
        let aff: fastwarp_core::Mat4 = [
            [0.4, 0.0, 0.0, 0.0],
            [0.0, 3.0 * c, -0.5 * s, 0.0],
            [0.0, 3.0 * s, 0.5 * c, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let g = Grid::new(&data, &aff).expect("grid");
        assert_eq!(g.s_ax, 2);
        let axial: fastwarp_core::Mat4 = [
            [-0.45, 0.0, 0.0, 0.0],
            [0.0, 0.45, 0.0, 0.0],
            [0.0, 0.0, 1.25, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(Grid::new(&data, &axial).expect("grid").s_ax, 2);
    }

    #[test]
    fn seed_slab_is_a_depth_in_millimetres() {
        assert_eq!(seed_slab_slices(1.0, 5.0), 5);
        assert_eq!(seed_slab_slices(1.2, 5.0), 4);
        assert_eq!(seed_slab_slices(1.5, 5.0), 3);
        assert_eq!(seed_slab_slices(0.5, 5.0), 10);
        assert_eq!(seed_slab_slices(3.0, 5.0), 2);
        assert_eq!(seed_slab_slices(20.0, 5.0), 1);
    }
}
