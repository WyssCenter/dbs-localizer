# Performance

## Registration

Data: T1w + T2w of Lead-Tutor sub-57245 (raw T1w, T2w aligned to it by one rigid fit) onto the
MNI 2009b template (0.5 mm), 1 mm registration grid, default recipe, a development build before 0.1.0 (internal version 0.4.0). Each
figure is the median of three runs of the engine's own `linear_s`, `syn_s` and `total_s`
(2026-11-09).

| Hardware | Linear | SyN | Registration total |
| --- | --- | --- | --- |
| NVIDIA RTX A4000 (Linux, Vulkan) | 3.5 s | 6.4 s | 12.4 s |
| Apple M5 Pro (MacBook Pro, Metal) | 4.1 s | 9.4 s | 13.7 s |
| Intel Lunar Lake integrated GPU (Linux laptop, Vulkan) | 3.5 s | 16.4 s | 21.0 s |
| AMD Radeon Pro 5500 XT (Intel iMac, Metal) | 6.1 s | 16.7 s | 23.5 s |
| NVIDIA Quadro K620, 2 GB (Windows Server 2022, Vulkan), T1w only[^1] | 10.6 s | 54.7 s | 67.3 s |
| ANTs on 16 CPU threads, same recipe | | | 1000 to 1500 s |

The total includes preparing and uploading the images. The A4000 machine was under heavy CPU
load (load average about 110 on 48 cores) during its runs.

[^1]: 2 GB is too little for T1w + T2w at 1 mm (1.6 GB): the capacity check refuses it up
    front (`GpuCapacityError`), so a caller falls back to ANTs. T1w alone needs 1.4 GB and ran
    only with `FASTWARP_GPU_MEMORY_BUDGET_MB=1900`, since the default budget keeps 1 GB free for
    the desktop.

## Linear sessions

One rigid fit of the T2w onto the T1w (the default rigid stage) takes 1.2 s on the M5 Pro, 1.6 s
on the RTX A4000, 1.8 s on the Radeon Pro 5500 XT, 2.1 s on Lunar Lake and 2.7 s on the Quadro
K620.

In a typical DBS localisation workflow on the Lead-Tutor cohort: a post-op CT onto the T1w anchor, five coarse
and two full rigid fits, takes 17 s on the laptop GPU where ANTs takes 32 s. The masked affine
brain-shift fit takes 2.4 to 4.8 s on the RTX A4000 where ANTs takes 5 to 11 s.

## Post-processing

Turning the composites into displacement fields on the 0.5 mm template and anchor grids, and
the Lanczos re-warp of the anchor (what Lead-DBS does next), on a laptop with 8
threads:

| Step | ANTs (ITK) | `compose_field` / `apply_transforms` |
| --- | --- | --- |
| Forward field (69 M voxels) | 36.5 s | 1.4 s |
| Inverse field (37 M voxels) | 20.9 s | 0.95 s |
| Lanczos re-warp | 31.0 s | 2.7 s |

TODO(author): state how the ANTs timings were produced (command lines, the ANTs version for the
registration timings) and which laptop the post-processing table used. The repository README
does not say. The GPU benchmarks in `crates/fastwarp-gpu/tests/syn_bench.rs` run with
`FASTWARP_GPU_BENCH`; TODO(author): say whether the table above came from them or from another
script, and where that script is.
