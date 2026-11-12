// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! ``dbs-localizer`` — single-binary egui GUI for the Wyss DBS Localizer pipeline.
//!
//! Dockable panels (``egui_dock``; the arrangement is saved between sessions):
//!   - **Subjects**: BIDS dataset picker and subject list
//!   - **Pipeline**: step selection with per-step options, prerequisites and progress
//!   - **Output**: streamed, filterable output of ``dbsloc-pipeline``
//!   - **3D Viewer**: embedded ``dbsloc-viz`` scene viewer (post-reconstruct)
//!
//! **Pipeline:** wired to ``dbsloc-pipeline``; **3D Viewer** embeds ``Scene3DEframeApp``.
//!
//! Release builds use the Windows GUI subsystem (no console window on a double click) and
//! attach to the launching terminal when there is one, so screenshot mode and errors still
//! print there; a window that cannot be created is reported in a message box.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod app_log;
mod atlas_download;
mod backdrop_download;
mod connectivity;
mod connectome_cache_ui;
mod connectome_panel;
mod connectome_roots;
mod follow;
mod gpu_status;
mod group_explorers;
mod group_panel;
mod guided;
mod import_wizard;
mod landing;
mod leaddbs_import;
mod panels;
mod refine;
mod report_import;
mod review;
mod stimulation_panel;
mod template_picker;
mod win32;

/// The windows that `dbs-localizer` opens instead of the main application when its first
/// argument names one; the pipeline and `dbsloc viz` start them as child processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerMode {
    Scene,
    Checkreg,
}

fn viewer_mode(first_arg: Option<&str>) -> Option<ViewerMode> {
    match first_arg {
        Some("scene") => Some(ViewerMode::Scene),
        Some("checkreg") => Some(ViewerMode::Checkreg),
        _ => None,
    }
}

fn main() {
    if !cfg!(debug_assertions) {
        win32::attach_parent_console();
    }
    let mut args = std::env::args().skip(1);
    if let Some(mode) = viewer_mode(args.next().as_deref()) {
        let rest: Vec<String> = args.collect();
        let code = match mode {
            ViewerMode::Scene => dbsloc_viz::scene_cli::run_from_args(rest),
            ViewerMode::Checkreg => dbsloc_viz::checkreg_cli::run_from_args(rest),
        };
        std::process::exit(code);
    }
    app_log::init();
    if let Some(note) = dbsloc_pipeline::legacy_env::legacy_env_note(std::env::vars()) {
        eprintln!("{note}");
        app_log::info(&note);
    }
    let screenshot = app::screenshot_mode();
    let options = eframe::NativeOptions {
        persist_window: !screenshot,
        persistence_path: screenshot.then(app::screenshot_persistence_path),
        viewport: egui::ViewportBuilder::default()
            .with_title("Wyss DBS Localizer")
            .with_app_id("dbsloc_app")
            .with_icon(dbsloc_viz::brand::window_icon())
            .with_maximized(true)
            .with_min_inner_size([900.0, 620.0])
            .with_inner_size([1440.0, 900.0]),
        depth_buffer: dbsloc_viz::SCENE_VIEWER_DEPTH_BUFFER_BITS,
        wgpu_options: dbsloc_viz::gpu_setup::native_wgpu_configuration(),
        ..Default::default()
    };
    let result = eframe::run_native(
        "dbsloc_app",
        options,
        Box::new(|cc| Ok(Box::new(app::DbslocApp::new(cc)))),
    );
    if let Err(e) = result {
        let text = gpu_status::startup_failure_text(&e.to_string());
        eprintln!("dbs-localizer: {text}");
        app_log::error(&format!("startup failure: {text}"));
        win32::error_box("Wyss DBS Localizer", &text);
        std::process::exit(1);
    }
    app_log::session_ended();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_modes_are_recognised_from_the_first_argument() {
        assert_eq!(viewer_mode(Some("scene")), Some(ViewerMode::Scene));
        assert_eq!(viewer_mode(Some("checkreg")), Some(ViewerMode::Checkreg));
    }

    #[test]
    fn anything_else_starts_the_application() {
        assert_eq!(viewer_mode(None), None);
        assert_eq!(viewer_mode(Some("--manifest")), None);
        assert_eq!(viewer_mode(Some("Scene")), None);
        assert_eq!(viewer_mode(Some("")), None);
    }
}
