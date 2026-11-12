// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use crate::bids::Subject;

pub const ANCHOR_NATIVE: &str = "anchorNative";
pub const DESC_PREPROC: &str = "preproc";
pub const DESC_PRECOREG: &str = "precoreg";
pub const CT_KEY: &str = "CT";
pub const T2W: &str = "T2w";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entities {
    pub pairs: Vec<(String, String)>,
    pub suffix: String,
}

pub fn strip_nifti_ext(name: &str) -> &str {
    let low = name.to_ascii_lowercase();
    for ext in [".nii.gz", ".nii", ".json"] {
        if low.ends_with(ext) {
            return &name[..name.len() - ext.len()];
        }
    }
    name
}

impl Entities {
    pub fn parse(file_name: &str) -> Entities {
        let stem = strip_nifti_ext(file_name);
        let tokens: Vec<&str> = stem.split('_').filter(|t| !t.is_empty()).collect();
        let mut pairs = Vec::new();
        let mut suffix = String::new();
        for (i, tok) in tokens.iter().enumerate() {
            match tok.split_once('-') {
                Some((k, v)) if !k.is_empty() && !v.is_empty() => {
                    pairs.push((k.to_owned(), v.to_owned()));
                }
                _ if i + 1 == tokens.len() => suffix = (*tok).to_owned(),
                _ => pairs.push(((*tok).to_owned(), String::new())),
            }
        }
        Entities { pairs, suffix }
    }

    pub fn from_path(path: &Path) -> Entities {
        Entities::parse(&path.file_name().unwrap_or_default().to_string_lossy())
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn with(mut self, key: &str, value: &str) -> Entities {
        self.pairs.retain(|(k, _)| k != key);
        self.pairs.push((key.to_owned(), value.to_owned()));
        self
    }

    pub fn with_suffix(mut self, suffix: &str) -> Entities {
        self.suffix = suffix.to_owned();
        self
    }

    pub fn stem(&self) -> String {
        let mut parts: Vec<String> = self
            .pairs
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    k.clone()
                } else {
                    format!("{k}-{v}")
                }
            })
            .collect();
        if !self.suffix.is_empty() {
            parts.push(self.suffix.clone());
        }
        parts.join("_")
    }
}

pub fn image_stem(raw: &Path, space: Option<&str>, desc: Option<&str>) -> String {
    let mut e = Entities::from_path(raw);
    if let Some(s) = space {
        e = e.with("space", s);
    }
    if let Some(d) = desc {
        e = e.with("desc", d);
    }
    e.stem()
}

pub fn image_name(raw: &Path, space: Option<&str>, desc: Option<&str>) -> String {
    format!("{}.nii.gz", image_stem(raw, space, desc))
}

pub fn sidecar_name(raw: &Path, space: Option<&str>, desc: Option<&str>) -> String {
    format!("{}.json", image_stem(raw, space, desc))
}

pub fn transform_name(
    subj_id: &str,
    ses: Option<&str>,
    acq: Option<&str>,
    from: &str,
    to: &str,
    desc: Option<&str>,
    ext: &str,
) -> String {
    let mut s = format!("sub-{subj_id}");
    if let Some(ses) = ses {
        s.push_str(&format!("_ses-{ses}"));
    }
    if let Some(acq) = acq {
        s.push_str(&format!("_acq-{acq}"));
    }
    s.push_str(&format!("_from-{from}_to-{to}"));
    if let Some(d) = desc {
        s.push_str(&format!("_desc-{d}"));
    }
    s.push_str(&format!("_mode-image_xfm.{ext}"));
    s
}

pub fn work_prefix(dir: &Path, tag: &str) -> PathBuf {
    crate::outputs::partial_of(&dir.join(tag))
}

pub fn key_tag(key: &str) -> String {
    key.split('_')
        .next()
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(3)
        .collect::<String>()
        .to_ascii_lowercase()
}

pub fn short_image(dir: &Path, tag: &str) -> PathBuf {
    dir.join(format!("{}{tag}.nii.gz", crate::outputs::PARTIAL_PREFIX))
}

pub fn suffixed(prefix: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", prefix.display()))
}

pub const ANTS_AFFINE_SUFFIX: &str = "0GenericAffine.mat";
pub const ANTS_COMPOSITE_SUFFIX: &str = "Composite.h5";
pub const ANTS_INVERSE_COMPOSITE_SUFFIX: &str = "InverseComposite.h5";
pub const ANTS_WARPED_SUFFIX: &str = "_Warped.nii.gz";

fn root(sub: &Subject, stage: &str) -> PathBuf {
    sub.subj_dir.join(stage)
}

pub fn coreg_root(sub: &Subject) -> PathBuf {
    root(sub, "coregistration")
}

pub fn norm_root(sub: &Subject) -> PathBuf {
    root(sub, "normalization")
}

pub fn brainshift_root(sub: &Subject) -> PathBuf {
    root(sub, "brainshift")
}

pub fn coreg_transform_dir(sub: &Subject) -> PathBuf {
    coreg_root(sub).join("transformations")
}

pub fn coreg_log_dir(sub: &Subject) -> PathBuf {
    coreg_root(sub).join("log")
}

pub fn norm_transform_dir(sub: &Subject) -> PathBuf {
    norm_root(sub).join("transformations")
}

pub fn norm_log_dir(sub: &Subject) -> PathBuf {
    norm_root(sub).join("log")
}

pub fn checkreg_dir(stage_root: &Path) -> PathBuf {
    stage_root.join("checkreg")
}

pub fn checkreg_png(checkreg_dir: &Path, image: &Path) -> PathBuf {
    let name = image.file_name().unwrap_or_default().to_string_lossy();
    let low = name.to_ascii_lowercase();
    let base = if low.ends_with(".nii.gz") {
        format!("{}.png", &name[..name.len() - 7])
    } else {
        format!("{name}.png")
    };
    checkreg_dir.join(base)
}

fn fallback_raw(sub: &Subject, ses: &str, suffix: &str) -> PathBuf {
    PathBuf::from(format!("sub-{}_ses-{ses}_{suffix}.nii.gz", sub.subj_id))
}

pub fn anchor_raw(sub: &Subject) -> PathBuf {
    sub.preop_anchor_raw
        .clone()
        .unwrap_or_else(|| fallback_raw(sub, "preop", &sub.anchor_modality))
}

pub fn ct_raw(sub: &Subject) -> PathBuf {
    sub.postop_ct_raw
        .clone()
        .unwrap_or_else(|| fallback_raw(sub, "postop", CT_KEY))
}

pub fn postop_mr_raw(sub: &Subject, key: &str) -> PathBuf {
    sub.postop_raw.get(key).cloned().unwrap_or_else(|| {
        let acq = key.split('_').next().unwrap_or("ax");
        PathBuf::from(format!(
            "sub-{}_ses-postop_acq-{acq}_T1w.nii.gz",
            sub.subj_id
        ))
    })
}

pub fn postop_raw_for(sub: &Subject, key: &str) -> PathBuf {
    if key == CT_KEY {
        ct_raw(sub)
    } else {
        postop_mr_raw(sub, key)
    }
}

pub fn t2_raw(sub: &Subject) -> PathBuf {
    sub.preop_t2_nonanchor()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| fallback_raw(sub, "preop", T2W))
}

pub fn preproc_image(sub: &Subject) -> PathBuf {
    sub.preproc_dir
        .join(image_name(&anchor_raw(sub), None, Some(DESC_PREPROC)))
}

pub fn preproc_record(sub: &Subject) -> PathBuf {
    sub.preproc_dir
        .join(sidecar_name(&anchor_raw(sub), None, Some(DESC_PREPROC)))
}

pub fn precoreg_image(sub: &Subject) -> PathBuf {
    sub.coreg_dir
        .join(image_name(&anchor_raw(sub), None, Some(DESC_PRECOREG)))
}

pub fn precoreg_transform(sub: &Subject, space: &str) -> PathBuf {
    coreg_transform_dir(sub).join(transform_name(
        &sub.subj_id,
        Some("preop"),
        None,
        &sub.anchor_modality,
        space,
        Some(DESC_PRECOREG),
        "mat",
    ))
}

pub fn precoreg_record(sub: &Subject) -> PathBuf {
    coreg_log_dir(sub).join(format!("sub-{}_desc-precoreg.json", sub.subj_id))
}

pub fn coreg_ct_image(sub: &Subject) -> PathBuf {
    sub.coreg_dir.join(image_name(
        &ct_raw(sub),
        Some(ANCHOR_NATIVE),
        Some(DESC_PREPROC),
    ))
}

fn ct_transform(sub: &Subject, from: &str, to: &str, ext: &str) -> PathBuf {
    coreg_transform_dir(sub).join(transform_name(
        &sub.subj_id,
        Some("postop"),
        None,
        from,
        to,
        None,
        ext,
    ))
}

pub fn coreg_ct_affine(sub: &Subject) -> PathBuf {
    ct_transform(sub, CT_KEY, ANCHOR_NATIVE, "mat")
}

pub fn coreg_ct_forward(sub: &Subject) -> PathBuf {
    ct_transform(sub, CT_KEY, ANCHOR_NATIVE, "h5")
}

pub fn coreg_ct_inverse(sub: &Subject) -> PathBuf {
    ct_transform(sub, ANCHOR_NATIVE, CT_KEY, "h5")
}

pub fn coreg_multistart_record(sub: &Subject) -> PathBuf {
    coreg_log_dir(sub).join(format!(
        "sub-{}_ses-postop_desc-multistart_{CT_KEY}.json",
        sub.subj_id
    ))
}

pub fn coreg_multistart_dir(sub: &Subject) -> PathBuf {
    coreg_log_dir(sub).join("multistart")
}

pub fn coreg_mr_image(sub: &Subject, key: &str) -> PathBuf {
    sub.coreg_dir.join(image_name(
        &postop_mr_raw(sub, key),
        Some(ANCHOR_NATIVE),
        Some(DESC_PREPROC),
    ))
}

pub fn coreg_mr_affine(sub: &Subject, key: &str) -> PathBuf {
    let raw = postop_mr_raw(sub, key);
    let e = Entities::from_path(&raw);
    let key_acq = key.split('_').next().filter(|a| !a.is_empty());
    coreg_transform_dir(sub).join(transform_name(
        &sub.subj_id,
        Some("postop"),
        e.get("acq").or(key_acq),
        &e.suffix,
        ANCHOR_NATIVE,
        None,
        "mat",
    ))
}

pub fn coreg_t2_image(sub: &Subject) -> PathBuf {
    sub.coreg_dir.join(image_name(
        &t2_raw(sub),
        Some(ANCHOR_NATIVE),
        Some(DESC_PREPROC),
    ))
}

pub fn coreg_t2_affine(sub: &Subject) -> PathBuf {
    let raw = t2_raw(sub);
    let e = Entities::from_path(&raw);
    coreg_transform_dir(sub).join(transform_name(
        &sub.subj_id,
        Some("preop"),
        e.get("acq"),
        T2W,
        ANCHOR_NATIVE,
        None,
        "mat",
    ))
}

pub fn norm_anchor_image(sub: &Subject, space: &str) -> PathBuf {
    sub.norm_dir.join(image_name(
        &anchor_raw(sub),
        Some(space),
        Some(DESC_PREPROC),
    ))
}

pub fn norm_postop_image(sub: &Subject, key: &str, space: &str) -> PathBuf {
    sub.norm_dir.join(image_name(
        &postop_raw_for(sub, key),
        Some(space),
        Some(DESC_PREPROC),
    ))
}

pub fn norm_forward(sub: &Subject, space: &str) -> PathBuf {
    norm_transform_dir(sub).join(transform_name(
        &sub.subj_id,
        None,
        None,
        ANCHOR_NATIVE,
        space,
        None,
        "h5",
    ))
}

pub fn norm_inverse(sub: &Subject, space: &str) -> PathBuf {
    norm_transform_dir(sub).join(transform_name(
        &sub.subj_id,
        None,
        None,
        space,
        ANCHOR_NATIVE,
        None,
        "h5",
    ))
}

pub const DESC_AFFINE: &str = "affine";

/// The linear (rigid + affine) part of [`norm_forward`] as an ITK `.mat`.
pub fn norm_forward_affine(sub: &Subject, space: &str) -> PathBuf {
    norm_transform_dir(sub).join(transform_name(
        &sub.subj_id,
        None,
        None,
        ANCHOR_NATIVE,
        space,
        Some(DESC_AFFINE),
        "mat",
    ))
}

/// The inverse of [`norm_forward_affine`].
pub fn norm_inverse_affine(sub: &Subject, space: &str) -> PathBuf {
    norm_transform_dir(sub).join(transform_name(
        &sub.subj_id,
        None,
        None,
        space,
        ANCHOR_NATIVE,
        Some(DESC_AFFINE),
        "mat",
    ))
}

pub fn norm_work_dir(sub: &Subject) -> PathBuf {
    norm_root(sub).join("work")
}

pub fn reconstruction_json(sub: &Subject) -> PathBuf {
    sub.recon_dir
        .join(format!("sub-{}_desc-reconstruction.json", sub.subj_id))
}

pub fn pacer_import(sub: &Subject) -> PathBuf {
    sub.recon_dir
        .join(format!("sub-{}_desc-pacer_import.json", sub.subj_id))
}

pub fn scene_manifest(sub: &Subject) -> PathBuf {
    sub.recon_dir.join("viz").join("scene_manifest.json")
}

pub fn uiprefs(sub: &Subject) -> PathBuf {
    sub.prefs_dir
        .join(format!("sub-{}_desc-uiprefs.mat", sub.subj_id))
}

pub fn prefs_rawimages(subj_dir: &Path, subj_id: &str) -> PathBuf {
    subj_dir
        .join("prefs")
        .join(format!("sub-{subj_id}_desc-rawimages.json"))
}

pub fn coreg_method_log(sub: &Subject) -> PathBuf {
    coreg_log_dir(sub).join(format!("sub-{}_desc-coregmethod.json", sub.subj_id))
}

pub fn norm_method_log(sub: &Subject) -> PathBuf {
    norm_log_dir(sub).join(format!("sub-{}_desc-normmethod.json", sub.subj_id))
}

pub fn brainshift_method_log(sub: &Subject) -> PathBuf {
    brainshift_root(sub)
        .join("log")
        .join(format!("sub-{}_desc-brainshiftmethod.json", sub.subj_id))
}

fn brain_mask(sub: &Subject, space: &str) -> PathBuf {
    let e = Entities::from_path(&ct_raw(sub))
        .with("space", space)
        .with("desc", "brain")
        .with_suffix("mask");
    sub.recon_dir.join(format!("{}.nii.gz", e.stem()))
}

pub fn brain_mask_ct(sub: &Subject) -> PathBuf {
    brain_mask(sub, "CT")
}

pub fn brain_mask_anchor(sub: &Subject) -> PathBuf {
    brain_mask(sub, ANCHOR_NATIVE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bids::resolve_subject_with;

    fn name(p: &Path) -> String {
        p.file_name().unwrap().to_string_lossy().into_owned()
    }

    fn rel(sub: &Subject, p: &Path) -> String {
        p.strip_prefix(&sub.subj_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    fn subject(tag: &str) -> (PathBuf, Subject) {
        let root = std::env::temp_dir().join(format!("dbsloc_deriv_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        touch(&root.join("rawdata/sub-01/ses-preop/anat/sub-01_ses-preop_acq-iso_T1w.nii"));
        touch(&root.join("rawdata/sub-01/ses-preop/anat/sub-01_ses-preop_acq-hr_T2w.nii.gz"));
        touch(&root.join("rawdata/sub-01/ses-postop/anat/sub-01_ses-postop_CT.nii.gz"));
        touch(&root.join("rawdata/sub-01/ses-postop/anat/sub-01_ses-postop_acq-ax_T2w.nii.gz"));
        let sub = resolve_subject_with(&root, "01", false).unwrap();
        (root, sub)
    }

    #[test]
    fn image_rule_inserts_space_and_desc_before_the_suffix() {
        let raw = Path::new("sub-01_ses-postop_acq-ax_T2w.nii.gz");
        assert_eq!(
            image_name(raw, Some(ANCHOR_NATIVE), Some(DESC_PREPROC)),
            "sub-01_ses-postop_acq-ax_space-anchorNative_desc-preproc_T2w.nii.gz"
        );
    }

    #[test]
    fn image_rule_replaces_present_entities_and_always_writes_nii_gz() {
        let raw = Path::new("sub-01_ses-preop_desc-old_T1w.nii");
        assert_eq!(
            image_name(raw, Some("MNI152NLin2009bAsym"), Some("preproc")),
            "sub-01_ses-preop_space-MNI152NLin2009bAsym_desc-preproc_T1w.nii.gz"
        );
        let twice = Path::new("sub-01_ses-preop_space-a_desc-b_T1w.nii.gz");
        assert_eq!(
            image_name(twice, Some("x"), Some("y")),
            "sub-01_ses-preop_space-x_desc-y_T1w.nii.gz"
        );
        assert_eq!(
            image_name(raw, None, Some("precoreg")),
            "sub-01_ses-preop_desc-precoreg_T1w.nii.gz"
        );
    }

    #[test]
    fn transform_rule_follows_the_fmriprep_layout() {
        assert_eq!(
            transform_name("01", Some("postop"), None, "CT", ANCHOR_NATIVE, None, "mat"),
            "sub-01_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat"
        );
        assert_eq!(
            transform_name(
                "01",
                Some("preop"),
                Some("iso"),
                "T1w",
                "MNI",
                Some("precoreg"),
                "mat"
            ),
            "sub-01_ses-preop_acq-iso_from-T1w_to-MNI_desc-precoreg_mode-image_xfm.mat"
        );
        assert_eq!(
            transform_name("01", None, None, "anchorNative", "MNI", None, "h5"),
            "sub-01_from-anchorNative_to-MNI_mode-image_xfm.h5"
        );
    }

    #[test]
    fn every_row_of_the_table_has_its_name() {
        let (root, sub) = subject("table");
        let space = "MNI152NLin2009bAsym";
        let cases = [
            (
                rel(&sub, &preproc_image(&sub)),
                "preprocessing/anat/sub-01_ses-preop_acq-iso_desc-preproc_T1w.nii.gz",
            ),
            (
                rel(&sub, &preproc_record(&sub)),
                "preprocessing/anat/sub-01_ses-preop_acq-iso_desc-preproc_T1w.json",
            ),
            (
                rel(&sub, &precoreg_image(&sub)),
                "coregistration/anat/sub-01_ses-preop_acq-iso_desc-precoreg_T1w.nii.gz",
            ),
            (
                rel(&sub, &precoreg_transform(&sub, space)),
                "coregistration/transformations/sub-01_ses-preop_from-T1w_to-MNI152NLin2009bAsym_desc-precoreg_mode-image_xfm.mat",
            ),
            (
                rel(&sub, &precoreg_record(&sub)),
                "coregistration/log/sub-01_desc-precoreg.json",
            ),
            (
                rel(&sub, &coreg_ct_image(&sub)),
                "coregistration/anat/sub-01_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz",
            ),
            (
                rel(&sub, &coreg_ct_affine(&sub)),
                "coregistration/transformations/sub-01_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat",
            ),
            (
                rel(&sub, &coreg_ct_forward(&sub)),
                "coregistration/transformations/sub-01_ses-postop_from-CT_to-anchorNative_mode-image_xfm.h5",
            ),
            (
                rel(&sub, &coreg_ct_inverse(&sub)),
                "coregistration/transformations/sub-01_ses-postop_from-anchorNative_to-CT_mode-image_xfm.h5",
            ),
            (
                rel(&sub, &coreg_multistart_record(&sub)),
                "coregistration/log/sub-01_ses-postop_desc-multistart_CT.json",
            ),
            (
                rel(&sub, &coreg_multistart_dir(&sub)),
                "coregistration/log/multistart",
            ),
            (
                rel(&sub, &coreg_mr_image(&sub, "ax_MRI")),
                "coregistration/anat/sub-01_ses-postop_acq-ax_space-anchorNative_desc-preproc_T2w.nii.gz",
            ),
            (
                rel(&sub, &coreg_mr_affine(&sub, "ax_MRI")),
                "coregistration/transformations/sub-01_ses-postop_acq-ax_from-T2w_to-anchorNative_mode-image_xfm.mat",
            ),
            (
                rel(&sub, &coreg_t2_image(&sub)),
                "coregistration/anat/sub-01_ses-preop_acq-hr_space-anchorNative_desc-preproc_T2w.nii.gz",
            ),
            (
                rel(&sub, &coreg_t2_affine(&sub)),
                "coregistration/transformations/sub-01_ses-preop_acq-hr_from-T2w_to-anchorNative_mode-image_xfm.mat",
            ),
            (
                rel(&sub, &norm_anchor_image(&sub, space)),
                "normalization/anat/sub-01_ses-preop_acq-iso_space-MNI152NLin2009bAsym_desc-preproc_T1w.nii.gz",
            ),
            (
                rel(&sub, &norm_postop_image(&sub, "CT", space)),
                "normalization/anat/sub-01_ses-postop_space-MNI152NLin2009bAsym_desc-preproc_CT.nii.gz",
            ),
            (
                rel(&sub, &norm_postop_image(&sub, "ax_MRI", space)),
                "normalization/anat/sub-01_ses-postop_acq-ax_space-MNI152NLin2009bAsym_desc-preproc_T2w.nii.gz",
            ),
            (
                rel(&sub, &norm_forward(&sub, space)),
                "normalization/transformations/sub-01_from-anchorNative_to-MNI152NLin2009bAsym_mode-image_xfm.h5",
            ),
            (
                rel(&sub, &norm_inverse(&sub, space)),
                "normalization/transformations/sub-01_from-MNI152NLin2009bAsym_to-anchorNative_mode-image_xfm.h5",
            ),
            (
                rel(&sub, &norm_forward_affine(&sub, space)),
                "normalization/transformations/sub-01_from-anchorNative_to-MNI152NLin2009bAsym_desc-affine_mode-image_xfm.mat",
            ),
            (
                rel(&sub, &norm_inverse_affine(&sub, space)),
                "normalization/transformations/sub-01_from-MNI152NLin2009bAsym_to-anchorNative_desc-affine_mode-image_xfm.mat",
            ),
            (rel(&sub, &norm_work_dir(&sub)), "normalization/work"),
            (
                rel(&sub, &coreg_method_log(&sub)),
                "coregistration/log/sub-01_desc-coregmethod.json",
            ),
            (
                rel(&sub, &norm_method_log(&sub)),
                "normalization/log/sub-01_desc-normmethod.json",
            ),
            (
                rel(&sub, &brainshift_method_log(&sub)),
                "brainshift/log/sub-01_desc-brainshiftmethod.json",
            ),
            (
                rel(&sub, &brain_mask_ct(&sub)),
                "reconstruction/sub-01_ses-postop_space-CT_desc-brain_mask.nii.gz",
            ),
            (
                rel(&sub, &brain_mask_anchor(&sub)),
                "reconstruction/sub-01_ses-postop_space-anchorNative_desc-brain_mask.nii.gz",
            ),
        ];
        for (got, want) in cases {
            assert_eq!(got, want);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn post_op_mr_transforms_without_acq_are_named_by_their_bundle_key() {
        let root = std::env::temp_dir().join(format!("dbsloc_deriv_noacq_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        touch(&root.join("rawdata/sub-01/ses-preop/anat/sub-01_ses-preop_T1w.nii.gz"));
        let post = root.join("rawdata/sub-01/ses-postop/anat");
        touch(&post.join("sub-01_ses-postop_cor_T2w.nii.gz"));
        touch(&post.join("sub-01_ses-postop_T2w.nii.gz"));
        let row = |file: &str, key: &str| {
            serde_json::json!({
                "path": format!("rawdata/sub-01/ses-postop/anat/{file}"),
                "session": "postop",
                "datatype": "anat",
                "entities": {"suffix": "T2w", "modality": key},
            })
        };
        std::fs::write(
            root.join("rawdata/sub-01/rawimages.json"),
            serde_json::json!([
                row("sub-01_ses-postop_cor_T2w.nii.gz", "cor_MRI"),
                row("sub-01_ses-postop_T2w.nii.gz", "ax_MRI"),
            ])
            .to_string(),
        )
        .unwrap();
        let sub = resolve_subject_with(&root, "01", true).unwrap();
        assert!(sub.postop_raw.contains_key("cor_MRI") && sub.postop_raw.contains_key("ax_MRI"));
        assert_eq!(
            name(&coreg_mr_affine(&sub, "ax_MRI")),
            "sub-01_ses-postop_acq-ax_from-T2w_to-anchorNative_mode-image_xfm.mat"
        );
        assert_eq!(
            name(&coreg_mr_affine(&sub, "cor_MRI")),
            "sub-01_ses-postop_acq-cor_from-T2w_to-anchorNative_mode-image_xfm.mat"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn checkreg_png_uses_the_image_stem() {
        let d = Path::new("/s/checkreg");
        assert_eq!(
            name(&checkreg_png(
                d,
                Path::new("/a/sub-01_space-X_desc-preproc_T1w.nii.gz")
            )),
            "sub-01_space-X_desc-preproc_T1w.png"
        );
    }

    #[test]
    fn entities_round_trip() {
        let e = Entities::parse("sub-01_ses-postop_acq-ax_T2w.nii.gz");
        assert_eq!(e.get("acq"), Some("ax"));
        assert_eq!(e.suffix, "T2w");
        assert_eq!(e.stem(), "sub-01_ses-postop_acq-ax_T2w");
    }
}
