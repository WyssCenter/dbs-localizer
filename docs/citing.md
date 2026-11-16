# Citing

```{include} ../README.md
:start-after: <!-- docs:citing -->
:end-before: <!-- /docs:citing -->
```

The [methods text](guide/methods-text.md) of a subject names, for each step that ran, the method
and its reference.

## References of the methods

The specifications list these references for the methods the steps use:

- **Bias-field correction (N4):** Tustison NJ, Avants BB, Cook PA, et al. (2010) N4ITK: Improved
  N3 bias correction. *IEEE Trans Med Imaging* 29(6):1310–1320.
  [doi:10.1109/TMI.2010.2046908](https://doi.org/10.1109/TMI.2010.2046908)
- **GPU registration engine (FastWarp):** Köhler RM, Payonk JP. FastWarp: the GPU
  registration engine of Wyss DBS Localizer. See [Citing FastWarp](https://fastwarp.readthedocs.io/latest/citing.html).
- **The ANTs programs:** Avants BB, Tustison NJ, Song G, Cook PA, Klein A, Gee JC (2011) A
  reproducible evaluation of ANTs similarity metric performance in brain image registration.
  *NeuroImage* 54(3):2033–2044.
  [doi:10.1016/j.neuroimage.2010.09.025](https://doi.org/10.1016/j.neuroimage.2010.09.025)
- **Normalization (SyN):** Avants BB, Epstein CL, Grossman M, Gee JC (2008) Symmetric
  diffeomorphic image registration with cross-correlation. *Med Image Anal* 12(1):26–41.
  [doi:10.1016/j.media.2007.06.004](https://doi.org/10.1016/j.media.2007.06.004)
- **Reconstruction (TRAC/CORE) and manual correction:** Horn A, Kühn AA (2015) Lead-DBS: A toolbox
  for deep brain stimulation electrode localizations and visualizations. *NeuroImage*
  107:127–135. [doi:10.1016/j.neuroimage.2014.12.002](https://doi.org/10.1016/j.neuroimage.2014.12.002)
- **PaCER:** Husch A, Petersen MV, Gemmar P, Goncalves J, Hertel F (2017) PaCER — A fully
  automated method for electrode trajectory and contact reconstruction in deep brain stimulation.
  *NeuroImage Clin* 17:80–89.
  [doi:10.1016/j.nicl.2017.10.004](https://doi.org/10.1016/j.nicl.2017.10.004)
- **Orientation of directional leads (DiODe):** Hellerbach A, et al. (2018) DiODe: Directional
  Orientation Detection of Segmented DBS Leads: A Sequential Algorithm Based on CT Imaging.
  *Stereotact Funct Neurosurg* 96(5):335–341.
  [doi:10.1159/000494738](https://doi.org/10.1159/000494738); Dembek TA, et al. (2021) DiODe v2:
  Unambiguous and Fully-Automated Detection of Directional DBS Lead Orientation. *Brain Sci*
  11(11):1450. [doi:10.3390/brainsci11111450](https://doi.org/10.3390/brainsci11111450)
- **Volume of activated tissue:** Horn A, Reich M, Vorwerk J, et al. (2017) Connectivity predicts
  deep brain stimulation outcome in Parkinson disease. *Ann Neurol* 82(1):67–78.
  [doi:10.1002/ana.24974](https://doi.org/10.1002/ana.24974) (the electric-field model); Kuncel
  AM, Cooper SE, Grill WM (2008), Mädler B, Coenen VA (2012) and Dembek TA, Barbe MT, Åström M,
  et al. (2017) for the sphere models of the same names.

The full reference lists are in the specifications:
[`specs/02_preprocessing.md`](../specs/02_preprocessing.md) §11,
[`specs/03_coregistration.md`](../specs/03_coregistration.md) §11,
[`specs/04_normalization.md`](../specs/04_normalization.md) §17,
[`specs/05_reconstruction.md`](../specs/05_reconstruction.md) §12,
[`specs/06_orientation.md`](../specs/06_orientation.md) §4,
[`specs/07_vat.md`](../specs/07_vat.md) §11 and [`specs/10_pacer.md`](../specs/10_pacer.md) §12.

## Atlases, templates and connectomes

Cite the atlas, template, backdrop and connectome you used. The application shows the citation
where you choose them, and [`NOTICE`](../NOTICE) lists those of the bundled data, for example the
DISTAL atlas (Ewert et al. 2017) and the AHEAD atlas (Alkemade et al. 2020).
