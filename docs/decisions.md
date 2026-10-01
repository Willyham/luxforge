# Product decisions

Accepted owner decisions and the questions still open. Proposals stay proposals until the owner decides; record each answer here and in the affected spec.

## Product and platform

- A fast, non-destructive desktop editor for professional and prosumer collections on macOS, Windows and Linux.
- The owner's M4 MacBook Pro (M4 Pro, 14 CPU and 20 GPU cores, 48 GB unified memory, macOS 26.5.2) is the first target and the reference machine.
- Engineering baselines: macOS 14+ arm64, Windows 11 24H2+ x64, Ubuntu 24.04 x64 with GNOME Wayland and X11. These are intended targets, not verified support; see [platforms](engineering/platforms.md).
- Unsigned development packages only. No signing, stores, auto-update or public release yet.
- Project code is GPL-3.0-or-later. Dependencies and extensions should be open source and license-compatible; no proprietary hosted service is a development prerequisite. The manual license, native and asset review is deferred and is not a passing result.
- Everything is v0 and breaking changes are expected. Only current catalog, recipe, API and module shapes are supported; no migrations, compatibility shims, old-version fixtures or historical parity requirements. Unsupported data is refused without rewriting it. No release-version planning, cloud or accounts, marketplace or generalized processing graph.
- Initial RAW targets are the original Nikon Z6 and the Fujifilm X100VI; implementation is requested, with the supplied DJI Air 2S DNG added for qualification. Benchmark established decoders before proposing a custom one.
- RAW editing stays continuous and non-destructive, in the same workflow sense as Lightroom: the original remains the source, adjustments remain recipe data and later edits do not operate on a JPEG baked from earlier WB/exposure settings. Keep high precision through editing and convert for display or explicit export. Neutral development is the initial direction; this does not select Adobe or camera-look matching. See the [initial RAW design](design/initial-raw.md).
- Lightroom Library and Develop are familiarity references. Map, Book, Slideshow, Print, Web and Publish Services are out of scope.
- All development tooling is Rust (`cargo xtask`); no second toolchain.

## Editing and storage

- Originals are read-only. Import references existing files with a stable asset ID, a verified content fingerprint and a changeable locator. SQLite is the local catalog. Folder relinking, sidecars, portability, backups and sync need their own workflow decisions.
- A "layer" is an ordered edit operation in a recipe. Each committed action stores a complete immutable recipe snapshot and one attributed history entry. Bitmap compositing, blend modes and arbitrary layer reordering are not selected.
- History is a graph: entries keep their undo parent and nothing is truncated. A named **version** (the owner's name for the Lightroom-style saved state) is a reference to one retained entry, not a branch. The catalog uses its [current internal format](design/versions-and-lineage.md#storage-catalog-format-12); unsupported formats are refused.
- Undo and redo navigate saved entries without appending rows. Preview is read-only. Restore appends an action and keeps all later entries. A new edit clears shortcut redo, but every entry stays available. Committed state survives restart; drafts do not.
- One workspace: centered photo, collapsible controls, visible history, Fit, numeric zoom and true 100%. No library grid during the editor milestones. Cmd/Ctrl+O imports; Cmd/Ctrl+Z and Shift+Cmd/Ctrl+Z navigate history.
- Geometry: the visible composition travels with mirror and quarter-turns, and a locked ratio swaps orientation on a quarter-turn. Fine angle is limited to ±45°. Space-drag pans.
- Pixel-stage edits address the content stage (the source after EXIF orientation) and are placed before quarter-turns, reflections and the crop, so changing the crop never moves or invalidates them. The host chooses a new layer's position from its effect stage. See [content-space edits](design/content-space-edits.md).
- Quarter-turns and reflections are one orientation layer holding the composed exact state. It sits ahead of the crop and any finish layer, is updated in place, and carries a later crop through the transform; four rotations leave one neutral layer. See [orientation layer](design/orientation-layer.md).
- Selecting the current entry in history is Return to current, not a historical preview.
- Crop (M4): free handles, composition move, thirds overlay, Free/Original/1:1/3:2/4:3/16:9/custom ratios, a drag-to-straighten guide, Apply, Cancel and reset. Option/Alt scales proportionally about a fixed center. Straightening preserves composition with only the trimming needed. Apply commits once; Cancel discards.
- Export (follow-up): JPEG quality 90, native destination picker, suggested `-edited.jpg`, never overwrite an existing file or a source alias. Optional metadata is stripped by default; Keep metadata retains supported descriptive, capture and GPS fields with correct geometry and profile.
- Recovery (follow-up): verified manual Locate keeps asset identity, layers and history and rejects changed or ambiguous sources.
- Live agents: human and agent actions share one history. An external commit preserves a human draft and marks it conflicted, resolved by explicit Discard or Reapply. Live JSON/IPC exists from M1; MCP is a later adapter over the same operations.

## Develop workspace

Accepted on 2026-09-20 for the [Develop workspace](design/develop-workspace.md) shell over the modules that exist today (pixel, transform, crop):

- State panel on the left (versions, history, recipe), tools panel on the right, both collapsible independently.
- Modules render as stacked collapsible sections in registry order. A build lists only registered modules; nothing is drawn for modules that do not exist.
- A history row shows the action title plus a one-value summary supplied by the module; the host stores the rendered label with the entry. The module supplies it through `label()` since the [post-consolidation review](#post-consolidation-review), which replaced declared `summary` templates.
- Test modules (pixel proof) live in a Developer section that is hidden unless the desktop is launched with `--developer`; the registry marks them `developer: true`, and since the [post-consolidation review](#post-consolidation-review) they register only in developer mode, so a normal build's API lists none of their methods.
- Compare is hold-`\` for the Original entry framed by the displayed entry's geometry (`preview.select` with `keep_geometry`), and Shift+`\` for the uncropped Original, released through `preview.return-current` or the previous selection. A tap of `\` (release within 200 ms) or a click of Compare toggles an aligned Before/After divider, with another tap of `\` or Escape to exit; `preview.compare` owns its fixed After entry, prior selection and divider position. Holding `\` during the slider temporarily shows Before across the photograph. Dark theme only; a light theme is not planned.
- Basic, histogram, export, Locate, heal and mask are outside this work. Their sections, buttons and notices are left out of the build entirely rather than drawn as placeholders. The generated tools panel must accept a `number` slider module without desktop changes, which is how Basic lands later.

Decided on 2026-09-21:

- A picking mode belongs beside the controls it fills, not in the hovering mode strip: a module that declares a `point-pick` or `sample-apply` canvas declares a `picker` control in its own panel (Basic's Neutral picker in the White balance group, RAW's Neutral WB in the RAW group), and the strip holds the pointer, canvas-takeover modes (crop) and view overlays only. Declared letters and the command palette still enter every mode.

Decided on 2026-09-23:

- The state panel's [Performance section](design/performance-panel.md#decisions) starts open on every launch. Memory is shown in binary units with Activity Monitor's MB and GB labels, and CPU as a percentage of one core, so it passes 100% whenever more than one core is busy.
- A module whose controls are a single group shows them without a sub-group header: a header naming the module's only group, such as RAW's "RAW development" or Transforms' "Exact transforms", repeats the band above it. The band keeps the module's reset. Descriptors and the API are unchanged.

Decided on 2026-09-26, aligning the shell with the boards:

- The histogram has no caption at all, neither a row under the plot nor a hover tooltip; the pointer readout stays in the status bar.
- The status bar leads with a plain sentence of what last happened. Entry, snapshot and source identifiers are not shown or copied there; they remain available through the API.
- On macOS the app's title bar is the window's title bar: a transparent, full-size-content native bar with the traffic lights inside the app's own. Windows and Linux keep their native frames.
- The state panel has no Recipe section. The layer stack stays readable through `recipe.describe`.

## Basic adjustments and histogram

Accepted on 2026-09-21 for the [Basic and histogram design](design/basic-and-histogram.md). These settle the product questions; implementation was authorized the same day and is delivered.

- JPEG first: the Basic controls and the histogram work on the supported SDR sRGB and greyscale JPEG subset. RAW is separate later work with its own input and colour contracts, not a prerequisite.
- One Basic layer per recipe with a fixed internal group order (White balance, Exposure, tone curve, Vibrance, Saturation), placed by its colour stage before the geometry tail and updated in place at the same identity.
- Slider release, key-up or Enter commits one action; Escape cancels; focus loss cancels an unfinished gesture. There is no Apply panel for adjustments.
- Global pointwise tone first: Contrast, Highlights, Shadows, Whites and Blacks are one monotone global curve before any edge-aware processing is considered.
- Numerical ranges and equations start from the Lightroom research and the design's proposed ranges and defaults; the numerical tasks select and freeze them against independent references before a control ships, and no value is claimed as Lightroom-equivalent.
- Priority: Slice A (histogram, clipping and Exposure) next, then Slice B (the remaining Basic controls); export, Locate and MCP keep their own follow-up priority.

The work ran to completion on the defaults below without further owner input; the owner reviews and refines the result afterwards. Each default is provisional and recorded in the design, so a later change is a normal edit, not a silent reinterpretation. Measured results against the provisional thresholds are in [performance](specs/performance.md).

- Performance targets are provisional thresholds: settled exact histogram p95 below 200 ms and a 64 MiB aggregate scratch target. The slider-to-presented-frame target was 100 ms p95 at first; on 2026-09-22 the owner set it to **16 ms p95 with an acceptable bound of 32 ms** (one and two frames at 60 Hz), so a figure below 16 ms passes, one below 32 ms is acceptable and one at or above 32 ms is a miss. A measured miss is reported with its figures and does not block delivery.
- Global tone stays global. If the tone study finds a visual case a global curve cannot pass, the control ships with that limitation documented and an edge-aware proposal recorded as later work.
- Clipping overlays: any channel at an endpoint counts; shadow clipping draws blue, highlight red, both magenta; tooltips state the rule.
- Neutral picker: a 5 × 5 patch at input-stage pixel centres clipped at the image edges, evaluated before the Basic layer; near-black, clipped and non-invertible samples are rejected with a reason.
- One Basic layer per recipe; more than one is never user-facing, and an imported stack with several reports ambiguity.
- Float exactness: results match an f64 stepwise reference within `1e-6 + 1e-6 × |reference|` and at most one output code of rounding where the design permits it.
- After Slice B, Tone Curve is the next module candidate; Detail, Texture and Clarity, Dehaze and the colour mixer follow in that order unless the owner reorders them.

## UI components

Accepted on 2026-09-21 for the [UI components design](design/ui-components.md), the closed control vocabulary modules may declare, and implemented; see [feature status](features.md).

- No scroll-wheel editing of controls in v0, and no preference to enable it: a trackpad scroll over a panel of sliders must never edit a photograph.
- `text`, `pad` and the `string` parameter kind are the second slice and wait for the colour mixer design; `curve` channels ship with the curve kind.
- Icon buttons are a declared style, so the Unicode glyphs are replaced with canvas-drawn vector paths in this work.
- Curve interpolation belongs to the module and is sampled through a declared query; the host owns no curve spline and the widget never interpolates.
- Option steps by one tenth of the declared step alongside Shift at ten times; the modifier can change later without a descriptor change.

## Module capabilities

Decided on 2026-09-23 under the owner's delegation for the [shared module capabilities](design/module-capabilities.md) ("make sensible decisions, don't block on me"); each is a default the owner can change.

- Settings are user-level only: module settings and named provider profiles, outside every catalog, with no history entries. Secrets live only in the OS credential store, starting with the macOS Keychain; a locked or unsupported store fails explicitly with no plaintext fallback.
- Sending image data is consented **per asset**: a grant names the module, profile, adapter, endpoint origin, data class and asset, and is not remembered for later photos. Downloads are granted per resource version and origin.
- Only the desktop (after Allow) or `luxforge-json --permission-authority` may grant. Live-session clients cannot; anyone may deny or revoke. Revocation cancels dependent jobs and never touches recipes, history or accepted artifacts; an endpoint change revokes the old grants.
- Remote endpoints require HTTPS and public addresses; plain HTTP is allowed only to loopback, labelled as such. No proxies.
- No remote provider adapter ships with the framework; the first real adapters arrive with Corrections. `managed-storage` and `local-runtime` wait for their first consumer.
- Derived artifacts live in a directory beside the catalog and move with it; the [current catalog format](design/versions-and-lineage.md#storage-catalog-format-12) holds their references beside the preset library, the mask table and the stroke store, and earlier formats are refused.

Revised by the owner on 2026-09-24, after the [architecture review](#architecture-review):

- The framework stays and is trimmed to what its consumers need: bound artifacts travel on the recipe as strokes do, `artifact.relocate` goes because the directory moves with its catalog, a grant no longer records its last use, settings and grants share one document store, settings use the module parameter vocabulary, capability jobs report through the activity board, and the proof endpoint leaves the shipped core crate.
- The transport uses a well-known, tested HTTP client, pinned (such as `ureq`), behind the existing address policy, instead of the hand-written HTTP/1.1 client. The policy itself is unchanged. On 2026-09-26 this was confirmed as `ureq`'s own agent, after a spike on cancellation ([post-consolidation review](#post-consolidation-review)).
- The `read-user-file` capability and the `file` setting kind are removed until a module needs them. They were thought to serve presets, but `preset.import` takes the file's text from its client, so nothing uses them. A module that reads a user-chosen file may add them back later, consented per canonical path.

## Programmable operations and modules

Every operation is programmable, including future tools, masks, clone strokes, settings and module lifecycle. The core owns recipe transactions, history, invariants and bounded services; tool modules own parameters, validation, controls and algorithms through those APIs. M3 uses linked modules with cheap registration and lazy resources. Real external loading is required later, and a missing or disabled provider must never silently erase edits or produce an incomplete export. Still open: the first external use case, package and runtime format, trust and UI contribution. See [modules](design/modules-and-api.md).

## Owner workflow priorities

The owner edits local files and syncs them to an external drive, so moved-original recovery matters. These are priorities and problem statements, not authorization to implement every solution.

| Priority | Direction |
| --- | --- |
| Speed and unused-tool bloat | Measure startup, loading, idle and first-use costs; keep the core small and initialize lazily |
| Filtering, tagging and collections | Design one retrieval model from concrete workflows |
| Full catalog with lazy shoot subsets | Keep catalog scale independent of decoded pixels; scope views to the working set |
| Multi-selection and stacking | Define selection ranges, stack identity and batch semantics in later library work |
| Bracket and panorama identification | Research detection separately; merging is not selected |
| Confusing export controls | One clear JPEG export path with explicit metadata behavior |

## Presets

The owner asked on 2026-09-23 for presets, with native presets and Lightroom import through a presets module whose apply is a history entry, and for the work to proceed without blocking on questions. It is delivered on the defaults recorded in the [presets design](design/presets.md#decisions-taken-on-defaults), each a proposal the owner reviews: presets as catalog data (see [versions and lineage](design/versions-and-lineage.md#storage-catalog-format-12)), only field-patch actions presettable, `apply-preset` carrying its settings, Lightroom values transferred for the controls Luxforge has and never clamped with RAW Kelvin and tint refused, the section first in the tools panel, and white balance unchecked when creating a preset.

## Rendering memory

- A shared working-memory budget is a target that keeps memory low, not a limit that refuses the user's work (owner, 2026-09-23). Work that needs more than the target has left still runs and completes. The 256 MiB spatial budget lowers how many tiles run at once, down to one. The 64 MiB colour scratch budget's row chunks are sized so the pool's workers stay well inside it, and a chunk past it still runs. Both keep a high-water mark that the timing tier reads against the target. Size limits on what is accepted — source and frame sizes, the halo and unit bounds a module declares — still refuse with `resource-limit`.
- When the spatial target holds a render's batch to fewer tiles than the pool has workers, each tile's own passes run on the pool rather than the target being raised (owner, 2026-09-23): the same bytes and the same memory, all three Presence fields at 60 MP in about 3.9 s instead of 11 s, for about twice the CPU time. An operation with a large summed halo now runs in larger tiles ([post-consolidation review](#post-consolidation-review)).
- Editing responsiveness comes first, and caching is an expected way to get it (owner, 2026-09-30). When a measured interaction repeats work whose inputs have not changed, a bounded, disposable cache is weighed by the memory it holds against the time it saves, not ruled out; no rule forbids caches as a class. The conditions a cache meets are [performance rule 14](engineering/performance-rules.md#rules). A cache whose budget adds to the RAW working set or another memory target still needs the owner's acceptance or delegation. The candidate caches are proposals in [instant previews](design/instant-preview.md#proposals-and-later-work).

## Architecture review

Decided by the owner on 2026-09-24 after a whole-codebase review of `main` at `7ce9557`. The owner decided the first two and asked for the review's recommendation on the rest. The consolidation is delivered and its outcome lives in the specs it changed; the post-consolidation programme that followed it completed on 2026-09-29.

- Consolidate rather than rewrite. Each cross-cutting mechanism keeps one implementation that every feature extends, and the copies are deleted. That covers committing and planning an edit, method dispatch and parameters, jobs, latest-job workers, desktop drafts, the JPEG and RAW evaluators, field-patch modules, colour math, smoke scenarios and test support.
- **Controls are the same for every source kind.** A JPEG and a RAW photo show one Exposure control and one White balance (temperature and tint) control set, as Lightroom does. Each control behaves as its source requires: on a RAW photo it sets the source development's white balance and exposure, and on a JPEG it sets Basic's relative adjustment. The RAW section's duplicate Exposure and white-balance controls merge into that one set. A module's applicability to a source kind is declared, not named by the desktop.
- **Mask coverage stays bit-identical** to its frozen `f64` reference. The mask gesture's extra runtime hop is fixed first. Faster `f32` coverage within the `1e-6` tolerance is considered only if a measurement then shows coverage limiting a paint gesture.
- The brush's cap of 64 segments per grid cell is measured on a realistic back-and-forth scrub before it changes. How a stroke that would pass it is handled was decided on 2026-09-26 ([post-consolidation review](#post-consolidation-review)).
- The crop draft moves onto the core `draft.*` lifecycle once the desktop has one draft driver, so agents see it in `session.state`. The crop geometry and canvas stay as they are. This supersedes "the crop draft stays desktop-local".
- Every mutating method carries `{request_id, actor}` and is deduplicated, with `expected_revision` wherever a revision exists, so an agent can retry any mutation safely. This supersedes the presets default that library methods take no mutation envelope.
- A mask's coverage grid never waits for the exact render, because it reads no pixel of the exact frame: the live overlay is filled by its own coverage worker from the preview's evaluation.
- Mask, component and stroke identities become a declared parameter kind, validated and deduplicated like any other parameter.
- The path primitives stay host primitives, and the recipe stops scanning every layer payload for strokes until a consumer other than masks exists.
- The developer component gallery's page is desktop view state and leaves the core session schema.
- The curve control vocabulary stays; the Tone curve is its product consumer, beside the developer controls proof.
- `probes/s0` and `cargo xtask probe` are removed; the Iced selection stays recorded in the research docs.
- The Develop workspace mockup images stay in git without LFS and are regenerated only when the design changes, because every agent worktree would otherwise need LFS.
- Fusing Basic's colour units (exposure into the white-balance matrix, and one Oklab pass shared by colour and the mixer) waits for a measurement showing that the pointwise pass limits a gesture.

## RAW white-balance drafts

- The original `raw-panel` criterion (owner, 2026-09-26) compared the drafted frame shown during motion with the released exact frame relative to the drag's own change: within 10% at Fit and at 100%, and within one code on average at Fit. On the recorded final review binary the Air 2S moving 100% result missed that criterion at 17.33%; see [instant previews](design/instant-preview.md#a-raw-white-balance-during-a-drag).
- The owner revised the 100% qualification on 2026-09-27: assess motion and a held, full-detail draft separately. At 100% the 10% relative-to-adjustment quality gate applies to the approximate drafted frame **after full-detail visible-region refinement under the shared 120 ms quiet policy, before release**. Motion must present the correct viewport identity, label the white balance approximate, respond visibly and refine to full detail; its half-resolution softness is recorded, without a chosen numeric softness threshold or a claim that every photo's moving quality is acceptable. At Fit the 10% relative gate and one-code mean gate remain. Release still redevelops RAW exactly. The recorded moving residuals (Z6 2.42%, X100VI 3.69%, Air 2S 17.33%) mix softness and white-balance error; fresh native `raw-panel` runs of the revised verifier pass for the Z6, X100VI and Air 2S, with held 100% residuals of 0.805669%, 1.387562% and 5.056605%, respectively. The original moving residuals remain recorded separately. These checks do not establish a general photo-error bound.
- **Highlight-clipped Bayer scenes are an exception to the white-balance gates** (owner, 2026-09-30). Drafts keep the approved `W = R · diag(g'/g) · R⁻¹` at its current per-pixel cost. The `raw-panel` scenario classifies a scene from the source itself: the share of the default crop's Bayer sites at 0.99 of sensor white or above under the channel-wise largest gains its drags develop at. A Bayer development with no DNG correction after the demosaic qualifies at 1% or more. For a qualifying scene a missed Fit or held 100% accuracy limit is recorded, with the exception, the clip share and the limit, and does not fail the scenario. Every other check stays a gate, and the limits are unchanged for every other scene. Of 33 measured sources, the five that miss a limit clip 1.25–4.07%: OM-1, Panasonic S5II, Sony A7 IV and A7 III, and Nikon Z6II. Two sources that pass also qualify, at 2.50% and 1.42%, and the next Bayer scene clips 0.61%. The demosaic clamps each gained site at sensor white, and the first-order `diag(g'/g)` cannot follow that clamp. The measurements are in [instant previews](design/instant-preview.md#popular-cameras). A clip-aware draft that reverses the clamp per pixel in camera RGB is deferred, to be measured and reconsidered later ([roadmap](plan.md)). Its cost was never timed. Every white-balance accuracy figure averages over the photograph alone (owner, 2026-09-30): the canvas beside a 3:2 or 4:3 photo at Fit never changes and diluted the Fit one-code mean by 15–24%.

## Post-consolidation review

Decided by the owner on 2026-09-26, who took the recommendations of a whole-codebase review of `main` at `4f8c3e1`. All six of its waves, the roadmap groundwork included, completed on 2026-09-29; each spec changed with its behaviour.

- **Declared source kinds land now.** `EffectDescriptor.sources` and one applicability rule replace every check of the RAW module's name, ahead of the open questions in [source controls](design/source-controls.md), none of which they touch.
- **The transport runs on `ureq`'s own agent** behind the unchanged address policy, after a one-day spike proves that cancelling a request by shutting its socket down works through rustls. If it cannot, this decision is amended to record that Luxforge owns its I/O loop.
- **A brush stroke that would pass the occupancy cap is refused when it is painted**, not when a layer first draws the mask. The cap stays at 64 segments per grid cell. A measurement on a realistic back-and-forth scrub sizes a decimation tolerance relative to the brush radius, which lowers occupancy without changing the coverage of any stored stroke. A stroke that reaches the cap does not start a new component, because components combine by maximum and painting would stop building up.
- **Undo, Redo and Restore are refused while a draft is open**, with the reason in the status bar, as every other discrete commit is.
- **A panic while the catalog owner serves a message is contained** and answered as `internal`, and the owner keeps serving other clients. Every worker contains its panics the same way.
- **`verify` reports a tier with a skipped or unrun component as incomplete**, with a distinct non-zero exit that names what was skipped.
- **The module capabilities framework stays and is trimmed further.** Activation is deferred with `local-runtime` until a module needs it. The desktop surface shrinks to the consent notice, the task control, the resource row and a settings form built from the tool controls. A resource installs from its pinned URL only. At the 1,024-record cap the oldest remote-image grant is evicted instead of a new grant being refused; asking again only reduces privilege.
- **A drafted preview may skip its exact phase at Fit while the gesture moves**, so the histogram and clipping overlay refresh on a pause or the release, if `editor-latency --mode paint` shows it closes the paint latency tail.
- **A crop draft at Fit shows its input stage at display resolution** from the proxy, with the exact stage only at a percentage zoom, and opens before the stage's pixels arrive.
- **The desktop's per-region derivation keys are removed** if the logged derivation stays under about 0.2 ms per message; otherwise only the heavy region is keyed, on one value. They are removed (decided on 2026-09-28 by the wave-4 agent under the owner's delegation, once measured): with every region derived on every message the derivation costs 0.05 to 0.14 ms at p50, but its p95 on the heavily shared host is above the line in most runs (0.08 to 0.68 ms by launch for the kept build; the keys-bypassed build passed 0.2 ms in two of the three repeated drags and in every burst), while the keyed build's own drag p95 reached 0.37 ms and a launch's `view()` inflated with its derivation, so the tail follows the host more than the build. The heavy region is the tools panel, 73 to 77% of the time (52% in a masked drag, where the Masks panel takes 43%), spread over its sections by their controls; it reads almost every editor input, so no one value keys it without the input tracking the keys were built on, and a drag derives it for every input anyway. Input to presented frame is unchanged. The figures are in [performance](specs/performance.md#the-workspace-derivation-per-message).
- **`draft.begin`, `draft.cancel` and `draft.reapply` run synchronously on the desktop thread**, like `draft.set`, proven by a press-to-first-frame measurement; only `draft.commit` stays an owner task.
- **One job API.** `job.read` and `job.cancel` serve every job kind, in place of `analysis.read`, `analysis.cancel`, `module.job.read` and `module.job.cancel`. **Events name the asset and revision they changed**, and JSON clients get an `events.wait` long poll before the MCP adapter.
- **A RAW source keeps a second development within a 600 MiB byte budget** (decided on 2026-09-28 by the integrator of the speed wave, under the owner's delegation, once the development executor and normalization changes had landed). The X100VI's hold-`\` compare still took about 250 ms to display after those changes and the eviction fix, above the owner's 150 ms bound, so the byte-budgeted second slot was chosen over a proxy-only compare: it keeps the compare exact, needs no second display path, and a proxy-only compare would still need a development at the other entry's gains to build its proxy. The slot keeps the most recently used other development, `luxforge_raw::RETAINED_DEVELOPMENT_BYTES` bounds it (the X100VI's fits, one at the 128 MP limit does not), and the plane gate still lets the source worker build one development at a time. A repeated Original/current switch then displays in about 20 ms on the X100VI and the Air 2S and about 32 ms on the Z6, at a steady cost of 230 to 460 MiB of RSS; the figures are in [performance](specs/performance.md#second-development). A byte-budgeted source cache that keeps JPEG decodes beside the RAW developments follows an RSS measurement on the M4.
- **Frames are opaque by contract**; the renderer carries no alpha.
- **`editor-acceptance` keeps only what `cargo test` cannot prove at the same layer**: the release run of the conformance suite, Basic numerics on the photo fixture, placement, and mask reopen through a fresh owner.
- **The components board shows widget states only**; the real panels are proven by their own scenarios.
- **A module supplies its history label through `label()`**, with its title as the default, in place of `summary` templates. Stored labels are unchanged.
- **Test modules, including the pixel proof, register only in developer mode.** Whether `PointReplace` seeds the Corrections repair primitive or is deleted is decided with the repair stage.
- **A spatial operation with a large summed halo runs in 1024 px tiles** (decided on 2026-09-28 by the integrator of the speed wave, under the owner's delegation of this decision). An operation whose summed halo at its stage is at most 128 px keeps 512 px tiles, and one past 128 px runs in 1024 px tiles, so Texture and Dehaze stop recomputing Clarity's margin around every small tile. One rule, `luxforge_raw::spatial_tile`, gives the side to the render, the point sample and the windowed proxy. The bound is where the measurement put it: up to 128 px, 1024 px tiles rendered Texture alone 9 to 14% slower and every other stack as fast or at most 14% faster on a stage of 10 MP or more (Dehaze alone at 47 to 107 px, Texture with Dehaze at 53 to 121 px, Clarity at 127 px), while a sample through them cost three to four times as much; past 128 px every stack rendered 17 to 51% faster (Clarity from 151 px, and every stack that holds Clarity on the generated 24 MP and 60 MP JPEGs). Texture alone would also sample in 37 ms instead of 9 ms, so it keeps 512 px tiles. So do Dehaze alone and Texture with Dehaze at every stage size, because Dehaze's halo never passes 107 px. The 256 MiB spatial target still sets how many tiles run at once, and a tile whose working set is larger than the target still runs alone, one tile per batch. Every Presence combination rendered the same SHA-256 in 512 and 1024 px tiles on the generated 24 MP, 60 MP and presence fixtures. That identity rests on this evidence, not on construction, because the box passes' running sums start at each rectangle's edge. The figures are in [performance](specs/performance.md#tile-size-by-summed-halo).
- **The ignored RAW diagnostics in the core's `render/linear.rs` are deleted** (decided on 2026-09-29 by the integrator of the tests wave, under the owner's delegation), with `luxforge-raw`'s `performance-diagnostics` feature that existed only for them. The RAW colour-row diagnostic compared production against the generic pixel evaluator from before the domain unification, a historical parity check the project does not keep. Concurrent Fit proxies against a RAW development are left to the RAW plan's whole-editor measurement, which measures the owner's responsiveness and cancellation through the editor rather than one core-only overlap.

Not adopted: deferring the whole capabilities framework until the first Corrections adapter. Detail's placement and preview policy are selected under [Detail](#detail).

## Interactive previews at 100%

Decided on 2026-09-27. The owner accepted the viewport behavior and authorized implementation; current behavior is described in [viewport rendering](design/instant-preview.md#viewport-rendering-at-100).

- Immediate adjustment feedback takes priority while dragging. A briefly softer image at 100% is acceptable during motion, with full detail restored on pause or release and the full-image histogram marked updating meanwhile.
- Clipping follows the displayed viewport while exact region pixels are outstanding: the overlay is marked approximate, uses the existing OR-of-clipped-pixels rule on a viewport-bounded grid with the existing 4096-cells-per-side cap, and is replaced by a matching exact-region overlay on refinement. Region, content and quality identity travel with the overlay; missing or stale tiles have no overlay. Viewport clipping never substitutes for whole-image counts.
- Exact settled pixels, full-image analysis, sampling and export remain the reference. The viewport implementation uses the documented half-scale proxy during motion and exact visible-region refinement under the shared quiet policy; unsupported stacks use their named fallback.
- The implementation authorization is not a latency or total-memory qualification result. Additional quality levels, guide approximation and GPU preview arithmetic, plus their numerical error limits, remain open in [product decisions](../tasks/product-decisions.json).

The owner provisionally accepted on 2026-09-27 the surface's temporary overlap ceiling: one current and one retiring full-photo allocation of at most 512 MiB each, plus two region sets of at most 32 MiB each, for up to 1088 MiB of photo textures including retirement. This is an engineering ceiling for those slots, not a total editor or GPU product budget. Crop GPU textures and tiles, overlays and backend upload staging are outside that total and are not fully measured. Their residency must be measured and bounded before a total-memory guarantee; the follow-up is in the [rendering plan](../tasks/rendering.json).

## Export

Accepted on 2026-09-27 for the delivered [JPEG export](design/export.md#decisions), beyond the export contract under [editing and storage](#editing-and-storage):

- The title bar's Export button opens a two-item menu, Export JPEG… and Export JPEG, keep metadata…, with `Cmd+E` and `Shift+Cmd+E` and both in the command palette.
- The desktop exports the displayed entry, including a history preview, and never a draft.
- The suggested name counts up (`-edited-2.jpg`) when `-edited.jpg` is taken.
- Keep metadata carries exactly the design's EXIF field set, and no IPTC or XMP.
- The encoder is libjpeg-turbo through the `mozjpeg` crate at its fastest profile: baseline, 4:4:4, one interleaved scan, standard Huffman tables. It is more than twice as fast as the `image` encoder at the same output; the comparison is in the [export design](design/export.md#decisions).
- Import decodes JPEG with the same libjpeg-turbo (owner, 2026-09-27), for dependency consolidation rather than speed: shipped code uses one JPEG library, and `image` keeps PNG only. The decoded pixels differ from the previous decoder's by at most 3 codes ([performance](specs/performance.md#jpeg-decode-libjpeg-turbo-against-zune-jpeg)).
- The codec is its own crate, `luxforge-jpeg` (owner, 2026-09-27): the one crate that names `mozjpeg`, depending on no workspace crate, with the JPEG container (the marker walk, the ICC chunks both ways) and the safety and recovery around the C library; the core keeps the policy ([architecture](design/architecture.md#the-jpeg-codec)). Its decoder goes on past libjpeg's harmless warnings and refuses the ones that stand for lost data (owner, 2026-09-27): truncated or corrupt image data, a scan out of step and anything libjpeg would guess are refused, as is any warning the crate does not list, while bytes between header segments and an unknown JFIF version are accepted, with the same pixels as the clean file.
- JPEG import stays strict (owner, 2026-09-27): extraneous bytes after a scan, an unknown Adobe colour transform, non-sequential scans and any data after the end-of-image marker are refused. Accepting trailing data (for example the video a Motion Photo appends) would mean reading past the image, a denial-of-service and parsing surface the owner does not want now; each relaxation is added deliberately, with its own review, only when a real file needs it.
- The methods are `export.plan`, `export.jpeg`, `export.read` and `export.cancel`; the export lane is its own instance of the lane runner until one job table exists.
- The earlier state-panel export proposal (presets, resizing, unique names by default, durable export records) is not adopted.

## Source-kind controls

Decided by the owner on 2026-09-27, who took every recommended default of the [source controls](design/source-controls.md#decisions) design:

- **Exposure is Basic's on every kind.** On a RAW photo it multiplies the developed scene-linear planes in Basic's colour stage, and the source development carries no exposure.
- **A masked white balance on RAW is relative**, Basic's ±100, as Lightroom's local Temp and Tint are.
- **JPEG keeps the frozen relative ±100 Temperature and Tint**, with As shot at 0 and 0. Basic's tint unit stays twice RAW's.
- **RAW keeps 2000–12000 K and ±100 Luxforge tint.**
- **Lightroom `Temperature` and `Tint` import converts** both together through the illuminant chromaticity they name, and refuses the pair when either result is outside the RAW ranges. It converts values and does not match renderings.
- **Presets store each kind's white balance separately.** Apply skips and reports a setting that does not apply to the photo's kind.
- **Reset Basic on a RAW photo also returns the development to As shot**, as one history entry.

## Masks panel

Decided on 2026-09-28. The owner delegated the behaviour choices of the [Masks panel design](design/masking-workspace.md#decisions) and asked for the panel to match its boards; current behaviour is described there and in the [user guide](user-guide.md#masks).

The owner requests [interaction repairs](design/masking-interactions.md): unplaced click-drag creation, actual coverage while drawing/painting, `O` for mask visibility, exclusive creation, correct selection/brush targets and responsive hover. These revisions are implemented with exact live coverage and strict creation ownership. Extra modifier shortcuts, relaxed navigation during creation and a numeric cursor target remain proposals; hover uses settled exact bytes without deferring readout.

- The panel is the design's: rows at the module-panel density, a Masks band, one overlay row, New mask and Add component as kind menus, the Brush section only while a brush is armed or selected, and an accent scope chip on each band bound to the open mask.
- Each component row carries its own `+ − ∩` mode control; the first component's shows `+` alone, dimmed, with the host's reason.
- Tool starts over Off show Tint automatically, including new gradients and brushes; explicit O/Off during the tool is honoured. Existing visible presentations are kept.
- Renames happen in place, from the row's menu. A component rename is a host command, `mask.rename-component`, with `mask.rename`'s rules.
- `mask.list` reports each stroke's settings so a stroke row can say what it painted.
- The luminance range is drawn with a generic `range` control kind above its four fields.
- The Polygon kind, model selections, the inference runtime and Refine edge stay proposals; the panel draws nothing for them.

## Plan refresh

Decided by the owner on 2026-09-28, accepting the recommendations recorded in [product decisions](../tasks/product-decisions.json) when the post-consolidation plans were refreshed after the speed wave.

- **Bayer highlight latitude is retained**, so the Z6 and the Air 2S keep values above sensor white through the demosaic as the X100VI does, provided the rendering change measured on the supplied Z6 and Air 2S files shows no new highlight artefacts such as false-colour clipped highlights. If it does, the Bayer path keeps its clip at sensor white and the [RAW design](design/initial-raw.md#pixel-and-color-contract) documents it as that path's limit. The RAW high-precision qualification carries the change and its measurement.
- **The RAW memory budget has two parts:** the one-development working set, whose investigation target stays 1.5 GiB of process CPU RSS until the whole-editor RAW measurement attributes today's miss, and the retained second development under its own 600 MiB byte budget (`luxforge_raw::RETAINED_DEVELOPMENT_BYTES`). GPU memory is reported separately. Only that measurement's numbers count against the budget; if a measured total is unacceptable, the choice is between a higher target, a smaller slot and a compare that shows only the proxy.
- **`luxforge-json` takes the desktop's `--developer` flag**, with the same meaning, and refuses `--proof-endpoint` without it, as the desktop does. It is not turned on automatically in debug builds, so an agent's `schema.list` never depends on the build profile and a test that needs a test module says so.
- **GPU residency beyond the photo-texture ceiling stays a standalone follow-up** outside the programme waves, measured beside the whole-editor RAW measurement before any total-memory claim. **The 1088 MiB photo-texture ceiling is one budget shared by every photo surface**, so a second surface draws from it rather than doubling it. Neither implies a total editor or GPU memory guarantee.
- **RAW priorities are narrowed** to what the recorded product decisions leave open: which recording modes, firmware and controlled quality scenes come next, and whether any camera-JPEG or film matching goes beyond the neutral development.

## Popular cameras and RawSpeed

Decided by the owner on 2026-09-30, after the [popularity study](research/popular-cameras.md) and the [RawSpeed evaluation](research/rawspeed-evaluation.md).

- **Camera support is weighed by the cameras photographers use,** not the owner's own bodies. The default and common recording modes of the most-used cameras are admitted first ([popular camera support](design/popular-camera-support.md)).
- **Nikon High Efficiency (HE and HE★) NEF is not supported** until upstream LibRaw decodes it. It is refused explicitly, before unpack, for every Nikon model, never decoded silently or as garbage.
- **RawSpeed fills the sensor mosaic for the modes where it is exact and faster,** behind LibRaw's identify, metadata and post-unpack steps, through the adapter's own decode rather than LibRaw's RawSpeed hook ([RawSpeed unpacking](design/rawspeed-unpack.md)). LibRaw is not replaced.

## Detail

Selected on 2026-09-30 under the owner's explicit delegation to plan the module, make sensible choices and record their rationale without owner review. These are design decisions, not implementation authorization or qualification results; [design](design/detail.md), [plan](../tasks/detail.json).

- Detail performs manual noise reduction followed by capture sharpening in a new `restoration` placement stage, after prepared source/pixel edits and before Basic, mixer and Presence. It reuses the host's spatial primitive. This avoids amplifying noise before denoising and keeps tone changes from changing the denoiser's input; Basic remains one composite. RAW sensor white balance and required source corrections stay upstream; JPEG relative white balance remains in Basic.
- Use bounded three-level wavelet shrinkage and thresholded, edge-gated unsharp masking, with numerical mappings frozen against independent references before production. Zero strength defaults on RAW and JPEG preserve Original and avoid automatic double sharpening of camera JPEGs. No AI, camera noise profiles or new module capabilities.
- Show Detail at Fit. Motion uses explicitly approximate, source-scale-aware proxy processing; quiet/release presents a display-bounded downsample of the exact final render. The existing 100% motion/refinement policy stays. Radius is in full-resolution content pixels; 100% is the inspection view for noise and halos. Extend the shared scale/preview contract rather than silently omitting the effect.
- Reuse mask targets, native field-patch presets, drafts and immutable history. Keep Lightroom Detail imports explicitly unsupported with per-setting reports until a mapping is separately qualified. Pre-tone spatial sampling, including Basic's neutral patch, must run off the catalog owner through bounded workers.
- Capture sharpening is part of the recipe and export evaluates it once. Output sharpening after destination resizing, tuned to medium and size, remains separate future export scope. Photographic quality and native performance remain unmeasured; the plan carries their gates and evidence.

Decided by the owner on 2026-09-30, when the plan was made prescriptive; these are design decisions, not implementation authorization:

- **16-bit hand-off on the JPEG path.** A spatial frame on the byte path that feeds a colour run or another spatial unit holds 16-bit encoded sRGB; 8-bit quantization happens only at a resample, a point replacement or the terminal output. An 8-bit hand-off between Detail and Basic bands under exposure and shadow lifts. This also settles Presence's JPEG spatial precision the same way.
- **A display-bounded restoration-prefix proxy cache.** The motion proxy after the leading restoration layers is kept and reused while only later layers change, so downstream drags do not re-run Detail each frame. It is a cache under [performance rule 14](engineering/performance-rules.md#rules).
- **Value-based mask overlays keep working behind Detail** through a bounded input-grid cache on the overlay worker, rather than the refusal that applies to a spatial prefix today.
- **No pixel work on the catalog owner.** A mutation that must read pixels through a spatial prefix, such as the colour-limited brush's seed, goes owner → point worker → owner with its revision and draft identity checked on return; performance rule 5 gains no exception.

Decided by the owner on 2026-10-01, after heavy sharpening left a long settlement visible only in the status bar and the Performance section: a render expected to take more than a second shows its progress on the photograph itself, as a bar across it. The settlement is not made faster here; the full-resolution restoration frame cache that would avoid it stays not planned ([instant previews](design/instant-preview.md#progress-of-a-long-exact-phase)).

## Lens and perspective planning

Lens and perspective scope and approach are selected for planning under the owner's delegation on 2026-09-30: [design and rationale](design/lens-and-perspective.md#scope-and-decisions). The initial scope is explicit offline Lensfun profile distortion plus manual two-axis perspective, with fixed-canvas coverage and no duplicate embedded DNG correction. These are planning decisions, not implemented or verified behavior; the plan adds no owner-review gate.

Decided by the owner on 2026-09-30, when the plan was made prescriptive:

- **RAW mosaics need no acknowledgement.** A RAW source whose development applies no luminance warp (NEF, RAF, and the Air 2S DNG, whose WarpRectilinear corrects only lateral chromatic aberration) has distortion `known-unapplied`, so a profile applies without an acknowledgement. A JPEG's status stays unknown and needs the explicit assume-uncorrected acknowledgement.
- **Perspective is not presettable**, like crop and transforms; Lightroom's Perspective and Upright settings stay unsupported as a different perspective model.
- **Strong minification is refused.** A combined lens and perspective map whose local minification exceeds 1.8× is refused with its reason, because the bilinear sampler would alias; the limit is documented.

Decided by the owner on 2026-10-01, after the Lens correction panel listed every database lens, most of them incompatible ([design](design/lens-and-perspective.md#decisions)):

- **No list of lenses, and never an incompatible one.** The section shows the applied profile with an option to change it, or the detected profile, or a warning that no lens was found with a search to find it. Lenses are listed only as search results, and only compatible ones.
- **Auto-apply on first open, as an entry.** When a photograph that has never had Lens correction is first opened and a compatible profile is detected, Luxforge commits it as an ordinary, undoable history entry with the module's normal label, right after its Original. In the catalog a photograph is created by a Develop and opened by preparing its original, so this happens when its original's preparation completes while nothing has yet moved its head. Only where in-camera distortion correction is known not to be applied (a RAW or DNG whose optics ledger says distortion is known-unapplied). A reset turns it off for good, because the reset keeps a neutral layer; developing the same file again or reopening the photograph once its head has moved never applies it again. A JPEG whose in-camera correction is unknown is not auto-corrected: the section shows the detected lens with an Apply button, which applies assuming the camera did not correct it and says so. This lives in the core, so API and agent clients get the same behaviour as the desktop.
- **Drone names are curated product names.** A small curated core table maps DJI camera codes to product names, keyed by normalized EXIF make and model, so a fixed-lens drone reads "DJI Air 2S (FC3411)". It is used for display and the issue report, never for matching, and lists only well-known codes.
- **Report a missing lens.** When nothing is compatible, the section warns and offers a button that opens a prefilled GitHub issue asking for support for the lens.

## Tone curve

- The curve editor removes a point on a double-click on that point, beside Delete for the selected point, and keeps its numeric point list closed behind a Points disclosure until the person opens it (owner, 2026-09-30). Both are changes to the shared curve editor, made with the Tone curve module; the other [Tone curve proposals](design/tone-curve.md#proposals-with-recorded-defaults) remain open below.
- A single click on the plot away from every point adds a point, and a double-click on a point removes it; the plot fills the panel's width up to a maximum and is centred beyond it (owner, 2026-10-01). A double-click on empty plot therefore adds one point and never removes it.
- Below black the curve uses a floor-subtracted luminance ratio (owner, 2026-09-30): with `L_floor` the linear output of the curve at encoded zero, `rgb_out = L_floor + rgb·(L_out − L_floor)/L`, which equals Basic's frozen ratio rule whenever the curve keeps black at zero. A lifted black then fades the deepest shadows toward grey instead of turning their noise into coloured speckle. Basic's Blacks keeps its frozen rule; changing it is a separate follow-up.

## Open product questions

Tracked in [product decisions](../tasks/product-decisions.json).

- How should catalog backup, portability, sidecars, folder relinking and external-drive sync work?
- Beyond the supplied files, which RAW recording modes/firmware and controlled quality scenes should be prioritized? The implemented decoder/developer and neutral defaults are explicit; broad visual acceptance, the measured resource target and additional DJI modes/scenes remain in [RAW qualification](design/initial-raw.md#remaining-qualification-and-decisions).
- Masking is authorized (2026-09-23) and is being implemented. Decided the same day: brush strokes are held in a **content-addressed stroke store** keyed by a hash of their contents, because every history entry stores a complete recipe and embedded strokes grow quadratically — about 37.5 MB across history for 200 strokes against 1.08 MB addressed. Entries stay full snapshots and pure deltas are rejected; a missing or corrupt stroke fails explicitly. It lands in phase C before the first brush ships, in the [current catalog format](design/versions-and-lineage.md#storage-catalog-format-12). The `points` kind, the stroke list and the `brush-paint` interaction are host primitives shared with the corrections proposal, not mask-private ones. See [masking](design/masking.md#stroke-storage).
- Do masking's remaining recorded defaults stand — masks as a target for the delivered modules rather than a local-adjustment module of their own, the idempotent component algebra, a radial that selects inside, one stroke amount instead of Flow and Density, and the A-to-D phase order with brushes before range selections?
- Do the [catalog design](design/catalog.md#proposals)'s eighteen recorded defaults stand — browsing files through an automatic event layer and developing only picks into the catalog, no ratings, keywords or flags, the event rules and offline place names, burst and bracket rules (brackets from metadata or previews), the loupe's camera previews with an on-demand development for 100%, moving on after picking a burst frame, cards browsed in place with copied files preferred, sending unedited photographs back, per-workspace undo, per-photograph resolution of missing originals and batch export in the first version, Empty Removed for permission authority only, the design scale, folders replacing import, events becoming catalog folders named at develop time, "On disk" for the filesystem, watching and reconciling the index, and showing every long job with progress — and is its implementation authorized?
- What is the first external module the owner would use, and what enablement and recovery behavior does it need?
- For the proposed [Corrections module](design/corrections.md), should AI Remove enter the accepted scope, and should a changed RAW source-development prefix require regeneration of a saved AI patch? Remote-photo consent is per asset by the [module capabilities](#module-capabilities) default.
- Which measured workloads and responsiveness budgets become acceptance requirements?
- For interactive previews, which additional measured quality levels and approximation error bounds are acceptable beyond the authorized viewport baseline? Temporary softness, the updating histogram, and viewport-bounded clipping are accepted above.
- Which of the [presets defaults](design/presets.md#decisions-taken-on-defaults) stand? RAW white balance import converts values without calibration ([source-kind controls](#source-kind-controls)).
- Which of the [Tone curve proposals](design/tone-curve.md#proposals-with-recorded-defaults) stand — a luminance composite with the luminance-ratio reconstruction rather than Lightroom's per-channel composite, one channel, order 5 after Basic and before the mixer, free endpoints, the unit-slope tail past white, sixteen points, the Lightroom `ToneCurvePV2012` transfer including an identity curve, a double-click add that snaps to the drawn curve, end points the desktop does not remove, and the delivered point rows? The module is implemented on these defaults.
- Which of the [Presence, colour mixer and vignette proposals](design/presence-mixer-vignette.md#proposals-with-recorded-defaults) (section names, stage order, mixer layout, vignette style, spatial gesture latency, sample cost) stand? Implementation was authorized on 2026-09-22 on the recorded defaults and is delivered; the owner refines the defaults after review, including whether spatial sliders should draft at a bounded resolution now that the measured misses are recorded.

The Basic and histogram product choices were decided on 2026-09-21 and implementation was authorized the same day; see [Basic adjustments and histogram](#basic-adjustments-and-histogram).
