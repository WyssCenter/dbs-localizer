// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::sha256::{hex, Sha256};
use crate::template_catalog::{
    entry_dir, entry_t1_path_in, shipped_root, user_data_dir, TemplateEntry, TemplateSource,
};

pub(crate) const CHUNK: usize = 256 * 1024;
pub(crate) const PROGRESS_STEP: u64 = 1 << 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const BODY_TIMEOUT_FLOOR: Duration = Duration::from_secs(600);
const MIN_BODY_BYTES_PER_SEC: u64 = 50_000;
pub const CANCELLED: &str = "cancelled";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum FetchEvent {
    Started { id: String, total_bytes: u64 },
    Progress { downloaded: u64, total: u64 },
    Verifying,
    Extracting,
    Done { t1: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    Bundled(PathBuf),
    MissingBundled,
    Installed(PathBuf),
    Available { size: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntryStatus {
    pub entry: &'static TemplateEntry,
    pub state: EntryState,
}

impl EntryState {
    pub fn label(&self) -> &'static str {
        match self {
            EntryState::Bundled(_) => "bundled",
            EntryState::MissingBundled => "missing",
            EntryState::Installed(_) => "installed",
            EntryState::Available { .. } => "available",
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            EntryState::Bundled(p) | EntryState::Installed(p) => Some(p),
            _ => None,
        }
    }
}

pub fn list_entries_in(shipped: Option<&Path>, data: Option<&Path>) -> Vec<EntryStatus> {
    crate::template_catalog::catalogue()
        .iter()
        .map(|e| {
            let found = entry_t1_path_in(e, shipped, data);
            let state = match (e.source, found) {
                (TemplateSource::Shipped, Some(p)) => EntryState::Bundled(p),
                (TemplateSource::Shipped, None) => EntryState::MissingBundled,
                (TemplateSource::Download { .. }, Some(p)) => EntryState::Installed(p),
                (TemplateSource::Download { zip_size, .. }, None) => {
                    EntryState::Available { size: zip_size }
                }
            };
            EntryStatus { entry: e, state }
        })
        .collect()
}

pub fn list_entries() -> Vec<EntryStatus> {
    list_entries_in(shipped_root().as_deref(), user_data_dir().as_deref())
}

pub(crate) fn is_cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::SeqCst)
}

pub(crate) fn io_err(what: &str, e: impl std::fmt::Display) -> String {
    format!("{what}: {e}")
}

fn download_pins(e: &TemplateEntry) -> Result<(&'static str, u64), String> {
    match e.source {
        TemplateSource::Shipped => Err(format!(
            "{} is bundled with the application, nothing to download",
            e.id
        )),
        TemplateSource::Download {
            url,
            zip_sha256,
            zip_size,
            ..
        } => {
            if zip_sha256.is_empty() {
                Err(format!(
                    "{} has no pinned checksum yet, refusing to download",
                    e.id
                ))
            } else {
                Ok((url, zip_size))
            }
        }
    }
}

fn staging_dir(e: &TemplateEntry, data_root: &Path) -> PathBuf {
    let dest = entry_dir(data_root, e);
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    parent.join(format!(
        "{}{}-{}",
        crate::outputs::PARTIAL_PREFIX,
        e.id,
        std::process::id()
    ))
}

pub fn fetch_entry(
    e: &TemplateEntry,
    data_root: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(FetchEvent),
) -> Result<PathBuf, String> {
    let (url, pinned_size) = download_pins(e)?;
    let dest = entry_dir(data_root, e);
    let staging = staging_dir(e, data_root);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|x| io_err("cannot create staging directory", x))?;
    let result = download_and_install(e, url, pinned_size, &staging, &dest, cancel, on_event);
    let _ = std::fs::remove_dir_all(&staging);
    if let Ok(t1) = &result {
        on_event(FetchEvent::Done { t1: t1.clone() });
    }
    result
}

fn body_timeout(pinned_size: u64) -> Duration {
    BODY_TIMEOUT_FLOOR.max(Duration::from_secs(pinned_size / MIN_BODY_BYTES_PER_SEC))
}

fn download_agent(pinned_size: u64) -> ureq::Agent {
    download_agent_with(pinned_size, &AgentStyle::default())
}

/// Agent settings beyond the shared timeouts: the connectome downloader (`specs/15` §4.4) sets a
/// product `User-Agent`, resolves redirects itself and reads every status code.
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentStyle {
    pub user_agent: Option<String>,
    pub manual_redirects: bool,
}

pub(crate) fn download_agent_with(pinned_size: u64, style: &AgentStyle) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .timeout_resolve(Some(CONNECT_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_send_request(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_recv_body(Some(body_timeout(pinned_size)));
    if let Some(ua) = &style.user_agent {
        b = b.user_agent(ua.as_str());
    }
    if style.manual_redirects {
        b = b.max_redirects(0).http_status_as_error(false);
    }
    b.build().into()
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum StreamEvent {
    Started(u64),
    Progress(u64, u64),
}

pub(crate) fn stream_download(
    url: &str,
    pinned_size: u64,
    out_path: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Result<String, String> {
    let response = download_agent(pinned_size)
        .get(url)
        .call()
        .map_err(|x| io_err(&format!("download of {url} failed"), x))?;
    let total = response.body().content_length().unwrap_or(pinned_size);
    on_event(StreamEvent::Started(total));
    let mut reader = response
        .into_body()
        .into_with_config()
        .limit(pinned_size.saturating_add(1 << 20))
        .reader();
    let mut out = File::create(out_path).map_err(|x| io_err("cannot create download file", x))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut downloaded = 0u64;
    let mut last_report = 0u64;
    loop {
        if is_cancelled(cancel) {
            return Err(CANCELLED.to_owned());
        }
        let n = reader
            .read(&mut buf)
            .map_err(|x| io_err("download interrupted", x))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|x| io_err("cannot write download file", x))?;
        downloaded += n as u64;
        if downloaded - last_report >= PROGRESS_STEP {
            last_report = downloaded;
            on_event(StreamEvent::Progress(downloaded, total));
        }
    }
    out.flush()
        .map_err(|x| io_err("cannot write download file", x))?;
    drop(out);
    on_event(StreamEvent::Progress(downloaded, total));
    Ok(hex(&hasher.finish()))
}

fn download_and_install(
    e: &TemplateEntry,
    url: &str,
    pinned_size: u64,
    staging: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(FetchEvent),
) -> Result<PathBuf, String> {
    let zip_path = staging.join("download.zip");
    let digest = stream_download(url, pinned_size, &zip_path, cancel, &mut |ev| match ev {
        StreamEvent::Started(t) => on_event(FetchEvent::Started {
            id: e.id.to_owned(),
            total_bytes: t,
        }),
        StreamEvent::Progress(downloaded, total) => {
            on_event(FetchEvent::Progress { downloaded, total })
        }
    })?;
    install_verified(e, &zip_path, staging, dest, cancel, on_event, Some(digest))
}

fn install_verified(
    e: &TemplateEntry,
    zip_path: &Path,
    staging: &Path,
    dest_dir: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(FetchEvent),
    known_digest: Option<String>,
) -> Result<PathBuf, String> {
    let result = install_inner(
        e,
        zip_path,
        staging,
        dest_dir,
        cancel,
        on_event,
        known_digest,
    );
    let _ = std::fs::remove_dir_all(staging);
    result
}

fn install_inner(
    e: &TemplateEntry,
    zip_path: &Path,
    staging: &Path,
    dest_dir: &Path,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(FetchEvent),
    known_digest: Option<String>,
) -> Result<PathBuf, String> {
    let TemplateSource::Download {
        url,
        zip_sha256,
        zip_size,
        t1_member,
        t2_member,
        licence_member,
    } = e.source
    else {
        return Err(format!("{} is bundled, nothing to install", e.id));
    };
    if zip_sha256.is_empty() {
        return Err(format!("{} has no pinned checksum", e.id));
    }
    on_event(FetchEvent::Verifying);
    let actual_size = std::fs::metadata(zip_path)
        .map_err(|x| io_err("cannot read downloaded archive", x))?
        .len();
    if actual_size != zip_size {
        return Err(format!(
            "size mismatch for {}: expected {zip_size} bytes, got {actual_size}",
            e.id
        ));
    }
    let digest = match known_digest {
        Some(d) => d,
        None => crate::sha256::file_sha256_hex(zip_path)
            .map_err(|x| io_err("cannot hash downloaded archive", x))?,
    };
    if digest != zip_sha256 {
        return Err(format!(
            "SHA-256 mismatch for {}: expected {zip_sha256}, got {digest}",
            e.id
        ));
    }
    if is_cancelled(cancel) {
        return Err(CANCELLED.to_owned());
    }
    on_event(FetchEvent::Extracting);
    let out_dir = staging.join("out");
    std::fs::create_dir_all(&out_dir).map_err(|x| io_err("cannot create output directory", x))?;
    let file = File::open(zip_path).map_err(|x| io_err("cannot open archive", x))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|x| io_err("cannot read archive", x))?;
    extract_plain(&mut archive, t1_member, &out_dir.join("t1.nii"), cancel)?;
    extract_plain(&mut archive, t2_member, &out_dir.join("t2.nii"), cancel)?;
    extract_plain(
        &mut archive,
        licence_member,
        &out_dir.join("LICENSE"),
        cancel,
    )?;
    let meta = serde_json::json!({
        "space": e.space,
        "res_mm": e.res_mm,
        "entry_id": e.id,
        "source_url": url,
        "zip_sha256": zip_sha256,
        "t1_member": t1_member,
        "t2_member": t2_member,
        "licence_member": licence_member,
        "fetched_by": "dbsloc template fetch",
    });
    let text = serde_json::to_string_pretty(&meta).map_err(|x| io_err("template.json", x))?;
    std::fs::write(out_dir.join("template.json"), text)
        .map_err(|x| io_err("cannot write template.json", x))?;
    drop(archive);
    let _ = std::fs::remove_file(zip_path);
    if is_cancelled(cancel) {
        return Err(CANCELLED.to_owned());
    }
    replace_dir(&out_dir, dest_dir)?;
    Ok(dest_dir.join("t1.nii"))
}

fn copy_member<R: Read, W: Write>(
    name: &str,
    mut src: R,
    mut dst: W,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        if is_cancelled(cancel) {
            return Err(CANCELLED.to_owned());
        }
        let n = src
            .read(&mut buf)
            .map_err(|x| io_err(&format!("cannot read archive member {name}"), x))?;
        if n == 0 {
            return Ok(());
        }
        dst.write_all(&buf[..n])
            .map_err(|x| io_err("cannot write extracted file", x))?;
    }
}

fn extract_plain<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    member: &str,
    out: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let entry = archive
        .by_name(member)
        .map_err(|x| io_err(&format!("archive member {member} missing"), x))?;
    let mut file = File::create(out).map_err(|x| io_err("cannot create extracted file", x))?;
    copy_member(member, entry, &mut file, cancel)?;
    file.flush()
        .map_err(|x| io_err("cannot write extracted file", x))
}

pub(crate) fn replace_dir(new_dir: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|x| io_err("cannot create the destination folder", x))?;
    }
    let aside = dest.with_file_name(format!(
        "{}old-{}-{}",
        crate::outputs::PARTIAL_PREFIX,
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&aside);
    let had_old = dest.exists();
    if had_old {
        std::fs::rename(dest, &aside)
            .map_err(|x| io_err("cannot replace the installed copy", x))?;
    }
    if let Err(x) = std::fs::rename(new_dir, dest) {
        if had_old {
            let _ = std::fs::rename(&aside, dest);
        }
        return Err(io_err("cannot install the download", x));
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&aside);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::template_catalog::lookup;
    use zip::write::SimpleFileOptions;

    const T1: &[u8] = b"fake t1 bytes fake t1 bytes fake t1 bytes";
    const T2: &[u8] = b"fake t2";
    const LIC: &[u8] = b"Copyright test licence";

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_fetch_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(dir: &Path) -> PathBuf {
        let p = dir.join("src.zip");
        let f = File::create(&p).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let o = SimpleFileOptions::default();
        w.start_file("pkg/t1.nii", o).unwrap();
        w.write_all(T1).unwrap();
        w.start_file("pkg/t2.nii", o).unwrap();
        w.write_all(T2).unwrap();
        w.start_file("COPYING", o).unwrap();
        w.write_all(LIC).unwrap();
        w.start_file("unrelated/other.txt", o).unwrap();
        w.write_all(b"x").unwrap();
        w.finish().unwrap();
        p
    }

    fn test_entry(zip: &Path, sha_override: Option<&str>) -> TemplateEntry {
        let sha = crate::sha256::file_sha256_hex(zip).unwrap();
        let size = std::fs::metadata(zip).unwrap().len();
        let sha: &'static str = Box::leak(
            sha_override
                .map(str::to_owned)
                .unwrap_or(sha)
                .into_boxed_str(),
        );
        TemplateEntry {
            id: "TestSpace_res-1mm",
            space: "TestSpace",
            res_mm: 1.0,
            title: "Test",
            source: TemplateSource::Download {
                url: "https://example.invalid/t.zip",
                zip_sha256: sha,
                zip_size: size,
                t1_member: "pkg/t1.nii",
                t2_member: "pkg/t2.nii",
                licence_member: "COPYING",
            },
        }
    }

    fn prepare(root: &Path, zip: &Path) -> PathBuf {
        let staging = root.join(".partial-test");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::copy(zip, staging.join("download.zip")).unwrap();
        staging
    }

    #[test]
    fn installs_layout_and_metadata() {
        let root = scratch("ok");
        let zip = make_zip(&root);
        let e = test_entry(&zip, None);
        let staging = prepare(&root, &zip);
        let dest = root.join("data/templates/space/TestSpace/res-1mm");
        let mut events = Vec::new();
        let t1 = install_verified(
            &e,
            &staging.join("download.zip"),
            &staging,
            &dest,
            &AtomicBool::new(false),
            &mut |ev| events.push(ev),
            None,
        )
        .unwrap();
        assert_eq!(t1, dest.join("t1.nii"));
        assert_eq!(std::fs::read(&t1).unwrap(), T1);
        assert_eq!(std::fs::read(dest.join("t2.nii")).unwrap(), T2);
        assert_eq!(std::fs::read(dest.join("LICENSE")).unwrap(), LIC);
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dest.join("template.json")).unwrap())
                .unwrap();
        assert_eq!(meta["space"], "TestSpace");
        assert_eq!(meta["entry_id"], "TestSpace_res-1mm");
        assert_eq!(meta["t1_member"], "pkg/t1.nii");
        assert_eq!(meta["fetched_by"], "dbsloc template fetch");
        assert_eq!(
            meta["zip_sha256"],
            crate::sha256::file_sha256_hex(&zip).unwrap()
        );
        assert!(!staging.exists());
        assert!(events.contains(&FetchEvent::Verifying));
        assert!(events.contains(&FetchEvent::Extracting));
        assert!(!dest.join("download.zip").exists());
        assert!(!dest.join("other.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sha_mismatch_installs_nothing() {
        let root = scratch("sha");
        let zip = make_zip(&root);
        let bad = "0".repeat(64);
        let e = test_entry(&zip, Some(&bad));
        let staging = prepare(&root, &zip);
        let dest = root.join("data/res-1mm");
        let err = install_verified(
            &e,
            &staging.join("download.zip"),
            &staging,
            &dest,
            &AtomicBool::new(false),
            &mut |_| {},
            None,
        )
        .unwrap_err();
        assert!(err.contains("SHA-256 mismatch"), "{err}");
        assert!(err.contains(&bad), "{err}");
        assert!(!dest.exists());
        assert!(!staging.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_leaves_nothing() {
        let root = scratch("cancel");
        let zip = make_zip(&root);
        let e = test_entry(&zip, None);
        let staging = prepare(&root, &zip);
        let dest = root.join("data/res-1mm");
        let err = install_verified(
            &e,
            &staging.join("download.zip"),
            &staging,
            &dest,
            &AtomicBool::new(true),
            &mut |_| {},
            None,
        )
        .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dest.exists());
        assert!(!staging.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_destination_is_replaced() {
        let root = scratch("replace");
        let zip = make_zip(&root);
        let e = test_entry(&zip, None);
        let staging = prepare(&root, &zip);
        let dest = root.join("data/res-1mm");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.txt"), b"old").unwrap();
        install_verified(
            &e,
            &staging.join("download.zip"),
            &staging,
            &dest,
            &AtomicBool::new(false),
            &mut |_| {},
            None,
        )
        .unwrap();
        assert!(!dest.join("stale.txt").exists());
        assert!(dest.join("t1.nii").is_file());
        let leftovers: Vec<_> = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["res-1mm".to_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn body_timeout_scales_with_the_archive_size() {
        assert_eq!(body_timeout(0), BODY_TIMEOUT_FLOOR);
        assert_eq!(body_timeout(300_000_000), Duration::from_secs(6000));
    }

    #[test]
    fn bundled_entry_refused_without_network() {
        let root = scratch("refuse");
        let shipped = lookup("MNI152NLin2009bAsym_res-1mm").unwrap();
        let err = fetch_entry(shipped, &root, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
        assert!(err.contains("bundled"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_reports_states() {
        let root = scratch("list");
        let rows = list_entries_in(None, Some(&root));
        assert_eq!(rows.len(), crate::template_catalog::catalogue().len());
        assert_eq!(rows[0].state, EntryState::MissingBundled);
        assert!(matches!(rows[1].state, EntryState::Available { size } if size > 0));
        let e = rows[1].entry;
        let d = entry_dir(&root, e);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("t1.nii.gz"), b"x").unwrap();
        let rows = list_entries_in(None, Some(&root));
        assert_eq!(rows[1].state, EntryState::Installed(d.join("t1.nii.gz")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
