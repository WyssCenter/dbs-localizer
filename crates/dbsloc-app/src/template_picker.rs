// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use dbsloc_pipeline::template_catalog;
use dbsloc_pipeline::template_fetch::{self, EntryState, EntryStatus, FetchEvent, CANCELLED};

enum Msg {
    Event(FetchEvent),
    Finished(Result<PathBuf, String>),
}

struct Active {
    idx: usize,
    rx: mpsc::Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    downloaded: u64,
    total: u64,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub struct TemplateUi {
    rows: Option<Vec<EntryStatus>>,
    pending: Option<usize>,
    custom: bool,
    active: Option<Active>,
    error: Option<String>,
}

pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
}

pub fn size_label(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}

pub fn row_label(row: &EntryStatus) -> String {
    match &row.state {
        EntryState::Bundled(_) => row.entry.title.to_owned(),
        EntryState::MissingBundled => format!("{} (not found)", row.entry.title),
        EntryState::Installed(_) => format!("{} (installed)", row.entry.title),
        EntryState::Available { size } => {
            format!("{} (download, {})", row.entry.title, size_label(*size))
        }
    }
}

pub fn matching_entry(rows: &[EntryStatus], template: &str) -> Option<usize> {
    let t = template.trim();
    if t.is_empty() {
        return None;
    }
    rows.iter()
        .position(|r| r.state.path().is_some_and(|p| p == Path::new(t)))
}

impl TemplateUi {
    pub fn rows(&mut self) -> &[EntryStatus] {
        self.rows.get_or_insert_with(template_fetch::list_entries)
    }

    pub fn refresh(&mut self) {
        self.rows = Some(template_fetch::list_entries());
    }

    #[cfg(test)]
    pub fn set_rows(&mut self, rows: Vec<EntryStatus>) {
        self.rows = Some(rows);
    }

    pub fn pending(&self) -> Option<usize> {
        self.pending
    }

    pub fn custom_requested(&self) -> bool {
        self.custom
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_downloading(&self) -> bool {
        self.active.is_some()
    }

    pub fn progress(&self) -> Option<Progress> {
        self.active.as_ref().map(|a| Progress {
            downloaded: a.downloaded,
            total: a.total,
        })
    }

    pub fn choose_custom(&mut self) {
        self.custom = true;
        self.pending = None;
        self.error = None;
    }

    pub fn choose(&mut self, idx: usize, tmpl: &mut Option<String>, tmpl_t2: &mut Option<String>) {
        self.error = None;
        let path = self
            .rows()
            .get(idx)
            .and_then(|r| r.state.path().map(|p| p.to_string_lossy().into_owned()));
        match path {
            Some(p) => {
                *tmpl = Some(p);
                *tmpl_t2 = None;
                self.pending = None;
                self.custom = false;
            }
            None => self.pending = Some(idx),
        }
    }

    pub fn start_download(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(entry) = template_catalog::catalogue().get(idx) else {
            return;
        };
        let Some(root) = template_catalog::user_data_dir() else {
            self.error = Some(format!(
                "No per-user data folder is available; set {}.",
                template_catalog::DATA_DIR_ENV
            ));
            return;
        };
        let ctx = ctx.clone();
        self.spawn(
            idx,
            move |cancel, on_event| template_fetch::fetch_entry(entry, &root, cancel, on_event),
            Some(ctx),
        );
    }

    pub fn spawn<F>(&mut self, idx: usize, job: F, ctx: Option<egui::Context>)
    where
        F: FnOnce(&AtomicBool, &mut dyn FnMut(FetchEvent)) -> Result<PathBuf, String>
            + Send
            + 'static,
    {
        if self.active.is_some() {
            return;
        }
        self.error = None;
        self.pending = Some(idx);
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
            idx,
            rx,
            cancel,
            downloaded: 0,
            total: 0,
        });
    }

    pub fn cancel(&mut self) {
        if let Some(a) = &self.active {
            a.cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn poll(&mut self, tmpl: &mut Option<String>, tmpl_t2: &mut Option<String>) {
        let mut finished: Option<Result<PathBuf, String>> = None;
        if let Some(a) = self.active.as_mut() {
            loop {
                match a.rx.try_recv() {
                    Ok(Msg::Event(FetchEvent::Started { total_bytes, .. })) => {
                        a.total = total_bytes;
                    }
                    Ok(Msg::Event(FetchEvent::Progress { downloaded, total })) => {
                        a.downloaded = downloaded;
                        a.total = total;
                    }
                    Ok(Msg::Event(_)) => {}
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
        let Some(result) = finished else { return };
        let idx = self.active.take().map(|a| a.idx);
        match result {
            Ok(path) => {
                self.refresh();
                self.pending = None;
                self.custom = false;
                self.error = None;
                *tmpl = Some(path.to_string_lossy().into_owned());
                *tmpl_t2 = None;
            }
            Err(e) if e == CANCELLED => {
                self.error = None;
                self.pending = idx;
            }
            Err(e) => {
                self.error = Some(e);
                self.pending = idx;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn rows_with(installed_idx: Option<usize>) -> Vec<EntryStatus> {
        template_catalog::catalogue()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let state = match (e.source, installed_idx) {
                    (template_catalog::TemplateSource::Shipped, _) => {
                        EntryState::Bundled(PathBuf::from("/shipped/t1.nii.gz"))
                    }
                    (_, Some(k)) if k == i => {
                        EntryState::Installed(PathBuf::from("/data/t1.nii.gz"))
                    }
                    (template_catalog::TemplateSource::Download { zip_size, .. }, _) => {
                        EntryState::Available { size: zip_size }
                    }
                };
                EntryStatus { entry: e, state }
            })
            .collect()
    }

    fn wait(ui: &mut TemplateUi, tmpl: &mut Option<String>, t2: &mut Option<String>) {
        let end = Instant::now() + Duration::from_secs(5);
        while ui.is_downloading() && Instant::now() < end {
            ui.poll(tmpl, t2);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!ui.is_downloading());
    }

    #[test]
    fn choosing_an_installed_entry_sets_its_path_and_clears_the_t2() {
        let mut ui = TemplateUi::default();
        ui.set_rows(rows_with(Some(1)));
        let mut tmpl = Some("custom.nii".to_owned());
        let mut t2 = Some("custom_t2.nii".to_owned());
        ui.choose(0, &mut tmpl, &mut t2);
        assert_eq!(tmpl.as_deref(), Some("/shipped/t1.nii.gz"));
        assert_eq!(t2, None);
        ui.choose(1, &mut tmpl, &mut t2);
        assert_eq!(tmpl.as_deref(), Some("/data/t1.nii.gz"));
        assert_eq!(matching_entry(ui.rows(), "/data/t1.nii.gz"), Some(1));
        assert_eq!(matching_entry(ui.rows(), "custom.nii"), None);
    }

    #[test]
    fn choosing_a_missing_entry_keeps_the_path_and_offers_the_download() {
        let mut ui = TemplateUi::default();
        ui.set_rows(rows_with(None));
        let mut tmpl = Some("custom.nii".to_owned());
        let mut t2 = Some("custom_t2.nii".to_owned());
        ui.choose(2, &mut tmpl, &mut t2);
        assert_eq!(ui.pending(), Some(2));
        assert_eq!(tmpl.as_deref(), Some("custom.nii"));
        assert_eq!(t2.as_deref(), Some("custom_t2.nii"));
        ui.choose_custom();
        assert_eq!(ui.pending(), None);
        assert!(ui.custom_requested());
        assert_eq!(tmpl.as_deref(), Some("custom.nii"));
    }

    #[test]
    fn progress_then_done_selects_the_downloaded_entry() {
        let mut ui = TemplateUi::default();
        ui.set_rows(rows_with(None));
        let (go_tx, go_rx) = mpsc::channel::<()>();
        ui.spawn(
            2,
            move |_cancel, on_event| {
                on_event(FetchEvent::Started {
                    id: "x".to_owned(),
                    total_bytes: 100,
                });
                on_event(FetchEvent::Progress {
                    downloaded: 40,
                    total: 100,
                });
                let _ = go_rx.recv();
                Ok(PathBuf::from("/data/new/t1.nii.gz"))
            },
            None,
        );
        let mut tmpl = Some("old.nii".to_owned());
        let mut t2 = Some("old_t2.nii".to_owned());
        let end = Instant::now() + Duration::from_secs(5);
        while ui.progress().is_none_or(|p| p.downloaded < 40) && Instant::now() < end {
            ui.poll(&mut tmpl, &mut t2);
            thread::sleep(Duration::from_millis(5));
        }
        let p = ui.progress().unwrap();
        assert_eq!((p.downloaded, p.total), (40, 100));
        assert_eq!(tmpl.as_deref(), Some("old.nii"));
        go_tx.send(()).unwrap();
        wait(&mut ui, &mut tmpl, &mut t2);
        assert_eq!(tmpl.as_deref(), Some("/data/new/t1.nii.gz"));
        assert_eq!(t2, None);
        assert_eq!(ui.pending(), None);
        assert_eq!(ui.error(), None);
    }

    #[test]
    fn a_failed_download_shows_the_error_and_keeps_the_choice() {
        let mut ui = TemplateUi::default();
        ui.set_rows(rows_with(None));
        ui.spawn(3, |_c, _e| Err("SHA-256 mismatch".to_owned()), None);
        let mut tmpl = Some("old.nii".to_owned());
        let mut t2 = None;
        wait(&mut ui, &mut tmpl, &mut t2);
        assert_eq!(ui.error(), Some("SHA-256 mismatch"));
        assert_eq!(ui.pending(), Some(3));
        assert_eq!(tmpl.as_deref(), Some("old.nii"));
    }

    #[test]
    fn cancel_resets_without_an_error() {
        let mut ui = TemplateUi::default();
        ui.set_rows(rows_with(None));
        ui.spawn(
            3,
            |cancel, _e| {
                let end = Instant::now() + Duration::from_secs(5);
                while !cancel.load(Ordering::SeqCst) && Instant::now() < end {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(CANCELLED.to_owned())
            },
            None,
        );
        let mut tmpl = None;
        let mut t2 = None;
        ui.cancel();
        wait(&mut ui, &mut tmpl, &mut t2);
        assert_eq!(ui.error(), None);
        assert_eq!(ui.pending(), Some(3));
        assert_eq!(tmpl, None);
    }

    #[test]
    fn labels_name_the_status() {
        let rows = rows_with(Some(1));
        assert!(row_label(&rows[0]).contains("bundled"));
        assert!(row_label(&rows[1]).ends_with("(installed)"));
        assert!(row_label(&rows[2]).contains("download, "));
        assert!(row_label(&rows[2]).contains(" MB)"));
    }

    #[test]
    fn dropping_an_active_download_sets_the_cancel_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        let (_tx, rx) = mpsc::channel();
        drop(Active {
            idx: 0,
            rx,
            cancel: Arc::clone(&flag),
            downloaded: 0,
            total: 0,
        });
        assert!(flag.load(Ordering::SeqCst));
    }
}
