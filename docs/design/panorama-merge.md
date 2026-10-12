# Panorama merges

Status: planned. Panorama implementation is not authorized; the HDR parts the owner asked to align to this design are the kind-tagged merge boundary (delivered) and the merge preview for HDR (in progress). On 2026-10-11 the owner asked for a panorama merge design and chose its scope ([decisions](../decisions.md#panorama-merges)): RAW or JPEG frames; selection only, with no automatic grouping; automatic crop with a Boundary Warp option; and raised source limits, so a panorama is not scaled down to today's 128 MP. The [task plan](../../tasks/library/panorama-merge.json) carries the work.

## Boards

Proposed boards for both merge kinds, drawn at 1440 × 900 logical points and rendered at 2×. Each one is also a standalone HTML page under [merges/html](merges/html/index.html). The bracket is made from the catalog boards' ferry frames. The panorama frames are cut from the owner's Sapa drone photograph. Names, values, sizes and times are illustrative.

| Board | Shows |
| --- | --- |
| [Selection menu](merges/selection-menu.png) | A bracket header with Pick all 3, Import merged and Pick merged. Seven overlapping frames selected, with the menu's HDR and Panorama groups (Pick merged…, Import merged…, each with its hint). HDR is disabled, with the plan's refusal as its tooltip, and holding ⌥ drops the ellipsis |
| [HDR preview](merges/hdr-preview.png) | "Merge HDR · 3 exposures": the picture with the deghost overlay tinting a moving boat "From 0 EV". The frames with exposure steppers and Reference, Alignment, Deghosting, the folder and the output. Cancel, Import merged, and Pick merged as primary |
| [Panorama preview](merges/panorama-preview.png) | "Merge panorama · 7 photos": the whole cylindrical stitch with numbered frame outlines, the crop rectangle, the dimmed coverage outside it and "Crop loses 9% at the edges". Projection with Auto's choice, Boundary Warp 0, the overlay switches, the frames with Reference, the folder and the output |
| [Boundary Warp](merges/panorama-warp.png) | The same sheet at Boundary Warp 70: bent edges and outlines, "Crop loses 2%" and a larger output. Inset: the sheet while preparing, "Reading 5 of 7", with its settings and buttons live |
| [Panorama result](merges/panorama-result.png) | Select after Pick merged: the active result with "Panorama · 7 photos" and its stack expanded, "Frame 1 of 7" … "Frame 7 of 7". The Info panel's Stack band with the projection, field of view and size, In the Development set, Collapse stack and Remove merge |
| [Large panorama in Develop](merges/large-develop.png) | The 342 MP result at Fit, with the histogram captioned "Overview". Inset: a far corner at 100%, a full-resolution window refreshed as the view moves |
| [Components](merges/components.png) | Menu items (normal, ⌥ held, refused), pending and failed rows for both kinds, badges at three cell sizes, the preview sheet's status lines (preparing, ready, refused) and the overlay legend chips |

Choices the boards make within the design:

- **The panorama badge** has an icon of its own: a wide frame with curved top and bottom.
- **The sheet's footer** says "⌥-click Pick merged or Import merged to merge with the last settings, without this preview".
- **Overlay colours.** The deghost overlay tints each region by the exposure it came from (violet below the reference, blue the reference, teal above), with fallback regions hatched. The crop is the accent rectangle with corner ticks, and frame outlines are white with numbered circles.
- **Clicking a frame** in the panorama sheet's frame strip makes it the reference.
- **Status lines.** HDR's picture is labelled "Display preview, not the saved pixels". The status bar after a merge names the result and the Development set.
- **Counts.** View counts add "1 merge" beside the photograph count, rather than counting the result as a photograph of the event.

## Outcome and boundaries

Overlapping frames shot by turning one camera become one editable panorama in Select. **Pick merged** and **Import merged** create it as [HDR merges](hdr-merge.md) do: a virtual result in the grid that leads a collapsed stack of its originals. Its durable float pixels are saved under the catalog's artifacts, with its own recipe, history, folder and collection memberships. Opening, editing or exporting it never requires another merge, and the originals stay byte-identical and independently pickable. Removal, library undo and redo, Put back, retention, Empty Removed and rerun are the HDR lifecycle unchanged. A panorama is a second **merge kind** over the boundary HDR built: a derived image source, a merge record with versioned provenance, and a result-led stack.

This design covers:

- one selection of 2–32 frames at a time, all RAW or all JPEG, from one body at one focal length;
- automatic registration under a rotating-camera model, with Perspective, Cylindrical and Spherical projections and Auto;
- exposure and colour equalization across frames, and seams that avoid cutting moving subjects;
- an automatic crop to the largest clean rectangle, with **Boundary Warp** to fill the frame instead;
- **large derived sources** up to 65,000 pixels a side and 512 MP, editable and exportable through bounded tiles;
- the **merge preview** sheet for both kinds, with its overlays and commands;
- generalizing the merge boundary, command parity, Select integration, recovery, bounded jobs and evidence.

It does not cover the following:

- **Inputs and grouping:**
  - HDR panoramas (bracketed positions);
  - merging merged results, which would make a processing graph;
  - automatic panorama detection or grouping in Select, which remains a later catalog feature;
  - mixed RAW and JPEG frames, mixed bodies or focal lengths.
- **Geometry:**
  - parallax correction for a camera that moved rather than turned;
  - multi-row gigapixel layouts beyond the size limits;
  - manual control points;
  - editable seams or masks.
- **Output and editing:**
  - content-aware or generative edge fill;
  - Upright-style perspective correction of the result beyond the projection's levelling, which the existing perspective module offers;
  - raising the limits for camera files;
  - HDR presentation or new export formats.

Export uses the existing JPEG contract within its 65,535-pixel side.

## Grid workflow

The [HDR workflow](hdr-merge.md#grid-workflow) applies with these differences. The [boards](#boards) draw both kinds.

- **Selection only.** With 2–32 suitable frames selected, the selection menu offers a **Panorama** group: **Pick merged…** ("Merge panorama and add to Development set") and **Import merged…** ("Merge panorama into the catalog only"). With 2–9 frames selected it also offers the **HDR** group with the same two items. Opening the menu plans the selection for each kind it offers, at once. A refusal disables that kind's items, with the plan's reason as their tooltip. A bracket header keeps its HDR actions only. There is no panorama header, detector or grouping.
- **The preview opens first.** Each item, and each header action, opens that kind's [merge preview](#the-merge-preview), with the chosen action as its primary button. Holding ⌥ drops the items' ellipsis, and ⌥-click on a header action or item merges at once with the kind's last settings, skipping the preview.
- **Plan before work.** `merge.plan` with `kind: panorama` reads headers only, like HDR's. It checks suitability and estimates the output from focal length, sensor size and frame count. It reports that registration, which needs pixels, decides the final size. A refusal that needs pixels (frames that do not overlap, no usable features) comes from the preview or the job, naming the frames, before anything is published.
- **Boundary Warp is a merge parameter**, not an edit: a different amount is a rerun into a new result. The projection's Auto resolves after registration, and the resolved projection is shown in the preview, on the result's Info band and in provenance.
- **Running.** One row before the first selected frame says "Merging panorama · N photos" with its stage ("Reading 2 of 7", "Matching", "Stitching", "Saving"), a bar and Cancel. A failure names the frame ("DSC_0004.NEF: shares no detail with the other frames") or the stage, with Dismiss.
- **Result.** The result is named after its reference frame with "Panorama", in the folder HDR's rule proposes. Its cell carries "Panorama · N photos" with the stack chevron ("Pano · N" where the long words do not fit, only the icon in the smallest cells). An original's footer reads "Frame k of N" in the stack's order, which is the frames' left-to-right order in the result (top-to-bottom, then left-to-right, for a multi-row panorama). The Info band says "Panorama · N photos", with the resolved projection and field of view, and "Frame k of N" for an original.
- **Claims.** A frame belongs to at most one active stack of any kind, as for HDR: a panorama of frames an active HDR stack holds is refused naming that stack, and the reverse.

## The merge preview

Decided by the owner on 2026-10-11, for both kinds ([decisions](../decisions.md#panorama-merges)). Like Lightroom's Photo Merge preview, **Pick merged** and **Import merged** open a sheet with a picture of the merge and its settings, rather than merging at once. It replaces HDR's Merge options… sheet. The [boards](#boards) draw it.

### What the person sees

- **The sheet** is titled "Merge HDR · N exposures" or "Merge panorama · N photos". Its buttons are Cancel, Import merged and Pick merged; the action that opened it is the primary.
- **The picture** is the merge drawn from the frames' analysis proxies (long side about 1,024 to 2,048 pixels), display-referred for viewing only, never the saved pixels.
- **The status line** under the picture names the preview's scope ("Preview from 1,024-pixel frames") and its key diagnostic. For HDR, that is the alignment residual and whether highlights or shadows were extended. For a panorama, it is the resolved projection, the field of view and the crop's loss.
- **Settings.** HDR's settings are the frames with their exposures and Reference, Alignment and Deghosting (Auto or Off) and the folder. A panorama's are the projection (Auto, Perspective, Cylindrical, Spherical), Boundary Warp 0–100, the frames with Reference, and the folder.
- **Output** shows the size and source bytes the merge will save.
- **Overlays.** The owner chose three:
  - HDR's **Show deghost overlay** tints each moving region and labels the exposure it came from ("From 0 EV", or "From −2 EV · fallback").
  - A panorama's **crop and coverage** shows the whole stitch with its ragged edge, dims what the crop drops and draws the crop rectangle, with "Crop loses N% at the edges".
  - A panorama's **frame outlines** number each frame 1…N in stack order.

  Seams are not drawn.
- **Preparing.** Opening the sheet starts the preview at once. While it reads and aligns, the picture area shows "Reading 2 of 7" or "Aligning" with a bar. The settings are live, and the merge buttons are already enabled: merging before the preview is ready continues the same work.
- **Changes.** A setting change redraws the picture from the retained proxies and registration: a projection, Boundary Warp, Deghosting or overlay change is a redraw. A change that needs registration again (Alignment, Reference, an exposure override) runs it on the proxies. Neither reads the frames again.
- **Refusals.** A refusal the preview finds ("Cannot stitch: DSC_4107.NEF shares no detail with the other frames") replaces the picture, disables both merge buttons and keeps the sheet open, so the person can change settings or Cancel.
- **Merging.** Pick merged or Import merged closes the sheet and shows the pending row. The merge reuses the preview's spooled frames, proxies and registration, so nothing is read or aligned twice, and the settings shown are the ones merged. Cancel discards the preview's spool.
- **Last settings.** ⌥-click merges at once with the kind's last settings: the settings of the person's last merge of that kind, kept in the desktop's [remembered state](preferences.md). The folder is excluded and comes from the plan as today. With none remembered, ⌥-click uses the defaults.

### Commands

The preview is programmable. The desktop sends only these requests. They are built in the core for HDR, as the [HDR design](hdr-merge.md#the-merge-preview) describes; a panorama preview is `not-ready` until its provider exists, and the desktop sheet is not built yet.

| Method | Contract |
| --- | --- |
| `merge.preview` | `kind`, targets, the kind's options and an optional `plan_id`. Answers a cancellable `merge-preview` job whose result is `{preview_id, revision, kind, options, path?, width, height, diagnostics?, overlays?, output?: {width, height, scale, source_bytes}, refusal?}`; a refused revision has no picture, diagnostics or output. `path` is a display-referred JPEG under the catalog's preview cache. `overlays` is data in the picture's coordinates: motion regions with their source input and exposure (HDR), and the covered region, crop rectangle and frame quadrilaterals (panorama). A client draws them, so an agent reads the same facts. |
| `merge.preview-update` | `preview_id` and the kind's full new options. Answers the next revision from the retained proxies, as a short job. Stale updates for the same preview are cancelled. |
| `merge.preview-close` | Discards a preview, its spool and its pictures, answering `{preview_id, closed}`. |
| `merge.create` | Accepts `preview_id`. When given, the job starts from the preview's spool and registration, reading and aligning nothing, and equals a merge without a preview bit for bit. Its options must equal the preview's latest revision's, and a stale `preview_id` is `conflict`. A preview still preparing is continued. Without it, the job behaves as today. |

Each client has at most one open preview; opening another closes the first. A preview occupies the merge lane while it reads and aligns, like a merge. Once ready, its spool stays on disk, counted against the kind's temporary-disk ceiling, until it is merged, closed, or idle for 30 minutes, or until its client disconnects. Its job and spool are cleaned up as a merge's are, including after a restart. A preview never publishes anything and is not a library change.

## Panorama processing contract

The owner settled the scope. The numerical algorithms, thresholds, feature detector, solver, blending method, interpolation kernel and layout details are delegated engineering choices within this contract. Do not add an owner-review, reference-matching or research gate before implementing them. Pin any added dependency, keep its notices, and record manual reviews as deferred. Prefer pure Rust code in `luxforge_core::merge::panorama`; OpenCV is not adopted.

### Validating inputs

- **Count and duplicates.** Accept 2–32 frames. No identity may appear twice, and no RAW and JPEG of one exposure may both be present.
- **Recording.** Every frame is RAW or every frame is JPEG. A mixed selection is refused naming the first frame of the other kind.
- **Camera and lens.** Make and model, monochrome/colour, recording mode, upright dimensions and focal length (within 1 mm or 2% of the first frame's, whichever is more, as HDR allows a zoom's rounded record) must match the first frame. A value recorded on one frame and not another is a mixture; values absent on every frame agree. A merged result is never an input.
- **Exposure.** Exposure, aperture and ISO may differ: panoramas are often shot in automatic exposure. Exposures more than 3 EV apart are refused as a bracket, not a panorama.
- **A bracket.** After registration, two frames registered to within 2% of the frame's diagonal of each other, overlapping at least 90%, are refused as "the same view; merge HDR instead", naming both frames.

### Working pixels

- **RAW frames** develop through the shared verified development, as HDR frames do. The reference frame's as-shot white balance and calibration apply to every frame, then each frame is normalized by its exposure from the settings, as HDR resolves it. Required embedded optical corrections (DNG opcodes) apply as they do for HDR.
- **Lens profiles.** When the lens module's detection would select a profile for the reference frame on first open (distortion `KnownUnapplied` and an eligible Lensfun profile), the job applies that profile's distortion model to every frame before registration. Provenance records the profile key, and the result's optics ledger reports it baked. Otherwise the solver fits one shared radial term, recorded as fitted, and an uncorrected or unknown distortion is left to the fit.
- **JPEG frames** decode through the shared JPEG decode, with its embedded ICC profile honoured (Display P3 phone files among them), into float32 linear sRGB/D65. A JPEG's in-camera correction is unknown, so no profile is applied; the shared radial fit absorbs what remains. This adds a JPEG-to-linear path, used only by merges. HDR stays RAW-only by the owner's decision.
- **Clipping evidence.** RAW frames carry the sensor-clipping level as HDR's do. JPEG frames mark a sample clipped where any encoded channel is at 255. Clipped samples steer seams away and are down-weighted in photometric fits.
- **The working space** is float32 linear sRGB/D65, keeping negative gamut components and values above one, as for HDR. The result is not tone-mapped.

### Registration

- **Analysis proxies.** Each developed frame streams into a bounded analysis proxy (long side about 1,536 pixels, delegated), then is dropped. Features are detected on the proxies' log luminance, at most a bounded number per frame (about 4,000, delegated).
- **Matching.** Match every pair for N ≤ 32: at most 496 pairs, each bounded. A pair is accepted on a robust, RANSAC-style fit of a rotation-induced homography with enough inliers and a probabilistic verification in the manner of Brown and Lowe. Its inlier count, ratio and residual are recorded.
- **Camera model.** The camera turns about its optical centre: one rotation a frame, and one focal length and optional radial term shared by every frame. The focal length comes initially from the focal length and sensor size, or the 35 mm equivalent, else from the matches. A bundle adjustment of every accepted pair's inliers then refines it (robust loss, bounded iterations, delegated). Parallax shows as residual, is reported and is not corrected.
- **Connectivity.** The accepted pairs must connect every frame. A frame left out is refused by name ("shares no detail with the other frames"), and nothing is stitched without it. There is no silent dropping.
- **Levelling.** The rotations are levelled so that the frames' common up direction is vertical (wave correction), unless a frame's recorded orientation contradicts it.
- **The reference** is the frame nearest the panorama's centre after levelling. Capture order breaks ties, or the person names it. It anchors white balance, metadata and the output scale.
- **Refusals** are typed: `Disconnected`, `LowTexture`, `AmbiguousRegistration` (residual or inlier thresholds), `SameView`, `ExcessiveFieldOfView` (beyond what the chosen projection can show) and `OutputTooLarge`. Each names its frames.

### Projection and output size

- **Projections.** Perspective (rectilinear) keeps straight lines straight and suits fields under about 120° wide. Cylindrical suits wide single rows. Spherical (equirectangular) suits tall, multi-row or 360° panoramas. Auto chooses from the registered field of view (delegated thresholds) and records which it chose. Perspective is refused beyond its field (`ExcessiveFieldOfView`), and the plan suggests Cylindrical.
- **Scale.** The output's scale matches the reference frame's centre pixel: about one output pixel to one input pixel there, never upscaled.
- **Too large at full scale.** A stitch above the large-source limits (65,000 pixels a side, 512 MP, 6 GiB of float RGB) is scaled down uniformly to fit. The plan's estimate and the job's progress report the scale before saving, and provenance records it. The plan names the scale whenever it can predict one.
- **One resample.** Every output pixel maps back through the boundary warp, the projection and the frame's rotation, focal length and radial term to an input position. Each frame is resampled once in the linear domain from its developed pixels. The interpolation kernel is delegated; whatever it is, results are finite and the reference comparison holds.

### Exposure, colour and seams

- **Photometric fit.** After normalization from metadata (RAW) or none (JPEG), a robust per-frame gain is fitted from overlapping, unclipped, low-gradient samples, anchored on the reference (gain 1). JPEG frames fit one gain a channel, because in-camera white balance may vary. RAW frames fit one gain a frame, since their white balance is common. Gains, residuals and confidence are recorded. Vignetting left after a profile is left to the blend; fitting a radial fall-off is an implementer choice.
- **Seams.** Seams are placed on the proxies by a minimum-cost cut through each overlap (graph cut or equivalent, delegated). The cost is colour difference, gradient and clipping, so a moving subject is taken whole from one frame rather than doubled. The seam map scales to full resolution with a bounded transition width. Seams are not editable.
- **Blending** is band-limited across seams (multiband in the manner of Burt and Adelson, or an equivalent), with its widest band bounded. Each output chunk is then computed from bounded windows of the frames and the seam map. Blending may not produce non-finite values. Any overshoot it introduces is bounded by the blend's declared kernel and recorded in diagnostics; nothing is clamped.
- **Diagnostics** record per-frame gains, seam lengths, frames contributing to each output region, pixels from clipped samples, and the largest registration residual.

### Boundary Warp and the crop

A stitch's covered region is ragged. The stored result is always a rectangle of covered pixels, because derived sources have no alpha and the renderer's frames are opaque by contract.

- **Boundary Warp 0** (default): crop to the largest axis-aligned rectangle of pixels every frame's mapping covers, as HDR's coverage does. Ties go to the smallest top row, then bottom row.
- **Boundary Warp 100:** a smooth, content-preserving mesh warp of the covered region onto its bounding rectangle, in the manner of He, Chang and Sun's rectangling, so the result loses almost no content but bends near the edges. The mesh's resolution and energy terms are delegated; it is bounded, deterministic and solved on the proxy.
- **Between:** each mesh vertex's displacement is scaled by the amount, then the result is cropped to the largest rectangle the warped region covers.
- **Rules.** The warp is part of the one output mapping, so pixels are still resampled once. Provenance records the amount, the warp mesh, the crop rectangle and the pixels lost. A crop under a quarter of the bounding rectangle on a side is refused as `InsufficientCoverage`.

### The provider and its evidence

The provider is `{module: "luxforge.panorama", version: 1}`, a lazy merge provider beside `luxforge.hdr`. It owns suitability, registration, projection, photometric fitting, seams, blending and the warp. The core owns admission, resources, publication, stacks, transactions and history, as for HDR. Its pure numerics take no file, catalog or job work.

`cargo test -p luxforge-core panorama_merge` checks the following against an independent f64 reference (`luxforge_reference::panorama`):

- **Scenes.** Synthetic scenes are rendered through known rotations, focal lengths, radial distortion, gains, noise and a moving subject, with 2–32 frames in single and multiple rows, including a 360° row.
- **Registration.** Every corner of every frame is within a declared pixel tolerance. The focal length and radial term are within declared tolerances.
- **Projection and output.** Exact projection mappings and crop rectangles.
- **Photometrics and seams.** Gains are recovered within a declared tolerance. A moving subject appears once.
- **Boundary Warp.** At 0 it equals the crop; at 100 the warp covers at least a declared fraction of the bounding rectangle.
- **Fixtures and determinism.** Complete stitched buffers match the reference within a declared tolerance. Output is bit-identical for any split into output chunks.
- **Coverage.** Every typed refusal is checked, along with cancellation at every stage and the stated bounds.

The tolerances are set by the implementer and recorded here when delivered.

## Large derived sources

The owner chose to raise the limits for derived sources rather than scale panoramas down: a derived source may be up to **65,000 pixels a side and 512 MP** (6 GiB of float32 RGB), inside JPEG's 65,535-pixel side. Camera files keep their current limits (16,384 a side, 128 MP, 1.5 GiB decoded), and so does every HDR result, since an HDR result is never larger than its reference frame.

Today every derived source is prepared into one whole-frame `LinearImage`, and the GPU surface uploads it whole: at most 2 × 2 textures of 8,192, charged to the 2 GiB preview budget. Export, catalog previews, samples and the Dehaze light already render through windowed GPU tiles, but they cut their windows from that one allocation. The reference renderer evaluates whole frames, bounded at 512 MiB of output. None of these holds a 512 MP source, so a source past the camera limits becomes a **paged source**.

### Storage

- **Two classes.** A derived source within the camera limits is stored and prepared exactly as today. A larger one is paged. Which class a source is follows from its dimensions alone.
- **Manifest format 2** adds `levels`. Level 0 is the full-resolution chunk grid as in format 1. Each further level is a 2 × 2 area average of the one before, stored as its own chunk grid, down to the first level whose long side is at most 4,096. A level is written only for a paged source. The pyramid adds about a third to the source's bytes.
- **Writing.** The merge job writes level 0 chunk by chunk as it stitches. It builds each further level in a bounded streaming pass over the previous level's chunks, never holding a level whole.
- **Limits.** The chunk side stays 1,024, so level 0 holds at most 4,096 chunks, and a source at most 5,462 over all its levels. The manifest is capped at 1 MiB, and each chunk at 16 MiB. The per-source `layout()` limits split from the frame and stage limits they bound together today: a camera source, a resident derived source, a paged derived source, and a rendered frame each have their own declared limits.
- **Shapes.** Format 1 manifests stay valid for resident sources. A paged source requires format 2. Any other shape is `incompatible` and kept.

### Preparation

- **No whole frame.** `source.prepare` on a paged source reads and verifies its manifest, and the smallest pyramid level, on the source worker. It never allocates level 0 whole.
- **Reading chunks.** Chunks of every level are read on demand through a bounded, verified chunk cache. Each chunk is hashed when read, and a damaged chunk fails what needed it, naming the chunk. The cache is charged to prepared-source accounting, at most 1.5 GiB, the most one resident source may hold today.
- **The plane gate** counts bytes for paged sources, so a paged source's cache and an editor or merge development cannot together exceed what the gate admits.
- **Windows and levels.** A paged source answers two requests. A *window* of any level, with halos, is assembled from the chunks it touches. A *resident level* is the largest pyramid level within the camera limits, prepared whole and held like an ordinary source.

### Rendering, samples and export

- **Below that level's scale** (Fit, and zooms under the level's scale), the GPU surface holds the resident level as its source. It renders the stack on that level, with spatial parameters in source pixels scaled to the level, as the reduced Fit path renders a reduced source today. This includes the picture at rest and the histogram. This is a declared approximation of the full-resolution output reduced to the view. Its tolerance against the reference's full-resolution rendering reduced the same way is declared per output family by the implementer, qualified on the corpus and reported. The Info panel's histogram says it counts the overview.
- **At and above the level's scale**, including 100%, the surface holds a window of level 0: the visible region with its halos, through `GpuSource::window`, refreshed as the view moves. Stale window reads are cancelled. A window is charged to the GPU preview budget like any source upload.
- **Samples** read level 0 through windows, so a sampled value is the full-resolution value at every zoom.
- **Export** streams full-resolution GPU tiles from level-0 windows, as it does today. Band bytes are capped at 256 MiB by choosing the tile side or splitting a band into columns before assembly, so a 65,000-pixel-wide band is never 2,048 rows high.
- **Staged sweeps.** A stack whose staged sweeps need stage textures beyond the GPU's 2 × 2 × 8,192 limit draws through window chains if the implementer makes that fit. Otherwise it falls back to the reference with its named reason (`tiles-budget`), never by failing silently.
- **Catalog previews** render from the resident level.
- **Bounds on large stages.** The Dehaze light's reduction grid scales its factor so the grid stays within its current bound. Presence keeps its declared 10,000-pixel scale cap. The 512-pixel spatial halo bound is unchanged.

### The reference on paged sources

The CPU reference remains the oracle, so it gains a **banded mode**. It evaluates the output stage in full-width bands, each from level-0 windows with the stack's halos and its exact spatial semantics, and streams the bands to the export encoder or a test. Two requirements:

- On any source small enough for both modes, the banded mode is bit-identical to the whole-frame mode, for every output family.
- A whole-frame statistic, such as the Dehaze light, is computed in a first bounded pass and gives the same result as the whole-frame computation.

The reference export of a paged source uses the banded mode, and so does the reference analysis at full resolution. The 512 MiB evaluated-frame limit then bounds a band, not the frame. Paged sources are qualified with a large generated derived entry in the GPU corpus, about 160 MP and wider than 16,384, through the banded reference.

### Memory

A paged source's process cost is bounded by:

- its chunk cache;
- its resident level (at most 1.5 GiB);
- the GPU windows within the existing preview and tile budgets;
- export's band cap.

No component holds level 0 whole. The measurement task reports the real peaks; no total-memory bound is claimed beyond those budgets.

## Generalizing the merge boundary

HDR's boundary was built for one kind. A panorama needs these changes. They also change the [HDR design](hdr-merge.md)'s stored shapes and commands, which describe them.

The kinds, stack members, provenance, catalog format, commands, lane, ceilings, usage exemption and desktop words below are implemented, and so are the [merge preview](#the-merge-preview)'s commands for HDR. This build registers HDR's provider alone (`crates/luxforge-core/src/merge/providers.rs`): schemas list `hdr` as the only `kind`, a panorama request is refused `not-ready` naming `luxforge.panorama`, there are no panorama options, and a panorama merge's provenance, which no build writes yet, is read as `incompatible` while browse still shows its stack.

- **Kinds.** `MergeKind` gains `Panorama` (`"panorama"`). `merges.kind` accepts it, and browse reads it rather than refusing it as `incompatible`.
- **Stack members.** `merge_inputs.offset_ev` becomes nullable: HDR members keep their EV offset, while panorama members have none and carry their position in the stack's order. Browse's reference ordinal comes from the provenance's reference, not from the zero offset.
- **Provenance.** `MergeProvenance` (format 2) holds kind-tagged parameters and resolution: `HdrParameters`/`HdrResolution` as today, or `PanoramaParameters` (projection, boundary warp, reference) and `PanoramaResolution`. `PanoramaResolution` holds:
  - the profile or fitted radial term;
  - focal length;
  - rotations and residuals;
  - pairs and connectivity;
  - the resolved projection and field of view;
  - the scale;
  - gains;
  - seam and blend diagnostics;
  - the warp mesh and crop.

  `match_key` hashes kind-tagged parameters. `MergeInput.frame` keeps the shared header fields, with exposure as recorded.
- **Catalog format.** Format 15 carries these shapes. Earlier catalogs are refused; there are no migrations. The HDR design's stored-shape paragraphs are updated in the same change.
- **Commands.** `merge.plan`, `merge.create` and `merge.rerun` take `kind` (`hdr` or `panorama`; `merge.plan` requires it). Kind-specific options live under `hdr` or `panorama`, and a field of the other kind is refused as `validation`. HDR's existing top-level option fields move under `hdr` in the same change. Input bounds are per kind (HDR 2–9, panorama 2–32). Refusals are typed per provider, and their messages name the kind.
- **Jobs.** The job kinds are `hdr-merge` and `panorama-merge`. They share one merge lane: one running merge of either kind and one waiting, with a further request refused as `resource-limit`. Stages are named per kind. A panorama's are `reading`, `matching`, `stitching` and `saving`.
- **Spool reads.** The spool is shared, and both kinds read it by bands rather than one seek and read a row. This is the read pattern behind HDR's measured 60 MP × 9 fusion limit, and HDR's fusion benefits from it too. A panorama's spool may hold proxies' features and seam maps beside the frames.
- **Ceilings.** The provisional temporary-disk ceiling is per kind: 16 GiB for HDR, and 64 GiB for a panorama (32 frames of 60 MP at 14 bytes a pixel, plus a 512 MP result). Each is preflighted against the volume's free space as HDR's is. Scratch stays explicitly accounted: 256 MiB for HDR and 512 MiB for a panorama, refusing before allocation.
- **Usage statistics.** A panorama `merge.create` or `merge.rerun` is an explicit exemption in the usage registry until the owner approves a `panorama-merge` event. It is never counted as `hdr-merge`. `merge-remove` and `merge-restore` count both kinds.
- **Desktop.** Select's merge state, menu, options sheet, pending row, badges, stack words, Info band and refusal text take the kind from the plan or the stack, not from a constant.

## Bounds, recovery and regeneration

HDR's [bounds and recovery](hdr-merge.md#bounds-recovery-and-regeneration) apply. A running panorama keeps no N developed frames together. It develops one frame at a time, spooling it with its proxy, and stitches output chunks from bounded windows. A request that would exceed the temporary ceiling, the free space or the scratch limit is refused before any work that would be thrown away. Registration decides the output size, so the scale that fits the large-source limits, and any refusal the final size causes (free space for the result), come after registration and before stitching, with nothing published. Cancellation is checked between reads, matching passes, solver iterations, seam passes and output chunks. Reopen, edit and export need only the saved source, never the originals.

## Evidence and acceptance

Ordinary correctness, recovery and rendered checks ship with the working feature. Natural-panorama quality and quiet-host timing follow it, with no dependency back into delivery. Claims stay limited to the sources and workloads actually run.

1. The independent reference fixtures above, and large-source fixtures above 128 MP: exact round trips, tile renders against the reference within the declared tolerance, and the overview pyramid.
2. Originals' hashes stay identical across success, cancellation, failures, undo/redo, removal, restoration and purge. Index rebuild and reopen preserve provenance and stacks of both kinds.
3. Failure injection at every source-publication boundary, retries, concurrent mutation and set replacement, for panoramas and HDR through the shared lane.
4. A real-owner multi-client comparison of both creation modes, reuse, refusal by name, mixed-kind claims, rerun with another Boundary Warp, and every desktop request matching the documented one.
5. A background rendered `panorama-merge` journey from generated frames:
   - Pick merged through the preview sheet, with Boundary Warp changed and the overlays on, and once by ⌥-click;
   - progress;
   - one collapsed panorama tile;
   - its expanded originals;
   - Develop at Fit, at 100% in a far corner, and in an edit with a mask;
   - export of a result above 128 MP;
   - Remove merge, undo and redo with no second merge;
   - a relaunch with the originals moved away.

   The `hdr-merge` journey and existing JPEG and RAW journeys remain valid.
6. After delivery, natural panoramas, the owner's own where available (the Z6, X100VI and Air 2S among them), qualify seams, levelling, colour, crop losses and warp quality. Photo-sized measurements behind the host lock report:
   - merge stages, cold and warm;
   - cancellation latency;
   - temporary disk;
   - peak process, source, scratch and GPU use;
   - editor latency at Fit and 100% on a 512 MP result;
   - export time;
   - browse latency beside a merge.

   Measured inputs are 24 and 60 MP frames, 5 and 32 of them.

The implementation answers the [performance-rules checklist](../engineering/performance-rules.md#review-checklist). When delivered, update this design, the HDR design's shared shapes, [feature status](../features.md), the [user guide](../user-guide.md), the [catalog design](catalog.md#hdr-and-other-merges) and command documentation.

## Numerical references

[Brown and Lowe, *Automatic Panoramic Image Stitching using Invariant Features* (IJCV 2007)](https://link.springer.com/article/10.1007/s11263-006-0002-3) describes feature matching, probabilistic pair verification, rotation bundle adjustment, wave correction, gain compensation and multiband blending; [OpenCV's stitching pipeline](https://docs.opencv.org/4.x/d1/d46/group__stitching.html) follows it. [Burt and Adelson's multiresolution spline (1983)](https://doi.org/10.1145/245.247) underlies multiband blending. [He, Chang and Sun, *Rectangling Panoramic Images via Warping* (SIGGRAPH 2013)](https://doi.org/10.1145/2461912.2462004) underlies boundary warping. [Hugin and Panorama Tools](https://hugin.sourceforge.io/) are an open-source reference for projections and photometric optimization. These are numerical references, not a decision to adopt any of them or a claim that their methods meet Luxforge's resource and quality contract.
