# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

from typing import Any

import numpy as np
import numpy.typing as npt

ENGINE_SOURCE: str
"""The revision of the engine this build was taken from."""
VERSION: str
"""The version of the compiled module (from Cargo.toml)."""

class GpuUnavailableError(RuntimeError):
    """No usable hardware GPU adapter.

    Software adapters are refused unless FASTWARP_GPU_ALLOW_SOFTWARE=1.
    """

class GpuCapacityError(RuntimeError):
    """The registration grid does not fit the GPU's memory budget or binding limit."""

class RegistrationError(RuntimeError):
    """The registration failed."""

class RegistrationCancelled(Exception):
    """The registration was cancelled through its CancelToken."""

class CancelToken:
    """A flag another thread sets to stop a running registration at its next iteration."""

    def __init__(self) -> None: ...
    def cancel(self) -> None:
        """Asks the registration using this token to stop; it raises RegistrationCancelled."""

    @property
    def cancelled(self) -> bool:
        """Whether cancel() has been called."""

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
    recipe: dict[str, dict[str, Any]] | None = None,
) -> dict[str, Any]: ...
def gpu_info(adapter: str | None = None) -> dict[str, Any]:
    """Opens the GPU the engine would use and describes it.

    Raises GpuUnavailableError when there is none.
    """

def fits_gpu(dims: tuple[int, int, int], channels: int, adapter: str | None = None) -> bool: ...
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
