// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! A minimal pure-Rust HDF5 subset, enough for ITK transform files.
//!
//! Written from the public HDF5 file-format specification (version 3.0). Not a general HDF5
//! library.
//!
//! # Supported when reading
//!
//! * Superblock version 2 and 3 (the "latest" file format libhdf5 uses for ITK 5 output), offset
//!   and length sizes up to 8 bytes, little-endian files.
//! * Version 2 object headers (`OHDR`) with continuation chunks (`OCHK`); Jenkins lookup3
//!   checksums are verified.
//! * Groups with compact link storage (link messages). Dense link storage (fractal heap plus
//!   v2 B-tree, used above 8 links per group) is refused.
//! * Datasets of rank 0 or 1 with IEEE float (32 or 64 bit, either byte order), integer, fixed
//!   and variable-length string types (global heap).
//! * Layouts: compact, contiguous, and chunked rank-1 datasets indexed by a v1 B-tree
//!   (layout v3), or by a single chunk, an implicit index or an unpaged fixed array (layout v4).
//! * Filters: deflate, shuffle, Fletcher32 (checksum stripped, not verified).
//!
//! # Not supported
//!
//! Superblock versions 0 and 1 (see `fastwarp_matfile` for the MATLAB subset), attributes,
//! soft and external links, dense groups, extensible-array and v2-B-tree chunk indices, paged
//! fixed arrays, chunked datasets of rank above 1, virtual datasets.
//!
//! # Writing
//!
//! Superblock v2, version 2 object headers without timestamps, compact groups, contiguous
//! uncompressed `f64` datasets and variable-length ASCII strings stored in one global heap
//! collection, laid out as ITK does.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use crate::error::{fmt_err, unsupported, RegError, Result};

const H5: &str = "HDF5";
const SIGNATURE: [u8; 8] = [0x89, b'H', b'D', b'F', 0x0d, 0x0a, 0x1a, 0x0a];
const UNDEF: u64 = u64::MAX;
const MAX_DATASET_BYTES: usize = 1 << 34;
const MAX_DEFLATE_RATIO: usize = 1032;

fn rotl(x: u32, k: u32) -> u32 {
    x.rotate_left(k)
}

/// Jenkins `lookup3` (`hashlittle`, initial value 0), the checksum of HDF5 metadata.
pub(crate) fn lookup3(data: &[u8]) -> u32 {
    let init = 0xdead_beef_u32.wrapping_add(data.len() as u32);
    let (mut a, mut b, mut c) = (init, init, init);
    let mut k = data;
    while k.len() > 12 {
        a = a.wrapping_add(u32::from_le_bytes([k[0], k[1], k[2], k[3]]));
        b = b.wrapping_add(u32::from_le_bytes([k[4], k[5], k[6], k[7]]));
        c = c.wrapping_add(u32::from_le_bytes([k[8], k[9], k[10], k[11]]));
        a = a.wrapping_sub(c);
        a ^= rotl(c, 4);
        c = c.wrapping_add(b);
        b = b.wrapping_sub(a);
        b ^= rotl(a, 6);
        a = a.wrapping_add(c);
        c = c.wrapping_sub(b);
        c ^= rotl(b, 8);
        b = b.wrapping_add(a);
        a = a.wrapping_sub(c);
        a ^= rotl(c, 16);
        c = c.wrapping_add(b);
        b = b.wrapping_sub(a);
        b ^= rotl(a, 19);
        a = a.wrapping_add(c);
        c = c.wrapping_sub(b);
        c ^= rotl(b, 4);
        b = b.wrapping_add(a);
        k = &k[12..];
    }
    if k.is_empty() {
        return c;
    }
    let mut t = [0u8; 12];
    t[..k.len()].copy_from_slice(k);
    a = a.wrapping_add(u32::from_le_bytes([t[0], t[1], t[2], t[3]]));
    b = b.wrapping_add(u32::from_le_bytes([t[4], t[5], t[6], t[7]]));
    c = c.wrapping_add(u32::from_le_bytes([t[8], t[9], t[10], t[11]]));
    c ^= b;
    c = c.wrapping_sub(rotl(b, 14));
    a ^= c;
    a = a.wrapping_sub(rotl(c, 11));
    b ^= a;
    b = b.wrapping_sub(rotl(a, 25));
    c ^= b;
    c = c.wrapping_sub(rotl(b, 16));
    a ^= c;
    a = a.wrapping_sub(rotl(c, 4));
    b ^= a;
    b = b.wrapping_sub(rotl(a, 14));
    c ^= b;
    c = c.wrapping_sub(rotl(b, 24));
    c
}

#[derive(Clone, Debug, PartialEq)]
enum Dtype {
    Float { size: usize, big_endian: bool },
    Int { size: usize },
    FixedString { size: usize },
    VlenString,
    Other(u8),
}

impl Dtype {
    fn size(&self) -> usize {
        match self {
            Dtype::Float { size, .. } | Dtype::Int { size } | Dtype::FixedString { size } => *size,
            Dtype::VlenString => 16,
            Dtype::Other(_) => 0,
        }
    }
}

#[derive(Clone, Debug)]
struct Filter {
    id: u16,
    params: Vec<u32>,
}

#[derive(Clone, Debug)]
enum ChunkIndex {
    BTreeV1(u64),
    Single {
        addr: u64,
        filtered: Option<(u64, u32)>,
    },
    FixedArray(u64),
    Implicit(u64),
}

#[derive(Clone, Debug)]
enum Layout {
    Compact(Vec<u8>),
    Contiguous { addr: u64, size: u64 },
    Chunked { chunk: Vec<u64>, index: ChunkIndex },
}

struct Msg {
    ty: u8,
    body: Vec<u8>,
}

struct Visits {
    seen: HashSet<u64>,
    budget: usize,
}

impl Visits {
    fn new(budget: usize) -> Self {
        Self {
            seen: HashSet::new(),
            budget,
        }
    }

    fn spend(&mut self, n: usize, what: &'static str) -> Result<()> {
        self.budget = self
            .budget
            .checked_sub(n)
            .ok_or_else(|| fmt_err(H5, what))?;
        Ok(())
    }
}

/// A parsed HDF5 file held in memory.
pub(crate) struct H5File {
    data: Vec<u8>,
    off_sz: usize,
    len_sz: usize,
    root: u64,
}

impl H5File {
    /// Reads and parses a file.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).map_err(|source| RegError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(data)
    }

    /// Parses the superblock of an in-memory file.
    pub(crate) fn parse(data: Vec<u8>) -> Result<Self> {
        if data.len() < 48 || data[..8] != SIGNATURE {
            return Err(fmt_err(H5, "not an HDF5 file (bad signature)"));
        }
        let version = data[8];
        if version != 2 && version != 3 {
            return Err(unsupported(
                H5,
                format!("superblock version {version} (only the version 2 and 3 formats)"),
            ));
        }
        let off_sz = usize::from(data[9]);
        let len_sz = usize::from(data[10]);
        if !(1..=8).contains(&off_sz) || !(1..=8).contains(&len_sz) {
            return Err(unsupported(
                H5,
                format!("offset size {off_sz} / length size {len_sz}"),
            ));
        }
        let end = 12 + 4 * off_sz;
        if data.len() < end + 4 {
            return Err(fmt_err(H5, "truncated superblock"));
        }
        let stored = u32::from_le_bytes([data[end], data[end + 1], data[end + 2], data[end + 3]]);
        if stored != lookup3(&data[..end]) {
            return Err(fmt_err(H5, "superblock checksum mismatch"));
        }
        let mut f = Self {
            data,
            off_sz,
            len_sz,
            root: 0,
        };
        f.root = f.uint(12 + 3 * off_sz, off_sz)?;
        Ok(f)
    }

    fn bytes(&self, pos: u64, n: usize) -> Result<&[u8]> {
        let start = usize::try_from(pos).map_err(|_| fmt_err(H5, "address out of range"))?;
        let end = start
            .checked_add(n)
            .ok_or_else(|| fmt_err(H5, "address overflow"))?;
        self.data
            .get(start..end)
            .ok_or_else(|| fmt_err(H5, format!("read of {n} byte(s) at {pos} past end of file")))
    }

    fn uint(&self, pos: usize, n: usize) -> Result<u64> {
        let b = self.bytes(pos as u64, n)?;
        let mut v = 0u64;
        for (i, x) in b.iter().enumerate() {
            v |= u64::from(*x) << (8 * i);
        }
        Ok(v)
    }

    fn is_undef(&self, v: u64) -> bool {
        v == UNDEF || (self.off_sz < 8 && v == (1u64 << (8 * self.off_sz)) - 1)
    }

    fn messages(&self, addr: u64) -> Result<Vec<Msg>> {
        let a = usize::try_from(addr).map_err(|_| fmt_err(H5, "address out of range"))?;
        if self.bytes(addr, 4)? != b"OHDR" {
            return Err(fmt_err(H5, format!("no object header at {addr}")));
        }
        let version = self.bytes(addr + 4, 1)?[0];
        if version != 2 {
            return Err(unsupported(H5, format!("object header version {version}")));
        }
        let flags = self.bytes(addr + 5, 1)?[0];
        let mut pos = a + 6;
        if flags & 0x20 != 0 {
            pos += 16;
        }
        if flags & 0x10 != 0 {
            pos += 4;
        }
        let size_len = 1usize << (flags & 3);
        let chunk0 = usize::try_from(self.uint(pos, size_len)?)
            .map_err(|_| fmt_err(H5, "object header chunk too large"))?;
        pos += size_len;
        let region_end = pos
            .checked_add(chunk0)
            .ok_or_else(|| fmt_err(H5, "object header chunk overflows"))?;
        let stored = self.uint(region_end, 4)? as u32;
        if stored != lookup3(self.bytes(addr, region_end - a)?) {
            return Err(fmt_err(
                H5,
                format!("object header checksum mismatch at {addr}"),
            ));
        }
        let track = flags & 4 != 0;
        let mut out = Vec::new();
        let mut visits = Visits::new(self.data.len());
        self.collect_messages(pos, region_end, track, &mut out, &mut visits, 0)?;
        Ok(out)
    }

    fn collect_messages(
        &self,
        start: usize,
        end: usize,
        track: bool,
        out: &mut Vec<Msg>,
        visits: &mut Visits,
        depth: usize,
    ) -> Result<()> {
        if depth > 16 {
            return Err(fmt_err(H5, "object header continuation chain too deep"));
        }
        let head = if track { 6 } else { 4 };
        let mut pos = start;
        while pos + head <= end {
            let ty = self.bytes(pos as u64, 1)?[0];
            let size = self.uint(pos + 1, 2)? as usize;
            pos += head;
            if pos.checked_add(size).is_none_or(|e| e > end) {
                return Err(fmt_err(H5, "object header message overruns its chunk"));
            }
            let body = self.bytes(pos as u64, size)?;
            pos += size;
            if ty == 0x10 {
                if body.len() < self.off_sz + self.len_sz {
                    return Err(fmt_err(H5, "short continuation message"));
                }
                let off = self.uint_slice(body, 0, self.off_sz);
                let len = usize::try_from(self.uint_slice(body, self.off_sz, self.len_sz))
                    .ok()
                    .filter(|l| *l >= 8)
                    .ok_or_else(|| fmt_err(H5, "bad continuation block length"))?;
                if !visits.seen.insert(off) {
                    return Err(fmt_err(
                        H5,
                        format!("object header continuation block at {off} referenced twice"),
                    ));
                }
                visits.spend(
                    len,
                    "object header continuation blocks larger than their file can hold",
                )?;
                if self.bytes(off, 4)? != b"OCHK" {
                    return Err(fmt_err(H5, "continuation block without OCHK signature"));
                }
                let start =
                    usize::try_from(off).map_err(|_| fmt_err(H5, "address out of range"))?;
                let block_end = start
                    .checked_add(len - 4)
                    .ok_or_else(|| fmt_err(H5, "continuation block overflows"))?;
                let stored = self.uint(block_end, 4)? as u32;
                if stored != lookup3(self.bytes(off, len - 4)?) {
                    return Err(fmt_err(H5, "continuation block checksum mismatch"));
                }
                self.collect_messages(start + 4, block_end, track, out, visits, depth + 1)?;
            } else {
                out.push(Msg {
                    ty,
                    body: body.to_vec(),
                });
            }
        }
        Ok(())
    }

    fn uint_slice(&self, b: &[u8], at: usize, n: usize) -> u64 {
        let mut v = 0u64;
        for i in 0..n {
            v |= u64::from(*b.get(at + i).unwrap_or(&0)) << (8 * i);
        }
        v
    }

    fn links(&self, addr: u64) -> Result<Vec<(String, u64)>> {
        let msgs = self.messages(addr)?;
        let mut out = Vec::new();
        for m in &msgs {
            match m.ty {
                0x02 => {
                    let flags = *m.body.get(1).unwrap_or(&0);
                    let mut at = 2;
                    if flags & 1 != 0 {
                        at += 8;
                    }
                    if self.uint_slice(&m.body, at, self.off_sz) != self.mask() {
                        return Err(unsupported(
                            H5,
                            "dense link storage (more than 8 links in a group)",
                        ));
                    }
                }
                0x11 => return Err(unsupported(H5, "symbol-table groups")),
                0x06 => {
                    let b = &m.body;
                    let flags = *b.get(1).unwrap_or(&0);
                    let mut at = 2;
                    let mut link_type = 0u8;
                    if flags & 8 != 0 {
                        link_type = *b.get(at).unwrap_or(&0);
                        at += 1;
                    }
                    if flags & 4 != 0 {
                        at += 8;
                    }
                    if flags & 0x10 != 0 {
                        at += 1;
                    }
                    let name_len_sz = 1usize << (flags & 3);
                    let name_len = self.uint_slice(b, at, name_len_sz) as usize;
                    at += name_len_sz;
                    let name = at
                        .checked_add(name_len)
                        .and_then(|end| b.get(at..end))
                        .ok_or_else(|| fmt_err(H5, "link name overruns its message"))?;
                    at += name_len;
                    if link_type == 0 {
                        let target = self.uint_slice(b, at, self.off_sz);
                        out.push((String::from_utf8_lossy(name).into_owned(), target));
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    fn mask(&self) -> u64 {
        if self.off_sz >= 8 {
            u64::MAX
        } else {
            (1u64 << (8 * self.off_sz)) - 1
        }
    }

    fn resolve(&self, path: &str) -> Result<u64> {
        let mut addr = self.root;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let links = self.links(addr)?;
            addr = links
                .into_iter()
                .find(|(n, _)| n == part)
                .map(|(_, a)| a)
                .ok_or_else(|| fmt_err(H5, format!("no such member `{part}` in `{path}`")))?;
        }
        Ok(addr)
    }

    /// Names of the members of the group at `path` (`""` is the root), in storage order.
    pub(crate) fn members(&self, path: &str) -> Result<Vec<String>> {
        let addr = self.resolve(path)?;
        Ok(self.links(addr)?.into_iter().map(|(n, _)| n).collect())
    }

    /// Whether `path` exists.
    pub(crate) fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    fn parse_dataspace(&self, b: &[u8]) -> Result<Vec<u64>> {
        let version = *b.first().ok_or_else(|| fmt_err(H5, "empty dataspace"))?;
        let rank = usize::from(*b.get(1).unwrap_or(&0));
        let (dims_at, scalar) = match version {
            1 => (8, false),
            2 => (4, b.get(3) == Some(&0)),
            v => return Err(unsupported(H5, format!("dataspace version {v}"))),
        };
        if scalar || rank == 0 {
            return Ok(Vec::new());
        }
        Ok((0..rank)
            .map(|i| self.uint_slice(b, dims_at + i * self.len_sz, self.len_sz))
            .collect())
    }

    fn parse_dtype(&self, b: &[u8]) -> Result<Dtype> {
        if b.len() < 8 {
            return Err(fmt_err(H5, "short datatype message"));
        }
        let class = b[0] & 0x0f;
        let size = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
        Ok(match class {
            0 => Dtype::Int { size },
            1 => {
                if size != 4 && size != 8 {
                    return Err(unsupported(H5, format!("float of {size} bytes")));
                }
                Dtype::Float {
                    size,
                    big_endian: b[1] & 1 != 0,
                }
            }
            3 => Dtype::FixedString { size },
            9 => {
                if b[1] & 0x0f == 1 {
                    Dtype::VlenString
                } else {
                    Dtype::Other(9)
                }
            }
            c => Dtype::Other(c),
        })
    }

    fn parse_layout(&self, b: &[u8]) -> Result<Layout> {
        let version = *b
            .first()
            .ok_or_else(|| fmt_err(H5, "empty layout message"))?;
        let class = *b.get(1).unwrap_or(&0);
        let o = self.off_sz;
        let l = self.len_sz;
        match (version, class) {
            (3, 0) | (4, 0) => {
                let size = self.uint_slice(b, 2, 2) as usize;
                Ok(Layout::Compact(
                    b.get(4..4 + size)
                        .ok_or_else(|| fmt_err(H5, "compact data overruns its message"))?
                        .to_vec(),
                ))
            }
            (3, 1) | (4, 1) => Ok(Layout::Contiguous {
                addr: self.uint_slice(b, 2, o),
                size: self.uint_slice(b, 2 + o, l),
            }),
            (3, 2) => {
                let dimensionality = usize::from(*b.get(2).unwrap_or(&0));
                let btree = self.uint_slice(b, 3, o);
                let chunk = (0..dimensionality)
                    .map(|i| self.uint_slice(b, 3 + o + 4 * i, 4))
                    .collect();
                Ok(Layout::Chunked {
                    chunk,
                    index: ChunkIndex::BTreeV1(btree),
                })
            }
            (4, 2) => {
                let flags = *b.get(2).unwrap_or(&0);
                let dimensionality = usize::from(*b.get(3).unwrap_or(&0));
                let enc = usize::from(*b.get(4).unwrap_or(&0));
                if !(1..=8).contains(&enc) {
                    return Err(fmt_err(H5, format!("bad chunk dimension size {enc}")));
                }
                let chunk: Vec<u64> = (0..dimensionality)
                    .map(|i| self.uint_slice(b, 5 + i * enc, enc))
                    .collect();
                let mut at = 5 + dimensionality * enc;
                let index_type = *b.get(at).unwrap_or(&0);
                at += 1;
                let index = match index_type {
                    1 => {
                        let filtered = if flags & 2 != 0 {
                            let size = self.uint_slice(b, at, l);
                            let mask = self.uint_slice(b, at + l, 4) as u32;
                            at += l + 4;
                            Some((size, mask))
                        } else {
                            None
                        };
                        ChunkIndex::Single {
                            addr: self.uint_slice(b, at, o),
                            filtered,
                        }
                    }
                    2 => ChunkIndex::Implicit(self.uint_slice(b, at, o)),
                    3 => ChunkIndex::FixedArray(self.uint_slice(b, at + 1, o)),
                    4 => return Err(unsupported(H5, "extensible-array chunk index")),
                    5 => return Err(unsupported(H5, "version 2 B-tree chunk index")),
                    t => return Err(unsupported(H5, format!("chunk index type {t}"))),
                };
                Ok(Layout::Chunked { chunk, index })
            }
            (v, c) => Err(unsupported(
                H5,
                format!("data layout version {v} class {c}"),
            )),
        }
    }

    fn parse_filters(&self, b: &[u8]) -> Result<Vec<Filter>> {
        let version = *b.first().unwrap_or(&0);
        let n = usize::from(*b.get(1).unwrap_or(&0));
        let mut at = if version == 1 { 8 } else { 2 };
        let mut out = Vec::new();
        for _ in 0..n {
            let id = self.uint_slice(b, at, 2) as u16;
            at += 2;
            let mut name_len = 0usize;
            if version == 1 || id >= 256 {
                name_len = self.uint_slice(b, at, 2) as usize;
                at += 2;
            }
            at += 2;
            let nparams = self.uint_slice(b, at, 2) as usize;
            at += 2;
            if version == 1 {
                at += name_len.div_ceil(8) * 8;
            } else {
                at += name_len;
            }
            if at + 4 * nparams > b.len() {
                return Err(fmt_err(H5, "filter parameters overrun their message"));
            }
            let params = (0..nparams)
                .map(|i| self.uint_slice(b, at + 4 * i, 4) as u32)
                .collect();
            at += 4 * nparams;
            if version == 1 && nparams % 2 == 1 {
                at += 4;
            }
            out.push(Filter { id, params });
        }
        Ok(out)
    }

    fn unfilter(
        &self,
        mut buf: Vec<u8>,
        filters: &[Filter],
        mask: u32,
        elem: usize,
        limit: usize,
    ) -> Result<Vec<u8>> {
        for (i, f) in filters.iter().enumerate().rev() {
            if mask & (1 << i) != 0 {
                continue;
            }
            buf = match f.id {
                1 => fastwarp_matfile::inflate_zlib_limited(&buf, limit)
                    .map_err(|e| fmt_err(H5, format!("deflate: {e}")))?,
                2 => {
                    let sz = f
                        .params
                        .first()
                        .map_or(elem, |v| *v as usize)
                        .clamp(1, buf.len().max(1));
                    unshuffle(&buf, sz)
                }
                3 => {
                    if buf.len() < 4 {
                        return Err(fmt_err(H5, "fletcher32 chunk shorter than its checksum"));
                    }
                    buf.truncate(buf.len() - 4);
                    buf
                }
                id => return Err(unsupported(H5, format!("filter id {id}"))),
            };
        }
        Ok(buf)
    }

    fn btree_chunks(
        &self,
        addr: u64,
        rank: usize,
        out: &mut Vec<(u64, u64, u64, u32)>,
        visits: &mut Visits,
        depth: usize,
    ) -> Result<()> {
        if depth > 16 {
            return Err(fmt_err(H5, "chunk B-tree too deep"));
        }
        if visits.seen.contains(&addr) {
            return Err(fmt_err(
                H5,
                format!("chunk B-tree node at {addr} referenced twice"),
            ));
        }
        if self.bytes(addr, 4)? != b"TREE" {
            return Err(fmt_err(H5, "chunk B-tree node without TREE signature"));
        }
        let b = self.bytes(addr, 8 + 2 * self.off_sz)?;
        let node_type = b[4];
        let level = b[5];
        let used = usize::from(u16::from_le_bytes([b[6], b[7]]));
        if node_type != 1 {
            return Err(fmt_err(H5, "chunk B-tree of the wrong node type"));
        }
        let key_len = 8 + 8 * (rank + 1);
        if level == 0 {
            visits.spend(
                used * (key_len + self.off_sz),
                "chunk B-tree lists more chunks than its file can hold",
            )?;
        }
        let mut at = addr as usize + 8 + 2 * self.off_sz;
        for _ in 0..used {
            let size = self.uint(at, 4)?;
            let mask = self.uint(at + 4, 4)? as u32;
            let offset = self.uint(at + 8, 8)?;
            let child = self.uint(at + key_len, self.off_sz)?;
            if level == 0 {
                out.push((offset, child, size, mask));
            } else {
                self.btree_chunks(child, rank, out, visits, depth + 1)?;
            }
            at += key_len + self.off_sz;
        }
        visits.seen.insert(addr);
        Ok(())
    }

    fn fixed_array_chunks(
        &self,
        addr: u64,
        c0: u64,
        out: &mut Vec<(u64, u64, u64, u32)>,
        chunk_bytes: u64,
    ) -> Result<()> {
        if self.bytes(addr, 4)? != b"FAHD" {
            return Err(fmt_err(H5, "fixed array header without FAHD signature"));
        }
        let a = addr as usize;
        let client = self.uint(a + 5, 1)?;
        let entry_size = self.uint(a + 6, 1)? as usize;
        let page_bits = self.uint(a + 7, 1)? as u32;
        let max_entries = self.uint(a + 8, self.len_sz)?;
        let block = self.uint(a + 8 + self.len_sz, self.off_sz)?;
        let hdr_len = 8 + self.len_sz + self.off_sz;
        if self.uint(a + hdr_len, 4)? as u32 != lookup3(self.bytes(addr, hdr_len)?) {
            return Err(fmt_err(H5, "fixed array header checksum mismatch"));
        }
        if page_bits >= 64 {
            return Err(unsupported(H5, "fixed array page size above 63 bits"));
        }
        if max_entries > (1u64 << page_bits) {
            return Err(unsupported(H5, "paged fixed array chunk index"));
        }
        if entry_size < self.off_sz {
            return Err(fmt_err(
                H5,
                format!("bad fixed array entry size {entry_size} (below the address size)"),
            ));
        }
        if self.is_undef(block) {
            return Ok(());
        }
        if self.bytes(block, 4)? != b"FADB" {
            return Err(fmt_err(H5, "fixed array data block without FADB signature"));
        }
        let mut at = block as usize + 6 + self.off_sz;
        usize::try_from(max_entries)
            .ok()
            .and_then(|n| n.checked_mul(entry_size))
            .and_then(|n| n.checked_add(at))
            .and_then(|n| n.checked_add(4))
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| fmt_err(H5, "fixed array data block runs past the end of the file"))?;
        for i in 0..max_entries {
            let caddr = self.uint(at, self.off_sz)?;
            let (size, mask) = if client == 1 {
                let sz_len = entry_size
                    .checked_sub(self.off_sz + 4)
                    .filter(|n| (1..=8).contains(n))
                    .ok_or_else(|| fmt_err(H5, "bad fixed array entry size"))?;
                (
                    self.uint(at + self.off_sz, sz_len)?,
                    self.uint(at + self.off_sz + sz_len, 4)? as u32,
                )
            } else {
                (chunk_bytes, 0)
            };
            at += entry_size;
            if !self.is_undef(caddr) {
                let first = i
                    .checked_mul(c0)
                    .ok_or_else(|| fmt_err(H5, "chunk offset overflow"))?;
                out.push((first, caddr, size, mask));
            }
        }
        let end = at;
        if self.uint(end, 4)? as u32 != lookup3(self.bytes(block, end - block as usize)?) {
            return Err(fmt_err(H5, "fixed array data block checksum mismatch"));
        }
        Ok(())
    }

    fn read_raw(&self, addr: u64) -> Result<(Dtype, Vec<u64>, Cow<'_, [u8]>)> {
        let msgs = self.messages(addr)?;
        let find = |ty: u8| msgs.iter().find(|m| m.ty == ty);
        let dims = self.parse_dataspace(
            &find(0x01)
                .ok_or_else(|| fmt_err(H5, "object without a dataspace is not a dataset"))?
                .body,
        )?;
        let dtype = self.parse_dtype(
            &find(0x03)
                .ok_or_else(|| fmt_err(H5, "dataset without a datatype"))?
                .body,
        )?;
        let layout = self.parse_layout(
            &find(0x08)
                .ok_or_else(|| fmt_err(H5, "dataset without a layout"))?
                .body,
        )?;
        let filters = match find(0x0b) {
            Some(m) => self.parse_filters(&m.body)?,
            None => Vec::new(),
        };
        let count = dims
            .iter()
            .try_fold(1u64, |a, d| a.checked_mul(*d))
            .ok_or_else(|| fmt_err(H5, "dataset too large"))?;
        if let Dtype::Other(class) = dtype {
            return Err(unsupported(H5, format!("datatype class {class}")));
        }
        let elem = dtype.size();
        if elem == 0 {
            return Err(fmt_err(H5, "datatype of size 0"));
        }
        let total = usize::try_from(count)
            .ok()
            .and_then(|c| c.checked_mul(elem))
            .filter(|t| *t <= MAX_DATASET_BYTES)
            .ok_or_else(|| fmt_err(H5, "dataset too large"))?;
        let raw = match layout {
            Layout::Compact(mut d) => {
                if d.len() < total {
                    return Err(fmt_err(H5, "compact dataset shorter than its extent"));
                }
                d.truncate(total);
                Cow::Owned(d)
            }
            Layout::Contiguous { addr, size } => {
                if self.is_undef(addr) {
                    if total > self.data.len() {
                        return Err(fmt_err(H5, "unallocated dataset larger than its file"));
                    }
                    Cow::Owned(zeroed(total)?)
                } else {
                    if size < total as u64 {
                        return Err(fmt_err(H5, "contiguous dataset smaller than its extent"));
                    }
                    Cow::Borrowed(self.bytes(addr, total)?)
                }
            }
            Layout::Chunked { chunk, index } => {
                if dims.len() != 1 || chunk.len() != 2 {
                    return Err(unsupported(H5, "chunked datasets of rank above 1"));
                }
                let c0 = chunk[0];
                if c0 == 0 || chunk[1] == 0 {
                    return Err(fmt_err(H5, "chunk dimension of size 0"));
                }
                let chunk_bytes = c0
                    .checked_mul(chunk[1])
                    .ok_or_else(|| fmt_err(H5, "chunk size overflow"))?;
                let cap = if filters.iter().any(|f| f.id == 1) {
                    self.data.len().saturating_mul(MAX_DEFLATE_RATIO)
                } else {
                    self.data.len()
                };
                if total > cap {
                    return Err(fmt_err(H5, "chunked dataset larger than its file can hold"));
                }
                let limit = usize::try_from(chunk_bytes)
                    .ok()
                    .and_then(|b| b.checked_add(4))
                    .ok_or_else(|| fmt_err(H5, "chunk size overflow"))?;
                let n_chunks = count.div_ceil(c0);
                let mut entries: Vec<(u64, u64, u64, u32)> = Vec::new();
                match index {
                    ChunkIndex::BTreeV1(a) => {
                        if !self.is_undef(a) {
                            let mut visits = Visits::new(self.data.len());
                            self.btree_chunks(a, 1, &mut entries, &mut visits, 0)?;
                        }
                    }
                    ChunkIndex::Single { addr, filtered } => {
                        if !self.is_undef(addr) {
                            let (size, mask) = filtered.unwrap_or((chunk_bytes, 0));
                            entries.push((0, addr, size, mask));
                        }
                    }
                    ChunkIndex::FixedArray(a) => {
                        self.fixed_array_chunks(a, c0, &mut entries, chunk_bytes)?;
                    }
                    ChunkIndex::Implicit(a) => {
                        if !self.is_undef(a) {
                            n_chunks
                                .checked_mul(chunk_bytes)
                                .and_then(|n| a.checked_add(n))
                                .filter(|end| *end <= self.data.len() as u64)
                                .ok_or_else(|| {
                                    fmt_err(
                                        H5,
                                        "implicit chunk index runs past the end of the file",
                                    )
                                })?;
                            usize::try_from(n_chunks)
                                .ok()
                                .and_then(|n| entries.try_reserve_exact(n).ok())
                                .ok_or_else(|| fmt_err(H5, "too many chunks to index"))?;
                            for i in 0..n_chunks {
                                entries.push((i * c0, a + i * chunk_bytes, chunk_bytes, 0));
                            }
                        }
                    }
                }
                entries.retain(|e| e.0 < count);
                if entries.len() as u64 > n_chunks {
                    return Err(fmt_err(
                        H5,
                        format!(
                            "chunk index lists {} chunks for a dataset of {n_chunks}",
                            entries.len()
                        ),
                    ));
                }
                let mut stored_budget = Visits::new(self.data.len());
                for e in &entries {
                    stored_budget.spend(
                        usize::try_from(e.2).unwrap_or(usize::MAX),
                        "stored chunks larger than their file can hold",
                    )?;
                }
                let mut out = zeroed(total)?;
                for (elem_off, caddr, size, mask) in entries {
                    let start = usize::try_from(elem_off)
                        .ok()
                        .and_then(|o| o.checked_mul(elem))
                        .ok_or_else(|| fmt_err(H5, "chunk offset overflow"))?;
                    if start >= total {
                        continue;
                    }
                    let size = usize::try_from(size)
                        .map_err(|_| fmt_err(H5, "chunk size out of range"))?;
                    let stored = self.bytes(caddr, size)?.to_vec();
                    let plain = self.unfilter(stored, &filters, mask, elem, limit)?;
                    let n = plain.len().min(total - start);
                    out[start..start + n].copy_from_slice(&plain[..n]);
                }
                Cow::Owned(out)
            }
        };
        Ok((dtype, dims, raw))
    }

    /// Reads a numeric dataset as `f64` (a `f32` dataset is widened).
    pub(crate) fn read_f64(&self, path: &str) -> Result<Vec<f64>> {
        let addr = self.resolve(path)?;
        let (dtype, _, raw) = self.read_raw(addr)?;
        match dtype {
            Dtype::Float {
                size: 8,
                big_endian,
            } => collect_fallible(raw.as_chunks::<8>().0.iter().map(|&a| {
                if big_endian {
                    f64::from_be_bytes(a)
                } else {
                    f64::from_le_bytes(a)
                }
            })),
            Dtype::Float {
                size: 4,
                big_endian,
            } => collect_fallible(raw.as_chunks::<4>().0.iter().map(|&a| {
                f64::from(if big_endian {
                    f32::from_be_bytes(a)
                } else {
                    f32::from_le_bytes(a)
                })
            })),
            other => Err(fmt_err(
                H5,
                format!("`{path}` is not a float dataset ({other:?})"),
            )),
        }
    }

    pub(crate) fn read_f32(&self, path: &str) -> Result<Vec<f32>> {
        let addr = self.resolve(path)?;
        let (dtype, _, raw) = self.read_raw(addr)?;
        match dtype {
            Dtype::Float {
                size: 8,
                big_endian,
            } => collect_fallible(raw.as_chunks::<8>().0.iter().map(|&a| {
                (if big_endian {
                    f64::from_be_bytes(a)
                } else {
                    f64::from_le_bytes(a)
                }) as f32
            })),
            Dtype::Float {
                size: 4,
                big_endian,
            } => collect_fallible(raw.as_chunks::<4>().0.iter().map(|&a| {
                if big_endian {
                    f32::from_be_bytes(a)
                } else {
                    f32::from_le_bytes(a)
                }
            })),
            other => Err(fmt_err(
                H5,
                format!("`{path}` is not a float dataset ({other:?})"),
            )),
        }
    }

    /// Reads a one- or two-byte integer dataset at the root as `u16` units; `None` if absent.
    pub(crate) fn read_u16_member(&self, name: &str) -> Result<Option<Vec<u16>>> {
        if !self.members("")?.iter().any(|n| n == name) {
            return Ok(None);
        }
        let addr = self.resolve(name)?;
        let (dtype, _, raw) = self.read_raw(addr)?;
        match dtype {
            Dtype::Int { size: 2 } => collect_fallible(
                raw.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes([b[0], b[1]])),
            )
            .map(Some),
            Dtype::Int { size: 1 } => collect_fallible(raw.iter().map(|b| u16::from(*b))).map(Some),
            other => Err(fmt_err(
                H5,
                format!("`{name}` is not a character dataset ({other:?})"),
            )),
        }
    }

    /// Reads the first string of a string dataset.
    pub(crate) fn read_string(&self, path: &str) -> Result<String> {
        let addr = self.resolve(path)?;
        let (dtype, _, raw) = self.read_raw(addr)?;
        match dtype {
            Dtype::FixedString { .. } => {
                let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
            }
            Dtype::VlenString => {
                if raw.len() < 8 + self.off_sz {
                    return Err(fmt_err(H5, "empty variable-length string dataset"));
                }
                let len = self.uint_slice(&raw, 0, 4) as usize;
                let heap = self.uint_slice(&raw, 4, self.off_sz);
                let index = self.uint_slice(&raw, 4 + self.off_sz, 4) as u16;
                let bytes = self.global_heap_object(heap, index, len)?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            other => Err(fmt_err(
                H5,
                format!("`{path}` is not a string dataset ({other:?})"),
            )),
        }
    }

    fn global_heap_object(&self, heap: u64, index: u16, len: usize) -> Result<Vec<u8>> {
        if self.bytes(heap, 4)? != b"GCOL" {
            return Err(fmt_err(H5, "global heap collection without GCOL signature"));
        }
        let base = usize::try_from(heap).map_err(|_| fmt_err(H5, "address out of range"))?;
        let size = usize::try_from(self.uint(base + 8, self.len_sz)?)
            .map_err(|_| fmt_err(H5, "global heap too large"))?;
        let mut at = base + 8 + self.len_sz;
        let end = base
            .checked_add(size)
            .ok_or_else(|| fmt_err(H5, "global heap overflows"))?;
        while at + 8 + self.len_sz <= end {
            let idx = self.uint(at, 2)? as u16;
            let obj_len = usize::try_from(self.uint(at + 8, self.len_sz)?)
                .map_err(|_| fmt_err(H5, "global heap object too large"))?;
            let data_at = at + 8 + self.len_sz;
            if idx == 0 {
                break;
            }
            if idx == index {
                if obj_len < len {
                    return Err(fmt_err(H5, "global heap object shorter than its string"));
                }
                return Ok(self.bytes(data_at as u64, len)?.to_vec());
            }
            at = obj_len
                .div_ceil(8)
                .checked_mul(8)
                .and_then(|n| data_at.checked_add(n))
                .ok_or_else(|| fmt_err(H5, "global heap object overflows"))?;
        }
        Err(fmt_err(H5, format!("global heap object {index} not found")))
    }
}

fn zeroed(len: usize) -> Result<Vec<u8>> {
    Vec::<u8>::new()
        .try_reserve_exact(len)
        .map_err(|_| fmt_err(H5, format!("cannot allocate {len} bytes for a dataset")))?;
    Ok(vec![0u8; len])
}

fn collect_fallible<T>(values: impl ExactSizeIterator<Item = T>) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(values.len())
        .map_err(|_| fmt_err(H5, format!("cannot allocate {} values", values.len())))?;
    out.extend(values);
    Ok(out)
}

pub(crate) fn unshuffle(buf: &[u8], elem: usize) -> Vec<u8> {
    let n = buf.len() / elem;
    let mut out = vec![0u8; buf.len()];
    for b in 0..elem {
        for i in 0..n {
            out[i * elem + b] = buf[b * n + i];
        }
    }
    let rest = buf.len() - n * elem;
    if rest > 0 {
        out[n * elem..].copy_from_slice(&buf[n * elem..]);
    }
    out
}

/// A node of a file to write.
pub(crate) enum Node<'a> {
    /// A group of named members.
    Group(Vec<(String, Node<'a>)>),
    /// A rank-1 little-endian `f64` dataset.
    F64(Vec<f64>),
    F32AsF64(&'a [f32]),
    /// A scalar-like variable-length ASCII string dataset of one element.
    Str(String),
}

fn collect_strings<'a>(members: &'a [(String, Node<'_>)], out: &mut Vec<&'a str>) {
    for (_, n) in members {
        match n {
            Node::Group(g) => collect_strings(g, out),
            Node::Str(s) => out.push(s),
            Node::F64(_) | Node::F32AsF64(_) => {}
        }
    }
}

fn pad8(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(8) {
        buf.push(0);
    }
}

fn ohdr(messages: &[(u8, u8, Vec<u8>)]) -> Vec<u8> {
    let total: usize = messages.iter().map(|(_, _, b)| 4 + b.len()).sum();
    let (flags, size_bytes): (u8, usize) = if total <= 0xff {
        (0, 1)
    } else if total <= 0xffff {
        (1, 2)
    } else {
        (2, 4)
    };
    let mut out = Vec::with_capacity(8 + total);
    out.extend_from_slice(b"OHDR");
    out.push(2);
    out.push(flags);
    out.extend_from_slice(&(total as u64).to_le_bytes()[..size_bytes]);
    for (ty, mflags, body) in messages {
        out.push(*ty);
        out.extend_from_slice(&(body.len() as u16).to_le_bytes());
        out.push(*mflags);
        out.extend_from_slice(body);
    }
    let sum = lookup3(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    out
}

fn dataspace(n: u64) -> Vec<u8> {
    let mut b = vec![2, 1, 1, 1];
    b.extend_from_slice(&n.to_le_bytes());
    b.extend_from_slice(&n.to_le_bytes());
    b
}

fn f64_type() -> Vec<u8> {
    vec![
        0x11, 0x20, 0x3f, 0x00, 8, 0, 0, 0, 0, 0, 64, 0, 52, 11, 0, 52, 0xff, 0x03, 0, 0,
    ]
}

fn vlen_string_type() -> Vec<u8> {
    vec![
        0x19, 0x01, 0x00, 0x00, 16, 0, 0, 0, 0x10, 0x00, 0x00, 0x00, 1, 0, 0, 0, 0, 0, 8, 0,
    ]
}

fn contiguous_layout(addr: u64, size: u64) -> Vec<u8> {
    let mut b = vec![3, 1];
    b.extend_from_slice(&addr.to_le_bytes());
    b.extend_from_slice(&size.to_le_bytes());
    b
}

struct Writer {
    buf: Vec<u8>,
    heap_addr: u64,
    next_string: u32,
}

impl Writer {
    fn dataset(&mut self, space: Vec<u8>, ty: Vec<u8>, raw: &[u8]) -> u64 {
        self.dataset_with(space, ty, raw.len(), |buf| buf.extend_from_slice(raw))
    }

    fn dataset_with(
        &mut self,
        space: Vec<u8>,
        ty: Vec<u8>,
        len: usize,
        fill: impl FnOnce(&mut Vec<u8>),
    ) -> u64 {
        pad8(&mut self.buf);
        let data_addr = self.buf.len() as u64;
        self.buf.reserve(len);
        fill(&mut self.buf);
        pad8(&mut self.buf);
        let header_addr = self.buf.len() as u64;
        let header = ohdr(&[
            (0x01, 1, space),
            (0x03, 1, ty),
            (0x05, 1, vec![3, 2]),
            (0x08, 0, contiguous_layout(data_addr, len as u64)),
        ]);
        self.buf.extend_from_slice(&header);
        header_addr
    }

    fn node(&mut self, node: &Node<'_>) -> Result<u64> {
        match node {
            Node::F64(v) => {
                Ok(
                    self.dataset_with(dataspace(v.len() as u64), f64_type(), v.len() * 8, |buf| {
                        for x in v.iter() {
                            buf.extend_from_slice(&x.to_le_bytes());
                        }
                    }),
                )
            }
            Node::F32AsF64(v) => {
                Ok(
                    self.dataset_with(dataspace(v.len() as u64), f64_type(), v.len() * 8, |buf| {
                        for x in v.iter() {
                            buf.extend_from_slice(&f64::from(*x).to_le_bytes());
                        }
                    }),
                )
            }
            Node::Str(s) => {
                self.next_string += 1;
                let mut raw = Vec::with_capacity(16);
                raw.extend_from_slice(&(s.len() as u32).to_le_bytes());
                raw.extend_from_slice(&self.heap_addr.to_le_bytes());
                raw.extend_from_slice(&self.next_string.to_le_bytes());
                Ok(self.dataset(dataspace(1), vlen_string_type(), &raw))
            }
            Node::Group(members) => self.group(members),
        }
    }

    fn group(&mut self, members: &[(String, Node<'_>)]) -> Result<u64> {
        let mut addrs = Vec::with_capacity(members.len());
        for (_, n) in members {
            addrs.push(self.node(n)?);
        }
        let mut link_info = vec![0u8, 0];
        link_info.extend_from_slice(&[0xff; 16]);
        let mut msgs: Vec<(u8, u8, Vec<u8>)> = vec![(0x02, 0, link_info), (0x0a, 1, vec![0, 0])];
        for ((name, _), addr) in members.iter().zip(addrs) {
            let nb = name.as_bytes();
            if nb.is_empty() || nb.len() > 0xffff {
                return Err(fmt_err(H5, format!("bad member name length {}", nb.len())));
            }
            let mut body = vec![1u8];
            if nb.len() <= 0xff {
                body.push(0);
                body.push(nb.len() as u8);
            } else {
                body.push(1);
                body.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            }
            body.extend_from_slice(nb);
            body.extend_from_slice(&addr.to_le_bytes());
            msgs.push((0x06, 0, body));
        }
        pad8(&mut self.buf);
        let at = self.buf.len() as u64;
        self.buf.extend_from_slice(&ohdr(&msgs));
        Ok(at)
    }
}

/// Serialises a tree of groups and datasets to the bytes of an HDF5 file.
pub(crate) fn write_bytes(root: &[(String, Node<'_>)]) -> Result<Vec<u8>> {
    let mut strings = Vec::new();
    collect_strings(root, &mut strings);
    let mut buf = vec![0u8; 48];
    let mut heap_addr = 0u64;
    if !strings.is_empty() {
        pad8(&mut buf);
        heap_addr = buf.len() as u64;
        let mut body = Vec::new();
        for (i, s) in strings.iter().enumerate() {
            body.extend_from_slice(&(i as u16 + 1).to_le_bytes());
            body.extend_from_slice(&1u16.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&(s.len() as u64).to_le_bytes());
            body.extend_from_slice(s.as_bytes());
            while !body.len().is_multiple_of(8) {
                body.push(0);
            }
        }
        let used = 16 + body.len();
        let size = used.max(4096).div_ceil(8) * 8;
        buf.extend_from_slice(b"GCOL");
        buf.push(1);
        buf.extend_from_slice(&[0, 0, 0]);
        buf.extend_from_slice(&(size as u64).to_le_bytes());
        buf.extend_from_slice(&body);
        if size > used {
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u32.to_le_bytes());
            buf.extend_from_slice(&((size - used) as u64).to_le_bytes());
            buf.resize(heap_addr as usize + size, 0);
        }
    }
    let mut w = Writer {
        buf,
        heap_addr,
        next_string: 0,
    };
    let root_addr = w.group(root)?;
    let mut buf = w.buf;
    pad8(&mut buf);
    let eof = buf.len() as u64;
    let mut sb = Vec::with_capacity(48);
    sb.extend_from_slice(&SIGNATURE);
    sb.extend_from_slice(&[2, 8, 8, 0]);
    sb.extend_from_slice(&0u64.to_le_bytes());
    sb.extend_from_slice(&UNDEF.to_le_bytes());
    sb.extend_from_slice(&eof.to_le_bytes());
    sb.extend_from_slice(&root_addr.to_le_bytes());
    let sum = lookup3(&sb);
    sb.extend_from_slice(&sum.to_le_bytes());
    buf[..48].copy_from_slice(&sb);
    Ok(buf)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const CTEST: &[u8] = include_bytes!("../../tests/data/ctest.h5");

    fn fixed_array_header(page_bits: u8) -> H5File {
        let mut data = Vec::new();
        data.extend_from_slice(b"FAHD");
        data.extend_from_slice(&[0, 0, 8, page_bits]);
        data.extend_from_slice(&4u64.to_le_bytes());
        data.extend_from_slice(&u64::MAX.to_le_bytes());
        let sum = lookup3(&data);
        data.extend_from_slice(&sum.to_le_bytes());
        H5File {
            data,
            off_sz: 8,
            len_sz: 8,
            root: 0,
        }
    }

    #[test]
    fn a_fixed_array_page_size_of_64_bits_or_more_is_an_error_not_a_shift_overflow() {
        let mut out = Vec::new();
        let err = fixed_array_header(64)
            .fixed_array_chunks(0, 1, &mut out, 8)
            .unwrap_err();
        assert!(err.to_string().contains("page size"), "{err}");
        fixed_array_header(255)
            .fixed_array_chunks(0, 1, &mut out, 8)
            .unwrap_err();
        fixed_array_header(8)
            .fixed_array_chunks(0, 1, &mut out, 8)
            .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_self_referencing_chunk_btree_stops_at_the_depth_cap() {
        let mut data = Vec::new();
        data.extend_from_slice(b"TREE");
        data.extend_from_slice(&[1, 1]);
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&[0u8; 16]);
        let key_len = 8 + 8 * 2;
        data.extend_from_slice(&vec![0u8; key_len]);
        data.extend_from_slice(&0u64.to_le_bytes());
        let f = H5File {
            data,
            off_sz: 8,
            len_sz: 8,
            root: 0,
        };
        let err = f
            .btree_chunks(0, 1, &mut Vec::new(), &mut Visits::new(usize::MAX), 0)
            .unwrap_err();
        assert!(err.to_string().contains("too deep"), "{err}");
    }

    #[test]
    fn lookup3_matches_known_vectors() {
        assert_eq!(lookup3(b""), 0xdead_beef);
        assert_eq!(lookup3(b"Four score and seven years ago"), 0x1777_0551);
    }

    #[test]
    fn ctest_is_parsed_with_verified_checksums() {
        let f = H5File::parse(CTEST.to_vec()).unwrap();
        let root = f.members("").unwrap();
        assert!(root.contains(&"TransformGroup".to_owned()));
        assert!(root.contains(&"ITKVersion".to_owned()));
        assert_eq!(f.members("TransformGroup").unwrap(), vec!["0", "1", "2"]);
        assert!(f.read_string("ITKVersion").unwrap().starts_with('5'));
        assert_eq!(
            f.read_string("TransformGroup/0/TransformType").unwrap(),
            "CompositeTransform_double_3_3"
        );
        assert_eq!(
            f.read_string("TransformGroup/1/TransformType").unwrap(),
            "AffineTransform_double_3_3"
        );
        assert_eq!(
            f.read_f64("TransformGroup/1/TransformParameters")
                .unwrap()
                .len(),
            12
        );
        assert_eq!(
            f.read_f64("TransformGroup/2/TransformFixedParameters")
                .unwrap()
                .len(),
            18
        );
        assert!(!f.exists("TransformGroup/3"));
    }

    #[test]
    fn written_file_reads_back_bit_exact() {
        let vals = vec![0.1, -0.0, f64::MIN_POSITIVE, 1e300, -1234.5678901234567];
        let root = vec![
            ("Name".to_owned(), Node::Str("hello".into())),
            (
                "G".to_owned(),
                Node::Group(vec![
                    ("v".to_owned(), Node::F64(vals.clone())),
                    ("empty".to_owned(), Node::F64(vec![])),
                    ("s".to_owned(), Node::Str("world wide".into())),
                ]),
            ),
        ];
        let bytes = write_bytes(&root).unwrap();
        let f = H5File::parse(bytes).unwrap();
        assert_eq!(f.members("").unwrap(), vec!["Name", "G"]);
        assert_eq!(f.read_string("Name").unwrap(), "hello");
        assert_eq!(f.read_string("G/s").unwrap(), "world wide");
        let back = f.read_f64("G/v").unwrap();
        assert_eq!(
            back.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            vals.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        assert!(f.read_f64("G/empty").unwrap().is_empty());
        assert!(f.read_f64("Name").is_err());
        assert!(f.read_string("G/v").is_err());
    }

    #[test]
    fn large_groups_and_headers_use_wider_size_fields() {
        let big: Vec<f64> = (0..5000).map(f64::from).collect();
        let root = vec![("big".to_owned(), Node::F64(big.clone()))];
        let f = H5File::parse(write_bytes(&root).unwrap()).unwrap();
        assert_eq!(f.read_f64("big").unwrap(), big);
        let long = "x".repeat(300);
        let root = vec![(long.clone(), Node::F64(vec![1.0]))];
        let f = H5File::parse(write_bytes(&root).unwrap()).unwrap();
        assert_eq!(f.read_f64(&long).unwrap(), vec![1.0]);
    }

    #[test]
    fn corrupt_files_are_rejected() {
        assert!(H5File::parse(vec![0; 10]).is_err());
        let mut bad = CTEST.to_vec();
        bad[60] ^= 0xff;
        assert!(H5File::parse(bad.clone()).is_ok());
        let f = H5File::parse(bad).unwrap();
        assert!(f.members("").is_err());
        let mut bad_sb = CTEST.to_vec();
        bad_sb[20] ^= 1;
        assert!(H5File::parse(bad_sb).is_err());
        let mut v0 = CTEST.to_vec();
        v0[8] = 0;
        assert!(matches!(
            H5File::parse(v0),
            Err(RegError::Unsupported { .. })
        ));
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Index {
        BTreeV1,
        Single,
        FixedArray,
        FixedArrayPlain,
    }

    fn chunked_file(values: &[f64], chunk: usize, index: Index, shuffle: bool) -> Vec<u8> {
        let filtered = index != Index::FixedArrayPlain;
        let n_chunks = if index == Index::Single {
            1
        } else {
            values.len().div_ceil(chunk)
        };
        let chunk = if index == Index::Single {
            values.len()
        } else {
            chunk
        };
        let mut buf = vec![0u8; 48];
        let mut entries: Vec<(u64, u64, u64)> = Vec::new();
        for ci in 0..n_chunks {
            let mut part: Vec<f64> =
                values[ci * chunk..values.len().min((ci + 1) * chunk)].to_vec();
            part.resize(chunk, 0.0);
            let mut raw: Vec<u8> = part.iter().flat_map(|v| v.to_le_bytes()).collect();
            if shuffle {
                let n = raw.len() / 8;
                let mut s = vec![0u8; raw.len()];
                for i in 0..n {
                    for b in 0..8 {
                        s[b * n + i] = raw[i * 8 + b];
                    }
                }
                raw = s;
            }
            let stored = if filtered {
                miniz_oxide::deflate::compress_to_vec_zlib(&raw, 5)
            } else {
                raw
            };
            pad8(&mut buf);
            entries.push(((ci * chunk) as u64, buf.len() as u64, stored.len() as u64));
            buf.extend_from_slice(&stored);
        }
        pad8(&mut buf);
        let index_addr = buf.len() as u64;
        match index {
            Index::BTreeV1 => {
                buf.extend_from_slice(b"TREE");
                buf.push(1);
                buf.push(0);
                buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
                buf.extend_from_slice(&UNDEF.to_le_bytes());
                buf.extend_from_slice(&UNDEF.to_le_bytes());
                for (off, addr, size) in &entries {
                    buf.extend_from_slice(&(*size as u32).to_le_bytes());
                    buf.extend_from_slice(&0u32.to_le_bytes());
                    buf.extend_from_slice(&off.to_le_bytes());
                    buf.extend_from_slice(&0u64.to_le_bytes());
                    buf.extend_from_slice(&addr.to_le_bytes());
                }
                buf.extend_from_slice(&0u32.to_le_bytes());
                buf.extend_from_slice(&0u32.to_le_bytes());
                buf.extend_from_slice(&((n_chunks * chunk) as u64).to_le_bytes());
                buf.extend_from_slice(&0u64.to_le_bytes());
            }
            Index::FixedArray | Index::FixedArrayPlain => {
                let entry_size = if filtered { 16u8 } else { 8u8 };
                let block_addr = index_addr + 4 + 4 + 8 + 8 + 4;
                let mut hdr = Vec::new();
                hdr.extend_from_slice(b"FAHD");
                hdr.extend_from_slice(&[0, u8::from(filtered), entry_size, 10]);
                hdr.extend_from_slice(&(n_chunks as u64).to_le_bytes());
                hdr.extend_from_slice(&block_addr.to_le_bytes());
                let sum = lookup3(&hdr);
                hdr.extend_from_slice(&sum.to_le_bytes());
                buf.extend_from_slice(&hdr);
                assert_eq!(buf.len() as u64, block_addr);
                let mut blk = Vec::new();
                blk.extend_from_slice(b"FADB");
                blk.extend_from_slice(&[0, u8::from(filtered)]);
                blk.extend_from_slice(&index_addr.to_le_bytes());
                for (_, addr, size) in &entries {
                    blk.extend_from_slice(&addr.to_le_bytes());
                    if filtered {
                        blk.extend_from_slice(&(*size as u32).to_le_bytes());
                        blk.extend_from_slice(&0u32.to_le_bytes());
                    }
                }
                let sum = lookup3(&blk);
                blk.extend_from_slice(&sum.to_le_bytes());
                buf.extend_from_slice(&blk);
            }
            Index::Single => {}
        }
        let layout = match index {
            Index::BTreeV1 => {
                let mut l = vec![3, 2, 2];
                l.extend_from_slice(&index_addr.to_le_bytes());
                l.extend_from_slice(&(chunk as u32).to_le_bytes());
                l.extend_from_slice(&8u32.to_le_bytes());
                l
            }
            _ => {
                let single = index == Index::Single;
                let mut l = vec![4, 2, if single { 2 } else { 0 }, 2, 4];
                l.extend_from_slice(&(chunk as u32).to_le_bytes());
                l.extend_from_slice(&8u32.to_le_bytes());
                if single {
                    l.push(1);
                    l.extend_from_slice(&entries[0].2.to_le_bytes());
                    l.extend_from_slice(&0u32.to_le_bytes());
                    l.extend_from_slice(&entries[0].1.to_le_bytes());
                } else {
                    l.push(3);
                    l.push(10);
                    l.extend_from_slice(&index_addr.to_le_bytes());
                }
                l
            }
        };
        let mut filters = vec![2u8, if shuffle { 2 } else { 1 }];
        if shuffle {
            filters.extend_from_slice(&[2, 0, 0, 0, 1, 0, 8, 0, 0, 0]);
        }
        if filtered {
            filters.extend_from_slice(&[1, 0, 0, 0, 1, 0, 5, 0, 0, 0]);
        } else {
            filters.clear();
        }
        let mut msgs = vec![
            (0x01, 1, dataspace(values.len() as u64)),
            (0x03, 1, f64_type()),
            (0x05, 1, vec![3, 2]),
            (0x08, 0, layout),
        ];
        if filtered {
            msgs.push((0x0b, 0, filters));
        }
        dataset_file(buf, &msgs)
    }

    fn dataset_file(mut buf: Vec<u8>, msgs: &[(u8, u8, Vec<u8>)]) -> Vec<u8> {
        pad8(&mut buf);
        let ds_addr = buf.len() as u64;
        buf.extend_from_slice(&ohdr(msgs));
        pad8(&mut buf);
        let root_addr = buf.len() as u64;
        let mut link = vec![1u8, 0, 1, b'd'];
        link.extend_from_slice(&ds_addr.to_le_bytes());
        let mut li = vec![0u8, 0];
        li.extend_from_slice(&[0xff; 16]);
        buf.extend_from_slice(&ohdr(&[(0x02, 0, li), (0x06, 0, link)]));
        pad8(&mut buf);
        let eof = buf.len() as u64;
        let mut sb = Vec::new();
        sb.extend_from_slice(&SIGNATURE);
        sb.extend_from_slice(&[2, 8, 8, 0]);
        sb.extend_from_slice(&0u64.to_le_bytes());
        sb.extend_from_slice(&UNDEF.to_le_bytes());
        sb.extend_from_slice(&eof.to_le_bytes());
        sb.extend_from_slice(&root_addr.to_le_bytes());
        let sum = lookup3(&sb);
        sb.extend_from_slice(&sum.to_le_bytes());
        buf[..48].copy_from_slice(&sb);
        buf
    }

    #[test]
    fn chunked_deflate_datasets_decode_for_every_supported_index() {
        let values: Vec<f64> = (0..37).map(|i| f64::from(i) * 0.37 - 4.0).collect();
        for index in [
            Index::BTreeV1,
            Index::Single,
            Index::FixedArray,
            Index::FixedArrayPlain,
        ] {
            for shuffle in [false, true] {
                if index == Index::FixedArrayPlain && shuffle {
                    continue;
                }
                let f = H5File::parse(chunked_file(&values, 8, index, shuffle)).unwrap();
                assert_eq!(f.read_f64("d").unwrap(), values, "shuffle {shuffle}");
            }
        }
    }

    #[test]
    fn shuffle_inverts() {
        let data: Vec<u8> = (0..24).collect();
        let n = data.len() / 4;
        let mut shuffled = vec![0u8; 24];
        for i in 0..n {
            for b in 0..4 {
                shuffled[b * n + i] = data[i * 4 + b];
            }
        }
        assert_eq!(unshuffle(&shuffled, 4), data);
    }

    fn implicit_layout(c0: u32, c1: u32, addr: u64) -> Vec<u8> {
        let mut l = vec![4, 2, 0, 2, 4];
        l.extend_from_slice(&c0.to_le_bytes());
        l.extend_from_slice(&c1.to_le_bytes());
        l.push(2);
        l.extend_from_slice(&addr.to_le_bytes());
        l
    }

    fn f64_bytes(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn read_raw_error(dtype: &[u8], count: u64, layout: Vec<u8>) -> String {
        let file = dataset_file(
            vec![0u8; 48],
            &[
                (0x01, 1, dataspace(count)),
                (0x03, 1, dtype.to_vec()),
                (0x08, 0, layout),
            ],
        );
        let f = H5File::parse(file).unwrap();
        let addr = f.resolve("d").unwrap();
        assert!(f.read_f64("d").is_err());
        f.read_raw(addr).unwrap_err().to_string()
    }

    #[test]
    fn datatypes_of_size_zero_are_rejected_before_the_chunk_index_is_walked() {
        let int0 = [0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        for count in [1u64 << 16, 1 << 60] {
            let err = read_raw_error(&int0, count, implicit_layout(1, 8, 48));
            assert!(err.contains("size 0"), "count {count}: {err}");
        }
    }

    #[test]
    fn unsupported_datatype_classes_are_rejected_before_the_chunk_index_is_walked() {
        let compound = [0x16, 0, 0, 0, 8, 0, 0, 0];
        for count in [1u64 << 16, 1 << 60] {
            let err = read_raw_error(&compound, count, implicit_layout(1, 8, 48));
            assert!(err.contains("datatype class 6"), "count {count}: {err}");
        }
    }

    #[test]
    fn chunk_dimension_sizes_above_eight_bytes_are_rejected() {
        let mut layout = vec![4, 2, 0, 2, 9];
        layout.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0]);
        layout.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 0, 0]);
        layout.push(2);
        layout.extend_from_slice(&48u64.to_le_bytes());
        let err = read_raw_error(&f64_type(), 4, layout);
        assert!(err.contains("chunk dimension size 9"), "{err}");
    }

    #[test]
    fn oversized_implicit_chunk_indices_are_rejected_before_they_are_walked() {
        let deflate = vec![2u8, 1, 1, 0, 0, 0, 1, 0, 5, 0, 0, 0];
        let file = dataset_file(
            vec![0u8; 48],
            &[
                (0x01, 1, dataspace(1024)),
                (0x03, 1, f64_type()),
                (0x08, 0, implicit_layout(1, 8, 48)),
                (0x0b, 0, deflate),
            ],
        );
        let err = H5File::parse(file).unwrap().read_f64("d").unwrap_err();
        assert!(
            err.to_string().contains("past the end of the file"),
            "{err}"
        );
        let file = dataset_file(
            vec![0u8; 48],
            &[
                (0x01, 1, dataspace(1 << 31)),
                (0x03, 1, f64_type()),
                (0x08, 0, implicit_layout(1, 8, 48)),
            ],
        );
        let err = H5File::parse(file).unwrap().read_f64("d").unwrap_err();
        assert!(err.to_string().contains("larger than its file"), "{err}");
    }

    #[test]
    fn implicit_chunk_indices_decode() {
        let values: Vec<f64> = (0..10).map(|i| f64::from(i) - 2.5).collect();
        let mut buf = vec![0u8; 48];
        let mut padded = values.clone();
        padded.resize(12, 0.0);
        buf.extend_from_slice(&f64_bytes(&padded));
        let file = dataset_file(
            buf,
            &[
                (0x01, 1, dataspace(10)),
                (0x03, 1, f64_type()),
                (0x08, 0, implicit_layout(4, 8, 48)),
            ],
        );
        assert_eq!(H5File::parse(file).unwrap().read_f64("d").unwrap(), values);
    }

    fn btree_node(level: u8, entries: &[(u64, u64, u32)], end: u64) -> Vec<u8> {
        let mut b = b"TREE".to_vec();
        b.push(1);
        b.push(level);
        b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        b.extend_from_slice(&UNDEF.to_le_bytes());
        b.extend_from_slice(&UNDEF.to_le_bytes());
        for (off, child, size) in entries {
            b.extend_from_slice(&size.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&off.to_le_bytes());
            b.extend_from_slice(&0u64.to_le_bytes());
            b.extend_from_slice(&child.to_le_bytes());
        }
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&end.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b
    }

    fn btree_file(
        count: u64,
        chunk: u32,
        deflate: bool,
        build: impl FnOnce(&mut Vec<u8>) -> u64,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; 48];
        let root = build(&mut buf);
        let mut layout = vec![3, 2, 2];
        layout.extend_from_slice(&root.to_le_bytes());
        layout.extend_from_slice(&chunk.to_le_bytes());
        layout.extend_from_slice(&8u32.to_le_bytes());
        let mut msgs = vec![
            (0x01, 1, dataspace(count)),
            (0x03, 1, f64_type()),
            (0x08, 0, layout),
        ];
        if deflate {
            msgs.push((0x0b, 0, vec![2u8, 1, 1, 0, 0, 0, 1, 0, 5, 0, 0, 0]));
        }
        dataset_file(buf, &msgs)
    }

    fn push_node(buf: &mut Vec<u8>, node: &[u8]) -> u64 {
        pad8(buf);
        let at = buf.len() as u64;
        buf.extend_from_slice(node);
        at
    }

    #[test]
    fn chunk_btree_nodes_shared_between_parents_are_rejected() {
        let values = [1.0, 2.0, 3.0, 4.0];
        let two_level = btree_file(4, 2, false, |buf| {
            let data = push_node(buf, &f64_bytes(&values));
            let a = push_node(buf, &btree_node(0, &[(0, data, 16)], 2));
            let b = push_node(buf, &btree_node(0, &[(2, data + 16, 16)], 4));
            push_node(buf, &btree_node(1, &[(0, a, 0), (2, b, 0)], 4))
        });
        let f = H5File::parse(two_level).unwrap();
        assert_eq!(f.read_f64("d").unwrap(), values);
        let dag = btree_file(4, 4, false, |buf| {
            let data = push_node(buf, &f64_bytes(&values));
            let leaf = push_node(buf, &btree_node(0, &[(0, data, 32)], 4));
            let mid = push_node(buf, &btree_node(1, &[(0, leaf, 0)], 4));
            push_node(buf, &btree_node(2, &[(0, mid, 0), (0, mid, 0)], 4))
        });
        let err = H5File::parse(dag).unwrap().read_f64("d").unwrap_err();
        assert!(err.to_string().contains("referenced twice"), "{err}");
    }

    #[test]
    fn overlapping_chunk_btree_leaves_are_bounded_by_the_file_size() {
        let file = btree_file(4, 4, false, |buf| {
            pad8(buf);
            let base = buf.len();
            buf.resize(base + 8 * 32 + 24 + 64 * 32, 0);
            let mut leaves = Vec::new();
            for j in 0..8 {
                let at = base + j * 32;
                buf[at..at + 4].copy_from_slice(b"TREE");
                buf[at + 4] = 1;
                buf[at + 5] = 0;
                buf[at + 6..at + 8].copy_from_slice(&64u16.to_le_bytes());
                leaves.push((0, at as u64, 0));
            }
            push_node(buf, &btree_node(1, &leaves, 4))
        });
        let err = H5File::parse(file).unwrap().read_f64("d").unwrap_err();
        assert!(
            err.to_string()
                .contains("more chunks than its file can hold"),
            "{err}"
        );
    }

    #[test]
    fn chunks_sharing_their_stored_bytes_are_rejected() {
        let file = btree_file(4, 1, false, |buf| {
            let blob = push_node(buf, &[0u8; 600]);
            let entries: Vec<(u64, u64, u32)> = (0..4).map(|i| (i, blob, 600)).collect();
            push_node(buf, &btree_node(0, &entries, 4))
        });
        let err = H5File::parse(file).unwrap().read_f64("d").unwrap_err();
        assert!(err.to_string().contains("stored chunks larger"), "{err}");
    }

    #[test]
    fn chunks_outside_the_dataset_are_skipped_before_they_are_decompressed() {
        let values = [1.5, -2.0, 3.25, 4.0];
        let file = btree_file(4, 4, true, |buf| {
            let stored = miniz_oxide::deflate::compress_to_vec_zlib(&f64_bytes(&values), 5);
            let blob = push_node(buf, &stored);
            push_node(
                buf,
                &btree_node(0, &[(0, blob, stored.len() as u32), (4, 0, 8)], 8),
            )
        });
        assert_eq!(H5File::parse(file).unwrap().read_f64("d").unwrap(), values);
    }

    #[test]
    fn fixed_array_entries_smaller_than_an_address_or_past_the_file_are_rejected() {
        for (entry_size, message) in [
            (0u8, "entry size"),
            (4, "entry size"),
            (8, "runs past the end of the file"),
        ] {
            let mut data = Vec::new();
            data.extend_from_slice(b"FAHD");
            data.extend_from_slice(&[0, 0, entry_size, 16]);
            data.extend_from_slice(&(1u64 << 16).to_le_bytes());
            data.extend_from_slice(&32u64.to_le_bytes());
            let sum = lookup3(&data);
            data.extend_from_slice(&sum.to_le_bytes());
            data.resize(32, 0);
            data.extend_from_slice(b"FADB");
            data.extend_from_slice(&[0, 0]);
            data.extend_from_slice(&0u64.to_le_bytes());
            let sum = lookup3(&data[32..]);
            data.extend_from_slice(&sum.to_le_bytes());
            data.extend_from_slice(&[0u8; 8]);
            let f = H5File {
                data,
                off_sz: 8,
                len_sz: 8,
                root: 0,
            };
            let mut out = Vec::new();
            let err = f.fixed_array_chunks(0, 1, &mut out, 8).unwrap_err();
            assert!(err.to_string().contains(message), "{err}");
            assert!(out.is_empty());
        }
    }

    fn ochk(continuations: &[(u64, u64)]) -> Vec<u8> {
        let mut b = b"OCHK".to_vec();
        for (addr, len) in continuations {
            b.push(0x10);
            b.extend_from_slice(&16u16.to_le_bytes());
            b.push(0);
            b.extend_from_slice(&addr.to_le_bytes());
            b.extend_from_slice(&len.to_le_bytes());
        }
        let sum = lookup3(&b);
        b.extend_from_slice(&sum.to_le_bytes());
        b
    }

    fn continuation_file(fan_out: usize, levels: usize, self_loop: bool) -> Vec<u8> {
        let values = [1.0, 2.0, 3.0];
        let mut buf = vec![0u8; 48];
        let data_addr = buf.len() as u64;
        buf.extend_from_slice(&f64_bytes(&values));
        let empty = ochk(&[]);
        let mut next = (push_node(&mut buf, &empty), empty.len() as u64);
        for _ in 0..levels {
            let block = ochk(&vec![next; fan_out]);
            next = (push_node(&mut buf, &block), block.len() as u64);
        }
        if self_loop {
            pad8(&mut buf);
            let at = buf.len() as u64;
            let block = ochk(&[(at, 28)]);
            assert_eq!(block.len(), 28);
            next = (push_node(&mut buf, &block), 28);
        }
        let mut cont = next.0.to_le_bytes().to_vec();
        cont.extend_from_slice(&next.1.to_le_bytes());
        dataset_file(
            buf,
            &[
                (0x01, 1, dataspace(3)),
                (0x03, 1, f64_type()),
                (0x08, 0, contiguous_layout(data_addr, 24)),
                (0x10, 0, cont),
            ],
        )
    }

    #[test]
    fn shared_or_looping_continuation_blocks_are_rejected() {
        let chain = H5File::parse(continuation_file(1, 3, false)).unwrap();
        assert_eq!(chain.read_f64("d").unwrap(), [1.0, 2.0, 3.0]);
        let fan_out = H5File::parse(continuation_file(4, 3, false)).unwrap();
        let err = fan_out.read_f64("d").unwrap_err();
        assert!(err.to_string().contains("referenced twice"), "{err}");
        let looped = H5File::parse(continuation_file(1, 0, true)).unwrap();
        assert!(looped.read_f64("d").is_err());
    }
}
