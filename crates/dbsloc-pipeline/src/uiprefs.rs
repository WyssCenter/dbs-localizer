// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Electrode model (`elmodel`) and post-op modality choice (`modality`) from a subject's
//! `uiprefs.mat`.

use std::path::Path;

use crate::bids::{PostopModality, Subject};

/// Reads `elmodel` from the file at `path`; `Ok(None)` when the variable is absent or empty.
pub fn electrode_model_from_uiprefs(path: &Path) -> Result<Option<String>, String> {
    fastwarp_reg::matv73::read_char_variable(path, "elmodel")
        .map(|v| v.filter(|s| !s.is_empty()))
        .map_err(|e| e.to_string())
}

/// Reads the stored post-op modality choice `modality` (1 = MR, 2 = CT, 3 = none;
/// `specs/13` §6.2) from a MATLAB v7.3 or v5 file. `Ok(None)` when the variable is absent, empty,
/// 3 or any other value.
pub fn postop_modality_from_uiprefs(path: &Path) -> Result<Option<PostopModality>, String> {
    let value = fastwarp_matfile::read_any_with(
        path,
        fastwarp_matfile::ReadOptions {
            only: Some("modality"),
            skip_fields: &[],
        },
    )
    .map_err(|e| e.to_string())?
    .into_iter()
    .next()
    .and_then(|(_, v)| match v {
        fastwarp_matfile::Mat5Value::Double { data, .. } if data.len() == 1 => Some(data[0]),
        _ => None,
    });
    Ok(match value {
        Some(1.0) => Some(PostopModality::Mri),
        Some(2.0) => Some(PostopModality::Ct),
        _ => None,
    })
}

pub fn subject_electrode_model(sub: &Subject) -> Result<Option<String>, String> {
    let path = sub.uiprefs_path();
    if !path.is_file() {
        return Ok(None);
    }
    electrode_model_from_uiprefs(&path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fastwarp/crates/fastwarp-matfile/tests/data")
            .join(name)
    }

    #[test]
    fn fixtures_yield_the_model() {
        for name in ["uiprefs_contiguous.mat", "uiprefs_chunked.mat"] {
            assert_eq!(
                electrode_model_from_uiprefs(&fixture(name))
                    .unwrap()
                    .as_deref(),
                Some("Boston Scientific Vercise Directed"),
                "{name}"
            );
        }
    }

    #[test]
    fn the_stored_post_op_modality_is_read_from_v5_and_absent_from_the_v73_fixtures() {
        use fastwarp_matfile::mat5::{write_mat5_file, Mat5Value};
        for name in ["uiprefs_contiguous.mat", "uiprefs_chunked.mat"] {
            assert_eq!(postop_modality_from_uiprefs(&fixture(name)).unwrap(), None);
        }
        let dir = tempfile::tempdir().unwrap();
        for (v, want) in [
            (1.0, Some(PostopModality::Mri)),
            (2.0, Some(PostopModality::Ct)),
            (3.0, None),
        ] {
            let p = dir.path().join(format!("m{v}.mat"));
            write_mat5_file(
                &p,
                &[
                    ("elmodel".to_owned(), Mat5Value::char("Medtronic 3389")),
                    ("modality".to_owned(), Mat5Value::scalar(v)),
                ],
                false,
            )
            .unwrap();
            assert_eq!(postop_modality_from_uiprefs(&p).unwrap(), want, "{v}");
        }
        let p = dir.path().join("none.mat");
        write_mat5_file(&p, &[("elmodel".to_owned(), Mat5Value::char("x"))], true).unwrap();
        assert_eq!(postop_modality_from_uiprefs(&p).unwrap(), None);
        let bad = dir.path().join("bad.mat");
        std::fs::write(&bad, b"not a mat file").unwrap();
        assert!(postop_modality_from_uiprefs(&bad).is_err());
    }

    #[test]
    fn a_missing_file_is_an_error_and_a_corrupt_one_too() {
        assert!(electrode_model_from_uiprefs(&fixture("absent.mat")).is_err());
        let dir = std::env::temp_dir().join("dbsloc_uiprefs_corrupt_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.mat");
        std::fs::write(&p, b"not a mat file").unwrap();
        assert!(electrode_model_from_uiprefs(&p).is_err());
    }
}
