// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use crate::error::{format_err, Result};
use crate::fibres::FibreSet;
use crate::Affine;

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use rayon::prelude::*;

const MAX_CELLS: u64 = 1 << 28;
const HISTOGRAM_BUDGET: usize = 1 << 27;

/// A binary mask on a voxel grid: `dims`, a 0-based voxel-to-mm `affine`, and `x`-fastest bytes.
#[derive(Clone, Copy, Debug)]
pub struct MaskView<'a> {
    pub dims: [usize; 3],
    pub affine: Affine,
    pub data: &'a [u8],
}

/// Dense grid of cubic cells over the fibre points mapping a cell to the fibres that pass through it (`specs/15` §3.5).
#[derive(Clone, Debug)]
pub struct FibreIndex {
    voxel_mm: f32,
    n_fibres: usize,
    origin: [i64; 3],
    dims: [usize; 3],
    cells: Vec<u64>,
    ids: Vec<u32>,
}

type Grid = ([i64; 3], [usize; 3]);

fn cell_of(p: &[f32; 3], inv: f64) -> [i64; 3] {
    [
        (f64::from(p[0]) * inv).floor() as i64,
        (f64::from(p[1]) * inv).floor() as i64,
        (f64::from(p[2]) * inv).floor() as i64,
    ]
}

fn grid_of(set: &FibreSet, inv: f64) -> Result<Grid> {
    let (lo, hi) = set
        .points
        .par_chunks(1 << 16)
        .map(|chunk| {
            let mut lo = [i64::MAX; 3];
            let mut hi = [i64::MIN; 3];
            for p in chunk {
                let c = cell_of(p, inv);
                for a in 0..3 {
                    lo[a] = lo[a].min(c[a]);
                    hi[a] = hi[a].max(c[a]);
                }
            }
            (lo, hi)
        })
        .reduce(
            || ([i64::MAX; 3], [i64::MIN; 3]),
            |a, b| {
                let mut lo = a.0;
                let mut hi = a.1;
                for i in 0..3 {
                    lo[i] = lo[i].min(b.0[i]);
                    hi[i] = hi[i].max(b.1[i]);
                }
                (lo, hi)
            },
        );
    if lo[0] > hi[0] {
        return Ok(([0; 3], [0; 3]));
    }
    let range_err = || format_err("fibre point outside the index range for this voxel size");
    let mut dims = [0usize; 3];
    let mut total = 1u64;
    for a in 0..3 {
        let n = hi[a]
            .checked_sub(lo[a])
            .and_then(|d| d.checked_add(1))
            .ok_or_else(range_err)?;
        total = total.checked_mul(n as u64).ok_or_else(range_err)?;
        if total > MAX_CELLS {
            return Err(range_err());
        }
        dims[a] = n as usize;
    }
    Ok((lo, dims))
}

fn fibre_cells(set: &FibreSet, f: usize, inv: f64, g: &Grid, out: &mut Vec<u64>) {
    out.clear();
    let (origin, dims) = g;
    for p in set.fibre(f) {
        let c = cell_of(p, inv);
        let id = (((c[2] - origin[2]) as u64 * dims[1] as u64) + (c[1] - origin[1]) as u64)
            * dims[0] as u64
            + (c[0] - origin[0]) as u64;
        if out.last() != Some(&id) {
            out.push(id);
        }
    }
    out.sort_unstable();
    out.dedup();
}

impl FibreIndex {
    /// Builds the index of `set` with cubic cells of `voxel_mm`, in parallel.
    pub fn build(set: &FibreSet, voxel_mm: f32) -> Result<Self> {
        if !voxel_mm.is_finite() || voxel_mm <= 0.0 {
            return Err(format_err("index voxel size must be positive"));
        }
        let inv = 1.0 / f64::from(voxel_mm);
        let g = grid_of(set, inv)?;
        let n_cells = g.1.iter().product::<usize>();
        let k = set.n_fibres();
        let blocks = (rayon::current_num_threads() * 2)
            .min(HISTOGRAM_BUDGET / n_cells.max(1))
            .clamp(1, k.max(1));
        let per = k.div_ceil(blocks).max(1);
        let ranges: Vec<(usize, usize)> =
            (0..k).step_by(per).map(|s| (s, (s + per).min(k))).collect();
        let hist: Vec<Vec<u32>> = ranges
            .par_iter()
            .map(|&(a, b)| {
                let mut h = vec![0u32; n_cells];
                let mut cells = Vec::new();
                for f in a..b {
                    fibre_cells(set, f, inv, &g, &mut cells);
                    for c in &cells {
                        h[*c as usize] += 1;
                    }
                }
                h
            })
            .collect();
        let mut starts = vec![0u64; n_cells + 1];
        for c in 0..n_cells {
            starts[c + 1] = starts[c] + hist.iter().map(|h| u64::from(h[c])).sum::<u64>();
        }
        let total = starts[n_cells] as usize;
        let mut cursors: Vec<Vec<u64>> = Vec::with_capacity(ranges.len());
        let mut running = starts[..n_cells].to_vec();
        for h in &hist {
            cursors.push(running.clone());
            for c in 0..n_cells {
                running[c] += u64::from(h[c]);
            }
        }
        drop(hist);
        drop(running);
        let ids: Vec<AtomicU32> = (0..total).map(|_| AtomicU32::new(0)).collect();
        ranges
            .par_iter()
            .zip(cursors.par_iter_mut())
            .for_each(|(&(a, b), cursor)| {
                let mut cells = Vec::new();
                for f in a..b {
                    fibre_cells(set, f, inv, &g, &mut cells);
                    for c in &cells {
                        let at = &mut cursor[*c as usize];
                        ids[*at as usize].store(f as u32, Ordering::Relaxed);
                        *at += 1;
                    }
                }
            });
        Ok(Self {
            voxel_mm,
            n_fibres: k,
            origin: g.0,
            dims: g.1,
            cells: starts,
            ids: ids.into_iter().map(AtomicU32::into_inner).collect(),
        })
    }

    /// An index from a cache's cell table; `ids` are cache fibre ids that `order` maps to source ids.
    pub fn from_cache(
        voxel_mm: f32,
        origin: [i64; 3],
        dims: [usize; 3],
        cells: Vec<u64>,
        mut ids: Vec<u32>,
        order: &[u32],
    ) -> Result<Self> {
        let n_cells = dims.iter().product::<usize>();
        if cells.len() != n_cells + 1
            || cells.last().copied() != Some(ids.len() as u64)
            || cells.windows(2).any(|w| w[0] > w[1])
        {
            return Err(format_err("cache cell table is inconsistent"));
        }
        let failed = AtomicU64::new(0);
        let mut slices: Vec<&mut [u32]> = Vec::with_capacity(n_cells);
        let mut rest = ids.as_mut_slice();
        for w in cells.windows(2) {
            let (head, tail) = rest.split_at_mut((w[1] - w[0]) as usize);
            slices.push(head);
            rest = tail;
        }
        slices.into_par_iter().for_each(|cell| {
            for id in cell.iter_mut() {
                match order.get(*id as usize) {
                    Some(src) => *id = *src,
                    None => {
                        failed.store(1, Ordering::Relaxed);
                        return;
                    }
                }
            }
            cell.sort_unstable();
        });
        if failed.load(Ordering::Relaxed) != 0 {
            return Err(format_err("cache cell table names a fibre out of range"));
        }
        Ok(Self {
            voxel_mm,
            n_fibres: order.len(),
            origin,
            dims,
            cells,
            ids,
        })
    }

    /// The grid as `(origin cell, dims, cell offsets, fibre ids)`; cell `(i, j, k)` is `i + n_x (j + n_y k)`.
    pub fn parts(&self) -> ([i64; 3], [usize; 3], &[u64], &[u32]) {
        (self.origin, self.dims, &self.cells, &self.ids)
    }

    pub fn n_fibres(&self) -> usize {
        self.n_fibres
    }

    pub fn voxel_mm(&self) -> f32 {
        self.voxel_mm
    }

    /// The number of cells that hold at least one fibre.
    pub fn n_cells(&self) -> usize {
        self.cells.windows(2).filter(|w| w[1] > w[0]).count()
    }

    /// The ascending fibre ids with a point in cell `cell` (cell = `floor(point / voxel_mm)`).
    pub fn fibres_in_cell(&self, cell: [i32; 3]) -> &[u32] {
        let mut at = [0usize; 3];
        for a in 0..3 {
            let rel = i64::from(cell[a]) - self.origin[a];
            if rel < 0 || rel as usize >= self.dims[a] {
                return &[];
            }
            at[a] = rel as usize;
        }
        let id = (at[2] * self.dims[1] + at[1]) * self.dims[0] + at[0];
        &self.ids[self.cells[id] as usize..self.cells[id + 1] as usize]
    }

    /// The ascending ids of the fibres with at least one point in any of `cells`.
    pub fn fibres_through(&self, cells: impl Iterator<Item = [i32; 3]>) -> Vec<u32> {
        let mut seen = vec![false; self.n_fibres];
        for c in cells {
            for f in self.fibres_in_cell(c) {
                seen[*f as usize] = true;
            }
        }
        collect(&seen)
    }

    /// The ascending ids of the fibres of `set` (the set the index was built from) with at least one point inside a non-zero voxel of `mask`.
    pub fn fibres_through_mask(&self, set: &FibreSet, mask: &MaskView<'_>) -> Result<Vec<u32>> {
        if set.n_fibres() != self.n_fibres {
            return Err(format_err(
                "the fibre set is not the one the index was built from",
            ));
        }
        let [nx, ny, nz] = mask.dims;
        let total = nx
            .checked_mul(ny)
            .and_then(|v| v.checked_mul(nz))
            .ok_or_else(|| format_err("mask dimensions overflow"))?;
        if mask.data.len() != total {
            return Err(format_err("mask data length does not match its dimensions"));
        }
        let inv = invert(&mask.affine).ok_or_else(|| format_err("mask affine is singular"))?;
        let cell_inv = 1.0 / f64::from(self.voxel_mm);
        let mut candidate = vec![false; self.n_fibres];
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    if mask.data[(k * ny + j) * nx + i] == 0 {
                        continue;
                    }
                    let mut lo = [i64::MAX; 3];
                    let mut hi = [i64::MIN; 3];
                    for corner in 0..8u8 {
                        let v = [
                            i as f64 - 0.5 + f64::from(corner & 1),
                            j as f64 - 0.5 + f64::from((corner >> 1) & 1),
                            k as f64 - 0.5 + f64::from((corner >> 2) & 1),
                        ];
                        let w = apply(&mask.affine, v);
                        for a in 0..3 {
                            let c = (w[a] * cell_inv).floor() as i64;
                            lo[a] = lo[a].min(c);
                            hi[a] = hi[a].max(c);
                        }
                    }
                    for x in lo[0]..=hi[0] {
                        for y in lo[1]..=hi[1] {
                            for z in lo[2]..=hi[2] {
                                let cell = [x, y, z].map(|v| i32::try_from(v).unwrap_or(i32::MAX));
                                for f in self.fibres_in_cell(cell) {
                                    candidate[*f as usize] = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        for (f, is_candidate) in candidate.iter().enumerate() {
            if !is_candidate {
                continue;
            }
            let hit = set.fibre(f).iter().any(|p| {
                let v = apply(&inv, [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]);
                let idx = v.map(|c| c.round());
                if idx.iter().any(|c| !c.is_finite()) {
                    return false;
                }
                let (i, j, k) = (idx[0], idx[1], idx[2]);
                i >= 0.0
                    && j >= 0.0
                    && k >= 0.0
                    && (i as usize) < nx
                    && (j as usize) < ny
                    && (k as usize) < nz
                    && mask.data[((k as usize) * ny + j as usize) * nx + i as usize] != 0
            });
            if hit {
                out.push(f as u32);
            }
        }
        Ok(out)
    }
}

fn collect(seen: &[bool]) -> Vec<u32> {
    seen.iter()
        .enumerate()
        .filter(|(_, s)| **s)
        .map(|(f, _)| f as u32)
        .collect()
}

fn apply(m: &Affine, v: [f64; 3]) -> [f64; 3] {
    let row = |r: usize| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2] + m[r][3];
    [row(0), row(1), row(2)]
}

fn invert(m: &Affine) -> Option<Affine> {
    let a = [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    let inv = [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * d,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * d,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * d,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * d,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * d,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * d,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * d,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * d,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * d,
        ],
    ];
    let t = [m[0][3], m[1][3], m[2][3]];
    let mut out = [[0.0; 4]; 4];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = inv[r][c];
        }
        out[r][3] = -(inv[r][0] * t[0] + inv[r][1] * t[1] + inv[r][2] * t[2]);
    }
    out[3][3] = 1.0;
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn set() -> FibreSet {
        FibreSet::from_fibres(&[
            vec![[0.5, 0.5, 0.5], [1.5, 0.5, 0.5], [2.5, 0.5, 0.5]],
            vec![[10.5, 10.5, 10.5], [10.5, 11.5, 10.5]],
            vec![[1.5, 5.5, 0.5], [1.5, 6.5, 0.5]],
            vec![[-3.5, -3.5, -3.5], [-2.5, -3.5, -3.5]],
        ])
        .unwrap()
    }

    #[test]
    fn query_returns_exactly_the_crossing_fibres() {
        let s = set();
        let idx = FibreIndex::build(&s, 1.0).unwrap();
        assert_eq!(idx.fibres_through([[1, 0, 0]].into_iter()), vec![0]);
        assert_eq!(
            idx.fibres_through([[1, 0, 0], [10, 11, 10]].into_iter()),
            vec![0, 1]
        );
        assert_eq!(idx.fibres_through([[-3, -4, -4]].into_iter()), vec![3]);
        assert!(idx
            .fibres_through([[5, 5, 5], [1, 1, 0]].into_iter())
            .is_empty());
        assert_eq!(idx.fibres_in_cell([1, 5, 0]), &[2]);
    }

    #[test]
    fn coarse_cells_group_neighbouring_points() {
        let s = set();
        let idx = FibreIndex::build(&s, 4.0).unwrap();
        assert_eq!(idx.fibres_through([[0, 0, 0]].into_iter()), vec![0]);
        assert_eq!(idx.fibres_through([[0, 1, 0]].into_iter()), vec![2]);
        assert_eq!(idx.fibres_through([[2, 2, 2]].into_iter()), vec![1]);
    }

    #[test]
    fn mask_query_is_exact_on_the_mask_grid() {
        let s = set();
        let idx = FibreIndex::build(&s, 4.0).unwrap();
        let affine = [
            [1.0, 0.0, 0.0, -4.5],
            [0.0, 1.0, 0.0, -4.5],
            [0.0, 0.0, 1.0, -4.5],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let dims = [20usize, 20, 20];
        let mut data = vec![0u8; 8000];
        let at = |x: i32, y: i32, z: i32| {
            (((z + 5) as usize * 20) + (y + 5) as usize) * 20 + (x + 5) as usize
        };
        data[at(1, 1, 0)] = 1;
        let mask = MaskView {
            dims,
            affine,
            data: &data,
        };
        assert!(idx.fibres_through_mask(&s, &mask).unwrap().is_empty());
        data[at(1, 0, 0)] = 1;
        data[at(10, 11, 10)] = 1;
        data[at(-4, -4, -4)] = 1;
        let mask = MaskView {
            dims,
            affine,
            data: &data,
        };
        assert_eq!(idx.fibres_through_mask(&s, &mask).unwrap(), vec![0, 1, 3]);
        data[at(-4, -4, -4)] = 0;
        data[at(-3, -4, -4)] = 1;
        let mask = MaskView {
            dims,
            affine,
            data: &data,
        };
        assert_eq!(idx.fibres_through_mask(&s, &mask).unwrap(), vec![0, 1, 3]);
        let bad = MaskView {
            dims: [2, 2, 2],
            affine,
            data: &data,
        };
        assert!(idx.fibres_through_mask(&s, &bad).is_err());
    }

    #[test]
    fn dense_grid_matches_a_brute_force_cell_scan() {
        let mut state = 12345u64;
        let mut rnd = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1u64 << 31) as f32) * 60.0 - 30.0
        };
        let fibres: Vec<Vec<[f32; 3]>> = (0..3000)
            .map(|_| {
                let o = [rnd(), rnd(), rnd()];
                (0..9)
                    .map(|i| {
                        [
                            o[0] + i as f32 * 0.7,
                            o[1] - i as f32 * 0.4,
                            o[2] + i as f32,
                        ]
                    })
                    .collect()
            })
            .collect();
        let s = FibreSet::from_fibres(&fibres).unwrap();
        let idx = FibreIndex::build(&s, 4.0).unwrap();
        for cx in -9..9 {
            for cy in -9..9 {
                let cell = [cx, cy, 0];
                let want: Vec<u32> = (0..s.n_fibres())
                    .filter(|f| {
                        s.fibre(*f).iter().any(|p| {
                            (p[0] / 4.0).floor() as i32 == cx
                                && (p[1] / 4.0).floor() as i32 == cy
                                && (p[2] / 4.0).floor() as i32 == 0
                        })
                    })
                    .map(|f| f as u32)
                    .collect();
                assert_eq!(idx.fibres_in_cell(cell), want.as_slice());
            }
        }
        assert!(idx.fibres_in_cell([100, 0, 0]).is_empty());
    }

    #[test]
    fn invalid_voxel_size_and_range_are_errors() {
        assert!(FibreIndex::build(&set(), 0.0).is_err());
        assert!(FibreIndex::build(&set(), 1e-6).is_err());
    }
}
