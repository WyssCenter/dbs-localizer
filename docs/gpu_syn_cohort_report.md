# GPU SyN vs ANTs — Lead-Tutor cohort (10 subjects)

Date: 2026-10-07. Code: `fastwarp/crates/fastwarp-reg` (`register`, ITK-faithful `syn`), `fastwarp/crates/fastwarp-gpu` (`WgpuBackend`, Vulkan, f32), tools `fastwarp_reg_syn` and `fastwarp_reg_ab`. Machine: Intel Core Ultra 7 268V, Intel Arc 140V; an ANTs cohort run was using 4 CPU threads at the same time.

**Setup.** For every subject, the SyN stage of the production normalize (1.0 mm isotropic registration grid, T1w + T2w with CC weights 0.5 / 0.5, T1w only for `76325`; `SynRecipe::cohort()` = `SyN[0.1,3,0]`, CC radius 4, `[100x70x50x20,1e-6,10]`, `8x4x2x1`, `3x2x1x0vox`) was run on the GPU, starting from the affine of the ANTs run that scored **1.092 mm** (run `ms10`, `Composite.h5` element 0). The SyN fields start at zero. Our `Composite.h5` / `InverseComposite.h5` were compared with ANTs': displacement difference inside the brain (template T1w > 0.1 × its 99th percentile, 4.39 M voxels), forward Jacobian, and the native contact coordinates of the reconstruction mapped to template space the way `point_warp.rs` does (that route reproduces the stored ANTs `mni` coordinates to < 1e-6 mm, column `repro`).

| subject | channels | SyN s | total s | field RMS diff mm | field max diff mm | contacts mean mm | contacts max mm | Jacobian min (ours / ANTs) | % J ≤ 0 | repro mm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 57245 | 2 | 31.8 | 38.3 | 0.0013 | 0.028 | 0.00048 | 0.00084 | 0.263 / 0.263 | 0.0 | 3.8e-07 |
| 84257 | 2 | 30.0 | 36.6 | 0.0008 | 0.010 | 0.00033 | 0.00043 | 0.316 / 0.316 | 0.0 | 6.1e-07 |
| 76325 | 1 | 15.8 | 21.4 | 0.0028 | 0.043 | 0.00077 | 0.00129 | 0.371 / 0.371 | 0.0 | 3.1e-07 |
| 15454 | 2 | 32.1 | 38.6 | 0.0013 | 0.014 | 0.00012 | 0.00027 | 0.290 / 0.290 | 0.0 | 3.9e-07 |
| 29781 | 2 | 25.2 | 30.8 | 0.0047 | 0.074 | 0.00061 | 0.00093 | 0.220 / 0.220 | 0.0 | 4.3e-07 |
| 33544 | 2 | 33.0 | 39.5 | 0.0000 | 0.002 | 0.00001 | 0.00001 | 0.261 / 0.261 | 0.0 | 4.6e-07 |
| 39468 | 2 | 24.8 | 31.3 | 0.0021 | 0.037 | 0.00076 | 0.00090 | 0.253 / 0.254 | 0.0 | 3.8e-07 |
| 78754 | 2 | 25.2 | 32.1 | 0.0017 | 0.036 | 0.00032 | 0.00040 | 0.303 / 0.303 | 0.0 | 6.3e-07 |
| 80206 | 2 | 30.6 | 36.8 | 0.0014 | 0.026 | 0.00247 | 0.00483 | 0.261 / 0.261 | 0.0 | 4.2e-07 |
| 93127 | 2 | 22.7 | 29.1 | 0.0020 | 0.061 | 0.00067 | 0.00097 | 0.373 / 0.373 | 0.0 | 7.5e-07 |

**Result.** Over all 200 contacts the GPU SyN moves the template-space coordinates by **0.00065 mm on average, 0.0048 mm at most**, against ANTs. The marker error of the cohort (1.092 mm, from the same reconstructions) is therefore unchanged by the GPU SyN. No folding on either side. The SyN stage takes **27.1 s** on average (15.8–33.0 s; whole tool including load, resample, preprocess and writing: 21–40 s), against 45–60 min for the ANTs normalize, which also contains the linear stages.

**Scope of this table.** The rigid / affine stages and the T2w → anchor coregistration come from ANTs here; the integrated pipeline below runs them on the GPU too. One subject per run of the A/B is in the ignored test `fastwarp/crates/fastwarp-gpu/tests/ab_real_subject.rs` (`DBSLOC_REG_AB_DIR`).

## Integrated pipeline (WP6, 2026-10-08)

`dbsloc run … --stages normalize,reconstruct --norm-isotropic-mm 1.0 --reg-backend wgpu` on the same 10 subjects (preprocess and CT coregistration taken from the earlier ANTs run): normalize runs without any ANTs process (T2w → anchor rigid, centre of mass → Rigid → Affine, SyN on the GPU; the CT apply onto the template grid in Rust on the CPU; native contacts mapped through `InverseComposite.h5` in Rust).

| subject | ANTs (+x / −x) mm | GPU pipeline (+x / −x) mm |
| --- | --- | --- |
| 15454 | 1.944 / 2.249 | 1.912 / 2.227 |
| 29781 | 0.775 / 0.630 | 0.775 / 0.629 |
| 33544 | 1.093 / 1.116 | 1.023 / 1.338 |
| 39468 | 0.643 / 1.097 | 0.648 / 1.098 |
| 57245 | 0.814 / 0.723 | 0.826 / 0.734 |
| 76325 | 1.796 / 1.493 | 1.805 / 1.485 |
| 78754 | 1.012 / 0.556 | 1.033 / 0.572 |
| 80206 | 0.703 / 1.146 | 0.710 / 1.167 |
| 84257 | 0.825 / 1.483 | 0.808 / 1.486 |
| 93127 | 0.804 / 0.947 | 0.806 / 0.942 |
| **mean (20 leads)** | **1.092** | **1.101** |

Plan acceptance (D9, the rule `scripts/leadtutor_cohort.py compare` applies: mean ≤ 1.142 mm, no subject more than 0.5 mm worse) is met. Normalize + reconstruct took 1–2 min per subject on the Arc 140V (ANTs: about 50 min). Two runs on one machine give byte-identical `Composite.h5` and `InverseComposite.h5` (checked on `57245` and `84257`, sha256). DX12 on the same Arc is covered by WP9 (`docs/gpu_vendor_report.md`): 9/10 subjects ran, mean 0.993 mm; other GPU vendors are pending.

## Finer grids: 0.8 mm and 0.5 mm (WP8, 2026-10-08)

**Binding split.** The five CC window planes (`a`, `b`, `a²`, `b²`, `ab`) and the five box sums are now bound as five separate `n · 4` storage buffers instead of one packed `n · 20` buffer, and the CC value/derivative kernel is split into `k_cc_deriv` and `k_cc_value` so no stage binds more than 8 storage buffers (`max_storage_buffers_per_shader_stage`). The arithmetic and the reduction order are unchanged: the 1.0 mm SyN outputs of `57245` and `84257` are **byte-identical** to the stored WP6 sha256 (`msg10_normalization_sha256.txt`).

**Capacity.** With the split layout the largest single binding of `fastwarp_gpu::plan_capacity` is the three-plane displacement field (833 MB at 0.5 mm, grid 394×466×378 = 69,402,312 voxels), so the binding boundary moves to 447³ and 0.5 mm passes the binding check; the total finest-level allocation is ≈ 11.24 GB, against the Arc 140V's 17.24 GiB shared heap.

**Cohort at 0.8 mm.** `--stages normalize,reconstruct --reg-backend wgpu --norm-isotropic-mm 0.8`, seeded from the GPU preprocessing + coregistration of the WP11 run: **mean 1.141 mm** over 20 leads, against the 1.0 mm GPU run (1.076 mm) and the ANTs 1.0 mm baseline (1.092 mm). Finer is not better here, matching the ANTs 0.5 mm spot check (`80206`: 1.16 / 1.52 mm at 0.5 mm vs 0.70 / 1.15 at 1.0 mm).

**Cohort at 0.5 mm: fails on device memory.** After the binding fix the run reaches the finest SyN level and then the device runs out of memory allocating the 833 MB three-plane field (`out of memory allocating 832827744 bytes for field`), after about 187 s on `57245`, even with `DBSLOC_GPU_MEMORY_BUDGET_MB=16000`. The binding blocker is removed; 0.5 mm does not fit the Arc's practical integrated-GPU budget. The deliverable is the measurement, not a default change.
