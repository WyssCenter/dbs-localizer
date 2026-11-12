# Test data

- `uiprefs_contiguous.mat`, `uiprefs_compact.mat`, `uiprefs_chunked.mat`, `uiprefs_chunked_shuffle.mat`: synthetic MATLAB v7.3 files with a fixed header date (2024-01-01) holding only the variables `elmodel` and `var00` to `var11`. They are the same content stored in four HDF5 layouts: contiguous, compact, chunked, and chunked with the shuffle filter.
- `ctest.h5`: an ITK transform file (HDF5, an ITK `TransformGroup`) made during development; the exact command that wrote it is not recorded. It holds transform parameters only, no image or subject data.
