// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Resolved electrode geometry for all catalogued leads (`specs/09` §2, §2.2, §3.1–§3.2).
//!
//! The reconstruct stage used to hardcode a 1.5 mm pitch, 4-contact lead that matches no real
//! device, so every reconstruction and every plausibility tolerance was computed against
//! fictional geometry. This module replaces that with the catalogue.
//!
//! The derivation (spacing overrides, `etageidx` archetypes, segment azimuths, array span,
//! tail fiducial level, segment arc) lives in
//! `dbsloc.electrodes.layout` on the Python side. Porting it here would create two copies to
//! keep in step, so `scripts/emit_electrode_geometry.py` runs it once and the result is
//! embedded as data; `tests/test_electrode_geometry_export.py` fails if the checked-in file
//! and the live derivation disagree.

use std::sync::OnceLock;

use serde::Deserialize;

const GEOMETRY_JSON: &str = include_str!("../data/electrode_geometry.json");

/// One lead in its local model frame: `+z` proximal, origin at the most-distal **level**.
#[derive(Clone, Debug, Deserialize)]
pub struct ElectrodeGeometry {
    /// Catalogue display name, the lookup key (e.g. `"Medtronic 3387"`).
    pub elmodel: String,
    /// Mesh/file base name (e.g. `"medtronic_3387"`).
    pub matfname: String,
    pub lead_diameter_mm: f64,
    /// Individual contact elements, counting each segment of a directional level separately.
    pub n_contacts: usize,
    /// Axial height of one contact. CORE matches the sampled profile against a comb of these.
    pub contact_length_mm: f64,
    /// Tip section beyond the most-distal contact; part of the artifact, not of a contact.
    pub tip_length_mm: f64,
    /// When true the tip itself is the most-distal contact, so it is metal, not insulator.
    pub tip_is_contact: bool,
    /// Nominal centre-to-centre level pitch. Non-uniform leads also have `level_z_mm`.
    pub eldist_mm: f64,
    /// Depth of each level along the shaft; `level_z_mm[0]` is always 0.
    pub level_z_mm: Vec<f64>,
    /// 0-based level index per contact, so the three segments of a level share one entry.
    pub contact_level: Vec<usize>,
    pub contact_xyz_mm: Vec<[f64; 3]>,
    pub is_directed: bool,
    /// Tip-apex to orientation-marker-centre distance (`specs/09` §2.2); directional leads only.
    pub marker_pos_mm: Option<f64>,
    pub marker_len_mm: Option<f64>,
    #[serde(rename = "array_span_mm")]
    array_span: f64,
    #[serde(rename = "tail_level")]
    tail_level_index: usize,
    #[serde(rename = "segment_arc_deg")]
    segment_arc: Option<f64>,
}

pub const TRAJECTORY_POINTS: usize = 50;
pub const TRAJECTORY_PAST_HEAD_MM: f64 = 5.0;

impl ElectrodeGeometry {
    pub fn n_levels(&self) -> usize {
        self.level_z_mm.len()
    }

    pub fn array_span_mm(&self) -> f64 {
        self.array_span
    }

    pub fn tail_level(&self) -> usize {
        self.tail_level_index
    }

    pub fn fiducial_span_mm(&self) -> f64 {
        match (
            self.level_z_mm.first(),
            self.level_z_mm.get(self.tail_level()),
        ) {
            (Some(a), Some(b)) => b - a,
            _ => 0.0,
        }
    }

    pub fn last_level_span_mm(&self) -> f64 {
        match (self.level_z_mm.first(), self.level_z_mm.last()) {
            (Some(a), Some(b)) => b - a,
            _ => 0.0,
        }
    }

    pub fn segment_arc_deg(&self) -> Option<f64> {
        self.segment_arc
    }

    pub fn trajectory_extension_factor(&self) -> f64 {
        if self.n_levels() > 8 {
            1.5
        } else {
            2.0
        }
    }

    pub fn trajectory_extension_mm(&self) -> f64 {
        self.trajectory_extension_factor() * self.array_span.max(0.0)
    }

    pub fn trajectory(&self, head: [f64; 3], tail: [f64; 3]) -> Option<Vec<[f64; 3]>> {
        let d = [tail[0] - head[0], tail[1] - head[1], tail[2] - head[2]];
        let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if !(n.is_finite() && n > 1e-6) {
            return None;
        }
        let u = [d[0] / n, d[1] / n, d[2] / n];
        let ext = self.trajectory_extension_mm();
        let a1 = [
            head[0] - TRAJECTORY_PAST_HEAD_MM * u[0],
            head[1] - TRAJECTORY_PAST_HEAD_MM * u[1],
            head[2] - TRAJECTORY_PAST_HEAD_MM * u[2],
        ];
        let a2 = [
            tail[0] + ext * u[0],
            tail[1] + ext * u[1],
            tail[2] + ext * u[2],
        ];
        let last = (TRAJECTORY_POINTS - 1) as f64;
        Some(
            (0..TRAJECTORY_POINTS)
                .map(|i| {
                    let s = i as f64 / last;
                    [
                        a1[0] + (a2[0] - a1[0]) * s,
                        a1[1] + (a2[1] - a1[1]) * s,
                        a1[2] + (a2[2] - a1[2]) * s,
                    ]
                })
                .collect(),
        )
    }

    /// Arc from the tip apex to the most-distal **level** centre.
    ///
    /// `specs/09` §3.1 puts `z = 0` at the apex. When the tip is insulating the first contact
    /// starts after it, so the centre is `tip_length + contact_length/2`; when the tip **is**
    /// the contact (`tipiscontact`) the metal reaches the apex and the centre is `tip_length/2`.
    /// PaCER calibrates `t0` against this distance, so getting it wrong shifts every contact
    /// along the shaft by the difference — 1.5 mm for a 1.5 mm tip.
    pub fn apex_to_first_level_mm(&self) -> f64 {
        if self.tip_is_contact {
            0.5 * self.tip_length_mm
        } else {
            self.tip_length_mm + 0.5 * self.contact_length_mm
        }
    }

    pub fn marker_offset_mm(&self) -> Option<f64> {
        self.marker_pos_mm
            .map(|mp| mp - self.apex_to_first_level_mm())
    }

    /// Arc from the tip apex to each level centre, the depths PaCER samples contacts at.
    pub fn apex_to_level_mm(&self) -> Vec<f64> {
        let base = self.apex_to_first_level_mm();
        self.level_z_mm.iter().map(|z| base + z).collect()
    }

    /// Model-frame `head` / `tail` / `x` / `y` fiducials (`specs/09` §2.2), the fit source
    /// points: head at the most distal level, tail at the tail fiducial level.
    pub fn model_fiducials(&self) -> [[f64; 3]; 4] {
        let z0 = self.level_z_mm.first().copied().unwrap_or(0.0);
        let z1 = self
            .level_z_mm
            .get(self.tail_level())
            .copied()
            .unwrap_or(z0);
        let r = 0.5 * self.lead_diameter_mm;
        [[0.0, 0.0, z0], [0.0, 0.0, z1], [r, 0.0, z0], [0.0, r, z0]]
    }
}

#[derive(Deserialize)]
struct Payload {
    models: Vec<ElectrodeGeometry>,
}

fn parse_catalog(json: &str) -> Result<Vec<ElectrodeGeometry>, String> {
    let models = serde_json::from_str::<Payload>(json)
        .map_err(|e| format!("electrode geometry JSON does not parse: {e}"))?
        .models;
    for e in &models {
        if e.contact_level.len() != e.n_contacts || e.contact_xyz_mm.len() != e.n_contacts {
            return Err(format!(
                "{}: n_contacts {} but {} contact levels and {} contact positions",
                e.elmodel,
                e.n_contacts,
                e.contact_level.len(),
                e.contact_xyz_mm.len()
            ));
        }
        if e.tail_level_index >= e.level_z_mm.len() {
            return Err(format!(
                "{}: tail level {} but {} levels",
                e.elmodel,
                e.tail_level_index,
                e.level_z_mm.len()
            ));
        }
        if e.segment_arc
            .is_some_and(|a| !(a.is_finite() && a > 0.0 && a < 120.0))
        {
            return Err(format!("{}: segment arc out of range", e.elmodel));
        }
    }
    Ok(models)
}

fn catalog_result() -> &'static Result<Vec<ElectrodeGeometry>, String> {
    static CATALOG: OnceLock<Result<Vec<ElectrodeGeometry>, String>> = OnceLock::new();
    CATALOG.get_or_init(|| parse_catalog(GEOMETRY_JSON))
}

fn catalog() -> &'static [ElectrodeGeometry] {
    match catalog_result() {
        Ok(models) => models,
        Err(_) => &[],
    }
}

/// All catalogued display names, in catalogue order.
pub fn model_names() -> Vec<&'static str> {
    catalog().iter().map(|e| e.elmodel.as_str()).collect()
}

/// Look up by display name or `matfname`, case- and whitespace-insensitive.
pub fn by_name(name: &str) -> Option<&'static ElectrodeGeometry> {
    let want = name.trim().to_ascii_lowercase();
    if want.is_empty() {
        return None;
    }
    catalog()
        .iter()
        .find(|e| e.elmodel.to_ascii_lowercase() == want || e.matfname.to_ascii_lowercase() == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalogue_passes_validation() {
        assert_eq!(catalog_result().as_ref().err(), None);
    }

    #[test]
    fn inconsistent_contact_counts_are_an_error_not_a_panic() {
        let row = |levels: &str, xyz: &str| {
            format!(
                r#"{{"models": [{{"elmodel": "Bad", "matfname": "bad", "lead_diameter_mm": 1.3,
                "n_contacts": 2, "contact_length_mm": 1.5, "tip_length_mm": 1.5,
                "tip_is_contact": false, "eldist_mm": 2.0, "level_z_mm": [0.0, 2.0],
                "contact_level": {levels}, "contact_xyz_mm": {xyz}, "is_directed": false,
                "marker_pos_mm": null, "marker_len_mm": null, "array_span_mm": 3.5,
                "tail_level": 1, "segment_arc_deg": null}}]}}"#
            )
        };
        assert!(parse_catalog(&row("[0, 1]", "[[0,0,0],[0,0,2]]")).is_ok());
        let short_levels = parse_catalog(&row("[0]", "[[0,0,0],[0,0,2]]"));
        assert!(
            short_levels.as_ref().is_err_and(|e| e.contains("Bad")),
            "{short_levels:?}"
        );
        assert!(parse_catalog(&row("[0, 1]", "[[0,0,0]]")).is_err());
        assert!(parse_catalog("not json").is_err());
    }

    #[test]
    fn catalog_is_populated_and_self_consistent() {
        let all = catalog();
        assert!(all.len() >= 70, "only {} models", all.len());
        for e in all {
            assert_eq!(e.contact_xyz_mm.len(), e.n_contacts, "{}", e.elmodel);
            assert_eq!(e.contact_level.len(), e.n_contacts, "{}", e.elmodel);
            assert!(!e.level_z_mm.is_empty(), "{}", e.elmodel);
            assert_eq!(e.level_z_mm[0], 0.0, "{}", e.elmodel);
            let n_lev = e.n_levels();
            for (i, &lv) in e.contact_level.iter().enumerate() {
                assert!(
                    lv < n_lev,
                    "{}: contact {i} level {lv} >= {n_lev}",
                    e.elmodel
                );
                let z = e.contact_xyz_mm[i][2];
                assert!(
                    (z - e.level_z_mm[lv]).abs() < 1e-6,
                    "{}: contact {i} z {z} != level {lv} z {}",
                    e.elmodel,
                    e.level_z_mm[lv]
                );
            }
        }
    }

    /// The pitch used to live as a literal 1.5 mm, which is neither Medtronic lead.
    #[test]
    fn medtronic_pitches_come_from_the_catalogue() {
        let e87 = by_name("Medtronic 3387").expect("3387 catalogued");
        let e89 = by_name("Medtronic 3389").expect("3389 catalogued");
        assert!((e87.eldist_mm - 3.0).abs() < 1e-6);
        assert!((e89.eldist_mm - 2.0).abs() < 1e-6);
        assert!((e87.fiducial_span_mm() - 9.0).abs() < 1e-6);
        assert!((e89.fiducial_span_mm() - 6.0).abs() < 1e-6);
        assert!((e87.array_span_mm() - 10.5).abs() < 1e-6);
        assert!((e89.array_span_mm() - 7.5).abs() < 1e-6);
        assert_eq!(e87.n_levels(), 4);
        assert!(!e87.is_directed);
    }

    /// 9 of the 10 Lead-Tutor subjects carry 8-contact directional leads. Four levels of
    /// (ring, 3 segments, 3 segments, ring) span 6 mm, not the 14 mm that counting contacts
    /// as levels would give.
    #[test]
    fn directional_lead_spans_levels_not_contacts() {
        let e = by_name("Boston Scientific Vercise Directed").expect("catalogued");
        assert_eq!(e.n_contacts, 8);
        assert_eq!(e.n_levels(), 4);
        assert!(e.is_directed);
        assert!((e.fiducial_span_mm() - 6.0).abs() < 1e-6);
        assert!(
            (e.fiducial_span_mm() - (e.n_contacts - 1) as f64 * e.eldist_mm).abs() > 7.0,
            "contact-count span must differ sharply from the level span"
        );
        assert_eq!(e.contact_level, vec![0, 1, 1, 1, 2, 2, 2, 3]);
        let r = 0.5 * e.lead_diameter_mm;
        for (i, p) in e.contact_xyz_mm.iter().enumerate() {
            let radial = (p[0] * p[0] + p[1] * p[1]).sqrt();
            let seg = e.contact_level[i] == 1 || e.contact_level[i] == 2;
            if seg {
                assert!(
                    (radial - r).abs() < 1e-6,
                    "segment {i} radial {radial} != {r}"
                );
            } else {
                assert!(radial < 1e-9, "ring {i} radial {radial} != 0");
            }
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn tail_fiducial_sits_at_the_fourth_level() {
        let hx = by_name("boston_vercise_cartesia_hx").expect("catalogued");
        assert_eq!(hx.n_levels(), 8);
        assert_eq!(hx.tail_level(), 3);
        assert!(close(hx.fiducial_span_mm(), 6.0));
        assert!(close(hx.last_level_span_mm(), 14.0));
        assert!(close(hx.array_span_mm(), 15.5));
        let f = hx.model_fiducials();
        assert!(close(f[0][2], 0.0) && close(f[1][2], 6.0));
        assert!(close(f[2][2], 0.0) && close(f[3][2], 0.0));

        let v8 = by_name("boston_vercise").expect("catalogued");
        assert_eq!(v8.n_levels(), 8);
        assert_eq!(v8.tail_level(), 3);
        assert!(close(v8.fiducial_span_mm(), 6.0));
        assert!(close(v8.model_fiducials()[1][2], 6.0));

        let x = by_name("boston_vercise_cartesia_x").expect("catalogued");
        assert!(close(x.fiducial_span_mm(), 6.0));
        assert!(close(x.array_span_mm(), 11.5));

        let numed = by_name("numed_minilead").expect("catalogued");
        assert!(close(numed.fiducial_span_mm(), 3.0));
        assert!(close(numed.array_span_mm(), 3.5));
    }

    #[test]
    fn four_level_leads_keep_the_tail_at_the_last_level() {
        for name in [
            "medtronic_3389",
            "medtronic_3387",
            "medtronic_b33005",
            "boston_vercise_directed",
            "abbott_directed_05",
            "aleva_directstim_directed",
        ] {
            let e = by_name(name).expect("catalogued");
            assert_eq!(e.n_levels(), 4, "{name}");
            assert_eq!(e.tail_level(), 3, "{name}");
            assert!(
                close(e.fiducial_span_mm(), e.last_level_span_mm()),
                "{name}"
            );
            assert!(close(e.model_fiducials()[1][2], e.level_z_mm[3]), "{name}");
        }
        let smart = by_name("smartflow_ngs-nc-06").expect("catalogued");
        assert_eq!(smart.tail_level(), smart.n_levels() - 1);
        for name in ["sceneray_sr1202", "sceneray_sr1212"] {
            let e = by_name(name).expect("catalogued");
            assert_eq!(e.n_levels(), 8, "{name}");
            assert_eq!(e.tail_level(), 7, "{name}");
        }
    }

    #[test]
    fn abbott_directed_segments_run_clockwise() {
        let az = |p: [f64; 3]| p[1].atan2(p[0]).to_degrees().rem_euclid(360.0);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-4;
        for name in [
            "abbott_directed_05",
            "abbott_directed_15",
            "aleva_directstim_directed",
        ] {
            let e = by_name(name).expect("catalogued");
            let first = e
                .contact_level
                .iter()
                .position(|&lv| e.contact_level.iter().filter(|&&l| l == lv).count() == 3)
                .expect("segmented level");
            let a = az(e.contact_xyz_mm[first]);
            let b = az(e.contact_xyz_mm[first + 1]);
            let c = az(e.contact_xyz_mm[first + 2]);
            assert!(
                near(a, 90.0) && near(b, 330.0) && near(c, 210.0),
                "{name}: {a} {b} {c}"
            );
        }
        let bsci = by_name("boston_vercise_directed").expect("catalogued");
        assert!(near(az(bsci.contact_xyz_mm[2]), 210.0));
        assert!(near(az(bsci.contact_xyz_mm[3]), 330.0));
    }

    #[test]
    fn marker_offset_is_measured_from_the_head() {
        for (name, s_m) in [
            ("Boston Scientific Vercise Directed", 10.25),
            ("Boston Scientific Vercise Cartesia HX", 18.15),
            ("Boston Scientific Vercise Cartesia X", 14.15),
            ("Abbott Directed 6172 (short)", 9.0),
            ("Abbott Directed 6173 (long)", 12.0),
            ("Medtronic B33005", 12.75),
            ("Medtronic B33015", 15.75),
        ] {
            let got = by_name(name).expect("catalogued").marker_offset_mm();
            assert!(
                got.is_some_and(|g| (g - s_m).abs() < 1e-9),
                "{name}: {got:?}"
            );
        }
        assert_eq!(by_name("Medtronic 3389").unwrap().marker_offset_mm(), None);
    }

    #[test]
    fn segment_arc_comes_from_the_catalogue() {
        for (name, arc) in [
            ("boston_vercise_directed", Some(90.0)),
            ("medtronic_b33005", Some(90.0)),
            ("medtronic_b33015", Some(90.0)),
            ("abbott_directed_05", Some(90.0)),
            ("boston_vercise_cartesia_hx", Some(100.0)),
            ("boston_vercise_cartesia_x", Some(100.0)),
            ("aleva_directstim_directed", Some(85.0)),
            ("medtronic_3389", None),
        ] {
            assert_eq!(
                by_name(name).expect("catalogued").segment_arc_deg(),
                arc,
                "{name}"
            );
        }
        let aleva = by_name("aleva_directstim_directed").expect("catalogued");
        assert_eq!(aleva.marker_len_mm, None);
    }

    #[test]
    fn trajectory_extends_k_array_spans_past_the_tail() {
        let hx = by_name("boston_vercise_cartesia_hx").expect("catalogued");
        assert!(close(hx.trajectory_extension_factor(), 2.0));
        let traj = hx
            .trajectory([0.0; 3], [0.0, 0.0, 6.0])
            .expect("trajectory");
        assert_eq!(traj.len(), TRAJECTORY_POINTS);
        assert!(close(traj[0][2], -5.0));
        assert!(close(traj[49][2], 6.0 + 2.0 * 15.5));

        let depth = by_name("2069-EPC-15C-35").expect("catalogued");
        assert!(depth.n_levels() > 8);
        assert!(close(depth.trajectory_extension_factor(), 1.5));
        assert!(close(depth.array_span_mm(), 51.0));
        let span = depth.fiducial_span_mm();
        let traj = depth
            .trajectory([0.0; 3], [0.0, 0.0, span])
            .expect("trajectory");
        assert!(close(traj[49][2], span + 1.5 * 51.0));

        assert!(hx.trajectory([1.0; 3], [1.0; 3]).is_none());
    }

    #[test]
    fn lookup_accepts_matfname_and_is_case_insensitive() {
        assert_eq!(by_name("medtronic_3387").unwrap().elmodel, "Medtronic 3387");
        assert_eq!(
            by_name("  MEDTRONIC 3387 ").unwrap().elmodel,
            "Medtronic 3387"
        );
        assert!(by_name("Not A Real Lead").is_none());
        assert!(by_name("").is_none());
    }
}
