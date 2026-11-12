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
// Portions derived from ITK `itkImageMomentsCalculator.hxx` (`Compute`: intensity-weighted first
// moment in physical coordinates over the whole image, error for zero total mass) and
// `itkCenteredTransformInitializer.hxx` (`InitializeTransform`: moments and geometry modes: rotation
// centre = fixed centre, translation = moving centre - fixed centre), and from ANTs
// `itkantsRegistrationHelper.h` (`GetCompositeTransformFromParserOption`: the
// `--initial-moving-transform [fixed,moving,feature]` modes 0 image centres, 1 centre of mass,
// 2 image origins). See `docs/THIRD_PARTY_PORTS.md`.

//! Initial alignment of `--initial-moving-transform [fixed,moving,feature]`.

use crate::error::{invalid, Result};
use crate::linalg::{vec3_sub, Vec3};
use crate::transform::AffineTransform;
use crate::volume::Volume;

/// The `feature` argument of `--initial-moving-transform [fixed,moving,feature]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitFeature {
    /// 1: centres of intensity mass.
    CentreOfMass,
}

/// ITK `ImageMomentsCalculator::GetCenterOfGravity`: the intensity-weighted mean physical
/// position (LPS mm) over every voxel; negative intensities weigh negatively. Fails when the
/// total mass is zero.
pub fn centre_of_gravity(vol: &Volume<f64>) -> Result<Vec3> {
    let g = vol.grid();
    let [nx, ny, nz] = g.size();
    let mut mass = 0.0f64;
    let mut cg = [0.0f64; 3];
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let v = vol.data()[g.linear_index(i, j, k)];
                let p = g.index_to_point([i as f64, j as f64, k as f64]);
                mass += v;
                cg[0] += p[0] * v;
                cg[1] += p[1] * v;
                cg[2] += p[2] * v;
            }
        }
    }
    if mass == 0.0 {
        return Err(invalid(
            "initial transform",
            "total mass of the image is zero",
        ));
    }
    Ok([cg[0] / mass, cg[1] / mass, cg[2] / mass])
}

/// The initial `Euler3DTransform` of the feature with identity rotation: rotation centre = fixed
/// reference point, translation = moving reference point - fixed reference point. Maps fixed
/// points to moving points.
pub fn initial_transform(
    fixed: &Volume<f64>,
    moving: &Volume<f64>,
    feature: InitFeature,
) -> Result<AffineTransform> {
    let (fc, mc) = match feature {
        InitFeature::CentreOfMass => (centre_of_gravity(fixed)?, centre_of_gravity(moving)?),
    };
    let t = vec3_sub(mc, fc);
    Ok(AffineTransform::new(crate::linalg::IDENTITY3, t, fc))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::linalg::IDENTITY3;

    fn blob(g: &Grid, c: Vec3) -> Volume<f64> {
        let data = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let p = g.index_to_point([i as f64, j as f64, k as f64]);
                let r2 = (p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) + (p[2] - c[2]).powi(2);
                (-r2 / 18.0).exp()
            })
            .collect();
        Volume::from_vec(*g, data).unwrap()
    }

    #[test]
    fn centre_of_mass_of_a_blob_is_its_centre() {
        let g = Grid::new(
            [40, 36, 32],
            [1.0, 1.5, 2.0],
            [-20.0, -30.0, -10.0],
            IDENTITY3,
        )
        .unwrap();
        let v = blob(&g, [3.0, -4.0, 5.0]);
        let c = centre_of_gravity(&v).unwrap();
        assert!(
            (c[0] - 3.0).abs() < 1e-3 && (c[1] + 4.0).abs() < 1e-3 && (c[2] - 5.0).abs() < 1e-3
        );
    }

    #[test]
    fn com_initialiser_translates_fixed_centre_onto_moving_centre() {
        let g = Grid::new([40, 36, 32], [1.0; 3], [-20.0, -18.0, -16.0], IDENTITY3).unwrap();
        let f = blob(&g, [0.0, 0.0, 0.0]);
        let m = blob(&g, [6.0, -3.0, 2.0]);
        let t = initial_transform(&f, &m, InitFeature::CentreOfMass).unwrap();
        let fc = centre_of_gravity(&f).unwrap();
        assert_eq!(t.center, fc);
        let mapped = t.apply(fc);
        assert!((mapped[0] - 6.0).abs() < 1e-3 && (mapped[1] + 3.0).abs() < 1e-3);
    }

    #[test]
    fn zero_mass_is_an_error() {
        let g = Grid::new([4, 4, 4], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        assert!(centre_of_gravity(&Volume::zeros(g)).is_err());
    }
}
