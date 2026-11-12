# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

import argparse
import hashlib
import http.client
import json
import struct
import sys
import time
import urllib.error
import urllib.request
import zlib
from pathlib import PurePosixPath

DEFAULT_URL = (
    "https://uni-koeln.sciebo.de/public.php/dav/files/8s7cxAWA4PA5GZq/leaddbs_data.zip"
)
ATLAS_PREFIX = "templates/space/MNI152NLin2009bAsym/atlases/"
USER_AGENT = "dbs-localizer-maintainer/1 (+https://github.com/WyssCenter/dbs-localizer)"
CHUNK = 1 << 20


def request(url, headers=None, method="GET"):
    h = {"User-Agent": USER_AGENT}
    h.update(headers or {})
    return urllib.request.urlopen(
        urllib.request.Request(url, headers=h, method=method), timeout=120
    )


def read_range(url, start, end):
    with request(url, {"Range": f"bytes={start}-{end}"}) as r:
        if r.status != 206:
            raise SystemExit(f"range request answered {r.status}, expected 206")
        data = r.read()
    if len(data) != end - start + 1:
        raise SystemExit("short range response")
    return data


def central_directory(url, size):
    tail_len = min(size, 1 << 16)
    tail = read_range(url, size - tail_len, size - 1)
    i = tail.rfind(b"PK\x05\x06")
    if i < 0:
        raise SystemExit("end of central directory not found")
    _, _, _, _, n, cd_size, cd_off, _ = struct.unpack("<IHHHHIIH", tail[i : i + 22])
    j = tail.rfind(b"PK\x06\x06")
    if j >= 0:
        (_, _, _, _, _, _, _, n, cd_size, cd_off) = struct.unpack(
            "<IQHHIIQQQQ", tail[j : j + 56]
        )
    cd = read_range(url, cd_off, cd_off + cd_size - 1)
    entries = []
    p = 0
    while p < len(cd):
        (
            sig,
            _,
            _,
            flag,
            method,
            _,
            _,
            crc,
            csize,
            usize,
            fl,
            el,
            cl,
            _,
            _,
            _,
            offset,
        ) = struct.unpack("<IHHHHHHIIIHHHHHII", cd[p : p + 46])
        if sig != 0x02014B50:
            raise SystemExit("bad central directory entry")
        raw = cd[p + 46 : p + 46 + fl]
        if flag & 0x800:
            name = raw.decode("utf-8")
        else:
            try:
                name = raw.decode("ascii")
            except UnicodeDecodeError:
                raise SystemExit(f"non-ASCII name without the UTF-8 flag: {raw!r}")
        extra = cd[p + 46 + fl : p + 46 + fl + el]
        q = 0
        while q + 4 <= len(extra):
            hid, hsz = struct.unpack("<HH", extra[q : q + 4])
            if hid == 1:
                vals = extra[q + 4 : q + 4 + hsz]
                k = 0
                if usize == 0xFFFFFFFF:
                    usize = struct.unpack("<Q", vals[k : k + 8])[0]
                    k += 8
                if csize == 0xFFFFFFFF:
                    csize = struct.unpack("<Q", vals[k : k + 8])[0]
                    k += 8
                if offset == 0xFFFFFFFF:
                    offset = struct.unpack("<Q", vals[k : k + 8])[0]
                    k += 8
            q += 4 + hsz
        entries.append(
            {
                "name": name,
                "flag": flag,
                "method": method,
                "crc": crc,
                "csize": csize,
                "usize": usize,
                "offset": offset,
            }
        )
        p += 46 + fl + el + cl
    if len(entries) != n:
        raise SystemExit(
            f"central directory lists {len(entries)} entries, end record says {n}"
        )
    return entries, cd_off


class Stream:
    def __init__(self, url, validator):
        self.url = url
        self.validator = validator
        self.pos = 0
        self.sha1 = hashlib.sha1()
        self.resp = None
        self.retries = 0

    def _open(self):
        headers = {}
        if self.pos:
            headers = {"Range": f"bytes={self.pos}-", "If-Range": self.validator}
        resp = request(self.url, headers)
        if self.pos and resp.status != 206:
            resp.close()
            raise SystemExit("server did not resume; the file changed")
        self.resp = resp

    def _interrupted(self, reason):
        self.resp = None
        self.retries += 1
        if self.retries > 20:
            raise SystemExit(f"too many interrupted reads at byte {self.pos}")
        print(f"resume at byte {self.pos} ({reason})", flush=True)
        time.sleep(min(60, 5 * self.retries))

    def read(self, n):
        out = bytearray()
        while len(out) < n:
            if self.resp is None:
                try:
                    self._open()
                except urllib.error.HTTPError as e:
                    if e.code < 500 and e.code not in (408, 429):
                        raise SystemExit(
                            f"host answered HTTP {e.code} at byte {self.pos}"
                        )
                    self._interrupted(e)
                    continue
                except (OSError, http.client.HTTPException) as e:
                    self._interrupted(e)
                    continue
            try:
                b = self.resp.read(min(CHUNK, n - len(out)))
            except (OSError, http.client.HTTPException) as e:
                self._interrupted(e)
                continue
            if not b:
                self._interrupted("end of stream")
                continue
            out += b
            self.pos += len(b)
            self.sha1.update(b)
        return bytes(out)

    def skip_to(self, target):
        while self.pos < target:
            self.read(min(CHUNK, target - self.pos))


RESERVED = (
    {"CON", "PRN", "AUX", "NUL"}
    | {f"COM{i}" for i in range(1, 10)}
    | {f"LPT{i}" for i in range(1, 10)}
)
INSTALL_RECORD = ".dbsloc-install.json"


def unsafe_component(c):
    return (
        c in ("", ".", "..")
        or c.endswith((".", " "))
        or any(ch in '\\:<>"|?*' or ord(ch) < 32 or 127 <= ord(ch) <= 159 for ch in c)
        or c.split(".", 1)[0].upper() in RESERVED
    )


def safe_rel(name):
    parts = name[len(ATLAS_PREFIX) :].split("/")
    if (
        len(parts) < 2
        or parts[0].startswith(".")
        or any(unsafe_component(p) for p in parts)
    ):
        raise SystemExit(f"unsafe path in package: {name!r}")
    if parts[-1].lower() == INSTALL_RECORD:
        raise SystemExit(f"reserved file name in package: {name!r}")
    return parts[0], "/".join(parts[1:])


def main():
    ap = argparse.ArgumentParser(
        description="Pin SHA-256 values of every atlas file in the Lead-DBS data package."
    )
    ap.add_argument("out", help="manifest JSON to write")
    ap.add_argument("--url", default=DEFAULT_URL)
    args = ap.parse_args()

    with request(args.url, method="HEAD") as r:
        head = {k.lower(): v for k, v in r.headers.items()}
    size = int(head["content-length"])
    etag = head.get("etag", "")
    validator = (
        etag if etag and not etag.startswith("W/") else head.get("last-modified", "")
    )
    if not validator:
        raise SystemExit(
            "host sends neither a strong ETag nor Last-Modified; resuming would be unsafe"
        )
    checksum = head.get("oc-checksum", "")
    if not checksum.upper().startswith("SHA1:"):
        raise SystemExit(f"host publishes no SHA-1 (OC-Checksum: {checksum!r})")
    host_sha1 = checksum.split(":", 1)[1].lower()
    entries, cd_off = central_directory(args.url, size)
    print(f"{size} bytes, {len(entries)} entries, host SHA-1 {host_sha1}", flush=True)

    atlases = {}
    s = Stream(args.url, validator)
    t0 = time.time()
    for e in sorted(entries, key=lambda x: x["offset"]):
        s.skip_to(e["offset"])
        lh = s.read(30)
        sig, _, _, _, _, _, _, _, _, nl, xl = struct.unpack("<IHHHHHIIIHH", lh)
        if sig != 0x04034B50:
            raise SystemExit(f"bad local header for {e['name']}")
        s.read(nl + xl)
        is_atlas = e["name"].startswith(ATLAS_PREFIX) and not e["name"].endswith("/")
        if not is_atlas:
            continue
        if e["flag"] & 1:
            raise SystemExit(f"encrypted entry {e['name']}")
        if e["method"] not in (0, 8):
            raise SystemExit(f"compression method {e['method']} in {e['name']}")
        atlas, rel = safe_rel(e["name"])
        d = zlib.decompressobj(-15) if e["method"] == 8 else None
        h = hashlib.sha256()
        crc = 0
        n_out = 0
        left = e["csize"]
        while left:
            b = s.read(min(CHUNK, left))
            left -= len(b)
            o = d.decompress(b) if d else b
            h.update(o)
            crc = zlib.crc32(o, crc)
            n_out += len(o)
        if d:
            o = d.flush()
            h.update(o)
            crc = zlib.crc32(o, crc)
            n_out += len(o)
        if n_out != e["usize"] or crc != e["crc"]:
            raise SystemExit(f"size or CRC-32 mismatch in {e['name']}")
        atlases.setdefault(atlas, []).append(
            {"path": rel, "bytes": n_out, "sha256": h.hexdigest()}
        )
        mb = s.pos / 1e6
        print(
            f"{mb:9.1f} MB {mb / max(1e-9, time.time() - t0):6.1f} MB/s {e['name']}",
            flush=True,
        )
    s.skip_to(size)
    if s.pos != size:
        raise SystemExit("stream length differs from Content-Length")
    if s.sha1.hexdigest() != host_sha1:
        raise SystemExit(
            f"SHA-1 {s.sha1.hexdigest()} differs from the host value {host_sha1}"
        )
    for name, files in atlases.items():
        seen = set()
        for f in files:
            key = f["path"].lower()
            if key in seen:
                raise SystemExit(
                    f"two files of {name!r} differ only in case or repeat: {f['path']!r}"
                )
            seen.add(key)

    manifest = {
        "package": {
            "file_name": PurePosixPath(args.url).name,
            "url": args.url,
            "bytes": size,
            "sha1": host_sha1,
            "last_modified": head.get("last-modified", ""),
            "central_directory_offset": cd_off,
            "atlas_prefix": ATLAS_PREFIX,
            "pinned": time.strftime("%Y-%m-%d"),
        },
        "atlases": [
            {
                "name": name,
                "bytes": sum(f["bytes"] for f in files),
                "files": sorted(files, key=lambda f: f["path"]),
            }
            for name, files in sorted(atlases.items(), key=lambda kv: kv[0].encode())
        ],
    }
    with open(args.out, "w", encoding="utf-8", newline="\n") as f:
        json.dump(manifest, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(
        f"wrote {args.out}: {len(atlases)} atlases, {sum(len(v) for v in atlases.values())} files",
        flush=True,
    )


if __name__ == "__main__":
    sys.exit(main())
