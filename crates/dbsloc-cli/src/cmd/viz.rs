// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, Subcommand};
use dbsloc_pipeline::paths::{absolute, gui_binary, GUI_BIN_ENV};

use crate::fail::{CmdResult, Fail};

#[derive(Subcommand)]
pub enum VizCmd {
    /// interactive 3D scene: template slices, the reconstructed leads and optional atlas meshes
    Scene(SceneArgs),
    /// registration quality check (checkreg) of the coregistration and normalization: overlay PNG and optional interactive review (see specs/03 addendum)
    Checkreg {
        #[command(subcommand)]
        cmd: CheckregCmd,
    },
}

#[derive(Subcommand)]
pub enum CheckregCmd {
    /// write one registration check PNG (the moving image with the fixed image's edges overlaid, axial montage)
    Gen(GenArgs),
    /// interactive quality check of the coregistration, normalization and brain shift slides; needs a display
    Review(ManifestArgs),
}

#[derive(Args)]
pub struct ManifestArgs {
    /// manifest JSON (scene: template_nii, reco_json, optional atlas_obj_dir / atlas_nii)
    #[arg(long, required = true)]
    pub manifest: PathBuf,
    /// load and validate only, open no window (for machines without a display)
    #[arg(long)]
    pub headless_check: bool,
}

#[derive(Args)]
pub struct SceneArgs {
    #[command(flatten)]
    pub manifest: ManifestArgs,
    /// mouse bindings of the 3D view: lead-dbs (default) or classic
    #[arg(long, value_parser = ["lead-dbs", "classic"])]
    pub navigation: Option<String>,
}

#[derive(Args)]
pub struct GenArgs {
    /// registered moving NIfTI
    #[arg(long, required = true)]
    pub moving: PathBuf,
    /// fixed (anchor) NIfTI
    #[arg(long, required = true)]
    pub fixed: PathBuf,
    /// output PNG path
    #[arg(long, required = true)]
    pub out: PathBuf,
    /// skip percentile window on moving (brainshift-style)
    #[arg(long)]
    pub no_preproc: bool,
    /// lower percentile (default: 0.2)
    #[arg(long, value_name = "PCT", allow_hyphen_values = true)]
    pub lbound: Option<f64>,
    /// upper percentile (default: 99.7)
    #[arg(long, value_name = "PCT", allow_hyphen_values = true)]
    pub ubound: Option<f64>,
    /// axial tiles in montage (default: 24)
    #[arg(long, value_name = "N")]
    pub max_slices: Option<usize>,
}

fn gui_binary_or_fail() -> Result<PathBuf, Fail> {
    gui_binary().ok_or_else(|| {
        Fail::new(
            127,
            format!(
                "dbsloc: error: 'dbs-localizer' not found; build from repo root: cargo build -p dbsloc-app --release, add target/release to PATH, or set {GUI_BIN_ENV}"
            ),
        )
    })
}

fn spawn(bin: &Path, args: &[String]) -> CmdResult {
    let status = Command::new(bin).args(args).status().map_err(|e| {
        Fail::new(
            1,
            format!("dbsloc: error: cannot start {}: {e}", bin.display()),
        )
    })?;
    Ok(status.code().unwrap_or(1))
}

fn path_arg(p: &Path) -> String {
    absolute(p).to_string_lossy().into_owned()
}

fn manifest_args(mode: &str, a: &ManifestArgs) -> Vec<String> {
    let mut v = vec![
        mode.to_owned(),
        "--manifest".to_owned(),
        path_arg(&a.manifest),
    ];
    if a.headless_check {
        v.push("--headless-check".to_owned());
    }
    v
}

fn scene_args(a: &SceneArgs) -> Vec<String> {
    let mut v = manifest_args("scene", &a.manifest);
    if let Some(n) = &a.navigation {
        v.extend(["--navigation".to_owned(), n.clone()]);
    }
    v
}

pub fn gen_options(a: &GenArgs) -> dbsloc_checkreg::GencheckregpairOptions {
    let mut opt = dbsloc_checkreg::GencheckregpairOptions {
        preproc: !a.no_preproc,
        ..Default::default()
    };
    if let Some(l) = a.lbound {
        opt.lbound_pct = l;
    }
    if let Some(u) = a.ubound {
        opt.ubound_pct = u;
    }
    if let Some(n) = a.max_slices {
        opt.max_montage_slices = n;
    }
    opt
}

pub fn run(cmd: VizCmd) -> CmdResult {
    match cmd {
        VizCmd::Scene(a) => {
            let b = gui_binary_or_fail()?;
            spawn(&b, &scene_args(&a))
        }
        VizCmd::Checkreg {
            cmd: CheckregCmd::Gen(a),
        } => {
            dbsloc_checkreg::gencheckregpair_png(
                &absolute(&a.moving),
                &absolute(&a.fixed),
                &absolute(&a.out),
                gen_options(&a),
            )
            .map_err(|e| Fail::new(1, e.to_string()))?;
            Ok(0)
        }
        VizCmd::Checkreg {
            cmd: CheckregCmd::Review(a),
        } => {
            let b = gui_binary_or_fail()?;
            spawn(&b, &manifest_args("checkreg", &a))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_args_select_the_scene_mode_and_forward_the_navigation_scheme() {
        let a = SceneArgs {
            manifest: ManifestArgs {
                manifest: PathBuf::from("m.json"),
                headless_check: true,
            },
            navigation: Some("classic".to_owned()),
        };
        let v = scene_args(&a);
        assert_eq!(v[0], "scene");
        assert_eq!(v[1], "--manifest");
        assert_eq!(v[3..], ["--headless-check", "--navigation", "classic"]);
        let a = SceneArgs {
            navigation: None,
            ..a
        };
        assert_eq!(scene_args(&a).len(), 4);
    }

    #[test]
    fn review_args_select_the_checkreg_mode() {
        let a = ManifestArgs {
            manifest: PathBuf::from("m.json"),
            headless_check: false,
        };
        let v = manifest_args("checkreg", &a);
        assert_eq!(v[0], "checkreg");
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn gen_options_apply_optional_flags() {
        let a = GenArgs {
            moving: PathBuf::from("m.nii.gz"),
            fixed: PathBuf::from("f.nii.gz"),
            out: PathBuf::from("o.png"),
            no_preproc: true,
            lbound: Some(0.5),
            ubound: Some(99.0),
            max_slices: Some(12),
        };
        let o = gen_options(&a);
        assert!(!o.preproc);
        assert_eq!(o.lbound_pct, 0.5);
        assert_eq!(o.ubound_pct, 99.0);
        assert_eq!(o.max_montage_slices, 12);
        let a = GenArgs {
            no_preproc: false,
            lbound: None,
            ubound: None,
            max_slices: None,
            ..a
        };
        let d = dbsloc_checkreg::GencheckregpairOptions::default();
        let o = gen_options(&a);
        assert!(o.preproc);
        assert_eq!(o.lbound_pct, d.lbound_pct);
        assert_eq!(o.max_montage_slices, d.max_montage_slices);
    }
}
