# Detail

Status: **planned, not implemented or photographically qualified.** The owner delegated the choices in this planning request on 2026-09-30; these are the selected design defaults, with no owner-review gate. This request does not authorize implementation. [Task plan](../../tasks/detail.json) · [decisions](../decisions.md#detail).

The planned panel is drawn on the [Detail board](develop-workspace/detail.png) (at 100%) and in its neutral, masked and Fit states on the [planned module panels](develop-workspace/planned-module-panels.png); [its own render](develop-workspace/modules/detail.png) shows the adjusted section. The boards are design references, not evidence of anything built.

## Outcome and scope

One small Detail module supplies manual capture sharpening and luminance/colour noise reduction on every supported JPEG and developed RAW source, globally or through existing masks. It keeps source bytes, continuous RAW editing, immutable history and UI/API parity. Lightroom informs familiar controls and 100% inspection, not equations or value equivalence ([Adobe's Detail guidance](https://helpx.adobe.com/in/lightroom-classic/desktop/process-and-develop-photos/retouch-photos.html)); the repository's [Lightroom](../research/lightroom/detail-and-local-contrast.md) and [darktable](../research/darktable/raw-and-denoise.md) research inform the alternatives.

This is **capture sharpening**: modest restoration of apparent edge definition at the developed image's pixel scale. It cannot recover missing information or undo arbitrary blur. Export renders this recipe once, including Detail. **Output sharpening** would be a separate export operation after destination resizing, tuned to output size and medium; it is out of scope, as are export resizing, AI denoise, sensor-domain denoise, camera/ISO noise profiles, deconvolution, automatic defaults, hot-pixel repair and a separate detail-preview pane. No new providers, capabilities, model downloads, dependencies or placeholder controls are selected.

## Decisions and rationale

| Choice | Selected default and reason |
| --- | --- |
| Processing order | Noise reduction, then capture sharpening, before Basic and Presence. Avoid enhancing noise before reducing it, and keep denoise response independent of later exposure/tone settings. Keep Basic's composite intact. |
| Host stage | Add `restoration`, a placement stage using the existing `Spatial` primitive. A new small stage expresses pre-tone work without moving Presence, overloading panel order or building a processing graph. |
| RAW/JPEG defaults | All three strengths start at zero on both kinds; no automatic layer at import. Preserve Original and avoid sharpening a JPEG that may already be sharpened. Ancillary defaults make the first deliberate adjustment useful. |
| Algorithms | Bounded multiscale wavelet shrinkage plus thresholded, edge-gated unsharp masking. Deterministic CPU kernels fit the spatial host; a fixed support is easier to bound and verify than patch-search denoise or iterative deconvolution. |
| Fit | Show the effect, with an approximate moving preview and a settled downsample of the exact processed frame. Permanently hiding Detail at Fit would make colour blotches and final composition misleading. |
| Masks and presets | Reuse host targets, spatial blending and field-patch presets; keep one layer per target and no module-owned selection/history. |
| Numerical work | Freeze equations, coefficient mappings and quality fixtures before production kernels. This is engineering qualification within the selected design, not an unanswered product decision. |

## Placement and pixel contract

For newly placed layers, the intended regions are:

```text
prepared source → pixel edits → restoration (Detail) → colour (Basic, mixer)
                → spatial (Presence) → geometry (orientation, crop) → finish (vignette)
```

RAW preparation still includes sensor white balance, demosaic, colour conversion and required DNG corrections before Detail. JPEG is already rendered input: Detail runs before Luxforge's relative white balance, exposure and tone but cannot undo camera processing, clipping or compression. This asymmetry is intentional; moving JPEG white balance ahead would split Basic or change existing semantics. RAW white-balance commits invalidate dependent renders through development identity; Detail edits never redevelop the mosaic.

Add `EffectStage::Restoration` (serialized `restoration`), order 0 for Detail. Placement puts new pixel edits before restoration and new colour edits after it; a leading RAW source layer remains index zero. Keep the existing relative order of pixel/colour layers in stacks without restoration. Insert restoration before the first colour/spatial/geometry/finish layer, after any leading source/pixel prefix. If an existing pixel proof is interleaved after colour, it stays there; do not move it to manufacture the nominal order. Existing layers never move on an ordinary edit: stored noncanonical orders still evaluate in stored order under the current host contract. Test insertion with every creation order, masked targets and presets. Do not silently reorder historical stacks or infer processing order from panel order.

Compile one Detail layer into at most two spatial units: denoise then sharpen, omitting inactive units. Zero strengths compile to nothing even if ancillary settings differ; unchanged settings are a history no-op, while changed ancillary settings are retained as ordinary recipe data. A reset keeps an existing neutral layer and its ID. No additional quantization between the two units: JPEG uses the spatial boundary's existing sRGB decode/encode and byte output, RAW retains signed float linear-sRGB planes until terminal output. No full-frame opponent-colour image or intermediate RAW clipping is allowed. Reuse the host's finite-value checks and reject unsupported current payload formats without rewriting data.

**Pre-tone sampling is a real host change.** Basic's JPEG neutral picker now reads through a spatial prefix. Extend the bounded point/query worker to evaluate its complete 5 × 5 patch with one tile cache against an immutable evaluation, off the catalog owner; its answer and subsequent apply must retain revision/entry identity and reject a stale apply. RAW's global sensor picker remains sensor-based. The same audit covers masked Basic queries, range-mask input sampling and overlays. Validation, field patches and no-op detection remain pixel-free. Do not obtain responsiveness by returning a pre-Detail pixel as a post-Detail sample.

## Algorithms and controls

The numerical task writes a short study and an independent `luxforge-reference` implementation before production code. Selected baseline:

- Convert each tile's linear RGB to the project's Oklab domain with signed cube roots and the inverse, retaining extended values. Denoise its lightness and chroma separately; sharpening changes lightness only. Freeze near-black and saturated-colour behaviour against the existing conversion, without an intermediate gamut clamp. This is a perceptual filter inside a scene-linear pipeline, not a calibrated sensor-noise model.
- Denoise uses three undecimated separable B-spline levels, kernel `[1,4,6,4,1]/16` at spacings 1, 2 and 4 full-resolution pixels. Shrink lightness residuals and chroma-vector magnitudes smoothly, reconstructing from the retained coarse plane and modified bands. Noise strengths set thresholds; each Detail control reduces suppression of structure. Freeze the per-band threshold curves and bounded local edge protection in the study. No image-wide noise estimate, ISO lookup or viewport-dependent statistics. The decomposition's radius is 14 px; the complete denoise unit, including local protection, must declare its actual support, capped at 24 px.
- Sharpen uses a separable Gaussian (sigma = Radius, support `ceil(3 × sigma)`) and a soft-thresholded lightness residual, scaled by Amount. Detail lowers the residual threshold to admit finer structure; Masking gates that residual toward stronger edges using a bounded local guide. Include a smooth overshoot limiter, frozen in the study. It must preserve constant patches and must not sharpen chroma independently. The entire sharpening unit, including guide and limiter, is capped at 16 px of halo. This is unsharp masking, not deconvolution.
- Use edge clamping at true image edges, full halos at tile/region/crop cuts and fixed arithmetic order. No tile-local noise estimator or periodic renormalization that changes with the viewport. The operation's combined halo is at most 40 px, below the host's 512 px ceiling; supporting larger radii or more scales needs a separate design change.

Proposed fields of the generated `edit.set-detail` patch (not available yet):

| Group / label | Field | Range / step | Default | Meaning |
| --- | --- | --- | --- | --- |
| Sharpening / Amount | `sharpening` | 0–150 / 1 | 0 | Capture sharpening gain; 0 is off |
| Sharpening / Radius | `radius` | 0.5–3.0 px / 0.1 | 1.0 | Full-resolution developed-content pixel sigma |
| Sharpening / Detail | `sharpen-detail` | 0–100 / 1 | 25 | Fine-residual retention, with increased noise risk |
| Sharpening / Masking | `sharpen-masking` | 0–100 / 1 | 0 | 0 admits all edges; 100 favours the strongest |
| Noise reduction / Luminance | `luminance` | 0–100 / 1 | 0 | Lightness-noise suppression; 0 is off |
| Noise reduction / Luminance detail | `luminance-detail` | 0–100 / 1 | 50 | Higher preserves more structure and noise |
| Noise reduction / Colour | `colour` | 0–100 / 1 | 0 | Chroma-noise suppression; 0 is off |
| Noise reduction / Colour detail | `colour-detail` | 0–100 / 1 | 50 | Higher protects fine colour boundaries |

The panel is **Detail**, collapsed initially, after Basic and before Presence; Sharpening precedes Noise reduction visually, independent of processing order. Use existing number controls, per-control/group/module resets, scope chip, keyboard/numeric editing and Copy as JSON. Label Masking as sharpening edge protection, distinct from the Masks panel. A short hint says “Judge fine detail at 100%”; use the existing zoom control, with no new viewport. Keep ancillary fields editable while a strength is zero, explaining that they take effect when enabled. No Alt-drag special preview in v0: Option already changes numeric step.

Descriptor: `luxforge.detail`, effect `luxforge.detail.adjust`, format 1, `sources: [jpeg, raw]`, `single_layer: true`, `maskable: true`; proposed actions `set-detail` (`patch: true`) and `reset-detail`. The field table is the one source for defaults, validation, controls and schema. Missing fields mean declared defaults; canonical all-default payload is `{}`. Register through the common assembly, with no I/O or eager pixel resources. Availability, unsupported formats, duplicate layers per target and invalid fields follow existing explicit refusals. Update the generated descriptor snapshot when implemented.

## Fit and percentage previews

Radius and wavelet spacing refer to full-resolution content pixels, unaffected by crop, zoom or display density. The current `compile(..., Stage)` and stage-based proxy eligibility cannot express this by themselves. Extend the common compilation context with the full-resolution stage, evaluated stage and per-axis sampling scale; compile exact at 1:1 and proxy with those same source-space supports scaled into its grid. Fractional proxy taps, normalization and halo rounding must be frozen by the numerical study, including supports below one proxy pixel. Do not force a minimum one-pixel *effect* radius that makes a Fit edge far wider than its full-resolution counterpart.

Extend the descriptor's preview contract with a generic opt-in **settled Fit from exact** policy, requested by Detail, and aggregate it only for active compiled operations. Admit `restoration` through the registry's proxy-eligibility rule, retaining explicit refusals/fallbacks for unsupported stacks. All effects still run during motion: Detail applies scaled kernels to the display-bounded source proxy (half-scale visible region at 100% and above). This is an approximation because denoising/sharpening and reduction do not commute; report a Detail approximation reason with the existing phase/generation metadata. Do not claim a noise threshold calibrated at full resolution remains exact on averaged pixels, and do not silently bypass Detail.

After 120 ms quiet or release, render the exact full frame already needed for whole-image analysis, downsample its **final rendered output** with the shared linear-light area reducer to physical Fit bounds and present that bounded raster. This explicitly changes today's Fit settlement, which retains the exact raster without uploading it. Settle every active Detail stack this way, including preset/history/Compare, other sliders and mask edits, zoom-to-Fit and resize. At 100% and above, refine the exact visible region first, then whole-image analysis as today. A scale of 1 uses exact pixels directly. An inactive Detail layer leaves the existing preview policy unchanged.

Tag the settled Fit raster as derived from exact processing, with display reduction distinguished from an effect approximation. Draft RAW-WB approximation must remain marked even after a full-resolution draft render. Counts stay exact whole-image counts, marked updating until their matching completion. Clipping and mask overlays must carry matching content/view/quality identity; late settlement must never replace a newer gesture, crop, photo or entry. Cache identity includes source/development, effective recipe and mask identity, draft revision, full stage, scale/window and view bounds. Reuse the existing latest-job queue, quiet timer, texture retirement and display caps; no periodic refinement loop or second full-frame clone.

Prove windowed/whole and region/full equality with multiple spatial segments, Detail before Presence and Dehaze's whole-stage estimate in particular. Preserve explicit fallbacks where cutting is unsupported; extend the shared window planner only where exactness is proved. A fallback is observable and measured, never evidence of a passing region latency check.

## Masks, presets and history

Every control is available on the global target or an existing mask via the host's optional `mask` field. The host blends the completed Detail result against that layer's input in linear light: `out = in + M × (detail(in) − in)`. Halos read unmasked neighbours; masking limits the write, not the neighbourhood. Zero coverage returns input exactly under the host's existing finite/signed-zero rules; full coverage equals unmasked processing. No full-resolution mask buffer or stored edge-mask artifact. Sharpening Masking is a transient local guide multiplied into sharpening only, never a host mask.

Global Detail precedes masked Detail; masked instances follow the mask list and reuse `mask.reorder`. Each instance denoises then sharpens its own input, so overlapping masks can process detail repeatedly; they do not erase global processing. Range masks read each operation's input, before that operation's effect, and may select differently at Detail than at Basic. The overlay keeps the existing first-bound-layer input rule and explicit refusal when a pixel-dependent overlay cannot be evaluated within its cost contract. Adding an earlier Detail target must invalidate that input identity, never create a circular dependency or silently display coverage from another stage.

Native preset capture/apply/export/import gains the patch automatically through discovery, including ancillary fields and zero-valued strengths. Presets address only global settings and retain masks untouched. Keep Lightroom Detail keys explicitly unsupported with per-setting reports in this slice: familiar ranges do not establish equivalent thresholds/radii. No approximate mapping or silent clamping; native round trips are the parity contract.

One gesture commits one attributed action on release/key-up/Enter; Escape/focus loss cancels, no-op gestures add no row, and external changes retain the draft with Discard/Reapply. Resets, preset application, mask targeting and history labels use the same action path. Verify request deduplication, revisions, undo/redo, historical preview, Restore, named versions and reopen. Original has no Detail effect. Missing/disabled Detail must retain recipes/history and refuse affected render, analysis, sample and export, not omit processing.

## Bounded implementation

The module owns equations, fields and support declarations; the host owns placement, compilation context, workers, memory, masks and preview presentation. Reuse `SpatialUnit`, the generic byte/linear pipeline and shared Rayon gate. No new rendering pipeline, per-tool executor or capabilities infrastructure.

| Performance checklist | Required answer before implementation handoff |
| --- | --- |
| Source I/O | Only the verified prepared-source path reads/hashes/decodes; warm Detail edits reuse source/development. Prove with counters. |
| Frames and scratch | At most the existing two segment frames (512 MiB each for byte output, 1.5 GiB each for RAW planes), in addition to retained sources; no planes per history entry or per scale at frame size. Tile planes, bands, masks and scratch use checked size arithmetic and reserve-before-allocate under the shared 256 MiB spatial target. Declare peak live scratch, not only outputs. |
| Points and owner | Samples/patches use bounded tile caches on the point worker, never a frame. Owner work is metadata/recipe compilation only; test another client while a query is held. Chained masked layers and cold Dehaze estimates are explicit cost cases. |
| Scheduling | One active/one replaceable pending preview; cancellation per row/pass or bounded chunk, including wavelet levels, reducer and upload preparation. Respect disconnect and job cancellation; release scratch on error/panic/cancel. |
| UI refresh and idle | Existing narrow mutation completion; one extra display-bounded upload at Fit settlement only when required. No history-page reload, new permanent timer or polling. |
| Limits versus targets | Halo/unit/source/frame/queue limits refuse explicitly; shared scratch targets reduce concurrency down to one tile, never refuse merely because another evaluation holds the target. Keep the aggregate photo-texture caps unchanged. |
| Evidence | Exact independent fixtures, buffer-sharing/no-work neutral tests, and finished-build 24/60 MP before/after measurements. Separate RSS, retained sources, CPU scratch, GPU residency and unmeasured backend staging; no total-memory guarantee inferred from per-buffer caps. |

## Verification and completion

The [task plan](../../tasks/detail.json) carries the executable acceptance detail. No product question remains as a prerequisite; coefficients, photographic coverage and measured cost are unqualified outputs of that work.

1. **Independent numerics before production.** Freeze equations, mappings, proxy taps, support/scratch formulae and tolerances in a short study, backed by `luxforge-reference` without production imports. Start at `1e-6 + 1e-6 × |reference|` and one output code at declared rounding boundaries; justify any adjustment before implementation. Identity/constants, sample/full, region/full and serial/pool equality are exact checks. Include tiny/odd images, borders/seams, impulses, slanted edges, gratings, neutral/saturated ramps, negative/HDR values, signal-dependent/chroma noise, JPEG blocks and masks.
2. **Photographic quality.** Hash and document provenance/settings for low/high-ISO skin, hair/fur, foliage/fabric, skies/shadows, coloured edges and repeating texture: authentic Z6 Bayer, X100VI X-Trans, Air 2S DNG and photographic JPEGs. Use paired clean/noisy captures where available and deterministic noise on clean references otherwise. At frozen moderate settings, designated synthetic gates require at least 25% flat-patch RMS noise reduction with less than one-code mean drift, and at least 10% sharpening MTF50 improvement with overshoot/undershoot at most 5% of edge contrast. These are provisional study gates, not universal photographic guarantees. Inspect 100% crops and final-size reductions for waxy skin, worms, halos, colour bleeding/shifts, ringing, moiré and compression amplification. Missing scenes or unresolved material defects prevent a quality-complete claim; tune within the design without an owner-review gate.
3. **Parity and real rendered evidence.** Independent JSON and generated controls must yield identical settings, recipes, errors, history and pixels. Cover masks, presets, conflicts, source preservation, provider loss and fresh-owner reopen. Native M4 background captures exercise Fit motion/settlement, 100%/200% refinement/pan, 1×/2× density, geometry, Compare and neutral picking. Correlate entry/snapshot, revision, draft, render generation, view, approximation reason and logs; reject stale work and false exact labels.
4. **Finished-build performance.** Measure release startup/discovery/first-use, cold/warm Fit, exact settlement/viewport/full processing, samples/picker, export and sustained slider/mask/pan/cancellation workloads. Compare equivalent no-Detail baselines on 24/60 MP JPEG and the authentic RAWs, including overlapping masks and Detail+Basic+Presence/Dehaze+crop. Record p50/p95, counts, host load, display bounds and CPU/GPU high-water marks. Keep below-16 ms p95 interaction as target, below 32 ms acceptable; report misses under the existing nonblocking latency policy. Settlement is measured separately: 120 ms schedules it, not guarantees completion. Measure serial/pool crossover before changing thresholds.
5. **Completion.** Narrow tests during edits, `quick` at finished handoff, `rendered` at integration, timing on the finished build and `full` with the authentic manifest before a module milestone claim. Automated launches stay hidden/background. Update current architecture, module/API, preview, masks, performance rules/evidence, feature status and user guide. Distinguish Windows/Linux functional checks from native GPU evidence; skipped checks are incomplete. Output sharpening and profiled/AI denoise remain separate future scope.
