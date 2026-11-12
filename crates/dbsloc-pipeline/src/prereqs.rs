// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use crate::bids::Subject;
use crate::bundled_bin::find_binary;
use crate::options::RunOptions;
use crate::reg_backend::{cached_probe, normalize_needs_ants, resolve, GpuProbe, RegBackend};
use crate::stages::Stage;
use crate::templates::missing_template_message;

pub const SKIP_EXT_BIN_ENV: &str = "DBSLOC_SKIP_EXT_BIN_PREREQ";

fn ext_bin_checks_on() -> bool {
    !std::env::var(SKIP_EXT_BIN_ENV)
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn missing_binary(name: &str) -> bool {
    ext_bin_checks_on() && find_binary(name).is_none()
}

fn norm_dir_has_nifti(sub: &Subject, stage: &str) -> Option<String> {
    if !sub.norm_dir.is_dir() {
        return Some(format!(
            "{stage}: no normalization/anat/ (run normalize or add data)"
        ));
    }
    if sub.norm.anat_preop.is_empty() && sub.norm.anat_postop.is_empty() {
        return Some(format!(
            "{stage}: no normalized image under normalization/anat/ (run normalize or add data)"
        ));
    }
    None
}

pub fn prereq_error(sub: &Subject, opts: &RunOptions, stage: Stage) -> Option<String> {
    prereq_error_with(sub, opts, stage, &|| cached_probe().clone())
}

pub fn prereq_error_with(
    sub: &Subject,
    opts: &RunOptions,
    stage: Stage,
    probe: &dyn Fn() -> GpuProbe,
) -> Option<String> {
    match stage {
        Stage::Preprocess => {
            if opts.backend_for(Stage::Preprocess) == RegBackend::Wgpu {
                if let Err(e) = resolve(RegBackend::Wgpu, &probe()) {
                    return Some(format!("preprocess: {e}"));
                }
            }
            if normalize_needs_ants(opts.backend_for(Stage::Preprocess), probe)
                && missing_binary("N4BiasFieldCorrection")
            {
                return Some(
                    "preprocess: N4BiasFieldCorrection not found (bundled or PATH)".to_owned(),
                );
            }
            None
        }
        Stage::Precoreg => {
            if opts.backend_for(Stage::Precoreg) == RegBackend::Wgpu {
                if let Err(e) = resolve(RegBackend::Wgpu, &probe()) {
                    return Some(format!("precoreg: {e}"));
                }
            }
            if normalize_needs_ants(opts.backend_for(Stage::Precoreg), probe)
                && missing_binary("antsRegistration")
            {
                return Some("precoreg: antsRegistration not found (bundled or PATH)".to_owned());
            }
            let t = opts.normalization_template.as_deref();
            if t.is_some_and(std::path::Path::is_file) {
                None
            } else {
                Some(format!("precoreg: {}", missing_template_message(t, None)))
            }
        }
        Stage::Coregister => {
            if opts.backend_for(Stage::Coregister) == RegBackend::Wgpu {
                if let Err(e) = resolve(RegBackend::Wgpu, &probe()) {
                    return Some(format!("coreg: {e}"));
                }
            }
            if normalize_needs_ants(opts.backend_for(Stage::Coregister), probe)
                && missing_binary("antsRegistration")
            {
                return Some("coreg: antsRegistration not found (bundled or PATH)".to_owned());
            }
            None
        }
        Stage::Normalize => {
            if opts.backend_for(Stage::Normalize) == RegBackend::Wgpu {
                if let Err(e) = resolve(RegBackend::Wgpu, &probe()) {
                    return Some(format!("normalize: {e}"));
                }
            }
            if normalize_needs_ants(opts.backend_for(Stage::Normalize), probe) {
                if missing_binary("antsRegistration") {
                    return Some(
                        "normalize: antsRegistration not found (bundled or PATH)".to_owned(),
                    );
                }
                let has_postop = sub.has_postop_ct() || !sub.postop_mr_images().is_empty();
                if has_postop && missing_binary("antsApplyTransforms") {
                    return Some(
                        "normalize: antsApplyTransforms not found (bundled or PATH)".to_owned(),
                    );
                }
            }
            let t = opts.normalization_template.as_deref();
            if t.is_some_and(std::path::Path::is_file) {
                None
            } else {
                Some(format!("normalize: {}", missing_template_message(t, None)))
            }
        }
        Stage::Reconstruct => {
            if !sub.norm_dir.is_dir() {
                return Some(
                    "reconstruct: no normalization/anat/ — run normalize with post-op data so \
                     template-space NIfTIs are written"
                        .to_owned(),
                );
            }
            sub.norm_postop_images().is_empty().then(|| {
                "reconstruct: needs a normalized post-op image (CT or MR, listed by \
                 normalization/log/*_desc-normmethod.json) under normalization/anat/ — only \
                 pre-op template anatomy is not enough for TRAC/CORE on post-op"
                    .to_owned()
            })
        }
        Stage::Brainshift => norm_dir_has_nifti(sub, "brainshift"),
        Stage::Visualize => {
            if let Some(e) = norm_dir_has_nifti(sub, "visualize") {
                return Some(e);
            }
            let r = sub.reconstruction_json();
            (!r.is_file()).then(|| {
                format!(
                    "visualize: missing {} (run reconstruct first; contact QC needs reco v2 JSON)",
                    r.file_name().unwrap_or_default().to_string_lossy()
                )
            })
        }
        Stage::Bids | Stage::Orientation | Stage::Vat => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bids::resolve_subject_with;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_prereq_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("rawdata/sub-1/ses-preop/anat")).unwrap();
        std::fs::write(
            d.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"),
            b"x",
        )
        .unwrap();
        d
    }

    fn sub_and_opts(root: &Path) -> (Subject, RunOptions) {
        let sub = crate::bids::resolve_subject_with(root, "1", false).unwrap();
        (sub, RunOptions::new(root, "1"))
    }

    #[test]
    fn normalize_needs_the_template_file() {
        let root = scratch("norm");
        let (sub, mut opts) = sub_and_opts(&root);
        let e = prereq_error(&sub, &opts, Stage::Normalize).unwrap();
        assert!(e.starts_with("normalize: no normalization template"), "{e}");
        opts.normalization_template = Some(root.join("missing.nii.gz"));
        let e = prereq_error(&sub, &opts, Stage::Normalize).unwrap();
        assert!(e.contains("never replaced"), "{e}");
        let t = root.join("t1.nii.gz");
        std::fs::write(&t, b"x").unwrap();
        opts.normalization_template = Some(t);
        assert!(prereq_error(&sub, &opts, Stage::Normalize).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_wgpu_backend_needs_no_ants_binary_and_fails_early_without_a_gpu() {
        let root = scratch("wgpu");
        let (sub, mut opts) = sub_and_opts(&root);
        let t = root.join("t1.nii.gz");
        std::fs::write(&t, b"x").unwrap();
        opts.normalization_template = Some(t);
        opts.reg_backend = RegBackend::Wgpu;
        let usable = || GpuProbe::usable("Test GPU", "Vulkan", "DiscreteGpu");
        assert!(prereq_error_with(&sub, &opts, Stage::Normalize, &usable).is_none());
        let none = || GpuProbe::unavailable("no hardware adapter found");
        let e = prereq_error_with(&sub, &opts, Stage::Normalize, &none).unwrap();
        assert!(
            e.starts_with("normalize: E_REG_BACKEND") && e.contains("no hardware adapter found"),
            "{e}"
        );
        opts.reg_backend = RegBackend::Auto;
        assert!(prereq_error_with(&sub, &opts, Stage::Normalize, &usable).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_wgpu_preprocess_backend_needs_no_n4_binary_and_fails_early_without_a_gpu() {
        let root = scratch("pre_wgpu");
        let (sub, mut opts) = sub_and_opts(&root);
        let usable = || GpuProbe::usable("Test GPU", "Vulkan", "DiscreteGpu");
        let none = || GpuProbe::unavailable("no hardware adapter found");
        opts.reg_backend = RegBackend::Wgpu;
        assert!(prereq_error_with(&sub, &opts, Stage::Preprocess, &usable).is_none());
        let e = prereq_error_with(&sub, &opts, Stage::Preprocess, &none).unwrap();
        assert!(
            e.starts_with("preprocess: E_REG_BACKEND") && e.contains("no hardware adapter found"),
            "{e}"
        );
        opts.reg_backend = RegBackend::Auto;
        assert!(prereq_error_with(&sub, &opts, Stage::Preprocess, &usable).is_none());
        let have_n4 = find_binary("N4BiasFieldCorrection").is_some();
        let e = prereq_error_with(&sub, &opts, Stage::Preprocess, &none);
        assert_eq!(
            e.as_deref()
                .is_some_and(|m| m.contains("N4BiasFieldCorrection not found")),
            !have_n4 && ext_bin_checks_on(),
            "{e:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_without_a_gpu_and_the_ants_backend_need_the_ants_binary() {
        let root = scratch("ants_needed");
        let (sub, mut opts) = sub_and_opts(&root);
        let t = root.join("t1.nii.gz");
        std::fs::write(&t, b"x").unwrap();
        opts.normalization_template = Some(t);
        let none = || GpuProbe::unavailable("no hardware adapter found");
        let usable = || GpuProbe::usable("Test GPU", "Vulkan", "DiscreteGpu");
        let have_ants = find_binary("antsRegistration").is_some();
        for (backend, probe_usable) in [
            (RegBackend::Auto, false),
            (RegBackend::Ants, true),
            (RegBackend::Ants, false),
        ] {
            opts.reg_backend = backend;
            let e = if probe_usable {
                prereq_error_with(&sub, &opts, Stage::Normalize, &usable)
            } else {
                prereq_error_with(&sub, &opts, Stage::Normalize, &none)
            };
            assert_eq!(
                e.as_deref()
                    .is_some_and(|m| m.contains("antsRegistration not found")),
                !have_ants && ext_bin_checks_on(),
                "{backend:?}: {e:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn normalize_on_ants_with_post_op_images_needs_ants_apply_transforms() {
        let root = scratch("apply");
        let post = root.join("rawdata/sub-1/ses-postop/anat");
        std::fs::create_dir_all(&post).unwrap();
        std::fs::write(post.join("sub-1_ses-postop_CT.nii.gz"), b"x").unwrap();
        let (sub, mut opts) = sub_and_opts(&root);
        let t = root.join("t1.nii.gz");
        std::fs::write(&t, b"x").unwrap();
        opts.normalization_template = Some(t);
        opts.reg_backend = RegBackend::Ants;
        let none = || GpuProbe::unavailable("no hardware adapter found");
        let have_reg = find_binary("antsRegistration").is_some();
        let have_apply = find_binary("antsApplyTransforms").is_some();
        let e = prereq_error_with(&sub, &opts, Stage::Normalize, &none);
        assert_eq!(
            e.as_deref()
                .is_some_and(|m| m == "normalize: antsApplyTransforms not found (bundled or PATH)"),
            have_reg && !have_apply && ext_bin_checks_on(),
            "{e:?}"
        );
        opts.reg_backend = RegBackend::Wgpu;
        let usable = || GpuProbe::usable("Test GPU", "Vulkan", "DiscreteGpu");
        assert!(prereq_error_with(&sub, &opts, Stage::Normalize, &usable).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_coronal_or_sagittal_only_post_op_mri_subject_passes_the_prerequisites() {
        for acq in ["cor", "sag"] {
            let root = scratch(&format!("only_{acq}"));
            let post = root.join("rawdata/sub-1/ses-postop/anat");
            std::fs::create_dir_all(&post).unwrap();
            std::fs::write(
                post.join(format!("sub-1_ses-postop_acq-{acq}_T2w.nii.gz")),
                b"x",
            )
            .unwrap();
            let (sub, mut opts) = sub_and_opts(&root);
            assert_eq!(sub.postop_modality.as_deref(), Some("MRI"), "{acq}");
            let t = root.join("t1.nii.gz");
            std::fs::write(&t, b"x").unwrap();
            opts.normalization_template = Some(t);
            opts.reg_backend = RegBackend::Wgpu;
            let usable = || GpuProbe::usable("Test GPU", "Vulkan", "DiscreteGpu");
            for stage in [
                Stage::Preprocess,
                Stage::Precoreg,
                Stage::Coregister,
                Stage::Normalize,
            ] {
                assert!(
                    prereq_error_with(&sub, &opts, stage, &usable).is_none(),
                    "{acq} {stage:?}"
                );
            }
            opts.reg_backend = RegBackend::Ants;
            let none = || GpuProbe::unavailable("no hardware adapter found");
            let have_reg = find_binary("antsRegistration").is_some();
            let have_apply = find_binary("antsApplyTransforms").is_some();
            let e = prereq_error_with(&sub, &opts, Stage::Normalize, &none);
            assert_eq!(
                e.as_deref().is_some_and(
                    |m| m == "normalize: antsApplyTransforms not found (bundled or PATH)"
                ),
                have_reg && !have_apply && ext_bin_checks_on(),
                "{acq}: {e:?}"
            );
            let key = format!("{acq}_MRI");
            let space = "MNI152NLin2009bAsym";
            std::fs::create_dir_all(crate::derivatives::norm_log_dir(&sub)).unwrap();
            std::fs::write(
                crate::derivatives::norm_method_log(&sub),
                format!(r#"{{"template_space": "{space}"}}"#),
            )
            .unwrap();
            let normalized = crate::derivatives::norm_postop_image(&sub, &key, space);
            std::fs::create_dir_all(normalized.parent().unwrap()).unwrap();
            std::fs::write(&normalized, b"x").unwrap();
            let sub = resolve_subject_with(&root, "1", false).unwrap();
            assert_eq!(sub.norm_postop_mr_in_use(), Some(normalized), "{acq}");
            assert!(
                prereq_error(&sub, &opts, Stage::Reconstruct).is_none(),
                "{acq}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_wgpu_coreg_backend_needs_no_ants_binary_and_fails_early_without_a_gpu() {
        let root = scratch("coreg_wgpu");
        let (sub, mut opts) = sub_and_opts(&root);
        opts.reg_backend = RegBackend::Wgpu;
        let usable = || GpuProbe::usable("Test GPU", "Vulkan", "DiscreteGpu");
        assert!(prereq_error_with(&sub, &opts, Stage::Coregister, &usable).is_none());
        let none = || GpuProbe::unavailable("no hardware adapter found");
        let e = prereq_error_with(&sub, &opts, Stage::Coregister, &none).unwrap();
        assert!(
            e.starts_with("coreg: E_REG_BACKEND") && e.contains("no hardware adapter found"),
            "{e}"
        );
        opts.reg_backend = RegBackend::Auto;
        assert!(prereq_error_with(&sub, &opts, Stage::Coregister, &usable).is_none());
        let have_ants = find_binary("antsRegistration").is_some();
        let e = prereq_error_with(&sub, &opts, Stage::Coregister, &none);
        assert_eq!(
            e.as_deref()
                .is_some_and(|m| m.contains("antsRegistration not found")),
            !have_ants && ext_bin_checks_on(),
            "{e:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_per_step_override_reaches_only_its_own_step() {
        let root = scratch("per_step");
        let (sub, mut opts) = sub_and_opts(&root);
        let t = root.join("t1.nii.gz");
        std::fs::write(&t, b"x").unwrap();
        opts.normalization_template = Some(t);
        let none = || GpuProbe::unavailable("no hardware adapter found");
        let gpu_error = |opts: &RunOptions, stage| {
            prereq_error_with(&sub, opts, stage, &none).is_some_and(|m| m.contains("E_REG_BACKEND"))
        };
        opts.reg_backend = RegBackend::Wgpu;
        opts.reg_backend_normalize = Some(RegBackend::Ants);
        assert!(gpu_error(&opts, Stage::Preprocess));
        assert!(gpu_error(&opts, Stage::Precoreg));
        assert!(gpu_error(&opts, Stage::Coregister));
        assert!(!gpu_error(&opts, Stage::Normalize));
        opts.reg_backend = RegBackend::Ants;
        opts.reg_backend_normalize = Some(RegBackend::Wgpu);
        opts.reg_backend_preprocess = None;
        assert!(!gpu_error(&opts, Stage::Preprocess));
        assert!(!gpu_error(&opts, Stage::Precoreg));
        assert!(!gpu_error(&opts, Stage::Coregister));
        assert!(gpu_error(&opts, Stage::Normalize));
        opts.reg_backend_coregister = Some(RegBackend::Wgpu);
        opts.reg_backend_preprocess = Some(RegBackend::Wgpu);
        assert!(gpu_error(&opts, Stage::Coregister));
        assert!(gpu_error(&opts, Stage::Precoreg));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconstruct_and_visualize_need_normalized_postop() {
        let root = scratch("recon");
        let (sub, opts) = sub_and_opts(&root);
        let e = prereq_error(&sub, &opts, Stage::Reconstruct).unwrap();
        assert!(e.contains("no normalization/anat/"), "{e}");
        std::fs::create_dir_all(&sub.norm_dir).unwrap();
        let space = "MNI152NLin2009bAsym";
        std::fs::create_dir_all(crate::derivatives::norm_log_dir(&sub)).unwrap();
        std::fs::write(
            crate::derivatives::norm_method_log(&sub),
            format!(r#"{{"template_space": "{space}"}}"#),
        )
        .unwrap();
        std::fs::write(crate::derivatives::norm_anchor_image(&sub, space), b"x").unwrap();
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let e = prereq_error(&sub, &opts, Stage::Reconstruct).unwrap();
        assert!(e.contains("needs a normalized post-op image"), "{e}");
        let post = root.join("rawdata/sub-1/ses-postop/anat");
        std::fs::create_dir_all(&post).unwrap();
        std::fs::write(post.join("sub-1_ses-postop_CT.nii.gz"), b"x").unwrap();
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        std::fs::write(
            crate::derivatives::norm_postop_image(&sub, "CT", space),
            b"x",
        )
        .unwrap();
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        assert!(prereq_error(&sub, &opts, Stage::Reconstruct).is_none());
        let e = prereq_error(&sub, &opts, Stage::Visualize).unwrap();
        assert!(e.contains("missing sub-1_desc-reconstruction.json"), "{e}");
        assert!(prereq_error(&sub, &opts, Stage::Orientation).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn precoreg_needs_the_template_and_follows_the_backend_rule() {
        let root = scratch("precoreg");
        let (sub, mut opts) = sub_and_opts(&root);
        let usable = || GpuProbe::usable("Test GPU", "Vulkan", "DiscreteGpu");
        let none = || GpuProbe::unavailable("no hardware adapter found");
        opts.reg_backend = RegBackend::Wgpu;
        let e = prereq_error_with(&sub, &opts, Stage::Precoreg, &none).unwrap();
        assert!(
            e.starts_with("precoreg: E_REG_BACKEND") && e.contains("no hardware adapter found"),
            "{e}"
        );
        let e = prereq_error_with(&sub, &opts, Stage::Precoreg, &usable).unwrap();
        assert!(e.starts_with("precoreg: no normalization template"), "{e}");
        let t = root.join("t1.nii.gz");
        std::fs::write(&t, b"x").unwrap();
        opts.normalization_template = Some(t);
        assert!(prereq_error_with(&sub, &opts, Stage::Precoreg, &usable).is_none());
        opts.reg_backend = RegBackend::Ants;
        let have_ants = find_binary("antsRegistration").is_some();
        let e = prereq_error_with(&sub, &opts, Stage::Precoreg, &usable);
        assert_eq!(
            e.as_deref()
                .is_some_and(|m| m.starts_with("precoreg: antsRegistration not found")),
            !have_ants && ext_bin_checks_on(),
            "{e:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
