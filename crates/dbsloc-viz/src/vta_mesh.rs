// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Volume of tissue activated: binary grid to an iso-surface for the 3D scene (`specs/07` §7.8).

use std::path::Path;

pub use glam::Vec3;
use lin_alg::f32::Vec3 as McVec3;
use mcubes::{MarchingCubes, MeshSide};

use crate::atlas_marching::{blur3d_box_radius1, crop_field, nonzero_crop, read_nifti_volume_f32};
use crate::atlas_wavefront::TriMeshScene;
use crate::error::{VizError, VizResult};

/// Face colour of a volume of tissue activated: red, semi-transparent like an atlas structure.
pub const VTA_RGBA: [f32; 4] = [0.93, 0.27, 0.22, 0.6];

const PREBLUR_ITERATIONS: u32 = 2;

/// One hemisphere's surface in the scene's world space.
#[derive(Debug, Clone)]
pub struct VtaSurface {
    /// `right` or `left`.
    pub side: String,
    /// Stimulation label the surface belongs to.
    pub label: String,
    /// Surface triangles in world mm.
    pub mesh: TriMeshScene,
}

fn apply(affine: &[[f64; 4]; 4], p: [f32; 3]) -> Vec3 {
    let p = [p[0] as f64, p[1] as f64, p[2] as f64];
    let row = |r: usize| {
        (affine[r][0] * p[0] + affine[r][1] * p[1] + affine[r][2] * p[2] + affine[r][3]) as f32
    };
    Vec3::new(row(0), row(1), row(2))
}

/// Meshes a field (x fastest) at `level` and maps the vertices with the voxel-to-world `affine`.
pub fn mesh_field(
    dims: [usize; 3],
    field: Vec<f32>,
    level: f32,
    affine: &[[f64; 4]; 4],
) -> VizResult<TriMeshScene> {
    let empty = TriMeshScene {
        positions: Vec::new(),
        normals: Vec::new(),
        indices: Vec::new(),
    };
    let [nx, ny, nz] = dims;
    if nx * ny * nz != field.len() {
        return Err(VizError::NiftiLoad(
            "the grid size does not match the values".to_owned(),
        ));
    }
    let pad = PREBLUR_ITERATIONS as usize + 2;
    let Some((origin, sub_dims)) = nonzero_crop((nx, ny, nz), &field, pad) else {
        return Ok(empty);
    };
    let mut sub = crop_field((nx, ny, nz), &field, origin, sub_dims);
    blur3d_box_radius1(sub_dims, &mut sub, PREBLUR_ITERATIONS);
    let mc = MarchingCubes::new(
        sub_dims,
        (1.0, 1.0, 1.0),
        (1.0, 1.0, 1.0),
        McVec3::new(0.0, 0.0, 0.0),
        sub,
        level,
    )
    .map_err(|e| VizError::NiftiLoad(e.to_string()))?;
    let raw = mc.generate(MeshSide::Both);
    let o = origin.map(|v| v as f32);
    let mut mesh = TriMeshScene {
        positions: raw
            .vertices
            .iter()
            .map(|v| {
                apply(
                    affine,
                    [v.posit.x + o[0], v.posit.y + o[1], v.posit.z + o[2]],
                )
            })
            .collect(),
        normals: Vec::new(),
        indices: raw.indices.iter().map(|&i| i as u32).collect(),
    };
    mesh.recompute_vertex_normals();
    Ok(mesh)
}

/// Meshes a binary grid (x fastest) with the voxel-to-world `affine`.
pub fn mesh_binary_grid(
    dims: [usize; 3],
    data: &[u8],
    affine: &[[f64; 4]; 4],
) -> VizResult<TriMeshScene> {
    let field = data.iter().map(|&v| f32::from(v != 0)).collect();
    mesh_field(dims, field, 0.5, affine)
}

/// Meshes a written binary VTA image (`sub-<id>_sim-binary_…_hemi-<H>.nii`).
pub fn mesh_vta_nifti(path: &Path) -> VizResult<TriMeshScene> {
    let (hdr, n0, n1, n2, data) = read_nifti_volume_f32(path)?;
    let affine = fastwarp_core::nifti_affine::header_affine(&hdr);
    let field = data.iter().map(|&v| f32::from(v > 0.5)).collect();
    mesh_field([n0, n1, n2], field, 0.5, &affine)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [[f64; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    fn ball(n: usize, r: f32) -> Vec<u8> {
        let c = (n as f32 - 1.0) / 2.0;
        let mut v = vec![0u8; n * n * n];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let d =
                        ((i as f32 - c).powi(2) + (j as f32 - c).powi(2) + (k as f32 - c).powi(2))
                            .sqrt();
                    v[i + n * (j + n * k)] = u8::from(d <= r);
                }
            }
        }
        v
    }

    #[test]
    fn a_ball_gives_a_closed_surface_around_its_centre() {
        let mut affine = IDENTITY;
        affine[0][3] = 100.0;
        let mesh = mesh_binary_grid([16, 16, 16], &ball(16, 4.0), &affine).expect("mesh");
        assert!(mesh.indices.len() > 100);
        let n = mesh.positions.len() as f32;
        let centre: Vec3 = mesh.positions.iter().copied().sum::<Vec3>() / n;
        assert!(
            (centre - Vec3::new(107.5, 7.5, 7.5)).length() < 0.6,
            "{centre}"
        );
    }

    #[test]
    fn an_empty_grid_gives_an_empty_mesh() {
        let mesh = mesh_binary_grid([4, 4, 4], &[0; 64], &IDENTITY).expect("mesh");
        assert!(mesh.positions.is_empty());
    }

    #[test]
    fn a_size_mismatch_is_an_error() {
        assert!(mesh_binary_grid([4, 4, 4], &[0; 10], &IDENTITY).is_err());
    }
}
