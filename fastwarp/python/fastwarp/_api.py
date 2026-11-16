# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""Implementation of the public FastWarp API; import the names from ``fastwarp``."""

from __future__ import annotations

import math
import warnings
from collections.abc import Mapping, Sequence
from os import PathLike, fspath
from pathlib import Path
from typing import Any

import numpy as np

from ._core import (
    CancelToken,
    GpuUnavailableError,
    gpu_info,
)
from ._core import LinearSession as _LinearSession
from ._core import apply_transforms as _apply_transforms
from ._core import check_linear_stage as _check_linear_stage
from ._core import cohort_recipe as _cohort_recipe
from ._core import compose_field as _compose_field
from ._core import fits_gpu as _fits_gpu
from ._core import register_syn as _register_syn

#: The citation of FastWarp as one line of text, for method sections and logs.
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


def _warn_skipped(result: dict[str, Any], stacklevel: int = 3) -> dict[str, Any]:
    """Emits each engine warning of ``result`` (a level skipped for lack of samples)."""
    for w in result.get("warnings", ()):
        warnings.warn(f"fastwarp: {w}", UserWarning, stacklevel=stacklevel)
    return result


def _as_list(x: Any) -> list[Any]:
    return x if isinstance(x, list) else [x]


def _prefix_folder(prefix: str) -> Path:
    return Path(prefix + "Composite.h5").parent


def _prepared_prefix(out_prefix: str | PathLike[str]) -> str:
    """``out_prefix`` as a string, a trailing separator kept, its folder created (checked before
    the registration runs)."""
    prefix = fspath(out_prefix)
    _prefix_folder(prefix).mkdir(parents=True, exist_ok=True)
    return prefix


#: The output layouts :func:`register_syn` writes, the values of its ``outputs`` argument.
OUTPUT_LAYOUTS = ("composite", "split", "both")


def register_syn(
    fixed: Any | list[Any],
    moving: Any | list[Any],
    out_prefix: str | PathLike[str],
    *,
    iso_mm: float = 1.0,
    write_warped: bool = False,
    adapter: str | None = None,
    cancel: CancelToken | None = None,
    recipe: Mapping[str, Any] | None = None,
    outputs: str = "composite",
) -> dict[str, Any]:
    """Register ``moving`` onto ``fixed`` on the GPU and write ANTs transform files.

    By default the registration runs a centre-of-mass start, a rigid and an affine stage (Mattes
    mutual information on the first image of each side), then a SyN stage (symmetric normalization,
    the deformable registration of ANTs; neighbourhood cross-correlation on every image), on an
    isotropic grid of ``iso_mm`` millimetres built from the first fixed image. A stage-list
    ``recipe`` can choose another start and other stages.

    Parameters
    ----------
    fixed : ANTsImage, tuple or list
        The image to register onto, or a list of two images (for example a T1-weighted and a
        T2-weighted scan). An image is an ``ANTsImage`` or a tuple
        ``(array, spacing, origin, direction)`` in ITK (LPS) geometry.
    moving : ANTsImage, tuple or list
        The image to register, or a list of as many images as ``fixed``. A second moving image
        must already be aligned to the first one.
    out_prefix : str or os.PathLike
        Prefix of the output files, joined to the file names as text, as in ANTs: ``"out/sub_"``
        gives ``out/sub_Composite.h5``. A string that ends in a path separator, such as
        ``"out/sub/"``, names a folder (a :class:`pathlib.Path` drops the trailing separator). The
        folder is created when it is missing.
    iso_mm : float, default 1.0
        Voxel size of the registration grid, in millimetres. Must be positive.
    write_warped : bool, default False
        Also write ``<out_prefix>_Warped.nii.gz``: the first moving image warped onto the
        registration grid.
    adapter : str, optional
        Use the GPU whose name contains this text (case-insensitive). By default the
        ``FASTWARP_GPU_ADAPTER`` environment variable applies, if set, and otherwise FastWarp
        chooses.
    cancel : CancelToken, optional
        A token that another thread can use to stop the run.
    recipe : dict, optional
        The registration schedule, in one of two forms. The cohort form
        ``{"rigid": {...}, "affine": {...}, "syn": {...}}`` takes the fields :func:`cohort_recipe`
        returns. The stage-list form ``{"initial": ..., "stages": [...]}`` names the start
        (``"com"``, the default; ``"identity"``; a ``pathlib.Path`` or ``{"path": ..., "invert":
        bool}`` to a file with one affine; or ``(parameters, center)``) and the stages (dicts with
        ``"transform"`` set to ``"rigid"``, ``"affine"`` or ``"syn"``: any number of linear stages,
        then at most one SyN stage). Both forms take ``"winsorize"``: ``[lower, upper]``
        quantiles (default ``[0.005, 0.995]``) or ``None`` for a plain rescale to [0, 1]. A stage
        or field left out keeps its default.
    outputs : {"composite", "split", "both"}, default "composite"
        Which transform files to write: the composites ``Composite.h5`` and
        ``InverseComposite.h5`` with ``0GenericAffine.mat``; ANTs' separate files
        ``0GenericAffine.mat``, ``1Warp.nii.gz`` and ``1InverseWarp.nii.gz``; or both. See
        :data:`OUTPUT_LAYOUTS`.

    Returns
    -------
    dict
        Timings in seconds (``total_s``, ``resample_s``, ``preprocess_s``, ``linear_s``, ``syn_s``,
        ``warp_s``, ``write_s``), the GPU with its graphics API and type (``adapter``), the
        registration grid size (``grid``), one ``(level, grid_size, iterations, converged,
        energy)`` tuple per SyN level (``levels``, empty without SyN), the combined affine
        (``affine_parameters``, ``affine_center``), the start (``initial_parameters``,
        ``initial_center``), one dict per linear stage (``linear_stages``), whether a SyN stage ran
        (``deformable``), the paths written (``files``), ``peak_gpu_bytes``,
        ``memory_budget_bytes``, the full ``recipe`` that ran, and ``warnings``: one message per
        linear level that was skipped because its samples left the metric undefined (each is
        also emitted as a ``UserWarning``).

    Raises
    ------
    ValueError
        An argument or the recipe is invalid. Checked before the GPU is opened.
    GpuUnavailableError
        No usable GPU. Raised before any work, so the caller can run ANTs instead.
    GpuCapacityError
        The stages of the recipe do not fit the GPU's memory. Raised before any work.
    RegistrationCancelled
        ``cancel`` was cancelled. No files are written.
    RegistrationError
        The registration failed, for example on an image with NaN voxels.

    See Also
    --------
    fits_gpu : Check the GPU memory without registering.
    cohort_recipe : The default recipe.
    linear_session : Rigid or affine fits only.

    Notes
    -----
    The call releases Python's global interpreter lock while it runs, so other Python threads keep
    running. With the default outputs the files are ``<out_prefix>Composite.h5`` (warps the moving
    image onto the fixed image), ``<out_prefix>InverseComposite.h5`` (the inverse) and
    ``<out_prefix>0GenericAffine.mat`` (the affine part).
    """
    result = _register_syn(
        [_image_args(i) for i in _as_list(fixed)],
        [_image_args(i) for i in _as_list(moving)],
        _prepared_prefix(out_prefix),
        iso_mm=float(iso_mm),
        write_warped=write_warped,
        adapter=adapter,
        cancel=cancel,
        recipe=_recipe_arg(recipe),
        outputs=outputs,
    )
    return _warn_skipped(result)


def _as_dict(stage: Any) -> Any:
    return dict(stage) if isinstance(stage, Mapping) else stage


def _initial_arg(initial: Any) -> Any:
    if isinstance(initial, PathLike):
        return {"path": fspath(initial)}
    if isinstance(initial, Mapping):
        d = dict(initial)
        if isinstance(d.get("path"), PathLike):
            d["path"] = fspath(d["path"])
        return d
    if isinstance(initial, (tuple, list)) and len(initial) == 2:
        params, center = _initial((initial[0], initial[1]))
        return {"parameters": list(params), "center": list(center)}
    return initial


def _recipe_arg(recipe: Mapping[str, Any] | None) -> dict[str, Any] | None:
    """``recipe`` as plain dicts and lists for the extension: mappings copied, the stage list
    made a list of dicts, a path start made ``{"path": str}``."""
    if recipe is None:
        return None
    out: dict[str, Any] = {}
    for k, v in recipe.items():
        if k == "stages" and isinstance(v, Sequence) and not isinstance(v, (str, bytes)):
            out[k] = [_as_dict(stage) for stage in v]
        elif k == "initial":
            out[k] = _initial_arg(v)
        elif k == "winsorize" and isinstance(v, tuple):
            out[k] = list(v)
        else:
            out[k] = _as_dict(v)
    return out


def cohort_recipe(channels: int = 1) -> dict[str, dict[str, Any]]:
    """Return the default registration recipe in full, in the cohort form.

    The rigid and affine stages use Mattes mutual information with 32 histogram bins on 25 % of
    the voxels, sampled on a regular grid, and at most 1000, 500, 250 and 100 iterations on four
    levels. The SyN stage uses cross-correlation in a window of radius 4 voxels, a gradient step of
    0.1, an update-field variance of 3 voxels² and no total-field smoothing, with at most 100, 70,
    50 and 20 iterations. All stages shrink the images by 8, 4, 2 and 1 and smooth them with
    Gaussian sigmas of 3, 2, 1 and 0 voxels. It is the recipe of Wyss DBS Localizer.

    Parameters
    ----------
    channels : int, default 1
        Number of images per side, 1 or 2. With 2 the SyN stage weights each channel 0.5.

    Returns
    -------
    dict
        ``{"rigid": {...}, "affine": {...}, "syn": {...}}`` with every field present, in the form
        :func:`register_syn` takes as ``recipe``.

    Raises
    ------
    ValueError
        ``channels`` is not 1 or 2.
    """
    return _cohort_recipe(channels)


def iso_grid_dims(img: Any, iso_mm: float = 1.0) -> tuple[int, int, int]:
    """Return the size of the registration grid that :func:`register_syn` builds.

    Along each axis the grid has ``floor((n - 1) * spacing / iso_mm) + 1`` voxels, where ``n`` and
    ``spacing`` are the image's size and spacing along that axis; an axis whose spacing already
    equals ``iso_mm`` keeps its size.

    Parameters
    ----------
    img : ANTsImage or tuple
        The fixed image, as an ``ANTsImage`` or a tuple ``(array, spacing, origin, direction)``.
    iso_mm : float, default 1.0
        Voxel size of the registration grid, in millimetres.

    Returns
    -------
    tuple of int
        Voxels along each of the three axes.
    """
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
    fixed: Any,
    moving: Any | None = None,
    *,
    iso_mm: float = 1.0,
    channels: int | None = None,
    adapter: str | None = None,
    recipe: Mapping[str, Any] | None = None,
) -> bool:
    """Check whether :func:`register_syn` would fit the GPU's memory, without registering.

    Runs the same capacity check as :func:`register_syn`. Returns ``False`` when there is no usable
    GPU.

    Parameters
    ----------
    fixed : ANTsImage, tuple or list
        The fixed image, or a list of one or two, as :func:`register_syn` takes them. The first
        fixed image sets the registration grid.
    moving : ANTsImage, tuple or list, optional
        The moving image or images, as many as ``fixed``. The largest of their registration grids
        counts. Without it, the moving image is assumed to have the fixed image's grid, which
        undercounts when the moving image is larger; pass it for the exact answer.
    iso_mm : float, default 1.0
        Voxel size of the registration grid, in millimetres. Must be positive.
    channels : int, optional
        Number of channels, 1 or 2. Defaults to the number of fixed images. Give ``channels=2``
        to ask about a second channel while passing one image.
    adapter : str, optional
        Check the GPU whose name contains this text.
    recipe : dict, optional
        The recipe, as :func:`register_syn` takes it; by default the default recipe. Each phase is
        checked on its own: a recipe without a SyN stage needs much less memory.

    Returns
    -------
    bool
        ``True`` when the registration fits, ``False`` when it does not or there is no GPU.

    Raises
    ------
    ValueError
        Not one or two fixed images, not as many moving images as fixed ones, ``channels`` not 1
        or 2 or not equal to the number of fixed images in a list, ``iso_mm`` not positive, or an
        invalid recipe.

    See Also
    --------
    iso_grid_dims : The size of the registration grid.
    gpu_info : The GPU and its memory budget.
    """
    fixed_list = _as_list(fixed)
    if not 1 <= len(fixed_list) <= 2:
        raise ValueError("give 1 or 2 fixed images")
    moving_list = None if moving is None else _as_list(moving)
    if moving_list is not None and len(moving_list) != len(fixed_list):
        raise ValueError("give as many moving images as fixed images")
    if channels is None:
        channels = len(fixed_list)
    elif isinstance(fixed, list) and channels != len(fixed_list):
        raise ValueError(f"channels={channels} does not match {len(fixed_list)} fixed images")
    elif not 1 <= channels <= 2:
        raise ValueError("channels must be 1 or 2")
    iso_mm = float(iso_mm)
    if not (math.isfinite(iso_mm) and iso_mm > 0.0):
        raise ValueError("iso_mm must be a positive number")
    fixed_img = fixed_list[0]
    moving_dims = None
    if moving_list is not None:
        grids = [iso_grid_dims(m, iso_mm) for m in moving_list]
        moving_dims = max(grids, key=lambda d: d[0] * d[1] * d[2])
    try:
        return _fits_gpu(
            iso_grid_dims(fixed_img, iso_mm), channels, adapter, moving_dims, _recipe_arg(recipe)
        )
    except GpuUnavailableError:
        return False


def gpu_available(adapter: str | None = None) -> bool:
    """Return whether a usable GPU opens.

    Software adapters, which emulate a GPU on the CPU, count only when
    ``FASTWARP_GPU_ALLOW_SOFTWARE=1`` is set.

    Parameters
    ----------
    adapter : str, optional
        Check the GPU whose name contains this text.

    Returns
    -------
    bool
        ``True`` when :func:`gpu_info` would succeed.
    """
    try:
        gpu_info(adapter)
    except GpuUnavailableError:
        return False
    return True


#: The interpolators :func:`apply_transforms` accepts, by their ANTs names.
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


def _invert_flags(invert: Any, count: int) -> list[bool] | None:
    """``invert`` as one ``bool`` per transform (``None`` stays ``None``: nothing inverted)."""
    if invert is None:
        return None
    if isinstance(invert, np.ndarray) and invert.ndim == 1 and invert.dtype == bool:
        invert = invert.tolist()
    if isinstance(invert, (bool, np.bool_)) or not isinstance(invert, Sequence):
        raise ValueError("invert must be a list with one bool per transform")
    flags = list(invert)
    if len(flags) != count:
        raise ValueError(f"invert has {len(flags)} flags for {count} transforms")
    if not all(isinstance(f, (bool, np.bool_)) for f in flags):
        raise ValueError("invert must hold bools (True inverts that transform)")
    return [bool(f) for f in flags]


def compose_field(
    transforms: str | PathLike[str] | list[str | PathLike[str]],
    reference: Any,
    out_path: str | PathLike[str],
    *,
    compression_level: int | None = None,
    invert: Sequence[bool] | None = None,
) -> Path:
    """Write a list of transforms as one displacement field on a reference grid.

    This is the file that ``antsApplyTransforms -o [field.nii.gz,1]`` writes: a NIfTI image of
    ``float32`` displacement vectors in LPS millimetres. It runs in parallel on the CPU and needs no
    GPU.

    Parameters
    ----------
    transforms : str, os.PathLike or list
        One transform file or a list of them, with the same meaning as the ``transformlist`` of
        ``ants.apply_transforms``: a point of the reference grid goes through the first file first.
        Files can be ``.h5``, ``.mat``, ``.txt``, ``.tfm``, ``.nii`` or ``.nii.gz``.
    reference : ANTsImage or tuple
        The output grid: an ``ANTsImage``, or a tuple ``(shape, spacing, origin, direction)``.
    out_path : str or os.PathLike
        The output file. ``.nii.gz`` is compressed with gzip on several threads; ``.nii`` is
        written uncompressed. The folder is created when it is missing, after the transforms have
        been read and the field computed.
    compression_level : int, optional
        Gzip level from 0 to 9; by default 1. Level 0 writes a valid gzip file without
        compressing it, which suits displacement fields because they barely compress.
    invert : list of bool, optional
        One flag per transform, as for :func:`apply_transforms`.

    Returns
    -------
    pathlib.Path
        The output path.

    Raises
    ------
    FileNotFoundError
        A transform file does not exist.
    ValueError
        ``compression_level`` is above 9, the transform list is empty, or ``invert`` is invalid.
    RegistrationError
        A transform file cannot be read, or the output cannot be written.
    """
    paths = _transform_paths(transforms)
    flags = _invert_flags(invert, len(paths))
    out = Path(out_path)
    _compose_field(
        paths,
        _grid_args(reference),
        str(out),
        compression_level=compression_level,
        invert=flags,
    )
    return out


def apply_transforms(
    moving: Any,
    reference: Any,
    transforms: str | PathLike[str] | list[str | PathLike[str]],
    *,
    interpolation: str = "linear",
    default_value: float = 0.0,
    invert: Sequence[bool] | None = None,
) -> Any:
    """Resample an image through a list of transforms, like ``antsApplyTransforms``.

    Runs in parallel on the CPU and needs no GPU.

    Parameters
    ----------
    moving : ANTsImage or tuple
        The image to resample.
    reference : ANTsImage or tuple
        The output grid: an ``ANTsImage``, or a tuple ``(shape, spacing, origin, direction)``.
    transforms : str, os.PathLike or list
        One transform file or a list of them, with the same meaning as the ``transformlist`` of
        ``ants.apply_transforms``. To warp a moving image onto the fixed image of a
        :func:`register_syn` run, pass its ``Composite.h5``.
    interpolation : str, default "linear"
        One of :data:`INTERPOLATORS`: ``"linear"``, ``"nearestNeighbor"`` (for label images) or
        ``"lanczosWindowedSinc"``.
    default_value : float, default 0.0
        Value of output voxels that map outside ``moving``.
    invert : list of bool, optional
        ANTsPy's ``whichtoinvert``: one ``bool`` per transform, ``True`` to use that transform's
        inverse in its place, as ``antsApplyTransforms -t [file,1]`` does. Only linear transforms
        (``.mat``, ``.txt``, composites of affines) can be inverted. ``None`` inverts nothing;
        unlike ANTsPy there is no automatic default.

    Returns
    -------
    ANTsImage or numpy.ndarray
        An ``ANTsImage`` like ``reference`` when ``reference`` is one, otherwise a ``float32``
        array of the reference's shape.

    Raises
    ------
    FileNotFoundError
        A transform file does not exist.
    ValueError
        ``interpolation`` is not supported, the transform list is empty, or ``invert`` does not
        hold one ``bool`` per transform or inverts a displacement field.
    RegistrationError
        A transform file cannot be read.
    """
    size, spacing, origin, direction = _grid_args(reference)
    paths = _transform_paths(transforms)
    flat = _apply_transforms(
        _image_args(moving),
        (size, spacing, origin, direction),
        paths,
        interpolation=interpolation,
        default_value=float(default_value),
        invert=_invert_flags(invert, len(paths)),
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
    """Rigid and affine fits of one image pair on the GPU.

    Made by :func:`linear_session`. The two images are prepared once (converted, and winsorized
    when ``winsorize`` is given), and their smoothed copies are kept between fits. Several fits of
    the same pair, such as a search from several starting positions or a rigid fit followed by an
    affine one, then pay for this preparation only once.

    Use the session in a ``with`` block, or call :meth:`close`, to free the GPU and the images.
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
        """Name of the GPU the session uses, with its graphics API and type."""
        return self._core.adapter

    @property
    def com_initial(self) -> tuple[tuple[float, ...], tuple[float, float, float]]:
        """The start that ``initial=None`` uses: ``(parameters, center)``.

        An affine that translates the centre of intensity mass of the fixed image onto that of the
        moving image: 12 parameters (identity matrix, then the translation in millimetres) and its
        centre in LPS millimetres.
        """
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
        """Run one rigid or affine fit and return its result.

        Parameters
        ----------
        stage : dict, optional
            The schedule: the fields of ``cohort_recipe()["rigid"]``. Fields left out keep their
            defaults.
        initial : tuple, optional
            The start, ``(parameters, center)`` of an affine that maps points of the fixed image to
            points of the moving image: 12 parameters (the 3 x 3 matrix row by row, then the
            translation in millimetres) and 3 centre coordinates in LPS millimetres. By default the
            centres of intensity mass (:attr:`com_initial`), as ``antsRegistration
            --initial-moving-transform [fixed,moving,1]``. The fit starts from the identity centred
            there and is combined with ``initial``.
        use_mask : bool, default True
            Sample the metric only inside the mask: ``mask`` if given, otherwise the session's
            ``fixed_mask``, if any.
        out_prefix : str or os.PathLike, optional
            Write the result as ``<out_prefix>0GenericAffine.mat``. A string ending in a path
            separator names a folder; the folder is created when it is missing.
        cancel : CancelToken, optional
            A token that another thread can use to stop the fit.
        transform : {"rigid", "affine"}, default "rigid"
            Rotation and translation, or also scaling and shearing.
        mask : ANTsImage or tuple, optional
            A fixed-image mask for this fit only, on any grid; its non-zero voxels are inside.

        Returns
        -------
        dict
            ``parameters`` and ``center`` (start and fit combined, what ``0GenericAffine.mat``
            holds), ``initial_parameters`` and ``initial_center``, ``metric`` (the final value
            of the negative mutual information, lower is better; ``None`` when the last level ran no
            iteration), ``iterations`` per level, ``total_s``, ``transform``, ``stage`` (the full
            schedule), ``adapter``, and ``warnings``: one message per level that was skipped because
            its samples left the metric undefined (each is also emitted as a ``UserWarning``).

        Raises
        ------
        ValueError
            An argument or the stage is invalid, a ``mask`` is given with ``use_mask=False``, or the
            session is closed or busy with another fit.
        RegistrationCancelled
            ``cancel`` was cancelled.
        RegistrationError
            The fit failed, for example because the GPU ran out of memory.
        """
        result = self._core.register(
            stage=None if stage is None else dict(stage),
            initial=_initial(initial),
            use_mask=use_mask,
            out_prefix=None if out_prefix is None else _prepared_prefix(out_prefix),
            cancel=cancel,
            transform=transform,
            mask=None if mask is None else _image_args(mask),
        )
        return _warn_skipped(result)

    def close(self) -> None:
        """Free the GPU and the images. Using the session afterwards raises ``ValueError``."""
        self._core.close()

    def __enter__(self) -> LinearSession:
        return self

    def __exit__(self, *args: object) -> bool:
        self.close()
        return False


#: Deprecated alias of :class:`LinearSession`; removed in fastwarp 0.3.0.
RigidSession = LinearSession


def linear_session(
    fixed: Any,
    moving: Any,
    *,
    fixed_mask: Any | None = None,
    winsorize: tuple[float, float] | None = None,
    adapter: str | None = None,
) -> LinearSession:
    """Prepare an image pair for rigid and affine fits on the GPU.

    Opens the GPU and prepares the images now; each :meth:`LinearSession.register` call then runs
    one fit. Unlike :func:`register_syn`, the session registers the images on their own grids,
    without resampling them, and changes their intensities only when ``winsorize`` is given.

    Parameters
    ----------
    fixed : ANTsImage or tuple
        The image to register onto.
    moving : ANTsImage or tuple
        The image to register.
    fixed_mask : ANTsImage or tuple, optional
        A mask on any grid whose non-zero voxels mark where the metric samples the fixed image, in
        every fit with ``use_mask=True`` that brings no mask of its own.
    winsorize : tuple of float, optional
        ``(lower, upper)`` quantiles, as ``antsRegistration --winsorize-image-intensities``:
        intensities below the lower or above the upper quantile are clipped to it, and the result
        is rescaled to [0, 1]. ``(0.0, 1.0)`` only rescales, which is what ``antsRegistration``
        does without that option. By default the intensities are used as they are; the result can
        then differ slightly from a run with ``(0.0, 1.0)``, because the GPU computes in 32-bit
        floating point.
    adapter : str, optional
        Use the GPU whose name contains this text.

    Returns
    -------
    LinearSession
        The prepared session; use it in a ``with`` block.

    Raises
    ------
    ValueError
        ``winsorize`` is not ``(lower, upper)`` with ``0 <= lower < upper <= 1``, or an image is
        not 3-D.
    GpuUnavailableError
        No usable GPU.
    RegistrationError
        The images cannot be prepared, for example because they have NaN voxels.

    See Also
    --------
    register_syn : Full normalization, including the rigid and affine stages.
    """
    return LinearSession(fixed, moving, fixed_mask=fixed_mask, winsorize=winsorize, adapter=adapter)


#: Deprecated alias of :func:`linear_session`; removed in fastwarp 0.3.0.
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
    """One rigid fit of ``moving`` onto ``fixed``.

    Deprecated: use ``linear_session(fixed, moving).register()`` in a ``with`` block instead.
    Looking it up as ``fastwarp.register_rigid`` emits a ``DeprecationWarning``; it will be removed
    in FastWarp 0.3.0.
    """
    # Everything a caller can get wrong is refused before the GPU is opened.
    stage_dict = _check_linear_stage(None if stage is None else dict(stage), "rigid")
    start = _initial(initial)
    prefix = None if out_prefix is None else _prepared_prefix(out_prefix)
    if prefix is not None and not _prefix_folder(prefix).is_dir():
        raise ValueError(f"output folder {_prefix_folder(prefix)} does not exist")
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
