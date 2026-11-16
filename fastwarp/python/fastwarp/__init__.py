# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""FastWarp: GPU SyN registration for Python, developed by Richard Köhler and Jan Philipp Payonk
as the registration engine of Wyss DBS Localizer.

The engine is a wgpu port of ANTs/ITK's SyN with the CC and Mattes MI metrics (Vulkan, Metal,
DX12). :func:`register_syn` runs the dbs-localizer cohort recipe (centre of mass, Rigid,
Affine, SyN on an isotropic grid), or any start, linear stages and optional SyN stage a
stage-list recipe names, and writes ANTs-compatible transforms.
"""

from __future__ import annotations

import warnings
from importlib.metadata import PackageNotFoundError, version
from typing import TYPE_CHECKING

from . import _api
from ._api import (
    CITATION,
    INTERPOLATORS,
    OUTPUT_LAYOUTS,
    LinearSession,
    apply_transforms,
    cohort_recipe,
    compose_field,
    fits_gpu,
    gpu_available,
    iso_grid_dims,
    linear_session,
    register_syn,
)
from ._core import (
    ENGINE_SOURCE,
    CancelToken,
    GpuCapacityError,
    GpuUnavailableError,
    RegistrationCancelled,
    RegistrationError,
    gpu_info,
)

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
    "OUTPUT_LAYOUTS",
    "GpuUnavailableError",
    "RegistrationCancelled",
    "RegistrationError",
    "LinearSession",
    "__version__",
    "apply_transforms",
    "cohort_recipe",
    "compose_field",
    "gpu_available",
    "gpu_info",
    "iso_grid_dims",
    "linear_session",
    "register_syn",
    "fits_gpu",
]

for _obj in (
    apply_transforms,
    cohort_recipe,
    compose_field,
    fits_gpu,
    gpu_available,
    gpu_info,
    iso_grid_dims,
    linear_session,
    register_syn,
    _api.register_rigid,
):
    _obj.__module__ = "fastwarp"
del _obj


def __dir__() -> list[str]:
    return sorted(__all__)


_DEPRECATED = {
    "RigidSession": ("LinearSession", _api.RigidSession),
    "rigid_session": ("linear_session", _api.rigid_session),
    "register_rigid": ("linear_session(...).register()", _api.register_rigid),
}


if TYPE_CHECKING:
    from ._api import RigidSession as RigidSession
    from ._api import register_rigid as register_rigid
    from ._api import rigid_session as rigid_session
else:

    def __getattr__(name: str):
        try:
            replacement, obj = _DEPRECATED[name]
        except KeyError:
            raise AttributeError(f"module 'fastwarp' has no attribute {name!r}") from None
        warnings.warn(
            f"fastwarp.{name} is deprecated and will be removed in fastwarp 0.3.0; "
            f"use fastwarp.{replacement}",
            DeprecationWarning,
            stacklevel=2,
        )
        return obj
