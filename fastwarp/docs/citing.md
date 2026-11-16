# Citing FastWarp

If you use FastWarp in your work, please cite the software. The citation data are in
[`CITATION.cff`](https://github.com/WyssCenter/fastwarp/blob/main/CITATION.cff) in the
repository; GitHub shows them under *Cite this repository* on the repository page.

```text
Köhler RM, Payonk JP, Neumann WJ. FastWarp: the GPU registration engine of Wyss DBS Localizer.
https://github.com/WyssCenter/fastwarp; 2026.
```

The same text is available in Python as {data}`fastwarp.CITATION`, for method logs:

```python
import fastwarp

print(fastwarp.CITATION, f"Version {fastwarp.__version__}.")
```

As BibTeX, with the version you used:

```bibtex
@software{fastwarp,
  author  = {K{\"o}hler, Richard M. and Payonk, Jan Philipp and Neumann, Wolf-Julian},
  title   = {{FastWarp}: the {GPU} registration engine of {Wyss DBS Localizer}},
  year    = {2026},
  version = {0.2.0},
  url     = {https://github.com/WyssCenter/fastwarp},
  license = {Apache-2.0}
}
```

## The method

FastWarp runs the SyN algorithm of ANTs. Please also cite the method:

> Avants BB, Epstein CL, Grossman M, Gee JC (2008). Symmetric diffeomorphic image registration
> with cross-correlation: evaluating automated labeling of elderly and neurodegenerative brain.
> *Medical Image Analysis* 12(1):26–41.
> [doi:10.1016/j.media.2007.06.004](https://doi.org/10.1016/j.media.2007.06.004)

The datasets of the [accuracy benchmark](benchmarks.md) have their own citations, listed with
each dataset.
