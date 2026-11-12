#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use dbsloc_connectome::write_data_mat;

use super::*;

fn random_set(n: usize, seed: u64) -> FibreSet {
    let mut state = seed;
    let mut rnd = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) as f32 / (1u64 << 31) as f32) * 80.0 - 40.0
    };
    let fibres: Vec<Vec<[f32; 3]>> = (0..n)
        .map(|f| {
            let o = [rnd(), rnd(), rnd()];
            (0..(5 + f % 7))
                .map(|i| {
                    [
                        o[0] + i as f32 * 0.8,
                        o[1] - i as f32 * 0.5,
                        o[2] + i as f32 * 0.3,
                    ]
                })
                .collect()
        })
        .collect();
    FibreSet::from_fibres(&fibres).unwrap()
}

fn target(root: &Path, set: &FibreSet) -> CacheTarget {
    let folder = root.join("dMRI").join("Synth");
    std::fs::create_dir_all(&folder).unwrap();
    write_data_mat(&folder.join("data.mat"), set).unwrap();
    CacheTarget {
        class: ConnectomeClass::Dmri,
        name: "Synth".into(),
        id: "synth".into(),
        folder,
    }
}

fn est(bytes: u64, free: Option<u64>) -> Estimate {
    Estimate {
        points: 0,
        estimated_bytes: bytes,
        free_bytes: free,
    }
}

#[test]
fn the_policy_follows_the_consent_rules() {
    let e = est(GB, Some(100 * GB));
    assert_eq!(
        decide(CacheSetting::Never, Some(Choice::Allow), true, &e, "n", "i"),
        Decision::Plain {
            note: None,
            warning: None
        }
    );
    assert_eq!(
        decide(CacheSetting::Ask, None, true, &e, "n", "i"),
        Decision::UseCache
    );
    match decide(CacheSetting::Ask, None, false, &e, "N", "i") {
        Decision::Plain {
            note: Some(n),
            warning: None,
        } => {
            assert!(n.contains("connectome cache for N not built (about 1.0 GB)"));
            assert!(n.contains("dbsloc connectome cache build i"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        decide(
            CacheSetting::Ask,
            Some(Choice::Decline),
            false,
            &e,
            "n",
            "i"
        ),
        Decision::Plain {
            note: None,
            warning: None
        }
    );
    assert_eq!(
        decide(CacheSetting::Ask, Some(Choice::Allow), false, &e, "n", "i"),
        Decision::Build
    );
    assert_eq!(
        decide(CacheSetting::Always, None, false, &e, "n", "i"),
        Decision::Build
    );
    let tight = est(GB, Some(6 * GB));
    match decide(CacheSetting::Always, None, false, &tight, "n", "i") {
        Decision::Plain {
            warning: Some(w), ..
        } => assert!(w.contains("not built")),
        other => panic!("{other:?}"),
    }
    assert!(est(GB, Some(7 * GB)).rule_ok());
    assert!(!est(GB, Some(7 * GB - 1)).rule_ok());
    assert!(est(GB, Some(6 * GB)).dialog_ok());
    assert!(!est(GB, Some(6 * GB - 1)).dialog_ok());
    assert_eq!(CacheSetting::parse("ASK"), Some(CacheSetting::Ask));
    assert_eq!(CacheSetting::parse("x"), None);
}

#[test]
fn the_whole_cache_equals_the_source_and_regions_equal_a_brute_force_scan() {
    let d = tempfile::tempdir().unwrap();
    let set = random_set(600, 7);
    let t = target(&d.path().join("c"), &set);
    with_cache_dir(&d.path().join("cache"), || {
        assert!(!is_valid(&t));
        let (report, built) = build(&t, None, None).unwrap();
        assert_eq!(report.fibres, 600);
        assert_eq!(built, set);
        assert!(is_valid(&t));
        let h = CacheHandle::open(&t).unwrap().unwrap();
        let (whole, index) = h.load_all(None).unwrap();
        assert_eq!(whole, set);
        let direct = FibreIndex::build(&set, 4.0).unwrap();
        for c in [[0, 0, 0], [-3, 2, 1], [5, 5, -4], [1, -1, 0]] {
            assert_eq!(index.fibres_in_cell(c), direct.fibres_in_cell(c));
        }
        for (lo, hi) in [
            ([-5.0, -5.0, -5.0], [6.0, 7.0, 4.0]),
            ([10.0, -20.0, 0.0], [10.0, -20.0, 0.0]),
            ([-100.0, -100.0, -100.0], [100.0, 100.0, 100.0]),
            ([200.0, 0.0, 0.0], [210.0, 5.0, 5.0]),
        ] {
            let (sub, ids) = h.region(lo, hi).unwrap();
            let cells = |p: &[f32; 3]| p.map(|v| (f64::from(v) / 4.0).floor() as i64);
            let cl = lo.map(|v| (v / 4.0).floor() as i64);
            let ch = hi.map(|v| (v / 4.0).floor() as i64);
            let want: Vec<u32> = (0..set.n_fibres())
                .filter(|f| {
                    set.fibre(*f).iter().any(|p| {
                        let c = cells(p);
                        (0..3).all(|a| c[a] >= cl[a] && c[a] <= ch[a])
                    })
                })
                .map(|f| f as u32)
                .collect();
            assert_eq!(ids, want);
            assert_eq!(sub.n_fibres(), want.len());
            for (i, f) in want.iter().enumerate() {
                assert_eq!(sub.fibre(i), set.fibre(*f as usize));
            }
        }
    });
}

#[test]
fn a_changed_source_invalidates_and_a_rebuild_replaces_the_cache() {
    let d = tempfile::tempdir().unwrap();
    let t = target(&d.path().join("c"), &random_set(50, 1));
    with_cache_dir(&d.path().join("cache"), || {
        build(&t, None, None).unwrap();
        assert!(is_valid(&t));
        let other = random_set(80, 2);
        write_data_mat(&t.folder.join("data.mat"), &other).unwrap();
        assert!(!is_valid(&t));
        assert!(CacheHandle::open(&t).unwrap().is_none());
        let (_, set) = build(&t, None, None).unwrap();
        assert_eq!(set, other);
        assert!(is_valid(&t));
    });
}

#[test]
fn a_cancelled_build_leaves_no_cache_and_no_partial_folder() {
    let d = tempfile::tempdir().unwrap();
    let t = target(&d.path().join("c"), &random_set(50, 1));
    with_cache_dir(&d.path().join("cache"), || {
        let cancel = AtomicBool::new(true);
        assert_eq!(
            build(&t, Some(&cancel), None).unwrap_err(),
            MapError::Cancelled
        );
        assert!(!is_valid(&t));
        let keydir = connectomes_cache_root().unwrap().join(t.key());
        let left: Vec<_> = std::fs::read_dir(&keydir).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
    });
}

#[test]
fn choices_listing_and_deleting_round_trip() {
    let d = tempfile::tempdir().unwrap();
    let t = target(&d.path().join("c"), &random_set(50, 3));
    with_cache_dir(&d.path().join("cache"), || {
        let key = t.key();
        assert!(choice(&key).is_none());
        set_choice(&key, "Synth", Choice::Decline, 123).unwrap();
        assert_eq!(choice(&key).unwrap().choice, Choice::Decline);
        let dec = declined();
        assert_eq!(dec.len(), 1);
        assert_eq!(dec[0].estimated_bytes, 123);
        reset_choice(&key).unwrap();
        assert!(declined().is_empty());

        build_and_allow(&t, None, None).unwrap();
        assert_eq!(choice(&key).unwrap().choice, Choice::Allow);
        let listed = list_caches();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Synth");
        assert!(listed[0].size_bytes > 0);
        assert!(listed[0].last_used.is_some());
        assert!(!listed[0].orphaned);

        let held = CacheHandle::open(&t).unwrap().unwrap();
        assert_eq!(
            delete_caches(std::slice::from_ref(&key)),
            vec![DeleteResult::InUse { key: key.clone() }]
        );
        assert!(is_valid(&t));
        assert!(choice(&key).is_some());
        drop(held);

        std::fs::remove_dir_all(&t.folder).unwrap();
        assert!(list_caches()[0].orphaned);
        let r = delete_caches(&[key.clone(), "../x".to_owned()]);
        assert!(matches!(r[0], DeleteResult::Deleted { .. }));
        assert!(matches!(r[1], DeleteResult::Failed { .. }));
        assert!(list_caches().is_empty());
        assert!(choice(&key).is_none());
    });
}

#[test]
fn prepare_asks_builds_and_reuses() {
    let d = tempfile::tempdir().unwrap();
    let t = target(&d.path().join("c"), &random_set(50, 4));
    with_cache_dir(&d.path().join("cache"), || {
        let (mut notes, mut warns) = (Vec::new(), Vec::new());
        let p = prepare(&t, CacheSetting::Ask, None, None, &mut notes, &mut warns).unwrap();
        assert!(matches!(p, Prepared::Plain));
        assert_eq!(notes.len(), 1);
        assert!(!is_valid(&t));
        let p = prepare(&t, CacheSetting::Always, None, None, &mut notes, &mut warns).unwrap();
        assert!(matches!(p, Prepared::Built(..)));
        let p = prepare(&t, CacheSetting::Ask, None, None, &mut notes, &mut warns).unwrap();
        assert!(matches!(p, Prepared::Cached(_)));
        let p = prepare(&t, CacheSetting::Never, None, None, &mut notes, &mut warns).unwrap();
        assert!(matches!(p, Prepared::Plain));
        assert_eq!(notes.len(), 1);
        assert!(warns.is_empty());
    });
}

#[test]
fn a_cancelled_build_and_allow_records_no_choice() {
    let d = tempfile::tempdir().unwrap();
    let t = target(&d.path().join("c"), &random_set(50, 5));
    with_cache_dir(&d.path().join("cache"), || {
        let cancel = AtomicBool::new(true);
        assert_eq!(
            build_and_allow(&t, Some(&cancel), None).unwrap_err(),
            MapError::Cancelled
        );
        assert!(choice(&t.key()).is_none());
    });
}

#[test]
fn deleting_skips_a_build_in_progress_and_keeps_its_partial_folder_and_choice() {
    let d = tempfile::tempdir().unwrap();
    let t = target(&d.path().join("c"), &random_set(50, 6));
    with_cache_dir(&d.path().join("cache"), || {
        let key = t.key();
        build(&t, None, None).unwrap();
        set_choice(&key, "Synth", Choice::Allow, 1).unwrap();
        let partial = connectomes_cache_root()
            .unwrap()
            .join(&key)
            .join(".partial-live");
        std::fs::create_dir_all(&partial).unwrap();
        let guard = BuildingGuard::new(&key);
        assert_eq!(
            delete_caches(std::slice::from_ref(&key)),
            vec![DeleteResult::InUse { key: key.clone() }]
        );
        assert!(partial.is_dir());
        assert!(choice(&key).is_some());
        assert!(is_valid(&t));
        drop(guard);
        assert!(matches!(
            delete_caches(std::slice::from_ref(&key))[0],
            DeleteResult::Deleted { .. }
        ));
        assert!(partial.is_dir());
        assert!(choice(&key).is_none());
    });
}

#[test]
fn a_cache_with_a_broken_structure_is_invalid_and_never_panics() {
    let d = tempfile::tempdir().unwrap();
    let t = target(&d.path().join("c"), &random_set(50, 8));
    with_cache_dir(&d.path().join("cache"), || {
        build(&t, None, None).unwrap();
        assert!(is_valid(&t));
        let v1 = version_dir(&t.key()).unwrap();
        let cells = v1.join("cells.u64");
        let good = std::fs::read(&cells).unwrap();
        let mut bad = good.clone();
        bad[0] = 1;
        std::fs::write(&cells, &bad).unwrap();
        assert!(!is_valid(&t));
        assert!(CacheHandle::open(&t).unwrap().is_none());
        std::fs::write(&cells, &good).unwrap();
        assert!(is_valid(&t));

        let offsets = v1.join("offsets.u64");
        let good = std::fs::read(&offsets).unwrap();
        let mut bad = good.clone();
        let last = bad.len() - 8;
        bad[last] ^= 1;
        std::fs::write(&offsets, &bad).unwrap();
        assert!(!is_valid(&t));
        std::fs::write(&offsets, &good).unwrap();

        let order = v1.join("order.u32");
        let mut bad = std::fs::read(&order).unwrap();
        let first: [u8; 4] = bad[4..8].try_into().unwrap();
        bad[0..4].copy_from_slice(&first);
        std::fs::write(&order, &bad).unwrap();
        let h = CacheHandle::open(&t).unwrap().unwrap();
        assert!(matches!(h.load_all(None), Err(MapError::Format(_))));
    });
}

#[test]
fn an_empty_cache_answers_a_region_with_an_empty_set() {
    let d = tempfile::tempdir().unwrap();
    let t = target(&d.path().join("c"), &FibreSet::from_fibres(&[]).unwrap());
    with_cache_dir(&d.path().join("cache"), || {
        build(&t, None, None).unwrap();
        let h = CacheHandle::open(&t).unwrap().unwrap();
        let (sub, ids) = h.region([-5.0; 3], [5.0; 3]).unwrap();
        assert_eq!(sub.n_fibres(), 0);
        assert!(ids.is_empty());
    });
}

#[test]
fn concurrent_choice_updates_all_survive_and_a_corrupt_file_is_kept() {
    let d = tempfile::tempdir().unwrap();
    with_cache_dir(&d.path().join("cache"), || {
        std::thread::scope(|s| {
            for i in 0..8 {
                s.spawn(move || {
                    set_choice(&format!("k{i}"), "n", Choice::Decline, i).unwrap();
                });
            }
        });
        assert_eq!(declined().len(), 8);
        let path = choices_path().unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            set_choice("x", "n", Choice::Allow, 1),
            Err(MapError::Format(_))
        ));
        assert!(path.with_extension("json.bak").is_file());
        set_choice("x", "n", Choice::Allow, 1).unwrap();
        assert_eq!(declined().len(), 0);
        assert!(choice("x").is_some());
    });
}

#[test]
#[ignore = "timing run on a real connectome; set DBSLOC_BENCH_CONNECTOME to a dMRI folder"]
fn real_connectome_timing() {
    let Some(folder) = std::env::var_os("DBSLOC_BENCH_CONNECTOME").map(PathBuf::from) else {
        return;
    };
    let cache = std::env::var_os("DBSLOC_BENCH_CACHE")
        .map(PathBuf::from)
        .unwrap();
    let t = CacheTarget {
        class: ConnectomeClass::Dmri,
        name: folder_name(&folder),
        id: "bench".into(),
        folder,
    };
    let src = t.source_file().unwrap();
    let lap = |label: &str, started: Instant| {
        eprintln!("BENCH {label}: {:.3} s", started.elapsed().as_secs_f64());
    };
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let s = Instant::now();
    let seq = one.install(|| {
        let set = dbsloc_connectome::read_fibres(&src).unwrap();
        lap("sequential read", s);
        let i = Instant::now();
        let index = FibreIndex::build(&set, 4.0).unwrap();
        lap("sequential index", i);
        drop(index);
        set
    });
    lap("sequential total", s);
    let s = Instant::now();
    let set = dbsloc_connectome::read_fibres(&src).unwrap();
    lap("parallel read", s);
    let i = Instant::now();
    let index = FibreIndex::build(&set, 4.0).unwrap();
    lap("parallel index", i);
    lap("parallel total", s);
    assert_eq!(set, seq);
    drop((index, seq));
    with_cache_dir(&cache, || {
        let s = Instant::now();
        let (report, built) = build(&t, None, None).unwrap();
        lap("cache build (incl. read)", s);
        eprintln!(
            "BENCH cache: {:.2} GB, P {}, K {}",
            report.bytes as f64 / 1e9,
            report.points,
            report.fibres
        );
        assert_eq!(built, set);
        drop(built);
        let s = Instant::now();
        let h = CacheHandle::open(&t).unwrap().unwrap();
        lap("cache open", s);
        let s = Instant::now();
        let (whole, idx) = h.load_all(None).unwrap();
        lap("cache load_all (set + index)", s);
        assert_eq!(whole, set);
        drop((whole, idx));
        for pass in ["cold-ish", "warm"] {
            let s = Instant::now();
            let (sub, ids) = h.region([6.0, -20.0, -14.0], [18.0, -8.0, -2.0]).unwrap();
            lap(&format!("region 12 mm box {pass}"), s);
            eprintln!(
                "BENCH region fibres {} of {} ({:.2} %)",
                ids.len(),
                set.n_fibres(),
                100.0 * ids.len() as f64 / set.n_fibres() as f64
            );
            drop(sub);
        }
    });
}
