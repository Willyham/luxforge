# Grain

Status: implemented behind the off-by-default [`grain` flag](#the-grain-flag) and verified on Linux ([task plan](../../tasks/editing/grain.json)): the [grain study](grain-study.md) and its `f64` reference, the module on both evaluation paths, the GPU program (device tests and the `grain` corpus family on RADV), the desktop section and its `grain` smoke scenario (under Xvfb), the API, presets, copy and paste, Lightroom preset import and the alignment rig's grain families. Open: native M4 qualification (the rendered tier's background `grain` scenario on the native GPU frame and the whole-corpus gate, the corpus's RAW grain cells included) and the [measurements](#resource-and-responsiveness-constraints), and the [Lightroom alignment round](#verification-and-acceptance), which needs the owner's grain export. Its [decisions](#decided) are the owner's; its [recorded defaults](#proposals-with-recorded-defaults) are the implementation's until the owner changes them.

Grain adds synthetic film grain to a photograph: a built-in module, `luxforge.grain`, with Lightroom's three controls, Amount, Size and Roughness, generated controls and generated API methods. It builds on the delivered [vignette](presence-mixer-vignette.md#vignette-one-positional-unit), whose finish stage and positional colour units it reuses: grain is a pattern that is a function of the pixel's position and the three sliders alone, so it needs no neighbourhood, no halo, no spatial primitive and no new host stage.

## Outcome and scope

| Module | Controls | Host additions |
| --- | --- | --- |
| Grain (`luxforge.grain`) | Amount, Size, Roughness | No new stage: a finish-stage layer of positional colour units, ordered after the vignette by its declared order. The host's one addition is module-level flag gating: a module declares the flags it is offered under, and the catalog owner refuses its actions other than its reset while one is off ([the grain flag](#the-grain-flag)) |

In scope: JPEG and RAW photographs and derived sources, through the same module on both evaluation paths, the GPU program, the desktop section, the API, presets, copy and paste, Lightroom preset import of the three settings, and a Lightroom alignment round for their responses at the end. Out of scope, with no placeholders: colour (chroma) grain, grain in masks (Lightroom has no local grain), film-stock presets or emulations, grain matched to a photograph's own noise (the [AI fill's](ai-editing.md) re-synthesis is a separate stage with a different purpose), a per-photo or stored random seed, and any approximate preview processing beyond the reduced-stage rule below.

Lightroom is the familiarity reference for names, ranges, defaults and placement. Its responses are the alignment target ([Lightroom alignment](lightroom-alignment.md)), measured at the end; until then no value is claimed as Lightroom-equivalent.

## Module boundaries

| Component | Owns | Does not own |
| --- | --- | --- |
| Grain module | Parameters, validation, the neutral rule, controls, the noise field and the application equation as one positional colour unit, its GPU program and its responses | Where the finish stage sits, output-stage coordinates, quantization, ordering among layers |
| Host | The finish stage, the declared order within it, positional colour runs on both evaluation paths, reduced-stage compilation (`CompileStage::scale`), point samples, the GPU plan | Any grain equation |

One editable layer with a fixed internal order, updated in place, exactly as the vignette is: `single_layer`, a neutral payload is a legal layer that compiles to nothing, a reset keeps the layer, two grain layers fail with `validation: ambiguous Grain layers`, and a layer of another payload format is refused, never rewritten.

## Placement

Grain is a `finish` effect (`luxforge.grain.film`) with declared `order: 10`, so a new grain layer is placed after the vignette (order 0) whichever was touched first, and both after the geometry tail. Lightroom applies grain after its post-crop vignette, so darkened corners carry grain as the frame's body does. The canonical stack becomes: source (RAW only) → Basic → mixer → presence → orientation → crop → vignette → grain. A finish layer before a geometry layer keeps failing compilation as it does today.

## Controls

| Field | Range | Default | Meaning |
| --- | --- | --- | --- |
| Amount | 0..100, step 1 | 0 | The grain's strength. 0 is the exact identity whatever Size and Roughness hold, and the layer is then neutral |
| Size | 0..100, step 1 | 25 | The grain's particle size, in pixels of the full-resolution output stage ([decided](#decided)) |
| Roughness | 0..100, step 1 | 50 | The grain's regularity: low is fine and even, high is uneven and clumped |

A missing key means the field's own default, as in the vignette, so `{}` is the all-default payload. One `set-grain` field patch and `reset-grain`; labels `Grain amount 40`, `Grain size 25`, `Grain roughness 50` and `Reset Grain`. The fields are presettable, so presets and [copy and paste](copy-settings.md) carry a Grain group with no change to either. The module is not maskable.

## Processing contract

The unit is one `PointwiseColor` in the finish stage's colour run, compiled against the output stage its layer receives, as the vignette is: its footprint is `max(1, full / stage)` per axis from the compile stage's full-resolution and evaluated sizes, and each slider's strength passes through the module's frozen response table, the identity until the [alignment round](#verification-and-acceptance) folds Lightroom's. The [grain study](grain-study.md) freezes its equations against an independent `f64` reference, within these requirements:

- **A pure function of position.** The grain at an output pixel is a function of that pixel's integer position in the full-resolution output stage, the three slider strengths and one frozen seed constant, and of nothing else: not the tile, the window, the zoom, the thread count, the renderer or the photograph. Rendering in any tile size, any window and on either renderer addresses the same field, and `render.sample` reads the rendered byte by construction.
- **Exact integer hashing.** Pseudo-random lattice values come from an integer hash of the lattice coordinates and the seed in `u32` wrapping arithmetic, which the CPU and WGSL compute identically; only the interpolation and the application are floating point. The GPU program then differs from the reference by f32 rounding of smooth functions alone, which the pointwise class's limit covers.
- **A normalised field.** The field has zero mean and unit standard deviation for every Size and Roughness, with bounded tails, so Amount alone sets strength, Size sets the dominant feature size in output pixels (Size 0 about one pixel), and Roughness sets the irregularity at fixed strength. The study chooses the noise construction (lattice gradient or value noise over a few octaves, a clumping modulation or other) and the Size and Roughness mappings, and records them with their spectra.
- **Luminance grain, preserving colour.** The field moves luminance in the tone study's encoded domain ([Basic tone](basic-tone.md)), with chroma kept by the luminance-ratio reconstruction and the near-black rule the existing units use. Its strength is weighted by tone so that it vanishes at black and at white: grain never lifts a black or tints a highlight, a below-white channel never passes white, and values above white, the RAW path's headroom, are unchanged. The application is mean-preserving to the study's tolerance on flat fields at every tone, so grain neither brightens nor darkens the picture.
- **Reduced stages.** A finish layer compiled at a stage reduced from full resolution (a drag's frame below 100% on the GPU, the CPU proxy of a session without a GPU) cannot address the full-resolution field pixel for pixel. There the unit evaluates the field at each stage pixel's centre in full-resolution coordinates, band-limited and attenuated so that its standard deviation and spectrum match the full-resolution grain reduced by the same area average, to the study's frozen statistical tolerance. The picture at rest, samples, the histogram and export always render the full-resolution field, at rest reduced as GPU-first reduces every stack ([GPU-first](gpu-first.md#the-contract)); the reduced-stage frame is motion, whose differences are reported, not gated. A CPU proxy frame of non-neutral grain at a reduced stage is therefore labelled approximate (`ProxyApproximation::stand_in`), as [instant previews](instant-preview.md) label a spatial layer's.
- **Non-finite values** fail as colour runs do; alpha is never touched; a neutral payload compiles to no unit and keeps the identity byte path; the GPU shape of a neutral drafted layer is an exact identity.

The GPU program (`crates/luxforge-core/src/modules/grain/unit.wgsl`, entry `lf_grain_film`) is a colour program the plan places in output space after the geometry tail and the vignette. It never recomputes the compile: its eleven uniform words are the CPU compile's six lattice steps and the five `f32` coefficients the CPU's per-pixel path multiplies by (the three gains, `4 beta` and the mean correction's coefficient), and it evaluates the field at the stage pixel the plan's position map hands it, which is the position the CPU unit is handed. A window at 100%, a tile of the picture at rest and a reduced stage therefore address the same field as the reference renderer, and a reduced stage's footprint reaches the program through the steps and gains compiled for it.

Grain is judged at 100%. A downsized export resamples the full-resolution terminal values in linear light ([export settings](export-settings.md#rendering)), so it averages grain as it averages any fine detail, and output sharpening, which follows, sharpens grain with the rest.

## API

Generated from the descriptors: `edit.set-grain` and `edit.reset-grain`, discoverable through `module.list` and `schema.list` whatever the [flag](#the-grain-flag) says, each patch validating exactly the fields sent, storing the patch as sent, deduplicating by request id and creating no entry for a patch that changes nothing. `draft.begin` accepts the patch action, `recipe.describe` reports the layer's values, and `render.sample` and `analysis.request` keep their identities. Parity is verified through the [field-patch conformance chapter](../engineering/development.md#the-field-patch-conformance-chapter).

## The grain flag

Grain is an experiment behind the live toggle `grain` ([settings and feature flags](settings-and-flags.md#gated-grain)), off by default and listed in Settings › Experiments after AI object picking. It gates what is offered and what writes new grain, never how a stored grain layer renders: a recipe's grain layer renders, samples, exports and describes identically whatever the flag says.

The module declares the flag (`flags: ["grain"]` in its descriptor), so a client knows from the descriptor alone. While it is off:

- the desktop draws no Grain section, offers no Grain group in the create-preset form or Copy settings, and lists no Grain palette entry;
- the host refuses `edit.set-grain`, `draft.begin` of `set-grain` and the commit of a `set-grain` draft, and a settings set carrying `set-grain` in `edit.apply-settings`, its draft's `draft.set` and commit, and `batch.apply-settings`, each as `not-ready` with `data.flag: "grain"`, writing nothing;
- `edit.reset-grain`, undo, redo and history navigation run, since removing grain or replaying a stored entry is not offering it;
- `module.list` and `schema.list` still describe the module and its methods; `edit.set-grain`'s schema carries `flags: ["grain"]` and says when it is refused, and `preset.groups` still lists the Grain group;
- Lightroom preset import still maps grain into the stored preset; only applying it waits for the flag.

On, everything behaves as this design describes. Turning the flag on or off takes effect at once: the desktop reads the flags again on a `flags.set` event and shows or hides the section.

## Presets and Lightroom import

[Preset import](presets.md#import) maps `GrainAmount`, `GrainSize` and `GrainFrequency` (Lightroom's Roughness) to Amount, Size and Roughness at the same number until the alignment round fits their responses. `GrainSeed` is reported as not carried: Luxforge's pattern is a property of the output position ([recorded default](#proposals-with-recorded-defaults)), and a seed qualifies only how the grain is laid out, not how strong or coarse it is. A `GrainAmount` of 0 stays neutral, with its size, frequency and seed listed as qualifying a neutral amount. The planned [Lightroom catalog import](lightroom-import.md) uses the same mapping when it is built.

## Desktop

While the [`grain` flag](#the-grain-flag) is on, a Grain section in the tools panel, after Vignette, collapsed, one group drawn without a header: Amount (unipolar, zero 0), Size (unipolar, default 25), Roughness (unipolar, default 50) and the module's reset. Every slider drafts through the core lifecycle, previews live and commits once on release, key-up or Enter; Escape cancels; a right-click copies the exact JSON request. The section has its accent dot when Amount is non-zero. The section is the generated one, with no grain-specific widget.

The rendered check is `cargo xtask smoke --scenario grain`, over the generated `fixtures/generated/grain.jpg`: a 2400 × 1600 flat mid-grey crossed by a band of black, white and four saturated patches, larger than the canvas at 100%. It opens with the flag off and no Grain section, has a second client turn the flag on and checks the section appears, collapsed, then drags and commits Amount at Fit, commits Size and Roughness at 100%, crops, undoes and redoes, and judges each 100% frame against the same view before any grain: grain shown and even across the view up to the canvas's edges, the grey's mean and neutrality kept, black and white untouched, each colour's mean kept, Size coarser, Roughness changing the pattern, and a crop, undo and redo keeping the grain's size and strength. Fit frames are reduced stages and are recorded, not judged.

## Resource and responsiveness constraints

The unit allocates nothing per frame beyond what a positional colour unit already holds: no table proportional to the frame, no scratch, no cache, no global estimate. Its per-pixel cost is a bounded number of hashes and interpolations, which the module's handoff measures on the reference at 24 MP and 60 MP. A grain drag is a pointwise finish-stage drag and is held to the pointwise drag budget on the GPU at Fit, 33% and 100%; the picture at rest, export and samples cost what a full-resolution finish unit costs. Every implementation handoff answers the [performance checklist](../engineering/performance-rules.md#review-checklist).

## Verification and acceptance

1. **The grain study first.** The [grain study](grain-study.md), the `f64` reference [`crates/luxforge-reference/src/grain.rs`](../../crates/luxforge-reference/src/grain.rs) and hand-built [fixtures](../../fixtures/grain/README.md) under `fixtures/grain/` freeze the hash, the field, the Size and Roughness mappings, the tone weight, the application, the reduced-stage rule and the tolerances: exact per-pixel against the reference at full resolution, statistical (standard deviation per tone, mean shift, radially averaged spectrum) at reduced stages. Fixtures include flat fields at several tones, a grey wedge, saturated colours, black and white, RAW headroom above white, and every slider's ends.
2. **Exactness and bounds.** Production matches the reference within the frozen tolerance; Amount 0 keeps the identity byte path; tiled and windowed renders equal whole-frame ones; mean preservation, black and white invariance, colour preservation and the field's normalisation hold on the fixtures; non-finite values, two grain layers and a finish layer before geometry are refused explicitly without rewriting anything.
3. **The history journey** through the API by the one field-patch conformance suite, and the module's placement in the `editor-acceptance` chapter for finish layers: grain following the vignette whichever was touched first, and a crop update leaving the grain's size unchanged. The original's bytes are unchanged throughout.
4. **The GPU** program held to the reference within the pointwise class's limits by device tests and by `cargo xtask gpu-qualification` over new `grain-*` corpus recipes, at rest at Fit, 33%, 50% and 100%, with motion below 100% reported, and in export, resized export, 16-bit export and samples.
5. **Native M4 rendered evidence** from a background smoke scenario `grain` with correlated state, requests and logs at Fit and 100%, and the measurements recorded in [performance](../specs/performance.md) with their scope.
6. **Lightroom alignment** last: a grain round through the alignment rig, measured as statistics rather than pixels, folded into each field's response, which raises the module's format so recipes of the earlier scale are refused.

## Decided

| # | Question | Decided | Not chosen |
| --- | --- | --- | --- |
| G1 | What Size is measured against | Pixels of the full-resolution output stage: the same settings look the same at 100% on every camera, so grain is finer relative to the frame on a 40 MP photograph than on a 20 MP one, and a crop does not change it | Relative to the source's short side, film-like, as darktable's grain is; deciding it by the alignment round's measurement |

## Proposals with recorded defaults

Recommendations for the owner, recorded as proposals until decided. The implementation runs on the defaults.

| Question | Default the plan runs on | Alternatives |
| --- | --- | --- |
| Section | Its own Grain section after Vignette, one module per section as every module has | An Effects section holding the vignette and grain, as Lightroom's panel does, which needs a section holding two modules |
| The pattern's seed | One frozen constant: every photograph's grain is the same pattern at the same output pixel, so no renderer path, preset or copy has a seed to carry and no evaluation binds one | A seed derived from the source's content digest and bound at evaluation as the scene white is; a stored seed in the payload, as Lightroom's `GrainSeed` |
| Grain colour | Luminance only, as Lightroom's | A colour grain control |
| Order against the vignette | After it (order 10), as Lightroom applies grain | Before it, so corners darken the grain with them |
| Reduced-stage frames | A band-limited, statistically matched field at the reduced stage, reported against the picture at rest and not gated, as every reduced-source drag is | Rendering the full-resolution field and reducing it on every drag frame below 100%, at the cost of a full-resolution pass per tick |
