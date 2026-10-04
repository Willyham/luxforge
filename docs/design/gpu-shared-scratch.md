# Shared scratch planes and sixteen masked spatial layers

Status: **planned** ([task plan](../../tasks/gpu-shared-scratch.json)). The owner asked for the plan on 2026-10-03, after the investigation recorded here, under the direction that interactive speed comes before memory rules so that painting keeps pace with tens of masks ([decisions](../decisions.md#gpu-previews)). The same day the owner raised the masked spatial layer cap from four to 16 and asked that a preview drawn on the slower CPU path say why. The cap is 16 ([below](#sixteen-masked-spatial-layers)). The notice's form is a proposal with recorded defaults ([below](#the-cpu-fallback-notice)).

## Outcome

Each GPU-preview slot holds one set of scratch planes, and every link of its chain uses that set in turn. Today each link holds its own set.

- **Cost of a layer.** A second masked Presence layer then costs the planes its applies keep and the intermediate its link writes, not a full set of planes.
- **Pixels.** Every frame stays bit for bit what today's evaluation draws.
- **Passes.** A tick runs the passes it runs today, except where another link wrote the scratch since this link last ran.
- **One figure.** The surface, the qualification report and the desktop's estimate before a boundary exists charge the same figure for a plan.

Three more changes come with it:

- **Texture's band.** It moves into a plane of one channel, which halves the planes every Texture layer keeps. This is independent of the pool.
- **The cap.** A recipe may hold 16 masked Presence or Detail layers instead of four.
- **The notice.** A gesture drawn on the CPU path for a reason that lasts tells the person why it is slower.

## Why

A plan runs as a chain of links ([the materialized chain](gpu-preview.md#where-the-code-lives)). Each spatial link holds its operation's planes, `SpatialSlot` in `crates/luxforge-ui/src/photo_surface/gpu_preview.rs`. A plane is either kept or scratch:

- **Kept.** An apply reads it, so it must hold its values after the link's passes have run. The link's frame pass reads it. So does every later tick that changes only an apply's word, or changes part of the input, since an incremental tick rewrites a kept plane only where its change reaches.
- **Scratch.** Only the link's own passes read it, within the tick that writes it. A later tick reuses scratch only to skip a pass whose output is still current. An incremental tick never does: it runs every pass the applies need.

Measured on 2026-10-03 with the qualifier's slot charge (`Qualifier::charged_bytes`) over masked Presence layers of Texture and Clarity, each through its own radial. The Fit stage is a 24 MP photograph's full-screen 2292 × 1528. The 100% window is 3778 × 2578, the largest window the owner's display holds read through Detail, Texture and Clarity, so the Presence-only figures are an upper bound. JPEG boundaries are `rgba16float`; RAW boundaries are `rgba32float`, as are their chain intermediates.

| Slot charge, MB | Fit, JPEG | Fit, RAW | 100%, JPEG | 100%, RAW |
| --- | ---: | ---: | ---: | ---: |
| One masked layer | 157.6 | 185.6 | 432.4 | 510.3 |
| Four masked layers | 551.6 | 663.6 | 1,528.2 | 1,839.8 |
| Each layer after the first | 131.3 | 159.3 | 365.3 | 443.2 |
| — kept planes (8.25 bytes a pixel) | 28.9 | 28.9 | 80.4 | 80.4 |
| — scratch planes (21.25 bytes a pixel) | 74.4 | 74.4 | 207.0 | 207.0 |
| — the link's intermediate | 28.0 | 56.0 | 77.9 | 155.8 |
| **Layers within the 2 GiB budget** | **16** | **13** | **5** | **4** |
| Four layers, scratch shared | 328.3 | 440.4 | 907.2 | 1,218.9 |
| Each layer after the first, scratch shared | 56.9 | 84.9 | 158.3 | 236.2 |
| **Layers within the budget, scratch shared** | **35** | **24** | **11** | **7** |
| Each layer after the first, shared and Texture's band in one channel (predicted) | 42.9 | 70.9 | 119.3 | 197.2 |
| **Layers within the budget, both (predicted)** | **47** | **28** | **15** | **9** |

The 100% totals were measured while a region's output took the photograph's square size bucket. It now takes the bucket the CPU's region picture reserves, which lowers every total in a 100% column by the same amount and changes no increment or count. The shared rows are today's charges minus the scratch of every layer after the first. The last two rows also subtract 4 bytes a pixel of kept plane from every layer, the band change below. A recipe holds at most 16 masks.

**What sharing costs.** It was measured by forgetting every link's scratch before each tick, which is the most sharing can ever cost.

- A Dehaze drag runs 20 of its link's 22 passes instead of 19.
- Texture, Clarity, Detail and colour drags run what they run today.
- Painted and moved masks run what they run today, since their incremental ticks already run every pass the applies need.

The extra Dehaze pass happens only when another spatial link runs after the dragged one.

**What sharing does not change.**

- **Detail beside Presence.** Once Detail holds noise reduction, all its planes are `rgba16float`, while Presence's are `r32float`, `rg32float` and `rgba32float`. Such a chain shares nothing, so the 100% peak, Detail then all three Presence fields at 1,159.2 MB, stays where it is. Sharpening alone keeps its planes in `r32float` and `rg32float`, which do share with Presence's.
- **Kept planes and intermediates.** Each layer still adds these ([later](#later)).

## Scope

In scope:

1. A scratch pool for each slot, shared by every link's spatial step, at the plane formats each link already uses.
2. One charge for a chain, link intermediates included. The surface's slot, `slot_charge` and the desktop's `region_charge` all use it.
3. Texture's band in a plane of one channel, with frames unchanged bit for bit.
4. Sixteen masked spatial layers, with a compile cache that holds every link sequence the largest plan needs ([below](#sixteen-masked-spatial-layers)).
5. The CPU fallback notice in the status bar ([below](#the-cpu-fallback-notice)).
6. Measurement and documentation of the result, once, at the end.

Out of scope:

- **Sharing across texture formats.** For example, Presence's half-precision-holdable planes taking Detail's `rgba16float` scratch ([alternatives](#alternatives-considered)).
- **Mask-sized planes.** Kept planes or intermediates held over a mask's bounds ([later](#later)).
- **Values and the CPU.** Any change to what a GPU frame holds, and any change to the CPU's renders, samples or export.

## Design

### The pool

- **Ownership.** The slot (`GpuSlot`) owns the pool's textures. Every link's spatial step, the last link's included, reads its scratch planes from them.
- **Classes.** A pool texture's class is the texture format the link's own plane assignment gives it, plus the plane's size: the boundary's `s × s` blocks, or a fixed size. Two plane formats kept in one texture format share a class: `Scalar` and `HalfScalar` in `r32float`, `Pair` and `HalfPair` in `rg32float`.
- **Extents.** Every link of a slot shares the boundary's size and origin, so a class has one extent.
- **Count.** The pool holds, for each class, as many textures as the most scratch textures of that class any one link holds.
- **Mapping.** A link's k-th scratch texture of a class, in the order of its own textures, maps to the pool's k-th texture of that class. The mapping depends on the link's own steps alone.
- **Lifetime.** The pool is fitted before any link's planes, from every link of the plan: the links before the last and `chain.last`.
  - A texture is added when the plan needs more of a class, and charged before it is created.
  - Textures past the plan's need retire through the surface's retirement worker, as planes do, charged until the GPU is done with them.
  - Removing or replacing a texture bumps the pool's generation. A link whose bind groups were built under another generation rebuilds them and forgets what its planes hold. Adding a texture changes no existing binding.
  - A new boundary size or origin replaces the whole pool, as it replaces every link's planes today.

### What may share, and what may not

- **Kept planes never share.** A texture holding a plane any apply of its step reads stays the link's own. After the core's composition (`compose`, `single_writer` in `crates/luxforge-core/src/render/gpu/spatial.rs`), an apply plane has one writer and serves no other unit as scratch.
  - Sharing one would break incremental ticks. Another link's passes would overwrite it beyond the rectangle this link rewrites, where a tick keeps its values.
  - It would also break apply-only drags, which read kept planes without running a pass.
- **A link decides its own formats.** Each pass keeps writing the texture format its own steps give its plane (`texture_formats`, `written_format`), and the pool supplies a texture of exactly that class. So:
  - pass modules, compiled sequences, the warm lists and the pipeline caches are unchanged;
  - two masked layers of one shape still share every pipeline;
  - no value changes, because nothing is ever stored in a format the link alone would not use.
- **One link, distinct textures.** Within one link, distinct scratch textures map to distinct pool textures, since a link's scratch planes are alive together.
- **No sharing within a link.** A link holds at most one spatial step: `chain::chain` splits before each. `spatial::assign`'s sharing between the spatial steps of one link therefore goes, and with it the public `plane_bytes`.

### The schedule across links

`spatial::Schedule` keeps, for each texture, the content key of what it holds, and skips a pass whose output is still current.

- **Records.** For a pool texture, the key lives in the pool as a record of who wrote it and what: `(holder, key)`.
- **Holder.** A holder is a link schedule's generation. It is drawn from a counter in the slot when the link's planes are created, and again at every reset.
- **Reading a key.** A link reads a pool texture's key only when the holder is its own current generation. Otherwise the texture is unknown, and the passes the applies need run as for planes never written.
- **Writing.** Running a pass that writes a pool texture records the link's generation and the pass's key.
- **Units at zero.** An apply whose unit is the identity at the tick's words (`GpuApply::identity`) returns its input before reading a plane, and the schedule runs no pass only such an apply needs until the unit leaves zero. A pass skipped that way records nothing in the pool, and a link whose unit leaves zero after another link wrote those textures runs their passes as for planes never written.
- **After an incremental tick.** `keep_only` keeps only the apply planes' keys, as it does today, and also clears the pool records the link holds, since that tick wrote them only where it ran.
- **Precision.** Links that share no class, such as a Detail link with noise reduction and a Presence link, never forget each other's scratch. A coarser rule, forgetting every pool texture whenever another link has run, would cost those links passes for nothing.

Pass counts change only where a link's scratch was written by another link since it last ran: the Dehaze case above. The single-link figures stay as they are:

- **Presence.** Texture 5, Clarity 0 and Dehaze 19 of 22.
- **Detail then Presence.** Clarity 0, Texture 5, Detail 17 and the colour layer between them 13, of 27.

### Partial and incremental evaluation

Nothing about where a pass runs changes: the pass rectangle, the `valid` rectangle, `needed`, or the incremental tick's rectangles.

- **Why that is correct.** A pass over a rectangle writes only that rectangle. What the apply needs depends on scratch texels only within the dependency cone that `GpuSpatial::reach` bounds, and the tick writes that cone itself. So scratch texels outside it are never read, whoever wrote them.
- **The risk.** Today such texels still hold the same link's earlier, often still correct, values. A cone wider than `reach` says would have passed every test so far and would fail only once the scratch is shared.
- **The poison test.** A switch for tests only writes a sentinel (NaN bits) into every pool texture and clears its records before each link's passes. Every bit-exact test of partial and incremental evaluation must pass with it on.

### Binding and charging

- **Binding.** A link's bind groups (`spatial::Groups`) take each plane's view from the link's own textures or from the pool. Pool textures are created with the usages planes have today.
- **One charge.** A pure function in the widget crate computes a chain's charge from its steps, the boundary's size, origin and format, with no device:
  - each link's intermediate before the last: 8 bytes a texel on a JPEG, 16 on a RAW;
  - each link's kept planes and its passes' parameter slices;
  - the pool.

  Three figures are built on it:
  - `slot_charge`, which adds the boundary, the output in its size bucket, the uniform and the buffers;
  - the live slot's `bytes`;
  - the desktop's `region_charge` (`crates/luxforge-app/src/app/gpu_preview.rs`), over the plan's steps converted with no boundary, which adds what `slot_charge` adds but the buffers.
- **Evidence.** `state.surface.gpu` reports the pool's bytes beside `gpu_preview_in_use_bytes`. The charge is refused before anything is created, and a refusal names `budget-exceeded`, as today.

### The qualification harness

The qualification session (`Session` in `crates/luxforge-ui/src/photo_surface/gpu_preview/qualification.rs`) runs its own chain loop over `SpatialSlot::tick`.

- **Layout.** It builds the pool and each link's planes through the slot's own constructor, so the two never lay out differently.
- **Comparison.** `same_planes` compares the pool and every link's planes.
- **Pass counts.** A sequence of ticks reports passes tick by tick, so tests can pin each tick's count.

### Texture's band in one channel

Texture's last pass writes its band, which its apply reads as one channel (`lf_presence_texture` reads `.x`), into the coefficients' `HalfPair` plane (`modules/presence/gpu.rs`). The composition then gives the band a plane of its own in that plane's format, `rg32float`: 8 bytes a pixel kept for one channel.

- **The change.** Declare the band as a `HalfScalar` plane of its own, written by the coarse smoother's last pass, so it is kept in `r32float`. The coefficients' plane becomes scratch.
- **Planes.** Texture holds 24 bytes a pixel instead of 28, and keeps 4 instead of 8. All three Presence units hold about 28.6 instead of 32.6.
- **Values.** The band is an `f32` in both layouts, so frames are bit for bit unchanged. A test draws both layouts and compares them.

### Sixteen masked spatial layers

The owner's decision of 2026-10-03.

- **The limit.** `MAX_MASKED_SPATIAL_LAYERS` (`crates/luxforge-core/src/modules/spatial.rs`) becomes 16, the figure for masks and for masked colour layers. Masked Presence and Detail layers still count against it together, and a seventeenth is refused with `resource-limit` naming the limit, in `registry/compile.rs`.
- **The settled render and export.** Every spatial layer is still a sequential full frame there. A whole-frame masked layer costs 809 ms at 60 MP and 204 ms at 24 MP, and a small mask about half ([the masked spatial primitive](../specs/performance.md#the-masked-spatial-primitive-one-to-four-layers)). Sixteen whole-frame masked layers are therefore about 13 s of exact render at 60 MP. A render expected to take more than a second already shows its progress on the photograph.
- **The compile cache.** The surface compiles one program sequence per link, and links of one shape share it. A link's shape is its mask's component programs, its units and the colour steps after it.
  - Sixteen masked layers of mixed mask kinds, unit sets and colour steps can need more than 16 sequences, warm lists included.
  - Each tick asks for every link's sequence. One of the plan's own would then be evicted to compile another, and the drag would never draw on the GPU.
  - The cache holds every sequence the largest plan needs, plus its warm list: 64 (`PIPELINE_CACHE`). Compiled pipelines are small, and pass modules are shared through the pass cache.
  - The warm list (`crates/luxforge-core/src/render/gpu/preview.rs`) warms one drag for each distinct drafted shape, which covers every masked spatial layer's, within 45 link sequences (`GPU_WARM_LINKS`) that leave the largest plan room in the cache ([GPU previews](gpu-preview.md#where-the-code-lives)).
- **The GPU budget.** With the pool and the band, 16 masked Presence layers fit the 2 GiB budget at Fit on a JPEG or a RAW. At 100%, about 15 fit on a JPEG and 9 on a RAW ([why](#why)). Past the budget the gesture takes the CPU path, and the notice below says so.
- **The harness.** `editor-latency`'s `--mask-presence` reads the cap from the core, and `--masks` reaches 16, the masks a recipe holds.

### The CPU fallback notice

The owner asked on 2026-10-03 that a person can tell why a preview is slower. Today the status bar names the frame on screen, `GPU preview · N ms` or `Approximate render · N ms`, and why a gesture took the CPU path was recorded only in the evidence, as `state.surface.gpu.plan_fallback` (`Editor::gpu_plan_fallback`).

The notice's place, timing and wording are proposals with recorded defaults, each of which the owner can revise: a short muted phrase after the render slot, with a one-sentence tooltip, one phrase for each class of reason, from the first tick of a gesture that takes the CPU path for a reason that lasts until the next GPU frame or the end of that gesture's settle. It is implemented, and [GPU previews](gpu-preview.md#labels-and-overlays-during-motion) holds its behaviour, its [classes and wording](gpu-preview.md#the-fallback-notices-classes), the recorded defaults' [alternatives](gpu-preview.md#proposals-with-recorded-defaults) and its evidence.

## Risks

- **A stale scratch read draws wrong pixels until the CPU settles.**
  - *Guarded by:* holder records, the poison test, ticks alternating between links, each held bit for bit to a fresh evaluation.
- **A binding outlives a pool texture.**
  - *Guarded by:* the pool's generation rebuilds every link's bindings when a texture is removed or replaced.
- **A refusal part-way through fitting leaves the pool short of what the links need.**
  - *Guarded by:* the tick returns the refusal before encoding anything, as a refused link does today. The next frame, after the retirement, fits again.
- **The surface and the qualification harness lay out planes differently.**
  - *Guarded by:* one constructor.
- **The efficiency work changes the same block writes.**
  - *Guarded by:* [the efficiency design](efficiency.md#the-desktop) skips program blocks whose shared buffer is unchanged in the slot's and each link's writes; whichever lands second keeps the other's block handling and each link's own schedule.
- **The desktop's estimate drifts from the slot's charge.**
  - *Guarded by:* one function, and a test holding the two together.
- **Extra work on the interface thread.**
  - *Guarded by:* fitting the pool reads the steps once a tick, in `O(links × planes)`, as fitting the planes already does. The measurement task records the frame-preparation figure.
- **A 16-layer plan needs more compiled sequences than the cache holds, so it never draws on the GPU.**
  - *Guarded by:* a cache sized for the largest plan, and a test that compiles a plan of 16 masked layers of mixed shapes once and then draws every tick on the GPU.
- **The notice flickers or blames the wrong thing.**
  - *Guarded by:* reasons that pass show nothing, and tests hold each class's text against the reason the evidence records.

## Alternatives considered

- **One plane assignment over the whole plan, as the surface shared planes before the chain existed.** It lets a later link's apply plane take an earlier link's scratch. The earlier link's next run would then overwrite it where incremental ticks keep its values. One schedule for the whole slot would also lose each link's own keys.
- **Forgetting all scratch whenever another link has run.** Simpler, but links of disjoint classes would forget each other's scratch for nothing.
- **Sharing across formats.** Presence's half-precision-holdable planes could take Detail's `rgba16float` scratch.
  - With a pool, it saves 12 bytes a pixel once per slot, not per layer.
  - It changes Presence's precision whenever Detail is in the chain, by the cost [plane precision](../specs/performance.md#plane-precision) measures.
  - It makes a link's pass modules depend on the links before it.
  - Not proposed.
- **Aliasing memory between textures.** wgpu 27 has no placed resources or heaps, so a pool of real textures is the only form sharing can take.

## Verification and acceptance

- **Bit for bit.** Every frame equals a fresh evaluation of the same plan:
  - through the real slot, over a chain of at least three spatial links whose ticks alternate between links;
  - through the qualifier, over masked Presence layers dragged in turn and painted;
  - with the poison on, for every partial and incremental test.
- **Kept planes.** No texture an apply reads is written by another link's pass.
- **Pass counts.** The single-link figures stand, and the multi-link Dehaze figure is pinned.
- **Charges.**
  - The pool is charged once, released to zero, refused before creation past the budget, and waits for a retiring texture, as planes do (`a_larger_plan_waits_for_the_planes_it_replaces_then_holds_its_own`).
  - `slot_charge`, the live slot and the desktop's `region_charge` agree, the last within the output's size bucket and the buffers.
- **Sixteen layers.**
  - A recipe holds 16 masked spatial layers, and the next is refused by name.
  - A plan of 16 masked layers of mixed shapes compiles once, then draws every tick on the GPU.
- **The notice.** Each class of reason shows its phrase and tooltip, and passing reasons and the preference off show none. A rendered scenario captures it with the reason the evidence records beside it.
- **Native.** On the M4:
  - the editor's paint rows of [painting over masked spatial layers](../specs/performance.md#painting-over-masked-spatial-layers) keep their latency within noise, before and after measured back to back;
  - painting over 8 and 16 masked Presence layers is measured at Fit and at 100%;
  - the settled render of 16 masked layers is timed; an export runs the same render, then encodes;
  - the GPU-preview peak falls wherever two or more spatial layers are held;
  - the Fit and 100% corpora stay within their limits;
  - the `quick` and `rendered` tiers pass;
  - the [performance-rules checklist](../engineering/performance-rules.md#review-checklist) is answered.

## Decisions

- **Decided by the owner on 2026-10-03** ([decisions](../decisions.md#gpu-previews)):
  - a recipe may hold 16 masked spatial layers;
  - a preview drawn on the CPU says why.
- **Recorded defaults.** The plan runs on these; each is a proposal the owner can revise:
  - the notice's place, timing and wording ([above](#the-cpu-fallback-notice));
  - the pool follows the current plan's need, so it shrinks when the plan needs less, rather than holding its high-water mark for the slot's life;
  - a pool texture's records name their holder;
  - the evidence reports the pool's bytes.

## Later

- **Kept planes and intermediates over a mask's bounds.** With scratch shared and the band in one channel, a masked layer costs about 4.25 bytes a pixel of kept planes and its intermediate, 8 or 16, over the whole boundary.
  - A layer whose output differs from its input only inside its mask could hold both over the mask's bounds and its reach, so a small mask's layer would cost its area.
  - It needs a growth policy for a painted mask and a way for the next link to read the unchanged input outside the bounds.
- **Detail's sharpening change in one channel.** Its apply reads one channel from an `rgba16float` plane, 8 bytes a pixel kept. A one-channel `r32float` plane would keep 4, but at a different precision, so Detail would need qualifying again.
