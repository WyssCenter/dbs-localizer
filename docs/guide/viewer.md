# The 3D viewer

The *3D Viewer* tab shows the leads, their contacts, slices of a background image and the
structures of an atlas, all in template space. After a run it opens the subject's scene
automatically.

## Mouse

Left drag rotates; right drag (or `Ctrl` + left drag) moves; the wheel or middle drag zooms;
double click resets the view. This is the same as in Lead-DBS. In the side panel's *View* section,
*Mouse & touchpad* switches to the *Classic* scheme. Touchpad pinch and two-finger scrolling work
too. The keyboard shortcuts are listed under [Keyboard shortcuts](application.md#keyboard-shortcuts).

## Side panel

- *Slices*: choose the *Backdrop* image (the template, the subject's normalized scans, a
  downloaded high-resolution image, or *Choose file…*), and set *Index* and *Opacity* of the
  axial, coronal and sagittal slices. `1`, `2`, `3` show or hide them.
- *Window / level*: brightness and contrast of the slices.
- *Electrode*: *Show lead bodies*, *Mark and number contacts*.
- *Stimulation*: *Show VTA* draws the volume of activated tissue.
- *Fibres*: one row per fibre layer with a tick box, a colour swatch (click it to change the
  colour of a file you added), *n of N fibres shown* and *Remove*. *Width* sets the line width in
  pixels; *Maximum shown* limits how many fibres each layer draws (200 at first): the fibres with
  the largest absolute value are kept. *Add fibre file…* loads a `.mat` or `.trk` fibre file as
  one more layer. *Show fibres* in the stimulation editor adds a stimulation's connected fibres,
  and *Show in 3D* of a fibre-filtering result adds its fibres, coloured from white to red
  (positive) or blue (negative).
- *Maps*: one row per map (for example a sweet-spot or network-mapping result) with a tick box, a
  mode list and a *Threshold* slider. *Isosurface* draws smoothed red surfaces around the positive
  values and blue ones around the negative values above the threshold; *Slices* colours those
  values on the visible slices instead (only the first map in this mode is drawn). The threshold
  starts at a quarter of the largest absolute value.
- *View*: camera angles and *Reset view* (`R`).
- *Atlas*: *Show atlas overlay*, *Opacity*, the *Atlas set* (DISTAL Minimal by default; AHEAD is
  also included) and *Preset*. `Up` / `Down` step through the structures; `Space` shows or hides
  the selected one.

*Save scene* (`Ctrl+S`) saves the view as a PNG or JPEG image. *Open Viewer in separate window*
(`F11`), in the same row as *Save scene* and *Help*, moves the viewer to its own window, for
example on a second screen. There the same row holds *Full screen* and *Dock to main window*.

To use other Lead-DBS atlases, set the *Lead-DBS atlases directory* on the *Write electrode image*
card to the `atlases` folder of a Lead-DBS installation, or download them
([Atlases, templates and backdrops](atlases-templates.md)).

## From the command line

`dbsloc run` opens the scene after the reconstruction when a display is available (`--no-scene3d`
writes it without opening it). `dbsloc viz scene --manifest <file>` opens a scene file again;
`dbsloc viz scene --help` lists the options.

The viewer is specified in [`specs/08_visualization.md`](../../specs/08_visualization.md) §11.
