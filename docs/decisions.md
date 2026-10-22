# Product decisions

Accepted owner decisions and the questions still open. Proposals stay proposals until the owner decides; record each answer here and in the affected spec.

## Window visibility and job monitoring

For the planned [visibility and monitoring](design/visibility-and-monitoring.md) work:

- Scope is visibility and event monitoring only; proxy CPU work, rendering, preview scheduling and CPU-pool scheduling are excluded.
- Pause presentation sampling only when the window is minimized or explicitly hidden. A visible window keeps sampling when unfocused or fully covered by other windows.
- Background jobs and required result handling continue; pausing presentation does not cancel work.

## Product and platform

- A fast, non-destructive desktop editor for professional and prosumer collections on macOS, Windows and Linux.
- The owner's M4 MacBook Pro (M4 Pro, 14 CPU and 20 GPU cores, 48 GB unified memory, macOS 26.5.2) is the first target and the reference machine.
- Engineering baselines: macOS 14+ arm64, Windows 11 24H2+ x64, Ubuntu 24.04 x64 with GNOME Wayland and X11. These are intended targets, not verified support; see [platforms](engineering/platforms.md).
- Development packages stay unsigned. Tag-build candidates have optional explicit signing/notarization; stores and a public release remain absent. [Automatic updates](#automatic-updates) ship with the first release.
- Project code is GPL-3.0-or-later. Dependencies and extensions should be open source and license-compatible; no proprietary hosted service is a development prerequisite. The manual license, native and asset review is deferred and is not a passing result.
- Everything is v0 and breaking changes are expected. Only current catalog, recipe, API and module shapes are supported until 1.0.0, when catalogs gain guaranteed migrations ([catalog compatibility](#catalog-compatibility)); no migrations, compatibility shims, old-version fixtures or historical parity requirements. Unsupported data is refused without rewriting it. No public compatibility framework, cloud or accounts, marketplace or generalized processing graph.
- Initial RAW targets are the original Nikon Z6 and the Fujifilm X100VI, with the supplied DJI Air 2S DNG for qualification. Benchmark established decoders before proposing a custom one.
- RAW editing stays continuous and non-destructive, in the same workflow sense as Lightroom: the original remains the source, adjustments remain recipe data and later edits do not operate on a JPEG baked from earlier WB/exposure settings. Keep high precision through editing and convert for display or explicit export. The development itself is neutral and selects no Adobe matching; new RAW photos start from a Luxforge look on top of it, with camera-preview matching as a setting ([RAW looks](#raw-looks)). See the [initial RAW design](design/initial-raw.md).
- Lightroom Library and Develop are familiarity references; their sliders' responses are followed ([Lightroom alignment](#lightroom-alignment)). Map, Book, Slideshow, Print, Web and Publish Services are out of scope.
- All development tooling is Rust (`cargo xtask`); no second toolchain.

## Initial release preparation

The [design](design/release-v0.1.md), [task plan](../tasks/project/release-v0-1.json) and [release notes draft](releases/v0.1.md) prepare a reviewable v0.1 result; local build rehearsals do not establish a qualified public release, and no release has been published. macOS and Linux release together: Apple silicon macOS plus the existing x86-64 Ubuntu 24.04 baseline. Both platform qualification gates precede a qualified joint release; automated pre-release builds report those gates as open. The owner's paid Apple Developer Program membership is active. The owner reports completing signing setup with the local notarization Keychain profile `luxforge-notarization`; identity/profile validation remains part of the final candidate gate; preparation does not authorize creating credentials or accepting further agreements. Platform-independent identity, complete source/notices and distribution advisory review can be prepared independently. A release-tag push publishes a GitHub pre-release automatically; current-format-only data rules continue.

Release tags trigger binary builds. Short `v0.1` normalizes to `0.1.0`; release tags supply the compiled application version without editing the workspace version/lockfile. The [tag workflow](design/tag-release-builds.md) builds macOS and Linux, optionally signs/notarizes macOS when hosted credentials are configured, and automatically publishes a GitHub pre-release after both builds and all asset uploads succeed. Assets are staged in a temporary draft before publication; a failed upload leaves it unpublished. Publication does not establish a release-ready verdict; source/policy/native/install gates remain open. `cargo xtask release TAG` requires clean synchronized main, runs quick checks, rechecks state and pushes one annotated tag; `--dry-run` checks Git state without mutation. Implementation work never invokes a real release or exports or uploads private credentials.

## Automatic updates

Luxforge updates itself from the first release, so nobody is stranded on v0.1: no launcher application, with an offer to move the app into Applications on macOS; Sparkle 2 on macOS; an in-house updater on Linux that replaces the user-level payload, falling back to offering a download when it cannot; Windows later on the same feed, key and commands; feeds served by the one backend API under `https://api.luxforge.app/v1/updates/`; and a release build that checks automatically without asking, offers what it finds, and installs automatically only when the person turns that on. The macOS download is a drag-to-Applications disk image. The [design](design/auto-update.md) and [task plan](../tasks/project/auto-update.json) describe the work.

## Catalog compatibility

The catalog format is numbered from 1. Until 1.0.0, catalog format changes need no migration and earlier formats are refused unchanged. From 1.0.0 onwards, every catalog format change ships with an automatic migration from each released format. Before then an update that changes the catalog format is flagged in its offer and never installed automatically, and the desktop launches past a catalog it cannot open with an offer to start a new one ([automatic updates](design/auto-update.md#launching-past-an-unsupported-catalog)).

## Editing and storage

- Originals are read-only. Import references existing files with a stable asset ID, a verified content fingerprint and a changeable locator. SQLite is the local catalog. Folder relinking, sidecars, portability, backups and sync need their own workflow decisions.
- A "layer" is an ordered edit operation in a recipe. Each committed action stores a complete immutable recipe snapshot and one attributed history entry. Bitmap compositing, blend modes and arbitrary layer reordering are not selected.
- History is a graph: entries keep their undo parent and nothing is truncated. **Auto collapse history** is a preference, on by default: an edit that sets the same control as the entry just committed hides that entry, and one that returns the control to where the run began moves back to the run's base without writing an entry. Collapsing hides entries and never deletes them; they stay in the catalog and reachable by id ([auto-collapse](design/versions-and-lineage.md#auto-collapse)). A named **version** (the owner's name for the Lightroom-style saved state) is a reference to one retained entry, not a branch. The catalog uses its [current internal format](design/versions-and-lineage.md#storage-the-catalog-format); unsupported formats are refused.
- Undo and redo navigate saved entries without appending rows. Preview is read-only. Restore appends an action and keeps all later entries. A new edit clears shortcut redo, but every entry stays available. Committed state survives restart; drafts do not.
- Develop: centered photo, collapsible controls, visible history, Fit, numeric zoom and true 100%. Cmd/Ctrl+O imports; Cmd/Ctrl+Z and Shift+Cmd/Ctrl+Z navigate history.
- Geometry: the visible composition travels with mirror and quarter-turns, and a locked ratio swaps orientation on a quarter-turn. Fine angle is limited to ±45°. Space-drag pans.
- Pixel-stage edits address the content stage (the source after EXIF orientation) and are placed before quarter-turns, reflections and the crop, so changing the crop never moves or invalidates them. The host chooses a new layer's position from its effect stage. See [content-space edits](design/content-space-edits.md).
- Quarter-turns and reflections are one orientation layer holding the composed exact state. It sits ahead of the crop and any finish layer, is updated in place, and carries a later crop through the transform; four rotations leave one neutral layer. See [orientation layer](design/orientation-layer.md).
- Selecting the current entry in history is Return to current, not a historical preview.
- Crop: free handles, composition move, thirds overlay, Free/Original/1:1/3:2/4:3/16:9/custom ratios, a drag-to-straighten guide, Apply, Cancel and reset. Option/Alt scales proportionally about a fixed center. Straightening preserves composition with only the trimming needed. Apply commits once; Cancel discards.
- Export never overwrites an existing file or a source alias. Its formats, quality, metadata levels and destination are [export settings](design/export-settings.md), held in presets and the remembered last export.
- Recovery: verified manual Locate keeps asset identity, layers and history and rejects changed or ambiguous sources.
- Live agents: human and agent actions share one history. An external commit preserves a human draft and marks it conflicted, resolved by explicit Discard or Reapply. Live JSON/IPC serves the same operations; MCP is a later adapter over them.

## Develop workspace

For the [Develop workspace](design/develop-workspace.md) shell:

- State panel on the left (versions, history, recipe), tools panel on the right, both collapsible independently.
- Modules render as stacked collapsible sections in registry order. A build lists only registered modules; nothing is drawn for modules that do not exist.
- A history row shows the action title plus a one-value summary supplied by the module through `label()`; the host stores the rendered label with the entry.
- Test modules (pixel proof) live in a Developer section that is hidden unless the desktop is launched with `--developer`; the registry marks them `developer: true`, and they register only in developer mode, so a normal build's API lists none of their methods.
- Compare is hold-`\` for the Original entry framed by the displayed entry's geometry (`preview.select` with `keep_geometry`), and Shift+`\` for the uncropped Original, released through `preview.return-current` or the previous selection. A tap of `\` (release within 200 ms) or a click of Compare toggles an aligned Before/After divider, with another tap of `\` or Escape to exit; `preview.compare` owns its fixed After entry, prior selection and divider position. Holding `\` during the slider temporarily shows Before across the photograph.
- A feature that is not built draws nothing: no placeholder sections, buttons or notices. The generated tools panel accepts a `number` slider module without desktop changes.
- A picking mode belongs beside the controls it fills, not in the hovering mode strip: a module that declares a `point-pick` or `sample-apply` canvas declares a `picker` control in its own panel (Basic's Neutral picker in the White balance group, RAW's Neutral WB in the RAW group), and the strip holds the pointer, canvas-takeover modes (crop) and view overlays only. Declared letters and the command palette still enter every mode.
- The state panel's [Performance section](design/performance-panel.md#decisions) starts open on first use and then remembers whether it was left open. Memory is shown in binary units with Activity Monitor's MB and GB labels, and CPU as a percentage of one core, so it passes 100% whenever more than one core is busy.
- A module whose controls are a single group shows them without a sub-group header: a header naming the module's only group, such as Presence's "Presence" or Vignette's "Vignette", repeats the band above it. The band keeps the module's reset. Descriptors and the API are unaffected.
- The histogram has no caption at all, neither a row under the plot nor a hover tooltip. Nothing is read under the pointer: a status-bar readout issuing one `render.sample` per pointer move cost the point worker minutes through Detail, Presence and masks and froze the window. `render.sample` stays the API's exact point query.
- The status bar leads with a plain sentence of what last happened. Entry, snapshot and source identifiers are not shown or copied there; they remain available through the API.
- On macOS the app's title bar is the window's title bar: a transparent, full-size-content native bar with the traffic lights inside the app's own. Windows and Linux keep their native frames.
- The state panel has no Recipe section. The layer stack stays readable through `recipe.describe`.

## Basic adjustments and histogram

For the [Basic and histogram design](design/basic-and-histogram.md), which is delivered:

- On JPEG, the Basic controls and the histogram work on the supported SDR sRGB and greyscale subset. RAW has its own input and colour contracts.
- One Basic layer per recipe with a fixed internal group order (White balance, Exposure, tone curve, Vibrance, Saturation), placed by its colour stage before the geometry tail and updated in place at the same identity.
- Slider release, key-up or Enter commits one action; Escape cancels; focus loss cancels an unfinished gesture. There is no Apply panel for adjustments.
- Global pointwise tone first: Contrast, Highlights, Shadows, Whites and Blacks are one monotone global curve before any edge-aware processing is considered.
- Numerical ranges and equations start from the Lightroom research and the design's proposed ranges and defaults; the numerical tasks select and freeze them against independent references before a control ships, and no value is claimed as Lightroom-equivalent.

The defaults below are provisional and recorded in the design for the owner to review and refine, so a later change is a normal edit, not a silent reinterpretation. Measured results against the provisional thresholds are in [performance](specs/performance.md).

- Performance targets are provisional thresholds: settled exact histogram p95 below 200 ms and a 64 MiB aggregate scratch target. The slider-to-presented-frame target is **16 ms p95 with an acceptable bound of 32 ms** (one and two frames at 60 Hz), so a figure below 16 ms passes, one below 32 ms is acceptable and one at or above 32 ms is a miss. A measured miss is reported with its figures and does not block delivery.
- Global tone stays global. If the tone study finds a visual case a global curve cannot pass, the control ships with that limitation documented and an edge-aware proposal recorded as later work.
- Clipping overlays: any channel at an endpoint counts; shadow clipping draws blue, highlight red, both magenta; tooltips state the rule.
- Neutral picker: a 5 × 5 patch at input-stage pixel centres clipped at the image edges, evaluated before the Basic layer; near-black, clipped and non-invertible samples are rejected with a reason.
- One Basic layer per recipe; more than one is never user-facing, and an imported stack with several reports ambiguity.
- Float exactness: results match an f64 stepwise reference within `1e-6 + 1e-6 × |reference|` and at most one output code of rounding where the design permits it.

## UI components

For the [UI components design](design/ui-components.md), the closed control vocabulary modules may declare (implemented; see [feature status](features.md)):

- No scroll-wheel editing of controls in v0, and no preference to enable it: a trackpad scroll over a panel of sliders must never edit a photograph.
- `text` is the second slice and waits for its first consumer; `curve` channels ship with the curve kind.
- Icon buttons are a declared style, drawn as canvas vector paths rather than Unicode glyphs.
- Curve interpolation belongs to the module and is sampled through a declared query; the host owns no curve spline and the widget never interpolates.
- Option steps by one tenth of the declared step alongside Shift at ten times; the modifier can change later without a descriptor change.

## Module capabilities

For the [shared module capabilities](design/module-capabilities.md), each a default the owner can change:

- Settings are user-level only: module settings and named provider profiles, outside every catalog, with no history entries. Secrets live only in the OS credential store, starting with the macOS Keychain; a locked or unsupported store fails explicitly with no plaintext fallback.
- Consent to send photo data to a provider is remembered per provider profile and data class until revoked; the capability contract takes this shape with AI editing's remote tier, and until then a grant names the module, profile, adapter, endpoint origin, data class and asset, and is not remembered for later photos. Downloads are granted per resource version and origin.
- Only the desktop (after Allow) or `luxforge-json --permission-authority` may grant. Live-session clients cannot; anyone may deny or revoke. Revocation cancels dependent jobs and never touches recipes, history or accepted artifacts; an endpoint change revokes the grants made for the earlier endpoint.
- Remote endpoints require HTTPS and public addresses; plain HTTP is allowed only to loopback, labelled as such. No proxies.
- No remote provider adapter ships with the framework; the first real adapters arrive with [AI editing](design/ai-editing.md). `managed-storage` waits for its first consumer; `local-runtime` is defined by AI editing.
- Derived artifacts live in a directory beside the catalog and move with it; the [current catalog format](design/versions-and-lineage.md#storage-the-catalog-format) holds their references beside the preset library, the mask table and the stroke store, and earlier formats are refused.
- The framework is trimmed to what its consumers need: bound artifacts travel on the recipe as strokes do; there is no `artifact.relocate`, because the directory moves with its catalog; a grant records no last use; settings and grants share one document store; settings use the module parameter vocabulary; capability jobs report through the activity board; and the proof endpoint is outside the shipped core crate.
- The transport is a well-known, tested HTTP client, pinned: `ureq`'s own agent, behind the address policy ([post-consolidation review](#post-consolidation-review)).
- There is no `read-user-file` capability or `file` setting kind until a module needs them: `preset.import` takes the file's text from its client. A module that reads a user-chosen file may add them, consented per canonical path.

## Programmable operations and modules

Every operation is programmable, including future tools, masks, clone strokes, settings and module lifecycle. The core owns recipe transactions, history, invariants and bounded services; tool modules own parameters, validation, controls and algorithms through those APIs. Modules are linked, with cheap registration and lazy resources. Real external loading is required later, and a missing or disabled provider must never silently erase edits or produce an incomplete export. Still open: the first external use case, package and runtime format, trust and UI contribution. See [modules](design/modules-and-api.md).

## Owner workflow priorities

The owner edits local files and syncs them to an external drive, so moved-original recovery matters. These are priorities and problem statements, not authorization to implement every solution.

| Priority | Direction |
| --- | --- |
| Speed and unused-tool bloat | Measure startup, loading, idle and first-use costs; keep the core small and initialize lazily |
| Filtering, tagging and collections | Design one retrieval model from concrete workflows |
| Full catalog with lazy shoot subsets | Keep catalog scale independent of decoded pixels; scope views to the working set |
| Multi-selection and stacking | Multi-selection and result-led HDR merge stacks are delivered ([design](design/hdr-merge.md)) |
| Bracket and panorama identification | Brackets and panoramas are detected in Select, panoramas from a folder named for them (DJI's `PANO0001`) or from previews that pan, and a RAW file with its camera JPEG is one photograph ([grouping](design/catalog.md#grouping-inside-a-view)); [HDR](#hdr-exposure-merges) and [panorama](#panorama-merges) merges are offered from a moment's header or a selection |
| Confusing export controls | One clear JPEG export path with explicit metadata behavior |

## Presets

Presets are native, with Lightroom import, through a presets module whose apply is a history entry. They are delivered on the defaults recorded in the [presets design](design/presets.md#decisions-taken-on-defaults), each a proposal the owner reviews: presets as catalog data (see [versions and lineage](design/versions-and-lineage.md#storage-the-catalog-format)), only field-patch actions presettable, `apply-settings` carrying its settings, Lightroom values transferred for the controls Luxforge has and never clamped with RAW Kelvin and tint refused, the section first in the tools panel, and white balance unchecked when creating a preset.

## Rendering memory

- A shared working-memory budget is a target that keeps memory low, not a limit that refuses the user's work. Work that needs more than the target has left still runs and completes. The 256 MiB spatial budget lowers how many tiles run at once, down to one. The 64 MiB colour scratch budget's row chunks are sized so the pool's workers stay well inside it, and a chunk past it still runs. Both keep a high-water mark that the timing tier reads against the target. Size limits on what is accepted — source and frame sizes, the halo and unit bounds a module declares — still refuse with `resource-limit`.
- When the spatial target holds a render's batch to fewer tiles than the pool has workers, each tile's own passes run on the pool rather than the target being raised: the same bytes and the same memory, all three Presence fields at 60 MP in about 3.9 s rather than 11 s, for about twice the CPU time. An operation with a large summed halo runs in larger tiles ([post-consolidation review](#post-consolidation-review)).
- Editing responsiveness comes first, and caching is an expected way to get it. When a measured interaction repeats work whose inputs have not changed, a bounded, disposable cache is weighed by the memory it holds against the time it saves, not ruled out; no rule forbids caches as a class. The conditions a cache meets are [performance rule 14](engineering/performance-rules.md#rules). A cache whose budget adds to the RAW working set or another memory target still needs the owner's acceptance or delegation. The candidate caches are proposals in [instant previews](design/instant-preview.md#proposals-and-later-work).
- An export of a 60 MP photograph may take more than 1 GiB of process memory. The 1 GiB peak remains the target for importing one; an export's peak is measured and reported ([performance](specs/performance.md#export-settings)), not held to it.

## Architecture review

From a whole-codebase review. The consolidation is delivered and its outcome lives in the specs it changed.

- Consolidate rather than rewrite. Each cross-cutting mechanism has one implementation that every feature extends, with no copies. That covers committing and planning an edit, method dispatch and parameters, jobs, latest-job workers, desktop drafts, the JPEG and RAW evaluators, field-patch modules, colour math, smoke scenarios and test support.
- **Controls are the same for every source kind.** A JPEG and a RAW photo show one Exposure control and one White balance (temperature and tint) control set, as Lightroom does. Each control behaves as its source requires: on a RAW photo it sets the source development's white balance and exposure, and on a JPEG it sets Basic's relative adjustment. A module's applicability to a source kind is declared, not named by the desktop.
- **Mask coverage stays bit-identical** to its frozen `f64` reference. Faster `f32` coverage within the `1e-6` tolerance is considered only if a measurement shows coverage limiting a paint gesture.
- The crop draft moves onto the core `draft.*` lifecycle once the desktop has one draft driver, so agents see it in `session.state`. The crop geometry and canvas stay as they are.
- Every mutating method carries `{request_id, actor}` and is deduplicated, with `expected_revision` wherever a revision exists, so an agent can retry any mutation safely, library methods included.
- A mask's coverage grid never waits for the exact render, because it reads no pixel of the exact frame: the live overlay is filled by its own coverage worker from the preview's evaluation.
- Mask, component and stroke identities are a declared parameter kind, validated and deduplicated like any other parameter.
- The path primitives stay host primitives, and the recipe does not scan every layer payload for strokes until a consumer other than masks exists.
- The developer component gallery's page is desktop view state, outside the core session schema.
- The curve control vocabulary stays; the Tone curve is its product consumer, beside the developer controls proof.
- The Develop workspace mockup images stay in git without LFS and are regenerated only when the design changes, because every agent worktree would otherwise need LFS.
- Fusing Basic's colour units (exposure into the white-balance matrix, and one Oklab pass shared by colour and the mixer) waits for a measurement showing that the pointwise pass limits a gesture.

## RAW white-balance drafts

- A RAW white-balance drag is drawn on the GPU, applying `W = R · diag(g'/g) · R⁻¹` to the planes developed at the committed balance, and the release redevelops the mosaic exactly. The `raw-panel` scenario reports the moving frame against the release's picture at rest and fails only on a gross error, a CPU job per tick or a frame the GPU did not draw ([GPU-first rendering](#gpu-first-rendering)).
- The scenario records each scene's clip share as context: the share of the default crop's Bayer sites at 0.99 of sensor white or above under the channel-wise largest gains its drags develop at. The demosaic clamps each gained site at sensor white, and the first-order `diag(g'/g)` cannot follow that clamp, so highlight-clipped Bayer scenes (a Bayer development with no DNG correction after the demosaic, clipping 1% or more) depart furthest from their release. The measurements are in [instant previews](design/instant-preview.md#popular-cameras). A clip-aware draft that reverses the clamp per pixel in camera RGB is deferred, to be measured and reconsidered later ([roadmap](plan.md)).
- Every white-balance accuracy figure averages over the photograph alone: the canvas beside a 3:2 or 4:3 photo at Fit never changes and dilutes a Fit mean by 15–24%.

## Post-consolidation review

From a second whole-codebase review; each spec describes the resulting behaviour.

- **Source kinds are declared.** `EffectDescriptor.sources` and one applicability rule decide where a module applies, and nothing checks the RAW module's name; the open questions in [source controls](design/source-controls.md) are untouched by it.
- **The transport runs on `ureq`'s own agent** behind the address policy; cancelling a request shuts its socket down, which works through rustls.
- **A brush stroke that would pass the occupancy cap is refused when it is painted**, not when a layer first draws the mask. The cap stays at 64 segments per grid cell. A measurement on a realistic back-and-forth scrub sizes a decimation tolerance relative to the brush radius, which lowers occupancy without changing the coverage of any stored stroke. A stroke that reaches the cap does not start a new component, because components combine by maximum and painting would stop building up.
- **Undo, Redo and Restore are refused while a draft is open**, with the reason in the status bar, as every other discrete commit is.
- **A panic while the catalog owner serves a message is contained** and answered as `internal`, and the owner keeps serving other clients. Every worker contains its panics the same way.
- **`verify` reports a tier with a skipped or unrun component as incomplete**, with a distinct non-zero exit that names what was skipped.
- **The module capabilities framework stays, trimmed.** Activation waits, with `local-runtime`, until a module needs it. The desktop surface is the consent notice, the task control, the resource row and a settings form built from the tool controls. A resource installs from its pinned URL only. At the 1,024-record cap the oldest remote-image grant is evicted rather than a new grant being refused; asking again only reduces privilege.
- **A drag the GPU draws queues no exact phase**, and its histogram and clipping counts are the displayed frame's, marked updating ([GPU-first rendering](#gpu-first-rendering)).
- **A crop draft at Fit shows its input stage at display resolution** and opens before the stage's pixels arrive: the GPU's picture at rest of the layer prefix, or the reference's exact stage reduced to the view where the GPU cannot draw it ([instant previews](design/instant-preview.md#a-crop-drafts-input-stage)).
- **The desktop derives every region on every message, with no per-region derivation keys.** The derivation costs 0.05 to 0.14 ms at p50, and its tail follows the host more than the build. The heavy region is the tools panel, 73 to 77% of the time (52% in a masked drag, where the Masks panel takes 43%), spread over its sections by their controls; it reads almost every editor input, so no one value keys it, and a drag derives it for every input anyway. The figures are in [performance](specs/performance.md#the-workspace-derivation-per-message).
- **`draft.begin`, `draft.cancel` and `draft.reapply` run synchronously on the desktop thread**, like `draft.set`, proven by a press-to-first-frame measurement; only `draft.commit` stays an owner task.
- **One job API.** `job.read` and `job.cancel` serve every job kind. **Events name the asset and revision they changed**, and JSON clients have an `events.wait` long poll.
- **A RAW source keeps a second development within a 600 MiB byte budget**, so the hold-`\` compare stays exact within the owner's 150 ms bound and needs no second display path; a proxy-only compare would still need a development at the other entry's gains to build its proxy. The slot keeps the most recently used other development, `luxforge_raw::RETAINED_DEVELOPMENT_BYTES` bounds it (the X100VI's fits, one at the 128 MP limit does not), and the plane gate still lets the source worker build one development at a time. A repeated Original/current switch displays in about 20 ms on the X100VI and the Air 2S and about 32 ms on the Z6, at a steady cost of 230 to 460 MiB of RSS; the figures are in [performance](specs/performance.md#second-development). A byte-budgeted source cache that keeps JPEG decodes beside the RAW developments follows an RSS measurement on the M4.
- **Frames are opaque by contract**; the renderer carries no alpha.
- **`editor-acceptance` keeps only what `cargo test` cannot prove at the same layer**: the release run of the conformance suite, Basic numerics on the photo fixture, placement, and mask reopen through a fresh owner.
- **The components board shows widget states only**; the real panels are proven by their own scenarios.
- **A module supplies its history label through `label()`**, with its title as the default.
- **Test modules, including the pixel proof, register only in developer mode.**
- **A spatial operation with a large summed halo runs in 1024 px tiles.** An operation whose summed halo at its stage is at most 128 px keeps 512 px tiles, and one past 128 px runs in 1024 px tiles, so Texture and Dehaze do not recompute Clarity's margin around every small tile. One rule, `render::limits::spatial_tile`, gives the side to the reference renderer's whole frames; a read through a spatial layer is a GPU tile or the reference's whole frame. The bound is where the measurement put it: up to 128 px, 1024 px tiles rendered Texture alone 9 to 14% slower and every other stack as fast or at most 14% faster on a stage of 10 MP or more (Dehaze alone at 47 to 107 px, Texture with Dehaze at 53 to 121 px, Clarity at 127 px), while a sample through them cost three to four times as much; past 128 px every stack rendered 17 to 51% faster (Clarity from 151 px, and every stack that holds Clarity on the generated 24 MP and 60 MP JPEGs). Texture alone would also sample in 37 ms rather than 9 ms, so it keeps 512 px tiles. So do Dehaze alone and Texture with Dehaze at every stage size, because Dehaze's halo never passes 107 px. The 256 MiB spatial target still sets how many tiles run at once, and a tile whose working set is larger than the target still runs alone, one tile per batch. Every Presence combination rendered the same SHA-256 in 512 and 1024 px tiles on the generated 24 MP, 60 MP and presence fixtures. That identity rests on this evidence, not on construction, because the box passes' running sums start at each rectangle's edge. The figures are in [performance](specs/performance.md#tile-size-by-summed-halo).

## Interactive previews at 100%

A drag and the picture at rest at 100% and above are the GPU's region, and the reference renders whole frames ([GPU previews](design/gpu-preview.md#at-100-and-above)).

- Immediate adjustment feedback takes priority while dragging. A briefly softer image at 100% is acceptable during motion, with full detail restored on pause or release and the full-image histogram marked updating meanwhile.

The surface's overlap ceiling is provisionally accepted: one current and one retiring full-photo allocation of at most 512 MiB each, within 1 GiB for every surface together ([performance rule 6](engineering/performance-rules.md#rules)). This is an engineering ceiling for those slots, not a total editor or GPU product budget. Crop GPU textures and tiles, overlays and backend upload staging are outside that total and are not fully measured. Their residency must be measured and bounded before a total-memory guarantee; the follow-up is in the [rendering plan](../tasks/rendering/gpu-memory.json).

## Export

For [JPEG export](design/export.md#decisions), beyond the export contract under [editing and storage](#editing-and-storage):

- The title bar's Export button opens a menu of Export… (`Cmd+E`), Export again (`Shift+Cmd+E`) and Export with preset, each in the command palette too.
- The desktop exports the displayed entry, including a history preview, and never a draft.
- The suggested name counts up (`-web-2.jpg`) when `-web.jpg` is taken.
- An HDR or panorama result, which has no original, is named after its stack's first input with `-hdr` or `-pano` (`LXF_0005-pano-web.jpg`), and an HDR panorama would take `-hdr-pano`. No name the editor suggests has spaces, which are awkward on Linux: each run of whitespace becomes `-`.
- Exported metadata carries at most the design's EXIF field set, and no IPTC or XMP.
- The encoder is libjpeg-turbo through the `mozjpeg` crate at its fastest profile: baseline, 4:4:4, one interleaved scan, standard Huffman tables. It is more than twice as fast as the `image` encoder at the same output; the comparison is in the [export design](design/export.md#decisions).
- Import decodes JPEG with the same libjpeg-turbo, for dependency consolidation rather than speed: shipped code uses one JPEG library, and `image` keeps PNG only.
- The codec is its own crate, `luxforge-jpeg`: the one crate that names `mozjpeg`, depending on no workspace crate, with the JPEG container (the marker walk, the ICC chunks both ways) and the safety and recovery around the C library; the core keeps the policy ([architecture](design/architecture.md#the-jpeg-codec)). Its decoder goes on past libjpeg's harmless warnings and refuses the ones that stand for lost data: truncated or corrupt image data, a scan out of step and anything libjpeg would guess are refused, as is any warning the crate does not list, while bytes between header segments and an unknown JFIF version are accepted, with the same pixels as the clean file.
- JPEG import stays strict: extraneous bytes after a scan, an unknown Adobe colour transform, non-sequential scans and any data after the end-of-image marker are refused. Accepting trailing data (for example the video a Motion Photo appends) would mean reading past the image, a denial-of-service and parsing surface the owner does not want now; each relaxation is added deliberately, with its own review, only when a real file needs it.
- The methods are `export.plan` and `export.write`, read and cancelled through `job.read` and `job.cancel`; the export lane is a lane of the one job table.
- The desktop's export records 72 pixels per inch for each physical pixel of a point on the window's display, 144 on Retina, on macOS only, so Preview's Actual Size matches the editor's 100%; `export.write` takes the density as a setting and the core never reads a display.

## Export settings planning

Export is comprehensive, with settings: JPEG, WebP and a lossless hand-off format, optimised for the web by default, with a separate print setting. From the [design's](design/export-settings.md) boards:

- **JPEG, WebP and TIFF.** DNG is not exported. The lossless hand-off is an 8- or 16-bit TIFF with the edits baked in.
- **Lossy WebP** through libwebp.
- **An Export sheet that writes directly** into the folder its settings name, with an editable suffix and no second save dialog. The same sheet serves batch export, Export again and Export with preset.
- **Output sharpening** comes after the core: formats, resizing, optimisation and presets first.

Quality and metadata are export settings, not fixed or chosen per export. The never-replace, never-a-draft and displayed-entry rules apply. Optimisation has no memory cap: optimised JPEG and file-size limits use the memory the image needs at any size. The sheet warns about large images, and long exports show a progress bar. The design records the other choices as proposals with defaults: the preset values, where presets are stored, the resampling filter, the quantisation tables, the warning threshold, when the progress bar appears, the metadata levels and the shortcuts. It also declares the resized, 16-bit and sharpened export tolerances.

## Source-kind controls

From the [source controls](design/source-controls.md#decisions) design's recommended defaults:

- **Exposure is Basic's on every kind.** On a RAW photo it multiplies the developed scene-linear planes in Basic's colour stage, and the source development carries no exposure.
- **A masked white balance on RAW is relative**, Basic's ±100, as Lightroom's local Temp and Tint are.
- **JPEG keeps the frozen relative ±100 Temperature and Tint**, with As shot at 0 and 0. Basic's tint unit stays twice RAW's.
- **RAW keeps 2000–12000 K and ±100 Luxforge tint.**
- **Lightroom `Temperature` and `Tint` import converts** both together through the illuminant chromaticity they name, and refuses the pair when either result is outside the RAW ranges. It converts values and does not match renderings.
- **Presets store each kind's white balance separately.** Apply skips and reports a setting that does not apply to the photo's kind.
- **Reset Basic on a RAW photo also returns the development to As shot**, as one history entry.

## Masks panel

The panel matches the [Masks panel design](design/masking-workspace.md#decisions)'s boards, whose behaviour choices were delegated; current behaviour is described there and in the [user guide](user-guide.md#masks).

The [interactions](design/masking-interactions.md) are unplaced click-drag creation, actual coverage while drawing/painting, `O` for mask visibility, exclusive creation, correct selection/brush targets and responsive hover, with exact live coverage and strict creation ownership. Extra modifier shortcuts, relaxed navigation during creation and a numeric cursor target remain proposals; hover uses settled exact bytes without deferring readout.

- The panel is the design's: rows at the module-panel density, a Masks band, one overlay row, New mask and Add component as kind menus, the Brush section only while a brush is armed or selected, and an accent scope chip on each band bound to the open mask.
- Each component row carries its own `+ − ∩` mode control; the first component's shows `+` alone, dimmed, with the host's reason.
- Tool starts over Off show Tint automatically, including new gradients and brushes; explicit O/Off during the tool is honoured. Existing visible presentations are kept.
- Gradients work as Lightroom's do: each gradient drag, including the one that places it, commits on release as one entry with no Apply; a committed gradient's handles [rest on the canvas](design/masking-interactions.md#resting-handles) while it is selected; a drawn gradient is selected, and opening a mask selects its first gradient. `⌫` on a mask's only component deletes the mask.
- Renames happen in place, from the row's menu. A component rename is a host command, `mask.rename-component`, with `mask.rename`'s rules.
- `mask.list` reports each stroke's settings so a stroke row can say what it painted.
- The luminance range is drawn with a generic `range` control kind above its four fields.
- The Polygon kind, model selections, the inference runtime and Refine edge stay proposals; the panel draws nothing for them.

## Plan refresh

The recommendations recorded in [product decisions](../tasks/project/product-decisions.json), accepted:

- **Bayer highlight latitude is retained**, so the Z6 and the Air 2S keep values above sensor white through the demosaic as the X100VI does, provided the rendering change measured on the supplied Z6 and Air 2S files shows no highlight artefacts such as false-colour clipped highlights. If it does, the Bayer path keeps its clip at sensor white and the [RAW design](design/initial-raw.md#pixel-and-color-contract) documents it as that path's limit. The RAW high-precision qualification carries the change and its measurement.
- **The RAW memory budget has two parts:** the one-development working set, whose investigation target stays 1.5 GiB of process CPU RSS until the whole-editor RAW measurement attributes the current miss, and the retained second development under its own 600 MiB byte budget (`luxforge_raw::RETAINED_DEVELOPMENT_BYTES`). GPU memory is reported separately. Only that measurement's numbers count against the budget; if a measured total is unacceptable, the choice is between a higher target, a smaller slot and a compare that shows only the proxy.
- **`luxforge-json` takes the desktop's `--developer` flag**, with the same meaning, and refuses `--proof-endpoint` without it, as the desktop does. It is not turned on automatically in debug builds, so an agent's `schema.list` never depends on the build profile and a test that needs a test module says so.
- **GPU residency beyond the photo-texture ceiling stays a standalone follow-up**, measured beside the whole-editor RAW measurement before any total-memory claim. **The 1 GiB photo-texture ceiling is one budget shared by every photo surface**, so a second surface draws from it rather than doubling it. Neither implies a total editor or GPU memory guarantee.
- **RAW priorities are narrowed** to what the recorded product decisions leave open: which recording modes, firmware and controlled quality scenes come next, and whether any camera-JPEG or film matching goes beyond the neutral development.

## Popular cameras and RawSpeed

From the [popularity study](research/popular-cameras.md) and the [RawSpeed evaluation](research/rawspeed-evaluation.md):

- **Camera support is weighed by the cameras photographers use,** not the owner's own bodies. The default and common recording modes of the most-used cameras are admitted first ([popular camera support](design/popular-camera-support.md)).
- **Nikon High Efficiency (HE and HE★) NEF is not supported** until upstream LibRaw decodes it. It is refused explicitly, before unpack, for every Nikon model, never decoded silently or as garbage.
- **RawSpeed fills the sensor mosaic for the modes where it is exact and faster,** behind LibRaw's identify, metadata and post-unpack steps, through the adapter's own decode rather than LibRaw's RawSpeed hook ([RawSpeed unpacking](design/rawspeed-unpack.md)). LibRaw is not replaced.

## Detail

Design decisions for [Detail](design/detail.md), with their rationale; they are not qualification results.

- Detail performs manual noise reduction followed by capture sharpening in a `restoration` placement stage, after prepared source/pixel edits and before Basic, mixer and Presence. It reuses the host's spatial primitive. This avoids amplifying noise before denoising and keeps tone changes from changing the denoiser's input; Basic remains one composite. RAW sensor white balance and required source corrections stay upstream; JPEG relative white balance remains in Basic.
- Use bounded three-level wavelet shrinkage and thresholded, edge-gated unsharp masking, with numerical mappings frozen against independent references before production. Zero strength defaults on RAW and JPEG preserve Original and avoid automatic double sharpening of camera JPEGs. No AI, camera noise profiles or new module capabilities.
- Show Detail at Fit. A Detail drag is drawn on the GPU at every zoom, with no quiet interval or 100% refinement, and the picture at rest is the GPU's, the stack at full resolution in tiles reduced to the view, for Detail as for every stack; the reference renderer's exact frame reduced to the view is drawn where the GPU cannot draw it ([GPU-first rendering](#gpu-first-rendering)). Radius is in full-resolution content pixels; 100% is the inspection view for noise and halos. Extend the shared scale/preview contract rather than silently omitting the effect.
- Reuse mask targets, native field-patch presets, drafts and immutable history. Keep Lightroom Detail imports explicitly unsupported with per-setting reports until a mapping is separately qualified. Pre-tone spatial sampling, including Basic's neutral patch, must run off the catalog owner through bounded workers.
- Capture sharpening is part of the recipe and export evaluates it once. Output sharpening after destination resizing, tuned to medium and size, remains separate future export scope. Photographic quality and native performance remain unmeasured; the plan carries their gates and evidence.
- **16-bit hand-off on the JPEG path.** A spatial frame on the byte path that feeds a colour run or another spatial unit holds 16-bit encoded sRGB; 8-bit quantization happens only at a resample, a point replacement or the terminal output. An 8-bit hand-off between Detail and Basic bands under exposure and shadow lifts. This also settles Presence's JPEG spatial precision the same way.
- **Value-based mask overlays keep working behind Detail** through a bounded input-grid cache on the overlay worker, rather than being refused behind a spatial prefix.
- **No pixel work on the catalog owner.** A mutation that must read pixels through a spatial prefix, such as the colour-limited brush's seed, goes from the owner to its tile service, a GPU tile or the reference's named answer, with its revision and draft identity checked on return; performance rule 5 gains no exception.
- A render expected to take more than a second shows its progress on the photograph itself, as a bar across it, not only in the status bar and the Performance section. The full-resolution restoration frame cache that would avoid a long settlement is not planned ([instant previews](design/instant-preview.md#progress-of-a-long-exact-phase)).

## Lens and perspective planning

Lens and perspective scope and approach: [design and rationale](design/lens-and-perspective.md#scope). The initial scope is explicit offline Lensfun profile distortion plus manual two-axis perspective, with fixed-canvas coverage and no duplicate embedded DNG correction.

- **RAW mosaics need no acknowledgement.** A RAW source whose development applies no luminance warp (NEF, RAF, and the Air 2S DNG, whose WarpRectilinear corrects only lateral chromatic aberration) has distortion `known-unapplied`, so a profile applies without an acknowledgement. A JPEG's status stays unknown and needs the explicit assume-uncorrected acknowledgement.
- **Perspective is not presettable**, like crop and transforms; Lightroom's Perspective and Upright settings stay unsupported as a different perspective model.
- **Strong minification is refused.** A combined lens and perspective map whose local minification exceeds 1.8× is refused with its reason, because the bilinear sampler would alias; the limit is documented.

The Lens correction section ([design](design/lens-and-perspective.md#decisions)):

- **No list of lenses, and never an incompatible one.** The section shows the applied profile with an option to change it, or the detected profile, or a warning that no lens was found with a search to find it. Lenses are listed only as search results, and only compatible ones.
- **Auto-apply on first open, as an entry.** When an import creates an asset that has never had Lens correction and a compatible profile is detected, Luxforge commits it as an ordinary, undoable history entry with the module's normal label, right after the import's Original. Only where in-camera distortion correction is known not to be applied (a RAW or DNG whose optics ledger says distortion is known-unapplied). A reset turns it off for good, because the reset keeps a neutral layer; a re-import or reopen of an existing asset never applies it again. A JPEG whose in-camera correction is unknown is not auto-corrected: the section shows the detected lens with an Apply button, which applies assuming the camera did not correct it and says so. This lives in the core, so API and agent clients get the same behaviour as the desktop.
- **Drone names are curated product names.** A small curated core table maps DJI camera codes to product names, keyed by normalized EXIF make and model, so a fixed-lens drone reads "DJI Air 2S (FC3411)". It is used for display and the issue report, never for matching, and lists only well-known codes.
- **Report a missing lens.** When nothing is compatible, the section warns and offers a button that opens a prefilled GitHub issue asking for support for the lens.

## Tone curve

- The curve editor removes a point on a double-click on that point, beside Delete for the selected point, and keeps its numeric point list closed behind a Points disclosure until the person opens it. Both belong to the shared curve editor; the other [Tone curve proposals](design/tone-curve.md#proposals-with-recorded-defaults) remain open below.
- A single click on the plot away from every point adds a point, and a double-click on a point removes it; the plot fills the panel's width up to a maximum and is centred beyond it. A double-click on empty plot therefore adds one point and never removes it.
- Below black the curve uses a floor-subtracted luminance ratio: with `L_floor` the linear output of the curve at encoded zero, `rgb_out = L_floor + rgb·(L_out − L_floor)/L`, which equals Basic's frozen ratio rule whenever the curve keeps black at zero. A lifted black then fades the deepest shadows toward grey instead of turning their noise into coloured speckle. Basic's Blacks keeps its frozen rule; changing it is a separate follow-up.

## GPU previews

For [GPU previews](design/gpu-preview.md):

- **Speed comes first for interactive previews**, provided settling causes no large, noticeable jump in the image. Generous preview error limits are acceptable, especially with interface affordances that soften the hand-off.
- **Interactive speed comes before the memory and performance rules**: painting and drawing masks over Clarity, Texture and sharpening must keep pace as Lightroom does with tens of masks, and memory may be spent where it buys that. Under it **the GPU-preview budget is 2 GiB**, so a plan's chain of intermediates, a painted mask's planes held across ticks and the heaviest spatial slots draw on the GPU: painting at Fit over three masked Presence layers peaks at 0.56 GB, a RAW with Detail beside them at 0.97 GB, and ten masks at 100% at 1.47 GB. GPU memory counts toward the process's footprint on Apple silicon, so the provisional 1 GiB peak for one 60 MP photograph does not bound a gesture; the native qualification measures the high-water mark.
- **A recipe may hold 16 masked spatial layers.** That is the figure for masks and for masked colour layers, so tens of masks can hold Clarity, Texture and Detail as in Lightroom.
  - Each layer is still a sequential full frame in the reference render: about 0.8 s for a whole-frame layer at 60 MP.
  - The GPU preview shares scratch planes across its chain so that most such stacks draw on the GPU. A slot past the 2 GiB budget, which 16 layers reach at 100% on a RAW, is drawn on the GPU from its reduced stage, softer, and only one past the budget there too has the reference draw it.
  - That work and the limit of 16 are implemented ([GPU previews](design/gpu-preview.md#plane-sharing-and-precision)).
- **A preview drawn by the reference renderer says why.** When a gesture is drawn on the slower reference path for a reason that lasts, such as a slot past the budget, the desktop tells the person why, so a slower drag is explained.
- **A held GPU-preview boundary may be up to 256 MiB**, so RAW drags at 100% whose spatial margins need a larger full-precision boundary draw on the GPU. The desktop releases its CPU copy once the surface holds the boundary, and the upload is spread so the arrival's overlap of render buffer, staging and texture stays near twice the boundary rather than three times.

The specific limits (CIEDE2000, in a pointwise and a spatial class) and the "GPU preview" label are proposals with recorded defaults in the [design](design/gpu-preview.md#proposals-with-recorded-defaults); the editor runs on them until the owner revises them. The settle dissolve and the fallback notices are accepted ([accepted defaults](#accepted-defaults-and-scope)).

## CPU and memory efficiency

From an audit of the workspace ([design](design/efficiency.md)):

- Every byte-identical finding the plan lists is accepted: kernels, source preparation, owner and desktop copying, and build features.
- The RAW colour layers before a spatial layer stay recomputed for every tile's halo, about 2.7 times the stage under Clarity. Neither a materialized frame nor a row-band cache is added for now ([known remaining costs](engineering/performance-rules.md#known-remaining-costs)).
- The normalized float mosaic is removed from RAW development's peak through a checked-in librtprocess patch that the build applies reproducibly; vendored sources are never edited.
- The catalog uses WAL with `synchronous=FULL`, `fullfsync` and `checkpoint_fullfsync` on, so a commit reaches the drive with one full flush ([catalog durability](design/efficiency.md#catalog-durability)).
- A separate `dist` profile with LTO and one codegen unit is accepted for packaging and timing, beside the daily `release` build. It waits until the timing runs other plans have outstanding are recorded, since they and every recorded baseline build `release`.
- Not adopted: DCT-scaled JPEG decode for proxies, parallel restart-marker JPEG export, cropping masked sensor margins, `target-cpu=apple-m4` and `panic = "abort"`.

## Settings and feature flags

For feature flags with a UI ([design](design/settings-and-flags.md)):

- Flags live in a **Settings surface with an Experiments tab**, rather than a beaker popover in the title bar.
- The Settings entry is **always visible**, in every build.
- **Developer mode is the first flag**; `--developer` forces it on for a launch.
- **A flag never changes how a photo renders or exports.** An experiment that changes pixels is a module the recipe records.

Settings has a **General** tab for the person's preferences, which are not experiments, with Auto collapse history as its first.

The gear button and Cmd+,, launch flags applying at the next launch, storing only explicit choices, `luxforge-json` reading no flag at launch and the two developer-only proof flags are proposals with recorded defaults in the [design](design/settings-and-flags.md#decisions).

## Preferences

From the proposed settings in the [design](design/preferences.md):

- **Remembered across launches, with no Settings row:** panel visibility, the thirds and clipping overlays, the brush's size, feather and flow, the window's frame and the last export folder.
- **Settings › General holds** the mask overlay colour, the canvas background, a switch for automatic lens correction of new RAW photos, an interface size and the catalog location.
- **The GPU renderer has no switch or preference**; `--no-gpu-render` is the launch switch for a session the reference renderer draws ([GPU-first rendering](#gpu-first-rendering)).
- **Export defaults** are the [export settings](design/export-settings.md)' presets and the remembered last export.

The field shapes, the three canvas colours, the four interface sizes, the catalog as a folder holding `catalog.sqlite` applied at the next launch with a fallback to the default catalog when its folder is missing, and the other details are proposals with recorded defaults in the [design](design/preferences.md#decisions).

## GPU-first rendering

From the [GPU-first design](design/gpu-first.md):

- The GPU is the renderer of record for the picture at every zoom, the histogram and clipping counts, `render.sample` and the other pixel reads, and export. The CPU kernels stay as the whole-frame reference renderer and as the renderer for a machine without a usable GPU, with a notice.
- Image correctness is a declared tolerance against the reference render for every output kind, measured on the qualification corpus. Bit identity is not required of the GPU; on one machine and driver the GPU is deterministic, and across machines an export or a histogram may differ in the last digit.
- Settled pixels and reports from the reference keep their exactness tests against frozen fixtures; the GPU's outputs are held to reference buffers within the declared tolerance.
- **The histogram's tolerance is the earth mover's distance.** The GPU's histogram is held to the reference's by the earth mover's distance between them, at most 0.25 code on each of red, green, blue and luminance, and each clipping count within 0.1% of the output pixel count. A summed absolute bin difference counts a pixel moved by one code twice and failed 89 of 250 corpus stacks whose histograms moved by at most 0.13 code; it is reported beside the gated figure, not gated ([performance](specs/performance.md#gpu-qualification-against-the-reference)).
- **Dehaze's light behind a spatial layer is computed exactly** from that layer's output, through the staged sweeps, at rest, in export and in motion where the drag leaves that input unchanged. A stand-in light, computed with those layers left out, is used only for a colour drag between those layers ([design](design/gpu-first.md#proposals-with-recorded-defaults)).
- **There are no CPU production paths.** There is no `gpu_preview` preference, `--no-gpu-render` being the launch switch for a session the reference renderer draws; and there is no CPU estimate store, reduced-grid store or resources API `budgets.reduced_planes`: each reference frame reduces its own estimates.
- **A RAW white-balance drag frame is reported, not gated, beside its release.** The drag applies the white-balance change to the planes developed at the committed balance, an approximation that misses the pointwise limits on some sensors (the Air 2S's worst block 2.7 to 5.0 ΔE00), while the release re-develops the mosaic exactly and is held to the reference. The `raw-panel` scenario reports the moving frame against the release's picture at rest and fails only on a gross error (a worst block past 10 ΔE00), a CPU job per tick or a frame the GPU did not draw.
- **The GPU's throughput.** The GPU export worker may use up to 2 GiB for its tiles, apart from the preview budget. An open's first picture is the GPU's view plan when its programs are warm; the reference renderer's frame only on a cold shader cache or without a GPU. If a hidden window is found to stop drawing frames, `analysis.request` answers from the reference while the window is hidden, so an API client never waits on visibility. A developed photograph's catalog tiers render on the GPU through the tile service.
- **A drag the GPU would refuse gets a GPU path, not a CPU proxy.** The GPU draws the three ordinary cases: a RAW white-balance drag (the white-balance change as a leading step over the source the GPU holds), a geometry drag on a stack with no content layer (planned from the source), and a 100% drag past the GPU budget (drawn from the Fit plan's reduced stage, sharp again at rest). What remains holds its last frame for a reason that passes within a tick or an upload, and otherwise renders one reference frame per tick, the latest winning, labelled.
- **A session without a GPU drags on a CPU proxy.** The software adapter is too slow to adopt (lavapipe in a container on the M4: a Basic drag's p95 at Fit 89 ms on 14 cores and 214 ms on 4, against 33 ms; [performance](specs/performance.md#software-adapters)), and it stays available behind `--software-adapter`. A session the GPU does not draw at all — no adapter, a lost device, `--no-gpu-render` — keeps the display-sized CPU proxy for a drag at Fit and below 100%, and at 100% and above the proxy magnified, the reference's whole frame sharp at rest. Only such a session uses it: on a machine with a GPU a drag the GPU refuses still holds its frame or has the reference draw whole frames.

## UI themes

For [UI themes](design/ui-themes.md):

- **Themes, light ones included.** Luxforge Dark, the visual language's palette, is the default. The principle that the photograph is the only colour on screen holds for it alone, so other themes may tint the chrome.
- **The photograph's surround stays neutral** in every theme. A theme chooses its lightness, never its hue.
- **Each theme keeps its own accent**, even near the clipping or mask colours. The import report notes an accent within 15 ΔE00 of one of them.
- **A few popular Omarchy themes are bundled** for now, beside themes imported from a folder.
- **The theme follows Omarchy's current one** where Omarchy runs, whenever the library holds it: a preference, on by default, that choosing a theme by hand turns off. A theme the library does not hold changes nothing ([following Omarchy](design/ui-themes.md#following-omarchy)).

The rest are proposals with recorded defaults in the [design](design/ui-themes.md#proposals-with-recorded-defaults): which themes are bundled (Tokyo Night, Catppuccin, Catppuccin Latte, Gruvbox, Nord and Everforest), the contrast floors, which Omarchy forms are read, a Theme choice for the canvas background as its default, the Appearance tab, and leaving the system appearance for later.

## AI editing

For [AI editing](design/ai-editing.md), together with the two Corrections decisions it depends on:

- **AI editing is in scope**: Remove, Select, generative fill and Replace, and sky replacement, on local models the person downloads on request.
- **Quality first; no licence or provenance gate on models.** Models are chosen and ranked by measured results. A weight licence keeps no model out of the lists and the terms of a model's training data are neither a criterion nor disclosed. The one thing shown is what affects the person: when a licence restricts how they may use their own results, the model's card says so in one line, and each feature's default pick is the best model without such a restriction. Weights are the person's downloads from their publishers, not project dependencies.
- **The Fast fill model is the best measured result**, LaMa or MI-GAN, whatever its source.
- **A stale AI patch keeps rendering.** When a RAW white-balance change or an earlier repair invalidates a frozen patch, it is marked stale, the row and the status bar say so, and export asks for an acknowledgement; render, sample and export do not fail.
- **The inference runtime is delegated**, to be chosen for quality first, then performance, then maintainability; ONNX Runtime through `ort` with Core ML is the measured choice under that order.
- **Every choice between models is presented as a plain trade-off** ("Finds smaller objects; slower to start", "Higher quality, needs 24 GB of memory"), never by a model's technical name alone.
- **The shadow and reflection region is an editable part of the selection**, suggested by a heuristic, where the models allow it.
- **Budgets are accepted as starting figures under a quality-first rule**: the 256 MiB analysis cache and the 1 GiB of held candidates are accepted; a budget that would degrade a result is raised, with the measurement, rather than the result reduced; optimisation comes after the quality is right.
- **The generative tier refuses to install below a model's declared minimum**, with the requirement shown: 16 GB of memory for generative removal and 24 GB for Replace's text encoder, from the generative qualification's measured peaks (klein 4B Q4_0 at 4.25 GB, 6.6 to 8.4 GB with the text encoder), re-checked on machines that size in the final qualification. The local generator works at 1024 px, one candidate at a time (31 s per candidate at 512 px, 67 s at 768 and 122 s at 1024 on a quiet host): Generate makes one and Regenerate adds one, and the budget is the measured time. The prompt-free conditioning that spares removal the text encoder is published by the owner as a pinned asset on the project's GitHub releases and installed with the weights.
- **Consent to send photo data to a remote provider is remembered** per provider profile and data class until revoked; the capability contract changes with the remote tier.
- **The repair stage** sits after source development and before colour, in content coordinates, with its own repair primitive (the Corrections decision).
- **Sky replacement** is a deterministic layer over the Select Sky mask, with imported skies or a CC0 set.
- **Generative fill comes first.** Its workflow (a brush selection in the Remove mode, the candidate strip, Regenerate, Replace and Accept) is built over the fake generator on the inference foundation and the Corrections candidate seam, while the generative models are qualified beside it; the Fast tier, the analysis cache, object picking and Select follow and join the same Remove mode.

Recorded defaults and the remaining open questions (the first hosted provider, a removal fine-tune, a macOS 27 Swift shim, what a CPU-only machine is offered) are in the [design](design/ai-editing.md#decisions).

## RAW looks

Photos that look vivid in the catalog, where the camera's embedded preview is shown, should not look flat once developed ([design](design/raw-looks.md)):

- **New RAW photos start from a Luxforge look**, Standard, rather than the bare neutral development. The development stays neutral; the look is a recipe layer on it.
- **Matching the camera's preview is a setting**, Match camera, fitted per photo to its embedded preview.
- **Standard is built first**, on an architecture that carries Match camera.

The design's recorded defaults are accepted ([design](design/raw-looks.md#decided)): Standard written into a new photograph's Original; the look after Basic and before the Tone curve; resolved knots stored in every payload; Reset Look returning to Standard, the dot lit only away from it; Amount 0–200; not presettable; no baseline exposure read; colour kept under a monochrome preview; sRGB and Adobe RGB previews fitted. The Standard look is the one the corpus study proposed (+1.15 EV, contrast 1.6, 1.5 EV of headroom, chroma 1.2), approved on its contact sheets.

## Lightroom import

For the [Lightroom import](design/lightroom-import.md#decided), which is planned and not authorized for implementation ([plan](../tasks/lightroom/lightroom-import.json)):

- **Only the photographs worked on in Lightroom enter the catalog** (L1): edited, picked, in a plain collection, or a virtual copy. The rest stay on disk and Select browses their folders.
- **Ratings and colour labels become collections** (L2) in a "From Lightroom" group; the catalog keeps no ratings. **Keywords** are kept in the import record and not shown (L3).
- **Catalog folders come from events** (L4), as Develop makes them, not from Lightroom's folder tree.
- **Virtual copies and snapshots become named versions** (L5); Lightroom's history steps are counted, not imported (L6).
- **Lightroom's previews are the imported photographs' first tiles** in Select and the filmstrip until Luxforge renders them, and are never shown in Compare (L7).
- **Each photograph's Lightroom settings text is kept** with an explicit Re-map (L8); the import is one-way (L9); smart collections are reported, not imported (L10); only catalog versions verified on the owner's installation are read (L11).

## Lightroom alignment

For [Lightroom alignment](design/lightroom-alignment.md):

- **Lightroom is not a rendering target, but its controls' responses are.** They are the result of years of research and people are used to them: someone who reaches for +10 Clarity or −10 Vignette expects the same result. Every supported setting's slider is realigned so a value does about what the same value does in Lightroom (level B).
- **Where a dimension still differs widely, a targeted algorithm change is considered** (level C), aimed at the dimension that differs.

The programme ([design](design/lightroom-alignment.md#decided)):

- **Settings are realigned by editing area** (A2): Basic tone and colour, then Presence, the mixer and the vignette, then Detail, then masks and geometry, then RAW white balance.
- **Lightroom is driven by generated XMP** (A3), with one manual import and export per round by the owner; all tooling stays in Rust.
- **Only Lightroom's rendered outputs are compared** (A4). No reverse engineering: no Adobe SDK source or binary is read, decompiled or used, and no Adobe profile, table or curve ships.
- **The base rendering is measured and subtracted** (A5); the Standard look is kept.
- **A realigned module refuses recipes of its earlier scale** (A6) through its raised format marker; edited photographs need a new catalog.
- **Validation on the owner's real edits** runs locally, figures only, with consent asked each run (A7).
- **Sliders are realigned, not only imports mapped, and on photographs and pairs as well as targets** (A9): the corpus photographs and round 2's pairs are measured in one Lightroom session before any control changes.
- **A range shortfall extends the control** (A10): where Lightroom's extreme does more than Luxforge's whole range, the internal range grows so ±100 reaches it, where the algorithm stays stable; what cannot be extended is reported.
- **The importers' calibrated maps are the bridge** (A11), each row becoming the identity, and removed, when its slider is realigned.
- **A setting gets an algorithm change** (A8) when, after its rescaling, its median ΔE2000 against Lightroom's response exceeds 2 at values of ±25 and beyond, or it differs in kind; the change targets that dimension.

In the [task plan](../tasks/lightroom/lightroom-alignment.json), TASK-001 to TASK-007 are authorized: the rig's generator, measurement and fitting, recording round 1 and the calibrated mapping rows for preset import, with the generator's fixes and the measurement of photographs, then TASK-005 to TASK-007. TASK-008 onward are not authorized.

## Colour grading

For [colour grading in the mixer](design/colour-grading.md) ([implementation plan](../tasks/editing/colour-grading.json)):

- **HSL / Grading tabs inside Colour mixer**, with three-way and individual/Global wheel views.
- **The working feature includes all four wheels, luminance, masks, Luxforge presets and direct Lightroom preset mappings.** Measured Lightroom conversions are later refinement, not an import prerequisite.
- **Retain hue choices at zero saturation**, with separate wheel, HSL and Grading resets and reset-all on the mixer band.
- **The plan is ready for implementation without approval or research gates.** Deliver a working feature first; all Lightroom matching and independent-reference perfection work comes at the end. Initial numerical and layout details are implementer choices within the settled contract, with ordinary tests and rendered verification alongside the feature.

The final refinement task may build the grading-only slice of the alignment tooling without waiting on the separate alignment programme. Actual Lightroom exports are measurement inputs; missing exports leave response figures unmeasured and do not delay the working feature or reference/tooling delivery. This decision does not authorize unrelated alignment work or change its policy on private-catalog validation.

## HDR exposure merges

For HDR merges ([design](design/hdr-merge.md)):

- Select's grid offers a merge for exposure brackets, creates one HDR result shown as a virtual photograph and collapses the original frames into a stack.
- Keep the merged pixels durably on disk so ordinary reopening needs no repeated merge; source files stay unchanged and each bracket frame can still be picked independently.
- A merge can be removed or undone without losing the originals. The lifecycle retains the generated result and edits for restoration through library undo/redo.
- **One Merge action**: **Merge HDR** makes the result in its catalog folder and keeps Select open ([Develop from catalog folders](#develop-from-catalog-folders)); ordinary file picks stay available.
- The source, provenance and stack boundary serves other merge kinds too, particularly [panoramas](#panorama-merges).

HDR merges take **RAW brackets only**, with **automatic alignment and deghosting**. JPEG bracket inputs are outside this plan; the existing SDR JPEG export remains available for merged results. Numerical/backend/layout details are delegated within the contract.

## Panorama merges

A second merge kind beside [HDR exposure merges](#hdr-exposure-merges) ([design](design/panorama-merge.md), [task plan](../tasks/library/panorama-merge.json)), with the same merge workflow (one **Merge panorama** action), stacks, lifecycle and saved derived source:

- **RAW or JPEG frames.** Every frame of one merge is RAW, or every frame is JPEG; mixing them is refused. HDR panoramas (bracketed positions) are out of scope, and HDR merges stay RAW-only.
- **Selection first.** Selecting frames offers the panorama merge, and the job checks their overlap. Select also groups panoramas, from DJI-style panorama folders and, where it can, frames shot by hand; a panorama's moment header offers the merge ([grouping](design/catalog.md#grouping-inside-a-view)).
- **Automatic crop with Boundary Warp.** The saved result is always the largest clean rectangle, with a Boundary Warp option (0–100) that bends the stitch to fill the rectangle instead; no empty pixels reach the editor.
- **Derived sources have higher limits**, about 65,000 pixels a side and 512 MP, rather than a panorama being scaled down to the 128 MP limit; camera files keep their own limits.

**A merge preview opens first, for both kinds.** It works like Lightroom's Photo Merge preview:

- The merge action opens a sheet with a picture built from the frames' proxies, the kind's settings and the merge button. ⌥-click merges at once with the kind's last settings.
- The preview's reading and alignment are reused by the merge.
- It is HDR's settings sheet too, in place of a separate Merge options… sheet.
- The overlays are HDR's deghost overlay, and a panorama's crop and coverage and its frame outlines. Seams are not shown.

Recorded defaults, delegated within that scope and open to the owner's correction: 2–32 frames; Auto, Perspective, Cylindrical and Spherical projections; Boundary Warp 0; frames more than 3 EV apart or registering as the same view are refused as a bracket; a detected lens profile is baked into RAW frames; the result is named with "Panorama"; a panorama is not counted in usage statistics until the owner approves a `panorama-merge` event. Numerical, solver, blending and layout details are delegated.

## Develop from catalog folders

Choosing a catalog folder in Select changes what Develop holds, a parent folder is selectable as a whole, and a place's photographs from several days are seen and developed together ([design](design/develop-from-folders.md)):

- **The catalog folder is what Develop holds.** Develop's filmstrip is the catalog folder last chosen in Select, with its subfolders, and follows it live; the core, the API and the desktop have no separate development set.
- **Year › Place › Day.** Develop N files each photograph under a year folder, a place folder and a day folder ("2026 › Konstanz › 12 Sep"), reusing year and place folders by name.
- **Years are real catalog folders**, selectable, renamable and movable; Select computes no separate year grouping.
- **One Merge action** per kind: Merge HDR, and Merge panorama.

## Corrections workspace

The [Corrections workspace](design/corrections-workspace.md) design and its boards are the target for the offline Clone/Heal desktop workflow:

- **A press away from a pin adds a correction**, over earlier ones included; corrections stack and each samples the ones below it. People see `Heal 5`, never an operation ID.
- **Sources are automatic** unless set: a deterministic search resolves one once, on a render worker, and stores it as an ordinary offset; `/` takes the next-ranked one.
- **Modifiers**: `⌥`-click sets the source, `⌘`-drag places a spot and pulls out its source, `⇧` draws straight strokes. A set source persists until Auto or Escape.
- **Pins and handles** select, move, resample and resize. Moving a correction keeps its source in place, and a stroke's pin is at its first point.
- **100 corrections per photograph.**

Corrections is implemented from the [design](design/corrections.md) and delivered.

**Heal quality.** From a blinded review of 16 corpus cases, Heal is accepted as delivered, with its failure classes documented in the user guide (seams across continuing lines, large repairs repeating their source, objects whose surroundings hold the object). The frozen automatic-source candidate set is kept; the proposed clearing-distance candidates are not adopted, since the review found no difference between them ([numerics](design/corrections-numerics.md#the-corpus-review)).

**Corrections runs set aside.** The Corrections plan is complete with these runs set aside: the timing, memory and idle measurements (the timing tier, `editor-latency` and `measure`), the `gpu-qualification` rerun of the 60 MP far-source Heal stack after the read-set atlas, and the gate run of the `repair-patch` family. They are unrun and are not passes; they stay listed as outstanding work in [feature status](features.md) and [performance](specs/performance.md#corrections-qualified-on-the-m4).

## Open product questions

[Product decisions](../tasks/project/product-decisions.json) carries the storage, external-module, workload, Presence, GPU reconciliation, themes and Lightroom-authorization tasks. The other recorded defaults remain proposals in their designs; they acquire no additional decision task.

Masking phases A–D are delivered. Brush strokes are content-addressed by a hash of their contents, history entries remain complete recipe snapshots, and pure deltas are rejected. A missing or corrupt stroke fails explicitly. The `points` kind, stroke list and `brush-paint` interaction are shared host primitives, including for Corrections ([stroke storage](design/masking.md#stroke-storage)).

- How should catalog backup, portability, sidecars, folder relinking and external-drive sync work? A consistent backup must account for the catalog's live WAL and derived artifacts as well as its rows.
- Do the [catalog design](design/catalog.md#proposals)'s eighteen recorded defaults stand? The feature is implemented on them; P13's choice of how to meet the photographs view's time remains open.
- What is the first external module the owner would use, and what enablement and recovery behaviour does it need? A processing tool needs its CPU reference and GPU qualification; external GPU loading is not delivered by the built-in module interface.
- Which of the [UI themes proposals](design/ui-themes.md#proposals-with-recorded-defaults) stand? They cover the six bundled Omarchy themes, contrast floors and moving an imported theme's own ink to meet them, the Omarchy forms read, the Theme canvas-background default, the Appearance tab, and leaving the system appearance for later.
- [AI editing](design/ai-editing.md) ([above](#ai-editing)) leaves open the first hosted provider, whether to fund a removal fine-tune, whether a macOS 27 Swift shim is worth a Mac-only accelerator, and what a CPU-only machine is offered.
- Are [Lightroom import](design/lightroom-import.md) and [Lightroom alignment](design/lightroom-alignment.md) beyond TASK-007 authorised for implementation? Their design decisions, including the scope refinements below, do not grant that authorisation.
- Which measured workloads and responsiveness budgets become acceptance requirements? Accepted renderer budgets do not establish a bound on total native GPU memory.
- Which of the [presets defaults](design/presets.md#decisions-taken-on-defaults) stand? Current RAW preset white-balance conversion has no calibration; calibrated conversion belongs to the planned Lightroom alignment work, as decided below.
- Which of the [Tone curve proposals](design/tone-curve.md#proposals-with-recorded-defaults) stand — a luminance composite with luminance-ratio reconstruction rather than Lightroom's per-channel composite, one channel, order 5 after Basic and before the mixer, free endpoints, the unit-slope tail past white, sixteen points, the Lightroom `ToneCurvePV2012` transfer including an identity curve, a single-click add that snaps to the drawn curve, endpoints the desktop does not remove, and the delivered point rows? The module is implemented on these defaults.
- Which of the [Presence, colour mixer and vignette proposals](design/presence-mixer-vignette.md#proposals-with-recorded-defaults) stand: section names, stage order, mixer layout and vignette style? Implementation is delivered on the recorded defaults. GPU-first settles the renderer paths for spatial gestures and samples.
- What does the separate review of the existing RAW importer's DNG SDK isotemperature table require? The alignment work's prohibition below does not silently decide its disposition.

### Accepted defaults and scope

- **GPU correctness and motion.** The [GPU-first picture, sample and export tolerances](design/gpu-first.md#the-contract) are accepted. Samples use their class's statistics without a block statistic over scattered sites, and match the at-rest screen byte at 100%. Reduced-source motion below 100% reports differences against the reference and the picture at rest; the at-rest gate stays strict. Softer budget-reduced motion at 100% is accepted subject to explicit qualification. This acceptance is not qualification evidence.
- **GPU execution and pixel reads.** The displayed/updating counts cadence; GPU export with the explicit reference option, reference fallback and whole-export restart; owner work on the blocking pool; release-compilation behaviour; first-photograph warm-up; at-most-four pixel-read replays; and the host renderer for existing stored picks are accepted. The bounded 13×13 (169-site) RAW sensor pick may remain on the catalog owner. Neutral-picker answers must name their renderer.
- **Catalog renderer exception.** The catalog focus check's 100% development and `develop_instead` tiers use the CPU reference as an explicit exception to GPU-first. This exception does not cover committed catalog tiers, which use GPU tile rendering where available ([catalog](design/catalog.md)).
- **Later export features.** Resize, 16-bit and wide-gamut export each declare and qualify their GPU-versus-reference tolerance when the feature is designed. Quality and metadata are export settings, held in presets and the remembered last export ([export settings](design/export-settings.md)).
- **GPU preview.** The 150 ms linear-light dissolve, status-bar render timings and lasting fallback/reference notices without a canvas badge, shrinking scratch pool and qualified mixed precision are accepted ([design](design/gpu-preview.md#proposals-with-recorded-defaults)). Later GPU optimisations remain proposals.
- **AI inputs and resources.** All durable analysis, guide, fill and remote-crop inputs use the reference renderer. Generation locks its photograph while permitting edits to another photograph; progress and cancellation remain responsive. Combined memory requirements include resident renderer resources, and the generative minimums (16 GB for removal, 24 GB for Replace) must be rechecked against that total ([design](design/ai-editing.md)).
- **Masking and Corrections.** Masking's P1–P11 recorded defaults are accepted ([design](design/masking.md)). The Corrections repair layer precedes Detail, its GPU qualification uses the spatial class, and its descriptor has `preset: false` ([design](design/corrections.md)).
- **Lightroom scope.** Imported Lightroom previews supply grid first tiles only, never a large Develop tile; an import has no grouped undo. Calibrated RAW preset white-balance conversion belongs to alignment. The prohibition on Adobe-derived code and tables applies to the alignment work; the existing RAW importer's DNG SDK isotemperature table requires a separate review. Export scope stays as designed. Implementation authorisation for both Lightroom plans remains open beyond what [Lightroom alignment](#lightroom-alignment) records.
- **Verification and attribution.** Cost attribution uses `resources.read` and the Performance section. RAW Look verification is the Look scenario, with no additional catalog/export steps or Look-specific corpus gate; it measures current behaviour, not historical rendering parity. Acceptance supplies no unmeasured performance or total-memory claim.
- **Other recorded defaults.** Catalog P1–P18, Tone curve, presets, themes, Presence, storage and external-module defaults remain proposals in their designs. No additional decision task is added.
- **Hosted GPU checks.** The portability dependency stays. The lavapipe lane uses small functional journeys, omits `large24` and `large60`, and runs every retained scenario before failing at the end. This authorises no CI or engineering fix, and lavapipe is not the production renderer.

## Auto tone planning

Auto tone sets the Basic values automatically ([design](design/auto-tone.md#decided)):

- Auto sets **Lightroom's eight**: Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Vibrance and Saturation.
- **No white balance**, as in Lightroom; Auto white balance remains a separate later proposal.
- **Auto in presets**, recomputed for each photo, and the Lightroom importer maps `AutoTone`. Per-slider Auto and a desktop Auto on a selection are not in scope.
- **Tuning by fitting** the targets to Lightroom's Auto on the owner's photos, after the working feature is delivered, followed by owner review.

The deterministic algorithm, analysis before Basic with a forward model through Basic and the Look, the starting targets, caps, button placement and Cmd+U are recorded defaults in the design. Lightroom fitting still requires the owner's export, per-run consent and owner review.

## Optional usage statistics planning

Luxforge asks for permission once on desktop opening and shares whole-app event information, including tools, images imported and periodic performance statistics, only after agreement; it sends quietly in background batches to `api.luxforge.app`, tolerates loss, and offers Settings opt-in/out. Events carry device details, a unique persistent ID, OS/app versions and CPU/GPU information for performance debugging and support targeting, and never PII, IP addresses or photo/metadata information. A usage timeline that strings together the events from a single user or session is acceptable as long as it holds no PII. Each image in Develop has a stable ID. There is no hourly or daily cap, and the disclosure just says events are tracked for debugging and analytics. Protocol choice is delegated; an open-source backend is preferred. The API does not exist yet.

The [design](design/usage-statistics.md) and its task plan record a non-modal once-per-configuration notice, protected host consent, typed events with session-relative times and sequence numbers, a random installation ID stable across opted-in launches and removed on saved opt-out, a random in-memory session ID per launch, a per-image ID that is an HMAC of the catalog fingerprint under a local key that never leaves the machine (a filename/creation-time hash collides, changes on rename, can contain a name and can be reversed by guessing), and bounded approved device context. Reports are pseudonymous timelines, with no PII, photo contents, filenames or metadata, no editing values and no IP-based logging/hashing/limiting. A short disclosure with the field list collapsed in Settings. Ten-minute or 512-event HTTPS JSON batches at least 60 seconds apart, with no hourly or daily cap, have no disk queue/replay/shutdown wait. A standalone Rust ingress and Vector/ClickHouse/Grafana are the local open-source reference, storing one row per event with timeline and path queries and 90-day retention. Because every build is version 0.0.0, a packaged build embeds its clean source revision in the reported release; unrevisioned builds never report. A checked-in activation switch keeps every build from asking or sending until the real endpoint passes its deployment checklist, a separately authorized change. Another process's opt-out is honoured by rereading stored consent before each upload, not by a file watch. These are choices within delegated scope; deployment is not approved. The working client is implemented, with production reporting off.

## MCP adapter planning

Agents reach Luxforge through `luxforge-mcp`, a stdio MCP server over the same operation registry, with no feature logic of its own ([design](design/mcp.md)). One generator offers two tool shapes: a compact default of three generic tools (list, describe and call any method), and one typed tool per method with `--tools all` or a namespace list. Preview results carry the picture inline by default, at most the 2048 px tier and never a full-resolution export; `--no-images` withholds pixels. The adapter attaches to the running desktop's live session by default, and `--headless --catalog PATH` starts its own `luxforge-json` child with Edit authority. The desktop gains Settings › Agents with the bundled binary's path, per-client configuration to copy and the connected live clients with Disconnect. Stdio only, dual-era protocol support (2026-07-28 with 2025-11-25 and 2025-06-18), the hand-written protocol layer and the other defaults the design lists are recorded defaults.

## High-zoom minimap planning

A minimap shows at zoom levels of **200% or more**. The inclusive percentage threshold is the requirement, with **click to jump and drag the viewport rectangle**. The [design](design/minimap.md) records this interaction with lower-right placement and the other layout and engineering defaults as recorded defaults. It is implemented; corpus qualification, broader rendered coverage and measurements remain outstanding.

## Copy and paste settings planning

Copying settings between photographs: **adjustments only** in the first delivery (the presettable groups; crop, orientation, lens correction, perspective, the RAW look and masks later), **every group except White balance** checked by default with neutral groups included and the last confirmed choice remembered for a quick `⌘C`, **multi-selection in Develop's filmstrip** with `⌘V` pasting to it, and **Paste from previous** (`⌥⌘V`). The [design](design/copy-settings.md) records the remaining defaults.

## Range mask picking

For colour and luminance range masks ([design](design/range-masks.md)):

- **Creation stays behind `masks.range-selections`**, off by default.
- **Lightroom-style picking.** Creating a range mask arms an eyedropper. Colour: a click replaces the samples with that colour, Shift+click adds one (at most five), a drag samples an area. Luminance: a click sets a band around the clicked tone, a drag sets the band to the area's tonal span. Escape, Enter or Done ends picking. The band slider, number fields, swatch slots and Refine remain.
- **A mask with no bound layer is read, not refused**, as the picking flow needs: an unbound mask reads its input where a first Basic adjustment would take it.

The derivation constants, the input site's colour-stage rule and the canvas details are delegated choices recorded in the design. Implemented, and qualified and measured on the M4 ([design](design/range-masks.md#verification-and-acceptance) for what remains).

## Code structure consolidation

The GPU code splits statically into `luxforge-gpu-types` and `luxforge-gpu` ([design](design/code-structure.md#approved-boundary-task-003)). The shared crate holds std-only GPU primitives, shader conventions and pure layout rules; the backend owns the wgpu executor and tile runner. Core keeps semantic planning and `TileService`, the app keeps one lowering path and host source/worker policy, and UI keeps Iced presentation on its own device. The split leaves the separate tile-worker device, budgets, bounded caches, retirement, cancellation and fallback behaviour as they are.
