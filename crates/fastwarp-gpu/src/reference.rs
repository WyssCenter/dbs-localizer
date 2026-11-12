// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! CPU references (f64 accumulate) and deterministic phantoms for the probe kernels.

use std::f64::consts::TAU;

/// Voxel grid, x fastest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub nx: u32,
    pub ny: u32,
    pub nz: u32,
}

impl Grid {
    /// Number of voxels.
    pub fn n(&self) -> usize {
        self.nx as usize * self.ny as usize * self.nz as usize
    }

    /// Linear index of `(x, y, z)`.
    pub fn idx(&self, x: usize, y: usize, z: usize) -> usize {
        x + self.nx as usize * (y + self.ny as usize * z)
    }
}

/// Small deterministic generator (splitmix64).
pub struct Lcg(u64);

impl Lcg {
    /// Seeds the generator.
    pub fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform f64 in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform f32 in [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        self.next_f64() as f32
    }
}

/// Sum of five low-frequency sinusoids, in [-amp, amp], evaluated with per-axis tables.
pub fn smooth_field(g: Grid, seed: u64, amp: f64) -> Vec<f32> {
    let (nx, ny, nz) = (g.nx as usize, g.ny as usize, g.nz as usize);
    let mut out = vec![0.0f64; g.n()];
    let mut rng = Lcg::new(seed);
    let comps = 5;
    for _ in 0..comps {
        let fx = 0.5 + 2.0 * rng.next_f64();
        let fy = 0.5 + 2.0 * rng.next_f64();
        let fz = 0.5 + 2.0 * rng.next_f64();
        let phi = TAU * rng.next_f64();
        let w = amp / comps as f64;
        let tab = |n: usize, f: f64, p: f64| -> Vec<(f64, f64)> {
            (0..n)
                .map(|i| {
                    let th = TAU * f * i as f64 / n as f64 + p;
                    (th.sin(), th.cos())
                })
                .collect()
        };
        let tx = tab(nx, fx, phi);
        let ty = tab(ny, fy, 0.0);
        let tz = tab(nz, fz, 0.0);
        let mut sxy = vec![0.0f64; nx * ny];
        let mut cxy = vec![0.0f64; nx * ny];
        for y in 0..ny {
            for x in 0..nx {
                let (sx, cx) = tx[x];
                let (sy, cy) = ty[y];
                sxy[x + nx * y] = sx * cy + cx * sy;
                cxy[x + nx * y] = cx * cy - sx * sy;
            }
        }
        for (z, &(sz, cz)) in tz.iter().enumerate() {
            let base = nx * ny * z;
            for k in 0..nx * ny {
                out[base + k] += w * (sxy[k] * cz + cxy[k] * sz);
            }
        }
    }
    out.into_iter().map(|v| v as f32).collect()
}

/// Smooth image around 0.5 plus white noise, clamped to [0.02, 0.98].
pub fn phantom_image(g: Grid, seed: u64, smooth_amp: f64, noise: f32) -> Vec<f32> {
    let mut v = smooth_field(g, seed, smooth_amp);
    let mut rng = Lcg::new(seed ^ 0xABCD);
    for x in v.iter_mut() {
        *x = (0.5 + *x + noise * (rng.next_f32() - 0.5)).clamp(0.02, 0.98);
    }
    v
}

/// Validity mask: 1 inside, 0 on a rim of `rim` voxels.
pub fn rim_mask(g: Grid, rim: usize) -> Vec<u32> {
    let (nx, ny, nz) = (g.nx as usize, g.ny as usize, g.nz as usize);
    let mut m = vec![1u32; g.n()];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let edge = x < rim
                    || y < rim
                    || z < rim
                    || x + rim >= nx
                    || y + rim >= ny
                    || z + rim >= nz;
                if edge {
                    m[x + nx * (y + ny * z)] = 0;
                }
            }
        }
    }
    m
}

/// Mean of an f32 slice, rounded to f32.
pub fn mean_f32(v: &[f32]) -> f32 {
    (v.iter().map(|&x| f64::from(x)).sum::<f64>() / v.len() as f64) as f32
}

/// Error summary of a comparison.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct ErrorStats {
    pub max_abs: f64,
    pub max_rel_to_range: f64,
    pub ref_max_abs: f64,
}

impl ErrorStats {
    /// Compares `got` against `want`; relative error is scaled by the largest reference magnitude
    /// (and at least `floor`).
    pub fn compare(got: &[f32], want: &[f64], floor: f64) -> Self {
        let mut max_abs = 0.0f64;
        let mut ref_max = 0.0f64;
        for (g, w) in got.iter().zip(want) {
            max_abs = max_abs.max((f64::from(*g) - w).abs());
            ref_max = ref_max.max(w.abs());
        }
        ErrorStats {
            max_abs,
            max_rel_to_range: max_abs / ref_max.max(floor),
            ref_max_abs: ref_max,
        }
    }
}

/// CPU `y0 + iters * a * x` in f64.
pub fn axpy_reference(x: &[f32], y0: &[f32], a: f32, iters: u32) -> Vec<f64> {
    x.iter()
        .zip(y0)
        .map(|(&x, &y)| f64::from(y) + f64::from(iters) * f64::from(a) * f64::from(x))
        .collect()
}

fn box_axis(src: &[f64], dst: &mut [f64], g: Grid, axis: usize, r: usize) {
    let dims = [g.nx as usize, g.ny as usize, g.nz as usize];
    let strides = [1usize, dims[0], dims[0] * dims[1]];
    let len = dims[axis];
    let stride = strides[axis];
    for z in 0..dims[2] {
        for y in 0..dims[1] {
            for x in 0..dims[0] {
                let c = [x, y, z][axis];
                let lo = c.saturating_sub(r);
                let hi = (c + r).min(len - 1);
                let base = x + dims[0] * (y + dims[1] * z) - c * stride;
                let mut s = 0.0;
                for k in lo..=hi {
                    s += src[base + k * stride];
                }
                dst[x + dims[0] * (y + dims[1] * z)] = s;
            }
        }
    }
}

/// Reference output of the CC chain.
pub struct CcReference {
    pub ux: Vec<f64>,
    pub uy: Vec<f64>,
    pub uz: Vec<f64>,
    pub value: f64,
}

/// CPU reference of the CC chain at box radius `r`: intermediates, three separable box sums,
/// value and derivative, in f64, using the same constants and degenerate-window rule as the GPU.
pub fn cc_reference(
    g: Grid,
    img_i: &[f32],
    img_j: &[f32],
    mask: &[u32],
    shift_i: f32,
    shift_j: f32,
    r: usize,
) -> CcReference {
    let n = g.n();
    let (si, sj) = (f64::from(shift_i), f64::from(shift_j));
    let mut inter: Vec<Vec<f64>> = (0..6).map(|_| vec![0.0; n]).collect();
    for i in 0..n {
        let m = f64::from(mask[i]);
        let a = (f64::from(img_i[i]) - si) * m;
        let b = (f64::from(img_j[i]) - sj) * m;
        inter[0][i] = a;
        inter[1][i] = b;
        inter[2][i] = a * a;
        inter[3][i] = b * b;
        inter[4][i] = a * b;
        inter[5][i] = m;
    }
    let mut sums: Vec<Vec<f64>> = Vec::with_capacity(6);
    let mut t1 = vec![0.0f64; n];
    let mut t2 = vec![0.0f64; n];
    for plane in &inter {
        box_axis(plane, &mut t1, g, 0, r);
        box_axis(&t1, &mut t2, g, 1, r);
        box_axis(&t2, &mut t1, g, 2, r);
        sums.push(t1.clone());
    }
    let mut ux = vec![0.0f64; n];
    let mut uy = vec![0.0f64; n];
    let mut uz = vec![0.0f64; n];
    let mut value = 0.0f64;
    let (nx, ny, nz) = (g.nx as usize, g.ny as usize, g.nz as usize);
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let i = g.idx(x, y, z);
                if inter[5][i] <= 0.5 {
                    continue;
                }
                let cnt = sums[5][i];
                let s_i = sums[0][i];
                let s_j = sums[1][i];
                let sff = sums[2][i] - s_i * s_i / cnt;
                let smm = sums[3][i] - s_j * s_j / cnt;
                let sfm = sums[4][i] - s_i * s_j / cnt;
                let den = sff * smm;
                if den > 1e-9 {
                    value -= sfm * sfm / den;
                    let ac = inter[0][i] - s_i / cnt;
                    let bc = inter[1][i] - s_j / cnt;
                    let c = 2.0 * sfm / den * (ac - sfm / smm * bc);
                    let b = &inter[1];
                    if x > 0 && x + 1 < nx {
                        ux[i] = c * 0.5 * (b[g.idx(x + 1, y, z)] - b[g.idx(x - 1, y, z)]);
                    }
                    if y > 0 && y + 1 < ny {
                        uy[i] = c * 0.5 * (b[g.idx(x, y + 1, z)] - b[g.idx(x, y - 1, z)]);
                    }
                    if z > 0 && z + 1 < nz {
                        uz[i] = c * 0.5 * (b[g.idx(x, y, z + 1)] - b[g.idx(x, y, z - 1)]);
                    }
                } else {
                    value -= 1.0;
                }
            }
        }
    }
    CcReference { ux, uy, uz, value }
}

/// Affine as three rows `[m00 m01 m02 t]`.
pub type Affine = [[f32; 4]; 3];

/// Smooth, mildly rotated and scaled affine about the grid centre plus a shift, in voxel units.
pub fn probe_affine(g: Grid) -> Affine {
    let (ax, ay, az) = (0.03f64, -0.02f64, 0.05f64);
    let (cx, sx) = (ax.cos(), ax.sin());
    let (cy, sy) = (ay.cos(), ay.sin());
    let (cz, sz) = (az.cos(), az.sin());
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    let mul = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| {
        let mut o = [[0.0f64; 3]; 3];
        for (i, row) in o.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        o
    };
    let m = mul(rz, mul(ry, rx));
    let s = 1.02;
    let centre = [
        f64::from(g.nx - 1) / 2.0,
        f64::from(g.ny - 1) / 2.0,
        f64::from(g.nz - 1) / 2.0,
    ];
    let shift = [2.0, -1.5, 1.0];
    let mut a = [[0.0f32; 4]; 3];
    for i in 0..3 {
        let mut t = centre[i] + shift[i];
        for j in 0..3 {
            a[i][j] = (s * m[i][j]) as f32;
            t -= s * m[i][j] * centre[j];
        }
        a[i][3] = t as f32;
    }
    a
}

/// Reference of `k_warp_scalar` with unit spacing and zero origins: displacement field, then
/// affine, then trilinear sampling with clamp-at-edge and a validity flag.
pub fn warp_reference(
    g: Grid,
    src: &[f32],
    field: [&[f32]; 3],
    affine: &Affine,
) -> (Vec<f64>, Vec<u8>) {
    let n = g.n();
    let mut out = vec![0.0f64; n];
    let mut valid = vec![0u8; n];
    let (nx, ny, nz) = (g.nx as usize, g.ny as usize, g.nz as usize);
    let m: Vec<[f64; 4]> = affine
        .iter()
        .map(|r| [r[0].into(), r[1].into(), r[2].into(), r[3].into()])
        .collect();
    let hi = [(nx - 1) as f64, (ny - 1) as f64, (nz - 1) as f64];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let i = g.idx(x, y, z);
                let p = [
                    x as f64 + f64::from(field[0][i]),
                    y as f64 + f64::from(field[1][i]),
                    z as f64 + f64::from(field[2][i]),
                ];
                let mut c = [0.0f64; 3];
                for (k, ck) in c.iter_mut().enumerate() {
                    *ck = m[k][0] * p[0] + m[k][1] * p[1] + m[k][2] * p[2] + m[k][3];
                }
                if (0..3).any(|k| c[k] < 0.0 || c[k] > hi[k]) {
                    continue;
                }
                let f = [c[0].floor(), c[1].floor(), c[2].floor()];
                let t = [c[0] - f[0], c[1] - f[1], c[2] - f[2]];
                let x0 = f[0] as usize;
                let y0 = f[1] as usize;
                let z0 = f[2] as usize;
                let x1 = (x0 + 1).min(nx - 1);
                let y1 = (y0 + 1).min(ny - 1);
                let z1 = (z0 + 1).min(nz - 1);
                let s = |xx: usize, yy: usize, zz: usize| f64::from(src[g.idx(xx, yy, zz)]);
                let lerp = |a: f64, b: f64, w: f64| a + (b - a) * w;
                let c00 = lerp(s(x0, y0, z0), s(x1, y0, z0), t[0]);
                let c10 = lerp(s(x0, y1, z0), s(x1, y1, z0), t[0]);
                let c01 = lerp(s(x0, y0, z1), s(x1, y0, z1), t[0]);
                let c11 = lerp(s(x0, y1, z1), s(x1, y1, z1), t[0]);
                let c0 = lerp(c00, c10, t[1]);
                let c1 = lerp(c01, c11, t[1]);
                out[i] = lerp(c0, c1, t[2]);
                valid[i] = 1;
            }
        }
    }
    (out, valid)
}

/// FNV-1a over the bit patterns of an f32 slice.
pub fn fnv_f32(v: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for x in v {
        for b in x.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// Error of a three-component field; relative error is scaled by the largest reference magnitude
/// over all components.
pub fn vector_error(got: [&[f32]; 3], want: [&[f64]; 3]) -> ErrorStats {
    let mut max_abs = 0.0f64;
    let mut ref_max = 0.0f64;
    for k in 0..3 {
        let e = ErrorStats::compare(got[k], want[k], 1e-12);
        max_abs = max_abs.max(e.max_abs);
        ref_max = ref_max.max(e.ref_max_abs);
    }
    ErrorStats {
        max_abs,
        max_rel_to_range: max_abs / ref_max.max(1e-12),
        ref_max_abs: ref_max,
    }
}
