// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

pub const REFERENCE_SPACE: &str = "MNI152NLin2009bAsym";
pub const DATA_DIR_ENV: &str = "DBSLOC_DATA_DIR";
pub const DEFAULT_ENTRY_ID: &str = "MNI152NLin2009bAsym_res-1mm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateSource {
    Shipped,
    Download {
        url: &'static str,
        zip_sha256: &'static str,
        zip_size: u64,
        t1_member: &'static str,
        t2_member: &'static str,
        licence_member: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemplateEntry {
    pub id: &'static str,
    pub space: &'static str,
    pub res_mm: f64,
    pub title: &'static str,
    pub source: TemplateSource,
}

impl TemplateEntry {
    pub fn res_label(&self) -> String {
        format!("{}mm", self.res_mm)
    }
}

pub const CATALOGUE: &[TemplateEntry] = &[
    TemplateEntry {
        id: "MNI152NLin2009bAsym_res-1mm",
        space: "MNI152NLin2009bAsym",
        res_mm: 1.0,
        title: "MNI 2009b asymmetric, 1.0 mm (bundled)",
        source: TemplateSource::Shipped,
    },
    TemplateEntry {
        id: "MNI152NLin2009bAsym_res-0.5mm",
        space: "MNI152NLin2009bAsym",
        res_mm: 0.5,
        title: "MNI 2009b asymmetric, 0.5 mm",
        source: TemplateSource::Download {
            url: "https://www.bic.mni.mcgill.ca/~vfonov/icbm/2009/mni_icbm152_nlin_asym_09b_nifti.zip",
            zip_sha256: "5759d292e531ad600dafd988955b014dead3156643361bec624793c41611c1ab",
            zip_size: 374_644_624,
            t1_member: "mni_icbm152_nlin_asym_09b/mni_icbm152_t1_tal_nlin_asym_09b_hires.nii",
            t2_member: "mni_icbm152_nlin_asym_09b/mni_icbm152_t2_tal_nlin_asym_09b_hires.nii",
            licence_member: "COPYING",
        },
    },
    TemplateEntry {
        id: "MNI152NLin2009bSym_res-0.5mm",
        space: "MNI152NLin2009bSym",
        res_mm: 0.5,
        title: "MNI 2009b symmetric, 0.5 mm",
        source: TemplateSource::Download {
            url: "https://www.bic.mni.mcgill.ca/~vfonov/icbm/2009/mni_icbm152_nlin_sym_09b_nifti.zip",
            zip_sha256: "acce3b85039176aaf7de2c3169272551ddfcae5d9a4e5ce642025b795f9f1d20",
            zip_size: 364_272_316,
            t1_member: "mni_icbm152_nlin_sym_09b/mni_icbm152_t1_tal_nlin_sym_09b_hires.nii",
            t2_member: "mni_icbm152_nlin_sym_09b/mni_icbm152_t2_tal_nlin_sym_09b_hires.nii",
            licence_member: "COPYING",
        },
    },
    TemplateEntry {
        id: "MNI152NLin2009aAsym_res-1mm",
        space: "MNI152NLin2009aAsym",
        res_mm: 1.0,
        title: "MNI 2009a asymmetric, 1.0 mm",
        source: TemplateSource::Download {
            url: "https://www.bic.mni.mcgill.ca/~vfonov/icbm/2009/mni_icbm152_nlin_asym_09a_nifti.zip",
            zip_sha256: "188e1706b0ed74a0d1b3a52ad1a2b198814815f70a65f3ded091b7ceb6296a44",
            zip_size: 59_587_750,
            t1_member: "mni_icbm152_nlin_asym_09a/mni_icbm152_t1_tal_nlin_asym_09a.nii",
            t2_member: "mni_icbm152_nlin_asym_09a/mni_icbm152_t2_tal_nlin_asym_09a.nii",
            licence_member: "COPYING",
        },
    },
    TemplateEntry {
        id: "MNI152NLin2009aSym_res-1mm",
        space: "MNI152NLin2009aSym",
        res_mm: 1.0,
        title: "MNI 2009a symmetric, 1.0 mm",
        source: TemplateSource::Download {
            url: "https://www.bic.mni.mcgill.ca/~vfonov/icbm/2009/mni_icbm152_nlin_sym_09a_nifti.zip",
            zip_sha256: "97068359213b5ec1b717f37a65ff260d1b8314954d8c6f573d3d047f574a838a",
            zip_size: 63_116_438,
            t1_member: "mni_icbm152_nlin_sym_09a/mni_icbm152_t1_tal_nlin_sym_09a.nii",
            t2_member: "mni_icbm152_nlin_sym_09a/mni_icbm152_t2_tal_nlin_sym_09a.nii",
            licence_member: "COPYING",
        },
    },
    TemplateEntry {
        id: "MNI152NLin2009cAsym_res-1mm",
        space: "MNI152NLin2009cAsym",
        res_mm: 1.0,
        title: "MNI 2009c asymmetric, 1.0 mm",
        source: TemplateSource::Download {
            url: "https://www.bic.mni.mcgill.ca/~vfonov/icbm/2009/mni_icbm152_nlin_asym_09c_nifti.zip",
            zip_sha256: "804382e5d68a42ff8096ee845a7600b6658434da762fa67840a149eb25d48bd4",
            zip_size: 58_602_393,
            t1_member: "mni_icbm152_nlin_asym_09c/mni_icbm152_t1_tal_nlin_asym_09c.nii",
            t2_member: "mni_icbm152_nlin_asym_09c/mni_icbm152_t2_tal_nlin_asym_09c.nii",
            licence_member: "COPYING",
        },
    },
    TemplateEntry {
        id: "MNI152NLin2009cSym_res-1mm",
        space: "MNI152NLin2009cSym",
        res_mm: 1.0,
        title: "MNI 2009c symmetric, 1.0 mm",
        source: TemplateSource::Download {
            url: "https://www.bic.mni.mcgill.ca/~vfonov/icbm/2009/mni_icbm152_nlin_sym_09c_nifti.zip",
            zip_sha256: "ed981b4611ddd797a22f539035979723ecb1badebd06dcd85df149b5ef00890d",
            zip_size: 57_583_823,
            t1_member: "mni_icbm152_nlin_sym_09c/mni_icbm152_t1_tal_nlin_sym_09c.nii",
            t2_member: "mni_icbm152_nlin_sym_09c/mni_icbm152_t2_tal_nlin_sym_09c.nii",
            licence_member: "COPYING",
        },
    },
];

pub fn catalogue() -> &'static [TemplateEntry] {
    CATALOGUE
}

pub fn default_entry() -> &'static TemplateEntry {
    &CATALOGUE[0]
}

pub fn lookup(id_or_space: &str) -> Option<&'static TemplateEntry> {
    let key = id_or_space.trim();
    if let Some(e) = CATALOGUE.iter().find(|e| e.id == key) {
        return Some(e);
    }
    if key == REFERENCE_SPACE {
        return Some(default_entry());
    }
    let mut hits = CATALOGUE.iter().filter(|e| e.space == key);
    match (hits.next(), hits.next()) {
        (Some(e), None) => Some(e),
        _ => None,
    }
}

pub fn user_data_dir_with(override_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(d) = override_dir.map(str::trim).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let nonempty = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(target_os = "windows") {
        nonempty("APPDATA").map(|p| p.join("dbsloc"))
    } else if cfg!(target_os = "macos") {
        nonempty("HOME").map(|p| p.join("Library/Application Support/dbsloc"))
    } else {
        nonempty("XDG_DATA_HOME")
            .or_else(|| nonempty("HOME").map(|h| h.join(".local/share")))
            .map(|p| p.join("dbsloc"))
    }
}

pub const CACHE_DIR_ENV: &str = "DBSLOC_CACHE_DIR";

pub fn cache_dir_with(override_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(d) = override_dir.map(str::trim).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let nonempty = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(target_os = "windows") {
        nonempty("LOCALAPPDATA").map(|p| p.join("dbsloc").join("cache"))
    } else if cfg!(target_os = "macos") {
        nonempty("HOME").map(|p| p.join("Library/Caches/dbsloc"))
    } else {
        nonempty("XDG_CACHE_HOME")
            .or_else(|| nonempty("HOME").map(|h| h.join(".cache")))
            .map(|p| p.join("dbsloc"))
    }
}

pub fn cache_dir() -> Option<PathBuf> {
    cache_dir_with(std::env::var(CACHE_DIR_ENV).ok().as_deref())
}

pub fn user_data_dir() -> Option<PathBuf> {
    user_data_dir_with(std::env::var(DATA_DIR_ENV).ok().as_deref())
}

pub fn entry_dir(root: &Path, e: &TemplateEntry) -> PathBuf {
    root.join("templates")
        .join("space")
        .join(e.space)
        .join(format!("res-{}", e.res_label()))
}

pub fn shipped_root() -> Option<PathBuf> {
    crate::bundled_bin::find_resources_root()
}

pub fn expected_t1_path_in(
    e: &TemplateEntry,
    shipped: Option<&Path>,
    data: Option<&Path>,
) -> PathBuf {
    match e.source {
        TemplateSource::Shipped => {
            let base = shipped
                .map(|r| r.join("resources"))
                .unwrap_or_else(|| PathBuf::from("resources"));
            entry_dir(&base, e).join("t1.nii.gz")
        }
        TemplateSource::Download { .. } => {
            let dir = entry_dir(data.unwrap_or_else(|| Path::new("")), e);
            let compressed = dir.join("t1.nii.gz");
            if compressed.is_file() {
                compressed
            } else {
                dir.join("t1.nii")
            }
        }
    }
}

pub fn entry_t1_path_in(
    e: &TemplateEntry,
    shipped: Option<&Path>,
    data: Option<&Path>,
) -> Option<PathBuf> {
    let have_root = match e.source {
        TemplateSource::Shipped => shipped.is_some(),
        TemplateSource::Download { .. } => data.is_some(),
    };
    let p = expected_t1_path_in(e, shipped, data);
    (have_root && p.is_file()).then_some(p)
}

pub fn entry_t1_path(e: &TemplateEntry) -> Option<PathBuf> {
    entry_t1_path_in(e, shipped_root().as_deref(), user_data_dir().as_deref())
}

pub fn infer_space(t1: &Path) -> String {
    if let Some(dir) = t1.parent() {
        if let Ok(text) = std::fs::read_to_string(dir.join("template.json")) {
            if let Some(space) = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("space").and_then(|s| s.as_str().map(str::to_owned)))
                .filter(|s| !s.is_empty())
            {
                return space;
            }
        }
    }
    let parts: Vec<String> = t1
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.len() >= 3 {
        for i in (0..parts.len() - 2).rev() {
            if parts[i] == "space" {
                return parts[i + 1].clone();
            }
        }
    }
    match custom_hash(t1) {
        Some(h) => format!("custom-{}", &h[..12]),
        None => "custom-unknown".to_owned(),
    }
}

type HashKey = (PathBuf, u64, Option<SystemTime>);

fn custom_hash(t1: &Path) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<HashKey, String>>> = OnceLock::new();
    let meta = std::fs::metadata(t1).ok().filter(|m| m.is_file())?;
    let key = (crate::paths::absolute(t1), meta.len(), meta.modified().ok());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(h) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return Some(h.clone());
    }
    let h = crate::sha256::file_sha256_hex(t1).ok()?;
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, h.clone());
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_cat_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ids_follow_space_and_resolution() {
        for e in CATALOGUE {
            assert_eq!(e.id, format!("{}_res-{}", e.space, e.res_label()));
        }
        let mut ids: Vec<_> = CATALOGUE.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CATALOGUE.len());
        assert_eq!(default_entry().source, TemplateSource::Shipped);
        assert_eq!(default_entry().id, DEFAULT_ENTRY_ID);
    }

    #[test]
    fn lookup_by_id_and_unique_space() {
        assert_eq!(lookup("MNI152NLin2009bAsym_res-0.5mm").unwrap().res_mm, 0.5);
        assert_eq!(lookup("MNI152NLin2009bAsym").unwrap().id, DEFAULT_ENTRY_ID);
        assert_eq!(
            lookup("MNI152NLin2009cSym").unwrap().id,
            "MNI152NLin2009cSym_res-1mm"
        );
        assert!(lookup("nope").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn every_download_entry_is_pinned_to_its_own_zip() {
        for e in catalogue() {
            if let TemplateSource::Download {
                url,
                zip_sha256,
                zip_size,
                t1_member,
                t2_member,
                licence_member,
            } = e.source
            {
                let stem = url
                    .rsplit('/')
                    .next()
                    .and_then(|f| f.strip_suffix("_nifti.zip"))
                    .unwrap();
                assert_eq!(zip_sha256.len(), 64, "{}", e.id);
                assert!(zip_size > 0, "{}", e.id);
                assert!(t1_member.starts_with(&format!("{stem}/")), "{}", e.id);
                assert!(t2_member.starts_with(&format!("{stem}/")), "{}", e.id);
                assert!(t1_member.contains("_t1_tal_"), "{}", e.id);
                assert!(t2_member.contains("_t2_tal_"), "{}", e.id);
                assert_eq!(licence_member, "COPYING");
            }
        }
    }

    #[test]
    fn data_dir_override_wins() {
        assert_eq!(
            user_data_dir_with(Some(" /x/y ")),
            Some(PathBuf::from("/x/y"))
        );
        let dir = entry_dir(
            Path::new("/d"),
            lookup("MNI152NLin2009bAsym_res-0.5mm").unwrap(),
        );
        assert!(dir.ends_with("templates/space/MNI152NLin2009bAsym/res-0.5mm"));
    }

    #[test]
    fn entry_paths_exist_only_when_installed() {
        let root = scratch("paths");
        let e = lookup("MNI152NLin2009bAsym_res-0.5mm").unwrap();
        assert!(entry_t1_path_in(e, None, Some(&root)).is_none());
        let d = entry_dir(&root, e);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("t1.nii.gz"), b"x").unwrap();
        assert_eq!(
            entry_t1_path_in(e, None, Some(&root)),
            Some(d.join("t1.nii.gz"))
        );
        std::fs::write(d.join("t1.nii"), b"x").unwrap();
        assert_eq!(
            entry_t1_path_in(e, None, Some(&root)),
            Some(d.join("t1.nii.gz"))
        );
        std::fs::remove_file(d.join("t1.nii.gz")).unwrap();
        assert_eq!(
            entry_t1_path_in(e, None, Some(&root)),
            Some(d.join("t1.nii"))
        );
        assert_eq!(expected_t1_path_in(e, None, Some(&root)), d.join("t1.nii"));
        assert!(entry_t1_path_in(e, None, None).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn space_inference_order() {
        let root = scratch("infer");
        let a = root.join("templates/space/MNI152NLin2009bAsym/t1.nii");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(infer_space(&a), "MNI152NLin2009bAsym");
        std::fs::write(
            a.parent().unwrap().join("template.json"),
            br#"{"space": "Foo"}"#,
        )
        .unwrap();
        assert_eq!(infer_space(&a), "Foo");
        let b = root.join("loose.nii");
        std::fs::write(&b, b"abc").unwrap();
        let s = infer_space(&b);
        assert_eq!(
            s,
            format!("custom-{}", &crate::sha256::sha256_hex(b"abc")[..12])
        );
        assert_eq!(infer_space(&b), s);
        std::fs::write(&b, b"abcd").unwrap();
        assert_eq!(
            infer_space(&b),
            format!("custom-{}", &crate::sha256::sha256_hex(b"abcd")[..12])
        );
        assert_eq!(infer_space(&root.join("missing.nii")), "custom-unknown");
        let _ = std::fs::remove_dir_all(&root);
    }
}
