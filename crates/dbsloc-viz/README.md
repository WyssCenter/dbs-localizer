# dbsloc-viz

Cross-platform **GPU** visualization (spec **08**).

## Stack (normative)

| Layer | Role |
|-------|------|
| **[wgpu](https://github.com/gfx-rs/wgpu)** (WebGPU) | **Default** raster path: **Vulkan** (Linux, Windows), **Metal** (macOS), **D3D12** (Windows). One codebase, one artifact family (PNG / future `glb`). |
| **[bytemuck](https://docs.rs/bytemuck)** | Uniform / vertex POD for the scene viewer bind groups. |
| **Python `dbsloc.visualization`** | Development helpers (2D slice extraction, grayscale PNG writer); the matplotlib montages were retired 2026-10-12. |

**Headless / CI:** set `WGPU_FORCE_FALLBACK_ADAPTER=1` so machines without a discrete GPU can use a software adapter when the OS provides one.

## Rust API

| Symbol | Feature | Description |
|--------|---------|-------------|
| [`gencheckregpair_png`](../dbsloc-checkreg/src/checkreg_pair.rs) | `checkreg` | Re-export from `dbsloc-checkreg`: CPU checkreg pair PNG (moving grey + fixed edges, axial montage; `specs/03` §12.2). |
| [`checkreg_app::CheckregReviewPanel`](src/checkreg_app.rs) | `checkreg-embed` | Interactive registration review panel (window: `dbs-localizer checkreg`, `checkreg_cli`). |
| [`Scene3DEframeApp`](src/scene3d_gpu.rs) | `scene-viewer` | Interactive 3D scene: template slices, atlas meshes, lead bodies (window: `dbs-localizer scene`, `scene_cli`). |
| [`refine_app::RefinePanel`](src/refine_app.rs) | `refine-embed` | Manual correction of lead markers and roll (`specs/08` §11.1.6). |
| [`reco_json`](src/reco_json.rs) | `data-mni` | Reco v2 JSON side geometry (path and byte loaders; a missing side is `None`). |
| [`scene_volume::TemplateVolume`](src/scene_volume.rs) | `scene-viewer` / `web` | Template NIfTI volume (path and byte loaders). |

Every NIfTI read goes through `nifti_io`: the header is read and its dimensions checked (at most 2048 per axis, 2^28 voxels) before the volume is allocated, then the voxels are decoded in bulk.

Shader: [`src/shaders/scene3d.wgsl`](src/shaders/scene3d.wgsl).

## Build

```bash
cargo build -p dbsloc-viz
cargo test -p dbsloc-viz
# Without GPU (if default fails):
WGPU_FORCE_FALLBACK_ADAPTER=1 cargo test -p dbsloc-viz
cargo clippy -p dbsloc-viz -- -D warnings
```

## Features

| Feature | Default | Meaning |
|---------|---------|----------|
| `wgpu-renderer` | **on** | wgpu, bytemuck. |
| `data-mni` | **on** (with wgpu) | `nifti` + `serde`/`serde_json` — NIfTI and Reco v2 loaders. |
| `checkreg` | **on** | CPU checkreg PNG. |
| `checkreg-embed` / `checkreg-viewer` | off | Interactive review panel / standalone binary. |
| `scene-viewer` | off | Interactive 3D scene and its binary. |
| `refine-embed` | off | Manual correction view (library only). |
| `web` | off | Byte loaders for the browser client (`dbsloc-web`). |

`cargo test -p dbsloc-viz --no-default-features` compiles a stub (no wgpu) — for CI that cannot link GPU stacks (only `thiserror` + stub).
