// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{PipelineError, PipelineResult};

pub const PARTIAL_PREFIX: &str = ".partial-";

struct Watched {
    id: u64,
    roots: Vec<PathBuf>,
    replaced: bool,
}

static WATCHES: Mutex<Vec<Watched>> = Mutex::new(Vec::new());
static NEXT_WATCH: AtomicU64 = AtomicU64::new(1);

pub struct ReplaceWatch {
    id: u64,
}

impl ReplaceWatch {
    pub fn new(roots: &[PathBuf]) -> ReplaceWatch {
        let id = NEXT_WATCH.fetch_add(1, Ordering::SeqCst);
        let roots = roots.iter().map(|r| crate::paths::absolute(r)).collect();
        WATCHES
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Watched {
                id,
                roots,
                replaced: false,
            });
        ReplaceWatch { id }
    }

    pub fn replaced(&self) -> bool {
        WATCHES
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|w| w.id == self.id && w.replaced)
    }
}

impl Drop for ReplaceWatch {
    fn drop(&mut self) {
        WATCHES
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|w| w.id != self.id);
    }
}

fn note_replaced(path: &Path) {
    let mut watches = WATCHES.lock().unwrap_or_else(|p| p.into_inner());
    if watches.is_empty() {
        return;
    }
    let path = crate::paths::absolute(path);
    for w in watches.iter_mut() {
        if w.roots.iter().any(|r| {
            path.strip_prefix(r).is_ok_and(|rest| {
                !rest
                    .components()
                    .any(|c| is_partial_name(&c.as_os_str().to_string_lossy()))
            })
        }) {
            w.replaced = true;
        }
    }
}

pub fn partial_of(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    target.with_file_name(format!("{PARTIAL_PREFIX}{name}"))
}

pub fn is_partial_name(name: &str) -> bool {
    name.starts_with(PARTIAL_PREFIX)
}

fn remove_if_present(p: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub fn unlink_outputs(paths: &[PathBuf]) -> PipelineResult<()> {
    for p in paths {
        remove_if_present(p)
            .map_err(|e| PipelineError::Io(format!("cannot replace {}: {e}", p.display())))?;
    }
    Ok(())
}

pub fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = partial_of(path);
    remove_if_present(&tmp)?;
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    note_replaced(path);
    Ok(())
}

pub const MAX_ANTS_FILE_NAME: usize = 32;

static ALIAS_COUNTER: AtomicU64 = AtomicU64::new(0);
static COPY_NOTED: AtomicBool = AtomicBool::new(false);

const ALIAS_PREFIX: &str = ".partial-i";

pub fn hide_console_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn alias_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(ALIAS_PREFIX)?;
    let (pid, tail) = rest.split_once('_')?;
    let n = tail.split('.').next()?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

fn sweep_stale_aliases(dir: &Path) {
    static SWEPT: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let swept = SWEPT.get_or_init(|| Mutex::new(HashSet::new()));
    if !swept
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(dir.to_path_buf())
    {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let me = std::process::id();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if alias_pid(&name).is_some_and(|pid| pid != me) && e.path().is_file() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub struct AliasGuard(Vec<PathBuf>);

impl Drop for AliasGuard {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn derivatives_roots() -> &'static Mutex<Vec<PathBuf>> {
    static ROOTS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    ROOTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Allow short aliases ([`shorten_long_inputs`]) for files under a chosen output root that is
/// not named `derivatives/leaddbs`.
pub fn register_derivatives_root(root: &Path) {
    let mut roots = derivatives_roots()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if !roots.iter().any(|r| r == root) {
        roots.push(root.to_path_buf());
    }
}

fn is_pipeline_derivative(p: &Path) -> bool {
    let s = p.to_string_lossy().to_ascii_lowercase();
    if s.contains("derivatives") && s.contains("leaddbs") {
        return true;
    }
    derivatives_roots()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .any(|r| p.starts_with(r))
}

fn alias_for(token: &str, guard: &mut Vec<PathBuf>) -> Option<PathBuf> {
    let p = Path::new(token);
    let name = p.file_name()?.to_string_lossy().into_owned();
    if !p.is_absolute()
        || name.len() <= MAX_ANTS_FILE_NAME
        || is_partial_name(&name)
        || !is_pipeline_derivative(p)
        || !p.is_file()
    {
        return None;
    }
    let ext = [".nii.gz", ".nii", ".mat", ".h5", ".txt"]
        .into_iter()
        .find(|e| name.to_ascii_lowercase().ends_with(e))
        .unwrap_or("");
    if let Some(dir) = p.parent() {
        sweep_stale_aliases(dir);
    }
    let n = ALIAS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let alias = p.with_file_name(format!("{ALIAS_PREFIX}{}_{n}{ext}", std::process::id()));
    let _ = std::fs::remove_file(&alias);
    if let Err(e) = std::fs::hard_link(p, &alias) {
        std::fs::copy(p, &alias).ok()?;
        if !COPY_NOTED.swap(true, Ordering::Relaxed) {
            eprintln!(
                "dbsloc: note: cannot hard-link {} ({e}); long tool inputs are copied to short names instead",
                p.display()
            );
        }
    }
    guard.push(alias.clone());
    Some(alias)
}

pub fn shorten_long_inputs(cmd: &Command) -> (Command, AliasGuard) {
    let mut out = Command::new(cmd.get_program());
    hide_console_window(&mut out);
    if let Some(d) = cmd.get_current_dir() {
        out.current_dir(d);
    }
    for (k, v) in cmd.get_envs() {
        match v {
            Some(v) => out.env(k, v),
            None => out.env_remove(k),
        };
    }
    let mut created = Vec::new();
    for arg in cmd.get_args() {
        let text = arg.to_string_lossy().into_owned();
        let mut rewritten = text.clone();
        for token in text.split(['[', ']', ',']) {
            if let Some(alias) = alias_for(token, &mut created) {
                rewritten = rewritten.replace(token, &alias.to_string_lossy());
            }
        }
        out.arg(rewritten);
    }
    (out, AliasGuard(created))
}

fn invalidate_lock(lock: &Path) {
    let _ = std::fs::remove_file(lock);
}

#[derive(Debug, Clone)]
pub struct Staged {
    targets: Vec<(PathBuf, PathBuf)>,
}

impl Staged {
    pub fn new(targets: Vec<PathBuf>) -> PipelineResult<Self> {
        let pairs = targets
            .into_iter()
            .map(|t| {
                let tmp = partial_of(&t);
                (t, tmp)
            })
            .collect();
        Staged::mapped(pairs)
    }

    pub fn mapped(targets: Vec<(PathBuf, PathBuf)>) -> PipelineResult<Self> {
        let s = Staged { targets };
        unlink_outputs(&s.partials())?;
        Ok(s)
    }

    pub fn partials(&self) -> Vec<PathBuf> {
        self.targets.iter().map(|(_, tmp)| tmp.clone()).collect()
    }

    pub fn tmp(&self, target: &Path) -> PathBuf {
        self.targets
            .iter()
            .find(|(t, _)| t == target)
            .map_or_else(|| partial_of(target), |(_, tmp)| tmp.clone())
    }

    pub fn discard(&self) {
        for p in self.partials() {
            let _ = std::fs::remove_file(p);
        }
    }

    pub fn commit(&self, lock: Option<&Path>) -> PipelineResult<()> {
        let mut changed = false;
        for (target, tmp) in &self.targets {
            let step = if tmp.is_file() {
                if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::rename(tmp, target).map(|()| true)
            } else {
                remove_if_present(target)
            };
            match step {
                Ok(c) => {
                    if c {
                        note_replaced(target);
                    }
                    changed |= c;
                }
                Err(e) => {
                    if changed {
                        if let Some(l) = lock {
                            invalidate_lock(l);
                        }
                    }
                    self.discard();
                    return Err(PipelineError::Io(format!(
                        "cannot replace {}: {e}",
                        target.display()
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn finish<T>(&self, result: PipelineResult<T>, lock: Option<&Path>) -> PipelineResult<T> {
        match result {
            Ok(v) => {
                self.commit(lock)?;
                Ok(v)
            }
            Err(e) => {
                self.discard();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_outputs_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn failure_keeps_the_previous_outputs() {
        let d = scratch("fail");
        let a = d.join("a.nii.gz");
        std::fs::write(&a, b"old").unwrap();
        let s = Staged::new(vec![a.clone()]).unwrap();
        std::fs::write(s.tmp(&a), b"half").unwrap();
        let r: PipelineResult<()> = Err(PipelineError::Message("boom".into()));
        assert!(s.finish(r, None).is_err());
        assert_eq!(std::fs::read(&a).unwrap(), b"old");
        assert!(!s.tmp(&a).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn success_replaces_written_and_removes_stale_outputs() {
        let d = scratch("ok");
        let a = d.join("a.nii.gz");
        let stale = d.join("stale.h5");
        let source = d.join("source.nii.gz");
        std::fs::write(&source, b"input").unwrap();
        std::fs::hard_link(&source, &a).unwrap();
        std::fs::write(&stale, b"old").unwrap();
        let s = Staged::new(vec![a.clone(), stale.clone()]).unwrap();
        std::fs::write(s.tmp(&a), b"new").unwrap();
        s.finish(Ok(()), None).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"new");
        assert_eq!(std::fs::read(&source).unwrap(), b"input");
        assert!(!stale.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn mapped_work_files_are_renamed_to_their_targets() {
        let d = scratch("mapped");
        let target = d.join("final").join("sub-1_xfm.mat");
        let work = d.join(".partial-sub-10GenericAffine.mat");
        let s = Staged::mapped(vec![(target.clone(), work.clone())]).unwrap();
        assert_eq!(s.tmp(&target), work);
        std::fs::write(&work, b"affine").unwrap();
        s.finish(Ok(()), None).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"affine");
        assert!(!work.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn long_derivative_inputs_get_short_aliases_that_vanish_afterwards() {
        let d = scratch("alias").join("derivatives").join("leaddbs");
        std::fs::create_dir_all(&d).unwrap();
        let long = d.join("sub-01_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz");
        std::fs::write(&long, b"ct").unwrap();
        let short = d.join("a.nii.gz");
        std::fs::write(&short, b"a").unwrap();
        let mut cmd = Command::new("tool");
        cmd.arg("-i")
            .arg(&long)
            .arg(format!("[{},0]", long.display()))
            .arg(&short);
        let (rewritten, guard) = shorten_long_inputs(&cmd);
        let args: Vec<String> = rewritten
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let alias = PathBuf::from(&args[1]);
        assert!(alias.file_name().unwrap().len() <= MAX_ANTS_FILE_NAME);
        assert_eq!(std::fs::read(&alias).unwrap(), b"ct");
        assert!(args[2].starts_with('[') && args[2].ends_with(",0]") && !args[2].contains("desc-"));
        assert_eq!(args[3], short.to_string_lossy());
        drop(guard);
        assert!(!alias.exists() && long.is_file());
        let _ = std::fs::remove_dir_all(d.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn long_inputs_under_a_registered_output_root_get_aliases() {
        let root = scratch("custom_root").join("my outputs");
        let d = root.join("sub-01").join("coregistration").join("anat");
        std::fs::create_dir_all(&d).unwrap();
        let long = d.join("sub-01_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz");
        std::fs::write(&long, b"ct").unwrap();
        let mut cmd = Command::new("tool");
        cmd.arg(&long);
        let (_, guard) = shorten_long_inputs(&cmd);
        assert!(guard.0.is_empty());
        drop(guard);
        register_derivatives_root(&root);
        let (_, guard) = shorten_long_inputs(&cmd);
        assert_eq!(guard.0.len(), 1);
        drop(guard);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn aliases_left_by_other_processes_are_swept() {
        let d = scratch("sweep").join("derivatives").join("leaddbs");
        std::fs::create_dir_all(&d).unwrap();
        let long = d.join("sub-01_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz");
        std::fs::write(&long, b"ct").unwrap();
        let other = std::process::id().wrapping_add(1);
        let stale = d.join(format!("{ALIAS_PREFIX}{other}_7.nii.gz"));
        let unrelated = d.join(".partial-iso_T1w.nii.gz");
        for p in [&stale, &unrelated] {
            std::fs::write(p, b"x").unwrap();
        }
        let mut cmd = Command::new("tool");
        cmd.arg(&long);
        let (_, guard) = shorten_long_inputs(&cmd);
        assert!(!stale.exists());
        assert!(unrelated.exists());
        assert_eq!(guard.0.len(), 1);
        assert_eq!(alias_pid(&format!("{ALIAS_PREFIX}12_3.h5")), Some(12));
        assert_eq!(alias_pid(".partial-iso_T1w.nii.gz"), None);
        drop(guard);
        let _ = std::fs::remove_dir_all(d.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn writes_to_temporary_files_do_not_count_as_replaced_outputs() {
        let d = scratch("watch_partial");
        let target = d.join("a.mat");
        std::fs::write(&target, b"old").unwrap();
        let watch = ReplaceWatch::new(std::slice::from_ref(&d));
        let s = Staged::new(vec![target.clone()]).unwrap();
        write_replacing(&s.tmp(&target), b"half").unwrap();
        write_replacing(&d.join(".partial-work").join("b.mat"), b"x").unwrap();
        assert!(!watch.replaced());
        s.discard();
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(!watch.replaced());
        write_replacing(&s.tmp(&target), b"new").unwrap();
        s.commit(None).unwrap();
        assert!(watch.replaced());
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        let watch = ReplaceWatch::new(std::slice::from_ref(&d));
        write_replacing(&target, b"newer").unwrap();
        assert!(watch.replaced());
        drop(watch);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_commit_that_removes_a_stale_output_counts_as_replaced() {
        let d = scratch("watch_stale");
        let stale = d.join("stale.json");
        std::fs::write(&stale, b"old").unwrap();
        let watch = ReplaceWatch::new(std::slice::from_ref(&d));
        let s = Staged::new(vec![stale.clone()]).unwrap();
        s.commit(None).unwrap();
        assert!(!stale.exists());
        assert!(watch.replaced());
        drop(watch);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_replacing_breaks_hardlinks() {
        let d = scratch("write");
        let source = d.join("source.json");
        let alias = d.join("alias.json");
        std::fs::write(&source, b"keep").unwrap();
        std::fs::hard_link(&source, &alias).unwrap();
        write_replacing(&alias, b"new").unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"keep");
        assert_eq!(std::fs::read(&alias).unwrap(), b"new");
        assert!(is_partial_name(
            &partial_of(&alias).file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&d);
    }
}
