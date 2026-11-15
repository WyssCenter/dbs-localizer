// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dbsloc_methods_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("rawdata/sub-1/ses-preop/anat")).unwrap();
    fs::write(
        d.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"),
        b"x",
    )
    .unwrap();
    d
}

fn subject(root: &Path) -> (Subject, RunOptions) {
    let sub = crate::bids::resolve_subject_with(root, "1", false).unwrap();
    (sub, RunOptions::new(root, "1"))
}

fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value.to_string()).unwrap();
}

fn entries(path: &Path) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap();
    text.split("\n\n")
        .filter(|e| !e.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn side(key: &'static str, head: bool, tail: bool, roll: bool) -> ManualSide<'static> {
    ManualSide {
        key,
        head,
        tail,
        roll,
    }
}

fn coreg_record(backend: &str, keys: &[&str], adapter: Option<Value>) -> Value {
    let mut log = crate::stage_impl::method_log_base(crate::stage_impl::COREG_METHOD_CT, backend);
    for key in keys {
        let method = if *key == "CT" {
            crate::stage_impl::COREG_METHOD_CT
        } else {
            crate::stage_impl::COREG_METHOD_MR
        };
        log.insert(
            (*key).to_owned(),
            json!({"method": method, "backend": backend}),
        );
    }
    if let Some(a) = adapter {
        log.insert("reg_adapter".into(), a);
    }
    Value::Object(log)
}

#[test]
fn format_entry_is_exact_with_and_without_references() {
    let with = format_entry(
        "2026-11-15T10:00:00Z",
        "preprocess",
        "1.2.3+abc",
        "One paragraph.",
        &["Ref one.", "Ref two."],
    );
    assert_eq!(
        with,
        "[2026-11-15T10:00:00Z] preprocess | Wyss DBS Localizer 1.2.3+abc\nOne paragraph.\nReferences:\n- Ref one.\n- Ref two.\n\n"
    );
    let without = format_entry("2026-11-15T10:00:00Z", "vat", "1.2.3", "Text.", &[]);
    assert_eq!(
        without,
        "[2026-11-15T10:00:00Z] vat | Wyss DBS Localizer 1.2.3\nText.\n\n"
    );
}

#[test]
fn appending_twice_keeps_the_first_entry_and_creates_the_directory() {
    let root = scratch("append");
    let path = root.join("deep/er/methods.txt");
    let first = format_entry("t1", "a", "v", "First.", &["R."]);
    let second = format_entry("t2", "b", "v", "Second.", &[]);
    append_entry(&path, &first).unwrap();
    let after_first = fs::read(&path).unwrap();
    assert_eq!(after_first, first.as_bytes());
    append_entry(&path, &second).unwrap();
    let both = fs::read(&path).unwrap();
    assert_eq!(&both[..after_first.len()], after_first.as_slice());
    assert_eq!(both, format!("{first}{second}").into_bytes());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_file_without_a_trailing_newline_gets_exactly_one_line_break() {
    let root = scratch("break");
    let path = root.join("methods.txt");
    fs::write(&path, "existing text").unwrap();
    let entry = format_entry("t", "a", "v", "P.", &[]);
    append_entry(&path, &entry).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("existing text\n{entry}")
    );
    let empty = root.join("empty.txt");
    fs::write(&empty, "").unwrap();
    append_entry(&empty, &entry).unwrap();
    assert_eq!(fs::read_to_string(&empty).unwrap(), entry);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn appending_does_not_write_through_a_hard_link() {
    let root = scratch("hardlink");
    let source = root.join("methods.txt");
    let alias = root.join("alias.txt");
    let first = format_entry("t1", "a", "v", "First.", &[]);
    append_entry(&source, &first).unwrap();
    fs::hard_link(&source, &alias).unwrap();
    append_entry(&source, &format_entry("t2", "b", "v", "Second.", &[])).unwrap();
    assert_eq!(fs::read_to_string(&alias).unwrap(), first);
    assert!(fs::read_to_string(&source).unwrap().starts_with(&first));
    assert!(fs::read_to_string(&source).unwrap().len() > first.len());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn concurrent_appends_keep_every_entry_exactly_once() {
    let root = scratch("threads");
    let path = root.join("log/methods.txt");
    let threads = 8usize;
    let each = 5usize;
    std::thread::scope(|scope| {
        for t in 0..threads {
            let path = &path;
            scope.spawn(move || {
                for i in 0..each {
                    let entry =
                        format_entry("t", &format!("step{t}x{i}"), "v", "Paragraph.", &["R."]);
                    append_entry(path, &entry).unwrap();
                }
            });
        }
    });
    let text = fs::read_to_string(&path).unwrap();
    let doc = parse_entries(&text);
    assert_eq!(doc.entries.len(), threads * each);
    assert!(doc.preamble.is_empty());
    for t in 0..threads {
        for i in 0..each {
            let step = format!("step{t}x{i}");
            assert_eq!(doc.entries.iter().filter(|e| e.step == step).count(), 1);
        }
    }
    for e in &doc.entries {
        assert!(
            e.raw.ends_with("Paragraph.\nReferences:\n- R.\n\n"),
            "{}",
            e.raw
        );
    }
    assert!(!lock_path(&path).exists());
    assert!(fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .all(|e| !crate::outputs::is_partial_name(&e.file_name().to_string_lossy())));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_stale_lock_is_broken_and_a_fresh_lock_is_released_after_use() {
    let root = scratch("stale");
    let path = root.join("methods.txt");
    let lock = lock_path(&path);
    assert!(lock
        .file_name()
        .unwrap()
        .to_string_lossy()
        .ends_with("methods.txt.lock"));
    let file = fs::File::create(&lock).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(120))
        .unwrap();
    drop(file);
    append_entry(&path, &format_entry("t", "a", "v", "P.", &[])).unwrap();
    assert!(!lock.exists());
    assert_eq!(
        parse_entries(&fs::read_to_string(&path).unwrap())
            .entries
            .len(),
        1
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn waiters_that_find_the_same_stale_lock_do_not_both_proceed() {
    let root = scratch("stale_race");
    let path = root.join("methods.txt");
    let lock = lock_path(&path);
    let file = fs::File::create(&lock).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(120))
        .unwrap();
    drop(file);
    let threads = 8usize;
    let inside = std::sync::atomic::AtomicUsize::new(0);
    let overlaps = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for t in 0..threads {
            let (path, lock, inside, overlaps) = (&path, &lock, &inside, &overlaps);
            scope.spawn(move || {
                {
                    let _guard = acquire_lock(lock).unwrap();
                    if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                        overlaps.fetch_add(1, Ordering::SeqCst);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    inside.fetch_sub(1, Ordering::SeqCst);
                }
                let entry = format_entry("t", &format!("step{t}"), "v", "Paragraph.", &[]);
                append_entry(path, &entry).unwrap();
            });
        }
    });
    assert_eq!(overlaps.load(Ordering::SeqCst), 0);
    let doc = parse_entries(&fs::read_to_string(&path).unwrap());
    assert_eq!(doc.entries.len(), threads);
    for t in 0..threads {
        let step = format!("step{t}");
        assert_eq!(doc.entries.iter().filter(|e| e.step == step).count(), 1);
    }
    let names: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".break")), "{names:?}");
    assert!(!lock.exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_released_lock_removes_only_its_own_lock_file() {
    let root = scratch("token");
    let lock = root.join("methods.txt.lock");
    let guard = acquire_lock(&lock).unwrap();
    assert_eq!(fs::read_to_string(&lock).unwrap(), guard.token);
    fs::write(&lock, "another writer").unwrap();
    drop(guard);
    assert_eq!(fs::read_to_string(&lock).unwrap(), "another writer");
    fs::remove_file(&lock).unwrap();
    let guard = acquire_lock(&lock).unwrap();
    fs::remove_file(&lock).unwrap();
    drop(guard);
    assert!(!lock.exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_lock_is_broken_only_when_it_is_older_than_a_file_just_created_beside_it_by_the_limit() {
    let root = scratch("fs_clock");
    let lock = root.join("methods.txt.lock");
    let at = |offset_s: i64| {
        let file = fs::File::create(&lock).unwrap();
        let base = SystemTime::now();
        let when = if offset_s >= 0 {
            base + Duration::from_secs(offset_s.unsigned_abs())
        } else {
            base - Duration::from_secs(offset_s.unsigned_abs())
        };
        file.set_modified(when).unwrap();
    };
    for (offset, broken) in [(-20, false), (60, false), (-120, true)] {
        at(offset);
        assert_eq!(break_stale_lock(&lock), broken, "offset {offset} s");
        assert_eq!(lock.exists(), !broken, "offset {offset} s");
        assert!(!breaker_path(&lock).exists());
    }
    let names: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.contains(".probe-")), "{names:?}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_lock_that_is_not_stale_is_left_alone() {
    let root = scratch("restore");
    let lock = root.join("methods.txt.lock");
    fs::write(&lock, "held by a live writer").unwrap();
    assert!(!break_stale_lock(&lock));
    assert_eq!(fs::read_to_string(&lock).unwrap(), "held by a live writer");
    assert!(!breaker_path(&lock).exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn only_the_holder_of_the_breaker_file_breaks_and_a_left_over_breaker_is_cleared() {
    let root = scratch("breaker");
    let lock = root.join("methods.txt.lock");
    let aged = |path: &Path| {
        let file = fs::File::create(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(120))
            .unwrap();
    };
    aged(&lock);
    fs::write(breaker_path(&lock), b"").unwrap();
    assert!(!break_stale_lock(&lock));
    assert!(lock.exists());
    assert!(breaker_path(&lock).exists());
    aged(&breaker_path(&lock));
    assert!(!break_stale_lock(&lock));
    assert!(!breaker_path(&lock).exists());
    assert!(break_stale_lock(&lock));
    assert!(!lock.exists());
    assert!(!breaker_path(&lock).exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_lock_that_cannot_be_created_fails_at_once_with_the_real_error() {
    let root = scratch("nolock");
    let err = acquire_lock(&root.join("missing_dir").join("methods.txt.lock"))
        .err()
        .unwrap();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    let blocker = root.join("a_file");
    fs::write(&blocker, b"x").unwrap();
    let err = append_entry(&blocker.join("log").join("methods.txt"), "entry\n").unwrap_err();
    assert_ne!(err.kind(), io::ErrorKind::TimedOut);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_file_path_helper_agrees_with_the_derivative_path_and_the_renderer_picks_the_mode() {
    let root = scratch("helper");
    let (sub, _) = subject(&root);
    assert_eq!(
        methods_file(&sub.subj_dir, &sub.subj_id),
        derivatives::methods_log(&sub)
    );
    assert_eq!(
        methods_file(&sub.subj_dir, &format!("sub-{}", sub.subj_id)),
        derivatives::methods_log(&sub)
    );
    let a = "[t1] preprocess | Wyss DBS Localizer 1\nOne.\n\n";
    let b = "[t2] preprocess | Wyss DBS Localizer 1\nTwo.\n\n";
    let bytes = format!("note\n{a}{b}").into_bytes();
    assert_eq!(render_bytes(&bytes, false), bytes);
    assert_eq!(render_bytes(&bytes, true), b.as_bytes());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_parser_strips_a_byte_order_mark_and_handles_crlf_files() {
    let a = "[t1] preprocess | Wyss DBS Localizer 1\r\nOne.\r\n\r\n";
    let b = "[t2] coreg | Wyss DBS Localizer 1\r\nTwo.\r\n\r\n";
    let doc = parse_entries(&format!("\u{feff}{a}{b}"));
    assert!(doc.preamble.is_empty());
    assert_eq!(doc.entries.len(), 2);
    assert_eq!(doc.entries[0].step, "preprocess");
    assert_eq!(render_last(&doc), format!("{a}{b}"));
    let no_final = parse_entries("[t] vat | Wyss DBS Localizer 1\r\nText.");
    assert_eq!(
        render_last(&no_final),
        "[t] vat | Wyss DBS Localizer 1\r\nText.\r\n\r\n"
    );
}

#[test]
fn the_parser_keeps_a_hand_edited_preamble_and_last_picks_the_final_entry_per_step() {
    let a1 = format_entry("2026-01-01T00:00:00Z", "preprocess", "v", "A1.", &[]);
    let b1 = format_entry("2026-01-02T00:00:00Z", "coreg", "v", "B1.", &["R."]);
    let a2 = format_entry("2026-01-03T00:00:00Z", "preprocess", "v", "A2.", &[]);
    let text = format!("My own note.\nSecond line\n\n{a1}{b1}{a2}");
    let doc = parse_entries(&text);
    assert_eq!(doc.preamble, "My own note.\nSecond line\n\n");
    let steps: Vec<&str> = doc.entries.iter().map(|e| e.step.as_str()).collect();
    assert_eq!(steps, ["preprocess", "coreg", "preprocess"]);
    assert_eq!(doc.entries[1].timestamp, "2026-01-02T00:00:00Z");
    assert_eq!(doc.entries[0].raw, a1);
    let rebuilt: String = doc.entries.iter().map(|e| e.raw.as_str()).collect();
    assert_eq!(format!("{}{rebuilt}", doc.preamble), text);
    let last = render_last(&doc);
    assert_eq!(last, format!("{b1}{a2}"));
    assert!(!last.contains("My own note"));
    let no_newline = parse_entries("[t] vat | Wyss DBS Localizer 1\nText.");
    assert_eq!(
        render_last(&no_newline),
        "[t] vat | Wyss DBS Localizer 1\nText.\n\n"
    );
    let plain = parse_entries("only a note\n");
    assert!(plain.entries.is_empty());
    assert_eq!(render_last(&plain), "");
    assert!(parse_entries("[not a header\n[x] y z | q\n")
        .entries
        .is_empty());
}

#[test]
fn preprocess_names_the_method_and_the_engine_from_the_record() {
    let rec = json!({
        "reg_backend": "wgpu",
        "reg_adapter": {"name": "Test GPU", "backend": "Vulkan", "device_type": "DiscreteGpu"},
        "n4": {"shrink_factor": 4, "fitting_levels": [50, 50, 50]},
    });
    let (p, refs) = preprocess(&rec, Some("T1w")).unwrap();
    assert!(p.contains("pre-operative T1-weighted anchor image"), "{p}");
    assert!(p.contains("N4 bias field correction"), "{p}");
    assert!(p.contains("shrink factor of 4"), "{p}");
    assert!(p.contains("3 fitting levels"), "{p}");
    assert!(
        p.contains("(WebGPU compute through the wgpu library) on the graphics processing unit Test GPU through Vulkan."),
        "{p}"
    );
    assert!(!p.contains("graphics interface"), "{p}");
    assert_eq!(refs, vec![REF_N4]);
    let ants = preprocess(&json!({"reg_backend": "ants"}), None).unwrap().0;
    assert!(ants.contains("Advanced Normalization Tools"), "{ants}");
    assert!(!ants.contains("wgpu"), "{ants}");
    assert!(ants.starts_with("The pre-operative anchor image"), "{ants}");
}

#[test]
fn the_engine_clause_follows_the_recorded_device_type() {
    let base = |device: Option<&str>| {
        let mut adapter = json!({"name": "Some Adapter", "backend": "Vulkan"});
        if let Some(d) = device {
            adapter["device_type"] = json!(d);
        }
        json!({"reg_backend": "wgpu", "reg_adapter": adapter})
    };
    for hardware in ["DiscreteGpu", "IntegratedGpu"] {
        let p = preprocess(&base(Some(hardware)), None).unwrap().0;
        assert!(
            p.contains("on the graphics processing unit Some Adapter through Vulkan"),
            "{p}"
        );
    }
    let cpu = preprocess(&base(Some("Cpu")), None).unwrap().0;
    assert!(cpu.contains("software renderer on the processor"), "{cpu}");
    assert!(!cpu.contains("graphics processing unit"), "{cpu}");
    let unknown = preprocess(&base(None), None).unwrap().0;
    assert!(unknown.contains("wgpu library)."), "{unknown}");
    assert!(!unknown.contains("graphics processing unit"), "{unknown}");
    assert!(!unknown.contains("software renderer"), "{unknown}");
    for p in [&cpu, &unknown] {
        assert!(!p.contains(")("), "{p}");
        assert!(!p.contains(") ("), "{p}");
    }
}

#[test]
fn the_ants_version_is_stated_only_when_the_record_has_it() {
    let with = "the Advanced Normalization Tools (ANTs) programs, version 2.6.5-gfdce4d2";
    let v = json!("2.6.5-gfdce4d2");
    let p = preprocess(&json!({"reg_backend": "ants", "ants_version": v}), None)
        .unwrap()
        .0;
    assert!(p.contains(with), "{p}");
    let p = precoreg(&json!({"reg_backend": "ants", "ants_version": v}), None)
        .unwrap()
        .0;
    assert!(p.contains(with), "{p}");
    let mut coreg = coreg_record("ants", &["CT", "ax_MRI"], None);
    coreg["CT"]["ants_version"] = v.clone();
    let p = coregister(&coreg, None).unwrap().0;
    assert_eq!(p.matches("version 2.6.5-gfdce4d2").count(), 1, "{p}");
    let p = normalize(
        &json!({"method": "ants_syn", "reg_backend": "ants", "ants_version": v}),
        None,
    )
    .unwrap()
    .0;
    assert!(p.contains(with), "{p}");
    let p = brainshift(&json!({"method": "scrf", "engine": "ants", "ants_version": v}))
        .unwrap()
        .0;
    assert!(p.contains(with), "{p}");
    for p in [
        preprocess(&json!({"reg_backend": "ants"}), None).unwrap().0,
        precoreg(&json!({"reg_backend": "ants"}), None).unwrap().0,
        normalize(&json!({"method": "ants_syn", "reg_backend": "ants"}), None)
            .unwrap()
            .0,
        brainshift(&json!({"method": "scrf", "engine": "ants"}))
            .unwrap()
            .0,
    ] {
        assert!(p.contains("programs"), "{p}");
        assert!(!p.contains("version"), "{p}");
    }
    let wgpu = preprocess(&json!({"reg_backend": "wgpu", "ants_version": v}), None)
        .unwrap()
        .0;
    assert!(!wgpu.contains("version"), "{wgpu}");
}

#[test]
fn preprocess_without_a_record_claims_no_engine_or_parameter() {
    let (p, _) = preprocess(&Value::Null, None).unwrap();
    assert!(p.contains("N4 bias field correction"), "{p}");
    assert!(!p.contains("wgpu"), "{p}");
    assert!(!p.contains("Advanced Normalization Tools"), "{p}");
    assert!(!p.contains("shrink"), "{p}");
    assert!(!p.contains("fitting"), "{p}");
}

#[test]
fn precoreg_needs_a_record_and_cites_ants_only_for_the_ants_engine() {
    assert!(precoreg(&json!({}), Some("T1w")).is_none());
    let rec = json!({"reg_backend": "ants", "template_space": "MNI152NLin2009bAsym"});
    let (p, refs) = precoreg(&rec, Some("T1w")).unwrap();
    assert!(p.contains("the MNI152NLin2009bAsym template"), "{p}");
    assert!(p.contains("rigidly"), "{p}");
    assert!(p.contains("Advanced Normalization Tools"), "{p}");
    assert_eq!(refs, vec![REF_AVANTS_2011]);
    let wgpu = json!({"reg_backend": "wgpu", "template_space": "custom-abc123"});
    let (p, refs) = precoreg(&wgpu, None).unwrap();
    assert!(p.contains("a custom template"), "{p}");
    assert!(!p.contains("abc123"), "{p}");
    assert!(refs.is_empty());
    let only_space = precoreg(&json!({"template_space": "X"}), None).unwrap().0;
    assert!(
        !only_space.contains("Advanced Normalization Tools"),
        "{only_space}"
    );
    assert!(!only_space.contains("wgpu"), "{only_space}");
}

#[test]
fn coregister_names_real_image_keys_and_the_hardware_engine() {
    let adapter = json!({"name": "Test GPU", "backend": "Vulkan", "device_type": "DiscreteGpu"});
    let rec = coreg_record("wgpu", &["CT", "ax_MRI", "cor_MRI"], Some(adapter));
    let (p, refs) = coregister(&rec, Some("T1w")).unwrap();
    assert!(
        p.contains("The post-operative computed tomography (CT) image was registered to the pre-operative T1-weighted anchor image by a rigid-body registration started from several initial positions"),
        "{p}"
    );
    assert!(
        p.contains(
            "The axial post-operative magnetic resonance imaging (MRI) image was registered"
        ),
        "{p}"
    );
    assert!(
        p.contains("The coronal post-operative MRI image was registered"),
        "{p}"
    );
    assert!(
        p.contains("on the graphics processing unit Test GPU through Vulkan"),
        "{p}"
    );
    assert!(!p.contains("ax_MRI"), "{p}");
    assert!(refs.is_empty(), "{refs:?}");
    let ants = coreg_record("ants", &["CT"], None);
    let (p, refs) = coregister(&ants, None).unwrap();
    assert!(p.contains("Advanced Normalization Tools"), "{p}");
    assert_eq!(refs, vec![REF_AVANTS_2011]);
    let unknown = coreg_record("ants", &["odd_key_7"], None);
    let p = coregister(&unknown, None).unwrap().0;
    assert!(
        p.starts_with("A post-operative image was registered"),
        "{p}"
    );
    assert!(!p.contains("odd_key_7"), "{p}");
    assert!(coregister(&json!({}), None).is_none());
    assert!(coregister(&json!({"method": "x", "approval": {}}), None).is_none());
    let bare = coregister(&json!({"CT": {"method": "unknown_method"}}), None)
        .unwrap()
        .0;
    assert!(!bare.contains("rigid"), "{bare}");
    assert!(!bare.contains("wgpu"), "{bare}");
    assert!(!bare.contains("Advanced Normalization Tools"), "{bare}");
}

#[test]
fn normalize_cites_syn_for_both_engines_and_ants_only_for_ants() {
    let rec = json!({
        "method": "ants_syn",
        "engine": "wgpu",
        "reg_backend": "wgpu",
        "channels": ["T1w", "T2w"],
        "template_space": "MNI152NLin2009bAsym",
        "norm_isotropic_mm": 1.5,
    });
    let (p, refs) = normalize(&rec, Some("T1w")).unwrap();
    assert!(p.contains("T1-weighted and T2-weighted images"), "{p}");
    assert!(p.contains("multispectral"), "{p}");
    assert!(p.contains("the MNI152NLin2009bAsym template space"), "{p}");
    assert!(p.contains("symmetric diffeomorphic (SyN)"), "{p}");
    assert!(p.contains("isotropic grid of 1.5 mm"), "{p}");
    assert!(p.contains("wgpu"), "{p}");
    assert_eq!(refs, vec![REF_AVANTS]);
    let ants = json!({"method": "ants_syn", "reg_backend": "ants", "channels": ["T1w"]});
    let (single, refs) = normalize(&ants, None).unwrap();
    assert!(
        single.starts_with("The pre-operative T1-weighted anchor image was normalized"),
        "{single}"
    );
    assert!(!single.contains("multispectral"), "{single}");
    assert!(!single.contains("isotropic"), "{single}");
    assert_eq!(refs, vec![REF_AVANTS, REF_AVANTS_2011]);
    let custom = normalize(&json!({"template_space": "custom-9f8e7d"}), None)
        .unwrap()
        .0;
    assert!(custom.contains("to a custom template space"), "{custom}");
    assert!(!custom.contains("9f8e7d"), "{custom}");
    assert!(normalize(&json!({}), None).is_none());
}

#[test]
fn brainshift_reports_only_what_the_record_backs() {
    assert!(brainshift(&json!({})).is_none());
    let rec = json!({"method": "scrf", "engine": "ants", "warp": "w", "affine": "a"});
    let (p, refs) = brainshift(&rec).unwrap();
    assert!(p.contains("warp field and affine transform"), "{p}");
    assert!(p.contains("Advanced Normalization Tools"), "{p}");
    assert_eq!(refs, vec![REF_AVANTS_2011]);
    let bare = brainshift(&json!({"method": "scrf"})).unwrap();
    assert!(!bare.0.contains("warp field"), "{}", bare.0);
    assert!(
        !bare.0.contains("Advanced Normalization Tools"),
        "{}",
        bare.0
    );
    assert!(bare.1.is_empty());
}

fn reco_with_status(method: &str, right: &str, left: &str) -> Value {
    json!({
        "method": method,
        "meta": {
            "reconstruct": crate::ct_recon::reconstruct_meta(
                &[("1", right), ("2", left)], &[], &[]
            ),
            "side_source": {"left": "metal_axis", "right": "metal_axis"},
            "settings": {"el_model": "Boston Scientific Vercise Directed", "template_space": "MNI152NLin2009bAsym"},
        },
    })
}

#[test]
fn reconstruct_describes_only_the_sides_traced_in_this_run() {
    let reco = reco_with_status("trac_core_rust_native", "kept", "traced");
    let (p, refs) = reconstruct(&reco, None).unwrap();
    assert!(
        p.contains("contact positions of the left lead were reconstructed"),
        "{p}"
    );
    assert!(p.contains("TRAC/CORE"), "{p}");
    assert!(
        p.contains("For the left lead, TRAC/CORE did not succeed"),
        "{p}"
    );
    assert!(!p.contains("right lead were"), "{p}");
    assert!(!p.contains("For the right lead"), "{p}");
    assert!(
        p.contains("The manually corrected position of the right lead was kept."),
        "{p}"
    );
    assert!(p.contains("Boston Scientific Vercise Directed"), "{p}");
    assert!(p.contains("MNI152NLin2009bAsym template"), "{p}");
    assert_eq!(refs, vec![REF_HORN_KUEHN_2015]);
    let both = reco_with_status("trac_core_rust", "traced", "traced");
    let p = reconstruct(&both, None).unwrap().0;
    assert!(
        p.contains("contact positions of both leads were reconstructed"),
        "{p}"
    );
    assert!(!p.contains("kept"), "{p}");
    let failed = reco_with_status("trac_core_rust_native", "traced", "failed");
    let p = reconstruct(&failed, None).unwrap().0;
    assert!(
        p.contains("contact positions of the right lead were reconstructed"),
        "{p}"
    );
    assert!(!p.contains("left"), "{p}");
    let pacer = reconstruct(&json!({"method": "pacer_rust"}), None).unwrap();
    assert!(pacer.0.contains("PaCER"), "{}", pacer.0);
    assert!(
        !pacer.0.contains("electrode model was assumed"),
        "{}",
        pacer.0
    );
    assert!(!pacer.0.contains("Template-space"), "{}", pacer.0);
    assert_eq!(pacer.1, vec![REF_HUSCH]);
    assert!(reconstruct(&json!({}), Some("Model")).is_none());
    let fallback = reconstruct(&json!({"method": "pacer_import"}), Some("Model X")).unwrap();
    assert!(!fallback.0.contains("Model X"), "{}", fallback.0);
    assert!(
        fallback.0.contains("imported from a PaCER result"),
        "{}",
        fallback.0
    );
    assert_eq!(fallback.1, vec![REF_HUSCH]);
}

#[test]
fn an_import_cites_pacer_only_when_the_file_says_pacer_and_names_the_models_it_states() {
    let import = |method: Option<&str>, props: Value| {
        let mut reco = json!({
            "props": props,
            "meta": {
                "pacer": "import_json",
                "reconstruct": crate::ct_recon::reconstruct_meta(
                    &[("1", "traced"), ("2", "traced")], &[], &[]
                ),
                "settings": {"el_model": "Run Option Model"},
            },
        });
        if let Some(m) = method {
            reco["method"] = json!(m);
        }
        reconstruct(&reco, Some("Run Option Model")).unwrap()
    };
    for method in [
        None,
        Some(""),
        Some("pacer_import"),
        Some("pacer"),
        Some("pacer_v1"),
    ] {
        let (p, refs) = import(method, json!({}));
        assert!(
            p.contains("imported from a PaCER result"),
            "{method:?}: {p}"
        );
        assert_eq!(refs, vec![REF_HUSCH], "{method:?}");
        assert!(!p.contains("Run Option Model"), "{p}");
        assert!(!p.contains("electrode model"), "{p}");
    }
    for method in ["trac_core_rust_native", "lead_dbs_manual", "something-else"] {
        let (p, refs) = import(Some(method), json!({}));
        assert!(
            p.contains("were imported from a reconstruction that had been computed beforehand; they were not reconstructed by this software."),
            "{p}"
        );
        assert!(!p.contains("PaCER"), "{p}");
        assert!(!p.contains(method), "{p}");
        assert!(refs.is_empty(), "{refs:?}");
    }
    let same = import(
        None,
        json!({"1": {"elmodel": "Model A"}, "2": {"elmodel": "Model A"}}),
    )
    .0;
    assert!(
        same.contains("The imported file states the electrode model Model A."),
        "{same}"
    );
    let differ = import(
        None,
        json!({"1": {"elmodel": "Model A"}, "2": {"elmodel": "Model B"}}),
    )
    .0;
    assert!(
        differ.contains("Model A for the right lead and Model B for the left lead"),
        "{differ}"
    );
    let one = import(None, json!({"2": {"elmodel": "Model B"}})).0;
    assert!(one.contains("Model B for the left lead"), "{one}");
    assert!(!one.contains("right lead."), "{one}");
}

#[test]
fn a_virtual_gpu_is_not_named_as_hardware() {
    let rec = json!({"reg_backend": "wgpu", "reg_adapter": {"name": "Virtual X", "backend": "Vulkan", "device_type": "VirtualGpu"}});
    let p = preprocess(&rec, None).unwrap().0;
    assert!(p.contains("wgpu library)."), "{p}");
    assert!(!p.contains("Virtual X"), "{p}");
    assert!(!p.contains("graphics processing unit"), "{p}");
}

#[test]
fn every_citation_is_the_text_of_its_spec_reference_line() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let pairs: [(&str, &[&str]); 14] = [
        (REF_N4, &["02_preprocessing.md"]),
        (REF_AVANTS_2011, &["03_coregistration.md"]),
        (REF_AVANTS, &["04_normalization.md"]),
        (REF_HORN_KUEHN_2015, &["05_reconstruction.md"]),
        (REF_HUSCH, &["05_reconstruction.md", "10_pacer.md"]),
        (REF_HELLERBACH, &["06_orientation.md"]),
        (REF_DEMBEK_2021, &["06_orientation.md"]),
        (REF_KURTEV, &["06_orientation.md"]),
        (REF_HORN_2017, &["07_vat.md"]),
        (REF_KUNCEL, &["07_vat.md"]),
        (REF_MAEDLER, &["07_vat.md"]),
        (REF_DEMBEK_2017, &["07_vat.md"]),
        (REF_BANIASADI, &["07_vat.md"]),
        (REF_BUTENKO, &["07_vat.md"]),
    ];
    for (reference, files) in pairs {
        for file in files {
            let spec = fs::read_to_string(root.join(file))
                .unwrap_or_else(|e| panic!("{file}: {e}"))
                .replace('*', "");
            let found = spec
                .lines()
                .filter(|l| l.starts_with("- "))
                .any(|l| l.contains(reference));
            assert!(
                found,
                "{file} has no reference line containing: {reference}"
            );
        }
    }
}

#[test]
fn reconstruct_through_the_real_merge_names_the_traced_and_the_kept_lead() {
    let old = reco_with_status("trac_core_rust_native", "traced", "traced");
    let kept = crate::manual_correction::KeptSides {
        old: old.clone(),
        keys: vec!["1"],
    };
    let mut fresh = json!({
        "version": 2,
        "method": "trac_core_rust_native",
        "meta": {
            "reconstruct": crate::ct_recon::reconstruct_meta(&[("2", "traced")], &[], &[]),
            "side_source": {"left": "trac_core"},
            "settings": {"el_model": "Boston Scientific Vercise Directed"},
        },
    });
    crate::manual_correction::merge_kept_sides(&mut fresh, &kept);
    let p = reconstruct(&fresh, None).unwrap().0;
    assert!(
        p.contains("contact positions of the left lead were reconstructed"),
        "{p}"
    );
    assert!(!p.contains("metal artefact"), "{p}");
    assert!(p.contains("position of the right lead was kept"), "{p}");
    let both = crate::manual_correction::KeptSides {
        old: old.clone(),
        keys: vec!["1", "2"],
    };
    let mut all_kept = crate::stage_impl::kept_only(Some(&both));
    crate::manual_correction::merge_kept_sides(&mut all_kept, &both);
    assert!(reconstruct(&all_kept, None).is_none());
}

#[test]
fn orientation_counts_only_sides_estimated_in_this_run() {
    let reco = json!({"meta": {"orientation": {
        "1": {"status": "resolved", "method": "diode_marker_v1"},
        "2": {"status": "not_directed", "method": "diode_marker_v1"},
    }}});
    let (p, refs) = orientation(&reco).unwrap();
    assert!(p.contains("estimated for the right lead"), "{p}");
    assert!(!p.contains("left"), "{p}");
    assert!(p.contains("DiODe"), "{p}");
    assert!(p.contains("computed tomography (CT)"), "{p}");
    assert_eq!(refs, vec![REF_HELLERBACH, REF_DEMBEK_2021, REF_KURTEV]);
    let ambiguous = json!({"meta": {"orientation": {
        "1": {"status": "resolved", "method": "diode_marker_v1"},
        "2": {"status": "ambiguous", "method": "diode_marker_v1"},
    }}});
    let p = orientation(&ambiguous).unwrap().0;
    assert!(p.contains("estimated for both leads"), "{p}");
    assert!(
        p.contains("For the left lead two candidate orientations remained"),
        "{p}"
    );
    let accepted = json!({"meta": {
        "orientation": {
            "1": {"status": "resolved", "method": "diode_marker_v1"},
            "2": {"status": "resolved", "method": "diode_marker_v1"},
        },
        "manual_correction": {"1": {"roll_review": "accepted"}, "2": {"roll_review": null}},
    }});
    let p = orientation(&accepted).unwrap().0;
    assert!(p.contains("estimated for the left lead"), "{p}");
    assert!(!p.contains("right"), "{p}");
    let all_kept = json!({"meta": {
        "orientation": {"1": {"status": "resolved", "method": "diode_marker_v1"}},
        "manual_correction": {"1": {"roll_review": "set"}},
    }});
    assert!(orientation(&all_kept).is_none());
    let none = json!({"meta": {"orientation": {
        "1": {"status": "failed", "method": "diode_marker_v1"},
        "2": {"status": "manual", "method": "manual"},
    }}});
    assert!(orientation(&none).is_none());
    assert!(orientation(&json!({})).is_none());
    let unresolved = json!({"meta": {"orientation": crate::ct_recon::ORIENTATION_UNRESOLVED}});
    assert!(orientation(&unresolved).is_none());
}

#[test]
fn vat_names_models_engine_and_a_rounded_threshold() {
    let sidecars = vec![
        json!({"model": "simbio", "threshold_v_per_m": 70.00000000000001, "engine": {"backend": "wgpu"}}),
        json!({"model": "kuncel", "engine": {"backend": "analytic"}}),
    ];
    let (p, refs) = vat(&sidecars).unwrap();
    assert!(p.contains("2 stimulation settings"), "{p}");
    assert!(p.contains("electric-field model"), "{p}");
    assert!(
        p.contains("in-process engine (WebGPU compute through the wgpu library)"),
        "{p}"
    );
    assert!(!p.contains("graphics processing unit"), "{p}");
    assert!(p.contains("threshold of 70 V/m"), "{p}");
    let named =
        vat(&[json!({"model": "simbio", "engine": {"backend": "wgpu", "adapter": "NVIDIA X"}})])
            .unwrap()
            .0;
    assert!(!named.contains("graphics processing unit"), "{named}");
    let cpu = vat(&[json!({"model": "simbio", "engine": {"backend": "cpu"}})])
        .unwrap()
        .0;
    assert!(
        cpu.contains("solved with the central processing unit"),
        "{cpu}"
    );
    assert!(p.contains("Kuncel et al. (2008)"), "{p}");
    assert_eq!(refs, vec![REF_HORN_2017, REF_KUNCEL]);
    let small = vat(&[json!({"model": "simbio", "threshold_v_per_m": 0.2000000001})])
        .unwrap()
        .0;
    assert!(small.contains("threshold of 0.2 V/m"), "{small}");
    let bare = vat(&[json!({"model": "simbio"})]).unwrap().0;
    assert!(bare.contains("1 stimulation setting "), "{bare}");
    assert!(!bare.contains("processing unit"), "{bare}");
    assert!(!bare.contains("V/m"), "{bare}");
    assert!(vat(&[json!({"model": "mystery"})]).is_none());
    assert!(vat(&[]).is_none());
}

#[test]
fn a_basis_taken_from_the_cache_is_never_described_as_solved_in_this_run() {
    let cpu_hit = json!({"model": "simbio", "engine": {
        "backend": "cpu",
        "basis_cache": "hit",
        "backends": [{"side": "right", "backend": "cpu", "adapter_info": null, "basis_cache": "hit"}],
    }});
    let p = vat(&[cpu_hit]).unwrap().0;
    assert!(
        p.contains("solved with a stored basis from an earlier computation"),
        "{p}"
    );
    assert!(!p.contains("central processing unit"), "{p}");
    let old_summary =
        json!({"model": "simbio", "engine": {"backend": "cpu", "basis_cache": "hit"}});
    let p = vat(&[old_summary]).unwrap().0;
    assert!(p.contains("stored basis"), "{p}");
    assert!(!p.contains("central processing unit"), "{p}");
    let mixed = json!({"model": "simbio", "engine": {
        "backend": "cpu",
        "basis_cache": "built",
        "backends": [
            {"side": "right", "backend": "cpu", "adapter_info": null, "basis_cache": "hit"},
            {"side": "left", "backend": "cpu", "adapter_info": null, "basis_cache": "built"},
        ],
    }});
    let p = vat(&[mixed]).unwrap().0;
    assert!(
        p.contains("a stored basis from an earlier computation for the right lead and with the central processing unit for the left lead"),
        "{p}"
    );
    let partly = json!({"model": "simbio", "engine": {
        "backend": "wgpu",
        "basis_cache": "built",
        "backends": [{"side": "right", "backend": "wgpu", "basis_cache": "built", "cached_contacts": 2,
            "adapter_info": {"name": "Test GPU", "backend": "Vulkan", "device_type": "DiscreteGpu"}}],
    }});
    let p = vat(&[partly]).unwrap().0;
    assert!(
        p.contains("solved partly from a stored basis, with the in-process engine (WebGPU compute through the wgpu library) on the graphics processing unit Test GPU through Vulkan"),
        "{p}"
    );
    assert!(
        p.contains("a quasi-static volume-conductor electric-field model computed with this software's own solver, following Horn et al. (2017)"),
        "{p}"
    );
}

#[test]
fn manual_correction_names_only_what_changed() {
    let (p, refs) = manual_correction(
        "native",
        &[side("1", true, true, false), side("2", false, false, true)],
    )
    .unwrap();
    assert!(p.contains("native image space"), "{p}");
    assert!(
        p.contains("For the right lead, the head and tail markers were moved."),
        "{p}"
    );
    assert!(
        p.contains("For the left lead, the rotation (roll) angle was set."),
        "{p}"
    );
    assert_eq!(refs, vec![REF_HORN_KUEHN_2015]);
    let (p, _) = manual_correction("mni", &[side("2", true, false, true)]).unwrap();
    assert!(
        p.contains(
            "For the left lead, the head marker was moved and the rotation (roll) angle was set."
        ),
        "{p}"
    );
    assert!(!p.contains("right"), "{p}");
    assert!(!p.contains("tail"), "{p}");
    let (p, _) = manual_correction("mni", &[side("1", false, true, false)]).unwrap();
    assert!(p.contains("the tail marker was moved"), "{p}");
    assert!(!p.contains("roll"), "{p}");
    let (p, _) = manual_correction(
        "native",
        &[
            side("1", false, false, false),
            side("2", false, false, true),
        ],
    )
    .unwrap();
    assert!(!p.contains("right"), "{p}");
    assert!(manual_correction("native", &[side("1", false, false, false)]).is_none());
    assert!(manual_correction("native", &[]).is_none());
}

#[test]
fn record_stage_appends_one_entry_per_call_from_the_records() {
    let root = scratch("record");
    let (sub, opts) = subject(&root);
    write_json(
        &derivatives::coreg_method_log(&sub),
        &coreg_record("ants", &["ax_MRI"], None),
    );
    assert!(record_stage(&sub, &opts, Stage::Coregister, &[]).unwrap());
    assert!(record_stage(&sub, &opts, Stage::Coregister, &[]).unwrap());
    let path = derivatives::methods_log(&sub);
    let all = entries(&path);
    assert_eq!(all.len(), 2);
    assert!(
        all[0].contains("] coreg | Wyss DBS Localizer "),
        "{}",
        all[0]
    );
    assert!(all[0].contains("\nReferences:\n- Avants BB"), "{}", all[0]);
    assert!(fs::read_to_string(&path).unwrap().ends_with("\n\n"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn record_stage_writes_nothing_without_a_method_or_for_bids_and_visualize() {
    let root = scratch("none");
    let (sub, opts) = subject(&root);
    for stage in [
        Stage::Bids,
        Stage::Visualize,
        Stage::Precoreg,
        Stage::Coregister,
        Stage::Normalize,
        Stage::Brainshift,
        Stage::Reconstruct,
        Stage::Orientation,
        Stage::Vat,
    ] {
        assert!(!record_stage(&sub, &opts, stage, &[]).unwrap(), "{stage:?}");
    }
    assert!(!derivatives::methods_log(&sub).exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn record_stage_describes_exactly_the_computed_vat_labels() {
    let root = scratch("vat");
    let (sub, opts) = subject(&root);
    let dir = crate::vat::settings::stimulations_dir(&sub.subj_dir).join("native");
    let name = format!("sub-{}_desc-vta.json", sub.subj_id);
    write_json(&dir.join("old").join(&name), &json!({"model": "kuncel"}));
    assert!(!record_stage(&sub, &opts, Stage::Vat, &[]).unwrap());
    write_json(&dir.join("new").join(&name), &json!({"model": "maedler"}));
    assert!(!record_stage(&sub, &opts, Stage::Vat, &["missing".to_owned()]).unwrap());
    assert!(record_stage(&sub, &opts, Stage::Vat, &["new".to_owned()]).unwrap());
    let text = fs::read_to_string(derivatives::methods_log(&sub)).unwrap();
    assert!(text.contains("1 stimulation setting "), "{text}");
    assert!(text.contains("Mädler"), "{text}");
    assert!(!text.contains("Kuncel et al. (2008)"), "{text}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn record_manual_correction_appends_one_entry_and_skips_unchanged_sides() {
    let root = scratch("manual");
    let (sub, _) = subject(&root);
    assert!(record_manual_correction(&sub, "native", &[side("1", true, true, false)]).unwrap());
    let text = fs::read_to_string(derivatives::methods_log(&sub)).unwrap();
    assert!(
        text.contains("] manual-correction | Wyss DBS Localizer "),
        "{text}"
    );
    assert!(!record_manual_correction(&sub, "native", &[]).unwrap());
    assert!(!record_manual_correction(&sub, "native", &[side("1", false, false, false)]).unwrap());
    assert_eq!(entries(&derivatives::methods_log(&sub)).len(), 1);
    let _ = fs::remove_dir_all(&root);
}
