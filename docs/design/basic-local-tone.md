# Basic tone that adapts to the picture

Status: **proposal awaiting the owner's review**. This is the level C change ([A1, A8](lightroom-alignment.md#decided)) that the [Lightroom alignment](lightroom-alignment.md#round-1-photographs-and-round-2) recorded for Highlights and Shadows: Lightroom Classic's answer the charts and the photographs differently, and no single global curve meets both. A study measured why, and asked the same of Whites, Blacks and Contrast. It recommends a change that is not local. Highlights, Shadows and Whites act on a RAW photograph's scene values relative to the photograph's own scene white, and Blacks and Contrast act after the Look. Nothing in production has changed. The prototypes are study code (`luxforge_reference::local_tone`, `cargo xtask lr-align tone-study`), and the [task plan](../../tasks/lightroom/basic-local-tone.json) is not authorized.

## Why

On the realigned build, at the identity, measured through the CPU reference renderer:

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

- **Today's curve, before or after the Look.** `global-pre`, `global-post`; `global-post-oklab` keeps Oklab chroma.
- **Edge-aware bases** under the same curve, `E' = T(B) + (E − B)`. Each has a box-mean control.
  - A self-guided filter on a 4× reduced grid at radii of 1.6 to 20% of the long side, with `eps` from 0.01 to 0.16. Clarity's tiled base is the 1.6% case.
  - He and Sun's fast guided filter, whose coefficients come from a whole-frame reduction of at most 128 or 256 samples on the long side and are applied at full resolution. It needs no halo.
- **The curve within the picture's own range.** Black and white points are read at quantiles of the frame being acted on (`adapt-*`).
- **The curve over the scene's white.** Before the Look, the scene values are divided by `c × W` and multiplied back after, where `W` is the photograph's 0.999 quantile of scene luminance and `c` is 1 to 4 (`white-pre-*`). The alternative is a fixed anchor 1, 1.5 or 2 stops lower on every photograph (`global-pre-shift*`).
- **Splits on round 2's pairs.** Some fields act on the scene-white curve before the Look and the rest after it (`split-*`).

Worst residuals at Lightroom values of ±25 and beyond (median CIEDE2000). For a single field: the worst joint residual, the worst excess over the class floors, and each photograph's residual at the joint strength. The full tables, every candidate's contact sheets and the per-photograph strengths are in the study's evidence (`study.md`, `tone-study.json`, `sheets/`).

| Field | Candidate | Worst | Excess | Photographs at the joint strength | Surround locality at −100 (Lightroom 10.7) | Photographs' fine-band energy at −100 (Lightroom 0.99) |
| --- | --- | --- | --- | --- | --- | --- |
| Highlights | today, before the Look | 2.97 | 1.71 | 0.6 to 4.2 | 0 | 0.84 |
| Highlights | after the Look | 4.27 | 3.54 | 0.8 to 8.2 | 0 | 0.74 |
| Highlights | guided base 10%, `eps` 0.04, before the Look | 3.16 | 1.45 | | 10.3 | 1.08 |
| Highlights | **scene white, `c` = 3** | 2.97 | 0.49 | **0.2 to 1.5** | 0 | 0.91 |
| Highlights | scene white, `c` = 4 | 2.95 | 0.22 | | 0 | 0.93 |
| Highlights | fixed anchor 2 stops lower | 2.95 | 0.05 | 0.2 to 2.5 | 0 | 0.96 |
| Highlights | scene white `c` = 3, fast guided base 10%, `eps` 0.04 | 3.16 | 0.27 | 0.3 to 1.5 | 10.6 | 1.02 |
| Shadows | today, before the Look | 2.20 | 0.16 | 0.5 to 2.9 | 0 | |
| Shadows | after the Look | 2.70 | 1.24 | | 0 | |
| Shadows | **scene white, `c` = 2** | **1.80** | 0.21 | 0.7 to 3.2 | 0 | |
| Shadows | scene white, `c` = 3 | 2.13 | 0.29 | 0.7 to 3.0 | 0 | |
| Shadows | fixed anchor 1.5 stops lower | 2.88 | 0.39 | 0.7 to 4.7 | 0 | |
| Whites | today, before the Look | 7.87 | | 4.6 to 11.6 at +100 | 0 | |
| Whites | best of any candidate | 7.00 | | | | |
| Blacks | today, before the Look | 7.34 | 4.77 | | 0 | |
| Blacks | **after the Look** | **2.59** | 0.09 | | 0 | |
| Blacks | scene white, `c` = 3 | 10.34 | | | | |

The floors are each curve's shape, which no strength removes:
- **Highlights:** the charts' own best strength leaves 2.16 at +75 and 2.98 at +100, at the stable limit of ±150 ([A10](lightroom-alignment.md#decided)).
- **Whites +100:** 3.59 on the charts at a strength of 200, and 4.5 to 7.6 on each photograph alone.
- **Blacks:** 2.57 at −100 and 2.17 at −75 on the charts. After the Look, single photographs' own floors at −100 reach 1.5 to 4.7, though the pooled photographs fit within 2.5.

Round 2's photograph pairs at today's responses (Exposure a plain gain), as the pooled median CIEDE2000 over the five photographs. Dehaze with Contrast is not rendered, because the study has no Dehaze.

| Arrangement | Contrast + Whites | Contrast + Blacks | Exposure + Highlights | Exposure + Whites |
| --- | --- | --- | --- | --- |
| Today: all before the Look | 12.43 | 12.96 | 4.90 | 5.92 |
| All after the Look | 6.27 | 4.64 | 10.81 | 7.99 |
| All on the scene-white curve, `c` = 3 | 16.00 | 19.23 | 1.46 | 6.15 |
| **Highlights, Shadows and Whites on the scene-white curve; Blacks and Contrast after the Look** | **5.92** | **4.64** | **1.46** | **6.15** |
| The same, with a fast guided base for Highlights and Shadows | 6.27 | 4.64 | 2.08 | 7.99 |

The remaining Exposure + Whites figure is Whites +100 on the two dim photographs darkened a stop (21 to 26 there), the re-stretch above. The remaining Contrast figures cannot yet be divided between the Contrast curve and its partners: Contrast alone was never exported on the photographs.

### What the study shows

- **Adapts to content, globally.** Highlights' conflict between charts and photographs is about where the curve's white sits. On RAW, Lightroom's Highlights acts on scene values relative to the photograph's own white, about 1.6 stops above it (`c` ≈ 3, where today's curve puts that white at scene 1.0). Anchored there, every photograph fits within 1.5 at the strength the charts need, against up to 4.2 today. A fixed anchor fits the pooled photographs as well, but not the dim ones: it reaches 2.5 for Highlights and 2.9 to 4.7 for Shadows. Shadows behaves the same way with its white lower (`c` ≈ 2) and then passes A8. Neither needs a local base to fit.
- **Locality is a difference in kind that matters only on synthetic surrounds.** An edge-aware base reproduces Lightroom's Highlights locality on the surround target and its preserved fine detail. But it makes the photographs five times as locally varied as Lightroom's (tone locality 1.7 against 0.36) and worsens Exposure + Highlights from 1.46 to 2.08. It does not lower Shadows' floor (2.12 to 2.03). A base wide enough to match also needs a radius of 5 to 10% of the long side. At 60 MP that is a halo of 1000 to 2000 pixels, past the 512-pixel bound, so it could only come from a whole-frame estimate.
- **Rendered, not scene, for Blacks and Contrast.** Blacks fits with one strength on charts and photographs once it acts after the Look: 7.34 falls to 2.59, the charts' own floor. The Contrast pairs fall from 12.4 and 13.0 to 5.9 and 4.6 the same way.
- **Whites** is not local and does not adapt through the scene white. Its negative half already fits. Its positive half differs in kind: Lightroom re-stretches a picture's top towards its own white, and the endpoint remap cannot do that at any strength. Of the arrangements studied, the scene side fits its pairs best.

## Design

### The operator

Basic's tone unit becomes two parts, around the Look. The curves inside each part are the frozen [Basic tone](basic-tone.md) stages; only their domain and place change.

**The scene part.** Whites, then Shadows, then Highlights, acting on the input Basic's own Exposure and white balance leave. On a scene-referred input, a RAW photograph's development, each stage runs in a domain scaled to the photograph's **scene white** `W_e`:

```text
W_e      = W × 2^exposure                              W: the development's scene white (below)
u_S      = L / (c_S × W_e)                              Shadows' domain, c_S = 2
u_H      = L / (c_H × W_e)                              Whites' and Highlights' domain, c_H = 3
whites(x), shadows(x), highlights(x) as frozen, on x = encode(u); L' = c × W_e × decode(x')
```

Composed in that order, the result is rebuilt by the luminance ratio with the near-black rule. Values past the domain's white pass through, as Highlights and Shadows already do outside `[0, 1]`. On a rendered input (a JPEG, a derived source) there is no scene white: the domain is today's (`c × W_e` is 1), and the scene part is today's Whites, Shadows and Highlights.

**The rendered part.** Blacks, then Contrast, then Vibrance and Saturation, acting after the Look on rendered values, on today's encoded luminance, unchanged. Vibrance and Saturation follow the tone, as Basic's frozen internal order has them. On a rendered input the rendered part follows the scene part directly.

**The scene white** `W` is a property of a RAW photograph's development, not of the stage. It is the 0.999 quantile of linear luminance over the developed planes, reduced by block means to at most 0.25 MP. It is computed once on the source worker when the photograph is developed, and kept with that development. It depends on neither the crop nor any other layer, so cropping does not change tones. Exposure scales it exactly, because Exposure is a plain gain on RAW.

The constants are the study's (`c_H` = 3 and `c_S` = 2 at a quarter of full size, measured on the study's quantile). The fold refits them at the frozen reduction, with the responses.

### Order on RAW and JPEG

| Input | Today | Proposed |
| --- | --- | --- |
| RAW | Scene unit (white balance, Exposure) → Whites/Blacks → Shadows → Highlights → Contrast → Vibrance/Saturation → **Look** → Tone curve → … | Scene unit → Whites → Shadows → Highlights (scene-white domain) → **Look** → Blacks → Contrast → Vibrance/Saturation → Tone curve → … |
| JPEG | Scene unit → Whites/Blacks → Shadows → Highlights → Contrast → Vibrance/Saturation | Scene unit → Whites → Shadows → Highlights → Blacks → Contrast → Vibrance/Saturation |

Exposure and white balance stay first, so the Look's shoulder still rolls off what Exposure raises, and a lower Exposure still recovers a RAW photograph's headroom ([Basic on the scene scale](basic-scene.md)). On a JPEG the only change is that Blacks follows Highlights and Shadows instead of sharing Whites' remap. The two remaps no longer share their gap clamp, so each part's monotonicity is proved again on its own, densely, as [Basic tone](basic-tone.md#monotonicity-the-dense-proof-and-its-scope) does today.

### One layer, two places

Basic stays one layer with one payload, one history entry per commit and one `edit.set-basic`. Placing its rendered part is the host's job:

- A colour operation may carry a **rendered tail**. The registry, compiling a stack whose input is scene-referred, places a layer's tail directly after the stack's rendering layer (the Look) when one follows it, and otherwise directly after the layer's own units. On a JPEG, and on a RAW stack whose Look is neutral, the result is the same.
- The layer's mask, compiled against the stage the layer receives, applies to both parts. The Look keeps the stage, so one mask serves both.
- The neutral picker still reads the stage before Basic; Auto tone's forward model compiles Basic and its tail around the Looks it already evaluates.

A second Basic layer for the tail was considered and rejected. One panel's values would live in two layers: their history, undo, presets and `edit.set-basic` would no longer be atomic, and a mask would have to be kept identical on both.

### Cost and the performance rules

Both parts are pointwise. They join the stack's colour run (the Look and the Tone curve are pointwise too), so the GPU still draws them in one fused pass with no new program boundary.

- **Halo, scratch, GPU window and staged sweeps:** none. The scene white is a compile-time constant like any slider value, so the unit has no global estimate, no light link and no plane.
- **Point queries** stay `O(layers)` ([rule 4](../engineering/performance-rules.md#rules)).
- **Drags:** a Highlights or Shadows drag changes uniform words. An Exposure drag changes `W_e` with the exposure it already carries.
- **The statistic's own cost:** one reduction and quantile per development on the source worker. It is measured on the development path at 24 and 60 MP and charged to the development, never to the catalog owner or a drag.

### The local option, if chosen

If the owner wants Lightroom's surround locality as well (question 1), the base is the fast guided filter over the scene part's domain:

- Its coefficients `(A, B)` come from the development's luminance reduced to at most 256 samples on its long side. In the scene-white domain they do not change with Exposure. They are kept with the development (at most 350 KiB at 256 samples, 87 KiB at 128) and read bilinearly at each pixel, so the unit stays pointwise with no halo.
- On the GPU they are one small texture per development.
- Point queries read one bilinear sample more.
- The study's base is 10% of the long side with `eps` 0.04.

It is not recommended: see the measured costs above.

### Format, responses, Auto tone and imports

- **Format.** Basic's payload format rises from 3 to 4: Highlights, Shadows, Whites, Blacks and Contrast mean something else on RAW. A layer or preset of format 3 is refused as incompatible, naming its format ([A6](lightroom-alignment.md#decided), current shapes only).
- **Responses.** Rounds 1, 1b and 2 are measured again on a study build of the new operator with its ranges extended as before. `lr-align fold` then refits `response.json` for Contrast, Highlights, Shadows, Whites, Blacks, Vibrance and Saturation, with the owner's Whites review kept. The study's joint strengths are the expected starting point: Highlights −111 and +86 at ±100; Shadows −20 and +21; Blacks −84 and +62 after the Look.
- **Auto tone.** The forward model uses the same split, the same scene white and the same Looks. Searched fields (Exposure, Whites, Blacks) search through it unchanged in method. Ruled fields (Contrast, Highlights, Shadows) keep their picture-statistic triggers and bounds in picture terms, expressed through the refitted responses. On the corpus, Auto's per-photograph outputs are recorded beside today's.
- **Preset import** keeps transferring Lightroom's Basic values as the same numbers ([A11](lightroom-alignment.md#decided)).

## Acceptance

Measured through the CPU reference renderer at full size with `lr-align measure` after the fold, on rounds 1, 1b and 2:

1. **Highlights:** within A8 jointly on charts and photographs from −100 to +50. The photographs are within A8 at every value. At +75 and +100 the charts are no worse than today's floor (2.2 and 3.0), recorded as the curve's shape.
2. **Shadows:** within A8 jointly at every value.
3. **Blacks:** within A8 from −50 to +100 jointly. At −75 and −100 no worse than the charts' own floor (2.2 and 2.6).
4. **Round 2's photograph pairs:**
   - Exposure + Highlights is within A8.
   - Contrast + Blacks and Contrast + Whites are recorded at no more than 5.0 and 6.5.
   - Exposure + Whites is no worse than today's 5.9 beyond the recorded Whites +100 re-stretch.
5. **No regressions:** Contrast, Whites, Vibrance and Saturation on the charts no worse than today. The JPEG renders of a payload without Blacks are unchanged within the tone tolerance.
6. **Reference and production:**
   - The new reference is frozen with exact fixtures, its dense monotonicity proof and the scene white's definition.
   - The CPU units are exact against it, and the WGSL within the declared tolerance.
   - A format 3 layer and preset are refused naming the format.
   - Point queries through Basic stay `O(layers)`.
   - The scene white is crop-independent and scales exactly with Exposure.
7. **Later, not gating delivery:**
   - The owner reviews the study's and the fold's sheets.
   - Whole-corpus `gpu-qualification` with the RAW manifest passes on the M4.
   - The performance checklist is answered with the development-path cost.
   - A confirming Lightroom round, which adds Contrast alone on the photographs, records the result.

## Questions for the owner

1. **Local or global.**
   - *Recommended:* adopt the global scene-white adaptation, which fits charts and photographs within A8 apart from the curves' floors. Record Lightroom's edge-aware Highlights on the surround target as a remaining difference in kind.
   - *Alternative:* add the fast guided base now. It matches that locality and the preserved fine detail, but photographs get five times Lightroom's tone locality, Exposure + Highlights worsens, and each development keeps up to 350 KiB of coefficients.
2. **Blacks, Contrast, Vibrance and Saturation after the Look on RAW.**
   - *Recommended:* move them, which brings the Contrast pairs from 12 to 13 down to 5 to 6 and Blacks from 7.3 to 2.6.
   - *Alternative:* keep them before the Look and accept those residuals.

   Vibrance and Saturation follow only to keep tone before colour. Their RAW responses were never measured on photographs, and the fold and confirming round measure them.
3. **Where the scene white comes from.**
   - *Recommended:* the development's, so it is independent of the crop and of earlier layers, and Basic stays pointwise.
   - *Alternative:* the stage Basic receives, so it follows the crop and any heal or detail layers before Basic. That makes it a global estimate with a GPU light link, and every read through Basic becomes a whole-stage read.
4. **Whites' positive half.**
   - *Recommended:* keep its endpoint remap, moved to the scene side, and give the re-stretch its own study once this lands.
   - *Alternative:* widen this change to include it now. That would delay this change, since the study has no candidate for it yet.

## Recorded choices, delegated to the implementer

| Choice | Recorded |
| --- | --- |
| `c_H`, `c_S` | 3 and 2, the study's; refitted in the fold at the frozen reduction, each to a whole or half number |
| The scene white's reduction and quantile | Block means to at most 0.25 MP, then the 0.999 quantile of linear luminance, from a fixed-bin histogram so the result is deterministic and tile-invariant |
| White balance and `W_e` | Exposure scales `W` exactly; Basic's white balance on RAW scales it by its gain on a neutral grey |
| Derived sources (HDR merges) | Take the rendered path, as they take the scene shift today ([Basic on the scene scale](basic-scene.md#limitations)) |
| The GPU programs | One tone program with a mode word (scene part or rendered part), or two; whichever keeps the fused colour run's program count no larger |

## Not in scope

- Whites' positive-half re-stretch.
- The shape at Highlights +75 and +100 and at Blacks −75 and −100.
- Lightroom's surround locality (question 1).
- Dehaze with Contrast.
- Masks' own alignment (TASK-009 of the [alignment plan](../../tasks/lightroom/lightroom-alignment.json)). A masked Basic layer takes this operator as a global one does.

## Files

- [`crates/luxforge-reference/src/local_tone.rs`](../../crates/luxforge-reference/src/local_tone.rs): the study's candidate operators (box, guided and fast guided bases, picture levels, both reconstructions) and their known-answer tests. None is frozen.
- [`xtask/src/lr_align/tone_study.rs`](../../xtask/src/lr_align/tone_study.rs): `cargo xtask lr-align tone-study`, the fits, measures, observations, pairs and contact sheets.
- [Task plan](../../tasks/lightroom/basic-local-tone.json).
