// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Resolved electrode geometry for all catalogued leads (`specs/09` §2, §3.1–§3.2).
//!
//! The reconstruct stage used to hardcode a 1.5 mm pitch, 4-contact lead that matches no real
//! device, so every reconstruction and every plausibility tolerance was computed against
//! fictional geometry. This module replaces that with the catalogue.
//!
//! The derivation (spacing overrides, `etageidx` archetypes, segment azimuths) lives in
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
    /// Distal-contact-centre to orientation-marker-centre distance; directional leads only.
    pub marker_pos_mm: Option<f64>,
    pub marker_len_mm: Option<f64>,
}

impl ElectrodeGeometry {
    pub fn n_levels(&self) -> usize {
        self.level_z_mm.len()
    }

    /// Expected `‖tail − head‖`: distal to proximal **level** centre.
    ///
    /// Not `(n_contacts − 1) · eldist`. On an 8-contact directional lead that formula gives
    /// 14 mm for a lead whose levels span 6 mm.
    pub fn contact_span_mm(&self) -> f64 {
        match (self.level_z_mm.first(), self.level_z_mm.last()) {
            (Some(a), Some(b)) => b - a,
            _ => 0.0,
        }
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

    /// Arc from the tip apex to each level centre, the depths PaCER samples contacts at.
    pub fn apex_to_level_mm(&self) -> Vec<f64> {
        let base = self.apex_to_first_level_mm();
        self.level_z_mm.iter().map(|z| base + z).collect()
    }

    /// Model-frame `head` / `tail` / `x` / `y` fiducials, the rigid-fit source points.
    pub fn model_fiducials(&self) -> [[f64; 3]; 4] {
        let z0 = self.level_z_mm.first().copied().unwrap_or(0.0);
        let z1 = self.level_z_mm.last().copied().unwrap_or(0.0);
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
                "marker_pos_mm": null, "marker_len_mm": null}}]}}"#
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
        assert!((e87.contact_span_mm() - 9.0).abs() < 1e-6);
        assert!((e89.contact_span_mm() - 6.0).abs() < 1e-6);
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
        assert!((e.contact_span_mm() - 6.0).abs() < 1e-6);
        assert!(
            (e.contact_span_mm() - (e.n_contacts - 1) as f64 * e.eldist_mm).abs() > 7.0,
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
