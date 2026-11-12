# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""FastWarp: GPU SyN registration for Python, developed by Richard Köhler and Jan Philipp Payonk
as the registration engine of Wyss DBS Localizer.

The engine is a wgpu port of ANTs/ITK's SyN with the CC and Mattes MI metrics (Vulkan, Metal,
DX12). :func:`register_syn` runs the dbs-localizer cohort recipe (centre of mass, Rigid,
Affine, SyN on an isotropic grid) and writes ANTs-compatible composite transforms.
"""

from __future__ import annotations

from collections.abc import Mapping
from importlib.metadata import PackageNotFoundError, version
from os import PathLike
from pathlib import Path
from typing import Any

import numpy as np

from ._core import (
    ENGINE_SOURCE,
    CancelToken,
    GpuCapacityError,
    GpuUnavailableError,
    RegistrationCancelled,
    RegistrationError,
    gpu_info,
)
from ._core import LinearSession as _LinearSession
from ._core import apply_transforms as _apply_transforms
from ._core import check_linear_stage as _check_linear_stage
from ._core import cohort_recipe as _cohort_recipe
from ._core import compose_field as _compose_field
from ._core import fits_gpu as _fits_gpu
from ._core import register_syn as _register_syn

try:
    __version__ = version("fastwarp")
except PackageNotFoundError:  # pragma: no cover - source tree without metadata
    __version__ = "0+unknown"

__all__ = [
    "CITATION",
    "ENGINE_SOURCE",
    "CancelToken",
    "GpuCapacityError",
    "INTERPOLATORS",
    "GpuUnavailableError",
    "RegistrationCancelled",
    "RegistrationError",
    "LinearSession",
    "RigidSession",
    "__version__",
    "apply_transforms",
    "cohort_recipe",
    "compose_field",
    "gpu_available",
    "gpu_info",
    "iso_grid_dims",
    "linear_session",
    "register_rigid",
    "rigid_session",
    "register_syn",
    "fits_gpu",
]

#: Citation for the engine, for method logs and reference lists.
CITATION = (
    "Köhler RM, Payonk JP, Neumann WJ. FastWarp: the GPU registration engine of "
    "Wyss DBS Localizer. https://github.com/WyssCenter/fastwarp; 2026."
)


def _image_args(img: Any) -> tuple:
    """(flat x-fastest float64 data, size, spacing, origin, direction) in ITK (LPS) geometry.

    Accepts an ``ANTsImage`` (or anything with ``numpy()``, ``spacing``, ``origin``,
    ``direction``) or a tuple ``(array, spacing, origin, direction)`` with ``array[i, j, k]``
    the voxel at ITK index (i, j, k).
    """
    if isinstance(img, tuple):
        arr, spacing, origin, direction = img
    else:
        arr, spacing, origin, direction = img.numpy(), img.spacing, img.origin, img.direction
    arr = np.asarray(arr)
    if arr.dtype not in (np.float32, np.float64):
        arr = arr.astype(np.float32)
    if arr.ndim != 3:
        raise ValueError(f"expected a 3-D image, got shape {arr.shape}")
    # A view for the x-fastest (Fortran-ordered) arrays ANTs returns; copied only otherwise.
    flat = arr.ravel(order="F")
    d = np.asarray(direction, dtype=np.float64).reshape(3, 3)
    return (
        flat,
        tuple(int(n) for n in arr.shape),
        tuple(float(s) for s in spacing),
        tuple(float(o) for o in origin),
        tuple(tuple(float(v) for v in row) for row in d),
    )


def _as_list(x: Any) -> list[Any]:
    return x if isinstance(x, list) else [x]


def _prepared_prefix(out_prefix: str | PathLike[str]) -> str:
    """``out_prefix`` as a string, its folder created (checked before the registration runs)."""
    Path(out_prefix).parent.mkdir(parents=True, exist_ok=True)
    return str(out_prefix)


def register_syn(
    fixed: Any | list[Any],
    moving: Any | list[Any],
    out_prefix: str | PathLike[str],
    *,
    iso_mm: float = 1.0,
    write_warped: bool = False,
    adapter: str | None = None,
    cancel: CancelToken | None = None,
    recipe: Mapping[str, Mapping[str, Any]] | None = None,
) -> dict[str, Any]:
    """Register ``moving`` to ``fixed`` on the GPU: Rigid and Affine (Mattes MI, first channel),
    then SyN (CC, every channel), from a centre-of-mass start.

    ``fixed`` / ``moving``: one image, or a list of two (T1w + T2w) for the multispectral SyN
    stage, the moving T2w already in the moving T1w's space. Writes
    ``<out_prefix>Composite.h5`` and ``<out_prefix>InverseComposite.h5`` (ANTs layout:
    ``[Affine, Warp]`` and ``[InverseWarp, Affine^-1]``) plus ``<out_prefix>0GenericAffine.mat``.
    Returns timings and per-level diagnostics.

    ``recipe`` sets the schedule: ``{"rigid": {...}, "affine": {...}, "syn": {...}}`` with the
    fields :func:`cohort_recipe` returns. A stage or field left out keeps the dbs-localizer cohort
    value; an unknown key or a value the GPU kernels do not support (MI bins other than 32, a CC
    radius above 8) raises ``ValueError`` before any work. The result's ``"recipe"`` is the full
    recipe that ran.

    ``adapter`` picks a GPU by a substring of its name. ``cancel`` stops the run at the next
    iteration when another thread calls ``cancel.cancel()``; the GIL is released while it runs.

    Raises :class:`GpuUnavailableError` (no hardware GPU), :class:`GpuCapacityError` (the
    ``iso_mm`` grid does not fit; checked before any work), :class:`RegistrationCancelled` and
    :class:`RegistrationError`. The first two mean "use another engine".
    """
    return _register_syn(
        [_image_args(i) for i in _as_list(fixed)],
        [_image_args(i) for i in _as_list(moving)],
        _prepared_prefix(out_prefix),
        iso_mm=float(iso_mm),
        write_warped=write_warped,
        adapter=adapter,
        cancel=cancel,
        recipe=None if recipe is None else {k: _as_dict(v) for k, v in recipe.items()},
    )


def _as_dict(stage: Any) -> Any:
    return dict(stage) if isinstance(stage, Mapping) else stage


def cohort_recipe(channels: int = 1) -> dict[str, dict[str, Any]]:
    """The dbs-localizer cohort recipe for 1 or 2 channels as :func:`register_syn` takes it:
    ``MI[32, Regular 0.25]`` Rigid and Affine at ``1000x500x250x100``, then ``SyN[0.1,3,0]``
    with ``CC`` radius 4 at ``100x70x50x20`` (weights 0.5 each for two channels); shrink
    ``8x4x2x1``, smoothing ``3x2x1x0`` voxels."""
    return _cohort_recipe(channels)


def iso_grid_dims(img: Any, iso_mm: float = 1.0) -> tuple[int, int, int]:
    """Voxels per axis of the registration grid :func:`register_syn` builds from ``img``."""
    shape = img.shape if not isinstance(img, tuple) else np.shape(img[0])
    spacing = img.spacing if not isinstance(img, tuple) else img[1]
    out = []
    for n, sp in zip(shape, spacing, strict=True):
        if abs(sp - iso_mm) < 1e-6:
            out.append(int(n))
        else:
            out.append(max(int(np.floor((n - 1) * sp / iso_mm + 1e-9)) + 1, 1))
    return (out[0], out[1], out[2])


def fits_gpu(
    fixed: Any, *, iso_mm: float = 1.0, channels: int = 1, adapter: str | None = None
) -> bool:
    """Whether :func:`register_syn` of ``fixed`` at ``iso_mm`` fits the GPU (False without one)."""
    try:
        return _fits_gpu(iso_grid_dims(fixed, iso_mm), channels, adapter)
    except GpuUnavailableError:
        return False


def gpu_available(adapter: str | None = None) -> bool:
    """True when a usable hardware GPU adapter opens (software adapters only with
    ``FASTWARP_GPU_ALLOW_SOFTWARE=1``)."""
    try:
        gpu_info(adapter)
    except GpuUnavailableError:
        return False
    return True


#: Interpolators :func:`apply_transforms` runs (ANTs names).
INTERPOLATORS = ("linear", "nearestNeighbor", "lanczosWindowedSinc")


def _grid_args(ref: Any) -> tuple:
    """(size, spacing, origin, direction) of an ``ANTsImage`` or of a tuple
    ``(shape, spacing, origin, direction)``."""
    if isinstance(ref, tuple):
        shape, spacing, origin, direction = ref
    else:
        shape, spacing, origin, direction = ref.shape, ref.spacing, ref.origin, ref.direction
    if len(shape) != 3:
        raise ValueError(f"expected a 3-D reference, got shape {tuple(shape)}")
    d = np.asarray(direction, dtype=np.float64).reshape(3, 3)
    return (
        tuple(int(n) for n in shape),
        tuple(float(v) for v in spacing),
        tuple(float(v) for v in origin),
        tuple(tuple(float(v) for v in row) for row in d),
    )


def _transform_paths(transforms: Any) -> list[str]:
    items = [transforms] if isinstance(transforms, (str, PathLike)) else list(transforms)
    for t in items:
        if not Path(t).is_file():
            raise FileNotFoundError(f"transform file {t} does not exist")
    return [str(t) for t in items]


def compose_field(
    transforms: str | PathLike[str] | list[str | PathLike[str]],
    reference: Any,
    out_path: str | PathLike[str],
    *,
    compression_level: int | None = None,
) -> Path:
    """Write the transforms as one displacement field on ``reference``'s grid.

    The file ``antsApplyTransforms --compose`` / ``ants.apply_transforms(compose=...)`` writes
    (vectors in LPS mm, ``float32``), computed in parallel and written with parallel gzip when
    ``out_path`` ends in ``.nii.gz``. ``transforms`` is a list as ``ants.apply_transforms`` takes
    it, with the same meaning.
    ``compression_level`` (0 to 9) sets the gzip level; 0 stores the data in valid gzip without
    compressing it, which suits displacement fields (they barely compress).
    """
    out = Path(out_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    _compose_field(
        _transform_paths(transforms),
        _grid_args(reference),
        str(out),
        compression_level=compression_level,
    )
    return out


def apply_transforms(
    moving: Any,
    reference: Any,
    transforms: str | PathLike[str] | list[str | PathLike[str]],
    *,
    interpolation: str = "linear",
    default_value: float = 0.0,
) -> Any:
    """Resample ``moving`` onto ``reference`` through ``transforms`` (``antsApplyTransforms``).

    ``transforms`` is a list as ``ants.apply_transforms`` takes it, with the same meaning.
    ``interpolation`` is one of :data:`INTERPOLATORS`. Returns an ``ANTsImage`` like
    ``reference`` when it is one, else a ``float32`` array of the reference's shape. Points that
    map outside ``moving`` take ``default_value``.
    """
    size, spacing, origin, direction = _grid_args(reference)
    flat = _apply_transforms(
        _image_args(moving),
        (size, spacing, origin, direction),
        _transform_paths(transforms),
        interpolation=interpolation,
        default_value=float(default_value),
    )
    arr = flat.reshape(size, order="F")
    if hasattr(reference, "new_image_like"):
        return reference.new_image_like(arr)
    return arr


def _initial(initial: tuple[Any, Any] | None) -> tuple[tuple[float, ...], tuple[float, ...]] | None:
    if initial is None:
        return None
    params, center = initial
    if np.size(params) != 12 or np.size(center) != 3:
        raise ValueError("initial must be (12 affine parameters, 3-element centre)")
    return (
        tuple(float(v) for v in np.asarray(params, dtype=np.float64).ravel()),
        tuple(float(v) for v in np.asarray(center, dtype=np.float64).ravel()),
    )


class LinearSession:
    """Linear (rigid or affine) fits of one image pair on the GPU, the pair prepared once.

    Made by :func:`linear_session`. Each :meth:`register` runs one ``Rigid`` or ``Affine`` stage
    on Mattes MI with regular sampling from a given start; the images are converted, winsorized
    and uploaded once, and their smoothed copies are kept between fits, so a multi-start, or a
    rigid fit followed by an affine one, pays the preparation once. Use it in a ``with`` block, or
    :meth:`close` it, to release the GPU and the kept images.
    """

    def __init__(
        self,
        fixed: Any,
        moving: Any,
        *,
        fixed_mask: Any | None = None,
        winsorize: tuple[float, float] | None = None,
        adapter: str | None = None,
    ) -> None:
        self._core = _LinearSession(
            _image_args(fixed),
            _image_args(moving),
            fixed_mask=None if fixed_mask is None else _image_args(fixed_mask),
            winsorize=None if winsorize is None else (float(winsorize[0]), float(winsorize[1])),
            adapter=adapter,
        )

    @property
    def adapter(self) -> str:
        """The GPU in use."""
        return self._core.adapter

    @property
    def com_initial(self) -> tuple[tuple[float, ...], tuple[float, float, float]]:
        """The centre-of-mass start ``(parameters, center)``, what ``initial=None`` uses."""
        return self._core.com_initial

    def register(
        self,
        stage: Mapping[str, Any] | None = None,
        initial: tuple[Any, Any] | None = None,
        use_mask: bool = True,
        out_prefix: str | PathLike[str] | None = None,
        cancel: CancelToken | None = None,
        *,
        transform: str = "rigid",
        mask: Any | None = None,
    ) -> dict[str, Any]:
        """One ``transform`` (``"rigid"`` or ``"affine"``) fit from ``initial``.

        ``stage``: the schedule, the fields of ``cohort_recipe()["rigid"]`` (left out = that
        value). ``initial``: ``(parameters, center)`` of an ITK affine mapping fixed points to
        moving ones (12 parameters: the row-major matrix, then the translation); ``None`` starts
        from the centres of intensity mass, like ``--initial-moving-transform [f,m,1]``. The
        fitted transform starts as the identity, centred where ``initial`` is. With ``use_mask``
        the metric samples only the fixed points inside ``mask`` (an image on any grid, its
        non-zero voxels; ``None`` = the session's ``fixed_mask``, if any).

        Returns the collapsed affine (start then fit) as ``parameters`` / ``center`` (what
        ``0GenericAffine.mat`` holds, written as ``<out_prefix>0GenericAffine.mat`` when
        ``out_prefix`` is given), the final MI value ``metric`` (lower is better), the start,
        iterations per level and the time.
        """
        return self._core.register(
            stage=None if stage is None else dict(stage),
            initial=_initial(initial),
            use_mask=use_mask,
            out_prefix=None if out_prefix is None else str(out_prefix),
            cancel=cancel,
            transform=transform,
            mask=None if mask is None else _image_args(mask),
        )

    def close(self) -> None:
        """Release the GPU and the kept images."""
        self._core.close()

    def __enter__(self) -> LinearSession:
        return self

    def __exit__(self, *args: object) -> bool:
        self.close()
        return False


#: The 0.3 name of :class:`LinearSession`.
RigidSession = LinearSession


def linear_session(
    fixed: Any,
    moving: Any,
    *,
    fixed_mask: Any | None = None,
    winsorize: tuple[float, float] | None = None,
    adapter: str | None = None,
) -> LinearSession:
    """A :class:`LinearSession` for ``fixed`` / ``moving`` (``ANTsImage`` or tuples, as
    :func:`register_syn` takes them): the pair prepared once for several fits.

    ``fixed_mask``: an image on any grid whose non-zero voxels are where the metric samples (in
    fits with ``use_mask`` that bring no mask of their own). ``winsorize``: ``(lower, upper)``
    intensity quantiles, as ``--winsorize-image-intensities``.
    """
    return LinearSession(fixed, moving, fixed_mask=fixed_mask, winsorize=winsorize, adapter=adapter)


#: The 0.3 name of :func:`linear_session`.
rigid_session = linear_session


def register_rigid(
    fixed: Any,
    moving: Any,
    *,
    stage: Mapping[str, Any] | None = None,
    initial: tuple[Any, Any] | None = None,
    fixed_mask: Any | None = None,
    winsorize: tuple[float, float] | None = None,
    out_prefix: str | PathLike[str] | None = None,
    adapter: str | None = None,
    cancel: CancelToken | None = None,
) -> dict[str, Any]:
    """One rigid fit of ``moving`` onto ``fixed`` on the GPU (Mattes MI, regular sampling).

    ``stage``: the schedule, the fields of ``cohort_recipe()["rigid"]`` (left out = that value).
    ``initial``: ``(parameters, center)`` of an ITK affine mapping fixed points to moving ones
    (12 parameters: the row-major matrix, then the translation); ``None`` starts from the
    centres of intensity mass, like ``antsRegistration --initial-moving-transform [f,m,1]``.
    ``fixed_mask`` / ``winsorize``: as :func:`rigid_session`. For several fits of one pair (a
    multi-start), a :func:`rigid_session` prepares the images once.

    Returns the collapsed affine as ``parameters`` / ``center`` (what ``0GenericAffine.mat`` holds,
    written as ``<out_prefix>0GenericAffine.mat`` when ``out_prefix`` is given), the final MI
    value ``metric`` (lower is better), the start, iterations per level and the time.
    """
    # Everything a caller can get wrong is refused before the GPU is opened.
    stage_dict = _check_linear_stage(None if stage is None else dict(stage), "rigid")
    start = _initial(initial)
    prefix = None if out_prefix is None else _prepared_prefix(out_prefix)
    if prefix is not None and not Path(prefix).parent.is_dir():
        raise ValueError(f"output folder {Path(prefix).parent} does not exist")
    if winsorize is not None and not 0.0 <= winsorize[0] < winsorize[1] <= 1.0:
        raise ValueError("winsorize must be (lower, upper) quantiles with 0 <= lower < upper <= 1")
    with linear_session(
        fixed, moving, fixed_mask=fixed_mask, winsorize=winsorize, adapter=adapter
    ) as session:
        return session.register(
            stage=stage_dict,
            initial=start,
            use_mask=fixed_mask is not None,
            out_prefix=prefix,
            cancel=cancel,
        )
