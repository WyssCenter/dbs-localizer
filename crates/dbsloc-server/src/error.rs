// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! HTTP error type (`{code, message}` bodies) and conversions from pipeline errors.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use dbsloc_api::ApiError;
use dbsloc_pipeline::bids::{BidsError, E_BIDS_PARSE, E_DUPL_RAW, E_NO_ANCHOR, E_NO_SUBJ};

/// Server-specific error codes not defined by the pipeline.
pub const E_NOT_FOUND: &str = "E_NOT_FOUND";
pub const E_BAD_KEY: &str = "E_BAD_KEY";
pub const E_BAD_RANGE: &str = "E_BAD_RANGE";
pub const E_BAD_STAGE: &str = "E_BAD_STAGE";
pub const E_RUN_ACTIVE: &str = "E_RUN_ACTIVE";
pub const E_BAD_RUN: &str = "E_BAD_RUN";
pub const E_UPLOAD: &str = "E_UPLOAD";
pub const E_DCM2NIIX: &str = "E_DCM2NIIX";
pub const E_NIFTI: &str = "E_NIFTI";
pub const E_NO_MANIFEST: &str = "E_NO_MANIFEST";
pub const E_BAD_SUBJECT: &str = "E_BAD_SUBJECT";
pub const E_SERVER: &str = "E_SERVER";
pub const E_BAD_REQUEST: &str = "E_BAD_REQUEST";
pub const E_IMPORT_CONFLICT: &str = "E_IMPORT_CONFLICT";
pub const E_HOST: &str = "E_HOST";
pub const E_ORIGIN: &str = "E_ORIGIN";

/// An error that renders as `{code, message}` with an HTTP status.
#[derive(Debug, Clone)]
pub struct ServerErr {
    pub status: StatusCode,
    pub error: ApiError,
}

impl ServerErr {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        ServerErr {
            status,
            error: ApiError::new(code, message),
        }
    }

    pub fn bad_request(code: impl Into<String>, message: impl Into<String>) -> Self {
        ServerErr::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        ServerErr::new(StatusCode::NOT_FOUND, E_NOT_FOUND, message)
    }

    pub fn not_found_code(code: impl Into<String>, message: impl Into<String>) -> Self {
        ServerErr::new(StatusCode::NOT_FOUND, code, message)
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        ServerErr::new(StatusCode::CONFLICT, code, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        ServerErr::new(StatusCode::INTERNAL_SERVER_ERROR, E_SERVER, message)
    }

    pub fn range(message: impl Into<String>) -> Self {
        ServerErr::new(StatusCode::RANGE_NOT_SATISFIABLE, E_BAD_RANGE, message)
    }

    pub fn code(&self) -> &str {
        &self.error.code
    }
}

impl IntoResponse for ServerErr {
    fn into_response(self) -> Response {
        (self.status, Json(self.error)).into_response()
    }
}

impl std::fmt::Display for ServerErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for ServerErr {}

impl From<BidsError> for ServerErr {
    fn from(e: BidsError) -> Self {
        let status = match e.code {
            E_NO_SUBJ => StatusCode::NOT_FOUND,
            E_NO_ANCHOR => StatusCode::UNPROCESSABLE_ENTITY,
            E_DUPL_RAW => StatusCode::CONFLICT,
            E_BIDS_PARSE => StatusCode::BAD_REQUEST,
            _ => StatusCode::BAD_REQUEST,
        };
        ServerErr {
            status,
            error: ApiError::new(e.code, e.message),
        }
    }
}

impl From<std::io::Error> for ServerErr {
    fn from(e: std::io::Error) -> Self {
        ServerErr::internal(format!("io: {e}"))
    }
}

impl From<tokio::task::JoinError> for ServerErr {
    fn from(e: tokio::task::JoinError) -> Self {
        ServerErr::internal(format!("handler task failed: {e}"))
    }
}

impl From<rusqlite::Error> for ServerErr {
    fn from(e: rusqlite::Error) -> Self {
        ServerErr::internal(format!("db: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bids_errors_keep_their_code_and_status() {
        let e = ServerErr::from(BidsError {
            code: E_DUPL_RAW,
            message: "duplicate".to_owned(),
        });
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert_eq!(e.code(), E_DUPL_RAW);
        let e = ServerErr::from(BidsError {
            code: E_NO_SUBJ,
            message: "nope".to_owned(),
        });
        assert_eq!(e.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn constructors_set_the_expected_codes() {
        assert_eq!(ServerErr::not_found("x").code(), E_NOT_FOUND);
        assert_eq!(
            ServerErr::range("x").status,
            StatusCode::RANGE_NOT_SATISFIABLE
        );
        assert_eq!(
            ServerErr::bad_request("x", "y").status,
            StatusCode::BAD_REQUEST
        );
    }
}
