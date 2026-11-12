// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Component, Path, PathBuf};

use crate::bids::simplify_verbatim;

pub const REPO_ROOT_ENV: &str = "DBSLOC_REPO_ROOT";

fn looks_like_repo(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file() && dir.join("resources").is_dir()
}

pub fn repo_root() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var(REPO_ROOT_ENV) {
        let p = PathBuf::from(raw.trim());
        if !raw.trim().is_empty() && p.is_dir() {
            return Some(p);
        }
    }
    if cfg!(debug_assertions) {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        if let Some(root) = manifest.parent().and_then(Path::parent) {
            if looks_like_repo(root) {
                return Some(root.to_path_buf());
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors().skip(1).take(6) {
            if looks_like_repo(anc) {
                return Some(anc.to_path_buf());
            }
        }
    }
    None
}

pub fn expand_user(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    let rest = if s == "~" {
        Some("")
    } else {
        s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\"))
    };
    match rest {
        Some(rest) => {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
            match home {
                Some(h) => PathBuf::from(h).join(rest),
                None => p.to_path_buf(),
            }
        }
        None => p.to_path_buf(),
    }
}

pub fn absolute(p: &Path) -> PathBuf {
    let p = expand_user(p);
    let abs = std::path::absolute(&p).unwrap_or(p);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            let mut out = simplify_verbatim(c);
            for part in tail.iter().rev() {
                if part == ".." {
                    out.pop();
                } else if part != "." {
                    out.push(part);
                }
            }
            return out;
        }
        match (cur.parent(), cur.components().next_back()) {
            (
                Some(parent),
                Some(c @ (Component::Normal(_) | Component::ParentDir | Component::CurDir)),
            ) => {
                tail.push(c.as_os_str().to_os_string());
                cur = parent;
            }
            _ => return abs.clone(),
        }
    }
}

pub const OUTPUT_DIR_ENV: &str = "DBSLOC_OUTPUT_DIR";

pub fn absolute_dataset_root(dataset_root: &Path) -> PathBuf {
    match dataset_root.canonicalize() {
        Ok(c) => simplify_verbatim(c),
        Err(_) => std::path::absolute(dataset_root).unwrap_or_else(|_| dataset_root.to_path_buf()),
    }
}

pub fn default_derivatives_root(bids_dir: &Path) -> PathBuf {
    derivatives_root_lexical(&absolute_dataset_root(bids_dir), None)
}

/// Pipeline output root that holds `sub-<id>/` (`specs/01` §7a): `output_dir` when set (made
/// absolute), else `<bids_dir>/derivatives/leaddbs`.
pub fn derivatives_root(bids_dir: &Path, output_dir: Option<&Path>) -> PathBuf {
    match output_dir.filter(|p| !p.as_os_str().is_empty()) {
        Some(p) => absolute(p),
        None => default_derivatives_root(bids_dir),
    }
}

/// [`derivatives_root`] as a pure path join, without touching the filesystem (for a UI thread).
pub fn derivatives_root_lexical(bids_dir: &Path, output_dir: Option<&Path>) -> PathBuf {
    match output_dir.filter(|p| !p.as_os_str().is_empty()) {
        Some(p) => p.to_path_buf(),
        None => bids_dir.join("derivatives").join("leaddbs"),
    }
}

pub fn py_float(v: f64) -> String {
    format!("{v:?}")
}

fn exe_candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut v = vec![dir.join(name)];
    if cfg!(windows) {
        v.push(dir.join(format!("{name}.exe")));
    }
    v
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for c in exe_candidates(&dir, name) {
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

pub const GUI_BIN_ENV: &str = "DBSLOC_GUI_BIN";

pub fn gui_binary() -> Option<PathBuf> {
    resolve_viz_binary("dbs-localizer", GUI_BIN_ENV)
}

pub fn resolve_viz_binary(name: &str, env_var: &str) -> Option<PathBuf> {
    if let Ok(raw) = std::env::var(env_var) {
        let p = PathBuf::from(raw.trim());
        if !raw.trim().is_empty() && p.is_file() {
            return Some(p);
        }
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        for c in exe_candidates(&dir, name) {
            if c.is_file() {
                return Some(absolute(&c));
            }
        }
    }
    if let Some(p) = on_path(name) {
        return Some(p);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(raw) = std::env::var("CARGO_TARGET_DIR") {
        if !raw.trim().is_empty() {
            let base = absolute(Path::new(raw.trim()));
            dirs.push(base.join("release"));
            dirs.push(base.join("debug"));
        }
    }
    if let Some(root) = repo_root() {
        dirs.push(root.join("target").join("release"));
        dirs.push(root.join("target").join("debug"));
    }
    for d in dirs {
        for c in exe_candidates(&d, name) {
            if c.is_file() {
                return Some(absolute(&c));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_float_matches_python_repr() {
        assert_eq!(py_float(1.0), "1.0");
        assert_eq!(py_float(1.5), "1.5");
        assert_eq!(py_float(0.75), "0.75");
        assert_eq!(py_float(2.0), "2.0");
        assert_eq!(py_float(0.7), "0.7");
    }

    #[test]
    fn expand_user_leaves_plain_paths() {
        assert_eq!(expand_user(Path::new("a/b")), PathBuf::from("a/b"));
    }

    #[test]
    fn absolute_canonicalizes_the_deepest_existing_ancestor() {
        let d = std::env::temp_dir().join(format!("dbsloc_paths_abs_{}", std::process::id()));
        std::fs::create_dir_all(d.join("a")).unwrap();
        let base = absolute(&d);
        assert_eq!(
            absolute(&d.join("a/b/c.nii")),
            base.join("a").join("b").join("c.nii")
        );
        assert_eq!(
            absolute(&d.join("a/x/../y.nii")),
            base.join("a").join("y.nii")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn derivatives_root_defaults_under_the_dataset_and_honours_a_chosen_folder() {
        let d = std::env::temp_dir().join(format!("dbsloc_paths_deriv_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let base = absolute(&d);
        assert_eq!(
            derivatives_root(&d, None),
            base.join("derivatives").join("leaddbs")
        );
        assert_eq!(
            derivatives_root(&d, Some(Path::new(""))),
            base.join("derivatives").join("leaddbs")
        );
        assert_eq!(derivatives_root(&d, Some(&d.join("out"))), base.join("out"));
        let rel = derivatives_root(&d, Some(Path::new("rel_out")));
        assert!(rel.is_absolute() && rel.ends_with("rel_out"), "{rel:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_viz_binary_is_none() {
        assert!(resolve_viz_binary("__dbsloc_no_such_bin__", "__DBSLOC_NO_SUCH_ENV__").is_none());
    }
}
