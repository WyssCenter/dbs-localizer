// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Browser file picker and chunked file access (`docs/SERVER_CLIENT_PLAN.md` §5.3).
//!
//! The picker keeps `web_sys::File` handles and never reads a file whole: the SHA-256 the server
//! needs in the upload-init request is computed by streaming `Blob.slice` chunks through an
//! incremental hasher, and the upload sends one sliced chunk at a time. At most one chunk is in
//! wasm memory. No `std::fs`: this is the browser File API.

use std::cell::RefCell;
use std::rc::Rc;

use dbsloc_api::ApiError;
use eframe::egui;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};

use crate::api::{ApiClient, ApiMsg};
use crate::logic::{chunk_count, chunk_range, StreamHasher};

/// Chunk size used when hashing a file (independent of the upload chunk size).
const HASH_CHUNK: u64 = 8 * 1024 * 1024;

/// One file chosen in the browser; its bytes stay in the browser until read in slices.
#[derive(Debug, Clone)]
pub struct PickedFile {
    pub name: String,
    pub size: u64,
    pub file: web_sys::File,
}

/// Where the picker leaves the chosen files for the app to take on its next frame.
pub type PickSlot = Rc<RefCell<Option<Vec<PickedFile>>>>;

/// Open the file chooser; the chosen files (possibly none) are left in `slot`.
pub fn pick_files(slot: PickSlot, ctx: egui::Context) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let input = document
        .create_element("input")
        .ok()
        .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok());
    let Some(input) = input else {
        return;
    };

    input.set_type("file");
    input.set_multiple(true);
    input.set_accept(".nii,.nii.gz,.gz,.dcm");

    let input_for_change = input.clone();
    let on_change = Closure::<dyn FnMut()>::new(move || {
        let Some(list) = input_for_change.files() else {
            return;
        };
        let files = (0..list.length())
            .filter_map(|i| list.get(i))
            .map(|file| PickedFile {
                name: file.name(),
                size: file.size() as u64,
                file,
            })
            .collect();
        *slot.borrow_mut() = Some(files);
        ctx.request_repaint();
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();

    if let Some(body) = document.body() {
        let _ = body.append_child(&input);
    }
    input.click();
}

async fn read_slice(file: &web_sys::File, start: u64, end: u64) -> Result<Vec<u8>, String> {
    let blob = file
        .slice_with_f64_and_f64(start as f64, end as f64)
        .map_err(|e| format!("slice: {e:?}"))?;
    let buf = JsFuture::from(blob.array_buffer())
        .await
        .map_err(|e| format!("read: {e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Hash every file chunk by chunk, reporting progress; the digests (one per file, in order) or
/// the first read error arrive as [`ApiMsg::HashDone`].
pub fn hash_files(api: ApiClient, generation: u64, files: Vec<PickedFile>) {
    spawn_local(async move {
        let mut done = 0_u64;
        let mut digests = Vec::with_capacity(files.len());
        for f in &files {
            let mut hasher = StreamHasher::new();
            for index in 0..chunk_count(f.size, HASH_CHUNK) {
                let Some((start, end)) = chunk_range(f.size, HASH_CHUNK, index) else {
                    break;
                };
                match read_slice(&f.file, start, end).await {
                    Ok(bytes) => hasher.update(&bytes),
                    Err(e) => {
                        api.emit(ApiMsg::HashDone {
                            generation,
                            result: Err(format!("{}: {e}", f.name)),
                        });
                        return;
                    }
                }
                done += end - start;
                api.emit(ApiMsg::HashProgress { generation, done });
            }
            digests.push(hasher.finish_hex());
        }
        api.emit(ApiMsg::HashDone {
            generation,
            result: Ok(digests),
        });
    });
}

/// Read bytes `[start, end)` of `file` and send them as chunk `chunk` of the upload.
pub fn send_chunk(
    api: ApiClient,
    generation: u64,
    upload_id: String,
    file: PickedFile,
    chunk: u64,
    start: u64,
    end: u64,
) {
    spawn_local(async move {
        match read_slice(&file.file, start, end).await {
            Ok(bytes) => api.put_chunk(generation, &upload_id, &file.name, chunk, bytes),
            Err(e) => api.emit(ApiMsg::UploadChunk {
                generation,
                file: file.name.clone(),
                chunk,
                len: 0,
                result: Err(ApiError::new("E_READ", e)),
            }),
        }
    });
}
