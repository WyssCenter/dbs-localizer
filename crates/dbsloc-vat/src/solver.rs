// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Finite-volume system and preconditioned conjugate gradients (`specs/07` §3.3, §3.4, §3.7).
//!
//! Cell-centred finite volumes on the rectilinear grid give the 7-point stencil with
//! harmonic-mean face conductances. Contacts are equipotential: all cells of a contact share
//! one unknown, so a floating contact is a node with a zero right-hand side and the system stays
//! well conditioned. Insulation cells are excluded (zero flux). The outer boundary is grounded.

use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::geometry::{Materials, SolverGrid, CONTACT_BASE, INSULATION, TISSUE};
use crate::VatError;

const CHUNK: usize = 8192;
const CANCEL_POLL: usize = 8;
const NO_NODE: u32 = u32::MAX;

/// Solver stopping rule (`specs/07` §3.7).
#[derive(Clone, Copy, Debug)]
pub struct SolverSettings {
    /// Relative residual at which iteration stops.
    pub rel_tol: f64,
    /// Iteration cap; exceeding it is `VatError::NotConverged`.
    pub max_iter: usize,
}

impl Default for SolverSettings {
    fn default() -> Self {
        Self {
            rel_tol: 1e-6,
            max_iter: 5000,
        }
    }
}

/// Progress report of a running solve.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    /// 0-based contact being solved.
    pub contact: usize,
    /// Iterations so far.
    pub iteration: usize,
    /// Current relative residual.
    pub rel_residual: f64,
}

/// Cancellation and progress hooks shared by every solve.
#[derive(Clone, Copy, Default)]
pub struct Control<'a> {
    /// Polled every few iterations; `true` ends the solve with `VatError::Cancelled`.
    pub cancel: Option<&'a AtomicBool>,
    /// Called every few iterations.
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
}

impl Control<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|c| c.load(Ordering::Relaxed))
    }
}

/// Iteration statistics of one solve.
#[derive(Clone, Copy, Debug)]
pub struct SolveStats {
    /// 0-based contact driven in this solve.
    pub contact: usize,
    /// Iterations used.
    pub iterations: usize,
    /// Final relative residual.
    pub rel_residual: f64,
}

/// Assembled linear system with contact nodes merged.
#[derive(Clone, Debug)]
pub struct System {
    /// Number of unknowns.
    pub n: usize,
    row_ptr: Vec<usize>,
    cols: Vec<u32>,
    vals: Vec<f64>,
    diag: Vec<f64>,
    /// Unknown index per cell (`u32::MAX` for insulation).
    pub node_of_cell: Vec<u32>,
    /// Unknown index per contact, `None` when no cell belongs to the contact.
    pub contact_node: Vec<Option<u32>>,
}

#[derive(Clone, Copy)]
enum Neighbour {
    Boundary,
    Insulation,
    Tissue(usize),
    Contact(usize),
}

fn neighbour(
    grid: &SolverGrid,
    mats: &Materials,
    i: usize,
    j: usize,
    k: usize,
    axis: usize,
    plus: bool,
) -> Neighbour {
    let mut p = [i, j, k];
    if plus {
        if p[axis] + 1 >= grid.dims[axis] {
            return Neighbour::Boundary;
        }
        p[axis] += 1;
    } else {
        if p[axis] == 0 {
            return Neighbour::Boundary;
        }
        p[axis] -= 1;
    }
    let idx = grid.index(p[0], p[1], p[2]);
    match mats.class[idx] {
        TISSUE => Neighbour::Tissue(idx),
        INSULATION => Neighbour::Insulation,
        c => Neighbour::Contact((c - CONTACT_BASE) as usize),
    }
}

fn face_area(grid: &SolverGrid, i: usize, j: usize, k: usize, axis: usize) -> f64 {
    let w = [grid.widths[0][i], grid.widths[1][j], grid.widths[2][k]];
    match axis {
        0 => w[1] * w[2],
        1 => w[0] * w[2],
        _ => w[0] * w[1],
    }
}

/// Assembles the system for `n_contacts` contacts (`specs/07` §3.3).
pub fn assemble(
    grid: &SolverGrid,
    mats: &Materials,
    n_contacts: usize,
) -> Result<System, VatError> {
    let [nx, ny, nz] = grid.dims;
    let n_cells = grid.n_cells();
    let mut node_of_cell = vec![NO_NODE; n_cells];
    let mut n_tissue = 0u32;
    let mut contact_present = vec![false; n_contacts];
    for (c, &cl) in mats.class.iter().enumerate() {
        if cl == TISSUE {
            node_of_cell[c] = n_tissue;
            n_tissue += 1;
        } else if cl >= CONTACT_BASE {
            let k = (cl - CONTACT_BASE) as usize;
            if k >= n_contacts {
                return Err(VatError::InvalidInput(format!(
                    "cell of contact {k} beyond {n_contacts} contacts"
                )));
            }
            contact_present[k] = true;
        }
    }
    let mut contact_node = vec![None; n_contacts];
    let mut next = n_tissue;
    for k in 0..n_contacts {
        if contact_present[k] {
            contact_node[k] = Some(next);
            next += 1;
        }
    }
    for (c, &cl) in mats.class.iter().enumerate() {
        if cl >= CONTACT_BASE {
            if let Some(nd) = contact_node[(cl - CONTACT_BASE) as usize] {
                node_of_cell[c] = nd;
            }
        }
    }
    let n = next as usize;
    let nt = n_tissue as usize;

    let mut row_ptr = Vec::with_capacity(n + 1);
    let mut cols: Vec<u32> = Vec::with_capacity(nt * 6);
    let mut vals: Vec<f64> = Vec::with_capacity(nt * 6);
    let mut diag = vec![0.0f64; n];
    let mut contact_entries: Vec<Vec<(u32, f64)>> = vec![Vec::new(); n_contacts];
    row_ptr.push(0);

    let mut row: Vec<(u32, f64)> = Vec::with_capacity(6);
    let mut node = 0usize;
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let c = grid.index(i, j, k);
                if mats.class[c] != TISSUE {
                    continue;
                }
                let sc = f64::from(mats.sigma[c]);
                row.clear();
                let mut d = 0.0f64;
                for axis in 0..3 {
                    let area = face_area(grid, i, j, k, axis);
                    let wc = [grid.widths[0][i], grid.widths[1][j], grid.widths[2][k]][axis];
                    for plus in [false, true] {
                        let g_half = sc * area / (0.5 * wc);
                        match neighbour(grid, mats, i, j, k, axis, plus) {
                            Neighbour::Boundary => d += g_half,
                            Neighbour::Insulation => {}
                            Neighbour::Contact(q) => {
                                if let Some(nd) = contact_node[q] {
                                    d += g_half;
                                    row.push((nd, -g_half));
                                    contact_entries[q].push((node as u32, -g_half));
                                    diag[nd as usize] += g_half;
                                }
                            }
                            Neighbour::Tissue(nb) => {
                                let mut p = [i, j, k];
                                p[axis] = if plus { p[axis] + 1 } else { p[axis] - 1 };
                                let wn = grid.widths[axis][p[axis]];
                                let sn = f64::from(mats.sigma[nb]);
                                let g = area / (0.5 * wc / sc + 0.5 * wn / sn);
                                d += g;
                                row.push((node_of_cell[nb], -g));
                            }
                        }
                    }
                }
                row.sort_unstable_by_key(|e| e.0);
                let mut idx = 0;
                while idx < row.len() {
                    let col = row[idx].0;
                    let mut v = 0.0;
                    while idx < row.len() && row[idx].0 == col {
                        v += row[idx].1;
                        idx += 1;
                    }
                    cols.push(col);
                    vals.push(v);
                }
                diag[node] += d;
                row_ptr.push(cols.len());
                node += 1;
            }
        }
    }
    for (q, entries) in contact_entries.iter_mut().enumerate() {
        if contact_node[q].is_none() {
            continue;
        }
        entries.sort_unstable_by_key(|e| e.0);
        let mut idx = 0;
        while idx < entries.len() {
            let col = entries[idx].0;
            let mut v = 0.0;
            while idx < entries.len() && entries[idx].0 == col {
                v += entries[idx].1;
                idx += 1;
            }
            cols.push(col);
            vals.push(v);
        }
        row_ptr.push(cols.len());
    }
    Ok(System {
        n,
        row_ptr,
        cols,
        vals,
        diag,
        node_of_cell,
        contact_node,
    })
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    let parts: Vec<f64> = a
        .par_chunks(CHUNK)
        .zip(b.par_chunks(CHUNK))
        .map(|(x, y)| x.iter().zip(y).map(|(p, q)| p * q).sum::<f64>())
        .collect();
    parts.iter().sum()
}

impl System {
    /// Number of stored off-diagonal entries.
    pub fn nnz(&self) -> usize {
        self.cols.len()
    }

    /// Off-diagonal CSR row pointers.
    pub fn row_ptr(&self) -> &[usize] {
        &self.row_ptr
    }

    /// Off-diagonal CSR column indices.
    pub fn cols(&self) -> &[u32] {
        &self.cols
    }

    /// Off-diagonal CSR values.
    pub fn vals(&self) -> &[f64] {
        &self.vals
    }

    /// Diagonal of the system matrix.
    pub fn diag(&self) -> &[f64] {
        &self.diag
    }

    /// Sparse product `y = A x` in `f64`.
    pub fn matvec(&self, x: &[f64], y: &mut [f64]) {
        y.par_iter_mut().enumerate().for_each(|(r, out)| {
            let (a, b) = (self.row_ptr[r], self.row_ptr[r + 1]);
            let mut s = self.diag[r] * x[r];
            for e in a..b {
                s += self.vals[e] * x[self.cols[e] as usize];
            }
            *out = s;
        });
    }

    /// Solves `A x = b` with Jacobi-preconditioned CG from a zero start.
    pub fn solve(
        &self,
        b: &[f64],
        settings: &SolverSettings,
        ctl: &Control<'_>,
        contact: usize,
    ) -> Result<(Vec<f64>, SolveStats), VatError> {
        let n = self.n;
        let bnorm = dot(b, b).sqrt();
        let mut x = vec![0.0f64; n];
        if bnorm == 0.0 {
            return Ok((
                x,
                SolveStats {
                    contact,
                    iterations: 0,
                    rel_residual: 0.0,
                },
            ));
        }
        let mut r = b.to_vec();
        let inv_d: Vec<f64> = self.diag.iter().map(|d| 1.0 / d).collect();
        let mut z: Vec<f64> = r.par_iter().zip(&inv_d).map(|(a, m)| a * m).collect();
        let mut p = z.clone();
        let mut ap = vec![0.0f64; n];
        let mut rz = dot(&r, &z);
        let mut rel = 1.0;
        for it in 0..settings.max_iter {
            if it % CANCEL_POLL == 0 {
                if ctl.cancelled() {
                    return Err(VatError::Cancelled);
                }
                if let Some(cb) = ctl.progress {
                    cb(Progress {
                        contact,
                        iteration: it,
                        rel_residual: rel,
                    });
                }
            }
            self.matvec(&p, &mut ap);
            let pap = dot(&p, &ap);
            if !(pap.is_finite() && pap > 0.0) {
                return Err(VatError::NotConverged {
                    iterations: it,
                    rel_residual: rel,
                });
            }
            let alpha = rz / pap;
            x.par_iter_mut()
                .zip(&p)
                .for_each(|(xi, pi)| *xi += alpha * pi);
            r.par_iter_mut()
                .zip(&ap)
                .for_each(|(ri, api)| *ri -= alpha * api);
            rel = dot(&r, &r).sqrt() / bnorm;
            if rel <= settings.rel_tol {
                return Ok((
                    x,
                    SolveStats {
                        contact,
                        iterations: it + 1,
                        rel_residual: rel,
                    },
                ));
            }
            z.par_iter_mut()
                .zip(&r)
                .zip(&inv_d)
                .for_each(|((zi, ri), m)| *zi = ri * m);
            let rz_new = dot(&r, &z);
            let beta = rz_new / rz;
            rz = rz_new;
            p.par_iter_mut()
                .zip(&z)
                .for_each(|(pi, zi)| *pi = zi + beta * *pi);
        }
        Err(VatError::NotConverged {
            iterations: settings.max_iter,
            rel_residual: rel,
        })
    }

    /// Solves with prescribed contact currents in mA; returns the potential of every unknown.
    pub fn solve_currents(
        &self,
        currents_ma: &[(usize, f64)],
        settings: &SolverSettings,
        ctl: &Control<'_>,
    ) -> Result<(Vec<f64>, SolveStats), VatError> {
        let mut b = vec![0.0f64; self.n];
        let mut first = 0;
        for (k, i) in currents_ma {
            let node = self
                .contact_node
                .get(*k)
                .copied()
                .flatten()
                .ok_or(VatError::ContactNotResolved { contact: *k })?;
            b[node as usize] += i;
            first = *k;
        }
        self.solve(&b, settings, ctl, first)
    }
}
