# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""benchmarks.accuracy: hash-pinned downloads (a fake server, no network), safe extraction,
the pin table and the dataset registry."""

from __future__ import annotations

import hashlib
import io
import re
import tarfile
from pathlib import Path

import pytest

from benchmarks.accuracy import datasets, fetch

PAYLOAD = bytes(range(256)) * 5000
SHA = hashlib.sha256(PAYLOAD).hexdigest()


class _Response(io.BytesIO):
    def __init__(self, data: bytes, status: int) -> None:
        super().__init__(data)
        self.status = status


@pytest.fixture
def server(monkeypatch):
    """Serves PAYLOAD; ``drop_after`` cuts the first response short like a dropped connection."""
    calls: list[int] = []
    state = {"drop_after": None}

    def fake_open(url, offset, timeout):
        calls.append(offset)
        data = PAYLOAD[offset:]
        if state["drop_after"] is not None and len(calls) == 1:
            data = data[: state["drop_after"]]

            class Dropping(_Response):
                def read(self, n=-1):
                    block = super().read(n)
                    if not block:
                        raise ConnectionResetError("dropped")
                    return block

            return Dropping(data, 206 if offset else 200)
        return _Response(data, 206 if offset else 200)

    monkeypatch.setattr(fetch, "_open", fake_open)
    monkeypatch.setattr(fetch.time, "sleep", lambda s: None)
    return calls, state


def test_download_verifies_and_keeps_a_good_file(tmp_path, server):
    calls, _ = server
    dest = tmp_path / "d" / "f.bin"
    fetch.download("https://example.org/f", dest, SHA, len(PAYLOAD))
    assert dest.read_bytes() == PAYLOAD
    fetch.download("https://example.org/f", dest, SHA, len(PAYLOAD))
    assert calls == [0]


def test_a_dropped_connection_resumes_with_a_range_request(tmp_path, server):
    calls, state = server
    state["drop_after"] = 300000
    dest = tmp_path / "f.bin"
    fetch.download("https://example.org/f", dest, SHA, len(PAYLOAD))
    assert dest.read_bytes() == PAYLOAD
    assert calls[0] == 0 and calls[1] > 0


def test_a_partial_file_from_an_earlier_run_is_resumed(tmp_path, server):
    calls, _ = server
    dest = tmp_path / "f.bin"
    (tmp_path / f"f.bin.{SHA[:16]}.part").write_bytes(PAYLOAD[:1000])
    fetch.download("https://example.org/f", dest, SHA, len(PAYLOAD))
    assert dest.read_bytes() == PAYLOAD and calls == [1000]


def test_a_wrong_hash_is_refused_and_nothing_is_kept(tmp_path, server):
    dest = tmp_path / "f.bin"
    with pytest.raises(fetch.HashMismatchError):
        fetch.download("https://example.org/f", dest, "0" * 64, len(PAYLOAD))
    assert not dest.exists() and not list(tmp_path.iterdir())
    with pytest.raises(fetch.HashMismatchError):
        fetch.download("https://example.org/f", dest, SHA, len(PAYLOAD) - 1)
    assert not list(tmp_path.iterdir())


def test_a_tampered_existing_file_is_replaced(tmp_path, server):
    dest = tmp_path / "f.bin"
    dest.write_bytes(b"x" * len(PAYLOAD))
    fetch.download("https://example.org/f", dest, SHA, len(PAYLOAD))
    assert dest.read_bytes() == PAYLOAD


def test_unpinned_and_plain_http_downloads_are_refused_before_any_request(tmp_path, server):
    calls, _ = server
    with pytest.raises(fetch.UnpinnedError):
        fetch.download("https://example.org/f", tmp_path / "f", None)
    with pytest.raises(ValueError, match="HTTPS"):
        fetch.download("http://example.org/f", tmp_path / "f", SHA)
    assert calls == []


def test_stream_sha256_stores_nothing(tmp_path, server, monkeypatch):
    monkeypatch.chdir(tmp_path)
    assert fetch.stream_sha256("https://example.org/f") == (SHA, len(PAYLOAD))
    assert not list(tmp_path.iterdir())


def _tar(path: Path, members: list[tuple[str, bytes | None, bytes]]) -> Path:
    with tarfile.open(path, "w:gz") as tf:
        for name, data, kind in members:
            info = tarfile.TarInfo(name)
            if kind == tarfile.REGTYPE:
                info.size = len(data)
                tf.addfile(info, io.BytesIO(data))
            else:
                info.type = kind
                info.linkname = "/etc/passwd"
                tf.addfile(info)
    return path


def test_safe_extract_writes_only_selected_regular_files(tmp_path):
    archive = _tar(
        tmp_path / "a.tar.gz",
        [
            ("top/s1/brain.nii.gz", b"one", tarfile.REGTYPE),
            ("top/s1/other.txt", b"skip", tarfile.REGTYPE),
            ("top/s2/brain.nii.gz", b"two", tarfile.REGTYPE),
            ("top/link", None, tarfile.SYMTYPE),
        ],
    )
    dest = tmp_path / "out"

    def select(name):
        m = re.fullmatch(r"top/(s\d)/brain\.nii\.gz", name)
        return f"brains/{m.group(1)}.nii.gz" if m else None

    written = fetch.safe_extract_tar(archive, dest, select)
    assert sorted(p.name for p in written) == ["s1.nii.gz", "s2.nii.gz"]
    assert (dest / "brains" / "s2.nii.gz").read_bytes() == b"two"
    assert sorted(p.name for p in dest.rglob("*") if p.is_file()) == ["s1.nii.gz", "s2.nii.gz"]


@pytest.mark.parametrize(
    "members",
    [
        [("../evil.txt", b"x", tarfile.REGTYPE)],
        [("/abs/evil.txt", b"x", tarfile.REGTYPE)],
        [("ok/../../evil.txt", b"x", tarfile.REGTYPE)],
        [("ok/link", None, tarfile.SYMTYPE)],
        [("ok/hard", None, tarfile.LNKTYPE)],
    ],
)
def test_unsafe_members_are_refused(tmp_path, members):
    archive = _tar(tmp_path / "bad.tar.gz", members)
    dest = tmp_path / "out"
    with pytest.raises(fetch.UnsafeArchiveError):
        fetch.safe_extract_tar(archive, dest, lambda name: name)
    assert not (tmp_path / "evil.txt").exists()
    assert not [p for p in dest.rglob("*") if p.is_file()]


def test_an_output_path_outside_the_folder_is_refused(tmp_path):
    archive = _tar(tmp_path / "a.tar.gz", [("ok.txt", b"x", tarfile.REGTYPE)])
    with pytest.raises(fetch.UnsafeArchiveError):
        fetch.safe_extract_tar(archive, tmp_path / "out", lambda name: "../escape.txt")
    assert not (tmp_path / "escape.txt").exists()


def test_data_folders_inside_the_repository_are_refused(tmp_path):
    with pytest.raises(ValueError, match="inside the repository"):
        fetch.check_outside_repo(fetch.repo_root() / "data")
    assert fetch.check_outside_repo(tmp_path) == tmp_path.resolve()


def test_every_pin_is_https_with_a_sha256_and_size():
    pins = datasets._pins()
    assert pins
    for source, files in pins.items():
        assert files, source
        for f in files:
            assert f["url"].startswith("https://"), f
            assert f["sha256"] is None or re.fullmatch(r"[0-9a-f]{64}", f["sha256"]), f
            assert isinstance(f["size"], int) and f["size"] > 0, f
            fetch.safe_member_path(f["path"])


def test_every_subject_file_comes_from_a_download():
    for ds in datasets.registry().values():
        direct = {d.path for d in ds.downloads if not d.extract}
        archived = [d for d in ds.downloads if d.extract]
        for s in (*ds.subjects, *([ds.template] if ds.template else [])):
            for rel in (s.image, s.labels, s.landmarks, s.mask):
                if rel is not None and rel not in direct:
                    assert archived, f"{ds.key}: {rel} comes from no download"
        assert ds.default_pairs >= ds.quick_pairs >= 1
        if ds.protocol == "template":
            assert ds.template is not None


def test_the_mindboggle_archive_rules_produce_the_subject_images():
    ds = datasets.get("mindboggle")
    (archive,) = [d for d in ds.downloads if d.extract]
    assert len(ds.subjects) == 20
    for s in ds.subjects:
        assert archive.select(f"OASIS-TRT-20_volumes/{s.id}/t1weighted_brain.nii.gz") == s.image
    assert archive.select("OASIS-TRT-20_volumes/OASIS-TRT-20-1/t1weighted.nii.gz") is None
    assert archive.select("OASIS-TRT-20_volumes/OASIS-TRT-20-1/labels.DKT31.manual.nii.gz") is None


def test_the_7t_archive_rules_produce_the_images_and_structure_masks():
    ds = datasets.get("deep7t")
    (archive,) = ds.downloads
    wb = "osf_dataset/M_7T/M0164_7T/MRimages/Nii_STD_BET/WB/M0164_7T_WB_"
    assert archive.select(wb + "MP2RAGE_STD_BET.nii.gz") == "M0164/mp2rage_brain.nii.gz"
    assert archive.select(wb + "INV2_e1_M_STD_BET_mask.nii.gz") == "M0164/brain_mask.nii.gz"
    masks = "osf_dataset/M_7T/M0164_7T/Masks/STN/calc/M0164_7T_STN_"
    assert archive.select(masks + "L_AA-BI_conj.nii.gz") == "M0164/STN_L.nii.gz"
    assert archive.select(masks + "L_AA_bin.nii.gz") is None
    assert archive.select(masks + "L_AA-BI_conj_dilated.nii.gz") is None
    assert (
        archive.select("osf_dataset/M_3T/M0164_3T/Masks/STN/calc/M0164_3T_STN_L_AA-BI_conj.nii.gz")
        is None
    )
    assert (
        archive.select(wb.replace("M0164_7T_WB", "M1334_7T_WB") + "MP2RAGE_STD_BET.nii.gz") is None
    )
    s = ds.subject("M0164")
    assert dict(s.masks)[1] == "M0164/STN_L.nii.gz" and len(s.masks) == 10


def test_a_stream_that_ends_early_is_resumed(tmp_path, server, monkeypatch):
    calls, _ = server
    real = fetch._open

    def short_first(url, offset, timeout):
        r = real(url, offset, timeout)
        if len(calls) == 1:
            return _Response(r.read()[:5000], r.status)
        return r

    monkeypatch.setattr(fetch, "_open", short_first)
    dest = tmp_path / "f.bin"
    fetch.download("https://example.org/f", dest, SHA, len(PAYLOAD))
    assert dest.read_bytes() == PAYLOAD and calls == [0, 5000]


def test_a_complete_partial_file_is_verified_without_a_request(tmp_path, server):
    calls, _ = server
    (tmp_path / f"f.bin.{SHA[:16]}.part").write_bytes(PAYLOAD)
    fetch.download("https://example.org/f", tmp_path / "f.bin", SHA, len(PAYLOAD))
    assert (tmp_path / "f.bin").read_bytes() == PAYLOAD and calls == []


def test_a_server_that_ignores_the_range_restarts_the_file(tmp_path, monkeypatch):
    calls = []

    def ignore_range(url, offset, timeout):
        calls.append(offset)
        return _Response(PAYLOAD, 200)

    monkeypatch.setattr(fetch, "_open", ignore_range)
    (tmp_path / f"f.bin.{SHA[:16]}.part").write_bytes(b"junk" * 100)
    fetch.download("https://example.org/f", tmp_path / "f.bin", SHA, len(PAYLOAD))
    assert (tmp_path / "f.bin").read_bytes() == PAYLOAD and calls == [400]


def test_insecure_redirects_and_short_pins_fail_without_retries(tmp_path, monkeypatch):
    handler = fetch._HttpsOnlyRedirect()
    with pytest.raises(fetch.InsecureRedirectError):
        handler.redirect_request(None, None, 302, "Found", {}, "http://example.org/f")
    calls = []

    def redirected(url, offset, timeout):
        calls.append(offset)
        raise fetch.InsecureRedirectError("http://example.org/f")

    monkeypatch.setattr(fetch, "_open", redirected)
    with pytest.raises(fetch.InsecureRedirectError):
        fetch.download("https://example.org/f", tmp_path / "f.bin", SHA, len(PAYLOAD))
    assert calls == [0]
    monkeypatch.setattr(fetch, "_open", lambda url, offset, timeout: _Response(PAYLOAD, 200))
    with pytest.raises(fetch.HashMismatchError, match="publishes"):
        fetch.stream_sha256("https://example.org/f", size=len(PAYLOAD) - 1)


def test_fetch_dataset_downloads_verifies_and_extracts(tmp_path, monkeypatch):
    archive = io.BytesIO()
    with tarfile.open(fileobj=archive, mode="w:gz") as tf:
        for name, data in (("top/s1/img.nii.gz", b"image"), ("top/s1/skip.txt", b"x")):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tf.addfile(info, io.BytesIO(data))
    served = {"https://h/a.tar.gz": archive.getvalue(), "https://h/l.txt": b"labels"}
    monkeypatch.setattr(
        fetch, "_open", lambda url, offset, timeout: _Response(served[url][offset:], 200)
    )

    def pin(url, path, extract=()):
        data = served[url]
        return datasets.Download(path, url, hashlib.sha256(data).hexdigest(), len(data), extract)

    ds = datasets.Dataset(
        key="t",
        title="t",
        licence="",
        downloads=(
            pin("https://h/a.tar.gz", "a.tar.gz", ((r"top/(s\d)/img\.nii\.gz", r"\1/img.nii.gz"),)),
            pin("https://h/l.txt", "s1/labels.txt"),
        ),
        subjects=(datasets.Subject(id="s1", image="s1/img.nii.gz", labels="s1/labels.txt"),),
        protocol="pairwise",
    )
    root = fetch.fetch_dataset(ds, tmp_path / "data", log=lambda s: None)
    assert (root / "s1" / "img.nii.gz").read_bytes() == b"image"
    assert (root / "s1" / "labels.txt").read_bytes() == b"labels"
    assert (root / ".fetched.json").is_file() and not (root / "downloads" / "a.tar.gz").exists()
    assert not (root / "s1" / "skip.txt").exists()
    fetch.fetch_dataset(ds, tmp_path / "data", log=lambda s: None)
    (root / "s1" / "img.nii.gz").unlink()
    with pytest.raises(FileNotFoundError, match="extract the archives again"):
        fetch.fetch_dataset(ds, tmp_path / "data", log=lambda s: None)
    bad = datasets.Dataset(
        key="u",
        title="u",
        licence="",
        subjects=(),
        protocol="pairwise",
        downloads=(datasets.Download("x", "https://h/l.txt", None, 6),),
    )
    with pytest.raises(fetch.UnpinnedError):
        fetch.fetch_dataset(bad, tmp_path / "data", log=lambda s: None)
