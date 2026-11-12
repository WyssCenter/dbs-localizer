// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Electrode prototype geometry — from device IFU / public specs (not Matlab). See `10_pacer` §9.

use super::errors::{PacerError, PacerResult};

/// Prototype for typing + zero calibration.
#[derive(Clone, Debug)]
pub struct ElectrodeSpec {
    pub name: &'static str,
    /// Pairwise spacings (mm) between the four **ring** contacts, length 3.
    pub diffs_mm: [f32; 3],
    /// Arc from distal tip to each ring contact centre (mm), length 4.
    pub ring_contact_centers_mm: [f32; 4],
    pub zero_to_first_peak_mm: f32,
    pub diameter_mm: f32,
}

/// Medtronic 3389 — from device IFU (`10_pacer` §9): 1.5 mm contacts, 0.5 mm gaps, 1.5 mm tip.
///
/// Centre-to-centre **pitch** is `contact_length + contact_spacing` = 2.0 mm, and the first
/// contact centre lies `tip_length + contact_length/2` = 2.25 mm of arc from the tip. Contact
/// span is `4·1.5 + 3·0.5` = 7.5 mm, the value the §156 width heuristic separates on.
pub fn medtronic_3389() -> ElectrodeSpec {
    ElectrodeSpec {
        name: "Medtronic 3389",
        diffs_mm: [2.0, 2.0, 2.0],
        ring_contact_centers_mm: [2.25, 4.25, 6.25, 8.25],
        zero_to_first_peak_mm: 2.25,
        diameter_mm: 1.27,
    }
}

/// Medtronic 3387 — 1.5 mm contacts, 1.5 mm gaps, 1.5 mm tip: pitch 3.0 mm, first centre at
/// 2.25 mm, contact span `4·1.5 + 3·1.5` = 10.5 mm.
pub fn medtronic_3387() -> ElectrodeSpec {
    ElectrodeSpec {
        name: "Medtronic 3387",
        diffs_mm: [3.0, 3.0, 3.0],
        ring_contact_centers_mm: [2.25, 5.25, 8.25, 11.25],
        zero_to_first_peak_mm: 2.25,
        diameter_mm: 1.27,
    }
}

/// Boston Scientific Vercise Directed: 8 contacts in **4 levels** (ring, 3 segments,
/// 3 segments, ring) at the same 2.0 mm level pitch as the 3389. PaCER types leads from the
/// axial intensity profile, which resolves levels rather than individual segments, so the
/// level geometry is what belongs here — but under its own name, so `best_match` does not
/// report a Boston lead as a Medtronic one.
///
/// Unlike the Medtronic leads this one is `tipiscontact`: the metal reaches the apex, so the
/// first level centre is `tip_length/2` = 0.75 mm, not `tip_length + contact_length/2` =
/// 2.25 mm. These were written as 2.25 by analogy with the 3389 in roadmap 2.5, which offset
/// PaCER's `t0` calibration by 1.5 mm for every tip-is-contact lead.
pub fn boston_verse() -> ElectrodeSpec {
    ElectrodeSpec {
        name: "Boston Scientific Vercise Directed",
        diffs_mm: [2.0, 2.0, 2.0],
        ring_contact_centers_mm: [0.75, 2.75, 4.75, 6.75],
        zero_to_first_peak_mm: 0.75,
        diameter_mm: 1.3,
    }
}

fn all() -> [ElectrodeSpec; 3] {
    [medtronic_3389(), medtronic_3387(), boston_verse()]
}

/// Match peaks (arc mm along shaft) to closest prototype. Returns (spec, l2, rms).
pub fn best_match(peak_s: &[f32]) -> (Option<ElectrodeSpec>, f64, f64) {
    if peak_s.len() < 2 {
        return (None, f64::INFINITY, f64::INFINITY);
    }
    let d_meas: Vec<f64> = (0..(peak_s.len() - 1))
        .map(|i| (peak_s[i + 1] - peak_s[i]) as f64)
        .collect();
    if d_meas.len() < 3 {
        return (None, f64::INFINITY, f64::INFINITY);
    }
    let d_meas = [d_meas[0], d_meas[1], d_meas[2]];
    let mut best: Option<ElectrodeSpec> = None;
    let mut best_d = f64::INFINITY;
    let mut tied = false;
    for e in all() {
        let dif: f64 = (0..3)
            .map(|i| d_meas[i] - e.diffs_mm[i] as f64)
            .map(|x| x * x)
            .sum::<f64>()
            .sqrt();
        if dif < best_d {
            best_d = dif;
            best = Some(e);
            tied = false;
        } else if dif == best_d {
            tied = true;
        }
    }
    if tied {
        return (None, f64::INFINITY, f64::INFINITY);
    }
    let rms = if let Some(ref e) = best {
        let s: f64 = (0..3)
            .map(|i| (d_meas[i] - e.diffs_mm[i] as f64).powi(2))
            .sum();
        (s / 3.0).sqrt()
    } else {
        f64::INFINITY
    };
    (best, best_d, rms)
}

/// Contact span (first contact start → last contact end), the width the area-based fallback
/// measures. `10_pacer` §156 splits 3389-class from 3387-class at 10.5 mm.
pub fn contact_span_mm(e: &ElectrodeSpec) -> f32 {
    const CONTACT_LEN_MM: f32 = 1.5;
    e.ring_contact_centers_mm[3] - e.ring_contact_centers_mm[0] + CONTACT_LEN_MM
}

/// Resolve user name.
pub fn by_name(n: &str) -> PacerResult<ElectrodeSpec> {
    let n = n.to_ascii_lowercase();
    if n.contains("3389") {
        return Ok(medtronic_3389());
    }
    if n.contains("3387") {
        return Ok(medtronic_3387());
    }
    if n.contains("verc") || n.contains("boston") {
        return Ok(boston_verse());
    }
    Err(PacerError::UnknownElectrode(n.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pitch is `contact_length + contact_spacing`, not the spacing. Both Medtronic leads have
    /// 1.5 mm contacts, so 3389 (0.5 mm gaps) pitches at 2.0 mm and 3387 (1.5 mm gaps) at 3.0 mm.
    #[test]
    fn medtronic_pitches_are_centre_to_centre() {
        let e89 = medtronic_3389();
        let e87 = medtronic_3387();
        assert_eq!(e89.diffs_mm, [2.0, 2.0, 2.0]);
        assert_eq!(e87.diffs_mm, [3.0, 3.0, 3.0]);
        for e in [&e89, &e87] {
            for i in 1..4 {
                let d = e.ring_contact_centers_mm[i] - e.ring_contact_centers_mm[i - 1];
                assert!(
                    (d - e.diffs_mm[i - 1]).abs() < 1e-5,
                    "{}: centre gap {d} != pitch {}",
                    e.name,
                    e.diffs_mm[i - 1]
                );
            }
        }
    }

    /// The first contact centre sits a real distance from the tip, not at zero, or `t0`
    /// calibration is offset by that much. How far depends on `tipiscontact`: an insulating
    /// tip puts it at `tip_length + contact_length/2`, a contact tip at `tip_length/2`.
    #[test]
    fn first_contact_centre_is_offset_from_tip() {
        for (e, expect) in [
            (medtronic_3389(), 2.25),
            (medtronic_3387(), 2.25),
            (boston_verse(), 0.75),
        ] {
            assert!(
                (e.ring_contact_centers_mm[0] - expect).abs() < 1e-5,
                "{}: first centre {} != {expect}",
                e.name,
                e.ring_contact_centers_mm[0]
            );
            assert!((e.zero_to_first_peak_mm - e.ring_contact_centers_mm[0]).abs() < 1e-5);
        }
    }

    /// `10_pacer` §156 splits the two classes at 10.5 mm of contact span.
    #[test]
    fn contact_span_matches_the_width_heuristic() {
        assert!((contact_span_mm(&medtronic_3389()) - 7.5).abs() < 1e-5);
        assert!((contact_span_mm(&medtronic_3387()) - 10.5).abs() < 1e-5);
        assert!(contact_span_mm(&medtronic_3389()) < 10.5);
        assert!(contact_span_mm(&medtronic_3387()) >= 10.5);
    }

    /// Typing must recover the right lead from ideal peak positions, and must not report a
    /// Boston lead under a Medtronic name.
    #[test]
    fn best_match_recovers_each_lead() {
        let expected = medtronic_3387();
        let (got, _d, rms) = best_match(&expected.ring_contact_centers_mm);
        let got = got.expect("a prototype matched");
        assert_eq!(got.name, expected.name);
        assert!(rms < 1e-4, "{}: rms {rms}", expected.name);
    }

    #[test]
    fn spacing_shared_by_two_prototypes_is_ambiguous() {
        assert_eq!(medtronic_3389().diffs_mm, boston_verse().diffs_mm);
        for e in [medtronic_3389(), boston_verse()] {
            let (got, d, rms) = best_match(&e.ring_contact_centers_mm);
            assert!(
                got.is_none(),
                "{}: typed as {:?}",
                e.name,
                got.map(|g| g.name)
            );
            assert!(d.is_infinite() && rms.is_infinite());
        }
        let (got, _, _) = best_match(&[0.75, 2.8, 4.7, 6.8]);
        assert!(got.is_none());
    }

    #[test]
    fn by_name_keeps_vendor_identity() {
        assert_eq!(by_name("boston vercise").unwrap().name, boston_verse().name);
        assert_eq!(by_name("Medtronic 3387").unwrap().name, "Medtronic 3387");
        assert!(by_name("not a lead").is_err());
    }

    /// These prototypes exist so PaCER can type a lead from its intensity peaks. They must
    /// agree with the shared catalogue, or PaCER names a lead whose geometry the reconstruct
    /// stage then rejects.
    #[test]
    fn prototypes_agree_with_the_shared_catalogue() {
        for (proto, name) in [
            (medtronic_3389(), "Medtronic 3389"),
            (medtronic_3387(), "Medtronic 3387"),
            (boston_verse(), "Boston Scientific Vercise Directed"),
        ] {
            let e = crate::electrode_by_name(name).unwrap_or_else(|| panic!("{name} catalogued"));
            assert!(
                (proto.diffs_mm[0] as f64 - e.eldist_mm).abs() < 1e-4,
                "{name}: pacer pitch {} != catalogue {}",
                proto.diffs_mm[0],
                e.eldist_mm
            );
            assert!(
                (proto.diameter_mm as f64 - e.lead_diameter_mm).abs() < 1e-4,
                "{name}: pacer diameter {} != catalogue {}",
                proto.diameter_mm,
                e.lead_diameter_mm
            );
            assert_eq!(
                proto.ring_contact_centers_mm.len(),
                e.n_levels(),
                "{name}: pacer levels != catalogue levels"
            );
            assert!(
                (proto.zero_to_first_peak_mm as f64 - e.apex_to_first_level_mm()).abs() < 1e-4,
                "{name}: pacer apex offset {} != catalogue {}",
                proto.zero_to_first_peak_mm,
                e.apex_to_first_level_mm()
            );
            for (i, (got, want)) in proto
                .ring_contact_centers_mm
                .iter()
                .zip(e.apex_to_level_mm().iter())
                .enumerate()
            {
                assert!(
                    (*got as f64 - *want).abs() < 1e-4,
                    "{name}: level {i} centre {got} != catalogue {want}"
                );
            }
        }
    }
}
