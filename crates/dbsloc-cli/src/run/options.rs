// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use dbsloc_pipeline::options::{PostopModality, RunOptions, VatBackend};
use dbsloc_pipeline::paths;
use dbsloc_pipeline::reg_backend::RegBackend;
use dbsloc_pipeline::stages::Stage;

use super::ants_args::AntsParsed;
use crate::fail::Fail;

pub use dbsloc_pipeline::options::NormGrid;

pub const NORM_METHODS: [&str; 1] = ["ants_syn"];
pub const REG_BACKENDS: [&str; 3] = ["auto", "wgpu", "ants"];
pub const RECON_METHODS: [&str; 4] = ["auto", "trac_core", "trac_core_native", "pacer"];
pub const LOG_MODES: [&str; 3] = ["default", "quiet", "plain"];
pub const VAT_BACKENDS: [&str; 3] = ["auto", "wgpu", "cpu"];

pub fn parse_norm_grid(raw: &str) -> Result<NormGrid, String> {
    NormGrid::parse(raw)
}

fn backend_flag(cli: Option<&str>) -> Result<Option<RegBackend>, Fail> {
    cli.map(|c| RegBackend::parse(c).map_err(|e| Fail::new(2, format!("dbsloc: error: {e}"))))
        .transpose()
}

pub struct Choices {
    pub norm_grid: Option<NormGrid>,
    pub norm_method: Option<String>,
    pub reg_backend: Option<RegBackend>,
    pub preprocess_backend: Option<RegBackend>,
    pub coregister_backend: Option<RegBackend>,
    pub normalize_backend: Option<RegBackend>,
    pub ants_threads: Option<i64>,
}

pub struct RunInputs<'a> {
    pub dataset_root: &'a std::path::Path,
    pub output_dir: Option<&'a std::path::Path>,
    pub leaddbs_atlases_dir: Option<&'a std::path::Path>,
    pub postop_modality: Option<PostopModality>,
    pub vat_backend: Option<VatBackend>,
    pub vat_labels: &'a [String],
    pub vat_stats_atlas: Option<&'a str>,
    pub subj_id: &'a str,
    pub stages: Vec<Stage>,
    pub template: Option<&'a PathBuf>,
    pub template_t2: Option<&'a PathBuf>,
    pub no_norm_multispectral: bool,
    pub recon_method: &'a str,
    pub recon_bright_metal: bool,
    pub el_model: Option<&'a str>,
    pub headless: bool,
    pub no_checkreg: bool,
    pub no_scene3d: bool,
    pub plan: bool,
    pub overwrite_approved: bool,
    pub fingerprint_locks: bool,
    pub no_index: bool,
}

pub fn build(
    inp: &RunInputs<'_>,
    choices: &Choices,
    ants: &AntsParsed,
) -> Result<RunOptions, Fail> {
    build_with_env(inp, choices, ants, &|name| std::env::var(name).ok())
}

fn explicit_fields(inp: &RunInputs<'_>, choices: &Choices) -> Vec<&'static str> {
    [
        (inp.headless, "headless"),
        (inp.no_checkreg, "checkreg"),
        (inp.no_scene3d, "no_scene3d"),
        (choices.norm_grid.is_some(), "norm_isotropic_mm"),
        (choices.norm_method.is_some(), "norm_method"),
        (choices.reg_backend.is_some(), "reg_backend"),
        (
            choices.preprocess_backend.is_some(),
            "reg_backend_preprocess",
        ),
        (
            choices.coregister_backend.is_some(),
            "reg_backend_coregister",
        ),
        (choices.normalize_backend.is_some(), "reg_backend_normalize"),
        (choices.ants_threads.is_some(), "ants_max_threads"),
        (inp.output_dir.is_some(), "output_dir"),
        (inp.leaddbs_atlases_dir.is_some(), "leaddbs_atlases_dir"),
        (inp.postop_modality.is_some(), "postop_modality"),
        (inp.vat_backend.is_some(), "vat_backend"),
        (!inp.vat_labels.is_empty(), "vat_labels"),
        (inp.vat_stats_atlas.is_some(), "vat_stats_atlas"),
    ]
    .into_iter()
    .filter_map(|(set, field)| set.then_some(field))
    .collect()
}

pub fn build_with_env(
    inp: &RunInputs<'_>,
    choices: &Choices,
    ants: &AntsParsed,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<RunOptions, Fail> {
    let mut o = RunOptions::new(paths::absolute(inp.dataset_root), inp.subj_id);
    o.stages = inp.stages.clone();
    o.normalization_template = inp.template.cloned();
    o.normalization_template_t2 = inp.template_t2.cloned();
    o.norm_multispectral = !inp.no_norm_multispectral;
    o.recon_method = inp.recon_method.to_owned();
    o.recon_bright_metal = inp.recon_bright_metal;
    o.el_model = inp.el_model.map(str::to_owned);
    o.ants_max_threads = choices
        .ants_threads
        .map(|v| u32::try_from(v.max(1)).unwrap_or(u32::MAX));
    o.ants_extra = ants.extra.clone();
    o.headless = inp.headless;
    o.checkreg = !inp.no_checkreg;
    o.no_scene3d = inp.no_scene3d;
    o.plan_only = inp.plan;
    o.overwrite_approved = inp.overwrite_approved;
    o.fingerprint_locks = inp.fingerprint_locks;
    o.use_index = !inp.no_index;
    o.output_dir = inp.output_dir.map(paths::absolute);
    o.leaddbs_atlases_dir = inp.leaddbs_atlases_dir.map(paths::absolute);
    if let Some(m) = inp.postop_modality {
        o.postop_modality = m;
    }
    if let Some(b) = inp.vat_backend {
        o.vat_backend = b;
    }
    o.vat_labels = inp.vat_labels.to_vec();
    o.vat_stats_atlas = inp.vat_stats_atlas.map(str::to_owned);
    if let Some(grid) = choices.norm_grid {
        o.norm_isotropic_mm = grid;
    }
    if let Some(m) = &choices.norm_method {
        o.norm_method.clone_from(m);
    }
    if let Some(b) = choices.reg_backend {
        o.reg_backend = b;
    }
    o.reg_backend_preprocess = choices.preprocess_backend;
    o.reg_backend_coregister = choices.coregister_backend;
    o.reg_backend_normalize = choices.normalize_backend;
    let explicit = explicit_fields(inp, choices);
    o.apply_env(&|field| explicit.contains(&field), lookup)
        .map_err(|e| Fail::new(2, format!("dbsloc: error: {e}")))?;
    Ok(o)
}

#[derive(Default, Clone, Copy)]
pub struct StepBackendFlags<'a> {
    pub preprocess: Option<&'a str>,
    pub coregister: Option<&'a str>,
    pub normalize: Option<&'a str>,
}

pub fn resolve_choices(
    cli_grid: Option<NormGrid>,
    cli_method: Option<&str>,
    cli_backend: Option<&str>,
    cli_steps: StepBackendFlags<'_>,
    cli_threads: Option<i64>,
) -> Result<Choices, Fail> {
    Ok(Choices {
        norm_grid: cli_grid,
        norm_method: cli_method.map(str::to_owned),
        reg_backend: backend_flag(cli_backend)?,
        preprocess_backend: backend_flag(cli_steps.preprocess)?,
        coregister_backend: backend_flag(cli_steps.coregister)?,
        normalize_backend: backend_flag(cli_steps.normalize)?,
        ants_threads: cli_threads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(root: &std::path::Path) -> RunInputs<'_> {
        RunInputs {
            dataset_root: root,
            output_dir: None,
            leaddbs_atlases_dir: None,
            postop_modality: None,
            vat_backend: None,
            vat_labels: &[],
            vat_stats_atlas: None,
            subj_id: "1",
            stages: Vec::new(),
            template: None,
            template_t2: None,
            no_norm_multispectral: false,
            recon_method: "auto",
            recon_bright_metal: false,
            el_model: None,
            headless: false,
            no_checkreg: false,
            no_scene3d: false,
            plan: false,
            overwrite_approved: false,
            fingerprint_locks: false,
            no_index: false,
        }
    }

    fn no_choices() -> Choices {
        Choices {
            norm_grid: None,
            norm_method: None,
            reg_backend: None,
            preprocess_backend: None,
            coregister_backend: None,
            normalize_backend: None,
            ants_threads: None,
        }
    }

    fn with_env(
        inp: &RunInputs<'_>,
        choices: &Choices,
        vars: &[(&str, &str)],
    ) -> Result<RunOptions, Fail> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        build_with_env(inp, choices, &AntsParsed::default(), &|name| {
            vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn grid_values() {
        assert_eq!(parse_norm_grid("1.5"), Ok(NormGrid::Mm(1.5)));
        assert_eq!(parse_norm_grid("2"), Ok(NormGrid::Mm(2.0)));
        assert_eq!(parse_norm_grid(" 0.8 "), Ok(NormGrid::Mm(0.8)));
        assert_eq!(parse_norm_grid("FULL"), Ok(NormGrid::Full));
        assert_eq!(parse_norm_grid("template"), Ok(NormGrid::Template));
        assert_eq!(parse_norm_grid("0.3"), Ok(NormGrid::Mm(0.3)));
        assert_eq!(parse_norm_grid("3.0"), Ok(NormGrid::Mm(3.0)));
        for bad in ["0", "-1", "0.01", "3.5", "abc", "nan", "inf", ""] {
            let e = parse_norm_grid(bad).unwrap_err();
            assert!(
                e.contains("millimetres from 0.3 to 3.0, 'full' or 'template'"),
                "{e}"
            );
        }
    }

    #[test]
    fn flags_win_over_the_environment() {
        let root = std::path::Path::new("/data");
        let mut inp = inputs(root);
        inp.no_checkreg = true;
        let choices = Choices {
            norm_grid: Some(NormGrid::Mm(2.0)),
            norm_method: Some("ants_syn".to_owned()),
            reg_backend: Some(RegBackend::Ants),
            normalize_backend: Some(RegBackend::Ants),
            ants_threads: Some(3),
            ..no_choices()
        };
        let o = with_env(
            &inp,
            &choices,
            &[
                ("DBSLOC_NORM_ISOTROPIC_MM", "bogus"),
                ("DBSLOC_NORM_METHOD", "spm"),
                ("DBSLOC_REG_BACKEND", "cuda"),
                ("DBSLOC_NORMALIZE_BACKEND", "wgpu"),
                ("DBSLOC_ANTS_MAX_THREADS", "x"),
                ("DBSLOC_OPEN_CHECKREG", "1"),
            ],
        )
        .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.norm_isotropic_mm, NormGrid::Mm(2.0));
        assert_eq!(o.reg_backend, RegBackend::Ants);
        assert_eq!(o.reg_backend_normalize, Some(RegBackend::Ants));
        assert_eq!(o.ants_max_threads, Some(3));
        assert!(!o.checkreg);
        assert!(!o.stop_on_error);
    }

    #[test]
    fn the_environment_fills_what_no_flag_sets() {
        let root = std::path::Path::new("/data");
        let o = with_env(
            &inputs(root),
            &no_choices(),
            &[
                ("DBSLOC_NORM_ISOTROPIC_MM", "Full"),
                ("DBSLOC_REG_BACKEND", " ANTs "),
                ("DBSLOC_PREPROCESS_BACKEND", "wgpu"),
                ("DBSLOC_ANTS_MAX_THREADS", "4"),
                ("DBSLOC_HEADLESS", "1"),
                ("DBSLOC_OPEN_SCENE3D", "0"),
            ],
        )
        .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.norm_isotropic_mm, NormGrid::Full);
        assert_eq!(o.reg_backend, RegBackend::Ants);
        assert_eq!(o.reg_backend_preprocess, Some(RegBackend::Wgpu));
        assert_eq!(o.ants_max_threads, Some(4));
        assert!(o.headless);
        assert!(o.no_scene3d);
        assert!(o.checkreg);
        assert!(o.stop_on_error);
        let o = with_env(&inputs(root), &no_choices(), &[])
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.norm_isotropic_mm, NormGrid::Template);
        assert_eq!(o.reg_backend, RegBackend::Auto);
        assert_eq!(o.ants_max_threads, None);
    }

    #[test]
    fn output_dir_flag_beats_the_variable_and_both_are_made_absolute() {
        let root = std::path::Path::new("/data");
        let o = with_env(
            &inputs(root),
            &no_choices(),
            &[("DBSLOC_OUTPUT_DIR", "env_out")],
        )
        .unwrap_or_else(|e| panic!("{:?}", e.message));
        let env_out = o.output_dir.unwrap_or_default();
        assert!(env_out.is_absolute() && env_out.ends_with("env_out"));
        let mut inp = inputs(root);
        let flag = std::path::Path::new("flag_out");
        inp.output_dir = Some(flag);
        let o = with_env(&inp, &no_choices(), &[("DBSLOC_OUTPUT_DIR", "env_out")])
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        let flag_out = o.output_dir.unwrap_or_default();
        assert!(flag_out.is_absolute() && flag_out.ends_with("flag_out"));
        let o = with_env(&inputs(root), &no_choices(), &[])
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.output_dir, None);
    }

    #[test]
    fn atlases_dir_flag_beats_the_variable_and_both_are_made_absolute() {
        let root = std::path::Path::new("/data");
        let var = [("DBSLOC_LEADDBS_ATLASES_DIR", "env_atlases")];
        let o = with_env(&inputs(root), &no_choices(), &var)
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        let env_dir = o.leaddbs_atlases_dir.unwrap_or_default();
        assert!(env_dir.is_absolute() && env_dir.ends_with("env_atlases"));
        let mut inp = inputs(root);
        inp.leaddbs_atlases_dir = Some(std::path::Path::new("flag_atlases"));
        let o = with_env(&inp, &no_choices(), &var).unwrap_or_else(|e| panic!("{:?}", e.message));
        let flag_dir = o.leaddbs_atlases_dir.unwrap_or_default();
        assert!(flag_dir.is_absolute() && flag_dir.ends_with("flag_atlases"));
        let o = with_env(&inputs(root), &no_choices(), &[])
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.leaddbs_atlases_dir, None);
    }

    #[test]
    fn postop_modality_flag_beats_the_variable() {
        let root = std::path::Path::new("/data");
        let var = [("DBSLOC_POSTOP_MODALITY", "mri")];
        let o = with_env(&inputs(root), &no_choices(), &var)
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.postop_modality, PostopModality::Mri);
        let mut inp = inputs(root);
        inp.postop_modality = Some(PostopModality::Ct);
        let o = with_env(&inp, &no_choices(), &var).unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.postop_modality, PostopModality::Ct);
        let o = with_env(&inputs(root), &no_choices(), &[])
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.postop_modality, PostopModality::Auto);
    }

    #[test]
    fn vat_flags_beat_the_variables() {
        let root = std::path::Path::new("/data");
        let var = [
            ("DBSLOC_VAT_BACKEND", "cpu"),
            ("DBSLOC_VAT_LABELS", "env_a,env_b"),
            ("DBSLOC_VAT_STATS_ATLAS", "env_atlas"),
        ];
        let o = with_env(&inputs(root), &no_choices(), &var)
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.vat_backend, VatBackend::Cpu);
        assert_eq!(o.vat_labels, ["env_a", "env_b"]);
        assert_eq!(o.vat_stats_atlas.as_deref(), Some("env_atlas"));
        let labels = vec!["flag".to_owned()];
        let mut inp = inputs(root);
        inp.vat_backend = Some(VatBackend::Wgpu);
        inp.vat_labels = &labels;
        inp.vat_stats_atlas = Some("flag_atlas");
        let o = with_env(&inp, &no_choices(), &var).unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.vat_backend, VatBackend::Wgpu);
        assert_eq!(o.vat_labels, ["flag"]);
        assert_eq!(o.vat_stats_atlas.as_deref(), Some("flag_atlas"));
        let o = with_env(&inputs(root), &no_choices(), &[])
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(o.vat_backend, VatBackend::Auto);
        assert!(o.vat_labels.is_empty());
        assert_eq!(o.vat_stats_atlas, None);
    }

    #[test]
    fn stop_on_error_follows_the_variable_then_checkreg() {
        let root = std::path::Path::new("/data");
        let mut off = inputs(root);
        off.no_checkreg = true;
        let stop = |inp: &RunInputs<'_>, vars: &[(&str, &str)]| {
            with_env(inp, &no_choices(), vars)
                .unwrap_or_else(|e| panic!("{:?}", e.message))
                .stop_on_error
        };
        assert!(stop(&inputs(root), &[]));
        assert!(!stop(&off, &[]));
        assert!(stop(&off, &[("DBSLOC_STOP_ON_ERROR", "1")]));
        assert!(!stop(&inputs(root), &[("DBSLOC_STOP_ON_ERROR", "0")]));
    }

    #[test]
    fn invalid_environment_values_exit_2_and_name_the_variable() {
        let root = std::path::Path::new("/data");
        for (name, bad) in [
            ("DBSLOC_NORM_ISOTROPIC_MM", "auto"),
            ("DBSLOC_NORM_METHOD", "spm"),
            ("DBSLOC_REG_BACKEND", "cuda"),
            ("DBSLOC_COREGISTER_BACKEND", "cuda"),
            ("DBSLOC_ANTS_MAX_THREADS", "two"),
        ] {
            let Err(e) = with_env(&inputs(root), &no_choices(), &[(name, bad)]) else {
                panic!("{name}={bad} was accepted");
            };
            assert_eq!(e.code, 2);
            let m = e.message.unwrap_or_default();
            assert!(m.starts_with("dbsloc: error: ") && m.contains(name), "{m}");
        }
    }

    #[test]
    fn backend_flags_parse_or_exit_2() {
        let flags = StepBackendFlags {
            normalize: Some("wgpu"),
            ..StepBackendFlags::default()
        };
        let c = resolve_choices(None, None, Some("ants"), flags, Some(2))
            .unwrap_or_else(|e| panic!("{:?}", e.message));
        assert_eq!(c.reg_backend, Some(RegBackend::Ants));
        assert_eq!(c.normalize_backend, Some(RegBackend::Wgpu));
        assert_eq!(c.preprocess_backend, None);
        let Err(e) = resolve_choices(None, None, Some("cuda"), StepBackendFlags::default(), None)
        else {
            panic!("cuda was accepted");
        };
        assert_eq!(e.code, 2);
    }
}
