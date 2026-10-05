# RAW looks

Status: planned. The owner decided on 2026-10-05 that new RAW photos start from a Luxforge look rather than the bare neutral development, with matching the camera's own preview as a setting, and accepted the design's defaults the same day ([decided](#decided)). Phase 1, the Standard look, is to be built first. Phase 2, Match camera, follows on the same layer and payload. [Task plan](../../tasks/raw-looks.json).

## Why

The catalog shows a RAW file's embedded camera JPEG. That picture carries the camera's picture style: a contrast curve, extra saturation, a soft highlight shoulder, and on some bodies local shadow lifting (Active D-Lighting, Fujifilm's DR modes). Develop shows Luxforge's own development, which is deliberately neutral ([initial RAW](initial-raw.md#pixel-and-color-contract)): white balance and the camera matrix into linear sRGB, the recipe, then clamp, the sRGB transfer function and quantization. There is no base tone curve, no saturation, no highlight roll-off and no baseline exposure; nothing reads DNG `BaselineExposure` or a Fujifilm DR setting. So a photo that looked vivid while choosing it looks flat and slightly dark once developed, and after its first render its catalog tile turns flat too.

Every mainstream RAW editor starts from a look for this reason: Lightroom's default profile (Adobe Color) carries a tone curve and colour table, darktable's scene-referred default adds exposure and a tone mapper, and RawTherapee offers an auto-matched curve fitted to the embedded JPEG. These are familiarity references, not rendering targets.

## Outcome and scope

- **One module, `luxforge.look`,** with one colour-stage layer on RAW photos. A **look** is data: a tone curve with a highlight shoulder, a chroma gain, a path to white for colours past the display, and an amount. The development itself stays neutral and unchanged; the look is an ordinary recipe layer, so originals, history, export and the API see it like any other edit.
- **Three looks.** **Standard**, Luxforge's own, fixed and camera-independent (phase 1). **Camera**, fitted per photo to its embedded camera preview (phase 2). **Neutral**, no look: today's rendering.
- **The starting look is a preference.** New RAW photos start from Standard by default. Settings › General chooses Standard, Match camera (phase 2) or Neutral.
- **A Look section** in Develop: the look choice and Amount. The same choice through `edit.set-look`.

Out of scope, with no placeholders: reproducing Picture Controls or film simulations (hue-specific colour tables, monochrome simulations, grain), local tone mapping matched to Active D-Lighting or DR modes, camera-model-specific Standard looks, user-authored or imported looks (DCP, LUT, Lightroom profiles), looks on JPEG photos, masked looks, and rewriting photos already in a catalog. Phase 1 shows no Camera choice anywhere.

## Placement

The look is a pointwise colour layer, `stage: Color`, **order 3**: after Basic (0), before the Tone curve (5) and the colour mixer (10). It is `single`, `sources: [Raw]` and not maskable.

- **Exposure and Basic tone act on scene values before the look.** Basic's Exposure multiplies the unclipped development, so +1 EV pushes highlights into the look's shoulder instead of into a hard clip, and Highlights and Whites shape scene values the shoulder then rolls off. This is why the look cannot sit before Basic: a shoulder followed by a linear gain would clip again.
- **The neutral picker is unaffected.** It samples the stage Basic receives, before the look.
- **The Tone curve and the mixer act on the looked picture,** so a person's curve is the last tonal word, read from a display-referred plot.
- **Placement is by order.** A look written into the Original sits alone in the colour stage; a Basic layer inserted later goes before it (order 0 < 3), a Tone curve after it.

## The look

The layer compiles to at most four pointwise units, in this order, each frozen against an independent `f64` reference in `crates/luxforge-reference/src/look.rs`. A Neutral look compiles to no unit and keeps the identity path.

1. **Tone.** A monotone curve `T` over **encoded luminance**, the sRGB transfer continued past `[0, 1]` that [Basic tone](basic-tone.md#working-tone-domain) and the [Tone curve](tone-curve.md) already use, with the Tone curve's floor-subtracted luminance-ratio reconstruction, so hue is kept. `T` is the Tone curve's open monotone interpolant (Fritsch–Butland PCHIP) over at most 24 knots, with a **look tail policy**: the knots span `[0, x_max]`, where `x_max = encode(2^H)` is the brightest scene value the look rolls off (`H` the headroom above sensor white). Below 0 the first segment continues linearly, so negative luminance stays finite and monotone. Above `x_max` the curve holds its last value, which is at most 1. The Tone curve's own tail policy, the unit-slope tail past white, is unchanged and stays byte-identical. The curve folds in the baseline lift: it maps the development's scene mid-grey to display mid-grey.
2. **Chroma.** A uniform Oklab chroma gain `c` at constant Oklab lightness and hue, by Basic's colour unit where its saturation formula expresses that gain exactly, otherwise a look-owned unit with the same Oklab conversions.
3. **Path to white.** A colour whose largest linear channel passes a knee `k` below 1 after tone and chroma has its Oklab chroma reduced at constant lightness and hue, smoothly, so it reaches white at the display boundary rather than clipping one channel at a time and shifting hue. A bounded closed form or a fixed number of bisection steps, decided by the study, so the GPU cost is fixed.
4. **Amount.** `out = in + a · (look(in) − in)` in linear light, `a = amount / 100`, amount 0–200; 0 compiles to nothing, 100 is the look. Past 100 it extrapolates, as Lightroom's profile Amount does.

**Payload** (current shape only):

```json
{"look": "standard", "amount": 100, "tone": [[0, 0], …, [x_max, 1]], "chroma": 1.12, "fit": null}
```

`look` is `standard`, `camera` or `neutral`. The resolved `tone` knots and `chroma` are **stored in every payload**, so a render depends only on the recipe: retuning Standard later never changes a photo already edited, and a camera fit never needs its preview again, not to export, not after the original goes missing. A Neutral payload is `{"look": "neutral"}`. `fit` is `null` except on a Camera look, where it records the fit's provenance ([Match camera](#phase-2-match-camera)).

### The Standard look

One fixed look, chosen by a study and frozen as knots and a chroma gain in `modules/look/standard.rs`. Its targets, which the study measures and the owner reviews:

- **Placement.** Over the corpus, the median midtone of Standard is within ±0.25 EV of the cameras' own JPEGs (histogram-matched on the embedded previews, which phase 2's fitter reuses as a measuring instrument). Recorded default before the study: a +0.5 EV lift.
- **Contrast.** A midtone slope in the range of the camera JPEGs, measured the same way. Recorded default: a log-logistic (sigmoid) curve with a midtone slope of about 1.5 in log–log terms.
- **Highlights.** The shoulder reaches display white at `H` = +1 EV above sensor white, so Basic's Exposure and Highlights keep recoverable range, and nothing at or below sensor white clips.
- **Chroma.** A gain of 1.10 to 1.20, chosen visually.
- **Owner review.** A contact sheet of about 24 corpus frames, each Neutral, Standard and the camera's preview side by side, is reviewed by the owner before the numbers freeze.

## Phase 2: Match camera

A Camera look is a Standard-shaped payload whose tone and chroma were fitted to the photo's embedded camera preview. Fitting is statistical, never pixel-aligned, so framing differences, in-camera lens correction and the preview's resolution do not matter.

**Inputs.** The photo's **as-shot development** (the Original's look input: RAW layer only, nothing from Basic) downscaled to at most 1024 px, and its **largest embedded JPEG** (`EmbeddedPreviews::largest_jpeg`, cut at its last EOI as the 100% region already does), decoded at the DCT scale that gives at most 1024 px, upright. Only an sRGB or Adobe RGB preview (by ICC profile or EXIF colour space) is fitted, converted to linear sRGB; another is refused with its reason.

**Fit** (recorded defaults, frozen in the fit study):

1. Pixels where the preview is clipped (a channel at 250 or more) or crushed (every channel below 5), and non-finite development pixels, are excluded. Fewer than 10% usable refuses the fit.
2. **Tone by histogram matching.** 1024-bin histograms of encoded luminance on both sides; the monotone quantile map from development to preview at fixed quantiles (for example 1, 3, 7, 15, 25, 40, 50, 60, 75, 85, 93, 97, 99%), with knot slopes held between a floor and a cap so noise cannot make a step or a plateau.
3. **The shoulder is Standard's.** Above the highest confident quantile the preview is clipped and says nothing, so the curve joins Standard's shoulder continuously and monotonically. A camera match never throws away highlights the RAW holds.
4. **Chroma.** After applying the fitted tone to the development, the median Oklab chroma ratio, preview over development, on mid-lightness pixels with real chroma, clamped to 0.7–1.6.
5. **Monochrome previews.** A preview with no chroma where the development has some is a monochrome picture style: the tone is fitted, chroma stays 1.0, and the fit says so. The photo is never turned monochrome.

`fit` records `{preview: [w, h], usable: fraction, notes: [...]}`, with notes such as `monochrome-preview`, and is shown in the Look section ("Matched to the camera's 2560 × 1707 preview").

**Where it runs.** Never on the catalog owner. The fit is a function of the original only (its as-shot development and its preview), not of the stack, so it is cached per source identity beside the prepared source and reused until the source leaves the cache.

- **At first open,** when the preference is Match camera: the source worker computes the fit after decoding, where `await_first_open` runs today, through a new hook that hands the module the prepared development and the original's bytes and returns opaque first-open data; `first_open` then proposes `match-camera-look` with it, on the owner, reading no pixels. This extends the [first-open contract](lens-and-perspective.md#modules-api-and-history), which today reads metadata only.
- **On request** (`edit.match-camera-look`, the Look section's Camera choice): the action defers its fit through the [off-owner pixel-read path](detail.md#pixel-reads-off-the-owner) as a new read kind keyed by the source identity and development, and is replayed with the answer, under the existing round and queue limits.

**When it cannot.** A photo with no usable preview (Canon R5 Mark II and R8 carry none; a 160 px thumbnail is not one), an unsupported colour space or too few usable pixels gets no Camera look. At first open it keeps its Standard look and the preparation's `first_open` report carries the module's error with the reason; on request the action is refused with `unavailable:` and the reason. Nothing falls back silently.

## Starting a new photo

**Standard and Neutral are in the Original.** A new RAW photograph's Original (`EditorService::new_photograph`) holds the look the `raw_look` preference names, beside the RAW development layer. This needs no pixels, so a batch Develop of picks costs nothing more, and the catalog's rendered tiers and batch exports of photos never opened in Develop carry the look, because those render the current entry without a first open. A new hook, `ToolModule::original`, lets a module contribute a layer to a new photograph's Original from its kind, metadata and the preferences; the look module is its first user. Before and after compares against the Original, so Before shows the starting look, not the flat development.

**Match camera is a first-open entry.** A fit needs pixels, so with the preference at Match camera the Original holds Standard, and the first preparation at revision 0 commits `Look Match camera` by the `system` actor after it, beside the lens entry, in registry order. Undo returns to Standard. A photo whose fit was refused stays at revision 0 and is asked again at its next preparation, as a lens proposal is.

**Catalog tiles follow.** A first-open entry is logged today but not announced to the preview lane, so a tile already rendered from the Original is not re-rendered until it is read again. Phase 2 announces first-open entries as edits are announced, which corrects the lens entry's tiles too.

**Existing photos are untouched.** Photos already in a catalog keep their recipes and render as before; choosing a look in their Look section is an ordinary edit. The preference applies to photos created after it changes.

## Controls, API and history

- **Look section**, on RAW photos only, before Basic in the tools panel: a choice row (Standard · Camera · Neutral; Camera from phase 2, disabled with its reason when the photo has no usable preview), Amount (0–200, step 1, a slider with the core draft lifecycle), and for a Camera look the fit's one-line provenance. JPEG photos have no Look section.
- **`edit.set-look {look?, amount?}`**, a field patch: `look: standard` writes the current Standard's knots; `look: neutral` writes the neutral payload; `look: camera` is refused with a pointer to `edit.match-camera-look`, which takes no parameters. `edit.reset-look` returns to Standard ([decided](#decided)). If the field-patch contract cannot carry the resolved `tone`, `chroma` and `fit` fields, the module implements its actions directly, as Lens does.
- **`query.sample-look`** answers the stored curve sampled at 257 points over `[0, x_max]` and the chroma gain, as `query.sample-curve` does; `recipe.describe` reports `look`, `amount`, `chroma`, the knot count and `fit`.
- **History labels**: `Look Standard`, `Look Neutral`, `Look Match camera`, `Look amount 80`, `Reset Look`.
- **Preference** `raw_look`: `standard` (default) or `neutral` in phase 1, `camera` added in phase 2; General row **Starting look for new RAW photos**, beside the lens switch; takes effect for photographs created from then on, and for Match camera at their first preparation. It changes no saved recipe.
- **Presets.** The look is not presettable ([decided](#decided)). Lightroom import's `CameraProfile` rows keep their mapping, with the reason text updated from "Luxforge keeps its own neutral rendering" to "Luxforge's looks are its own".

## GPU and reference

Under [GPU-first](gpu-first.md), each unit is a WGSL program beside its CPU unit, listed in `GPU_PROGRAMS`, whose uniforms are a pure function of its description (`assert_uniforms_follow_descriptions`) and whose device output meets the pointwise limits over the 65,536-pixel qualification grid (`gpu_colour_tests.rs`). The tone unit reuses the Tone curve's program with a tail-policy word; its `describe` identity is parameterized by module so the two never share a description. The CPU units are checked exactly against the frozen fixture from the `f64` reference; the GPU within the declared tolerance. The GPU-first integration is in flight: whichever lands second regenerates `GPU_PROGRAMS`, the qualification step list and the conformance list rather than hand-merging them.

## Resource and responsiveness

- **Owner:** writing a look into an Original is a payload copy; no pixel, no file read. `pick.develop` time is unchanged within noise.
- **Render:** three or four pointwise units on every RAW frame, the same class of cost as the Tone curve plus Basic's colour unit; measured at Fit and 100% on 24 MP and 60 MP RAW.
- **Fit (phase 2):** one embedded-preview extraction (1–3 MB of positional reads), one DCT-scaled decode and one downscale of the development, on the source worker. Target under 60 ms at 24 MP beyond the preparation itself; measured. Cached per source; nothing on the owner, the UI thread or any timer.
- **Memory:** two 1024 px working images during a fit, released after; one small cached fit per prepared source.

## Verification and acceptance

1. **Exactness.** The CPU units match the frozen `f64` reference fixture exactly; the GPU programs pass their qualification within the declared tolerance; the Tone curve's tail policy and outputs stay byte-identical; Neutral and amount 0 keep the identity path.
2. **Properties.** Monotone tone on an extended ramp, including negative and above-white luminance; greys stay grey; hue kept through tone and chroma; no channel clips below the shoulder; the path to white never increases chroma; finite on extreme inputs.
3. **Original and preference.** A new RAW Original holds the preference's look; a JPEG's holds none; existing photos and their renders are unchanged; batch Develop time unchanged.
4. **Native rendered evidence** on the M4 from a background `look` smoke scenario over the supplied Z6, X100VI and Air 2S files: a new photo opens with Standard; Neutral, Standard and Amount through the section and the API; the catalog tile carries the look; export matches the render. Phase 2 adds Match camera through the preference and the section, a refused fit's reason, and a monochrome-preview note.
5. **Fit quality (phase 2).** Over the corpus, the fitted look's median luminance error against the preview is reported per camera, with the cases where it is refused or degraded listed. No pixel equivalence with any camera is claimed.
6. **Measurement** once all feature work is done: per-frame cost of the look units, fit cost, `pick.develop` with and without it.

## Decided

The owner, on 2026-10-05, after noticing that photos which look vivid in the catalog look flat in Develop:

- New RAW photos start from a Luxforge look (Standard) by default rather than the bare neutral development, replacing "neutral development is the initial direction" for the starting point. The development itself stays neutral.
- Matching the camera's own preview is offered as a setting.
- Standard is built first; the architecture carries Match camera.

The owner accepted these defaults on 2026-10-05:

| Question | Decided | Not chosen |
| --- | --- | --- |
| Where Standard lives | In the Original, so tiles, batch exports and Before carry it | A first-open entry after the Original, as Lens; tiles and exports of unopened photos would stay flat |
| Order | Colour stage, order 3, after Basic and before the Tone curve | Before Basic, which clips a later Exposure push |
| Stored data | Resolved knots and chroma stored in every payload | Standard by name only, which would re-render old photos on every retune |
| Reset and the edited dot | Reset Look returns to Standard; the band's dot is lit only when the look is not the current Standard at amount 100 | Neutral as the default, lighting the dot on every new RAW |
| Amount | 0–200, as Lightroom's profile Amount | 0–100, or no Amount |
| Presets | Not presettable | Presettable Standard and Neutral, with Camera refitted per photo |
| Baseline exposure | Not read: the Standard curve's lift is camera-independent, and Match camera absorbs a camera's exposure strategy | Read DNG `BaselineExposure` and the Fujifilm DR setting into the look as recorded data, with corpus evidence |
| Monochrome previews | Tone matched, colour kept, noted | Match to monochrome |
| Fit colour spaces | sRGB and Adobe RGB previews | sRGB only |

## References

- [Initial RAW](initial-raw.md), [Basic tone](basic-tone.md), [Tone curve](tone-curve.md), [Basic colour](basic-colour.md)
- [Lens first open](lens-and-perspective.md#modules-api-and-history), [Preferences](preferences.md), [Catalog rendered previews](catalog.md#rendered-previews), [Embedded previews inventory](../research/embedded-previews.md)
- [GPU-first](gpu-first.md), [GPU previews](gpu-preview.md), [Detail: pixel reads off the owner](detail.md#pixel-reads-off-the-owner)
