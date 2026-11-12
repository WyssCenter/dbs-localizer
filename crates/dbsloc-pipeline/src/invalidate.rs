// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use serde_json::{json, Value};

use crate::bids::Subject;
use crate::checkreg::{
    default_brainshift_method_log_path, default_coreg_method_log_path,
    default_norm_method_log_path, method_log_allows_continue, set_approval,
};
use crate::locks::{approval_flag, read_approval, stage_approval_path};
use crate::outputs::write_replacing;
use crate::stages::Stage;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Invalidation {
    pub changed: Vec<Stage>,
    pub reviews_reset: Vec<Stage>,
    pub warnings: Vec<String>,
}

impl Invalidation {
    pub fn log_line(&self, completed: Stage) -> Option<String> {
        self.summary(&format!("{} re-ran", completed.cli_name()))
    }

    pub fn summary(&self, cause: &str) -> Option<String> {
        if self.changed.is_empty() && self.reviews_reset.is_empty() {
            return None;
        }
        let join = |stages: &[Stage]| {
            stages
                .iter()
                .map(|s| s.cli_name())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut line = format!("dbsloc: {cause};");
        if !self.changed.is_empty() {
            line.push_str(&format!(" approvals invalidated: {}", join(&self.changed)));
        }
        if !self.reviews_reset.is_empty() {
            if !self.changed.is_empty() {
                line.push(';');
            }
            line.push_str(&format!(" reviews reset: {}", join(&self.reviews_reset)));
        }
        Some(line)
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn invalidate_lock(path: &Path, by: &str, at: f64) -> Result<bool, String> {
    let Some(Value::Object(mut obj)) = read_approval(path)? else {
        return Ok(false);
    };
    if !approval_flag(&Value::Object(obj.clone())) {
        return Ok(false);
    }
    let mut set = false;
    for key in ["approval", "approved"] {
        if let Some(v) = obj.get_mut(key) {
            *v = json!(false);
            set = true;
        }
    }
    if !set {
        obj.insert("approval".into(), json!(false));
    }
    obj.remove("invalidated_by");
    obj.remove("invalidated_at");
    obj.insert("invalidated_by".into(), json!(by));
    obj.insert("invalidated_at".into(), json!(at));
    let text = serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| e.to_string())?;
    write_replacing(path, (text + "\n").as_bytes())
        .map_err(|e| format!("cannot rewrite {}: {e}", path.display()))?;
    Ok(true)
}

#[derive(Clone, Copy, PartialEq)]
enum Cleared {
    Nothing,
    Approval,
    ReviewOnly,
}

fn clear_method_log(path: &Path, stage: Stage) -> Result<Cleared, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Cleared::Nothing);
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return Ok(Cleared::Nothing);
    };
    let was_approved = method_log_allows_continue(path);
    if !was_approved && root.get("reviewed").is_none() {
        return Ok(Cleared::Nothing);
    }
    let branch = match stage {
        Stage::Coregister => "coreg",
        Stage::Normalize => "norm",
        _ => "brainshift",
    };
    let scalar_branch = if stage == Stage::Coregister {
        "norm"
    } else {
        branch
    };
    let io = |e: std::io::Error| format!("cannot rewrite {}: {e}", path.display());
    match root.get("approval").or_else(|| root.get("approved")) {
        Some(Value::Object(m)) if stage == Stage::Coregister => {
            for key in m.keys() {
                set_approval(path, branch, Some(key), false, false).map_err(io)?;
            }
        }
        _ => set_approval(path, scalar_branch, None, false, false).map_err(io)?,
    }
    Ok(if was_approved {
        Cleared::Approval
    } else {
        Cleared::ReviewOnly
    })
}

pub fn invalidate_stage_lock(
    sub: &Subject,
    stage: Stage,
    invalidated_by: &str,
) -> Result<bool, String> {
    invalidate_lock(
        &stage_approval_path(&sub.locks_dir(), stage.cli_name()),
        invalidated_by,
        now_secs(),
    )
}

pub fn invalidate_dependents(sub: &Subject, completed: Stage) -> Invalidation {
    invalidate_dependents_as(sub, completed, completed.cli_name())
}

pub fn invalidate_dependents_as(
    sub: &Subject,
    completed: Stage,
    invalidated_by: &str,
) -> Invalidation {
    invalidate_stages(sub, &completed.invalidated_dependents(), invalidated_by)
}

pub fn invalidate_from(sub: &Subject, first: &[Stage], invalidated_by: &str) -> Invalidation {
    let stages: Vec<Stage> = Stage::default_sequence()
        .iter()
        .copied()
        .filter(|s| {
            first
                .iter()
                .any(|f| f == s || f.invalidated_dependents().contains(s))
        })
        .collect();
    invalidate_stages(sub, &stages, invalidated_by)
}

fn invalidate_stages(sub: &Subject, stages: &[Stage], invalidated_by: &str) -> Invalidation {
    let at = now_secs();
    let locks = sub.locks_dir();
    let mut out = Invalidation::default();
    for &stage in stages {
        let mut touched = false;
        let mut review_only = false;
        match invalidate_lock(
            &stage_approval_path(&locks, stage.cli_name()),
            invalidated_by,
            at,
        ) {
            Ok(t) => touched |= t,
            Err(e) => out.warnings.push(format!("dbsloc: warning: {e}")),
        }
        let mlog = match stage {
            Stage::Coregister => Some(default_coreg_method_log_path(sub)),
            Stage::Normalize => Some(default_norm_method_log_path(sub)),
            Stage::Brainshift => Some(default_brainshift_method_log_path(sub)),
            _ => None,
        };
        if let Some(m) = mlog {
            match clear_method_log(&m, stage) {
                Ok(Cleared::Approval) => touched = true,
                Ok(Cleared::ReviewOnly) => review_only = true,
                Ok(Cleared::Nothing) => {}
                Err(e) => out.warnings.push(format!("dbsloc: warning: {e}")),
            }
        }
        if touched {
            out.changed.push(stage);
        } else if review_only {
            out.reviews_reset.push(stage);
        }
    }
    out
}
