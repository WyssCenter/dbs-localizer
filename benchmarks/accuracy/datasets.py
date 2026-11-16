# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""The benchmark datasets: where each file comes from, its pinned SHA-256, and the subjects.

The pins live in ``pins.json`` beside this file (one list of files per source). A file whose
``sha256`` is ``null`` is a placeholder: ``fetch`` refuses it until the pin is filled in with
``python -m benchmarks.accuracy pin``. Licences and citations are in ``docs/benchmarks.md``.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from functools import cache
from pathlib import Path

PINS = Path(__file__).with_name("pins.json")

#: DKT31 cortical labels (Klein and Tourville 2012), left 1002-1035 and right 2002-2035: the
#: Desikan-Killiany regions without the banks of the STS (1), corpus callosum (4) and poles.
DKT31 = tuple(h * 1000 + n for h in (1, 2) for n in range(2, 36) if n not in (4, 32, 33))

#: Manually labelled subcortical structures of the OASIS-TRT-20 "CMA" labels (FreeSurfer
#: numbers): thalamus, caudate, putamen, pallidum, hippocampus, amygdala, accumbens, ventral DC.
CMA_SUBCORTICAL = (10, 11, 12, 13, 17, 18, 26, 28, 49, 50, 51, 52, 53, 54, 58, 60)

CMA_NAMES = {
    10: "Left-Thalamus",
    11: "Left-Caudate",
    12: "Left-Putamen",
    13: "Left-Pallidum",
    17: "Left-Hippocampus",
    18: "Left-Amygdala",
    26: "Left-Accumbens",
    28: "Left-VentralDC",
    49: "Right-Thalamus",
    50: "Right-Caudate",
    51: "Right-Putamen",
    52: "Right-Pallidum",
    53: "Right-Hippocampus",
    54: "Right-Amygdala",
    58: "Right-Accumbens",
    60: "Right-VentralDC",
}


#: The deep nuclei of the 7 T masks (Isaacs et al. 2020), numbered for this benchmark.
DEEP_NUCLEI = {
    1: "STN_L",
    2: "STN_R",
    3: "SN_L",
    4: "SN_R",
    5: "RN_L",
    6: "RN_R",
    7: "GPe_L",
    8: "GPe_R",
    9: "GPi_L",
    10: "GPi_R",
}


@dataclass(frozen=True)
class Download:
    """One pinned file. ``path`` is where it is stored under the dataset folder; an archive
    lists ``extract`` rules ``(member regex, output path template)`` and is deleted after
    extraction unless ``--keep-archives`` is given."""

    path: str
    url: str
    sha256: str | None
    size: int | None
    extract: tuple[tuple[str, str], ...] = ()

    def select(self, member: str) -> str | None:
        """Output path of an archive member (first matching rule), or ``None`` to skip it."""
        for pattern, template in self.extract:
            m = re.fullmatch(pattern, member)
            if m:
                return m.expand(template)
        return None


@dataclass(frozen=True)
class Subject:
    """One image with its ground truth, paths relative to the dataset folder: a label map
    (``labels``), one binary mask per structure (``masks``: ``(label, path)``), or landmarks.
    ``mask`` is the brain mask the field statistics are taken in."""

    id: str
    image: str
    labels: str | None = None
    landmarks: str | None = None
    mask: str | None = None
    masks: tuple[tuple[int, str], ...] = ()


@dataclass(frozen=True)
class Dataset:
    """A dataset, the protocol it is registered with and how its ground truth is grouped."""

    key: str
    title: str
    licence: str
    downloads: tuple[Download, ...]
    subjects: tuple[Subject, ...]
    protocol: str
    groups: dict[str, tuple[int, ...]] = field(default_factory=dict)
    label_names: dict[int, str] = field(default_factory=dict)
    template: Subject | None = None
    crop_to_foreground: bool = False
    default_pairs: int = 10
    quick_pairs: int = 1
    in_default_run: bool = True
    note: str = ""

    def subject(self, sid: str) -> Subject:
        """The subject (or template) with id ``sid``."""
        if self.template is not None and sid == self.template.id:
            return self.template
        for s in self.subjects:
            if s.id == sid:
                return s
        raise KeyError(f"{self.key} has no subject {sid!r}")

    @property
    def ground_truth(self) -> str:
        """``"labels"`` or ``"landmarks"``."""
        s = self.subjects[0] if self.subjects else None
        return "landmarks" if s is not None and s.landmarks else "labels"

    @property
    def download_bytes(self) -> int:
        """Total size of the downloads (unknown sizes count as 0)."""
        return sum(d.size or 0 for d in self.downloads)


@cache
def _pins() -> dict[str, list[dict]]:
    return json.loads(PINS.read_text(encoding="utf-8"))


def _files(source: str, *, prefix: str = "") -> list[Download]:
    return [
        Download(prefix + f["path"], f["url"], f.get("sha256"), f.get("size"))
        for f in _pins()[source]
    ]


def _archive(source: str, extract: tuple[tuple[str, str], ...]) -> list[Download]:
    return [
        Download(f["path"], f["url"], f.get("sha256"), f.get("size"), extract)
        for f in _pins()[source]
    ]


def _mni2009c(prefix: str) -> tuple[list[Download], Subject]:
    files = _files("mni2009c", prefix=prefix)
    tpl = Subject(
        id="MNI152NLin2009cAsym",
        image=prefix + "tpl-MNI152NLin2009cAsym_res-01_T1w.nii.gz",
        landmarks=prefix + "tpl-MNI152NLin2009cAsym_res-01_desc-groundtruth_afids.fcsv",
        mask=prefix + "tpl-MNI152NLin2009cAsym_res-01_desc-brain_mask.nii.gz",
    )
    return files, tpl


def _afids_openneuro(source: str) -> tuple[list[Download], list[Subject]]:
    files = _files(source)
    images: dict[str, list[str]] = {}
    marks: dict[str, list[str]] = {}
    for f in files:
        if m := re.fullmatch(r"sub-([^/]+)/anat/[^/]+_T1w\.nii\.gz", f.path):
            images.setdefault(m.group(1), []).append(f.path)
        elif m := re.fullmatch(
            r"derivatives/afids_groundtruth/sub-([^/]+)/anat/[^/]+\.fcsv", f.path
        ):
            marks.setdefault(m.group(1), []).append(f.path)
    subjects = []
    for sid in sorted(images):
        if len(images[sid]) != 1 or len(marks.get(sid, [])) != 1:
            raise ValueError(f"{source}: subject {sid} needs exactly one T1w and one fcsv")
        subjects.append(Subject(id=sid, image=images[sid][0], landmarks=marks[sid][0]))
    return files, subjects


def _mindboggle() -> Dataset:
    volumes = _archive(
        "mindboggle_volumes",
        (
            (
                r"OASIS-TRT-20_volumes/(OASIS-TRT-20-\d+)/t1weighted_brain\.nii\.gz",
                r"brains/\1_t1weighted_brain.nii.gz",
            ),
        ),
    )
    labels = _files("mindboggle_labels")
    ids = sorted(
        (re.search(r"(OASIS-TRT-20-\d+)_", d.path).group(1) for d in labels),
        key=lambda s: int(s.rsplit("-", 1)[1]),
    )
    subjects = tuple(
        Subject(
            id=sid,
            image=f"brains/{sid}_t1weighted_brain.nii.gz",
            labels=f"labels/{sid}_DKT31_CMA_labels.nii.gz",
            mask=f"brains/{sid}_t1weighted_brain.nii.gz",
        )
        for sid in ids
    )
    return Dataset(
        key="mindboggle",
        title="Mindboggle-101 OASIS-TRT-20: DKT31 cortical and CMA subcortical labels",
        licence="cortical labels and images CC BY 4.0; subcortical labels CC BY-NC-ND 4.0",
        downloads=(*volumes, *labels),
        subjects=subjects,
        protocol="pairwise",
        groups={"cortical": DKT31, "subcortical": CMA_SUBCORTICAL},
        label_names=CMA_NAMES,
        crop_to_foreground=True,
        default_pairs=20,
        quick_pairs=1,
    )


def _snsx() -> Dataset:
    files, subjects = _afids_openneuro("snsx")
    tpl_files, tpl = _mni2009c("template/")
    return Dataset(
        key="snsx",
        title="SNSX 7T (OpenNeuro ds004470): AFIDs landmarks, subjects onto MNI 2009c",
        licence="images and landmarks CC0 1.0; template: MNI ICBM 2009c licence",
        downloads=(*files, *tpl_files),
        subjects=tuple(subjects),
        protocol="template",
        template=tpl,
        default_pairs=20,
        quick_pairs=1,
    )


def _lhscpd() -> Dataset:
    files, subjects = _afids_openneuro("lhscpd")
    tpl_files, tpl = _mni2009c("template/")
    return Dataset(
        key="lhscpd",
        title="LHSCPD 1.5T (OpenNeuro ds004471): AFIDs landmarks, subjects onto MNI 2009c",
        licence="images and landmarks CC0 1.0; template: MNI ICBM 2009c licence",
        downloads=(*files, *tpl_files),
        subjects=tuple(subjects),
        protocol="template",
        template=tpl,
        default_pairs=40,
        quick_pairs=1,
        in_default_run=False,
        note="gadolinium-enhanced T1w of patients with Parkinson's disease; optional",
    )


def _deep7t() -> Dataset:
    sub = r"osf_dataset/M_7T/(M\d{4})_7T/"
    wb = sub + r"MRimages/Nii_STD_BET/WB/\1_7T_WB_"
    archive = _archive(
        "deep7t",
        (
            (wb + r"MP2RAGE_STD_BET\.nii\.gz", r"\1/mp2rage_brain.nii.gz"),
            (wb + r"INV2_e1_M_STD_BET_mask\.nii\.gz", r"\1/brain_mask.nii.gz"),
            (
                sub + r"Masks/(STN|SN|RN|GPe|GPi)/calc/\1_7T_\2_([LR])_AA-BI_conj\.nii\.gz",
                r"\1/\2_\3.nii.gz",
            ),
        ),
    )
    ids = ("M0164", "M1334", "M4514", "M5342", "M6982", "M7038", "M7337", "M7703", "M9740", "M9773")
    subjects = tuple(
        Subject(
            id=sid,
            image=f"{sid}/mp2rage_brain.nii.gz",
            mask=f"{sid}/brain_mask.nii.gz",
            masks=tuple((label, f"{sid}/{name}.nii.gz") for label, name in DEEP_NUCLEI.items()),
        )
        for sid in ids
    )
    return Dataset(
        key="deep7t",
        title="7 T MP2RAGE with manual STN, SN, RN, GPe and GPi masks (Isaacs et al. 2020)",
        licence="Creative Commons Attribution according to the article; no licence on OSF",
        downloads=tuple(archive),
        subjects=subjects,
        protocol="pairwise",
        groups={"deep_nuclei": tuple(DEEP_NUCLEI)},
        label_names=DEEP_NUCLEI,
        crop_to_foreground=True,
        default_pairs=10,
        quick_pairs=1,
    )


@cache
def registry() -> dict[str, Dataset]:
    """Every dataset by key."""
    out = {}
    for make in (_mindboggle, _deep7t, _snsx, _lhscpd):
        d = make()
        out[d.key] = d
    return out


def get(key: str) -> Dataset:
    """The dataset ``key``."""
    reg = registry()
    if key not in reg:
        raise KeyError(f"unknown dataset {key!r}; choose from {sorted(reg)}")
    return reg[key]


def default_keys() -> list[str]:
    """The datasets of a run without ``--dataset``."""
    return [k for k, d in registry().items() if d.in_default_run]
