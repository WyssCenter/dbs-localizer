#![allow(clippy::unwrap_used, clippy::needless_range_loop)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use dbsloc_reconstruct::electrode_by_name;
use dbsloc_vat::conductivity::{ConductivityModel, Encapsulation};
use dbsloc_vat::geometry::{BodyKind, GridSpec, LeadFrame};
use dbsloc_vat::solver::{Control, SolverSettings};
use dbsloc_vat::{
    apply, assemble_system, Basis, BasisSolver, CaseSetting, ContactSetting, CpuSolver,
    EngineInput, Mode, Polarity, ResolvedModelSettings, SourceCombination, SourceSetting, VatError,
};
use dbsloc_vat_gpu::{
    choose_vat_backend, vat_capacity, GpuVatSolver, VatBackend, VatCapacityError, VatChoiceError,
    VatRequest,
};
use fastwarp_gpu::CapacityCaps;

const HEAD: [f64; 3] = [10.0, -20.0, 5.0];

fn lead_3389() -> (LeadFrame, dbsloc_reconstruct::ElectrodeGeometry) {
    let model = electrode_by_name("Medtronic 3389").cloned().unwrap();
    let r = 0.5 * model.lead_diameter_mm;
    let span = model.fiducial_span_mm();
    let frame = LeadFrame::from_markers(
        HEAD,
        [HEAD[0], HEAD[1], HEAD[2] + span],
        Some([HEAD[0] + r, HEAD[1], HEAD[2]]),
        Some([HEAD[0], HEAD[1] + r, HEAD[2]]),
        &model,
    )
    .unwrap();
    (frame, model)
}

fn sources() -> Vec<SourceSetting> {
    vec![SourceSetting {
        mode: Mode::Current,
        amplitude: 1.0,
        pulse_width_us: 60.0,
        contacts: vec![ContactSetting {
            index: 2,
            polarity: Polarity::Cathode,
            percent: 100.0,
            impedance_kohm: 1.0,
        }],
        case: CaseSetting {
            polarity: Polarity::Anode,
            percent: 100.0,
        },
    }]
}

fn coarse_grid() -> GridSpec {
    GridSpec {
        fine_spacing_mm: 0.5,
        fine_half_width_mm: 3.0,
        fine_margin_z_mm: 3.0,
        growth: 1.5,
        domain_margin_mm: 15.0,
        max_cells: 5_000_000,
        output_spacing_mm: 0.5,
    }
}

fn spec_grid() -> GridSpec {
    GridSpec {
        fine_spacing_mm: 0.25,
        fine_half_width_mm: 5.0,
        fine_margin_z_mm: 5.0,
        growth: 1.25,
        domain_margin_mm: 40.0,
        max_cells: 20_000_000,
        output_spacing_mm: 0.2,
    }
}

fn input(grid: GridSpec, tol: f64) -> EngineInput {
    let (lead, model) = lead_3389();
    EngineInput {
        lead,
        model,
        body: BodyKind::Catalogue,
        settings: ResolvedModelSettings {
            threshold_v_per_mm: 0.2,
            conductivity: ConductivityModel::Homogeneous { s_per_m: 0.14 },
            encapsulation: Encapsulation::default(),
            remove_electrode: true,
            source_combination: SourceCombination::Max,
            solver: SolverSettings {
                rel_tol: tol,
                max_iter: 5000,
            },
        },
        sources: sources(),
        grid,
    }
}

fn solution_bits(b: &Basis, k: usize) -> Vec<u64> {
    b.solutions[k]
        .as_ref()
        .unwrap()
        .iter()
        .map(|v| v.to_bits())
        .collect()
}

#[test]
fn vat_spmv_matches_cpu_matvec() {
    let Some(be) = common::backend() else { return };
    let solver = GpuVatSolver::new(be.context()).unwrap();
    let system = assemble_system(&input(coarse_grid(), 1e-6)).unwrap();
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let x: Vec<f32> = (0..system.n)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        })
        .collect();
    let gpu = solver.spmv(&system, &x).unwrap();
    let x64: Vec<f64> = x.iter().map(|&v| f64::from(v)).collect();
    let mut cpu = vec![0.0f64; system.n];
    system.matvec(&x64, &mut cpu);
    let mut worst = 0.0f64;
    for i in 0..system.n {
        let scale = system.diag()[i].abs() * x64[i].abs().max(0.25);
        worst = worst.max((f64::from(gpu[i]) - cpu[i]).abs() / scale);
    }
    eprintln!(
        "spmv: {} rows, worst row error {:.3e} (relative to |diag x|)",
        system.n, worst
    );
    assert!(worst < 1e-5, "spmv error {worst}");
}

#[test]
fn vat_solve_is_deterministic() {
    let Some(be) = common::backend() else { return };
    let solver = GpuVatSolver::new(be.context()).unwrap();
    let inp = input(coarse_grid(), 1e-6);
    let a = solver.solve_basis(&inp, &[1], &Control::default()).unwrap();
    let b = solver.solve_basis(&inp, &[1], &Control::default()).unwrap();
    assert_eq!(solution_bits(&a, 1), solution_bits(&b, 1));
    assert_eq!(
        a.stats.iter().map(|s| s.iterations).collect::<Vec<_>>(),
        b.stats.iter().map(|s| s.iterations).collect::<Vec<_>>()
    );
}

#[test]
fn vat_basis_matches_cpu_reference() {
    let Some(be) = common::backend() else { return };
    let solver = GpuVatSolver::new(be.context()).unwrap();
    let inp = input(coarse_grid(), 1e-6);
    let t = Instant::now();
    let cpu = CpuSolver
        .solve_basis(&inp, &[1, 2], &Control::default())
        .unwrap();
    let cpu_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let gpu = solver
        .solve_basis(&inp, &[1, 2], &Control::default())
        .unwrap();
    let gpu_s = t.elapsed().as_secs_f64();
    let mut z_worst = 0.0f64;
    for j in 0..4 {
        for k in [1, 2] {
            let (a, b) = (cpu.z_kohm[j][k], gpu.z_kohm[j][k]);
            z_worst = z_worst.max((a - b).abs() / a.abs().max(b.abs()));
        }
    }
    let rc = apply(&cpu, &inp.sources).unwrap();
    let rg = apply(&gpu, &inp.sources).unwrap();
    let emax = rc.e_mag.data.iter().fold(0.0f32, |m, &v| m.max(v));
    let mut e_worst = 0.0f64;
    let mut counted = 0usize;
    for (a, b) in rc.e_mag.data.iter().zip(&rg.e_mag.data) {
        if *a > 0.1 * emax {
            counted += 1;
            e_worst = e_worst.max(f64::from((a - b).abs()) / f64::from(*a));
        }
    }
    let diff_max = rc
        .e_mag
        .data
        .iter()
        .zip(&rg.e_mag.data)
        .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    for (c, g) in cpu.stats.iter().zip(&gpu.stats) {
        eprintln!(
            "contact {}: cpu {} it {:.2e}, gpu {} it true residual {:.2e}",
            c.contact, c.iterations, c.rel_residual, g.iterations, g.rel_residual
        );
    }
    eprintln!(
        "coarse grid {:?}: Z worst rel diff {:.3e}, |E| worst rel diff over {} voxels > 10 % max {:.3e}, max |dE| / max |E| {:.3e}, V_raw cpu {:.3} gpu {:.3}; cpu {:.2}s gpu {:.2}s",
        cpu.grid.dims,
        z_worst,
        counted,
        e_worst,
        f64::from(diff_max / emax),
        rc.volumes.raw_mm3,
        rg.volumes.raw_mm3,
        cpu_s,
        gpu_s
    );
    assert!(z_worst < 0.005, "Z differs by {z_worst}");
    assert!(e_worst < 0.01, "|E| differs by {e_worst}");
}

#[test]
fn vat_cancel_returns_cancelled() {
    let Some(be) = common::backend() else { return };
    let solver = GpuVatSolver::new(be.context()).unwrap();
    let flag = AtomicBool::new(true);
    let ctl = Control {
        cancel: Some(&flag),
        progress: None,
    };
    let r = solver.solve_basis(&input(coarse_grid(), 1e-6), &[1], &ctl);
    assert!(matches!(r, Err(VatError::Cancelled)));
}

#[test]
fn vat_capacity_refuses_an_absurd_system() {
    let caps = CapacityCaps {
        max_storage_buffer_binding_size: 128 << 20,
        memory_budget: 1 << 30,
    };
    assert!(vat_capacity(600_000, 4_000_000, &caps, 65_535).is_ok());
    assert!(matches!(
        vat_capacity(2_000_000_000, 14_000_000_000, &caps, 65_535),
        Err(VatCapacityError::Dispatch { .. })
    ));
    assert!(matches!(
        vat_capacity(10_000_000, 70_000_000, &caps, 65_535),
        Err(VatCapacityError::Binding { .. })
    ));
    let small = CapacityCaps {
        max_storage_buffer_binding_size: 1 << 30,
        memory_budget: 64 << 20,
    };
    assert!(matches!(
        vat_capacity(10_000_000, 70_000_000, &small, 65_535),
        Err(VatCapacityError::Memory { .. })
    ));
}

#[test]
fn vat_backend_choice_follows_the_rule() {
    let cpu = choose_vat_backend(VatRequest::Cpu, None, 10, 70).unwrap();
    assert_eq!(cpu.backend, VatBackend::Cpu);
    let auto = choose_vat_backend(VatRequest::Auto, None, 10, 70).unwrap();
    assert_eq!(auto.backend, VatBackend::Cpu);
    assert_eq!(
        choose_vat_backend(VatRequest::Wgpu, None, 10, 70),
        Err(VatChoiceError::NoDevice)
    );
    let Some(be) = common::backend() else { return };
    let ctx = be.context();
    let ok = choose_vat_backend(VatRequest::Auto, Some(ctx), 600_000, 4_000_000).unwrap();
    assert_eq!(ok.backend, VatBackend::Wgpu);
    let too_big = choose_vat_backend(VatRequest::Auto, Some(ctx), usize::MAX / 1024, 0).unwrap();
    assert_eq!(too_big.backend, VatBackend::Cpu);
    assert!(matches!(
        choose_vat_backend(VatRequest::Wgpu, Some(ctx), usize::MAX / 1024, 0),
        Err(VatChoiceError::Capacity(_))
    ));
}

#[test]
#[ignore = "spec-domain timing; run: cargo test --release -p dbsloc-vat-gpu vat_spec_domain -- --ignored --nocapture"]
fn vat_spec_domain_timing() {
    let Some(be) = common::backend() else { return };
    let solver = GpuVatSolver::new(be.context()).unwrap();
    let inp = input(spec_grid(), 1e-6);
    let contacts = [0usize, 1, 2, 3];
    let t = Instant::now();
    let gpu = solver
        .solve_basis(&inp, &contacts, &Control::default())
        .unwrap();
    let gpu_total = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let cpu = CpuSolver
        .solve_basis(&inp, &contacts, &Control::default())
        .unwrap();
    let cpu_total = t.elapsed().as_secs_f64();
    let mut z_worst = 0.0f64;
    for j in 0..4 {
        for k in 0..4 {
            let (a, b) = (cpu.z_kohm[j][k], gpu.z_kohm[j][k]);
            z_worst = z_worst.max((a - b).abs() / a.abs().max(b.abs()));
        }
    }
    let rc = apply(&cpu, &inp.sources).unwrap();
    let rg = apply(&gpu, &inp.sources).unwrap();
    let emax = rc.e_mag.data.iter().fold(0.0f32, |m, &v| m.max(v));
    let mut e_worst = 0.0f64;
    for (a, b) in rc.e_mag.data.iter().zip(&rg.e_mag.data) {
        if *a > 0.1 * emax {
            e_worst = e_worst.max(f64::from((a - b).abs()) / f64::from(*a));
        }
    }
    for (c, g) in cpu.stats.iter().zip(&gpu.stats) {
        eprintln!(
            "contact {}: cpu {} it, gpu {} it (true residual {:.2e})",
            c.contact, c.iterations, g.iterations, g.rel_residual
        );
    }
    eprintln!(
        "spec domain {:?} ({} cells): gpu {:.2}s total, {:.2}s per contact; cpu {:.2}s total, {:.2}s per contact; Z worst {:.3e}; |E| worst {:.3e}; V_raw cpu {:.2} gpu {:.2}; adapter {}",
        gpu.grid.dims,
        gpu.grid.n_cells(),
        gpu_total,
        gpu_total / contacts.len() as f64,
        cpu_total,
        cpu_total / contacts.len() as f64,
        z_worst,
        e_worst,
        rc.volumes.raw_mm3,
        rg.volumes.raw_mm3,
        be.context().info.name
    );
}
