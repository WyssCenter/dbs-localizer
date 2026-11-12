// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! TRAC/CORE against a synthetic lead with known contact positions.
//!
//! The point of these tests is that they can fail. The previous tracer reported the centreline
//! extremes as `head`/`tail`, which no assertion on output *shape* catches — the arrays were the
//! right size and the values were finite. Only ground truth catches it, so the phantom is built
//! from the electrode geometry and the recovered fiducials are compared against the coordinates
//! that were painted.

use ndarray::Array3;

use dbsloc_reconstruct::{
    apex_to_proximal_metal_mm, core_on_line, electrode_by_name, trace_side, ElectrodeGeometry,
    TracConfig, TracError,
};

const VOX_MM: f64 = 0.5;
const NU: usize = 96;
const NV: usize = 96;
const NS: usize = 96;

/// Radiological-convention affine (negative *x*), 0.5 mm isotropic, centred near the origin so
/// the spec's +8.7 / −15.5 mm z window falls inside the volume.
fn phantom_affine() -> fastwarp_core::Mat4 {
    [
        [-VOX_MM, 0.0, 0.0, 0.5 * VOX_MM * NU as f64],
        [0.0, VOX_MM, 0.0, -0.5 * VOX_MM * NV as f64],
        [0.0, 0.0, VOX_MM, -0.5 * VOX_MM * NS as f64],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn world_of(aff: &fastwarp_core::Mat4, i: usize, j: usize, k: usize) -> [f64; 3] {
    fastwarp_core::apply_rowmajor(aff, [i as f64, j as f64, k as f64])
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / n, v[1] / n, v[2] / n]
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Perpendicular distance of `p` from the line through `o` with direction `u`.
fn radial(p: [f64; 3], o: [f64; 3], u: [f64; 3]) -> f64 {
    let r = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
    let a = r[0] * u[0] + r[1] * u[1] + r[2] * u[2];
    let q = [r[0] - a * u[0], r[1] - a * u[1], r[2] - a * u[2]];
    (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt()
}

struct Phantom {
    data: Array3<f64>,
    aff: fastwarp_core::Mat4,
    /// Centre of the most-distal level: the `head` a correct reconstruction must return.
    head: [f64; 3],
    tail: [f64; 3],
}

/// Intensities for one phantom, in the volume's own convention (`metal_bright` false is the
/// inverted `lpost` TRAC/CORE reads).
#[derive(Clone, Copy)]
struct Paint {
    shaft: f64,
    metal: f64,
    /// Apparent radius of the artifact. The bare lead by default; a real CT blooms far wider.
    artifact_r_mm: f64,
    /// A skull-like disc through every slice: `(centre_xy, radius_mm, intensity)`.
    bone: Option<([f64; 2], f64, f64)>,
}

/// Paint a slanted lead: a background intensity ramp standing in for tissue structure (so the
/// threshold has something other than the lead to reject), a shaft of insulator, metal at each
/// level, and a tip. `metal_bright` mirrors the CT convention before `lpost` inversion.
fn build_phantom(
    e: &ElectrodeGeometry,
    head: [f64; 3],
    u_shaft: [f64; 3],
    metal_bright: bool,
) -> Phantom {
    let paint = Paint {
        shaft: if metal_bright { 120.0 } else { -40.0 },
        metal: if metal_bright { 2200.0 } else { -900.0 },
        artifact_r_mm: 0.5 * e.lead_diameter_mm,
        bone: None,
    };
    build_phantom_on(phantom_affine(), (NU, NV, NS), e, head, u_shaft, paint)
}

fn build_phantom_on(
    aff: fastwarp_core::Mat4,
    dims: (usize, usize, usize),
    e: &ElectrodeGeometry,
    head: [f64; 3],
    u_shaft: [f64; 3],
    paint: Paint,
) -> Phantom {
    let (nu, nv, ns) = dims;
    let mut data = Array3::<f64>::zeros((nu, nv, ns));
    let bg = 40.0;
    let shaft = paint.shaft;
    let metal = paint.metal;
    let r_lead = paint.artifact_r_mm;

    let intervals: Vec<(f64, f64)> = e
        .level_z_mm
        .iter()
        .enumerate()
        .map(|(k, &z)| {
            let half = if k == 0 && e.tip_is_contact {
                0.5 * e.tip_length_mm.max(e.contact_length_mm)
            } else {
                0.5 * e.contact_length_mm
            };
            (z - half, z + half)
        })
        .collect();
    let z_lo = intervals[0].0 - e.tip_length_mm;
    let z_hi = 40.0;

    for i in 0..nu {
        for j in 0..nv {
            for k in 0..ns {
                let w = world_of(&aff, i, j, k);
                let mut v = bg + 0.4 * w[2] + 0.2 * w[0];
                if let Some(([bx, by], br, bv)) = paint.bone {
                    if ((w[0] - bx).powi(2) + (w[1] - by).powi(2)).sqrt() <= br {
                        v = bv;
                    }
                }
                let rel = [w[0] - head[0], w[1] - head[1], w[2] - head[2]];
                let a = rel[0] * u_shaft[0] + rel[1] * u_shaft[1] + rel[2] * u_shaft[2];
                let rad = radial(w, head, u_shaft);
                if rad <= r_lead && a >= z_lo && a <= z_hi {
                    v = shaft;
                    if intervals.iter().any(|&(lo, hi)| a >= lo && a <= hi) {
                        v = metal;
                    }
                }
                data[[i, j, k]] = v;
            }
        }
    }
    let span = e.contact_span_mm();
    let tail = [
        head[0] + u_shaft[0] * span,
        head[1] + u_shaft[1] * span,
        head[2] + u_shaft[2] * span,
    ];
    Phantom {
        data,
        aff,
        head,
        tail,
    }
}

/// Left hemisphere, tip at negative *x*, slanted so the trace has to track in both in-plane
/// axes rather than sitting in one column of voxels.
fn left_lead(_e: &ElectrodeGeometry) -> ([f64; 3], [f64; 3]) {
    let head = [-11.0, -3.0, -9.0];
    let u = unit([0.18, 0.10, 1.0]);
    (head, u)
}

/// The phantom paints metal at its true extent, so the blooming allowance is switched off.
fn cfg() -> TracConfig {
    TracConfig {
        distal_bloom_mm: 0.0,
        ..TracConfig::default()
    }
}

/// `lpost` for CT is intensity-inverted upstream (`specs/05` §4.1), so metal reads dark.
#[test]
fn recovers_ring_lead_contacts_from_inverted_ct() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let (head, u) = left_lead(e);
    let ph = build_phantom(e, head, u, false);
    let r = trace_side(&ph.data, &ph.aff, 1, true, e, &cfg()).expect("trace succeeds");

    assert!(
        dist(r.head, ph.head) < 1.0,
        "head {:?} vs truth {:?} = {:.2} mm (metal {:.2} vs model {:.2})",
        r.head,
        ph.head,
        dist(r.head, ph.head),
        r.metal_extent_mm,
        r.model_metal_extent_mm
    );
    assert!(
        dist(r.tail, ph.tail) < 1.0,
        "tail {:?} vs truth {:?} = {:.2} mm",
        r.tail,
        ph.tail,
        dist(r.tail, ph.tail)
    );
    assert!(
        (r.metal_extent_mm - r.model_metal_extent_mm).abs() < 1.5,
        "metal extent {:.2} vs model {:.2}",
        r.metal_extent_mm,
        r.model_metal_extent_mm
    );
}

/// Every lead the validation cohort uses, in both metal polarities. Measured 2026-09-10 on
/// this 0.5 mm phantom: head error 0.28–0.55 mm for the 4-level leads, 0.67–1.00 mm for the
/// 8-ring Vercise, whose longer uniform comb is the weakest case. The bound is deliberately
/// loose enough not to be brittle and tight enough that a regression to centreline extremes
/// (180 mm on real data) or a hardcoded pitch cannot pass.
#[test]
fn every_cohort_lead_recovers_within_a_voxel_or_two() {
    let mut worst = 0.0f64;
    let mut worst_name = String::new();
    for name in [
        "Medtronic 3387",
        "Medtronic 3389",
        "Boston Scientific Vercise Directed",
        "Medtronic B33005",
        "Abbott Directed 6172 (short)",
        "Boston Scientific Vercise",
    ] {
        let e = electrode_by_name(name).expect("catalogued");
        let (head, u) = left_lead(e);
        for dark in [true, false] {
            let ph = build_phantom(e, head, u, !dark);
            let r = trace_side(&ph.data, &ph.aff, 1, dark, e, &cfg())
                .unwrap_or_else(|err| panic!("{name} dark={dark}: {err}"));
            let d = dist(r.head, ph.head);
            if d > worst {
                worst = d;
                worst_name = format!("{name} dark={dark}");
            }
            let rel = [
                r.head[0] - ph.head[0],
                r.head[1] - ph.head[1],
                r.head[2] - ph.head[2],
            ];
            let along = (rel[0] * u[0] + rel[1] * u[1] + rel[2] * u[2]).abs();
            assert!(
                along < 0.3,
                "{name} dark={dark}: {along:.3} mm along the shaft is CORE's own error"
            );
            assert!(d < 1.2, "{name} dark={dark}: head off by {d:.3} mm");
            assert!(
                (dist(r.head, r.tail) - e.contact_span_mm()).abs() < 0.2,
                "{name}: span {:.2} != model {:.2}",
                dist(r.head, r.tail),
                e.contact_span_mm()
            );
            assert!(
                (r.metal_extent_mm - r.model_metal_extent_mm).abs() < 1.5,
                "{name} dark={dark}: metal extent {:.2} vs model {:.2}",
                r.metal_extent_mm,
                r.model_metal_extent_mm
            );
        }
    }
    assert!(worst < 1.2, "worst {worst_name}: {worst:.3} mm");
}

#[test]
fn recovers_ring_lead_from_bright_metal() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let (head, u) = left_lead(e);
    let ph = build_phantom(e, head, u, true);
    let r = trace_side(&ph.data, &ph.aff, 1, false, e, &cfg()).expect("trace succeeds");
    assert!(
        dist(r.head, ph.head) < 1.0,
        "head off by {:.2} mm",
        dist(r.head, ph.head)
    );
}

/// The 3389's 2.0 mm pitch must land the contacts 2.0 mm apart, not 3.0. A tracer that only
/// finds the shaft and spaces contacts by a hardcoded pitch passes the 3387 test and fails this.
#[test]
fn pitch_comes_from_the_matched_lead() {
    for name in ["Medtronic 3387", "Medtronic 3389"] {
        let e = electrode_by_name(name).expect("catalogued");
        let (head, u) = left_lead(e);
        let ph = build_phantom(e, head, u, false);
        let r = trace_side(&ph.data, &ph.aff, 1, true, e, &cfg()).expect("trace succeeds");
        let span = dist(r.head, r.tail);
        assert!(
            (span - e.contact_span_mm()).abs() < 0.2,
            "{name}: span {span:.2} != {:.2}",
            e.contact_span_mm()
        );
        assert!(
            dist(r.head, ph.head) < 1.0,
            "{name}: head off by {:.2} mm",
            dist(r.head, ph.head)
        );
    }
}

/// 9 of the 10 Lead-Tutor subjects carry these. Four levels of metal at 2.0 mm produce the same
/// axial comb as a 4-ring lead, so TRAC/CORE must place them without needing roll.
#[test]
fn recovers_directional_lead_levels() {
    let e = electrode_by_name("Boston Scientific Vercise Directed").expect("catalogued");
    let (head, u) = left_lead(e);
    let ph = build_phantom(e, head, u, false);
    let r = trace_side(&ph.data, &ph.aff, 1, true, e, &cfg()).expect("trace succeeds");
    assert!(
        dist(r.head, ph.head) < 1.0,
        "head {:?} vs truth {:?} = {:.2} mm",
        r.head,
        ph.head,
        dist(r.head, ph.head)
    );
    assert!((dist(r.head, r.tail) - 6.0).abs() < 0.2);
}

/// `head`/`tail` must be contact 0 and the last level, not the ends of the traced centreline.
/// The centreline spans the whole z window — about 24 mm — so the two are far apart.
#[test]
fn fiducials_are_contacts_not_centreline_extremes() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let (head, u) = left_lead(e);
    let ph = build_phantom(e, head, u, false);
    let r = trace_side(&ph.data, &ph.aff, 1, true, e, &cfg()).expect("trace succeeds");

    let ends = dist(r.centreline[0], r.centreline[r.centreline.len() - 1]);
    assert!(ends > 15.0, "centreline only spans {ends:.1} mm");
    assert!(
        (dist(r.head, r.tail) - 9.0).abs() < 0.2,
        "head→tail {:.2} mm should be the 9 mm model span, not the {ends:.1} mm centreline",
        dist(r.head, r.tail)
    );
}

/// The right-hemisphere seed must find the right-hemisphere lead. Splitting on the median *x*
/// of a threshold mask put both sides on whichever artifact was larger. Both leads live in one
/// volume here, combined by keeping the darker (metal) value at each voxel.
#[test]
fn each_hemisphere_finds_its_own_lead() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let u = unit([0.18, 0.10, 1.0]);
    let head_l = [-11.0, -3.0, -9.0];
    let head_r = [11.5, -2.0, -9.5];
    let mut ph = build_phantom(e, head_l, u, false);
    let ph_r = build_phantom(e, head_r, u, false);
    for (a, b) in ph.data.iter_mut().zip(ph_r.data.iter()) {
        if *b < *a {
            *a = *b;
        }
    }

    let l = trace_side(&ph.data, &ph.aff, 1, true, e, &cfg()).expect("left");
    let r = trace_side(&ph.data, &ph.aff, 2, true, e, &cfg()).expect("right");
    assert!(
        dist(l.head, head_l) < 1.0,
        "left head off by {:.2} mm",
        dist(l.head, head_l)
    );
    assert!(
        dist(r.head, head_r) < 1.0,
        "right head off by {:.2} mm",
        dist(r.head, head_r)
    );
    assert!(l.head[0] < 0.0 && r.head[0] > 0.0, "hemispheres swapped");
}

/// A volume with no lead must fail, not return the best available noise.
#[test]
fn empty_volume_is_rejected() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let aff = phantom_affine();
    let mut data = Array3::<f64>::zeros((NU, NV, NS));
    for i in 0..NU {
        for j in 0..NV {
            for k in 0..NS {
                let w = world_of(&aff, i, j, k);
                data[[i, j, k]] = 40.0 + 0.4 * w[2] + 0.2 * w[0];
            }
        }
    }
    let got = trace_side(&data, &aff, 1, true, e, &cfg());
    assert!(
        matches!(
            got,
            Err(TracError::NoEntry)
                | Err(TracError::NoContactMatch)
                | Err(TracError::ShortCentreline)
        ),
        "expected a refusal, got {got:?}"
    );
}

/// 0.5 mm radiological grid covering the given world ranges (mm), *x* descending with index
/// as in [`phantom_affine`].
fn affine_covering(
    x: (f64, f64),
    y: (f64, f64),
    z: (f64, f64),
) -> (fastwarp_core::Mat4, (usize, usize, usize)) {
    let n = |lo: f64, hi: f64| ((hi - lo) / VOX_MM).round() as usize + 1;
    let aff = [
        [-VOX_MM, 0.0, 0.0, x.1],
        [0.0, VOX_MM, 0.0, y.0],
        [0.0, 0.0, VOX_MM, z.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    (aff, (n(x.0, x.1), n(y.0, y.1), n(z.0, z.1)))
}

/// Where the frontal bone meets the corner of a 45 × 60 mm seed box at the +8.7 mm start plane.
const SKULL_SLIVER_XY: [f64; 2] = [-43.5, 58.0];

/// What seeded four of the cohort's left leads on the frontal skull (2026-09-11). The bloomed
/// artifact averages 17 mm² in cross-section, over the old 12 mm² "compact" limit, while the
/// sliver of frontal bone at the corner of the search box is 11 mm² and passes it. The sliver is
/// painted denser than the lead body, as bone is against a shaft, so intensity cannot separate
/// them either: only the shape of the search region can.
#[test]
fn seed_ignores_the_skull_sliver_at_the_search_corner() {
    let e = electrode_by_name("Boston Scientific Vercise Directed").expect("catalogued");
    let (head, u) = left_lead(e);
    let (aff, dims) = affine_covering((-52.0, 8.0), (-30.0, 66.0), (-24.0, 24.0));
    let paint = Paint {
        shaft: -700.0,
        metal: -2400.0,
        artifact_r_mm: 2.3,
        bone: Some((SKULL_SLIVER_XY, 1.8, -1500.0)),
    };
    let ph = build_phantom_on(aff, dims, e, head, u, paint);
    let r = trace_side(&ph.data, &ph.aff, 1, true, e, &cfg()).expect("trace succeeds");
    assert!(
        r.entry[1] < 20.0,
        "seeded on the skull sliver: entry {:?}",
        r.entry
    );
    assert!(
        dist(r.head, ph.head) < 1.5,
        "head {:?} vs truth {:?} = {:.2} mm (entry {:?})",
        r.head,
        ph.head,
        dist(r.head, ph.head),
        r.entry
    );
}

/// Bone alone, inside the search box but outside the ellipse, is not an entry. The earlier
/// fallback to "any component when nothing is compact" made it one.
#[test]
fn skull_alone_is_not_an_entry() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let (aff, dims) = affine_covering((-52.0, 8.0), (-30.0, 66.0), (-24.0, 24.0));
    let (nu, nv, ns) = dims;
    let mut data = Array3::<f64>::zeros(dims);
    for i in 0..nu {
        for j in 0..nv {
            for k in 0..ns {
                let w = world_of(&aff, i, j, k);
                let mut v = 40.0 + 0.4 * w[2] + 0.2 * w[0];
                let d = ((w[0] - SKULL_SLIVER_XY[0]).powi(2) + (w[1] - SKULL_SLIVER_XY[1]).powi(2))
                    .sqrt();
                if d <= 1.8 {
                    v = -1500.0;
                }
                data[[i, j, k]] = v;
            }
        }
    }
    let got = trace_side(&data, &aff, 1, true, e, &cfg());
    assert!(
        matches!(got, Err(TracError::NoEntry)),
        "expected NoEntry, got {got:?}"
    );
}

#[test]
fn core_on_a_supplied_steep_line_places_the_contacts() {
    let e = electrode_by_name("Boston Scientific Vercise Directed").expect("catalogued");
    let head = [-11.0, -3.0, -9.0];
    let u = unit([-0.3, 1.6, 1.0]);
    let ph = build_phantom(e, head, u, false);
    let deep = [
        head[0] - 4.0 * u[0],
        head[1] - 4.0 * u[1],
        head[2] - 4.0 * u[2],
    ];
    let shallow = [
        head[0] + 18.0 * u[0],
        head[1] + 18.0 * u[1],
        head[2] + 18.0 * u[2],
    ];
    let r = core_on_line(&ph.data, &ph.aff, deep, shallow, true, e, &cfg()).expect("placed");
    assert!(
        dist(r.head, ph.head) < 0.3,
        "head {:?} vs truth {:?} = {:.2} mm",
        r.head,
        ph.head,
        dist(r.head, ph.head)
    );
    assert!(dist(r.tail, ph.tail) < 0.3);
}

#[test]
fn core_on_line_refuses_a_line_ending_below_the_proximal_level() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let head = [-11.0, -3.0, -9.0];
    let u = unit([0.18, 0.10, 1.0]);
    let ph = build_phantom(e, head, u, false);
    let deep = [
        head[0] - 3.0 * u[0],
        head[1] - 3.0 * u[1],
        head[2] - 3.0 * u[2],
    ];
    let short = apex_to_proximal_metal_mm(e) - 1.0;
    let shallow = [
        deep[0] + short * u[0],
        deep[1] + short * u[1],
        deep[2] + short * u[2],
    ];
    let got = core_on_line(&ph.data, &ph.aff, deep, shallow, true, e, &cfg());
    assert!(
        matches!(got, Err(TracError::ShorterThanElectrode)),
        "{:?}",
        got.map(|r| r.head)
    );
}

fn translated(m: &fastwarp_core::Mat4, t: [f64; 3]) -> fastwarp_core::Mat4 {
    let mut out = *m;
    for a in 0..3 {
        out[a][3] += t[a];
    }
    out
}

fn translation(t: [f64; 3]) -> fastwarp_core::Mat4 {
    let mut out = fastwarp_core::identity_mat4();
    for a in 0..3 {
        out[a][3] = t[a];
    }
    out
}

#[test]
fn an_identity_world_to_reference_matches_the_default_windows() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let (head, u) = left_lead(e);
    let ph = build_phantom(e, head, u, false);
    let plain = trace_side(&ph.data, &ph.aff, 1, true, e, &cfg()).expect("default windows");
    let mapped_cfg = TracConfig {
        world_to_reference: Some(fastwarp_core::identity_mat4()),
        ..cfg()
    };
    let mapped = trace_side(&ph.data, &ph.aff, 1, true, e, &mapped_cfg).expect("identity map");
    assert!(dist(plain.head, mapped.head) < 1e-9);
    assert!(dist(plain.tail, mapped.tail) < 1e-9);
    assert!(dist(plain.entry, mapped.entry) < 1e-9);
    assert_eq!(plain.centreline.len(), mapped.centreline.len());
}

#[test]
fn a_translated_grid_with_the_matching_map_finds_the_same_lead() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let (head, u) = left_lead(e);
    let ph = build_phantom(e, head, u, false);
    let t = [6.0, -4.0, 30.0];
    let aff_shifted = translated(&ph.aff, t);
    let reference = trace_side(&ph.data, &ph.aff, 1, true, e, &cfg()).expect("untranslated");

    let mapped_cfg = TracConfig {
        world_to_reference: Some(translation([-t[0], -t[1], -t[2]])),
        ..cfg()
    };
    let shifted =
        trace_side(&ph.data, &aff_shifted, 1, true, e, &mapped_cfg).expect("translated, mapped");
    let want = [
        reference.head[0] + t[0],
        reference.head[1] + t[1],
        reference.head[2] + t[2],
    ];
    assert!(
        dist(shifted.head, want) < 1e-6,
        "{:?} vs {want:?}",
        shifted.head
    );
    assert!(dist(shifted.entry, reference.entry) > 1.0);
    let entry_back = [
        shifted.entry[0] - t[0],
        shifted.entry[1] - t[1],
        shifted.entry[2] - t[2],
    ];
    assert!(dist(entry_back, reference.entry) < 1e-6);

    match trace_side(&ph.data, &aff_shifted, 1, true, e, &cfg()) {
        Err(_) => {}
        Ok(r) => assert!(
            dist(r.head, want) > 2.0,
            "reference-space windows must not work in a shifted space"
        ),
    }
}

#[test]
fn a_singular_world_to_reference_is_an_error() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let (head, u) = left_lead(e);
    let ph = build_phantom(e, head, u, false);
    let cfg = TracConfig {
        world_to_reference: Some([[0.0; 4]; 4]),
        ..cfg()
    };
    assert!(trace_side(&ph.data, &ph.aff, 1, true, e, &cfg).is_err());
}
