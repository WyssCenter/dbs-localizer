// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The compute backend a registration loop runs on.
//!
//! The loop in `syn` is written once against [`Backend`]; the CPU implementation in `cpu` is the
//! `f64` reference, a GPU implementation provides the same operations on device buffers. Every
//! operation is a whole-volume kernel; scalars cross the boundary as `f64`.

use crate::error::Result;
use crate::field_ops::{InvertParams, InvertStats, VectorField};
use crate::grid::Grid;
use crate::transform::AffineTransform;
use crate::volume::Volume;

/// A metric value with its derivative field.
pub type MetricEvaluation<F> = (f64, F);

/// The two directions of one metric evaluation: `(fixed, moving)` then `(moving, fixed)`.
pub type MetricPair<F> = (MetricEvaluation<F>, MetricEvaluation<F>);

/// An inverted field with the statistics of its inversion.
pub type Inversion<F> = (F, InvertStats);

/// The two inversions of [`Backend::invert_pair`] in call order: the new inverse, then the new
/// field.
pub type InversionPair<F> = (Inversion<F>, Inversion<F>);

/// Operations the symmetric normalisation loop needs.
///
/// Scalar images ([`Backend::Image`]) and displacement fields ([`Backend::Field`]) are opaque
/// buffers owned by the backend. Fields always live on the grid they were created with.
pub trait Backend: Sync {
    /// A scalar volume buffer.
    type Image: Send + Sync;
    /// A displacement-field buffer (three components per voxel).
    type Field: Send + Sync;

    /// A short name for records and logs.
    fn name(&self) -> String;

    /// Moves a volume onto the backend.
    fn upload_image(&self, volume: &Volume<f64>) -> Result<Self::Image>;

    fn upload_image_owned(&self, volume: Volume<f64>) -> Result<Self::Image> {
        self.upload_image(&volume)
    }

    /// A zero displacement field on `grid`.
    fn zero_field(&self, grid: &Grid) -> Result<Self::Field>;

    /// Moves a field onto the backend.
    fn upload_field(&self, field: &VectorField) -> Result<Self::Field>;

    /// Brings a field back to the host.
    fn download_field(&self, field: &Self::Field) -> Result<VectorField>;

    /// The grid of a field.
    fn field_grid(&self, field: &Self::Field) -> Grid;

    /// A copy of a field.
    fn copy_field(&self, field: &Self::Field) -> Result<Self::Field>;

    /// Resamples `source` onto the grid of `field`: output voxel `x` takes the linearly
    /// interpolated value of `source` at `affine(x + field(x))` (the field first, then the
    /// optional affine), and zero where that point lies outside `source`'s buffer.
    fn warp_through_field(
        &self,
        source: &Self::Image,
        field: &Self::Field,
        affine: Option<&AffineTransform>,
    ) -> Result<Self::Image>;

    /// ANTs neighbourhood cross-correlation of two images on the same grid: the metric value
    /// (`-mean(localCC)`) and the dense image-space derivative with respect to the moving image
    /// (`coefficient * grad(moving)` per voxel, not divided by the voxel count).
    fn cc_value_derivative(
        &self,
        fixed: &Self::Image,
        moving: &Self::Image,
        radius: usize,
    ) -> Result<(f64, Self::Field)>;

    /// Both directions of [`Backend::cc_value_derivative`] on one image pair: first
    /// `(fixed, moving)`, then `(moving, fixed)`. The default evaluates them one after the other;
    /// a backend that can share the symmetric window sums overrides it with bit-identical results.
    fn cc_value_derivative_pair(
        &self,
        fixed: &Self::Image,
        moving: &Self::Image,
        radius: usize,
    ) -> Result<MetricPair<Self::Field>> {
        let forward = self.cc_value_derivative(fixed, moving, radius)?;
        let backward = self.cc_value_derivative(moving, fixed, radius)?;
        Ok((forward, backward))
    }

    /// [`Backend::cc_value_derivative_pair`] with the [`Backend::l2_norm`] of both derivatives,
    /// in the same order. The default calls them one after the other; a backend that can return
    /// all the scalars at once overrides it with bit-identical results.
    fn cc_value_derivative_pair_with_norms(
        &self,
        fixed: &Self::Image,
        moving: &Self::Image,
        radius: usize,
    ) -> Result<(MetricPair<Self::Field>, [f64; 2])> {
        let pair = self.cc_value_derivative_pair(fixed, moving, radius)?;
        let norms = [self.l2_norm(&pair.0 .1)?, self.l2_norm(&pair.1 .1)?];
        Ok((pair, norms))
    }

    /// SyN's Gaussian smoothing of a field (`variance` in squared voxels of the field's grid,
    /// faces pinned to zero). Takes the field so that `variance <= 0` returns it unchanged
    /// without a copy.
    fn smooth_field(&self, field: Self::Field, variance: f64) -> Result<Self::Field>;

    /// Largest `|u / spacing|` over the field.
    fn max_scaled_norm(&self, field: &Self::Field) -> Result<f64>;

    /// [`Backend::max_scaled_norm`] of two fields, `(a, b)`. The default calls it twice; a
    /// backend that can return both values at once overrides it with bit-identical results.
    fn max_scaled_norm_pair(&self, a: &Self::Field, b: &Self::Field) -> Result<(f64, f64)> {
        Ok((self.max_scaled_norm(a)?, self.max_scaled_norm(b)?))
    }

    /// Euclidean norm of the whole field as one vector.
    fn l2_norm(&self, field: &Self::Field) -> Result<f64>;

    /// `field *= s`.
    fn scale(&self, field: &mut Self::Field, s: f64) -> Result<()>;

    /// `acc += a * x`.
    fn axpy(&self, acc: &mut Self::Field, a: f64, x: &Self::Field) -> Result<()>;

    /// `out(x) = warping(x) + displacement(x + warping(x))`, zero displacement outside the
    /// buffer (ITK `ComposeDisplacementFieldsImageFilter`).
    fn compose(&self, displacement: &Self::Field, warping: &Self::Field) -> Result<Self::Field>;

    /// ITK's fixed-point inverse of `field` starting from `initial`; `initial`'s buffer is
    /// updated in place and returned as the inverse.
    fn invert(
        &self,
        field: &Self::Field,
        initial: Self::Field,
        params: &InvertParams,
    ) -> Result<(Self::Field, InvertStats)>;

    /// The two inversions with which SyN re-estimates a field and its inverse:
    /// `inverse = invert(field, initial = inverse)`, then `field = invert(inverse, initial =
    /// field)`. The default calls [`Backend::invert`] twice; a backend that can return both
    /// statistics at once overrides it with bit-identical results.
    fn invert_pair(
        &self,
        field: Self::Field,
        inverse: Self::Field,
        params: &InvertParams,
    ) -> Result<InversionPair<Self::Field>> {
        let inverse = self.invert(&field, inverse, params)?;
        let field = self.invert(&inverse.0, field, params)?;
        Ok((inverse, field))
    }

    /// The field resampled onto `grid` (linear, zero outside the old buffer).
    fn resample_field(&self, field: &Self::Field, grid: &Grid) -> Result<Self::Field>;
}
