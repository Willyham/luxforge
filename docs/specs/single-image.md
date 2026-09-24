# Crop, export and conflicts

Status: the M4 crop module is implemented; export, Locate and MCP remain editor follow-ups. History behavior comes from [layers and history](edit-history.md).

## Viewport and coordinates

Fit, an editable zoom percentage, 100% and pan are session operations available through UI and API. 100% means one source pixel to one physical framebuffer pixel, never a logical UI pixel. A magnified Fit texture may appear while source detail loads only with a clear loading state. Image-edit coordinates are independent of zoom and DPI: each layer acts in its input stage, and a later transform never reinterprets an earlier edit. A pick on the canvas is mapped from the rendered pixel back through the geometry tail to the content pixel it shows, the same mapping `render.locate` answers; through a straightened crop that is the content pixel whose area contains the sampled position. For crop UI, map the canvas through the view transform into the crop layer's input space. Space-drag pans; ordinary drag inside the crop moves the composition.

## Geometry contract

EXIF orientation is applied once before layer evaluation. Quarter-turns and reflections are exact integer mappings held in an orientation layer ([history spec](edit-history.md#exact-transforms)). The crop layer (effect `lightwell.geometry.crop`, format 1, geometry stage, order 10) combines fine straightening and crop with explicit input and output geometry. Its order is later than the orientation's, so the orientation layer is always ahead of it and the crop's input stage includes every transform and every content, colour and spatial edit; only finish layers, which act on the crop's output, follow it. A transform made after a crop is applied ahead of it and carries it, as the next paragraph and the [history spec](edit-history.md#exact-transforms) describe. A stack holds at most one crop layer; editing it keeps its identity. Pixel-stage layers are always before the crop, so a crop change never moves or invalidates them.

A transform carries the crop by mapping its whole-pixel output rectangle through the turn or reflection in box space: the rotated box turns and reflects with its input, a quarter turn keeps `θ` and swaps the box, and a reflection negates `θ`. At `θ = 0` the carry is exact and the new output is byte-identical to the old one turned or reflected. At any other angle the box extents are not whole pixels, so the extents are kept and the origin moves to the nearest whole box pixel at which they are covered, at most half a box pixel from the exact position; a rectangle touching the rotated source on opposite sides is fitted instead, trimming at most one pixel on an axis. The output is then the old one turned up to that sub-pixel resample.

### Rotated box and payload

Let the layer's input stage be `W × H` pixels with a top-left origin, x right and y down, and let `θ` be the angle in degrees, finite and within −45 to +45. Positive `θ` turns the image clockwise on screen. The rotated box is the axis-aligned bounding box of the input rotated about its center:

```text
BW = W·|cos θ| + H·|sin θ|      BH = W·|sin θ| + H·|cos θ|
input → box:  x = cos θ·(u − W/2) − sin θ·(v − H/2) + BW/2
              y = sin θ·(u − W/2) + cos θ·(v − H/2) + BH/2
box → input:  u = cos θ·(x − BW/2) + sin θ·(y − BH/2) + W/2
              v = −sin θ·(x − BW/2) + cos θ·(y − BH/2) + H/2
```

The persisted payload is `{"angle": θ, "x", "y", "width", "height"}` with the rectangle normalized to the box: `x`, `y` in 0..1 from the box's top-left corner, `width`, `height` in 0..1, all finite, extents positive, `x + width ≤ 1` and `y + height ≤ 1`. Normalization keeps the payload independent of how the box was computed; the layer still acts on one fixed input stage.

### Output rounding and validity

The output stage is whole pixels: `ox = round(x·BW)`, `oy = round(y·BH)`, `ow = max(1, round(width·BW))`, `oh = max(1, round(height·BH))`, rounding half away from zero. A payload is valid only when the four corners of that rounded rectangle, `(ox, oy)`, `(ox + ow, oy)`, `(ox, oy + oh)` and `(ox + ow, oy + oh)`, map through box → input to points inside `[0, W] × [0, H]` with a tolerance of 0.001 source pixels. Because the source is convex, covered corners mean every output pixel samples real source content: there are no empty corners, ever, and the module rejects any payload that would need them with a validation error naming the offending corner. The desktop and the fitting functions only ever produce whole-pixel box rectangles, so rounding never changes what was validated. Rendering the same payload at a stage other than the one it was fitted on, which a display-bounded proxy does, re-rounds that rectangle at another scale; a corner that then maps less than one source pixel outside the stage is shrunk back in by whole pixels on that side rather than refused, and a miss of a pixel or more still fails with the same error.

### Sampling

Output pixel `(i, j)` has its center at box `(ox + i + 0.5, oy + j + 0.5)`. That center maps to input `(u, v)` and samples the input raster at index coordinates `(u − 0.5, v − 0.5)`: pixel-center convention, bilinear interpolation of the four surrounding pixels with indices clamped to the raster edge. Interpolation weights are applied in linear light: each 8-bit sRGB channel is decoded with the sRGB transfer function, blended, encoded back and rounded to the nearest 8-bit value; alpha is blended linearly. The reference is an f64 evaluation of the same formula; the production path must match it within one 8-bit step per channel.

When `θ = 0` the box equals the input, the mapping is an integer translation, and the crop compiles to the host's exact geometry primitive: an exact copy of the source rectangle with no interpolation, composable with neighbouring exact transforms into one raster pass. Any other angle compiles to the host's `Resample` primitive, which is a stage boundary: exact layers before it render into a bounded intermediate frame, the resample writes the output frame, and exact layers after it compose as before. At most two full frames exist at once and each is subject to the 512 MiB frame limit. Point queries evaluate through the resample recursively and never allocate a frame.

### Fitting

Fitting keeps the composition and trims only what an empty corner would require. Given a box rectangle with center `c` and half extents `(hw, hh)`, and the rotated source as a convex polygon in box space:

1. If `c` lies outside the polygon, move it to the nearest point inside; otherwise keep it.
2. Find the largest scale `s ≥ 0` such that every corner `c + s·(±hw, ±hh)` lies inside the polygon. Each corner and each polygon edge gives one linear bound, so `s` is exact.
3. Scale the rectangle about `c` by `min(1, s)` and snap it inward to whole box pixels.

Angle changes are always evaluated against the last frame gesture: the draft keeps the rectangle and angle that the last handle, move or ratio change produced, and every later angle value refits that reference at the new angle. Sweeping the angle away and back therefore returns the exact reference rectangle with no cumulative trim. Moving the rectangle clamps the horizontal delta and then the vertical delta against the polygon so it slides along a boundary instead of stopping. Locked-ratio and Option scaling use the same corner bounds with the anchor fixed. Golden cases cover off-center, near-boundary, portrait and landscape inputs and ±45°.

## Crop module

Ratios: Free, Original, 1:1, 3:2, 4:3, 16:9 and custom, with a locked ratio able to swap orientation. Original means the crop layer's input stage ratio, so it already reflects preceding quarter-turns. Angle from −45° to +45°, a drag-to-straighten guide, Apply, Cancel and reset; quarter-turn controls handle larger rotation. A quarter-turn made after a crop carries the visible crop and swaps its ratio orientation, and a reflection carries an off-center composition with the image and reverses the angle, because the transform goes ahead of the crop and the crop is re-expressed through it.

The module declares three actions. `crop` takes `angle`, `x`, `y`, `width` and `height` exactly as persisted. `crop-fit` takes `aspect` (`free`, `original`, `1:1`, `3:2`, `4:3`, `16:9` or `custom` with `aspect-width` and `aspect-height`), `angle` and an optional normalized `center-x`/`center-y`, and commits the largest covered rectangle with that ratio about that center (default: the existing crop's center, else the box center); `free` keeps the existing crop's ratio or the input ratio. `crop-reset` returns an existing crop layer to the neutral payload `angle 0, x 0, y 0, width 1, height 1` and is a no-op without one. All three update the existing crop layer in place, keeping its identity, or append one when none exists; a neutral crop appended to a stack without one is a no-op, and so is a request equal to the saved payload. Updating a crop earlier in the stack changes the stage of every later layer, so the host compiles the whole new stack before persisting and rejects a change that would push a later layer out of its stage; pixel-stage layers precede the crop, so this applies to later geometry only.

Free edge and corner handles, crop movement and a thirds overlay. In Free mode a side moves independently and a corner changes width and height. Holding Option (Alt on Windows/Linux) applies one scale factor about the fixed center, preserving the current ratio even in Free mode, clamped at the first source boundary without shifting the center or stretching an axis. Choosing a ratio keeps the center and fits the largest rectangle of that ratio inside the current one; swap uses the inverse ratio the same way. The guide takes a dragged line and rotates by the angle that makes it horizontal or vertical, whichever is nearer, within ±45°.

While drafting, the canvas shows the crop layer's input stage rotated by the draft angle, dimmed outside the rectangle, with the frame, thirds and handles drawn in box space and mapped through the view transform; 100% keeps one input pixel per physical pixel. The preview rotation is the GPU's display filter, not the reference sampler; the committed render is. An input stage wider or taller than one texture-atlas layer (2048 px) is uploaded as tiles of at most that size, each drawn turned with the whole stage, so the draft is one picture at every angle. The input stage is the stack up to the crop layer, or up to where a new crop would go: every transform and every pixel, colour and spatial edit is shown under the frame, and only finish layers such as the post-crop vignette, which act on the crop's output, are not. An orientation layer a stored stack holds after its crop is not shown either, until a transform folds it ahead of the crop. Space-drag pans, ordinary drag inside the frame moves the composition.

A draft is transient. A draft on an existing crop layer opens at exactly its committed rectangle and angle; the payload records no ratio, so the draft chooses and locks the first declared ratio the rectangle has (Original, then each `W:H` in either orientation, the long side within 2r pixels of the short side times r) and is Free otherwise or on a neutral payload. The panel's ratio and angle controls read that same seeded state while no draft is open, and changing one opens the draft with the change; nothing commits before Apply. Apply or Enter commits one semantic action and one new snapshot; Cancel or Escape discards. A commit whose render fails keeps its entry and shows the failure in place of the photograph: the picture from before the commit is never left on screen under it, and a zoom does not hand it back. Adjusting an existing crop layer keeps its ID. Reset commits `crop-reset` through the history service and ends the draft; whole-recipe reset is a separate explicit action. No pointer event commits history or resamples a saved image. Every parameter and action is in the module's API; gesture simulation is unnecessary.

## Conflicts

An agent commit during a human draft keeps the draft and marks it conflicted, offering Discard or explicit Reapply against the latest revision followed by revalidation. When the commit changed the orientation ahead of the crop, Reapply first carries the draft through that change exactly as a transform carries a committed crop, and a locked ratio turns with a quarter turn, so the frame keeps selecting the same content. Stale revisions never overwrite silently. Restore, undo and redo do not discard drafts implicitly. During history preview, changes require Return to current or Restore first. Current-state notifications do not retarget the selected historical entry.

## Export (follow-up)

Export evaluates the committed snapshot and writes a new JPEG at source-scale crop dimensions, quality 90 by default, freezing the revision and metadata option so later changes cannot affect an in-flight job. No resize presets, watermarks or batch export. Output is sRGB with a valid embedded profile, normalized orientation, regenerated dimensions and no obsolete thumbnail. Keep metadata starts off, omitting optional EXIF, IPTC and XMP capture, camera, creator, copyright and GPS fields; when on, the explicitly supported valid fields are retained with corrected structural information. The reader and writer, exact field policy and tolerances must be proven; private manufacturer metadata round-tripping is not promised.

A native destination picker suggests the source stem plus `-edited.jpg`. Reject every existing file and source alias, including symlinks and hardlinks. Write and flush a temporary file in the destination directory and publish without replacement; failure or cancellation removes only temporary output. Inspect output pixels, tags, profile, orientation and dimensions independently in both metadata modes.

Manual Locate and the MCP adapter are specified in [source recovery](source-recovery.md) and [architecture](../design/architecture.md#agent-contract); MCP adds no separate feature logic.

## Acceptance

1. Exercise pixel and transform history through the current module handlers before crop testing.
2. Exercise every handle, movement, ratio and angle control and Option scaling at Fit, numeric zoom and 100% on supported display scales, with distinct pan and crop gestures.
3. Sweep angle away and back, use off-center subjects and near-boundary crops, and compose with pixel and transform effects before and after. Check coverage, output size, sampling tolerance and source detail.
4. Apply, Cancel, reset, undo, redo, preview, restore, edit again and reopen: exactly one action per commit and every snapshot retained.
5. Live UI and API clients with stale revisions, draft conflicts, restore and preview during reconnect, malformed input and failed writes; compare complete stacks and decoded output.
6. Native M4 rendered content correlated with state, logs, entry IDs, revisions and render generation, plus queue, memory and latency measurements and unchanged original hashes.
7. Follow-ups add verified Locate, both export metadata modes, MCP interoperability and complete package acceptance.

RAW, PNG, tonal controls, a multi-image library and externally loaded modules remain later scope. A Lightroom-style interaction reference does not imply Adobe rendering compatibility.
