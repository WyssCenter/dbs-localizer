// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! WP3: chunked resumable uploads, sha256, NIfTI validation + header scrub, and the convert and
//! commit steps, which delegate DICOM conversion and BIDS placement to `dbsloc_pipeline::import`.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::Extension;
use axum::Json;
use dbsloc_api::{
    BidsMappingEntry, ConvertedSeries, UploadCommitRequest, UploadFileRequest, UploadInitRequest,
    UploadInitResponse,
};
use dbsloc_pipeline::bids::BidsError;
use dbsloc_pipeline::import::{
    self, Acq, ImportError, ImportItem, Session, E_DCM2NIIX as IMPORT_E_DCM2NIIX,
    E_IMPORT_CANCELLED, E_IMPORT_FILE, E_IMPORT_RULES,
};
use nifti::NiftiHeader;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::AuditEntry;
use crate::error::{
    ServerErr, E_BAD_KEY, E_BAD_SUBJECT, E_DCM2NIIX, E_IMPORT_CONFLICT, E_NIFTI, E_RUN_ACTIVE,
    E_UPLOAD,
};
use crate::files::{sanitize_key, valid_dataset_name};
use crate::middleware::ClientAddr;
use crate::routes::blocking;
use crate::state::AppState;

const CONVERTED_FILE: &str = "converted.json";
const MAX_SPATIAL_DIM: u16 = 2048;
const MAX_EXTRA_DIM: u16 = 4096;

/// Persisted upload manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadManifest {
    pub dataset: String,
    pub chunk_size: u64,
    pub files: Vec<UploadFileRequest>,
}

type FileStamp = (u64, std::time::SystemTime);

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

pub struct StagedUpload {
    pub manifest: UploadManifest,
    index: HashMap<String, usize>,
    stamp: Option<FileStamp>,
}

impl StagedUpload {
    fn new(manifest: UploadManifest, stamp: Option<FileStamp>) -> StagedUpload {
        let index = manifest
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.clone(), i))
            .collect();
        StagedUpload {
            manifest,
            index,
            stamp,
        }
    }

    pub fn file(&self, name: &str) -> Option<&UploadFileRequest> {
        self.index
            .get(name)
            .and_then(|&i| self.manifest.files.get(i))
    }
}

const UPLOAD_CACHE_CAP: usize = 256;

#[derive(Default)]
pub struct UploadCache {
    map: Mutex<HashMap<String, Arc<StagedUpload>>>,
}

impl UploadCache {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<StagedUpload>>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, id: &str) -> Option<Arc<StagedUpload>> {
        self.map().get(id).cloned()
    }

    pub fn insert(&self, id: &str, upload: Arc<StagedUpload>) {
        let mut map = self.map();
        if map.len() >= UPLOAD_CACHE_CAP && !map.contains_key(id) {
            map.clear();
        }
        map.insert(id.to_owned(), upload);
    }

    pub fn remove(&self, id: &str) {
        self.map().remove(id);
    }

    pub fn len(&self) -> usize {
        self.map().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

const STALE_UPLOAD_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

fn newest_mtime(path: &Path) -> Option<std::time::SystemTime> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mut newest = meta.modified().ok();
    if meta.is_dir() {
        for e in std::fs::read_dir(path).ok()?.flatten() {
            if let Some(t) = newest_mtime(&e.path()) {
                newest = Some(newest.map_or(t, |n| n.max(t)));
            }
        }
    }
    newest
}

pub fn sweep_stale_uploads(incoming: &Path) {
    let Ok(rd) = std::fs::read_dir(incoming) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in rd.flatten() {
        let path = e.path();
        let stale = newest_mtime(&path)
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > STALE_UPLOAD_AGE);
        if stale {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

fn upload_dir(state: &AppState, id: &str) -> PathBuf {
    state.config().incoming_dir().join(id)
}

fn manifest_path(state: &AppState, id: &str) -> PathBuf {
    upload_dir(state, id).join("upload.json")
}

fn validate_upload_id(id: &str) -> Result<(), ServerErr> {
    let ok = id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(ServerErr::bad_request(E_UPLOAD, "invalid upload id"))
    }
}

fn read_manifest(state: &AppState, id: &str) -> Result<Arc<StagedUpload>, ServerErr> {
    validate_upload_id(id)?;
    let path = manifest_path(state, id);
    let stamp = file_stamp(&path);
    if let Some(hit) = state.uploads().get(id) {
        if stamp.is_some() && hit.stamp == stamp {
            return Ok(hit);
        }
        state.uploads().remove(id);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|_| ServerErr::not_found(format!("no upload {id}")))?;
    let manifest: UploadManifest = serde_json::from_str(&text)
        .map_err(|e| ServerErr::bad_request(E_UPLOAD, format!("bad upload manifest: {e}")))?;
    let staged = Arc::new(StagedUpload::new(manifest, stamp));
    state.uploads().insert(id, Arc::clone(&staged));
    Ok(staged)
}

/// An announced file name: a sanitized key whose `/`-separated segments are all non-empty and not `.`.
fn valid_upload_name(name: &str) -> bool {
    sanitize_key(name).is_ok() && name.split('/').all(|seg| !seg.is_empty() && seg != ".")
}

/// Number of chunks of a file of `size` bytes.
fn chunk_count(size: u64, chunk_size: u64) -> u64 {
    size.div_ceil(chunk_size.max(1))
}

/// The exact body length chunk `index` must have, or `None` when the index is out of range.
fn expected_chunk_len(size: u64, chunk_size: u64, index: u64) -> Option<u64> {
    let chunk = chunk_size.max(1);
    let count = chunk_count(size, chunk);
    if index >= count {
        return None;
    }
    if index + 1 == count {
        Some(size - index * chunk)
    } else {
        Some(chunk)
    }
}

fn verify_sha(file: &Path, declared: &UploadFileRequest) -> Result<(), ServerErr> {
    let expected = declared
        .sha256
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ServerErr::bad_request(E_UPLOAD, format!("sha256 missing for {}", declared.name))
        })?;
    let got = dbsloc_pipeline::sha256::file_sha256_hex(file)
        .map_err(|e| ServerErr::internal(format!("hash {}: {e}", file.display())))?;
    if got.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(ServerErr::bad_request(
            E_UPLOAD,
            format!(
                "sha256 mismatch for {} (expected {expected})",
                declared.name
            ),
        ))
    }
}

/// Assemble the chunks of one announced file into `assembled/<name>` and return that path.
///
/// The chunks must be exactly the contiguous set `0..ceil(size/chunk_size)` and the assembled length
/// must equal the announced `size`; gaps, duplicates or a wrong length are rejected.
fn assemble_file(
    state: &AppState,
    id: &str,
    name: &str,
    expected_size: u64,
    chunk_size: u64,
) -> Result<PathBuf, ServerErr> {
    sanitize_key(name)
        .map_err(|_| ServerErr::bad_request(E_UPLOAD, format!("bad file name: {name}")))?;
    let base = upload_dir(state, id);
    let chunks_dir = base.join("chunks").join(name);
    let mut chunks: Vec<(u64, PathBuf)> = Vec::new();
    let rd = std::fs::read_dir(&chunks_dir)
        .map_err(|_| ServerErr::bad_request(E_UPLOAD, format!("no chunks for {name}")))?;
    for e in rd.flatten() {
        let fname = e.file_name().to_string_lossy().into_owned();
        if let Ok(n) = fname.parse::<u64>() {
            chunks.push((n, e.path()));
        }
    }
    chunks.sort_by_key(|(n, _)| *n);
    let expected_count = chunk_count(expected_size, chunk_size);
    let contiguous = expected_count > 0
        && chunks.len() as u64 == expected_count
        && chunks.iter().enumerate().all(|(i, (n, _))| *n == i as u64);
    if !contiguous {
        return Err(ServerErr::bad_request(
            E_UPLOAD,
            format!("file {name} does not have the expected {expected_count} contiguous chunks"),
        ));
    }
    let out = base.join("assembled").join(name);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut writer = std::io::BufWriter::new(std::fs::File::create(&out)?);
    let mut written: u64 = 0;
    for (_, p) in &chunks {
        let mut f = std::fs::File::open(p)?;
        written += std::io::copy(&mut f, &mut writer)?;
    }
    writer.flush()?;
    drop(writer);
    if written != expected_size {
        let _ = std::fs::remove_file(&out);
        return Err(ServerErr::bad_request(
            E_UPLOAD,
            format!("file {name} assembled to {written} bytes, expected {expected_size}"),
        ));
    }
    Ok(out)
}

/// POST a staged upload; creates an upload id and returns the chunk size.
pub async fn upload_init(
    State(state): State<AppState>,
    AxumPath(ds): AxumPath<String>,
    Extension(client): Extension<ClientAddr>,
    Json(req): Json<UploadInitRequest>,
) -> Result<Json<UploadInitResponse>, ServerErr> {
    blocking(move || init_blocking(&state, ds, req, &client))
        .await
        .map(Json)
}

fn init_blocking(
    state: &AppState,
    ds: String,
    req: UploadInitRequest,
    client: &ClientAddr,
) -> Result<UploadInitResponse, ServerErr> {
    if !valid_dataset_name(&ds) {
        return Err(ServerErr::bad_request(E_BAD_KEY, "invalid dataset name"));
    }
    if !state.dataset_root(&ds).is_dir() {
        return Err(ServerErr::not_found(format!("no dataset {ds}")));
    }
    if req.files.is_empty() {
        return Err(ServerErr::bad_request(E_UPLOAD, "no files announced"));
    }
    sweep_stale_uploads(&state.config().incoming_dir());
    let mut total: u64 = 0;
    let mut names: HashSet<&str> = HashSet::new();
    for f in &req.files {
        if !valid_upload_name(&f.name) {
            return Err(ServerErr::bad_request(
                E_UPLOAD,
                format!("bad file name: {}", f.name),
            ));
        }
        if f.size == 0 {
            return Err(ServerErr::bad_request(
                E_UPLOAD,
                format!("file {} is empty", f.name),
            ));
        }
        if !names.insert(f.name.as_str()) {
            return Err(ServerErr::bad_request(
                E_UPLOAD,
                format!("file announced twice: {}", f.name),
            ));
        }
        let digest = f.sha256.as_deref().map(str::trim).unwrap_or("");
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ServerErr::bad_request(
                E_UPLOAD,
                format!("file {} is missing a valid sha256", f.name),
            ));
        }
        total = total.saturating_add(f.size);
    }
    for f in &req.files {
        let mut prefix = String::new();
        for seg in f.name.split('/') {
            if !prefix.is_empty() && names.contains(prefix.as_str()) {
                return Err(ServerErr::bad_request(
                    E_UPLOAD,
                    format!("file {prefix} is also used as a directory of {}", f.name),
                ));
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
        }
    }
    if total > state.config().max_upload_bytes {
        return Err(ServerErr::bad_request(
            E_UPLOAD,
            format!("upload is {total} bytes, over the limit"),
        ));
    }
    let id = crate::runs::new_id();
    let dir = upload_dir(state, &id);
    std::fs::create_dir_all(dir.join("chunks"))?;
    let manifest = UploadManifest {
        dataset: ds.clone(),
        chunk_size: state.config().chunk_size,
        files: req.files,
    };
    let text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| ServerErr::internal(format!("manifest: {e}")))?;
    let path = manifest_path(state, &id);
    let tmp = path.with_file_name("upload.json.partial");
    let written = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    state.uploads().insert(
        &id,
        Arc::new(StagedUpload::new(manifest, file_stamp(&path))),
    );
    let _ = crate::audit::append(
        &state.audit_log(),
        &AuditEntry::now(
            client.as_str(),
            Some(ds),
            None,
            "upload_init",
            Some(format!("id={id}")),
        ),
    );
    Ok(UploadInitResponse {
        upload_id: id,
        chunk_size: state.config().chunk_size,
    })
}

/// PUT one raw chunk (idempotent per index).
pub async fn upload_chunk(
    State(state): State<AppState>,
    AxumPath((id, file, chunk)): AxumPath<(String, String, u32)>,
    body: Bytes,
) -> Result<StatusCode, ServerErr> {
    blocking(move || chunk_blocking(&state, &id, &file, chunk, &body)).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn chunk_blocking(
    state: &AppState,
    id: &str,
    file: &str,
    chunk: u32,
    body: &[u8],
) -> Result<(), ServerErr> {
    let staged = read_manifest(state, id)?;
    let manifest = &staged.manifest;
    sanitize_key(file)
        .map_err(|_| ServerErr::bad_request(E_BAD_KEY, format!("bad file name: {file}")))?;
    let declared = staged.file(file).ok_or_else(|| {
        ServerErr::bad_request(E_UPLOAD, format!("file {file} was not announced"))
    })?;
    let expected = expected_chunk_len(declared.size, manifest.chunk_size, u64::from(chunk))
        .ok_or_else(|| {
            ServerErr::bad_request(
                E_UPLOAD,
                format!("chunk {chunk} is out of range for {file}"),
            )
        })?;
    if body.len() as u64 != expected {
        return Err(ServerErr::bad_request(
            E_UPLOAD,
            format!(
                "chunk {chunk} of {file} is {} bytes, expected {expected}",
                body.len()
            ),
        ));
    }
    let dest = upload_dir(state, id)
        .join("chunks")
        .join(file)
        .join(chunk.to_string());
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_file_name(format!("{chunk}.{}.partial", crate::runs::new_id()));
    let result = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &dest));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    Ok(())
}

/// POST convert: for a DICOM upload run `dcm2niix` and return the series; empty otherwise.
pub async fn upload_convert(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<ConvertedSeries>>, ServerErr> {
    blocking(move || convert_blocking(&state, &id))
        .await
        .map(Json)
}

fn import_err(e: ImportError) -> ServerErr {
    match e.code {
        IMPORT_E_DCM2NIIX | E_IMPORT_CANCELLED => ServerErr::bad_request(E_DCM2NIIX, e.message),
        E_IMPORT_FILE => ServerErr::bad_request(E_NIFTI, e.message),
        E_IMPORT_RULES => ServerErr::bad_request(E_IMPORT_RULES, e.message),
        code => ServerErr::from(BidsError {
            code,
            message: e.message,
        }),
    }
}

fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn mapping_of(item: &ImportItem) -> BidsMappingEntry {
    BidsMappingEntry {
        file: file_name_of(&item.source),
        session: Some(item.session.as_str().to_owned()),
        modality: "anat".to_owned(),
        suffix: item.suffix.clone(),
        acq: item.acq.map(|a| a.as_str().to_owned()),
    }
}

fn convert_blocking(state: &AppState, id: &str) -> Result<Vec<ConvertedSeries>, ServerErr> {
    let staged = read_manifest(state, id)?;
    let manifest = &staged.manifest;
    let in_dir = upload_dir(state, id).join("assembled");
    for f in &manifest.files {
        let file = assemble_file(state, id, &f.name, f.size, manifest.chunk_size)?;
        verify_sha(&file, f)?;
    }
    if !import::looks_like_dicom(&in_dir) {
        return Ok(Vec::new());
    }
    let out_dir = upload_dir(state, id).join("nifti");
    let _ = std::fs::remove_dir_all(&out_dir);
    let cancel = Arc::new(AtomicBool::new(false));
    let converted = import::convert_dicom(&in_dir, &out_dir, &cancel).map_err(import_err)?;
    let series: Vec<ConvertedSeries> = converted
        .iter()
        .map(|c| ConvertedSeries {
            file: file_name_of(&c.nifti),
            series_number: c.series_number,
            description: c.description.clone(),
            protocol: c.protocol.clone(),
            modality: c.modality.clone(),
            dims: c.dims,
            voxel_mm: c.voxel_mm,
            mapping: mapping_of(&c.suggested),
        })
        .collect();
    let text = serde_json::to_vec(&series)
        .map_err(|e| ServerErr::internal(format!("converted list: {e}")))?;
    std::fs::write(upload_dir(state, id).join(CONVERTED_FILE), text)?;
    Ok(series)
}

fn read_converted(state: &AppState, id: &str) -> Result<HashSet<String>, ServerErr> {
    let path = upload_dir(state, id).join(CONVERTED_FILE);
    if !path.is_file() {
        return Ok(HashSet::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let series: Vec<ConvertedSeries> = serde_json::from_str(&text)
        .map_err(|e| ServerErr::bad_request(E_UPLOAD, format!("bad converted list: {e}")))?;
    Ok(series.into_iter().map(|s| s.file).collect())
}

/// Validate a NIfTI header (dims / datatype / qform or sform), then blank `descrip` + `aux_file`.
/// `max_decompressed` caps the length of a gzip payload so a small gzip cannot expand without bound.
pub fn validate_and_scrub(path: &Path, max_decompressed: u64) -> Result<(), ServerErr> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !(name.ends_with(".nii") || name.ends_with(".nii.gz")) {
        return Err(ServerErr::bad_request(
            E_NIFTI,
            format!("not a NIfTI file: {name}"),
        ));
    }
    let hdr = NiftiHeader::from_file(path)
        .map_err(|e| ServerErr::bad_request(E_NIFTI, format!("invalid NIfTI header: {e}")))?;
    let dim0 = hdr.dim[0];
    if !(3..=7).contains(&dim0) {
        return Err(ServerErr::bad_request(
            E_NIFTI,
            format!("NIfTI dimensionality {} is not a volume", dim0),
        ));
    }
    if hdr.dim[1] == 0 || hdr.dim[2] == 0 || hdr.dim[3] == 0 {
        return Err(ServerErr::bad_request(
            E_NIFTI,
            "NIfTI has a zero-sized spatial dimension",
        ));
    }
    if hdr.dim[1..=3].iter().any(|&d| d > MAX_SPATIAL_DIM) {
        return Err(ServerErr::bad_request(
            E_NIFTI,
            format!(
                "NIfTI dims too large: {}x{}x{}",
                hdr.dim[1], hdr.dim[2], hdr.dim[3]
            ),
        ));
    }
    if hdr.dim[4..=usize::from(dim0)]
        .iter()
        .any(|&d| d > MAX_EXTRA_DIM)
    {
        return Err(ServerErr::bad_request(
            E_NIFTI,
            "NIfTI higher dimension is too large",
        ));
    }
    if hdr.datatype == 0 {
        return Err(ServerErr::bad_request(E_NIFTI, "NIfTI has no datatype"));
    }
    if hdr.qform_code == 0 && hdr.sform_code == 0 {
        return Err(ServerErr::bad_request(
            E_NIFTI,
            "NIfTI has neither a qform nor an sform",
        ));
    }
    scrub_header(path, max_decompressed)
}

const HEADER_LEN: usize = 348;
const SCRUB_START: usize = 148;
const SCRUB_END: usize = 252;

fn check_sizeof(head: &[u8]) -> Result<(), ServerErr> {
    let raw = [head[0], head[1], head[2], head[3]];
    if i32::from_le_bytes(raw) == 348 || i32::from_be_bytes(raw) == 348 {
        Ok(())
    } else {
        Err(ServerErr::bad_request(
            E_NIFTI,
            format!(
                "only NIfTI-1 headers are supported (sizeof_hdr={})",
                i32::from_le_bytes(raw)
            ),
        ))
    }
}

fn too_short() -> ServerErr {
    ServerErr::bad_request(E_NIFTI, "NIfTI file too short")
}

/// Zero the NIfTI-1 `descrip` (offset 148, 80 bytes) and `aux_file` (offset 228, 24 bytes) fields,
/// streaming: a plain file is patched in place, a gzip file is re-encoded into a sibling temp file
/// that replaces the original.
fn scrub_header(path: &Path, max_decompressed: u64) -> Result<(), ServerErr> {
    let mut magic = [0u8; 2];
    std::fs::File::open(path)?
        .read_exact(&mut magic)
        .map_err(|_| too_short())?;
    if magic == [0x1f, 0x8b] {
        scrub_gz(path, max_decompressed)
    } else {
        scrub_plain(path)
    }
}

fn scrub_plain(path: &Path) -> Result<(), ServerErr> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    if f.metadata()?.len() < HEADER_LEN as u64 {
        return Err(too_short());
    }
    let mut head = [0u8; 4];
    f.read_exact(&mut head)?;
    check_sizeof(&head)?;
    f.seek(SeekFrom::Start(SCRUB_START as u64))?;
    f.write_all(&[0u8; SCRUB_END - SCRUB_START])?;
    f.flush()?;
    Ok(())
}

fn scrub_gz(path: &Path, max_decompressed: u64) -> Result<(), ServerErr> {
    let fname = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{fname}.{}.scrub", crate::runs::new_id()));
    let result = scrub_gz_into(path, &tmp, max_decompressed);
    match result {
        Ok(()) => std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            ServerErr::from(e)
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn scrub_gz_into(src: &Path, tmp: &Path, max_decompressed: u64) -> Result<(), ServerErr> {
    let gz_err = |e: std::io::Error| ServerErr::bad_request(E_NIFTI, format!("gzip decode: {e}"));
    let mut dec = flate2::read::GzDecoder::new(std::io::BufReader::new(std::fs::File::open(src)?));
    let mut head = vec![0u8; HEADER_LEN];
    dec.read_exact(&mut head).map_err(|_| too_short())?;
    check_sizeof(&head)?;
    head[SCRUB_START..SCRUB_END].fill(0);
    let mut enc = flate2::write::GzEncoder::new(
        std::io::BufWriter::new(std::fs::File::create(tmp)?),
        flate2::Compression::default(),
    );
    enc.write_all(&head)?;
    let mut total = HEADER_LEN as u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = dec.read(&mut buf).map_err(gz_err)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_decompressed {
            return Err(ServerErr::bad_request(
                E_UPLOAD,
                "decompressed NIfTI exceeds the size limit",
            ));
        }
        enc.write_all(&buf[..n])?;
    }
    let mut out = enc.finish()?;
    out.flush()?;
    Ok(())
}

/// A BIDS entity used in a placement path must be a non-empty `[A-Za-z0-9]+` token: no `/`, `\`,
/// `.`, `..` or whitespace, so it can never escape `rawdata/`.
fn valid_bids_entity(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn parse_session(entry: &BidsMappingEntry) -> Result<Session, ServerErr> {
    match entry.session.as_deref().map(str::trim) {
        None | Some("") => Err(ServerErr::bad_request(
            E_BAD_SUBJECT,
            format!("session is required for {} (preop or postop)", entry.file),
        )),
        Some("preop") => Ok(Session::Preop),
        Some("postop") => Ok(Session::Postop),
        Some(other) => Err(ServerErr::bad_request(
            E_BAD_KEY,
            format!("invalid session: {other}"),
        )),
    }
}

fn parse_acq(entry: &BidsMappingEntry) -> Result<Option<Acq>, ServerErr> {
    match entry.acq.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(a) => Acq::ALL
            .into_iter()
            .find(|k| k.as_str() == a)
            .map(Some)
            .ok_or_else(|| ServerErr::bad_request(E_BAD_KEY, format!("invalid acq: {a}"))),
    }
}

fn sidecar_of(nifti: &Path) -> Option<PathBuf> {
    let name = file_name_of(nifti);
    let stem = name
        .strip_suffix(".nii.gz")
        .or_else(|| name.strip_suffix(".nii"))
        .unwrap_or(&name);
    Some(nifti.with_file_name(format!("{stem}.json"))).filter(|p| p.is_file())
}

/// POST commit: place files in `rawdata/` through `dbsloc_pipeline::import`.
pub async fn upload_commit(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Extension(client): Extension<ClientAddr>,
    Json(req): Json<UploadCommitRequest>,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || commit_blocking(&state, &id, &client, &req))
        .await
        .map(Json)
}

fn commit_blocking(
    state: &AppState,
    id: &str,
    client: &ClientAddr,
    req: &UploadCommitRequest,
) -> Result<Value, ServerErr> {
    let staged = read_manifest(state, id)?;
    let manifest = &staged.manifest;
    if !valid_dataset_name(&manifest.dataset) {
        return Err(ServerErr::bad_request(E_BAD_KEY, "invalid dataset name"));
    }
    if req.mapping.is_empty() {
        return Err(ServerErr::bad_request(E_UPLOAD, "empty mapping"));
    }
    let subject = req
        .subject
        .trim()
        .strip_prefix("sub-")
        .unwrap_or(req.subject.trim())
        .to_owned();
    if !valid_bids_entity(&subject) {
        return Err(ServerErr::bad_request(
            E_BAD_SUBJECT,
            format!("invalid subject label: {}", req.subject),
        ));
    }
    let mut destinations: HashSet<(Session, String, Option<Acq>)> = HashSet::new();
    let mut parsed: Vec<(Session, Option<Acq>)> = Vec::new();
    for entry in &req.mapping {
        if entry.modality != "anat" {
            return Err(ServerErr::bad_request(
                E_BAD_KEY,
                format!(
                    "invalid modality: {} (only anat is accepted)",
                    entry.modality
                ),
            ));
        }
        if !valid_bids_entity(&entry.suffix) {
            return Err(ServerErr::bad_request(
                E_BAD_KEY,
                format!("invalid suffix: {}", entry.suffix),
            ));
        }
        let session = parse_session(entry)?;
        let acq = parse_acq(entry)?;
        if !destinations.insert((session, entry.suffix.clone(), acq)) {
            return Err(ServerErr::bad_request(
                E_UPLOAD,
                format!(
                    "two files map to the same destination ({}, {})",
                    session.as_str(),
                    entry.suffix
                ),
            ));
        }
        parsed.push((session, acq));
    }
    let root = state.dataset_root(&manifest.dataset);
    if !root.is_dir() {
        return Err(ServerErr::not_found(format!(
            "no dataset {}",
            manifest.dataset
        )));
    }
    if state
        .db()
        .active_for_subject(&manifest.dataset, &subject)?
        .is_some()
    {
        return Err(ServerErr::conflict(
            E_RUN_ACTIVE,
            format!(
                "sub-{subject} has a queued, running or awaiting-QC run; wait for it to finish"
            ),
        ));
    }

    let converted = read_converted(state, id)?;
    let mut items: Vec<ImportItem> = Vec::new();
    let mut total: u64 = 0;
    for (entry, (session, acq)) in req.mapping.iter().zip(parsed) {
        sanitize_key(&entry.file).map_err(|_| {
            ServerErr::bad_request(E_UPLOAD, format!("bad file name: {}", entry.file))
        })?;
        let is_converted = converted.contains(&entry.file);
        let file = if is_converted {
            let p = upload_dir(state, id).join("nifti").join(&entry.file);
            if !p.is_file() {
                return Err(ServerErr::bad_request(
                    E_UPLOAD,
                    format!("converted file {} is missing", entry.file),
                ));
            }
            p
        } else {
            let declared = staged.file(&entry.file).ok_or_else(|| {
                ServerErr::bad_request(E_UPLOAD, format!("file {} was not announced", entry.file))
            })?;
            let p = assemble_file(state, id, &entry.file, declared.size, manifest.chunk_size)?;
            verify_sha(&p, declared)?;
            p
        };
        total = total.saturating_add(std::fs::metadata(&file)?.len());
        if total > state.config().max_upload_bytes {
            return Err(ServerErr::bad_request(
                E_UPLOAD,
                "upload exceeds the configured size limit",
            ));
        }
        validate_and_scrub(&file, state.config().max_upload_bytes)?;
        let sidecar = if is_converted {
            sidecar_of(&file)
        } else {
            None
        };
        items.push(ImportItem {
            source: file,
            sidecar,
            session,
            suffix: entry.suffix.clone(),
            acq,
        });
    }

    let plan = import::plan_import(&root, &subject, &items).map_err(import_err)?;
    if !req.replace_existing {
        let conflicts = plan.conflicts();
        if !conflicts.is_empty() {
            let names: Vec<&str> = conflicts.iter().map(|f| f.relative_path.as_str()).collect();
            return Err(ServerErr::conflict(
                E_IMPORT_CONFLICT,
                format!(
                    "already present: {}; commit with replace_existing to overwrite",
                    names.join(", ")
                ),
            ));
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let report = import::run_import(&plan, req.replace_existing, &cancel, &mut |_| {})
        .map_err(import_err)?;
    let invalidated = report
        .invalidation
        .summary("the upload replaced raw images");

    let files: Vec<String> = plan.files.iter().map(|f| f.relative_path.clone()).collect();
    let _ = std::fs::remove_dir_all(upload_dir(state, id));
    state.uploads().remove(id);
    let _ = crate::audit::append(
        &state.audit_log(),
        &AuditEntry::now(
            client.as_str(),
            Some(manifest.dataset.clone()),
            Some(subject.clone()),
            "upload_commit",
            Some(match &invalidated {
                Some(line) => format!("id={id} files={} {line}", files.len()),
                None => format!("id={id} files={}", files.len()),
            }),
        ),
    );
    Ok(json!({ "subject": subject, "files": files, "invalidated": invalidated }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_nifti() -> Vec<u8> {
        let mut b = vec![0u8; 352 + 8];
        b[0..4].copy_from_slice(&348i32.to_le_bytes());
        b[40..42].copy_from_slice(&3i16.to_le_bytes());
        b[42..44].copy_from_slice(&2i16.to_le_bytes());
        b[44..46].copy_from_slice(&2i16.to_le_bytes());
        b[46..48].copy_from_slice(&2i16.to_le_bytes());
        for i in 4..8 {
            b[40 + 2 * i..42 + 2 * i].copy_from_slice(&1i16.to_le_bytes());
        }
        b[70..72].copy_from_slice(&2i16.to_le_bytes());
        b[72..74].copy_from_slice(&8i16.to_le_bytes());
        for i in 1..4 {
            b[76 + 4 * i..80 + 4 * i].copy_from_slice(&1.0f32.to_le_bytes());
        }
        b[108..112].copy_from_slice(&352.0f32.to_le_bytes());
        b[252..254].copy_from_slice(&1i16.to_le_bytes());
        b[344..348].copy_from_slice(b"n+1\0");
        b
    }

    #[test]
    fn validate_accepts_a_minimal_nifti_and_scrubs_text() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.nii");
        let mut b = minimal_nifti();
        b[148..154].copy_from_slice(b"CANCER");
        b[228..230].copy_from_slice(b"AB");
        std::fs::write(&p, &b).unwrap();
        validate_and_scrub(&p, 1 << 30).unwrap();
        let out = std::fs::read(&p).unwrap();
        assert!(out[148..228].iter().all(|&x| x == 0));
        assert!(out[228..252].iter().all(|&x| x == 0));
    }

    #[test]
    fn validate_rejects_bad_dims_and_missing_qform() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.nii");
        std::fs::write(&p, minimal_nifti()).unwrap();
        let mut b = minimal_nifti();
        b[252..254].copy_from_slice(&0i16.to_le_bytes());
        std::fs::write(&p, &b).unwrap();
        assert_eq!(validate_and_scrub(&p, 1 << 30).unwrap_err().code(), E_NIFTI);
        let q = d.path().join("t.txt");
        std::fs::write(&q, minimal_nifti()).unwrap();
        assert_eq!(validate_and_scrub(&q, 1 << 30).unwrap_err().code(), E_NIFTI);
    }

    #[test]
    fn a_gzip_that_expands_past_the_cap_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.nii.gz");
        let mut b = minimal_nifti();
        b.resize(4 * 1024 * 1024, 0);
        std::fs::write(&p, gz_encode(&b).unwrap()).unwrap();
        let err = validate_and_scrub(&p, 64 * 1024).unwrap_err();
        assert_eq!(err.code(), E_UPLOAD, "{err}");
        validate_and_scrub(&p, 8 * 1024 * 1024).unwrap();
    }

    #[test]
    fn bids_entities_reject_traversal_and_separators() {
        for bad in ["", "..", "../evil", "a/b", "a\\b", "a.b", "a b", "-x"] {
            assert!(!valid_bids_entity(bad), "{bad} was accepted");
        }
        for good in ["anat", "T1w", "01", "ses1", "CT"] {
            assert!(valid_bids_entity(good), "{good} was rejected");
        }
    }

    fn gz_encode(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(bytes)?;
        enc.finish()
    }

    fn gz_decode(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn oversized_dims_are_rejected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.nii");
        for axis in 0..3 {
            let mut b = minimal_nifti();
            let off = 42 + 2 * axis;
            b[off..off + 2].copy_from_slice(&3000i16.to_le_bytes());
            std::fs::write(&p, &b).unwrap();
            let err = validate_and_scrub(&p, 1 << 30).unwrap_err();
            assert_eq!(err.code(), E_NIFTI, "axis {axis}: {err}");
        }
        let mut b = minimal_nifti();
        b[40..42].copy_from_slice(&4i16.to_le_bytes());
        b[48..50].copy_from_slice(&5000i16.to_le_bytes());
        std::fs::write(&p, &b).unwrap();
        assert_eq!(validate_and_scrub(&p, 1 << 30).unwrap_err().code(), E_NIFTI);
        let mut b = minimal_nifti();
        b[42..44].copy_from_slice(&2048i16.to_le_bytes());
        std::fs::write(&p, &b).unwrap();
        validate_and_scrub(&p, 1 << 30).unwrap();
    }

    #[test]
    fn plain_scrub_patches_in_place_and_keeps_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.nii");
        let mut b = minimal_nifti();
        b[148..154].copy_from_slice(b"CANCER");
        b[228..230].copy_from_slice(b"AB");
        b[352..360].copy_from_slice(b"voxeldat");
        std::fs::write(&p, &b).unwrap();
        validate_and_scrub(&p, 1 << 30).unwrap();
        let out = std::fs::read(&p).unwrap();
        assert_eq!(out.len(), b.len());
        assert!(out[148..252].iter().all(|&x| x == 0));
        assert_eq!(&out[..148], &b[..148]);
        assert_eq!(&out[252..], &b[252..]);
    }

    #[test]
    fn gz_scrub_streams_to_a_temp_and_replaces_the_original() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.nii.gz");
        let mut b = minimal_nifti();
        b[148..154].copy_from_slice(b"CANCER");
        b[228..230].copy_from_slice(b"AB");
        b[352..360].copy_from_slice(b"voxeldat");
        std::fs::write(&p, gz_encode(&b).unwrap()).unwrap();
        validate_and_scrub(&p, 1 << 30).unwrap();
        let out = gz_decode(&std::fs::read(&p).unwrap());
        assert_eq!(out.len(), b.len());
        assert!(out[148..252].iter().all(|&x| x == 0));
        assert_eq!(&out[..148], &b[..148]);
        assert_eq!(&out[252..], &b[252..]);
        let names: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "temp file left behind: {names:?}");
    }

    #[test]
    fn a_gz_bomb_leaves_the_original_and_no_temp_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.nii.gz");
        let mut b = minimal_nifti();
        b.resize(4 * 1024 * 1024, 0);
        let gz = gz_encode(&b).unwrap();
        std::fs::write(&p, &gz).unwrap();
        assert_eq!(
            validate_and_scrub(&p, 64 * 1024).unwrap_err().code(),
            E_UPLOAD
        );
        assert_eq!(std::fs::read(&p).unwrap(), gz);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn chunk_lengths_are_exact_and_indices_bounded() {
        assert_eq!(expected_chunk_len(48, 16, 0), Some(16));
        assert_eq!(expected_chunk_len(48, 16, 2), Some(16));
        assert_eq!(expected_chunk_len(48, 16, 3), None);
        assert_eq!(expected_chunk_len(50, 16, 3), Some(2));
        assert_eq!(expected_chunk_len(10, 16, 0), Some(10));
        assert_eq!(expected_chunk_len(10, 16, 1), None);
        assert_eq!(expected_chunk_len(0, 16, 0), None);
    }

    #[test]
    fn upload_names_reject_empty_and_dot_segments() {
        for bad in ["", "a//b", "./a", "a/./b", "a/", "../a", "a:b", r"a\b"] {
            assert!(!valid_upload_name(bad), "{bad} was accepted");
        }
        for good in ["a.nii", "dicom/IM0001.dcm"] {
            assert!(valid_upload_name(good), "{good} was rejected");
        }
    }

    use crate::config::Config;

    struct Staged {
        _root: tempfile::TempDir,
        state: AppState,
        client: ClientAddr,
    }

    fn staged() -> Staged {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new_idle(Config::default_for_root(root.path())).unwrap();
        std::fs::create_dir_all(state.dataset_root("demo")).unwrap();
        Staged {
            _root: root,
            state,
            client: ClientAddr("test".to_owned()),
        }
    }

    fn stage_upload(st: &Staged, id: &str, names: &[&str]) {
        let dir = upload_dir(&st.state, id);
        let mut files = Vec::new();
        for name in names {
            let data = minimal_nifti();
            let chunk = dir.join("chunks").join(name);
            std::fs::create_dir_all(&chunk).unwrap();
            std::fs::write(chunk.join("0"), &data).unwrap();
            files.push(UploadFileRequest {
                name: (*name).to_owned(),
                size: data.len() as u64,
                sha256: Some(dbsloc_pipeline::sha256::sha256_hex(&data)),
            });
        }
        let manifest = UploadManifest {
            dataset: "demo".to_owned(),
            chunk_size: 1 << 20,
            files,
        };
        std::fs::write(
            dir.join("upload.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn entry(
        file: &str,
        session: Option<&str>,
        suffix: &str,
        acq: Option<&str>,
    ) -> BidsMappingEntry {
        BidsMappingEntry {
            file: file.to_owned(),
            session: session.map(str::to_owned),
            modality: "anat".to_owned(),
            suffix: suffix.to_owned(),
            acq: acq.map(str::to_owned),
        }
    }

    fn commit(
        st: &Staged,
        id: &str,
        mapping: Vec<BidsMappingEntry>,
        replace_existing: bool,
    ) -> Result<Value, ServerErr> {
        let req = UploadCommitRequest {
            subject: "X".to_owned(),
            mapping,
            replace_existing,
        };
        commit_blocking(&st.state, id, &st.client, &req)
    }

    fn two_image_mapping() -> Vec<BidsMappingEntry> {
        vec![
            entry("pre.nii", Some("preop"), "T1w", None),
            entry("post.nii", Some("postop"), "T1w", Some("ax")),
        ]
    }

    #[test]
    fn commit_places_gz_files_with_acq_and_index_modality() {
        let st = staged();
        let id = "a".repeat(32);
        stage_upload(&st, &id, &["pre.nii", "post.nii"]);
        let out = commit(&st, &id, two_image_mapping(), false).unwrap();
        let root = st.state.dataset_root("demo");
        let post = root.join("rawdata/sub-X/ses-postop/anat/sub-X_ses-postop_acq-ax_T1w.nii.gz");
        let pre = root.join("rawdata/sub-X/ses-preop/anat/sub-X_ses-preop_T1w.nii.gz");
        assert!(post.is_file(), "{out}");
        assert!(pre.is_file());
        assert_eq!(&std::fs::read(&post).unwrap()[..2], &[0x1f, 0x8b]);
        let files: Vec<&str> = out["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(files.contains(&"rawdata/sub-X/ses-postop/anat/sub-X_ses-postop_acq-ax_T1w.nii.gz"));
        let rows: Vec<Value> = serde_json::from_str(
            &std::fs::read_to_string(root.join("rawdata/sub-X/rawimages.json")).unwrap(),
        )
        .unwrap();
        assert!(rows
            .iter()
            .any(|r| r["session"] == "postop" && r["entities"]["modality"] == "ax_MRI"));
    }

    #[test]
    fn a_postop_row_without_acq_is_rejected_before_anything_is_written() {
        let st = staged();
        let id = "b".repeat(32);
        stage_upload(&st, &id, &["pre.nii", "post.nii"]);
        let mapping = vec![
            entry("pre.nii", Some("preop"), "T1w", None),
            entry("post.nii", Some("postop"), "T1w", None),
        ];
        let err = commit(&st, &id, mapping, false).unwrap_err();
        assert_eq!(err.code(), E_IMPORT_RULES, "{err}");
        assert!(!st.state.dataset_root("demo").join("rawdata").exists());
    }

    #[test]
    fn a_missing_session_is_rejected() {
        let st = staged();
        let id = "c".repeat(32);
        stage_upload(&st, &id, &["pre.nii"]);
        let err = commit(&st, &id, vec![entry("pre.nii", None, "T1w", None)], false).unwrap_err();
        assert_eq!(err.code(), E_BAD_SUBJECT);
        assert!(err.to_string().contains("session is required"));
    }

    #[test]
    fn a_second_commit_needs_replace_existing() {
        let st = staged();
        let id = "d".repeat(32);
        stage_upload(&st, &id, &["pre.nii", "post.nii"]);
        commit(&st, &id, two_image_mapping(), false).unwrap();
        let id2 = "e".repeat(32);
        stage_upload(&st, &id2, &["pre.nii", "post.nii"]);
        let err = commit(&st, &id2, two_image_mapping(), false).unwrap_err();
        assert_eq!(err.code(), E_IMPORT_CONFLICT, "{err}");
        assert_eq!(err.status, StatusCode::CONFLICT);
        commit(&st, &id2, two_image_mapping(), true).unwrap();
    }

    #[test]
    fn import_errors_map_to_server_codes() {
        let e = |code: &'static str| ImportError {
            code,
            message: "m".to_owned(),
        };
        assert_eq!(import_err(e(IMPORT_E_DCM2NIIX)).code(), E_DCM2NIIX);
        assert_eq!(import_err(e(E_IMPORT_CANCELLED)).code(), E_DCM2NIIX);
        assert_eq!(import_err(e(E_IMPORT_FILE)).code(), E_NIFTI);
        assert_eq!(import_err(e(E_IMPORT_RULES)).code(), E_IMPORT_RULES);
    }

    fn manifest_of(n: usize) -> UploadManifest {
        UploadManifest {
            dataset: "demo".to_owned(),
            chunk_size: 8,
            files: (0..n)
                .map(|i| UploadFileRequest {
                    name: format!("f{i}.nii"),
                    size: 1,
                    sha256: None,
                })
                .collect(),
        }
    }

    #[test]
    fn staged_uploads_find_files_by_name() {
        let staged = StagedUpload::new(manifest_of(1000), None);
        assert_eq!(
            staged.file("f999.nii").map(|f| f.name.as_str()),
            Some("f999.nii")
        );
        assert_eq!(
            staged.file("f0.nii").map(|f| f.name.as_str()),
            Some("f0.nii")
        );
        assert!(staged.file("missing.nii").is_none());
    }

    #[test]
    fn the_upload_cache_is_bounded_and_forgets_removed_uploads() {
        let cache = UploadCache::default();
        for i in 0..UPLOAD_CACHE_CAP {
            cache.insert(
                &format!("{i}"),
                Arc::new(StagedUpload::new(manifest_of(0), None)),
            );
        }
        assert_eq!(cache.len(), UPLOAD_CACHE_CAP);
        cache.insert("0", Arc::new(StagedUpload::new(manifest_of(0), None)));
        assert_eq!(cache.len(), UPLOAD_CACHE_CAP);
        cache.insert("new", Arc::new(StagedUpload::new(manifest_of(0), None)));
        assert_eq!(cache.len(), 1);
        cache.remove("new");
        assert!(cache.is_empty());
        assert!(cache.get("new").is_none());
    }
}
