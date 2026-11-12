# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""compose_field / apply_transforms against ANTs (antsApplyTransforms through ANTsPy).

The affine tests need no GPU, so they also run on the wheel jobs; the composite tests use a
registration and need one.
"""

from __future__ import annotations

import gzip
from pathlib import Path

import fastwarp as g
import numpy as np
import pytest

ants = pytest.importorskip("ants")

needs_gpu = pytest.mark.skipif(not g.gpu_available(), reason="no usable GPU adapter")


def _rot(deg: float) -> np.ndarray:
    a = np.deg2rad(deg)
    return np.array([[np.cos(a), -np.sin(a), 0.0], [np.sin(a), np.cos(a), 0.0], [0.0, 0.0, 1.0]])


def _image(shape, spacing, origin, direction, seed=0):
    """A sharp-edged blob with texture, so every interpolator's differences show."""
    idx = np.indices(shape, dtype=np.float64)
    c = [(n - 1) / 2 for n in shape]
    r = sum(((idx[a] - c[a]) / (0.35 * shape[a])) ** 2 for a in range(3))
    vol = np.where(r <= 1.0, 100.0, 10.0) + 5.0 * np.sin(idx[0] * 0.7) * np.cos(idx[1] * 0.4)
    vol += np.random.default_rng(seed).standard_normal(shape)
    return ants.from_numpy(
        vol.astype("float32"), origin=origin, spacing=spacing, direction=direction
    )


@pytest.fixture(scope="module")
def scene(tmp_path_factory):
    """An oblique moving image, a reference on another grid, and an ANTs affine between them."""
    d = tmp_path_factory.mktemp("post")
    moving = _image((40, 44, 36), (1.1, 0.9, 1.3), (-20.0, 15.0, -10.0), _rot(12))
    reference = _image((48, 40, 30), (0.8, 1.0, 1.5), (-22.0, 12.0, -14.0), _rot(-7), seed=1)
    tx = ants.create_ants_transform(
        transform_type="AffineTransform",
        dimension=3,
        matrix=np.array([[1.05, 0.04, 0.0], [-0.03, 0.97, 0.02], [0.01, 0.0, 1.02]]),
        translation=(1.3, -2.1, 0.7),
        center=(2.0, 30.0, 10.0),
    )
    mat = d / "affine.mat"
    ants.write_transform(tx, str(mat))
    return d, moving, reference, mat


def _field(path) -> np.ndarray:
    return ants.image_read(str(path)).numpy()


def test_compose_field_equals_ants(scene):
    d, moving, reference, mat = scene
    mine = g.compose_field(mat, reference, d / "mine.nii.gz")
    theirs = ants.apply_transforms(
        fixed=reference, moving=moving, transformlist=[str(mat)], compose=str(d / "ants")
    )
    a, b = _field(mine), _field(theirs)
    assert a.shape == b.shape == (*reference.shape, 3)
    assert np.abs(a - b).max() < 1e-4
    ref = ants.image_read(str(mine))
    assert np.allclose(ref.origin, reference.origin) and np.allclose(
        ref.direction, reference.direction
    )


@pytest.mark.parametrize("interp", g.INTERPOLATORS)
def test_apply_transforms_equals_ants(scene, interp):
    _, moving, reference, mat = scene
    mine = g.apply_transforms(moving, reference, mat, interpolation=interp)
    theirs = ants.apply_transforms(
        fixed=reference, moving=moving, transformlist=[str(mat)], interpolator=interp
    )
    assert mine.shape == reference.shape
    assert np.allclose(mine.origin, reference.origin)
    diff = np.abs(mine.numpy() - theirs.numpy())
    assert diff.max() < 1e-3, f"{interp}: max |diff| {diff.max()}"
    # Part of the reference lies outside the moving image: both give the default value there.
    assert (theirs.numpy() == 0).sum() > 0
    assert ((mine.numpy() == 0) == (theirs.numpy() == 0)).mean() > 0.999


def test_transform_order_is_ants_order(scene):
    """A list means what it means to ants.apply_transforms, in both orders."""
    d, moving, reference, mat = scene
    shift = ants.create_ants_transform(
        transform_type="AffineTransform", dimension=3, translation=(3.0, 0.0, -1.0)
    )
    ants.write_transform(shift, str(d / "shift.mat"))
    for order in ([str(mat), str(d / "shift.mat")], [str(d / "shift.mat"), str(mat)]):
        mine = g.apply_transforms(moving, reference, order, interpolation="linear")
        theirs = ants.apply_transforms(fixed=reference, moving=moving, transformlist=order)
        assert np.abs(mine.numpy() - theirs.numpy()).max() < 1e-3
        field = g.compose_field(order, reference, d / "both.nii.gz")
        ants_field = ants.apply_transforms(
            fixed=reference, moving=moving, transformlist=order, compose=str(d / "both_ants")
        )
        assert np.abs(_field(field) - _field(ants_field)).max() < 1e-4


def test_stored_gzip_is_gzip_and_holds_the_same_field(scene):
    d, _, reference, mat = scene
    a = g.compose_field(mat, reference, d / "l0.nii.gz", compression_level=0)
    b = g.compose_field(mat, reference, d / "l6.nii.gz", compression_level=6)
    with gzip.open(a) as f:
        assert (
            len(f.read())
            == 352 + reference.shape[0] * reference.shape[1] * reference.shape[2] * 3 * 4
        )
    assert np.array_equal(_field(a), _field(b))
    assert a.stat().st_size > b.stat().st_size


def test_a_tuple_reference_gives_an_array(scene):
    _, moving, reference, mat = scene
    ref = (reference.shape, reference.spacing, reference.origin, reference.direction)
    out = g.apply_transforms(moving, ref, [mat])
    assert isinstance(out, np.ndarray) and out.dtype == np.float32 and out.shape == reference.shape
    assert np.array_equal(out, g.apply_transforms(moving, reference, mat).numpy())


def test_bad_arguments(scene, tmp_path):
    _, moving, reference, mat = scene
    with pytest.raises(FileNotFoundError):
        g.apply_transforms(moving, reference, tmp_path / "missing.h5")
    with pytest.raises(ValueError, match="interpolation"):
        g.apply_transforms(moving, reference, mat, interpolation="bSpline")
    with pytest.raises(ValueError, match="compression_level"):
        g.compose_field(mat, reference, tmp_path / "x.nii.gz", compression_level=10)
    with pytest.raises(ValueError, match="3-D"):
        g.compose_field(mat, ((4, 4), (1, 1), (0, 0), np.eye(2)), tmp_path / "x.nii.gz")
    with pytest.raises(ValueError, match="at least one"):
        g.apply_transforms(moving, reference, [])
    bad = tmp_path / "bad.h5"
    bad.write_bytes(b"not hdf5")
    with pytest.raises(g.RegistrationError, match="bad.h5"):
        g.compose_field(bad, reference, tmp_path / "x.nii.gz")


@needs_gpu
def test_registration_composites_equal_ants(tmp_path):
    """The real case: the engine's SyN composites, on grids unlike the registration grid."""
    sp, shape = 2.0, (56, 64, 48)
    idx = np.indices(shape, dtype=np.float64) * sp

    def head(shift):
        def ell(c, r):
            return sum(((idx[a] - c[a] - shift[a]) / r[a]) ** 2 for a in range(3)) <= 1.0

        v = np.zeros(shape)
        v[ell((56, 64, 48), (44, 52, 38))] = 0.3
        v[ell((56, 64, 48), (38, 46, 32))] = 0.7
        v[ell((48, 70, 50), (6, 10, 5))] = 0.2
        v[ell((56, 40, 40), (8, 6, 6))] = 1.0
        v += 0.01 * np.random.default_rng(0).standard_normal(shape)
        return ants.from_numpy(v.astype("float32"), spacing=(sp,) * 3)

    fixed, moving = head((0, 0, 0)), head((4, -3, 2))
    g.register_syn(fixed, moving, tmp_path / "r_", iso_mm=sp)
    fwd, inv = str(tmp_path / "r_Composite.h5"), str(tmp_path / "r_InverseComposite.h5")
    fine = ants.resample_image(fixed, (1.5, 1.5, 1.5), use_voxels=False, interp_type=0)
    for tx, ref, mov, name in ((fwd, fine, moving, "f"), (inv, moving, fixed, "i")):
        mine = g.compose_field(tx, ref, tmp_path / f"{name}.nii.gz", compression_level=0)
        theirs = ants.apply_transforms(
            fixed=ref, moving=mov, transformlist=[tx], compose=str(tmp_path / f"{name}_ants")
        )
        assert np.abs(_field(mine) - _field(theirs)).max() < 1e-4
    mine = g.apply_transforms(moving, fine, fwd, interpolation="lanczosWindowedSinc")
    theirs = ants.apply_transforms(
        fixed=fine, moving=moving, transformlist=[fwd], interpolator="lanczosWindowedSinc"
    )
    assert np.abs(mine.numpy() - theirs.numpy()).max() < 1e-4
    assert Path(tmp_path / "f.nii.gz").stat().st_size > 0
