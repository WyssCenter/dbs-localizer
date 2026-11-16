# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""benchmarks.accuracy: timings (ANTs log parsing, summaries), the linear and post-processing
steps with the engines replaced by stubs, and the report's timing tables. No GPU, no ANTs."""

from __future__ import annotations

import json
import math
import os
from pathlib import Path

import numpy as np
import pytest

from benchmarks.accuracy import datasets, linear, post, recipes, report, run, timing

SNSX_PAIRS = [run.pair_id(f, m) for f, m in run.select_pairs(datasets.get("snsx"), 3)]

ANTS_LOG = """\
All_Command_lines_OK
Using double precision for computations.
Registration using 3 total stages.

Stage 0
  iterations = 20x10
  using the Mattes MI metric (number of bins = 32, weight = 1, use gradient filter = 0)
*** Running Euler3DTransform registration ***

DIAGNOSTIC,Iteration,metricValue,convergenceValue,ITERATION_TIME_INDEX,SINCE_LAST
 2DIAGNOSTIC,    10, -8.590278115811e-01, 4.906169283285e-05, 1.0168e-01, 7.7846e-03,
  Elapsed time (stage 0): 1.0627e-01


Stage 1
*** Running AffineTransform registration ***

 2DIAGNOSTIC,    10, -9.312382732571e-01, 5.570771786045e-05, 1.4132e-01, 9.8460e-03,
  Elapsed time (stage 1): 1.4739e-01


Stage 2
  using the CC metric (radius = 2, weight = 1.0000e+00, use gradient filter = 0)
     Total field sigma (voxel space) = 0
*** Running SyN registration (varianceForUpdateField = 3.0000e+00, varianceForTotalField = \
0.0000e+00) ***

XXDIAGNOSTIC,Iteration,metricValue,convergenceValue,ITERATION_TIME_INDEX,SINCE_LAST
 1DIAGNOSTIC,     3, -9.210115372501e-01, 1.797693134862e+308, 1.0066e+00, 2.6965e-01,
  Elapsed time (stage 2): 1.017


Total elapsed time: 1.276
"""


def test_the_ants_log_gives_the_stage_times_and_the_total():
    parsed = timing.parse_ants_log(ANTS_LOG.replace("\n", "\r\n"))
    assert [(s["stage"], s["transform"]) for s in parsed["stages"]] == [
        (0, "Euler3DTransform"),
        (1, "AffineTransform"),
        (2, "SyN"),
    ]
    assert [s["elapsed_s"] for s in parsed["stages"]] == [0.10627, 0.14739, 1.017]
    assert parsed["total_s"] == 1.276
    t = timing.ants_registration_timings(ANTS_LOG, n_stages=3)
    assert t["linear_s"] == pytest.approx(0.10627 + 0.14739)
    assert t["syn_s"] == pytest.approx(1.017)
    assert t["total_s"] == 1.276 and "note" not in t


def test_an_incomplete_ants_log_gives_no_split_and_says_why():
    plain = timing.ants_registration_timings("All_Command_lines_OK\n", n_stages=3)
    assert plain["linear_s"] is None and plain["syn_s"] is None and plain["total_s"] is None
    assert "not a --verbose 1 log" in plain["note"]
    cut = ANTS_LOG[: ANTS_LOG.index("Stage 2")]
    t = timing.ants_registration_timings(cut, n_stages=3)
    assert t["linear_s"] is None and "2 stage times for 3 stages" in t["note"]
    no_total = ANTS_LOG.replace("Total elapsed time: 1.276", "")
    t = timing.ants_registration_timings(no_total, n_stages=3)
    assert t["syn_s"] == pytest.approx(1.017) and t["total_s"] is None
    assert "Total elapsed time" in t["note"]
    bad = ANTS_LOG.replace("(stage 1): 1.4739e-01", "(stage 1): nan")
    assert timing.ants_registration_timings(bad, n_stages=3)["linear_s"] is None


def test_an_unknown_stage_transform_gives_no_split():
    log = ANTS_LOG.replace("AffineTransform", "SomethingNew")
    t = timing.ants_registration_timings(log, n_stages=3)
    assert t["linear_s"] is None and t["syn_s"] is None and t["total_s"] == 1.276
    assert "transform" in t["note"]
    assert timing.is_linear("SyN") is False and timing.is_linear("Euler3DTransform")


def test_one_stage_logs_count_as_linear():
    log = "*** Running Euler3DTransform registration ***\n  Elapsed time (stage 0): 2.5\n"
    t = timing.ants_registration_timings(log + "Total elapsed time: 2.6\n", n_stages=1)
    assert t["linear_s"] == 2.5 and t["syn_s"] is None and t["total_s"] == 2.6


def test_registration_timings_fall_back_to_the_recorded_info_and_log(tmp_path):
    fw = {"engine": "fastwarp", "wall_s": 9.0, "info": {"linear_s": 1, "syn_s": 5.5}}
    t = timing.registration_timings(fw)
    assert (t["linear_s"], t["syn_s"], t["total_s"], t["wall_s"]) == (1.0, 5.5, None, 9.0)
    (tmp_path / "ants.log").write_text(ANTS_LOG)
    t = timing.registration_timings({"engine": "ants", "wall_s": 2.0}, tmp_path)
    assert t["syn_s"] == pytest.approx(1.017) and t["wall_s"] == 2.0
    stored = {"engine": "ants", "timings": {"linear_s": 3.0, "syn_s": 4.0, "total_s": 7.5}}
    assert timing.registration_timings(stored)["total_s"] == 7.5
    missing = timing.registration_timings({"engine": "ants"}, tmp_path / "nothing")
    assert missing["total_s"] is None


def test_describe_and_spread_ignore_missing_values():
    d = timing.describe([4.0, None, 1.0, math.nan, 2.0, 3.0, "x"])
    assert d["n"] == 4 and d["median"] == 2.5 and d["min"] == 1.0 and d["max"] == 4.0
    assert (d["q1"], d["q3"]) == (1.75, 3.25)
    assert timing.spread(d) == "2.5 (1.8–3.2)"
    assert timing.spread(timing.describe([7.0])) == "7.0"
    assert timing.spread(timing.describe([])) == "n/a"
    assert math.isnan(timing.ratio(1.0, 0.0)) and timing.ratio(6.0, 2.0) == 3.0


def test_itk_threads_is_restored_and_child_env_carries_it(monkeypatch):
    monkeypatch.setenv(run.ITK_THREADS, "3")
    monkeypatch.setenv("ANTS_RANDOM_SEED", "1")
    with run.itk_threads(5):
        assert os.environ[run.ITK_THREADS] == "5"
    assert os.environ[run.ITK_THREADS] == "3"
    monkeypatch.delenv(run.ITK_THREADS)
    with run.itk_threads(5):
        pass
    assert run.ITK_THREADS not in os.environ
    monkeypatch.setenv(run.ITK_THREADS, "7")
    with run.itk_threads(None):
        assert run.ITK_THREADS not in os.environ
    assert os.environ[run.ITK_THREADS] == "7"
    monkeypatch.delenv(run.ITK_THREADS)
    env = run.child_env(4)
    assert env[run.ITK_THREADS] == "4" and "ANTS_RANDOM_SEED" not in env
    assert run.ITK_THREADS not in os.environ


def test_all_cores_removes_every_thread_count_variable(monkeypatch):
    for name in ("ITK_NUMBER_OF_THREADS_ENV_LIST", "RAYON_NUM_THREADS"):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("NSLOTS", "2")
    monkeypatch.setenv(run.ITK_THREADS, "3")
    monkeypatch.setenv("RAYON_RS_NUM_CPUS", "4")
    assert run.thread_env() == {"NSLOTS": "2", run.ITK_THREADS: "3", "RAYON_RS_NUM_CPUS": "4"}
    env = run.child_env(None)
    assert "NSLOTS" not in env and run.ITK_THREADS not in env
    assert run.child_env(5)[run.ITK_THREADS] == "5" and "NSLOTS" not in run.child_env(5)
    with run.itk_threads(None), run.rayon_threads(None):
        assert run.thread_env() == {}
    assert run.thread_env() == {"NSLOTS": "2", run.ITK_THREADS: "3", "RAYON_RS_NUM_CPUS": "4"}
    monkeypatch.setenv("ITK_NUMBER_OF_THREADS_ENV_LIST", "MY_SLOTS:")
    monkeypatch.setenv("MY_SLOTS", "6")
    assert run.itk_thread_vars(os.environ) == (
        "ITK_NUMBER_OF_THREADS_ENV_LIST",
        "MY_SLOTS",
        run.ITK_THREADS,
    )
    env = run.child_env(None)
    assert "MY_SLOTS" not in env and "ITK_NUMBER_OF_THREADS_ENV_LIST" not in env
    assert env["NSLOTS"] == "2"
    with run.itk_threads(None):
        assert "MY_SLOTS" not in os.environ
    assert os.environ["MY_SLOTS"] == "6"


def test_linear_argv_runs_one_stage_from_a_start():
    rec = recipes.recipe("cohort")
    argv = recipes.ants_linear_argv(rec["rigid"], "rigid", "/f", "/m", "/o/r_")
    assert argv[argv.index("--initial-moving-transform") + 1] == "[/f,/m,1]"
    assert argv.count("--transform") == 1 and "Rigid[0.1]" in argv
    assert argv[-2:] == ["--verbose", "1"]
    argv = recipes.ants_linear_argv(rec["affine"], "affine", "/f", "/m", "/o/a_", "/o/r.mat")
    assert argv[argv.index("--initial-moving-transform") + 1] == "[/o/r.mat]"
    assert "Affine[0.1]" in argv
    with pytest.raises(ValueError, match="rigid"):
        recipes.ants_linear_argv(rec["rigid"], "syn", "/f", "/m", "/o/")


def test_rotated_starts_turn_about_the_centre():
    params = [*np.eye(3).ravel(), 1.0, 2.0, 3.0]
    center = [10.0, -5.0, 2.0]
    p, c = linear.rotated_start(params, center, [0, 0, 90])
    assert c == center
    np.testing.assert_allclose(linear.map_affine(p, c, [center]), [[11.0, -3.0, 5.0]])
    moved = linear.map_affine(p, c, [[11.0, -5.0, 2.0]])
    np.testing.assert_allclose(moved, [[11.0, -2.0, 5.0]], atol=1e-12)
    r = linear.rotation_matrix([15, -20, 30])
    np.testing.assert_allclose(r @ r.T, np.eye(3), atol=1e-12)
    coarse = linear.coarse_stage(recipes.recipe("cohort")["rigid"], 2)
    assert coarse["shrink_factors"] == [8, 4] and coarse["iterations"] == [1000, 500]


def test_itk_affine_files_and_distances(tmp_path):
    savemat = pytest.importorskip("scipy.io").savemat

    params = np.array([1.0, 0, 0, 0, 1, 0, 0, 0, 1, 2, 0, 0])
    savemat(
        tmp_path / "a.mat",
        {"AffineTransform_double_3_3": params.reshape(12, 1), "fixed": np.zeros((3, 1))},
        format="4",
    )
    p, c = linear.read_itk_affine(tmp_path / "a.mat")
    assert p == params.tolist() and c == [0.0, 0.0, 0.0]
    ident = {"parameters": [*np.eye(3).ravel(), 0, 0, 0], "center": [0, 0, 0]}
    d = linear.affine_distance({"parameters": p, "center": c}, ident, np.zeros((4, 3)))
    assert d == {"mean_mm": 2.0, "max_mm": 2.0}
    mask = np.zeros((4, 4, 4), bool)
    mask[1, 2, 3] = True
    pts = linear.mask_points(mask, (1.0, 0.0, 0.0), (2.0, 2.0, 2.0), np.eye(3), step=1)
    np.testing.assert_allclose(pts, [[3.0, 4.0, 6.0]])


def _fake_ants(tmp_path_factory=None):
    savemat = pytest.importorskip("scipy.io").savemat
    calls = []

    def fake(cmd, log, env, timeout_s):
        calls.append((cmd, env))
        out = cmd[cmd.index("--output") + 1].strip("[]")
        transform = "Euler3DTransform" if "Rigid[0.1]" in cmd else "AffineTransform"
        Path(log).write_text(
            f"*** Running {transform} registration ***\n  Elapsed time (stage 0): 1.5\n"
            "Total elapsed time: 1.6\n"
        )
        savemat(
            f"{out}0GenericAffine.mat",
            {
                "AffineTransform_double_3_3": np.array([1.0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]),
                "fixed": np.zeros(3),
            },
            format="4",
        )
        return 0, 2.0

    return fake, calls


def test_ants_linear_runs_rigid_then_affine_from_it(tmp_path, monkeypatch):
    fake, calls = _fake_ants()
    monkeypatch.setattr(linear, "run_child", fake)
    cfg = linear.config(recipes.recipe("cohort"))
    rec = linear.run_ants_linear(
        tmp_path / "f.nii.gz",
        tmp_path / "m.nii.gz",
        tmp_path,
        cfg,
        threads=3,
        ants_dir=None,
        timeout_s=10,
    )
    assert rec["status"] == "ok" and set(rec["fits"]) == {"rigid", "affine"}
    assert rec["fits"]["rigid"]["seconds"] == 1.5 and rec["fits"]["affine"]["wall_s"] == 2.0
    assert len(calls) == 2 and all(env[run.ITK_THREADS] == "3" for _, env in calls)
    second = calls[1][0]
    start = second[second.index("--initial-moving-transform") + 1]
    assert start == f"[{(tmp_path / 'rigid_0GenericAffine.mat').as_posix()}]"


def test_a_failed_ants_rigid_fit_stops_the_linear_run(tmp_path, monkeypatch):
    def fail(cmd, log, env, timeout_s):
        Path(log).write_text("boom\n")
        return None, 10.0

    monkeypatch.setattr(linear, "run_child", fail)
    cfg = linear.config(recipes.recipe("cohort"))
    rec = linear.run_ants_linear(
        tmp_path / "f", tmp_path / "m", tmp_path, cfg, threads=1, ants_dir=None, timeout_s=1
    )
    assert rec["status"] == "failed" and "timed out" in rec["error"]
    assert set(rec["fits"]) == {"rigid"}


def test_post_ants_arguments_and_child_time():
    kw = {"reference": "/r.nii.gz", "moving": "/m.nii.gz", "forward": "/c.h5", "inverse": "/i.h5"}
    fwd = post.ants_args("forward_field", **kw, out="/o/f.nii.gz")
    assert fwd[fwd.index("--output") + 1] == "[/o/f.nii.gz,1]"
    assert fwd[fwd.index("--reference-image") + 1] == "/r.nii.gz" and "--float" in fwd
    inv = post.ants_args("inverse_field", **kw, out="/o/i.nii.gz")
    assert inv[inv.index("--reference-image") + 1] == "/m.nii.gz"
    assert inv[inv.index("--transform") + 1] == "/i.h5"
    warp = post.ants_args("lanczos_warp", **kw, out="/o/w.nii.gz")
    assert warp[warp.index("--interpolation") + 1] == "LanczosWindowedSinc"
    assert warp[warp.index("--output") + 1] == "/o/w.nii.gz"
    with pytest.raises(ValueError, match="unknown step"):
        post.ants_args("other", **kw, out="/o")
    assert post.child_seconds("noise\nFASTWARP_BENCH_SECONDS 3.25\n") == 3.25
    assert post.child_seconds("nothing") is None


def _work_pair(
    work: Path,
    dataset: str,
    pair: str,
    rec: dict,
    iso_mm: float = 1.0,
    data: Path | None = None,
) -> Path:
    folder = work / dataset / "cohort" / pair
    data = work.parent if data is None else data
    for engine, timings, wall in (
        ("fastwarp", {"linear_s": 2.0, "syn_s": 6.0, "total_s": 10.0}, 11.0),
        ("ants", {"linear_s": 60.0, "syn_s": 300.0, "total_s": 361.0}, 365.0),
    ):
        out = folder / engine
        out.mkdir(parents=True)
        record = {
            "engine": engine,
            "status": "ok",
            "recipe": rec,
            "iso_mm": iso_mm,
            "wall_s": wall,
            "finished": "2026-11-13T00:00:00+00:00",
            "timings": timings,
            "environment": {"cpu": "test cpu", "gpu": {"name": "test gpu"}, "threads": "all cores"},
        }
        (out / "run.json").write_text(json.dumps(record))
        (out / "reg_Composite.h5").write_bytes(b"x")
        (out / "reg_InverseComposite.h5").write_bytes(b"x")
    fixed, moving = pair.split("__")
    (folder / "pair.json").write_text(
        json.dumps(
            {
                "fixed": fixed,
                "moving": moving,
                "fixed_prep": {"files": {"reference": str(folder / "ref.nii.gz")}},
            }
        )
    )
    (folder / "ref.nii.gz").write_bytes(b"ref")
    image = data / dataset / datasets.get(dataset).subject(moving).image
    image.parent.mkdir(parents=True, exist_ok=True)
    image.write_bytes(b"moving")
    return folder


def _stub_steps(seconds: float, threads: int | None = 2):
    def fastwarp_steps(reference, moving, forward, inverse, tmp, formats):
        assert os.environ.get("RAYON_NUM_THREADS") == (None if threads is None else str(threads))
        assert os.environ.get(run.ITK_THREADS) == (None if threads is None else str(threads))
        assert tmp.is_dir()
        (tmp / "fastwarp_forward_field.nii").write_bytes(b"x")
        entry = {"seconds": seconds, "voxels": 1_000_000, "bytes": 2_000_000}
        steps = {s: {fmt: dict(entry) for fmt in formats} for s in post.STEPS}
        return {"steps": steps, "wall_s": 3 * seconds}

    expected = threads

    def ants_steps(
        reference, moving, forward, inverse, tmp, *, threads, ants_dir, timeout_s, formats
    ):
        assert threads == expected
        assert forward.name == "reg_Composite.h5"
        assert forward.parent.name == "fastwarp"
        entry = {"seconds": 10 * seconds, "bytes": 3_000_000}
        steps = {s: {fmt: dict(entry) for fmt in formats} for s in post.STEPS}
        return {"steps": steps, "wall_s": 30 * seconds}

    def compare(tmp):
        return {
            "forward_field": {"field_diff_max_mm": 0.01, "field_diff_rms_mm": 0.001},
            "inverse_field": {"field_diff_max_mm": 0.02, "field_diff_rms_mm": 0.002},
            "lanczos_warp": {"rms_relative": 1e-6},
        }

    return {"fastwarp_steps": fastwarp_steps, "ants_steps": ants_steps, "compare": compare}


def _environment_after_cap(**kw):
    threads = kw["threads"]
    assert os.environ.get("RAYON_NUM_THREADS") == (None if threads is None else str(threads))
    return {
        "cpu": "test cpu",
        "threads": run.thread_setting(threads),
        "itk_threads": threads,
        "rayon_threads": os.environ.get("RAYON_NUM_THREADS"),
    }


def test_post_runs_each_pair_once_and_cleans_up(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    work = tmp_path / "work"
    folder = _work_pair(work, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec)
    monkeypatch.setattr(post, "environment", _environment_after_cap)
    monkeypatch.delenv("RAYON_NUM_THREADS", raising=False)
    kw = {"data": tmp_path, "work": work, "keys": ["mindboggle"], "pairs": 2, "threads": 2}
    rows = post.run_post(**kw, log=lambda s: None, **_stub_steps(0.5))
    assert [(r["status"], r["ran"]) for r in rows] == [("ok", True), (None, False)]
    record = json.loads((folder / "post.json").read_text())
    assert record["source_engine"] == "fastwarp" and record["threads"] == 2
    assert record["warm_up_bytes"] == len(b"ref") + len(b"moving") + 2
    assert record["environment"]["rayon_threads"] == "2"
    assert record["config"]["outputs"] == ["nii"]
    assert not (folder / "post_tmp").exists() and "RAYON_NUM_THREADS" not in os.environ
    assert json.loads((work / "environment_post.json").read_text())["itk_threads"] == 2
    assert post.post_done(folder, post.config(rec, 1.0))
    again = post.run_post(**kw, log=lambda s: None, **_stub_steps(0.5))
    assert not any(r["ran"] for r in again)
    secs = run.measured_seconds(work)
    assert secs["post_fastwarp"] == 1.5 and secs["post_ants"] == 15.0
    assert secs["fastwarp"] == 11.0 and secs["ants"] == 365.0
    p = run.plan(work=work, keys=["mindboggle"], recipe_name="cohort", pairs=1, quick=False)
    assert p["post"]["mindboggle"]["to_run"] == 0


def test_post_uses_all_cores_without_threads(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    work = tmp_path / "work"
    folder = _work_pair(work, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec)
    monkeypatch.setattr(post, "environment", _environment_after_cap)
    monkeypatch.setenv("RAYON_NUM_THREADS", "3")
    monkeypatch.setenv(run.ITK_THREADS, "3")
    kw = {"data": tmp_path, "work": work, "keys": ["mindboggle"], "pairs": 1}
    rows = post.run_post(**kw, log=lambda s: None, **_stub_steps(0.5, threads=None))
    assert [r["status"] for r in rows] == ["ok"]
    record = json.loads((folder / "post.json").read_text())
    assert record["threads"] == "all cores" and record["environment"]["threads"] == "all cores"
    assert os.environ["RAYON_NUM_THREADS"] == "3" and os.environ[run.ITK_THREADS] == "3"
    assert run.ITK_THREADS not in run.child_env(None)


def test_post_with_gzip_times_both_formats(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    work = tmp_path / "work"
    _work_pair(work, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec)
    monkeypatch.setattr(post, "environment", _environment_after_cap)
    kw = {"data": tmp_path, "work": work, "keys": ["mindboggle"], "pairs": 1, "threads": 2}
    post.run_post(**kw, gzip=True, log=lambda s: None, **_stub_steps(0.5))
    got = post.rows(work, "cohort", rec)
    assert sorted({(r["step"], r["format"]) for r in got}) == sorted(
        (s, f) for s in post.STEPS for f in ("nii", "nii.gz")
    )
    assert all(r["fastwarp_bytes"] == 2_000_000 and r["iso_mm"] == 1.0 for r in got)
    other = recipes.recipe("cohort")
    other["syn"]["cc_radius"] = 2
    assert post.rows(work, "cohort", other) == []


def test_a_failing_fastwarp_step_is_recorded(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    folder = _work_pair(
        tmp_path, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec, data=tmp_path
    )

    def broken(*args):
        raise RuntimeError("no luck")

    stubs = {**_stub_steps(1.0), "fastwarp_steps": broken}
    record = post.post_pair(
        datasets.get("mindboggle"),
        folder,
        post.config(rec, 1.0),
        data=tmp_path,
        threads=1,
        ants_dir=None,
        timeout_s=None,
        **stubs,
    )
    assert record["status"] == "failed" and "no luck" in record["error"]
    assert not (folder / "post_tmp").exists()


def _linear_records(work: Path, rec: dict) -> None:
    cfg = linear.config(rec)
    for i, pair in enumerate(SNSX_PAIRS[:2]):
        folder = work / "snsx" / "linear" / pair
        env = {"cpu": "test cpu", "gpu": {"name": "test gpu"}}
        fits_fw = {
            "rigid": {"seconds": 1.0 + i},
            "affine": {"seconds": 1.5},
            "multistart": {"seconds": 4.0},
        }
        fits_ants = {"rigid": {"seconds": 20.0}, "affine": {"seconds": 30.0 + i}}
        for engine, fits in (("fastwarp", fits_fw), ("ants", fits_ants)):
            record = {
                "kind": "linear",
                "engine": engine,
                "status": "ok",
                "config": cfg,
                "iso_mm": 1.0,
                "fits": fits,
                "wall_s": 9.0,
                "environment": env,
            }
            if engine == "fastwarp":
                record["prepare_s"] = 0.5
            (folder / engine).mkdir(parents=True)
            (folder / engine / "run.json").write_text(json.dumps(record))
        comparison = {
            "config": cfg,
            "iso_mm": 1.0,
            "fits": {
                "rigid": {
                    "fastwarp": {"landmark_mean_mm": 4.0},
                    "ants": {"landmark_mean_mm": 4.2},
                    "agreement": {"mean_mm": 0.3, "max_mm": 0.9},
                }
            },
        }
        (folder / "comparison.json").write_text(json.dumps(comparison))


def test_report_tabulates_the_timings_without_an_evaluation(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    work = tmp_path / "work"
    _work_pair(work, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec)
    other = _work_pair(work, "mindboggle", "OASIS-TRT-20-2__OASIS-TRT-20-3", rec)
    run_json = other / "ants" / "run.json"
    data = json.loads(run_json.read_text())
    data["timings"]["total_s"] = 363.0
    data["environment"]["cpu"] = "another cpu"
    run_json.write_text(json.dumps(data))
    _work_pair(work, "mindboggle", "OASIS-TRT-20-3__OASIS-TRT-20-4", rec, iso_mm=2.0)
    monkeypatch.setattr(post, "environment", _environment_after_cap)
    post.run_post(
        data=tmp_path,
        work=work,
        keys=["mindboggle"],
        pairs=1,
        threads=2,
        log=lambda s: None,
        **_stub_steps(0.5),
    )
    _linear_records(work, rec)
    (work / "environment.json").write_text(
        json.dumps({"cpu": "test cpu", "threads": "all cores", "ants_version": "2.6.5", "gpu": {}})
    )
    (work / "environment_linear.json").write_text(json.dumps({"itk_threads": 8}))
    md = report.report(work=work, recipe_name="cohort", plot=False).read_text(encoding="utf-8")
    assert "## Registration time" in md and "## Linear fits" in md and "## Post-processing" in md
    fw = "| mindboggle | 1 mm | all cores | fastwarp | 2 | 2.0 (2.0–2.0) | 6.0 (6.0–6.0) |"
    assert fw in md
    assert (
        "| mindboggle | 1 mm | all cores | ants | 2 | 60.0 (60.0–60.0) | 300.0 (300.0–300.0) |"
        in md
    )
    ratio = (
        "| mindboggle | 1 mm | all cores | ANTs / FastWarp |  | 30.0x (1 pair) | 50.0x (1 pair) |"
    )
    assert ratio + " 36.1x (1 pair) | 33.2x (1 pair) |" in md
    assert "| mindboggle | 2 mm | all cores | fastwarp | 1 |" in md
    lin = "| snsx | 1 mm | not recorded | rigid | 2 / 2 | 1.50 (1.25–1.75) | 20.00 (20.00–20.00) |"
    assert lin + " 15.0x (2 pairs) | 4.00 (4.00–4.00) | 4.20 (4.20–4.20) | 0.30 (0.30–0.30) |" in md
    both = "| snsx | 1 mm | not recorded | rigid + affine (FastWarp with its preparation) | 2 / 2 |"
    assert both + " 3.50 (3.25–3.75) | 50.50 (50.25–50.75) | 14.7x (2 pairs) |" in md
    assert (
        "| snsx | 1 mm | not recorded | session preparation | 2 / 0 | 0.50 (0.50–0.50) | n/a |"
        in md
    )
    assert (
        "| snsx | 1 mm | not recorded | rigid multi-start | 2 / 0 | 4.00 (4.00–4.00) | n/a |" in md
    )
    post_row = (
        "| mindboggle | 1 mm | 2 | forward field (compose) | .nii | 1 | 1.0 M | 5.00 | 0.50 |"
    )
    assert post_row + " 10.0x (1 pair) | 2 / 3 | max 0.010 mm |" in md
    assert "relative RMS 1.00e-06" in md
    assert "## Machines" in md and "another cpu" in md
    assert "ANTs 2.6.5" in md and "driver name" in md
    assert "- `run`: threads all cores (ANTs and FastWarp's CPU work)" in md
    assert "- `linear`: threads ANTs 8, FastWarp all cores (ANTs" in md
    assert "- `post`: threads 2 (ANTs and FastWarp's CPU work)" in md
    out = work / "results" / "cohort"
    assert (out / "timings.csv").is_file() and (out / "linear.csv").is_file()
    assert (out / "post.csv").read_text().startswith("dataset,iso_mm,pair,step,format")
    only = report.report(work=work, recipe_name="cohort", plot=False, iso_mm=2.0).read_text(
        encoding="utf-8"
    )
    assert "| mindboggle | 2 mm | all cores | fastwarp | 1 |" in only and "| 1 mm |" not in only


def test_report_refuses_an_empty_work_folder(tmp_path):
    with pytest.raises(RuntimeError, match="nothing to report"):
        report.report(work=tmp_path, recipe_name="cohort", plot=False)


def test_measured_seconds_keep_the_steps_apart(tmp_path):
    rec = recipes.recipe("cohort")
    _linear_records(tmp_path, rec)
    secs = run.measured_seconds(tmp_path)
    assert secs["linear_fastwarp"] == 9.0 and secs["linear_ants"] == 9.0
    assert secs["fastwarp"] == run.DEFAULT_SECONDS["fastwarp"]
    left = linear.todo_count(tmp_path, ["snsx"], "cohort", 3, False, 1.0)
    assert left["snsx"] == {"pairs": 3, "to_run": {"fastwarp": 1, "ants": 1}}
    got = linear.rows(tmp_path, "cohort")
    assert len(got) == 16 and {r["iso_mm"] for r in got} == {1.0}
    assert [r["agreement_mean_mm"] for r in got if r["fit"] == "rigid"] == [None, 0.3, None, 0.3]
    other = recipes.recipe("cohort")
    other["rigid"]["gradient_step"] = 0.2
    recipes.RECIPES["other-linear"] = other
    try:
        assert linear.rows(tmp_path, "other-linear") == []
    finally:
        del recipes.RECIPES["other-linear"]
    comparison = tmp_path / "snsx" / "linear" / SNSX_PAIRS[0] / "comparison.json"
    stale = json.loads(comparison.read_text())
    stale["iso_mm"] = 2.0
    comparison.write_text(json.dumps(stale))
    got = linear.rows(tmp_path, "cohort")
    assert [r["landmark_mean_mm"] for r in got if r["fit"] == "rigid"] == [None, None, 4.2, 4.0]


def test_post_redoes_a_pair_whose_registration_changed(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    work = tmp_path / "work"
    folder = _work_pair(work, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec)
    monkeypatch.setattr(post, "environment", _environment_after_cap)
    kw = {"data": tmp_path, "work": work, "keys": ["mindboggle"], "pairs": 1, "threads": 2}
    post.run_post(**kw, log=lambda s: None, **_stub_steps(0.5))
    cfg = post.config(rec, 1.0)
    assert post.post_done(folder, cfg)
    run_json = folder / "fastwarp" / "run.json"
    data = json.loads(run_json.read_text())
    data["finished"] = "2026-11-14T00:00:00+00:00"
    run_json.write_text(json.dumps(data))
    assert not post.post_done(folder, cfg)
    rows = post.run_post(**kw, log=lambda s: None, **_stub_steps(0.5))
    assert [(r["status"], r["ran"]) for r in rows] == [("ok", True)]
    assert json.loads((folder / "post.json").read_text())["source_finished"] == data["finished"]


def test_a_failed_post_is_retried_after_its_registration_changed(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    work = tmp_path / "work"
    folder = _work_pair(work, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec)
    monkeypatch.setattr(post, "environment", _environment_after_cap)
    kw = {"data": tmp_path, "work": work, "keys": ["mindboggle"], "pairs": 1, "threads": 2}

    def broken(tmp):
        raise ValueError("shapes differ")

    stubs = {**_stub_steps(0.5), "compare": broken}
    rows = post.run_post(**kw, log=lambda s: None, **stubs)
    assert [(r["status"], r["ran"]) for r in rows] == [("failed", True)]
    rows = post.run_post(**kw, log=lambda s: None, **stubs)
    assert [(r["status"], r["ran"]) for r in rows] == [("failed", False)]
    run_json = folder / "fastwarp" / "run.json"
    data = json.loads(run_json.read_text())
    data["finished"] = "2026-11-14T00:00:00+00:00"
    run_json.write_text(json.dumps(data))
    rows = post.run_post(**kw, log=lambda s: None, **_stub_steps(0.5))
    assert [(r["status"], r["ran"]) for r in rows] == [("ok", True)]


def test_a_failed_comparison_fails_the_post_record(tmp_path):
    rec = recipes.recipe("cohort")
    folder = _work_pair(
        tmp_path, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec, data=tmp_path
    )

    def broken(tmp):
        raise ValueError("shapes differ")

    stubs = {**_stub_steps(1.0), "compare": broken}
    record = post.post_pair(
        datasets.get("mindboggle"),
        folder,
        post.config(rec, 1.0),
        data=tmp_path,
        threads=2,
        ants_dir=None,
        timeout_s=None,
        **stubs,
    )
    assert record["status"] == "failed" and "shapes differ" in record["error"]


def test_ants_steps_fall_back_to_the_wall_time(tmp_path, monkeypatch):
    def fake(cmd, log, env, timeout_s):
        assert env[run.ITK_THREADS] == "3"
        out = cmd[cmd.index("--output") + 1].strip("[]").removesuffix(",1")
        Path(out).write_bytes(b"xyz")
        marker = "" if "inverse" in out else "FASTWARP_BENCH_SECONDS 0.25\n"
        Path(log).write_text(marker)
        return 0, 2.0

    monkeypatch.setattr(post, "run_child", fake)
    f = tmp_path / "x"
    out = post.run_ants_steps(f, f, f, f, tmp_path, threads=3, ants_dir=None, timeout_s=5)
    steps = out["steps"]
    assert "error" not in out and steps["forward_field"]["nii"]["seconds"] == 0.25
    inverse = steps["inverse_field"]["nii"]
    assert inverse["seconds"] == 2.0 and "timing_note" in inverse and inverse["bytes"] == 3
    assert (tmp_path / "ants_lanczos_warp.nii").is_file()


def _stub_linear(monkeypatch, calls):
    monkeypatch.setattr(linear, "environment", _environment_after_cap)

    def prep(ds, sid, data, work, iso_mm):
        assert os.environ[run.ITK_THREADS] == "2"
        return {"files": {"registration": str(work / f"{sid}.nii.gz")}}

    monkeypatch.setattr(linear, "prepare_fixed", prep)
    monkeypatch.setattr(linear, "prepare_moving", lambda *a: Path("moving.nii.gz"))

    def engine(name):
        def fit(fixed, moving, out, cfg, **kw):
            calls.append(name)
            return {"kind": "linear", "engine": name, "status": "ok", "fits": {}, "wall_s": 1.0}

        return fit

    def compare(ds, folder, prep, *, data, cfg, iso_mm):
        calls.append("compare")
        return {"fits": {}, "config": cfg, "iso_mm": iso_mm}

    monkeypatch.setattr(linear, "run_fastwarp_linear", engine("fastwarp"))
    monkeypatch.setattr(linear, "run_ants_linear", engine("ants"))
    monkeypatch.setattr(linear, "compare", compare)


def _snsx_data(tmp_path: Path) -> tuple[Path, Path]:
    data, work = tmp_path / "data", tmp_path / "work"
    (data / "snsx").mkdir(parents=True)
    (data / "snsx" / ".fetched.json").write_text("{}")
    return data, work


def test_linear_runs_each_pair_once(tmp_path, monkeypatch):
    calls: list[str] = []
    _stub_linear(monkeypatch, calls)
    data, work = _snsx_data(tmp_path)
    monkeypatch.delenv(run.ITK_THREADS, raising=False)
    monkeypatch.delenv("RAYON_NUM_THREADS", raising=False)
    kw = {"data": data, "work": work, "keys": ["snsx"], "pairs": 2, "threads": 2}
    rows = linear.run_linear(**kw, log=lambda s: None)
    assert calls == ["fastwarp", "ants", "compare"] * 2
    assert all(r["status"] == "ok" and r["ran"] for r in rows)
    assert run.ITK_THREADS not in os.environ and "RAYON_NUM_THREADS" not in os.environ
    for pair in SNSX_PAIRS[:2]:
        assert (work / "snsx" / "linear" / pair / "comparison.json").is_file()
    again = linear.run_linear(**kw, log=lambda s: None)
    assert len(calls) == 6 and not any(r["ran"] for r in again)
    again = linear.run_linear(**{**kw, "iso_mm": 2.0}, log=lambda s: None)
    assert calls[6:] == ["fastwarp", "ants", "compare"] * 2


def test_a_failed_linear_comparison_is_a_failed_row(tmp_path, monkeypatch):
    calls: list[str] = []
    _stub_linear(monkeypatch, calls)

    def broken(ds, folder, prep, *, data, cfg, iso_mm):
        raise ValueError("labels differ")

    monkeypatch.setattr(linear, "compare", broken)
    data, work = _snsx_data(tmp_path)
    kw = {"data": data, "work": work, "keys": ["snsx"], "pairs": 1, "threads": 2}
    rows = linear.run_linear(**kw, log=lambda s: None)
    assert [(r["engine"], r["status"]) for r in rows] == [
        ("fastwarp", "ok"),
        ("ants", "ok"),
        ("comparison", "failed"),
    ]
    saved = json.loads((work / "snsx" / "linear" / SNSX_PAIRS[0] / "comparison.json").read_text())
    assert "labels differ" in saved["error"]


def test_linear_records_a_failed_preparation(tmp_path, monkeypatch):
    calls: list[str] = []
    _stub_linear(monkeypatch, calls)

    def broken(*args):
        raise OSError("unreadable")

    monkeypatch.setattr(linear, "prepare_fixed", broken)
    data, work = _snsx_data(tmp_path)
    kw = {"data": data, "work": work, "keys": ["snsx"], "pairs": 1, "threads": 2}
    rows = linear.run_linear(**kw, log=lambda s: None)
    assert [r["status"] for r in rows] == ["failed", "failed"] and calls == []
    record = json.loads(
        (work / "snsx" / "linear" / SNSX_PAIRS[0] / "ants" / "run.json").read_text()
    )
    assert record["status"] == "failed" and "unreadable" in record["error"]
    again = linear.run_linear(**kw, log=lambda s: None)
    assert not any(r["ran"] for r in again)


def test_thread_labels_of_old_and_new_records():
    assert timing.thread_label({"threads": "all cores", "itk_threads": None}) == "all cores"
    assert timing.thread_label({"threads": 4}) == 4
    assert timing.thread_label({"itk_threads": 8, "rayon_threads": "8"}) == 8
    assert timing.thread_label({"itk_threads": 8}) == "ANTs 8, FastWarp all cores"
    assert timing.thread_label({}) is None


def test_physical_cores_from_cpuinfo():
    block = "processor\t: {}\nphysical id\t: {}\ncore id\t\t: {}\n\n"
    text = "".join(block.format(i, i // 4, i % 2) for i in range(8))
    assert run.physical_cores_in_cpuinfo(text) == 4
    assert run.physical_cores_in_cpuinfo("processor : 0\n") is None


def test_timings_of_different_thread_settings_stay_apart():
    def row(pair, engine, threads, total):
        return {
            "dataset": "d",
            "iso_mm": 1.0,
            "threads": threads,
            "pair": pair,
            "engine": engine,
            "total_s": total,
        }

    rows = [
        row("p1", "fastwarp", "all cores", 10.0),
        row("p1", "ants", "all cores", 100.0),
        row("p2", "fastwarp", 4, 20.0),
        row("p2", "ants", "all cores", 300.0),
    ]
    table = report.registration_timing_table(rows)
    got = [(r["threads"], r["engine"], r["n"], r["total_s"]) for r in table]
    assert got == [
        ("4", "fastwarp", 1, "20.0"),
        ("all cores", "fastwarp", 1, "10.0"),
        ("all cores", "ants", 2, "200.0 (150.0–250.0)"),
        ("all cores", "ANTs / FastWarp", "", "10.0x (1 pair)"),
    ]
    assert len(timing.machines({"run": rows})) == 2
    rows[1]["cpu"] = "another cpu"
    table = report.registration_timing_table(rows)
    assert [r for r in table if r["engine"] == "ANTs / FastWarp"][0]["total_s"] == "n/a"


def test_a_post_that_raised_is_not_retried_until_its_registration_changes(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    work = tmp_path / "work"
    folder = _work_pair(work, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec)
    monkeypatch.setattr(post, "environment", _environment_after_cap)
    calls = []

    def broken(*args, **kwargs):
        calls.append(1)
        raise OSError("unreadable")

    monkeypatch.setattr(post, "post_pair", broken)
    kw = {"data": tmp_path, "work": work, "keys": ["mindboggle"], "pairs": 1, "threads": 2}
    assert [r["ran"] for r in post.run_post(**kw, log=lambda s: None)] == [True]
    record = json.loads((folder / "post.json").read_text())
    assert record["status"] == "failed" and record["source_engine"] == "fastwarp"
    assert [r["ran"] for r in post.run_post(**kw, log=lambda s: None)] == [False]
    assert len(calls) == 1


def test_plan_counts_a_gzip_post_as_done(tmp_path, monkeypatch):
    rec = recipes.recipe("cohort")
    work = tmp_path / "work"
    _work_pair(work, "mindboggle", "OASIS-TRT-20-1__OASIS-TRT-20-2", rec)
    monkeypatch.setattr(post, "environment", _environment_after_cap)
    kw = {"data": tmp_path, "work": work, "keys": ["mindboggle"], "pairs": 1, "threads": 2}
    post.run_post(**kw, gzip=True, log=lambda s: None, **_stub_steps(0.5))
    p = run.plan(work=work, keys=["mindboggle"], recipe_name="cohort", pairs=1, quick=False)
    assert p["post"]["mindboggle"]["to_run"] == 0
    secs = run.measured_seconds(work)
    assert secs["post_fastwarp"] == run.DEFAULT_SECONDS["post_fastwarp"]


def test_a_comparison_rerun_that_cannot_prepare_is_a_failed_row(tmp_path, monkeypatch):
    calls: list[str] = []
    _stub_linear(monkeypatch, calls)
    data, work = _snsx_data(tmp_path)
    kw = {"data": data, "work": work, "keys": ["snsx"], "pairs": 1, "threads": 2}
    linear.run_linear(**kw, log=lambda s: None)
    comparison = work / "snsx" / "linear" / SNSX_PAIRS[0] / "comparison.json"
    stored = json.loads(comparison.read_text())
    comparison.write_text(json.dumps({**stored, "error": "earlier failure"}))

    def broken(*args):
        raise OSError("unreadable")

    monkeypatch.setattr(linear, "prepare_fixed", broken)
    rows = linear.run_linear(**kw, log=lambda s: None)
    assert [(r["engine"], r["status"], r["ran"]) for r in rows] == [
        ("fastwarp", "ok", False),
        ("ants", "ok", False),
        ("comparison", "failed", True),
    ]
