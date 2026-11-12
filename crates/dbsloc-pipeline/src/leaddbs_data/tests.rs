// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::AtomicBool;

use super::*;

fn write(path: &Path, bytes: &[u8]) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn atlases_root(base: &Path) -> PathBuf {
    base.join("templates")
        .join("space")
        .join(ATLAS_SPACE)
        .join("atlases")
}

fn fixture(base: &Path) {
    let a = atlases_root(base);
    write(&a.join("A/lh/a.nii.gz"), b"12345");
    write(&a.join("A/atlas_index.mat"), b"123");
    write(&a.join("B/rh/b.nii.gz"), b"1234567");
    write(&a.join(".hidden/h.nii.gz"), b"1");
    write(&base.join("connectomes/dMRI/X/data.mat"), b"12345678");
    write(&base.join("connectomes/dMRI/X/sub/more.bin"), b"12");
    write(&base.join("connectomes/fMRI/Y/dataset_info.json"), b"{}");
}

fn context(primary: &Path) -> ScanContext {
    ScanContext {
        bundled_atlases: Vec::new(),
        roots: ConnectomeRoots::from_primary(primary),
    }
}

fn run_scan(dir: &Path, ctx: &ScanContext) -> Res<Inventory> {
    scan(dir, ctx, &AtomicBool::new(false), &mut |_| {})
}

fn list_tree(root: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let m = std::fs::metadata(e.path()).unwrap();
            let stamp = if m.is_dir() {
                SystemTime::UNIX_EPOCH
            } else {
                m.modified().unwrap()
            };
            out.push((e.path(), m.len(), stamp));
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out.sort();
    out
}

fn tree_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_path_buf();
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn destinations(base: &Path, free: u64) -> Destinations {
    Destinations {
        atlases_dir: base.join("atlases"),
        connectomes_root: base.join("connectomes"),
        free_space_override: Some(free),
    }
}

#[test]
fn scan_lists_items_with_exact_sizes() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    fixture(src.path());
    let mut seen = Vec::new();
    let inv = scan(
        src.path(),
        &context(dst.path()),
        &AtomicBool::new(false),
        &mut |n| seen.push(n),
    )
    .unwrap();
    assert_eq!(seen, vec![1, 2, 3, 4]);
    let names: Vec<&str> = inv.atlases.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["A", "B"]);
    assert_eq!((inv.atlases[0].bytes, inv.atlases[0].files), (8, 2));
    assert!(inv.atlases[0].has_index);
    assert_eq!((inv.atlases[1].bytes, inv.atlases[1].files), (7, 1));
    assert!(!inv.atlases[1].has_index);
    assert_eq!(inv.connectomes.len(), 2);
    let x = &inv.connectomes[0];
    assert_eq!((x.class, x.name.as_str()), (ConnectomeClass::Dmri, "X"));
    assert_eq!((x.bytes, x.files), (10, 2));
    let y = &inv.connectomes[1];
    assert_eq!((y.class, y.name.as_str()), (ConnectomeClass::Fmri, "Y"));
    assert_eq!((y.bytes, y.files), (2, 1));
    assert!(inv.notes.is_empty());
}

#[test]
fn scan_accepts_data_folder_and_connectome_root() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    fixture(src.path());
    let ctx = context(dst.path());
    let a = run_scan(src.path(), &ctx).unwrap();
    let b = run_scan(&src.path().join("connectomes"), &ctx).unwrap();
    assert_eq!(a.connectome_root, b.connectome_root);
    assert_eq!(b.connectomes.len(), 2);
    assert!(b.atlases_root.is_none());

    let empty = tempfile::tempdir().unwrap();
    let inv = run_scan(empty.path(), &ctx).unwrap();
    assert!(inv.is_empty());
    assert!(inv.notes[0].starts_with("No Lead-DBS atlases or connectomes were found in "));

    let missing = src.path().join("nope");
    let err = run_scan(&missing, &ctx).unwrap_err();
    assert_eq!(err.code, LeadDbsCode::Missing);
    assert!(err.message.contains("is not a folder that can be read"));

    let dotted = src
        .path()
        .join("connectomes")
        .join("..")
        .join("connectomes");
    assert_eq!(
        run_scan(&dotted, &ctx).unwrap_err().code,
        LeadDbsCode::Missing
    );
}

#[test]
fn scan_flags_bundled_present_and_catalogue_matches() {
    let src = tempfile::tempdir().unwrap();
    let primary = tempfile::tempdir().unwrap();
    fixture(src.path());
    write(&primary.path().join("dMRI/X/data.mat"), b"1");
    let name = crate::connectome_catalog::catalogue()
        .iter()
        .find(|e| e.class == ConnectomeClass::Dmri)
        .map(|e| (e.folder().to_owned(), e.id, e.access))
        .unwrap();
    write(
        &src.path()
            .join("connectomes/dMRI")
            .join(&name.0)
            .join("data.mat"),
        b"1",
    );
    let ctx = ScanContext {
        bundled_atlases: vec!["A".to_owned()],
        roots: ConnectomeRoots::from_primary(primary.path()),
    };
    let inv = run_scan(src.path(), &ctx).unwrap();
    assert!(inv.atlases[0].bundled);
    assert!(!inv.atlases[1].bundled);
    let x = inv.connectomes.iter().find(|c| c.name == "X").unwrap();
    assert!(x.present);
    assert!(x.catalogue_id.is_none());
    let y = inv.connectomes.iter().find(|c| c.name == "Y").unwrap();
    assert!(!y.present);
    let cat = inv.connectomes.iter().find(|c| c.name == name.0).unwrap();
    assert_eq!(cat.catalogue_id.as_deref(), Some(name.1));
    assert_eq!(cat.access, Some(name.2));
}

#[test]
fn scan_and_link_plan_write_nothing() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    fixture(src.path());
    let before = list_tree(src.path());
    let inv = run_scan(src.path(), &context(dst.path())).unwrap();
    let plan = link_plan(&inv);
    assert_eq!(plan.atlases_dir, inv.atlases_root);
    assert_eq!(plan.connectomes_dir, inv.connectome_root);
    assert!(plan.atlases_dir.is_some() && plan.connectomes_dir.is_some());
    assert_eq!(list_tree(src.path()), before);
    assert!(list_tree(dst.path()).is_empty());
}

#[test]
fn copy_default_selection_copies_exact_bytes_and_skips_existing() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    write(
        &out.path().join("connectomes/fMRI/Y/dataset_info.json"),
        b"{}",
    );
    let ctx = ScanContext {
        bundled_atlases: vec!["B".to_owned()],
        roots: ConnectomeRoots::from_primary(out.path().join("connectomes")),
    };
    let inv = run_scan(src.path(), &ctx).unwrap();
    let sel = default_selection(&inv);
    assert_eq!(sel.atlases, ["A"]);
    assert_eq!(sel.connectomes, [(ConnectomeClass::Dmri, "X".to_owned())]);
    let before = list_tree(src.path());
    let mut events = Vec::new();
    let outcome = copy(
        &inv,
        &sel,
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |e| events.push(e),
    )
    .unwrap();
    assert_eq!(outcome.copied.len(), 2);
    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.atlases_dir, Some(out.path().join("atlases")));
    assert_eq!(
        tree_bytes(&out.path().join("atlases/A")),
        tree_bytes(&atlases_root(src.path()).join("A"))
    );
    assert_eq!(
        tree_bytes(&out.path().join("connectomes/dMRI/X")),
        tree_bytes(&src.path().join("connectomes/dMRI/X"))
    );
    assert!(!out.path().join("atlases/B").exists());
    assert!(matches!(events.last(), Some(CopyEvent::AllDone)));
    assert!(events.iter().any(|e| matches!(
        e,
        CopyEvent::ItemStarted {
            index: 0,
            count: 2,
            ..
        }
    )));
    assert_eq!(list_tree(src.path()), before);

    let again = copy(
        &inv,
        &Selection {
            atlases: vec!["A".to_owned()],
            connectomes: vec![(ConnectomeClass::Fmri, "Y".to_owned())],
        },
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert!(again.copied.is_empty());
    assert_eq!(again.skipped.len(), 2);
    assert_eq!(again.atlases_dir, None);
    let leftovers: Vec<_> = list_tree(out.path())
        .into_iter()
        .filter(|(p, _, _)| p.to_string_lossy().contains(STAGING_PREFIX))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn copy_with_too_little_space_fails_before_creating_anything() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    let sel = default_selection(&inv);
    let err = copy(
        &inv,
        &sel,
        &destinations(out.path(), 1000),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err.code, LeadDbsCode::Disk);
    assert!(err
        .message
        .starts_with("not enough free disk space: need 2 GB, 0 GB free on "));
    assert!(list_tree(out.path()).is_empty());
}

#[test]
fn copy_unknown_selection_is_missing() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(out.path())).unwrap();
    let sel = Selection {
        atlases: vec!["Z".to_owned()],
        connectomes: Vec::new(),
    };
    let err = copy(
        &inv,
        &sel,
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err.code, LeadDbsCode::Missing);
}

#[test]
fn cancel_during_copy_keeps_finished_items_and_leaves_no_staging() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    let sel = default_selection(&inv);
    let cancel = AtomicBool::new(false);
    let err = copy(
        &inv,
        &sel,
        &destinations(out.path(), 100 * GB),
        &cancel,
        &mut |e| {
            if matches!(e, CopyEvent::ItemDone { .. }) {
                cancel.store(true, Ordering::SeqCst);
            }
        },
    )
    .unwrap_err();
    assert!(err.is_cancelled());
    let partial = err.partial.as_ref().unwrap();
    assert_eq!(partial.copied.len(), 1);
    assert_eq!(partial.copied[0].name, "A");
    assert_eq!(partial.atlases_dir, Some(out.path().join("atlases")));
    assert!(out.path().join("atlases/A").is_dir());
    assert!(!out.path().join("atlases/B").exists());
    assert!(!out.path().join("connectomes").exists());

    let cancel = AtomicBool::new(false);
    let err = copy(
        &inv,
        &Selection {
            atlases: vec!["B".to_owned()],
            connectomes: Vec::new(),
        },
        &destinations(out.path(), 100 * GB),
        &cancel,
        &mut |e| {
            if matches!(e, CopyEvent::ItemStarted { .. }) {
                cancel.store(true, Ordering::SeqCst);
            }
        },
    )
    .unwrap_err();
    assert!(err.is_cancelled());
    assert!(!out.path().join("atlases/B").exists());
    let leftovers: Vec<_> = list_tree(out.path())
        .into_iter()
        .filter(|(p, _, _)| p.to_string_lossy().contains(STAGING_PREFIX))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn symbolic_links_are_neither_counted_nor_copied() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("secret.bin"), b"secret");
    let a = atlases_root(src.path()).join("A");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(outside.path(), a.join("link_dir")).is_ok()
        && std::os::unix::fs::symlink(outside.path().join("secret.bin"), a.join("link.bin"))
            .is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(outside.path(), a.join("link_dir")).is_ok()
        && std::os::windows::fs::symlink_file(
            outside.path().join("secret.bin"),
            a.join("link.bin"),
        )
        .is_ok();
    if !made {
        eprintln!("SKIPPED: the operating system refused to create a symbolic link");
        return;
    }
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    assert_eq!((inv.atlases[0].bytes, inv.atlases[0].files), (8, 2));
    let outcome = copy(
        &inv,
        &Selection {
            atlases: vec!["A".to_owned()],
            connectomes: Vec::new(),
        },
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(outcome.copied[0].skipped_links, 2);
    assert!(!out.path().join("atlases/A/link_dir").exists());
    assert!(!out.path().join("atlases/A/link.bin").exists());
    assert!(out.path().join("atlases/A/lh/a.nii.gz").is_file());
}

fn atlas_a() -> Selection {
    Selection {
        atlases: vec!["A".to_owned()],
        connectomes: Vec::new(),
    }
}

#[test]
fn copy_refuses_a_destination_inside_the_source_and_creates_nothing() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    let before = list_tree(src.path());
    let inside = Destinations {
        atlases_dir: src.path().join("copies").join("atlases"),
        connectomes_root: out.path().join("connectomes"),
        free_space_override: Some(100 * GB),
    };
    let err = copy(
        &inv,
        &atlas_a(),
        &inside,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err.code, LeadDbsCode::Io);
    assert!(err.message.contains("is inside the Lead-DBS folder"));
    assert!(err.message.contains("choose a data directory outside it"));
    assert!(err.partial.is_none());
    assert_eq!(list_tree(src.path()), before);
    assert!(list_tree(out.path()).is_empty());

    let in_atlases = Destinations {
        atlases_dir: out.path().join("atlases"),
        connectomes_root: atlases_root(src.path()).join("A").join("c"),
        free_space_override: Some(100 * GB),
    };
    let both = Selection {
        atlases: vec!["A".to_owned()],
        connectomes: vec![(ConnectomeClass::Dmri, "X".to_owned())],
    };
    let err = copy(
        &inv,
        &both,
        &in_atlases,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(err.message.contains("is inside the Lead-DBS folder"));
    assert_eq!(list_tree(src.path()), before);
}

#[test]
fn copy_refuses_a_destination_that_contains_the_source() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("leaddbs");
    fixture(&src);
    let inv = run_scan(&src, &context(&base.path().join("connectomes"))).unwrap();
    let before = list_tree(&src);
    let around = Destinations {
        atlases_dir: base.path().to_path_buf(),
        connectomes_root: base.path().join("connectomes"),
        free_space_override: Some(100 * GB),
    };
    let err = copy(
        &inv,
        &atlas_a(),
        &around,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err.code, LeadDbsCode::Io);
    assert!(err.message.contains("is inside the destination"));
    assert_eq!(list_tree(&src), before);
    assert!(!base.path().join("connectomes").exists());
}

#[test]
fn entry_classification_follows_links_and_junctions_only() {
    assert_eq!(classify_entry(true, false, false, false), Kind::Link);
    assert_eq!(classify_entry(true, true, false, true), Kind::Link);
    assert_eq!(classify_entry(false, false, true, true), Kind::Link);
    assert_eq!(classify_entry(false, false, true, false), Kind::Dir);
    assert_eq!(classify_entry(false, true, false, true), Kind::File);
    assert_eq!(classify_entry(false, true, false, false), Kind::File);
    assert_eq!(classify_entry(false, false, false, false), Kind::Skip);
}

#[test]
fn a_name_listed_twice_is_copied_once() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    let sel = Selection {
        atlases: vec!["A".to_owned(), "A".to_owned()],
        connectomes: vec![
            (ConnectomeClass::Dmri, "X".to_owned()),
            (ConnectomeClass::Dmri, "X".to_owned()),
        ],
    };
    let mut started = 0;
    let outcome = copy(
        &inv,
        &sel,
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |e| {
            if matches!(e, CopyEvent::ItemStarted { .. }) {
                started += 1;
            }
        },
    )
    .unwrap();
    assert_eq!(outcome.copied.len(), 2);
    assert!(outcome.skipped.is_empty());
    assert_eq!(started, 2);
}

fn marker(dir: &Path, pid: u64, heartbeat: u64) {
    write(
        &dir.join(STAGING_MARKER),
        format!("{{\"pid\":{pid},\"heartbeat\":{heartbeat}}}").as_bytes(),
    );
}

#[test]
fn only_marked_stale_staging_folders_are_removed() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    let atlases = out.path().join("atlases");
    write(&atlases.join(".partial-copy-foreign/keep.txt"), b"1");
    write(&atlases.join(".partial-copy-old/data.bin"), b"1");
    marker(&atlases.join(".partial-copy-old"), 1, 1);
    write(&atlases.join(".partial-copy-fresh/data.bin"), b"1");
    marker(&atlases.join(".partial-copy-fresh"), 1, unix_now());
    copy(
        &inv,
        &atlas_a(),
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert!(atlases.join(".partial-copy-foreign/keep.txt").is_file());
    assert!(!atlases.join(".partial-copy-old").exists());
    assert!(atlases.join(".partial-copy-fresh/data.bin").is_file());
    assert!(atlases.join("A/lh/a.nii.gz").is_file());
    assert!(!atlases.join("A").join(STAGING_MARKER).exists());
}

#[test]
fn a_running_or_unmarked_staging_folder_of_the_same_name_fails_the_item() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    let staging = out.path().join("atlases/.partial-copy-A");
    write(&staging.join("data.bin"), b"1");
    let err = copy(
        &inv,
        &atlas_a(),
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err.code, LeadDbsCode::Io);
    assert!(err.message.contains("was not made by this application"));
    assert!(staging.join("data.bin").is_file());

    marker(&staging, 1, unix_now());
    let err = copy(
        &inv,
        &atlas_a(),
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(err
        .message
        .contains("another copy of A seems to be running"));
    assert!(err.message.contains(".partial-copy-A"));
    assert!(err.message.contains("delete that folder"));
    assert!(staging.join("data.bin").is_file());
    assert!(!out.path().join("atlases/A").exists());

    marker(&staging, 1, unix_now() - 2 * 3600);
    copy(
        &inv,
        &atlas_a(),
        &destinations(out.path(), 100 * GB),
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert!(out.path().join("atlases/A/lh/a.nii.gz").is_file());
    assert!(!staging.exists());
}

#[test]
fn staging_state_is_stale_after_ten_minutes_without_a_heartbeat() {
    assert_eq!(staging_state(1000, 1000), StagingState::Fresh);
    assert_eq!(staging_state(1000, 1600), StagingState::Fresh);
    assert_eq!(staging_state(1000, 1601), StagingState::Stale);
    assert_eq!(staging_state(2000, 1000), StagingState::Fresh);
}

#[test]
fn the_heartbeat_is_due_every_thirty_seconds() {
    assert!(!heartbeat_due(100, 129));
    assert!(heartbeat_due(100, 130));
    assert!(!heartbeat_due(100, 50));
}

#[test]
fn a_stale_staging_folder_is_replaced_and_a_fresh_one_names_the_way_out() {
    let out = tempfile::tempdir().unwrap();
    let staging = out.path().join(".partial-copy-A");
    write(&staging.join("old.bin"), b"1");
    marker(&staging, 1, 1000);
    prepare_staging(&staging, "A", 1000 + 601).unwrap();
    assert!(!staging.join("old.bin").exists());
    assert_eq!(staging_marker(&staging), Some(1601));

    let err = prepare_staging(&staging, "A", 1601 + 5 * 60).unwrap_err();
    assert!(err.message.contains("last active 5 minutes ago"));
    assert!(err.message.contains(&staging.display().to_string()));
    assert!(err.message.contains("delete that folder and try again"));
}

#[test]
fn the_same_process_does_not_keep_a_stale_staging_folder_busy() {
    let out = tempfile::tempdir().unwrap();
    let staging = out.path().join(".partial-copy-A");
    write(&staging.join("old.bin"), b"1");
    marker(&staging, u64::from(std::process::id()), 1000);
    prepare_staging(&staging, "A", 1000 + 700).unwrap();
    assert!(!staging.join("old.bin").exists());
}

#[test]
fn a_failed_marker_write_leaves_no_staging_folder_behind() {
    let out = tempfile::tempdir().unwrap();
    let staging = out.path().join(".partial-copy-A");
    let err = prepare_staging_with(&staging, "A", 5, &|p, _| {
        Err(io_err(p, &std::io::Error::other("disk full")))
    })
    .unwrap_err();
    assert_eq!(err.code, LeadDbsCode::Io);
    assert!(!staging.exists());
}

#[test]
fn a_removal_is_retried_and_reports_the_last_failure() {
    use std::cell::Cell;
    let calls = Cell::new(0u32);
    let ok = remove_dir_retrying(Path::new("x"), 5, Duration::ZERO, &|_| {
        calls.set(calls.get() + 1);
        if calls.get() < 3 {
            Err(std::io::Error::other("busy"))
        } else {
            Ok(())
        }
    });
    assert!(ok.is_ok());
    assert_eq!(calls.get(), 3);
    calls.set(0);
    let bad = remove_dir_retrying(Path::new("x"), 4, Duration::ZERO, &|_| {
        calls.set(calls.get() + 1);
        Err(std::io::Error::other("busy"))
    });
    assert!(bad.is_err());
    assert_eq!(calls.get(), 4);
}

#[test]
fn the_removal_note_names_the_folder_and_the_item() {
    let note = removal_note(Path::new("/tmp/.partial-copy-A"), "A");
    assert!(note.starts_with("the temporary folder "));
    assert!(note.contains(".partial-copy-A"));
    assert!(note.ends_with("could not be removed; delete it before copying A again"));
}

#[test]
fn a_running_copy_refreshes_its_heartbeat() {
    let out = tempfile::tempdir().unwrap();
    let staging = out.path().join(".partial-copy-A");
    std::fs::create_dir(&staging).unwrap();
    marker(&staging, 1, 5);
    let mut sink = |_: CopyEvent| {};
    let mut progress = Progress {
        copied: 0,
        reported: 0,
        total: 10,
        on_event: &mut sink,
        staging: Some(staging.clone()),
        last_beat: 0,
    };
    progress.add(1, false);
    let beat = staging_marker(&staging).unwrap();
    assert!(beat >= unix_now() - 5);
    assert_eq!(progress.last_beat, beat);
}

#[test]
fn only_the_destinations_a_selected_job_uses_are_guarded() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    let dest = Destinations {
        atlases_dir: out.path().join("atlases"),
        connectomes_root: src.path().join("connectomes").join("inside"),
        free_space_override: Some(100 * GB),
    };
    let outcome = copy(
        &inv,
        &atlas_a(),
        &dest,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(outcome.copied.len(), 1);
    let sel = Selection {
        atlases: Vec::new(),
        connectomes: vec![(ConnectomeClass::Dmri, "X".to_owned())],
    };
    let err = copy(&inv, &sel, &dest, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
    assert!(err.message.contains("is inside the Lead-DBS folder"));

    let dest2 = Destinations {
        atlases_dir: src.path().join("copies"),
        connectomes_root: out.path().join("connectomes"),
        free_space_override: Some(100 * GB),
    };
    let outcome = copy(&inv, &sel, &dest2, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert_eq!(outcome.copied.len(), 1);
}

#[test]
fn a_scan_without_a_primary_root_marks_nothing_as_present() {
    let src = tempfile::tempdir().unwrap();
    fixture(src.path());
    let ctx = ScanContext {
        bundled_atlases: Vec::new(),
        roots: ConnectomeRoots::from_primary(PathBuf::new()),
    };
    let inv = run_scan(src.path(), &ctx).unwrap();
    assert!(!inv.connectomes.is_empty());
    assert!(inv.connectomes.iter().all(|c| !c.present));
}

#[test]
fn same_volume_is_true_for_two_folders_of_one_disk() {
    let base = tempfile::tempdir().unwrap();
    assert!(same_volume(
        &base.path().join("a"),
        &base.path().join("b").join("c")
    ));
}

#[test]
fn an_unknown_free_space_is_reported_as_a_note() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fixture(src.path());
    let inv = run_scan(src.path(), &context(&out.path().join("connectomes"))).unwrap();
    let job = Job {
        kind: DataKind::Atlas,
        name: "A",
        source: &inv.atlases[0].path,
        dest: out.path().join("atlases/A"),
        bytes: 8,
    };
    let dest = Destinations {
        atlases_dir: out.path().join("atlases"),
        connectomes_root: out.path().join("connectomes"),
        free_space_override: None,
    };
    let notes = check_disk_with(&[&job], &dest, &|_| None).unwrap();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].starts_with("free disk space on "));
    assert!(notes[0].ends_with(" could not be checked"));
    assert!(check_disk_with(&[&job], &dest, &|_| Some(100 * GB))
        .unwrap()
        .is_empty());
    let huge = Job {
        bytes: u64::MAX,
        ..job
    };
    assert!(check_disk_with(&[&huge, &huge], &dest, &|_| Some(100 * GB)).is_err());
}

#[cfg(windows)]
#[test]
fn verbatim_and_plain_drive_paths_share_a_volume_key() {
    let plain = std::env::temp_dir();
    let text = plain.to_string_lossy().into_owned();
    let verbatim = PathBuf::from(format!(r"\\?\{text}"));
    assert_eq!(volume_key(&plain), volume_key(&verbatim));
}

#[test]
fn bundled_names_are_the_visible_folders() {
    let res = tempfile::tempdir().unwrap();
    write(&res.path().join("atlases/leaddbs/Beta/x"), b"1");
    write(&res.path().join("atlases/leaddbs/Alpha/x"), b"1");
    write(&res.path().join("atlases/leaddbs/.hid/x"), b"1");
    assert_eq!(bundled_atlas_names(Some(res.path())), ["Alpha", "Beta"]);
    assert!(bundled_atlas_names(None).is_empty());
}
