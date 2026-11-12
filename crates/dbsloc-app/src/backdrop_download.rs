// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use dbsloc_pipeline::backdrop_catalog::{self, BackdropEntry};
use dbsloc_pipeline::backdrop_fetch::{self, BackdropEvent};
use dbsloc_pipeline::backdrops::{BackdropKind, BackdropOption};
use dbsloc_pipeline::template_catalog;
use dbsloc_pipeline::template_fetch::CANCELLED;
use dbsloc_viz::BackdropChoice;
use eframe::egui;

use crate::panels::{callout, em, palette};
use crate::template_picker::size_label;

pub const CUSTOM_PREFIX: &str = "custom:";

pub fn kind_name(kind: BackdropKind) -> &'static str {
    match kind {
        BackdropKind::Template => "template",
        BackdropKind::Patient => "patient",
        BackdropKind::Download => "download",
        BackdropKind::Custom => "custom",
    }
}

pub fn custom_id(path: &Path) -> String {
    format!("{CUSTOM_PREFIX}{}", path.to_string_lossy())
}

fn custom_choice(path: &Path) -> BackdropChoice {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    BackdropChoice {
        id: custom_id(path),
        label: format!("{name} (chosen file)"),
        path: Some(path.to_path_buf()),
        kind: "custom".to_owned(),
        detail: None,
    }
}

pub fn backdrop_choices(
    options: &[BackdropOption],
    entries: &[&BackdropEntry],
    persisted_id: Option<&str>,
) -> Vec<BackdropChoice> {
    backdrop_choices_with(
        options,
        entries,
        persisted_id,
        &backdrop_catalog::installed_path,
    )
}

pub fn backdrop_choices_with(
    options: &[BackdropOption],
    entries: &[&BackdropEntry],
    persisted_id: Option<&str>,
    installed: &dyn Fn(&BackdropEntry) -> Option<PathBuf>,
) -> Vec<BackdropChoice> {
    let mut out: Vec<BackdropChoice> = options
        .iter()
        .map(|o| BackdropChoice {
            id: o.id.clone(),
            label: o.label.clone(),
            path: Some(o.path.clone()),
            kind: kind_name(o.kind).to_owned(),
            detail: None,
        })
        .collect();
    for e in entries {
        if options.iter().any(|o| o.id == e.id) {
            continue;
        }
        if let Some(path) = installed(e) {
            out.push(BackdropChoice {
                id: e.id.to_owned(),
                label: e.title.to_owned(),
                path: Some(path),
                kind: "download".to_owned(),
                detail: None,
            });
            continue;
        }
        out.push(BackdropChoice {
            id: e.id.to_owned(),
            label: e.title.to_owned(),
            path: None,
            kind: "download".to_owned(),
            detail: Some(format!("download ({})", size_label(e.size))),
        });
    }
    if let Some(path) = persisted_id.and_then(|id| id.strip_prefix(CUSTOM_PREFIX)) {
        let path = Path::new(path);
        if path.is_file() {
            out.push(custom_choice(path));
        }
    }
    out
}

pub enum Outcome {
    Installed { id: String },
}

enum Msg {
    Event(BackdropEvent),
    Finished(Result<PathBuf, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Verifying,
    Cropping,
}

struct Active {
    id: String,
    cancelled: bool,
    rx: mpsc::Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    downloaded: u64,
    total: u64,
    phase: Phase,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub struct BackdropDownloadUi {
    pending: Option<String>,
    active: Option<Active>,
    error: Option<String>,
    busy_notice: bool,
}

pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    pub phase: Phase,
}

impl BackdropDownloadUi {
    #[cfg(test)]
    pub fn pending(&self) -> Option<&str> {
        self.pending.as_deref()
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub fn is_downloading(&self) -> bool {
        self.active.is_some()
    }

    pub fn progress(&self) -> Option<Progress> {
        self.active.as_ref().map(|a| Progress {
            downloaded: a.downloaded,
            total: a.total,
            phase: a.phase,
        })
    }

    pub fn is_visible(&self) -> bool {
        self.pending.is_some() || self.active.is_some()
    }

    pub fn request(&mut self, id: &str) {
        if self.active.is_some() {
            self.busy_notice = true;
            return;
        }
        self.busy_notice = false;
        self.error = None;
        self.pending = Some(id.to_owned());
    }

    pub fn dismiss(&mut self) {
        if self.active.is_none() {
            self.pending = None;
            self.error = None;
            self.busy_notice = false;
        }
    }

    pub fn cancel(&mut self) {
        if let Some(a) = self.active.as_mut() {
            a.cancelled = true;
            a.cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn start_download(&mut self, id: &str, ctx: &egui::Context) {
        let Some(entry) = backdrop_catalog::lookup(id) else {
            self.error = Some(format!("Unknown backdrop {id}."));
            return;
        };
        let Some(root) = template_catalog::user_data_dir() else {
            self.error = Some(format!(
                "No per-user data folder is available; set {}.",
                template_catalog::DATA_DIR_ENV
            ));
            return;
        };
        if let Err(e) = backdrop_fetch::check_free_space(entry, &root) {
            self.error = Some(e);
            return;
        }
        let ctx = ctx.clone();
        self.spawn(
            id,
            move |cancel, on_event| backdrop_fetch::fetch_entry(entry, &root, cancel, on_event),
            Some(ctx),
        );
    }

    pub fn spawn<F>(&mut self, id: &str, job: F, ctx: Option<egui::Context>)
    where
        F: FnOnce(&AtomicBool, &mut dyn FnMut(BackdropEvent)) -> Result<PathBuf, String>
            + Send
            + 'static,
    {
        if self.active.is_some() {
            return;
        }
        self.error = None;
        self.busy_notice = false;
        self.pending = Some(id.to_owned());
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let tx_events = tx.clone();
            let ctx_events = ctx.clone();
            let result = job(worker_cancel.as_ref(), &mut |ev| {
                let _ = tx_events.send(Msg::Event(ev));
                if let Some(c) = &ctx_events {
                    c.request_repaint();
                }
            });
            let _ = tx.send(Msg::Finished(result));
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        });
        self.active = Some(Active {
            id: id.to_owned(),
            cancelled: false,
            rx,
            cancel,
            downloaded: 0,
            total: 0,
            phase: Phase::Downloading,
        });
    }

    pub fn poll(&mut self) -> Option<Outcome> {
        let mut finished: Option<Result<PathBuf, String>> = None;
        if let Some(a) = self.active.as_mut() {
            loop {
                match a.rx.try_recv() {
                    Ok(Msg::Event(BackdropEvent::Started { total_bytes, .. })) => {
                        a.total = total_bytes;
                    }
                    Ok(Msg::Event(BackdropEvent::Progress { downloaded, total })) => {
                        a.downloaded = downloaded;
                        a.total = total;
                    }
                    Ok(Msg::Event(BackdropEvent::Verifying)) => a.phase = Phase::Verifying,
                    Ok(Msg::Event(BackdropEvent::Cropping)) => a.phase = Phase::Cropping,
                    Ok(Msg::Event(BackdropEvent::Done { .. })) => {}
                    Ok(Msg::Finished(r)) => {
                        finished = Some(r);
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = Some(Err("the download stopped unexpectedly".to_owned()));
                        break;
                    }
                }
            }
        }
        let result = finished?;
        let (id, cancelled) = self.active.take().map(|a| (a.id.clone(), a.cancelled))?;
        match result {
            Ok(_) if cancelled => {
                self.pending = None;
                self.error = None;
                None
            }
            Ok(_) => {
                self.pending = None;
                self.error = None;
                Some(Outcome::Installed { id })
            }
            Err(e) if e == CANCELLED => {
                self.pending = None;
                self.error = None;
                None
            }
            Err(e) => {
                self.error = Some(e);
                self.pending = Some(id);
                None
            }
        }
    }

    pub fn show_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if !self.is_visible() {
            return;
        }
        let pal = palette(ui);
        let unit = em(ui);
        let Some(id) = self
            .active
            .as_ref()
            .map(|a| a.id.clone())
            .or_else(|| self.pending.clone())
        else {
            return;
        };
        let Some(entry) = backdrop_catalog::lookup(&id) else {
            self.pending = None;
            return;
        };
        let mut start = false;
        let mut dismiss = false;
        let mut cancel = false;
        let heading = format!("Download backdrop: {}", entry.title);
        callout(ui, pal.accent, &heading, |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(entry.citation())
                        .small()
                        .color(pal.text_muted),
                )
                .wrap(),
            );
            ui.add_space(unit * 0.3);
            ui.add(
                egui::Label::new(format!(
                    "Download size {}. Free disk space needed: {}. The data licence is CC0 \
                     (public domain).",
                    size_label(entry.size),
                    size_label(backdrop_fetch::required_free_bytes(entry))
                ))
                .wrap(),
            );
            if let Some(p) = self.progress() {
                let (frac, text) = match p.phase {
                    Phase::Downloading => {
                        let frac = if p.total > 0 {
                            (p.downloaded as f32 / p.total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        (
                            frac,
                            format!(
                                "{} of {}",
                                size_label(p.downloaded),
                                size_label(p.total.max(p.downloaded))
                            ),
                        )
                    }
                    Phase::Verifying => (1.0, "Checking the download…".to_owned()),
                    Phase::Cropping => (1.0, "Cutting out the deep brain region…".to_owned()),
                };
                ui.add(egui::ProgressBar::new(frac).text(text));
                if ui
                    .button("Cancel")
                    .on_hover_text("Stop the download and remove the partial file")
                    .clicked()
                {
                    cancel = true;
                }
            } else {
                if self.busy_notice {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new("Another backdrop is already downloading.")
                                .color(pal.text_muted),
                        )
                        .wrap(),
                    );
                }
                if let Some(err) = &self.error {
                    ui.add(egui::Label::new(egui::RichText::new(err).color(pal.err)).wrap());
                }
                ui.horizontal_wrapped(|ui| {
                    let label = format!("Download ({})", size_label(entry.size));
                    if ui
                        .button(label)
                        .on_hover_text("Download this backdrop and show it behind the slices")
                        .clicked()
                    {
                        start = true;
                    }
                    if ui
                        .button("Not now")
                        .on_hover_text("Close this message without downloading")
                        .clicked()
                    {
                        dismiss = true;
                    }
                });
            }
        });
        if start {
            self.start_download(&id, ctx);
        }
        if dismiss {
            self.dismiss();
        }
        if cancel {
            self.cancel();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(ui: &mut BackdropDownloadUi) -> Option<Outcome> {
        let end = Instant::now() + Duration::from_secs(5);
        let mut out = None;
        while ui.is_downloading() && Instant::now() < end {
            if let Some(o) = ui.poll() {
                out = Some(o);
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!ui.is_downloading());
        out
    }

    fn wait_for_cancel(cancel: &AtomicBool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !cancel.load(Ordering::SeqCst) && Instant::now() < end {
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn progress_then_done_reports_the_installed_id() {
        let mut ui = BackdropDownloadUi::default();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        ui.spawn(
            "x",
            move |_cancel, on_event| {
                on_event(BackdropEvent::Started {
                    id: "x".to_owned(),
                    total_bytes: 100,
                });
                on_event(BackdropEvent::Progress {
                    downloaded: 40,
                    total: 100,
                });
                on_event(BackdropEvent::Cropping);
                let _ = go_rx.recv();
                Ok(PathBuf::from("/data/x/backdrop.nii.gz"))
            },
            None,
        );
        let end = Instant::now() + Duration::from_secs(5);
        while ui.progress().is_none_or(|p| p.phase != Phase::Cropping) && Instant::now() < end {
            let _ = ui.poll();
            thread::sleep(Duration::from_millis(5));
        }
        let p = ui.progress().unwrap();
        assert_eq!((p.downloaded, p.total), (40, 100));
        go_tx.send(()).unwrap();
        match wait(&mut ui) {
            Some(Outcome::Installed { id }) => assert_eq!(id, "x"),
            None => panic!("no outcome"),
        }
        assert!(!ui.is_visible());
    }

    #[test]
    fn an_error_is_kept_and_the_card_stays_open() {
        let mut ui = BackdropDownloadUi::default();
        ui.spawn("x", |_c, _e| Err("network down".to_owned()), None);
        assert!(wait(&mut ui).is_none());
        assert_eq!(ui.error(), Some("network down"));
        assert_eq!(ui.pending(), Some("x"));
        ui.dismiss();
        assert!(!ui.is_visible());
        assert_eq!(ui.error(), None);
    }

    #[test]
    fn cancel_closes_the_card_quietly() {
        let mut ui = BackdropDownloadUi::default();
        ui.spawn(
            "x",
            |cancel, _e| {
                wait_for_cancel(cancel);
                Err(CANCELLED.to_owned())
            },
            None,
        );
        ui.cancel();
        assert!(wait(&mut ui).is_none());
        assert_eq!(ui.error(), None);
        assert!(!ui.is_visible());
    }

    #[test]
    fn dropping_sets_the_cancel_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&flag);
        let mut ui = BackdropDownloadUi::default();
        ui.spawn(
            "x",
            move |cancel, _e| {
                wait_for_cancel(cancel);
                seen.store(cancel.load(Ordering::SeqCst), Ordering::SeqCst);
                Err(CANCELLED.to_owned())
            },
            None,
        );
        drop(ui);
        let end = Instant::now() + Duration::from_secs(5);
        while !flag.load(Ordering::SeqCst) && Instant::now() < end {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn a_finish_after_cancel_is_treated_as_cancelled() {
        let mut ui = BackdropDownloadUi::default();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        ui.spawn(
            "x",
            move |_c, _e| {
                let _ = go_rx.recv();
                Ok(PathBuf::from("/data/x/backdrop.nii.gz"))
            },
            None,
        );
        ui.cancel();
        go_tx.send(()).unwrap();
        assert!(wait(&mut ui).is_none());
        assert!(!ui.is_visible());
        assert_eq!(ui.error(), None);
    }

    #[test]
    fn an_installed_catalogue_entry_is_listed_with_its_path_even_without_an_option() {
        let entries = backdrop_catalog::entries_for_space("MNI152NLin2009bAsym");
        let first = entries[0];
        let installed =
            |e: &BackdropEntry| (e.id == first.id).then(|| PathBuf::from("/data/backdrop.nii.gz"));
        let choices = backdrop_choices_with(&[], &entries, None, &installed);
        assert_eq!(choices[0].id, first.id);
        assert_eq!(
            choices[0].path.as_deref(),
            Some(Path::new("/data/backdrop.nii.gz"))
        );
        assert_eq!(choices[0].detail, None);
        assert_eq!(choices[0].kind, "download");
        assert!(choices[1].path.is_none());
    }

    #[test]
    fn a_second_request_while_running_does_not_replace_the_download() {
        let mut ui = BackdropDownloadUi::default();
        ui.spawn(
            "x",
            |cancel, _e| {
                wait_for_cancel(cancel);
                Err(CANCELLED.to_owned())
            },
            None,
        );
        ui.request("y");
        assert_eq!(ui.pending(), Some("x"));
        ui.cancel();
        let _ = wait(&mut ui);
    }

    fn option(id: &str, kind: BackdropKind) -> BackdropOption {
        BackdropOption {
            id: id.to_owned(),
            label: format!("label {id}"),
            path: PathBuf::from(format!("/files/{id}.nii.gz")),
            kind,
        }
    }

    #[test]
    fn choices_list_installed_then_missing_downloads_then_custom() {
        let entries = backdrop_catalog::entries_for_space("MNI152NLin2009bAsym");
        assert!(entries.len() >= 2);
        let first = entries[0];
        let options = vec![
            option("template-t1", BackdropKind::Template),
            option("patient-preop", BackdropKind::Patient),
            option(first.id, BackdropKind::Download),
        ];
        let dir = std::env::temp_dir().join(format!("dbsloc_bd_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mine.nii.gz");
        std::fs::write(&file, b"x").unwrap();
        let persisted = custom_id(&file);
        let choices = backdrop_choices_with(&options, &entries, Some(&persisted), &|_| None);
        let ids: Vec<&str> = choices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids[..3], ["template-t1", "patient-preop", first.id]);
        assert_eq!(ids.last().copied(), Some(persisted.as_str()));
        assert_eq!(choices.len(), 3 + entries.len() - 1 + 1);
        assert!(choices[2].path.is_some());
        assert_eq!(choices[2].kind, "download");
        let missing = &choices[3];
        assert_eq!(missing.id, entries[1].id);
        assert!(missing.path.is_none());
        assert_eq!(missing.label, entries[1].title);
        let last = choices.last().unwrap();
        assert_eq!(last.kind, "custom");
        assert_eq!(last.label, "mine.nii.gz (chosen file)");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_download_detail_shows_the_size() {
        let entries = backdrop_catalog::entries_for_space("MNI152NLin2009bAsym");
        let choices = backdrop_choices_with(&[], &entries, None, &|_| None);
        let big = choices
            .iter()
            .find(|c| c.id == "edlow2019-7t-exvivo_res-0.5mm")
            .unwrap();
        assert_eq!(big.detail.as_deref(), Some("download (74 MB)"));
    }

    #[test]
    fn a_custom_id_for_a_missing_file_is_not_listed() {
        let persisted = custom_id(Path::new("/no/such/file.nii.gz"));
        assert!(backdrop_choices(&[], &[], Some(&persisted)).is_empty());
        assert!(backdrop_choices(&[], &[], Some("template-t1")).is_empty());
    }
}
