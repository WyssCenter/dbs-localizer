// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Field from potential, sampling, VTA post-processing (`specs/07` §3.8, §3.9).

use rayon::prelude::*;

use crate::geometry::{LeadFrame, Materials, SolverGrid, INSULATION, TISSUE};
use crate::Grid;

/// Clamp of written |E| in V/m (`specs/07` §3.8 step 3).
pub const EFIELD_CLAMP_V_PER_M: f32 = 10_000.0;
/// Margin around the supra-threshold bounding box in millimetres (`specs/07` §3.8 step 1).
pub const OUTPUT_MARGIN_MM: f64 = 5.0;
/// Largest output grid extent per axis (`specs/07` §3.8 step 1).
pub const MAX_OUTPUT_DIM: usize = 400;
/// Radius of the opening ball in voxels (`specs/07` §3.8 step 6).
pub const OPENING_RADIUS_VOXELS: usize = 3;
/// Electrode removal starts this far distal of the head marker (`specs/07` §3.8 step 2).
pub const REMOVAL_START_MM: f64 = 5.0;

/// Cell-centre electric field in V/m from node potentials; non-tissue cells are zero.
pub fn cell_efield(
    grid: &SolverGrid,
    mats: &Materials,
    node_of_cell: &[u32],
    phi_nodes: &[f64],
) -> Vec<[f32; 3]> {
    let [nx, ny, nz] = grid.dims;
    let plane = nx * ny;
    let mut out = vec![[0f32; 3]; grid.n_cells()];
    out.par_chunks_mut(plane)
        .enumerate()
        .for_each(|(k, chunk)| {
            debug_assert!(k < nz);
            for j in 0..ny {
                for i in 0..nx {
                    let c = grid.index(i, j, k);
                    if mats.class[c] != TISSUE {
                        continue;
                    }
                    let phi_c = phi_nodes[node_of_cell[c] as usize];
                    let sc = f64::from(mats.sigma[c]);
                    let mut e = [0f64; 3];
                    for (axis, ea) in e.iter_mut().enumerate() {
                        let pos = [i, j, k];
                        let wc = grid.widths[axis][pos[axis]];
                        let mut faces = [0.0f64; 2];
                        for (side, f) in faces.iter_mut().enumerate() {
                            let plus = side == 1;
                            let mut p = pos;
                            let at_edge = if plus {
                                p[axis] + 1 >= grid.dims[axis]
                            } else {
                                p[axis] == 0
                            };
                            if at_edge {
                                *f = 0.0;
                                continue;
                            }
                            p[axis] = if plus { p[axis] + 1 } else { p[axis] - 1 };
                            let nb = grid.index(p[0], p[1], p[2]);
                            *f = match mats.class[nb] {
                                INSULATION => phi_c,
                                TISSUE => {
                                    let wn = grid.widths[axis][p[axis]];
                                    let sn = f64::from(mats.sigma[nb]);
                                    let gc = sc / (0.5 * wc);
                                    let gn = sn / (0.5 * wn);
                                    (gc * phi_c + gn * phi_nodes[node_of_cell[nb] as usize])
                                        / (gc + gn)
                                }
                                _ => phi_nodes[node_of_cell[nb] as usize],
                            };
                        }
                        *ea = -(faces[1] - faces[0]) / wc * 1000.0;
                    }
                    chunk[i + nx * j] = [e[0] as f32, e[1] as f32, e[2] as f32];
                }
            }
        });
    out
}

/// Magnitude of a vector field.
pub fn magnitudes(v: &[[f32; 3]]) -> Vec<f32> {
    v.par_iter()
        .map(|e| (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt())
        .collect()
}

/// Trilinear sampler of a cell-centred scalar over the valid (tissue) cells.
pub struct Sampler<'a> {
    grid: &'a SolverGrid,
    class: &'a [u16],
    values: &'a [f32],
}

impl<'a> Sampler<'a> {
    /// Wraps the grid, its classes and one scalar per cell.
    pub fn new(grid: &'a SolverGrid, mats: &'a Materials, values: &'a [f32]) -> Self {
        Self {
            grid,
            class: &mats.class,
            values,
        }
    }

    fn axis(&self, a: usize, v: f64) -> (usize, usize, f64) {
        let c = &self.grid.centres[a];
        let n = c.len();
        if v <= c[0] {
            return (0, 0, 0.0);
        }
        if v >= c[n - 1] {
            return (n - 1, n - 1, 0.0);
        }
        let hi = c.partition_point(|&x| x <= v);
        let lo = hi - 1;
        (lo, hi, (v - c[lo]) / (c[hi] - c[lo]))
    }

    /// Value at a lead-local point; zero outside the domain or without valid neighbours.
    pub fn sample(&self, p: [f64; 3]) -> f32 {
        let g = self.grid;
        for (a, &v) in p.iter().enumerate() {
            let f = &g.faces[a];
            if v < f[0] || v > f[f.len() - 1] {
                return 0.0;
            }
        }
        let (i0, i1, tx) = self.axis(0, p[0]);
        let (j0, j1, ty) = self.axis(1, p[1]);
        let (k0, k1, tz) = self.axis(2, p[2]);
        let mut acc = 0.0f64;
        let mut wsum = 0.0f64;
        for (kk, wz) in [(k0, 1.0 - tz), (k1, tz)] {
            for (jj, wy) in [(j0, 1.0 - ty), (j1, ty)] {
                for (ii, wx) in [(i0, 1.0 - tx), (i1, tx)] {
                    let w = wx * wy * wz;
                    if w <= 0.0 {
                        continue;
                    }
                    let idx = g.index(ii, jj, kk);
                    if self.class[idx] == TISSUE {
                        acc += w * f64::from(self.values[idx]);
                        wsum += w;
                    }
                }
            }
        }
        if wsum > 0.0 {
            (acc / wsum) as f32
        } else {
            0.0
        }
    }
}

/// World-space output grid description.
#[derive(Clone, Debug)]
pub struct OutputGrid {
    /// Voxel counts.
    pub dims: [usize; 3],
    /// World position of voxel (0, 0, 0).
    pub origin: [f64; 3],
    /// Isotropic spacing in millimetres.
    pub spacing: f64,
}

impl OutputGrid {
    /// Voxel-to-world affine of this grid.
    pub fn affine(&self) -> [[f64; 4]; 4] {
        let s = self.spacing;
        [
            [s, 0.0, 0.0, self.origin[0]],
            [0.0, s, 0.0, self.origin[1]],
            [0.0, 0.0, s, self.origin[2]],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    /// World position of a voxel centre.
    pub fn world(&self, i: usize, j: usize, k: usize) -> [f64; 3] {
        [
            self.origin[0] + self.spacing * i as f64,
            self.origin[1] + self.spacing * j as f64,
            self.origin[2] + self.spacing * k as f64,
        ]
    }
}

/// Chooses the output grid for a supra-threshold bounding box (`specs/07` §3.8 step 1).
pub fn output_grid_for_bbox(
    min: [f64; 3],
    max: [f64; 3],
    spacing: f64,
    warnings: &mut Vec<String>,
) -> OutputGrid {
    let mut origin = [0.0; 3];
    let mut dims = [0usize; 3];
    for a in 0..3 {
        let lo = ((min[a] - OUTPUT_MARGIN_MM) / spacing).floor() * spacing;
        let hi = ((max[a] + OUTPUT_MARGIN_MM) / spacing).ceil() * spacing;
        let mut n = ((hi - lo) / spacing).round() as usize + 1;
        let mut o = lo;
        if n > MAX_OUTPUT_DIM {
            warnings.push(format!(
                "output grid axis {a} clipped from {n} to {MAX_OUTPUT_DIM} voxels"
            ));
            o += ((n - MAX_OUTPUT_DIM) / 2) as f64 * spacing;
            n = MAX_OUTPUT_DIM;
        }
        origin[a] = o;
        dims[a] = n;
    }
    OutputGrid {
        dims,
        origin,
        spacing,
    }
}

/// Samples a per-cell magnitude onto the output grid, with electrode removal when requested.
pub fn sample_to_output(
    sampler: &Sampler<'_>,
    lead: &LeadFrame,
    out: &OutputGrid,
    lead_radius: f64,
    remove_electrode: bool,
) -> Vec<f32> {
    let [nx, ny, nz] = out.dims;
    let mut data = vec![0f32; nx * ny * nz];
    data.par_chunks_mut(nx * ny)
        .enumerate()
        .for_each(|(k, plane)| {
            for j in 0..ny {
                for i in 0..nx {
                    let mut l = lead.to_local(out.world(i, j, k));
                    if remove_electrode && l[2] > -REMOVAL_START_MM {
                        let rho = (l[0] * l[0] + l[1] * l[1]).sqrt();
                        let (ux, uy) = if rho > 1e-9 {
                            (l[0] / rho, l[1] / rho)
                        } else {
                            (1.0, 0.0)
                        };
                        l[0] = ux * (rho + lead_radius);
                        l[1] = uy * (rho + lead_radius);
                    }
                    plane[i + nx * j] = sampler.sample(l);
                }
            }
        });
    data
}

/// Binary mask of values strictly above the threshold.
pub fn threshold(values: &[f32], thr: f32) -> Vec<u8> {
    values.par_iter().map(|&v| u8::from(v > thr)).collect()
}

/// Number of set voxels.
pub fn count_set(mask: &[u8]) -> usize {
    mask.iter().filter(|&&v| v != 0).count()
}

/// Fills background voxels that cannot reach the grid border through 6-connected background.
pub fn fill_holes(mask: &mut Grid<u8>) {
    let [nx, ny, nz] = mask.dims;
    if nx == 0 || ny == 0 || nz == 0 {
        return;
    }
    let idx = |i: usize, j: usize, k: usize| i + nx * (j + ny * k);
    let mut outside = vec![false; mask.data.len()];
    let mut stack: Vec<[usize; 3]> = Vec::new();
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let border =
                    i == 0 || j == 0 || k == 0 || i + 1 == nx || j + 1 == ny || k + 1 == nz;
                if border && mask.data[idx(i, j, k)] == 0 {
                    outside[idx(i, j, k)] = true;
                    stack.push([i, j, k]);
                }
            }
        }
    }
    while let Some([i, j, k]) = stack.pop() {
        let nbrs = [
            (i.wrapping_sub(1), j, k),
            (i + 1, j, k),
            (i, j.wrapping_sub(1), k),
            (i, j + 1, k),
            (i, j, k.wrapping_sub(1)),
            (i, j, k + 1),
        ];
        for (a, b, c) in nbrs {
            if a < nx && b < ny && c < nz {
                let q = idx(a, b, c);
                if mask.data[q] == 0 && !outside[q] {
                    outside[q] = true;
                    stack.push([a, b, c]);
                }
            }
        }
    }
    for (v, &o) in mask.data.iter_mut().zip(&outside) {
        if *v == 0 && !o {
            *v = 1;
        }
    }
}

fn ball_offsets(radius: usize) -> Vec<[i64; 3]> {
    let r = radius as i64;
    let mut out = Vec::new();
    for dz in -r..=r {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy + dz * dz <= r * r {
                    out.push([dx, dy, dz]);
                }
            }
        }
    }
    out
}

/// Morphological opening (erosion then dilation) with a ball of the given radius in voxels.
pub fn open_ball(mask: &Grid<u8>, radius: usize) -> Grid<u8> {
    let [nx, ny, nz] = mask.dims;
    let offs = ball_offsets(radius);
    let at = |i: i64, j: i64, k: i64| -> bool {
        i >= 0
            && j >= 0
            && k >= 0
            && (i as usize) < nx
            && (j as usize) < ny
            && (k as usize) < nz
            && mask.data[i as usize + nx * (j as usize + ny * k as usize)] != 0
    };
    let mut eroded = vec![0u8; mask.data.len()];
    eroded
        .par_chunks_mut(nx * ny)
        .enumerate()
        .for_each(|(k, plane)| {
            for j in 0..ny {
                for i in 0..nx {
                    if mask.data[i + nx * (j + ny * k)] == 0 {
                        continue;
                    }
                    let keep = offs
                        .iter()
                        .all(|o| at(i as i64 + o[0], j as i64 + o[1], k as i64 + o[2]));
                    plane[i + nx * j] = u8::from(keep);
                }
            }
        });
    let mut out = vec![0u8; mask.data.len()];
    out.par_chunks_mut(nx * ny)
        .enumerate()
        .for_each(|(k, plane)| {
            for j in 0..ny {
                for i in 0..nx {
                    let hit = offs.iter().any(|o| {
                        let (a, b, c) = (i as i64 + o[0], j as i64 + o[1], k as i64 + o[2]);
                        a >= 0
                            && b >= 0
                            && c >= 0
                            && (a as usize) < nx
                            && (b as usize) < ny
                            && (c as usize) < nz
                            && eroded[a as usize + nx * (b as usize + ny * c as usize)] != 0
                    });
                    plane[i + nx * j] = u8::from(hit);
                }
            }
        });
    Grid {
        dims: mask.dims,
        data: out,
    }
}

/// True when a supra-threshold cell touches the outer boundary of the solver grid.
pub fn touches_boundary(grid: &SolverGrid, mags: &[f32], thr: f32) -> bool {
    let [nx, ny, nz] = grid.dims;
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let edge = i == 0 || j == 0 || k == 0 || i + 1 == nx || j + 1 == ny || k + 1 == nz;
                if edge && mags[grid.index(i, j, k)] > thr {
                    return true;
                }
            }
        }
    }
    false
}

/// Lead-local centre bounding box of the cells above the threshold, if any.
pub fn supra_bbox_world(
    grid: &SolverGrid,
    mats: &Materials,
    mags: &[f32],
    thr: f32,
    lead: &LeadFrame,
) -> Option<([f64; 3], [f64; 3])> {
    let [nx, ny, nz] = grid.dims;
    let init = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3], false);
    let (min, max, any) = (0..nz)
        .into_par_iter()
        .map(|k| {
            let mut mn = [f64::INFINITY; 3];
            let mut mx = [f64::NEG_INFINITY; 3];
            let mut any = false;
            for j in 0..ny {
                for i in 0..nx {
                    let c = grid.index(i, j, k);
                    if mats.class[c] == TISSUE && mags[c] > thr {
                        let w = lead.to_world(grid.centre(i, j, k));
                        for a in 0..3 {
                            mn[a] = mn[a].min(w[a]);
                            mx[a] = mx[a].max(w[a]);
                        }
                        any = true;
                    }
                }
            }
            (mn, mx, any)
        })
        .reduce(
            || init,
            |a, b| {
                let mut mn = a.0;
                let mut mx = a.1;
                for d in 0..3 {
                    mn[d] = mn[d].min(b.0[d]);
                    mx[d] = mx[d].max(b.1[d]);
                }
                (mn, mx, a.2 || b.2)
            },
        );
    any.then_some((min, max))
}
