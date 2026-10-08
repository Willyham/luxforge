# Auto tone

Status: **implemented; owner M4/corpus qualification and Lightroom fitting remain open**. The owner requested a plan for an Auto tone feature that sets the Basic values automatically, and decided its scope on 2026-10-07 ([decided](#decided)). The algorithm's numbers below are starting targets that ship as research defaults and are fitted afterwards. [Tasks](../../tasks/editing/auto-tone.json) track delivery, qualification and the separate owner fitting round.

## How other editors do it

The [research summary](#references) behind this design, with documentation and source code distinguished from community reports:

- **Lightroom Classic, Lightroom and Camera Raw.** The Basic panel's Auto button sets Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Vibrance and Saturation as absolute values. It ignores their current positions, and it does not touch Texture, Clarity, Dehaze or white balance. Auto white balance is a separate entry in the white balance menu. Since December 2017 (Classic 7.1) Auto has been a neural network trained against "tens of thousands of professionally edited photos". Its inputs and architecture are unpublished. Shift+double-click on a slider label runs Auto for that slider alone; for Whites and Blacks this behaves like a search for the clipping point. A develop preset can carry "Auto Settings" (`crs:AutoTone`), which is recomputed for each photo it is applied to. Users report too much lift in the shadows, flattened contrast, a near-constant Vibrance of about +15, and the occasional badly wrong exposure. Sync and multi-photo apply have had bugs that copied one photo's Auto values to the others.
- **Photoshop.** Auto Tone, Auto Contrast and Auto Color are percentile black and white points. Auto Tone sets them per channel, which also removes a colour cast. Auto Contrast clips all channels by the same amount. Auto Color finds the darkest and lightest colours and can optionally pull near-neutral midtones to grey. The clip allowance is a fraction of a percent. The default since CS6, "Enhance Brightness and Contrast", is undocumented.
- **RawTherapee and ART Auto Levels** (`ImProcFunctions::getAutoExp`) work from a histogram of the raw data:
  - **Exposure** blends two estimates: one puts the mean at 18% grey, the other extrapolates the histogram's top octiles to the clipping point.
  - **Black point and highlight compression** come from 0.02% clip percentiles.
  - **Brightness** comes from the median.
  - **Contrast** comes from the octile spread.
- **darktable.**
  - **Pickers:** they compute from mean, minimum and maximum (filmic's auto-tune) or from deciles of log luminance (the tone equalizer).
  - **Exposure "automatic" mode:** it maps a raw percentile to a target in EV.
  - **Scene-referred default:** a fixed +0.7 EV, corrected for the camera's exposure bias.
- **Capture One, Apple and DxO.**
  - **Capture One's Auto Adjust** is a configurable set: white balance, exposure, high dynamic range and levels.
  - **Apple's Core Image `autoAdjustmentFilters`** analyses the image and returns parameterised filters (tone curve, highlight/shadow, vibrance).
  - **DxO Smart Lighting** is a local tone map.
- **Learned approaches.** The MIT-Adobe FiveK data set (5,000 RAW files, each edited by 5 experts in Lightroom) is licensed for research only. Weights trained on it are a real licence conflict for a GPL-3.0 project that permits commercial redistribution. Methods that train on degraded good images (Distort-and-Recover) or on unpaired images (Exposure, Hu et al.) avoid paired expert data, but none is needed here.

What Luxforge takes from this:

- Auto is a command whose output is ordinary slider values, committed as one history entry.
- It is deterministic and explainable. It uses percentile statistics robust to hot pixels and outliers.
- Whites and Blacks are a clipping-point search through the real tone pipeline.
- Targets are expressed in terms of the picture, not slider numbers, so they survive the [Lightroom alignment](lightroom-alignment.md) response changes.
- Presets that carry Auto recompute it for each photo, which avoids Lightroom's sync bug.
- No learned model.

## Behaviour and scope

**Auto tone** sets the eight fields Lightroom's Auto sets on the photo's global Basic layer: Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Vibrance and Saturation. They are set in one entry labelled `Auto tone`.

- **Writes:**
  - All eight fields, absolutely, including zeros, so earlier values of those fields do not survive.
  - It creates the Basic layer when there is none.
  - It leaves Temperature and Tint, the RAW development's white balance and every other module untouched.
- **Repeatable:** it analyses the stage the Basic layer receives (below), as the neutral picker does. Its result therefore does not depend on the Basic values already set. A second Auto on an unchanged photo gives the same values and is a no-op with no entry.
- **Undo:** one undo returns every field to its previous value.
- **Sliders:** they move to the committed values exactly as after any other Basic edit.
- **Refusals:**
  - **Mask target:** refused (`validation: Auto tone applies to the photo's global Basic layer`). Lightroom has no Auto for masks either.
  - **Historical entry or open draft:** refused, as every Basic action is.
  - **No tonal range:** a photo whose analysis finds none is refused with its reason, and nothing is committed. Examples: a black or uniform frame, or too few usable samples.
- **Not in scope**, with no placeholder controls:
  - Auto white balance. The owner chose tone only; it remains a separate later proposal.
  - Per-slider Auto (Shift+double-click).
  - A desktop "Auto selected photos" command. [Presets](#presets-and-import) carry Auto to several photos through `batch.apply-settings`, which analyses each photo separately.
  - A learned model.
  - Texture, Clarity and Dehaze.

### Command and API parity

| Method | Mutates | Result |
| --- | --- | --- |
| `edit.auto-tone {asset_id, mutation}` | yes | Basic's `auto-tone` action. Analyses the current revision and commits one `Auto tone` entry, or a no-op. Revision-checked and deduplicated like every action. Answers the mutation result with the [explanation](#explanation) it used as `analysis.auto-tone` |
| `query.auto-tone {asset_id, entry_id?}` | no | Basic's query: the values Auto would set for the current or named entry, with the [explanation](#explanation), and no commit |

- **Arguments:** `auto-tone` takes no parameters. It is a Basic action, not a field patch, so it is not a draft action and has no Amount.
- **Pixel reads:** it plans through the host's deferred read path, which a pixel-reading picker already uses: the catalog owner never reads the sample or renders a frame.
- **Agent use:** an agent can preview with the query and then commit with the action, or commit directly and read the report from the action's answer. The action and the query share one function, so they cannot disagree for the same identity.
- **Collapse:** an `Auto tone` entry never collapses into the slider edits after it. It sets eight controls, and auto-collapse only merges edits of the control the last edit set.

### Explanation

The query answers a bounded report (at most 4 KiB):

- `algorithm` (`auto-tone/1`) and `values`.
- The sample's dimensions, count and exclusions, and how source clipping was detected (`source_clip_detection`: `jpeg-code-255` or `raw-sensor-white`).
- The statistics each value came from: the luminance percentiles, the bright and dark band fractions, the clipping fractions and the mean chroma.
- The fields that stopped at a bound.
- The layers the forward model used and the layers it left out.

`edit.auto-tone` answers the same report as `analysis.auto-tone` beside its mutation result, also on a no-op: the report its values came from, added to the answer after the commit and never stored with the request, so a deduplicated retry, which analyses nothing, carries none. A preset whose Auto step applied answers it the same way. The desktop shows the report's text in the Auto button's tooltip after a run, taken from that answer; it asks no query, so a click analyses and solves once. The query exists so a person or an agent can see why Auto would choose a value without committing.

## Analysis input

Auto reads one bounded **analysis sample**: linear RGB values of the stage the Basic layer receives, at a fixed grid over the **output frame**. The grid is the tile layer's generic sample grid (`tiles::grid`, `StageQuestions::grid_before`): it knows nothing of Auto, which owns only what it computes from it.

- **Grid and mapping:**
  - The grid is drawn over the output frame, after lens, perspective, straighten and crop, so the analysis covers what the picture shows.
  - Each grid point maps back through the stack's geometry to the Basic input stage. The value read there is the input pixel nearest that point, without interpolation.
  - The long side has at most 1024 points (a 24 MP 3:2 frame gives about 700,000), at pixel centres `(i + 0.5) · W / gx`.
  - Points that land outside the input stage are dropped.
  - Point-sampling keeps the photo's distribution of values. An area average would narrow it and hide clipped highlights.
- **Renderer:** the tile service reads the sample, from the GPU's tiles where the GPU draws the stack and from the reference renderer otherwise. It is the same deferred path `render.sample` and the neutral picker use, extended from a patch to a bounded strided grid.
  - The GPU renders the prefix tiles and gathers the grid with no full-frame CPU buffer.
  - The reference shares its per-call stage evaluation. A spatial prefix currently materializes its reference frames, as [performance rule 4](../engineering/performance-rules.md#rules) documents. Auto keeps only the bounded sample after that call.
- **Exclusions:** samples that are non-finite, or whose source was already clipped, are flagged.
  - **JPEG:** the source pixel under the point has code 255 in any channel.
  - **RAW:** a sensor site the pixel develops from is at the sensor's white. The developed planes cannot say so: a Bayer demosaic limits each site after its white-balance gain, X-Trans does not, and the camera matrix mixes the channels before the planes are stored. So the flag reads the retained mosaic, as the neutral picker does (`RawSource::sensor_clipped_at`): the point maps through the default crop and orientation, and a corrected DNG's warp per channel, to where the pixel is read; it is flagged when any site within one site of there, which holds the nearest site of each Bayer colour, is at or above the picker's clip threshold, 0.995 of the range above that site's black level, before white balance and any optical gain, with the DNG's repairs applied. A linear or monochrome layout reads the pixel's own samples. The prepared source carries the sensor to the read, which holds it only for that call.
  - Finite clipped samples count in the luminance statistics. Non-finite samples are counted in the report but excluded from every numerical statistic.
  - Clipped samples are left out of the clipping fractions, so Auto never greys out a sky that was already clipped in the file.
- **Identity:** asset, source fingerprint, the content of the layers before Basic, the geometry after it, grid dimensions, the input mode, how clipping is detected and the renderer. Nothing of Auto is part of it.
  - Basic's own values, the Look and later colour layers are not part of it, so a repeated Auto or a query followed by the action reads the sample once.
  - Retained grids live in one least-recently-used cache per render context, capped at 32 MiB in total and shared by every client, the desktop and API clients alike; a read evicts the least recently used grids before it builds its own.
  - Removing a photo to Removed, or deleting its records, releases its grids; a read under way then cannot put its grid back.
- **Cancellation:** the sample read and the solve run under the request's cancellation and stop within a row, tile or solver chunk; one still waiting in the tile service's queue is answered as soon as its turn comes. The owner cancels a parked `edit.auto-tone` (or preset) read ([detail](detail.md)):
  - when the same client parks another read for the same method and photo, or opens another photograph (`source.prepare`, which every desktop open sends): the call is answered `cancelled`;
  - when, after any message, the photo's entry or revision or the client's draft is no longer the one read: the call is replayed as a stale read is, answering `conflict`;
  - when the client disconnects: the call is dropped unanswered.

  A batch's reads belong to its job, not the client that started it: only cancelling the job stops them. `query.auto-tone` is cancelled only by its client's disconnect. A late result never commits: every replay checks what its read was read from before it writes.

## The solve (`auto-tone/1`)

The solve runs off the owner, in a pure function in `luxforge-core` over the sample, on the tile service's thread right after the sample read (the desktop's GPU tile worker, or the reference service's `luxforge-tiles` thread), so other pixel reads wait behind it.

**Forward model.** The global Basic layer at candidate values, keeping its stored white balance, followed by the global Look layers after Basic, then the output boundary's clamp. A Look before Basic is part of the analysed input, so it is not applied again. It reuses the modules' own compiled pointwise units, so Auto solves through what renders. Basic's module builds it once per analysis (`auto_tone::forward_model`, in `modules/basic/auto/` with the solver); the fitting rig, the measurements and the GPU agreement test solve through the same model.

- Leaving out the Tone curve, the mixer, Presence, the vignette and every masked layer is deliberate. Auto sets Basic for the photo as its look renders it, not to undo the person's creative layers.
- The explanation lists the layers left out.
- When the [Lightroom alignment](lightroom-alignment.md) changes Basic's responses, Auto keeps its picture targets and finds new values without a change of its own.

**Statistics.**

- `y` is the output's encoded Rec. 709 luminance.
- `P_q` is a weighted percentile of `y` over the sample.
- The clipping fractions count non-excluded samples with any output channel at 1 (highlight) or 0 (shadow).

**Steps**, in fixed order. Every search runs over the field's own grid, so the committed value meets its constraint exactly:

1. **Exposure** `E` within ±4 EV.
   - Find, on the 0.01 EV grid, the `E` whose median `P_50(y)` is closest to `m = 0.46`.
   - Then lower it, if needed, until at most 2% of samples have `y ≥ 0.98`.
   - The search is monotone when every layer after Basic in the model declares its luminance response monotonic for its payload (`ToolModule::monotonic_luminance`). The Look declares it up to amount 100; above that it extrapolates and can reverse luminance, so the model evaluates all 801 Exposure values, then takes the nearest lower guard-compliant value. The report names the search. This can be substantially slower; it remains cancellable at every chunk.
2. **Highlights** `H` within −100..0.
   - `H = −min(100, 250 · f_hi)`, where `f_hi` is the fraction with `y > 0.80` at `E`.
3. **Shadows** `S` within 0..60.
   - `S = min(60, 200 · f_lo)`, where `f_lo` is the fraction with `y < 0.20`.
   - The cap answers the over-lift complaint.
4. **Contrast** `C` within −50..50.
   - `C = 100 · (0.60 − (P_90 − P_10))`, measured with the values so far.
5. **Whites** `W` within −60..60: the largest integer whose highlight clipping fraction is at most 0.05%. Binary search.
6. **Blacks** `B` within −60..60: the smallest integer whose shadow clipping fraction is at most 0.05%. Binary search.
7. **Second sweep.** Repeat step 1 with the other tone values fixed, then steps 5 and 6. Exactly two sweeps.
8. **Vibrance** and **Saturation.** `ĉ` is the mean Oklab chroma of non-excluded, non-near-black output samples at the final tone values.
   - Vibrance is `clamp(60 · (1 − ĉ / c_v), 0, 25)`, with `c_v = 0.07`.
   - Saturation is `clamp(−60 · (ĉ / c_s − 1), −15, 0)` when `ĉ > c_s = 0.14`, and 0 otherwise.

**Rounding and determinism.**

- Values round half away from zero to the fields' steps: 0.01 EV for Exposure, integers for the rest.
- The solve uses `f64` for statistics and deterministic weighted reductions, so the same sample gives the same values on every machine.

**Bounds of the search.**

- The clipping fraction is monotone in Whites and Blacks for Basic alone, which the [tone study](basic-tone.md) proves. Through a Look the binary search still returns a value that meets the constraint, the largest one where the fraction is monotone.
- Repeated evaluation may use a reduction of the sample, provided tests prove it gives the same fractions and percentiles as brute-force evaluation of the whole sample.

**Refusal.** Fewer than 1,024 usable samples, an input luminance spread `P_99 − P_1` under half a stop, or a median at or below `1e−6` is refused with the reason.

The constants above are the starting targets. They live in one versioned `AutoToneTargets` and are tested as a set. [Tuning](#tuning-against-lightrooms-auto) replaces them as `auto-tone/2`. A change of targets changes only future Auto results: Auto stores values, not a flag, so no recipe, preset or catalog format changes and existing photographs render as before.

## Presets and import

**The settings set.**

- A settings set may name the analysis step `"auto-tone": {}`, the one key whose value may be empty. A registry answer beside `patch_action` declares which actions are analysis steps; today that is only Basic's `auto-tone`.
- **Order:** the step runs after every field-patch step, whatever the key order, and analyses the intermediate stack those steps produced. A preset's white balance, Detail or Look therefore informs Auto, and the result equals applying the rest of the preset and then Auto.
- **Planning:** a composite with an analysis step plans through the same deferred read as the single action. Other composites keep planning without rasterizing anything.
- **Store-time refusal:** a set naming both `auto-tone` and any of its eight fields under `set-basic` is refused when stored, because Auto would overwrite them. Lightroom refuses the same combination.
- **Label:** the entry is still labelled `Preset: <name>`.
- **No tonal range:** a photo whose analysis finds none skips the step and reports it under `skipped` with the reason. The rest of the preset applies, so a batch continues.

**Capture.** `preset.capture` accepts `{"auto-tone": true}` and returns `{"auto-tone": {}}`. The desktop create form gains an Auto tone row in Basic's Tone group; ticking it clears and disables the eight fields it would overwrite.

**Batch.** `batch.apply-settings` applies through `edit.apply-settings` for each photo, so each photo is analysed separately. One photo's values are never copied to another.

**Lightroom import.**

- `crs:AutoTone="True"` in XMP, or `AutoTone = true` in `.lrtemplate`, maps to the `auto-tone` step. It is no longer refused as `Luxforge has no Auto Tone`.
- The eight fields in the same preset are reported as overridden by Auto tone and not imported.
- `AutoTone` false is neutral.
- The earlier process versions' `Auto*` switches keep their current treatment.
- A Luxforge preset document round-trips the key.

## Desktop

- **Button:** an **Auto** text button in the Tone group's header, where Lightroom places it.
  - It is declared in Basic's descriptor as a header action, and it is drawn by the existing sub-group header with actions. There is no Basic-specific widget code.
  - **Shortcut:** Cmd+U / Ctrl+U, Lightroom's, declared as the action's `shortcut` (`Command+U`) in Basic's descriptor; the desktop's key binding and palette hint come from that declaration.
- **Run:** a click sends `edit.auto-tone` with the expected revision. While the analysis runs the button shows a busy state, and a second click is ignored. The entry then appears in history and the sliders follow it.
- **Disabled, with a tooltip saying why:**
  - on a mask target;
  - for a historical entry;
  - during an open draft or Compare;
  - without a photo.
- **Refusal:** shown in the status bar with its reason.
- **No other UI:** no new preference, panel or dependency.

## Tuning against Lightroom's Auto

The owner chose to fit the targets to Lightroom's Auto on their own photos, after the working feature is delivered. Delivery never waits on it.

**Owner's part.** The owner picks 40 to 100 of their photos, RAW and JPEG, and in Lightroom Classic applies Auto on the current process version with the default profile. They then export two things for each photo:

- the XMP sidecar, for Lightroom's values;
- a full-size sRGB JPEG of the Auto result.

**Fitting.**

- The fitting tool compares picture statistics, not slider numbers: median, P10/P90, the band fractions, the clipping fractions and the mean chroma. It compares Lightroom's Auto export with Luxforge's Auto on the same originals.
- It fits the targets of `AutoToneTargets` to minimise a weighted median difference, holding out a quarter of the photos to check the fit.
- Comparing pictures keeps the fit independent of the slider response alignment, which is not yet done. The difference between the two programs' base renderings is reported beside it.
- Slider values are compared too, for information. They are converted wherever the alignment has a fitted conversion.

**Conduct.** The rig follows the alignment programme's rules ([decided](lightroom-alignment.md#decided)):

- rendered outputs only, with no Adobe code or data;
- run locally on the owner's photos, keeping figures only;
- consent asked each run.

**Local rig.** `tools/cargo-cached xtask auto-tone-fit --manifest FILE --output NEW_FILE --consent-owner-photos [--rounds 1..6]` reads a manifest with `photos`, each holding a unique `id`, `original`, `lightroom_auto` and `xmp` path relative to the manifest. The optional `lightroom_base` is a separate export without Auto; without it the base gap is unavailable. It requires 40–100 photos and refuses before reading the manifest unless this run carries consent. It keeps at most 100 bounded grids (1,300 MiB at the square maximum) plus the current source/temporary render buffers; its temporary private catalog is removed on exit. Reports retain chosen ids and figures, not images or source paths. XMP must include finite values for all eight sliders.

The fitter sorts ids by SHA-256 and holds out every fourth photo. In fixed coordinate order it tries both directions for all eight targets, halving the steps each round, accepting only strict training improvements. Its objective is the median per-photo weighted absolute picture difference: median ×4, P10/P90 ×2, bright/dark fractions ×1, clipping fractions ×10 and chroma ×4. All clipping statistics in picture comparisons count the exported pixels, including originally clipped ones; the solver itself keeps its source-clipped exclusions. The rig reads its grids without the sensor, so a RAW photo's grid there carries no source-clipped flags (`source_clip_detection` would read `unavailable`). No Lightroom slider conversions are installed yet, so slider values are informational. The report proposes `auto-tone/2` without adopting it. A synthetic export test perturbs median and spread, checks recovery within 0.025 and 0.04 respectively, and requires held-out error to fall by at least half.

**Result.** The owner reviews the fitted `auto-tone/2` on the corpus and their photos. Lightroom's known weaknesses, such as a constant +15 Vibrance or over-lifted shadows, are reported where the fit reproduces them, so the owner can keep a target that differs.

## Acceptance and evidence

1. **Solver.**
   - The solver's values on synthetic samples equal hand-computed expectations, including dark, bright, low-contrast, high-key, clipped-source, saturated and degenerate cases.
   - Every search meets its constraint at the committed value and fails it one step further.
   - The same sample gives the same values across thread counts.
   - A second solve with Basic already at the result changes nothing.
2. **Analysis sample.**
   - The reference sample equals an independent grid read of the reference frame, exactly.
   - The GPU sample matches it within the declared display tolerance.
   - Auto values from the GPU and reference samples agree within 0.02 EV and 2 units on every field for every corpus photo. Any difference is reported.
   - The sample honours crop, straighten, lens and perspective geometry and the RAW and JPEG kinds.
3. **Command parity.**
   - `edit.auto-tone` and `query.auto-tone` agree, and the action's `analysis.auto-tone` is the query's report.
   - Auto creates or updates the one global Basic layer with one `Auto tone` entry, keeps white balance, refuses masks, history and drafts, deduplicates retries and is undone in one step.
   - The source checksum is unchanged.
   - An independent JSON client gets the same result as the desktop.
4. **Presets.**
   - The preset composite runs Auto after the other steps, refuses the overlapping set at store, skips a degenerate photo and analyses each photo of a batch separately.
   - The import maps `AutoTone` in XMP and `.lrtemplate` with its report.
5. **Desktop.** A real background rendered journey on the M4 correlates the button, the history row, the slider values, the session state, logs and captures, on generated photos and the supplied RAW fixtures. It covers busy state, the disabled states, Cmd+U, undo, a refusal and the preset create form.
6. **Measurement.** Once delivery is complete, release photo-sized measurements on the M4 record click-to-entry latency for 24 MP and 60 MP, warm and cold, on the GPU and the reference, together with sample bytes and solve time.
   - Provisional targets: p95 at or below 250 ms (24 MP) and 400 ms (60 MP) on the GPU, warm.
   - A miss is reported with its figures and does not block delivery.
7. **Tuning.** It is recorded with held-out figures and the owner's review. It is not a gate for 1 to 6.

### Available native evidence

Final quick and full headless checks pass on M2. All 58 components before the rendered GPU gate pass. The gate passes its 159 measured recipe/source combinations with originals unchanged and no tolerance changes; 118 combinations lack their RAW sources, so the aggregate remains **incomplete**, not a full-corpus pass. The standard timing tier passes functionally, but its four timing components are marked unreliable by host load. The separate Auto engine distributions were taken under low recorded load and are scoped in the [performance spec](../specs/performance.md#auto-tone); their warm latency exceeds the provisional budgets.

On Apple M2 / Metal, generated JPEG and public licensed Nikon Z6 RAW background journeys pass: all eight committed and displayed values match the independent query; the busy button, tooltip explanation, Cmd+U, repeat, undo, history/Compare refusals and per-photo preset form are correlated with captures and logs. The RAW GPU and reference predictions agree exactly on all eight fields. A separate RAW preset test proves that changing white balance and running Auto together equals changing white balance first and then running Auto, with other layers and original bytes preserved.

These journeys predate the tooltip taking the report from the action's answer rather than a second query; core and desktop tests cover that path, and the native journey has not been run again since.

Generated half/float GPU samples with Detail plus crop, straighten, lens and perspective stay within the declared display tolerance and Auto value bounds. Exact CPU grids, cache identities, solver oracles, stale-client refusal, batch per-photo analysis and imports have focused tests. The fitting rig passes synthetic and repository-fixture stand-ins; these are not Lightroom calibration. Owner M4 checks, the full private RAW corpus and native Windows/Linux GPU qualification remain unrun.

## Performance-rules review for implementation

- **Source:** only the verified source cache is read. Auto never reads, hashes or decodes the original itself.
- **Memory:** each grid is at most 13 MiB (RGB and source-clipped flags) at a 1024-point long side. Retained grids total at most 32 MiB in one least-recently-used cache, with eviction before building a replacement. A RAW read holds the prepared source's sensor, which the source cache already retains, only for its call. Positions, gather ordering, one 256-square tile, and the solver's f64 luminance array and RGB chunk are charged as scratch before allocation. Auto adds no retained full-frame buffer. A spatial reference prefix uses the reference renderer's existing whole-frame implementation, then releases it with the read; the GPU gathers from bounded prefix tiles. The separate fitting command retains at most 100 grids (1,300 MiB).
- **Threads:** nothing runs on the owner or UI thread beyond planning metadata. The sample read and the solve both run on the tile service's thread, with cancellation at chunk granularity; a superseded or stale request is cancelled there ([cancellation](#analysis-input)).
- **Timers:** none. No idle timer, polling or per-tick work. Auto runs only on request.
- **Desktop requests:** the button sends one mutation, then the normal `asset.state`/session refresh and preview. The tooltip shows the report the mutation answered, pinned to the entry the refresh read back, so nothing is read or solved again. It adds no history-page read. An explanation for a displaced entry is discarded.
- **Reuse:** a query followed by the action, or a repeated Auto, reuses the grid by asset, verified source/development identity, semantic prefix, output geometry, grid, input mode, clip detection and renderer, for any client. Neither the grid nor the small per-request replay memo holds a source or evaluation. Removing or deleting the photo releases its grids; stale fills cannot repopulate them. Solved reports are not cached across requests.
- **RAW clip flags:** at most 27 sensor sites a grid point, read on the tile service's thread with the grid. An indication on 2026-10-08 on the M4 under heavy shared load (load average 38), the three supplied RAWs' full grids of about 700,000 points: 21 to 24 ms on the X100VI, 33 to 35 ms on the Z6 and 48 to 49 ms on the Air 2S DNG, whose warp maps each channel; not a measurement of the click-to-entry budget.
- **Baseline:** no before/after `editor-performance` improvement is claimed. The dedicated Auto engine measurement isolates its new work; it does not include source preparation, catalog commit or presentation and cannot establish the click-to-entry budget.
- **Evidence:** exact tests cover the CPU sample and the solver, and declared-tolerance comparisons cover the GPU sample. Measurements follow delivery, once.

## Decided

The owner decided on 2026-10-07:

| # | Question | Decided | Not chosen |
| --- | --- | --- | --- |
| T1 | Which sliders | Lightroom's eight: Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Vibrance, Saturation | The six tone sliders and Vibrance; tone only |
| T2 | White balance | Not in Auto tone, as in Lightroom; Auto white balance stays a separate later proposal | A separate Auto white balance in this plan; white balance in the Auto button |
| T3 | Beyond the button and its API | Auto in presets, recomputed per photo, and the Lightroom importer mapping `AutoTone` | Per-slider Auto; a desktop Auto on a selection |
| T4 | Tuning | Fit the targets to Lightroom's Auto on the owner's photos after delivery, then owner review | Corpus review only; no tuning |

**Recorded defaults**, recommended and delegated within this contract:

- deterministic statistics and searches rather than a learned model;
- analysis before Basic, with a forward model through Basic and the Look only;
- the starting targets;
- the grid size and caps;
- the button's place and its shortcut.

Routine code organisation, the reduction used for repeated evaluation and the busy-state presentation belong to the implementer.

## Execution

**Tasks.** Eight tasks:

- **The solver** and **the bounded analysis sample** start independently.
- **The Basic action and query** integrate them.
- **The desktop button** and **presets and import** build on the action.
- **The Lightroom fitting rig** can be prepared from the solver alone, before the owner's export exists.
- **Qualification, measurement and documentation** follow working delivery.
- **The owner's Lightroom round, the fit and the review** come last, with no dependency back into delivery.

**Implementation review:** the plan was checked against upstream `main` at `76af7e1e`. The current module, field-patch and tile-service interfaces support the work. Preset batches must enter the same deferred analysis path as individual actions; analysis identity must use intermediate layer content, not newly allocated layer IDs. The plan uses the roadmap's high-tier model minimum.

## References

- Research sources: [Adobe, December 2017 Lightroom update](https://blog.adobe.com/en/publish/2017/12/12/announcing-december-update-lightroom); [Julieanne Kost, applying Auto tone in Lightroom Classic](https://jkost.com/blog/2022/02/applying-auto-tone-adjustments-in-lightroom-classic.html); [ExifTool XMP-crs tags](https://exiftool.org/TagNames/XMP.html) (`AutoTone`, `AutoToneDigest`); [Adobe community: Auto Whites and Blacks](https://community.adobe.com/questions-675/auto-whites-and-blacks-in-lightroom-what-is-behind-this-adjustment-986381); [RawTherapee `improcfun.cc`](https://github.com/Beep6581/RawTherapee/blob/dev/rtengine/improcfun.cc) and [RawPedia Exposure](https://rawpedia.rawtherapee.com/Exposure); darktable [`exposure.c`](https://github.com/darktable-org/darktable/blob/master/src/iop/exposure.c) and [`filmicrgb.c`](https://github.com/darktable-org/darktable/blob/master/src/iop/filmicrgb.c); [Apple Core Image auto adjustment](https://developer.apple.com/library/ios/documentation/GraphicsImaging/Conceptual/CoreImaging/ci_autoadjustment/ci_autoadjustmentSAVE.html); [MIT-Adobe FiveK](https://data.csail.mit.edu/graphics/fivek/); [Exposure, Hu et al.](https://arxiv.org/abs/1709.09602); [Distort-and-Recover, Park et al.](https://arxiv.org/abs/1804.04450). Adobe's help pages refused automated fetches. Their content is cited through Adobe's own blog and secondary sources, and Lightroom's algorithm is undocumented.
- Luxforge: [Basic and histogram](basic-and-histogram.md), [Basic tone](basic-tone.md), [white balance and the neutral picker](basic-white-balance.md), [Basic colour](basic-colour.md), [presets](presets.md), [RAW looks](raw-looks.md), [Lightroom alignment](lightroom-alignment.md), [GPU-first](gpu-first.md), [performance rules](../engineering/performance-rules.md).
