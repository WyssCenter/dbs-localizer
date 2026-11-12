// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Lead frame, electrode body and solver grid (`specs/07` §3.1, §3.7).

use dbsloc_reconstruct::ElectrodeGeometry;
use rayon::prelude::*;

use crate::conductivity::{ConductivityModel, Encapsulation};
use crate::VatError;

/// Class code of a tissue cell.
pub const TISSUE: u16 = 0;
/// Class code of an insulation cell.
pub const INSULATION: u16 = 1;
/// Class code of contact 0; contact `k` is `CONTACT_BASE + k`.
pub const CONTACT_BASE: u16 = 2;

const SEGMENT_HALF_ARC: f64 = std::f64::consts::FRAC_PI_4;
const SEGMENT_INNER_FACTOR: f64 = 0.5;
const MAX_CELL_WIDTH_MM: f64 = 6.0;

/// Material of one point of space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    /// Volume conductor.
    Tissue,
    /// Lead body that is not a contact, including the orientation marker.
    Insulation,
    /// Contact with the given 0-based index.
    Contact(usize),
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn unit(a: [f64; 3]) -> Option<[f64; 3]> {
    let n = norm(a);
    (n.is_finite() && n > 1e-12).then(|| [a[0] / n, a[1] / n, a[2] / n])
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn orthogonal_part(v: [f64; 3], axis: [f64; 3]) -> Option<[f64; 3]> {
    let d = dot(v, axis);
    unit([v[0] - d * axis[0], v[1] - d * axis[1], v[2] - d * axis[2]])
}

/// Orthonormal lead frame in world millimetres: origin at the head marker, `ez` toward the tail.
#[derive(Clone, Copy, Debug)]
pub struct LeadFrame {
    /// Head marker (centre of the most distal level).
    pub head: [f64; 3],
    /// Lead x axis.
    pub ex: [f64; 3],
    /// Lead y axis.
    pub ey: [f64; 3],
    /// Lead axis, head toward tail.
    pub ez: [f64; 3],
    /// Axial stretch of the catalogue model, `|tail - head| / model span`.
    pub stretch: f64,
}

impl LeadFrame {
    /// Frame from Reco v2 markers (`specs/05` §6); `x` / `y` fix the roll, either alone suffices.
    pub fn from_markers(
        head: [f64; 3],
        tail: [f64; 3],
        x: Option<[f64; 3]>,
        y: Option<[f64; 3]>,
        model: &ElectrodeGeometry,
    ) -> Result<Self, VatError> {
        let axis = sub(tail, head);
        let len = norm(axis);
        let ez = unit(axis)
            .ok_or_else(|| VatError::InvalidInput("head and tail markers coincide".into()))?;
        let ex_raw = x.and_then(|p| orthogonal_part(sub(p, head), ez));
        let ey_raw = y.and_then(|p| orthogonal_part(sub(p, head), ez));
        let (ex, ey) = match (ex_raw, ey_raw) {
            (Some(ex), Some(ey_r)) => {
                let ey = orthogonal_part(ey_r, ex).unwrap_or_else(|| cross(ez, ex));
                if dot(cross(ex, ey), ez) < 0.0 {
                    (ex, cross(ez, ex))
                } else {
                    (ex, ey)
                }
            }
            (Some(ex), None) => (ex, cross(ez, ex)),
            (None, Some(ey)) => (cross(ey, ez), ey),
            (None, None) => {
                let seed = if ez[0].abs() < 0.9 {
                    [1.0, 0.0, 0.0]
                } else {
                    [0.0, 1.0, 0.0]
                };
                let ex = orthogonal_part(seed, ez)
                    .ok_or_else(|| VatError::InvalidInput("degenerate lead axis".into()))?;
                (ex, cross(ez, ex))
            }
        };
        let span = model.contact_span_mm();
        let stretch = if span.is_finite() && span > 1e-6 && len.is_finite() {
            len / span
        } else {
            1.0
        };
        Ok(Self {
            head,
            ex,
            ey,
            ez,
            stretch,
        })
    }

    /// World millimetres to lead-local millimetres.
    pub fn to_local(&self, p: [f64; 3]) -> [f64; 3] {
        let d = sub(p, self.head);
        [dot(d, self.ex), dot(d, self.ey), dot(d, self.ez)]
    }

    /// Lead-local millimetres to world millimetres.
    pub fn to_world(&self, l: [f64; 3]) -> [f64; 3] {
        [
            self.head[0] + self.ex[0] * l[0] + self.ey[0] * l[1] + self.ez[0] * l[2],
            self.head[1] + self.ex[1] * l[0] + self.ey[1] * l[1] + self.ez[1] * l[2],
            self.head[2] + self.ex[2] * l[0] + self.ey[2] * l[1] + self.ez[2] * l[2],
        ]
    }
}

/// World position of the catalogue centre of contact `k` (0-based).
pub fn contact_centre_world(
    lead: &LeadFrame,
    model: &ElectrodeGeometry,
    k: usize,
) -> Option<[f64; 3]> {
    let p = model.contact_xyz_mm.get(k)?;
    Some(lead.to_world([p[0], p[1], p[2] * lead.stretch]))
}

/// Which body the solver places in the volume conductor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BodyKind {
    /// The catalogue lead of `EngineInput::model`.
    Catalogue,
    /// A single spherical contact centred on the lead origin (synthetic acceptance tests, `specs/07` §9.1).
    Sphere {
        /// Sphere radius in millimetres.
        radius_mm: f64,
    },
}

#[derive(Clone, Debug)]
struct ContactShape {
    z_lo: f64,
    z_hi: f64,
    azimuth: Option<f64>,
}

/// Analytic electrode body in lead-local millimetres.
#[derive(Clone, Debug)]
pub enum Body {
    /// Catalogue lead.
    Lead(LeadBody),
    /// Sphere at the origin; contact 0.
    Sphere(f64),
}

/// Cylinder with a rounded tip and the catalogue contacts, stretched along the lead.
#[derive(Clone, Debug)]
pub struct LeadBody {
    radius: f64,
    apex_z: f64,
    dome_len: f64,
    contacts: Vec<ContactShape>,
    z_min: f64,
    z_max: f64,
}

impl Body {
    /// Builds the body for a catalogue model with axial stretch `stretch`.
    pub fn new(kind: BodyKind, model: &ElectrodeGeometry, stretch: f64) -> Result<Self, VatError> {
        match kind {
            BodyKind::Sphere { radius_mm } => {
                if !(radius_mm.is_finite() && radius_mm > 0.0) {
                    return Err(VatError::InvalidInput(
                        "sphere radius must be positive".into(),
                    ));
                }
                Ok(Body::Sphere(radius_mm))
            }
            BodyKind::Catalogue => LeadBody::new(model, stretch).map(Body::Lead),
        }
    }

    /// Number of contacts of the body.
    pub fn n_contacts(&self) -> usize {
        match self {
            Body::Lead(l) => l.contacts.len(),
            Body::Sphere(_) => 1,
        }
    }

    /// Radius used by the electrode-removal collapse (`specs/07` §3.8 step 2).
    pub fn lead_radius(&self) -> f64 {
        match self {
            Body::Lead(l) => l.radius,
            Body::Sphere(r) => *r,
        }
    }

    /// Axial range (lead-local z) occupied by contacts and tip.
    pub fn z_range(&self) -> (f64, f64) {
        match self {
            Body::Lead(l) => (l.z_min, l.z_max),
            Body::Sphere(r) => (-r, *r),
        }
    }

    /// Material at a lead-local point.
    pub fn material(&self, l: [f64; 3]) -> Material {
        match self {
            Body::Sphere(r) => {
                if dot(l, l) <= r * r {
                    Material::Contact(0)
                } else {
                    Material::Tissue
                }
            }
            Body::Lead(b) => b.material(l),
        }
    }

    /// Distance from a lead-local point to the body surface, zero inside.
    pub fn distance(&self, l: [f64; 3]) -> f64 {
        match self {
            Body::Sphere(r) => (norm(l) - r).max(0.0),
            Body::Lead(b) => b.distance(l),
        }
    }
}

impl LeadBody {
    fn new(model: &ElectrodeGeometry, stretch: f64) -> Result<Self, VatError> {
        let radius = 0.5 * model.lead_diameter_mm;
        if !(radius.is_finite() && radius > 0.0) {
            return Err(VatError::InvalidInput(format!(
                "{}: lead diameter is not positive",
                model.elmodel
            )));
        }
        let s = if stretch.is_finite() && stretch > 1e-3 {
            stretch
        } else {
            1.0
        };
        let apex_z = -model.apex_to_first_level_mm() * s;
        let half = 0.5 * model.contact_length_mm * s;
        let tip_len = if model.tip_is_contact {
            model.tip_length_mm * s
        } else {
            (-half - apex_z).max(1e-3)
        };
        let dome_len = tip_len.min(radius).max(1e-3);
        let distal = model
            .contact_xyz_mm
            .iter()
            .map(|p| p[2])
            .fold(f64::INFINITY, f64::min);
        let mut contacts = Vec::with_capacity(model.n_contacts);
        let mut tip_taken = false;
        for p in &model.contact_xyz_mm {
            let z = p[2] * s;
            let azimuth = ((p[0] * p[0] + p[1] * p[1]).sqrt() > 1e-6).then(|| p[1].atan2(p[0]));
            let is_tip = model.tip_is_contact && !tip_taken && (p[2] - distal).abs() < 1e-9;
            let z_lo = if is_tip { apex_z } else { z - half };
            if is_tip {
                tip_taken = true;
            }
            contacts.push(ContactShape {
                z_lo,
                z_hi: z + half,
                azimuth: if is_tip { None } else { azimuth },
            });
        }
        let z_max = contacts
            .iter()
            .map(|c| c.z_hi)
            .fold(f64::NEG_INFINITY, f64::max);
        Ok(Self {
            radius,
            apex_z,
            dome_len,
            contacts,
            z_min: apex_z,
            z_max,
        })
    }

    fn radius_at(&self, z: f64) -> Option<f64> {
        if z < self.apex_z {
            return None;
        }
        let top = self.apex_z + self.dome_len;
        if z >= top {
            return Some(self.radius);
        }
        let t = (top - z) / self.dome_len;
        Some(self.radius * (1.0 - t * t).max(0.0).sqrt())
    }

    fn material(&self, l: [f64; 3]) -> Material {
        let rho = (l[0] * l[0] + l[1] * l[1]).sqrt();
        let Some(rb) = self.radius_at(l[2]) else {
            return Material::Tissue;
        };
        if rho > rb {
            return Material::Tissue;
        }
        for (k, c) in self.contacts.iter().enumerate() {
            if l[2] < c.z_lo || l[2] >= c.z_hi {
                continue;
            }
            match c.azimuth {
                None => return Material::Contact(k),
                Some(az) => {
                    if rho < SEGMENT_INNER_FACTOR * self.radius {
                        continue;
                    }
                    let mut d = (l[1].atan2(l[0]) - az).abs();
                    if d > std::f64::consts::PI {
                        d = 2.0 * std::f64::consts::PI - d;
                    }
                    if d <= SEGMENT_HALF_ARC {
                        return Material::Contact(k);
                    }
                }
            }
        }
        Material::Insulation
    }

    fn distance(&self, l: [f64; 3]) -> f64 {
        let rho = (l[0] * l[0] + l[1] * l[1]).sqrt();
        let top = self.apex_z + self.dome_len;
        if l[2] >= top {
            return (rho - self.radius).max(0.0);
        }
        let scale = self.radius / self.dome_len;
        let dz = (l[2] - top) * scale;
        ((rho * rho + dz * dz).sqrt() - self.radius).max(0.0)
    }
}

/// Solver grid parameters and output sampling (`specs/07` §3.7, §3.8).
#[derive(Clone, Debug)]
pub struct GridSpec {
    /// Cell width in the fine region, millimetres.
    pub fine_spacing_mm: f64,
    /// Half width of the fine region across the lead.
    pub fine_half_width_mm: f64,
    /// Fine-region margin beyond the lead along its axis.
    pub fine_margin_z_mm: f64,
    /// Geometric growth factor of the cell widths outside the fine region.
    pub growth: f64,
    /// Minimum distance from every contact to the grounded outer boundary.
    pub domain_margin_mm: f64,
    /// Refusal threshold for the solver cell count.
    pub max_cells: usize,
    /// Output voxel spacing (`vat_grid_mm`).
    pub output_spacing_mm: f64,
}

impl Default for GridSpec {
    fn default() -> Self {
        Self {
            fine_spacing_mm: 0.2,
            fine_half_width_mm: 6.0,
            fine_margin_z_mm: 6.0,
            growth: 1.15,
            domain_margin_mm: 40.0,
            max_cells: 60_000_000,
            output_spacing_mm: 0.2,
        }
    }
}

/// Rectilinear solver grid in lead-local millimetres.
#[derive(Clone, Debug)]
pub struct SolverGrid {
    /// Cell counts per axis.
    pub dims: [usize; 3],
    /// Cell faces per axis (`dims[a] + 1` ascending values).
    pub faces: [Vec<f64>; 3],
    /// Cell centres per axis.
    pub centres: [Vec<f64>; 3],
    /// Cell widths per axis.
    pub widths: [Vec<f64>; 3],
}

impl SolverGrid {
    /// Linear cell index, x fastest.
    #[inline]
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        i + self.dims[0] * (j + self.dims[1] * k)
    }

    /// Total cell count.
    pub fn n_cells(&self) -> usize {
        self.dims[0] * self.dims[1] * self.dims[2]
    }

    /// Lead-local coordinates of a cell centre.
    #[inline]
    pub fn centre(&self, i: usize, j: usize, k: usize) -> [f64; 3] {
        [self.centres[0][i], self.centres[1][j], self.centres[2][k]]
    }

    /// Cell containing a lead-local point, `None` outside the domain.
    pub fn locate(&self, p: [f64; 3]) -> Option<[usize; 3]> {
        let mut out = [0usize; 3];
        for a in 0..3 {
            let f = &self.faces[a];
            if p[a] < f[0] || p[a] > f[f.len() - 1] {
                return None;
            }
            out[a] = f
                .partition_point(|&v| v <= p[a])
                .saturating_sub(1)
                .min(self.dims[a] - 1);
        }
        Some(out)
    }
}

fn graded_axis(
    lo_fine: f64,
    hi_fine: f64,
    h: f64,
    growth: f64,
    lo_dom: f64,
    hi_dom: f64,
) -> Vec<f64> {
    let a = (lo_fine / h - 0.5).floor() as i64;
    let b = (hi_fine / h - 0.5).ceil() as i64;
    let mut faces: Vec<f64> = (a..=b).map(|m| (m as f64 + 0.5) * h).collect();
    let mut w = h;
    let mut pos = faces[0];
    let mut below = Vec::new();
    while pos > lo_dom {
        w = (w * growth).min(MAX_CELL_WIDTH_MM);
        pos -= w;
        below.push(pos);
    }
    below.reverse();
    let mut pos = faces[faces.len() - 1];
    let mut w = h;
    let mut above = Vec::new();
    while pos < hi_dom {
        w = (w * growth).min(MAX_CELL_WIDTH_MM);
        pos += w;
        above.push(pos);
    }
    let mut out = below;
    out.append(&mut faces);
    out.extend(above);
    out
}

/// Builds the graded grid around a body (`specs/07` §3.7).
pub fn build_grid(body: &Body, spec: &GridSpec) -> Result<SolverGrid, VatError> {
    let h = spec.fine_spacing_mm;
    if !(h.is_finite() && h > 0.0 && spec.growth >= 1.0 && spec.growth <= 2.0) {
        return Err(VatError::InvalidInput("invalid grid specification".into()));
    }
    let (z_lo, z_hi) = body.z_range();
    let m = spec.domain_margin_mm;
    let fw = spec.fine_half_width_mm;
    let fz = spec.fine_margin_z_mm;
    let ax_xy = graded_axis(-fw, fw, h, spec.growth, -m.max(fw), m.max(fw));
    let ax_z = graded_axis(z_lo - fz, z_hi + fz, h, spec.growth, z_lo - m, z_hi + m);
    let faces = [ax_xy.clone(), ax_xy, ax_z];
    let dims = [faces[0].len() - 1, faces[1].len() - 1, faces[2].len() - 1];
    let cells = dims[0]
        .checked_mul(dims[1])
        .and_then(|v| v.checked_mul(dims[2]))
        .unwrap_or(usize::MAX);
    if cells > spec.max_cells {
        return Err(VatError::GridTooLarge { cells });
    }
    let centres = [0, 1, 2].map(|a| faces[a].windows(2).map(|w| 0.5 * (w[0] + w[1])).collect());
    let widths = [0, 1, 2].map(|a| faces[a].windows(2).map(|w| w[1] - w[0]).collect());
    Ok(SolverGrid {
        dims,
        faces,
        centres,
        widths,
    })
}

/// Per-cell class and conductivity.
#[derive(Clone, Debug)]
pub struct Materials {
    /// Class per cell: `TISSUE`, `INSULATION` or `CONTACT_BASE + k`.
    pub class: Vec<u16>,
    /// Conductivity per cell in S/m (meaningful for tissue cells only).
    pub sigma: Vec<f32>,
}

/// Classifies every cell and assigns tissue conductivity (`specs/07` §3.1, §3.2).
pub fn build_materials(
    grid: &SolverGrid,
    body: &Body,
    lead: &LeadFrame,
    conductivity: &ConductivityModel,
    encapsulation: &Encapsulation,
) -> Materials {
    let [nx, ny, nz] = grid.dims;
    let plane = nx * ny;
    let mut class = vec![TISSUE; grid.n_cells()];
    let mut sigma = vec![0f32; grid.n_cells()];
    class
        .par_chunks_mut(plane)
        .zip(sigma.par_chunks_mut(plane))
        .enumerate()
        .for_each(|(k, (cl, sg))| {
            debug_assert!(k < nz);
            for j in 0..ny {
                for i in 0..nx {
                    let l = grid.centre(i, j, k);
                    let c = i + nx * j;
                    match body.material(l) {
                        Material::Insulation => cl[c] = INSULATION,
                        Material::Contact(q) => cl[c] = CONTACT_BASE + q as u16,
                        Material::Tissue => {
                            let enc = encapsulation.thickness_mm > 0.0
                                && body.distance(l) <= encapsulation.thickness_mm;
                            sg[c] = if enc {
                                encapsulation.s_per_m as f32
                            } else {
                                conductivity.sigma_at(lead.to_world(l)) as f32
                            };
                        }
                    }
                }
            }
        });
    Materials { class, sigma }
}
