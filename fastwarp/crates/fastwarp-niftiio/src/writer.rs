// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use bytemuck::Pod;
use ndarray::{ArrayBase, Axis, Data, Dimension, RemoveAxis};
use nifti::{DataElement, Endianness, NiftiHeader, NiftiType};
use rayon::prelude::*;

use crate::error::NiftiFileError;
use crate::gzip::ParGzWriter;

const DEFAULT_LEVEL: u32 = 1;
const BATCH_BYTES: usize = 16 << 20;
const MAGIC_SINGLE_FILE: [u8; 4] = *b"n+1\0";

#[derive(Clone, Debug)]
pub struct WriterOptions<'a> {
    path: PathBuf,
    header: Option<&'a NiftiHeader>,
    level: Option<u32>,
}

impl<'a> WriterOptions<'a> {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let mut path = path.as_ref().to_owned();
        if path.extension().is_none() {
            let _ = path.set_extension("nii");
        }
        let gz = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".gz"));
        Self {
            path,
            header: None,
            level: gz.then_some(DEFAULT_LEVEL),
        }
    }

    pub fn reference_header(mut self, header: &'a NiftiHeader) -> Self {
        self.header = Some(header);
        self
    }

    pub fn compression_level(mut self, level: u32) -> Self {
        self.level = Some(level);
        self
    }

    pub fn write_nifti<A, S, D>(&self, data: &ArrayBase<S, D>) -> Result<(), NiftiFileError>
    where
        S: Data<Elem = A> + Sync,
        A: DataElement + Pod + Default + Send + Sync,
        D: Dimension + RemoveAxis,
    {
        self.write_nifti_with_type(data, A::DATA_TYPE)
    }

    pub fn write_nifti_with_type<A, S, D>(
        &self,
        data: &ArrayBase<S, D>,
        datatype: NiftiType,
    ) -> Result<(), NiftiFileError>
    where
        S: Data<Elem = A> + Sync,
        A: Pod + Default + Send + Sync,
        D: Dimension + RemoveAxis,
    {
        if std::mem::size_of::<A>() != datatype.size_of() {
            return Err(NiftiFileError::UnsupportedDataType(datatype));
        }
        let header = self.prepare_header(data.shape(), datatype)?;
        let file = File::create(&self.path)?;
        match self.level {
            Some(level) => {
                let mut w = ParGzWriter::new(BufWriter::new(file), level);
                write_all(&mut w, &header, data)?;
                w.finish()?.flush()?;
            }
            None => {
                let mut w = BufWriter::with_capacity(1 << 20, file);
                write_all(&mut w, &header, data)?;
                w.flush()?;
            }
        }
        Ok(())
    }

    fn prepare_header(
        &self,
        shape: &[usize],
        datatype: NiftiType,
    ) -> Result<NiftiHeader, NiftiFileError> {
        if shape.is_empty() || shape.len() > 7 {
            return Err(NiftiFileError::Write(format!(
                "cannot write a {}-dimensional array",
                shape.len()
            )));
        }
        let mut dim = [1u16; 8];
        dim[0] = shape.len() as u16;
        for (slot, &n) in dim[1..].iter_mut().zip(shape) {
            *slot = u16::try_from(n)
                .map_err(|_| NiftiFileError::Write(format!("dimension {n} exceeds 65535")))?;
        }
        let mut header = NiftiHeader {
            dim,
            sizeof_hdr: 348,
            datatype: datatype as i16,
            bitpix: (datatype.size_of() * 8) as i16,
            vox_offset: 352.0,
            scl_inter: 0.0,
            scl_slope: 1.0,
            magic: MAGIC_SINGLE_FILE,
            ..self.header.cloned().unwrap_or_else(|| NiftiHeader {
                sform_code: 2,
                ..NiftiHeader::default()
            })
        };
        header.descrip.resize(80, 0);
        Ok(header)
    }
}

fn write_all<W: Write, A, S, D>(
    w: &mut W,
    header: &NiftiHeader,
    data: &ArrayBase<S, D>,
) -> Result<(), NiftiFileError>
where
    S: Data<Elem = A> + Sync,
    A: Pod + Default + Send + Sync,
    D: Dimension + RemoveAxis,
{
    w.write_all(&header_bytes(header))?;
    w.write_all(&[0u8; 4])?;
    let swap = header.endianness != Endianness::native();
    write_data(w, data, swap)
}

fn write_data<W: Write, A, S, D>(
    w: &mut W,
    data: &ArrayBase<S, D>,
    swap: bool,
) -> Result<(), NiftiFileError>
where
    S: Data<Elem = A> + Sync,
    A: Pod + Default + Send + Sync,
    D: Dimension + RemoveAxis,
{
    let total = data.len();
    if total == 0 {
        return Ok(());
    }
    let axis = data.shape().iter().rposition(|&n| n > 1).unwrap_or(0);
    let slabs = data.shape()[axis];
    let slab_len = total / slabs;
    let size = std::mem::size_of::<A>();
    let per_batch = (BATCH_BYTES / (slab_len * size).max(1)).clamp(1, slabs);
    let mut buf: Vec<A> = vec![A::default(); per_batch * slab_len];
    let mut start = 0;
    while start < slabs {
        let n = per_batch.min(slabs - start);
        let chunk = &mut buf[..n * slab_len];
        chunk
            .par_chunks_mut(slab_len)
            .enumerate()
            .for_each(|(s, dst)| {
                let view = data.index_axis(Axis(axis), start + s);
                for (o, v) in dst.iter_mut().zip(view.t().iter()) {
                    *o = *v;
                }
            });
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(chunk);
        if swap && size > 1 {
            bytes.par_chunks_mut(size).for_each(|c| c.reverse());
        }
        w.write_all(bytes)?;
        start += n;
    }
    Ok(())
}

fn header_bytes(h: &NiftiHeader) -> [u8; 348] {
    let big = h.endianness == Endianness::Big;
    let mut out = [0u8; 348];
    let mut pos = 0usize;
    macro_rules! put {
        ($v:expr) => {{
            let b = if big {
                $v.to_be_bytes()
            } else {
                $v.to_le_bytes()
            };
            out[pos..pos + b.len()].copy_from_slice(&b);
            pos += b.len();
        }};
    }
    macro_rules! raw {
        ($v:expr) => {{
            let n = $v.len();
            out[pos..pos + n].copy_from_slice(&$v);
            pos += n;
        }};
    }
    put!(h.sizeof_hdr);
    raw!(h.data_type);
    raw!(h.db_name);
    put!(h.extents);
    put!(h.session_error);
    raw!([h.regular]);
    raw!([h.dim_info]);
    for v in h.dim {
        put!(v);
    }
    put!(h.intent_p1);
    put!(h.intent_p2);
    put!(h.intent_p3);
    put!(h.intent_code);
    put!(h.datatype);
    put!(h.bitpix);
    put!(h.slice_start);
    for v in h.pixdim {
        put!(v);
    }
    put!(h.vox_offset);
    put!(h.scl_slope);
    put!(h.scl_inter);
    put!(h.slice_end);
    raw!([h.slice_code]);
    raw!([h.xyzt_units]);
    put!(h.cal_max);
    put!(h.cal_min);
    put!(h.slice_duration);
    put!(h.toffset);
    put!(h.glmax);
    put!(h.glmin);
    raw!(h.descrip[..80]);
    raw!(h.aux_file);
    put!(h.qform_code);
    put!(h.sform_code);
    for v in [
        h.quatern_b,
        h.quatern_c,
        h.quatern_d,
        h.quatern_x,
        h.quatern_y,
        h.quatern_z,
    ] {
        put!(v);
    }
    for v in h.srow_x.iter().chain(&h.srow_y).chain(&h.srow_z) {
        put!(*v);
    }
    raw!(h.intent_name);
    raw!(h.magic);
    debug_assert_eq!(pos, 348);
    out
}
