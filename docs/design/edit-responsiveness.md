# Edit responsiveness

Status: **proposal with recorded defaults; implementation planned** in [the plan](../../tasks/edit-responsiveness.json). The three repairs of 2026-10-04 are in: the status-bar readout is gone, owner tasks run off the update loop and every launch logs. Everything below is what an ordinary edit still pays for little, found by a read-only audit the same day, with a recorded default for each so the plan can run; each default is a proposal until the owner records it in [decisions](../decisions.md#open-product-questions).

## The problem, measured

On 2026-10-04 the owner edited a 16 MP DJI Air 2S DNG at 33%: a lens profile, a quarter turn, a crop, Basic, a tone curve, the mixer, Presence with Dehaze +10, a linear mask carrying Exposure and Presence, a radial mask and a brush mask carrying Basic, then Sharpening 45. Input stalled for 1 to 2 s five times once Presence sat inside a mask, and for 231.7 s after the sharpening edit, until the editor was quit (`spindump` "slow hid response"). A relaunch hung for 125 s and macOS wrote a hang report whose stackshot named both causes:

- The status bar's pointer readout asked `render.sample` on every move. Through Detail (restoration stage), global Presence with Dehaze and masked Presence the exact point path evaluated three nested spatial segments serially on one core and, on an estimate-store miss, walked the whole stage through Detail's tiles ([performance rules](../engineering/performance-rules.md#known-remaining-costs)).
- Every `owner_task` did its work on the update loop: iced 0.14 polls a new task once, synchronously, and a future that never awaits completes there. The readout's wait was the window's. So was every wait on a source preparation: a RAW white-balance release froze the window for its whole redevelopment, 0.5 to 1.6 s measured ([instant previews](instant-preview.md#a-raw-white-balance-during-a-drag)).

## What is already done

- The readout is removed end to end. Nothing is read under the pointer; `render.sample` stays the API's exact point query, and the `histogram` and `raw-panel` scenarios read their pixels through it as recorded `api` steps ([decisions](../decisions.md#basic-adjustments-and-histogram)).
- `owner_task` and `owner_work` run on the runtime's blocking pool, at one hop per answer ([rule 12](../engineering/performance-rules.md#rules)). The gesture's `draft.set` keeps its synchronous path.
- A default launch writes a bounded `events.jsonl` to the platform log directory, keeping the launch before it ([development](../engineering/development.md#running-the-application)).

## Scope: the remaining costs

Ranked by cost against what the person gets. Figures marked *measured* are in [performance](../specs/performance.md); the rest are estimates from measured parts and are unmeasured until the plan's last task.

| Cost | Trigger | Size | What it buys | Default |
| --- | --- | --- | --- | --- |
| The estimate store holds 8 entries, oldest out, with no refresh on a hit (`render/context.rs`, `EstimateStore`) | Dehaze plus masked Dehaze layers: four estimating units, two stages per job, so one job fills it and the next evicts the committed stack's exact lights | Cold reductions in point queries (*measured* 108 to 421 ms each, one segment) and GPU drags falling back at 100% (`region-estimate`) and at a windowed Fit (`window-estimate`) | 32 KiB saved | Least-recently-used, 256 entries, at most 1 MiB |
| A 120 ms pause during a drag starts a whole-frame exact render, reduction and settled display that a GPU tick never supersedes (`app/preview.rs` `quiet_refine`, `preview/worker.rs`) | Any pause in a slider drag or stroke, at Fit or 100% | About 1 s at 24 MP and 3 s at 60 MP with Detail, Presence and three masked layers, on the pool, while the drag goes on (parts *measured*; the analogue of a held pool put a 24 MP GPU drag at p95 46 against 9 ms) | A histogram and a settled display for a value already left | The next GPU tick supersedes a settle started for an older draft revision and re-arms the quiet timer; release settlement is unchanged |
| The exact point path through nested spatial segments: a per-pixel lock and scan of the held tiles, per-pixel pulls of the colour runs, every evaluation serial, and a whole-stage walk on an estimate miss (`render/spatial.rs` `PointTiles`, `build_reduction_by_tiles`; `render/pipeline.rs` `fill_pulled`) | `render.sample` from any client, `mask.sample-input` and a colour-limited brush seed (three to four segments behind Detail and Presence), the neutral picker (Detail only) | *Measured* 27 to 36 ms through Detail alone and 140 to 227 ms through one Presence segment behind Basic; three segments unmeasured; 125 to 231 s observed with a miss on 16 MP | An exact pick or seed, and UI/API parity | Copy each held earlier tile into the later tile's planes once; a point query's reduction and tile fills run on the pool, bounded by its cancel; the byte stays the rendered byte. Alternative recorded: refuse a cold miss with `preparation-required` naming the exact render |
| No GPU preview at a percentage below 100% (`not-fit`; [GPU previews](gpu-preview.md#at-100-and-above)) | Every drag and brush position at 33% or 50% | The CPU proxy at the displayed size of the whole stage, up to 8 MP, per tick: roughly 80 to 300 ms at 33% of 60 MP (estimate) | Nothing: Fit draws the same proxy on the GPU | Plan the GPU preview at the displayed-size proxy exactly as Fit does |
| `EstimateAfterSpatial` refuses the windowed path whenever an estimating unit follows a tiled segment (`render/compiled.rs`, `render/window.rs`) | Detail before Dehaze, or masked Dehaze after global Presence | 100% motion renders a whole-output proxy, refinement the whole exact frame, and a crop at Fit the whole proxy stage, growing with the inverse of the crop area; past 256 MiB the GPU boundary is refused and past 128 MiB the restoration prefix is not cached | Exactness of the window, which the stored exact estimate already gives | Accept the cut when the store holds the exact estimate for the prefix, as [Detail](detail.md#expected-costs) called provably exact |
| A colour drag under Dehaze at 100% needs an exact-stage estimate for a new prefix every tick (`render/gpu/preview.rs` `RegionEstimate`) | Basic or a mask drag at 100% or above with Dehaze present | A cold whole-stage reduction per CPU tick: *measured* 110 to 420 ms per unit, 864 ms for the first 60 MP region | An exact atmospheric light during motion | During motion only, the light prepared from the drafted colour run over the held reduction, *measured* at 2.8 to 6.7 ms per tick and within the limits on 44 of 49 cells ([performance](../specs/performance.md#dehaze-behind-detail-at-100)), labelled approximate as every motion frame is; settlement exact. Alternative: hold the starting light |
| Compare exit and a history return re-render the exact frame and reduce it again although the retained After raster and report are the picture that returns (`app/preview.rs` content key, `app/compare_after.rs`, `app/history.rs`) | Every `\` release and every return to the entry just left | One exact render, about 1 to 3 s on a heavy stack | Nothing | Re-adopt the retained frame and report when the content serial of the picture returning equals the one retained; otherwise render. Alternative: a second content slot, which would need a named limit under rule 6 |
| The whole-frame exact render and reduction after every commit at Fit | Every commit | The same second or three as a mid-drag settle | The exact histogram and clipping counts the owner requires, Detail's settled display, a warm estimate store and an instant 100% | Keep. Whether an approximate histogram may stand in while it runs is an open question for the owner, not part of this plan |

Costs the audit found and leaves as they are: brush coverage evaluated per pixel (*measured*, within the bit-identity contract), the value-mask input grid and Masks-panel thumbnails behind Detail (owner-accepted; keyed by the prefix, so not per commit), the Performance section sampler, the GPU warm list and the lens query choice (unmeasured, small per call).

## Constraints

- Settled pixels, reports, samples and exports keep their bytes: every change here is to when and where work runs, or to a motion-only approximation that is labelled and settles exact ([rule 4](../engineering/performance-rules.md#rules), [rule 11](../engineering/performance-rules.md#rules)).
- Caches stay bounded, keyed, evicting and disposable, with their limits listed in [architecture](architecture.md#limits) ([rule 14](../engineering/performance-rules.md#rules)).
- The update loop waits for nothing slow ([rule 12](../engineering/performance-rules.md#rules)); the gesture's draft path stays synchronous.
- Timing runs wait for the finished plan and a quiet host, one at a time ([rule 13](../engineering/performance-rules.md#rules)).

## Acceptance

Each task proves its own change with exact-buffer tests in both pixel domains, correlated rendered checks where the screen changes, and the quick tier. The plan's last task measures, on a quiet M4: a RAW white-balance release and a commit answer against the 2026-09 figures (frozen window against one frame), the 16 MP stack above at 33% (tick, settle, Compare toggle), a cropped Detail and Dehaze stack at Fit, and a three-segment point sample warm and cold through the API, recording each with its load and updating the known-costs rows and the roadmap.

## Proposals with recorded defaults

| Question | Default the plan runs on | Alternative |
| --- | --- | --- |
| Owner tasks on the blocking pool, one frame later per answer (slider release to committed frame p50 about 28 against 20 ms when measured in 2026-09) | Adopted 2026-10-04: the update loop otherwise waits on every source preparation and pixel read | A hop-free path for requests the owner answers in microseconds, with its own measurement |
| A point query whose estimate is cold | Reduce on the pool, bounded by the query's cancel; the answer stays exact | Refuse with `preparation-required` naming the exact render that will store it |
| The atmospheric light during a colour drag at 100% | The measured reduced light, motion only, labelled approximate | Hold the light the drag started with |
| Compare exit and history return | Re-adopt the retained frame and report on a matching content serial | A second content slot with a named limit |
| An approximate histogram while the exact settle runs | Not built; the exact contract stands | A labelled approximate histogram from the proxy, replaced on settle |
