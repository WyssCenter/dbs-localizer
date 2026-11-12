// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{json, Map, Value};

pub struct Summary<'a> {
    pub plan: &'a Value,
    pub apply: bool,
    pub planned: &'a [String],
    pub rows: &'a [(String, String, f64)],
    pub profile: bool,
    pub interrupted: bool,
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

pub fn any_error(rows: &[(String, String, f64)]) -> bool {
    rows.iter().any(|(_, r, _)| r.starts_with("error:"))
}

pub fn total_seconds(rows: &[(String, String, f64)]) -> f64 {
    rows.iter().map(|r| r.2).sum()
}

pub fn build(s: &Summary<'_>) -> Value {
    let lip = s.plan.get("lock_input_paths").filter(|v| !v.is_null());
    let fingerprint = lip
        .and_then(Value::as_object)
        .is_some_and(|m| !m.is_empty());
    let mut out = Map::new();
    out.insert(
        "subj_id".into(),
        s.plan.get("subj_id").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "output_dir".into(),
        s.plan.get("output_dir").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "locks_dir".into(),
        s.plan.get("locks_dir").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "warnings".into(),
        s.plan
            .get("warnings")
            .filter(|v| v.is_array())
            .cloned()
            .unwrap_or_else(|| json!([])),
    );
    out.insert("fingerprint_locks".into(), json!(fingerprint));
    out.insert("apply".into(), json!(s.apply));
    out.insert("stages".into(), json!(s.planned));
    out.insert(
        "results".into(),
        Value::Array(
            s.rows
                .iter()
                .map(|(st, r, dt)| json!({"stage": st, "result": r, "seconds": round3(*dt)}))
                .collect(),
        ),
    );
    if s.profile {
        out.insert("total_seconds".into(), json!(round3(total_seconds(s.rows))));
        let timings: Map<String, Value> = s
            .rows
            .iter()
            .map(|(st, _, dt)| (st.clone(), json!(round3(*dt))))
            .collect();
        out.insert("stage_timings_sec".into(), Value::Object(timings));
    }
    if let Some(l) = lip {
        out.insert("lock_input_paths".into(), l.clone());
    }
    if s.interrupted {
        out.insert("interrupted".into(), json!(true));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<(String, String, f64)> {
        vec![
            ("bids".into(), "plan:bids".into(), 0.12345),
            ("coreg".into(), "error: x".into(), 1.0),
        ]
    }

    #[test]
    fn keys_and_order() {
        let plan = json!({"subj_id": "1", "locks_dir": "/l", "lock_input_paths": null});
        let planned = vec!["bids".to_owned(), "coreg".to_owned()];
        let r = rows();
        let v = build(&Summary {
            plan: &plan,
            apply: false,
            planned: &planned,
            rows: &r,
            profile: false,
            interrupted: false,
        });
        let keys: Vec<&String> = v
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        assert_eq!(
            keys,
            [
                "subj_id",
                "output_dir",
                "locks_dir",
                "warnings",
                "fingerprint_locks",
                "apply",
                "stages",
                "results"
            ]
        );
        assert_eq!(v["results"][0]["seconds"], json!(0.123));
        assert_eq!(v["fingerprint_locks"], json!(false));
        assert_eq!(v["warnings"], json!([]));
        assert!(any_error(&r));
    }

    #[test]
    fn profile_lock_inputs_and_interrupt() {
        let plan = json!({"subj_id": "1", "locks_dir": "/l", "lock_input_paths": {"bids": ["/a"]}});
        let planned = vec!["bids".to_owned()];
        let r = rows();
        let v = build(&Summary {
            plan: &plan,
            apply: true,
            planned: &planned,
            rows: &r,
            profile: true,
            interrupted: true,
        });
        assert_eq!(v["fingerprint_locks"], json!(true));
        assert_eq!(v["total_seconds"], json!(1.123));
        assert_eq!(v["stage_timings_sec"]["coreg"], json!(1.0));
        assert_eq!(v["lock_input_paths"]["bids"][0], json!("/a"));
        assert_eq!(v["interrupted"], json!(true));
    }

    #[test]
    fn plan_warnings_are_copied() {
        let plan = json!({"subj_id": "1", "locks_dir": "/l", "warnings": ["x"]});
        let planned = vec!["bids".to_owned()];
        let r = rows();
        let v = build(&Summary {
            plan: &plan,
            apply: false,
            planned: &planned,
            rows: &r,
            profile: false,
            interrupted: false,
        });
        assert_eq!(v["warnings"], json!(["x"]));
    }
}
