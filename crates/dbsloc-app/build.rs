// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RC");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let ico = manifest_dir.join("../dbsloc-viz/assets/brand/dbs-loc-icon.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap_or_default());
    if let Err(e) = embed_icon(&ico, &out_dir) {
        println!("cargo:warning=dbs-localizer: the executable icon was not embedded: {e}");
    }
}

fn embed_icon(ico: &Path, out_dir: &Path) -> Result<(), String> {
    let staged = out_dir.join("dbs-localizer.ico");
    std::fs::copy(ico, &staged).map_err(|e| format!("copy {}: {e}", ico.display()))?;
    let rc = out_dir.join("dbs-localizer.rc");
    std::fs::write(&rc, "1 ICON \"dbs-localizer.ico\"\r\n").map_err(|e| e.to_string())?;
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let linked = if target_env == "msvc" {
        let res = out_dir.join("dbs-localizer.res");
        let tool = find_rc_exe().ok_or("rc.exe (Windows SDK) not found; set RC to its path")?;
        let status = Command::new(&tool)
            .current_dir(out_dir)
            .args(["/nologo", "/fo"])
            .arg(&res)
            .arg(&rc)
            .status()
            .map_err(|e| format!("{}: {e}", tool.display()))?;
        if !status.success() {
            return Err(format!("{} exited with {status}", tool.display()));
        }
        res
    } else {
        let obj = out_dir.join("dbs-localizer.res.o");
        let status = Command::new("windres")
            .current_dir(out_dir)
            .args(["-i"])
            .arg(&rc)
            .args(["-O", "coff", "-o"])
            .arg(&obj)
            .status()
            .map_err(|e| format!("windres: {e}"))?;
        if !status.success() {
            return Err(format!("windres exited with {status}"));
        }
        obj
    };
    println!("cargo:rustc-link-arg-bins={}", linked.display());
    Ok(())
}

fn find_rc_exe() -> Option<PathBuf> {
    if let Some(p) = env::var_os("RC").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = on_path("rc.exe") {
        return Some(p);
    }
    let arch = match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => "x86",
    };
    let kits = env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
        .join(r"Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&kits)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("10."))
        })
        .collect();
    versions.sort();
    versions
        .into_iter()
        .rev()
        .map(|v| v.join(arch).join("rc.exe"))
        .find(|p| p.is_file())
}

fn on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}
