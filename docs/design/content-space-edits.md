# Content-space edits: pixel tools before the geometry tail

Status: implemented and verified on the M4 Mac. The contracts are in the [history spec](../specs/edit-history.md), the [crop spec](../specs/single-image.md) and the [module contract](modules-and-api.md); this document records the reasoning.

## Problem

Today a new layer is always appended to the end of the stack, and a layer's coordinates address its input stage. A pixel edit made after a crop is therefore stored in the crop's output coordinates. Moving the crop moves the edit, and shrinking the crop can be rejected because it would push the edit out of its stage. That is tolerable for the pixel proof tool and unacceptable for brushes, heal or stamp tools, whose strokes must stay anchored to the photograph while the crop is adjusted freely.

## Decision

Pixel-stage edits address one fixed **content stage**: the source after EXIF orientation. Quarter-turns, reflections and the crop form the **geometry tail** at the end of the stack. The host inserts a pixel-stage layer before the first geometry-stage layer instead of appending it, so every pixel edit is carried by the geometry after it and no geometry change reinterprets it. This is the Lightroom model: local edits live in the uncropped, oriented image; crop and orientation are applied last.

The invariant that a layer's coordinates are its input stage is unchanged. Placing the pixel layer before the tail is what makes "the canvas coordinate system stays the same regardless of crop" true: a pick in the top-left of a cropped view resolves to the content pixel that is drawn there, for example (100, 100), and that is the coordinate the layer stores. Storing content coordinates on a layer positioned after the crop is rejected as a design: it would give one layer kind a second coordinate convention and force the renderer to unmap through the crop for that kind only.

### Ordering rule

- The host, not the module, chooses a new layer's position from the effect's declared stage. A `pixel` effect inserts immediately before the first `geometry` layer, or appends when the stack has no geometry layer. A `geometry` effect appends, as today. `Update` keeps the layer's identity and position, as today.
- Ordering within the tail is unchanged: transforms and the crop keep their sequence, a stack holds at most one crop layer, and the crop module edits it in place.
- Recipe validation does not reject stacks with a pixel layer after a geometry layer. Such a stack still renders exactly as it always did, because the renderer's meaning of a layer's coordinates has not changed; only where the host places new layers has. The current shapes rule needs no format marker change because no stored shape changes.
- The crop conflict rule (a crop change that would push a later layer out of its stage is rejected) stays in force. With pixel layers before the crop it no longer triggers for them.

### Planning against the content stage

`plan` for a pixel-stage action validates coordinates against the stage at the insertion index, not the output stage, and its no-op check samples that stage. `StageContext` gains the insertion index and a sampler over the stack prefix before it; `stage_before(i)` already answers the stage. The point sampler is the existing per-segment evaluation, so planning still allocates no frame.

### Mapping a view point to content coordinates

The desktop's point pick currently reports output-stage coordinates of the rendered raster. It must instead report content coordinates: map the output pixel back through the geometry tail. Exact geometry unmaps by integer inverse; a rotated crop resample maps the output pixel center to fractional input coordinates, which round to the nearest content pixel; the picked point is refused when it lands outside the content stage.

The mapping lives in the core and is exposed as a query so API callers can do what the canvas does. The method is `render.locate {asset_id, entry_id?, x, y}`, returning `{content_x, content_y, width, height}` or a `validation` error for a point outside the stage. It never commits. The GUI pick and the API method share the one implementation.

### What stays observable

A pixel edit before a transform moves with the image: unchanged. A pixel edit that addresses transformed dimensions is no longer produced by the host. The pixel module's parameter notes and the history, architecture and crop specs are updated to say content stage. During a crop draft the desktop renders the crop layer's input stage, which now includes every pixel edit, so edits are visible while cropping.

## Performance

Crop-last does not cost more here. A crop at angle zero composes into the exact geometry pass, so it is not a second pass. A rotated crop is a resample that reads only the input pixels its output needs. A point replacement is a point write. The pre-crop frame is only materialized when a pixel-stage layer precedes the crop, which is already the case today whenever an edit precedes a crop, and peak memory stays at two frames. For future brushes the cost of a stroke is proportional to the stroke's own area, and pulling the crop's region of interest back through the tail lets a full-frame stage evaluate only the visible pixels. Crop-first would only save work while the crop is small, and would have to re-render everything the moment the crop changed. Changes under `crates/` answer the performance checklist and are measured on photo-sized inputs.

## Consequences for other designs

- The Basic proposal states that its layer "stays at its saved position among pixel and geometry effects" and is never hoisted ahead of a crop. Under this decision a color-stage layer also belongs before the geometry tail, since it is pointwise and its coordinates are not geometry-dependent. That proposal is updated to say so when it is implemented; nothing in it is implemented here.
- Repeated exact transforms collapse into one [orientation layer](orientation-layer.md); that layer is part of the geometry tail like any other geometry layer.

## Acceptance

- Set a pixel on a cropped photograph, then move, enlarge and shrink the crop through UI and API: the same content pixel stays edited, the crop is never rejected because of the pixel, and exact buffers prove it at angle zero and through the resample reference at a nonzero angle.
- The pick on the canvas and `render.locate` agree, at Fit and at a percentage, before and after transforms and crops.
- Existing identity, ordering and immutability proofs still pass; the interleaving renderer tests keep proving that the renderer's coordinate meaning is unchanged.
- `cargo xtask check`, the editor acceptance run and a background smoke with correlated state and logs.
