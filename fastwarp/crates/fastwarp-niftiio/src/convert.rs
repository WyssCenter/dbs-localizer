// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use nifti::NiftiType;
use rayon::prelude::*;

use crate::error::NiftiFileError;

const MIN_PAR_LEN: usize = 1 << 14;
const TILE: usize = 16;

pub trait Elem: Copy + Send + Sync {
    const SIZE: usize;
    fn load(bytes: &[u8], big_endian: bool) -> Self;
    fn to_f64(self) -> f64;
    fn to_f32(self) -> f32;
}

macro_rules! impl_elem {
    ($($t:ty),*) => {$(
        impl Elem for $t {
            const SIZE: usize = std::mem::size_of::<$t>();
            #[inline(always)]
            fn load(bytes: &[u8], big_endian: bool) -> Self {
                let mut a = [0u8; std::mem::size_of::<$t>()];
                a.copy_from_slice(&bytes[..std::mem::size_of::<$t>()]);
                if big_endian {
                    <$t>::from_be_bytes(a)
                } else {
                    <$t>::from_le_bytes(a)
                }
            }
            #[inline(always)]
            fn to_f64(self) -> f64 {
                self as f64
            }
            #[inline(always)]
            fn to_f32(self) -> f32 {
                self as f32
            }
        }
    )*};
}

impl_elem!(u8, i8, u16, i16, u32, i32, u64, i64, f32, f64);

macro_rules! with_elem {
    ($dt:expr, $e:ident => $body:expr) => {
        match $dt {
            NiftiType::Uint8 => {
                type $e = u8;
                $body
            }
            NiftiType::Int8 => {
                type $e = i8;
                $body
            }
            NiftiType::Uint16 => {
                type $e = u16;
                $body
            }
            NiftiType::Int16 => {
                type $e = i16;
                $body
            }
            NiftiType::Uint32 => {
                type $e = u32;
                $body
            }
            NiftiType::Int32 => {
                type $e = i32;
                $body
            }
            NiftiType::Uint64 => {
                type $e = u64;
                $body
            }
            NiftiType::Int64 => {
                type $e = i64;
                $body
            }
            NiftiType::Float32 => {
                type $e = f32;
                $body
            }
            NiftiType::Float64 => {
                type $e = f64;
                $body
            }
            other => Err(NiftiFileError::UnsupportedDataType(other)),
        }
    };
}

pub fn scalar_size(datatype: NiftiType) -> Option<usize> {
    match datatype {
        NiftiType::Uint8 | NiftiType::Int8 => Some(1),
        NiftiType::Uint16 | NiftiType::Int16 => Some(2),
        NiftiType::Uint32 | NiftiType::Int32 | NiftiType::Float32 => Some(4),
        NiftiType::Uint64 | NiftiType::Int64 | NiftiType::Float64 => Some(8),
        _ => None,
    }
}

fn check_len(bytes: &[u8], count: usize, size: usize) -> Result<(), NiftiFileError> {
    let need = count
        .checked_mul(size)
        .ok_or(NiftiFileError::BadVolumeSize)?;
    if bytes.len() < need {
        return Err(NiftiFileError::IncompatibleLength {
            got: bytes.len(),
            want: need,
        });
    }
    Ok(())
}

fn flat<E: Elem, T: Send>(
    bytes: &[u8],
    big: bool,
    count: usize,
    conv: &(impl Fn(E) -> T + Sync),
) -> Vec<T> {
    bytes[..count * E::SIZE]
        .par_chunks_exact(E::SIZE)
        .with_min_len(MIN_PAR_LEN)
        .map(|c| conv(E::load(c, big)))
        .collect()
}

fn c_order<E: Elem, T: Send + Copy + Default>(
    bytes: &[u8],
    big: bool,
    dims: &[usize],
    conv: &(impl Fn(E) -> T + Sync),
) -> Vec<T> {
    let count: usize = dims.iter().product();
    let mut out = vec![T::default(); count];
    if count == 0 {
        return out;
    }
    if dims.len() > 4 {
        let nd = dims.len();
        let mut sf = vec![1usize; nd];
        for a in 1..nd {
            sf[a] = sf[a - 1] * dims[a - 1];
        }
        let inner = count / dims[0];
        out.par_chunks_mut(inner)
            .enumerate()
            .for_each(|(i0, chunk)| {
                let mut idx = vec![0usize; nd];
                idx[0] = i0;
                for o in chunk.iter_mut() {
                    let off: usize = idx.iter().zip(&sf).map(|(i, s)| i * s).sum();
                    *o = conv(E::load(&bytes[off * E::SIZE..], big));
                    let mut a = nd - 1;
                    while a > 0 {
                        idx[a] += 1;
                        if idx[a] < dims[a] {
                            break;
                        }
                        idx[a] = 0;
                        a -= 1;
                    }
                }
            });
        return out;
    }
    let mut d = [1usize; 4];
    d[..dims.len()].copy_from_slice(dims);
    let [n0, n1, n2, rest] = d;
    out.par_chunks_mut(TILE * n1 * n2 * rest)
        .enumerate()
        .for_each(|(ib, chunk)| {
            let i0 = ib * TILE;
            let ilen = chunk.len() / (n1 * n2 * rest);
            for j in 0..n1 {
                for k0 in (0..n2).step_by(TILE) {
                    let klen = TILE.min(n2 - k0);
                    for ii in 0..ilen {
                        for kk in 0..klen {
                            let k = k0 + kk;
                            let dst = ((ii * n1 + j) * n2 + k) * rest;
                            for c in 0..rest {
                                let src = i0 + ii + n0 * (j + n1 * (k + n2 * c));
                                chunk[dst + c] = conv(E::load(&bytes[src * E::SIZE..], big));
                            }
                        }
                    }
                }
            }
        });
    out
}

pub fn decode_map<T: Send>(
    bytes: &[u8],
    datatype: NiftiType,
    big_endian: bool,
    count: usize,
    f: impl Fn(f64) -> T + Sync,
) -> Result<Vec<T>, NiftiFileError> {
    with_elem!(datatype, E => {
        check_len(bytes, count, E::SIZE)?;
        Ok(flat::<E, T>(bytes, big_endian, count, &|v: E| f(v.to_f64())))
    })
}

pub fn decode_f32(
    bytes: &[u8],
    datatype: NiftiType,
    big_endian: bool,
    count: usize,
    scale: Option<(f32, f32)>,
) -> Result<Vec<f32>, NiftiFileError> {
    with_elem!(datatype, E => {
        check_len(bytes, count, E::SIZE)?;
        Ok(match scale {
            Some((slope, inter)) => {
                flat::<E, f32>(bytes, big_endian, count, &|v: E| v.to_f32() * slope + inter)
            }
            None => flat::<E, f32>(bytes, big_endian, count, &|v: E| v.to_f32()),
        })
    })
}

pub fn decode_map_c_order<T: Send + Copy + Default>(
    bytes: &[u8],
    datatype: NiftiType,
    big_endian: bool,
    dims: &[usize],
    f: impl Fn(f64) -> T + Sync,
) -> Result<Vec<T>, NiftiFileError> {
    let count = dims
        .iter()
        .try_fold(1usize, |a, &d| a.checked_mul(d))
        .ok_or(NiftiFileError::BadVolumeSize)?;
    with_elem!(datatype, E => {
        check_len(bytes, count, E::SIZE)?;
        Ok(c_order::<E, T>(bytes, big_endian, dims, &|v: E| f(v.to_f64())))
    })
}
