// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! One entry point for every MAT file: sniffs the header and returns [`Mat5Value`] trees.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use crate::error::{MatError, Result};
use crate::mat5::{read_mat5, Mat5Value};
use crate::matv73::{MatFile, MatRef, MatValue};

const MAX_DEPTH: usize = 64;
const SKIPPED_ROOT_MEMBERS: [&str; 2] = ["#refs#", "#subsystem#"];

/// Which variables [`read_any_with`] decodes.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadOptions<'a> {
    /// Decode only this top-level variable (the result is empty when the file lacks it).
    pub only: Option<&'a str>,
    /// Top-level fields of the decoded variables that are not decoded in a v7.3 file (they
    /// become `[]`); ignored for v5 files, which are parsed whole.
    pub skip_fields: &'a [&'a str],
}

pub(crate) trait Resolver {
    fn value(&self, r: MatRef) -> Result<MatValue>;
    fn class_name(&self, r: MatRef) -> Result<Option<String>>;
}

impl Resolver for MatFile {
    fn value(&self, r: MatRef) -> Result<MatValue> {
        MatFile::value(self, r)
    }

    fn class_name(&self, r: MatRef) -> Result<Option<String>> {
        MatFile::class_name(self, r)
    }
}

fn is_v73(head: &[u8]) -> bool {
    head.starts_with(b"MATLAB 7.3 MAT-file")
        || (head.len() >= 126 && u16::from_le_bytes([head[124], head[125]]) == 0x0200)
}

/// Reads every variable of the MAT file at `path` (v5, v7 or v7.3) into [`Mat5Value`] trees.
///
/// Classes that cannot be decoded become [`Mat5Value::Unsupported`]; they never fail the file.
pub fn read_any(path: &Path) -> Result<Vec<(String, Mat5Value)>> {
    read_any_with(path, ReadOptions::default())
}

/// [`read_any`] with [`ReadOptions`].
pub fn read_any_with(path: &Path, options: ReadOptions<'_>) -> Result<Vec<(String, Mat5Value)>> {
    let io = |source| MatError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut head = [0u8; 128];
    let mut got = 0;
    let mut file = std::fs::File::open(path).map_err(io)?;
    while got < head.len() {
        let n = file.read(&mut head[got..]).map_err(io)?;
        if n == 0 {
            break;
        }
        got += n;
    }
    drop(file);
    if is_v73(&head[..got]) {
        let f = MatFile::open(path)?;
        return read_v73(&f, options);
    }
    let bytes = std::fs::read(path).map_err(io)?;
    let vars = read_mat5(&bytes).map_err(|e| MatError::Format(e.to_string()))?;
    Ok(match options.only {
        Some(name) => vars.into_iter().filter(|(n, _)| n == name).collect(),
        None => vars,
    })
}

fn read_v73(f: &MatFile, options: ReadOptions<'_>) -> Result<Vec<(String, Mat5Value)>> {
    let names: Vec<String> = match options.only {
        Some(name) => {
            if f.resolve(name)?.is_none() {
                return Ok(Vec::new());
            }
            vec![name.to_owned()]
        }
        None => f
            .members(f.root())?
            .into_iter()
            .filter(|n| !SKIPPED_ROOT_MEMBERS.contains(&n.as_str()))
            .collect(),
    };
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let Some(r) = f.resolve(&name)? else {
            continue;
        };
        let mut visiting = HashSet::new();
        let value = convert(f, r, 0, options.skip_fields, &mut visiting, &name)?;
        out.push((name, value));
    }
    Ok(out)
}

fn unsupported(class_name: &str) -> Mat5Value {
    Mat5Value::Unsupported {
        class_name: class_name.to_owned(),
        dims: vec![1, 1],
    }
}

pub(crate) fn convert<R: Resolver>(
    f: &R,
    r: MatRef,
    depth: usize,
    skip: &[&str],
    visiting: &mut HashSet<MatRef>,
    path: &str,
) -> Result<Mat5Value> {
    if depth > MAX_DEPTH {
        return Ok(unsupported("nested too deeply"));
    }
    if !visiting.insert(r) {
        return Ok(unsupported("cyclic reference"));
    }
    let result = convert_inner(f, r, depth, skip, visiting, path);
    visiting.remove(&r);
    result
}

fn convert_inner<R: Resolver>(
    f: &R,
    r: MatRef,
    depth: usize,
    skip: &[&str],
    visiting: &mut HashSet<MatRef>,
    path: &str,
) -> Result<Mat5Value> {
    Ok(match f.value(r)? {
        MatValue::Double { dims, data } => Mat5Value::Double { dims, data },
        MatValue::Logical { dims, data } => Mat5Value::Logical { dims, data },
        MatValue::Char(s) => Mat5Value::Char(s),
        MatValue::CharMatrix { dims, rows } => Mat5Value::CharMatrix { dims, rows },
        MatValue::Unsupported { class_name, dims } => Mat5Value::Unsupported { class_name, dims },
        MatValue::Cell { dims, items } => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                let sub = format!("{path}{{{}}}", i + 1);
                out.push(convert(f, item, depth + 1, &[], visiting, &sub)?);
            }
            Mat5Value::Cell { dims, items: out }
        }
        MatValue::Struct { fields } => convert_struct(f, &fields, depth, skip, visiting, path)?,
        MatValue::Empty { class, dims } => match class.as_str() {
            "char" => Mat5Value::Char(String::new()),
            "cell" => Mat5Value::Cell {
                dims: if dims.len() >= 2 { dims } else { vec![0, 0] },
                items: Vec::new(),
            },
            _ => Mat5Value::Empty,
        },
    })
}

fn convert_struct<R: Resolver>(
    f: &R,
    fields: &[(String, MatRef)],
    depth: usize,
    skip: &[&str],
    visiting: &mut HashSet<MatRef>,
    path: &str,
) -> Result<Mat5Value> {
    let skipped = |name: &str| depth == 0 && skip.contains(&name);
    let mut array_dims: Option<Vec<usize>> = None;
    for (_, fr) in fields {
        if f.class_name(*fr)?.is_none() {
            if let Ok(MatValue::Cell { dims, .. }) = f.value(*fr) {
                array_dims = Some(dims);
                break;
            }
        }
    }
    let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
    let Some(dims) = array_dims else {
        let mut values = Vec::with_capacity(fields.len());
        for (name, fr) in fields {
            if skipped(name) {
                values.push(Mat5Value::Empty);
                continue;
            }
            let sub = format!("{path}.{name}");
            values.push(convert(f, *fr, depth + 1, &[], visiting, &sub)?);
        }
        return Ok(Mat5Value::Struct {
            dims: vec![1, 1],
            fields: names,
            elements: vec![values],
        });
    };
    let count = dims
        .iter()
        .try_fold(1usize, |a, d| a.checked_mul(*d))
        .ok_or_else(|| MatError::Format(format!("{path}: struct dimensions overflow")))?;
    let mut elements: Vec<Vec<Mat5Value>> = vec![Vec::with_capacity(fields.len()); count];
    for (name, fr) in fields {
        if skipped(name) {
            for el in &mut elements {
                el.push(Mat5Value::Empty);
            }
            continue;
        }
        let refs = match f.value(*fr)? {
            MatValue::Cell { items, .. } if items.len() == count => items,
            _ => {
                return Err(MatError::Format(format!(
                    "{path}.{name}: not a struct array field"
                )))
            }
        };
        for (e, item) in refs.into_iter().enumerate() {
            let sub = format!("{path}({}).{name}", e + 1);
            elements[e].push(convert(f, item, depth + 1, &[], visiting, &sub)?);
        }
    }
    Ok(Mat5Value::Struct {
        dims,
        fields: names,
        elements,
    })
}

#[cfg(test)]
mod tests;
