// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: AGPL-3.0-only

use std::ops::Range;

use ndarray::{Array3, Axis};

use super::volume::CtVolume;

const HEAD_TISSUE_HU: Range<f32> = -300.0..1800.0;
const MIDLINE_STRIDE: usize = 3;

pub(super) fn out_of_fov_mask(data: &Array3<f32>) -> Array3<bool> {
    let mut mask = Array3::from_elem(data.dim(), false);
    for (row, mut out) in data.lanes(Axis(2)).into_iter().zip(mask.lanes_mut(Axis(2))) {
        let n = row.len();
        let lead = row.iter().take_while(|&&v| v == 0.0).count();
        for o in out.iter_mut().take(lead) {
            *o = true;
        }
        if lead == n {
            continue;
        }
        let trail = row.iter().rev().take_while(|&&v| v == 0.0).count();
        for o in out.iter_mut().skip(n - trail) {
            *o = true;
        }
    }
    mask
}

impl CtVolume {
    pub fn head_midline_x(&self) -> Option<f64> {
        let outside = out_of_fov_mask(&self.data);
        let mut sum = 0.0f64;
        let mut n = 0u64;
        for k in (0..self.nk).step_by(MIDLINE_STRIDE) {
            for j in (0..self.nj).step_by(MIDLINE_STRIDE) {
                for i in (0..self.ni).step_by(MIDLINE_STRIDE) {
                    if outside[(k, j, i)] || !HEAD_TISSUE_HU.contains(&self.data[(k, j, i)]) {
                        continue;
                    }
                    sum += self.index_to_world(i as f64, j as f64, k as f64)[0];
                    n += 1;
                }
            }
        }
        (n > 0).then(|| sum / n as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AIR_HU: f32 = -1000.0;
    const TISSUE_HU: f32 = 40.0;
    const SHAPE: (usize, usize, usize) = (60, 70, 90);

    fn head_phantom() -> Array3<f32> {
        let (nk, nj, ni) = SHAPE;
        let c = [44.5, 34.5, 29.5];
        Array3::from_shape_fn((nk, nj, ni), |(k, j, i)| {
            let d = [
                (i as f64 - c[0]) / 30.0,
                (j as f64 - c[1]) / 25.0,
                (k as f64 - c[2]) / 20.0,
            ];
            if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= 1.0 {
                TISSUE_HU
            } else {
                AIR_HU
            }
        })
    }

    fn crop_fov(data: &Array3<f32>) -> Array3<f32> {
        let mut out = data.clone();
        for ((k, _j, i), v) in out.indexed_iter_mut() {
            let slab = i < 10;
            let wedge = i as f64 + 0.6 * k as f64 > 112.0;
            if slab || wedge {
                assert_eq!(*v, AIR_HU, "the crop must not cut the head at ({i}, {k})");
                *v = 0.0;
            }
        }
        out
    }

    fn affine() -> fastwarp_core::Mat4 {
        [
            [-0.9, 0.0, 0.0, 41.0],
            [0.0, 0.9, 0.0, -30.0],
            [0.0, 0.0, 1.1, -20.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn naive_midline_x(vol: &CtVolume) -> f64 {
        let mut sum = 0.0;
        let mut n = 0.0;
        for k in (0..vol.nk).step_by(MIDLINE_STRIDE) {
            for j in (0..vol.nj).step_by(MIDLINE_STRIDE) {
                for i in (0..vol.ni).step_by(MIDLINE_STRIDE) {
                    if HEAD_TISSUE_HU.contains(&vol.data[(k, j, i)]) {
                        sum += vol.index_to_world(i as f64, j as f64, k as f64)[0];
                        n += 1.0;
                    }
                }
            }
        }
        sum / n
    }

    #[test]
    fn asymmetric_field_of_view_does_not_move_the_midline() {
        let full = CtVolume::from_array(head_phantom(), affine());
        let cropped = CtVolume::from_array(crop_fov(&full.data), affine());
        let want = full.head_midline_x().expect("tissue");
        let got = cropped.head_midline_x().expect("tissue");
        assert!(
            (got - want).abs() < 1e-9,
            "midline {got} vs uncropped {want}"
        );
        let biased = naive_midline_x(&cropped);
        assert!(
            (biased - want).abs() > 5.0,
            "the zero fill must bias a midline that counts it ({biased} vs {want})"
        );
    }

    #[test]
    fn zero_runs_touching_a_row_end_are_outside_the_field_of_view() {
        let mut data = Array3::from_elem((1, 2, 8), TISSUE_HU);
        for i in [0, 1, 4, 7] {
            data[(0, 0, i)] = 0.0;
        }
        for i in 0..8 {
            data[(0, 1, i)] = 0.0;
        }
        let mask = out_of_fov_mask(&data);
        let row0: Vec<bool> = (0..8).map(|i| mask[(0, 0, i)]).collect();
        assert_eq!(
            row0,
            vec![true, true, false, false, false, false, false, true]
        );
        assert!((0..8).all(|i| mask[(0, 1, i)]));
    }

    #[test]
    fn interior_zero_tissue_still_counts() {
        let mut data = Array3::from_elem((3, 3, 9), AIR_HU);
        for i in 3..9 {
            data[(0, 0, i)] = 0.0;
        }
        data[(0, 0, 8)] = AIR_HU;
        let vol = CtVolume::from_array(data, fastwarp_core::identity_mat4());
        assert_eq!(vol.head_midline_x(), Some(4.5));
    }

    #[test]
    fn no_tissue_inside_the_field_of_view_has_no_midline() {
        let vol = CtVolume::from_array(
            Array3::<f32>::zeros((4, 4, 4)),
            fastwarp_core::identity_mat4(),
        );
        assert_eq!(vol.head_midline_x(), None);
    }
}
