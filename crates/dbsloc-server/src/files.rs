// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! File-key allowlist, HTTP `Range`, content types and file serving.
//!
//! A *file key* is a percent-decoded path relative to a filesystem root (a dataset root or the
//! configured templates directory). Resolution is mandatory-security:
//!
//! * the key must be relative: no absolute paths, no `..` components, no empty key;
//! * the resolved path is canonicalised (following symlinks) and must stay inside the canonical
//!   root, so a symlink pointing out of the root is refused;
//! * only regular files are served, always as `Content-Disposition: attachment`.

use std::io::{Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;
use dbsloc_pipeline::bids::simplify_verbatim;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

use crate::error::{ServerErr, E_BAD_KEY};

/// Inclusive byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

/// A dataset name: one normal path component of `[A-Za-z0-9._-]`, never `.` or `..`.
pub fn valid_dataset_name(ds: &str) -> bool {
    if ds.is_empty() || ds == "." || ds == ".." {
        return false;
    }
    let mut components = Path::new(ds).components();
    let Some(Component::Normal(name)) = components.next() else {
        return false;
    };
    if components.next().is_some() {
        return false;
    }
    name.to_str().is_some_and(|s| {
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    })
}

/// Reject non-relative, traversal or empty keys, and keys with `:` or `\`.
pub fn sanitize_key(key: &str) -> Result<(), ServerErr> {
    if key.is_empty() {
        return Err(ServerErr::bad_request(E_BAD_KEY, "empty file key"));
    }
    if key.contains('\0') {
        return Err(ServerErr::bad_request(E_BAD_KEY, "file key contains NUL"));
    }
    if key.contains(':') || key.contains('\\') {
        return Err(ServerErr::bad_request(
            E_BAD_KEY,
            "file key contains `:` or a backslash",
        ));
    }
    let p = Path::new(key);
    if p.is_absolute() {
        return Err(ServerErr::bad_request(
            E_BAD_KEY,
            format!("file key must be relative: {key}"),
        ));
    }
    for comp in p.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(ServerErr::bad_request(
                    E_BAD_KEY,
                    format!("file key may not contain `..`: {key}"),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ServerErr::bad_request(
                    E_BAD_KEY,
                    format!("file key must be relative: {key}"),
                ));
            }
        }
    }
    Ok(())
}

/// File extensions a client may fetch from a `stimulations/<space>/<label>/` folder.
const STIMULATION_EXTENSIONS: [&str; 4] = [".nii", ".nii.gz", ".json", ".mat"];

/// Why a subject file key is withheld although it belongs to the subject: the `headmodel/` cache
/// of the `vat` stage (`specs/07` §3.6) is server-internal, partial work files are never served,
/// and a `stimulations/` folder serves only its NIfTI, JSON and MAT results.
pub fn withheld_reason(key: &str) -> Option<&'static str> {
    let comps: Vec<&str> = key.split('/').filter(|c| !c.is_empty()).collect();
    if comps.iter().any(|c| c.starts_with(".partial-")) {
        return Some("work files are not served");
    }
    if comps.iter().any(|c| c.eq_ignore_ascii_case("headmodel")) {
        return Some("the head-model cache is not served");
    }
    let at = comps
        .iter()
        .position(|c| c.eq_ignore_ascii_case("stimulations"))?;
    let name = comps.last()?.to_ascii_lowercase();
    if comps.len() > at + 1 && !STIMULATION_EXTENSIONS.iter().any(|ext| name.ends_with(ext)) {
        return Some("only NIfTI, JSON and MAT results of a stimulation are served");
    }
    None
}

/// Resolve `key` under `root`, refusing escapes (including symlink escapes). The file must exist.
pub fn resolve_in_root(root: &Path, key: &str) -> Result<PathBuf, ServerErr> {
    sanitize_key(key)?;
    let root_canon = std::fs::canonicalize(root)
        .map(simplify_verbatim)
        .map_err(|e| ServerErr::not_found(format!("root unavailable: {}: {e}", root.display())))?;
    let candidate = root.join(key);
    let canon = std::fs::canonicalize(&candidate)
        .map(simplify_verbatim)
        .map_err(|_| ServerErr::not_found(format!("no such file: {key}")))?;
    if !canon.starts_with(&root_canon) {
        return Err(ServerErr::bad_request(
            E_BAD_KEY,
            format!("file key escapes the root: {key}"),
        ));
    }
    if !canon.is_file() {
        return Err(ServerErr::not_found(format!("not a file: {key}")));
    }
    Ok(canon)
}

/// The key of `path` relative to `root`, with `/` separators; `None` when outside the root.
pub fn file_key(root: &Path, path: &Path) -> Option<String> {
    let root = std::fs::canonicalize(root).ok().map(simplify_verbatim)?;
    let canon = std::fs::canonicalize(path).ok().map(simplify_verbatim)?;
    let rel = canon.strip_prefix(&root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Parse a `Range: bytes=…` header against a body of `len` bytes. Multiple ranges are refused.
/// The unit is matched case-insensitively (`bytes=` / `Bytes=`).
pub fn parse_range(header: &str, len: u64) -> Result<ByteRange, ServerErr> {
    let trimmed = header.trim();
    let spec = match trimmed.get(..6) {
        Some(unit) if unit.eq_ignore_ascii_case("bytes=") => &trimmed[6..],
        _ => return Err(ServerErr::range("Range must start with `bytes=`")),
    };
    if spec.contains(',') {
        return Err(ServerErr::range("multiple ranges are not supported"));
    }
    if len == 0 {
        return Err(ServerErr::range("empty file"));
    }
    let (start_s, end_s) = spec
        .split_once('-')
        .ok_or_else(|| ServerErr::range("malformed Range"))?;
    if start_s.is_empty() {
        let n: u64 = end_s
            .trim()
            .parse()
            .map_err(|_| ServerErr::range("malformed suffix Range"))?;
        if n == 0 {
            return Err(ServerErr::range("empty suffix Range"));
        }
        let start = len.saturating_sub(n);
        return Ok(ByteRange {
            start,
            end: len - 1,
        });
    }
    let start: u64 = start_s
        .trim()
        .parse()
        .map_err(|_| ServerErr::range("malformed Range start"))?;
    let end = if end_s.trim().is_empty() {
        len - 1
    } else {
        end_s
            .trim()
            .parse::<u64>()
            .map_err(|_| ServerErr::range("malformed Range end"))?
            .min(len - 1)
    };
    if start > end || start >= len {
        return Err(ServerErr::range("Range out of bounds"));
    }
    Ok(ByteRange { start, end })
}

/// `Content-Type` for a file name.
pub fn content_type(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.ends_with(".nii.gz") {
        "application/gzip"
    } else if name.ends_with(".nii") {
        "application/octet-stream"
    } else if name.ends_with(".json") {
        "application/json"
    } else if name.ends_with(".png") {
        "image/png"
    } else if name.ends_with(".h5") || name.ends_with(".hdf5") {
        "application/octet-stream"
    } else if name.ends_with(".toml") || name.ends_with(".txt") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

/// Serve `path` with optional `Range`, streaming the body. Both dataset and template routes call this.
pub fn serve(path: &Path, range_header: Option<&str>) -> Result<Response, ServerErr> {
    let mut file = std::fs::File::open(path)
        .map_err(|_| ServerErr::not_found(format!("no such file: {}", path.display())))?;
    let len = file
        .metadata()
        .map_err(|e| ServerErr::internal(format!("stat {}: {e}", path.display())))?
        .len();
    let range = match range_header {
        Some(h) if !h.trim().is_empty() => Some(parse_range(h, len)?),
        _ => None,
    };
    let (start, body_len) = match range {
        Some(r) => (r.start, r.len()),
        None => (0, len),
    };
    if start > 0 {
        file.seek(SeekFrom::Start(start))
            .map_err(|e| ServerErr::internal(format!("seek {}: {e}", path.display())))?;
    }
    let reader = tokio::fs::File::from_std(file).take(body_len);
    let ctype = content_type(path);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_owned());
    let disposition = format!("attachment; filename=\"{}\"", sanitize_filename(&file_name));

    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, ctype)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CONTENT_LENGTH, body_len);
    let status = match range {
        Some(r) => {
            builder = builder.header(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", r.start, r.end, len),
            );
            StatusCode::PARTIAL_CONTENT
        }
        None => StatusCode::OK,
    };
    builder
        .status(status)
        .body(Body::from_stream(ReaderStream::with_capacity(
            reader,
            256 * 1024,
        )))
        .map_err(|e| ServerErr::internal(format!("response: {e}")))
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .filter(|c| !matches!(c, '"' | '\\'))
        .collect()
}

#[cfg(test)]
fn header_value(s: &str) -> Option<axum::http::HeaderValue> {
    axum::http::HeaderValue::from_str(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/a.nii.gz"), b"hello").unwrap();
        d
    }

    #[test]
    fn rejects_parent_dir_absolute_and_empty_keys() {
        for bad in [
            "",
            "../a",
            "a/../../b",
            "/etc/passwd",
            "sub/../a",
            "a:b",
            "C:evil",
            "file.nii:stream",
            "a\\b",
            "..\\a",
        ] {
            assert!(sanitize_key(bad).is_err(), "{bad} was accepted");
        }
        assert!(sanitize_key("sub/a.nii.gz").is_ok());
        assert!(sanitize_key("./sub/a.nii.gz").is_ok());
    }

    #[test]
    fn dataset_names_reject_dot_dotdot_and_separators() {
        for bad in [
            "", ".", "..", "a/b", "a\\b", "/abs", "../x", "a/../b", "sub/..", "a:b", "a b",
        ] {
            assert!(!valid_dataset_name(bad), "{bad} was accepted");
        }
        for good in ["demo", "sub-01", "cohort_2024", "data.set"] {
            assert!(valid_dataset_name(good), "{good} was rejected");
        }
    }

    #[test]
    fn disposition_filenames_are_printable_ascii() {
        assert_eq!(sanitize_filename("a\"b\\c.nii"), "abc.nii");
        assert_eq!(sanitize_filename("caf\u{e9}\n.nii"), "caf__.nii");
        let name = sanitize_filename("\u{4e2d}\u{6587}.nii");
        assert!(header_value(&format!("attachment; filename=\"{name}\"")).is_some());
    }

    #[tokio::test]
    async fn serve_streams_full_and_ranged_bodies() {
        use http_body_util::BodyExt;
        let d = root();
        let p = d.path().join("sub/a.nii.gz");
        let resp = serve(&p, None).unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(resp.headers()[header::CONTENT_LENGTH], "5");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"hello");
        let resp = serve(&p, Some("bytes=1-3")).unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers()[header::CONTENT_RANGE], "bytes 1-3/5");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"ell");
        let resp = serve(&p, Some("bytes=-2")).unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"lo");
    }

    #[test]
    fn resolves_a_file_inside_the_root() {
        let d = root();
        let p = resolve_in_root(d.path(), "sub/a.nii.gz").unwrap();
        assert!(p.ends_with("a.nii.gz"));
    }

    #[test]
    fn refuses_escape_and_missing_files() {
        let d = root();
        assert_eq!(
            resolve_in_root(d.path(), "../outside").unwrap_err().code(),
            E_BAD_KEY
        );
        assert_eq!(
            resolve_in_root(d.path(), "sub/missing.nii")
                .unwrap_err()
                .code(),
            crate::error::E_NOT_FOUND
        );
    }

    #[test]
    fn symlink_escape_is_refused() {
        let d = root();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
        #[cfg(windows)]
        let link = std::os::windows::fs::symlink_file(
            outside.path().join("secret.txt"),
            d.path().join("sub/link.txt"),
        );
        #[cfg(unix)]
        let link = std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            d.path().join("sub/link.txt"),
        );
        if link.is_ok() {
            let e = resolve_in_root(d.path(), "sub/link.txt").unwrap_err();
            assert_eq!(e.code(), E_BAD_KEY);
        }
    }

    #[test]
    fn range_parsing_covers_the_forms() {
        assert_eq!(
            parse_range("bytes=0-4", 10).unwrap(),
            ByteRange { start: 0, end: 4 }
        );
        assert_eq!(
            parse_range("bytes=5-", 10).unwrap(),
            ByteRange { start: 5, end: 9 }
        );
        assert_eq!(
            parse_range("bytes=-3", 10).unwrap(),
            ByteRange { start: 7, end: 9 }
        );
        assert_eq!(
            parse_range("bytes=8-100", 10).unwrap(),
            ByteRange { start: 8, end: 9 }
        );
        for bad in [
            "items=0-1",
            "bytes=5-2",
            "bytes=10-",
            "bytes=1-2,3-4",
            "bytes=-0",
        ] {
            assert!(parse_range(bad, 10).is_err(), "{bad} was accepted");
        }
        assert_eq!(
            parse_range("BYTES=1-3", 10).unwrap(),
            ByteRange { start: 1, end: 3 }
        );
        assert_eq!(
            parse_range(" Bytes=1-3 ", 10).unwrap(),
            ByteRange { start: 1, end: 3 }
        );
    }

    #[test]
    fn content_types_by_extension() {
        assert_eq!(content_type(Path::new("a.nii.gz")), "application/gzip");
        assert_eq!(content_type(Path::new("a.json")), "application/json");
        assert_eq!(content_type(Path::new("a.png")), "image/png");
        assert_eq!(content_type(Path::new("a.h5")), "application/octet-stream");
    }

    #[test]
    fn file_key_is_relative_with_forward_slashes() {
        let d = root();
        let p = d.path().join("sub/a.nii.gz");
        assert_eq!(file_key(d.path(), &p).unwrap(), "sub/a.nii.gz");
        assert!(file_key(d.path(), &d.path().join("../nope")).is_none());
    }
}
