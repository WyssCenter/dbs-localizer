#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::{AtomicBool, Ordering};

use dbsloc_connectome::{write_data_mat, FibreSet, FunctionalConnectome};
use fastwarp_matfile::mat5::read_mat5_file;

use super::*;
use crate::connectome_maps::fibervals::fibre_values;
use crate::connectome_maps::functional::tests::build_connectome;
use crate::connectome_maps::nii::{diagonal_affine, write_nii};
use crate::connectome_maps::{no_progress, Progress};

const SPACE: &str = "MNI152NLin2009bAsym";
const LABEL: &str = "stim1";

fn line(y: f32, z: f32) -> Vec<[f32; 3]> {
    (-12..=12).map(|i| [i as f32 * 0.5, y, z]).collect()
}

fn fibres() -> FibreSet {
    let mut f = vec![
        line(0.0, 0.0),
        line(1.0, 0.0),
        line(0.0, 0.2),
        line(4.0, 0.0),
    ];
    for i in 0..6 {
        f.push(line(20.0 + i as f32, 30.0));
    }
    FibreSet::from_fibres(&f).unwrap()
}

fn structural_connectome(root: &Path) -> ResolvedConnectome {
    let folder = root.join("dMRI").join("Synth");
    std::fs::create_dir_all(&folder).unwrap();
    write_data_mat(&folder.join("data.mat"), &fibres()).unwrap();
    std::fs::write(
        folder.join("dataset_info.json"),
        br#"{"name":"Synth","tag":"SYN"}"#,
    )
    .unwrap();
    resolve_connectome(&ConnectomeRoots::from_primary(root), "Synth").unwrap()
}

fn stim_dir(root: &Path) -> PathBuf {
    stimulation_folder(root, "01", SPACE, LABEL)
}

fn write_vta(root: &Path, side: &str, centre: [f64; 3]) {
    let mut v = NiiVolume::zeros(
        [3, 3, 3],
        diagonal_affine(
            [1.0; 3],
            [centre[0] - 1.0, centre[1] - 1.0, centre[2] - 1.0],
        ),
    );
    v.data.fill(1.0);
    let dir = stim_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    write_nii(
        &dir.join(format!("sub-01_sim-binary_model-kuncel_hemi-{side}.nii")),
        &v,
    )
    .unwrap();
}

fn request(root: &Path, conn: &ResolvedConnectome) -> StructuralRequest {
    StructuralRequest {
        derivatives_root: root.to_path_buf(),
        subject_id: "01".into(),
        label: LABEL.into(),
        space: SPACE.into(),
        connectome: conn.clone(),
        kind: SeedKind::Binary,
        model: None,
        grid: OutputGrid::Mm2,
        seed_file: None,
        write_subset: true,
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    n.sort();
    n
}

#[test]
fn structural_job_writes_the_named_files_with_sidecars() {
    let d = tempfile::tempdir().unwrap();
    let conn = structural_connectome(&d.path().join("connectomes"));
    assert_eq!(conn.tag, "SYN");
    assert_eq!(conn.digest.len(), 64);
    write_vta(d.path(), "R", [0.0, 0.0, 0.0]);
    let engine = StructuralEngine::load(&conn).unwrap();
    let cancel = AtomicBool::new(false);
    let out = run_structural(&engine, &request(d.path(), &conn), &cancel, &no_progress).unwrap();
    assert!(out.skipped.is_none());
    let base = "sub-01_sim-binary_model-kuncel";
    let want: Vec<String> = [
        format!("{base}_seed-dMRI.json"),
        format!("{base}_seed-dMRI.nii"),
        format!("{base}_seed-dMRI_conn-SYN_fibers.json"),
        format!("{base}_seed-dMRI_conn-SYN_fibers.mat"),
        format!("{base}_seed-dMRI_conn-SYN_strucmap.json"),
        format!("{base}_seed-dMRI_conn-SYN_strucmap.nii"),
        format!("{base}_seed-dMRI_hemi-L.json"),
        format!("{base}_seed-dMRI_hemi-L.nii"),
        format!("{base}_seed-dMRI_hemi-R.json"),
        format!("{base}_seed-dMRI_hemi-R.nii"),
        format!("{base}_hemi-R.nii"),
    ]
    .into_iter()
    .collect();
    let mut want = want;
    want.sort();
    assert_eq!(names(&stim_dir(d.path())), want);
    let dir = stim_dir(d.path());
    let map = read_nii(&dir.join(format!("{base}_seed-dMRI_conn-SYN_strucmap.nii"))).unwrap();
    assert_eq!(map.dims, [91, 109, 91]);
    assert_eq!(map.data.iter().sum::<f32>(), 21.0);
    let side: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(format!("{base}_seed-dMRI_conn-SYN_strucmap.json")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(side["software"], version_string());
    assert_eq!(side["connectome"]["tag"], "SYN");
    assert_eq!(side["connectome"]["class"], "dMRI");
    assert_eq!(side["connectome"]["licence_record"], Value::Null);
    assert_eq!(side["connectome"]["digest"], conn.digest);
    assert_eq!(side["params"]["connected_fibres"], 3);
    assert_eq!(side["params"]["grid"], "2mm");
    let seed_file = dir.join(format!("{base}_seed-dMRI.nii"));
    assert_eq!(side["seed"]["sha256"], file_sha256_hex(&seed_file).unwrap());
    assert!(side["created_at"].as_str().unwrap().ends_with('Z'));
    let subset =
        read_mat5_file(&dir.join(format!("{base}_seed-dMRI_conn-SYN_fibers.mat"))).unwrap();
    let src = subset
        .iter()
        .find(|(k, _)| k == "source_fiber_index")
        .unwrap();
    assert_eq!(src.1.dims(), vec![3, 1]);
    assert!(!names(&dir).iter().any(|n| n.starts_with(".partial-")));
}

#[test]
fn an_unconnected_seed_is_skipped_and_writes_no_map() {
    let d = tempfile::tempdir().unwrap();
    let conn = structural_connectome(&d.path().join("connectomes"));
    write_vta(d.path(), "R", [30.0, 30.0, 10.0]);
    let engine = StructuralEngine::load(&conn).unwrap();
    let cancel = AtomicBool::new(false);
    let out = run_structural(&engine, &request(d.path(), &conn), &cancel, &no_progress).unwrap();
    assert!(out
        .skipped
        .as_deref()
        .unwrap()
        .contains("no connected fibres"));
    let n = names(&stim_dir(d.path()));
    assert!(!n.iter().any(|f| f.contains("strucmap")));
    assert!(n.iter().any(|f| f.ends_with("_seed-dMRI.nii")));
}

#[test]
fn cancel_mid_run_commits_nothing_and_leaves_no_partials() {
    let d = tempfile::tempdir().unwrap();
    let conn = structural_connectome(&d.path().join("connectomes"));
    write_vta(d.path(), "R", [0.0, 0.0, 0.0]);
    let engine = StructuralEngine::load(&conn).unwrap();
    let cancel = AtomicBool::new(false);
    let progress = |_: &Progress| cancel.store(true, Ordering::Relaxed);
    let r = run_structural(&engine, &request(d.path(), &conn), &cancel, &progress);
    assert_eq!(r.unwrap_err(), MapError::Cancelled);
    assert_eq!(
        names(&stim_dir(d.path())),
        vec!["sub-01_sim-binary_model-kuncel_hemi-R.nii".to_owned()]
    );
}

#[test]
fn a_space_mismatch_and_a_missing_stimulation_are_reported() {
    let d = tempfile::tempdir().unwrap();
    let conn = structural_connectome(&d.path().join("connectomes"));
    let engine = StructuralEngine::load(&conn).unwrap();
    let cancel = AtomicBool::new(false);
    let mut req = request(d.path(), &conn);
    let e = run_structural(&engine, &req, &cancel, &no_progress).unwrap_err();
    assert_eq!(e.code(), "E_SEED_EMPTY");
    req.space = "Other".into();
    let e = run_structural(&engine, &req, &cancel, &no_progress).unwrap_err();
    assert_eq!(e.code(), "E_CONN_SPACE");
}

#[test]
fn fibervals_job_writes_one_file_per_available_side() {
    let d = tempfile::tempdir().unwrap();
    let conn = structural_connectome(&d.path().join("connectomes"));
    let mut e = NiiVolume::zeros([6, 5, 5], diagonal_affine([1.0; 3], [-3.0, -2.0, -2.0]));
    for (x, v) in [(0usize, 300.0f32), (1, 500.0), (2, 100.0)] {
        let n = e.index(x + 2, 2, 2);
        e.data[n] = v;
    }
    let dir = stim_dir(d.path());
    std::fs::create_dir_all(&dir).unwrap();
    write_nii(&dir.join("sub-01_sim-efield_model-simbio_hemi-R.nii"), &e).unwrap();
    let set = fibres();
    let cancel = AtomicBool::new(false);
    let out = run_fibervals(
        &set,
        &FibervalsRequest {
            derivatives_root: d.path().to_path_buf(),
            subject_id: "01".into(),
            label: LABEL.into(),
            space: SPACE.into(),
            connectome: conn,
            kind: SeedKind::Efield,
            model: None,
            threshold: 200.0,
            sides: vec![Side::Right, Side::Left],
        },
        &cancel,
        &no_progress,
    )
    .unwrap();
    assert_eq!(out.warnings.len(), 1);
    let file = dir.join("sub-01_sim-efield_model-simbio_hemi-R_conn-SYN_fibervals.mat");
    assert!(file.is_file());
    assert!(dir
        .join("sub-01_sim-efield_model-simbio_hemi-R_conn-SYN_fibervals.json")
        .is_file());
    assert!(!dir
        .join("sub-01_sim-efield_model-simbio_hemi-L_conn-SYN_fibervals.mat")
        .exists());
    let vars = read_mat5_file(&file).unwrap();
    let get = |n: &str| vars.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    assert_eq!(
        get("total_fibers"),
        Some(fastwarp_matfile::Mat5Value::scalar(10.0))
    );
    assert_eq!(
        get("threshold_vpm"),
        Some(fastwarp_matfile::Mat5Value::scalar(200.0))
    );
    let idx = get("fiber_index").unwrap();
    assert_eq!(idx.dims(), vec![1, 1]);
}

#[test]
fn functional_label_follows_the_spec() {
    assert_eq!(functional_label("GSP1000v2", None), "GSP1000v2xFullSet");
    assert_eq!(
        functional_label("PPMI74P15C", Some("Pa tients!")),
        "PPMI74P15CxPatients"
    );
}

fn functional_setup(root: &Path) -> (PathBuf, FunctionalConnectome, ResolvedConnectome) {
    let folder = root.join("fMRI").join("Synth");
    build_connectome(&folder);
    let conn = FunctionalConnectome::open(&folder).unwrap();
    let mut resolved = resolve_connectome(
        &ConnectomeRoots::from_primary(root),
        folder.to_str().unwrap(),
    )
    .unwrap();
    resolved.space = SPACE.to_owned();
    let mut seed = NiiVolume::zeros(conn.grid.dims, conn.grid.affine);
    seed.data[0] = 1.0;
    let dir = stim_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    write_nii(
        &dir.join("sub-01_sim-binary_model-kuncel_hemi-R.nii"),
        &seed,
    )
    .unwrap();
    (folder, conn, resolved)
}

fn functional_request(root: &Path, conn: &ResolvedConnectome) -> FunctionalRequest {
    FunctionalRequest {
        derivatives_root: root.to_path_buf(),
        subject_id: "01".into(),
        label: LABEL.into(),
        space: SPACE.into(),
        connectome: conn.clone(),
        kind: SeedKind::Binary,
        model: None,
        seed_file: None,
        per_subject_maps: true,
    }
}

#[test]
fn functional_job_writes_maps_and_per_subject_corr_files() {
    let d = tempfile::tempdir().unwrap();
    let (_, conn, resolved) = functional_setup(d.path());
    let cancel = AtomicBool::new(false);
    let out = run_functional_batch(
        &conn,
        &[functional_request(d.path(), &resolved)],
        &FunctionalPass::default(),
        &cancel,
        &no_progress,
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].status, Status::Ok);
    let dir = stim_dir(d.path());
    let base = "sub-01_sim-binary_model-kuncel";
    let n = names(&dir);
    for desc in ["AvgR", "VarR", "AvgRFz", "T"] {
        let f = format!("{base}_seed-fMRI_conn-SYNxFullSet_desc-{desc}_funcmap.nii");
        assert!(n.contains(&f), "{f} in {n:?}");
        assert!(n.contains(&f.replace(".nii", ".json")));
    }
    for id in ["0001", "0002"] {
        assert!(n.contains(&format!(
            "{base}_seed-fMRI_conn-SYNxFullSet_id-{id}_corr.nii"
        )));
    }
    assert!(n.contains(&format!("{base}_seed-fMRI.nii")));
    assert!(n.contains(&format!("{base}_seed-fMRI_hemi-L.nii")));
    let avg = read_nii(&dir.join(format!(
        "{base}_seed-fMRI_conn-SYNxFullSet_desc-AvgR_funcmap.nii"
    )))
    .unwrap();
    assert!((avg.data[0] - 1.0).abs() < 1e-5);
    assert!(!n.iter().any(|f| f.starts_with(".partial-")));
}

#[test]
fn functional_job_with_a_subset_changes_the_label_and_cancel_commits_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (_, conn, mut resolved) = functional_setup(d.path());
    resolved.subset = Some("Patients".to_owned());
    let cancel = AtomicBool::new(false);
    let progress = |p: &Progress| {
        if p.stage == "functional" {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    let before = names(&stim_dir(d.path()));
    let r = run_functional_batch(
        &conn,
        &[functional_request(d.path(), &resolved)],
        &FunctionalPass::default(),
        &cancel,
        &progress,
    );
    assert_eq!(r.unwrap_err(), MapError::Cancelled);
    assert_eq!(names(&stim_dir(d.path())), before);
    cancel.store(false, Ordering::Relaxed);
    run_functional_batch(
        &conn,
        &[functional_request(d.path(), &resolved)],
        &FunctionalPass::default(),
        &cancel,
        &no_progress,
    )
    .unwrap();
    assert!(names(&stim_dir(d.path()))
        .iter()
        .any(|n| n.contains("conn-SYNxPatients_desc-T_funcmap.nii")));
}

#[test]
fn a_missing_connectome_is_e_conn_missing() {
    let d = tempfile::tempdir().unwrap();
    let e = resolve_connectome(&ConnectomeRoots::from_primary(d.path()), "Nope").unwrap_err();
    assert_eq!(e.code(), "E_CONN_MISSING");
    let e = resolve_connectome(&ConnectomeRoots::from_primary(d.path()), " > x").unwrap_err();
    assert_eq!(e.code(), "E_CONN_USAGE");
}

fn opts(setting: crate::connectome_cache::CacheSetting) -> LoadOptions<'static> {
    LoadOptions {
        setting,
        cancel: None,
        progress: None,
    }
}

fn read_bytes(dir: &Path, suffix: &str) -> Vec<u8> {
    let name = names(dir)
        .into_iter()
        .find(|n| n.ends_with(suffix))
        .unwrap();
    std::fs::read(dir.join(name)).unwrap()
}

#[test]
fn cached_and_plain_structural_runs_write_identical_outputs() {
    use crate::connectome_cache::CacheSetting;
    let d = tempfile::tempdir().unwrap();
    let conn = structural_connectome(&d.path().join("connectomes"));
    let (plain_root, cached_root) = (d.path().join("plain"), d.path().join("cached"));
    for root in [&plain_root, &cached_root] {
        write_vta(root, "R", [0.0, 0.0, 0.0]);
    }
    let cancel = AtomicBool::new(false);
    crate::connectome_cache::with_cache_dir(&d.path().join("cache"), || {
        let plain = StructuralEngine::load_with(&conn, &opts(CacheSetting::Never)).unwrap();
        assert!(!plain.uses_cache());
        run_structural(&plain, &request(&plain_root, &conn), &cancel, &no_progress).unwrap();
        let asked = StructuralEngine::load_with(&conn, &opts(CacheSetting::Ask)).unwrap();
        assert!(!asked.uses_cache());
        assert_eq!(asked.notes.len(), 1);
        let cached = StructuralEngine::load_with(&conn, &opts(CacheSetting::Always)).unwrap();
        assert!(cached.uses_cache());
        run_structural(
            &cached,
            &request(&cached_root, &conn),
            &cancel,
            &no_progress,
        )
        .unwrap();
        let again = StructuralEngine::load_with(&conn, &opts(CacheSetting::Ask)).unwrap();
        assert!(again.uses_cache());
        assert!(again.notes.is_empty());
    });
    let (a, b) = (stim_dir(&plain_root), stim_dir(&cached_root));
    for suffix in ["_strucmap.nii", "_fibers.mat", "_seed-dMRI.nii"] {
        assert_eq!(read_bytes(&a, suffix), read_bytes(&b, suffix), "{suffix}");
    }
}

#[test]
fn fibre_values_agree_between_the_set_the_index_and_the_cache() {
    use crate::connectome_cache::CacheSetting;
    let d = tempfile::tempdir().unwrap();
    let conn = structural_connectome(&d.path().join("connectomes"));
    let mut e = NiiVolume::zeros([6, 5, 5], diagonal_affine([1.0; 3], [-3.0, -2.0, -2.0]));
    for (x, v) in [(0usize, 300.0f32), (1, 500.0), (2, 100.0)] {
        let n = e.index(x + 2, 2, 2);
        e.data[n] = v;
    }
    let set = fibres();
    let cancel = AtomicBool::new(false);
    let want = fibre_values(&set, &e, 200.0, &cancel).unwrap();
    assert!(!want.fiber_index.is_empty());
    crate::connectome_cache::with_cache_dir(&d.path().join("cache"), || {
        for setting in [CacheSetting::Never, CacheSetting::Always] {
            let engine = StructuralEngine::load_with(&conn, &opts(setting)).unwrap();
            assert_eq!(engine.uses_cache(), setting == CacheSetting::Always);
            let got = fibre_values_from(&engine, &e, 200.0, &cancel).unwrap();
            assert_eq!(got, want);
        }
    });
}

#[test]
fn a_cancelled_load_returns_cancelled() {
    use crate::connectome_cache::CacheSetting;
    let d = tempfile::tempdir().unwrap();
    let conn = structural_connectome(&d.path().join("connectomes"));
    let cancel = AtomicBool::new(true);
    crate::connectome_cache::with_cache_dir(&d.path().join("cache"), || {
        for setting in [CacheSetting::Never, CacheSetting::Always] {
            let r = StructuralEngine::load_with(
                &conn,
                &LoadOptions {
                    setting,
                    cancel: Some(&cancel),
                    progress: None,
                },
            );
            assert_eq!(r.err(), Some(MapError::Cancelled));
        }
    });
}
