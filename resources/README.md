# Wyss DBS Localizer Resources

Bundled dependencies for single-executable distribution.

## ANTs Binaries (v2.6.5)

Essential binaries only (248MB total):
- `antsRegistration`
- `antsApplyTransforms`
- `N4BiasFieldCorrection`

**Platforms**: Linux x86_64, macOS ARM64, Windows x86_64

**Source**: https://github.com/ANTsX/ANTs/releases/tag/v2.6.5

## Usage

The Wyss DBS Localizer pipeline will automatically use the bundled ANTs binaries appropriate for the current platform.

## License

ANTs is licensed under Apache License 2.0. See: https://github.com/ANTsX/ANTs/blob/master/LICENSE

## dcm2niix (v1.0.20260724)

DICOM → NIfTI converter used by the *Import images* wizard (`specs/01` §8.4):
`dcm2niix/{linux,mac,win}/dcm2niix[.exe]` (Windows also carries the MSVC runtime
DLLs from the upstream zip). Resolution order: `DBSLOC_DCM2NIIX_BIN` → this
directory → `PATH` (`dbsloc_pipeline::import::find_dcm2niix`).

**Source**: https://github.com/rordenlab/dcm2niix/releases/tag/v1.0.20260724
(`dcm2niix_lnx.zip`, `dcm2niix_mac.zip` universal x86_64 + arm64, `dcm2niix_win.zip`).

**License**: BSD-style 2-clause, `dcm2niix/LICENSE` (see `NOTICE`).

## AHEAD Atlas

The default subcortical atlas is AHEAD `structures_mni09b` (CC BY 4.0), shipped
under `resources/atlases/ahead/structures_mni09b/` and copied into the `dbsloc`
wheel as `dbsloc/atlas_data/ahead/structures_mni09b/`.

To refresh or reproduce from upstream:

```bash
python3 scripts/download_ahead_atlas.py
```

See `docs/ATLAS_AHEAD.md`, `resources/atlases/ahead/manifest.json`, and `NOTICE`.
