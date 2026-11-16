// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The `fastwarp` command line. The same library runs the `fastwarp` program and
//! `fastwarp._core.cli_main` of the Python extension (`python -m fastwarp`, the `fastwarp` console
//! script), so both behave alike and return the same exit status. [`engine`] holds the engine
//! calls the Python functions use as well.

use std::ffi::OsString;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

use clap::Parser;

pub mod ants;
pub mod args;
pub mod bench;
mod commands;
pub mod engine;
pub mod image;
pub mod post;
pub mod recipe;
pub mod session;

pub use engine::Failure;

/// The exit status of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// Success; for `fits`, the registration fits.
    Ok = 0,
    /// The registration (or another engine step) failed.
    Registration = 1,
    /// Invalid arguments: an unknown option, a bad value, an unsupported file format.
    Usage = 2,
    /// A file could not be read or written.
    Io = 3,
    /// No usable GPU adapter.
    GpuUnavailable = 4,
    /// The registration grid does not fit the GPU.
    GpuCapacity = 5,
    /// The run was cancelled.
    Cancelled = 6,
}

impl ExitStatus {
    /// The status as a process exit code.
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// A failed command: the exit status and the one-line message printed on standard error.
#[derive(Clone, Debug, PartialEq)]
pub struct CliError {
    pub status: ExitStatus,
    pub message: String,
    /// A usage error that names something FastWarp does not support (rather than an invalid
    /// argument); the Python layer `fastwarp.ants` raises `UnsupportedArgument` for it.
    pub unsupported: bool,
}

impl CliError {
    pub fn new(status: ExitStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            unsupported: false,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ExitStatus::Usage, message)
    }

    /// A usage error for something FastWarp does not support.
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            unsupported: true,
            ..Self::usage(message)
        }
    }
}

impl From<Failure> for CliError {
    fn from(f: Failure) -> Self {
        let status = match &f {
            Failure::Invalid(_) => ExitStatus::Usage,
            Failure::GpuUnavailable(_) => ExitStatus::GpuUnavailable,
            Failure::GpuCapacity(_) => ExitStatus::GpuCapacity,
            Failure::Registration(_) => ExitStatus::Registration,
            Failure::Cancelled => ExitStatus::Cancelled,
            Failure::Read(_) | Failure::Write(_) => ExitStatus::Io,
        };
        CliError::new(status, f.to_string())
    }
}

/// Runs the command line on `args` (the program name first, as `std::env::args_os` gives it),
/// writing results to `out` and messages to `err`; returns the exit status.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match args::Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let text = e.render().to_string();
            if e.use_stderr() {
                let _ = err.write_all(text.as_bytes());
                let _ = err.flush();
                return ExitStatus::Usage.code();
            }
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
            return ExitStatus::Ok.code();
        }
    };
    let quiet = QuietPanics::begin();
    let result = catch_unwind(AssertUnwindSafe(|| {
        commands::dispatch(cli.command, &mut *out, &mut *err)
    }));
    drop(quiet);
    let result = result.unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        Err(CliError::new(
            ExitStatus::Registration,
            format!("internal error (panic): {msg}"),
        ))
    });
    let _ = out.flush();
    match result {
        Ok(status) => status.code(),
        Err(e) => {
            let _ = writeln!(err, "fastwarp: error: {}", one_line(&e.message));
            let _ = err.flush();
            e.status.code()
        }
    }
}

/// Runs the command line on the arguments of this process with standard output and error.
pub fn main_from_env() -> i32 {
    run(
        std::env::args_os(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

static ACTIVE_RUNS: AtomicUsize = AtomicUsize::new(0);
static HOOK: Once = Once::new();

struct QuietPanics;

impl QuietPanics {
    fn begin() -> Self {
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let backtrace = std::env::var_os("RUST_BACKTRACE").is_some_and(|v| v != "0");
                let silent = ACTIVE_RUNS.load(Ordering::SeqCst) > 0 && !backtrace;
                if !silent {
                    previous(info);
                }
            }));
        });
        ACTIVE_RUNS.fetch_add(1, Ordering::SeqCst);
        QuietPanics
    }
}

impl Drop for QuietPanics {
    fn drop(&mut self) {
        ACTIVE_RUNS.fetch_sub(1, Ordering::SeqCst);
    }
}

fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn run_args(args: &[&str]) -> (i32, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut all = vec!["fastwarp"];
        all.extend_from_slice(args);
        let code = run(all, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn help_and_version_succeed_on_stdout() {
        let (code, out, err) = run_args(&["--help"]);
        assert_eq!(code, 0);
        assert!(err.is_empty());
        for sub in [
            "register", "linear", "apply", "compose", "recipe", "gpu-info", "fits", "bench",
        ] {
            assert!(out.contains(sub), "{sub} missing in {out}");
        }
        let (code, out, _) = run_args(&["--version"]);
        assert_eq!(code, 0);
        assert!(out.contains(env!("CARGO_PKG_VERSION")));
        let (code, out, _) = run_args(&["register", "--help"]);
        assert_eq!(code, 0);
        assert!(out.contains("--iso-mm"));
        assert!(out.contains("smallest voxel"), "{out}");
        assert!(out.contains("register_syn defaults to 1.0 mm"), "{out}");
    }

    #[test]
    fn usage_errors_exit_2_on_stderr() {
        for args in [
            &[][..],
            &["frobnicate"],
            &["register", "f.nii"],
            &["register", "f.nii", "m.nii"],
            &[
                "register", "f.nii", "m.nii", "-o", "x_", "--fixed2", "f2.nii",
            ],
            &["register", "f.nii", "m.nii", "-o", "x_", "--iso-mm", "abc"],
            &["apply", "-r", "r.nii", "-i", "i.nii", "-o", "o.nii"],
            &[
                "compose",
                "-r",
                "r.nii",
                "-o",
                "o.nii",
                "-t",
                "a.mat",
                "--compression-level",
                "10",
            ],
            &["recipe", "--channels", "3"],
            &["linear", "f.nii", "m.nii", "--transform", "similarity"],
            &["linear", "f.nii", "m.nii", "--winsorize", "0.9,0.1"],
            &["bench", "f.nii"],
            &["bench", "--size", "huge"],
            &["bench", "--repeat", "0"],
            &["bench", "f.nii", "m.nii", "--size", "tiny"],
        ] {
            let (code, out, err) = run_args(args);
            assert_eq!(code, 2, "{args:?}: {err}");
            assert!(out.is_empty(), "{args:?}: {out}");
            assert!(!err.is_empty(), "{args:?}");
        }
    }

    #[test]
    fn recipe_prints_the_cohort_recipe() {
        let (code, out, _) = run_args(&["recipe"]);
        assert_eq!(code, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, recipe::Recipe::cohort(1).to_json());
        let (_, out, _) = run_args(&["recipe", "--channels", "2"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["syn"]["channel_weights"], serde_json::json!([0.5, 0.5]));
    }

    #[test]
    fn panic_child_probe() {
        if std::env::var_os("FASTWARP_PANIC_PROBE").is_none() {
            return;
        }
        let quiet = QuietPanics::begin();
        let caught = catch_unwind(|| panic!("probe boom"));
        drop(quiet);
        assert!(caught.is_err());
    }

    fn probe_stderr(backtrace: Option<&str>) -> String {
        let exe = std::env::current_exe().unwrap();
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["tests::panic_child_probe", "--exact", "--nocapture"])
            .env("FASTWARP_PANIC_PROBE", "1")
            .env_remove("RUST_BACKTRACE");
        if let Some(v) = backtrace {
            cmd.env("RUST_BACKTRACE", v);
        }
        let o = cmd.output().unwrap();
        assert!(o.status.success(), "{o:?}");
        String::from_utf8_lossy(&o.stderr).into_owned()
    }

    #[test]
    fn a_panic_prints_nothing_unless_backtrace_is_set() {
        let quiet = probe_stderr(None);
        assert!(!quiet.contains("probe boom"), "{quiet}");
        let zero = probe_stderr(Some("0"));
        assert!(!zero.contains("probe boom"), "{zero}");
        let loud = probe_stderr(Some("1"));
        assert!(loud.contains("probe boom"), "{loud}");
    }

    #[test]
    fn engine_failures_map_to_distinct_statuses() {
        let cases = [
            (Failure::Invalid("x".into()), 2),
            (Failure::Registration("x".into()), 1),
            (Failure::Read("x".into()), 3),
            (Failure::Write("x".into()), 3),
            (Failure::GpuUnavailable("x".into()), 4),
            (Failure::GpuCapacity("x".into()), 5),
            (Failure::Cancelled, 6),
        ];
        for (f, code) in cases {
            assert_eq!(CliError::from(f.clone()).status.code(), code, "{f:?}");
        }
    }

    #[test]
    fn errors_are_one_line_with_the_program_name() {
        let (code, out, err) = run_args(&["register", "f.mha", "m.nii", "-o", "x_"]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert_eq!(err.lines().count(), 1, "{err}");
        assert!(
            err.starts_with("fastwarp: error: fixed image f.mha"),
            "{err}"
        );
        let (code, _, err) = run_args(&[
            "apply", "-r", "nope.nii", "-i", "i.nii", "-o", "o.nii", "-t", "a.mat",
        ]);
        assert_eq!(code, 3, "{err}");
        assert!(err.contains("no such file"), "{err}");
    }

    #[test]
    fn negative_default_values_and_gpu_info_json_parse() {
        let cli = args::Cli::try_parse_from([
            "fastwarp", "apply", "-r", "r.nii", "-i", "i.nii", "-o", "o.nii", "-t", "a.mat", "-f",
            "-1024",
        ])
        .unwrap();
        match cli.command {
            args::Command::Apply(a) => assert_eq!(a.default_value, -1024.0),
            other => panic!("{other:?}"),
        }
        assert!(args::Cli::try_parse_from(["fastwarp", "gpu-info", "--json"]).is_ok());
    }

    #[test]
    fn transform_values() {
        let t = |p: &str, invert: bool| args::TransformArg {
            path: std::path::PathBuf::from(p),
            invert,
        };
        assert_eq!(args::parse_transform("a.mat"), Ok(t("a.mat", false)));
        assert_eq!(args::parse_transform("[a.mat,1]"), Ok(t("a.mat", true)));
        assert_eq!(
            args::parse_transform("[ a.mat , 0 ]"),
            Ok(t("a.mat", false))
        );
        assert_eq!(args::parse_transform("[a.mat]"), Ok(t("a.mat", false)));
        assert_eq!(
            args::parse_transform("[x,y.txt,true]"),
            Ok(t("x,y.txt", true))
        );
        assert!(args::parse_transform("[a.mat,2]").is_err());
        assert_eq!(
            args::parse_transform("[dir,with,comma.mat]"),
            Ok(t("dir,with,comma.mat", false))
        );
        assert!(args::parse_transform("[a.mat,1").is_err());
        assert!(args::parse_transform("[,1]").is_err());
        assert!(args::parse_transform("").is_err());
        let cli = args::Cli::try_parse_from([
            "fastwarp",
            "register",
            "f.nii",
            "m.nii",
            "-o",
            "x_",
            "--outputs",
            "split",
        ])
        .unwrap();
        match cli.command {
            args::Command::Register(a) => assert_eq!(a.outputs, args::Outputs::Split),
            other => panic!("{other:?}"),
        }
        assert!(args::Cli::try_parse_from([
            "fastwarp",
            "register",
            "f.nii",
            "m.nii",
            "-o",
            "x_",
            "--outputs",
            "tar",
        ])
        .is_err());
    }

    #[test]
    fn winsorize_values() {
        assert_eq!(args::parse_winsorize("0.005,0.995"), Ok((0.005, 0.995)));
        assert_eq!(args::parse_winsorize(" 0 , 1 "), Ok((0.0, 1.0)));
        assert!(args::parse_winsorize("0.5").is_err());
        assert!(args::parse_winsorize("a,b").is_err());
        assert!(args::parse_winsorize("0.6,0.4").is_err());
        assert!(args::parse_winsorize("-0.1,0.4").is_err());
    }
}
