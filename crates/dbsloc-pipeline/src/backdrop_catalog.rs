// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use crate::template_catalog::user_data_dir;

const CITATION: &str = "Edlow BL, Mareyam A, Horn A, et al. 7 Tesla MRI of the ex vivo human brain at 100 micron resolution. Sci Data 6, 244 (2019), doi:10.1038/s41597-019-0254-8; data: OpenNeuro ds002179, doi:10.18112/openneuro.ds002179.v1.1.0.";

const LICENCE: &str = "Data licence: CC0 1.0 Universal (public domain dedication), https://creativecommons.org/publicdomain/zero/1.0/\n\nCitation: Edlow BL, Mareyam A, Horn A, et al. 7 Tesla MRI of the ex vivo human brain at 100 micron resolution. Sci Data 6, 244 (2019), doi:10.1038/s41597-019-0254-8; data: OpenNeuro ds002179, doi:10.18112/openneuro.ds002179.v1.1.0.\n";

pub const BACKDROP_FILE: &str = "backdrop.nii.gz";
pub const BACKDROP_FILE_PLAIN: &str = "backdrop.nii";

#[derive(Debug, Clone, PartialEq)]
pub struct BackdropEntry {
    pub id: &'static str,
    pub space: &'static str,
    pub res_mm: f64,
    pub title: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    pub crop_mm: Option<[[f64; 2]; 3]>,
    pub expected_dims: [usize; 3],
    pub licence_text: &'static str,
}

impl BackdropEntry {
    pub fn citation(&self) -> &'static str {
        CITATION
    }
}

static CATALOGUE: [BackdropEntry; 2] = [
    BackdropEntry {
        id: "edlow2019-7t-exvivo_res-0.5mm",
        space: "MNI152NLin2009bAsym",
        res_mm: 0.5,
        title: "7T ex vivo brain (Edlow 2019), whole brain, 0.5 mm",
        url: "https://s3.amazonaws.com/openneuro.org/ds002179/derivatives/sub-EXC004/processed_data/MNI/Synthesized_FLASH25_in_MNI_v2_500um.nii.gz",
        size: 74_205_499,
        sha256: "a47e6fbf50c0043272ca674d461d90a6346083ac7a66e8eb2277d4fff279691b",
        crop_mm: None,
        expected_dims: [395, 467, 379],
        licence_text: LICENCE,
    },
    BackdropEntry {
        id: "edlow2019-7t-exvivo_res-0.2mm_desc-deepbrain",
        space: "MNI152NLin2009bAsym",
        res_mm: 0.2,
        title: "7T ex vivo brain (Edlow 2019), deep brain region, 0.2 mm",
        url: "https://s3.amazonaws.com/openneuro.org/ds002179/derivatives/sub-EXC004/processed_data/MNI/Synthesized_FLASH25_in_MNI_v2_200um.nii.gz",
        size: 1_154_496_050,
        sha256: "acdbb81d5d3b6c793cb05ed520baad61db4eac6305e6e024d45ddb9d920e76c1",
        crop_mm: Some([[-50.0, 50.0], [-60.0, 35.0], [-40.0, 35.0]]),
        expected_dims: [986, 1166, 946],
        licence_text: LICENCE,
    },
];

pub fn catalogue() -> &'static [BackdropEntry] {
    &CATALOGUE
}

pub fn lookup(id: &str) -> Option<&'static BackdropEntry> {
    let key = id.trim();
    CATALOGUE.iter().find(|e| e.id == key)
}

pub fn entries_for_space(space: &str) -> Vec<&'static BackdropEntry> {
    CATALOGUE.iter().filter(|e| e.space == space).collect()
}

pub fn entry_dir_in(root: &Path, space: &str, id: &str) -> PathBuf {
    root.join("backdrops").join("space").join(space).join(id)
}

pub fn entry_dir(space: &str, id: &str) -> Option<PathBuf> {
    user_data_dir().map(|r| entry_dir_in(&r, space, id))
}

pub fn installed_path_in(root: &Path, e: &BackdropEntry) -> Option<PathBuf> {
    let dir = entry_dir_in(root, e.space, e.id);
    [BACKDROP_FILE, BACKDROP_FILE_PLAIN]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

pub fn installed_path(e: &BackdropEntry) -> Option<PathBuf> {
    user_data_dir().and_then(|r| installed_path_in(&r, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_URL: &str =
    "https://s3.amazonaws.com/openneuro.org/ds002179/derivatives/sub-EXC004/processed_data/MNI/";

    #[test]
    fn pins_are_present_and_ids_unique() {
        let mut ids: Vec<&str> = catalogue().iter().map(|e| e.id).collect();
        for e in catalogue() {
            assert_eq!(e.sha256.len(), 64, "{}", e.id);
            assert!(e.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(e.size > 0);
            assert!(e.url.starts_with(BASE_URL), "{}", e.id);
            assert!(e.licence_text.contains("CC0 1.0 Universal"));
            assert!(e.licence_text.contains(CITATION));
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), catalogue().len());
    }

    #[test]
    fn lookup_and_space_filter() {
        assert!(lookup("edlow2019-7t-exvivo_res-0.5mm").is_some());
        assert!(lookup("nope").is_none());
        assert_eq!(entries_for_space("MNI152NLin2009bAsym").len(), 2);
        assert!(entries_for_space("Other").is_empty());
    }

    #[test]
    fn installed_needs_the_backdrop_file() {
        let d = tempfile::tempdir().unwrap();
        let e = &catalogue()[0];
        assert!(installed_path_in(d.path(), e).is_none());
        let dir = entry_dir_in(d.path(), e.space, e.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(BACKDROP_FILE), b"x").unwrap();
        assert_eq!(
            installed_path_in(d.path(), e),
            Some(dir.join(BACKDROP_FILE))
        );
        std::fs::write(dir.join(BACKDROP_FILE_PLAIN), b"x").unwrap();
        assert_eq!(
            installed_path_in(d.path(), e),
            Some(dir.join(BACKDROP_FILE))
        );
        std::fs::remove_file(dir.join(BACKDROP_FILE)).unwrap();
        assert_eq!(
            installed_path_in(d.path(), e),
            Some(dir.join(BACKDROP_FILE_PLAIN))
        );
    }
}
