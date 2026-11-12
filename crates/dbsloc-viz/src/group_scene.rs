// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The group 3D scene (`specs/14` §5): every patient's leads in template space, coloured by
//! group or by a clinical variable, with instanced lead bodies and contact markers.
//!
//! This module holds the CPU model (manifest → leads, colours, instances, legend, picking); the
//! renderer in `scene3d_gpu` uploads one mesh per electrode model and one instance per lead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::atlas_wavefront::TriMeshScene;
use crate::electrode_mesh::{canonical_electrode, lead_instance_matrix};
use crate::error::{VizError, VizResult};
use crate::reco_json::load_block_side_geometry_opt;
use crate::scene_manifest::{GroupBlockV2, GroupColourByV2, GroupShowV2, SceneManifestV1};

/// Active contacts (`specs/14` §5.2).
pub const ACTIVE_RGB: [f32; 3] = [0.9, 0.1, 0.1];
/// Pale edge of an active contact.
pub const ACTIVE_EDGE_RGB: [f32; 3] = [0.9, 0.9, 0.7];
/// Opacity of the leads that are not selected.
pub const DIMMED_ALPHA: f32 = 0.3;
/// Yellow of a VTA surface and its opacity (`specs/14` §5.2).
pub const VTA_RGB: [f32; 3] = [0.95, 0.80, 0.15];
pub const VTA_ALPHA: f32 = 0.35;
/// Radius (mm) of a contact marker; inactive contacts take half of it.
pub const MARKER_RADIUS_MM: f32 = 0.9;
/// Radius (mm) of a point of the clinical point cloud.
pub const POINT_RADIUS_MM: f32 = 0.8;

const INSULATION_RGB: [f32; 3] = [0.20, 0.21, 0.24];
const METAL_RGB: [f32; 3] = [0.80, 0.82, 0.86];
const INSULATION_GROUP_FACTOR: f32 = 0.8;

/// One lead of a group scene.
#[derive(Debug, Clone)]
pub struct GroupLead {
    /// Name in the manifest (a mirrored copy ends with `_mirrored`).
    pub subject: String,
    /// The patient the lead belongs to (the source of a mirrored copy).
    pub patient: String,
    pub mirrored: bool,
    pub group: u32,
    pub group_label: String,
    /// 0 = right, 1 = left.
    pub side: usize,
    pub model: Option<String>,
    pub head: Vec3,
    pub tail: Vec3,
    pub x_ref: Option<Vec3>,
    pub y_ref: Option<Vec3>,
    pub contacts: Vec<Vec3>,
    pub active: Vec<bool>,
    /// One value per contact, or per contact pair (`pair_values`); NaN = missing.
    pub values: Option<Vec<f32>>,
    pub pair_values: bool,
    pub vta: Option<PathBuf>,
}

/// A drawn group: number, label, colour.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupInfo {
    pub number: u32,
    pub label: String,
    pub color: [f32; 3],
}

/// What the leads are coloured by.
#[derive(Debug, Clone, PartialEq)]
pub enum ColourMode {
    Group,
    Clinical {
        name: String,
        normalisation: String,
        range: [f32; 2],
    },
}

/// A contact (or the midpoint of a contact pair) with its state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactPoint {
    pub pos: Vec3,
    pub active: bool,
    pub value: f32,
}

/// Interactive display state of a group scene.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupDisplay {
    pub show_active: bool,
    pub show_passive: bool,
    pub highlight_active: bool,
    pub show_vtas: bool,
    pub point_cloud: bool,
    pub selected_patient: Option<String>,
    pub selected_group: Option<u32>,
}

impl GroupDisplay {
    /// The display state a manifest asks for.
    pub fn from_show(show: &GroupShowV2) -> GroupDisplay {
        GroupDisplay {
            show_active: show.active_contacts,
            show_passive: show.passive_contacts,
            highlight_active: show.highlight_active,
            show_vtas: show.vtas,
            point_cloud: show.point_cloud,
            selected_patient: None,
            selected_group: None,
        }
    }
}

/// How a lead is emphasised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Normal,
    Selected,
    Dimmed,
}

/// Part of a lead drawn from a shared mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Part {
    Insulation,
    Metal,
    Sphere,
}

/// One instance: rows of the affine model → world map, colour and extra parameters
/// (`extra.x` = rim strength of a contact marker).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Instance {
    pub m0: [f32; 4],
    pub m1: [f32; 4],
    pub m2: [f32; 4],
    pub colour: [f32; 4],
    pub extra: [f32; 4],
}

/// Instances of one mesh: opaque ones and the dimmed ones drawn through the transparency pass.
#[derive(Debug, Clone, Default)]
pub struct Batch {
    pub model: Option<String>,
    pub part: Option<Part>,
    pub opaque: Vec<Instance>,
    pub dimmed: Vec<Instance>,
}

/// A range of the shared model mesh buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshRange {
    pub first_index: u32,
    pub index_count: u32,
    pub base_vertex: i32,
}

/// The shared meshes of a group scene: one per electrode model and one marker sphere.
#[derive(Debug, Clone, Default)]
pub struct GroupModels {
    pub names: Vec<String>,
    pub insulation: Vec<Option<MeshRange>>,
    pub metal: Vec<Option<MeshRange>>,
    pub sphere: Option<MeshRange>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl GroupModels {
    /// Builds the meshes of every model in `names` that the catalogue knows, and the sphere.
    pub fn build(names: &[String]) -> GroupModels {
        let mut out = GroupModels::default();
        for name in names {
            let group = dbsloc_reconstruct::electrode_by_name(name).and_then(canonical_electrode);
            out.names.push(name.clone());
            match group {
                Some(g) => {
                    let ins = out.append(&g.insulation);
                    let metal = out.append(&g.metal);
                    out.insulation.push(ins);
                    out.metal.push(metal);
                }
                None => {
                    out.insulation.push(None);
                    out.metal.push(None);
                }
            }
        }
        out.sphere = out.append(&unit_sphere(12, 18));
        out
    }

    fn append(&mut self, mesh: &TriMeshScene) -> Option<MeshRange> {
        if mesh.indices.is_empty() {
            return None;
        }
        let base_vertex = i32::try_from(self.positions.len()).ok()?;
        let first_index = u32::try_from(self.indices.len()).ok()?;
        self.positions
            .extend(mesh.positions.iter().map(|p| p.to_array()));
        self.normals.extend(
            (0..mesh.positions.len())
                .map(|i| mesh.normals.get(i).copied().unwrap_or(Vec3::Y).to_array()),
        );
        self.indices.extend_from_slice(&mesh.indices);
        Some(MeshRange {
            first_index,
            index_count: u32::try_from(mesh.indices.len()).ok()?,
            base_vertex,
        })
    }

    /// Index of a model in [`GroupModels::names`] that has a body mesh.
    pub fn body_of(&self, name: &str) -> Option<usize> {
        let i = self.names.iter().position(|n| n == name)?;
        (self.insulation[i].is_some() || self.metal[i].is_some()).then_some(i)
    }

    /// Number of model body meshes uploaded.
    pub fn mesh_count(&self) -> usize {
        self.insulation
            .iter()
            .zip(&self.metal)
            .filter(|(a, b)| a.is_some() || b.is_some())
            .count()
    }
}

/// A unit sphere with `lat` latitude bands and `lon` longitude segments.
pub fn unit_sphere(lat: usize, lon: usize) -> TriMeshScene {
    let mut positions = Vec::new();
    for i in 0..=lat {
        let theta = std::f32::consts::PI * i as f32 / lat as f32;
        for j in 0..=lon {
            let phi = std::f32::consts::TAU * j as f32 / lon as f32;
            positions.push(Vec3::new(
                theta.sin() * phi.cos(),
                theta.sin() * phi.sin(),
                theta.cos(),
            ));
        }
    }
    let normals = positions.clone();
    let mut indices = Vec::new();
    let stride = (lon + 1) as u32;
    for i in 0..lat as u32 {
        for j in 0..lon as u32 {
            let a = i * stride + j;
            let b = a + stride;
            indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    TriMeshScene {
        positions,
        normals,
        indices,
    }
}

/// Blue → white → red over `t` in [0, 1] (`specs/14` §5.2).
pub fn blue_white_red(t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        let u = t * 2.0;
        [u, u, 1.0]
    } else {
        let u = (t - 0.5) * 2.0;
        [1.0, 1.0 - u, 1.0 - u]
    }
}

/// Position of `v` in `range` (0..1); a flat range maps to the middle. NaN stays NaN.
pub fn unit_position(v: f32, range: [f32; 2]) -> f32 {
    if !v.is_finite() {
        return f32::NAN;
    }
    let span = range[1] - range[0];
    if span > 1e-12 {
        ((v - range[0]) / span).clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// The colour of a value, `None` when it is missing.
pub fn value_colour(v: f32, range: [f32; 2]) -> Option<[f32; 3]> {
    let t = unit_position(v, range);
    t.is_finite().then(|| blue_white_red(t))
}

/// What the legend shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Legend {
    Groups(Vec<(String, [f32; 3])>),
    ColourBar {
        name: String,
        normalisation: String,
        range: [f32; 2],
    },
}

/// The nearest lead under a screen position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickHit {
    pub lead: usize,
    pub point: Option<usize>,
    pub distance_px: f32,
}

/// The group scene model.
#[derive(Debug, Clone)]
pub struct GroupScene {
    pub guid: String,
    pub title: String,
    pub leads: Vec<GroupLead>,
    pub groups: Vec<GroupInfo>,
    pub colour: ColourMode,
    pub show: GroupShowV2,
    pub mirror: bool,
    pub centre: Vec3,
    pub warnings: Vec<String>,
    pub cache_dir: PathBuf,
}

fn to_vec3(p: [f64; 3]) -> Vec3 {
    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

fn default_group_colour(number: u32) -> [f32; 3] {
    const PALETTE: [[f32; 3]; 8] = [
        [0.36, 0.62, 0.84],
        [0.93, 0.55, 0.28],
        [0.46, 0.74, 0.45],
        [0.80, 0.45, 0.70],
        [0.85, 0.75, 0.30],
        [0.45, 0.72, 0.72],
        [0.70, 0.55, 0.40],
        [0.60, 0.60, 0.75],
    ];
    PALETTE[(number.max(1) as usize - 1) % PALETTE.len()]
}

impl GroupScene {
    /// Reads every lead of a version-2 manifest. A patient whose reconstruction cannot be read is left out with a warning; no lead at all is an error.
    pub fn from_manifest(m: &SceneManifestV1) -> VizResult<GroupScene> {
        let group = m
            .group
            .as_ref()
            .ok_or_else(|| VizError::Json("the manifest has no group block".to_owned()))?;
        let mut warnings = Vec::new();
        let mut leads = Vec::new();
        for s in &group.subjects {
            for side in 0..2usize {
                let name = if side == 0 { "right" } else { "left" };
                let geo = match load_block_side_geometry_opt(&s.reco_json, name, &s.block) {
                    Ok(Some(g)) => g,
                    Ok(None) => continue,
                    Err(e) => {
                        warnings.push(format!("{} {name}: {e}", s.subject));
                        continue;
                    }
                };
                let Some(markers) = geo.markers.as_ref() else {
                    warnings.push(format!("{} {name}: no markers", s.subject));
                    continue;
                };
                let contacts: Vec<Vec3> = geo
                    .contacts
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|c| to_vec3(*c))
                    .collect();
                let mut active = s.active_contacts.get(side).clone();
                active.resize(contacts.len(), false);
                let values = s.value.as_ref().map(|v| {
                    v.get(side)
                        .iter()
                        .map(|x| x.map_or(f32::NAN, |x| x as f32))
                        .collect::<Vec<f32>>()
                });
                let model = s
                    .elmodel
                    .clone()
                    .or_else(|| geo.elmodel.clone())
                    .filter(|m| !m.trim().is_empty());
                leads.push(GroupLead {
                    subject: s.subject.clone(),
                    patient: s
                        .source_subject
                        .clone()
                        .unwrap_or_else(|| s.subject.clone()),
                    mirrored: s.mirrored,
                    group: s.group,
                    group_label: s.label.clone(),
                    side,
                    model,
                    head: to_vec3(markers.head),
                    tail: to_vec3(markers.tail),
                    x_ref: markers.x.map(to_vec3),
                    y_ref: markers.y.map(to_vec3),
                    contacts,
                    active,
                    values,
                    pair_values: s.value_kind == "pair",
                    vta: s.vta.get(side).clone(),
                });
            }
        }
        if leads.is_empty() {
            return Err(VizError::Json(
                "the group scene has no lead that could be read".to_owned(),
            ));
        }
        let mut scene = GroupScene {
            guid: group.guid.clone(),
            title: group.title.clone(),
            groups: group_infos(group, &leads),
            colour: colour_mode(group),
            show: group.show,
            mirror: group.mirror,
            centre: Vec3::ZERO,
            warnings,
            cache_dir: crate::atlas_marching::user_mesh_cache_dir()
                .and_then(|d| d.parent().map(|p| p.join("vta_meshes")))
                .unwrap_or_else(|| std::env::temp_dir().join("dbsloc_vta_meshes")),
            leads,
        };
        scene.centre = scene.mean_contact();
        Ok(scene)
    }

    fn mean_contact(&self) -> Vec3 {
        let mut sum = Vec3::ZERO;
        let mut n = 0u32;
        for l in &self.leads {
            if l.contacts.is_empty() {
                sum += 0.5 * (l.head + l.tail);
                n += 1;
            } else {
                for c in &l.contacts {
                    sum += *c;
                    n += 1;
                }
            }
        }
        if n == 0 {
            Vec3::ZERO
        } else {
            sum / n as f32
        }
    }

    /// The patients of the scene in manifest order, mirrored copies excluded.
    pub fn patients(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in &self.leads {
            if !l.mirrored && !out.contains(&l.patient) {
                out.push(l.patient.clone());
            }
        }
        out
    }

    /// Number of leads per electrode model (leads without a model are counted under an empty name).
    pub fn leads_per_model(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for l in &self.leads {
            *out.entry(l.model.clone().unwrap_or_default()).or_insert(0) += 1;
        }
        out
    }

    /// Distinct model names that are in the catalogue.
    pub fn model_names(&self) -> Vec<String> {
        self.leads_per_model()
            .into_keys()
            .filter(|n| !n.is_empty() && dbsloc_reconstruct::electrode_by_name(n).is_some())
            .collect()
    }

    /// Colour of a lead's group.
    pub fn group_colour(&self, number: u32) -> [f32; 3] {
        self.groups
            .iter()
            .find(|g| g.number == number)
            .map_or_else(|| default_group_colour(number), |g| g.color)
    }

    /// The legend of the current colouring.
    pub fn legend(&self) -> Legend {
        match &self.colour {
            ColourMode::Group => Legend::Groups(
                self.groups
                    .iter()
                    .map(|g| (g.label.clone(), g.color))
                    .collect(),
            ),
            ColourMode::Clinical {
                name,
                normalisation,
                range,
            } => Legend::ColourBar {
                name: name.clone(),
                normalisation: normalisation.clone(),
                range: *range,
            },
        }
    }

    /// How `lead` is emphasised under `display`.
    pub fn emphasis(&self, display: &GroupDisplay, lead: &GroupLead) -> Emphasis {
        if display.selected_patient.is_none() && display.selected_group.is_none() {
            return Emphasis::Normal;
        }
        let by_patient = display
            .selected_patient
            .as_ref()
            .is_some_and(|p| *p == lead.patient || *p == lead.subject);
        let by_group = display.selected_group.is_some_and(|g| g == lead.group);
        if by_patient || by_group {
            Emphasis::Selected
        } else {
            Emphasis::Dimmed
        }
    }

    /// The contact points of a lead with their value and active flag.
    pub fn contact_points(lead: &GroupLead) -> Vec<ContactPoint> {
        let value_at = |i: usize| {
            lead.values
                .as_ref()
                .and_then(|v| v.get(i))
                .copied()
                .unwrap_or(f32::NAN)
        };
        let active_at = |i: usize| lead.active.get(i).copied().unwrap_or(false);
        if lead.pair_values {
            lead.contacts
                .windows(2)
                .enumerate()
                .map(|(i, w)| ContactPoint {
                    pos: 0.5 * (w[0] + w[1]),
                    active: active_at(i) || active_at(i + 1),
                    value: value_at(i),
                })
                .collect()
        } else {
            lead.contacts
                .iter()
                .enumerate()
                .map(|(i, c)| ContactPoint {
                    pos: *c,
                    active: active_at(i),
                    value: value_at(i),
                })
                .collect()
        }
    }

    fn body_colours(&self, lead: &GroupLead) -> ([f32; 3], [f32; 3]) {
        match self.colour {
            ColourMode::Group => {
                let g = self.group_colour(lead.group);
                (g.map(|c| c * INSULATION_GROUP_FACTOR), g)
            }
            ColourMode::Clinical { .. } => (INSULATION_RGB, METAL_RGB),
        }
    }

    /// The instances to draw under `display`, grouped by mesh. `models` supplies the meshes; a lead whose model has none gets contact markers instead of a body.
    pub fn build_batches(&self, display: &GroupDisplay, models: &GroupModels) -> Vec<Batch> {
        let mut map: BTreeMap<(String, Part), Batch> = BTreeMap::new();
        let clinical = matches!(self.colour, ColourMode::Clinical { .. });
        let cloud = clinical && display.point_cloud;
        let markers_default =
            display.show_active && display.show_passive && !display.highlight_active;
        for lead in &self.leads {
            let emphasis = self.emphasis(display, lead);
            let alpha = if emphasis == Emphasis::Dimmed {
                DIMMED_ALPHA
            } else {
                1.0
            };
            let dimmed = emphasis == Emphasis::Dimmed;
            let mut push = |model: &str, part: Part, inst: Instance| {
                let b = map
                    .entry((model.to_owned(), part))
                    .or_insert_with(|| Batch {
                        model: (part != Part::Sphere).then(|| model.to_owned()),
                        part: Some(part),
                        ..Batch::default()
                    });
                if dimmed {
                    b.dimmed.push(inst);
                } else {
                    b.opaque.push(inst);
                }
            };
            let mut has_body = false;
            if let Some(name) = lead.model.as_deref() {
                if let (Some(i), Some(geom)) = (
                    models.body_of(name),
                    dbsloc_reconstruct::electrode_by_name(name),
                ) {
                    if let Some(rows) =
                        lead_instance_matrix(geom, lead.head, lead.tail, lead.x_ref, lead.y_ref)
                    {
                        let (ins, metal) = self.body_colours(lead);
                        let key = models.names[i].as_str();
                        let make = |rgb: [f32; 3]| Instance {
                            m0: rows[0],
                            m1: rows[1],
                            m2: rows[2],
                            colour: [rgb[0], rgb[1], rgb[2], alpha],
                            extra: [0.0; 4],
                        };
                        if models.insulation[i].is_some() {
                            push(key, Part::Insulation, make(ins));
                        }
                        if models.metal[i].is_some() {
                            push(key, Part::Metal, make(metal));
                        }
                        has_body = true;
                    }
                }
            }
            let group_rgb = self.group_colour(lead.group);
            for p in Self::contact_points(lead) {
                let shown =
                    (p.active && display.show_active) || (!p.active && display.show_passive);
                if !shown {
                    continue;
                }
                let spec = if cloud {
                    value_colour(p.value, self.range()).map(|c| (c, POINT_RADIUS_MM, 0.0))
                } else if display.highlight_active || !markers_default || !has_body {
                    if p.active && display.highlight_active {
                        Some((ACTIVE_RGB, MARKER_RADIUS_MM, 1.0))
                    } else if p.active {
                        Some((group_rgb, MARKER_RADIUS_MM, 0.0))
                    } else {
                        Some((group_rgb, 0.5 * MARKER_RADIUS_MM, 0.0))
                    }
                } else {
                    None
                };
                if let Some((rgb, r, rim)) = spec {
                    push(
                        "",
                        Part::Sphere,
                        Instance {
                            m0: [r, 0.0, 0.0, p.pos.x],
                            m1: [0.0, r, 0.0, p.pos.y],
                            m2: [0.0, 0.0, r, p.pos.z],
                            colour: [rgb[0], rgb[1], rgb[2], alpha],
                            extra: [rim, 0.0, 0.0, 0.0],
                        },
                    );
                }
            }
        }
        map.into_values().collect()
    }

    fn range(&self) -> [f32; 2] {
        match &self.colour {
            ColourMode::Clinical { range, .. } => *range,
            ColourMode::Group => [0.0, 1.0],
        }
    }

    /// RGBA of a VTA surface of `lead` (`specs/14` §5.2): yellow, or the group colour when colouring by group.
    pub fn vta_colour(&self, display: &GroupDisplay, lead: &GroupLead) -> [f32; 4] {
        let rgb = match self.colour {
            ColourMode::Group => self.group_colour(lead.group),
            ColourMode::Clinical { .. } => VTA_RGB,
        };
        let a = match self.emphasis(display, lead) {
            Emphasis::Dimmed => VTA_ALPHA * DIMMED_ALPHA,
            _ => VTA_ALPHA,
        };
        [rgb[0], rgb[1], rgb[2], a]
    }

    /// The lead nearest to `cursor` within `radius_px`. `project` maps world mm to screen points.
    pub fn pick(
        &self,
        project: &dyn Fn(Vec3) -> Option<[f32; 2]>,
        cursor: [f32; 2],
        radius_px: f32,
    ) -> Option<PickHit> {
        let mut best: Option<PickHit> = None;
        for (li, lead) in self.leads.iter().enumerate() {
            let mut distance = f32::INFINITY;
            if let (Some(a), Some(b)) = (project(lead.head), project(lead.tail)) {
                distance = segment_distance(cursor, a, b);
            }
            let mut near_point: Option<(usize, f32)> = None;
            for (pi, p) in Self::contact_points(lead).iter().enumerate() {
                if let Some(s) = project(p.pos) {
                    let d = ((s[0] - cursor[0]).powi(2) + (s[1] - cursor[1]).powi(2)).sqrt();
                    if d <= radius_px && near_point.is_none_or(|(_, best)| d < best) {
                        near_point = Some((pi, d));
                    }
                }
            }
            if let Some((_, d)) = near_point {
                distance = distance.min(d);
            }
            let near_point = near_point.map(|(pi, _)| pi);
            if distance <= radius_px && best.is_none_or(|b| distance < b.distance_px) {
                best = Some(PickHit {
                    lead: li,
                    point: near_point,
                    distance_px: distance,
                });
            }
        }
        best
    }

    /// `sub-<id> · group · side · value` for the hover text of a lead.
    pub fn hover_text(&self, hit: &PickHit) -> String {
        let lead = &self.leads[hit.lead];
        let side = if lead.side == 0 { "right" } else { "left" };
        let value = match hit.point {
            Some(pi) => Self::contact_points(lead)
                .get(pi)
                .map(|p| p.value)
                .filter(|v| v.is_finite()),
            None => None,
        };
        let value = value.map_or_else(|| "no value".to_owned(), |v| format!("{v:.2}"));
        format!("{} · {} · {side} · {value}", lead.subject, lead.group_label)
    }

    /// VTA surfaces to mesh: (lead index, image path).
    pub fn vta_jobs(&self) -> Vec<(usize, PathBuf)> {
        self.leads
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.vta.clone().filter(|p| p.is_file()).map(|p| (i, p)))
            .collect()
    }
}

fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 > 1e-9 {
        (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = [a[0] + t * ab[0], a[1] + t * ab[1]];
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt()
}

fn group_infos(group: &GroupBlockV2, leads: &[GroupLead]) -> Vec<GroupInfo> {
    let mut out: Vec<GroupInfo> = group
        .groups
        .iter()
        .map(|g| GroupInfo {
            number: g.number,
            label: if g.label.is_empty() {
                format!("Group {}", g.number)
            } else {
                g.label.clone()
            },
            color: g.color.unwrap_or_else(|| default_group_colour(g.number)),
        })
        .collect();
    for l in leads {
        if !out.iter().any(|g| g.number == l.group) {
            out.push(GroupInfo {
                number: l.group,
                label: if l.group_label.is_empty() {
                    format!("Group {}", l.group)
                } else {
                    l.group_label.clone()
                },
                color: default_group_colour(l.group),
            });
        }
    }
    out
}

fn colour_mode(group: &GroupBlockV2) -> ColourMode {
    match &group.colour_by {
        GroupColourByV2::Name(_) => ColourMode::Group,
        GroupColourByV2::Clinical(c) => {
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for s in &group.subjects {
                if let Some(v) = &s.value {
                    for x in v.right.iter().chain(&v.left).flatten() {
                        lo = lo.min(*x as f32);
                        hi = hi.max(*x as f32);
                    }
                }
            }
            let range = c
                .range
                .map(|r| [r[0] as f32, r[1] as f32])
                .filter(|r| r[0].is_finite() && r[1].is_finite())
                .or_else(|| (lo <= hi).then_some([lo, hi]))
                .unwrap_or([0.0, 1.0]);
            ColourMode::Clinical {
                name: c.clinical.clone(),
                normalisation: c.normalisation.clone(),
                range,
            }
        }
    }
}

const VTA_CACHE_MAGIC: &[u8; 8] = b"DBSVTAM1";

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_f32(b: &[u8], at: usize) -> Option<f32> {
    read_u32(b, at).map(f32::from_bits)
}

fn decode_mesh(bytes: &[u8]) -> Option<TriMeshScene> {
    if bytes.get(..8)? != VTA_CACHE_MAGIC {
        return None;
    }
    let nv = read_u32(bytes, 8)? as usize;
    let ni = read_u32(bytes, 12)? as usize;
    if bytes.len() != 16 + nv * 24 + ni * 4 {
        return None;
    }
    let mut at = 16;
    let vec3s = |n: usize, at: &mut usize| -> Option<Vec<Vec3>> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(Vec3::new(
                read_f32(bytes, *at)?,
                read_f32(bytes, *at + 4)?,
                read_f32(bytes, *at + 8)?,
            ));
            *at += 12;
        }
        Some(v)
    };
    let positions = vec3s(nv, &mut at)?;
    let normals = vec3s(nv, &mut at)?;
    let mut indices = Vec::with_capacity(ni);
    for _ in 0..ni {
        let i = read_u32(bytes, at)?;
        if i as usize >= nv {
            return None;
        }
        indices.push(i);
        at += 4;
    }
    Some(TriMeshScene {
        positions,
        normals,
        indices,
    })
}

fn encode_mesh(mesh: &TriMeshScene) -> Vec<u8> {
    let nv = mesh.positions.len();
    let mut out = Vec::with_capacity(16 + nv * 24 + mesh.indices.len() * 4);
    out.extend_from_slice(VTA_CACHE_MAGIC);
    out.extend_from_slice(&(nv as u32).to_le_bytes());
    out.extend_from_slice(&(mesh.indices.len() as u32).to_le_bytes());
    for p in &mesh.positions {
        for c in p.to_array() {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    for i in 0..nv {
        for c in mesh.normals.get(i).copied().unwrap_or(Vec3::Y).to_array() {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    for i in &mesh.indices {
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

/// Surface of a VTA image, cached under `cache_dir` by the SHA-256 of the image (`specs/14` §5.5). A cache that cannot be read or written only costs time.
pub fn vta_mesh_cached(image: &Path, cache_dir: &Path) -> VizResult<TriMeshScene> {
    let key = fastwarp_core::sha256::file_sha256_hex(image)
        .map_err(|e| VizError::NiftiLoad(format!("{}: {e}", image.display())))?;
    let cached = cache_dir.join(format!("{key}.vtamesh"));
    if let Some(mesh) = std::fs::read(&cached).ok().as_deref().and_then(decode_mesh) {
        return Ok(mesh);
    }
    let mesh = crate::vta_mesh::mesh_vta_nifti(image)?;
    if std::fs::create_dir_all(cache_dir).is_ok() {
        let partial = cache_dir.join(format!(".{key}.{}.partial", std::process::id()));
        if std::fs::write(&partial, encode_mesh(&mesh)).is_ok()
            && std::fs::rename(&partial, &cached).is_err()
        {
            let _ = std::fs::remove_file(&partial);
        }
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests;
