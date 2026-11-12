// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Atlas sets the 3D viewer can show (`specs/11` §1): bundled Lead-DBS-layout folders, folders
//! of a user-chosen Lead-DBS atlases directory, and the bundled AHEAD masks. Every set is in
//! [`ATLAS_SPACE`]; other template spaces reach it through the space bridge.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::atlas_index::{AtlasIndex, ATLAS_INDEX_FILE};
use crate::bids::simplify_verbatim;

pub const LEADDBS_ATLASES_ENV: &str = "DBSLOC_LEADDBS_ATLASES_DIR";
pub const ATLAS_SPACE: &str = "MNI152NLin2009bAsym";
pub const DEFAULT_ATLAS_NAME: &str = "DISTAL Minimal (Ewert 2017)";
pub const AHEAD_ATLAS_ID: &str = "ahead";
pub const AHEAD_ATLAS_NAME: &str = "AHEAD basal ganglia (Alkemade 2020)";
pub const AHEAD_DEFAULT_ON: [&str; 8] = [
    "STN",
    "GPi",
    "GPe",
    "RN",
    "SN",
    "subthalamic",
    "pallidus",
    "red",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AtlasLayout {
    /// `lh/`, `rh/`, `mixed/`, `midline/` subfolders and an optional `atlas_index.mat`.
    Lead,
    /// One directory of mask NIfTI files (bundled AHEAD).
    Flat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AtlasSetEntry {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    pub layout: AtlasLayout,
    pub index: Option<AtlasIndex>,
    pub default_on_substrings: Option<Vec<String>>,
    pub citation: Option<String>,
}

/// The `DBSLOC_LEADDBS_ATLASES_DIR` value, when set and not empty.
pub fn external_atlases_dir_from_env() -> Option<PathBuf> {
    std::env::var_os(LEADDBS_ATLASES_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Atlas sets under the bundled `resources/` (found like the bundled binaries) and under
/// `external_dir`, or `DBSLOC_LEADDBS_ATLASES_DIR` when `external_dir` is `None`.
pub fn discover_atlas_sets(
    external_dir: Option<&Path>,
    log: &mut dyn FnMut(String),
) -> Vec<AtlasSetEntry> {
    let external = external_dir
        .map(Path::to_path_buf)
        .or_else(external_atlases_dir_from_env);
    bundled_and_external_atlas_sets(external.as_deref(), log)
}

/// Atlas sets under the bundled `resources/`, the downloaded atlases of the per-user data folder
/// and `external_dir`; reads no Lead-DBS atlases variable (a run resolves the directory in
/// `RunOptions::apply_env`).
pub fn bundled_and_external_atlas_sets(
    external_dir: Option<&Path>,
    log: &mut dyn FnMut(String),
) -> Vec<AtlasSetEntry> {
    let resources = crate::bundled_bin::find_resources_root().map(|r| r.join("resources"));
    let user = crate::leaddbs_data::user_atlases_dir_opt();
    atlas_sets(resources.as_deref(), user.as_deref(), external_dir, log)
}

/// Atlas sets in this order (`specs/11` §10.5): `<resources>/atlases/leaddbs/*` (id
/// `leaddbs:<folder>`), the downloaded folders of `user_dir` (id `user:<folder>`, skipped when it
/// is the external root), the folders of `external_dir` (id `external:<folder>`), then the flat
/// AHEAD directory (id `ahead`). A name found in two roots keeps both ids.
/// Each Lead-DBS folder's `atlas_index.mat` is parsed when present; a file that fails to parse is
/// reported through `log` and the set is kept without an index.
pub fn atlas_sets(
    resources: Option<&Path>,
    user_dir: Option<&Path>,
    external_dir: Option<&Path>,
    log: &mut dyn FnMut(String),
) -> Vec<AtlasSetEntry> {
    let mut out = Vec::new();
    let external = external_dir.and_then(|dir| match external_root(dir) {
        Ok(root) => Some(root),
        Err(e) => {
            log(format!("atlas: Lead-DBS atlases directory: {e}"));
            None
        }
    });
    if let Some(res) = resources {
        let bundled = res.join("atlases").join("leaddbs");
        if bundled.is_dir() {
            out.extend(lead_sets(&bundled, "leaddbs", log));
        }
    }
    if let Some(user) = user_dir.filter(|u| u.is_dir()) {
        let canon = std::fs::canonicalize(user).map(simplify_verbatim).ok();
        if canon.is_none() || canon != external {
            out.extend(lead_sets(user, "user", log));
        }
    }
    if let Some(root) = &external {
        out.extend(lead_sets(root, "external", log));
    }
    if let Some(res) = resources {
        let ahead = res.join("atlases").join("ahead").join("structures_mni09b");
        if ahead.is_dir() {
            out.push(AtlasSetEntry {
                id: AHEAD_ATLAS_ID.to_owned(),
                name: AHEAD_ATLAS_NAME.to_owned(),
                dir: ahead,
                layout: AtlasLayout::Flat,
                index: None,
                default_on_substrings: Some(
                    AHEAD_DEFAULT_ON.iter().map(|s| (*s).to_owned()).collect(),
                ),
                citation: Some("Alkemade et al. 2020".to_owned()),
            });
        }
    }
    out
}

/// The default set (`specs/11` §1.3): `DISTAL Minimal (Ewert 2017)` when listed (bundled before
/// external), else AHEAD.
pub fn default_atlas(entries: &[AtlasSetEntry]) -> Option<&AtlasSetEntry> {
    entries
        .iter()
        .find(|e| e.layout == AtlasLayout::Lead && e.name == DEFAULT_ATLAS_NAME)
        .or_else(|| entries.iter().find(|e| e.id == AHEAD_ATLAS_ID))
}

/// Scene-manifest `atlas_lead_sets` entries for `entries`.
pub fn atlas_lead_sets_json(entries: &[AtlasSetEntry]) -> Vec<Value> {
    entries
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "dir": e.dir.to_string_lossy(),
                "id": e.id,
                "layout": e.layout,
                "default_on_substrings": e.default_on_substrings,
                "index": e.index,
            })
        })
        .collect()
}

fn external_root(dir: &Path) -> Result<PathBuf, String> {
    if dir.components().any(|c| c == Component::ParentDir) {
        return Err(format!("{} contains `..`", dir.display()));
    }
    let dir = crate::paths::expand_user(dir);
    let canon = std::fs::canonicalize(&dir)
        .map(simplify_verbatim)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    if !canon.is_dir() {
        return Err(format!("{} is not a directory", canon.display()));
    }
    let nested = canon
        .join("templates")
        .join("space")
        .join(ATLAS_SPACE)
        .join("atlases");
    Ok(if nested.is_dir() { nested } else { canon })
}

fn lead_sets(root: &Path, prefix: &str, log: &mut dyn FnMut(String)) -> Vec<AtlasSetEntry> {
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) => {
            log(format!("atlas: {}: {e}", root.display()));
            return Vec::new();
        }
    };
    let mut dirs: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_owned();
            let path = e.path();
            (!name.starts_with('.') && path.is_dir()).then_some((name, path))
        })
        .collect();
    dirs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    dirs.into_iter()
        .map(|(name, dir)| {
            let index_path = dir.join(ATLAS_INDEX_FILE);
            let index = if index_path.is_file() {
                match AtlasIndex::read(&index_path) {
                    Ok(i) => Some(i),
                    Err(e) => {
                        log(format!(
                            "atlas {name}: {} ignored: {e}",
                            index_path.display()
                        ));
                        None
                    }
                }
            } else {
                None
            };
            let citation = index
                .as_ref()
                .and_then(|i| i.citation_name.clone().or_else(|| i.citation_short.clone()));
            AtlasSetEntry {
                id: format!("{prefix}:{name}"),
                name,
                dir,
                layout: AtlasLayout::Lead,
                index,
                default_on_substrings: None,
                citation,
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn resources() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")
    }

    fn distal_present() -> bool {
        let ok = resources()
            .join("atlases/leaddbs")
            .join(DEFAULT_ATLAS_NAME)
            .join(ATLAS_INDEX_FILE)
            .is_file();
        if !ok {
            eprintln!("skipped: bundled {DEFAULT_ATLAS_NAME} is absent");
        }
        ok
    }

    #[test]
    fn atlas_catalog_lists_bundled_sets_and_defaults_to_distal_minimal() {
        if !distal_present() {
            return;
        }
        let mut logs = Vec::new();
        let sets = atlas_sets(Some(&resources()), None, None, &mut |m| logs.push(m));
        assert!(logs.is_empty(), "{logs:?}");
        let def = default_atlas(&sets).unwrap();
        assert_eq!(def.id, format!("leaddbs:{DEFAULT_ATLAS_NAME}"));
        assert_eq!(def.layout, AtlasLayout::Lead);
        assert_eq!(
            def.citation.as_deref(),
            Some("DISTAL Minimal Atlas (Ewert 2017)")
        );
        assert_eq!(def.index.as_ref().unwrap().structures.len(), 15);
        let ahead = sets.last().unwrap();
        assert_eq!(ahead.id, AHEAD_ATLAS_ID);
        assert_eq!(ahead.layout, AtlasLayout::Flat);
        assert!(ahead.index.is_none());
    }

    #[test]
    fn atlas_catalog_json_has_the_manifest_shape() {
        if !distal_present() {
            return;
        }
        let sets = atlas_sets(Some(&resources()), None, None, &mut |_| {});
        let json = atlas_lead_sets_json(&sets);
        let d = json
            .iter()
            .find(|v| v["name"] == DEFAULT_ATLAS_NAME)
            .unwrap();
        assert_eq!(d["id"], format!("leaddbs:{DEFAULT_ATLAS_NAME}"));
        assert_eq!(d["layout"], "lead");
        assert!(d["default_on_substrings"].is_null());
        assert!(d["dir"].as_str().unwrap().ends_with(DEFAULT_ATLAS_NAME));
        let idx = &d["index"];
        assert_eq!(idx["version"], 2.1);
        assert_eq!(idx["citation_name"], "DISTAL Minimal Atlas (Ewert 2017)");
        assert_eq!(idx["citation_short"], "Ewert et al. 2017");
        assert_eq!(idx["citation_long"].as_array().unwrap().len(), 2);
        let stn = &idx["structures"][0];
        assert_eq!(stn["file"], "STN.nii.gz");
        assert_eq!(stn["name"], "STN");
        assert_eq!(stn["kind"], 3);
        assert_eq!(stn["tissue_type"], 1);
        assert_eq!(
            stn["sides"][0],
            json!({"side": "right", "tag": "STN_right", "color": [1.0, 0.5, 0.0], "alpha": 0.7, "threshold": 0.4})
        );
        assert_eq!(stn["sides"][1]["side"], "left");
        assert_eq!(
            idx["presets"][0],
            json!({"label": "Primary DBS targets", "show": [0, 4, 13, 14], "hide": [1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12], "mode": "absolute"})
        );
        assert_eq!(idx["default_preset"], 0);
        assert_eq!(idx["labels"][0]["name"], "Abbreviations");
        assert_eq!(idx["skipped"], json!([]));
        let ahead = json.iter().find(|v| v["id"] == AHEAD_ATLAS_ID).unwrap();
        assert_eq!(ahead["layout"], "flat");
        assert!(ahead["index"].is_null());
        assert_eq!(ahead["default_on_substrings"][0], "STN");
    }

    #[test]
    fn atlas_catalog_reads_an_external_directory_and_survives_a_bad_index() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("atlases");
        std::fs::create_dir_all(root.join("B atlas/rh")).unwrap();
        std::fs::create_dir_all(root.join("A atlas/lh")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("A atlas").join(ATLAS_INDEX_FILE), b"garbage").unwrap();
        std::fs::write(root.join("loose.nii.gz"), b"").unwrap();
        let mut logs = Vec::new();
        let sets = atlas_sets(None, None, Some(&root), &mut |m| logs.push(m));
        let ids: Vec<&str> = sets.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["external:A atlas", "external:B atlas"]);
        assert!(sets.iter().all(|s| s.index.is_none()));
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert!(logs[0].contains("A atlas"));
        assert!(default_atlas(&sets).is_none());
    }

    #[test]
    fn atlas_catalog_finds_the_space_folder_of_a_leaddbs_install() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp
            .path()
            .join("templates/space")
            .join(ATLAS_SPACE)
            .join("atlases")
            .join(DEFAULT_ATLAS_NAME);
        std::fs::create_dir_all(&nested).unwrap();
        let sets = atlas_sets(None, None, Some(tmp.path()), &mut |_| {});
        assert_eq!(sets.len(), 1);
        assert_eq!(default_atlas(&sets).unwrap().name, DEFAULT_ATLAS_NAME);
    }

    #[test]
    fn atlas_catalog_rejects_parent_components_and_missing_dirs() {
        let mut logs = Vec::new();
        let sets = atlas_sets(None, None, Some(Path::new("../x")), &mut |m| logs.push(m));
        assert!(sets.is_empty());
        let _ = atlas_sets(
            None,
            None,
            Some(Path::new("/definitely/not/here")),
            &mut |m| logs.push(m),
        );
        assert_eq!(logs.len(), 2, "{logs:?}");
    }

    fn lead_folder(root: &Path, name: &str) {
        std::fs::create_dir_all(root.join(name).join("lh")).unwrap();
    }

    #[test]
    fn atlas_catalog_lists_downloaded_atlases_as_user_sets() {
        let tmp = tempfile::tempdir().unwrap();
        let resources = tmp.path().join("resources");
        lead_folder(&resources.join("atlases/leaddbs"), "Shipped (A 2020)");
        let user = tmp.path().join("data/atlases/leaddbs");
        lead_folder(&user, "Fetched (B 2021)");
        lead_folder(&user, ".staging-x-1");
        let external = tmp.path().join("leaddbs-install");
        lead_folder(&external, "Local (C 2022)");
        let mut logs = Vec::new();
        let sets = atlas_sets(Some(&resources), Some(&user), Some(&external), &mut |m| {
            logs.push(m)
        });
        let ids: Vec<&str> = sets.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "leaddbs:Shipped (A 2020)",
                "user:Fetched (B 2021)",
                "external:Local (C 2022)"
            ]
        );
        assert!(logs.is_empty(), "{logs:?}");
        let missing = tmp.path().join("nowhere");
        let sets = atlas_sets(None, Some(&missing), None, &mut |m| logs.push(m));
        assert!(sets.is_empty());
        assert!(logs.is_empty(), "{logs:?}");
    }

    #[test]
    fn atlas_catalog_lists_the_user_root_once_when_it_is_the_external_root() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("data/atlases/leaddbs");
        lead_folder(&user, "Fetched (B 2021)");
        let sets = atlas_sets(None, Some(&user), Some(&user), &mut |_| {});
        let ids: Vec<&str> = sets.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["external:Fetched (B 2021)"]);
    }

    #[test]
    fn atlas_catalog_keeps_every_id_of_a_name_found_in_several_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let resources = tmp.path().join("resources");
        lead_folder(&resources.join("atlases/leaddbs"), "Same (A 2020)");
        let user = tmp.path().join("data/atlases/leaddbs");
        lead_folder(&user, "Same (A 2020)");
        lead_folder(&user, "Other (D 2023)");
        let external = tmp.path().join("ext");
        lead_folder(&external, "Same (A 2020)");
        lead_folder(&external, "Other (D 2023)");
        let mut logs = Vec::new();
        let sets = atlas_sets(Some(&resources), Some(&user), Some(&external), &mut |m| {
            logs.push(m)
        });
        let ids: Vec<&str> = sets.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "leaddbs:Same (A 2020)",
                "user:Other (D 2023)",
                "user:Same (A 2020)",
                "external:Other (D 2023)",
                "external:Same (A 2020)"
            ]
        );
        assert!(logs.is_empty(), "{logs:?}");
    }
}
