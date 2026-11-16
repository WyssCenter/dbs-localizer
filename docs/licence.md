# Licence and third-party notices

FastWarp is licensed under the Apache License 2.0, reproduced below. Copyright 2026 Wyss Center
for Bio and Neuroengineering.

Parts of FastWarp are ports of [ITK](https://itk.org) and [ANTs](https://github.com/ANTsX/ANTs),
which are licensed under the Apache License 2.0 as well. Each ported file keeps the upstream
copyright line and says what was changed; [Third-party ports](THIRD_PARTY_PORTS.md) lists every
such file with its upstream source and version. The sample-point generator derives from
Mersenne Twister code under the BSD-3-Clause licence. The upstream notices are in the `NOTICE`
file below, which is shipped in every wheel.

Every file in the repository states its licence, in an SPDX header or through `REUSE.toml`,
following the [REUSE](https://reuse.software/) specification. The Code of Conduct is adapted from
the Contributor Covenant and licensed under CC BY 4.0.

The figure on the home page shows the MNI ICBM 2009c template (Copyright 1993–2004 Louis
Collins, McConnell Brain Imaging Centre, Montreal Neurological Institute, McGill University,
distributed under its own permissive notice, `LICENSES/LicenseRef-MNI-ICBM2009.txt`) and a scan
of the SNSX dataset (OpenNeuro ds004470, CC0 1.0). Neither image is part of the Python package.

## LICENSE

```{literalinclude} ../LICENSE
:language: text
```

## NOTICE

```{literalinclude} ../NOTICE
:language: text
```
