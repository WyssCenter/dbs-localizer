# Reference

Look-up pages: the command line, where results are stored, every run option and environment
variable, and the terms the documentation uses.

::::{grid} 1 2 2 2
:gutter: 3

:::{grid-item-card} {fas}`terminal` Command line
:link: cli
:link-type: doc

Every `dbsloc` command, the options of `dbsloc run`, exit codes and environment variables.
:::

:::{grid-item-card} {fas}`folder-tree` Outputs and file layout
:link: outputs
:link-type: doc

The results folder of a subject, and the files kept outside the dataset.
:::

:::{grid-item-card} {fas}`sliders` Run reference
:link: RUN_REFERENCE
:link-type: doc

All run options, environment variables, viewer modes and their precedence, in detail.
:::

:::{grid-item-card} {fas}`spell-check` Glossary
:link: glossary
:link-type: doc

Anchor, approval, normalization, VTA, connectome and the other terms.
:::
::::

The behaviour of every step is written down in the
[specifications](https://github.com/WyssCenter/dbs-localizer/tree/main/specs), starting with
[`specs/00_overview.md`](https://github.com/WyssCenter/dbs-localizer/blob/main/specs/00_overview.md).

```{toctree}
:hidden:

cli
outputs
RUN_REFERENCE
glossary
```
