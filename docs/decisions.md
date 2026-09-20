# Product decisions

Accepted owner decisions and the questions still open. Proposals stay proposals until the owner decides; record each answer here and in the affected spec.

## Product and platform

- A fast, non-destructive desktop editor for professional and prosumer collections on macOS, Windows and Linux.
- The owner's M4 MacBook Pro (M4 Pro, 14 CPU and 20 GPU cores, 48 GB unified memory, macOS 26.5.2) is the first target and the reference machine.
- Engineering baselines: macOS 14+ arm64, Windows 11 24H2+ x64, Ubuntu 24.04 x64 with GNOME Wayland and X11. These are intended targets, not verified support; see [platforms](engineering/platforms.md).
- Unsigned development packages only. No signing, stores, auto-update or public release yet.
- Project code is GPL-3.0-or-later. Dependencies and extensions should be open source and license-compatible; no proprietary hosted service is a development prerequisite. The manual license, native and asset review is deferred and is not a passing result.
- Everything is v0 and breaking changes are expected. Only current catalog, recipe, API and module shapes are supported; no migrations, compatibility shims, old-version fixtures or historical parity requirements. Unsupported data is refused without rewriting it. No release-version planning, cloud or accounts, marketplace or generalized processing graph.
- Initial RAW targets are the original Nikon Z6 and the Fujifilm X100VI. Benchmark established decoders before proposing a custom one.
- Lightroom Library and Develop are familiarity references. Map, Book, Slideshow, Print, Web and Publish Services are out of scope.
- All development tooling is Rust (`cargo xtask`); no second toolchain.

## Editing and storage

- Originals are read-only. Import references existing files with a stable asset ID, a verified content fingerprint and a changeable locator. SQLite is the local catalog. Folder relinking, sidecars, portability, backups and sync need their own workflow decisions.
- A "layer" is an ordered edit operation in a recipe. Each committed action stores a complete immutable recipe snapshot and one attributed history entry. Bitmap compositing, blend modes and arbitrary layer reordering are not selected.
- History is a graph: entries keep their undo parent and nothing is truncated. A named **version** (the owner's name for the Lightroom-style saved state) is a reference to one retained entry, not a branch. The catalog uses internal format 2; unsupported formats are refused. See [versions and lineage](design/versions-and-lineage.md).
- Undo and redo navigate saved entries without appending rows. Preview is read-only. Restore appends an action and keeps all later entries. A new edit clears shortcut redo, but every entry stays available. Committed state survives restart; drafts do not.
- One workspace: centered photo, collapsible controls, visible history, Fit, numeric zoom and true 100%. No library grid during the editor milestones. Cmd/Ctrl+O imports; Cmd/Ctrl+Z and Shift+Cmd/Ctrl+Z navigate history.
- Geometry: the visible composition travels with mirror and quarter-turns, and a locked ratio swaps orientation on a quarter-turn. Fine angle is limited to ±45°. Space-drag pans.
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
- Should RAW precede a small library or tonal tools? Which camera modes and camera-JPEG or film-simulation matching matter most?
- What is the first external module the owner would use, and what enablement and recovery behavior does it need?
- Which measured workloads and responsiveness budgets become acceptance requirements?

The [Basic and histogram proposal](design/basic-and-histogram.md#open-decisions) also leaves JPEG-first delivery, Basic layer organization, gesture commit timing, global Tone quality scope and relative priority open. These are recommendations for the requested planning work, not accepted product decisions.
