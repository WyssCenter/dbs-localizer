// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

mod convert;
mod error;
mod gzip;
mod reader;
mod writer;

pub use convert::{decode_f32, decode_map, decode_map_c_order, scalar_size};
pub use error::NiftiFileError;
pub use gzip::{gzip_parallel, ParGzWriter, BLOCK_SIZE};
pub use reader::NiftiFile;
pub use writer::WriterOptions;
