// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use flate2::write::DeflateEncoder;
use flate2::Compression;

use super::*;
use crate::sha1::sha1_hex;
use crate::sha256::sha256_hex;

const PREFIX: &str = "templates/space/MNI152NLin2009bAsym/atlases/";
const ALPHA: &str = "Alpha (Test 2020)";
const BETA: &str = "Beta (Test 2021)";
const ETAG_1: &str = "\"v1\"";
const ETAG_2: &str = "\"v2\"";

#[derive(Clone, Debug)]
struct Req {
    method: String,
    headers: HashMap<String, String>,
}

impl Req {
    fn range(&self) -> Option<(u64, Option<u64>)> {
        let v = self.headers.get("range")?.strip_prefix("bytes=")?;
        let (a, b) = v.split_once('-')?;
        Some((a.parse().ok()?, b.parse().ok()))
    }
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    declared_len: Option<usize>,
}

struct Server {
    addr: String,
    log: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = text.split("\r\n");
    let method = lines.next()?.split(' ').next()?.to_owned();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Some(Req { method, headers })
}

impl Server {
    fn start(handler: impl Fn(&Req) -> Resp + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log2, stop2) = (Arc::clone(&log), Arc::clone(&stop));
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let Some(req) = read_request(&mut stream) else {
                    continue;
                };
                log2.lock().unwrap().push(req.clone());
                let resp = handler(&req);
                let declared = resp.declared_len.unwrap_or(resp.body.len());
                let mut head = format!("HTTP/1.1 {} Status\r\n", resp.status);
                for (k, v) in &resp.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str(&format!(
                    "Content-Length: {declared}\r\nConnection: close\r\n\r\n"
                ));
                let _ = stream.write_all(head.as_bytes());
                if req.method != "HEAD" {
                    let _ = stream.write_all(&resp.body);
                }
                let _ = stream.flush();
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        Server {
            addr,
            log,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/leaddbs_data.zip", self.addr)
    }

    fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

const LAST_MODIFIED: &str = "Wed, 11 Nov 2026 10:00:00 GMT";
const WEAK_ETAG: &str = "W/\"weak1\"";

#[derive(Clone, Copy, Default)]
struct Behaviour {
    no_range: bool,
    truncate_big_get: bool,
    new_etag_after_truncate: bool,
    short_big_206: bool,
    weak_etag: bool,
    other_etag_on_get: bool,
}

fn zip_handler(data: Vec<u8>, b: Behaviour) -> impl Fn(&Req) -> Resp + Send + Sync {
    let truncated = AtomicBool::new(false);
    let sha1 = sha1_hex(&data);
    move |req| {
        let etag = if b.weak_etag {
            WEAK_ETAG
        } else if (b.other_etag_on_get && req.method == "GET")
            || (b.new_etag_after_truncate && truncated.load(Ordering::SeqCst))
        {
            ETAG_2
        } else {
            ETAG_1
        };
        let base = |status: u16, body: Vec<u8>| Resp {
            status,
            headers: vec![
                ("ETag".to_owned(), etag.to_owned()),
                ("Last-Modified".to_owned(), LAST_MODIFIED.to_owned()),
                ("OC-Checksum".to_owned(), format!("SHA1:{sha1}")),
            ],
            body,
            declared_len: None,
        };
        if req.method == "HEAD" {
            return base(200, data.clone());
        }
        let if_range_ok = b.other_etag_on_get
            || req
                .headers
                .get("if-range")
                .is_none_or(|v| (v == etag && !etag.starts_with("W/")) || v == LAST_MODIFIED);
        let range = req.range().filter(|_| !b.no_range && if_range_ok);
        let Some((start, end)) = range else {
            return base(200, data.clone());
        };
        let len = data.len() as u64;
        let mut end = end.unwrap_or(len - 1).min(len - 1);
        if b.short_big_206 && end - start + 1 > 100_000 {
            end = start + (end - start) / 2;
        }
        let mut resp = base(206, data[start as usize..=end as usize].to_vec());
        resp.headers.push((
            "Content-Range".to_owned(),
            format!("bytes {start}-{end}/{len}"),
        ));
        if b.truncate_big_get
            && resp.body.len() > 100_000
            && !truncated.swap(true, Ordering::SeqCst)
        {
            resp.declared_len = Some(resp.body.len());
            let half = resp.body.len() / 2;
            resp.body.truncate(half);
        }
        resp
    }
}

struct Member {
    name: String,
    data: Vec<u8>,
    deflate: bool,
}

fn member(name: String, data: Vec<u8>, deflate: bool) -> Member {
    Member {
        name,
        data,
        deflate,
    }
}

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn text(n: usize) -> Vec<u8> {
    b"atlas index structure STN GPi RN "
        .iter()
        .copied()
        .cycle()
        .take(n)
        .collect()
}

fn a(rel: &str) -> String {
    format!("{PREFIX}{ALPHA}/{rel}")
}

fn b(rel: &str) -> String {
    format!("{PREFIX}{BETA}/{rel}")
}

fn standard_members() -> Vec<Member> {
    vec![
        member(a(""), Vec::new(), false),
        member(a("atlas_index.mat"), text(200_000), true),
        member(a("lh/"), Vec::new(), false),
        member(a("lh/STN.nii.gz"), noise(1, 300_000), false),
        member(
            "templates/space/MNI152NLin2009bAsym/t1.nii".to_owned(),
            noise(2, 50_000),
            false,
        ),
        member(a("rh/STN.nii.gz"), noise(3, 120_000), true),
        member("templates/other.txt".to_owned(), text(5_000), true),
        member(b("atlas_index.mat"), text(1_000), true),
        member(b("rh/x.nii.gz"), noise(4, 2_000), false),
    ]
}

struct Built {
    bytes: Vec<u8>,
    spans: Vec<(String, u64, u64)>,
}

fn put16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn put32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn put64(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&v.to_le_bytes());
}

#[derive(Default)]
struct ZipOpts {
    zip64: bool,
    flip_in: Option<String>,
    comment: usize,
    zip64_extra: usize,
    offset_like: Option<(String, String)>,
}

fn build_zip(members: &[Member], zip64: bool, flip_in: Option<&str>) -> Built {
    build_zip_with(
        members,
        &ZipOpts {
            zip64,
            flip_in: flip_in.map(str::to_owned),
            ..ZipOpts::default()
        },
    )
}

fn build_zip_with(members: &[Member], o: &ZipOpts) -> Built {
    let zip64 = o.zip64;
    let flip_in = o.flip_in.as_deref();
    let mut out = Vec::new();
    let mut cd = Vec::new();
    let mut starts = Vec::new();
    let mut offset_of = HashMap::new();
    {
        let mut at = 0u64;
        for m in members {
            offset_of.insert(m.name.clone(), at);
            let comp_len = if m.deflate {
                let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
                e.write_all(&m.data).unwrap();
                e.finish().unwrap().len()
            } else {
                m.data.len()
            };
            at += 30 + m.name.len() as u64 + comp_len as u64;
        }
    }
    for m in members {
        let offset = out.len() as u64;
        starts.push((m.name.clone(), offset));
        let cd_offset_value = match &o.offset_like {
            Some((x, y)) if *x == m.name => offset_of[y],
            _ => offset,
        };
        let mut crc = Crc::new();
        crc.update(&m.data);
        let comp = if m.deflate {
            let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
            e.write_all(&m.data).unwrap();
            e.finish().unwrap()
        } else {
            m.data.clone()
        };
        let method: u16 = if m.deflate { 8 } else { 0 };
        put32(&mut out, SIG_LOCAL);
        put16(&mut out, 20);
        put16(&mut out, 0x800);
        put16(&mut out, method);
        put32(&mut out, 0);
        put32(&mut out, crc.sum());
        put32(&mut out, comp.len() as u32);
        put32(&mut out, m.data.len() as u32);
        put16(&mut out, m.name.len() as u16);
        put16(&mut out, 0);
        out.extend_from_slice(m.name.as_bytes());
        let data_at = out.len();
        out.extend_from_slice(&comp);
        if flip_in == Some(m.name.as_str()) {
            out[data_at + comp.len() / 2] ^= 0xFF;
        }
        put32(&mut cd, SIG_CENTRAL);
        put16(&mut cd, 45);
        put16(&mut cd, 20);
        put16(&mut cd, 0x800);
        put16(&mut cd, method);
        put32(&mut cd, 0);
        put32(&mut cd, crc.sum());
        let big = |v: u64| if zip64 { u32::MAX } else { v as u32 };
        put32(&mut cd, big(comp.len() as u64));
        put32(&mut cd, big(m.data.len() as u64));
        put16(&mut cd, m.name.len() as u16);
        put16(&mut cd, if zip64 { 28 } else { 0 });
        put16(&mut cd, 0);
        put16(&mut cd, 0);
        put16(&mut cd, 0);
        put32(&mut cd, 0);
        put32(&mut cd, big(cd_offset_value));
        cd.extend_from_slice(m.name.as_bytes());
        if zip64 {
            put16(&mut cd, 1);
            put16(&mut cd, 24);
            put64(&mut cd, m.data.len() as u64);
            put64(&mut cd, comp.len() as u64);
            put64(&mut cd, cd_offset_value);
        }
    }
    let cd_offset = out.len() as u64;
    out.extend_from_slice(&cd);
    let n = members.len() as u64;
    if zip64 {
        let z = out.len() as u64;
        put32(&mut out, SIG_ZIP64_EOCD);
        put64(&mut out, 44 + o.zip64_extra as u64);
        put16(&mut out, 45);
        put16(&mut out, 45);
        put32(&mut out, 0);
        put32(&mut out, 0);
        put64(&mut out, n);
        put64(&mut out, n);
        put64(&mut out, cd.len() as u64);
        put64(&mut out, cd_offset);
        out.extend(std::iter::repeat_n(0x5Au8, o.zip64_extra));
        put32(&mut out, SIG_LOCATOR);
        put32(&mut out, 0);
        put64(&mut out, z);
        put32(&mut out, 1);
        put32(&mut out, SIG_EOCD);
        put32(&mut out, 0);
        put16(&mut out, u16::MAX);
        put16(&mut out, u16::MAX);
        put32(&mut out, u32::MAX);
        put32(&mut out, u32::MAX);
        put16(&mut out, o.comment as u16);
    } else {
        put32(&mut out, SIG_EOCD);
        put32(&mut out, 0);
        put16(&mut out, n as u16);
        put16(&mut out, n as u16);
        put32(&mut out, cd.len() as u32);
        put32(&mut out, cd_offset as u32);
        put16(&mut out, o.comment as u16);
    }
    out.extend(std::iter::repeat_n(b'c', o.comment));
    let spans = starts
        .iter()
        .enumerate()
        .map(|(i, (name, s))| {
            let end = starts.get(i + 1).map_or(cd_offset, |n| n.1);
            (name.clone(), *s, end)
        })
        .collect();
    Built { bytes: out, spans }
}

fn manifest_for(members: &[Member], zip: &[u8], url: &str) -> AtlasManifest {
    let mut atlases = Vec::new();
    for name in [ALPHA, BETA] {
        let folder = format!("{PREFIX}{name}/");
        let mut files: Vec<ManifestFile> = members
            .iter()
            .filter_map(|m| {
                let rel = m.name.strip_prefix(&folder)?;
                (!rel.is_empty() && !rel.ends_with('/')).then(|| ManifestFile {
                    path: rel.to_owned(),
                    bytes: m.data.len() as u64,
                    sha256: sha256_hex(&m.data),
                })
            })
            .collect();
        files.sort_by(|x, y| x.path.cmp(&y.path));
        atlases.push(ManifestAtlas {
            name: name.to_owned(),
            bytes: files.iter().map(|f| f.bytes).sum(),
            files,
        });
    }
    AtlasManifest {
        package: PackagePin {
            file_name: "leaddbs_data.zip".to_owned(),
            url: url.to_owned(),
            bytes: zip.len() as u64,
            sha1: sha1_hex(zip),
            last_modified: "Wed, 11 Nov 2026 10:00:00 GMT".to_owned(),
            central_directory_offset: 0,
            atlas_prefix: PREFIX.to_owned(),
            pinned: "2026-11-11".to_owned(),
        },
        atlases,
    }
}

fn settings(server: &Server) -> AtlasFetchSettings {
    AtlasFetchSettings {
        url: Some(server.url()),
        user_agent: user_agent(),
        retry_delays: vec![Duration::from_millis(10); 5],
        free_space_override: Some(u64::MAX),
        bundled_dir: None,
        max_request_bytes: MAX_REQUEST_BYTES,
    }
}

struct Outcome {
    result: Res<PathBuf>,
    events: Vec<AtlasEvent>,
}

fn fetch(
    m: &AtlasManifest,
    name: &str,
    data: &Path,
    s: &AtlasFetchSettings,
    cancel_at_start: bool,
) -> Outcome {
    let cancel = AtomicBool::new(false);
    let mut events = Vec::new();
    let result = fetch_atlas_with(m, name, data, s, &cancel, &mut |ev| {
        if cancel_at_start && matches!(ev, AtlasEvent::Started { .. }) {
            cancel.store(true, Ordering::SeqCst);
        }
        events.push(ev);
    });
    Outcome { result, events }
}

fn member_data<'m>(members: &'m [Member], name: &str) -> &'m [u8] {
    &members.iter().find(|m| m.name == name).unwrap().data
}

fn assert_clean(data: &Path, name: &str) {
    assert!(!downloaded_atlases_dir(data).join(name).exists());
    let root = data.join("atlases");
    let leftovers: Vec<String> = std::fs::read_dir(&root)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn overlaps(req: &Req, len: u64, s: u64, e: u64) -> bool {
    match req.range() {
        Some((a, b)) => {
            let b = b.unwrap_or(len - 1);
            a < e && b >= s
        }
        None => true,
    }
}

#[test]
fn one_atlas_installs_byte_identical_and_only_its_runs_are_requested() {
    for zip64 in [false, true] {
        let members = standard_members();
        let built = build_zip(&members, zip64, None);
        let len = built.bytes.len() as u64;
        let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
        let m = manifest_for(&members, &built.bytes, &server.url());
        let d = tempfile::tempdir().unwrap();
        let out = fetch(&m, ALPHA, d.path(), &settings(&server), false);
        let path = out.result.unwrap();
        assert_eq!(path, downloaded_atlases_dir(d.path()).join(ALPHA));
        for rel in ["atlas_index.mat", "lh/STN.nii.gz", "rh/STN.nii.gz"] {
            let got = std::fs::read(staged_path(&path, rel)).unwrap();
            assert_eq!(got, member_data(&members, &a(rel)), "{rel} zip64={zip64}");
        }
        let rec: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path.join(INSTALL_RECORD)).unwrap())
                .unwrap();
        assert_eq!(rec["atlas"], ALPHA);
        assert_eq!(rec["source"], "leaddbs_data.zip");
        assert_eq!(rec["package_sha1"], m.package.sha1);
        assert_eq!(
            rec["package_last_modified"],
            "Wed, 11 Nov 2026 10:00:00 GMT"
        );
        assert_eq!(rec["files"], 3);
        assert_eq!(rec["bytes"], m.get(ALPHA).unwrap().bytes);
        assert_eq!(rec["version"], 1);
        assert!(rec["installed"].as_str().unwrap().ends_with('Z'));
        assert!(!downloaded_atlases_dir(d.path()).join(BETA).exists());
        assert!(!d.path().join("atlases").join(".download.lock").exists());
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "HEAD");
        for r in &reqs {
            assert!(r.headers["user-agent"].starts_with("dbsloc/"));
        }
        for (name, s, e) in &built.spans {
            if name.starts_with(&a("")) {
                continue;
            }
            for r in reqs.iter().filter(|r| r.method == "GET") {
                assert!(
                    !overlaps(r, len, *s, *e),
                    "{name} requested by {:?} zip64={zip64}",
                    r.range()
                );
            }
        }
        let run_gets = reqs
            .iter()
            .filter(|r| r.method == "GET")
            .filter(|r| {
                built
                    .spans
                    .iter()
                    .any(|(n, s, e)| n.starts_with(&a("")) && overlaps(r, len, *s, *e))
            })
            .count();
        assert_eq!(run_gets, 2, "zip64={zip64}");
        assert!(matches!(
            out.events.first(),
            Some(AtlasEvent::Started { .. })
        ));
        assert!(matches!(out.events.last(), Some(AtlasEvent::Done { .. })));
        assert!(out.events.contains(&AtlasEvent::Progress {
            downloaded: built
                .spans
                .iter()
                .filter(|(n, _, _)| n.starts_with(&a("")))
                .map(|(_, s, e)| e - s)
                .sum(),
            total: built
                .spans
                .iter()
                .filter(|(n, _, _)| n.starts_with(&a("")))
                .map(|(_, s, e)| e - s)
                .sum(),
        }));
        assert_clean_staging(d.path());
    }
}

fn assert_clean_staging(data: &Path) {
    let names: Vec<String> = std::fs::read_dir(data.join("atlases"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(names.is_empty(), "{names:?}");
}

#[test]
fn a_flipped_byte_or_a_wrong_pin_installs_nothing() {
    let members = standard_members();
    let built = build_zip(&members, false, Some(&a("lh/STN.nii.gz")));
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Checksum, "{err}");
    assert!(err.message.contains("CRC-32"), "{err}");
    assert_clean(d.path(), ALPHA);

    let good = build_zip(&members, false, None);
    let server = Server::start(zip_handler(good.bytes.clone(), Behaviour::default()));
    let mut m = manifest_for(&members, &good.bytes, &server.url());
    m.atlases[0].files[1].sha256 = "0".repeat(64);
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Checksum, "{err}");
    assert!(err.message.contains("SHA-256 mismatch"), "{err}");
    assert_clean(d.path(), ALPHA);
}

#[test]
fn an_extra_or_missing_member_means_the_package_changed() {
    let mut members = standard_members();
    let m_before = {
        let built = build_zip(&members, false, None);
        manifest_for(&members, &built.bytes, "http://unused")
    };
    members.insert(2, member(a("extra.txt"), text(10), false));
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let mut m = m_before.clone();
    m.package.url = server.url();
    let d = tempfile::tempdir().unwrap();
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::PackageChanged);
    assert_eq!(err.message, PACKAGE_CHANGED);
    assert_clean(d.path(), ALPHA);
    let len = built.bytes.len() as u64;
    for r in server.requests().iter().filter(|r| r.method == "GET") {
        for (_, s, e) in &built.spans {
            assert!(!overlaps(r, len, *s, *e), "{:?}", r.range());
        }
    }

    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let mut m = manifest_for(&members, &built.bytes, &server.url());
    m.atlases[0].files.push(ManifestFile {
        path: "zz_missing.nii.gz".to_owned(),
        bytes: 0,
        sha256: sha256_hex(b""),
    });
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::PackageChanged);
    assert_clean(d.path(), ALPHA);
}

#[test]
fn a_manifest_with_an_unsafe_path_or_bad_pin_is_refused() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let good = manifest_for(&members, &built.bytes, "https://example.invalid/p.zip");
    let text = serde_json::to_string(&good).unwrap();
    assert_eq!(parse_manifest(&text).unwrap(), good);
    for bad in [
        "../evil",
        "lh/../../evil",
        "a\\b",
        "C:x",
        "/abs",
        "a//b",
        "./a",
        "",
        INSTALL_RECORD,
    ] {
        let mut m = good.clone();
        m.atlases[0].files[0].path = bad.to_owned();
        let err = parse_manifest(&serde_json::to_string(&m).unwrap()).unwrap_err();
        assert_eq!(err.kind, AtlasErrorKind::Manifest, "{bad}");
    }
    let mut m = good.clone();
    m.atlases[0].files[0].sha256 = m.atlases[0].files[0].sha256.to_uppercase();
    assert!(parse_manifest(&serde_json::to_string(&m).unwrap()).is_err());
    let mut m = good.clone();
    m.atlases[1].name = ALPHA.to_owned();
    assert!(parse_manifest(&serde_json::to_string(&m).unwrap()).is_err());
    let mut m = good.clone();
    m.atlases[0].name = "../x".to_owned();
    assert!(parse_manifest(&serde_json::to_string(&m).unwrap()).is_err());
    let mut m = good;
    m.atlases[0].bytes += 1;
    assert!(parse_manifest(&serde_json::to_string(&m).unwrap()).is_err());
}

#[test]
fn windows_unsafe_paths_and_names_are_refused() {
    for bad in [
        "CON",
        "lh/nul.txt",
        "Com1.nii.gz",
        "lpt9",
        "lh/Aux",
        "prn.x/a",
        "a.",
        "lh./a",
        "a /b",
        "a ",
        "a<b",
        "a>b",
        "a|b",
        "a?b",
        "a*b",
        "a\"b",
        "a\u{1}b",
        "a\u{7f}b",
    ] {
        assert!(check_pinned_path(bad).is_err(), "{bad:?}");
    }
    for good in [
        "console.nii",
        "COM10.nii",
        "lh/COM0",
        "nulls/a.nii.gz",
        "lh/STN (Ewert).nii.gz",
        "a.b.c",
    ] {
        assert!(check_pinned_path(good).is_ok(), "{good:?}");
    }
    for bad in [
        "NUL", "Atlas.", "At|las", "Atlas ", ".hidden", "a/b", "a\\b", "x:y",
    ] {
        assert!(check_atlas_name(bad).is_err(), "{bad:?}");
    }
    assert!(check_atlas_name("DISTAL Minimal (Ewert 2017)").is_ok());
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let mut m = manifest_for(&members, &built.bytes, "https://example.invalid/p.zip");
    let mut twin = m.atlases[0].files[0].clone();
    twin.path = twin.path.to_uppercase();
    m.atlases[0].bytes += twin.bytes;
    m.atlases[0].files.push(twin);
    let err = parse_manifest(&serde_json::to_string(&m).unwrap()).unwrap_err();
    assert!(err.message.contains("listed twice"), "{err}");
}

#[test]
fn a_server_without_range_support_is_refused() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(
        built.bytes.clone(),
        Behaviour {
            no_range: true,
            ..Behaviour::default()
        },
    ));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::NoPartial);
    assert_eq!(err.message, NO_PARTIAL);
    assert_clean(d.path(), ALPHA);
}

#[test]
fn a_dropped_connection_resumes_and_succeeds() {
    let members = standard_members();
    let built = build_zip(&members, true, None);
    let server = Server::start(zip_handler(
        built.bytes.clone(),
        Behaviour {
            truncate_big_get: true,
            ..Behaviour::default()
        },
    ));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let out = fetch(&m, ALPHA, d.path(), &settings(&server), false);
    let path = out.result.unwrap();
    let got = std::fs::read(path.join("lh").join("STN.nii.gz")).unwrap();
    assert_eq!(got, member_data(&members, &a("lh/STN.nii.gz")));
    assert!(out
        .events
        .iter()
        .any(|e| matches!(e, AtlasEvent::Retry { attempt: 1, .. })));
    assert!(continued_inside_a_run(
        &server.requests(),
        &built,
        Some(ETAG_1)
    ));
}

fn alpha_runs(built: &Built) -> Vec<(u64, u64)> {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let mut last_alpha = false;
    for (name, s, e) in &built.spans {
        let alpha = name.starts_with(&a(""));
        if alpha {
            match runs.last_mut() {
                Some(r) if last_alpha => r.1 = *e,
                _ => runs.push((*s, *e)),
            }
        }
        last_alpha = alpha;
    }
    runs
}

fn continued_inside_a_run(reqs: &[Req], built: &Built, if_range: Option<&str>) -> bool {
    let runs = alpha_runs(built);
    reqs.iter()
        .filter(|r| r.method == "GET")
        .filter(|r| {
            if_range.is_none_or(|v| r.headers.get("if-range").map(String::as_str) == Some(v))
        })
        .filter_map(Req::range)
        .any(|(s, _)| runs.iter().any(|(rs, re)| s > *rs && s < *re))
}

#[test]
fn a_short_206_is_continued_without_a_retry() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(
        built.bytes.clone(),
        Behaviour {
            short_big_206: true,
            ..Behaviour::default()
        },
    ));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let out = fetch(&m, ALPHA, d.path(), &settings(&server), false);
    let path = out.result.unwrap();
    let got = std::fs::read(path.join("lh").join("STN.nii.gz")).unwrap();
    assert_eq!(got, member_data(&members, &a("lh/STN.nii.gz")));
    assert!(
        !out.events
            .iter()
            .any(|e| matches!(e, AtlasEvent::Retry { .. })),
        "{:?}",
        out.events
    );
    assert!(continued_inside_a_run(&server.requests(), &built, None));
}

#[test]
fn a_weak_etag_is_not_used_for_if_range() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(
        built.bytes.clone(),
        Behaviour {
            weak_etag: true,
            truncate_big_get: true,
            ..Behaviour::default()
        },
    ));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let out = fetch(&m, ALPHA, d.path(), &settings(&server), false);
    out.result.unwrap();
    let reqs = server.requests();
    let validators: Vec<&str> = reqs
        .iter()
        .filter_map(|r| r.headers.get("if-range").map(String::as_str))
        .collect();
    assert!(!validators.is_empty());
    assert!(
        validators.iter().all(|v| *v == LAST_MODIFIED),
        "{validators:?}"
    );
    assert!(continued_inside_a_run(&reqs, &built, Some(LAST_MODIFIED)));
}

#[test]
fn a_206_with_another_etag_aborts() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(
        built.bytes.clone(),
        Behaviour {
            other_etag_on_get: true,
            ..Behaviour::default()
        },
    ));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::PackageChanged, "{err}");
    assert!(err.message.contains("during the download"), "{err}");
    assert_clean(d.path(), ALPHA);
}

#[test]
fn an_end_record_with_a_comment_is_found_with_a_longer_tail_read() {
    let members = standard_members();
    let built = build_zip_with(
        &members,
        &ZipOpts {
            comment: 300,
            ..ZipOpts::default()
        },
    );
    let len = built.bytes.len() as u64;
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let path = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap();
    assert!(path.join(INSTALL_RECORD).is_file());
    let tails = server
        .requests()
        .iter()
        .filter_map(Req::range)
        .filter(|(_, e)| *e == Some(len - 1))
        .count();
    assert_eq!(tails, 2);
}

#[test]
fn a_zip64_locator_outside_the_tail_is_read_separately() {
    let members = standard_members();
    let built = build_zip_with(
        &members,
        &ZipOpts {
            zip64: true,
            zip64_extra: 100,
            ..ZipOpts::default()
        },
    );
    let len = built.bytes.len() as u64;
    let record = len - 22 - 20 - 56 - 100;
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap();
    assert!(server
        .requests()
        .iter()
        .filter_map(Req::range)
        .any(|r| r == (record, Some(record + 55))));
}

#[test]
fn an_entry_sharing_its_offset_is_a_broken_package() {
    let members = standard_members();
    let built = build_zip_with(
        &members,
        &ZipOpts {
            offset_like: Some((a("rh/STN.nii.gz"), "templates/other.txt".to_owned())),
            ..ZipOpts::default()
        },
    );
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Archive, "{err}");
    assert!(err.message.contains("broken"), "{err}");
    assert_clean(d.path(), ALPHA);
}

#[test]
fn a_folder_without_an_install_record_is_never_replaced() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let dest = downloaded_atlases_dir(d.path()).join(BETA);
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("edited.nii"), b"mine").unwrap();
    let err = fetch(&m, BETA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::ExistingFolder, "{err}");
    assert!(err.message.contains("remove or rename it first"), "{err}");
    assert_eq!(std::fs::read(dest.join("edited.nii")).unwrap(), b"mine");
    assert!(server.requests().is_empty());
}

#[test]
fn leftovers_are_swept_at_the_next_fetch_and_remove() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let user = downloaded_atlases_dir(d.path());
    let removing = user.join(".removing-Old-1");
    let unknown = user.join(".partial-old-not a stamp");
    let fresh_staging = d.path().join("atlases").join(".staging-Other-3");
    for p in [&removing, &unknown, &fresh_staging] {
        std::fs::create_dir_all(p).unwrap();
    }
    fetch(&m, BETA, d.path(), &settings(&server), false)
        .result
        .unwrap();
    assert!(!removing.exists());
    assert!(unknown.exists() && fresh_staging.exists());
    std::fs::create_dir_all(&removing).unwrap();
    remove_atlas(BETA, d.path(), None).unwrap();
    assert!(!removing.exists());
    sweep_leftovers(d.path(), None, Duration::ZERO);
    assert!(!fresh_staging.exists());
    assert!(unknown.exists());
}

#[test]
fn remove_refuses_while_a_download_runs() {
    let d = tempfile::tempdir().unwrap();
    let dir = downloaded_atlases_dir(d.path()).join("Fetched");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(INSTALL_RECORD), b"{}").unwrap();
    std::fs::write(
        d.path().join("atlases").join(".download.lock"),
        r#"{"pid": 1, "host": "another-computer"}"#,
    )
    .unwrap();
    let err = remove_atlas("Fetched", d.path(), None).unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Busy, "{err}");
    assert!(err.message.contains("another atlas"), "{err}");
    assert!(dir.is_dir());
}

#[test]
fn a_changed_etag_on_resume_aborts() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(
        built.bytes.clone(),
        Behaviour {
            truncate_big_get: true,
            new_etag_after_truncate: true,
            ..Behaviour::default()
        },
    ));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::PackageChanged, "{err}");
    assert!(err.message.contains("during the download"), "{err}");
    assert_clean(d.path(), ALPHA);
}

#[test]
fn cancel_leaves_no_folder_and_no_staging() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), true)
        .result
        .unwrap_err();
    assert!(err.is_cancelled(), "{err}");
    assert_clean(d.path(), ALPHA);
}

#[test]
fn cancel_on_the_first_progress_event_leaves_nothing() {
    let mut members = standard_members();
    let big = (PROGRESS_STEP as usize) * 3;
    members.insert(4, member(a("lh/big.nii.gz"), noise(9, big), false));
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let mut downloaded_at_cancel = None;
    let err = fetch_atlas_with(
        &m,
        ALPHA,
        d.path(),
        &settings(&server),
        &cancel,
        &mut |ev| {
            if let AtlasEvent::Progress { downloaded, total } = ev {
                if downloaded_at_cancel.is_none() {
                    downloaded_at_cancel = Some((downloaded, total));
                    cancel.store(true, Ordering::SeqCst);
                }
            }
        },
    )
    .unwrap_err();
    assert!(err.is_cancelled(), "{err}");
    let (downloaded, total) = downloaded_at_cancel.unwrap();
    assert!(
        downloaded >= PROGRESS_STEP && downloaded < total,
        "{downloaded} {total}"
    );
    assert_clean(d.path(), ALPHA);
}

#[test]
fn the_install_record_name_is_reserved_in_any_case() {
    for bad in [
        ".dbsloc-install.json",
        ".DBSLOC-INSTALL.JSON",
        "lh/.Dbsloc-Install.Json",
    ] {
        assert!(check_pinned_path(bad).is_err(), "{bad}");
    }
    assert!(check_pinned_path("lh/dbsloc-install.json").is_ok());
}

#[test]
fn the_request_timeout_follows_the_range_length() {
    assert_eq!(request_timeout(0), Duration::from_secs(120));
    assert_eq!(request_timeout(6_000_000), Duration::from_secs(120));
    assert_eq!(request_timeout(MAX_REQUEST_BYTES), Duration::from_secs(640));
    assert!(request_timeout(MAX_REQUEST_BYTES) <= Duration::from_secs(11 * 60));
}

#[test]
fn a_run_longer_than_the_request_cap_is_fetched_in_several_requests() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let len = built.bytes.len() as u64;
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let cap = 40_000u64;
    let mut s = settings(&server);
    s.max_request_bytes = cap;
    let out = fetch(&m, ALPHA, d.path(), &s, false);
    let path = out.result.unwrap();
    let got = std::fs::read(path.join("lh").join("STN.nii.gz")).unwrap();
    assert_eq!(got, member_data(&members, &a("lh/STN.nii.gz")));
    assert!(!out
        .events
        .iter()
        .any(|e| matches!(e, AtlasEvent::Retry { .. })));
    let runs = alpha_runs(&built);
    let first = runs[0];
    assert!(first.1 - first.0 > 3 * cap);
    let ranges: Vec<(u64, u64)> = server
        .requests()
        .iter()
        .filter(|r| r.method == "GET")
        .filter_map(Req::range)
        .map(|(s, e)| (s, e.unwrap_or(len - 1)))
        .collect();
    assert!(ranges.iter().all(|(s, e)| e - s < cap), "{ranges:?}");
    let in_first = ranges
        .iter()
        .filter(|(s, _)| *s >= first.0 && *s < first.1)
        .count() as u64;
    assert_eq!(in_first, (first.1 - first.0).div_ceil(cap));
}

#[test]
fn a_failed_install_rename_puts_the_old_copy_back() {
    let d = tempfile::tempdir().unwrap();
    let staging = d.path().join("atlases").join(".staging-X-1");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("a.nii"), b"new").unwrap();
    let dest = downloaded_atlases_dir(d.path()).join("X");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("old.nii"), b"old").unwrap();
    let refuse_staging = |from: &Path, to: &Path| {
        if from == staging.as_path() {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "held by a scanner",
            ))
        } else {
            std::fs::rename(from, to)
        }
    };
    let err = install_dir(&staging, &dest, &refuse_staging).unwrap_err();
    assert!(err.message.contains("cannot install the download"), "{err}");
    assert_eq!(std::fs::read(dest.join("old.nii")).unwrap(), b"old");
    assert!(staging.join("a.nii").is_file());
    let names: Vec<String> = std::fs::read_dir(downloaded_atlases_dir(d.path()))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["X"]);
}

fn old_copies(data: &Path, name: &str) -> Vec<PathBuf> {
    let prefix = format!("{OLD_COPY_PREFIX}{name}-");
    std::fs::read_dir(downloaded_atlases_dir(data))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

fn fail_install_keeping_aside(data: &Path, name: &str) -> (AtlasFetchError, PathBuf) {
    let staging = data.join("atlases").join(format!(".staging-{name}-1"));
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("a.nii"), b"new").unwrap();
    let dest = downloaded_atlases_dir(data).join(name);
    let only_aside = |from: &Path, to: &Path| {
        if from == dest.as_path() {
            std::fs::rename(from, to)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "held by a scanner",
            ))
        }
    };
    let err = install_dir(&staging, &dest, &only_aside).unwrap_err();
    let _ = std::fs::remove_dir_all(&staging);
    assert!(!dest.exists());
    let kept = old_copies(data, name);
    assert_eq!(kept.len(), 1, "{kept:?}");
    (err, kept[0].clone())
}

fn set_dir_mtime(dir: &Path, t: std::time::SystemTime) {
    #[cfg(windows)]
    let f = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::File::options()
            .access_mode(0x0100)
            .custom_flags(0x0200_0000)
            .open(dir)
            .unwrap()
    };
    #[cfg(not(windows))]
    let f = std::fs::File::open(dir).unwrap();
    f.set_modified(t).unwrap();
}

#[test]
fn a_failed_rollback_names_the_kept_copy_and_the_sweep_restores_even_an_old_install() {
    let d = tempfile::tempdir().unwrap();
    let dest = downloaded_atlases_dir(d.path()).join("X");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("old.nii"), b"old").unwrap();
    let three_days = std::time::SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
    set_dir_mtime(&dest, three_days);
    let age = std::fs::metadata(&dest)
        .unwrap()
        .modified()
        .unwrap()
        .elapsed()
        .unwrap();
    assert!(age > Duration::from_secs(2 * 24 * 3600));
    let (err, aside) = fail_install_keeping_aside(d.path(), "X");
    assert!(err.message.contains(&aside.display().to_string()), "{err}");
    assert_eq!(std::fs::read(aside.join("old.nii")).unwrap(), b"old");
    sweep_leftovers(d.path(), None, STALE_STAGING_AGE);
    assert_eq!(std::fs::read(dest.join("old.nii")).unwrap(), b"old");
    assert!(old_copies(d.path(), "X").is_empty());
}

#[test]
fn after_a_failed_rollback_the_next_fetch_restores_then_replaces() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let dest = fetch(&m, BETA, d.path(), &settings(&server), false)
        .result
        .unwrap();
    std::fs::write(dest.join("marker.txt"), b"first install").unwrap();
    let (_, aside) = fail_install_keeping_aside(d.path(), BETA);
    assert!(aside.join("marker.txt").is_file());
    let again = fetch(&m, BETA, d.path(), &settings(&server), false)
        .result
        .unwrap();
    assert_eq!(again, dest);
    assert!(dest.join(INSTALL_RECORD).is_file());
    assert_eq!(
        std::fs::read(dest.join("rh").join("x.nii.gz")).unwrap(),
        member_data(&members, &b("rh/x.nii.gz"))
    );
    assert!(!dest.join("marker.txt").exists());
    assert!(old_copies(d.path(), BETA).is_empty());
}

fn crashed_copy(data: &Path, name: &str, stamp: u64) -> PathBuf {
    let p = downloaded_atlases_dir(data).join(format!("{OLD_COPY_PREFIX}{name}-77-{stamp:020}"));
    std::fs::create_dir_all(p.join("lh")).unwrap();
    std::fs::write(p.join(INSTALL_RECORD), b"{}").unwrap();
    std::fs::write(p.join("stamp.txt"), stamp.to_string()).unwrap();
    p
}

#[test]
fn a_copy_left_by_a_crash_is_restored_before_remove_and_fetch() {
    let d = tempfile::tempdir().unwrap();
    crashed_copy(d.path(), BETA, 5);
    let removed = remove_atlas(BETA, d.path(), None).unwrap();
    assert!(!removed.exists());
    assert!(old_copies(d.path(), BETA).is_empty());

    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    crashed_copy(d.path(), ALPHA, 9);
    let path = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap();
    assert!(!path.join("stamp.txt").exists());
    assert!(path.join("lh").join("STN.nii.gz").is_file());
    assert!(old_copies(d.path(), ALPHA).is_empty());
}

#[test]
fn the_newest_copy_is_restored_and_others_go_only_while_the_folder_exists() {
    let d = tempfile::tempdir().unwrap();
    let user = downloaded_atlases_dir(d.path());
    crashed_copy(d.path(), "X", 5);
    crashed_copy(d.path(), "X", 12);
    crashed_copy(d.path(), "X", 7);
    recover_old_copies(&user);
    assert_eq!(
        std::fs::read(user.join("X").join("stamp.txt")).unwrap(),
        b"12"
    );
    assert!(old_copies(d.path(), "X").is_empty());

    std::fs::create_dir_all(user.join("Y")).unwrap();
    std::fs::write(user.join("Y").join("keep.txt"), b"y").unwrap();
    crashed_copy(d.path(), "Y", 1);
    crashed_copy(d.path(), "Y", 2);
    recover_old_copies(&user);
    assert!(user.join("Y").join("keep.txt").is_file());
    assert!(old_copies(d.path(), "Y").is_empty());

    assert_eq!(
        parse_old_copy(".partial-old-A-b (C 2020)-12-00000000000000000042"),
        Some(("A-b (C 2020)".to_owned(), 42))
    );
    assert_eq!(parse_old_copy(".partial-old-X-12"), None);
    assert_eq!(
        parse_old_copy(".partial-old-..-1-00000000000000000002"),
        None
    );
    assert_eq!(parse_old_copy(".partial-old-X-2-1234"), None);
    assert_eq!(
        parse_old_copy(".partial-old-X-2-+0000000000000000001"),
        None
    );
    assert_eq!(
        parse_old_copy(".partial-old-X-2-99999999999999999999"),
        None
    );
    assert_eq!(
        parse_old_copy(".partial-old-X-2-00000000000000001234"),
        Some(("X".to_owned(), 1234))
    );
}

#[test]
fn an_old_copy_whose_discard_rename_fails_is_left_whole_and_swept_later() {
    let d = tempfile::tempdir().unwrap();
    let staging = d.path().join("atlases").join(".staging-X-1");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("a.nii"), b"new").unwrap();
    let dest = downloaded_atlases_dir(d.path()).join("X");
    std::fs::create_dir_all(dest.join("lh")).unwrap();
    std::fs::write(dest.join("old.nii"), b"old").unwrap();
    std::fs::write(dest.join("lh").join("deep.nii"), b"deep").unwrap();
    let no_removing = |from: &Path, to: &Path| {
        let target = to.file_name().unwrap().to_string_lossy().into_owned();
        if target.starts_with(REMOVING_PREFIX) {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "held by a scanner",
            ))
        } else {
            std::fs::rename(from, to)
        }
    };
    install_dir(&staging, &dest, &no_removing).unwrap();
    assert_eq!(std::fs::read(dest.join("a.nii")).unwrap(), b"new");
    let kept = old_copies(d.path(), "X");
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read(kept[0].join("old.nii")).unwrap(), b"old");
    assert_eq!(
        std::fs::read(kept[0].join("lh").join("deep.nii")).unwrap(),
        b"deep"
    );
    sweep_leftovers(d.path(), None, STALE_STAGING_AGE);
    assert!(old_copies(d.path(), "X").is_empty());
    assert_eq!(std::fs::read(dest.join("a.nii")).unwrap(), b"new");
    let names: Vec<String> = std::fs::read_dir(downloaded_atlases_dir(d.path()))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["X"]);
}

#[test]
fn a_removing_copy_is_never_restored() {
    let d = tempfile::tempdir().unwrap();
    let user = downloaded_atlases_dir(d.path());
    let doomed = user.join(format!("{REMOVING_PREFIX}X-77-00000000000000000005"));
    std::fs::create_dir_all(&doomed).unwrap();
    std::fs::write(doomed.join(INSTALL_RECORD), b"{}").unwrap();
    sweep_leftovers(d.path(), None, STALE_STAGING_AGE);
    assert!(!user.join("X").exists());
    assert!(!doomed.exists());
}

#[test]
fn set_aside_names_never_repeat() {
    let a1 = aside_stamp();
    let a2 = aside_stamp();
    assert!(a2 > a1);
}

#[test]
fn renames_are_retried_before_giving_up() {
    let calls = std::cell::Cell::new(0u32);
    let flaky = |_: &Path, _: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() < 3 {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "held by a scanner",
            ))
        } else {
            Ok(())
        }
    };
    rename_with_retries(Path::new("a"), Path::new("b"), 5, Duration::ZERO, &flaky).unwrap();
    assert_eq!(calls.get(), 3);
    calls.set(0);
    let never = |_: &Path, _: &Path| {
        calls.set(calls.get() + 1);
        Err(std::io::Error::other("still held"))
    };
    assert!(
        rename_with_retries(Path::new("a"), Path::new("b"), 5, Duration::ZERO, &never).is_err()
    );
    assert_eq!(calls.get(), 5);

    let d = tempfile::tempdir().unwrap();
    let staging = d.path().join(".staging-x");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("a.nii"), b"new").unwrap();
    let dest = d.path().join("leaddbs").join("X");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("old.nii"), b"old").unwrap();
    let tries = std::cell::Cell::new(0u32);
    let sometimes = |from: &Path, to: &Path| {
        tries.set(tries.get() + 1);
        if tries.get() % 2 == 1 {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "held by a scanner",
            ))
        } else {
            std::fs::rename(from, to)
        }
    };
    install_dir(&staging, &dest, &sometimes).unwrap();
    assert_eq!(std::fs::read(dest.join("a.nii")).unwrap(), b"new");
    assert!(!dest.join("old.nii").exists());
    assert!(!staging.exists());
    let left: Vec<_> = std::fs::read_dir(d.path().join("leaddbs"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(left.len(), 1);
}

#[test]
fn bundled_and_unknown_names_are_refused_before_any_request() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let bundled = d.path().join("bundled");
    std::fs::create_dir_all(bundled.join(ALPHA)).unwrap();
    let mut s = settings(&server);
    s.bundled_dir = Some(bundled);
    let err = fetch(&m, ALPHA, d.path(), &s, false).result.unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Bundled);
    assert!(err.message.contains("already bundled"));
    let err = fetch(&m, "Nope", d.path(), &s, false).result.unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::UnknownName);
    assert!(err.message.contains("dbsloc atlas list"));
    assert!(err.is_usage());
    assert!(server.requests().is_empty());
}

#[test]
fn a_changed_package_is_a_note_and_per_file_pins_decide() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let mut m = manifest_for(&members, &built.bytes, &server.url());
    m.package.bytes += 1;
    m.package.sha1 = "1".repeat(40);
    let d = tempfile::tempdir().unwrap();
    let out = fetch(&m, BETA, d.path(), &settings(&server), false);
    assert!(out.result.is_ok());
    assert!(out.events.iter().any(|e| matches!(
        e,
        AtlasEvent::Note { message } if message.contains("changed on Wed, 11 Nov 2026")
    )));
}

#[test]
fn a_running_download_holds_the_lock() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("atlases");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join(".download.lock"),
        r#"{"pid": 1, "host": "another-computer", "id": "Other"}"#,
    )
    .unwrap();
    let err = fetch(&m, ALPHA, d.path(), &settings(&server), false)
        .result
        .unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Busy, "{err}");
    assert!(server.requests().is_empty());
}

#[test]
fn free_space_is_checked_with_the_numbers() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let mut s = settings(&server);
    s.free_space_override = Some(5_000_000);
    let err = fetch(&m, ALPHA, d.path(), &s, false).result.unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Disk);
    let need = required_free_bytes(m.get(ALPHA).unwrap());
    assert_eq!(
        err.message,
        format!(
            "not enough free disk space for {ALPHA}: need {} MB, 5 MB free",
            need.div_ceil(1_000_000)
        )
    );
    assert!(server.requests().is_empty());
}

#[test]
fn a_second_install_replaces_the_first_without_leftovers() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let server = Server::start(zip_handler(built.bytes.clone(), Behaviour::default()));
    let m = manifest_for(&members, &built.bytes, &server.url());
    let d = tempfile::tempdir().unwrap();
    let path = fetch(&m, BETA, d.path(), &settings(&server), false)
        .result
        .unwrap();
    std::fs::write(path.join("stray.txt"), b"x").unwrap();
    let again = fetch(&m, BETA, d.path(), &settings(&server), false)
        .result
        .unwrap();
    assert_eq!(again, path);
    assert!(!path.join("stray.txt").exists());
    let names: Vec<String> = std::fs::read_dir(downloaded_atlases_dir(d.path()))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [BETA]);
}

#[test]
fn stale_staging_folders_are_swept() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("atlases");
    let mine = root.join(".staging-A-1");
    let other = root.join(".staging-B-2");
    let unrelated = root.join("leaddbs");
    for p in [&mine, &other, &unrelated] {
        std::fs::create_dir_all(p).unwrap();
    }
    sweep_leftovers(d.path(), Some(&mine), Duration::from_secs(3600));
    assert!(other.exists());
    sweep_leftovers(d.path(), Some(&mine), Duration::ZERO);
    assert!(mine.exists() && unrelated.exists());
    assert!(!other.exists());
}

#[test]
fn remove_needs_the_install_record() {
    let d = tempfile::tempdir().unwrap();
    let dir = downloaded_atlases_dir(d.path());
    std::fs::create_dir_all(dir.join("Copied")).unwrap();
    let err = remove_atlas("Copied", d.path(), None).unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::NotDownloaded);
    assert!(dir.join("Copied").is_dir());
    std::fs::create_dir_all(dir.join("Fetched").join("lh")).unwrap();
    std::fs::write(dir.join("Fetched").join(INSTALL_RECORD), b"{}").unwrap();
    assert_eq!(
        remove_atlas("Fetched", d.path(), None).unwrap(),
        dir.join("Fetched")
    );
    assert!(!dir.join("Fetched").exists());
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(names.len(), 1);
    let bundled = d.path().join("bundled");
    std::fs::create_dir_all(bundled.join("Shipped")).unwrap();
    let err = remove_atlas("Shipped", d.path(), Some(&bundled)).unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Bundled);
    let err = remove_atlas("Nothing", d.path(), Some(&bundled)).unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::NotInstalled);
    assert!(remove_atlas("../x", d.path(), None).is_err());
}

#[test]
fn list_marks_bundled_installed_and_available() {
    let members = standard_members();
    let built = build_zip(&members, false, None);
    let m = manifest_for(&members, &built.bytes, "https://example.invalid/p.zip");
    let d = tempfile::tempdir().unwrap();
    let bundled = d.path().join("resources").join("atlases").join("leaddbs");
    std::fs::create_dir_all(bundled.join("Shipped Only")).unwrap();
    std::fs::write(bundled.join("Shipped Only").join("a.nii"), [0u8; 10]).unwrap();
    let data = d.path().join("data");
    let user = downloaded_atlases_dir(&data);
    std::fs::create_dir_all(user.join(BETA)).unwrap();
    std::fs::write(user.join(BETA).join(INSTALL_RECORD), b"{}").unwrap();
    let rows = list_atlases_in(&m, Some(&bundled), Some(&data));
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, [ALPHA, BETA, "Shipped Only"]);
    assert_eq!(rows[0].state, AtlasState::Available);
    assert_eq!(rows[0].bytes, m.get(ALPHA).unwrap().bytes);
    assert_eq!(rows[1].state.label(), "installed");
    assert!(rows[1].state.removable());
    assert_eq!(rows[1].state.path(), Some(user.join(BETA).as_path()));
    assert_eq!(rows[2].state.label(), "bundled");
    assert_eq!(rows[2].bytes, 10);
    assert!(!rows[2].state.removable());
    let none = list_atlases_in(&m, None, None);
    assert!(none.iter().all(|r| r.state == AtlasState::Available));
}

#[test]
fn embedded_manifest_meets_the_acceptance_checks() {
    let m = manifest().unwrap();
    assert_eq!(m.atlases.len(), 73);
    let mut names = HashSet::new();
    for a in &m.atlases {
        assert!(names.insert(a.name.as_str()), "{}", a.name);
        for f in &a.files {
            assert!(is_lower_hex(&f.sha256, 64), "{} {}", a.name, f.path);
            check_pinned_path(&f.path).unwrap();
        }
    }
    let distal = crate::atlas_catalog::DEFAULT_ATLAS_NAME;
    assert!(m.get(distal).is_some());
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/atlases/leaddbs");
    let err = resolve_download(m, distal, Some(&bundled)).unwrap_err();
    assert_eq!(err.kind, AtlasErrorKind::Bundled);
    let mut sorted: Vec<&[u8]> = m.atlases.iter().map(|a| a.name.as_bytes()).collect();
    sorted.sort_unstable();
    let listed: Vec<&[u8]> = m.atlases.iter().map(|a| a.name.as_bytes()).collect();
    assert_eq!(sorted, listed);
}
