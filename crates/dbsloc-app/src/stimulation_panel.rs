// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::vat::engine::Fail;
use dbsloc_pipeline::vat::settings::{
    self as cfg, ConductivityMode, LabelEntry, LabelState, Mode, ModelKind, Polarity,
    SourceCombination, Stimulation,
};
use dbsloc_pipeline::vat::settings::{Side, Source};
use dbsloc_viz::vta_mesh::{mesh_binary_grid, mesh_vta_nifti, VtaSurface};
use eframe::egui;

use crate::connectivity::{Connectivity, ConnectivityOutput, Inputs};
use crate::follow::{self, Choice, Follow, Prompt, TabState, Target};
use crate::panels::{em, palette, popout_window, PopoutBar};

pub const STIMULATION_PLACEHOLDER: &str =
    "Open a subject with a reconstruction and choose Open stimulation editor on the Volume of activated tissue step.";

pub const STIMULATION_NO_SUBJECT: &str =
    "No subject is selected. Select a subject in the subject list to edit its stimulation.";

pub fn empty_state_text(target: Option<&Target>, notice: Option<&str>) -> (String, Option<String>) {
    match target {
        Some(Target {
            subject: Some(id), ..
        }) => (
            format!(
                "Stimulation · sub-{id}: the stimulation editor cannot be shown for this subject."
            ),
            Some(
                notice
                    .unwrap_or("The subject could not be opened.")
                    .to_owned(),
            ),
        ),
        Some(Target { subject: None, .. }) => (STIMULATION_NO_SUBJECT.to_owned(), None),
        None => (
            STIMULATION_PLACEHOLDER.to_owned(),
            notice.map(str::to_owned),
        ),
    }
}

const POPOUT_SALT: &str = "dbsloc_stimulation_popout";
const POPOUT_TITLE: &str = "Stimulation - Wyss DBS Localizer";
const POPOUT_NAME: &str = "the stimulation editor";

/// Seconds an edit must rest before the preview is recomputed.
pub const PREVIEW_DEBOUNCE_SECONDS: f64 = 0.3;

const SOURCE_COUNT: usize = 4;

/// Splits 100 percent into `n` shares of two decimals that add up to exactly 100.
pub fn equal_shares(n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let each = (10000 / n) as f64 / 100.0;
    let mut shares = vec![each; n];
    let used: f64 = shares[..n - 1].iter().sum();
    shares[n - 1] = ((100.0 - used) * 100.0).round() / 100.0;
    shares
}

/// A label made of the UTC date and time of `secs` since the epoch, `YYYYMMDDHHMMSS`.
pub fn timestamp_label(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn now_label() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    timestamp_label(secs)
}

/// `base` when free, else `base_2`, `base_3`, … within the 64 character limit.
pub fn unique_label(base: &str, existing: &[String]) -> String {
    if !existing.iter().any(|e| e == base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| {
            let suffix = format!("_{n}");
            let keep: String = base
                .chars()
                .take(64usize.saturating_sub(suffix.len()))
                .collect();
            format!("{keep}{suffix}")
        })
        .find(|candidate| !existing.contains(candidate))
        .unwrap_or_else(|| base.to_owned())
}

/// What a debounced preview scheduler asks the caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewStep {
    Wait,
    Start { cancel_previous: bool },
}

/// Debounce and cancel bookkeeping of the live preview, on explicit time.
#[derive(Debug, Default, Clone)]
pub struct Scheduler {
    dirty_at: Option<f64>,
    running: bool,
}

impl Scheduler {
    /// Notes an edit at `now`; `true` when a running preview should be cancelled at once.
    pub fn on_edit(&mut self, now: f64) -> bool {
        self.dirty_at = Some(now);
        self.running
    }

    pub fn step(&mut self, now: f64, debounce: f64) -> PreviewStep {
        match self.dirty_at {
            Some(at) if now - at >= debounce => {
                self.dirty_at = None;
                let cancel_previous = self.running;
                self.running = true;
                PreviewStep::Start { cancel_previous }
            }
            _ => PreviewStep::Wait,
        }
    }

    pub fn finished(&mut self) {
        self.running = false;
    }

    pub fn forget_pending(&mut self) {
        self.dirty_at = None;
    }

    pub fn pending(&self) -> bool {
        self.dirty_at.is_some()
    }

    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running
    }
}

/// The settings being edited, with the editing operations of the Lead-DBS programmer.
#[derive(Debug, Clone)]
pub struct Editor {
    pub stim: Stimulation,
    pub contacts: [usize; 2],
}

impl Editor {
    pub fn new(mut stim: Stimulation, contacts: [usize; 2]) -> Self {
        cfg::normalise(&mut stim, contacts);
        Editor { stim, contacts }
    }

    pub fn source_mut(&mut self, side: Side, source: usize) -> Option<&mut Source> {
        self.stim.sides.get_mut(side).sources.get_mut(source)
    }

    pub fn source(&self, side: Side, source: usize) -> Option<&Source> {
        self.stim.sides.get(side).sources.get(source)
    }

    fn rebalance(src: &mut Source) {
        for pol in [Polarity::Cathode, Polarity::Anode] {
            let case_in = src.case.polarity == pol;
            let members: Vec<usize> = src
                .contacts
                .iter()
                .enumerate()
                .filter(|(_, c)| c.polarity == pol)
                .map(|(i, _)| i)
                .collect();
            let shares = equal_shares(members.len() + usize::from(case_in));
            for (slot, i) in members.iter().enumerate() {
                src.contacts[*i].percent = shares[slot];
            }
            if case_in {
                src.case.percent = shares[members.len()];
            }
        }
        for c in &mut src.contacts {
            if c.polarity == Polarity::Off {
                c.percent = 0.0;
            }
        }
        if src.case.polarity == Polarity::Off {
            src.case.percent = 0.0;
        }
    }

    fn fix_case(src: &mut Source) {
        let anode = src.contacts.iter().any(|c| c.polarity == Polarity::Anode);
        let cathode = src.contacts.iter().any(|c| c.polarity == Polarity::Cathode);
        if anode {
            src.case.polarity = Polarity::Off;
            src.case.percent = 0.0;
        } else if cathode {
            src.case.polarity = Polarity::Anode;
        }
    }

    /// Contact `contact` (1-based) alone is the cathode and the case the anode.
    pub fn set_monopolar(&mut self, side: Side, source: usize, contact: u32) {
        let Some(src) = self.source_mut(side, source) else {
            return;
        };
        for c in &mut src.contacts {
            c.polarity = Polarity::Off;
            c.percent = 0.0;
        }
        if let Some(c) = src.contacts.iter_mut().find(|c| c.index == contact) {
            c.polarity = Polarity::Cathode;
        }
        src.case.polarity = Polarity::Anode;
        Self::rebalance(src);
    }

    /// Sets the polarity of a contact and shares the percentages again.
    pub fn set_polarity(&mut self, side: Side, source: usize, contact: u32, pol: Polarity) {
        let Some(src) = self.source_mut(side, source) else {
            return;
        };
        if let Some(c) = src.contacts.iter_mut().find(|c| c.index == contact) {
            c.polarity = pol;
            if pol == Polarity::Off {
                c.percent = 0.0;
            }
        }
        Self::fix_case(src);
        Self::rebalance(src);
    }

    /// Sets the case polarity and shares the percentages again.
    pub fn set_case(&mut self, side: Side, source: usize, pol: Polarity) {
        let Some(src) = self.source_mut(side, source) else {
            return;
        };
        src.case.polarity = pol;
        Self::rebalance(src);
    }

    /// Types a percentage: above 0 makes an unused contact a cathode, 0 switches it off.
    pub fn set_percent(&mut self, side: Side, source: usize, contact: u32, percent: f64) {
        let Some(src) = self.source_mut(side, source) else {
            return;
        };
        if let Some(c) = src.contacts.iter_mut().find(|c| c.index == contact) {
            c.percent = percent.clamp(0.0, 100.0);
            if c.percent == 0.0 {
                c.polarity = Polarity::Off;
            } else if c.polarity == Polarity::Off {
                c.polarity = Polarity::Cathode;
            }
        }
    }

    pub fn clear_source(&mut self, side: Side, source: usize) {
        let contacts = self.contacts[side.index()];
        let index = source as u32 + 1;
        if let Some(src) = self.source_mut(side, source) {
            let mode = src.mode;
            *src = Source::blank(index, contacts);
            src.mode = mode;
        }
    }

    pub fn has_active_source(&self) -> bool {
        Side::ALL.iter().any(|side| {
            self.stim
                .sides
                .get(*side)
                .sources
                .iter()
                .any(Source::is_active)
        })
    }

    pub fn report(&self) -> cfg::Report {
        if is_group_label(&self.stim.label) {
            let mut stim = self.stim.clone();
            stim.label = "group".to_owned();
            return cfg::check(&stim, self.contacts);
        }
        cfg::check(&self.stim, self.contacts)
    }
}

/// The binary VTA images a computation wrote into one label folder, by side.
pub fn computed_files(dir: &Path) -> Vec<(Side, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let names: Vec<(String, PathBuf)> = rd
        .filter_map(Result::ok)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    let mut out = Vec::new();
    for side in Side::ALL {
        let suffix = format!("_hemi-{}.nii", side.letter());
        let mut found: Vec<&(String, PathBuf)> = names
            .iter()
            .filter(|(n, _)| n.contains("_sim-binary_model-") && n.ends_with(&suffix))
            .collect();
        found.sort();
        if let Some((_, p)) = found.first() {
            out.push((side, p.clone()));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StimFocus {
    pub subject: String,
    pub label: String,
    pub group_label: bool,
}

pub fn is_group_label(label: &str) -> bool {
    label.starts_with("gs_")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelControls {
    pub picker: bool,
    pub actions: bool,
    pub compute: bool,
    pub group_tag: bool,
}

pub fn label_controls(label: &str) -> LabelControls {
    let group = is_group_label(label);
    LabelControls {
        picker: !group,
        actions: !group,
        compute: !group,
        group_tag: group,
    }
}

struct Prepared {
    focus: Option<StimFocus>,
    subject: String,
    subj_dir: PathBuf,
    space: Option<String>,
    contacts: [usize; 2],
    labels: Vec<LabelEntry>,
    first: Option<Stimulation>,
}

fn resolve(opts: &RunOptions) -> Result<dbsloc_pipeline::bids::Subject, String> {
    dbsloc_pipeline::bids::resolve_subject_in(
        &opts.bids_dir,
        opts.output_dir.as_deref(),
        &opts.subject,
        true,
    )
    .map_err(|e| e.to_string())
}

fn prepare(opts: &RunOptions, focus: Option<StimFocus>) -> Result<Prepared, String> {
    let sub = resolve(opts)?;
    let contacts = cfg::contact_counts(&sub)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "The subject has no reconstruction yet. Run Reconstruct first: the contacts of the stimulation come from the electrode model of the lead.".to_owned()
        })?;
    if contacts == [0, 0] {
        return Err("The reconstruction has no lead with an electrode model.".to_owned());
    }
    let subject = sub
        .subj_id
        .strip_prefix("sub-")
        .unwrap_or(&sub.subj_id)
        .to_owned();
    let labels = cfg::list_labels(&sub.subj_dir, &subject);
    let first = match &focus {
        Some(f) => cfg::load(&sub.subj_dir, &subject, &f.label).ok(),
        None => labels
            .iter()
            .find(|l| l.state == LabelState::Canonical)
            .and_then(|l| cfg::load(&sub.subj_dir, &subject, &l.label).ok()),
    };
    Ok(Prepared {
        focus,
        space: sub.norm.template_space.clone(),
        subject,
        subj_dir: sub.subj_dir.clone(),
        contacts,
        labels,
        first,
    })
}

struct PreviewSideResult {
    side: Side,
    raw_mm3: f64,
    final_mm3: f64,
    seconds: f64,
    basis_cache: &'static str,
    backend: &'static str,
    surface: Option<VtaSurface>,
}

enum PreviewMsg {
    Done {
        generation: u64,
        sides: Vec<PreviewSideResult>,
        warnings: Vec<String>,
        elapsed: f64,
        in_scene: bool,
    },
    Failed {
        generation: u64,
        message: String,
    },
}

fn fail_text(fail: &Fail) -> String {
    match fail {
        Fail::Cancelled => "cancelled".to_owned(),
        Fail::Code { code, message } => format!("{code}: {message}"),
    }
}

fn run_preview(
    opts: &RunOptions,
    stim: &Stimulation,
    generation: u64,
    cancel: &AtomicBool,
) -> Option<PreviewMsg> {
    let started = Instant::now();
    let sub = match resolve(opts) {
        Ok(s) => s,
        Err(message) => {
            return Some(PreviewMsg::Failed {
                generation,
                message,
            })
        }
    };
    let params = dbsloc_pipeline::vat::stage::VatParams::default();
    let result =
        dbsloc_pipeline::vat::preview::preview(&sub, opts, stim, &params, true, Some(cancel));
    let preview = match result {
        Ok(p) => p,
        Err(Fail::Cancelled) => return None,
        Err(fail) => {
            return Some(PreviewMsg::Failed {
                generation,
                message: fail_text(&fail),
            })
        }
    };
    let warp = dbsloc_pipeline::vat::warp::SpaceWarp::load(&sub).ok();
    let mut sides = Vec::new();
    for (side, outcome) in &preview.sides {
        let field = &outcome.field;
        let surface = mesh_binary_grid(field.binary.dims, &field.binary.data, &field.affine)
            .ok()
            .filter(|m| !m.positions.is_empty())
            .and_then(|mut mesh| {
                let warp = warp.as_ref()?;
                let points: Vec<[f64; 3]> = mesh
                    .positions
                    .iter()
                    .map(|p| [f64::from(p.x), f64::from(p.y), f64::from(p.z)])
                    .collect();
                let mapped = warp.points(&points).ok()?;
                for (dst, src) in mesh.positions.iter_mut().zip(mapped) {
                    *dst = glam_vec(src);
                }
                mesh.recompute_vertex_normals();
                Some(VtaSurface {
                    side: side.name().to_owned(),
                    label: stim.label.clone(),
                    mesh,
                })
            });
        sides.push(PreviewSideResult {
            side: *side,
            raw_mm3: field.volumes.raw_mm3,
            final_mm3: field.volumes.final_mm3,
            seconds: outcome.seconds,
            basis_cache: outcome.basis_cache,
            backend: outcome.backend,
            surface,
        });
    }
    Some(PreviewMsg::Done {
        generation,
        sides,
        warnings: preview.warnings,
        elapsed: started.elapsed().as_secs_f64(),
        in_scene: warp.is_some(),
    })
}

fn glam_vec(p: [f64; 3]) -> dbsloc_viz::vta_mesh::Vec3 {
    dbsloc_viz::vta_mesh::Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Switch(String),
    Delete,
}

#[derive(Default)]
pub struct StimOutput {
    pub compute: Option<String>,
    pub errors: Vec<(String, String)>,
    pub log: Vec<String>,
    pub focus: bool,
    pub saved: bool,
    pub fibre_layer: Option<dbsloc_viz::scene_layers::FibreLayer>,
    pub open_connectomes: bool,
    pub remember_connectome: Option<String>,
}

impl StimOutput {
    pub fn merge(&mut self, other: StimOutput) {
        self.saved |= other.saved;
        if other.compute.is_some() {
            self.compute = other.compute;
        }
        self.errors.extend(other.errors);
        self.log.extend(other.log);
        self.focus |= other.focus;
        self.open_connectomes |= other.open_connectomes;
        if other.fibre_layer.is_some() {
            self.fibre_layer = other.fibre_layer;
        }
        if other.remember_connectome.is_some() {
            self.remember_connectome = other.remember_connectome;
        }
    }
}

/// What the application tells the tab each frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct StimEnv {
    pub run_active: bool,
    pub batch_active: bool,
    pub run_wait: Option<&'static str>,
    pub foreign_subject: bool,
    pub subject_selected: bool,
}

pub fn stim_target_matches(
    bound_dir: &Path,
    bound_subject: &str,
    bids_dir: &Path,
    selected: Option<&str>,
) -> bool {
    let bare = |s: &str| s.strip_prefix("sub-").unwrap_or(s).to_owned();
    bound_dir == bids_dir && selected.map(bare) == Some(bare(bound_subject))
}

struct PreviewRun {
    generation: u64,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<PreviewMsg>,
    started: Instant,
}

struct PreviewDone {
    sides: Vec<(Side, f64, f64, f64, &'static str, &'static str)>,
    warnings: Vec<String>,
    elapsed: f64,
    in_scene: bool,
}

struct Session {
    opts: RunOptions,
    subject: String,
    subj_dir: PathBuf,
    space: Option<String>,
    labels: Vec<LabelEntry>,
    editor: Editor,
    saved: Option<Stimulation>,
    pristine: Option<Stimulation>,
    side: Side,
    source: [usize; 2],
    new_label: Option<String>,
    confirm: Option<Pending>,
    preview_on: bool,
    sched: Scheduler,
    run: Option<PreviewRun>,
    generation: u64,
    last: Option<PreviewDone>,
    last_error: Option<String>,
    status: Option<String>,
    awaiting: Option<String>,
    loading: Option<mpsc::Receiver<Vec<VtaSurface>>>,
    computed: Vec<VtaSurface>,
    preview_surfaces: Vec<VtaSurface>,
    scene_dirty: bool,
    conn: Connectivity,
}

enum Phase {
    Idle,
    Preparing(mpsc::Receiver<Result<(Box<Prepared>, RunOptions), String>>),
    Open(Box<Session>),
}

pub struct StimulationTab {
    phase: Phase,
    popped_out: bool,
    notice: Option<String>,
    serial: u64,
    scene: Vec<VtaSurface>,
    preview_default: bool,
    connectomes_dir: Option<PathBuf>,
    connectome_choice: Option<String>,
    connectome_cache: dbsloc_pipeline::connectome_cache::CacheSetting,
    target: Option<Target>,
    prompt: Option<Prompt>,
    stay_on: Option<Target>,
    discarded: bool,
    stale: bool,
}

impl Default for StimulationTab {
    fn default() -> Self {
        StimulationTab {
            phase: Phase::Idle,
            popped_out: false,
            target: None,
            prompt: None,
            stay_on: None,
            discarded: false,
            stale: false,
            notice: None,
            serial: 0,
            scene: Vec::new(),
            preview_default: false,
            connectomes_dir: None,
            connectome_choice: None,
            connectome_cache: dbsloc_pipeline::connectome_cache::CacheSetting::default(),
        }
    }
}

impl Session {
    fn label_names(&self) -> Vec<String> {
        self.labels.iter().map(|l| l.label.clone()).collect()
    }

    fn dirty(&self) -> bool {
        self.saved.as_ref() != Some(&self.editor.stim)
    }

    fn has_edits(&self) -> bool {
        self.dirty() && self.pristine.as_ref() != Some(&self.editor.stim)
    }

    fn load_label(&mut self, label: &str) -> Result<(), String> {
        let stim = cfg::load(&self.subj_dir, &self.subject, label).map_err(|e| e.to_string())?;
        self.editor = Editor::new(stim, self.editor.contacts);
        self.saved = Some(self.editor.stim.clone());
        self.after_label_change();
        Ok(())
    }

    fn begin_label(&mut self, label: String, base: Option<Stimulation>) {
        let contacts = self.editor.contacts;
        let mut stim = base.unwrap_or_else(|| {
            Stimulation::blank(&label, &self.subject, ModelKind::Efield, contacts)
        });
        stim.label = label;
        stim.subject = self.subject.clone();
        stim.provenance = cfg::Provenance::default();
        self.editor = Editor::new(stim, contacts);
        self.saved = None;
        self.after_label_change();
    }

    fn after_label_change(&mut self) {
        self.cancel_preview();
        self.conn.cancel();
        self.conn.invalidate();
        self.last = None;
        self.last_error = None;
        self.preview_surfaces.clear();
        self.scene_dirty = true;
        self.status = None;
        self.sched.forget_pending();
        if self.preview_on {
            self.sched.on_edit(f64::NEG_INFINITY);
        }
    }

    fn cancel_preview(&mut self) {
        if let Some(run) = &self.run {
            run.cancel.store(true, Ordering::SeqCst);
        }
        self.run = None;
        self.sched.finished();
        self.generation += 1;
    }

    fn start_preview(&mut self, ctx: &egui::Context) {
        if let Some(run) = &self.run {
            run.cancel.store(true, Ordering::SeqCst);
        }
        self.generation += 1;
        let generation = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let opts = self.opts.clone();
        let mut stim = self.editor.stim.clone();
        stim.subject = self.subject.clone();
        let worker_cancel = Arc::clone(&cancel);
        let repaint = ctx.clone();
        thread::spawn(move || {
            let message = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_preview(&opts, &stim, generation, &worker_cancel)
            }));
            let message = match message {
                Ok(m) => m,
                Err(_) => Some(PreviewMsg::Failed {
                    generation,
                    message: "the preview stopped unexpectedly".to_owned(),
                }),
            };
            if let Some(m) = message {
                let _ = tx.send(m);
            }
            repaint.request_repaint();
        });
        self.run = Some(PreviewRun {
            generation,
            cancel,
            rx,
            started: Instant::now(),
        });
        self.last_error = None;
    }

    fn poll_preview(&mut self) -> bool {
        let Some(run) = &self.run else {
            return false;
        };
        let msg = match run.rx.try_recv() {
            Ok(m) => m,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                if run.cancel.load(Ordering::SeqCst) {
                    self.run = None;
                    self.sched.finished();
                    return false;
                }
                PreviewMsg::Failed {
                    generation: run.generation,
                    message: "the preview ended without a result".to_owned(),
                }
            }
        };
        let current = self.run.as_ref().map(|r| r.generation);
        match msg {
            PreviewMsg::Done {
                generation,
                sides,
                warnings,
                elapsed,
                in_scene,
            } if Some(generation) == current => {
                self.run = None;
                self.sched.finished();
                self.preview_surfaces = sides
                    .iter()
                    .filter_map(|s| s.surface.clone())
                    .collect::<Vec<_>>();
                self.last = Some(PreviewDone {
                    sides: sides
                        .iter()
                        .map(|s| {
                            (
                                s.side,
                                s.raw_mm3,
                                s.final_mm3,
                                s.seconds,
                                s.basis_cache,
                                s.backend,
                            )
                        })
                        .collect(),
                    warnings,
                    elapsed,
                    in_scene,
                });
                true
            }
            PreviewMsg::Failed {
                generation,
                message,
            } if Some(generation) == current => {
                self.run = None;
                self.sched.finished();
                self.last = None;
                self.last_error = Some(message);
                self.preview_surfaces.clear();
                true
            }
            _ => false,
        }
    }
}

impl StimulationTab {
    pub fn set_connectome_context(
        &mut self,
        dir: Option<&Path>,
        choice: Option<&str>,
        cache: dbsloc_pipeline::connectome_cache::CacheSetting,
    ) {
        self.connectome_cache = cache;
        if self.connectomes_dir.as_deref() != dir {
            self.connectomes_dir = dir.map(Path::to_path_buf);
        }
        if self.connectome_choice.as_deref() != choice {
            self.connectome_choice = choice.map(str::to_owned);
        }
    }

    /// Opens the next session with the preview already switched on.
    pub fn set_preview_default(&mut self, on: bool) {
        self.preview_default = on;
    }

    /// `true` once the editor is open and no preview is waiting or running.
    pub fn is_settled(&self) -> bool {
        match &self.phase {
            Phase::Open(s) => {
                s.run.is_none() && !s.sched.pending() && s.loading.is_none() && s.awaiting.is_none()
            }
            Phase::Preparing(_) => false,
            Phase::Idle => true,
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self.phase, Phase::Open(_))
    }

    pub fn is_preparing(&self) -> bool {
        matches!(self.phase, Phase::Preparing(_))
    }

    pub fn is_dirty(&self) -> bool {
        match &self.phase {
            Phase::Open(s) => !self.discarded && s.has_edits(),
            _ => false,
        }
    }

    #[cfg(test)]
    pub fn target(&self) -> Option<&Target> {
        self.target.as_ref()
    }

    pub fn is_busy(&self) -> bool {
        match &self.phase {
            Phase::Open(s) => s.awaiting.is_some() || s.loading.is_some() || s.conn.is_busy(),
            _ => false,
        }
    }

    pub fn stayed_on(&self, selected: &Target) -> bool {
        self.stay_on.as_ref() == Some(selected)
    }

    pub fn set_prompt(&mut self, prompt: Option<Prompt>) {
        if prompt.is_none() {
            self.stay_on = None;
            self.discarded = false;
        }
        if prompt.is_some() != self.prompt.is_some() {
            self.serial += 1;
        }
        self.prompt = prompt;
    }

    pub fn show_empty(&mut self, target: Target, notice: Option<String>) {
        if let Phase::Open(s) = &mut self.phase {
            s.cancel_preview();
            s.conn.cancel();
        }
        self.phase = Phase::Idle;
        self.popped_out = false;
        self.clear_scene();
        self.target = Some(target);
        self.notice = notice;
        self.prompt = None;
        self.stay_on = None;
        self.discarded = false;
        self.stale = false;
    }

    pub fn mark_stale(&mut self) {
        if matches!(self.phase, Phase::Idle) {
            self.stale = true;
        }
    }

    pub fn forget_target(&mut self) {
        if matches!(self.phase, Phase::Idle) {
            self.target = None;
            self.notice = None;
            self.prompt = None;
            self.stay_on = None;
            self.discarded = false;
            self.stale = false;
        }
    }

    pub fn follow_selection(
        &mut self,
        selected: &Target,
        run_active: bool,
        ready: bool,
        ctx: &egui::Context,
        opts: impl FnOnce(&Target) -> Result<RunOptions, String>,
    ) -> Follow {
        let decision = follow::decide(
            self.target.as_ref(),
            selected,
            TabState {
                dirty: self.is_dirty(),
                busy: run_active || self.is_busy(),
                ready,
                stayed: self.stayed_on(selected),
                stale: self.stale,
            },
        );
        match decision {
            Follow::Idle => self.set_prompt(None),
            Follow::Load => match opts(selected) {
                Ok(o) => self.open(o, None, ctx),
                Err(e) => self.show_empty(selected.clone(), Some(e)),
            },
            Follow::Clear => self.show_empty(selected.clone(), None),
            other => self.set_prompt(Some(Prompt {
                follow: other,
                selected: selected.clone(),
            })),
        }
        decision
    }

    pub fn apply_choice(&mut self, choice: Choice, out: &mut StimOutput) {
        match choice {
            Choice::SaveAndSwitch => {
                let Phase::Open(s) = &mut self.phase else {
                    return;
                };
                if s.editor.report().errors.is_empty() && (!s.has_edits() || save_now(s, out)) {
                    self.stay_on = None;
                }
            }
            Choice::DiscardAndSwitch => {
                self.stay_on = None;
                self.discarded = true;
            }
            Choice::Stay => {
                self.stay_on = self.prompt.as_ref().map(|p| p.selected.clone());
            }
            Choice::Switch => self.stay_on = None,
        }
    }

    /// Starts opening the editor for the subject of `opts` on a worker.
    pub fn open(&mut self, opts: RunOptions, focus: Option<StimFocus>, ctx: &egui::Context) {
        if let Phase::Open(s) = &mut self.phase {
            s.cancel_preview();
            s.conn.cancel();
        }
        self.clear_scene();
        self.target = Some(Target::new(opts.bids_dir.clone(), Some(&opts.subject)));
        self.prompt = None;
        self.stay_on = None;
        self.discarded = false;
        self.stale = false;
        let (tx, rx) = mpsc::channel();
        self.phase = Phase::Preparing(rx);
        self.notice = None;
        let repaint = ctx.clone();
        thread::spawn(move || {
            let out = prepare(&opts, focus).map(|p| (Box::new(p), opts));
            let _ = tx.send(out);
            repaint.request_repaint();
        });
    }

    pub fn close(&mut self) {
        if let Phase::Open(s) = &mut self.phase {
            s.cancel_preview();
            s.conn.cancel();
        }
        self.phase = Phase::Idle;
        self.popped_out = false;
        self.clear_scene();
        self.target = None;
        self.prompt = None;
        self.stay_on = None;
        self.discarded = false;
        self.stale = false;
    }

    fn clear_scene(&mut self) {
        if !self.scene.is_empty() {
            self.scene.clear();
            self.serial += 1;
        }
    }

    /// The surfaces the 3D viewer should draw, with a counter that changes with them.
    pub fn scene_surfaces(&self) -> (u64, &[VtaSurface]) {
        if self.prompt.is_some() {
            return (self.serial, &[]);
        }
        (self.serial, &self.scene)
    }

    pub fn bound_subject(&self) -> Option<(&Path, &str)> {
        match &self.phase {
            Phase::Open(s) => Some((s.opts.bids_dir.as_path(), s.subject.as_str())),
            _ => None,
        }
    }

    /// A pipeline step finished; loads the written VTA images after the `vat` step.
    pub fn stage_completed(
        &mut self,
        stage: &str,
        run_owner: Option<(&Path, &str)>,
        ctx: &egui::Context,
    ) {
        let Phase::Open(s) = &mut self.phase else {
            return;
        };
        if stage != dbsloc_pipeline::stages::Stage::Vat.label() {
            return;
        }
        let ours = run_owner.is_some_and(|(dir, subject)| {
            stim_target_matches(&s.opts.bids_dir, &s.subject, dir, Some(subject))
        });
        if !ours {
            return;
        }
        s.conn.invalidate();
        let Some(label) = s.awaiting.take() else {
            return;
        };
        let Some(space) = s.space.clone() else {
            s.status = Some("The VTA was computed in native space only.".to_owned());
            return;
        };
        let dir = cfg::label_dir(&s.subj_dir, &space, &label);
        let (tx, rx) = mpsc::channel();
        s.loading = Some(rx);
        let repaint = ctx.clone();
        thread::spawn(move || {
            let surfaces: Vec<VtaSurface> = computed_files(&dir)
                .into_iter()
                .filter_map(|(side, path)| {
                    let mesh = mesh_vta_nifti(&path).ok()?;
                    Some(VtaSurface {
                        side: side.name().to_owned(),
                        label: label.clone(),
                        mesh,
                    })
                })
                .collect();
            let _ = tx.send(surfaces);
            repaint.request_repaint();
        });
    }

    /// The run ended; forgets a computation that never reported.
    pub fn run_ended(&mut self) {
        if let Phase::Open(s) = &mut self.phase {
            if s.awaiting.take().is_some() {
                s.status = None;
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let ready = match &self.phase {
            Phase::Preparing(rx) => match rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("the preparation stopped unexpectedly".to_owned()))
                }
            },
            _ => None,
        };
        if let Some(result) = ready {
            match result {
                Ok((p, opts)) => {
                    let p = *p;
                    let focused = p.focus.is_some();
                    let editor = match p.first {
                        Some(stim) => Editor::new(stim, p.contacts),
                        None => {
                            let existing: Vec<String> =
                                p.labels.iter().map(|l| l.label.clone()).collect();
                            let label = match &p.focus {
                                Some(f) => f.label.clone(),
                                None => unique_label(&now_label(), &existing),
                            };
                            Editor::new(
                                Stimulation::blank(
                                    &label,
                                    &p.subject,
                                    ModelKind::Efield,
                                    p.contacts,
                                ),
                                p.contacts,
                            )
                        }
                    };
                    let on_disk =
                        p.labels.iter().any(|l| {
                            l.label == editor.stim.label && l.state == LabelState::Canonical
                        }) || (focused
                            && cfg::load(&p.subj_dir, &p.subject, &editor.stim.label).is_ok());
                    let saved = on_disk.then(|| editor.stim.clone());
                    let pristine = (!on_disk).then(|| editor.stim.clone());
                    let side = Side::ALL
                        .into_iter()
                        .find(|s| p.contacts[s.index()] > 0)
                        .unwrap_or(Side::Right);
                    let mut sched = Scheduler::default();
                    if self.preview_default {
                        sched.on_edit(f64::NEG_INFINITY);
                    }
                    self.phase = Phase::Open(Box::new(Session {
                        opts,
                        subject: p.subject,
                        subj_dir: p.subj_dir,
                        space: p.space,
                        labels: p.labels,
                        editor,
                        saved,
                        pristine,
                        side,
                        source: [0, 0],
                        new_label: None,
                        confirm: None,
                        preview_on: self.preview_default,
                        sched,
                        run: None,
                        generation: 0,
                        last: None,
                        last_error: None,
                        status: None,
                        awaiting: None,
                        loading: None,
                        computed: Vec::new(),
                        preview_surfaces: Vec::new(),
                        scene_dirty: false,
                        conn: Connectivity::default(),
                    }));
                }
                Err(e) => {
                    self.phase = Phase::Idle;
                    self.notice = Some(e);
                }
            }
            ctx.request_repaint();
        }
        let mut changed = false;
        if let Phase::Open(s) = &mut self.phase {
            changed |= s.poll_preview();
            changed |= std::mem::take(&mut s.scene_dirty);
            if let Some(rx) = &s.loading {
                match rx.try_recv() {
                    Ok(surfaces) => {
                        s.loading = None;
                        s.status = Some(if surfaces.is_empty() {
                            "The computation wrote no VTA for this stimulation (no active source)."
                                .to_owned()
                        } else {
                            "VTA computed and loaded into the 3D viewer.".to_owned()
                        });
                        s.computed = surfaces;
                        changed = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => s.loading = None,
                }
            }
            if changed {
                let shown: Vec<VtaSurface> = if s.preview_on && !s.preview_surfaces.is_empty() {
                    s.preview_surfaces.clone()
                } else {
                    s.computed.clone()
                };
                self.scene = shown;
                self.serial += 1;
            }
        }
    }

    pub fn pop_out(&mut self) {
        self.popped_out = self.is_open();
    }

    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, env: StimEnv) -> StimOutput {
        self.poll(ctx);
        let mut out = StimOutput::default();
        if !self.is_open() {
            self.popped_out = false;
            self.placeholder(ui);
            return out;
        }
        if self.popped_out {
            self.popped_notice(ui);
            return out;
        }
        self.session_ui(ui, None, env, &mut out);
        out
    }

    pub fn show_popout(&mut self, ctx: &egui::Context, env: StimEnv) -> StimOutput {
        let mut out = StimOutput::default();
        if !self.popped_out {
            return out;
        }
        if !self.is_open() {
            self.popped_out = false;
            ctx.request_repaint();
            return out;
        }
        let bring_back = popout_window(
            ctx,
            POPOUT_SALT,
            POPOUT_TITLE,
            POPOUT_NAME,
            true,
            |ui, bar| {
                self.session_ui(ui, Some(bar), env, &mut out);
            },
        );
        if bring_back {
            self.popped_out = false;
            out.focus = true;
            ctx.request_repaint();
        }
        out
    }

    fn placeholder(&self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.vertical_centered(|ui| {
            ui.add_space(unit * 2.0);
            if self.is_preparing() {
                ui.add(egui::Spinner::new().size(unit * 1.5));
                let text = match self.target.as_ref().and_then(Target::name) {
                    Some(name) => format!("Opening the stimulation editor for {name}…"),
                    None => "Opening the stimulation editor…".to_owned(),
                };
                ui.add(
                    egui::Label::new(egui::RichText::new(text).color(pal.text_muted))
                        .selectable(false),
                );
                return;
            }
            let (text, notice) = empty_state_text(self.target.as_ref(), self.notice.as_deref());
            ui.add(
                egui::Label::new(egui::RichText::new(text).color(pal.text_muted))
                    .wrap()
                    .selectable(false),
            );
            if let Some(notice) = &notice {
                ui.add_space(unit * 0.4);
                ui.add(
                    egui::Label::new(egui::RichText::new(notice).small().color(pal.warn))
                        .wrap()
                        .selectable(false),
                );
            }
            if let Some(note) = self
                .prompt
                .as_ref()
                .and_then(|p| follow::placeholder_note(&follow::STIMULATION_WORDING, p))
            {
                ui.add_space(unit * 0.4);
                ui.add(
                    egui::Label::new(egui::RichText::new(note).small().color(pal.text_muted))
                        .wrap()
                        .selectable(false),
                );
            }
        });
    }

    fn popped_notice(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.vertical_centered(|ui| {
            ui.add_space(unit * 2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new("The stimulation editor is open in a separate window.")
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
            ui.add_space(unit * 0.5);
            if ui
                .button("Dock to main window")
                .on_hover_text(
                    "Close the separate window and show the stimulation editor here again",
                )
                .clicked()
            {
                self.popped_out = false;
                ui.ctx().request_repaint();
            }
        });
    }

    fn session_ui(
        &mut self,
        ui: &mut egui::Ui,
        mut bar: Option<&mut PopoutBar>,
        env: StimEnv,
        out: &mut StimOutput,
    ) {
        let now = ui.input(|i| i.time);
        let ctx = ui.ctx().clone();
        let mut pop = false;
        let mut close = false;
        let conn_dir = self.connectomes_dir.clone();
        let conn_choice = self.connectome_choice.clone();
        let conn_cache = self.connectome_cache;
        let prompt = self.prompt.clone();
        let discarded = self.discarded;
        let mut choice = None;
        let Phase::Open(s) = &mut self.phase else {
            return;
        };
        let pal = palette(ui);
        let unit = em(ui);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("Stimulation · sub-{}", s.subject)).strong(),
                )
                .selectable(false),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Edit the settings of a lead like the Lead-DBS programmer; Save writes them, Compute VTA runs the step.",
                    )
                    .small()
                    .color(pal.text_muted),
                )
                .selectable(false),
            );
            if let Some(bar) = bar.as_deref_mut() {
                bar.show(ui);
            }
            if bar.is_none()
                && ui
                    .button("Open stimulation editor in separate window")
                    .on_hover_text("Show the stimulation editor in a separate window")
                    .clicked()
            {
                pop = true;
            }
            if ui
                .button("Close")
                .on_hover_text("Close the stimulation editor; unsaved settings are lost")
                .clicked()
            {
                close = true;
            }
        });
        if let Some(p) = &prompt {
            let save_blocked = (!s.editor.report().errors.is_empty())
                .then_some("Fix the errors in the settings first, or discard them.");
            choice = follow::banner(
                ui,
                &follow::STIMULATION_WORDING,
                &s.subject,
                p,
                s.has_edits() && !discarded,
                save_blocked,
            );
        }
        if discarded {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(follow::discarded_text(
                        &follow::STIMULATION_WORDING,
                        &s.subject,
                    ))
                    .small()
                    .color(pal.text_muted),
                )
                .wrap(),
            );
            ui.disable();
        }
        ui.separator();
        let before = s.editor.stim.clone();
        egui::Panel::bottom("stimulation_footer")
            .resizable(false)
            .show(ui, |ui| {
                ui.add_space(unit * 0.2);
                validation_area(s, ui, &pal);
                preview_area(s, ui, &ctx, &pal, env, now);
                action_row(s, ui, env, out);
                if let Some(status) = &s.status {
                    ui.add(
                        egui::Label::new(egui::RichText::new(status).small().color(pal.ok)).wrap(),
                    );
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("stimulation_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        label_row(s, ui, out);
                        confirm_row(s, ui, &pal, out);
                        model_row(s, ui);
                        ui.add_space(unit * 0.3);
                        side_tabs(s, ui);
                        source_panel(s, ui, &pal);
                        ui.add_space(unit * 0.3);
                        model_settings_section(s, ui);
                        ui.add_space(unit * 0.3);
                        connectivity_section(
                            s,
                            ui,
                            &ctx,
                            env,
                            conn_dir.as_deref(),
                            conn_choice.as_deref(),
                            conn_cache,
                            out,
                        );
                    });
            });
        if s.editor.stim != before {
            if s.sched.on_edit(now) {
                if let Some(run) = &s.run {
                    run.cancel.store(true, Ordering::SeqCst);
                }
            }
            s.status = None;
        }
        if s.preview_on && !env.run_active && s.sched.pending() {
            match s.sched.step(now, PREVIEW_DEBOUNCE_SECONDS) {
                PreviewStep::Start { .. } => {
                    if s.editor.report().errors.is_empty() && s.editor.has_active_source() {
                        s.start_preview(&ctx);
                    } else {
                        s.sched.finished();
                        s.cancel_preview();
                        s.preview_surfaces.clear();
                        s.scene_dirty = true;
                        s.last = None;
                    }
                }
                PreviewStep::Wait => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        if s.run.is_some() || s.loading.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if pop {
            self.popped_out = true;
            ctx.request_repaint();
        }
        if close {
            self.close();
        }
        if let Some(c) = choice {
            self.apply_choice(c, out);
            ctx.request_repaint();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn connectivity_section(
    s: &mut Session,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    env: StimEnv,
    connectomes_dir: Option<&Path>,
    remembered: Option<&str>,
    cache: dbsloc_pipeline::connectome_cache::CacheSetting,
    out: &mut StimOutput,
) {
    let Some(root) = s.subj_dir.parent().map(Path::to_path_buf) else {
        return;
    };
    let mut co = ConnectivityOutput::default();
    let inputs = Inputs {
        derivatives_root: &root,
        subject: &s.subject,
        space: s.space.as_deref(),
        label: &s.editor.stim.label,
        saved: !s.dirty(),
        run_active: env.run_active,
        connectomes_dir,
        remembered,
        cache,
    };
    s.conn.show(ui, ctx, &inputs, &mut co);
    out.log.extend(co.log);
    out.errors.extend(co.errors);
    out.open_connectomes |= co.open_connectomes;
    if co.fibre_layer.is_some() {
        out.fibre_layer = co.fibre_layer;
    }
    if co.remember.is_some() {
        out.remember_connectome = co.remember;
    }
}

fn label_row(s: &mut Session, ui: &mut egui::Ui, out: &mut StimOutput) {
    let names = s.label_names();
    let entries = s.labels.clone();
    let controls = label_controls(&s.editor.stim.label);
    if !controls.picker {
        ui.horizontal_wrapped(|ui| {
            ui.label("Label:");
            ui.label(s.editor.stim.label.clone());
            if controls.group_tag {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("Group stimulation")
                            .small()
                            .color(palette(ui).text_muted),
                    )
                    .selectable(false),
                )
                .on_hover_text(
                    "Used by the group analysis; Compute VTAs in the group panel computes it",
                );
            }
        });
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Label:");
        let current = s.editor.stim.label.clone();
        let unsaved = !names.contains(&current);
        egui::ComboBox::from_id_salt("stimulation_label")
            .selected_text(if unsaved {
                format!("{current} (not saved yet)")
            } else {
                current.clone()
            })
            .show_ui(ui, |ui| {
                for entry in &entries {
                    let importable = entry.state == LabelState::Importable;
                    let text = if importable {
                        format!("{} (Lead-DBS only: import it first)", entry.label)
                    } else {
                        entry.label.clone()
                    };
                    let resp = ui
                        .add_enabled(
                            !importable,
                            egui::Button::selectable(entry.label == current, text),
                        )
                        .on_hover_text(if importable {
                            "A Lead-DBS label folder without settings of ours; import it with `dbsloc stim import`"
                        } else {
                            "Open this stimulation"
                        });
                    if resp.clicked() && entry.label != current {
                        if s.dirty() {
                            s.confirm = Some(Pending::Switch(entry.label.clone()));
                        } else if let Err(e) = s.load_label(&entry.label) {
                            out.errors.push(("Stimulation".to_owned(), e));
                        }
                    }
                }
            })
            .response
            .on_hover_text("The stimulations of this subject");
        if ui
            .button("New…")
            .on_hover_text("Start a new stimulation with its own label")
            .clicked()
        {
            s.new_label = Some(unique_label(&now_label(), &names));
        }
        if ui
            .button("Duplicate")
            .on_hover_text("Copy the settings on screen into a new stimulation")
            .clicked()
        {
            let label = unique_label(&format!("{current}_copy"), &names);
            let base = s.editor.stim.clone();
            s.begin_label(label, Some(base));
        }
        if ui
            .add_enabled(!unsaved, egui::Button::new("Delete"))
            .on_hover_text("Delete this stimulation and its results from the output directory")
            .clicked()
        {
            s.confirm = Some(Pending::Delete);
        }
    });
    let mut create = None;
    let mut cancel = false;
    if let Some(text) = &mut s.new_label {
        ui.horizontal_wrapped(|ui| {
            ui.label("New label:");
            ui.add(egui::TextEdit::singleline(text).desired_width(em(ui) * 14.0))
                .on_hover_text("1 to 64 letters, digits, dots, underscores or hyphens");
            let valid =
                cfg::label_is_valid(text) && !text.starts_with("gs_") && !names.contains(text);
            if ui
                .add_enabled(valid, egui::Button::new("Create"))
                .on_hover_text("Create the stimulation with this label")
                .clicked()
            {
                create = Some(text.clone());
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if !valid {
                ui.label(
                    egui::RichText::new("Use a free label of letters, digits, . _ -")
                        .small()
                        .color(palette(ui).text_muted),
                );
            }
        });
    }
    if cancel {
        s.new_label = None;
    }
    if let Some(label) = create {
        s.new_label = None;
        if s.dirty() {
            s.confirm = Some(Pending::Switch(label));
        } else {
            s.begin_label(label, None);
        }
    }
}

fn confirm_row(
    s: &mut Session,
    ui: &mut egui::Ui,
    pal: &crate::app::tokens::Palette,
    out: &mut StimOutput,
) {
    let Some(pending) = s.confirm.clone() else {
        return;
    };
    let (text, yes) = match &pending {
        Pending::Switch(_) => (
            format!(
                "The settings of {} have unsaved changes.",
                s.editor.stim.label
            ),
            "Discard the changes",
        ),
        Pending::Delete => (
            format!(
                "Delete {} and every result written for it?",
                s.editor.stim.label
            ),
            "Delete",
        ),
    };
    ui.horizontal_wrapped(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(text).color(pal.warn)).selectable(false));
        if ui.button(yes).clicked() {
            s.confirm = None;
            match pending {
                Pending::Switch(label) => {
                    if s.labels.iter().any(|l| l.label == label) {
                        if let Err(e) = s.load_label(&label) {
                            out.errors.push(("Stimulation".to_owned(), e));
                        }
                    } else {
                        s.begin_label(label, None);
                    }
                }
                Pending::Delete => delete_label(s, out),
            }
        }
        if ui.button("Cancel").clicked() {
            s.confirm = None;
        }
    });
}

fn delete_label(s: &mut Session, out: &mut StimOutput) {
    let label = s.editor.stim.label.clone();
    if !cfg::label_is_valid(&label) {
        return;
    }
    let root = cfg::stimulations_dir(&s.subj_dir);
    let mut failed = Vec::new();
    if let Ok(spaces) = std::fs::read_dir(&root) {
        for space in spaces.filter_map(Result::ok) {
            let dir = space.path().join(&label);
            if dir.is_dir() {
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    failed.push(format!("{}: {e}", dir.display()));
                }
            }
        }
    }
    if !failed.is_empty() {
        out.errors.push((
            "Could not delete the stimulation".to_owned(),
            failed.join("\n"),
        ));
    }
    out.log.push(format!("Stimulation {label} deleted"));
    s.labels = cfg::list_labels(&s.subj_dir, &s.subject);
    let next = s
        .labels
        .iter()
        .find(|l| l.state == LabelState::Canonical)
        .map(|l| l.label.clone());
    match next {
        Some(l) => {
            if s.load_label(&l).is_err() {
                s.begin_label(unique_label(&now_label(), &s.label_names()), None);
            }
        }
        None => s.begin_label(unique_label(&now_label(), &s.label_names()), None),
    }
    s.computed.clear();
}

fn model_row(s: &mut Session, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Model:");
        let current = s.editor.stim.model;
        egui::ComboBox::from_id_salt("stimulation_model")
            .selected_text(model_text(current))
            .show_ui(ui, |ui| {
                for kind in ModelKind::ALL {
                    ui.selectable_value(&mut s.editor.stim.model, kind, model_text(kind))
                        .on_hover_text(model_hover(kind));
                }
            })
            .response
            .on_hover_text(model_hover(current));
        dbsloc_viz::widgets::info_icon(ui, model_hover(current));
    });
}

fn model_id(kind: ModelKind) -> &'static str {
    match kind {
        ModelKind::Efield => "efield",
        ModelKind::Kuncel => "kuncel",
        ModelKind::Maedler => "maedler",
        ModelKind::Dembek => "dembek",
        ModelKind::Fastfield => "fastfield",
        ModelKind::Ossdbs => "ossdbs",
    }
}

/// Display name and id of a model as the combo shows it.
pub fn model_text(kind: ModelKind) -> String {
    let id = model_id(kind);
    match kind {
        ModelKind::Efield => format!("{} ({id}, default)", kind.display_name()),
        ModelKind::Fastfield | ModelKind::Ossdbs => {
            format!("{} ({id}, import only)", kind.display_name())
        }
        _ => format!("{} ({id}, approximate sphere)", kind.display_name()),
    }
}

fn model_hover(kind: ModelKind) -> &'static str {
    match kind {
        ModelKind::Efield => {
            "Volume-conductor field computed in this program; settings changes update the preview from a cached per-contact basis."
        }
        ModelKind::Kuncel => "Analytical sphere, voltage mode, one contact per source.",
        ModelKind::Maedler => "Analytical sphere from voltage and impedance, one contact per source.",
        ModelKind::Dembek => "Analytical sphere from a current density threshold, one contact per source.",
        ModelKind::Fastfield => {
            "Settings of this model can be imported but not computed; computing asks for the E-field model."
        }
        ModelKind::Ossdbs => {
            "Results of this model can be read but not computed; computing asks for the E-field model."
        }
    }
}

fn side_tabs(s: &mut Session, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        for side in Side::ALL {
            let n = s.editor.contacts[side.index()];
            let active = s
                .editor
                .stim
                .sides
                .get(side)
                .sources
                .iter()
                .filter(|src| src.is_active())
                .count();
            let title = match side {
                Side::Right => "Right",
                Side::Left => "Left",
            };
            let text = if n == 0 {
                format!("{title} (no lead)")
            } else if active > 0 {
                format!("{title} · on")
            } else {
                title.to_owned()
            };
            let resp = ui
                .add_enabled(n > 0, egui::Button::selectable(s.side == side, text))
                .on_hover_text(match side {
                    Side::Right => "Settings of the right lead",
                    Side::Left => "Settings of the left lead",
                });
            if resp.clicked() {
                s.side = side;
            }
        }
    });
}

fn source_panel(s: &mut Session, ui: &mut egui::Ui, pal: &crate::app::tokens::Palette) {
    let side = s.side;
    let n = s.editor.contacts[side.index()];
    if n == 0 {
        return;
    }
    ui.horizontal(|ui| {
        for i in 0..SOURCE_COUNT {
            let src = s.editor.source(side, i);
            let on = src.is_some_and(Source::is_active);
            let text = if on {
                format!("Source {} · on", i + 1)
            } else {
                format!("Source {}", i + 1)
            };
            if ui
                .add(egui::Button::selectable(s.source[side.index()] == i, text))
                .on_hover_text("Up to four sources per lead are combined in the field")
                .clicked()
            {
                s.source[side.index()] = i;
            }
        }
    });
    let i = s.source[side.index()].min(SOURCE_COUNT - 1);
    let index = i as u32 + 1;
    let mut active = s.editor.stim.sides.get(side).active_source == index;
    if ui
        .checkbox(&mut active, "Active source")
        .on_hover_text("The source the Lead-DBS programmer marks as the one in use; every source with an amplitude is computed")
        .changed()
        && active
    {
        s.editor.stim.sides.get_mut(side).active_source = index;
    }
    let mut monopolar = None;
    let mut clear = false;
    let mut polarity_change: Vec<(u32, Polarity)> = Vec::new();
    let mut case_change = None;
    let mut percent_change: Vec<(u32, f64)> = Vec::new();
    if let Some(src) = s.editor.source_mut(side, i) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Mode:");
            ui.selectable_value(&mut src.mode, Mode::Voltage, "Voltage (V)")
                .on_hover_text("Constant voltage on the used contacts");
            ui.selectable_value(&mut src.mode, Mode::Current, "Current (mA)")
                .on_hover_text("Constant current shared by the contacts of each polarity");
        });
        let (unit_text, max, soft) = match src.mode {
            Mode::Voltage => (" V", 10.0, 10.0),
            Mode::Current => (" mA", 10.0, 20.0),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(match src.mode {
                Mode::Voltage => "Amplitude (V):",
                Mode::Current => "Amplitude (mA):",
            });
            ui.add(
                egui::DragValue::new(&mut src.amplitude)
                    .speed(0.05)
                    .range(0.0..=soft * 5.0)
                    .max_decimals(3)
                    .suffix(unit_text),
            )
            .on_hover_text("Amplitude of this source; zero switches the source off");
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = em(ui) * 12.0;
                ui.add(
                    egui::Slider::new(&mut src.amplitude, 0.0..=max)
                        .clamping(egui::SliderClamping::Never)
                        .show_value(false),
                )
                .on_hover_text("Drag to change the amplitude");
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Pulse width (µs):");
            ui.add(
                egui::DragValue::new(&mut src.pulse_width_us)
                    .speed(1.0)
                    .range(1.0..=1000.0)
                    .suffix(" µs"),
            )
            .on_hover_text(
                "Pulse width of this source; the Dembek model scales the threshold with it",
            );
            ui.label("Frequency (Hz):");
            let mut hz = src.frequency_hz.unwrap_or(130.0);
            if ui
                .add(
                    egui::DragValue::new(&mut hz)
                        .speed(1.0)
                        .range(1.0..=1000.0)
                        .suffix(" Hz"),
                )
                .on_hover_text("Informational; no model reads the frequency")
                .changed()
            {
                src.frequency_hz = Some(hz);
            }
        });
        ui.add_space(em(ui) * 0.2);
        egui::Grid::new(("stimulation_contacts", side.index(), i))
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Contact").strong());
                ui.label(egui::RichText::new("Polarity").strong());
                ui.label(egui::RichText::new("Percent").strong());
                ui.label(egui::RichText::new("Impedance (kΩ)").strong());
                ui.end_row();
                for c in &mut src.contacts {
                    ui.label(format!("Contact {}", c.index))
                        .on_hover_text("Contact in the order of the electrode model");
                    ui.horizontal(|ui| {
                        for (pol, text, hover) in POLARITY_BUTTONS {
                            if ui
                                .add(egui::Button::selectable(c.polarity == pol, text))
                                .on_hover_text(hover)
                                .clicked()
                                && c.polarity != pol
                            {
                                polarity_change.push((c.index, pol));
                            }
                        }
                    });
                    let mut percent = c.percent;
                    if ui
                        .add(
                            egui::DragValue::new(&mut percent)
                                .speed(0.5)
                                .range(0.0..=100.0)
                                .max_decimals(2)
                                .suffix(" %"),
                        )
                        .on_hover_text("Share of the current of this polarity")
                        .changed()
                    {
                        percent_change.push((c.index, percent));
                    }
                    ui.add(
                        egui::DragValue::new(&mut c.impedance_kohm)
                            .speed(0.05)
                            .range(0.01..=100.0)
                            .max_decimals(2),
                    )
                    .on_hover_text("Contact impedance; only the Mädler and Dembek models read it");
                    ui.end_row();
                }
                ui.label("Case")
                    .on_hover_text("The implanted pulse generator");
                ui.horizontal(|ui| {
                    for (pol, text, hover) in POLARITY_BUTTONS {
                        if ui
                            .add(egui::Button::selectable(src.case.polarity == pol, text))
                            .on_hover_text(hover)
                            .clicked()
                            && src.case.polarity != pol
                        {
                            case_change = Some(pol);
                        }
                    }
                });
                ui.label(format!("{:.2} %", src.case.percent));
                ui.label("");
                ui.end_row();
            });
        ui.add_space(em(ui) * 0.2);
        ui.horizontal_wrapped(|ui| {
            for c in &src.contacts {
                if ui
                    .small_button(format!("Monopolar on contact {}", c.index))
                    .on_hover_text(format!(
                        "Contact {} alone is the cathode (−) and the case the anode (+)",
                        c.index
                    ))
                    .clicked()
                {
                    monopolar = Some(c.index);
                }
            }
            if ui
                .small_button("Clear source")
                .on_hover_text("Switch every contact and the amplitude of this source off")
                .clicked()
            {
                clear = true;
            }
        });
    }
    let _ = pal;
    for (contact, pol) in polarity_change {
        s.editor.set_polarity(side, i, contact, pol);
    }
    for (contact, percent) in percent_change {
        s.editor.set_percent(side, i, contact, percent);
    }
    if let Some(pol) = case_change {
        s.editor.set_case(side, i, pol);
    }
    if let Some(contact) = monopolar {
        s.editor.set_monopolar(side, i, contact);
    }
    if clear {
        s.editor.clear_source(side, i);
    }
}

const POLARITY_BUTTONS: [(Polarity, &str, &str); 3] = [
    (Polarity::Off, "Off", "Contact not used"),
    (Polarity::Cathode, "−", "Cathode (negative)"),
    (Polarity::Anode, "+", "Anode (positive)"),
];

fn model_settings_section(s: &mut Session, ui: &mut egui::Ui) {
    let kind = s.editor.stim.model;
    egui::CollapsingHeader::new(egui::RichText::new("Model settings").strong())
        .id_salt("stimulation_model_settings")
        .default_open(false)
        .show(ui, |ui| {
            let m = &mut s.editor.stim.model_settings;
            ui.horizontal_wrapped(|ui| {
                ui.label("Activation threshold (V/mm):");
                ui.add(
                    egui::DragValue::new(&mut m.threshold_v_per_mm)
                        .speed(0.005)
                        .range(0.001..=10.0)
                        .max_decimals(4),
                );
                dbsloc_viz::widgets::info_icon(
                    ui,
                    "Field strength above which tissue counts as activated. Default 0.2 V/mm (general heuristic).",
                );
                egui::ComboBox::from_id_salt("stimulation_threshold_preset")
                    .selected_text("Preset…")
                    .show_ui(ui, |ui| {
                        for (name, value) in THRESHOLD_PRESETS {
                            if ui.selectable_label(false, format!("{name} ({value} V/mm)")).clicked() {
                                m.threshold_v_per_mm = value;
                            }
                        }
                    })
                    .response
                    .on_hover_text("Copy a published threshold into the field");
            });
            if kind == ModelKind::Efield {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Conductivity:");
                    ui.selectable_value(
                        &mut m.conductivity.mode,
                        ConductivityMode::GmWm,
                        "Grey and white matter",
                    )
                    .on_hover_text("Grey matter from the atlas mask, white matter elsewhere");
                    ui.selectable_value(
                        &mut m.conductivity.mode,
                        ConductivityMode::Homogeneous,
                        "Homogeneous",
                    )
                    .on_hover_text("One conductivity everywhere");
                    dbsloc_viz::widgets::info_icon(
                        ui,
                        "Tissue conductivity model of the field computation (specs/07 §2.5).",
                    );
                });
                match m.conductivity.mode {
                    ConductivityMode::GmWm => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Grey matter (S/m):");
                            ui.add(
                                egui::DragValue::new(&mut m.conductivity.gm_s_per_m)
                                    .speed(0.005)
                                    .range(0.001..=5.0)
                                    .max_decimals(4),
                            );
                            dbsloc_viz::widgets::info_icon(ui, "Default 0.33 S/m.");
                            ui.label("White matter (S/m):");
                            ui.add(
                                egui::DragValue::new(&mut m.conductivity.wm_s_per_m)
                                    .speed(0.005)
                                    .range(0.001..=5.0)
                                    .max_decimals(4),
                            );
                            dbsloc_viz::widgets::info_icon(ui, "Default 0.14 S/m.");
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Grey matter mask:");
                            let mut text = m.conductivity.gm_mask.clone().unwrap_or_default();
                            if ui
                                .add(egui::TextEdit::singleline(&mut text).desired_width(em(ui) * 22.0))
                                .on_hover_text("`atlas:<set name>`; empty uses homogeneous white matter")
                                .changed()
                            {
                                m.conductivity.gm_mask = (!text.trim().is_empty()).then_some(text);
                            }
                            dbsloc_viz::widgets::info_icon(
                                ui,
                                "Atlas set whose grey-matter mask is warped to the subject. Default: DISTAL Minimal.",
                            );
                        });
                    }
                    ConductivityMode::Homogeneous => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Conductivity (S/m):");
                            let mut v = m.conductivity.homogeneous_s_per_m.unwrap_or(0.1);
                            if ui
                                .add(
                                    egui::DragValue::new(&mut v)
                                        .speed(0.005)
                                        .range(0.001..=5.0)
                                        .max_decimals(4),
                                )
                                .changed()
                            {
                                m.conductivity.homogeneous_s_per_m = Some(v);
                            }
                            dbsloc_viz::widgets::info_icon(ui, "Default 0.1 S/m.");
                        });
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label("Encapsulation (mm):");
                    ui.add(
                        egui::DragValue::new(&mut m.encapsulation.thickness_mm)
                            .speed(0.01)
                            .range(0.0..=1.0)
                            .max_decimals(3),
                    );
                    dbsloc_viz::widgets::info_icon(
                        ui,
                        "Thickness of a tissue layer around the lead; 0 switches it off.",
                    );
                    if m.encapsulation.thickness_mm > 0.0 {
                        ui.label("Conductivity (S/m):");
                        let mut v = m.encapsulation.s_per_m.unwrap_or(0.1);
                        if ui
                            .add(egui::DragValue::new(&mut v).speed(0.005).range(0.001..=5.0))
                            .changed()
                        {
                            m.encapsulation.s_per_m = Some(v);
                        }
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut m.remove_electrode, "Remove the electrode from the VTA");
                dbsloc_viz::widgets::info_icon(
                    ui,
                    "Collapse the volume of the lead out of the VTA, as Lead-DBS does. Default on.",
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Several sources:");
                ui.selectable_value(&mut m.source_combination, SourceCombination::Max, "Maximum")
                    .on_hover_text("Per-voxel maximum of the source fields (Lead-DBS behaviour)");
                ui.selectable_value(&mut m.source_combination, SourceCombination::Sum, "Sum")
                    .on_hover_text("Per-voxel sum of the source fields");
                dbsloc_viz::widgets::info_icon(ui, "How the fields of several active sources are combined.");
            });
            if kind == ModelKind::Dembek {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Reference pulse width (µs):");
                    ui.add(
                        egui::DragValue::new(&mut m.dembek.reference_pulse_width_us)
                            .speed(1.0)
                            .range(1.0..=1000.0),
                    );
                    dbsloc_viz::widgets::info_icon(
                        ui,
                        "Pulse width at which the threshold applies; the sphere scales with the source pulse width. Default 60 µs.",
                    );
                });
            }
            if ui
                .button("Restore defaults")
                .on_hover_text("Reset every model setting to the Lead-DBS defaults")
                .clicked()
            {
                s.editor.stim.model_settings = cfg::ModelSettings::default();
            }
        });
}

const THRESHOLD_PRESETS: [(&str, f64); 4] = [
    ("General heuristic", 0.2),
    ("GPi", 0.2),
    ("STN", 0.19),
    ("VIM", 0.165),
];

fn validation_area(s: &Session, ui: &mut egui::Ui, pal: &crate::app::tokens::Palette) {
    let report = s.editor.report();
    for e in &report.errors {
        ui.add(
            egui::Label::new(egui::RichText::new(format!("✖  {e}")).color(pal.err))
                .wrap()
                .selectable(false),
        );
    }
    for w in &report.warnings {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("⚠  {w}"))
                    .small()
                    .color(pal.text_muted),
            )
            .wrap()
            .selectable(false),
        );
    }
    if matches!(
        s.editor.stim.model,
        ModelKind::Fastfield | ModelKind::Ossdbs
    ) {
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "This model is not computed; choose the E-field model to compute or preview.",
                )
                .small()
                .color(pal.text_muted),
            )
            .wrap()
            .selectable(false),
        );
    }
}

fn preview_area(
    s: &mut Session,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    pal: &crate::app::tokens::Palette,
    env: StimEnv,
    now: f64,
) {
    ui.horizontal_wrapped(|ui| {
        let mut on = s.preview_on;
        if ui
            .add_enabled(!env.run_active, egui::Checkbox::new(&mut on, "Preview"))
            .on_hover_text(
                "Recompute the volume of tissue activated after each edit and show it in the 3D viewer; the first preview of a lead builds its basis, which can take tens of seconds on the processor",
            )
            .changed()
        {
            s.preview_on = on;
            if on {
                s.sched.on_edit(now);
            } else {
                s.cancel_preview();
                s.preview_surfaces.clear();
                s.scene_dirty = true;
                s.last = None;
                s.last_error = None;
            }
        }
        if env.run_active {
            ui.add(
                egui::Label::new(
                    egui::RichText::new("Paused while a run is in progress")
                        .small()
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
        }
        let _ = ctx;
    });
    if !s.preview_on {
        return;
    }
    if let Some(run) = &s.run {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Spinner::new().size(em(ui)));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "Computing the preview… {:.0} s (the first one for a lead builds its basis)",
                        run.started.elapsed().as_secs_f64()
                    ))
                    .small()
                    .color(pal.text_muted),
                )
                .selectable(false),
            );
            if ui.small_button("Stop").on_hover_text("Cancel this preview").clicked() {
                run.cancel.store(true, Ordering::SeqCst);
            }
        });
    } else if s.sched.pending() {
        ui.add(
            egui::Label::new(
                egui::RichText::new("Waiting for the edit to settle…")
                    .small()
                    .color(pal.text_muted),
            )
            .selectable(false),
        );
    } else if let Some(err) = &s.last_error {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("✖  Preview failed: {err}")).color(pal.err),
            )
            .wrap()
            .selectable(false),
        );
    } else if let Some(done) = &s.last {
        if done.sides.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new("No active source: nothing to show.")
                        .small()
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
        }
        for (side, raw, fin, seconds, cache, backend) in &done.sides {
            let title = match side {
                Side::Right => "Right",
                Side::Left => "Left",
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{title}: {fin:.1} mm³ activated ({raw:.1} mm³ before smoothing); basis {cache}, {backend}, {seconds:.2} s"
                    ))
                    .small(),
                )
                .wrap()
                .selectable(false),
            );
        }
        if !done.sides.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(if done.in_scene {
                        format!("Updated in {:.2} s; shown in the 3D viewer (Show VTA).", done.elapsed)
                    } else {
                        format!(
                            "Updated in {:.2} s; the subject has no normalization, so the 3D viewer cannot show it.",
                            done.elapsed
                        )
                    })
                    .small()
                    .color(pal.text_muted),
                )
                .wrap()
                .selectable(false),
            );
        }
        for w in &done.warnings {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("⚠  {w}"))
                        .small()
                        .color(pal.text_muted),
                )
                .wrap()
                .selectable(false),
            );
        }
    }
}

fn action_row(s: &mut Session, ui: &mut egui::Ui, env: StimEnv, out: &mut StimOutput) {
    let report = s.editor.report();
    let valid = report.errors.is_empty();
    let dirty = s.dirty();
    ui.add_space(em(ui) * 0.3);
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(valid && dirty, egui::Button::new("Save"))
            .on_hover_text(if valid {
                "Write the settings of this stimulation to the output directory"
            } else {
                "Fix the errors above first"
            })
            .clicked()
        {
            save_now(s, out);
        }
        let compute_hover = if env.foreign_subject {
            foreign_subject_text(&s.subject, env)
        } else if env.batch_active {
            "A run of several subjects is in progress. Compute the VTA when it has finished."
                .to_owned()
        } else if env.run_active {
            "A run is in progress. Compute the VTA when it has finished.".to_owned()
        } else if let Some(wait) = env.run_wait {
            wait.to_owned()
        } else if !valid {
            "Fix the errors above first.".to_owned()
        } else {
            "Save the settings and run the volume of activated tissue step for this stimulation."
                .to_owned()
        };
        let can_compute = valid && compute_allowed(env);
        if label_controls(&s.editor.stim.label).compute
            && ui
                .add_enabled(can_compute, egui::Button::new("Compute VTA"))
                .on_hover_text(compute_hover.as_str())
                .on_disabled_hover_text(compute_hover.as_str())
                .clicked()
        {
            request_compute(s, env, out);
        }
        if dirty {
            ui.add(
                egui::Label::new(
                    egui::RichText::new("Unsaved changes")
                        .small()
                        .color(palette(ui).text_muted),
                )
                .selectable(false),
            );
        }
    });
}

fn foreign_subject_text(subject: &str, env: StimEnv) -> String {
    let selection = if env.subject_selected {
        "another subject or dataset is selected"
    } else {
        "no subject is selected"
    };
    format!(
        "This editor belongs to sub-{subject}, but {selection}. Select sub-{subject} again to \
         compute its VTA, or close the editor and open it for the subject you want."
    )
}

fn compute_allowed(env: StimEnv) -> bool {
    !env.foreign_subject && !env.run_active && !env.batch_active && env.run_wait.is_none()
}

fn request_compute(s: &mut Session, env: StimEnv, out: &mut StimOutput) -> bool {
    if !compute_allowed(env) || !s.editor.report().errors.is_empty() {
        return false;
    }
    if s.dirty() && !save_now(s, out) {
        return false;
    }
    s.awaiting = Some(s.editor.stim.label.clone());
    out.compute = Some(s.editor.stim.label.clone());
    s.status = Some("Computing the VTA; the result loads into the 3D viewer.".to_owned());
    true
}

fn save_now(s: &mut Session, out: &mut StimOutput) -> bool {
    match cfg::save(&s.subj_dir, &s.editor.stim) {
        Ok(path) => {
            out.log
                .push(format!("Stimulation saved: {}", path.display()));
            s.saved = Some(s.editor.stim.clone());
            s.labels = cfg::list_labels(&s.subj_dir, &s.subject);
            s.status = Some("Saved.".to_owned());
            out.saved = true;
            true
        }
        Err(e) => {
            out.errors
                .push(("Could not save the stimulation".to_owned(), e.to_string()));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_label_hides_the_label_actions_and_compute() {
        let c = label_controls("gs_0123");
        assert!(!c.picker && !c.actions && !c.compute && c.group_tag);
        let c = label_controls("clinical");
        assert!(c.picker && c.actions && c.compute && !c.group_tag);
    }

    #[test]
    fn a_group_label_passes_the_editor_check() {
        let mut e = editor(4);
        e.stim.label = "gs_0123".to_owned();
        assert!(e.report().errors.is_empty());
        assert!(cfg::check(&e.stim, e.contacts)
            .errors
            .iter()
            .any(|x| x.to_string().contains("gs_")));
    }

    fn editor(n: usize) -> Editor {
        Editor::new(
            Stimulation::blank("t", "01", ModelKind::Efield, [n, n]),
            [n, n],
        )
    }

    #[test]
    fn shares_add_up_to_one_hundred() {
        for n in 1..=9 {
            let shares = equal_shares(n);
            assert_eq!(shares.len(), n);
            let sum: f64 = shares.iter().sum();
            assert!((sum - 100.0).abs() < 1e-9, "{n}: {sum}");
        }
        assert!(equal_shares(0).is_empty());
    }

    #[test]
    fn the_timestamp_label_is_the_utc_date_and_time() {
        assert_eq!(timestamp_label(0), "19700101000000");
        assert_eq!(timestamp_label(1_700_000_000), "20231114221320");
        assert!(cfg::label_is_valid(&timestamp_label(1_800_000_000)));
    }

    #[test]
    fn a_taken_label_gets_a_numbered_suffix() {
        let taken = vec!["a".to_owned(), "a_2".to_owned()];
        assert_eq!(unique_label("b", &taken), "b");
        assert_eq!(unique_label("a", &taken), "a_3");
        let long = "x".repeat(64);
        let out = unique_label(&long, std::slice::from_ref(&long));
        assert_eq!(out.chars().count(), 64);
        assert!(out.ends_with("_2"));
    }

    #[test]
    fn the_editor_state_round_trips_through_the_settings_file() {
        let mut e = editor(8);
        e.set_monopolar(Side::Right, 0, 3);
        if let Some(src) = e.source_mut(Side::Right, 0) {
            src.amplitude = 2.5;
            src.mode = Mode::Current;
            src.frequency_hz = Some(130.0);
        }
        e.stim.model_settings.threshold_v_per_mm = 0.19;
        let text = cfg::canonical_json(&e.stim).expect("serialize");
        let back: Stimulation = serde_json::from_str(&text).expect("parse");
        let again = Editor::new(back, [8, 8]);
        assert_eq!(again.stim, e.stim);
        assert!(again.report().errors.is_empty());
    }

    #[test]
    fn the_monopolar_button_makes_one_cathode_and_the_case_the_anode() {
        let mut e = editor(8);
        e.set_polarity(Side::Left, 1, 5, Polarity::Anode);
        e.set_monopolar(Side::Left, 1, 2);
        let src = e.source(Side::Left, 1).expect("source");
        for c in &src.contacts {
            if c.index == 2 {
                assert_eq!(c.polarity, Polarity::Cathode);
                assert_eq!(c.percent, 100.0);
            } else {
                assert_eq!(c.polarity, Polarity::Off);
                assert_eq!(c.percent, 0.0);
            }
        }
        assert_eq!(src.case.polarity, Polarity::Anode);
        assert_eq!(src.case.percent, 100.0);
        assert!(e
            .source(Side::Right, 1)
            .is_some_and(|s| s.contacts.iter().all(|c| c.polarity == Polarity::Off)));
    }

    #[test]
    fn a_bipolar_choice_takes_the_case_out_and_shares_each_polarity() {
        let mut e = editor(4);
        e.set_polarity(Side::Right, 0, 1, Polarity::Cathode);
        e.set_polarity(Side::Right, 0, 2, Polarity::Cathode);
        e.set_polarity(Side::Right, 0, 4, Polarity::Anode);
        let src = e.source(Side::Right, 0).expect("source");
        assert_eq!(src.case.polarity, Polarity::Off);
        assert_eq!(src.contacts[0].percent, 50.0);
        assert_eq!(src.contacts[1].percent, 50.0);
        assert_eq!(src.contacts[3].percent, 100.0);
        if let Some(s) = e.source_mut(Side::Right, 0) {
            s.amplitude = 1.0;
        }
        assert!(e.report().errors.is_empty(), "{:?}", e.report().errors);
    }

    #[test]
    fn typing_a_percentage_sets_and_clears_the_polarity() {
        let mut e = editor(4);
        e.set_percent(Side::Right, 0, 2, 40.0);
        let c = &e.source(Side::Right, 0).expect("source").contacts[1];
        assert_eq!((c.polarity, c.percent), (Polarity::Cathode, 40.0));
        e.set_percent(Side::Right, 0, 2, 0.0);
        let c = &e.source(Side::Right, 0).expect("source").contacts[1];
        assert_eq!(c.polarity, Polarity::Off);
    }

    #[test]
    fn a_current_source_that_does_not_add_up_is_an_error_and_blocks_saving() {
        let mut e = editor(4);
        e.set_monopolar(Side::Right, 0, 1);
        if let Some(src) = e.source_mut(Side::Right, 0) {
            src.amplitude = 1.5;
        }
        assert!(e.report().errors.is_empty());
        e.set_percent(Side::Right, 0, 1, 60.0);
        let report = e.report();
        assert!(!report.errors.is_empty());
        assert!(report.errors[0].to_string().contains("E_VAT_settings"));
        let valid = report.errors.is_empty();
        let dirty = true;
        assert!(!(valid && dirty));
    }

    #[test]
    fn an_inactive_source_never_blocks_saving() {
        let mut e = editor(4);
        e.set_monopolar(Side::Right, 0, 1);
        assert!(e.report().errors.is_empty());
        assert!(!e.has_active_source());
        if let Some(src) = e.source_mut(Side::Right, 0) {
            src.amplitude = 1.0;
        }
        assert!(e.has_active_source());
    }

    #[test]
    fn clearing_a_source_keeps_its_mode_and_index() {
        let mut e = editor(4);
        e.set_monopolar(Side::Right, 2, 1);
        if let Some(src) = e.source_mut(Side::Right, 2) {
            src.amplitude = 2.0;
            src.mode = Mode::Voltage;
        }
        e.clear_source(Side::Right, 2);
        let src = e.source(Side::Right, 2).expect("source");
        assert_eq!(src.index, 3);
        assert_eq!(src.mode, Mode::Voltage);
        assert_eq!(src.amplitude, 0.0);
        assert_eq!(src.contacts.len(), 4);
    }

    #[test]
    fn the_preview_waits_for_the_edit_to_settle() {
        let mut s = Scheduler::default();
        assert_eq!(s.step(10.0, 0.3), PreviewStep::Wait);
        assert!(!s.on_edit(1.0));
        assert_eq!(s.step(1.1, 0.3), PreviewStep::Wait);
        assert!(s.pending());
        assert!(!s.on_edit(1.2));
        assert_eq!(s.step(1.4, 0.3), PreviewStep::Wait);
        assert_eq!(
            s.step(1.5, 0.3),
            PreviewStep::Start {
                cancel_previous: false
            }
        );
        assert!(!s.pending());
        assert_eq!(s.step(9.0, 0.3), PreviewStep::Wait);
    }

    #[test]
    fn an_edit_during_a_preview_cancels_it_and_restarts_after_the_pause() {
        let mut s = Scheduler::default();
        s.on_edit(0.0);
        assert!(matches!(s.step(0.5, 0.3), PreviewStep::Start { .. }));
        assert!(s.is_running());
        assert!(s.on_edit(0.6));
        assert_eq!(s.step(0.7, 0.3), PreviewStep::Wait);
        assert_eq!(
            s.step(0.95, 0.3),
            PreviewStep::Start {
                cancel_previous: true
            }
        );
        s.finished();
        assert!(!s.is_running());
        s.on_edit(2.0);
        assert_eq!(
            s.step(2.4, 0.3),
            PreviewStep::Start {
                cancel_previous: false
            }
        );
    }

    #[test]
    fn computed_files_are_found_per_side_by_name() {
        let dir = std::env::temp_dir().join(format!("dbsloc_stim_files_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        for name in [
            "sub-01_sim-binary_model-simbio_hemi-R.nii",
            "sub-01_sim-binary_model-simbio_hemi-L.nii",
            "sub-01_sim-efield_model-simbio_hemi-R.nii",
            "sub-01_sim-binary_model-simbio_hemi-R.mat",
        ] {
            std::fs::write(dir.join(name), b"").expect("file");
        }
        let found = computed_files(&dir);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, Side::Right);
        assert!(found[0]
            .1
            .ends_with("sub-01_sim-binary_model-simbio_hemi-R.nii"));
        assert_eq!(found[1].0, Side::Left);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn model_texts_show_the_display_name_and_the_id() {
        for kind in ModelKind::ALL {
            let text = model_text(kind);
            assert!(text.contains(kind.display_name()), "{text}");
            assert!(text.contains(model_id(kind)), "{text}");
        }
    }

    fn open_session(dir: &Path) -> StimulationTab {
        let mut tab = StimulationTab::default();
        let editor = editor(8);
        tab.phase = Phase::Open(Box::new(Session {
            opts: RunOptions::new(dir, "01"),
            subject: "01".to_owned(),
            subj_dir: dir.to_path_buf(),
            space: None,
            labels: Vec::new(),
            editor,
            saved: None,
            pristine: None,
            side: Side::Right,
            source: [0, 0],
            new_label: None,
            confirm: None,
            preview_on: false,
            sched: Scheduler::default(),
            run: None,
            generation: 0,
            last: None,
            last_error: None,
            status: None,
            awaiting: None,
            loading: None,
            computed: Vec::new(),
            preview_surfaces: Vec::new(),
            scene_dirty: false,
            conn: Connectivity::default(),
        }));
        tab.target = Some(Target::new(dir, Some("01")));
        tab
    }

    fn mark_clean(tab: &mut StimulationTab) {
        let Phase::Open(s) = &mut tab.phase else {
            panic!("open");
        };
        s.saved = Some(s.editor.stim.clone());
        assert!(!tab.is_dirty());
    }

    fn options_for(t: &Target) -> Result<RunOptions, String> {
        Ok(RunOptions::new(
            t.bids_dir.clone(),
            t.subject.clone().unwrap_or_default(),
        ))
    }

    fn no_load(_: &Target) -> Result<RunOptions, String> {
        panic!("the editor must not open another subject here")
    }

    fn settle(tab: &mut StimulationTab, ctx: &egui::Context) {
        for _ in 0..1000 {
            if !tab.is_preparing() {
                return;
            }
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = tab.show(ui, ctx, StimEnv::default());
            })
            .textures_delta
            .clear();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the editor did not finish opening");
    }

    fn labels_on_screen(tab: &mut StimulationTab) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let (output, _) = frame(tab, &ctx, StimEnv::default(), Vec::new());
        output
            .platform_output
            .accesskit_update
            .expect("accessibility tree")
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().or(n.value()).map(str::to_owned))
            .collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dbsloc_stim_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn a_clean_editor_follows_the_selected_subject_and_says_when_it_cannot_show_it() {
        let dir = temp_dir("follow_clean");
        std::fs::create_dir_all(dir.join("rawdata").join("sub-02")).expect("subject");
        let mut tab = open_session(&dir);
        mark_clean(&mut tab);
        let ctx = egui::Context::default();
        let sub02 = Target::new(&dir, Some("sub-02"));
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, options_for),
            Follow::Load
        );
        assert_eq!(tab.target(), Some(&sub02));
        assert_eq!(tab.bound_subject(), None);
        settle(&mut tab, &ctx);
        assert!(!tab.is_open());
        assert_eq!(tab.target(), Some(&sub02));
        let (text, reason) = empty_state_text(tab.target(), tab.notice.as_deref());
        assert!(
            text.contains("sub-02") && !text.contains("sub-01"),
            "{text}"
        );
        assert!(reason.is_some_and(|r| !r.is_empty()));
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Idle
        );
        let sub03 = Target::new(&dir, Some("sub-03"));
        assert_eq!(
            tab.follow_selection(&sub03, false, true, &ctx, options_for),
            Follow::Load
        );
        assert_eq!(tab.target(), Some(&sub03));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsaved_settings_keep_the_editor_on_its_subject_and_offer_the_choice() {
        let dir = temp_dir("follow_dirty");
        let mut tab = open_session(&dir);
        valid_session(&mut tab);
        assert!(tab.is_dirty());
        let ctx = egui::Context::default();
        let sub02 = Target::new(&dir, Some("sub-02"));
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Ask
        );
        assert_eq!(tab.bound_subject(), Some((dir.as_path(), "01")));
        assert!(tab.scene_surfaces().1.is_empty());
        let labels = labels_on_screen(&mut tab);
        for wanted in [
            "Save and switch to sub-02",
            "Discard and switch to sub-02",
            "Stay on sub-01",
        ] {
            assert!(labels.iter().any(|l| l == wanted), "{wanted}: {labels:?}");
        }

        let mut out = StimOutput::default();
        tab.apply_choice(Choice::Stay, &mut out);
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Stayed
        );
        assert_eq!(tab.bound_subject(), Some((dir.as_path(), "01")));

        tab.apply_choice(Choice::SaveAndSwitch, &mut out);
        let label = match &tab.phase {
            Phase::Open(s) => s.editor.stim.label.clone(),
            _ => panic!("open"),
        };
        assert!(cfg::stimulation_json_path(&dir, "01", &label).is_file());
        assert!(!cfg::stimulation_json_path(&dir, "02", &label).exists());
        assert!(!tab.is_dirty());
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, options_for),
            Follow::Load
        );
        assert_eq!(tab.target(), Some(&sub02));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discarding_switches_without_writing_and_going_back_keeps_the_edits() {
        let dir = temp_dir("follow_discard");
        let mut tab = open_session(&dir);
        valid_session(&mut tab);
        let label = match &tab.phase {
            Phase::Open(s) => s.editor.stim.label.clone(),
            _ => panic!("open"),
        };
        let ctx = egui::Context::default();
        let sub01 = Target::new(&dir, Some("sub-01"));
        let sub02 = Target::new(&dir, Some("sub-02"));
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Ask
        );
        let mut out = StimOutput::default();
        tab.apply_choice(Choice::DiscardAndSwitch, &mut out);
        assert!(!tab.is_dirty());
        assert_eq!(
            tab.follow_selection(&sub01, false, true, &ctx, no_load),
            Follow::Idle
        );
        assert!(tab.is_dirty());
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Ask
        );
        tab.apply_choice(Choice::DiscardAndSwitch, &mut out);
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, options_for),
            Follow::Load
        );
        assert_eq!(tab.target(), Some(&sub02));
        assert!(!cfg::stimulation_json_path(&dir, "01", &label).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_job_of_the_editors_subject_or_a_run_holds_the_editor() {
        let dir = temp_dir("follow_busy");
        let mut tab = open_session(&dir);
        valid_session(&mut tab);
        let ctx = egui::Context::default();
        let sub02 = Target::new(&dir, Some("sub-02"));
        let Phase::Open(s) = &mut tab.phase else {
            panic!("open");
        };
        let mut out = StimOutput::default();
        assert!(request_compute(s, StimEnv::default(), &mut out));
        assert!(!tab.is_dirty());
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Wait
        );
        assert_eq!(tab.bound_subject(), Some((dir.as_path(), "01")));
        tab.run_ended();
        assert_eq!(
            tab.follow_selection(&sub02, true, true, &ctx, no_load),
            Follow::Wait
        );
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, options_for),
            Follow::Load
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_no_subject_selected_a_clean_editor_says_so_and_a_dirty_one_asks() {
        let dir = temp_dir("follow_none");
        let ctx = egui::Context::default();
        let none = Target::new(&dir, None);
        let mut tab = open_session(&dir);
        valid_session(&mut tab);
        assert_eq!(
            tab.follow_selection(&none, false, true, &ctx, no_load),
            Follow::Ask
        );
        mark_clean(&mut tab);
        assert_eq!(
            tab.follow_selection(&none, false, true, &ctx, no_load),
            Follow::Clear
        );
        assert!(!tab.is_open());
        let (text, reason) = empty_state_text(tab.target(), tab.notice.as_deref());
        assert_eq!(text, STIMULATION_NO_SUBJECT);
        assert!(reason.is_none());
        tab.forget_target();
        assert!(tab.target().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn nodes_on_screen(tab: &mut StimulationTab) -> Vec<(String, bool)> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let (output, _) = frame(tab, &ctx, StimEnv::default(), Vec::new());
        output
            .platform_output
            .accesskit_update
            .expect("accessibility tree")
            .nodes
            .iter()
            .filter_map(|(_, n)| {
                n.label()
                    .or(n.value())
                    .map(|l| (l.to_owned(), n.is_disabled()))
            })
            .collect()
    }

    fn opened_without_settings(dir: &Path, ctx: &egui::Context) -> StimulationTab {
        let mut tab = StimulationTab::default();
        let opts = RunOptions::new(dir, "02");
        tab.target = Some(Target::new(dir, Some("02")));
        let (tx, rx) = mpsc::channel();
        tab.phase = Phase::Preparing(rx);
        let prepared = Prepared {
            focus: None,
            subject: "02".to_owned(),
            subj_dir: dir.to_path_buf(),
            space: None,
            contacts: [8, 8],
            labels: Vec::new(),
            first: None,
        };
        tx.send(Ok((Box::new(prepared), opts))).expect("send");
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = tab.show(ui, ctx, StimEnv::default());
        })
        .textures_delta
        .clear();
        assert!(tab.is_open());
        tab
    }

    #[test]
    fn an_untouched_new_label_does_not_hold_the_editor_and_is_never_written() {
        let dir = temp_dir("follow_pristine");
        let ctx = egui::Context::default();
        let mut tab = opened_without_settings(&dir, &ctx);
        assert!(!tab.is_dirty());
        let sub02 = Target::new(&dir, Some("sub-02"));
        let sub03 = Target::new(&dir, Some("sub-03"));
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Idle
        );
        assert_eq!(
            tab.follow_selection(&sub03, false, true, &ctx, options_for),
            Follow::Load
        );
        assert_eq!(tab.target(), Some(&sub03));
        assert!(!cfg::stimulations_dir(&dir).exists());

        let mut tab = opened_without_settings(&dir, &ctx);
        let Phase::Open(s) = &mut tab.phase else {
            panic!("open");
        };
        s.editor.set_monopolar(Side::Right, 0, 1);
        assert!(tab.is_dirty());
        assert_eq!(
            tab.follow_selection(&sub03, false, true, &ctx, no_load),
            Follow::Ask
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discarded_settings_are_read_only_until_the_editor_switches() {
        let dir = temp_dir("follow_read_only");
        let mut tab = open_session(&dir);
        valid_session(&mut tab);
        let ctx = egui::Context::default();
        let sub02 = Target::new(&dir, Some("sub-02"));
        assert!(nodes_on_screen(&mut tab)
            .iter()
            .any(|(l, off)| l == "New…" && !off));
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Ask
        );
        tab.apply_choice(Choice::DiscardAndSwitch, &mut StimOutput::default());
        assert_eq!(
            tab.follow_selection(&sub02, true, true, &ctx, no_load),
            Follow::Wait
        );
        let nodes = nodes_on_screen(&mut tab);
        assert!(
            nodes.iter().any(|(l, off)| l == "New…" && *off),
            "{nodes:?}"
        );
        let note = follow::discarded_text(&follow::STIMULATION_WORDING, "01");
        assert!(nodes.iter().any(|(l, _)| *l == note), "{nodes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_state_retries_after_a_change_and_names_where_it_goes() {
        let dir = temp_dir("follow_retry");
        let ctx = egui::Context::default();
        let sub02 = Target::new(&dir, Some("sub-02"));
        let sub03 = Target::new(&dir, Some("sub-03"));
        let mut tab = StimulationTab::default();
        tab.show_empty(sub02.clone(), Some("no reconstruction".to_owned()));
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, no_load),
            Follow::Idle
        );
        tab.mark_stale();
        assert_eq!(
            tab.follow_selection(&sub02, true, true, &ctx, no_load),
            Follow::Idle
        );
        assert_eq!(
            tab.follow_selection(&sub02, false, true, &ctx, options_for),
            Follow::Load
        );
        assert!(tab.is_preparing());
        settle(&mut tab, &ctx);

        assert_eq!(
            tab.follow_selection(&sub03, false, false, &ctx, no_load),
            Follow::Soon
        );
        let labels = labels_on_screen(&mut tab);
        assert!(
            labels.iter().any(|l| l == "Switching to sub-03…"),
            "{labels:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tab_draws_headless_with_an_error_and_without_one() {
        let dir = std::env::temp_dir().join(format!("dbsloc_stim_draw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let mut tab = open_session(&dir);
        let ctx = egui::Context::default();
        for round in 0..3 {
            if round == 1 {
                if let Phase::Open(s) = &mut tab.phase {
                    s.editor.set_monopolar(Side::Right, 0, 2);
                    s.editor.set_percent(Side::Right, 0, 2, 70.0);
                    if let Some(src) = s.editor.source_mut(Side::Right, 0) {
                        src.amplitude = 2.0;
                    }
                    s.preview_on = true;
                    s.last_error = Some("E_VAT_no_native: no native lead".to_owned());
                }
            }
            if round == 2 {
                if let Phase::Open(s) = &mut tab.phase {
                    s.editor.set_monopolar(Side::Right, 0, 2);
                    s.side = Side::Left;
                    s.preview_on = false;
                }
            }
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let out = tab.show(ui, &ctx, StimEnv::default());
                assert!(out.compute.is_none());
            })
            .textures_delta
            .clear();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_is_blocked_and_compute_writes_nothing_while_an_error_stands() {
        let dir = std::env::temp_dir().join(format!("dbsloc_stim_save_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let mut tab = open_session(&dir);
        let Phase::Open(s) = &mut tab.phase else {
            panic!("open");
        };
        s.editor.set_monopolar(Side::Right, 0, 1);
        if let Some(src) = s.editor.source_mut(Side::Right, 0) {
            src.amplitude = 1.0;
        }
        s.editor.set_percent(Side::Right, 0, 1, 10.0);
        assert!(!s.editor.report().errors.is_empty());
        assert!(!cfg::stimulation_json_path(&dir, "01", "t").exists());
        s.editor.set_monopolar(Side::Right, 0, 1);
        assert!(s.editor.report().errors.is_empty());
        let mut out = StimOutput::default();
        assert!(save_now(s, &mut out));
        assert!(cfg::stimulation_json_path(&dir, "01", "t").is_file());
        assert!(!s.dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_editor_matches_only_its_own_dataset_and_subject() {
        let data = Path::new("data");
        assert!(stim_target_matches(data, "01", data, Some("sub-01")));
        assert!(stim_target_matches(data, "01", data, Some("01")));
        assert!(stim_target_matches(data, "sub-01", data, Some("01")));
        assert!(stim_target_matches(data, "sub-01", data, Some("sub-01")));
        assert!(!stim_target_matches(data, "01", data, Some("sub-02")));
        assert!(!stim_target_matches(data, "01", data, None));
        assert!(!stim_target_matches(
            data,
            "01",
            Path::new("other"),
            Some("sub-01")
        ));
    }

    fn frame(
        tab: &mut StimulationTab,
        ctx: &egui::Context,
        env: StimEnv,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<String>) {
        let mut compute = None;
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            compute = tab.show(ui, ctx, env).compute;
        });
        output.textures_delta.clear();
        (output, compute)
    }

    fn click_compute(tab: &mut StimulationTab, env: StimEnv) -> Option<String> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let (output, _) = frame(tab, &ctx, env, Vec::new());
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree");
        let rect = update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("Compute VTA"))
            .and_then(|(_, n)| n.bounds())
            .expect("Compute VTA button");
        let pos = egui::pos2(
            ((rect.x0 + rect.x1) / 2.0) as f32,
            ((rect.y0 + rect.y1) / 2.0) as f32,
        );
        let press = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let mut compute = None;
        for events in [
            vec![egui::Event::PointerMoved(pos)],
            vec![press(true)],
            vec![press(false)],
            Vec::new(),
        ] {
            let (_, c) = frame(tab, &ctx, env, events);
            compute = compute.or(c);
        }
        compute
    }

    fn valid_session(tab: &mut StimulationTab) {
        let Phase::Open(s) = &mut tab.phase else {
            panic!("open");
        };
        s.editor.set_monopolar(Side::Right, 0, 1);
        if let Some(src) = s.editor.source_mut(Side::Right, 0) {
            src.amplitude = 1.0;
        }
        assert!(s.editor.report().errors.is_empty());
    }

    #[test]
    fn compute_asks_for_nothing_while_another_subject_is_selected() {
        let dir = std::env::temp_dir().join(format!("dbsloc_stim_foreign_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let mut tab = open_session(&dir);
        assert_eq!(tab.bound_subject(), Some((dir.as_path(), "01")));
        valid_session(&mut tab);
        let foreign = StimEnv {
            foreign_subject: true,
            subject_selected: true,
            ..StimEnv::default()
        };
        let waiting = StimEnv {
            run_wait: Some(crate::app::RUN_WAIT_IMPORT),
            ..StimEnv::default()
        };
        let batch = StimEnv {
            run_active: true,
            batch_active: true,
            ..StimEnv::default()
        };
        for env in [foreign, waiting, batch] {
            assert_eq!(click_compute(&mut tab, env), None);
            let Phase::Open(s) = &tab.phase else {
                panic!("open");
            };
            assert!(s.awaiting.is_none());
            assert!(!cfg::stimulation_json_path(&dir, "01", &s.editor.stim.label).exists());
        }

        let own = StimEnv::default();
        let label = click_compute(&mut tab, own).expect("a click on Compute VTA asks to compute");
        let Phase::Open(s) = &tab.phase else {
            panic!("open");
        };
        assert_eq!(label, s.editor.stim.label);
        assert!(s.awaiting.is_some());
        assert!(cfg::stimulation_json_path(&dir, "01", &label).is_file());

        let Phase::Open(s) = &mut tab.phase else {
            panic!("open");
        };
        let mut out = StimOutput::default();
        assert!(!request_compute(s, foreign, &mut out));
        assert!(out.compute.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_run_of_the_editors_subject_loads_its_vta() {
        let dir = std::env::temp_dir().join(format!("dbsloc_stim_owner_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let mut tab = open_session(&dir);
        valid_session(&mut tab);
        let ctx = egui::Context::default();
        let vat = dbsloc_pipeline::stages::Stage::Vat.label();
        let Phase::Open(s) = &mut tab.phase else {
            panic!("open");
        };
        s.space = Some("MNI152NLin2009bAsym".to_owned());
        let mut out = StimOutput::default();
        assert!(request_compute(s, StimEnv::default(), &mut out));

        tab.stage_completed(vat, Some((Path::new("elsewhere"), "01")), &ctx);
        tab.stage_completed(vat, Some((dir.as_path(), "sub-02")), &ctx);
        tab.stage_completed(vat, None, &ctx);
        let Phase::Open(s) = &tab.phase else {
            panic!("open");
        };
        assert!(s.awaiting.is_some());
        assert!(s.loading.is_none());

        tab.stage_completed(vat, Some((dir.as_path(), "01")), &ctx);
        let Phase::Open(s) = &tab.phase else {
            panic!("open");
        };
        assert!(s.awaiting.is_none());
        assert!(s.loading.is_some());

        let mut tab = open_session(&dir);
        valid_session(&mut tab);
        let Phase::Open(s) = &mut tab.phase else {
            panic!("open");
        };
        assert!(request_compute(s, StimEnv::default(), &mut out));
        tab.run_ended();
        let Phase::Open(s) = &tab.phase else {
            panic!("open");
        };
        assert!(s.awaiting.is_none());
        assert!(s.status.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
