// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Connectome endpoints (`specs/15` §4.8): the catalogue with its installed state and a polite,
//! licence-gated download that runs on the server, one transfer at a time. Connectome files are
//! never served to clients and no response carries a server path.

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use dbsloc_api::SSE_END_EVENT;
use dbsloc_pipeline::connectome_catalog::{lookup, Access, ConnectomeEntry};
use dbsloc_pipeline::connectome_fetch::{
    discover_connectomes, fetch_connectome, import_only_error, licence_text_sha256, list_catalogue,
    Acceptance, ConnectomeEvent, ConnectomeState, FetchSettings,
};
use futures::stream::BoxStream;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ServerErr, E_BAD_REQUEST};
use crate::middleware::ClientAddr;
use crate::routes::{audit, blocking};
use crate::state::AppState;

/// The server refuses downloads unless `allow_connectome_download = true`.
pub const E_CONN_DISABLED: &str = "E_CONN_DISABLED";
/// A transfer is already running.
pub const E_CONN_BUSY: &str = "E_CONN_BUSY";
/// The licence hash or the attestation of the request does not match.
pub const E_CONN_TERMS: &str = "E_CONN_TERMS";

const STREAM_POLL: Duration = Duration::from_millis(150);

struct TransferInner {
    events: Vec<Value>,
    finished: Option<Value>,
}

/// The one connectome transfer of the server: its progress events and, once over, its outcome.
pub struct Transfer {
    pub id: String,
    pub cancel: Arc<AtomicBool>,
    inner: Mutex<TransferInner>,
}

impl Transfer {
    /// A new transfer of connectome `id`.
    pub fn new(id: &str) -> Transfer {
        Transfer {
            id: id.to_owned(),
            cancel: Arc::new(AtomicBool::new(false)),
            inner: Mutex::new(TransferInner {
                events: Vec::new(),
                finished: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TransferInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, event: Value) {
        self.lock().events.push(event);
    }

    fn finish(&self, outcome: Value) {
        self.lock().finished = Some(outcome);
    }

    /// Whether the transfer has ended.
    pub fn is_finished(&self) -> bool {
        self.lock().finished.is_some()
    }

    fn since(&self, from: usize) -> (Vec<Value>, Option<Value>) {
        let g = self.lock();
        (
            g.events.get(from..).unwrap_or_default().to_vec(),
            g.finished.clone(),
        )
    }
}

fn event_json(ev: &ConnectomeEvent) -> Value {
    let mut v = serde_json::to_value(ev).unwrap_or(Value::Null);
    if let ConnectomeEvent::Done { path } = ev {
        v["path"] = json!(path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default());
    }
    v
}

fn entry_json(
    entry: &ConnectomeEntry,
    status: &dbsloc_pipeline::connectome_fetch::ConnectomeStatus,
) -> Value {
    let mut v = json!({
        "id": entry.id,
        "name": entry.name,
        "class": entry.class.dir_name(),
        "kind": entry.class.kind_label(),
        "space": entry.space,
        "cohort": entry.cohort,
        "citation": entry.citation,
        "acknowledgement": entry.acknowledgement,
        "licence_name": entry.licence_name,
        "licence_url": entry.licence_url,
        "licence_text": entry.licence_text,
        "licence_text_sha256": licence_text_sha256(entry),
        "access": entry.access.label(),
        "agreement_with": entry.agreement_with,
        "dataset_url": entry.dataset_url,
        "instructions": entry.instructions,
        "version": entry.version,
        "total_bytes": entry.total_bytes(),
        "state": status.state.label(),
    });
    if let ConnectomeState::Partial { percent, bytes } = &status.state {
        v["percent"] = json!(percent);
        v["partial_bytes"] = json!(bytes);
    }
    v
}

/// `GET /connectomes`: the catalogue with the installed state, the connectomes found in the roots
/// (name, class and root kind only) and whether downloads are allowed.
pub async fn list(State(state): State<AppState>) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        let roots = state.config().connectome_roots();
        let installed = discover_connectomes(&roots);
        let rows = list_catalogue(&roots, &installed);
        let catalogue: Vec<Value> = rows.iter().map(|s| entry_json(s.entry, s)).collect();
        let found: Vec<Value> = installed
            .iter()
            .map(|c| {
                json!({
                    "name": c.name,
                    "id": c.id,
                    "class": c.class.dir_name(),
                    "root": c.root_kind.label(),
                    "catalogue_id": c.catalogue_id,
                    "space": c.space,
                    "version": c.version,
                    "missing": c.missing,
                })
            })
            .collect();
        let transfer = state
            .transfer_slot()
            .as_ref()
            .filter(|t| !t.is_finished())
            .map(|t| t.id.clone());
        Ok(Json(json!({
            "download_allowed": state.config().allow_connectome_download,
            "transfer": transfer,
            "catalogue": catalogue,
            "installed": found,
        })))
    })
    .await
}

/// Body of `POST /connectomes/{id}/fetch`.
#[derive(Debug, Deserialize)]
pub struct FetchRequest {
    /// Who accepts the terms; recorded in the audit log.
    pub accepted_by: String,
    /// SHA-256 of the licence text the client showed; must equal the current text.
    pub licence_text_sha256: String,
    /// The attestation of an approved data use agreement (entries of access class `dua`).
    #[serde(default)]
    pub attest: bool,
}

fn audit_name(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect::<String>()
}

/// `POST /connectomes/{id}/fetch`: starts the download on the server and returns 202.
pub async fn fetch(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Extension(client): Extension<ClientAddr>,
    Json(req): Json<FetchRequest>,
) -> Result<Response, ServerErr> {
    blocking(move || {
        if !state.config().allow_connectome_download {
            return Err(ServerErr::new(
                StatusCode::FORBIDDEN,
                E_CONN_DISABLED,
                "connectome downloads are disabled on this server (allow_connectome_download)",
            ));
        }
        let entry = lookup(&id)
            .ok_or_else(|| ServerErr::not_found(format!("no connectome {id} in the catalogue")))?;
        if entry.access == Access::Dua {
            return Err(ServerErr::conflict(
                E_CONN_TERMS,
                import_only_error(entry).message,
            ));
        }
        if req.accepted_by.trim().is_empty() {
            return Err(ServerErr::bad_request(
                E_BAD_REQUEST,
                "accepted_by must name who accepts the terms",
            ));
        }
        if req.licence_text_sha256 != licence_text_sha256(entry) {
            return Err(ServerErr::conflict(
                E_CONN_TERMS,
                "the licence text changed or was not the one shown; reload the catalogue",
            ));
        }
        let acceptance = Acceptance {
            accept_terms: true,
            attest_data_use_agreement: req.attest,
            via: "server",
        };
        dbsloc_pipeline::connectome_fetch::check_acceptance(entry, &acceptance)
            .map_err(|e| ServerErr::conflict(E_CONN_TERMS, e.message))?;
        let transfer = {
            let mut slot = state.transfer_slot();
            if slot.as_ref().is_some_and(|t| !t.is_finished()) {
                return Err(ServerErr::conflict(
                    E_CONN_BUSY,
                    "a connectome transfer is already running",
                ));
            }
            let t = Arc::new(Transfer::new(entry.id));
            *slot = Some(Arc::clone(&t));
            t
        };
        let roots = state.config().connectome_roots();
        let sanitizer = state.sanitizer(None);
        let worker = Arc::clone(&transfer);
        std::thread::spawn(move || {
            let mut on_event = |ev: ConnectomeEvent| worker.push(event_json(&ev));
            let result = fetch_connectome(
                entry,
                &roots,
                &acceptance,
                &FetchSettings::default(),
                &worker.cancel,
                &mut on_event,
            );
            let mut outcome = match result {
                Ok(out) => json!({ "state": "done", "folder": out.folder }),
                Err(e) if e.is_cancelled() => json!({ "state": "cancelled" }),
                Err(e) => json!({
                    "state": "failed",
                    "code": e.code.code(),
                    "message": e.message,
                }),
            };
            sanitizer.value(&mut outcome);
            worker.finish(outcome);
        });
        audit(
            &state,
            client.as_str(),
            None,
            None,
            "connectome_fetch",
            Some(format!(
                "id={} accepted_by={} attest={}",
                entry.id,
                audit_name(&req.accepted_by),
                req.attest
            )),
        );
        Ok((StatusCode::ACCEPTED, Json(json!({ "id": entry.id }))).into_response())
    })
    .await
}

fn current_transfer(state: &AppState, id: &str) -> Result<Arc<Transfer>, ServerErr> {
    state
        .transfer_slot()
        .as_ref()
        .filter(|t| t.id == id)
        .cloned()
        .ok_or_else(|| ServerErr::not_found(format!("no transfer of {id}")))
}

type SseStream = BoxStream<'static, Result<Event, Infallible>>;

fn transfer_stream(transfer: Arc<Transfer>, state: AppState) -> SseStream {
    futures::stream::unfold((transfer, 0usize, false), move |(t, sent, done)| {
        let state = state.clone();
        async move {
            if done {
                return None;
            }
            let mut sent = sent;
            loop {
                let (events, finished) = t.since(sent);
                if !events.is_empty() {
                    let items: Vec<Result<Event, Infallible>> = events
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            Ok(Event::default()
                                .id((sent + i).to_string())
                                .json_data(v)
                                .unwrap_or_else(|_| Event::default().comment("event")))
                        })
                        .collect();
                    sent += events.len();
                    return Some((futures::stream::iter(items).boxed(), (t, sent, false)));
                }
                if let Some(outcome) = finished {
                    let end = Event::default()
                        .event(SSE_END_EVENT)
                        .json_data(&outcome)
                        .unwrap_or_else(|_| Event::default().event(SSE_END_EVENT).data("{}"));
                    return Some((
                        futures::stream::iter(vec![Ok(end)]).boxed(),
                        (t, sent, true),
                    ));
                }
                if state.is_shutdown() {
                    return None;
                }
                tokio::time::sleep(STREAM_POLL).await;
            }
        }
    })
    .flatten()
    .boxed()
}

/// `GET /connectomes/{id}/fetch/events`: SSE of the progress of the current (or last) transfer of
/// `id`: every event so far, then live ones, ending with an `end` event with the outcome.
pub async fn events(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ServerErr> {
    let transfer = current_transfer(&state, &id)?;
    Ok(Sse::new(transfer_stream(transfer, state))
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// `POST /connectomes/{id}/fetch/cancel`: stops the running transfer of `id`; a partial file stays
/// for resume.
pub async fn cancel(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Extension(client): Extension<ClientAddr>,
) -> Result<StatusCode, ServerErr> {
    blocking(move || {
        let transfer = current_transfer(&state, &id)?;
        if transfer.is_finished() {
            return Err(ServerErr::conflict(
                E_CONN_BUSY,
                format!("the transfer of {id} is not running"),
            ));
        }
        transfer.cancel.store(true, Ordering::SeqCst);
        audit(
            &state,
            client.as_str(),
            None,
            None,
            "connectome_cancel",
            Some(format!("id={id}")),
        );
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}
