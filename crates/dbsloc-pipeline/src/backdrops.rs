// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backdrop_catalog::{entries_for_space, installed_path_in};
use crate::bids::Subject;
use crate::derivatives;
use crate::template_catalog::user_data_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackdropKind {
    Template,
    Patient,
    Download,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackdropOption {
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    pub kind: BackdropKind,
}

fn option(id: &str, label: String, path: PathBuf, kind: BackdropKind) -> BackdropOption {
    BackdropOption {
        id: id.to_owned(),
        label,
        path,
        kind,
    }
}

pub fn scene_backdrops(sub: &Subject, template_t1: &Path, space: &str) -> Vec<BackdropOption> {
    scene_backdrops_in(sub, template_t1, space, user_data_dir().as_deref())
}

pub fn scene_backdrops_in(
    sub: &Subject,
    template_t1: &Path,
    space: &str,
    data: Option<&Path>,
) -> Vec<BackdropOption> {
    let mut out = Vec::new();
    if template_t1.is_file() {
        out.push(option(
            "template-t1",
            format!("{space} T1w (template)"),
            template_t1.to_path_buf(),
            BackdropKind::Template,
        ));
    }
    if let Some(dir) = template_t1.parent() {
        if let Some(t2) = ["t2.nii.gz", "t2.nii"]
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())
        {
            out.push(option(
                "template-t2",
                format!("{space} T2w (template)"),
                t2,
                BackdropKind::Template,
            ));
        }
    }
    let patient = [
        (
            "patient-postop-ct",
            "Patient post-op CT (normalized)",
            derivatives::norm_postop_image(sub, derivatives::CT_KEY, space),
        ),
        (
            "patient-postop-ax",
            "Patient post-op MRI, axial (normalized)",
            derivatives::norm_postop_image(sub, "ax_MRI", space),
        ),
        (
            "patient-postop-cor",
            "Patient post-op MRI, coronal (normalized)",
            derivatives::norm_postop_image(sub, "cor_MRI", space),
        ),
        (
            "patient-postop-sag",
            "Patient post-op MRI, sagittal (normalized)",
            derivatives::norm_postop_image(sub, "sag_MRI", space),
        ),
    ];
    for (id, label, path) in patient {
        if path.is_file() {
            out.push(option(id, label.to_owned(), path, BackdropKind::Patient));
        }
    }
    let preop = derivatives::norm_anchor_image(sub, space);
    if preop.is_file() {
        out.push(option(
            "patient-preop",
            format!("Patient pre-op {} (normalized)", sub.anchor_modality),
            preop,
            BackdropKind::Patient,
        ));
    }
    if let Some(data) = data {
        for e in entries_for_space(space) {
            if let Some(p) = installed_path_in(data, e) {
                out.push(option(e.id, e.title.to_owned(), p, BackdropKind::Download));
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SPACE: &str = "MNI152NLin2009bAsym";

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    fn subject(root: &Path) -> Subject {
        let anat = root.join("rawdata/sub-1/ses-preop/anat");
        touch(&anat.join("sub-1_ses-preop_T1w.nii.gz"));
        touch(&root.join("rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_ct.nii.gz"));
        crate::bids::resolve_subject_with(root, "1", false).unwrap()
    }

    #[test]
    fn kinds_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&BackdropKind::Download).unwrap(),
            "\"download\""
        );
        let o: BackdropOption =
            serde_json::from_str(r#"{"id":"a","label":"b","path":"c","kind":"patient"}"#).unwrap();
        assert_eq!(o.kind, BackdropKind::Patient);
    }

    #[test]
    fn lists_exactly_the_existing_files_in_spec_order() {
        let d = tempfile::tempdir().unwrap();
        let sub = subject(d.path());
        let tdir = d.path().join("tpl");
        let t1 = tdir.join("t1.nii.gz");
        touch(&t1);
        let only_t1 = scene_backdrops_in(&sub, &t1, SPACE, None);
        assert_eq!(
            only_t1.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            vec!["template-t1"]
        );
        assert_eq!(only_t1[0].label, format!("{SPACE} T1w (template)"));

        touch(&tdir.join("t2.nii"));
        touch(&derivatives::norm_anchor_image(&sub, SPACE));
        touch(&derivatives::norm_postop_image(&sub, "sag_MRI", SPACE));
        touch(&derivatives::norm_postop_image(&sub, "CT", SPACE));
        let data = d.path().join("data");
        let e = &crate::backdrop_catalog::catalogue()[0];
        touch(&crate::backdrop_catalog::entry_dir_in(&data, e.space, e.id).join("backdrop.nii.gz"));
        let all = scene_backdrops_in(&sub, &t1, SPACE, Some(&data));
        let ids: Vec<&str> = all.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "template-t1",
                "template-t2",
                "patient-postop-ct",
                "patient-postop-sag",
                "patient-preop",
                e.id
            ]
        );
        assert_eq!(all[2].label, "Patient post-op CT (normalized)");
        assert_eq!(all[4].label, "Patient pre-op T1w (normalized)");
        assert_eq!(all[5].kind, BackdropKind::Download);
        assert!(all.iter().all(|o| o.path.is_file()));
        let other = scene_backdrops_in(&sub, &t1, "OtherSpace", Some(&data));
        assert!(other.iter().all(|o| o.kind != BackdropKind::Download));
    }
}
