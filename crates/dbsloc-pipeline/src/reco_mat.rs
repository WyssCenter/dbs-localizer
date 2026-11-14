// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Reco v2 JSON → reference-implementation reconstruction `.mat` (`specs/12` Part 1).
//!
//! The file holds one variable `reco` (`specs/12` §3) mapped from the JSON by the rules of
//! `specs/12` §7, plus the provenance variable `dbsloc_provenance` (a struct of char fields).
//! Side index 1 is the right hemisphere, 2 the left (`specs/05` §8).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use dbsloc_reconstruct::native_frame::contacts_from_markers;
use dbsloc_reconstruct::{add3, electrode_by_name, norm3, scale3, sub3, ElectrodeGeometry};
use fastwarp_matfile::mat5::{mat5_bytes, Mat5Value};

use crate::manual_correction::side_review;
use crate::outputs::write_replacing;
use crate::tutor::{side_hemispheres, REFERENCE_SPACE};
use crate::version::{utc_now_rfc3339, version_string, PRODUCT};

pub const RECO_VARIABLE: &str = "reco";
pub const PROVENANCE_VARIABLE: &str = "dbsloc_provenance";
pub const MAT_TRAJECTORY_POINTS: usize = dbsloc_reconstruct::TRAJECTORY_POINTS;
const RADIUS_TOLERANCE_MM: f64 = 1e-6;
const ROLL_DETERMINED: [&str; 3] = ["resolved", "ambiguous", "manual"];
const MARKER_FIELDS: [&str; 4] = ["head", "tail", "x", "y"];
const HEMISPHERES: [&str; 2] = ["right", "left"];

/// The variables of the `.mat` file and the warnings of the conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoMat {
    pub variables: Vec<(String, Mat5Value)>,
    pub warnings: Vec<String>,
}

/// `sub-<id>_desc-reconstruction.json` → `sub-<id>_desc-reconstruction.mat` in the same folder.
pub fn reco_mat_path(json_path: &Path) -> PathBuf {
    json_path.with_extension("mat")
}

fn xyz(v: &Value) -> Option<[f64; 3]> {
    match v.as_array()?.as_slice() {
        [a, b, c] => {
            let p = [a.as_f64()?, b.as_f64()?, c.as_f64()?];
            p.iter().all(|x| x.is_finite()).then_some(p)
        }
        _ => None,
    }
}

fn points(v: &Value) -> Option<Vec<[f64; 3]>> {
    v.as_array()?.iter().map(xyz).collect()
}

fn unit(v: [f64; 3]) -> Option<[f64; 3]> {
    let n = norm3(v);
    (n.is_finite() && n > 1e-6).then(|| scale3(v, 1.0 / n))
}

fn side_label(block: &str, index: usize) -> String {
    format!(
        "{block} side {} ({})",
        index + 1,
        HEMISPHERES.get(index).copied().unwrap_or("?")
    )
}

fn side_object<'a>(block: Option<&'a Map<String, Value>>, key: Option<&str>) -> Option<&'a Value> {
    block?.get(key?).filter(|v| v.is_object())
}

fn model_name<'a>(reco: &'a Value, key: &str) -> Option<&'a str> {
    reco["props"][key]["elmodel"]
        .as_str()
        .or_else(|| reco["meta"]["elmodel"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Trajectory of `specs/12` §4: 50 points from `head − 5·û` to `tail + k·span·û`, where span is
/// the catalogue array span (`specs/09` §2.2) and `k = 1.5` for models with more than 8
/// levels, else 2.
pub fn synthesize_trajectory(
    head: [f64; 3],
    tail: [f64; 3],
    electrode: &ElectrodeGeometry,
) -> Option<Vec<[f64; 3]>> {
    electrode.trajectory(head, tail)
}

fn at_radius(
    head: [f64; 3],
    p: [f64; 3],
    r: f64,
    name: &str,
    label: &str,
    warnings: &mut Vec<String>,
) -> Result<[f64; 3], String> {
    let d = sub3(p, head);
    let n = norm3(d);
    if !(n.is_finite() && n > 1e-9) {
        return Err(format!(
            "reco_mat: {label}: markers.{name} coincides with markers.head"
        ));
    }
    if (n - r).abs() <= RADIUS_TOLERANCE_MM {
        return Ok(p);
    }
    warnings.push(format!(
        "dbsloc: warning: reco_mat: {label}: |{name} - head| = {n:.6} mm rescaled to the lead \
         radius {r} mm (specs/12 §7)"
    ));
    Ok(add3(head, scale3(d, r / n)))
}

struct SideExport {
    coords: Vec<[f64; 3]>,
    trajectory: Vec<[f64; 3]>,
    markers: [[f64; 3]; 4],
    trajvector: [f64; 3],
}

fn export_side(
    side: &Value,
    electrode: &ElectrodeGeometry,
    label: &str,
    warnings: &mut Vec<String>,
) -> Result<SideExport, String> {
    let m = &side["markers"];
    let mut markers = [[0.0; 3]; 4];
    for (slot, name) in markers.iter_mut().zip(MARKER_FIELDS) {
        *slot = xyz(&m[name]).ok_or_else(|| {
            format!("reco_mat: {label}: markers.{name} must hold three finite numbers")
        })?;
    }
    let [head, tail, x, y] = markers;
    let trajvector = unit(sub3(tail, head))
        .ok_or_else(|| format!("reco_mat: {label}: head and tail coincide"))?;
    let r = 0.5 * electrode.lead_diameter_mm;
    let x = at_radius(head, x, r, "x", label, warnings)?;
    let y = at_radius(head, y, r, "y", label, warnings)?;
    let stored = match side.get("coords_mm") {
        None | Some(Value::Null) => None,
        Some(v) => Some(points(v).ok_or_else(|| {
            format!("reco_mat: {label}: coords_mm must be rows of three finite numbers")
        })?),
    };
    let coords = match stored {
        Some(rows) if rows.len() == electrode.n_contacts => rows,
        other => {
            let rows = contacts_from_markers(electrode, head, tail, x, y).ok_or_else(|| {
                format!(
                    "reco_mat: {label}: the contacts need re-resolving ({} rows for the {} \
                     contacts of {}) but the markers are degenerate",
                    other.as_ref().map_or(0, Vec::len),
                    electrode.n_contacts,
                    electrode.elmodel
                )
            })?;
            warnings.push(format!(
                "dbsloc: note: reco_mat: {label}: {} contact rows re-resolved from the markers to \
                 the {} contacts of {} (specs/12 §7)",
                other.as_ref().map_or(0, Vec::len),
                electrode.n_contacts,
                electrode.elmodel
            ));
            rows
        }
    };
    let stored_trajectory = match side.get("trajectory") {
        None | Some(Value::Null) => None,
        Some(v) => Some(points(v).ok_or_else(|| {
            format!("reco_mat: {label}: trajectory must be rows of three finite numbers")
        })?),
    };
    let trajectory = match stored_trajectory {
        Some(rows) if rows.len() == MAT_TRAJECTORY_POINTS => rows,
        other => {
            warnings.push(format!(
                "dbsloc: note: reco_mat: {label}: trajectory of {} points rebuilt as the \
                 {MAT_TRAJECTORY_POINTS}-point line from the markers (specs/12 §4)",
                other.as_ref().map_or(0, Vec::len)
            ));
            synthesize_trajectory(head, tail, electrode)
                .ok_or_else(|| format!("reco_mat: {label}: head and tail coincide"))?
        }
    };
    Ok(SideExport {
        coords,
        trajectory,
        markers: [head, tail, x, y],
        trajvector,
    })
}

fn export_block(
    name: &str,
    block: &Map<String, Value>,
    keys: &[Option<String>; 2],
    electrodes: &[Option<&'static ElectrodeGeometry>; 2],
    sides: usize,
    with_trajvector: bool,
    warnings: &mut Vec<String>,
) -> Result<Mat5Value, String> {
    let mut coords = Vec::with_capacity(sides);
    let mut trajectories = Vec::with_capacity(sides);
    let mut markers = Vec::with_capacity(sides);
    let mut trajvectors = Vec::with_capacity(sides);
    for index in 0..sides {
        let label = side_label(name, index);
        let key = keys.get(index).and_then(Option::as_deref);
        match side_object(Some(block), key) {
            Some(side) => {
                let electrode = electrodes.get(index).copied().flatten().ok_or_else(|| {
                    format!("reco_mat: {label}: electrode model not in the catalogue")
                })?;
                let s = export_side(side, electrode, &label, warnings)?;
                coords.push(Mat5Value::rows3(&s.coords));
                trajectories.push(Mat5Value::rows3(&s.trajectory));
                markers.push(s.markers.iter().map(|p| Mat5Value::row(p)).collect());
                trajvectors.push(Mat5Value::row(&s.trajvector));
            }
            None => {
                coords.push(Mat5Value::Empty);
                trajectories.push(Mat5Value::Empty);
                markers.push(vec![Mat5Value::Empty; MARKER_FIELDS.len()]);
                trajvectors.push(Mat5Value::Empty);
            }
        }
    }
    let mut fields = vec![
        ("coords_mm".to_owned(), Mat5Value::cell_row(coords)),
        ("trajectory".to_owned(), Mat5Value::cell_row(trajectories)),
        (
            "markers".to_owned(),
            Mat5Value::Struct {
                dims: vec![1, sides],
                fields: MARKER_FIELDS.iter().map(|f| (*f).to_owned()).collect(),
                elements: markers,
            },
        ),
    ];
    if with_trajvector {
        fields.push(("trajvector".to_owned(), Mat5Value::cell_row(trajvectors)));
    }
    Ok(Mat5Value::struct_scalar(fields))
}

fn side_keys(
    meta: &Map<String, Value>,
    template: (&str, &Map<String, Value>),
    native: Option<&Map<String, Value>>,
) -> Result<([Option<String>; 2], String), String> {
    let declared = meta.get("side_keys").is_some_and(|v| !v.is_null());
    let mut pairs = side_hemispheres(meta, template.1, template.0)?;
    if let Some(native) = native {
        if declared || pairs.is_empty() {
            pairs.extend(side_hemispheres(meta, native, "native")?);
        }
        for key in native.iter().filter(|(_, v)| v.is_object()).map(|(k, _)| k) {
            if !pairs.iter().any(|(k, _)| k == key) {
                return Err(format!(
                    "reco_mat: native side {key:?} is not in the {} block and the file declares \
                     no meta.side_keys; cannot tell its hemisphere",
                    template.0
                ));
            }
        }
    }
    let mut keys: [Option<String>; 2] = [None, None];
    for (key, hemisphere) in &pairs {
        let index = usize::from(*hemisphere == "left");
        match &keys[index] {
            Some(k) if k != key => {
                return Err(format!(
                    "reco_mat: sides {k:?} and {key:?} are both the {hemisphere} hemisphere"
                ))
            }
            _ => keys[index] = Some(key.clone()),
        }
    }
    let described = match meta.get("side_keys").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => format!(
            "resolved by mean x: {}",
            keys.iter()
                .zip(HEMISPHERES)
                .filter_map(|(k, h)| k.as_ref().map(|k| format!("{k}={h}")))
                .collect::<Vec<_>>()
                .join(",")
        ),
    };
    Ok((keys, described))
}

fn truthy(v: &Value) -> bool {
    v.as_bool()
        .or_else(|| v.as_f64().map(|x| x != 0.0))
        .unwrap_or(false)
}

/// `props(k).manually_corrected` of `specs/12` §7.
pub fn manually_corrected_flag(
    reco: &Value,
    key: &str,
    electrode: Option<&ElectrodeGeometry>,
) -> bool {
    let roll_determined = reco["meta"]["orientation"][key]["status"]
        .as_str()
        .is_some_and(|s| ROLL_DETERMINED.contains(&s));
    truthy(&reco["props"][key]["manually_corrected"])
        || side_review(reco, key).is_some()
        || (electrode.is_some_and(|e| e.is_directed) && roll_determined)
}

/// The `reco` and `dbsloc_provenance` variables of `specs/12` §3 / §7 for a Reco v2 file.
/// `source_json` is the JSON file name recorded in the provenance, `written_at` its export time.
pub fn reco_mat_from_json(
    reco: &Value,
    source_json: &str,
    written_at: &str,
) -> Result<RecoMat, String> {
    let mut converted = reco.clone();
    let tail_conversions =
        dbsloc_reconstruct::tail_fiducial::convert_legacy_tail(&mut converted, None);
    let reco = &converted;
    let root = reco
        .as_object()
        .ok_or("reco_mat: the reconstruction JSON root must be an object")?;
    if root.get("version").and_then(Value::as_f64) != Some(2.0) {
        return Err("reco_mat: expected a version 2 reconstruction (specs/05 §8)".to_owned());
    }
    let empty = Map::new();
    let meta = root
        .get("meta")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let template_space = meta
        .get("template_space")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let use_reference = template_space.is_some_and(|s| s != REFERENCE_SPACE);
    let (mni_source, mni) = if use_reference {
        let space = template_space.unwrap_or_default();
        let reference = root
            .get("reference")
            .and_then(Value::as_object)
            .filter(|b| b.values().any(Value::is_object))
            .ok_or_else(|| {
                format!(
                    "reco_mat: the template space is {space}, not {REFERENCE_SPACE}, and the file \
                     has no reference block; the .mat mni block must be in {REFERENCE_SPACE} \
                     (specs/12 §7); re-run reconstruct with the template bridge available"
                )
            })?;
        if let Some(s) = reference.get("space").and_then(Value::as_str) {
            if s != REFERENCE_SPACE {
                return Err(format!(
                    "reco_mat: the reference block is in {s}, not {REFERENCE_SPACE}"
                ));
            }
        }
        ("reference", reference)
    } else {
        let mni = root
            .get("mni")
            .and_then(Value::as_object)
            .ok_or("reco_mat: the reconstruction has no mni block")?;
        ("mni", mni)
    };
    let native = root.get("native").and_then(Value::as_object);
    let (keys, side_keys_text) = side_keys(meta, (mni_source, mni), native)?;
    let present = |index: usize| {
        let key = keys[index].as_deref();
        side_object(Some(mni), key).is_some() || side_object(native, key).is_some()
    };
    let sides = if present(1) {
        2
    } else if present(0) {
        1
    } else {
        return Err("reco_mat: the reconstruction has no lead sides".to_owned());
    };
    let mut electrodes: [Option<&'static ElectrodeGeometry>; 2] = [None, None];
    for index in 0..sides {
        if let Some(key) = keys[index].as_deref() {
            if present(index) {
                let name = model_name(reco, key).ok_or_else(|| {
                    format!(
                        "reco_mat: {}: no electrode model (props.elmodel / meta.elmodel)",
                        side_label("props", index)
                    )
                })?;
                electrodes[index] = Some(electrode_by_name(name).ok_or_else(|| {
                    format!("reco_mat: electrode model {name:?} is not in the catalogue")
                })?);
            }
        }
    }
    let fallback_model = electrodes
        .iter()
        .flatten()
        .next()
        .map(|e| e.elmodel.clone())
        .unwrap_or_default();
    let mut warnings: Vec<String> = tail_conversions.into_iter().map(|c| c.warning).collect();
    let props = (0..sides)
        .map(|index| match (keys[index].as_deref(), electrodes[index]) {
            (Some(key), Some(e)) => vec![
                Mat5Value::char(&e.elmodel),
                Mat5Value::scalar(f64::from(u8::from(manually_corrected_flag(
                    reco,
                    key,
                    Some(e),
                )))),
            ],
            _ => vec![Mat5Value::char(&fallback_model), Mat5Value::scalar(0.0)],
        })
        .collect();
    let mut fields = vec![(
        "props".to_owned(),
        Mat5Value::Struct {
            dims: vec![1, sides],
            fields: vec!["elmodel".to_owned(), "manually_corrected".to_owned()],
            elements: props,
        },
    )];
    match native {
        Some(block) => fields.push((
            "native".to_owned(),
            export_block(
                "native",
                block,
                &keys,
                &electrodes,
                sides,
                false,
                &mut warnings,
            )?,
        )),
        None => warnings.push(
            "dbsloc: note: reco_mat: the reconstruction has no native block (post-op MRI \
             template path); the .mat carries only mni, so the reference implementation rebuilds \
             native from its own registration transforms on the first native load (specs/12 §5.1)"
                .to_owned(),
        ),
    }
    fields.push((
        "mni".to_owned(),
        export_block(
            mni_source,
            mni,
            &keys,
            &electrodes,
            sides,
            true,
            &mut warnings,
        )?,
    ));
    for (index, key) in keys.iter().enumerate().take(sides) {
        let key = key.as_deref();
        if side_object(native, key).is_some() && side_object(Some(mni), key).is_none() {
            warnings.push(format!(
                "dbsloc: warning: reco_mat: {} is not in {mni_source}; its mni entries are empty",
                side_label("native", index)
            ));
        }
    }
    let text = |s: &str| Mat5Value::char(s);
    let provenance = Mat5Value::struct_scalar(vec![
        ("product".to_owned(), text(PRODUCT)),
        ("exporter".to_owned(), text(&version_string())),
        ("source_json".to_owned(), text(source_json)),
        (
            "template_space".to_owned(),
            text(template_space.unwrap_or_default()),
        ),
        ("mni_source".to_owned(), text(mni_source)),
        ("side_keys".to_owned(), text(&side_keys_text)),
        ("written_at".to_owned(), text(written_at)),
    ]);
    Ok(RecoMat {
        variables: vec![
            (RECO_VARIABLE.to_owned(), Mat5Value::struct_scalar(fields)),
            (PROVENANCE_VARIABLE.to_owned(), provenance),
        ],
        warnings,
    })
}

/// Writes the `.mat` of `reco` to `mat_path` (compressed v7, replaced atomically) and returns the
/// conversion warnings. The provenance names the sibling `.json` as the source.
pub fn write_reco_mat(reco: &Value, mat_path: &Path) -> Result<Vec<String>, String> {
    let source = mat_path
        .with_extension("json")
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let out = reco_mat_from_json(reco, &source, &utc_now_rfc3339())?;
    let bytes = mat5_bytes(&out.variables, true).map_err(|e| format!("reco_mat: {e}"))?;
    write_replacing(mat_path, &bytes)
        .map_err(|e| format!("reco_mat: cannot write {}: {e}", mat_path.display()))?;
    Ok(out.warnings)
}

/// Writes the `.mat` twin of the reconstruction JSON at `json_path` and reports through `log`;
/// an export problem is a warning, never a stage failure (the JSON stays the primary file).
pub fn export_beside_json(json_path: &Path, reco: &Value, log: &mut dyn FnMut(String)) {
    let mat_path = reco_mat_path(json_path);
    match write_reco_mat(reco, &mat_path) {
        Ok(warnings) => {
            for w in warnings {
                log(w);
            }
            log(format!(
                "[reconstruct] written: {}",
                mat_path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        Err(e) => {
            let _ = std::fs::remove_file(&mat_path);
            log(format!(
                "dbsloc: warning: reconstruct: the Lead-DBS compatible {} was not written: {e}",
                mat_path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use dbsloc_reconstruct::native_frame::{resolve_fiducials, SideGeometry};
    use dbsloc_reconstruct::{cross3, unit3};
    use fastwarp_core::dot3;
    use fastwarp_matfile::mat5::read_mat5_file;
    use serde_json::json;

    use crate::ct_recon::{side_json, SIDE_KEYS_CONVENTION};
    use crate::template_contacts::{recompute_template_contacts, WarpedContacts};

    const BOSTON: &str = "Boston Scientific Vercise Directed";
    const MEDTRONIC: &str = "Medtronic 3387";
    const STAMP: &str = "2026-10-22T00:00:00Z";

    fn geometry(model: &str, head: [f64; 3], tilt: [f64; 3], anterior: [f64; 3]) -> SideGeometry {
        let e = electrode_by_name(model).unwrap();
        let tail = add3(head, scale3(unit3(tilt), e.fiducial_span_mm()));
        let u = unit3(sub3(tail, head));
        let y = unit3(sub3(anterior, scale3(u, dot3(anterior, u))));
        let x = cross3(y, u);
        let r = 0.5 * e.lead_diameter_mm;
        resolve_fiducials(
            [
                head,
                tail,
                add3(head, scale3(x, r)),
                add3(head, scale3(y, r)),
            ],
            e,
        )
        .unwrap()
    }

    fn sides(a: &SideGeometry, b: &SideGeometry) -> Value {
        json!({"1": side_json(a), "2": side_json(b)})
    }

    fn bilateral() -> Value {
        let n1 = geometry(BOSTON, [22.0, -4.0, 30.0], [0.1, 0.2, 1.0], [0.3, 1.0, 0.1]);
        let n2 = geometry(
            BOSTON,
            [-18.0, -5.0, 29.0],
            [-0.1, 0.1, 1.0],
            [-0.2, 1.0, 0.0],
        );
        let m1 = geometry(
            BOSTON,
            [12.0, -13.0, -8.0],
            [0.2, 0.3, 1.0],
            [0.3, 1.0, 0.0],
        );
        let m2 = geometry(
            BOSTON,
            [-11.5, -14.0, -9.0],
            [-0.2, 0.25, 1.0],
            [-0.2, 1.0, 0.0],
        );
        let mut reco = json!({
            "version": 2,
            "subj_id": "1",
            "method": "trac_core_rust_native",
            "mni": sides(&m1, &m2),
            "native": sides(&n1, &n2),
            "props": {
                "1": {"elmodel": BOSTON, "manually_corrected": false},
                "2": {"elmodel": BOSTON, "manually_corrected": false},
            },
            "meta": {
                "elmodel": BOSTON,
                "side_keys": SIDE_KEYS_CONVENTION,
                "template_space": REFERENCE_SPACE,
                "orientation": {
                    "1": {"status": "resolved", "roll_deg": 12.5, "flags": []},
                    "2": {"status": "failed", "roll_deg": null, "flags": []},
                },
            },
        });
        recompute_template_contacts(
            &mut reco,
            "mni",
            None,
            &|_| WarpedContacts::Omit,
            &mut |_| {},
        );
        reco
    }

    fn export(reco: &Value) -> RecoMat {
        reco_mat_from_json(reco, "sub-1_desc-reconstruction.json", STAMP).unwrap()
    }

    fn reco_var(out: &RecoMat) -> &Mat5Value {
        assert_eq!(out.variables[0].0, RECO_VARIABLE);
        &out.variables[0].1
    }

    fn rows(v: &Mat5Value) -> Vec<[f64; 3]> {
        v.as_rows()
            .unwrap()
            .into_iter()
            .map(|r| [r[0], r[1], r[2]])
            .collect()
    }

    fn marker(block: &Mat5Value, index: usize, name: &str) -> [f64; 3] {
        let m = block
            .field("markers", 0)
            .unwrap()
            .field(name, index)
            .unwrap();
        assert_eq!(m.dims(), vec![1, 3]);
        rows(m)[0]
    }

    fn close(a: &[[f64; 3]], b: &[[f64; 3]], tol: f64) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(p, q)| p.iter().zip(q).all(|(u, v)| (u - v).abs() <= tol))
    }

    fn json_rows(v: &Value) -> Vec<[f64; 3]> {
        points(v).unwrap()
    }

    fn assert_block_matches_json(mat: &Mat5Value, json_block: &Value, keys: [&str; 2]) {
        for (index, key) in keys.iter().enumerate() {
            let side = &json_block[*key];
            let coords = mat.field("coords_mm", 0).unwrap().item(index).unwrap();
            assert!(close(&rows(coords), &json_rows(&side["coords_mm"]), 1e-9));
            let traj = mat.field("trajectory", 0).unwrap().item(index).unwrap();
            assert_eq!(traj.dims(), vec![MAT_TRAJECTORY_POINTS, 3]);
            assert!(close(&rows(traj), &json_rows(&side["trajectory"]), 1e-9));
            for name in MARKER_FIELDS {
                let want = xyz(&side["markers"][name]).unwrap();
                assert!(close(&[marker(mat, index, name)], &[want], 1e-9), "{name}");
            }
        }
    }

    fn write_fixture_if_asked(reco: &Value, out: &RecoMat) {
        let Some(dir) = std::env::var_os("DBSLOC_RECO_MAT_FIXTURE_OUT") else {
            return;
        };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = mat5_bytes(&out.variables, true).unwrap();
        std::fs::write(dir.join("bilateral.mat"), bytes).unwrap();
        let text = serde_json::to_string(reco).unwrap() + "\n";
        std::fs::write(dir.join("bilateral.json"), text).unwrap();
    }

    #[test]
    fn reco_mat_bilateral_layout_classes_and_values() {
        let reco = bilateral();
        let out = export(&reco);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        write_fixture_if_asked(&reco, &out);
        let r = reco_var(&out);
        let Mat5Value::Struct { dims, fields, .. } = r else {
            panic!("reco is not a struct");
        };
        assert_eq!(dims, &vec![1, 1]);
        assert_eq!(fields, &["props", "native", "mni"]);
        let props = r.field("props", 0).unwrap();
        assert_eq!(props.dims(), vec![1, 2]);
        assert_eq!(props.field("elmodel", 0), Some(&Mat5Value::char(BOSTON)));
        assert_eq!(
            props.field("manually_corrected", 0),
            Some(&Mat5Value::scalar(1.0))
        );
        assert_eq!(
            props.field("manually_corrected", 1),
            Some(&Mat5Value::scalar(0.0))
        );
        let native = r.field("native", 0).unwrap();
        let mni = r.field("mni", 0).unwrap();
        assert!(native.field("trajvector", 0).is_none());
        for block in [native, mni] {
            for f in ["coords_mm", "trajectory", "markers"] {
                assert_eq!(block.field(f, 0).unwrap().dims(), vec![1, 2]);
            }
            for i in 0..2 {
                let c = block.field("coords_mm", 0).unwrap().item(i).unwrap();
                assert_eq!(c.dims(), vec![8, 3]);
            }
        }
        assert_block_matches_json(native, &reco["native"], ["1", "2"]);
        assert_block_matches_json(mni, &reco["mni"], ["1", "2"]);
        let e = electrode_by_name(BOSTON).unwrap();
        let r_lead = 0.5 * e.lead_diameter_mm;
        for block in [native, mni] {
            for i in 0..2 {
                let [h, t, x, y] = MARKER_FIELDS.map(|n| marker(block, i, n));
                assert!((norm3(sub3(x, h)) - r_lead).abs() < 1e-6);
                assert!((norm3(sub3(y, h)) - r_lead).abs() < 1e-6);
                let contacts = contacts_from_markers(e, h, t, x, y).unwrap();
                let stored = rows(block.field("coords_mm", 0).unwrap().item(i).unwrap());
                assert!(close(&contacts, &stored, 1e-6));
            }
        }
        for i in 0..2 {
            let tv = mni.field("trajvector", 0).unwrap().item(i).unwrap();
            assert_eq!(tv.dims(), vec![1, 3]);
            let [h, t] = ["head", "tail"].map(|n| marker(mni, i, n));
            assert!(close(&rows(tv), &[unit3(sub3(t, h))], 1e-12));
        }
        let prov = &out.variables[1];
        assert_eq!(prov.0, PROVENANCE_VARIABLE);
        assert_eq!(
            prov.1.field("source_json", 0),
            Some(&Mat5Value::char("sub-1_desc-reconstruction.json"))
        );
        assert_eq!(prov.1.field("written_at", 0), Some(&Mat5Value::char(STAMP)));
        assert_eq!(
            prov.1.field("side_keys", 0),
            Some(&Mat5Value::char(SIDE_KEYS_CONVENTION))
        );
    }

    #[test]
    fn reco_mat_file_round_trips_through_the_reader() {
        let reco = bilateral();
        let dir = std::env::temp_dir().join(format!("dbsloc_reco_mat_{}", std::process::id()));
        let json_path = dir.join("sub-1_desc-reconstruction.json");
        let mat_path = reco_mat_path(&json_path);
        assert_eq!(
            mat_path.file_name().unwrap(),
            "sub-1_desc-reconstruction.mat"
        );
        let warnings = write_reco_mat(&reco, &mat_path).unwrap();
        assert!(warnings.is_empty());
        let vars = read_mat5_file(&mat_path).unwrap();
        let names: Vec<&str> = vars.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, [RECO_VARIABLE, PROVENANCE_VARIABLE]);
        assert_eq!(vars[0].1, *reco_var(&export(&reco)));
        assert_eq!(
            vars[1].1.field("source_json", 0),
            Some(&Mat5Value::char("sub-1_desc-reconstruction.json"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reco_mat_left_only_fills_side_one_with_empties_and_a_numeric_zero() {
        let mut reco = bilateral();
        for block in ["native", "mni"] {
            reco[block].as_object_mut().unwrap().remove("1");
        }
        reco["props"].as_object_mut().unwrap().remove("1");
        reco["meta"]["orientation"]
            .as_object_mut()
            .unwrap()
            .remove("1");
        let out = export(&reco);
        let r = reco_var(&out);
        let props = r.field("props", 0).unwrap();
        assert_eq!(props.dims(), vec![1, 2]);
        assert_eq!(
            props.field("manually_corrected", 0),
            Some(&Mat5Value::scalar(0.0))
        );
        assert_eq!(props.field("elmodel", 0), Some(&Mat5Value::char(BOSTON)));
        for block in ["native", "mni"] {
            let b = r.field(block, 0).unwrap();
            assert_eq!(b.field("coords_mm", 0).unwrap().dims(), vec![1, 2]);
            assert_eq!(
                b.field("coords_mm", 0).unwrap().item(0),
                Some(&Mat5Value::Empty)
            );
            assert_eq!(
                b.field("trajectory", 0).unwrap().item(0),
                Some(&Mat5Value::Empty)
            );
            let markers = b.field("markers", 0).unwrap();
            for name in MARKER_FIELDS {
                assert_eq!(markers.field(name, 0), Some(&Mat5Value::Empty));
            }
            assert_block_matches_json_side(b, &reco[block]["2"], 1);
        }
        let tv = r.field("mni", 0).unwrap().field("trajvector", 0).unwrap();
        assert_eq!(tv.item(0), Some(&Mat5Value::Empty));
    }

    fn assert_block_matches_json_side(mat: &Mat5Value, side: &Value, index: usize) {
        let coords = mat.field("coords_mm", 0).unwrap().item(index).unwrap();
        assert!(close(&rows(coords), &json_rows(&side["coords_mm"]), 1e-9));
        assert!(close(
            &[marker(mat, index, "head")],
            &[xyz(&side["markers"]["head"]).unwrap()],
            1e-12
        ));
    }

    #[test]
    fn reco_mat_right_only_is_one_by_one() {
        let mut reco = bilateral();
        for block in ["native", "mni"] {
            reco[block].as_object_mut().unwrap().remove("2");
        }
        let r = export(&reco);
        let r = reco_var(&r);
        assert_eq!(r.field("props", 0).unwrap().dims(), vec![1, 1]);
        assert_eq!(
            r.field("mni", 0)
                .unwrap()
                .field("markers", 0)
                .unwrap()
                .dims(),
            vec![1, 1]
        );
    }

    #[test]
    fn reco_mat_non_reference_space_exports_the_reference_block_or_refuses() {
        let mut reco = bilateral();
        reco["meta"]["template_space"] = json!("MNI152NLin6Asym");
        let err = reco_mat_from_json(&reco, "x.json", STAMP).unwrap_err();
        assert!(err.contains("no reference block"), "{err}");
        let shift = [0.5, -1.0, 2.0];
        let mut reference = json!({"space": REFERENCE_SPACE});
        for key in ["1", "2"] {
            let mut side = reco["mni"][key].clone();
            for f in ["coords_mm", "trajectory"] {
                let moved: Vec<[f64; 3]> = json_rows(&side[f])
                    .iter()
                    .map(|p| add3(*p, shift))
                    .collect();
                side[f] = json!(moved);
            }
            for name in MARKER_FIELDS {
                side["markers"][name] = json!(add3(xyz(&side["markers"][name]).unwrap(), shift));
            }
            reference[key] = side;
        }
        reco["reference"] = reference;
        let out = export(&reco);
        let mni = reco_var(&out).field("mni", 0).unwrap();
        assert_block_matches_json(mni, &reco["reference"], ["1", "2"]);
        assert_eq!(
            out.variables[1].1.field("mni_source", 0),
            Some(&Mat5Value::char("reference"))
        );
        assert_eq!(
            out.variables[1].1.field("template_space", 0),
            Some(&Mat5Value::char("MNI152NLin6Asym"))
        );
        reco["reference"]["space"] = json!("MNI152NLin6Asym");
        assert!(reco_mat_from_json(&reco, "x.json", STAMP).is_err());
    }

    #[test]
    fn reco_mat_pacer_per_level_rows_are_re_resolved_and_the_trajectory_rebuilt() {
        let mut reco = bilateral();
        let e = electrode_by_name(BOSTON).unwrap();
        let side = &reco["native"]["1"];
        let [h, t, x, y] = MARKER_FIELDS.map(|n| xyz(&side["markers"][n]).unwrap());
        let u = unit3(sub3(t, h));
        let levels: Vec<[f64; 3]> = e
            .level_z_mm
            .iter()
            .map(|z| add3(h, scale3(u, *z)))
            .collect();
        let short: Vec<[f64; 3]> = (0..20).map(|i| add3(h, scale3(u, i as f64))).collect();
        reco["native"]["1"]["coords_mm"] = json!(levels);
        reco["native"]["1"]["trajectory"] = json!(short);
        let out = export(&reco);
        assert_eq!(out.warnings.len(), 2, "{:?}", out.warnings);
        assert!(out.warnings[0].contains("4 contact rows re-resolved"));
        assert!(out.warnings[1].contains("trajectory of 20 points"));
        let native = reco_var(&out).field("native", 0).unwrap();
        let coords = rows(native.field("coords_mm", 0).unwrap().item(0).unwrap());
        assert!(close(
            &coords,
            &contacts_from_markers(e, h, t, x, y).unwrap(),
            1e-12
        ));
        let traj = rows(native.field("trajectory", 0).unwrap().item(0).unwrap());
        assert_eq!(traj.len(), MAT_TRAJECTORY_POINTS);
        let a1 = sub3(h, scale3(u, 5.0));
        let a2 = add3(t, scale3(u, 2.0 * e.array_span_mm()));
        assert!(close(&[traj[0], traj[49]], &[a1, a2], 1e-9));
    }

    #[test]
    fn reco_mat_trajectory_extension_uses_one_and_a_half_spans_beyond_eight_levels() {
        let e = electrode_by_name(BOSTON).unwrap();
        assert!(e.n_levels() <= 8);
        let traj = synthesize_trajectory([0.0; 3], [0.0, 0.0, 6.0], e).unwrap();
        let end = 6.0 + 2.0 * 7.5;
        assert!((traj[49][2] - end).abs() < 1e-12);
        assert!((traj[0][2] + 5.0).abs() < 1e-12);
        let hx = electrode_by_name("Boston Scientific Vercise Cartesia HX").unwrap();
        assert_eq!(hx.n_levels(), 8);
        let traj = synthesize_trajectory([0.0; 3], [0.0, 0.0, 6.0], hx).unwrap();
        assert!((traj[49][2] - (6.0 + 2.0 * 15.5)).abs() < 1e-9);
        let long = dbsloc_reconstruct::electrode_model_names()
            .into_iter()
            .filter_map(electrode_by_name)
            .find(|m| m.n_levels() > 8)
            .unwrap();
        let traj = synthesize_trajectory([0.0; 3], [0.0, 0.0, 6.0], long).unwrap();
        assert!((traj[49][2] - (6.0 + 1.5 * long.array_span_mm())).abs() < 1e-9);
    }

    #[test]
    fn reco_mat_converts_an_old_convention_tail_on_read() {
        const HX: &str = "Boston Scientific Vercise Cartesia HX";
        let e = electrode_by_name(HX).unwrap();
        let mut reco = bilateral();
        let n1 = geometry(HX, [22.0, -4.0, 30.0], [0.1, 0.2, 1.0], [0.3, 1.0, 0.1]);
        let m1 = geometry(HX, [12.0, -13.0, -8.0], [0.2, 0.3, 1.0], [0.3, 1.0, 0.0]);
        let mut heads = Vec::new();
        for (block, g) in [("native", &n1), ("mni", &m1)] {
            let mut side = side_json(g);
            let u = unit3(sub3(g.tail, g.head));
            side["markers"]["tail"] = json!(add3(g.head, scale3(u, e.last_level_span_mm())));
            reco[block]["1"] = side;
            heads.push((g.head, u));
        }
        reco["props"]["1"]["elmodel"] = json!(HX);
        assert!(reco["meta"].get("tail_fiducial").is_none());
        let out = export(&reco);
        assert!(
            out.warnings.iter().any(|w| w
                == "dbsloc: warning: reconstruction: side 1: tail marker moved from the last to the 4th contact level (file written before 2026-11-14)"),
            "{:?}",
            out.warnings
        );
        let var = reco_var(&out);
        for (block, (head, u)) in ["native", "mni"].into_iter().zip(heads) {
            let tail = marker(var.field(block, 0).unwrap(), 0, "tail");
            let want = add3(head, scale3(u, 6.0));
            assert!(
                close(&[tail], &[want], 1e-9),
                "{block}: {tail:?} vs {want:?}"
            );
        }
        assert_eq!(
            reco["native"]["1"]["markers"]["tail"],
            json!(add3(n1.head, scale3(unit3(sub3(n1.tail, n1.head)), 14.0)))
        );
    }

    #[test]
    fn reco_mat_manually_corrected_follows_the_specs_12_rule() {
        let boston = electrode_by_name(BOSTON);
        let ring = electrode_by_name(MEDTRONIC);
        let base = json!({"props": {"1": {"manually_corrected": false}}, "meta": {}});
        assert!(!manually_corrected_flag(&base, "1", boston));
        let mut v = base.clone();
        v["props"]["1"]["manually_corrected"] = json!(true);
        assert!(manually_corrected_flag(&v, "1", boston));
        let mut v = base.clone();
        v["meta"]["manual_correction"] = json!({"1": {"review": "accepted"}});
        assert!(manually_corrected_flag(&v, "1", boston));
        v["meta"]["manual_correction"] = json!({"1": {"review": "corrected"}});
        assert!(manually_corrected_flag(&v, "1", ring));
        for status in ["resolved", "ambiguous", "manual"] {
            let mut v = base.clone();
            v["meta"]["orientation"] = json!({"1": {"status": status}});
            assert!(manually_corrected_flag(&v, "1", boston), "{status}");
            assert!(!manually_corrected_flag(&v, "1", ring), "{status}");
        }
        for orientation in [json!({"1": {"status": "failed"}}), json!("unresolved: …")] {
            let mut v = base.clone();
            v["meta"]["orientation"] = orientation;
            assert!(!manually_corrected_flag(&v, "1", boston));
        }
        assert!(!manually_corrected_flag(&base, "2", boston));
    }

    #[test]
    fn reco_mat_rescales_x_and_y_to_the_lead_radius_with_a_warning() {
        let mut reco = bilateral();
        let h = xyz(&reco["native"]["2"]["markers"]["head"]).unwrap();
        let x = xyz(&reco["native"]["2"]["markers"]["x"]).unwrap();
        reco["native"]["2"]["markers"]["x"] = json!(add3(h, scale3(sub3(x, h), 1.5)));
        let out = export(&reco);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("native side 2 (left): |x - head|"));
        let native = reco_var(&out).field("native", 0).unwrap();
        assert!(close(&[marker(native, 1, "x")], &[x], 1e-12));
    }

    #[test]
    fn reco_mat_without_side_keys_resolves_hemispheres_by_mean_x() {
        let mut reco = bilateral();
        reco["meta"].as_object_mut().unwrap().remove("side_keys");
        for block in ["native", "mni", "props"] {
            let b = reco[block].as_object_mut().unwrap();
            let one = b.remove("1").unwrap();
            let two = b.remove("2").unwrap();
            b.insert("1".into(), two);
            b.insert("2".into(), one);
        }
        let orientation = reco["meta"]["orientation"].as_object_mut().unwrap();
        let one = orientation.remove("1").unwrap();
        orientation.insert("2".into(), one);
        let out = export(&reco);
        let r = reco_var(&out);
        let mni = r.field("mni", 0).unwrap();
        assert!(marker(mni, 0, "head")[0] > 0.0);
        assert!(marker(mni, 1, "head")[0] < 0.0);
        assert_block_matches_json(mni, &reco["mni"], ["2", "1"]);
        assert_block_matches_json(r.field("native", 0).unwrap(), &reco["native"], ["2", "1"]);
        let props = r.field("props", 0).unwrap();
        assert_eq!(
            props.field("manually_corrected", 0),
            Some(&Mat5Value::scalar(1.0))
        );
        assert_eq!(
            out.variables[1].1.field("side_keys", 0),
            Some(&Mat5Value::char("resolved by mean x: 2=right,1=left"))
        );
    }

    #[test]
    fn reco_mat_template_only_file_omits_native_with_a_note() {
        let mut reco = bilateral();
        reco.as_object_mut().unwrap().remove("native");
        let out = export(&reco);
        let Mat5Value::Struct { fields, .. } = reco_var(&out) else {
            panic!("reco is not a struct");
        };
        assert_eq!(fields, &["props", "mni"]);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("no native block"));
    }

    #[test]
    fn reco_mat_refuses_bad_files() {
        let mut reco = bilateral();
        reco["version"] = json!(1);
        assert!(reco_mat_from_json(&reco, "x", STAMP).is_err());
        let mut reco = bilateral();
        reco["props"]["1"]["elmodel"] = json!("No Such Lead");
        reco["meta"]["elmodel"] = json!("No Such Lead");
        assert!(reco_mat_from_json(&reco, "x", STAMP)
            .unwrap_err()
            .contains("not in the catalogue"));
        let mut reco = bilateral();
        reco["mni"]["1"]["markers"]["head"] = json!([1.0, "a", 2.0]);
        assert!(reco_mat_from_json(&reco, "x", STAMP).is_err());
        let mut reco = bilateral();
        reco["mni"] = json!({});
        reco["native"] = json!({});
        assert!(reco_mat_from_json(&reco, "x", STAMP)
            .unwrap_err()
            .contains("no lead sides"));
    }
}
