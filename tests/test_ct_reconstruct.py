# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""Post-op CT reconstruction through the Rust binding (``dbsloc_reconstruct.reconstruct_ct``)."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

hr = pytest.importorskip("dbsloc_reconstruct", reason="PyO3 extension not built")


def _inputs(tmp: Path) -> dict[str, str]:
    for d in ("coreg", "norm", "recon"):
        (tmp / d).mkdir(exist_ok=True)
    ct = tmp / "coreg" / "sub-1_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz"
    ct.write_bytes(b"")
    inverse = tmp / "norm" / "sub-1_from-MNI_to-anchorNative_mode-image_xfm.h5"
    inverse.write_bytes(b"")
    tpl = tmp / "t1.nii"
    tpl.write_bytes(b"")
    return {
        "ct": str(ct),
        "template": str(tpl),
        "norm_inverse_composite": str(inverse),
    }


def test_explicit_native_method_without_a_template_is_an_os_error() -> None:
    with pytest.raises(OSError, match="E_RECON_native: no template given"):
        hr.reconstruct_ct("1", method="trac_core_native", el_model="Medtronic 3387")


def test_blockers_name_the_missing_piece(tmp_path: Path) -> None:
    kw = {"method": "trac_core_native", "el_model": "Medtronic 3387"}
    tpl = tmp_path / "t1.nii"
    with pytest.raises(OSError, match="does not exist"):
        hr.reconstruct_ct("1", template=str(tpl), **kw)
    tpl.write_bytes(b"")
    with pytest.raises(OSError, match="no coregistered post-op CT"):
        hr.reconstruct_ct("1", template=str(tpl), **kw)
    (tmp_path / "coreg").mkdir()
    ct = (
        tmp_path
        / "coreg"
        / "sub-1_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz"
    )
    ct.write_bytes(b"")
    with pytest.raises(OSError, match="no native->template transform"):
        hr.reconstruct_ct("1", template=str(tpl), ct=str(ct), **kw)


def test_an_unresolved_electrode_is_reported_once_the_inputs_exist(
    tmp_path: Path,
) -> None:
    with pytest.raises(ValueError, match="E_RECON_elmodel"):
        hr.reconstruct_ct("1", method="trac_core_native", **_inputs(tmp_path))
    with pytest.raises(ValueError, match="from uiprefs"):
        hr.reconstruct_ct(
            "1",
            method="trac_core_native",
            el_model_error="E_RECON_elmodel: from uiprefs",
            **_inputs(tmp_path),
        )


def test_a_broken_ct_is_an_input_error(tmp_path: Path) -> None:
    with pytest.raises(OSError, match="E_RECON_input"):
        hr.reconstruct_ct(
            "1", method="trac_core", el_model="Medtronic 3387", **_inputs(tmp_path)
        )


def test_unknown_method_is_refused_by_rust() -> None:
    with pytest.raises(ValueError, match="unknown recon_method"):
        hr.reconstruct_ct("1", method="bogus")


def test_a_pacer_import_needs_no_electrode(tmp_path: Path) -> None:
    mk = {"head": [0, 0, 0], "tail": [0, 0, 5], "x": [1, 0, 0], "y": [0, 1, 0]}
    imp = tmp_path / "sub-1_desc-pacer_import.json"
    imp.write_text(
        json.dumps(
            {
                "version": 2,
                "subj_id": "1",
                "method": "pacer",
                "native": {"1": {"markers": mk}},
                "mni": {"1": {"markers": mk}},
            }
        ),
        encoding="utf-8",
    )
    r = json.loads(hr.reconstruct_ct("1", method="pacer", pacer_import=str(imp)))
    assert r["meta"]["pacer"] == "import_json"


def test_thread_counts_are_clamped_not_rejected() -> None:
    for n in (-3, 0, 2**40):
        with pytest.raises(OSError, match="no template given"):
            hr.reconstruct_ct("1", method="trac_core_native", ants_threads=n)
