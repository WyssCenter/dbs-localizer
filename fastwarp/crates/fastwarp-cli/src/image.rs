// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Images of the command line: NIfTI-1 (`.nii`, `.nii.gz`) only, read with the geometry ITK (and
//! so ANTsPy) derives from the header, in LPS, and the voxels as `float32` as `ants.image_read`
//! holds them.

use std::path::Path;

use fastwarp_niftiio::{NiftiFile, WriterOptions};
use fastwarp_reg::nifti_io::{header_from_grid, read_grid as nifti_grid};
use fastwarp_reg::{Grid, RegError, Volume};
use ndarray::Array3;

use crate::ants::PixelType;
use crate::{CliError, ExitStatus};

/// Whether `path` names a NIfTI-1 file: `.nii` in any case, or that followed by `.gz` in lower
/// case (the reader and writer decide on gzip by the lower-case `.gz`).
pub fn is_nifti_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = name.strip_suffix(".gz").unwrap_or(&name);
    base.to_ascii_lowercase().ends_with(".nii")
}

/// Refuses a file name that is not `.nii` / `.nii.gz`; `role` names the argument.
pub fn check_nifti_name(path: &Path, role: &str) -> Result<(), CliError> {
    if is_nifti_name(path) {
        Ok(())
    } else {
        Err(CliError::usage(format!(
            "{role} {}: only NIfTI images (.nii, .nii.gz) are supported",
            path.display()
        )))
    }
}

fn read_err(path: &Path, e: RegError) -> CliError {
    CliError::new(
        ExitStatus::Io,
        format!("cannot read {}: {}", path.display(), strip_path(path, &e)),
    )
}

fn strip_path(path: &Path, e: &RegError) -> String {
    let text = e.to_string();
    let shown = path.display().to_string();
    for prefix in [format!("NIfTI {shown}: "), format!("{shown}: ")] {
        if let Some(rest) = text.strip_prefix(&prefix) {
            return rest.to_string();
        }
    }
    text
}

fn check_input(path: &Path, role: &str) -> Result<(), CliError> {
    check_nifti_name(path, role)?;
    if !path.is_file() {
        return Err(CliError::new(
            ExitStatus::Io,
            format!("{role} {}: no such file", path.display()),
        ));
    }
    let file = NiftiFile::open(path).map_err(|e| {
        CliError::new(
            ExitStatus::Io,
            format!("cannot read {}: {e}", path.display()),
        )
    })?;
    let dim = file.header().dim;
    let rank = usize::from(dim[0].min(7));
    let volumes: u64 = (4..=rank).map(|a| u64::from(dim[a].max(1))).product();
    if rank < 3 || volumes > 1 {
        return Err(CliError::usage(format!(
            "{role} {}: expected a 3-D image, the header has dim {:?}",
            path.display(),
            &dim[..=rank.max(1)]
        )));
    }
    Ok(())
}

/// Reads a 3-D NIfTI image; `role` names the argument in messages.
pub fn load(path: &Path, role: &str) -> Result<Volume<f64>, CliError> {
    check_input(path, role)?;
    Volume::<f64>::load_nifti_f64(path).map_err(|e| read_err(path, e))
}

/// Reads only the grid of a 3-D NIfTI image.
pub fn grid(path: &Path, role: &str) -> Result<Grid, CliError> {
    check_input(path, role)?;
    nifti_grid(path).map_err(|e| read_err(path, e))
}

/// Creates the folder of an output file.
pub fn create_parent(path: &Path) -> Result<(), CliError> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => std::fs::create_dir_all(dir).map_err(|e| {
            CliError::new(
                ExitStatus::Io,
                format!("cannot create the folder {}: {e}", dir.display()),
            )
        }),
        _ => Ok(()),
    }
}

/// Writes `data` (x fastest) on `grid` as a `float32` NIfTI file with sform and qform set.
pub fn save(path: &Path, grid: Grid, data: Vec<f32>) -> Result<(), CliError> {
    let write_err = |e: RegError| {
        CliError::new(
            ExitStatus::Io,
            format!("cannot write {}: {}", path.display(), strip_path(path, &e)),
        )
    };
    Volume::from_vec(grid, data)
        .map_err(write_err)?
        .save_nifti(path)
        .map_err(write_err)
}

/// Writes `data` (x fastest) on `grid` as a NIfTI file of voxel type `pixel`, with sform and
/// qform set and the header fields ANTs sets (`pixdim[4..8]` 0, units mm and s). Integer types take the value truncated toward zero and then wrapped to the type's
/// width, as ITK's static cast in `antsApplyTransforms -u` does (`uchar` -2.7 gives 254).
pub fn save_as(path: &Path, grid: Grid, data: &[f32], pixel: PixelType) -> Result<(), CliError> {
    if data.len() != grid.num_voxels() {
        return Err(CliError::new(
            ExitStatus::Registration,
            format!(
                "cannot write {}: {} values for {} voxels",
                path.display(),
                data.len(),
                grid.num_voxels()
            ),
        ));
    }
    let [nx, ny, nz] = grid.size();
    let mut hdr = header_from_grid(&grid);
    hdr.pixdim[4..].fill(0.0);
    hdr.xyzt_units = 10;
    let opts = WriterOptions::new(path).reference_header(&hdr);
    macro_rules! write_as {
        ($t:ty, $cast:expr) => {
            opts.write_nifti(&Array3::<$t>::from_shape_fn((nx, ny, nz), |(i, j, k)| {
                $cast(data[grid.linear_index(i, j, k)])
            }))
        };
    }
    match pixel {
        PixelType::Char => write_as!(i8, |v: f32| v as i64 as i8),
        PixelType::UChar => write_as!(u8, |v: f32| v as i64 as u8),
        PixelType::Short => write_as!(i16, |v: f32| v as i64 as i16),
        PixelType::Int => write_as!(i32, |v: f32| v as i64 as i32),
        PixelType::Double => write_as!(f64, f64::from),
        PixelType::Float => write_as!(f32, |v: f32| v),
    }
    .map_err(|e| {
        CliError::new(
            ExitStatus::Io,
            format!("cannot write {}: {e}", path.display()),
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn nifti_names() {
        for ok in ["a.nii", "a.nii.gz", "dir.x/A.NII.gz", "b.Nii"] {
            assert!(is_nifti_name(Path::new(ok)), "{ok}");
        }
        for bad in [
            "A.NII.GZ",
            "a.mha",
            "a.nrrd",
            "a.nii.bz2",
            "a.img",
            "a.hdr",
            "nii",
            "a.gz",
        ] {
            assert!(!is_nifti_name(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn another_format_is_a_usage_error() {
        let e = load(Path::new("brain.mha"), "fixed image").unwrap_err();
        assert_eq!(e.status, ExitStatus::Usage);
        assert!(e.message.contains("only NIfTI"), "{}", e.message);
    }

    #[test]
    fn integer_outputs_wrap_as_ants_writes_them() {
        let values = [
            2.7f32, -2.7, 300.4, -300.6, 70000.6, -70000.2, 127.5, 128.5, 255.9, 0.49,
        ];
        let grid = Grid::new(
            [10, 1, 1],
            [1.0; 3],
            [0.0; 3],
            fastwarp_reg::linalg::IDENTITY3,
        )
        .unwrap();
        let dir = std::env::temp_dir().join(format!("fastwarp_cli_cast_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ants: [(PixelType, [f64; 10]); 4] = [
            (
                PixelType::Char,
                [2., -2., 44., -44., 112., -112., 127., -128., -1., 0.],
            ),
            (
                PixelType::UChar,
                [2., 254., 44., 212., 112., 144., 127., 128., 255., 0.],
            ),
            (
                PixelType::Short,
                [2., -2., 300., -300., 4464., -4464., 127., 128., 255., 0.],
            ),
            (
                PixelType::Int,
                [2., -2., 300., -300., 70000., -70000., 127., 128., 255., 0.],
            ),
        ];
        for (pixel, want) in ants {
            let path = dir.join(format!("{}.nii", pixel.name()));
            save_as(&path, grid, &values, pixel).unwrap();
            let back = Volume::<f64>::load_nifti_f64(&path).unwrap();
            assert_eq!(back.data(), want.as_slice(), "{}", pixel.name());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let e = load(
            &PathBuf::from("no_such_fastwarp_file.nii.gz"),
            "moving image",
        )
        .unwrap_err();
        assert_eq!(e.status, ExitStatus::Io);
        assert!(e.message.contains("no such file"), "{}", e.message);
    }
}
