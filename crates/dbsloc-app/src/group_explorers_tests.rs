#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;

fn record() -> GroupRecord {
    GroupRecord::new("20261025000000", "Cohort")
}

fn entries() -> Vec<ConnectomeEntry> {
    vec![
        ConnectomeEntry {
            spec: "gsp".to_owned(),
            label: "GSP (functional)".to_owned(),
            class: ConnectomeClass::Fmri,
        },
        ConnectomeEntry {
            spec: "ppmi".to_owned(),
            label: "PPMI (structural)".to_owned(),
            class: ConnectomeClass::Dmri,
        },
        ConnectomeEntry {
            spec: "tracts".to_owned(),
            label: "Tracts (structural)".to_owned(),
            class: ConnectomeClass::DmriMultiTract,
        },
    ]
}

fn gate<'a>(
    variables: &'a [String],
    computed: usize,
    connectomes: Option<&'a [ConnectomeEntry]>,
) -> Gate<'a> {
    Gate {
        variables,
        included: computed.max(3),
        computed,
        busy: false,
        connectomes,
    }
}

#[test]
fn the_default_sweet_spot_options_equal_the_command_line_defaults() {
    let state = ExplorerState::default();
    let o = sweetspot_options(&state, Path::new("g"), "UPDRS");
    let expected = SweetspotSettings {
        cv: CvStrategy::LeaveOnePatientOut,
        ..SweetspotSettings::default()
    };
    assert_eq!(o.settings, expected);
    assert_eq!(o.settings.k_folds, 5);
    assert_eq!(o.settings.permutations, 1000);
    assert_eq!(
        o.settings.seed,
        dbsloc_pipeline::group::sweetspot::DEFAULT_SEED
    );
    assert_eq!(o.space, REFERENCE_SPACE);
    assert_eq!(o.variable, "UPDRS");
    assert!(o.covariates.is_empty() && o.model.is_none() && o.patients.is_none());
}

#[test]
fn the_default_fibre_filtering_options_equal_the_command_line_defaults() {
    let state = ExplorerState::default();
    let o = fibre_options(&state, Path::new("g"), "UPDRS", "ppmi", None);
    assert_eq!(o.settings, FibreSettings::default());
    assert_eq!(display_percent(&o.settings), 25.0);
    assert_eq!(o.scheme, CvScheme::LeaveOneOut);
    assert_eq!(o.seed, 1);
    assert!(!o.mirror);
    assert_eq!(o.connectome, "ppmi");
    assert_eq!(o.space, REFERENCE_SPACE);
}

#[test]
fn the_default_network_mapping_options_equal_the_command_line_defaults() {
    let state = ExplorerState::default();
    let o = netmap_options(
        &state,
        Path::new("g"),
        "UPDRS",
        "gsp",
        Some(Path::new("c")),
        None,
    );
    assert_eq!(o.settings, NetmapSettings::default());
    assert_eq!(o.mask, MaskChoice::Grey);
    assert_eq!(o.scheme, CvScheme::LeaveOneOut);
    assert_eq!(o.seed, 1);
    assert_eq!(o.connectome_root.as_deref(), Some(Path::new("c")));
    assert!(!o.mirror);
}

#[test]
fn the_cross_validation_choice_maps_to_both_schemes() {
    let mut cv = CvChoice {
        kind: CvKind::KFold,
        k: 7,
        iterations: 3,
        ..CvChoice::default()
    };
    assert_eq!(
        cv.scheme(),
        CvScheme::KFold {
            k: 7,
            iterations: 3
        }
    );
    assert_eq!(cv.strategy(), CvStrategy::KFold);
    cv.kind = CvKind::LeaveNothingOut;
    cv.permutations = 250;
    assert_eq!(cv.scheme(), CvScheme::LeaveNothingOut { permutations: 250 });
    assert_eq!(cv.strategy(), CvStrategy::LeaveNothingOut);
    cv.kind = CvKind::LeaveOneCohortOut;
    assert_eq!(cv.scheme(), CvScheme::LeaveOneCohortOut);
    assert_eq!(cv.strategy(), CvStrategy::LeaveOneCohortOut);
    let mut state = ExplorerState::default();
    state.cv.kind = CvKind::KFold;
    state.cv.k = 9;
    let s = sweetspot_settings(&state);
    assert_eq!((s.cv, s.k_folds), (CvStrategy::KFold, 9));
}

#[test]
fn the_edited_settings_reach_the_options() {
    let mut state = ExplorerState {
        covariates: vec!["Age".to_owned()],
        ..ExplorerState::default()
    };
    state.fibre.test = FibreTest::Pearson;
    set_display_percent(&mut state.fibre, 40.0);
    state.fibre_mirror = true;
    state.netmap.map = MapKind::AMap;
    state.mask = MaskPick::Brain;
    let f = fibre_options(&state, Path::new("g"), "v", "c", None);
    assert_eq!(f.settings.test, FibreTest::Pearson);
    assert_eq!(display_percent(&f.settings), 40.0);
    assert!(f.mirror);
    assert_eq!(f.covariates, vec!["Age".to_owned()]);
    let n = netmap_options(&state, Path::new("g"), "v", "c", None, Some(Path::new("a")));
    assert_eq!(n.settings.map, MapKind::AMap);
    assert_eq!(n.mask, MaskChoice::Brain);
    assert_eq!(n.atlases_dir.as_deref(), Some(Path::new("a")));
}

#[test]
fn a_preset_sets_the_test_and_is_recognised_until_it_is_edited() {
    let mut s = SweetspotSettings::default();
    assert_eq!(current_preset(&s), Some(Preset::Eisenstein2014));
    s.apply_preset(Preset::Dembek2019);
    assert_eq!(current_preset(&s), Some(Preset::Dembek2019));
    assert_eq!(s.test, Test::Wilcoxon);
    s.n_threshold_percent = 10.0;
    assert_eq!(current_preset(&s), None);
    s.apply_preset(Preset::Reich2019);
    s.level = Level::EFields;
    assert_eq!(current_preset(&s), None);
}

#[test]
fn the_tests_offered_follow_the_level() {
    let vta = sweet_tests(Level::Vtas);
    assert!(vta.contains(&Test::TTest) && !vta.contains(&Test::Correlations));
    assert_eq!(
        sweet_tests(Level::EFields),
        vec![Test::Correlations, Test::ReverseTTests]
    );
}

#[test]
fn a_result_row_is_read_from_each_record_kind() {
    let sweet = json!({
        "outcome": {"variable": "UPDRS"},
        "cross_validation": {"n": 12, "r": 0.6, "p": 0.0123, "auc": null},
    });
    let row = parse_row(
        Kind::SweetSpot,
        "a",
        PathBuf::from("f"),
        Some(&sweet),
        Some(0),
    );
    assert_eq!(row.variable, "UPDRS");
    assert_eq!(
        (row.n, row.r, row.p, row.auc),
        (Some(12), Some(0.6), Some(0.0123), None)
    );
    assert_eq!(row.created, "1970-01-01T00:00:00");
    let fibre = json!({
        "created_at": "2026-10-25T10:11:12Z",
        "provenance": {"variable": "BDI"},
        "cross_validation": {"n": 9, "r": -0.2, "p": 0.5, "auc": 0.7},
    });
    let row = parse_row(
        Kind::FibreFiltering,
        "b",
        PathBuf::from("f"),
        Some(&fibre),
        None,
    );
    assert_eq!(row.variable, "BDI");
    assert_eq!(row.created, "2026-10-25T10:11:12Z");
    assert_eq!(row.auc, Some(0.7));
    let net = json!({
        "provenance": {"variable": "YBOCS"},
        "results": {"n": 20, "r": 0.4, "p": 0.0004, "auc": null},
    });
    let row = parse_row(
        Kind::NetworkMapping,
        "c",
        PathBuf::from("f"),
        Some(&net),
        None,
    );
    assert_eq!((row.variable.as_str(), row.n), ("YBOCS", Some(20)));
    let text = row_text(&row);
    assert!(text.contains("YBOCS") && text.contains("n 20") && text.contains("p <0.001"));
    assert!(!text.contains("AUC"));
    let missing = parse_row(Kind::NetworkMapping, "d", PathBuf::from("f"), None, None);
    assert_eq!(
        (missing.n, missing.r, missing.variable.as_str()),
        (None, None, "")
    );
    assert!(row_text(&missing).contains("r -"));
}

#[test]
fn the_results_list_reads_the_records_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut rec = record();
    for (kind, id, at) in [
        (
            Kind::FibreFiltering,
            "20261025000001",
            "2026-10-25T00:00:01Z",
        ),
        (
            Kind::FibreFiltering,
            "20261025000009",
            "2026-10-25T00:00:09Z",
        ),
        (Kind::SweetSpot, "20261025000005", "2026-10-25T00:00:05Z"),
    ] {
        kind.register(&mut rec, id);
        let folder = dir.path().join(kind.folder()).join(id);
        std::fs::create_dir_all(&folder).unwrap();
        let body = json!({
            "created_at": at,
            "provenance": {"variable": "v"},
            "outcome": {"variable": "v"},
            "cross_validation": {"n": 4, "r": 0.1, "p": 0.2},
        });
        std::fs::write(folder.join(kind.json_name()), body.to_string()).unwrap();
    }
    rec.analyses.push(AnalysisRef {
        kind: "atlasstats".to_owned(),
        id: "x".to_owned(),
        record: "x.json".to_owned(),
    });
    assert_eq!(rows_key(&rec.analyses).len(), 3);
    let rows = load_rows(dir.path(), &rec.analyses);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["20261025000009", "20261025000005", "20261025000001"]
    );
    assert_eq!(
        rows[0].folder,
        dir.path().join("fiberfiltering").join("20261025000009")
    );
    assert_eq!(rows[0].n, Some(4));
}

#[test]
fn a_run_is_registered_under_the_folder_names_of_the_record() {
    let mut rec = record();
    Kind::SweetSpot.register(&mut rec, "s1");
    Kind::FibreFiltering.register(&mut rec, "f1");
    Kind::NetworkMapping.register(&mut rec, "n1");
    let got: Vec<(&str, &str, &str)> = rec
        .analyses
        .iter()
        .map(|a| (a.kind.as_str(), a.id.as_str(), a.record.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("sweetspot", "s1", "sweetspots/s1/sweetspot.json"),
            ("fiberfiltering", "f1", "fiberfiltering/f1/fibrefilter.json"),
            ("networkmapping", "n1", "networkmapping/n1/netmap.json"),
        ]
    );
    for k in Kind::ALL {
        assert_eq!(Kind::from_record_kind(k.record_kind()), Some(k));
    }
    assert_eq!(Kind::from_record_kind("atlasstats"), None);
}

#[test]
fn a_map_is_stale_when_the_seed_kind_the_grid_or_the_connectome_differ() {
    let side = json!({
        "connectome": {"digest": "d1"},
        "seed": {"kind": "binary", "grid": "2mm"},
    });
    assert!(!map_is_stale(&side, "d1", "binary", Some("2mm")));
    assert!(map_is_stale(&side, "d2", "binary", Some("2mm")));
    assert!(map_is_stale(&side, "d1", "efield", Some("2mm")));
    assert!(map_is_stale(&side, "d1", "binary", Some("1mm")));
    assert!(!map_is_stale(&side, "d1", "binary", None));
    assert!(!map_is_stale(&json!({}), "d1", "binary", Some("2mm")));
}

#[test]
fn a_map_is_recomputed_when_its_inputs_changed_but_kept_without_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let vta = dir.path().join("vta.nii");
    std::fs::write(&vta, b"one").unwrap();
    let sha = file_sha256_hex(&vta).unwrap();
    let seed = dir.path().join("sub-01_seed-dMRI.nii");
    let map = dir.path().join("sub-01_conn-x_strucmap.nii");
    std::fs::write(&map, b"m").unwrap();
    assert!(!map_needs_run(&map, "d", "binary", Some("2mm")));
    std::fs::write(
        dir.path().join("sub-01_seed-dMRI.json"),
        json!({"inputs": [{"file": vta.display().to_string(), "sha256": sha}]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("sub-01_conn-x_strucmap.json"),
        json!({
            "connectome": {"digest": "d"},
            "seed": {"kind": "binary", "grid": "2mm", "file": seed.display().to_string()},
            "inputs": [],
        })
        .to_string(),
    )
    .unwrap();
    assert!(!map_needs_run(&map, "d", "binary", Some("2mm")));
    std::fs::write(&vta, b"two").unwrap();
    assert!(map_needs_run(&map, "d", "binary", Some("2mm")));
    assert!(map_needs_run(&map, "other", "binary", Some("2mm")));
    std::fs::write(&vta, b"one").unwrap();
    assert!(!map_needs_run(&map, "d", "binary", Some("2mm")));
    std::fs::remove_file(&vta).unwrap();
    assert!(map_needs_run(&map, "d", "binary", Some("2mm")));
}

#[test]
fn run_is_blocked_with_the_reason_the_owner_can_act_on() {
    let none: Vec<String> = Vec::new();
    let vars = vec!["UPDRS".to_owned()];
    let reason = |kind, var: Option<&str>, conn: Option<&str>, g: Gate<'_>| {
        run_block_reason(kind, var, conn, &g)
    };
    assert!(reason(Kind::SweetSpot, None, None, gate(&none, 5, None))
        .unwrap()
        .contains("clinical variables"));
    assert_eq!(
        reason(Kind::SweetSpot, None, None, gate(&vars, 5, None)),
        Some("Choose the outcome variable")
    );
    assert!(
        reason(Kind::SweetSpot, Some("UPDRS"), None, gate(&vars, 2, None))
            .unwrap()
            .contains("computed VTAs")
    );
    assert_eq!(
        reason(Kind::SweetSpot, Some("UPDRS"), None, gate(&vars, 5, None)),
        None
    );
    let mut busy = gate(&vars, 5, None);
    busy.busy = true;
    assert!(reason(Kind::SweetSpot, Some("UPDRS"), None, busy)
        .unwrap()
        .contains("running job"));
    let mut few = gate(&vars, 5, None);
    few.included = 2;
    assert!(reason(Kind::SweetSpot, Some("UPDRS"), None, few)
        .unwrap()
        .contains("included patients"));
}

#[test]
fn run_needs_an_installed_connectome_of_the_right_kind() {
    let vars = vec!["UPDRS".to_owned()];
    let ents = entries();
    let r = |kind, conn: Option<&str>, list: Option<&[ConnectomeEntry]>| {
        run_block_reason(kind, Some("UPDRS"), conn, &gate(&vars, 5, list))
    };
    assert_eq!(
        r(Kind::FibreFiltering, None, None),
        Some("Looking for installed connectomes")
    );
    assert!(r(Kind::FibreFiltering, None, Some(&[]))
        .unwrap()
        .contains("installed"));
    assert_eq!(
        r(Kind::FibreFiltering, None, Some(&ents)),
        Some("Choose a connectome")
    );
    assert_eq!(
        r(Kind::FibreFiltering, Some("gsp"), Some(&ents)),
        Some("Choose a connectome")
    );
    assert_eq!(r(Kind::FibreFiltering, Some("ppmi"), Some(&ents)), None);
    assert_eq!(r(Kind::FibreFiltering, Some("tracts"), Some(&ents)), None);
    assert_eq!(r(Kind::NetworkMapping, Some("gsp"), Some(&ents)), None);
    assert_eq!(r(Kind::NetworkMapping, Some("ppmi"), Some(&ents)), None);
    assert_eq!(
        r(Kind::NetworkMapping, Some("tracts"), Some(&ents)),
        Some("Choose a connectome")
    );
    let only_functional = &ents[..1];
    assert!(r(Kind::FibreFiltering, None, Some(only_functional))
        .unwrap()
        .contains("installed"));
    assert_eq!(eligible(Kind::SweetSpot, &ents).len(), 0);
}

#[test]
fn the_state_keeps_a_valid_variable_and_drops_stale_covariates() {
    let mut state = ExplorerState::default();
    let vars = vec!["A".to_owned(), "B".to_owned(), "C".to_owned()];
    state.normalise(&vars);
    assert_eq!(state.variable.as_deref(), Some("A"));
    state.variable = Some("B".to_owned());
    state.covariates = vec!["B".to_owned(), "C".to_owned(), "Gone".to_owned()];
    state.normalise(&vars);
    assert_eq!(state.covariates, vec!["C".to_owned()]);
    state.normalise(&["Z".to_owned()]);
    assert_eq!(state.variable.as_deref(), Some("Z"));
    assert!(state.covariates.is_empty());
    state.normalise(&[]);
    assert_eq!(state.variable, None);
}

#[test]
fn map_files_are_picked_per_side_without_coverage_or_group_maps() {
    let names: Vec<String> = [
        "d_sweetspot-x_hemi-R_stat-ttest_desc-pos_map.nii.gz",
        "d_sweetspot-x_hemi-R_stat-ttest_desc-coverage_map.nii.gz",
        "d_sweetspot-x_group-1_hemi-R_stat-ttest_desc-pos_map.nii.gz",
        "d_sweetspot-x_hemi-L_stat-ttest_desc-pos_map.nii.gz",
        "model.json",
        "sweetspot.json",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let picked = pick_map_files(Kind::SweetSpot, &names);
    assert_eq!(picked.len(), 2);
    assert_eq!(picked[0].0, "R");
    assert!(picked[0]
        .1
        .ends_with("hemi-R_stat-ttest_desc-pos_map.nii.gz"));
    assert!(!picked[0].1.contains("coverage") && !picked[0].1.contains("group"));
    assert_eq!(picked[1].0, "L");
    let net = vec![
        "d_netmap-x_stat-rmap_map.nii.gz".to_owned(),
        "netmap.json".to_owned(),
    ];
    assert_eq!(
        pick_map_files(Kind::NetworkMapping, &net),
        vec![(String::new(), "d_netmap-x_stat-rmap_map.nii.gz".to_owned())]
    );
    assert!(pick_map_files(Kind::FibreFiltering, &net).is_empty());
}

#[test]
fn layer_ids_name_the_result_and_the_side() {
    assert_eq!(layer_ids(Kind::SweetSpot, "a"), vec!["ss:a:R", "ss:a:L"]);
    assert_eq!(
        layer_ids(Kind::FibreFiltering, "a"),
        vec!["ff:a:R", "ff:a:L"]
    );
    assert_eq!(layer_ids(Kind::NetworkMapping, "a"), vec!["nm:a"]);
}

#[test]
fn showing_a_result_without_files_explains_why() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("fiberfiltering").join("a")).unwrap();
    let e = load_layers(dir.path(), Kind::FibreFiltering, "a").unwrap_err();
    assert!(e.contains("no fibre"));
    std::fs::create_dir_all(dir.path().join("networkmapping").join("a")).unwrap();
    let e = load_layers(dir.path(), Kind::NetworkMapping, "a").unwrap_err();
    assert!(e.contains("no map file"));
    assert!(load_layers(dir.path(), Kind::SweetSpot, "a").is_err());
}

#[test]
fn deleting_a_result_removes_the_folder_and_the_record_entry() {
    let dir = tempfile::tempdir().unwrap();
    let mut rec = record();
    Kind::NetworkMapping.register(&mut rec, "n1");
    Kind::NetworkMapping.register(&mut rec, "n2");
    let folder = dir.path().join("networkmapping").join("n1");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("netmap.json"), "{}").unwrap();
    delete_analysis(dir.path(), &mut rec, Kind::NetworkMapping, "n1").unwrap();
    assert!(!folder.exists());
    assert_eq!(rec.analyses.len(), 1);
    assert_eq!(rec.analyses[0].id, "n2");
    assert!(delete_analysis(dir.path(), &mut rec, Kind::NetworkMapping, "..").is_err());
    assert!(delete_analysis(dir.path(), &mut rec, Kind::NetworkMapping, "").is_err());
    assert_eq!(rec.analyses.len(), 1);
}

#[test]
fn the_summary_line_reports_r_p_auc_and_n() {
    let line = summary_line(
        Kind::SweetSpot,
        "a",
        "leave-one-patient-out",
        0.5,
        0.0412,
        Some(0.8),
        11,
    );
    assert_eq!(
        line,
        "group: sweet spot a: leave-one-patient-out r = 0.500 (p = 0.041, AUC 0.80, n = 11)"
    );
    let line = summary_line(Kind::FibreFiltering, "b", "k-fold", -0.1, 0.0001, None, 5);
    assert!(line.contains("p = <0.001") && !line.contains("AUC"));
    assert!(line.starts_with("group: fibre filtering b"));
}

#[test]
fn the_last_connectome_choice_survives_a_state_round_trip() {
    let mut state = crate::app::DbslocAppState::default();
    state
        .group_connectomes
        .insert("fiberfiltering".to_owned(), "ppmi".to_owned());
    let text = serde_json::to_string(&state).unwrap();
    let back: crate::app::DbslocAppState = serde_json::from_str(&text).unwrap();
    assert_eq!(
        back.group_connectomes
            .get("fiberfiltering")
            .map(String::as_str),
        Some("ppmi")
    );
    let old: crate::app::DbslocAppState = serde_json::from_str(r#"{"bids_dir":"D:/x"}"#).unwrap();
    assert!(old.group_connectomes.is_empty());
}

#[test]
fn the_remembered_connectome_is_preferred_when_it_is_still_eligible() {
    let mut state = ExplorerState {
        kind: Kind::FibreFiltering,
        ..ExplorerState::default()
    };
    state.connectomes.load = ListLoad::Ready(vec![
        ConnectomeEntry {
            spec: "a".to_owned(),
            label: "A".to_owned(),
            class: ConnectomeClass::Dmri,
        },
        ConnectomeEntry {
            spec: "b".to_owned(),
            label: "B".to_owned(),
            class: ConnectomeClass::Dmri,
        },
    ]);
    let mut env = GroupEnv::default();
    state.choose_connectome(&env);
    assert_eq!(
        state
            .connectome
            .get(&Kind::FibreFiltering)
            .map(String::as_str),
        Some("a")
    );
    state.connectome.clear();
    env.last_connectomes
        .insert("fiberfiltering".to_owned(), "b".to_owned());
    state.choose_connectome(&env);
    assert_eq!(
        state
            .connectome
            .get(&Kind::FibreFiltering)
            .map(String::as_str),
        Some("b")
    );
    env.last_connectomes
        .insert("fiberfiltering".to_owned(), "gone".to_owned());
    state
        .connectome
        .insert(Kind::FibreFiltering, "gone".to_owned());
    state.choose_connectome(&env);
    assert_eq!(
        state
            .connectome
            .get(&Kind::FibreFiltering)
            .map(String::as_str),
        Some("a")
    );
}

#[test]
fn only_fibre_filtering_and_network_mapping_ask_about_the_cache_and_only_under_ask() {
    let state = ExplorerState::default();
    let sweet = Plan::Sweetspot(Box::new(sweetspot_options(&state, Path::new("g"), "v")));
    let fibre = Plan::Fibre(Box::new(fibre_options(
        &state,
        Path::new("g"),
        "v",
        "ppmi",
        None,
    )));
    let net = Plan::Netmap(Box::new(netmap_options(
        &state,
        Path::new("g"),
        "v",
        "ppmi",
        None,
        None,
    )));
    for setting in [CacheSetting::Ask, CacheSetting::Always, CacheSetting::Never] {
        assert!(!plan_asks(&sweet, setting));
        assert_eq!(plan_asks(&fibre, setting), setting == CacheSetting::Ask);
        assert_eq!(plan_asks(&net, setting), setting == CacheSetting::Ask);
    }
}

#[test]
fn the_fibre_options_carry_the_cache_setting_of_the_application() {
    let mut state = ExplorerState::default();
    assert_eq!(
        fibre_options(&state, Path::new("g"), "v", "c", None).cache,
        CacheSetting::Ask
    );
    state.cache = CacheSetting::Never;
    assert_eq!(
        fibre_options(&state, Path::new("g"), "v", "c", None).cache,
        CacheSetting::Never
    );
}

#[test]
fn a_sweet_spot_plan_never_needs_a_consent_probe() {
    let state = ExplorerState::default();
    let sweet = Plan::Sweetspot(Box::new(sweetspot_options(&state, Path::new("g"), "v")));
    assert!(consent_probe(&record(), &sweet).is_none());
}

#[test]
fn an_unresolvable_connectome_needs_no_consent_so_the_job_reports_the_error() {
    let state = ExplorerState::default();
    let fibre = Plan::Fibre(Box::new(fibre_options(
        &state,
        Path::new("g"),
        "v",
        "no-such-connectome",
        Some(Path::new("definitely-missing-dir")),
    )));
    assert!(consent_probe(&record(), &fibre).is_none());
}
