// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Stimulation settings endpoints (`specs/07` §2.3, §2.6, §5.2, §7.8): list, read, write, import
//! and preview. Clients never write the settings files themselves; every write goes through the
//! pipeline's validator and writers.

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use dbsloc_pipeline::bids::{resolve_subject_with, Subject};
use dbsloc_pipeline::options::{RunOptions, VatBackend};
use dbsloc_pipeline::vat::engine::Fail;
use dbsloc_pipeline::vat::preview::{preview, Preview};
use dbsloc_pipeline::vat::settings::{
    check, contact_counts, label_is_valid, list_labels, load, normalise, read_stimparameters_mat,
    save, stimparameters_mat_path, write_stimparameters_mat, StimError, Stimulation,
};
use dbsloc_pipeline::vat::stage::VatParams;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ServerErr, E_BAD_REQUEST, E_RUN_ACTIVE};
use crate::middleware::ClientAddr;
use crate::routes::{audit, blocking, require_dataset};
use crate::runs::{canonical_subject, new_id};
use crate::state::AppState;

/// Error code of every settings failure that has no more specific `E_VAT_*` code.
pub const E_VAT_SETTINGS: &str = "E_VAT_settings";
/// Largest accepted settings JSON.
pub const MAX_SETTINGS_BYTES: usize = 4 * 1024 * 1024;
/// Largest accepted `stimparameters.mat` upload.
pub const MAX_MAT_BYTES: usize = 64 * 1024 * 1024;

const DEFAULT_IMPORT_CONTACTS: usize = 8;

/// Map a settings error onto an HTTP error: validation failures are 422 with the code of the first
/// failed rule, a missing label 404, unreadable content 400.
pub fn stim_err(e: StimError) -> ServerErr {
    match e {
        StimError::Invalid(errors) => {
            let code = errors.first().map_or(E_VAT_SETTINGS, |v| v.code);
            let message = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            ServerErr::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
        }
        StimError::NotFound(m) => ServerErr::not_found(m),
        StimError::Json(m) | StimError::Mat(m) => ServerErr::bad_request(E_VAT_SETTINGS, m),
        StimError::Io(m) => ServerErr::internal(m),
    }
}

fn check_label(label: &str) -> Result<(), ServerErr> {
    if label_is_valid(label) {
        Ok(())
    } else {
        Err(ServerErr::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            E_VAT_SETTINGS,
            format!(
                "label {label:?} must be 1-64 characters of [A-Za-z0-9._-] and not start with '.'"
            ),
        ))
    }
}

fn subject_of(state: &AppState, ds: &str, sid: &str) -> Result<(Subject, String), ServerErr> {
    let sid = canonical_subject(sid)?;
    let root = require_dataset(state, ds)?;
    let sub = resolve_subject_with(&root, &sid, true)?;
    Ok((sub, sid))
}

fn refuse_while_running(state: &AppState, ds: &str, sid: &str) -> Result<(), ServerErr> {
    if let Some(job) = state.db().active_for_subject(ds, sid)? {
        if job.phase != "awaiting_qc" {
            return Err(ServerErr::conflict(
                E_RUN_ACTIVE,
                format!(
                    "sub-{sid} has a queued or running run; its stimulation settings are frozen"
                ),
            ));
        }
    }
    Ok(())
}

/// The settings written by [`commit`].
#[derive(Debug)]
pub struct Committed {
    pub stimulation: Stimulation,
    pub contacts_per_side: [usize; 2],
    pub warnings: Vec<String>,
}

/// Normalise, validate (`specs/07` §2.6) and write the canonical JSON and its `.mat` twin.
pub fn commit(
    sub: &Subject,
    mut stim: Stimulation,
    reco_counts: Option<[usize; 2]>,
    mut warnings: Vec<String>,
) -> Result<Committed, ServerErr> {
    stim.subject.clone_from(&sub.subj_id);
    let counts = reco_counts.unwrap_or_else(|| stim.contact_counts());
    warnings.extend(normalise(&mut stim, counts));
    let report = check(&stim, counts);
    warnings.extend(report.warnings);
    if !report.errors.is_empty() {
        return Err(stim_err(StimError::Invalid(report.errors)));
    }
    save(&sub.subj_dir, &stim).map_err(stim_err)?;
    let saved = load(&sub.subj_dir, &sub.subj_id, &stim.label).map_err(stim_err)?;
    let mat = stimparameters_mat_path(&sub.subj_dir, &sub.subj_id, "native", &stim.label);
    warnings.extend(write_stimparameters_mat(&mat, &saved, [0.0, 0.0]).map_err(stim_err)?);
    Ok(Committed {
        stimulation: saved,
        contacts_per_side: counts,
        warnings,
    })
}

fn committed_json(c: &Committed) -> Value {
    json!({
        "label": c.stimulation.label,
        "contacts_per_side": c.contacts_per_side,
        "warnings": c.warnings,
        "stimulation": c.stimulation,
    })
}

/// `GET /datasets/{ds}/subjects/{sid}/stimulations`
pub async fn list(
    State(state): State<AppState>,
    AxumPath((ds, sid)): AxumPath<(String, String)>,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        let (sub, _) = subject_of(&state, &ds, &sid)?;
        let rows = list_labels(&sub.subj_dir, &sub.subj_id);
        Ok(Json(serde_json::to_value(rows).unwrap_or(Value::Null)))
    })
    .await
}

/// `GET /datasets/{ds}/subjects/{sid}/stimulations/{label}`
pub async fn get_one(
    State(state): State<AppState>,
    AxumPath((ds, sid, label)): AxumPath<(String, String, String)>,
) -> Result<Json<Stimulation>, ServerErr> {
    blocking(move || {
        check_label(&label)?;
        let (sub, _) = subject_of(&state, &ds, &sid)?;
        Ok(Json(
            load(&sub.subj_dir, &sub.subj_id, &label).map_err(stim_err)?,
        ))
    })
    .await
}

/// `PUT /datasets/{ds}/subjects/{sid}/stimulations/{label}`
pub async fn put_one(
    State(state): State<AppState>,
    AxumPath((ds, sid, label)): AxumPath<(String, String, String)>,
    Extension(client): Extension<ClientAddr>,
    body: Bytes,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        check_label(&label)?;
        let stim: Stimulation = serde_json::from_slice(&body).map_err(|e| {
            ServerErr::bad_request(E_VAT_SETTINGS, format!("stimulation settings: {e}"))
        })?;
        if stim.label != label {
            return Err(ServerErr::bad_request(
                E_VAT_SETTINGS,
                format!("the body holds label {:?}, not {label:?}", stim.label),
            ));
        }
        let (sub, sid) = subject_of(&state, &ds, &sid)?;
        let lock = state.subject_lock(&ds, &sid);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        refuse_while_running(&state, &ds, &sid)?;
        let counts = contact_counts(&sub).map_err(stim_err)?;
        let done = commit(&sub, stim, counts, Vec::new())?;
        audit(
            &state,
            client.as_str(),
            Some(&ds),
            Some(&sid),
            "stimulation_set",
            Some(format!("label={label}")),
        );
        Ok(Json(committed_json(&done)))
    })
    .await
}

/// Query of the import endpoint.
#[derive(Debug, Default, Deserialize)]
pub struct ImportQuery {
    label: Option<String>,
}

fn upload_name(name: Option<&str>) -> String {
    name.map(|n| {
        n.chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .take(96)
            .collect()
    })
    .unwrap_or_default()
}

/// `POST /datasets/{ds}/subjects/{sid}/stimulations/import`: a multipart upload (field `file`,
/// optional field `label`) of a `sub-<id>_desc-stimparameters.mat`, or the raw file as the body
/// with `?label=`.
pub async fn import(
    State(state): State<AppState>,
    AxumPath((ds, sid)): AxumPath<(String, String)>,
    Query(query): Query<ImportQuery>,
    Extension(client): Extension<ClientAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ServerErr> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    blocking(move || {
        let upload = crate::multipart::read_upload(content_type.as_deref(), &body)?;
        if upload.bytes.is_empty() {
            return Err(ServerErr::bad_request(
                E_BAD_REQUEST,
                "the uploaded file is empty",
            ));
        }
        let (sub, sid) = subject_of(&state, &ds, &sid)?;
        let lock = state.subject_lock(&ds, &sid);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        refuse_while_running(&state, &ds, &sid)?;
        let counts = contact_counts(&sub).map_err(stim_err)?;
        let fallback = counts
            .map(|c| c[0].max(c[1]))
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_IMPORT_CONTACTS);
        let tmp = state
            .config()
            .incoming_dir()
            .join(format!(".stimparameters-{}.mat", new_id()));
        if let Some(parent) = tmp.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tmp, &upload.bytes)?;
        let read = read_stimparameters_mat(&tmp, &sid, fallback);
        let _ = std::fs::remove_file(&tmp);
        let (mut stim, notes) = read.map_err(stim_err)?;
        let requested = query
            .label
            .clone()
            .or_else(|| upload.field("label").map(str::to_owned))
            .filter(|l| !l.is_empty());
        if let Some(l) = requested {
            stim.label = l;
        }
        if stim.label.is_empty() {
            return Err(ServerErr::bad_request(
                E_VAT_SETTINGS,
                "the file holds no label; pass ?label=<label>",
            ));
        }
        stim.provenance.imported_from = Some(json!({
            "upload": upload_name(upload.filename.as_deref()),
            "sha256": dbsloc_pipeline::sha256::sha256_hex(&upload.bytes),
        }));
        let label = stim.label.clone();
        let done = commit(&sub, stim, counts, notes)?;
        audit(
            &state,
            client.as_str(),
            Some(&ds),
            Some(&sid),
            "stimulation_import",
            Some(format!("label={label}")),
        );
        Ok((StatusCode::OK, Json(committed_json(&done))).into_response())
    })
    .await
}

/// Body of the preview endpoint.
#[derive(Debug, Default, Deserialize)]
pub struct PreviewRequest {
    /// Unsaved settings; absent previews the saved ones.
    #[serde(default)]
    pub stimulation: Option<Value>,
    /// `cpu` (default) or `auto`; `auto` may use the GPU and is refused while a job runs.
    #[serde(default)]
    pub backend: Option<String>,
}

fn fail_err(fail: Fail) -> ServerErr {
    match fail {
        Fail::Cancelled => ServerErr::conflict("cancelled", "the preview was cancelled"),
        Fail::Code { code, message } => {
            ServerErr::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
        }
    }
}

fn runs_of(binary: &[u8]) -> Vec<[usize; 2]> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, v) in binary.iter().enumerate() {
        match (*v != 0, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push([s, i - s]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push([s, binary.len() - s]);
    }
    runs
}

/// The JSON of a preview: per hemisphere the output grid (`dims`, `affine`), the binary VTA as
/// runs of set voxels (`[start, length]` in the flat x-fastest index) and |E| in V/m at the voxels
/// where it is non-zero (empty for analytical models).
pub fn preview_json(label: &str, p: &Preview) -> Value {
    let sides: Vec<Value> = p
        .sides
        .iter()
        .map(|(side, o)| {
            let f = &o.field;
            let runs = runs_of(&f.binary.data);
            let voxels: usize = runs.iter().map(|r| r[1]).sum();
            let (index, value): (Vec<usize>, Vec<f64>) = f
                .e_mag
                .data
                .iter()
                .enumerate()
                .filter(|(_, v)| **v > 0.0)
                .map(|(i, v)| (i, (f64::from(*v) * 1000.0).round() / 1000.0))
                .unzip();
            json!({
                "side": side.name(),
                "dims": f.binary.dims,
                "affine": f.affine,
                "volume_mm3": f.volumes.final_mm3,
                "raw_volume_mm3": f.volumes.raw_mm3,
                "binary_voxels": voxels,
                "binary_runs": runs,
                "e_mag": { "index": index, "value_v_per_m": value },
                "analytic": o.analytic,
                "basis_cache": o.basis_cache,
                "backend": o.backend,
                "seconds": o.seconds,
                "touches_boundary": f.touches_boundary,
                "warnings": o.warnings,
            })
        })
        .collect();
    json!({ "label": label, "warnings": p.warnings, "sides": sides })
}

/// `POST /datasets/{ds}/subjects/{sid}/stimulations/{label}/preview`: computes the field of
/// unsaved settings from the cached basis (`specs/07` §7.8) and writes no result file.
pub async fn preview_one(
    State(state): State<AppState>,
    AxumPath((ds, sid, label)): AxumPath<(String, String, String)>,
    body: Bytes,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        check_label(&label)?;
        let request: PreviewRequest = if body.iter().all(u8::is_ascii_whitespace) {
            PreviewRequest::default()
        } else {
            serde_json::from_slice(&body).map_err(|e| {
                ServerErr::bad_request(E_BAD_REQUEST, format!("preview request: {e}"))
            })?
        };
        let backend = match request.backend.as_deref() {
            None | Some("cpu") => VatBackend::Cpu,
            Some("auto") => VatBackend::Auto,
            Some(other) => {
                return Err(ServerErr::bad_request(
                    E_BAD_REQUEST,
                    format!("backend {other:?} is not cpu or auto"),
                ))
            }
        };
        let (sub, sid) = subject_of(&state, &ds, &sid)?;
        let mut stim: Stimulation = match request.stimulation {
            Some(v) => serde_json::from_value(v).map_err(|e| {
                ServerErr::bad_request(E_VAT_SETTINGS, format!("stimulation settings: {e}"))
            })?,
            None => load(&sub.subj_dir, &sub.subj_id, &label).map_err(stim_err)?,
        };
        if stim.label != label {
            return Err(ServerErr::bad_request(
                E_VAT_SETTINGS,
                format!("the settings hold label {:?}, not {label:?}", stim.label),
            ));
        }
        stim.subject.clone_from(&sub.subj_id);
        let counts = contact_counts(&sub)
            .map_err(stim_err)?
            .unwrap_or_else(|| stim.contact_counts());
        normalise(&mut stim, counts);
        let report = check(&stim, counts);
        if !report.errors.is_empty() {
            return Err(stim_err(StimError::Invalid(report.errors)));
        }
        if backend != VatBackend::Cpu && state.jobs_running() > 0 {
            return Err(ServerErr::conflict(
                "E_GPU_BUSY",
                "a job is using the GPU; preview with backend cpu or retry after it",
            ));
        }
        let Some(_slot) = state.try_begin_preview() else {
            return Err(ServerErr::conflict(
                "E_PREVIEW_BUSY",
                "another preview is running",
            ));
        };
        let mut opts = RunOptions::new(state.dataset_root(&ds), &sid);
        opts.headless = true;
        opts.checkreg = false;
        opts.no_scene3d = true;
        opts.vat_backend = backend;
        if let Some(t) = state.config().template_t1() {
            opts.normalization_template = Some(t);
        }
        let result =
            preview(&sub, &opts, &stim, &VatParams::default(), true, None).map_err(fail_err)?;
        Ok(Json(preview_json(&label, &result)))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_cover_the_set_voxels() {
        assert_eq!(runs_of(&[0, 1, 1, 0, 1]), vec![[1, 2], [4, 1]]);
        assert_eq!(runs_of(&[1, 1]), vec![[0, 2]]);
        assert!(runs_of(&[0, 0]).is_empty());
        assert!(runs_of(&[]).is_empty());
    }

    #[test]
    fn a_validation_failure_is_422_with_the_rule_code() {
        let stim = Stimulation::blank(
            "gs_x",
            "01",
            dbsloc_pipeline::vat::settings::ModelKind::Efield,
            [4, 4],
        );
        let report = check(&stim, [4, 4]);
        let e = stim_err(StimError::Invalid(report.errors));
        assert_eq!(e.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(e.code(), "E_VAT_settings");
        assert!(e.error.message.contains("gs_"));
    }

    #[test]
    fn upload_names_keep_only_safe_characters() {
        assert_eq!(upload_name(Some("a b/../c.mat")), "ab..c.mat");
        assert_eq!(upload_name(None), "");
    }
}
