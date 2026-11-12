// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Structural seed maps (`specs/15` §5): seed construction, fibre selection and map accumulation.

use std::sync::atomic::{AtomicBool, Ordering};

use dbsloc_connectome::{FibreIndex, FibreSet};
use rayon::prelude::*;

use super::nii::{
    apply, covering_box, crop_to_content, diagonal_affine, invert_affine, resample_box,
    voxel_sizes, Affine, Interp, NiiVolume,
};
use super::{check_cancel, report, MapError, MapResult, ProgressFn};

const TOUCH_EPS_MM: f64 = 1e-6;
const SEED_RIM_MM: f64 = 2.0;
const ACCUMULATE_CHUNK: usize = 4096;

/// Which stimulation image seeds the map (`specs/15` §5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedKind {
    Binary,
    Efield,
    EfieldGauss,
}

impl SeedKind {
    /// The name in file names and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            SeedKind::Binary => "binary",
            SeedKind::Efield => "efield",
            SeedKind::EfieldGauss => "efieldgauss",
        }
    }

    pub fn parse(text: &str) -> Option<SeedKind> {
        match text.trim() {
            "binary" => Some(SeedKind::Binary),
            "efield" => Some(SeedKind::Efield),
            "efieldgauss" => Some(SeedKind::EfieldGauss),
            _ => None,
        }
    }

    pub fn interp(self) -> Interp {
        match self {
            SeedKind::Binary => Interp::Nearest,
            _ => Interp::Trilinear,
        }
    }
}

/// The structural output grids of `specs/15` §5.4 for `MNI152NLin2009bAsym`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputGrid {
    Mm2,
    Mm1,
    Mm05,
}

impl OutputGrid {
    pub fn parse(text: &str) -> Option<OutputGrid> {
        match text.trim() {
            "2mm" => Some(OutputGrid::Mm2),
            "1mm" => Some(OutputGrid::Mm1),
            "0.5mm" => Some(OutputGrid::Mm05),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            OutputGrid::Mm2 => "2mm",
            OutputGrid::Mm1 => "1mm",
            OutputGrid::Mm05 => "0.5mm",
        }
    }

    /// Voxel dimensions and the 0-based voxel-to-mm affine.
    pub fn spec(self) -> ([usize; 3], Affine) {
        match self {
            OutputGrid::Mm2 => (
                [91, 109, 91],
                diagonal_affine([2.0; 3], [-90.0, -126.0, -72.0]),
            ),
            OutputGrid::Mm1 => (
                [182, 218, 182],
                diagonal_affine([1.0; 3], [-90.0, -126.0, -72.0]),
            ),
            OutputGrid::Mm05 => (
                [394, 466, 378],
                diagonal_affine([0.5; 3], [-98.0, -134.0, -72.0]),
            ),
        }
    }
}

/// The seed grid `bb` of `specs/15` §5.1: 0.22 mm, 492 x 492 x 265 voxels.
pub fn seed_grid() -> ([usize; 3], Affine) {
    (
        [492, 492, 265],
        diagonal_affine([0.22; 3], [-53.0, -63.0, -25.0]),
    )
}

/// Builds the seed of `specs/15` §5.1 steps 1 to 3 from the per-side images: each side is
/// resampled onto the seed grid (nearest neighbour for `Binary`, trilinear otherwise, outside and
/// NaN = 0), the sides are summed and the sum is cropped to its content plus a 2 mm rim.
pub fn build_seed(sides: &[&NiiVolume], kind: SeedKind) -> MapResult<NiiVolume> {
    let (dims, affine) = seed_grid();
    let mut lo = [usize::MAX; 3];
    let mut hi = [0usize; 3];
    for side in sides {
        if let Some((l, h)) = covering_box(side, &affine, dims) {
            for a in 0..3 {
                lo[a] = lo[a].min(l[a]);
                hi[a] = hi[a].max(h[a]);
            }
        }
    }
    if sides.is_empty() || lo[0] == usize::MAX {
        return Err(MapError::SeedEmpty(
            "no stimulation image overlaps the seed grid".into(),
        ));
    }
    let mut sum: Option<NiiVolume> = None;
    for side in sides {
        let r = resample_box(side, &affine, lo, hi, kind.interp())?;
        match &mut sum {
            None => sum = Some(r),
            Some(s) => s.data.iter_mut().zip(&r.data).for_each(|(a, b)| *a += *b),
        }
    }
    let sum = sum.ok_or_else(|| MapError::SeedEmpty("no stimulation image".into()))?;
    let mean = voxel_sizes(&sum.affine).iter().sum::<f64>() / 3.0;
    let rim = (SEED_RIM_MM / mean).round().max(0.0) as usize;
    crop_to_content(&sum, rim)
        .ok_or_else(|| MapError::SeedEmpty("the stimulation seed is empty".into()))
}

/// The right and left halves of a seed (`specs/15` §5.1 step 4): voxels centred at `x >= 0` and
/// `x <= 0`; a voxel on `x = 0` stays in both.
pub fn split_hemispheres(seed: &NiiVolume) -> (NiiVolume, NiiVolume) {
    let mut right = seed.clone();
    let mut left = seed.clone();
    for n in 0..seed.len() {
        let x = seed.centre(n)[0];
        if x < -1e-9 {
            right.data[n] = 0.0;
        }
        if x > 1e-9 {
            left.data[n] = 0.0;
        }
    }
    (right, left)
}

/// The fibres a seed connects (`specs/15` §5.2).
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    /// Ascending 0-based ids of the connected fibres.
    pub fibre_ids: Vec<u32>,
    /// `s_f` of each connected fibre.
    pub strengths: Vec<f32>,
    /// Chebyshev search radius `(h_x + h_y + h_z) / 6` in mm.
    pub radius_mm: f64,
    /// Number of non-zero finite seed voxels.
    pub seed_voxels: usize,
}

/// The Chebyshev search radius of a seed image.
pub fn search_radius(seed_affine: &Affine) -> f64 {
    voxel_sizes(seed_affine).iter().sum::<f64>() / 6.0
}

struct SeedLookup<'a> {
    seed: &'a NiiVolume,
    inv: Affine,
    radius: f64,
    extent: [f64; 3],
}

impl SeedLookup<'_> {
    fn touched(&self, points: &[[f32; 3]], scratch: &mut Vec<u32>) -> Option<f32> {
        scratch.clear();
        let [nx, ny, nz] = self.seed.dims;
        let lim = [nx as i64 - 1, ny as i64 - 1, nz as i64 - 1];
        for p in points {
            let w = [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])];
            let u = apply(&self.inv, w);
            let mut lo = [0i64; 3];
            let mut hi = [0i64; 3];
            for a in 0..3 {
                lo[a] = ((u[a] - self.extent[a] - TOUCH_EPS_MM).ceil() as i64).max(0);
                hi[a] = ((u[a] + self.extent[a] + TOUCH_EPS_MM).floor() as i64).min(lim[a]);
            }
            for k in lo[2]..=hi[2] {
                for j in lo[1]..=hi[1] {
                    for i in lo[0]..=hi[0] {
                        let n = self.seed.index(i as usize, j as usize, k as usize);
                        let v = self.seed.data[n];
                        if !v.is_finite() || v == 0.0 {
                            continue;
                        }
                        let c = apply(&self.seed.affine, [i as f64, j as f64, k as f64]);
                        let d = (c[0] - w[0])
                            .abs()
                            .max((c[1] - w[1]).abs())
                            .max((c[2] - w[2]).abs());
                        if d <= self.radius + TOUCH_EPS_MM {
                            scratch.push(n as u32);
                        }
                    }
                }
            }
        }
        if scratch.is_empty() {
            return None;
        }
        scratch.sort_unstable();
        scratch.dedup();
        let sum: f64 = scratch
            .iter()
            .map(|n| f64::from(self.seed.data[*n as usize]))
            .sum();
        let s = (sum / scratch.len() as f64) as f32;
        (s != 0.0 && s.is_finite()).then_some(s)
    }
}

/// The box (mm) that holds every fibre point able to touch a seed voxel: the non-zero voxel
/// centres grown by the search radius; `None` for an empty seed.
pub fn seed_search_box(seed: &NiiVolume) -> Option<([f64; 3], [f64; 3])> {
    let margin = search_radius(&seed.affine) + 1e-3;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for (n, v) in seed.data.iter().enumerate() {
        if v.is_finite() && *v != 0.0 {
            let c = seed.centre(n);
            for a in 0..3 {
                lo[a] = lo[a].min(c[a] - margin);
                hi[a] = hi[a].max(c[a] + margin);
            }
            any = true;
        }
    }
    any.then_some((lo, hi))
}

/// Selects the fibres touched by the non-zero voxels of `seed` and their strengths
/// (`specs/15` §5.2). Candidates come from `index` (built from `set`); the touch test is exact:
/// a fibre point within the Chebyshev radius of a seed voxel centre, boundary inclusive.
pub fn select_fibres(
    set: &FibreSet,
    index: &FibreIndex,
    seed: &NiiVolume,
    cancel: &AtomicBool,
) -> MapResult<Selection> {
    let radius = search_radius(&seed.affine);
    let inv = invert_affine(&seed.affine)
        .ok_or_else(|| MapError::Format("seed affine is singular".into()))?;
    let mut extent = [0.0; 3];
    for (a, e) in extent.iter_mut().enumerate() {
        *e = radius * (inv[a][0].abs() + inv[a][1].abs() + inv[a][2].abs());
    }
    let cell = f64::from(index.voxel_mm());
    let mut cells: Vec<[i32; 3]> = Vec::new();
    let mut seed_voxels = 0usize;
    for (n, v) in seed.data.iter().enumerate() {
        if !v.is_finite() || *v == 0.0 {
            continue;
        }
        seed_voxels += 1;
        let c = seed.centre(n);
        let lo = c.map(|x| ((x - radius - TOUCH_EPS_MM) / cell).floor() as i32);
        let hi = c.map(|x| ((x + radius + TOUCH_EPS_MM) / cell).floor() as i32);
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    cells.push([x, y, z]);
                }
            }
        }
        if seed_voxels.is_multiple_of(65_536) {
            check_cancel(cancel)?;
        }
    }
    cells.sort_unstable();
    cells.dedup();
    let candidates = index.fibres_through(cells.into_iter());
    let lookup = SeedLookup {
        seed,
        inv,
        radius,
        extent,
    };
    let hits: Vec<(u32, f32)> = candidates
        .par_iter()
        .map_init(Vec::new, |scratch, f| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            lookup
                .touched(set.fibre(*f as usize), scratch)
                .map(|s| (*f, s))
        })
        .flatten()
        .collect();
    check_cancel(cancel)?;
    Ok(Selection {
        fibre_ids: hits.iter().map(|h| h.0).collect(),
        strengths: hits.iter().map(|h| h.1).collect(),
        radius_mm: radius,
        seed_voxels,
    })
}

/// Accumulates the strengths of the selected fibres on the grid (`specs/15` §5.3): every fibre
/// adds `s_f` once to each distinct voxel `floor(A^-1 p + 0.5)` its points visit; points outside
/// the grid are dropped.
pub fn accumulate_map(
    set: &FibreSet,
    selection: &Selection,
    dims: [usize; 3],
    affine: &Affine,
    cancel: &AtomicBool,
) -> MapResult<NiiVolume> {
    let inv = invert_affine(affine)
        .ok_or_else(|| MapError::Format("output grid affine is singular".into()))?;
    let total = dims[0]
        .checked_mul(dims[1])
        .and_then(|v| v.checked_mul(dims[2]))
        .filter(|t| *t <= u32::MAX as usize)
        .ok_or_else(|| MapError::Memory("output grid is too large".into()))?;
    let mut map = NiiVolume {
        dims,
        affine: *affine,
        data: vec![0.0; total],
    };
    let n = selection.fibre_ids.len();
    let mut start = 0;
    while start < n {
        check_cancel(cancel)?;
        let end = (start + ACCUMULATE_CHUNK).min(n);
        let voxel_lists: Vec<Vec<u32>> = (start..end)
            .into_par_iter()
            .map(|i| {
                let mut out: Vec<u32> = Vec::new();
                for p in set.fibre(selection.fibre_ids[i] as usize) {
                    let u = apply(&inv, [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
                        .map(|c| (c + 0.5).floor());
                    if u.iter()
                        .zip(dims)
                        .any(|(c, d)| !(*c >= 0.0 && *c < d as f64))
                    {
                        continue;
                    }
                    let (i, j, k) = (u[0] as usize, u[1] as usize, u[2] as usize);
                    out.push(((k * dims[1] + j) * dims[0] + i) as u32);
                }
                out.sort_unstable();
                out.dedup();
                out
            })
            .collect();
        for (offset, list) in voxel_lists.iter().enumerate() {
            let s = selection.strengths[start + offset];
            for v in list {
                map.data[*v as usize] += s;
            }
        }
        start = end;
    }
    Ok(map)
}

/// A structural seed map and the fibres behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct StructuralMap {
    pub selection: Selection,
    pub map: NiiVolume,
}

/// Runs `specs/15` §5.2 and §5.3 for one seed. `None` when no fibre is connected (the caller
/// reports `skipped`, no map is written).
pub fn structural_map(
    set: &FibreSet,
    index: &FibreIndex,
    seed: &NiiVolume,
    grid_dims: [usize; 3],
    grid_affine: &Affine,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> MapResult<Option<StructuralMap>> {
    report(progress, "select", 0, 2, "selecting fibres");
    let selection = select_fibres(set, index, seed, cancel)?;
    if selection.fibre_ids.is_empty() {
        return Ok(None);
    }
    report(progress, "accumulate", 1, 2, "accumulating the map");
    let map = accumulate_map(set, &selection, grid_dims, grid_affine, cancel)?;
    report(progress, "accumulate", 2, 2, "map done");
    Ok(Some(StructuralMap { selection, map }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::connectome_maps::no_progress;

    fn line(y: f32, z: f32) -> Vec<[f32; 3]> {
        (-12..=12).map(|i| [i as f32 * 0.5, y, z]).collect()
    }

    pub(crate) fn cube_seed() -> NiiVolume {
        let mut v = NiiVolume::zeros([3, 3, 3], diagonal_affine([1.0; 3], [-1.0; 3]));
        v.data.fill(1.0);
        v
    }

    pub(crate) fn ten_fibres() -> FibreSet {
        let mut fibres = vec![
            line(0.0, 0.0),
            line(1.5, 0.0),
            line(0.0, 0.2),
            line(1.6, 0.0),
        ];
        for i in 0..6 {
            fibres.push(line(20.0 + i as f32, 30.0));
        }
        FibreSet::from_fibres(&fibres).unwrap()
    }

    #[test]
    fn selects_exactly_the_fibres_through_the_cube_with_boundary_inclusive() {
        let set = ten_fibres();
        let index = FibreIndex::build(&set, 4.0).unwrap();
        let cancel = AtomicBool::new(false);
        let sel = select_fibres(&set, &index, &cube_seed(), &cancel).unwrap();
        assert_eq!(sel.fibre_ids, vec![0, 1, 2]);
        assert_eq!(sel.strengths, vec![1.0, 1.0, 1.0]);
        assert_eq!(sel.seed_voxels, 27);
        assert!((sel.radius_mm - 0.5).abs() < 1e-12);
    }

    #[test]
    fn map_counts_voxel_visits_once_per_fibre() {
        let set = ten_fibres();
        let index = FibreIndex::build(&set, 4.0).unwrap();
        let cancel = AtomicBool::new(false);
        let (dims, affine) = OutputGrid::Mm2.spec();
        let r = structural_map(
            &set,
            &index,
            &cube_seed(),
            dims,
            &affine,
            &cancel,
            &no_progress,
        )
        .unwrap()
        .unwrap();
        let m = &r.map;
        assert_eq!(m.dims, [91, 109, 91]);
        let nonzero = m.data.iter().filter(|v| **v != 0.0).count();
        assert_eq!(nonzero, 14);
        assert_eq!(m.data.iter().sum::<f32>(), 21.0);
        for i in 42..=48 {
            assert_eq!(m.data[m.index(i, 63, 36)], 2.0);
            assert_eq!(m.data[m.index(i, 64, 36)], 1.0);
        }
    }

    #[test]
    fn weights_average_over_touched_voxels_and_zero_mean_disconnects() {
        let mut seed = NiiVolume::zeros([3, 3, 1], diagonal_affine([1.0; 3], [-1.0, -1.0, 0.0]));
        let (a, b, c, d) = (
            seed.index(0, 0, 0),
            seed.index(2, 0, 0),
            seed.index(0, 2, 0),
            seed.index(2, 2, 0),
        );
        seed.data[a] = 1.0;
        seed.data[b] = 3.0;
        seed.data[c] = -1.0;
        seed.data[d] = 1.0;
        let set =
            FibreSet::from_fibres(&[line(-1.0, 0.0), line(1.0, 0.0), line(5.0, 5.0)]).unwrap();
        let index = FibreIndex::build(&set, 4.0).unwrap();
        let cancel = AtomicBool::new(false);
        let sel = select_fibres(&set, &index, &seed, &cancel).unwrap();
        assert_eq!(sel.fibre_ids, vec![0]);
        assert_eq!(sel.strengths, vec![2.0]);
        let (dims, affine) = OutputGrid::Mm2.spec();
        let map = accumulate_map(&set, &sel, dims, &affine, &cancel).unwrap();
        assert_eq!(map.data.iter().cloned().fold(0.0f32, f32::max), 2.0);
        assert_eq!(map.data.iter().filter(|v| **v != 0.0).count(), 7);
    }

    #[test]
    fn an_unconnected_seed_gives_no_map() {
        let set = ten_fibres();
        let index = FibreIndex::build(&set, 4.0).unwrap();
        let cancel = AtomicBool::new(false);
        let mut far = NiiVolume::zeros([1, 1, 1], diagonal_affine([1.0; 3], [50.0, 50.0, 50.0]));
        far.data[0] = 1.0;
        let (dims, affine) = OutputGrid::Mm2.spec();
        let r = structural_map(&set, &index, &far, dims, &affine, &cancel, &no_progress).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn cancel_stops_the_selection() {
        let set = ten_fibres();
        let index = FibreIndex::build(&set, 4.0).unwrap();
        let cancel = AtomicBool::new(true);
        let r = select_fibres(&set, &index, &cube_seed(), &cancel);
        assert_eq!(r, Err(MapError::Cancelled));
    }

    #[test]
    fn seed_building_crops_with_a_rim_and_sums_the_sides() {
        let mut right = NiiVolume::zeros([2, 2, 2], diagonal_affine([1.0; 3], [4.0, 0.0, 0.0]));
        right.data.fill(1.0);
        let mut left = NiiVolume::zeros([2, 2, 2], diagonal_affine([1.0; 3], [-5.0, 0.0, 0.0]));
        left.data.fill(1.0);
        let seed = build_seed(&[&right, &left], SeedKind::Binary).unwrap();
        let rim = 9usize;
        assert!(seed.dims[0] > 2 * rim);
        assert!(seed.data.iter().all(|v| *v == 0.0 || *v == 1.0));
        assert!(seed.data.contains(&1.0));
        let (r, l) = split_hemispheres(&seed);
        assert!(r.data.iter().sum::<f32>() > 0.0);
        assert!(l.data.iter().sum::<f32>() > 0.0);
        assert_eq!(
            build_seed(&[], SeedKind::Binary).unwrap_err().code(),
            "E_SEED_EMPTY"
        );
    }
}
