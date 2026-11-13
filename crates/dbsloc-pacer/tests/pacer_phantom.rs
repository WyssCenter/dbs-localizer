// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: AGPL-3.0-only

//! PaCER end to end on a synthetic native-space CT with known contact positions.
//!
//! `run_pacer` was ~1900 lines that nothing could call, so nothing had ever checked its output
//! against ground truth. These tests do, through the `pacer_sides` glue that turns its
//! polynomials into the fiducials a reconstruction is made of.

use ndarray::Array3;

use dbsloc_pacer::{pacer_sides, CtVolume, PacerConfig};
use dbsloc_reconstruct::{electrode_by_name, ElectrodeGeometry};

const VOX_MM: f64 = 0.5;
const N: usize = 120;
/// Shaft length painted. `specs/10` §5 sizes its component filter off a 40 mm minimum lead, so
/// a short phantom is discarded as too small before any geometry is looked at.
const SHAFT_MM: f64 = 55.0;
/// Apparent artifact radius. Metal blooms wider than the 0.635 mm shaft in a real CT — the
/// reason the lead samples as one continuous plateau on `sub-76325` — and the component filter
/// is calibrated for that, not for a geometrically exact cylinder.
const ARTIFACT_R_MM: f64 = 1.0;
/// Provided-mask cylinder radius around the lead, well beyond the 3 mm erosion.
const MASK_MARGIN_MM: f64 = 8.0;

/// Native post-op CT convention: metal is **bright** and PaCER reads it un-inverted
/// (`specs/05` §7), unlike the template-space `lpost` TRAC/CORE takes.
///
/// The whole shaft is high-HU, not just the contacts: four isolated contact blobs are not an
/// elongated object and PaCER rejects them as non-electrode shapes. Contacts are brighter
/// still, which is what its peak detection keys on.
fn phantom(e: &ElectrodeGeometry, apex: [f64; 3], dir: [f64; 3]) -> (CtVolume, [f64; 3]) {
    let aff: fastwarp_core::Mat4 = [
        [VOX_MM, 0.0, 0.0, -0.5 * VOX_MM * N as f64],
        [0.0, VOX_MM, 0.0, -0.5 * VOX_MM * N as f64],
        [0.0, 0.0, VOX_MM, -0.5 * VOX_MM * N as f64],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let n = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    let u = [dir[0] / n, dir[1] / n, dir[2] / n];

    let mut data = Array3::<f32>::from_elem((N, N, N), 30.0);
    let levels = e.apex_to_level_mm();
    let half: Vec<f64> = (0..e.n_levels())
        .map(|k| {
            if k == 0 && e.tip_is_contact {
                0.5 * e.tip_length_mm.max(e.contact_length_mm)
            } else {
                0.5 * e.contact_length_mm
            }
        })
        .collect();

    let end = [
        apex[0] + u[0] * SHAFT_MM,
        apex[1] + u[1] * SHAFT_MM,
        apex[2] + u[2] * SHAFT_MM,
    ];
    let range = |axis: usize| {
        let lo = apex[axis].min(end[axis]) - ARTIFACT_R_MM;
        let hi = apex[axis].max(end[axis]) + ARTIFACT_R_MM;
        let to_idx = |w: f64| (w - aff[axis][3]) / VOX_MM;
        let first = to_idx(lo).floor().max(0.0) as usize;
        let last = (to_idx(hi).ceil().max(0.0) as usize).min(N - 1);
        first..=last
    };
    for k in range(2) {
        for j in range(1) {
            for i in range(0) {
                let w = fastwarp_core::apply_rowmajor(&aff, [i as f64, j as f64, k as f64]);
                let rel = [w[0] - apex[0], w[1] - apex[1], w[2] - apex[2]];
                let a = rel[0] * u[0] + rel[1] * u[1] + rel[2] * u[2];
                let q = [rel[0] - a * u[0], rel[1] - a * u[1], rel[2] - a * u[2]];
                let rad = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
                if rad <= ARTIFACT_R_MM && (0.0..=SHAFT_MM).contains(&a) {
                    let on_contact = levels
                        .iter()
                        .zip(half.iter())
                        .any(|(c, h)| (a - c).abs() <= *h);
                    data[[k, j, i]] = if on_contact { 3000.0 } else { 2200.0 };
                }
            }
        }
    }
    (CtVolume::from_array(data, aff), u)
}

/// `no_mask`: the phantom has no skull or scalp, so brain masking has nothing to key on.
fn cfg() -> PacerConfig {
    PacerConfig {
        no_mask: true,
        vox_size_mm: [VOX_MM, VOX_MM, VOX_MM],
        distal_bloom_mm: 0.0,
        ..PacerConfig::default()
    }
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn worst_level_error(got: &[[f64; 3]], e: &ElectrodeGeometry, apex: [f64; 3], u: [f64; 3]) -> f64 {
    e.apex_to_level_mm()
        .into_iter()
        .enumerate()
        .map(|(k, arc)| {
            let want = [
                apex[0] + u[0] * arc,
                apex[1] + u[1] * arc,
                apex[2] + u[2] * arc,
            ];
            dist(got[k], want)
        })
        .fold(0.0f64, f64::max)
}

/// Measured 2026-10-23 on this phantom: worst level error 0.619 mm, span 8.999 mm,
/// recovered shaft 55.0 mm against 55.0 painted.
#[test]
fn recovers_ring_lead_levels_in_native_space() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let apex = [-10.0, -2.0, -27.0];
    let (vol, u) = phantom(e, apex, [0.12, 0.08, 1.0]);

    let sides = pacer_sides(&vol, &cfg(), Some("Medtronic 3387")).expect("pacer runs");
    assert_eq!(sides.len(), 1, "one lead painted, {} found", sides.len());
    let s = &sides[0];
    assert_eq!(s.elmodel, "Medtronic 3387");
    assert_eq!(s.level_mm.len(), e.n_levels());

    let worst = worst_level_error(&s.level_mm, e, apex, u);
    assert!(worst < 0.8, "worst level error {worst:.2} mm");
    assert!(
        (dist(s.head, s.tail) - e.contact_span_mm()).abs() < 0.3,
        "span {:.2} != model {:.2}",
        dist(s.head, s.tail),
        e.contact_span_mm()
    );
    assert!(s.shaft_len_mm > 10.0, "shaft {:.1} mm", s.shaft_len_mm);
}

/// The span must follow the pinned lead, so one hardcoded pitch cannot satisfy both.
#[test]
fn span_follows_the_pinned_lead() {
    let name = "Medtronic 3389";
    let e = electrode_by_name(name).expect("catalogued");
    let (vol, _u) = phantom(e, [-10.0, -2.0, -27.0], [0.12, 0.08, 1.0]);
    let sides = pacer_sides(&vol, &cfg(), Some(name)).expect("pacer runs");
    let s = &sides[0];
    assert!(
        (dist(s.head, s.tail) - e.contact_span_mm()).abs() < 0.3,
        "{name}: span {:.2} != {:.2}",
        dist(s.head, s.tail),
        e.contact_span_mm()
    );
}

#[test]
fn empty_volume_finds_no_electrode() {
    let vol = CtVolume::from_array(
        Array3::<f32>::from_elem((40, 40, 40), 30.0),
        fastwarp_core::identity_mat4(),
    );
    assert!(pacer_sides(&vol, &cfg(), Some("Medtronic 3387")).is_err());
}

fn masked_cfg(mask: Array3<bool>) -> PacerConfig {
    PacerConfig {
        no_mask: false,
        brain_mask: Some(std::sync::Arc::new(mask)),
        ..cfg()
    }
}

#[test]
fn a_provided_brain_mask_replaces_the_computed_one() {
    let e = electrode_by_name("Medtronic 3387").expect("catalogued");
    let apex = [-10.0, -2.0, -27.0];
    let (vol, u) = phantom(e, apex, [0.12, 0.08, 1.0]);

    let mut inside = Array3::from_elem((N, N, N), false);
    for k in 0..N {
        for j in 0..N {
            for i in 0..N {
                let w = fastwarp_core::apply_rowmajor(&vol.affine, [i as f64, j as f64, k as f64]);
                let rel = [w[0] - apex[0], w[1] - apex[1], w[2] - apex[2]];
                let a = rel[0] * u[0] + rel[1] * u[1] + rel[2] * u[2];
                let r2 = rel[0] * rel[0] + rel[1] * rel[1] + rel[2] * rel[2] - a * a;
                inside[[k, j, i]] = r2.sqrt() <= MASK_MARGIN_MM
                    && (-MASK_MARGIN_MM..=SHAFT_MM + MASK_MARGIN_MM).contains(&a);
            }
        }
    }
    let sides = pacer_sides(&vol, &masked_cfg(inside), Some("Medtronic 3387")).expect("pacer runs");
    assert_eq!(sides.len(), 1);
    assert!(worst_level_error(&sides[0].level_mm, e, apex, u) < 0.8);

    let outside = Array3::from_elem((N, N, N), false);
    let none = pacer_sides(&vol, &masked_cfg(outside), Some("Medtronic 3387"));
    assert!(
        matches!(none, Err(dbsloc_pacer::PacerError::NoElectrode)),
        "{none:?}"
    );
}

#[test]
fn a_brain_mask_on_another_grid_is_refused() {
    let vol = CtVolume::from_array(
        Array3::<f32>::from_elem((20, 20, 20), 30.0),
        fastwarp_core::identity_mat4(),
    );
    let wrong = Array3::from_elem((20, 20, 19), true);
    let got = pacer_sides(&vol, &masked_cfg(wrong), Some("Medtronic 3387"));
    assert!(
        matches!(got, Err(dbsloc_pacer::PacerError::BrainMaskGrid { .. })),
        "{got:?}"
    );
}
