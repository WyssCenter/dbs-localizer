# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Hash-pinned downloads and safe archive extraction.

Every file is downloaded over HTTPS (redirects included), checked against a SHA-256 pinned in
:mod:`benchmarks.accuracy.datasets` before it is used, and an archive member is written only
when its name and the output path stay inside the data folder and it is a regular file.
"""

from __future__ import annotations

import hashlib
import http.client
import os
import ssl
import tarfile
import time
import urllib.error
import urllib.request
from collections.abc import Callable, Iterator
from pathlib import Path, PurePosixPath
from typing import BinaryIO

CHUNK = 1 << 20
USER_AGENT = "fastwarp-benchmark"


class UnpinnedError(RuntimeError):
    """A download has no pinned SHA-256 yet: fetch refuses it."""


class HashMismatchError(RuntimeError):
    """A downloaded file does not match its pinned size or SHA-256."""


class UnsafeArchiveError(RuntimeError):
    """An archive member would leave the output folder or is not a regular file."""


class InsecureRedirectError(RuntimeError):
    """A server redirected a download to a non-HTTPS address (not retried)."""


class _HttpsOnlyRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not newurl.lower().startswith("https://"):
            raise InsecureRedirectError(f"refusing a redirect to a non-HTTPS URL: {newurl}")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def _opener() -> urllib.request.OpenerDirector:
    return urllib.request.build_opener(_HttpsOnlyRedirect())


def _check_url(url: str) -> None:
    if not url.lower().startswith("https://"):
        raise ValueError(f"only HTTPS downloads are allowed, got {url}")


def _open(url: str, offset: int, timeout: float):
    headers = {"User-Agent": USER_AGENT}
    if offset:
        headers["Range"] = f"bytes={offset}-"
    return _opener().open(urllib.request.Request(url, headers=headers), timeout=timeout)


def _chunks(
    url: str, offset: int, *, retries: int, timeout: float, size: int | None = None
) -> Iterator[tuple[int, bytes]]:
    """Yields ``(offset, chunk)`` from ``offset`` on, reopening with a Range request after a
    dropped connection, or after a stream that ends before ``size`` bytes. A server that
    ignores the Range yields from offset 0 again."""
    attempt = 0
    while True:
        try:
            with _open(url, offset, timeout) as r:
                if offset and getattr(r, "status", 200) != 206:
                    offset = 0
                while True:
                    block = r.read(CHUNK)
                    if not block:
                        if size is not None and offset < size:
                            raise ConnectionError(f"the stream ended after {offset} bytes")
                        return
                    yield offset, block
                    offset += len(block)
                    attempt = 0
        except (
            ConnectionError,
            TimeoutError,
            urllib.error.URLError,
            http.client.HTTPException,
            ssl.SSLError,
        ) as e:
            if isinstance(e, urllib.error.HTTPError) and e.code < 500 and e.code != 429:
                raise
            if isinstance(e, ssl.SSLCertVerificationError) or isinstance(
                getattr(e, "reason", None), ssl.SSLCertVerificationError
            ):
                raise
            attempt += 1
            if attempt > retries:
                raise
            time.sleep(min(2.0 * attempt, 30.0))


def stream_sha256(
    url: str, *, size: int | None = None, retries: int = 8, timeout: float = 120.0
) -> tuple[str, int]:
    """SHA-256 and size of a URL's content, computed while streaming (nothing is stored).

    With ``size`` (the host's published size), a stream that ends early is resumed, and one
    of another length raises :class:`HashMismatchError`.
    """
    _check_url(url)
    h, n = hashlib.sha256(), 0
    for offset, block in _chunks(url, 0, retries=retries, timeout=timeout, size=size):
        if offset == 0 and n:
            h, n = hashlib.sha256(), 0
        h.update(block)
        n += len(block)
    if size is not None and n != size:
        raise HashMismatchError(f"{url}: got {n} bytes, the host publishes {size}")
    return h.hexdigest(), n


def sha256_file(path: Path) -> str:
    """SHA-256 of a local file."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(CHUNK), b""):
            h.update(block)
    return h.hexdigest()


def download(
    url: str,
    dest: Path,
    sha256: str | None,
    size: int | None = None,
    *,
    retries: int = 8,
    timeout: float = 120.0,
    progress: Callable[[int, int | None], None] | None = None,
) -> Path:
    """Download ``url`` to ``dest`` and check it against ``sha256`` (and ``size`` when given).

    An existing ``dest`` that matches is kept. A partial ``<dest>.part`` from an interrupted
    run is resumed. A file that does not match is deleted and :class:`HashMismatchError` is
    raised; a download without a pin raises :class:`UnpinnedError` before any request.
    """
    if not sha256:
        raise UnpinnedError(
            f"{dest.name} has no pinned SHA-256; fill it in pins.json first "
            "(python -m benchmarks.accuracy pin)"
        )
    _check_url(url)
    sha256 = sha256.lower()
    dest = Path(dest)
    if dest.is_file() and (size is None or dest.stat().st_size == size):
        if sha256_file(dest) == sha256:
            return dest
        dest.unlink()
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_name(f"{dest.name}.{sha256[:16]}.part")
    h = hashlib.sha256()
    have = 0
    if part.is_file():
        have = part.stat().st_size
        if size is not None and have > size:
            part.unlink()
            have = 0
        else:
            with open(part, "rb") as f:
                for block in iter(lambda: f.read(CHUNK), b""):
                    h.update(block)
    complete = size is not None and have == size
    with open(part, "ab" if have else "wb") as out:
        stream = () if complete else _chunks(url, have, retries=retries, timeout=timeout, size=size)
        for offset, block in stream:
            if offset == 0 and have:
                out.seek(0)
                out.truncate()
                h, have = hashlib.sha256(), 0
            out.write(block)
            h.update(block)
            have += len(block)
            if size is not None and have > size:
                break
            if progress is not None:
                progress(have, size)
    got = h.hexdigest()
    if (size is not None and have != size) or got != sha256:
        part.unlink(missing_ok=True)
        raise HashMismatchError(
            f"{url}: got {have} bytes with SHA-256 {got}, expected "
            f"{size if size is not None else '?'} bytes with SHA-256 {sha256}"
        )
    os.replace(part, dest)
    return dest


def safe_member_path(name: str) -> PurePosixPath:
    """The archive member name as a relative path, or :class:`UnsafeArchiveError` when it is
    absolute, has a drive, or climbs out with ``..``."""
    if "\\" in name or name.startswith("/") or (len(name) > 1 and name[1] == ":"):
        raise UnsafeArchiveError(f"unsafe member name {name!r}")
    p = PurePosixPath(name)
    if p.is_absolute() or any(part == ".." for part in p.parts) or not p.parts:
        raise UnsafeArchiveError(f"unsafe member name {name!r}")
    return p


def _inside(root: Path, rel: str) -> Path:
    target = (root / rel).resolve()
    base = root.resolve()
    if target != base and base not in target.parents:
        raise UnsafeArchiveError(f"{rel!r} would be written outside {root}")
    return target


def _copy(src: BinaryIO, target: Path, size: int) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = target.with_name(target.name + ".part")
    written = 0
    with open(tmp, "wb") as out:
        for block in iter(lambda: src.read(CHUNK), b""):
            written += len(block)
            if written > size:
                break
            out.write(block)
    if written != size:
        tmp.unlink(missing_ok=True)
        raise UnsafeArchiveError(f"{target.name}: member size {written} != header size {size}")
    os.replace(tmp, target)


def safe_extract_tar(archive: Path, dest: Path, select: Callable[[str], str | None]) -> list[Path]:
    """Extract the members ``select`` maps to a relative output path (``None`` skips one).

    A selected member must be a regular file with a safe name, and its output path must stay
    inside ``dest``; links, devices and anything else are refused. Nothing is taken from the
    archive's permissions or owners. Returns the written paths.
    """
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []
    with tarfile.open(archive, mode="r:*") as tf:
        for member in tf:
            safe_member_path(member.name)
            rel = select(member.name)
            if rel is None:
                continue
            if not member.isreg():
                raise UnsafeArchiveError(f"{member.name!r} is not a regular file")
            safe_member_path(rel)
            target = _inside(dest, rel)
            src = tf.extractfile(member)
            if src is None:
                raise UnsafeArchiveError(f"{member.name!r} has no data")
            with src:
                _copy(src, target, member.size)
            written.append(target)
    return written


def repo_root() -> Path:
    """The repository this benchmark lives in."""
    return Path(__file__).resolve().parents[2]


def check_outside_repo(folder: Path) -> Path:
    """``folder`` resolved, or :class:`ValueError` when it lies inside the repository (data
    and results never go into the checkout)."""
    folder = Path(folder).resolve()
    root = repo_root()
    if folder == root or root in folder.parents:
        raise ValueError(f"{folder} is inside the repository {root}; choose a folder outside it")
    return folder


def fetch_dataset(
    ds, data: Path, *, keep_archives: bool = False, log: Callable[[str], None] = print
) -> Path:
    """Download, verify and extract every file of ``ds`` into ``<data>/<ds.key>``.

    Files already present and matching their pin are kept; an archive is extracted once (a
    marker named after its SHA-256 records it) and deleted afterwards unless
    ``keep_archives``. Writes ``.fetched.json`` when every subject's files exist.
    """
    import json

    root = check_outside_repo(data) / ds.key
    unpinned = [d.path for d in ds.downloads if not d.sha256]
    if unpinned:
        raise UnpinnedError(
            f"{ds.key}: {len(unpinned)} files have no pinned SHA-256: {unpinned[:3]}"
        )
    for i, d in enumerate(ds.downloads, 1):
        if d.extract:
            marker = root / ".extracted" / f"{d.sha256}.json"
            if marker.is_file():
                continue
            archive = root / "downloads" / Path(d.path).name
            log(f"[{i}/{len(ds.downloads)}] {d.path} ({(d.size or 0) / 1e6:.0f} MB)")
            download(d.url, archive, d.sha256, d.size)
            written = safe_extract_tar(archive, root, d.select)
            if not written:
                raise UnsafeArchiveError(f"{d.path}: no member matched the extraction rules")
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_text(json.dumps([str(p.relative_to(root)) for p in written]), "utf-8")
            if not keep_archives:
                archive.unlink()
        else:
            target = root / safe_member_path(d.path)
            _inside(root, d.path)
            if not target.is_file():
                log(f"[{i}/{len(ds.downloads)}] {d.path}")
            download(d.url, target, d.sha256, d.size)
    missing = []
    for s in (*ds.subjects, *([ds.template] if ds.template else [])):
        for rel in (s.image, s.labels, s.landmarks, s.mask, *(m for _, m in s.masks)):
            if rel and not (root / rel).is_file():
                missing.append(rel)
    if missing:
        raise FileNotFoundError(
            f"{ds.key}: {len(missing)} expected files are missing: {missing[:3]}; delete "
            f"{root / '.extracted'} to extract the archives again"
        )
    (root / ".fetched.json").write_text(
        json.dumps({"dataset": ds.key, "files": len(ds.downloads)}), encoding="utf-8"
    )
    return root
