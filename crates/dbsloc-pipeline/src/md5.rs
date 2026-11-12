// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! MD5 (RFC 1321), used only to verify the digests that Dataverse publishes per file.

use std::sync::LazyLock;

const SHIFTS: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

static K: LazyLock<[u32; 64]> = LazyLock::new(|| {
    let mut k = [0u32; 64];
    for (i, v) in k.iter_mut().enumerate() {
        *v = (((i + 1) as f64).sin().abs() * 4_294_967_296.0) as u32;
    }
    k
});

pub struct Md5 {
    state: [u32; 4],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    pub fn new() -> Self {
        Md5 {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buf: [0; 64],
            buf_len: 0,
            total: 0,
        }
    }

    fn compress(state: &mut [u32; 4], block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            m[i] = u32::from_le_bytes(*chunk);
        }
        let k = &*K;
        let [mut a, mut b, mut c, mut d] = *state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let t = a
                .wrapping_add(f)
                .wrapping_add(k[i])
                .wrapping_add(m[g])
                .rotate_left(SHIFTS[i]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(t);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                Self::compress(&mut self.state, &block);
                self.buf_len = 0;
            }
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks {
            Self::compress(&mut self.state, block);
        }
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len += rest.len();
    }

    pub fn finish(mut self) -> [u8; 16] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        let used = (self.buf_len + 1) % 64;
        let zeros = if used <= 56 { 56 - used } else { 120 - used };
        pad.extend(std::iter::repeat_n(0u8, zeros));
        pad.extend_from_slice(&bits.to_le_bytes());
        self.update(&pad);
        let mut out = [0u8; 16];
        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *o = s.to_le_bytes();
        }
        out
    }
}

/// Lowercase hex MD5 of `data`.
pub fn md5_hex(data: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(data);
    crate::sha256::hex(&h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn split_updates_match_one_shot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let mut h = Md5::new();
        for c in data.chunks(37) {
            h.update(c);
        }
        assert_eq!(crate::sha256::hex(&h.finish()), md5_hex(&data));
    }
}
