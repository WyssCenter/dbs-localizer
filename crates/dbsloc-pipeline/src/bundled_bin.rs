// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Locate ANTs/N4 binaries: bundled copy takes priority over system PATH.
//!
//! Port of `dbsloc/bundled_bin.py`.

use std::path::{Path, PathBuf};

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
    fn find_binary_nonexistent_returns_none() {
        assert!(find_binary("__dbsloc_nonexistent_binary_xyz__").is_none());
    }
}
