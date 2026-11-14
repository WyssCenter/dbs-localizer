// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{json, Value};

use crate::electrode_catalog::{by_name, ElectrodeGeometry};

pub const TAIL_FIDUCIAL_KEY: &str = "tail_fiducial";
pub const TAIL_FIDUCIAL_FOURTH_LEVEL: &str = "fourth_level";
pub const MARKER_BLOCKS: [&str; 4] = ["native", "mni", "reference", "scrf"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailConversion {
    pub side: String,
    pub warning: String,
}

fn xyz(v: &Value) -> Option<[f64; 3]> {
    let a = v.as_array()?;
    if a.len() != 3 {
        return None;
    }
    let p = [a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?];
    p.iter().all(|c| c.is_finite()).then_some(p)
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn side_model(
    reco: &Value,
    key: &str,
    fallback: Option<&str>,
) -> Option<&'static ElectrodeGeometry> {
    let name = reco["props"][key]["elmodel"]
        .as_str()
        .or_else(|| reco["meta"]["elmodel"].as_str())
        .or(fallback)?;
    by_name(name)
}

fn head_tail(reco: &Value, block: &str, key: &str) -> Option<([f64; 3], [f64; 3])> {
    let m = &reco[block][key]["markers"];
    Some((xyz(&m["head"])?, xyz(&m["tail"])?))
}

pub fn tail_conversion_warning(side: &str) -> String {
    format!(
        "dbsloc: warning: reconstruction: side {side}: tail marker moved from the last to the 4th contact level (file written before 2026-11-14)"
    )
}

pub fn has_tail_fiducial(reco: &Value) -> bool {
    reco["meta"].get(TAIL_FIDUCIAL_KEY).is_some()
}

pub fn convert_legacy_tail(reco: &mut Value, fallback_model: Option<&str>) -> Vec<TailConversion> {
    if !reco.is_object() || has_tail_fiducial(reco) {
        return Vec::new();
    }
    let mut keys: Vec<String> = Vec::new();
    for block in ["native", "mni"] {
        if let Some(sides) = reco.get(block).and_then(Value::as_object) {
            for (k, v) in sides {
                if v.is_object() && !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
        }
    }
    let mut out = Vec::new();
    for key in keys {
        let Some(model) = side_model(reco, &key, fallback_model) else {
            continue;
        };
        if model.n_levels() <= 4 {
            continue;
        }
        let last = model.last_level_span_mm();
        let fiducial = model.fiducial_span_mm();
        if !(last.is_finite() && last > 1e-9 && fiducial.is_finite()) {
            continue;
        }
        let Some((head, tail)) =
            head_tail(reco, "native", &key).or_else(|| head_tail(reco, "mni", &key))
        else {
            continue;
        };
        let d = dist(head, tail);
        if (d - last).abs() >= (d - fiducial).abs() {
            continue;
        }
        let ratio = fiducial / last;
        for block in MARKER_BLOCKS {
            let Some((head, tail)) = head_tail(reco, block, &key) else {
                continue;
            };
            let new_tail = [
                head[0] + ratio * (tail[0] - head[0]),
                head[1] + ratio * (tail[1] - head[1]),
                head[2] + ratio * (tail[2] - head[2]),
            ];
            let trajectory = model.trajectory(head, new_tail);
            let Some(side) = reco
                .get_mut(block)
                .and_then(|b| b.get_mut(&key))
                .and_then(Value::as_object_mut)
            else {
                continue;
            };
            if let Some(markers) = side.get_mut("markers").and_then(Value::as_object_mut) {
                markers.insert("tail".into(), json!(new_tail));
            }
            if let (Some(t), true) = (trajectory, side.contains_key("trajectory")) {
                side.insert("trajectory".into(), json!(t));
            }
        }
        out.push(TailConversion {
            warning: tail_conversion_warning(&key),
            side: key,
        });
    }
    out
}

pub fn mark_tail_fiducial(reco: &mut Value) {
    let Some(obj) = reco.as_object_mut() else {
        return;
    };
    let meta = obj.entry("meta").or_insert_with(|| json!({}));
    if let Some(meta) = meta.as_object_mut() {
        meta.insert(TAIL_FIDUCIAL_KEY.into(), json!(TAIL_FIDUCIAL_FOURTH_LEVEL));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const HX: &str = "Boston Scientific Vercise Cartesia HX";

    fn side(head: [f64; 3], tail: [f64; 3], model: &ElectrodeGeometry) -> Value {
        json!({
            "coords_mm": [[1.0, 2.0, 3.0]],
            "trajectory": model.trajectory(head, tail).unwrap(),
            "markers": {"head": head, "tail": tail, "x": [head[0] + 0.65, head[1], head[2]],
                        "y": [head[0], head[1] + 0.65, head[2]]},
            "trajvector": [0.0, 0.0, 1.0],
        })
    }

    fn file(model: &str, span_native: f64, span_mni: f64) -> Value {
        let e = by_name(model).unwrap();
        let hn = [10.0, -5.0, -3.0];
        let hm = [12.0, -12.0, -8.0];
        json!({
            "version": 2,
            "props": {"1": {"elmodel": model}},
            "meta": {"elmodel": model},
            "native": {"1": side(hn, [hn[0], hn[1], hn[2] + span_native], e)},
            "mni": {"1": side(hm, [hm[0], hm[1], hm[2] + span_mni], e)},
        })
    }

    fn tail(reco: &Value, block: &str) -> [f64; 3] {
        xyz(&reco[block]["1"]["markers"]["tail"]).unwrap()
    }

    #[test]
    fn old_convention_hx_file_moves_the_tail_to_the_fourth_level() {
        let e = by_name(HX).unwrap();
        let mut reco = file(HX, 14.0, 15.4);
        let before = reco.clone();
        let got = convert_legacy_tail(&mut reco, None);
        assert_eq!(
            got,
            vec![TailConversion {
                side: "1".into(),
                warning: "dbsloc: warning: reconstruction: side 1: tail marker moved from the last to the 4th contact level (file written before 2026-11-14)".into(),
            }]
        );
        let tn = tail(&reco, "native");
        assert!((tn[2] - (-3.0 + 6.0)).abs() < 1e-9, "{tn:?}");
        let tm = tail(&reco, "mni");
        assert!((tm[2] - (-8.0 + 15.4 * 6.0 / 14.0)).abs() < 1e-9, "{tm:?}");
        let traj = reco["native"]["1"]["trajectory"].as_array().unwrap();
        let end = xyz(&traj[49]).unwrap();
        assert!((end[2] - (3.0 + 2.0 * e.array_span_mm())).abs() < 1e-9);
        for block in ["native", "mni"] {
            for k in ["coords_mm", "trajvector"] {
                assert_eq!(reco[block]["1"][k], before[block]["1"][k]);
            }
            for k in ["head", "x", "y"] {
                assert_eq!(
                    reco[block]["1"]["markers"][k],
                    before[block]["1"]["markers"][k]
                );
            }
        }
        assert!(!has_tail_fiducial(&reco));
        assert!(convert_legacy_tail(&mut reco, None).is_empty());
    }

    #[test]
    fn template_only_old_convention_file_decides_from_the_mni_block() {
        let mut reco = file(HX, 14.0, 14.0);
        reco.as_object_mut().unwrap().remove("native");
        let got = convert_legacy_tail(&mut reco, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].warning, tail_conversion_warning("1"));
        let tm = tail(&reco, "mni");
        assert!((tm[2] - (-8.0 + 6.0)).abs() < 1e-9, "{tm:?}");

        let mut reference_style = file(HX, 6.0, 6.2);
        reference_style.as_object_mut().unwrap().remove("native");
        let before = reference_style.clone();
        assert!(convert_legacy_tail(&mut reference_style, None).is_empty());
        assert_eq!(reference_style, before);
    }

    #[test]
    fn a_side_missing_from_the_native_block_decides_from_the_mni_block() {
        let e = by_name(HX).unwrap();
        let mut reco = file(HX, 6.0, 14.0);
        let h = [-12.0, -12.0, -8.0];
        reco["mni"]["2"] = side(h, [h[0], h[1], h[2] + 14.0], e);
        reco["props"]["2"] = json!({"elmodel": HX});
        let got = convert_legacy_tail(&mut reco, None);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].side, "2");
        let t2 = xyz(&reco["mni"]["2"]["markers"]["tail"]).unwrap();
        assert!((t2[2] - (-8.0 + 6.0)).abs() < 1e-9, "{t2:?}");
        let t1 = tail(&reco, "mni");
        assert!(
            (t1[2] - (-8.0 + 14.0)).abs() < 1e-9,
            "side 1 decided from native: {t1:?}"
        );
    }

    #[test]
    fn reference_style_file_is_used_unchanged() {
        let mut reco = file(HX, 6.1, 6.6);
        let before = reco.clone();
        assert!(convert_legacy_tail(&mut reco, None).is_empty());
        assert_eq!(reco, before);
    }

    #[test]
    fn four_level_models_are_never_converted() {
        for model in ["Boston Scientific Vercise Directed", "Medtronic 3387"] {
            let mut reco = file(model, 3.0, 3.0);
            let before = reco.clone();
            assert!(convert_legacy_tail(&mut reco, None).is_empty(), "{model}");
            assert_eq!(reco, before, "{model}");
        }
    }

    #[test]
    fn a_file_with_the_field_is_never_converted() {
        let mut reco = file(HX, 14.0, 14.0);
        mark_tail_fiducial(&mut reco);
        assert_eq!(reco["meta"]["tail_fiducial"], json!("fourth_level"));
        assert_eq!(reco["meta"]["elmodel"], json!(HX));
        let before = reco.clone();
        assert!(convert_legacy_tail(&mut reco, None).is_empty());
        assert_eq!(reco, before);
    }

    #[test]
    fn the_model_falls_back_to_the_caller() {
        let mut reco = file(HX, 14.0, 14.0);
        reco.as_object_mut().unwrap().remove("props");
        reco["meta"].as_object_mut().unwrap().remove("elmodel");
        assert!(convert_legacy_tail(&mut reco.clone(), None).is_empty());
        assert_eq!(convert_legacy_tail(&mut reco, Some(HX)).len(), 1);
    }

    #[test]
    fn mark_adds_meta_when_missing() {
        let mut reco = json!({"version": 2});
        mark_tail_fiducial(&mut reco);
        assert_eq!(reco["meta"]["tail_fiducial"], json!("fourth_level"));
    }
}
