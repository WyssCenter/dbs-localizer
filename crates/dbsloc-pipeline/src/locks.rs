// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::bids::Subject;
use crate::outputs::write_replacing;
use crate::paths::absolute;
use crate::sha256::file_sha256_hex;

const FINGERPRINT_FIELDS: [&str; 5] = ["path", "exists", "mtime_ns", "size", "sha256"];

pub fn stage_approval_path(locks_dir: &Path, stage: &str) -> PathBuf {
    locks_dir.join(format!("{stage}.json"))
}

fn mtime_ns(meta: &std::fs::Metadata) -> Option<i128> {
    let t = meta.modified().ok()?;
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => Some(d.as_nanos() as i128),
        Err(e) => Some(-(e.duration().as_nanos() as i128)),
    }
}

#[derive(Debug, Default)]
pub struct HashMemo(HashMap<(String, Option<u64>, Option<i64>), String>);

fn snap_unhashed(raw: &Path) -> Value {
    let r = absolute(raw).to_string_lossy().into_owned();
    match std::fs::metadata(raw).ok().filter(|m| m.is_file()) {
        None => json!({
            "path": r, "exists": false, "mtime_ns": null, "size": null, "sha256": null
        }),
        Some(m) => {
            let mtime = mtime_ns(&m).map_or(Value::Null, |n| {
                i64::try_from(n).map_or(Value::Null, |v| json!(v))
            });
            json!({
                "path": r, "exists": true, "mtime_ns": mtime, "size": m.len(), "sha256": null
            })
        }
    }
}

fn fill_hash(raw: &Path, entry: &mut Value, memo: &mut HashMemo) -> Result<(), String> {
    if entry.get("exists") != Some(&Value::Bool(true)) {
        return Ok(());
    }
    let key = (
        entry["path"].as_str().unwrap_or_default().to_owned(),
        entry["size"].as_u64(),
        entry["mtime_ns"].as_i64(),
    );
    let sha = match memo.0.get(&key) {
        Some(s) => s.clone(),
        None => {
            let s =
                file_sha256_hex(raw).map_err(|e| format!("cannot hash {}: {e}", raw.display()))?;
            memo.0.insert(key, s.clone());
            s
        }
    };
    entry["sha256"] = json!(sha);
    Ok(())
}

pub fn snap_input_paths(paths: &[PathBuf]) -> Result<Vec<Value>, String> {
    let mut memo = HashMemo::default();
    paths
        .iter()
        .map(|raw| {
            let mut entry = snap_unhashed(raw);
            fill_hash(raw, &mut entry, &mut memo)?;
            Ok(entry)
        })
        .collect()
}

fn path_key(v: &Value) -> Option<String> {
    let p = v.get("path")?.as_str()?;
    Some(if cfg!(windows) {
        p.to_lowercase()
    } else {
        p.to_owned()
    })
}

fn fields_match(want: &Value, got: &Value, fields: &[&str]) -> bool {
    let Some(g) = got.as_object() else {
        return false;
    };
    fields.iter().all(|f| {
        if *f == "path" {
            return path_key(want) == path_key(got);
        }
        g.get(*f).is_some_and(|v| Some(v) == want.get(*f))
    })
}

pub fn approval_inputs_still_valid(approval: &Value, paths: &[PathBuf]) -> bool {
    approval_inputs_still_valid_with(approval, paths, &mut HashMemo::default())
}

pub fn approval_inputs_still_valid_with(
    approval: &Value,
    paths: &[PathBuf],
    memo: &mut HashMemo,
) -> bool {
    let Some(got) = approval.get("inputs").and_then(Value::as_array) else {
        return false;
    };
    if got.len() != paths.len() {
        return false;
    }
    let mut by_path: BTreeMap<String, &Value> = BTreeMap::new();
    for g in got {
        match path_key(g) {
            Some(k) => {
                if by_path.insert(k, g).is_some() {
                    return false;
                }
            }
            None => return false,
        }
    }
    let cheap = &FINGERPRINT_FIELDS[..FINGERPRINT_FIELDS.len() - 1];
    let mut pending = Vec::with_capacity(paths.len());
    for raw in paths {
        let want = snap_unhashed(raw);
        let Some(g) = path_key(&want).and_then(|k| by_path.get(&k).copied()) else {
            return false;
        };
        if !fields_match(&want, g, cheap) || g.get("sha256").is_none() {
            return false;
        }
        pending.push((raw, want, g));
    }
    pending.into_iter().all(|(raw, mut want, g)| {
        fill_hash(raw, &mut want, memo).is_ok() && fields_match(&want, g, &FINGERPRINT_FIELDS)
    })
}

pub fn read_approval(path: &Path) -> Result<Option<Value>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("corrupt lock {}: {e}", path.display()))
}

pub fn approval_flag(d: &Value) -> bool {
    let Some(obj) = d.as_object().filter(|o| !o.is_empty()) else {
        return false;
    };
    match obj.get("approval").or_else(|| obj.get("approved")) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
        _ => false,
    }
}

pub fn is_approved(path: &Path) -> Result<bool, String> {
    Ok(read_approval(path)?.as_ref().is_some_and(approval_flag))
}

pub fn lock_allows_skip(path: &Path, input_paths: Option<&[PathBuf]>) -> Result<bool, String> {
    lock_allows_skip_with(path, input_paths, &mut HashMemo::default())
}

pub fn lock_allows_skip_with(
    path: &Path,
    input_paths: Option<&[PathBuf]>,
    memo: &mut HashMemo,
) -> Result<bool, String> {
    let Some(d) = read_approval(path)? else {
        return Ok(false);
    };
    if !approval_flag(&d) {
        return Ok(false);
    }
    Ok(match input_paths.filter(|p| !p.is_empty()) {
        None => true,
        Some(paths) => approval_inputs_still_valid_with(&d, paths, memo),
    })
}

pub fn write_approval(
    path: &Path,
    approved: bool,
    method: Option<&str>,
    input_paths: &[PathBuf],
    timestamp: Option<f64>,
) -> std::io::Result<()> {
    write_approval_with(path, approved, method, input_paths, timestamp, &Map::new())
}

pub fn write_approval_with(
    path: &Path,
    approved: bool,
    method: Option<&str>,
    input_paths: &[PathBuf],
    timestamp: Option<f64>,
    extra: &Map<String, Value>,
) -> std::io::Result<()> {
    let mut d = Map::new();
    d.insert("approval".into(), json!(approved));
    if let Some(m) = method {
        d.insert("method".into(), json!(m));
    }
    if !input_paths.is_empty() {
        let snap = snap_input_paths(input_paths).map_err(std::io::Error::other)?;
        d.insert("inputs".into(), Value::Array(snap));
    }
    for (k, v) in extra {
        d.insert(k.clone(), v.clone());
    }
    let ts = timestamp.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    });
    d.insert("timestamp".into(), json!(ts));
    let text = serde_json::to_string_pretty(&Value::Object(d)).map_err(std::io::Error::other)?;
    write_replacing(path, (text + "\n").as_bytes())
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(absolute(p).to_string_lossy().into_owned()))
        .collect()
}

pub fn default_lock_input_paths(sub: &Subject) -> BTreeMap<String, Vec<PathBuf>> {
    let pre: Vec<PathBuf> = sub
        .preop_anchor_raw
        .clone()
        .or_else(|| sub.preop_raw.values().next().cloned())
        .into_iter()
        .collect();
    let used: Vec<PathBuf> = sub
        .anchor_in_use()
        .or_else(|| pre.first().cloned())
        .into_iter()
        .collect();
    let post: Vec<PathBuf> = sub.postop_raw.values().cloned().collect();
    let mut d: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    if !pre.is_empty() {
        for name in ["bids", "preprocess", "precoreg", "brainshift"] {
            d.insert(name.to_owned(), pre.clone());
        }
    }
    if !used.is_empty() {
        let mut norm = used.clone();
        norm.extend(sub.preop_t2_nonanchor().map(Path::to_path_buf));
        d.insert("normalize".to_owned(), norm);
    }
    let base: Vec<PathBuf> = used.iter().chain(post.iter()).cloned().collect();
    if !base.is_empty() {
        d.insert("coreg".to_owned(), base);
    }
    let mut norm_paths = sub.norm_postop_images();
    if norm_paths.is_empty() {
        norm_paths.extend(sub.norm.anat_preop.values().next().cloned());
    }
    let reco = sub.reconstruction_json();
    if let Some(first) = norm_paths.first().cloned() {
        d.insert("reconstruct".to_owned(), norm_paths.clone());
        let mut vis = vec![first.clone()];
        if reco.is_file() {
            vis.push(reco.clone());
        }
        d.insert("visualize".to_owned(), dedupe(vis));
    }
    d.insert("vat".to_owned(), crate::vat::stage::lock_input_paths(sub));
    let mut orient: Vec<PathBuf> = norm_paths.first().cloned().into_iter().collect();
    if let Some(ct_norm) = sub.norm.anat_postop.get("CT").filter(|p| p.is_file()) {
        orient.push(absolute(ct_norm));
    }
    if let Some(raw_ct) = sub.postop_ct_raw.as_ref().filter(|p| p.is_file()) {
        orient.push(absolute(raw_ct));
    }
    if let Some(coreg_affine) = sub.coreg.transform.get("CT").filter(|p| p.is_file()) {
        orient.push(absolute(coreg_affine));
    }
    if reco.is_file() {
        orient.push(reco);
    }
    if !orient.is_empty() {
        d.insert("orientation".to_owned(), dedupe(orient));
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_locks_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn snapshot_records_content_hash_and_resolved_path() {
        let d = scratch("snap");
        let f = d.join("a.nii");
        std::fs::write(&f, b"abc").unwrap();
        let s = snap_input_paths(std::slice::from_ref(&f)).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0]["path"], json!(absolute(&f).to_string_lossy()));
        assert_eq!(s[0]["exists"], json!(true));
        assert_eq!(s[0]["size"], json!(3));
        assert_eq!(
            s[0]["sha256"],
            json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        let missing = snap_input_paths(&[d.join("nope.nii")]).unwrap();
        assert_eq!(missing[0]["exists"], json!(false));
        assert!(missing[0]["sha256"].is_null());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn same_size_rewrite_invalidates_the_lock() {
        let d = scratch("rewrite");
        let a = d.join("up.nii");
        std::fs::write(&a, b"x").unwrap();
        let lock = d.join("preprocess.json");
        write_approval(&lock, true, Some("n4"), std::slice::from_ref(&a), None).unwrap();
        assert!(lock_allows_skip(&lock, Some(std::slice::from_ref(&a))).unwrap());
        std::fs::write(&a, b"y").unwrap();
        assert!(!lock_allows_skip(&lock, Some(std::slice::from_ref(&a))).unwrap());
        assert!(lock_allows_skip(&lock, None).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn entries_match_by_path_not_position() {
        let d = scratch("order");
        let a = d.join("a.nii");
        let b = d.join("b.nii");
        std::fs::write(&a, b"1").unwrap();
        std::fs::write(&b, b"2").unwrap();
        let lock = d.join("coreg.json");
        write_approval(&lock, true, None, &[a.clone(), b.clone()], None).unwrap();
        assert!(lock_allows_skip(&lock, Some(&[b.clone(), a.clone()])).unwrap());
        assert!(!lock_allows_skip(&lock, Some(std::slice::from_ref(&a))).unwrap());
        let mut v = read_approval(&lock).unwrap().unwrap();
        let first = v["inputs"][0]["path"].as_str().unwrap().to_owned();
        v["inputs"][0]["path"] = json!(if cfg!(windows) {
            first.to_uppercase()
        } else {
            first
        });
        assert!(approval_inputs_still_valid(&v, &[b, a]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_locks_are_errors_not_absent() {
        let d = scratch("corrupt");
        let p = d.join("x.json");
        assert_eq!(read_approval(&p).unwrap(), None);
        std::fs::write(&p, "{not json").unwrap();
        assert!(read_approval(&p).unwrap_err().contains("corrupt lock"));
        assert!(lock_allows_skip(&p, None).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn approval_flags_and_legacy_entries() {
        let d = scratch("flags");
        let p = d.join("x.json");
        assert!(!is_approved(&p).unwrap());
        for (body, want) in [
            (r#"{"approval": true}"#, true),
            (r#"{"approved": 1}"#, true),
            (r#"{"approval": 0}"#, false),
            (r#"{"approval": "yes"}"#, false),
        ] {
            std::fs::write(&p, body).unwrap();
            assert_eq!(is_approved(&p).unwrap(), want, "{body}");
        }
        let a = d.join("a.nii");
        std::fs::write(&a, b"k").unwrap();
        let mut snap = snap_input_paths(std::slice::from_ref(&a)).unwrap();
        snap[0].as_object_mut().unwrap().remove("sha256");
        let legacy = json!({"approval": true, "inputs": snap});
        assert!(!approval_inputs_still_valid(
            &legacy,
            std::slice::from_ref(&a)
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn normalize_fingerprint_includes_the_t2_channel_input() {
        let d = scratch("t2fp");
        let anat = d.join("rawdata/sub-1/ses-preop/anat");
        std::fs::create_dir_all(&anat).unwrap();
        let t1 = anat.join("sub-1_ses-preop_T1w.nii.gz");
        std::fs::write(&t1, b"t1").unwrap();
        let sub = crate::bids::resolve_subject_with(&d, "1", false).unwrap();
        let anchor = sub.preop_anchor_raw.clone().unwrap();
        assert_eq!(
            default_lock_input_paths(&sub)["normalize"],
            vec![anchor.clone()]
        );
        let t2 = anat.join("sub-1_ses-preop_T2w.nii.gz");
        std::fs::write(&t2, b"t2").unwrap();
        let sub = crate::bids::resolve_subject_with(&d, "1", false).unwrap();
        let t2_resolved = sub.preop_t2_nonanchor().unwrap().to_path_buf();
        assert!(t2_resolved.ends_with("sub-1_ses-preop_T2w.nii.gz"));
        let norm = default_lock_input_paths(&sub)["normalize"].clone();
        assert_eq!(norm, vec![anchor, t2_resolved.clone()]);
        assert!(!default_lock_input_paths(&sub)["coreg"].contains(&t2_resolved));
        let lock = d.join("normalize.json");
        write_approval(&lock, true, None, &norm, None).unwrap();
        assert!(lock_allows_skip(&lock, Some(&norm)).unwrap());
        std::fs::write(&t2, b"T2").unwrap();
        assert!(!lock_allows_skip(&lock, Some(&norm)).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cheap_fields_are_compared_before_hashing_and_hashes_are_memoized() {
        let d = scratch("memo");
        let a = d.join("a.nii");
        std::fs::write(&a, b"one").unwrap();
        let lock = d.join("coreg.json");
        write_approval(&lock, true, None, std::slice::from_ref(&a), None).unwrap();
        let approval = read_approval(&lock).unwrap().unwrap();
        let mut memo = HashMemo::default();
        assert!(approval_inputs_still_valid_with(
            &approval,
            std::slice::from_ref(&a),
            &mut memo
        ));
        assert_eq!(memo.0.len(), 1);
        assert!(lock_allows_skip_with(&lock, Some(std::slice::from_ref(&a)), &mut memo).unwrap());
        assert_eq!(memo.0.len(), 1);
        std::fs::write(&a, b"longer").unwrap();
        let mut fresh = HashMemo::default();
        assert!(!approval_inputs_still_valid_with(
            &approval,
            std::slice::from_ref(&a),
            &mut fresh
        ));
        assert!(fresh.0.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn written_lock_has_the_python_shape() {
        let d = scratch("shape");
        let a = d.join("a.nii");
        std::fs::write(&a, b"k").unwrap();
        let p = stage_approval_path(&d.join("locks"), "preprocess");
        write_approval(&p, true, Some("n4"), std::slice::from_ref(&a), Some(1.5)).unwrap();
        let v = read_approval(&p).unwrap().unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["approval", "method", "inputs", "timestamp"]);
        assert_eq!(
            v["inputs"][0]["path"],
            json!(absolute(&a).to_string_lossy())
        );
        assert!(std::fs::read_to_string(&p).unwrap().ends_with("}\n"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
