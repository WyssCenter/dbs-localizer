// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use fastwarp_matfile::{
    read_any, read_mat5_header_index, read_mat5_var, stream_columns_f32, Mat5Error, Mat5Value,
    MatError, MatFile, MatValue, VarEntry,
};

use crate::error::{format_err, io_err, Error, Result};

pub(crate) struct Numeric {
    pub data: Vec<f64>,
}

fn mat_err(path: &Path, message: impl std::fmt::Display) -> Error {
    Error::Mat {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}

pub(crate) fn is_v73_header(head: &[u8]) -> bool {
    head.starts_with(b"\x89HDF") || (head.len() >= 126 && head[124] == 0x00 && head[125] == 0x02)
}

/// Every variable of a v5, v7 or v7.3 file as value trees; undecodable classes are unsupported values.
pub(crate) fn open(path: &Path) -> Result<Vec<(String, Mat5Value)>> {
    read_any(path).map_err(|e| match e {
        MatError::Io { source, .. } => io_err(path, source),
        other => mat_err(path, other),
    })
}

pub(crate) type Visit<'a> = dyn FnMut(usize, &[f32], usize) -> Result<()> + 'a;

enum Kind {
    V5 { index: Vec<VarEntry> },
    V73(MatFile),
}

pub(crate) struct Streaming {
    path: PathBuf,
    kind: Kind,
}

enum StreamErr {
    V5(Mat5Error),
    V73(MatError),
    Visit(Error),
}

impl From<Mat5Error> for StreamErr {
    fn from(e: Mat5Error) -> Self {
        StreamErr::V5(e)
    }
}

impl From<MatError> for StreamErr {
    fn from(e: MatError) -> Self {
        StreamErr::V73(e)
    }
}

fn v5_numeric(path: &Path, name: &str, v: &Mat5Value) -> Result<Numeric> {
    let data = match v {
        Mat5Value::Double { data, .. } => data.clone(),
        Mat5Value::Logical { data, .. } => data.iter().map(|b| f64::from(u8::from(*b))).collect(),
        Mat5Value::Empty => Vec::new(),
        _ => {
            return Err(format_err(format!(
                "{}: `{name}` is not a numeric array",
                path.display()
            )))
        }
    };
    Ok(Numeric { data })
}

fn v73_numeric(path: &Path, name: &str, v: MatValue) -> Result<Numeric> {
    let data = match v {
        MatValue::Double { data, .. } => data,
        MatValue::Logical { data, .. } => data.iter().map(|b| f64::from(u8::from(*b))).collect(),
        MatValue::Empty { .. } => Vec::new(),
        _ => {
            return Err(format_err(format!(
                "{}: `{name}` is not a numeric array",
                path.display()
            )))
        }
    };
    Ok(Numeric { data })
}

/// Opens `path` for variable-by-variable and streaming reads; only the variable index (v5) or the
/// superblock (v7.3) is read.
pub(crate) fn open_streaming(path: &Path) -> Result<Streaming> {
    let mut file = std::fs::File::open(path).map_err(|e| io_err(path, e))?;
    let mut head = [0u8; 128];
    let mut got = 0;
    while got < head.len() {
        let n = file.read(&mut head[got..]).map_err(|e| io_err(path, e))?;
        if n == 0 {
            break;
        }
        got += n;
    }
    let kind = if is_v73_header(&head[..got]) {
        drop(file);
        Kind::V73(MatFile::open(path).map_err(|e| mat_err(path, e))?)
    } else {
        let mut reader = BufReader::new(file);
        Kind::V5 {
            index: read_mat5_header_index(&mut reader).map_err(|e| mat_err(path, e))?,
        }
    };
    Ok(Streaming {
        path: path.to_path_buf(),
        kind,
    })
}

impl Streaming {
    fn reopen(&self) -> Result<std::fs::File> {
        std::fs::File::open(&self.path).map_err(|e| io_err(&self.path, e))
    }

    /// MATLAB dimensions of the numeric variable `name`, `None` when it is missing.
    pub(crate) fn shape(&self, name: &str) -> Result<Option<Vec<usize>>> {
        match &self.kind {
            Kind::V5 { index } => match index.iter().find(|e| e.name == name) {
                None => Ok(None),
                Some(e) if e.is_numeric() => Ok(Some(e.dims.clone())),
                Some(_) => Err(format_err(format!(
                    "{}: `{name}` is not a numeric array",
                    self.path.display()
                ))),
            },
            Kind::V73(mf) => Ok(mf
                .numeric_info(name)
                .map_err(|e| mat_err(&self.path, e))?
                .map(|i| i.dims)),
        }
    }

    /// A whole (small) numeric variable as `f64`.
    pub(crate) fn numeric(&self, name: &str) -> Result<Option<Numeric>> {
        match &self.kind {
            Kind::V5 { index } => {
                if !index.iter().any(|e| e.name == name) {
                    return Ok(None);
                }
                let mut file = self.reopen()?;
                match read_mat5_var(&mut file, name).map_err(|e| mat_err(&self.path, e))? {
                    None => Ok(None),
                    Some(v) => v5_numeric(&self.path, name, &v).map(Some),
                }
            }
            Kind::V73(mf) => match mf.get(name).map_err(|e| mat_err(&self.path, e))? {
                None => Ok(None),
                Some(v) => v73_numeric(&self.path, name, v).map(Some),
            },
        }
    }

    /// A text variable; `None` when missing or not text.
    pub(crate) fn text(&self, name: &str) -> Result<Option<String>> {
        match &self.kind {
            Kind::V5 { index } => {
                if !index.iter().any(|e| e.name == name) {
                    return Ok(None);
                }
                let mut file = self.reopen()?;
                let v = read_mat5_var(&mut file, name).map_err(|e| mat_err(&self.path, e))?;
                Ok(match v {
                    Some(Mat5Value::Char(s)) => Some(s),
                    Some(Mat5Value::Empty) => Some(String::new()),
                    _ => None,
                })
            }
            Kind::V73(mf) => Ok(mf
                .get(name)
                .map_err(|e| mat_err(&self.path, e))?
                .and_then(|v| v.as_text().map(str::to_owned))),
        }
    }

    /// Streams columns `cols` of the 2-D numeric variable `name` as `f32` pieces of at most
    /// `chunk_len` values: `visit(col, piece, first_row)`.
    pub(crate) fn stream_cols(
        &self,
        name: &str,
        cols: &[usize],
        chunk_len: usize,
        visit: &mut Visit<'_>,
    ) -> Result<()> {
        let result = match &self.kind {
            Kind::V5 { .. } => {
                let mut file = self.reopen()?;
                stream_columns_f32(&mut file, name, cols, chunk_len, |c, ch, f| {
                    visit(c, ch, f).map_err(StreamErr::Visit)
                })
            }
            Kind::V73(mf) => mf.stream_numeric_f32(name, cols, chunk_len, |c, ch, f| {
                visit(c, ch, f).map_err(StreamErr::Visit)
            }),
        };
        result.map_err(|e| match e {
            StreamErr::V5(e) => mat_err(&self.path, e),
            StreamErr::V73(e) => mat_err(&self.path, e),
            StreamErr::Visit(e) => e,
        })
    }
}
