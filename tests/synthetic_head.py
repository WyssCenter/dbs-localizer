# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

"""
Synthetic head for the end-to-end pipeline test (roadmap 4.4).

One analytic anatomy, sampled three ways: as the template (canonical pose, scale 1), as the
subject's pre-op T1w (scaled and rigidly moved) and as the subject's post-op CT (the same
scaled head in another rigid pose, with two leads in it). Every ground-truth coordinate is
known in closed form, so the test can check each stage against it.

The leads are Medtronic 3387 (insulating 1.5 mm tip, four 1.5 mm rings at 3.0 mm pitch),
painted as one continuous bloomed plateau the way a real post-op CT shows them.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

import nibabel as nib
import numpy as np
from numpy.typing import NDArray

__all__ = ("SyntheticHead", "mr_pose", "write_synthetic_dataset")

_HEAD_C = np.array([0.0, -15.0, 15.0])
_HEAD_AX = np.array([72.0, 92.0, 78.0])


def _rot(axis: int, deg: float) -> NDArray[np.float64]:
    t = np.radians(deg)
    c, s = np.cos(t), np.sin(t)
    r = np.eye(3)
    i, j = [a for a in range(3) if a != axis]
    r[i, i], r[i, j], r[j, i], r[j, j] = c, -s, s, c
    return r


def _ellipsoid(p: NDArray[np.float64], c, ax) -> NDArray[np.float64]:
    q = (p - np.asarray(c)) / np.asarray(ax)
    return np.sqrt((q * q).sum(axis=-1))


def _labels(c: NDArray[np.float64]) -> dict[str, NDArray[np.bool_]]:
    """Tissue masks at canonical coordinates ``c`` (``(..., 3)`` mm)."""
    r = _ellipsoid(c, _HEAD_C, _HEAD_AX)
    head = r <= 1.0
    scalp = head & (r > 0.93)
    skull = (r <= 0.93) & (r > 0.86)
    brain = r <= 0.86
    wm = _ellipsoid(c, _HEAD_C + [0, 5, 5], _HEAD_AX * [0.6, 0.62, 0.5]) <= 1.0
    vent = (_ellipsoid(c, [-9, -2, 14], [5, 22, 8]) <= 1.0) | (
        _ellipsoid(c, [9, -2, 14], [5, 22, 8]) <= 1.0
    )
    deep = (_ellipsoid(c, [-14, -12, -4], [7, 9, 7]) <= 1.0) | (
        _ellipsoid(c, [14, -12, -4], [7, 9, 7]) <= 1.0
    )
    stem = (_ellipsoid(c, [0, -30, -40], [12, 14, 35]) <= 1.0) & brain
    marker = _ellipsoid(c, [30, 35, 35], [10, 8, 9]) <= 1.0
    return {
        "head": head,
        "scalp": scalp,
        "skull": skull,
        "brain": brain & ~stem,
        "wm": wm & brain,
        "vent": vent & brain,
        "deep": deep & brain,
        "stem": stem,
        "marker": marker & brain,
    }


def _t1(c: NDArray[np.float64]) -> NDArray[np.float64]:
    m = _labels(c)
    v = np.zeros(c.shape[:-1])
    v[m["scalp"]] = 70.0
    v[m["skull"]] = 12.0
    v[m["brain"]] = 95.0
    v[m["stem"]] = 115.0
    v[m["wm"]] = 135.0
    v[m["deep"]] = 80.0
    v[m["marker"]] = 60.0
    v[m["vent"]] = 25.0
    return v


def _ct(c: NDArray[np.float64]) -> NDArray[np.float64]:
    m = _labels(c)
    v = np.full(c.shape[:-1], -1000.0)
    v[m["scalp"]] = 25.0
    v[m["skull"]] = 1300.0
    v[m["brain"] | m["stem"]] = 35.0
    v[m["wm"]] = 28.0
    v[m["vent"]] = 5.0
    return v


@dataclass(frozen=True)
class SyntheticHead:
    """Poses and lead geometry; ``scale`` maps canonical (template) mm to subject mm."""

    anchor_r: NDArray[np.float64]
    anchor_t: NDArray[np.float64]
    ct_r: NDArray[np.float64]
    ct_t: NDArray[np.float64]
    scale: float = 0.94

    @staticmethod
    def default() -> SyntheticHead:
        return SyntheticHead(
            scale=0.94,
            anchor_r=_rot(0, 6.0) @ _rot(2, -4.0),
            anchor_t=np.array([3.0, -5.0, 8.0]),
            ct_r=_rot(0, -9.0) @ _rot(1, 5.0),
            ct_t=np.array([-6.0, 4.0, -10.0]),
        )

    def canonical_to_anchor(self, c: NDArray[np.float64]) -> NDArray[np.float64]:
        return (self.scale * np.asarray(c)) @ self.anchor_r.T + self.anchor_t

    def anchor_to_canonical(self, a: NDArray[np.float64]) -> NDArray[np.float64]:
        return ((np.asarray(a) - self.anchor_t) @ self.anchor_r) / self.scale

    def canonical_to_ct(self, c: NDArray[np.float64]) -> NDArray[np.float64]:
        return (self.scale * np.asarray(c)) @ self.ct_r.T + self.ct_t

    def ct_to_canonical(self, w: NDArray[np.float64]) -> NDArray[np.float64]:
        return ((np.asarray(w) - self.ct_t) @ self.ct_r) / self.scale

    def lead_heads_canonical(self) -> dict[str, NDArray[np.float64]]:
        """Contact-0 centres in template mm, keyed by Reco v2 side (``"1"`` = right)."""
        return {"1": np.array([12.0, -13.0, -7.0]), "2": np.array([-12.0, -13.0, -7.0])}

    def lead_dirs_canonical(self) -> dict[str, NDArray[np.float64]]:
        d = {"1": np.array([0.25, 0.45, 1.0]), "2": np.array([-0.25, 0.45, 1.0])}
        return {k: v / np.linalg.norm(v) for k, v in d.items()}


def mr_pose(h: SyntheticHead) -> SyntheticHead:
    return SyntheticHead(
        scale=h.scale,
        anchor_r=h.anchor_r,
        anchor_t=h.anchor_t,
        ct_r=_rot(0, 5.0) @ _rot(2, 7.0),
        ct_t=np.array([5.0, -3.0, 6.0]),
    )


_MR_ARTIFACT_RADIUS_MM = 4.0
_EL_MODEL = "Medtronic 3387"
_TIP_MM = 1.5
_CONTACT_MM = 1.5
_PITCH_MM = 3.0


def _paint_leads(
    vol: NDArray[np.float64],
    world: NDArray[np.float64],
    h: SyntheticHead,
    values: tuple[float, float] = (3000.0, 2400.0),
    radius: float = 1.2,
) -> None:
    """Leads as a continuous bloomed metal plateau, the way a real post-op CT shows them."""
    for side, c0 in h.lead_heads_canonical().items():
        u_c = h.lead_dirs_canonical()[side]
        head = h.canonical_to_ct(c0)
        u = (h.scale * u_c) @ h.ct_r.T
        u = u / np.linalg.norm(u)
        d = world - head
        a = d @ u
        perp = np.linalg.norm(d - a[..., None] * u, axis=-1)
        start = -0.5 * _CONTACT_MM - 1.0
        body = (perp <= radius) & (a >= start) & (a <= 70.0)
        canon = h.ct_to_canonical(world[body])
        inside = _ellipsoid(canon, _HEAD_C, _HEAD_AX) <= 0.86
        idx = np.flatnonzero(body.reshape(-1))[inside]
        flat = vol.reshape(-1)
        flat[idx] = np.where(
            a.reshape(-1)[idx] <= 3 * _PITCH_MM + 0.5 * _CONTACT_MM,
            values[0],
            values[1],
        )


def _grid(
    lo: NDArray[np.float64], hi: NDArray[np.float64], vox: float
) -> tuple[tuple[int, int, int], NDArray[np.float64]]:
    n = np.ceil((hi - lo) / vox).astype(int) + 1
    aff = np.diag([vox, vox, vox, 1.0])
    aff[:3, 3] = lo
    return (int(n[0]), int(n[1]), int(n[2])), aff


def _world(shape, aff) -> NDArray[np.float64]:
    ii, jj, kk = np.meshgrid(*[np.arange(s) for s in shape], indexing="ij")
    ijk = np.stack([ii, jj, kk], axis=-1).astype(np.float64)
    return ijk @ aff[:3, :3].T + aff[:3, 3]


def _save(vol: NDArray[np.float64], aff: NDArray[np.float64], p: Path, dtype) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    img = nib.Nifti1Image(vol.astype(dtype), aff)
    img.set_qform(aff, code=1)
    img.set_sform(aff, code=1)
    img.to_filename(str(p))


def write_synthetic_dataset(
    root: Path,
    *,
    subj: str = "01",
    template_vox: float = 3.0,
    anchor_vox: float = 1.25,
    ct_vox: float = 1.0,
    mr_vox: float = 1.0,
    postop: str = "ct",
) -> tuple[Path, SyntheticHead]:
    """BIDS dataset under ``root`` plus ``root/template/{t1,brainmask}.nii.gz``; returns the template path."""
    if postop not in ("ct", "mr", "both"):
        raise ValueError(postop)
    h = SyntheticHead.default()
    ext = _HEAD_AX * 1.08

    shape, aff = _grid(_HEAD_C - ext, _HEAD_C + ext, template_vox)
    tpl = root / "template" / "t1.nii.gz"
    w_tpl = _world(shape, aff)
    _save(_t1(w_tpl), aff, tpl, np.float32)
    tissue = _labels(w_tpl)
    _save(
        (tissue["brain"] | tissue["stem"]).astype(np.uint8),
        aff,
        tpl.with_name("brainmask.nii.gz"),
        np.uint8,
    )

    centre_a = h.canonical_to_anchor(_HEAD_C)
    half = h.scale * ext.max()
    shape, aff = _grid(centre_a - half, centre_a + half, anchor_vox)
    w = _world(shape, aff)
    t1 = _t1(h.anchor_to_canonical(w))
    raw = root / "rawdata" / f"sub-{subj}"
    _save(
        t1,
        aff,
        raw / "ses-preop" / "anat" / f"sub-{subj}_ses-preop_T1w.nii.gz",
        np.float32,
    )

    if postop in ("ct", "both"):
        centre_c = h.canonical_to_ct(_HEAD_C)
        shape, aff = _grid(centre_c - half, centre_c + half, ct_vox)
        w = _world(shape, aff)
        ct = _ct(h.ct_to_canonical(w))
        _paint_leads(ct, w, h)
        _save(
            ct,
            aff,
            raw / "ses-postop" / "anat" / f"sub-{subj}_ses-postop_CT.nii.gz",
            np.int16,
        )
    if postop in ("mr", "both"):
        h_mr = mr_pose(h)
        centre_m = h_mr.canonical_to_ct(_HEAD_C)
        shape, aff = _grid(centre_m - half, centre_m + half, mr_vox)
        w = _world(shape, aff)
        mr = _t1(h_mr.ct_to_canonical(w))
        _paint_leads(mr, w, h_mr, values=(4.0, 4.0), radius=_MR_ARTIFACT_RADIUS_MM)
        _save(
            mr,
            aff,
            raw / "ses-postop" / "anat" / f"sub-{subj}_ses-postop_acq-ax_T1w.nii.gz",
            np.float32,
        )

    (root / "derivatives" / "leaddbs" / f"sub-{subj}").mkdir(
        parents=True, exist_ok=True
    )
    (root / "dataset_description.json").write_text(
        json.dumps(
            {"Name": "Wyss DBS Localizer synthetic head", "BIDSVersion": "1.8.0"}
        ),
        encoding="utf-8",
    )
    return tpl, h


EL_MODEL = _EL_MODEL
