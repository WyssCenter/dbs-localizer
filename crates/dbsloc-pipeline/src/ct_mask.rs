// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use fastwarp_reg::apply::{apply_nifti, Interpolation};
use fastwarp_reg::transform::{read_transform_file, TransformChain, TransformElem};
use ndarray::Array3;

use crate::ants_argv::build_ants_mask_apply_argv;
use crate::bids::Subject;
use crate::ct_recon::{WarpRoute, BRAIN_MASK_NAME};
use crate::derivatives;
use crate::error::{PipelineError, PipelineResult};
use crate::nifti_io::{load_mask, write_u8_3d};
use crate::options::RunOptions;
use crate::outputs::Staged;
use crate::protocol::RunEvent;
use crate::space_bridge::Bridge;
use crate::stage_impl::{command_from, file_name_of, log, require_binary, spawn_logged};
use crate::template_catalog::{default_entry, entry_t1_path, infer_space, REFERENCE_SPACE};

pub const NO_MASK_NOTE: &str = "dbsloc: note: no template brain mask; PaCER computes its own";
pub const MASK_THRESHOLD: f64 = 0.5;
const WORK_TAG: &str = "mkw";
const ANCHOR_TAG: &str = "mka";
const CT_TAG: &str = "mkc";
const MASK_CACHE_DIR: &str = "masks";

type Exec<'a> = dyn FnMut(&[String]) -> PipelineResult<()> + 'a;
type BridgeLookup<'a> = dyn FnMut() -> Result<Option<Bridge>, String> + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtMasks {
    pub raw_ct: PathBuf,
    pub anchor: PathBuf,
}

impl CtMasks {
    pub fn candidates(&self) -> Vec<PathBuf> {
        vec![self.raw_ct.clone(), self.anchor.clone()]
    }
}

fn message(text: impl Into<String>) -> PipelineError {
    PipelineError::Message(text.into())
}

fn short_hash(path: &Path) -> Result<String, String> {
    crate::sha256::file_sha256_hex(path)
        .map(|h| h[..12].to_owned())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn threshold_into(src: &Path, dest: &Path) -> Result<u64, String> {
    let (above, aff) = load_mask(src, MASK_THRESHOLD).map_err(|e| e.to_string())?;
    let mask: Array3<u8> = above.mapv(u8::from);
    let count = mask.iter().map(|v| u64::from(*v)).sum();
    write_u8_3d(dest, &mask, &aff).map_err(|e| e.to_string())?;
    Ok(count)
}

pub(crate) fn template_mask_with(
    template: &Path,
    reference_t1: Option<&Path>,
    bridge: &mut BridgeLookup<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<Option<PathBuf>, String> {
    if let Some(beside) = template
        .parent()
        .map(|d| d.join(BRAIN_MASK_NAME))
        .filter(|p| p.is_file())
    {
        return Ok(Some(beside));
    }
    if infer_space(template) == REFERENCE_SPACE {
        return Ok(None);
    }
    let Some(reference) = reference_t1
        .and_then(Path::parent)
        .map(|d| d.join(BRAIN_MASK_NAME))
        .filter(|p| p.is_file())
    else {
        return Ok(None);
    };
    let Some(bridge) = bridge()? else {
        return Ok(None);
    };
    let dir = bridge.dir.join(MASK_CACHE_DIR);
    let cached = dir.join(format!("brainmask-{}.nii.gz", short_hash(&reference)?));
    if cached.is_file() {
        return Ok(Some(cached));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    log(
        tx,
        format!(
            "dbsloc: note: reconstruct: bridging the {REFERENCE_SPACE} brain mask into {}",
            bridge.target_space
        ),
    );
    let work = dir.join(format!(
        "{}{WORK_TAG}.nii.gz",
        crate::outputs::PARTIAL_PREFIX
    ));
    let staged = Staged::new(vec![cached.clone()]).map_err(|e| e.to_string())?;
    let result = (|| {
        bridge.warp_image_to_target(&reference, template, &work, false)?;
        threshold_into(&work, &staged.tmp(&cached)).map(|_| ())
    })();
    let _ = std::fs::remove_file(&work);
    staged
        .finish(result.map_err(PipelineError::Message), None)
        .map_err(|e| e.to_string())?;
    Ok(Some(cached))
}

pub(crate) fn template_mask(
    template: &Path,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<Option<PathBuf>, PipelineError> {
    let reference_t1 = entry_t1_path(default_entry());
    let mut cancelled = false;
    let mut lookup = || {
        crate::space_bridge::ensure_bridge(template, opts, cancel, tx).map_err(|e| match e {
            PipelineError::Cancelled => {
                cancelled = true;
                "cancelled".to_owned()
            }
            other => other.to_string(),
        })
    };
    let got = template_mask_with(template, reference_t1.as_deref(), &mut lookup, tx);
    if cancelled {
        return Err(PipelineError::Cancelled);
    }
    match got {
        Ok(found) => Ok(found),
        Err(why) => {
            log(
                tx,
                format!("dbsloc: warning: reconstruct: template brain mask unavailable: {why}"),
            );
            Ok(None)
        }
    }
}

enum Apply {
    InProcess,
    Ants,
}

pub(crate) struct ApplyPlan<'a> {
    pub route: WarpRoute,
    pub apply_bin: &'a dyn Fn() -> PipelineResult<PathBuf>,
}

fn read_chain(path: &Path, invert_affine: bool) -> Result<TransformChain, String> {
    let chain = read_transform_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !invert_affine {
        return Ok(chain);
    }
    match chain.queue().first() {
        Some(TransformElem::Affine(a)) => {
            let inverse = a
                .inverse()
                .map_err(|e| format!("{} is not invertible: {e}", path.display()))?;
            Ok(TransformChain::from_queue(vec![TransformElem::Affine(
                inverse,
            )]))
        }
        _ => Err(format!("{} holds no affine transform", path.display())),
    }
}

struct Hop<'a> {
    src: &'a Path,
    reference: &'a Path,
    transform: &'a Path,
    invert_affine: bool,
    work: &'a Path,
}

fn apply_to_grid(
    plan: &ApplyPlan<'_>,
    hop: &Hop<'_>,
    exec: &mut Exec<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    let Hop {
        src,
        reference,
        transform,
        invert_affine,
        work,
    } = *hop;
    let mode = if plan.route == WarpRoute::Ants {
        Apply::Ants
    } else {
        match read_chain(transform, invert_affine) {
            Ok(chain) => {
                apply_nifti(src, reference, &chain, Interpolation::Linear, 0.0, work)
                    .map_err(|e| message(format!("reconstruct: brain mask: {e}")))?;
                Apply::InProcess
            }
            Err(e) if plan.route == WarpRoute::Rust => {
                return Err(message(format!("reconstruct: brain mask: {e}")));
            }
            Err(e) => {
                log(
                    tx,
                    format!(
                        "dbsloc: note: reconstruct: {e}; the brain mask uses antsApplyTransforms"
                    ),
                );
                Apply::Ants
            }
        }
    };
    if matches!(mode, Apply::Ants) {
        let bin = (plan.apply_bin)()?;
        let argv =
            build_ants_mask_apply_argv(&bin, src, reference, work, transform, invert_affine, &[]);
        exec(&argv)?;
    }
    Ok(())
}

pub(crate) struct MaskInputs<'a> {
    pub template_mask: &'a Path,
    pub norm_inverse: &'a Path,
    pub coreg_affine: &'a Path,
    pub anchor: &'a Path,
    pub raw_ct: &'a Path,
    pub anchor_out: &'a Path,
    pub raw_ct_out: &'a Path,
    pub work_dir: &'a Path,
}

pub(crate) fn generate_with(
    inputs: &MaskInputs<'_>,
    plan: &ApplyPlan<'_>,
    exec: &mut Exec<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<(u64, u64)> {
    std::fs::create_dir_all(inputs.work_dir)?;
    let work = derivatives::short_image(inputs.work_dir, WORK_TAG);
    let staged = Staged::mapped(vec![
        (
            inputs.anchor_out.to_path_buf(),
            derivatives::short_image(inputs.work_dir, ANCHOR_TAG),
        ),
        (
            inputs.raw_ct_out.to_path_buf(),
            derivatives::short_image(inputs.work_dir, CT_TAG),
        ),
    ])?;
    let result = (|| -> PipelineResult<(u64, u64)> {
        log(
            tx,
            format!(
                "[reconstruct] brain mask: template → {}",
                file_name_of(inputs.anchor)
            ),
        );
        let hop = Hop {
            src: inputs.template_mask,
            reference: inputs.anchor,
            transform: inputs.norm_inverse,
            invert_affine: false,
            work: &work,
        };
        apply_to_grid(plan, &hop, exec, tx)?;
        let anchor_voxels =
            threshold_into(&work, &staged.tmp(inputs.anchor_out)).map_err(message)?;
        let _ = std::fs::remove_file(&work);
        log(
            tx,
            format!(
                "[reconstruct] brain mask: anchor → {}",
                file_name_of(inputs.raw_ct)
            ),
        );
        let anchor_mask_input = staged.tmp(inputs.anchor_out);
        let hop = Hop {
            src: &anchor_mask_input,
            reference: inputs.raw_ct,
            transform: inputs.coreg_affine,
            invert_affine: true,
            work: &work,
        };
        apply_to_grid(plan, &hop, exec, tx)?;
        let ct_voxels = threshold_into(&work, &staged.tmp(inputs.raw_ct_out)).map_err(message)?;
        Ok((anchor_voxels, ct_voxels))
    })();
    let _ = std::fs::remove_file(&work);
    staged.finish(result, None)
}

pub fn gen_ct_mask(
    sub: &Subject,
    opts: &RunOptions,
    norm_inverse: Option<&Path>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<Option<CtMasks>> {
    let fallback = |reason: &str| -> PipelineResult<Option<CtMasks>> {
        log(tx, format!("dbsloc: note: reconstruct: {reason}"));
        log(tx, NO_MASK_NOTE);
        Ok(None)
    };
    let Some(template) = opts
        .normalization_template
        .as_deref()
        .filter(|t| t.is_file())
    else {
        return fallback("no normalization template, so no template brain mask");
    };
    let Some(norm_inverse) = norm_inverse.filter(|p| p.is_file()) else {
        return fallback("no normalization transform to carry the template brain mask");
    };
    let coreg_affine = derivatives::coreg_ct_affine(sub);
    if !coreg_affine.is_file() {
        return fallback("no CT coregistration transform to carry the template brain mask");
    }
    let (Some(anchor), Some(raw_ct)) = (
        sub.anchor_in_use(),
        sub.postop_ct_raw.clone().filter(|p| p.is_file()),
    ) else {
        return fallback("anchor or raw CT image missing");
    };
    let Some(template_mask) = template_mask(template, opts, cancel, tx)? else {
        log(tx, NO_MASK_NOTE);
        return Ok(None);
    };
    let route = crate::stage_impl::warp_route_for(sub, opts);
    let resolve_bin = || require_binary("antsApplyTransforms", "reconstruct");
    let plan = ApplyPlan {
        route,
        apply_bin: &resolve_bin,
    };
    let anchor_out = derivatives::brain_mask_anchor(sub);
    let raw_ct_out = derivatives::brain_mask_ct(sub);
    let mut exec = |argv: &[String]| {
        let mut cmd = command_from(argv, opts);
        spawn_logged(&mut cmd, "reconstruct", cancel, tx)
    };
    let (anchor_voxels, ct_voxels) = generate_with(
        &MaskInputs {
            template_mask: &template_mask,
            norm_inverse,
            coreg_affine: &coreg_affine,
            anchor: &anchor,
            raw_ct: &raw_ct,
            anchor_out: &anchor_out,
            raw_ct_out: &raw_ct_out,
            work_dir: &sub.recon_dir,
        },
        &plan,
        &mut exec,
        tx,
    )?;
    log(
        tx,
        format!(
            "[reconstruct] brain mask: {anchor_voxels} voxels on the anchor grid, {ct_voxels} on the CT grid"
        ),
    );
    Ok(Some(CtMasks {
        raw_ct: raw_ct_out,
        anchor: anchor_out,
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use crate::nifti_io::load_volume_f64;
    use fastwarp_reg::transform::{write_composite_pair, AffineTransform, CompositePair};
    use fastwarp_reg::{Grid, Volume};

    const ID3: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    const N: usize = 32;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_ctmask_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn grid() -> Grid {
        Grid::new([N, N, N], [1.0; 3], [10.0, 20.0, -30.0], ID3).unwrap()
    }

    fn sphere(center: [f64; 3], radius: f64) -> Volume<f32> {
        let g = grid();
        let data = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let d2 = (i as f64 - center[0]).powi(2)
                    + (j as f64 - center[1]).powi(2)
                    + (k as f64 - center[2]).powi(2);
                f32::from(d2 <= radius * radius)
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    }

    fn centroid(path: &Path) -> ([f64; 3], u64) {
        let (data, _) = load_volume_f64(path).unwrap();
        let (mut sum, mut acc) = (0u64, [0.0f64; 3]);
        for ((i, j, k), v) in data.indexed_iter() {
            assert!(*v == 0.0 || *v == 1.0, "mask value {v}");
            if *v == 1.0 {
                sum += 1;
                acc[0] += i as f64;
                acc[1] += j as f64;
                acc[2] += k as f64;
            }
        }
        let n = sum as f64;
        ([acc[0] / n, acc[1] / n, acc[2] / n], sum)
    }

    pub(crate) struct Files {
        pub dir: PathBuf,
        pub template_mask: PathBuf,
        pub norm_inverse: PathBuf,
        pub coreg_affine: PathBuf,
        pub anchor: PathBuf,
        pub raw_ct: PathBuf,
        pub anchor_out: PathBuf,
        pub raw_ct_out: PathBuf,
    }

    pub(crate) fn files(name: &str, norm_shift: [f64; 3], coreg_shift: [f64; 3]) -> Files {
        let dir = scratch(name);
        let template_mask = dir.join("brainmask.nii.gz");
        sphere([16.0, 16.0, 16.0], 6.0)
            .save_nifti(&template_mask)
            .unwrap();
        let anchor = dir.join("anchor.nii.gz");
        Volume::<f32>::zeros(grid()).save_nifti(&anchor).unwrap();
        let raw_ct = dir.join("sub-1_ses-postop_CT.nii.gz");
        Volume::<f32>::zeros(grid()).save_nifti(&raw_ct).unwrap();
        let norm = AffineTransform::from_translation(norm_shift);
        let pair = CompositePair {
            forward: TransformChain::from_queue(vec![TransformElem::Affine(
                norm.inverse().unwrap(),
            )]),
            inverse: TransformChain::from_queue(vec![TransformElem::Affine(norm)]),
        };
        write_composite_pair(&dir.join("norm_"), &pair).unwrap();
        fastwarp_reg::register::write_linear_output(
            &dir.join("ct_to_anchor_"),
            &AffineTransform::from_translation(coreg_shift),
        )
        .unwrap();
        let coreg_affine = dir.join("ct_to_anchor_0GenericAffine.mat");
        Files {
            template_mask,
            norm_inverse: dir.join("norm_InverseComposite.h5"),
            coreg_affine,
            anchor,
            raw_ct,
            anchor_out: dir.join("sub-1_ses-postop_space-anchorNative_desc-brain_mask.nii.gz"),
            raw_ct_out: dir.join("sub-1_ses-postop_space-CT_desc-brain_mask.nii.gz"),
            dir,
        }
    }

    pub(crate) fn inputs(f: &Files) -> MaskInputs<'_> {
        MaskInputs {
            template_mask: &f.template_mask,
            norm_inverse: &f.norm_inverse,
            coreg_affine: &f.coreg_affine,
            anchor: &f.anchor,
            raw_ct: &f.raw_ct,
            anchor_out: &f.anchor_out,
            raw_ct_out: &f.raw_ct_out,
            work_dir: &f.dir,
        }
    }

    fn no_partials(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with(".partial-"))
    }

    #[test]
    fn masks_follow_the_normalization_and_the_inverse_coregistration() {
        let f = files("in_process", [4.0, 0.0, 0.0], [0.0, 3.0, 0.0]);
        let no_bin = || -> PipelineResult<PathBuf> { panic!("no binary lookup expected") };
        let plan = ApplyPlan {
            route: WarpRoute::Rust,
            apply_bin: &no_bin,
        };
        let mut exec = |_: &[String]| -> PipelineResult<()> { panic!("no external tool expected") };
        let (a, c) = generate_with(&inputs(&f), &plan, &mut exec, None).unwrap();
        let (anchor_centre, anchor_n) = centroid(&f.anchor_out);
        let (ct_centre, ct_n) = centroid(&f.raw_ct_out);
        assert_eq!((a, c), (anchor_n, ct_n));
        for ax in 0..3 {
            let want_anchor = [12.0, 16.0, 16.0][ax];
            let want_ct = [12.0, 19.0, 16.0][ax];
            assert!(
                (anchor_centre[ax] - want_anchor).abs() < 0.6,
                "anchor {anchor_centre:?}"
            );
            assert!((ct_centre[ax] - want_ct).abs() < 0.6, "ct {ct_centre:?}");
        }
        assert!(anchor_n > 700 && ct_n > 700, "{anchor_n} {ct_n}");
        assert!(no_partials(&f.dir));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_failed_application_keeps_the_previous_output_and_leaves_no_partials() {
        let f = files("failed", [0.0; 3], [0.0; 3]);
        std::fs::write(&f.anchor_out, b"previous").unwrap();
        let bin = || -> PipelineResult<PathBuf> { Ok(PathBuf::from("antsApplyTransforms")) };
        let plan = ApplyPlan {
            route: WarpRoute::Ants,
            apply_bin: &bin,
        };
        let mut exec = |_: &[String]| -> PipelineResult<()> { Err(message("boom")) };
        assert!(generate_with(&inputs(&f), &plan, &mut exec, None).is_err());
        assert_eq!(std::fs::read(&f.anchor_out).unwrap(), b"previous");
        assert!(!f.raw_ct_out.exists());
        assert!(no_partials(&f.dir));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    pub(crate) fn recorded_argv(f: &Files) -> Vec<Vec<String>> {
        let bin = || -> PipelineResult<PathBuf> { Ok(PathBuf::from("antsApplyTransforms")) };
        let plan = ApplyPlan {
            route: WarpRoute::Ants,
            apply_bin: &bin,
        };
        let mut calls: Vec<Vec<String>> = Vec::new();
        let source = f.template_mask.clone();
        let mut exec = |argv: &[String]| -> PipelineResult<()> {
            calls.push(argv.to_vec());
            let out = &argv[argv.iter().position(|a| a == "-o").unwrap() + 1];
            std::fs::copy(&source, out).unwrap();
            Ok(())
        };
        generate_with(&inputs(f), &plan, &mut exec, None).unwrap();
        calls
    }

    #[test]
    fn the_ants_route_inverts_the_coregistration_and_stages_under_short_names() {
        let f = files("ants_argv", [0.0; 3], [0.0; 3]);
        let calls = recorded_argv(&f);
        assert_eq!(calls.len(), 2);
        let after = |argv: &[String], flag: &str| {
            argv[argv.iter().position(|a| a == flag).unwrap() + 1].clone()
        };
        assert_eq!(
            calls[0],
            build_ants_mask_apply_argv(
                Path::new("antsApplyTransforms"),
                &f.template_mask,
                &f.anchor,
                &derivatives::short_image(&f.dir, WORK_TAG),
                &f.norm_inverse,
                false,
                &[],
            )
        );
        assert_eq!(after(&calls[0], "-t"), f.norm_inverse.to_string_lossy());
        assert_eq!(
            after(&calls[1], "-t"),
            format!("[{},1]", f.coreg_affine.display())
        );
        assert_eq!(after(&calls[1], "-r"), f.raw_ct.to_string_lossy());
        assert_eq!(
            after(&calls[1], "-i"),
            derivatives::short_image(&f.dir, ANCHOR_TAG).to_string_lossy()
        );
        for argv in &calls {
            for arg in argv {
                for part in arg.split([',', '[', ']']) {
                    if part.contains(crate::outputs::PARTIAL_PREFIX) {
                        let n = file_name_of(Path::new(part));
                        assert!(n.len() <= crate::outputs::MAX_ANTS_FILE_NAME, "{n}");
                    }
                }
            }
        }
        assert!(f.anchor_out.is_file() && f.raw_ct_out.is_file());
        assert!(no_partials(&f.dir));
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    fn template_dir(name: &str, space: Option<&str>, with_mask: bool) -> (PathBuf, PathBuf) {
        let dir = scratch(name);
        let t1 = dir.join("t1.nii.gz");
        Volume::<f32>::zeros(grid()).save_nifti(&t1).unwrap();
        if let Some(space) = space {
            std::fs::write(
                dir.join("template.json"),
                format!("{{\"space\": \"{space}\"}}"),
            )
            .unwrap();
        }
        if with_mask {
            sphere([16.0, 16.0, 16.0], 5.0)
                .save_nifti(&dir.join(BRAIN_MASK_NAME))
                .unwrap();
        }
        (dir, t1)
    }

    fn no_bridge() -> Result<Option<Bridge>, String> {
        panic!("the bridge must not be consulted")
    }

    #[test]
    fn a_mask_beside_the_template_is_used_as_is() {
        let (dir, t1) = template_dir("beside", Some("Custom"), true);
        let got = template_mask_with(&t1, None, &mut no_bridge, None).unwrap();
        assert_eq!(got, Some(dir.join(BRAIN_MASK_NAME)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_mask_beside_the_reference_template_means_none() {
        let (dir, t1) = template_dir("reference", Some(REFERENCE_SPACE), false);
        assert_eq!(
            template_mask_with(&t1, Some(&t1), &mut no_bridge, None).unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_space_without_a_reference_mask_means_none() {
        let (dir, t1) = template_dir("no_reference_mask", Some("Custom"), false);
        let (ref_dir, ref_t1) = template_dir("no_reference_mask_ref", None, false);
        assert_eq!(
            template_mask_with(&t1, Some(&ref_t1), &mut no_bridge, None).unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&ref_dir);
    }

    #[test]
    fn another_space_bridges_the_reference_mask_once_and_caches_it() {
        let (dir, t1) = template_dir("bridged", Some("Custom"), false);
        let (ref_dir, ref_t1) = template_dir("bridged_ref", None, true);
        let bridge_dir = scratch("bridged_bridge");
        let affine = AffineTransform::from_translation([2.0, 0.0, 0.0]);
        let pair = CompositePair {
            forward: TransformChain::from_queue(vec![TransformElem::Affine(affine)]),
            inverse: TransformChain::from_queue(vec![TransformElem::Affine(
                affine.inverse().unwrap(),
            )]),
        };
        let prefix = crate::space_bridge::bridge_prefix(&bridge_dir);
        write_composite_pair(&prefix, &pair).unwrap();
        let bridge = Bridge {
            dir: bridge_dir.clone(),
            target_space: "Custom".into(),
            prefix,
        };
        let calls = std::cell::Cell::new(0);
        let mut lookup = || {
            calls.set(calls.get() + 1);
            Ok(Some(bridge.clone()))
        };
        let first = template_mask_with(&t1, Some(&ref_t1), &mut lookup, None)
            .unwrap()
            .unwrap();
        assert!(
            first.starts_with(bridge_dir.join(MASK_CACHE_DIR)),
            "{first:?}"
        );
        let (centre, n) = centroid(&first);
        assert!(n > 400, "{n}");
        assert!(
            (centre[0] - 14.0).abs() < 0.6 || (centre[0] - 18.0).abs() < 0.6,
            "{centre:?}"
        );
        let second = template_mask_with(&t1, Some(&ref_t1), &mut lookup, None)
            .unwrap()
            .unwrap();
        assert_eq!(first, second);
        assert!(no_partials(&bridge_dir.join(MASK_CACHE_DIR)));

        let mut absent = || Ok(None);
        let (dir2, t1b) = template_dir("bridged_none", Some("Custom"), false);
        assert_eq!(
            template_mask_with(&t1b, Some(&ref_t1), &mut absent, None).unwrap(),
            None
        );
        for d in [dir, ref_dir, bridge_dir, dir2] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn without_a_template_mask_the_stage_logs_the_note_and_keeps_pacers_own() {
        let root = scratch("fallback");
        let anat = root.join("rawdata/sub-1/ses-preop/anat");
        std::fs::create_dir_all(&anat).unwrap();
        Volume::<f32>::zeros(grid())
            .save_nifti(&anat.join("sub-1_ses-preop_T1w.nii.gz"))
            .unwrap();
        let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        let opts = RunOptions::new(&root, "1");
        let (tx, rx) = mpsc::channel();
        let got = gen_ct_mask(&sub, &opts, None, None, Some(&tx)).unwrap();
        drop(tx);
        assert_eq!(got, None);
        let lines: Vec<String> = rx
            .iter()
            .filter_map(|e| match e {
                RunEvent::LogLine { line } => Some(line),
                _ => None,
            })
            .collect();
        assert!(lines.iter().any(|l| l == NO_MASK_NOTE), "{lines:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
