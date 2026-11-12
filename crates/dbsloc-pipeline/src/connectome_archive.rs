// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Safe extraction of connectome archives: zip, tar and tar.gz (`specs/15` §4.3, §4.6).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::connectome_fetch::{ConnectomeError, ErrorCode};
use crate::template_fetch::{is_cancelled, CHUNK};

const BLOCK: usize = 512;
const MAX_NAME: usize = 4096;
const MAX_META: u64 = 1 << 20;

/// Archive layouts `dbsloc connectome import` can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
}

/// Kind of an archive by file name, `None` for anything else.
pub fn archive_kind(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else if name.ends_with(".tar") {
        Some(ArchiveKind::Tar)
    } else {
        None
    }
}

/// File name of an archive without its archive suffix.
pub fn archive_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    for suffix in [".tar.gz", ".tgz", ".tar", ".zip"] {
        if lower.ends_with(suffix) {
            return name[..name.len() - suffix.len()].to_owned();
        }
    }
    name
}

fn archive_err(message: impl Into<String>) -> ConnectomeError {
    ConnectomeError::new(ErrorCode::Archive, message)
}

pub(crate) fn map_io(what: &str, e: &std::io::Error) -> ConnectomeError {
    if e.kind() == std::io::ErrorKind::StorageFull {
        ConnectomeError::new(ErrorCode::Disk, format!("{what}: the disk is full"))
    } else {
        ConnectomeError::new(ErrorCode::Io, format!("{what}: {e}"))
    }
}

/// A member name as a relative path, refusing absolute names, `..` components and drive letters.
pub fn safe_relative(name: &str) -> Result<PathBuf, ConnectomeError> {
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(archive_err(format!("absolute member name refused: {name}")));
    }
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                return Err(archive_err(format!(
                    "member name with '..' refused: {name}"
                )))
            }
            p if p.contains(':') || p.contains('\0') => {
                return Err(archive_err(format!("member name refused: {name}")))
            }
            p => out.push(p),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(archive_err(format!("empty member name refused: {name:?}")));
    }
    Ok(out)
}

fn copy_limited<R: Read, W: Write>(
    mut src: R,
    mut dst: W,
    mut left: u64,
    cancel: &AtomicBool,
) -> Result<(), ConnectomeError> {
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        if is_cancelled(cancel) {
            return Err(ConnectomeError::cancelled());
        }
        let want = usize::try_from(left).map_or(CHUNK, |l| l.min(CHUNK));
        let n = src
            .read(&mut buf[..want])
            .map_err(|e| map_io("cannot read the archive", &e))?;
        if n == 0 {
            return Err(archive_err("the archive ends inside a member"));
        }
        dst.write_all(&buf[..n])
            .map_err(|e| map_io("cannot write an extracted file", &e))?;
        left -= n as u64;
    }
    Ok(())
}

/// Sum of the uncompressed member sizes of a zip archive.
pub fn zip_uncompressed_bytes(archive: &Path) -> Result<u64, ConnectomeError> {
    let file = File::open(archive).map_err(|e| map_io("cannot open the archive", &e))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| archive_err(format!("cannot read the zip archive: {e}")))?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let entry = zip
            .by_index_raw(i)
            .map_err(|e| archive_err(format!("cannot read the zip archive: {e}")))?;
        total = total.saturating_add(entry.size());
    }
    Ok(total)
}

/// Extracts a zip archive into `dest`. Entries with absolute names, `..` components, links or
/// special files are refused; `__MACOSX/` resource forks are skipped.
pub fn extract_zip(
    archive: &Path,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<(), ConnectomeError> {
    let file = File::open(archive).map_err(|e| map_io("cannot open the archive", &e))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| archive_err(format!("cannot read the zip archive: {e}")))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| archive_err(format!("cannot read the zip archive: {e}")))?;
        let name = entry
            .name()
            .map_err(|e| archive_err(format!("unreadable member name: {e}")))?
            .into_owned();
        if name.starts_with("__MACOSX/") {
            continue;
        }
        let rel = safe_relative(&name)?;
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170_000;
            if kind != 0 && kind != 0o100_000 && kind != 0o040_000 {
                return Err(archive_err(format!("link or special file refused: {name}")));
            }
        }
        let target = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| map_io("cannot create a folder", &e))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| map_io("cannot create a folder", &e))?;
        }
        let out =
            File::create(&target).map_err(|e| map_io("cannot create an extracted file", &e))?;
        let size = entry.size();
        copy_limited(&mut entry, out, size, cancel)?;
    }
    Ok(())
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        let mut v = u64::from(field[0] & 0x7f);
        for b in &field[1..] {
            v = v.checked_mul(256)?.checked_add(u64::from(*b))?;
        }
        return Some(v);
    }
    let text: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| char::from(*b))
        .collect();
    let t = text.trim();
    if t.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(t, 8).ok()
}

fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn header_checksum_ok(h: &[u8; BLOCK]) -> bool {
    let stored = parse_octal(&h[148..156]);
    let sum: u64 = h
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(*b)
            }
        })
        .sum();
    stored == Some(sum)
}

fn read_block<R: Read>(r: &mut R, buf: &mut [u8; BLOCK]) -> Result<bool, ConnectomeError> {
    let mut got = 0;
    while got < BLOCK {
        let n = r
            .read(&mut buf[got..])
            .map_err(|e| map_io("cannot read the archive", &e))?;
        if n == 0 {
            return if got == 0 {
                Ok(false)
            } else {
                Err(archive_err("the tar archive ends inside a header"))
            };
        }
        got += n;
    }
    Ok(true)
}

fn read_meta<R: Read>(r: &mut R, size: u64) -> Result<Vec<u8>, ConnectomeError> {
    if size > MAX_META {
        return Err(archive_err("oversized tar metadata record"));
    }
    let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
    let mut data = vec![0u8; usize::try_from(padded).unwrap_or(0)];
    r.read_exact(&mut data)
        .map_err(|e| map_io("cannot read the archive", &e))?;
    data.truncate(usize::try_from(size).unwrap_or(0));
    Ok(data)
}

fn skip_bytes<R: Read>(r: &mut R, n: u64) -> Result<(), ConnectomeError> {
    let copied = std::io::copy(&mut r.take(n), &mut std::io::sink())
        .map_err(|e| map_io("cannot read the archive", &e))?;
    if copied == n {
        Ok(())
    } else {
        Err(archive_err("the tar archive ends inside a member"))
    }
}

fn pax_path(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    for rec in text.split('\n') {
        let body = rec.split_once(' ').map_or(rec, |(_, b)| b);
        if let Some(v) = body.strip_prefix("path=") {
            return Some(v.to_owned());
        }
    }
    None
}

/// Extracts a tar stream. `map(name, is_dir)` gives the path relative to `dest` for a regular
/// file or folder, or an error to refuse it. Links, devices and fifos are always refused.
/// Returns the number of regular files written.
pub fn extract_tar<R: Read>(
    mut r: R,
    dest: &Path,
    map: &mut dyn FnMut(&str, bool) -> Result<PathBuf, ConnectomeError>,
    cancel: &AtomicBool,
) -> Result<usize, ConnectomeError> {
    let mut files = 0usize;
    let mut long_name: Option<String> = None;
    let mut pax_name: Option<String> = None;
    let mut zero_blocks = 0;
    let mut h = [0u8; BLOCK];
    loop {
        if is_cancelled(cancel) {
            return Err(ConnectomeError::cancelled());
        }
        if !read_block(&mut r, &mut h)? {
            break;
        }
        if h.iter().all(|b| *b == 0) {
            zero_blocks += 1;
            if zero_blocks >= 2 {
                break;
            }
            continue;
        }
        zero_blocks = 0;
        if !header_checksum_ok(&h) {
            return Err(archive_err("the tar archive has a corrupt header"));
        }
        let size = parse_octal(&h[124..136]).ok_or_else(|| archive_err("bad tar member size"))?;
        let flag = h[156];
        match flag {
            b'L' => {
                let data = read_meta(&mut r, size)?;
                long_name = Some(cstr(&data));
                continue;
            }
            b'x' => {
                let data = read_meta(&mut r, size)?;
                pax_name = pax_path(&data);
                continue;
            }
            b'g' => {
                read_meta(&mut r, size)?;
                continue;
            }
            _ => {}
        }
        let mut name = cstr(&h[0..100]);
        if &h[257..262] == b"ustar" && h[345] != 0 {
            name = format!("{}/{name}", cstr(&h[345..500]));
        }
        if let Some(n) = pax_name.take().or_else(|| long_name.take()) {
            name = n;
        }
        if name.len() > MAX_NAME {
            return Err(archive_err("member name too long"));
        }
        let padding = size.div_ceil(BLOCK as u64) * BLOCK as u64 - size;
        match flag {
            b'0' | 0 => {
                let rel = map(&name, false)?;
                let target = dest.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| map_io("cannot create a folder", &e))?;
                }
                let out = File::create(&target)
                    .map_err(|e| map_io("cannot create an extracted file", &e))?;
                copy_limited(&mut r, out, size, cancel)?;
                skip_bytes(&mut r, padding)?;
                files += 1;
            }
            b'5' => {
                let rel = map(&name, true)?;
                std::fs::create_dir_all(dest.join(rel))
                    .map_err(|e| map_io("cannot create a folder", &e))?;
                skip_bytes(&mut r, size + padding)?;
            }
            _ => return Err(archive_err(format!("link or special file refused: {name}"))),
        }
    }
    Ok(files)
}

/// Extracts a `.tar` or `.tar.gz` file into `dest` with the generic path rules.
pub fn extract_tar_file(
    archive: &Path,
    kind: ArchiveKind,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<usize, ConnectomeError> {
    let file = File::open(archive).map_err(|e| map_io("cannot open the archive", &e))?;
    let reader = std::io::BufReader::with_capacity(1 << 20, file);
    let mut map = |name: &str, _dir: bool| safe_relative(name);
    match kind {
        ArchiveKind::TarGz => {
            extract_tar(flate2::read::GzDecoder::new(reader), dest, &mut map, cancel)
        }
        _ => extract_tar(reader, dest, &mut map, cancel),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tar_header(name: &str, size: usize, flag: u8) -> [u8; BLOCK] {
        let mut h = [0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        h[156] = flag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        h[155] = b' ';
        h
    }

    pub(crate) fn tar_bytes(members: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data, flag) in members {
            out.extend_from_slice(&tar_header(name, data.len(), *flag));
            out.extend_from_slice(data);
            out.extend(std::iter::repeat_n(
                0u8,
                data.len().div_ceil(BLOCK) * BLOCK - data.len(),
            ));
        }
        out.extend(std::iter::repeat_n(0u8, 2 * BLOCK));
        out
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn safe_relative_refuses_escapes() {
        assert_eq!(
            safe_relative("a/b.txt").unwrap(),
            Path::new("a").join("b.txt")
        );
        assert_eq!(safe_relative("./a//b").unwrap(), Path::new("a").join("b"));
        for bad in [
            "../x",
            "a/../../x",
            "/etc/passwd",
            "\\x",
            "C:/x",
            "a/b:c",
            "",
        ] {
            let e = safe_relative(bad).unwrap_err();
            assert_eq!(e.code, ErrorCode::Archive, "{bad}");
        }
    }

    #[test]
    fn kinds_and_stems() {
        assert_eq!(
            archive_kind(Path::new("a.TAR.GZ")),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(archive_kind(Path::new("a.tgz")), Some(ArchiveKind::TarGz));
        assert_eq!(archive_kind(Path::new("a.tar")), Some(ArchiveKind::Tar));
        assert_eq!(archive_kind(Path::new("a.zip")), Some(ArchiveKind::Zip));
        assert_eq!(archive_kind(Path::new("a.rar")), None);
        assert_eq!(archive_stem(Path::new("/x/My Conn.tar.gz")), "My Conn");
    }

    #[test]
    fn tar_extracts_files_and_folders() {
        let d = tempfile::tempdir().unwrap();
        let tar = tar_bytes(&[
            ("top/", b"", b'5'),
            ("top/a.txt", b"hello", b'0'),
            ("top/sub/b.bin", &[7u8; 1000], b'0'),
        ]);
        let n = extract_tar(
            &tar[..],
            d.path(),
            &mut |n, _| safe_relative(n),
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(std::fs::read(d.path().join("top/a.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(d.path().join("top/sub/b.bin")).unwrap().len(),
            1000
        );
    }

    #[test]
    fn tar_refuses_links_and_traversal() {
        let outer = tempfile::tempdir().unwrap();
        let inner = outer.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        let link = tar_bytes(&[("top/l", b"", b'2')]);
        let e = extract_tar(
            &link[..],
            &inner,
            &mut |n, _| safe_relative(n),
            &no_cancel(),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Archive);
        let slip = tar_bytes(&[("../x", b"no", b'0')]);
        let e = extract_tar(
            &slip[..],
            &inner,
            &mut |n, _| safe_relative(n),
            &no_cancel(),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Archive);
        assert!(!outer.path().join("x").exists());
    }

    #[test]
    fn tar_detects_a_corrupt_header_and_truncation() {
        let d = tempfile::tempdir().unwrap();
        let mut tar = tar_bytes(&[("a", b"hello", b'0')]);
        tar[0] ^= 1;
        let e = extract_tar(
            &tar[..],
            d.path(),
            &mut |n, _| safe_relative(n),
            &no_cancel(),
        )
        .unwrap_err();
        assert!(e.message.contains("corrupt"), "{e}");
        let tar = tar_bytes(&[("a", &[1u8; 2000], b'0')]);
        let e = extract_tar(
            &tar[..700],
            d.path(),
            &mut |n, _| safe_relative(n),
            &no_cancel(),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Archive);
    }

    #[test]
    fn tar_reads_gnu_long_names() {
        let d = tempfile::tempdir().unwrap();
        let long = format!("dir/{}", "n".repeat(150));
        let mut tar = Vec::new();
        let mut data = long.clone().into_bytes();
        data.push(0);
        tar.extend_from_slice(&tar_header("././@LongLink", data.len(), b'L'));
        tar.extend_from_slice(&data);
        tar.extend(std::iter::repeat_n(0u8, BLOCK - data.len()));
        tar.extend_from_slice(&tar_header("short", 2, b'0'));
        tar.extend_from_slice(b"hi");
        tar.extend(std::iter::repeat_n(0u8, BLOCK - 2));
        tar.extend(std::iter::repeat_n(0u8, 2 * BLOCK));
        extract_tar(
            &tar[..],
            d.path(),
            &mut |n, _| safe_relative(n),
            &no_cancel(),
        )
        .unwrap();
        assert!(d.path().join(&long).is_file());
    }

    #[test]
    fn zip_extraction_refuses_traversal() {
        use zip::write::SimpleFileOptions;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.zip");
        let mut w = zip::ZipWriter::new(File::create(&p).unwrap());
        w.start_file("../x", SimpleFileOptions::default()).unwrap();
        w.write_all(b"x").unwrap();
        w.finish().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let e = extract_zip(&p, &out, &no_cancel()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Archive);
        assert!(!d.path().join("x").exists());
    }

    #[test]
    fn zip_extraction_and_size() {
        use zip::write::SimpleFileOptions;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ok.zip");
        let mut w = zip::ZipWriter::new(File::create(&p).unwrap());
        let o = SimpleFileOptions::default();
        w.add_directory("top/", o).unwrap();
        w.start_file("top/data.mat", o).unwrap();
        w.write_all(&[3u8; 300]).unwrap();
        w.start_file("__MACOSX/top/._data.mat", o).unwrap();
        w.write_all(b"fork").unwrap();
        w.finish().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        extract_zip(&p, &out, &no_cancel()).unwrap();
        assert_eq!(std::fs::read(out.join("top/data.mat")).unwrap().len(), 300);
        assert!(!out.join("__MACOSX").exists());
        assert_eq!(zip_uncompressed_bytes(&p).unwrap(), 304);
    }
}
