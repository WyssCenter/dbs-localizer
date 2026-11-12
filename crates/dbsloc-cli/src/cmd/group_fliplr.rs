// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use clap::{Args, Subcommand};
use dbsloc_pipeline::group::fliplr::{
    describe, resolve_or_build, FlipBuildError, FlipEnv, FlipStatus, TemplateFiles,
};
use dbsloc_pipeline::reg_backend::RegBackend;
use dbsloc_pipeline::template_catalog;
use serde_json::{json, Value};

use super::interrupt_flag;
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum FliplrCmd {
    /// build the left-right flip transform of a template space (minutes; cached for every later run)
    Build(BuildArgs),
    /// print where the flip transform of a template space is cached
    Show(ShowArgs),
}

#[derive(Args)]
pub struct BuildArgs {
    /// template of the space: a catalogue id or space, or the path of a T1w file (default: the shipped MNI template)
    #[arg(long, value_name = "PATH|ID")]
    pub template: Option<String>,
    /// build again even when a current transform exists
    #[arg(long)]
    pub force: bool,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ShowArgs {
    /// template of the space: a catalogue id or space, or the path of a T1w file (default: the shipped MNI template)
    #[arg(long, value_name = "PATH|ID")]
    pub template: Option<String>,
    /// print a JSON summary
    #[arg(long)]
    pub json: bool,
}

pub fn run(cmd: FliplrCmd) -> CmdResult {
    match cmd {
        FliplrCmd::Build(a) => build(&a),
        FliplrCmd::Show(a) => show(&a),
    }
}

fn env() -> FlipEnv {
    FlipEnv::from_environment(RegBackend::Auto, None)
}

fn template_files(spec: Option<&str>, env: &FlipEnv) -> Result<TemplateFiles, Fail> {
    let spec = spec.unwrap_or(template_catalog::default_entry().space);
    TemplateFiles::resolve(spec, env.shipped_root.as_deref(), env.user_root.as_deref())
        .map_err(|e| Fail::new(1, format!("dbsloc: error: {e}")))
}

fn status_json(s: &FlipStatus) -> Value {
    json!({
        "space": s.space,
        "cache_dir": s.cache_dir,
        "found": s.found.is_some(),
        "forward": s.found.as_ref().map(|p| &p.forward),
        "inverse": s.found.as_ref().map(|p| &p.inverse),
        "stale_cache": s.stale,
    })
}

fn build(a: &BuildArgs) -> CmdResult {
    let cancel = interrupt_flag();
    let env = env();
    let files = template_files(a.template.as_deref(), &env)?;
    let built = resolve_or_build(
        &files.space,
        Some(&files.t1),
        &env,
        a.force,
        Some(&cancel),
        &mut |l| err_line(&l),
    );
    let paths = match built {
        Ok(p) => p,
        Err(FlipBuildError::Cancelled) => return Ok(130),
        Err(FlipBuildError::Failed(why)) => {
            return Err(Fail::new(1, format!("dbsloc: error: E_GROUP_FLIP: {why}")))
        }
    };
    if a.json {
        out_json(&json!({
            "space": files.space,
            "forward": paths.forward,
            "inverse": paths.inverse,
        }));
    } else {
        out_line(&format!("forward  {}", paths.forward.display()));
        out_line(&format!("inverse  {}", paths.inverse.display()));
    }
    Ok(0)
}

fn show(a: &ShowArgs) -> CmdResult {
    let env = env();
    let files = template_files(a.template.as_deref(), &env)?;
    let status = describe(&files, &env);
    if a.json {
        out_json(&status_json(&status));
        return Ok(0);
    }
    out_line(&format!("space    {}", status.space));
    if let Some(d) = &status.cache_dir {
        out_line(&format!("cache    {}", d.display()));
    }
    match &status.found {
        Some(p) => {
            out_line(&format!("forward  {}", p.forward.display()));
            out_line(&format!("inverse  {}", p.inverse.display()));
        }
        None if status.stale => out_line(
            "not current: the cached transform was built from other template files; run `dbsloc group fliplr build`",
        ),
        None => out_line("not built; run `dbsloc group fliplr build`"),
    }
    Ok(0)
}
