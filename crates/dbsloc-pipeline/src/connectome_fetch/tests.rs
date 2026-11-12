// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use super::*;
use crate::connectome_archive::tests::tar_bytes;
use crate::md5::md5_hex;
use crate::sha1::sha1_hex;

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    declared_len: Option<usize>,
}

impl Resp {
    fn new(status: u16, body: Vec<u8>) -> Resp {
        Resp {
            status,
            headers: Vec::new(),
            body,
            declared_len: None,
        }
    }

    fn header(mut self, k: &str, v: &str) -> Resp {
        self.headers.push((k.to_owned(), v.to_owned()));
        self
    }
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
    let first = lines.next()?;
    let mut parts = first.split(' ');
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Some(Req {
        method,
        path,
        headers,
    })
}

impl Server {
    fn start(handler: impl Fn(&Req, usize) -> Resp + Send + Sync + 'static) -> Server {
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
                let index = {
                    let mut l = log2.lock().unwrap();
                    l.push(req.clone());
                    l.len() - 1
                };
                let resp = handler(&req, index);
                let reason = match resp.status {
                    200 => "OK",
                    206 => "Partial Content",
                    302 => "Found",
                    303 => "See Other",
                    403 => "Forbidden",
                    404 => "Not Found",
                    429 => "Too Many Requests",
                    _ => "Status",
                };
                let declared = resp.declared_len.unwrap_or(resp.body.len());
                let mut head = format!("HTTP/1.1 {} {reason}\r\n", resp.status);
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

    fn base(&self) -> String {
        format!("http://{}", self.addr)
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

fn leak_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in members {
        w.start_file(*name, SimpleFileOptions::default()).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn base_entry() -> ConnectomeEntry {
    lookup("loh2020-torpd75").unwrap().clone()
}

fn zip_entry(id: &str, data: &[u8], folder: &str, verified: bool) -> ConnectomeEntry {
    let mut e = base_entry();
    e.id = leak_str(id.to_owned());
    e.files = Box::leak(
        vec![ConnectomeFile {
            source: Source::LeadDbs {
                id: leak_str(format!("{id}-lead")),
            },
            file_name: "pack.zip",
            bytes: data.len() as u64,
            source_digest: leak_str(format!("sha1:{}", sha1_hex(data))),
            sha256: "",
        }]
        .into_boxed_slice(),
    );
    e.install = Install::ZipFolder {
        folder: leak_str(folder.to_owned()),
    };
    e.folder_verified = verified;
    e.marker = "dataset_info.json";
    e
}

fn serve_file(req: &Req, data: &[u8], honour_range: bool, oc: Option<&str>) -> Resp {
    let range = req
        .headers
        .get("range")
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.strip_suffix('-'))
        .and_then(|v| v.parse::<usize>().ok());
    let mut resp = match range {
        Some(n) if honour_range && n > 0 => Resp::new(206, data[n..].to_vec()).header(
            "Content-Range",
            &format!("bytes {n}-{}/{}", data.len() - 1, data.len()),
        ),
        _ => Resp::new(200, data.to_vec()),
    };
    resp = resp.header("ETag", "\"v1\"");
    let oc = oc.map_or_else(|| format!("SHA1:{}", sha1_hex(data)), str::to_owned);
    resp.header("OC-Checksum", &oc)
}

fn lead_handler(
    data: Vec<u8>,
    honour_range: bool,
    truncate_first: bool,
    oc: Option<String>,
) -> impl Fn(&Req, usize) -> Resp + Send + Sync {
    let file_hits = Arc::new(Mutex::new(0usize));
    move |req, _| {
        if req.path.starts_with("/release") {
            return Resp::new(302, Vec::new()).header("Location", "/files/pack.zip");
        }
        let mut hits = file_hits.lock().unwrap();
        *hits += 1;
        let mut resp = serve_file(req, &data, honour_range, oc.as_deref());
        if truncate_first && *hits == 1 {
            let half = data.len() / 2;
            resp.declared_len = Some(resp.body.len());
            resp.body.truncate(half);
        }
        resp
    }
}

fn settings_for(server: &Server) -> FetchSettings {
    FetchSettings {
        leaddbs_release_url: format!("{}/release", server.base()),
        dataverse_base_url: server.base(),
        figshare_base_url: format!("{}/files", server.base()),
        retry_delays: vec![Duration::from_millis(10), Duration::from_millis(10)],
        disk_slack_bytes: 0,
        ..FetchSettings::default()
    }
}

fn accept() -> Acceptance {
    Acceptance {
        accept_terms: true,
        attest_data_use_agreement: true,
        via: "cli",
    }
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn tor_zip() -> Vec<u8> {
    zip_bytes(&[
        ("Test Conn/dataset_info.json", b"{\"tag\":\"T\"}".as_slice()),
        ("Test Conn/vol/a.mat", &[9u8; 4000]),
    ])
}

fn fetch(
    e: &ConnectomeEntry,
    roots: &ConnectomeRoots,
    s: &FetchSettings,
    a: &Acceptance,
) -> Result<FetchOutcome, ConnectomeError> {
    fetch_connectome(e, roots, a, s, &no_cancel(), &mut |_| {})
}

fn roots_in(d: &Path) -> ConnectomeRoots {
    ConnectomeRoots::from_primary(d.join("connectomes"))
}

fn part_files(roots: &ConnectomeRoots, e: &ConnectomeEntry) -> Vec<PathBuf> {
    std::fs::read_dir(partial_dir(&roots.primary, e))
        .map(|rd| {
            rd.flatten()
                .map(|x| x.path())
                .filter(|p| p.extension().is_some_and(|x| x == "part"))
                .collect()
        })
        .unwrap_or_default()
}

fn env_of(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<&str, &str> = pairs.iter().copied().collect();
    move |k| map.get(k).map(|v| (*v).to_owned())
}

#[test]
fn roots_precedence_flag_then_env_then_default() {
    let data = Some(PathBuf::from("data"));
    let env = env_of(&[
        (CONNECTOMES_DIR_ENV, " env_primary "),
        (LEADDBS_CONNECTOMES_ENV, "env_ext"),
    ]);
    let r = ConnectomeRoots::resolve_with(Some(Path::new("flag_root")), &env, data.clone(), None)
        .unwrap();
    assert!(r.primary.ends_with("flag_root"));
    assert!(r.external.as_ref().unwrap().ends_with("env_ext"));
    let r = ConnectomeRoots::resolve_with(None, &env, data.clone(), None).unwrap();
    assert!(r.primary.ends_with("env_primary"));
    let blank = env_of(&[(CONNECTOMES_DIR_ENV, "  "), (LEADDBS_CONNECTOMES_ENV, "")]);
    let r = ConnectomeRoots::resolve_with(None, &blank, data, None).unwrap();
    assert!(r.primary.ends_with(Path::new("data").join("connectomes")));
    assert!(r.external.is_none());
    let e = ConnectomeRoots::resolve_with(None, &blank, None, None).unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage);
}

#[test]
fn the_external_root_setting_wins_over_the_environment() {
    let env = env_of(&[(LEADDBS_CONNECTOMES_ENV, "env_ext")]);
    let data = Some(PathBuf::from("data"));
    let setting = Path::new("setting_ext");
    let r = ConnectomeRoots::resolve_with(None, &env, data.clone(), Some(setting)).unwrap();
    assert!(r.external.as_ref().unwrap().ends_with("setting_ext"));
    let r = ConnectomeRoots::resolve_with(None, &env, data.clone(), Some(Path::new(""))).unwrap();
    assert!(r.external.as_ref().unwrap().ends_with("env_ext"));
    let r = ConnectomeRoots::resolve_with(Some(Path::new("flag_root")), &env, data, Some(setting))
        .unwrap();
    assert!(r.primary.ends_with("flag_root"));
    assert!(r.external.as_ref().unwrap().ends_with("setting_ext"));
}

static SETTING_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn resolve_uses_the_setting_given_to_the_pipeline() {
    let _guard = SETTING_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let primary = std::env::temp_dir().join("dbsloc_setting_primary");
    set_external_root_setting(Some(PathBuf::from("pipeline_setting_ext")));
    let with = ConnectomeRoots::resolve(Some(&primary));
    set_external_root_setting(Some(PathBuf::new()));
    let cleared = ConnectomeRoots::resolve(Some(&primary));
    set_external_root_setting(None);
    let r = with.unwrap();
    assert!(r
        .external
        .as_ref()
        .unwrap()
        .ends_with("pipeline_setting_ext"));
    assert_eq!(
        cleared.unwrap().external,
        ConnectomeRoots::resolve(Some(&primary)).unwrap().external
    );
}

#[test]
fn discovery_orders_roots_and_marks_duplicates() {
    let d = tempfile::tempdir().unwrap();
    let primary = d.path().join("p");
    let ext = d.path().join("e");
    let put = |p: PathBuf, bytes: &[u8]| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    };
    put(primary.join("dMRI/Alpha/data.mat"), b"x");
    put(primary.join("dMRI/.partial-hidden/data.mat"), b"x");
    std::fs::create_dir_all(primary.join("dMRI/Empty")).unwrap();
    put(primary.join("dMRI_MultiTract/Multi/tract_ADJ.mat"), b"x");
    let mut info = br#"{"name":"G","tag":"GSP1000v2","type":"fMRI_timecourses","subsets":[{"name":"Full Set","subs":[1]}],"citation":{"short":"Caf"#.to_vec();
    info.push(0xe9);
    info.extend_from_slice(br#""}}"#);
    put(primary.join("fMRI/Renamed GSP/dataset_info.json"), &info);
    put(ext.join("dMRI/Alpha/data.mat"), b"y");
    put(ext.join("dMRI/MGH-USC HCP 32 (Horn 2017)/data.trk"), b"y");
    let elsewhere = d.path().join("elsewhere/Mine");
    put(elsewhere.join("data.mat"), b"z");
    let roots = ConnectomeRoots {
        primary: primary.clone(),
        external: Some(ext),
    };
    register_folder(
        &primary,
        RegistryEntry {
            path: elsewhere.display().to_string(),
            class: "dMRI".into(),
            id: "mine".into(),
            registered_at: "t".into(),
            space: None,
        },
    )
    .unwrap();
    register_folder(
        &primary,
        RegistryEntry {
            path: d.path().join("gone").display().to_string(),
            class: "dMRI".into(),
            id: "gone".into(),
            registered_at: "t".into(),
            space: Some("Other".into()),
        },
    )
    .unwrap();
    let found = discover_connectomes(&roots);
    let names: Vec<(&str, &str)> = found
        .iter()
        .map(|c| (c.name.as_str(), c.root_kind.label()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("Alpha", "primary"),
            ("Renamed GSP", "primary"),
            ("Multi", "primary"),
            ("Mine", "registered"),
            ("gone", "registered"),
            ("Alpha", "external"),
            ("MGH-USC HCP 32 (Horn 2017)", "external"),
        ]
    );
    let by = |n: &str, k: RootKind| {
        found
            .iter()
            .find(|c| c.name == n && c.root_kind == k)
            .unwrap()
    };
    assert!(by("Alpha", RootKind::Primary).duplicate_of.is_none());
    assert_eq!(
        by("Alpha", RootKind::External).duplicate_of,
        Some(by("Alpha", RootKind::Primary).path.clone())
    );
    let g = by("Renamed GSP", RootKind::Primary);
    assert_eq!(g.catalogue_id, Some("cohen2023-gsp1000v2"));
    assert_eq!(g.tag.as_deref(), Some("GSP1000v2"));
    assert_eq!(g.subsets, vec!["Full Set"]);
    assert_eq!(g.class, ConnectomeClass::Fmri);
    assert_eq!(
        by("MGH-USC HCP 32 (Horn 2017)", RootKind::External).catalogue_id,
        Some("horn2017-mghusc-hcp32")
    );
    assert!(by("gone", RootKind::Registered).missing);
    assert_eq!(by("Mine", RootKind::Registered).id, "Mine");
    let rows = list_catalogue(&roots, &found);
    let gsp = rows
        .iter()
        .find(|r| r.entry.id == "cohen2023-gsp1000v2")
        .unwrap();
    assert_eq!(gsp.state.label(), "installed");
    let other = rows
        .iter()
        .find(|r| r.entry.id == "loh2020-torpd75")
        .unwrap();
    assert!(matches!(other.state, ConnectomeState::Available { size } if size > 0));
}

#[test]
fn registry_round_trip_and_forget() {
    let d = tempfile::tempdir().unwrap();
    let folder = d.path().join("F");
    std::fs::create_dir_all(&folder).unwrap();
    let entry = |p: &Path| RegistryEntry {
        path: p.display().to_string(),
        class: "dMRI".into(),
        id: "f".into(),
        registered_at: "t".into(),
        space: None,
    };
    register_folder(d.path(), entry(&folder)).unwrap();
    register_folder(d.path(), entry(&folder)).unwrap();
    assert_eq!(read_registry(d.path()).unwrap().len(), 1);
    forget_folder(d.path(), &folder).unwrap();
    assert!(read_registry(d.path()).unwrap().is_empty());
    assert_eq!(
        forget_folder(d.path(), &folder).unwrap_err().code,
        ErrorCode::Missing
    );
    assert!(folder.is_dir());
    std::fs::write(d.path().join(REGISTRY_FILE), "nope").unwrap();
    assert_eq!(read_registry(d.path()).unwrap_err().code, ErrorCode::Format);
}

#[test]
fn licence_record_content_history_and_staleness() {
    let d = tempfile::tempdir().unwrap();
    let e = lookup("ewert2017-ppmi85").unwrap();
    let p = write_licence_record(d.path(), e, "cli", Some(&e.attestation_sentence())).unwrap();
    assert_eq!(p, d.path().join("licenses/ewert2017-ppmi85.json"));
    let v = read_licence_record(d.path(), e.id).unwrap();
    assert_eq!(v["id"], e.id);
    assert_eq!(v["dataset"], e.name);
    assert_eq!(v["access"], "dua");
    assert_eq!(v["accepted_via"], "cli");
    assert_eq!(v["attestation"], e.attestation_sentence());
    assert_eq!(v["licence_text_sha256"], licence_text_sha256(e));
    assert_eq!(v["licence_url"], e.licence_url);
    assert_eq!(v["terms_fetched"], e.terms_fetched);
    assert!(v["accepted_at"].as_str().unwrap().ends_with('Z'));
    assert!(!licence_record_is_stale(&v, e));
    let mut old = v.clone();
    old["licence_text_sha256"] = json!("0");
    assert!(licence_record_is_stale(&old, e));
    assert_eq!(
        v["licence_text_sha256"].as_str().unwrap().len(),
        64,
        "a SHA-256 hex digest"
    );
    write_licence_record(d.path(), e, "gui", None).unwrap();
    let kept: Vec<String> = std::fs::read_dir(d.path().join("licenses"))
        .unwrap()
        .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert_eq!(
        read_licence_record(d.path(), e.id).unwrap()["attestation"],
        Value::Null
    );
}

#[test]
fn acceptance_rules() {
    let open = lookup("loh2020-torpd75").unwrap();
    let dua = lookup("horn2017-mghusc-hcp32").unwrap();
    let none = Acceptance {
        accept_terms: false,
        attest_data_use_agreement: true,
        via: "cli",
    };
    assert_eq!(
        check_acceptance(open, &none).unwrap_err().code,
        ErrorCode::Terms
    );
    let terms_only = Acceptance {
        accept_terms: true,
        attest_data_use_agreement: false,
        via: "cli",
    };
    assert!(check_acceptance(open, &terms_only).is_ok());
    assert_eq!(
        check_acceptance(dua, &terms_only).unwrap_err().code,
        ErrorCode::Terms
    );
    assert!(check_acceptance(dua, &accept()).is_ok());
}

#[test]
fn refused_fetches_make_no_request_and_write_no_record() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let e = zip_entry("t-terms", &data, "Test Conn", false);
    let no = Acceptance {
        accept_terms: false,
        attest_data_use_agreement: false,
        via: "cli",
    };
    assert_eq!(
        fetch(&e, &roots, &s, &no).unwrap_err().code,
        ErrorCode::Terms
    );
    let mut dua = zip_entry("t-dua", &data, "Test Conn", false);
    dua.access = Access::Dua;
    let terms_only = Acceptance {
        accept_terms: true,
        attest_data_use_agreement: false,
        via: "cli",
    };
    assert_eq!(
        fetch(&dua, &roots, &s, &terms_only).unwrap_err().code,
        ErrorCode::Terms
    );
    assert!(server.requests().is_empty());
    assert!(!roots.primary.join("licenses").exists());
}

#[test]
fn disk_check_happens_before_any_request() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let mut s = settings_for(&server);
    s.free_space_override = Some(10);
    let e = zip_entry("t-disk", &data, "Test Conn", false);
    let err = fetch(&e, &roots, &s, &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Disk);
    assert!(
        err.message
            .contains("not enough free disk space for t-disk"),
        "{err}"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn fetch_resumes_with_range_verifies_installs_and_records() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, true, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let e = zip_entry("t-resume", &data, "Test Conn", true);
    let mut events = Vec::new();
    let out = fetch_connectome(&e, &roots, &accept(), &s, &no_cancel(), &mut |ev| {
        events.push(ev)
    })
    .unwrap();
    assert_eq!(out.path, roots.primary.join("fMRI/Test Conn"));
    assert!(out.path.join("vol/a.mat").is_file());
    let reqs = server.requests();
    let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "/release?id=t-resume-lead",
            "/files/pack.zip",
            "/files/pack.zip"
        ]
    );
    assert!(reqs.iter().all(|r| r.headers["user-agent"] == user_agent()));
    assert!(!reqs[0].headers.contains_key("range"));
    assert!(!reqs[1].headers.contains_key("range"));
    let half = data.len() / 2;
    assert_eq!(reqs[2].headers["range"], format!("bytes={half}-"));
    assert_eq!(reqs[2].headers["if-range"], "\"v1\"");
    assert!(events
        .iter()
        .any(|ev| matches!(ev, ConnectomeEvent::Retry { .. })));
    assert!(events.contains(&ConnectomeEvent::Verifying));
    let rec: Value =
        serde_json::from_str(&std::fs::read_to_string(out.path.join(INSTALL_RECORD)).unwrap())
            .unwrap();
    assert_eq!(rec["id"], "t-resume");
    assert_eq!(rec["files"][0]["sha256"], crate::sha256::sha256_hex(&data));
    assert_eq!(
        rec["files"][0]["source_digest"],
        format!("sha1:{}", sha1_hex(&data))
    );
    assert_eq!(out.files[0].sha256, crate::sha256::sha256_hex(&data));
    let lic = read_licence_record(&roots.primary, "t-resume").unwrap();
    assert_eq!(lic["accepted_via"], "cli");
    assert!(out.licence_record.is_file());
    assert!(!partial_dir(&roots.primary, &e).exists());
    assert!(!roots.primary.join(LOCK_FILE).exists());
}

#[test]
fn ignored_range_restarts_from_zero() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), false, true, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let e = zip_entry("t-norange", &data, "Test Conn", true);
    let out = fetch(&e, &roots, &s, &accept()).unwrap();
    assert!(out.path.join("dataset_info.json").is_file());
    let reqs = server.requests();
    assert!(reqs[2].headers.contains_key("range"));
}

#[test]
fn wrong_digest_fails_with_both_values_and_removes_the_partial() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, Some(String::new())));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let mut e = zip_entry("t-bad", &data, "Test Conn", true);
    let wrong = "1".repeat(40);
    e.files = Box::leak(
        vec![ConnectomeFile {
            source_digest: leak_str(format!("sha1:{wrong}")),
            ..e.files[0]
        }]
        .into_boxed_slice(),
    );
    let err = fetch(&e, &roots, &s, &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Checksum);
    assert!(
        err.message.contains(&wrong) && err.message.contains(&sha1_hex(&data)),
        "{err}"
    );
    assert!(part_files(&roots, &e).is_empty());
    assert!(!roots.primary.join("fMRI/Test Conn").exists());
}

#[test]
fn pinned_sha256_mismatch_is_a_checksum_error() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let mut e = zip_entry("t-sha256", &data, "Test Conn", true);
    e.files = Box::leak(
        vec![ConnectomeFile {
            sha256: leak_str("0".repeat(64)),
            ..e.files[0]
        }]
        .into_boxed_slice(),
    );
    let err = fetch(&e, &roots, &s, &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Checksum);
    assert!(err.message.contains("SHA-256"), "{err}");
}

#[test]
fn changed_server_checksum_stops_before_writing() {
    let data = tor_zip();
    let server = Server::start(lead_handler(
        data.clone(),
        true,
        false,
        Some(format!("SHA1:{}", "2".repeat(40))),
    ));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let e = zip_entry("t-changed", &data, "Test Conn", true);
    let err = fetch(&e, &roots, &s, &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Changed);
    assert!(err.message.contains("update Wyss DBS Localizer"), "{err}");
    assert!(part_files(&roots, &e).is_empty());
}

#[test]
fn changed_length_is_a_changed_error() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let mut e = zip_entry("t-len", &data, "Test Conn", true);
    e.files = Box::leak(
        vec![ConnectomeFile {
            bytes: data.len() as u64 + 5,
            ..e.files[0]
        }]
        .into_boxed_slice(),
    );
    assert_eq!(
        fetch(&e, &roots, &s, &accept()).unwrap_err().code,
        ErrorCode::Changed
    );
}

#[test]
fn retry_after_is_honoured_once() {
    let data = tor_zip();
    let inner = lead_handler(data.clone(), true, false, None);
    let hits = Arc::new(Mutex::new(0usize));
    let server = Server::start(move |req, i| {
        if req.path.starts_with("/files") {
            let mut h = hits.lock().unwrap();
            *h += 1;
            if *h == 1 {
                return Resp::new(429, Vec::new()).header("Retry-After", "1");
            }
        }
        inner(req, i)
    });
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let e = zip_entry("t-429", &data, "Test Conn", true);
    let started = Instant::now();
    let mut waits = Vec::new();
    fetch_connectome(&e, &roots, &accept(), &s, &no_cancel(), &mut |ev| {
        if let ConnectomeEvent::Retry { wait_secs, .. } = ev {
            waits.push(wait_secs);
        }
    })
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(waits, vec![1]);
}

#[test]
fn forbidden_is_not_retried() {
    let server = Server::start(|req, _| {
        if req.path.starts_with("/release") {
            Resp::new(302, Vec::new()).header("Location", "/files/pack.zip")
        } else {
            Resp::new(403, Vec::new())
        }
    });
    let data = tor_zip();
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let e = zip_entry("t-403", &data, "Test Conn", true);
    let err = fetch(&e, &roots, &s, &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Host);
    assert!(err.message.contains("403"), "{err}");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn gone_file_is_re_resolved_once() {
    let data = tor_zip();
    let inner = lead_handler(data.clone(), true, false, None);
    let hits = Arc::new(Mutex::new(0usize));
    let server = Server::start(move |req, i| {
        if req.path.starts_with("/files") {
            let mut h = hits.lock().unwrap();
            *h += 1;
            if *h == 1 {
                return Resp::new(404, Vec::new());
            }
        }
        inner(req, i)
    });
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let e = zip_entry("t-404", &data, "Test Conn", true);
    fetch(&e, &roots, &s, &accept()).unwrap();
    let n_release = server
        .requests()
        .iter()
        .filter(|r| r.path.starts_with("/release"))
        .count();
    assert_eq!(n_release, 2);
}

#[test]
fn unreachable_host_keeps_nothing_and_reports_e_conn_host() {
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}", l.local_addr().unwrap().port())
    };
    let data = tor_zip();
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = FetchSettings {
        leaddbs_release_url: format!("{dead}/release"),
        retry_delays: vec![Duration::from_millis(5)],
        disk_slack_bytes: 0,
        ..FetchSettings::default()
    };
    let e = zip_entry("t-dead", &data, "Test Conn", true);
    let err = fetch(&e, &roots, &s, &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Host);
    assert!(err.message.starts_with("cannot reach 127.0.0.1"), "{err}");
}

#[test]
fn cancel_keeps_the_partial_and_a_later_fetch_resumes_it() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, true, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let mut s = settings_for(&server);
    s.retry_delays = vec![Duration::from_secs(30)];
    let e = zip_entry("t-cancel", &data, "Test Conn", true);
    let cancel = AtomicBool::new(false);
    let err = fetch_connectome(&e, &roots, &accept(), &s, &cancel, &mut |ev| {
        if matches!(ev, ConnectomeEvent::Retry { .. }) {
            cancel.store(true, Ordering::SeqCst);
        }
    })
    .unwrap_err();
    assert!(err.is_cancelled());
    let parts = part_files(&roots, &e);
    assert_eq!(parts.len(), 1);
    assert_eq!(
        std::fs::metadata(&parts[0]).unwrap().len(),
        (data.len() / 2) as u64
    );
    assert!(matches!(
        list_catalogue(&roots, &[])
            .iter()
            .find(|r| r.entry.id == "loh2020-torpd75")
            .unwrap()
            .state,
        ConnectomeState::Available { .. }
    ));
    assert!(!roots.primary.join(LOCK_FILE).exists());
    s.retry_delays = vec![Duration::from_millis(5)];
    let out = fetch(&e, &roots, &s, &accept()).unwrap();
    assert!(out.path.join("vol/a.mat").is_file());
    let last = server.requests().last().cloned().unwrap();
    assert_eq!(last.headers["range"], format!("bytes={}-", data.len() / 2));
}

#[test]
fn partial_state_reports_a_percentage() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let e = lookup("loh2020-torpd75").unwrap();
    let dir = partial_dir(&roots.primary, e);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("x.part"), vec![0u8; 1000]).unwrap();
    let row = list_catalogue(&roots, &[])
        .into_iter()
        .find(|r| r.entry.id == e.id)
        .unwrap();
    assert!(matches!(
        row.state,
        ConnectomeState::Partial { bytes: 1000, .. }
    ));
}

#[test]
fn second_fetch_is_busy_and_a_stale_lock_is_replaced() {
    let d = tempfile::tempdir().unwrap();
    let primary = d.path().join("c");
    let held = DownloadLock::acquire(&primary, "first").unwrap();
    let err = DownloadLock::acquire(&primary, "second").unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    assert!(err.message.contains("first"), "{err}");
    drop(held);
    assert!(!primary.join(LOCK_FILE).exists());
    std::fs::write(
        primary.join(LOCK_FILE),
        json!({"pid": 4_000_000_000u64, "host": hostname(), "started_at": "t", "id": "dead"})
            .to_string(),
    )
    .unwrap();
    let again = DownloadLock::acquire(&primary, "third").unwrap();
    assert!(primary.join(LOCK_FILE).is_file());
    drop(again);
    std::fs::write(
        primary.join(LOCK_FILE),
        json!({"pid": std::process::id(), "host": "other-host", "id": "remote"}).to_string(),
    )
    .unwrap();
    assert_eq!(
        DownloadLock::acquire(&primary, "x").unwrap_err().code,
        ErrorCode::Busy
    );
}

#[test]
fn an_empty_fresh_lock_counts_as_held_and_an_old_one_as_stale() {
    let d = tempfile::tempdir().unwrap();
    let primary = d.path().join("c");
    std::fs::create_dir_all(&primary).unwrap();
    let lock = primary.join(LOCK_FILE);
    std::fs::write(&lock, b"").unwrap();
    let err = DownloadLock::acquire(&primary, "x").unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    std::fs::write(&lock, b"{not json").unwrap();
    assert_eq!(
        DownloadLock::acquire(&primary, "x").unwrap_err().code,
        ErrorCode::Busy
    );
    let old = std::time::SystemTime::now() - Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let held = DownloadLock::acquire(&primary, "y").unwrap();
    drop(held);
    assert!(!lock.exists());
    std::fs::write(&lock, b"").unwrap();
    let set_mtime = |t: std::time::SystemTime| {
        std::fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(t)
            .unwrap();
    };
    set_mtime(std::time::SystemTime::now() + Duration::from_secs(5));
    assert_eq!(
        DownloadLock::acquire(&primary, "z").unwrap_err().code,
        ErrorCode::Busy
    );
    set_mtime(std::time::SystemTime::now() + Duration::from_secs(3600));
    let held = DownloadLock::acquire(&primary, "z").unwrap();
    drop(held);
    assert!(!lock.exists());
}

#[test]
fn busy_lock_blocks_a_fetch_before_any_request() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let _held = DownloadLock::acquire(&roots.primary, "other").unwrap();
    let e = zip_entry("t-busy", &data, "Test Conn", true);
    let err = fetch(&e, &roots, &settings_for(&server), &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    assert!(server.requests().is_empty());
}

#[test]
fn zip_with_traversal_is_refused_and_the_archive_kept() {
    let data = zip_bytes(&[("../escape.txt", b"x".as_slice())]);
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let e = zip_entry("t-slip", &data, "Test Conn", true);
    let err = fetch(&e, &roots, &settings_for(&server), &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Archive);
    assert!(!d.path().join("connectomes/escape.txt").exists());
    assert!(!roots.primary.join("fMRI/Test Conn").exists());
    assert_eq!(part_files(&roots, &e).len(), 1);
}

#[test]
fn verified_folder_and_marker_are_enforced_unverified_folder_is_recorded() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let s = settings_for(&server);
    let wrong = zip_entry("t-folder", &data, "Other Name", true);
    assert_eq!(
        fetch(&wrong, &roots, &s, &accept()).unwrap_err().code,
        ErrorCode::Archive
    );
    let free = zip_entry("t-free", &data, "Other Name", false);
    let out = fetch(&free, &roots, &s, &accept()).unwrap();
    assert_eq!(out.folder, "Test Conn");
    assert!(out.path.ends_with("Test Conn"));
    let nomarker = zip_bytes(&[("Top/other.txt", b"x".as_slice())]);
    let server2 = Server::start(lead_handler(nomarker.clone(), true, false, None));
    let e = zip_entry("t-marker", &nomarker, "Top", true);
    let err = fetch(&e, &roots, &settings_for(&server2), &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Archive);
    assert!(err.message.contains("dataset_info.json"), "{err}");
}

#[test]
fn variant_folders_install_beside_the_entry_folder_and_list_separately() {
    let data = zip_bytes(&[
        ("Test Conn/dataset_info.json", b"{}".as_slice()),
        ("Test Conn/vol/a.mat", &[9u8; 4000]),
        ("Test Conn B/dataset_info.json", b"{}".as_slice()),
        ("Test Conn C/other.txt", b"x".as_slice()),
    ]);
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let e = zip_entry("loh2020-torpd75", &data, "Test Conn", true);
    let out = fetch(&e, &roots, &settings_for(&server), &accept()).unwrap();
    assert_eq!(out.folder, "Test Conn");
    let class_dir = roots.primary.join("fMRI");
    assert!(class_dir.join("Test Conn/dataset_info.json").is_file());
    assert!(class_dir.join("Test Conn B/dataset_info.json").is_file());
    assert!(!class_dir.join("Test Conn C").exists());
    let entry_rec = read_install_record(&class_dir.join("Test Conn")).unwrap();
    assert!(entry_rec.get("variant_of").is_none());
    let variant_rec = read_install_record(&class_dir.join("Test Conn B")).unwrap();
    assert_eq!(variant_rec["variant_of"], "loh2020-torpd75");
    assert_eq!(variant_rec["name"], "Test Conn B");
    let left: Vec<String> = std::fs::read_dir(&class_dir)
        .unwrap()
        .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
    let found = discover_connectomes(&roots);
    assert_eq!(found.len(), 2);
    let variant = found.iter().find(|c| c.name == "Test Conn B").unwrap();
    assert_eq!(variant.catalogue_id, None);
    assert_eq!(
        found
            .iter()
            .filter(|c| c.catalogue_id == Some("loh2020-torpd75"))
            .count(),
        1
    );
    let rows = list_catalogue(&roots, &found);
    let row = rows
        .iter()
        .find(|r| r.entry.id == "loh2020-torpd75")
        .unwrap();
    assert_eq!(
        row.state,
        ConnectomeState::Installed(class_dir.join("Test Conn"))
    );
}

#[test]
fn two_unrelated_folders_in_a_zip_are_refused_for_an_unverified_entry() {
    let data = zip_bytes(&[
        ("One/dataset_info.json", b"{}".as_slice()),
        ("Two/dataset_info.json", b"{}".as_slice()),
    ]);
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let e = zip_entry("t-two", &data, "Test Conn", false);
    let err = fetch(&e, &roots, &settings_for(&server), &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Archive);
    assert!(err.message.contains("Test Conn"), "{err}");
    assert!(!roots.primary.join("fMRI/One").exists());
}

#[test]
fn an_existing_install_is_replaced() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let old = roots.primary.join("fMRI/Test Conn");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("stale.txt"), b"old").unwrap();
    let e = zip_entry("t-replace", &data, "Test Conn", true);
    fetch(&e, &roots, &settings_for(&server), &accept()).unwrap();
    assert!(!old.join("stale.txt").exists());
    assert!(old.join("dataset_info.json").is_file());
    let left: Vec<String> = std::fs::read_dir(roots.primary.join("fMRI"))
        .unwrap()
        .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["Test Conn"]);
}

fn gsp_entry(id: &str, files: Vec<(&'static str, Vec<u8>, u64)>) -> ConnectomeEntry {
    let mut e = lookup("cohen2023-gsp1000v2").unwrap().clone();
    e.id = leak_str(id.to_owned());
    e.files = Box::leak(
        files
            .iter()
            .map(|(name, data, fid)| ConnectomeFile {
                source: Source::Dataverse { file_id: *fid },
                file_name: name,
                bytes: data.len() as u64,
                source_digest: leak_str(format!("md5:{}", md5_hex(data))),
                sha256: "",
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    e.install = Install::GspTars { folder: "GSP Test" };
    e
}

fn dataverse_handler(
    files: Vec<(u64, Vec<u8>, &'static str)>,
) -> impl Fn(&Req, usize) -> Resp + Send + Sync {
    move |req, _| {
        if let Some(id) = req.path.strip_prefix("/api/access/datafile/") {
            let id: u64 = id.parse().unwrap();
            let name = files.iter().find(|f| f.0 == id).unwrap().2;
            return Resp::new(303, Vec::new()).header("Location", &format!("/blob/{name}"));
        }
        let name = req.path.strip_prefix("/blob/").unwrap();
        let data = &files.iter().find(|f| f.2 == name).unwrap().1;
        let n = req
            .headers
            .get("range")
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.strip_suffix('-'))
            .and_then(|v| v.parse::<usize>().ok());
        match n {
            Some(n) if n > 0 => Resp::new(206, data[n..].to_vec()).header(
                "Content-Range",
                &format!("bytes {n}-{}/{}", data.len() - 1, data.len()),
            ),
            _ => Resp::new(200, data.clone()),
        }
    }
}

#[test]
fn gsp_tars_are_verified_extracted_and_installed() {
    let info = br#"{"name":"GSP Test","tag":"T"}"#.to_vec();
    let tar = tar_bytes(&[
        ("GSP1000.txt", b"list".as_slice(), b'0'),
        ("sub-0001_run_1.mat", &[5u8; 700], b'0'),
        ("sub-0002_run_1.mat", &[6u8; 10], b'0'),
    ]);
    let e = gsp_entry(
        "t-gsp",
        vec![
            ("dataset_info.json", info.clone(), 11),
            ("GSP1000_leadDBS_00.tar", tar.clone(), 22),
        ],
    );
    let server = Server::start(dataverse_handler(vec![
        (11, info.clone(), "dataset_info.json"),
        (22, tar.clone(), "GSP1000_leadDBS_00.tar"),
    ]));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let mut counter = Vec::new();
    let out = fetch_connectome(
        &e,
        &roots,
        &accept(),
        &settings_for(&server),
        &no_cancel(),
        &mut |ev| {
            if let ConnectomeEvent::FileStarted { index, count, .. } = ev {
                counter.push((index, count));
            }
        },
    )
    .unwrap();
    assert_eq!(counter, vec![(1, 2), (2, 2)]);
    assert_eq!(out.path, roots.primary.join("fMRI/GSP Test"));
    assert_eq!(
        std::fs::read(out.path.join("GSP1000.txt")).unwrap(),
        b"list"
    );
    assert_eq!(
        std::fs::read(out.path.join("dataset_info.json")).unwrap(),
        info
    );
    assert_eq!(
        std::fs::read(out.path.join("vol/sub-0001_run_1.mat"))
            .unwrap()
            .len(),
        700
    );
    assert!(out.path.join("vol/sub-0002_run_1.mat").is_file());
    assert!(out.path.join(INSTALL_RECORD).is_file());
    assert_eq!(out.files.len(), 2);
    assert!(!partial_dir(&roots.primary, &e).exists());
    let paths: Vec<String> = server.requests().iter().map(|r| r.path.clone()).collect();
    assert!(paths.contains(&"/api/access/datafile/22".to_owned()));
}

#[test]
fn gsp_refuses_unexpected_members_and_wrong_md5() {
    let info = br#"{"name":"x"}"#.to_vec();
    let evil = tar_bytes(&[("evil.sh", b"x".as_slice(), b'0')]);
    let e = gsp_entry(
        "t-gsp-evil",
        vec![
            ("dataset_info.json", info.clone(), 11),
            ("GSP1000_leadDBS_00.tar", evil.clone(), 22),
        ],
    );
    let server = Server::start(dataverse_handler(vec![
        (11, info.clone(), "dataset_info.json"),
        (22, evil.clone(), "GSP1000_leadDBS_00.tar"),
    ]));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let err = fetch(&e, &roots, &settings_for(&server), &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Archive);
    assert!(!roots.primary.join("fMRI/GSP Test").exists());
    let mut bad = gsp_entry("t-gsp-md5", vec![("dataset_info.json", info.clone(), 11)]);
    bad.files = Box::leak(
        vec![ConnectomeFile {
            source_digest: leak_str(format!("md5:{}", "3".repeat(32))),
            ..bad.files[0]
        }]
        .into_boxed_slice(),
    );
    let err = fetch(&bad, &roots, &settings_for(&server), &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Checksum);
    assert!(err.message.contains("MD5"), "{err}");
}

#[test]
fn gsp_run_names() {
    assert!(is_gsp_run_name("sub-0001_run_1.mat"));
    assert!(is_gsp_run_name("sub-0999_run_12.mat"));
    for bad in [
        "sub-001_run_1.mat",
        "sub-0001_run_.mat",
        "sub-0001_run_1.txt",
        "../sub-0001_run_1.mat",
        "x",
    ] {
        assert!(!is_gsp_run_name(bad), "{bad}");
    }
}

#[test]
fn import_registers_a_folder_in_place() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let folder = d.path().join("MyConn");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("data.mat"), b"placeholder").unwrap();
    let out = import_folder(&roots, &folder, None, None).unwrap();
    assert_eq!(out.class, ConnectomeClass::Dmri);
    assert!(out.registered && !out.extracted);
    assert_eq!(out.id, "MyConn");
    assert!(folder.join("data.mat").is_file());
    assert!(!roots.primary.join("dMRI").exists());
    let found = discover_connectomes(&roots);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].root_kind, RootKind::Registered);
    let err = import_folder(&roots, &folder, Some(ConnectomeClass::Fmri), None).unwrap_err();
    assert_eq!(err.code, ErrorCode::Format);
    let empty = d.path().join("Empty");
    std::fs::create_dir_all(empty.join("dMRI")).unwrap();
    let err = import_folder(&roots, &empty, None, None).unwrap_err();
    assert!(err.message.contains("connectome root"), "{err}");
    let multi = d.path().join("Multi");
    std::fs::create_dir_all(&multi).unwrap();
    std::fs::write(multi.join("a.mat"), b"x").unwrap();
    assert_eq!(
        import_folder(&roots, &multi, None, None).unwrap_err().code,
        ErrorCode::Format
    );
    let out = import_folder(&roots, &multi, Some(ConnectomeClass::DmriMultiTract), None).unwrap();
    assert_eq!(out.class, ConnectomeClass::DmriMultiTract);
}

#[test]
fn import_of_a_restricted_folder_records_licence_only_with_acceptance() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let folder = d.path().join("MGH-USC HCP 32 (Horn 2017)");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("data.mat"), b"x").unwrap();
    let out = import_folder(&roots, &folder, None, None).unwrap();
    assert_eq!(out.catalogue_id, Some("horn2017-mghusc-hcp32"));
    assert!(out.licence_record.is_none());
    assert!(out
        .licence_note
        .as_deref()
        .unwrap()
        .contains("--attest-data-use-agreement"));
    let terms_only = Acceptance {
        accept_terms: true,
        attest_data_use_agreement: false,
        via: "import",
    };
    assert!(import_folder(&roots, &folder, None, Some(&terms_only))
        .unwrap()
        .licence_record
        .is_none());
    let full = Acceptance {
        accept_terms: true,
        attest_data_use_agreement: true,
        via: "import",
    };
    let out = import_folder(&roots, &folder, None, Some(&full)).unwrap();
    assert!(out.licence_record.is_some());
    let v = read_licence_record(&roots.primary, "horn2017-mghusc-hcp32").unwrap();
    assert_eq!(v["accepted_via"], "import");
    assert!(v["attestation"]
        .as_str()
        .unwrap()
        .contains("data use agreement"));
}

#[test]
fn import_extracts_zip_and_tar_gz_into_the_primary_root() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let zip = d.path().join("pack one.zip");
    std::fs::write(
        &zip,
        zip_bytes(&[
            ("Inner/data.mat", b"x".as_slice()),
            ("Inner/more.txt", b"y"),
        ]),
    )
    .unwrap();
    let out = import_archive(&roots, &zip, None, None, &no_cancel()).unwrap();
    assert!(out.extracted && !out.registered);
    assert_eq!(out.path, roots.primary.join("dMRI/Inner"));
    assert!(out.path.join(INSTALL_RECORD).is_file());
    let flat = d.path().join("flat.zip");
    std::fs::write(&flat, zip_bytes(&[("dataset_info.json", b"{}".as_slice())])).unwrap();
    let out = import_archive(&roots, &flat, None, None, &no_cancel()).unwrap();
    assert_eq!(out.class, ConnectomeClass::Fmri);
    assert_eq!(out.path, roots.primary.join("fMRI/flat"));
    let tgz = d.path().join("t.tar.gz");
    let tar = tar_bytes(&[
        ("Tarred/", b"".as_slice(), b'5'),
        ("Tarred/data.mat", b"z", b'0'),
    ]);
    let mut enc = flate2::write::GzEncoder::new(
        std::fs::File::create(&tgz).unwrap(),
        flate2::Compression::default(),
    );
    enc.write_all(&tar).unwrap();
    enc.finish().unwrap();
    let out = import_archive(&roots, &tgz, None, None, &no_cancel()).unwrap();
    assert_eq!(out.path, roots.primary.join("dMRI/Tarred"));
    let left: Vec<String> = std::fs::read_dir(&roots.primary)
        .unwrap()
        .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(PARTIAL))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let bad = d.path().join("slip.zip");
    std::fs::write(&bad, zip_bytes(&[("../x", b"x".as_slice())])).unwrap();
    assert_eq!(
        import_archive(&roots, &bad, None, None, &no_cancel())
            .unwrap_err()
            .code,
        ErrorCode::Archive
    );
    let txt = d.path().join("a.rar");
    std::fs::write(&txt, b"x").unwrap();
    assert_eq!(
        import_archive(&roots, &txt, None, None, &no_cancel())
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
}

#[test]
fn old_partials_are_swept() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("dMRI/.partial-x");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.part"), b"x").unwrap();
    sweep_old_partials(d.path(), Duration::from_secs(3600));
    assert!(dir.exists());
    sweep_old_partials(d.path(), Duration::ZERO);
    assert!(!dir.exists());
}

#[test]
fn required_space_follows_the_install_kind() {
    let zip = lookup("horn2013-gibbs19").unwrap();
    let bytes = zip.total_bytes();
    assert_eq!(required_free_bytes(zip, 7), bytes + bytes + bytes / 50 + 7);
    let gsp = lookup("cohen2023-gsp1000v2").unwrap();
    assert_eq!(required_free_bytes(gsp, 0), 218_932_842_997 + 6_271_344_640);
}

#[test]
fn dataverse_metadata_comparison() {
    let e = lookup("cohen2023-gsp1000v2").unwrap();
    let files: Vec<Value> = e
        .files
        .iter()
        .map(|f| {
            let Source::Dataverse { file_id } = f.source else { unreachable!() };
            json!({"dataFile": {"id": file_id, "filesize": f.bytes, "md5": f.source_digest.strip_prefix("md5:").unwrap()}})
        })
        .collect();
    let mut meta = json!({"data": {"latestVersion": {"files": files}}});
    let facts = compare_dataverse_metadata(e, &meta);
    assert_eq!(facts.len(), 38);
    assert!(facts.iter().all(|f| f.matches));
    meta["data"]["latestVersion"]["files"][3]["dataFile"]["md5"] = json!("0");
    let facts = compare_dataverse_metadata(e, &meta);
    assert_eq!(facts.iter().filter(|f| !f.matches).count(), 1);
}

#[test]
fn remote_probe_heads_the_resolved_file() {
    let data = tor_zip();
    let server = Server::start(lead_handler(data.clone(), true, false, None));
    let e = zip_entry("t-probe", &data, "Test Conn", true);
    let facts = probe_remote(&e, &settings_for(&server)).unwrap();
    assert_eq!(facts.len(), 1);
    assert!(facts[0].matches, "{facts:?}");
    let methods: Vec<String> = server.requests().iter().map(|r| r.method.clone()).collect();
    assert_eq!(methods, vec!["GET", "HEAD"]);
}

#[test]
fn user_agent_has_the_product_string() {
    let ua = user_agent();
    assert!(
        ua.starts_with(&format!("dbsloc/{VERSION} (Wyss DBS Localizer")),
        "{ua}"
    );
    assert!(ua.ends_with("research software)"));
}

fn figshare_entry(id: &str, data: &[u8], md5: &str) -> ConnectomeEntry {
    let mut e = zip_entry(id, data, "Test Conn", true);
    e.files = Box::leak(
        vec![ConnectomeFile {
            source: Source::Figshare { file_id: 4242 },
            file_name: "pack.zip",
            bytes: data.len() as u64,
            source_digest: leak_str(format!("md5:{md5}")),
            sha256: "",
        }]
        .into_boxed_slice(),
    );
    e
}

fn figshare_handler(data: Vec<u8>) -> impl Fn(&Req, usize) -> Resp + Send + Sync {
    move |req, _| {
        if req.path.starts_with("/files/") {
            return Resp::new(302, Vec::new()).header("Location", "/storage/pack.zip");
        }
        serve_file(req, &data, true, None)
    }
}

#[test]
fn figshare_fetch_follows_the_redirect_verifies_md5_and_installs() {
    let data = tor_zip();
    let server = Server::start(figshare_handler(data.clone()));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let e = figshare_entry("t-fig", &data, &md5_hex(&data));
    let out = fetch(&e, &roots, &settings_for(&server), &accept()).unwrap();
    assert_eq!(out.path, roots.primary.join("fMRI/Test Conn"));
    assert!(out.path.join("vol/a.mat").is_file());
    let reqs = server.requests();
    let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["/files/4242", "/storage/pack.zip"]);
    assert!(reqs.iter().all(|r| r.headers["user-agent"] == user_agent()));
    assert_eq!(
        out.files[0].source_digest,
        format!("md5:{}", md5_hex(&data))
    );
    assert_eq!(out.files[0].sha256, sha256_hex(&data));
    assert!(roots.primary.join("licenses/t-fig.json").is_file());
}

#[test]
fn figshare_md5_mismatch_is_a_checksum_error_and_installs_nothing() {
    let data = tor_zip();
    let server = Server::start(figshare_handler(data.clone()));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let e = figshare_entry("t-fig-bad", &data, "00000000000000000000000000000000");
    let err = fetch(&e, &roots, &settings_for(&server), &accept()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Checksum);
    assert!(err.message.contains("MD5"), "{err}");
    assert!(!roots.primary.join("fMRI/Test Conn").exists());
    assert!(part_files(&roots, &e).is_empty());
}

#[test]
fn figshare_remote_probe_heads_the_resolved_file() {
    let data = tor_zip();
    let server = Server::start(figshare_handler(data.clone()));
    let e = figshare_entry("t-fig-probe", &data, &md5_hex(&data));
    let facts = probe_remote(&e, &settings_for(&server)).unwrap();
    assert_eq!(facts.len(), 1);
    assert!(facts[0].matches, "{facts:?}");
}

#[test]
fn a_dua_entry_is_never_fetched_even_with_both_acceptance_flags() {
    let server = Server::start(lead_handler(tor_zip(), true, false, None));
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    for id in [
        "horn2017-mghusc-hcp32",
        "ewert2017-ppmi85",
        "horn2017-ppmi74p15c",
    ] {
        let e = lookup(id).unwrap();
        let err = fetch(e, &roots, &settings_for(&server), &accept()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Terms);
        assert_eq!(
            err.message,
            format!("{id} is import-only: obtain it with your own data access and import it")
        );
    }
    assert!(server.requests().is_empty());
    assert!(!roots.primary.exists());
}

fn pin_for(name: &'static str, data: &[u8]) -> ConnectomeFile {
    ConnectomeFile {
        source: Source::LeadDbs { id: "unused" },
        file_name: name,
        bytes: data.len() as u64,
        source_digest: leak_str(format!("sha1:{}", sha1_hex(data))),
        sha256: leak_str(sha256_hex(data)),
    }
}

fn primary_entries(roots: &ConnectomeRoots) -> usize {
    std::fs::read_dir(&roots.primary).map_or(0, |rd| rd.count())
}

#[test]
fn import_of_a_known_name_with_wrong_bytes_fails_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let good = tor_zip();
    let pin = pin_for("known.zip", &good);
    let mut wrong = good.clone();
    let last = wrong.len() - 1;
    wrong[last] ^= 0xff;
    let zip = d.path().join("known.zip");
    std::fs::write(&zip, &wrong).unwrap();
    let err = import_archive_against(&roots, &zip, None, None, &no_cancel(), &[&pin]).unwrap_err();
    assert_eq!(err.code, ErrorCode::Checksum);
    assert!(err.message.contains("SHA-1"), "{err}");
    assert!(err.message.contains(" instead of "), "{err}");
    assert!(
        err.message
            .contains("is named like the catalogue file known.zip"),
        "{err}"
    );
    assert!(
        err.message.contains("but is not that file: SHA-1 "),
        "{err}"
    );
    assert!(
        err.message.ends_with(
            "If the download was interrupted, download it again. If this is your own archive, rename it and import it again; it will then be imported without verification."
        ),
        "{err}"
    );
    assert_eq!(primary_entries(&roots), 0);
    wrong.push(0);
    std::fs::write(&zip, &wrong).unwrap();
    let err = import_archive_against(&roots, &zip, None, None, &no_cancel(), &[&pin]).unwrap_err();
    assert_eq!(err.code, ErrorCode::Checksum);
    assert!(err.message.contains("bytes instead of"), "{err}");
    assert_eq!(primary_entries(&roots), 0);
}

#[test]
fn the_catalogue_name_match_ignores_case_and_browser_duplicate_counters() {
    for name in [
        "known.zip",
        "KNOWN.ZIP",
        "Known.Zip",
        "known (1).zip",
        "known(2).zip",
        "known (12).ZIP",
    ] {
        assert!(names_catalogue_file(name, "known.zip"), "{name}");
    }
    for name in [
        "known (a).zip",
        "known ().zip",
        "known (1).tar",
        "known2.zip",
        "known (1)zip",
        "mine.zip",
        "kno",
        "known (1).zip.bak",
    ] {
        assert!(!names_catalogue_file(name, "known.zip"), "{name}");
    }
    assert!(names_catalogue_file("set (3).tar.gz", "set.tar.gz"));
    assert!(names_catalogue_file("SET.TAR.GZ", "set.tar.gz"));
    assert!(!names_catalogue_file("set (3).gz", "set.tar.gz"));
}

#[test]
fn import_of_a_renamed_or_recased_catalogue_file_is_still_verified() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let good = tor_zip();
    let pin = pin_for("known.zip", &good);
    let mut wrong = good.clone();
    let last = wrong.len() - 1;
    wrong[last] ^= 0xff;
    for name in ["KNOWN.zip", "known (1).zip", "known(2).zip"] {
        let zip = d.path().join(name);
        std::fs::write(&zip, &wrong).unwrap();
        let err =
            import_archive_against(&roots, &zip, None, None, &no_cancel(), &[&pin]).unwrap_err();
        assert_eq!(err.code, ErrorCode::Checksum, "{name}: {err}");
        assert_eq!(primary_entries(&roots), 0, "{name}");
        std::fs::write(&zip, &good).unwrap();
        let out = import_archive_against(&roots, &zip, None, None, &no_cancel(), &[&pin]).unwrap();
        assert_eq!(out.verified, Some(true), "{name}");
        std::fs::remove_dir_all(&roots.primary).unwrap();
        std::fs::remove_file(&zip).unwrap();
    }
}

#[test]
fn import_of_a_known_name_with_wrong_sha256_fails() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let good = tor_zip();
    let mut pin = pin_for("known.zip", &good);
    pin.sha256 = leak_str("0".repeat(64));
    let zip = d.path().join("known.zip");
    std::fs::write(&zip, &good).unwrap();
    let err = import_archive_against(&roots, &zip, None, None, &no_cancel(), &[&pin]).unwrap_err();
    assert_eq!(err.code, ErrorCode::Checksum);
    assert!(err.message.contains("SHA-256"), "{err}");
}

#[test]
fn import_of_a_known_name_with_correct_bytes_is_verified() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let good = tor_zip();
    let pin = pin_for("known.zip", &good);
    let zip = d.path().join("known.zip");
    std::fs::write(&zip, &good).unwrap();
    let out = import_archive_against(&roots, &zip, None, None, &no_cancel(), &[&pin]).unwrap();
    assert_eq!(out.verified, Some(true));
    assert!(out.extracted);
    assert!(out.path.join("vol/a.mat").is_file());
}

#[test]
fn import_of_an_unknown_name_is_unverified_and_a_folder_has_no_verdict() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let zip = d.path().join("mine.zip");
    std::fs::write(&zip, tor_zip()).unwrap();
    let out = import_archive(&roots, &zip, None, None, &no_cancel()).unwrap();
    assert_eq!(out.verified, Some(false));
    assert_eq!(out.verification_note(), Some(UNVERIFIED_ARCHIVE_NOTE));
    assert!(UNVERIFIED_ARCHIVE_NOTE.contains("not verified"));
    let folder = d.path().join("Folder");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("data.mat"), b"x").unwrap();
    let out = import_folder(&roots, &folder, None, None).unwrap();
    assert_eq!(out.verified, None);
    assert_eq!(out.verification_note(), None);
}

#[test]
fn import_of_a_catalogue_named_archive_with_wrong_size_fails_before_extraction() {
    let d = tempfile::tempdir().unwrap();
    let roots = roots_in(d.path());
    let zip = d.path().join("Pediatric_rsfMRI.zip");
    std::fs::write(&zip, tor_zip()).unwrap();
    let err = import_archive(&roots, &zip, None, None, &no_cancel()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Checksum);
    assert_eq!(primary_entries(&roots), 0);
}

#[test]
fn licence_hash_ignores_line_ending_style() {
    let e = lookup("horn2013-gibbs19").unwrap();
    let crlf = ConnectomeEntry {
        licence_text: Box::leak(e.licence_text.replace('\n', "\r\n").into_boxed_str()),
        ..e.clone()
    };
    assert_eq!(licence_text_sha256(e), licence_text_sha256(&crlf));
}
