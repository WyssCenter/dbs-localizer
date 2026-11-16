# Environment variables

FastWarp reads these variables when a function opens the GPU, so they can be set in the shell
or in Python (`os.environ[...] = ...`) before the call.

Before version 0.1.0 the engine was part of Wyss DBS Localizer and used the prefix `DBSLOC_`.
Each `FASTWARP_` variable falls back to its `DBSLOC_` name (for example `DBSLOC_GPU_BACKEND` for
`FASTWARP_GPU_BACKEND`) when the `FASTWARP_` name is not set. A `FASTWARP_` variable set to an
empty value counts as set.

Switches such as `FASTWARP_GPU_ALLOW_SOFTWARE` are on when their value is `1`, `true` or `yes`
(in any case), and off otherwise.

## Runtime

| Variable | Default | Effect |
| --- | --- | --- |
| `FASTWARP_GPU_BACKEND` | `auto` | Graphics API: `vulkan` (or `vk`), `dx12` (or `d3d12`), `metal` or `auto`, in any case. `auto` tries Vulkan on Linux, Vulkan then DirectX 12 on Windows, and Metal on macOS. An unknown value raises {class}`~fastwarp.GpuUnavailableError`. |
| `FASTWARP_GPU_ADAPTER` | unset | Use the adapter whose name contains this text, ignoring case. The `adapter=` argument of a function takes precedence. |
| `FASTWARP_GPU_ALLOW_SOFTWARE` | off | Accept a software adapter (Mesa lavapipe, Microsoft WARP). Slow; meant for tests on machines without a GPU. |
| `FASTWARP_GPU_MEMORY_BUDGET_MB` | unset | The memory budget of the {term}`capacity check`, in mebibytes (MiB, 1024 × 1024 bytes), in place of the figure the graphics API reports; nothing is subtracted from it. A value that is not a whole number is ignored. See [GPU and memory](guide/gpu.md#the-memory-budget). |
| `FASTWARP_GPU_WAIT_TIMEOUT_S` | `300` | Seconds to wait for submitted GPU work before failing with {class}`~fastwarp.RegistrationError`. A value that is not a positive number is ignored. |
| `RAYON_NUM_THREADS` | all logical cores | CPU threads for the work FastWarp does on the CPU (resampling, preparing images, composing fields, {func}`~fastwarp.apply_transforms`). It is read once, when the thread pool starts, which opening the GPU also does; set it before the first FastWarp call. The GPU work is not affected. |
| `WGPU_FORCE_FALLBACK_ADAPTER` | off | Use software adapters only, and accept them. Read by FastWarp in the same way as the switches above. |

## Tests and development

These variables are read by the test suites and benchmarks of the repository, not by the
installed package. See [Testing](testing.md).

| Variable | Effect |
| --- | --- |
| `FASTWARP_GPU_REQUIRE_ADAPTER` | A missing GPU adapter fails the Rust and Python GPU tests instead of skipping them. |
| `FASTWARP_REQUIRE_ANTSPY` | An ANTsPy that does not import fails the Python tests instead of skipping the comparisons with ANTs. |
| `FASTWARP_ANTS_DIR` | Folder with the ANTs 2.6.5 programs for the ignored Rust tests that compare with ANTs. Unset, they skip; a folder without a program fails them. |
| `FASTWARP_ATLAS_INDEX_MAT` | Path to an atlas index `.mat` file for a `fastwarp-matfile` test. Unset skips it; a missing file fails it. |
| `FASTWARP_UIPREFS_DIR` | Folder of real `.mat` files for an ignored `fastwarp-matfile` test. |
| `FASTWARP_REG_AB_DIR` | Folder of a real subject with ANTs outputs, for the ignored A/B tests in `fastwarp-gpu`. |
| `FASTWARP_REG_PROTO_DIR` | Folder of prototype composite transforms for a `fastwarp-reg` test. Unset skips it. |
| `FASTWARP_REG_ANTS_COMPOSITE` | Path of a real ANTs composite `.h5` file for a `fastwarp-reg` test. Unset skips it. |
| `FASTWARP_GPU_BENCH` | Set to any value, enables the ignored GPU benchmarks in `crates/fastwarp-gpu/tests/syn_bench.rs`. |

The Python tests also set `ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS=1` for ITK inside ANTsPy, so that
the comparisons with ANTs are repeatable.
