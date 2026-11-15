// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::bids::Subject;
use crate::derivatives;
use crate::options::RunOptions;
use crate::stages::Stage;
use crate::version;

pub type Paragraph = (String, Vec<&'static str>);

pub const REF_N4: &str = "Tustison NJ, Avants BB, Cook PA, et al. (2010) N4ITK: Improved N3 bias correction. IEEE Trans Med Imaging 29(6):1310–1320. doi:10.1109/TMI.2010.2046908.";
pub const REF_AVANTS: &str = "Avants BB, Epstein CL, Grossman M, Gee JC (2008) Symmetric diffeomorphic image registration with cross-correlation. Med Image Anal 12(1):26–41. doi:10.1016/j.media.2007.06.004.";
pub const REF_AVANTS_2011: &str = "Avants BB, Tustison NJ, Song G, Cook PA, Klein A, Gee JC (2011) A reproducible evaluation of ANTs similarity metric performance in brain image registration. NeuroImage 54(3):2033–2044. doi:10.1016/j.neuroimage.2010.09.025.";
pub const REF_HORN_KUEHN_2015: &str = "Horn A, Kühn AA (2015) Lead-DBS: A toolbox for deep brain stimulation electrode localizations and visualizations. NeuroImage 107:127–135. doi:10.1016/j.neuroimage.2014.12.002.";
pub const REF_HUSCH: &str = "Husch A, Petersen MV, Gemmar P, Goncalves J, Hertel F (2017) PaCER — A fully automated method for electrode trajectory and contact reconstruction in deep brain stimulation. NeuroImage Clin 17:80–89. doi:10.1016/j.nicl.2017.10.004.";
pub const REF_HELLERBACH: &str = "Hellerbach A, et al. (2018) DiODe: Directional Orientation Detection of Segmented DBS Leads: A Sequential Algorithm Based on CT Imaging. Stereotact Funct Neurosurg 96(5):335–341. doi:10.1159/000494738.";
pub const REF_DEMBEK_2021: &str = "Dembek TA, et al. (2021) DiODe v2: Unambiguous and Fully-Automated Detection of Directional DBS Lead Orientation. Brain Sci 11(11):1450. doi:10.3390/brainsci11111450.";
pub const REF_KURTEV: &str = "Kurtev-Rittstieg R, et al. (2020) Orientation of Directional DBS Leads on CT: Resolving the Ambiguity. bioRxiv. doi:10.1101/2020.09.16.298653.";
pub const REF_HORN_2017: &str = "Horn A, Reich M, Vorwerk J, et al. (2017) Connectivity predicts deep brain stimulation outcome in Parkinson disease. Ann Neurol 82(1):67–78. doi:10.1002/ana.24974.";
pub const REF_KUNCEL: &str = "Kuncel AM, Cooper SE, Grill WM (2008) A method to estimate the spatial extent of activation in thalamic deep brain stimulation. Clin Neurophysiol 119(9):2148–2158. doi:10.1016/j.clinph.2008.02.025.";
pub const REF_MAEDLER: &str = "Mädler B, Coenen VA (2012) Explaining clinical effects of deep brain stimulation through simplified target-specific modeling of the volume of activated tissue. AJNR Am J Neuroradiol 33(6):1072–1080. doi:10.3174/ajnr.A2906.";
pub const REF_DEMBEK_2017: &str = "Dembek TA, Barbe MT, Åström M, et al. (2017) Probabilistic mapping of deep brain stimulation effects in essential tremor. NeuroImage Clin 13:164–173. doi:10.1016/j.nicl.2016.11.019.";
pub const REF_BANIASADI: &str = "Baniasadi M, Proverbio D, Gonçalves J, Hertel F, Husch A (2020) FastField: An open-source toolbox for efficient approximation of deep brain stimulation electric fields. NeuroImage 223:117330. doi:10.1016/j.neuroimage.2020.117330.";
pub const REF_BUTENKO: &str = "Butenko K, Bahls C, Schröder M, Köhling R, van Rienen U (2020) OSS-DBS: Open-source simulation platform for deep brain stimulation with a comprehensive automated modeling. PLoS Comput Biol 16(7):e1008023. doi:10.1371/journal.pcbi.1008023.";

const ENGINE_ANTS: &str = "the Advanced Normalization Tools (ANTs) programs";
const ORIENTATION_METHOD: &str = "diode_marker_v1";
const NORM_METHOD_SYN: &str = "ants_syn";
const BRAINSHIFT_METHOD: &str = "scrf";
pub const MOVED_MM: f64 = 1e-6;

pub struct ManualSide<'a> {
    pub key: &'a str,
    pub head: bool,
    pub tail: bool,
    pub roll: bool,
}

pub fn format_entry(
    timestamp: &str,
    step: &str,
    version: &str,
    paragraph: &str,
    references: &[&str],
) -> String {
    let paragraph = paragraph.replace(['\r', '\n'], " ");
    let mut out = format!("[{timestamp}] {step} | Wyss DBS Localizer {version}\n{paragraph}\n");
    if !references.is_empty() {
        out.push_str("References:\n");
        for reference in references {
            out.push_str("- ");
            out.push_str(reference);
            out.push('\n');
        }
    }
    out.push('\n');
    out
}

const LOCK_STALE: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_POLL: Duration = Duration::from_millis(20);
const LOCK_RELEASE_TRIES: u32 = 8;
const LOCK_DENIED_GRACE: Duration = Duration::from_millis(500);
const LOCK_BREAK_EVERY: Duration = Duration::from_millis(250);

pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

static LOCK_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = LOCK_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{n}", std::process::id())
}

fn remove_with_retries(path: &Path) {
    for _ in 0..LOCK_RELEASE_TRIES {
        match fs::remove_file(path) {
            Ok(()) => return,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(LOCK_POLL / 2),
        }
    }
}

struct LockGuard {
    path: PathBuf,
    token: String,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        for _ in 0..LOCK_RELEASE_TRIES {
            match fs::read_to_string(&self.path) {
                Ok(held) if held == self.token => {}
                Ok(_) => return,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return,
                Err(_) => {
                    std::thread::sleep(LOCK_POLL / 2);
                    continue;
                }
            }
            match fs::remove_file(&self.path) {
                Ok(()) => return,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(LOCK_POLL / 2),
            }
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn older_by_more_than_stale(earlier: SystemTime, later: SystemTime) -> bool {
    later
        .duration_since(earlier)
        .is_ok_and(|gap| gap > LOCK_STALE)
}

fn sibling(lock: &Path, suffix: &str) -> PathBuf {
    let mut name = lock.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    lock.with_file_name(name)
}

fn breaker_path(lock: &Path) -> PathBuf {
    sibling(lock, ".break")
}

fn file_system_now(lock: &Path) -> Option<SystemTime> {
    let probe = sibling(lock, &format!(".probe-{}", unique_suffix()));
    let created = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .ok()?;
    drop(created);
    let now = modified(&probe);
    remove_with_retries(&probe);
    now
}

fn clear_left_over_breaker(lock: &Path) {
    let breaker = breaker_path(lock);
    let Some(made) = modified(&breaker) else {
        return;
    };
    let Ok(held) = fs::read(&breaker) else {
        return;
    };
    let left_over = file_system_now(lock).is_some_and(|now| older_by_more_than_stale(made, now));
    if left_over && fs::read(&breaker).is_ok_and(|again| again == held) {
        remove_with_retries(&breaker);
    }
}

fn break_stale_lock(lock: &Path) -> bool {
    let breaker = breaker_path(lock);
    let guard = match create_lock(&breaker) {
        Ok(guard) => guard,
        Err(_) => {
            clear_left_over_breaker(lock);
            return false;
        }
    };
    let stale = match (modified(lock), modified(&breaker)) {
        (Some(held_since), Some(now)) => older_by_more_than_stale(held_since, now),
        _ => false,
    };
    let broken = stale && fs::remove_file(lock).is_ok();
    drop(guard);
    broken
}

fn create_lock(lock: &Path) -> io::Result<LockGuard> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(lock)?;
    let token = unique_suffix();
    if let Err(e) = file.write_all(token.as_bytes()).and_then(|()| file.flush()) {
        drop(file);
        let _ = fs::remove_file(lock);
        return Err(e);
    }
    Ok(LockGuard {
        path: lock.to_path_buf(),
        token,
    })
}

fn acquire_lock(lock: &Path) -> io::Result<LockGuard> {
    let deadline = Instant::now() + LOCK_WAIT;
    let mut denied_since: Option<Instant> = None;
    let mut last_break: Option<Instant> = None;
    loop {
        let attempt = create_lock(lock);
        let denied = matches!(&attempt, Err(e) if e.kind() == io::ErrorKind::PermissionDenied)
            && !lock.exists();
        if !denied {
            denied_since = None;
        }
        match attempt {
            Ok(guard) => return Ok(guard),
            Err(_)
                if denied
                    && denied_since.get_or_insert_with(Instant::now).elapsed()
                        < LOCK_DENIED_GRACE =>
            {
                std::thread::sleep(LOCK_POLL / 4);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists || lock.exists() => {
                let due = last_break.is_none_or(|t| t.elapsed() >= LOCK_BREAK_EVERY);
                if due {
                    last_break = Some(Instant::now());
                    if break_stale_lock(lock) {
                        continue;
                    }
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("the methods file is locked by {}", lock.display()),
                    ));
                }
                std::thread::sleep(LOCK_POLL);
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn append_entry(path: &Path, entry: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _guard = acquire_lock(&lock_path(path))?;
    let mut bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(entry.as_bytes());
    let mut tries = 0u32;
    loop {
        match crate::outputs::write_replacing(path, &bytes) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && tries < LOCK_RELEASE_TRIES => {
                tries += 1;
                std::thread::sleep(LOCK_POLL);
            }
            other => return other,
        }
    }
}

fn text(v: &Value) -> Option<&str> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn positive(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite() && *x > 0.0)
}

fn fmt_decimal(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn weighted(modality: &str) -> String {
    match modality {
        "T1w" => "T1-weighted".to_owned(),
        "T2w" => "T2-weighted".to_owned(),
        other => other.to_owned(),
    }
}

fn template_ref(space: &str) -> String {
    if space.starts_with("custom-") {
        "a custom template".to_owned()
    } else {
        format!("the {space} template")
    }
}

fn template_space_ref(space: &str) -> String {
    if space.starts_with("custom-") {
        "a custom template space".to_owned()
    } else {
        format!("the {space} template space")
    }
}

fn device_phrase(rec: &Value) -> String {
    let adapter = &rec["reg_adapter"];
    match text(&adapter["device_type"]) {
        Some("DiscreteGpu" | "IntegratedGpu") => {
            let unit = match text(&adapter["name"]) {
                Some(name) => format!("the graphics processing unit {name}"),
                None => "the graphics processing unit".to_owned(),
            };
            match text(&adapter["backend"]) {
                Some(api) => format!(" on {unit} through {api}"),
                None => format!(" on {unit}"),
            }
        }
        Some("Cpu") => " with a software renderer on the processor".to_owned(),
        _ => String::new(),
    }
}

fn ants_phrase(rec: &Value) -> String {
    match text(&rec["ants_version"]) {
        Some(v) => format!("{ENGINE_ANTS}, version {v}"),
        None => ENGINE_ANTS.to_owned(),
    }
}

fn engine_phrase(token: Option<&str>, noun: &str, rec: &Value) -> Option<String> {
    match token {
        Some("wgpu") => Some(format!(
            "the in-process {noun} (WebGPU compute through the wgpu library){}",
            device_phrase(rec)
        )),
        Some("ants") => Some(ants_phrase(rec)),
        _ => None,
    }
}

fn anchor_phrase(anchor: Option<&str>) -> String {
    match anchor.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => format!("the pre-operative {} anchor image", weighted(a)),
        None => "the pre-operative anchor image".to_owned(),
    }
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn side_name(key: &str) -> Option<&'static str> {
    match key {
        "1" => Some("right"),
        "2" => Some("left"),
        _ => None,
    }
}

fn lead_phrase(sides: &[&str]) -> String {
    match sides {
        [one] => format!("the {one} lead"),
        _ => "both leads".to_owned(),
    }
}

fn push_unique(refs: &mut Vec<&'static str>, reference: &'static str) {
    if !refs.contains(&reference) {
        refs.push(reference);
    }
}

pub fn preprocess(rec: &Value, anchor: Option<&str>) -> Option<Paragraph> {
    let target = capitalise(&anchor_phrase(anchor));
    let mut details: Vec<String> = Vec::new();
    if text(&rec["n4"]["parameters"]) == Some("program defaults") {
        details.push("the default parameters of the program".to_owned());
    }
    if let Some(shrink) = positive(&rec["n4"]["shrink_factor"]) {
        details.push(format!("a shrink factor of {}", fmt_decimal(shrink)));
    }
    let levels = &rec["n4"]["fitting_levels"];
    let level_count = levels
        .as_u64()
        .or_else(|| levels.as_array().map(|a| a.len() as u64))
        .filter(|n| *n > 0);
    if let Some(n) = level_count {
        details.push(format!("{n} fitting levels"));
    }
    let mut paragraph = format!(
        "{target} was corrected for intensity inhomogeneity (bias field) with N4 bias field correction"
    );
    if !details.is_empty() {
        paragraph.push_str(&format!(", using {}", join_and(&details)));
    }
    paragraph.push('.');
    if let Some(engine) = engine_phrase(text(&rec["reg_backend"]), "engine", rec) {
        paragraph.push_str(&format!(" The computation was carried out with {engine}."));
    }
    Some((paragraph, vec![REF_N4]))
}

pub fn precoreg(rec: &Value, anchor: Option<&str>) -> Option<Paragraph> {
    let token = text(&rec["reg_backend"]).or_else(|| text(&rec["backend"]));
    let engine = engine_phrase(token, "registration engine", rec);
    let space = text(&rec["template_space"]);
    if engine.is_none() && space.is_none() {
        return None;
    }
    let target = capitalise(&anchor_phrase(anchor));
    let template = match space {
        Some(s) => template_ref(s),
        None => "a template".to_owned(),
    };
    let mut paragraph = format!(
        "{target} was rigidly (rotation and translation only) registered to {template} as a pre-coregistration step."
    );
    let mut refs: Vec<&'static str> = Vec::new();
    if let Some(engine) = engine {
        paragraph.push_str(&format!(" The registration was carried out with {engine}."));
        if token == Some("ants") {
            refs.push(REF_AVANTS_2011);
        }
    }
    Some((paragraph, refs))
}

fn coreg_method(token: &str) -> Option<&'static str> {
    match token {
        "ants_rigid_multistart" => {
            Some("a rigid-body registration started from several initial positions")
        }
        "ants_rigid" => Some("a rigid-body registration"),
        _ => None,
    }
}

pub fn coregister(rec: &Value, anchor: Option<&str>) -> Option<Paragraph> {
    let map = rec.as_object()?;
    let fixed = anchor_phrase(anchor);
    let mut sentences: Vec<String> = Vec::new();
    let mut refs: Vec<&'static str> = Vec::new();
    let mut ct_named = false;
    let mut mri_named = false;
    for (key, image) in map {
        let Some(method) = image.get("method").and_then(Value::as_str) else {
            continue;
        };
        let subject = match key.as_str() {
            "CT" => {
                let s = if ct_named {
                    "The post-operative CT image"
                } else {
                    "The post-operative computed tomography (CT) image"
                };
                ct_named = true;
                s.to_owned()
            }
            "ax_MRI" | "cor_MRI" | "sag_MRI" => {
                let plane = match key.as_str() {
                    "ax_MRI" => "axial",
                    "cor_MRI" => "coronal",
                    _ => "sagittal",
                };
                let s = if mri_named {
                    format!("The {plane} post-operative MRI image")
                } else {
                    format!("The {plane} post-operative magnetic resonance imaging (MRI) image")
                };
                mri_named = true;
                s
            }
            _ => "A post-operative image".to_owned(),
        };
        let by = coreg_method(method)
            .map(|m| format!(" by {m}"))
            .unwrap_or_default();
        let token = text(&image["backend"]).or_else(|| text(&rec["engine"]));
        let mut probe = rec.clone();
        probe["ants_version"] = image["ants_version"].clone();
        let engine = engine_phrase(token, "registration engine", &probe)
            .map(|e| format!(" using {e}"))
            .unwrap_or_default();
        if token == Some("ants") {
            push_unique(&mut refs, REF_AVANTS_2011);
        }
        sentences.push(format!("{subject} was registered to {fixed}{by}{engine}."));
    }
    if sentences.is_empty() {
        return None;
    }
    Some((sentences.join(" "), refs))
}

pub fn normalize(rec: &Value, anchor: Option<&str>) -> Option<Paragraph> {
    let method = text(&rec["method"]) == Some(NORM_METHOD_SYN);
    let space = text(&rec["template_space"]);
    let channels: Vec<String> = rec["channels"]
        .as_array()
        .map(|a| a.iter().filter_map(text).map(weighted).collect())
        .unwrap_or_default();
    if !method && space.is_none() && channels.is_empty() {
        return None;
    }
    let subject = match channels.as_slice() {
        [] => format!("{} was normalized", capitalise(&anchor_phrase(anchor))),
        [one] => format!("The pre-operative {one} anchor image was normalized"),
        many => format!(
            "The pre-operative {} images were normalized jointly (multispectral registration)",
            join_and(many)
        ),
    };
    let by = if method {
        " by a rigid, affine and symmetric diffeomorphic (SyN) registration"
    } else {
        ""
    };
    let target = match space {
        Some(s) => format!(" to {}", template_space_ref(s)),
        None => " to a template space".to_owned(),
    };
    let mut paragraph = format!("{subject}{target}{by}.");
    if let Some(mm) = positive(&rec["norm_isotropic_mm"]) {
        paragraph.push_str(&format!(
            " The registration was computed on an isotropic grid of {} mm.",
            fmt_decimal(mm)
        ));
    }
    let token = text(&rec["reg_backend"]).or_else(|| text(&rec["engine"]));
    let mut refs: Vec<&'static str> = Vec::new();
    if method {
        refs.push(REF_AVANTS);
    }
    if let Some(engine) = engine_phrase(token, "registration engine", rec) {
        paragraph.push_str(&format!(" The registration was carried out with {engine}."));
        if token == Some("ants") {
            push_unique(&mut refs, REF_AVANTS_2011);
        }
    }
    Some((paragraph, refs))
}

pub fn brainshift(rec: &Value) -> Option<Paragraph> {
    if text(&rec["method"]) != Some(BRAINSHIFT_METHOD) {
        return None;
    }
    let transforms = if text(&rec["warp"]).is_some() && text(&rec["affine"]).is_some() {
        "A previously computed warp field and affine transform were applied"
    } else {
        "A previously computed transform was applied"
    };
    let mut paragraph =
        format!("{transforms} to resample an image onto a reference image in the brain shift step");
    let mut refs: Vec<&'static str> = Vec::new();
    if text(&rec["engine"]) == Some("ants") {
        paragraph.push_str(&format!(", using {}", ants_phrase(rec)));
        refs.push(REF_AVANTS_2011);
    }
    paragraph.push('.');
    Some((paragraph, refs))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReconKind {
    TracNative,
    TracTemplate,
    Pacer,
    PacerImport,
    OtherImport,
}

fn is_pacer_method(method: Option<&str>) -> bool {
    match method {
        None => true,
        Some(m) => m == "pacer_import" || m.starts_with("pacer"),
    }
}

fn recon_kind(reco: &Value) -> Option<ReconKind> {
    if text(&reco["meta"]["pacer"]) == Some("import_json") {
        return Some(if is_pacer_method(text(&reco["method"])) {
            ReconKind::PacerImport
        } else {
            ReconKind::OtherImport
        });
    }
    match text(&reco["method"]) {
        Some("trac_core_rust_native") => return Some(ReconKind::TracNative),
        Some("trac_core_rust") => return Some(ReconKind::TracTemplate),
        Some("pacer_rust") => return Some(ReconKind::Pacer),
        Some(m) if m.starts_with("pacer") => return Some(ReconKind::PacerImport),
        _ => {}
    }
    match text(&reco["meta"]["settings"]["recon_method"]) {
        Some("trac_core_native") => Some(ReconKind::TracNative),
        Some("trac_core") => Some(ReconKind::TracTemplate),
        Some("pacer") => Some(ReconKind::Pacer),
        Some("pacer_import") => Some(ReconKind::PacerImport),
        _ => None,
    }
}

fn imported_models(reco: &Value, traced_keys: &[&str]) -> Option<String> {
    let keys: Vec<&str> = if reco["meta"]["reconstruct"]["status"].is_object() {
        traced_keys.to_vec()
    } else {
        vec!["1", "2"]
    };
    let stated: Vec<(&str, &str)> = keys
        .iter()
        .filter_map(|k| Some((side_name(k)?, text(&reco["props"][*k]["elmodel"])?)))
        .collect();
    let mut models: Vec<&str> = Vec::new();
    for (_, m) in &stated {
        if !models.contains(m) {
            models.push(m);
        }
    }
    match models.as_slice() {
        [] => None,
        [one] if stated.len() == keys.len() => Some(format!(
            " The imported file states the electrode model {one}."
        )),
        _ => {
            let parts: Vec<String> = stated
                .iter()
                .map(|(side, m)| format!("{m} for the {side} lead"))
                .collect();
            Some(format!(
                " The imported file states the electrode model {}.",
                join_and(&parts)
            ))
        }
    }
}

pub fn reconstruct(reco: &Value, el_model: Option<&str>) -> Option<Paragraph> {
    let kind = recon_kind(reco)?;
    let status = reco["meta"]["reconstruct"]["status"].as_object();
    let mut traced: Vec<&'static str> = Vec::new();
    let mut traced_keys: Vec<&'static str> = Vec::new();
    let mut kept: Vec<&'static str> = Vec::new();
    if let Some(st) = status {
        for key in ["1", "2"] {
            let Some(name) = side_name(key) else {
                continue;
            };
            match st.get(key).and_then(text) {
                Some("traced") => {
                    traced.push(name);
                    traced_keys.push(key);
                }
                Some("kept") => kept.push(name),
                _ => {}
            }
        }
        if traced.is_empty() {
            return None;
        }
    }
    let what = if status.is_some() {
        format!(
            "The trajectory and contact positions of {}",
            lead_phrase(&traced)
        )
    } else {
        "The electrode trajectories and contact positions".to_owned()
    };
    let mut refs: Vec<&'static str> = Vec::new();
    let mut paragraph = match kind {
        ReconKind::TracNative => {
            push_unique(&mut refs, REF_HORN_KUEHN_2015);
            format!("{what} were reconstructed with the TRAC/CORE algorithm in the native frame of the post-operative image")
        }
        ReconKind::TracTemplate => {
            push_unique(&mut refs, REF_HORN_KUEHN_2015);
            format!("{what} were reconstructed with the TRAC/CORE algorithm in template space")
        }
        ReconKind::Pacer => {
            push_unique(&mut refs, REF_HUSCH);
            format!("{what} were reconstructed with PaCER, a fully automated method for electrode trajectory and contact reconstruction")
        }
        ReconKind::PacerImport => {
            push_unique(&mut refs, REF_HUSCH);
            format!("{what} were imported from a PaCER result that had been computed beforehand; they were not reconstructed by this software")
        }
        ReconKind::OtherImport => {
            format!("{what} were imported from a reconstruction that had been computed beforehand; they were not reconstructed by this software")
        }
    };
    paragraph.push('.');
    if matches!(kind, ReconKind::TracNative | ReconKind::TracTemplate) {
        let candidates: Vec<&str> = if status.is_some() {
            traced.clone()
        } else {
            vec!["right", "left"]
        };
        let metal: Vec<&str> = candidates
            .into_iter()
            .filter(|n| text(&reco["meta"]["side_source"][*n]) == Some("metal_axis"))
            .collect();
        if !metal.is_empty() {
            paragraph.push_str(&format!(
                " For {}, TRAC/CORE did not succeed and the axis of the metal artefact was used instead.",
                lead_phrase(&metal)
            ));
        }
    }
    if !kept.is_empty() {
        paragraph.push_str(&format!(
            " The manually corrected position of {} was kept.",
            lead_phrase(&kept)
        ));
    }
    if matches!(kind, ReconKind::PacerImport | ReconKind::OtherImport) {
        if let Some(sentence) = imported_models(reco, &traced_keys) {
            paragraph.push_str(&sentence);
        }
    } else {
        let model = text(&reco["meta"]["settings"]["el_model"])
            .or_else(|| el_model.map(str::trim).filter(|m| !m.is_empty()));
        if let Some(model) = model {
            paragraph.push_str(&format!(" The electrode model was assumed to be {model}."));
        }
    }
    if let Some(space) = text(&reco["meta"]["settings"]["template_space"]) {
        paragraph.push_str(&format!(
            " Template-space coordinates refer to {}.",
            template_ref(space)
        ));
    }
    Some((paragraph, refs))
}

pub fn orientation(reco: &Value) -> Option<Paragraph> {
    let report = reco["meta"]["orientation"].as_object()?;
    let mut resolved: Vec<&str> = Vec::new();
    let mut ambiguous: Vec<&str> = Vec::new();
    for key in ["1", "2"] {
        let Some(name) = side_name(key) else {
            continue;
        };
        let Some(entry) = report.get(key) else {
            continue;
        };
        if !reco["meta"]["manual_correction"][key]["roll_review"].is_null() {
            continue;
        }
        if text(&entry["method"]) != Some(ORIENTATION_METHOD) {
            continue;
        }
        match text(&entry["status"]) {
            Some("resolved") => resolved.push(name),
            Some("ambiguous") => ambiguous.push(name),
            _ => {}
        }
    }
    let mut estimated: Vec<&str> = resolved.clone();
    estimated.extend(ambiguous.iter().copied());
    if estimated.is_empty() {
        return None;
    }
    estimated.sort_by_key(|n| *n != "right");
    let mut paragraph = format!(
        "The rotation (roll) angle of the directional contact segments was estimated for {} from the artefact of the orientation marker in the native post-operative computed tomography (CT) image, with an algorithm based on the DiODe (directional orientation detection) approach.",
        lead_phrase(&estimated)
    );
    if !ambiguous.is_empty() {
        paragraph.push_str(&format!(
            " For {} two candidate orientations remained.",
            lead_phrase(&ambiguous)
        ));
    }
    Some((paragraph, vec![REF_HELLERBACH, REF_DEMBEK_2021, REF_KURTEV]))
}

fn vat_model_reference(slug: &str) -> Option<&'static str> {
    match slug {
        "simbio" => Some(REF_HORN_2017),
        "kuncel" => Some(REF_KUNCEL),
        "maedler" => Some(REF_MAEDLER),
        "dembek" => Some(REF_DEMBEK_2017),
        "fastfield" => Some(REF_BANIASADI),
        "ossdbs" => Some(REF_BUTENKO),
        _ => None,
    }
}

const CPU_SOLVER: &str = "with the central processing unit";
const CACHED_SOLVER: &str = "with a stored basis from an earlier computation";

fn side_solver(entry: &Value, label_info: &Value) -> Option<String> {
    let backend = text(&entry["backend"])?;
    if text(&entry["basis_cache"]) == Some("hit") {
        return Some(CACHED_SOLVER.to_owned());
    }
    let solver = match backend {
        "cpu" => CPU_SOLVER.to_owned(),
        "wgpu" => {
            let info = entry.get("adapter_info").unwrap_or(label_info);
            let probe = json!({"reg_adapter": info.clone()});
            format!("with {}", engine_phrase(Some("wgpu"), "engine", &probe)?)
        }
        _ => return None,
    };
    let partly = text(&entry["basis_cache"]) == Some("built")
        && entry["cached_contacts"].as_u64().is_some_and(|n| n > 0);
    if partly {
        Some(format!("partly from a stored basis, {solver}"))
    } else {
        Some(solver)
    }
}

fn solver_clause(sc: &Value) -> Option<String> {
    let engine = &sc["engine"];
    let label_info = &engine["adapter_info"];
    let entries: Vec<(&str, &Value)> = engine["backends"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| Some((text(&e["side"])?, e)))
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        let backend = text(&engine["backend"]);
        if backend.is_some() && text(&engine["basis_cache"]) == Some("hit") {
            return Some(CACHED_SOLVER.to_owned());
        }
        let probe = json!({"reg_adapter": label_info.clone()});
        return match backend {
            Some("wgpu") => {
                engine_phrase(Some("wgpu"), "engine", &probe).map(|e| format!("with {e}"))
            }
            Some("cpu") => Some(CPU_SOLVER.to_owned()),
            _ => None,
        };
    }
    let per_side: Vec<(&str, String)> = entries
        .iter()
        .filter_map(|(side, e)| Some((*side, side_solver(e, label_info)?)))
        .collect();
    let mut phrases: Vec<&String> = Vec::new();
    for (_, p) in &per_side {
        if !phrases.contains(&p) {
            phrases.push(p);
        }
    }
    match phrases.as_slice() {
        [] => None,
        [one] if per_side.len() == entries.len() => Some((*one).clone()),
        many => {
            let segments: Vec<String> = many
                .iter()
                .map(|p| {
                    let leads: Vec<String> = per_side
                        .iter()
                        .filter(|(_, q)| q == *p)
                        .map(|(s, _)| format!("the {s} lead"))
                        .collect();
                    format!("{p} for {}", join_and(&leads))
                })
                .collect();
            Some(segments.join(" and "))
        }
    }
}

fn vat_clause(slug: &str, sidecars: &[&Value]) -> Option<String> {
    let clause = match slug {
        "simbio" => {
            let mut s = "a quasi-static volume-conductor electric-field model computed with this software's own solver, following Horn et al. (2017)".to_owned();
            let mut solvers: Vec<String> = Vec::new();
            for sc in sidecars {
                if let Some(c) = solver_clause(sc) {
                    if !solvers.contains(&c) {
                        solvers.push(c);
                    }
                }
            }
            if let [one] = solvers.as_slice() {
                s.push_str(&format!(", solved {one}"));
            }
            let mut thresholds: Vec<String> = Vec::new();
            for sc in sidecars {
                if let Some(t) = positive(&sc["threshold_v_per_m"]) {
                    let t = fmt_decimal(t);
                    if !thresholds.contains(&t) {
                        thresholds.push(t);
                    }
                }
            }
            if !thresholds.is_empty() {
                s.push_str(&format!(
                    ", with an electric-field magnitude threshold of {} V/m",
                    thresholds.join(" / ")
                ));
            }
            s
        }
        "kuncel" => "the analytical model of Kuncel et al. (2008)".to_owned(),
        "maedler" => "the analytical model of Mädler and Coenen (2012)".to_owned(),
        "dembek" => "the analytical model of Dembek et al. (2017)".to_owned(),
        "fastfield" => {
            "the FastField electric-field approximation of Baniasadi et al. (2020)".to_owned()
        }
        "ossdbs" => "the OSS-DBS simulation platform of Butenko et al. (2020)".to_owned(),
        _ => return None,
    };
    Some(clause)
}

pub fn vat(sidecars: &[Value]) -> Option<Paragraph> {
    let mut slugs: Vec<&str> = Vec::new();
    let mut count = 0usize;
    for sc in sidecars {
        let Some(slug) = text(&sc["model"]).filter(|s| vat_model_reference(s).is_some()) else {
            continue;
        };
        count += 1;
        if !slugs.contains(&slug) {
            slugs.push(slug);
        }
    }
    if slugs.is_empty() {
        return None;
    }
    let mut clauses: Vec<String> = Vec::new();
    let mut refs: Vec<&'static str> = Vec::new();
    for slug in &slugs {
        let group: Vec<&Value> = sidecars
            .iter()
            .filter(|sc| text(&sc["model"]) == Some(slug))
            .collect();
        if let Some(clause) = vat_clause(slug, &group) {
            clauses.push(clause);
        }
        if let Some(reference) = vat_model_reference(slug) {
            push_unique(&mut refs, reference);
        }
    }
    let noun = if count == 1 {
        "stimulation setting"
    } else {
        "stimulation settings"
    };
    let paragraph = format!(
        "The volume of tissue activated (VTA) was estimated for {count} {noun} using {}.",
        join_and(&clauses)
    );
    Some((paragraph, refs))
}

pub fn manual_correction(space: &str, sides: &[ManualSide<'_>]) -> Option<Paragraph> {
    let mut sentences: Vec<String> = Vec::new();
    for key in ["1", "2"] {
        let Some(name) = side_name(key) else {
            continue;
        };
        let Some(side) = sides.iter().find(|s| s.key == key) else {
            continue;
        };
        let mut parts: Vec<&str> = Vec::new();
        match (side.head, side.tail) {
            (true, true) => parts.push("the head and tail markers were moved"),
            (true, false) => parts.push("the head marker was moved"),
            (false, true) => parts.push("the tail marker was moved"),
            (false, false) => {}
        }
        if side.roll {
            parts.push("the rotation (roll) angle was set");
        }
        if parts.is_empty() {
            continue;
        }
        let parts: Vec<String> = parts.iter().map(|p| (*p).to_owned()).collect();
        sentences.push(format!("For the {name} lead, {}.", join_and(&parts)));
    }
    if sentences.is_empty() {
        return None;
    }
    let space_text = match space {
        "native" => " in the native image space",
        "mni" => " in template space",
        _ => "",
    };
    let paragraph = format!(
        "The reconstruction was corrected manually{space_text}. {}",
        sentences.join(" ")
    );
    Some((paragraph, vec![REF_HORN_KUEHN_2015]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodsEntry {
    pub timestamp: String,
    pub step: String,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MethodsDocument {
    pub preamble: String,
    pub entries: Vec<MethodsEntry>,
}

fn parse_header(line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let rest = line.strip_prefix('[')?;
    let (timestamp, after) = rest.split_once("] ")?;
    if timestamp.is_empty() || timestamp.contains(char::is_whitespace) {
        return None;
    }
    let (step, _) = after.split_once(" | ")?;
    if step.is_empty() || step.contains(char::is_whitespace) {
        return None;
    }
    Some((timestamp.to_owned(), step.to_owned()))
}

pub fn parse_entries(text: &str) -> MethodsDocument {
    let mut doc = MethodsDocument::default();
    let mut current: Option<MethodsEntry> = None;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for line in text.split_inclusive('\n') {
        if let Some((timestamp, step)) = parse_header(line) {
            if let Some(done) = current.take() {
                doc.entries.push(done);
            }
            current = Some(MethodsEntry {
                timestamp,
                step,
                raw: line.to_owned(),
            });
        } else if let Some(entry) = current.as_mut() {
            entry.raw.push_str(line);
        } else {
            doc.preamble.push_str(line);
        }
    }
    if let Some(done) = current {
        doc.entries.push(done);
    }
    doc
}

pub fn last_entry_per_step(doc: &MethodsDocument) -> Vec<&MethodsEntry> {
    let mut kept: Vec<&MethodsEntry> = Vec::new();
    for (i, entry) in doc.entries.iter().enumerate() {
        let later = doc.entries[i + 1..].iter().any(|e| e.step == entry.step);
        if !later {
            kept.push(entry);
        }
    }
    kept
}

pub fn render_last(doc: &MethodsDocument) -> String {
    let mut out = String::new();
    for entry in last_entry_per_step(doc) {
        let newline = if entry.raw.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        out.push_str(&entry.raw);
        if !out.ends_with('\n') {
            out.push_str(newline);
        }
        if !(out.ends_with("\n\n") || out.ends_with("\n\r\n")) {
            out.push_str(newline);
        }
    }
    out
}

pub fn methods_file(subject_dir: &Path, subject_id: &str) -> PathBuf {
    let id = subject_id.strip_prefix("sub-").unwrap_or(subject_id);
    subject_dir
        .join("log")
        .join(format!("sub-{id}_desc-methods.txt"))
}

pub fn render_bytes(bytes: &[u8], latest_only: bool) -> Vec<u8> {
    if latest_only {
        let text = String::from_utf8_lossy(bytes);
        render_last(&parse_entries(&text)).into_bytes()
    } else {
        bytes.to_vec()
    }
}

fn read_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn vat_sidecars(sub: &Subject, labels: &[String]) -> Vec<Value> {
    let name = format!("sub-{}_desc-vta.json", sub.subj_id);
    labels
        .iter()
        .map(|label| {
            let path: PathBuf =
                crate::vat::settings::label_dir(&sub.subj_dir, "native", label).join(&name);
            read_json(&path)
        })
        .collect()
}

fn stage_paragraph(
    sub: &Subject,
    opts: &RunOptions,
    stage: Stage,
    vat_labels: &[String],
) -> Option<Paragraph> {
    let anchor = Some(sub.anchor_modality.as_str());
    match stage {
        Stage::Bids | Stage::Visualize => None,
        Stage::Preprocess => preprocess(&read_json(&derivatives::preproc_record(sub)), anchor),
        Stage::Precoreg => precoreg(&read_json(&derivatives::precoreg_record(sub)), anchor),
        Stage::Coregister => coregister(&read_json(&derivatives::coreg_method_log(sub)), anchor),
        Stage::Normalize => normalize(&read_json(&derivatives::norm_method_log(sub)), anchor),
        Stage::Brainshift => brainshift(&read_json(&derivatives::brainshift_method_log(sub))),
        Stage::Reconstruct => reconstruct(
            &read_json(&derivatives::reconstruction_json(sub)),
            opts.el_model.as_deref(),
        ),
        Stage::Orientation => orientation(&read_json(&derivatives::reconstruction_json(sub))),
        Stage::Vat => vat(&vat_sidecars(sub, vat_labels)),
    }
}

pub fn record_stage(
    sub: &Subject,
    opts: &RunOptions,
    stage: Stage,
    vat_labels: &[String],
) -> io::Result<bool> {
    let Some((paragraph, references)) = stage_paragraph(sub, opts, stage, vat_labels) else {
        return Ok(false);
    };
    let entry = format_entry(
        &version::utc_now_rfc3339(),
        stage.cli_name(),
        &version::version_string(),
        &paragraph,
        &references,
    );
    append_entry(&derivatives::methods_log(sub), &entry)?;
    Ok(true)
}

pub fn record_manual_correction(
    sub: &Subject,
    space: &str,
    sides: &[ManualSide<'_>],
) -> io::Result<bool> {
    let Some((paragraph, references)) = manual_correction(space, sides) else {
        return Ok(false);
    };
    let entry = format_entry(
        &version::utc_now_rfc3339(),
        "manual-correction",
        &version::version_string(),
        &paragraph,
        &references,
    );
    append_entry(&derivatives::methods_log(sub), &entry)?;
    Ok(true)
}

#[cfg(test)]
#[path = "methods_text_tests.rs"]
mod tests;
