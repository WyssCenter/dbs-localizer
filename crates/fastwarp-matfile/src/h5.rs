// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;

use crate::error::{fmt_err, unsupported, Result};
use crate::source::Source;

pub(crate) const SIGNATURE: [u8; 8] = [0x89, b'H', b'D', b'F', 0x0d, 0x0a, 0x1a, 0x0a];
pub(crate) const MAX_ELEMS: u64 = 1 << 24;
const MAX_BYTES: u64 = 8 * MAX_ELEMS;
const MAX_NODES: usize = 100_000;
const STREAM_MAX_CHUNKS: usize = 1 << 19;
const MAX_DEPTH: usize = 16;
const MAX_HEAP: u64 = 64 << 20;

pub(crate) struct Msg {
    pub(crate) ty: u16,
    pub(crate) body: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Dtype {
    Int {
        size: usize,
        signed: bool,
        big: bool,
    },
    Float {
        size: usize,
        big: bool,
    },
    Str {
        size: usize,
    },
    Ref {
        size: usize,
    },
    Other {
        class: u8,
        size: usize,
    },
}

impl Dtype {
    pub(crate) fn size(self) -> usize {
        match self {
            Dtype::Int { size, .. }
            | Dtype::Float { size, .. }
            | Dtype::Str { size }
            | Dtype::Ref { size }
            | Dtype::Other { size, .. } => size,
        }
    }

    pub(crate) fn class(self) -> u8 {
        match self {
            Dtype::Int { .. } => 0,
            Dtype::Float { .. } => 1,
            Dtype::Str { .. } => 3,
            Dtype::Ref { .. } => 7,
            Dtype::Other { class, .. } => class,
        }
    }
}

pub(crate) struct Space {
    pub(crate) dims: Vec<u64>,
    pub(crate) null: bool,
}

impl Space {
    pub(crate) fn count(&self) -> Option<u64> {
        if self.null {
            return Some(0);
        }
        self.dims.iter().try_fold(1u64, |a, d| a.checked_mul(*d))
    }
}

pub(crate) struct Attr {
    pub(crate) dtype: Dtype,
    pub(crate) data: Vec<u8>,
}

enum Layout {
    Compact(Vec<u8>),
    Contiguous { addr: u64, size: u64 },
    Chunked { chunk: Vec<u64>, btree: u64 },
}

pub(crate) struct Filter {
    id: u16,
    params: Vec<u32>,
}

type ChunkEntry = (Vec<u64>, u64, u64, u32);

pub(crate) fn le(b: &[u8]) -> u64 {
    b.iter()
        .take(8)
        .enumerate()
        .fold(0u64, |v, (i, x)| v | (u64::from(*x) << (8 * i)))
}

fn pad8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

pub(crate) fn find_signature(src: &Source) -> Result<u64> {
    let len = src.len();
    let mut at = 0u64;
    while at.checked_add(8).is_some_and(|e| e <= len) {
        if src.read(at, 8)?[..] == SIGNATURE[..] {
            return Ok(at);
        }
        at = if at == 0 { 512 } else { at.saturating_mul(2) };
    }
    Err(fmt_err("no HDF5 signature at 0, 512, 1024, ..."))
}

pub(crate) fn superblock_version(src: &Source) -> Result<(u64, u8)> {
    let base = find_signature(src)?;
    let v = src
        .read(base + 8, 1)
        .map_err(|_| fmt_err("truncated superblock"))?[0];
    Ok((base, v))
}

pub(crate) struct H5 {
    src: Source,
    base: u64,
    o: usize,
    l: usize,
    pub(crate) root: u64,
}

impl H5 {
    pub(crate) fn new(src: Source) -> Result<Self> {
        let (base, version) = superblock_version(&src)?;
        if version > 1 {
            return Err(unsupported(format!("superblock version {version}")));
        }
        let sizes = src
            .read(base + 13, 2)
            .map_err(|_| fmt_err("truncated superblock"))?;
        let (o, l) = (usize::from(sizes[0]), usize::from(sizes[1]));
        if !(1..=8).contains(&o) || !(1..=8).contains(&l) {
            return Err(unsupported(format!("offset size {o} / length size {l}")));
        }
        let mut h = H5 {
            src,
            base,
            o,
            l,
            root: 0,
        };
        let addrs = if version == 0 { 24 } else { 28 };
        h.root = h.off((addrs + 5 * o) as u64)?;
        Ok(h)
    }

    pub(crate) fn bytes_loaded(&self) -> u64 {
        self.src.bytes_loaded()
    }

    pub(crate) fn offset_size(&self) -> usize {
        self.o
    }

    fn bytes(&self, pos: u64, n: usize) -> Result<Cow<'_, [u8]>> {
        let abs = self
            .base
            .checked_add(pos)
            .ok_or_else(|| fmt_err("address overflow"))?;
        self.src.read(abs, n).map_err(|e| match e {
            crate::MatError::Format(_) => {
                fmt_err(format!("read of {n} byte(s) at {pos} past end of file"))
            }
            other => other,
        })
    }

    fn uint(&self, pos: u64, n: usize) -> Result<u64> {
        Ok(le(&self.bytes(pos, n.min(8))?))
    }

    fn off(&self, pos: u64) -> Result<u64> {
        self.uint(pos, self.o)
    }

    pub(crate) fn is_undef(&self, v: u64) -> bool {
        if self.o >= 8 {
            v == u64::MAX
        } else {
            v == (1u64 << (8 * self.o)) - 1
        }
    }

    pub(crate) fn messages(&self, addr: u64) -> Result<Vec<Msg>> {
        let head = self.bytes(addr, 16)?;
        if &head[..4] == b"OHDR" {
            return Err(unsupported("version 2 object header in a version 0 file"));
        }
        if head[0] != 1 {
            return Err(unsupported(format!("object header version {}", head[0])));
        }
        let first_len = u64::from(u32::from_le_bytes([head[8], head[9], head[10], head[11]]));
        let mut chunks = vec![(addr + 16, first_len)];
        let mut out = Vec::new();
        let mut seen = 0usize;
        while let Some((start, len)) = chunks.pop() {
            seen += 1;
            if seen > 64 {
                return Err(fmt_err("too many object header continuation blocks"));
            }
            start
                .checked_add(len)
                .ok_or_else(|| fmt_err("object header chunk overflow"))?;
            let block = self.bytes(start, usize::try_from(len).unwrap_or(usize::MAX))?;
            let mut pos = 0usize;
            while pos + 8 <= block.len() {
                let h = &block[pos..pos + 8];
                let ty = u16::from_le_bytes([h[0], h[1]]);
                let size = usize::from(u16::from_le_bytes([h[2], h[3]]));
                pos += 8;
                let body = block
                    .get(pos..pos + size)
                    .ok_or_else(|| fmt_err("object header message overruns its chunk"))?
                    .to_vec();
                pos += size;
                if ty == 0x10 {
                    if body.len() < self.o + self.l {
                        return Err(fmt_err("short continuation message"));
                    }
                    let off = le(&body[..self.o]);
                    let n = le(&body[self.o..self.o + self.l]);
                    chunks.push((off, n));
                } else if ty != 0 {
                    out.push(Msg { ty, body });
                }
                if out.len() > 100_000 {
                    return Err(fmt_err("too many object header messages"));
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn symbol_table(&self, msgs: &[Msg]) -> Option<(u64, u64)> {
        let st = find(msgs, 0x11)?;
        let b = st.body.get(..2 * self.o)?;
        Some((le(&b[..self.o]), le(&b[self.o..])))
    }

    fn heap_data(&self, heap: u64) -> Result<Vec<u8>> {
        if &self.bytes(heap, 4)?[..] != b"HEAP" {
            return Err(fmt_err("local heap without HEAP signature"));
        }
        let size = self.uint(heap + 8, self.l)?;
        if size > MAX_HEAP {
            return Err(fmt_err("local heap too large"));
        }
        let data = self.off(heap + 8 + 2 * self.l as u64)?;
        Ok(self.bytes(data, size as usize)?.into_owned())
    }

    pub(crate) fn group_entries(
        &self,
        msgs: &[Msg],
        visit: &mut dyn FnMut(&[u8], u64) -> bool,
    ) -> Result<()> {
        let (btree, heap) = self
            .symbol_table(msgs)
            .ok_or_else(|| fmt_err("object is not a symbol-table group"))?;
        let names = self.heap_data(heap)?;
        let mut budget = MAX_NODES;
        self.walk_group(btree, &names, visit, 0, &mut budget)?;
        Ok(())
    }

    pub(crate) fn lookup(&self, msgs: &[Msg], name: &str) -> Result<Option<u64>> {
        let mut found = None;
        self.group_entries(msgs, &mut |n, obj| {
            if n == name.as_bytes() {
                found = Some(obj);
                true
            } else {
                false
            }
        })?;
        Ok(found)
    }

    fn walk_group(
        &self,
        node: u64,
        names: &[u8],
        visit: &mut dyn FnMut(&[u8], u64) -> bool,
        depth: usize,
        budget: &mut usize,
    ) -> Result<bool> {
        if depth > MAX_DEPTH {
            return Err(fmt_err("group B-tree too deep"));
        }
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| fmt_err("group B-tree too large"))?;
        let h = self.bytes(node, 8)?;
        if &h[..4] != b"TREE" || h[4] != 0 {
            return Err(fmt_err("group B-tree node malformed"));
        }
        let level = h[5];
        let used = usize::from(u16::from_le_bytes([h[6], h[7]]));
        let step = (self.o + self.l) as u64;
        let mut at = node + 8 + 2 * self.o as u64 + self.l as u64;
        for _ in 0..used {
            let child = self.off(at)?;
            at += step;
            let stop = if level > 0 {
                self.walk_group(child, names, visit, depth + 1, budget)?
            } else {
                self.search_snod(child, names, visit, budget)?
            };
            if stop {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn search_snod(
        &self,
        node: u64,
        names: &[u8],
        visit: &mut dyn FnMut(&[u8], u64) -> bool,
        budget: &mut usize,
    ) -> Result<bool> {
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| fmt_err("group B-tree too large"))?;
        let h = self.bytes(node, 8)?;
        if &h[..4] != b"SNOD" {
            return Err(fmt_err("symbol node without SNOD signature"));
        }
        let n = usize::from(u16::from_le_bytes([h[6], h[7]]));
        let entry = 2 * self.o + 24;
        let body = self.bytes(node + 8, n * entry)?;
        for i in 0..n {
            let e = &body[i * entry..(i + 1) * entry];
            let name_off = le(&e[..self.o]);
            let obj = le(&e[self.o..2 * self.o]);
            if visit(heap_name(names, name_off)?, obj) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn dataspace(&self, b: &[u8]) -> Result<Space> {
        let version = *b.first().ok_or_else(|| fmt_err("empty dataspace"))?;
        let rank = usize::from(*b.get(1).unwrap_or(&0));
        let dims_at = match version {
            1 => 8,
            2 => 4,
            v => return Err(unsupported(format!("dataspace version {v}"))),
        };
        if rank > 32 {
            return Err(fmt_err("dataspace rank above 32"));
        }
        if version == 2 {
            match b.get(3) {
                Some(0) => {
                    return Ok(Space {
                        dims: Vec::new(),
                        null: false,
                    })
                }
                Some(2) => {
                    return Ok(Space {
                        dims: Vec::new(),
                        null: true,
                    })
                }
                _ => {}
            }
        }
        let dims = (0..rank)
            .map(|i| {
                let at = dims_at + i * self.l;
                b.get(at..at + self.l)
                    .map(le)
                    .ok_or_else(|| fmt_err("dataspace dimensions overrun the message"))
            })
            .collect::<Result<Vec<u64>>>()?;
        Ok(Space { dims, null: false })
    }

    pub(crate) fn dataset_header(&self, msgs: &[Msg]) -> Result<Option<(Space, Dtype)>> {
        let Some(space) = find(msgs, 0x01) else {
            return Ok(None);
        };
        let space = self.dataspace(&space.body)?;
        let dtype = datatype(
            &find(msgs, 0x03)
                .ok_or_else(|| fmt_err("dataset without a datatype"))?
                .body,
        )?;
        Ok(Some((space, dtype)))
    }

    pub(crate) fn read_raw(&self, msgs: &[Msg], space: &Space, dtype: Dtype) -> Result<Vec<u8>> {
        let size = dtype.size();
        let count = space
            .count()
            .filter(|c| *c <= MAX_ELEMS)
            .ok_or_else(|| fmt_err("dataset too large"))?;
        let total = count
            .checked_mul(size as u64)
            .filter(|t| *t <= MAX_BYTES)
            .ok_or_else(|| fmt_err("dataset too large"))? as usize;
        if total == 0 {
            return Ok(Vec::new());
        }
        let layout = self.layout(
            &find(msgs, 0x08)
                .ok_or_else(|| fmt_err("dataset without a layout"))?
                .body,
        )?;
        let filters = match find(msgs, 0x0b) {
            Some(m) => filters(&m.body)?,
            None => Vec::new(),
        };
        match layout {
            Layout::Compact(d) => Ok(d
                .get(..total)
                .ok_or_else(|| fmt_err("compact data shorter than the dataspace"))?
                .to_vec()),
            Layout::Contiguous { addr, size: n } => {
                if self.is_undef(addr) {
                    Ok(vec![0u8; total])
                } else if n < total as u64 {
                    Err(fmt_err("contiguous data shorter than the dataspace"))
                } else {
                    Ok(self.bytes(addr, total)?.into_owned())
                }
            }
            Layout::Chunked { chunk, btree } => {
                self.chunked(&space.dims, &chunk, btree, size, &filters, total)
            }
        }
    }

    pub(crate) fn attribute(&self, msgs: &[Msg], name: &str) -> Result<Option<Attr>> {
        for m in msgs.iter().filter(|m| m.ty == 0x0c) {
            if let Some(a) = self.parse_attribute(&m.body, name)? {
                return Ok(Some(a));
            }
        }
        Ok(None)
    }

    fn parse_attribute(&self, b: &[u8], want: &str) -> Result<Option<Attr>> {
        let short = || fmt_err("attribute message too short");
        let version = *b.first().ok_or_else(short)?;
        let u16_at = |at: usize| {
            b.get(at..at + 2)
                .map(|x| usize::from(u16::from_le_bytes([x[0], x[1]])))
                .ok_or_else(short)
        };
        let (ns, ts, ss) = (u16_at(2)?, u16_at(4)?, u16_at(6)?);
        let (start, pad): (usize, fn(usize) -> usize) = match version {
            1 => (8, pad8),
            2 => (8, |n| n),
            3 => (9, |n| n),
            v => return Err(unsupported(format!("attribute message version {v}"))),
        };
        let name = b.get(start..start + ns).ok_or_else(short)?;
        let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        if &name[..end] != want.as_bytes() {
            return Ok(None);
        }
        if version > 1 && b[1] & 0x03 != 0 {
            return Err(unsupported("shared attribute datatype or dataspace"));
        }
        let mut at = start + pad(ns);
        let dtype = datatype(b.get(at..at + ts).ok_or_else(short)?)?;
        at += pad(ts);
        let space = self.dataspace(b.get(at..at + ss).ok_or_else(short)?)?;
        at += pad(ss);
        let total = space
            .count()
            .and_then(|c| c.checked_mul(dtype.size() as u64))
            .filter(|t| *t <= MAX_BYTES)
            .ok_or_else(|| fmt_err("attribute too large"))? as usize;
        let data = b.get(at..at + total).ok_or_else(short)?.to_vec();
        Ok(Some(Attr { dtype, data }))
    }

    fn layout(&self, b: &[u8]) -> Result<Layout> {
        let version = *b.first().ok_or_else(|| fmt_err("empty layout message"))?;
        let short = || fmt_err("layout message too short");
        match version {
            3 => {
                let class = *b.get(1).ok_or_else(short)?;
                match class {
                    0 => {
                        let n = usize::from(u16::from_le_bytes([
                            *b.get(2).ok_or_else(short)?,
                            *b.get(3).ok_or_else(short)?,
                        ]));
                        Ok(Layout::Compact(b.get(4..4 + n).ok_or_else(short)?.to_vec()))
                    }
                    1 => Ok(Layout::Contiguous {
                        addr: le(b.get(2..2 + self.o).ok_or_else(short)?),
                        size: le(b.get(2 + self.o..2 + self.o + self.l).ok_or_else(short)?),
                    }),
                    2 => {
                        let n = usize::from(*b.get(2).ok_or_else(short)?);
                        let btree = le(b.get(3..3 + self.o).ok_or_else(short)?);
                        let chunk = (0..n)
                            .map(|i| {
                                let at = 3 + self.o + 4 * i;
                                b.get(at..at + 4).map(le).ok_or_else(short)
                            })
                            .collect::<Result<Vec<u64>>>()?;
                        Ok(Layout::Chunked { chunk, btree })
                    }
                    c => Err(unsupported(format!("layout class {c}"))),
                }
            }
            1 | 2 => {
                let n = usize::from(*b.get(1).ok_or_else(short)?);
                let class = *b.get(2).ok_or_else(short)?;
                let mut at = 8;
                let addr = if class == 0 {
                    0
                } else {
                    let a = le(b.get(at..at + self.o).ok_or_else(short)?);
                    at += self.o;
                    a
                };
                let dims = (0..n)
                    .map(|i| b.get(at + 4 * i..at + 4 * i + 4).map(le).ok_or_else(short))
                    .collect::<Result<Vec<u64>>>()?;
                at += 4 * n;
                match class {
                    0 => {
                        let len = le(b.get(at..at + 4).ok_or_else(short)?) as usize;
                        Ok(Layout::Compact(
                            b.get(at + 4..at + 4 + len).ok_or_else(short)?.to_vec(),
                        ))
                    }
                    1 => {
                        let size = dims.iter().try_fold(1u64, |a, d| a.checked_mul(*d));
                        Ok(Layout::Contiguous {
                            addr,
                            size: size.ok_or_else(short)?,
                        })
                    }
                    2 => Ok(Layout::Chunked {
                        chunk: dims,
                        btree: addr,
                    }),
                    c => Err(unsupported(format!("layout class {c}"))),
                }
            }
            v => Err(unsupported(format!("data layout version {v}"))),
        }
    }

    fn chunked(
        &self,
        dims: &[u64],
        chunk: &[u64],
        btree: u64,
        elem: usize,
        filters: &[Filter],
        total: usize,
    ) -> Result<Vec<u8>> {
        let rank = dims.len();
        if rank == 0 || chunk.len() != rank + 1 || chunk[rank] != elem as u64 {
            return Err(fmt_err("chunk dimensions do not match the dataspace"));
        }
        let cdims = &chunk[..rank];
        let celems = cdims
            .iter()
            .try_fold(1u64, |a, d| a.checked_mul(*d))
            .filter(|c| (1..=MAX_ELEMS).contains(c))
            .ok_or_else(|| fmt_err("chunk size out of range"))?;
        let chunk_bytes = celems as usize * elem;
        let mut out = vec![0u8; total];
        if self.is_undef(btree) {
            return Ok(out);
        }
        let mut entries = Vec::new();
        let mut budget = MAX_NODES;
        self.chunk_tree(btree, rank, &mut entries, 0, &mut budget, MAX_NODES)?;
        let mut work = 0u64;
        for (offsets, caddr, size, mask) in entries {
            work += celems;
            if work > 4 * MAX_ELEMS {
                return Err(fmt_err("too many chunk elements"));
            }
            let stored = self
                .bytes(caddr, usize::try_from(size).unwrap_or(usize::MAX))?
                .into_owned();
            let plain = unfilter(stored, filters, mask, elem, chunk_bytes)?;
            if plain.len() < chunk_bytes {
                return Err(fmt_err("chunk shorter than its extent"));
            }
            for lin in 0..celems as usize {
                let mut rem = lin;
                let mut dest = 0usize;
                let mut inside = true;
                let mut stride = 1usize;
                for ax in (0..rank).rev() {
                    let c = cdims[ax] as usize;
                    let idx = rem % c;
                    rem /= c;
                    let pos = (offsets[ax] as usize).saturating_add(idx);
                    if pos >= dims[ax] as usize {
                        inside = false;
                        break;
                    }
                    dest += pos * stride;
                    stride *= dims[ax] as usize;
                }
                if inside {
                    let (s, d) = (lin * elem, dest * elem);
                    out[d..d + elem].copy_from_slice(&plain[s..s + elem]);
                }
            }
        }
        Ok(out)
    }

    fn chunk_tree(
        &self,
        node: u64,
        rank: usize,
        out: &mut Vec<ChunkEntry>,
        depth: usize,
        budget: &mut usize,
        limit: usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(fmt_err("chunk B-tree too deep"));
        }
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| fmt_err("chunk B-tree too large"))?;
        let h = self.bytes(node, 8)?;
        if &h[..4] != b"TREE" || h[4] != 1 {
            return Err(fmt_err("chunk B-tree node malformed"));
        }
        let level = h[5];
        let used = usize::from(u16::from_le_bytes([h[6], h[7]]));
        let key_len = 8 + 8 * (rank as u64 + 1);
        let mut at = node + 8 + 2 * self.o as u64;
        for _ in 0..used {
            let size = self.uint(at, 4)?;
            let mask = self.uint(at + 4, 4)? as u32;
            let offsets = (0..rank)
                .map(|i| self.uint(at + 8 + 8 * i as u64, 8))
                .collect::<Result<Vec<u64>>>()?;
            let child = self.off(at + key_len)?;
            if level == 0 {
                if out.len() >= limit {
                    return Err(fmt_err("too many chunks"));
                }
                out.push((offsets, child, size, mask));
            } else {
                self.chunk_tree(child, rank, out, depth + 1, budget, limit)?;
            }
            at += key_len + self.o as u64;
        }
        Ok(())
    }
}

pub(crate) fn find(msgs: &[Msg], ty: u16) -> Option<&Msg> {
    msgs.iter().find(|m| m.ty == ty)
}

fn heap_name(names: &[u8], off: u64) -> Result<&[u8]> {
    let start = usize::try_from(off)
        .ok()
        .filter(|s| *s < names.len())
        .ok_or_else(|| fmt_err("heap name offset past the heap"))?;
    let tail = &names[start..];
    let end = tail
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| fmt_err("unterminated heap name"))?;
    Ok(&tail[..end])
}

pub(crate) fn datatype(b: &[u8]) -> Result<Dtype> {
    if b.len() < 8 {
        return Err(fmt_err("short datatype message"));
    }
    let class = b[0] & 0x0f;
    let bits = b[1];
    let size = le(&b[4..8]) as usize;
    if size == 0 {
        return Err(fmt_err("datatype of size 0"));
    }
    Ok(match class {
        0 => Dtype::Int {
            size,
            signed: bits & 0x08 != 0,
            big: bits & 0x01 != 0,
        },
        1 if matches!(size, 4 | 8) && bits & 0x40 == 0 => Dtype::Float {
            size,
            big: bits & 0x01 != 0,
        },
        3 => Dtype::Str { size },
        7 if bits & 0x0f == 0 => Dtype::Ref { size },
        _ => Dtype::Other { class, size },
    })
}

fn ordered(b: &[u8], big: bool) -> u64 {
    if big {
        b.iter().fold(0u64, |v, x| (v << 8) | u64::from(*x))
    } else {
        le(b)
    }
}

pub(crate) fn decode_u64(dtype: Dtype, raw: &[u8]) -> Result<Vec<u64>> {
    match dtype {
        Dtype::Int { size, big, .. } if matches!(size, 1 | 2 | 4 | 8) => {
            Ok(raw.chunks_exact(size).map(|c| ordered(c, big)).collect())
        }
        Dtype::Ref { size } if matches!(size, 1..=8) => {
            Ok(raw.chunks_exact(size).map(le).collect())
        }
        d => Err(unsupported(format!(
            "integer decoding of datatype class {} ({} byte)",
            d.class(),
            d.size()
        ))),
    }
}

pub(crate) fn decode_f64(dtype: Dtype, raw: &[u8]) -> Result<Vec<f64>> {
    match dtype {
        Dtype::Float { size: 8, big } => Ok(raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| f64::from_bits(ordered(c, big)))
            .collect()),
        Dtype::Float { size: 4, big } => Ok(raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f64::from(f32::from_bits(ordered(c, big) as u32)))
            .collect()),
        Dtype::Int { size, signed, big } if matches!(size, 1 | 2 | 4 | 8) => Ok(raw
            .chunks_exact(size)
            .map(|c| {
                let v = ordered(c, big);
                if signed {
                    let shift = 64 - 8 * size as u32;
                    (((v << shift) as i64) >> shift) as f64
                } else {
                    v as f64
                }
            })
            .collect()),
        d => Err(unsupported(format!(
            "numeric decoding of datatype class {} ({} byte)",
            d.class(),
            d.size()
        ))),
    }
}

fn filters(b: &[u8]) -> Result<Vec<Filter>> {
    let version = *b.first().unwrap_or(&0);
    let n = usize::from(*b.get(1).unwrap_or(&0));
    let short = || fmt_err("filter pipeline message too short");
    let mut at = match version {
        1 => 8,
        2 => 2,
        v => return Err(unsupported(format!("filter pipeline version {v}"))),
    };
    let mut out = Vec::new();
    for _ in 0..n {
        let id = le(b.get(at..at + 2).ok_or_else(short)?) as u16;
        at += 2;
        let mut name_len = 0usize;
        if version == 1 || id >= 256 {
            name_len = le(b.get(at..at + 2).ok_or_else(short)?) as usize;
            at += 2;
        }
        at += 2;
        let nparams = le(b.get(at..at + 2).ok_or_else(short)?) as usize;
        at += 2;
        at += if version == 1 {
            pad8(name_len)
        } else {
            name_len
        };
        let params = (0..nparams)
            .map(|i| {
                b.get(at + 4 * i..at + 4 * i + 4)
                    .map(|x| le(x) as u32)
                    .ok_or_else(short)
            })
            .collect::<Result<Vec<u32>>>()?;
        at += 4 * nparams;
        if version == 1 && nparams % 2 == 1 {
            at += 4;
        }
        out.push(Filter { id, params });
    }
    Ok(out)
}

fn unshuffle(buf: &[u8], elem: usize) -> Vec<u8> {
    let n = buf.len() / elem;
    let mut out = buf.to_vec();
    for i in 0..n {
        for b in 0..elem {
            out[i * elem + b] = buf[b * n + i];
        }
    }
    out
}

fn unfilter(
    mut buf: Vec<u8>,
    filters: &[Filter],
    mask: u32,
    elem: usize,
    limit: usize,
) -> Result<Vec<u8>> {
    for (i, f) in filters.iter().enumerate().rev() {
        if i < 32 && mask & (1 << i) != 0 {
            continue;
        }
        buf = match f.id {
            1 => crate::inflate::inflate_zlib_limited(&buf, limit)
                .map_err(|e| fmt_err(format!("deflate: {e}")))?,
            2 => {
                let sz = f.params.first().map_or(elem, |v| *v as usize).max(1);
                unshuffle(&buf, sz)
            }
            3 => {
                if buf.len() < 4 {
                    return Err(fmt_err("fletcher32 chunk shorter than its checksum"));
                }
                buf.truncate(buf.len() - 4);
                buf
            }
            id => return Err(unsupported(format!("filter id {id}"))),
        };
    }
    Ok(buf)
}

pub(crate) fn to_f32(dtype: Dtype, raw: &[u8], out: &mut Vec<f32>) -> Result<()> {
    out.clear();
    match dtype {
        Dtype::Float { size: 4, big } => out.extend(
            raw.as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_bits(ordered(c, big) as u32)),
        ),
        Dtype::Float { size: 8, big } => out.extend(
            raw.as_chunks::<8>()
                .0
                .iter()
                .map(|c| f64::from_bits(ordered(c, big)) as f32),
        ),
        Dtype::Int { size, signed, big } if matches!(size, 1 | 2 | 4 | 8) => {
            out.extend(raw.chunks_exact(size).map(|c| {
                let v = ordered(c, big);
                if signed {
                    let shift = 64 - 8 * size as u32;
                    (((v << shift) as i64) >> shift) as f32
                } else {
                    v as f32
                }
            }))
        }
        d => {
            return Err(unsupported(format!(
                "numeric decoding of datatype class {} ({} byte)",
                d.class(),
                d.size()
            )))
        }
    }
    Ok(())
}

const PAR_MIN_CHUNKS: usize = 32;
const BATCH_CHUNKS: usize = 128;
const MAX_SPAN: u64 = 256 << 20;

pub(crate) struct ChunkJob {
    pub(crate) o0: usize,
    pub(crate) o1: usize,
    pub(crate) addr: u64,
    pub(crate) stored: usize,
    pub(crate) mask: u32,
}

pub(crate) struct ChunkGeometry {
    pub(crate) c0: usize,
    pub(crate) c1: usize,
    pub(crate) chunk_bytes: usize,
    pub(crate) elem: usize,
    pub(crate) rows: usize,
    pub(crate) dtype: Dtype,
}

pub(crate) type ReadFn<'a> = dyn Fn(u64, usize) -> Result<Cow<'a, [u8]>> + Sync + 'a;

type Pieces = Vec<(usize, usize, Vec<f32>)>;

pub(crate) type ColumnVisit<'a, E> =
    dyn FnMut(usize, &[f32], usize) -> std::result::Result<(), E> + 'a;

pub(crate) struct ColumnPlan<'a> {
    pub(crate) cols: &'a [usize],
    pub(crate) rows: usize,
    pub(crate) chunk_len: usize,
    pub(crate) dtype: Dtype,
}

impl H5 {
    /// Streams columns of a 2-D numeric dataset as `f32`; see [`crate::MatFile::stream_numeric_f32`].
    pub(crate) fn stream_cols<E: From<crate::MatError>>(
        &self,
        msgs: &[Msg],
        space: &Space,
        dtype: Dtype,
        cols: &[usize],
        chunk_len: usize,
        visit: &mut ColumnVisit<'_, E>,
    ) -> std::result::Result<(), E> {
        if chunk_len == 0 {
            return Err(fmt_err("chunk_len must be positive").into());
        }
        let [ncols, rows] = space.dims.as_slice() else {
            return Err(unsupported("streaming needs a 2-D dataset").into());
        };
        let (ncols, rows) = (*ncols as usize, *rows as usize);
        if let Some(c) = cols.iter().find(|c| **c >= ncols) {
            return Err(fmt_err(format!("column {c} of a {rows} x {ncols} array")).into());
        }
        if cols.iter().enumerate().any(|(i, c)| cols[..i].contains(c)) {
            return Err(fmt_err("a column was requested twice").into());
        }
        let size = dtype.size();
        if !matches!(dtype, Dtype::Int { .. } | Dtype::Float { .. })
            || !matches!(size, 1 | 2 | 4 | 8)
        {
            return Err(unsupported(format!(
                "numeric decoding of datatype class {} ({size} byte)",
                dtype.class()
            ))
            .into());
        }
        if rows == 0 || cols.is_empty() {
            return Ok(());
        }
        let layout = self.layout(
            &find(msgs, 0x08)
                .ok_or_else(|| fmt_err("dataset without a layout"))?
                .body,
        )?;
        let filters = match find(msgs, 0x0b) {
            Some(m) => filters(&m.body)?,
            None => Vec::new(),
        };
        let plan = ColumnPlan {
            cols,
            rows,
            chunk_len,
            dtype,
        };
        let mut delivered = 0usize;
        let mut out: Vec<f32> = Vec::new();
        match layout {
            Layout::Compact(d) => {
                for &c in cols {
                    let from = c
                        .checked_mul(rows)
                        .and_then(|e| e.checked_mul(size))
                        .ok_or_else(|| fmt_err("offset overflow"))?;
                    let raw = d
                        .get(from..from + rows * size)
                        .ok_or_else(|| fmt_err("compact data shorter than the dataspace"))?;
                    delivered += emit(&plan, c, 0, raw, &mut out, visit)?;
                }
            }
            Layout::Contiguous { addr, size: n } => {
                if self.is_undef(addr) {
                    return Err(unsupported("dataset without allocated storage").into());
                }
                let total = (ncols as u64)
                    .checked_mul(rows as u64)
                    .and_then(|e| e.checked_mul(size as u64))
                    .ok_or_else(|| fmt_err("dataset too large"))?;
                if n < total {
                    return Err(fmt_err("contiguous data shorter than the dataspace").into());
                }
                for &c in cols {
                    let mut at = 0usize;
                    while at < rows {
                        let take = chunk_len.min(rows - at);
                        let pos = addr + ((c * rows + at) * size) as u64;
                        let raw = self.bytes(pos, take * size)?;
                        delivered += emit(&plan, c, at, &raw, &mut out, visit)?;
                        at += take;
                    }
                }
            }
            Layout::Chunked { chunk, btree } => {
                if chunk.len() != 3 || chunk[2] != size as u64 {
                    return Err(fmt_err("chunk dimensions do not match the dataspace").into());
                }
                let (c0, c1) = (chunk[0] as usize, chunk[1] as usize);
                let celems = (c0 as u64)
                    .checked_mul(c1 as u64)
                    .filter(|c| (1..=MAX_ELEMS).contains(c))
                    .ok_or_else(|| fmt_err("chunk size out of range"))?
                    as usize;
                let chunk_bytes = celems * size;
                if self.is_undef(btree) {
                    return Err(unsupported("dataset without allocated storage").into());
                }
                let mut entries = Vec::new();
                let mut budget = STREAM_MAX_CHUNKS;
                self.chunk_tree(btree, 2, &mut entries, 0, &mut budget, STREAM_MAX_CHUNKS)?;
                let jobs: Vec<ChunkJob> = entries
                    .into_iter()
                    .filter_map(|(offsets, addr, stored, mask)| {
                        let (o0, o1) = (offsets[0] as usize, offsets[1] as usize);
                        (o1 < rows && o0 < ncols && cols.iter().any(|c| (o0..o0 + c0).contains(c)))
                            .then_some(ChunkJob {
                                o0,
                                o1,
                                addr,
                                stored: usize::try_from(stored).unwrap_or(usize::MAX),
                                mask,
                            })
                    })
                    .collect();
                let geometry = ChunkGeometry {
                    c0,
                    c1,
                    chunk_bytes,
                    elem: size,
                    rows,
                    dtype,
                };
                let read = |pos: u64, n: usize| self.bytes(pos, n);
                delivered += stream_chunks(
                    &jobs,
                    &read,
                    &filters,
                    &geometry,
                    &plan,
                    PAR_MIN_CHUNKS,
                    visit,
                )?;
            }
        }
        if delivered != cols.len() * rows {
            return Err(unsupported("dataset with unallocated chunks").into());
        }
        Ok(())
    }
}

fn emit<E: From<crate::MatError>>(
    plan: &ColumnPlan<'_>,
    col: usize,
    first: usize,
    raw: &[u8],
    out: &mut Vec<f32>,
    visit: &mut ColumnVisit<'_, E>,
) -> std::result::Result<usize, E> {
    let size = plan.dtype.size();
    let mut done = 0usize;
    let n = raw.len() / size;
    debug_assert!(first + n <= plan.rows && plan.cols.contains(&col));
    while done < n {
        let take = plan.chunk_len.min(n - done);
        to_f32(plan.dtype, &raw[done * size..(done + take) * size], out)?;
        visit(col, out, first + done)?;
        done += take;
    }
    Ok(n)
}

fn decode_chunk(
    stored: Vec<u8>,
    job: &ChunkJob,
    filters: &[Filter],
    g: &ChunkGeometry,
    cols: &[usize],
) -> Result<Pieces> {
    let plain = unfilter(stored, filters, job.mask, g.elem, g.chunk_bytes)?;
    if plain.len() < g.chunk_bytes {
        return Err(fmt_err("chunk shorter than its extent"));
    }
    let width = g.c1.min(g.rows - job.o1);
    let mut out = Pieces::new();
    for &c in cols.iter().filter(|c| (job.o0..job.o0 + g.c0).contains(c)) {
        let from = (c - job.o0) * g.c1 * g.elem;
        let mut values = Vec::new();
        to_f32(g.dtype, &plain[from..from + width * g.elem], &mut values)?;
        out.push((c, job.o1, values));
    }
    Ok(out)
}

fn read_batch(jobs: &[ChunkJob], read: &ReadFn<'_>) -> Vec<Result<Vec<u8>>> {
    let single = |j: &ChunkJob| read(j.addr, j.stored).map(Cow::into_owned);
    let lo = jobs.iter().map(|j| j.addr).min();
    let hi = jobs
        .iter()
        .map(|j| j.addr.saturating_add(j.stored as u64))
        .max();
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return Vec::new();
    };
    let wanted: u64 = jobs.iter().map(|j| j.stored as u64).sum();
    let span = hi - lo;
    if jobs.len() > 1 && span <= MAX_SPAN && span <= wanted.saturating_mul(2) + (1 << 16) {
        if let Ok(block) = read(lo, span as usize) {
            return jobs
                .iter()
                .map(|j| {
                    let from = (j.addr - lo) as usize;
                    block
                        .get(from..from + j.stored)
                        .map(<[u8]>::to_vec)
                        .ok_or_else(|| fmt_err("chunk outside its read block"))
                })
                .collect();
        }
    }
    jobs.iter().map(single).collect()
}

fn deliver<E: From<crate::MatError>>(
    pieces: Pieces,
    plan: &ColumnPlan<'_>,
    visit: &mut ColumnVisit<'_, E>,
) -> std::result::Result<usize, E> {
    let mut done = 0usize;
    for (col, first, values) in pieces {
        let mut at = 0usize;
        while at < values.len() {
            let take = plan.chunk_len.min(values.len() - at);
            visit(col, &values[at..at + take], first + at)?;
            at += take;
        }
        done += values.len();
    }
    Ok(done)
}

pub(crate) fn stream_chunks<E: From<crate::MatError>>(
    jobs: &[ChunkJob],
    read: &ReadFn<'_>,
    filters: &[Filter],
    g: &ChunkGeometry,
    plan: &ColumnPlan<'_>,
    parallel_min: usize,
    visit: &mut ColumnVisit<'_, E>,
) -> std::result::Result<usize, E> {
    use rayon::prelude::*;

    let mut delivered = 0usize;
    if jobs.len() < parallel_min || rayon::current_num_threads() < 2 {
        for job in jobs {
            let stored = read(job.addr, job.stored)?.into_owned();
            let pieces = decode_chunk(stored, job, filters, g, plan.cols)?;
            delivered += deliver(pieces, plan, visit)?;
        }
        return Ok(delivered);
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<Result<Vec<u8>>>>(3);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for batch in jobs.chunks(BATCH_CHUNKS) {
                if tx.send(read_batch(batch, read)).is_err() {
                    return;
                }
            }
        });
        let outcome = (|| -> std::result::Result<(), E> {
            let mut next = 0usize;
            while let Ok(batch) = rx.recv() {
                let batch_jobs = &jobs[next..next + batch.len()];
                next += batch.len();
                let decoded: Vec<Result<Pieces>> = batch
                    .into_par_iter()
                    .zip(batch_jobs.par_iter())
                    .map(|(stored, job)| decode_chunk(stored?, job, filters, g, plan.cols))
                    .collect();
                for pieces in decoded {
                    delivered += deliver(pieces?, plan, visit)?;
                }
            }
            Ok(())
        })();
        drop(rx);
        outcome?;
        Ok(delivered)
    })
}

#[cfg(test)]
mod chunk_tests {
    use std::io::Write;

    use super::*;

    type RunOutput = (
        std::result::Result<usize, crate::MatError>,
        Vec<(usize, usize, Vec<f32>)>,
    );

    const COLS: usize = 4;
    const C1: usize = 50;

    fn value(col: usize, row: usize) -> f64 {
        (col as f64 + 1.0) * 1000.0 + row as f64 * 0.37
    }

    fn build(rows: usize) -> (Vec<u8>, Vec<ChunkJob>) {
        let mut file = vec![0xEEu8; 17];
        let mut jobs = Vec::new();
        let mut o1 = 0;
        while o1 < rows {
            let mut plain = Vec::new();
            for c in 0..COLS {
                for r in o1..o1 + C1 {
                    plain.extend_from_slice(&value(c, r).to_le_bytes());
                }
            }
            let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(3));
            enc.write_all(&plain).unwrap();
            let stored = enc.finish().unwrap();
            jobs.push(ChunkJob {
                o0: 0,
                o1,
                addr: file.len() as u64,
                stored: stored.len(),
                mask: 0,
            });
            file.extend_from_slice(&stored);
            file.extend_from_slice(&[0u8; 5]);
            o1 += C1;
        }
        (file, jobs)
    }

    fn run(
        file: &[u8],
        jobs: &[ChunkJob],
        rows: usize,
        chunk_len: usize,
        parallel_min: usize,
    ) -> RunOutput {
        let read = |pos: u64, n: usize| -> Result<Cow<'_, [u8]>> {
            file.get(pos as usize..pos as usize + n)
                .map(Cow::Borrowed)
                .ok_or_else(|| fmt_err("past end"))
        };
        let filters = vec![Filter {
            id: 1,
            params: Vec::new(),
        }];
        let dtype = Dtype::Float {
            size: 8,
            big: false,
        };
        let g = ChunkGeometry {
            c0: COLS,
            c1: C1,
            chunk_bytes: COLS * C1 * 8,
            elem: 8,
            rows,
            dtype,
        };
        let cols = [0usize, 1, 2, 3];
        let plan = ColumnPlan {
            cols: &cols,
            rows,
            chunk_len,
            dtype,
        };
        let mut got = Vec::new();
        let res = stream_chunks(
            jobs,
            &read,
            &filters,
            &g,
            &plan,
            parallel_min,
            &mut |c, piece: &[f32], first| -> std::result::Result<(), crate::MatError> {
                got.push((c, first, piece.to_vec()));
                Ok(())
            },
        );
        (res, got)
    }

    #[test]
    fn parallel_matches_sequential_bit_for_bit() {
        let rows = C1 * 700;
        let (file, jobs) = build(rows);
        for chunk_len in [7usize, 50, 4096] {
            let (a, seq) = run(&file, &jobs, rows, chunk_len, usize::MAX);
            let (b, par) = run(&file, &jobs, rows, chunk_len, 1);
            assert_eq!(a.unwrap(), COLS * rows);
            assert_eq!(b.unwrap(), COLS * rows);
            assert_eq!(seq, par);
        }
        let (_, par) = run(&file, &jobs, rows, 50, 1);
        for (c, first, piece) in par {
            for (i, v) in piece.iter().enumerate() {
                assert_eq!(v.to_bits(), (value(c, first + i) as f32).to_bits());
            }
        }
    }

    #[test]
    fn the_first_failing_chunk_in_order_is_reported() {
        let rows = C1 * 700;
        let (mut file, jobs) = build(rows);
        for bad in [400usize, 90] {
            let at = jobs[bad].addr as usize;
            file[at + 2] ^= 0xFF;
            file[at + 3] ^= 0xFF;
        }
        let (a, _) = run(&file, &jobs, rows, 50, usize::MAX);
        let (b, par) = run(&file, &jobs, rows, 50, 1);
        let (a, b) = (a.unwrap_err().to_string(), b.unwrap_err().to_string());
        assert_eq!(a, b);
        assert!(par.iter().all(|(_, first, _)| *first < 90 * C1));
    }
}
