# Quickstart

This page takes one subject from scans to reconstructed leads. You need a pre-operative MRI
(T1-weighted, and T2-weighted if you have one) and a post-operative CT or MRI of the same
patient, as DICOM folders or NIfTI files. The [basic ideas](guide/concepts.md) explain the words
used here.

## First run

```{include} ../README.md
:start-after: <!-- docs:start -->
:end-before: <!-- /docs:start -->
```

## Check the result

- Look at every registration in the *Registration review* tab before you continue
  ([Checking registrations](guide/review.md)).
- Look at the leads in the *3D Viewer* and in *Refine localization*, and correct a lead that is
  in the wrong place ([Checking and correcting the electrodes](guide/correction.md)).
- *Methods text* in the *Pipeline* tab shows a paragraph for each step that ran, ready for the
  methods section of a paper ([Methods text](guide/methods-text.md)).
- The results are in `derivatives/leaddbs/sub-<id>/` ([Outputs and file layout](outputs.md)).

To check the accuracy on public data with expert reconstructions, run the
[Lead-Tutor self-check](guide/lead-tutor.md).
