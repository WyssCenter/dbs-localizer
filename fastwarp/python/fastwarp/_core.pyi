# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

from typing import Any

import numpy as np
import numpy.typing as npt

ENGINE_SOURCE: str
"""The revision of the earlier standalone copy of the engine this code was taken from.

It names a revision of that earlier copy (developed inside Wyss DBS Localizer), not a revision of
the FastWarp repository. Later changes are listed in the changelog.
"""
VERSION: str
"""The version of the compiled module (from Cargo.toml)."""

class GpuUnavailableError(RuntimeError):
    """No usable GPU.

    Raised before any work when no hardware GPU opens for the graphics APIs tried, when the
    ``adapter`` filter matches none, when ``FASTWARP_GPU_BACKEND`` has an unknown value, or when
    the device or its self-test fails. Software adapters count only with
    ``FASTWARP_GPU_ALLOW_SOFTWARE=1``. A caller can catch it and run ANTs instead.
    """

class GpuCapacityError(RuntimeError):
    """The registration does not fit the GPU.

    Raised by ``register_syn`` before any work when the registration grid needs more GPU memory
    than the budget, or a larger buffer than the device allows. Use a larger ``iso_mm``, another
    GPU, or ANTs.
    """

class RegistrationError(RuntimeError):
    """The engine failed while it worked.

    For example on an image with NaN voxels, when the GPU runs out of memory or is reset, when a
    transform file cannot be read, or when an output file cannot be written. An internal error
    of the engine is raised as this exception as well.
    """

class RegistrationCancelled(Exception):
    """The run was stopped through its ``CancelToken``.

    It derives from ``Exception``, not ``RuntimeError``, so that a handler for failures does not
    catch a cancellation by accident. No files are written.
    """

class CancelToken:
    """A flag that stops a running registration or fit from another thread.

    Pass it as ``cancel=`` to ``register_syn`` or ``LinearSession.register``. After
    ``cancel()`` the run stops at its next iteration and raises ``RegistrationCancelled``. A token
    stays cancelled; use a new one for the next run.
    """

    def __init__(self) -> None: ...
    def cancel(self) -> None:
        """Ask the run that uses this token to stop at its next iteration."""

    @property
    def cancelled(self) -> bool:
        """Whether ``cancel()`` has been called."""

_Image = tuple[
    npt.NDArray[np.float32] | npt.NDArray[np.float64],
    tuple[int, int, int],
    tuple[float, float, float],
    tuple[float, float, float],
    tuple[tuple[float, float, float], tuple[float, float, float], tuple[float, float, float]],
]

def register_syn(
    fixed: list[_Image],
    moving: list[_Image],
    out_prefix: str,
    iso_mm: float = 1.0,
    write_warped: bool = False,
    adapter: str | None = None,
    cancel: CancelToken | None = None,
    recipe: dict[str, Any] | None = None,
    outputs: str = "composite",
) -> dict[str, Any]: ...
def gpu_info(adapter: str | None = None) -> dict[str, Any]:
    """Open the GPU that FastWarp would use and describe it.

    Parameters
    ----------
    adapter : str, optional
        Describe the GPU whose name contains this text (case-insensitive). By default the
        ``FASTWARP_GPU_ADAPTER`` environment variable applies, if set.

    Returns
    -------
    dict
        ``name`` (the adapter's name), ``backend`` (the graphics API, for example
        ``"Vulkan"``, ``"Metal"`` or ``"Dx12"``), ``device_type`` (for example
        ``"DiscreteGpu"`` or ``"IntegratedGpu"``), ``driver`` (the driver's name as the API
        reports it, not a version) and ``memory_budget_bytes`` (the memory budget of the
        capacity check, in bytes).

    Raises
    ------
    GpuUnavailableError
        No usable GPU.
    """

def fits_gpu(
    dims: tuple[int, int, int],
    channels: int,
    adapter: str | None = None,
    moving_dims: tuple[int, int, int] | None = None,
    recipe: dict[str, Any] | None = None,
) -> bool: ...
def compose_field(
    transforms: list[str],
    reference: tuple[
        tuple[int, int, int],
        tuple[float, float, float],
        tuple[float, float, float],
        tuple[tuple[float, float, float], tuple[float, float, float], tuple[float, float, float]],
    ],
    out_path: str,
    compression_level: int | None = None,
    invert: list[bool] | None = None,
) -> None: ...
def apply_transforms(
    moving: _Image,
    reference: tuple[
        tuple[int, int, int],
        tuple[float, float, float],
        tuple[float, float, float],
        tuple[tuple[float, float, float], tuple[float, float, float], tuple[float, float, float]],
    ],
    transforms: list[str],
    interpolation: str = "linear",
    default_value: float = 0.0,
    invert: list[bool] | None = None,
) -> npt.NDArray[np.float32]: ...
def cohort_recipe(channels: int = 1) -> dict[str, dict[str, Any]]: ...

class LinearSession:
    def __init__(
        self,
        fixed: _Image,
        moving: _Image,
        fixed_mask: _Image | None = None,
        winsorize: tuple[float, float] | None = None,
        adapter: str | None = None,
    ) -> None: ...
    @property
    def adapter(self) -> str: ...
    @property
    def com_initial(self) -> tuple[tuple[float, ...], tuple[float, float, float]]: ...
    def register(
        self,
        stage: dict[str, Any] | None = None,
        initial: tuple[tuple[float, ...], tuple[float, ...]] | None = None,
        use_mask: bool = True,
        out_prefix: str | None = None,
        cancel: CancelToken | None = None,
        transform: str = "rigid",
        mask: _Image | None = None,
    ) -> dict[str, Any]: ...
    def close(self) -> None: ...
    def __enter__(self) -> LinearSession: ...
    def __exit__(self, *args: object) -> bool: ...

def check_linear_stage(
    stage: dict[str, Any] | None = None, transform: str = "rigid"
) -> dict[str, Any]: ...
def cli_main(argv: list[str]) -> int: ...

class UnsupportedArgument(ValueError):
    """An argument of ``fastwarp.ants`` that FastWarp does not support.

    Raised before any work for an argument that would change the result. Re-exported as
    ``fastwarp.ants.UnsupportedArgument``.
    """

_Grid = tuple[
    tuple[int, int, int],
    tuple[float, float, float],
    tuple[float, float, float],
    tuple[tuple[float, float, float], tuple[float, float, float], tuple[float, float, float]],
]

def ants_parse_registration(argv: list[str]) -> dict[str, Any]: ...
def ants_parse_apply(argv: list[str]) -> dict[str, Any]: ...
def ants_resolve_seed(
    random_seed: int | None = None, ants_random_seed: str | None = None
) -> int: ...
def ants_registration(
    argv: list[str],
    images: list[tuple[str, _Image]] = ...,
    keep: list[str] = ...,
    iso_mm: float | None = None,
    adapter: str | None = None,
    cancel: CancelToken | None = None,
) -> dict[str, Any]: ...
def ants_apply_transforms(
    argv: list[str],
    images: list[tuple[str, _Image]] = ...,
    grids: list[tuple[str, _Grid]] = ...,
    keep: list[str] = ...,
) -> dict[str, Any]: ...
