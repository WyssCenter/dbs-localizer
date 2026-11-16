# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""``fastwarp.ants``, the ANTsPy-compatible layer, against ANTsPy 0.6.3.

The argument lists (T1) are compared with the ones ANTsPy builds, with ANTsPy's library call
replaced so that nothing registers; they, the refusals, the warnings and the fallback need no
GPU. ``apply_transforms`` (T5) runs on the CPU. The return dict (T4), determinism (T9) and the
raw argument list against ``fastwarp ants-registration`` register on the GPU and skip without
one.
"""

from __future__ import annotations

import gzip
import subprocess
import sys
import warnings
from pathlib import Path

import fastwarp as g
import fastwarp.ants as fa
import numpy as np
import pytest

needs_gpu = pytest.mark.skipif(not g.gpu_available(), reason="no usable GPU adapter")

NAMES = {
    "fixed": "F",
    "moving": "M",
    "warpedmovout": "WM",
    "warpedfixout": "WF",
    "mask": "MASK",
    "extra_fixed": "F2",
    "extra_moving": "M2",
}


@pytest.fixture(scope="module")
def ants():
    return pytest.importorskip("ants")


@pytest.fixture(scope="module")
def R(ants):
    return sys.modules["ants.registration.registration"]


def _img(ants, shape=(8, 9, 7), seed=0, spacing=(1.0, 1.0, 1.0)):
    data = np.random.default_rng(seed).random(shape).astype("float32")
    return ants.from_numpy(data, spacing=spacing)


@pytest.fixture(scope="module")
def pair(ants):
    return _img(ants, seed=0), _img(ants, seed=1)


def test_importing_the_module_does_not_import_antspy():
    code = "import sys, fastwarp.ants; print('ants' in sys.modules)"
    out = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, check=True)
    assert out.stdout.strip() == "False"


def test_without_antspy_argument_lists_run_and_images_need_it():
    code = "\n".join(
        [
            "import sys; sys.modules['ants'] = None; import fastwarp.ants as fa",
            "assert fa.registration(['--version'], None) == 0",
            "try:",
            "    fa.registration(object(), object())",
            "except ImportError as e:",
            "    print('IMPORT', e)",
        ]
    )
    out = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, check=True)
    assert "antsRegistration compatibility mode" in out.stdout
    assert "IMPORT fastwarp.ants needs ANTsPy" in out.stdout
    assert "pip install antspyx" in out.stdout


def _antspy_argv(monkeypatch, R, fixed, moving, t, kw):
    labels = iter(["F", "M", "WF", "WM", "MASK"])
    by_id = {}
    for extra in kw.get("multivariate_extras") or []:
        by_id[id(extra[1])] = "F2"
        by_id[id(extra[2])] = "M2"
    captured = []

    def lib(name):
        def run(args):
            captured.append(list(args))
            return 0

        return run

    monkeypatch.setattr(R, "get_pointer_string", lambda img: by_id.get(id(img)) or next(labels))
    monkeypatch.setattr(R, "get_lib_fn", lib)
    R.registration(fixed, moving, type_of_transform=t, outprefix="P_", **kw)
    return captured[0]


def _fastwarp_argv(ants, fixed, moving, t, kw, names):
    a = {"fixed": fixed, "moving": moving, **fa._DEFAULTS, **kw, "type_of_transform": t}
    a["outprefix"] = "P_"
    a["random_seed"] = sys.modules["ants.config"]._random_seed
    tt, _ = fa._validate(ants, a)
    return fa._registration_argv(tt, a, names)


LINEAR = ["Rigid", "Affine", "AffineFast", "BOLDAffine", "QuickRigid", "DenseRigid", "BOLDRigid"]
MYSYN = [
    "SyN",
    "SyNRA",
    "SyNOnly",
    "SyNAggro",
    "SyNLessAggro",
    "ElasticSyN",
    "SyNBold",
    "SyNBoldAff",
]
CC4 = {"syn_metric": "CC", "syn_sampling": 4}

T1_CASES = [
    *[(t, {}) for t in LINEAR],
    (
        "Rigid",
        {"aff_iterations": (10, 5), "aff_shrink_factors": (2, 1), "aff_smoothing_sigmas": (1, 0)},
    ),
    ("Affine", {"aff_iterations": 50, "aff_shrink_factors": 2, "aff_smoothing_sigmas": 1}),
    ("Affine", {"smoothing_in_mm": True, "aff_random_sampling_rate": 0.3}),
    ("BOLDRigid", {"smoothing_in_mm": True}),
    ("QuickRigid", {"aff_metric": "MI"}),
    ("Rigid", {"mask": "MASK"}),
    (
        "Rigid",
        {
            "initial_transform": "Identity",
            "write_composite_transform": True,
            "singleprecision": False,
            "verbose": True,
        },
    ),
    ("Affine", {"initial_transform": "init.mat"}),
    ("Affine", {"initial_transform": ["[init.mat,1]"]}),
    ("Rigid", {"restrict_transformation": (1, 1, 1, 1, 1, 1)}),
    ("SyNCC", {}),
    ("SyNCC", {"mask_all_stages": True, "aff_random_sampling_rate": 0.5}),
    ("SyNabp", {}),
    *[(t, CC4) for t in MYSYN],
    (
        "SyN",
        {
            "syn_metric": "CC",
            "syn_sampling": 2,
            "grad_step": 0.1,
            "flow_sigma": 2.5,
            "total_sigma": 0.5,
            "reg_iterations": (30, 20, 10, 5),
        },
    ),
    ("SyNOnly", {"syn_metric": "cc", "syn_sampling": 3, "reg_iterations": (7,)}),
    ("SyNOnly", {"syn_metric": "CC", "syn_sampling": 2, "multivariate_extras": "EXTRAS"}),
    *[(f"antsRegistrationSyN[{s}]", {}) for s in ("r", "a", "s", "sr", "so")],
    ("antsRegistrationSyN", {}),
    ("antsRegistrationSyN[s,4]", {}),
    ("antsRegistrationSyNQuick[r]", {}),
    ("antsRegistrationSyNQuick[a]", {}),
    ("antsRegistrationSyN[s]", {"multivariate_extras": "EXTRAS"}),
    ("antsRegistrationSyN[a]", {"mask": "MASK"}),
]


def _resolve(ants, kw, fixed):
    out = dict(kw)
    if out.get("mask") == "MASK":
        out["mask"] = fixed
    if out.get("multivariate_extras") == "EXTRAS":
        out["multivariate_extras"] = [("CC", _img(ants, seed=2), _img(ants, seed=3), 0.5, 2)]
    return out


@pytest.mark.parametrize(
    ("t", "kw"), T1_CASES, ids=[f"{t}-{i}" for i, (t, _) in enumerate(T1_CASES)]
)
def test_t1_argument_list_is_antspys(ants, R, pair, monkeypatch, t, kw):
    fixed, moving = pair
    kw = _resolve(ants, kw, fixed)
    theirs = _antspy_argv(monkeypatch, R, fixed, moving, t, kw)
    mine = _fastwarp_argv(ants, fixed, moving, t, kw, NAMES)
    assert mine == theirs
    real = _fastwarp_argv(ants, fixed, moving, t, kw, fa._registration_names())
    parsed = g._core.ants_parse_registration(real)
    assert parsed["kind"] == "run"
    assert len(parsed["plan"]["recipe"]["stages"]) == real.count("--transform")


def test_t1_random_seed_follows_ants_config(ants, R, pair, monkeypatch):
    fixed, moving = pair
    monkeypatch.setattr(sys.modules["ants.config"], "_random_seed", 7)
    theirs = _antspy_argv(monkeypatch, R, fixed, moving, "Rigid", {})
    mine = _fastwarp_argv(ants, fixed, moving, "Rigid", {}, NAMES)
    assert mine == theirs
    assert mine[mine.index("--random-seed") + 1] == "7"


def _no_engine(monkeypatch):
    def fail(*args, **kwargs):
        raise AssertionError("the engine was called")

    monkeypatch.setattr(fa._core, "ants_registration", fail)
    monkeypatch.setattr(fa._core, "ants_apply_transforms", fail)


PREFIX = "fastwarp.ants.registration: "
SYN_MATTES = (
    PREFIX + "syn_metric='mattes' is not supported: FastWarp's SyN stage has no Mattes mutual "
    "information metric (type_of_transform='{t}' uses syn_metric for its SyN stage); use "
    "type_of_transform='SyNCC', or syn_metric='CC' with syn_sampling (the CC radius) from 1 to 8."
)
QUICK = (
    PREFIX + "type_of_transform='antsRegistrationSyNQuick[{s}]' is not supported: its SyN stage "
    "uses Mattes mutual information, which FastWarp's SyN stage does not have (it uses CC); use "
    "'antsRegistrationSyN[{s}]' or 'SyNCC'."
)
TV = "FastWarp has no time-varying velocity field transform."


def _type_refusal(t, why):
    return f"{PREFIX}type_of_transform={t!r} is not supported: {why}"


REFUSALS = [
    (
        {"type_of_transform": "Translation"},
        _type_refusal("Translation", "FastWarp has no Translation transform."),
    ),
    (
        {"type_of_transform": "Similarity"},
        _type_refusal("Similarity", "FastWarp has no Similarity transform."),
    ),
    (
        {"type_of_transform": "TRSAA"},
        _type_refusal("TRSAA", "FastWarp has no Translation or Similarity transform."),
    ),
    (
        {"type_of_transform": "Elastic"},
        _type_refusal("Elastic", "FastWarp has no GaussianDisplacementField transform."),
    ),
    ({"type_of_transform": "TV[3]"}, _type_refusal("TV[3]", TV)),
    ({"type_of_transform": "TVMSQ"}, _type_refusal("TVMSQ", TV)),
    ({"type_of_transform": "TVMSQC"}, _type_refusal("TVMSQC", TV)),
    *[({"type_of_transform": t}, SYN_MATTES.format(t=t)) for t in MYSYN],
    ({}, SYN_MATTES.format(t="SyN")),
    (
        {"type_of_transform": "SyNRA", "syn_metric": "meansquares"},
        PREFIX + "syn_metric='meansquares' is not supported: FastWarp's SyN stage uses CC only.",
    ),
    (
        {"type_of_transform": "SyN", "syn_metric": "CC"},
        PREFIX + "syn_sampling=32 is not supported: with syn_metric='CC' it is the CC radius, and "
        "the GPU CC kernels support radius 1 to 8.",
    ),
    (
        {"type_of_transform": "SyNOnly", **CC4, "reg_iterations": tuple(range(11))},
        PREFIX + "reg_iterations=(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10) is not supported: ANTsPy "
        "derives the smoothing sigmas of 1 to 10 levels only.",
    ),
    (
        {"type_of_transform": "SyNOnly", **CC4, "reg_iterations": ()},
        PREFIX + "reg_iterations=() is not supported: ANTsPy derives the smoothing sigmas of 1 to "
        "10 levels only.",
    ),
    (
        {"type_of_transform": "antsRegistrationSyN[t]"},
        _type_refusal("antsRegistrationSyN[t]", "FastWarp has no Translation transform."),
    ),
    *[
        (
            {"type_of_transform": f"antsRegistrationSyN[{s}]"},
            _type_refusal(f"antsRegistrationSyN[{s}]", "FastWarp has no BSplineSyN transform."),
        )
        for s in ("b", "br", "bo")
    ],
    *[
        ({"type_of_transform": f"antsRegistrationSyNQuick[{s}]"}, QUICK.format(s=s))
        for s in ("s", "sr", "so")
    ],
    *[
        (
            {"type_of_transform": t},
            _type_refusal(t, "FastWarp has no GC (global correlation) metric."),
        )
        for t in ("antsRegistrationSyNRepro[s]", "antsRegistrationSyNQuickRepro[a]")
    ],
    (
        {"type_of_transform": "Rigid", "aff_metric": "GC"},
        PREFIX + "aff_metric='GC' is not supported: FastWarp's linear stages use Mattes mutual "
        "information.",
    ),
    (
        {"type_of_transform": "SyNCC", "aff_metric": "meansquares"},
        PREFIX + "aff_metric='meansquares' is not supported: FastWarp's linear stages use Mattes "
        "mutual information.",
    ),
    (
        {"type_of_transform": "Affine", "aff_sampling": 64},
        PREFIX + "aff_sampling=64 is not supported: the GPU Mattes kernels use 32 histogram bins.",
    ),
    (
        {"type_of_transform": "SyNCC", "mask": "MASK"},
        PREFIX + "mask={mask!r} is not supported: with type_of_transform='SyNCC' ANTsPy applies "
        "the mask to the SyN stage, and FastWarp's SyN stage has no mask.",
    ),
    (
        {"type_of_transform": "antsRegistrationSyN[s]", "mask": "MASK"},
        PREFIX + "mask={mask!r} is not supported: with type_of_transform='antsRegistrationSyN[s]' "
        "ANTsPy applies the mask to the SyN stage, and FastWarp's SyN stage has no mask.",
    ),
    (
        {"type_of_transform": "Rigid", "moving_mask": "MASK"},
        PREFIX + "moving_mask={mask!r} is not supported: FastWarp has no moving-image mask.",
    ),
    (
        {"type_of_transform": "Rigid", "restrict_transformation": (1, 0, 1, 1, 1, 1)},
        PREFIX + "restrict_transformation=(1, 0, 1, 1, 1, 1) is not supported: FastWarp cannot "
        "restrict transform components.",
    ),
    (
        {"type_of_transform": "SyNCC", "use_legacy_histogram_matching": True},
        PREFIX + "use_legacy_histogram_matching=True is not supported: FastWarp has no histogram "
        "matching.",
    ),
    (
        {"type_of_transform": "Rigid", "initial_transform": ["a.mat", "b.mat"]},
        PREFIX + "initial_transform=['a.mat', 'b.mat'] is not supported: FastWarp starts the "
        "registration from one affine transform file.",
    ),
    (
        {"type_of_transform": "SyNCC", "initial_transform": "warp.nii.gz"},
        PREFIX + "initial_transform='warp.nii.gz' is not supported: a displacement field "
        "cannot start the registration; FastWarp starts from one affine transform file.",
    ),
    (
        {"type_of_transform": "Rigid", "initial_transform": "[f.nii.gz,m.nii.gz,1]"},
        PREFIX + "initial_transform='[f.nii.gz,m.nii.gz,1]' is not supported: the images are "
        "ANTsImages here, not files; leave initial_transform=None for the centres of intensity "
        "mass.",
    ),
    (
        {"type_of_transform": "antsRegistrationSyN[s]", "multivariate_extras": "OFFGRID"},
        PREFIX + "multivariate_extras[0][1]={offgrid!r} is not supported: it is not on fixed's "
        "grid, and FastWarp's SyN channels share one fixed grid; resample it onto that grid "
        "first.",
    ),
    (
        {"type_of_transform": "SyNOnly", **CC4, "multivariate_extras": "TWO"},
        PREFIX + "multivariate_extras={extras!r} is not supported: FastWarp's SyN stage takes one "
        "extra CC metric at most (two channels).",
    ),
    (
        {"type_of_transform": "antsRegistrationSyN[s]", "multivariate_extras": "MI"},
        PREFIX + "multivariate_extras[0][0]='MI' is not supported: FastWarp's SyN stage uses CC "
        "only.",
    ),
    (
        {"type_of_transform": "antsRegistrationSyN[so]", "multivariate_extras": "RADIUS4"},
        PREFIX + "multivariate_extras[0][4]=4 is not supported: FastWarp's SyN stage uses one CC "
        "radius for both channels (2 here).",
    ),
]


@pytest.mark.parametrize(("kw", "message"), REFUSALS, ids=[str(i) for i in range(len(REFUSALS))])
def test_refusals_name_the_argument_before_any_work(ants, pair, monkeypatch, tmp_path, kw, message):
    _no_engine(monkeypatch)
    fixed, moving = pair
    kw = dict(kw)
    f2, m2 = _img(ants, seed=2), _img(ants, seed=3)
    offgrid = ants.from_numpy(f2.numpy(), spacing=(1.5, 1.0, 1.0))
    extras = {
        "TWO": [("CC", f2, m2, 1, 4), ("CC", f2, m2, 1, 4)],
        "MI": [("MI", f2, m2, 1, 2)],
        "RADIUS4": [("CC", f2, m2, 1, 4)],
        "OFFGRID": [("CC", offgrid, m2, 1, 2)],
    }
    for key in ("mask", "moving_mask"):
        if kw.get(key) == "MASK":
            kw[key] = fixed
    if kw.get("multivariate_extras") in extras:
        kw["multivariate_extras"] = extras[kw["multivariate_extras"]]
    with pytest.raises(fa.UnsupportedArgument) as e:
        fa.registration(fixed, moving, outprefix=str(tmp_path / "o_"), **kw)
    expected = message.format(mask=fixed, extras=kw.get("multivariate_extras"), offgrid=offgrid)
    assert str(e.value) == expected
    assert isinstance(e.value, ValueError)
    assert not any(tmp_path.iterdir())


def test_two_dimensional_images_are_refused(ants, monkeypatch):
    _no_engine(monkeypatch)
    f = ants.from_numpy(np.ones((8, 8), "float32"))
    with pytest.raises(fa.UnsupportedArgument) as e:
        fa.registration(f, f, "Rigid")
    assert str(e.value) == (
        PREFIX + "fixed.dimension=2 is not supported: FastWarp handles 3-D images only."
    )


def test_antspy_checks_keep_antspy_messages(ants, pair, monkeypatch):
    _no_engine(monkeypatch)
    fixed, moving = pair
    with pytest.raises(ValueError, match="^Foo does not exist$"):
        fa.registration(fixed, moving, "Foo")
    with pytest.raises(ValueError, match="^Fixed and moving images must be ANTsImage objects$"):
        fa.registration(fixed.numpy(), moving)
    nan = fixed.clone()
    nan[0, 0, 0] = np.nan
    with pytest.raises(ValueError, match="^fixed image has NaNs - replace these$"):
        fa.registration(nan, moving, "Rigid")
    with pytest.raises(ValueError, match="^aff_iterations length should equal"):
        fa.registration(fixed, moving, "Rigid", aff_iterations=(1, 2, 3))
    with pytest.raises(ValueError, match="^Extra parameters are only valid for 's' or 'b'"):
        fa.registration(fixed, moving, "antsRegistrationSyN[r,4]")
    with pytest.raises(TypeError, match="unexpected keyword argument 'foo'"):
        fa.registration(fixed, moving, "Rigid", foo=1)
    with pytest.raises(ValueError, match="iso_mm must be a positive number"):
        fa.registration(fixed, moving, "Rigid", iso_mm=0)
    with pytest.raises(ValueError, match="fallback must be None or 'ants'"):
        fa.registration(fixed, moving, "Rigid", fallback="ANTs")


class _Stop(Exception):
    pass


def _stop_engine(monkeypatch):
    def stop(*args, **kwargs):
        raise _Stop

    monkeypatch.setattr(fa._core, "ants_registration", stop)


@pytest.mark.parametrize(
    ("t", "kw", "argument"),
    [
        ("SyNCC", {"aff_iterations": (10, 5, 1, 0)}, "aff_iterations=(10, 5, 1, 0)"),
        ("SyNCC", {"grad_step": 0.3}, "grad_step=0.3"),
        ("SyNabp", {"aff_metric": "MI"}, "aff_metric='MI'"),
        ("Rigid", {"syn_metric": "CC"}, "syn_metric='CC'"),
        ("DenseRigid", {"aff_random_sampling_rate": 0.5}, "aff_random_sampling_rate=0.5"),
        ("BOLDAffine", {"aff_shrink_factors": (4, 2, 1, 1)}, "aff_shrink_factors=(4, 2, 1, 1)"),
        ("antsRegistrationSyN[s]", {"use_legacy_histogram_matching": True}, None),
        ("Rigid", {"mask_all_stages": True}, "mask_all_stages=True"),
    ],
)
def test_arguments_antspy_ignores_warn(pair, monkeypatch, tmp_path, t, kw, argument):
    _stop_engine(monkeypatch)
    fixed, moving = pair
    name = argument or "use_legacy_histogram_matching=True"
    with pytest.warns(UserWarning, match="has no effect") as rec, pytest.raises(_Stop):
        fa.registration(fixed, moving, t, outprefix=str(tmp_path / "o_"), **kw)
    texts = [str(w.message) for w in rec]
    assert (
        f"{PREFIX}{name} has no effect with type_of_transform={t!r} (ANTsPy ignores it too)."
        in texts
    )


def test_the_ignored_radius_of_antsregistrationsyn_warns(pair, monkeypatch, tmp_path):
    _stop_engine(monkeypatch)
    fixed, moving = pair
    with pytest.warns(UserWarning, match="ANTsPy 0.6.3 uses radius 2"), pytest.raises(_Stop):
        fa.registration(fixed, moving, "antsRegistrationSyN[s,4]", outprefix=str(tmp_path / "o_"))


def test_fallback_runs_antspy_with_the_same_arguments(ants, pair, monkeypatch):
    _no_engine(monkeypatch)
    fixed, moving = pair
    calls = []

    def fake(*args, **kwargs):
        calls.append((args, kwargs))
        return {
            "warpedmovout": None,
            "warpedfixout": None,
            "fwdtransforms": [],
            "invtransforms": [],
        }

    monkeypatch.setattr(ants, "registration", fake)
    with pytest.warns(UserWarning, match=r"running ANTsPy instead of FastWarp \(fallback='ants'\)"):
        out = fa.registration(fixed, moving, grad_step=0.3, fallback="ants")
    assert out["engine"] == "ants"
    ((args, kwargs),) = calls
    assert args[0] is fixed and args[1] is moving
    assert kwargs["type_of_transform"] == "SyN" and kwargs["grad_step"] == 0.3
    assert set(kwargs) == set(fa._DEFAULTS)
    with pytest.raises(fa.UnsupportedArgument):
        fa.registration(fixed, moving)


def test_fallback_without_a_gpu(ants, pair, monkeypatch, tmp_path):
    fixed, moving = pair

    def no_gpu(*args, **kwargs):
        raise g.GpuUnavailableError("no GPU here")

    monkeypatch.setattr(fa._core, "ants_registration", no_gpu)
    monkeypatch.setattr(ants, "registration", lambda *a, **k: {"fwdtransforms": []})
    with pytest.raises(g.GpuUnavailableError):
        fa.registration(fixed, moving, "Rigid", outprefix=str(tmp_path / "a_"))
    with pytest.warns(UserWarning, match="no GPU here"):
        out = fa.registration(
            fixed, moving, "Rigid", outprefix=str(tmp_path / "b_"), fallback="ants"
        )
    assert out == {"fwdtransforms": [], "engine": "ants"}


def test_raw_argument_list_refusals_and_version(ants, monkeypatch, capsys):
    with pytest.raises(fa.UnsupportedArgument) as e:
        fa.registration(
            [
                "-d",
                "3",
                "-t",
                "BSplineSyN[0.1,26,0,3]",
                "-m",
                "CC[f.nii.gz,m.nii.gz,1,2]",
                "-c",
                "10",
                "-f",
                "1",
                "-s",
                "0",
                "-o",
                "x_",
            ],
            None,
        )
    assert str(e.value) == (
        PREFIX + "transform BSplineSyN is not supported: FastWarp runs Rigid, Affine and SyN."
    )
    assert fa.registration(["--version"], None) == 0
    assert "antsRegistration compatibility mode" in capsys.readouterr().out
    with pytest.raises(ValueError, match="unknown option --foo"):
        fa.registration(["--foo"], None)
    with pytest.warns(
        UserWarning, match="type_of_transform='Rigid' has no effect with an argument"
    ):
        fa.registration(["--version"], None, "Rigid")


def test_memory_images_reach_the_runner(ants, pair, monkeypatch, tmp_path):
    """Every in-memory image of a call is a tuple the runner takes, checked without a GPU: the
    argument list is swapped for ``--version`` after the images are converted."""
    real = fa._core.ants_registration
    seen = []

    def version(argv, **kwargs):
        seen.append(sorted(name for name, _ in kwargs["images"]))
        return real(["--version"], **kwargs)

    monkeypatch.setattr(fa._core, "ants_registration", version)
    fixed, moving = pair
    extras = [("CC", _img(ants, seed=2), _img(ants, seed=3), 0.5, 2)]
    for t, kw in (
        ("Rigid", {"mask": fixed}),
        ("antsRegistrationSyN[s]", {"multivariate_extras": extras}),
    ):
        with pytest.raises(KeyError):
            fa.registration(fixed, moving, t, outprefix=str(tmp_path / "o_"), **kw)
    assert seen == [
        ["<fixed>.nii", "<mask>.nii", "<moving>.nii"],
        [
            "<extra_fixed_0>.nii",
            "<extra_moving_0>.nii",
            "<fixed>.nii",
            "<moving>.nii",
        ],
    ]


def test_fallback_gets_text_paths(ants, pair, monkeypatch, tmp_path):
    _no_engine(monkeypatch)
    fixed, moving = pair
    calls = []
    monkeypatch.setattr(ants, "registration", lambda *a, **k: calls.append(k) or {})
    with pytest.warns(UserWarning):
        fa.registration(fixed, moving, outprefix=tmp_path / "p_", fallback="ants")
    assert calls[0]["outprefix"] == str(tmp_path / "p_")


def test_parse_and_seed_bindings():
    plan = g._core.ants_parse_apply(["-d", "3", "-i", "m.nii", "-r", "f.nii", "-o", "w.nii"])
    assert plan["kind"] == "run" and plan["plan"]["output"]["kind"] == "image"
    assert g._core.ants_resolve_seed(None, "9") == 9
    assert g._core.ants_resolve_seed(3, "9") == 3
    assert g._core.ants_resolve_seed() == 20260910
    with pytest.raises(ValueError, match="ANTS_RANDOM_SEED"):
        g._core.ants_resolve_seed(None, "x")


def _rot(deg):
    a = np.deg2rad(deg)
    return np.array([[np.cos(a), -np.sin(a), 0.0], [np.sin(a), np.cos(a), 0.0], [0.0, 0.0, 1.0]])


def _blob(ants, shape, spacing, origin, direction, seed=0):
    idx = np.indices(shape, dtype=np.float64)
    c = [(n - 1) / 2 for n in shape]
    r = sum(((idx[a] - c[a]) / (0.35 * shape[a])) ** 2 for a in range(3))
    vol = np.where(r <= 1.0, 100.0, 10.0) + 5.0 * np.sin(idx[0] * 0.7) * np.cos(idx[1] * 0.4)
    vol += np.random.default_rng(seed).standard_normal(shape)
    return ants.from_numpy(
        vol.astype("float32"), origin=origin, spacing=spacing, direction=direction
    )


@pytest.fixture(scope="module")
def scene(ants, tmp_path_factory):
    """An oblique moving image, a reference on another grid, two affines and an ANTs field."""
    d = tmp_path_factory.mktemp("ants_apply")
    moving = _blob(ants, (30, 34, 26), (1.1, 0.9, 1.3), (-15.0, 12.0, -8.0), _rot(12))
    reference = _blob(ants, (36, 30, 24), (0.8, 1.0, 1.5), (-16.0, 10.0, -12.0), _rot(-7), 1)
    a = ants.create_ants_transform(
        transform_type="AffineTransform",
        dimension=3,
        matrix=np.array([[1.05, 0.04, 0.0], [-0.03, 0.97, 0.02], [0.01, 0.0, 1.02]]),
        translation=(1.3, -2.1, 0.7),
        center=(2.0, 20.0, 5.0),
    )
    b = ants.create_ants_transform(
        transform_type="AffineTransform",
        dimension=3,
        matrix=np.array([[0.98, 0.0, 0.05], [0.0, 1.03, 0.0], [-0.02, 0.0, 1.0]]),
        translation=(2.0, 0.5, -1.0),
    )
    ants.write_transform(a, str(d / "a.mat"))
    ants.write_transform(b, str(d / "b.mat"))
    field = ants.apply_transforms(reference, moving, [str(d / "b.mat")], compose=str(d / "field_"))
    return d, moving, reference, str(d / "a.mat"), str(d / "b.mat"), field


def _same_grid(a, b):
    assert a.shape == b.shape
    np.testing.assert_allclose(a.spacing, b.spacing, atol=1e-9)
    np.testing.assert_allclose(a.origin, b.origin, atol=1e-9)
    np.testing.assert_allclose(a.direction, b.direction, atol=1e-12)


@pytest.mark.parametrize(
    ("which", "kw"),
    [
        ("a", {}),
        ("a", {"interpolator": "nearestNeighbor"}),
        ("a", {"interpolator": "lanczosWindowedSinc", "defaultvalue": -7}),
        ("a,field", {}),
        ("field,a", {}),
        ("a,b", {"whichtoinvert": [True, True]}),
        ("a,b", {"whichtoinvert": [None, None]}),
        ("a", {"singleprecision": True}),
    ],
)
def test_t5_apply_transforms_equals_antspy(ants, scene, which, kw):
    d, moving, reference, a, b, field = scene
    files = {"a": a, "b": b, "field": field}
    tl = [files[k] for k in which.split(",")]
    mine = fa.apply_transforms(reference, moving, tl, **kw)
    theirs = ants.apply_transforms(reference, moving, tl, **kw)
    _same_grid(mine, reference)
    assert mine.pixeltype == theirs.pixeltype
    diff = np.abs(mine.numpy() - theirs.numpy())
    assert diff.max() < 1e-3, diff.max()


def test_t5_output_takes_the_fixed_pixel_type(ants, scene):
    d, moving, reference, a, _, _ = scene
    ref8 = reference.clone("unsigned char")
    mine = fa.apply_transforms(ref8, moving, a)
    theirs = ants.apply_transforms(ref8, moving, a)
    assert mine.pixeltype == theirs.pixeltype == "unsigned char"
    assert np.abs(mine.numpy().astype(int) - theirs.numpy().astype(int)).max() <= 1


@pytest.mark.parametrize("single", [False, True])
def test_t5_compose_writes_antspys_file(ants, scene, single):
    d, moving, reference, a, b, _ = scene
    mine = fa.apply_transforms(
        reference, moving, [a, b], compose=str(d / f"mine{single}_"), singleprecision=single
    )
    theirs = ants.apply_transforms(
        reference, moving, [a, b], compose=str(d / f"theirs{single}_"), singleprecision=single
    )
    assert mine == str(d / f"mine{single}_comptx.nii.gz")
    m, t = ants.image_read(mine), ants.image_read(theirs)
    assert m.pixeltype == t.pixeltype
    _same_grid(m, t)
    assert np.abs(m.numpy() - t.numpy()).max() < 1e-4
    h5 = fa.apply_transforms(reference, moving, [a], compose=str(d / "c.h5"))
    assert h5 == str(d / "c.h5") and Path(h5).is_file()
    through_h5 = ants.apply_transforms(reference, moving, [h5])
    direct = ants.apply_transforms(reference, moving, [a])
    assert np.abs(through_h5.numpy() - direct.numpy()).max() < 1e-3


def test_t5_refusals_and_antspy_checks(ants, scene, monkeypatch):
    d, moving, reference, a, _, field = scene
    pre = "fastwarp.ants.apply_transforms: "
    with pytest.raises(fa.UnsupportedArgument) as e:
        fa.apply_transforms(reference, moving, a, interpolator="genericLabel")
    assert str(e.value) == (
        pre + "interpolator='genericLabel' is not supported: FastWarp interpolates with linear, "
        "nearestNeighbor or lanczosWindowedSinc."
    )
    with pytest.raises(fa.UnsupportedArgument) as e:
        fa.apply_transforms(reference, moving, a, imagetype=3)
    assert str(e.value) == (
        pre + "imagetype=3 is not supported: FastWarp resamples scalar 3-D images only."
    )
    with pytest.raises(fa.UnsupportedArgument, match="fixed='ref.nii.gz' is not supported"):
        fa.apply_transforms("ref.nii.gz", "mov.nii.gz", a)
    with pytest.raises(ValueError, match="^interpolator not supported - see"):
        fa.apply_transforms(reference, moving, a, interpolator="cubic")
    with pytest.raises(FileNotFoundError, match="^Transform nope.mat does not exist$"):
        fa.apply_transforms(reference, moving, "nope.mat")
    with pytest.raises(ValueError, match=r"^Cannot invert transform 0 \(.*\) because it is not"):
        fa.apply_transforms(reference, moving, [field], whichtoinvert=[True])
    with pytest.raises(ValueError, match="^Transform list and inversion list must be the same"):
        fa.apply_transforms(reference, moving, [a], whichtoinvert=[True, False])
    small_fixed, small_moving = _img(ants, seed=4), _img(ants, seed=5)
    with pytest.warns(UserWarning, match="running ANTsPy instead"):
        out = fa.apply_transforms(
            small_fixed, small_moving, a, interpolator="genericLabel", fallback="ants"
        )
    theirs = ants.apply_transforms(small_fixed, small_moving, a, interpolator="genericLabel")
    assert np.array_equal(out.numpy(), theirs.numpy())


def _head(ants, shape, spacing, shift=(0.0, 0.0, 0.0), pixeltype="float"):
    pts = np.indices(shape, dtype=np.float64) * np.asarray(spacing).reshape(3, 1, 1, 1)
    s = np.asarray(shift)
    centre = np.array([31.0, 34.0, 27.0])

    def ellipsoid(c, r):
        return sum(((pts[a] - c[a] - s[a]) / r[a]) ** 2 for a in range(3)) <= 1.0

    vol = np.zeros(shape)
    vol[ellipsoid(centre, (24, 27, 20))] = 0.3
    vol[ellipsoid(centre, (20, 23, 16))] = 0.7
    vol[ellipsoid(centre + (-5, 4, 2), (4, 6, 3))] = 0.2
    vol[ellipsoid(centre + (0, -10, -5), (5, 4, 4))] = 1.0
    vol += 0.01 * np.random.default_rng(0).standard_normal(shape)
    if pixeltype == "unsigned char":
        vol = np.clip(vol * 200, 0, 255).round()
    img = ants.from_numpy(vol.astype("float32"), spacing=spacing)
    return img.clone(pixeltype)


T4_CASES = [
    ("Rigid", {}, "unsigned char"),
    ("Rigid", {"mask": "MASK"}, "float"),
    ("antsRegistrationSyN[a]", {"mask": "MASK"}, "float"),
    ("antsRegistrationSyN[s]", {"multivariate_extras": "EXTRAS"}, "float"),
    ("Rigid", {"write_composite_transform": True}, "float"),
    ("antsRegistrationSyN[s]", {}, "float"),
    ("SyNOnly", {"syn_metric": "CC", "syn_sampling": 2, "reg_iterations": (20, 10)}, "float"),
]


def _suffixes(result, prefix):
    out = {}
    for key in ("fwdtransforms", "invtransforms"):
        v = result[key]
        out[key] = v[len(prefix) :] if isinstance(v, str) else [f[len(prefix) :] for f in v]
    return out


@needs_gpu
@pytest.mark.parametrize(("t", "kw", "pixeltype"), T4_CASES)
def test_t4_return_dict_is_antspys(ants, tmp_path, monkeypatch, t, kw, pixeltype):
    monkeypatch.setattr(sys.modules["ants.config"], "_random_seed", 20260910)
    fixed = _head(ants, (32, 34, 28), (2.0, 2.0, 2.0), pixeltype=pixeltype)
    moving = _head(ants, (30, 36, 30), (2.2, 1.9, 1.8), shift=(3.0, -2.0, 1.5))
    masked = kw.get("mask") == "MASK"
    if masked:
        kw = {**kw, "mask": ants.threshold_image(fixed.clone("float"), 0.15, 1e9)}
    if kw.get("multivariate_extras") == "EXTRAS":
        kw = {**kw, "multivariate_extras": [("CC", fixed * 2.0, moving * 2.0, 0.5, 2)]}
    theirs = ants.registration(fixed, moving, t, outprefix=str(tmp_path / "a_"), **kw)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        mine = fa.registration(fixed, moving, t, outprefix=str(tmp_path / "f_"), **kw)
    skipped = [str(w.message) for w in caught if "was skipped" in str(w.message)]
    if masked:
        assert skipped, "the coarse masked levels have too few samples and are skipped"
        assert all("level" in w and "candidate points" in w for w in skipped)
    assert set(mine) == set(theirs)
    assert _suffixes(mine, str(tmp_path / "f_")) == _suffixes(theirs, str(tmp_path / "a_"))
    for key in ("fwdtransforms", "invtransforms"):
        files = mine[key] if isinstance(mine[key], list) else [mine[key]]
        assert all(Path(f).is_file() for f in files)
    produced = sorted(p.name[2:] for p in tmp_path.iterdir() if p.name.startswith("f_"))
    expected = sorted(p.name[2:] for p in tmp_path.iterdir() if p.name.startswith("a_"))
    assert produced == expected
    for key, grid in (("warpedmovout", fixed), ("warpedfixout", moving)):
        _same_grid(mine[key], grid)
        _same_grid(theirs[key], grid)
        assert mine[key].pixeltype == theirs[key].pixeltype == pixeltype
    if pixeltype == "float" and not masked:
        a = mine["warpedmovout"].numpy().ravel()
        b = theirs["warpedmovout"].numpy().ravel()
        assert np.corrcoef(a, b)[0, 1] > 0.9


@needs_gpu
def test_t9_two_runs_are_identical(ants, tmp_path):
    fixed = _head(ants, (32, 34, 28), (2.0, 2.0, 2.0))
    moving = _head(ants, (30, 36, 30), (2.2, 1.9, 1.8), shift=(3.0, -2.0, 1.5))
    one = fa.registration(fixed, moving, "QuickRigid", outprefix=str(tmp_path / "one_"))
    two = fa.registration(fixed, moving, "QuickRigid", outprefix=str(tmp_path / "two_"))
    assert np.array_equal(one["warpedmovout"].numpy(), two["warpedmovout"].numpy())
    assert Path(one["fwdtransforms"][0]).read_bytes() == Path(two["fwdtransforms"][0]).read_bytes()


def _contents(path: Path) -> bytes:
    data = path.read_bytes()
    return gzip.decompress(data) if path.name.endswith(".gz") else data


@needs_gpu
def test_raw_argument_list_writes_what_the_subcommand_writes(ants, tmp_path):
    f, m = tmp_path / "f.nii.gz", tmp_path / "m.nii.gz"
    ants.image_write(_head(ants, (32, 34, 28), (2.0, 2.0, 2.0)), str(f))
    ants.image_write(_head(ants, (30, 36, 30), (2.2, 1.9, 1.8), shift=(3.0, -2.0, 1.5)), str(m))

    def argv(prefix):
        return [
            "-d",
            "3",
            "-r",
            f"[{f},{m},1]",
            "-t",
            "Rigid[0.1]",
            "-m",
            f"MI[{f},{m},1,32,Regular,0.25]",
            "-c",
            "[20x10,1e-6,10]",
            "-f",
            "2x1",
            "-s",
            "1x0vox",
            "-t",
            "SyN[0.2,3,0]",
            "-m",
            f"CC[{f},{m},1,2]",
            "-c",
            "[10x5,1e-6,10]",
            "-f",
            "2x1",
            "-s",
            "1x0vox",
            "-o",
            f"[{prefix},{prefix}W.nii.gz,{prefix}IW.nii.gz]",
            "--float",
            "1",
        ]

    assert fa.registration(argv(tmp_path / "py_"), None) == 0
    p = subprocess.run(
        [sys.executable, "-m", "fastwarp", "ants-registration", *argv(tmp_path / "cli_")],
        capture_output=True,
        text=True,
        timeout=900,
    )
    assert p.returncode == 0, p.stderr
    py = sorted(x.name[3:] for x in tmp_path.iterdir() if x.name.startswith("py_"))
    cli = sorted(x.name[4:] for x in tmp_path.iterdir() if x.name.startswith("cli_"))
    assert (
        py
        == cli
        == sorted(
            ["0GenericAffine.mat", "1Warp.nii.gz", "1InverseWarp.nii.gz", "W.nii.gz", "IW.nii.gz"]
        )
    )
    for name in py:
        assert _contents(tmp_path / f"py_{name}") == _contents(tmp_path / f"cli_{name}"), name


@needs_gpu
def test_a_composite_from_one_run_starts_the_next(ants, tmp_path):
    fixed = _head(ants, (32, 34, 28), (2.0, 2.0, 2.0))
    moving = _head(ants, (30, 36, 30), (2.2, 1.9, 1.8), shift=(3.0, -2.0, 1.5))
    rigid = fa.registration(
        fixed, moving, "Rigid", outprefix=str(tmp_path / "r_"), write_composite_transform=True
    )
    assert rigid["fwdtransforms"].endswith("Composite.h5")
    syn = fa.registration(
        fixed,
        moving,
        "SyNCC",
        initial_transform=rigid["fwdtransforms"],
        outprefix=str(tmp_path / "s_"),
    )
    assert [Path(f).name for f in syn["fwdtransforms"]] == [
        "s_1Warp.nii.gz",
        "s_0GenericAffine.mat",
    ]
    assert all(Path(f).is_file() for f in syn["fwdtransforms"] + syn["invtransforms"])


APPLY_CASES = [
    ("a", {}),
    ("a", {"interpolator": "nearestNeighbor", "defaultvalue": -3}),
    ("a", {"interpolator": "lanczosWindowedSinc", "singleprecision": True, "verbose": True}),
    ("a,field", {}),
    ("field,a", {}),
    ("a,b", {"whichtoinvert": [True, False]}),
    ("a,b", {"whichtoinvert": [None, None]}),
    ("a,b", {"compose": "COMPOSE"}),
    ("a", {"compose": "H5"}),
]


@pytest.mark.parametrize(("which", "kw"), APPLY_CASES)
def test_t1_apply_argument_list_is_antspys(ants, scene, monkeypatch, which, kw):
    d, moving, reference, a, b, field = scene
    files = {"a": a, "b": b, "field": field}
    tl = [files[k] for k in which.split(",")]
    kw = dict(kw)
    if kw.get("compose") == "COMPOSE":
        kw["compose"] = str(d / "cmp_")
    if kw.get("compose") == "H5":
        kw["compose"] = str(d / "cmp.h5")
    module = sys.modules["ants.registration.apply_transforms"]
    internal = sys.modules["ants.internal"]
    labels = iter(["M", "WM", "F"] if kw.get("compose") is None else ["M", "F"])
    captured = []

    def lib(name):
        def run(args):
            captured.append(list(args))
            return 0

        return run

    monkeypatch.setattr(internal, "get_pointer_string", lambda img: next(labels))
    monkeypatch.setattr(module, "get_lib_fn", lib)
    ants.apply_transforms(reference, moving, tl, **kw)
    invert = fa._which_to_invert(tl, kw.get("whichtoinvert"))
    mine, _ = fa._apply_argv(
        tl,
        kw.get("interpolator", "linear"),
        invert,
        kw.get("compose"),
        kw.get("imagetype", 0),
        kw.get("defaultvalue", 0),
        kw.get("singleprecision", False),
        kw.get("verbose", False),
        {"fixed": "F", "moving": "M", "warped": "WM"},
    )
    assert mine == captured[0]


@needs_gpu
def test_skipped_levels_warn_in_linear_sessions_and_the_command_line(ants, tmp_path):
    fixed = _head(ants, (32, 34, 28), (2.0, 2.0, 2.0))
    moving = _head(ants, (30, 36, 30), (2.2, 1.9, 1.8), shift=(3.0, -2.0, 1.5))
    mask = ants.threshold_image(fixed, 0.15, 1e9)
    stage = {
        "iterations": [50, 20, 20, 5],
        "shrink_factors": [6, 4, 2, 1],
        "smoothing_sigmas": [3, 2, 1, 0],
        "sampling_percentage": 0.2,
    }
    with (
        g.linear_session(fixed, moving, fixed_mask=mask) as session,
        pytest.warns(UserWarning, match="was skipped"),
    ):
        out = session.register(stage=stage)
    assert out["warnings"] and all("rigid stage: level" in w for w in out["warnings"])
    f, m, x = tmp_path / "f.nii.gz", tmp_path / "m.nii.gz", tmp_path / "x.nii.gz"
    for img, path in ((fixed, f), (moving, m), (mask, x)):
        ants.image_write(img, str(path))
    argv = [
        "ants-registration",
        "-d",
        "3",
        "-r",
        f"[{f},{m},1]",
        "-t",
        "Rigid[0.25]",
        "-m",
        f"MI[{f},{m},1,32,Regular,0.2]",
        "-c",
        "50x20x20x5",
        "-f",
        "6x4x2x1",
        "-s",
        "3x2x1x0",
        "-x",
        f"[{x},NA]",
        "-o",
        str(tmp_path / "cli_"),
    ]
    p = subprocess.run(
        [sys.executable, "-m", "fastwarp", *argv], capture_output=True, text=True, timeout=600
    )
    assert p.returncode == 0, p.stderr
    assert "fastwarp: warning: ants-registration: linear stage 1 (rigid): level" in p.stderr
