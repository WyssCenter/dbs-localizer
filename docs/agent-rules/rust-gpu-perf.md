# Rust GPU performance rules (dbsloc-viz)

**Applies to:** `crates/dbsloc-viz/**`

## Buffer lifecycle

Never call `create_buffer_init` or `create_buffer` inside a per-frame or per-geometry-rebuild path. Allocate once at startup with a generous capacity; grow via a new `create_buffer` only when the data exceeds the current capacity, then swap.

Use `queue.write_buffer` to update existing buffers in place.

## Draw calls

Always prefer `render_pass.draw_indexed` over `draw` for indexed meshes. Never expand an indexed `TriMeshScene` into a flat vertex stream; upload positions + normals + a separate index buffer and call `draw_indexed`. Flat expansion wastes 4-6x GPU memory for a typical welded atlas mesh.

## Frame latency

Every `eframe::NativeOptions` for an interactive viewer must set:

```rust
wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
    desired_maximum_frame_latency: Some(1),
    ..Default::default()
},
```

Default (3) adds ~33 ms perceived input lag on 60 Hz monitors.

## Volume uploads

Before resampling a binary ROI mask to template space, compute the ROI bounding box in source voxel space and crop the output grid to that box (plus a small pad). Processing the full 394×466×378 template grid for a small subcortical structure wastes ~99% of work.

## Cache files

Compressed `.obj.gz` or `.obj.lz4` reduces `precomputed_obj/` from hundreds of MB to ~20-60 MB for a full atlas. Prefer zstd or gzip via the `flate2` crate.
