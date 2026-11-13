// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: AGPL-3.0-only

//! Brain mask from CT — `specs/10_pacer.md` §4.

use std::cmp::Ordering;

use ndarray::Array3;
use rayon::prelude::*;

use super::errors::PacerResult;

type P3 = [f64; 3];

fn sub3(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross3(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot3(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm3sq(a: P3) -> f64 {
    dot3(a, a)
}

/// Triangulated 3-D convex hull (gift-wrapping / incremental expansion).
struct Hull3D {
    pts: Vec<P3>,
    faces: Vec<[usize; 3]>,
}

impl Hull3D {
    fn build(pts: &[P3]) -> Option<Self> {
        let n = pts.len();
        if n < 4 {
            return None;
        }

        let p0 = 0usize;
        let p1 = (1..n).find(|&i| norm3sq(sub3(pts[i], pts[p0])) > 1e-20)?;
        let d01 = sub3(pts[p1], pts[p0]);
        let p2 = (0..n)
            .filter(|&i| i != p0 && i != p1)
            .find(|&i| norm3sq(cross3(d01, sub3(pts[i], pts[p0]))) > 1e-20)?;
        let n012 = cross3(sub3(pts[p1], pts[p0]), sub3(pts[p2], pts[p0]));
        let p3 = (0..n)
            .filter(|&i| i != p0 && i != p1 && i != p2)
            .find(|&i| dot3(n012, sub3(pts[i], pts[p0])).abs() > 1e-10)?;

        let cref: P3 = {
            let mut c = [0.0f64; 3];
            for &idx in &[p0, p1, p2, p3] {
                for d in 0..3 {
                    c[d] += pts[idx][d];
                }
            }
            for v in &mut c {
                *v /= 4.0;
            }
            c
        };

        let tet = [p0, p1, p2, p3];
        let mut faces: Vec<[usize; 3]> =
            vec![[p0, p1, p2], [p0, p2, p3], [p0, p3, p1], [p1, p3, p2]];
        for f in &mut faces {
            let nn = cross3(sub3(pts[f[1]], pts[f[0]]), sub3(pts[f[2]], pts[f[0]]));
            if dot3(nn, sub3(cref, pts[f[0]])) > 0.0 {
                f.swap(1, 2);
            }
        }

        let mut in_hull: std::collections::HashSet<usize> = tet.iter().copied().collect();

        for i in 0..n {
            if in_hull.contains(&i) {
                continue;
            }
            let visible: Vec<usize> = faces
                .iter()
                .enumerate()
                .filter_map(|(fi, &f)| {
                    let nn = cross3(sub3(pts[f[1]], pts[f[0]]), sub3(pts[f[2]], pts[f[0]]));
                    if dot3(nn, sub3(pts[i], pts[f[0]])) > 1e-10 {
                        Some(fi)
                    } else {
                        None
                    }
                })
                .collect();
            if visible.is_empty() {
                continue;
            }
            in_hull.insert(i);

            let mut edge_count: std::collections::HashMap<(usize, usize), usize> =
                std::collections::HashMap::new();
            for &fi in &visible {
                let f = faces[fi];
                for &(a, b) in &[(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                    *edge_count.entry((a.min(b), a.max(b))).or_insert(0) += 1;
                }
            }
            let horizon: Vec<(usize, usize)> = edge_count
                .into_iter()
                .filter_map(|(e, c)| if c == 1 { Some(e) } else { None })
                .collect();

            let mut to_remove = visible;
            to_remove.sort_unstable_by(|a, b| b.cmp(a));
            for fi in to_remove {
                faces.swap_remove(fi);
            }

            for (a, b) in horizon {
                let mut f = [a, b, i];
                let nn = cross3(sub3(pts[f[1]], pts[f[0]]), sub3(pts[f[2]], pts[f[0]]));
                if dot3(nn, sub3(cref, pts[f[0]])) > 0.0 {
                    f.swap(1, 2);
                }
                faces.push(f);
            }
        }

        Some(Hull3D {
            pts: pts.to_vec(),
            faces,
        })
    }
}

fn max_v3(a: f64, b: f64, c: f64) -> f64 {
    a.max(b).max(c)
}

pub(super) fn par_fill<T, F>(shape: (usize, usize, usize), init: T, f: F) -> Array3<T>
where
    T: Copy + Send + Sync,
    F: Fn(usize, &mut [T]) + Sync,
{
    let (nk, nj, ni) = shape;
    let plane = nj * ni;
    let mut v = vec![init; nk * plane];
    if plane > 0 {
        v.par_chunks_mut(plane)
            .enumerate()
            .for_each(|(k, s)| f(k, s));
    }
    Array3::from_shape_vec(shape, v).unwrap_or_else(|_| Array3::from_elem(shape, init))
}

fn ball_offsets(radius_vox: f64) -> Vec<(isize, isize, isize)> {
    let r = radius_vox.ceil() as i32;
    let mut o = vec![];
    for di in -r..=r {
        for dj in -r..=r {
            for dk in -r..=r {
                let d = ((di * di + dj * dj + dk * dk) as f64).sqrt();
                if d <= radius_vox + 1e-5 {
                    o.push((di as isize, dj as isize, dk as isize));
                }
            }
        }
    }
    o
}

fn medfilt3(x: &Array3<bool>) -> Array3<bool> {
    let (nk, nj, ni) = x.dim();
    par_fill((nk, nj, ni), false, |k, out| {
        for j in 0..nj {
            for i in 0..ni {
                let mut c = 0usize;
                for dk in 0i32..=2 {
                    for dj in 0i32..=2 {
                        for di in 0i32..=2 {
                            let ii = (i as i32 + di - 1).clamp(0, ni as i32 - 1) as usize;
                            let jj = (j as i32 + dj - 1).clamp(0, nj as i32 - 1) as usize;
                            let kk = (k as i32 + dk - 1).clamp(0, nk as i32 - 1) as usize;
                            if x[(kk, jj, ii)] {
                                c += 1;
                            }
                        }
                    }
                }
                out[j * ni + i] = c > 13;
            }
        }
    })
}

fn true_pixels_per_slice(x: &Array3<bool>) -> Vec<Vec<(usize, usize)>> {
    let (nk, nj, ni) = x.dim();
    (0..nk)
        .into_par_iter()
        .map(|k| {
            let mut v = Vec::new();
            for j in 0..nj {
                for i in 0..ni {
                    if x[(k, j, i)] {
                        v.push((j, i));
                    }
                }
            }
            v
        })
        .collect()
}

fn dilate3(x: &Array3<bool>, off: &[(isize, isize, isize)]) -> Array3<bool> {
    let (nk, nj, ni) = x.dim();
    let src = true_pixels_per_slice(x);
    par_fill((nk, nj, ni), false, |k, out| {
        for &(di, dj, dk) in off {
            let kk = k as isize - dk;
            if kk < 0 || kk as usize >= nk {
                continue;
            }
            for &(j, i) in &src[kk as usize] {
                let ii = i as isize + di;
                let jj = j as isize + dj;
                if ii < 0 || jj < 0 || ii as usize >= ni || jj as usize >= nj {
                    continue;
                }
                out[jj as usize * ni + ii as usize] = true;
            }
        }
    })
}

fn erode3(x: &Array3<bool>, off: &[(isize, isize, isize)]) -> Array3<bool> {
    let (nk, nj, ni) = x.dim();
    par_fill((nk, nj, ni), false, |k, out| {
        for j in 0..nj {
            for i in 0..ni {
                if !x[(k, j, i)] {
                    continue;
                }
                let mut keep = true;
                for (di, dj, dk) in off {
                    let ii = i as isize + di;
                    let jj = j as isize + dj;
                    let kk = k as isize + dk;
                    if ii < 0 || jj < 0 || kk < 0 {
                        keep = false;
                        break;
                    }
                    let (ii, jj, kk) = (ii as usize, jj as usize, kk as usize);
                    if ii >= ni || jj >= nj || kk >= nk || !x[(kk, jj, ii)] {
                        keep = false;
                        break;
                    }
                }
                out[j * ni + i] = keep;
            }
        }
    })
}

fn morph_open(x: &Array3<bool>, r_vox: f64) -> Array3<bool> {
    let off = ball_offsets(r_vox);
    let e = erode3(x, &off);
    dilate3(&e, &off)
}

fn morph_close(x: &Array3<bool>, r_vox: f64) -> Array3<bool> {
    let off = ball_offsets(r_vox);
    let d = dilate3(x, &off);
    erode3(&d, &off)
}

const MIN_B: f32 = 15.0;
const MAX_B: f32 = 60.0;

/// 26-neighbour offsets except origin.
fn neigh26() -> impl Iterator<Item = (isize, isize, isize)> {
    (-1isize..=1)
        .flat_map(|di| (-1isize..=1).flat_map(move |dj| (-1isize..=1).map(move |dk| (di, dj, dk))))
}

/// Largest 26-connected component as mask.
fn largest_3d_cc(m: &Array3<bool>) -> Array3<bool> {
    let (nk, nj, ni) = (m.shape()[0], m.shape()[1], m.shape()[2]);
    let mut lab = Array3::from_elem((nk, nj, ni), 0i32);
    let mut id = 0i32;
    for k in 0..nk {
        for j in 0..nj {
            for i in 0..ni {
                if !m[(k, j, i)] || lab[(k, j, i)] > 0 {
                    continue;
                }
                id += 1;
                let mut st = vec![(k, j, i)];
                lab[(k, j, i)] = id;
                while let Some((ck, cj, ci)) = st.pop() {
                    for (di, dj, dk) in neigh26() {
                        if (di | dj | dk) == 0 {
                            continue;
                        }
                        let ii = ci as isize + di;
                        let jj = cj as isize + dj;
                        let kk = ck as isize + dk;
                        if ii < 0
                            || jj < 0
                            || kk < 0
                            || ii as usize >= ni
                            || jj as usize >= nj
                            || kk as usize >= nk
                        {
                            continue;
                        }
                        let (ii, jj, kk) = (ii as usize, jj as usize, kk as usize);
                        if m[(kk, jj, ii)] && lab[(kk, jj, ii)] == 0 {
                            lab[(kk, jj, ii)] = id;
                            st.push((kk, jj, ii));
                        }
                    }
                }
            }
        }
    }
    if id == 0 {
        return Array3::from_elem((nk, nj, ni), false);
    }
    let mut counts = vec![0usize; (id as usize) + 1];
    for v in lab.iter() {
        if *v > 0 {
            counts[*v as usize] += 1;
        }
    }
    let Some(best) = (1..=id as usize).max_by_key(|&i| counts[i]) else {
        return Array3::from_elem((nk, nj, ni), false);
    };
    let best = best as i32;
    par_fill((nk, nj, ni), false, |k, out| {
        for j in 0..nj {
            for i in 0..ni {
                out[j * ni + i] = lab[(k, j, i)] == best;
            }
        }
    })
}

struct HullPlane {
    n: P3,
    a: P3,
}

const HULL_TOL_MM: f64 = 1e-4;

fn hull_planes(hull: &Hull3D, cref: P3) -> Vec<HullPlane> {
    let vs = &hull.pts;
    let mut out = Vec::with_capacity(hull.faces.len());
    for &[v0, v1, v2] in &hull.faces {
        let a = vs[v0];
        let b = vs[v1];
        let c = vs[v2];
        let mut nn = cross3(sub3(b, a), sub3(c, a));
        let nlen = norm3sq(nn).sqrt();
        if nlen < 1e-12 {
            continue;
        }
        nn[0] /= nlen;
        nn[1] /= nlen;
        nn[2] /= nlen;
        if dot3(nn, sub3(cref, a)) > 0.0 {
            nn[0] = -nn[0];
            nn[1] = -nn[1];
            nn[2] = -nn[2];
        }
        out.push(HullPlane { n: nn, a });
    }
    out
}

fn inside_planes(p: P3, planes: &[HullPlane]) -> bool {
    planes
        .iter()
        .all(|pl| dot3(pl.n, sub3(p, pl.a)).partial_cmp(&HULL_TOL_MM) != Some(Ordering::Greater))
}

fn hull_row(
    planes: &[HullPlane],
    affine: &fastwarp_core::Mat4,
    j: usize,
    k: usize,
    i0: usize,
    i1: usize,
) -> Option<(usize, usize)> {
    const SLOP_MM: f64 = 1e-6;
    let base = fastwarp_core::apply_rowmajor(affine, [0.0, j as f64, k as f64]);
    let col = [affine[0][0], affine[1][0], affine[2][0]];
    let mut lo = f64::NEG_INFINITY;
    let mut hi = f64::INFINITY;
    for pl in planes {
        let f0 = dot3(pl.n, sub3(base, pl.a));
        let g = dot3(pl.n, col);
        if g.abs() < 1e-12 {
            if f0 > HULL_TOL_MM + SLOP_MM {
                return None;
            }
            continue;
        }
        let t = (HULL_TOL_MM - f0) / g;
        if g > 0.0 {
            hi = hi.min(t);
        } else {
            lo = lo.max(t);
        }
    }
    let a = (lo.ceil() - 1.0).max(i0 as f64);
    let b = (hi.floor() + 1.0).min(i1 as f64);
    if a.is_nan() || b.is_nan() || a > b {
        return None;
    }
    let (a, b) = (a as usize, b as usize);
    let inside = |i: usize| {
        let w = fastwarp_core::apply_rowmajor(affine, [i as f64, j as f64, k as f64]);
        inside_planes(w, planes)
    };
    let first = (a..=b).find(|&i| inside(i))?;
    let last = (first..=b).rev().find(|&i| inside(i))?;
    Some((first, last))
}

fn boundary_voxels(m: &Array3<bool>) -> Vec<(usize, usize, usize)> {
    let (nk, nj, ni) = (m.shape()[0], m.shape()[1], m.shape()[2]);
    let per_k: Vec<Vec<(usize, usize, usize)>> = (0..nk)
        .into_par_iter()
        .map(|k| {
            let mut b = vec![];
            for j in 0..nj {
                for i in 0..ni {
                    if !m[(k, j, i)] {
                        continue;
                    }
                    let mut surf = false;
                    for (di, dj, dk) in neigh26() {
                        if (di | dj | dk) == 0 {
                            continue;
                        }
                        let ii = i as isize + di;
                        let jj = j as isize + dj;
                        let kk = k as isize + dk;
                        if ii < 0
                            || jj < 0
                            || kk < 0
                            || ii as usize >= ni
                            || jj as usize >= nj
                            || kk as usize >= nk
                        {
                            surf = true;
                            break;
                        }
                        if !m[(kk as usize, jj as usize, ii as usize)] {
                            surf = true;
                            break;
                        }
                    }
                    if surf {
                        b.push((i, j, k));
                    }
                }
            }
            b
        })
        .collect();
    per_k.into_iter().flatten().collect()
}

/// Convex hull in world mm, rasterized; falls back to `m` on hull build failure.
fn hull_fill(m: &Array3<bool>, affine: &fastwarp_core::Mat4) -> PacerResult<Array3<bool>> {
    let b = boundary_voxels(m);
    if b.len() < 8 {
        return Ok(m.clone());
    }
    let step = (b.len() / 5000).max(1);
    let pts: Vec<P3> = b
        .iter()
        .step_by(step)
        .map(|&(i, j, k)| fastwarp_core::apply_rowmajor(affine, [i as f64, j as f64, k as f64]))
        .collect();
    if pts.len() < 4 {
        return Ok(m.clone());
    }
    let n = pts.len() as f64;
    let cref: P3 = [
        pts.iter().map(|p| p[0]).sum::<f64>() / n,
        pts.iter().map(|p| p[1]).sum::<f64>() / n,
        pts.iter().map(|p| p[2]).sum::<f64>() / n,
    ];
    let hull = match Hull3D::build(&pts) {
        Some(h) => h,
        None => {
            return Ok(m.clone());
        }
    };
    let planes = hull_planes(&hull, cref);
    let (nk, nj, ni) = (m.shape()[0], m.shape()[1], m.shape()[2]);
    let mut i0 = ni;
    let mut i1 = 0usize;
    let mut j0 = nj;
    let mut j1 = 0;
    let mut k0 = nk;
    let mut k1 = 0;
    for &(i, j, k) in &b {
        i0 = i0.min(i);
        i1 = i1.max(i);
        j0 = j0.min(j);
        j1 = j1.max(j);
        k0 = k0.min(k);
        k1 = k1.max(k);
    }
    if i0 > i1 {
        return Ok(m.clone());
    }
    Ok(par_fill((nk, nj, ni), false, |k, out| {
        if k < k0 || k > k1 {
            return;
        }
        for j in j0..=j1 {
            if let Some((a, z)) = hull_row(&planes, affine, j, k, i0, i1) {
                for v in &mut out[j * ni + a..=j * ni + z] {
                    *v = true;
                }
            }
        }
    }))
}

/// §4: brain mask + offset flag for HU, then 3mm erosion in voxel units (§5 pre-metal).
pub fn build_brain_mask(
    vol: &Array3<f32>,
    vox: [f64; 3],
    affine: &fastwarp_core::Mat4,
) -> PacerResult<Array3<bool>> {
    let off = if vol.iter().copied().fold(f32::INFINITY, f32::min) >= 0.0 {
        1024.0f32
    } else {
        0.0f32
    };
    let lo = MIN_B + off;
    let hi = MAX_B + off;
    let (nk, nj, ni) = vol.dim();
    let th = par_fill((nk, nj, ni), false, |k, out| {
        for j in 0..nj {
            for i in 0..ni {
                let v = vol[(k, j, i)];
                out[j * ni + i] = v > lo && v < hi;
            }
        }
    });
    let th = medfilt3(&th);
    let r = max_v3(vox[0], vox[1], vox[2]);
    let r_vox = (2.0 / r).max(1.0);
    let mut morph = morph_open(&th, r_vox);
    let n_all = th.len();
    let frac = morph.iter().filter(|x| **x).count() as f64 / n_all as f64;
    if !(0.15..=0.30).contains(&frac) {
        let c = morph_close(&th, r_vox);
        morph = erode3(&c, &ball_offsets((2.0 / r).max(0.5)));
    }
    let mut cc = largest_3d_cc(&morph);
    let h = hull_fill(&cc, affine)?;
    cc = h;
    let r_er = (3.0 / r).max(1.0);
    let cc = erode3(&cc, &ball_offsets(r_er));
    Ok(cc)
}

/// §4 "Erosion before metal step" for a mask that was not computed here (`specs/02` §7.1): the
/// same ~3 mm spherical erosion `build_brain_mask` applies to its own hull.
pub fn erode_provided_mask(mask: &Array3<bool>, vox: [f64; 3]) -> Array3<bool> {
    let r = max_v3(vox[0], vox[1], vox[2]);
    erode3(mask, &ball_offsets((3.0 / r).max(1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    fn blob(shape: (usize, usize, usize), seed: u64) -> Array3<bool> {
        let mut g = Lcg(seed);
        let (nk, nj, ni) = shape;
        let c = [
            0.5 * ni as f64 + g.next(),
            0.5 * nj as f64 + g.next(),
            0.5 * nk as f64 + g.next(),
        ];
        let r = [
            0.3 * ni as f64 + 2.0 * g.next(),
            0.25 * nj as f64 + 2.0 * g.next(),
            0.35 * nk as f64 + 2.0 * g.next(),
        ];
        let mut m = Array3::from_shape_fn(shape, |(k, j, i)| {
            let x = (i as f64 - c[0]) / r[0];
            let y = (j as f64 - c[1]) / r[1];
            let z = (k as f64 - c[2]) / r[2];
            x * x + y * y + z * z < 1.0
        });
        for v in m.iter_mut() {
            if g.next() < 0.08 {
                *v = !*v;
            }
        }
        m
    }

    fn medfilt3_reference(x: &Array3<bool>) -> Array3<bool> {
        let (nk, nj, ni) = x.dim();
        let mut y = x.clone();
        for k in 0..nk {
            for j in 0..nj {
                for i in 0..ni {
                    let mut c = 0usize;
                    for dk in 0i32..=2 {
                        for dj in 0i32..=2 {
                            for di in 0i32..=2 {
                                let ii = (i as i32 + di - 1).clamp(0, ni as i32 - 1) as usize;
                                let jj = (j as i32 + dj - 1).clamp(0, nj as i32 - 1) as usize;
                                let kk = (k as i32 + dk - 1).clamp(0, nk as i32 - 1) as usize;
                                if x[(kk, jj, ii)] {
                                    c += 1;
                                }
                            }
                        }
                    }
                    y[(k, j, i)] = c > 13;
                }
            }
        }
        y
    }

    fn dilate3_reference(x: &Array3<bool>, off: &[(isize, isize, isize)]) -> Array3<bool> {
        let (nk, nj, ni) = x.dim();
        let mut y = Array3::from_elem((nk, nj, ni), false);
        for k in 0..nk {
            for j in 0..nj {
                for i in 0..ni {
                    if !x[(k, j, i)] {
                        continue;
                    }
                    for (di, dj, dk) in off {
                        let ii = i as isize + di;
                        let jj = j as isize + dj;
                        let kk = k as isize + dk;
                        if ii < 0 || jj < 0 || kk < 0 {
                            continue;
                        }
                        let (ii, jj, kk) = (ii as usize, jj as usize, kk as usize);
                        if ii < ni && jj < nj && kk < nk {
                            y[(kk, jj, ii)] = true;
                        }
                    }
                }
            }
        }
        y
    }

    fn erode3_reference(x: &Array3<bool>, off: &[(isize, isize, isize)]) -> Array3<bool> {
        let (nk, nj, ni) = x.dim();
        let mut y = x.clone();
        for k in 0..nk {
            for j in 0..nj {
                for i in 0..ni {
                    if !x[(k, j, i)] {
                        y[(k, j, i)] = false;
                        continue;
                    }
                    for (di, dj, dk) in off {
                        let ii = i as isize + di;
                        let jj = j as isize + dj;
                        let kk = k as isize + dk;
                        if ii < 0 || jj < 0 || kk < 0 {
                            y[(k, j, i)] = false;
                            break;
                        }
                        let (ii, jj, kk) = (ii as usize, jj as usize, kk as usize);
                        if ii >= ni || jj >= nj || kk >= nk || !x[(kk, jj, ii)] {
                            y[(k, j, i)] = false;
                            break;
                        }
                    }
                }
            }
        }
        y
    }

    fn point_in_hull_reference(p: P3, hull: &Hull3D, cref: P3) -> bool {
        let vs = &hull.pts;
        for &[v0, v1, v2] in &hull.faces {
            let a = vs[v0];
            let b = vs[v1];
            let c = vs[v2];
            let mut nn = cross3(sub3(b, a), sub3(c, a));
            let nlen = norm3sq(nn).sqrt();
            if nlen < 1e-12 {
                continue;
            }
            nn[0] /= nlen;
            nn[1] /= nlen;
            nn[2] /= nlen;
            if dot3(nn, sub3(cref, a)) > 0.0 {
                nn[0] = -nn[0];
                nn[1] = -nn[1];
                nn[2] = -nn[2];
            }
            if dot3(nn, sub3(p, a)) > 1e-4 {
                return false;
            }
        }
        true
    }

    fn hull_fill_reference(m: &Array3<bool>, affine: &fastwarp_core::Mat4) -> Array3<bool> {
        let b = boundary_voxels(m);
        let step = (b.len() / 5000).max(1);
        let pts: Vec<P3> = b
            .iter()
            .step_by(step)
            .map(|&(i, j, k)| fastwarp_core::apply_rowmajor(affine, [i as f64, j as f64, k as f64]))
            .collect();
        let n = pts.len() as f64;
        let cref: P3 = [
            pts.iter().map(|p| p[0]).sum::<f64>() / n,
            pts.iter().map(|p| p[1]).sum::<f64>() / n,
            pts.iter().map(|p| p[2]).sum::<f64>() / n,
        ];
        let hull = Hull3D::build(&pts).expect("hull");
        let (nk, nj, ni) = m.dim();
        let (mut i0, mut i1, mut j0, mut j1, mut k0, mut k1) = (ni, 0, nj, 0, nk, 0);
        for ((k, j, i), v) in m.indexed_iter() {
            if *v {
                i0 = i0.min(i);
                i1 = i1.max(i);
                j0 = j0.min(j);
                j1 = j1.max(j);
                k0 = k0.min(k);
                k1 = k1.max(k);
            }
        }
        let mut out = Array3::from_elem((nk, nj, ni), false);
        for k in k0..=k1 {
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let w = fastwarp_core::apply_rowmajor(affine, [i as f64, j as f64, k as f64]);
                    if point_in_hull_reference(w, &hull, cref) {
                        out[(k, j, i)] = true;
                    }
                }
            }
        }
        out
    }

    #[test]
    fn parallel_filters_match_the_serial_reference() {
        for (seed, shape) in [(1u64, (17, 23, 29)), (2, (31, 12, 20)), (3, (8, 40, 9))] {
            let m = blob(shape, seed);
            assert_eq!(medfilt3(&m), medfilt3_reference(&m));
            for r in [1.0, 1.7, 3.0] {
                let off = ball_offsets(r);
                assert_eq!(
                    dilate3(&m, &off),
                    dilate3_reference(&m, &off),
                    "dilate r={r}"
                );
                assert_eq!(erode3(&m, &off), erode3_reference(&m, &off), "erode r={r}");
            }
        }
    }

    #[test]
    fn row_interval_hull_fill_matches_the_per_voxel_test() {
        let (c, s) = (0.31f64.cos(), 0.31f64.sin());
        let affines: [fastwarp_core::Mat4; 3] = [
            [
                [0.5, 0.0, 0.0, -10.0],
                [0.0, 0.5, 0.0, -12.0],
                [0.0, 0.0, 0.5, -8.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            [
                [-0.45, 0.0, 0.0, 20.0],
                [0.0, 0.45, 0.0, -12.0],
                [0.0, 0.0, 1.25, -30.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            [
                [0.6 * c, -0.6 * s, 0.1, 3.0],
                [0.6 * s, 0.6 * c, -0.2, -4.0],
                [0.05, 0.2, 1.1, -9.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        ];
        for (n, aff) in affines.iter().enumerate() {
            let m = largest_3d_cc(&medfilt3(&blob((26, 34, 38), 5 + n as u64)));
            let got = hull_fill(&m, aff).expect("hull");
            let want = hull_fill_reference(&m, aff);
            let diff = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
            assert_eq!(diff, 0, "affine {n}");
            assert!(got.iter().filter(|v| **v).count() > m.iter().filter(|v| **v).count() / 2);
        }
    }
}
