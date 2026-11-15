// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Locate ANTs/N4 binaries: bundled copy takes priority over system PATH.
//!
//! Port of `dbsloc/bundled_bin.py`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Resolve a bundled ANTs binary by name.  Returns the absolute path or `None`.
///
/// Search order:
/// 1. `$DBSLOC_ANTS_DIR/<name>[.exe]`
/// 2. `<resources>/ants/<platform>/<name>[.exe]` — relative to current executable
/// 3. `<resources>/ants/<platform>/<name>[.exe]` — relative to `DBSLOC_REPO_ROOT`
pub fn find_bundled_binary(name: &str) -> Option<PathBuf> {
    find_bundled_in("ants", name)
}

/// Resolve a bundled binary under `resources/<subdir>/<platform>/`, relative to the current
/// executable and then to `DBSLOC_REPO_ROOT`.  The `ants` subdirectory also honours `DBSLOC_ANTS_DIR`.
pub fn find_bundled_in(subdir: &str, name: &str) -> Option<PathBuf> {
    if subdir == "ants" {
        if let Ok(dir) = std::env::var("DBSLOC_ANTS_DIR") {
            let d = Path::new(dir.trim());
            if d.is_dir() {
                if let Some(p) = try_in_dir(d, name) {
                    return Some(p);
                }
            }
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1).take(6) {
            let dir = ancestor.join("resources").join(subdir).join(platform_dir());
            if let Some(p) = try_in_dir(&dir, name) {
                return Some(p);
            }
        }
    }

    if let Some(root) = crate::paths::repo_root() {
        if let Some(p) = try_in_dir(
            &root.join("resources").join(subdir).join(platform_dir()),
            name,
        ) {
            return Some(p);
        }
    }

    None
}

/// Directory that holds `resources/`: relative to the current executable, then `DBSLOC_REPO_ROOT`
/// and the source tree, the same places the bundled binaries are searched.
pub fn find_resources_root() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1).take(6) {
            if ancestor.join("resources").is_dir() {
                return Some(ancestor.to_path_buf());
            }
        }
    }
    crate::paths::repo_root().filter(|r| r.join("resources").is_dir())
}

/// Find binary: bundled first, then system PATH.  Returns `None` if not found anywhere.
pub fn find_binary(name: &str) -> Option<PathBuf> {
    if let Some(p) = find_bundled_binary(name) {
        return Some(p);
    }
    find_on_path(name)
}

const VERSION_LIMIT: Duration = Duration::from_secs(5);
const VERSION_OUTPUT_CAP: usize = 64 * 1024;

pub fn parse_ants_version(output: &str) -> Option<String> {
    let rest = output.lines().find_map(|line| {
        let line = line.trim();
        let head = line.get(..13)?;
        head.eq_ignore_ascii_case("ANTs Version:")
            .then(|| line[13..].trim())
    })?;
    let token = rest.split_whitespace().next()?;
    let (numbers, suffix) = match token.split_once('-') {
        Some((n, s)) => (n, Some(s)),
        None => (token, None),
    };
    let parts: Vec<&str> = numbers.split('.').collect();
    let numeric = parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()));
    let suffix_ok = suffix.is_none_or(|s| {
        !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric())
    });
    (numeric && suffix_ok).then(|| token.to_owned())
}

enum VersionRun {
    Exited(Option<String>),
    TimedOut,
    NotStarted,
}

const VERSION_TIMEOUTS: u32 = 2;

fn run_for_version(binary: &Path) -> VersionRun {
    let mut cmd = Command::new(binary);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::outputs::hide_console_window(&mut cmd);
    let Ok(mut child) = cmd.spawn() else {
        return VersionRun::NotStarted;
    };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        if let Some(out) = stdout.as_mut() {
            let mut chunk = [0u8; 4096];
            loop {
                match out.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let room = VERSION_OUTPUT_CAP.saturating_sub(kept.len());
                        kept.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
            }
        }
        kept
    });
    let deadline = Instant::now() + VERSION_LIMIT;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status.success()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let output = reader.join().unwrap_or_default();
    match exited {
        None => VersionRun::TimedOut,
        Some(false) => VersionRun::Exited(None),
        Some(true) => VersionRun::Exited(parse_ants_version(&String::from_utf8_lossy(&output))),
    }
}

#[derive(Clone)]
enum CachedVersion {
    Known(Option<String>),
    TimedOut(u32),
}

fn version_cache() -> &'static Mutex<HashMap<PathBuf, CachedVersion>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedVersion>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_version_with(binary: &Path, run: &dyn Fn(&Path) -> VersionRun) -> Option<String> {
    let before = version_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(binary)
        .cloned();
    let timeouts = match before {
        Some(CachedVersion::Known(v)) => return v,
        Some(CachedVersion::TimedOut(n)) if n >= VERSION_TIMEOUTS => return None,
        Some(CachedVersion::TimedOut(n)) => n,
        None => 0,
    };
    let (entry, found) = match run(binary) {
        VersionRun::Exited(v) => (Some(CachedVersion::Known(v.clone())), v),
        VersionRun::TimedOut => (Some(CachedVersion::TimedOut(timeouts + 1)), None),
        VersionRun::NotStarted => (None, None),
    };
    if let Some(entry) = entry {
        version_cache()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(binary.to_path_buf(), entry);
    }
    found
}

fn cached_version(binary: &Path) -> Option<String> {
    cached_version_with(binary, &run_for_version)
}

pub fn ants_version(binary: &Path) -> Option<String> {
    if let Some(v) = cached_version(binary) {
        return Some(v);
    }
    let stem = binary.file_stem()?.to_string_lossy().into_owned();
    if stem == "antsRegistration" {
        return None;
    }
    let sibling = try_in_dir(binary.parent()?, "antsRegistration")?;
    cached_version(&sibling)
}

fn platform_dir() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(target_os = "macos")]
    {
        "mac"
    }
    #[cfg(target_os = "windows")]
    {
        "win"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "linux"
    }
}

fn try_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = dir.join(name);
    if is_executable(&p) {
        return Some(p);
    }
    let pe = dir.join(format!("{name}.exe"));
    if is_executable(&pe) {
        return Some(pe);
    }
    None
}

fn is_executable(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Search `PATH` for an executable called `name`.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let exe_name = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&exe_name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_dir_nonempty() {
        assert!(!platform_dir().is_empty());
    }

    #[test]
    fn the_version_is_parsed_from_the_captured_program_output() {
        let real = "ANTs Version: 2.6.5-gfdce4d2\nCompiled: Jan 14 2026 17:26:52\n\n";
        assert_eq!(parse_ants_version(real).as_deref(), Some("2.6.5-gfdce4d2"));
        assert_eq!(
            parse_ants_version("ANTs Version: 2.5.0\n").as_deref(),
            Some("2.5.0")
        );
        assert_eq!(
            parse_ants_version("noise\r\nants version: 2.4.1-abc\r\n").as_deref(),
            Some("2.4.1-abc")
        );
    }

    #[test]
    fn anything_that_is_not_a_plausible_version_gives_none() {
        for text in [
            "",
            "\n\n",
            "ANTs Version:",
            "ANTs Version: unknown",
            "ANTs Version: 2",
            "ANTs Version: 2.6.5-this-is-way-too-long-a-suffix",
            "ANTs Version: 2.6.5-g!d",
            "ERROR:  Invalid flag provided version\nERROR:  Invalid command line flags found!",
            "Version: 2.6.5",
            "ANTs Version: 2..5",
            "ÄÄÄÄÄÄÄÄÄÄÄÄÄÄÄ",
        ] {
            assert_eq!(parse_ants_version(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_missing_program_has_no_version_and_is_not_cached() {
        let missing = Path::new("definitely_missing_dir").join("antsRegistration");
        assert_eq!(ants_version(&missing), None);
        assert_eq!(ants_version(&missing), None);
        assert!(version_cache()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&missing)
            .is_none());
    }

    #[test]
    fn only_a_program_that_exited_is_cached_and_timeouts_stop_after_two() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let calls = AtomicU32::new(0);
        let slow = Path::new("fake_dir_timeout").join("antsRegistration");
        let timing_out = |_: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            VersionRun::TimedOut
        };
        for _ in 0..4 {
            assert_eq!(cached_version_with(&slow, &timing_out), None);
        }
        assert_eq!(calls.load(Ordering::SeqCst), VERSION_TIMEOUTS);

        let calls = AtomicU32::new(0);
        let unstarted = Path::new("fake_dir_spawn").join("antsRegistration");
        let failing = |_: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            VersionRun::NotStarted
        };
        assert_eq!(cached_version_with(&unstarted, &failing), None);
        assert_eq!(cached_version_with(&unstarted, &failing), None);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let calls = AtomicU32::new(0);
        let silent = Path::new("fake_dir_silent").join("antsRegistration");
        let no_version = |_: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            VersionRun::Exited(None)
        };
        assert_eq!(cached_version_with(&silent, &no_version), None);
        assert_eq!(cached_version_with(&silent, &no_version), None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls = AtomicU32::new(0);
        let good = Path::new("fake_dir_good").join("antsRegistration");
        let known = |_: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            VersionRun::Exited(Some("2.6.5".to_owned()))
        };
        assert_eq!(cached_version_with(&good, &known).as_deref(), Some("2.6.5"));
        assert_eq!(cached_version_with(&good, &known).as_deref(), Some("2.6.5"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_bundled_program_reports_a_version_and_the_apply_program_borrows_its_sibling() {
        let Some(reg) = find_bundled_binary("antsRegistration") else {
            return;
        };
        let version = ants_version(&reg).expect("bundled antsRegistration prints a version");
        assert!(
            version.starts_with(|c: char| c.is_ascii_digit()),
            "{version}"
        );
        if let Some(apply) = find_bundled_binary("antsApplyTransforms") {
            assert_eq!(ants_version(&apply).as_deref(), Some(version.as_str()));
        }
        if let Some(n4) = find_bundled_binary("N4BiasFieldCorrection") {
            assert_eq!(ants_version(&n4).as_deref(), Some(version.as_str()));
        }
    }

    #[test]
    fn find_binary_nonexistent_returns_none() {
        assert!(find_binary("__dbsloc_nonexistent_binary_xyz__").is_none());
    }
}
