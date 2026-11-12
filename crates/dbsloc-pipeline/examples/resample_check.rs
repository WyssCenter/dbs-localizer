// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Manual check: resample the real MNI template and report grid + memory footprint.
//!
//! `cargo run -p dbsloc-pipeline --release --example resample_check -- <template.nii> <mm>`

use std::path::PathBuf;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let src = PathBuf::from(
        args.next()
            .ok_or("usage: resample_check <template.nii> [mm]")?,
    );
    let mm: f64 = args
        .next()
        .unwrap_or_else(|| "1.0".into())
        .parse()
        .map_err(|e| format!("mm: {e}"))?;
    let out = std::env::temp_dir().join(format!("dbsloc_iso_{mm}mm.nii.gz"));
    let t0 = std::time::Instant::now();
    match dbsloc_pipeline::resample::resample_isotropic(&src, &out, mm) {
        Ok(p) => {
            let src_len = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
            let out_len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            println!(
                "ok in {:.1}s: {} ({} MB) -> {} ({} MB)",
                t0.elapsed().as_secs_f64(),
                src.display(),
                src_len / 1_048_576,
                p.display(),
                out_len / 1_048_576
            );
        }
        Err(e) => println!("error: {e}"),
    }
    Ok(())
}
