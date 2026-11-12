// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::error::{fmt_err, MatError, Result};

const PAGE: u64 = 1 << 12;
const MAX_PAGES: usize = 4096;

pub(crate) enum Source {
    Bytes(Vec<u8>),
    File(FileSource),
}

pub(crate) struct FileSource {
    path: PathBuf,
    len: u64,
    loaded: AtomicU64,
    state: Mutex<FileState>,
}

struct FileState {
    file: File,
    pages: HashMap<u64, Vec<u8>>,
}

impl Source {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let io = |source| MatError::Io {
            path: path.to_path_buf(),
            source,
        };
        let file = File::open(path).map_err(io)?;
        let len = file.metadata().map_err(io)?.len();
        Ok(Source::File(FileSource {
            path: path.to_path_buf(),
            len,
            loaded: AtomicU64::new(0),
            state: Mutex::new(FileState {
                file,
                pages: HashMap::new(),
            }),
        }))
    }

    pub(crate) fn len(&self) -> u64 {
        match self {
            Source::Bytes(b) => b.len() as u64,
            Source::File(f) => f.len,
        }
    }

    pub(crate) fn bytes_loaded(&self) -> u64 {
        match self {
            Source::Bytes(b) => b.len() as u64,
            Source::File(f) => f.loaded.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn read(&self, pos: u64, n: usize) -> Result<Cow<'_, [u8]>> {
        let end = pos
            .checked_add(n as u64)
            .filter(|e| *e <= self.len())
            .ok_or_else(|| fmt_err(format!("read of {n} byte(s) at {pos} past end of file")))?;
        match self {
            Source::Bytes(b) => Ok(Cow::Borrowed(&b[pos as usize..end as usize])),
            Source::File(f) => f.read(pos, end).map(Cow::Owned),
        }
    }
}

impl FileSource {
    fn read(&self, pos: u64, end: u64) -> Result<Vec<u8>> {
        let io = |source| MatError::Io {
            path: self.path.clone(),
            source,
        };
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let n = (end - pos) as usize;
        if end - pos > PAGE {
            let mut out = vec![0u8; n];
            state.file.seek(SeekFrom::Start(pos)).map_err(io)?;
            state.file.read_exact(&mut out).map_err(io)?;
            self.loaded.fetch_add(n as u64, Ordering::Relaxed);
            return Ok(out);
        }
        let mut out = Vec::with_capacity(n);
        let mut at = pos;
        while at < end {
            let page = at / PAGE;
            if !state.pages.contains_key(&page) {
                if state.pages.len() >= MAX_PAGES {
                    state.pages.clear();
                }
                let start = page * PAGE;
                let len = PAGE.min(self.len - start) as usize;
                let mut buf = vec![0u8; len];
                state.file.seek(SeekFrom::Start(start)).map_err(io)?;
                state.file.read_exact(&mut buf).map_err(io)?;
                self.loaded.fetch_add(len as u64, Ordering::Relaxed);
                state.pages.insert(page, buf);
            }
            let buf = state
                .pages
                .get(&page)
                .ok_or_else(|| fmt_err("page cache miss"))?;
            let from = (at - page * PAGE) as usize;
            let to = ((end - page * PAGE) as usize).min(buf.len());
            if to <= from {
                return Err(fmt_err("file shrank while it was read"));
            }
            out.extend_from_slice(&buf[from..to]);
            at = page * PAGE + to as u64;
        }
        Ok(out)
    }
}
