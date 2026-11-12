// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Lead Group endpoints and group jobs (`specs/14` §4.6, §10.3, §10.6): the group record JSON and
//! its Lead Group twin with dataset-relative patient paths, clinical table uploads, and one job per
//! cohort analysis run by one runner subprocess through the shared job queue.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use dbsloc_api::RunStatus;
use dbsloc_pipeline::group::clinical::{import_table, ImportOptions};
use dbsloc_pipeline::group::record::{
    find_record_file, is_alphanumeric_id, subject_name, utc_digits_now, SCHEMA,
};
use dbsloc_pipeline::group::{
    create_group, load_record, load_subject_data, locate, open_group, save_record, write_m_twin,
    GroupError, GroupRecord, Note,
};
use dbsloc_pipeline::protocol::RunEvent;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::db::{JobRow, NewJob};
use crate::error::{ServerErr, E_BAD_KEY, E_BAD_REQUEST, E_RUN_ACTIVE};
use crate::files::{resolve_in_root, serve};
use crate::middleware::ClientAddr;
use crate::routes::{audit, blocking, require_dataset};
use crate::runs::{canonical_subject, new_id};
use crate::state::AppState;

/// Prefix of the job subject of a group job (`@group:<guid>`); never a valid BIDS label, so it
/// cannot collide with a subject run, and the one-active-job rule then holds per group.
pub const GROUP_SUBJECT_PREFIX: &str = "@group:";
/// The group already exists.
pub const E_GROUP_EXISTS: &str = "E_GROUP_EXISTS";
/// Largest accepted clinical table.
pub const MAX_CLINICAL_BYTES: usize = 16 * 1024 * 1024;
/// Largest accepted group record JSON.
pub const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;
/// The job kinds of `specs/14` §4.6.
pub const JOB_KINDS: [&str; 6] = [
    "group_calc",
    "group_stats",
    "sweetspot",
    "fibrefilter",
    "netmap",
    "export",
];

const DEFAULT_DERIVATIVES: &str = "derivatives/leaddbs";
const CANCEL_GRACE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(25);

fn group_err(e: GroupError) -> ServerErr {
    match e {
        GroupError::Lg { code, message } => {
            ServerErr::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
        }
        GroupError::Invalid(m) | GroupError::Clinical(m) | GroupError::Json(m) => {
            ServerErr::new(StatusCode::UNPROCESSABLE_ENTITY, "E_GROUP", m)
        }
        GroupError::Mat(m) => ServerErr::new(StatusCode::UNPROCESSABLE_ENTITY, "E_GROUP", m),
        GroupError::Io(m) => ServerErr::internal(m),
    }
}

fn group_dir(state: &AppState, ds: &str, guid: &str) -> Result<PathBuf, ServerErr> {
    if !is_alphanumeric_id(guid) {
        return Err(ServerErr::bad_request(
            E_BAD_KEY,
            "a group id is made of letters and digits only",
        ));
    }
    Ok(state
        .dataset_root(ds)
        .join("derivatives")
        .join("leadgroup")
        .join(guid))
}

fn job_subject(guid: &str) -> String {
    format!("{GROUP_SUBJECT_PREFIX}{guid}")
}

fn existing_record_file(state: &AppState, ds: &str, guid: &str) -> Result<PathBuf, ServerErr> {
    require_dataset(state, ds)?;
    let dir = group_dir(state, ds, guid)?;
    if !dir.is_dir() {
        return Err(ServerErr::not_found(format!("no group {guid} in {ds}")));
    }
    find_record_file(&dir).map_err(|_| ServerErr::not_found(format!("no group {guid} in {ds}")))
}

fn norm_path(s: &str) -> String {
    s.replace('\\', "/").trim_end_matches('/').to_owned()
}

fn dataset_root_text(root: &Path) -> String {
    norm_path(&root.to_string_lossy())
}

fn relative_derivatives(root: &str, stored: &str) -> String {
    let abs = norm_path(stored);
    let fold = |s: &str| {
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s.to_owned()
        }
    };
    let prefix = format!("{}/", fold(root));
    if fold(&abs).starts_with(&prefix) {
        abs[prefix.len()..].to_owned()
    } else {
        DEFAULT_DERIVATIVES.to_owned()
    }
}

/// The record as clients see it: dataset roots are dataset-relative (`root` empty, the derivatives
/// folder relative to the dataset), never server paths.
pub fn record_dto(record: &GroupRecord, dataset_root: &Path) -> Value {
    let root = dataset_root_text(dataset_root);
    let mut v = serde_json::to_value(record).unwrap_or(Value::Null);
    if let Some(list) = v.get_mut("datasets").and_then(Value::as_array_mut) {
        for d in list {
            let stored = d["derivatives_root"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            d["root"] = json!("");
            d["derivatives_root"] = json!(relative_derivatives(&root, &stored));
        }
    }
    v
}

fn safe_relative(rel: &str) -> Result<String, ServerErr> {
    let rel = rel.trim().trim_end_matches(['/', '\\']);
    let rel = if rel.is_empty() {
        DEFAULT_DERIVATIVES
    } else {
        rel
    };
    if crate::sanitize::looks_absolute(rel)
        || rel.contains(':')
        || rel.contains('\\')
        || rel.split('/').any(|c| c == ".." || c.is_empty())
    {
        return Err(ServerErr::bad_request(
            E_BAD_KEY,
            format!("dataset folder {rel:?} must be a relative folder inside the dataset"),
        ));
    }
    Ok(rel.to_owned())
}

/// Turn a record a client sent into the stored form: dataset roots are forced onto the server's
/// dataset, patients are canonical `sub-<id>` names, and the fields only the server writes (the
/// Lead Group twin state, the analyses, the creation time) are kept from `stored`.
pub fn normalize_incoming(
    mut incoming: GroupRecord,
    stored: &GroupRecord,
    guid: &str,
    dataset_root: &Path,
) -> Result<GroupRecord, ServerErr> {
    if incoming.schema != SCHEMA {
        return Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            format!("schema {:?} is not {SCHEMA}", incoming.schema),
        ));
    }
    if incoming.guid != guid {
        return Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            format!("the record holds guid {:?}, not {guid:?}", incoming.guid),
        ));
    }
    if incoming.dataset_name != stored.dataset_name {
        return Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            "the dataset name of a group cannot change",
        ));
    }
    let root = dataset_root_text(dataset_root);
    for d in &mut incoming.datasets {
        let rel = safe_relative(&d.derivatives_root)?;
        d.root.clone_from(&root);
        d.derivatives_root = format!("{root}/{rel}");
    }
    for p in &mut incoming.patients {
        let id = canonical_subject(&p.subject)?;
        p.subject = subject_name(&id);
    }
    incoming.lead_group = stored.lead_group.clone();
    incoming.analyses.clone_from(&stored.analyses);
    incoming.created_at.clone_from(&stored.created_at);
    incoming.validate().map_err(group_err)?;
    Ok(incoming)
}

fn relative_clone(record: &GroupRecord, dataset_root: &Path) -> GroupRecord {
    let root = dataset_root_text(dataset_root);
    let mut rel = record.clone();
    for d in &mut rel.datasets {
        let stored = d.derivatives_root.clone();
        d.root = String::new();
        d.derivatives_root = relative_derivatives(&root, &stored);
    }
    rel
}

/// Save the record and write its Lead Group twin with `patient.list` entries relative to the
/// dataset (`specs/14` §4.6): the patient files are read through the absolute record, the twin is
/// built from a relative copy. A twin failure leaves the saved record and is returned as a note.
pub fn save_record_and_twin(
    json_path: &Path,
    record: &mut GroupRecord,
    dataset_root: &Path,
) -> Result<Vec<Note>, ServerErr> {
    let folder = json_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let (subjects, mut notes) = load_subject_data(record);
    let mut rel = relative_clone(record, dataset_root);
    match write_m_twin(&mut rel, &subjects, &folder) {
        Ok(report) => {
            record.lead_group = rel.lead_group;
            notes.extend(report.warnings);
        }
        Err(e) => notes.push(Note::plain(format!(
            "the Lead Group file was not written: {e}"
        ))),
    }
    save_record(json_path, record).map_err(group_err)?;
    Ok(notes)
}

fn notes_json(notes: &[Note]) -> Vec<String> {
    notes.iter().map(ToString::to_string).collect()
}

fn refuse_active_job(state: &AppState, ds: &str, guid: &str) -> Result<(), ServerErr> {
    if state
        .db()
        .active_for_subject(ds, &job_subject(guid))?
        .is_some()
    {
        return Err(ServerErr::conflict(
            E_RUN_ACTIVE,
            format!("group {guid} has a queued or running job"),
        ));
    }
    Ok(())
}

/// Body of `POST /datasets/{ds}/groups`.
#[derive(Debug, Default, Deserialize)]
pub struct CreateRequest {
    #[serde(default)]
    pub guid: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub atlas_set: Option<String>,
    #[serde(default)]
    pub dataset_name: Option<String>,
}

/// `POST /datasets/{ds}/groups`
pub async fn create(
    State(state): State<AppState>,
    AxumPath(ds): AxumPath<String>,
    Extension(client): Extension<ClientAddr>,
    body: Bytes,
) -> Result<Response, ServerErr> {
    blocking(move || {
        let root = require_dataset(&state, &ds)?;
        let req: CreateRequest = if body.iter().all(u8::is_ascii_whitespace) {
            CreateRequest::default()
        } else {
            serde_json::from_slice(&body)
                .map_err(|e| ServerErr::bad_request(E_BAD_REQUEST, format!("group request: {e}")))?
        };
        let guid = req.guid.clone().unwrap_or_else(utc_digits_now);
        if !is_alphanumeric_id(&guid) {
            return Err(ServerErr::bad_request(
                E_BAD_KEY,
                "a group id is made of letters and digits only",
            ));
        }
        let dir = group_dir(&state, &ds, &guid)?;
        if dir.exists() {
            return Err(ServerErr::conflict(
                E_GROUP_EXISTS,
                format!("group {guid} already exists in {ds}"),
            ));
        }
        let location = locate(
            &root,
            &guid,
            None,
            req.dataset_name.as_deref().or(Some(&ds)),
        )
        .map_err(group_err)?;
        let (mut record, json_path, mut notes) =
            create_group(&location, &req.title, req.atlas_set.as_deref()).map_err(group_err)?;
        let root_text = dataset_root_text(&root);
        record.ensure_dataset(&root_text, &format!("{root_text}/{DEFAULT_DERIVATIVES}"));
        notes.extend(save_record_and_twin(&json_path, &mut record, &root)?);
        audit(
            &state,
            client.as_str(),
            Some(&ds),
            None,
            "group_create",
            Some(format!("guid={guid}")),
        );
        let out = json!({
            "guid": guid,
            "record": record_dto(&record, &root),
            "warnings": notes_json(&notes),
        });
        Ok((StatusCode::CREATED, Json(out)).into_response())
    })
    .await
}

/// `GET /datasets/{ds}/groups`
pub async fn list(
    State(state): State<AppState>,
    AxumPath(ds): AxumPath<String>,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        let root = require_dataset(&state, &ds)?;
        let base = root.join("derivatives").join("leadgroup");
        let mut rows = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&base) {
            let mut dirs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            for dir in dirs {
                let Ok(file) = find_record_file(&dir) else {
                    continue;
                };
                let Ok(r) = load_record(&file) else { continue };
                rows.push(json!({
                    "guid": r.guid,
                    "title": r.title,
                    "dataset_name": r.dataset_name,
                    "patients": r.patients.len(),
                    "groups": r.groups.len(),
                    "clinical_variables": r.clinical.len(),
                    "analyses": r.analyses.len(),
                    "updated_at": r.updated_at,
                }));
            }
        }
        Ok(Json(Value::Array(rows)))
    })
    .await
}

/// `GET /datasets/{ds}/groups/{guid}`
pub async fn get_one(
    State(state): State<AppState>,
    AxumPath((ds, guid)): AxumPath<(String, String)>,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        let file = existing_record_file(&state, &ds, &guid)?;
        let record = load_record(&file).map_err(group_err)?;
        Ok(Json(record_dto(&record, &state.dataset_root(&ds))))
    })
    .await
}

/// `PUT /datasets/{ds}/groups/{guid}`: replaces the record (the JSON of `GET`); the server
/// rewrites the Lead Group twin with dataset-relative paths.
pub async fn put_one(
    State(state): State<AppState>,
    AxumPath((ds, guid)): AxumPath<(String, String)>,
    Extension(client): Extension<ClientAddr>,
    body: Bytes,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        let file = existing_record_file(&state, &ds, &guid)?;
        let incoming: GroupRecord = serde_json::from_slice(&body)
            .map_err(|e| ServerErr::bad_request(E_BAD_REQUEST, format!("group record: {e}")))?;
        let lock = state.subject_lock(&ds, &job_subject(&guid));
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        refuse_active_job(&state, &ds, &guid)?;
        let root = state.dataset_root(&ds);
        let stored = load_record(&file).map_err(group_err)?;
        let mut record = normalize_incoming(incoming, &stored, &guid, &root)?;
        let notes = save_record_and_twin(&file, &mut record, &root)?;
        audit(
            &state,
            client.as_str(),
            Some(&ds),
            None,
            "group_put",
            Some(format!("guid={guid} patients={}", record.patients.len())),
        );
        Ok(Json(json!({
            "record": record_dto(&record, &root),
            "warnings": notes_json(&notes),
        })))
    })
    .await
}

/// Query of the clinical upload.
#[derive(Debug, Default, Deserialize)]
pub struct ClinicalQuery {
    #[serde(default)]
    pub replace: bool,
    #[serde(default)]
    pub subject_column: Option<String>,
    /// `tsv` (default) or `csv`; a multipart file name with a `.csv` extension also selects csv.
    #[serde(default)]
    pub format: Option<String>,
}

/// `POST /datasets/{ds}/groups/{guid}/clinical`: a TSV or CSV table with a subject column
/// (`specs/14` §10.3), as a multipart file or the raw body. The upload is recorded in the audit
/// log with the group id and the variable count only, never the values.
pub async fn clinical(
    State(state): State<AppState>,
    AxumPath((ds, guid)): AxumPath<(String, String)>,
    Query(query): Query<ClinicalQuery>,
    Extension(client): Extension<ClientAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ServerErr> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    blocking(move || {
        let file = existing_record_file(&state, &ds, &guid)?;
        let upload = crate::multipart::read_upload(content_type.as_deref(), &body)?;
        let csv = match query.format.as_deref() {
            Some("csv") => true,
            Some("tsv") | None => upload
                .filename
                .as_deref()
                .is_some_and(|n| n.to_ascii_lowercase().ends_with(".csv")),
            Some(other) => {
                return Err(ServerErr::bad_request(
                    E_BAD_REQUEST,
                    format!("format {other:?} is not tsv or csv"),
                ))
            }
        };
        let text = String::from_utf8(upload.bytes)
            .map_err(|_| ServerErr::bad_request(E_BAD_REQUEST, "the table is not UTF-8 text"))?;
        let lock = state.subject_lock(&ds, &job_subject(&guid));
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        refuse_active_job(&state, &ds, &guid)?;
        let root = state.dataset_root(&ds);
        let mut opened = open_group(&file).map_err(group_err)?;
        let report = import_table(
            &mut opened.record,
            &text,
            if csv { ',' } else { '\t' },
            &ImportOptions {
                replace: query.replace,
                subject_column: query
                    .subject_column
                    .clone()
                    .filter(|s| !s.trim().is_empty()),
                source: upload.filename.clone().map_or_else(
                    || "upload".to_owned(),
                    |n| {
                        n.chars()
                            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                            .take(96)
                            .collect()
                    },
                ),
            },
        )
        .map_err(|e| group_err(GroupError::from(e)))?;
        let notes = save_record_and_twin(&file, &mut opened.record, &root)?;
        audit(
            &state,
            client.as_str(),
            Some(&ds),
            None,
            "clinical_upload",
            Some(format!(
                "guid={guid} imported={} replaced={}",
                report.imported.len(),
                report.replaced.len()
            )),
        );
        Ok(Json(json!({
            "imported": report.imported,
            "replaced": report.replaced,
            "unknown_subjects": report.unknown_subjects,
            "patients_without_row": report.patients_without_row,
            "warnings": notes_json(&notes),
        })))
    })
    .await
}

fn plain_text(v: &Value, what: &str, max: usize) -> Result<String, ServerErr> {
    let s = v
        .as_str()
        .ok_or_else(|| ServerErr::bad_request(E_BAD_REQUEST, format!("{what} must be a string")))?;
    if s.trim().is_empty() || s.chars().count() > max || s.chars().any(char::is_control) {
        return Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            format!("{what} must be 1-{max} printable characters"),
        ));
    }
    Ok(s.to_owned())
}

fn token(v: &Value, what: &str) -> Result<String, ServerErr> {
    let s = plain_text(v, what, 64)?;
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && !s.starts_with('.')
    {
        Ok(s)
    } else {
        Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            format!("{what} must be made of [A-Za-z0-9._-]"),
        ))
    }
}

fn connectome_name(v: &Value) -> Result<String, ServerErr> {
    let s = plain_text(v, "connectome", 128)?;
    if s.contains(['/', '\\', ':']) || s.contains("..") {
        return Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            "connectome is a catalogue id or an installed name, never a path",
        ));
    }
    Ok(s)
}

fn flag(v: &Value, what: &str) -> Result<bool, ServerErr> {
    v.as_bool().ok_or_else(|| {
        ServerErr::bad_request(E_BAD_REQUEST, format!("{what} must be true or false"))
    })
}

fn string_list(v: &Value, what: &str, max: usize) -> Result<Vec<String>, ServerErr> {
    let a = v
        .as_array()
        .ok_or_else(|| ServerErr::bad_request(E_BAD_REQUEST, format!("{what} must be a list")))?;
    a.iter().map(|x| plain_text(x, what, max)).collect()
}

fn subject_list(v: &Value) -> Result<Vec<String>, ServerErr> {
    string_list(v, "subjects", 64)?
        .iter()
        .map(|s| canonical_subject(s).map(|id| subject_name(&id)))
        .collect()
}

fn object(v: &Value, what: &str) -> Result<Value, ServerErr> {
    if v.is_object() {
        Ok(v.clone())
    } else {
        Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            format!("{what} must be an object"),
        ))
    }
}

fn backend(v: &Value) -> Result<String, ServerErr> {
    let s = plain_text(v, "vat_backend", 8)?;
    dbsloc_pipeline::options::VatBackend::parse(&s)
        .map(|b| b.as_str().to_owned())
        .map_err(|m| ServerErr::bad_request(E_BAD_REQUEST, m))
}

/// Validate the options of a job kind and return the normalised object the runner receives;
/// unknown keys are refused.
pub fn validate_job(kind: &str, options: &Value) -> Result<Value, ServerErr> {
    if !JOB_KINDS.contains(&kind) {
        return Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            format!("kind {kind:?} is not one of {}", JOB_KINDS.join(", ")),
        ));
    }
    let empty = Map::new();
    let given = match options {
        Value::Null => &empty,
        Value::Object(m) => m,
        _ => {
            return Err(ServerErr::bad_request(
                E_BAD_REQUEST,
                "options must be an object",
            ))
        }
    };
    let mut out = Map::new();
    for (key, v) in given {
        let normalised: Value = match (kind, key.as_str()) {
            ("group_calc", "subjects") | ("sweetspot", "subjects") => json!(subject_list(v)?),
            (
                "group_calc",
                "stop_on_first_failure" | "contact_distances" | "mirror" | "sync_settings",
            )
            | ("group_stats", "csv" | "lead_dbs" | "contact_distances")
            | ("fibrefilter" | "netmap", "mirror") => json!(flag(v, key)?),
            ("group_calc", "flip_transform") => match v.as_str() {
                Some(s @ ("auto" | "plain")) => json!(s),
                _ => {
                    return Err(ServerErr::bad_request(
                        E_BAD_REQUEST,
                        "flip_transform is auto or plain",
                    ))
                }
            },
            ("group_calc", "copy_settings_from") => {
                let label = plain_text(v, key, 64)?;
                if !dbsloc_pipeline::vat::settings::label_is_valid(&label) {
                    return Err(ServerErr::bad_request(E_BAD_REQUEST, "invalid label"));
                }
                json!(label)
            }
            ("group_calc" | "group_stats", "vat_backend") => json!(backend(v)?),
            ("sweetspot" | "fibrefilter" | "netmap", "variable") => {
                json!(plain_text(v, key, 128)?)
            }
            ("sweetspot" | "fibrefilter" | "netmap", "covariates") => {
                json!(string_list(v, key, 128)?)
            }
            ("sweetspot", "preset")
            | ("sweetspot" | "fibrefilter" | "netmap", "space" | "model") => {
                json!(token(v, key)?)
            }
            ("sweetspot" | "fibrefilter" | "netmap", "settings") => object(v, key)?,
            ("fibrefilter" | "netmap", "scheme") => object(v, key)?,
            ("fibrefilter" | "netmap", "seed") => match v.as_u64() {
                Some(n) => json!(n),
                None => {
                    return Err(ServerErr::bad_request(
                        E_BAD_REQUEST,
                        "seed must be a non-negative integer",
                    ))
                }
            },
            ("fibrefilter" | "netmap", "connectome") => json!(connectome_name(v)?),
            ("netmap", "mask") => match v.as_str() {
                Some(s @ ("grey" | "brain" | "all")) => json!(s),
                _ => {
                    return Err(ServerErr::bad_request(
                        E_BAD_REQUEST,
                        "mask is grey, brain or all on the server",
                    ))
                }
            },
            ("export", "only") => match v.as_str() {
                Some(s @ ("twin" | "clinical")) => json!(s),
                _ => {
                    return Err(ServerErr::bad_request(
                        E_BAD_REQUEST,
                        "only is twin or clinical",
                    ))
                }
            },
            _ => {
                return Err(ServerErr::bad_request(
                    E_BAD_REQUEST,
                    format!("option {key:?} is not known for {kind}"),
                ))
            }
        };
        out.insert(key.clone(), normalised);
    }
    if out.get("sync_settings") == Some(&json!(true)) && !out.contains_key("copy_settings_from") {
        return Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            "sync_settings needs copy_settings_from",
        ));
    }
    let required: &[&str] = match kind {
        "sweetspot" => &["variable"],
        "fibrefilter" | "netmap" => &["variable", "connectome"],
        _ => &[],
    };
    for r in required {
        if !out.contains_key(*r) {
            return Err(ServerErr::bad_request(
                E_BAD_REQUEST,
                format!("{kind} needs the option {r}"),
            ));
        }
    }
    Ok(Value::Object(out))
}

/// Body of `POST /datasets/{ds}/groups/{guid}/jobs`.
#[derive(Debug, Deserialize)]
pub struct JobRequest {
    pub kind: String,
    #[serde(default)]
    pub options: Value,
}

/// `POST /datasets/{ds}/groups/{guid}/jobs`: queues one cohort analysis; 202 with the job id.
pub async fn submit_job(
    State(state): State<AppState>,
    AxumPath((ds, guid)): AxumPath<(String, String)>,
    Extension(client): Extension<ClientAddr>,
    Json(req): Json<JobRequest>,
) -> Result<Response, ServerErr> {
    blocking(move || {
        let file = existing_record_file(&state, &ds, &guid)?;
        let options = validate_job(&req.kind, &req.options)?;
        let spec = json!({
            "group_job": {
                "kind": req.kind,
                "group_file": file.to_string_lossy(),
                "options": options,
            }
        });
        let id = new_id();
        let blocking_job = state.db().insert_unless_active(&NewJob {
            id: id.clone(),
            dataset: ds.clone(),
            subject: job_subject(&guid),
            options: spec.to_string(),
            priority: 1,
            resume: false,
            gated_stage: None,
            client: Some(client.as_str().to_owned()),
        })?;
        if blocking_job.is_some() {
            return Err(ServerErr::conflict(
                E_RUN_ACTIVE,
                format!("group {guid} already has a queued or running job"),
            ));
        }
        state.runs().get_or_create(&id, state.run_ndjson(&ds, &id));
        audit(
            &state,
            client.as_str(),
            Some(&ds),
            None,
            "group_job_submit",
            Some(format!("id={id} guid={guid} kind={}", req.kind)),
        );
        Ok((StatusCode::ACCEPTED, Json(json!({ "id": id }))).into_response())
    })
    .await
}

fn job_of_group(state: &AppState, ds: &str, guid: &str, id: &str) -> Result<JobRow, ServerErr> {
    require_dataset(state, ds)?;
    if !is_alphanumeric_id(guid) {
        return Err(ServerErr::bad_request(E_BAD_KEY, "invalid group id"));
    }
    state
        .db()
        .get(id)?
        .filter(|j| j.dataset == ds && j.subject == job_subject(guid))
        .ok_or_else(|| ServerErr::not_found(format!("no job {id} of group {guid}")))
}

/// `GET /datasets/{ds}/groups/{guid}/jobs/active`: the queued or running job of the group, or null.
pub async fn active_job(
    State(state): State<AppState>,
    AxumPath((ds, guid)): AxumPath<(String, String)>,
) -> Result<Json<Option<RunStatus>>, ServerErr> {
    blocking(move || {
        require_dataset(&state, &ds)?;
        if !is_alphanumeric_id(&guid) {
            return Err(ServerErr::bad_request(E_BAD_KEY, "invalid group id"));
        }
        Ok(Json(
            state
                .db()
                .active_for_subject(&ds, &job_subject(&guid))?
                .map(|j| crate::dto::run_status(&j)),
        ))
    })
    .await
}

/// `GET /datasets/{ds}/groups/{guid}/jobs/{id}`
pub async fn job_status(
    State(state): State<AppState>,
    AxumPath((ds, guid, id)): AxumPath<(String, String, String)>,
) -> Result<Json<RunStatus>, ServerErr> {
    blocking(move || {
        let job = job_of_group(&state, &ds, &guid, &id)?;
        Ok(Json(crate::dto::run_status(&job)))
    })
    .await
}

/// `GET /datasets/{ds}/groups/{guid}/jobs/{id}/events`: SSE of the job events, replayed then live,
/// ending with the `end` event (same stream as `/runs/{id}/events`).
pub async fn job_events(
    State(state): State<AppState>,
    AxumPath((ds, guid, id)): AxumPath<(String, String, String)>,
    Query(query): Query<crate::routes::EventsQuery>,
    headers: HeaderMap,
) -> Result<Response, ServerErr> {
    let last = crate::routes::last_event_id(&headers, &query);
    let stream = blocking(move || {
        job_of_group(&state, &ds, &guid, &id)?;
        crate::runs::event_stream(&state, &id, last)
    })
    .await?;
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// `POST /datasets/{ds}/groups/{guid}/jobs/{id}/cancel`
pub async fn job_cancel(
    State(state): State<AppState>,
    AxumPath((ds, guid, id)): AxumPath<(String, String, String)>,
    Extension(client): Extension<ClientAddr>,
) -> Result<StatusCode, ServerErr> {
    blocking(move || {
        job_of_group(&state, &ds, &guid, &id)?;
        crate::runs::cancel(&state, &id)?;
        audit(
            &state,
            client.as_str(),
            Some(&ds),
            None,
            "group_job_cancel",
            Some(format!("id={id}")),
        );
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

/// `GET /datasets/{ds}/groups/{guid}/files/{*key}`: a file under `derivatives/leadgroup/<guid>/`
/// (the record, the twin, exports and analysis outputs). Work files are not served; reading a
/// clinical table is audit-logged.
pub async fn get_file(
    State(state): State<AppState>,
    AxumPath((ds, guid, key)): AxumPath<(String, String, String)>,
    Extension(client): Extension<ClientAddr>,
    headers: HeaderMap,
) -> Result<Response, ServerErr> {
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    blocking(move || {
        require_dataset(&state, &ds)?;
        let dir = group_dir(&state, &ds, &guid)?;
        if key.split('/').any(|c| c.starts_with(".partial-")) {
            return Err(ServerErr::new(
                StatusCode::FORBIDDEN,
                E_BAD_KEY,
                "work files are not served",
            ));
        }
        let path = resolve_in_root(&dir, &key)?;
        if key.to_ascii_lowercase().contains("clinical") {
            audit(
                &state,
                client.as_str(),
                Some(&ds),
                None,
                "clinical_read",
                Some(format!("guid={guid}")),
            );
        }
        serve(&path, range.as_deref())
    })
    .await
}

/// Runner subprocess of a group job: writes the job line, relays the runner's `RunEvent` lines to
/// `tx`, and on `cancel` sends the runner a `cancel` line and kills it after a grace period.
pub fn run_group_subprocess(
    mut cmd: std::process::Command,
    spec_line: &str,
    tx: &mpsc::Sender<RunEvent>,
    cancel: &Arc<AtomicBool>,
) -> std::io::Result<std::process::ExitStatus> {
    use std::sync::Mutex;
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    if let Some(w) = stdin.as_mut() {
        let sent = w
            .write_all(spec_line.as_bytes())
            .and_then(|()| w.write_all(b"\n"))
            .and_then(|()| w.flush());
        if let Err(e) = sent {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    }
    let child = Arc::new(Mutex::new(child));
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let child = Arc::clone(&child);
        let stop = Arc::clone(&stop);
        let cancel = Arc::clone(cancel);
        std::thread::spawn(move || {
            let mut stdin = stdin;
            while !stop.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    if let Some(w) = stdin.as_mut() {
                        let _ = w.write_all(b"cancel\n").and_then(|()| w.flush());
                    }
                    let deadline = std::time::Instant::now() + CANCEL_GRACE;
                    while !stop.load(Ordering::SeqCst) {
                        if std::time::Instant::now() >= deadline {
                            let _ = child.lock().unwrap_or_else(|p| p.into_inner()).kill();
                            break;
                        }
                        std::thread::sleep(POLL);
                    }
                    return;
                }
                std::thread::sleep(POLL);
            }
        })
    };
    if let Some(stdout) = stdout {
        let mut reader = std::io::BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    if let Ok(ev) = serde_json::from_str::<RunEvent>(line.trim_end()) {
                        let _ = tx.send(ev);
                    }
                }
            }
        }
    }
    let status = loop {
        let polled = child.lock().unwrap_or_else(|p| p.into_inner()).try_wait();
        match polled {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                let mut c = child.lock().unwrap_or_else(|p| p.into_inner());
                let _ = c.kill();
                let _ = c.wait();
                break Err(e);
            }
        }
    };
    stop.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    status
}

/// Run a queued group job: start the runner on the job line, drive the events through the shared
/// job machinery, then refresh the Lead Group twin with dataset-relative paths.
pub fn run_group_job(state: &AppState, job: JobRow) {
    let ndjson = state.run_ndjson(&job.dataset, &job.id);
    if let Some(parent) = ndjson.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ctx = state.runs().get_or_create(&job.id, ndjson);
    let cancel = state.register_cancel(&job.id);
    let spec = serde_json::from_str::<Value>(&job.options).ok();
    let group_file = spec
        .as_ref()
        .and_then(|s| s["group_job"]["group_file"].as_str())
        .map(PathBuf::from);
    let (tx, rx) = mpsc::channel::<RunEvent>();
    let runner = {
        let cancel = Arc::clone(&cancel);
        let mut cmd =
            crate::runs::runner_command(state.runner_bin(), std::env::vars_os().map(|(k, _)| k));
        let config = state.config();
        cmd.env(
            dbsloc_pipeline::connectome_fetch::CONNECTOMES_DIR_ENV,
            config.connectomes_dir(),
        );
        if let Some(d) = &config.leaddbs_connectomes_dir {
            cmd.env(
                dbsloc_pipeline::connectome_fetch::LEADDBS_CONNECTOMES_ENV,
                d,
            );
        }
        let line = job.options.clone();
        std::thread::spawn(move || run_group_subprocess(cmd, &line, &tx, &cancel))
    };
    crate::runs::drive_job(state, &job, &ctx, &cancel, rx, runner);
    if let Some(file) = group_file.filter(|f| f.is_file()) {
        let root = state.dataset_root(&job.dataset);
        if let Ok(mut record) = load_record(&file) {
            let _ = save_record_and_twin(&file, &mut record, &root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_options_are_validated_per_kind() {
        let ok = validate_job(
            "group_calc",
            &json!({"mirror": true, "subjects": ["01", "sub-02"]}),
        )
        .unwrap();
        assert_eq!(ok["subjects"], json!(["sub-01", "sub-02"]));
        assert!(validate_job("group_calc", &json!({"nope": 1})).is_err());
        assert!(validate_job("group_calc", &json!({"mirror": "yes"})).is_err());
        assert!(validate_job(
            "group_calc",
            &json!({"sync_settings": true, "copy_settings_from": "demo"})
        )
        .is_ok());
        assert!(validate_job("group_calc", &json!({"sync_settings": true})).is_err());
        assert!(validate_job("group_calc", &json!({"sync_settings": false})).is_ok());
        assert!(validate_job(
            "group_calc",
            &json!({"sync_settings": "yes", "copy_settings_from": "demo"})
        )
        .is_err());
        assert!(validate_job("sweetspot", &json!({})).is_err());
        assert!(validate_job("sweetspot", &json!({"variable": "UPDRS"})).is_ok());
        assert!(validate_job("fibrefilter", &json!({"variable": "x"})).is_err());
        assert!(validate_job(
            "fibrefilter",
            &json!({"variable": "x", "connectome": "../etc"})
        )
        .is_err());
        assert!(validate_job(
            "netmap",
            &json!({"variable": "x", "connectome": "GSP", "mask": "/tmp/m.nii"})
        )
        .is_err());
        assert!(validate_job("export", &json!({"only": "twin"})).is_ok());
        assert!(validate_job("bogus", &json!({})).is_err());
        assert!(validate_job("export", &Value::Null).is_ok());
    }

    #[test]
    fn dataset_roots_become_relative_and_escapes_are_refused() {
        assert_eq!(
            relative_derivatives("/srv/ds", "/srv/ds/derivatives/leaddbs"),
            "derivatives/leaddbs"
        );
        assert_eq!(
            relative_derivatives("C:/data/ds", "C:\\data\\ds\\derivatives\\other"),
            "derivatives/other"
        );
        assert_eq!(
            relative_derivatives("/srv/ds", "/elsewhere/x"),
            DEFAULT_DERIVATIVES
        );
        assert!(safe_relative("../x").is_err());
        assert!(safe_relative("/abs").is_err());
        assert!(safe_relative("a\\b").is_err());
        assert_eq!(safe_relative("").unwrap(), DEFAULT_DERIVATIVES);
        assert_eq!(
            safe_relative("derivatives/leaddbs/").unwrap(),
            "derivatives/leaddbs"
        );
    }
}
