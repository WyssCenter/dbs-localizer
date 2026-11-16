// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The ANTs compatibility mode: `fastwarp ants-registration ARGS...` and
//! `fastwarp ants-apply-transforms ARGS...` take the arguments of `antsRegistration` and
//! `antsApplyTransforms` (ANTs 2.6.5), run the subset FastWarp supports and write the files ANTs
//! writes. Anything that would change the result and is not supported is refused before any file
//! is read or the GPU is opened.
//!
//! [`parse_registration`] and [`parse_apply`] are pure functions of the argument list (no file
//! access, no environment), so the same rules serve the command line and the Python layer
//! `fastwarp.ants`, which runs the plans with [`run_registration`] and [`run_apply_plan`].

use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::image::is_nifti_name;
use crate::post::Interp;

mod apply;
mod registration;
pub(crate) mod run;

pub use apply::{parse_apply, ApplyCall, ApplyOutput, ApplyPlan};
pub use registration::{
    parse_registration, InitialArg, LinearStageSpec, RegistrationCall, RegistrationPlan, StageSpec,
    SynStageSpec,
};
pub use run::{
    run_apply_plan, run_registration, Images, KeptImage, MemoryImage, RegistrationOutcome,
    RunOptions,
};

/// Why an ANTs argument list is refused: the message, and whether it names something FastWarp
/// does not support or an invalid argument. The command line exits with status 2 for both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String, pub RefusalKind);

/// The two kinds of [`Refusal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    /// Valid for ANTs, but not supported by FastWarp.
    Unsupported,
    /// Invalid for ANTs as well (a malformed value, an unknown option).
    Invalid,
}

impl Refusal {
    /// An invalid argument.
    pub fn invalid(message: impl Into<String>) -> Self {
        Refusal(message.into(), RefusalKind::Invalid)
    }

    /// Whether FastWarp does not support what the arguments ask for.
    pub fn is_unsupported(&self) -> bool {
        self.1 == RefusalKind::Unsupported
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

pub(crate) fn refuse<T>(message: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::invalid(message))
}

pub(crate) fn unsupported<T>(
    what: impl fmt::Display,
    why: impl fmt::Display,
) -> Result<T, Refusal> {
    Err(Refusal(
        format!("{what} is not supported: {why}"),
        RefusalKind::Unsupported,
    ))
}

/// Whether an option takes a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Takes {
    /// A flag without a value (`-h`, `--version`).
    Nothing,
    /// A value that may be left out (`--float`, `-v`).
    Optional,
    /// A value that must be given.
    Required,
}

/// One option of a program: its short and long name, its canonical key and its value.
pub(crate) struct OptSpec {
    pub short: Option<&'static str>,
    pub long: &'static str,
    pub takes: Takes,
}

/// One option as given: the canonical long name, the spelling used, and its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Opt {
    pub key: &'static str,
    pub spelled: String,
    pub value: Option<String>,
}

impl Opt {
    pub fn value(&self) -> Result<&str, Refusal> {
        match &self.value {
            Some(v) => Ok(v),
            None => refuse(format!("{} needs a value", self.spelled)),
        }
    }

    pub fn shown(&self) -> String {
        match &self.value {
            Some(v) => format!("{} {v}", self.spelled),
            None => self.spelled.clone(),
        }
    }
}

fn is_option(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next() == Some('-')
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '-')
}

fn depth(s: &str) -> i64 {
    s.chars().fold(0i64, |d, c| match c {
        '[' => d + 1,
        ']' => d - 1,
        _ => d,
    })
}

/// `s` without the white space around `[`, `]` and `,` (the ANTs scripts pass
/// `[ 1000x500,1e-6,10 ]` as separate shell words).
pub(crate) fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut segment = String::new();
    for c in s.chars() {
        if matches!(c, '[' | ']' | ',') {
            out.push_str(segment.trim());
            segment.clear();
            out.push(c);
        } else {
            segment.push(c);
        }
    }
    out.push_str(segment.trim());
    out
}

/// Splits `args` into options of `program`: a value follows its option, and the words of a
/// value with brackets that span several shell words are joined.
pub(crate) fn tokenize(args: &[String], specs: &[OptSpec]) -> Result<Vec<Opt>, Refusal> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let token = args[i].as_str();
        i += 1;
        if !is_option(token) {
            return refuse(format!("unexpected argument {token:?}"));
        }
        let spec = specs
            .iter()
            .find(|s| s.long == token || s.short == Some(token))
            .ok_or_else(|| Refusal::invalid(format!("unknown option {token}")))?;
        let mut value = None;
        if spec.takes != Takes::Nothing && i < args.len() && !is_option(&args[i]) {
            let mut v = args[i].clone();
            i += 1;
            while depth(&v) > 0 && i < args.len() {
                v.push(' ');
                v.push_str(&args[i]);
                i += 1;
            }
            if depth(&v) != 0 {
                return refuse(format!("{token} {v}: unbalanced brackets"));
            }
            value = Some(tidy(&v));
        }
        if spec.takes == Takes::Required && value.is_none() {
            return refuse(format!("{token} needs a value"));
        }
        out.push(Opt {
            key: spec.long,
            spelled: token.to_string(),
            value,
        });
    }
    Ok(out)
}

/// A value of the form `NAME[a,b,...]`, `[a,b,...]` or `a`: the name before the bracket (empty
/// for `[...]` and for a bare value) and the arguments (a bare value is one argument). Empty
/// trailing arguments are dropped, so their defaults apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Call {
    pub name: String,
    pub args: Vec<String>,
    pub bracketed: bool,
}

pub(crate) fn call(value: &str) -> Result<Call, Refusal> {
    let Some(open) = value.find('[') else {
        if value.contains(']') {
            return refuse(format!("{value}: unbalanced brackets"));
        }
        return Ok(Call {
            name: String::new(),
            args: vec![value.to_string()],
            bracketed: false,
        });
    };
    let inner = value[open + 1..]
        .strip_suffix(']')
        .filter(|s| !s.contains('[') && !s.contains(']'))
        .ok_or_else(|| Refusal::invalid(format!("{value}: expected NAME[arguments]")))?;
    let mut args: Vec<String> = inner.split(',').map(|a| a.trim().to_string()).collect();
    while args.last().is_some_and(|a| a.is_empty()) {
        args.pop();
    }
    Ok(Call {
        name: value[..open].trim().to_string(),
        args,
        bracketed: true,
    })
}

pub(crate) fn parse_bool(opt: &Opt, default_bare: bool) -> Result<bool, Refusal> {
    match opt.value.as_deref() {
        None => Ok(default_bare),
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            _ => refuse(format!("{}: expected 0 or 1, got {v:?}", opt.spelled)),
        },
    }
}

pub(crate) fn parse_f64(text: &str, what: &str) -> Result<f64, Refusal> {
    text.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| Refusal::invalid(format!("{what}: {text:?} is not a number")))
}

pub(crate) fn parse_u32(text: &str, what: &str) -> Result<u32, Refusal> {
    text.parse::<u32>()
        .map_err(|_| Refusal::invalid(format!("{what}: {text:?} is not a non-negative integer")))
}

/// `MxNxO` as integers.
pub(crate) fn parse_u32_list(text: &str, what: &str) -> Result<Vec<u32>, Refusal> {
    text.split('x').map(|t| parse_u32(t, what)).collect()
}

/// The canonical form of a path for comparing two arguments without touching the file system:
/// `.` components dropped, separators unified.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| -> Vec<String> {
        p.components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    };
    norm(a) == norm(b)
}

/// Refuses an image file that is not NIfTI.
pub(crate) fn check_image_name(path: &Path) -> Result<(), Refusal> {
    if is_nifti_name(path) {
        Ok(())
    } else {
        unsupported(
            format!("image {}", path.display()),
            "FastWarp reads and writes NIfTI images (.nii, .nii.gz) only.",
        )
    }
}

pub(crate) fn path_arg(text: &str, what: &str) -> Result<PathBuf, Refusal> {
    if text.is_empty() {
        return refuse(format!("{what}: expected a file name"));
    }
    Ok(PathBuf::from(text))
}

/// `-n` of both programs: `Linear`, `NearestNeighbor` and `LanczosWindowedSinc` (any case, as
/// ANTs lower-cases the name).
pub(crate) fn parse_interpolation(value: &str) -> Result<Interp, Refusal> {
    let c = call(value)?;
    let name = if c.bracketed {
        c.name.clone()
    } else {
        value.to_string()
    };
    let interp = match name.to_ascii_lowercase().as_str() {
        "linear" => Interp::Linear,
        "nearestneighbor" => Interp::NearestNeighbor,
        "lanczoswindowedsinc" => Interp::LanczosWindowedSinc,
        "multilabel"
        | "gaussian"
        | "bspline"
        | "cosinewindowedsinc"
        | "welchwindowedsinc"
        | "hammingwindowedsinc"
        | "genericlabel" => {
            return unsupported(
                format!("interpolation {name}"),
                "use Linear, NearestNeighbor or LanczosWindowedSinc.",
            )
        }
        _ => {
            return refuse(format!(
                "unrecognized interpolation option {}",
                name.to_ascii_lowercase()
            ))
        }
    };
    if c.bracketed && !c.args.is_empty() {
        return refuse(format!("interpolation {name} takes no parameters"));
    }
    Ok(interp)
}

/// The ANTs name of an interpolator.
pub fn interpolation_name(i: Interp) -> &'static str {
    match i {
        Interp::Linear => "Linear",
        Interp::NearestNeighbor => "NearestNeighbor",
        Interp::LanczosWindowedSinc => "LanczosWindowedSinc",
    }
}

/// Voxel type of an output image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelType {
    Char,
    UChar,
    Short,
    Int,
    Float,
    Double,
}

impl PixelType {
    /// The `antsApplyTransforms -u` name.
    pub fn name(self) -> &'static str {
        match self {
            PixelType::Char => "char",
            PixelType::UChar => "uchar",
            PixelType::Short => "short",
            PixelType::Int => "int",
            PixelType::Float => "float",
            PixelType::Double => "double",
        }
    }

    /// The type ANTs writes by default: `float` with `--float 1`, else `double`.
    pub fn internal(float: bool) -> Self {
        if float {
            PixelType::Float
        } else {
            PixelType::Double
        }
    }
}

pub(crate) fn dimensionality(opt: &Opt) -> Result<(), Refusal> {
    match opt.value()? {
        "3" => Ok(()),
        v @ ("2" | "4") => unsupported(
            format!("{} {v}", opt.spelled),
            "FastWarp registers and resamples 3-D images.",
        ),
        v => refuse(format!("{}: expected 2, 3 or 4, got {v:?}", opt.spelled)),
    }
}

/// The seed of every linear stage: `--random-seed` when given, else the value of the
/// environment variable `ANTS_RANDOM_SEED` when set (pass it in), else FastWarp's fixed default
/// 20260910 (ANTs would use the clock).
pub fn resolve_seed(
    random_seed: Option<u32>,
    ants_random_seed: Option<&str>,
) -> Result<u32, Refusal> {
    match (random_seed, ants_random_seed) {
        (Some(seed), _) => Ok(seed),
        (None, Some(text)) => text.trim().parse::<u32>().map_err(|_| {
            Refusal::invalid(format!(
                "ANTS_RANDOM_SEED={text:?} is not a non-negative integer"
            ))
        }),
        (None, None) => Ok(fastwarp_reg::linear::COHORT_RANDOM_SEED),
    }
}

pub(crate) fn version_text(program: &str) -> String {
    format!(
        "fastwarp {} ({program} compatibility mode, ANTs 2.6.5 subset)\n",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn tidy_removes_spaces_around_brackets_and_commas() {
        assert_eq!(tidy("[ 1000x500,1e-6,10 ]"), "[1000x500,1e-6,10]");
        assert_eq!(tidy("MI[ a b.nii , c.nii ,1 ]"), "MI[a b.nii,c.nii,1]");
        assert_eq!(tidy(" x "), "x");
    }

    #[test]
    fn calls() {
        let c = call("Rigid[0.1]").unwrap();
        assert_eq!(
            (c.name.as_str(), c.args.clone(), c.bracketed),
            ("Rigid", vec!["0.1".to_string()], true)
        );
        let c = call("[a,b,,]").unwrap();
        assert_eq!(c.args, vec!["a", "b"]);
        assert_eq!(c.name, "");
        let c = call("prefix_").unwrap();
        assert!(!c.bracketed);
        assert_eq!(c.args, vec!["prefix_"]);
        assert!(call("a[b").is_err());
        assert!(call("a[b]c").is_err());
        assert!(call("a[b[c]]").is_err());
        assert!(call("a]").is_err());
    }

    #[test]
    fn options_and_values() {
        let specs = [
            OptSpec {
                short: Some("-c"),
                long: "--convergence",
                takes: Takes::Required,
            },
            OptSpec {
                short: None,
                long: "--float",
                takes: Takes::Optional,
            },
            OptSpec {
                short: Some("-f"),
                long: "--default-value",
                takes: Takes::Required,
            },
        ];
        let args: Vec<String> = ["-c", "[", "10x5,1e-6,10", "]", "--float", "-f", "-1024"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let opts = tokenize(&args, &specs).unwrap();
        assert_eq!(opts[0].value.as_deref(), Some("[10x5,1e-6,10]"));
        assert_eq!(opts[0].key, "--convergence");
        assert_eq!(opts[1].value, None);
        assert_eq!(opts[2].value.as_deref(), Some("-1024"));
        let bad = |a: &[&str]| {
            let v: Vec<String> = a.iter().map(|s| s.to_string()).collect();
            tokenize(&v, &specs).unwrap_err().0
        };
        assert_eq!(bad(&["--foo"]), "unknown option --foo");
        assert_eq!(bad(&["-c"]), "-c needs a value");
        assert_eq!(bad(&["-c", "[", "10"]), "-c [ 10: unbalanced brackets");
        assert_eq!(bad(&["stray"]), "unexpected argument \"stray\"");
        assert_eq!(bad(&["-c", "5", "6"]), "unexpected argument \"6\"");
    }

    #[test]
    fn interpolators() {
        assert_eq!(parse_interpolation("Linear"), Ok(Interp::Linear));
        assert_eq!(
            parse_interpolation("nearestneighbor"),
            Ok(Interp::NearestNeighbor)
        );
        assert_eq!(
            parse_interpolation("LanczosWindowedSinc"),
            Ok(Interp::LanczosWindowedSinc)
        );
        assert_eq!(
            parse_interpolation("GenericLabel[Linear]").unwrap_err().0,
            "interpolation GenericLabel is not supported: use Linear, NearestNeighbor or \
             LanczosWindowedSinc."
        );
        assert_eq!(
            parse_interpolation("BSpline").unwrap_err().0,
            "interpolation BSpline is not supported: use Linear, NearestNeighbor or \
             LanczosWindowedSinc."
        );
        assert_eq!(
            parse_interpolation("Foo").unwrap_err().0,
            "unrecognized interpolation option foo"
        );
    }

    #[test]
    fn the_seed_rule() {
        assert_eq!(resolve_seed(Some(7), Some("9")), Ok(7));
        assert_eq!(resolve_seed(None, Some(" 9 ")), Ok(9));
        assert_eq!(resolve_seed(None, None), Ok(20260910));
        assert_eq!(
            resolve_seed(None, Some("x")).unwrap_err().0,
            "ANTS_RANDOM_SEED=\"x\" is not a non-negative integer"
        );
    }

    #[test]
    fn paths_compare_without_dot_components() {
        assert!(same_path(Path::new("./a/b.nii"), Path::new("a/b.nii")));
        assert!(!same_path(Path::new("a/b.nii"), Path::new("a/c.nii")));
    }
}
