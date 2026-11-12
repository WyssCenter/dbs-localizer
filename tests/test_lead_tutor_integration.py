# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""
Integration tests against a local Lead-Tutor BIDS copy.

Setup:
  export HPDATA_LEADTUTOR=/path/to/LeadTutor/LeadTutor
  (folder containing ``dataset_description.json``)
"""

from __future__ import annotations

import json
from pathlib import Path

import nibabel as nib
import pytest

hr = pytest.importorskip("dbsloc_reconstruct", reason="PyO3 extension not built")

pytestmark = pytest.mark.integration


def _subjects(root: Path) -> list[str]:
    raw = root / "rawdata"
    if not raw.is_dir():
        pytest.skip("rawdata/ missing in this copy")
    return sorted(
        p.name[4:] for p in raw.iterdir() if p.is_dir() and p.name.startswith("sub-")
    )


def test_bids_top_level(lead_tutor_root: Path) -> None:
    d = json.loads(
        (lead_tutor_root / "dataset_description.json").read_text(encoding="utf-8")
    )
    assert "BIDSVersion" in d or "Name" in d or "PipelineDescription" in d


def test_ten_subjects_in_rawdata(lead_tutor_root: Path) -> None:
    subs = _subjects(lead_tutor_root)
    assert len(subs) == 10, f"expected 10 subjects, got {len(subs)}: {subs}"


def test_every_subject_resolves_to_a_t1_anchor_and_a_ct(lead_tutor_root: Path) -> None:
    for sid in _subjects(lead_tutor_root):
        sub = json.loads(hr.resolve_subject(str(lead_tutor_root), sid, use_index=True))
        assert sub["subj_id"] == sid
        assert sub["anchor_modality"] == "T1w", sid
        assert Path(sub["preop_raw"]["T1w"]).is_file()
        assert sub["postop_modality"] == "CT", sid
        assert Path(sub["postop_raw"]["CT"]).is_file()


def test_load_anchor_nifti(lead_tutor_root: Path) -> None:
    one = _subjects(lead_tutor_root)[0]
    sub = json.loads(hr.resolve_subject(str(lead_tutor_root), one, use_index=True))
    img = nib.load(str(Path(sub["preop_raw"][sub["anchor_modality"]])))
    assert len(img.shape) == 3
    assert all(z > 0 for z in img.header.get_zooms()[:3])
