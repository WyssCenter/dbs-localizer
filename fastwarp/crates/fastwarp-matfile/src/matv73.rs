// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! MATLAB v7.3 values on top of the HDF5 subset in `h5`.
//!
//! MATLAB conventions: arrays are column-major, so an HDF5 dataset of shape
//! (a, b) is the MATLAB array (b, a); `MATLAB_class` names the class; cells are datasets of
//! object references into `#refs#`; structs are groups with one member per field (a struct array
//! stores one reference per element in each field); empty arrays carry `MATLAB_empty = 1`.

use std::path::Path;

use crate::error::{fmt_err, unsupported, MatError, Result};
use crate::h5::{decode_f64, decode_u64, Dtype, Msg, Space, H5, SIGNATURE};
use crate::source::Source;

const NUMERIC: [&str; 10] = [
    "double", "single", "int8", "uint8", "int16", "uint16", "int32", "uint32", "int64", "uint64",
];

/// Address of an object (dataset or group) inside a [`MatFile`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MatRef(pub(crate) u64);

/// One decoded MATLAB value. Dimensions are in MATLAB order and data is column-major as stored.
#[derive(Clone, Debug, PartialEq)]
pub enum MatValue {
    /// Every numeric class, converted to `f64`.
    Double {
        dims: Vec<usize>,
        data: Vec<f64>,
    },
    Logical {
        dims: Vec<usize>,
        data: Vec<bool>,
    },
    Char(String),
    /// A char array that is not a `1×N` row; `rows[i]` is row `i`.
    CharMatrix {
        dims: Vec<usize>,
        rows: Vec<String>,
    },
    /// Cell array (or one field of a struct array): one reference per element, column-major.
    Cell {
        dims: Vec<usize>,
        items: Vec<MatRef>,
    },
    /// Struct (or plain group): field names in file order with the address of each field.
    Struct {
        fields: Vec<(String, MatRef)>,
    },
    /// An empty array of `class` with its MATLAB dimensions.
    Empty {
        class: String,
        dims: Vec<usize>,
    },
    /// A class this reader does not decode (sparse, function handle, objects, MCOS classes).
    Unsupported {
        class_name: String,
        dims: Vec<usize>,
    },
}

impl MatValue {
    /// The text of a char array; an empty char array is `""`.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            MatValue::Char(s) => Some(s),
            MatValue::Empty { class, .. } if class == "char" => Some(""),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<&[f64]> {
        match self {
            MatValue::Double { data, .. } => Some(data),
            _ => None,
        }
    }

    /// The single element of a 1-element numeric array.
    pub fn scalar(&self) -> Option<f64> {
        match self.as_f64() {
            Some([v]) => Some(*v),
            _ => None,
        }
    }

    pub fn cell_items(&self) -> Option<&[MatRef]> {
        match self {
            MatValue::Cell { items, .. } => Some(items),
            _ => None,
        }
    }

    pub fn field_ref(&self, name: &str) -> Option<MatRef> {
        match self {
            MatValue::Struct { fields } => fields.iter().find(|(n, _)| n == name).map(|f| f.1),
            _ => None,
        }
    }

    pub fn dims(&self) -> Option<&[usize]> {
        match self {
            MatValue::Double { dims, .. }
            | MatValue::Logical { dims, .. }
            | MatValue::Cell { dims, .. }
            | MatValue::Empty { dims, .. }
            | MatValue::CharMatrix { dims, .. }
            | MatValue::Unsupported { dims, .. } => Some(dims),
            _ => None,
        }
    }

    pub fn is_empty_array(&self) -> bool {
        matches!(self, MatValue::Empty { .. })
    }
}

/// Class and MATLAB dimensions of a numeric variable, read from its header only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumericInfo {
    pub class: String,
    pub dims: Vec<usize>,
}

struct NumericHeader {
    msgs: Vec<Msg>,
    space: Space,
    dtype: Dtype,
    class: String,
}

/// A MATLAB v7.3 file opened for random access.
pub struct MatFile {
    h5: H5,
}

impl MatFile {
    /// Opens `path` and reads only the superblock; objects are read when asked for.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            h5: H5::new(Source::open(path)?)?,
        })
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        Ok(Self {
            h5: H5::new(Source::Bytes(data))?,
        })
    }

    /// Bytes read from disk so far (pages of the cache plus large direct reads); the whole
    /// buffer for [`MatFile::from_bytes`].
    pub fn bytes_loaded(&self) -> u64 {
        self.h5.bytes_loaded()
    }

    pub fn root(&self) -> MatRef {
        MatRef(self.h5.root)
    }

    /// Resolves a `/`-separated group path such as `atlases/threshold/type`.
    pub fn resolve(&self, path: &str) -> Result<Option<MatRef>> {
        let mut cur = self.h5.root;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let msgs = self.h5.messages(cur)?;
            if self.h5.symbol_table(&msgs).is_none() {
                return Err(fmt_err(format!(
                    "`{part}` is looked up in a non-group object"
                )));
            }
            match self.h5.lookup(&msgs, part)? {
                Some(next) => cur = next,
                None => return Ok(None),
            }
        }
        Ok(Some(MatRef(cur)))
    }

    pub fn get(&self, path: &str) -> Result<Option<MatValue>> {
        match self.resolve(path)? {
            Some(r) => self.value(r).map(Some),
            None => Ok(None),
        }
    }

    /// Decodes the object at `r`. Cells and structs return references only; nothing they point
    /// to is read.
    pub fn value(&self, r: MatRef) -> Result<MatValue> {
        if self.h5.is_undef(r.0) {
            return Err(fmt_err("undefined object reference"));
        }
        let msgs = self.h5.messages(r.0)?;
        let class = self.attr_text(&msgs, "MATLAB_class")?;
        if self.h5.symbol_table(&msgs).is_some() {
            return match class.as_deref() {
                None | Some("struct") => Ok(MatValue::Struct {
                    fields: self.members_of(&msgs)?,
                }),
                Some(c) => Ok(MatValue::Unsupported {
                    class_name: if self.attr_uint(&msgs, "MATLAB_sparse")?.is_some() {
                        "sparse".to_owned()
                    } else {
                        c.to_owned()
                    },
                    dims: vec![1, 1],
                }),
            };
        }
        let header = match self.h5.dataset_header(&msgs) {
            Err(MatError::Unsupported(_)) => {
                return Ok(MatValue::Unsupported {
                    class_name: class.unwrap_or_else(|| "unknown".to_owned()),
                    dims: vec![1, 1],
                })
            }
            other => other?,
        };
        let (space, dtype) =
            header.ok_or_else(|| fmt_err("object is neither a group nor a dataset"))?;
        let rank = space.dims.len();
        let mut dims: Vec<usize> = space.dims.iter().rev().map(|d| *d as usize).collect();
        while dims.len() < 2 {
            dims.push(1);
        }
        if self
            .attr_uint(&msgs, "MATLAB_empty")?
            .is_some_and(|v| v != 0)
        {
            let raw = self.h5.read_raw(&msgs, &space, dtype)?;
            let dims = decode_u64(dtype, &raw)
                .map(|v| v.into_iter().map(|d| d as usize).collect())
                .unwrap_or_default();
            return Ok(MatValue::Empty {
                class: class.unwrap_or_default(),
                dims,
            });
        }
        let class = class.as_deref();
        match dtype {
            Dtype::Ref { size } if matches!(class, None | Some("cell")) => {
                if size != self.h5.offset_size() {
                    return Err(unsupported(format!("{size}-byte object references")));
                }
                let raw = self.h5.read_raw(&msgs, &space, dtype)?;
                let items = decode_u64(dtype, &raw)?.into_iter().map(MatRef).collect();
                Ok(MatValue::Cell { dims, items })
            }
            Dtype::Int { size: 1 | 2, .. } if class == Some("char") => {
                let raw = self.h5.read_raw(&msgs, &space, dtype)?;
                let units: Vec<u16> = decode_u64(dtype, &raw)?
                    .into_iter()
                    .map(|u| u as u16)
                    .collect();
                let text = |units: &[u16]| -> String {
                    char::decode_utf16(units.iter().copied())
                        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                        .collect()
                };
                let (r, c) = (dims[0], dims[1]);
                let row = rank <= 1 || r == 1 || c == 0;
                if row || dims.len() != 2 || r.checked_mul(c) != Some(units.len()) {
                    return Ok(if row {
                        MatValue::Char(text(&units))
                    } else {
                        MatValue::Unsupported {
                            class_name: "char".to_owned(),
                            dims,
                        }
                    });
                }
                let rows = (0..r)
                    .map(|i| {
                        let row: Vec<u16> = (0..c).map(|j| units[j * r + i]).collect();
                        text(&row)
                    })
                    .collect();
                Ok(MatValue::CharMatrix { dims, rows })
            }
            Dtype::Int { .. } if class == Some("logical") => {
                let raw = self.h5.read_raw(&msgs, &space, dtype)?;
                let data = decode_u64(dtype, &raw)?
                    .into_iter()
                    .map(|v| v != 0)
                    .collect();
                Ok(MatValue::Logical { dims, data })
            }
            Dtype::Int { .. } | Dtype::Float { .. }
                if class.is_none_or(|c| NUMERIC.contains(&c)) =>
            {
                let raw = self.h5.read_raw(&msgs, &space, dtype)?;
                Ok(MatValue::Double {
                    dims,
                    data: decode_f64(dtype, &raw)?,
                })
            }
            _ => Ok(MatValue::Unsupported {
                class_name: class.unwrap_or("unknown").to_owned(),
                dims,
            }),
        }
    }

    fn numeric_header(&self, path: &str) -> Result<Option<NumericHeader>> {
        let Some(r) = self.resolve(path)? else {
            return Ok(None);
        };
        let msgs = self.h5.messages(r.0)?;
        if self.h5.symbol_table(&msgs).is_some() {
            return Err(fmt_err(format!("`{path}` is a group, not a numeric array")));
        }
        let class = self.attr_text(&msgs, "MATLAB_class")?.unwrap_or_default();
        let Some((space, dtype)) = self.h5.dataset_header(&msgs)? else {
            return Err(fmt_err(format!("`{path}` is not a dataset")));
        };
        if !NUMERIC.contains(&class.as_str())
            || !matches!(dtype, Dtype::Int { .. } | Dtype::Float { .. })
        {
            return Err(unsupported(format!(
                "`{path}` has class {class:?} (numeric classes only)"
            )));
        }
        if self
            .attr_uint(&msgs, "MATLAB_empty")?
            .is_some_and(|v| v != 0)
        {
            return Err(fmt_err(format!("`{path}` is an empty array")));
        }
        Ok(Some(NumericHeader {
            msgs,
            space,
            dtype,
            class,
        }))
    }

    /// Class and MATLAB dimensions of the numeric variable at `path`; `None` when it is missing.
    /// Only the object header is read.
    pub fn numeric_info(&self, path: &str) -> Result<Option<NumericInfo>> {
        Ok(self
            .numeric_header(path)?
            .map(|NumericHeader { space, class, .. }| {
                let mut dims: Vec<usize> = space.dims.iter().rev().map(|d| *d as usize).collect();
                while dims.len() < 2 {
                    dims.push(1);
                }
                NumericInfo { class, dims }
            }))
    }

    /// Streams columns `cols` of the 2-D numeric variable at `path` as `f32` in bounded memory.
    ///
    /// HDF5 stores the MATLAB array transposed, so MATLAB column `j` is the `j`-th slowest
    /// hyperslab of the dataset. `visitor(col, chunk, first_index)` receives pieces of at most
    /// `chunk_len` elements of column `col`; `first_index` is the row of `chunk[0]`. Contiguous
    /// datasets are read with one seek per piece; chunked datasets (deflate, shuffle and
    /// Fletcher32 filters) are read chunk by chunk, skipping chunks that hold no requested
    /// column, so pieces of one column arrive in chunk (file) order. Memory: `chunk_len` values
    /// for a contiguous dataset, one chunk (stored + inflated) for a chunked one, plus the chunk
    /// index (about 100 bytes per chunk, at most 2^19 chunks). A dataset with unallocated chunks,
    /// or one that is not 2-D, is [`MatError::Unsupported`]. The `MATLAB_int_decode` attribute is
    /// not interpreted, as in [`MatFile::get`].
    pub fn stream_numeric_f32<E: From<MatError>>(
        &self,
        path: &str,
        cols: &[usize],
        chunk_len: usize,
        mut visitor: impl FnMut(usize, &[f32], usize) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let Some(NumericHeader {
            msgs, space, dtype, ..
        }) = self.numeric_header(path)?
        else {
            return Err(fmt_err(format!("variable `{path}` not found")).into());
        };
        self.h5
            .stream_cols(&msgs, &space, dtype, cols, chunk_len, &mut visitor)
    }

    /// The `MATLAB_class` attribute of the object at `r`, without reading its data.
    pub fn class_name(&self, r: MatRef) -> Result<Option<String>> {
        let msgs = self.h5.messages(r.0)?;
        self.attr_text(&msgs, "MATLAB_class")
    }

    /// The member `name` of the group at `r` (a struct field), without reading it.
    pub fn field(&self, r: MatRef, name: &str) -> Result<Option<MatRef>> {
        let msgs = self.h5.messages(r.0)?;
        if self.h5.symbol_table(&msgs).is_none() {
            return Ok(None);
        }
        Ok(self.h5.lookup(&msgs, name)?.map(MatRef))
    }

    /// Decodes field `name` of a [`MatValue::Struct`]; `None` for other values or a missing
    /// field.
    pub fn struct_field(&self, v: &MatValue, name: &str) -> Result<Option<MatValue>> {
        match v.field_ref(name) {
            Some(r) => self.value(r).map(Some),
            None => Ok(None),
        }
    }

    /// Decodes element `index` (column-major, 0-based) of the cell at `path`; only that element
    /// is dereferenced.
    pub fn cell_element(&self, path: &str, index: usize) -> Result<Option<MatValue>> {
        let Some(cell) = self.get(path)? else {
            return Ok(None);
        };
        match cell.cell_items().and_then(|items| items.get(index)) {
            Some(r) => self.value(*r).map(Some),
            None => Ok(None),
        }
    }

    /// Names of the members of the group at `r`, in file order.
    pub fn members(&self, r: MatRef) -> Result<Vec<String>> {
        let msgs = self.h5.messages(r.0)?;
        Ok(self.members_of(&msgs)?.into_iter().map(|m| m.0).collect())
    }

    fn members_of(&self, msgs: &[Msg]) -> Result<Vec<(String, MatRef)>> {
        let mut out = Vec::new();
        self.h5.group_entries(msgs, &mut |name, addr| {
            out.push((String::from_utf8_lossy(name).into_owned(), MatRef(addr)));
            false
        })?;
        Ok(out)
    }

    fn attr_text(&self, msgs: &[Msg], name: &str) -> Result<Option<String>> {
        Ok(self.h5.attribute(msgs, name)?.and_then(|a| match a.dtype {
            Dtype::Str { .. } => {
                let end = a.data.iter().position(|c| *c == 0).unwrap_or(a.data.len());
                Some(String::from_utf8_lossy(&a.data[..end]).trim().to_owned())
            }
            _ => None,
        }))
    }

    fn attr_uint(&self, msgs: &[Msg], name: &str) -> Result<Option<u64>> {
        Ok(self
            .h5
            .attribute(msgs, name)?
            .and_then(|a| decode_u64(a.dtype, &a.data).ok())
            .and_then(|v| v.first().copied()))
    }

    fn read_char_units(&self, name: &str) -> Result<Option<Vec<u16>>> {
        let msgs = self.h5.messages(self.h5.root)?;
        if self.h5.symbol_table(&msgs).is_none() {
            return Err(fmt_err("root object is not a symbol-table group"));
        }
        let Some(obj) = self.h5.lookup(&msgs, name)? else {
            return Ok(None);
        };
        let msgs = self.h5.messages(obj)?;
        let (space, dtype) = self
            .h5
            .dataset_header(&msgs)?
            .ok_or_else(|| fmt_err("variable is not a dataset"))?;
        if !matches!(dtype, Dtype::Int { size: 1 | 2, .. }) {
            return Err(fmt_err(format!(
                "variable is not a character dataset (datatype class {}, {} byte)",
                dtype.class(),
                dtype.size()
            )));
        }
        let raw = self.h5.read_raw(&msgs, &space, dtype)?;
        Ok(Some(
            decode_u64(dtype, &raw)?
                .into_iter()
                .map(|u| u as u16)
                .collect(),
        ))
    }
}

/// Byte offset of the HDF5 superblock in `data` and its version.
pub fn locate_superblock(data: &[u8]) -> Result<(usize, u8)> {
    let mut at = 0usize;
    while at.checked_add(8).is_some_and(|e| e <= data.len()) {
        if data[at..at + 8] == SIGNATURE {
            return data
                .get(at + 8)
                .map(|v| (at, *v))
                .ok_or_else(|| fmt_err("truncated superblock"));
        }
        at = if at == 0 { 512 } else { at.saturating_mul(2) };
    }
    Err(fmt_err("no HDF5 signature at 0, 512, 1024, ..."))
}

/// Reads the character variable `name` from the file at `path`.
///
/// Returns `Ok(None)` when the file has no such variable. The value is decoded as UTF-16 up to
/// the first NUL and trailing whitespace is trimmed. Superblock versions 2 and 3 are
/// [`crate::MatError::Unsupported`].
pub fn read_char_variable(path: &Path, name: &str) -> Result<Option<String>> {
    let file = MatFile::open(path)?;
    Ok(file.read_char_units(name)?.map(|u| decode_utf16(&u)))
}

/// Same as [`read_char_variable`] on the bytes of a file.
pub fn read_char_variable_bytes(data: &[u8], name: &str) -> Result<Option<String>> {
    let file = MatFile::from_bytes(data.to_vec())?;
    Ok(file.read_char_units(name)?.map(|u| decode_utf16(&u)))
}

/// Decodes UTF-16 code units up to the first NUL and trims trailing whitespace.
pub fn decode_utf16(units: &[u16]) -> String {
    let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
    let s: String = char::decode_utf16(units[..end].iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    s.trim_end().to_owned()
}

#[cfg(test)]
mod tests;
