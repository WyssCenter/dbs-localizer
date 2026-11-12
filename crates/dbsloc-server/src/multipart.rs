// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! A small `multipart/form-data` reader for the single-file uploads of the stimulation and clinical
//! endpoints. A body without a multipart content type is taken as the raw file.

use crate::error::{ServerErr, E_BAD_REQUEST};

/// One part of a multipart body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub data: Vec<u8>,
}

/// An uploaded file with the text fields that came with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub filename: Option<String>,
    pub bytes: Vec<u8>,
    pub fields: Vec<(String, String)>,
}

impl Upload {
    /// The text field `name`, when present.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The boundary of a `multipart/form-data` content type.
pub fn boundary_of(content_type: &str) -> Option<String> {
    let lower = content_type.to_ascii_lowercase();
    if !lower.trim_start().starts_with("multipart/form-data") {
        return None;
    }
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k.trim().eq_ignore_ascii_case("boundary"))
            .then(|| v.trim().trim_matches('"').to_owned())
            .filter(|b| !b.is_empty())
    })
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn header_param(line: &str, key: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let at = lower.find(&format!("{key}=\""))?;
    let rest = &line[at + key.len() + 2..];
    rest.find('"').map(|end| rest[..end].to_owned())
}

fn bad(message: &str) -> ServerErr {
    ServerErr::bad_request(
        E_BAD_REQUEST,
        format!("malformed multipart body: {message}"),
    )
}

/// Split `body` into its parts.
pub fn parse(body: &[u8], boundary: &str) -> Result<Vec<Part>, ServerErr> {
    let delim = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let mut pos = find(body, &delim, 0).ok_or_else(|| bad("no boundary"))? + delim.len();
    loop {
        if body[pos..].starts_with(b"--") {
            return Ok(parts);
        }
        if !body[pos..].starts_with(b"\r\n") {
            return Err(bad("boundary line"));
        }
        pos += 2;
        let head_end = find(body, b"\r\n\r\n", pos).ok_or_else(|| bad("part headers"))?;
        let head = String::from_utf8_lossy(&body[pos..head_end]).into_owned();
        let data_start = head_end + 4;
        let mut closing = b"\r\n".to_vec();
        closing.extend_from_slice(&delim);
        let data_end = find(body, &closing, data_start).ok_or_else(|| bad("unterminated part"))?;
        let disposition = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-disposition"))
            .unwrap_or_default();
        parts.push(Part {
            name: header_param(disposition, "name").unwrap_or_default(),
            filename: header_param(disposition, "filename"),
            data: body[data_start..data_end].to_vec(),
        });
        pos = data_end + closing.len();
        if pos >= body.len() {
            return Err(bad("missing closing boundary"));
        }
    }
}

/// The uploaded file of a request: the `file` part (or the first part with a file name) of a
/// multipart body with the other parts as text fields, or the whole body of any other request.
pub fn read_upload(content_type: Option<&str>, body: &[u8]) -> Result<Upload, ServerErr> {
    let Some(boundary) = content_type.and_then(boundary_of) else {
        return Ok(Upload {
            filename: None,
            bytes: body.to_vec(),
            fields: Vec::new(),
        });
    };
    let parts = parse(body, &boundary)?;
    let at = parts
        .iter()
        .position(|p| p.name == "file")
        .or_else(|| parts.iter().position(|p| p.filename.is_some()))
        .ok_or_else(|| {
            ServerErr::bad_request(E_BAD_REQUEST, "the multipart body has no `file` part")
        })?;
    let mut parts = parts;
    let file = parts.remove(at);
    Ok(Upload {
        filename: file.filename,
        bytes: file.data,
        fields: parts
            .into_iter()
            .filter(|p| p.filename.is_none())
            .map(|p| (p.name, String::from_utf8_lossy(&p.data).trim().to_owned()))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(boundary: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, file, data) in parts {
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            match file {
                Some(f) => out.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n"
                    )
                    .as_bytes(),
                ),
                None => out.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
                ),
            }
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        out
    }

    #[test]
    fn boundary_is_read_from_the_content_type() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=\"abc\""),
            Some("abc".to_owned())
        );
        assert_eq!(
            boundary_of("Multipart/Form-Data; charset=x; boundary=xyz"),
            Some("xyz".to_owned())
        );
        assert_eq!(boundary_of("application/json"), None);
        assert_eq!(boundary_of("multipart/form-data"), None);
    }

    #[test]
    fn a_file_part_and_fields_are_split_even_with_binary_data() {
        let data = [0u8, 13, 10, 45, 45, 255, 1];
        let b = body(
            "XB",
            &[("label", None, b"my label"), ("file", Some("a.mat"), &data)],
        );
        let up = read_upload(Some("multipart/form-data; boundary=XB"), &b).unwrap();
        assert_eq!(up.filename.as_deref(), Some("a.mat"));
        assert_eq!(up.bytes, data);
        assert_eq!(up.field("label"), Some("my label"));
    }

    #[test]
    fn a_raw_body_is_the_file() {
        let up = read_upload(Some("application/octet-stream"), b"abc").unwrap();
        assert_eq!(up.bytes, b"abc");
        assert!(up.filename.is_none());
        let up = read_upload(None, b"x").unwrap();
        assert_eq!(up.bytes, b"x");
    }

    #[test]
    fn broken_bodies_are_refused() {
        assert!(read_upload(Some("multipart/form-data; boundary=B"), b"nothing").is_err());
        let mut b = body("B", &[("file", Some("a"), b"x")]);
        b.truncate(b.len() - 6);
        assert!(read_upload(Some("multipart/form-data; boundary=B"), &b).is_err());
        let b = body("B", &[("note", None, b"x")]);
        assert!(read_upload(Some("multipart/form-data; boundary=B"), &b).is_err());
    }
}
