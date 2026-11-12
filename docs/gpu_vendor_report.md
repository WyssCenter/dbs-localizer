# Graphics-API and vendor validation of the wgpu backend (WP9)

*Work package WP9 of the GPU follow-up plan (retired 2026-10-23, in git history). The engine and its per-backend known-answer
self-test landed in roadmap 3.18 (`docs/gpu_syn_cohort_report.md`, `docs/gpu_coreg_report.md`,
`docs/gpu_n4_report.md`). This report records the cross-API and cross-vendor validation runs.
Run 9a (DirectX 12 on the Arc 140V, 2026-10-08) is complete; runs 9b (NVIDIA Vulkan), 9c (NVIDIA
DirectX 12) and 9d (Apple Metal) are pending — the user runs them on those machines.*

## What the runs share

Every run uses the WP5 harness (`scripts/leadtutor_cohort.py`) with the same 10-subject
Lead-Tutor cohort, the 1.0 mm isotropic grid (`--norm-isotropic-mm 1.0`), the production stage
set `--stages normalize,reconstruct`, and a seed copied from the `ms15` run's `preprocessing` +
`coregistration`, so only normalize and reconstruct differ between runs. The reference is the
Arc 140V **Vulkan** run (integrated pipeline: 1.101 mm over all 10 subjects, WP6). D9
(D9, the rule `scripts/leadtutor_cohort.py compare` applies) requires mean ≤ 1.142 mm, no subject worse than the ANTs baseline by
more than 0.5 mm, and the run mean within 0.02 mm of the Vulkan reference. Each backend's kernels
are checked on known-answer data at device creation and recompiled on a mismatch; the attempts and
the verdict are recorded per kernel family in `reg_adapter` (`self_test_attempts_syn`,
`self_test_attempts_mi`, `verdict`).

## Per-run results

On the 9 subjects that ran, the Arc Vulkan mean is **0.994 mm** (the full 10-subject WP6 run is
1.101 mm; `15454` is the subject DX12 refuses, below).

| Run | Machine / adapter | Driver | Backend | Subjects run | Mean (20 leads) | Max per-lead change vs Arc Vulkan | Determinism | Self-test attempts / verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 9a | Intel Arc 140V (this laptop) | not captured in this report | DirectX 12 (`DBSLOC_GPU_BACKEND=dx12`) | 9/10 (`15454` refused) | **0.993 mm** (Vulkan 0.994, Δ −0.001) | 0.109 mm (`33544` −x) | two runs of `57245` and `84257` byte-identical (`Composite.h5`, `InverseComposite.h5`, `_Warped.nii.gz`) | SyN `smooth_field` failed on 2 attempts (`15454`); MI passed; run otherwise `GO` |
| 9b | NVIDIA (user) | pending | Vulkan | pending (user) | pending | pending | pending | pending |
| 9c | NVIDIA (user) | pending | DirectX 12 | pending (user) | pending | pending | pending | pending |
| 9d | Apple (user) | pending | Metal | pending (user) | pending | pending | pending | pending |

### Run 9a notes

- **9/10 ran.** `15454` was **refused**, not run on wrong results: the SyN `smooth_field` kernel's
  known-answer self-test failed on the DX12 driver (variance 0.3 / 0.8 / 3.0 gave relative errors
  1.04 / 2.64 / 0.82 against a tolerance of 2e-6) after 2 attempts with freshly compiled kernels —
  an intermittent DX12 driver miscompile of that kernel. Because the request was an explicit
  `wgpu`, the stage errors with `E_REG_BACKEND` rather than silently running the wrong field.
- **D9 passes** on the 9 subjects: mean 0.993 mm, within 0.02 mm of the Vulkan reference, and the
  largest per-lead change is 0.109 mm (`33544` −x).
- **Accuracy across APIs** is at the self-test-noise level: DX12 0.993 mm vs Vulkan 0.994 mm over
  the same 9 subjects (Δ −0.001 mm).

## Run sheet (NVIDIA machine)

One GPU/ANTs job at a time; keep the run roots outside `%TEMP%` (Storage Sense deletes old-mtime
files there), as that plan required.

1. **Seed.** Copy `<LeadTutor>/hpdbs_runs/ms15` (its `preprocessing` +
   `coregistration` outputs) to the NVIDIA machine as plain copies, e.g.
   `<runs>/nvidia_seed`. Pass that path as `--seed-from`.
2. **NVIDIA Vulkan:**
   ```
   uv run python scripts/leadtutor_cohort.py run --data-root <LeadTutor/LeadTutor> --run-root <runs>/nvidia_vulkan --template <MNI152NLin2009bAsym/t1.nii> --norm-isotropic-mm 1.0 --reg-backend wgpu --stages normalize,reconstruct --seed-from <copied ms15> --gpu-backend vulkan
   ```
3. **NVIDIA DirectX 12:** the same line with `--run-root <runs>/nvidia_dx12 --gpu-backend dx12`.
4. **Score.** Experts file `<experts.json>` (expert markers, not redistributed):
   ```
   uv run python scripts/leadtutor_cohort.py score --run-root <runs>/nvidia_vulkan --experts <experts.json>
   ```
   Do the same for `<runs>/nvidia_dx12`.
5. **Compare** each score against the Vulkan reference (`<msg10_scores.json>` is the 1.101 mm
   Vulkan-equivalent stage set):
   ```
   uv run python scripts/leadtutor_cohort.py compare --from-scores <msg10_scores.json> <runs>/nvidia_vulkan/scores.json
   uv run python scripts/leadtutor_cohort.py compare --from-scores <msg10_scores.json> <runs>/nvidia_dx12/scores.json
   ```
6. **Determinism.** Re-run one subject (e.g. `57245`) into a fresh root with the same seed and
   compare:
   ```
   uv run python scripts/leadtutor_cohort.py determinism <runs>/nvidia_vulkan <runs>/nvidia_vulkan_repeat
   ```
7. **Record** the adapter, driver, backend, mean, max per-lead change, determinism and self-test
   attempts in the table above (`docs/gpu_vendor_report.md`), one row per run.

If a backend fails D9: bisect with `fastwarp_reg_ab` on the worst subject (`--ours` against the Arc
Vulkan composite), then the kernel parity tests on that adapter; record the outcome and do not
change defaults without the user (WP9 of the follow-up plan).
