# AHEAD atlas (Alkemade et al. 2020) — Wyss DBS Localizer distribution policy

**Reference (cite in publications):**  
Alkemade, J. M., Mulder, M. J., Groot, J. M., Isaacs, B. R., van Berendonk, N., Lute, N., et al. (2020). The Amsterdam Ultra-high field adult lifespan database (AHEAD): A freely available multimodal 7 Tesla submillimeter magnetic resonance imaging database. *NeuroImage*.

**Dataset:** <https://doi.org/10.21942/uva.10007840.v2>  
**Atlas file:** `structures_mni09b.tar.gz` (`figshare` file id `21209229`)  
**Expected MD5:** `024d96847765af480dfd69612011684c`  
**License:** Creative Commons Attribution 4.0 International (CC BY 4.0), <https://creativecommons.org/licenses/by/4.0/>

## Redistribution

AHEAD `structures_mni09b.tar.gz` is listed by the Figshare API under **CC BY 4.0**. This permits redistribution, including commercial redistribution, provided attribution is preserved and changes are indicated.

Before committing or bundling the atlas files:

1. Keep this file and the `NOTICE` entry with the distribution.
2. Preserve the source URL, DOI, file id, checksum, and license.
3. Cite Alkemade et al. (2020) in documentation and publications that use the atlas.
4. If files are converted, meshed, downsampled, or thresholded, record that as a derived form.

## Wyss DBS Localizer layout

The repository and the release archives ship the extracted structure masks here:

```text
resources/atlases/ahead/structures_mni09b/
```

The pipeline finds them through the `resources/` root of the install (`bundled_bin::find_resources_root`, used by `ahead_atlas_path` in `crates/dbsloc-pipeline/src/stage_impl.rs`). There is no Python wheel copy (the wheel layout and `dbsloc/atlas_paths.py` were removed).

The scene viewer accepts this as a flat NIfTI atlas directory. The default scene manifest includes it when the directory exists. Users may override atlas paths with `DBSLOC_SCENE_ATLAS_LEAD_DIR`, `DBSLOC_SCENE_ATLAS_OBJ_DIR`, or `DBSLOC_SCENE_ATLAS_NII`.

## Refresh from upstream

```bash
python3 scripts/download_ahead_atlas.py
```

This downloads `structures_mni09b.tar.gz`, verifies its MD5 checksum, and extracts mask NIfTIs into the resource directory. Use `--archive <path>` to install from a local archive.
