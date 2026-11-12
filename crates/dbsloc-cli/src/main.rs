// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod cmd;
mod fail;
mod localtime;
mod run;
mod spacedef;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use dbsloc_pipeline::legacy_env::legacy_env_note;

use crate::fail::{err_line, out_line, CmdResult};
use crate::run::ants_args::{prescan, AntsParsed};

#[derive(Parser)]
#[command(
    name = "dbsloc",
    bin_name = "dbsloc",
    about = "Wyss DBS Localizer pipeline CLI",
    version = version_text()
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// print package version
    Version,
    /// BIDS subject queries
    Subject {
        #[command(subcommand)]
        cmd: cmd::subject::SubjectCmd,
    },
    /// run pipeline (default: execute stage binaries; use --plan for plan/locks only)
    Run(Box<cmd::run::RunArgs>),
    /// visualization (checkreg; 3D scene)
    Viz {
        #[command(subcommand)]
        cmd: cmd::viz::VizCmd,
    },
    /// GPU registration backend
    Gpu {
        #[command(subcommand)]
        cmd: cmd::gpu::GpuCmd,
    },
    /// normalization template catalogue and downloads
    Template {
        #[command(subcommand)]
        cmd: cmd::template::TemplateCmd,
    },
    /// downloadable 3D viewer backdrop images (catalogue and downloads)
    Backdrop {
        #[command(subcommand)]
        cmd: cmd::backdrop::BackdropCmd,
    },
    #[command(
        about = "Lead-DBS atlases: list, download from the Lead-DBS server, remove downloaded ones"
    )]
    Atlas {
        #[command(subcommand)]
        cmd: cmd::atlas::AtlasCmd,
    },
    /// normative connectomes: catalogue, licence-checked downloads, import of your own folders
    Connectome {
        #[command(subcommand)]
        cmd: cmd::connectome::ConnectomeCmd,
    },
    /// data of an existing Lead-DBS installation: scan atlases and connectomes, copy them in
    Leaddbs {
        #[command(subcommand)]
        cmd: cmd::leaddbs::LeadDbsCmd,
    },
    /// Lead-Tutor: upload the electrode markers in MNI space to the Lead-Tutor validation service
    Tutor {
        #[command(subcommand)]
        cmd: cmd::tutor::TutorCmd,
    },
    /// stage lock / approval JSON
    Lock {
        #[command(subcommand)]
        cmd: cmd::lock::LockCmd,
    },
    /// import NIfTI files and DICOM folders into a subject of a BIDS dataset
    Import(Box<cmd::import::ImportArgs>),
    /// reconstruction: show and manually correct the lead placement
    Reco {
        #[command(subcommand)]
        cmd: cmd::reco::RecoCmd,
    },
    /// group analysis: record, patients, groups, clinical variables, Lead Group file
    Group {
        #[command(subcommand)]
        cmd: cmd::group::GroupCmd,
    },
    /// vendor session reports: inspect, import into a subject, list, anonymise
    Report {
        #[command(subcommand)]
        cmd: cmd::report::ReportCmd,
    },
    /// stimulation settings: list, show, set and import (Lead-DBS stimparameters)
    Stim {
        #[command(subcommand)]
        cmd: cmd::stim::StimCmd,
    },
}

fn dispatch(cli: Cli, ants: &AntsParsed) -> CmdResult {
    match cli.command {
        None => {
            let mut c = build_command();
            let _ = c.print_help();
            Ok(0)
        }
        Some(Command::Version) => {
            out_line(&format!("dbsloc {}", version_text()));
            Ok(0)
        }
        Some(Command::Subject { cmd }) => cmd::subject::run(cmd),
        Some(Command::Run(args)) => cmd::run::run(&args, ants),
        Some(Command::Viz { cmd }) => cmd::viz::run(cmd),
        Some(Command::Gpu { cmd }) => cmd::gpu::run(cmd),
        Some(Command::Template { cmd }) => cmd::template::run(cmd),
        Some(Command::Backdrop { cmd }) => cmd::backdrop::run(cmd),
        Some(Command::Atlas { cmd }) => cmd::atlas::run(cmd),
        Some(Command::Connectome { cmd }) => cmd::connectome::run(cmd),
        Some(Command::Leaddbs { cmd }) => cmd::leaddbs::run(cmd),
        Some(Command::Tutor { cmd }) => cmd::tutor::run(cmd),
        Some(Command::Lock { cmd }) => cmd::lock::run(cmd),
        Some(Command::Reco { cmd }) => cmd::reco::run(cmd),
        Some(Command::Stim { cmd }) => cmd::stim::run(cmd),
        Some(Command::Report { cmd }) => cmd::report::run(cmd),
        Some(Command::Group { cmd }) => cmd::group::run(cmd),
        Some(Command::Import(args)) => cmd::import::run(&args),
    }
}

fn version_text() -> &'static str {
    static TEXT: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(dbsloc_pipeline::version::version_string);
    TEXT.as_str()
}

fn build_command() -> clap::Command {
    Cli::command().mut_subcommand("run", |c| c.after_help(cmd::run::epilog()))
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn main() -> ExitCode {
    let vars = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v));
    if let Some(note) = legacy_env_note(vars) {
        err_line(&note);
    }
    let argv: Vec<OsString> = std::env::args_os().collect();
    let (argv, ants) = prescan(argv);
    let matches = build_command().get_matches_from(argv);
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    match dispatch(cli, &ants) {
        Ok(code) => exit_code(code),
        Err(f) => {
            if let Some(m) = &f.message {
                err_line(m);
            }
            exit_code(f.code)
        }
    }
}
