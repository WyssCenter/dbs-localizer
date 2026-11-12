// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceDef {
    pub name: String,
    pub norm_mapping: Vec<(String, String)>,
    pub misfit_template: String,
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl SpaceDef {
    pub fn from_json(d: &Value) -> Result<SpaceDef, String> {
        let obj = d
            .as_object()
            .ok_or_else(|| "spacedef root must be a JSON object".to_owned())?;
        let name = obj
            .get("name")
            .or_else(|| obj.get("spacedef_name"))
            .map_or_else(|| "unknown".to_owned(), text);
        let norm_mapping = obj
            .get("norm_mapping")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), text(v))).collect())
            .unwrap_or_default();
        let misfit_template = obj
            .get("misfit_template")
            .map_or_else(|| "t1".to_owned(), text);
        Ok(SpaceDef {
            name,
            norm_mapping,
            misfit_template,
        })
    }
}

pub fn load(path: &Path) -> Result<Option<SpaceDef>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let d: Value =
        serde_json::from_str(&raw).map_err(|e| format!("{}: invalid JSON: {e}", path.display()))?;
    SpaceDef::from_json(&d)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn primary_template_stem(anchor_key: &str, sp: &SpaceDef) -> String {
    let mut a = anchor_key;
    for strip in ["iso_", "ax_", "r_"] {
        if a.get(..strip.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(strip))
        {
            a = &a[strip.len()..];
            break;
        }
    }
    if let Some((_, v)) = sp.norm_mapping.iter().find(|(k, _)| k == a) {
        return v.clone();
    }
    for (k, v) in &sp.norm_mapping {
        if a.ends_with(k.as_str()) || a.contains(k.as_str()) {
            return v.clone();
        }
    }
    sp.misfit_template.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sp() -> SpaceDef {
        SpaceDef::from_json(&json!({
            "name": "MNI",
            "norm_mapping": {"T1w": "t1", "T2w": "t2"},
            "misfit_template": "t1"
        }))
        .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn exact_and_prefixed_keys_map() {
        assert_eq!(primary_template_stem("T2w", &sp()), "t2");
        assert_eq!(primary_template_stem("iso_T2w", &sp()), "t2");
        assert_eq!(primary_template_stem("ISO_T1w", &sp()), "t1");
    }

    #[test]
    fn substring_and_misfit_fallbacks() {
        assert_eq!(primary_template_stem("acq-x_T2w", &sp()), "t2");
        assert_eq!(primary_template_stem("FLAIR", &sp()), "t1");
    }

    #[test]
    fn name_falls_back_to_spacedef_name_then_unknown() {
        let a =
            SpaceDef::from_json(&json!({"spacedef_name": "X"})).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a.name, "X");
        let b = SpaceDef::from_json(&json!({})).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(b.name, "unknown");
        assert_eq!(b.misfit_template, "t1");
    }

    #[test]
    fn non_object_root_is_rejected() {
        assert!(SpaceDef::from_json(&json!([1])).is_err());
    }
}
