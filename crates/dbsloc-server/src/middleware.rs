// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The pass-through middleware every route goes through (the WP6 auth seam), the Host / Origin
//! request guard against DNS rebinding and cross-site requests, and the optional development CORS
//! layer.
//!
//! v1 authentication is `none` (D2): the middleware records the client address for the audit log
//! and allows the request. WP6 adds authentication and dataset ACL here without touching handlers.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::{ServerErr, E_HOST, E_ORIGIN};
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostName {
    name: String,
    is_ip: bool,
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok()
}

fn parse_host(authority: &str) -> Option<HostName> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (inner, after) = rest.split_once(']')?;
        let ip: Ipv6Addr = inner.parse().ok()?;
        if !after.is_empty() && !valid_port(after.strip_prefix(':')?) {
            return None;
        }
        return Some(HostName {
            name: format!("[{ip}]"),
            is_ip: true,
        });
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if port.is_some_and(|p| !valid_port(p)) {
        return None;
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    {
        return None;
    }
    Some(HostName {
        name: host.to_ascii_lowercase(),
        is_ip: host.parse::<Ipv4Addr>().is_ok(),
    })
}

fn host_allowed(host: &HostName, allowed: &[String]) -> bool {
    host.is_ip || host.name == "localhost" || allowed.contains(&host.name)
}

fn request_authority(req: &Request) -> Result<Option<&str>, ()> {
    let mut hosts = req.headers().get_all(header::HOST).iter();
    match (hosts.next(), hosts.next()) {
        (Some(v), None) => v.to_str().map(Some).map_err(|_| ()),
        (Some(_), Some(_)) => Err(()),
        (None, _) => Ok(req.uri().authority().map(|a| a.as_str())),
    }
}

fn same_origin(origin: &str, authority: Option<&str>) -> bool {
    let Some(authority) = authority else {
        return false;
    };
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        && rest.eq_ignore_ascii_case(authority)
}

fn origin_permitted(headers: &HeaderMap, authority: Option<&str>, cors: &[String]) -> bool {
    let origin = match headers.get(header::ORIGIN).map(HeaderValue::to_str) {
        None => None,
        Some(Ok(o)) => Some(o),
        Some(Err(_)) => return false,
    };
    let listed = origin.is_some_and(|o| o != "null" && cors.iter().any(|c| c == o));
    if let Some(o) = origin {
        if o == "null" || (!listed && !same_origin(o, authority)) {
            return false;
        }
    }
    let cross_site = headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("cross-site"));
    !cross_site || listed
}

const LOGGED_VALUE_CHARS: usize = 100;

fn loggable(value: &[u8]) -> String {
    let quoted = format!("{:?}", String::from_utf8_lossy(value));
    if quoted.chars().count() <= LOGGED_VALUE_CHARS {
        return quoted;
    }
    let mut cut: String = quoted.chars().take(LOGGED_VALUE_CHARS).collect();
    cut.push_str("...");
    cut
}

fn header_values(headers: &HeaderMap, name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, v) in headers.get_all(name).iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(b", ");
        }
        out.extend_from_slice(v.as_bytes());
    }
    out
}

fn refused_value(req: &Request, code: &str) -> Vec<u8> {
    if code == E_HOST {
        if req.headers().contains_key(header::HOST) {
            return header_values(req.headers(), header::HOST.as_str());
        }
        return req
            .uri()
            .authority()
            .map(|a| a.as_str().as_bytes().to_vec())
            .unwrap_or_default();
    }
    let mut out = b"origin=".to_vec();
    out.extend(header_values(req.headers(), header::ORIGIN.as_str()));
    out.extend_from_slice(b" sec-fetch-site=");
    out.extend(header_values(req.headers(), "sec-fetch-site"));
    out
}

fn peer_of(req: &Request) -> String {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn refuse(req: &Request, code: &'static str) -> Response {
    eprintln!(
        "dbsloc_server: refused {} from {}: {code}: {}",
        loggable(req.method().as_str().as_bytes()),
        peer_of(req),
        loggable(&refused_value(req, code)),
    );
    let message = if code == E_HOST {
        "the request names a host this server does not answer to; a server reached by a DNS name \
         must list that name in allowed_hosts"
    } else {
        "a request that changes data must come from this server's own pages or an origin listed \
         in cors_origins"
    };
    ServerErr::new(StatusCode::FORBIDDEN, code, message).into_response()
}

fn guard_verdict(req: &Request, config: &crate::config::Config) -> Option<&'static str> {
    let Ok(authority) = request_authority(req) else {
        return Some(E_HOST);
    };
    if let Some(a) = authority {
        if !parse_host(a).is_some_and(|h| host_allowed(&h, &config.allowed_hosts)) {
            return Some(E_HOST);
        }
    }
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe && !origin_permitted(req.headers(), authority, &config.cors_origins) {
        return Some(E_ORIGIN);
    }
    None
}

/// Refuses foreign hosts (`E_HOST`, DNS rebinding) and cross-site writes (`E_ORIGIN`) before any
/// other layer runs.
pub async fn request_guard_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(code) = guard_verdict(&req, state.config()) {
        return refuse(&req, code);
    }
    next.run(req).await
}

/// The client address recorded for the audit log; inserted by [`audit_middleware`].
#[derive(Debug, Clone)]
pub struct ClientAddr(pub String);

impl ClientAddr {
    /// `"unknown"` when no address is available (e.g. `oneshot` tests).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Records the client address in the request extensions and, for now, allows everything.
pub async fn audit_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let _ = state.config().auth_mode.as_str();
    let addr = client_addr(&req, state.config().trust_proxy);
    req.extensions_mut().insert(ClientAddr(addr));
    next.run(req).await
}

fn forwarded_for(req: &Request) -> Option<String> {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Behind a trusted reverse proxy the socket peer is the proxy, so the forwarded header is the
/// client; otherwise the header is ignored (a client could spoof the audit log).
fn client_addr(req: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        if let Some(fwd) = forwarded_for(req) {
            return fwd;
        }
    }
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return addr.to_string();
    }
    "unknown".to_owned()
}

const SANITIZE_LIMIT: usize = 64 * 1024 * 1024;

fn dataset_of(path: &str) -> Option<String> {
    path.strip_prefix("/api/v1/datasets/")
        .and_then(|rest| rest.split('/').next())
        .filter(|ds| !ds.is_empty())
        .map(str::to_owned)
}

pub async fn sanitize_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    let is_api = path == "/api" || path.starts_with("/api/");
    let resp = next.run(req).await;
    let status = resp.status();
    if resp.headers().contains_key(header::CONTENT_DISPOSITION) {
        return resp;
    }
    let ctype = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_json = ctype.starts_with("application/json");
    let is_error = status.is_client_error() || status.is_server_error();
    let is_text = ctype.starts_with("text/plain") || ctype.is_empty();
    if !((is_json && (is_api || is_error)) || (is_error && is_text)) {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, SANITIZE_LIMIT).await else {
        return crate::error::ServerErr::internal("the response could not be prepared")
            .into_response();
    };
    if status.is_server_error() {
        eprintln!(
            "dbsloc_server: {method} {path}: {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    let sanitizer = state.sanitizer(dataset_of(&path).as_deref());
    let out = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(mut v) if is_json => {
            if !sanitizer.value(&mut v) {
                return Response::from_parts(parts, axum::body::Body::from(bytes));
            }
            serde_json::to_vec(&v).unwrap_or_default()
        }
        _ => {
            let text = String::from_utf8_lossy(&bytes);
            sanitizer.text(&text).into_owned().into_bytes()
        }
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, axum::body::Body::from(out))
}

const CORS_METHODS: &str = "GET, POST, PUT, OPTIONS";
const CORS_ALLOW_HEADERS: &str = "content-type, range, last-event-id";
const CORS_EXPOSE_HEADERS: &str = "content-range, content-length, accept-ranges";

fn allowed_origin(origins: &[String], req: &Request) -> Option<HeaderValue> {
    let origin = req.headers().get(header::ORIGIN)?;
    let text = origin.to_str().ok()?;
    origins.iter().any(|o| o == text).then(|| origin.clone())
}

fn add_vary_origin(resp: &mut Response) {
    resp.headers_mut()
        .append(header::VARY, HeaderValue::from_static("Origin"));
}

/// Development CORS: with `cors_origins` configured, an exactly matching `Origin` is allowed and a
/// preflight `OPTIONS` is answered with `204`. Without configured origins it does nothing.
pub async fn cors_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let origins = &state.config().cors_origins;
    if origins.is_empty() {
        return next.run(req).await;
    }
    let allowed = allowed_origin(origins, &req);
    if let Some(origin) = allowed.as_ref() {
        if req.method() == Method::OPTIONS {
            let mut resp = StatusCode::NO_CONTENT.into_response();
            let h = resp.headers_mut();
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
            h.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static(CORS_METHODS),
            );
            h.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static(CORS_ALLOW_HEADERS),
            );
            h.insert(
                header::ACCESS_CONTROL_MAX_AGE,
                HeaderValue::from_static("600"),
            );
            add_vary_origin(&mut resp);
            return resp;
        }
    }
    let mut resp = next.run(req).await;
    if let Some(origin) = allowed {
        let h = resp.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        h.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static(CORS_EXPOSE_HEADERS),
        );
    }
    add_vary_origin(&mut resp);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;

    #[test]
    fn falls_back_to_unknown_without_connect_info() {
        let req = HttpRequest::builder().body(Body::empty()).unwrap();
        assert_eq!(client_addr(&req, false), "unknown");
    }

    #[test]
    fn x_forwarded_for_is_ignored_unless_the_proxy_is_trusted() {
        let req = HttpRequest::builder()
            .header("x-forwarded-for", "10.0.0.9, 10.0.0.1")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_addr(&req, false), "unknown");
        assert_eq!(client_addr(&req, true), "10.0.0.9");
    }

    #[test]
    fn a_trusted_forwarded_header_wins_over_the_socket_peer() {
        let mut req = HttpRequest::builder()
            .header("x-forwarded-for", "10.0.0.9")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4321))));
        assert_eq!(client_addr(&req, true), "10.0.0.9");
        assert_eq!(client_addr(&req, false), "127.0.0.1:4321");
    }

    #[test]
    fn the_peer_is_used_when_a_trusted_proxy_sent_no_header() {
        let mut req = HttpRequest::builder().body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4321))));
        assert_eq!(client_addr(&req, true), "127.0.0.1:4321");
    }

    #[test]
    fn the_dataset_is_read_from_dataset_routes_only() {
        assert_eq!(
            dataset_of("/api/v1/datasets/demo/subjects/01").as_deref(),
            Some("demo")
        );
        assert_eq!(dataset_of("/api/v1/datasets/demo").as_deref(), Some("demo"));
        assert_eq!(dataset_of("/api/v1/runs/x"), None);
        assert_eq!(dataset_of("/api/v1/datasets/"), None);
    }

    #[test]
    fn origins_match_exactly() {
        let origins = vec!["http://localhost:8081".to_owned()];
        let ok = HttpRequest::builder()
            .header("origin", "http://localhost:8081")
            .body(Body::empty())
            .unwrap();
        let other = HttpRequest::builder()
            .header("origin", "http://localhost:8081.evil.test")
            .body(Body::empty())
            .unwrap();
        assert!(allowed_origin(&origins, &ok).is_some());
        assert!(allowed_origin(&origins, &other).is_none());
    }

    fn name(n: &str) -> Option<HostName> {
        Some(HostName {
            name: n.to_owned(),
            is_ip: false,
        })
    }

    fn ip(n: &str) -> Option<HostName> {
        Some(HostName {
            name: n.to_owned(),
            is_ip: true,
        })
    }

    #[test]
    fn hosts_parse_with_and_without_ports() {
        assert_eq!(parse_host("localhost"), name("localhost"));
        assert_eq!(parse_host("LocalHost:8080"), name("localhost"));
        assert_eq!(parse_host("127.0.0.1:8080"), ip("127.0.0.1"));
        assert_eq!(parse_host("10.1.2.3"), ip("10.1.2.3"));
        assert_eq!(parse_host("dbs.lab.example:443"), name("dbs.lab.example"));
        assert_eq!(parse_host("host:"), None);
        assert_eq!(parse_host("host:80a"), None);
        assert_eq!(parse_host("host:65536"), None);
        assert_eq!(parse_host("host:+80"), None);
        assert_eq!(parse_host("a:1:2"), None);
    }

    #[test]
    fn ipv6_hosts_need_brackets() {
        assert_eq!(parse_host("[::1]:8080"), ip("[::1]"));
        assert_eq!(parse_host("[::1]"), ip("[::1]"));
        assert_eq!(parse_host("[FE80::1]"), ip("[fe80::1]"));
        assert_eq!(parse_host("::1"), None);
        assert_eq!(parse_host("[::1]8080"), None);
        assert_eq!(parse_host("[::1"), None);
        assert_eq!(parse_host("[evil.example]"), None);
    }

    #[test]
    fn a_single_trailing_dot_is_ignored() {
        assert_eq!(parse_host("localhost.:8080"), name("localhost"));
        assert_eq!(parse_host("Dbs.Lab.Example."), name("dbs.lab.example"));
        assert_eq!(parse_host("127.0.0.1."), ip("127.0.0.1"));
        assert_eq!(parse_host("."), None);
    }

    #[test]
    fn empty_and_userinfo_hosts_are_malformed() {
        assert_eq!(parse_host(""), None);
        assert_eq!(parse_host(":8080"), None);
        assert_eq!(parse_host("user@localhost"), None);
        assert_eq!(parse_host("user:pw@127.0.0.1:8080"), None);
        assert_eq!(parse_host("evil.example/x"), None);
        assert_eq!(parse_host("evil example"), None);
    }

    #[test]
    fn ip_literals_localhost_and_listed_names_are_allowed() {
        let listed = vec![crate::config::normalize_allowed_host("DBS.lab.example.").unwrap()];
        let allowed = |h: &str| parse_host(h).is_some_and(|h| host_allowed(&h, &listed));
        assert!(allowed("127.0.0.1:8080"));
        assert!(allowed("[::1]:8080"));
        assert!(allowed("LOCALHOST"));
        assert!(allowed("dbs.lab.EXAMPLE:443"));
        assert!(!allowed("evil.example:8080"));
        assert!(!allowed("localhost.evil.example"));
        assert!(!allowed("0x7f.1"));
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn origins_must_match_the_host_or_be_listed() {
        let cors = vec!["http://localhost:8081".to_owned()];
        let host = Some("127.0.0.1:8080");
        let ok = |pairs: &[(&'static str, &str)], authority| {
            origin_permitted(&headers(pairs), authority, &cors)
        };
        assert!(ok(&[], host));
        assert!(ok(&[("origin", "http://127.0.0.1:8080")], host));
        assert!(ok(&[("origin", "https://127.0.0.1:8080")], host));
        assert!(ok(&[("origin", "http://localhost:8081")], host));
        assert!(ok(&[("origin", "http://localhost:8081")], None));
        assert!(!ok(&[("origin", "http://evil.example")], host));
        assert!(!ok(&[("origin", "http://127.0.0.1")], host));
        assert!(!ok(&[("origin", "http://127.0.0.1:8080/x")], host));
        assert!(!ok(&[("origin", "http://127.0.0.1:8080")], None));
        assert!(!ok(&[("origin", "null")], host));
        assert!(!ok(&[("origin", "127.0.0.1:8080")], host));
        assert!(!ok(&[("sec-fetch-site", "cross-site")], host));
        assert!(!ok(
            &[
                ("origin", "http://127.0.0.1:8080"),
                ("sec-fetch-site", "Cross-Site")
            ],
            host
        ));
        assert!(ok(
            &[
                ("origin", "http://localhost:8081"),
                ("sec-fetch-site", "cross-site")
            ],
            host
        ));
        assert!(ok(
            &[
                ("origin", "http://127.0.0.1:8080"),
                ("sec-fetch-site", "same-origin")
            ],
            host
        ));
        assert!(ok(&[("sec-fetch-site", "none")], host));
    }

    #[test]
    fn logged_values_are_escaped_and_capped() {
        assert_eq!(loggable(b"evil.example"), "\"evil.example\"");
        let injected = loggable(b"evil\r\ndbsloc_server: fake line");
        assert!(!injected.contains('\n') && !injected.contains('\r'));
        assert!(injected.contains("\\r\\n"));
        assert_eq!(loggable(b"h\xffst"), "\"h\u{fffd}st\"");
        let long = loggable(&[b'a'; 500]);
        assert_eq!(long.chars().count(), LOGGED_VALUE_CHARS + 3);
        assert!(long.ends_with("..."));
    }

    #[test]
    fn the_refused_value_is_the_host_or_the_origin_headers() {
        let req = HttpRequest::builder()
            .uri("http://from.uri/")
            .header("host", "evil.example")
            .header("origin", "http://evil.example")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap();
        assert_eq!(refused_value(&req, E_HOST), b"evil.example");
        assert_eq!(
            refused_value(&req, E_ORIGIN),
            b"origin=http://evil.example sec-fetch-site=cross-site"
        );
        let req = HttpRequest::builder()
            .uri("http://from.uri/")
            .body(Body::empty())
            .unwrap();
        assert_eq!(refused_value(&req, E_HOST), b"from.uri");
        assert_eq!(peer_of(&req), "unknown");
    }

    #[test]
    fn a_listed_null_origin_is_still_refused() {
        let cors = vec!["null".to_owned()];
        assert!(!origin_permitted(
            &headers(&[("origin", "null")]),
            Some("127.0.0.1:8080"),
            &cors
        ));
    }

    #[test]
    fn duplicate_or_binary_host_headers_are_malformed() {
        let req = HttpRequest::builder()
            .header("host", "127.0.0.1")
            .header("host", "evil.example")
            .body(Body::empty())
            .unwrap();
        assert!(request_authority(&req).is_err());
        let req = HttpRequest::builder()
            .header("host", HeaderValue::from_bytes(b"h\xffst").unwrap())
            .body(Body::empty())
            .unwrap();
        assert!(request_authority(&req).is_err());
        let req = HttpRequest::builder()
            .uri("http://evil.example:8080/api")
            .body(Body::empty())
            .unwrap();
        assert_eq!(request_authority(&req), Ok(Some("evil.example:8080")));
        let req = HttpRequest::builder().body(Body::empty()).unwrap();
        assert_eq!(request_authority(&req), Ok(None));
    }
}
