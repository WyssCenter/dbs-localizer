// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::io::{self, Write};

use flate2::{Compress, Compression, FlushCompress, Status};
use rayon::prelude::*;

pub const BLOCK_SIZE: usize = 1 << 20;
const BATCH_BYTES: usize = 16 * BLOCK_SIZE;
const GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];

struct Block {
    deflated: Vec<u8>,
    crc: u32,
    len: u64,
}

fn deflate_block(level: u32, data: &[u8], last: bool) -> io::Result<Block> {
    let mut c = Compress::new(Compression::new(level.min(9)), false);
    let mut out = Vec::with_capacity(data.len() / 2 + 1024);
    let flush = if last {
        FlushCompress::Finish
    } else {
        FlushCompress::Sync
    };
    loop {
        if out.capacity() - out.len() < 4096 {
            out.reserve(64 * 1024);
        }
        let consumed = c.total_in() as usize;
        let status = c
            .compress_vec(&data[consumed..], &mut out, flush)
            .map_err(io::Error::other)?;
        let all_in = c.total_in() as usize == data.len();
        let spare = out.capacity() > out.len();
        match (last, status) {
            (true, Status::StreamEnd) => break,
            (false, _) if all_in && spare => break,
            _ => {}
        }
    }
    Ok(Block {
        deflated: out,
        crc: crc32fast::hash(data),
        len: data.len() as u64,
    })
}

pub struct ParGzWriter<W: Write> {
    inner: W,
    level: u32,
    pending: Vec<u8>,
    crc: u32,
    total: u64,
    started: bool,
}

impl<W: Write> ParGzWriter<W> {
    pub fn new(inner: W, level: u32) -> Self {
        Self {
            inner,
            level,
            pending: Vec::new(),
            crc: 0,
            total: 0,
            started: false,
        }
    }

    fn start(&mut self) -> io::Result<()> {
        if !self.started {
            self.inner.write_all(&GZIP_HEADER)?;
            self.started = true;
        }
        Ok(())
    }

    fn emit(&mut self, blocks: Vec<Block>) -> io::Result<()> {
        self.start()?;
        for b in blocks {
            self.inner.write_all(&b.deflated)?;
            let mut h = crc32fast::Hasher::new_with_initial_len(self.crc, self.total);
            h.combine(&crc32fast::Hasher::new_with_initial_len(b.crc, b.len));
            self.crc = h.finalize();
            self.total += b.len;
        }
        Ok(())
    }

    fn compress_full_blocks(&mut self, data: &[u8]) -> io::Result<()> {
        let level = self.level;
        let blocks = data
            .par_chunks(BLOCK_SIZE)
            .map(|c| deflate_block(level, c, false))
            .collect::<io::Result<Vec<_>>>()?;
        self.emit(blocks)
    }

    fn drain_pending(&mut self) -> io::Result<()> {
        let n = self.pending.len() / BLOCK_SIZE * BLOCK_SIZE;
        if n == 0 {
            return Ok(());
        }
        let mut buf = std::mem::take(&mut self.pending);
        let result = self.compress_full_blocks(&buf[..n]);
        buf.drain(..n);
        self.pending = buf;
        result
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.drain_pending()?;
        let tail = std::mem::take(&mut self.pending);
        let last = deflate_block(self.level, &tail, true)?;
        self.emit(vec![last])?;
        self.inner.write_all(&self.crc.to_le_bytes())?;
        self.inner.write_all(&(self.total as u32).to_le_bytes())?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for ParGzWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.pending.is_empty() && data.len() >= BLOCK_SIZE {
            let n = data.len() / BLOCK_SIZE * BLOCK_SIZE;
            self.compress_full_blocks(&data[..n])?;
            self.pending.extend_from_slice(&data[n..]);
        } else {
            self.pending.extend_from_slice(data);
            if self.pending.len() >= BATCH_BYTES {
                self.drain_pending()?;
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn gzip_parallel(data: &[u8], level: u32) -> io::Result<Vec<u8>> {
    let mut w = ParGzWriter::new(Vec::with_capacity(data.len() / 2 + 64), level);
    w.write_all(data)?;
    w.finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::io::Read;

    use flate2::read::GzDecoder;

    use super::*;

    fn pseudo(n: usize) -> Vec<u8> {
        let mut s = 0x9e3779b97f4a7c15u64;
        (0..n)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                if i % 3 == 0 {
                    (s >> 56) as u8
                } else {
                    (i / 64) as u8
                }
            })
            .collect()
    }

    fn single_member_decode(gz: &[u8]) -> Vec<u8> {
        let mut d = GzDecoder::new(gz);
        let mut out = Vec::new();
        d.read_to_end(&mut out).unwrap();
        let rest = d.into_inner();
        assert!(rest.is_empty(), "{} trailing byte(s)", rest.len());
        out
    }

    #[test]
    fn empty_and_one_byte_payloads_decode() {
        for level in [1, 6] {
            assert!(single_member_decode(&gzip_parallel(&[], level).unwrap()).is_empty());
            assert_eq!(
                single_member_decode(&gzip_parallel(&[7], level).unwrap()),
                [7]
            );
        }
    }

    #[test]
    fn multi_block_payloads_decode_as_one_member() {
        for n in [
            BLOCK_SIZE - 1,
            BLOCK_SIZE,
            BLOCK_SIZE + 1,
            3 * BLOCK_SIZE,
            5 * BLOCK_SIZE + 12345,
        ] {
            let data = pseudo(n);
            let gz = gzip_parallel(&data, 1).unwrap();
            assert_eq!(single_member_decode(&gz), data, "n = {n}");
        }
    }

    #[test]
    fn many_small_writes_equal_one_big_write() {
        let data = pseudo(40 * BLOCK_SIZE + 17);
        let whole = gzip_parallel(&data, 1).unwrap();
        let mut w = ParGzWriter::new(Vec::new(), 1);
        for chunk in data.chunks(777_777) {
            w.write_all(chunk).unwrap();
        }
        let parts = w.finish().unwrap();
        assert_eq!(single_member_decode(&parts), data);
        assert_eq!(parts, whole);
    }

    #[test]
    fn compresses_repetitive_data() {
        let data = vec![0u8; 8 * BLOCK_SIZE];
        let gz = gzip_parallel(&data, 1).unwrap();
        assert!(gz.len() < data.len() / 100);
        assert_eq!(single_member_decode(&gz), data);
    }
}
