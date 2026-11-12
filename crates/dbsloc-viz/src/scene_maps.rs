// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use crate::atlas_wavefront::TriMeshScene;
use crate::error::VizResult;
use crate::scene_fibres::{lerp_colour, pack_colour, NEGATIVE_PEAK, POSITIVE_PEAK};
use crate::scene_layers::{MapLayer, MapSettings};
use crate::vta_mesh::mesh_field;

pub const MAP_SURFACE_ALPHA: f32 = 0.5;
pub const SLICE_ALPHA: f32 = 0.85;

pub struct MapMeshes {
    pub positive: TriMeshScene,
    pub negative: TriMeshScene,
}

fn signed_part(values: &[f32], sign: f32) -> Vec<f32> {
    values
        .iter()
        .map(|&v| {
            let s = v * sign;
            if s.is_finite() && s > 0.0 {
                s
            } else {
                0.0
            }
        })
        .collect()
}

pub fn mesh_map(layer: &MapLayer, settings: &MapSettings) -> VizResult<MapMeshes> {
    let floor = settings.max_abs() * 1e-4;
    let level = settings.threshold.max(floor).max(f32::MIN_POSITIVE);
    let build = |sign: f32, peak: f32| -> VizResult<TriMeshScene> {
        if peak <= level {
            return Ok(TriMeshScene {
                positions: Vec::new(),
                normals: Vec::new(),
                indices: Vec::new(),
            });
        }
        mesh_field(
            layer.dims,
            signed_part(&layer.values, sign),
            level,
            &layer.affine,
        )
    };
    Ok(MapMeshes {
        positive: build(1.0, settings.max_pos)?,
        negative: build(-1.0, settings.max_neg)?,
    })
}

pub fn slice_rgba(value: f32, settings: &MapSettings) -> [u8; 4] {
    if !value.is_finite() || value.abs() < settings.threshold || value == 0.0 {
        return [255, 255, 255, 0];
    }
    let (peak, target) = if value > 0.0 {
        (settings.max_pos, POSITIVE_PEAK)
    } else {
        (settings.max_neg, NEGATIVE_PEAK)
    };
    let span = (peak - settings.threshold).max(f32::EPSILON);
    let t = (value.abs() - settings.threshold) / span;
    let [r, g, b] = lerp_colour([1.0; 3], target, t);
    pack_colour([r, g, b, SLICE_ALPHA])
}

pub fn sample_trilinear(dims: [usize; 3], values: &[f32], ijk: [f64; 3]) -> f32 {
    let [nx, ny, nz] = dims;
    let mut lo = [0isize; 3];
    let mut frac = [0.0_f32; 3];
    for a in 0..3 {
        let x = ijk[a];
        let n = dims[a] as f64;
        if !x.is_finite() || x < -0.5 || x > n - 0.5 {
            return 0.0;
        }
        let f = x.floor();
        lo[a] = f as isize;
        frac[a] = (x - f) as f32;
    }
    let at = |i: isize, j: isize, k: isize| -> f32 {
        if i < 0 || j < 0 || k < 0 || i as usize >= nx || j as usize >= ny || k as usize >= nz {
            return 0.0;
        }
        let v = values[(k as usize * ny + j as usize) * nx + i as usize];
        if v.is_finite() {
            v
        } else {
            0.0
        }
    };
    let mut acc = 0.0_f32;
    for dk in 0..2 {
        for dj in 0..2 {
            for di in 0..2 {
                let w = (if di == 1 { frac[0] } else { 1.0 - frac[0] })
                    * (if dj == 1 { frac[1] } else { 1.0 - frac[1] })
                    * (if dk == 1 { frac[2] } else { 1.0 - frac[2] });
                if w > 0.0 {
                    acc += w * at(lo[0] + di, lo[1] + dj, lo[2] + dk);
                }
            }
        }
    }
    acc
}

pub fn invert(affine: &[[f64; 4]; 4]) -> Option<[[f64; 4]; 4]> {
    fastwarp_core::invert_rowmajor(affine)
}

fn apply(m: &[[f64; 4]; 4], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

fn compose(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..4).map(|k| a[r][k] * b[k][c]).sum::<f64>()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plane {
    Axial,
    Coronal,
    Sagittal,
}

pub fn plane_texture_size(plane: Plane, n: [usize; 3]) -> (usize, usize) {
    match plane {
        Plane::Axial => (n[1], n[0]),
        Plane::Coronal => (n[0], n[2]),
        Plane::Sagittal => (n[1], n[2]),
    }
}

fn texel_voxel(plane: Plane, index: usize, col: usize, row: usize, n: [usize; 3]) -> [f64; 3] {
    let flip = |row: usize| (n[2] - 1 - row.min(n[2] - 1)) as f64;
    match plane {
        Plane::Axial => [row as f64, col as f64, index as f64],
        Plane::Coronal => [col as f64, index as f64, flip(row)],
        Plane::Sagittal => [index as f64, col as f64, flip(row)],
    }
}

pub fn resample_plane(
    layer: &MapLayer,
    settings: &MapSettings,
    backdrop_affine: &[[f64; 4]; 4],
    backdrop_dims: [usize; 3],
    plane: Plane,
    index: usize,
) -> Option<(Vec<u8>, u32, u32)> {
    let inv = invert(&layer.affine)?;
    let to_map = compose(&inv, backdrop_affine);
    let (w, h) = plane_texture_size(plane, backdrop_dims);
    let mut out = vec![0u8; w * h * 4];
    for row in 0..h {
        for col in 0..w {
            let ijk = apply(&to_map, texel_voxel(plane, index, col, row, backdrop_dims));
            let v = sample_trilinear(layer.dims, &layer.values, ijk);
            let px = slice_rgba(v, settings);
            let at = (row * w + col) * 4;
            out[at..at + 4].copy_from_slice(&px);
        }
    }
    Some((out, w as u32, h as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_layers::{value_extrema, MapMode};

    const IDENTITY: [[f64; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    fn blobs(n: usize) -> MapLayer {
        let mut values = vec![0.0_f32; n * n * n];
        let centres = [([5.0_f32, 8.0, 8.0], 1.0_f32), ([18.0, 8.0, 8.0], -0.8)];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    for (c, amp) in centres {
                        let d2 = (i as f32 - c[0]).powi(2)
                            + (j as f32 - c[1]).powi(2)
                            + (k as f32 - c[2]).powi(2);
                        values[(k * n + j) * n + i] += amp * (-d2 / 8.0).exp();
                    }
                }
            }
        }
        MapLayer::new(
            "blobs".to_owned(),
            "blobs".to_owned(),
            [n, n, n],
            IDENTITY,
            values,
            MapMode::Isosurface,
        )
    }

    fn settings(layer: &MapLayer, fraction: f32) -> MapSettings {
        let (max_pos, max_neg) = value_extrema(&layer.values);
        MapSettings {
            visible: true,
            mode: MapMode::Isosurface,
            threshold: fraction * max_pos.max(max_neg),
            max_pos,
            max_neg,
        }
    }

    fn centre(mesh: &TriMeshScene) -> glam::Vec3 {
        mesh.positions.iter().copied().sum::<glam::Vec3>() / mesh.positions.len() as f32
    }

    #[test]
    fn a_positive_and_a_negative_blob_give_one_surface_each() {
        let layer = blobs(24);
        let meshes = mesh_map(&layer, &settings(&layer, 0.25)).expect("mesh");
        assert!(meshes.positive.indices.len() > 60);
        assert!(meshes.negative.indices.len() > 60);
        let p = centre(&meshes.positive);
        let n = centre(&meshes.negative);
        assert!((p - glam::Vec3::new(5.0, 8.0, 8.0)).length() < 1.0, "{p}");
        assert!((n - glam::Vec3::new(18.0, 8.0, 8.0)).length() < 1.0, "{n}");
        assert!(meshes
            .positive
            .positions
            .iter()
            .chain(&meshes.negative.positions)
            .all(|v| v.is_finite()));
    }

    #[test]
    fn a_higher_threshold_shrinks_the_surface_and_above_the_peak_removes_it() {
        let layer = blobs(24);
        let low = mesh_map(&layer, &settings(&layer, 0.2)).expect("mesh");
        let high = mesh_map(&layer, &settings(&layer, 0.6)).expect("mesh");
        assert!(high.positive.indices.len() < low.positive.indices.len());
        let mut none = settings(&layer, 1.0);
        none.threshold *= 1.5;
        let gone = mesh_map(&layer, &none).expect("mesh");
        assert!(gone.positive.positions.is_empty() && gone.negative.positions.is_empty());
    }

    #[test]
    fn an_all_zero_map_meshes_to_nothing() {
        let mut layer = blobs(8);
        layer.values.iter_mut().for_each(|v| *v = 0.0);
        let meshes = mesh_map(&layer, &settings(&layer, 0.25)).expect("mesh");
        assert!(meshes.positive.positions.is_empty());
        assert!(meshes.negative.positions.is_empty());
    }

    #[test]
    fn slice_colours_are_transparent_below_the_threshold_and_two_sided_above() {
        let s = MapSettings {
            visible: true,
            mode: MapMode::Slices,
            threshold: 1.0,
            max_pos: 5.0,
            max_neg: 3.0,
        };
        assert_eq!(slice_rgba(0.5, &s)[3], 0);
        assert_eq!(slice_rgba(-0.5, &s)[3], 0);
        assert_eq!(slice_rgba(f32::NAN, &s)[3], 0);
        let top = slice_rgba(5.0, &s);
        assert_eq!(&top[..3], &pack_colour([0.9176, 0.2, 0.1373, 1.0])[..3]);
        assert_eq!(top[3], pack_colour([0.0, 0.0, 0.0, SLICE_ALPHA])[3]);
        let bottom = slice_rgba(-3.0, &s);
        assert_eq!(
            &bottom[..3],
            &pack_colour([0.2824, 0.6157, 0.9725, 1.0])[..3]
        );
        let just_in = slice_rgba(1.0, &s);
        assert_eq!(&just_in[..3], &[255, 255, 255]);
        assert!(just_in[3] > 0);
    }

    #[test]
    fn trilinear_sampling_interpolates_and_is_zero_outside() {
        let values = vec![0.0, 2.0, 0.0, 2.0, 0.0, 2.0, 0.0, 2.0];
        let dims = [2, 2, 2];
        assert_eq!(sample_trilinear(dims, &values, [0.5, 0.0, 0.0]), 1.0);
        assert_eq!(sample_trilinear(dims, &values, [1.0, 1.0, 1.0]), 2.0);
        assert_eq!(sample_trilinear(dims, &values, [2.0, 0.0, 0.0]), 0.0);
        assert_eq!(sample_trilinear(dims, &values, [f64::NAN, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn a_resampled_axial_slice_has_the_blob_at_its_voxel_position() {
        let layer = blobs(24);
        let s = settings(&layer, 0.5);
        let (px, w, h) =
            resample_plane(&layer, &s, &IDENTITY, [24, 24, 24], Plane::Axial, 8).expect("resample");
        assert_eq!((w, h), (24, 24));
        let alpha = |row: usize, col: usize| px[(row * 24 + col) * 4 + 3];
        assert!(alpha(5, 8) > 0);
        assert!(alpha(18, 8) > 0);
        assert_eq!(alpha(12, 8), 0);
        assert_eq!(alpha(5, 20), 0);
    }

    #[test]
    fn coronal_and_sagittal_slices_flip_the_rows_so_superior_is_up() {
        let mut layer = blobs(24);
        layer.values.iter_mut().for_each(|v| *v = 0.0);
        layer.values[(20 * 24 + 8) * 24 + 5] = 1.0;
        let s = settings(&layer, 0.5);
        let (px, w, _) = resample_plane(&layer, &s, &IDENTITY, [24, 24, 24], Plane::Coronal, 8)
            .expect("resample");
        let w = w as usize;
        let row = 24 - 1 - 20;
        assert!(px[(row * w + 5) * 4 + 3] > 0);
        let (px, w, _) = resample_plane(&layer, &s, &IDENTITY, [24, 24, 24], Plane::Sagittal, 5)
            .expect("resample");
        let w = w as usize;
        assert!(px[(row * w + 8) * 4 + 3] > 0);
    }

    #[test]
    fn a_translated_map_is_found_at_its_world_position() {
        let mut layer = blobs(24);
        layer.affine[0][3] = 10.0;
        let s = settings(&layer, 0.5);
        let (px, w, _) =
            resample_plane(&layer, &s, &IDENTITY, [48, 24, 24], Plane::Axial, 8).expect("resample");
        let w = w as usize;
        assert!(px[(15 * w + 8) * 4 + 3] > 0);
        assert_eq!(px[(5 * w + 8) * 4 + 3], 0);
    }
}
