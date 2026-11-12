// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! MATLAB Level-5 MAT files (v6 / v7): writer and a reader for the same subset.
//!
//! Written from the public MAT-File Level 5 format description: a 128-byte header, then one data
//! element per variable, each a `miMATRIX` (optionally wrapped in a zlib `miCOMPRESSED`
//! element). Supported classes: `double` (real), `char` (1×N, UTF-16), `logical`, `cell` and
//! `struct`. The reader also accepts the small data element format and any numeric storage type
//! for `double` data. Every read is bounds-checked; malformed input is an `Err`, never a panic.

use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use flate2::read::ZlibDecoder;
use thiserror::Error;

const MI_INT8: u32 = 1;
const MI_UINT8: u32 = 2;
const MI_INT16: u32 = 3;
const MI_UINT16: u32 = 4;
const MI_INT32: u32 = 5;
const MI_UINT32: u32 = 6;
const MI_SINGLE: u32 = 7;
const MI_DOUBLE: u32 = 9;
const MI_INT64: u32 = 12;
const MI_UINT64: u32 = 13;
const MI_MATRIX: u32 = 14;
const MI_COMPRESSED: u32 = 15;
const MI_UTF8: u32 = 16;
const MI_UTF16: u32 = 17;

const MX_CELL: u8 = 1;
const MX_STRUCT: u8 = 2;
const MX_CHAR: u8 = 4;
const MX_DOUBLE: u8 = 6;
const MX_UINT64_CLASS_MAX: u8 = 15;
const MX_UINT8: u8 = 9;

const FLAG_COMPLEX: u32 = 0x08;
const FLAG_LOGICAL: u32 = 0x02;

const HEADER_LEN: usize = 128;
const MAX_NAME_LEN: usize = 63;
const MIN_FIELD_NAME_LEN: usize = 32;
const MAX_DEPTH: usize = 64;
const MAX_INFLATED: usize = 1 << 28;

/// Errors of the Level-5 MAT writer and reader.
#[derive(Debug, Error)]
pub enum Mat5Error {
    #[error("MAT v5: {0}")]
    Io(#[from] std::io::Error),
    /// The bytes do not follow the Level-5 format.
    #[error("MAT v5: {0}")]
    Format(String),
    /// A valid file that uses a feature outside the supported subset.
    #[error("MAT v5: unsupported: {0}")]
    Unsupported(String),
    /// A value that cannot be written (bad name, dims that do not match the data, …).
    #[error("MAT v5: cannot write: {0}")]
    Invalid(String),
}

pub type Mat5Result<T> = std::result::Result<T, Mat5Error>;

fn fmt_err(m: impl Into<String>) -> Mat5Error {
    Mat5Error::Format(m.into())
}

fn unsupported(m: impl Into<String>) -> Mat5Error {
    Mat5Error::Unsupported(m.into())
}

fn invalid(m: impl Into<String>) -> Mat5Error {
    Mat5Error::Invalid(m.into())
}

/// An owned MATLAB value. Array data is column-major, as MATLAB stores it.
#[derive(Clone, Debug, PartialEq)]
pub enum Mat5Value {
    Double {
        dims: Vec<usize>,
        data: Vec<f64>,
    },
    /// A `1×N` char row (`0×0` when empty).
    Char(String),
    Logical {
        dims: Vec<usize>,
        data: Vec<bool>,
    },
    Cell {
        dims: Vec<usize>,
        items: Vec<Mat5Value>,
    },
    /// `elements[e][f]` = field `fields[f]` of element `e` (column-major element order).
    Struct {
        dims: Vec<usize>,
        fields: Vec<String>,
        elements: Vec<Vec<Mat5Value>>,
    },
    /// A char array that is not a `1×N` row; `rows[i]` is row `i`.
    CharMatrix {
        dims: Vec<usize>,
        rows: Vec<String>,
    },
    /// An array the reader does not decode (sparse, complex, object, function handle, opaque).
    Unsupported {
        class_name: String,
        dims: Vec<usize>,
    },
    /// `[]`, a `0×0 double`.
    Empty,
}

impl Mat5Value {
    pub fn scalar(v: f64) -> Self {
        Mat5Value::Double {
            dims: vec![1, 1],
            data: vec![v],
        }
    }

    pub fn row(values: &[f64]) -> Self {
        Mat5Value::Double {
            dims: vec![1, values.len()],
            data: values.to_vec(),
        }
    }

    /// An `N×3` matrix from row-major points.
    pub fn rows3(points: &[[f64; 3]]) -> Self {
        let n = points.len();
        let mut data = Vec::with_capacity(n * 3);
        for c in 0..3 {
            data.extend(points.iter().map(|p| p[c]));
        }
        Mat5Value::Double {
            dims: vec![n, 3],
            data,
        }
    }

    pub fn logical(v: bool) -> Self {
        Mat5Value::Logical {
            dims: vec![1, 1],
            data: vec![v],
        }
    }

    pub fn char(s: &str) -> Self {
        Mat5Value::Char(s.to_owned())
    }

    /// A `1×N` cell row.
    pub fn cell_row(items: Vec<Mat5Value>) -> Self {
        Mat5Value::Cell {
            dims: vec![1, items.len()],
            items,
        }
    }

    /// A `1×1` struct from `(field, value)` pairs.
    pub fn struct_scalar(fields: Vec<(String, Mat5Value)>) -> Self {
        let (names, values): (Vec<String>, Vec<Mat5Value>) = fields.into_iter().unzip();
        Mat5Value::Struct {
            dims: vec![1, 1],
            fields: names,
            elements: vec![values],
        }
    }

    pub fn dims(&self) -> Vec<usize> {
        match self {
            Mat5Value::Double { dims, .. }
            | Mat5Value::Logical { dims, .. }
            | Mat5Value::Cell { dims, .. }
            | Mat5Value::Struct { dims, .. }
            | Mat5Value::CharMatrix { dims, .. }
            | Mat5Value::Unsupported { dims, .. } => dims.clone(),
            Mat5Value::Char(s) => match s.encode_utf16().count() {
                0 => vec![0, 0],
                n => vec![1, n],
            },
            Mat5Value::Empty => vec![0, 0],
        }
    }

    pub fn is_empty_array(&self) -> bool {
        self.dims().contains(&0)
    }

    /// Field `name` of struct element `index`.
    pub fn field(&self, name: &str, index: usize) -> Option<&Mat5Value> {
        let Mat5Value::Struct {
            fields, elements, ..
        } = self
        else {
            return None;
        };
        let f = fields.iter().position(|n| n == name)?;
        elements.get(index)?.get(f)
    }

    /// Item `index` of a cell array.
    pub fn item(&self, index: usize) -> Option<&Mat5Value> {
        match self {
            Mat5Value::Cell { items, .. } => items.get(index),
            _ => None,
        }
    }

    /// Row-major rows of a 2-D `double` matrix.
    pub fn as_rows(&self) -> Option<Vec<Vec<f64>>> {
        let Mat5Value::Double { dims, data } = self else {
            return None;
        };
        let [r, c] = dims.as_slice() else {
            return None;
        };
        let (r, c) = (*r, *c);
        Some(
            (0..r)
                .map(|i| {
                    (0..c)
                        .filter_map(|j| data.get(j * r + i).copied())
                        .collect()
                })
                .collect(),
        )
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_NAME_LEN
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn checked_count(dims: &[usize]) -> Option<usize> {
    dims.iter().try_fold(1usize, |acc, d| acc.checked_mul(*d))
}

fn padded(n: usize) -> usize {
    n.div_ceil(8) * 8
}

fn to_u32(n: usize, what: &str) -> Mat5Result<u32> {
    u32::try_from(n).map_err(|_| invalid(format!("{what} is larger than 4 GiB")))
}

fn push_element(out: &mut Vec<u8>, ty: u32, data: &[u8]) -> Mat5Result<()> {
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&to_u32(data.len(), "a data element")?.to_le_bytes());
    out.extend_from_slice(data);
    out.resize(out.len() + padded(data.len()) - data.len(), 0);
    Ok(())
}

fn check_dims(dims: &[usize], len: usize, what: &str) -> Mat5Result<()> {
    if dims.len() < 2 {
        return Err(invalid(format!("{what}: needs at least two dimensions")));
    }
    if dims.iter().any(|d| i32::try_from(*d).is_err()) {
        return Err(invalid(format!(
            "{what}: a dimension exceeds the int32 range"
        )));
    }
    if checked_count(dims) != Some(len) {
        return Err(invalid(format!(
            "{what}: dims {dims:?} do not match {len} elements"
        )));
    }
    Ok(())
}

fn encode_matrix(name: &str, value: &Mat5Value, depth: usize, out: &mut Vec<u8>) -> Mat5Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid("values nested too deeply"));
    }
    let mut body = Vec::new();
    let flags = |class: u8, bits: u32| -> Vec<u8> {
        let mut v = (u32::from(class) | (bits << 8)).to_le_bytes().to_vec();
        v.extend_from_slice(&0u32.to_le_bytes());
        v
    };
    let dims_bytes = |dims: &[usize]| -> Vec<u8> {
        dims.iter()
            .flat_map(|d| i32::try_from(*d).unwrap_or(i32::MAX).to_le_bytes())
            .collect()
    };
    let what = if name.is_empty() { "element" } else { name };
    match value {
        Mat5Value::Double { dims, data } => {
            check_dims(dims, data.len(), what)?;
            push_element(&mut body, MI_UINT32, &flags(MX_DOUBLE, 0))?;
            push_element(&mut body, MI_INT32, &dims_bytes(dims))?;
            push_element(&mut body, MI_INT8, name.as_bytes())?;
            let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
            push_element(&mut body, MI_DOUBLE, &bytes)?;
        }
        Mat5Value::Empty => {
            push_element(&mut body, MI_UINT32, &flags(MX_DOUBLE, 0))?;
            push_element(&mut body, MI_INT32, &dims_bytes(&[0, 0]))?;
            push_element(&mut body, MI_INT8, name.as_bytes())?;
            push_element(&mut body, MI_DOUBLE, &[])?;
        }
        Mat5Value::Char(s) => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let dims = if units.is_empty() {
                vec![0, 0]
            } else {
                vec![1, units.len()]
            };
            check_dims(&dims, units.len(), what)?;
            push_element(&mut body, MI_UINT32, &flags(MX_CHAR, 0))?;
            push_element(&mut body, MI_INT32, &dims_bytes(&dims))?;
            push_element(&mut body, MI_INT8, name.as_bytes())?;
            let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
            push_element(&mut body, MI_UINT16, &bytes)?;
        }
        Mat5Value::CharMatrix { dims, rows } => {
            let [r, c] = dims.as_slice() else {
                return Err(invalid(format!(
                    "{what}: a char matrix needs two dimensions"
                )));
            };
            let (r, c) = (*r, *c);
            let row_units: Vec<Vec<u16>> =
                rows.iter().map(|s| s.encode_utf16().collect()).collect();
            if row_units.len() != r || row_units.iter().any(|u| u.len() != c) {
                return Err(invalid(format!("{what}: rows do not match dims {dims:?}")));
            }
            check_dims(dims, r * c, what)?;
            push_element(&mut body, MI_UINT32, &flags(MX_CHAR, 0))?;
            push_element(&mut body, MI_INT32, &dims_bytes(dims))?;
            push_element(&mut body, MI_INT8, name.as_bytes())?;
            let mut bytes = Vec::with_capacity(r * c * 2);
            for j in 0..c {
                for row in &row_units {
                    bytes.extend_from_slice(&row[j].to_le_bytes());
                }
            }
            push_element(&mut body, MI_UINT16, &bytes)?;
        }
        Mat5Value::Unsupported { class_name, .. } => {
            return Err(invalid(format!(
                "{what}: a value of unsupported class {class_name:?} cannot be written"
            )));
        }
        Mat5Value::Logical { dims, data } => {
            check_dims(dims, data.len(), what)?;
            push_element(&mut body, MI_UINT32, &flags(MX_UINT8, FLAG_LOGICAL))?;
            push_element(&mut body, MI_INT32, &dims_bytes(dims))?;
            push_element(&mut body, MI_INT8, name.as_bytes())?;
            let bytes: Vec<u8> = data.iter().map(|b| u8::from(*b)).collect();
            push_element(&mut body, MI_UINT8, &bytes)?;
        }
        Mat5Value::Cell { dims, items } => {
            check_dims(dims, items.len(), what)?;
            push_element(&mut body, MI_UINT32, &flags(MX_CELL, 0))?;
            push_element(&mut body, MI_INT32, &dims_bytes(dims))?;
            push_element(&mut body, MI_INT8, name.as_bytes())?;
            for item in items {
                encode_matrix("", item, depth + 1, &mut body)?;
            }
        }
        Mat5Value::Struct {
            dims,
            fields,
            elements,
        } => {
            check_dims(dims, elements.len(), what)?;
            if let Some(bad) = fields.iter().find(|f| !valid_name(f)) {
                return Err(invalid(format!("{what}: invalid field name {bad:?}")));
            }
            for (i, f) in fields.iter().enumerate() {
                if fields[..i].contains(f) {
                    return Err(invalid(format!("{what}: duplicate field name {f:?}")));
                }
            }
            if let Some(e) = elements.iter().find(|e| e.len() != fields.len()) {
                return Err(invalid(format!(
                    "{what}: an element has {} values for {} fields",
                    e.len(),
                    fields.len()
                )));
            }
            let width = fields
                .iter()
                .map(|f| f.len() + 1)
                .max()
                .unwrap_or(0)
                .max(MIN_FIELD_NAME_LEN);
            push_element(&mut body, MI_UINT32, &flags(MX_STRUCT, 0))?;
            push_element(&mut body, MI_INT32, &dims_bytes(dims))?;
            push_element(&mut body, MI_INT8, name.as_bytes())?;
            let width32 = i32::try_from(width).map_err(|_| invalid("field name width"))?;
            push_element(&mut body, MI_INT32, &width32.to_le_bytes())?;
            let mut names = vec![0u8; width * fields.len()];
            for (i, f) in fields.iter().enumerate() {
                names[i * width..i * width + f.len()].copy_from_slice(f.as_bytes());
            }
            push_element(&mut body, MI_INT8, &names)?;
            for element in elements {
                for v in element {
                    encode_matrix("", v, depth + 1, &mut body)?;
                }
            }
        }
    }
    push_element(out, MI_MATRIX, &body)
}

fn header() -> [u8; HEADER_LEN] {
    let mut h = [b' '; HEADER_LEN];
    let text = b"MATLAB 5.0 MAT-file, Platform: dbsloc, Created by: Wyss DBS Localizer";
    h[..text.len()].copy_from_slice(text);
    h[116..124].fill(0);
    h[124..126].copy_from_slice(&0x0100u16.to_le_bytes());
    h[126..128].copy_from_slice(b"IM");
    h
}

/// The bytes of a Level-5 MAT file holding `vars`, each element zlib-compressed when `compress`.
pub fn mat5_bytes(vars: &[(String, Mat5Value)], compress: bool) -> Mat5Result<Vec<u8>> {
    let mut out = header().to_vec();
    for (i, (name, value)) in vars.iter().enumerate() {
        if !valid_name(name) {
            return Err(invalid(format!("invalid variable name {name:?}")));
        }
        if vars[..i].iter().any(|(n, _)| n == name) {
            return Err(invalid(format!("variable {name:?} given twice")));
        }
        let mut element = Vec::new();
        encode_matrix(name, value, 0, &mut element)?;
        if compress {
            let z = miniz_oxide::deflate::compress_to_vec_zlib(&element, 6);
            out.extend_from_slice(&MI_COMPRESSED.to_le_bytes());
            out.extend_from_slice(&to_u32(z.len(), "a compressed variable")?.to_le_bytes());
            out.extend_from_slice(&z);
        } else {
            out.extend_from_slice(&element);
        }
    }
    Ok(out)
}

/// Writes [`mat5_bytes`] to `w`.
pub fn write_mat5<W: Write>(
    w: &mut W,
    vars: &[(String, Mat5Value)],
    compress: bool,
) -> Mat5Result<()> {
    w.write_all(&mat5_bytes(vars, compress)?)?;
    Ok(())
}

/// Writes [`mat5_bytes`] to `path` (created or truncated).
pub fn write_mat5_file(
    path: &Path,
    vars: &[(String, Mat5Value)],
    compress: bool,
) -> Mat5Result<()> {
    std::fs::write(path, mat5_bytes(vars, compress)?)?;
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn at_end(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, n: usize) -> Mat5Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.bytes.len())
            .ok_or_else(|| fmt_err("element runs past the end of its container"))?;
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> Mat5Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn element(&mut self) -> Mat5Result<(u32, &'a [u8])> {
        let first = self.u32()?;
        if first >> 16 != 0 {
            let n = (first >> 16) as usize;
            if n > 4 {
                return Err(fmt_err(format!("small data element of {n} bytes")));
            }
            let data = self.take(4)?;
            return Ok((first & 0xFFFF, &data[..n]));
        }
        let n = self.u32()? as usize;
        let data = self.take(n)?;
        let pad = padded(n) - n;
        self.pos = self.pos.saturating_add(pad.min(self.remaining()));
        Ok((first, data))
    }
}

fn element_size(ty: u32) -> Option<usize> {
    match ty {
        MI_INT8 | MI_UINT8 => Some(1),
        MI_INT16 | MI_UINT16 => Some(2),
        MI_INT32 | MI_UINT32 | MI_SINGLE => Some(4),
        MI_DOUBLE | MI_INT64 | MI_UINT64 => Some(8),
        _ => None,
    }
}

fn numbers(ty: u32, data: &[u8]) -> Mat5Result<Vec<f64>> {
    let size = element_size(ty).ok_or_else(|| unsupported(format!("numeric data type {ty}")))?;
    if !data.len().is_multiple_of(size) {
        return Err(fmt_err(format!(
            "{} bytes of numeric data are not a multiple of {size}",
            data.len()
        )));
    }
    Ok(data
        .chunks_exact(size)
        .map(|c| {
            let mut b = [0u8; 8];
            b[..size].copy_from_slice(c);
            match ty {
                MI_INT8 => f64::from(c[0] as i8),
                MI_UINT8 => f64::from(c[0]),
                MI_INT16 => f64::from(i16::from_le_bytes([b[0], b[1]])),
                MI_UINT16 => f64::from(u16::from_le_bytes([b[0], b[1]])),
                MI_INT32 => f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                MI_UINT32 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                MI_SINGLE => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                MI_INT64 => i64::from_le_bytes(b) as f64,
                MI_UINT64 => u64::from_le_bytes(b) as f64,
                _ => f64::from_le_bytes(b),
            }
        })
        .collect())
}

fn read_dims(ty: u32, data: &[u8]) -> Mat5Result<Vec<usize>> {
    if ty != MI_INT32 || !data.len().is_multiple_of(4) || data.len() < 8 {
        return Err(fmt_err("dimensions must be at least two int32 values"));
    }
    data.as_chunks::<4>()
        .0
        .iter()
        .map(|c| {
            let d = i32::from_le_bytes(*c);
            usize::try_from(d).map_err(|_| fmt_err(format!("negative dimension {d}")))
        })
        .collect()
}

fn read_text(data: &[u8]) -> Mat5Result<String> {
    let end = data.iter().position(|b| *b == 0).unwrap_or(data.len());
    String::from_utf8(data[..end].to_vec()).map_err(|_| fmt_err("a name is not valid UTF-8"))
}

fn class_label(class: u8, bits: u32) -> String {
    let base = match class {
        MX_CELL => "cell",
        MX_STRUCT => "struct",
        3 => "object",
        MX_CHAR => "char",
        5 => "sparse",
        MX_DOUBLE => "double",
        7 => "single",
        8 => "int8",
        MX_UINT8 => "uint8",
        10 => "int16",
        11 => "uint16",
        12 => "int32",
        13 => "uint32",
        14 => "int64",
        MX_UINT64_CLASS_MAX => "uint64",
        16 => "function_handle",
        17 => "opaque",
        18 => "object",
        other => return format!("class {other}"),
    };
    if bits & FLAG_COMPLEX != 0 {
        format!("{base} (complex)")
    } else {
        base.to_owned()
    }
}

fn char_units(name: &str, ty: u32, data: &[u8], count: usize) -> Mat5Result<Vec<u16>> {
    match ty {
        MI_UINT16 | MI_UTF16 => {
            if data.len() != count.saturating_mul(2) {
                return Err(fmt_err(format!("{name}: char data length")));
            }
            Ok(data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect())
        }
        MI_UINT8 | MI_INT8 | MI_UTF8 => match std::str::from_utf8(data) {
            Ok(text) => Ok(text.encode_utf16().collect()),
            Err(_) if ty != MI_UTF8 => Ok(data.iter().map(|b| u16::from(*b)).collect()),
            Err(_) => Err(fmt_err(format!("{name}: invalid UTF-8"))),
        },
        other => Err(unsupported(format!("{name}: char data type {other}"))),
    }
}

fn parse_matrix(body: &[u8], depth: usize) -> Mat5Result<(String, Mat5Value)> {
    if depth > MAX_DEPTH {
        return Err(fmt_err("values nested too deeply"));
    }
    if body.is_empty() {
        return Ok((String::new(), Mat5Value::Empty));
    }
    let mut cur = Cursor::new(body);
    let (ty, flags) = cur.element()?;
    if ty != MI_UINT32 || flags.len() != 8 {
        return Err(fmt_err("array flags must be two uint32 values"));
    }
    let word = u32::from_le_bytes([flags[0], flags[1], flags[2], flags[3]]);
    let class = (word & 0xFF) as u8;
    let bits = (word >> 8) & 0xFF;
    let complex = bits & FLAG_COMPLEX != 0;
    let decoded = !complex
        && matches!(
            class,
            MX_CELL | MX_STRUCT | MX_CHAR | MX_DOUBLE..=MX_UINT64_CLASS_MAX
        );
    let (ty, dims) = cur.element()?;
    let dims = match read_dims(ty, dims) {
        Ok(d) => d,
        Err(_) if !decoded => Vec::new(),
        Err(e) => return Err(e),
    };
    let name = match cur.element() {
        Ok((_, n)) => read_text(n).unwrap_or_default(),
        Err(_) if !decoded => String::new(),
        Err(e) => return Err(e),
    };
    let opaque = |dims: Vec<usize>| {
        (
            name.clone(),
            Mat5Value::Unsupported {
                class_name: class_label(class, bits),
                dims,
            },
        )
    };
    if !decoded {
        return Ok(opaque(dims));
    }
    let count = checked_count(&dims).ok_or_else(|| fmt_err("dimensions overflow"))?;
    let value = match class {
        MX_UINT8 if bits & FLAG_LOGICAL != 0 => {
            let (ty, data) = cur.element()?;
            if element_size(ty) != Some(1) || data.len() != count {
                return Err(fmt_err(format!("{name}: logical data")));
            }
            Mat5Value::Logical {
                dims,
                data: data.iter().map(|b| *b != 0).collect(),
            }
        }
        MX_DOUBLE..=MX_UINT64_CLASS_MAX => {
            let data = if cur.at_end() && count == 0 {
                Vec::new()
            } else {
                let (ty, data) = cur.element()?;
                match numbers(ty, data) {
                    Ok(v) => v,
                    Err(Mat5Error::Unsupported(_)) => return Ok(opaque(dims)),
                    Err(e) => return Err(e),
                }
            };
            if data.len() != count {
                return Err(fmt_err(format!(
                    "{name}: {} values for dims {dims:?}",
                    data.len()
                )));
            }
            if count == 0 && dims == [0, 0] {
                Mat5Value::Empty
            } else {
                Mat5Value::Double { dims, data }
            }
        }
        MX_CHAR => {
            let units = if cur.at_end() && count == 0 {
                Vec::new()
            } else {
                let (ty, data) = cur.element()?;
                match char_units(&name, ty, data, count) {
                    Ok(u) => u,
                    Err(Mat5Error::Unsupported(_)) => return Ok(opaque(dims)),
                    Err(e) => return Err(e),
                }
            };
            let row = dims.len() == 2 && (dims[0] == 1 || dims == [0, 0]);
            if row {
                Mat5Value::Char(
                    String::from_utf16(&units)
                        .map_err(|_| fmt_err(format!("{name}: invalid UTF-16")))?,
                )
            } else if dims.len() == 2 && units.len() == count {
                let (r, c) = (dims[0], dims[1]);
                let rows = if c == 0 {
                    Vec::new()
                } else {
                    (0..r)
                        .map(|i| {
                            let row: Vec<u16> = (0..c).map(|j| units[j * r + i]).collect();
                            String::from_utf16_lossy(&row)
                        })
                        .collect()
                };
                Mat5Value::CharMatrix { dims, rows }
            } else {
                return Ok(opaque(dims));
            }
        }
        MX_CELL => {
            if count > cur.remaining() / 8 {
                return Err(fmt_err(format!("{name}: {count} cells in too few bytes")));
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                let (ty, sub) = cur.element()?;
                if ty != MI_MATRIX {
                    return Err(fmt_err(format!("{name}: a cell is not a matrix")));
                }
                items.push(parse_matrix(sub, depth + 1)?.1);
            }
            Mat5Value::Cell { dims, items }
        }
        _ => {
            let (ty, width) = cur.element()?;
            let width = match numbers(ty, width)?.as_slice() {
                [w] if *w >= 1.0 && *w <= 4096.0 => *w as usize,
                _ => return Err(fmt_err(format!("{name}: field name length"))),
            };
            let (_, names) = cur.element()?;
            if names.len() % width != 0 {
                return Err(fmt_err(format!("{name}: field names")));
            }
            let fields = names
                .chunks_exact(width)
                .map(read_text)
                .collect::<Mat5Result<Vec<_>>>()?;
            let per_element = fields.len();
            let needed = count
                .checked_mul(per_element)
                .ok_or_else(|| fmt_err("struct size overflows"))?;
            if needed > cur.remaining() / 8 || (per_element == 0 && count > cur.bytes.len()) {
                return Err(fmt_err(format!(
                    "{name}: {count} elements in too few bytes"
                )));
            }
            let mut elements = Vec::with_capacity(count);
            for _ in 0..count {
                let mut values = Vec::with_capacity(per_element);
                for _ in 0..per_element {
                    let (ty, sub) = cur.element()?;
                    if ty != MI_MATRIX {
                        return Err(fmt_err(format!("{name}: a field value is not a matrix")));
                    }
                    values.push(parse_matrix(sub, depth + 1)?.1);
                }
                elements.push(values);
            }
            Mat5Value::Struct {
                dims,
                fields,
                elements,
            }
        }
    };
    Ok((name, value))
}

/// Parses a Level-5 MAT file written by [`mat5_bytes`] (plain or compressed elements).
pub fn read_mat5(bytes: &[u8]) -> Mat5Result<Vec<(String, Mat5Value)>> {
    if bytes.len() < HEADER_LEN {
        return Err(fmt_err("shorter than the 128-byte header"));
    }
    match &bytes[126..128] {
        b"IM" => {}
        b"MI" => return Err(unsupported("big-endian files")),
        _ => return Err(fmt_err("no endian indicator; not a Level-5 MAT file")),
    }
    let version = u16::from_le_bytes([bytes[124], bytes[125]]);
    if version == 0x0200 {
        return Err(unsupported(
            "a v7.3 (HDF5) file; read it with fastwarp_matfile::MatFile",
        ));
    }
    if version != 0x0100 {
        return Err(fmt_err(format!("header version {version:#06x}")));
    }
    let mut cur = Cursor::new(&bytes[HEADER_LEN..]);
    let mut vars = Vec::new();
    while !cur.at_end() {
        let ty = cur.u32()?;
        let n = cur.u32()? as usize;
        let data = cur.take(n)?;
        match ty {
            MI_MATRIX => vars.push(parse_matrix(data, 0)?),
            MI_COMPRESSED => {
                let inflated = crate::inflate::inflate_zlib_limited(data, MAX_INFLATED)
                    .map_err(|e| fmt_err(format!("compressed element: {e}")))?;
                let mut inner = Cursor::new(&inflated);
                let (ty, body) = inner.element()?;
                if ty != MI_MATRIX {
                    return Err(unsupported(format!("compressed element of type {ty}")));
                }
                vars.push(parse_matrix(body, 0)?);
            }
            other => return Err(unsupported(format!("top-level element of type {other}"))),
        }
    }
    Ok(vars)
}

/// Reads and parses `path` with [`read_mat5`].
pub fn read_mat5_file(path: &Path) -> Mat5Result<Vec<(String, Mat5Value)>> {
    read_mat5(&std::fs::read(path)?)
}

const MX_UINT64: u8 = MX_UINT64_CLASS_MAX;
const INDEX_BUF: usize = 512;
const STREAM_BUF: usize = 1 << 16;
const MAX_HEAD_BYTES: usize = 1024;

/// One top-level variable of a Level-5 file, as listed by [`read_mat5_header_index`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarEntry {
    pub name: String,
    /// MATLAB class code: 1 cell, 2 struct, 4 char, 6 double, 7 single, 8 int8, 9 uint8,
    /// 10 int16, 11 uint16, 12 int32, 13 uint32, 14 int64, 15 uint64.
    pub class: u8,
    pub dims: Vec<usize>,
    /// File offset of the element's outer tag.
    pub offset: u64,
    /// Whether the element is a zlib `miCOMPRESSED` wrapper.
    pub compressed: bool,
}

impl VarEntry {
    /// Whether the class is one of the numeric classes (`double` to `uint64`).
    pub fn is_numeric(&self) -> bool {
        (MX_DOUBLE..=MX_UINT64).contains(&self.class)
    }
}

struct ElemTag {
    ty: u32,
    len: usize,
    small: bool,
}

fn read_elem_tag<R: Read>(r: &mut R) -> Mat5Result<ElemTag> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    let first = u32::from_le_bytes(b);
    if first >> 16 != 0 {
        let len = (first >> 16) as usize;
        if len > 4 {
            return Err(fmt_err(format!("small data element of {len} bytes")));
        }
        return Ok(ElemTag {
            ty: first & 0xFFFF,
            len,
            small: true,
        });
    }
    r.read_exact(&mut b)?;
    Ok(ElemTag {
        ty: first,
        len: u32::from_le_bytes(b) as usize,
        small: false,
    })
}

fn read_elem_bytes<R: Read>(r: &mut R, tag: &ElemTag, cap: usize) -> Mat5Result<Vec<u8>> {
    if tag.small {
        let mut b = [0u8; 4];
        r.read_exact(&mut b)?;
        return Ok(b[..tag.len].to_vec());
    }
    if tag.len > cap {
        return Err(fmt_err(format!(
            "header element of {} bytes (limit {cap})",
            tag.len
        )));
    }
    let mut out = vec![0u8; tag.len];
    r.read_exact(&mut out)?;
    let pad = (padded(tag.len) - tag.len) as u64;
    std::io::copy(&mut r.by_ref().take(pad), &mut std::io::sink())?;
    Ok(out)
}

struct MatrixHead {
    class: u8,
    bits: u32,
    dims: Vec<usize>,
    name: String,
}

fn read_matrix_head<R: Read>(r: &mut R) -> Mat5Result<MatrixHead> {
    let tag = read_elem_tag(r)?;
    let flags = read_elem_bytes(r, &tag, 8)?;
    if tag.ty != MI_UINT32 || flags.len() != 8 {
        return Err(fmt_err("array flags must be two uint32 values"));
    }
    let word = u32::from_le_bytes([flags[0], flags[1], flags[2], flags[3]]);
    let tag = read_elem_tag(r)?;
    let dims = read_elem_bytes(r, &tag, MAX_HEAD_BYTES)?;
    let dims = read_dims(tag.ty, &dims)?;
    let tag = read_elem_tag(r)?;
    let name = read_text(&read_elem_bytes(r, &tag, MAX_HEAD_BYTES)?)?;
    Ok(MatrixHead {
        class: (word & 0xFF) as u8,
        bits: (word >> 8) & 0xFF,
        dims,
        name,
    })
}

fn check_file_header(h: &[u8]) -> Mat5Result<()> {
    match &h[126..128] {
        b"IM" => {}
        b"MI" => return Err(unsupported("big-endian files")),
        _ => return Err(fmt_err("no endian indicator; not a Level-5 MAT file")),
    }
    match u16::from_le_bytes([h[124], h[125]]) {
        0x0100 => Ok(()),
        0x0200 => Err(unsupported(
            "a v7.3 (HDF5) file; read it with fastwarp_matfile::MatFile",
        )),
        v => Err(fmt_err(format!("header version {v:#06x}"))),
    }
}

fn open_element<R: Read + Seek>(
    r: &mut R,
    offset: u64,
    buf: usize,
) -> Mat5Result<Box<dyn Read + '_>> {
    r.seek(SeekFrom::Start(offset))?;
    let mut tag = [0u8; 8];
    r.read_exact(&mut tag)?;
    let ty = u32::from_le_bytes([tag[0], tag[1], tag[2], tag[3]]);
    let n = u64::from(u32::from_le_bytes([tag[4], tag[5], tag[6], tag[7]]));
    let payload = BufReader::with_capacity(buf, r.by_ref().take(n));
    match ty {
        MI_MATRIX => Ok(Box::new(payload)),
        MI_COMPRESSED => {
            let mut z = ZlibDecoder::new(payload);
            let mut inner = [0u8; 8];
            z.read_exact(&mut inner)?;
            let ity = u32::from_le_bytes([inner[0], inner[1], inner[2], inner[3]]);
            if ity != MI_MATRIX {
                return Err(unsupported(format!("compressed element of type {ity}")));
            }
            Ok(Box::new(z))
        }
        other => Err(unsupported(format!("top-level element of type {other}"))),
    }
}

/// Lists the top-level variables of a Level-5 file without reading their data.
///
/// The walk seeks from tag to tag. For a compressed element only the leading bytes (array flags,
/// dimensions, name; a few hundred at most) are inflated. A file whose last element is cut off
/// after its header still yields that element; reading its data then fails.
pub fn read_mat5_header_index<R: Read + Seek>(reader: &mut R) -> Mat5Result<Vec<VarEntry>> {
    let len = reader.seek(SeekFrom::End(0))?;
    if len < HEADER_LEN as u64 {
        return Err(fmt_err("shorter than the 128-byte header"));
    }
    reader.seek(SeekFrom::Start(0))?;
    let mut h = [0u8; HEADER_LEN];
    reader.read_exact(&mut h)?;
    check_file_header(&h)?;
    let mut out = Vec::new();
    let mut pos = HEADER_LEN as u64;
    while pos < len {
        if len - pos < 8 {
            return Err(fmt_err("element runs past the end of its container"));
        }
        reader.seek(SeekFrom::Start(pos))?;
        let mut tag = [0u8; 8];
        reader.read_exact(&mut tag)?;
        let ty = u32::from_le_bytes([tag[0], tag[1], tag[2], tag[3]]);
        let n = u64::from(u32::from_le_bytes([tag[4], tag[5], tag[6], tag[7]]));
        let head = {
            let mut body = open_element(reader, pos, INDEX_BUF)?;
            read_matrix_head(&mut body)?
        };
        out.push(VarEntry {
            name: head.name,
            class: head.class,
            dims: head.dims,
            offset: pos,
            compressed: ty == MI_COMPRESSED,
        });
        pos = pos.saturating_add(8).saturating_add(n);
    }
    Ok(out)
}

fn find_entry(index: &[VarEntry], name: &str) -> Mat5Result<VarEntry> {
    index
        .iter()
        .find(|e| e.name == name)
        .cloned()
        .ok_or_else(|| fmt_err(format!("variable `{name}` not found")))
}

/// Reads one variable of a Level-5 file in full (like [`read_mat5`], for that variable only).
///
/// Returns `Ok(None)` when the file has no variable `name`. The inflated size is capped at 256 MiB.
pub fn read_mat5_var<R: Read + Seek>(reader: &mut R, name: &str) -> Mat5Result<Option<Mat5Value>> {
    let index = read_mat5_header_index(reader)?;
    let Some(entry) = index.iter().find(|e| e.name == name) else {
        return Ok(None);
    };
    let mut body = open_element(reader, entry.offset, STREAM_BUF)?;
    let mut bytes = Vec::new();
    body.by_ref()
        .take(MAX_INFLATED as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_INFLATED {
        return Err(fmt_err(format!("`{name}` inflates to more than 256 MiB")));
    }
    Ok(Some(parse_matrix(&bytes, 0)?.1))
}

fn convert_f32(ty: u32, bytes: &[u8], out: &mut Vec<f32>) -> Mat5Result<()> {
    out.clear();
    match ty {
        MI_INT8 => out.extend(bytes.iter().map(|b| f32::from(*b as i8))),
        MI_UINT8 => out.extend(bytes.iter().map(|b| f32::from(*b))),
        MI_INT16 => out.extend(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| f32::from(i16::from_le_bytes(*c))),
        ),
        MI_UINT16 => out.extend(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| f32::from(u16::from_le_bytes(*c))),
        ),
        MI_INT32 => out.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| i32::from_le_bytes(*c) as f32),
        ),
        MI_UINT32 => out.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c) as f32),
        ),
        MI_SINGLE => out.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c)),
        ),
        MI_DOUBLE => out.extend(
            bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| f64::from_le_bytes(*c) as f32),
        ),
        MI_INT64 => out.extend(
            bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| i64::from_le_bytes(*c) as f32),
        ),
        MI_UINT64 => out.extend(
            bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c) as f32),
        ),
        other => return Err(unsupported(format!("numeric data type {other}"))),
    }
    Ok(())
}

/// Streams the numeric variable `name` as `f32` without holding it in memory.
///
/// The element is inflated incrementally and every stored type (`int8` to `uint64`, `single`,
/// `double`) is converted to `f32` on the fly. `visitor(chunk, first_index, dims)` is called in
/// order with at most `chunk_len` elements of the column-major data; `first_index` is the linear
/// (column-major) index of `chunk[0]` and `dims` the MATLAB dimensions. Chunks do not respect
/// column boundaries: an `N x 4` array arrives as `x[0..N], y[0..N], ...` in runs of `chunk_len`
/// elements (use [`stream_columns_f32`] to receive per-column pieces). A truncated file fails
/// after the chunks that were readable. Memory: `chunk_len * (4 + stored size)` bytes plus a
/// 64 KiB input buffer, independent of the array size. Complex arrays and non-numeric classes
/// are [`Mat5Error::Unsupported`].
pub fn stream_mat5_numeric_f32<R, E>(
    reader: &mut R,
    name: &str,
    chunk_len: usize,
    mut visitor: impl FnMut(&[f32], usize, &[usize]) -> Result<(), E>,
) -> Result<(), E>
where
    R: Read + Seek,
    E: From<Mat5Error>,
{
    if chunk_len == 0 {
        return Err(invalid("chunk_len must be positive").into());
    }
    let index = read_mat5_header_index(reader)?;
    let entry = find_entry(&index, name)?;
    if !entry.is_numeric() {
        return Err(unsupported(format!(
            "`{name}` has MATLAB class {} (numeric classes only)",
            entry.class
        ))
        .into());
    }
    let mut body = open_element(reader, entry.offset, STREAM_BUF)?;
    let head = read_matrix_head(&mut body)?;
    if head.bits & FLAG_COMPLEX != 0 {
        return Err(unsupported("complex arrays").into());
    }
    let count = checked_count(&head.dims).ok_or_else(|| fmt_err("dimensions overflow"))?;
    if count == 0 {
        return Ok(());
    }
    let tag = read_elem_tag(&mut body)?;
    let size =
        element_size(tag.ty).ok_or_else(|| unsupported(format!("numeric data type {}", tag.ty)))?;
    if tag.len
        != count
            .checked_mul(size)
            .ok_or_else(|| fmt_err("size overflow"))?
    {
        return Err(fmt_err(format!(
            "{name}: {} data bytes for dims {:?}",
            tag.len, head.dims
        ))
        .into());
    }
    let mut out: Vec<f32> = Vec::new();
    if tag.small {
        let bytes = read_elem_bytes(&mut body, &tag, 4)?;
        let mut done = 0usize;
        while done < count {
            let n = chunk_len.min(count - done);
            convert_f32(tag.ty, &bytes[done * size..(done + n) * size], &mut out)?;
            visitor(&out, done, &head.dims)?;
            done += n;
        }
        return Ok(());
    }
    let mut buf = vec![0u8; chunk_len.min(count) * size];
    let mut done = 0usize;
    while done < count {
        let n = chunk_len.min(count - done);
        body.read_exact(&mut buf[..n * size])
            .map_err(Mat5Error::from)?;
        convert_f32(tag.ty, &buf[..n * size], &mut out)?;
        visitor(&out, done, &head.dims)?;
        done += n;
    }
    Ok(())
}

/// Streams the chosen columns of a 2-D numeric variable as `f32`.
///
/// One inflate pass over the variable (MATLAB stores it column by column, so a row-major view
/// would need one pass per column; callers that want `[x, y, z]` tuples fill an interleaved array
/// by index instead). `visitor(col, chunk, first_index)` receives pieces of at most `chunk_len`
/// elements of column `col`, where `first_index` is the row of `chunk[0]`. Pieces arrive in file
/// order; the pieces of different columns are not interleaved within one chunk but may alternate
/// at column boundaries, so use `first_index` to place them. Memory as for
/// [`stream_mat5_numeric_f32`].
pub fn stream_columns_f32<R, E>(
    reader: &mut R,
    name: &str,
    cols: &[usize],
    chunk_len: usize,
    mut visitor: impl FnMut(usize, &[f32], usize) -> Result<(), E>,
) -> Result<(), E>
where
    R: Read + Seek,
    E: From<Mat5Error>,
{
    let mut checked = false;
    stream_mat5_numeric_f32(reader, name, chunk_len, |chunk, start, dims| {
        let [rows, ncols] = dims else {
            return Err(unsupported(format!("`{name}` is not a 2-D array")).into());
        };
        if !checked {
            if let Some(c) = cols.iter().find(|c| **c >= *ncols) {
                return Err(invalid(format!("column {c} of a {rows} x {ncols} array")).into());
            }
            if cols.iter().enumerate().any(|(i, c)| cols[..i].contains(c)) {
                return Err(invalid("a column was requested twice").into());
            }
            checked = true;
        }
        let end = start + chunk.len();
        for &c in cols {
            let (lo, hi) = (c * rows, (c + 1) * rows);
            let (a, b) = (start.max(lo), end.min(hi));
            if a < b {
                visitor(c, &chunk[a - start..b - start], a - lo)?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn sample() -> Vec<(String, Mat5Value)> {
        let markers = Mat5Value::Struct {
            dims: vec![1, 2],
            fields: vec!["head".into(), "tail".into()],
            elements: vec![
                vec![Mat5Value::Empty, Mat5Value::Empty],
                vec![
                    Mat5Value::row(&[1.0, 2.0, 3.0]),
                    Mat5Value::row(&[4.0, 5.0, 6.5]),
                ],
            ],
        };
        let reco = Mat5Value::struct_scalar(vec![
            (
                "props".into(),
                Mat5Value::Struct {
                    dims: vec![1, 2],
                    fields: vec!["elmodel".into(), "manually_corrected".into()],
                    elements: vec![
                        vec![
                            Mat5Value::char("Boston Scientific Vercise Directed"),
                            Mat5Value::scalar(0.0),
                        ],
                        vec![
                            Mat5Value::char("Boston Scientific Vercise Directed"),
                            Mat5Value::scalar(1.0),
                        ],
                    ],
                },
            ),
            (
                "coords_mm".into(),
                Mat5Value::cell_row(vec![
                    Mat5Value::Empty,
                    Mat5Value::rows3(&[[1.0, 2.0, 3.0], [-4.0, 5.25, 1e-300]]),
                ]),
            ),
            ("markers".into(), markers),
            ("flag".into(), Mat5Value::logical(true)),
            (
                "mask".into(),
                Mat5Value::Logical {
                    dims: vec![2, 2],
                    data: vec![true, false, false, true],
                },
            ),
            ("unicode".into(), Mat5Value::char("µm – ü 😀")),
            ("blank".into(), Mat5Value::char("")),
            ("nothing".into(), Mat5Value::Empty),
            (
                "cube".into(),
                Mat5Value::Double {
                    dims: vec![2, 1, 3],
                    data: vec![1.0, 2.0, 3.0, f64::NAN, f64::INFINITY, -0.0],
                },
            ),
            (
                "no_fields".into(),
                Mat5Value::Struct {
                    dims: vec![1, 1],
                    fields: vec![],
                    elements: vec![vec![]],
                },
            ),
        ]);
        vec![
            ("reco".into(), reco),
            ("x".into(), Mat5Value::scalar(std::f64::consts::PI)),
        ]
    }

    fn same(a: &Mat5Value, b: &Mat5Value) -> bool {
        match (a, b) {
            (
                Mat5Value::Double { dims: da, data: xa },
                Mat5Value::Double { dims: db, data: xb },
            ) => {
                da == db
                    && xa.len() == xb.len()
                    && xa.iter().zip(xb).all(|(p, q)| p.to_bits() == q.to_bits())
            }
            (
                Mat5Value::Cell {
                    dims: da,
                    items: ia,
                },
                Mat5Value::Cell {
                    dims: db,
                    items: ib,
                },
            ) => da == db && ia.len() == ib.len() && ia.iter().zip(ib).all(|(p, q)| same(p, q)),
            (
                Mat5Value::Struct {
                    dims: da,
                    fields: fa,
                    elements: ea,
                },
                Mat5Value::Struct {
                    dims: db,
                    fields: fb,
                    elements: eb,
                },
            ) => {
                da == db
                    && fa == fb
                    && ea.len() == eb.len()
                    && ea.iter().zip(eb).all(|(p, q)| {
                        p.len() == q.len() && p.iter().zip(q).all(|(u, v)| same(u, v))
                    })
            }
            _ => a == b,
        }
    }

    #[test]
    fn mat5_round_trips_every_class_plain_and_compressed() {
        let vars = sample();
        for compress in [false, true] {
            let bytes = mat5_bytes(&vars, compress).unwrap();
            assert_eq!(&bytes[126..128], b"IM");
            assert!(bytes.starts_with(b"MATLAB 5.0 MAT-file"));
            let back = read_mat5(&bytes).unwrap();
            assert_eq!(back.len(), vars.len());
            for ((na, va), (nb, vb)) in vars.iter().zip(&back) {
                assert_eq!(na, nb);
                assert!(same(va, vb), "{na}: {va:?} != {vb:?}");
            }
        }
    }

    #[test]
    fn mat5_plain_elements_are_eight_byte_aligned() {
        let bytes = mat5_bytes(&sample(), false).unwrap();
        assert_eq!(bytes.len() % 8, 0);
    }

    #[test]
    fn mat5_accessors_read_struct_and_cell_values() {
        let vars = sample();
        let reco = &vars[0].1;
        let props = reco.field("props", 0).unwrap();
        assert_eq!(props.dims(), vec![1, 2]);
        assert_eq!(
            props.field("manually_corrected", 1),
            Some(&Mat5Value::scalar(1.0))
        );
        let coords = reco.field("coords_mm", 0).unwrap().item(1).unwrap();
        assert_eq!(
            coords.as_rows().unwrap(),
            vec![vec![1.0, 2.0, 3.0], vec![-4.0, 5.25, 1e-300]]
        );
        assert!(reco
            .field("coords_mm", 0)
            .unwrap()
            .item(0)
            .unwrap()
            .is_empty_array());
    }

    #[test]
    fn mat5_writer_rejects_inconsistent_values() {
        let bad = |v: Mat5Value| mat5_bytes(&[("v".into(), v)], false).unwrap_err();
        assert!(matches!(
            bad(Mat5Value::Double {
                dims: vec![2, 2],
                data: vec![1.0]
            }),
            Mat5Error::Invalid(_)
        ));
        assert!(matches!(
            bad(Mat5Value::Struct {
                dims: vec![1, 1],
                fields: vec!["1bad".into()],
                elements: vec![vec![Mat5Value::Empty]],
            }),
            Mat5Error::Invalid(_)
        ));
        assert!(matches!(
            bad(Mat5Value::Struct {
                dims: vec![1, 2],
                fields: vec!["a".into()],
                elements: vec![vec![Mat5Value::Empty]],
            }),
            Mat5Error::Invalid(_)
        ));
        assert!(mat5_bytes(&[("bad name".into(), Mat5Value::Empty)], false).is_err());
        assert!(mat5_bytes(
            &[
                ("a".into(), Mat5Value::Empty),
                ("a".into(), Mat5Value::Empty)
            ],
            false
        )
        .is_err());
    }

    #[test]
    fn mat5_reader_rejects_other_files_with_clear_errors() {
        assert!(matches!(read_mat5(b"short"), Err(Mat5Error::Format(_))));
        let mut v73 = header().to_vec();
        v73[124..126].copy_from_slice(&0x0200u16.to_le_bytes());
        assert!(read_mat5(&v73).unwrap_err().to_string().contains("v7.3"));
        let mut be = header().to_vec();
        be[126..128].copy_from_slice(b"MI");
        assert!(matches!(read_mat5(&be), Err(Mat5Error::Unsupported(_))));
        let mut int32 = header().to_vec();
        let mut element = Vec::new();
        let mut body = Vec::new();
        push_element(&mut body, MI_UINT32, &[12, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        push_element(&mut body, MI_INT32, &[1, 0, 0, 0, 1, 0, 0, 0]).unwrap();
        push_element(&mut body, MI_INT8, b"v").unwrap();
        push_element(&mut body, MI_INT32, &[7, 0, 0, 0]).unwrap();
        push_element(&mut element, MI_MATRIX, &body).unwrap();
        int32.extend_from_slice(&element);
        assert_eq!(
            read_mat5(&int32).unwrap(),
            vec![("v".to_owned(), Mat5Value::scalar(7.0))]
        );
        assert!(read_mat5(&header()).unwrap().is_empty());
    }

    #[test]
    fn mat5_reader_accepts_small_elements_and_integer_storage() {
        let mut bytes = header().to_vec();
        let mut body = Vec::new();
        push_element(&mut body, MI_UINT32, &[6, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        push_element(&mut body, MI_INT32, &[1, 0, 0, 0, 2, 0, 0, 0]).unwrap();
        body.extend_from_slice(&(MI_INT8 | (1 << 16)).to_le_bytes());
        body.extend_from_slice(b"v\0\0\0");
        body.extend_from_slice(&(MI_UINT8 | (2 << 16)).to_le_bytes());
        body.extend_from_slice(&[3, 250, 0, 0]);
        let mut element = Vec::new();
        push_element(&mut element, MI_MATRIX, &body).unwrap();
        bytes.extend_from_slice(&element);
        let vars = read_mat5(&bytes).unwrap();
        assert_eq!(vars, vec![("v".into(), Mat5Value::row(&[3.0, 250.0]))]);
    }

    fn lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state >> 33
    }

    #[test]
    fn mat5_truncated_and_garbage_input_never_panics() {
        for compress in [false, true] {
            let bytes = mat5_bytes(&sample(), compress).unwrap();
            for cut in 0..bytes.len() {
                let _ = read_mat5(&bytes[..cut]);
            }
            let mut state = 0x5eed_u64 + u64::from(compress);
            for _ in 0..4000 {
                let mut b = bytes.clone();
                for _ in 0..1 + lcg(&mut state) % 6 {
                    let i = HEADER_LEN + (lcg(&mut state) as usize) % (b.len() - HEADER_LEN);
                    b[i] = lcg(&mut state) as u8;
                }
                let _ = read_mat5(&b);
            }
        }
        let mut state = 7u64;
        for len in 0..600 {
            let mut b = header().to_vec();
            b.extend((0..len).map(|_| lcg(&mut state) as u8));
            let _ = read_mat5(&b);
        }
    }

    #[test]
    fn mat5_huge_declared_sizes_are_errors_not_allocations() {
        let mut bytes = header().to_vec();
        let mut body = Vec::new();
        push_element(&mut body, MI_UINT32, &[1, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        let big = i32::MAX.to_le_bytes();
        let mut dims = big.to_vec();
        dims.extend_from_slice(&big);
        push_element(&mut body, MI_INT32, &dims).unwrap();
        push_element(&mut body, MI_INT8, b"c").unwrap();
        let mut element = Vec::new();
        push_element(&mut element, MI_MATRIX, &body).unwrap();
        bytes.extend_from_slice(&element);
        assert!(read_mat5(&bytes).is_err());
    }

    #[test]
    fn an_empty_char_matrix_with_a_huge_row_count_allocates_nothing() {
        let mut bytes = header().to_vec();
        let mut body = Vec::new();
        push_element(&mut body, MI_UINT32, &[MX_CHAR, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        let mut dims = 805_306_368i32.to_le_bytes().to_vec();
        dims.extend_from_slice(&0i32.to_le_bytes());
        push_element(&mut body, MI_INT32, &dims).unwrap();
        push_element(&mut body, MI_INT8, b"c").unwrap();
        push_element(&mut body, MI_UINT16, &[]).unwrap();
        let mut element = Vec::new();
        push_element(&mut element, MI_MATRIX, &body).unwrap();
        bytes.extend_from_slice(&element);
        let vars = read_mat5(&bytes).unwrap();
        assert_eq!(vars.len(), 1);
        match &vars[0].1 {
            Mat5Value::CharMatrix { dims, rows } => {
                assert_eq!(dims, &[805_306_368, 0]);
                assert!(rows.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mat5_file_helpers_round_trip() {
        let dir = std::env::temp_dir().join(format!("fastwarp_mat5_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.mat");
        write_mat5_file(&path, &sample(), true).unwrap();
        let back = read_mat5_file(&path).unwrap();
        assert_eq!(back.len(), 2);
        let mut buf = Vec::new();
        write_mat5(&mut buf, &sample(), true).unwrap();
        assert_eq!(buf, std::fs::read(&path).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn push_small_or(out: &mut Vec<u8>, ty: u32, data: &[u8]) {
        if (1..=4).contains(&data.len()) {
            out.extend_from_slice(&(ty | ((data.len() as u32) << 16)).to_le_bytes());
            out.extend_from_slice(data);
            out.resize(out.len() + 4 - data.len(), 0);
        } else {
            push_element(out, ty, data).unwrap();
        }
    }

    fn numeric_element(name: &str, class: u8, ty: u32, dims: &[usize], data: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut flags = u32::from(class).to_le_bytes().to_vec();
        flags.extend_from_slice(&[0; 4]);
        push_element(&mut body, MI_UINT32, &flags).unwrap();
        let d: Vec<u8> = dims
            .iter()
            .flat_map(|d| (*d as i32).to_le_bytes())
            .collect();
        push_element(&mut body, MI_INT32, &d).unwrap();
        push_small_or(&mut body, MI_INT8, name.as_bytes());
        push_small_or(&mut body, ty, data);
        let mut out = Vec::new();
        push_element(&mut out, MI_MATRIX, &body).unwrap();
        out
    }

    fn file_of(elements: &[Vec<u8>], compress: bool, level: u8) -> Vec<u8> {
        let mut out = header().to_vec();
        for e in elements {
            if compress {
                let z = miniz_oxide::deflate::compress_to_vec_zlib(e, level);
                out.extend_from_slice(&MI_COMPRESSED.to_le_bytes());
                out.extend_from_slice(&(z.len() as u32).to_le_bytes());
                out.extend_from_slice(&z);
            } else {
                out.extend_from_slice(e);
            }
        }
        out
    }

    fn single_bytes(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn fibers_values(n: usize) -> Vec<f32> {
        (0..n * 4)
            .map(|i| ((i * 7919) % 10007) as f32 * 0.25 - 100.0)
            .collect()
    }

    fn collect_all(bytes: &[u8], name: &str, chunk_len: usize) -> (Vec<f32>, Vec<usize>, usize) {
        let mut all = Vec::new();
        let mut dims_seen = Vec::new();
        let mut calls = 0;
        stream_mat5_numeric_f32(
            &mut std::io::Cursor::new(bytes),
            name,
            chunk_len,
            |chunk, first, dims| -> Result<(), Mat5Error> {
                assert_eq!(first, all.len());
                assert!(chunk.len() <= chunk_len);
                all.extend_from_slice(chunk);
                dims_seen = dims.to_vec();
                calls += 1;
                Ok(())
            },
        )
        .unwrap();
        (all, dims_seen, calls)
    }

    #[test]
    fn streamed_single_equals_the_values() {
        let values = fibers_values(1000);
        let el = numeric_element("fibers", 7, MI_SINGLE, &[1000, 4], &single_bytes(&values));
        for compress in [true, false] {
            let bytes = file_of(std::slice::from_ref(&el), compress, 6);
            for chunk_len in [4000, 1000, 333, 7, 1] {
                if chunk_len == 1 && compress {
                    continue;
                }
                let (all, dims, calls) = collect_all(&bytes, "fibers", chunk_len);
                assert_eq!(all, values, "compress {compress} chunk {chunk_len}");
                assert_eq!(dims, vec![1000, 4]);
                assert_eq!(calls, 4000usize.div_ceil(chunk_len));
            }
        }
    }

    #[test]
    fn streamed_double_matches_the_whole_read() {
        let data: Vec<f64> = (0..12).map(|i| f64::from(i) * 1.5 - 4.0).collect();
        let vars = vec![
            ("other".to_owned(), Mat5Value::scalar(2.0)),
            (
                "m".to_owned(),
                Mat5Value::Double {
                    dims: vec![3, 4],
                    data: data.clone(),
                },
            ),
        ];
        let bytes = mat5_bytes(&vars, true).unwrap();
        let (all, dims, _) = collect_all(&bytes, "m", 5);
        let whole = read_mat5(&bytes).unwrap();
        let Mat5Value::Double { data: d, .. } = &whole[1].1 else {
            panic!("not a double")
        };
        assert_eq!(all, d.iter().map(|v| *v as f32).collect::<Vec<_>>());
        assert_eq!(dims, vec![3, 4]);
    }

    #[test]
    fn streamed_columns_are_placed_by_index() {
        let n = 1000;
        let values = fibers_values(n);
        let el = numeric_element("fibers", 7, MI_SINGLE, &[n, 4], &single_bytes(&values));
        let bytes = file_of(&[el], true, 6);
        for chunk_len in [4000, 999, 250, 64] {
            let mut got = vec![vec![f32::NAN; n]; 4];
            let mut order = Vec::new();
            stream_columns_f32(
                &mut std::io::Cursor::new(&bytes),
                "fibers",
                &[3, 0, 2],
                chunk_len,
                |col, chunk, first| -> Result<(), Mat5Error> {
                    got[col][first..first + chunk.len()].copy_from_slice(chunk);
                    order.push(col);
                    Ok(())
                },
            )
            .unwrap();
            for c in [0usize, 2, 3] {
                assert_eq!(
                    got[c],
                    values[c * n..(c + 1) * n],
                    "col {c} chunk {chunk_len}"
                );
            }
            assert!(got[1].iter().all(|v| v.is_nan()));
        }
        let err = stream_columns_f32(
            &mut std::io::Cursor::new(&bytes),
            "fibers",
            &[4],
            100,
            |_, _, _| -> Result<(), Mat5Error> { Ok(()) },
        );
        assert!(err.is_err());
        let err = stream_columns_f32(
            &mut std::io::Cursor::new(&bytes),
            "fibers",
            &[1, 1],
            100,
            |_, _, _| -> Result<(), Mat5Error> { Ok(()) },
        );
        assert!(err.is_err());
    }

    #[test]
    fn integer_classes_and_narrow_storage_convert() {
        let ints: Vec<i16> = vec![-32768, -1, 0, 1, 32767, 300];
        let raw: Vec<u8> = ints.iter().flat_map(|v| v.to_le_bytes()).collect();
        let a = numeric_element("a", 10, MI_INT16, &[2, 3], &raw);
        let doubles = numeric_element("b", 6, MI_UINT8, &[1, 5], &[0, 1, 2, 250, 255]);
        let big: Vec<u8> = [u64::MAX, 0, 1 << 40]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let c = numeric_element("c", 15, MI_UINT64, &[1, 3], &big);
        let neg: Vec<u8> = [-5i8, 127, -128].iter().map(|v| *v as u8).collect();
        let d = numeric_element("d", 8, MI_INT8, &[3, 1], &neg);
        let bytes = file_of(&[a, doubles, c, d], true, 6);
        let (all, dims, _) = collect_all(&bytes, "a", 4);
        assert_eq!(all, vec![-32768.0, -1.0, 0.0, 1.0, 32767.0, 300.0]);
        assert_eq!(dims, vec![2, 3]);
        assert_eq!(
            collect_all(&bytes, "b", 100).0,
            vec![0.0, 1.0, 2.0, 250.0, 255.0]
        );
        assert_eq!(
            collect_all(&bytes, "c", 100).0,
            vec![u64::MAX as f32, 0.0, (1u64 << 40) as f32]
        );
        assert_eq!(collect_all(&bytes, "d", 2).0, vec![-5.0, 127.0, -128.0]);
    }

    #[test]
    fn small_data_elements_stream_and_read() {
        let el = numeric_element("s", 7, MI_SINGLE, &[1, 1], &single_bytes(&[2.5]));
        let bytes = file_of(&[el], true, 6);
        assert_eq!(collect_all(&bytes, "s", 16).0, vec![2.5]);
        let idx = read_mat5_header_index(&mut std::io::Cursor::new(&bytes)).unwrap();
        assert_eq!(idx[0].name, "s");
        assert_eq!(idx[0].dims, vec![1, 1]);
    }

    #[test]
    fn header_index_lists_variables_without_the_data() {
        let values = fibers_values(1000);
        let big = numeric_element("fibers", 7, MI_SINGLE, &[1000, 4], &single_bytes(&values));
        let plain = mat5_bytes(
            &[
                ("first".to_owned(), Mat5Value::row(&[1.0, 2.0])),
                ("txt".to_owned(), Mat5Value::char("hello")),
            ],
            false,
        )
        .unwrap();
        let mut bytes = plain;
        let tail = file_of(std::slice::from_ref(&big), true, 6);
        bytes.extend_from_slice(&tail[HEADER_LEN..]);
        let index = read_mat5_header_index(&mut std::io::Cursor::new(&bytes)).unwrap();
        let names: Vec<_> = index.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["first", "txt", "fibers"]);
        assert_eq!(index[0].dims, vec![1, 2]);
        assert_eq!(index[2].dims, vec![1000, 4]);
        assert_eq!(index.iter().map(|e| e.class).collect::<Vec<_>>(), [6, 4, 7]);
        assert_eq!(
            index.iter().map(|e| e.compressed).collect::<Vec<_>>(),
            [false, false, true]
        );
        assert!(index[0].is_numeric() && !index[1].is_numeric() && index[2].is_numeric());
        let v = read_mat5_var(&mut std::io::Cursor::new(&bytes), "txt")
            .unwrap()
            .unwrap();
        assert_eq!(v, Mat5Value::char("hello"));
        assert!(read_mat5_var(&mut std::io::Cursor::new(&bytes), "nope")
            .unwrap()
            .is_none());
        let err = stream_mat5_numeric_f32(
            &mut std::io::Cursor::new(&bytes),
            "txt",
            10,
            |_, _, _| -> Result<(), Mat5Error> { Ok(()) },
        );
        assert!(matches!(err, Err(Mat5Error::Unsupported(_))));
    }

    #[test]
    fn truncated_data_keeps_the_index_and_fails_the_stream() {
        let values = fibers_values(1000);
        let el = numeric_element("fibers", 7, MI_SINGLE, &[1000, 4], &single_bytes(&values));
        for compress in [true, false] {
            let full = file_of(std::slice::from_ref(&el), compress, 6);
            let cut = &full[..HEADER_LEN + 8 + 600];
            let index = read_mat5_header_index(&mut std::io::Cursor::new(cut)).unwrap();
            assert_eq!(index.len(), 1);
            assert_eq!(index[0].name, "fibers");
            assert_eq!(index[0].dims, vec![1000, 4]);
            let mut seen = 0usize;
            let err = stream_mat5_numeric_f32(
                &mut std::io::Cursor::new(cut),
                "fibers",
                100,
                |chunk, _, _| -> Result<(), Mat5Error> {
                    seen += chunk.len();
                    Ok(())
                },
            );
            assert!(err.is_err(), "compress {compress}");
            assert!(seen < 4000);
            assert!(read_mat5_var(&mut std::io::Cursor::new(cut), "fibers").is_err());
        }
    }

    #[test]
    fn stream_rejects_zero_chunks_missing_names_and_v73() {
        let el = numeric_element("a", 7, MI_SINGLE, &[1, 2], &single_bytes(&[1.0, 2.0]));
        let bytes = file_of(&[el], true, 6);
        let visit = |_: &[f32], _: usize, _: &[usize]| -> Result<(), Mat5Error> { Ok(()) };
        assert!(stream_mat5_numeric_f32(&mut std::io::Cursor::new(&bytes), "a", 0, visit).is_err());
        assert!(stream_mat5_numeric_f32(&mut std::io::Cursor::new(&bytes), "b", 4, visit).is_err());
        let mut v73 = bytes.clone();
        v73[124..126].copy_from_slice(&0x0200u16.to_le_bytes());
        assert!(matches!(
            read_mat5_header_index(&mut std::io::Cursor::new(&v73)),
            Err(Mat5Error::Unsupported(_))
        ));
        assert!(read_mat5_header_index(&mut std::io::Cursor::new(&bytes[..100])).is_err());
    }

    fn struct_element(name: &str, fields: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut flags = u32::from(MX_STRUCT).to_le_bytes().to_vec();
        flags.extend_from_slice(&[0; 4]);
        push_element(&mut body, MI_UINT32, &flags).unwrap();
        push_element(&mut body, MI_INT32, &[1, 0, 0, 0, 1, 0, 0, 0]).unwrap();
        push_small_or(&mut body, MI_INT8, name.as_bytes());
        push_element(&mut body, MI_INT32, &32i32.to_le_bytes()).unwrap();
        let mut names = vec![0u8; 32 * fields.len()];
        for (i, (f, _)) in fields.iter().enumerate() {
            names[i * 32..i * 32 + f.len()].copy_from_slice(f.as_bytes());
        }
        push_element(&mut body, MI_INT8, &names).unwrap();
        for (_, e) in fields {
            body.extend_from_slice(e);
        }
        let mut out = Vec::new();
        push_element(&mut out, MI_MATRIX, &body).unwrap();
        out
    }

    fn i32_bytes(values: &[i32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn char_matrix_element(name: &str, rows: &[&str]) -> Vec<u8> {
        let units: Vec<Vec<u16>> = rows.iter().map(|r| r.encode_utf16().collect()).collect();
        let (r, c) = (rows.len(), units[0].len());
        let mut data = Vec::new();
        for j in 0..c {
            for row in &units {
                data.extend_from_slice(&row[j].to_le_bytes());
            }
        }
        numeric_element(name, MX_CHAR, MI_UINT16, &[r, c], &data)
    }

    fn mixed_file_bytes(compress: bool) -> Vec<u8> {
        let s = struct_element(
            "S",
            &[
                (
                    "count",
                    numeric_element("", 12, MI_INT32, &[1, 3], &i32_bytes(&[1, -2, 3])),
                ),
                (
                    "gain",
                    numeric_element("", 7, MI_SINGLE, &[1, 2], &single_bytes(&[0.5, 2.0])),
                ),
                (
                    "names",
                    char_matrix_element("", &["abcde", "fghij", "klmno"]),
                ),
                (
                    "weights",
                    numeric_element("", 5, MI_INT32, &[3, 3], &i32_bytes(&[0, 1, 2, 3, 4, 5])),
                ),
                (
                    "handle",
                    numeric_element("", 17, MI_UINT8, &[1, 1], &[1, 2, 3, 4, 5, 6, 7, 8, 9]),
                ),
                (
                    "plain",
                    numeric_element("", 6, MI_DOUBLE, &[1, 1], &2.5f64.to_le_bytes()),
                ),
            ],
        );
        let elements = vec![
            numeric_element("n", 13, MI_UINT32, &[1, 1], &9u32.to_le_bytes()),
            s,
            numeric_element("fn", 16, MI_UINT8, &[1, 1], &[0, 0, 0, 0, 0, 0, 0, 0]),
        ];
        file_of(&elements, compress, 6)
    }

    #[test]
    fn mat5_reader_decodes_what_it_can_and_keeps_the_rest_as_unsupported() {
        for compress in [false, true] {
            let vars = read_mat5(&mixed_file_bytes(compress)).unwrap();
            assert_eq!(vars.len(), 3);
            assert_eq!(vars[0], ("n".to_owned(), Mat5Value::scalar(9.0)));
            let s = &vars[1].1;
            assert_eq!(
                s.field("count", 0),
                Some(&Mat5Value::row(&[1.0, -2.0, 3.0]))
            );
            assert_eq!(s.field("gain", 0), Some(&Mat5Value::row(&[0.5, 2.0])));
            assert_eq!(
                s.field("names", 0),
                Some(&Mat5Value::CharMatrix {
                    dims: vec![3, 5],
                    rows: vec!["abcde".into(), "fghij".into(), "klmno".into()],
                })
            );
            assert_eq!(
                s.field("weights", 0),
                Some(&Mat5Value::Unsupported {
                    class_name: "sparse".into(),
                    dims: vec![3, 3],
                })
            );
            assert!(matches!(
                s.field("handle", 0),
                Some(Mat5Value::Unsupported { class_name, .. }) if class_name == "opaque"
            ));
            assert_eq!(s.field("plain", 0), Some(&Mat5Value::scalar(2.5)));
            assert!(matches!(
                &vars[2].1,
                Mat5Value::Unsupported { class_name, .. } if class_name == "function_handle"
            ));
        }
    }

    #[test]
    fn read_any_returns_the_decodable_fields_of_a_v5_file() {
        let dir = std::env::temp_dir().join(format!("fastwarp_mat5_any_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mixed.mat");
        std::fs::write(&path, mixed_file_bytes(true)).unwrap();
        let vars = crate::read_any(&path).unwrap();
        assert_eq!(vars.len(), 3);
        assert_eq!(vars[1].1.field("plain", 0), Some(&Mat5Value::scalar(2.5)));
        assert!(matches!(
            vars[1].1.field("weights", 0),
            Some(Mat5Value::Unsupported { .. })
        ));
        let only = crate::read_any_with(
            &path,
            crate::ReadOptions {
                only: Some("n"),
                skip_fields: &[],
            },
        )
        .unwrap();
        assert_eq!(only.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn char_matrix_round_trips_and_unsupported_is_refused() {
        let m = Mat5Value::CharMatrix {
            dims: vec![2, 3],
            rows: vec!["abc".into(), "dé€".into()],
        };
        let bytes = mat5_bytes(&[("m".into(), m.clone())], true).unwrap();
        assert_eq!(read_mat5(&bytes).unwrap(), vec![("m".to_owned(), m)]);
        let ragged = Mat5Value::CharMatrix {
            dims: vec![2, 3],
            rows: vec!["abc".into(), "d".into()],
        };
        assert!(matches!(
            mat5_bytes(&[("m".into(), ragged)], false),
            Err(Mat5Error::Invalid(_))
        ));
        let un = Mat5Value::Unsupported {
            class_name: "sparse".into(),
            dims: vec![1, 1],
        };
        let err = mat5_bytes(&[("u".into(), un)], false).unwrap_err();
        assert!(err.to_string().contains("unsupported class"));
    }

    fn unpadded_char_element(name: &str, text: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut flags = u32::from(MX_CHAR).to_le_bytes().to_vec();
        flags.extend_from_slice(&[0; 4]);
        push_element(&mut body, MI_UINT32, &flags).unwrap();
        let d = i32_bytes(&[1, text.len() as i32]);
        push_element(&mut body, MI_INT32, &d).unwrap();
        push_small_or(&mut body, MI_INT8, name.as_bytes());
        body.extend_from_slice(&MI_UTF8.to_le_bytes());
        body.extend_from_slice(&(text.len() as u32).to_le_bytes());
        body.extend_from_slice(text);
        assert_ne!(body.len() % 8, 0);
        let mut out = Vec::new();
        out.extend_from_slice(&MI_MATRIX.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn compressed_elements_without_final_padding_are_read() {
        let elements = vec![
            unpadded_char_element("a", b"abc"),
            unpadded_char_element("b", b"hello"),
            numeric_element("c", 6, MI_DOUBLE, &[1, 1], &1.5f64.to_le_bytes()),
        ];
        let bytes = file_of(&elements, true, 6);
        let vars = read_mat5(&bytes).unwrap();
        assert_eq!(vars.len(), 3);
        assert_eq!(vars[0], ("a".to_owned(), Mat5Value::char("abc")));
        assert_eq!(vars[1], ("b".to_owned(), Mat5Value::char("hello")));
        let index = read_mat5_header_index(&mut std::io::Cursor::new(&bytes)).unwrap();
        assert_eq!(index.len(), 3);
        let var = read_mat5_var(&mut std::io::Cursor::new(&bytes), "b")
            .unwrap()
            .unwrap();
        assert_eq!(var, Mat5Value::char("hello"));
    }

    #[test]
    #[ignore = "writes a 320 MB file; run with --ignored --nocapture"]
    fn bench_stream_twenty_million_by_four_single() {
        let n = 20_000_000usize;
        let dir = std::env::temp_dir().join(format!("fastwarp_mat5_bench_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fibers.mat");
        let mut data = Vec::with_capacity(n * 4 * 4);
        for i in 0..n * 4 {
            data.extend_from_slice(&((i % 4099) as f32 * 0.5).to_le_bytes());
        }
        let el = numeric_element("fibers", 7, MI_SINGLE, &[n, 4], &data);
        drop(data);
        std::fs::write(&path, file_of(&[el], true, 1)).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        let start = std::time::Instant::now();
        let mut points = vec![[0f32; 3]; n];
        let mut ids = 0.0f64;
        let mut file = std::fs::File::open(&path).unwrap();
        stream_columns_f32(
            &mut file,
            "fibers",
            &[0, 1, 2, 3],
            1 << 20,
            |col, chunk, first| -> Result<(), Mat5Error> {
                if col < 3 {
                    for (p, v) in points[first..].iter_mut().zip(chunk) {
                        p[col] = *v;
                    }
                } else {
                    ids += chunk.iter().map(|v| f64::from(*v)).sum::<f64>();
                }
                Ok(())
            },
        )
        .unwrap();
        let secs = start.elapsed().as_secs_f64();
        println!(
            "streamed {n} x 4 single ({size} bytes on disk) in {secs:.2} s, {:.0} MB/s inflated, id sum {ids}",
            (n * 16) as f64 / 1e6 / secs
        );
        assert_eq!(points[n - 1][0], ((n - 1) % 4099) as f32 * 0.5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
