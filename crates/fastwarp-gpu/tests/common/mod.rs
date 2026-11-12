#![allow(dead_code, clippy::unwrap_used, clippy::needless_range_loop)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use fastwarp_gpu::{BackendSelect, GpuError, WgpuBackend};
use fastwarp_reg::apply::{apply_chain, Interpolation};
use fastwarp_reg::field_ops::VectorField;
use fastwarp_reg::linalg::{Vec3, IDENTITY3};
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::syn::SynResult;
use fastwarp_reg::{AffineTransform, Grid, Volume};

pub fn require_adapter() -> bool {
    fastwarp_gpu::device::env_var("FASTWARP_GPU_REQUIRE_ADAPTER")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub fn backend() -> Option<WgpuBackend> {
    match WgpuBackend::new(&BackendSelect::from_env().unwrap_or_default()) {
        Ok(b) => {
            eprintln!(
                "adapter: {} ({}), selftest attempts {}",
                b.context().info.name,
                b.context().info.backend,
                b.selftest_record().attempts
            );
            Some(b)
        }
        Err(GpuError::NoAdapter(why)) => {
            if require_adapter() {
                panic!("no GPU adapter with FASTWARP_GPU_REQUIRE_ADAPTER set: {why}");
            }
            eprintln!("skip: no adapter ({why})");
            None
        }
        Err(e) => panic!("backend creation failed: {e}"),
    }
}

pub struct Blob {
    pub centre: Vec3,
    pub sigma: f64,
    pub amplitude: f64,
}

pub fn blobs(centre: Vec3) -> Vec<Blob> {
    let off = |d: Vec3| [centre[0] + d[0], centre[1] + d[1], centre[2] + d[2]];
    vec![
        Blob {
            centre: off([0.0, 0.0, 0.0]),
            sigma: 7.0,
            amplitude: 0.7,
        },
        Blob {
            centre: off([9.0, 3.0, -2.0]),
            sigma: 3.5,
            amplitude: 0.9,
        },
        Blob {
            centre: off([-8.0, 6.0, 4.0]),
            sigma: 4.0,
            amplitude: 0.8,
        },
        Blob {
            centre: off([2.0, -9.0, 5.0]),
            sigma: 3.0,
            amplitude: 0.6,
        },
        Blob {
            centre: off([-3.0, -4.0, -9.0]),
            sigma: 3.5,
            amplitude: 0.85,
        },
    ]
}

pub fn intensity(blobs: &[Blob], p: Vec3) -> f64 {
    let mut v = 0.0;
    for b in blobs {
        let d2 = (p[0] - b.centre[0]).powi(2)
            + (p[1] - b.centre[1]).powi(2)
            + (p[2] - b.centre[2]).powi(2);
        v += b.amplitude * (-d2 / (2.0 * b.sigma * b.sigma)).exp();
    }
    (v / 1.6).min(1.0)
}

pub fn contrast2(v: f64) -> f64 {
    1.0 - v * v
}

pub fn deformation(extent: f64, centre: Vec3, amplitude: f64, p: Vec3) -> Vec3 {
    let k = 2.0 * std::f64::consts::PI / (1.3 * extent);
    [
        amplitude * (k * (p[1] - centre[1])).sin(),
        0.8 * amplitude * (k * (p[2] - centre[2]) + 0.5).sin(),
        0.6 * amplitude * (k * (p[0] - centre[0])).sin(),
    ]
}

pub struct Phantom {
    pub grid: Grid,
    pub fixed: Volume<f64>,
    pub moving: Volume<f64>,
    pub fixed2: Volume<f64>,
    pub moving2: Volume<f64>,
    pub truth: VectorField,
    pub initial: AffineTransform,
}

pub fn phantom(n: usize, spacing: f64, amplitude: f64) -> Phantom {
    let origin = [-(n as f64 - 1.0) * spacing / 2.0; 3];
    let grid = Grid::new([n, n, n], [spacing; 3], origin, IDENTITY3).unwrap();
    let extent = n as f64 * spacing;
    let centre = [0.0; 3];
    let bl = blobs(centre);
    let t0 = [1.0, -0.5, 0.5];
    let initial = AffineTransform::from_translation(t0);
    let map = |p: Vec3| {
        let s = deformation(extent, centre, amplitude, p);
        [p[0] + s[0], p[1] + s[1], p[2] + s[2]]
    };
    let fixed = Volume::from_vec(
        grid,
        (0..grid.num_voxels())
            .map(|idx| {
                let [i, j, k] = grid.ijk(idx);
                intensity(&bl, grid.index_to_point([i as f64, j as f64, k as f64]))
            })
            .collect(),
    )
    .unwrap();
    let moving = Volume::from_vec(
        grid,
        (0..grid.num_voxels())
            .map(|idx| {
                let [i, j, k] = grid.ijk(idx);
                let y = grid.index_to_point([i as f64, j as f64, k as f64]);
                intensity(&bl, map([y[0] - t0[0], y[1] - t0[1], y[2] - t0[2]]))
            })
            .collect(),
    )
    .unwrap();
    let fixed2 =
        Volume::from_vec(grid, fixed.data().iter().map(|v| contrast2(*v)).collect()).unwrap();
    let moving2 =
        Volume::from_vec(grid, moving.data().iter().map(|v| contrast2(*v)).collect()).unwrap();
    let truth = VectorField::from_fn(grid, |x| {
        let mut y = x;
        for _ in 0..60 {
            let s = deformation(extent, centre, amplitude, y);
            y = [x[0] - s[0], x[1] - s[1], x[2] - s[2]];
        }
        [y[0] - x[0], y[1] - x[1], y[2] - x[2]]
    });
    Phantom {
        grid,
        fixed,
        moving,
        fixed2,
        moving2,
        truth,
        initial,
    }
}

pub fn small_recipe(levels: &[(u32, f64, u32)], radius: usize) -> SynRecipe {
    let mut r = SynRecipe::cohort();
    r.cc_radius = radius;
    r.iterations = levels.iter().map(|l| l.2).collect();
    r.shrink_factors = levels.iter().map(|l| l.0).collect();
    r.smoothing_sigmas = levels.iter().map(|l| l.1).collect();
    r
}

pub fn ncc(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let ma = a.iter().sum::<f64>() / n;
    let mb = b.iter().sum::<f64>() / n;
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    sab / (saa * sbb).sqrt()
}

pub fn recovery(p: &Phantom, result: &SynResult) -> (f64, f64, usize) {
    let g = p.grid;
    let [nx, ny, nz] = g.size();
    let margin = 3;
    let (mut e0, mut e1, mut count) = (0.0, 0.0, 0usize);
    for idx in 0..g.num_voxels() {
        let [i, j, k] = g.ijk(idx);
        if i < margin
            || j < margin
            || k < margin
            || i + margin >= nx
            || j + margin >= ny
            || k + margin >= nz
        {
            continue;
        }
        if p.fixed.data()[idx] < 0.15 {
            continue;
        }
        let x = g.index_to_point([i as f64, j as f64, k as f64]);
        let truth = p.truth.data()[idx];
        let rec = result.pair.forward.apply(x);
        let want = p
            .initial
            .apply([x[0] + truth[0], x[1] + truth[1], x[2] + truth[2]]);
        let dev =
            ((rec[0] - want[0]).powi(2) + (rec[1] - want[1]).powi(2) + (rec[2] - want[2]).powi(2))
                .sqrt();
        let init = (truth[0].powi(2) + truth[1].powi(2) + truth[2].powi(2)).sqrt();
        e0 += init;
        e1 += dev;
        count += 1;
    }
    (e0 / count as f64, e1 / count as f64, count)
}

pub fn warped_ncc(
    p: &Phantom,
    result: &SynResult,
    moving: &Volume<f64>,
    fixed: &Volume<f64>,
) -> (f64, f64) {
    let before = {
        let chain =
            fastwarp_reg::TransformChain::from_queue(vec![fastwarp_reg::TransformElem::Affine(
                p.initial,
            )]);
        let w = apply_chain(moving, &p.grid, &chain, Interpolation::Linear, 0.0).unwrap();
        ncc(fixed.data(), w.data())
    };
    let w = apply_chain(
        moving,
        &p.grid,
        &result.pair.forward,
        Interpolation::Linear,
        0.0,
    )
    .unwrap();
    (before, ncc(fixed.data(), w.data()))
}

pub fn inverse_consistency(p: &Phantom, result: &SynResult) -> f64 {
    let g = p.grid;
    let [nx, ny, nz] = g.size();
    let margin = 3;
    let mut worst = 0.0f64;
    for idx in 0..g.num_voxels() {
        let [i, j, k] = g.ijk(idx);
        if i < margin
            || j < margin
            || k < margin
            || i + margin >= nx
            || j + margin >= ny
            || k + margin >= nz
        {
            continue;
        }
        let x = g.index_to_point([i as f64, j as f64, k as f64]);
        let y = result.pair.forward.apply(x);
        let back = result.pair.inverse.apply(y);
        let d =
            ((back[0] - x[0]).powi(2) + (back[1] - x[1]).powi(2) + (back[2] - x[2]).powi(2)).sqrt();
        worst = worst.max(d / g.spacing()[0]);
    }
    worst
}

pub fn head_intensity(p: Vec3, radii: Vec3) -> f64 {
    let r =
        ((p[0] / radii[0]).powi(2) + (p[1] / radii[1]).powi(2) + (p[2] / radii[2]).powi(2)).sqrt();
    let edge = |x: f64, w: f64| 0.5 * (1.0 + (x / w).tanh());
    let scalp = edge(1.0 - r, 0.03);
    let skull = edge(0.92 - r, 0.02) - edge(0.85 - r, 0.02);
    let brain = edge(0.85 - r, 0.02);
    let vent =
        (-(((p[0] / 8.0).powi(2)) + ((p[1] + 6.0) / 22.0).powi(2) + ((p[2] - 8.0) / 9.0).powi(2)))
            .exp();
    let thal =
        (-(((p[0] - 15.0) / 10.0).powi(2) + (p[1] / 12.0).powi(2) + (p[2] / 10.0).powi(2))).exp();
    0.3 * scalp + 0.9 * skull + brain * (0.55 - 0.35 * vent + 0.25 * thal)
}

pub struct Head {
    pub grid: Grid,
    pub fixed: Volume<f64>,
    pub moving: Volume<f64>,
}

pub fn head(size: [usize; 3], spacing: f64, amplitude: f64) -> Head {
    let origin = [
        -(size[0] as f64 - 1.0) * spacing / 2.0,
        -(size[1] as f64 - 1.0) * spacing / 2.0,
        -(size[2] as f64 - 1.0) * spacing / 2.0,
    ];
    let grid = Grid::new(size, [spacing; 3], origin, IDENTITY3).unwrap();
    let radii = [70.0, 90.0, 75.0];
    let extent = 170.0;
    let map = |q: Vec3| {
        let s = deformation(extent, [0.0; 3], amplitude, q);
        [q[0] + s[0], q[1] + s[1], q[2] + s[2]]
    };
    let make = |f: &dyn Fn(Vec3) -> f64| {
        Volume::from_vec(
            grid,
            (0..grid.num_voxels())
                .map(|idx| {
                    let [i, j, k] = grid.ijk(idx);
                    f(grid.index_to_point([i as f64, j as f64, k as f64]))
                })
                .collect(),
        )
        .unwrap()
    };
    let fixed = make(&|p| head_intensity(p, radii));
    let moving = make(&|p| head_intensity(map(p), radii));
    Head {
        grid,
        fixed,
        moving,
    }
}

pub fn digest(r: &SynResult) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in r.warp.data().iter().chain(r.inverse_warp.data()) {
        h = (h ^ u64::from(v.to_bits())).wrapping_mul(0x0100_0000_01b3);
    }
    h
}
