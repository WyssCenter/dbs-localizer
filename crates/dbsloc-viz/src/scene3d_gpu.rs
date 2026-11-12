// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Interactive 3D scene — egui + wgpu paint callback (template slice + atlas meshes + reco lines).

use std::borrow::Cow;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AOrd};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError, Weak};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use bytemuck::{Pod, Zeroable};
use eframe::egui;
use eframe::egui_wgpu;
use eframe::egui_wgpu::wgpu;
use glam::camera::rh::{proj::directx, view::look_at_mat4};
use glam::{Mat4, Vec3};
use image::{ImageBuffer, Rgba};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

use crate::atlas_marching::{
    atlas_label_color, mesh_atlas_stubs, mesh_binary_mask_nifti_cached_with_params,
    mesh_binary_mask_nifti_with_params, meshes_from_label_nifti, AtlasMeshCacheDirs,
    LeadAtlasMeshParams, LeadRoiSource, NamedAtlasMesh,
};
use crate::atlas_sets::{build_lead_set, default_set_index, AtlasSetInfo};
use crate::atlas_wavefront::{load_obj_triangles, TriMeshScene};
use crate::electrode_mesh::{build_electrode, ElectrodeMeshGroup, INSULATION_RGBA, METAL_RGBA};
use crate::error::{VizError, VizResult};
use crate::gesture_zoom::{
    adjust_dolly_distance, adjust_dolly_distance_factor, adjust_dolly_distance_pinch,
    adjust_dolly_distance_right_drag, key_pan_points, lead_dbs_drag_zoom_factor,
    lead_dbs_wheel_zoom_factor, pan_scale, split_wheel_events, NavigationScheme, WheelFrame,
    DOLLY_DIST_MAX, DOLLY_DIST_MIN, LEAD_DBS_ORBIT_RAD_PER_POINT, LEAD_DBS_PAN_GAIN,
};
use crate::group_scene::{
    blue_white_red, vta_mesh_cached, ColourMode, GroupDisplay, GroupModels, GroupScene, Instance,
    Legend, MeshRange,
};
use crate::reco_json::{load_mni_side_geometry_opt, MniSideGeometry};
use crate::scene_fibres::{build_geometry, FibreGeometry, FibreSeg};
use crate::scene_layers::SceneLayers;
use crate::scene_manifest::GroupShowV2;
use crate::scene_manifest::{AtlasLeadSetV1, SceneBackdropV1, SceneManifestV1};
use crate::scene_maps::{mesh_map, resample_plane, MapMeshes, Plane, MAP_SURFACE_ALPHA};
use crate::scene_oit::{
    accumulation_targets, begin_accumulation_pass, begin_opaque_pass, depth_frame, OitCompositor,
    OPAQUE_DEPTH_FORMAT,
};
use crate::scene_volume::{ijk_to_world_mm, TemplateVolume, VoxelWorld};
use crate::vta_mesh::{VtaSurface, VTA_RGBA};

const SHADER: &str = include_str!("shaders/scene3d.wgsl");

/// Must match [`eframe::NativeOptions::depth_buffer`] for `dbs-localizer scene` so egui's render pass
/// exposes a depth attachment compatible with [`SceneWgpuRenderer`] pipelines.
pub const SCENE_VIEWER_DEPTH_BUFFER_BITS: u8 = 24;

/// Run a worker closure off the UI thread on desktop. On `wasm32` there is no `std::thread`, so the
/// job runs inline on the main thread (time-slicing the atlas / ROI build is a WP5 follow-up); the
/// browser viewer normally uses the precomputed `.obj` meshes and meshes nothing.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_worker<F: FnOnce() + Send + 'static>(f: F) {
    thread::spawn(f);
}
#[cfg(target_arch = "wasm32")]
fn spawn_worker<F: FnOnce() + 'static>(f: F) {
    f();
}

/// `for_each` over the atlas mesh worklist: parallel on desktop (rayon), sequential on `wasm32`
/// (no rayon pool in the browser). Must match the parallel branch exactly.
type RoiWork = (usize, String, LeadRoiSource);

#[cfg(not(target_arch = "wasm32"))]
fn for_each_worklist<T, F>(items: Vec<RoiWork>, init: T, op: F)
where
    T: Send + Clone,
    F: Fn(&mut T, RoiWork) + Send + Sync,
{
    items.into_par_iter().for_each_with(init, op);
}
#[cfg(target_arch = "wasm32")]
fn for_each_worklist<T, F>(items: Vec<RoiWork>, mut init: T, op: F)
where
    F: Fn(&mut T, RoiWork),
{
    for item in items {
        op(&mut init, item);
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlobalsGpu {
    view_proj: [[f32; 4]; 4],
    light_dir: [f32; 4],
    cam_pos: [f32; 4],
    atlas_unlit: f32,
    atlas_flat_normals: f32,
    /// Display window / level applied to the normalized template slice samples.
    slice_window: f32,
    slice_level: f32,
    oit_frame: [f32; 4],
    view_fwd: [f32; 4],
    ribbon: [f32; 4],
    _pad: [f32; 24],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshVert {
    pos: [f32; 3],
    n: [f32; 3],
    c: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct QuadVert {
    pos: [f32; 3],
    uv: [f32; 2],
    _pad: [f32; 2],
}

pub const LEAD_DBS_DEFAULT_AZIMUTH_DEG: f32 = 180.0;
pub const LEAD_DBS_DEFAULT_ELEVATION_DEG: f32 = 21.0;
pub const LEAD_DBS_DEFAULT_VISIBLE_HEIGHT_MM: f32 = 79.3;
pub const LEAD_DBS_DEFAULT_TARGET_MM: [f32; 3] = [-3.155, -25.274, -3.650];

const ELEVATION_MAX_DEG: f32 = 89.0;
const ELEVATION_MAX_RAD: f32 = ELEVATION_MAX_DEG * std::f32::consts::PI / 180.0;
const VIEW_HALF_FOV_RAD: f32 = 22.5 * std::f32::consts::PI / 180.0;
const ORTHO_EYE_MM: f32 = 3000.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Projection {
    #[default]
    Orthographic,
    Perspective,
}

impl Projection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Orthographic => "Orthographic",
            Self::Perspective => "Perspective",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Orthographic => {
                "Parallel projection, as in Lead-DBS: distant and near structures keep their size"
            }
            Self::Perspective => "45° field of view: nearer structures look larger",
        }
    }
}

#[derive(Clone)]
pub struct SceneShared {
    pub azimuth: f32,
    pub elevation: f32,
    pub projection: Projection,
    pub distance: f32,
    pub target: Vec3,
    pub axial_k: usize,
    pub coronal_j: usize,
    pub sagittal_i: usize,
    pub show_axial: bool,
    pub show_coronal: bool,
    pub show_sagittal: bool,
    /// Slice opacity 0..1 (render alpha for template slice planes).
    pub axial_opacity: f32,
    pub coronal_opacity: f32,
    pub sagittal_opacity: f32,
    pub atlas_opacity: f32,
    /// Master switch: hide **all** atlas structures (slices + trajectories unchanged).
    pub atlas_overlay_on: bool,
    /// Index into [`Scene3DPayload::atlas_groups`].
    pub active_atlas_group: usize,
    /// Per-atlas mesh opacity 0..1; length matches `show_atlas`; combined with `atlas_opacity`.
    pub atlas_alpha: Vec<f32>,
    pub show_atlas: Vec<bool>,
    /// Per-structure RGB tint (GUI); **alpha** still from mesh default × sliders.
    pub atlas_rgb: Vec<[f32; 3]>,
    pub atlas_vectors_group: Option<usize>,
    /// Id of the chosen atlas set; `None` until resolved against the loaded sets (default set).
    pub atlas_id: Option<String>,
    /// Display window (contrast width) for the template slices, 0..1 of the
    /// normalized intensity range. Level is its centre.
    pub slice_window: f32,
    pub slice_level: f32,
    /// Draw the reconstructed lead **bodies** as procedural meshes (`specs/09`, `electrode_mesh`).
    pub show_electrodes: bool,
    /// Draw the volumes of tissue activated of the loaded stimulation.
    pub show_vta: bool,
    /// Draw a numbered label at each reconstructed contact.
    pub show_contact_labels: bool,
    pub revision: u64,
    pub slice_world: [f32; 3],
}

impl SceneShared {
    pub fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Snapshot of everything the user can adjust, for persistence between sessions.
    pub fn snapshot(&self) -> SceneViewState {
        SceneViewState {
            camera_convention: 1,
            slice_convention: 1,
            slice_mm: Some(self.slice_world.map(f64::from)),
            backdrop_id: None,
            azimuth: self.azimuth,
            elevation: self.elevation,
            projection: self.projection,
            distance: self.distance,
            target: [self.target.x, self.target.y, self.target.z],
            axial_k: self.axial_k,
            coronal_j: self.coronal_j,
            sagittal_i: self.sagittal_i,
            show_axial: self.show_axial,
            show_coronal: self.show_coronal,
            show_sagittal: self.show_sagittal,
            axial_opacity: self.axial_opacity,
            coronal_opacity: self.coronal_opacity,
            sagittal_opacity: self.sagittal_opacity,
            atlas_opacity: self.atlas_opacity,
            atlas_overlay_on: self.atlas_overlay_on,
            active_atlas_group: self.active_atlas_group,
            atlas_id: self.atlas_id.clone(),
            atlas_alpha: self.atlas_alpha.clone(),
            show_atlas: self.show_atlas.clone(),
            atlas_rgb: self.atlas_rgb.clone(),
            slice_window: self.slice_window,
            slice_level: self.slice_level,
            show_electrodes: self.show_electrodes,
            show_vta: self.show_vta,
            show_contact_labels: self.show_contact_labels,
            fibre_width_px: crate::scene_layers::DEFAULT_FIBRE_WIDTH_PX,
            max_fibres: crate::scene_layers::DEFAULT_MAX_FIBRES,
        }
    }

    pub fn set_slices_to_world(&mut self, backdrop: &TemplateVolume, mm: [f32; 3]) {
        self.slice_world = mm;
        let (i, j, k) = slice_indices_for_world(backdrop, mm);
        self.sagittal_i = i;
        self.coronal_j = j;
        self.axial_k = k;
    }

    pub fn note_slice_edits(
        &mut self,
        backdrop: &TemplateVolume,
        before: (usize, usize, usize),
        world_before: [f32; 3],
    ) {
        let now = [self.sagittal_i, self.coronal_j, self.axial_k];
        let was = [before.0, before.1, before.2];
        for axis in 0..3 {
            if now[axis] != was[axis] && self.slice_world[axis] == world_before[axis] {
                self.slice_world[axis] = plane_world_mm(backdrop, axis, now[axis]) as f32;
            }
        }
    }

    /// Restore camera and slice setup. Atlas vectors are only taken when their
    /// length matches the loaded group, so a changed atlas cannot corrupt state.
    pub fn restore(&mut self, s: &SceneViewState) {
        if s.camera_convention >= 1 {
            self.azimuth = wrap_angle(s.azimuth);
            self.elevation = s.elevation.clamp(-ELEVATION_MAX_RAD, ELEVATION_MAX_RAD);
            self.projection = s.projection;
            self.distance = s.distance.clamp(DOLLY_DIST_MIN, DOLLY_DIST_MAX);
            self.target = Vec3::new(s.target[0], s.target[1], s.target[2]);
        }
        self.axial_opacity = s.axial_opacity.clamp(0.0, 1.0);
        self.coronal_opacity = s.coronal_opacity.clamp(0.0, 1.0);
        self.sagittal_opacity = s.sagittal_opacity.clamp(0.0, 1.0);
        self.atlas_opacity = s.atlas_opacity.clamp(0.0, 1.0);
        self.atlas_overlay_on = s.atlas_overlay_on;
        self.active_atlas_group = s.active_atlas_group;
        self.atlas_id = s.atlas_id.clone();
        if !s.show_atlas.is_empty()
            && s.show_atlas.len() == s.atlas_alpha.len()
            && s.show_atlas.len() == s.atlas_rgb.len()
        {
            self.show_atlas = s.show_atlas.clone();
            self.atlas_alpha = s.atlas_alpha.clone();
            self.atlas_rgb = s.atlas_rgb.clone();
            self.atlas_vectors_group = Some(s.active_atlas_group);
        }
        self.slice_window = s.slice_window.clamp(0.01, 1.0);
        self.slice_level = s.slice_level.clamp(0.0, 1.0);
        self.show_electrodes = s.show_electrodes;
        self.show_vta = s.show_vta;
        self.show_contact_labels = s.show_contact_labels;
        self.bump();
    }

    pub fn restore_slices(&mut self, s: &SceneViewState, backdrop: &TemplateVolume) {
        if s.slice_convention < 1 {
            return;
        }
        self.show_axial = s.show_axial;
        self.show_coronal = s.show_coronal;
        self.show_sagittal = s.show_sagittal;
        if let Some(mm) = s.slice_mm {
            self.set_slices_to_world(backdrop, [mm[0] as f32, mm[1] as f32, mm[2] as f32]);
        }
        self.bump();
    }
}

/// Serializable snapshot of [`SceneShared`] — camera, slices and atlas selection.
///
/// The GUI persists this so a restart restores the view the user left behind
/// (project UX rule: camera position and atlas selections must survive a restart).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SceneViewState {
    pub camera_convention: u8,
    pub slice_convention: u32,
    pub slice_mm: Option<[f64; 3]>,
    pub backdrop_id: Option<String>,
    pub atlas_id: Option<String>,
    pub azimuth: f32,
    pub elevation: f32,
    pub projection: Projection,
    pub distance: f32,
    pub target: [f32; 3],
    pub axial_k: usize,
    pub coronal_j: usize,
    pub sagittal_i: usize,
    pub show_axial: bool,
    pub show_coronal: bool,
    pub show_sagittal: bool,
    pub axial_opacity: f32,
    pub coronal_opacity: f32,
    pub sagittal_opacity: f32,
    pub atlas_opacity: f32,
    pub atlas_overlay_on: bool,
    pub active_atlas_group: usize,
    pub atlas_alpha: Vec<f32>,
    pub show_atlas: Vec<bool>,
    pub atlas_rgb: Vec<[f32; 3]>,
    pub slice_window: f32,
    pub slice_level: f32,
    pub show_electrodes: bool,
    pub show_vta: bool,
    pub show_contact_labels: bool,
    pub fibre_width_px: f32,
    pub max_fibres: usize,
}

impl Default for SceneViewState {
    fn default() -> Self {
        Self {
            camera_convention: 0,
            slice_convention: 0,
            slice_mm: None,
            backdrop_id: None,
            atlas_id: None,
            azimuth: 0.0,
            elevation: 0.0,
            projection: Projection::default(),
            distance: 300.0,
            target: [0.0; 3],
            axial_k: 0,
            coronal_j: 0,
            sagittal_i: 0,
            show_axial: false,
            show_coronal: true,
            show_sagittal: true,
            axial_opacity: 1.0,
            coronal_opacity: 1.0,
            sagittal_opacity: 1.0,
            atlas_opacity: 1.0,
            atlas_overlay_on: true,
            active_atlas_group: 0,
            atlas_alpha: Vec::new(),
            show_atlas: Vec::new(),
            atlas_rgb: Vec::new(),
            slice_window: 1.0,
            slice_level: 0.5,
            show_electrodes: true,
            show_vta: true,
            show_contact_labels: false,
            fibre_width_px: crate::scene_layers::DEFAULT_FIBRE_WIDTH_PX,
            max_fibres: crate::scene_layers::DEFAULT_MAX_FIBRES,
        }
    }
}

/// One atlas product in the GUI (Lead tree, OBJ dir, or single label NIfTI).
#[derive(Clone, Debug)]
pub struct AtlasSurfaceGroup {
    pub label: String,
    pub meshes: Vec<NamedAtlasMesh>,
    default_flags: Vec<bool>,
    /// When set, group meshes come from Lead-layout NIfTIs under this root (enables cache writes).
    pub lead_set_dir: Option<std::path::PathBuf>,
    pub info: Arc<AtlasSetInfo>,
}

impl AtlasSurfaceGroup {
    pub fn default_show_flags(&self) -> Vec<bool> {
        let mut flags = self.default_flags.clone();
        flags.resize(self.meshes.len(), true);
        flags
    }

    pub fn id(&self) -> &str {
        &self.info.id
    }
}

const OBJ_GROUP_ID: &str = "obj-directory";
const LABEL_NIFTI_GROUP_ID: &str = "labeled-nifti";

fn per_mesh_group(
    label: &str,
    id: &str,
    meshes: Vec<NamedAtlasMesh>,
    lead_set_dir: Option<PathBuf>,
) -> AtlasSurfaceGroup {
    let names: Vec<String> = meshes.iter().map(|m| m.name.clone()).collect();
    AtlasSurfaceGroup {
        label: label.to_string(),
        default_flags: vec![true; meshes.len()],
        meshes,
        lead_set_dir,
        info: Arc::new(AtlasSetInfo::per_mesh(id, &names)),
    }
}

fn load_obj_group(dir: &Path, label: &str) -> VizResult<AtlasSurfaceGroup> {
    let mut out_mesh: Vec<NamedAtlasMesh> = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| crate::error::VizError::NiftiLoad(e.to_string()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("obj"))
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    for p in paths {
        let mesh = load_obj_triangles(&p)?;
        let name = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("roi")
            .to_string();
        let h: u32 = name.bytes().map(|b| b as u32).sum();
        let color = atlas_label_color(&name, h);
        out_mesh.push(NamedAtlasMesh {
            name,
            mesh: Some(mesh),
            color,
            source: None,
        });
    }
    Ok(per_mesh_group(label, OBJ_GROUP_ID, out_mesh, None))
}

fn lead_set_group(
    set: &AtlasLeadSetV1,
    global: &Option<Vec<String>>,
) -> VizResult<AtlasSurfaceGroup> {
    let built = build_lead_set(set, global)?;
    Ok(AtlasSurfaceGroup {
        label: set.name.clone(),
        meshes: built.meshes,
        default_flags: built.initial,
        lead_set_dir: Some(set.dir.clone()),
        info: Arc::new(built.info),
    })
}

/// The lead sets of a manifest, with the legacy single `atlas_lead_nii_dir` as one set.
fn manifest_lead_sets(m: &SceneManifestV1) -> Vec<AtlasLeadSetV1> {
    if !m.atlas_lead_sets.is_empty() {
        return m.atlas_lead_sets.clone();
    }
    m.atlas_lead_nii_dir
        .iter()
        .map(|d| AtlasLeadSetV1 {
            name: d
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Lead atlas")
                .to_string(),
            dir: d.clone(),
            id: None,
            layout: None,
            default_on_substrings: None,
            index: None,
        })
        .collect()
}

/// Ids of the atlas groups a manifest yields, in group order, without reading any folder.
pub fn manifest_atlas_ids(m: &SceneManifestV1) -> Vec<String> {
    let mut ids: Vec<String> = manifest_lead_sets(m)
        .iter()
        .map(|s| s.set_id().to_string())
        .collect();
    if m.atlas_obj_dir.as_ref().is_some_and(|d| d.is_dir()) {
        ids.push(OBJ_GROUP_ID.to_string());
    }
    if m.atlas_nii.is_some() {
        ids.push(LABEL_NIFTI_GROUP_ID.to_string());
    }
    ids
}

fn atlas_group_stubs(m: &SceneManifestV1) -> VizResult<Vec<AtlasSurfaceGroup>> {
    let mut groups: Vec<AtlasSurfaceGroup> = Vec::new();
    for set in manifest_lead_sets(m) {
        groups.push(lead_set_group(&set, &m.atlas_default_on_substrings)?);
    }
    if let Some(ref dir) = m.atlas_obj_dir {
        if dir.is_dir() {
            groups.push(load_obj_group(dir, "OBJ directory")?);
        }
    }
    if let Some(ref nii) = m.atlas_nii {
        let max = m.atlas_max_labels.map(|n| n as usize);
        let meshes = meshes_from_label_nifti(nii, max)?;
        groups.push(per_mesh_group(
            "Labeled NIfTI",
            LABEL_NIFTI_GROUP_ID,
            meshes,
            None,
        ));
    }
    Ok(groups)
}

fn build_atlas_groups(
    m: &SceneManifestV1,
    vol: &TemplateVolume,
) -> VizResult<Vec<AtlasSurfaceGroup>> {
    let mut groups = atlas_group_stubs(m)?;
    let lead_params = LeadAtlasMeshParams {
        template: m.atlas_mesh_resample_to_template.then_some(vol),
        resample_to_template: m.atlas_mesh_resample_to_template,
        mesh_smooth_iters: m.atlas_mesh_smooth_iters,
        source_ijk_subtract: m.atlas_lead_resample_ijk_subtract,
        native_vertex_ijk_shift: m.atlas_lead_native_mc_ijk_shift,
    };
    let active = default_atlas_group(&groups);
    for (gi, g) in groups.iter_mut().enumerate() {
        if g.lead_set_dir.is_none() {
            continue;
        }
        let mesh_idx: Vec<bool> = if !m.atlas_lead_lazy_load {
            vec![true; g.meshes.len()]
        } else if gi == active {
            g.default_show_flags()
        } else {
            vec![false; g.meshes.len()]
        };
        mesh_atlas_stubs(&mut g.meshes, &lead_params, &mesh_idx)?;
    }
    Ok(groups)
}

/// The group shown when no set was chosen (`specs/11` §1.3).
fn default_atlas_group(groups: &[AtlasSurfaceGroup]) -> usize {
    default_set_index(groups.iter().map(|g| g.label.as_str()))
}

/// Points `active_atlas_group` at the set named by `atlas_id` (unknown or unset → the default
/// set) and records that set's id. Returns `true` when the active group changed.
fn resolve_active_atlas(sh: &mut SceneShared, groups: &[AtlasSurfaceGroup]) -> bool {
    if groups.is_empty() {
        return false;
    }
    if sh.atlas_id.as_deref().is_some_and(|id| {
        groups
            .get(sh.active_atlas_group)
            .is_some_and(|g| g.id() == id)
    }) {
        return false;
    }
    let want = sh
        .atlas_id
        .as_deref()
        .and_then(|id| groups.iter().position(|g| g.id() == id))
        .unwrap_or_else(|| default_atlas_group(groups));
    let changed = sh.active_atlas_group != want;
    sh.active_atlas_group = want;
    sh.atlas_id = Some(groups[want].id().to_string());
    changed
}

/// Visibility, opacity and colour vectors of `gix` at their defaults.
fn reset_atlas_vectors(sh: &mut SceneShared, groups: &[AtlasSurfaceGroup], gix: usize) {
    let Some(g) = groups.get(gix) else {
        return;
    };
    sh.show_atlas = g.default_show_flags();
    sh.atlas_alpha = vec![1.0; g.meshes.len()];
    sh.atlas_rgb = g
        .meshes
        .iter()
        .map(|m| [m.color[0], m.color[1], m.color[2]])
        .collect();
    sh.atlas_vectors_group = Some(gix);
}

fn mesh_cache_template_path(
    man: &SceneManifestV1,
    params: &LeadAtlasMeshParams<'_>,
) -> Option<PathBuf> {
    params
        .resample_to_template
        .then(|| man.template_nii.clone())
}

fn mesh_lead_roi(
    man: &SceneManifestV1,
    params: &LeadAtlasMeshParams<'_>,
    cache_root: Option<&Path>,
    src: &LeadRoiSource,
    name: &str,
) -> VizResult<NamedAtlasMesh> {
    match cache_root {
        Some(root) if man.atlas_cache_precompute => {
            let cache = AtlasMeshCacheDirs::for_lead_set(root, &man.atlas_cache_dir_name);
            let template_path = mesh_cache_template_path(man, params);
            mesh_binary_mask_nifti_cached_with_params(
                src,
                name,
                params,
                &cache,
                template_path.as_deref(),
                man.atlas_mesh_preblur_iters,
                man.atlas_mesh_morph_close_iters,
                man.atlas_mesh_weld_eps_mm,
                man.atlas_mesh_taubin_iters,
                man.atlas_mesh_taubin_lambda,
                man.atlas_mesh_taubin_mu,
            )
        }
        _ => mesh_binary_mask_nifti_with_params(src, name, params),
    }
}

struct RecoGeometry {
    points_l: Vec<Vec3>,
    points_r: Vec<Vec3>,
    contacts_l: Vec<Vec3>,
    contacts_r: Vec<Vec3>,
    mesh_l: bool,
    mesh_r: bool,
    electrode_meshes: Vec<ElectrodeMeshGroup>,
}

fn side_geometry(
    side: Option<&MniSideGeometry>,
) -> (Vec<Vec3>, Vec<Vec3>, Vec<ElectrodeMeshGroup>) {
    let Some(side) = side else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let points = side_to_world_points(side);
    let contacts = side_to_contact_points(side);
    let meshes = build_electrode_meshes(side, &points);
    (points, contacts, meshes)
}

fn load_reco_geometry(path: &Path) -> VizResult<RecoGeometry> {
    let left = load_mni_side_geometry_opt(path, "left")?;
    let right = load_mni_side_geometry_opt(path, "right")?;
    if left.is_none() && right.is_none() {
        return Err(VizError::Json(format!(
            "{}: the reconstruction has neither a left nor a right lead",
            path.display()
        )));
    }
    let (points_l, contacts_l, meshes_l) = side_geometry(left.as_ref());
    let (points_r, contacts_r, meshes_r) = side_geometry(right.as_ref());
    let (mesh_l, mesh_r) = (!meshes_l.is_empty(), !meshes_r.is_empty());
    let mut electrode_meshes = meshes_l;
    electrode_meshes.extend(meshes_r);
    Ok(RecoGeometry {
        points_l,
        points_r,
        contacts_l,
        contacts_r,
        mesh_l,
        mesh_r,
        electrode_meshes,
    })
}

fn load_scene_leads(m: &SceneManifestV1) -> VizResult<(RecoGeometry, Option<GroupScene>)> {
    if m.group.is_some() {
        let empty = RecoGeometry {
            points_l: Vec::new(),
            points_r: Vec::new(),
            contacts_l: Vec::new(),
            contacts_r: Vec::new(),
            mesh_l: false,
            mesh_r: false,
            electrode_meshes: Vec::new(),
        };
        return Ok((empty, Some(GroupScene::from_manifest(m)?)));
    }
    Ok((load_reco_geometry(Path::new(&m.reco_json))?, None))
}

fn group_view_defaults(
    group: &Option<GroupScene>,
    mut defaults: SceneViewDefaults,
) -> SceneViewDefaults {
    if let Some(g) = group {
        defaults.target = g.centre;
    }
    defaults
}

static NEXT_PAYLOAD_ID: AtomicU64 = AtomicU64::new(1);

pub const DEFAULT_SLICE_MM: [f32; 3] = [-30.0, -30.0, -30.0];

fn slice_indices_for_world(vol: &TemplateVolume, mm: [f32; 3]) -> (usize, usize, usize) {
    let q = VoxelWorld::world_to_voxel(&vol.hdr).apply(mm[0], mm[1], mm[2]);
    let pick = |v: f32, n: usize| -> usize {
        if v.is_finite() && v >= -0.5 && v <= n as f32 - 0.5 {
            (v.round().max(0.0) as usize).min(n.saturating_sub(1))
        } else {
            n / 2
        }
    };
    (pick(q.x, vol.n0), pick(q.y, vol.n1), pick(q.z, vol.n2))
}

fn plane_world_mm(vol: &TemplateVolume, axis: usize, index: usize) -> f64 {
    let mut ijk = [
        vol.n0.saturating_sub(1) as f32 * 0.5,
        vol.n1.saturating_sub(1) as f32 * 0.5,
        vol.n2.saturating_sub(1) as f32 * 0.5,
    ];
    ijk[axis] = index as f32;
    let w = ijk_to_world_mm(&vol.hdr, ijk[0], ijk[1], ijk[2]);
    f64::from([w.x, w.y, w.z][axis])
}

fn default_slice_indices(vol: &TemplateVolume) -> (usize, usize, usize) {
    slice_indices_for_world(vol, DEFAULT_SLICE_MM)
}

fn axes_in_xyz_order(hdr: &nifti::NiftiHeader) -> bool {
    let a = fastwarp_core::nifti_affine::header_affine(hdr);
    (0..3).all(|c| {
        let own = a[c][c].abs();
        (0..3).all(|r| a[r][c].abs() <= own)
    })
}

const AXES_ORDER_ERROR: &str =
    "This image's axes are not in x / y / z order; resample it to the template first.";

fn load_backdrop_volume(path: &Path) -> BackdropLoadResult {
    let vol = TemplateVolume::load(path)
        .map_err(|e| format!("Could not load the backdrop: {}: {e}", path.display()))?;
    if axes_in_xyz_order(&vol.hdr) {
        Ok(vol)
    } else {
        Err(AXES_ORDER_ERROR.to_string())
    }
}

fn format_signed_mm(v: f64) -> String {
    let t = format!("{v:.1}");
    if t == "-0.0" {
        "0.0".to_string()
    } else {
        t.replace('-', "\u{2212}")
    }
}

fn world_bounds_overlap(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> bool {
    a.0.cmple(b.1).all() && b.0.cmple(a.1).all()
}

fn template_world_bounds(vol: &TemplateVolume) -> (Vec3, Vec3) {
    let hdr = &vol.hdr;
    let n0f = vol.n0.saturating_sub(1) as f32;
    let n1f = vol.n1.saturating_sub(1) as f32;
    let n2f = vol.n2.saturating_sub(1) as f32;
    let mut mn = Vec3::splat(f32::INFINITY);
    let mut mx = Vec3::splat(f32::NEG_INFINITY);
    for i in [0.0_f32, n0f] {
        for j in [0.0_f32, n1f] {
            for k in [0.0_f32, n2f] {
                let w = ijk_to_world_mm(hdr, i, j, k);
                mn = mn.min(w);
                mx = mx.max(w);
            }
        }
    }
    (mn, mx)
}

/// World-space center of the template voxel box.
#[cfg(test)]
fn template_world_center(vol: &TemplateVolume) -> Vec3 {
    let (mn, mx) = template_world_bounds(vol);
    (mn + mx) * 0.5
}

fn default_view_target(vol: &TemplateVolume) -> Vec3 {
    let (mn, mx) = template_world_bounds(vol);
    let t = Vec3::from_array(LEAD_DBS_DEFAULT_TARGET_MM);
    if t.cmpge(mn).all() && t.cmple(mx).all() {
        t
    } else {
        (mn + mx) * 0.5
    }
}

fn default_view_distance() -> f32 {
    LEAD_DBS_DEFAULT_VISIBLE_HEIGHT_MM / (2.0 * VIEW_HALF_FOV_RAD.tan())
}

fn default_view_defaults(
    vol: &TemplateVolume,
    axial_k: usize,
    coronal_j: usize,
    sagittal_i: usize,
) -> SceneViewDefaults {
    SceneViewDefaults {
        azimuth: LEAD_DBS_DEFAULT_AZIMUTH_DEG.to_radians(),
        elevation: LEAD_DBS_DEFAULT_ELEVATION_DEG.to_radians(),
        distance: default_view_distance(),
        target: default_view_target(vol),
        axial_k,
        coronal_j,
        sagittal_i,
    }
}

fn orbit_eye(shared: &SceneShared) -> Vec3 {
    let (sa, ca) = shared.azimuth.sin_cos();
    let (se, ce) = shared.elevation.sin_cos();
    shared.target + shared.distance * Vec3::new(sa * ce, -ca * ce, se)
}

fn view_forward(shared: &SceneShared) -> Vec3 {
    (shared.target - orbit_eye(shared)).normalize_or(Vec3::NEG_Y)
}

fn camera_eye(shared: &SceneShared) -> Vec3 {
    match shared.projection {
        Projection::Perspective => orbit_eye(shared),
        Projection::Orthographic => {
            let dir = (orbit_eye(shared) - shared.target).normalize();
            shared.target + dir * ORTHO_EYE_MM
        }
    }
}

/// Scene uses **RAS**: superior is **+Z**, which is the camera up vector. Azimuth 0 looks from the
/// front (camera on −Y); elevation is the angle above the horizontal plane. Returns the
/// view-projection matrix and the camera position of the active projection.
fn orbit_vp(shared: &SceneShared, aspect: f32) -> (Mat4, Vec3) {
    let eye = camera_eye(shared);
    let view = look_at_mat4(eye, shared.target, Vec3::Z);
    let proj = match shared.projection {
        Projection::Perspective => directx::perspective(45_f32.to_radians(), aspect, 0.5, 5000.0),
        Projection::Orthographic => {
            let half_h = shared.distance * VIEW_HALF_FOV_RAD.tan();
            let half_w = half_h * aspect;
            directx::orthographic(-half_w, half_w, -half_h, half_h, 0.5, 2.0 * ORTHO_EYE_MM)
        }
    };
    (proj * view, eye)
}

/// Camera-plane basis `(right, up)` for the current orbit pose.
fn orbit_screen_basis(shared: &SceneShared) -> (Vec3, Vec3) {
    let cam_fwd = (shared.target - orbit_eye(shared)).normalize();
    let mut right = cam_fwd.cross(Vec3::Z);
    if right.length_squared() < 1e-8 {
        right = Vec3::X;
    } else {
        right = right.normalize();
    }
    let up = right.cross(cam_fwd).normalize();
    (right, up)
}

/// Move the orbit target by a screen-space delta in points (+x right, +y down) so the scene
/// follows the pointer, a two-finger scroll or a keyboard step. Shared by every pan source.
fn pan_target(shared: &mut SceneShared, delta_points: egui::Vec2) {
    if delta_points == egui::Vec2::ZERO {
        return;
    }
    let (right, up) = orbit_screen_basis(shared);
    shared.target += (-right * delta_points.x + up * delta_points.y) * pan_scale(shared.distance);
    shared.bump();
}

const SCENE_KEY_HELP: &[(&str, &str)] = &[
    ("R", "Reset the view"),
    ("1 / 2 / 3", "Toggle the axial / coronal / sagittal slice"),
    ("Up / Down", "Move to the previous / next atlas structure"),
    ("Space", "Show or hide the focused atlas structure"),
    ("Shift+Arrows", "Pan with the keyboard"),
    ("Ctrl+S", "Save the rendered scene to an image"),
    ("F1", "Show or hide this help"),
];

fn navigation_help_grid(ui: &mut egui::Ui, rows: &[(&str, &str)]) {
    for (what, how) in rows {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(*what).strong());
            ui.label(*how);
        });
    }
}

fn breakable_text(ui: &egui::Ui, text: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::simple(
        text.to_owned(),
        egui::TextStyle::Body.resolve(ui.style()),
        egui::Color32::PLACEHOLDER,
        f32::INFINITY,
    );
    job.wrap.break_anywhere = true;
    job
}

fn fitted_slider(ui: &mut egui::Ui, slider: egui::Slider, widest_value: &str) -> egui::Response {
    let em = ui.text_style_height(&egui::TextStyle::Body);
    let font = ui.style().drag_value_text_style.resolve(ui.style());
    let text_width = ui
        .painter()
        .layout_no_wrap(widest_value.to_owned(), font, egui::Color32::PLACEHOLDER)
        .size()
        .x;
    let value_box = (text_width + 2.0 * ui.spacing().button_padding.x)
        .max(ui.spacing().interact_size.x)
        + ui.spacing().item_spacing.x;
    let available = ui.available_width();
    let with_value = available - value_box >= em * 3.0;
    let track = if with_value {
        available - value_box
    } else {
        (available - ui.spacing().item_spacing.x).max(em * 2.0)
    };
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = track;
        ui.add(slider.show_value(with_value))
    })
    .inner
}

const MIN_INLINE_SLIDER_EMS: f32 = 6.0;

fn inline_slider_fits(available: f32, column: f32, spacing: f32, em: f32) -> bool {
    available - column - spacing >= em * MIN_INLINE_SLIDER_EMS
}

fn label_column(ui: &egui::Ui, labels: &[(&str, bool)]) -> f32 {
    let em = ui.text_style_height(&egui::TextStyle::Body);
    let font = egui::TextStyle::Body.resolve(ui.style());
    labels
        .iter()
        .map(|(text, info)| {
            let width = ui
                .painter()
                .layout_no_wrap((*text).to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x;
            if *info {
                width + ui.spacing().item_spacing.x + em
            } else {
                width
            }
        })
        .fold(0.0_f32, f32::max)
        .ceil()
}

fn indented(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let em = ui.text_style_height(&egui::TextStyle::Body);
    ui.horizontal_top(|ui| {
        ui.add_space(em * 0.8);
        ui.vertical(add);
    });
}

#[derive(Clone, Copy)]
struct RowLabel<'a> {
    text: &'a str,
    column: f32,
    info: Option<&'a str>,
    accessible: Option<(&'a str, f64)>,
}

impl<'a> RowLabel<'a> {
    fn new(text: &'a str, column: f32) -> Self {
        Self {
            text,
            column,
            info: None,
            accessible: None,
        }
    }

    fn info(mut self, info: &'a str) -> Self {
        self.info = Some(info);
        self
    }

    fn named(mut self, name: &'a str, value: f64) -> Self {
        self.accessible = Some((name, value));
        self
    }
}

fn labelled_slider(
    ui: &mut egui::Ui,
    row: RowLabel,
    slider: egui::Slider,
    widest_value: &str,
    hint: &str,
) -> egui::Response {
    let em = ui.text_style_height(&egui::TextStyle::Body);
    let inline = inline_slider_fits(
        ui.available_width(),
        row.column,
        ui.spacing().item_spacing.x,
        em,
    );
    let add_label = |ui: &mut egui::Ui| {
        let id = ui.add(egui::Label::new(row.text).wrap()).id;
        if let Some(info) = row.info {
            crate::widgets::info_icon(ui, info);
        }
        id
    };
    let (label_id, response) = if inline {
        ui.horizontal(|ui| {
            let height = ui.spacing().interact_size.y;
            let id = ui
                .allocate_ui_with_layout(
                    egui::vec2(row.column, height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    add_label,
                )
                .inner;
            (id, fitted_slider(ui, slider, widest_value))
        })
        .inner
    } else {
        let id = ui.horizontal(add_label).inner;
        (id, fitted_slider(ui, slider, widest_value))
    };
    let response = response.on_hover_text(hint);
    match row.accessible {
        Some((name, value)) => {
            response.widget_info(|| egui::WidgetInfo::slider(true, value, name));
            response
        }
        None => response.labelled_by(label_id),
    }
}

/// Trajectory samples for one side, in world mm.
///
/// Contacts are deliberately **not** appended here: they are a separate list
/// with their own meaning. Concatenating them numbered trajectory samples as if
/// they were contacts, and put a spurious polyline segment from the distal
/// trajectory sample back to the first contact.
fn fallback_line_vertices(points: &[Vec3], enabled: bool) -> Vec<Vec3> {
    if !enabled {
        return Vec::new();
    }
    points.windows(2).flat_map(|w| [w[0], w[1]]).collect()
}

fn side_to_world_points(side: &MniSideGeometry) -> Vec<Vec3> {
    side.trajectory
        .iter()
        .map(|t| Vec3::new(t[0] as f32, t[1] as f32, t[2] as f32))
        .collect()
}

/// Reconstructed contact centres for one side, in world mm.
fn side_to_contact_points(side: &MniSideGeometry) -> Vec<Vec3> {
    side.contacts
        .as_ref()
        .map(|cc| {
            cc.iter()
                .map(|t| Vec3::new(t[0] as f32, t[1] as f32, t[2] as f32))
                .collect()
        })
        .unwrap_or_default()
}

fn to_world(t: [f64; 3]) -> Vec3 {
    Vec3::new(t[0] as f32, t[1] as f32, t[2] as f32)
}

/// Build the procedural lead body for one reconstructed side (`specs/09`).
///
/// Returns an empty vector when the reconstruction does not name a catalogued electrode, or
/// when it carries no rigid-frame markers to place the mesh — the caller then falls back to the
/// trajectory line alone. `points` is that side's trajectory, used to extend the shaft to the
/// most-proximal visible sample.
fn build_electrode_meshes(side: &MniSideGeometry, points: &[Vec3]) -> Vec<ElectrodeMeshGroup> {
    let Some(cat) = side
        .elmodel
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .and_then(dbsloc_reconstruct::electrode_by_name)
    else {
        return Vec::new();
    };
    let Some(markers) = &side.markers else {
        return Vec::new();
    };
    let head = to_world(markers.head);
    let tail = to_world(markers.tail);
    let x_ref = markers.x.map(to_world);
    let y_ref = markers.y.map(to_world);
    let proximal_mm = (tail - head)
        .try_normalize()
        .map(|ez| {
            points
                .iter()
                .map(|p| (*p - head).dot(ez))
                .fold(0.0_f32, f32::max)
        })
        .unwrap_or(0.0);
    build_electrode(cat, head, tail, x_ref, y_ref, proximal_mm)
        .into_iter()
        .collect()
}

/// Initial camera + slice indices (`Reset view` restores this snapshot).
#[derive(Clone, Copy, Debug)]
pub struct SceneViewDefaults {
    pub azimuth: f32,
    pub elevation: f32,
    pub distance: f32,
    pub target: Vec3,
    pub axial_k: usize,
    pub coronal_j: usize,
    pub sagittal_i: usize,
}

/// Stored so lazy Lead ROI loads can rebuild [`LeadAtlasMeshParams`] with template reference.
#[derive(Clone, Debug)]
pub struct LeadAtlasLoadConfig {
    pub resample_to_template: bool,
    pub mesh_smooth_iters: u32,
    pub source_ijk_subtract: f32,
    pub native_vertex_ijk_shift: f32,
}

const LAYER_DEBOUNCE_SECONDS: f64 = 0.12;
const MAP_POSITIVE_RGBA: [f32; 4] = [0.9176, 0.2, 0.1373, MAP_SURFACE_ALPHA];
const MAP_NEGATIVE_RGBA: [f32; 4] = [0.2824, 0.6157, 0.9725, MAP_SURFACE_ALPHA];

#[derive(Default)]
struct LayerBuild {
    fibre_running: bool,
    map_running: bool,
    fibre_seen: u64,
    fibre_seen_at: f64,
    map_seen: u64,
    map_seen_at: f64,
    fibre_built: u64,
    fibres: Option<Arc<FibreGeometry>>,
    map_built: u64,
    maps: Option<Arc<Vec<Arc<MapMeshes>>>>,
    mesh_cache: Vec<MeshCacheEntry>,
}

struct MeshCacheEntry {
    layer: Arc<crate::scene_layers::MapLayer>,
    threshold: u32,
    peaks: [u32; 2],
    meshes: Arc<MapMeshes>,
}

pub struct Scene3DPayload {
    pub vol: Arc<TemplateVolume>,
    backdrop: Mutex<(u64, Arc<TemplateVolume>)>,
    space: String,
    manifest_backdrops: Vec<SceneBackdropV1>,
    atlas_ids: Vec<String>,
    pub atlas_groups: Mutex<Vec<AtlasSurfaceGroup>>,
    /// Trajectory samples (the drawn polyline).
    pub points_l: Vec<Vec3>,
    pub points_r: Vec<Vec3>,
    pub mesh_l: bool,
    pub mesh_r: bool,
    /// Reconstructed contact centres — labelled and marked, never drawn as line.
    pub contacts_l: Vec<Vec3>,
    pub contacts_r: Vec<Vec3>,
    /// Procedural lead bodies (`electrode_mesh`), one per side; empty when the model or the
    /// rigid-frame markers are missing from the reconstruction.
    pub electrode_meshes: Vec<ElectrodeMeshGroup>,
    pub shared: Arc<Mutex<SceneShared>>,
    pub view_defaults: SceneViewDefaults,
    pub lead_atlas_load: Option<LeadAtlasLoadConfig>,
    id: u64,
    atlas_generation: AtomicU64,
    vta: Mutex<Vec<VtaSurface>>,
    vta_generation: AtomicU64,
    layers: Mutex<SceneLayers>,
    fibre_generation: AtomicU64,
    map_generation: AtomicU64,
    overlay_pending: Arc<AtomicUsize>,
    layer_build: Mutex<LayerBuild>,
    group: Option<Arc<GroupScene>>,
    group_models: Option<Arc<GroupModels>>,
    group_display: Mutex<GroupDisplay>,
    group_generation: AtomicU64,
    group_vta: Mutex<Vec<(usize, TriMeshScene)>>,
    group_vta_done: AtomicUsize,
    group_vta_total: AtomicUsize,
    group_vta_started: AtomicBool,
    atlas_manifest: Option<SceneManifestV1>,
    atlas_loading: Arc<AtomicBool>,
    atlas_load_started: AtomicBool,
    atlas_error: Arc<Mutex<Option<String>>>,
    /// Mirrors `atlas_error.is_some()` so the paint path never takes that lock.
    atlas_error_set: Arc<AtomicBool>,
    atlas_loaded: Arc<AtomicUsize>,
    atlas_total: Arc<AtomicUsize>,
    /// On-demand ROI meshing queue. Enabling a structure never meshes on the UI
    /// thread — the request is handed to this worker instead.
    roi_tx: Mutex<Option<mpsc::Sender<(usize, usize)>>>,
    roi_worker_started: AtomicBool,
    roi_queued: Mutex<std::collections::HashSet<(usize, usize)>>,
    roi_jobs_total: Arc<AtomicUsize>,
    roi_jobs_done: Arc<AtomicUsize>,
    roi_current: Arc<Mutex<Option<String>>>,
}

impl Scene3DPayload {
    pub fn backdrop_volume(&self) -> Arc<TemplateVolume> {
        Arc::clone(&self.backdrop_snapshot().1)
    }

    fn backdrop_snapshot(&self) -> (u64, Arc<TemplateVolume>) {
        let g = self.backdrop.lock().unwrap_or_else(PoisonError::into_inner);
        (g.0, Arc::clone(&g.1))
    }

    fn set_backdrop(&self, vol: Arc<TemplateVolume>) {
        {
            let mut g = self.backdrop.lock().unwrap_or_else(PoisonError::into_inner);
            g.0 += 1;
            g.1 = Arc::clone(&vol);
        }
        let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        let mm = sh.slice_world;
        sh.set_slices_to_world(&vol, mm);
        sh.slice_window = 1.0;
        sh.slice_level = 0.5;
        sh.bump();
    }

    /// Replaces the volumes of tissue activated drawn in the scene.
    pub fn set_vta_surfaces(&self, surfaces: Vec<VtaSurface>) {
        *self.vta.lock().unwrap_or_else(PoisonError::into_inner) = surfaces;
        self.vta_generation.fetch_add(1, AOrd::Release);
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bump();
    }

    fn bump_shared(&self) {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bump();
    }

    fn with_layers<R>(&self, f: impl FnOnce(&mut SceneLayers) -> R) -> R {
        let mut layers = self.layers.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut layers)
    }

    fn with_layer_build<R>(&self, f: impl FnOnce(&mut LayerBuild) -> R) -> R {
        let mut build = self
            .layer_build
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        f(&mut build)
    }

    fn touch_fibres(&self) {
        self.fibre_generation.fetch_add(1, AOrd::Release);
        self.bump_shared();
    }

    fn touch_maps(&self) {
        self.map_generation.fetch_add(1, AOrd::Release);
        self.bump_shared();
    }

    pub fn upsert_fibre_layer(&self, layer: crate::scene_layers::FibreLayer) {
        self.with_layers(|l| l.upsert_fibre(layer));
        self.touch_fibres();
    }

    pub fn upsert_fibre_layer_shared(&self, layer: Arc<crate::scene_layers::FibreLayer>) {
        self.with_layers(|l| l.upsert_fibre_shared(layer));
        self.touch_fibres();
    }

    pub fn upsert_map_layer_shared(&self, layer: Arc<crate::scene_layers::MapLayer>) {
        self.with_layers(|l| l.upsert_map_shared(layer));
        self.touch_maps();
    }

    pub fn layer_settings(&self) -> crate::scene_layers::LayerSettings {
        self.with_layers(|l| l.settings())
    }

    pub fn restore_layer_settings(&self, saved: &crate::scene_layers::LayerSettings) {
        self.with_layers(|l| l.restore_settings(saved));
        self.fibre_generation.fetch_add(1, AOrd::Release);
        self.touch_maps();
    }

    pub fn remove_fibre_layer(&self, id: &str) {
        if self.with_layers(|l| l.remove_fibre(id)) {
            self.touch_fibres();
        }
    }

    pub fn upsert_map_layer(&self, layer: crate::scene_layers::MapLayer) {
        self.with_layers(|l| l.upsert_map(layer));
        self.touch_maps();
    }

    pub fn remove_map_layer(&self, id: &str) {
        if self.with_layers(|l| l.remove_map(id)) {
            self.touch_maps();
        }
    }

    pub fn set_fibre_file_supported(&self, supported: bool) {
        self.with_layers(|l| l.fibre_file_supported = supported);
    }

    pub fn take_removed_layers(&self) -> (Vec<String>, Vec<String>) {
        self.with_layers(|l| l.take_removed())
    }

    pub fn take_fibre_file_request(&self) -> bool {
        self.with_layers(|l| std::mem::take(&mut l.fibre_file_requested))
    }

    fn fibre_geometry(&self) -> (u64, Option<Arc<FibreGeometry>>) {
        self.with_layer_build(|b| (b.fibre_built, b.fibres.clone()))
    }

    fn map_meshes(&self) -> (u64, Option<Arc<Vec<Arc<MapMeshes>>>>) {
        self.with_layer_build(|b| (b.map_built, b.maps.clone()))
    }

    fn map_built_generation(&self) -> u64 {
        self.with_layer_build(|b| b.map_built)
    }

    pub fn start_layer_builds_if_needed(self: &Arc<Self>, now: f64) -> bool {
        let fibre_want = self.fibre_generation.load(AOrd::Acquire);
        let map_want = self.map_generation.load(AOrd::Acquire);
        let (start_fibres, start_maps, busy) = self.with_layer_build(|b| {
            let ready = |seen: &mut u64, seen_at: &mut f64, want: u64| {
                if *seen != want {
                    *seen = want;
                    *seen_at = now;
                }
                now - *seen_at >= LAYER_DEBOUNCE_SECONDS
            };
            let mut start_fibres = false;
            if fibre_want != b.fibre_built && !b.fibre_running {
                start_fibres = ready(&mut b.fibre_seen, &mut b.fibre_seen_at, fibre_want);
                b.fibre_running = start_fibres;
            }
            let mut start_maps = false;
            if map_want != b.map_built && !b.map_running {
                start_maps = ready(&mut b.map_seen, &mut b.map_seen_at, map_want);
                b.map_running = start_maps;
            }
            let busy = self.overlay_pending.load(AOrd::Acquire) > 0
                || fibre_want != b.fibre_built
                || map_want != b.map_built
                || b.fibre_running
                || b.map_running;
            (start_fibres, start_maps, busy)
        });
        if start_fibres {
            let me = Arc::clone(self);
            spawn_worker(move || me.run_fibre_build(fibre_want));
        }
        if start_maps {
            let me = Arc::clone(self);
            spawn_worker(move || me.run_map_build(map_want));
        }
        busy
    }

    fn run_fibre_build(&self, generation: u64) {
        let (layers, max) = self.with_layers(|l| {
            let layers: Vec<_> = l
                .fibres
                .iter()
                .filter(|f| l.fibre_setting(&f.id).visible)
                .map(|f| (Arc::clone(f), l.effective_colouring(f)))
                .collect();
            (layers, l.max_fibres)
        });
        let geometry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            build_geometry(&layers, max)
        }))
        .unwrap_or_default();
        self.with_layer_build(|b| {
            b.fibres = Some(Arc::new(geometry));
            b.fibre_built = generation;
            b.fibre_running = false;
        });
        self.bump_shared();
    }

    fn run_map_build(&self, generation: u64) {
        let work: Vec<_> = self.with_layers(|l| {
            l.maps
                .iter()
                .filter_map(|m| {
                    let s = l.map_settings.get(&m.id)?;
                    (s.visible && s.mode == crate::scene_layers::MapMode::Isosurface)
                        .then(|| (Arc::clone(m), *s))
                })
                .collect()
        });
        let mut cache = self.with_layer_build(|b| std::mem::take(&mut b.mesh_cache));
        let mut next: Vec<MeshCacheEntry> = Vec::new();
        let build_all = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for (layer, settings) in &work {
                let threshold = settings.threshold.to_bits();
                let peaks = [settings.max_pos.to_bits(), settings.max_neg.to_bits()];
                let hit = cache.iter().position(|e| {
                    Arc::ptr_eq(&e.layer, layer) && e.threshold == threshold && e.peaks == peaks
                });
                let meshes = match hit {
                    Some(i) => Arc::clone(&cache.swap_remove(i).meshes),
                    None => match mesh_map(layer, settings) {
                        Ok(m) => Arc::new(m),
                        Err(e) => {
                            eprintln!("dbsloc: warning: scene: map {}: {e}", layer.label);
                            continue;
                        }
                    },
                };
                next.push(MeshCacheEntry {
                    layer: Arc::clone(layer),
                    threshold,
                    peaks,
                    meshes,
                });
            }
        }));
        if build_all.is_err() {
            next.clear();
        }
        let meshes: Vec<Arc<MapMeshes>> = next.iter().map(|e| Arc::clone(&e.meshes)).collect();
        self.with_layer_build(|b| {
            b.maps = Some(Arc::new(meshes));
            b.mesh_cache = next;
            b.map_built = generation;
            b.map_running = false;
        });
        self.bump_shared();
    }

    /// The group scene model of a version-2 manifest.
    pub fn group(&self) -> Option<&Arc<GroupScene>> {
        self.group.as_ref()
    }

    /// The display state of a group scene.
    pub fn group_display(&self) -> Option<GroupDisplay> {
        self.group.as_ref()?;
        Some(
            self.group_display
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        )
    }

    fn store_group_display(&self, display: GroupDisplay) -> bool {
        let mut cur = self
            .group_display
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *cur == display {
            return false;
        }
        *cur = display;
        self.group_generation.fetch_add(1, AOrd::Release);
        true
    }

    /// Sets the display state of a group scene and redraws.
    pub fn set_group_display(&self, display: GroupDisplay) {
        if self.group.is_some() && self.store_group_display(display) {
            self.shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .bump();
        }
    }

    /// (surfaces meshed, surfaces to mesh) of the group's volumes of tissue activated.
    pub fn group_vta_progress(&self) -> (usize, usize) {
        (
            self.group_vta_done.load(AOrd::Relaxed),
            self.group_vta_total.load(AOrd::Relaxed),
        )
    }

    /// Meshes the VTA images of a group scene on a worker thread (once), caching every surface by the hash of its image.
    pub fn start_group_vta_load(self: &Arc<Self>) {
        let Some(group) = self.group.clone() else {
            return;
        };
        if self.group_vta_started.swap(true, AOrd::AcqRel) {
            return;
        }
        let me = Arc::clone(self);
        spawn_worker(move || {
            for (lead, path) in group.vta_jobs() {
                match vta_mesh_cached(&path, &group.cache_dir) {
                    Ok(mesh) => {
                        me.group_vta
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push((lead, mesh));
                    }
                    Err(e) => eprintln!("dbsloc: warning: scene: {}: {e}", path.display()),
                }
                let done = me.group_vta_done.fetch_add(1, AOrd::AcqRel) + 1;
                if done.is_multiple_of(8) {
                    me.group_generation.fetch_add(1, AOrd::Release);
                    me.shared
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .bump();
                }
            }
            me.group_generation.fetch_add(1, AOrd::Release);
            me.shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .bump();
        });
    }

    /// Side and label of every volume of tissue activated now loaded.
    pub fn vta_labels(&self) -> Vec<(String, String)> {
        self.vta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|s| (s.side.clone(), s.label.clone()))
            .collect()
    }

    fn bump_atlas_generation(&self) {
        self.atlas_generation.fetch_add(1, AOrd::Release);
    }

    fn atlas_generation(&self) -> u64 {
        self.atlas_generation.load(AOrd::Acquire)
    }

    /// Load one Lead-layout ROI NIfTI into GPU on first enable if it has a source.
    /// Returns **`true`** if new triangle data was loaded (caller should bump scene revision).
    pub fn try_load_lead_roi(&self, group_idx: usize, roi_idx: usize) -> VizResult<bool> {
        let Some(ref cfg) = self.lead_atlas_load else {
            return Ok(false);
        };
        let (src, name, cache_root) = {
            let groups = self
                .atlas_groups
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let g = groups.get(group_idx).ok_or_else(|| {
                VizError::NiftiLoad(format!("atlas group index {group_idx} out of range"))
            })?;
            let nm = g.meshes.get(roi_idx).ok_or_else(|| {
                VizError::NiftiLoad(format!("atlas ROI index {roi_idx} out of range"))
            })?;
            if nm.mesh.is_some() {
                return Ok(false);
            }
            let Some(ref src) = nm.source else {
                return Ok(false);
            };
            (src.clone(), nm.name.clone(), g.lead_set_dir.clone())
        };
        let params = LeadAtlasMeshParams {
            template: cfg.resample_to_template.then_some(&self.vol),
            resample_to_template: cfg.resample_to_template,
            mesh_smooth_iters: cfg.mesh_smooth_iters,
            source_ijk_subtract: cfg.source_ijk_subtract,
            native_vertex_ijk_shift: cfg.native_vertex_ijk_shift,
        };
        let loaded = match self.atlas_manifest.as_ref() {
            Some(man) => mesh_lead_roi(man, &params, cache_root.as_deref(), &src, &name)?,
            None => mesh_binary_mask_nifti_with_params(&src, &name, &params)?,
        };
        let mut groups = self
            .atlas_groups
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let nm = groups
            .get_mut(group_idx)
            .and_then(|g| g.meshes.get_mut(roi_idx))
            .ok_or_else(|| VizError::NiftiLoad("atlas group/roi disappeared".into()))?;
        nm.mesh = loaded.mesh;
        self.bump_atlas_generation();
        Ok(true)
    }

    /// Queue an atlas ROI for meshing on the background worker.
    ///
    /// Meshing a ROI decompresses a NIfTI mask and runs marching cubes plus
    /// smoothing — seconds to minutes. It must never run on the UI thread, so
    /// enabling a structure only ever enqueues here.
    pub fn request_lead_roi(self: &Arc<Self>, group_idx: usize, roi_idx: usize) {
        if self.lead_atlas_load.is_none() {
            return;
        }
        {
            let groups = self
                .atlas_groups
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(nm) = groups.get(group_idx).and_then(|g| g.meshes.get(roi_idx)) else {
                return;
            };
            if nm.mesh.is_some() || nm.source.is_none() {
                return;
            }
        }
        if !self
            .roi_queued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((group_idx, roi_idx))
        {
            return;
        }
        self.ensure_roi_worker();
        self.roi_jobs_total.fetch_add(1, AOrd::Relaxed);
        let tx = self
            .roi_tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(tx) = tx {
            if tx.send((group_idx, roi_idx)).is_err() {
                self.roi_queued
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&(group_idx, roi_idx));
                self.roi_jobs_done.fetch_add(1, AOrd::Relaxed);
            }
        }
    }

    fn ensure_roi_worker(self: &Arc<Self>) {
        if self
            .roi_worker_started
            .compare_exchange(false, true, AOrd::SeqCst, AOrd::Relaxed)
            .is_err()
        {
            return;
        }
        let (tx, rx) = mpsc::channel::<(usize, usize)>();
        *self.roi_tx.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
        let weak: Weak<Self> = Arc::downgrade(self);
        spawn_worker(move || {
            while let Ok((gix, rix)) = rx.recv() {
                let Some(this) = weak.upgrade() else {
                    break;
                };
                this.run_roi_job(gix, rix);
            }
        });
    }

    fn run_roi_job(&self, gix: usize, rix: usize) {
        let name = self
            .atlas_groups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(gix)
            .and_then(|g| g.meshes.get(rix))
            .map(|nm| nm.name.clone());
        *self
            .roi_current
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = name;
        match self.try_load_lead_roi(gix, rix) {
            Ok(true) => self
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .bump(),
            Ok(false) => {}
            Err(e) => {
                let mut ae = self
                    .atlas_error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if ae.is_none() {
                    *ae = Some(e.to_string());
                    self.atlas_error_set.store(true, AOrd::Relaxed);
                }
            }
        }
        self.roi_queued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(gix, rix));
        self.roi_jobs_done.fetch_add(1, AOrd::Relaxed);
        if self.roi_jobs_done.load(AOrd::Relaxed) >= self.roi_jobs_total.load(AOrd::Relaxed) {
            *self
                .roi_current
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    /// `true` while the initial atlas load or any queued ROI is still working.
    pub fn busy(&self) -> bool {
        self.atlas_loading()
            || self.roi_jobs_done.load(AOrd::Relaxed) < self.roi_jobs_total.load(AOrd::Relaxed)
    }

    /// `(done, total, current item name)` across both the initial atlas load and
    /// the on-demand queue — enough for a real progress bar with an item label.
    pub fn load_progress(&self) -> (usize, usize, Option<String>) {
        let (adone, atotal) = self.atlas_progress();
        let rdone = self.roi_jobs_done.load(AOrd::Relaxed);
        let rtotal = self.roi_jobs_total.load(AOrd::Relaxed);
        let name = self
            .roi_current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        (adone + rdone, atotal + rtotal, name)
    }

    fn lead_atlas_load_config(m: &SceneManifestV1) -> Option<LeadAtlasLoadConfig> {
        (!m.atlas_lead_sets.is_empty() || m.atlas_lead_nii_dir.is_some()).then_some({
            LeadAtlasLoadConfig {
                resample_to_template: m.atlas_mesh_resample_to_template,
                mesh_smooth_iters: m.atlas_mesh_smooth_iters,
                source_ijk_subtract: m.atlas_lead_resample_ijk_subtract,
                native_vertex_ijk_shift: m.atlas_lead_native_mc_ijk_shift,
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        m: &SceneManifestV1,
        vol: TemplateVolume,
        reco: RecoGeometry,
        group: Option<GroupScene>,
        atlas_groups: Vec<AtlasSurfaceGroup>,
        shared: SceneShared,
        view_defaults: SceneViewDefaults,
        lead_atlas_load: Option<LeadAtlasLoadConfig>,
        atlas_manifest: Option<SceneManifestV1>,
        atlas_loading: bool,
        atlas_load_started: bool,
    ) -> Self {
        let vol = Arc::new(vol);
        let group_display =
            GroupDisplay::from_show(&group.as_ref().map_or_else(GroupShowV2::default, |g| g.show));
        let group_models = group
            .as_ref()
            .map(|g| Arc::new(GroupModels::build(&g.model_names())));
        let group_vta_total = group.as_ref().map_or(0, |g| g.vta_jobs().len());
        Self {
            backdrop: Mutex::new((0, Arc::clone(&vol))),
            vol,
            space: m.space.clone(),
            manifest_backdrops: m.backdrops.clone(),
            atlas_ids: manifest_atlas_ids(m),
            atlas_groups: Mutex::new(atlas_groups),
            points_l: reco.points_l,
            points_r: reco.points_r,
            mesh_l: reco.mesh_l,
            mesh_r: reco.mesh_r,
            contacts_l: reco.contacts_l,
            contacts_r: reco.contacts_r,
            electrode_meshes: reco.electrode_meshes,
            shared: Arc::new(Mutex::new(shared)),
            view_defaults,
            lead_atlas_load,
            id: NEXT_PAYLOAD_ID.fetch_add(1, AOrd::Relaxed),
            atlas_generation: AtomicU64::new(0),
            vta: Mutex::new(manifest_vta_surfaces(m)),
            vta_generation: AtomicU64::new(0),
            layers: Mutex::new(SceneLayers::default()),
            fibre_generation: AtomicU64::new(0),
            map_generation: AtomicU64::new(0),
            overlay_pending: Arc::new(AtomicUsize::new(0)),
            layer_build: Mutex::new(LayerBuild::default()),
            group: group.map(Arc::new),
            group_models,
            group_display: Mutex::new(group_display),
            group_generation: AtomicU64::new(0),
            group_vta: Mutex::new(Vec::new()),
            group_vta_done: AtomicUsize::new(0),
            group_vta_total: AtomicUsize::new(group_vta_total),
            group_vta_started: AtomicBool::new(false),
            atlas_manifest,
            atlas_loading: Arc::new(AtomicBool::new(atlas_loading)),
            atlas_load_started: AtomicBool::new(atlas_load_started),
            atlas_error: Arc::new(Mutex::new(None)),
            atlas_error_set: Arc::new(AtomicBool::new(false)),
            atlas_loaded: Arc::new(AtomicUsize::new(0)),
            atlas_total: Arc::new(AtomicUsize::new(0)),
            roi_tx: Mutex::new(None),
            roi_worker_started: AtomicBool::new(false),
            roi_queued: Mutex::new(std::collections::HashSet::new()),
            roi_jobs_total: Arc::new(AtomicUsize::new(0)),
            roi_jobs_done: Arc::new(AtomicUsize::new(0)),
            roi_current: Arc::new(Mutex::new(None)),
        }
    }

    pub fn from_manifest(m: &SceneManifestV1) -> VizResult<Self> {
        let vol = TemplateVolume::load(Path::new(&m.template_nii))?;
        let atlas_groups = build_atlas_groups(m, &vol)?;
        let (reco, group) = load_scene_leads(m)?;

        let (sagittal_i, coronal_j, axial_k) = default_slice_indices(&vol);
        let view_defaults = group_view_defaults(
            &group,
            default_view_defaults(&vol, axial_k, coronal_j, sagittal_i),
        );
        let mut shared = SceneShared {
            azimuth: view_defaults.azimuth,
            elevation: view_defaults.elevation,
            projection: Projection::default(),
            distance: view_defaults.distance,
            target: view_defaults.target,
            axial_k,
            coronal_j,
            sagittal_i,
            show_axial: false,
            show_coronal: true,
            show_sagittal: true,
            axial_opacity: 1.0,
            coronal_opacity: 1.0,
            sagittal_opacity: 1.0,
            atlas_opacity: 1.0,
            atlas_overlay_on: true,
            active_atlas_group: 0,
            atlas_alpha: Vec::new(),
            show_atlas: Vec::new(),
            atlas_rgb: Vec::new(),
            atlas_vectors_group: None,
            atlas_id: None,
            slice_window: 1.0,
            slice_level: 0.5,
            show_electrodes: true,
            show_vta: true,
            show_contact_labels: false,
            revision: 1,
            slice_world: DEFAULT_SLICE_MM,
        };
        resolve_active_atlas(&mut shared, &atlas_groups);
        let active = shared.active_atlas_group;
        reset_atlas_vectors(&mut shared, &atlas_groups, active);
        Ok(Self::assemble(
            m,
            vol,
            reco,
            group,
            atlas_groups,
            shared,
            view_defaults,
            Self::lead_atlas_load_config(m),
            None,
            false,
            true,
        ))
    }

    /// Fast-load path: template + reco only. Atlas loads asynchronously after window is shown.
    pub fn from_manifest_fast(m: &SceneManifestV1) -> VizResult<Self> {
        let vol = TemplateVolume::load(Path::new(&m.template_nii))?;
        let (reco, group) = load_scene_leads(m)?;

        let (sagittal_i, coronal_j, axial_k) = default_slice_indices(&vol);
        let view_defaults = group_view_defaults(
            &group,
            default_view_defaults(&vol, axial_k, coronal_j, sagittal_i),
        );
        let shared = SceneShared {
            azimuth: view_defaults.azimuth,
            elevation: view_defaults.elevation,
            projection: Projection::default(),
            distance: view_defaults.distance,
            target: view_defaults.target,
            axial_k,
            coronal_j,
            sagittal_i,
            show_axial: false,
            show_coronal: true,
            show_sagittal: true,
            axial_opacity: 1.0,
            coronal_opacity: 1.0,
            sagittal_opacity: 1.0,
            atlas_opacity: 1.0,
            atlas_overlay_on: true,
            active_atlas_group: 0,
            atlas_alpha: Vec::new(),
            show_atlas: Vec::new(),
            atlas_rgb: Vec::new(),
            atlas_vectors_group: None,
            atlas_id: None,
            slice_window: 1.0,
            slice_level: 0.5,
            show_electrodes: true,
            show_vta: true,
            show_contact_labels: false,
            revision: 1,
            slice_world: DEFAULT_SLICE_MM,
        };
        let want_atlas = m.atlas_obj_dir.is_some()
            || m.atlas_nii.is_some()
            || !m.atlas_lead_sets.is_empty()
            || m.atlas_lead_nii_dir.is_some();
        Ok(Self::assemble(
            m,
            vol,
            reco,
            group,
            Vec::new(),
            shared,
            view_defaults,
            Self::lead_atlas_load_config(m),
            want_atlas.then(|| m.clone()),
            want_atlas,
            false,
        ))
    }

    pub fn atlas_loading(&self) -> bool {
        self.atlas_loading.load(AOrd::Relaxed)
    }

    pub fn atlas_error_message(&self) -> Option<String> {
        if !self.has_atlas_error() {
            return None;
        }
        self.atlas_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Lock-free "is there an atlas error" test, for the paint path.
    pub fn has_atlas_error(&self) -> bool {
        self.atlas_error_set.load(AOrd::Relaxed)
    }

    pub fn atlas_progress(&self) -> (usize, usize) {
        (
            self.atlas_loaded.load(AOrd::Relaxed),
            self.atlas_total.load(AOrd::Relaxed),
        )
    }

    pub fn start_atlas_load_if_needed(self: &Arc<Self>) {
        if self.atlas_manifest.is_none() {
            return;
        }
        if self
            .atlas_load_started
            .compare_exchange(false, true, AOrd::SeqCst, AOrd::Relaxed)
            .is_err()
        {
            return;
        }

        let this = Arc::clone(self);
        let Some(man) = this.atlas_manifest.clone() else {
            return;
        };
        let atlas_loading = Arc::clone(&this.atlas_loading);
        let atlas_error = Arc::clone(&this.atlas_error);
        let atlas_error_set = Arc::clone(&this.atlas_error_set);
        let atlas_loaded = Arc::clone(&this.atlas_loaded);
        let atlas_total = Arc::clone(&this.atlas_total);

        spawn_worker(move || {
            let r: VizResult<()> = (|| {
                let stub_groups = atlas_group_stubs(&man)?;
                {
                    let mut g = this
                        .atlas_groups
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    *g = stub_groups;
                }
                this.bump_atlas_generation();

                {
                    let mut sh = this.shared.lock().unwrap_or_else(PoisonError::into_inner);
                    let groups = this
                        .atlas_groups
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    resolve_active_atlas(&mut sh, &groups);
                    let gix = sh.active_atlas_group;
                    let n = groups.get(gix).map_or(0, |g| g.meshes.len());
                    if sh.atlas_vectors_group != Some(gix) || sh.show_atlas.len() != n {
                        reset_atlas_vectors(&mut sh, &groups, gix);
                    }
                    drop(groups);
                    sh.bump();
                }

                let Some(cfg) = &this.lead_atlas_load else {
                    atlas_loading.store(false, AOrd::Relaxed);
                    return Ok(());
                };
                let params = LeadAtlasMeshParams {
                    template: cfg.resample_to_template.then_some(&this.vol),
                    resample_to_template: cfg.resample_to_template,
                    mesh_smooth_iters: cfg.mesh_smooth_iters,
                    source_ijk_subtract: cfg.source_ijk_subtract,
                    native_vertex_ijk_shift: cfg.native_vertex_ijk_shift,
                };

                let (group_idx, cache_root, worklist) = {
                    let sh = this.shared.lock().unwrap_or_else(PoisonError::into_inner);
                    let ag = this
                        .atlas_groups
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    let gix = sh.active_atlas_group.min(ag.len().saturating_sub(1));
                    let Some(g) = ag.get(gix) else {
                        return Ok(());
                    };
                    let flags = if sh.atlas_vectors_group == Some(gix)
                        && sh.show_atlas.len() == g.meshes.len()
                    {
                        sh.show_atlas.clone()
                    } else {
                        g.default_show_flags()
                    };
                    let mut items: Vec<RoiWork> = Vec::new();
                    for (i, nm) in g.meshes.iter().enumerate() {
                        if let Some(src) = &nm.source {
                            let want = flags.get(i).copied().unwrap_or(false)
                                || !man.atlas_lead_lazy_load
                                || !man.atlas_cache_only_default_on;
                            if want && nm.mesh.is_none() {
                                items.push((i, nm.name.clone(), src.clone()));
                            }
                        }
                    }
                    (gix, g.lead_set_dir.clone(), items)
                };

                atlas_total.store(worklist.len(), AOrd::Relaxed);
                atlas_loaded.store(0, AOrd::Relaxed);

                let (tx, rx) = mpsc::channel::<
                    VizResult<(usize, Option<crate::atlas_wavefront::TriMeshScene>)>,
                >();
                let job = |tx: &mut mpsc::Sender<
                    VizResult<(usize, Option<crate::atlas_wavefront::TriMeshScene>)>,
                >,
                           (roi_idx, name, src): RoiWork| {
                    let r = mesh_lead_roi(&man, &params, cache_root.as_deref(), &src, &name)
                        .map(|nm| (roi_idx, nm.mesh));
                    let _ = tx.send(r);
                };
                for_each_worklist(worklist, tx.clone(), job);
                drop(tx);

                for r in rx.iter() {
                    match r {
                        Ok((roi_idx, mesh)) => {
                            {
                                let mut groups = this
                                    .atlas_groups
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner);
                                if let Some(nm) = groups
                                    .get_mut(group_idx)
                                    .and_then(|g| g.meshes.get_mut(roi_idx))
                                {
                                    nm.mesh = mesh;
                                }
                            }
                            this.bump_atlas_generation();
                            atlas_loaded.fetch_add(1, AOrd::Relaxed);
                            this.shared
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .bump();
                        }
                        Err(e) => {
                            let mut ae = atlas_error.lock().unwrap_or_else(PoisonError::into_inner);
                            if ae.is_none() {
                                *ae = Some(e.to_string());
                                atlas_error_set.store(true, AOrd::Relaxed);
                            }
                        }
                    }
                }

                Ok(())
            })();
            if let Err(e) = r {
                *atlas_error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
                atlas_error_set.store(true, AOrd::Relaxed);
            }
            atlas_loading.store(false, AOrd::Relaxed);
        });
    }

    pub fn shared_clone(&self) -> Arc<Mutex<SceneShared>> {
        Arc::clone(&self.shared)
    }
}

#[inline]
fn slice_uv(u: f32, v: f32) -> [f32; 2] {
    [u, 1.0_f32 - v]
}

fn append_quad_strip(out: &mut Vec<QuadVert>, corners: [Vec3; 4], uv_corner: [[f32; 2]; 4]) {
    for i in 0..4 {
        out.push(QuadVert {
            pos: corners[i].to_array(),
            uv: slice_uv(uv_corner[i][0], uv_corner[i][1]),
            _pad: [1.0, 0.0],
        });
    }
}

/// UV inputs before [`slice_uv`] for strip order `[c0,c1,c3,c2]` (must match `scene_volume` row layout).
const SLICE_UV_AXIAL: [[f32; 2]; 4] = [[0.0, 1.0], [0.0, 0.0], [1.0, 1.0], [1.0, 0.0]];
const SLICE_UV_CORONAL: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
const SLICE_UV_SAGITTAL: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];

type OverlayKey = (u64, u64, usize, usize, [u32; 3]);
type OverlayPixels = (Vec<u8>, u32, u32);

struct OverlayJob {
    key: OverlayKey,
    result: Arc<Mutex<Option<OverlayPixels>>>,
}

fn overlay_key(
    payload_id: u64,
    backdrop_gen: u64,
    index: usize,
    layer: &Arc<crate::scene_layers::MapLayer>,
    settings: &crate::scene_layers::MapSettings,
) -> OverlayKey {
    (
        payload_id,
        backdrop_gen,
        index,
        Arc::as_ptr(layer) as usize,
        [
            settings.threshold.to_bits(),
            settings.max_pos.to_bits(),
            settings.max_neg.to_bits(),
        ],
    )
}

struct SlicePlaneSlot {
    tex: wgpu::Texture,
    bind: wgpu::BindGroup,
    tex_w: u32,
    tex_h: u32,
    format: wgpu::TextureFormat,
}

impl SlicePlaneSlot {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        label: &str,
        tw: u32,
        th: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("{label}_bg")),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Self {
            tex: texture,
            bind,
            tex_w: tw,
            tex_h: th,
            format,
        }
    }

    fn resize_if_needed(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        label: &str,
        tw: u32,
        th: u32,
    ) {
        if self.tex_w == tw && self.tex_h == th {
            return;
        }
        *self = Self::new(device, layout, sampler, label, tw, th, self.format);
    }
}

struct InstDraw {
    range: MeshRange,
    instances: std::ops::Range<u32>,
}

fn instance_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    label: &'static str,
    fragment: &'static str,
    targets: &[Option<wgpu::ColorTargetState>],
    depth: Option<wgpu::DepthStencilState>,
) -> wgpu::RenderPipeline {
    let float4 = |offset: u64, location: u32| wgpu::VertexAttribute {
        offset,
        shader_location: location,
        format: wgpu::VertexFormat::Float32x4,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_inst"),
            buffers: &[
                Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MeshVert>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        float4(24, 2),
                    ],
                }),
                Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        float4(0, 3),
                        float4(16, 4),
                        float4(32, 5),
                        float4(48, 6),
                        float4(64, 7),
                    ],
                }),
            ],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment),
            targets,
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: depth,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

pub struct SceneWgpuRenderer {
    /// Atlas meshes: weighted blended order-independent transparency into the accumulation
    /// targets; depth test against the opaque pass, no depth write.
    atlas_pipeline: wgpu::RenderPipeline,
    /// Electrode lead bodies: opaque, depth write.
    electrode_pipeline: wgpu::RenderPipeline,
    /// Template slice quads: opaque, depth write.
    quad_pipeline: wgpu::RenderPipeline,
    quad_pipeline_overlay: wgpu::RenderPipeline,
    /// Reco polylines: opaque, depth test and write.
    line_pipeline: wgpu::RenderPipeline,
    /// Instanced lead bodies and contact markers of a group scene: opaque, depth write.
    inst_pipeline: wgpu::RenderPipeline,
    /// The same instances at reduced opacity, through the order-independent transparency pass.
    inst_oit_pipeline: wgpu::RenderPipeline,
    compositor: OitCompositor,
    quad_layout: wgpu::BindGroupLayout,
    slab_uni: wgpu::Buffer,
    mesh_vb: wgpu::Buffer,
    mesh_vb_cap: u64,
    mesh_ib: wgpu::Buffer,
    mesh_ib_cap: u64,
    atlas_indices: std::ops::Range<u32>,
    oit_frame: [f32; 4],
    /// Per electrode surface group (insulation / metal): (first index, index count). Drawn
    /// opaque with the depth pipeline so leads occlude anatomy and other overlays.
    electrode_draw_ranges: Vec<(u32, u32)>,
    inst_mesh_vb: wgpu::Buffer,
    inst_mesh_vb_cap: u64,
    inst_mesh_ib: wgpu::Buffer,
    inst_mesh_ib_cap: u64,
    inst_vb: wgpu::Buffer,
    inst_vb_cap: u64,
    inst_models_payload: Option<u64>,
    inst_draws_opaque: Vec<InstDraw>,
    inst_draws_dimmed: Vec<InstDraw>,
    geometry_key: Option<GeometryKey>,
    slice_keys: [Option<(u64, u64, usize)>; 3],
    quad_vb: wgpu::Buffer,
    quad_vb_cap: u64,
    line_vb: wgpu::Buffer,
    line_vb_cap: u64,
    line_vert_count: u32,
    slice_sampler: wgpu::Sampler,
    slice_axial: SlicePlaneSlot,
    slice_coronal: SlicePlaneSlot,
    slice_sagittal: SlicePlaneSlot,
    overlay_slots: [SlicePlaneSlot; 3],
    overlay_keys: [Option<OverlayKey>; 3],
    overlay_jobs: [Option<OverlayJob>; 3],
    overlay_on: [bool; 3],
    fibre_pipeline: wgpu::RenderPipeline,
    quad_rgba_pipeline: wgpu::RenderPipeline,
    fibre_chunks: Vec<(wgpu::Buffer, u32, u64)>,
    fibre_used: usize,
    fibre_key: Option<(u64, u64)>,
    ribbon_params: [f32; 4],
    /// Per plane axial/coronal/sagittal: base vertex index and count (0 or 4).
    quad_ranges: [[u32; 2]; 3],
    uni_bind: wgpu::BindGroup,
    last_rev: u64,
    rendered: Option<RenderKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RenderKey {
    payload_id: u64,
    geometry_rev: u64,
    camera_rev: u64,
    size: [u32; 2],
    aspect_bits: u32,
}

/// Write bytes into an existing wgpu buffer, or reallocate (doubling) when capacity is exceeded.
fn write_or_grow_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buf: &mut wgpu::Buffer,
    cap: &mut u64,
    data: &[u8],
    usage: wgpu::BufferUsages,
    label: &str,
) {
    let needed = data.len().max(1) as u64;
    if needed > *cap {
        let new_cap = (needed * 2).next_power_of_two();
        *buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: new_cap,
            usage,
            mapped_at_creation: false,
        });
        *cap = new_cap;
    }
    queue.write_buffer(buf, 0, data);
}

impl SceneWgpuRenderer {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        depth_format: Option<wgpu::TextureFormat>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene3d"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });

        let mesh_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene_uni"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(256),
                },
                count: None,
            }],
        });

        let quad_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene_quad"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipe_mesh = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene_mesh_pl"),
            bind_group_layouts: &[Some(&mesh_layout)],
            immediate_size: 0,
        });

        let pipe_quad = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene_quad_pl"),
            bind_group_layouts: &[Some(&mesh_layout), Some(&quad_layout)],
            immediate_size: 0,
        });

        let targets_blend = &[Some(wgpu::ColorTargetState {
            format: target_format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let targets_opaque = &[Some(wgpu::ColorTargetState {
            format: target_format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let depth_state = |write: bool, compare: wgpu::CompareFunction| {
            Some(wgpu::DepthStencilState {
                format: OPAQUE_DEPTH_FORMAT,
                depth_write_enabled: Some(write),
                depth_compare: Some(compare),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            })
        };
        let atlas_depth = depth_state(false, wgpu::CompareFunction::LessEqual);
        let quad_depth = depth_state(true, wgpu::CompareFunction::Less);
        let quad_depth_overlay = depth_state(false, wgpu::CompareFunction::Less);
        let line_depth = depth_state(true, wgpu::CompareFunction::Less);
        let accumulation_targets = accumulation_targets();

        let atlas_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene_atlas"),
            layout: Some(&pipe_mesh),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MeshVert>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 24,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_atlas_oit"),
                targets: &accumulation_targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: atlas_depth,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let electrode_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene_electrode"),
            layout: Some(&pipe_mesh),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MeshVert>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 24,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: targets_opaque,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: quad_depth.clone(),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let make_quad_pipeline =
            |label: &'static str, depth_stencil: Option<wgpu::DepthStencilState>| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pipe_quad),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_quad"),
                        buffers: &[Some(wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<QuadVert>() as wgpu::BufferAddress,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &[
                                wgpu::VertexAttribute {
                                    offset: 0,
                                    shader_location: 0,
                                    format: wgpu::VertexFormat::Float32x3,
                                },
                                wgpu::VertexAttribute {
                                    offset: 12,
                                    shader_location: 1,
                                    format: wgpu::VertexFormat::Float32x2,
                                },
                                wgpu::VertexAttribute {
                                    offset: 20,
                                    shader_location: 2,
                                    format: wgpu::VertexFormat::Float32,
                                },
                            ],
                        })],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_quad"),
                        targets: targets_blend,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleStrip,
                        ..Default::default()
                    },
                    depth_stencil,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let quad_pipeline = make_quad_pipeline("scene_quad", quad_depth);
        let quad_pipeline_overlay = make_quad_pipeline("scene_quad_overlay", quad_depth_overlay);

        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene_line"),
            layout: Some(&pipe_mesh),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_line"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        offset: 0,
                        shader_location: 0,
                        format: wgpu::VertexFormat::Float32x3,
                    }],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_line"),
                targets: targets_opaque,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: line_depth,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let inst_pipeline = instance_pipeline(
            device,
            &shader,
            &pipe_mesh,
            "scene_instances",
            "fs_inst",
            targets_opaque,
            depth_state(true, wgpu::CompareFunction::Less),
        );
        let inst_oit_pipeline = instance_pipeline(
            device,
            &shader,
            &pipe_mesh,
            "scene_instances_oit",
            "fs_inst_oit",
            &accumulation_targets,
            depth_state(false, wgpu::CompareFunction::LessEqual),
        );
        let fibre_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene_fibres"),
            layout: Some(&pipe_mesh),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fibre"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<FibreSeg>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 24,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Unorm8x4,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_fibre"),
                targets: targets_opaque,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: depth_state(true, wgpu::CompareFunction::Less),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let quad_rgba_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene_quad_rgba"),
            layout: Some(&pipe_quad),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_quad"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadVert>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 20,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_quad_rgba"),
                targets: targets_blend,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: OPAQUE_DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: -4,
                    slope_scale: -2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let make_buffer = |label: &str, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 64,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let inst_mesh_vb = make_buffer("inst_mesh_vb", wgpu::BufferUsages::VERTEX);
        let inst_mesh_ib = make_buffer("inst_mesh_ib", wgpu::BufferUsages::INDEX);
        let inst_vb = make_buffer("inst_vb", wgpu::BufferUsages::VERTEX);

        let slab_uni = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene_globals"),
            size: 256,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uni_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene_uni_bg"),
            layout: &mesh_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: slab_uni.as_entire_binding(),
            }],
        });

        let mesh_vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mesh_vb"),
            size: 64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mesh_ib = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mesh_ib"),
            size: 64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let quad_vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad_vb"),
            size: 512,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let line_vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("line_vb"),
            size: 64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let slice_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("slice_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let slot = |label: &str, format: wgpu::TextureFormat| {
            SlicePlaneSlot::new(device, &quad_layout, &slice_sampler, label, 4, 4, format)
        };
        let gray = wgpu::TextureFormat::R8Unorm;
        let slice_axial = slot("slice_axial", gray);
        let slice_coronal = slot("slice_coronal", gray);
        let slice_sagittal = slot("slice_sagittal", gray);
        let rgba = wgpu::TextureFormat::Rgba8Unorm;
        let overlay_slots = [
            slot("map_overlay_axial", rgba),
            slot("map_overlay_coronal", rgba),
            slot("map_overlay_sagittal", rgba),
        ];

        Self {
            atlas_pipeline,
            electrode_pipeline,
            quad_pipeline,
            quad_pipeline_overlay,
            line_pipeline,
            inst_pipeline,
            inst_oit_pipeline,
            compositor: OitCompositor::new(device, target_format, depth_format),
            quad_layout,
            slab_uni,
            mesh_vb_cap: 64,
            mesh_vb,
            mesh_ib_cap: 64,
            mesh_ib,
            atlas_indices: 0..0,
            oit_frame: depth_frame([]),
            electrode_draw_ranges: Vec::new(),
            inst_mesh_vb,
            inst_mesh_vb_cap: 64,
            inst_mesh_ib,
            inst_mesh_ib_cap: 64,
            inst_vb,
            inst_vb_cap: 64,
            inst_models_payload: None,
            inst_draws_opaque: Vec::new(),
            inst_draws_dimmed: Vec::new(),
            geometry_key: None,
            slice_keys: [None; 3],
            quad_vb_cap: 512,
            quad_vb,
            line_vb_cap: 64,
            line_vb,
            line_vert_count: 0,
            slice_sampler,
            slice_axial,
            slice_coronal,
            slice_sagittal,
            overlay_slots,
            overlay_keys: [None; 3],
            overlay_jobs: [None, None, None],
            overlay_on: [false; 3],
            fibre_pipeline,
            quad_rgba_pipeline,
            fibre_chunks: Vec::new(),
            fibre_used: 0,
            fibre_key: None,
            ribbon_params: [1.0, 1.0, 0.0, 0.0],
            quad_ranges: [[0, 0]; 3],
            uni_bind,
            last_rev: 0,
            rendered: None,
        }
    }

    fn rebuild_geometry(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        payload: &Scene3DPayload,
    ) {
        let sh = payload
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let key = GeometryKey::new(&sh, payload);
        if self.geometry_key.as_ref() != Some(&key) {
            self.upload_meshes(device, queue, payload, &sh);
            self.geometry_key = Some(key);
        }
        self.upload_fibres(device, queue, payload);
        self.update_slices(device, queue, payload, &sh);
        self.last_rev = sh.revision;
    }

    fn upload_fibres(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        payload: &Scene3DPayload,
    ) {
        let (generation, geometry) = payload.fibre_geometry();
        let key = (payload.id, generation);
        if self.fibre_key == Some(key) {
            return;
        }
        let mut used = 0;
        if let Some(geometry) = geometry {
            let seg = std::mem::size_of::<FibreSeg>() as u64;
            let per_buffer = (device.limits().max_buffer_size / 2 / seg).clamp(1, 1 << 21) as usize;
            for chunk in geometry.segments.chunks(per_buffer) {
                let bytes = chunk.len() as u64 * seg;
                let fits = self
                    .fibre_chunks
                    .get(used)
                    .is_some_and(|(_, _, capacity)| *capacity >= bytes);
                if !fits {
                    let capacity = (bytes + bytes / 2).min(per_buffer as u64 * seg).max(bytes);
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("fibre_segments"),
                        size: capacity,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    if used < self.fibre_chunks.len() {
                        self.fibre_chunks[used] = (buffer, 0, capacity);
                    } else {
                        self.fibre_chunks.push((buffer, 0, capacity));
                    }
                }
                let slot = &mut self.fibre_chunks[used];
                queue.write_buffer(&slot.0, 0, bytemuck::cast_slice(chunk));
                slot.1 = chunk.len() as u32;
                used += 1;
            }
        }
        self.fibre_used = used;
        self.fibre_key = Some(key);
    }

    fn upload_meshes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        payload: &Scene3DPayload,
        sh: &SceneShared,
    ) {
        let mut verts: Vec<MeshVert> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        self.electrode_draw_ranges.clear();

        let groups_lock = payload
            .atlas_groups
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let atlas_meshes: &[NamedAtlasMesh] = if sh.atlas_overlay_on {
            let ix = sh
                .active_atlas_group
                .min(groups_lock.len().saturating_sub(1));
            groups_lock
                .get(ix)
                .map(|g| g.meshes.as_slice())
                .unwrap_or(&[])
        } else {
            &[]
        };

        let g_ao = sh.atlas_opacity.clamp(0.0, 1.0);
        let mut atlas_draws = 0usize;
        for (ai, nm) in atlas_meshes.iter().enumerate() {
            if !sh.show_atlas.get(ai).copied().unwrap_or(false) {
                continue;
            }
            let Some(mesh) = nm.mesh.as_ref() else {
                continue;
            };
            let rgb =
                sh.atlas_rgb
                    .get(ai)
                    .copied()
                    .unwrap_or([nm.color[0], nm.color[1], nm.color[2]]);
            let roi = sh
                .atlas_alpha
                .get(ai)
                .copied()
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
            let colour = [rgb[0], rgb[1], rgb[2], nm.color[3] * g_ao * roi];
            if append_indexed_mesh(&mut verts, &mut indices, mesh, colour).is_some() {
                atlas_draws += 1;
            }
        }
        drop(groups_lock);
        if sh.show_vta {
            let surfaces = payload.vta.lock().unwrap_or_else(PoisonError::into_inner);
            for surface in surfaces.iter() {
                if append_indexed_mesh(&mut verts, &mut indices, &surface.mesh, VTA_RGBA).is_some()
                {
                    atlas_draws += 1;
                }
            }
        }
        if let (_, Some(maps)) = payload.map_meshes() {
            for meshes in maps.iter() {
                for (mesh, colour) in [
                    (&meshes.positive, MAP_POSITIVE_RGBA),
                    (&meshes.negative, MAP_NEGATIVE_RGBA),
                ] {
                    if append_indexed_mesh(&mut verts, &mut indices, mesh, colour).is_some() {
                        atlas_draws += 1;
                    }
                }
            }
        }
        let group_display = payload.group_display();
        if let (Some(group), Some(display)) = (payload.group.as_ref(), group_display.as_ref()) {
            if display.show_vtas && sh.show_vta {
                let meshes = payload
                    .group_vta
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                for (lead, mesh) in meshes.iter() {
                    if let Some(l) = group.leads.get(*lead) {
                        let colour = group.vta_colour(display, l);
                        if append_indexed_mesh(&mut verts, &mut indices, mesh, colour).is_some() {
                            atlas_draws += 1;
                        }
                    }
                }
            }
        }
        self.atlas_indices = 0..u32::try_from(indices.len()).unwrap_or(u32::MAX);
        self.oit_frame = depth_frame(verts.iter().map(|v| Vec3::from_array(v.pos)));

        if sh.show_electrodes {
            for group in &payload.electrode_meshes {
                for (tri, colour) in [
                    (&group.insulation, INSULATION_RGBA),
                    (&group.metal, METAL_RGBA),
                ] {
                    if let Some(range) = append_indexed_mesh(&mut verts, &mut indices, tri, colour)
                    {
                        self.electrode_draw_ranges.push(range);
                    }
                }
            }
        }
        self.inst_draws_opaque.clear();
        self.inst_draws_dimmed.clear();
        if let (Some(group), Some(models), Some(display)) = (
            payload.group.as_ref(),
            payload.group_models.as_ref(),
            group_display.as_ref(),
        ) {
            self.upload_group_instances(
                device,
                queue,
                payload.id,
                group,
                models,
                display,
                sh.show_electrodes,
            );
        }
        crate::viz_eprintln!(
            "[scene3d] upload_meshes: atlas_draws={} verts={} indices={}",
            atlas_draws,
            verts.len(),
            indices.len(),
        );

        if !verts.is_empty() {
            write_or_grow_buffer(
                device,
                queue,
                &mut self.mesh_vb,
                &mut self.mesh_vb_cap,
                bytemuck::cast_slice(&verts),
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                "mesh_vb",
            );
            write_or_grow_buffer(
                device,
                queue,
                &mut self.mesh_ib,
                &mut self.mesh_ib_cap,
                bytemuck::cast_slice(&indices),
                wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                "mesh_ib",
            );
        }

        let mut line_verts: Vec<[f32; 3]> = Vec::new();
        for (pts, has_mesh) in [
            (&payload.points_l, payload.mesh_l),
            (&payload.points_r, payload.mesh_r),
        ] {
            line_verts.extend(
                fallback_line_vertices(pts, !has_mesh)
                    .iter()
                    .map(|v| v.to_array()),
            );
        }
        self.line_vert_count = line_verts.len() as u32;
        if !line_verts.is_empty() {
            write_or_grow_buffer(
                device,
                queue,
                &mut self.line_vb,
                &mut self.line_vb_cap,
                bytemuck::cast_slice(&line_verts),
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                "line_vb",
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_group_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        payload_id: u64,
        group: &GroupScene,
        models: &GroupModels,
        display: &GroupDisplay,
        show_bodies: bool,
    ) {
        if self.inst_models_payload != Some(payload_id) {
            let verts: Vec<MeshVert> = models
                .positions
                .iter()
                .zip(&models.normals)
                .map(|(p, n)| MeshVert {
                    pos: *p,
                    n: *n,
                    c: [1.0; 4],
                })
                .collect();
            if !verts.is_empty() {
                write_or_grow_buffer(
                    device,
                    queue,
                    &mut self.inst_mesh_vb,
                    &mut self.inst_mesh_vb_cap,
                    bytemuck::cast_slice(&verts),
                    wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    "inst_mesh_vb",
                );
                write_or_grow_buffer(
                    device,
                    queue,
                    &mut self.inst_mesh_ib,
                    &mut self.inst_mesh_ib_cap,
                    bytemuck::cast_slice(&models.indices),
                    wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                    "inst_mesh_ib",
                );
            }
            self.inst_models_payload = Some(payload_id);
        }
        let mut instances: Vec<Instance> = Vec::new();
        let mut dimmed: Vec<(MeshRange, &[Instance])> = Vec::new();
        let batches = group.build_batches(display, models);
        for batch in &batches {
            let range = match batch.part {
                Some(crate::group_scene::Part::Sphere) => models.sphere,
                Some(part) if show_bodies => {
                    let idx = batch
                        .model
                        .as_deref()
                        .and_then(|m| models.names.iter().position(|n| n == m));
                    idx.and_then(|i| match part {
                        crate::group_scene::Part::Insulation => models.insulation[i],
                        _ => models.metal[i],
                    })
                }
                _ => None,
            };
            let Some(range) = range else {
                continue;
            };
            if !batch.opaque.is_empty() {
                let start = instances.len() as u32;
                instances.extend_from_slice(&batch.opaque);
                self.inst_draws_opaque.push(InstDraw {
                    range,
                    instances: start..instances.len() as u32,
                });
            }
            if !batch.dimmed.is_empty() {
                dimmed.push((range, &batch.dimmed));
            }
        }
        for (range, list) in dimmed {
            let start = instances.len() as u32;
            instances.extend_from_slice(list);
            self.inst_draws_dimmed.push(InstDraw {
                range,
                instances: start..instances.len() as u32,
            });
        }
        if !instances.is_empty() {
            write_or_grow_buffer(
                device,
                queue,
                &mut self.inst_vb,
                &mut self.inst_vb_cap,
                bytemuck::cast_slice(&instances),
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                "inst_vb",
            );
        }
    }

    fn update_slices(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        payload: &Scene3DPayload,
        sh: &SceneShared,
    ) {
        let (backdrop_gen, vol) = payload.backdrop_snapshot();
        let hdr = &vol.hdr;
        let (n0, n1, n2) = (vol.n0, vol.n1, vol.n2);
        let lo = -0.5_f32;
        let hi0 = n0 as f32 - 0.5;
        let hi1 = n1 as f32 - 0.5;
        let hi2 = n2 as f32 - 0.5;

        let mut quad_verts: Vec<QuadVert> = Vec::new();
        let mut quad_ranges = [[0u32, 0u32]; 3];
        let mut overlay_on = [false; 3];
        let active_map = payload.with_layers(|l| l.active_slice_map());
        if active_map.is_none() {
            self.overlay_keys = [None; 3];
            self.overlay_jobs = [None, None, None];
        }
        let planes = [
            (
                sh.show_axial,
                sh.axial_k.min(n2.saturating_sub(1)),
                sh.axial_opacity,
            ),
            (
                sh.show_coronal,
                sh.coronal_j.min(n1.saturating_sub(1)),
                sh.coronal_opacity,
            ),
            (
                sh.show_sagittal,
                sh.sagittal_i.min(n0.saturating_sub(1)),
                sh.sagittal_opacity,
            ),
        ];
        for (p, &(show, index, opacity)) in planes.iter().enumerate() {
            if !show {
                continue;
            }
            let slice_key = (payload.id, backdrop_gen, index);
            if self.slice_keys[p] != Some(slice_key) {
                let (buf, bpr, tw, th) = match p {
                    0 => vol.axial_r8(index),
                    1 => vol.coronal_r8(index),
                    _ => vol.sagittal_r8(index),
                };
                let label = ["slice_axial", "slice_coronal", "slice_sagittal"][p];
                let slot = match p {
                    0 => &mut self.slice_axial,
                    1 => &mut self.slice_coronal,
                    _ => &mut self.slice_sagittal,
                };
                slot.resize_if_needed(
                    device,
                    &self.quad_layout,
                    &self.slice_sampler,
                    label,
                    tw,
                    th,
                );
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &slot.tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &buf,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bpr),
                        rows_per_image: Some(th),
                    },
                    wgpu::Extent3d {
                        width: tw,
                        height: th,
                        depth_or_array_layers: 1,
                    },
                );
                self.slice_keys[p] = Some(slice_key);
            }
            let f = index as f32;
            let (corners, uv) = match p {
                0 => (
                    [
                        ijk_to_world_mm(hdr, lo, lo, f),
                        ijk_to_world_mm(hdr, hi0, lo, f),
                        ijk_to_world_mm(hdr, hi0, hi1, f),
                        ijk_to_world_mm(hdr, lo, hi1, f),
                    ],
                    SLICE_UV_AXIAL,
                ),
                1 => (
                    [
                        ijk_to_world_mm(hdr, lo, f, lo),
                        ijk_to_world_mm(hdr, hi0, f, lo),
                        ijk_to_world_mm(hdr, hi0, f, hi2),
                        ijk_to_world_mm(hdr, lo, f, hi2),
                    ],
                    SLICE_UV_CORONAL,
                ),
                _ => (
                    [
                        ijk_to_world_mm(hdr, f, lo, lo),
                        ijk_to_world_mm(hdr, f, hi1, lo),
                        ijk_to_world_mm(hdr, f, hi1, hi2),
                        ijk_to_world_mm(hdr, f, lo, hi2),
                    ],
                    SLICE_UV_SAGITTAL,
                ),
            };
            let [c0, c1, c2, c3] = corners;
            let base = quad_verts.len();
            quad_ranges[p][0] = base as u32;
            append_quad_strip(&mut quad_verts, [c0, c1, c3, c2], uv);
            let a = opacity.clamp(0.0, 1.0);
            for v in &mut quad_verts[base..] {
                v._pad[0] = a;
            }
            quad_ranges[p][1] = 4;
            if let Some((layer, settings)) = active_map.as_ref() {
                let key = overlay_key(payload.id, backdrop_gen, index, layer, settings);
                if self.overlay_keys[p] != Some(key) {
                    let finished = self.overlay_jobs[p].as_ref().and_then(|job| {
                        job.result
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .take()
                            .map(|pixels| (job.key, pixels))
                    });
                    if let Some((done_key, (pixels, w, h))) = finished {
                        self.overlay_jobs[p] = None;
                        let label = [
                            "map_overlay_axial",
                            "map_overlay_coronal",
                            "map_overlay_sagittal",
                        ][p];
                        self.overlay_slots[p].resize_if_needed(
                            device,
                            &self.quad_layout,
                            &self.slice_sampler,
                            label,
                            w,
                            h,
                        );
                        queue.write_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &self.overlay_slots[p].tex,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            &pixels,
                            wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(w * 4),
                                rows_per_image: Some(h),
                            },
                            wgpu::Extent3d {
                                width: w,
                                height: h,
                                depth_or_array_layers: 1,
                            },
                        );
                        self.overlay_keys[p] = Some(done_key);
                    }
                    if self.overlay_keys[p] != Some(key) && self.overlay_jobs[p].is_none() {
                        let plane = [Plane::Axial, Plane::Coronal, Plane::Sagittal][p];
                        let affine = fastwarp_core::nifti_affine::header_affine(hdr);
                        let result = Arc::new(Mutex::new(None));
                        self.overlay_jobs[p] = Some(OverlayJob {
                            key,
                            result: Arc::clone(&result),
                        });
                        let (layer, settings) = (Arc::clone(layer), *settings);
                        let shared = Arc::clone(&payload.shared);
                        let pending = Arc::clone(&payload.overlay_pending);
                        pending.fetch_add(1, AOrd::AcqRel);
                        spawn_worker(move || {
                            let pixels = resample_plane(
                                &layer,
                                &settings,
                                &affine,
                                [n0, n1, n2],
                                plane,
                                index,
                            )
                            .unwrap_or_else(|| {
                                let (w, h) =
                                    crate::scene_maps::plane_texture_size(plane, [n0, n1, n2]);
                                (vec![0u8; w * h * 4], w as u32, h as u32)
                            });
                            *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(pixels);
                            #[cfg(not(target_arch = "wasm32"))]
                            shared.lock().unwrap_or_else(PoisonError::into_inner).bump();
                            #[cfg(target_arch = "wasm32")]
                            drop(shared);
                            pending.fetch_sub(1, AOrd::AcqRel);
                        });
                    }
                }
                overlay_on[p] = self.overlay_keys[p].is_some();
            }
        }
        self.quad_ranges = quad_ranges;
        self.overlay_on = overlay_on;
        if !quad_verts.is_empty() {
            write_or_grow_buffer(
                device,
                queue,
                &mut self.quad_vb,
                &mut self.quad_vb_cap,
                bytemuck::cast_slice(&quad_verts),
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                "quad_vb",
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_globals(
        &self,
        queue: &wgpu::Queue,
        vp: Mat4,
        eye: Vec3,
        view_fwd: Vec3,
        atlas_unlit: bool,
        atlas_flat_normals: bool,
        slice_window: f32,
        slice_level: f32,
    ) {
        let mut g = GlobalsGpu::zeroed();
        g.view_proj = vp.to_cols_array_2d();
        let l = Vec3::new(0.45, 0.65, 0.55).normalize();
        g.light_dir = [l.x, l.y, l.z, 0.0];
        g.cam_pos = [eye.x, eye.y, eye.z, 0.0];
        g.atlas_unlit = atlas_unlit as u8 as f32;
        g.atlas_flat_normals = atlas_flat_normals as u8 as f32;
        g.slice_window = slice_window.max(1e-3);
        g.slice_level = slice_level;
        g.oit_frame = self.oit_frame;
        g.view_fwd = [view_fwd.x, view_fwd.y, view_fwd.z, 0.0];
        g.ribbon = self.ribbon_params;
        queue.write_buffer(&self.slab_uni, 0, bytemuck::bytes_of(&g));
    }

    fn encode_scene(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        size: [u32; 2],
        plane_opaque: [bool; 3],
    ) {
        self.compositor.ensure_targets(device, size);
        let Some(targets) = self.compositor.targets() else {
            return;
        };
        {
            let mut pass = begin_opaque_pass(encoder, targets);
            pass.set_bind_group(0, &self.uni_bind, &[]);
            self.draw_opaque(&mut pass, plane_opaque);
        }
        let mut pass = begin_accumulation_pass(encoder, targets);
        if !self.atlas_indices.is_empty() {
            pass.set_bind_group(0, &self.uni_bind, &[]);
            pass.set_pipeline(&self.atlas_pipeline);
            pass.set_vertex_buffer(0, self.mesh_vb.slice(..));
            pass.set_index_buffer(self.mesh_ib.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(self.atlas_indices.clone(), 0, 0..1);
        }
        if !self.inst_draws_dimmed.is_empty() {
            pass.set_bind_group(0, &self.uni_bind, &[]);
            self.draw_instances(&mut pass, &self.inst_oit_pipeline, &self.inst_draws_dimmed);
        }
    }

    fn draw_instances(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pipeline: &wgpu::RenderPipeline,
        draws: &[InstDraw],
    ) {
        pass.set_pipeline(pipeline);
        pass.set_vertex_buffer(0, self.inst_mesh_vb.slice(..));
        pass.set_vertex_buffer(1, self.inst_vb.slice(..));
        pass.set_index_buffer(self.inst_mesh_ib.slice(..), wgpu::IndexFormat::Uint32);
        for d in draws {
            pass.draw_indexed(
                d.range.first_index..d.range.first_index + d.range.index_count,
                d.range.base_vertex,
                d.instances.clone(),
            );
        }
    }

    fn draw_opaque(&self, pass: &mut wgpu::RenderPass<'_>, plane_opaque: [bool; 3]) {
        if !self.electrode_draw_ranges.is_empty() {
            pass.set_pipeline(&self.electrode_pipeline);
            pass.set_vertex_buffer(0, self.mesh_vb.slice(..));
            pass.set_index_buffer(self.mesh_ib.slice(..), wgpu::IndexFormat::Uint32);
            for &(first, cnt) in &self.electrode_draw_ranges {
                pass.draw_indexed(first..first + cnt, 0, 0..1);
            }
        }

        if !self.inst_draws_opaque.is_empty() {
            self.draw_instances(pass, &self.inst_pipeline, &self.inst_draws_opaque);
        }

        if self.line_vert_count > 0 {
            pass.set_pipeline(&self.line_pipeline);
            pass.set_vertex_buffer(0, self.line_vb.slice(..));
            pass.draw(0..self.line_vert_count, 0..1);
        }

        if self.fibre_used > 0 {
            pass.set_pipeline(&self.fibre_pipeline);
            for (buffer, count, _) in &self.fibre_chunks[..self.fibre_used] {
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..6, 0..*count);
            }
        }

        let stride = std::mem::size_of::<QuadVert>() as wgpu::BufferAddress;
        let plane_draw = [
            (&self.slice_axial.bind, self.quad_ranges[0], plane_opaque[0]),
            (
                &self.slice_coronal.bind,
                self.quad_ranges[1],
                plane_opaque[1],
            ),
            (
                &self.slice_sagittal.bind,
                self.quad_ranges[2],
                plane_opaque[2],
            ),
        ];
        for (bind, range, opaque) in plane_draw {
            let base = range[0];
            let cnt = range[1];
            if cnt == 0 {
                continue;
            }
            pass.set_pipeline(if opaque {
                &self.quad_pipeline
            } else {
                &self.quad_pipeline_overlay
            });
            pass.set_bind_group(1, bind, &[]);
            let off = base as wgpu::BufferAddress * stride;
            pass.set_vertex_buffer(
                0,
                self.quad_vb
                    .slice(off..off + cnt as wgpu::BufferAddress * stride),
            );
            pass.draw(0..cnt, 0..1);
        }

        if self.overlay_on.iter().any(|&on| on) {
            pass.set_pipeline(&self.quad_rgba_pipeline);
            for (p, slot) in self.overlay_slots.iter().enumerate() {
                let [base, cnt] = self.quad_ranges[p];
                if !self.overlay_on[p] || cnt == 0 {
                    continue;
                }
                pass.set_bind_group(1, &slot.bind, &[]);
                let off = base as wgpu::BufferAddress * stride;
                pass.set_vertex_buffer(
                    0,
                    self.quad_vb
                        .slice(off..off + cnt as wgpu::BufferAddress * stride),
                );
                pass.draw(0..cnt, 0..1);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GeometryKey {
    payload_id: u64,
    atlas_generation: u64,
    vta_generation: u64,
    group_generation: u64,
    map_built: u64,
    show_vta: bool,
    atlas_overlay_on: bool,
    active_atlas_group: usize,
    show_atlas: Vec<bool>,
    atlas_rgb: Vec<[u32; 3]>,
    atlas_alpha: Vec<u32>,
    atlas_opacity: u32,
    show_electrodes: bool,
}

impl GeometryKey {
    fn new(sh: &SceneShared, payload: &Scene3DPayload) -> Self {
        Self {
            payload_id: payload.id,
            atlas_generation: payload.atlas_generation(),
            vta_generation: payload.vta_generation.load(AOrd::Acquire),
            group_generation: payload.group_generation.load(AOrd::Acquire),
            map_built: payload.map_built_generation(),
            show_vta: sh.show_vta,
            atlas_overlay_on: sh.atlas_overlay_on,
            active_atlas_group: sh.active_atlas_group,
            show_atlas: sh.show_atlas.clone(),
            atlas_rgb: sh.atlas_rgb.iter().map(|c| c.map(f32::to_bits)).collect(),
            atlas_alpha: sh.atlas_alpha.iter().map(|a| a.to_bits()).collect(),
            atlas_opacity: sh.atlas_opacity.to_bits(),
            show_electrodes: sh.show_electrodes,
        }
    }
}

fn manifest_vta_surfaces(m: &SceneManifestV1) -> Vec<VtaSurface> {
    m.vta
        .iter()
        .filter_map(|entry| match crate::vta_mesh::mesh_vta_nifti(&entry.path) {
            Ok(mesh) => Some(VtaSurface {
                side: entry.side.clone(),
                label: entry.label.clone(),
                mesh,
            }),
            Err(e) => {
                eprintln!("dbsloc: warning: scene: {}: {e}", entry.path.display());
                None
            }
        })
        .collect()
}

fn append_indexed_mesh(
    verts: &mut Vec<MeshVert>,
    indices: &mut Vec<u32>,
    mesh: &TriMeshScene,
    colour: [f32; 4],
) -> Option<(u32, u32)> {
    let base = u32::try_from(verts.len()).ok()?;
    let n = mesh.positions.len();
    let first = u32::try_from(indices.len()).ok()?;
    for tri in mesh.indices.as_chunks::<3>().0 {
        if tri.iter().all(|&ix| (ix as usize) < n) {
            indices.extend(tri.iter().map(|&ix| base + ix));
        }
    }
    let count = u32::try_from(indices.len()).ok()? - first;
    if count == 0 {
        return None;
    }
    verts.extend(mesh.positions.iter().enumerate().map(|(i, p)| {
        MeshVert {
            pos: p.to_array(),
            n: mesh
                .normals
                .get(i)
                .copied()
                .unwrap_or(Vec3::new(0.0, 1.0, 0.0))
                .to_array(),
            c: colour,
        }
    }));
    Some((first, count))
}

struct ScenePaintCallback {
    payload: Arc<Scene3DPayload>,
    aspect: f32,
    rect: egui::Rect,
}

fn viewport_size_px(
    rect: egui::Rect,
    screen: &egui_wgpu::ScreenDescriptor,
    max_dim: u32,
) -> [u32; 2] {
    let v = egui::epaint::ViewportInPixels::from_points(
        &rect,
        screen.pixels_per_point,
        screen.size_in_pixels,
    );
    let clamp = |n: i32| u32::try_from(n).unwrap_or(0).min(max_dim);
    [clamp(v.width_px), clamp(v.height_px)]
}

impl egui_wgpu::CallbackTrait for ScenePaintCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(r) = resources.get_mut::<SceneWgpuRenderer>() else {
            return Vec::new();
        };
        let size = viewport_size_px(self.rect, screen, device.limits().max_texture_dimension_2d);
        if size[0] == 0 || size[1] == 0 {
            return Vec::new();
        }
        let rev = self
            .payload
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .revision;
        if rev != r.last_rev
            || r.geometry_key
                .as_ref()
                .is_none_or(|k| k.payload_id != self.payload.id)
        {
            r.rebuild_geometry(device, queue, &self.payload);
        }
        let sh = self
            .payload
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let key = RenderKey {
            payload_id: self.payload.id,
            geometry_rev: r.last_rev,
            camera_rev: sh.revision,
            size,
            aspect_bits: self.aspect.to_bits(),
        };
        if r.rendered == Some(key) {
            return Vec::new();
        }
        let (vp, eye) = orbit_vp(&sh, self.aspect);
        let view_fwd = view_forward(&sh);
        let (slice_window, slice_level) = (sh.slice_window, sh.slice_level);
        let plane_opaque = [
            sh.axial_opacity >= 0.999_f32,
            sh.coronal_opacity >= 0.999_f32,
            sh.sagittal_opacity >= 0.999_f32,
        ];
        drop(sh);
        let atlas_unlit = self
            .payload
            .atlas_manifest
            .as_ref()
            .is_some_and(|m| m.atlas_unlit);
        let atlas_flat_normals = self
            .payload
            .atlas_manifest
            .as_ref()
            .is_some_and(|m| m.atlas_flat_normals);
        let ribbon_width = self
            .payload
            .with_layers(|l| l.fibre_width_px.clamp(1.0, 6.0));
        r.ribbon_params = [
            size[0] as f32,
            size[1] as f32,
            ribbon_width * screen.pixels_per_point,
            0.0,
        ];
        r.write_globals(
            queue,
            vp,
            eye,
            view_fwd,
            atlas_unlit,
            atlas_flat_normals,
            slice_window,
            slice_level,
        );
        r.encode_scene(device, encoder, size, plane_opaque);
        r.rendered = Some(key);
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(r) = resources.get::<SceneWgpuRenderer>() else {
            return;
        };
        r.compositor.composite(render_pass);
    }
}

#[derive(Clone, Debug)]
pub struct HostButton {
    pub id: &'static str,
    pub label: String,
    pub hover: String,
    pub primary: bool,
}

pub struct Scene3DEframeApp {
    pub title: String,
    host_buttons: Vec<HostButton>,
    host_clicked: Option<&'static str>,
    pub payload: Arc<Scene3DPayload>,
    gpu_inited: bool,
    last_scene_rect_points: Option<egui::Rect>,
    autosave_done: bool,
    /// When Some, the save-scene dialog is open with the given path buffer.
    save_path_edit: Option<String>,
    /// Structure the ↑ / ↓ keys act on, and which the list highlights.
    focused_structure: usize,
    /// Result of the most recent save, shown next to the Save button.
    save_status: Option<Result<PathBuf, String>>,
    reveal_error: Option<String>,
    /// Atlas selection waiting for the atlas to finish loading.
    pending_atlas_restore: Option<SceneViewState>,
    /// Pending result of the off-thread screenshot crop + encode.
    save_result_rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    /// The atlas error window is dismissible; the message itself is kept.
    atlas_error_dismissed: bool,
    /// `DBSLOC_SCENE3D_AUTOSAVE_PNG`, resolved once at construction.
    autosave_png: Option<PathBuf>,
    /// The navigation / shortcut help window; deliberately not persisted.
    help_open: bool,
    navigation: NavigationScheme,
    /// The window draws on a software rasteriser (no graphics driver): the atlas starts hidden.
    software_rendering: bool,
    software_defaults_applied: bool,
    /// The software default hid the atlas and the user has not touched the switch since; the
    /// persisted state keeps the atlas on, so a later session on a graphics card shows it.
    atlas_hidden_by_software: bool,
    keyboard_focus: bool,
    backdrop: BackdropUi,
    /// Entry of the preset menu last applied to `preset_group`; `None` after a manual change.
    preset_choice: Option<usize>,
    preset_group: Option<usize>,
    /// Label set used for structure names; `None` = file names.
    atlas_label_set: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct BackdropChoice {
    pub id: String,
    pub label: String,
    pub path: Option<PathBuf>,
    pub kind: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackdropRequest {
    Install(String),
    ChooseFile,
}

const TEMPLATE_BACKDROP_ID: &str = "template-t1";

type BackdropLoadResult = Result<TemplateVolume, String>;

struct BackdropLoad {
    id: String,
    rx: mpsc::Receiver<BackdropLoadResult>,
    stale: bool,
}

struct BackdropUi {
    choices: Vec<BackdropChoice>,
    allow_choose_file: bool,
    current_id: String,
    request: Option<BackdropRequest>,
    pending_id: Option<String>,
    queued: Option<(String, PathBuf)>,
    load: Option<BackdropLoad>,
    error: Option<String>,
    warning: Option<String>,
}

enum BackdropPick {
    Id(String),
    ChooseFile,
}

fn template_backdrop_choice(space: &str) -> BackdropChoice {
    let label = if space.is_empty() {
        "T1w (template)".to_string()
    } else {
        format!("{space} T1w (template)")
    };
    BackdropChoice {
        id: TEMPLATE_BACKDROP_ID.to_string(),
        label,
        path: None,
        kind: "template".to_string(),
        detail: None,
    }
}

fn manifest_backdrop_choices(payload: &Scene3DPayload) -> Vec<BackdropChoice> {
    let mut out: Vec<BackdropChoice> = payload
        .manifest_backdrops
        .iter()
        .map(|b| BackdropChoice {
            id: b.id.clone(),
            label: b.label.clone(),
            path: Some(b.path.clone()),
            kind: b.kind.clone(),
            detail: None,
        })
        .collect();
    if !out.iter().any(|c| c.id == TEMPLATE_BACKDROP_ID) {
        out.insert(0, template_backdrop_choice(&payload.space));
    }
    out
}

impl BackdropUi {
    fn new(payload: &Scene3DPayload) -> Self {
        Self::with_choices(manifest_backdrop_choices(payload))
    }

    fn with_choices(choices: Vec<BackdropChoice>) -> Self {
        Self {
            choices,
            allow_choose_file: false,
            current_id: TEMPLATE_BACKDROP_ID.to_string(),
            request: None,
            pending_id: None,
            queued: None,
            load: None,
            error: None,
            warning: None,
        }
    }
}

impl BackdropUi {
    fn choose(&mut self, id: &str, path: PathBuf) {
        self.error = None;
        if id == self.current_id {
            self.queued = None;
            if let Some(load) = self.load.as_mut() {
                load.stale = true;
            }
            return;
        }
        if let Some(load) = self.load.as_mut() {
            if load.id == id {
                load.stale = false;
                self.queued = None;
                return;
            }
        }
        self.queued = Some((id.to_string(), path));
    }

    fn choose_template(&mut self) {
        self.queued = None;
        self.error = None;
        self.warning = None;
        self.current_id = TEMPLATE_BACKDROP_ID.to_string();
        if let Some(load) = self.load.as_mut() {
            load.stale = true;
        }
    }

    fn next_to_spawn(&mut self) -> Option<(String, PathBuf)> {
        if self.load.is_some() {
            return None;
        }
        self.queued.take()
    }

    fn accept_result(&mut self) -> Option<String> {
        let load = self.load.take()?;
        (!load.stale && self.queued.is_none()).then_some(load.id)
    }

    fn loading(&self) -> bool {
        self.load.is_some() || self.queued.is_some()
    }

    fn saved_id(&self) -> String {
        self.pending_id
            .clone()
            .or_else(|| self.queued.as_ref().map(|q| q.0.clone()))
            .or_else(|| {
                self.load
                    .as_ref()
                    .filter(|l| !l.stale)
                    .map(|l| l.id.clone())
            })
            .unwrap_or_else(|| self.current_id.clone())
    }
}

/// What the persisted view state records for the atlas switch: a hide applied for software
/// rendering is session-only, the user's own choice is kept.
fn persisted_atlas_overlay(live: bool, hidden_by_software: bool) -> bool {
    live || hidden_by_software
}

impl Scene3DEframeApp {
    pub fn navigation(&self) -> NavigationScheme {
        self.navigation
    }

    pub fn set_navigation(&mut self, scheme: NavigationScheme) {
        self.navigation = scheme;
    }

    pub fn new(title: String, payload: Scene3DPayload) -> Self {
        let backdrop = BackdropUi::new(&payload);
        Self {
            title,
            payload: Arc::new(payload),
            gpu_inited: false,
            last_scene_rect_points: None,
            autosave_done: false,
            save_path_edit: None,
            focused_structure: 0,
            save_status: None,
            reveal_error: None,
            host_buttons: Vec::new(),
            host_clicked: None,
            pending_atlas_restore: None,
            save_result_rx: None,
            atlas_error_dismissed: false,
            help_open: false,
            navigation: NavigationScheme::default(),
            autosave_png: std::env::var_os("DBSLOC_SCENE3D_AUTOSAVE_PNG")
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty()),
            software_rendering: false,
            software_defaults_applied: false,
            atlas_hidden_by_software: false,
            keyboard_focus: true,
            backdrop,
            preset_choice: None,
            preset_group: None,
            atlas_label_set: None,
        }
    }

    /// The window draws on a software rasteriser, so the atlas was hidden at start.
    pub fn software_rendering(&self) -> bool {
        self.software_rendering
    }

    /// The atlas is off only because the window draws on a software rasteriser.
    pub fn atlas_hidden_by_software(&self) -> bool {
        self.atlas_hidden_by_software
    }

    /// Same as [`eframe::App::ui`], for embedding inside another `eframe::App` (e.g. `dbsloc-app`).
    pub fn show_embedded(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.show_embedded_with(ui, frame, Vec::new());
    }

    pub fn show_embedded_with(
        &mut self,
        ui: &mut egui::Ui,
        frame: &mut eframe::Frame,
        host_buttons: Vec<HostButton>,
    ) -> Option<&'static str> {
        self.host_buttons = host_buttons;
        self.host_clicked = None;
        eframe::App::ui(self, ui, frame);
        self.host_buttons.clear();
        self.host_clicked.take()
    }

    /// Snapshot of the camera, slices and atlas selection, for persistence. A hide of the
    /// atlas applied for software rendering is not persisted.
    pub fn view_state(&self) -> SceneViewState {
        let mut state = self
            .payload
            .shared_clone()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot();
        state.backdrop_id = Some(self.backdrop.saved_id());
        (state.fibre_width_px, state.max_fibres) = self
            .payload
            .with_layers(|l| (l.fibre_width_px, l.max_fibres));
        state.atlas_overlay_on =
            persisted_atlas_overlay(state.atlas_overlay_on, self.atlas_hidden_by_software);
        state
    }

    /// Apply a previously saved [`SceneViewState`]. The atlas part is deferred
    /// until the atlas meshes finish loading, so it is never clobbered by the
    /// default-visibility resync.
    ///
    pub fn restore_view_state(&mut self, state: &SceneViewState) {
        let backdrop = self.payload.backdrop_volume();
        {
            let shared = self.payload.shared_clone();
            let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
            sh.restore(state);
            sh.restore_slices(state, &backdrop);
        }
        let changed = self.payload.with_layers(|l| {
            let width = state.fibre_width_px.clamp(1.0, 6.0);
            let max = state
                .max_fibres
                .clamp(1, crate::scene_layers::MAX_FIBRES_LIMIT);
            let changed = l.max_fibres != max;
            l.fibre_width_px = width;
            l.max_fibres = max;
            changed
        });
        if changed {
            self.payload.touch_fibres();
        } else {
            self.payload.bump_shared();
        }
        self.backdrop.pending_id = state.backdrop_id.clone();
        self.apply_pending_backdrop();
    }

    /// Same as [`Self::restore_view_state`], but also re-applies the atlas
    /// selection once loading completes.
    pub fn restore_view_state_deferred(&mut self, state: &SceneViewState) {
        self.restore_view_state(state);
        let mut pending = state.clone();
        pending.slice_convention = 0;
        self.pending_atlas_restore = Some(pending);
    }

    pub fn current_backdrop_id(&self) -> &str {
        &self.backdrop.current_id
    }

    pub fn set_backdrop_choices(&mut self, choices: Vec<BackdropChoice>, allow_choose_file: bool) {
        let mut choices = choices;
        if !choices.iter().any(|c| c.id == TEMPLATE_BACKDROP_ID) {
            choices.insert(0, template_backdrop_choice(&self.payload.space));
        }
        self.backdrop.choices = choices;
        self.backdrop.allow_choose_file = allow_choose_file;
        let current = self.backdrop.current_id.clone();
        if !self.backdrop.choices.iter().any(|c| c.id == current) {
            self.select_backdrop(TEMPLATE_BACKDROP_ID);
        }
        self.apply_pending_backdrop();
    }

    /// Ids of the atlas sets the manifest lists, in combo order.
    pub fn atlas_ids(&self) -> Vec<String> {
        self.payload.atlas_ids.clone()
    }

    /// Id of the atlas set shown, or asked for and not yet resolved.
    pub fn current_atlas_id(&self) -> Option<String> {
        self.payload
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .atlas_id
            .clone()
    }

    /// Show the atlas set `id`; `false` when the manifest does not list it.
    pub fn select_atlas(&mut self, id: &str) -> bool {
        if !self.payload.atlas_ids.iter().any(|a| a == id) {
            return false;
        }
        let mut sh = self
            .payload
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        sh.atlas_id = Some(id.to_string());
        let groups = self
            .payload
            .atlas_groups
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        resolve_active_atlas(&mut sh, &groups);
        drop(groups);
        sh.bump();
        true
    }

    pub fn backdrop_ids(&self) -> Vec<String> {
        self.backdrop.choices.iter().map(|c| c.id.clone()).collect()
    }

    pub fn select_backdrop(&mut self, id: &str) -> bool {
        let Some(choice) = self.backdrop.choices.iter().find(|c| c.id == id).cloned() else {
            return false;
        };
        if id == TEMPLATE_BACKDROP_ID {
            let was_template = self.backdrop.current_id == TEMPLATE_BACKDROP_ID;
            self.backdrop.choose_template();
            if !was_template {
                self.payload.set_backdrop(Arc::clone(&self.payload.vol));
            }
            return true;
        }
        let Some(path) = choice.path else {
            return false;
        };
        self.backdrop.choose(id, path);
        true
    }

    pub fn take_backdrop_request(&mut self) -> Option<BackdropRequest> {
        self.backdrop.request.take()
    }

    fn apply_pending_backdrop(&mut self) {
        let Some(id) = self.backdrop.pending_id.clone() else {
            return;
        };
        if !self.backdrop.choices.iter().any(|c| c.id == id) {
            return;
        }
        self.backdrop.pending_id = None;
        if id != self.backdrop.current_id {
            self.select_backdrop(&id);
        }
    }

    fn start_next_backdrop_load(&mut self, ctx: &egui::Context) {
        let Some((id, path)) = self.backdrop.next_to_spawn() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        spawn_worker(move || {
            let res = load_backdrop_volume(&path);
            let _ = tx.send(res);
            repaint.request_repaint();
        });
        self.backdrop.load = Some(BackdropLoad {
            id,
            rx,
            stale: false,
        });
    }

    fn poll_backdrop_load(&mut self, ctx: &egui::Context) {
        self.start_next_backdrop_load(ctx);
        let Some(load) = self.backdrop.load.as_ref() else {
            return;
        };
        let res = match load.rx.try_recv() {
            Ok(res) => res,
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("The backdrop loader stopped unexpectedly.".to_string())
            }
        };
        let accepted = self.backdrop.accept_result();
        if let Some(id) = accepted {
            match res {
                Ok(vol) => {
                    let overlaps = world_bounds_overlap(
                        template_world_bounds(&vol),
                        template_world_bounds(&self.payload.vol),
                    );
                    self.backdrop.warning = (!overlaps).then(|| {
                        let space = if self.payload.space.is_empty() {
                            "the template space"
                        } else {
                            self.payload.space.as_str()
                        };
                        format!("This image does not overlap the template; is it in {space}?")
                    });
                    self.backdrop.error = None;
                    self.backdrop.current_id = id;
                    self.payload.set_backdrop(Arc::new(vol));
                }
                Err(e) => {
                    self.backdrop.error = Some(e);
                }
            }
        }
        self.start_next_backdrop_load(ctx);
    }

    fn backdrop_controls(state: &BackdropUi, ui: &mut egui::Ui) -> Option<BackdropPick> {
        let mut pick = None;
        let loading = state.loading();
        let label_id = ui.label("Backdrop").id;
        let selected = state
            .choices
            .iter()
            .find(|c| c.id == state.current_id)
            .map(|c| c.label.clone())
            .unwrap_or_else(|| state.current_id.clone());
        let combo = egui::ComboBox::from_id_salt("dbsloc_backdrop_combo")
            .width(ui.available_width())
            .truncate()
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for c in &state.choices {
                    let text = match &c.detail {
                        Some(d) => format!("{} \u{2014} {d}", c.label),
                        None => c.label.clone(),
                    };
                    let rich = if c.path.is_none() && c.id != TEMPLATE_BACKDROP_ID {
                        egui::RichText::new(text).weak()
                    } else {
                        egui::RichText::new(text)
                    };
                    if ui
                        .selectable_label(c.id == state.current_id, rich)
                        .clicked()
                    {
                        pick = Some(BackdropPick::Id(c.id.clone()));
                    }
                }
                if state.allow_choose_file
                    && ui.selectable_label(false, "Choose file\u{2026}").clicked()
                {
                    pick = Some(BackdropPick::ChooseFile);
                }
            });
        combo
            .response
            .labelled_by(label_id)
            .on_hover_text("The image the three slices show");
        if loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading backdrop\u{2026}");
            });
        }
        if let Some(e) = &state.error {
            ui.add(
                egui::Label::new(egui::RichText::new(e).color(ui.visuals().error_fg_color)).wrap(),
            );
        }
        if let Some(w) = &state.warning {
            ui.add(
                egui::Label::new(egui::RichText::new(w).color(ui.visuals().warn_fg_color)).wrap(),
            );
        }
        pick
    }

    fn handle_backdrop_pick(&mut self, pick: BackdropPick, ctx: &egui::Context) {
        self.backdrop.pending_id = None;
        match pick {
            BackdropPick::ChooseFile => {
                self.backdrop.request = Some(BackdropRequest::ChooseFile);
                ctx.request_repaint();
            }
            BackdropPick::Id(id) => {
                if id == self.backdrop.current_id && !self.backdrop.loading() {
                    return;
                }
                let installed = self
                    .backdrop
                    .choices
                    .iter()
                    .find(|c| c.id == id)
                    .is_some_and(|c| c.path.is_some() || c.id == TEMPLATE_BACKDROP_ID);
                if installed {
                    self.select_backdrop(&id);
                } else {
                    self.backdrop.request = Some(BackdropRequest::Install(id));
                    ctx.request_repaint();
                }
            }
        }
    }

    fn ensure_gpu(&mut self, frame: &eframe::Frame) {
        if self.gpu_inited {
            return;
        }
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let depth_fmt = egui_wgpu::depth_format_from_bits(SCENE_VIEWER_DEPTH_BUFFER_BITS, 0);
        let renderer = SceneWgpuRenderer::new(&rs.device, rs.target_format, depth_fmt);
        rs.renderer.write().callback_resources.insert(renderer);
        self.software_rendering = crate::gpu_setup::is_software(rs.adapter.get_info().device_type);
        self.gpu_inited = true;
    }

    /// On a software rasteriser the translucent atlas costs seconds per frame, so it starts
    /// hidden; applied once, after any deferred view restore, and the user can turn it back on.
    fn apply_software_defaults(&mut self) {
        if !self.gpu_inited
            || !self.software_rendering
            || self.software_defaults_applied
            || self.pending_atlas_restore.is_some()
        {
            return;
        }
        self.software_defaults_applied = true;
        let mut sh = self
            .payload
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if sh.atlas_overlay_on {
            sh.atlas_overlay_on = false;
            sh.bump();
            self.atlas_hidden_by_software = true;
        }
    }
}

impl Scene3DEframeApp {
    /// One text-line height — the layout unit, so the panel tracks font size.
    fn unit(ui: &egui::Ui) -> f32 {
        ui.text_style_height(&egui::TextStyle::Body)
    }

    /// The viewer is embedded in the application window, so it must draw from
    /// the same palette — see `crate::ui_tokens`.
    fn accent(ui: &egui::Ui) -> egui::Color32 {
        crate::ui_tokens::palette_for(ui).accent
    }

    fn muted(ui: &egui::Ui) -> egui::Color32 {
        crate::ui_tokens::palette_for(ui).text_muted
    }

    fn warn_color(ui: &egui::Ui) -> egui::Color32 {
        crate::ui_tokens::palette_for(ui).warn
    }

    fn ok_color(ui: &egui::Ui) -> egui::Color32 {
        crate::ui_tokens::palette_for(ui).ok
    }

    fn err_color(ui: &egui::Ui) -> egui::Color32 {
        crate::ui_tokens::palette_for(ui).err
    }

    /// Default save target: next to the scene manifest when we know it, else the
    /// user's home directory. Never a bare relative name into an unknown folder.
    fn default_save_path(&self) -> String {
        let stem: String = self
            .title
            .rsplit('—')
            .next()
            .unwrap_or("scene")
            .trim()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let dir = dirs_home().unwrap_or_else(|| PathBuf::from("."));
        dir.join(format!("{stem}_scene3d.png"))
            .to_string_lossy()
            .into_owned()
    }

    /// A 0..1 value edited as a percentage. Each slider carries its own name so
    /// AccessKit reports "Axial opacity", never five identical "Opacity".
    fn opacity_slider(
        ui: &mut egui::Ui,
        row: RowLabel,
        name: &str,
        value: &mut f32,
        hint: &str,
    ) -> bool {
        let current = f64::from(*value);
        labelled_slider(
            ui,
            row.named(name, current),
            Self::opacity_slider_widget(value),
            "100%",
            hint,
        )
        .changed()
    }

    fn opacity_slider_widget(value: &mut f32) -> egui::Slider<'_> {
        egui::Slider::new(value, 0.0..=1.0)
            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
            .custom_parser(|s| {
                s.trim()
                    .trim_end_matches('%')
                    .parse::<f64>()
                    .ok()
                    .map(|v| v / 100.0)
            })
    }

    /// Keyboard shortcuts for the scene, per the project UX rules. Plain keys are
    /// only consumed when no text field has focus, so typing a path is unaffected.
    fn handle_scene_shortcuts(
        &mut self,
        ctx: &egui::Context,
        n_structures: usize,
        active: bool,
    ) -> SceneKeyAction {
        let mut act = SceneKeyAction::default();
        if self.help_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            act.toggle_help = true;
        }
        if !active {
            return act;
        }
        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::S,
            ))
        }) {
            act.save = true;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F1)) {
            act.toggle_help = true;
        }
        if ctx.text_edit_focused() || self.save_path_edit.is_some() {
            return act;
        }
        let plain = egui::Modifiers::NONE;
        let shift = egui::Modifiers::SHIFT;
        ctx.input_mut(|i| {
            if i.consume_key(shift, egui::Key::ArrowLeft) {
                act.pan_right -= 1;
            }
            if i.consume_key(shift, egui::Key::ArrowRight) {
                act.pan_right += 1;
            }
            if i.consume_key(shift, egui::Key::ArrowUp) {
                act.pan_up += 1;
            }
            if i.consume_key(shift, egui::Key::ArrowDown) {
                act.pan_up -= 1;
            }
            if i.consume_key(plain, egui::Key::R) {
                act.reset_view = true;
            }
            if i.consume_key(plain, egui::Key::Num1) {
                act.toggle_axial = true;
            }
            if i.consume_key(plain, egui::Key::Num2) {
                act.toggle_coronal = true;
            }
            if i.consume_key(plain, egui::Key::Num3) {
                act.toggle_sagittal = true;
            }
            if i.consume_key(plain, egui::Key::ArrowDown) {
                act.focus_delta = 1;
            }
            if i.consume_key(plain, egui::Key::ArrowUp) {
                act.focus_delta = -1;
            }
            if i.consume_key(plain, egui::Key::Space) {
                act.toggle_focused = true;
            }
        });
        if n_structures == 0 {
            act.focus_delta = 0;
            act.toggle_focused = false;
        }
        act
    }

    /// Project a world point into the viewport, if it is in front of the camera.
    fn project(rect: egui::Rect, vp: Mat4, p: Vec3) -> Option<egui::Pos2> {
        let clip = vp * glam::Vec4::new(p.x, p.y, p.z, 1.0);
        if clip.w <= 1e-6 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        if !(-1.2..=1.2).contains(&ndc.x) || !(-1.2..=1.2).contains(&ndc.y) {
            return None;
        }
        Some(egui::pos2(
            rect.left() + (ndc.x * 0.5 + 0.5) * rect.width(),
            rect.top() + (1.0 - (ndc.y * 0.5 + 0.5)) * rect.height(),
        ))
    }

    /// Mark and number the reconstructed **contacts** (Lead-DBS `eltext` parity).
    ///
    /// The index is the contact's position in the reconstruction's own list, not
    /// the manufacturer's contact numbering — the scene manifest does not carry
    /// the electrode model, so claiming device numbering here would be a guess.
    fn paint_contact_labels(&self, ui: &egui::Ui, rect: egui::Rect, vp: Mat4) {
        let painter = ui.painter_at(rect);
        let color = ui.visuals().strong_text_color();
        let dark = ui.visuals().dark_mode;
        let halo = if dark {
            egui::Color32::from_black_alpha(190)
        } else {
            egui::Color32::from_white_alpha(210)
        };
        let marker = egui::Color32::from_rgb(0x56, 0xb4, 0xe9);
        let unit = Self::unit(ui);
        let font = egui::FontId::monospace(unit * 0.8);
        let mut placed: Vec<egui::Rect> = Vec::new();
        for (side, pts) in [
            ("L", &self.payload.contacts_l),
            ("R", &self.payload.contacts_r),
        ] {
            for (i, p) in pts.iter().enumerate() {
                let Some(pos) = Self::project(rect, vp, *p) else {
                    continue;
                };
                painter.circle_filled(pos, unit * 0.22, marker);
                painter.circle_stroke(
                    pos,
                    unit * 0.22,
                    egui::Stroke::new(1.0_f32, if dark { halo } else { color }),
                );

                let galley =
                    painter.layout_no_wrap(format!("{side}{}", i + 1), font.clone(), color);
                let candidates = [
                    pos + egui::vec2(unit * 0.4, -unit * 0.35),
                    pos + egui::vec2(-unit * 0.4 - galley.size().x, -unit * 0.35),
                ];
                let Some(at) = candidates.into_iter().find(|at| {
                    let r = egui::Rect::from_min_size(*at, galley.size()).expand(2.0);
                    rect.contains_rect(r) && !placed.iter().any(|q| q.intersects(r))
                }) else {
                    continue;
                };
                let bg = egui::Rect::from_min_size(at, galley.size()).expand(2.0);
                placed.push(bg);
                painter.rect_filled(bg, 3.0, halo);
                painter.galley(at, galley, color);
            }
        }
    }

    /// Anatomical orientation markers and a millimetre scale bar.
    ///
    /// NIfTI defines the `sform` target as RAS+, so world +x/+y/+z are the
    /// patient's right / anterior / superior. Every axis label is projected
    /// through the same view-projection as the scene, so it stays correct under
    /// any camera.
    fn paint_orientation(&self, ui: &egui::Ui, rect: egui::Rect, vp: Mat4, target: Vec3) {
        let painter = ui.painter_at(rect);
        let unit = Self::unit(ui);
        let muted = Self::muted(ui);
        let font = egui::FontId::proportional(unit * 0.85);
        let Some(centre_px) = Self::project(rect, vp, target) else {
            return;
        };
        let probe = 10.0_f32;
        for (label, dir) in [
            ("R", Vec3::X),
            ("L", -Vec3::X),
            ("A", Vec3::Y),
            ("P", -Vec3::Y),
            ("S", Vec3::Z),
            ("I", -Vec3::Z),
        ] {
            let Some(p) = Self::project(rect, vp, target + dir * probe) else {
                continue;
            };
            let d = p - centre_px;
            if d.length() < 2.0 {
                continue;
            }
            let inset = unit * 1.3;
            let usable = egui::vec2(
                (rect.width() * 0.5 - inset).max(1.0),
                (rect.height() * 0.5 - inset).max(1.0),
            );
            let scale = (usable.x / d.x.abs()).min(usable.y / d.y.abs());
            let at = rect.center() + d * scale;
            let galley = painter.layout_no_wrap(label.to_owned(), font.clone(), muted);
            let bg = egui::Rect::from_center_size(at, galley.size()).expand(3.0);
            painter.rect_filled(
                bg,
                3.0,
                if ui.visuals().dark_mode {
                    egui::Color32::from_black_alpha(150)
                } else {
                    egui::Color32::from_white_alpha(180)
                },
            );
            painter.galley(bg.min + egui::vec2(3.0, 3.0), galley, muted);
        }

        let right = {
            let a = Self::project(rect, vp, target);
            let b = Self::project(rect, vp, target + Vec3::X * 10.0);
            match (a, b) {
                (Some(a), Some(b)) => (b - a).length() / 10.0,
                _ => 0.0,
            }
        };
        if right <= 1e-4 {
            return;
        }
        let mm = [1.0_f32, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0]
            .into_iter()
            .find(|m| m * right >= 60.0)
            .unwrap_or(100.0);
        let px = mm * right;
        let y = rect.bottom() - unit * 1.4;
        let x0 = rect.left() + unit * 1.4;
        let chip = egui::Rect::from_min_max(
            egui::pos2(x0 - unit * 0.4, y - unit * 1.5),
            egui::pos2(x0 + px + unit * 0.4, y + unit * 0.5),
        );
        painter.rect_filled(
            chip,
            4.0,
            if ui.visuals().dark_mode {
                egui::Color32::from_black_alpha(150)
            } else {
                egui::Color32::from_white_alpha(180)
            },
        );
        let stroke = egui::Stroke::new(2.0_f32, muted);
        painter.line_segment([egui::pos2(x0, y), egui::pos2(x0 + px, y)], stroke);
        painter.line_segment(
            [
                egui::pos2(x0, y - unit * 0.25),
                egui::pos2(x0, y + unit * 0.25),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(x0 + px, y - unit * 0.25),
                egui::pos2(x0 + px, y + unit * 0.25),
            ],
            stroke,
        );
        painter.text(
            egui::pos2(x0 + px * 0.5, y - unit * 0.5),
            egui::Align2::CENTER_BOTTOM,
            format!("{mm:.0} mm"),
            font,
            muted,
        );
    }
}

/// Home directory, without pulling in another dependency.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// What the scene keyboard shortcuts asked for this frame.
#[derive(Default, Clone, Copy)]
struct SceneKeyAction {
    save: bool,
    reset_view: bool,
    toggle_axial: bool,
    toggle_coronal: bool,
    toggle_sagittal: bool,
    focus_delta: i32,
    toggle_focused: bool,
    /// Shift+Arrow pan presses this frame: +1 right / up, -1 left / down.
    pan_right: i32,
    pan_up: i32,
    toggle_help: bool,
}

/// One frame's read-only view of the atlas groups, taken under a single lock so
/// the paint path never contends with the background mesh loader.
struct GroupView {
    labels: Vec<String>,
    ids: Vec<String>,
    citations: Vec<Option<String>>,
    names: Vec<String>,
    meshed: Vec<bool>,
    /// Group these names belong to — queued ROI indices are only valid for it.
    group_index: usize,
    info: Arc<AtlasSetInfo>,
}

impl eframe::App for Scene3DEframeApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ensure_gpu(frame);
        self.payload.start_atlas_load_if_needed();
        self.payload.start_group_vta_load();
        if self
            .payload
            .start_layer_builds_if_needed(ctx.input(|i| i.time))
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }

        if !self.autosave_done
            && self.autosave_png.is_some()
            && !self.payload.atlas_loading()
            && !self.backdrop.loading()
            && self.last_scene_rect_points.is_some()
        {
            self.autosave_done = true;
            if let Some(p) = self.autosave_png.clone() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(p)));
            }
        }

        let groups_view = {
            let mut sh = self
                .payload
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let ag = self
                .payload
                .atlas_groups
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if resolve_active_atlas(&mut sh, &ag) {
                sh.bump();
            }
            let requested_group = sh.active_atlas_group;
            drop(sh);
            let labels: Vec<String> = ag.iter().map(|g| g.label.clone()).collect();
            let ids: Vec<String> = ag.iter().map(|g| g.id().to_string()).collect();
            let citations: Vec<Option<String>> =
                ag.iter().map(|g| g.info.citation_short.clone()).collect();
            let active = requested_group.min(ag.len().saturating_sub(1));
            let (names, meshed, info) = ag
                .get(active)
                .map(|g| {
                    (
                        g.meshes.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
                        g.meshes
                            .iter()
                            .map(|m| m.mesh.is_some())
                            .collect::<Vec<_>>(),
                        Arc::clone(&g.info),
                    )
                })
                .unwrap_or_default();
            GroupView {
                labels,
                ids,
                citations,
                names,
                meshed,
                group_index: active,
                info,
            }
        };
        let n_groups = groups_view.labels.len();
        let n_structures = groups_view.names.len();
        self.focused_structure = self.focused_structure.min(n_structures.saturating_sub(1));

        if let Some(pending) = self.pending_atlas_restore.clone() {
            if n_structures > 0 && pending.show_atlas.len() == n_structures {
                self.payload
                    .shared
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .restore(&pending);
                self.pending_atlas_restore = None;
            } else if !self.payload.busy() {
                self.pending_atlas_restore = None;
            }
        }
        self.apply_software_defaults();
        self.poll_backdrop_load(&ctx);

        let viewer_rect = ui.max_rect();
        let (hovered, pressed_at) = ctx.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer
                    .any_pressed()
                    .then(|| i.pointer.interact_pos())
                    .flatten(),
            )
        });
        if let Some(pos) = pressed_at {
            self.keyboard_focus = viewer_rect.contains(pos);
        }
        let pointer_inside = hovered.is_some_and(|p| viewer_rect.contains(p));
        let keys =
            self.handle_scene_shortcuts(&ctx, n_structures, pointer_inside || self.keyboard_focus);
        if keys.save && self.save_path_edit.is_none() {
            self.save_path_edit = Some(self.default_save_path());
        }
        if keys.toggle_help {
            self.help_open = !self.help_open;
        }

        let unit = Self::unit(ui);
        let accent = Self::accent(ui);
        let muted = Self::muted(ui);
        let warn = Self::warn_color(ui);
        let ok = Self::ok_color(ui);
        let err = Self::err_color(ui);

        egui::Panel::top("scene3d_menu").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let primary_fill = crate::ui_tokens::palette_for(ui).accent_fill;
                for host in std::mem::take(&mut self.host_buttons) {
                    let clicked = if host.primary {
                        ui.add(
                            egui::Button::new(
                                egui::RichText::new(&host.label)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(primary_fill)
                            .stroke(egui::Stroke::NONE),
                        )
                    } else {
                        ui.button(&host.label)
                    }
                    .on_hover_text(&host.hover)
                    .clicked();
                    if clicked {
                        self.host_clicked = Some(host.id);
                    }
                }
                if ui
                    .button(egui::RichText::new("Save scene").strong())
                    .on_hover_text(
                        "Save the rendered scene as an image, without the controls (Ctrl+S)",
                    )
                    .clicked()
                    && self.save_path_edit.is_none()
                {
                    self.save_path_edit = Some(self.default_save_path());
                }
                if ui
                    .button("Help")
                    .on_hover_text("Mouse, touchpad and keyboard controls (F1)")
                    .clicked()
                {
                    self.help_open = !self.help_open;
                }
                match &self.save_status {
                    Some(Ok(p)) => {
                        ui.label(
                            egui::RichText::new(format!("✔ Saved to {}", p.display()))
                                .small()
                                .color(ok),
                        );
                        if ui
                            .button("Copy path")
                            .on_hover_text("Copy the saved image path to the clipboard")
                            .clicked()
                        {
                            ctx.copy_text(p.to_string_lossy().into_owned());
                        }
                        if ui
                            .small_button("Open folder")
                            .on_hover_text(
                                "Open the folder with the saved scene image in the file manager",
                            )
                            .clicked()
                        {
                            let full = std::path::absolute(p).unwrap_or_else(|_| p.clone());
                            self.reveal_error = crate::widgets::reveal_in_file_manager(
                                full.parent().unwrap_or(full.as_path()),
                            )
                            .err();
                        }
                    }
                    Some(Err(e)) => {
                        ui.label(egui::RichText::new(format!("✖ {e}")).small().color(err));
                    }
                    None => {}
                }
                if let Some(e) = &self.reveal_error {
                    ui.label(egui::RichText::new(format!("✖ {e}")).small().color(err));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&self.title).small().color(muted))
                            .truncate()
                            .selectable(false),
                    )
                    .on_hover_text("Subject this scene was reconstructed from");
                });
            });
        });

        if self.help_open {
            let mut open = true;
            egui::Window::new("Scene controls")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
                    ui.label(egui::RichText::new("Mouse & touchpad").strong());
                    navigation_help_grid(ui, self.navigation.help_rows());
                    ui.separator();
                    ui.label(egui::RichText::new("Keyboard").strong());
                    navigation_help_grid(ui, SCENE_KEY_HELP);
                });
            self.help_open = open;
        }

        let mut do_save: Option<PathBuf> = None;
        let mut close_save_dlg = false;
        if let Some(ref mut path_buf) = self.save_path_edit {
            let mut open = true;
            egui::Window::new("Save scene image")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
                    ui.label("Output file (.png or .jpg):");
                    ui.add(
                        egui::TextEdit::singleline(path_buf).desired_width(Self::unit(ui) * 22.0),
                    )
                    .on_hover_text("Full path of the image file to write");
                    let path = Path::new(path_buf.as_str());
                    let ext_ok = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
                        .unwrap_or(false);
                    if !ext_ok {
                        ui.label(
                            egui::RichText::new("⚠ Give the file a .png or .jpg extension.")
                                .small()
                                .color(Self::warn_color(ui)),
                        );
                    } else if path.exists() {
                        ui.label(
                            egui::RichText::new("⚠ This file already exists and will be replaced.")
                                .small()
                                .color(Self::warn_color(ui)),
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(ext_ok, egui::Button::new("Save"))
                            .on_hover_text("Write the image")
                            .clicked()
                        {
                            do_save = Some(PathBuf::from(path_buf.as_str()));
                            close_save_dlg = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close_save_dlg = true;
                        }
                    });
                });
            if !open {
                close_save_dlg = true;
            }
        }
        if close_save_dlg {
            self.save_path_edit = None;
        }
        if let Some(path) = do_save {
            self.save_status = None;
            self.reveal_error = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(path)));
        }

        let shared = self.payload.shared_clone();
        let mut roi_requests: Vec<usize> = Vec::new();
        let mut scroll_to_focus = false;

        let mut sh: SceneShared = shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let vectors_before = (sh.atlas_vectors_group, sh.show_atlas.len());
        let mut changed = false;
        let backdrop_vol = self.payload.backdrop_volume();
        let slice_before = (sh.sagittal_i, sh.coronal_j, sh.axial_k);
        let world_before = sh.slice_world;
        let mut backdrop_pick: Option<BackdropPick> = None;
        egui::Panel::right("ctrl_v3")
            .default_size(unit * 15.0)
            .min_size(unit * 10.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if keys.reset_view {
                            reset_view(&mut sh, &self.payload.view_defaults, &backdrop_vol);
                            changed = true;
                        }
                        if keys.toggle_axial {
                            sh.show_axial = !sh.show_axial;
                            changed = true;
                        }
                        if keys.toggle_coronal {
                            sh.show_coronal = !sh.show_coronal;
                            changed = true;
                        }
                        if keys.toggle_sagittal {
                            sh.show_sagittal = !sh.show_sagittal;
                            changed = true;
                        }
                        if keys.focus_delta != 0 && n_structures > 0 {
                            let next = self.focused_structure as i32 + keys.focus_delta;
                            self.focused_structure =
                                next.clamp(0, n_structures as i32 - 1) as usize;
                            scroll_to_focus = true;
                        }
                        if keys.toggle_focused && self.focused_structure < sh.show_atlas.len() {
                            let i = self.focused_structure;
                            sh.show_atlas[i] = !sh.show_atlas[i];
                            if sh.show_atlas[i] {
                                roi_requests.push(i);
                            }
                            self.preset_choice = None;
                            changed = true;
                            scroll_to_focus = true;
                        }

                        egui::CollapsingHeader::new(
                            egui::RichText::new("Slices")
                                .strong(),
                        )
                        .id_salt("dbsloc_sec_slices_v2")
                        .default_open(true)
                        .show(ui, |ui| {
                            backdrop_pick = Self::backdrop_controls(&self.backdrop, ui);
                            let slice_col =
                                label_column(ui, &[("Index", false), ("Opacity", false)]);
                            changed |= ui
                                .checkbox(&mut sh.show_axial, "Axial slice")
                                .on_hover_text("Show the horizontal template slice (key 1)")
                                .changed();
                            if sh.show_axial {
 indented(ui, |ui| {
                                let max = backdrop_vol.n2.saturating_sub(1);
                                let index_value = sh.axial_k as f64;
changed |= labelled_slider(
    ui,
    RowLabel::new("Index", slice_col).named("Axial index", index_value),
    egui::Slider::new(&mut sh.axial_k, 0..=max),
                                    &max.to_string(),
                                    "Which axial slice of the backdrop to draw",
                                )
                                .changed();
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "Index {} \u{00b7} z {} mm",
                                            sh.axial_k,
                                            format_signed_mm(plane_world_mm(&backdrop_vol, 2, sh.axial_k)),
                                        ))
                                        .weak(),
                                    )
                                    .wrap(),
                                );
                                changed |= Self::opacity_slider(
                                    ui,
                                    RowLabel::new("Opacity", slice_col),
                                    "Axial opacity",
                                    &mut sh.axial_opacity,
                                    "How opaque the axial slice is drawn",
                                );
 });
                            }

                            changed |= ui
                                .checkbox(&mut sh.show_coronal, "Coronal slice")
                                .on_hover_text("Show the front-facing template slice (key 2)")
                                .changed();
                            if sh.show_coronal {
 indented(ui, |ui| {
                                let max = backdrop_vol.n1.saturating_sub(1);
                                let index_value = sh.coronal_j as f64;
changed |= labelled_slider(
    ui,
    RowLabel::new("Index", slice_col).named("Coronal index", index_value),
    egui::Slider::new(&mut sh.coronal_j, 0..=max),
                                    &max.to_string(),
                                    "Which coronal slice of the backdrop to draw",
                                )
                                .changed();
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "Index {} \u{00b7} y {} mm",
                                            sh.coronal_j,
                                            format_signed_mm(plane_world_mm(&backdrop_vol, 1, sh.coronal_j)),
                                        ))
                                        .weak(),
                                    )
                                    .wrap(),
                                );
                                changed |= Self::opacity_slider(
                                    ui,
                                    RowLabel::new("Opacity", slice_col),
                                    "Coronal opacity",
                                    &mut sh.coronal_opacity,
                                    "How opaque the coronal slice is drawn",
                                );
 });
                            }

                            changed |= ui
                                .checkbox(&mut sh.show_sagittal, "Sagittal slice")
                                .on_hover_text("Show the side-facing template slice (key 3)")
                                .changed();
                            if sh.show_sagittal {
 indented(ui, |ui| {
                                let max = backdrop_vol.n0.saturating_sub(1);
                                let index_value = sh.sagittal_i as f64;
changed |= labelled_slider(
    ui,
    RowLabel::new("Index", slice_col).named("Sagittal index", index_value),
    egui::Slider::new(&mut sh.sagittal_i, 0..=max),
                                    &max.to_string(),
                                    "Which sagittal slice of the backdrop to draw",
                                )
                                .changed();
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "Index {} \u{00b7} x {} mm",
                                            sh.sagittal_i,
                                            format_signed_mm(plane_world_mm(&backdrop_vol, 0, sh.sagittal_i)),
                                        ))
                                        .weak(),
                                    )
                                    .wrap(),
                                );
                                changed |= Self::opacity_slider(
                                    ui,
                                    RowLabel::new("Opacity", slice_col),
                                    "Sagittal opacity",
                                    &mut sh.sagittal_opacity,
                                    "How opaque the sagittal slice is drawn",
                                );
 });
                            }
                        });

                        egui::CollapsingHeader::new(
                            egui::RichText::new("Window / level")
                                .strong(),
                        )
                        .id_salt("dbsloc_sec_window_level_v2")
                        .default_open(true)
                        .show(ui, |ui| {
                            let wl_col = label_column(ui, &[("Window", true), ("Level", false)]);
                            changed |= labelled_slider(
                                ui,
                                RowLabel::new("Window", wl_col).info(
                                    "Window is the width of the displayed intensity range; \
                                     level is its centre. Both are fractions of the \
                                     template's normalized intensity.",
                                ),
                                egui::Slider::new(&mut sh.slice_window, 0.05..=1.0)
                                    .fixed_decimals(2),
                                "1.00",
                                "A narrower window gives more contrast",
                            )
                            .changed();
                            changed |= labelled_slider(
                                ui,
                                RowLabel::new("Level", wl_col),
                                egui::Slider::new(&mut sh.slice_level, 0.0..=1.0).fixed_decimals(2),
                                "1.00",
                                "A lower level gives a brighter image",
                            )
                            .changed();
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .button("Full range")
                                    .on_hover_text(
                                        "Window 1.00, level 0.50 — the whole intensity range",
                                    )
                                    .clicked()
                                {
                                    sh.slice_window = 1.0;
                                    sh.slice_level = 0.5;
                                    changed = true;
                                }
                                if ui
                                    .button("Soft tissue")
                                    .on_hover_text(
                                        "Narrow window centred low — brings out grey and \
                                             white matter",
                                    )
                                    .clicked()
                                {
                                    sh.slice_window = 0.45;
                                    sh.slice_level = 0.35;
                                    changed = true;
                                }
                                if ui
                                    .button("Bone / metal")
                                    .on_hover_text(
                                        "Narrow window centred high — brings out dense \
                                             structures",
                                    )
                                    .clicked()
                                {
                                    sh.slice_window = 0.35;
                                    sh.slice_level = 0.80;
                                    changed = true;
                                }
                            });
                        });

                        egui::CollapsingHeader::new(
                            egui::RichText::new("Electrode")
                                .strong(),
                        )
                        .id_salt("dbsloc_sec_electrode_v2")
                        .default_open(true)
                        .show(ui, |ui| {
                            changed |= ui
                                .checkbox(&mut sh.show_electrodes, "Show lead bodies")
                                .on_hover_text(
                                    "Draw each lead as a 3D body: shaft, tip and the contact \
                                     rings or directional segments of its electrode model.",
                                )
                                .changed();
                            changed |= ui
                                .checkbox(&mut sh.show_contact_labels, "Mark and number contacts")
                                .on_hover_text(
                                    "Mark each reconstructed contact and number it by its \
                                         position in the reconstruction's contact list (L1…, \
                                         R1…). That is not the manufacturer's contact \
                                         numbering.",
                                )
                                .changed();
                            let (cl, cr) =
                                (self.payload.contacts_l.len(), self.payload.contacts_r.len());
                            ui.label(
                                egui::RichText::new(if cl + cr == 0 {
                                    "No contact coordinates in this reconstruction".to_owned()
                                } else {
                                    format!("{cl} left / {cr} right contacts")
                                })
                                .small()
                                .color(muted),
                            );
                        });

                        egui::CollapsingHeader::new(
                            egui::RichText::new("Stimulation")
                                .strong(),
                        )
                        .id_salt("dbsloc_sec_vta_v1")
                        .default_open(true)
                        .show(ui, |ui| {
                            changed |= ui
                                .checkbox(&mut sh.show_vta, "Show VTA")
                                .on_hover_text(
                                    "Draw the volume of tissue activated of the computed or                                      previewed stimulation as a red, semi-transparent surface.",
                                )
                                .changed();
                            let loaded = self.payload.vta_labels();
                            ui.label(
                                egui::RichText::new(if loaded.is_empty() {
                                    "No stimulation loaded".to_owned()
                                } else {
                                    let sides: Vec<&str> =
                                        loaded.iter().map(|(side, _)| side.as_str()).collect();
                                    format!("{}: {}", loaded[0].1, sides.join(", "))
                                })
                                .small()
                                .color(muted),
                            );
                        });

                        changed |= layer_controls(ui, &self.payload, muted);
                        changed |= group_controls(ui, &self.payload, muted);

                        egui::CollapsingHeader::new(
                            egui::RichText::new("View")
                                .strong(),
                        )
                        .id_salt("dbsloc_sec_view_v2")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::ComboBox::from_id_salt("dbsloc_projection")
                                .width(ui.available_width())
                                .truncate()
                                .selected_text(sh.projection.label())
                                .show_ui(ui, |ui| {
                                    for p in [Projection::Orthographic, Projection::Perspective] {
                                        if ui
                                            .selectable_value(&mut sh.projection, p, p.label())
                                            .on_hover_text(p.description())
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(sh.projection.description());
                            let view_col = label_column(
                                ui,
                                &[
                                    ("Distance (mm)", false),
                                    ("Elevation (°)", false),
                                    ("Azimuth (°)", false),
                                ],
                            );
                            changed |= labelled_slider(
                                ui,
                                RowLabel::new("Distance (mm)", view_col),
                                egui::Slider::new(&mut sh.distance, 20.0..=2000.0)
                                    .logarithmic(true)
                                    .fixed_decimals(0),
                                "2000",
                                "Distance from the camera to the point it looks at",
                            )
                            .changed();

                            let mut elevation_deg = sh.elevation.to_degrees();
                            if labelled_slider(
                                ui,
                                RowLabel::new("Elevation (°)", view_col),
                                egui::Slider::new(
                                    &mut elevation_deg,
                                    -ELEVATION_MAX_DEG..=ELEVATION_MAX_DEG,
                                )
                                .fixed_decimals(0),
                                "-89",
                                "Camera angle above (positive) or below the horizontal plane",
                            )
                            .changed()
                            {
                                sh.elevation = elevation_deg
                                    .to_radians()
                                    .clamp(-ELEVATION_MAX_RAD, ELEVATION_MAX_RAD);
                                changed = true;
                            }

                            let mut azimuth_deg = sh.azimuth.to_degrees();
                            if labelled_slider(
                                ui,
                                RowLabel::new("Azimuth (°)", view_col),
                                egui::Slider::new(&mut azimuth_deg, -180.0..=180.0)
                                    .fixed_decimals(0),
                                "-180",
                                "Camera angle around the vertical axis: 0° looks from the front, 180° from the back",
                            )
                            .changed()
                            {
                                sh.azimuth = wrap_angle(azimuth_deg.to_radians());
                                changed = true;
                            }

                            if ui
                                .button("⟳ Reset view")
                                .on_hover_text(
                                    "Restore the default camera, slices and contrast (key R)",
                                )
                                .clicked()
                            {
                                reset_view(&mut sh, &self.payload.view_defaults, &backdrop_vol);
                                changed = true;
                            }

                            egui::CollapsingHeader::new("Mouse & touchpad")
                                .id_salt("dbsloc_sec_navigation_v1")
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label("Scheme");
                                        egui::ComboBox::from_id_salt("dbsloc_nav_scheme")
                                            .width(ui.available_width())
                                            .truncate()
                                            .selected_text(self.navigation.label())
                                            .show_ui(ui, |ui| {
                                                for scheme in NavigationScheme::ALL {
                                                    ui.selectable_value(
                                                        &mut self.navigation,
                                                        scheme,
                                                        scheme.label(),
                                                    );
                                                }
                                            })
                                            .response
                                            .on_hover_text(self.navigation.description());
                                    });
                                    navigation_help_grid(ui, self.navigation.help_rows());
                                })
                                .header_response
                                .on_hover_text("How to rotate, pan and zoom the scene (F1)");
                        });

                        egui::CollapsingHeader::new(
                            egui::RichText::new("Atlas")
                                .strong(),
                        )
                        .id_salt("dbsloc_sec_atlas_v2")
                        .default_open(true)
                        .show(ui, |ui| {
                            if self.software_rendering {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(
                                            "Software rendering (no graphics driver): the \
                                             atlas starts hidden because drawing it on the \
                                             processor takes seconds per frame. Turn it on \
                                             below if you need it.",
                                        )
                                        .small()
                                        .color(warn),
                                    )
                                    .wrap(),
                                );
                            }
                            if n_groups == 0 {
                                let failed = self.payload.has_atlas_error();
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(if failed {
                                            "The atlas failed to load — no structures \
                                                 are available. See the message below."
                                        } else {
                                            "No atlas configured — this scene manifest \
                                                 lists no atlas paths."
                                        })
                                        .small()
                                        .color(if failed { warn } else { muted }),
                                    )
                                    .wrap(),
                                );
                                if failed
                                    && ui
                                        .button("Show the atlas error")
                                        .on_hover_text(
                                            "Reopen the message describing why the \
                                                 atlas could not be loaded",
                                        )
                                        .clicked()
                                {
                                    self.atlas_error_dismissed = false;
                                }
                                return;
                            }
                            let toggled = ui
                                .checkbox(&mut sh.atlas_overlay_on, "Show atlas overlay")
                                .on_hover_text("Master switch for every atlas structure")
                                .changed();
                            if toggled {
                                self.atlas_hidden_by_software = false;
                            }
                            changed |= toggled;
                            let atlas_col = label_column(ui, &[("Opacity", false)]);
                            changed |= Self::opacity_slider(
                                ui,
                                RowLabel::new("Opacity", atlas_col),
                                "Atlas opacity",
                                &mut sh.atlas_opacity,
                                "Opacity applied to every atlas structure",
                            );

                            let sel = sh.active_atlas_group.min(n_groups - 1);
                            let mut pick_group: Option<usize> = None;
                            let set_label_id = ui.label("Atlas set").id;
                            let set_hover = match &groups_view.citations[sel] {
                                Some(c) => format!(
                                    "Which atlas the structure list below belongs to: {} ({c})",
                                    groups_view.labels[sel]
                                ),
                                None => format!(
                                    "Which atlas the structure list below belongs to: {}",
                                    groups_view.labels[sel]
                                ),
                            };
                            egui::ComboBox::from_id_salt("dbsloc_atlas_group")
                                .width(ui.available_width())
                                .truncate()
                                .selected_text(groups_view.labels[sel].as_str())
                                .show_ui(ui, |ui| {
                                    for (i, label) in groups_view.labels.iter().enumerate() {
                                        let mut resp =
                                            ui.selectable_label(i == sel, label.as_str());
                                        if let Some(c) = &groups_view.citations[i] {
                                            resp = resp.on_hover_text(c);
                                        }
                                        if resp.clicked() && i != sel {
                                            pick_group = Some(i);
                                        }
                                    }
                                })
                                .response
                                .labelled_by(set_label_id)
                                .on_hover_text(set_hover);
                            if let Some(i) = pick_group {
                                sh.active_atlas_group = i;
                                sh.atlas_id = groups_view.ids.get(i).cloned();
                                changed = true;
                            }
                            if let Some(c) = &groups_view.citations[sel] {
                                let full = groups_view.info.citation_name.clone();
                                let line = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(c.as_str()).small().color(muted),
                                    )
                                    .wrap(),
                                );
                                if let Some(full) = full {
                                    line.on_hover_text(full);
                                }
                            }

                            let listed_group = groups_view.group_index;
                            let info = Arc::clone(&groups_view.info);
                            if sh.atlas_vectors_group != Some(listed_group)
                                || sh.show_atlas.len() != n_structures
                            {
                                let ag = self
                                    .payload
                                    .atlas_groups
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner);
                                if ag.get(listed_group).is_some() {
                                    reset_atlas_vectors(&mut sh, &ag, listed_group);
                                    for (i, on) in sh.show_atlas.iter().enumerate() {
                                        if *on {
                                            roi_requests.push(i);
                                        }
                                    }
                                    changed = true;
                                }
                            }
                            if self.preset_group != Some(listed_group) {
                                let defaults = self
                                    .payload
                                    .atlas_groups
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .get(listed_group)
                                    .map(AtlasSurfaceGroup::default_show_flags);
                                self.preset_choice = defaults
                                    .filter(|d| *d == sh.show_atlas)
                                    .and_then(|d| info.initial_menu_entry(&d));
                                self.preset_group = Some(listed_group);
                                self.atlas_label_set = None;
                            }
                            if sh.atlas_alpha.len() != n_structures {
                                sh.atlas_alpha.resize(n_structures, 1.0);
                            }
                            if sh.atlas_rgb.len() != n_structures {
                                sh.atlas_rgb.resize(n_structures, [0.75, 0.75, 0.75]);
                            }

                            let menu = info.preset_menu();
                            let mut pick_preset: Option<usize> = None;
                            let preset_label_id = ui.label("Preset").id;
                            let preset_text = self
                                .preset_choice
                                .and_then(|i| menu.get(i))
                                .map_or("Custom selection", |p| p.label.as_str());
                            egui::ComboBox::from_id_salt("dbsloc_atlas_preset")
                                .width(ui.available_width())
                                .truncate()
                                .selected_text(preset_text)
                                .show_ui(ui, |ui| {
                                    for (i, p) in menu.iter().enumerate() {
                                        if ui
                                            .selectable_label(
                                                self.preset_choice == Some(i),
                                                p.label.as_str(),
                                            )
                                            .clicked()
                                        {
                                            pick_preset = Some(i);
                                        }
                                    }
                                })
                                .response
                                .labelled_by(preset_label_id)
                                .on_hover_text(
                                    "Show a predefined set of structures. Surfaces that are \
                                     not prepared yet are built in the background.",
                                );
                            if let Some(i) = pick_preset {
                                roi_requests
                                    .extend(menu[i].apply(&mut sh.show_atlas, &info.mesh_structure));
                                self.preset_choice = Some(i);
                                changed = true;
                            }
                            if !info.label_sets.is_empty() {
                                let names_label_id = ui.label("Structure names").id;
                                let current = self
                                    .atlas_label_set
                                    .and_then(|k| info.label_sets.get(k))
                                    .map_or("File names", |(n, _)| n.as_str());
                                egui::ComboBox::from_id_salt("dbsloc_atlas_label_set")
                                    .width(ui.available_width())
                                    .truncate()
                                    .selected_text(current)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.atlas_label_set,
                                            None,
                                            "File names",
                                        );
                                        for (k, (n, _)) in info.label_sets.iter().enumerate() {
                                            ui.selectable_value(
                                                &mut self.atlas_label_set,
                                                Some(k),
                                                n.as_str(),
                                            );
                                        }
                                    })
                                    .response
                                    .labelled_by(names_label_id)
                                    .on_hover_text("Which names the structure list shows");
                            }
                            let shown = sh.show_atlas.iter().filter(|x| **x).count();
                            ui.label(
                                egui::RichText::new(format!("{shown}/{n_structures} shown"))
                                    .small()
                                    .color(muted),
                            );

                            let id = ui.make_persistent_id("dbsloc_scene3d_structures_v2");
                            egui::collapsing_header::CollapsingState::load_with_default_open(
                                ui.ctx(),
                                id,
                                true,
                            )
                            .show_header(ui, |ui| {
                                ui.label("Structures");
                                crate::widgets::info_icon(ui, "Up / Down to move, Space to toggle");
                            })
                            .body(|ui| {
                                for (i, tag) in groups_view.names.iter().enumerate() {
                                    if i >= sh.show_atlas.len() {
                                        break;
                                    }
                                    let name = &info.display_name(i, tag, self.atlas_label_set);
                                    let focused = i == self.focused_structure;
                                    let highlight = ui.painter().add(egui::Shape::Noop);
                                    let row = ui.scope(|ui| {
                                        ui.set_min_width(ui.available_width());
                                        let resp = ui
                                            .add(egui::Checkbox::new(
                                                &mut sh.show_atlas[i],
                                                breakable_text(ui, name),
                                            ))
                                            .on_hover_text(if groups_view.meshed[i] {
                                                format!("Show or hide {name}")
                                            } else {
                                                format!(
                                                    "Show {name} — its surface is prepared \
                                                         in the background the first time it \
                                                         is shown"
                                                )
                                            });
                                        if resp.changed() {
                                            if sh.show_atlas[i] {
                                                roi_requests.push(i);
                                            }
                                            self.preset_choice = None;
                                            changed = true;
                                        }
                                        if resp.clicked() {
                                            self.focused_structure = i;
                                        }
                                        if sh.show_atlas[i] {
                                            ui.horizontal(|ui| {
                                                if !groups_view.meshed[i] {
                                                    ui.add(egui::Spinner::new().size(unit * 0.7))
                                                        .on_hover_text("Preparing this surface…");
                                                }
                                                changed |= ui
                                                    .color_edit_button_rgb(&mut sh.atlas_rgb[i])
                                                    .on_hover_text(format!("Colour of {name}"))
                                                    .changed();
                                                let slider = Self::opacity_slider_widget(
                                                    &mut sh.atlas_alpha[i],
                                                );
                                                let sr = fitted_slider(ui, slider, "100%")
                                                    .on_hover_text(format!("Opacity of {name}"));
                                                changed |= sr.changed();
                                                let value = f64::from(sh.atlas_alpha[i]);
                                                sr.widget_info(|| {
                                                    egui::WidgetInfo::slider(
                                                        true,
                                                        value,
                                                        format!("{name} opacity"),
                                                    )
                                                });
                                            });
                                        }
                                    });
                                    if focused {
                                        ui.painter().set(
                                            highlight,
                                            egui::Shape::rect_filled(
                                                row.response.rect.expand2(egui::vec2(3.0, 1.0)),
                                                3.0,
                                                accent.gamma_multiply(0.20),
                                            ),
                                        );
                                    }
                                    if focused && scroll_to_focus {
                                        row.response.scroll_to_me(Some(egui::Align::Center));
                                    }
                                }
                            });
                        });
                    });
            });
        if changed {
            sh.note_slice_edits(&backdrop_vol, slice_before, world_before);
            let mut live = shared.lock().unwrap_or_else(PoisonError::into_inner);
            live.azimuth = sh.azimuth;
            live.elevation = sh.elevation;
            live.projection = sh.projection;
            live.distance = sh.distance;
            live.target = sh.target;
            live.axial_k = sh.axial_k;
            live.coronal_j = sh.coronal_j;
            live.sagittal_i = sh.sagittal_i;
            live.slice_world = sh.slice_world;
            live.show_axial = sh.show_axial;
            live.show_coronal = sh.show_coronal;
            live.show_sagittal = sh.show_sagittal;
            live.axial_opacity = sh.axial_opacity;
            live.coronal_opacity = sh.coronal_opacity;
            live.sagittal_opacity = sh.sagittal_opacity;
            live.atlas_opacity = sh.atlas_opacity;
            live.atlas_overlay_on = sh.atlas_overlay_on;
            live.active_atlas_group = sh.active_atlas_group;
            live.atlas_id = sh.atlas_id.clone();
            live.slice_window = sh.slice_window;
            live.slice_level = sh.slice_level;
            live.show_electrodes = sh.show_electrodes;
            live.show_vta = sh.show_vta;
            live.show_contact_labels = sh.show_contact_labels;
            if (live.atlas_vectors_group, live.show_atlas.len()) == vectors_before {
                live.show_atlas = sh.show_atlas;
                live.atlas_alpha = sh.atlas_alpha;
                live.atlas_rgb = sh.atlas_rgb;
                live.atlas_vectors_group = sh.atlas_vectors_group;
            }
            live.bump();
        }
        if let Some(pick) = backdrop_pick {
            let pick_ctx = ui.ctx().clone();
            self.handle_backdrop_pick(pick, &pick_ctx);
        }

        if !roi_requests.is_empty()
            && self
                .payload
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .active_atlas_group
                == groups_view.group_index
        {
            for r in roi_requests {
                self.payload.request_lead_roi(groups_view.group_index, r);
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let sense = egui::Sense::click_and_drag().union(egui::Sense::hover());
            let (rect, resp) = ui.allocate_exact_size(ui.available_size(), sense);
            self.last_scene_rect_points = Some(rect);
            let scheme = self.navigation;
            let resp = resp.on_hover_text(scheme.hover_text());

            let aspect = rect.width() / rect.height().max(1.0);
            if resp.hovered() {
                let (wheel, pinch) = ui.ctx().input(|i| {
                    let events = i.events.iter().filter_map(|ev| match ev {
                        egui::Event::MouseWheel {
                            unit,
                            delta,
                            modifiers,
                            ..
                        } => Some((*unit, *delta, *modifiers)),
                        _ => None,
                    });
                    (split_wheel_events(events, rect.height()), i.zoom_delta())
                });
                let pinch_active = (pinch - 1.0).abs() > 1e-4;
                if pinch_active || wheel != WheelFrame::default() {
                    let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    if pinch_active {
                        adjust_dolly_distance_pinch(&mut sh.distance, pinch);
                        sh.bump();
                    } else {
                        if wheel.zoom != 0.0 {
                            if scheme == NavigationScheme::LeadDbs {
                                adjust_dolly_distance_factor(
                                    &mut sh.distance,
                                    lead_dbs_wheel_zoom_factor(wheel.zoom),
                                );
                            } else {
                                adjust_dolly_distance(&mut sh.distance, wheel.zoom);
                            }
                            sh.bump();
                        }
                        pan_target(&mut sh, wheel.pan);
                    }
                }
            }

            if keys.pan_right != 0 || keys.pan_up != 0 {
                let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                pan_target(&mut sh, key_pan_points(keys.pan_right, keys.pan_up));
            }

            let centred = resp.double_clicked_by(egui::PointerButton::Primary)
                && match (self.payload.group(), resp.interact_pointer_pos()) {
                    (Some(group), Some(pointer)) => {
                        let vp = orbit_vp(
                            &shared.lock().unwrap_or_else(PoisonError::into_inner),
                            aspect,
                        )
                        .0;
                        match group_pick(group, rect, vp, pointer, Self::unit(ui) * 0.8) {
                            Some(hit) => {
                                let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                                sh.target = lead_centre(&group.leads[hit.lead]);
                                sh.bump();
                                true
                            }
                            None => false,
                        }
                    }
                    _ => false,
                };
            let (alt, shift, ctrl) = ui
                .ctx()
                .input(|i| (i.modifiers.alt, i.modifiers.shift, i.modifiers.ctrl));
            let pri_on = resp.dragged_by(egui::PointerButton::Primary);
            let sec_on = resp.dragged_by(egui::PointerButton::Secondary);
            let mid_on = resp.dragged_by(egui::PointerButton::Middle);

            if scheme == NavigationScheme::LeadDbs {
                let d = resp.drag_delta();
                if centred {
                } else if resp.double_clicked()
                    || resp.double_clicked_by(egui::PointerButton::Secondary)
                    || resp.double_clicked_by(egui::PointerButton::Middle)
                {
                    let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    let backdrop_vol = self.payload.backdrop_volume();
                    reset_view(&mut sh, &self.payload.view_defaults, &backdrop_vol);
                    sh.bump();
                } else if mid_on || (pri_on && shift) {
                    if d != egui::Vec2::ZERO {
                        let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                        adjust_dolly_distance_factor(
                            &mut sh.distance,
                            lead_dbs_drag_zoom_factor(d),
                        );
                        sh.bump();
                    }
                } else if sec_on || (pri_on && ctrl) {
                    if d != egui::Vec2::ZERO {
                        let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                        pan_target(&mut sh, d * LEAD_DBS_PAN_GAIN);
                    }
                } else if pri_on && d != egui::Vec2::ZERO {
                    let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    sh.azimuth = wrap_angle(sh.azimuth + d.x * LEAD_DBS_ORBIT_RAD_PER_POINT);
                    sh.elevation = (sh.elevation - d.y * LEAD_DBS_ORBIT_RAD_PER_POINT)
                        .clamp(-ELEVATION_MAX_RAD, ELEVATION_MAX_RAD);
                    sh.bump();
                }
            } else if sec_on && ctrl {
                let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                adjust_dolly_distance_right_drag(&mut sh.distance, resp.drag_delta().y);
                sh.bump();
            } else if sec_on || mid_on || (pri_on && (alt || shift)) {
                let d = resp.drag_delta();
                if d != egui::Vec2::ZERO {
                    let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    pan_target(&mut sh, d);
                }
            } else if pri_on {
                let rad_per_px = std::f32::consts::PI / rect.height().max(1.0);
                let dx = resp.drag_delta().x * rad_per_px;
                let dy = resp.drag_delta().y * rad_per_px;
                if dx != 0.0 || dy != 0.0 {
                    let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    sh.azimuth = wrap_angle(sh.azimuth + dx);
                    sh.elevation = (sh.elevation - dy).clamp(-ELEVATION_MAX_RAD, ELEVATION_MAX_RAD);
                    sh.bump();
                }
            }

            let cb = ScenePaintCallback {
                payload: Arc::clone(&self.payload),
                aspect,
                rect,
            };
            ui.painter()
                .add(egui_wgpu::Callback::new_paint_callback(rect, cb));

            let (labels_on, vp, target) = {
                let sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
                (sh.show_contact_labels, orbit_vp(&sh, aspect).0, sh.target)
            };
            self.paint_orientation(ui, rect, vp, target);
            if let Some(group) = self.payload.group() {
                paint_group_legend(ui, rect, group, muted);
                if let Some(pointer) = resp.hover_pos() {
                    paint_group_hover(ui, rect, vp, group, pointer, accent);
                }
            }
            if labels_on {
                self.paint_contact_labels(ui, rect, vp);
            }
        });

        let mut screenshot: Option<(PathBuf, std::sync::Arc<egui::ColorImage>)> = None;
        ctx.input_mut(|i| {
            for ev in &i.events {
                if let egui::Event::Screenshot {
                    user_data, image, ..
                } = ev
                {
                    if let Some(data) = &user_data.data {
                        if let Ok(p) = data.clone().downcast::<PathBuf>() {
                            screenshot = Some(((*p).clone(), std::sync::Arc::clone(image)));
                        }
                    }
                }
            }
        });
        if let Some((path, img)) = screenshot {
            match self.last_scene_rect_points {
                None => {
                    self.save_status = Some(Err(
                        "The scene area was not drawn yet — try again.".to_owned()
                    ));
                }
                Some(rect_points) => {
                    let ppp = ctx.pixels_per_point();
                    let (tx, rx) = mpsc::channel::<Result<PathBuf, String>>();
                    self.save_result_rx = Some(rx);
                    let ctx_thread = ctx.clone();
                    spawn_worker(move || {
                        let r = crop_and_save_screenshot(&img, rect_points, ppp, &path);
                        let _ = tx.send(r);
                        ctx_thread.request_repaint();
                    });
                }
            }
        }
        if let Some(rx) = &self.save_result_rx {
            match rx.try_recv() {
                Ok(res) => {
                    self.save_status = Some(res);
                    self.save_result_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.save_result_rx = None,
            }
        }

        if self.payload.busy() {
            ctx.request_repaint();
            let (done, total, current) = self.payload.load_progress();
            egui::Area::new(egui::Id::new("dbsloc_atlas_loading"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::RIGHT_TOP, egui::Vec2::new(-12.0, 12.0))
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(Self::unit(ui) * 18.0);
                        let label = match current {
                            Some(name) => format!("Preparing {name}"),
                            None => "Preparing atlas surfaces".to_owned(),
                        };
                        if total > 0 {
                            ui.add(
                                egui::ProgressBar::new(done as f32 / total as f32)
                                    .desired_width(Self::unit(ui) * 15.0)
                                    .text(format!("{label} — {done}/{total}")),
                            );
                        } else {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new());
                                ui.label(label);
                            });
                        }
                    });
                });
        }

        if let Some(msg) = self.payload.atlas_error_message() {
            if !self.atlas_error_dismissed {
                let mut open = true;
                egui::Window::new("⚠ Atlas could not be loaded")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(&ctx, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&msg).monospace().small()).wrap(),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .button("Copy")
                                .on_hover_text("Copy the full message to the clipboard")
                                .clicked()
                            {
                                ctx.copy_text(msg.clone());
                            }
                            if ui.button("Dismiss").clicked() {
                                self.atlas_error_dismissed = true;
                            }
                        });
                    });
                if !open {
                    self.atlas_error_dismissed = true;
                }
            }
        }
    }
}

/// The side-panel controls of a group scene: what to show, which patient or group to highlight.
fn group_controls(ui: &mut egui::Ui, payload: &Scene3DPayload, muted: egui::Color32) -> bool {
    let (Some(group), Some(mut d)) = (payload.group(), payload.group_display()) else {
        return false;
    };
    let before = d.clone();
    let clinical = matches!(group.colour, ColourMode::Clinical { .. });
    egui::CollapsingHeader::new(egui::RichText::new("Group").strong())
        .id_salt("dbsloc_sec_group_v1")
        .default_open(true)
        .show(ui, |ui| {
            ui.checkbox(&mut d.show_active, "Show active contacts")
                .on_hover_text("Draw the contacts that carry current in the group stimulation.");
            ui.checkbox(&mut d.show_passive, "Show passive contacts")
                .on_hover_text("Draw the contacts that carry no current.");
            ui.checkbox(&mut d.highlight_active, "Highlight active contacts")
                .on_hover_text(
                    "Draw active contacts in red with a pale edge at full size and passive \
                     contacts at half size.",
                );
            ui.checkbox(&mut d.show_vtas, "Show volumes of tissue activated")
                .on_hover_text(
                    "Draw each lead's volume of tissue activated, tinted by group or yellow \
                     when colouring by a clinical variable.",
                );
            if clinical {
                ui.checkbox(&mut d.point_cloud, "Colour contacts by value")
                    .on_hover_text(
                        "Draw one point per contact, coloured blue, white or red by the \
                         clinical value; contacts without a value are not drawn.",
                    );
            }
            let patients = group.patients();
            let patient_text = d
                .selected_patient
                .clone()
                .unwrap_or_else(|| "None".to_owned());
            egui::ComboBox::from_label("Highlight patient")
                .selected_text(patient_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.selected_patient, None, "None");
                    for p in &patients {
                        ui.selectable_value(&mut d.selected_patient, Some(p.clone()), p);
                    }
                })
                .response
                .on_hover_text("Highlight one patient's leads; the other leads are drawn faint.");
            let group_text = d
                .selected_group
                .and_then(|n| group.groups.iter().find(|g| g.number == n))
                .map_or_else(|| "None".to_owned(), |g| g.label.clone());
            egui::ComboBox::from_label("Highlight group")
                .selected_text(group_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.selected_group, None, "None");
                    for g in &group.groups {
                        ui.selectable_value(&mut d.selected_group, Some(g.number), &g.label);
                    }
                })
                .response
                .on_hover_text(
                    "Highlight the patients of one group; the other leads are drawn faint.",
                );
            let (done, total) = payload.group_vta_progress();
            if total > 0 && done < total {
                ui.add(
                    egui::ProgressBar::new(done as f32 / total as f32)
                        .text(format!("Volumes of tissue activated {done} / {total}")),
                );
            }
            let per_model = group.leads_per_model();
            ui.label(
                egui::RichText::new(format!(
                    "{} leads, {} electrode models",
                    group.leads.len(),
                    per_model.len()
                ))
                .small()
                .color(muted),
            );
            for w in group.warnings.iter().take(3) {
                ui.label(egui::RichText::new(w).small().color(muted));
            }
        });
    if d == before {
        return false;
    }
    payload.store_group_display(d)
}

struct FibreRow {
    id: String,
    label: String,
    shown: Option<usize>,
    range: Option<crate::scene_fibres::SignRange>,
    total: usize,
    uniform: bool,
    signed: bool,
    colour: [f32; 4],
    setting: crate::scene_layers::FibreSettings,
}

struct MapRow {
    id: String,
    label: String,
    setting: crate::scene_layers::MapSettings,
}

fn fibre_count_text(row: &FibreRow) -> String {
    if !row.setting.visible {
        return format!("hidden, {} fibres", row.total);
    }
    let Some(shown) = row.shown else {
        return "building…".to_owned();
    };
    let mut text = format!("{shown} of {} fibres shown", row.total);
    if row.signed {
        if let Some(range) = row.range {
            if let Some((lo, hi)) = range.pos {
                text.push_str(&format!(", positive {lo:.3} to {hi:.3}"));
            }
            if let Some((lo, hi)) = range.neg {
                text.push_str(&format!(", negative -{lo:.3} to -{hi:.3}"));
            }
        }
    }
    text
}

fn swatch(ui: &mut egui::Ui, colour: [f32; 4], hint: &str) {
    let size = egui::Vec2::splat(ui.text_style_height(&egui::TextStyle::Body));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    ui.painter().rect_filled(
        rect,
        2.0,
        egui::Color32::from_rgb(c(colour[0]), c(colour[1]), c(colour[2])),
    );
    response.on_hover_text(hint);
}

fn layer_controls(ui: &mut egui::Ui, payload: &Scene3DPayload, muted: egui::Color32) -> bool {
    use crate::scene_fibres::POSITIVE_PEAK;
    use crate::scene_layers::{FibreColouring, FibreSettings, MapMode, MAX_FIBRES_LIMIT};
    let (_, built) = payload.fibre_geometry();
    let (mut fibres, mut maps, mut width, mut max_fibres, supported) = payload.with_layers(|l| {
        let fibres: Vec<FibreRow> = l
            .fibres
            .iter()
            .map(|f| {
                let (uniform, colour) = match l.effective_colouring(f) {
                    FibreColouring::Uniform(c) => (true, c),
                    _ => (
                        false,
                        [POSITIVE_PEAK[0], POSITIVE_PEAK[1], POSITIVE_PEAK[2], 1.0],
                    ),
                };
                FibreRow {
                    id: f.id.clone(),
                    label: f.label.clone(),
                    shown: built
                        .as_ref()
                        .and_then(|g| g.shown.iter().find(|(id, _)| *id == f.id).map(|(_, n)| *n)),
                    range: built
                        .as_ref()
                        .and_then(|g| g.ranges.iter().find(|(id, _)| *id == f.id).map(|(_, r)| *r)),
                    total: f.total_fibres.max(f.n_fibres()),
                    uniform,
                    signed: f.colouring == FibreColouring::Signed,
                    colour,
                    setting: l.fibre_setting(&f.id),
                }
            })
            .collect();
        let maps: Vec<MapRow> = l
            .maps
            .iter()
            .filter_map(|m| {
                l.map_settings.get(&m.id).map(|s| MapRow {
                    id: m.id.clone(),
                    label: m.label.clone(),
                    setting: *s,
                })
            })
            .collect();
        (
            fibres,
            maps,
            l.fibre_width_px,
            l.max_fibres,
            l.fibre_file_supported,
        )
    });
    let before_fibres: Vec<_> = fibres.iter().map(|f| (f.setting, f.colour)).collect();
    let before_maps: Vec<_> = maps.iter().map(|m| m.setting).collect();
    let (before_width, before_max) = (width, max_fibres);
    let mut remove_fibres: Vec<String> = Vec::new();
    let mut remove_maps: Vec<String> = Vec::new();
    let mut want_file = false;

    if !fibres.is_empty() || supported {
        egui::CollapsingHeader::new(egui::RichText::new("Fibres").strong())
            .id_salt("dbsloc_sec_fibres_v1")
            .default_open(true)
            .show(ui, |ui| {
                for row in &mut fibres {
                    ui.checkbox(&mut row.setting.visible, row.label.as_str())
                        .on_hover_text("Show or hide the fibres of this layer.");
                    indented(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            if row.uniform {
                                ui.color_edit_button_rgba_unmultiplied(&mut row.colour)
                                    .on_hover_text("Colour of every fibre in this layer.");
                            } else {
                                swatch(
                                    ui,
                                    row.colour,
                                    "Fibres run from white to this colour with their value.",
                                );
                            }
                            ui.label(
                                egui::RichText::new(fibre_count_text(row))
                                    .small()
                                    .color(muted),
                            );
                            if ui
                                .small_button("Remove")
                                .on_hover_text("Remove this fibre layer from the viewer.")
                                .clicked()
                            {
                                remove_fibres.push(row.id.clone());
                            }
                        });
                    });
                }
                if !fibres.is_empty() {
                    let col = label_column(ui, &[("Width", false), ("Maximum shown", false)]);
                    labelled_slider(
                        ui,
                        RowLabel::new("Width", col).named("Fibre width", f64::from(width)),
                        egui::Slider::new(&mut width, 1.0..=6.0)
                            .step_by(1.0)
                            .suffix(" px"),
                        "6 px",
                        "Width of every fibre on screen, in pixels.",
                    );
                    labelled_slider(
                        ui,
                        RowLabel::new("Maximum shown", col)
                            .named("Maximum fibres shown", max_fibres as f64),
                        egui::Slider::new(&mut max_fibres, 1..=MAX_FIBRES_LIMIT).logarithmic(true),
                        "100000",
                        "Most fibres drawn per layer; the fibres with the largest absolute \
                         value are kept.",
                    );
                }
                if supported
                    && ui
                        .button("Add fibre file…")
                        .on_hover_text(
                            "Draw the fibres of a Lead-DBS fibre file (.mat) or a TrackVis file \
                             (.trk) in a colour of your choice.",
                        )
                        .clicked()
                {
                    want_file = true;
                }
            });
    }

    if !maps.is_empty() {
        egui::CollapsingHeader::new(egui::RichText::new("Maps").strong())
            .id_salt("dbsloc_sec_maps_v1")
            .default_open(true)
            .show(ui, |ui| {
                for row in &mut maps {
                    ui.checkbox(&mut row.setting.visible, row.label.as_str())
                        .on_hover_text("Show or hide this map.");
                    indented(ui, |ui| {
                        egui::ComboBox::from_id_salt(("dbsloc_map_mode", row.id.as_str()))
                            .selected_text(match row.setting.mode {
                                MapMode::Isosurface => "Isosurface",
                                MapMode::Slices => "Slices",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut row.setting.mode,
                                    MapMode::Isosurface,
                                    "Isosurface",
                                )
                                .on_hover_text(
                                    "Smoothed surfaces around the positive (red) and negative \
                                     (blue) values above the threshold.",
                                );
                                ui.selectable_value(
                                    &mut row.setting.mode,
                                    MapMode::Slices,
                                    "Slices",
                                )
                                .on_hover_text(
                                    "Colour the values above the threshold on the anatomy \
                                     slices. Only the first map in this mode is drawn.",
                                );
                            })
                            .response
                            .on_hover_text("How this map is drawn.");
                        let max_abs = row.setting.max_abs();
                        if max_abs > 0.0 {
                            let col = label_column(ui, &[("Threshold", false)]);
                            let name = format!("Threshold of {}", row.label);
                            labelled_slider(
                                ui,
                                RowLabel::new("Threshold", col)
                                    .named(&name, f64::from(row.setting.threshold)),
                                egui::Slider::new(&mut row.setting.threshold, 0.0..=max_abs)
                                    .max_decimals(4),
                                "0.0000",
                                "Values with a smaller absolute value are not shown.",
                            );
                        }
                        if ui
                            .small_button("Remove")
                            .on_hover_text("Remove this map from the viewer.")
                            .clicked()
                        {
                            remove_maps.push(row.id.clone());
                        }
                    });
                }
            });
    }

    let fibres_changed = fibres
        .iter()
        .zip(&before_fibres)
        .any(|(f, b)| (f.setting, f.colour) != *b)
        || max_fibres != before_max;
    let maps_changed = maps.iter().zip(&before_maps).any(|(m, b)| m.setting != *b);
    let width_changed = width != before_width;
    if fibres_changed || maps_changed || width_changed || want_file {
        payload.with_layers(|l| {
            for f in &fibres {
                l.fibre_settings.insert(
                    f.id.clone(),
                    FibreSettings {
                        visible: f.setting.visible,
                        uniform: f.uniform.then_some(f.colour),
                    },
                );
            }
            for m in &maps {
                l.map_settings.insert(m.id.clone(), m.setting);
            }
            l.fibre_width_px = width;
            l.max_fibres = max_fibres;
            l.fibre_file_requested |= want_file;
        });
    }
    if fibres_changed {
        payload.touch_fibres();
    }
    if maps_changed {
        payload.touch_maps();
    }
    if width_changed && !fibres_changed {
        payload.bump_shared();
    }
    for id in &remove_fibres {
        payload.remove_fibre_layer(id);
    }
    for id in &remove_maps {
        payload.remove_map_layer(id);
    }
    fibres_changed
        || maps_changed
        || width_changed
        || !remove_fibres.is_empty()
        || !remove_maps.is_empty()
}

fn mix_colour(rgb: [f32; 3]) -> egui::Color32 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgb(c(rgb[0]), c(rgb[1]), c(rgb[2]))
}

fn paint_group_legend(ui: &egui::Ui, rect: egui::Rect, group: &GroupScene, text: egui::Color32) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let em = font.size.max(8.0);
    let bg = ui.visuals().extreme_bg_color.gamma_multiply(0.85);
    let painter = ui.painter();
    match group.legend() {
        Legend::Groups(entries) => {
            let row = em * 1.5;
            let width = em * 12.0;
            let height = row * entries.len() as f32 + em;
            let frame = egui::Rect::from_min_size(
                egui::pos2(rect.left() + em, rect.bottom() - height - em),
                egui::vec2(width, height),
            );
            painter.rect_filled(frame, em * 0.4, bg);
            for (i, (label, colour)) in entries.iter().enumerate() {
                let y = frame.top() + 0.5 * em + row * (i as f32 + 0.5);
                painter.circle_filled(
                    egui::pos2(frame.left() + em, y),
                    0.45 * em,
                    mix_colour(*colour),
                );
                painter.text(
                    egui::pos2(frame.left() + 2.0 * em, y),
                    egui::Align2::LEFT_CENTER,
                    label,
                    font.clone(),
                    text,
                );
            }
        }
        Legend::ColourBar {
            name,
            normalisation,
            range,
        } => {
            let bar_w = em * 12.0;
            let bar_h = em * 1.1;
            let frame = egui::Rect::from_min_size(
                egui::pos2(rect.left() + em, rect.bottom() - em * 5.0 - em),
                egui::vec2(bar_w + 2.0 * em, em * 5.0),
            );
            painter.rect_filled(frame, em * 0.4, bg);
            let title = if normalisation.is_empty() || normalisation == "none" {
                name.clone()
            } else {
                format!("{name} ({normalisation})")
            };
            painter.text(
                frame.left_top() + egui::vec2(em, 0.6 * em),
                egui::Align2::LEFT_TOP,
                title,
                font.clone(),
                text,
            );
            let top = frame.top() + 2.2 * em;
            let steps = 48usize;
            for i in 0..steps {
                let t = (i as f32 + 0.5) / steps as f32;
                let x0 = frame.left() + em + bar_w * i as f32 / steps as f32;
                let x1 = frame.left() + em + bar_w * (i as f32 + 1.0) / steps as f32;
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, top),
                        egui::pos2(x1 + 0.5, top + bar_h),
                    ),
                    0.0,
                    mix_colour(blue_white_red(t)),
                );
            }
            painter.text(
                egui::pos2(frame.left() + em, top + bar_h + 0.3 * em),
                egui::Align2::LEFT_TOP,
                format!("{:.2}", range[0]),
                font.clone(),
                text,
            );
            painter.text(
                egui::pos2(frame.left() + em + bar_w, top + bar_h + 0.3 * em),
                egui::Align2::RIGHT_TOP,
                format!("{:.2}", range[1]),
                font,
                text,
            );
        }
    }
}

fn lead_centre(lead: &crate::group_scene::GroupLead) -> Vec3 {
    if lead.contacts.is_empty() {
        0.5 * (lead.head + lead.tail)
    } else {
        lead.contacts.iter().copied().sum::<Vec3>() / lead.contacts.len() as f32
    }
}

fn group_pick(
    group: &GroupScene,
    rect: egui::Rect,
    vp: Mat4,
    pointer: egui::Pos2,
    radius: f32,
) -> Option<crate::group_scene::PickHit> {
    let project = |p: Vec3| Scene3DEframeApp::project(rect, vp, p).map(|s| [s.x, s.y]);
    group.pick(&project, [pointer.x, pointer.y], radius)
}

fn paint_group_hover(
    ui: &egui::Ui,
    rect: egui::Rect,
    vp: Mat4,
    group: &GroupScene,
    pointer: egui::Pos2,
    accent: egui::Color32,
) {
    let em = egui::TextStyle::Small.resolve(ui.style()).size.max(8.0);
    let Some(hit) = group_pick(group, rect, vp, pointer, em * 1.2) else {
        return;
    };
    let lead = &group.leads[hit.lead];
    let painter = ui.painter();
    if let (Some(a), Some(b)) = (
        Scene3DEframeApp::project(rect, vp, lead.head),
        Scene3DEframeApp::project(rect, vp, lead.tail),
    ) {
        painter.line_segment([a, b], egui::Stroke::new(0.25 * em, accent));
    }
    let text = group.hover_text(&hit);
    let galley = painter.layout_no_wrap(
        text,
        egui::TextStyle::Small.resolve(ui.style()),
        ui.visuals().text_color(),
    );
    let pos = pointer + egui::vec2(em, em);
    let frame = egui::Rect::from_min_size(pos, galley.size() + egui::vec2(em, 0.6 * em));
    painter.rect_filled(
        frame,
        0.3 * em,
        ui.visuals().extreme_bg_color.gamma_multiply(0.92),
    );
    painter.galley(
        frame.min + egui::vec2(0.5 * em, 0.3 * em),
        galley,
        ui.visuals().text_color(),
    );
}

fn reset_view(sh: &mut SceneShared, d: &SceneViewDefaults, backdrop: &TemplateVolume) {
    sh.show_axial = false;
    sh.show_coronal = true;
    sh.show_sagittal = true;
    sh.azimuth = d.azimuth;
    sh.elevation = d.elevation;
    sh.distance = d.distance;
    sh.target = d.target;
    sh.set_slices_to_world(backdrop, DEFAULT_SLICE_MM);
    sh.axial_opacity = 1.0;
    sh.coronal_opacity = 1.0;
    sh.sagittal_opacity = 1.0;
    sh.slice_window = 1.0;
    sh.slice_level = 0.5;
}

/// Wrap an angle into `(-pi, pi]`.
fn wrap_angle(a: f32) -> f32 {
    use std::f32::consts::PI;
    let mut x = (a + PI) % (2.0 * PI);
    if x < 0.0 {
        x += 2.0 * PI;
    }
    x - PI
}

/// Write an egui screenshot to a PNG / JPEG file. Worker-thread helper shared
/// with the embedding application so it does not need its own image encoder.
pub fn save_color_image(img: &egui::ColorImage, path: &Path) -> Result<(), String> {
    let (w, h) = (img.size[0], img.size[1]);
    let mut buf: Vec<u8> = Vec::with_capacity(w * h * 4);
    for px in &img.pixels {
        buf.extend_from_slice(&[px.r(), px.g(), px.b(), px.a()]);
    }
    let im = ImageBuffer::<Rgba<u8>, _>::from_raw(w as u32, h as u32, buf)
        .ok_or_else(|| "Could not build the image buffer.".to_owned())?;
    im.save(path)
        .map_err(|e| format!("Could not write {}: {e}", path.display()))
}

/// Crop a full-window screenshot to the scene rectangle and write it to disk.
/// Runs on a worker thread — never on the UI thread.
fn crop_and_save_screenshot(
    img: &egui::ColorImage,
    rect_points: egui::Rect,
    ppp: f32,
    path: &Path,
) -> Result<PathBuf, String> {
    let w = img.size[0];
    let h = img.size[1];
    let x0 = (rect_points.min.x * ppp).floor().max(0.0) as usize;
    let y0 = (rect_points.min.y * ppp).floor().max(0.0) as usize;
    let x1 = (rect_points.max.x * ppp).ceil().min(w as f32) as usize;
    let y1 = (rect_points.max.y * ppp).ceil().min(h as f32) as usize;
    if x1 <= x0 || y1 <= y0 {
        return Err("The scene area is too small to save.".to_owned());
    }
    let cw = x1 - x0;
    let chh = y1 - y0;
    let mut buf: Vec<u8> = Vec::with_capacity(cw * chh * 4);
    for yy in y0..y1 {
        let row = yy * w;
        for xx in x0..x1 {
            let px = img.pixels[row + xx];
            buf.extend_from_slice(&[px.r(), px.g(), px.b(), px.a()]);
        }
    }
    let im = ImageBuffer::<Rgba<u8>, _>::from_raw(cw as u32, chh as u32, buf)
        .ok_or_else(|| "Could not build the image buffer.".to_owned())?;
    im.save(path)
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod software_default_tests {
    use super::persisted_atlas_overlay;

    #[test]
    fn a_software_hide_is_session_only_and_a_user_choice_is_kept() {
        assert!(persisted_atlas_overlay(false, true));
        assert!(persisted_atlas_overlay(true, false));
        assert!(persisted_atlas_overlay(true, true));
        assert!(!persisted_atlas_overlay(false, false));
    }
}

#[cfg(test)]
mod pan_tests {
    use glam::Vec3;

    use super::{orbit_screen_basis, pan_target, Projection, SceneShared, DEFAULT_SLICE_MM};
    use crate::gesture_zoom::{key_pan_points, pan_scale};

    fn shared(distance: f32) -> SceneShared {
        SceneShared {
            azimuth: 0.0,
            elevation: 0.0,
            projection: Projection::default(),
            distance,
            target: Vec3::new(1.0, 2.0, 3.0),
            axial_k: 0,
            coronal_j: 0,
            sagittal_i: 0,
            show_axial: false,
            show_coronal: true,
            show_sagittal: true,
            axial_opacity: 1.0,
            coronal_opacity: 1.0,
            sagittal_opacity: 1.0,
            atlas_opacity: 1.0,
            atlas_overlay_on: true,
            active_atlas_group: 0,
            atlas_alpha: vec![],
            show_atlas: vec![],
            atlas_rgb: vec![],
            atlas_vectors_group: None,
            atlas_id: None,
            slice_window: 1.0,
            slice_level: 0.5,
            show_electrodes: true,
            show_vta: true,
            show_contact_labels: false,
            revision: 1,
            slice_world: DEFAULT_SLICE_MM,
        }
    }

    #[test]
    fn panning_right_moves_the_target_against_the_screen_right_axis() {
        let mut sh = shared(100.0);
        let before = sh.target;
        let (right, _) = orbit_screen_basis(&sh);
        pan_target(&mut sh, egui::vec2(10.0, 0.0));
        let moved = sh.target - before;
        assert!(
            (moved + right * 10.0 * pan_scale(100.0)).length() < 1e-5,
            "{moved:?}"
        );
        assert_eq!(sh.revision, 2);
    }

    #[test]
    fn panning_down_moves_the_target_along_screen_up() {
        let mut sh = shared(100.0);
        let before = sh.target;
        let (_, up) = orbit_screen_basis(&sh);
        pan_target(&mut sh, egui::vec2(0.0, 10.0));
        let moved = sh.target - before;
        assert!(
            (moved - up * 10.0 * pan_scale(100.0)).length() < 1e-5,
            "{moved:?}"
        );
        assert!(
            up.z > 0.99,
            "screen up is world superior at the horizon: {up:?}"
        );
    }

    #[test]
    fn pan_magnitude_scales_with_the_camera_distance() {
        let mut near = shared(50.0);
        let mut far = shared(500.0);
        let b_near = near.target;
        let b_far = far.target;
        pan_target(&mut near, egui::vec2(7.0, -3.0));
        pan_target(&mut far, egui::vec2(7.0, -3.0));
        let ratio = (far.target - b_far).length() / (near.target - b_near).length();
        assert!((ratio - 10.0).abs() < 1e-3, "{ratio}");
    }

    #[test]
    fn a_keyboard_step_pans_a_fixed_fraction_of_the_distance() {
        let mut sh = shared(200.0);
        let before = sh.target;
        pan_target(&mut sh, key_pan_points(0, 1));
        let moved = sh.target - before;
        assert!((moved.length() - 10.0).abs() < 1e-3, "{moved:?}");
        assert!(
            moved.z < 0.0,
            "Shift+Up lifts the scene, so the target drops: {moved:?}"
        );
    }

    #[test]
    fn a_zero_delta_leaves_the_scene_untouched() {
        let mut sh = shared(100.0);
        pan_target(&mut sh, egui::Vec2::ZERO);
        assert_eq!(sh.revision, 1);
        assert_eq!(sh.target, Vec3::new(1.0, 2.0, 3.0));
    }
}

#[cfg(test)]
mod atlas_data_tests {
    use std::io::Write;

    use super::{Scene3DPayload, SceneManifestV1};
    use dbsloc_checkreg::nifti_io::make_nifti1_bytes;

    pub(super) fn manifest(
        nii_path: std::path::PathBuf,
        json_path: std::path::PathBuf,
    ) -> SceneManifestV1 {
        SceneManifestV1 {
            manifest_version: 1,
            subj_id: "TEST".into(),
            space: "test".into(),
            template_nii: nii_path,
            reco_json: json_path,
            atlas_obj_dir: None,
            atlas_nii: None,
            atlas_max_labels: None,
            atlas_lead_sets: vec![],
            atlas_lead_nii_dir: None,
            atlas_default_on_substrings: None,
            atlas_mesh_smooth_iters: 0,
            atlas_lead_resample_ijk_subtract: 0.0,
            atlas_lead_native_mc_ijk_shift: 0.0,
            atlas_lead_lazy_load: true,
            atlas_cache_precompute: true,
            atlas_cache_dir_name: "precomputed_obj".into(),
            atlas_cache_only_default_on: true,
            atlas_mesh_weld_eps_mm: 0.05,
            atlas_mesh_preblur_iters: 6,
            atlas_mesh_morph_close_iters: 0,
            atlas_mesh_resample_to_template: false,
            atlas_unlit: false,
            atlas_flat_normals: false,
            atlas_mesh_taubin_iters: 10,
            atlas_mesh_taubin_lambda: 0.5,
            atlas_mesh_taubin_mu: -0.53,
            subj_dir: None,
            backdrops: vec![],
            vta: vec![],
            group: None,
        }
    }

    pub(super) fn template(dir: &std::path::Path) -> std::path::PathBuf {
        let n = 8usize;
        let voxels: Vec<f32> = (0..n * n * n).map(|i| (i % 7) as f32).collect();
        let nii_path = dir.join("t1.nii");
        std::fs::write(&nii_path, make_nifti1_bytes(n, n, n, &voxels)).unwrap();
        nii_path
    }

    fn group(label: &str, id: &str, n: usize) -> super::AtlasSurfaceGroup {
        let meshes = (0..n)
            .map(|i| crate::atlas_marching::NamedAtlasMesh {
                name: format!("{label}{i}"),
                mesh: None,
                color: [0.1, 0.2, 0.3, 0.7],
                source: None,
            })
            .collect();
        super::per_mesh_group(label, id, meshes, None)
    }

    #[test]
    fn the_atlas_id_selects_the_set_and_unknown_ids_fall_back_to_distal_minimal() {
        let groups = vec![
            group("AHEAD", "ahead", 2),
            group(
                crate::atlas_sets::DEFAULT_ATLAS_SET_NAME,
                "leaddbs:distal",
                3,
            ),
        ];
        let state: super::SceneViewState =
            serde_json::from_str(r#"{ "active_atlas_group": 0, "atlas_opacity": 0.5 }"#).unwrap();
        assert!(state.atlas_id.is_none());
        let mut sh = super::default_slice_tests::shared_for_tests();
        sh.restore(&state);
        assert!(super::resolve_active_atlas(&mut sh, &groups));
        assert_eq!(sh.active_atlas_group, 1);
        assert_eq!(sh.atlas_id.as_deref(), Some("leaddbs:distal"));
        assert!(!super::resolve_active_atlas(&mut sh, &groups));

        sh.atlas_id = Some("ahead".into());
        assert!(super::resolve_active_atlas(&mut sh, &groups));
        assert_eq!(sh.active_atlas_group, 0);
        let snap = sh.snapshot();
        assert_eq!(snap.atlas_id.as_deref(), Some("ahead"));
        let back: super::SceneViewState =
            serde_json::from_str(&serde_json::to_string(&snap).unwrap()).unwrap();
        assert_eq!(back.atlas_id.as_deref(), Some("ahead"));

        sh.atlas_id = Some("gone".into());
        super::resolve_active_atlas(&mut sh, &groups);
        assert_eq!(sh.active_atlas_group, 1);
        assert_eq!(sh.atlas_id.as_deref(), Some("leaddbs:distal"));
        super::reset_atlas_vectors(&mut sh, &groups, 1);
        assert_eq!(sh.show_atlas, vec![true; 3]);
        assert_eq!(sh.atlas_rgb[0], [0.1, 0.2, 0.3]);
        assert_eq!(sh.atlas_vectors_group, Some(1));
    }

    pub(super) fn layer_payload(tag: &str) -> std::sync::Arc<Scene3DPayload> {
        let tmp = std::env::temp_dir().join(format!("dbsloc_scene3d_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let nii_path = template(&tmp);
        let json_path = tmp.join("reco.json");
        std::fs::write(
            &json_path,
            r#"{ "version": 2, "meta": { "side_keys": "1=right,2=left" },
            "mni": { "2": { "trajectory": [[-3.0, 3.0, 4.0], [-4.0, 4.0, 5.0]] } } }"#,
        )
        .unwrap();
        std::sync::Arc::new(
            Scene3DPayload::from_manifest_fast(&manifest(nii_path, json_path)).unwrap(),
        )
    }

    pub(super) fn bundle(id: &str, n: usize) -> crate::scene_layers::FibreLayer {
        let mut points = Vec::new();
        let mut offsets = vec![0u32];
        for f in 0..n {
            for p in 0..5 {
                points.push([p as f32, f as f32, 0.0]);
            }
            offsets.push(points.len() as u32);
        }
        crate::scene_layers::FibreLayer {
            id: id.to_owned(),
            label: id.to_owned(),
            points,
            offsets,
            values: (0..n).map(|i| i as f32).collect(),
            colouring: crate::scene_layers::FibreColouring::Strength,
            total_fibres: n * 2,
        }
    }

    pub(super) fn wait_for_builds(payload: &std::sync::Arc<Scene3DPayload>) {
        let start = std::time::Instant::now();
        let mut now = 0.0;
        while payload.start_layer_builds_if_needed(now) {
            assert!(start.elapsed().as_secs() < 20, "layer build did not finish");
            std::thread::sleep(std::time::Duration::from_millis(5));
            now += 0.05;
        }
    }

    #[test]
    fn fibre_layers_build_off_thread_and_respect_the_maximum() {
        let payload = layer_payload("fibres");
        payload.upsert_fibre_layer(bundle("a", 30));
        wait_for_builds(&payload);
        let (_, geometry) = payload.fibre_geometry();
        let geometry = geometry.unwrap();
        assert_eq!(geometry.shown, vec![("a".to_owned(), 30)]);
        assert_eq!(geometry.segments.len(), 30 * 4);

        payload.with_layers(|l| l.max_fibres = 7);
        payload.touch_fibres();
        wait_for_builds(&payload);
        let geometry = payload.fibre_geometry().1.unwrap();
        assert_eq!(geometry.shown, vec![("a".to_owned(), 7)]);

        payload.with_layers(|l| l.fibre_settings.get_mut("a").unwrap().visible = false);
        payload.touch_fibres();
        wait_for_builds(&payload);
        assert!(payload.fibre_geometry().1.unwrap().segments.is_empty());
        payload.remove_fibre_layer("a");
        wait_for_builds(&payload);
        assert!(payload.fibre_geometry().1.unwrap().shown.is_empty());
    }

    #[test]
    fn map_isosurfaces_are_meshed_off_thread_and_slice_maps_are_not() {
        let payload = layer_payload("maps");
        let n = 16usize;
        let mut values = vec![0.0_f32; n * n * n];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let d2 = (i as f32 - 8.0).powi(2)
                        + (j as f32 - 8.0).powi(2)
                        + (k as f32 - 8.0).powi(2);
                    values[(k * n + j) * n + i] = (-d2 / 6.0).exp();
                }
            }
        }
        let map = crate::scene_layers::MapLayer::new(
            "m".to_owned(),
            "m".to_owned(),
            [n; 3],
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            values,
            crate::scene_layers::MapMode::Isosurface,
        );
        payload.upsert_map_layer(map);
        wait_for_builds(&payload);
        let meshes = payload.map_meshes().1.unwrap();
        assert_eq!(meshes.len(), 1);
        assert!(meshes[0].positive.indices.len() > 30);
        assert!(meshes[0].negative.positions.is_empty());

        payload.with_layers(|l| {
            l.map_settings.get_mut("m").unwrap().mode = crate::scene_layers::MapMode::Slices
        });
        payload.touch_maps();
        wait_for_builds(&payload);
        assert!(payload.map_meshes().1.unwrap().is_empty());
        assert!(payload.with_layers(|l| l.active_slice_map()).is_some());
    }

    fn blob_map(id: &str) -> crate::scene_layers::MapLayer {
        let n = 16usize;
        let mut values = vec![0.0_f32; n * n * n];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let d2 = (i as f32 - 8.0).powi(2)
                        + (j as f32 - 8.0).powi(2)
                        + (k as f32 - 8.0).powi(2);
                    values[(k * n + j) * n + i] = (-d2 / 6.0).exp();
                }
            }
        }
        crate::scene_layers::MapLayer::new(
            id.to_owned(),
            id.to_owned(),
            [n; 3],
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            values,
            crate::scene_layers::MapMode::Isosurface,
        )
    }

    #[test]
    fn removed_layers_are_reported_once_to_the_host() {
        let payload = layer_payload("removals");
        payload.upsert_fibre_layer(bundle("a", 3));
        payload.upsert_map_layer(blob_map("m"));
        payload.remove_fibre_layer("a");
        payload.remove_map_layer("m");
        payload.remove_map_layer("m");
        assert_eq!(
            payload.take_removed_layers(),
            (vec!["a".to_owned()], vec!["m".to_owned()])
        );
        assert_eq!(payload.take_removed_layers(), (Vec::new(), Vec::new()));
    }

    #[test]
    fn layer_settings_survive_a_scene_reload() {
        let old = layer_payload("old");
        old.upsert_fibre_layer(bundle("a", 3));
        old.upsert_map_layer(blob_map("m"));
        old.with_layers(|l| {
            l.fibre_settings.get_mut("a").unwrap().visible = false;
            let m = l.map_settings.get_mut("m").unwrap();
            m.visible = false;
            m.threshold = 0.5;
            m.mode = crate::scene_layers::MapMode::Slices;
        });
        let saved = old.layer_settings();
        let fresh = layer_payload("fresh");
        fresh.upsert_fibre_layer_shared(std::sync::Arc::new(bundle("a", 3)));
        fresh.upsert_map_layer_shared(std::sync::Arc::new(blob_map("m")));
        fresh.restore_layer_settings(&saved);
        assert!(!fresh.with_layers(|l| l.fibre_setting("a").visible));
        let m = fresh.with_layers(|l| l.map_settings["m"]);
        assert!(!m.visible);
        assert_eq!(m.threshold, 0.5);
        assert_eq!(m.mode, crate::scene_layers::MapMode::Slices);
    }

    #[test]
    fn isosurfaces_are_reused_until_their_threshold_or_data_changes() {
        let payload = layer_payload("mesh_cache");
        payload.upsert_map_layer(blob_map("m"));
        wait_for_builds(&payload);
        let first = std::sync::Arc::clone(&payload.map_meshes().1.unwrap()[0]);
        payload.touch_maps();
        wait_for_builds(&payload);
        let again = std::sync::Arc::clone(&payload.map_meshes().1.unwrap()[0]);
        assert!(std::sync::Arc::ptr_eq(&first, &again));
        payload.with_layers(|l| l.map_settings.get_mut("m").unwrap().threshold = 0.6);
        payload.touch_maps();
        wait_for_builds(&payload);
        let moved = std::sync::Arc::clone(&payload.map_meshes().1.unwrap()[0]);
        assert!(!std::sync::Arc::ptr_eq(&first, &moved));
    }

    #[test]
    fn the_fibre_row_counts_the_built_fibres_and_never_a_hidden_layer() {
        let mut row = super::FibreRow {
            id: "a".to_owned(),
            label: "a".to_owned(),
            shown: None,
            range: None,
            total: 900,
            uniform: false,
            signed: true,
            colour: [1.0; 4],
            setting: crate::scene_layers::FibreSettings::default(),
        };
        assert_eq!(super::fibre_count_text(&row), "building…");
        row.shown = Some(200);
        row.range = Some(crate::scene_fibres::SignRange {
            pos: Some((2.0, 5.0)),
            neg: Some((1.0, 3.0)),
        });
        assert_eq!(
            super::fibre_count_text(&row),
            "200 of 900 fibres shown, positive 2.000 to 5.000, negative -1.000 to -3.000"
        );
        row.setting.visible = false;
        assert_eq!(super::fibre_count_text(&row), "hidden, 900 fibres");
    }

    #[test]
    fn layer_updates_and_scene_state_locks_do_not_deadlock() {
        let payload = layer_payload("locks");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (payload, stop) = (payload.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut round = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    payload.upsert_fibre_layer(bundle("w", 3 + round % 4));
                    payload.remove_fibre_layer("w");
                    round += 1;
                }
            })
        };
        let reader = {
            let (payload, stop) = (payload.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let mut sh = payload.shared.lock().unwrap();
                    sh.bump();
                    let _ = payload.with_layers(|l| l.fibres.len());
                    let _ = payload.fibre_geometry();
                    let _ = payload.map_built_generation();
                }
            })
        };
        let start = std::time::Instant::now();
        let mut now = 0.0;
        while start.elapsed().as_millis() < 300 {
            payload.start_layer_builds_if_needed(now);
            now += 0.2;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();
        reader.join().unwrap();
        wait_for_builds(&payload);
    }

    #[test]
    fn payload_defaults_and_backdrop_swap_keep_world_positions() {
        let tmp = std::env::temp_dir().join(format!("dbsloc_scene3d_bd_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let nii_path = template(&tmp);
        let json_path = tmp.join("reco.json");
        std::fs::write(
            &json_path,
            r#"{ "version": 2, "meta": { "side_keys": "1=right,2=left" },
            "mni": { "2": { "trajectory": [[-3.0, 3.0, 4.0], [-4.0, 4.0, 5.0]] } } }"#,
        )
        .unwrap();
        let payload = Scene3DPayload::from_manifest_fast(&manifest(nii_path, json_path)).unwrap();
        {
            let sh = payload.shared.lock().unwrap();
            assert!(!sh.show_axial && sh.show_coronal && sh.show_sagittal);
        }
        let other = crate::scene_volume::TemplateVolume {
            n0: 40,
            n1: 40,
            n2: 40,
            hdr: nifti::NiftiHeader {
                sform_code: 1,
                srow_x: [1.0, 0.0, 0.0, -50.0],
                srow_y: [0.0, 1.0, 0.0, -50.0],
                srow_z: [0.0, 0.0, 1.0, -50.0],
                ..Default::default()
            },
            normed: vec![0.0; 40 * 40 * 40],
        };
        payload.shared.lock().unwrap().slice_window = 0.3;
        payload.set_backdrop(std::sync::Arc::new(other));
        let (generation, vol) = payload.backdrop_snapshot();
        assert_eq!(generation, 1);
        assert_eq!(vol.n0, 40);
        let sh = payload.shared.lock().unwrap();
        assert_eq!((sh.sagittal_i, sh.coronal_j, sh.axial_k), (20, 20, 20));
        assert_eq!((sh.slice_window, sh.slice_level), (1.0, 0.5));
        drop(sh);
        let app = super::Scene3DEframeApp::new("t".into(), payload);
        assert_eq!(app.current_backdrop_id(), "template-t1");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_unilateral_reconstruction_loads_with_one_side_empty() {
        let tmp = std::env::temp_dir().join(format!("dbsloc_scene3d_uni_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let nii_path = template(&tmp);
        let json_path = tmp.join("sub-TEST_desc-reconstruction.json");
        std::fs::write(
            &json_path,
            r#"{
            "version": 2,
            "meta": { "side_keys": "1=right,2=left", "sides_found": ["left"] },
            "mni": { "2": { "trajectory": [[-3.0, 3.0, 4.0], [-4.0, 4.0, 5.0]] } }
        }"#,
        )
        .unwrap();
        let payload = Scene3DPayload::from_manifest_fast(&manifest(nii_path.clone(), json_path))
            .unwrap_or_else(|e| panic!("from_manifest_fast: {e}"));
        assert_eq!(payload.points_l.len(), 2);
        assert!(payload.points_r.is_empty() && payload.contacts_r.is_empty());

        let empty_path = tmp.join("empty.json");
        std::fs::write(
            &empty_path,
            r#"{ "version": 2, "meta": { "side_keys": "1=right,2=left" }, "mni": {} }"#,
        )
        .unwrap();
        assert!(Scene3DPayload::from_manifest_fast(&manifest(nii_path, empty_path)).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn obj_atlas_manifest_loads_triangle_mesh() {
        let tmp = std::env::temp_dir().join(format!("dbsloc_scene3d_data_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let n = 8usize;
        let mut voxels = vec![0f32; n * n * n];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    voxels[i + j * n + k * n * n] = (i + j + k) as f32 / 21.0;
                }
            }
        }
        let nii_path = tmp.join("t1.nii");
        let mut f = std::fs::File::create(&nii_path).unwrap();
        f.write_all(&make_nifti1_bytes(n, n, n, &voxels)).unwrap();

        let json_path = tmp.join("sub-TEST_desc-reconstruction.json");
        std::fs::write(
            &json_path,
            r#"{
            "version": 2,
            "subj_id": "TEST",
            "method": "test",
            "meta": { "side_keys": "1=right,2=left" },
            "mni": {
                "1": { "trajectory": [[2.0, 2.0, 4.0], [5.0, 5.0, 4.0]] },
                "2": { "trajectory": [[3.0, 3.0, 4.0], [4.0, 4.0, 4.0]] }
            }
        }"#,
        )
        .unwrap();

        let obj_dir = tmp.join("atlas_obj");
        std::fs::create_dir_all(&obj_dir).unwrap();
        std::fs::write(
            obj_dir.join("stn.obj"),
            "v 2.0 4.0 4.0\nv 6.0 4.0 4.0\nv 4.0 6.0 4.0\nf 1 2 3\n",
        )
        .unwrap();

        let man = SceneManifestV1 {
            manifest_version: 1,
            subj_id: "TEST".into(),
            space: "test".into(),
            template_nii: nii_path,
            reco_json: json_path,
            atlas_obj_dir: Some(obj_dir),
            atlas_nii: None,
            atlas_max_labels: None,
            atlas_lead_sets: vec![],
            atlas_lead_nii_dir: None,
            atlas_default_on_substrings: None,
            atlas_mesh_smooth_iters: 0,
            atlas_lead_resample_ijk_subtract: 0.0,
            atlas_lead_native_mc_ijk_shift: 0.0,
            atlas_lead_lazy_load: true,
            atlas_cache_precompute: true,
            atlas_cache_dir_name: "precomputed_obj".into(),
            atlas_cache_only_default_on: true,
            atlas_mesh_weld_eps_mm: 0.05,
            atlas_mesh_preblur_iters: 6,
            atlas_mesh_morph_close_iters: 0,
            atlas_mesh_resample_to_template: false,
            atlas_unlit: false,
            atlas_flat_normals: false,
            atlas_mesh_taubin_iters: 10,
            atlas_mesh_taubin_lambda: 0.5,
            atlas_mesh_taubin_mu: -0.53,
            subj_dir: None,
            backdrops: vec![],
            vta: vec![],
            group: None,
        };

        let payload =
            Scene3DPayload::from_manifest(&man).unwrap_or_else(|e| panic!("from_manifest: {e}"));

        let groups = payload.atlas_groups.lock().unwrap();
        let gm = groups
            .iter()
            .flat_map(|g| g.meshes.iter())
            .find(|m| m.mesh.is_some())
            .expect("expected at least one OBJ atlas mesh");
        let mesh = gm.mesh.as_ref().unwrap();
        assert!(
            mesh.indices.len() >= 3,
            "OBJ ROI should yield triangle indices"
        );
    }
}

#[cfg(test)]
mod default_slice_tests {
    use glam::Vec3;
    use nifti::NiftiHeader;

    use super::*;
    use dbsloc_checkreg::nifti_io::make_nifti1_bytes;

    fn volume(res: f32, origin: [f32; 3], dims: (usize, usize, usize)) -> TemplateVolume {
        TemplateVolume {
            n0: dims.0,
            n1: dims.1,
            n2: dims.2,
            hdr: NiftiHeader {
                sform_code: 1,
                srow_x: [res, 0.0, 0.0, origin[0]],
                srow_y: [0.0, res, 0.0, origin[1]],
                srow_z: [0.0, 0.0, res, origin[2]],
                ..Default::default()
            },
            normed: Vec::new(),
        }
    }

    fn one_mm() -> TemplateVolume {
        volume(1.0, [-98.0, -134.0, -72.0], (197, 233, 189))
    }

    fn half_mm() -> TemplateVolume {
        volume(0.5, [-98.2, -134.2, -72.2], (395, 467, 379))
    }

    pub(super) fn shared_for_tests() -> SceneShared {
        shared_state()
    }

    fn shared_state() -> SceneShared {
        SceneShared {
            azimuth: 0.0,
            elevation: 0.0,
            projection: Projection::default(),
            distance: 100.0,
            target: Vec3::ZERO,
            axial_k: 0,
            coronal_j: 0,
            sagittal_i: 0,
            show_axial: true,
            show_coronal: false,
            show_sagittal: false,
            axial_opacity: 1.0,
            coronal_opacity: 1.0,
            sagittal_opacity: 1.0,
            atlas_opacity: 1.0,
            atlas_overlay_on: true,
            active_atlas_group: 0,
            atlas_alpha: vec![],
            show_atlas: vec![],
            atlas_rgb: vec![],
            atlas_vectors_group: None,
            atlas_id: None,
            slice_window: 1.0,
            slice_level: 0.5,
            show_electrodes: true,
            show_vta: true,
            show_contact_labels: false,
            revision: 1,
            slice_world: DEFAULT_SLICE_MM,
        }
    }

    #[test]
    fn one_mm_grid_defaults() {
        assert_eq!(default_slice_indices(&one_mm()), (68, 104, 42));
    }

    #[test]
    fn half_mm_grid_defaults() {
        assert_eq!(default_slice_indices(&half_mm()), (136, 208, 84));
    }

    #[test]
    fn outside_extent_uses_the_centre_index_per_axis() {
        let cropped = volume(0.2, [-49.85, -59.85, -39.85], (500, 475, 375));
        assert_eq!(
            slice_indices_for_world(&cropped, [-30.0, -70.0, 100.0]),
            (99, 237, 187)
        );
        let (i, _, _) = slice_indices_for_world(&cropped, [-30.0, -30.0, -30.0]);
        assert_eq!(i, 99);
        assert_eq!(
            slice_indices_for_world(&one_mm(), [500.0, -900.0, 1000.0]),
            (98, 116, 94)
        );
    }

    #[test]
    fn world_mm_of_a_plane_round_trips() {
        let v = one_mm();
        assert!((plane_world_mm(&v, 0, 68) + 30.0).abs() < 1e-4);
        assert!((plane_world_mm(&v, 1, 104) + 30.0).abs() < 1e-4);
        assert!((plane_world_mm(&v, 2, 42) + 30.0).abs() < 1e-4);
    }

    #[test]
    fn minus_sign_is_typographic() {
        assert_eq!(format_signed_mm(-30.0), "\u{2212}30.0");
        assert_eq!(format_signed_mm(-0.04), "0.0");
        assert_eq!(format_signed_mm(12.34), "12.3");
    }

    #[test]
    fn reset_view_shows_sagittal_and_coronal_at_the_default_point() {
        let mut sh = shared_state();
        let d = default_view_defaults(&one_mm(), 0, 0, 0);
        reset_view(&mut sh, &d, &half_mm());
        assert!(!sh.show_axial && sh.show_coronal && sh.show_sagittal);
        assert_eq!((sh.sagittal_i, sh.coronal_j, sh.axial_k), (136, 208, 84));
    }

    #[test]
    fn snapshot_without_slice_convention_keeps_default_slices() {
        let mut sh = shared_state();
        let v = one_mm();
        sh.set_slices_to_world(&v, DEFAULT_SLICE_MM);
        sh.show_axial = false;
        sh.show_coronal = true;
        sh.show_sagittal = true;
        let snap = SceneViewState {
            axial_k: 5,
            coronal_j: 6,
            sagittal_i: 7,
            show_axial: true,
            show_coronal: false,
            show_sagittal: false,
            ..Default::default()
        };
        sh.restore(&snap);
        sh.restore_slices(&snap, &v);
        assert_eq!((sh.sagittal_i, sh.coronal_j, sh.axial_k), (68, 104, 42));
        assert!(!sh.show_axial && sh.show_coronal && sh.show_sagittal);
    }

    #[test]
    fn snapshot_with_slice_convention_maps_world_mm_onto_another_grid() {
        let mut sh = shared_state();
        let one = one_mm();
        sh.set_slices_to_world(&one, [-12.0, 8.0, -20.0]);
        sh.show_axial = true;
        sh.show_coronal = true;
        sh.show_sagittal = false;
        let snap = sh.snapshot();
        assert_eq!(snap.slice_convention, 1);
        let mm = snap.slice_mm.unwrap();
        assert!((mm[0] + 12.0).abs() < 1e-4 && (mm[1] - 8.0).abs() < 1e-4);
        let mut other = shared_state();
        other.restore_slices(&snap, &half_mm());
        assert_eq!(
            (other.sagittal_i, other.coronal_j, other.axial_k),
            (172, 284, 104)
        );
        assert!(other.show_axial && other.show_coronal && !other.show_sagittal);
    }

    #[test]
    fn old_view_state_json_still_loads() {
        let old = r#"{"camera_convention":1,"azimuth":1.0,"axial_k":3,"show_axial":true}"#;
        let s: SceneViewState = serde_json::from_str(old).unwrap();
        assert_eq!(s.slice_convention, 0);
        assert!(s.slice_mm.is_none() && s.backdrop_id.is_none());
        assert_eq!(s.axial_k, 3);
        let round: SceneViewState =
            serde_json::from_str(&serde_json::to_string(&SceneViewState::default()).unwrap())
                .unwrap();
        assert_eq!(round.slice_convention, 0);
    }

    fn switch_to(sh: &mut SceneShared, to: &TemplateVolume) {
        let mm = sh.slice_world;
        sh.set_slices_to_world(to, mm);
    }

    #[test]
    fn a_crop_round_trip_restores_the_plane() {
        let mut sh = shared_state();
        let one = one_mm();
        sh.set_slices_to_world(&one, DEFAULT_SLICE_MM);
        let cropped = volume(0.2, [-49.85, -9.85, -39.85], (500, 475, 375));
        switch_to(&mut sh, &cropped);
        assert_eq!(sh.coronal_j, 237);
        switch_to(&mut sh, &one);
        assert_eq!((sh.sagittal_i, sh.coronal_j, sh.axial_k), (68, 104, 42));
        assert_eq!(sh.slice_world, DEFAULT_SLICE_MM);
    }

    #[test]
    fn repeated_grid_switches_do_not_drift() {
        let mut sh = shared_state();
        let one = one_mm();
        let half = half_mm();
        sh.set_slices_to_world(&one, DEFAULT_SLICE_MM);
        for _ in 0..10 {
            switch_to(&mut sh, &half);
            switch_to(&mut sh, &one);
        }
        assert_eq!((sh.sagittal_i, sh.coronal_j, sh.axial_k), (68, 104, 42));
        assert_eq!(sh.slice_world, DEFAULT_SLICE_MM);
    }

    #[test]
    fn only_user_moved_axes_update_the_stored_world_position() {
        let mut sh = shared_state();
        let half = half_mm();
        sh.set_slices_to_world(&half, DEFAULT_SLICE_MM);
        let before = (sh.sagittal_i, sh.coronal_j, sh.axial_k);
        let world = sh.slice_world;
        sh.coronal_j += 10;
        sh.note_slice_edits(&half, before, world);
        assert_eq!(sh.slice_world[0], -30.0);
        assert_eq!(sh.slice_world[2], -30.0);
        assert!((sh.slice_world[1] - -25.2).abs() < 1e-3);
        let before = (sh.sagittal_i, sh.coronal_j, sh.axial_k);
        let world = sh.slice_world;
        let d = default_view_defaults(&half, 0, 0, 0);
        reset_view(&mut sh, &d, &half);
        sh.note_slice_edits(&half, before, world);
        assert_eq!(sh.slice_world, DEFAULT_SLICE_MM);
    }

    #[test]
    fn axis_order_check_refuses_permuted_headers() {
        assert!(axes_in_xyz_order(&one_mm().hdr));
        let flipped = NiftiHeader {
            sform_code: 1,
            srow_x: [-1.0, 0.0, 0.0, 90.0],
            srow_y: [0.0, 1.0, 0.0, -100.0],
            srow_z: [0.0, 0.0, -1.0, 80.0],
            ..Default::default()
        };
        assert!(axes_in_xyz_order(&flipped));
        let permuted = NiftiHeader {
            sform_code: 1,
            srow_x: [0.0, 1.0, 0.0, 0.0],
            srow_y: [1.0, 0.0, 0.0, 0.0],
            srow_z: [0.0, 0.0, 1.0, 0.0],
            ..Default::default()
        };
        assert!(!axes_in_xyz_order(&permuted));
    }

    #[test]
    fn a_permuted_file_is_refused_with_the_exact_message() {
        let dir = std::env::temp_dir().join(format!("dbsloc_axes_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("perm.nii");
        let n = 4usize;
        let voxels = vec![1.0_f32; n * n * n];
        let mut bytes = make_nifti1_bytes(n, n, n, &voxels);
        let rows: [[f32; 4]; 3] = [
            [0.0, 1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let off = 280 + r * 16 + c * 4;
                bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        bytes[254..256].copy_from_slice(&1i16.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let err = load_backdrop_volume(&path).err().unwrap();
        assert_eq!(err, AXES_ORDER_ERROR);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fake_load(id: &str) -> BackdropLoad {
        let (_tx, rx) = mpsc::channel();
        BackdropLoad {
            id: id.to_string(),
            rx,
            stale: false,
        }
    }

    #[test]
    fn backdrop_queue_keeps_one_load_and_the_latest_choice() {
        let mut ui = BackdropUi::with_choices(Vec::new());
        ui.choose("a", PathBuf::from("a"));
        let (id, _) = ui.next_to_spawn().unwrap();
        ui.load = Some(fake_load(&id));
        assert!(ui.next_to_spawn().is_none());
        ui.choose("b", PathBuf::from("b"));
        ui.choose("c", PathBuf::from("c"));
        assert!(ui.next_to_spawn().is_none());
        assert_eq!(ui.saved_id(), "c");
        assert_eq!(ui.accept_result(), None);
        assert_eq!(ui.next_to_spawn().unwrap().0, "c");
    }

    #[test]
    fn choosing_the_shown_or_template_backdrop_drops_the_in_flight_result() {
        let mut ui = BackdropUi::with_choices(Vec::new());
        ui.current_id = "shown".into();
        ui.load = Some(fake_load("x"));
        ui.choose("shown", PathBuf::from("s"));
        assert!(ui.queued.is_none());
        assert_eq!(ui.accept_result(), None);

        ui.load = Some(fake_load("x"));
        ui.choose_template();
        assert_eq!(ui.current_id, TEMPLATE_BACKDROP_ID);
        assert_eq!(ui.accept_result(), None);

        ui.load = Some(fake_load("x"));
        ui.choose("x", PathBuf::from("x"));
        assert!(ui.queued.is_none());
        assert_eq!(ui.accept_result(), Some("x".to_string()));
    }

    #[test]
    fn a_stale_choice_of_the_loading_id_is_revived_not_reloaded() {
        let mut ui = BackdropUi::with_choices(Vec::new());
        ui.current_id = "shown".into();
        ui.load = Some(fake_load("x"));
        ui.choose("shown", PathBuf::from("s"));
        ui.choose("x", PathBuf::from("x"));
        assert!(ui.queued.is_none());
        assert_eq!(ui.accept_result(), Some("x".to_string()));
    }

    #[test]
    fn view_state_keeps_a_pending_backdrop_id() {
        let tmp = std::env::temp_dir().join(format!("dbsloc_pending_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let n = 8usize;
        let nii = tmp.join("t1.nii");
        std::fs::write(&nii, make_nifti1_bytes(n, n, n, &vec![1.0_f32; n * n * n])).unwrap();
        let reco = tmp.join("reco.json");
        std::fs::write(
            &reco,
            r#"{ "version": 2, "meta": { "side_keys": "1=right,2=left" },
            "mni": { "2": { "trajectory": [[-3.0, 3.0, 4.0], [-4.0, 4.0, 5.0]] } } }"#,
        )
        .unwrap();
        let mut man = SceneManifestV1 {
            backdrops: vec![],
            vta: vec![],
            ..serde_json::from_str(
                r#"{ "subj_id": "x", "template_nii": "t.nii", "reco_json": "r.json" }"#,
            )
            .unwrap()
        };
        man.template_nii = nii;
        man.reco_json = reco;
        let payload = Scene3DPayload::from_manifest_fast(&man).unwrap();
        let mut app = Scene3DEframeApp::new("t".into(), payload);
        let state = SceneViewState {
            backdrop_id: Some("later".into()),
            ..Default::default()
        };
        app.restore_view_state(&state);
        assert_eq!(app.view_state().backdrop_id.as_deref(), Some("later"));
        assert_eq!(app.current_backdrop_id(), TEMPLATE_BACKDROP_ID);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn world_bounds_overlap_detects_disjoint_boxes() {
        let a = (Vec3::splat(0.0), Vec3::splat(10.0));
        assert!(world_bounds_overlap(
            a,
            (Vec3::splat(5.0), Vec3::splat(20.0))
        ));
        assert!(!world_bounds_overlap(
            a,
            (Vec3::splat(11.0), Vec3::splat(20.0))
        ));
    }
}

#[cfg(test)]
mod panel_width_tests {
    use super::*;

    fn panel_width_after(frames: usize) -> Vec<f32> {
        let ctx = egui::Context::default();
        let mut azimuth = -137.6_f32;
        let mut distance = 1234.5_f32;
        let mut opacity = 1.0_f32;
        let mut index = 255_u32;
        let mut widths = Vec::new();
        for _ in 0..frames {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                egui::Panel::right("panel_width_test")
                    .default_size(150.0)
                    .min_size(100.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let col = label_column(
                                    ui,
                                    &[
                                        ("Azimuth (°)", false),
                                        ("Distance (mm)", false),
                                        ("Opacity", false),
                                        ("Index", false),
                                    ],
                                );
                                labelled_slider(
                                    ui,
                                    RowLabel::new("Azimuth (°)", col),
                                    egui::Slider::new(&mut azimuth, -180.0..=180.0)
                                        .fixed_decimals(0),
                                    "-180",
                                    "Azimuth",
                                );
                                labelled_slider(
                                    ui,
                                    RowLabel::new("Distance (mm)", col),
                                    egui::Slider::new(&mut distance, 20.0..=2000.0)
                                        .logarithmic(true)
                                        .fixed_decimals(0),
                                    "2000",
                                    "Distance",
                                );
                                labelled_slider(
                                    ui,
                                    RowLabel::new("Opacity", col),
                                    Scene3DEframeApp::opacity_slider_widget(&mut opacity),
                                    "100%",
                                    "Opacity",
                                );
                                labelled_slider(
                                    ui,
                                    RowLabel::new("Index", col),
                                    egui::Slider::new(&mut index, 0..=255),
                                    "255",
                                    "Index",
                                );
                            });
                    });
            })
            .textures_delta
            .clear();
            let state =
                egui::containers::panel::PanelState::load(&ctx, egui::Id::new("panel_width_test"))
                    .expect("panel state stored");
            widths.push(state.outer_rect.width());
        }
        widths
    }

    #[test]
    fn trajectory_line_is_drawn_only_without_a_mesh() {
        let pts = [Vec3::ZERO, Vec3::X, Vec3::new(2.0, 0.0, 0.0)];
        assert!(fallback_line_vertices(&pts, false).is_empty());
        assert_eq!(fallback_line_vertices(&pts, true).len(), 4);
        assert!(fallback_line_vertices(&pts[..1], true).is_empty());
    }

    #[test]
    fn inline_slider_needs_room_after_the_label_column() {
        assert!(inline_slider_fits(300.0, 80.0, 8.0, 14.0));
        assert!(!inline_slider_fits(150.0, 80.0, 8.0, 14.0));
        assert!(inline_slider_fits(
            80.0 + 8.0 + 14.0 * MIN_INLINE_SLIDER_EMS,
            80.0,
            8.0,
            14.0
        ));
        assert!(!inline_slider_fits(100.0, 100.0, 8.0, 14.0));
    }

    #[test]
    fn control_panel_rows_never_widen_the_panel() {
        let widths = panel_width_after(5);
        eprintln!("panel widths per frame: {widths:?}");
        assert!(
            widths[4] <= 150.0 + 1.0,
            "panel widened to {} px: {widths:?}",
            widths[4]
        );
        assert!(
            (widths[4] - widths[1]).abs() < 0.01,
            "panel width drifts: {widths:?}"
        );
    }
}

#[cfg(test)]
mod camera_tests {
    use glam::{Vec3, Vec4};
    use nifti::NiftiHeader;

    use super::*;

    fn shared(azimuth_deg: f32, elevation_deg: f32, projection: Projection) -> SceneShared {
        SceneShared {
            azimuth: azimuth_deg.to_radians(),
            elevation: elevation_deg.to_radians(),
            projection,
            distance: 100.0,
            target: Vec3::new(1.0, 2.0, 3.0),
            axial_k: 0,
            coronal_j: 0,
            sagittal_i: 0,
            show_axial: false,
            show_coronal: true,
            show_sagittal: true,
            axial_opacity: 1.0,
            coronal_opacity: 1.0,
            sagittal_opacity: 1.0,
            atlas_opacity: 1.0,
            atlas_overlay_on: true,
            active_atlas_group: 0,
            atlas_alpha: vec![],
            show_atlas: vec![],
            atlas_rgb: vec![],
            atlas_vectors_group: None,
            atlas_id: None,
            slice_window: 1.0,
            slice_level: 0.5,
            show_electrodes: true,
            show_vta: true,
            show_contact_labels: false,
            revision: 1,
            slice_world: DEFAULT_SLICE_MM,
        }
    }

    fn volume(origin: [f32; 3], n: usize) -> TemplateVolume {
        TemplateVolume {
            n0: n,
            n1: n,
            n2: n,
            hdr: NiftiHeader {
                sform_code: 1,
                srow_x: [1.0, 0.0, 0.0, origin[0]],
                srow_y: [0.0, 1.0, 0.0, origin[1]],
                srow_z: [0.0, 0.0, 1.0, origin[2]],
                ..Default::default()
            },
            normed: vec![0.0; n * n * n],
        }
    }

    fn assert_close(a: Vec3, b: Vec3) {
        assert!((a - b).length() < 1e-4, "{a:?} vs {b:?}");
    }

    #[test]
    fn orbit_eye_placements() {
        let t = Vec3::new(1.0, 2.0, 3.0);
        let d = 100.0;
        let cases = [
            (0.0, 0.0, Vec3::new(0.0, -d, 0.0)),
            (90.0, 0.0, Vec3::new(d, 0.0, 0.0)),
            (180.0, 0.0, Vec3::new(0.0, d, 0.0)),
            (0.0, 90.0, Vec3::new(0.0, 0.0, d)),
            (90.0, 90.0, Vec3::new(0.0, 0.0, d)),
            (180.0, 90.0, Vec3::new(0.0, 0.0, d)),
        ];
        for (az, el, off) in cases {
            let sh = shared(az, el, Projection::Perspective);
            assert_close(orbit_eye(&sh), t + off);
        }
    }

    #[test]
    fn view_forward_points_from_the_eye_to_the_target_in_both_projections() {
        for projection in [Projection::Orthographic, Projection::Perspective] {
            let sh = shared(30.0, 20.0, projection);
            let fwd = view_forward(&sh);
            assert!((fwd.length() - 1.0).abs() < 1e-5);
            let expected = (sh.target - camera_eye(&sh)).normalize();
            assert_close(fwd, expected);
        }
        let sh = shared(180.0, 0.0, Projection::Orthographic);
        assert_close(view_forward(&sh), Vec3::NEG_Y);
    }

    #[test]
    fn orthographic_framing_matches_perspective() {
        let half_h = 100.0 * VIEW_HALF_FOV_RAD.tan();
        for projection in [Projection::Orthographic, Projection::Perspective] {
            let sh = shared(180.0, 21.0, projection);
            let (vp, eye) = orbit_vp(&sh, 1.5);
            let c = vp * Vec4::new(sh.target.x, sh.target.y, sh.target.z, 1.0);
            assert!((c.x / c.w).abs() < 1e-4 && (c.y / c.w).abs() < 1e-4);
            let (_, up) = orbit_screen_basis(&sh);
            let p = sh.target + up * half_h;
            let c = vp * Vec4::new(p.x, p.y, p.z, 1.0);
            assert!(
                (c.y / c.w - 1.0).abs() < 1e-3,
                "{projection:?}: {}",
                c.y / c.w
            );
            assert_eq!(eye, camera_eye(&sh));
        }
        let sh = shared(180.0, 21.0, Projection::Orthographic);
        assert!((camera_eye(&sh) - sh.target).length() - ORTHO_EYE_MM < 1e-2);
    }

    #[test]
    fn default_target_inside_bounds_is_lead_dbs_point() {
        let vol = volume([-98.0, -134.0, -72.0], 200);
        assert_close(
            default_view_target(&vol),
            Vec3::from_array(LEAD_DBS_DEFAULT_TARGET_MM),
        );
    }

    #[test]
    fn default_target_outside_bounds_is_template_center() {
        let vol = volume([100.0, 100.0, 100.0], 50);
        assert_close(default_view_target(&vol), template_world_center(&vol));
        assert_close(template_world_center(&vol), Vec3::new(124.5, 124.5, 124.5));
    }

    #[test]
    fn default_distance_frames_visible_height() {
        let d = default_view_distance();
        assert!(
            (2.0 * d * VIEW_HALF_FOV_RAD.tan() - LEAD_DBS_DEFAULT_VISIBLE_HEIGHT_MM).abs() < 1e-3
        );
        assert!((d - 95.7).abs() < 0.1);
    }

    #[test]
    fn restore_ignores_camera_of_old_snapshot() {
        let mut sh = shared(10.0, 20.0, Projection::Orthographic);
        let before = (sh.azimuth, sh.elevation, sh.distance, sh.target);
        let mut snap = SceneViewState {
            azimuth: 1.0,
            elevation: 0.5,
            projection: Projection::Perspective,
            distance: 42.0,
            target: [9.0, 9.0, 9.0],
            slice_window: 0.5,
            ..Default::default()
        };
        sh.restore(&snap);
        assert_eq!(before, (sh.azimuth, sh.elevation, sh.distance, sh.target));
        assert_eq!(sh.projection, Projection::Orthographic);
        assert!((sh.slice_window - 0.5).abs() < 1e-6);

        snap.camera_convention = 1;
        sh.restore(&snap);
        assert!((sh.azimuth - 1.0).abs() < 1e-6);
        assert!((sh.elevation - 0.5).abs() < 1e-6);
        assert!((sh.distance - 42.0).abs() < 1e-6);
        assert_eq!(sh.target, Vec3::splat(9.0));
        assert_eq!(sh.projection, Projection::Perspective);
        assert_eq!(sh.snapshot().camera_convention, 1);
    }
}

#[cfg(test)]
mod oit_render_tests;
