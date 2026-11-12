// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
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
// Portions derived from ITK `itkMersenneTwisterRandomVariateGenerator.h` (seeding, `reload`,
// `twist`, tempering, `GetVariateWithOpenRange`, `GetVariateWithOpenUpperRange`,
// `GetNormalVariate`; that file is itself derived from the Mersenne Twister code of Makoto
// Matsumoto, Takuji Nishimura, Shawn Cokus and Richard J. Wagner, BSD-3-Clause:
// Copyright (C) 1997 - 2002, Makoto Matsumoto and Takuji Nishimura; Copyright (C) 2000 - 2003,
// Richard J. Wagner; the BSD-3-Clause conditions and disclaimer are in the header of the
// upstream file), from ITK `itkImageRegistrationMethodv4.hxx` (`SetMetricSamplePoints`: REGULAR
// and RANDOM strategies, jitter by one third of the virtual spacing, per-metric seed sequence)
// and from `itkImageRandomConstIteratorWithIndex.hxx` (`RandomJump`). See
// `docs/THIRD_PARTY_PORTS.md`.

//! ITK's Mersenne Twister and the metric sample-point selection of `ImageRegistrationMethodv4`.

use crate::grid::Grid;
use crate::linalg::Vec3;

const STATE_LEN: usize = 624;
const PERIOD: usize = 397;

/// ITK's `MersenneTwisterRandomVariateGenerator` (MT19937), seeded with a 32-bit integer.
#[derive(Clone, Debug)]
pub struct Mt19937 {
    state: [u32; STATE_LEN],
    next: usize,
    left: usize,
}

fn twist(m: u32, s0: u32, s1: u32) -> u32 {
    let mix = (s0 & 0x8000_0000) | (s1 & 0x7fff_ffff);
    let mag = if s1 & 1 == 1 { 0x9908_b0df } else { 0 };
    m ^ (mix >> 1) ^ mag
}

impl Mt19937 {
    /// Seeds the generator like ITK's `SetSeed(seed)` / `Initialize(seed)`.
    pub fn new(seed: u32) -> Self {
        let mut state = [0u32; STATE_LEN];
        state[0] = seed;
        for i in 1..STATE_LEN {
            let prev = state[i - 1];
            state[i] = 1_812_433_253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        let mut mt = Self {
            state,
            next: 0,
            left: 0,
        };
        mt.reload();
        mt
    }

    fn reload(&mut self) {
        let s = &mut self.state;
        for p in 0..STATE_LEN - PERIOD {
            s[p] = twist(s[p + PERIOD], s[p], s[p + 1]);
        }
        for p in STATE_LEN - PERIOD..STATE_LEN - 1 {
            s[p] = twist(s[p + PERIOD - STATE_LEN], s[p], s[p + 1]);
        }
        let last = STATE_LEN - 1;
        s[last] = twist(s[last + PERIOD - STATE_LEN], s[last], s[0]);
        self.left = STATE_LEN;
        self.next = 0;
    }

    /// The next 32-bit output (`GetIntegerVariate`).
    pub fn integer(&mut self) -> u32 {
        if self.left == 0 {
            self.reload();
        }
        self.left -= 1;
        let mut s1 = self.state[self.next];
        self.next += 1;
        s1 ^= s1 >> 11;
        s1 ^= (s1 << 7) & 0x9d2c_5680;
        s1 ^= (s1 << 15) & 0xefc6_0000;
        s1 ^ (s1 >> 18)
    }

    /// A variate in `[0, 1)` (`GetVariateWithOpenUpperRange`).
    pub fn open_upper(&mut self) -> f64 {
        f64::from(self.integer()) * (1.0 / 4_294_967_296.0)
    }

    /// A variate in `(0, 1)` (`GetVariateWithOpenRange`).
    pub fn open(&mut self) -> f64 {
        (f64::from(self.integer()) + 0.5) * (1.0 / 4_294_967_296.0)
    }

    /// A normal variate by ITK's Box-Muller form (`GetNormalVariate(mean, variance)`): one
    /// open-range then one open-upper-range draw per call, the cosine branch only.
    pub fn normal(&mut self, mean: f64, variance: f64) -> f64 {
        let r = (-2.0 * (1.0 - self.open()).ln() * variance).sqrt();
        let phi = 2.0 * std::f64::consts::PI * self.open_upper();
        mean + r * phi.cos()
    }
}

/// `antsRegistration` metric sampling strategies (`MI[...,None|Regular|Random,percentage]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SamplingStrategy {
    /// Every `ceil(1 / percentage)`-th voxel in raster order, jittered.
    Regular,
}

/// A strategy with its sampling percentage as `antsRegistration` stores it (a `float`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplingSpec {
    /// The strategy.
    pub strategy: SamplingStrategy,
    /// Fraction of the voxels in `(0, 1]`.
    pub percentage: f32,
}

/// The seed bookkeeping of `ImageRegistrationMethodv4`: `m_CurrentRandomSeed` starts at the
/// `--random-seed` value for each registration stage and is incremented once per generator
/// created, never reset between levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedSequence {
    current: u32,
}

impl SeedSequence {
    /// A sequence starting at `seed` (the value of `--random-seed`).
    pub fn new(seed: u32) -> Self {
        Self { current: seed }
    }

    /// The seed for the next generator (post-increment).
    pub fn next_seed(&mut self) -> u32 {
        let s = self.current;
        self.current = self.current.wrapping_add(1);
        s
    }
}

fn jittered_point(grid: &Grid, index: [usize; 3], rng: &mut Mt19937) -> Vec3 {
    let spacing = grid.spacing();
    let mut p = grid.index_to_point([index[0] as f64, index[1] as f64, index[2] as f64]);
    for d in 0..3 {
        p[d] += rng.normal(0.0, 1.0) * (spacing[d] / 3.0);
    }
    p
}

/// `ImageRegistrationMethodv4::SetMetricSamplePoints` for one metric on the virtual domain
/// `grid` of the current level: the physical sample points. `mask` keeps a point only when it
/// returns `true` (the fixed image mask; applied after the jitter, as ITK does).
///
/// Regular sampling takes voxel 0 and then every `ceil(1 / percentage)`-th voxel in raster order
/// (x fastest). Each point is perturbed by `N(0, 1) * spacing / 3` per axis.
pub fn metric_sample_points(
    grid: &Grid,
    spec: SamplingSpec,
    seeds: &mut SeedSequence,
    mask: Option<&dyn Fn(Vec3) -> bool>,
) -> Vec<Vec3> {
    let mut out = Vec::new();
    let keep = |p: Vec3| mask.is_none_or(|m| m(p));
    match spec.strategy {
        SamplingStrategy::Regular => {
            let mut rng = Mt19937::new(seeds.next_seed());
            let stride = (1.0 / f64::from(spec.percentage)).ceil().max(1.0) as usize;
            let [nx, ny, nz] = grid.size();
            let mut count = stride;
            for k in 0..nz {
                for j in 0..ny {
                    for i in 0..nx {
                        if count == stride {
                            count = 0;
                            let p = jittered_point(grid, [i, j, k], &mut rng);
                            if keep(p) {
                                out.push(p);
                            }
                        }
                        count += 1;
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::linalg::IDENTITY3;

    #[test]
    fn mt19937_matches_the_published_reference_outputs() {
        let mut mt = Mt19937::new(5489);
        let first: Vec<u32> = (0..10).map(|_| mt.integer()).collect();
        assert_eq!(
            first,
            [
                3_499_211_612,
                581_869_302,
                3_890_346_734,
                3_586_334_585,
                545_404_204,
                4_161_255_391,
                3_922_919_429,
                949_333_985,
                2_715_962_298,
                1_323_567_403
            ]
        );
        let mut mt = Mt19937::new(5489);
        let mut last = 0;
        for _ in 0..10_000 {
            last = mt.integer();
        }
        assert_eq!(last, 4_123_659_995);
    }

    #[test]
    fn variate_ranges_and_normal_statistics() {
        let mut mt = Mt19937::new(1);
        for _ in 0..1000 {
            let a = mt.open();
            let b = mt.open_upper();
            assert!(a > 0.0 && a < 1.0 && (0.0..1.0).contains(&b));
        }
        let n = 20_000;
        let mut sum = 0.0;
        let mut sum2 = 0.0;
        for _ in 0..n {
            let v = mt.normal(0.0, 1.0);
            sum += v;
            sum2 += v * v;
        }
        let mean = sum / n as f64;
        let var = sum2 / n as f64 - mean * mean;
        assert!(mean.abs() < 0.03);
        assert!((var - 1.0).abs() < 0.05);
    }

    #[test]
    fn normal_variate_follows_the_itk_box_muller_formula() {
        let mut a = Mt19937::new(77);
        let mut b = Mt19937::new(77);
        let u1 = (f64::from(b.integer()) + 0.5) / 4_294_967_296.0;
        let u2 = f64::from(b.integer()) / 4_294_967_296.0;
        let want = (-2.0 * (1.0 - u1).ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
        assert_eq!(a.normal(0.0, 1.0), want);
    }

    #[test]
    fn regular_sampling_takes_every_fourth_voxel_and_the_seed_advances_once_per_call() {
        let g = Grid::new([8, 4, 2], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let spec = SamplingSpec {
            strategy: SamplingStrategy::Regular,
            percentage: 0.25,
        };
        let mut seeds = SeedSequence::new(20_260_910);
        let a = metric_sample_points(&g, spec, &mut seeds, None);
        assert_eq!(a.len(), 16);
        let b = metric_sample_points(&g, spec, &mut seeds, None);
        assert_ne!(a, b);
        let c = metric_sample_points(&g, spec, &mut SeedSequence::new(20_260_910), None);
        assert_eq!(a, c);

        let mut rng = Mt19937::new(20_260_910);
        let first = [
            rng.normal(0.0, 1.0) / 3.0,
            rng.normal(0.0, 1.0) / 3.0,
            rng.normal(0.0, 1.0) / 3.0,
        ];
        for d in 0..3 {
            assert!((a[0][d] - first[d]).abs() < 1e-15);
        }
        let second = [
            4.0 + rng.normal(0.0, 1.0) / 3.0,
            rng.normal(0.0, 1.0) / 3.0,
            rng.normal(0.0, 1.0) / 3.0,
        ];
        for d in 0..3 {
            assert!((a[1][d] - second[d]).abs() < 1e-15);
        }
    }

    #[test]
    fn mask_filters_after_the_jitter() {
        let g = Grid::new([8, 4, 2], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let spec = SamplingSpec {
            strategy: SamplingStrategy::Regular,
            percentage: 0.5,
        };
        let all = metric_sample_points(&g, spec, &mut SeedSequence::new(3), None);
        let keep_left = |p: Vec3| p[0] < 3.5;
        let some = metric_sample_points(&g, spec, &mut SeedSequence::new(3), Some(&keep_left));
        assert!(some.len() < all.len() && !some.is_empty());
        assert!(some.iter().all(|p| p[0] < 3.5));
        let kept: Vec<Vec3> = all.iter().copied().filter(|p| p[0] < 3.5).collect();
        assert_eq!(some, kept);
    }

    fn mask_of(data: Vec<f64>, size: [usize; 3], origin: Vec3) -> crate::mask::MaskVolume {
        let g = Grid::new(size, [1.0; 3], origin, IDENTITY3).unwrap();
        crate::mask::MaskVolume::from_volume(crate::volume::Volume::from_vec(g, data).unwrap())
    }

    #[test]
    fn a_mask_of_ones_keeps_every_sample_bit_for_bit() {
        let g = Grid::new([8, 4, 2], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let mask = mask_of(vec![1.0; 10 * 6 * 4], [10, 6, 4], [-1.0; 3]);
        let spec = SamplingSpec {
            strategy: SamplingStrategy::Regular,
            percentage: 0.25,
        };
        let plain = metric_sample_points(&g, spec, &mut SeedSequence::new(7), None);
        let keep = |p: Vec3| mask.is_inside(p);
        let masked = metric_sample_points(
            &g,
            SamplingSpec {
                strategy: SamplingStrategy::Regular,
                percentage: 0.25,
            },
            &mut SeedSequence::new(7),
            Some(&keep),
        );
        assert_eq!(plain, masked);
    }

    #[test]
    fn a_half_mask_keeps_about_half_the_regular_samples() {
        let g = Grid::new([8, 4, 2], [1.0; 3], [0.0; 3], IDENTITY3).unwrap();
        let data: Vec<f64> = (0..g.num_voxels())
            .map(|lin| f64::from(u8::from(g.ijk(lin)[0] < 4)))
            .collect();
        let mask = mask_of(data, [8, 4, 2], [0.0; 3]);
        let spec = || SamplingSpec {
            strategy: SamplingStrategy::Regular,
            percentage: 0.25,
        };
        let plain = metric_sample_points(&g, spec(), &mut SeedSequence::new(7), None);
        let keep = |p: Vec3| mask.is_inside(p);
        let masked = metric_sample_points(&g, spec(), &mut SeedSequence::new(7), Some(&keep));
        assert_eq!(plain.len(), 16);
        assert_eq!(
            masked,
            plain
                .iter()
                .copied()
                .filter(|p| mask.is_inside(*p))
                .collect::<Vec<_>>()
        );
        assert!((7..=9).contains(&masked.len()), "{}", masked.len());
    }

    #[test]
    fn the_mask_inside_test_is_evaluated_in_physical_space() {
        let (s, c) = 0.4f64.sin_cos();
        let dir = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
        let mg = Grid::new([6, 6, 6], [1.0; 3], [-3.0, -3.0, -3.0], dir).unwrap();
        let data: Vec<f64> = (0..mg.num_voxels())
            .map(|lin| f64::from(u8::from(mg.ijk(lin)[0] >= 3)))
            .collect();
        let mask = crate::mask::MaskVolume::new(mg, data).unwrap();
        let g = Grid::new([8, 8, 8], [1.0; 3], [-4.0, -4.0, -4.0], IDENTITY3).unwrap();
        let spec = || SamplingSpec {
            strategy: SamplingStrategy::Regular,
            percentage: 0.25,
        };
        let plain = metric_sample_points(&g, spec(), &mut SeedSequence::new(3), None);
        let keep = |p: Vec3| mask.is_inside(p);
        let masked = metric_sample_points(&g, spec(), &mut SeedSequence::new(3), Some(&keep));
        let expected: Vec<Vec3> = plain
            .iter()
            .copied()
            .filter(|p| mask.is_inside(*p))
            .collect();
        assert_eq!(masked, expected);
        assert!(!masked.is_empty() && masked.len() < plain.len());
    }
}
