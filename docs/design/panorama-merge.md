# Panorama merges

Status: planned. Panorama implementation is not authorized; the HDR parts the owner asked to align to this design are the kind-tagged merge boundary (delivered) and the merge preview for HDR (in progress). On 2026-10-11 the owner asked for a panorama merge design and chose its scope ([decisions](../decisions.md#panorama-merges)): RAW or JPEG frames; selection only, with no automatic grouping; automatic crop with a Boundary Warp option; and raised source limits, so a panorama is not scaled down to today's 128 MP. The [task plan](../../tasks/library/panorama-merge.json) carries the work.

## Boards

Proposed boards for both merge kinds, drawn at 1440 × 900 logical points and rendered at 2×. Each one is also a standalone HTML page under [merges/html](merges/html/index.html). The bracket is made from the catalog boards' ferry frames. The panorama frames are cut from the owner's Sapa drone photograph. Names, values, sizes and times are illustrative. The boards draw the earlier pair of actions, Pick merged and Import merged, and a Development set; these are now one action, **Merge HDR** or **Merge panorama**, which makes the result in its catalog folder, and the sheet's buttons are Cancel and Merge.

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
- **The sheet's footer** says "⌥-click Merge HDR or Merge panorama to merge with the last settings, without this preview".
- **Overlay colours.** The deghost overlay tints each region by the exposure it came from (violet below the reference, blue the reference, teal above), with fallback regions hatched. The crop is the accent rectangle with corner ticks, and frame outlines are white with numbered circles.
- **Clicking a frame** in the panorama sheet's frame strip makes it the reference.
- **Status lines.** HDR's picture is labelled "Display preview, not the saved pixels". The status bar after a merge names the result and its folder.
- **Counts.** View counts add "1 merge" beside the photograph count, rather than counting the result as a photograph of the event.

## Outcome and boundaries

Overlapping frames shot by turning one camera become one editable panorama in Select. **Merge panorama** creates it in its catalog folder as [HDR merges](hdr-merge.md) do: a virtual result in the grid that leads a collapsed stack of its originals. Its durable float pixels are saved under the catalog's artifacts, with its own recipe, history, folder and collection memberships. Opening, editing or exporting it never requires another merge, and the originals stay byte-identical and independently pickable. Removal, library undo and redo, Put back, retention, Empty Removed and rerun are the HDR lifecycle unchanged. A panorama is a second **merge kind** over the boundary HDR built: a derived image source, a merge record with versioned provenance, and a result-led stack.

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

- **Selection only.** With 2–32 suitable frames selected, the selection menu offers **Merge panorama…** ("Merge the frames into one panorama in the catalog"). With 2–9 frames selected it also offers **Merge HDR…**. Opening the menu plans the selection for each kind it offers, at once. A refusal disables that kind's item, with the plan's reason as its tooltip. A bracket header keeps its HDR action only. There is no panorama header, detector or grouping.
- **The preview opens first.** Each item, and the header's Merge HDR, opens that kind's [merge preview](#the-merge-preview). Holding ⌥ drops the items' ellipsis, and ⌥-click on the header action or an item merges at once with the kind's last settings, skipping the preview.
- **Plan before work.** `merge.plan` with `kind: panorama` reads headers only, like HDR's. It checks suitability and estimates the output from focal length, sensor size and frame count. It reports that registration, which needs pixels, decides the final size. A refusal that needs pixels (frames that do not overlap, no usable features) comes from the preview or the job, naming the frames, before anything is published.
- **Boundary Warp is a merge parameter**, not an edit: a different amount is a rerun into a new result. The projection's Auto resolves after registration, and the resolved projection is shown in the preview, on the result's Info band and in provenance.
- **Running.** One row before the first selected frame says "Merging panorama · N photos" with its stage ("Reading 2 of 7", "Matching", "Stitching", "Saving"), a bar and Cancel. A failure names the frame ("DSC_0004.NEF: shares no detail with the other frames") or the stage, with Dismiss.
- **Result.** The result is named after its reference frame with "Panorama", in the folder HDR's rule proposes. Its cell carries "Panorama · N photos" with the stack chevron ("Pano · N" where the long words do not fit, only the icon in the smallest cells). An original's footer reads "Frame k of N" in the stack's order, which is the frames' left-to-right order in the result (top-to-bottom, then left-to-right, for a multi-row panorama). The Info band says "Panorama · N photos", with the resolved projection and field of view, and "Frame k of N" for an original.
- **Claims.** A frame belongs to at most one active stack of any kind, as for HDR: a panorama of frames an active HDR stack holds is refused naming that stack, and the reverse.

### In the desktop

Select draws this workflow with HDR's code (`crates/luxforge-app/src/{state,app,view}/select_merge.rs`, the sheet's model in `state/select_merge/sheet.rs`), taking the kind's words and icon from the plan or the stack; [HDR's desktop](hdr-merge.md#in-the-desktop) describes what both kinds share.

- **Menu.** Over 2–9 selected frames the menu has the HDR group then the Panorama group, each headed by its kind with its one item and hint; over 10–32 only the Panorama group. Each offered kind's `merge.plan` is sent when the menu opens. HDR's item is refused for JPEG frames with the plan's reason; a panorama of RAW or JPEG frames is offered. No moment header offers a panorama.
- **The sheet.** Merge panorama… opens the [merge preview](#the-merge-preview) titled "Merge panorama · N photos". Its status line reads "Preview from 1,024-pixel frames · Cylindrical, 164° × 41° · Crop loses 9% at the edges" (under 1% is said as such), with the legend chips for the overlays shown. **Crop and coverage** dims everything outside the crop, outlines the covered region and draws the crop in the accent with corner ticks; **Frame outlines** draws each frame's outline in white with its number, from 1 in the stack's order, in a circle. Both are on when the sheet opens and send nothing. The side has Projection (Auto, Perspective, Cylindrical, Spherical), with "Auto chose Cylindrical for a 164° × 41° field." under it while Auto is chosen; Boundary Warp 0–100, sent once the slider is let go (its reset sends 0); the frames as a strip of numbered thumbnails in the stack's order (the plan's order, unnumbered, until a revision has registered them), the reference framed in the accent and named above ("Reference: 4 · DSC_4104.NEF"), a click on another frame making it the reference by path; the folder, as HDR's; and the output "38,400 × 8,900 · 342 MP · 4.1 GB". The revision's `diagnostics.frames` names each frame's path in the stack's order, which is how the sheet numbers the strip.
- **Last settings.** A panorama merge started from the sheet or by ⌥-click remembers its projection and Boundary Warp as the `panorama_merge` [preference](preferences.md#preferences) (`{projection, boundary_warp}`; the reference and folder are never remembered), and ⌥-click and a newly opened sheet start from them, else from Auto and 0.
- **Running and result.** The pending row and its failure read "Merging panorama · N photos" and "Panorama merge failed · N photos" with the job's stage words and the panorama icon (a wide frame with curved top and bottom). The result's badge is "Panorama · N photos", "Pano · N" or the icon alone; each original's footer and band read "Frame k of N". A panorama result shown alone in the Info panel sends `merge.inspect` once and its band adds the resolved projection and field of view ("Cylindrical · 164° × 41°"), "Reading the merge…" until it answers.
- **Large results in Develop.** A result above the resident limits, viewed in its catalog folder, opens in Develop with `D` through the open every photograph takes, drawn as a [paged source](#large-derived-sources) is, and Select is shown again as it was.

## The merge preview

Decided by the owner on 2026-10-11, for both kinds ([decisions](../decisions.md#panorama-merges)). Like Lightroom's Photo Merge preview, **Merge HDR** and **Merge panorama** open a sheet with a picture of the merge and its settings, rather than merging at once. It is built in the desktop for both kinds: [HDR's](hdr-merge.md#in-the-desktop) and [a panorama's](#in-the-desktop). The [boards](#boards) draw it.

### What the person sees

- **The sheet** is titled "Merge HDR · N exposures" or "Merge panorama · N photos". Its buttons are Cancel and Merge, the primary.
- **The picture** is the merge drawn from the frames' analysis proxies (long side about 1,024 to 2,048 pixels), display-referred for viewing only, never the saved pixels.
- **The status line** under the picture names the preview's scope ("Preview from 1,024-pixel frames") and its key diagnostic. For HDR, that is the alignment residual and whether highlights or shadows were extended. For a panorama, it is the resolved projection, the field of view and the crop's loss.
- **Settings.** HDR's settings are the frames with their exposures and Reference, Alignment and Deghosting (Auto or Off) and the folder. A panorama's are the projection (Auto, Perspective, Cylindrical, Spherical), Boundary Warp 0–100, the frames with Reference, and the folder.
- **Output** shows the size and source bytes the merge will save.
- **Overlays.** The owner chose three:
  - HDR's **Show deghost overlay** tints each moving region and labels the exposure it came from ("From 0 EV", or "From −2 EV · fallback").
  - A panorama's **crop and coverage** shows the whole stitch with its ragged edge, dims what the crop drops and draws the crop rectangle, with "Crop loses N% at the edges".
  - A panorama's **frame outlines** number each frame 1…N in stack order.

  Seams are not drawn.
- **Preparing.** Opening the sheet starts the preview at once. While it reads and aligns, the picture area shows "Reading 2 of 7" or "Aligning" with a bar. The settings are live, and Merge is already enabled: merging before the preview is ready continues the same work.
- **Changes.** A setting change redraws the picture from the retained proxies and registration: a projection, Boundary Warp, Deghosting or overlay change is a redraw. A change that needs registration again (Alignment, Reference, an exposure override) runs it on the proxies. Neither reads the frames again.
- **Refusals.** A refusal the preview finds ("Cannot stitch: DSC_4107.NEF shares no detail with the other frames") replaces the picture, disables Merge and keeps the sheet open, so the person can change settings or Cancel.
- **Merging.** Merge closes the sheet and shows the pending row. The merge reuses the preview's spooled frames, proxies and registration, so nothing is read or aligned twice, and the settings shown are the ones merged. Cancel discards the preview's spool.
- **Last settings.** ⌥-click on Merge HDR or Merge panorama merges at once with the kind's last settings: the settings of the person's last merge of that kind, kept in the desktop's [remembered state](preferences.md). The folder is excluded and comes from the plan as today. With none remembered, ⌥-click uses the defaults.

### Commands

The preview is programmable. The desktop sends only these requests. They are built in the core for both kinds, as the [HDR design](hdr-merge.md#the-merge-preview) describes. The desktop's sheets are built on them for [HDR](hdr-merge.md#in-the-desktop) and [panoramas](#in-the-desktop).

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
- **The frame path** (`luxforge_core::merge::panorama::frames`) reads, develops and spools one frame at a time, outside the source worker's plane gate, into HDR's spool format with an analysis proxy (and a colour proxy for a preview). A frame may be multiplied by its exposure scale as it is spooled; its clipping evidence is taken before that scale. A lens profile is applied as the reference renderer applies a lens layer: each output pixel centre mapped through the profile's distortion with its cover scale and sampled bilinearly with clamped taps. A warped pixel's clipping evidence is the largest level among the taps it reads.
- **JPEG colour profiles.** A merge accepts any RGB matrix/TRC profile (sRGB, Display P3, Adobe RGB and others) and converts it relative colorimetrically through the D50 connection space. It refuses a profile whose CICP tag disagrees with its matrix and curves as `unsupported-profile`, and reads a file with no profile as sRGB. Below 255, a JPEG pixel's level is its largest channel's linear value at its own tone curve. Opening a JPEG in the editor still accepts sRGB only.
- **Tolerances.** A JPEG frame matches an independent f64 reference within 5 × 10⁻⁴ absolute in linear sRGB. That reference is the IEC 61966-2-1 transfer, and for Display P3 the matrix derived from the primaries' chromaticities. A lens-baked RAW frame matches the reference crate's f64 lens warp of the same development within 10⁻⁶, relative to the larger of one and the value. A RAW frame with no profile is the shared development bit for bit. `cargo test -p luxforge-core panorama_frames` checks all three.

### Registration

Registration is built as pure numerics in `luxforge_core::merge::panorama` (`proxy`, `features`, `matching`, `bundle`, `registration`); no job runs it yet.

- **Conventions.** Coordinates are full-resolution upright frame pixels, pixel `(x, y)` centred at `(x, y)`, with the principal point at the frame's centre. Camera and world directions have `x` right, `y` down and `z` forward. A frame's rotation takes world directions to its camera's. The shared lens records the ideal offset `p` from the centre at `p·(1 + k·|p|²/R²)`, with `R` half the frame's diagonal; `k` stays within ±0.14, where every point of the frame has one ideal point.
- **Analysis proxies.** Each developed frame streams, in bands of any height, into an analysis proxy reduced by an integer box factor to at most 1,536 pixels a side. It holds each box's `log2` Rec. 709 luminance and whether the box is usable (finite, unclipped and above 2⁻¹⁶). That is 5 bytes a proxy pixel, under 12 MiB for a 60 MP frame. Thresholds stated in proxy pixels scale by the box factor, and every result is in full-resolution pixels.
- **Features.** At most 4,000 a frame are detected on up to three pyramid levels. Each is a Harris corner (harmonic-mean response at least 10⁻³ EV² a pixel²), the strongest three in each 16-pixel cell, refined to sub-pixel by a Newton step on the response. Each carries a 256-bit comparison descriptor of an oriented 8 × 8 grid. Comparisons ignore any monotone change of brightness, so exposure and gain do not change a descriptor. One detection holds at most 40 bytes a proxy pixel of scratch.
- **Matching.** Every pair is matched, at most 496 for 32 frames. Putative matches are mutual nearest neighbours by Hamming distance (at most 64 bits, Lowe's ratio 0.8). A RANSAC homography makes at most 512 draws, seeded by the pair, with inliers within 3 proxy pixels both ways. It is verified in the manner of Brown and Lowe: at least 12 inliers, and `n_i > α + β·n_f` from inlier probabilities 0.6 and 0.1, a prior of 10⁻⁶ and a posterior of 0.999 (about `8.0 + 0.31·n_f`). Frames the verified pairs leave apart from the largest group are refused as `Disconnected`.
- **Camera model.** One rotation a frame, and one focal length and radial term shared by every frame (the term is fixed when a lens profile was applied). The focal length starts from the recorded focal length and sensor size, or the 35 mm equivalent. Without a record it starts from the median of the verified homographies' implied focal lengths, or from the diagonal when none implies one. A search over 1% steps then keeps the candidate under which per-pair rotations fitted to the homography inliers agree with the most matches; it spans ×/÷ 1.25 around a record, 1.5 around matches and 3 around the diagonal, at most 256 candidates. Each pair's rotation is fitted to its inliers within 3 proxy pixels and verified again, and a maximum spanning tree places the frames. A Levenberg–Marquardt bundle adjustment then refines every accepted pair's inliers both ways: at most 150 a pair, Huber's loss at 1 proxy pixel, weight `4^−ℓ` for a match on pyramid level `ℓ`, and at most 50 iterations a round. After each round every verified pair is classified again within 2 proxy pixels, and frames are placed from verified pairs to placed frames, for at most 3 rounds. Each pair records its matches, its matches in the overlap, its inliers, inlier ratio and RMS residual. The registration records the largest and RMS residuals. Parallax shows as residual and is not corrected.
- **Connectivity.** The accepted pairs must connect every frame. A frame left out is refused by name, and nothing is stitched without it. There is no silent dropping. A frame that a verified homography links to the largest group, but that no turn of one camera registers (a zoom, parallax), is refused as `AmbiguousRegistration`; otherwise it is `Disconnected`.
- **A bracket.** Two frames are refused as `SameView` when every corner of each lands within 2% of the diagonal of the other's and each covers at least 90% of the other.
- **Levelling.** The world's down is the direction most nearly perpendicular to every frame's right axis (wave correction), drawn weakly (10⁻³ a frame) towards the frames' own down, so a column or two nearly parallel frames keep their recorded up. When that rolls a frame within 60° of the horizon by more than 45°, or lies more than 45° from the frames' mean down, the frames' recorded orientation contradicts it. The mean down is then used, and the result names the frame rolled furthest. The world's yaw 0 is the centre of the frames' yaw arc.
- **The reference** is the frame whose optical axis is nearest the panorama's centre: the yaw arc's centre at the middle of the frames' pitch range. Frames within 0.1° of the nearest are tied, and capture order breaks the tie. The person may name it instead. It anchors white balance, metadata and the output scale.
- **Refusals** are typed: `Disconnected`, `LowTexture` (fewer than 32 features), `AmbiguousRegistration`, `SameView`, `ExcessiveFieldOfView` (beyond what the chosen projection can show) and `OutputTooLarge`. Each names its frames. Registration refuses with the first four.
- **Bounds and cancellation.** One registration of 32 frames holds at most 128 MiB besides the features, with every pair at its bound. Cancellation is checked before each stage (matching, verifying, focal, bundle, levelling), each pair, focal candidate, bundle iteration and damping. Within those it is checked every 256 features of a pair's matching, every 64 RANSAC draws and every 64 rows of each detection pass. It therefore ends a stage within one such unit of work. Results are identical on any thread count: pairs run independently on the shared pool and combine in pair order.
- **Declared tolerances** against `luxforge_reference::panorama`, with and without noise, gains and a moving subject, for 320 × 240 frames read at full scale. After the one world rotation registration leaves free, every frame corner and centre lies within 0.5 pixel for full circles of 12 and 32 frames and a 3 × 5 grid. Open single rows of 2–6 frames are held to 1% of the frame's diagonal, since their focal length rests on the overlaps' perspective alone. The focal length is within 1%, a fitted radial term within 0.01, and a levelled world's down within 0.25° of the truth. The tests run under `cargo test -p luxforge-core panorama_registration`.

### Projection and output size

The stitch is built as pure numerics in `luxforge_core::merge::panorama` (`projection`, `layout`, `photometric`, `seams`, `blend`, `warp`, `stitch`), and the panorama job runs it through its stitch seam (`stitching.rs`). The plan lays out a stitch from the registration and each frame's colour proxy without reading a frame. Rendering draws any rectangle of the result from windows of the frames' full-resolution pixels, which the job reads back from the spool on request, one window of at most 8 Mi pixels at a time.

- **Projections.** Perspective (rectilinear) keeps straight lines straight. Cylindrical suits wide single rows. Spherical (equirectangular) suits tall, multi-row or 360° panoramas. Perspective is refused (`ExcessiveFieldOfView`, suggesting Cylindrical) when any frame's edge lies more than 60° from the plane's centre horizontally or vertically (120° across), or behind the plane. Cylindrical is refused (suggesting Spherical) beyond 70° of latitude. Auto chooses Perspective for a field within 90° × 90° that Perspective can show, Cylindrical when no frame edge passes 55° of latitude, and Spherical otherwise, and records its choice.
- **Layout.** The output plane is the levelled world's: yaw across and, for the angular projections, latitude (Spherical) or its tangent (Cylindrical) down. The bounding rectangle holds every frame's boundary, traced at 256 points an edge. A row that closes the circle wraps exactly once around 360°; otherwise the plane's seam lies in the widest gap between frames.
- **Scale.** Full scale makes the reference frame's centre pixel at most one output pixel in any direction (the inverse of the projection's largest stretch there), so nothing is upscaled.
- **Too large at full scale.** A stitch above the large-source limits (65,000 pixels a side, 512 MP, 6 GiB of float RGB) is scaled down uniformly to fit. The scale and the full-scale size are in the plan's diagnostics. Below an eighth of full scale it is refused as `OutputTooLarge`, naming the full-scale size. Below full scale each output pixel averages `n × n` points, `n = ⌈1/scale⌉`, at most 4.
- **One resample.** Every output pixel maps back through the boundary warp, the projection and the frame's rotation, focal length and radial term to an input position. Each frame is sampled once, bilinearly with clamped taps, in the linear domain. Each output pixel depends only on its position and the plan, so the result is bit-identical for any split into chunks. A render reads one frame window at a time, at most 8 Mi pixels, splitting a tile whose window would be larger.

### Exposure, colour and seams

Gains, seams and the blend are solved on the **plan grid**: square cells of the reference's colour-proxy factor at the output's scale, grown until the grid is at most 4,096 cells a side and 2²⁰ cells. A frame covers a cell when all four of its corner pixels lie in the frame, and is eligible there when it also covers the eight neighbours.

- **Photometric fit.** Each cell shared by eligible frames (on a lattice of at most 2¹⁶ cells, at most 4 frames a cell) yields a sample where both frames are unclipped, every channel is above 2⁻¹² and the brightness gradient is at most 0.1 stop a proxy pixel. Each pair with at least 16 samples contributes the median `log2` ratio, weighted by its count up to 4,096. The fit starts from each frame's exposure scale from its settings. A weighted least-squares fit anchored on the reference (gain 1) gives each frame's gain: one a channel for JPEG, one a frame for RAW. A frame whose overlaps give too few samples keeps its exposure scale. Each gain records its RMS residual, its sample count and an uncertainty in stops.
- **Seams.** Frames join the composite from the reference outward, each next the frame sharing the most eligible cells with it. Each join is a minimum s–t cut over the overlap: cells beside the composite alone stay with it, cells beside the new frame alone go to it, and cutting between two cells costs `c(p) + c(q) + 1`. A cell's cost `c` is the mean `log2` colour difference, plus the difference of the two frames' brightness gradients, plus 4 where either sample is clipped, quantized at 1/32 stop and capped at 255. The cut is Boykov and Kolmogorov's algorithm, exact up to 2¹⁵ overlap cells. Larger overlaps are cut coarse to fine: halved recursively, cut, then refined exactly within two coarse cells of the coarse seam. A moving subject is therefore taken whole from one frame.
- **Blending** has two bands split at the plan grid. Detail switches at the seam, through bilinear weights of the four cells around each point. The low band (a Gaussian of σ = 4 cells, reach 12) fades across it as a per-cell correction field. Each output point therefore reads only its frames at its own position and the plan, and a chunk reads bounded windows. Output is never clamped. Each chunk measures its largest overshoot past the frames' own samples and its pixels from clipped samples; the provenance records both for the whole result.
- **Diagnostics** (`StitchDiagnostics`, serializable for provenance) record:
  - the projection and how it was chosen, the field and the scale;
  - the full-scale size, the Boundary Warp amount, the crop, and the covered and lost pixels;
  - per-frame gains, seam lengths between each pair, and each frame's contributing pixels and bounds;
  - an estimate of the pixels taken from clipped samples, and the largest registration residual;
  - the plan grid, the mesh size, the blend's width in pixels and the supersampling.
- **Bounds.** A plan holds at most 160 MiB besides the colour proxies, at the largest grid and 32 frames; with 32 frames' proxies it fits the panorama's 512 MiB scratch limit. A render holds at most 192 MiB besides the chunk it returns. Cancellation is checked at every stage, frame, cut phase, mesh sweep batch, tile and window.

### Boundary Warp and the crop

A stitch's covered region is ragged. The stored result is always a rectangle of covered pixels, because derived sources have no alpha and the renderer's frames are opaque by contract.

- **Boundary Warp 0** (default): crop to the largest axis-aligned rectangle of plan cells whose corner pixels some frame covers. Ties go to the smallest top row, then bottom row, then left column.
- **Boundary Warp 100:** a smooth mesh warp of the covered region onto its bounding rectangle, solved on the plan grid. Mesh vertices lie every 8 cells, at most 128 quads a side. Edge vertices move straight inwards to the region's interior edge within half a mesh spacing, corners move to the nearest interior cell, and the rest of the mesh is the harmonic (membrane) interpolation, relaxed until no vertex moves more than 10⁻⁴ pixel. This is the smoothness term of He, Chang and Sun's rectangling without its line-preserving term, so straight lines bend gently near the edges.
- **Between:** each vertex's displacement is scaled by the amount, then the result is cropped to the largest rectangle the warped region covers. Every amount shares the plan's scale, gains and seams, and `StitchPlan::with_boundary_warp` replans only the warp and crop.
- **Rules.** The warp is part of the one output mapping, so pixels are still resampled once. The plan exposes the amount, the warp mesh, the crop rectangle and the pixels lost for provenance. A crop under a quarter of the bounding rectangle on a side is refused as `InsufficientCoverage`, suggesting Boundary Warp.
- **Preview.** `StitchPlan::preview` draws the whole warped bounding rectangle from the colour proxies, at most 2,048 pixels a side by default, with the same arithmetic as a render. Its overlays, in picture coordinates, are the covered region as row runs, the crop rectangle, and each frame's outline in stack order. Stack order is rows top to bottom, then each row left to right.

### The provider and its evidence

The provider is `{module: "luxforge.panorama", version: 1}`, a lazy merge provider beside `luxforge.hdr`. It owns suitability, registration, projection, photometric fitting, seams, blending and the warp. The core owns admission, resources, publication, stacks, transactions and history, as for HDR. Its pure numerics take no file, catalog or job work.

`cargo test -p luxforge-core panorama_registration` and `cargo test -p luxforge-core panorama_stitch` check the following against an independent f64 reference (`luxforge_reference::panorama`):

- **Scenes.** Synthetic scenes are rendered through known rotations, focal lengths, radial distortion, gains, noise and a moving subject, with 2–32 frames in single and multiple rows, including a 360° row.
- **Registration.** Every corner of every frame is within a declared pixel tolerance. The focal length and radial term are within declared tolerances.
- **Projection and output.** Exact projection mappings and crop rectangles.
- **Photometrics and seams.** Gains are recovered within a declared tolerance. A moving subject appears once.
- **Boundary Warp.** At 0 it equals the crop; at 100 the warp covers at least a declared fraction of the bounding rectangle.
- **Fixtures and determinism.** Complete stitched buffers match the reference within a declared tolerance. Output is bit-identical for any split into output chunks.
- **Coverage.** Every typed refusal is checked, along with cancellation at every stage and the stated bounds.

Registration's tolerances are under [Registration](#registration). The stitch's, for synthetic 320 × 240 frames:

- Layouts and mappings match the reference within 10⁻⁶ pixel, and its crop rectangles exactly, under every projection and Boundary Warp 0.
- A stitched sample is within 0.2% of the scene on average over the crop and 4% at every pixel, relative to the scene value plus 0.02. Blending passes the frames' own samples by at most 0.005.
- Gains are within 1%.
- A moving subject is never doubled.
- At Boundary Warp 100 the crop covers at least 95% of the bounding rectangle.
- A banded cut costs at most 10% more than the exact cut.

`cargo test -p luxforge-core panorama_merge_end_to_end` runs a row of generated JPEG frames through the owner, the shared frame path, registration and the production stitch. The result's bounding rectangle is the reference's layout of the registered geometry its provenance records. Each pixel lies between the values the frames recording it hold there: within 2% on average relative to the lower value plus 0.02 (about two JPEG codes), and within 10% at all but 0.1% of pixels.

## Large derived sources

The owner chose to raise the limits for derived sources rather than scale panoramas down: a derived source may be up to **65,000 pixels a side and 512 MP** (6 GiB of float32 RGB), inside JPEG's 65,535-pixel side. Camera files keep their current limits (16,384 a side, 128 MP, 1.5 GiB decoded), and so does every HDR result, since an HDR result is never larger than its reference frame.

Today every derived source is prepared into one whole-frame `LinearImage`, and the GPU surface uploads it whole: at most 2 × 2 textures of 8,192, charged to the 2 GiB preview budget. Export, catalog previews, samples and the Dehaze light already render through windowed GPU tiles, but they cut their windows from that one allocation. The reference renderer evaluates whole frames, bounded at 512 MiB of output. None of these holds a 512 MP source, so a source past the camera limits becomes a **paged source**.

### Storage

Storage and preparation are implemented (`crates/luxforge-core/src/artifacts/derived_source.rs` and its `paged_source.rs`).

- **Two classes.** A derived source within the camera limits is resident: stored and prepared exactly as before, as every HDR result is. A larger one is paged. Which class a source is follows from its dimensions alone (`SourceClass::of`).
- **Limits.** `layout()`'s one bound is split into declared plane limits (`crate::source::PlaneLimits`): `CAMERA_SOURCE` and `RESIDENT_DERIVED_SOURCE` (16,384 a side, 128 MP, 1.5 GiB of planes, every whole `LinearImage`), `PAGED_DERIVED_SOURCE` (65,000 a side, 512,000,000 pixels, 6 GiB) and `RENDERED_FRAME` (the camera limits, for frames and stages the renderer allocates whole). The chunk side stays 1,024 and each chunk at most 16 MiB; level 0 holds at most 4,096 chunks and a source at most 5,462 over all its levels; a manifest of either format is at most 1 MiB.
- **Manifest format 2** is a paged source's: `{format: 2, width, height, colour, sample, layout, chunk_side, reduction: "area-average-2x2", crop?, levels: [{width, height, chunks: [{x, y, width, height, bytes, sha256}]}]}`, level 0 first. Each further level halves the one before, rounding up, down to the first whose long side is at most 4,096. Each of its pixels is the area average of the one, two or four pixels of the level before that it covers: their sum in binary64, top-left, top-right, bottom-left, bottom-right, from the first, divided by their count and rounded once to binary32. One pixel alone keeps its exact value. Chunk origins are multiples of the even chunk side, so every block lies in one chunk of the level before. The pyramid adds about a third to the source's bytes.
- **Shapes.** Format 1 stays a resident source's and format 2 a paged source's. A format 1 manifest with paged dimensions, a format 2 manifest with resident dimensions, another level count, level size or grid, another reduction, and any unknown field or format are `incompatible` and kept.
- **Writing.** `DerivedSourceWriter::begin_paged` returns a `PagedSourceWrite`. Its `put(ordinal, planes)` takes level-0 chunks as a producer emits them, in any order, each encoded, hashed, staged, synced and renamed at once. `finish` builds each further level from the stored chunks of the level before, each chunk of the next level from the up to four chunks it covers, read back and verified one at a time. It then writes the manifest last and syncs `sources/` once. The writer never holds a level whole, and its scratch stays within 48 MiB. A failure or cancellation leaves no manifest. `write_with` routes paged dimensions through the same writer, so a caller writing chunk by chunk needs no other path.
- **The catalog** records every level's chunks, level 0 first, as the source's chunks (`derived_source_chunks`, at most 5,462). Retention, collection and Empty Removed therefore keep or release every level with its source, as they do a resident source's one grid.

### Preparation

- **No whole frame.** `source.prepare` on a paged source reads and verifies its manifest and its smallest level, the overview, on the source worker (`PreparedSource::Paged`, holding a shared `PagedSource`). It never allocates level 0 whole. The owner stats only a paged source's manifest; its chunks are verified by what reads them.
- **Reading chunks.** Chunks of every level are read on demand through the source's chunk cache, most recently used kept longest, at most 1.5 GiB (`CHUNK_CACHE_BYTES`). Each chunk is hashed when read. A missing or damaged chunk fails only what needed it, naming its level and ordinal (`derived source <id> level <l> chunk <n> is corrupt` or `is missing`), and is not kept.
- **The plane gate** counts bytes as well as leases. A development's planes, a paged source's overview, every chunk its cache keeps and its resident level are all charged to it. The cache grows only while every lease's bytes together stay within 3 GiB: one camera-sized source beside a full cache. Past that it gives up its own least recently used chunks or reads a chunk through without keeping it. A paged source's leases keep the worker waiting, as a resident source's planes do, until the editor lets the source go.
- **Windows and levels.** `PagedSource::window` reads a rectangle of any level, grown by a halo and clamped to the level, assembled from the chunks it touches. A window may be as wide as its level and holds at most 1.5 GiB of planes. `PagedSource::resident_level` reads the largest level within the camera limits whole on first request and holds it like an ordinary source's planes. If the gate would not admit it even with the cache emptied, the read is refused as `resource-limit`.
- **Paths that need a whole frame** — recipe evaluation, renders, samples and rendered catalog previews — refuse a paged source with `resource-limit`, naming it and saying it has no whole frame. They never allocate one. Each reads levels and windows once its path is delivered, as the sections below describe.

### Rendering, samples and export

Implemented. Opening a paged photograph asks the source worker for its resident level (`SourceWork::Level`) before the first preview; the planner then chooses its source by the view (`crates/luxforge-core/src/render/gpu/preview.rs`).

- **Below 100%** (Fit, and the percentage zooms under it), the GPU surface holds the resident level as its source. The stack is planned over the **overview** (`Evaluation::overview`): the resident level as the source, the stage still level 0's, spatial parameters in source pixels scaled to the level, as a reduced proxy is planned. A view below 100% is drawn at most 4,096 pixels a side, and a paged source's resident level is always larger, so the overview is never coarser than the view. The picture at rest, its tiles and the histogram's counts are the overview's. The reference renderer's frame of a paged job is the overview too, always shown reduced, never at full size (`ExactOutcome::overview`). This is a declared approximation of the full-resolution output reduced to the view. It is held to the class's overview limits against the banded reference reduced to the same view (`docs/design/gpu-first.md`, "Tolerance by output kind"). The histogram's tooltips and the Info card say when the counts are the overview's, with its size.
- **At 100% and above**, the boundary is cut from a window of level 0 held beside the resident level (`PhotoSurface::gpu_window`, `crates/luxforge-app/src/app/gpu_paged.rs`). The window is the region the plan reads, grown by 256 pixels on every side and clamped to level 0. It is read off the interface thread through the chunk cache, so a pan or a drag within it reads nothing. A plan that leaves it asks for another window and cancels the read in flight; a stale answer is dropped. Until the window is held, the view's boundary is `boundary-pending` and the reference's frame stands. The pipeline charges the window to the GPU preview budget like any source upload. The window is released when the view returns below 100% or the photograph changes. A light behind the region is computed over the resident level.
- **Samples and Auto tone** read level 0 through windows on the GPU tile worker, a tile's window at a time (`GpuSource::reading`), so a sampled value is the full-resolution value at every zoom. The reference tile service has no whole frame to read, so on a host without the GPU a sample of a paged source is refused with `resource-limit`, naming the source.
- **Export** streams full-resolution GPU tiles from level-0 windows. Each tile's window is read through the chunk cache on its first upload and dropped once on the GPU. A band of tiles wider than the runner's source of two 8,192-texel textures is read in windows side by side (`WINDOW_SIDE`). A band's codes are capped at 256 MiB (`STREAM_BAND_BYTES`): the stream takes the tallest tile side whose band fits (`band_sides`), so a 65,000-pixel-wide band is 1,024 rows high, never 2,048. A chunk that fails to read fails the export with its error. When the GPU cannot stream the stack, or the reference is requested, the export renders in the reference's banded mode.
- **Staged sweeps** whose stage would pass 2 × 8,192 on a side (`SWEEP_STAGE_SIDE_LIMIT`) are drawn chained instead, each tile's chain over its own window. A stack that cannot be chained falls back to the banded reference, naming `tiles-budget`.
- **Catalog previews** render from the resident level.
- **Bounds on large stages.** The Dehaze light's reduction grid scales its factor so the grid stays within its current bound. Presence keeps its declared 10,000-pixel scale cap. The 512-pixel spatial halo bound is unchanged.
- **No whole frame.** Nothing plans or allocates level 0 whole, on the CPU or the GPU. `PreviewSource::whole` refuses a paged source with `resource-limit`, naming it.

### The reference on paged sources

The CPU reference remains the oracle, so it has a **banded mode** (`crates/luxforge-core/src/render/bands.rs`). It evaluates the output stage in full-width bands, each from a window of level 0 with the stack's halos and its exact spatial semantics, and streams the bands to the export encoder, the analysis or a test. The same mode reads any rectangle of the output as bytes or linear values, and reduces the output to a view by the reference's own linear-light area average, band by band.

- **Windows.** Producing a rectangle of the output walks it back through the stack: through each exact geometry, a resample's taps with their margin, a spatial operation's summed halo and a repair's own rectangle. That names the window of level 0 to read and the window of each spatial or repair stage to materialize, and one windowed evaluation builds them. A spatial tile's value does not depend on the rectangle it is cut to, and the last pass writes the whole render's rows over the rectangle, so the output is the whole-frame render byte for byte. A resample whose reads cannot be bounded is refused by name (`resource-limit`); no stack the modules build today has one.
- **First pass.** Whole-stage statistics are computed before any band, in stage order, each from its input stage read in full-width bands through the stack before it. A spatial operation's global estimates, such as the Dehaze light, come from one reduction built band by band, each block summed in the one order every reduction sums in, so the estimate equals the whole frame's bit for bit. A repair (Clone and Heal) evaluates its operations over the read set of every destination once and holds that working set for every band.
- **Reduction factor.** The Dehaze light's reduction is 16-pixel blocks up to about 67 MP; on a larger stage its factor grows in steps of 16 until the grid is within its 262,144-block bound (`estimate_reduction`). The GPU's light link reduces at the same factor.
- **Bounds.** A band is at most 2,048 rows and at most the 512 MiB evaluated-frame limit of output, which bounds a band, not the frame. The windows one band reads and builds (the level-0 window, every stage's frame and the band's output) hold at most 512 MiB; a band past that is cut in two along its longer side until each piece fits. A window may be as wide as a paged source but is otherwise held to the camera limits. Cancellation is read between pieces and bands and by every pass inside one.
- **Parity.** On sources small enough for both modes, the banded output is bit-identical to the whole-frame output for colour, spatial, masked, geometry and repair stacks, at budgets that cut every band into pieces (`reference_bands` tests).

**Export and analysis.** The export of a paged source and `analysis.request` at full resolution use the banded mode. The export encodes each band as it is rendered. The GPU streams a paged export first; the banded mode encodes it when the export asks for the reference, on an owner with no GPU provider, or when the GPU falls back, its result naming the reference renderer with the GPU's reason. The analysis counts each band and adds the counts, giving the report of the whole output without holding it.

Paged sources are qualified with `derived-paged`, a generated derived entry in the GPU corpus of 20,000 × 8,000 (160 MP), through the banded reference: its overview below 100%, its level-0 window at 100%, its samples and its export. No corpus run has measured it yet; its histogram and the minimap's overview are recorded as gaps.

### Memory

A paged source's process cost is bounded by:

- its chunk cache;
- its resident level (at most 1.5 GiB);
- the GPU windows within the existing preview and tile budgets;
- export's band cap.

No component holds level 0 whole. The measurement task reports the real peaks; no total-memory bound is claimed beyond those budgets.

## Generalizing the merge boundary

HDR's boundary was built for one kind. A panorama needs these changes. They also change the [HDR design](hdr-merge.md)'s stored shapes and commands, which describe them.

The kinds, stack members, provenance, catalog format, commands, jobs, lane, ceilings, usage exemption and desktop words below are implemented, and so are the [merge preview](#the-merge-preview)'s commands for both kinds. Both providers are registered (`crates/luxforge-core/src/merge/providers.rs`): schemas list `hdr` and `panorama`, each with its options. The panorama job, its preview and its publication (`crates/luxforge-core/src/merge/panorama/job.rs` and `preview.rs`) run the production stitch through a narrow seam (`stitching.rs`: plan, render a chunk from frame windows read one at a time, draw the preview). A build without a stitch would answer a panorama's provider unavailable on `merge.plan` and refuse a panorama `merge.create`, `merge.rerun` or `merge.preview` as `not-ready` naming `luxforge.panorama`. Browse, `stack.inspect` and `merge.inspect` read a panorama's provenance. The desktop offers panorama merges [in Select](#in-the-desktop).

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
- **Catalog format.** The current catalog format (1) carries these shapes. Other formats are refused; there are no migrations before the first public release. The HDR design's stored-shape paragraphs are updated in the same change.
- **Commands.** `merge.plan`, `merge.create`, `merge.rerun`, `merge.preview` and `merge.preview-update` take `kind` (`hdr` or `panorama`; `merge.plan` requires it; a rerun's is its merge's). Kind-specific options live under `hdr` or `panorama`, and a field of the other kind is refused as `validation`. A panorama's are `panorama: {reference?, projection?: auto|perspective|cylindrical|spherical, boundary_warp?: 0–100}`, the reference named by path; a rerun keeps each recorded option it does not name. Input bounds are per kind (HDR 2–9, panorama 2–32). Refusals are typed per provider, name the frames by file name and carry `kind` in their data.
  - **A panorama's plan** reads headers only: [validating inputs](#validating-inputs) as far as headers tell, no reference unless one is named, and a provisional size (`estimates.provisional`) from the frames' focal length and sensor size (the 35 mm equivalent, or the focal length times the camera's crop factor in the lens profile index) and their count, laid in one row overlapping by a third on a cylinder, with `estimates.scale` when the limits predict a scale below 1. Registration decides the final size.
  - **The match key** of a panorama does not depend on the order the frames are named in, since registration decides the stack's order: two requests for the same frames and options are one merge.
  - **`merge.inspect`** answers a panorama's resolved projection and field of view, and the result's optics ledger: the baked profile's distortion when one was baked, else a derived source's.
- **Jobs.** The job kinds are `hdr-merge` and `panorama-merge`. They share one merge lane: one running merge of either kind and one waiting, with a further request refused as `resource-limit`. Stages are named per kind. A panorama's are `reading` ("Reading 2 of 7", the planned reference first, so its white balance and lens profile develop the rest; each frame's features are detected as it is spooled, and the decoded frames are held to the header rules again), `matching` (registration, then the stitch's plan, which sizes the result), `stitching` ("Stitching 3 of 12", chunk by chunk from bounded windows of the spooled frames, resident or paged by the result's size) and `saving` (the frames' signatures checked again). The result is named "<reference stem> Panorama", takes the reference frame's metadata, and leads a stack of its frames left to right in the result (top to bottom, then left to right, for several rows); the provenance records every frame in that order. Publication is HDR's one transaction.
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
3. Failure injection at every source-publication boundary, retries and concurrent mutation, for panoramas and HDR through the shared lane.
4. A real-owner multi-client comparison of creation into the proposed folder, reuse, refusal by name, mixed-kind claims, rerun with another Boundary Warp, and every desktop request matching the documented one.
5. A background rendered `panorama-merge` journey from generated frames:
   - Merge panorama through the preview sheet, with Boundary Warp changed and the overlays on, and once by ⌥-click;
   - progress;
   - one collapsed panorama tile;
   - its expanded originals;
   - Develop at Fit, at 100% in a far corner, and in an edit with a mask;
   - export of a result above 128 MP;
   - Remove merge, undo and redo with no second merge;
   - a relaunch with the originals moved away.

   The `hdr-merge` journey and existing JPEG and RAW journeys remain valid. The `panorama-merge` scenario (`xtask/src/panorama_merge_smoke.rs`) generates a four-frame row and a ten-frame row of 24 MP JPEGs whose result is about 150 MP. It runs three launches: `select` makes both merges through the menu, the sheet and ⌥-click; `develop` opens the results from the catalog folders they were filed in, edits and exports them; and `offline` reopens them with the frames renamed away. It has not yet been run.
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
