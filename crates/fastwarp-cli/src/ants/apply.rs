// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! `antsApplyTransforms` arguments to a FastWarp resampling or transform-writing plan.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{
    call, check_image_name, dimensionality, interpolation_name, parse_bool, parse_f64,
    parse_interpolation, path_arg, refuse, tokenize, unsupported, version_text, OptSpec, PixelType,
    Refusal, Takes,
};
use crate::args::{parse_transform, TransformArg};
use crate::post::Interp;

const PROGRAM: &str = "antsApplyTransforms";

const fn spec(short: Option<&'static str>, long: &'static str, takes: Takes) -> OptSpec {
    OptSpec { short, long, takes }
}

const SPECS: &[OptSpec] = &[
    spec(None, "--version", Takes::Nothing),
    spec(Some("-h"), "--help", Takes::Nothing),
    spec(Some("-d"), "--dimensionality", Takes::Required),
    spec(Some("-e"), "--input-image-type", Takes::Required),
    spec(None, "--time-index", Takes::Required),
    spec(Some("-i"), "--input", Takes::Required),
    spec(Some("-r"), "--reference-image", Takes::Required),
    spec(Some("-o"), "--output", Takes::Required),
    spec(Some("-n"), "--interpolation", Takes::Required),
    spec(Some("-u"), "--output-data-type", Takes::Required),
    spec(Some("-t"), "--transform", Takes::Required),
    spec(Some("-f"), "--default-value", Takes::Required),
    spec(Some("-z"), "--static-cast-for-R", Takes::Optional),
    spec(None, "--float", Takes::Optional),
    spec(Some("-v"), "--verbose", Takes::Optional),
];

/// What an `antsApplyTransforms` argument list asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum ApplyCall {
    Help(String),
    Version(String),
    Run(Box<ApplyPlan>),
}

/// What `--output` writes.
#[derive(Clone, Debug, PartialEq)]
pub enum ApplyOutput {
    /// `warped.nii.gz`: the input resampled onto the reference grid.
    Image(PathBuf),
    /// `DisplacementField[f.nii.gz]` or `[f.nii.gz,1]`: the transforms as one displacement field
    /// on the reference grid.
    Field(PathBuf),
    /// `[f.h5,1]`: that field in an HDF5 transform file (a composite holding the one field).
    FieldH5(PathBuf),
    /// `CompositeTransform[c.h5]`: the transforms, not collapsed, as one composite.
    Composite(PathBuf),
    /// `Linear[a.mat,invert]`: linear transforms collapsed into one affine (inverted).
    Linear { path: PathBuf, invert: bool },
}

impl ApplyOutput {
    pub fn path(&self) -> &Path {
        match self {
            ApplyOutput::Image(p)
            | ApplyOutput::Field(p)
            | ApplyOutput::FieldH5(p)
            | ApplyOutput::Composite(p)
            | ApplyOutput::Linear { path: p, .. } => p,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            ApplyOutput::Image(_) => "image",
            ApplyOutput::Field(_) => "displacement_field",
            ApplyOutput::FieldH5(_) => "displacement_field_h5",
            ApplyOutput::Composite(_) => "composite",
            ApplyOutput::Linear { .. } => "linear",
        }
    }
}

/// An `antsApplyTransforms` call FastWarp runs.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplyPlan {
    /// `-i`: needed for an image output.
    pub input: Option<PathBuf>,
    /// `-r`: needed for an image or displacement-field output.
    pub reference: Option<PathBuf>,
    pub output: ApplyOutput,
    /// `-t` in command-line order; a reference point goes through the first one first.
    pub transforms: Vec<TransformArg>,
    pub interpolation: Interp,
    pub default_value: f64,
    /// Voxel type of an image output, or of a displacement field (`float` or `double`).
    pub output_type: PixelType,
    pub float: bool,
    pub verbose: bool,
    /// Options accepted but without effect in FastWarp, to print as warnings.
    pub warnings: Vec<String>,
}

impl ApplyPlan {
    /// The plan as JSON.
    pub fn to_json(&self) -> Value {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let mut output = json!({"kind": self.output.kind(), "path": text(self.output.path())});
        if let ApplyOutput::Linear { invert, .. } = &self.output {
            output["invert"] = json!(invert);
        }
        json!({
            "program": PROGRAM,
            "input": self.input.as_deref().map(text),
            "reference": self.reference.as_deref().map(text),
            "output": output,
            "transforms": self.transforms.iter().map(|t| json!({
                "path": text(&t.path), "invert": t.invert,
            })).collect::<Vec<_>>(),
            "interpolation": interpolation_name(self.interpolation),
            "default_value": self.default_value,
            "output_type": self.output_type.name(),
            "float": self.float,
            "verbose": self.verbose,
            "warnings": self.warnings,
        })
    }
}

pub(crate) fn help_text() -> String {
    format!(
        "{}\
Usage: fastwarp ants-apply-transforms [antsApplyTransforms options]

Runs the subset of antsApplyTransforms (ANTs 2.6.5) that FastWarp supports, on the CPU, and
writes what antsApplyTransforms writes. Options that would change the result and are not
supported are refused with exit status 2 before any work. See docs/cli.md.

Supported:
  -d 3   -e 0   -i input.nii.gz   -r reference.nii.gz
  -o warped.nii.gz | DisplacementField[field.nii.gz] | [field.nii.gz,1] | [field.h5,1]
     | CompositeTransform[composite.h5] | Linear[affine.mat,<invert>]
  -t file | [file,useInverse] (repeat; [file,1] inverts linear transforms only)
  -n Linear|NearestNeighbor|LanczosWindowedSinc   -f value
  -u char|uchar|short|int|float|double|default   --float 0|1   -v 0|1
  -z (accepted and ignored)   --version   -h, --help
",
        version_text(PROGRAM)
    )
}

fn parse_output(value: &str, spelled: &str) -> Result<ApplyOutput, Refusal> {
    let what = format!("{spelled} {value}");
    let c = call(value)?;
    let lower = |p: &Path| p.to_string_lossy().to_ascii_lowercase();
    let field = |p: PathBuf| -> Result<ApplyOutput, Refusal> {
        if lower(&p).ends_with(".h5") {
            Ok(ApplyOutput::FieldH5(p))
        } else {
            check_image_name(&p)?;
            Ok(ApplyOutput::Field(p))
        }
    };
    if !c.bracketed {
        let p = path_arg(value, &what)?;
        check_image_name(&p)?;
        return Ok(ApplyOutput::Image(p));
    }
    let flag = |s: Option<&String>| -> Result<bool, Refusal> {
        match s.map(|s| s.to_ascii_lowercase()).as_deref() {
            None | Some("0") | Some("false") => Ok(false),
            Some("1") | Some("true") => Ok(true),
            Some(other) => refuse(format!("{what}: expected 0 or 1, not {other:?}")),
        }
    };
    let name = c.name.to_ascii_lowercase();
    match (name.as_str(), c.args.as_slice()) {
        ("", [path]) | ("", [path, _]) => {
            let p = path_arg(path, &what)?;
            if flag(c.args.get(1))? {
                field(p)
            } else {
                check_image_name(&p)?;
                Ok(ApplyOutput::Image(p))
            }
        }
        ("displacementfield", [path]) => field(path_arg(path, &what)?),
        ("compositetransform", [path]) => {
            let p = path_arg(path, &what)?;
            if !lower(&p).ends_with(".h5") {
                return unsupported(
                    format!("composite output {}", p.display()),
                    "FastWarp writes a composite transform as an .h5 file.",
                );
            }
            Ok(ApplyOutput::Composite(p))
        }
        ("linear", [path]) | ("linear", [path, _]) => {
            let p = path_arg(path, &what)?;
            if !lower(&p).ends_with(".mat") {
                return unsupported(
                    format!("linear output {}", p.display()),
                    "FastWarp writes the collapsed affine as a .mat file.",
                );
            }
            Ok(ApplyOutput::Linear {
                path: p,
                invert: flag(c.args.get(1))?,
            })
        }
        _ => refuse(format!(
            "{what}: expected warped.nii.gz, DisplacementField[field.nii.gz], \
             CompositeTransform[composite.h5], Linear[affine.mat,<invert>] or [field,1]"
        )),
    }
}

fn pixel_type(value: &str, spelled: &str) -> Result<Option<PixelType>, Refusal> {
    Ok(Some(match value.to_ascii_lowercase().as_str() {
        "char" => PixelType::Char,
        "uchar" => PixelType::UChar,
        "short" => PixelType::Short,
        "int" => PixelType::Int,
        "float" => PixelType::Float,
        "double" => PixelType::Double,
        "default" => return Ok(None),
        _ => {
            return refuse(format!(
                "{spelled} {value}: expected char, uchar, short, int, float, double or default"
            ))
        }
    }))
}

/// Parses the arguments of `antsApplyTransforms` (without the program name). Pure: reads no
/// file; image names are checked by their extension only, and `-t [file,1]` of a `.nii` /
/// `.nii.gz` file (a displacement field) is refused here.
///
/// Checked later, when the files are read (`fastwarp ants-apply-transforms` exits 3 for a file
/// that cannot be read, 2 for the rest): that the images exist and are 3-D NIfTI, that each
/// transform file can be read, that an inverted `.h5` / `.mat` holds no displacement field and is
/// invertible, that a `Linear[...]` output gets linear transforms only, that a
/// `CompositeTransform[...]` output gets at least one transform, and that the output folder
/// exists.
pub fn parse_apply<S: AsRef<str>>(args: &[S]) -> Result<ApplyCall, Refusal> {
    let args: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
    let opts = tokenize(&args, SPECS)?;
    if opts.iter().any(|o| o.key == "--help") {
        return Ok(ApplyCall::Help(help_text()));
    }
    if opts.iter().any(|o| o.key == "--version") {
        return Ok(ApplyCall::Version(version_text(PROGRAM)));
    }
    let mut single = std::collections::HashMap::new();
    for o in &opts {
        if o.key == "--transform" {
            continue;
        }
        if single.insert(o.key, o).is_some() {
            return refuse(format!(
                "{} is given more than once; give it once",
                o.spelled
            ));
        }
    }
    let get = |k: &str| single.get(k).copied();
    if let Some(o) = get("--dimensionality") {
        dimensionality(o)?;
    }
    if let Some(o) = get("--input-image-type") {
        let v = o.value()?;
        match v.to_ascii_lowercase().as_str() {
            "0" | "scalar" => {}
            "1" | "2" | "3" | "4" | "5" | "6" | "vector" | "tensor" | "time-series"
            | "multichannel" | "five-dimensional" | "physical-vector" => {
                return unsupported(
                    format!("{} {v}", o.spelled),
                    "FastWarp resamples scalar images only.",
                )
            }
            _ => {
                return refuse(format!(
                    "{}: expected 0 to 6 or scalar, vector, tensor, time-series, multichannel, \
                     five-dimensional, physical-vector, got {v:?}",
                    o.spelled
                ))
            }
        }
    }
    let mut warnings = Vec::new();
    if let Some(o) = get("--time-index") {
        warnings.push(format!(
            "{} is ignored: it applies to time-series input (-e 3) only",
            o.shown()
        ));
    }
    if let Some(o) = get("--static-cast-for-R") {
        warnings.push(format!(
            "{} is ignored: it only changes how ANTs reads transforms in R",
            o.shown()
        ));
    }
    let interpolation = match get("--interpolation") {
        Some(o) => parse_interpolation(o.value()?)?,
        None => Interp::Linear,
    };
    let default_value = match get("--default-value") {
        Some(o) => parse_f64(o.value()?, &o.shown())?,
        None => 0.0,
    };
    let float = match get("--float") {
        Some(o) => parse_bool(o, true)?,
        None => false,
    };
    let verbose = match get("--verbose") {
        Some(o) => parse_bool(o, true)?,
        None => false,
    };
    let explicit_type = match get("--output-data-type") {
        Some(o) => pixel_type(o.value()?, &o.spelled)?,
        None => None,
    };
    let transforms = opts
        .iter()
        .filter(|o| o.key == "--transform")
        .map(|o| {
            let v = o.value()?;
            parse_transform(v).map_err(|e| Refusal::invalid(format!("{} {v}: {e}", o.spelled)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for t in &transforms {
        let name = t.path.to_string_lossy().to_ascii_lowercase();
        if t.invert && (name.ends_with(".nii") || name.ends_with(".nii.gz")) {
            return unsupported(
                format!("inverting the displacement field {}", t.path.display()),
                "pass the matching InverseWarp file (ANTs cannot invert a field either).",
            );
        }
    }
    let Some(out) = get("--output") else {
        return refuse("--output is required");
    };
    let output = parse_output(out.value()?, &out.spelled)?;
    let image_path = |key: &str| -> Result<Option<PathBuf>, Refusal> {
        match get(key) {
            None => Ok(None),
            Some(o) => {
                let p = path_arg(o.value()?, &o.spelled)?;
                check_image_name(&p)?;
                Ok(Some(p))
            }
        }
    };
    let input = image_path("--input")?;
    let reference = image_path("--reference-image")?;
    if matches!(output, ApplyOutput::Image(_)) && input.is_none() {
        return refuse("--input is required to write a warped image");
    }
    if matches!(
        output,
        ApplyOutput::Image(_) | ApplyOutput::Field(_) | ApplyOutput::FieldH5(_)
    ) && reference.is_none()
    {
        return refuse("--reference-image is required for this output");
    }
    let output_type = match (&output, explicit_type) {
        (ApplyOutput::Image(_), Some(t)) => t,
        _ => {
            let image = matches!(output, ApplyOutput::Image(_));
            if let (false, Some(o)) = (image, get("--output-data-type")) {
                warnings.push(format!(
                    "{} is ignored for a transform output: as in ANTs, its type follows --float",
                    o.shown()
                ));
            }
            PixelType::internal(float)
        }
    };
    Ok(ApplyCall::Run(Box::new(ApplyPlan {
        input,
        reference,
        output,
        transforms,
        interpolation,
        default_value,
        output_type,
        float,
        verbose,
        warnings,
    })))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn plan(s: &str) -> ApplyPlan {
        match parse_apply(&words(s)) {
            Ok(ApplyCall::Run(p)) => *p,
            other => panic!("{s}: {other:?}"),
        }
    }

    fn refused(s: &str) -> String {
        match parse_apply(&words(s)) {
            Err(e) => e.0,
            Ok(other) => panic!("{s}: accepted as {other:?}"),
        }
    }

    fn t(path: &str, invert: bool) -> TransformArg {
        TransformArg {
            path: PathBuf::from(path),
            invert,
        }
    }

    #[test]
    fn an_ants_py_call_parses() {
        let p = plan(
            "-d 3 -i m.nii.gz -o w.nii.gz -r f.nii.gz -n linear -t o_1Warp.nii.gz \
             -t [o_0GenericAffine.mat,1] -z 1 --float 1 -f -5 -v 1",
        );
        assert_eq!(p.output, ApplyOutput::Image(PathBuf::from("w.nii.gz")));
        assert_eq!(
            p.transforms,
            vec![t("o_1Warp.nii.gz", false), t("o_0GenericAffine.mat", true)]
        );
        assert_eq!(p.default_value, -5.0);
        assert_eq!(p.output_type, PixelType::Float);
        assert!(p.verbose && p.float);
        assert_eq!(
            p.warnings,
            vec!["-z 1 is ignored: it only changes how ANTs reads transforms in R"]
        );
        let p = plan("-i m.nii -r f.nii -o w.nii -t [ a.mat , 1 ]");
        assert_eq!(p.output_type, PixelType::Double);
        assert_eq!(p.transforms, vec![t("a.mat", true)]);
        assert!(p.warnings.is_empty());
        let p = plan("-i m.nii -r f.nii -o w.nii -u short --float");
        assert_eq!(p.output_type, PixelType::Short);
        assert!(p.transforms.is_empty());
        assert_eq!(
            p.to_json()["output"],
            json!({"kind": "image", "path": "w.nii"})
        );
    }

    #[test]
    fn transform_outputs() {
        let out = |s: &str| plan(s).output;
        assert_eq!(
            out("-r f.nii -o DisplacementField[d.nii.gz] -t a.mat"),
            ApplyOutput::Field(PathBuf::from("d.nii.gz"))
        );
        assert_eq!(
            out("-r f.nii -o [d.nii.gz,1] -t a.mat"),
            ApplyOutput::Field(PathBuf::from("d.nii.gz"))
        );
        assert_eq!(
            out("-r f.nii -o [d.h5,1] -t a.mat"),
            ApplyOutput::FieldH5(PathBuf::from("d.h5"))
        );
        assert_eq!(
            out("-i m.nii -r f.nii -o [w.nii.gz,0] -t a.mat"),
            ApplyOutput::Image(PathBuf::from("w.nii.gz"))
        );
        assert_eq!(
            out("-o CompositeTransform[c.h5] -t a.mat -t b.nii.gz"),
            ApplyOutput::Composite(PathBuf::from("c.h5"))
        );
        assert_eq!(
            out("-o Linear[l.mat,1] -t a.mat"),
            ApplyOutput::Linear {
                path: PathBuf::from("l.mat"),
                invert: true
            }
        );
        assert_eq!(
            plan("-r f.nii -o [d.nii.gz,1] -t a.mat").output_type,
            PixelType::Double
        );
        assert_eq!(
            plan("-r f.nii -o [d.nii.gz,1] -t a.mat --float 1").output_type,
            PixelType::Float
        );
        let p = plan("-r f.nii -o [d.nii.gz,1] -t a.mat -u short -e scalar --time-index 2");
        assert_eq!(p.output_type, PixelType::Double);
        assert_eq!(
            p.warnings,
            vec![
                "--time-index 2 is ignored: it applies to time-series input (-e 3) only",
                "-u short is ignored for a transform output: as in ANTs, its type follows --float",
            ]
        );
        assert_eq!(
            plan("-r f.nii -o [d.nii.gz,1] -t a.mat -u float").output_type,
            PixelType::Double
        );
    }

    #[test]
    fn help_and_version() {
        assert!(matches!(
            parse_apply(&words("--help")),
            Ok(ApplyCall::Help(_))
        ));
        assert!(matches!(
            parse_apply(&words("--version")),
            Ok(ApplyCall::Version(_))
        ));
    }

    #[test]
    fn every_refusal_has_its_message() {
        let base = "-i m.nii -r f.nii -o w.nii -t a.mat";
        let cases: Vec<(String, &str)> = vec![
            (format!("{base} -d 2"), "-d 2 is not supported: FastWarp registers and resamples 3-D images."),
            (format!("{base} -e 3"), "-e 3 is not supported: FastWarp resamples scalar images only."),
            (format!("{base} -e 9"), "-e: expected 0 to 6 or scalar, vector, tensor, time-series, multichannel, five-dimensional, physical-vector, got \"9\""),
            (format!("{base} -e time-series"), "-e time-series is not supported: FastWarp resamples scalar images only."),
            (format!("{base} -n GenericLabel[Linear]"), "interpolation GenericLabel is not supported: use Linear, NearestNeighbor or LanczosWindowedSinc."),
            (format!("{base} -n MultiLabel[0.3,4]"), "interpolation MultiLabel is not supported: use Linear, NearestNeighbor or LanczosWindowedSinc."),
            (format!("{base} -n Gaussian"), "interpolation Gaussian is not supported: use Linear, NearestNeighbor or LanczosWindowedSinc."),
            (format!("{base} -n CosineWindowedSinc"), "interpolation CosineWindowedSinc is not supported: use Linear, NearestNeighbor or LanczosWindowedSinc."),
            (format!("{base} -n Foo"), "unrecognized interpolation option foo"),
            (format!("{base} -u long"), "-u long: expected char, uchar, short, int, float, double or default"),
            (format!("{base} -f abc"), "-f abc: \"abc\" is not a number"),
            (format!("{base} -n Linear -n Linear"), "-n is given more than once; give it once"),
            (format!("{base} --bogus"), "unknown option --bogus"),
            ("-i m.nii -r f.nii -t a.mat".into(), "--output is required"),
            ("-r f.nii -o w.nii -t a.mat".into(), "--input is required to write a warped image"),
            ("-i m.nii -o w.nii -t a.mat".into(), "--reference-image is required for this output"),
            ("-o [d.nii.gz,1] -t a.mat".into(), "--reference-image is required for this output"),
            ("-i m.mha -r f.nii -o w.nii -t a.mat".into(), "image m.mha is not supported: FastWarp reads and writes NIfTI images (.nii, .nii.gz) only."),
            ("-i m.nii -r f.nii -o w.mha -t a.mat".into(), "image w.mha is not supported: FastWarp reads and writes NIfTI images (.nii, .nii.gz) only."),
            ("-o CompositeTransform[c.txt] -t a.mat".into(), "composite output c.txt is not supported: FastWarp writes a composite transform as an .h5 file."),
            ("-o Linear[l.txt] -t a.mat".into(), "linear output l.txt is not supported: FastWarp writes the collapsed affine as a .mat file."),
            ("-o Foo[x.nii] -t a.mat".into(), "-o Foo[x.nii]: expected warped.nii.gz, DisplacementField[field.nii.gz], CompositeTransform[composite.h5], Linear[affine.mat,<invert>] or [field,1]"),
            (format!("{base} -t [w.nii.gz,1]"), "inverting the displacement field w.nii.gz is not supported: pass the matching InverseWarp file (ANTs cannot invert a field either)."),
            (format!("{base} -t [b.mat,2]"), "-t [b.mat,2]: \"[b.mat,2]\": the inverse flag is 0 or 1, not \"2\""),
        ];
        for (args, want) in cases {
            assert_eq!(refused(&args), want, "{args}");
        }
    }
}
