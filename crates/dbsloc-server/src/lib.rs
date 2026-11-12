// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! `dbsloc-server` — the GPU-server front end of `docs/SERVER_CLIENT_PLAN.md`.
//!
//! WP2 (server core: config, axum router, one pass-through auth-seam middleware, datasets /
//! subjects / plan / files, run queue + SSE + cancel, QC approval + resume, lock approval, GPU
//! probe, audit log) and WP3 (chunked resumable uploads, sha256, NIfTI validation + header scrub,
//! BIDS placement + `rawimages.json` merge with rollback, and a `dcm2niix` convert path that
//! returns `E_DCM2NIIX` when the binary is missing). The built `dbsloc-web` client is served at `/`
//! when `web_dir` is configured.
//!
//! The server is a remote `dbsloc run`: it drives the same `dbsloc-pipeline` stages, locks, checkreg
//! manifest and scene manifest, and it never keeps a second subject catalogue.

#![forbid(unsafe_code)]

pub mod audit;
pub mod config;
pub mod connectomes;
pub mod db;
pub mod dto;
pub mod error;
pub mod files;
pub mod groups;
pub mod middleware;
pub mod multipart;
pub mod routes;
pub mod runs;
pub mod sanitize;
pub mod state;
pub mod stimulations;
pub mod timeutil;
pub mod uploads;
pub mod web;

pub use config::Config;
pub use error::ServerErr;
pub use state::AppState;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};
use axum::{middleware as axum_middleware, Router};

use crate::middleware::{
    audit_middleware, cors_middleware, request_guard_middleware, sanitize_middleware,
};

/// Build the full `/api/v1` router over `state`, wrapped (outermost first) in the Host / Origin
/// request guard, development CORS, the pass-through audit middleware the WP6 auth seam plugs into,
/// and the response sanitizer.
pub fn build_router(state: AppState) -> Router {
    let chunk_limit = state.config().chunk_size as usize + 1024 * 1024;
    let routes = Router::new()
        .route("/api/v1/gpu", get(routes::gpu))
        .route("/api/v1/datasets", get(routes::list_datasets))
        .route("/api/v1/datasets/{ds}/subjects", get(routes::list_subjects))
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}",
            get(routes::subject_summary),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/plan",
            post(routes::plan),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/qc/checkreg",
            get(routes::qc_manifest),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/qc/checkreg/{slide}/approval",
            post(routes::qc_approve),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/locks/{stage}",
            post(routes::lock_approve),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/viz/scene",
            get(routes::viz_scene),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/files/{*key}",
            get(routes::get_dataset_file),
        )
        .route("/api/v1/templates/{*key}", get(routes::get_template_file))
        .route("/api/v1/atlas/{*key}", get(routes::get_atlas_file))
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/runs",
            post(routes::submit_run),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/runs/active",
            get(routes::active_run),
        )
        .route("/api/v1/runs/{id}", get(routes::run_status))
        .route("/api/v1/runs/{id}/events", get(routes::run_events))
        .route("/api/v1/runs/{id}/cancel", post(routes::cancel_run))
        .route(
            "/api/v1/datasets/{ds}/uploads",
            post(routes::upload_init).layer(axum::extract::DefaultBodyLimit::max(1024 * 1024)),
        )
        .route(
            "/api/v1/uploads/{id}/{file}/{chunk}",
            put(routes::upload_chunk).layer(axum::extract::DefaultBodyLimit::max(chunk_limit)),
        )
        .route("/api/v1/uploads/{id}/convert", post(routes::upload_convert))
        .route("/api/v1/uploads/{id}/commit", post(routes::upload_commit))
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/stimulations",
            get(stimulations::list),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/stimulations/import",
            post(stimulations::import).layer(DefaultBodyLimit::max(
                stimulations::MAX_MAT_BYTES + 1024 * 1024,
            )),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/stimulations/{label}",
            get(stimulations::get_one)
                .put(stimulations::put_one)
                .layer(DefaultBodyLimit::max(stimulations::MAX_SETTINGS_BYTES)),
        )
        .route(
            "/api/v1/datasets/{ds}/subjects/{sid}/stimulations/{label}/preview",
            post(stimulations::preview_one)
                .layer(DefaultBodyLimit::max(stimulations::MAX_SETTINGS_BYTES)),
        )
        .route(
            "/api/v1/datasets/{ds}/groups",
            get(groups::list)
                .post(groups::create)
                .layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route(
            "/api/v1/datasets/{ds}/groups/{guid}",
            get(groups::get_one)
                .put(groups::put_one)
                .layer(DefaultBodyLimit::max(groups::MAX_RECORD_BYTES)),
        )
        .route(
            "/api/v1/datasets/{ds}/groups/{guid}/clinical",
            post(groups::clinical).layer(DefaultBodyLimit::max(groups::MAX_CLINICAL_BYTES)),
        )
        .route(
            "/api/v1/datasets/{ds}/groups/{guid}/jobs",
            post(groups::submit_job),
        )
        .route(
            "/api/v1/datasets/{ds}/groups/{guid}/jobs/active",
            get(groups::active_job),
        )
        .route(
            "/api/v1/datasets/{ds}/groups/{guid}/jobs/{id}",
            get(groups::job_status),
        )
        .route(
            "/api/v1/datasets/{ds}/groups/{guid}/jobs/{id}/events",
            get(groups::job_events),
        )
        .route(
            "/api/v1/datasets/{ds}/groups/{guid}/jobs/{id}/cancel",
            post(groups::job_cancel),
        )
        .route(
            "/api/v1/datasets/{ds}/groups/{guid}/files/{*key}",
            get(groups::get_file),
        )
        .route("/api/v1/connectomes", get(connectomes::list))
        .route("/api/v1/connectomes/{id}/fetch", post(connectomes::fetch))
        .route(
            "/api/v1/connectomes/{id}/fetch/events",
            get(connectomes::events),
        )
        .route(
            "/api/v1/connectomes/{id}/fetch/cancel",
            post(connectomes::cancel),
        )
        .fallback(web::fallback);

    routes
        .with_state(state.clone())
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            sanitize_middleware,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            audit_middleware,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            cors_middleware,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state,
            request_guard_middleware,
        ))
}
