// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The static web client (`dbsloc-web` `dist/`) served at `/` when `web_dir` is configured.

use std::path::Path;

use axum::extract::State;
use axum::http::{header, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::error::{ServerErr, E_BAD_KEY};
use crate::files::{resolve_in_root, sanitize_key};
use crate::state::AppState;

/// `Content-Type` of a served web asset.
pub fn web_content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "wasm" => "application/wasm",
        "css" => "text/css",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "json" => "application/json",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn percent_decode(raw: &str) -> Result<String, ServerErr> {
    let bad = || ServerErr::bad_request(E_BAD_KEY, "malformed percent-encoding in the path");
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or_else(bad)?;
            let hex = std::str::from_utf8(hex).map_err(|_| bad())?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| bad())?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| bad())
}

fn serve_static(dir: &Path, raw_path: &str) -> Result<Response, ServerErr> {
    let decoded = percent_decode(raw_path)?;
    let trimmed = decoded.trim_start_matches('/');
    let key = if trimmed.is_empty() || trimmed.ends_with('/') {
        format!("{trimmed}index.html")
    } else {
        trimmed.to_owned()
    };
    sanitize_key(&key)?;
    let has_extension = !trimmed.ends_with('/') && Path::new(trimmed).extension().is_some();
    let path = match resolve_in_root(dir, &key) {
        Ok(p) => p,
        Err(e) if e.status == StatusCode::NOT_FOUND && !has_extension => {
            resolve_in_root(dir, "index.html")?
        }
        Err(e) => return Err(e),
    };
    let bytes = std::fs::read(&path)
        .map_err(|e| ServerErr::internal(format!("read {}: {e}", path.display())))?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, web_content_type(&path))
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(axum::body::Body::from(bytes))
        .map_err(|e| ServerErr::internal(format!("response: {e}")))
}

/// Router fallback: the web client for a non-`/api` `GET`, the JSON `404` for everything else.
pub async fn fallback(State(state): State<AppState>, method: Method, uri: Uri) -> Response {
    let path = uri.path().to_owned();
    let is_api = path == "/api" || path.starts_with("/api/");
    let readable = method == Method::GET || method == Method::HEAD;
    let dir = state.config().web_dir.clone();
    let (Some(dir), false, true) = (dir, is_api, readable) else {
        return ServerErr::not_found("no such route").into_response();
    };
    match tokio::task::spawn_blocking(move || serve_static(&dir, &path)).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => e.into_response(),
        Err(e) => ServerErr::from(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_cover_the_web_assets() {
        for (name, want) in [
            ("index.html", "text/html; charset=utf-8"),
            ("app.js", "text/javascript"),
            ("app_bg.wasm", "application/wasm"),
            ("style.css", "text/css"),
            ("logo.png", "image/png"),
            ("logo.svg", "image/svg+xml"),
            ("favicon.ico", "image/x-icon"),
            ("manifest.json", "application/json"),
            ("blob.bin", "application/octet-stream"),
        ] {
            assert_eq!(web_content_type(Path::new(name)), want, "{name}");
        }
    }

    #[test]
    fn percent_decoding_accepts_valid_and_refuses_malformed_input() {
        assert_eq!(percent_decode("/a%20b.js").unwrap(), "/a b.js");
        assert_eq!(percent_decode("/%2e%2e/x").unwrap(), "/../x");
        assert!(percent_decode("/a%2").is_err());
        assert!(percent_decode("/a%zz").is_err());
        assert!(percent_decode("/%ff").is_err());
    }

    #[test]
    fn serving_refuses_traversal_and_falls_back_to_the_index() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("index.html"), b"<html></html>").unwrap();
        std::fs::write(d.path().join("a.js"), b"1").unwrap();
        assert!(serve_static(d.path(), "/").is_ok());
        assert!(serve_static(d.path(), "/a.js").is_ok());
        assert!(serve_static(d.path(), "/some/route").is_ok());
        assert!(serve_static(d.path(), "/missing.js").is_err());
        assert!(serve_static(d.path(), "/%2e%2e/secret").is_err());
        assert!(serve_static(d.path(), "/..%2fsecret").is_err());
    }
}
