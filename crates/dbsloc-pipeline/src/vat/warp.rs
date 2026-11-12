// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Native to template warping of VTA fields (`specs/07` §4): a trilinear image warp of the
//! native |E| (or the analytical sphere mask) through the forward normalization composite onto
//! a template output grid, with the supra-threshold region found through the point transform.

use std::path::PathBuf;

use dbsloc_vat::field::{
    count_set, fill_holes, open_ball, output_grid_for_bbox, threshold, EFIELD_CLAMP_V_PER_M,
    OPENING_RADIUS_VOXELS,
};
use dbsloc_vat::{Arrow, Grid as VatGrid};
use fastwarp_core::Mat4;
use fastwarp_reg::apply::{apply_chain, Interpolation};
use fastwarp_reg::transform::{read_transform_file, TransformChain};
use fastwarp_reg::{Grid, Volume};

use crate::bids::Subject;
use crate::point_warp::{PointWarp, RustPointWarp};
use crate::vat::outputs::Gradient;

/// A point mapping between two spaces.
pub type PointMap<'a> = dyn Fn(&[[f64; 3]]) -> Result<Vec<[f64; 3]>, String> + 'a;

/// The grid, the field and the binary of one hemisphere in one space.
#[derive(Debug, Clone)]
pub struct Field {
    pub dims: [usize; 3],
    pub affine: Mat4,
    pub e_mag: Vec<f32>,
    pub binary_raw: Vec<u8>,
    pub binary: Vec<u8>,
    pub raw_mm3: f64,
    pub final_mm3: f64,
}

/// The normalization of a subject: the image chain (template to native points) and the point
/// chain (native to template points) of its composites.
#[derive(Debug, Clone)]
pub struct SpaceWarp {
    pub space: String,
    image_chain: TransformChain,
    point_chain: TransformChain,
    linear_chain: Option<TransformChain>,
    pub files: Vec<PathBuf>,
}

/// Why a warp is not available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable(pub String);

impl SpaceWarp {
    /// Reads the composites of the subject's normalization.
    pub fn load(sub: &Subject) -> Result<SpaceWarp, Unavailable> {
        let space = sub
            .norm
            .template_space
            .clone()
            .ok_or_else(|| Unavailable("no normalization".to_owned()))?;
        let forward = sub
            .norm
            .transform_forward
            .clone()
            .filter(|p| p.is_file())
            .ok_or_else(|| Unavailable("no forward normalization transform".to_owned()))?;
        let inverse = sub
            .norm
            .transform_inverse
            .clone()
            .filter(|p| p.is_file())
            .ok_or_else(|| Unavailable("no inverse normalization transform".to_owned()))?;
        let read = |p: &PathBuf| {
            read_transform_file(p).map_err(|e| Unavailable(format!("{}: {e}", p.display())))
        };
        let image_chain = read(&forward)?;
        let point_chain = read(&inverse)?;
        let linear_path = crate::derivatives::norm_inverse_affine(sub, &space);
        let linear_chain = linear_path
            .is_file()
            .then(|| read_transform_file(&linear_path).ok())
            .flatten();
        let mut files = vec![forward, inverse];
        if linear_chain.is_some() {
            files.push(linear_path);
        }
        Ok(SpaceWarp {
            space,
            image_chain,
            point_chain,
            linear_chain,
            files,
        })
    }

    /// One native point to a template point, evaluating the chain directly.
    pub fn point_direct(&self, p: [f64; 3]) -> [f64; 3] {
        fastwarp_reg::map_point_ras(&self.point_chain, p)
    }

    /// Native points to template points (`specs/05` §6).
    pub fn points(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, String> {
        RustPointWarp::from_chain(self.point_chain.clone())
            .to_template(pts)
            .map_err(|e| e.to_string())
    }

    fn rotate(&self, v: [f64; 3]) -> [f64; 3] {
        let Some(chain) = &self.linear_chain else {
            return v;
        };
        let o = fastwarp_reg::map_point_ras(chain, [0.0; 3]);
        let t = fastwarp_reg::map_point_ras(chain, v);
        let r = [t[0] - o[0], t[1] - o[1], t[2] - o[2]];
        let n = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
        if n > 0.0 {
            [r[0] / n, r[1] / n, r[2] / n]
        } else {
            v
        }
    }

    /// Arrows moved to template space: positions through the point warp, directions rotated by
    /// the linear part of the normalization (`specs/07` §4 step 6).
    pub fn gradient(&self, arrows: &[Arrow], length_mm: f64) -> Result<Gradient, String> {
        let pos: Vec<[f64; 3]> = arrows.iter().map(|a| a.position).collect();
        let mapped = self.points(&pos)?;
        Ok(Gradient {
            positions: mapped,
            vectors: arrows
                .iter()
                .map(|a| {
                    let d = self.rotate(a.direction);
                    [d[0] * length_mm, d[1] * length_mm, d[2] * length_mm]
                })
                .collect(),
        })
    }
}

/// Native arrows as a [`Gradient`] with vectors of `length_mm`.
pub fn native_gradient(arrows: &[Arrow], length_mm: f64) -> Gradient {
    Gradient {
        positions: arrows.iter().map(|a| a.position).collect(),
        vectors: arrows
            .iter()
            .map(|a| {
                [
                    a.direction[0] * length_mm,
                    a.direction[1] * length_mm,
                    a.direction[2] * length_mm,
                ]
            })
            .collect(),
    }
}

fn voxel_world(aff: &Mat4, i: usize, j: usize, k: usize) -> [f64; 3] {
    let p = [i as f64, j as f64, k as f64];
    [
        aff[0][0] * p[0] + aff[0][1] * p[1] + aff[0][2] * p[2] + aff[0][3],
        aff[1][0] * p[0] + aff[1][1] * p[1] + aff[1][2] * p[2] + aff[1][3],
        aff[2][0] * p[0] + aff[2][1] * p[1] + aff[2][2] * p[2] + aff[2][3],
    ]
}

impl Field {
    /// The field of a native engine result.
    pub fn from_native(r: &dbsloc_vat::FieldResult) -> Field {
        Field {
            dims: r.binary.dims,
            affine: r.affine,
            e_mag: r.e_mag.data.clone(),
            binary_raw: r.binary_raw.data.clone(),
            binary: r.binary.data.clone(),
            raw_mm3: r.volumes.raw_mm3,
            final_mm3: r.volumes.final_mm3,
        }
    }

    fn has_field(&self) -> bool {
        !self.e_mag.is_empty()
    }

    fn supra_centres(&self, thr: f32) -> Vec<[f64; 3]> {
        let [nx, ny, nz] = self.dims;
        let mut out = Vec::new();
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let idx = i + nx * (j + ny * k);
                    let hit = if self.has_field() {
                        self.e_mag[idx] > thr
                    } else {
                        self.binary[idx] != 0
                    };
                    if hit {
                        out.push(voxel_world(&self.affine, i, j, k));
                    }
                }
            }
        }
        out
    }

    fn bounds(points: &[[f64; 3]]) -> Option<([f64; 3], [f64; 3])> {
        let first = points.first()?;
        let mut lo = *first;
        let mut hi = *first;
        for p in points {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        Some((lo, hi))
    }
}

fn grid_of(dims: [usize; 3], affine: &Mat4) -> Result<Grid, String> {
    Grid::from_ras_affine(dims, affine).map_err(|e| e.to_string())
}

/// Resamples `src` onto a new output grid through `image_chain` (output points to source
/// points); `mapped` are the source supra-threshold centres in the output space.
fn resample(
    src: &Field,
    thr: f32,
    image_chain: &TransformChain,
    mapped: &[[f64; 3]],
    spacing: f64,
    warnings: &mut Vec<String>,
) -> Result<Option<Field>, String> {
    let Some((lo, hi)) = Field::bounds(mapped) else {
        return Ok(None);
    };
    let out = output_grid_for_bbox(lo, hi, spacing, warnings);
    let src_grid = grid_of(src.dims, &src.affine)?;
    let out_affine = out.affine();
    let out_grid = grid_of(out.dims, &out_affine)?;
    let analytic = !src.has_field();
    let values: Vec<f64> = if analytic {
        src.binary.iter().map(|&v| f64::from(v)).collect()
    } else {
        src.e_mag.iter().map(|&v| f64::from(v)).collect()
    };
    let image = Volume::from_vec(src_grid, values).map_err(|e| e.to_string())?;
    let resampled = apply_chain(&image, &out_grid, image_chain, Interpolation::Linear, 0.0)
        .map_err(|e| e.to_string())?;
    let vox = out.spacing.powi(3);
    if analytic {
        let binary: Vec<u8> = resampled
            .data()
            .iter()
            .map(|&v| u8::from(v >= 0.5))
            .collect();
        let raw = count_set(&binary) as f64 * vox;
        return Ok(Some(Field {
            dims: out.dims,
            affine: out_affine,
            e_mag: Vec::new(),
            binary_raw: binary.clone(),
            binary,
            raw_mm3: raw,
            final_mm3: raw,
        }));
    }
    let e: Vec<f32> = resampled
        .data()
        .iter()
        .map(|&v| (v as f32).min(EFIELD_CLAMP_V_PER_M))
        .collect();
    let raw_mask = threshold(&e, thr);
    let raw_mm3 = count_set(&raw_mask) as f64 * vox;
    let binary_raw = raw_mask;
    let mut filled = VatGrid {
        dims: out.dims,
        data: binary_raw.clone(),
    };
    fill_holes(&mut filled);
    let opened = open_ball(&filled, OPENING_RADIUS_VOXELS);
    let final_mm3 = count_set(&opened.data) as f64 * vox;
    Ok(Some(Field {
        dims: out.dims,
        affine: out_affine,
        e_mag: e,
        binary_raw,
        binary: opened.data,
        raw_mm3,
        final_mm3,
    }))
}

/// The native field warped into the template space of `warp` (`specs/07` §4 step 2 and
/// §3.8 steps 4 to 7 on the warped values); `None` when no voxel is supra-threshold.
pub fn to_template(
    native: &Field,
    thr_v_per_m: f32,
    spacing: f64,
    warp: &SpaceWarp,
    warnings: &mut Vec<String>,
) -> Result<Option<Field>, String> {
    let centres = native.supra_centres(thr_v_per_m);
    let mapped = warp.points(&centres)?;
    resample(
        native,
        thr_v_per_m,
        &warp.image_chain,
        &mapped,
        spacing,
        warnings,
    )
}

/// The template field resampled into the reference space through the bridge (`specs/07` §4
/// step 3): `points` maps template points to reference points, `image_chain` is the bridge
/// inverse composite.
pub fn to_reference(
    template: &Field,
    thr_v_per_m: f32,
    spacing: f64,
    image_chain: &TransformChain,
    points: &PointMap<'_>,
    warnings: &mut Vec<String>,
) -> Result<Option<Field>, String> {
    let centres = template.supra_centres(thr_v_per_m);
    let mapped = points(&centres)?;
    resample(
        template,
        thr_v_per_m,
        image_chain,
        &mapped,
        spacing,
        warnings,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use fastwarp_reg::transform::{AffineTransform, TransformElem};

    fn translation_warp(ras_shift: [f64; 3]) -> SpaceWarp {
        let lps = [ras_shift[0], ras_shift[1], -ras_shift[2]];
        let forward = AffineTransform::from_translation(lps);
        let inverse = forward.inverse().unwrap();
        SpaceWarp {
            space: "MNI152NLin2009bAsym".to_owned(),
            image_chain: TransformChain::from_queue(vec![TransformElem::Affine(forward)]),
            point_chain: TransformChain::from_queue(vec![TransformElem::Affine(inverse)]),
            linear_chain: None,
            files: Vec::new(),
        }
    }

    fn cube_field() -> Field {
        let dims = [31, 31, 31];
        let aff = [
            [0.5, 0.0, 0.0, 0.0],
            [0.0, 0.5, 0.0, 0.0],
            [0.0, 0.0, 0.5, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut e = vec![0f32; 31 * 31 * 31];
        for k in 10..21usize {
            for j in 10..21usize {
                for i in 10..21usize {
                    e[i + 31 * (j + 31 * k)] = 500.0;
                }
            }
        }
        let binary: Vec<u8> = e.iter().map(|&v| u8::from(v > 200.0)).collect();
        let raw_mm3 = binary.iter().filter(|&&v| v != 0).count() as f64 * 0.125;
        Field {
            dims,
            affine: aff,
            e_mag: e,
            binary_raw: binary.clone(),
            binary,
            raw_mm3,
            final_mm3: raw_mm3,
        }
    }

    fn centre_of(f: &Field) -> [f64; 3] {
        let [nx, ny, nz] = f.dims;
        let mut sum = [0.0; 3];
        let mut n = 0.0;
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    if f.binary[i + nx * (j + ny * k)] != 0 {
                        let w = voxel_world(&f.affine, i, j, k);
                        for a in 0..3 {
                            sum[a] += w[a];
                        }
                        n += 1.0;
                    }
                }
            }
        }
        [sum[0] / n, sum[1] / n, sum[2] / n]
    }

    #[test]
    fn a_translation_moves_the_vta_by_the_translation() {
        let native = cube_field();
        let c0 = centre_of(&native);
        let warp = translation_warp([3.0, -2.0, 4.0]);
        let mut w = Vec::new();
        let t = to_template(&native, 200.0, 0.5, &warp, &mut w)
            .unwrap()
            .unwrap();
        let c1 = centre_of(&t);
        assert!((c1[0] - c0[0] - 3.0).abs() < 0.3, "{c1:?} {c0:?}");
        assert!((c1[1] - c0[1] + 2.0).abs() < 0.3, "{c1:?} {c0:?}");
        assert!((c1[2] - c0[2] - 4.0).abs() < 0.3, "{c1:?} {c0:?}");
        assert!((t.raw_mm3 - native.raw_mm3).abs() / native.raw_mm3 < 0.2);
    }

    #[test]
    fn an_empty_field_has_no_template_field() {
        let mut native = cube_field();
        native.e_mag.iter_mut().for_each(|v| *v = 0.0);
        let warp = translation_warp([0.0; 3]);
        let mut w = Vec::new();
        assert!(to_template(&native, 200.0, 0.5, &warp, &mut w)
            .unwrap()
            .is_none());
    }
}
