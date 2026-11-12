# N4 bias-field correction in-process on the wgpu backend (WP11)

*Work package WP11 of the GPU follow-up plan (retired 2026-10-23, in git history). The engine half (the `f64` CPU reference in
`fastwarp/crates/fastwarp-reg/src/n4.rs`; the GPU kernels and host loop in `fastwarp/crates/fastwarp-gpu`: `gpu_n4.rs`,
`n4_kernels.rs`, `shaders/n4.wgsl`) and the pipeline half (`run_preprocess` resolving
`reg_backend` the way `normalize` and `coreg` do) are implemented. This report records the
Lead-Tutor cohort acceptance run (2026-10-08).*

## What ran

`scripts/leadtutor_cohort.py run` with `--stages preprocess,coreg,normalize,reconstruct
--reg-backend wgpu --norm-isotropic-mm 1.0`, seeded from the raw data root only (no ANTs output
used), on the Arc 140V (Vulkan). All 10 Lead-Tutor subjects. A `wgpu` run starts no ANTs process:
the `preprocess` record carries `reg_backend: wgpu`, `run_preprocess` never calls
`N4BiasFieldCorrection`, and the `N4BiasFieldCorrection` prerequisite check is skipped. The ANTs
baseline is the 1.0 mm T1w + T2w run that scored **1.092 mm**.

## Cohort result

Per-lead marker error against the expert markers, the full `wgpu` pipeline
(preprocess, coreg, normalize, reconstruct):

- **Mean 1.076 mm** over 20 leads vs the ANTs baseline **1.092 mm** (Δ −0.017).
- **D9 rule passes**: mean ≤ reference + 0.05 mm, no subject worse by > 0.5 mm.
- Largest per-lead change 0.643 mm (`15454` +x); worst subject change +0.325 mm (`15454`).

The N4 is the only difference from the WP6/WP7 runs (ANTs N4, GPU coreg + GPU normalize), and its
effect on the final marker error is at the level of the coregistration start optimum, not the SyN.

## Timing

Per subject, from the `reg_timings` block of `preprocessing/anat/<stem>_N4.json`:

| Stage | In-process (`wgpu`) | Bundled ANTs |
| --- | --- | --- |
| N4 (`n4_s`) | 9.7–23.2 s | 37–54 s |
| read + N4 + write (`total_s`) | 10.5–24.1 s | — |

The in-process N4 is about twice as fast as ANTs. The plan's acceptance target of ≤ 3 s per subject
is **not met**: the port is still host-overhead-bound (the `f32` evaluation, the per-iteration
histogram build and the small read-backs), the same bottleneck documented for the SyN and coreg
stages.

## Determinism

A second run of `57245` and `33544` from raw, into a fresh root, produced **byte-identical**
`Composite.h5`, `InverseComposite.h5` and `_Warped.nii.gz`.

## Accuracy of the engine

- CPU `f64` reference vs the bundled ANTs N4: mean |ratio − 1| ≈ 1e-6 inside the head.
- GPU vs the CPU reference: ≈ 3e-5 relative per voxel on a small case, 1.36e-4 at the full
  4-level × 50-iteration schedule.

## Kernel dispatch bug found and fixed

The flat N4 kernels were dispatched as a 1-D workgroup grid. On a real 256×256×176 T1w that is
about 180 000 workgroups, past `max_compute_workgroups_per_dimension` (65535), so `n4 eval` failed
with a validation error. They now use a 2-D workgroup grid with the x-stride in the uniform
(`fastwarp/crates/fastwarp-gpu/src/gpu_n4.rs`, `n4_kernels.rs`, `shaders/n4.wgsl`); regression test
`gpu_n4_handles_a_grid_past_the_1d_dispatch_limit`.

## Record

`preprocessing/anat/<stem>_N4.json` carries `reg_backend`, `reg_requested`, `reg_note`,
`reg_adapter`, `reg_timings` (`read_s`, `n4_s`, `write_s`, `total_s`), the N4 parameter block and
the per-level convergence values.
