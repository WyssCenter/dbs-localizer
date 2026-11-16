# Performance and validation

How FastWarp's speed and accuracy are measured against ANTs, and what the tests check on every
change. **The benchmark results are not published yet**: the benchmark is written and tested on
a synthetic phantom, and its first full run on public data is still to be done.

::::{grid} 1 1 3 3
:gutter: 3

:::{grid-item-card} {fas}`stopwatch` Performance
:link: performance
:link-type: doc

How run times are measured and how to repeat the measurement. Results pending; earlier
measurements on non-public data are labelled as such.
:::

:::{grid-item-card} {fas}`bullseye` Accuracy benchmark
:link: benchmarks
:link-type: doc

FastWarp and ANTs on public brain MRI with manual labels and landmarks. Results pending; anyone
can download the data and run it.
:::

:::{grid-item-card} {fas}`vial` Testing
:link: testing
:link-type: doc

The Rust and Python tests, the comparisons with the ANTs programs, and what CI runs.
:::
::::

## What the tests check today

Until the benchmark results are published, these are the agreements with ANTs that the test
suite enforces. All are on synthetic images, not on brain scans.

| What | Compared with | Test | Required agreement |
| --- | --- | --- | --- |
| Transform files read by ANTs | `antsApplyTransforms` 2.6.5 mapping the same points | `crates/fastwarp-reg/tests/ants_compat.rs` | within 0.001 mm |
| Resampling (`apply_transforms`), every interpolator | ANTsPy | `tests/test_post.py` | within 0.001 on images with values up to about 110 |
| Displacement fields (`compose_field`) | ANTsPy | `tests/test_post.py` | within 0.0001 mm |
| Affine fit (`linear_session`) | five ANTs fits with different random seeds | `tests/test_linear.py` | within 0.5 mm of the median ANTs fit, within 0.5 mm of the true transform, and at most 0.15 mm further from it than the median ANTs fit |
| Masked affine fit | three ANTs fits with the same fixed-image mask | `tests/test_linear.py` | at most 0.25 mm further from the true transform than the median ANTs fit |
| Rigid fit, final metric value | `antsRegistration` 2.6.5 | `crates/fastwarp-reg/tests/coreg_metric.rs` | within 0.1 % (CPU reference engine) |
| SyN on the GPU | FastWarp's CPU reference engine | `crates/fastwarp-gpu/tests/syn_parity.rs` | displacement fields within 5 % (RMS), similarity within 0.002 |
| Full normalization (`register_syn`) | the known shift of a synthetic image | `tests/test_register.py` | translation within 0.3 mm |

The tests against the ANTs programs run in the `full` workflow; see [Testing](testing.md). No
test in CI compares FastWarp's SyN with ANTs' SyN on real images. The ignored A/B tests in
`crates/fastwarp-gpu/tests/ab_real_subject.rs` do, on one non-public subject that only the
developers have (`FASTWARP_REG_AB_DIR`): points marked in the subject's scan, carried through
FastWarp's and through ANTs' transforms, must land less than 2 mm apart on average. The comparison on public data is the purpose of the
[accuracy benchmark](benchmarks.md).

```{toctree}
:hidden:

performance
benchmarks
testing
```
