// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Manual check: build or load the template space bridge for a target template and report it.
//!
//! `cargo run -p dbsloc-pipeline --release --example bridge_check -- <target_t1> [auto|wgpu|ants]`
//!
//! Prints the cache directory, the backend that built it, the elapsed seconds, the fitted
//! target-to-reference affine with its RMS residual, and `points_to_reference` for a few target
//! points (the origin, roughly the subthalamic level on both sides, and a point above the
//! midline).

use std::path::PathBuf;

use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::reg_backend::RegBackend;
use dbsloc_pipeline::space_bridge::{ensure_bridge, BRIDGE_RECORD};

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let target = PathBuf::from(
        args.next()
            .ok_or("usage: bridge_check <target_t1> [auto|wgpu|ants]")?,
    );
    let backend = match args.next() {
        Some(raw) => RegBackend::parse(&raw)?,
        None => RegBackend::Auto,
    };
    let mut opts = RunOptions::new(std::env::temp_dir(), "bridge_check");
    opts.reg_backend = backend;
    let (tx, rx) = std::sync::mpsc::channel();
    let printer = std::thread::spawn(move || {
        for event in rx {
            if let dbsloc_pipeline::protocol::RunEvent::LogLine { line } = event {
                eprintln!("{line}");
            }
        }
    });
    let t0 = std::time::Instant::now();
    let built = ensure_bridge(&target, &opts, None, Some(&tx)).map_err(|e| e.to_string());
    drop(tx);
    let _ = printer.join();
    let Some(bridge) = built? else {
        println!("no bridge needed: the target is the reference space (or the bridge is disabled)");
        return Ok(());
    };
    let elapsed = t0.elapsed().as_secs_f64();
    let recorded = std::fs::read_to_string(bridge.dir.join(BRIDGE_RECORD))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("backend").and_then(|b| b.as_str().map(str::to_owned)))
        .unwrap_or_else(|| "unknown".into());
    println!("cache dir: {}", bridge.dir.display());
    println!("target space: {}", bridge.target_space);
    println!("backend: {recorded} (requested {})", backend.as_str());
    println!("elapsed: {elapsed:.1} s");
    let fit = bridge.affine_fit_to_reference()?;
    println!("target -> reference affine (RAS mm):");
    for row in fit.matrix {
        println!(
            "  [{:10.5} {:10.5} {:10.5} {:10.5}]",
            row[0], row[1], row[2], row[3]
        );
    }
    println!("affine RMS residual: {:.3} mm", fit.rms_mm);
    let pts = [
        [0.0, 0.0, 0.0],
        [12.0, -13.0, -5.0],
        [-12.0, -13.0, -5.0],
        [0.0, -20.0, 10.0],
    ];
    let mapped = bridge.points_to_reference(&pts)?;
    for (p, m) in pts.iter().zip(mapped.iter()) {
        println!(
            "target ({:7.2} {:7.2} {:7.2}) -> reference ({:7.2} {:7.2} {:7.2})",
            p[0], p[1], p[2], m[0], m[1], m[2]
        );
    }
    Ok(())
}
