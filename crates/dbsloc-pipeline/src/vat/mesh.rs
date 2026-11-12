// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Display surface of a binary VTA (`vatfv`, `specs/07` §5.2): the boundary faces of the
//! voxel mask as a shared-vertex triangle mesh, smoothed with Taubin filtering.

use std::collections::HashMap;

pub type Affine = [[f64; 4]; 4];

/// Triangle mesh in world millimetres; `faces` index `vertices` (0-based).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<[f64; 3]>,
    pub faces: Vec<[u32; 3]>,
}

const SMOOTH_ITERATIONS: usize = 10;
const TAUBIN_LAMBDA: f64 = 0.5;
const TAUBIN_MU: f64 = -0.53;

fn apply(a: &Affine, p: [f64; 3]) -> [f64; 3] {
    [
        a[0][0] * p[0] + a[0][1] * p[1] + a[0][2] * p[2] + a[0][3],
        a[1][0] * p[0] + a[1][1] * p[1] + a[1][2] * p[2] + a[1][3],
        a[2][0] * p[0] + a[2][1] * p[1] + a[2][2] * p[2] + a[2][3],
    ]
}

fn inside(mask: &[u8], dims: [usize; 3], i: i64, j: i64, k: i64) -> bool {
    if i < 0 || j < 0 || k < 0 {
        return false;
    }
    let (i, j, k) = (i as usize, j as usize, k as usize);
    i < dims[0] && j < dims[1] && k < dims[2] && mask[i + dims[0] * (j + dims[1] * k)] != 0
}

fn vertex_id(
    ids: &mut HashMap<[i64; 3], u32>,
    vertices: &mut Vec<[f64; 3]>,
    corner: [i64; 3],
    affine: &Affine,
) -> u32 {
    *ids.entry(corner).or_insert_with(|| {
        let p = [
            corner[0] as f64 - 0.5,
            corner[1] as f64 - 0.5,
            corner[2] as f64 - 0.5,
        ];
        vertices.push(apply(affine, p));
        (vertices.len() - 1) as u32
    })
}

fn taubin(mesh: &mut Mesh) {
    let n = mesh.vertices.len();
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); n];
    for f in &mesh.faces {
        for e in 0..3 {
            let (a, b) = (f[e], f[(e + 1) % 3]);
            nb[a as usize].push(b);
            nb[b as usize].push(a);
        }
    }
    for list in &mut nb {
        list.sort_unstable();
        list.dedup();
    }
    for it in 0..2 * SMOOTH_ITERATIONS {
        let factor = if it % 2 == 0 {
            TAUBIN_LAMBDA
        } else {
            TAUBIN_MU
        };
        let prev = mesh.vertices.clone();
        for (v, list) in nb.iter().enumerate() {
            if list.is_empty() {
                continue;
            }
            let mut mean = [0.0; 3];
            for &o in list {
                for a in 0..3 {
                    mean[a] += prev[o as usize][a];
                }
            }
            for a in 0..3 {
                mean[a] /= list.len() as f64;
                mesh.vertices[v][a] = prev[v][a] + factor * (mean[a] - prev[v][a]);
            }
        }
    }
}

/// Boundary surface of `mask` (x fastest, `dims`); voxel centres map through `affine`.
pub fn surface(mask: &[u8], dims: [usize; 3], affine: &Affine) -> Mesh {
    let mut mesh = Mesh::default();
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    for k in 0..dims[2] as i64 {
        for j in 0..dims[1] as i64 {
            for i in 0..dims[0] as i64 {
                if !inside(mask, dims, i, j, k) {
                    continue;
                }
                for axis in 0..3 {
                    for sign in [-1i64, 1] {
                        let mut nbr = [i, j, k];
                        nbr[axis] += sign;
                        if inside(mask, dims, nbr[0], nbr[1], nbr[2]) {
                            continue;
                        }
                        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                        let plane = [i, j, k][axis] + i64::from(sign > 0);
                        let corner = |du: i64, dv: i64| {
                            let mut c = [0i64; 3];
                            c[axis] = plane;
                            c[u] = [i, j, k][u] + du;
                            c[v] = [i, j, k][v] + dv;
                            c
                        };
                        let quad = [corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1)];
                        let mut q = [0u32; 4];
                        for (slot, c) in q.iter_mut().zip(quad) {
                            *slot = vertex_id(&mut ids, &mut mesh.vertices, c, affine);
                        }
                        let outward_is_positive = sign > 0;
                        let (a, b, c, d) = if outward_is_positive {
                            (q[0], q[1], q[2], q[3])
                        } else {
                            (q[0], q[3], q[2], q[1])
                        };
                        mesh.faces.push([a, b, c]);
                        mesh.faces.push([a, c, d]);
                    }
                }
            }
        }
    }
    taubin(&mut mesh);
    mesh
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn identity() -> Affine {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    #[test]
    fn a_single_voxel_is_a_closed_cube_of_twelve_triangles() {
        let m = surface(&[1], [1, 1, 1], &identity());
        assert_eq!(m.faces.len(), 12);
        assert_eq!(m.vertices.len(), 8);
        assert!(m
            .faces
            .iter()
            .flatten()
            .all(|&i| (i as usize) < m.vertices.len()));
    }

    #[test]
    fn the_surface_encloses_the_mask_volume_after_smoothing() {
        let dims = [9, 9, 9];
        let mut mask = vec![0u8; 729];
        for k in 2..7usize {
            for j in 2..7usize {
                for i in 2..7usize {
                    mask[i + 9 * (j + 9 * k)] = 1;
                }
            }
        }
        let m = surface(&mask, dims, &identity());
        let mut vol = 0.0;
        for f in &m.faces {
            let (a, b, c) = (
                m.vertices[f[0] as usize],
                m.vertices[f[1] as usize],
                m.vertices[f[2] as usize],
            );
            vol += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0;
        }
        assert!(vol > 0.0, "{vol}");
        assert!((vol - 125.0).abs() < 25.0, "{vol}");
    }
}
