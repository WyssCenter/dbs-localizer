// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use dbsloc_reconstruct::electrode_by_name;

use crate::analytic::{analytic, AnalyticInput, AnalyticKind};
use crate::conductivity::{ConductivityModel, Encapsulation};
use crate::field::{cell_efield, fill_holes, open_ball};
use crate::geometry::{BodyKind, GridSpec, LeadFrame};
use crate::solver::{Control, SolverSettings};
use crate::{
    apply, prepare, solve, solve_basis_subset, CaseSetting, ContactSetting, EngineInput, Grid,
    Mode, Polarity, ResolvedModelSettings, SourceCombination, SourceSetting, VatError,
};

const HEAD: [f64; 3] = [10.0, -20.0, 5.0];

fn lead_3389() -> (LeadFrame, dbsloc_reconstruct::ElectrodeGeometry) {
    let model = electrode_by_name("Medtronic 3389")
        .cloned()
        .expect("3389 catalogued");
    let r = 0.5 * model.lead_diameter_mm;
    let span = model.contact_span_mm();
    let frame = LeadFrame::from_markers(
        HEAD,
        [HEAD[0], HEAD[1], HEAD[2] + span],
        Some([HEAD[0] + r, HEAD[1], HEAD[2]]),
        Some([HEAD[0], HEAD[1] + r, HEAD[2]]),
        &model,
    )
    .expect("frame");
    (frame, model)
}

fn contact(index: usize, polarity: Polarity, percent: f64) -> ContactSetting {
    ContactSetting {
        index,
        polarity,
        percent,
        impedance_kohm: 1.0,
    }
}

fn source(
    mode: Mode,
    amplitude: f64,
    contacts: Vec<ContactSetting>,
    case: Polarity,
) -> SourceSetting {
    SourceSetting {
        mode,
        amplitude,
        pulse_width_us: 60.0,
        contacts,
        case: CaseSetting {
            polarity: case,
            percent: if case == Polarity::Off { 0.0 } else { 100.0 },
        },
    }
}

fn settings(sigma: f64, tol: f64) -> ResolvedModelSettings {
    ResolvedModelSettings {
        threshold_v_per_mm: 0.2,
        conductivity: ConductivityModel::Homogeneous { s_per_m: sigma },
        encapsulation: Encapsulation::default(),
        remove_electrode: true,
        source_combination: SourceCombination::Max,
        solver: SolverSettings {
            rel_tol: tol,
            max_iter: 5000,
        },
    }
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

fn input_3389(sources: Vec<SourceSetting>, tol: f64, grid: GridSpec) -> EngineInput {
    let (lead, model) = lead_3389();
    EngineInput {
        lead,
        model,
        body: BodyKind::Catalogue,
        settings: settings(0.14, tol),
        sources,
        grid,
    }
}

fn voxel_at(result: &crate::FieldResult, world: [f64; 3]) -> Option<usize> {
    let a = result.affine;
    let d = result.e_mag.dims;
    let mut idx = [0usize; 3];
    for ax in 0..3 {
        let v = (world[ax] - a[ax][3]) / a[ax][ax];
        let n = v.round();
        if (v - n).abs() > 1e-6 || n < 0.0 || n >= d[ax] as f64 {
            return None;
        }
        idx[ax] = n as usize;
    }
    Some(result.e_mag.index(idx[0], idx[1], idx[2]))
}

#[test]
fn sphere_current_matches_inverse_square() {
    let t = Instant::now();
    let (lead, model) = lead_3389();
    let lead = LeadFrame { head: HEAD, ..lead };
    let mut st = settings(0.1, 1e-6);
    st.remove_electrode = false;
    st.threshold_v_per_mm = 0.05;
    let grid = GridSpec {
        fine_spacing_mm: 0.4,
        fine_half_width_mm: 5.0,
        fine_margin_z_mm: 5.0,
        growth: 1.3,
        domain_margin_mm: 25.0,
        max_cells: 5_000_000,
        output_spacing_mm: 0.5,
    };
    let src = source(
        Mode::Current,
        3.0,
        vec![contact(1, Polarity::Cathode, 100.0)],
        Polarity::Anode,
    );
    let input = EngineInput {
        lead,
        model,
        body: BodyKind::Sphere { radius_mm: 1.2 },
        settings: st,
        sources: vec![src],
        grid,
    };
    let r1 = solve(&input, &Control::default()).expect("solve");
    for rho in [3.0, 3.5, 5.0, 6.0] {
        let idx = voxel_at(&r1, [HEAD[0] + rho, HEAD[1], HEAD[2]]).expect("voxel on grid");
        let analytic = 3e-3 / (4.0 * std::f64::consts::PI * 0.1 * (rho * 1e-3) * (rho * 1e-3));
        let got = f64::from(r1.e_mag.data[idx]);
        assert!(
            (got - analytic).abs() / analytic < 0.05,
            "rho {rho}: got {got} expected {analytic}"
        );
    }

    let (_, grid_s, mats, system) = prepare(&input).expect("prepare");
    let (x, _) = system
        .solve_currents(&[(0, 3.0)], &input.settings.solver, &Control::default())
        .expect("solve");
    let phi_at = |rho: f64| {
        let c = grid_s.locate([rho, 0.0, 0.0]).expect("inside");
        let cell = grid_s.index(c[0], c[1], c[2]);
        assert!((grid_s.centres[0][c[0]] - rho).abs() < 1e-9);
        x[system.node_of_cell[cell] as usize]
    };
    let dphi = phi_at(2.8) - phi_at(4.4);
    let expect = 3.0 / (4.0 * std::f64::consts::PI * 0.1) * (1.0 / 2.8 - 1.0 / 4.4);
    assert!(
        (dphi - expect).abs() / expect < 0.05,
        "potential difference {dphi} vs {expect}"
    );
    let e = cell_efield(&grid_s, &mats, &system.node_of_cell, &x);
    assert_eq!(e.len(), grid_s.n_cells());
    eprintln!("sphere test {:?}", t.elapsed());
}

#[test]
fn solve_is_deterministic() {
    let sources = vec![source(
        Mode::Current,
        3.0,
        vec![contact(2, Polarity::Cathode, 100.0)],
        Polarity::Anode,
    )];
    let input = input_3389(sources, 1e-6, coarse_grid());
    let r1 = solve(&input, &Control::default()).expect("solve");
    let r2 = solve(&input, &Control::default()).expect("solve");
    assert_eq!(r1.e_mag.data, r2.e_mag.data, "byte-identical re-run");
}

#[test]
fn reciprocity_and_voltage_mode_potentials() {
    let t = Instant::now();
    let sources = vec![source(
        Mode::Voltage,
        3.0,
        vec![
            contact(2, Polarity::Cathode, 100.0),
            contact(3, Polarity::Cathode, 100.0),
        ],
        Polarity::Anode,
    )];
    let input = input_3389(sources, 1e-8, coarse_grid());
    let basis = solve_basis_subset(&input, &[0, 1, 2, 3], &Control::default()).expect("basis");
    let mut max_z = 0.0f64;
    for row in &basis.z_kohm {
        for v in row {
            max_z = max_z.max(v.abs());
        }
    }
    for j in 0..4 {
        for k in 0..4 {
            let d = (basis.z_kohm[j][k] - basis.z_kohm[k][j]).abs();
            assert!(d <= 1e-6 * max_z, "Z[{j}][{k}] asymmetry {d}");
        }
    }
    let r = apply(&basis, &input.sources).expect("apply");
    let v = &r.contact_potentials_v[0];
    assert!((v[1] + 3.0).abs() < 1e-6, "contact 2 potential {}", v[1]);
    assert!((v[2] + 3.0).abs() < 1e-6, "contact 3 potential {}", v[2]);
    let i = &r.contact_currents_ma[0];
    assert!(i[1] < 0.0 && i[2] < 0.0 && i[0] == 0.0 && i[3] == 0.0);
    assert!(
        v[0] > -3.0 && v[0] < 0.0,
        "floating contact 1 potential {}",
        v[0]
    );
    assert!(
        v[3] > -3.0 && v[3] < 0.0,
        "floating contact 4 potential {}",
        v[3]
    );

    let bipolar = vec![source(
        Mode::Voltage,
        3.0,
        vec![
            contact(2, Polarity::Cathode, 100.0),
            contact(3, Polarity::Anode, 100.0),
        ],
        Polarity::Off,
    )];
    let r2 = apply(&basis, &bipolar).expect("apply");
    let v2 = &r2.contact_potentials_v[0];
    assert!(
        (v2[2] - v2[1] - 3.0).abs() < 1e-6,
        "bipolar voltage difference {}",
        v2[2] - v2[1]
    );
    let i2 = &r2.contact_currents_ma[0];
    assert!(
        (i2[1] + i2[2]).abs() < 1e-9,
        "net current is zero with the case off"
    );
    eprintln!("reciprocity test {:?}", t.elapsed());
}

#[test]
fn superposition_matches_direct_solve() {
    let t = Instant::now();
    let a = source(
        Mode::Current,
        1.2,
        vec![contact(2, Polarity::Cathode, 100.0)],
        Polarity::Anode,
    );
    let b = source(
        Mode::Current,
        0.8,
        vec![contact(3, Polarity::Cathode, 100.0)],
        Polarity::Anode,
    );
    let ab = source(
        Mode::Current,
        2.0,
        vec![
            contact(2, Polarity::Cathode, 60.0),
            contact(3, Polarity::Cathode, 40.0),
        ],
        Polarity::Anode,
    );
    let input = input_3389(vec![a, b, ab], 1e-10, coarse_grid());
    let basis = solve_basis_subset(&input, &[1, 2], &Control::default()).expect("basis");
    let r = apply(&basis, &input.sources).expect("apply");
    let ia = &r.contact_currents_ma[0];
    let ib = &r.contact_currents_ma[1];
    let iab = &r.contact_currents_ma[2];
    for k in 0..4 {
        assert!(
            (iab[k] - (ia[k] + ib[k])).abs() < 1e-12,
            "currents add at {k}"
        );
    }

    let (_, grid, mats, system) = prepare(&input).expect("prepare");
    let (direct, _) = system
        .solve_currents(
            &[(1, -1.2), (2, -0.8)],
            &input.settings.solver,
            &Control::default(),
        )
        .expect("direct");
    let s1 = basis.solutions[1].as_ref().expect("solved");
    let s2 = basis.solutions[2].as_ref().expect("solved");
    let mut max_phi = 0.0f64;
    let mut max_diff = 0.0f64;
    for ((d, p), q) in direct.iter().zip(s1).zip(s2) {
        let sup = -1.2 * p - 0.8 * q;
        max_phi = max_phi.max(d.abs());
        max_diff = max_diff.max((d - sup).abs());
    }
    assert!(
        max_diff / max_phi <= 1e-8,
        "superposition error {}",
        max_diff / max_phi
    );
    let e_direct = cell_efield(&grid, &mats, &system.node_of_cell, &direct);
    assert_eq!(e_direct.len(), grid.n_cells());
    eprintln!("superposition test {:?}", t.elapsed());
}

fn monopolar_case(grid: GridSpec) {
    let t = Instant::now();
    let sources = vec![source(
        Mode::Current,
        1.0,
        vec![contact(2, Polarity::Cathode, 100.0)],
        Polarity::Anode,
    )];
    let input = input_3389(sources, 1e-6, grid);
    let basis = solve_basis_subset(&input, &[1], &Control::default()).expect("basis");
    let st = basis.stats[0];
    eprintln!(
        "monopolar: cells {:?} ({}), domain x [{:.1}, {:.1}] z [{:.1}, {:.1}] mm, iterations {}, residual {:e}",
        basis.grid.dims,
        basis.grid.n_cells(),
        basis.grid.faces[0][0],
        basis.grid.faces[0][basis.grid.dims[0]],
        basis.grid.faces[2][0],
        basis.grid.faces[2][basis.grid.dims[2]],
        st.iterations,
        st.rel_residual
    );
    let r = apply(&basis, &input.sources).expect("apply");
    let mut kept = basis.clone();
    kept.settings.remove_electrode = false;
    let r_kept = apply(&kept, &input.sources).expect("apply");
    let three = vec![source(
        Mode::Current,
        3.0,
        vec![contact(2, Polarity::Cathode, 100.0)],
        Polarity::Anode,
    )];
    let r3 = apply(&basis, &three).expect("apply");
    eprintln!(
        "monopolar 1 mA, 0.2 V/mm: V_raw {:.2} (no removal {:.2}) mm3, V_final {:.2} mm3; 3 mA V_raw {:.2} mm3; solve {:.1}s",
        r.volumes.raw_mm3,
        r_kept.volumes.raw_mm3,
        r.volumes.final_mm3,
        r3.volumes.raw_mm3,
        basis.seconds
    );
    assert!(r_kept.volumes.raw_mm3 > 10.0 && r_kept.volumes.raw_mm3 < 200.0);
    assert!(r.volumes.raw_mm3 > 3.0 && r.volumes.raw_mm3 < r_kept.volumes.raw_mm3);
    assert!(r3.volumes.raw_mm3 > 20.0 && r3.volumes.raw_mm3 < 200.0);
    assert!(r.volumes.final_mm3 <= r.volumes.raw_mm3 * 1.2 + 1.0);
    assert!(r3.volumes.final_mm3 > 0.0);
    assert!(!r.touches_boundary);
    eprintln!("monopolar test {:?}", t.elapsed());
}

#[test]
fn monopolar_cathode_vta_volume_is_plausible() {
    monopolar_case(GridSpec {
        fine_spacing_mm: 0.5,
        fine_half_width_mm: 4.0,
        fine_margin_z_mm: 4.0,
        growth: 1.4,
        domain_margin_mm: 15.0,
        max_cells: 20_000_000,
        output_spacing_mm: 0.3,
    });
}

#[test]
#[ignore = "spec domain (40 mm) at 0.25 mm; run with --ignored"]
fn monopolar_cathode_vta_volume_spec_domain() {
    monopolar_case(GridSpec {
        fine_spacing_mm: 0.25,
        fine_half_width_mm: 5.0,
        fine_margin_z_mm: 5.0,
        growth: 1.25,
        domain_margin_mm: 40.0,
        max_cells: 20_000_000,
        output_spacing_mm: 0.2,
    });
}

#[test]
fn analytical_radii_and_voxel_volume() {
    let (lead, model) = lead_3389();
    assert!((crate::kuncel_radius_mm(3.0) - 3.6307).abs() < 1e-4);
    assert!((crate::kuncel_radius_mm(1.0) - 2.0226).abs() < 1e-4);
    assert!((crate::kuncel_radius_mm(2.0) - 2.9388).abs() < 1e-4);
    assert!((crate::maedler_radius_mm(3.0, 1000.0) - 3.3941).abs() < 1e-4);
    assert!((crate::maedler_radius_mm(2.0, 1000.0) - 2.7923).abs() < 1e-4);
    assert!((crate::maedler_radius_mm(3.0, 500.0) - 4.4242).abs() < 1e-4);
    assert!((crate::maedler_radius_mm(3.0, 2000.0) - 2.0185).abs() < 1e-4);
    let thr = 200.0;
    assert!((crate::dembek_radius_mm(3.0 / 1000.0, thr, 1.0) - 3.2863).abs() < 1e-4);
    assert!((crate::dembek_radius_mm(3.0 / 1000.0, thr, 1.5) - 3.7114).abs() < 1e-4);

    let src = |mode: Mode, amp: f64| {
        source(
            mode,
            amp,
            vec![contact(2, Polarity::Cathode, 100.0)],
            Polarity::Off,
        )
    };
    let base = |kind: AnalyticKind, sources: Vec<SourceSetting>| AnalyticInput {
        lead,
        model: model.clone(),
        kind,
        threshold_v_per_mm: 0.2,
        dembek_reference_pulse_width_us: 60.0,
        sources,
    };
    let r = analytic(&base(AnalyticKind::Kuncel, vec![src(Mode::Voltage, 3.0)])).expect("kuncel");
    assert!((r.source_radii_mm[0] - 3.6307).abs() < 1e-4);
    let sphere = 4.0 / 3.0 * std::f64::consts::PI * 3.6307f64.powi(3);
    assert!((sphere - 200.47).abs() < 0.05);
    assert!(
        (r.volumes.raw_mm3 - sphere).abs() / sphere < 0.02,
        "voxel volume {} vs sphere {sphere}",
        r.volumes.raw_mm3
    );
    let again =
        analytic(&base(AnalyticKind::Kuncel, vec![src(Mode::Voltage, 3.0)])).expect("kuncel");
    assert_eq!(r.binary.data, again.binary.data);

    let d = analytic(&base(AnalyticKind::Dembek, vec![src(Mode::Voltage, 3.0)])).expect("dembek");
    assert!((d.source_radii_mm[0] - 3.2863).abs() < 1e-4);
    let dc = analytic(&base(AnalyticKind::Dembek, vec![src(Mode::Current, 3.0)])).expect("dembek");
    assert!((dc.source_radii_mm[0] - 3.2863).abs() < 1e-4);

    let two = source(
        Mode::Voltage,
        3.0,
        vec![
            contact(2, Polarity::Cathode, 50.0),
            contact(3, Polarity::Cathode, 50.0),
        ],
        Polarity::Off,
    );
    assert!(matches!(
        analytic(&base(AnalyticKind::Kuncel, vec![two])),
        Err(VatError::Montage(_))
    ));
    assert!(matches!(
        analytic(&base(AnalyticKind::Kuncel, vec![src(Mode::Current, 3.0)])),
        Err(VatError::ModeNotSupported(_))
    ));
    let weak = analytic(&base(AnalyticKind::Kuncel, vec![src(Mode::Voltage, 0.05)])).expect("weak");
    assert_eq!(weak.volumes.raw_mm3, 0.0);
    assert_eq!(weak.warnings.len(), 1);
}

#[test]
fn cancel_flag_stops_the_solve() {
    let sources = vec![source(
        Mode::Current,
        1.0,
        vec![contact(2, Polarity::Cathode, 100.0)],
        Polarity::Anode,
    )];
    let input = input_3389(sources, 1e-8, coarse_grid());
    let flag = AtomicBool::new(true);
    let ctl = Control {
        cancel: Some(&flag),
        progress: None,
    };
    let err = solve_basis_subset(&input, &[1], &ctl).expect_err("cancelled");
    assert!(matches!(err, VatError::Cancelled));
    flag.store(false, Ordering::Relaxed);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let cb = |_p: crate::Progress| {
        calls.fetch_add(1, Ordering::Relaxed);
    };
    let ctl = Control {
        cancel: Some(&flag),
        progress: Some(&cb),
    };
    solve_basis_subset(&input, &[1], &ctl).expect("solve");
    assert!(calls.load(Ordering::Relaxed) > 0);
}

#[test]
fn hole_filling_and_opening_on_a_blob() {
    let n = 40usize;
    let mut g = Grid {
        dims: [n, n, n],
        data: vec![0u8; n * n * n],
    };
    let c = 20.0f64;
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                let d = ((i as f64 - c).powi(2) + (j as f64 - c).powi(2) + (k as f64 - c).powi(2))
                    .sqrt();
                if d <= 10.0 && d > 3.0 {
                    let q = g.index(i, j, k);
                    g.data[q] = 1;
                }
            }
        }
    }
    for i in 30..38 {
        let idx = g.index(i, 20, 20);
        g.data[idx] = 1;
    }
    let idx = g.index(3, 3, 3);
    g.data[idx] = 1;
    let centre = g.index(20, 20, 20);
    assert_eq!(g.data[centre], 0);
    fill_holes(&mut g);
    assert_eq!(g.data[centre], 1, "cavity filled");
    let opened = open_ball(&g, 3);
    assert_eq!(opened.data[centre], 1);
    assert_eq!(
        opened.data[opened.index(3, 3, 3)],
        0,
        "isolated voxel removed"
    );
    assert_eq!(
        opened.data[opened.index(36, 20, 20)],
        0,
        "thin spike removed"
    );
    let ball: usize = (0..n * n * n)
        .filter(|&q| {
            let (i, j, k) = (q % n, (q / n) % n, q / (n * n));
            ((i as f64 - c).powi(2) + (j as f64 - c).powi(2) + (k as f64 - c).powi(2)).sqrt()
                <= 10.0
        })
        .count();
    let kept = opened.data.iter().filter(|&&v| v != 0).count();
    assert!(
        kept as f64 > 0.9 * ball as f64 && kept <= ball,
        "ball kept: {kept} of {ball}"
    );
}

#[test]
fn directed_lead_has_distinct_segments_and_unresolved_contact_errors() {
    let model = electrode_by_name("Boston Scientific Vercise Directed")
        .cloned()
        .expect("catalogued");
    let r = 0.5 * model.lead_diameter_mm;
    let frame = LeadFrame::from_markers(
        [0.0; 3],
        [0.0, 0.0, model.contact_span_mm()],
        Some([r, 0.0, 0.0]),
        Some([0.0, r, 0.0]),
        &model,
    )
    .expect("frame");
    let sources = vec![source(
        Mode::Current,
        1.0,
        vec![contact(2, Polarity::Cathode, 100.0)],
        Polarity::Anode,
    )];
    let grid = GridSpec {
        fine_spacing_mm: 0.1,
        fine_half_width_mm: 1.0,
        fine_margin_z_mm: 1.0,
        growth: 2.0,
        domain_margin_mm: 40.0,
        max_cells: 5_000_000,
        output_spacing_mm: 0.4,
    };
    let input = EngineInput {
        lead: frame,
        model,
        body: BodyKind::Catalogue,
        settings: settings(0.14, 1e-6),
        sources,
        grid,
    };
    let (_, _, mats, system) = prepare(&input).expect("prepare");
    let count = |k: u16| {
        mats.class
            .iter()
            .filter(|&&c| c == crate::geometry::CONTACT_BASE + k)
            .count()
    };
    assert!(count(1) > 0 && count(2) > 0 && count(3) > 0);
    let ratio = count(1) as f64 / count(2) as f64;
    assert!((0.8..1.25).contains(&ratio), "segments comparable: {ratio}");
    assert!(system.contact_node.iter().all(Option::is_some));

    let mut coarse = input.clone();
    coarse.grid.fine_spacing_mm = 2.0;
    coarse.grid.fine_half_width_mm = 4.0;
    coarse.grid.growth = 2.0;
    let err = solve_basis_subset(&coarse, &[1], &Control::default());
    assert!(matches!(
        err,
        Err(VatError::ContactNotResolved { contact: 1 })
    ));
}
