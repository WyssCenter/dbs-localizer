# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Registration runs: every pair once with FastWarp (GPU) and once with ANTs, same recipe.

Layout under the work folder::

    environment.json                     versions, GPU, CPU of the latest run
    <dataset>/prep/<subject>/            fixed image on the registration grid, evaluation grid
    <dataset>/<recipe>/<fixed>__<moving>/{fastwarp,ants}/   reg_Composite.h5, run.json, ...

A pair whose engine folder holds a ``run.json`` with status ``ok`` and both composites is
skipped, so an interrupted run resumes. ANTs runs in a child process, one at a time; FastWarp
and ANTs never run at the same time. Both use all cores unless ``threads`` caps both (ITK
threads in the ANTs process's environment only, FastWarp's ``RAYON_NUM_THREADS``). Each
``run.json`` holds the registration's ``timings`` (linear, SyN, total; see :mod:`.timing`).
"""

from __future__ import annotations

import datetime as dt
import json
import math
import os
import platform
import re
import subprocess
import sys
import time
from collections.abc import Callable, Iterator, Mapping
from contextlib import AbstractContextManager, contextmanager
from pathlib import Path
from typing import Any

from . import datasets, recipes, timing

ENGINES = ("fastwarp", "ants")
#: File-name prefix of the transforms in an engine folder (``reg_Composite.h5``, ...).
PREFIX = "reg_"
ANTSPY_REGISTRATION = (
    "import sys, ants; sys.exit(int(ants.lib.antsRegistration(sys.argv[1:]) or 0))"
)
ITK_THREADS = "ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS"

#: Seconds used by ``plan`` until the work folder has measured ones (guesses, not
#: measurements): a one-channel registration with ANTs on the benchmark's cropped 1 mm grids on
#: all cores of a desktop and with FastWarp on a desktop GPU; the evaluation of a pair; the
#: linear fits of a pair (``linear_*``) and the post-processing of a pair (``post_*``), per
#: engine.
DEFAULT_SECONDS = {
    "ants": 400.0,
    "fastwarp": 40.0,
    "evaluate": 60.0,
    "linear_ants": 120.0,
    "linear_fastwarp": 20.0,
    "post_ants": 90.0,
    "post_fastwarp": 15.0,
}


#: The variables FastWarp's thread pool reads for its thread count.
RAYON_THREAD_VARS = ("RAYON_NUM_THREADS", "RAYON_RS_NUM_CPUS")


def itk_thread_vars(env: Mapping[str, str]) -> tuple[str, ...]:
    """The variables ITK 5 reads for its default thread count in ``env``:
    ``ITK_NUMBER_OF_THREADS_ENV_LIST`` itself, the names it lists (``NSLOTS`` when it is not
    set) and ``ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS``, which ITK reads last, so it wins."""
    listed = env.get("ITK_NUMBER_OF_THREADS_ENV_LIST")
    names = [n for n in (listed.split(":") if listed is not None else ["NSLOTS"]) if n]
    return tuple(dict.fromkeys(["ITK_NUMBER_OF_THREADS_ENV_LIST", *names, ITK_THREADS]))


def thread_env(env: Mapping[str, str] | None = None) -> dict[str, str]:
    """The thread-count variables of ITK and FastWarp set in ``env`` (default: this process),
    which the all-cores setting removes."""
    env = os.environ if env is None else env
    names = (*itk_thread_vars(env), *RAYON_THREAD_VARS)
    return {n: env[n] for n in names if n in env}


def _with_threads(env: dict[str, str], names: tuple[str, ...], var: str, threads: int | None):
    for n in names:
        env.pop(n, None)
    if threads is not None:
        env[var] = str(threads)


@contextmanager
def _swap_env(names: tuple[str, ...], var: str, threads: int | None) -> Iterator[None]:
    old = {n: os.environ[n] for n in names if n in os.environ}
    for n in names:
        os.environ.pop(n, None)
    if threads is not None:
        os.environ[var] = str(threads)
    try:
        yield
    finally:
        for n in names:
            os.environ.pop(n, None)
        os.environ.update(old)


def itk_threads(threads: int | None) -> AbstractContextManager[None]:
    """A context in which in-process ANTsPy calls use ``threads`` ITK threads (``None``: no
    cap, all cores: every variable ITK reads for the count is removed); the previous variables
    are restored after."""
    return _swap_env((*itk_thread_vars(os.environ), ITK_THREADS), ITK_THREADS, threads)


def rayon_threads(threads: int | None) -> AbstractContextManager[None]:
    """A context in which FastWarp's CPU work uses ``threads`` threads (``RAYON_NUM_THREADS``;
    ``None``: no cap, all cores); the previous values are restored after. FastWarp reads it
    once, when its thread pool starts (opening the GPU starts it), so it takes effect only if
    nothing in this process used FastWarp before; the commands enter it before anything else."""
    return _swap_env(RAYON_THREAD_VARS, "RAYON_NUM_THREADS", threads)


def child_env(threads: int | None) -> dict[str, str]:
    """The environment of an ANTs child process: this one's, without any variable ITK reads
    for its thread count, with ``ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS=threads`` when
    ``threads`` is given (``None``: all cores), and without ``ANTS_RANDOM_SEED``."""
    env = dict(os.environ)
    _with_threads(env, itk_thread_vars(env), ITK_THREADS, threads)
    env.pop("ANTS_RANDOM_SEED", None)
    return env


def thread_setting(threads: int | None) -> int | str:
    """What a record states about the CPU threads: ``threads``, or ``"all cores"``."""
    return "all cores" if threads is None else int(threads)


def natural_key(s: str) -> list:
    """Sort key that orders ``sub-2`` before ``sub-10``."""
    return [int(t) if t.isdigit() else t for t in re.split(r"(\d+)", s)]


def select_pairs(ds: datasets.Dataset, n: int) -> list[tuple[str, str]]:
    """The first ``n`` ``(fixed, moving)`` pairs of the dataset's protocol.

    ``template``: the template is fixed, the subjects (in natural order) move. ``pairwise``:
    a ring, subject ``i`` fixed and subject ``i + k`` moving, for ``k = 1, 2, ...`` in turn,
    so every subject is fixed and moving equally often within each round.
    """
    ids = sorted((s.id for s in ds.subjects), key=natural_key)
    if ds.protocol == "template":
        if ds.template is None:
            raise ValueError(f"{ds.key}: the template protocol needs a template")
        return [(ds.template.id, sid) for sid in ids[:n]]
    if ds.protocol != "pairwise":
        raise ValueError(f"{ds.key}: unknown protocol {ds.protocol!r}")
    out: list[tuple[str, str]] = []
    m = len(ids)
    for k in range(1, m):
        for i in range(m):
            if len(out) >= n:
                return out
            out.append((ids[i], ids[(i + k) % m]))
    return out


def pair_id(fixed: str, moving: str) -> str:
    """Folder name of a pair."""
    return f"{fixed}__{moving}"


def _record(folder: Path) -> dict[str, Any] | None:
    meta = folder / "run.json"
    if not meta.is_file():
        return None
    try:
        return json.loads(meta.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None


def same_config(record: dict[str, Any] | None, rec: dict | None, iso_mm: float | None) -> bool:
    """Whether a ``run.json`` was made with ``rec`` and ``iso_mm`` (``None`` matches any)."""
    if record is None:
        return False
    if rec is not None and record.get("recipe") != rec:
        return False
    return iso_mm is None or record.get("iso_mm") == iso_mm


def engine_done(folder: Path, rec: dict | None = None, iso_mm: float | None = None) -> bool:
    """Whether an engine folder holds a successful run (of ``rec`` at ``iso_mm``, if given)."""
    record = _record(folder)
    return (
        record is not None
        and record.get("status") == "ok"
        and same_config(record, rec, iso_mm)
        and composite(folder).is_file()
        and inverse_composite(folder).is_file()
    )


def composite(folder: Path) -> Path:
    """The forward composite transform of an engine folder."""
    return folder / f"{PREFIX}Composite.h5"


def inverse_composite(folder: Path) -> Path:
    """The inverse composite transform of an engine folder."""
    return folder / f"{PREFIX}InverseComposite.h5"


def engine_status(folder: Path) -> str | None:
    """The status recorded in an engine folder's ``run.json``, if any."""
    record = _record(folder)
    return None if record is None else record.get("status")


def _write_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(data, indent=1, default=str), encoding="utf-8")
    os.replace(tmp, path)


def _now() -> str:
    return dt.datetime.now(dt.UTC).isoformat(timespec="seconds")


def _cpu_name() -> str:
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    if sys.platform == "darwin":
        try:
            return subprocess.run(
                ["sysctl", "-n", "machdep.cpu.brand_string"],
                capture_output=True,
                text=True,
                check=True,
                timeout=10,
            ).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            pass
    return platform.processor() or platform.machine()


def physical_cores_in_cpuinfo(text: str) -> int | None:
    """The number of distinct ``(physical id, core id)`` pairs of a Linux ``/proc/cpuinfo``."""
    pairs = set()
    phys = core = None
    for line in [*text.splitlines(), ""]:
        key, _, value = line.partition(":")
        key = key.strip()
        if key == "physical id":
            phys = value.strip()
        elif key == "core id":
            core = value.strip()
        elif not line.strip():
            if core is not None:
                pairs.add((phys, core))
            phys = core = None
    return len(pairs) or None


def _physical_cores() -> int | None:
    try:
        found = physical_cores_in_cpuinfo(Path("/proc/cpuinfo").read_text())
        if found:
            return found
    except OSError:
        pass
    if sys.platform == "darwin":
        try:
            out = subprocess.run(
                ["sysctl", "-n", "hw.physicalcpu"],
                capture_output=True,
                text=True,
                check=True,
                timeout=10,
            )
            return int(out.stdout.strip())
        except (OSError, subprocess.SubprocessError, ValueError):
            pass
    return None


def _git(*args: str) -> str | None:
    root = Path(__file__).resolve().parents[2]
    try:
        out = subprocess.run(
            ["git", "-C", str(root), *args], capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


def ants_tool(ants_dir: Path | None, name: str, antspy_code: str) -> list[str]:
    """The command that runs the ANTs program ``name``: the program in ``ants_dir``, or
    ANTsPy's bundled one through this Python (``antspy_code``)."""
    if ants_dir is None:
        return [sys.executable, "-I", "-c", antspy_code]
    exe = f"{name}.exe" if sys.platform == "win32" else name
    path = Path(ants_dir) / exe
    if not path.is_file():
        raise FileNotFoundError(f"{path} does not exist")
    return [str(path)]


def ants_program(ants_dir: Path | None) -> list[str]:
    """The command that runs ``antsRegistration``: the program in ``ants_dir``, or ANTsPy's
    bundled one through this Python."""
    return ants_tool(ants_dir, "antsRegistration", ANTSPY_REGISTRATION)


def run_child(
    cmd: list[str], log: Path, env: dict[str, str], timeout_s: float | None
) -> tuple[int | None, float]:
    """Run ``cmd`` with its output in ``log``; returns the exit code (``None`` when it timed
    out) and the wall time in seconds."""
    t0 = time.perf_counter()
    try:
        with open(log, "w", encoding="utf-8") as f:
            proc = subprocess.run(
                cmd, stdout=f, stderr=subprocess.STDOUT, env=env, timeout=timeout_s, check=False
            )
    except subprocess.TimeoutExpired:
        return None, time.perf_counter() - t0
    return proc.returncode, time.perf_counter() - t0


def _ants_version(ants_dir: Path | None) -> str | None:
    try:
        out = subprocess.run(
            [*ants_program(ants_dir), "--version"], capture_output=True, text=True, timeout=120
        )
    except (OSError, subprocess.SubprocessError) as e:
        return f"error: {e}"
    text = out.stdout + out.stderr
    for line in text.splitlines():
        if line.startswith("ANTs Version:"):
            return line.split(":", 1)[1].strip()
    return text.strip() or None


def environment(
    *,
    ants_dir: Path | None,
    threads: int | None,
    adapter: str | None,
    caller_env: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    """Versions and hardware of this run, with the thread setting (``threads``: a cap on both
    engines' CPU threads, ``None``: all cores) and the thread-count variables the caller had
    set (``caller_env``, which the setting replaced)."""
    import numpy

    dirty = _git("status", "--porcelain", "--untracked-files=no")
    env: dict[str, Any] = {
        "recorded": _now(),
        "benchmark_commit": _git("rev-parse", "HEAD"),
        "benchmark_tree_dirty": None if dirty is None else bool(dirty),
        "platform": platform.platform(),
        "python": sys.version.split()[0],
        "cpu": _cpu_name(),
        "cpu_count": os.cpu_count(),
        "cpu_count_physical": _physical_cores(),
        "threads": thread_setting(threads),
        "itk_threads": threads,
        "rayon_threads": os.environ.get("RAYON_NUM_THREADS"),
        "thread_env_of_caller": thread_env() if caller_env is None else dict(caller_env),
        "fastwarp_env": {k: v for k, v in sorted(os.environ.items()) if k.startswith("FASTWARP_")},
        "numpy": numpy.__version__,
    }
    try:
        import scipy

        env["scipy"] = scipy.__version__
    except ImportError:
        env["scipy"] = None
    try:
        import fastwarp

        env["fastwarp"] = fastwarp.__version__
        env["fastwarp_engine_source"] = fastwarp.ENGINE_SOURCE
        try:
            env["gpu"] = fastwarp.gpu_info(adapter)
        except fastwarp.GpuUnavailableError as e:
            env["gpu"] = {"error": str(e)}
    except ImportError as e:
        env["fastwarp"] = None
        env["fastwarp_error"] = str(e)
    try:
        import ants

        env["antspyx"] = ants.__version__
    except ImportError:
        env["antspyx"] = None
    if ants_dir is not None:
        env["ants_program"] = str(Path(ants_dir).resolve())
    else:
        env["ants_program"] = "ANTsPy's bundled antsRegistration"
    env["ants_version"] = _ants_version(ants_dir)
    return env


class RunLock:
    """A lock file so that two runs never share a work folder (and never run ANTs at once)."""

    def __init__(self, work: Path) -> None:
        self.path = Path(work) / ".run.lock"

    def __enter__(self) -> RunLock:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        try:
            fd = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
        except FileExistsError:
            raise RuntimeError(
                f"{self.path} exists: another run uses this work folder. Delete the file if "
                "no run is active."
            ) from None
        with os.fdopen(fd, "w") as f:
            f.write(f"{os.getpid()} {_now()}\n")
        return self

    def __exit__(self, *args: object) -> None:
        self.path.unlink(missing_ok=True)


def _crop_box(mask, margin: int) -> tuple[list[int], list[int]]:
    import numpy as np

    idx = np.nonzero(mask)
    if not idx[0].size:
        raise ValueError("the foreground mask is empty")
    lo = [max(int(i.min()) - margin, 0) for i in idx]
    hi = [min(int(i.max()) + 1 + margin, n) for i, n in zip(idx, mask.shape, strict=True)]
    return lo, hi


def _same_grid(a, b) -> bool:
    import numpy as np

    return (
        tuple(a.shape) == tuple(b.shape)
        and np.allclose(a.spacing, b.spacing, atol=1e-4)
        and np.allclose(a.origin, b.origin, atol=1e-3)
        and np.allclose(a.direction, b.direction, atol=1e-4)
    )


def _on_grid(img, iso_mm: float) -> bool:
    import numpy as np

    return bool(np.all(np.abs(np.asarray(img.spacing, dtype=float) - iso_mm) < 1e-6))


def _read_json(path: Path) -> dict[str, Any]:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def _foreground(img):
    import numpy as np

    a = img.numpy()
    positive = a[a > 0]
    if not positive.size:
        return a > 0
    return a > 0.1 * float(np.percentile(positive, 99))


def prep_folder(ds: datasets.Dataset, sid: str, work: Path, iso_mm: float) -> Path:
    """Folder of a subject's prepared images for one registration grid."""
    return work / ds.key / "prep" / f"iso{iso_mm:g}mm" / sid


def prepare_moving(ds: datasets.Dataset, sid: str, data: Path, work: Path, iso_mm: float) -> Path:
    """The moving image both engines register: the subject's image, resampled linearly to an
    ``iso_mm`` grid (cached) when its spacing differs, as the fixed image is. FastWarp would
    otherwise resample it internally while ANTs samples the native image."""
    import ants

    source = data / ds.key / ds.subject(sid).image
    target = prep_folder(ds, sid, work, iso_mm) / "moving_registration.nii.gz"
    if target.is_file():
        return target
    img = ants.image_read(str(source))
    if _on_grid(img, iso_mm):
        return source
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = target.with_name("tmp_" + target.name)
    ants.image_write(
        ants.resample_image(img, (iso_mm,) * 3, use_voxels=False, interp_type=0).clone("float"),
        str(tmp),
    )
    os.replace(tmp, target)
    return target


def prepare_fixed(
    ds: datasets.Dataset, sid: str, data: Path, work: Path, iso_mm: float
) -> dict[str, Any]:
    """Write the fixed subject's registration image, evaluation grid and mask (cached).

    The registration image is the subject's image, cropped to its foreground plus 10 voxels
    for skull-stripped datasets, resampled linearly to an ``iso_mm`` grid when its spacing
    differs. Both engines register onto this one file. The evaluation grid is the cropped
    native grid: the label map for label datasets, the image itself otherwise (with one
    cropped file per structure mask, if the subject has them).
    """
    import ants

    folder = prep_folder(ds, sid, work, iso_mm)
    meta_path = folder / "prep.json"
    if meta_path.is_file():
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        if meta.get("iso_mm") == iso_mm and all(Path(p).is_file() for p in meta["files"].values()):
            return meta
    subj = ds.subject(sid)
    img = ants.image_read(str(data / ds.key / subj.image))
    labels = ants.image_read(str(data / ds.key / subj.labels)) if subj.labels else None
    mask = ants.image_read(str(data / ds.key / subj.mask)) if subj.mask else None
    structures = {label: ants.image_read(str(data / ds.key / rel)) for label, rel in subj.masks}
    others = [(labels, "labels"), (mask, "mask")]
    others += [(m, f"structure mask {label}") for label, m in structures.items()]
    for other, what in others:
        if other is not None and not _same_grid(img, other):
            raise ValueError(f"{ds.key}/{sid}: the {what} are not on the image's grid")
    mask_arr = mask.numpy() > 0 if mask is not None else _foreground(img)
    if ds.crop_to_foreground:
        lo, hi = _crop_box(img.numpy() > 0, 10)
    else:
        lo, hi = [0, 0, 0], list(img.shape)
    crop = (lo != [0, 0, 0]) or (hi != list(img.shape))

    def cut(im):
        return ants.crop_indices(im, lo, hi) if crop else im

    img_c = cut(img)
    ref = cut(labels) if labels is not None else img_c
    mask_img = img_c.new_image_like(
        mask_arr[lo[0] : hi[0], lo[1] : hi[1], lo[2] : hi[2]].astype("float32")
    )
    if _on_grid(img_c, iso_mm):
        reg = img_c
    else:
        reg = ants.resample_image(img_c, (iso_mm,) * 3, use_voxels=False, interp_type=0)
    folder.mkdir(parents=True, exist_ok=True)
    files = {
        "registration": folder / "fixed_registration.nii.gz",
        "reference": folder
        / ("reference_labels.nii.gz" if labels is not None else "reference.nii.gz"),
        "mask": folder / "mask.nii.gz",
    }
    ants.image_write(reg.clone("float"), str(files["registration"]))
    ants.image_write(ref, str(files["reference"]))
    ants.image_write(mask_img, str(files["mask"]))
    for label, m in structures.items():
        files[f"structure_{label}"] = folder / f"structure_{label}.nii.gz"
        ants.image_write(cut(m), str(files[f"structure_{label}"]))
    meta = {
        "subject": sid,
        "iso_mm": iso_mm,
        "crop_lower": lo,
        "crop_upper": hi,
        "registration_shape": list(reg.shape),
        "files": {k: str(v) for k, v in files.items()},
    }
    _write_json(meta_path, meta)
    return meta


def run_fastwarp(
    fixed: Path, moving: Path, out: Path, rec: dict, *, iso_mm: float, adapter: str | None
) -> dict[str, Any]:
    """One FastWarp registration; returns the ``run.json`` content."""
    import ants
    import fastwarp

    record: dict[str, Any] = {
        "engine": "fastwarp",
        "started": _now(),
        "recipe": rec,
        "iso_mm": iso_mm,
        "fixed_image": str(fixed),
        "moving_image": str(moving),
    }
    t_read = time.perf_counter()
    fixed_img = ants.image_read(str(fixed))
    moving_img = ants.image_read(str(moving))
    record["read_s"] = time.perf_counter() - t_read
    if not fastwarp.fits_gpu(fixed_img, moving_img, iso_mm=iso_mm, adapter=adapter):
        record.update(status="skipped", reason="no GPU, or the grid does not fit the GPU")
        return record
    t0 = time.perf_counter()
    try:
        info = fastwarp.register_syn(
            fixed_img,
            moving_img,
            (out / PREFIX).as_posix(),
            iso_mm=iso_mm,
            adapter=adapter,
            recipe=rec,
        )
    except (fastwarp.GpuUnavailableError, fastwarp.GpuCapacityError) as e:
        record.update(status="skipped", reason=str(e))
        return record
    except Exception as e:
        record.update(status="failed", error=f"{type(e).__name__}: {e}")
        return record
    record["wall_s"] = time.perf_counter() - t0
    record["finished"] = _now()
    record["info"] = info
    record["timings"] = timing.fastwarp_registration_timings(info)
    record["recipe_matches"] = info.get("recipe") == rec
    record["status"] = "ok" if record["recipe_matches"] else "failed"
    if not record["recipe_matches"]:
        record["error"] = "FastWarp reports a recipe other than the one requested"
    return record


def run_ants(
    fixed: Path,
    moving: Path,
    out: Path,
    rec: dict,
    *,
    threads: int | None,
    ants_dir: Path | None,
    timeout_s: float | None,
    iso_mm: float,
) -> dict[str, Any]:
    """One ``antsRegistration`` in a child process; returns the ``run.json`` content, with the
    stage times read from its log (:func:`timing.ants_registration_timings`)."""
    argv = recipes.ants_argv(rec, fixed.as_posix(), moving.as_posix(), (out / PREFIX).as_posix())
    cmd = [*ants_program(ants_dir), *argv]
    env = child_env(threads)
    record: dict[str, Any] = {
        "engine": "ants",
        "started": _now(),
        "recipe": rec,
        "iso_mm": iso_mm,
        "argv": argv,
        "program": "ANTsPy" if ants_dir is None else cmd[0],
        "itk_threads": threads,
    }
    log = out / "ants.log"
    code, wall = run_child(cmd, log, env, timeout_s)
    if code is None:
        record.update(status="failed", error=f"timed out after {timeout_s} s")
        return record
    record["wall_s"] = wall
    record["finished"] = _now()
    record["exit_code"] = code
    record["timings"] = timing.ants_registration_timings(
        log.read_text(encoding="utf-8", errors="replace"), n_stages=3
    )
    ok = code == 0 and composite(out).is_file() and inverse_composite(out).is_file()
    record["status"] = "ok" if ok else "failed"
    if not ok:
        record["error"] = f"antsRegistration exited with {code}; see {log.name}"
    return record


def _row(dataset: str, pair: str, engine: str, status: str | None, ran: bool) -> dict:
    return {"dataset": dataset, "pair": pair, "engine": engine, "status": status, "ran": ran}


def run(
    *,
    data: Path,
    work: Path,
    keys: list[str],
    recipe_name: str = "cohort",
    pairs: int | None = None,
    quick: bool = False,
    engines: tuple[str, ...] = ENGINES,
    iso_mm: float = 1.0,
    threads: int | None = None,
    ants_dir: Path | None = None,
    adapter: str | None = None,
    retry_failed: bool = False,
    ants_timeout_s: float | None = 4 * 3600.0,
    log: Callable[[str], None] = print,
) -> list[dict[str, Any]]:
    """Register every selected pair with every engine not done yet; returns one row per
    pair and engine with its status. ``threads`` caps the CPU threads of both engines (``None``:
    all cores)."""
    for e in engines:
        if e not in ENGINES:
            raise ValueError(f"unknown engine {e!r}")
    rec = recipes.recipe(recipe_name)
    rows = []
    caller = thread_env()
    with RunLock(work), rayon_threads(threads):
        env = environment(ants_dir=ants_dir, threads=threads, adapter=adapter, caller_env=caller)
        _write_json(work / "environment.json", env)
        log(f"fastwarp {env.get('fastwarp')}, ANTsPy {env.get('antspyx')}, GPU {env.get('gpu')}")
        for key in keys:
            ds = datasets.get(key)
            if not (data / ds.key / ".fetched.json").is_file():
                raise RuntimeError(f"{ds.key} is not fetched into {data}; run fetch first")
            n = pairs if pairs is not None else (ds.quick_pairs if quick else ds.default_pairs)
            selected = select_pairs(ds, n)
            log(f"{ds.key}: {len(selected)} pairs, recipe {recipe_name}")
            for i, (fixed_id, moving_id) in enumerate(selected, 1):
                folder = work / ds.key / recipe_name / pair_id(fixed_id, moving_id)

                todo = []
                for engine in engines:
                    out = folder / engine
                    record = _record(out)
                    same = same_config(record, rec, iso_mm)
                    status = None if record is None else record.get("status")
                    skip_failed = same and status in ("failed", "skipped") and not retry_failed
                    if engine_done(out, rec, iso_mm) or skip_failed:
                        rows.append(_row(ds.key, folder.name, engine, status, False))
                    else:
                        todo.append(engine)
                pair_meta = _read_json(folder / "pair.json")
                stale = pair_meta.get("recipe") != rec or pair_meta.get("iso_mm") != iso_mm
                if not todo and not stale:
                    continue
                try:
                    with itk_threads(threads):
                        prep = prepare_fixed(ds, fixed_id, data, work, iso_mm)
                        moving = prepare_moving(ds, moving_id, data, work, iso_mm)
                except Exception as e:
                    error = f"preparing the images failed: {type(e).__name__}: {e}"
                    log(f"[{i}/{len(selected)}] {folder.name}: {error}")
                    for engine in todo:
                        _write_json(
                            folder / engine / "run.json",
                            {
                                "engine": engine,
                                "status": "failed",
                                "error": error,
                                "recipe": rec,
                                "iso_mm": iso_mm,
                                "environment": env,
                            },
                        )
                        rows.append(_row(ds.key, folder.name, engine, "failed", True))
                    continue
                fixed = Path(prep["files"]["registration"])
                _write_json(
                    folder / "pair.json",
                    {
                        "dataset": ds.key,
                        "fixed": fixed_id,
                        "moving": moving_id,
                        "recipe_name": recipe_name,
                        "recipe": rec,
                        "iso_mm": iso_mm,
                        "fixed_prep": prep,
                        "moving_registration": str(moving),
                        "environment": env,
                    },
                )
                for engine in todo:
                    out = folder / engine
                    out.mkdir(parents=True, exist_ok=True)
                    log(f"[{i}/{len(selected)}] {folder.name} {engine} ...")
                    try:
                        if engine == "fastwarp":
                            with itk_threads(threads):
                                record = run_fastwarp(
                                    fixed, moving, out, rec, iso_mm=iso_mm, adapter=adapter
                                )
                        else:
                            record = run_ants(
                                fixed,
                                moving,
                                out,
                                rec,
                                threads=threads,
                                ants_dir=ants_dir,
                                timeout_s=ants_timeout_s,
                                iso_mm=iso_mm,
                            )
                    except Exception as e:
                        record = {
                            "engine": engine,
                            "status": "failed",
                            "recipe": rec,
                            "iso_mm": iso_mm,
                            "error": f"{type(e).__name__}: {e}",
                        }
                    record["environment"] = env
                    _write_json(out / "run.json", record)
                    wall = record.get("wall_s", math.nan)
                    log(f"    {record['status']} in {wall:.1f} s")
                    rows.append(_row(ds.key, folder.name, engine, record["status"], True))
    return rows


def measured_seconds(work: Path) -> dict[str, float]:
    """Median seconds per registration, evaluated pair, linear run and post-processed pair
    (per engine) found in a work folder, falling back to :data:`DEFAULT_SECONDS`."""
    import statistics

    found: dict[str, list[float]] = {k: [] for k in DEFAULT_SECONDS}
    if work.is_dir():
        for meta in work.glob("*/*/*/*/run.json"):
            try:
                rec = json.loads(meta.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                continue
            kind = rec.get("kind", "registration")
            key = rec.get("engine") if kind == "registration" else f"{kind}_{rec.get('engine')}"
            if rec.get("status") == "ok" and key in found and "wall_s" in rec:
                found[key].append(float(rec["wall_s"]))
        for meta in work.glob("*/*/*/post.json"):
            try:
                rec = json.loads(meta.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                continue
            if rec.get("status") == "ok" and (rec.get("config") or {}).get("outputs") == ["nii"]:
                for engine in ENGINES:
                    wall = (rec.get(engine) or {}).get("wall_s")
                    if wall is not None:
                        found[f"post_{engine}"].append(float(wall))
        for meta in work.glob("*/*/*/evaluation.json"):
            try:
                rec = json.loads(meta.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                continue
            if "wall_s" in rec:
                found["evaluate"].append(float(rec["wall_s"]))
    return {k: statistics.median(v) if v else DEFAULT_SECONDS[k] for k, v in found.items()}


def plan(
    *,
    work: Path,
    keys: list[str],
    recipe_name: str,
    pairs: int | None,
    quick: bool,
    engines: tuple[str, ...] = ENGINES,
    iso_mm: float = 1.0,
    linear_keys: list[str] | None = None,
    post: bool = True,
) -> dict[str, Any]:
    """Pairs still to run per dataset and an estimate of the time, from the medians measured
    in ``work`` or :data:`DEFAULT_SECONDS`: registrations plus evaluation (``datasets``), the
    post-processing of the same pairs (``post``, unless ``post`` is false) and the linear fits
    on ``linear_keys`` (``linear``)."""
    from . import linear as linear_step
    from . import post as post_step

    secs = measured_seconds(work)
    out: dict[str, Any] = {
        "seconds_per": secs,
        "datasets": {},
        "post": {},
        "linear": {},
        "total_s": 0.0,
    }
    rec = recipes.recipe(recipe_name)
    for key in keys:
        ds = datasets.get(key)
        n = pairs if pairs is not None else (ds.quick_pairs if quick else ds.default_pairs)
        selected = select_pairs(ds, n)
        todo = {e: 0 for e in engines}
        post_left = 0
        for fixed_id, moving_id in selected:
            folder = work / ds.key / recipe_name / pair_id(fixed_id, moving_id)
            for e in engines:
                if not engine_done(folder / e, rec, iso_mm):
                    todo[e] += 1
            if not any(
                post_step.post_done(folder, post_step.config(rec, iso_mm, gzip))
                for gzip in (False, True)
            ):
                post_left += 1
        est = sum(todo[e] * secs[e] for e in engines) + len(selected) * secs["evaluate"]
        out["datasets"][key] = {"pairs": len(selected), "to_run": todo, "estimate_s": est}
        out["total_s"] += est
        if post:
            est = post_left * (secs["post_ants"] + secs["post_fastwarp"])
            out["post"][key] = {"pairs": len(selected), "to_run": post_left, "estimate_s": est}
            out["total_s"] += est
    left = linear_step.todo_count(work, linear_keys or [], recipe_name, pairs, quick, iso_mm)
    for key, d in left.items():
        est = sum(d["to_run"][e] * secs[f"linear_{e}"] for e in ENGINES)
        out["linear"][key] = {**d, "estimate_s": est}
        out["total_s"] += est
    return out
