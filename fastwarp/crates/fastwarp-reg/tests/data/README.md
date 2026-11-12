# Test data

- `uiprefs_contiguous.mat`, `uiprefs_chunked_shuffle.mat`: synthetic MATLAB v7.3 files with a fixed header date (2024-01-01) holding only the variables `elmodel` and `var00` to `var11`, in two HDF5 layouts (contiguous, chunked with the shuffle filter). Copies of the same files live in `crates/fastwarp-matfile/tests/data/`, which has the other two layouts.
- `ctest.h5`, `ms0.mat`, `ms20.mat`, `init0.txt`, `init20.txt`: ITK transform files (HDF5, MATLAB v4, text) made during development; the exact command that wrote them is not recorded. They hold transform parameters only (a few hundred bytes each; `ctest.h5` an ITK `TransformGroup`), no image or subject data.
