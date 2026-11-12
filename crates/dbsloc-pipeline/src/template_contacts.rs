// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{json, Map, Value};

use dbsloc_reconstruct::electrode_by_name;
use dbsloc_reconstruct::native_frame::contacts_from_markers;
use dbsloc_reconstruct::orientation::template_marker_directions;
use dbsloc_reconstruct::{add3, norm3, scale3, sub3};

use crate::ct_recon::SIDE_KEYS;

pub const TEMPLATE_CONTACTS_RULE: &str = "from_markers";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpedContacts {
    Capture,
    Keep,
    Omit,
}

fn as_xyz(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array()?;
    if a.len() < 3 {
        return None;
    }
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

fn model_name(root: &Value, key: &str, fallback: Option<&str>) -> Option<String> {
    root["props"][key]["elmodel"]
        .as_str()
        .or_else(|| root["meta"]["elmodel"].as_str())
        .or(fallback)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn flagged(root: &Value) -> bool {
    root["meta"]["template_contacts"].as_str() == Some(TEMPLATE_CONTACTS_RULE)
}

pub fn recompute_template_contacts(
    root: &mut Value,
    block: &str,
    fallback_model: Option<&str>,
    policy: &dyn Fn(&str) -> WarpedContacts,
    log: &mut dyn FnMut(String),
) {
    let was_flagged = flagged(root);
    let mut done = false;
    for key in SIDE_KEYS {
        if policy(key) == WarpedContacts::Capture {
            if let Some(obj) = root
                .get_mut(block)
                .and_then(|b| b.get_mut(key))
                .and_then(Value::as_object_mut)
            {
                if let Some(old) = obj.get("coords_mm").cloned() {
                    obj.insert("coords_mm_warped".into(), old);
                }
            }
        }
        let Some(side) = root.get(block).and_then(|b| b.get(key)) else {
            continue;
        };
        if !side.is_object() {
            continue;
        }
        let warn = |log: &mut dyn FnMut(String), why: &str| {
            log(format!(
                "dbsloc: warning: reconstruct: {block} side {key}: contacts kept as individually mapped points ({why})"
            ));
        };
        let Some(name) = model_name(root, key, fallback_model) else {
            warn(log, "no electrode model");
            continue;
        };
        let Some(electrode) = electrode_by_name(&name) else {
            warn(
                log,
                &format!("electrode model {name:?} not in the catalogue"),
            );
            continue;
        };
        let m = &side["markers"];
        let (Some(head), Some(tail), Some(x), Some(y)) = (
            as_xyz(&m["head"]),
            as_xyz(&m["tail"]),
            as_xyz(&m["x"]),
            as_xyz(&m["y"]),
        ) else {
            warn(log, "a marker is missing");
            continue;
        };
        let Some(contacts) = contacts_from_markers(electrode, head, tail, x, y) else {
            warn(log, "markers are degenerate");
            continue;
        };
        let Some(obj) = root
            .get_mut(block)
            .and_then(|b| b.get_mut(key))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        match policy(key) {
            WarpedContacts::Capture => {}
            WarpedContacts::Keep => {
                if !obj.contains_key("coords_mm_warped") && !was_flagged {
                    if let Some(old) = obj.get("coords_mm").cloned() {
                        obj.insert("coords_mm_warped".into(), old);
                    }
                }
            }
            WarpedContacts::Omit => {
                obj.remove("coords_mm_warped");
            }
        }
        obj.insert("coords_mm".into(), json!(contacts));
        done = true;
    }
    if done {
        mark_template_contacts(root);
    }
}

pub fn construct_template_markers(
    warped: &mut Map<String, Value>,
    source: &Map<String, Value>,
) -> Vec<&'static str> {
    let mut vertical = Vec::new();
    for key in SIDE_KEYS {
        let Some(from) = source.get(key).map(|s| &s["markers"]) else {
            continue;
        };
        let (Some(src_head), Some(src_y)) = (as_xyz(&from["head"]), as_xyz(&from["y"])) else {
            continue;
        };
        let Some(markers) = warped
            .get_mut(key)
            .and_then(|b| b.get_mut("markers"))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let (Some(head), Some(tail)) = (
            markers.get("head").and_then(as_xyz),
            markers.get("tail").and_then(as_xyz),
        ) else {
            continue;
        };
        let y_vec = sub3(src_y, src_head);
        let r = norm3(y_vec);
        if !(r.is_finite() && r > 1e-9) {
            continue;
        }
        match template_marker_directions(scale3(y_vec, 1.0 / r), head, tail) {
            Some((x_dir, y_dir)) => {
                markers.insert("x".into(), json!(add3(head, scale3(x_dir, r))));
                markers.insert("y".into(), json!(add3(head, scale3(y_dir, r))));
            }
            None => vertical.push(key),
        }
    }
    vertical
}

pub fn mark_template_contacts(root: &mut Value) {
    let Some(obj) = root.as_object_mut() else {
        return;
    };
    let meta = obj
        .entry("meta")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(meta) = meta.as_object_mut() {
        meta.insert("template_contacts".into(), json!(TEMPLATE_CONTACTS_RULE));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn side(coords: Value, y: [f64; 3]) -> Value {
        json!({
            "coords_mm": coords,
            "markers": {
                "head": [0.0, 0.0, 0.0],
                "tail": [0.0, 0.0, 9.0],
                "x": [0.65, 0.0, 0.0],
                "y": y,
            },
        })
    }

    fn reco() -> Value {
        let y = [0.0, 0.65, 0.0];
        json!({
            "props": {"1": {"elmodel": "Medtronic 3387"}},
            "meta": {},
            "mni": {"1": side(json!([[9.0, 9.0, 9.0]]), y)},
        })
    }

    #[test]
    fn contacts_replace_warped_points_and_keep_them() {
        let mut r = reco();
        let mut lines = Vec::new();
        recompute_template_contacts(
            &mut r,
            "mni",
            None,
            &|_| WarpedContacts::Capture,
            &mut |l| lines.push(l),
        );
        assert!(lines.is_empty());
        let rows = r["mni"]["1"]["coords_mm"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1], json!([0.0, 0.0, 3.0]));
        assert_eq!(r["mni"]["1"]["coords_mm_warped"], json!([[9.0, 9.0, 9.0]]));
        assert_eq!(r["meta"]["template_contacts"], "from_markers");
    }

    #[test]
    fn rerun_keeps_the_first_warped_points() {
        let mut r = reco();
        let mut sink = |_: String| {};
        recompute_template_contacts(&mut r, "mni", None, &|_| WarpedContacts::Capture, &mut sink);
        recompute_template_contacts(&mut r, "mni", None, &|_| WarpedContacts::Keep, &mut sink);
        assert_eq!(r["mni"]["1"]["coords_mm_warped"], json!([[9.0, 9.0, 9.0]]));
    }

    #[test]
    fn omit_drops_warped_points() {
        let mut r = reco();
        let mut sink = |_: String| {};
        recompute_template_contacts(&mut r, "mni", None, &|_| WarpedContacts::Capture, &mut sink);
        recompute_template_contacts(&mut r, "mni", None, &|_| WarpedContacts::Omit, &mut sink);
        assert!(r["mni"]["1"].get("coords_mm_warped").is_none());
    }

    #[test]
    fn a_fallback_keeps_the_mapped_contacts_for_the_later_orientation_recompute() {
        let good = [0.0, 0.65, 0.0];
        let mut r = reco();
        r["props"]["2"] = json!({"elmodel": "Medtronic 3387"});
        r["mni"]["2"] = side(json!([[7.0, 7.0, 7.0]]), good);
        r["mni"]["1"]["markers"]["y"] = json!([0.0, 0.0, 0.0]);
        let mut sink = |_: String| {};
        recompute_template_contacts(&mut r, "mni", None, &|_| WarpedContacts::Capture, &mut sink);
        assert_eq!(r["mni"]["1"]["coords_mm_warped"], json!([[9.0, 9.0, 9.0]]));
        assert_eq!(r["mni"]["1"]["coords_mm"], json!([[9.0, 9.0, 9.0]]));
        assert_eq!(r["meta"]["template_contacts"], "from_markers");
        r["mni"]["1"]["markers"]["y"] = json!([0.0, 0.65, 0.0]);
        recompute_template_contacts(&mut r, "mni", None, &|_| WarpedContacts::Keep, &mut sink);
        assert_eq!(r["mni"]["1"]["coords_mm"].as_array().unwrap().len(), 4);
        assert_eq!(r["mni"]["1"]["coords_mm_warped"], json!([[9.0, 9.0, 9.0]]));
    }

    #[test]
    fn degenerate_markers_keep_warped_contacts_and_warn() {
        let mut r = reco();
        r["mni"]["1"]["markers"]["tail"] = json!([0.0, 0.0, 0.0]);
        let mut lines = Vec::new();
        recompute_template_contacts(
            &mut r,
            "mni",
            None,
            &|_| WarpedContacts::Capture,
            &mut |l| lines.push(l),
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("degenerate"));
        assert_eq!(r["mni"]["1"]["coords_mm"], json!([[9.0, 9.0, 9.0]]));
        assert_eq!(r["mni"]["1"]["coords_mm_warped"], json!([[9.0, 9.0, 9.0]]));
        assert!(r["meta"].get("template_contacts").is_none());
    }
}
