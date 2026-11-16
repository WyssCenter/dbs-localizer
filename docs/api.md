# API reference

Everything below except the section [ANTsPy compatibility](#antspy-compatibility) is imported from
the package itself, `import fastwarp`, and listed in `fastwarp.__all__`; that section is the
module `fastwarp.ants`. Modules whose names start with an underscore are private.

An *image* is an `ANTsImage` or a tuple `(array, spacing, origin, direction)` (see
[Images](guide/images.md)); a *reference grid* is an `ANTsImage` or a tuple
`(shape, spacing, origin, direction)`. Lengths are in millimetres and coordinates in
{term}`LPS`.

## Registration

Full normalization and linear fits on the GPU. Guides: [Normalization](guide/normalization.md),
[Linear fits](guide/linear.md).

```{eval-rst}
.. autoapifunction:: fastwarp.register_syn
```

```{eval-rst}
.. autoapidata:: fastwarp.OUTPUT_LAYOUTS
```

```{eval-rst}
.. autoapifunction:: fastwarp.linear_session
```

```{eval-rst}
.. autoapiclass:: fastwarp.LinearSession
   :members:
```

```{eval-rst}
.. autoapiclass:: fastwarp.CancelToken
   :members:
```

## Recipe

The registration schedule. Guide: [Recipes](guide/recipes.md).

```{eval-rst}
.. autoapifunction:: fastwarp.cohort_recipe
```

## Resampling

Applying ANTs transform files on the CPU. Guide: [Resampling](guide/resampling.md).

```{eval-rst}
.. autoapifunction:: fastwarp.apply_transforms
```

```{eval-rst}
.. autoapifunction:: fastwarp.compose_field
```

```{eval-rst}
.. autoapidata:: fastwarp.INTERPOLATORS
```

## ANTsPy compatibility

The module `fastwarp.ants`: ANTsPy's `ants.registration` and `ants.apply_transforms`. Guide:
[ANTsPy compatibility](guide/ants.md).

```{eval-rst}
.. autoapifunction:: fastwarp.ants.registration
```

```{eval-rst}
.. autoapifunction:: fastwarp.ants.apply_transforms
```

```{eval-rst}
.. autoapiexception:: fastwarp.ants.UnsupportedArgument
```

## GPU checks

Guide: [GPU and memory](guide/gpu.md).

```{eval-rst}
.. autoapifunction:: fastwarp.gpu_available
```

```{eval-rst}
.. autoapifunction:: fastwarp.gpu_info
```

```{eval-rst}
.. autoapifunction:: fastwarp.fits_gpu
```

```{eval-rst}
.. autoapifunction:: fastwarp.iso_grid_dims
```

## Exceptions

When each one is raised: [Errors](errors.md).

```{eval-rst}
.. autoapiexception:: fastwarp.GpuUnavailableError
```

```{eval-rst}
.. autoapiexception:: fastwarp.GpuCapacityError
```

```{eval-rst}
.. autoapiexception:: fastwarp.RegistrationError
```

```{eval-rst}
.. autoapiexception:: fastwarp.RegistrationCancelled
```

## Package information

```{eval-rst}
.. py:data:: fastwarp.__version__
   :type: str

   The installed version of FastWarp, for example ``"0.1.0"``, read from the package metadata.
   It is ``"0+unknown"`` when FastWarp runs from a source tree that was not installed.
```

```{eval-rst}
.. autoapidata:: fastwarp.CITATION
```

```{eval-rst}
.. autoapidata:: fastwarp.ENGINE_SOURCE
```
