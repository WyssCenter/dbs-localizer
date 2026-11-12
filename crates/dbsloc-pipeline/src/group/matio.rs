// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! MAT reading for group files: v5 / v7 / v7.3 into one value tree, plus a lossless JSON encoding.

use std::path::Path;

use fastwarp_matfile::mat5::Mat5Value;
use fastwarp_matfile::{read_any_with, ReadOptions};
use serde_json::{json, Map, Value};

use super::GroupError;

/// A variable decoded from a MAT file and the paths of values that could not be decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub value: Mat5Value,
    pub dropped: Vec<String>,
}

/// Reads variable `name` of the MAT file at `path`; `None` when the file has no such variable.
/// In a v7.3 file the top-level fields listed in `skip` are not decoded (they become `[]`).
/// Values of classes that cannot be decoded are `Unsupported` and listed in `dropped`.
pub fn read_variable(
    path: &Path,
    name: &str,
    skip: &[&str],
) -> Result<Option<Decoded>, GroupError> {
    let vars = read_any_with(
        path,
        ReadOptions {
            only: Some(name),
            skip_fields: skip,
        },
    )
    .map_err(|e| match e {
        fastwarp_matfile::MatError::Io { .. } => GroupError::Io(format!("cannot read {e}")),
        other => GroupError::Mat(format!("{}: {other}", path.display())),
    })?;
    Ok(vars.into_iter().next().map(|(_, value)| {
        let mut dropped = Vec::new();
        collect_dropped(&value, name, &mut dropped);
        Decoded { value, dropped }
    }))
}

/// Appends the path of every `Unsupported` value in `v` (`M.a.b`, `M.c{2}`, `M.s(3).f`).
pub fn collect_dropped(v: &Mat5Value, path: &str, out: &mut Vec<String>) {
    match v {
        Mat5Value::Unsupported { .. } => out.push(path.to_owned()),
        Mat5Value::Cell { items, .. } => {
            for (i, item) in items.iter().enumerate() {
                collect_dropped(item, &format!("{path}{{{}}}", i + 1), out);
            }
        }
        Mat5Value::Struct {
            fields, elements, ..
        } => {
            for (e, element) in elements.iter().enumerate() {
                for (name, value) in fields.iter().zip(element) {
                    let sub = if elements.len() == 1 {
                        format!("{path}.{name}")
                    } else {
                        format!("{path}({}).{name}", e + 1)
                    };
                    collect_dropped(value, &sub, out);
                }
            }
        }
        _ => {}
    }
}

/// Numbers of a numeric or logical array (column-major); empty for other values.
pub fn numbers(v: &Mat5Value) -> Vec<f64> {
    match v {
        Mat5Value::Double { data, .. } => data.clone(),
        Mat5Value::Logical { data, .. } => data.iter().map(|b| f64::from(u8::from(*b))).collect(),
        _ => Vec::new(),
    }
}

/// First number of a numeric or logical array.
pub fn first_number(v: &Mat5Value) -> Option<f64> {
    numbers(v).first().copied()
}

/// The text of a char value.
pub fn text(v: &Mat5Value) -> Option<&str> {
    match v {
        Mat5Value::Char(s) => Some(s),
        _ => None,
    }
}

/// The items of a cell as text; `None` when the value is no cell or an item is not char.
pub fn cell_texts(v: &Mat5Value) -> Option<Vec<String>> {
    match v {
        Mat5Value::Cell { items, .. } => items.iter().map(|i| text(i).map(str::to_owned)).collect(),
        _ => None,
    }
}

/// The elements of a struct value as scalar structs (a scalar struct is its own element).
pub fn struct_elements(v: &Mat5Value) -> Vec<Mat5Value> {
    match v {
        Mat5Value::Struct {
            fields, elements, ..
        } => elements
            .iter()
            .map(|el| Mat5Value::Struct {
                dims: vec![1, 1],
                fields: fields.clone(),
                elements: vec![el.clone()],
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Field names of a struct value.
pub fn field_names(v: &Mat5Value) -> &[String] {
    match v {
        Mat5Value::Struct { fields, .. } => fields,
        _ => &[],
    }
}

/// An `rows × cols` matrix as row vectors; empty for a non-matrix.
pub fn matrix_rows(v: &Mat5Value) -> Vec<Vec<f64>> {
    v.as_rows().unwrap_or_default()
}

/// Builds a column-major `Double` from row vectors.
pub fn rows_to_mat(rows: &[Vec<f64>]) -> Mat5Value {
    let r = rows.len();
    let c = rows.first().map_or(0, Vec::len);
    if r == 0 || c == 0 {
        return Mat5Value::Empty;
    }
    let mut data = Vec::with_capacity(r * c);
    for j in 0..c {
        data.extend(
            rows.iter()
                .map(|row| row.get(j).copied().unwrap_or(f64::NAN)),
        );
    }
    Mat5Value::Double {
        dims: vec![r, c],
        data,
    }
}

/// A struct value with the listed fields (a `1×1` struct).
pub fn scalar_struct(fields: Vec<(&str, Mat5Value)>) -> Mat5Value {
    Mat5Value::struct_scalar(fields.into_iter().map(|(n, v)| (n.to_owned(), v)).collect())
}

/// A `dims`-shaped column of numbers.
pub fn column(values: &[f64]) -> Mat5Value {
    Mat5Value::Double {
        dims: vec![values.len(), 1],
        data: values.to_vec(),
    }
}

/// A cell array of char values with the given dims.
pub fn text_cell(dims: Vec<usize>, items: &[String]) -> Mat5Value {
    Mat5Value::Cell {
        dims,
        items: items.iter().map(|s| Mat5Value::char(s)).collect(),
    }
}

/// Approximate size in bytes of a value when written.
pub fn approx_bytes(v: &Mat5Value) -> u64 {
    match v {
        Mat5Value::Double { data, .. } => 8 * data.len() as u64 + 64,
        Mat5Value::Logical { data, .. } => data.len() as u64 + 64,
        Mat5Value::Char(s) => 2 * s.len() as u64 + 64,
        Mat5Value::Cell { items, .. } => 64 + items.iter().map(approx_bytes).sum::<u64>(),
        Mat5Value::Struct {
            fields, elements, ..
        } => {
            64 + 32 * fields.len() as u64
                + elements
                    .iter()
                    .flatten()
                    .map(|e| approx_bytes(e) + 8)
                    .sum::<u64>()
        }
        Mat5Value::CharMatrix { rows, .. } => {
            64 + rows.iter().map(|r| 2 * r.len() as u64).sum::<u64>()
        }
        Mat5Value::Unsupported { .. } => 64,
        Mat5Value::Empty => 16,
    }
}

fn dims_json(dims: &[usize]) -> Value {
    Value::Array(dims.iter().map(|d| json!(d)).collect())
}

fn number_json(v: f64) -> Value {
    if v.is_nan() {
        Value::String("NaN".to_owned())
    } else if v == f64::INFINITY {
        Value::String("Inf".to_owned())
    } else if v == f64::NEG_INFINITY {
        Value::String("-Inf".to_owned())
    } else {
        serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
    }
}

fn number_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => match s.as_str() {
            "NaN" => Some(f64::NAN),
            "Inf" => Some(f64::INFINITY),
            "-Inf" => Some(f64::NEG_INFINITY),
            _ => None,
        },
        _ => None,
    }
}

/// Lossless JSON encoding of a MAT value (class, dims, data; NaN and infinities as strings).
pub fn to_json(v: &Mat5Value) -> Value {
    match v {
        Mat5Value::Double { dims, data } => json!({
            "class": "double",
            "dims": dims_json(dims),
            "data": data.iter().map(|x| number_json(*x)).collect::<Vec<_>>(),
        }),
        Mat5Value::Char(s) => json!({"class": "char", "text": s}),
        Mat5Value::Logical { dims, data } => json!({
            "class": "logical",
            "dims": dims_json(dims),
            "data": data,
        }),
        Mat5Value::Cell { dims, items } => json!({
            "class": "cell",
            "dims": dims_json(dims),
            "items": items.iter().map(to_json).collect::<Vec<_>>(),
        }),
        Mat5Value::Struct {
            dims,
            fields,
            elements,
        } => json!({
            "class": "struct",
            "dims": dims_json(dims),
            "fields": fields,
            "elements": elements
                .iter()
                .map(|el| Value::Array(el.iter().map(to_json).collect()))
                .collect::<Vec<_>>(),
        }),
        Mat5Value::CharMatrix { dims, rows } => json!({
            "class": "charmatrix",
            "dims": dims_json(dims),
            "rows": rows,
        }),
        Mat5Value::Unsupported { class_name, dims } => json!({
            "class": "unsupported",
            "class_name": class_name,
            "dims": dims_json(dims),
        }),
        Mat5Value::Empty => json!({"class": "empty"}),
    }
}

fn dims_of(v: &Value) -> Option<Vec<usize>> {
    v.as_array()?
        .iter()
        .map(|d| d.as_u64().map(|d| d as usize))
        .collect()
}

/// Inverse of [`to_json`]; `None` for a value that is not such an encoding.
pub fn from_json(v: &Value) -> Option<Mat5Value> {
    let obj: &Map<String, Value> = v.as_object()?;
    let class = obj.get("class")?.as_str()?;
    Some(match class {
        "empty" => Mat5Value::Empty,
        "char" => Mat5Value::Char(obj.get("text")?.as_str()?.to_owned()),
        "charmatrix" => Mat5Value::CharMatrix {
            dims: dims_of(obj.get("dims")?)?,
            rows: obj
                .get("rows")?
                .as_array()?
                .iter()
                .map(|r| r.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?,
        },
        "unsupported" => Mat5Value::Unsupported {
            class_name: obj.get("class_name")?.as_str()?.to_owned(),
            dims: dims_of(obj.get("dims")?)?,
        },
        "double" => Mat5Value::Double {
            dims: dims_of(obj.get("dims")?)?,
            data: obj
                .get("data")?
                .as_array()?
                .iter()
                .map(number_of)
                .collect::<Option<Vec<_>>>()?,
        },
        "logical" => Mat5Value::Logical {
            dims: dims_of(obj.get("dims")?)?,
            data: obj
                .get("data")?
                .as_array()?
                .iter()
                .map(Value::as_bool)
                .collect::<Option<Vec<_>>>()?,
        },
        "cell" => Mat5Value::Cell {
            dims: dims_of(obj.get("dims")?)?,
            items: obj
                .get("items")?
                .as_array()?
                .iter()
                .map(from_json)
                .collect::<Option<Vec<_>>>()?,
        },
        "struct" => Mat5Value::Struct {
            dims: dims_of(obj.get("dims")?)?,
            fields: obj
                .get("fields")?
                .as_array()?
                .iter()
                .map(|f| f.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?,
            elements: obj
                .get("elements")?
                .as_array()?
                .iter()
                .map(|el| {
                    el.as_array()?
                        .iter()
                        .map(from_json)
                        .collect::<Option<Vec<_>>>()
                })
                .collect::<Option<Vec<_>>>()?,
        },
        _ => return None,
    })
}
