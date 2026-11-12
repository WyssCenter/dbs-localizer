// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! CPU reference E-field engine and analytical VTA models (`specs/07` §3, §6).
//!
//! Pure computation: no file I/O, no NIfTI, no pipeline wiring. A GPU backend implements
//! [`BasisSolver`] and returns the same [`Basis`].

pub mod analytic;
pub mod conductivity;
pub mod field;
pub mod geometry;
pub mod solver;

#[cfg(test)]
mod tests;

use std::time::Instant;

use dbsloc_reconstruct::ElectrodeGeometry;
use thiserror::Error;

pub use analytic::{
    analytic, dembek_radius_mm, kuncel_radius_mm, maedler_radius_mm, AnalyticInput, AnalyticKind,
};
pub use conductivity::{ConductivityModel, Encapsulation, Volume};
pub use geometry::{BodyKind, GridSpec, LeadFrame};
pub use solver::{Control, Progress, SolveStats, SolverSettings};

use geometry::{Body, Materials, SolverGrid};

/// Errors of the engine; the variants map to the `E_VAT_*` codes of `specs/07` §7.4.
#[derive(Debug, Error)]
pub enum VatError {
    /// The cancel flag was set.
    #[error("cancelled by user")]
    Cancelled,
    /// CG did not reach the tolerance within the iteration cap (`E_VAT_solver`).
    #[error("solver did not converge after {iterations} iterations (relative residual {rel_residual:e})")]
    NotConverged {
        /// Iterations performed.
        iterations: usize,
        /// Last relative residual.
        rel_residual: f64,
    },
    /// No grid cell belongs to a contact that must be solved; the grid is too coarse.
    #[error("contact {contact} is not resolved by the solver grid")]
    ContactNotResolved {
        /// 0-based contact index.
        contact: usize,
    },
    /// The solver grid exceeds the cell cap (`E_VAT_capacity`).
    #[error("solver grid of {cells} cells exceeds the cap")]
    GridTooLarge {
        /// Requested cell count.
        cells: usize,
    },
    /// A needed contact basis was not solved.
    #[error("contact {contact} basis was not solved")]
    UnsolvedContact {
        /// 0-based contact index.
        contact: usize,
    },
    /// Invalid input (`E_VAT_settings`).
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Analytical model with other than one used contact (`E_VAT_montage`).
    #[error("{0}")]
    Montage(String),
    /// Analytical model in an unsupported mode (`E_VAT_mode`).
    #[error("{0}")]
    ModeNotSupported(String),
    /// A compute backend failed (device, driver or memory error).
    #[error("compute backend error: {0}")]
    Backend(String),
    /// The impedance system is singular.
    #[error("transfer-impedance system is singular")]
    Singular,
}

/// Dense 3D array, x fastest.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid<T> {
    /// Voxel counts.
    pub dims: [usize; 3],
    /// Voxel values.
    pub data: Vec<T>,
}

impl<T> Grid<T> {
    /// Grid without voxels.
    pub fn empty() -> Self {
        Self {
            dims: [0; 3],
            data: Vec::new(),
        }
    }

    /// Linear index of a voxel.
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        i + self.dims[0] * (j + self.dims[1] * k)
    }
}

/// Contact or case polarity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    /// Not used.
    Off,
    /// Negative.
    Cathode,
    /// Positive.
    Anode,
}

/// Source control mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Constant voltage, amplitude in V.
    Voltage,
    /// Constant current, amplitude in mA.
    Current,
}

/// Multi-source combination rule (`specs/07` §3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceCombination {
    /// Per-point maximum of the source magnitudes.
    Max,
    /// Magnitude of the vector sum.
    Sum,
}

/// One contact of a source (`index` is 1-based).
#[derive(Clone, Copy, Debug)]
pub struct ContactSetting {
    /// 1-based catalogue contact index.
    pub index: usize,
    /// Polarity.
    pub polarity: Polarity,
    /// Percent of the polarity group, 0 to 100.
    pub percent: f64,
    /// Contact impedance in kOhm (analytical models).
    pub impedance_kohm: f64,
}

/// Case (pulse generator housing) setting.
#[derive(Clone, Copy, Debug)]
pub struct CaseSetting {
    /// Polarity.
    pub polarity: Polarity,
    /// Percent.
    pub percent: f64,
}

/// One validated source of a side.
#[derive(Clone, Debug)]
pub struct SourceSetting {
    /// Voltage or current control.
    pub mode: Mode,
    /// Volts or milliamps.
    pub amplitude: f64,
    /// Pulse width in microseconds (Dembek).
    pub pulse_width_us: f64,
    /// Contacts of the source.
    pub contacts: Vec<ContactSetting>,
    /// Case setting.
    pub case: CaseSetting,
}

/// Resolved model parameters (`specs/07` §2.5).
#[derive(Clone, Debug)]
pub struct ResolvedModelSettings {
    /// Activation threshold in V/mm.
    pub threshold_v_per_mm: f64,
    /// Tissue conductivity model.
    pub conductivity: ConductivityModel,
    /// Encapsulation shell.
    pub encapsulation: Encapsulation,
    /// Collapse the lead volume out of the field (`specs/07` §3.8 step 2).
    pub remove_electrode: bool,
    /// Multi-source rule.
    pub source_combination: SourceCombination,
    /// Solver stopping rule.
    pub solver: SolverSettings,
}

impl Default for ResolvedModelSettings {
    fn default() -> Self {
        Self {
            threshold_v_per_mm: 0.2,
            conductivity: ConductivityModel::GreyWhite {
                gm_s_per_m: 0.33,
                wm_s_per_m: 0.14,
                gm_mask: None,
            },
            encapsulation: Encapsulation::default(),
            remove_electrode: true,
            source_combination: SourceCombination::Max,
            solver: SolverSettings::default(),
        }
    }
}

/// Everything the engine needs for one side.
#[derive(Clone, Debug)]
pub struct EngineInput {
    /// Lead frame in the computation space.
    pub lead: LeadFrame,
    /// Catalogue model.
    pub model: ElectrodeGeometry,
    /// Body placed in the volume conductor (`Catalogue` except in synthetic tests).
    pub body: BodyKind,
    /// Resolved model settings.
    pub settings: ResolvedModelSettings,
    /// Validated sources of the side.
    pub sources: Vec<SourceSetting>,
    /// Solver grid and output spacing.
    pub grid: GridSpec,
}

/// Per-contact unit-current solutions and the transfer-impedance matrix (`specs/07` §3.4).
#[derive(Clone, Debug)]
pub struct Basis {
    /// Lead frame the grid lives in.
    pub lead: LeadFrame,
    /// Solver grid in lead-local millimetres.
    pub grid: SolverGrid,
    /// Cell classes and conductivities.
    pub materials: Materials,
    /// Unknown index per cell (`u32::MAX` for insulation).
    pub node_of_cell: Vec<u32>,
    /// Unknown index per contact.
    pub contact_node: Vec<Option<u32>>,
    /// Number of contacts.
    pub n_contacts: usize,
    /// Potential of every unknown for 1 mA at contact `k` (V), `None` when not solved.
    pub solutions: Vec<Option<Vec<f64>>>,
    /// `z_kohm[j][k]`: potential of contact `j` for 1 mA at contact `k` (kOhm); NaN when unsolved.
    pub z_kohm: Vec<Vec<f64>>,
    /// Iteration statistics per solved contact.
    pub stats: Vec<SolveStats>,
    /// Model settings the basis was built with.
    pub settings: ResolvedModelSettings,
    /// Output voxel spacing in millimetres.
    pub output_spacing_mm: f64,
    /// Lead radius used by the electrode removal.
    pub lead_radius_mm: f64,
    /// Seconds spent in the solves.
    pub seconds: f64,
    /// Warnings raised while building the basis.
    pub warnings: Vec<String>,
}

/// Volumes in mm³ (`specs/07` §3.9).
#[derive(Clone, Debug, PartialEq)]
pub struct Volumes {
    /// Raw VTA volume.
    pub raw_mm3: f64,
    /// Volume after hole filling and opening.
    pub final_mm3: f64,
    /// Raw volume of each source on its own.
    pub per_source_raw_mm3: Vec<f64>,
    /// Sum of analytic sphere volumes (analytical models only).
    pub sphere_mm3: Option<f64>,
}

/// Result of one side.
#[derive(Clone, Debug)]
pub struct FieldResult {
    /// Clamped |E| in V/m on the output grid (empty for analytical models).
    pub e_mag: Grid<f32>,
    /// Final binary VTA.
    pub binary: Grid<u8>,
    /// Binary VTA before hole filling and opening.
    pub binary_raw: Grid<u8>,
    /// Voxel-to-world affine of the output grid.
    pub affine: [[f64; 4]; 4],
    /// Volumes.
    pub volumes: Volumes,
    /// Contact currents in mA per source (empty for analytical models).
    pub contact_currents_ma: Vec<Vec<f64>>,
    /// Contact potentials in V per source (empty for analytical models).
    pub contact_potentials_v: Vec<Vec<f64>>,
    /// Sphere radius per source (analytical models only).
    pub source_radii_mm: Vec<f64>,
    /// A supra-threshold cell touches the solver boundary (`specs/07` §10).
    pub touches_boundary: bool,
    /// Warnings.
    pub warnings: Vec<String>,
}

/// Backend that produces a [`Basis`]; the wgpu backend implements the same trait.
pub trait BasisSolver {
    /// Solves the unit-current problems of the listed 0-based contacts.
    fn solve_basis(
        &self,
        input: &EngineInput,
        contacts: &[usize],
        ctl: &Control<'_>,
    ) -> Result<Basis, VatError>;
}

pub(crate) fn prepare(
    input: &EngineInput,
) -> Result<(Body, SolverGrid, Materials, solver::System), VatError> {
    let body = Body::new(input.body, &input.model, input.lead.stretch)?;
    let grid = geometry::build_grid(&body, &input.grid)?;
    let materials = geometry::build_materials(
        &grid,
        &body,
        &input.lead,
        &input.settings.conductivity,
        &input.settings.encapsulation,
    );
    let system = solver::assemble(&grid, &materials, body.n_contacts())?;
    Ok((body, grid, materials, system))
}

/// Assembles the finite-volume system of `input` without solving it.
pub fn assemble_system(input: &EngineInput) -> Result<solver::System, VatError> {
    prepare(input).map(|(_, _, _, system)| system)
}

/// The f64 reference backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuSolver;

impl BasisSolver for CpuSolver {
    fn solve_basis(
        &self,
        input: &EngineInput,
        contacts: &[usize],
        ctl: &Control<'_>,
    ) -> Result<Basis, VatError> {
        build_basis(input, contacts, |_| {
            Ok(CpuUnitSolver {
                settings: input.settings.solver,
                ctl: *ctl,
            })
        })
    }
}

struct CpuUnitSolver<'a> {
    settings: SolverSettings,
    ctl: Control<'a>,
}

impl SystemSolver for CpuUnitSolver<'_> {
    fn solve_unit(
        &mut self,
        system: &solver::System,
        contact: usize,
    ) -> Result<(Vec<f64>, SolveStats), VatError> {
        system.solve_currents(&[(contact, 1.0)], &self.settings, &self.ctl)
    }
}

/// The CPU per-contact solver as a [`SystemSolver`], for callers that pick the backend once the
/// system is assembled.
pub fn cpu_unit_solver<'a>(settings: SolverSettings, ctl: Control<'a>) -> impl SystemSolver + 'a {
    CpuUnitSolver { settings, ctl }
}

impl<T: SystemSolver + ?Sized> SystemSolver for Box<T> {
    fn solve_unit(
        &mut self,
        system: &solver::System,
        contact: usize,
    ) -> Result<(Vec<f64>, SolveStats), VatError> {
        (**self).solve_unit(system, contact)
    }
}

/// Per-contact linear solver of one assembled system; a backend uploads the matrix when it is
/// created and reuses it for every contact.
pub trait SystemSolver {
    /// Solves for 1 mA at `contact`; returns the potential of every unknown in volts.
    fn solve_unit(
        &mut self,
        system: &solver::System,
        contact: usize,
    ) -> Result<(Vec<f64>, SolveStats), VatError>;
}

/// Builds a [`Basis`] with a caller-supplied linear solver: `begin` receives the assembled system
/// once (after the contacts are checked) and returns the solver used for every listed contact.
pub fn build_basis<S, B>(
    input: &EngineInput,
    contacts: &[usize],
    begin: B,
) -> Result<Basis, VatError>
where
    S: SystemSolver,
    B: FnOnce(&solver::System) -> Result<S, VatError>,
{
    let start = Instant::now();
    let (body, grid, materials, system) = prepare(input)?;
    let n_contacts = body.n_contacts();
    for &k in contacts {
        if k >= n_contacts {
            return Err(VatError::InvalidInput(format!(
                "contact {} does not exist ({n_contacts} contacts)",
                k + 1
            )));
        }
    }
    let mut warnings = Vec::new();
    if input.settings.conductivity.is_missing_mask() {
        warnings
            .push("no grey-matter mask; homogeneous white-matter conductivity used".to_string());
    }
    for &k in contacts {
        if system.contact_node[k].is_none() {
            return Err(VatError::ContactNotResolved { contact: k });
        }
    }
    let mut solver = begin(&system)?;
    let mut solutions: Vec<Option<Vec<f64>>> = vec![None; n_contacts];
    let mut z = vec![vec![f64::NAN; n_contacts]; n_contacts];
    let mut stats = Vec::new();
    for &k in contacts {
        let (x, st) = solver.solve_unit(&system, k)?;
        for (j, row) in z.iter_mut().enumerate() {
            if let Some(nd) = system.contact_node[j] {
                row[k] = x[nd as usize];
            }
        }
        stats.push(SolveStats { contact: k, ..st });
        solutions[k] = Some(x);
    }
    Ok(Basis {
        lead: input.lead,
        lead_radius_mm: body.lead_radius(),
        grid,
        materials,
        node_of_cell: system.node_of_cell,
        contact_node: system.contact_node,
        n_contacts,
        solutions,
        z_kohm: z,
        stats,
        settings: input.settings.clone(),
        output_spacing_mm: input.grid.output_spacing_mm,
        seconds: start.elapsed().as_secs_f64(),
        warnings,
    })
}

/// Solves the basis of every contact on the CPU.
pub fn solve_basis(input: &EngineInput, ctl: &Control<'_>) -> Result<Basis, VatError> {
    let body = Body::new(input.body, &input.model, input.lead.stretch)?;
    let all: Vec<usize> = (0..body.n_contacts()).collect();
    CpuSolver.solve_basis(input, &all, ctl)
}

/// Solves only the listed 0-based contacts on the CPU.
pub fn solve_basis_subset(
    input: &EngineInput,
    contacts: &[usize],
    ctl: &Control<'_>,
) -> Result<Basis, VatError> {
    CpuSolver.solve_basis(input, contacts, ctl)
}

/// Contacts used by any active source of the input, 0-based and sorted.
pub fn used_contacts_of(sources: &[SourceSetting]) -> Vec<usize> {
    let mut v: Vec<usize> = sources
        .iter()
        .filter(|s| s.amplitude > 0.0)
        .flat_map(|s| s.contacts.iter())
        .filter(|c| c.percent > 0.0 && c.polarity != Polarity::Off && c.index > 0)
        .map(|c| c.index - 1)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Solves the needed contacts and applies the sources of the input.
pub fn solve(input: &EngineInput, ctl: &Control<'_>) -> Result<FieldResult, VatError> {
    let need = used_contacts_of(&input.sources);
    let basis = solve_basis_subset(input, &need, ctl)?;
    apply(&basis, &input.sources)
}

fn solve_dense(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Result<Vec<f64>, VatError> {
    let n = b.len();
    for col in 0..n {
        let piv = (col..n)
            .max_by(|&p, &q| a[p][col].abs().total_cmp(&a[q][col].abs()))
            .ok_or(VatError::Singular)?;
        if a[piv][col].abs().partial_cmp(&1e-300) != Some(std::cmp::Ordering::Greater) {
            return Err(VatError::Singular);
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for r in col + 1..n {
            let f = a[r][col] / a[col][col];
            if f != 0.0 {
                let pivot_row: Vec<f64> = a[col].clone();
                for (dst, v) in a[r].iter_mut().zip(&pivot_row).skip(col) {
                    *dst -= f * v;
                }
                let bc = b[col];
                b[r] -= f * bc;
            }
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let mut s = b[r];
        for c in r + 1..n {
            s -= a[r][c] * x[c];
        }
        x[r] = s / a[r][r];
    }
    Ok(x)
}

struct Drive {
    currents_ma: Vec<f64>,
    potentials_v: Vec<f64>,
}

fn drive_source(basis: &Basis, src: &SourceSetting) -> Result<Option<Drive>, VatError> {
    let n = basis.n_contacts;
    let used: Vec<(usize, Polarity, f64)> = src
        .contacts
        .iter()
        .filter(|c| c.percent > 0.0 && c.polarity != Polarity::Off)
        .map(|c| {
            if c.index == 0 || c.index > n {
                Err(VatError::InvalidInput(format!(
                    "contact {} does not exist on the lead",
                    c.index
                )))
            } else {
                Ok((c.index - 1, c.polarity, c.percent))
            }
        })
        .collect::<Result<_, _>>()?;
    if src.amplitude.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) || used.is_empty() {
        return Ok(None);
    }
    for &(k, _, _) in &used {
        if basis.solutions[k].is_none() {
            return Err(VatError::UnsolvedContact { contact: k });
        }
    }
    let has_cath = used.iter().any(|u| u.1 == Polarity::Cathode);
    let has_an = used.iter().any(|u| u.1 == Polarity::Anode);
    let mut case_on = src.case.polarity != Polarity::Off && src.case.percent > 0.0;
    let mut case_pol = src.case.polarity;
    if !case_on && !(has_cath && has_an) {
        case_on = true;
        case_pol = if has_cath {
            Polarity::Anode
        } else {
            Polarity::Cathode
        };
    }
    let a = src.amplitude;
    let mut currents = vec![0.0; n];
    let mut offset = 0.0;
    match src.mode {
        Mode::Current => {
            for &(k, pol, perc) in &used {
                let sign = if pol == Polarity::Cathode { -1.0 } else { 1.0 };
                currents[k] += sign * a * perc / 100.0;
            }
        }
        Mode::Voltage => {
            let m = used.len();
            let volt = |pol: Polarity| -> f64 {
                if case_on {
                    if pol == case_pol {
                        0.0
                    } else if pol == Polarity::Cathode {
                        -a
                    } else {
                        a
                    }
                } else if pol == Polarity::Cathode {
                    -0.5 * a
                } else {
                    0.5 * a
                }
            };
            let dim = if case_on { m } else { m + 1 };
            let mut mat = vec![vec![0.0; dim]; dim];
            let mut rhs = vec![0.0; dim];
            for (r, &(kr, polr, _)) in used.iter().enumerate() {
                for (c, &(kc, _, _)) in used.iter().enumerate() {
                    mat[r][c] = basis.z_kohm[kr][kc];
                }
                if !case_on {
                    mat[r][m] = 1.0;
                    mat[m][r] = 1.0;
                }
                rhs[r] = volt(polr);
            }
            let sol = solve_dense(mat, rhs)?;
            for (r, &(k, _, _)) in used.iter().enumerate() {
                currents[k] += sol[r];
            }
            if !case_on {
                offset = sol[m];
            }
        }
    }
    let mut potentials = vec![f64::NAN; n];
    for (j, p) in potentials.iter_mut().enumerate() {
        if basis.contact_node[j].is_none() {
            continue;
        }
        let mut s = offset;
        let mut ok = true;
        for (k, &i) in currents.iter().enumerate() {
            if i != 0.0 {
                let z = basis.z_kohm[j][k];
                if z.is_nan() {
                    ok = false;
                }
                s += z * i;
            }
        }
        if ok {
            *p = s;
        }
    }
    Ok(Some(Drive {
        currents_ma: currents,
        potentials_v: potentials,
    }))
}

fn source_phi(basis: &Basis, currents: &[f64]) -> Vec<f64> {
    let n_nodes = basis
        .solutions
        .iter()
        .flatten()
        .next()
        .map_or(0, |s| s.len());
    let mut phi = vec![0.0; n_nodes];
    for (k, &i) in currents.iter().enumerate() {
        if i == 0.0 {
            continue;
        }
        if let Some(sol) = &basis.solutions[k] {
            for (p, s) in phi.iter_mut().zip(sol) {
                *p += i * s;
            }
        }
    }
    phi
}

fn empty_field(
    n_sources: usize,
    currents: Vec<Vec<f64>>,
    potentials: Vec<Vec<f64>>,
) -> FieldResult {
    FieldResult {
        e_mag: Grid::empty(),
        binary: Grid::empty(),
        binary_raw: Grid::empty(),
        affine: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
        volumes: Volumes {
            raw_mm3: 0.0,
            final_mm3: 0.0,
            per_source_raw_mm3: vec![0.0; n_sources],
            sphere_mm3: None,
        },
        contact_currents_ma: currents,
        contact_potentials_v: potentials,
        source_radii_mm: Vec::new(),
        touches_boundary: false,
        warnings: Vec::new(),
    }
}

/// Superposes the basis for the given sources and runs the `specs/07` §3.8 post-processing.
pub fn apply(basis: &Basis, sources: &[SourceSetting]) -> Result<FieldResult, VatError> {
    let n_src = sources.len();
    let mut currents_out = Vec::with_capacity(n_src);
    let mut potentials_out = Vec::with_capacity(n_src);
    let mut fields: Vec<Option<Vec<[f32; 3]>>> = Vec::with_capacity(n_src);
    for src in sources {
        match drive_source(basis, src)? {
            None => {
                currents_out.push(vec![0.0; basis.n_contacts]);
                potentials_out.push(vec![0.0; basis.n_contacts]);
                fields.push(None);
            }
            Some(d) => {
                let phi = source_phi(basis, &d.currents_ma);
                fields.push(Some(field::cell_efield(
                    &basis.grid,
                    &basis.materials,
                    &basis.node_of_cell,
                    &phi,
                )));
                currents_out.push(d.currents_ma);
                potentials_out.push(d.potentials_v);
            }
        }
    }
    let active: Vec<&Vec<[f32; 3]>> = fields.iter().flatten().collect();
    if active.is_empty() {
        return Ok(empty_field(n_src, currents_out, potentials_out));
    }
    let n_cells = basis.grid.n_cells();
    let combined: Vec<f32> = match basis.settings.source_combination {
        SourceCombination::Max => {
            let mut m = vec![0f32; n_cells];
            for f in &active {
                let mags = field::magnitudes(f);
                for (a, b) in m.iter_mut().zip(&mags) {
                    if *b > *a {
                        *a = *b;
                    }
                }
            }
            m
        }
        SourceCombination::Sum => {
            let mut v = vec![[0f32; 3]; n_cells];
            for f in &active {
                for (a, b) in v.iter_mut().zip(f.iter()) {
                    for d in 0..3 {
                        a[d] += b[d];
                    }
                }
            }
            field::magnitudes(&v)
        }
    };
    let thr = (1000.0 * basis.settings.threshold_v_per_mm) as f32;
    let mut warnings = basis.warnings.clone();
    let Some((bmin, bmax)) =
        field::supra_bbox_world(&basis.grid, &basis.materials, &combined, thr, &basis.lead)
    else {
        let mut r = empty_field(n_src, currents_out, potentials_out);
        r.warnings = warnings;
        return Ok(r);
    };
    let touches = field::touches_boundary(&basis.grid, &combined, thr);
    if touches {
        warnings.push("the VTA reaches the model boundary".to_string());
    }
    let out = field::output_grid_for_bbox(bmin, bmax, basis.output_spacing_mm, &mut warnings);
    let remove = basis.settings.remove_electrode;
    let sampled = {
        let s = field::Sampler::new(&basis.grid, &basis.materials, &combined);
        field::sample_to_output(&s, &basis.lead, &out, basis.lead_radius_mm, remove)
    };
    let e_clamped: Vec<f32> = sampled
        .iter()
        .map(|&v| v.min(field::EFIELD_CLAMP_V_PER_M))
        .collect();
    let raw_mask = field::threshold(&e_clamped, thr);
    let vox = out.spacing.powi(3);
    let raw_mm3 = field::count_set(&raw_mask) as f64 * vox;
    let binary_raw = Grid {
        dims: out.dims,
        data: raw_mask,
    };
    let mut filled = binary_raw.clone();
    field::fill_holes(&mut filled);
    let final_grid = field::open_ball(&filled, field::OPENING_RADIUS_VOXELS);
    let final_mm3 = field::count_set(&final_grid.data) as f64 * vox;
    let mut per_source = Vec::with_capacity(n_src);
    for f in &fields {
        match f {
            None => per_source.push(0.0),
            Some(v) => {
                let mags = field::magnitudes(v);
                let s = field::Sampler::new(&basis.grid, &basis.materials, &mags);
                let data =
                    field::sample_to_output(&s, &basis.lead, &out, basis.lead_radius_mm, remove);
                let count = data
                    .iter()
                    .filter(|&&x| x.min(field::EFIELD_CLAMP_V_PER_M) > thr)
                    .count();
                per_source.push(count as f64 * vox);
            }
        }
    }
    Ok(FieldResult {
        e_mag: Grid {
            dims: out.dims,
            data: e_clamped,
        },
        binary: final_grid,
        binary_raw,
        affine: out.affine(),
        volumes: Volumes {
            raw_mm3,
            final_mm3,
            per_source_raw_mm3: per_source,
            sphere_mm3: None,
        },
        contact_currents_ma: currents_out,
        contact_potentials_v: potentials_out,
        source_radii_mm: Vec::new(),
        touches_boundary: touches,
        warnings,
    })
}

/// Rebuilds a [`Basis`] from stored per-contact solutions (a basis cache): the grid, materials
/// and system layout are derived from `input` again, the solves are not repeated.
pub fn rebuild_basis(
    input: &EngineInput,
    solutions: Vec<Option<Vec<f64>>>,
    z_kohm: Vec<Vec<f64>>,
    stats: Vec<SolveStats>,
) -> Result<Basis, VatError> {
    let (body, grid, materials, system) = prepare(input)?;
    let n_contacts = body.n_contacts();
    if solutions.len() != n_contacts
        || z_kohm.len() != n_contacts
        || z_kohm.iter().any(|r| r.len() != n_contacts)
    {
        return Err(VatError::InvalidInput(
            "the stored basis has another contact count".into(),
        ));
    }
    if solutions.iter().flatten().any(|s| s.len() != system.n) {
        return Err(VatError::InvalidInput(
            "the stored basis does not match the solver grid".into(),
        ));
    }
    let mut warnings = Vec::new();
    if input.settings.conductivity.is_missing_mask() {
        warnings
            .push("no grey-matter mask; homogeneous white-matter conductivity used".to_string());
    }
    Ok(Basis {
        lead: input.lead,
        lead_radius_mm: body.lead_radius(),
        grid,
        materials,
        node_of_cell: system.node_of_cell,
        contact_node: system.contact_node,
        n_contacts,
        solutions,
        z_kohm,
        stats,
        settings: input.settings.clone(),
        output_spacing_mm: input.grid.output_spacing_mm,
        seconds: 0.0,
        warnings,
    })
}

/// One display arrow of the field (`specs/07` §5.2 `vatgrad`): a world position and the unit
/// direction of the field there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrow {
    /// World position in millimetres.
    pub position: [f64; 3],
    /// Unit field direction in world coordinates.
    pub direction: [f64; 3],
}

/// Field arrows inside the supra-threshold region, thinned to at most `max_arrows`; each cell
/// takes the vector of the source with the largest magnitude (`Max`) or the sum (`Sum`).
pub fn field_arrows(
    basis: &Basis,
    sources: &[SourceSetting],
    max_arrows: usize,
) -> Result<Vec<Arrow>, VatError> {
    let n_cells = basis.grid.n_cells();
    let mut best: Vec<[f32; 3]> = vec![[0f32; 3]; n_cells];
    let mut best_mag: Vec<f32> = vec![0f32; n_cells];
    for src in sources {
        let Some(d) = drive_source(basis, src)? else {
            continue;
        };
        let phi = source_phi(basis, &d.currents_ma);
        let e = field::cell_efield(&basis.grid, &basis.materials, &basis.node_of_cell, &phi);
        match basis.settings.source_combination {
            SourceCombination::Max => {
                for (c, v) in e.iter().enumerate() {
                    let m = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
                    if m > best_mag[c] {
                        best_mag[c] = m;
                        best[c] = *v;
                    }
                }
            }
            SourceCombination::Sum => {
                for (c, v) in e.iter().enumerate() {
                    for a in 0..3 {
                        best[c][a] += v[a];
                    }
                }
            }
        }
    }
    let thr = 1000.0 * basis.settings.threshold_v_per_mm as f32;
    let hits: Vec<usize> = (0..n_cells)
        .filter(|&c| {
            let v = best[c];
            (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() > thr
        })
        .collect();
    if hits.is_empty() || max_arrows == 0 {
        return Ok(Vec::new());
    }
    let stride = hits.len().div_ceil(max_arrows).max(1);
    let [nx, ny, _] = basis.grid.dims;
    let mut out = Vec::with_capacity(hits.len() / stride + 1);
    for &c in hits.iter().step_by(stride) {
        let (i, j, k) = (c % nx, (c / nx) % ny, c / (nx * ny));
        let v = best[c];
        let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if n <= 0.0 {
            continue;
        }
        let l = [
            f64::from(v[0] / n),
            f64::from(v[1] / n),
            f64::from(v[2] / n),
        ];
        let lead = &basis.lead;
        let dir = [
            lead.ex[0] * l[0] + lead.ey[0] * l[1] + lead.ez[0] * l[2],
            lead.ex[1] * l[0] + lead.ey[1] * l[1] + lead.ez[1] * l[2],
            lead.ex[2] * l[0] + lead.ey[2] * l[1] + lead.ez[2] * l[2],
        ];
        out.push(Arrow {
            position: lead.to_world(basis.grid.centre(i, j, k)),
            direction: dir,
        });
    }
    Ok(out)
}
