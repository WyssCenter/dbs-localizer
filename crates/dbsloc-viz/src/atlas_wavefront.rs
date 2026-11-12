// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Minimal Wavefront `.obj` loader (triangles only). Clean-room.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::fs::read_to_string;
use std::path::Path;

use glam::{Vec3, Vec4};

use crate::error::{VizError, VizResult};

#[derive(Debug, Clone)]
pub struct TriMeshScene {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

impl TriMeshScene {
    fn neighbors_from_indices(n: usize, indices: &[u32]) -> Vec<Vec<usize>> {
        let mut neighbors: Vec<HashSet<usize>> = (0..n).map(|_| HashSet::new()).collect();
        for tri in indices.chunks(3) {
            if tri.len() != 3 {
                continue;
            }
            let a = tri[0] as usize;
            let b = tri[1] as usize;
            let c = tri[2] as usize;
            if a >= n || b >= n || c >= n {
                continue;
            }
            neighbors[a].insert(b);
            neighbors[a].insert(c);
            neighbors[b].insert(a);
            neighbors[b].insert(c);
            neighbors[c].insert(a);
            neighbors[c].insert(b);
        }
        neighbors
            .into_iter()
            .map(|hs| hs.into_iter().collect())
            .collect()
    }

    /// Per-vertex normals: average face normals (area-weighted by triangle cross).
    pub fn recompute_vertex_normals(&mut self) {
        let n = self.positions.len();
        if n == 0 || self.indices.len() < 3 {
            return;
        }
        let mut accum: Vec<Vec4> = vec![Vec4::ZERO; n];
        for tri in self.indices.chunks(3) {
            let ia = tri[0] as usize;
            let ib = tri[1] as usize;
            let ic = tri[2] as usize;
            if ia >= n || ib >= n || ic >= n {
                continue;
            }
            let p0 = self.positions[ia];
            let p1 = self.positions[ib];
            let p2 = self.positions[ic];
            let nn = (p1 - p0).cross(p2 - p0);
            for &ix in &[ia, ib, ic] {
                accum[ix] += Vec4::new(nn.x, nn.y, nn.z, 1.0);
            }
        }
        self.normals = accum
            .into_iter()
            .map(|a| {
                let w = a.w.max(1.0);
                Vec3::new(a.x / w, a.y / w, a.z / w).normalize_or_zero()
            })
            .collect();
    }

    /// Simple umbrella Laplacian; **lambda** in (0,1]. Recompute normals after.
    pub fn laplacian_smooth(&mut self, iterations: u32, lambda: f32) {
        if iterations == 0 || lambda <= 0.0 {
            return;
        }
        let n = self.positions.len();
        if n == 0 {
            return;
        }
        let neighbors = Self::neighbors_from_indices(n, &self.indices);
        let lam = lambda.min(1.0);
        for _ in 0..iterations {
            let old = self.positions.clone();
            for i in 0..n {
                let nb = &neighbors[i];
                if nb.is_empty() || nb.len() == 1 {
                    continue;
                }
                let mut avg = Vec3::ZERO;
                for &j in nb {
                    avg += old[j];
                }
                avg /= nb.len() as f32;
                self.positions[i] = old[i] * (1.0 - lam) + avg * lam;
            }
        }
        self.recompute_vertex_normals();
    }

    /// Weld near-duplicate vertices by quantizing positions with `eps_mm` grid.
    /// Returns new indexed mesh. Source normals (e.g. mcubes gradient normals) are
    /// accumulated and averaged per merged vertex — NOT recomputed from triangles —
    /// so smooth gradient normals survive the weld step.
    pub fn weld_vertices(&self, eps_mm: f32) -> Self {
        let eps = eps_mm.max(1e-6);
        let inv = 1.0_f32 / eps;
        let mut map: HashMap<(i32, i32, i32), u32> = HashMap::new();
        let mut new_pos: Vec<Vec3> = Vec::new();
        let mut new_indices: Vec<u32> = Vec::with_capacity(self.indices.len());
        let mut normal_accum: Vec<Vec4> = Vec::new();
        let have_normals = self.normals.len() == self.positions.len();

        let n = self.positions.len();
        for &ix in &self.indices {
            let i = ix as usize;
            if i >= n {
                continue;
            }
            let p = self.positions[i];
            let key = (
                (p.x * inv).round() as i32,
                (p.y * inv).round() as i32,
                (p.z * inv).round() as i32,
            );
            let out_ix = if let Some(&j) = map.get(&key) {
                j
            } else {
                let j = new_pos.len() as u32;
                new_pos.push(p);
                normal_accum.push(Vec4::ZERO);
                map.insert(key, j);
                j
            };
            if have_normals {
                let nm = self.normals[i];
                normal_accum[out_ix as usize] += Vec4::new(nm.x, nm.y, nm.z, 1.0);
            }
            new_indices.push(out_ix);
        }

        let normals: Vec<Vec3> = if have_normals {
            normal_accum
                .into_iter()
                .map(|a| {
                    let w = a.w.max(1.0);
                    Vec3::new(a.x / w, a.y / w, a.z / w).normalize_or_zero()
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut out = TriMeshScene {
            positions: new_pos,
            normals,
            indices: new_indices,
        };
        if !have_normals {
            out.recompute_vertex_normals();
        }
        out
    }

    /// Taubin smoothing (two-step λ/μ). Keeps volume better than plain Laplacian.
    pub fn taubin_smooth(&mut self, iters: u32, lambda: f32, mu: f32) {
        if iters == 0 {
            return;
        }
        let n = self.positions.len();
        if n == 0 || self.indices.len() < 3 {
            return;
        }
        let neighbors = Self::neighbors_from_indices(n, &self.indices);

        let mut lap = vec![Vec3::ZERO; n];
        for _ in 0..iters {
            let old = self.positions.clone();
            for i in 0..n {
                let nb = &neighbors[i];
                if nb.is_empty() || nb.len() == 1 {
                    lap[i] = Vec3::ZERO;
                    continue;
                }
                let mut avg = Vec3::ZERO;
                for &j in nb {
                    avg += old[j];
                }
                avg /= nb.len() as f32;
                lap[i] = avg - old[i];
            }
            for i in 0..n {
                self.positions[i] = old[i] + lap[i] * lambda;
            }

            let mid = self.positions.clone();
            for i in 0..n {
                let nb = &neighbors[i];
                if nb.is_empty() || nb.len() == 1 {
                    lap[i] = Vec3::ZERO;
                    continue;
                }
                let mut avg = Vec3::ZERO;
                for &j in nb {
                    avg += mid[j];
                }
                avg /= nb.len() as f32;
                lap[i] = avg - mid[i];
            }
            for i in 0..n {
                self.positions[i] = mid[i] + lap[i] * mu;
            }
        }

        let have_normals = self.normals.len() == n;
        if have_normals {
            let old_n = self.normals.clone();
            for i in 0..n {
                let nb = &neighbors[i];
                if nb.is_empty() {
                    continue;
                }
                let mut avg = Vec3::ZERO;
                for &j in nb {
                    avg += old_n[j];
                }
                avg /= nb.len() as f32;
                self.normals[i] = (old_n[i] * 0.5 + avg * 0.5).normalize_or_zero();
            }
        } else {
            self.recompute_vertex_normals();
        }
    }
}

fn parse_face_vertex(tok: &str) -> VizResult<usize> {
    let i = tok
        .split('/')
        .next()
        .ok_or_else(|| VizError::NiftiLoad("empty face token".into()))?;
    let vi: i32 = i
        .parse()
        .map_err(|_| VizError::NiftiLoad(format!("face idx {tok}")))?;
    if vi <= 0 {
        return Err(VizError::NiftiLoad(format!(
            "OBJ face index must be positive, got {vi}"
        )));
    }
    Ok((vi - 1) as usize)
}

/// Load triangle mesh; computes flat normals if file has no `vn` directives.
pub fn load_obj_triangles(path: &Path) -> VizResult<TriMeshScene> {
    let s = read_to_string(path)
        .map_err(|e| VizError::NiftiLoad(format!("obj {}: {e}", path.display())))?;
    parse_obj_triangles(&s).map_err(|e| VizError::NiftiLoad(format!("obj {}: {e}", path.display())))
}

impl TriMeshScene {
    /// Parse a Wavefront `.obj` **string** (no filesystem) — the byte/string entry point for the
    /// browser viewer (`docs/SERVER_CLIENT_PLAN.md` §6.1). Same triangle-only rules as
    /// [`load_obj_triangles`]; computes flat normals when the source has no `vn` directives.
    pub fn parse_str(s: &str) -> VizResult<Self> {
        parse_obj_triangles(s)
    }
}

impl std::str::FromStr for TriMeshScene {
    type Err = VizError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_obj_triangles(s)
    }
}

/// Parse a Wavefront `.obj` string into a triangle mesh (no I/O). Triangle faces only; computes
/// per-vertex normals when the source carries no `vn` directives.
pub fn parse_obj_triangles(s: &str) -> VizResult<TriMeshScene> {
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();

    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(tag) = parts.next() else { continue };
        match tag {
            "v" => {
                let x: f32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| VizError::NiftiLoad("obj v".into()))?;
                let y: f32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| VizError::NiftiLoad("obj v".into()))?;
                let z: f32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| VizError::NiftiLoad("obj v".into()))?;
                positions.push(Vec3::new(x, y, z));
            }
            "vn" => {
                let x: f32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| VizError::NiftiLoad("obj vn".into()))?;
                let y: f32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| VizError::NiftiLoad("obj vn".into()))?;
                let z: f32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| VizError::NiftiLoad("obj vn".into()))?;
                normals.push(Vec3::new(x, y, z).normalize_or_zero());
            }
            "f" => {
                let a = parse_face_vertex(
                    parts
                        .next()
                        .ok_or_else(|| VizError::NiftiLoad("obj f".into()))?,
                )?;
                let b = parse_face_vertex(
                    parts
                        .next()
                        .ok_or_else(|| VizError::NiftiLoad("obj f".into()))?,
                )?;
                let c = parse_face_vertex(
                    parts
                        .next()
                        .ok_or_else(|| VizError::NiftiLoad("obj f".into()))?,
                )?;
                if parts.next().is_some() {
                    return Err(VizError::NiftiLoad(
                        "only triangular faces supported".into(),
                    ));
                }
                indices.extend([a as u32, b as u32, c as u32]);
            }
            _ => {}
        }
    }

    if positions.is_empty() || indices.is_empty() {
        return Err(VizError::NiftiLoad("empty mesh".into()));
    }
    if let Some(bad) = indices.iter().find(|&&i| i as usize >= positions.len()) {
        return Err(VizError::NiftiLoad(format!(
            "OBJ face index {} out of range ({} vertices)",
            *bad as usize + 1,
            positions.len()
        )));
    }

    if normals.len() != positions.len() {
        let mut accum: Vec<Vec4> = vec![Vec4::ZERO; positions.len()];
        for tri in indices.chunks(3) {
            let ia = tri[0] as usize;
            let ib = tri[1] as usize;
            let ic = tri[2] as usize;
            let p0 = positions[ia];
            let p1 = positions[ib];
            let p2 = positions[ic];
            let n = (p1 - p0).cross(p2 - p0).normalize_or_zero();
            for &ix in &[ia, ib, ic] {
                accum[ix] += Vec4::new(n.x, n.y, n.z, 1.0);
            }
        }
        normals = accum
            .into_iter()
            .map(|a| {
                let w = a.w.max(1.0);
                Vec3::new(a.x / w, a.y / w, a.z / w).normalize_or_zero()
            })
            .collect();
    }

    Ok(TriMeshScene {
        positions,
        normals,
        indices,
    })
}

/// Save triangle mesh as Wavefront OBJ (ASCII). Uses 1-based indices.
pub fn save_obj_triangles(path: &Path, mesh: &TriMeshScene) -> VizResult<()> {
    if mesh.positions.is_empty() || mesh.indices.is_empty() {
        return Err(VizError::NiftiLoad(format!(
            "cannot write empty mesh: {}",
            path.display()
        )));
    }
    let Some(parent) = path.parent() else {
        return Err(VizError::NiftiLoad(format!(
            "obj path has no parent: {}",
            path.display()
        )));
    };
    fs::create_dir_all(parent)
        .map_err(|e| VizError::NiftiLoad(format!("mkdir {}: {e}", parent.display())))?;

    let have_normals = mesh.normals.len() == mesh.positions.len();
    let mut out = String::new();
    out.push_str("# dbsloc-viz cached atlas mesh\n");
    for p in &mesh.positions {
        out.push_str(&format!("v {} {} {}\n", p.x, p.y, p.z));
    }
    if have_normals {
        for n in &mesh.normals {
            out.push_str(&format!("vn {} {} {}\n", n.x, n.y, n.z));
        }
    }
    for tri in mesh.indices.chunks(3) {
        if tri.len() != 3 {
            continue;
        }
        let a = tri[0] as usize + 1;
        let b = tri[1] as usize + 1;
        let c = tri[2] as usize + 1;
        if have_normals {
            out.push_str(&format!("f {a}//{a} {b}//{b} {c}//{c}\n"));
        } else {
            out.push_str(&format!("f {a} {b} {c}\n"));
        }
    }
    fs::write(path, out)
        .map_err(|e| VizError::NiftiLoad(format!("obj write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn loads_triangle() {
        let dir = std::env::temp_dir().join("dbsloc_obj_test.obj");
        fs::write(&dir, "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
        let m = load_obj_triangles(&dir).unwrap();
        assert_eq!(m.positions.len(), 3);
        assert_eq!(m.indices.len(), 3);
        let _ = fs::remove_file(&dir);
    }

    #[test]
    fn parses_from_str_without_io() {
        let text = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let m = TriMeshScene::parse_str(text).expect("from_str");
        assert_eq!(m.positions.len(), 3);
        assert_eq!(m.indices, vec![0, 1, 2]);
        assert_eq!(m.normals.len(), 3, "flat normals computed without vn");
        assert!(parse_obj_triangles("").is_err(), "empty mesh is an error");
    }

    #[test]
    fn rejects_face_indices_past_the_vertex_list() {
        let text = "v 0 0 0
v 1 0 0
v 0 1 0
f 1 2 4
";
        assert!(parse_obj_triangles(text).is_err());
        let late = "f 1 2 3
v 0 0 0
v 1 0 0
v 0 1 0
";
        assert!(parse_obj_triangles(late).is_ok());
    }

    #[test]
    fn weld_reduces_vertices_and_roundtrips_obj() {
        let m = TriMeshScene {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            normals: vec![],
            indices: vec![0, 1, 2, 3, 4, 5],
        };
        let w = m.weld_vertices(1e-3);
        assert_eq!(w.positions.len(), 3);
        assert_eq!(w.indices.len(), 6);

        let p = std::env::temp_dir().join("dbsloc_obj_weld_rt.obj");
        save_obj_triangles(&p, &w).unwrap();
        let rt = load_obj_triangles(&p).unwrap();
        assert_eq!(rt.indices.len(), w.indices.len());
        assert_eq!(rt.positions.len(), w.positions.len());
        let _ = fs::remove_file(&p);
    }
}
