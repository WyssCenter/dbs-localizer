// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Reads MATLAB v7.3 `.mat` files (HDF5 behind a 512-byte user block).
//!
//! Written from the public HDF5 file-format specification and the MATLAB v7.3 conventions
//! (column-major arrays, `MATLAB_class`, cells as object references). Supported: superblock versions 0 and 1 (symbol-table groups, version 1 object
//! headers with continuation blocks, local heaps, version 1 group and chunk B-trees), datasets of
//! any rank with fixed-point, IEEE floating-point and object-reference elements in compact,
//! contiguous or chunked (deflate / shuffle / Fletcher32) layouts, and the `MATLAB_class` /
//! `MATLAB_empty` attributes. Every read is bounds-checked; malformed input is an `Err`.
//!
//! [`MatFile::open`] reads through a small page cache, so only the objects that are asked for
//! are read from disk; cells and structs hold [`MatRef`]s that are dereferenced on demand.

mod any;
pub mod error;
mod h5;
mod inflate;
pub mod mat5;
pub mod matv73;
mod source;

pub use any::{read_any, read_any_with, ReadOptions};
pub use error::{MatError, Result};
pub use inflate::inflate_zlib_limited;
pub use mat5::{
    read_mat5, read_mat5_header_index, read_mat5_var, stream_columns_f32, stream_mat5_numeric_f32,
    write_mat5, Mat5Error, Mat5Value, VarEntry,
};
pub use matv73::{MatFile, MatRef, MatValue, NumericInfo};
