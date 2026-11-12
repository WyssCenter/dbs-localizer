# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""``scripts/leadtutor_cohort.py``: scorer, D9 compare, determinism, temp refusal."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import pytest

from scripts import leadtutor_cohort as lc


def _write_reco(root: Path, sid: str, leads: dict[str, dict]) -> Path:
    reco = (
        root
        / lc.DERIV
        / f"sub-{sid}"
        / "reconstruction"
        / f"sub-{sid}_desc-reconstruction.json"
    )
    reco.parent.mkdir(parents=True, exist_ok=True)
    reco.write_text(json.dumps({"mni": leads}), encoding="utf-8")
    return reco


EXPERTS = {
    "7": {
        "leftElectrode": {"head": [10.0, 0.0, 0.0], "tail": [10.0, 0.0, 10.0]},
        "rightElectrode": {"head": [-10.0, 0.0, 0.0], "tail": [-10.0, 0.0, 10.0]},
    }
}


def test_score_orders_leads_by_head_x_not_by_key() -> None:
    reco = {
        "mni": {
            "1": {"markers": {"head": [-12.0, 0.0, 0.0], "tail": [-12.0, 0.0, 10.0]}},
            "2": {"markers": {"head": [11.0, 0.0, 0.0], "tail": [11.0, 0.0, 10.0]}},
        }
    }
    assert lc.score_leads(reco, EXPERTS["7"]) == pytest.approx([1.0, 2.0])


def test_score_subject_reads_a_reconstruction_file(tmp_path: Path) -> None:
    reco = _write_reco(
        tmp_path,
        "7",
        {
            "1": {"markers": {"head": [-12.0, 0.0, 0.0], "tail": [-12.0, 0.0, 10.0]}},
            "2": {"markers": {"head": [11.0, 0.0, 0.0], "tail": [11.0, 0.0, 10.0]}},
        },
    )
    assert lc.score_subject(reco, EXPERTS["7"]) == pytest.approx([1.0, 2.0])


def test_score_run_writes_the_scores_object(tmp_path: Path) -> None:
    _write_reco(
        tmp_path,
        "7",
        {
            "1": {"markers": {"head": [10.0, 0.0, 0.0], "tail": [10.0, 0.0, 10.0]}},
            "2": {"markers": {"head": [-10.0, 0.0, 0.0], "tail": [-10.0, 0.0, 10.0]}},
        },
    )
    scores = lc.score_run(tmp_path, EXPERTS)
    assert scores == {"7": pytest.approx([0.0, 0.0])}
    assert lc.mean_of(lc._all_leads(scores)) == pytest.approx(0.0)


def test_score_missing_reconstruction_is_none(tmp_path: Path) -> None:
    assert lc.score_run(tmp_path, EXPERTS) == {"7": None}


def test_d9_mean_margin_boundary() -> None:
    reference = {"a": [1.0, 1.0]}
    at = lc.d9_rule(reference, {"a": [1.05, 1.05]})
    assert at["mean_ok"] and at["pass"]
    over = lc.d9_rule(reference, {"a": [1.050002, 1.05]})
    assert not over["mean_ok"] and not over["pass"]


def test_d9_subject_margin_boundary() -> None:
    reference = {"a": [1.0, 1.0], "b": [1.0, 1.0]}
    at = lc.d9_rule(reference, {"a": [1.5, 1.5], "b": [0.5, 0.5]})
    assert at["mean_ok"] and at["subject_ok"] and at["pass"]
    over = lc.d9_rule(reference, {"a": [1.500002, 1.5], "b": [0.5, 0.5]})
    assert over["mean_ok"] and not over["subject_ok"] and not over["pass"]


def test_d9_reports_the_largest_per_lead_change() -> None:
    verdict = lc.d9_rule({"a": [1.0, 1.0]}, {"a": [1.1, 1.02]})
    assert verdict["largest_lead_change"] == pytest.approx(0.1)
    assert verdict["largest_lead_at"] == "sub-a +x"


def test_compare_scores_renders_both_tables_and_a_verdict() -> None:
    text = lc.compare_scores({"a": [1.0, 1.0]}, {"a": [1.04, 1.04]})
    assert "sub-a" in text
    assert "reference mean : 1.000" in text
    assert "D9: PASS" in text


def test_compare_rejects_a_missing_subject(tmp_path: Path) -> None:
    (tmp_path / "scores.json").write_text(
        json.dumps({"a": [1.0, 1.0]}), encoding="utf-8"
    )
    only_one = lc.load_scores(tmp_path / "scores.json")
    text = lc.compare_scores(only_one, {"b": [1.0, 1.0]})
    assert "missing on one side" in text
    assert "D9: FAIL" in text
    verdict = lc.d9_rule(only_one, {"b": [1.0, 1.0]})
    assert verdict["missing"] == ["a"]
    assert not verdict["coverage_ok"] and not verdict["pass"]


def test_null_scored_candidate_subject_fails_d9(tmp_path: Path) -> None:
    ref = tmp_path / "ref.json"
    cand = tmp_path / "cand.json"
    ref.write_text(json.dumps({"a": [1.0, 1.0], "b": [2.0, 2.0]}), encoding="utf-8")
    cand.write_text(json.dumps({"a": [1.0, 1.0], "b": None}), encoding="utf-8")
    candidate = lc.load_scores(cand)
    assert candidate == {"a": [1.0, 1.0], "b": None}
    verdict = lc.d9_rule(lc.load_scores(ref), candidate)
    assert verdict["missing"] == ["b"]
    assert verdict["mean_ok"] and verdict["subject_ok"]
    assert not verdict["pass"]


def test_cmd_compare_exit_code_follows_the_verdict(tmp_path: Path) -> None:
    ref = tmp_path / "ref.json"
    good = tmp_path / "good.json"
    bad = tmp_path / "bad.json"
    ref.write_text(json.dumps({"a": [1.0, 1.0]}), encoding="utf-8")
    good.write_text(json.dumps({"a": [1.0, 1.0]}), encoding="utf-8")
    bad.write_text(json.dumps({"a": None}), encoding="utf-8")

    def run(candidate: Path) -> int:
        args = argparse.Namespace(from_scores=True, reference=ref, candidate=candidate)
        return lc.cmd_compare(args)

    assert run(good) == 0
    assert run(bad) == 1


def _tree(root: Path, sid: str, payload: bytes) -> None:
    base = root / lc.DERIV / f"sub-{sid}" / "normalization" / "anat"
    base.mkdir(parents=True, exist_ok=True)
    (base / "sub-x_ses-preop_space-MNI_desc-preproc_T1w.nii.gz").write_bytes(payload)
    xfm = base.parent / "transformations"
    xfm.mkdir(parents=True, exist_ok=True)
    (xfm / "sub-x_from-anchorNative_to-MNI_mode-image_xfm.h5").write_bytes(payload)
    (xfm / "sub-x_from-MNI_to-anchorNative_mode-image_xfm.h5").write_bytes(payload)


def test_determinism_identical_trees(tmp_path: Path) -> None:
    _tree(tmp_path / "left", "7", b"same")
    _tree(tmp_path / "right", "7", b"same")
    report, identical = lc.determinism_report(
        tmp_path / "left", tmp_path / "right", ["7"]
    )
    assert identical
    assert "identical" in report


def test_determinism_flags_a_changed_byte(tmp_path: Path) -> None:
    _tree(tmp_path / "left", "7", b"same")
    _tree(tmp_path / "right", "7", b"other")
    _, identical = lc.determinism_report(tmp_path / "left", tmp_path / "right", ["7"])
    assert not identical


def test_determinism_flags_a_missing_file(tmp_path: Path) -> None:
    _tree(tmp_path / "left", "7", b"same")
    (tmp_path / "right" / lc.DERIV / "sub-7").mkdir(parents=True)
    _, identical = lc.determinism_report(tmp_path / "left", tmp_path / "right", ["7"])
    assert not identical


def test_run_root_under_temp_is_refused(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="temporary directory"):
        lc.assert_run_root_outside_temp(tmp_path)


def test_run_root_outside_temp_is_accepted() -> None:
    lc.assert_run_root_outside_temp(lc.REPO / "hpdbs_runs" / "demo")


def test_parse_tasklist_finds_the_ants_binaries() -> None:
    text = '"antsRegistration.exe","10","Console","1","1 K"\n"explorer.exe","9","x","1","1 K"\n'
    assert lc.parse_tasklist(text) == ["antsRegistration.exe"]


def test_parse_profile_log_reads_the_stage_timings() -> None:
    text = 'noise\n{\n  "stage_timings_sec": {"normalize": 12.5},\n  "total_seconds": 13.0\n}\n'
    profile = lc.parse_profile_log(text)
    assert profile is not None
    assert profile["stage_timings_sec"] == {"normalize": 12.5}


def test_build_run_argv_pins_the_harness_flags(tmp_path: Path) -> None:
    argv = lc.build_run_argv(
        ["dbsloc"],
        tmp_path,
        "7",
        template=tmp_path / "t1.nii.gz",
        isotropic_mm="1.0",
        stages="normalize,reconstruct",
        reg_backend="wgpu",
        extra=["--no-norm-multispectral"],
    )
    assert argv[:4] == ["dbsloc", "run", str(tmp_path), "7"]
    joined = " ".join(argv)
    for flag in (
        "--recon-method trac_core_native",
        "--headless",
        "--no-checkreg",
        "--no-scene3d",
        "--log plain",
        "--profile",
        "--overwrite-approved",
        "--norm-isotropic-mm 1.0",
        "--stages normalize,reconstruct",
        "--reg-backend wgpu",
        "--no-norm-multispectral",
    ):
        assert flag in joined


def test_subjects_default_and_comma_list() -> None:
    assert lc.subjects_from_arg(None) == list(lc.DEFAULT_SUBJECTS)
    assert lc.subjects_from_arg(["1,2", "3"]) == ["1", "2", "3"]


def test_normalization_record_and_level_seconds(tmp_path: Path) -> None:
    base = tmp_path / lc.DERIV / "sub-7" / "normalization" / "log"
    base.mkdir(parents=True)
    (base / "sub-7_desc-normmethod.json").write_text(
        json.dumps(
            {
                "reg_timings": {"load_s": 1.0, "syn_s": 45.9},
                "reg_rigid_levels": [{"seconds": 1.5}, {"seconds": 2.0}],
            }
        ),
        encoding="utf-8",
    )
    record = lc.normalization_record(tmp_path, "7")
    assert record is not None
    assert record["reg_timings"]["syn_s"] == pytest.approx(45.9)
    assert lc.level_seconds(record, "reg_rigid_levels") == pytest.approx(3.5)
    assert lc.level_seconds(record, "reg_syn_levels") is None


def test_load_experts_accepts_wrapper_and_bare_mapping(tmp_path: Path) -> None:
    wrapped = tmp_path / "wrapped.json"
    wrapped.write_text(json.dumps({"subjects": EXPERTS}), encoding="utf-8")
    bare = tmp_path / "bare.json"
    bare.write_text(json.dumps(EXPERTS), encoding="utf-8")
    assert lc.load_experts(wrapped) == EXPERTS
    assert lc.load_experts(bare) == EXPERTS


def test_load_experts_rejects_a_list_shaped_subjects(tmp_path: Path) -> None:
    path = tmp_path / "bad.json"
    path.write_text(json.dumps({"subjects": [1, 2]}), encoding="utf-8")
    with pytest.raises(SystemExit, match="subjects"):
        lc.load_experts(path)


def test_seed_subject_from_data_root_copies_rawdata_and_prefs(tmp_path: Path) -> None:
    src = tmp_path / "src"
    (src / "rawdata" / "sub-7" / "anat").mkdir(parents=True)
    (src / "rawdata" / "sub-7" / "anat" / "x.nii.gz").write_bytes(b"x")
    (src / "dataset_description.json").write_text("{}", encoding="utf-8")
    prefs = src / lc.DERIV / "sub-7" / "prefs"
    prefs.mkdir(parents=True)
    (prefs / "p.mat").write_bytes(b"p")
    root = tmp_path / "run"
    lc.seed_subject(root, "7", src, None)
    assert (root / "rawdata" / "sub-7" / "anat" / "x.nii.gz").is_file()
    assert (root / lc.DERIV / "sub-7" / "prefs" / "p.mat").is_file()
    assert not (root / lc.DERIV / "sub-7" / "preprocessing").exists()
    assert (root / "dataset_description.json").is_file()


def test_seed_subject_from_seed_root_copies_stages_but_not_t2_coreg(
    tmp_path: Path,
) -> None:
    src = tmp_path / "seed"
    (src / lc.DERIV / "sub-7" / "preprocessing" / "anat").mkdir(parents=True)
    (src / lc.DERIV / "sub-7" / "preprocessing" / "anat" / "keep.nii.gz").write_bytes(
        b"k"
    )
    coreg = src / lc.DERIV / "sub-7" / "coregistration" / "anat"
    coreg.mkdir(parents=True)
    t2_name = "sub-7_ses-preop_space-anchorNative_desc-preproc_T2w.nii.gz"
    ct_name = "sub-7_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz"
    (coreg / t2_name).write_bytes(b"t")
    (coreg / ct_name).write_bytes(b"c")
    root = tmp_path / "run"
    lc.seed_subject(root, "7", None, src)
    assert (
        root / lc.DERIV / "sub-7" / "preprocessing" / "anat" / "keep.nii.gz"
    ).is_file()
    assert (root / lc.DERIV / "sub-7" / "coregistration" / "anat" / ct_name).is_file()
    assert not (
        root / lc.DERIV / "sub-7" / "coregistration" / "anat" / t2_name
    ).exists()


def test_seed_exclude_leaves_a_stage_out(tmp_path: Path) -> None:
    src = tmp_path / "seed"
    for stage in ("preprocessing", "coregistration"):
        base = src / lc.DERIV / "sub-7" / stage / "anat"
        base.mkdir(parents=True)
        (base / "f.bin").write_bytes(b"f")
    root = tmp_path / "run"
    lc.seed_subject(root, "7", None, src, {"coregistration"})
    assert (root / lc.DERIV / "sub-7" / "preprocessing").is_dir()
    assert not (root / lc.DERIV / "sub-7" / "coregistration").exists()


def test_seed_subject_does_not_overwrite_an_existing_stage(tmp_path: Path) -> None:
    src = tmp_path / "seed"
    (src / lc.DERIV / "sub-7" / "preprocessing").mkdir(parents=True)
    (src / lc.DERIV / "sub-7" / "preprocessing" / "a.bin").write_bytes(b"a")
    root = tmp_path / "run"
    dst = root / lc.DERIV / "sub-7" / "preprocessing"
    dst.mkdir(parents=True)
    (dst / "old.bin").write_bytes(b"old")
    lc.seed_subject(root, "7", None, src)
    assert (dst / "old.bin").is_file()
    assert not (dst / "a.bin").exists()


def test_seed_subject_without_a_source_is_an_error(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="data-root"):
        lc.seed_subject(tmp_path / "run", "7", None, None)


def test_timings_tables_read_logs_and_records(tmp_path: Path) -> None:
    logs = tmp_path / "logs"
    logs.mkdir(parents=True)
    (logs / "sub-7.log").write_text(
        "note\n"
        + json.dumps({"stage_timings_sec": {"normalize": 12.0}, "total_seconds": 13.0})
        + "\n",
        encoding="utf-8",
    )
    base = tmp_path / lc.DERIV / "sub-7" / "normalization" / "log"
    base.mkdir(parents=True)
    (base / "sub-7_desc-normmethod.json").write_text(
        json.dumps(
            {
                "reg_timings": {"syn_s": 45.9},
                "reg_affine_levels": [{"seconds": 1.0}, {"seconds": 2.0}],
            }
        ),
        encoding="utf-8",
    )
    markdown, csv = lc.timings_tables(tmp_path, ["7"])
    assert "| subject | normalize | total_seconds |" in markdown
    assert "affine_levels_s" in markdown
    assert "table,subject" in csv
