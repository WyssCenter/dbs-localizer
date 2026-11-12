# dbsloc-reconstruct

Rust reconstruction algorithms, no file or process I/O: TRAC/CORE (`specs/05` §4.2), the
native-frame CT path (rigid frame fit, metal-axis fallback, gates; `specs/05` §4.4–4.5), PaCER
(`specs/10`) and the plausibility checks.

## Build

- `cargo test -p dbsloc-reconstruct`

## Python extension

The `dbsloc_reconstruct` Python module is built from `crates/dbsloc-pipeline`, because the CT
reconstruction it exposes needs the NIfTI I/O and the `antsApplyTransforms` point warp that
live there:

```bash
cd crates/dbsloc-pipeline
maturin develop --release
```
