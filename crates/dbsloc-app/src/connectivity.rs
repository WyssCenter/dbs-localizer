// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use dbsloc_connectome::{read_fibres, FunctionalConnectome};
use dbsloc_pipeline::connectome_cache::CacheSetting;
use dbsloc_pipeline::connectome_catalog::ConnectomeClass;
use dbsloc_pipeline::connectome_fetch::discover_connectomes;
use dbsloc_pipeline::connectome_maps::job::{
    functional_label, resolve_connectome, run_functional_batch, run_structural, FunctionalPass,
    FunctionalRequest, LoadOptions, Status, StructuralEngine, StructuralRequest, SubjectOutcome,
};
use dbsloc_pipeline::connectome_maps::structural::{OutputGrid, SeedKind};
use dbsloc_pipeline::connectome_maps::{MapError, Progress};
use dbsloc_viz::scene_layers::{FibreColouring, FibreLayer};
use eframe::egui;

use crate::connectome_cache_ui::{check_consent, pre_load, CacheRun, ConsentFlow};
use crate::panels::{em, palette};

const FILE_PALETTE: [[f32; 4]; 6] = [
    [0.36, 0.55, 0.75, 1.0],
    [0.80, 0.52, 0.25, 1.0],
    [0.47, 0.67, 0.42, 1.0],
    [0.70, 0.45, 0.65, 1.0],
    [0.80, 0.70, 0.30, 1.0],
    [0.45, 0.65, 0.65, 1.0],
];

pub fn file_palette_colour(n: usize) -> [f32; 4] {
    FILE_PALETTE[n % FILE_PALETTE.len()]
}

pub fn stimulation_layer_id(subject: &str, label: &str, tag: &str) -> String {
    format!("stim:{subject}:{label}:{tag}")
}

pub fn stimulation_layer_label(label: &str, tag: &str) -> String {
    format!("{label} · {tag}")
}

pub fn file_layer_id(path: &Path) -> String {
    format!("file:{}", path.display())
}

pub fn size_hint(bytes: u64) -> String {
    const GB: f64 = 1_073_741_824.0;
    const MB: f64 = 1_048_576.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", (b / MB).max(1.0))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Gate {
    pub saved: bool,
    pub template_space: bool,
    pub vta: bool,
    pub chosen: bool,
    pub run_active: bool,
    pub busy: bool,
}

pub fn compute_blocker(g: Gate) -> Option<&'static str> {
    if !g.saved {
        Some("Save the stimulation first")
    } else if !g.template_space {
        Some("The VTA of this stimulation exists in native space only")
    } else if !g.vta {
        Some("Compute the VTA first; the connectivity is seeded from it")
    } else if !g.chosen {
        Some("Choose a connectome first")
    } else if g.run_active {
        Some("A run is in progress")
    } else if g.busy {
        Some("A connectivity job is already running")
    } else {
        None
    }
}

pub fn computed_status(tag: &str, maps: usize) -> Option<String> {
    (maps > 0).then(|| format!("Computed with {tag}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outputs {
    pub maps: Vec<String>,
    pub subset: Option<String>,
}

pub fn scan_outputs(names: &[String], subject: &str, structural: bool, key: &str) -> Outputs {
    let prefix = format!("sub-{subject}_");
    let mut out = Outputs::default();
    for name in names.iter().filter(|n| n.starts_with(&prefix)) {
        if structural {
            if name.ends_with(&format!("_conn-{key}_strucmap.nii"))
                || name.ends_with(&format!("_conn-{key}_strucmap.nii.gz"))
            {
                out.maps.push(name.clone());
            } else if name.ends_with(&format!("_conn-{key}_fibers.mat")) {
                out.subset = Some(name.clone());
            }
        } else if name.contains(&format!("_conn-{key}_desc-")) && name.contains("_funcmap.nii") {
            out.maps.push(name.clone());
        }
    }
    out.maps.sort();
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub spec: String,
    pub label: String,
    pub structural: bool,
}

fn fibre_file_size(path: &Path) -> Option<u64> {
    ["data.mat", "data.trk", "data.trk.gz"]
        .iter()
        .find_map(|n| std::fs::metadata(path.join(n)).ok())
        .map(|m| m.len())
}

fn list_entries(dir: Option<&Path>) -> Result<Vec<Entry>, String> {
    let roots = crate::connectome_roots::resolve(dir).map_err(|e| e.message)?;
    let mut out = Vec::new();
    for c in discover_connectomes(&roots) {
        if c.missing || c.duplicate_of.is_some() {
            continue;
        }
        match c.class {
            ConnectomeClass::Fmri => {
                out.push(Entry {
                    spec: c.name.clone(),
                    label: format!("{} (functional)", c.name),
                    structural: false,
                });
                for subset in &c.subsets {
                    if subset.eq_ignore_ascii_case("Full Set") {
                        continue;
                    }
                    out.push(Entry {
                        spec: format!("{} > {subset}", c.name),
                        label: format!("{} · {subset} (functional)", c.name),
                        structural: false,
                    });
                }
            }
            ConnectomeClass::Dmri | ConnectomeClass::DmriMultiTract => {
                let label = match fibre_file_size(&c.path) {
                    Some(bytes) => format!("{} ({})", c.name, size_hint(bytes)),
                    None => c.name.clone(),
                };
                out.push(Entry {
                    spec: c.name.clone(),
                    label,
                    structural: true,
                });
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub vta: bool,
    pub tag: Option<String>,
    pub structural: bool,
    pub outputs: Outputs,
    pub folder: PathBuf,
    pub error: Option<String>,
    pub subject: String,
    pub label: String,
    pub spec: Option<String>,
}

impl Probe {
    pub fn is_for(&self, subject: &str, label: &str, spec: Option<&str>) -> bool {
        self.subject == subject && self.label == label && self.spec.as_deref() == spec
    }
}

#[derive(Debug, Clone)]
struct Target {
    dir: Option<PathBuf>,
    derivatives_root: PathBuf,
    subject: String,
    space: Option<String>,
    label: String,
    spec: Option<String>,
}

fn probe(t: &Target) -> Probe {
    let mut p = Probe {
        subject: t.subject.clone(),
        label: t.label.clone(),
        spec: t.spec.clone(),
        ..Probe::default()
    };
    let Some(space) = &t.space else {
        return p;
    };
    p.folder = t
        .derivatives_root
        .join(format!("sub-{}", t.subject))
        .join("stimulations")
        .join(space)
        .join(&t.label);
    p.vta = !crate::stimulation_panel::computed_files(&p.folder).is_empty();
    let Some(spec) = &t.spec else {
        return p;
    };
    let roots = match crate::connectome_roots::resolve(t.dir.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            p.error = Some(e.message);
            return p;
        }
    };
    match resolve_connectome(&roots, spec) {
        Ok(conn) => {
            p.structural = conn.class != ConnectomeClass::Fmri;
            let key = if p.structural {
                conn.tag.clone()
            } else {
                functional_label(&conn.tag, conn.subset.as_deref())
            };
            let names: Vec<String> = std::fs::read_dir(&p.folder)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            p.outputs = scan_outputs(&names, &t.subject, p.structural, &key);
            p.tag = Some(key);
        }
        Err(e) => p.error = Some(e.to_string()),
    }
    p
}

#[derive(Debug, Clone, Default)]
struct Shown {
    stage: String,
    done: u64,
    total: u64,
    message: String,
}

#[derive(Debug)]
struct Done {
    files: Vec<PathBuf>,
    notes: Vec<String>,
    warnings: Vec<String>,
    skipped: Option<String>,
    connected: Option<u64>,
}

struct ComputeRun {
    cancel: Arc<AtomicBool>,
    shared: Arc<Mutex<Shown>>,
    rx: mpsc::Receiver<Result<Done, MapError>>,
    name: String,
}

fn connected_fibres(files: &[PathBuf]) -> Option<u64> {
    let map = files
        .iter()
        .find(|f| f.to_string_lossy().ends_with("_strucmap.nii"))?;
    let text = std::fs::read_to_string(map.with_extension("json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["params"]["connected_fibres"].as_u64()
}

fn compute(
    t: &Target,
    run: &CacheRun,
    cancel: &AtomicBool,
    progress: &(dyn Fn(&Progress) + Sync),
) -> Result<Done, MapError> {
    let (Some(space), Some(spec)) = (&t.space, &t.spec) else {
        return Err(MapError::Usage("no template-space stimulation".to_owned()));
    };
    let roots = crate::connectome_roots::resolve(t.dir.as_deref())
        .map_err(|e| MapError::Usage(e.message))?;
    let conn = resolve_connectome(&roots, spec)?;
    if conn.class == ConnectomeClass::Fmri {
        let functional = FunctionalConnectome::open(&conn.path)?;
        let req = FunctionalRequest {
            derivatives_root: t.derivatives_root.clone(),
            subject_id: t.subject.clone(),
            label: t.label.clone(),
            space: space.clone(),
            connectome: conn,
            kind: SeedKind::Binary,
            model: None,
            seed_file: None,
            per_subject_maps: false,
        };
        let outcomes = run_functional_batch(
            &functional,
            std::slice::from_ref(&req),
            &FunctionalPass::default(),
            cancel,
            progress,
        )?;
        let Some(SubjectOutcome {
            status,
            files,
            warnings,
            ..
        }) = outcomes.into_iter().next()
        else {
            return Err(MapError::Io("the job returned no result".to_owned()));
        };
        return match status {
            Status::Ok => Ok(Done {
                files,
                notes: Vec::new(),
                warnings,
                skipped: None,
                connected: None,
            }),
            Status::Skipped(reason) => Ok(Done {
                files,
                notes: Vec::new(),
                warnings,
                skipped: Some(reason),
                connected: None,
            }),
            Status::Error { code, message } => Err(MapError::Io(format!("{code}: {message}"))),
        };
    }
    progress(&Progress {
        stage: "load".to_owned(),
        done: 0,
        total: 0,
        message: conn.name.clone(),
    });
    let prepared = pre_load(&conn, run, cancel, progress)?;
    let engine = StructuralEngine::load_with(
        &conn,
        &LoadOptions {
            setting: prepared.setting,
            cancel: Some(cancel),
            progress: Some(progress),
        },
    )?;
    let req = StructuralRequest {
        derivatives_root: t.derivatives_root.clone(),
        subject_id: t.subject.clone(),
        label: t.label.clone(),
        space: space.clone(),
        connectome: conn,
        kind: SeedKind::Binary,
        model: None,
        grid: OutputGrid::Mm2,
        seed_file: None,
        write_subset: true,
    };
    let outcome = run_structural(&engine, &req, cancel, progress)?;
    let connected = connected_fibres(&outcome.files);
    let mut warnings = prepared.warnings;
    warnings.extend(engine.warnings.iter().cloned());
    warnings.extend(outcome.warnings);
    Ok(Done {
        files: outcome.files,
        notes: engine.notes.clone(),
        warnings,
        skipped: outcome.skipped,
        connected,
    })
}

pub fn read_layer(
    path: &Path,
    id: String,
    label: String,
    colouring: FibreColouring,
) -> Result<FibreLayer, String> {
    let set = read_fibres(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let n = set.fibre_offsets.len().saturating_sub(1);
    if n == 0 {
        return Err(format!("{} holds no fibres", path.display()));
    }
    let values = if set.vals.len() == n {
        set.vals.clone()
    } else {
        vec![1.0; n]
    };
    Ok(FibreLayer {
        id,
        label,
        points: set.points,
        offsets: set.fibre_offsets,
        values,
        colouring,
        total_fibres: n,
    })
}

#[derive(Default)]
pub struct FibreFiles {
    pending: Vec<mpsc::Receiver<Option<Result<FibreLayer, String>>>>,
    added: usize,
}

impl FibreFiles {
    pub fn request(&mut self, ctx: &egui::Context) {
        if !self.pending.is_empty() {
            return;
        }
        let ctx = ctx.clone();
        let colour = file_palette_colour(self.added);
        self.added += 1;
        let (tx, rx) = mpsc::channel();
        self.pending.push(rx);
        thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Select a fibre file")
                .add_filter("Fibre file", &["mat", "trk", "gz"])
                .pick_file();
            let result = picked.map(|path| {
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                read_layer(
                    &path,
                    file_layer_id(&path),
                    label,
                    FibreColouring::Uniform(colour),
                )
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub fn poll(&mut self) -> Vec<Result<FibreLayer, String>> {
        let mut ready = Vec::new();
        self.pending.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                ready.extend(result);
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        ready
    }
}

#[derive(Default)]
pub struct ConnectivityOutput {
    pub log: Vec<String>,
    pub errors: Vec<(String, String)>,
    pub fibre_layer: Option<FibreLayer>,
    pub open_connectomes: bool,
    pub remember: Option<String>,
}

pub struct Inputs<'a> {
    pub derivatives_root: &'a Path,
    pub subject: &'a str,
    pub space: Option<&'a str>,
    pub label: &'a str,
    pub saved: bool,
    pub run_active: bool,
    pub connectomes_dir: Option<&'a Path>,
    pub remembered: Option<&'a str>,
    pub cache: CacheSetting,
}

#[derive(Default)]
pub struct Connectivity {
    entries: Option<Vec<Entry>>,
    listing: Option<mpsc::Receiver<Result<Vec<Entry>, String>>>,
    list_error: Option<String>,
    choice: Option<String>,
    probe_key: Option<(String, Option<String>)>,
    probing: Option<mpsc::Receiver<Probe>>,
    found: Probe,
    stale: bool,
    run: Option<ComputeRun>,
    fibres: Option<mpsc::Receiver<Result<FibreLayer, String>>>,
    message: Option<String>,
    consent: ConsentFlow,
    pending: Option<Target>,
}

impl Connectivity {
    pub fn invalidate(&mut self) {
        self.stale = true;
        self.message = None;
    }

    pub fn is_busy(&self) -> bool {
        self.run.is_some()
            || self.consent.is_active()
            || self.listing.is_some()
            || self.probing.is_some()
            || self.fibres.is_some()
    }

    pub fn cancel(&self) {
        if let Some(run) = &self.run {
            run.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn target(&self, i: &Inputs) -> Target {
        Target {
            dir: i.connectomes_dir.map(Path::to_path_buf),
            derivatives_root: i.derivatives_root.to_path_buf(),
            subject: i.subject.to_owned(),
            space: i.space.map(str::to_owned),
            label: i.label.to_owned(),
            spec: self.choice.clone(),
        }
    }

    fn start_listing(&mut self, i: &Inputs, ctx: &egui::Context) {
        let dir = i.connectomes_dir.map(Path::to_path_buf);
        let (tx, rx) = mpsc::channel();
        self.listing = Some(rx);
        let repaint = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(list_entries(dir.as_deref()));
            repaint.request_repaint();
        });
    }

    fn start_probe(&mut self, i: &Inputs, ctx: &egui::Context) {
        let target = self.target(i);
        self.probe_key = Some((i.label.to_owned(), self.choice.clone()));
        self.stale = false;
        let (tx, rx) = mpsc::channel();
        self.probing = Some(rx);
        let repaint = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(probe(&target));
            repaint.request_repaint();
        });
    }

    fn request_compute(&mut self, i: &Inputs, ctx: &egui::Context) {
        if self.choice.is_none() || self.consent.is_active() {
            return;
        }
        let target = self.target(i);
        let probe = target.clone();
        self.pending = Some(target);
        self.consent.begin(ctx, i.cache, move || {
            let spec = probe.spec.as_deref()?;
            let roots = crate::connectome_roots::resolve(probe.dir.as_deref()).ok()?;
            let conn = resolve_connectome(&roots, spec).ok()?;
            check_consent(&conn, CacheSetting::Ask)
        });
    }

    fn start_compute(
        &mut self,
        target: Target,
        run: CacheRun,
        ctx: &egui::Context,
        out: &mut ConnectivityOutput,
    ) {
        let Some(spec) = target.spec.clone() else {
            return;
        };
        let label = target.label.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shown::default()));
        let (tx, rx) = mpsc::channel();
        let worker_cancel = Arc::clone(&cancel);
        let worker_shared = Arc::clone(&shared);
        let repaint = ctx.clone();
        let name = spec.clone();
        thread::spawn(move || {
            let sink = |p: &Progress| {
                if let Ok(mut s) = worker_shared.lock() {
                    *s = Shown {
                        stage: p.stage.clone(),
                        done: p.done,
                        total: p.total,
                        message: p.message.clone(),
                    };
                }
                repaint.request_repaint();
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compute(&target, &run, &worker_cancel, &sink)
            }))
            .unwrap_or_else(|_| Err(MapError::Io("the job stopped unexpectedly".to_owned())));
            let _ = tx.send(result);
            repaint.request_repaint();
        });
        out.log
            .push(format!("Connectivity: computing {label} with {spec}"));
        self.message = None;
        self.run = Some(ComputeRun {
            cancel,
            shared,
            rx,
            name,
        });
    }

    fn start_fibres(&mut self, ctx: &egui::Context) {
        let (Some(tag), Some(subset)) = (&self.found.tag, &self.found.outputs.subset) else {
            return;
        };
        let path = self.found.folder.join(subset);
        let id = stimulation_layer_id(&self.found.subject, &self.found.label, tag);
        let label = stimulation_layer_label(&self.found.label, tag);
        let (tx, rx) = mpsc::channel();
        self.fibres = Some(rx);
        let repaint = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(read_layer(&path, id, label, FibreColouring::Strength));
            repaint.request_repaint();
        });
    }

    fn poll(&mut self, out: &mut ConnectivityOutput) {
        if let Some(rx) = &self.listing {
            match rx.try_recv() {
                Ok(Ok(entries)) => {
                    self.entries = Some(entries);
                    self.listing = None;
                }
                Ok(Err(e)) => {
                    self.list_error = Some(e);
                    self.entries = Some(Vec::new());
                    self.listing = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.listing = None,
            }
        }
        if let Some(rx) = &self.probing {
            match rx.try_recv() {
                Ok(p) => {
                    self.found = p;
                    self.probing = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.probing = None,
            }
        }
        if let Some(rx) = &self.fibres {
            match rx.try_recv() {
                Ok(Ok(layer)) => {
                    self.message = Some(format!(
                        "{} fibres loaded into the 3D viewer.",
                        layer.n_fibres()
                    ));
                    out.fibre_layer = Some(layer);
                    self.fibres = None;
                }
                Ok(Err(e)) => {
                    out.errors.push(("Could not load the fibres".to_owned(), e));
                    self.fibres = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.fibres = None,
            }
        }
        if let Some(run) = &self.run {
            let result = match run.rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(MapError::Io(
                    "the job ended without a result".to_owned(),
                ))),
            };
            if let Some(result) = result {
                let name = run.name.clone();
                self.run = None;
                self.stale = true;
                match result {
                    Ok(done) => {
                        for n in &done.notes {
                            out.log.push(format!("dbsloc: note: connectivity: {n}"));
                        }
                        for w in &done.warnings {
                            out.log.push(format!("Connectivity: warning: {w}"));
                        }
                        self.message = Some(match &done.skipped {
                            Some(reason) => format!("Nothing written: {reason}."),
                            None => {
                                let maps: Vec<String> = done
                                    .files
                                    .iter()
                                    .filter_map(|f| f.file_name())
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .filter(|n| n.ends_with("map.nii"))
                                    .collect();
                                let fibres = done
                                    .connected
                                    .map(|n| format!(", {n} connected fibres"))
                                    .unwrap_or_default();
                                format!("Wrote {}{fibres}.", maps.join(", "))
                            }
                        });
                        for f in &done.files {
                            out.log.push(format!("Connectivity: wrote {}", f.display()));
                        }
                    }
                    Err(MapError::Cancelled) => {
                        self.message = Some("Cancelled; nothing was written.".to_owned());
                        out.log.push(format!("Connectivity: cancelled ({name})"));
                    }
                    Err(e) => {
                        out.errors.push((
                            "Could not compute the connectivity".to_owned(),
                            e.to_string(),
                        ));
                    }
                }
            }
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        i: &Inputs,
        out: &mut ConnectivityOutput,
    ) {
        self.poll(out);
        if let Some(run) = self.consent.poll(ctx) {
            if let Some(target) = self.pending.take() {
                self.start_compute(target, run, ctx, out);
            }
        }
        if self.choice.is_none() {
            self.choice = i.remembered.map(str::to_owned);
        }
        let unit = em(ui);
        egui::CollapsingHeader::new(egui::RichText::new("Connectivity").strong())
            .id_salt("stimulation_connectivity")
            .default_open(false)
            .show(ui, |ui| {
                if self.entries.is_none() && self.listing.is_none() {
                    self.start_listing(i, ctx);
                }
                let key = (i.label.to_owned(), self.choice.clone());
                let key_changed = self.probe_key.as_ref() != Some(&key);
                if self.probing.is_none() && (key_changed || (self.stale && self.run.is_none())) {
                    self.start_probe(i, ctx);
                }
                self.body(ui, ctx, i, out, unit);
            });
        if self.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn body(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        i: &Inputs,
        out: &mut ConnectivityOutput,
        unit: f32,
    ) {
        let pal = palette(ui);
        let Some(entries) = self.entries.clone() else {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("Looking for installed connectomes…")
                            .small()
                            .color(pal.text_muted),
                    )
                    .selectable(false),
                );
            });
            return;
        };
        if entries.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            self.list_error
                                .as_deref()
                                .unwrap_or("No connectome is installed."),
                        )
                        .small()
                        .color(pal.text_muted),
                    )
                    .selectable(false),
                );
                if ui
                    .button("Connectomes…")
                    .on_hover_text("Open the connectome window to download or import one")
                    .clicked()
                {
                    out.open_connectomes = true;
                }
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Connectome:");
            let selected = self
                .choice
                .as_ref()
                .and_then(|c| entries.iter().find(|e| &e.spec == c))
                .map_or("Choose…", |e| e.label.as_str())
                .to_owned();
            egui::ComboBox::from_id_salt("stimulation_connectome")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for e in &entries {
                        let on = self.choice.as_deref() == Some(e.spec.as_str());
                        if ui.selectable_label(on, &e.label).clicked() && !on {
                            self.choice = Some(e.spec.clone());
                            self.message = None;
                            out.remember = Some(e.spec.clone());
                        }
                    }
                })
                .response
                .on_hover_text(
                    "The connectome the fibres or functional maps of this stimulation come from",
                );
            if ui
                .small_button("Connectomes…")
                .on_hover_text("Open the connectome window to download or import more")
                .clicked()
            {
                out.open_connectomes = true;
            }
        });
        let chosen = self
            .choice
            .as_ref()
            .is_some_and(|c| entries.iter().any(|e| &e.spec == c));
        let current = self
            .found
            .is_for(i.subject, i.label, self.choice.as_deref());
        let gate = Gate {
            saved: i.saved,
            template_space: i.space.is_some(),
            vta: self.found.vta && current,
            chosen,
            run_active: i.run_active,
            busy: self.run.is_some() || self.consent.is_active(),
        };
        if let Some(run) = &self.run {
            let shown = run.shared.lock().map(|s| s.clone()).unwrap_or_default();
            let text = if shown.total > 0 {
                format!(
                    "{} {}/{} {}",
                    shown.stage, shown.done, shown.total, shown.message
                )
            } else if shown.stage.is_empty() {
                format!("Starting {}", run.name)
            } else {
                format!("{} {}", shown.stage, shown.message)
            };
            ui.horizontal(|ui| {
                let bar = if shown.total > 0 {
                    egui::ProgressBar::new(shown.done as f32 / shown.total as f32)
                } else {
                    egui::ProgressBar::new(0.0).animate(true)
                };
                ui.add(bar.desired_width(unit * 18.0).text(text));
                if ui
                    .button("Cancel")
                    .on_hover_text("Stop the job; nothing is written")
                    .clicked()
                {
                    run.cancel.store(true, Ordering::SeqCst);
                }
            });
        }
        let blocker = compute_blocker(gate);
        ui.horizontal_wrapped(|ui| {
            let hover = blocker.unwrap_or(
                "Compute the structural or functional connectivity map seeded from the VTA of this stimulation",
            );
            if ui
                .add_enabled(blocker.is_none(), egui::Button::new("Compute connectivity"))
                .on_hover_text(hover)
                .clicked()
            {
                self.request_compute(i, ctx);
            }
            let can_show = current
                && !self.stale
                && self.probing.is_none()
                && self.found.structural
                && self.found.outputs.subset.is_some()
                && self.fibres.is_none()
                && chosen;
            if ui
                .add_enabled(can_show, egui::Button::new("Show fibres"))
                .on_hover_text(if can_show {
                    "Add the fibres connected to this stimulation to the 3D viewer, coloured by strength"
                } else {
                    "Compute the connectivity with a structural connectome first"
                })
                .clicked()
            {
                self.start_fibres(ctx);
            }
        });
        if let Some(msg) = &self.message {
            ui.add(
                egui::Label::new(egui::RichText::new(msg).small().color(pal.ok))
                    .wrap()
                    .selectable(false),
            );
        } else if let (Some(tag), true) = (&self.found.tag, chosen && current) {
            if let Some(text) = computed_status(tag, self.found.outputs.maps.len()) {
                ui.add(
                    egui::Label::new(egui::RichText::new(text).small().color(pal.text_muted))
                        .selectable(false),
                );
            }
        }
        if current
            && chosen
            && !self.found.outputs.maps.is_empty()
            && crate::panels::open_folder_button(
                ui,
                "Open the folder with the connectivity maps and fibre subsets in the file manager",
            )
        {
            if let Err(e) = crate::panels::reveal_in_file_manager(&self.found.folder) {
                self.message = Some(e);
            }
        }
        if let Some(e) = self.found.error.as_ref().filter(|_| current) {
            ui.add(
                egui::Label::new(egui::RichText::new(e).small().color(pal.warn))
                    .wrap()
                    .selectable(false),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Gate {
        Gate {
            saved: true,
            template_space: true,
            vta: true,
            chosen: true,
            run_active: false,
            busy: false,
        }
    }

    #[test]
    fn the_compute_button_needs_everything() {
        assert_eq!(compute_blocker(ready()), None);
        assert!(compute_blocker(Gate {
            saved: false,
            ..ready()
        })
        .is_some());
        assert!(compute_blocker(Gate {
            template_space: false,
            ..ready()
        })
        .is_some());
        assert!(compute_blocker(Gate {
            vta: false,
            ..ready()
        })
        .is_some());
        assert!(compute_blocker(Gate {
            chosen: false,
            ..ready()
        })
        .is_some());
        assert_eq!(
            compute_blocker(Gate {
                run_active: true,
                ..ready()
            }),
            Some("A run is in progress")
        );
        assert!(compute_blocker(Gate {
            busy: true,
            ..ready()
        })
        .is_some());
    }

    #[test]
    fn the_status_names_the_tag_once_a_map_exists() {
        assert_eq!(computed_status("Gibbs19", 0), None);
        assert_eq!(
            computed_status("Gibbs19", 1).as_deref(),
            Some("Computed with Gibbs19")
        );
    }

    #[test]
    fn layer_ids_and_labels_follow_the_scheme() {
        assert_eq!(
            stimulation_layer_id("01", "clin", "Gibbs19"),
            "stim:01:clin:Gibbs19"
        );
        assert_eq!(stimulation_layer_label("clin", "Gibbs19"), "clin · Gibbs19");
        assert_eq!(file_layer_id(Path::new("a/b.trk")), "file:a/b.trk");
    }

    #[test]
    fn the_file_palette_cycles() {
        assert_eq!(
            file_palette_colour(0),
            file_palette_colour(FILE_PALETTE.len())
        );
        assert_ne!(file_palette_colour(0), file_palette_colour(1));
        assert!(FILE_PALETTE.iter().all(|c| c[3] == 1.0));
    }

    #[test]
    fn sizes_read_in_megabytes_or_gigabytes() {
        assert_eq!(size_hint(10), "1 MB");
        assert_eq!(size_hint(3_015_912_949), "2.8 GB");
        assert_eq!(size_hint(685_944_962), "654 MB");
    }

    #[test]
    fn outputs_are_found_by_the_conn_entity() {
        let names: Vec<String> = [
            "sub-01_sim-binary_model-simbio_seed-dMRI_conn-Gibbs19_strucmap.nii",
            "sub-01_sim-binary_model-simbio_seed-dMRI_conn-Gibbs19_strucmap.json",
            "sub-01_sim-binary_model-simbio_seed-dMRI_conn-Gibbs19_fibers.mat",
            "sub-01_sim-binary_model-simbio_seed-dMRI_conn-Other_strucmap.nii",
            "sub-02_sim-binary_model-simbio_seed-dMRI_conn-Gibbs19_strucmap.nii",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let o = scan_outputs(&names, "01", true, "Gibbs19");
        assert_eq!(o.maps.len(), 1);
        assert!(o.subset.is_some());
        assert_eq!(scan_outputs(&names, "01", true, "Nope"), Outputs::default());
    }

    #[test]
    fn functional_maps_are_found_by_the_label() {
        let names: Vec<String> = [
            "sub-01_sim-binary_model-simbio_seed-fMRI_conn-GSP1000v2xFullSet_desc-AvgR_funcmap.nii",
            "sub-01_sim-binary_model-simbio_seed-fMRI_conn-GSP1000v2xFullSet_desc-T_funcmap.nii",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let o = scan_outputs(&names, "01", false, "GSP1000v2xFullSet");
        assert_eq!(o.maps.len(), 2);
        assert!(o.subset.is_none());
    }

    #[test]
    fn a_probe_belongs_to_its_subject_label_and_connectome() {
        let p = Probe {
            subject: "01".to_owned(),
            label: "clin".to_owned(),
            spec: Some("Gibbs19".to_owned()),
            ..Probe::default()
        };
        assert!(p.is_for("01", "clin", Some("Gibbs19")));
        assert!(!p.is_for("01", "other", Some("Gibbs19")));
        assert!(!p.is_for("01", "clin", Some("Other")));
        assert!(!p.is_for("02", "clin", Some("Gibbs19")));
        assert!(!p.is_for("01", "clin", None));
        assert!(!Probe::default().is_for("01", "clin", Some("Gibbs19")));
    }

    #[test]
    fn a_probe_reads_the_folder_of_its_own_label() {
        let t = Target {
            dir: None,
            derivatives_root: PathBuf::from("root"),
            subject: "01".to_owned(),
            space: Some("MNI".to_owned()),
            label: "new".to_owned(),
            spec: None,
        };
        let p = probe(&t);
        assert!(p.folder.ends_with(Path::new("stimulations/MNI/new")));
        assert!(p.is_for("01", "new", None));
        assert!(!p.vta);
    }

    #[test]
    fn a_missing_fibre_file_is_an_error() {
        assert!(read_layer(
            Path::new("definitely-missing.mat"),
            "id".to_owned(),
            "label".to_owned(),
            FibreColouring::Strength
        )
        .is_err());
    }
}
