# Basic tone that adapts to the picture

Status: **decided and implemented on the CPU reference and in WGSL**; the WGSL is unqualified, since this Linux host has no Metal. This is the level C change ([A1, A8](lightroom-alignment.md#decided)) that the [Lightroom alignment](lightroom-alignment.md#measured-at-the-identity) records for Highlights and Shadows: Lightroom Classic's answer the charts and the photographs differently, and no single global curve meets both. A study measured why, and asked the same of Whites, Blacks and Contrast. The change is not local. Highlights, Shadows and Whites act on a RAW photograph's scene values relative to the photograph's own scene white, and Blacks, Contrast, Vibrance and Saturation act after the Look ([decided](#decided)). The study's prototypes stay study code (`luxforge_reference::local_tone`, `cargo xtask lr-align tone-study`); the frozen operator is in `luxforge_reference::tone`. M4 qualification and the owner's review remain (TASK-006 of the [task plan](../../tasks/lightroom/basic-local-tone.json)).

## Why

With one global curve before the Look, at the identity, measured through the CPU reference renderer:

- **Highlights** fits the charts and the photographs with different strengths. At Lightroom −100 the charts want about −111 and the pooled photographs about −24. Each photograph has its own: −16, −32, −36, −79 and −85.
- **Shadows** is stronger on the photographs than on the charts. It is also local on the photographs: Lightroom's tone locality is 1.7 and 3.3 at ±100, against Luxforge's 0.4 and 0.3.
- **Whites** +100 and **Blacks** −100 miss by 7 to 8 (median CIEDE2000).
- Round 2's photograph pairs of **Contrast** with Blacks or Whites miss by 12 to 13.

A level C change targets the dimension that differs. So the study's first job was to separate the two explanations the data allows. A control can *adapt to the picture's content*: locally, as an edge-aware base would, or globally, through a statistic of the whole picture. Or it can *act on scene rather than rendered values*: before or after the Look, which on RAW is where most of the picture's tone is made.

## The study

`cargo xtask lr-align tone-study --charts ROUND1 --photographs ROUND1B [--pairs ROUND2] --output NEW [--cache DIR] [--candidates ID,...] [--fields ID,...|none]` compares candidate operators with Lightroom Classic's exports and nothing else ([A4](lightroom-alignment.md#decided)). The exports used are round 1's charts (the wedge, the patch grid and the surround target), round 1b's five corpus photographs at ±50 and ±100, and round 2's photograph pairs. Every candidate is a pure `f64` function in `luxforge_reference::local_tone`, evaluated on the subject's own pixels.

**Photographs.** Each photograph is its development's scene-linear planes, rendered once through the reference renderer with the Look removed. Basic Exposure is set to −3 EV, a plain gain on RAW, so the 16-bit output keeps three stops above white. The planes are scaled back up and reduced 4× per axis. The photograph's own Look is then applied from its payload.

**Checks.**
- The study's neutral matches the editor's own neutral render within a median CIEDE2000 of 0.001 to 0.09 per photograph.
- The prototypes have known-answer tests (`tools/cargo-cached test -p luxforge-reference --lib local_tone`).

**Measures.** The study uses `lr-align measure`'s:
- carried responses in Oklab;
- the median CIEDE2000 over the samples Lightroom's response moves;
- surround locality;
- tone locality;
- band energy at three scales.

**Fitting.** For each candidate and each Lightroom value, the study fits:
- the best strength on the charts alone;
- the best strength on the photographs alone;
- each photograph's own best strength;
- one strength shared by both classes, the joint fit A8 asks for.

Two figures separate shape from adaptation:
- The **floor**: each class's residual at its own best strength. It is what the curve's shape cannot reach, whatever the strength.
- The **excess**: the joint fit's residual above each class's floor. It is the conflict between charts and photographs, which adaptation should remove.

**Scope of the figures.** CPU only, at a quarter of each photograph's size. No GPU output was measured; this Linux host has no Metal.

### What Lightroom does

Read off Lightroom's exports alone:

| Control | On the charts | On the photographs | Kind |
| --- | --- | --- | --- |
| Highlights | The wedge loses 5 to 22 L\* from the midtones up at −100. On the surround target a patch follows a moderately different surround but keeps its own beside a strongly different one (locality 10.7 at −100, 6.4 at +100): an edge-aware response | A third to a half of the wedge's change at the same lightness. The strength varies about fivefold between photographs and orders with their scene white: the two whose scene stays below a third of clipping take the most, the three that reach clipping the least. Tone locality 0.36, about as global as Luxforge's | Adapts to the picture's scene white; edge-aware on synthetic surrounds only |
| Shadows | Small, except a dark patch on a dark surround | Larger than on the charts, by up to three times on the photographs with few deep shadows; tone locality 1.7 and 3.3 | Adapts to the picture's range; somewhat local on photographs |
| Whites | The negative half matches the photographs. +100 lifts the wedge's midtones by 20 L\* | +100 lifts the midtones by 8 L\* on the brightest photograph and 34 L\* on the dimmest; no surround locality | The positive half re-stretches a picture's top to its own white, which differs in kind from an endpoint remap |
| Blacks | | The same as the charts to within a few L\* in every decile; no surround locality | Neither local nor adaptive |

### Candidates and results

Candidates that act before the Look run on the scene planes and are then rendered through the Look; candidates that act after it run on the rendered values. On a chart, which has no Look, the two places coincide.

- **The global curve, before or after the Look.** `global-pre`, `global-post`; `global-post-oklab` keeps Oklab chroma.
- **Edge-aware bases** under the same curve, `E' = T(B) + (E − B)`. Each has a box-mean control.
  - A self-guided filter on a 4× reduced grid at radii of 1.6 to 20% of the long side, with `eps` from 0.01 to 0.16. Clarity's tiled base is the 1.6% case.
  - He and Sun's fast guided filter, whose coefficients come from a whole-frame reduction of at most 128 or 256 samples on the long side and are applied at full resolution. It needs no halo.
- **The curve within the picture's own range.** Black and white points are read at quantiles of the frame being acted on (`adapt-*`).
- **The curve over the scene's white.** Before the Look, the scene values are divided by `c × W` and multiplied back after, where `W` is the photograph's 0.999 quantile of scene luminance and `c` is 1 to 4 (`white-pre-*`). The alternative is a fixed anchor 1, 1.5 or 2 stops lower on every photograph (`global-pre-shift*`).
- **Splits on round 2's pairs.** Some fields act on the scene-white curve before the Look and the rest after it (`split-*`).

Worst residuals at Lightroom values of ±25 and beyond (median CIEDE2000). For a single field: the worst joint residual, the worst excess over the class floors, and each photograph's residual at the joint strength. The full tables, every candidate's contact sheets and the per-photograph strengths are in the study's evidence (`study.md`, `tone-study.json`, `sheets/`).

| Field | Candidate | Worst | Excess | Photographs at the joint strength | Surround locality at −100 (Lightroom 10.7) | Photographs' fine-band energy at −100 (Lightroom 0.99) |
| --- | --- | --- | --- | --- | --- | --- |
| Highlights | global, before the Look | 2.97 | 1.71 | 0.6 to 4.2 | 0 | 0.84 |
| Highlights | after the Look | 4.27 | 3.54 | 0.8 to 8.2 | 0 | 0.74 |
| Highlights | guided base 10%, `eps` 0.04, before the Look | 3.16 | 1.45 | | 10.3 | 1.08 |
| Highlights | **scene white, `c` = 3** | 2.97 | 0.49 | **0.2 to 1.5** | 0 | 0.91 |
| Highlights | scene white, `c` = 4 | 2.95 | 0.22 | | 0 | 0.93 |
| Highlights | fixed anchor 2 stops lower | 2.95 | 0.05 | 0.2 to 2.5 | 0 | 0.96 |
| Highlights | scene white `c` = 3, fast guided base 10%, `eps` 0.04 | 3.16 | 0.27 | 0.3 to 1.5 | 10.6 | 1.02 |
| Shadows | global, before the Look | 2.20 | 0.16 | 0.5 to 2.9 | 0 | |
| Shadows | after the Look | 2.70 | 1.24 | | 0 | |
| Shadows | **scene white, `c` = 2** | **1.80** | 0.21 | 0.7 to 3.2 | 0 | |
| Shadows | scene white, `c` = 3 | 2.13 | 0.29 | 0.7 to 3.0 | 0 | |
| Shadows | fixed anchor 1.5 stops lower | 2.88 | 0.39 | 0.7 to 4.7 | 0 | |
| Whites | global, before the Look | 7.87 | | 4.6 to 11.6 at +100 | 0 | |
| Whites | best of any candidate | 7.00 | | | | |
| Blacks | global, before the Look | 7.34 | 4.77 | | 0 | |
| Blacks | **after the Look** | **2.59** | 0.09 | | 0 | |
| Blacks | scene white, `c` = 3 | 10.34 | | | | |

The floors are each curve's shape, which no strength removes:
- **Highlights:** the charts' own best strength leaves 2.16 at +75 and 2.98 at +100, at the stable limit of ±150 ([A10](lightroom-alignment.md#decided)).
- **Whites +100:** 3.59 on the charts at a strength of 200, and 4.5 to 7.6 on each photograph alone.
- **Blacks:** 2.57 at −100 and 2.17 at −75 on the charts. After the Look, single photographs' own floors at −100 reach 1.5 to 4.7, though the pooled photographs fit within 2.5.

Round 2's photograph pairs at the global curve's responses (Exposure a plain gain), as the pooled median CIEDE2000 over the five photographs. Dehaze with Contrast is not rendered, because the study has no Dehaze.

| Arrangement | Contrast + Whites | Contrast + Blacks | Exposure + Highlights | Exposure + Whites |
| --- | --- | --- | --- | --- |
| All before the Look | 12.43 | 12.96 | 4.90 | 5.92 |
| All after the Look | 6.27 | 4.64 | 10.81 | 7.99 |
| All on the scene-white curve, `c` = 3 | 16.00 | 19.23 | 1.46 | 6.15 |
| **Highlights, Shadows and Whites on the scene-white curve; Blacks and Contrast after the Look** | **5.92** | **4.64** | **1.46** | **6.15** |
| The same, with a fast guided base for Highlights and Shadows | 6.27 | 4.64 | 2.08 | 7.99 |

The remaining Exposure + Whites figure is Whites +100 on the two dim photographs darkened a stop (21 to 26 there), the re-stretch above. The pairs alone cannot divide the remaining Contrast figures between the Contrast curve and its partners; round 3's Contrast alone on the photographs is in [the refit](#the-refit).

### What the study shows

- **Adapts to content, globally.** Highlights' conflict between charts and photographs is about where the curve's white sits. On RAW, Lightroom's Highlights acts on scene values relative to the photograph's own white, about 1.6 stops above it (`c` ≈ 3, where the global curve puts that white at scene 1.0). Anchored there, every photograph fits within 1.5 at the strength the charts need, against up to 4.2 for the global curve. A fixed anchor fits the pooled photographs as well, but not the dim ones: it reaches 2.5 for Highlights and 2.9 to 4.7 for Shadows. Shadows behaves the same way with its white lower (`c` ≈ 2) and then passes A8. Neither needs a local base to fit.
- **Locality is a difference in kind that matters only on synthetic surrounds.** An edge-aware base reproduces Lightroom's Highlights locality on the surround target and its preserved fine detail. But it makes the photographs five times as locally varied as Lightroom's (tone locality 1.7 against 0.36) and worsens Exposure + Highlights from 1.46 to 2.08. It does not lower Shadows' floor (2.12 to 2.03). A base wide enough to match also needs a radius of 5 to 10% of the long side. At 60 MP that is a halo of 1000 to 2000 pixels, past the 512-pixel bound, so it could only come from a whole-frame estimate.
- **Rendered, not scene, for Blacks and Contrast.** Blacks fits with one strength on charts and photographs once it acts after the Look: 7.34 falls to 2.59, the charts' own floor. The Contrast pairs fall from 12.4 and 13.0 to 5.9 and 4.6 the same way.
- **Whites** is not local and does not adapt through the scene white. Its negative half already fits. Its positive half differs in kind: Lightroom re-stretches a picture's top towards its own white, and the endpoint remap cannot do that at any strength. Of the arrangements studied, the scene side fits its pairs best.

## Design

### The operator

Basic's tone unit is two parts, around the Look. The curves inside each part are the frozen [Basic tone](basic-tone.md) stages; only their domain and place change.

**The scene part.** Whites, then Shadows, then Highlights, acting on the input Basic's own Exposure and white balance leave. On a scene-referred input, a RAW photograph's development, each stage runs in a domain scaled to the photograph's **scene white** `W_e`:

```text
W_e      = W × g                                        W: the development's scene white (below)
                                                        g: Basic's scene gain on a neutral grey
u_S      = L / (c_S × W_e)                              Shadows' domain, c_S = 2
u_H      = L / (c_H × W_e)                              Whites' and Highlights' domain, c_H = 3
whites(x), shadows(x), highlights(x) as frozen, on x = encode(u); L' = c × W_e × decode(x')
```

`g` is `Σ luma_c × 2^stops_c`, the luminance gain Basic's white balance and Exposure give a neutral grey; a later layer's white is also scaled by the gain of every unmasked Basic layer before it. Composed in that order, the result is rebuilt by the luminance ratio with the near-black rule. Values past the domain's white pass through, as Highlights and Shadows do outside `[0, 1]`. On a rendered input (a JPEG, a derived source) there is no scene white: the domain is the rendered one (`c × W_e` is 1), and the scene part is the plain Whites, Shadows and Highlights.

**The rendered part.** Blacks, then Contrast, then Vibrance and Saturation, acting after the Look on rendered values, on the encoded luminance. Vibrance and Saturation follow the tone, as Basic's frozen internal order has them. On a rendered input the rendered part follows the scene part directly.

**The scene white** `W` is a property of a RAW photograph's development, not of the stage. It is the 0.999 quantile of linear luminance over the developed planes, reduced by block means to at most 0.25 MP. The block side is the least `k` whose grid over the view holds at most 2¹⁸ blocks; `W` is the block means' order statistic at index `⌊0.999 (n − 1)⌋`, no less than 1/256, read over the development's view rectangle of the planes, unturned. It is computed once on the developing worker when the photograph is developed (`LinearImage::measured_scene_white`), kept with that development and by its proxies, and bound to the recipe wherever an evaluation reads that source. It depends on neither the crop nor any other layer, so cropping does not change tones. Exposure scales it exactly, because Exposure is a plain gain on RAW.

The constants are the study's, `c_H` = 3 and `c_S` = 2, rechecked at the frozen reduction (see [the refit](#the-refit)).

### Order on RAW and JPEG

| Input | Order |
| --- | --- |
| RAW | Scene unit (white balance, Exposure) → Whites → Shadows → Highlights (scene-white domain) → **Look** → Blacks → Contrast → Vibrance/Saturation → Tone curve → … |
| JPEG | Scene unit → Whites → Shadows → Highlights → Blacks → Contrast → Vibrance/Saturation |

Exposure and white balance come first, so the Look's shoulder rolls off what Exposure raises, and a lower Exposure recovers a RAW photograph's headroom ([Basic on the scene scale](basic-scene.md)). On a JPEG, Blacks follows Highlights and Shadows rather than sharing Whites' remap. The two remaps do not share a gap clamp, so each part's monotonicity is proved on its own, densely, as [Basic tone](basic-tone.md#monotonicity-the-dense-proof-and-its-scope) does.

### One layer, two places

Basic stays one layer with one payload, one history entry per commit and one `edit.set-basic`. Placing its rendered part is the host's job:

- A colour operation may carry a **rendered tail**. The registry, compiling a stack whose input is scene-referred, places a layer's tail directly after the first Look that follows it with only colour and spatial layers between, and otherwise directly after the layer's own units. On a JPEG the tail always follows its layer. A prefix of the stack (the Before view, a layer's input, the neutral picker) holds only the tails the whole stack places before its cut, so a Basic layer's tail never appears in the stage a later Look reads.
- The layer's mask, compiled against the stage the layer receives, applies to both parts. The Look keeps the stage, so one mask serves both.
- The neutral picker reads the stage before Basic; Auto tone's forward model compiles Basic and its tail around the Looks it already evaluates.

The tail is not a second Basic layer: one panel's values would then live in two layers, their history, undo, presets and `edit.set-basic` would not be atomic, and a mask would have to be kept identical on both.

### Cost and the performance rules

Both parts are pointwise. They join the stack's colour run (the Look and the Tone curve are pointwise too), so the GPU draws them in one fused pass with no new program boundary. Both parts are the one tone program, `lf_basic_tone`, whose uniform is 13 words, four of them the domain's white, its inverse, and the carry into and out of Shadows' domain, skipped where the two domains are equal. A drafted Basic layer, whose units are all present so a drag keeps one program sequence, holds four units: the scene unit, the scene part, the rendered tail and colour.

- **Halo, scratch, GPU window and staged sweeps:** none. The scene white is a compile-time constant like any slider value, so the unit has no global estimate, no light link and no plane.
- **Point queries** stay `O(layers)` ([rule 4](../engineering/performance-rules.md#rules)).
- **Drags:** a Highlights or Shadows drag changes uniform words. An Exposure drag changes `W_e` with the exposure it already carries.
- **The statistic's own cost:** one pass over the view's pixels, the rows of blocks on the shared pool, and a selection over at most 2¹⁸ means, once per development on the developing worker, never on the catalog owner, the UI thread or a drag. Its scratch is the means, at most 2 MiB. Its time at 24 and 60 MP is unmeasured; the next timing run measures it.
- **Compilation:** an evaluation over a RAW photograph binds its scene white and recompiles once when it first reads the development.

### The local option, not chosen

Lightroom's surround locality would need a fast guided base over the scene part's domain: coefficients from the development's luminance reduced to at most 256 samples on its long side, kept with the development (up to 350 KiB) and read bilinearly at each pixel, one small texture per development on the GPU. The global operator is [decided](#decided) instead, for the measured costs above.

### Format, responses, Auto tone and imports

- **Format.** Basic's payload format is 4. A layer or preset of an earlier format is refused as incompatible, naming its format ([A6](lightroom-alignment.md#decided), current shapes only).
- **Responses.** Refitted by `lr-align fold` from rounds 1 and 1b measured on a study build of this operator ([the refit](#the-refit)), with the owner's Whites review kept.
- **Auto tone.** The forward model uses the same split, the same scene white and the same Looks. Searched fields (Exposure, Whites, Blacks) search through it unchanged in method. Ruled fields (Contrast, Highlights, Shadows) keep their picture-statistic triggers and bounds in picture terms, expressed through the refitted responses.
- **Preset import** keeps transferring Lightroom's Basic values as the same numbers ([A11](lightroom-alignment.md#decided)).

## The refit

Rounds 1 and 1b are measured on a study build of this operator whose responses stretch Contrast and Highlights to ±150 and Whites to ±200, every other field the identity, and folded into `response.json`. The fold chooses each side's knots together under the floor ([Basic realigned](lightroom-alignment.md#basic-realigned)): a greedy floor carries Shadows' slope at −10 outward and pushes every later knot past the floor, leaving Shadows at −40 and +37 where the charts and the photographs both want about −25 and +22, 3.1 to 3.4 jointly. Strengths at the slider's ends: Contrast −144.4 and +99.4, Highlights −112.5 and +91.9, Shadows −25 and +22.5, Whites −42.5 and +100 (the owner's review; the joint fit puts +100 at 137.5), Blacks −83.8 and +60, Vibrance −98.8 and +88.8, Saturation −100 and +85.

Measured on the refitted build through the CPU reference renderer at full size, on rounds 1, 1b and 2, on this Linux host with no GPU output: the worst median CIEDE2000 at the identity, on the charts and on the photographs, at Lightroom values of ±25 and beyond.

| Field | Charts | Photographs |
| --- | --- | --- |
| Highlights | 1.80 from −100 to +50; 2.21 at +75 and 2.96 at +100 | 1.03 |
| Shadows | 1.94 | 1.81 |
| Blacks | 1.22 from −50 to +100; 2.25 at −75 and 2.57 at −100 | 1.34 from −50 to +100; 2.64 at −100 |
| Whites | 1.67 to +50; 4.26 at +75 and 10.31 at +100 | 2.04 to +50; 4.95 at +100 |
| Contrast | 3.31 | not exported in rounds 1 and 1b |
| Vibrance | 4.71 | not exported |
| Saturation | 1.83 | not exported |

Round 2's photograph pairs, worst median: Exposure + Highlights 1.45, Contrast + Blacks 3.75, Contrast + Whites 5.87, Exposure + Whites 6.13, whose worst point is Whites +100 a stop darker, the re-stretch. Dehaze + Contrast on the photographs is 7.94, and Contrast + Blacks on the charts 3.85.

Round 3, the confirming round, is measured on this build and never fitted; its photographs hold Contrast alone. Worst median at ±35 and beyond: on the photographs Contrast 2.41, Highlights 1.08, Shadows 1.39, Blacks 2.06, Whites 4.45 (+75, the re-stretch); on the charts Shadows 1.77 and Blacks 2.22. Contrast's best value on the photographs is about Lightroom's own (−35 at −37.5, +75 at +71.9).

## Acceptance

Status against the criteria below, from the refit's measurements: 1 to 4 are met, with Blacks −75 and −100 and Highlights +75 within 0.05 of their floors; in 5, Contrast and Saturation on the charts are within 0.03 of the global curve's, inside the fit's 0.1 resolution, and a rendered payload without Blacks is the single curve to `1e-12` (`a_rendered_payload_without_blacks_is_the_single_curve`); 6 is met on the CPU, with the WGSL unqualified; of 7, the confirming round is measured (above), and the owner's review, M4 qualification and the performance checklist's development-path cost remain.

Measured through the CPU reference renderer at full size with `lr-align measure` after the fold, on rounds 1, 1b and 2:

1. **Highlights:** within A8 jointly on charts and photographs from −100 to +50. The photographs are within A8 at every value. At +75 and +100 the charts are no worse than the curve's floor (2.2 and 3.0), recorded as the curve's shape.
2. **Shadows:** within A8 jointly at every value.
3. **Blacks:** within A8 from −50 to +100 jointly. At −75 and −100 no worse than the charts' own floor (2.2 and 2.6).
4. **Round 2's photograph pairs:**
   - Exposure + Highlights is within A8.
   - Contrast + Blacks and Contrast + Whites are recorded at no more than 5.0 and 6.5.
   - Exposure + Whites is no worse than 5.9 beyond the recorded Whites +100 re-stretch.
5. **No regressions:** Contrast, Whites, Vibrance and Saturation on the charts no worse than the global curve's (3.28, 10.31, 4.71 and 1.81). The JPEG renders of a payload without Blacks equal the single curve within the tone tolerance.
6. **Reference and production:**
   - The new reference is frozen with exact fixtures, its dense monotonicity proof and the scene white's definition.
   - The CPU units are exact against it, and the WGSL within the declared tolerance.
   - A layer and preset of an earlier format are refused naming the format.
   - Point queries through Basic stay `O(layers)`.
   - The scene white is crop-independent and scales exactly with Exposure.
7. **Later, not gating delivery:**
   - The owner reviews the study's and the fold's sheets.
   - Whole-corpus `gpu-qualification` with the RAW manifest passes on the M4.
   - The performance checklist is answered with the development-path cost.
   - A confirming Lightroom round, which adds Contrast alone on the photographs, records the result.

## Decided

Each recommendation is decided:

1. **Global, not local.** The global scene-white adaptation, with no edge-aware base. Lightroom's edge-aware Highlights on the surround target is recorded as a remaining difference in kind.
2. **Blacks, Contrast, Vibrance and Saturation after the Look on RAW.** Vibrance and Saturation follow only to keep tone before colour; their RAW responses are refitted with the others.
3. **The development's scene white**, independent of the crop and of earlier layers, so Basic stays pointwise.
4. **Whites' positive half** keeps its endpoint remap, moved to the scene side; the re-stretch gets its own study later.

## Recorded choices, delegated to the implementer

| Choice | Recorded |
| --- | --- |
| `c_H`, `c_S` | 3 and 2, the study's, rechecked at the frozen reduction ([the refit](#the-refit)) |
| The scene white's reduction and quantile | Block means to at most 0.25 MP, then the exact order statistic at `⌊0.999 (n − 1)⌋` of their linear luminance, each block summed in `f64` in row order: deterministic, and equal to the reference bit for bit. A fixed-bin histogram was not needed, since the selection over at most 2¹⁸ means is cheap |
| White balance and `W_e` | Exposure scales `W` exactly; Basic's white balance on RAW scales it by its gain on a neutral grey |
| Derived sources (HDR merges) | Take the rendered path: they have no source-stage layer, so they are not scene-referred, as for the scene shift ([Basic on the scene scale](basic-scene.md#limitations)) |
| The GPU programs | One tone program for both parts, the domain in four uniform words; the fused colour run's program count is unchanged |
| Tail placement | After the first Look past the layer with only colour and spatial layers between; a geometry or source layer between, or no Look, keeps the tail with its layer |

## Not in scope

- Whites' positive-half re-stretch.
- The shape at Highlights +75 and +100 and at Blacks −75 and −100.
- Lightroom's surround locality ([decided](#decided), 1).
- Dehaze with Contrast.
- Masks' own alignment (TASK-009 of the [alignment plan](../../tasks/lightroom/lightroom-alignment.json)). A masked Basic layer takes this operator as a global one does.

## Files

- [`crates/luxforge-reference/src/tone.rs`](../../crates/luxforge-reference/src/tone.rs): the frozen operator (`Domain`, `scene_tone`, `rendered_tone`, `scene_white`, `exposed_white`), its fixture [`tone-domain-cases.json`](../../fixtures/basic/tone-domain-cases.json) and, in `tests/studies/tone.rs`, the dense monotonicity proof of the scene part at every corner of its end strengths.
- [`crates/luxforge-core/src/modules/basic/tone.rs`](../../crates/luxforge-core/src/modules/basic/tone.rs) and [`tone.wgsl`](../../crates/luxforge-core/src/modules/basic/tone.wgsl): the unit in either domain; `basic/mod.rs` compiles the scene part and the rendered tail.
- [`crates/luxforge-core/src/modules/registry/compile.rs`](../../crates/luxforge-core/src/modules/registry/compile.rs): the tail's placement, the running scene white and prefixes (`compile_prefix`, `compile_range`).
- [`crates/luxforge-core/src/source/linear.rs`](../../crates/luxforge-core/src/source/linear.rs): `measured_scene_white`, exact against the reference.
- [`crates/luxforge-reference/src/lr_align.rs`](../../crates/luxforge-reference/src/lr_align.rs): `floored_joint_knots`, the fold's knot choice.
- [`crates/luxforge-reference/src/local_tone.rs`](../../crates/luxforge-reference/src/local_tone.rs): the study's candidate operators (box, guided and fast guided bases, picture levels, both reconstructions) and their known-answer tests. None is frozen.
- [`xtask/src/lr_align/tone_study.rs`](../../xtask/src/lr_align/tone_study.rs): `cargo xtask lr-align tone-study`, the fits, measures, observations, pairs and contact sheets.
- [Task plan](../../tasks/lightroom/basic-local-tone.json).
