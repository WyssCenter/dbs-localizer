// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Per-lead basis cache (`specs/07` §3.6): the per-contact unit-current solutions and the
//! transfer-impedance matrix, stored under `headmodel/native/` with the key they depend on.

use std::path::{Path, PathBuf};

use dbsloc_vat::{Basis, SolveStats};
use serde_json::{json, Value};

use crate::outputs::write_replacing;
use crate::sha256::sha256_hex;
use crate::vat::settings::Side;

/// Cached solutions of one lead.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub solutions: Vec<Option<Vec<f64>>>,
    pub z_kohm: Vec<Vec<f64>>,
    pub stats: Vec<SolveStats>,
    pub built_by: Option<String>,
    pub adapter: Option<String>,
}

/// The two files of one cache entry.
#[derive(Debug, Clone)]
pub struct CachePaths {
    pub json: PathBuf,
    pub bin: PathBuf,
}

impl CachePaths {
    /// `<subject folder>/headmodel/native/sub-<id>_hemi-<R|L>_desc-efieldbasis.{json,bin}`.
    pub fn new(subject_dir: &Path, subject: &str, side: Side) -> CachePaths {
        let dir = subject_dir.join("headmodel").join("native");
        let stem = format!("sub-{subject}_hemi-{}_desc-efieldbasis", side.letter());
        CachePaths {
            json: dir.join(format!("{stem}.json")),
            bin: dir.join(format!("{stem}.bin")),
        }
    }
}

/// Hex SHA-256 of a cache key.
pub fn key_sha256(key: &Value) -> String {
    sha256_hex(key.to_string().as_bytes())
}

fn read_json(paths: &CachePaths) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(&paths.json).ok()?).ok()
}

/// Whether the stored entry carries `key_sha`.
pub fn matches(paths: &CachePaths, key_sha: &str) -> bool {
    read_json(paths)
        .and_then(|v| {
            v.get("key_sha256")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|s| s == key_sha)
}

/// The stored solutions when the entry carries `key_sha` and both files are intact.
pub fn load(paths: &CachePaths, key_sha: &str) -> Option<Loaded> {
    let meta = read_json(paths)?;
    if meta.get("key_sha256")?.as_str()? != key_sha {
        return None;
    }
    let n_contacts = usize::try_from(meta.get("n_contacts")?.as_u64()?).ok()?;
    let n_nodes = usize::try_from(meta.get("n_nodes")?.as_u64()?).ok()?;
    let solved: Vec<usize> = meta
        .get("solved")?
        .as_array()?
        .iter()
        .map(|v| v.as_u64().and_then(|n| usize::try_from(n).ok()))
        .collect::<Option<_>>()?;
    let bytes = std::fs::read(&paths.bin).ok()?;
    if bytes.len() != solved.len().checked_mul(n_nodes)?.checked_mul(8)? {
        return None;
    }
    let mut solutions: Vec<Option<Vec<f64>>> = vec![None; n_contacts];
    for (slot, &k) in solved.iter().enumerate() {
        if k >= n_contacts {
            return None;
        }
        let part = &bytes[slot * n_nodes * 8..(slot + 1) * n_nodes * 8];
        solutions[k] = Some(
            (0..n_nodes)
                .map(|n| {
                    let b: [u8; 8] = part[n * 8..n * 8 + 8].try_into().unwrap_or([0; 8]);
                    f64::from_le_bytes(b)
                })
                .collect(),
        );
    }
    let z_rows = meta.get("z_kohm")?.as_array()?;
    if z_rows.len() != n_contacts {
        return None;
    }
    let mut z_kohm = Vec::with_capacity(n_contacts);
    for row in z_rows {
        let cells = row.as_array()?;
        if cells.len() != n_contacts {
            return None;
        }
        z_kohm.push(
            cells
                .iter()
                .map(|c| c.as_f64().unwrap_or(f64::NAN))
                .collect::<Vec<f64>>(),
        );
    }
    let stats = meta
        .get("stats")?
        .as_array()?
        .iter()
        .map(|s| {
            Some(SolveStats {
                contact: usize::try_from(s.get("contact")?.as_u64()?).ok()?,
                iterations: usize::try_from(s.get("iterations")?.as_u64()?).ok()?,
                rel_residual: s.get("rel_residual")?.as_f64()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let text = |k: &str| meta.get(k).and_then(Value::as_str).map(str::to_owned);
    Some(Loaded {
        solutions,
        z_kohm,
        stats,
        built_by: text("built_by"),
        adapter: text("adapter"),
    })
}

/// Stores every solved contact of `basis` under `key`, recording the backend that built it (not
/// part of the key: either backend's basis is accepted, `specs/07` §3.6).
pub fn store(
    paths: &CachePaths,
    key: &Value,
    basis: &Basis,
    built_by: &str,
    adapter: Option<&str>,
) -> std::io::Result<()> {
    let solved: Vec<usize> = basis
        .solutions
        .iter()
        .enumerate()
        .filter_map(|(k, s)| s.as_ref().map(|_| k))
        .collect();
    let n_nodes = solved
        .first()
        .and_then(|&k| basis.solutions[k].as_ref().map(Vec::len))
        .unwrap_or(0);
    let mut bytes = Vec::with_capacity(solved.len() * n_nodes * 8);
    for &k in &solved {
        if let Some(s) = &basis.solutions[k] {
            for v in s {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let z: Vec<Vec<Value>> = basis
        .z_kohm
        .iter()
        .map(|row| {
            row.iter()
                .map(|v| if v.is_finite() { json!(v) } else { Value::Null })
                .collect()
        })
        .collect();
    let stats: Vec<Value> = basis
        .stats
        .iter()
        .map(|s| {
            json!({"contact": s.contact, "iterations": s.iterations, "rel_residual": s.rel_residual})
        })
        .collect();
    let meta = json!({
        "schema": "dbsloc-efieldbasis",
        "schema_version": 1,
        "key": key,
        "key_sha256": key_sha256(key),
        "built_by": built_by,
        "adapter": adapter,
        "n_contacts": basis.n_contacts,
        "n_nodes": n_nodes,
        "solved": solved,
        "z_kohm": z,
        "stats": stats,
    });
    write_replacing(&paths.bin, &bytes)?;
    write_replacing(
        &paths.json,
        (serde_json::to_string_pretty(&meta)? + "\n").as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_headmodel_layout() {
        let p = CachePaths::new(Path::new("/d/sub-01"), "01", Side::Left);
        assert!(p
            .json
            .ends_with("headmodel/native/sub-01_hemi-L_desc-efieldbasis.json"));
        assert!(p
            .bin
            .ends_with("headmodel/native/sub-01_hemi-L_desc-efieldbasis.bin"));
    }

    #[test]
    fn a_missing_entry_matches_nothing() {
        let p = CachePaths::new(Path::new("/definitely/not/here"), "01", Side::Right);
        assert!(!matches(&p, "abc"));
        assert!(load(&p, "abc").is_none());
    }
}
