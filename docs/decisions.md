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
- History is a graph: entries keep their undo parent and nothing is truncated. A named **version** (the owner's name for the Lightroom-style saved state) is a reference to one retained entry, not a branch. The catalog uses internal format 4; unsupported formats are refused. See [versions and lineage](design/versions-and-lineage.md).
- Undo and redo navigate saved entries without appending rows. Preview is read-only. Restore appends an action and keeps all later entries. A new edit clears shortcut redo, but every entry stays available. Committed state survives restart; drafts do not.
- One workspace: centered photo, collapsible controls, visible history, Fit, numeric zoom and true 100%. No library grid during the editor milestones. Cmd/Ctrl+O imports; Cmd/Ctrl+Z and Shift+Cmd/Ctrl+Z navigate history.
- Geometry: the visible composition travels with mirror and quarter-turns, and a locked ratio swaps orientation on a quarter-turn. Fine angle is limited to ±45°. Space-drag pans.
- Pixel-stage edits address the content stage (the source after EXIF orientation) and are placed before quarter-turns, reflections and the crop, so changing the crop never moves or invalidates them. The host chooses a new layer's position from its effect stage. See [content-space edits](design/content-space-edits.md).
- Quarter-turns and reflections are one orientation layer holding the composed exact state, updated in place while it is the last layer; four rotations leave one neutral layer. See [orientation layer](design/orientation-layer.md).
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

## Basic adjustments and histogram

Accepted on 2026-09-21 for the [Basic and histogram design](design/basic-and-histogram.md). These settle the product questions; implementation was authorized on 2026-09-21.

- JPEG first: the Basic controls and the histogram work on the supported SDR sRGB and greyscale JPEG subset. RAW is separate later work with its own input and colour contracts, not a prerequisite.
- One Basic layer per recipe with a fixed internal group order (White balance, Exposure, tone curve, Vibrance, Saturation), placed by its colour stage before the geometry tail and updated in place at the same identity.
- Slider release, key-up or Enter commits one action; Escape cancels; focus loss cancels an unfinished gesture. There is no Apply panel for adjustments.
- Global pointwise tone first: Contrast, Highlights, Shadows, Whites and Blacks are one monotone global curve before any edge-aware processing is considered.
- Numerical ranges and equations start from the Lightroom research and the design's proposed ranges and defaults; the numerical tasks select and freeze them against independent references before a control ships, and no value is claimed as Lightroom-equivalent.
- Priority: Slice A (histogram, clipping and Exposure) next, then Slice B (the remaining Basic controls); export, Locate and MCP keep their own follow-up priority.

The plan runs to completion on the defaults below without further owner input; the owner reviews and refines the result afterwards. Each default is provisional and recorded in the design, so a later change is a normal edit, not a silent reinterpretation.

- Performance targets are provisional thresholds: warm 24 MP slider-to-presented-frame p95 below 100 ms, settled exact histogram p95 below 200 ms, a 64 MiB aggregate scratch cap. A measured miss is reported with its figures and does not block delivery.
- Global tone stays global. If the tone study finds a visual case a global curve cannot pass, the control ships with that limitation documented and an edge-aware proposal recorded as later work.
- Clipping overlays: any channel at an endpoint counts; shadow clipping draws blue, highlight red, both magenta; tooltips state the rule.
- Neutral picker: a 5 × 5 patch at input-stage pixel centres clipped at the image edges, evaluated before the Basic layer; near-black, clipped and non-invertible samples are rejected with a reason.
- One Basic layer per recipe; more than one is never user-facing, and an imported stack with several reports ambiguity.
- Float exactness: results match an f64 stepwise reference within `1e-6 + 1e-6 × |reference|` and at most one output code of rounding where the design permits it.
- After Slice B, Tone Curve is the next module candidate; Detail, Texture and Clarity, Dehaze and the colour mixer follow in that order unless the owner reorders them.

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

## Open product questions

Tracked in [product decisions](../tasks/product-decisions.json).

- How should catalog backup, portability, sidecars, folder relinking and external-drive sync work?
- Beyond the supplied files, which RAW recording modes/firmware and controlled quality scenes should be prioritized? The implemented decoder/developer and neutral defaults are explicit; broad visual acceptance, the measured resource target and additional DJI modes/scenes remain in [RAW qualification](design/initial-raw.md#remaining-qualification-and-decisions).
- What is the first external module the owner would use, and what enablement and recovery behavior does it need?
- Which measured workloads and responsiveness budgets become acceptance requirements?

The Basic and histogram product choices were decided on 2026-09-21 and implementation was authorized the same day; see [Basic adjustments and histogram](#basic-adjustments-and-histogram).
