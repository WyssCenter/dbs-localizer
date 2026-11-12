// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The `f64` CPU reference backend.
//!
//! Every kernel is data-parallel over voxels with rayon; reductions sum in a fixed chunk order
//! (`field_ops::ordered_sum`), so two runs produce bit-identical results whatever the number
//! of threads.

use rayon::prelude::*;

use crate::backend::Backend;
use crate::error::Result;
use crate::field_ops::{
    compose_displacement_fields, gaussian_smooth_displacement_field, invert_displacement_field,
    resample_field, InvertParams, InvertStats, VectorField,
};
use crate::grid::Grid;
use crate::interp::linear_at_point;
use crate::metric::cc::{CcInputs, CcMetric};
use crate::transform::{AffineTransform, TransformChain};
use crate::volume::Volume;

/// The reference backend: images are `Volume<f64>`, fields are [`VectorField`].
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuBackend;

impl Backend for CpuBackend {
    type Image = Volume<f64>;
    type Field = VectorField;

    fn name(&self) -> String {
        "cpu-f64".to_string()
    }

    fn upload_image(&self, volume: &Volume<f64>) -> Result<Self::Image> {
        Ok(volume.clone())
    }

    fn upload_image_owned(&self, volume: Volume<f64>) -> Result<Self::Image> {
        Ok(volume)
    }

    fn zero_field(&self, grid: &Grid) -> Result<Self::Field> {
        Ok(VectorField::zeros(*grid))
    }

    fn upload_field(&self, field: &VectorField) -> Result<Self::Field> {
        Ok(field.clone())
    }

    fn download_field(&self, field: &Self::Field) -> Result<VectorField> {
        Ok(field.clone())
    }

    fn field_grid(&self, field: &Self::Field) -> Grid {
        *field.grid()
    }

    fn copy_field(&self, field: &Self::Field) -> Result<Self::Field> {
        Ok(field.clone())
    }

    fn warp_through_field(
        &self,
        source: &Self::Image,
        field: &Self::Field,
        affine: Option<&AffineTransform>,
    ) -> Result<Self::Image> {
        let grid = *field.grid();
        let data: Vec<f64> = field
            .data()
            .par_iter()
            .enumerate()
            .map(|(idx, u)| {
                let [i, j, k] = grid.ijk(idx);
                let x = grid.index_to_point([i as f64, j as f64, k as f64]);
                let mut p = [x[0] + u[0], x[1] + u[1], x[2] + u[2]];
                if let Some(a) = affine {
                    p = a.apply(p);
                }
                linear_at_point(source, p).unwrap_or(0.0)
            })
            .collect();
        Volume::from_vec(grid, data)
    }

    fn cc_value_derivative(
        &self,
        fixed: &Self::Image,
        moving: &Self::Image,
        radius: usize,
    ) -> Result<(f64, Self::Field)> {
        let identity = TransformChain::new();
        let grid = *fixed.grid();
        let evaluation = CcMetric::new(radius).evaluate(
            &CcInputs {
                virtual_grid: &grid,
                fixed,
                moving,
                fixed_transform: &identity,
                moving_transform: &identity,
            },
            true,
        )?;
        Ok((
            evaluation.value,
            VectorField::from_vec(grid, evaluation.derivative)?,
        ))
    }

    fn smooth_field(&self, field: Self::Field, variance: f64) -> Result<Self::Field> {
        if variance <= 0.0 {
            return Ok(field);
        }
        Ok(gaussian_smooth_displacement_field(&field, variance))
    }

    fn max_scaled_norm(&self, field: &Self::Field) -> Result<f64> {
        Ok(field.max_scaled_norm())
    }

    fn l2_norm(&self, field: &Self::Field) -> Result<f64> {
        Ok(field.l2_norm())
    }

    fn scale(&self, field: &mut Self::Field, s: f64) -> Result<()> {
        field.scale(s);
        Ok(())
    }

    fn axpy(&self, acc: &mut Self::Field, a: f64, x: &Self::Field) -> Result<()> {
        acc.axpy(a, x)
    }

    fn compose(&self, displacement: &Self::Field, warping: &Self::Field) -> Result<Self::Field> {
        compose_displacement_fields(displacement, warping)
    }

    fn invert(
        &self,
        field: &Self::Field,
        initial: Self::Field,
        params: &InvertParams,
    ) -> Result<(Self::Field, InvertStats)> {
        invert_displacement_field(field, Some(&initial), params)
    }

    fn resample_field(&self, field: &Self::Field, grid: &Grid) -> Result<Self::Field> {
        resample_field(field, grid)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;

    fn grid() -> Grid {
        Grid::new([8, 7, 6], [1.5, 1.5, 2.0], [-3.0, 2.0, 1.0], IDENTITY3).unwrap()
    }

    fn ramp(g: Grid) -> Volume<f64> {
        let data = (0..g.num_voxels())
            .map(|idx| {
                let [i, j, k] = g.ijk(idx);
                2.0 * i as f64 + 5.0 * j as f64 - k as f64
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    }

    #[test]
    fn warp_through_a_zero_field_is_the_identity_and_translates_with_an_affine() {
        let b = CpuBackend;
        let g = grid();
        let v = ramp(g);
        let zero = b.zero_field(&g).unwrap();
        let same = b.warp_through_field(&v, &zero, None).unwrap();
        for (a, c) in same.data().iter().zip(v.data()) {
            assert!((a - c).abs() < 1e-12);
        }
        let shift = AffineTransform::from_translation([3.0, 0.0, 0.0]);
        let moved = b.warp_through_field(&v, &zero, Some(&shift)).unwrap();
        let at = |i, j, k| moved.data()[g.linear_index(i, j, k)];
        assert!((at(2, 3, 2) - (2.0 * 4.0 + 15.0 - 2.0)).abs() < 1e-12);
        assert_eq!(at(7, 3, 2), 0.0);
    }

    #[test]
    fn warp_applies_the_field_before_the_affine() {
        let b = CpuBackend;
        let g = grid();
        let v = ramp(g);
        let field = VectorField::from_fn(g, |_| [3.0, 0.0, 0.0]);
        let scale = AffineTransform::from_offset(
            [[0.5, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [0.0; 3],
        );
        let f = b.upload_field(&field).unwrap();
        let out = b.warp_through_field(&v, &f, Some(&scale)).unwrap();
        let x = g.index_to_point([1.0, 2.0, 2.0]);
        let q = [0.5 * (x[0] + 3.0), x[1], x[2]];
        let got = out.data()[g.linear_index(1, 2, 2)];
        match linear_at_point(&v, q) {
            Some(w) => assert!((got - w).abs() < 1e-12),
            None => assert_eq!(got, 0.0),
        }
        let swapped = linear_at_point(&v, [0.5 * x[0] + 3.0, x[1], x[2]]);
        assert_ne!(swapped, linear_at_point(&v, q));
    }

    #[test]
    fn cc_of_an_image_with_itself_is_minus_one_with_zero_derivative() {
        let b = CpuBackend;
        let g = Grid::new([9, 9, 9], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let data = (0..g.num_voxels())
            .map(|i| (((i * 2_654_435_761usize) % 997) as f64) / 997.0)
            .collect();
        let v = Volume::from_vec(g, data).unwrap();
        let (value, d) = b.cc_value_derivative(&v, &v, 2).unwrap();
        assert!((value + 1.0).abs() < 1e-12);
        assert!(d.data().iter().all(|c| c.iter().all(|x| x.abs() < 1e-9)));
    }

    #[test]
    fn the_default_cc_pair_is_two_calls_and_smoothing_by_value_keeps_a_zero_variance_field() {
        let b = CpuBackend;
        let g = Grid::new([9, 8, 7], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let image = |seed: usize| {
            let data = (0..g.num_voxels())
                .map(|i| (((i + seed) * 2_654_435_761usize) % 997) as f64 / 997.0)
                .collect();
            Volume::from_vec(g, data).unwrap()
        };
        let (a, c) = (image(1), image(5));
        let ((v1, d1), (v2, d2)) = b.cc_value_derivative_pair(&a, &c, 2).unwrap();
        let (w1, e1) = b.cc_value_derivative(&a, &c, 2).unwrap();
        let (w2, e2) = b.cc_value_derivative(&c, &a, 2).unwrap();
        assert_eq!((v1.to_bits(), v2.to_bits()), (w1.to_bits(), w2.to_bits()));
        assert_eq!(d1.data(), e1.data());
        assert_eq!(d2.data(), e2.data());
        let kept = b.smooth_field(d1.clone(), 0.0).unwrap();
        assert_eq!(kept.data(), d1.data());
        let init = VectorField::zeros(g);
        let (inv, _) = b.invert(&d1, init, &InvertParams::SYN).unwrap();
        assert_eq!(inv.grid(), &g);
    }

    #[test]
    fn name_and_basic_reductions() {
        let b = CpuBackend;
        assert_eq!(b.name(), "cpu-f64");
        let g = grid();
        let mut f = b
            .upload_field(&VectorField::from_fn(g, |_| [3.0, 0.0, 4.0]))
            .unwrap();
        assert!((b.l2_norm(&f).unwrap() - 5.0 * (g.num_voxels() as f64).sqrt()).abs() < 1e-9);
        b.scale(&mut f, 2.0).unwrap();
        let h = b.copy_field(&f).unwrap();
        b.axpy(&mut f, -1.0, &h).unwrap();
        assert_eq!(b.max_scaled_norm(&f).unwrap(), 0.0);
        assert_eq!(b.field_grid(&f), g);
        assert_eq!(b.download_field(&h).unwrap().data()[0], [6.0, 0.0, 8.0]);
    }
}
