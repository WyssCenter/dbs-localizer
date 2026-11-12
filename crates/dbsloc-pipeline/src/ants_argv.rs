// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Argument vectors for ANTs / N4 (`specs/02` §4.1, `specs/03` §5.1–§5.2, `specs/04` §5).
//!
//! The only argv builders in Wyss DBS Localizer; Python reaches them through the `dbsloc_reconstruct`
//! extension.

use std::path::Path;

/// Fixed `antsRegistration --random-seed`.
///
/// ANTs seeds from the **system clock** by default and the MI metric samples voxels randomly,
/// so an unseeded run is never repeatable. A seed alone is **not** sufficient: measured
/// 2026-09-10 on a synthetic pair, hashing the warped output over three runs each, `seed + 1
/// ITK thread` was identical 3/3 while `seed + 2 threads` and `no seed + 1 thread` both varied
/// every time. The multi-threaded metric sums in a nondeterministic order and floating-point
/// addition is not associative, so bit-identical output needs the seed **and**
/// `ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS=1` (`RunOptions::ants_max_threads = Some(1)`). The
/// seed is set unconditionally because it is free and necessary; single-threading is not,
/// because it costs real time on a 0.5 mm template. ANTs requires a nonzero value.
pub const ANTS_RANDOM_SEED: u32 = 20260910;

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Build `N4BiasFieldCorrection` argv.
pub fn build_n4_argv(n4_bin: &Path, input: &Path, output: &Path) -> Vec<String> {
    vec![
        s(n4_bin),
        "-d".into(),
        "3".into(),
        "-i".into(),
        s(input),
        "-o".into(),
        s(output),
    ]
}

fn mi_stage(fix: &str, mov: &str) -> Vec<String> {
    vec![
        "--metric".into(),
        format!("MI[{fix},{mov},1,32,Regular,0.25]"),
        "--convergence".into(),
        "[1000x500x250x100,1e-6,10]".into(),
        "--shrink-factors".into(),
        "8x4x2x1".into(),
        "--smoothing-sigmas".into(),
        "3x2x1x0vox".into(),
    ]
}

/// Build `antsRegistration` argv for **rigid** CT-to-anchor coregistration (`specs/03` §5.2).
///
/// The CT and the MR image the same head, so the map is rigid; the affine stage this once
/// carried measured scale errors of 6–11 % on five of ten Lead-Tutor subjects (2026-09-11)
/// and displaced one lead by 7 mm.
///
/// `init` = `None` keeps the centre-of-mass initialiser `[fixed,moving,1]`; `Some(path)` passes
/// the transform file verbatim (`specs/03` §5.1 step 4: the best multi-start coarse result).
/// `fixed_mask` adds `--masks [<fixed_mask>,NULL]` (same step: the eroded non-zero anchor mask,
/// so defaced blank regions do not enter the metric).
pub fn build_ants_ct_coreg_argv(
    ants_bin: &Path,
    moving: &Path,
    fixed: &Path,
    out_prefix: &Path,
    init: Option<&Path>,
    fixed_mask: Option<&Path>,
    extra: &[String],
) -> Vec<String> {
    let pfx = s(out_prefix);
    let mov = s(moving);
    let fix = s(fixed);
    let init_arg = match init {
        Some(p) => s(p),
        None => format!("[{fix},{mov},1]"),
    };
    let mut v = vec![
        s(ants_bin),
        "--dimensionality".into(),
        "3".into(),
        "--output".into(),
        format!("[{pfx},{pfx}_Warped.nii.gz]"),
        "--initial-moving-transform".into(),
        init_arg,
        "--transform".into(),
        "Rigid[0.1]".into(),
    ];
    v.extend(mi_stage(&fix, &mov));
    if let Some(mask) = fixed_mask {
        v.push("--masks".into());
        v.push(format!("[{},NULL]", s(mask)));
    }
    v.push("--random-seed".into());
    v.push(ANTS_RANDOM_SEED.to_string());
    v.extend(extra.iter().cloned());
    v
}

/// Coarse rigid schedule of the CT → anchor multi-start (`specs/03` §5.1 step 3).
pub const COARSE_RIGID_TRANSFORM: &str = "Rigid[0.1]";
pub const COARSE_RIGID_CONVERGENCE: &str = "[200x100,1e-6,10]";
pub const COARSE_RIGID_SHRINK_FACTORS: &str = "8x4";
pub const COARSE_RIGID_SMOOTHING_SIGMAS: &str = "3x2vox";

/// One coarse rigid start of the CT → anchor multi-start (`specs/03` §5.1 step 3).
///
/// Two levels, no warped image (`--output [<work_prefix>]`), `--verbose 1` so the
/// `DIAGNOSTIC` lines carrying the metric reach stdout; `extra` is the same pass-through list
/// the full stage receives.
pub fn build_ants_ct_coarse_argv(
    ants_bin: &Path,
    moving: &Path,
    fixed: &Path,
    work_prefix: &Path,
    init: &Path,
    extra: &[String],
) -> Vec<String> {
    let pfx = s(work_prefix);
    let mov = s(moving);
    let fix = s(fixed);
    let mut v = vec![
        s(ants_bin),
        "--dimensionality".into(),
        "3".into(),
        "--output".into(),
        format!("[{pfx}]"),
        "--initial-moving-transform".into(),
        s(init),
        "--transform".into(),
        COARSE_RIGID_TRANSFORM.into(),
        "--metric".into(),
        format!("MI[{fix},{mov},1,32,Regular,0.25]"),
        "--convergence".into(),
        COARSE_RIGID_CONVERGENCE.into(),
        "--shrink-factors".into(),
        COARSE_RIGID_SHRINK_FACTORS.into(),
        "--smoothing-sigmas".into(),
        COARSE_RIGID_SMOOTHING_SIGMAS.into(),
        "--random-seed".into(),
        ANTS_RANDOM_SEED.to_string(),
        "--verbose".into(),
        "1".into(),
    ];
    v.extend(extra.iter().cloned());
    v
}

pub fn build_ants_mr_coreg_argv(
    ants_bin: &Path,
    moving: &Path,
    fixed: &Path,
    out_prefix: &Path,
    extra: &[String],
) -> Vec<String> {
    build_ants_ct_coreg_argv(ants_bin, moving, fixed, out_prefix, None, None, extra)
}

pub const PRECOREG_CONVERGENCE: &str = "[1000x500x250,1e-6,10]";
pub const PRECOREG_SHRINK_FACTORS: &str = "4x2x1";
pub const PRECOREG_SMOOTHING_SIGMAS: &str = "2x1x0vox";

pub fn build_ants_precoreg_argv(
    ants_bin: &Path,
    moving: &Path,
    fixed: &Path,
    out_prefix: &Path,
    extra: &[String],
) -> Vec<String> {
    let mov = s(moving);
    let fix = s(fixed);
    let mut v = vec![
        s(ants_bin),
        "--dimensionality".into(),
        "3".into(),
        "--output".into(),
        format!("[{}]", s(out_prefix)),
        "--initial-moving-transform".into(),
        format!("[{fix},{mov},1]"),
        "--transform".into(),
        "Rigid[0.1]".into(),
        "--metric".into(),
        format!("MI[{fix},{mov},1,32,Regular,0.25]"),
        "--convergence".into(),
        PRECOREG_CONVERGENCE.into(),
        "--shrink-factors".into(),
        PRECOREG_SHRINK_FACTORS.into(),
        "--smoothing-sigmas".into(),
        PRECOREG_SMOOTHING_SIGMAS.into(),
        "--random-seed".into(),
        ANTS_RANDOM_SEED.to_string(),
    ];
    v.extend(extra.iter().cloned());
    v
}

pub const MULTISPECTRAL_CC_WEIGHT: &str = "0.5";

/// Build `antsRegistration` argv for Rigid + Affine + SyN normalization (`specs/04` §5).
pub fn build_ants_syn_argv(
    ants_bin: &Path,
    moving: &Path,
    fixed: &Path,
    out_prefix: &Path,
    extra: &[String],
) -> Vec<String> {
    syn_argv(ants_bin, moving, fixed, None, out_prefix, extra)
}

pub fn build_ants_syn_multispectral_argv(
    ants_bin: &Path,
    moving: &Path,
    fixed: &Path,
    moving_t2: &Path,
    fixed_t2: &Path,
    out_prefix: &Path,
    extra: &[String],
) -> Vec<String> {
    syn_argv(
        ants_bin,
        moving,
        fixed,
        Some((moving_t2, fixed_t2)),
        out_prefix,
        extra,
    )
}

fn syn_argv(
    ants_bin: &Path,
    moving: &Path,
    fixed: &Path,
    t2: Option<(&Path, &Path)>,
    out_prefix: &Path,
    extra: &[String],
) -> Vec<String> {
    let pfx = s(out_prefix);
    let mov = s(moving);
    let fix = s(fixed);
    let mut v = vec![
        s(ants_bin),
        "--dimensionality".into(),
        "3".into(),
        "--output".into(),
        format!("[{pfx},{pfx}_Warped.nii.gz]"),
        "--winsorize-image-intensities".into(),
        "[0.005,0.995]".into(),
        "--use-histogram-matching".into(),
        "0".into(),
        "--initial-moving-transform".into(),
        format!("[{fix},{mov},1]"),
        "--transform".into(),
        "Rigid[0.1]".into(),
    ];
    v.extend(mi_stage(&fix, &mov));
    v.extend(["--transform".into(), "Affine[0.1]".into()]);
    v.extend(mi_stage(&fix, &mov));
    v.extend(["--transform".into(), "SyN[0.1,3,0]".into()]);
    match t2 {
        None => v.extend(["--metric".into(), format!("CC[{fix},{mov},1,4]")]),
        Some((mov2, fix2)) => {
            let w = MULTISPECTRAL_CC_WEIGHT;
            v.extend([
                "--metric".into(),
                format!("CC[{fix},{mov},{w},4]"),
                "--metric".into(),
                format!("CC[{},{},{w},4]", s(fix2), s(mov2)),
            ]);
        }
    }
    v.extend([
        "--convergence".into(),
        "[100x70x50x20,1e-6,10]".into(),
        "--shrink-factors".into(),
        "8x4x2x1".into(),
        "--smoothing-sigmas".into(),
        "3x2x1x0vox".into(),
        "--write-composite-transform".into(),
        "1".into(),
        "--random-seed".into(),
        ANTS_RANDOM_SEED.to_string(),
    ]);
    v.extend(extra.iter().cloned());
    v
}

const APPLY_TRANSFORMS_PASS_THROUGH: [&str; 5] = [
    "--float",
    "--verbose",
    "--default-value",
    "--output-data-type",
    "--input-image-type",
];

pub fn apply_transforms_extra(extra: &[String]) -> Vec<String> {
    let mut keep = false;
    extra
        .iter()
        .filter(|token| {
            if token.starts_with("--") {
                keep = APPLY_TRANSFORMS_PASS_THROUGH.contains(&token.as_str());
            }
            keep
        })
        .cloned()
        .collect()
}

/// Build `antsApplyTransforms` argv to warp a CT into template space.
pub fn build_ants_apply_transforms_argv(
    apply_bin: &Path,
    input: &Path,
    reference: &Path,
    output: &Path,
    transform: &Path,
    extra: &[String],
) -> Vec<String> {
    let mut v = vec![
        s(apply_bin),
        "-d".into(),
        "3".into(),
        "-i".into(),
        s(input),
        "-r".into(),
        s(reference),
        "-o".into(),
        s(output),
        "-n".into(),
        "Linear".into(),
        "-t".into(),
        s(transform),
    ];
    v.extend(apply_transforms_extra(extra));
    v
}

/// `antsApplyTransforms` argv that resamples a brain mask (linear) onto `reference`;
/// `invert_affine` wraps `transform` as `[<file>,1]`, the inverse of a `.mat` affine.
pub fn build_ants_mask_apply_argv(
    apply_bin: &Path,
    input: &Path,
    reference: &Path,
    output: &Path,
    transform: &Path,
    invert_affine: bool,
    extra: &[String],
) -> Vec<String> {
    let token = if invert_affine {
        format!("[{},1]", transform.display())
    } else {
        s(transform)
    };
    let mut v = vec![
        s(apply_bin),
        "-d".into(),
        "3".into(),
        "-i".into(),
        s(input),
        "-r".into(),
        s(reference),
        "-o".into(),
        s(output),
        "-n".into(),
        "Linear".into(),
        "-t".into(),
        token,
    ];
    v.extend(apply_transforms_extra(extra));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(x: &str) -> PathBuf {
        PathBuf::from(x)
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn apply_transforms_receives_only_the_pass_through_options_it_accepts() {
        let extra = strs(&[
            "--use-histogram-matching",
            "1",
            "--float",
            "1",
            "--winsorize-image-intensities",
            "[0.01,0.99]",
            "--default-value",
            "-1",
            "--verbose",
        ]);
        assert_eq!(
            apply_transforms_extra(&extra),
            strs(&["--float", "1", "--default-value", "-1", "--verbose"])
        );
        let a = build_ants_apply_transforms_argv(
            &p("antsApplyTransforms"),
            &p("/c.nii.gz"),
            &p("/t.nii"),
            &p("/o.nii.gz"),
            &p("/x.h5"),
            &extra,
        );
        assert!(!a
            .iter()
            .any(|x| x.contains("histogram") || x.contains("winsorize")));
        let m = build_ants_mask_apply_argv(
            &p("antsApplyTransforms"),
            &p("/m.nii.gz"),
            &p("/t.nii"),
            &p("/o.nii.gz"),
            &p("/x.mat"),
            true,
            &extra,
        );
        assert_eq!(
            m[m.len() - 5..],
            strs(&["--float", "1", "--default-value", "-1", "--verbose"])[..]
        );
        let r = build_ants_syn_argv(&p("a"), &p("/m"), &p("/f"), &p("/o"), &extra);
        assert_eq!(r[r.len() - extra.len()..], extra[..]);
    }

    #[test]
    fn precoreg_argv_matches_the_snapshot() {
        let got = build_ants_precoreg_argv(
            &p("antsRegistration"),
            &p("/m.nii.gz"),
            &p("/f.nii.gz"),
            &p("/o/.partial-pfx"),
            &strs(&["--verbose", "1"]),
        );
        let want = strs(&[
            "antsRegistration",
            "--dimensionality",
            "3",
            "--output",
            "[/o/.partial-pfx]",
            "--initial-moving-transform",
            "[/f.nii.gz,/m.nii.gz,1]",
            "--transform",
            "Rigid[0.1]",
            "--metric",
            "MI[/f.nii.gz,/m.nii.gz,1,32,Regular,0.25]",
            "--convergence",
            "[1000x500x250,1e-6,10]",
            "--shrink-factors",
            "4x2x1",
            "--smoothing-sigmas",
            "2x1x0vox",
            "--random-seed",
            "20260910",
            "--verbose",
            "1",
        ]);
        assert_eq!(got, want);
    }

    #[test]
    fn full_coreg_argv_matches_the_python_snapshot() {
        let got = build_ants_ct_coreg_argv(
            &p("antsRegistration"),
            &p("/m.nii.gz"),
            &p("/f.nii.gz"),
            &p("/o/pfx"),
            Some(&p("/o/_multistart/coarse_p20_0GenericAffine.mat")),
            Some(&p("/o/_multistart/fixed_mask.nii.gz")),
            &strs(&["--verbose", "1"]),
        );
        let want = strs(&[
            "antsRegistration",
            "--dimensionality",
            "3",
            "--output",
            "[/o/pfx,/o/pfx_Warped.nii.gz]",
            "--initial-moving-transform",
            "/o/_multistart/coarse_p20_0GenericAffine.mat",
            "--transform",
            "Rigid[0.1]",
            "--metric",
            "MI[/f.nii.gz,/m.nii.gz,1,32,Regular,0.25]",
            "--convergence",
            "[1000x500x250x100,1e-6,10]",
            "--shrink-factors",
            "8x4x2x1",
            "--smoothing-sigmas",
            "3x2x1x0vox",
            "--masks",
            "[/o/_multistart/fixed_mask.nii.gz,NULL]",
            "--random-seed",
            "20260910",
            "--verbose",
            "1",
        ]);
        assert_eq!(got, want);
        let com = build_ants_ct_coreg_argv(
            &p("antsRegistration"),
            &p("/m.nii.gz"),
            &p("/f.nii.gz"),
            &p("/o/pfx"),
            None,
            None,
            &[],
        );
        assert_eq!(com[6], "[/f.nii.gz,/m.nii.gz,1]");
        assert!(!com.iter().any(|a| a == "--masks"));
        for gone in [
            "--float",
            "--interpolation",
            "--winsorize-image-intensities",
            "--use-histogram-matching",
        ] {
            assert!(!com.iter().any(|a| a == gone), "{gone}");
        }
    }

    #[test]
    fn coarse_argv_matches_the_python_snapshot_for_every_start() {
        for tag in ["m40", "m20", "p00", "p20", "p40"] {
            let work = format!("/o/_multistart/coarse_{tag}_");
            let start = format!("/o/_multistart/start_{tag}.txt");
            let got = build_ants_ct_coarse_argv(
                &p("antsRegistration"),
                &p("/m.nii.gz"),
                &p("/f.nii.gz"),
                &p(&work),
                &p(&start),
                &[],
            );
            let want = strs(&[
                "antsRegistration",
                "--dimensionality",
                "3",
                "--output",
                &format!("[{work}]"),
                "--initial-moving-transform",
                &start,
                "--transform",
                "Rigid[0.1]",
                "--metric",
                "MI[/f.nii.gz,/m.nii.gz,1,32,Regular,0.25]",
                "--convergence",
                "[200x100,1e-6,10]",
                "--shrink-factors",
                "8x4",
                "--smoothing-sigmas",
                "3x2vox",
                "--random-seed",
                "20260910",
                "--verbose",
                "1",
            ]);
            assert_eq!(got, want, "{tag}");
        }
        let extra = build_ants_ct_coarse_argv(
            &p("a"),
            &p("m"),
            &p("f"),
            &p("w"),
            &p("s"),
            &strs(&["--float", "1"]),
        );
        assert_eq!(&extra[extra.len() - 2..], &strs(&["--float", "1"])[..]);
    }

    #[test]
    fn syn_argv_matches_the_python_snapshot() {
        let got = build_ants_syn_argv(
            &p("antsRegistration"),
            &p("/m.nii.gz"),
            &p("/t.nii.gz"),
            &p("/n/pfx"),
            &strs(&["--float", "1"]),
        );
        let mi = "MI[/t.nii.gz,/m.nii.gz,1,32,Regular,0.25]";
        let want = strs(&[
            "antsRegistration",
            "--dimensionality",
            "3",
            "--output",
            "[/n/pfx,/n/pfx_Warped.nii.gz]",
            "--winsorize-image-intensities",
            "[0.005,0.995]",
            "--use-histogram-matching",
            "0",
            "--initial-moving-transform",
            "[/t.nii.gz,/m.nii.gz,1]",
            "--transform",
            "Rigid[0.1]",
            "--metric",
            mi,
            "--convergence",
            "[1000x500x250x100,1e-6,10]",
            "--shrink-factors",
            "8x4x2x1",
            "--smoothing-sigmas",
            "3x2x1x0vox",
            "--transform",
            "Affine[0.1]",
            "--metric",
            mi,
            "--convergence",
            "[1000x500x250x100,1e-6,10]",
            "--shrink-factors",
            "8x4x2x1",
            "--smoothing-sigmas",
            "3x2x1x0vox",
            "--transform",
            "SyN[0.1,3,0]",
            "--metric",
            "CC[/t.nii.gz,/m.nii.gz,1,4]",
            "--convergence",
            "[100x70x50x20,1e-6,10]",
            "--shrink-factors",
            "8x4x2x1",
            "--smoothing-sigmas",
            "3x2x1x0vox",
            "--write-composite-transform",
            "1",
            "--random-seed",
            "20260910",
            "--float",
            "1",
        ]);
        assert_eq!(got, want);
    }

    #[test]
    fn mr_coreg_argv_is_the_ct_full_stage_with_com_init_and_no_mask() {
        let got = build_ants_mr_coreg_argv(
            &p("antsRegistration"),
            &p("/raw/sub-1_ses-preop_T2w.nii.gz"),
            &p("/pre/sub-1_ses-preop_T1w_N4.nii.gz"),
            &p("/c/sub-1_T2w_to_T1w"),
            &strs(&["--float", "1"]),
        );
        let want = strs(&[
            "antsRegistration",
            "--dimensionality",
            "3",
            "--output",
            "[/c/sub-1_T2w_to_T1w,/c/sub-1_T2w_to_T1w_Warped.nii.gz]",
            "--initial-moving-transform",
            "[/pre/sub-1_ses-preop_T1w_N4.nii.gz,/raw/sub-1_ses-preop_T2w.nii.gz,1]",
            "--transform",
            "Rigid[0.1]",
            "--metric",
            "MI[/pre/sub-1_ses-preop_T1w_N4.nii.gz,/raw/sub-1_ses-preop_T2w.nii.gz,1,32,Regular,0.25]",
            "--convergence",
            "[1000x500x250x100,1e-6,10]",
            "--shrink-factors",
            "8x4x2x1",
            "--smoothing-sigmas",
            "3x2x1x0vox",
            "--random-seed",
            "20260910",
            "--float",
            "1",
        ]);
        assert_eq!(got, want);
        assert_eq!(
            got,
            build_ants_ct_coreg_argv(
                &p("antsRegistration"),
                &p("/raw/sub-1_ses-preop_T2w.nii.gz"),
                &p("/pre/sub-1_ses-preop_T1w_N4.nii.gz"),
                &p("/c/sub-1_T2w_to_T1w"),
                None,
                None,
                &strs(&["--float", "1"]),
            )
        );
    }

    #[test]
    fn multispectral_syn_argv_adds_one_weighted_cc_per_channel_to_syn_only() {
        let single = build_ants_syn_argv(
            &p("antsRegistration"),
            &p("/m.nii.gz"),
            &p("/t.nii.gz"),
            &p("/n/pfx"),
            &strs(&["--float", "1"]),
        );
        let multi = build_ants_syn_multispectral_argv(
            &p("antsRegistration"),
            &p("/m.nii.gz"),
            &p("/t.nii.gz"),
            &p("/m2.nii.gz"),
            &p("/t2.nii.gz"),
            &p("/n/pfx"),
            &strs(&["--float", "1"]),
        );
        let syn = single.iter().position(|a| a == "SyN[0.1,3,0]").unwrap();
        assert_eq!(multi[..=syn], single[..=syn]);
        assert_eq!(
            multi[syn + 1..syn + 5],
            strs(&[
                "--metric",
                "CC[/t.nii.gz,/m.nii.gz,0.5,4]",
                "--metric",
                "CC[/t2.nii.gz,/m2.nii.gz,0.5,4]",
            ])[..]
        );
        assert_eq!(multi[syn + 5..], single[syn + 3..]);
        assert_eq!(multi.len(), single.len() + 2);
        assert_eq!(
            multi.iter().filter(|a| a.starts_with("MI[")).count(),
            2,
            "rigid and affine stay T1-only"
        );
        assert!(!multi
            .iter()
            .any(|a| a.contains("m2.nii.gz") && a.starts_with("MI[")));
    }

    #[test]
    fn apply_and_n4_argv() {
        let a = build_ants_apply_transforms_argv(
            &p("antsApplyTransforms"),
            &p("/c.nii.gz"),
            &p("/t.nii"),
            &p("/o.nii.gz"),
            &p("/n/pfxComposite.h5"),
            &strs(&["--float", "1"]),
        );
        assert_eq!(
            a,
            strs(&[
                "antsApplyTransforms",
                "-d",
                "3",
                "-i",
                "/c.nii.gz",
                "-r",
                "/t.nii",
                "-o",
                "/o.nii.gz",
                "-n",
                "Linear",
                "-t",
                "/n/pfxComposite.h5",
                "--float",
                "1"
            ])
        );
        let n4 = build_n4_argv(&p("/ants/N4"), &p("/in.nii.gz"), &p("/out.nii.gz"));
        assert_eq!(
            &n4[1..],
            &["-d", "3", "-i", "/in.nii.gz", "-o", "/out.nii.gz"]
        );
    }

    #[test]
    fn every_registration_is_seeded() {
        let bin = p("antsRegistration");
        let x = p("x.nii.gz");
        for argv in [
            build_ants_ct_coreg_argv(&bin, &x, &x, &p("pfx"), None, None, &[]),
            build_ants_ct_coarse_argv(&bin, &x, &x, &p("w"), &x, &[]),
            build_ants_syn_argv(&bin, &x, &x, &p("pfx"), &[]),
            build_ants_mr_coreg_argv(&bin, &x, &x, &p("pfx"), &[]),
            build_ants_syn_multispectral_argv(&bin, &x, &x, &x, &x, &p("pfx"), &[]),
        ] {
            let i = argv
                .iter()
                .position(|a| a == "--random-seed")
                .expect("seeded");
            assert_eq!(argv[i + 1], ANTS_RANDOM_SEED.to_string());
        }
    }

    #[test]
    fn native_recipes_render_the_syn_argv() {
        use fastwarp_reg::linear::{LinearRecipe, COHORT_RANDOM_SEED};
        use fastwarp_reg::recipe::SynRecipe;

        assert_eq!(COHORT_RANDOM_SEED, ANTS_RANDOM_SEED);
        let bin = p("antsRegistration");
        let (mov, fix) = (p("m.nii.gz"), p("f.nii.gz"));
        let (mov2, fix2) = (p("m2.nii.gz"), p("f2.nii.gz"));
        let (sm, sf) = (s(&mov), s(&fix));
        let (sm2, sf2) = (s(&mov2), s(&fix2));

        let transforms = |argv: &[String]| -> Vec<usize> {
            argv.iter()
                .enumerate()
                .filter(|(_, a)| a.as_str() == "--transform")
                .map(|(i, _)| i)
                .collect()
        };
        let slice_to_sigmas = |argv: &[String], from: usize| -> Vec<String> {
            let end = argv[from..]
                .iter()
                .position(|a| a == "--smoothing-sigmas")
                .expect("sigmas")
                + from
                + 2;
            argv[from..end].to_vec()
        };

        let t1 = build_ants_syn_argv(&bin, &mov, &fix, &p("pfx"), &[]);
        let at = transforms(&t1);
        assert_eq!(at.len(), 3);
        assert_eq!(
            slice_to_sigmas(&t1, at[0]),
            LinearRecipe::cohort_rigid().to_ants_args(&sf, &sm)
        );
        assert_eq!(
            slice_to_sigmas(&t1, at[1]),
            LinearRecipe::cohort_affine().to_ants_args(&sf, &sm)
        );
        assert_eq!(
            slice_to_sigmas(&t1, at[2]),
            SynRecipe::cohort().to_ants_args(&[(sf.as_str(), sm.as_str())])
        );

        let ms = build_ants_syn_multispectral_argv(&bin, &mov, &fix, &mov2, &fix2, &p("pfx"), &[]);
        let at = transforms(&ms);
        assert_eq!(
            slice_to_sigmas(&ms, at[2]),
            SynRecipe::cohort_multispectral()
                .to_ants_args(&[(sf.as_str(), sm.as_str()), (sf2.as_str(), sm2.as_str())])
        );
    }
}
