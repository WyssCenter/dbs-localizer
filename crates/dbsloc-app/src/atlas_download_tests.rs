// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

#![allow(clippy::unwrap_used)]

use super::*;
use std::time::{Duration, Instant};

use dbsloc_pipeline::atlas_fetch::{
    AtlasErrorKind, AtlasManifest, ManifestAtlas, ManifestFile, PackagePin,
};
use dbsloc_pipeline::connectome_fetch::INSTALL_RECORD;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("dbsloc_atlas_dl_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(
            root.join("resources")
                .join("atlases")
                .join("leaddbs")
                .join("Bundled Atlas"),
        )
        .unwrap();
        Fixture { root }
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn bundled(&self) -> PathBuf {
        self.root.join("resources").join("atlases").join("leaddbs")
    }

    fn install(&self, name: &str, record: bool) {
        let dir = atlas_fetch::downloaded_atlases_dir(&self.data()).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("atlas_index.mat"), b"x").unwrap();
        if record {
            std::fs::write(dir.join(INSTALL_RECORD), b"{}").unwrap();
        }
    }

    fn lister(&self, with_data: bool) -> Lister {
        let data = self.data();
        let bundled = self.bundled();
        Box::new(move || {
            let data_root = with_data.then(|| data.clone());
            Listing {
                rows: Ok(atlas_fetch::list_atlases_in(
                    &manifest(),
                    Some(bundled.as_path()),
                    data_root.as_deref(),
                )),
                data_root,
            }
        })
    }

    fn window(&self) -> AtlasDownloadWindow {
        let mut w = AtlasDownloadWindow::with_lister(self.lister(true));
        w.open();
        w
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn atlas(name: &str, bytes: u64) -> ManifestAtlas {
    ManifestAtlas {
        name: name.to_owned(),
        bytes,
        files: vec![ManifestFile {
            path: "atlas_index.mat".to_owned(),
            bytes,
            sha256: "0".repeat(64),
        }],
    }
}

fn manifest() -> AtlasManifest {
    AtlasManifest {
        package: PackagePin {
            file_name: "leaddbs_data.zip".to_owned(),
            url: "http://127.0.0.1:1/leaddbs_data.zip".to_owned(),
            bytes: 1,
            sha1: "0".repeat(40),
            last_modified: String::new(),
            central_directory_offset: 0,
            atlas_prefix: "atlases/".to_owned(),
            pinned: String::new(),
        },
        atlases: vec![
            atlas("Alpha Atlas (Doe 2020)", 400_000),
            atlas("Bundled Atlas", 2_000_000),
            atlas("Copied Atlas", 30_000_000),
            atlas("Downloaded Atlas", 812_000_000),
        ],
    }
}

fn row<'a>(w: &'a AtlasDownloadWindow, name: &str) -> &'a AtlasStatus {
    w.rows().iter().find(|r| r.name == name).unwrap()
}

fn action(w: &AtlasDownloadWindow, name: &str) -> RowAction {
    w.row_action(row(w, name))
}

fn wait(w: &mut AtlasDownloadWindow) {
    let end = Instant::now() + Duration::from_secs(5);
    while (w.downloading().is_some() || w.removing().is_some()) && Instant::now() < end {
        w.poll();
        thread::sleep(Duration::from_millis(5));
    }
    assert!(w.downloading().is_none());
    assert!(w.removing().is_none());
}

fn wait_for_cancel(cancel: &AtomicBool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !cancel.load(Ordering::SeqCst) && Instant::now() < end {
        thread::sleep(Duration::from_millis(5));
    }
}

fn blocked_job(
    cancel: &AtomicBool,
    _e: &mut dyn FnMut(AtlasEvent),
) -> Result<PathBuf, AtlasFetchError> {
    wait_for_cancel(cancel);
    Err(AtlasFetchError::cancelled())
}

#[test]
fn rows_offer_the_action_that_fits_their_state() {
    let fx = Fixture::new("actions");
    fx.install("Copied Atlas", false);
    fx.install("Downloaded Atlas", true);
    let w = fx.window();
    assert_eq!(action(&w, "Bundled Atlas"), RowAction::Nothing);
    assert_eq!(row(&w, "Bundled Atlas").state.label(), "bundled");
    assert_eq!(
        action(&w, "Alpha Atlas (Doe 2020)"),
        RowAction::Download { enabled: true }
    );
    assert_eq!(action(&w, "Copied Atlas"), RowAction::NotRemovable);
    assert_eq!(
        action(&w, "Downloaded Atlas"),
        RowAction::Remove { enabled: true }
    );
}

#[test]
fn without_a_data_folder_every_download_is_disabled() {
    let fx = Fixture::new("nodata");
    let mut w = AtlasDownloadWindow::with_lister(fx.lister(false));
    w.open();
    assert!(!w.has_data_root());
    assert_eq!(
        action(&w, "Alpha Atlas (Doe 2020)"),
        RowAction::Download { enabled: false }
    );
    w.start_download("Alpha Atlas (Doe 2020)", &egui::Context::default());
    assert!(w.downloading().is_none());
    assert_eq!(
        w.error_for("Alpha Atlas (Doe 2020)"),
        Some(no_data_dir_message().as_str())
    );
    assert!(no_data_dir_message().contains("DBSLOC_DATA_DIR"));
}

#[test]
fn a_listing_error_is_shown_and_blocks_downloads() {
    let mut w = AtlasDownloadWindow::with_lister(Box::new(|| Listing {
        rows: Err("the atlas manifest is invalid".to_owned()),
        data_root: Some(PathBuf::from("/data")),
    }));
    w.open();
    assert!(w.rows().is_empty());
    assert_eq!(
        w.list_error.as_deref(),
        Some("The atlas manifest is invalid.")
    );
    assert!(!w.can_download());
}

#[test]
fn the_filter_matches_part_of_a_name_ignoring_case() {
    let fx = Fixture::new("filter");
    let mut w = fx.window();
    assert_eq!(w.visible_rows().len(), 4);
    w.set_filter("  DOE ");
    let names: Vec<&str> = w.visible_rows().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Alpha Atlas (Doe 2020)"]);
    w.set_filter("atlas");
    assert_eq!(w.visible_rows().len(), 4);
    w.set_filter("nothing like this");
    assert!(w.visible_rows().is_empty());
}

#[test]
fn progress_then_success_marks_the_row_installed() {
    let fx = Fixture::new("success");
    let mut w = fx.window();
    let name = "Alpha Atlas (Doe 2020)";
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let data = fx.data();
    w.spawn(
        name,
        move |_cancel, on_event| {
            on_event(AtlasEvent::Started {
                name: "Alpha Atlas (Doe 2020)".to_owned(),
                total_bytes: 400_000,
                files: 1,
            });
            on_event(AtlasEvent::Progress {
                downloaded: 100_000,
                total: 400_000,
            });
            on_event(AtlasEvent::Note {
                message: "the Lead-DBS data package changed on today".to_owned(),
            });
            let _ = go_rx.recv();
            on_event(AtlasEvent::Installing);
            let dir = atlas_fetch::downloaded_atlases_dir(&data).join("Alpha Atlas (Doe 2020)");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(INSTALL_RECORD), b"{}").unwrap();
            on_event(AtlasEvent::Done { path: dir.clone() });
            Ok(dir)
        },
        None,
    );
    assert_eq!(action(&w, name), RowAction::Cancel);
    assert_eq!(
        action(&w, "Copied Atlas"),
        RowAction::Download { enabled: false }
    );
    let end = Instant::now() + Duration::from_secs(5);
    while w.note().is_none() && Instant::now() < end {
        w.poll();
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        w.progress(),
        Some(Progress {
            downloaded: 100_000,
            total: 400_000,
            installing: false
        })
    );
    assert_eq!(
        w.note(),
        Some("The Lead-DBS data package changed on today.")
    );
    go_tx.send(()).unwrap();
    wait(&mut w);
    assert!(w.is_freshly_installed(name));
    assert_eq!(w.error_for(name), None);
    assert_eq!(action(&w, name), RowAction::Remove { enabled: true });
    assert_eq!(
        action(&w, "Copied Atlas"),
        RowAction::Download { enabled: true }
    );
}

#[test]
fn a_retry_is_reported_while_the_download_continues() {
    let fx = Fixture::new("retry");
    let mut w = fx.window();
    w.spawn(
        "Copied Atlas",
        |cancel, on_event| {
            on_event(AtlasEvent::Retry {
                attempt: 2,
                wait_secs: 10,
                reason: "connection reset".to_owned(),
            });
            blocked_job(cancel, on_event)
        },
        None,
    );
    let end = Instant::now() + Duration::from_secs(5);
    while w.note().is_none() && Instant::now() < end {
        w.poll();
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        w.note(),
        Some("Download paused (connection reset); retry 2 in 10 seconds.")
    );
    w.cancel();
    wait(&mut w);
}

#[test]
fn an_error_stays_in_its_row_until_the_next_attempt() {
    let fx = Fixture::new("error");
    let mut w = fx.window();
    let name = "Copied Atlas";
    w.spawn(
        name,
        |_c, _e| {
            Err(AtlasFetchError::new(
                AtlasErrorKind::NoPartial,
                atlas_fetch::NO_PARTIAL,
            ))
        },
        None,
    );
    wait(&mut w);
    assert_eq!(
        w.error_for(name),
        Some("The host does not support partial downloads.")
    );
    assert!(!w.is_freshly_installed(name));
    assert_eq!(action(&w, name), RowAction::Download { enabled: true });
    w.spawn(name, blocked_job, None);
    assert_eq!(w.error_for(name), None);
    w.cancel();
    wait(&mut w);
}

#[test]
fn cancel_leaves_no_error_and_no_installed_note() {
    let fx = Fixture::new("cancel");
    let mut w = fx.window();
    let name = "Copied Atlas";
    w.spawn(name, blocked_job, None);
    w.cancel();
    wait(&mut w);
    assert_eq!(w.error_for(name), None);
    assert!(!w.is_freshly_installed(name));
    assert_eq!(action(&w, name), RowAction::Download { enabled: true });
}

#[test]
fn any_error_after_cancel_is_not_shown() {
    let fx = Fixture::new("cancel_err");
    let mut w = fx.window();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    w.spawn(
        "Copied Atlas",
        move |_c, _e| {
            let _ = go_rx.recv();
            Err(AtlasFetchError::new(
                AtlasErrorKind::Host,
                "connection reset",
            ))
        },
        None,
    );
    w.cancel();
    go_tx.send(()).unwrap();
    wait(&mut w);
    assert_eq!(w.error_for("Copied Atlas"), None);
}

#[test]
fn a_success_that_beat_the_cancel_is_reported_as_installed() {
    let fx = Fixture::new("late_ok");
    let mut w = fx.window();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    w.spawn(
        "Copied Atlas",
        move |_c, _e| {
            let _ = go_rx.recv();
            Ok(PathBuf::from("/data/atlases/leaddbs/Copied Atlas"))
        },
        None,
    );
    w.cancel();
    go_tx.send(()).unwrap();
    wait(&mut w);
    assert!(w.is_freshly_installed("Copied Atlas"));
}

#[test]
fn only_one_download_runs_at_a_time() {
    let fx = Fixture::new("one");
    let mut w = fx.window();
    w.spawn("Copied Atlas", blocked_job, None);
    w.spawn(
        "Alpha Atlas (Doe 2020)",
        |_c, _e| Ok(PathBuf::from("/x")),
        None,
    );
    w.start_download("Alpha Atlas (Doe 2020)", &egui::Context::default());
    assert_eq!(w.downloading(), Some("Copied Atlas"));
    assert_eq!(
        action(&w, "Alpha Atlas (Doe 2020)"),
        RowAction::Download { enabled: false }
    );
    w.cancel();
    wait(&mut w);
    assert!(!w.is_freshly_installed("Alpha Atlas (Doe 2020)"));
}

#[test]
fn a_vanished_worker_is_reported_as_an_error() {
    let fx = Fixture::new("panic");
    let mut w = fx.window();
    w.spawn(
        "Copied Atlas",
        |_c, _e| -> Result<PathBuf, AtlasFetchError> { panic!("worker failed") },
        None,
    );
    wait(&mut w);
    assert_eq!(
        w.error_for("Copied Atlas"),
        Some("The download stopped unexpectedly.")
    );
}

#[test]
fn dropping_the_window_cancels_the_download() {
    let fx = Fixture::new("drop");
    let flag = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&flag);
    let mut w = fx.window();
    w.spawn(
        "Copied Atlas",
        move |cancel, _e| {
            wait_for_cancel(cancel);
            seen.store(cancel.load(Ordering::SeqCst), Ordering::SeqCst);
            Err(AtlasFetchError::cancelled())
        },
        None,
    );
    drop(w);
    let end = Instant::now() + Duration::from_secs(5);
    while !flag.load(Ordering::SeqCst) && Instant::now() < end {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn remove_deletes_a_downloaded_atlas_and_the_row_becomes_available() {
    let fx = Fixture::new("remove");
    fx.install("Downloaded Atlas", true);
    let mut w = fx.window();
    let data = fx.data();
    w.spawn_remove(
        "Downloaded Atlas",
        move || atlas_fetch::remove_atlas("Downloaded Atlas", &data, None),
        None,
    );
    assert_eq!(w.removing(), Some("Downloaded Atlas"));
    wait(&mut w);
    assert!(!atlas_fetch::downloaded_atlases_dir(&fx.data())
        .join("Downloaded Atlas")
        .exists());
    assert_eq!(w.error_for("Downloaded Atlas"), None);
    assert_eq!(
        action(&w, "Downloaded Atlas"),
        RowAction::Download { enabled: true }
    );
}

#[test]
fn a_refused_removal_shows_the_reason_in_the_row() {
    let fx = Fixture::new("remove_refused");
    fx.install("Copied Atlas", false);
    let mut w = fx.window();
    let data = fx.data();
    w.spawn_remove(
        "Copied Atlas",
        move || atlas_fetch::remove_atlas("Copied Atlas", &data, None),
        None,
    );
    wait(&mut w);
    let err = w.error_for("Copied Atlas").unwrap();
    assert!(
        err.contains("was not downloaded by Wyss DBS Localizer"),
        "{err}"
    );
    assert!(atlas_fetch::downloaded_atlases_dir(&fx.data())
        .join("Copied Atlas")
        .is_dir());
}

#[test]
fn a_second_removal_waits_for_the_first() {
    let fx = Fixture::new("remove_two");
    fx.install("Downloaded Atlas", true);
    let mut w = fx.window();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    w.spawn_remove(
        "Downloaded Atlas",
        move || {
            let _ = go_rx.recv();
            Ok(PathBuf::from("/x"))
        },
        None,
    );
    assert_eq!(
        action(&w, "Downloaded Atlas"),
        RowAction::Remove { enabled: false }
    );
    w.spawn_remove("Other", || Ok(PathBuf::from("/y")), None);
    assert_eq!(w.removing(), Some("Downloaded Atlas"));
    go_tx.send(()).unwrap();
    wait(&mut w);
}

#[test]
fn sizes_keep_small_atlases_readable() {
    assert_eq!(atlas_size(400_000), "0.4 MB");
    assert_eq!(atlas_size(30_000_000), "30 MB");
    assert_eq!(atlas_size(812_000_000), "812 MB");
    assert_eq!(atlas_size(1_500_000_000), "1.5 GB");
}

fn draw(w: &mut AtlasDownloadWindow, size: egui::Vec2) {
    let ctx = egui::Context::default();
    for _ in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| w.show(ui.ctx()))
            .textures_delta
            .clear();
    }
}

#[test]
fn the_window_draws_every_row_state_at_wide_and_narrow_sizes() {
    let fx = Fixture::new("draw");
    fx.install("Copied Atlas", false);
    fx.install("Downloaded Atlas", true);
    for size in [egui::vec2(1400.0, 900.0), egui::vec2(420.0, 700.0)] {
        let mut w = fx.window();
        w.errors.insert(
            "Downloaded Atlas".to_owned(),
            "Something failed.".to_owned(),
        );
        w.installed.insert("Downloaded Atlas".to_owned());
        w.spawn(
            "Alpha Atlas (Doe 2020)",
            |cancel, on_event| {
                on_event(AtlasEvent::Progress {
                    downloaded: 1,
                    total: 2,
                });
                blocked_job(cancel, on_event)
            },
            None,
        );
        draw(&mut w, size);
        assert!(w.is_open());
        w.cancel();
        wait(&mut w);
    }
}

fn frame(
    ctx: &egui::Context,
    w: &mut AtlasDownloadWindow,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        events,
        ..Default::default()
    };
    ctx.run_ui(input, |ui| w.show(ui.ctx()))
        .textures_delta
        .clear();
}

fn close_button_center(ctx: &egui::Context) -> egui::Pos2 {
    let rect = ctx
        .memory(|m| m.area_rect(egui::Id::new("atlas_download_window")))
        .unwrap();
    let style = ctx.global_style();
    let margin = style.spacing.window_margin;
    let heading = ctx.fonts_mut(|f| f.row_height(&egui::TextStyle::Heading.resolve(&style)));
    egui::pos2(
        rect.max.x - margin.rightf() - heading / 2.0,
        rect.min.y + margin.topf() + heading / 2.0,
    )
}

fn click(ctx: &egui::Context, w: &mut AtlasDownloadWindow, size: egui::Vec2, pos: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(ctx, w, size, vec![egui::Event::PointerMoved(pos)]);
    frame(ctx, w, size, vec![button(true)]);
    frame(ctx, w, size, vec![button(false)]);
    frame(ctx, w, size, Vec::new());
}

#[test]
fn the_close_button_hides_the_window_and_the_download_keeps_running() {
    let fx = Fixture::new("close");
    for size in [egui::vec2(1400.0, 900.0), egui::vec2(420.0, 700.0)] {
        let ctx = egui::Context::default();
        let mut w = fx.window();
        w.spawn("Copied Atlas", blocked_job, None);
        for _ in 0..3 {
            frame(&ctx, &mut w, size, Vec::new());
        }
        assert!(w.is_open());
        click(&ctx, &mut w, size, close_button_center(&ctx));
        assert!(!w.is_open());
        assert_eq!(w.downloading(), Some("Copied Atlas"));
        frame(&ctx, &mut w, size, Vec::new());
        assert!(!w.is_open());
        assert_eq!(w.downloading(), Some("Copied Atlas"));
        w.open();
        assert!(w.is_open());
        assert_eq!(w.downloading(), Some("Copied Atlas"));
        w.cancel();
        wait(&mut w);
    }
}

#[test]
fn a_running_removal_disables_every_download_and_says_why() {
    let fx = Fixture::new("remove_blocks");
    fx.install("Downloaded Atlas", true);
    let mut w = fx.window();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    w.spawn_remove(
        "Downloaded Atlas",
        move || {
            let _ = go_rx.recv();
            Ok(PathBuf::from("/x"))
        },
        None,
    );
    assert_eq!(
        action(&w, "Copied Atlas"),
        RowAction::Download { enabled: false }
    );
    assert_eq!(
        w.download_hover(row(&w, "Copied Atlas")),
        "An atlas is being removed; wait until the removal has finished"
    );
    w.spawn("Copied Atlas", |_c, _e| Ok(PathBuf::from("/y")), None);
    w.start_download("Copied Atlas", &egui::Context::default());
    assert_eq!(w.downloading(), None);
    go_tx.send(()).unwrap();
    wait(&mut w);
    assert_eq!(
        action(&w, "Copied Atlas"),
        RowAction::Download { enabled: true }
    );
}

#[test]
fn a_running_download_disables_every_remove_and_says_why() {
    let fx = Fixture::new("download_blocks");
    fx.install("Downloaded Atlas", true);
    let mut w = fx.window();
    w.spawn("Copied Atlas", blocked_job, None);
    assert_eq!(
        action(&w, "Downloaded Atlas"),
        RowAction::Remove { enabled: false }
    );
    assert_eq!(
        w.remove_hover(),
        "An atlas is downloading; wait until the download has finished"
    );
    w.spawn_remove("Downloaded Atlas", || Ok(PathBuf::from("/y")), None);
    w.start_remove("Downloaded Atlas", &egui::Context::default());
    assert_eq!(w.removing(), None);
    w.cancel();
    wait(&mut w);
    assert_eq!(
        action(&w, "Downloaded Atlas"),
        RowAction::Remove { enabled: true }
    );
    assert_eq!(
        w.remove_hover(),
        "Delete this downloaded atlas from this computer"
    );
    assert!(atlas_fetch::downloaded_atlases_dir(&fx.data())
        .join("Downloaded Atlas")
        .is_dir());
}

#[test]
fn a_busy_lock_reads_as_an_atlas_operation_in_the_row() {
    let fx = Fixture::new("busy");
    fx.install("Downloaded Atlas", true);
    let mut w = fx.window();
    w.spawn_remove(
        "Downloaded Atlas",
        || {
            Err(AtlasFetchError::new(
                AtlasErrorKind::Busy,
                "another download is running (PPN Atlas); wait for it to finish",
            ))
        },
        None,
    );
    wait(&mut w);
    assert_eq!(
        w.error_for("Downloaded Atlas"),
        Some(concat!(
            "Another atlas download or removal is running on this computer; ",
            "try again when it has finished. ",
            "Details: Another download is running (PPN Atlas); wait for it to finish."
        ))
    );
    let text = w.error_for("Downloaded Atlas").unwrap();
    assert!(!text.contains("connectome"));
    assert!(!text.contains("  "), "{text:?}");
}

#[test]
fn messages_have_no_runs_of_spaces() {
    let busy = error_text(&AtlasFetchError::new(AtlasErrorKind::Busy, "x"));
    for text in [no_data_dir_message(), busy, INSTALLED_NOTE.to_owned()] {
        assert!(!text.contains("  "), "{text:?}");
        assert!(!text.contains('\n'), "{text:?}");
    }
    assert_eq!(
        no_data_dir_message(),
        "No per-user data folder is available, so atlases cannot be downloaded. Set the \
         DBSLOC_DATA_DIR environment variable to a folder you can write to and start Wyss DBS \
         Localizer again."
    );
}

#[test]
fn the_progress_text_counts_downloaded_data() {
    let p = Progress {
        downloaded: 100_000,
        total: 400_000,
        installing: false,
    };
    let (frac, text) = progress_text("Alpha", &p);
    assert!((frac - 0.25).abs() < 1e-6);
    assert_eq!(text, "Alpha: 0.1 MB of 0.4 MB downloaded");
    let (frac, text) = progress_text(
        "Alpha",
        &Progress {
            installing: true,
            ..p
        },
    );
    assert_eq!(frac, 1.0);
    assert_eq!(text, "Alpha: installing…");
}

#[test]
fn the_shared_window_opens_from_the_context_and_reports_its_download() {
    let fx = Fixture::new("shared");
    let ctx = egui::Context::default();
    install(&ctx, AtlasDownloadWindow::with_lister(fx.lister(true)));
    assert_eq!(running_download(&ctx), None);
    open_window(&ctx);
    let s = shared(&ctx);
    assert!(lock(&s).is_open());
    lock(&s).spawn("Copied Atlas", blocked_job, None);
    assert_eq!(running_download(&ctx).as_deref(), Some("Copied Atlas"));
    lock(&s).cancel();
    wait(&mut lock(&s));
    assert_eq!(running_download(&ctx), None);
}
