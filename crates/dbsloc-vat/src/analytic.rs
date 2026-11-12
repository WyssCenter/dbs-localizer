// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Analytical sphere models (`specs/07` §6).

use dbsloc_reconstruct::ElectrodeGeometry;
use rayon::prelude::*;

use crate::geometry::{contact_centre_world, LeadFrame};
use crate::{FieldResult, Grid, Mode, Polarity, SourceSetting, VatError, Volumes};

const KUNCEL_U0_V: f64 = 0.1;
const KUNCEL_K_V_PER_MM2: f64 = 0.22;
const MAEDLER_K1: f64 = -1.0473;
const MAEDLER_K3: f64 = 0.2786;
const MAEDLER_K4: f64 = 0.0009856;
const DEMBEK_FACTOR: f64 = 0.72;
const VOXELS_PER_MM: f64 = 5.0;

/// Analytical sphere model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalyticKind {
    /// Kuncel 2008, voltage mode.
    Kuncel,
    /// Mädler 2012 equation 3, voltage mode with impedance.
    Maedler,
    /// Dembek 2017, current-density threshold.
    Dembek,
}

/// Input of the analytical models.
#[derive(Clone, Debug)]
pub struct AnalyticInput {
    /// Lead frame in the computation space.
    pub lead: LeadFrame,
    /// Catalogue model.
    pub model: ElectrodeGeometry,
    /// Model used.
    pub kind: AnalyticKind,
    /// Activation threshold in V/mm (Dembek only).
    pub threshold_v_per_mm: f64,
    /// Dembek reference pulse width in microseconds.
    pub dembek_reference_pulse_width_us: f64,
    /// One entry per source; each active source must use exactly one contact.
    pub sources: Vec<SourceSetting>,
}

/// Kuncel 2008 radius in mm for a voltage in V; zero at or below 0.1 V.
pub fn kuncel_radius_mm(u_volts: f64) -> f64 {
    if u_volts <= KUNCEL_U0_V {
        0.0
    } else {
        ((u_volts - KUNCEL_U0_V) / KUNCEL_K_V_PER_MM2).sqrt()
    }
}

/// Mädler 2012 radius in mm for a voltage in V and an impedance in ohm.
pub fn maedler_radius_mm(u_volts: f64, z_ohm: f64) -> f64 {
    let k4z = MAEDLER_K4 * z_ohm;
    let disc =
        k4z * k4z + 2.0 * MAEDLER_K1 * k4z + MAEDLER_K1 * MAEDLER_K1 + 4.0 * MAEDLER_K3 * u_volts;
    let r = -(k4z - disc.max(0.0).sqrt() + MAEDLER_K1) / (2.0 * MAEDLER_K3);
    r.max(0.0)
}

/// Dembek 2017 radius in mm for a current in A, a threshold in V/m and the pulse width ratio.
pub fn dembek_radius_mm(current_a: f64, threshold_v_per_m: f64, pulse_width_ratio: f64) -> f64 {
    pulse_width_ratio.powf(0.3) * (DEMBEK_FACTOR * current_a / threshold_v_per_m).sqrt() * 1000.0
}

fn used_contacts(src: &SourceSetting) -> Vec<(usize, f64)> {
    src.contacts
        .iter()
        .filter(|c| c.percent > 0.0 && c.polarity != Polarity::Off)
        .map(|c| (c.index, c.impedance_kohm))
        .collect()
}

fn identity4() -> [[f64; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn empty_result(n_sources: usize, radii: Vec<f64>, warnings: Vec<String>) -> FieldResult {
    FieldResult {
        e_mag: Grid::empty(),
        binary: Grid::empty(),
        binary_raw: Grid::empty(),
        affine: identity4(),
        volumes: Volumes {
            raw_mm3: 0.0,
            final_mm3: 0.0,
            per_source_raw_mm3: vec![0.0; n_sources],
            sphere_mm3: Some(0.0),
        },
        contact_currents_ma: vec![Vec::new(); n_sources],
        contact_potentials_v: vec![Vec::new(); n_sources],
        source_radii_mm: radii,
        touches_boundary: false,
        warnings,
    }
}

/// Union of per-source spheres voxelised on the `specs/07` §6.4 grid.
pub fn analytic(input: &AnalyticInput) -> Result<FieldResult, VatError> {
    let mut warnings = Vec::new();
    let n_src = input.sources.len();
    let mut spheres: Vec<Option<([f64; 3], f64)>> = vec![None; n_src];
    let mut radii = vec![0.0; n_src];
    for (s, src) in input.sources.iter().enumerate() {
        let used = used_contacts(src);
        if !(src.amplitude > 0.0 && !used.is_empty()) {
            continue;
        }
        if used.len() != 1 {
            return Err(VatError::Montage(format!(
                "source {}: analytical models need exactly one used contact, found {}",
                s + 1,
                used.len()
            )));
        }
        if input.kind != AnalyticKind::Dembek && src.mode != Mode::Voltage {
            return Err(VatError::ModeNotSupported(format!(
                "source {}: voltage mode required",
                s + 1
            )));
        }
        let (index, imp_kohm) = used[0];
        if index == 0 || index > input.model.n_contacts {
            return Err(VatError::InvalidInput(format!(
                "source {}: contact {index} does not exist on {}",
                s + 1,
                input.model.elmodel
            )));
        }
        let z_ohm = 1000.0 * if imp_kohm > 0.0 { imp_kohm } else { 1.0 };
        let r = match input.kind {
            AnalyticKind::Kuncel => {
                if src.amplitude <= KUNCEL_U0_V {
                    warnings.push(format!(
                        "source {}: Kuncel voltage {} V is not above 0.1 V; empty VTA",
                        s + 1,
                        src.amplitude
                    ));
                }
                kuncel_radius_mm(src.amplitude)
            }
            AnalyticKind::Maedler => maedler_radius_mm(src.amplitude, z_ohm),
            AnalyticKind::Dembek => {
                let current_a = match src.mode {
                    Mode::Voltage => src.amplitude / z_ohm,
                    Mode::Current => src.amplitude / 1000.0,
                };
                dembek_radius_mm(
                    current_a,
                    1000.0 * input.threshold_v_per_mm,
                    src.pulse_width_us / input.dembek_reference_pulse_width_us,
                )
            }
        };
        radii[s] = r;
        if r > 0.0 {
            let c =
                contact_centre_world(&input.lead, &input.model, index - 1).ok_or_else(|| {
                    VatError::InvalidInput(format!("contact {index} has no catalogue position"))
                })?;
            spheres[s] = Some((c, r));
        }
    }
    let all: Vec<([f64; 3], f64)> = spheres.iter().flatten().copied().collect();
    if all.is_empty() {
        return Ok(empty_result(n_src, radii, warnings));
    }
    let sphere_mm3: f64 = all
        .iter()
        .map(|(_, r)| 4.0 / 3.0 * std::f64::consts::PI * r.powi(3))
        .sum();
    let mut lo = [0.0; 3];
    let mut n = [0usize; 3];
    let mut step = [0.0; 3];
    for a in 0..3 {
        let mn = all
            .iter()
            .map(|(c, r)| c[a] - r)
            .fold(f64::INFINITY, f64::min);
        let mx = all
            .iter()
            .map(|(c, r)| c[a] + r)
            .fold(f64::NEG_INFINITY, f64::max);
        let lower = mn.floor();
        let upper = mx.ceil();
        lo[a] = lower;
        n[a] = (VOXELS_PER_MM * (upper - lower)).round() as usize;
        step[a] = (upper - lower) / (n[a] as f64 - 1.0);
    }
    let rasterise = |sel: &[([f64; 3], f64)]| -> Vec<u8> {
        let mut data = vec![0u8; n[0] * n[1] * n[2]];
        data.par_chunks_mut(n[0] * n[1])
            .enumerate()
            .for_each(|(k, plane)| {
                let z = lo[2] + step[2] * k as f64;
                for j in 0..n[1] {
                    let y = lo[1] + step[1] * j as f64;
                    for i in 0..n[0] {
                        let x = lo[0] + step[0] * i as f64;
                        let inside = sel.iter().any(|(c, r)| {
                            let d = [x - c[0], y - c[1], z - c[2]];
                            d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= r * r
                        });
                        plane[i + n[0] * j] = u8::from(inside);
                    }
                }
            });
        data
    };
    let vox = step[0] * step[1] * step[2];
    let data = rasterise(&all);
    let raw = data.iter().filter(|&&v| v != 0).count() as f64 * vox;
    let per_source: Vec<f64> = spheres
        .iter()
        .map(|sp| match sp {
            Some(one) => {
                rasterise(std::slice::from_ref(one))
                    .iter()
                    .filter(|&&v| v != 0)
                    .count() as f64
                    * vox
            }
            None => 0.0,
        })
        .collect();
    let affine = [
        [step[0], 0.0, 0.0, lo[0]],
        [0.0, step[1], 0.0, lo[1]],
        [0.0, 0.0, step[2], lo[2]],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let grid = Grid { dims: n, data };
    Ok(FieldResult {
        e_mag: Grid::empty(),
        binary: grid.clone(),
        binary_raw: grid,
        affine,
        volumes: Volumes {
            raw_mm3: raw,
            final_mm3: raw,
            per_source_raw_mm3: per_source,
            sphere_mm3: Some(sphere_mm3),
        },
        contact_currents_ma: vec![Vec::new(); n_src],
        contact_potentials_v: vec![Vec::new(); n_src],
        source_radii_mm: radii,
        touches_boundary: false,
        warnings,
    })
}
