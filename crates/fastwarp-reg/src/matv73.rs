// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! MATLAB v7.3 reader, re-exported from `fastwarp_matfile`. Character variables of files with
//! superblock version 2 or 3 (not written by MATLAB) are read through the crate's own HDF5
//! reader.

use std::path::Path;

pub use fastwarp_matfile::matv73::{decode_utf16, locate_superblock, MatFile, MatRef, MatValue};

use crate::error::{RegError, Result};
use crate::transform::hdf5::H5File;

/// Reads the character variable `name` from the file at `path`.
///
/// Returns `Ok(None)` when the file has no such variable. The value is decoded as UTF-16 up to
/// the first NUL and trailing whitespace is trimmed.
pub fn read_char_variable(path: &Path, name: &str) -> Result<Option<String>> {
    let data = std::fs::read(path).map_err(|source| RegError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    read_char_variable_bytes(&data, name)
}

/// Same as [`read_char_variable`] on the bytes of a file.
pub fn read_char_variable_bytes(data: &[u8], name: &str) -> Result<Option<String>> {
    match locate_superblock(data)? {
        (base, 2 | 3) => Ok(H5File::parse(data[base..].to_vec())?
            .read_u16_member(name)?
            .map(|u| decode_utf16(&u))),
        _ => Ok(fastwarp_matfile::matv73::read_char_variable_bytes(
            data, name,
        )?),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const CONTIGUOUS: &[u8] = include_bytes!("../tests/data/uiprefs_contiguous.mat");
    const CHUNKED: &[u8] = include_bytes!("../tests/data/uiprefs_chunked_shuffle.mat");
    const ITK_H5: &[u8] = include_bytes!("../tests/data/ctest.h5");

    #[test]
    fn version_0_files_are_read_through_fastwarp_matfile() {
        for bytes in [CONTIGUOUS, CHUNKED] {
            assert_eq!(
                read_char_variable_bytes(bytes, "elmodel")
                    .unwrap()
                    .as_deref(),
                Some("Boston Scientific Vercise Directed")
            );
            assert_eq!(read_char_variable_bytes(bytes, "nope").unwrap(), None);
        }
        assert!(read_char_variable_bytes(&CONTIGUOUS[..600], "elmodel").is_err());
    }

    #[test]
    fn a_version_2_or_3_file_is_read_through_the_shared_reader() {
        assert_eq!(read_char_variable_bytes(ITK_H5, "nope").unwrap(), None);
        assert!(read_char_variable_bytes(ITK_H5, "ITKVersion").is_err());
    }

    #[test]
    fn matfile_errors_keep_their_message() {
        let e = read_char_variable_bytes(b"not a mat file", "x").unwrap_err();
        assert!(e.to_string().starts_with("MATLAB v7.3: "), "{e}");
    }
}
