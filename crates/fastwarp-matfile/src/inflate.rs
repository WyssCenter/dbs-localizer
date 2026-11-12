// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::io::Read;

pub fn inflate_zlib_limited(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let hint = limit.min(data.len().saturating_mul(4));
    let mut out = Vec::with_capacity(hint);
    let cap = (limit as u64).saturating_add(1);
    flate2::read::ZlibDecoder::new(data)
        .take(cap)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() > limit {
        return Err(format!("inflated data exceeds {limit} bytes"));
    }
    Ok(out)
}
