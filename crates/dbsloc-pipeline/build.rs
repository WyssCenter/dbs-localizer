// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DBSLOC_GIT_HASH");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    // .git/index is not watched: editors rewrite it constantly and each write would rebuild the crate.
    for p in watched_git_paths(&manifest_dir) {
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
    let hash = std::env::var("DBSLOC_GIT_HASH")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| git_describe(&manifest_dir))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=DBSLOC_GIT_HASH={hash}");
}

fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .arg("--no-optional-locks")
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    Some(text)
}

fn git_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(git_output(dir, &["rev-parse", "--git-path", name])?);
    Some(if p.is_absolute() { p } else { dir.join(p) })
}

fn watched_git_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.extend(git_path(dir, "HEAD"));
    paths.extend(git_path(dir, "packed-refs"));
    let ref_file = git_output(dir, &["symbolic-ref", "-q", "HEAD"])
        .and_then(|refname| git_path(dir, &refname));
    if let Some(ref_file) = ref_file {
        let watched = if ref_file.exists() {
            Some(ref_file)
        } else {
            git_path(dir, "refs").and_then(|root| nearest_dir_below(&ref_file, &root))
        };
        paths.extend(watched);
    }
    paths
}

fn nearest_dir_below(ref_file: &Path, root: &Path) -> Option<PathBuf> {
    ref_file
        .ancestors()
        .skip(1)
        .take_while(|a| *a != root)
        .find(|a| a.is_dir() && a.starts_with(root))
        .map(Path::to_path_buf)
}

fn git_describe(dir: &Path) -> Option<String> {
    let mut hash = git_output(dir, &["rev-parse", "--short=12", "HEAD"])?;
    let dirty = git_output(dir, &["status", "--porcelain", "--untracked-files=no"]).is_some();
    if dirty {
        hash.push_str("-dirty");
    }
    Some(hash)
}
