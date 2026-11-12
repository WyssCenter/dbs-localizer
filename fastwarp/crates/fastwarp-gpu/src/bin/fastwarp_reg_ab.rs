// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use fastwarp_gpu::ab::{
    brain_mask, compare, compare_affines, contacts_vs_stored, load_f64, render, AbResult,
    SubjectFiles, BRAIN_MASK_FRACTION,
};

/// Compares our SyN outputs with the ANTs outputs of a subject folder.
#[derive(Parser, Debug)]
#[command(name = "fastwarp_reg_ab")]
struct Cli {
    /// Subject folder (see `fastwarp_gpu::ab::SubjectFiles`).
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    /// Print the elements of a transform file and exit.
    #[arg(long)]
    inspect: Option<PathBuf>,
    /// Prefix of our outputs (as given to `fastwarp_reg_syn --out`).
    #[arg(long)]
    ours: Option<PathBuf>,
    /// Registration grid spacing used for the mask, in millimetres.
    #[arg(long, default_value_t = 1.0)]
    iso_mm: f64,
    /// Write the report as JSON.
    #[arg(long)]
    json: Option<PathBuf>,
    /// First affine transform file (an `.mat`, `.txt` or `Composite.h5`; element 0 is used).
    #[arg(long)]
    affine_a: Option<PathBuf>,
    /// Second affine transform file; with `--affine-a` and `--template` the two are compared.
    #[arg(long)]
    affine_b: Option<PathBuf>,
    /// Template T1w that defines the brain mask of the affine comparison.
    #[arg(long)]
    template: Option<PathBuf>,
    /// Only compare the contact coordinates of this Reco v2 JSON with our inverse composite.
    #[arg(long)]
    reco: Option<PathBuf>,
}

fn run(cli: &Cli) -> AbResult<()> {
    if let Some(p) = &cli.inspect {
        return inspect(p);
    }
    if let (Some(a), Some(b), Some(t)) = (&cli.affine_a, &cli.affine_b, &cli.template) {
        let first = |p: &std::path::Path| -> AbResult<fastwarp_reg::AffineTransform> {
            let chain = fastwarp_reg::transform::read_transform_file(p)?;
            match chain.queue().first() {
                Some(fastwarp_reg::transform::TransformElem::Affine(x)) => Ok(*x),
                _ => Err(format!("{}: element 0 is not an affine", p.display()).into()),
            }
        };
        let template = fastwarp_reg::register::resample_isotropic(&load_f64(t)?, cli.iso_mm)?;
        let mask = brain_mask(&template, BRAIN_MASK_FRACTION);
        let d = compare_affines(&first(a)?, &first(b)?, template.grid(), &mask);
        println!(
            "affine a vs b over {} mask voxels: max {:.4} mm, mean {:.4}, rms {:.4}; matrix max |diff| {:.2e}; offset diff {:.4?} mm; det a {:.5} b {:.5}",
            d.voxels, d.max_mm, d.mean_mm, d.rms_mm, d.matrix_max_abs, d.offset_diff, d.det_ours, d.det_ants
        );
        if let Some(p) = &cli.json {
            std::fs::write(p, serde_json::to_string_pretty(&d)?)?;
        }
        return Ok(());
    }
    let ours = cli.ours.as_ref().ok_or("--ours is required")?;
    if let Some(reco) = &cli.reco {
        let diffs = contacts_vs_stored(reco, ours)?;
        for d in &diffs {
            println!(
                "side {}: n {}, mean {:.4} mm, max {:.4} mm",
                d.side, d.points, d.mean_mm, d.max_mm
            );
        }
        if let Some(p) = &cli.json {
            std::fs::write(p, serde_json::to_string_pretty(&diffs)?)?;
        }
        return Ok(());
    }
    let files = SubjectFiles::from_dir(&cli.dir)?;
    let report = compare(&files, ours, cli.iso_mm)?;
    println!("{}", render(&report));
    if let Some(p) = &cli.json {
        std::fs::write(p, serde_json::to_string_pretty(&report)?)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fastwarp_reg_ab: {e}");
            ExitCode::FAILURE
        }
    }
}

fn inspect(path: &std::path::Path) -> AbResult<()> {
    let chain = fastwarp_reg::transform::read_transform_file(path)?;
    for (i, e) in chain.queue().iter().enumerate() {
        match e {
            fastwarp_reg::transform::TransformElem::Affine(a) => {
                println!(
                    "{i}: affine params {:?} fixed {:?}",
                    a.parameters(),
                    a.fixed_parameters()
                );
            }
            fastwarp_reg::transform::TransformElem::DisplacementField(f) => {
                let g = f.grid();
                println!(
                    "{i}: field size {:?} spacing {:?} origin {:?} direction {:?}",
                    g.size(),
                    g.spacing(),
                    g.origin(),
                    g.direction()
                );
            }
        }
    }
    Ok(())
}
