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
// Portions derived from ITK `itkWindowedSincInterpolateImageFunction.hxx` (`VRadius = 3`, the
// `LanczosWindowFunction`, the weight table indexed from offset `1 - VRadius` and the exact-grid
// special case) as `antsApplyTransforms -n LanczosWindowedSinc` runs it, taps outside the image
// reading zero (measured against ANTsPy's antsApplyTransforms), and from `antsApplyTransforms --compose` (the displacement
// `T(x) - x` sampled at every reference voxel). See `docs/THIRD_PARTY_PORTS.md`.

//! What a caller does with the transforms after the registration, in parallel Rust instead of
//! single-threaded ITK: the composite as a dense displacement field on a reference grid
//! (`antsApplyTransforms --compose`), and an image resampled through it with the linear, nearest
//! or Lanczos windowed-sinc interpolator (`antsApplyTransforms -n ...`).

use std::f64::consts::PI;
use std::path::Path;

use fastwarp_niftiio::WriterOptions;
use fastwarp_reg::apply::{apply_chain, Interpolation};
use fastwarp_reg::interp::is_inside_point;
use fastwarp_reg::nifti_io::header_from_grid;
use fastwarp_reg::transform::field_nifti::INTENT_VECTOR;
use fastwarp_reg::transform::{DisplacementField, TransformChain};
use fastwarp_reg::{Grid, RegError, Result, Volume};
use ndarray::{ArrayView5, ShapeBuilder};
use rayon::prelude::*;

/// Interpolators of [`resample`]; the names are ANTs' (`antsApplyTransforms -n`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Linear,
    NearestNeighbor,
    LanczosWindowedSinc,
}

impl Interp {
    pub fn from_ants_name(name: &str) -> Option<Self> {
        match name {
            "linear" | "Linear" => Some(Self::Linear),
            "nearestNeighbor" | "NearestNeighbor" => Some(Self::NearestNeighbor),
            "lanczosWindowedSinc" | "LanczosWindowedSinc" => Some(Self::LanczosWindowedSinc),
            _ => None,
        }
    }
}

/// `T(x) - x` at every voxel centre `x` of `reference` (LPS mm), as `antsApplyTransforms
/// --compose` writes it.
pub fn compose_field(chain: &TransformChain, reference: &Grid) -> Result<DisplacementField> {
    let mut data = vec![0f32; reference.num_voxels() * 3];
    data.par_chunks_mut(3).enumerate().for_each(|(idx, out)| {
        let [i, j, k] = reference.ijk(idx);
        let x = reference.index_to_point([i as f64, j as f64, k as f64]);
        let p = chain.apply(x);
        for a in 0..3 {
            out[a] = (p[a] - x[a]) as f32;
        }
    });
    DisplacementField::new(*reference, data)
}

/// [`fastwarp_reg::transform::field_nifti::write_field_nifti`] without its serial copy of the
/// field into a fresh array: the writer reads the interleaved `[x y z]` data through a strided
/// view. Same file: `f32`, `dim = [5, nx, ny, nz, 1, 3]`, intent `NIFTI_INTENT_VECTOR`.
/// `level` is the gzip level of a `.nii.gz` (`None`: the writer's default, 1).
pub fn write_field(path: &Path, field: &DisplacementField, level: Option<u32>) -> Result<()> {
    let g = field.grid();
    let [nx, ny, nz] = g.size();
    let view = ArrayView5::from_shape(
        (nx, ny, nz, 1, 3).strides((3, 3 * nx, 3 * nx * ny, 1, 1)),
        field.data(),
    )
    .map_err(|e| RegError::Nifti {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut hdr = header_from_grid(g);
    hdr.intent_code = INTENT_VECTOR;
    let mut opts = WriterOptions::new(path).reference_header(&hdr);
    if let (Some(level), true) = (level, path.to_string_lossy().ends_with(".gz")) {
        opts = opts.compression_level(level);
    }
    opts.write_nifti(&view).map_err(|e| RegError::Nifti {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

/// `image` resampled onto `reference` through `chain`; points outside the image buffer take
/// `default_value`.
pub fn resample(
    image: &Volume<f64>,
    reference: &Grid,
    chain: &TransformChain,
    interp: Interp,
    default_value: f64,
) -> Result<Vec<f32>> {
    let values: Vec<f64> = match interp {
        Interp::Linear => {
            apply_chain(
                image,
                reference,
                chain,
                Interpolation::Linear,
                default_value,
            )?
            .into_parts()
            .1
        }
        Interp::NearestNeighbor => {
            apply_chain(
                image,
                reference,
                chain,
                Interpolation::NearestNeighbor,
                default_value,
            )?
            .into_parts()
            .1
        }
        Interp::LanczosWindowedSinc => {
            return Ok(lanczos_resample(image, reference, chain, default_value))
        }
    };
    Ok(values.into_iter().map(|v| v as f32).collect())
}

const RADIUS: usize = 3;
const TAPS: usize = 2 * RADIUS;

/// ITK's weights for the taps at offsets `1 - R ..= R` from `floor(c)`, `d = c - floor(c)`:
/// `sinc(x) * lanczos(x)` at `x = d - offset`, or a single 1 at offset 0 when `d == 0`.
///
/// With `x = d - o` and integer `o`, `sin(pi x) = (-1)^o sin(pi d)` and
/// `sin(pi x / R) = sin(pi d / R) cos(pi o / R) - cos(pi d / R) sin(pi o / R)`, so one `sin` and
/// one `sin_cos` per axis replace ITK's two `sin` per tap; the weights agree to rounding.
fn lanczos_weights(d: f64) -> [f64; TAPS] {
    let mut w = [0.0; TAPS];
    if d == 0.0 {
        w[RADIUS - 1] = 1.0;
        return w;
    }
    let r = RADIUS as f64;
    let s_pi = (PI * d).sin();
    let (s_r, c_r) = (PI * d / r).sin_cos();
    for (i, wi) in w.iter_mut().enumerate() {
        let o = i as f64 + 1.0 - r;
        let x = d - o;
        // (-1)^o with o = i + 1 - R; i + 1 + R has the same parity and cannot underflow.
        let sign = if (i + 1 + RADIUS).is_multiple_of(2) {
            1.0
        } else {
            -1.0
        };
        let (so, co) = (PI * o / r).sin_cos();
        let sin_x = sign * s_pi;
        let sin_x_r = s_r * co - c_r * so;
        *wi = (sin_x / (PI * x)) * (sin_x_r / (PI * x / r));
    }
    w
}

fn lanczos_resample(
    image: &Volume<f64>,
    reference: &Grid,
    chain: &TransformChain,
    default_value: f64,
) -> Vec<f32> {
    let g = image.grid();
    let size = g.size();
    let data = image.data();
    (0..reference.num_voxels())
        .into_par_iter()
        .map(|idx| {
            let [i, j, k] = reference.ijk(idx);
            let p = chain.apply(reference.index_to_point([i as f64, j as f64, k as f64]));
            if !is_inside_point(image, p) {
                return default_value as f32;
            }
            let c = g.point_to_index(p);
            let mut w = [[0.0; TAPS]; 3];
            let mut at = [[0usize; TAPS]; 3];
            for a in 0..3 {
                let base = c[a].floor();
                w[a] = lanczos_weights(c[a] - base);
                let n = size[a] as i64;
                for t in 0..TAPS {
                    // A tap outside the image reads zero, as in antsApplyTransforms (not the
                    // edge voxel): its weight is dropped.
                    let at_t = base as i64 + t as i64 + 1 - RADIUS as i64;
                    if (0..n).contains(&at_t) {
                        at[a][t] = at_t as usize;
                    } else {
                        w[a][t] = 0.0;
                    }
                }
            }
            let mut acc = 0.0;
            for kz in 0..TAPS {
                if w[2][kz] == 0.0 {
                    continue;
                }
                let mut acc_y = 0.0;
                for jy in 0..TAPS {
                    if w[1][jy] == 0.0 {
                        continue;
                    }
                    let row = size[0] * (at[1][jy] + size[1] * at[2][kz]);
                    let mut acc_x = 0.0;
                    for ix in 0..TAPS {
                        acc_x += w[0][ix] * data[row + at[0][ix]];
                    }
                    acc_y += w[1][jy] * acc_x;
                }
                acc += w[2][kz] * acc_y;
            }
            acc as f32
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use fastwarp_reg::transform::{AffineTransform, TransformElem};

    /// ITK's loop, literally: `x = d + R`, then `x -= 1` and `window(x) * sinc(x)` per tap.
    fn itk_weights(d: f64) -> [f64; TAPS] {
        let mut w = [0.0; TAPS];
        if d == 0.0 {
            w[RADIUS - 1] = 1.0;
            return w;
        }
        let sinc = |x: f64| {
            if x == 0.0 {
                1.0
            } else {
                (PI * x).sin() / (PI * x)
            }
        };
        let window = |x: f64| {
            let z = PI / RADIUS as f64 * x;
            if x == 0.0 {
                1.0
            } else {
                z.sin() / z
            }
        };
        let mut x = d + RADIUS as f64;
        for wi in &mut w {
            x -= 1.0;
            *wi = window(x) * sinc(x);
        }
        w
    }

    #[test]
    fn weights_match_itks_direct_evaluation() {
        for n in 0..1000 {
            let d = n as f64 / 1000.0 + 1e-7 * (n % 3) as f64;
            let (a, b) = (lanczos_weights(d), itk_weights(d));
            for t in 0..TAPS {
                assert!(
                    (a[t] - b[t]).abs() < 1e-12,
                    "d={d} tap {t}: {} vs {}",
                    a[t],
                    b[t]
                );
            }
        }
    }

    fn grid(n: usize) -> Grid {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        Grid::new([n, n + 1, n + 2], [1.0; 3], [0.0; 3], id).unwrap()
    }

    #[test]
    fn identity_reproduces_the_image_exactly() {
        let g = grid(9);
        let vals: Vec<f64> = (0..g.num_voxels())
            .map(|i| (i as f64 * 0.37).sin())
            .collect();
        let img = Volume::from_vec(g, vals.clone()).unwrap();
        let out = resample(
            &img,
            &g,
            &TransformChain::new(),
            Interp::LanczosWindowedSinc,
            0.0,
        )
        .unwrap();
        for (a, b) in out.iter().zip(&vals) {
            assert_eq!(*a, *b as f32);
        }
    }

    #[test]
    fn taps_outside_the_image_read_zero() {
        // A 1-voxel-thick image: every tap but the centre one lies outside along z.
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let g = Grid::new([8, 8, 1], [1.0; 3], [0.0; 3], id).unwrap();
        let img = Volume::from_vec(g, vec![1.0; g.num_voxels()]).unwrap();
        let mut chain = TransformChain::new();
        chain.push(TransformElem::Affine(AffineTransform::from_translation([
            0.0, 0.0, 0.25,
        ])));
        let out = resample(&img, &g, &chain, Interp::LanczosWindowedSinc, -1.0).unwrap();
        let wz = lanczos_weights(0.25)[RADIUS - 1];
        let v = out[g.linear_index(4, 4, 0)];
        assert!((f64::from(v) - wz).abs() < 1e-6, "{v} vs {wz}");
    }

    #[test]
    fn a_constant_image_stays_constant_and_outside_is_the_default() {
        let g = grid(10);
        let img = Volume::from_vec(g, vec![2.5; g.num_voxels()]).unwrap();
        let mut chain = TransformChain::new();
        chain.push(TransformElem::Affine(AffineTransform::from_translation([
            0.3, -0.45, 4.2,
        ])));
        let out = resample(&img, &g, &chain, Interp::LanczosWindowedSinc, -1.0).unwrap();
        let (mut inside, mut outside) = (0, 0);
        for (idx, v) in out.iter().enumerate() {
            let [i, j, k] = g.ijk(idx);
            let src = [i as f64 + 0.3, j as f64 - 0.45, k as f64 + 4.2];
            let n = g.size();
            if src[2] < n[2] as f64 - 0.5 {
                // Away from the border every tap is inside; the Lanczos weights do not sum to
                // exactly 1 and ITK does not normalise them either.
                let interior = (0..3).all(|a| src[a] >= 3.0 && src[a] <= n[a] as f64 - 4.0);
                if interior {
                    assert!((v - 2.5).abs() < 0.1, "({i},{j},{k}) = {v}");
                    inside += 1;
                }
            } else {
                assert_eq!(*v, -1.0);
                outside += 1;
            }
        }
        assert!(inside > 0 && outside > 0);
    }

    #[test]
    fn the_strided_writer_writes_the_engines_file() {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let g = Grid::new([4, 5, 6], [1.5, 2.0, 2.5], [1.0, -2.0, 3.0], id).unwrap();
        let f = DisplacementField::from_fn(g, |p| [p[0] * 0.1, -p[1], p[2] * p[0]]);
        let dir = std::env::temp_dir().join(format!("fastwarp_post_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["f.nii", "f.nii.gz"] {
            let (a, b) = (dir.join(format!("a_{name}")), dir.join(format!("b_{name}")));
            write_field(&a, &f, None).unwrap();
            fastwarp_reg::transform::field_nifti::write_field_nifti(&b, &f).unwrap();
            assert_eq!(
                std::fs::read(&a).unwrap(),
                std::fs::read(&b).unwrap(),
                "{name}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn composed_field_is_the_point_map_minus_the_point() {
        let g = grid(5);
        let mut chain = TransformChain::new();
        chain.push(TransformElem::Affine(AffineTransform::from_offset(
            [[1.1, 0.1, 0.0], [0.0, 0.9, 0.0], [0.0, 0.0, 1.0]],
            [1.0, -2.0, 0.5],
        )));
        let f = compose_field(&chain, &g).unwrap();
        for idx in 0..g.num_voxels() {
            let [i, j, k] = g.ijk(idx);
            let x = g.index_to_point([i as f64, j as f64, k as f64]);
            let p = chain.apply(x);
            let u = f.get(i, j, k).unwrap();
            for a in 0..3 {
                assert!((u[a] - (p[a] - x[a])).abs() < 1e-5);
            }
        }
    }
}
