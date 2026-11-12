// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Procedural electrode geometry for the interactive 3D scene (`specs/09`).
//!
//! Lead-DBS ships its contact meshes under **GPL-3.0**; this project does not copy Lead-DBS
//! material into its code (`AGENTS.md` §9). This module therefore derives a lead from the
//! catalogue scalars instead: the shaft is a cylinder, each contact is a band (a full ring for
//! ring contacts, a 120° arc for the segments of a directional level) and the tip is a rounded cap.
//!
//! The mesh is built in the lead's local model frame (`+z` proximal, origin at the most-distal
//! **level** centre — [`ElectrodeGeometry::level_z_mm`]`[0] == 0`) and then mapped into world
//! millimetres by the rigid frame the reconstruction already writes as its `head` / `tail` /
//! `x` / `y` markers. Those four markers are the model fiducials (`electrode_catalog`), so the
//! mapping is exact; no new geometry has to be inferred.
//!
//! Clean-room: derived only from `specs/09_electrode_models.md` and the catalogue data, not from
//! any Lead-DBS source or mesh file.

use dbsloc_reconstruct::ElectrodeGeometry;
use glam::Vec3;

use crate::atlas_wavefront::TriMeshScene;

const TAU: f32 = std::f32::consts::TAU;

/// Latitude bands in the rounded tip.
const DOME_STACKS: usize = 6;

/// Insulation (shaft / non-conductive tip) — a matte dark grey.
pub const INSULATION_RGBA: [f32; 4] = [0.20, 0.21, 0.24, 1.0];
/// Contacts (platinum–iridium) — a light metallic grey.
pub const METAL_RGBA: [f32; 4] = [0.80, 0.82, 0.86, 1.0];

/// Both surface groups of one lead. Kept apart so each can take its own colour without a
/// per-vertex colour attribute.
#[derive(Debug, Clone)]
pub struct ElectrodeMeshGroup {
    pub insulation: TriMeshScene,
    pub metal: TriMeshScene,
}

impl ElectrodeMeshGroup {
    pub fn is_empty(&self) -> bool {
        self.insulation.indices.is_empty() && self.metal.indices.is_empty()
    }
}

/// How finely each ring is tessellated around the shaft.
const RADIAL_SEGMENTS: usize = 24;
/// A directional level carries three segments; `specs/09` §10 fixes each at 120°.
const SEGMENT_ARC: f32 = TAU / 3.0;
/// Contacts sit slightly proud of the insulation so the two surfaces do not z-fight.
const CONTACT_OUTER_FACTOR: f32 = 1.06;
/// Inner radius of a directional pad, as a fraction of the lead radius.
const PAD_INNER_FACTOR: f32 = 0.80;

/// Orthonormal lead frame in world millimetres.
#[derive(Clone, Copy, Debug)]
struct Frame {
    head: Vec3,
    ex: Vec3,
    ey: Vec3,
    ez: Vec3,
    stretch: f32,
}

impl Frame {
    /// Build from the reconstruction markers. `x_ref` / `y_ref` are the ring-plane fiducials;
    /// either alone fixes the roll, and both absent leaves an arbitrary (harmless for ring
    /// contacts) azimuth reference.
    fn from_markers(
        head: Vec3,
        tail: Vec3,
        x_ref: Option<Vec3>,
        y_ref: Option<Vec3>,
    ) -> Option<Self> {
        let ez = (tail - head).try_normalize()?;
        let orthogonal = |v: Vec3| {
            let raw = v - head;
            (raw - ez * raw.dot(ez)).try_normalize()
        };
        let (ex, ey) = match (x_ref.and_then(orthogonal), y_ref.and_then(orthogonal)) {
            (Some(ex), Some(ey_raw)) => {
                let ey = (ey_raw - ex * ey_raw.dot(ex))
                    .try_normalize()
                    .unwrap_or_else(|| ez.cross(ex));
                if ex.cross(ey).dot(ez) < 0.0 {
                    (-ex, ey)
                } else {
                    (ex, ey)
                }
            }
            (Some(ex), None) => (ex, ez.cross(ex)),
            (None, Some(ey)) => (ey.cross(ez), ey),
            (None, None) => {
                let ex = any_perp(ez);
                (ex, ez.cross(ex))
            }
        };
        Some(Self {
            head,
            ex,
            ey,
            ez,
            stretch: 1.0,
        })
    }

    #[inline]
    fn point(&self, lx: f32, ly: f32, lz: f32) -> Vec3 {
        self.head + self.ex * lx + self.ey * ly + self.ez * (lz * self.stretch)
    }

    #[inline]
    fn dir(&self, lx: f32, ly: f32, lz: f32) -> Vec3 {
        self.ex * lx + self.ey * ly + self.ez * lz
    }
}

fn any_perp(ez: Vec3) -> Vec3 {
    let candidate = if ez.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    (candidate - ez * candidate.dot(ez))
        .try_normalize()
        .unwrap_or(Vec3::X)
}

#[derive(Default)]
struct Builder {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    indices: Vec<u32>,
}

impl Builder {
    fn vertex(&mut self, p: Vec3, n: Vec3) -> u32 {
        let i = self.positions.len() as u32;
        self.positions.push(p);
        self.normals.push(n);
        i
    }

    fn triangle(&mut self, a: u32, b: u32, c: u32) {
        self.indices.extend_from_slice(&[a, b, c]);
    }

    fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.triangle(a, b, c);
        self.triangle(a, c, d);
    }

    /// Ring of `n + 1` samples from `a0` to `a1` inclusive.
    fn angles(&self, a0: f32, a1: f32, n: usize) -> Vec<f32> {
        (0..=n)
            .map(|i| a0 + (a1 - a0) * (i as f32) / (n as f32))
            .collect()
    }

    /// Solid curved block between `z0` and `z1`, `r_in..r_out`, `a0..a1`.
    #[allow(clippy::too_many_arguments)]
    fn sector_solid(
        &mut self,
        frame: &Frame,
        z0: f32,
        z1: f32,
        r_in: f32,
        r_out: f32,
        a0: f32,
        a1: f32,
        seg: usize,
    ) {
        let span = a1 - a0;
        let full = span.abs() >= TAU - 1e-3;
        let n = ((seg as f32) * span.abs() / TAU).ceil().max(4.0) as usize;
        let angles = self.angles(a0, a1, n);
        let radial = |t: f32| frame.dir(t.cos(), t.sin(), 0.0);

        let mut ob = Vec::with_capacity(angles.len());
        let mut ot = Vec::with_capacity(angles.len());
        for &t in &angles {
            let (c, s) = (t.cos(), t.sin());
            let nr = radial(t);
            ob.push(self.vertex(frame.point(r_out * c, r_out * s, z0), nr));
            ot.push(self.vertex(frame.point(r_out * c, r_out * s, z1), nr));
        }
        for i in 0..n {
            self.quad(ob[i], ob[i + 1], ot[i + 1], ot[i]);
        }
        if full {
            let cz1 = frame.point(0.0, 0.0, z1);
            let cz0 = frame.point(0.0, 0.0, z0);
            let c1 = self.vertex(cz1, frame.dir(0.0, 0.0, 1.0));
            let c0 = self.vertex(cz0, frame.dir(0.0, 0.0, -1.0));
            for i in 0..n {
                self.triangle(ot[i], ot[i + 1], c1);
                self.triangle(ob[i + 1], ob[i], c0);
            }
            return;
        }

        let mut ib = Vec::with_capacity(angles.len());
        let mut it = Vec::with_capacity(angles.len());
        for &t in &angles {
            let (c, s) = (t.cos(), t.sin());
            let nr = -radial(t);
            ib.push(self.vertex(frame.point(r_in * c, r_in * s, z0), nr));
            it.push(self.vertex(frame.point(r_in * c, r_in * s, z1), nr));
        }
        for i in 0..n {
            self.quad(ib[i + 1], ib[i], it[i], it[i + 1]);
        }
        let nz1 = frame.dir(0.0, 0.0, 1.0);
        let nz0 = frame.dir(0.0, 0.0, -1.0);
        let mut ocap1 = Vec::with_capacity(angles.len());
        let mut icap1 = Vec::with_capacity(angles.len());
        let mut ocap0 = Vec::with_capacity(angles.len());
        let mut icap0 = Vec::with_capacity(angles.len());
        for &t in &angles {
            let (c, s) = (t.cos(), t.sin());
            ocap1.push(self.vertex(frame.point(r_out * c, r_out * s, z1), nz1));
            icap1.push(self.vertex(frame.point(r_in * c, r_in * s, z1), nz1));
            ocap0.push(self.vertex(frame.point(r_out * c, r_out * s, z0), nz0));
            icap0.push(self.vertex(frame.point(r_in * c, r_in * s, z0), nz0));
        }
        for i in 0..n {
            self.quad(ocap1[i], ocap1[i + 1], icap1[i + 1], icap1[i]);
            self.quad(ocap0[i + 1], ocap0[i], icap0[i], icap0[i + 1]);
        }
        for (front, z_a, z_b, wall_normal_sign) in [(true, z1, z0, 1.0_f32), (false, z1, z0, -1.0)]
        {
            let idx = if front { 0 } else { angles.len() - 1 };
            let t = angles[idx];
            let tangent = frame.dir(-t.sin(), t.cos(), 0.0) * wall_normal_sign;
            let ro = r_out;
            let ri = r_in;
            let (c, s) = (t.cos(), t.sin());
            let v0 = self.vertex(frame.point(ro * c, ro * s, z_a), tangent);
            let v1 = self.vertex(frame.point(ri * c, ri * s, z_a), tangent);
            let v2 = self.vertex(frame.point(ri * c, ri * s, z_b), tangent);
            let v3 = self.vertex(frame.point(ro * c, ro * s, z_b), tangent);
            if front {
                self.quad(v0, v1, v2, v3);
            } else {
                self.quad(v3, v2, v1, v0);
            }
        }
    }

    /// Rounded distal end between `z_apex` and `z_base`: a hemisphere of radius `r` when the tip
    /// is at least `r` long, a flattened half-ellipsoid when it is shorter, plus a straight
    /// section up to `z_base` when it is longer. The base ring always has radius `r`.
    fn dome(&mut self, frame: &Frame, z_apex: f32, z_base: f32, r: f32, seg: usize) {
        let length = (z_base - z_apex).max(1e-4);
        let c = length.min(r);
        let z_centre = z_apex + c;
        let apex = self.vertex(frame.point(0.0, 0.0, z_apex), frame.dir(0.0, 0.0, -1.0));
        let angles = self.angles(0.0, TAU, seg);
        let mut rings: Vec<Vec<u32>> = Vec::with_capacity(DOME_STACKS);
        for k in 1..=DOME_STACKS {
            let phi = -std::f32::consts::FRAC_PI_2
                + std::f32::consts::FRAC_PI_2 * (k as f32) / (DOME_STACKS as f32);
            let (sp, cp) = phi.sin_cos();
            let mut ring = Vec::with_capacity(angles.len());
            for &t in &angles {
                let (ct, st) = (t.cos(), t.sin());
                let normal = frame
                    .dir(cp * ct / r, cp * st / r, sp / (c * frame.stretch))
                    .try_normalize()
                    .unwrap_or_else(|| frame.dir(ct, st, 0.0));
                ring.push(self.vertex(
                    frame.point(r * cp * ct, r * cp * st, z_centre + c * sp),
                    normal,
                ));
            }
            rings.push(ring);
        }
        for i in 0..seg {
            self.triangle(apex, rings[0][i + 1], rings[0][i]);
        }
        for k in 1..rings.len() {
            for i in 0..seg {
                self.quad(
                    rings[k - 1][i],
                    rings[k - 1][i + 1],
                    rings[k][i + 1],
                    rings[k][i],
                );
            }
        }
        if z_base - z_centre > 1e-4 {
            let mut lo = Vec::with_capacity(angles.len());
            let mut hi = Vec::with_capacity(angles.len());
            for &t in &angles {
                let (ct, st) = (t.cos(), t.sin());
                let normal = frame.dir(ct, st, 0.0);
                lo.push(self.vertex(frame.point(r * ct, r * st, z_centre), normal));
                hi.push(self.vertex(frame.point(r * ct, r * st, z_base), normal));
            }
            for i in 0..seg {
                self.quad(lo[i], lo[i + 1], hi[i + 1], hi[i]);
            }
        }
    }
}

fn axial_stretch(geom: &ElectrodeGeometry, world_span: f32) -> f32 {
    let lo = geom.level_z_mm.iter().copied().fold(f64::MAX, f64::min);
    let hi = geom.level_z_mm.iter().copied().fold(f64::MIN, f64::max);
    let span = (hi - lo) as f32;
    let s = world_span / span;
    if span.is_finite() && span > 1e-6 && s.is_finite() && s > 1e-3 {
        s
    } else {
        1.0
    }
}

/// Build both surface groups of one lead in world millimetres.
///
/// `proximal_mm` is the shaft length to draw beyond the origin (the most-proximal trajectory
/// sample projected onto the lead axis), so the lead is not cut off inside the brain.
pub fn build_electrode(
    geom: &ElectrodeGeometry,
    head: Vec3,
    tail: Vec3,
    x_ref: Option<Vec3>,
    y_ref: Option<Vec3>,
    proximal_mm: f32,
) -> Option<ElectrodeMeshGroup> {
    let mut frame = Frame::from_markers(head, tail, x_ref, y_ref)?;
    frame.stretch = axial_stretch(geom, (tail - head).length());
    let r = 0.5 * geom.lead_diameter_mm as f32;
    if !(r.is_finite() && r > 0.0) {
        return None;
    }
    let contact_len = geom.contact_length_mm as f32;
    let half = (0.5 * contact_len).max(0.05);
    let apex_z = -(geom.apex_to_first_level_mm() as f32);
    let first_contact_distal = -half;
    let tip_top = first_contact_distal.max(apex_z + 0.1);
    let max_level = geom.level_z_mm.iter().copied().fold(0.0_f64, f64::max) as f32;
    let prox = (proximal_mm / frame.stretch).max(max_level + contact_len + 1.0);

    let mut ins = Builder::default();
    let mut metal = Builder::default();

    let tip = if geom.tip_is_contact {
        &mut metal
    } else {
        &mut ins
    };
    tip.dome(&frame, apex_z, tip_top, r, RADIAL_SEGMENTS);

    ins.sector_solid(&frame, tip_top, prox, 0.0, r, 0.0, TAU, RADIAL_SEGMENTS);

    let r_out = r * CONTACT_OUTER_FACTOR;
    for (lv, &z) in geom.level_z_mm.iter().enumerate() {
        let z = z as f32;
        let indices: Vec<usize> = geom
            .contact_level
            .iter()
            .enumerate()
            .filter_map(|(i, &l)| (l == lv).then_some(i))
            .collect();
        let directed = indices.iter().any(|&i| {
            let p = geom.contact_xyz_mm[i];
            (p[0] * p[0] + p[1] * p[1]).sqrt() > 1e-6
        });
        if directed {
            for &i in &indices {
                let p = geom.contact_xyz_mm[i];
                let angle = (p[1] as f32).atan2(p[0] as f32);
                metal.sector_solid(
                    &frame,
                    z - half,
                    z + half,
                    r * PAD_INNER_FACTOR,
                    r_out,
                    angle - 0.5 * SEGMENT_ARC,
                    angle + 0.5 * SEGMENT_ARC,
                    RADIAL_SEGMENTS,
                );
            }
        } else {
            metal.sector_solid(
                &frame,
                z - half,
                z + half,
                0.0,
                r_out,
                0.0,
                TAU,
                RADIAL_SEGMENTS,
            );
        }
    }

    Some(ElectrodeMeshGroup {
        insulation: TriMeshScene {
            positions: ins.positions,
            normals: ins.normals,
            indices: ins.indices,
        },
        metal: TriMeshScene {
            positions: metal.positions,
            normals: metal.normals,
            indices: metal.indices,
        },
    })
}

/// Shaft length (mm, proximal of the most-proximal contact) of a [`canonical_electrode`].
pub const CANONICAL_SHAFT_MM: f32 = 30.0;

fn level_span(geom: &ElectrodeGeometry) -> f32 {
    let lo = geom.level_z_mm.iter().copied().fold(f64::MAX, f64::min);
    let hi = geom.level_z_mm.iter().copied().fold(f64::MIN, f64::max);
    let span = (hi - lo) as f32;
    if span.is_finite() && span > 1e-6 {
        span
    } else {
        1.0
    }
}

/// The lead of `geom` in its own frame (head at the origin, `+z` towards the tail, unit
/// stretch) for instanced drawing: place it with [`lead_instance_matrix`].
pub fn canonical_electrode(geom: &ElectrodeGeometry) -> Option<ElectrodeMeshGroup> {
    build_electrode(
        geom,
        Vec3::ZERO,
        Vec3::new(0.0, 0.0, level_span(geom)),
        Some(Vec3::X),
        Some(Vec3::Y),
        CANONICAL_SHAFT_MM,
    )
}

/// Rows of the affine map from the [`canonical_electrode`] frame to world millimetres for a lead
/// with the given markers: `world = (row0 . p, row1 . p, row2 . p)` for `p = (x, y, z, 1)`.
pub fn lead_instance_matrix(
    geom: &ElectrodeGeometry,
    head: Vec3,
    tail: Vec3,
    x_ref: Option<Vec3>,
    y_ref: Option<Vec3>,
) -> Option<[[f32; 4]; 3]> {
    let frame = Frame::from_markers(head, tail, x_ref, y_ref)?;
    let s = axial_stretch(geom, (tail - head).length());
    let ez = frame.ez * s;
    Some([
        [frame.ex.x, frame.ey.x, ez.x, head.x],
        [frame.ex.y, frame.ey.y, ez.y, head.y],
        [frame.ex.z, frame.ey.z, ez.z, head.z],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbsloc_reconstruct::electrode_by_name;

    fn geom(name: &str) -> &'static ElectrodeGeometry {
        electrode_by_name(name).unwrap_or_else(|| panic!("{name} catalogued"))
    }

    #[test]
    fn builds_non_empty_geometry_for_ring_lead() {
        let g = geom("Medtronic 3389");
        let head = Vec3::new(0.0, 0.0, -20.0);
        let tail = Vec3::new(0.0, 0.0, 20.0);
        let x = Vec3::new(1.0, 0.0, -20.0);
        let m = build_electrode(g, head, tail, Some(x), None, 60.0).expect("mesh");
        assert!(!m.metal.indices.is_empty(), "contacts present");
        assert!(!m.insulation.indices.is_empty(), "shaft present");
        assert_eq!(m.metal.indices.len() % 3, 0);
        assert_eq!(m.insulation.indices.len() % 3, 0);
        for p in m
            .metal
            .positions
            .iter()
            .chain(m.insulation.positions.iter())
        {
            assert!(p.is_finite());
            assert!(p.x.abs() < 2.0 && p.y.abs() < 2.0, "off axis: {p:?}");
        }
    }

    #[test]
    fn directional_lead_has_arc_pads() {
        let g = geom("Boston Scientific Vercise Directed");
        assert!(g.is_directed);
        let head = Vec3::new(0.0, 0.0, 0.0);
        let tail = Vec3::new(0.0, 0.0, 6.0);
        let x = Vec3::new(1.0, 0.0, 0.0);
        let m = build_electrode(g, head, tail, Some(x), None, 40.0).expect("mesh");
        assert!(!m.metal.indices.is_empty());
        assert!(m
            .metal
            .positions
            .iter()
            .any(|p| (p.x * p.x + p.y * p.y).sqrt() > 0.5));
    }

    fn band_centres(m: &ElectrodeMeshGroup, g: &ElectrodeGeometry, s: f32) -> Vec<f32> {
        let mut out = Vec::new();
        for &z in &g.level_z_mm {
            let z = z as f32 * s;
            let half = 0.5 * g.contact_length_mm as f32 * s;
            let zs: Vec<f32> = m
                .metal
                .positions
                .iter()
                .map(|p| p.z)
                .filter(|&pz| pz >= z - half - 1e-3 && pz <= z + half + 1e-3)
                .collect();
            let lo = zs.iter().copied().fold(f32::MAX, f32::min);
            let hi = zs.iter().copied().fold(f32::MIN, f32::max);
            out.push(0.5 * (lo + hi));
        }
        out
    }

    #[test]
    fn exact_catalogue_span_keeps_unscaled_geometry() {
        let g = geom("Medtronic 3389");
        let span = (g.level_z_mm[g.level_z_mm.len() - 1] - g.level_z_mm[0]) as f32;
        assert!((axial_stretch(g, span) - 1.0).abs() < 1e-6);
        let m = build_electrode(g, Vec3::ZERO, Vec3::Z * span, Some(Vec3::X), None, 30.0)
            .expect("mesh");
        for (k, c) in band_centres(&m, g, 1.0).iter().enumerate() {
            assert!((c - g.level_z_mm[k] as f32).abs() < 1e-4);
        }
    }

    #[test]
    fn stretched_tail_scales_axial_positions_only() {
        let g = geom("Medtronic 3389");
        let span = (g.level_z_mm[g.level_z_mm.len() - 1] - g.level_z_mm[0]) as f32;
        let m = build_electrode(
            g,
            Vec3::ZERO,
            Vec3::Z * span * 1.3,
            Some(Vec3::X),
            None,
            30.0,
        )
        .expect("mesh");
        for (k, c) in band_centres(&m, g, 1.3).iter().enumerate() {
            let expected = 1.3 * g.level_z_mm[k] as f32;
            assert!((c - expected).abs() < 1e-4, "level {k}: {c} vs {expected}");
        }
        let r = 0.5 * g.lead_diameter_mm as f32;
        for p in &m.insulation.positions {
            assert!((p.x * p.x + p.y * p.y).sqrt() <= r + 1e-4);
        }
        let min_z = m
            .insulation
            .positions
            .iter()
            .map(|p| p.z)
            .fold(f32::MAX, f32::min);
        assert!((min_z + 1.3 * g.apex_to_first_level_mm() as f32).abs() < 1e-3);
    }

    #[test]
    fn degenerate_span_uses_unit_stretch() {
        let g = geom("Medtronic 3389");
        assert_eq!(axial_stretch(g, 0.0), 1.0);
        assert_eq!(axial_stretch(g, f32::NAN), 1.0);
        let m = build_electrode(g, Vec3::ZERO, Vec3::Z * 1e-5, Some(Vec3::X), None, 30.0);
        if let Some(m) = m {
            assert!(m.metal.positions.iter().all(|p| p.is_finite()));
        }
    }

    #[test]
    fn degenerate_axis_yields_none() {
        let g = geom("Medtronic 3387");
        let p = Vec3::new(1.0, 2.0, 3.0);
        assert!(build_electrode(g, p, p, None, None, 40.0).is_none());
    }

    fn dome_for(z_apex: f32, z_base: f32, r: f32) -> Builder {
        let frame = Frame::from_markers(Vec3::ZERO, Vec3::Z, Some(Vec3::X), None).expect("frame");
        let mut b = Builder::default();
        b.dome(&frame, z_apex, z_base, r, RADIAL_SEGMENTS);
        b
    }

    #[test]
    fn dome_is_rounded_and_closed_at_the_apex() {
        for (z_apex, z_base) in [(-1.5_f32, -0.5_f32), (-0.4, -0.2), (-3.0, -0.5)] {
            let r = 0.65_f32;
            let b = dome_for(z_apex, z_base, r);
            let min_z = b.positions.iter().map(|p| p.z).fold(f32::MAX, f32::min);
            let max_z = b.positions.iter().map(|p| p.z).fold(f32::MIN, f32::max);
            assert!(
                (min_z - z_apex).abs() < 1e-5,
                "apex stays at the catalogue tip"
            );
            assert!(
                (max_z - z_base).abs() < 1e-5,
                "base stays at the contact edge"
            );
            for p in &b.positions {
                assert!(
                    (p.x * p.x + p.y * p.y).sqrt() <= r + 1e-5,
                    "never wider than the lead"
                );
            }
            let at_base = b
                .positions
                .iter()
                .filter(|p| (p.z - z_base).abs() < 1e-5)
                .map(|p| (p.x * p.x + p.y * p.y).sqrt())
                .fold(0.0_f32, f32::max);
            assert!((at_base - r).abs() < 1e-4, "base ring has the lead radius");
        }
    }

    #[test]
    fn dome_normals_are_unit_and_faces_point_outward() {
        let b = dome_for(-1.5, -0.5, 0.65);
        for n in &b.normals {
            assert!((n.length() - 1.0).abs() < 1e-4, "unit normal: {n:?}");
        }
        for tri in b.indices.chunks(3) {
            let [a, bb, c] = [tri[0], tri[1], tri[2]].map(|i| b.positions[i as usize]);
            let face = (bb - a).cross(c - a);
            if face.length_squared() < 1e-12 {
                continue;
            }
            let vertex_normal = tri
                .iter()
                .map(|&i| b.normals[i as usize])
                .fold(Vec3::ZERO, |acc, n| acc + n);
            assert!(
                face.dot(vertex_normal) > 0.0,
                "winding agrees with the normals"
            );
        }
    }

    #[test]
    fn rounded_tip_does_not_move_the_apex() {
        let g = geom("Medtronic 3389");
        let span = (g.level_z_mm[g.level_z_mm.len() - 1] - g.level_z_mm[0]) as f32;
        let m = build_electrode(g, Vec3::ZERO, Vec3::Z * span, Some(Vec3::X), None, 30.0)
            .expect("mesh");
        let min_z = m
            .insulation
            .positions
            .iter()
            .chain(m.metal.positions.iter())
            .map(|p| p.z)
            .fold(f32::MAX, f32::min);
        assert!((min_z + g.apex_to_first_level_mm() as f32).abs() < 1e-4);
    }
}
