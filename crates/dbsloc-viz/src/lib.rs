// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! 2D/3D visualization: **wgpu** (WebGPU) for cross-platform **GPU** rendering.
//!
//! # Stack (see `specs/08_visualization.md` Addendum A)
//!
//! - **Default:** [`wgpu`](https://github.com/gfx-rs/wgpu) — **Vulkan** (Linux, Windows), **Metal** (macOS),
//!   **Direct3D 12** (Windows). Same code paths, portable, modern.
//! - **CI / no GPU:** set `WGPU_FORCE_FALLBACK_ADAPTER=1` when the OS provides a software adapter.
//!
//! # Rust API
//!
//! - [`gencheckregpair_png`] (`checkreg`): CPU checkreg pair PNG (moving + fixed edges; `specs/03_coregistration.md` §12.2). The review window is `checkreg_cli` (`checkreg-viewer`).
//!

pub mod error;

/// Verbose diagnostics for visualization tools.
///
/// Enabled via env `DBSLOC_VIZ_VERBOSE=1` (or `true/yes/on`), or via the `--verbose` flag in
/// `dbs-localizer scene` (which sets the env var before constructing payloads).
pub fn viz_verbose() -> bool {
    let Ok(v) = std::env::var("DBSLOC_VIZ_VERBOSE") else {
        return false;
    };
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[macro_export]
macro_rules! viz_eprintln {
    ($($arg:tt)*) => {{
        if $crate::viz_verbose() {
            eprintln!($($arg)*);
        }
    }};
}

/// Reco v2 JSON loaders (path for the desktop, `from_str`/`from_bytes` for the web viewer).
#[cfg(feature = "data-mni")]
pub mod reco_json;

#[cfg(feature = "scene-viewer")]
mod atlas_marching;
#[cfg(feature = "scene-viewer")]
mod atlas_sets;
/// Wavefront `.obj` parsing. Compiled for the desktop viewer and for the browser byte loaders.
#[cfg(any(feature = "scene-viewer", feature = "web"))]
pub mod atlas_wavefront;
#[cfg(feature = "checkreg-embed")]
pub mod checkreg_app;
#[cfg(feature = "checkreg-viewer")]
pub mod checkreg_cli;
/// Interactive checkreg review state shared by the binary and the browser viewer
/// (`docs/SERVER_CLIENT_PLAN.md` §6.6). Holds no filesystem or process code.
#[cfg(any(feature = "web", feature = "checkreg-embed"))]
pub mod checkreg_view;
/// Procedural electrode meshes from the `specs/09` catalogue (`scene-viewer`).
#[cfg(feature = "scene-viewer")]
pub mod electrode_mesh;
#[cfg(feature = "scene-viewer")]
pub mod group_scene;
#[cfg(feature = "refine-embed")]
pub mod refine_app;
#[cfg(feature = "refine-embed")]
mod refine_lead;
#[cfg(feature = "refine-embed")]
pub mod refine_volume;
#[cfg(feature = "scene-viewer")]
mod scene3d_gpu;
#[cfg(feature = "scene-viewer")]
pub mod scene_cli;
#[cfg(feature = "scene-viewer")]
mod scene_fibres;
#[cfg(feature = "scene-viewer")]
pub mod scene_layers;
#[cfg(feature = "scene-viewer")]
mod scene_manifest;
#[cfg(feature = "scene-viewer")]
mod scene_maps;
#[cfg(feature = "scene-viewer")]
mod scene_oit;
/// Template NIfTI volume + byte loader. Compiled for the desktop viewer and the browser loaders.
#[cfg(any(feature = "scene-viewer", feature = "web"))]
pub mod scene_volume;
#[cfg(any(feature = "checkreg-embed", feature = "refine-embed"))]
mod slice_ui;
#[cfg(feature = "refine-embed")]
pub mod volume_preview;
/// Volume of tissue activated: binary grid to a scene surface (`specs/07` §7.8).
#[cfg(feature = "scene-viewer")]
pub mod vta_mesh;
/// Precomputed AHEAD atlas meshes for the web viewer (`docs/SERVER_CLIENT_PLAN.md` §6.3, D7).
#[cfg(feature = "web")]
pub mod web_atlas;

/// Wyss Center brand assets (IBM Plex Sans, avatar, window icon) for every interactive window.
#[cfg(feature = "interactive-eframe")]
pub mod brand;
#[cfg(feature = "interactive-eframe")]
pub mod gesture_zoom;
/// Adapter selection for every interactive window: hardware first, software rasteriser fallback,
/// `DBSLOC_FORCE_SOFTWARE_RENDERING=1` to force it.
#[cfg(all(feature = "interactive-eframe", not(target_arch = "wasm32")))]
pub mod gpu_setup;
/// Shared colour tokens for every interactive window (viewer + application).
#[cfg(feature = "interactive-eframe")]
pub mod ui_tokens;
/// Small shared egui widgets (information icon) for every interactive window.
#[cfg(feature = "interactive-eframe")]
pub mod widgets;

/// Checkreg `gencheckregpair` PNG (CPU, spec `03_coregistration` §12.2) + interactive multi-plane slices.
#[cfg(feature = "checkreg")]
pub use dbsloc_checkreg::checkreg_pair::{
    crosshair_uv_for_plane, gencheckregpair_png, load_prepared_checkreg_volumes,
    pick_ijk_from_plane_uv, CheckregSlicePlane, GencheckregpairOptions, PreparedCheckregVols,
    RawWindowStats,
};
#[cfg(feature = "checkreg")]
pub use dbsloc_checkreg::{CheckregError, CheckregResult};

/// Re-export for callers that need [`error::VizError`].
pub use error::{VizError, VizResult};

/// Browser viewer byte loaders + precomputed atlas meshes (`docs/SERVER_CLIENT_PLAN.md` §6).
#[cfg(feature = "web")]
pub use web_atlas::{load_precomputed_ahead_from_bytes, AHEAD_PRECOMPUTED_OBJECTS};

#[cfg(feature = "interactive-eframe")]
pub use gesture_zoom::NavigationScheme;

#[cfg(feature = "refine-embed")]
pub use refine_app::{RefineOutput, RefinePanel, RefineRequest, RefineSide, SideEdit};
#[cfg(feature = "refine-embed")]
pub use volume_preview::VolumePreview;

/// Interactive 3D scene (template slice + Reco + atlas meshes). Window: `scene_cli`.
#[cfg(feature = "scene-viewer")]
pub use scene3d_gpu::{
    manifest_atlas_ids, save_color_image, AtlasSurfaceGroup, BackdropChoice, BackdropRequest,
    HostButton, Projection, Scene3DEframeApp, Scene3DPayload, SceneShared, SceneViewDefaults,
    SceneViewState, SCENE_VIEWER_DEPTH_BUFFER_BITS,
};
#[cfg(feature = "scene-viewer")]
pub use scene_manifest::{
    AtlasIndexV1, AtlasLabelSetV1, AtlasLayoutV1, AtlasLeadSetV1, AtlasPresetV1, AtlasSideV1,
    AtlasSkippedV1, AtlasStructureSideV1, AtlasStructureV1, ClinicalColourV2, GroupBlockV2,
    GroupColourByV2, GroupDefV2, GroupShowV2, GroupSubjectV2, SceneBackdropV1, SceneManifestV1,
    SceneVtaV1, SidePairV2,
};
