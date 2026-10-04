# CPU and memory efficiency

Status: in progress. The owner accepted the scope and the decisions below on 2026-10-03. Done: the 16-bit quantizer, most of Detail's kernel work, the launch scan, the histogram reducer, API answers, the lineage query, live-session writes, the WAL catalog, strokes shared on entry reads, the painting copies, collapsed sections and job reads. The rest is in progress or not started, and the `dist` build profile is deferred. The task plan is [efficiency](../../tasks/efficiency.json).

## Outcome

Spend less CPU time and hold less memory for the same pictures, the same numbers and the same or better latency. Every change in this plan leaves every output byte, histogram count, sample and digest identical, except the catalog's journal mode, which changes how a commit reaches the disk and not what it stores.

The work comes from a read-only audit of the whole workspace on 2026-10-03. The audit found the idle path, the desktop's texture and upload handling, the subscriptions and the GPU preview's steady state already tight. The remaining cost is concentrated in four places: per-pixel kernels (spatial tiles, the Detail and Presence filters, the RAW input reads), source preparation (hashing, zero-fills, reads), per-tick copying on the owner and the desktop while painting, and build configuration. Every figure below is an estimate from reading the code. None was measured, and none may be claimed until the measurement task records it under [rule 13](../engineering/performance-rules.md#rules).

## Constraints

- **Byte identity.** A kernel change keeps the same arithmetic in the same order per value, or proves equality another way (an exhaustive test over a finite domain, such as every `f32` threshold). Existing exactness tests stay unchanged and must pass. Rust does not contract `a * b + c` into a fused multiply-add, so reordering loops or vectorizing over independent values is safe; reassociating a sum is not.
- **Performance rules.** Each task answers the [review checklist](../engineering/performance-rules.md#review-checklist) for its own change. No new full-frame allocation without a named limit, no frame work on the owner or the interface thread, and no new idle wake-up.
- **Vendored sources stay pristine.** Native changes are checked-in patches that `crates/luxforge-raw/build.rs` applies to a copy in `OUT_DIR` with exact context matching, as `patches/librtprocess-local.patch` already is. No file under `vendor/` is edited.
- **Dependencies stay pinned and noticed.** A new crate (such as `sha2-asm`) is pinned in `Cargo.lock`, passes `cargo deny`, and gets its notice where [dependencies](../engineering/dependencies.md) says notices live. The task never claims a licence review is complete.
- **Portability.** Windows and Linux keep building and behave the same. An Apple-specific speed-up is selected at run time or by target, never assumed.
- **Measure once, at the end.** Tasks run only their own unit tests while building. A single measurement task takes before/after figures on a quiet host once the feature work is merged, with the base and the branch built back to back and run in alternating order.

## Owner decisions, 2026-10-03

- All findings in the two byte-identical tiers below are accepted.
- **The RAW colour layers before a spatial layer stay recomputed per tile** (about 2.7× the stage under Clarity). No materialized frame or row-band cache is added for now. It is recorded as a known remaining cost in the [performance rules](../engineering/performance-rules.md#known-remaining-costs).
- **Clarity and Dehaze get a reduced-grid cache** under rule 14, with the contract in [the reduced-grid cache](#the-reduced-grid-cache).
- **The float mosaic is removed from the RAW development peak** through a reproducible, checked-in librtprocess patch, never by editing vendored code.
- **The catalog moves to WAL with full flushes**, as recommended in [catalog durability](#catalog-durability).
- **Packaging and timing use a separate optimized profile**, as recommended in [build profile](#build-profile). The daily release build is unchanged. The same day the owner deferred it until the timing runs other plans have outstanding are recorded, so this plan measures in `release`.
- Not adopted, all of which change bytes or break something: DCT-scaled JPEG decode for proxies, parallel restart-marker JPEG export, cropping masked sensor margins, `target-cpu=apple-m4`, and `panic = "abort"`.

## Work

### Build and dependencies

- **Hardware SHA-256.** `sha2 = "=0.10.9"` compiles its aarch64 SHA-2 backend only with the `asm` feature (`sha2-0.10.9/src/sha256.rs`), which the workspace does not enable. Every original is therefore hashed in software on the M4 before its decode, as are artifacts and capability payloads. That costs an estimated 50–230 ms on a cold RAW open and 20–30 ms on a JPEG.
  - The aarch64 backend checks for the CPU feature at run time and falls back to software.
  - The `asm` feature also pulls in the `sha2-asm` crate, compiled with `cc`; on x86 it replaces only the software fallback, since SHA-NI is already used there.
  - Either enable `asm` (a new pinned crate and notice), or move to a pinned `sha2` release whose aarch64 backend needs no feature, if one exists and its API change is contained.
  - Digests are identical, so this is not a format change.
- **JPEG XL DNG decode on the shared pool.** `jxl-oxide` is built with `default-features = false`, which drops its `rayon` feature, so a JPEG XL DNG (Pixel, Galaxy) decodes serially on the source worker and cannot be cancelled during `render_frame`. With `rayon` enabled its default pool is the global one, so no private pool is added (rule 9). The decode is deterministic.
- **Half floats on x86.** `half` is built without `std`, so x86_64 cannot detect F16C at run time and converts each boundary texel in software. Enable `std`. aarch64 already uses the hardware conversion, so the M4 is unchanged.
- **The system scan at launch.** `app/mod.rs:558` calls `iced::system::information()`. Iced's `sysinfo` path runs `System::new_all()` and `refresh_all()`, walking every process on the host, to supply two strings: `state.backend`'s graphics backend and adapter. Supply them without a process scan: from the renderer's adapter information if a hook is cheap, or by deferring the call until something reads them. `state.backend` keeps reporting the same strings to every client, with no extra wait for an API reader. Done: iced offers no adapter hook and wgpu's device none either, and only an evidence run reads the two strings (its frames, its capture gate and its `backend` event), so only an evidence launch asks for them.

### Build profile

Deferred by the owner on 2026-10-03: the other plans' outstanding timing runs and every baseline the [performance spec](../specs/performance.md) records build `release`, and a `dist` figure is never compared with a `release` one. Its task stays blocked until the owner reopens it.

Add `[profile.dist]`, inheriting from `release` with `codegen-units = 1` and LTO. Thin and fat LTO are measured against each other and the faster is kept, unless fat LTO's build time is unreasonable for the timing tier. `cargo xtask package` and the timing commands (`editor-performance`, `editor-latency`, `measure`, the `timing` tier) build with `dist`. `develop`, `smoke`, `quick` and `rendered` keep `release`, so the edit loop does not slow down. `panic` stays `unwind`, because `mozjpeg` unwinds libjpeg errors into Rust. Timing reports already record their profile, and a dist figure is never compared with a release baseline.

### Source preparation

- **Read the original with its size.** `source.rs:112` reads through `File::take` into `Vec::new()`. `Take` hides the file's size hint, so the buffer grows by doubling and can hold up to twice the file, for example 64 MiB for a 40 MB NEF, through the whole unpack. Reserve the file's length, bounded by the same limit, before reading.
- **Leave zeroing to the writers.** `luxforge-raw` zero-fills large buffers with `Vec::resize` on one thread before a pooled pass writes every value. The places are `develop.rs:143` (the RGB planes, 12 bytes per site), `develop.rs:257` (direct development), `lib.rs:810` (the u16 mosaic) and `dng.rs:1314` (the Air 2S warp scratch). Allocate these zeroed through one fallible helper (`alloc_zeroed` behind a safe wrapper, or `try_reserve` plus `vec![0; n]`), so the kernel provides zero pages and the pooled writers fault them in, as rule 2 already asks. The estimate is 13–38 ms per development and per committed white-balance change, depending on size.
- **No rescan of a proxy for non-finite values.** `proxy.rs:804` builds every RAW proxy through `LinearImage::with_fingerprint`, which scans every value on one thread (`source/linear.rs:167`). A weighted mean of finite planes is finite, so build it through the validated constructor, with a debug assertion and a test.

### The 16-bit quantizer

Done. `colour::srgb::Quantizer16` replaced the search over 65,535 `f32` thresholds with a 128 KiB index of the square root of `[0, 1]` in 65,536 bins, no bin holding more than two thresholds, and row passes take the tables once. `slow_byte_quantize16_is_the_threshold_search_at_every_f32_in_the_unit_interval` proves it gives the search's code for every `f32` in `[0, 1]` and the special values. Its figures are in the [performance spec](../specs/performance.md).

### Spatial tiles

- **Reuse the tile planes.** `run_tile` (`render/spatial.rs:327`, allocating at `:368`, `:377` and `:406`) allocates a fresh, zeroed `vec!` for the tile's input and for each unit's output, about 34–125 MB per tile under Presence. At 60 MP that is several GB of new zero pages per render. Only the unit scratch (`TileScratch`) is reused today. Give each batch slot two ping-pong plane buffers, one for the input and even units and one for odd units, grown to the largest request and never cleared. `fill` and `PlanesMut` already write every value, the same contract `TileScratch` relies on. A point query keeps owned buffers. The tile's working set charged to the spatial budget does not grow.
- **No barrier between batches.** `run_batches` (`render/spatial.rs:706`) waits for a whole batch before reserving the next. On the M4 Pro's 10 performance and 4 efficiency cores, a serial tile on an efficiency core holds the batch back. Replace the barrier with a rolling window under the same reservation: start a tile when one finishes, and write tiles back in order. The cancellation check stays between tiles, and the budget bounds what runs at once exactly as now.

### Detail and Presence kernels

- **Detail's limiter and reconstruction.** Smoothing, noise reduction's shrinkage (one chroma factor for a and b) and its level planes already run over row slices, with the halo checked once a pass and frozen references in `modules/detail/exactness.rs`. What still reads through the clamped, release-asserted `Geometry::index` (`modules/detail/filters.rs:128`) per pixel is `Sharpen::limited` (`sharpen.rs:57`, 14 indexed reads a pixel: the pixel, its blur, the guide's four neighbours and the 3×3 extrema) and the reconstruction loops that end Denoise and Sharpen. Add an interior path over row slices for pixels whose 3×3 reach is inside the held region, keeping the clamped, asserted path for the band near the stage edge, and freeze today's limiter as the reference first.
- **Presence's box and guided passes.** `horizontal_mean`, `vertical_strip`, the combine and `upsample` in `modules/presence/filters.rs`, and `Planes::sample` in `modules/spatial.rs`, read every tap through clamps and bounds checks. The horizontal running sum is one dependent `f64` add chain. Read interior rows as slices, interleave two to four rows' independent sums, and precompute `upsample`'s per-column index and weight once per call. Each output value's own sequence of operations is unchanged.

### The reduced-grid cache

Clarity's reduced source (`modules/presence/clarity.rs:142`) and Dehaze's reduced grid (`modules/presence/dehaze.rs:262`) are anchored at the stage origin and do not depend on the tile. Yet every tile re-encodes and re-sums them over its own halo, which is about 2.7× the stage at 60 MP for Clarity alone. They also do not depend on the unit's amount or radius, so an amount drag recomputes them for nothing. Under rule 14:

- **What.** The f32 reduced plane a unit builds from its input stage before any coefficient is applied: Clarity's encoded-luminance 4× block grid, and Dehaze's reduced grid. A unit declares that it has one, as it declares an estimate key.
- **Key.** Everything the plane depends on, mirroring `EstimateKey`: the source fingerprint and development identity, the prefix hash of the layers before the operation, the stage dimensions and the sampling of the phase (exact, proxy or window), and the unit's own reduction key. Neither the unit's position nor its amount is part of it. Each part of the key has a test proving that a change misses.
- **Budget.** 64 MiB in total, named in the [limits](architecture.md#limits) and held in the `RenderContext` beside the estimate store. The largest single entry, Dehaze at 60 MP, is about 45 MB. The store evicts the least recently used entry before adding one that would pass the budget, and refuses to hold an entry larger than the budget.
- **Fill.** Built on the shared pool by the render that first needs it, as a pass of its own before that operation's tiles, never on the owner or the interface thread. Its cancellation is the render's.
- **Use.** A tile reads the sub-rectangle it needs, and the guided filter stays per tile. When the plane covers the operation's reach, the tile's own fill shrinks to what the other units' halos still need. A point sample reads the cached plane when present and otherwise computes its window as today; both give the same byte.
- **Disposable.** Losing an entry costs only time. It is never part of history, an artifact or a source.
- **Adoption.** Its hit rate across an amount drag and across settle, its rebuild cost and its retained bytes are measured on 24 MP and 60 MP inputs before it is kept. Each masked layer reads its own input and so has its own entry, and at 60 MP the budget holds about one Dehaze entry; with up to 16 masked spatial layers ([GPU shared scratch](gpu-shared-scratch.md#sixteen-masked-spatial-layers)), the measurement includes a stack of several masked Clarity and Dehaze layers.

### RAW rendering reads

- **Row reads instead of pixel reads.** `load_pulled` (`render/linear.rs:639`) and `region_in` (`render/pipeline.rs:715`) read each pixel through the geometry's unmap, the view's orientation `match`, the entry and white-balance adjustment, as `f64` through a `Vec<[f64; 3]>`, then narrow back to `f32` with a per-pixel `finish` check. For a signed-permutation geometry with no entry, compose the geometry and the view into a first index and stride per row and copy spans. For a spatial frame entry, copy the three plane rows. Keep `f32` end to end where the value is already `f32`. Hoist the white-balance test out of the loop. The per-pixel white-balance overflow error stays reported exactly as now.
- **Portrait planes read in blocks.** `ViewReader::row` (`source/linear.rs:408`) walks orientations 5 to 8 down a column, touching a new row of all three planes for every output pixel. Load each chunk as a blocked transpose: walk source rows in order and write the chunk's output rows from each contiguous span.
- **Resolve without replacements.** `Segment::resolve` (`render/compiled.rs:631`) composes every operation's geometry per pixel even when the segment has no pixel replacements. Return straight after the unmap when it has none.
- **Quantize `f32` without the guard.** The terminal path (`terminal_srgb`, `render/linear.rs:157`) widens an `f32` to `f64` and uses `Quantizer::rounded`'s ±1e-12 guard. For an `f32` input the guard band can contain only a threshold and its `next_down`, which `the_f32_thresholds_are_where_both_quantizers_change_code` already pins. Use the direct quantizer for `f32` rows.

### The RAW float mosaic

During a development the u16 mosaic (2 bytes per site), the normalized `f32` mosaic (4) and the RGB planes (12) coexist: 18 bytes per site, about 1.08 GB at 60 MP, and 30 per site while the second development is held. The demosaics read the float mosaic only in RCD's tile fill (`LIM01(rawData / 65536)`), Markesteijn's equivalent read and the border pass.

Add a checked-in patch, `crates/luxforge-raw/patches/librtprocess-mosaic.patch`, applied by `build.rs` after `librtprocess-local.patch` with the same exact-context rule. With it, `rcd.cc`, `markesteijn.cc` and `border.cc` read each site through one inline function that computes `((s - black) * scale) * gain` from the u16 mosaic and the per-site black, scale and gain tables, in the same `f32` operations and order as `Normalization::run`. The Rust side passes the tables in place of the float mosaic. A development whose sensor stage rewrites the float mosaic keeps the current path: DNG sensor-stage corrections (`apply_sensor`, as on Pixel 7/8 Pro, Leica and Ricoh GR II) and sparse sensor repairs. The saving is 4 bytes per site at peak (240 MB at 60 MP) and one full pass. Updating the vendored source then needs both patches reviewed again, which the exact-context check enforces.

### Analysis and the owner

- **Histogram reducer.** `Bins::add_pixel` (`analysis.rs:202`) counts each channel's 0 and 255 into their own counters, which equal `r[0]`, `r[255]` and so on, and then recomputes the same comparisons in `clip_class`. Derive the per-channel extremes from the bins in `into_report`. Classify with a 256-entry table (`v == 0` as bit 0, `v == 255` as bit 1), combining the three channels with `|` for any-channel and `&` for all-channel counts into a four-way class count. Use `u32` counts per bounded chunk, widened on merge. This runs between the exact render and the settled frame, an estimated 5–18 ms per settle.
- **API answers not copied twice.** `ApiResponse::success` (`api/mod.rs:57`) calls `serde_json::to_value` on its result, which is usually already a `Value`, so every answer is rebuilt on the owner thread. Add a constructor that moves a `Value` in, and use it wherever the answer is one.
- **Lineage prepared once.** `history.lineage` (`editor/history.rs:394`) runs `query_row`, preparing the statement anew for each of up to 100 steps after every undo, redo and restore. Prepare it once (`prepare_cached`), or answer with one recursive CTE.
- **Buffered live-session writes.** The TCP session writer (`api/transport.rs:138`, `:200`) serializes straight into the socket, one `write` per JSON fragment, with Nagle on. Wrap it in a `BufWriter` flushed at each line, and set `TCP_NODELAY`.
- **Strokes shared on entry reads.** `entry_from` → `hydrate_strokes` → `read_strokes` (`editor/catalog.rs:378`, `:515`) reads, parses and re-hashes every stroke a new entry references, even though the entry cache holds the same immutable, content-addressed strokes for the asset's other entries. That is O(strokes) per commit and O(n²) over a painting session. Resolve stroke IDs from cached entries of the same asset first, and query only the rest. Those strokes came from the catalog and were verified there, so the cache's rule that it holds only what the catalog returned still holds.

### Painting

A brush tick is O(points), and so O(n²) over a stroke, on both sides of the owner call:

- **Desktop.** Each path-growing move clears the stroke's memoized decimation (`mask_draft/brush.rs:192`), re-runs Ramer–Douglas–Peucker over the captured path through `capture_error` (`app/masks.rs:1673`), clones the points and builds a `Value` per point (`brush.rs:315`). It then overwrites `pending` even when a round trip is in flight (`app/draft.rs:196`), so the work is thrown away, deep-clones the fields into `sent` (`draft.rs:296`) and clones the set again into the session (`app/gesture.rs:745`). Build the fields only when they will be sent, by marking the draft dirty while a round trip is in flight. Hold the decimated points as `Arc<[[f64; 2]]>` and cache their `Value`. Keep a revision in place of the `sent` clone.
- **Owner.** Every call deep-clones the session, draft included (`api/owner.rs:1483`). `draft.set` clones the draft (`api/methods.rs:2074`) and builds `next.request()` only to test a key (`:2083`). Planning builds `draft.request()` again (`editor/plan.rs:608`). The answer repeats the draft. Test keys on the fields directly, snapshot the session only for handlers that can park a pixel read (or share the draft behind an `Arc`), and avoid the second `request()`.
- **Not in scope.** The wire protocol stays as it is. Append-only points would be an API change for the owner to decide.
- **Done, and what remains.** The desktop builds a stroke's fields once per send, from the decimation cached as `Arc<[[f64; 2]]>`, which a position snapping into the cell before it keeps. `draft.set` answers in the update that sends it, so every path-growing move is still one send with one whole-path decimation and one copy into `sent`. The owner takes `draft.set`'s fields out of its request, merges without copying the fields it replaces and keeps the draft out of its rollback copy, so an unplanned tick copies the path once, into its answer. A tick planned over a spatial stack still copies it twice more: the request `Prepared` reads (`Draft::request`) and the values the mask planner splits; removing them needs the planner to read parameters by reference.

The frozen coverage, draft identities, evidence semantics (`frame_pending`, `drained`) and one-entry-per-stroke history do not change.

### The desktop

- **GPU preview blocks.** Each prepared frame during a GPU gesture concatenates every program's block into one `Vec<u32>` (`pack`, `luxforge-ui/src/photo_surface/gpu_preview.rs`), then the slot and each link of a spatial chain (`gpu_preview/chain.rs`, `write`) compare the whole of it with their last write through `mask::changed_ranges` and copy it again on any difference: up to about 6 MiB per frame with a lens warp grid or a heavy brush mask. Separately, the lens warp grid is converted and collected into a new `Arc<[u32]>` on every tick (`luxforge-app/src/app/gpu_plan.rs`, `grid.nodes`) although it is fixed for the draft. Convert the grid once when the boundary is held. In the surface, remember each step's block `Arc` and offset, skip blocks whose pointer is unchanged and compare only the rest. The [shared scratch pool](gpu-shared-scratch.md) restructures the same chains; whichever lands second adapts to the other.
- **Collapsed sections build no controls.** `state/tools.rs:814` builds every section's control models on every message, collapsed or not. For presets that clones each preset's settings and strings (`state/presets.rs:451`), about 1 ms per message with a large library. Build controls only for a section that shows them, after checking what reads section models outside the view: evidence summaries, `query_choice.rs` and curve sampling.
- **Job polls without a full update.** While an export or capability job runs, `time::every(100 ms)` produces two full updates per tick (`app/export.rs:330`, `app/capabilities.rs:429`), each re-deriving the workspace and rebuilding the view even when the read changed nothing. Extend the idle fast path in `update_inner` to unchanged poll reads, or replace the timer with an owner wake on job change like the event waker.
- **Done, and what remains.** A collapsed or unavailable section builds no control models; the evidence report and the readers outside the view that need them build them on demand from the same inputs. A live job is read inside its subscription's stream (`app/job_reads.rs`), which sends the desktop a message only when the job's record changes or the job ends, because iced rebuilds the view after every message whatever `update` does; an export reports no progress, so only its end reaches the update loop. Each live job still costs one owner read every 100 ms on the executor; only an owner push on job change would remove it. An expanded Presets section still builds its library on every message. A `module.status` answer applied after a newer read can show an older job record until the job next changes.

## Later candidates, not in this plan

Byte-identical findings of the same audit that were not taken into scope. Each needs the owner's agreement before it joins the plan:

- hashing the original concurrently with its decode;
- a per-orientation upright copy for JPEGs with EXIF orientation 2 to 8;
- fusing RAW's output scale with the camera matrix;
- per-row view resolution in the RAW proxy;
- a pooled Air 2S warp copy-back;
- once-per-stage GainMap taps for sensor-stage maps;
- Clarity fusing encode with block-sum;
- Texture squaring its encoded plane once;
- a guard band before the luminance-range `powf`;
- Oklab computed once per pixel for colour-limited strokes;
- skipping `mask_feedback_key` for non-progressive coverage;
- a shared `Arc` for the owner's head;
- streaming the recipe identity hash;
- one draft plan per `draft.set`;
- a cached prefix hash per sampled point;
- mask outline subdivision reusing its shared points;
- a lazy history list;
- filtering keyboard events the keymap ignores;
- an outline cache for the crop and compare canvases.

Also outside this plan: x86_64 libjpeg-turbo SIMD (`nasm_simd`, which needs NASM on Windows and Linux hosts), a `mimalloc` A/B measurement, and sizing the GPU preview's output texture to its frame rather than to the photo's square bucket.

## Catalog durability

The catalog opens with `synchronous=FULL` and `locking_mode=EXCLUSIVE`, and a rollback journal by default (`editor.rs:642`). Each commit writes its pages twice and syncs three to four times. Bundled SQLite's syncs on macOS are plain `fsync`, because `fullfsync` is off by default, so a commit is not flushed from the drive's cache. That contradicts the crate's own rule in `atomic_file.rs`, where every durable write is `F_FULLFSYNC`.

In the durable build, open with `PRAGMA journal_mode=WAL`, `synchronous=FULL`, `fullfsync=ON` and `checkpoint_fullfsync=ON`, after `locking_mode=EXCLUSIVE`, so no shared-memory file is created. A commit then appends once and makes one full flush; checkpoints run automatically. Test builds keep `synchronous=OFF` behind `test-skip-disk-flush`.

The consequences, each covered by a test or documented:

- While a catalog is open, `<catalog>-wal` lives beside it, and a crash leaves it there for the next open to recover. Anything that copies, moves or backs up a catalog treats the WAL file as part of it. The catalog-backup question stays open in [decisions](../decisions.md#open-product-questions).
- A clean close checkpoints and removes the WAL file. Quitting the desktop through the macOS application menu (Cmd+Q) does not run its close, as before this change, so the WAL file stays beside the catalog and the next open recovers it.
- The format marker, the refusal of unsupported catalogs and recovery after an interrupted commit behave exactly as now.

## Acceptance

- Every task's own exactness, recovery and bound tests pass. No existing exactness test is loosened or deleted, and new byte-identity claims have tests that would fail on a one-code difference.
- `cargo xtask check` and `cargo xtask verify --tier quick` pass after integration, and `verify --tier rendered` passes after the desktop and rendering changes.
- The measurement task records, on the M4 in `release`, with the base and the branch built back to back and run in alternating order:
  - `editor-performance` on the generated 24 MP and 60 MP JPEGs: Basic with Presence, Detail, and the masked stacks;
  - cold open and committed white balance on the owner's RAW manifest sources, with peak RSS;
  - `editor-latency` slider, paint and settle on 24 MP and 60 MP;
  - the reduced-grid cache's hit rate, rebuild cost and retained bytes;
  - `measure` idle and launch.
- A change that does not pay for itself is reverted rather than kept. Results go to [performance](../specs/performance.md), with known costs updated in the [performance rules](../engineering/performance-rules.md#known-remaining-costs).
