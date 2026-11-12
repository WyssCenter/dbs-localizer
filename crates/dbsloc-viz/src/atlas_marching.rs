// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Labeled NIfTI → triangle meshes (marching cubes, MIT `mcubes`).
//!
//! Lead-DBS layout: per-structure `.nii` / `.nii.gz` under `lh/`, `rh/`, `mixed/`, `midline/`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use glam::Vec3;
use lin_alg::f32::Vec3 as McVec3;
use mcubes::{MarchingCubes, MeshSide};
use nifti::NiftiHeader;
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::atlas_wavefront::{load_obj_triangles, save_obj_triangles, TriMeshScene};
use crate::error::{VizError, VizResult};
use crate::scene_volume::{resample_binary_mask_to_template, TemplateVolume, VoxelWorld};

/// Knobs for Lead-layout atlas meshing (template resample, smoothing, MATLAB-style index shift on native grid).
pub struct LeadAtlasMeshParams<'a> {
    pub template: Option<&'a TemplateVolume>,
    /// When true and `template` set, resample each mask onto the template voxel grid (nearest center).
    pub resample_to_template: bool,
    pub mesh_smooth_iters: u32,
    /// Subtracted from continuous source ijk before rounding during resample (parity knob vs other toolchains).
    pub source_ijk_subtract: f32,
    /// Added to marching-cubes vertex (i,j,k) before the voxel → world affine when meshing **on atlas native grid**;
    /// **0** is correct: `mcubes` vertices are 0-based voxel indices, as is the NIfTI affine. Ignored after resample (`0` used).
    pub native_vertex_ijk_shift: f32,
}

/// Same subdirectory names Lead-DBS uses (`ea_genatlastable`); `heatmap/` skipped for now.
const LEAD_ATLAS_SUBDIRS: [&str; 4] = ["lh", "rh", "mixed", "midline"];
const MAX_LEAD_ATLAS_FILES: usize = 512;

/// Cache key of a meshed mask: the mask's content hash and the meshing parameters, nothing
/// machine-specific, so a `.obj` + `.cache.json` pair shipped beside the masks is accepted on
/// every install.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AtlasMeshCacheKey {
    version: u32,
    src_sha256: String,
    template_sha256: Option<String>,
    resample_to_template: bool,
    source_ijk_subtract: f32,
    native_vertex_ijk_shift: f32,
    preblur_iters: u32,
    morph_close_iters: u32,
    weld_eps_mm: f32,
    taubin_iters: u32,
    taubin_lambda: f32,
    taubin_mu: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    split: Option<HemisphereSplit>,
}

const ATLAS_MESH_CACHE_VERSION: u32 = 6;

/// Half of a `mixed/` file kept for one side (`specs/11` §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HemisphereSplit {
    Right,
    Left,
}

/// Where one atlas structure-side comes from and how it is thresholded.
#[derive(Debug, Clone, PartialEq)]
pub struct LeadRoiSource {
    pub path: PathBuf,
    /// Iso-level in processed-image units; `None` → half the image maximum (`specs/11` §4.2).
    pub threshold: Option<f32>,
    pub split: Option<HemisphereSplit>,
}

impl LeadRoiSource {
    pub fn whole(path: PathBuf) -> Self {
        LeadRoiSource {
            path,
            threshold: None,
            split: None,
        }
    }
}

/// Where meshed atlas structures are looked up and stored: the `precomputed_obj` directory
/// shipped beside the masks (read only) and the per-user cache directory (read, then written).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlasMeshCacheDirs {
    pub shipped: PathBuf,
    pub user: Option<PathBuf>,
}

impl AtlasMeshCacheDirs {
    pub fn for_lead_set(root: &Path, cache_dir_name: &str) -> Self {
        AtlasMeshCacheDirs {
            shipped: root.join(cache_dir_name),
            user: user_mesh_cache_dir(),
        }
    }
}

/// The per-user atlas mesh cache: `%LOCALAPPDATA%\dbsloc\cache\atlas_meshes` on Windows,
/// `~/Library/Caches/dbsloc/atlas_meshes` on macOS, `$XDG_CACHE_HOME/dbsloc/atlas_meshes`
/// (else `~/.cache/dbsloc/atlas_meshes`) elsewhere.
pub fn user_mesh_cache_dir() -> Option<PathBuf> {
    user_mesh_cache_dir_from(|name| std::env::var_os(name).map(PathBuf::from))
}

fn user_mesh_cache_dir_from(env: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    let nonempty = |name: &str| env(name).filter(|p| !p.as_os_str().is_empty());
    let base = if cfg!(target_os = "windows") {
        nonempty("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        nonempty("HOME")?.join("Library/Caches")
    } else {
        nonempty("XDG_CACHE_HOME").or_else(|| nonempty("HOME").map(|h| h.join(".cache")))?
    };
    Some(base.join("dbsloc").join("cache").join("atlas_meshes"))
}

fn morph3d_close_radius1(dims: (usize, usize, usize), field: &mut [f32], iters: u32) {
    let (nx, ny, nz) = dims;
    if nx == 0 || ny == 0 || nz == 0 || iters == 0 {
        return;
    }
    let n = nx * ny * nz;
    if field.len() != n {
        return;
    }
    let idx = |i: usize, j: usize, k: usize| -> usize { k * ny * nx + j * nx + i };
    let mut tmp = vec![0.0_f32; n];
    for _ in 0..iters {
        for k in 0..nz {
            let k0 = k.saturating_sub(1);
            let k1 = (k + 1).min(nz - 1);
            for j in 0..ny {
                let j0 = j.saturating_sub(1);
                let j1 = (j + 1).min(ny - 1);
                for i in 0..nx {
                    let i0 = i.saturating_sub(1);
                    let i1 = (i + 1).min(nx - 1);
                    let mut mx = 0.0_f32;
                    for kk in [k0, k, k1] {
                        for jj in [j0, j, j1] {
                            for ii in [i0, i, i1] {
                                mx = mx.max(field[idx(ii, jj, kk)]);
                            }
                        }
                    }
                    tmp[idx(i, j, k)] = mx;
                }
            }
        }
        field.copy_from_slice(&tmp);

        for k in 0..nz {
            let k0 = k.saturating_sub(1);
            let k1 = (k + 1).min(nz - 1);
            for j in 0..ny {
                let j0 = j.saturating_sub(1);
                let j1 = (j + 1).min(ny - 1);
                for i in 0..nx {
                    let i0 = i.saturating_sub(1);
                    let i1 = (i + 1).min(nx - 1);
                    let mut mn = f32::INFINITY;
                    for kk in [k0, k, k1] {
                        for jj in [j0, j, j1] {
                            for ii in [i0, i, i1] {
                                mn = mn.min(field[idx(ii, jj, kk)]);
                            }
                        }
                    }
                    tmp[idx(i, j, k)] = mn;
                }
            }
        }
        field.copy_from_slice(&tmp);
    }
}

fn file_sha256(path: &Path) -> VizResult<String> {
    fastwarp_core::sha256::file_sha256_hex(path)
        .map_err(|e| VizError::NiftiLoad(format!("{}: {e}", path.display())))
}

fn sanitize_roi_label_for_filename(label: &str) -> String {
    let mut s = String::with_capacity(label.len().min(128));
    for ch in label.chars().take(128) {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => s.push(ch),
            '.' | '/' | '\\' => s.push('_'),
            _ => s.push('_'),
        }
    }
    let s = s.trim_start_matches(['_', '.']);
    if s.is_empty() {
        "roi".to_string()
    } else {
        s.to_string()
    }
}

pub(crate) fn read_nifti_volume_f32(
    path: &Path,
) -> VizResult<(NiftiHeader, usize, usize, usize, Vec<f32>)> {
    let vol = dbsloc_checkreg::nifti_io::read_volume_f32(path)?;
    let [n0, n1, n2] = vol.dims;
    Ok((vol.header, n0, n1, n2, vol.data))
}

fn collect_labels_from_flat(data: &[f32], n0: usize, n1: usize, n2: usize) -> Vec<u32> {
    let mut set: HashSet<u32> = HashSet::new();
    let mut k = 0usize;
    for _k in 0..n2 {
        for _j in 0..n1 {
            for _i in 0..n0 {
                let v = data[k];
                k += 1;
                let lab = v.round() as i32;
                if lab > 0 {
                    set.insert(lab as u32);
                }
            }
        }
    }
    let mut labels: Vec<u32> = set.into_iter().collect();
    labels.sort_unstable();
    labels
}

/// Apply a 3×3 matrix to a Vec3 (row-major: out[i] = sum_j m[i][j] * v[j]).
fn mat3_mul_vec(m: [[f32; 3]; 3], v: Vec3) -> Vec3 {
    Vec3::new(
        m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z,
        m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z,
        m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z,
    )
}

fn marching_cubes_from_field(
    hdr: &NiftiHeader,
    dims: (usize, usize, usize),
    values: Vec<f32>,
    level: f32,
    vertex_ijk_shift: f32,
    origin: [isize; 3],
    smooth_iters: u32,
) -> VizResult<TriMeshScene> {
    let (nx, ny, nz) = dims;
    let mc = MarchingCubes::new(
        (nx, ny, nz),
        (1.0, 1.0, 1.0),
        (1.0, 1.0, 1.0),
        McVec3::new(0.0, 0.0, 0.0),
        values,
        level,
    )
    .map_err(|e| VizError::NiftiLoad(e.to_string()))?;

    let mesh = mc.generate(MeshSide::Both);
    let s = vertex_ijk_shift;
    let [o0, o1, o2] = origin.map(|o| o as f32);
    let to_world = VoxelWorld::from_header(hdr);
    let rot = to_world.linear();

    let mut positions = Vec::with_capacity(mesh.vertices.len());
    let mut normals = Vec::with_capacity(mesh.vertices.len());
    let mut indices: Vec<u32> = Vec::with_capacity(mesh.indices.len());

    for v in &mesh.vertices {
        let p = v.posit;
        let w = to_world.apply(p.x + o0 + s, p.y + o1 + s, p.z + o2 + s);
        positions.push(w);
        let n_ijk = Vec3::new(v.normal.x, v.normal.y, v.normal.z);
        let n_world = mat3_mul_vec(rot, n_ijk).normalize_or_zero();
        normals.push(n_world);
    }
    for &ix in &mesh.indices {
        indices.push(ix as u32);
    }

    let mut out = TriMeshScene {
        positions,
        normals,
        indices,
    };
    if smooth_iters > 0 {
        out.laplacian_smooth(smooth_iters, 0.45);
        out.recompute_vertex_normals();
    }
    Ok(out)
}

pub(crate) fn blur3d_box_radius1(dims: (usize, usize, usize), field: &mut [f32], iters: u32) {
    let (nx, ny, nz) = dims;
    if nx == 0 || ny == 0 || nz == 0 || iters == 0 {
        return;
    }
    let n = nx * ny * nz;
    if field.len() != n {
        return;
    }
    let mut tmp = vec![0.0_f32; n];
    let idx = |i: usize, j: usize, k: usize| -> usize { k * ny * nx + j * nx + i };
    for _ in 0..iters {
        for k in 0..nz {
            let k0 = k.saturating_sub(1);
            let k1 = (k + 1).min(nz - 1);
            for j in 0..ny {
                let j0 = j.saturating_sub(1);
                let j1 = (j + 1).min(ny - 1);
                for i in 0..nx {
                    let i0 = i.saturating_sub(1);
                    let i1 = (i + 1).min(nx - 1);
                    let mut s = 0.0_f32;
                    let mut c = 0.0_f32;
                    for kk in [k0, k, k1] {
                        for jj in [j0, j, j1] {
                            for ii in [i0, i, i1] {
                                s += field[idx(ii, jj, kk)];
                                c += 1.0;
                            }
                        }
                    }
                    tmp[idx(i, j, k)] = s / c;
                }
            }
        }
        field.copy_from_slice(&tmp);
    }
}

fn mesh_one_label(
    hdr: &NiftiHeader,
    dims: (usize, usize, usize),
    vol_data: &[f32],
    label: u32,
) -> VizResult<TriMeshScene> {
    let values: Vec<f32> = vol_data
        .iter()
        .map(|&v| {
            let l = v.round() as u32;
            if l == label {
                1.0_f32
            } else {
                0.0_f32
            }
        })
        .collect();
    marching_cubes_from_field(hdr, dims, values, 0.5, 0.0, [0; 3], 0)
}

/// `specs/11` §2.5 and §4.1 steps 3–4 in place: non-finite → 0, a graded image is shifted down by
/// its smallest inside value. Returns the iso-level (`explicit`, else half the unprocessed
/// maximum), or `None` when no voxel is inside.
pub(crate) fn prepare_mask_values(data: &mut [f32], explicit: Option<f32>) -> Option<f32> {
    let mut first: Option<f32> = None;
    let mut binary = true;
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    for v in data.iter_mut() {
        if !v.is_finite() {
            *v = 0.0;
            continue;
        }
        if *v == 0.0 {
            continue;
        }
        match first {
            None => first = Some(*v),
            Some(f) if f != *v => binary = false,
            Some(_) => {}
        }
        min = min.min(*v);
        max = max.max(*v);
    }
    first?;
    if !binary {
        for v in data.iter_mut().filter(|v| **v != 0.0) {
            *v -= min;
        }
    }
    Some(explicit.filter(|t| t.is_finite()).unwrap_or(max * 0.5))
}

/// `specs/11` §2.4: zero every voxel whose centre lies on the other side of world x = 0.
pub(crate) fn split_hemisphere(
    hdr: &NiftiHeader,
    dims: (usize, usize, usize),
    data: &mut [f32],
    keep: HemisphereSplit,
) {
    let (nx, ny, nz) = dims;
    if data.len() != nx * ny * nz {
        return;
    }
    let to_world = VoxelWorld::from_header(hdr);
    for k in 0..nz {
        for j in 0..ny {
            let row = &mut data[(k * ny + j) * nx..(k * ny + j + 1) * nx];
            for (i, v) in row.iter_mut().enumerate() {
                let x = to_world.apply(i as f32, j as f32, k as f32).x;
                let drop = match keep {
                    HemisphereSplit::Right => x < 0.0,
                    HemisphereSplit::Left => x > 0.0,
                };
                if drop {
                    *v = 0.0;
                }
            }
        }
    }
}

/// Bounding box of the non-zero voxels grown by `pad` on every side; the box may reach past the
/// volume, where [`crop_field`] fills zeros so a structure touching the boundary still closes.
pub(crate) fn nonzero_crop(
    dims: (usize, usize, usize),
    field: &[f32],
    pad: usize,
) -> Option<([isize; 3], (usize, usize, usize))> {
    let (nx, ny, nz) = dims;
    let mut lo = [usize::MAX; 3];
    let mut hi = [0usize; 3];
    for k in 0..nz {
        for j in 0..ny {
            let row = &field[(k * ny + j) * nx..(k * ny + j + 1) * nx];
            let Some(first) = row.iter().position(|&v| v != 0.0) else {
                continue;
            };
            let last = row.iter().rposition(|&v| v != 0.0).unwrap_or(first);
            lo = [lo[0].min(first), lo[1].min(j), lo[2].min(k)];
            hi = [hi[0].max(last), hi[1].max(j), hi[2].max(k)];
        }
    }
    if lo[0] == usize::MAX {
        return None;
    }
    let origin = [0, 1, 2].map(|a| lo[a] as isize - pad as isize);
    let size = [0, 1, 2].map(|a| hi[a] - lo[a] + 1 + 2 * pad);
    Some((origin, (size[0], size[1], size[2])))
}

pub(crate) fn crop_field(
    dims: (usize, usize, usize),
    field: &[f32],
    origin: [isize; 3],
    sub: (usize, usize, usize),
) -> Vec<f32> {
    let (nx, ny, nz) = dims;
    let (sx, sy, sz) = sub;
    let mut out = vec![0.0_f32; sx * sy * sz];
    let i0 = origin[0].max(0);
    let i1 = (origin[0] + sx as isize).min(nx as isize);
    if i0 >= i1 {
        return out;
    }
    let run = (i1 - i0) as usize;
    let dst_i = (i0 - origin[0]) as usize;
    for kk in 0..sz {
        let k = origin[2] + kk as isize;
        if k < 0 || k >= nz as isize {
            continue;
        }
        for jj in 0..sy {
            let j = origin[1] + jj as isize;
            if j < 0 || j >= ny as isize {
                continue;
            }
            let src = (k as usize * ny + j as usize) * nx + i0 as usize;
            let dst = (kk * sy + jj) * sx + dst_i;
            out[dst..dst + run].copy_from_slice(&field[src..src + run]);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn mesh_mask_field(
    hdr: &NiftiHeader,
    dims: (usize, usize, usize),
    field: Vec<f32>,
    level: f32,
    morph_close_iters: u32,
    preblur_iters: u32,
    vertex_ijk_shift: f32,
    smooth_iters: u32,
    crop: bool,
) -> VizResult<TriMeshScene> {
    let pad = morph_close_iters as usize + preblur_iters as usize + 2;
    let (origin, sub_dims, mut sub) = match crop.then(|| nonzero_crop(dims, &field, pad)).flatten()
    {
        Some((origin, sub_dims)) => (origin, sub_dims, crop_field(dims, &field, origin, sub_dims)),
        None => ([0; 3], dims, field),
    };
    morph3d_close_radius1(sub_dims, &mut sub, morph_close_iters);
    blur3d_box_radius1(sub_dims, &mut sub, preblur_iters);
    marching_cubes_from_field(
        hdr,
        sub_dims,
        sub,
        level,
        vertex_ijk_shift,
        origin,
        smooth_iters,
    )
}

#[derive(Debug, Clone)]
pub struct NamedAtlasMesh {
    pub name: String,
    pub mesh: Option<TriMeshScene>,
    pub color: [f32; 4],
    /// Lead-layout source; `mesh` filled on demand when `atlas_lead_lazy_load`.
    pub source: Option<LeadRoiSource>,
}

fn empty_mesh() -> TriMeshScene {
    TriMeshScene {
        positions: Vec::new(),
        normals: Vec::new(),
        indices: Vec::new(),
    }
}

pub(crate) fn atlas_label_color(name: &str, id: u32) -> [f32; 4] {
    let mut h = id.wrapping_mul(0x9E37_79B9);
    for b in name.as_bytes() {
        h = h.wrapping_add(*b as u32).wrapping_mul(0x85EB_CA6B);
    }
    let r = ((h & 0xFF) as f32) / 255.0 * 0.85 + 0.15;
    let g = (((h >> 8) & 0xFF) as f32) / 255.0 * 0.85 + 0.15;
    let b = (((h >> 16) & 0xFF) as f32) / 255.0 * 0.85 + 0.15;
    [r, g, b, DEFAULT_ATLAS_ALPHA]
}

/// Face opacity of a structure whose index gives none (`specs/11` §4.4).
pub(crate) const DEFAULT_ATLAS_ALPHA: f32 = 0.7;

pub(crate) fn mesh_binary_mask_nifti_with_params(
    src: &LeadRoiSource,
    display_name: &str,
    params: &LeadAtlasMeshParams<'_>,
) -> VizResult<NamedAtlasMesh> {
    mesh_binary_mask_nifti_with_params_inner(
        src,
        display_name,
        params,
        None,
        None,
        0,
        0,
        0.05,
        10,
        0.5,
        -0.53,
    )
}

#[derive(Deserialize)]
struct CachedObjDigest {
    #[serde(default)]
    obj_sha256: Option<String>,
}

fn user_cache_stem(stem: &str, key: &AtlasMeshCacheKey) -> String {
    let text = serde_json::to_string(key).unwrap_or_default();
    let hash = fastwarp_core::sha256::sha256_hex(text.as_bytes());
    format!("{stem}-{}", &hash[..16])
}

fn cached_mesh_in(dir: &Path, stem: &str, key: &AtlasMeshCacheKey) -> Option<PathBuf> {
    let obj_path = dir.join(format!("{stem}.obj"));
    let text = std::fs::read_to_string(dir.join(format!("{stem}.cache.json"))).ok()?;
    let got: AtlasMeshCacheKey = serde_json::from_str(&text).ok()?;
    if got != *key || !obj_path.is_file() {
        return None;
    }
    let digest: CachedObjDigest = serde_json::from_str(&text).ok()?;
    if let Some(want) = digest.obj_sha256 {
        if file_sha256(&obj_path).ok()? != want {
            return None;
        }
    }
    Some(obj_path)
}

fn temp_sibling(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}-{nanos}.tmp", std::process::id()))
}

fn rename_into_place(tmp: &Path, dst: &Path) -> bool {
    if std::fs::rename(tmp, dst).is_ok() {
        return true;
    }
    let _ = std::fs::remove_file(tmp);
    false
}

fn store_mesh_in(dir: &Path, stem: &str, key: &AtlasMeshCacheKey, mesh: &TriMeshScene) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let obj_path = dir.join(format!("{stem}.obj"));
    let obj_tmp = temp_sibling(&obj_path);
    if save_obj_triangles(&obj_tmp, mesh).is_err() {
        let _ = std::fs::remove_file(&obj_tmp);
        return;
    }
    let Ok(obj_sha256) = file_sha256(&obj_tmp) else {
        let _ = std::fs::remove_file(&obj_tmp);
        return;
    };
    let Ok(serde_json::Value::Object(mut record)) = serde_json::to_value(key) else {
        let _ = std::fs::remove_file(&obj_tmp);
        return;
    };
    record.insert("obj_sha256".into(), serde_json::Value::String(obj_sha256));
    let Ok(text) = serde_json::to_string_pretty(&record) else {
        let _ = std::fs::remove_file(&obj_tmp);
        return;
    };
    if !rename_into_place(&obj_tmp, &obj_path) {
        return;
    }
    let key_path = dir.join(format!("{stem}.cache.json"));
    let key_tmp = temp_sibling(&key_path);
    if std::fs::write(&key_tmp, text).is_err() {
        let _ = std::fs::remove_file(&key_tmp);
        return;
    }
    rename_into_place(&key_tmp, &key_path);
}

#[allow(clippy::too_many_arguments)]
fn mesh_binary_mask_nifti_with_params_inner(
    src: &LeadRoiSource,
    display_name: &str,
    params: &LeadAtlasMeshParams<'_>,
    cache: Option<&AtlasMeshCacheDirs>,
    template_path: Option<&Path>,
    preblur_iters: u32,
    morph_close_iters: u32,
    weld_eps_mm: f32,
    taubin_iters: u32,
    taubin_lambda: f32,
    taubin_mu: f32,
) -> VizResult<NamedAtlasMesh> {
    let path = src.path.as_path();
    if let Some(cache) = cache {
        let key = AtlasMeshCacheKey {
            version: ATLAS_MESH_CACHE_VERSION,
            src_sha256: file_sha256(path)?,
            template_sha256: template_path.map(file_sha256).transpose()?,
            resample_to_template: params.resample_to_template,
            source_ijk_subtract: params.source_ijk_subtract,
            native_vertex_ijk_shift: params.native_vertex_ijk_shift,
            preblur_iters,
            morph_close_iters,
            weld_eps_mm,
            taubin_iters,
            taubin_lambda,
            taubin_mu,
            threshold: src.threshold,
            split: src.split,
        };
        let stem = sanitize_roi_label_for_filename(display_name);
        let user_stem = user_cache_stem(&stem, &key);
        let hit = cached_mesh_in(&cache.shipped, &stem, &key).or_else(|| {
            cache
                .user
                .as_deref()
                .and_then(|dir| cached_mesh_in(dir, &user_stem, &key))
        });
        if let Some(obj_path) = hit {
            crate::viz_eprintln!(
                "[atlas-cache] hit: {} ({})",
                display_name,
                obj_path.display()
            );
            let mesh = load_obj_triangles(&obj_path)?;
            let h: u32 = display_name.bytes().map(|b| b as u32).sum();
            let color = atlas_label_color(display_name, h);
            return Ok(NamedAtlasMesh {
                name: display_name.to_string(),
                mesh: Some(mesh),
                color,
                source: None,
            });
        }
        crate::viz_eprintln!("[atlas-cache] miss: {}", display_name);
        let computed = mesh_binary_mask_nifti_with_params_inner(
            src,
            display_name,
            params,
            None,
            template_path,
            preblur_iters,
            morph_close_iters,
            weld_eps_mm,
            taubin_iters,
            taubin_lambda,
            taubin_mu,
        )?;
        if let Some(mesh) = computed.mesh.as_ref().filter(|m| !m.positions.is_empty()) {
            let mut mesh = mesh.weld_vertices(weld_eps_mm);
            mesh.taubin_smooth(taubin_iters, taubin_lambda, taubin_mu);
            if let Some(user) = cache.user.as_deref() {
                store_mesh_in(user, &user_stem, &key, &mesh);
            }
            return Ok(NamedAtlasMesh {
                name: computed.name,
                mesh: Some(mesh),
                color: computed.color,
                source: None,
            });
        }
        return Ok(computed);
    }

    let empty = |why: &str| -> VizResult<NamedAtlasMesh> {
        crate::viz_eprintln!("[atlas] {display_name}: {why} ({})", path.display());
        Ok(NamedAtlasMesh {
            name: display_name.to_string(),
            mesh: Some(empty_mesh()),
            color: atlas_label_color(display_name, 0),
            source: None,
        })
    };
    let (src_hdr, n0, n1, n2, mut field) = read_nifti_volume_f32(path)?;
    if let Some(side) = src.split {
        split_hemisphere(&src_hdr, (n0, n1, n2), &mut field, side);
    }
    let Some(level) = prepare_mask_values(&mut field, src.threshold) else {
        return empty("no voxel inside the mask");
    };
    if !field.iter().any(|&v| v > level) {
        return empty("no voxel above the threshold");
    }

    let smooth = params.mesh_smooth_iters;
    let (hdr_mc, dims_mc, vert_shift) = if params.resample_to_template {
        if let Some(t) = params.template {
            field = resample_binary_mask_to_template(
                &src_hdr,
                (n0, n1, n2),
                &field,
                t,
                params.source_ijk_subtract,
            )?;
            if !field.iter().any(|&v| v > level) {
                return empty("no voxel above the threshold after resampling to the template");
            }
            (&t.hdr, (t.n0, t.n1, t.n2), 0.0_f32)
        } else {
            (&src_hdr, (n0, n1, n2), params.native_vertex_ijk_shift)
        }
    } else {
        (&src_hdr, (n0, n1, n2), params.native_vertex_ijk_shift)
    };

    let mesh = mesh_mask_field(
        hdr_mc,
        dims_mc,
        field,
        level,
        morph_close_iters,
        preblur_iters,
        vert_shift,
        smooth,
        true,
    )?;
    let h: u32 = display_name.bytes().map(|b| b as u32).sum();
    let color = atlas_label_color(display_name, h);
    let bbox = mesh.positions.iter().fold(
        ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]),
        |(mn, mx), p| {
            (
                [mn[0].min(p.x), mn[1].min(p.y), mn[2].min(p.z)],
                [mx[0].max(p.x), mx[1].max(p.y), mx[2].max(p.z)],
            )
        },
    );
    crate::viz_eprintln!(
        "[atlas] {}: {} verts, world bbox [{:.1},{:.1},{:.1}]–[{:.1},{:.1},{:.1}]",
        display_name,
        mesh.positions.len(),
        bbox.0[0],
        bbox.0[1],
        bbox.0[2],
        bbox.1[0],
        bbox.1[1],
        bbox.1[2],
    );
    Ok(NamedAtlasMesh {
        name: display_name.to_string(),
        mesh: Some(mesh),
        color,
        source: None,
    })
}

/// Cached variant: loads the OBJ whose key (mask content hash + meshing parameters) matches from
/// the shipped `precomputed_obj/` directory, then from the per-user cache; otherwise meshes the
/// NIfTI, welds + Taubin-smooths and writes OBJ + key JSON into the per-user cache only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mesh_binary_mask_nifti_cached_with_params(
    src: &LeadRoiSource,
    display_name: &str,
    params: &LeadAtlasMeshParams<'_>,
    cache: &AtlasMeshCacheDirs,
    template_path: Option<&Path>,
    preblur_iters: u32,
    morph_close_iters: u32,
    weld_eps_mm: f32,
    taubin_iters: u32,
    taubin_lambda: f32,
    taubin_mu: f32,
) -> VizResult<NamedAtlasMesh> {
    mesh_binary_mask_nifti_with_params_inner(
        src,
        display_name,
        params,
        Some(cache),
        template_path,
        preblur_iters,
        morph_close_iters,
        weld_eps_mm,
        taubin_iters,
        taubin_lambda,
        taubin_mu,
    )
}

fn is_nifti_file(p: &Path) -> bool {
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    if ext == "nii" {
        return true;
    }
    if ext == "gz" {
        return p
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.to_ascii_lowercase().ends_with(".nii"));
    }
    false
}

fn nifti_stem(p: &Path) -> Option<String> {
    let name = p.file_name()?.to_str()?;
    let lower = name.to_ascii_lowercase();
    let cut = if lower.ends_with(".nii.gz") {
        7
    } else if lower.ends_with(".nii") {
        4
    } else {
        return None;
    };
    Some(name[..name.len() - cut].to_string())
}

/// Sorted (byte order) NIfTI file names in `dir`; `*_temp.nii*` scratch files are skipped
/// (`specs/11` §2.2).
pub(crate) fn sorted_nifti_names(dir: &Path) -> VizResult<Vec<String>> {
    let entries = std::fs::read_dir(dir).map_err(|e| VizError::NiftiLoad(e.to_string()))?;
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_nifti_file(p))
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .filter(|n| {
            !nifti_stem(Path::new(n)).is_some_and(|s| s.to_ascii_lowercase().ends_with("_temp"))
        })
        .collect();
    names.sort();
    names.truncate(MAX_LEAD_ATLAS_FILES);
    Ok(names)
}

/// `true` when `root` has any of the Lead-DBS side folders (`specs/11` §2.1).
pub fn has_lead_side_folders(root: &Path) -> bool {
    LEAD_ATLAS_SUBDIRS.iter().any(|s| root.join(s).is_dir())
}

/// Structure name of a side-folder file: the file name without `.nii` / `.nii.gz`.
pub(crate) fn structure_name_of(file: &str) -> String {
    nifti_stem(Path::new(file)).unwrap_or_else(|| file.to_string())
}

/// One structure found by scanning a Lead layout without `atlas_index.mat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedStructure {
    pub file: String,
    /// Atlas type code, `specs/11` §2.1.
    pub kind: usize,
}

/// `specs/11` §2.3: right-only, left-only, bilateral (in `lh/` order), mixed, midline.
pub fn scan_lead_layout(root: &Path) -> VizResult<Vec<ScannedStructure>> {
    if !root.is_dir() {
        return Err(VizError::NiftiLoad(format!(
            "atlas directory not found: {}",
            root.display()
        )));
    }
    let list = |sub: &str| -> VizResult<Vec<String>> {
        let d = root.join(sub);
        if d.is_dir() {
            sorted_nifti_names(&d)
        } else {
            Ok(Vec::new())
        }
    };
    let (lh, rh, mixed, midline) = (list("lh")?, list("rh")?, list("mixed")?, list("midline")?);
    let lh_set: HashSet<&str> = lh.iter().map(String::as_str).collect();
    let rh_set: HashSet<&str> = rh.iter().map(String::as_str).collect();
    let entry = |file: &String, kind: usize| ScannedStructure {
        file: file.clone(),
        kind,
    };
    let mut out: Vec<ScannedStructure> = Vec::new();
    out.extend(
        rh.iter()
            .filter(|f| !lh_set.contains(f.as_str()))
            .map(|f| entry(f, 1)),
    );
    out.extend(
        lh.iter()
            .filter(|f| !rh_set.contains(f.as_str()))
            .map(|f| entry(f, 2)),
    );
    out.extend(
        lh.iter()
            .filter(|f| rh_set.contains(f.as_str()))
            .map(|f| entry(f, 3)),
    );
    out.extend(mixed.iter().map(|f| entry(f, 4)));
    out.extend(midline.iter().map(|f| entry(f, 5)));
    out.truncate(MAX_LEAD_ATLAS_FILES);
    Ok(out)
}

/// Flat atlas folder (`.nii*` directly under `root`, e.g. AHEAD): `(stem, path)` sorted by stem.
pub fn collect_flat_atlas_items(root: &Path) -> VizResult<Vec<(String, PathBuf)>> {
    if !root.is_dir() {
        return Err(VizError::NiftiLoad(format!(
            "atlas directory not found: {}",
            root.display()
        )));
    }
    let mut items: Vec<(String, PathBuf)> = sorted_nifti_names(root)?
        .into_iter()
        .map(|n| (structure_name_of(&n), root.join(n)))
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(items)
}

/// Meshes the stubs where `mesh_idx[i]` is true (parallel); the others keep `mesh: None`.
pub fn mesh_atlas_stubs(
    meshes: &mut [NamedAtlasMesh],
    params: &LeadAtlasMeshParams<'_>,
    mesh_idx: &[bool],
) -> VizResult<()> {
    if mesh_idx.len() != meshes.len() {
        return Err(VizError::NiftiLoad(format!(
            "atlas mesh_idx len {} != items {}",
            mesh_idx.len(),
            meshes.len()
        )));
    }
    let to_load: Vec<(usize, LeadRoiSource, String)> = meshes
        .iter()
        .enumerate()
        .filter(|(i, m)| mesh_idx[*i] && m.mesh.is_none())
        .filter_map(|(i, m)| m.source.clone().map(|s| (i, s, m.name.clone())))
        .collect();
    let run = |(i, src, name): (usize, LeadRoiSource, String)| {
        (i, mesh_binary_mask_nifti_with_params(&src, &name, params))
    };
    #[cfg(not(target_arch = "wasm32"))]
    let done: Vec<(usize, VizResult<NamedAtlasMesh>)> = to_load.into_par_iter().map(run).collect();
    #[cfg(target_arch = "wasm32")]
    let done: Vec<(usize, VizResult<NamedAtlasMesh>)> = to_load.into_iter().map(run).collect();
    for (i, r) in done {
        meshes[i].mesh = r?.mesh;
    }
    Ok(())
}

pub fn meshes_from_label_nifti(
    path: &Path,
    max_labels: Option<usize>,
) -> VizResult<Vec<NamedAtlasMesh>> {
    let (hdr, n0, n1, n2, vol_data) = read_nifti_volume_f32(path)?;

    let mut labels = collect_labels_from_flat(&vol_data, n0, n1, n2);
    let cap = max_labels.unwrap_or(64).min(256);
    if labels.len() > cap {
        labels.truncate(cap);
    }

    #[cfg(not(target_arch = "wasm32"))]
    let out: VizResult<Vec<NamedAtlasMesh>> = labels
        .par_iter()
        .map(|&lab| {
            let mesh = mesh_one_label(&hdr, (n0, n1, n2), &vol_data, lab)?;
            let name = format!("label_{lab}");
            let color = atlas_label_color(&name, lab);
            Ok(NamedAtlasMesh {
                name,
                mesh: Some(mesh),
                color,
                source: None,
            })
        })
        .collect();
    #[cfg(target_arch = "wasm32")]
    let out: VizResult<Vec<NamedAtlasMesh>> = labels
        .iter()
        .map(|&lab| {
            let mesh = mesh_one_label(&hdr, (n0, n1, n2), &vol_data, lab)?;
            let name = format!("label_{lab}");
            let color = atlas_label_color(&name, lab);
            Ok(NamedAtlasMesh {
                name,
                mesh: Some(mesh),
                color,
                source: None,
            })
        })
        .collect();
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn ball_mask(dir: &Path) -> PathBuf {
        let n = 16usize;
        let c = (n as f32 - 1.0) / 2.0;
        let voxels: Vec<f32> = (0..n * n * n)
            .map(|i| {
                let (x, y, z) = (i % n, (i / n) % n, i / (n * n));
                let d2 = (x as f32 - c).powi(2) + (y as f32 - c).powi(2) + (z as f32 - c).powi(2);
                if d2 <= 25.0 {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        let path = dir.join("ball_mask.nii");
        std::fs::write(
            &path,
            dbsloc_checkreg::nifti_io::make_nifti1_bytes(n, n, n, &voxels),
        )
        .unwrap();
        path
    }

    fn params() -> LeadAtlasMeshParams<'static> {
        LeadAtlasMeshParams {
            template: None,
            resample_to_template: false,
            mesh_smooth_iters: 0,
            source_ijk_subtract: 0.0,
            native_vertex_ijk_shift: 0.0,
        }
    }

    fn mesh_with(mask: &Path, cache: &AtlasMeshCacheDirs) -> NamedAtlasMesh {
        mesh_binary_mask_nifti_cached_with_params(
            &LeadRoiSource::whole(mask.to_path_buf()),
            "STN left",
            &params(),
            cache,
            None,
            6,
            0,
            0.05,
            10,
            0.5,
            -0.53,
        )
        .expect("mesh")
    }

    fn only_file(dir: &Path, suffix: &str) -> PathBuf {
        let found: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(suffix) && !n.starts_with('.'))
            })
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        found.into_iter().next().unwrap()
    }

    #[test]
    fn a_mismatched_or_partial_user_cache_pair_is_not_accepted() {
        let tmp = std::env::temp_dir().join(format!("dbsloc_atlas_pair_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mask = ball_mask(&tmp);
        let user = tmp.join("user");
        let cache = AtlasMeshCacheDirs {
            shipped: tmp.join("shipped"),
            user: Some(user.clone()),
        };
        let _ = mesh_with(&mask, &cache);
        let obj = only_file(&user, ".obj");
        let stem = obj.file_stem().unwrap().to_string_lossy().into_owned();
        let key: AtlasMeshCacheKey = serde_json::from_str(
            &std::fs::read_to_string(only_file(&user, ".cache.json")).unwrap(),
        )
        .unwrap();
        assert!(cached_mesh_in(&user, &stem, &key).is_some());
        std::fs::write(&obj, "v 0 0 0\n").unwrap();
        assert!(cached_mesh_in(&user, &stem, &key).is_none());
        let other = AtlasMeshCacheKey {
            taubin_iters: 3,
            ..key.clone()
        };
        assert_ne!(
            user_cache_stem("STN_left", &key),
            user_cache_stem("STN_left", &other)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn ball_with_affine(dir: &Path, srows: [[f32; 4]; 3]) -> (PathBuf, NiftiHeader, Vec3) {
        let n = 24usize;
        let c = [11.0_f32, 12.0, 10.0];
        let voxels: Vec<f32> = (0..n * n * n)
            .map(|i| {
                let (x, y, z) = (i % n, (i / n) % n, i / (n * n));
                let d2 = (x as f32 - c[0]).powi(2)
                    + (y as f32 - c[1]).powi(2)
                    + (z as f32 - c[2]).powi(2);
                if d2 <= 25.0 {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        let mut bytes = dbsloc_checkreg::nifti_io::make_nifti1_bytes(n, n, n, &voxels);
        for (r, row) in srows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let off = 280 + r * 16 + c * 4;
                bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        let path = dir.join("ball_affine.nii");
        std::fs::write(&path, bytes).unwrap();
        let hdr = NiftiHeader::from_file(&path).unwrap();
        let to_world = VoxelWorld::from_header(&hdr);
        let (mut sum, mut count) = (Vec3::ZERO, 0.0_f32);
        for (i, v) in voxels.iter().enumerate() {
            if *v > 0.5 {
                sum += to_world.apply((i % n) as f32, ((i / n) % n) as f32, (i / (n * n)) as f32);
                count += 1.0;
            }
        }
        (path, hdr, sum / count)
    }

    fn vertex_mean(m: &NamedAtlasMesh) -> Vec3 {
        let mesh = m.mesh.as_ref().unwrap();
        mesh.positions.iter().copied().sum::<Vec3>() / mesh.positions.len() as f32
    }

    #[test]
    fn meshes_sit_on_the_mask_in_world_space() {
        let tmp = std::env::temp_dir().join(format!("dbsloc_atlas_world_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let (path, hdr, centroid) = ball_with_affine(
            &tmp,
            [
                [-0.5, 0.0, 0.0, 30.0],
                [0.0, 0.5, 0.1, -20.0],
                [0.0, 0.0, 0.7, -12.0],
            ],
        );
        let native = mesh_binary_mask_nifti_with_params(
            &LeadRoiSource::whole(path.clone()),
            "ball",
            &params(),
        )
        .unwrap();
        let d = vertex_mean(&native) - centroid;
        assert!(d.length() < 0.01, "native mesh off the mask by {d:?}");

        let same_grid = TemplateVolume {
            n0: 24,
            n1: 24,
            n2: 24,
            hdr: hdr.clone(),
            normed: vec![0.0; 24 * 24 * 24],
        };
        let resampled = mesh_binary_mask_nifti_with_params(
            &LeadRoiSource::whole(path.clone()),
            "ball",
            &LeadAtlasMeshParams {
                template: Some(&same_grid),
                resample_to_template: true,
                ..params()
            },
        )
        .unwrap();
        let (a, b) = (
            native.mesh.as_ref().unwrap(),
            resampled.mesh.as_ref().unwrap(),
        );
        assert_eq!(a.positions.len(), b.positions.len());
        let worst = a
            .positions
            .iter()
            .zip(&b.positions)
            .map(|(p, q)| (*p - *q).length())
            .fold(0.0_f32, f32::max);
        assert!(
            worst < 1e-4,
            "resample onto the same grid moved the mesh by {worst} mm"
        );

        let other_grid = TemplateVolume {
            n0: 48,
            n1: 48,
            n2: 48,
            hdr: NiftiHeader {
                sform_code: 1,
                srow_x: [0.5, 0.0, 0.0, 12.15],
                srow_y: [0.0, 0.5, 0.0, -20.35],
                srow_z: [0.0, 0.0, 0.5, -12.4],
                ..Default::default()
            },
            normed: vec![0.0; 48 * 48 * 48],
        };
        let moved = mesh_binary_mask_nifti_with_params(
            &LeadRoiSource::whole(path.clone()),
            "ball",
            &LeadAtlasMeshParams {
                template: Some(&other_grid),
                resample_to_template: true,
                ..params()
            },
        )
        .unwrap();
        let d = vertex_mean(&moved) - centroid;
        assert!(d.length() < 0.1, "resampled mesh off the mask by {d:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn ahead_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/atlases/ahead/structures_mni09b")
    }

    fn default_manifest() -> crate::scene_manifest::SceneManifestV1 {
        serde_json::from_str(
            r#"{ "subj_id": "x", "template_nii": "t.nii", "reco_json": "r.json" }"#,
        )
        .unwrap()
    }

    fn shipped_key(mask: &Path) -> AtlasMeshCacheKey {
        let m = default_manifest();
        AtlasMeshCacheKey {
            version: ATLAS_MESH_CACHE_VERSION,
            src_sha256: file_sha256(mask).unwrap(),
            template_sha256: None,
            resample_to_template: m.atlas_mesh_resample_to_template,
            source_ijk_subtract: m.atlas_lead_resample_ijk_subtract,
            native_vertex_ijk_shift: m.atlas_lead_native_mc_ijk_shift,
            preblur_iters: m.atlas_mesh_preblur_iters,
            morph_close_iters: m.atlas_mesh_morph_close_iters,
            weld_eps_mm: m.atlas_mesh_weld_eps_mm,
            taubin_iters: m.atlas_mesh_taubin_iters,
            taubin_lambda: m.atlas_mesh_taubin_lambda,
            taubin_mu: m.atlas_mesh_taubin_mu,
            threshold: None,
            split: None,
        }
    }

    fn ahead_masks() -> Vec<(String, PathBuf)> {
        let dir = ahead_dir();
        if !dir.is_dir() {
            return Vec::new();
        }
        collect_flat_atlas_items(&dir).unwrap()
    }

    #[test]
    fn shipped_ahead_meshes_match_the_current_cache_key() {
        let shipped = ahead_dir().join("precomputed_obj");
        for (name, mask) in ahead_masks() {
            let stem = sanitize_roi_label_for_filename(&name);
            assert!(
                cached_mesh_in(&shipped, &stem, &shipped_key(&mask)).is_some(),
                "{stem}: shipped mesh is stale; run the ignored test regenerate_shipped_ahead_meshes with DBSLOC_REGENERATE_AHEAD_MESHES=1"
            );
        }
    }

    #[test]
    #[ignore = "rewrites resources/; set DBSLOC_REGENERATE_AHEAD_MESHES=1"]
    fn regenerate_shipped_ahead_meshes() {
        if std::env::var_os("DBSLOC_REGENERATE_AHEAD_MESHES").is_none() {
            return;
        }
        let shipped = ahead_dir().join("precomputed_obj");
        let m = default_manifest();
        let params = LeadAtlasMeshParams {
            template: None,
            resample_to_template: m.atlas_mesh_resample_to_template,
            mesh_smooth_iters: m.atlas_mesh_smooth_iters,
            source_ijk_subtract: m.atlas_lead_resample_ijk_subtract,
            native_vertex_ijk_shift: m.atlas_lead_native_mc_ijk_shift,
        };
        for (name, mask) in ahead_masks() {
            let stem = sanitize_roi_label_for_filename(&name);
            let work = std::env::temp_dir()
                .join(format!("dbsloc_regen_ahead_{}_{stem}", std::process::id()));
            let _ = std::fs::remove_dir_all(&work);
            let cache = AtlasMeshCacheDirs {
                shipped: work.join("none"),
                user: Some(work.join("out")),
            };
            mesh_binary_mask_nifti_cached_with_params(
                &LeadRoiSource::whole(mask.clone()),
                &name,
                &params,
                &cache,
                None,
                m.atlas_mesh_preblur_iters,
                m.atlas_mesh_morph_close_iters,
                m.atlas_mesh_weld_eps_mm,
                m.atlas_mesh_taubin_iters,
                m.atlas_mesh_taubin_lambda,
                m.atlas_mesh_taubin_mu,
            )
            .unwrap();
            let key = shipped_key(&mask);
            let out_stem = user_cache_stem(&stem, &key);
            let out = work.join("out");
            std::fs::copy(
                out.join(format!("{out_stem}.obj")),
                shipped.join(format!("{stem}.obj")),
            )
            .unwrap();
            let text = serde_json::to_string_pretty(&key).unwrap();
            std::fs::write(shipped.join(format!("{stem}.cache.json")), text).unwrap();
            let _ = std::fs::remove_dir_all(&work);
        }
    }

    #[test]
    fn binary_and_graded_masks_get_their_iso_level() {
        let mut binary = vec![0.0, 0.9993, f32::NAN, 0.9993, f32::INFINITY];
        let level = prepare_mask_values(&mut binary, None).unwrap();
        assert!((level - 0.49965).abs() < 1e-6);
        assert_eq!(binary, vec![0.0, 0.9993, 0.0, 0.9993, 0.0]);
        let mut graded = vec![0.0, 0.2, 0.6, 1.0];
        assert_eq!(prepare_mask_values(&mut graded, None), Some(0.5));
        let want = [0.0, 0.0, 0.4, 0.8];
        assert!(graded.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-6));
        let mut stored = vec![0.0, 0.5, 1.0];
        assert_eq!(prepare_mask_values(&mut stored, Some(0.4)), Some(0.4));
        assert_eq!(prepare_mask_values(&mut [0.0, f32::NAN], None), None);
    }

    #[test]
    fn a_mixed_file_splits_at_world_x_zero() {
        let hdr = NiftiHeader {
            sform_code: 1,
            srow_x: [1.0, 0.0, 0.0, -2.0],
            srow_y: [0.0, 1.0, 0.0, 0.0],
            srow_z: [0.0, 0.0, 1.0, 0.0],
            ..Default::default()
        };
        let dims = (5, 2, 1);
        let x_of = |i: usize| i as f32 - 2.0;
        for keep in [HemisphereSplit::Right, HemisphereSplit::Left] {
            let mut data = vec![1.0_f32; 10];
            split_hemisphere(&hdr, dims, &mut data, keep);
            for (n, v) in data.iter().enumerate() {
                let x = x_of(n % 5);
                let kept = match keep {
                    HemisphereSplit::Right => x >= 0.0,
                    HemisphereSplit::Left => x <= 0.0,
                };
                assert_eq!(*v == 1.0, kept, "{keep:?} voxel x = {x}");
            }
        }
    }

    #[test]
    fn a_structure_touching_the_volume_boundary_is_closed() {
        let n = 6usize;
        let field = vec![1.0_f32; n * n * n];
        let hdr = NiftiHeader {
            sform_code: 1,
            srow_x: [1.0, 0.0, 0.0, 0.0],
            srow_y: [0.0, 1.0, 0.0, 0.0],
            srow_z: [0.0, 0.0, 1.0, 0.0],
            ..Default::default()
        };
        let mesh = mesh_mask_field(&hdr, (n, n, n), field, 0.5, 0, 0, 0.0, 0, true).unwrap();
        let (mn, mx) = mesh.positions.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(a, b), p| (a.min(*p), b.max(*p)),
        );
        assert!((mn - Vec3::splat(-0.5)).length() < 1e-4, "{mn:?}");
        assert!((mx - Vec3::splat(5.5)).length() < 1e-4, "{mx:?}");
    }

    #[test]
    fn cropped_meshing_matches_the_full_grid() {
        let (nx, ny, nz) = (40usize, 36usize, 30usize);
        let mut field = vec![0.0_f32; nx * ny * nz];
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let d2 = (i as f32 - 27.0).powi(2)
                        + (j as f32 - 11.0).powi(2) * 1.4
                        + (k as f32 - 9.0).powi(2);
                    if d2 <= 20.0 || (i == 31 && j == 11 && k == 9) {
                        field[(k * ny + j) * nx + i] = 1.0;
                    }
                }
            }
        }
        let hdr = NiftiHeader {
            sform_code: 1,
            srow_x: [-0.5, 0.0, 0.0, 30.0],
            srow_y: [0.0, 0.5, 0.0, -20.0],
            srow_z: [0.0, 0.0, 0.5, -10.0],
            ..Default::default()
        };
        let full =
            mesh_mask_field(&hdr, (nx, ny, nz), field.clone(), 0.5, 1, 3, 1.0, 0, false).unwrap();
        let cropped = mesh_mask_field(&hdr, (nx, ny, nz), field, 0.5, 1, 3, 1.0, 0, true).unwrap();
        assert!(!full.positions.is_empty());
        assert_eq!(full.positions.len(), cropped.positions.len());
        assert_eq!(full.indices.len(), cropped.indices.len());
        let key = |p: &Vec3| [p.x, p.y, p.z].map(|v| (v * 1000.0).round() as i64);
        let mut a: Vec<_> = full.positions.iter().map(key).collect();
        let mut b: Vec<_> = cropped.positions.iter().map(key).collect();
        a.sort_unstable();
        b.sort_unstable();
        let worst = a
            .iter()
            .zip(&b)
            .flat_map(|(x, y)| (0..3).map(move |c| (x[c] - y[c]).abs()))
            .max()
            .unwrap_or(0);
        assert!(worst <= 1, "max vertex difference {worst} µm");
        let normals_ok = full
            .normals
            .iter()
            .zip(&cropped.normals)
            .all(|(x, y)| x.is_finite() && y.is_finite());
        assert!(normals_ok);
    }

    #[test]
    fn user_cache_dir_follows_the_platform_convention() {
        let env = |name: &str| match name {
            "LOCALAPPDATA" => Some(PathBuf::from("C:/Users/x/AppData/Local")),
            "HOME" => Some(PathBuf::from("/home/x")),
            "XDG_CACHE_HOME" => Some(PathBuf::from("/xdg")),
            _ => None,
        };
        let dir = user_mesh_cache_dir_from(env).unwrap();
        let text = dir.to_string_lossy().replace('\\', "/");
        assert!(text.ends_with("dbsloc/cache/atlas_meshes"), "{text}");
        if cfg!(target_os = "windows") {
            assert!(text.starts_with("C:/Users/x/AppData/Local"), "{text}");
        } else if cfg!(target_os = "macos") {
            assert!(text.starts_with("/home/x/Library/Caches"), "{text}");
        } else {
            assert!(text.starts_with("/xdg"), "{text}");
        }
        let no_xdg = |name: &str| match name {
            "HOME" => Some(PathBuf::from("/home/y")),
            "LOCALAPPDATA" => Some(PathBuf::from("")),
            _ => None,
        };
        let dir = user_mesh_cache_dir_from(no_xdg);
        if cfg!(target_os = "windows") {
            assert!(dir.is_none());
        } else {
            assert!(dir.unwrap().to_string_lossy().contains("/home/y"));
        }
    }

    #[test]
    fn shipped_meshes_hit_by_content_hash_and_writes_go_to_the_user_cache_only() {
        let tmp = std::env::temp_dir().join(format!("dbsloc_atlas_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mask = ball_mask(&tmp);
        let shipped = tmp.join("shipped");
        let user = tmp.join("user");
        std::fs::create_dir_all(&shipped).unwrap();
        let cache = AtlasMeshCacheDirs {
            shipped: shipped.clone(),
            user: Some(user.clone()),
        };
        let first = mesh_with(&mask, &cache);
        let obj = only_file(&user, ".obj");
        let key_path = only_file(&user, ".cache.json");
        let obj_name = obj.file_name().unwrap().to_string_lossy().into_owned();
        assert!(obj_name.starts_with("STN_left-"), "{obj_name}");
        assert_eq!(
            key_path.file_name().unwrap().to_string_lossy(),
            obj_name.replace(".obj", ".cache.json")
        );
        assert!(std::fs::read_dir(&shipped).unwrap().next().is_none());
        let key: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&key_path).unwrap()).unwrap();
        assert_eq!(key["version"], serde_json::json!(ATLAS_MESH_CACHE_VERSION));
        assert_eq!(
            key["src_sha256"].as_str().unwrap(),
            file_sha256(&mask).unwrap()
        );
        assert!(key.get("path").is_none() && key.get("src").is_none());
        assert_eq!(
            key["obj_sha256"].as_str().unwrap(),
            file_sha256(&obj).unwrap()
        );

        std::fs::rename(&obj, shipped.join("STN_left.obj")).unwrap();
        std::fs::rename(&key_path, shipped.join("STN_left.cache.json")).unwrap();
        std::fs::remove_dir_all(&user).unwrap();
        let shipped_only = AtlasMeshCacheDirs {
            shipped: shipped.clone(),
            user: Some(user.clone()),
        };
        let second = mesh_with(&mask, &shipped_only);
        assert!(!user.exists(), "a cache hit writes nothing");
        assert_eq!(
            first.mesh.as_ref().unwrap().positions.len(),
            second.mesh.as_ref().unwrap().positions.len()
        );

        let stale = std::fs::read_to_string(shipped.join("STN_left.cache.json"))
            .unwrap()
            .replace("\"taubin_iters\": 10", "\"taubin_iters\": 3");
        std::fs::write(shipped.join("STN_left.cache.json"), stale).unwrap();
        let _ = mesh_with(&mask, &shipped_only);
        assert!(
            only_file(&user, ".obj").is_file(),
            "a parameter change misses the shipped copy and fills the user cache"
        );
        let no_user = AtlasMeshCacheDirs {
            shipped: tmp.join("missing"),
            user: None,
        };
        let third = mesh_with(&mask, &no_user);
        assert!(third.mesh.is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
