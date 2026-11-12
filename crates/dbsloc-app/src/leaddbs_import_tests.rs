// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::time::Duration;

use dbsloc_pipeline::connectome_catalog::ConnectomeClass;
use dbsloc_pipeline::connectome_fetch::ConnectomeRoots;
use dbsloc_pipeline::leaddbs_data::CopiedItem;

use super::*;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("dbsloc_ldbs_gui_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let atlases = root.join("templates/space/MNI152NLin2009bAsym/atlases");
        for atlas in ["A", "B"] {
            fs::create_dir_all(atlases.join(atlas)).unwrap();
            fs::write(atlases.join(atlas).join("atlas_index.mat"), vec![0u8; 100]).unwrap();
        }
        fs::create_dir_all(root.join("connectomes/dMRI/X")).unwrap();
        fs::write(root.join("connectomes/dMRI/X/data.mat"), vec![0u8; 50]).unwrap();
        fs::create_dir_all(root.join("connectomes/fMRI/Y")).unwrap();
        fs::write(root.join("connectomes/fMRI/Y/dataset_info.json"), b"{}").unwrap();
        Fixture { root }
    }

    fn inventory(&self, bundled: &[&str]) -> Inventory {
        let primary = self.root.join("primary");
        scan(
            &self.root,
            &ScanContext {
                bundled_atlases: bundled.iter().map(|s| (*s).to_owned()).collect(),
                roots: ConnectomeRoots::from_primary(&primary),
            },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn dest(free_atlases: Option<u64>, free_connectomes: Option<u64>) -> Destination {
    Destination {
        atlases_dir: Some(PathBuf::from("atlases")),
        connectomes_root: PathBuf::from("connectomes"),
        free_atlases,
        free_connectomes,
        same_volume: false,
    }
}

fn dest_same_volume(free_atlases: Option<u64>, free_connectomes: Option<u64>) -> Destination {
    Destination {
        same_volume: true,
        ..dest(free_atlases, free_connectomes)
    }
}

fn connectome(access: Option<Access>, present: bool) -> ConnectomeItem {
    ConnectomeItem {
        class: ConnectomeClass::Dmri,
        name: "N".to_owned(),
        path: PathBuf::from("N"),
        bytes: 1,
        files: 1,
        unreadable: 0,
        catalogue_id: None,
        access,
        present,
    }
}

fn draw(imp: &mut LeadDbsImport, state: &mut DbslocAppState, size: egui::Vec2) -> Events {
    let ctx = egui::Context::default();
    let mut last = Events::default();
    for _ in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            last = imp.show(ui.ctx(), state);
        })
        .textures_delta
        .clear();
    }
    last
}

const SIZES: [egui::Vec2; 2] = [egui::vec2(1400.0, 900.0), egui::vec2(420.0, 700.0)];

#[test]
fn a_row_note_names_what_is_already_there_and_what_needs_an_agreement() {
    let fx = Fixture::new("notes");
    let inv = fx.inventory(&["B"]);
    assert_eq!(atlas_note(&inv.atlases[0]), "");
    assert_eq!(atlas_note(&inv.atlases[1]), "Already included");
    assert_eq!(connectome_note(&connectome(None, false)), "");
    assert_eq!(connectome_note(&connectome(None, true)), "Already there");
    assert_eq!(
        connectome_note(&connectome(Some(Access::Dua), false)),
        "Needs your data use agreement"
    );
    assert_eq!(
        connectome_note(&connectome(Some(Access::CohortTerms), true)),
        "Already there; Cohort data terms"
    );
    assert_eq!(connectome_note(&connectome(Some(Access::Open), false)), "");
}

#[test]
fn the_default_ticks_skip_bundled_atlases_and_present_connectomes() {
    let fx = Fixture::new("ticks");
    let inv = fx.inventory(&["B"]);
    let ticks = default_ticks(&inv);
    assert_eq!(ticks.atlases, vec![true, false]);
    assert_eq!(ticks.connectomes, vec![true, true]);
    let sel = selection_from(&inv, &ticks);
    assert_eq!(sel.atlases, vec!["A".to_owned()]);
    assert_eq!(sel.connectomes.len(), 2);
}

#[test]
fn the_needed_size_follows_the_ticks() {
    let fx = Fixture::new("needed");
    let inv = fx.inventory(&[]);
    let mut ticks = default_ticks(&inv);
    let (a, c) = needed_bytes(&inv, &ticks);
    assert_eq!(a, inv.atlas_bytes());
    assert_eq!(c, inv.connectome_bytes());
    assert!(a > 0 && c > 0);
    ticks.atlases[1] = false;
    ticks.connectomes = vec![false, false];
    assert_eq!(needed_bytes(&inv, &ticks), (inv.atlases[0].bytes, 0));
}

#[test]
fn copy_needs_a_tick_and_enough_room_and_says_why_not() {
    let fx = Fixture::new("gate");
    let inv = fx.inventory(&[]);
    let mut ticks = default_ticks(&inv);
    let roomy = dest(Some(50 * GB), Some(50 * GB));
    assert_eq!(
        import_gate(Mode::Copy, &inv, &ticks, Some(&roomy)),
        ImportGate::Enabled
    );
    let tight = dest(Some(GB / 2), Some(GB / 2));
    match import_gate(Mode::Copy, &inv, &ticks, Some(&tight)) {
        ImportGate::Disabled(reason) => {
            assert!(reason.contains("not enough free disk space"), "{reason}");
        }
        ImportGate::Enabled => panic!("a full disk must disable the button"),
    }
    let split = dest(Some(50 * GB), Some(GB / 2));
    assert!(matches!(
        import_gate(Mode::Copy, &inv, &ticks, Some(&split)),
        ImportGate::Disabled(_)
    ));
    ticks.atlases = vec![false, false];
    ticks.connectomes = vec![false, false];
    assert_eq!(
        import_gate(Mode::Copy, &inv, &ticks, Some(&tight)),
        ImportGate::Disabled("Tick at least one item to copy.".to_owned())
    );
    assert_eq!(
        import_gate(Mode::Link, &inv, &ticks, Some(&tight)),
        ImportGate::Enabled
    );
}

#[test]
fn a_folder_without_data_cannot_be_imported() {
    let inv = empty_inventory();
    let ticks = default_ticks(&inv);
    assert!(matches!(
        import_gate(Mode::Link, &inv, &ticks, None),
        ImportGate::Disabled(_)
    ));
}

#[test]
fn the_copy_text_gives_the_needed_and_the_free_size() {
    let text = copy_text(12_300_000_000, Some(&dest(Some(45 * GB), Some(60 * GB))));
    assert!(text.starts_with("Needs 12.3 GB (45.0 GB free)."), "{text}");
    assert!(copy_text(0, None).contains("free space unknown"));
}

#[test]
fn the_summary_of_a_link_names_the_folders_and_the_licence_list() {
    let fx = Fixture::new("linksum");
    let mut inv = fx.inventory(&[]);
    inv.connectomes[0].access = Some(Access::Dua);
    let plan = link_plan(&inv);
    let summary = link_summary(&inv, &plan);
    assert!(
        summary.lines[0].starts_with("2 atlases"),
        "{:?}",
        summary.lines
    );
    assert!(summary.lines[1].starts_with("2 connectomes"));
    assert!(summary.lines.last().unwrap().contains("Nothing was copied"));
    assert_eq!(
        summary.licence,
        vec!["X: Needs your data use agreement".to_owned()]
    );
}

#[test]
fn the_summary_of_a_copy_lists_copied_and_skipped_items_and_only_copied_licences() {
    let fx = Fixture::new("copysum");
    let mut inv = fx.inventory(&[]);
    inv.connectomes[0].access = Some(Access::CohortTerms);
    inv.connectomes[1].access = Some(Access::Dua);
    let item = |kind, name: &str| CopiedItem {
        kind,
        name: name.to_owned(),
        path: PathBuf::from(name),
        bytes: 2_000_000,
        skipped_links: 0,
    };
    let outcome = CopyOutcome {
        copied: vec![
            item(DataKind::Atlas, "A"),
            item(DataKind::Connectome(ConnectomeClass::Dmri), "X"),
        ],
        skipped: vec![item(DataKind::Atlas, "B")],
        atlases_dir: Some(PathBuf::from("data/atlases/leaddbs")),
        notes: Vec::new(),
    };
    let summary = copy_summary(&inv, &outcome);
    assert!(summary.lines[0].starts_with("Copied 1 atlas (2 MB) into "));
    assert!(summary.lines[1].starts_with("Copied 1 connectome (2 MB)"));
    assert_eq!(summary.lines[2], "Already there, not copied: B.");
    assert_eq!(summary.lines.len(), 3);
    assert_eq!(summary.licence, vec!["X: Cohort data terms".to_owned()]);

    let mut linked = outcome.clone();
    linked.copied[0].skipped_links = 3;
    linked.notes = vec!["free disk space on D: could not be checked".to_owned()];
    let summary = copy_summary(&inv, &linked);
    assert!(summary
        .lines
        .contains(&"A: 3 linked entries were not copied.".to_owned()));
    assert!(summary
        .lines
        .contains(&"free disk space on D: could not be checked.".to_owned()));
}

#[test]
fn a_copy_that_stopped_early_stores_the_atlases_directory_and_lists_what_was_copied() {
    let fx = Fixture::new("partial");
    let inv = fx.inventory(&[]);
    let mut state = DbslocAppState::default();
    let partial = CopyOutcome {
        copied: vec![CopiedItem {
            kind: DataKind::Atlas,
            name: "A".to_owned(),
            path: PathBuf::from("data/atlases/leaddbs/A"),
            bytes: 2_000_000,
            skipped_links: 0,
        }],
        skipped: Vec::new(),
        atlases_dir: Some(PathBuf::from("data/atlases/leaddbs")),
        notes: Vec::new(),
    };
    let cancelled = LeadDbsDataError::cancelled().with_partial(partial.clone());
    let (notice, error) = apply_copy_failure(&mut state, &inv, &cancelled);
    assert_eq!(
        state.stage.leaddbs_atlases_dir,
        Some(PathBuf::from("data/atlases/leaddbs"))
    );
    assert!(error.is_none());
    let notice = notice.unwrap();
    assert!(notice.contains("Copied 1 atlas"), "{notice}");

    let mut state = DbslocAppState::default();
    let failed =
        LeadDbsDataError::new(leaddbs_data::LeadDbsCode::Io, "disk gone").with_partial(partial);
    let (notice, error) = apply_copy_failure(&mut state, &inv, &failed);
    assert!(notice.is_none());
    let error = error.unwrap();
    assert!(
        error.contains("disk gone") && error.contains("Copied 1 atlas"),
        "{error}"
    );
    assert!(state.stage.leaddbs_atlases_dir.is_some());

    let mut state = DbslocAppState::default();
    let nothing = LeadDbsDataError::new(leaddbs_data::LeadDbsCode::Io, "refused");
    apply_copy_failure(&mut state, &inv, &nothing);
    assert_eq!(state.stage.leaddbs_atlases_dir, None);
}

#[test]
fn opening_the_first_start_window_sets_the_flag_at_once() {
    let mut state = DbslocAppState::default();
    assert!(!begin_first_start(&mut state, false, false));
    assert!(!state.leaddbs_import_offered);
    assert!(!begin_first_start(&mut state, true, true));
    assert!(!state.leaddbs_import_offered);
    assert!(begin_first_start(&mut state, true, false));
    assert!(state.leaddbs_import_offered);
    assert!(!begin_first_start(&mut state, true, false));
    let (back, complete) = crate::app::parse_app_state(&crate::app::state_to_json(&state));
    assert!(complete);
    assert!(back.leaddbs_import_offered);
}

#[test]
fn the_notes_of_a_scan_are_always_listed() {
    let fx = Fixture::new("notes_lines");
    let mut inv = fx.inventory(&[]);
    assert!(note_lines(&inv).is_empty());
    inv.notes = vec!["Skipped X because it is a link; links are never followed.".to_owned()];
    assert_eq!(
        note_lines(&inv),
        vec![(inv.notes[0].clone(), false)],
        "a note of a non-empty inventory is muted"
    );
    let mut empty = empty_inventory();
    empty.notes = vec!["No Lead-DBS atlases or connectomes were found.".to_owned()];
    assert_eq!(note_lines(&empty), vec![(empty.notes[0].clone(), true)]);
}

#[test]
fn the_space_check_sums_the_needs_on_one_volume_and_separates_two_volumes() {
    let same = dest_same_volume(Some(5 * GB), Some(6 * GB));
    assert!(space_shortfall(2 * GB + GB / 2, 2 * GB + GB / 2, &same).is_some());
    assert!(space_shortfall(2 * GB, 2 * GB, &same).is_none());
    let apart = dest(Some(5 * GB), Some(5 * GB));
    assert!(space_shortfall(3 * GB, 3 * GB, &apart).is_none());
    assert!(space_shortfall(5 * GB, 0, &apart).is_some());
    assert!(space_shortfall(0, 5 * GB, &apart).is_some());
    let unknown = dest_same_volume(None, Some(3 * GB));
    assert!(space_shortfall(2 * GB, 2 * GB, &unknown).is_some());
}

#[test]
fn a_scan_that_ends_without_data_returns_to_the_step_it_came_from() {
    assert_eq!(scan_return(false, &ScanEnd::Found), (Step::Found, None));
    assert_eq!(scan_return(true, &ScanEnd::Cancelled), (Step::Found, None));
    assert_eq!(scan_return(false, &ScanEnd::Cancelled), (Step::Ask, None));
    assert_eq!(
        scan_return(false, &ScanEnd::Failed("E_LDBS_IO: x".to_owned())),
        (Step::Ask, Some("E_LDBS_IO: x".to_owned()))
    );
    assert_eq!(
        scan_return(true, &ScanEnd::Vanished),
        (
            Step::Found,
            Some("The scan stopped unexpectedly.".to_owned())
        )
    );
    assert_eq!(
        scan_return(false, &ScanEnd::Vanished),
        (Step::Ask, Some(SCAN_STOPPED_TEXT.to_owned()))
    );
}

#[test]
fn a_vanished_scan_worker_shows_the_error_and_keeps_the_old_folder() {
    let fx = Fixture::new("vanish_scan");
    let inv = fx.inventory(&[]);
    let mut imp = LeadDbsImport::default();
    imp.open(false);
    imp.inventory = Some(inv.clone());
    imp.step = Step::Found;
    let (tx, rx) = mpsc::channel();
    drop(tx);
    imp.scan = Some(ScanJob {
        rx,
        cancel: Arc::new(AtomicBool::new(false)),
        sized: 0,
    });
    imp.scan_source = Some(PathBuf::from("the-new-folder"));
    imp.step = Step::Scanning;
    imp.poll_scan();
    assert_eq!(imp.step(), Step::Found);
    assert_eq!(imp.error.as_deref(), Some(SCAN_STOPPED_TEXT));
    assert_eq!(imp.inventory.as_ref().map(|i| &i.source), Some(&inv.source));
    assert!(imp.scan_source.is_none());
}

#[test]
fn a_vanished_copy_worker_keeps_what_was_seen_complete() {
    let fx = Fixture::new("vanish_copy");
    let inv = fx.inventory(&[]);
    let done = vec![
        (DataKind::Atlas, "A".to_owned()),
        (DataKind::Connectome(ConnectomeClass::Dmri), "X".to_owned()),
    ];
    let e = vanished_copy_error(&inv, &done, Path::new("data/atlases"), Path::new("data/c"));
    let partial = e.partial.as_ref().unwrap();
    assert_eq!(partial.copied.len(), 2);
    assert_eq!(partial.copied[0].path, Path::new("data/atlases").join("A"));
    assert_eq!(
        partial.copied[1].path,
        Path::new("data/c").join("dMRI").join("X")
    );
    assert_eq!(partial.copied[0].bytes, inv.atlases[0].bytes);
    assert_eq!(partial.atlases_dir, Some(PathBuf::from("data/atlases")));
    let none = vanished_copy_error(&inv, &[], Path::new("a"), Path::new("c"));
    assert_eq!(none.partial.as_ref().unwrap().atlases_dir, None);
    let mut state = DbslocAppState::default();
    let (notice, error) = apply_copy_failure(&mut state, &inv, &e);
    assert!(notice.is_none());
    assert!(error.unwrap().contains("Copied before the stop and kept"));
    assert_eq!(
        state.stage.leaddbs_atlases_dir,
        Some(PathBuf::from("data/atlases"))
    );
}

#[test]
fn a_vanished_copy_worker_ends_the_copy_step_with_a_partial_outcome() {
    let fx = Fixture::new("vanish_copy_ui");
    let inv = fx.inventory(&[]);
    let mut state = DbslocAppState::default();
    let mut imp = LeadDbsImport::default();
    imp.open(false);
    imp.inventory = Some(inv);
    imp.dest = Some(dest(Some(50 * GB), Some(50 * GB)));
    let (tx, rx) = mpsc::channel();
    tx.send(CopyMsg::Event(CopyEvent::ItemDone {
        kind: DataKind::Atlas,
        name: "A".to_owned(),
    }))
    .unwrap();
    drop(tx);
    imp.copy = Some(CopyJob {
        rx,
        cancel: Arc::new(AtomicBool::new(false)),
        item: None,
        copied: 0,
        total: 0,
        done: Vec::new(),
    });
    imp.step = Step::Copying;
    let mut ev = Events::default();
    imp.poll_copy(&mut state, &mut ev);
    assert_eq!(imp.step(), Step::Found);
    assert!(imp.error.as_deref().unwrap().contains("Copied 1 atlas"));
    assert!(state.stage.leaddbs_atlases_dir.is_some());
}

#[test]
fn a_new_folder_does_not_replace_the_path_of_the_inventory_shown() {
    let fx = Fixture::new("pending_path");
    let inv = fx.inventory(&[]);
    let mut imp = LeadDbsImport::default();
    imp.open(false);
    imp.inventory = Some(inv.clone());
    imp.step = Step::Found;
    imp.scan_source = Some(PathBuf::from("pending"));
    assert_eq!(imp.inventory.as_ref().unwrap().source, inv.source);
    let (tx, rx) = mpsc::channel();
    tx.send(ScanMsg::Done(Box::new(Err(LeadDbsDataError::cancelled()))))
        .unwrap();
    imp.scan = Some(ScanJob {
        rx,
        cancel: Arc::new(AtomicBool::new(false)),
        sized: 0,
    });
    imp.step = Step::Scanning;
    imp.poll_scan();
    assert_eq!(imp.step(), Step::Found);
    assert!(imp.scan_source.is_none());
    assert_eq!(imp.inventory.as_ref().unwrap().source, inv.source);
}

#[test]
fn the_licence_list_matches_kind_and_name() {
    let mut inv = empty_inventory();
    let mut dmri = connectome(Some(Access::Dua), false);
    dmri.name = "Same".to_owned();
    let mut fmri = connectome(Some(Access::Dua), false);
    fmri.class = ConnectomeClass::Fmri;
    fmri.name = "Same".to_owned();
    inv.connectomes = vec![dmri, fmri];
    let only = [(ConnectomeClass::Fmri, "Same".to_owned())];
    assert_eq!(
        licence_list(&inv, Some(&only)),
        vec!["Same: Needs your data use agreement".to_owned()]
    );
    assert_eq!(licence_list(&inv, None).len(), 2);
    assert!(licence_list(&inv, Some(&[])).is_empty());
}

#[test]
fn the_licence_hint_names_the_real_controls() {
    assert!(LICENCE_HINT.contains("Open Connectomes…, select the connectome and tick the boxes under Licence and terms to record your acceptance."));
}

#[test]
fn small_sizes_are_shown_in_kilobytes() {
    assert_eq!(bytes_label(312_000), "312 kB");
    assert_eq!(bytes_label(400_000), "400 kB");
    let text = copy_text(312_000, None);
    assert!(text.starts_with("Needs 312 kB"), "{text}");
}

#[test]
fn the_first_start_question_is_asked_exactly_once() {
    let mut state = DbslocAppState::default();
    assert!(!state.leaddbs_import_offered);
    assert!(first_start_request(&state, true, false));
    assert!(!first_start_request(&state, false, false));
    assert!(!first_start_request(&state, true, true));
    let mut imp = LeadDbsImport::default();
    imp.open(true);
    assert!(imp.is_open());
    let mut ev = Events::default();
    imp.close(&mut state, &mut ev);
    assert!(state.leaddbs_import_offered);
    assert!(ev.closed);
    assert!(!first_start_request(&state, true, false));
}

#[test]
fn skip_and_escape_both_set_the_flag() {
    let mut state = DbslocAppState::default();
    let mut imp = LeadDbsImport::default();
    imp.open(true);
    let ctx = egui::Context::default();
    let mut ev = Events::default();
    imp.handle(&ctx, &mut state, Act::Skip, &mut ev);
    assert!(state.leaddbs_import_offered && !imp.is_open());

    let mut state = DbslocAppState::default();
    imp.open(true);
    press_escape(&mut imp, &mut state, true);
    assert!(!imp.is_open());
    assert!(state.leaddbs_import_offered);
}

fn press_escape(imp: &mut LeadDbsImport, state: &mut DbslocAppState, pointer_over: bool) {
    let ctx = egui::Context::default();
    let centre = egui::pos2(SIZES[0].x / 2.0, SIZES[0].y / 2.0);
    let far = egui::pos2(2.0, 2.0);
    let at = if pointer_over { centre } else { far };
    for frame in 0..4 {
        let mut events = vec![egui::Event::PointerMoved(at)];
        if frame == 3 {
            events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let input = egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SIZES[0])),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            imp.show(ui.ctx(), state);
        })
        .textures_delta
        .clear();
    }
}

#[test]
fn escape_elsewhere_in_the_application_does_not_close_the_window() {
    let mut state = DbslocAppState::default();
    let mut imp = LeadDbsImport::default();
    imp.open(false);
    press_escape(&mut imp, &mut state, false);
    assert!(imp.is_open());
    assert!(!state.leaddbs_import_offered);
}

#[test]
fn escape_closes_only_when_the_window_has_the_key_and_nothing_else_does() {
    let base = EscapeContext {
        pointer_over: true,
        ..EscapeContext::default()
    };
    assert!(escape_closes(&base));
    assert!(escape_closes(&EscapeContext {
        pointer_over: false,
        window_focused: true,
        ..base
    }));
    assert!(!escape_closes(&EscapeContext {
        pointer_over: false,
        ..base
    }));
    assert!(!escape_closes(&EscapeContext { busy: true, ..base }));
    assert!(!escape_closes(&EscapeContext {
        popup_open: true,
        ..base
    }));
    assert!(!escape_closes(&EscapeContext {
        other_focused: true,
        ..base
    }));
}

#[test]
fn escape_does_not_close_the_window_while_a_job_runs() {
    let mut state = DbslocAppState::default();
    let mut imp = LeadDbsImport::default();
    imp.open(false);
    let (_tx, rx) = mpsc::channel();
    imp.scan = Some(ScanJob {
        rx,
        cancel: Arc::new(AtomicBool::new(false)),
        sized: 0,
    });
    imp.step = Step::Scanning;
    press_escape(&mut imp, &mut state, true);
    assert!(imp.is_open());
    assert!(!state.leaddbs_import_offered);
}

#[test]
fn every_step_draws_at_a_wide_and_a_narrow_size() {
    let fx = Fixture::new("draw");
    let inv = fx.inventory(&["B"]);
    for size in SIZES {
        let mut state = DbslocAppState::default();

        let mut imp = LeadDbsImport::default();
        imp.open(true);
        draw(&mut imp, &mut state, size);
        assert_eq!(imp.step(), Step::Ask);
        let mut later = LeadDbsImport::default();
        later.open(false);
        draw(&mut later, &mut state, size);

        let mut imp = LeadDbsImport::default();
        imp.open(false);
        let (_tx, rx) = mpsc::channel();
        imp.scan = Some(ScanJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            sized: 7,
        });
        imp.step = Step::Scanning;
        draw(&mut imp, &mut state, size);
        assert_eq!(imp.step(), Step::Scanning);

        for mode in [Mode::Link, Mode::Copy] {
            let mut imp = LeadDbsImport::default();
            imp.open(false);
            imp.ticks = default_ticks(&inv);
            imp.inventory = Some(inv.clone());
            imp.dest = Some(dest(Some(50 * GB), Some(50 * GB)));
            imp.mode = mode;
            imp.step = Step::Found;
            imp.error = Some("An error line.".to_owned());
            draw(&mut imp, &mut state, size);
            assert!(imp.inventory.is_some());
        }

        let mut imp = LeadDbsImport::default();
        imp.open(false);
        imp.inventory = Some(empty_inventory());
        imp.step = Step::Found;
        draw(&mut imp, &mut state, size);

        let mut imp = LeadDbsImport::default();
        imp.open(false);
        let (_tx, rx) = mpsc::channel();
        imp.copy = Some(CopyJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            item: Some((0, 2, "A".to_owned())),
            copied: 5,
            total: 10,
            done: Vec::new(),
        });
        imp.step = Step::Copying;
        draw(&mut imp, &mut state, size);
        assert_eq!(imp.step(), Step::Copying);

        let mut imp = LeadDbsImport::default();
        imp.open(false);
        imp.summary = Some(Summary {
            lines: vec!["Copied 1 atlas (1 MB).".to_owned()],
            licence: vec!["X: Needs your data use agreement".to_owned()],
        });
        imp.step = Step::Done;
        draw(&mut imp, &mut state, size);
        assert_eq!(imp.step(), Step::Done);
    }
}

#[test]
fn the_link_mode_stores_both_settings() {
    let fx = Fixture::new("link");
    let inv = fx.inventory(&[]);
    let mut state = DbslocAppState::default();
    let mut imp = LeadDbsImport::default();
    imp.open(false);
    imp.inventory = Some(inv.clone());
    imp.mode = Mode::Link;
    imp.step = Step::Found;
    let ctx = egui::Context::default();
    let mut ev = Events::default();
    imp.handle(&ctx, &mut state, Act::Import, &mut ev);
    assert_eq!(imp.step(), Step::Done);
    assert!(ev.refresh);
    assert_eq!(state.stage.leaddbs_atlases_dir, inv.atlases_root);
    assert_eq!(state.stage.leaddbs_connectomes_dir, inv.connectome_root);
    assert!(state.stage.leaddbs_atlases_dir.is_some());
    assert!(state.stage.leaddbs_connectomes_dir.is_some());
}

#[test]
fn a_link_leaves_a_missing_root_unchanged() {
    let mut state = DbslocAppState::default();
    state.stage.leaddbs_atlases_dir = Some(PathBuf::from("old"));
    apply_link(
        &mut state,
        &LinkPlan {
            atlases_dir: None,
            connectomes_dir: Some(PathBuf::from("c")),
        },
    );
    assert_eq!(state.stage.leaddbs_atlases_dir, Some(PathBuf::from("old")));
    assert_eq!(
        state.stage.leaddbs_connectomes_dir,
        Some(PathBuf::from("c"))
    );
}

#[test]
fn the_copy_outcome_stores_the_atlases_directory() {
    let mut state = DbslocAppState::default();
    let none = CopyOutcome {
        copied: Vec::new(),
        skipped: Vec::new(),
        atlases_dir: None,
        notes: Vec::new(),
    };
    apply_copy_outcome(&mut state, &none);
    assert_eq!(state.stage.leaddbs_atlases_dir, None);
    let some = CopyOutcome {
        atlases_dir: Some(PathBuf::from("data/atlases/leaddbs")),
        ..none
    };
    apply_copy_outcome(&mut state, &some);
    assert_eq!(
        state.stage.leaddbs_atlases_dir,
        Some(PathBuf::from("data/atlases/leaddbs"))
    );
    assert_eq!(state.stage.leaddbs_connectomes_dir, None);
}

#[test]
fn the_window_scans_a_folder_and_links_it_end_to_end() {
    let fx = Fixture::new("flow");
    let mut state = DbslocAppState::default();
    state.stage.connectomes_dir = Some(fx.root.join("primary"));
    let mut imp = LeadDbsImport::default();
    imp.open_with_folder(fx.root.clone(), false);
    let ctx = egui::Context::default();
    for _ in 0..200 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SIZES[0])),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            imp.show(ui.ctx(), &mut state);
        })
        .textures_delta
        .clear();
        if imp.step() == Step::Found {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(imp.step(), Step::Found);
    let inv = imp.inventory.as_ref().unwrap();
    assert_eq!(inv.atlases.len(), 2);
    assert_eq!(inv.connectomes.len(), 2);
    let mut ev = Events::default();
    imp.handle(&ctx, &mut state, Act::Import, &mut ev);
    assert!(state.stage.leaddbs_atlases_dir.is_some());
    assert!(state.stage.leaddbs_connectomes_dir.is_some());
}
