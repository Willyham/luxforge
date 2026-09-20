# Basic adjustments and histogram

Status: proposal for implementation planning. No tools or commands in this document are implemented by this plan. M3/M4 work is owned separately; this proposal does not change their scope, acceptance or status. Product choices below remain proposals until the owner decides.

## Outcome and delivery order

Make the current single-image editor useful for everyday brightness, tone and color corrections, with immediate, trustworthy feedback about the result. Use the supplied Lightroom screenshot as a familiarity reference, not as an instruction to reproduce every visible feature or Adobe's processing.

Recommend starting with the supported SDR sRGB/greyscale JPEG subset. Linearizing a rendered JPEG makes exposure arithmetic meaningful but does not turn it into scene-linear RAW data. PNG, RAW, broad ICC conversion and calibrated display output keep their existing separate scope.

| Slice | User-visible result | Main dependency |
| --- | --- | --- |
| A — inspect and expose | RGB histogram, output clipping indicators/overlays, pixel readout, Exposure slider with live preview and one undo step per gesture | Histogram can use today's raster; exposure needs decimal controls, shared drafts and pointwise float processing |
| B — complete the core Basic controls | Contrast, Highlights, Shadows, Whites, Blacks; Temperature, Tint and neutral picker; Vibrance and Saturation | Selected numerical contracts and the Slice A processing/editing foundation |
| C — candidate next modules | Tone Curve, then Detail and local presence tools | Separate designs and owner priority; not implementation tasks in this plan |

Slice A is independently demonstrable before all of Slice B. It does not wait for export, Locate, MCP, RAW or a library. Final mixed-stack acceptance does require the actual M4 crop implementation. No milestone numbers are assigned here.

## Module boundaries

Propose two linked built-ins, with lazy resources:

| Component | Owns | Does not own |
| --- | --- | --- |
| Basic adjustment tool | Parameters, validation, neutral/no-op rules, declarative controls and compilation of White Balance, Tone and Color processing | History transactions, source decode, job scheduling or catalog writes |
| Histogram inspector | A deterministic RGB reduction, clipping predicates, inspectable result schema and histogram presentation | A recipe effect or an undo entry |
| Shared host additions | Decimal parameter/slider semantics, revision-bound drafts, updating an identified layer, bounded pointwise color execution and analysis jobs | Tool-specific equations or bespoke Basic-only transaction logic |

Basic is one editable recipe effect with internal algorithm units for white balance, exposure, tone and color. This avoids making the result depend on the order in which a person first touches individual Basic sliders. It does not require a new crate or external module loader. The histogram uses the common operation registry and a small analysis interface; do not invent a no-op image layer to fit it into today's effect-only interface.

Proposed internal order: **White Balance → Exposure → tonal curve → Vibrance → Saturation**. The layer itself stays at its saved position among pixel and geometry effects. First non-neutral use appends it; later adjustments replace that layer's payload at the same ID and position in a new immutable snapshot. Never silently hoist it ahead of a crop or move a pixel replacement. Initially create at most one Basic layer per recipe; if an unsupported/imported stack contains multiple candidates, report ambiguity rather than silently choosing one. Whether multiple Basic instances should become user-facing is outside this scope.

Each slice persists only its implemented state and supports only the current Basic payload and algorithm. Breaking changes may replace that shape; update the internal effect format when its interpretation changes and reject unsupported formats explicitly. Do not retain earlier evaluators, conversions or dormant controls.

## Relationship to the work in progress

The inspected checkout contains M3 descriptors and generated actions. They currently have integer, enum and 8-bit color parameters; `ActionPlan` appends a layer; processing has only `ExactGeometry` and `PointReplace`. M4 is expected to extend decimals, drafts, layer replacement and interpolation. Read its delivered interfaces before integration and reuse them where available.

| Work that can be prepared alongside M3/M4 | Integration boundary |
| --- | --- |
| Histogram reducer with hand-counted buffers; clipping predicates | Immutable final raster and render identity |
| Color/exposure reference functions and synthetic fixtures | Declared color stage and point sampler |
| Tone, white-balance and vibrance numerical studies | Module-owned compilation into the host's bounded color operation |
| Descriptors and control behavior on paper | M3 descriptor validation and M4 transaction/draft contract |

Shared files likely to need coordinated integration are `modules/descriptor.rs`, `modules/processing.rs`, `modules/mod.rs`, `render.rs`, `preview.rs`, `editor.rs`, `api/methods.rs` and the desktop's `app/`, `state/` and `view/` layers under `crates/`. Keep algorithm and reducer implementations in dedicated files. One integrator should reconcile shared host changes against the delivered M3/M4 baseline; independent algorithm tasks need not wait for crop UI completion. Do not build a second draft system while crop's is in progress.

## Basic controls and interaction

These are proposed Lightwell ranges and units, not claims of numeric equivalence to Lightroom. Defaults are neutral, zero. Ranges, step, display precision, defaults and descriptions belong in descriptors and API schemas.

| Group/control | Proposed UI range | Required behavior |
| --- | --- | --- |
| Tone / Exposure | −5.00 to +5.00 EV, 0.01 step | Multiply linear-light channels by `2^EV` |
| Tone / Contrast | −100 to +100, step 1 | Change midtone separation with a defined pivot and smooth monotone curve |
| Tone / Highlights, Shadows | −100 to +100, step 1 | Smoothly change bright/dark ranges while retaining ordering and exposing retained detail |
| Tone / Whites, Blacks | −100 to +100, step 1 | Control bright/dark endpoints separately from broad highlight/shadow shaping |
| White Balance / Temperature, Tint | −100 to +100, step 1 | Relative warm/cool and green/magenta correction of rendered JPEGs; not Kelvin |
| Color / Vibrance, Saturation | −100 to +100, step 1 | Chroma-dependent versus uniform color-intensity adjustment; saturation −100 is neutral grayscale |

Each slider has editable numeric text, keyboard steps, an accessible name/value/unit, and an explicit reset action. Double-click reset can be an additional shortcut. Group reset and Reset Basic preserve other groups/effects as appropriate and commit once. Reset on a neutral layer is a no-op; resetting an existing layer retains its ID with neutral parameters. No hidden group-enable/bypass feature is required.

Propose slider release as the normal commit boundary, matching common photo-editor interaction. Pointer-down begins a revision-bound draft, pointer updates replace its settings and preview request, and release commits once. Escape cancels; numeric Enter commits once; an arrow-key hold is one gesture ending on key-up. Focus loss cancels an unfinished gesture rather than committing an intermediate value. Invalid text stays editable with a useful error and commits nothing. A gesture that returns to its start is a no-op.

Reuse the core draft primitives supplied for crop, with Basic-specific commit timing. Independent clients can begin, set, inspect, cancel and commit drafts semantically without pointer simulation. Keep at most one active tool draft per client/asset; changing tools with a draft requires explicit commit or discard, not silent replacement.

An external commit preserves the draft and marks conflict. Discard or explicit Reapply is required. Reapply preserves the user's changed fields, merges them onto the newest compatible Basic state and revalidates geometry/picker inputs; it must not overwrite unrelated fields edited by another client. Undo/redo, Restore and history preview follow the same conflict rules. Historical previews remain read-only, and their values and histogram remain attached to the selected entry.

## Numerical and color contract

### Pointwise foundation and exposure

- Input is the existing verified, oriented sRGB/greyscale JPEG decode. Preserve source/profile rejection behavior. Do not silently reinterpret Adobe RGB, Display P3 or malformed ICC inputs as sRGB.
- Decode the sRGB transfer function into float linear sRGB, D65, apply the ordered operations, then encode and quantize at an explicitly declared output boundary. Use standard sRGB transfer constants; the [W3C conversion reference](https://www.w3.org/TR/css-color-4/#color-conversion-code) is an independent reference for conversion tests, not a dependency or an adopted CSS rendering pipeline.
- Exposure is `out = in * 2^EV`. Preserve finite values outside `[0,1]` between color operations so a later operation can bring them back. For the initial SDR output, explicitly clamp at the output boundary, encode, and round nonnegative codes using `floor(255 * encoded + 0.5)`. Do not clip/quantize after each slider or transient preview.
- Preserve alpha. The supported JPEG path is opaque. Identity/no-color stacks retain the exact existing byte path and source sharing; zero-valued Basic must not introduce round-trip changes. Pixel replacements enter at their original recipe position as encoded sRGB values converted to the working domain only when required.
- Specify and test finite bounds for compiled coefficients, layer depth, scratch and intermediate values. Overflow/non-finite results are explicit errors, not NaNs uploaded to the GPU or silently clipped intermediate state.
- Match a separate f64 stepwise reference: proposed float tolerance `1e-6 + 1e-6 * abs(reference)` for supported non-clipped exposure/conversion cases, with at most one output code of rounding difference where permitted. Identity, current geometry/pixel fixtures and histogram counts remain exact. Freeze additional per-algorithm tolerances before implementation.

Geometry/color operations are ordered, not globally commutative. Nonlinear tonal processing before interpolation can differ from processing after it. M4's interpolation domain and output boundaries must be explicit; preserve its accepted behavior and prove mixed stacks against a stepwise reference. Fuse only transformations with equivalent results under the established tolerance. Do not add a whole-image float buffer to get around that requirement.

Point sampling and the future neutral picker use the same compiled evaluation as rendering without rasterizing a frame. A bounded crop filter may require a fixed local neighborhood; document its cost instead of claiming every interpolated query has the old exact-geometry cost. The initial Basic algorithms are pointwise and add no image-sized analysis to a sample or no-op check.

### Tone contract to prove before shipping

Recommend a deterministic, pointwise luminance-based family for the first Tone controls. Start from linear-sRGB luminance with documented coefficients, use smooth overlapping tonal weights, and reconstruct RGB with explicit handling near black and outside gamut. Contrast has a fixed documented pivot; endpoint adjustments have an explicit crossing-prevention rule. Define the complete parameter-to-curve equations, internal order, extrapolation beyond white/below black and zero-luminance behavior in the numerical task before writing production controls.

Prove identity, finite output, nondecreasing neutral-ramp response over all allowed parameter combinations, smooth transitions, no dark/bright range inversion, and distinct broad-range versus endpoint effects. Use equal-luminance patches in different surroundings to prove this first algorithm is global. Reference photos must show usable shadow/highlight adjustments without obvious posterization, hue shifts or loss of local contrast. If the pointwise proposal cannot meet the visual acceptance, return the algorithm/scope decision to the owner; do not silently introduce an edge-aware local stage or call a weak brightness offset finished.

These controls reshape retained information. A clipped JPEG plateau cannot regain missing detail. Adobe describes its tone controls as image-adaptive; its formulas and numeric behavior are not this contract. [Adobe tone procedure](https://helpx.adobe.com/lightroom-classic/desktop/help/tone-control-adjustment.html)

### White balance and neutral picker

Recommend a documented relative chromatic-adaptation or channel-gain transform with luminance normalization, not multiplying encoded bytes. The numerical task selects one, fixes its matrices/gain mapping and produces warm/cool, green/magenta, near-black and saturated-patch references. Zero/zero means the JPEG's existing rendering, shown as Original; do not imply the camera's RAW white balance is recoverable. A custom value shows Custom.

The neutral picker samples a bounded patch (proposed 5×5 at input-stage pixel centers, clipped at image edges), evaluates before the target Basic layer, and computes reproducible parameters. Store the resulting numeric settings in the recipe; the sampled position/statistics may be action metadata, not a dependency on a future screen image. Map displayed coordinates back through subsequent geometry. Reject near-black, clipped or non-invertible/invalid samples with an explanation; do not guess. If the relative axes cannot represent the correction within range, report it rather than silently clamping. Freeze averaging, rejection thresholds and solver tolerance in the numerical task. UI and API use the same sample coordinates and solver; cancel commits nothing.

### Saturation and vibrance

Select a documented perceptual chroma space and fixed gamut policy in the numerical task; evaluate Oklab as the first candidate. Saturation scales chroma while preserving its achromatic axis; Vibrance varies gain with existing chroma, with smooth hue weighting if skin-like hue protection is selected. Skin-like weighting is a color heuristic, not face detection or a promise about every skin tone. Define negative-value behavior, order, neutral/black cases and gamut handling explicitly, and verify neutral preservation, saturation −100 grayscale, monotone response and varied portrait/saturated-color fixtures. No ML resource is needed.

## Histogram and clipping contract

The inspector describes the **rendered SDR sRGB output of the full current composition**, after crop and edits, before UI overlays, scaling for the viewport or monitor conversion. It is not the camera/RAW histogram. Panning, zoom and display scale do not change its population. Label this domain in the UI/API so users do not read output endpoint counts as evidence of sensor clipping or recoverable detail.

- Three arrays of 256 unsigned 64-bit counts; each output byte selects its exact channel bin. Every channel sum equals the full output pixel count. Use integer reductions with deterministic merging. An overlapped gray area means overlapping RGB counts, not a separate luminance histogram.
- Plot filled RGB channels with a shared linear vertical scale and visible overlap. Counts returned by the API stay raw; presentation normalization must not change their meaning. Empty, pending, unavailable, superseded and failed are explicit states. A stale result may remain only with a visible stale label.
- Shadow/highlight counters report per-channel endpoints (`code == 0`, `code == 255`) plus any-channel and all-channel pixel counts. Endpoints include values quantized to those codes; these are output clipping warnings, not an inference about the original capture. A colored indicator identifies channels with endpoint pixels; add textual counts for accessibility.
- Clicking a triangle toggles its overlay; hover may preview it. Proposed mask rule: any-channel endpoint, blue for shadow, red for highlight; a pixel matching both uses magenta. Tooltips state this rule. Masks never alter the raster, saved recipe, histogram population or future export. UI and API share the predicate.
- RGB hover readout uses the compiled sample at final image coordinates, reports 0–255 codes, and includes the selected render identity. The histogram computation itself is a full-image worker operation, not a repeated owner-thread point query.
- Key each result to asset/source fingerprint, entry/snapshot, effective recipe identity, client draft ID/revision when present, output dimensions and color contract. Carry render generation with delivery. Counts and overlays must match the image currently presented, including drafts and history preview, not simply the newest catalog revision.

First implementation uses exact full-resolution counts. Reuse the final raster allocation, reduce during its production where practical, or scan it on a worker without a second render/copy. Exposure or view-only changes must not cause duplicate source decoding. During active gestures the previous histogram can be marked updating while an exact replacement is pending; do not secretly switch to thumbnail counts that miss single-pixel clipping. Approximate draft analysis is a later measured proposal, not the default contract.

Do not allocate a full-resolution mask: derive overlay values from the same final samples, then reduce/upload only the bounded display overlay. A Fit overlay must retain any clipped source sample contributing to a display cell rather than test a blurred thumbnail. 100% overlays use exact source-detail coordinates. Count and overlay tests cover isolated clipped pixels, crop edges and both-endpoint pixels.

## Shared API and jobs

The following are required capabilities, **not existing command names**. Final method names and schemas are chosen with the M3/M4 integration, avoiding duplicate APIs.

| Capability | Required semantics |
| --- | --- |
| Inspect Basic | Effective parameter values, layer ID/position, payload format and availability for an explicit current or historical snapshot |
| Set Basic fields | Atomic field patch with expected revision, request ID and actor; omitted fields preserved, unknown fields rejected; one history entry or no-op |
| Draft lifecycle | Begin/set/read/cancel/commit/reapply through the core, explicit draft/base revision and conflict state |
| Reset field/group/Basic | Neutral parameters applied by the same transaction path; unrelated effects unchanged |
| Neutral sample | Image-space point/patch and explicit source stage; returns validated settings or a structured error |
| Request/read/cancel histogram | Frozen current, history or caller-owned draft target; job/result identity, readiness, exactness, domain, counts and structured errors |
| Overlay settings/readout | Per-client view state and semantic sampling; no history mutation |

All capabilities appear in discovery and work from an independent JSON client; MCP later inherits them. Read-only analysis must work without a GUI and cannot require switching the GUI selection. Requests return promptly; full-frame work never runs on the catalog owner. Reuse shared preview evaluation where targets coincide. Bound analysis to one active plus one replaceable pending job globally; report superseded requests explicitly and avoid a pending queue per slider event. Shared work is reference-counted so one client's cancel/disconnect does not invalidate another's result. Cap completed small reports/handles to the eight live clients; retain no per-result raster. Test competing clients and document scheduling fairness.

## Resource and responsiveness constraints

Preserve the current 64 MP, 16384 px/side, 512 MiB evaluated-frame and source-size limits. Float color arithmetic should stream per pixel or use bounded rows/tiles: a 60 MP float RGBA frame alone would exceed the current frame budget. Proposed new scratch limit is 64 MiB aggregate across active work, with reservations before allocation; do not raise existing limits without a measured decision. Source and byte raster remain shared `Arc` buffers. A histogram report is bounded to 16 KiB of counters/metadata before protocol encoding; worker-local bins are charged to scratch. Display overlays fit existing upload limits and the aggregate memory accounting.

Use the shared Rayon pool above a measured threshold. Registration/hiding an unused tool starts no worker and allocates no processing resources. Cancellation is checked between bounded chunks; stale frames/results are rejected even if computation finishes. No new idle polling is needed; retain existing event-sync behavior and gate completion polling on actual jobs.

Measure release builds on 24 MP and 60 MP, source cache cold/warm separately, with at least 30 samples. Separate algorithm, histogram, scheduling, GPU upload and presented-frame latency. Proposed targets: warm 24 MP slider-to-presented-frame p95 below 100 ms, settled exact histogram p95 below 200 ms after the final input, and existing memory/idle hypotheses from [performance](../specs/performance.md). These targets require owner acceptance after measurement; they are not current performance claims. Record 60 MP tails and peak RSS/GPU/scratch even if targets are missed. Do not add approximate processing, another cache or a timer just to claim a pass.

Every implementation handoff answers the [performance checklist](../engineering/performance-rules.md): verified-source reads; allocation bounds/sharing; no frame work in samples, validation or owner handlers; narrow UI refreshes; timers; 24/60 MP measurements; reference-buffer and buffer-sharing evidence.

## Verification and acceptance

1. Hand-counted tiny histograms: all-black/white, RGB primaries, uniform gray, ramps, single clipped pixels, both-endpoint pixels and cropped-away clipping. Sums, channel/any/all counts and overlay predicates are exact.
2. Independent f64 reference color evaluation and lossless expected buffers: identity, ±EV, inverse non-clipped exposure, gradients, step wedges, saturated and portrait-like colors, order around pixel replacement and exact/interpolated geometry. Frozen tolerances precede production implementation.
3. Complete history journey: gesture commit/cancel/reset, repeated values, retry/deduplication, undo/redo, preview/restore, reopen, failed write, unavailable provider, source missing/changed and current-format fixtures. Originals remain byte-identical.
4. Two-client races: conflict during drag/picker, explicit field-preserving Reapply, selected historical entry during external edits, analysis cancellation/reconnect and superseded results. No frame/histogram identity mismatch and no unexpected history entries.
5. Native M4 rendered inspection at Fit, 100% and available display scales: sliders, keyboard/numeric editing, focus, reset, clipping overlays, histogram overlap/readout, crop and photo changes. Correlate captures with snapshot/revision/draft/render generation and logs. Inspect smooth gradients, saturated colors, backlit portraits and noisy shadows. Record calibrated-color and screen-reader limitations honestly.
6. Extend existing acceptance/performance harnesses for these paths; run `cargo xtask check` on each implementation handoff and release diagnostics on the final editor. Headless/VM results are distinct from native rendered evidence. No performance claim follows from small fixtures alone.
7. Update current module/color contracts, feature status and user guide only for demonstrated behavior. If export is implemented concurrently, validate it uses the same evaluator/output contract; this plan does not own export encoding or metadata.

## Later module candidates

| Candidate | Useful next scope | Why separate |
| --- | --- | --- |
| Tone Curve | Monotone point curve, composite first; numeric point API and reset | Reuses pointwise color stage but needs curve interaction and interpolation contracts |
| Detail | Sharpening, then noise reduction, judged at 100% | Requires scale, neighborhood halos and noise/detail quality evidence |
| Texture / Clarity | Fine/mid-scale local contrast | Requires bounded multiscale processing and halo/edge tests; cannot be relabeled global Contrast |
| Dehaze | Global atmospheric-haze correction with explicit limits | Requires a selected estimation model, color/noise review and photo-sized analysis budget |
| B&W / Color Mixer | Explicit grayscale treatment and sampled/ranged color control | Additional interaction and color contracts; saturation −100 alone is not a B&W mixer |

Auto Tone, profiles/presets, HDR, local masks, healing, red-eye and screenshot metadata rows are not selected for the first two slices. No disabled placeholders for them. Histogram dragging can follow once tone control semantics are proven; first ship the histogram as feedback. The existing export/Locate/MCP commitments keep their own priorities.

## Open decisions

| Decision | Recommendation | Effect if changed |
| --- | --- | --- |
| JPEG now versus RAW prerequisite | Deliver on existing JPEG support | RAW first changes input/color contracts and substantially expands the dependency chain |
| Editable Basic organization | One layer with fixed internal group order, stable position among other effects | Separate tool layers need an explicit ordering and targeting UX |
| Commit timing | Slider release/key-up/Enter; Escape cancels | An Apply/Cancel panel is possible, but changes draft lifetime and keyboard interaction |
| First highlights/shadows quality scope | Prove a global pointwise curve before considering edge-aware processing | Local/adaptive behavior adds analysis, tiles/halos and larger performance work |
| Numerical limits and visual quality | Confirm ranges and select equations using the reference studies | Do not declare formulas or perceptual tolerances accepted merely because a candidate was documented |
| Relative priority | Demonstrate Slice A, then finish Slice B | Owner may continue export or RAW/library work before Slice B |

The task plan starts with resolving these product/integration choices. Independent fixture/reference preparation is runnable while choices are reviewed. Planning authorization is not implementation authorization.

## References

- Repository contracts: [modules](modules-and-api.md), [history](../specs/edit-history.md), [crop](../specs/single-image.md), [architecture](architecture.md), [performance rules](../engineering/performance-rules.md).
- Local research: [Lightroom tone/color](../research/lightroom/tone-and-color-tools.md), [rendering/color](../research/lightroom/rendering-and-color.md), [darktable algorithms](../research/darktable/tone-and-color-tools.md). These supply context, not accepted Lightwell behavior.
- Adobe describes Basic controls, relative JPEG temperature and RGB histogram/clipping interactions in [Image tone and color](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/image-tone-color.html). Familiar labels do not establish implementation equivalence.
