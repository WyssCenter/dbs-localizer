# CT → anchor coregistration on the wgpu backend (WP7)

*Work package WP7 of the GPU follow-up plan (retired 2026-10-23, in git history). The engine half (mask, initial transform,
final metric) landed in `fastwarp/crates/fastwarp-reg`; the pipeline half wired the `wgpu` branch of
`run_coregister`. This report records the Lead-Tutor cohort acceptance run (2026-10-08).*

## What ran

`scripts/leadtutor_cohort.py run` with `--reg-backend wgpu --stages coreg,normalize,reconstruct
--norm-isotropic-mm 1.0`, seeded from the `ms15` run's `preprocessing` only (the coregistration
stage was excluded, so `coreg` runs on the GPU), on the Arc 140V (Vulkan). All 10 Lead-Tutor
subjects. The ANTs baseline is `ms15`/`ms10` (ANTs coreg + ANTs normalize, mean 1.092 mm).

## Cohort result

Per-subject marker error against the expert markers, GPU coreg + GPU normalize + reconstruct:

| subject | +x | −x | mean | Δ vs ANTs |
| --- | --- | --- | --- | --- |
| 15454 | 2.539 | 2.444 | 2.491 | +0.395 |
| 29781 | 0.568 | 0.781 | 0.674 | −0.028 |
| 33544 | 1.244 | 1.330 | 1.287 | +0.183 |
| 39468 | 0.657 | 1.173 | 0.915 | +0.044 |
| 57245 | 0.613 | 0.553 | 0.583 | −0.186 |
| 76325 | 1.777 | 1.483 | 1.630 | −0.014 |
| 78754 | 1.033 | 0.569 | 0.801 | +0.017 |
| 80206 | 0.717 | 1.206 | 0.962 | +0.037 |
| 84257 | 0.794 | 1.377 | 1.085 | −0.069 |
| 93127 | 0.798 | 0.930 | 0.864 | −0.012 |

- **Mean 1.129 mm** over 20 leads vs the ANTs baseline **1.092 mm** (Δ +0.037).
- Largest per-lead change 0.594 mm (`15454` +x); worst subject change +0.395 mm (`15454`).
- **D9 rule passes**: mean ≤ reference + 0.05 mm, no subject worse by > 0.5 mm.

The GPU normalize alone (ANTs coreg) scored 1.101 mm (Δ +0.009, `docs/gpu_syn_cohort_report.md`),
so the GPU coreg contributes about +0.03 mm of the mean.

## Multi-start pitch

Chosen coarse start pitch, GPU vs ANTs:

| subject | GPU | ANTs | note |
| --- | --- | --- | --- |
| 15454 | +40 | +20 | clear disagreement (GPU metric margin ≈ 15 %) |
| 29781 | +40 | +40 | agree |
| 33544 | +20 | +40 | |
| 39468 | +0 | +20 | |
| 57245 | +20 | +20 | agree |
| 76325 | +0 | +0 | agree |
| 78754 | +20 | +0 | |
| 80206 | +0 | +0 | agree |
| 84257 | +20 | +20 | agree |
| 93127 | +40 | −20 | near-tie: GPU metrics −0.4845 (+40) vs −0.4844 (−20) |

Agreement 5/10. Per decision E7 of the follow-up plan these disagreements are **counted, not
failed on**: the full masked rigid converges to a comparable optimum from any start (D9 passes),
and `93127` is a near-tie. `15454` and `39468` are the two subjects where the coarse GPU metric
clearly prefers a different start than ANTs; the coarse metric landscape is the remaining
accuracy gap, and it is the subject of further work.

## Timing

GPU coreg wall time per subject 81–290 s (mean ≈ 157 s) against the ANTs coreg 64–125 s (mean
≈ 88 s). The plan's 5–10 s estimate is not met: the `wgpu` coreg is host-overhead-bound (per-level
full-resolution recursive smoothing, sample generation and per-iteration read-backs), the same
bottleneck documented for the SyN stages. Normalize on the GPU in the same run took 30–101 s.

## Determinism

A second run of `57245` and `33544` into a fresh root, seeded identically, produced **byte-identical**
`Composite.h5`, `InverseComposite.h5` and `_Warped.nii.gz` for the CT coreg and the SyN stages.

## Non-finite CT voxels

`39468` stores its CT background (≈ 21 % of the volume) as NaN. ITK replaces those voxels with 0:
an identity `antsApplyTransforms` of the raw CT under the bundled ANTs returns ≈ 0 at the NaN
voxels, and the ANTs-warped CT contains no NaN. The `wgpu` path now does the same
(`stage_impl::replace_non_finite` maps non-finite voxels to 0 before preprocessing), and the
preprocessing histogram ignores non-finite voxels with `std::min`/`std::max` semantics
(`fastwarp/crates/fastwarp-reg/src/preprocess.rs`), matching `itkImageToHistogramFilter`. With that, `39468`
registers and its coarse start metrics reproduce ANTs (`−0.2860` vs `−0.2856` at −40°).
