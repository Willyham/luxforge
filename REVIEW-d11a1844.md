# Review of d11a1844: responsive viewport previews

Commit `d11a1844 perf: render responsive viewport previews`, reviewed on 2026-09-27 against
`docs/design/instant-preview.md` ("Viewport rendering at 100%") and the evidence recorded under the
Codex worktree's `artifacts/` folder. Line numbers are at d11a1844. This file is a work list:
delete it in the change that resolves it.

## Summary

- **Pixel parity holds.** Byte and RAW exact regions equal the matching rectangle of
  `Render::frame`, and half-detail regions equal the whole half stage rendered with the exact
  stage's estimates, over about 52,000 randomized regions (see "Checked and sound").
- **The photo surface blanks the photograph** on every slider release at 100%, on Fit refits and
  zoom changes, and during moving pans. A deferred upload can then stay blank until unrelated
  input, and texture retirement stalls the render thread's submit. The recorded evidence cannot
  see any of this.
- **One draft fence is wrong** (bug 4), and the settled mask overlay at 100% and above is coarse
  (bug 5).
- **Evidence.** Most numbers match their artifacts, but the raw-panel gate was moved after a failing
  run and the owner's decision was rewritten without the owner; the functional checks come from an
  uncommitted script; most viewport timings predate later production edits.
- **Owner questions; do not decide these:** the raw-panel acceptance criterion (E1), and the
  documented GPU budget wording if the fix for bug 1 changes it.

All unit suites pass at d11a1844: `luxforge-core` 837, `luxforge-ui` 132, `luxforge-app` 415.

## Bugs

### 1. The photograph goes blank whenever its texture changes size (High)

Confirmed on the owner's M4 (headless Metal) by the probes in Appendix A.

- **Where.**
  - Full image, including Fit: `crates/luxforge-ui/src/photo_surface.rs:1456-1467`.
    `write(Layer::Photo)` retires the old picture, then refuses the new one because
    `retiring_full > 0` is now always true, however few bytes are retiring. The photo slot is left
    empty. Before this commit the texture was replaced and written in the same `prepare`.
  - Region sets at 100% and above: `photo_surface.rs:1514-1544`. `write_region` always writes the
    back set; a back set of another size is retired first, and admission then refuses the new one
    (`region_count >= 2`).
  - Draw: the viewport draw shows only textures of the current content (`photo_surface.rs:1074-1087`,
    `region_key_order` at `1225-1247`). The app has already adopted the new content
    (`crates/luxforge-app/src/app/preview.rs`, `region_ready` and `present`), so the previous picture
    is hidden and nothing is drawn. The Fit draw (`photo_surface.rs:1162-1189`) still draws the
    clipping and mask overlays over the empty canvas.
- **When.**
  - Every slider release at 100%: the committed exact region is about twice the size of the
    half-detail sets, and the committed recipe is new content, so the draft's region is hidden
    while the exact region waits.
  - The second input of the next drag, which needs the set that held the exact region.
  - Moving pans: `scaled_rect` (`crates/luxforge-core/src/render/entry.rs:197`) maps the full
    rectangle to the half stage with floor and ceil at a ratio that is not exactly one half (for
    example 4409/8820), so the half raster flips between 451/452 × 417/418 px. In the 200%
    moving-pan evidence the raster size changed 117 times in 156 regions, and 97 uploads were
    deferred in 3 s.
  - Every Fit proxy of a new size: panel toggle, window resize, rotate, crop, straighten, another
    asset, zoom Fit ↔ 50% ↔ 100%, and the display-scale refit at open.
- **Fix direction.** Never replace a drawable picture with nothing. In order of preference:
  - Keep drawing the previous picture, marked updating, until its replacement is admitted. The
    design already allows this: "Before a new interactive region lands, the previous picture may
    stay visible with an updating label."
  - Stop reallocating on small size changes: allocate region sets in size buckets large enough for
    either quality (two sets sized for a 2880 × 1800 exact region fit the 2 × 32 MiB budget) and
    draw the used sub-rectangle.
  - Admit a new full-image texture by bytes (retiring plus new within budget), not by count. This
    changes the documented bound "one full-image allocation … including allocations submitted to
    the GPU but not yet retired" (`docs/design/instant-preview.md`, performance-rules rule 6); if it
    changes the accepted budget, record it as a proposal for the owner.
- **Done when.** The three correctness probes in Appendix A pass as permanent surface tests (they
  return early without a GPU adapter, so report that as a skip, not a pass), and a background
  native run records no draw without a photograph (see E4).

### 2. A deferred upload can stay blank until unrelated input (High)

From the code of this commit, iced_winit 0.14.1 and wgpu-core 27.0.3.

- **Where.** The retirement wake reaches the app only through `waker::subscription()`, gated on
  `surface_retirement_pending()` (`crates/luxforge-app/src/app/mod.rs:1036-1040`). iced_winit
  recomputes subscriptions only inside `update` (`iced_winit-0.14.1/src/lib.rs:1338`); the
  retirement starts later, in the redraw's `prepare`. If no queue is busy at that update, the wake
  is buffered in the channel and nothing redraws.
- **Repro.** At Fit with the clipping and mask overlays off, zoom to 100% and back to Fit with the
  keyboard, making no edits. The retained proxy is handed back (`zoom_changed`, then
  `present_retained`; no job runs), the full texture is retired, the proxy is deferred, and the
  canvas stays blank until something redraws: a mouse move over the photo or a control, a key, or
  the Performance section's 1 s tick (only while that section is expanded and the state panel is
  shown). The same happens when an exact-only job (no proxy phase) changes the dimensions at Fit,
  such as a rotate on a small photograph.
- **Why evidence missed it.** Evidence mode subscribes to a 250 ms tick and, while a capture is
  pending, to `iced::window::frames()` (`mod.rs:1066-1073`), so a redraw always follows. Captures
  also wait for the new draw (`capture_photo_ready`, `app/evidence.rs:371`). The comment at
  `evidence.rs:295-297` says the previous texture stays visible meanwhile; the slot is empty.
- **Fix direction.** Keep the wake path open while the surface may still owe a draw: gate the
  subscription on "the presenter holds a frame or region whose version the surface has not drawn"
  (compare with `surface_diagnostics()`), or keep the channel subscription while a photograph is
  open, as the owner-event subscription already does (a blocked channel stream costs nothing while
  idle). Fixing bug 1 removes most deferrals but not this race.
- **Done when.** A unit test shows the wake stays subscribed after `present()` hands over a frame of
  new dimensions, until the surface reports that version drawn; a background native run of
  "100%, then Fit, then idle" draws the proxy with no further input.

### 3. Retirement blocks the render thread's submit (Medium)

Confirmed by `review_retirement_never_blocks_a_render_thread_submit` (Appendix A): an empty
`queue.submit` took 57.6 ms while a retirement was in flight against a 63.6 ms GPU workload,
against 50 µs without one. With frame-sized GPU work of 1.5, 3.0 and 4.5 ms the submit stalled
1.1, 2.6 and 4.1 ms.

- **Where.** `photo_surface.rs:1710-1734`: the worker calls
  `device.poll(PollType::Wait { submission_index: None, timeout: None })`. In wgpu-core 27.0.3
  `poll` holds the device's `snatchable_lock` and `fence` read guards for the whole GPU wait
  (`device/global.rs:2091-2093`, `device/resource.rs:754-757`), and `Queue::submit` needs
  `fence.write()` (`device/queue.rs:1160-1163`). `retire()` runs in `prepare`, just before that
  frame's own submit, so the submit waits for the GPU.
- **Consequences.** The UI thread waits for retirement, which the design says never happens, and
  with `timeout: None` a GPU hang hangs the UI. Bugs 1 and 2 put retirements on exactly the frames
  that carry new content.
- **Fix direction.** Drop the picture at once (wgpu keeps a resource alive until the submissions
  that use it complete) and release its byte charge from `Queue::on_submitted_work_done`, which
  runs during the render thread's own submit and poll maintenance, or poll with `PollType::Poll`.
  Never hold a wgpu lock across a GPU wait.
- **Done when.** The stall probe shows no submit wait. Keep it as an ignored diagnostic, not a CI
  timing gate.

### 4. A new draft's regions are dropped because of the previous draft's revision (Medium-Low)

Confirmed by the unit test in Appendix B.

- **Where.** `crates/luxforge-app/src/app/preview.rs:884-890` drops a region when
  `draft_revision < displayed_draft_revision`, without comparing draft IDs. Revisions restart at 0
  for each draft (`Draft::new`), and `displayed_draft_revision` is reset only when a frame is
  presented.
- **Failure.** Draft A reaches revision N at 100% or above. Draft B starts before A's committed or
  reseed frame is shown, for example because B's first job supersedes A's committed settle job.
  B's regions 1 to N−1 are discarded. Quick successive brush strokes at 100% freeze the view for
  as many inputs as the previous stroke had.
- **Fix direction.** Track the displayed draft's stamp (`draft_id` and revision), compare revisions
  only within the same draft, and clear it when a draft is released or cancelled.
- **Done when.** Appendix B's test passes, and a region of the same draft at an older revision is
  still dropped.

### 5. The settled mask overlay at 100% and above is about 3× coarser (Low-Medium)

- **Where.** At 100% and above `overlay_cells()` returns cell counts for the viewport rectangle
  (`crates/luxforge-app/src/app/overlay.rs:351-360`), and every mask overlay request uses them
  (`app/masks.rs:760`). The settle job's whole-frame phase reuses the same counts for a whole-stage
  grid (`crates/luxforge-core/src/preview/worker.rs:568-579`).
- **Failure.** For a 60 MP photograph in a window about 1440 × 900 points on a 2× display, the
  settled coverage after a release is about 7 px per cell, with the viewport's aspect ratio rather
  than the photograph's, against 2 px on the region grid during motion and about 2.4 px before this
  commit. The region branch also divides by the display scale, giving one cell per 2 × 2 physical
  pixels, while `displayed_size` at a percentage works in physical pixels.
- **Fix direction.** Give the whole-stage grid whole-image cell counts (compute both in the app, or
  let the request carry both), and use one density rule in both branches.

### 6. The half-detail clipping overlay is misplaced by up to about 1.5 px (Low)

- **Where.** The grid is derived from the half raster, which covers `scaled_rect`'s floor and ceil
  footprint, but it is placed over `frame.full_rect` (`app/preview.rs:923`,
  `app/overlay.rs:246-251`, `app/presenter.rs:231-250`). The photograph itself is placed by `rect`
  and `stage`.
- **Example.** On a 213 × 159 stage, full x 101–133 becomes half 50–67, which covers full x
  99.5–133.4, so the approximate overlay is shifted and compressed during motion.
- **Fix direction.** Place the overlay over the raster's own footprint (carry `rect` and `stage` as
  `RegionFrame` does), or derive the grid only over `full_rect`.

### 7. Minor

- `plan_proxy_region` reports every half-stage compile error as `SegmentMismatch`
  (`crates/luxforge-core/src/render/entry.rs:408-415`), so the fallback reason can be wrong, for
  example for a straightened crop that lands 1.2 px outside the half stage.
- A cold Dehaze estimate for a windowed Fit proxy now reads the exact render's token
  (`preview/worker.rs:93-97` and `139-145`), which is `superseded` for immediate jobs; the design
  says the proxy phase reads only `abandoned`. Drafts are unaffected (their token is `abandoned`).
  Decide and document.
- The crop-stage texture is outside the 512/32/32 MiB accounting. This predates the commit, but the
  documented bound does not mention it.
- Stale text: the `WindowPlan::of` doc (`render/window.rs:181-185`) still says positional units
  prevent a window; `Segment::positional` (`render.rs:842-844`) says such a segment is never cut,
  and the flag is set (`modules/registry/compile.rs:412`) but never read;
  `crates/luxforge-ui/src/lib.rs:11-13` says the crate starts no thread, but each pipeline now
  spawns a retirement worker.

## Tests that claim more than they check

- `a_half_detail_region_is_the_same_scaled_recipe_at_the_same_pixels` (`render/window.rs:702`):
  one interior rectangle on an even 192 × 144 source, with no mask, spatial layer, odd origin or
  edge. Removing the mask cut from `segment.operations` still passes it.
- `half_detail_dehaze_overlap_is_identical_across_pans` (`window.rs:1205`): its "exact globals"
  comparison uses `diagnostic_proxy_frame` (`window.rs:554`), which runs the same pipeline, so it
  is not an independent reference.
- `rotated_resample_entry_keeps_only_the_viewport_on_both_domains` (`window.rs:836`): the RAW
  `resample_peak_bytes() == 0` assertion cannot fail, because only the byte rasterizer records it
  (`render.rs:1704`).
- `exact_viewport_matches_whole_buffer_on_both_domains_and_complex_stacks` (`window.rs:593`) and
  `a_spatial_viewport_crossing_the_tile_grid_keeps_whole_stage_pixels` (`window.rs:1069`) render
  the region first in the same context, so the reference frame reuses the region's stored estimate.
- `interactive_viewport_delivers_one_bounded_region_without_a_report` (`preview/tests.rs:475`)
  would also pass for a full-detail fallback; it never checks half scale or `reduced_detail`.
  `settled_viewport_carries_region_then_whole_stage_mask_coverage` (`preview/tests.rs:529`) only
  checks that the two grids differ.
- `a_capture_waits_for_the_adopted_photo_texture_and_checks_region_identity`
  (`app/evidence.rs:2893`) was fixed after the failed full run by rewriting its fixture; it now
  covers only the any-slot match and checks content and generation mismatches, not version or
  quality. `capture_accepts_a_drawn_region_beneath_older_exact_detail` (`evidence.rs:337`) builds a
  drawn state with two content IDs, which the surface cannot produce.
- Nothing drives `PhotoPipeline::write`, `write_region`, `prepare`, `draw` or retirement; the new
  surface tests cover pure helpers only.

## Evidence and documentation

Almost every figure in the docs matches an artifact. These need correcting:

- **E1. Raw-panel gate (owner question).** The full run failed all three raw panels on binary
  6d5f99b5, because the harness expected `render_proxy == false` at 100%. `xtask/src/raw_panel_smoke.rs`
  was then changed and the same binary passed:
  - the proxy expectation became `true` at both zooms (`raw_panel_smoke.rs:932`);
  - a 1 s held pause, `quiet-at-100`, was added;
  - the 10% white-balance check now compares the release with the post-pause capture
    (`raw_panel_smoke.rs:1223`) instead of the drag capture, and the "one new full-slot version"
    check starts from it (`:1193`).

  Under the old comparison the Air 2S at 100% scores 17.33% of the drag's change (Z6 2.42%, X100VI
  3.69%), so it would fail the unchanged 10% limit. `docs/decisions.md` replaced the "(owner,
  2026-09-26)" bullet with the new criterion and no owner or date, which AGENTS.md forbids. Restore
  the owner's text, record the new criterion as a proposal, report the moving-frame figures plainly,
  and ask the owner.
- **E2. Uncommitted verifier.** The 106-check multi-mask journey and the 45-check fallback journey
  come from `artifacts/viewport-functional-prep/verify.py`, which is gitignored, was never
  committed and was edited after a failing run. `docs/specs/performance.md` cites both a "44-check"
  and a "45-check" fallback journey. Commit the checks as an xtask scenario, or remove the counts.
- **E3. Stale builds.** The viewport timings under "Native viewport-region qualification" in
  `performance.md` come from binary 285ec965 (runs at 02:30–02:32), but production files
  (`photo_surface.rs`, `render/window.rs`, `app/preview.rs`, `presenter.rs`, `overlay.rs`,
  `masks.rs`, `mod.rs`, `snapshot.rs`) were last modified at 02:48–02:58. Only `viewport-60mp-100`
  was re-run on the committed build, 6d5f99b5. The RAW viewport journeys behind TASK-020's RAW
  scope ran on bd694cc3 and 63bd9e3f. "Production SHA 6d5f99b5" is a binary SHA-256, not a commit.
  Re-measure after the fixes (timing runs come last, per AGENTS.md) and label binaries accurately.
- **E4. Blind diagnostics.** `SurfaceDiagnostics` updates `drawn_*` only when something is drawn
  (`photo_surface.rs:1144-1161` and `1169-1188`), so a blank draw leaves the previous identity in
  place and native evidence cannot detect bugs 1 and 2. The `deferred_uploads` counter does show
  them: in the 60 MP 100% journeys it rises at each pause and at release (3, 4, 6, 7), and it rose
  by 97 during the 3 s 200% pan burst. Count draws that show no photograph, and fail the viewport
  scenarios on any.
- **E5. Unreported or wrong figures.**
  - TASK-010 is marked completed, but its acceptance needs the with-and-without-deferral
    measurement in `performance.md`, and only the after values are published. Matched pairs in the
    artifacts: Fit drag 24 MP 10.49/14.38 → 8.59/8.86 ms, 60 MP 15.34/17.86 → 8.56/8.75 ms, 60 MP
    paint 10.17/17.04 → 8.16/11.17 ms (p50/p95).
  - Release to settled histogram at Fit got slower in the same pairs: 24 MP 32.48/35.42 →
    33.23/62.72 ms, 60 MP 48.40/51.83 → 62.03/73.00 ms (2 samples each, loaded host). Report it.
  - The after `editor-performance` run (`final-core-24mp`) started at load 8.43, above the 8.0
    threshold; say so.
  - "157 region adoptions" is 156 regions plus the whole-frame adoption. "Core window tests passed
    13 with one ignored" is 14 plus 1 ignored in `render::window`. The "wider 24/60 MP matrix …
    above" is not in `performance.md`. The "451 × 418 intermediate" is not recorded by any
    committed test; the counter test uses a 1000 × 800 fixture.
- **E6. Stale status-bar text.** `docs/design/instant-preview.md:89` says the bar shows "Rendered in
  7 ms (proxy, approximate)" at Fit and "(approximate)" at 100%, and
  `docs/engineering/development.md:265` says "Rendered in N ms". The app shows "Approximate render
  · N ms" or "Exact render · N ms" (`crates/luxforge-app/src/state/status.rs`). This commit removed
  the user guide's correct examples.

## Checked and sound

- **Pixel parity.** Randomized probes over about 52,000 regions found no difference. They covered
  byte and RAW sources (RAW view orientations 2, 5, 6 and 7, and a white-balance matrix with
  exposure); vignette, radial, range and mixed masks; straight crops, and rotated crops from 4.5° to
  44° with orientation before and after; Presence (Dehaze, Clarity, Texture), masked and not,
  before and after crops; rectangles at every edge, 1 × 1 corners, overflowing and random; 512 px
  tile seams on a 1151 × 793 source; every origin on stages from 1 × 1 to 9 × 9; and frame and
  region in separate render contexts.
- **Coordinates.** Origins are added once (`apply_units`); masks read whole-stage coordinates
  through the cut; resample taps translate exactly; estimates always come from the uncut render;
  `LinearImage::window` is correct for all eight orientations and shares planes; byte windows are
  upright, because the decoder applies EXIF orientation; `scaled_rect` always covers `full_rect`.
- **Cancellation.** Interactive regions read `abandoned` throughout (source build, exact estimate,
  passes, coverage); refinement and whole-frame analysis read `superseded`.
- **Fences.** Release, cancel, history and asset fences in `preview_ready` and `ViewLoaded` are
  correct apart from bug 4.
- **Memory.** Byte bounds hold: at most one full image live or retiring, and two region sets of at
  most 32 MiB each including aprons. Retirement accounting balances. Chunked uploads are correct
  and borrow the raster; chunking does not bound wgpu's own staging, which the docs already call
  unmeasured.

## Verifying the fixes

- While fixing, run the narrow tests (`cargo test -p luxforge-ui photo_surface`,
  `cargo test -p luxforge-app preview`, `cargo test -p luxforge-core window`) and Appendices A
  and B.
- Before handing off, run `quick`; then `rendered`, since UI changes need a real rendered check with
  correlated state and logs; `full` before any "verified" claim. Timing runs
  (`editor-latency --zoom 100 --mode viewport`, the 200% `--moving-pan` burst) come last.
- Update `docs/design/instant-preview.md`, `docs/specs/performance.md`,
  `docs/engineering/performance-rules.md`, `docs/features.md` and `docs/user-guide.md` to the fixed
  behaviour, and answer the performance-rules checklist for the `crates/` changes.

## Appendix A: photo-surface GPU probes

Append this module to the end of `crates/luxforge-ui/src/photo_surface.rs`, then run
`cargo test -p luxforge-ui review_ -- --test-threads=1 --nocapture`. It drives the real
`PhotoPipeline` on a headless adapter and returns early without one, so a pass without an adapter
is not evidence. At d11a1844 the first three tests fail with:

- `review_resumed_drag_after_exact_refinement_has_a_drawable_region`: "revision 4 was adopted but
  the surface can draw nothing for it … slots=[Some((400, 250, Some(3))), None] deferred_uploads
  3->4"
- `review_release_at_100_percent_has_a_drawable_region`: "the committed exact region was adopted but
  nothing is drawable for it: slots=[Some((400, 250, Some(2))), None]"
- `review_a_fit_proxy_of_new_dimensions_is_drawn_in_the_frame_it_arrives`: "the new Fit proxy was
  not written (wrote=false) and the photo slot is None … although only 7852416 bytes were retiring
  against a 512 MiB budget"

`review_retirement_never_blocks_a_render_thread_submit` and
`review_retirement_submit_stall_with_small_gpu_frames` measure bug 3. They are timing diagnostics
and must not become CI gates (performance-rules rule 13).

```rust
/// Review-only GPU probes (not part of the commit): a headless wgpu device drives the real
/// `PhotoPipeline` slot logic, retirement worker and wgpu-core locking.
#[cfg(test)]
mod review_gpu_probes {
    use super::*;
    use iced::widget::shader::Pipeline as _;
    use std::time::{Duration, Instant};

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
                return value;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter =
            block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        eprintln!("adapter: {:?}", adapter.get_info());
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    fn raster(width: u32, height: u32, version: u64) -> Frame {
        let pixels: Arc<[u8]> = vec![0u8; (width * height * 4) as usize].into();
        Frame::new(pixels, width, height, version).expect("a whole raster")
    }

    fn settle(pipeline: &PhotoPipeline) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while pipeline.retiring_regions.load(Ordering::Acquire) > 0
            || pipeline.retiring_full.load(Ordering::Acquire) > 0
        {
            assert!(Instant::now() < deadline, "retirement never finished");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Keep the GPU busy: `copies` copies between two `size`-byte buffers in one submission.
    fn busy_gpu(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: u64,
        copies: u32,
    ) -> wgpu::SubmissionIndex {
        let usage = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        let buffer = || {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("review.busy"),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let (a, b) = (buffer(), buffer());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for index in 0..copies {
            if index % 2 == 0 {
                encoder.copy_buffer_to_buffer(&a, 0, &b, 0, size);
            } else {
                encoder.copy_buffer_to_buffer(&b, 0, &a, 0, size);
            }
        }
        queue.submit([encoder.finish()])
    }

    fn wait(device: &wgpu::Device, index: wgpu::SubmissionIndex) {
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .expect("device wait");
    }

    const BUSY_SIZE: u64 = 64 * 1024 * 1024;
    const BUSY_COPIES: u32 = 96;

    /// A drag at 100%, a pause that refines the region to exact detail, then the drag resumes.
    /// The second resumed revision needs the set that held the exact region, so its upload is
    /// deferred behind retirement; the older revision is hidden; nothing is drawable.
    #[test]
    fn review_resumed_drag_after_exact_refinement_has_a_drawable_region() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline =
            PhotoPipeline::new(&device, &queue, wgpu::TextureFormat::Bgra8UnormSrgb);
        let full_stage = (2000, 1200);
        let interactive = |content: u64, generation: u64, version: u64| {
            RegionFrame::new(
                raster(400, 250, version),
                [100, 100, 500, 350],
                (1000, 600),
                full_stage,
                0.5,
                RegionQuality::Interactive,
                content,
                generation,
            )
            .expect("half-detail region")
        };
        let exact = |content: u64, generation: u64, version: u64| {
            RegionFrame::new(
                raster(800, 500, version),
                [200, 200, 1000, 700],
                full_stage,
                full_stage,
                1.0,
                RegionQuality::Exact,
                content,
                generation,
            )
            .expect("exact region")
        };
        pipeline.write_region(&device, &queue, &interactive(1, 1, 1));
        pipeline.write_region(&device, &queue, &interactive(2, 2, 2));
        assert_eq!(region_draw_order(&pipeline.regions, 2, full_stage).len(), 1);
        // Quiet refinement of revision 2 (deferred once, then admitted after retirement).
        pipeline.write_region(&device, &queue, &exact(2, 3, 3));
        settle(&pipeline);
        pipeline.write_region(&device, &queue, &exact(2, 3, 3));
        assert!(pipeline.regions.iter().flatten().any(|picture| picture
            .region_key
            .is_some_and(|key| key.quality == RegionQuality::Exact)));
        // The drag resumes: revision 3 reuses the half-detail set.
        pipeline.write_region(&device, &queue, &interactive(3, 4, 4));
        assert_eq!(
            region_draw_order(&pipeline.regions, 3, full_stage).len(),
            1,
            "revision 3 is drawn"
        );
        // A frame is still executing on the GPU, as during every drag.
        let busy = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        let deferred_before = surface_diagnostics().deferred_uploads;
        pipeline.write_region(&device, &queue, &interactive(4, 5, 5));
        let deferred_after = surface_diagnostics().deferred_uploads;
        let drawable = region_draw_order(&pipeline.regions, 4, full_stage);
        let slots: Vec<_> = pipeline
            .regions
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|picture| (picture.width, picture.height, picture.content_id))
            })
            .collect();
        wait(&device, busy);
        settle(&pipeline);
        assert!(
            !drawable.is_empty(),
            "revision 4 was adopted but the surface can draw nothing for it: the canvas is blank \
             for this frame (older revision hidden, new upload deferred). slots={slots:?} \
             deferred_uploads {deferred_before}->{deferred_after}"
        );
    }

    /// A quick drag at 100% leaves two half-detail sets. Release commits a new recipe identity
    /// whose settle job starts with an exact visible region (full detail, other dimensions).
    #[test]
    fn review_release_at_100_percent_has_a_drawable_region() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline =
            PhotoPipeline::new(&device, &queue, wgpu::TextureFormat::Bgra8UnormSrgb);
        let full_stage = (2000, 1200);
        let interactive = |content: u64, generation: u64, version: u64| {
            RegionFrame::new(
                raster(400, 250, version),
                [100, 100, 500, 350],
                (1000, 600),
                full_stage,
                0.5,
                RegionQuality::Interactive,
                content,
                generation,
            )
            .expect("half-detail region")
        };
        pipeline.write_region(&device, &queue, &interactive(1, 1, 1));
        pipeline.write_region(&device, &queue, &interactive(2, 2, 2));
        let busy = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        // The committed recipe (content 3) arrives as its settle job's exact region.
        let committed = RegionFrame::new(
            raster(800, 500, 3),
            [200, 200, 1000, 700],
            full_stage,
            full_stage,
            1.0,
            RegionQuality::Exact,
            3,
            3,
        )
        .expect("exact region");
        pipeline.write_region(&device, &queue, &committed);
        let drawable = region_draw_order(&pipeline.regions, 3, full_stage);
        let slots: Vec<_> = pipeline
            .regions
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|picture| (picture.width, picture.height, picture.content_id))
            })
            .collect();
        wait(&device, busy);
        settle(&pipeline);
        assert!(
            !drawable.is_empty(),
            "the committed exact region was adopted but nothing is drawable for it: slots={slots:?}"
        );
    }

    /// At Fit a proxy of new dimensions (rotation, crop, refit) replaces a small proxy.
    #[test]
    fn review_a_fit_proxy_of_new_dimensions_is_drawn_in_the_frame_it_arrives() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline =
            PhotoPipeline::new(&device, &queue, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert!(pipeline.write(&device, &queue, Layer::Photo, &raster(1716, 1144, 1), None, None));
        let busy = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        let wrote =
            pipeline.write(&device, &queue, Layer::Photo, &raster(1144, 1716, 2), None, None);
        let slot = pipeline.slots[Layer::Photo.index()]
            .as_ref()
            .map(|picture| (picture.width, picture.height, picture.version));
        let retiring = pipeline.retiring_bytes.load(Ordering::Acquire);
        wait(&device, busy);
        settle(&pipeline);
        assert!(
            wrote && slot.is_some(),
            "the new Fit proxy was not written (wrote={wrote}) and the photo slot is {slot:?}: \
             the non-viewport draw has nothing to draw, although only {retiring} bytes were \
             retiring against a 512 MiB budget"
        );
    }

    /// Characterise the stall with a frame-sized GPU workload (prints only).
    #[test]
    fn review_retirement_submit_stall_with_small_gpu_frames() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline =
            PhotoPipeline::new(&device, &queue, wgpu::TextureFormat::Bgra8UnormSrgb);
        let full_stage = (2000, 1200);
        let region = |width: u32, content: u64, version: u64| {
            RegionFrame::new(
                raster(width, 200, version),
                [0, 0, width, 200],
                full_stage,
                full_stage,
                1.0,
                RegionQuality::Exact,
                content,
                version,
            )
            .expect("region")
        };
        drop(pipeline);
        for copies in [2u32, 8, 24] {
            let mut gpus = Vec::new();
            let mut stalls = Vec::new();
            for round in 0..5u64 {
                let mut pipeline =
                    PhotoPipeline::new(&device, &queue, wgpu::TextureFormat::Bgra8UnormSrgb);
                let base = 10 + round * 10 + u64::from(copies) * 1000;
                pipeline.write_region(&device, &queue, &region(300, base, base));
                pipeline.write_region(&device, &queue, &region(300, base + 1, base + 1));
                settle(&pipeline);
                let index = busy_gpu(&device, &queue, 16 * 1024 * 1024, copies);
                let started = Instant::now();
                wait(&device, index);
                gpus.push(started.elapsed());
                let index = busy_gpu(&device, &queue, 16 * 1024 * 1024, copies);
                pipeline.write_region(&device, &queue, &region(600, base + 2, base + 2));
                let retiring = pipeline.retiring_regions.load(Ordering::Acquire);
                // The rest of the frame's prepare and draw encoding before its submit.
                std::thread::sleep(Duration::from_micros(300));
                let started = Instant::now();
                queue.submit(None);
                stalls.push((retiring, started.elapsed()));
                wait(&device, index);
                settle(&pipeline);
            }
            eprintln!(
                "{copies} copies: gpu frames {gpus:?}; (retiring, submit stall) {stalls:?}"
            );
        }
    }

    /// The retirement worker waits with `device.poll(Wait)`. The render thread's next
    /// `Queue::submit` must not wait for that.
    #[test]
    fn review_retirement_never_blocks_a_render_thread_submit() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline =
            PhotoPipeline::new(&device, &queue, wgpu::TextureFormat::Bgra8UnormSrgb);
        // How long the busy workload takes on this GPU.
        let index = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        let started = Instant::now();
        wait(&device, index);
        let gpu = started.elapsed();
        // Control: an empty submit while the GPU is busy and nothing retires.
        let index = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        std::thread::sleep(Duration::from_millis(2));
        let started = Instant::now();
        queue.submit(None);
        let control = started.elapsed();
        wait(&device, index);
        // Two region sets, then a region of new dimensions retires the back set while the GPU
        // is busy — what a frame's `prepare` does before that frame's own submit.
        let full_stage = (2000, 1200);
        let region = |width: u32, content: u64, version: u64| {
            RegionFrame::new(
                raster(width, 200, version),
                [0, 0, width, 200],
                full_stage,
                full_stage,
                1.0,
                RegionQuality::Exact,
                content,
                version,
            )
            .expect("region")
        };
        pipeline.write_region(&device, &queue, &region(300, 1, 1));
        pipeline.write_region(&device, &queue, &region(300, 2, 2));
        settle(&pipeline);
        let index = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        pipeline.write_region(&device, &queue, &region(600, 3, 3));
        let retiring = pipeline.retiring_regions.load(Ordering::Acquire);
        std::thread::sleep(Duration::from_millis(2));
        let started = Instant::now();
        queue.submit(None);
        let blocked = started.elapsed();
        wait(&device, index);
        settle(&pipeline);
        eprintln!(
            "busy workload {gpu:?}; empty submit without retirement {control:?}; \
             with retirement in flight ({retiring} retiring) {blocked:?}"
        );
        assert!(
            blocked < Duration::from_millis(4),
            "the render thread's submit waited {blocked:?} for the retirement worker's GPU wait \
             (busy workload {gpu:?}, control submit {control:?})"
        );
    }
}
```

## Appendix B: draft-fence probe

Append to `crates/luxforge-app/src/app/preview_tests.rs`, then run
`cargo test -p luxforge-app review_probe`. At d11a1844 it fails with "draft B's first region was
dropped because draft A had reached revision 10".

```rust
/// REVIEW PROBE: a region of a *new* draft B must not be rejected because the frame on screen
/// came from an earlier draft A at a higher revision. Draft revisions restart at 0 per draft.
#[test]
fn review_probe_new_draft_region_is_not_fenced_by_an_older_drafts_revision() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    // Draft A's revision 10 was the last frame presented.
    editor.displayed_draft_revision = Some(10);
    editor.presented_generation = 7;
    // Draft B is now the session's live draft, at its first revision.
    let asset = editor.state.as_ref().unwrap().asset.id.clone();
    let mut draft_b = luxforge_core::Draft::new("basic.set", asset, 4);
    draft_b.draft_revision = 1;
    editor.session.draft = Some(draft_b.clone());
    let (analysis, raster) = drafted(&editor, 8, &draft_b.draft_id, 1, &[[40, 50, 60, 255]], 1, 1);
    let rect = luxforge_core::Region { x0: 0, y0: 0, width: 1, height: 1 };
    let stage = luxforge_core::StageSize { width: 1, height: 1 };
    editor.preview_generation = 8;
    ticket(&mut editor, 8, 2);
    let (_, shown) = editor.region_ready(luxforge_core::PreviewResult {
        generation: 8,
        entry_id: analysis.identity.entry_id.clone(),
        identity: analysis.identity,
        draft_revision: Some(1),
        intent: luxforge_core::PreviewIntent::Interactive,
        viewport_declined: None,
        outcome: luxforge_core::PhaseOutcome::Region(luxforge_core::RegionOutcome {
            frame: luxforge_core::RegionFrame {
                raster: raster.as_ref().clone(),
                rect,
                stage,
                full_rect: rect,
                full_stage: stage,
                approximation: luxforge_core::ProxyApproximation::default(),
            },
            mask_overlay: luxforge_core::MaskOverlayOutcome::default(),
        }),
        approximate_white_balance: false,
        render_ms: 1.0,
        queue_wait_ms: None,
    });
    assert!(shown, "draft B's first region was dropped because draft A had reached revision 10");
    finish(editor, catalog);
}
```
