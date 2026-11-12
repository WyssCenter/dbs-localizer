// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::paths::absolute;
use crate::template_catalog::{
    self, default_entry, entry_dir, expected_t1_path_in, infer_space, lookup, TemplateEntry,
};

pub const TEMPLATE_ENV: &str = "DBSLOC_TEMPLATE_T1";
pub const DEFAULT_TEMPLATE_SPACE: &str = template_catalog::REFERENCE_SPACE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateResolution {
    pub path: PathBuf,
    pub source: &'static str,
    pub space: String,
}

fn shipped_base(root: Option<&Path>) -> Option<PathBuf> {
    root.map(Path::to_path_buf)
        .or_else(template_catalog::shipped_root)
}

pub fn default_template_candidates(root: Option<&Path>) -> Vec<PathBuf> {
    let Some(base) = shipped_base(root) else {
        return Vec::new();
    };
    let d = entry_dir(&base.join("resources"), default_entry());
    vec![d.join("t1.nii.gz"), d.join("t1.nii")]
}

pub fn template_help_text(root: Option<&Path>) -> String {
    let cands = default_template_candidates(root);
    let where_ = match cands.first() {
        Some(c) => c.display().to_string(),
        None => format!(
            "resources/templates/space/{DEFAULT_TEMPLATE_SPACE}/res-1mm/t1.nii.gz \
             (Wyss DBS Localizer installation)"
        ),
    };
    format!(
        "no normalization template: the shipped {DEFAULT_TEMPLATE_SPACE} template is missing at \
         {where_}; pass --template <path|id>, or set {TEMPLATE_ENV}=<path|id>; \
         `dbsloc template list` shows the catalogue ids and what is installed"
    )
}

fn resolve_entry(
    e: &'static TemplateEntry,
    source: &'static str,
    root: Option<&Path>,
    data_override: Option<&str>,
) -> TemplateResolution {
    let shipped = shipped_base(root);
    let data = template_catalog::user_data_dir_with(data_override);
    TemplateResolution {
        path: absolute(&expected_t1_path_in(e, shipped.as_deref(), data.as_deref())),
        source,
        space: e.space.to_owned(),
    }
}

fn resolve_value(
    raw: &str,
    source: &'static str,
    root: Option<&Path>,
    data_override: Option<&str>,
) -> TemplateResolution {
    if let Some(e) = lookup(raw) {
        return resolve_entry(e, source, root, data_override);
    }
    let path = absolute(Path::new(raw));
    let space = infer_space(&path);
    TemplateResolution {
        path,
        source,
        space,
    }
}

pub fn catalogue_entry_for_path(
    path: &Path,
    root: Option<&Path>,
    data_override: Option<&str>,
) -> Option<&'static TemplateEntry> {
    let shipped = shipped_base(root);
    let data = template_catalog::user_data_dir_with(data_override);
    let want = absolute(path);
    template_catalog::catalogue()
        .iter()
        .find(|e| absolute(&expected_t1_path_in(e, shipped.as_deref(), data.as_deref())) == want)
}

pub fn select_template_with(
    explicit: Option<&Path>,
    env: &HashMap<String, String>,
    root: Option<&Path>,
) -> Option<TemplateResolution> {
    let data_override = env.get(template_catalog::DATA_DIR_ENV).map(String::as_str);
    if let Some(p) = explicit.filter(|p| !p.as_os_str().to_string_lossy().trim().is_empty()) {
        return Some(resolve_value(
            p.to_string_lossy().trim(),
            "--template",
            root,
            data_override,
        ));
    }
    if let Some(raw) = env
        .get(TEMPLATE_ENV)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return Some(resolve_value(raw, TEMPLATE_ENV, root, data_override));
    }
    default_template_candidates(root)
        .into_iter()
        .find(|c| c.is_file())
        .map(|c| TemplateResolution {
            path: absolute(&c),
            source: "resources/templates",
            space: default_entry().space.to_owned(),
        })
}

pub fn select_template(explicit: Option<&Path>) -> Option<TemplateResolution> {
    let env: HashMap<String, String> = [TEMPLATE_ENV, template_catalog::DATA_DIR_ENV]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_owned(), v)))
        .collect();
    select_template_with(explicit, &env, None)
}

pub const TEMPLATE_T2_ENV: &str = "DBSLOC_TEMPLATE_T2";
pub const TEMPLATE_T2_SIBLING: &str = "beside the T1 template";

pub fn t2_sibling_candidates(t1: &Path) -> Vec<PathBuf> {
    let Some(dir) = t1.parent() else {
        return Vec::new();
    };
    vec![dir.join("t2.nii.gz"), dir.join("t2.nii")]
}

pub fn select_template_t2_with(
    explicit: Option<&Path>,
    env: &HashMap<String, String>,
    t1: Option<&Path>,
) -> Option<TemplateResolution> {
    if let Some(p) = explicit.filter(|p| !p.as_os_str().to_string_lossy().trim().is_empty()) {
        return Some(TemplateResolution {
            path: absolute(p),
            source: "--template-t2",
            space: String::new(),
        });
    }
    if let Some(raw) = env
        .get(TEMPLATE_T2_ENV)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return Some(TemplateResolution {
            path: absolute(Path::new(raw)),
            source: TEMPLATE_T2_ENV,
            space: String::new(),
        });
    }
    t2_sibling_candidates(t1?)
        .into_iter()
        .find(|c| c.is_file())
        .map(|c| TemplateResolution {
            space: String::new(),
            path: absolute(&c),
            source: TEMPLATE_T2_SIBLING,
        })
}

pub fn select_template_t2(
    explicit: Option<&Path>,
    t1: Option<&Path>,
) -> Option<TemplateResolution> {
    let env: HashMap<String, String> = std::env::var(TEMPLATE_T2_ENV)
        .ok()
        .map(|v| (TEMPLATE_T2_ENV.to_owned(), v))
        .into_iter()
        .collect();
    select_template_t2_with(explicit, &env, t1)
}

pub fn missing_template_t2_message(path: &Path) -> String {
    format!(
        "T2 template {} does not exist or is not a file; an explicitly given T2 template \
         (--template-t2 or {TEMPLATE_T2_ENV}) is never replaced by another source; pass \
         --no-norm-multispectral to normalize on the T1w channel only",
        path.display()
    )
}

pub fn missing_template_message(path: Option<&Path>, root: Option<&Path>) -> String {
    let data = std::env::var(template_catalog::DATA_DIR_ENV).ok();
    missing_template_message_with(path, root, data.as_deref())
}

pub fn missing_template_message_with(
    path: Option<&Path>,
    root: Option<&Path>,
    data_override: Option<&str>,
) -> String {
    match path {
        None => template_help_text(root),
        Some(p) => match catalogue_entry_for_path(p, root, data_override) {
            Some(e)
                if !p.is_file()
                    && matches!(e.source, template_catalog::TemplateSource::Shipped) =>
            {
                format!(
                    "template {} is missing from this installation ({}); reinstall Wyss DBS \
                     Localizer or pass --template <path|id>",
                    e.id,
                    p.display()
                )
            }
            Some(e) if !p.is_file() => format!(
                "template {} is not installed; run: dbsloc template fetch {}",
                e.id, e.id
            ),
            _ => format!(
                "template {} does not exist or is not a file; an explicitly given template \
             (--template or DBSLOC_TEMPLATE_T1) is never replaced by another source",
                p.display()
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(v: Option<&str>) -> HashMap<String, String> {
        v.map(|s| (TEMPLATE_ENV.to_owned(), s.to_owned()))
            .into_iter()
            .collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_tpl_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn explicit_wins_even_when_missing() {
        let root = scratch("explicit");
        let missing = root.join("nope.nii.gz");
        let r =
            select_template_with(Some(&missing), &env_with(Some("x.nii")), Some(&root)).unwrap();
        assert_eq!(r.source, "--template");
        assert!(r.path.ends_with("nope.nii.gz"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn env_beats_in_tree_default() {
        let root = scratch("env");
        let d = root.join("resources/templates/space/MNI152NLin2009bAsym/res-1mm");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("t1.nii.gz"), b"x").unwrap();
        let env_tpl = root.join("other.nii");
        let r = select_template_with(
            None,
            &env_with(Some(&env_tpl.to_string_lossy())),
            Some(&root),
        )
        .unwrap();
        assert_eq!(r.source, TEMPLATE_ENV);
        let r = select_template_with(None, &env_with(Some("  ")), Some(&root)).unwrap();
        assert_eq!(r.source, "resources/templates");
        assert!(r.path.ends_with("t1.nii.gz"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn in_tree_default_prefers_gz_then_nii() {
        let root = scratch("nii");
        let d = root.join("resources/templates/space/MNI152NLin2009bAsym/res-1mm");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("t1.nii"), b"x").unwrap();
        let r = select_template_with(None, &env_with(None), Some(&root)).unwrap();
        assert!(r.path.ends_with("t1.nii"));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn env_t2(v: Option<&str>) -> HashMap<String, String> {
        v.map(|s| (TEMPLATE_T2_ENV.to_owned(), s.to_owned()))
            .into_iter()
            .collect()
    }

    #[test]
    fn t2_template_precedence_flag_then_env_then_sibling() {
        let root = scratch("t2prec");
        let t1 = root.join("t1.nii");
        std::fs::write(&t1, b"x").unwrap();
        assert!(select_template_t2_with(None, &env_t2(None), Some(&t1)).is_none());
        std::fs::write(root.join("t2.nii"), b"x").unwrap();
        let r = select_template_t2_with(None, &env_t2(None), Some(&t1)).unwrap();
        assert_eq!(r.source, TEMPLATE_T2_SIBLING);
        assert!(r.path.ends_with("t2.nii"));
        std::fs::write(root.join("t2.nii.gz"), b"x").unwrap();
        let r = select_template_t2_with(None, &env_t2(Some("  ")), Some(&t1)).unwrap();
        assert!(r.path.ends_with("t2.nii.gz"), "{:?}", r.path);
        let env_tpl = root.join("env_t2.nii");
        let r = select_template_t2_with(None, &env_t2(Some(&env_tpl.to_string_lossy())), Some(&t1))
            .unwrap();
        assert_eq!(r.source, TEMPLATE_T2_ENV);
        assert!(r.path.ends_with("env_t2.nii"));
        let flag = root.join("flag_t2.nii");
        let r = select_template_t2_with(
            Some(&flag),
            &env_t2(Some(&env_tpl.to_string_lossy())),
            Some(&t1),
        )
        .unwrap();
        assert_eq!(r.source, "--template-t2");
        assert!(
            r.path.ends_with("flag_t2.nii"),
            "missing explicit path is kept"
        );
        assert!(select_template_t2_with(None, &env_t2(None), None).is_none());
        assert!(missing_template_t2_message(&flag).contains("--no-norm-multispectral"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_configured_is_none_and_help_lists_all_sources() {
        let root = scratch("none");
        assert!(select_template_with(None, &env_with(None), Some(&root)).is_none());
        let msg = missing_template_message(None, Some(&root));
        assert!(msg.contains("--template"));
        assert!(msg.contains(TEMPLATE_ENV));
        assert!(msg.contains("t1.nii.gz"));
        assert!(msg.contains("--template <path|id>"));
        assert!(msg.contains("dbsloc template list"));
        assert!(msg.contains("res-1mm"));
        let missing = missing_template_message(Some(Path::new("/x/t.nii")), Some(&root));
        assert!(missing.contains("never replaced"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn catalogue_id_resolves_to_the_installed_entry_or_a_not_installed_error() {
        let root = scratch("cat_id");
        let data = root.join("data");
        let mut env = env_with(None);
        env.insert(
            template_catalog::DATA_DIR_ENV.to_owned(),
            data.to_string_lossy().into_owned(),
        );
        let id = "MNI152NLin2009bAsym_res-0.5mm";
        let r = select_template_with(Some(Path::new(id)), &env, Some(&root)).unwrap();
        assert_eq!(r.source, "--template");
        assert_eq!(r.space, "MNI152NLin2009bAsym");
        assert!(!r.path.is_file());
        let msg = missing_template_message_with(
            Some(&r.path),
            Some(&root),
            Some(&data.to_string_lossy()),
        );
        assert_eq!(
            msg,
            format!("template {id} is not installed; run: dbsloc template fetch {id}")
        );
        let d = entry_dir(&data, lookup(id).unwrap());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("t1.nii.gz"), b"x").unwrap();
        let r2 = select_template_with(Some(Path::new(id)), &env, Some(&root)).unwrap();
        assert!(r2.path.is_file());
        assert!(r2.path.ends_with("res-0.5mm/t1.nii.gz"));
        env.insert(TEMPLATE_ENV.to_owned(), id.to_owned());
        let r3 = select_template_with(None, &env, Some(&root)).unwrap();
        assert_eq!(r3.source, TEMPLATE_ENV);
        assert_eq!(r3.path, r2.path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_and_path_resolutions_carry_the_space() {
        let root = scratch("space");
        let d = root.join("resources/templates/space/MNI152NLin2009bAsym/res-1mm");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("t1.nii.gz"), b"x").unwrap();
        let r = select_template_with(None, &env_with(None), Some(&root)).unwrap();
        assert_eq!(r.space, "MNI152NLin2009bAsym");
        let other = root.join("mine.nii");
        std::fs::write(&other, b"hello").unwrap();
        let r = select_template_with(Some(&other), &env_with(None), Some(&root)).unwrap();
        assert!(r.space.starts_with("custom-"), "{}", r.space);
        let _ = std::fs::remove_dir_all(&root);
    }
}
