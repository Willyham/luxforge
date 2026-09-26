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
- History is a graph: entries keep their undo parent and nothing is truncated. A named **version** (the owner's name for the Lightroom-style saved state) is a reference to one retained entry, not a branch. The catalog uses internal format 10; unsupported formats are refused. See [versions and lineage](design/versions-and-lineage.md).
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
- A history row shows the action title plus a one-value summary supplied by the module through a declared `summary` template; the host stores the rendered label with the entry.
- Test modules (pixel proof) live in a Developer section that is hidden unless the desktop is launched with `--developer`; the registry marks them `developer: true` and their API is unaffected.
- Compare is hold-`\` for the Original entry, through `preview.select` and `preview.return-current`. Dark theme only; a light theme is not planned.
- Basic, histogram, export, Locate, heal and mask are outside this work. Their sections, buttons and notices are left out of the build entirely rather than drawn as placeholders. The generated tools panel must accept a `number` slider module without desktop changes, which is how Basic lands later.

Decided on 2026-09-21:

- A picking mode belongs beside the controls it fills, not in the hovering mode strip: a module that declares a `point-pick` or `sample-apply` canvas declares a `picker` control in its own panel (Basic's Neutral picker in the White balance group, RAW's Neutral WB in the RAW group), and the strip holds the pointer, canvas-takeover modes (crop) and view overlays only. Declared letters and the command palette still enter every mode.

Decided on 2026-09-23:

- The state panel's [Performance section](design/performance-panel.md#decisions) starts open on every launch. Memory is shown in binary units with Activity Monitor's MB and GB labels, and CPU as a percentage of one core, so it passes 100% whenever more than one core is busy.
- A module whose controls are a single group shows them without a sub-group header: a header naming the module's only group, such as RAW's "RAW development" or Transforms' "Exact transforms", repeats the band above it. The band keeps the module's reset. Descriptors and the API are unchanged.

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
- Only the desktop (after Allow) or `luxforge-json --permission-authority` may grant. Live-session clients cannot; anyone may deny or revoke. Revocation cancels dependent jobs and never touches recipes, history or accepted artifacts; an endpoint or path change revokes the old grants.
- Remote endpoints require HTTPS and public addresses; plain HTTP is allowed only to loopback, labelled as such. No proxies.
- No remote provider adapter ships with the framework; the first real adapters arrive with Corrections. `managed-storage` and `local-runtime` wait for their first consumer.
- Derived artifacts live in a directory beside the catalog and move with it; catalog format 10 holds their references beside the preset library, the mask table and the stroke store, and earlier formats are refused.

Revised by the owner on 2026-09-24, after the [architecture review](#architecture-review):

- The framework stays and is trimmed to what its consumers need: bound artifacts travel on the recipe as strokes do, `artifact.relocate` goes because the directory moves with its catalog, a grant no longer records its last use, settings and grants share one document store, settings use the module parameter vocabulary, capability jobs report through the activity board, and the proof endpoint leaves the shipped core crate.
- The transport uses a well-known, tested HTTP client, pinned (such as `ureq`), behind the existing address policy, instead of the hand-written HTTP/1.1 client. The policy itself is unchanged.
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

The owner asked on 2026-09-23 for presets, with native presets and Lightroom import through a presets module whose apply is a history entry, and for the work to proceed without blocking on questions. It is delivered on the defaults recorded in the [presets design](design/presets.md#decisions-taken-on-defaults), each a proposal the owner reviews: presets as catalog data (format 7), only field-patch actions presettable, `apply-preset` carrying its settings, Lightroom values transferred for the controls Luxforge has and never clamped with RAW Kelvin and tint refused, the section first in the tools panel, and white balance unchecked when creating a preset.

## Rendering memory

- A shared working-memory budget is a target that keeps memory low, not a limit that refuses the user's work (owner, 2026-09-23). Work that needs more than the target has left still runs and completes. The 256 MiB spatial budget lowers how many tiles run at once, down to one. The 64 MiB colour scratch budget's row chunks are sized so the pool's workers stay well inside it, and a chunk past it still runs. Both keep a high-water mark that the timing tier reads against the target. Size limits on what is accepted — source and frame sizes, the halo and unit bounds a module declares — still refuse with `resource-limit`.
- When the spatial target holds a render's batch to fewer tiles than the pool has workers, each tile's own passes run on the pool rather than the target being raised (owner, 2026-09-23): the same bytes and the same memory, all three Presence fields at 60 MP in about 3.9 s instead of 11 s, for about twice the CPU time. Larger tiles for a large summed halo remain a proposal.

## Architecture review

Decided by the owner on 2026-09-24 after a whole-codebase review of `main` at `7ce9557`. The owner decided the first two and asked for the review's recommendation on the rest. The work is planned in [consolidation](design/consolidation.md), and each spec changes when its behaviour does.

- Consolidate rather than rewrite. Each cross-cutting mechanism keeps one implementation that every feature extends, and the copies are deleted. That covers committing and planning an edit, method dispatch and parameters, jobs, latest-job workers, desktop drafts, the JPEG and RAW evaluators, field-patch modules, colour math, smoke scenarios and test support.
- **Controls are the same for every source kind.** A JPEG and a RAW photo show one Exposure control and one White balance (temperature and tint) control set, as Lightroom does. Each control behaves as its source requires: on a RAW photo it sets the source development's white balance and exposure, and on a JPEG it sets Basic's relative adjustment. The RAW section's duplicate Exposure and white-balance controls merge into that one set. A module's applicability to a source kind is declared, not named by the desktop.
- **Mask coverage stays bit-identical** to its frozen `f64` reference. The mask gesture's extra runtime hop is fixed first. Faster `f32` coverage within the `1e-6` tolerance is considered only if a measurement then shows coverage limiting a paint gesture.
- The brush's cap of 64 segments per grid cell is measured on a realistic back-and-forth scrub before it changes. After that, either the cap rises with its recorded per-pixel cost, or a stroke that reaches it starts a new brush component.
- The crop draft moves onto the core `draft.*` lifecycle once the desktop has one draft driver, so agents see it in `session.state`. The crop geometry and canvas stay as they are. This supersedes "the crop draft stays desktop-local".
- Every mutating method carries `{request_id, actor}` and is deduplicated, with `expected_revision` wherever a revision exists, so an agent can retry any mutation safely. This supersedes the presets default that library methods take no mutation envelope.
- A mask's coverage grid is delivered with the proxy phase rather than after the exact render, because it reads no pixel of the exact frame.
- Mask, component and stroke identities become a declared parameter kind, validated and deduplicated like any other parameter.
- The path primitives stay host primitives, and the recipe stops scanning every layer payload for strokes until a consumer other than masks exists.
- The developer component gallery's page is desktop view state and leaves the core session schema.
- The curve control vocabulary stays, although only the developer controls proof uses it; Tone Curve is its planned consumer.
- `probes/s0` and `cargo xtask probe` are removed; the Iced selection stays recorded in the research docs.
- The Develop workspace mockup images stay in git without LFS and are regenerated only when the design changes, because every agent worktree would otherwise need LFS.
- Fusing Basic's colour units (exposure into the white-balance matrix, and one Oklab pass shared by colour and the mixer) waits for a measurement showing that the pointwise pass limits a gesture.

## RAW white-balance drafts

- The `raw-panel` scenario checks a drafted RAW white balance relative to the drag (owner, 2026-09-26): at Fit and at 100% the released exact frame is within a tenth of the drag's own change from the approximate frame, and at Fit within one code of it on average. The Air 2S's full-size error at a strong gain change is above a code, which the design's accuracy table already records; see [instant previews](design/instant-preview.md#a-raw-white-balance-during-a-drag).

## Open product questions

Tracked in [product decisions](../tasks/product-decisions.json).

- How should catalog backup, portability, sidecars, folder relinking and external-drive sync work?
- Beyond the supplied files, which RAW recording modes/firmware and controlled quality scenes should be prioritized? The implemented decoder/developer and neutral defaults are explicit; broad visual acceptance, the measured resource target and additional DJI modes/scenes remain in [RAW qualification](design/initial-raw.md#remaining-qualification-and-decisions).
- Masking is authorized (2026-09-23) and is being implemented. Decided the same day: brush strokes are held in a **content-addressed stroke store** keyed by a hash of their contents, because every history entry stores a complete recipe and embedded strokes grow quadratically — about 37.5 MB across history for 200 strokes against 1.08 MB addressed. Entries stay full snapshots and pure deltas are rejected; a missing or corrupt stroke fails explicitly. It lands in phase C before the first brush ships, in catalog format 7. The `points` kind, the stroke list and the `brush-paint` interaction are host primitives shared with the corrections proposal, not mask-private ones. See [masking](design/masking.md#stroke-storage).
- Do masking's remaining recorded defaults stand — masks as a target for the delivered modules rather than a local-adjustment module of their own, the idempotent component algebra, a radial that selects inside, one stroke amount instead of Flow and Density, and the A-to-D phase order with brushes before range selections?
- What is the first external module the owner would use, and what enablement and recovery behavior does it need?
- For the proposed [Corrections module](design/corrections.md), should AI Remove enter the accepted scope, and should a changed RAW source-development prefix require regeneration of a saved AI patch? Remote-photo consent is per asset by the [module capabilities](#module-capabilities) default.
- Which measured workloads and responsiveness budgets become acceptance requirements?
- Which of the [presets defaults](design/presets.md#decisions-taken-on-defaults) stand, and should RAW white balance import get a calibrated conversion?
- Which of the [Presence, colour mixer and vignette proposals](design/presence-mixer-vignette.md#proposals-with-recorded-defaults) (section names, stage order, mixer layout, vignette style, JPEG spatial precision, spatial gesture latency, sample cost) stand? Implementation was authorized on 2026-09-22 on the recorded defaults and is delivered; the owner refines the defaults after review, including whether spatial sliders should draft at a bounded resolution now that the measured misses are recorded.

The Basic and histogram product choices were decided on 2026-09-21 and implementation was authorized the same day; see [Basic adjustments and histogram](#basic-adjustments-and-histogram).
