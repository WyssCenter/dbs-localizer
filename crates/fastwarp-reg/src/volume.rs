// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Scalar volumes on a [`Grid`].

use crate::error::{RegError, Result};
use crate::grid::Grid;

/// A scalar volume: one value per voxel of its [`Grid`], x-fastest (`linear_index`).
///
/// NIfTI load and save live in `nifti_io` (`Volume::<f32>::load_nifti` / `save_nifti`).
#[derive(Clone, Debug, PartialEq)]
pub struct Volume<T> {
    grid: Grid,
    data: Vec<T>,
}

impl<T: Copy + Default> Volume<T> {
    /// A volume of default values (zeros for numbers).
    pub fn zeros(grid: Grid) -> Self {
        let n = grid.num_voxels();
        Self {
            grid,
            data: vec![T::default(); n],
        }
    }
}

impl<T> Volume<T> {
    /// Wraps `data`; fails when its length is not the grid's voxel count.
    pub fn from_vec(grid: Grid, data: Vec<T>) -> Result<Self> {
        if data.len() != grid.num_voxels() {
            return Err(RegError::Length {
                what: "volume data",
                expected: grid.num_voxels(),
                got: data.len(),
            });
        }
        Ok(Self { grid, data })
    }

    /// The grid of the volume.
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// The voxel values, x-fastest.
    pub fn data(&self) -> &[T] {
        &self.data
    }

    /// Mutable voxel values, x-fastest.
    pub fn data_mut(&mut self) -> &mut [T] {
        &mut self.data
    }

    /// Consumes the volume into its grid and values.
    pub fn into_parts(self) -> (Grid, Vec<T>) {
        (self.grid, self.data)
    }
}

impl<T: Copy> Volume<T> {
    /// Value at voxel `(i, j, k)`; `None` outside the grid.
    pub fn get(&self, i: usize, j: usize, k: usize) -> Option<T> {
        let [nx, ny, nz] = self.grid.size();
        (i < nx && j < ny && k < nz).then(|| self.data[self.grid.linear_index(i, j, k)])
    }
}

impl Volume<f32> {
    /// The same volume with `f64` values (ANTs reads and registers images as double).
    pub fn to_f64(&self) -> Volume<f64> {
        Volume {
            grid: self.grid,
            data: self.data.iter().map(|v| f64::from(*v)).collect(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;

    #[test]
    fn construction_checks_length_and_indexing_is_x_fastest() {
        let g = Grid::new([2, 3, 4], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        assert!(Volume::from_vec(g, vec![0.0f32; 5]).is_err());
        let data: Vec<f32> = (0..24).map(|v| v as f32).collect();
        let v = Volume::from_vec(g, data).unwrap();
        assert_eq!(v.get(1, 0, 0), Some(1.0));
        assert_eq!(v.get(0, 1, 0), Some(2.0));
        assert_eq!(v.get(0, 0, 1), Some(6.0));
        assert_eq!(v.get(2, 0, 0), None);
        let z = Volume::<f32>::zeros(g);
        assert_eq!(z.data().len(), 24);
    }
}
