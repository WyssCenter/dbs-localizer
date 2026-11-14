// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Interactive 3D scene: template slice + Reco + atlas (Lead-layout NIfTI dirs, `.obj` dir, or labeled NIfTI).
//! Manifest: see `crate::SceneManifestV1` (`specs/08` companion).
//!
//! `dbs-localizer scene --manifest PATH`  
//! `dbs-localizer scene --manifest PATH --headless-check` — load template/reco/atlas paths, exit 0 (no GPU window).

use std::path::PathBuf;
use std::sync::PoisonError;

use crate::{
    NavigationScheme, Scene3DEframeApp, Scene3DPayload, SceneManifestV1,
    SCENE_VIEWER_DEPTH_BUFFER_BITS,
};
use clap::Parser;
use eframe::egui;

#[derive(Parser, Debug)]
#[command(name = "dbs-localizer scene")]
struct Args {
    #[arg(long, value_name = "PATH")]
    manifest: PathBuf,
    #[arg(long, action = clap::ArgAction::SetTrue)]
    headless_check: bool,
    /// Print diagnostic logs (atlas load, geometry rebuild).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    verbose: bool,
    #[arg(long, value_name = "lead-dbs|classic")]
    navigation: Option<String>,
    /// id of the slice backdrop to show, from the manifest's backdrops list
    #[arg(long, value_name = "ID")]
    backdrop: Option<String>,
    /// id of the atlas set to show, from the manifest's atlas sets
    #[arg(long, value_name = "ID")]
    atlas: Option<String>,
}

fn parse_navigation(text: &str) -> Result<NavigationScheme, String> {
    match text.to_ascii_lowercase().as_str() {
        "lead-dbs" => Ok(NavigationScheme::LeadDbs),
        "classic" => Ok(NavigationScheme::Classic),
        other => Err(format!(
            "unknown navigation scheme `{other}`; use `lead-dbs` or `classic`"
        )),
    }
}

/// Runs the 3D scene window with the arguments that follow `scene` on the command line and
/// returns the process exit code.
pub fn run_from_args(args: impl IntoIterator<Item = String>) -> i32 {
    let args = Args::parse_from(std::iter::once("dbs-localizer scene".to_owned()).chain(args));
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e:?}");
            1
        }
    }
}

fn run(args: Args) -> eframe::Result<()> {
    let navigation = match args.navigation.as_deref().map(parse_navigation) {
        Some(Ok(scheme)) => scheme,
        Some(Err(e)) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
        None => NavigationScheme::default(),
    };
    if args.verbose {
        std::env::set_var("DBSLOC_VIZ_VERBOSE", "1");
    }
    let man = SceneManifestV1::from_json_file(&args.manifest)
        .map_err(|e| eframe::Error::AppCreation(std::format!("scene manifest: {e}").into()))?;
    if let Some(id) = args.atlas.as_deref() {
        let ids = crate::manifest_atlas_ids(&man);
        if !ids.iter().any(|a| a == id) {
            eprintln!(
                "dbs-localizer scene: unknown atlas '{id}'; listed: {}",
                ids.join(", ")
            );
            std::process::exit(2);
        }
    }
    let payload = if args.headless_check {
        Scene3DPayload::from_manifest(&man)
    } else {
        Scene3DPayload::from_manifest_fast(&man)
    }
    .map_err(|e| eframe::Error::AppCreation(std::format!("scene load: {e}").into()))?;
    if args.headless_check {
        let (group_count, structure_count) = {
            let groups = payload
                .atlas_groups
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            (
                groups.len(),
                groups.iter().map(|g| g.meshes.len()).sum::<usize>(),
            )
        };
        println!(
            "scene manifest v{}: subj={} space={} atlas_groups={} atlas_structures={} atlas_lead={} atlas_objs={} atlas_nii={}",
            man.manifest_version,
            man.subj_id,
            man.space,
            group_count,
            structure_count,
            man.atlas_lead_sets.len() + man.atlas_lead_nii_dir.is_some() as usize,
            man.atlas_obj_dir.is_some(),
            man.atlas_nii.is_some(),
        );
        let (active, shown, meshed) = {
            let sh = payload
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let groups = payload
                .atlas_groups
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let meshed = groups
                .get(sh.active_atlas_group)
                .map_or(0, |g| g.meshes.iter().filter(|m| m.mesh.is_some()).count());
            (
                groups
                    .get(sh.active_atlas_group)
                    .map(|g| g.id().to_owned())
                    .unwrap_or_default(),
                sh.show_atlas.iter().filter(|v| **v).count(),
                meshed,
            )
        };
        println!("active_atlas={active} structures_shown={shown} structures_meshed={meshed}");
        if let Some(group) = payload.group() {
            println!(
                "group {}: leads={} patients={} models={} vta_surfaces_to_mesh={}",
                group.guid,
                group.leads.len(),
                group.patients().len(),
                group.leads_per_model().len(),
                payload.group_vta_progress().1,
            );
        }
        return Ok(());
    }

    let backdrop = args.backdrop.clone();
    let atlas = args.atlas.clone();
    let title = format!("Electrode reconstruction scene — Subject {}", man.subj_id);
    let viewport_title = title.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(viewport_title)
            .with_icon(crate::brand::window_icon())
            .with_maximized(true)
            .with_inner_size([1280.0, 800.0]),
        depth_buffer: SCENE_VIEWER_DEPTH_BUFFER_BITS,
        wgpu_options: crate::gpu_setup::native_wgpu_configuration(),
        ..Default::default()
    };
    eframe::run_native(
        "dbsloc_scene3d",
        options,
        Box::new(move |cc| {
            crate::brand::install_fonts(&cc.egui_ctx);
            crate::brand::apply_theme(&cc.egui_ctx);
            let mut app = Scene3DEframeApp::new(title, payload);
            app.set_navigation(navigation);
            if let Some(id) = atlas.as_deref() {
                app.select_atlas(id);
            }
            if let Some(id) = backdrop.as_deref() {
                if !app.select_backdrop(id) {
                    eprintln!(
                        "dbs-localizer scene: unknown backdrop '{id}'; listed: {}",
                        app.backdrop_ids().join(", ")
                    );
                    std::process::exit(2);
                }
            }
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
}
