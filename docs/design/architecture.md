# Architecture

One application service, used by the desktop UI and by external clients alike, owns asset state, recipe and history transactions and bounded work. Rendering consumes immutable snapshots. Every edit action is a tool module: the registry validates descriptors once, the host dispatches actions and resolves processing through it, and the desktop renders controls from the same descriptors.

## Workspace

Rust 1.94 workspace: Iced 0.14 on wgpu, `image` for JPEG and PNG, `moxcms` for conservative sRGB profile recognition, `rfd` for native and portal dialogs, bundled SQLite through `rusqlite`, Rayon for the parallel raster pass. Exact versions are pinned in `Cargo.lock`.

- `crates/lightwell-core`: images, recipes, rendering, SQLite catalog and history, preview scheduling, JSON API.
- `crates/lightwell-app`: the desktop, split into `app/` (the Iced application, messages, update, owner tasks, evidence, keymap and the crop driver), `state/` (the pure view model, no framework types) and `view/` (rendering, no core types and no owner access), plus native adapters, diagnostics and the crop draft state machine and its canvas (`crop_draft.rs`, `crop_canvas.rs`); the desktop and headless `lightwell-json` binaries. The boundaries between those layers are enforced by `cargo xtask check-repository`.
- `xtask`: development, check, evidence, acceptance and packaging commands.
- `probes/s0`: the isolated Iced/egui comparison workspace used to select Iced; not the maintained application.

Add boundaries when there is real code to own them. Crates and dynamically loaded binaries are separate choices.

## Boundaries

| Boundary | Responsibility |
| --- | --- |
| Desktop shell | Layout, focus, pointer capture, mapping gestures to semantic commands; displays authoritative state and holds none |
| Core service | Asset, layer and snapshot identity, shared invariants, atomic commits, revisions, request deduplication, history navigation |
| Catalog | Read-only source references and verified fingerprints; durable layers, snapshots, history, current/redo state and request results |
| Renderer and scheduler | Evaluate an immutable ordered stack from verified originals with bounded memory, cancellation and generation identity |
| Tool modules | One descriptor each (effects, actions, parameters, controls, optional canvas pick), input parsing, state validation and no-op detection, compilation of payloads into host processing primitives; [modules](modules-and-api.md) |
| JSON/IPC, later MCP | Transport to the same catalog owner plus operation discovery; no alternate persistence or edit logic |

## Sources, layers and snapshots

Import references a JPEG and creates a stable asset plus Original. Path and root are mutable locators; a full content fingerprint verifies the bytes. Same-filesystem aliases resolve to one asset, and identical copies at different paths are not merged. Missing or changed sources keep their edits and report an explicit rendering limitation.

An edit layer is an identified, typed operation with parameters and an input/output stage contract. A recipe is the ordered stack of layers. A history entry names a semantic action and stores its complete resulting immutable recipe. Each layer's coordinates refer to its input stage: a pixel edit before a rotation travels with the image, one after it addresses the rotated dimensions. Optimizations may fuse operations only where the result stays byte-identical.

## Persistence

Local SQLite holds current state, action entries with their snapshots, a monotonic revision, redo navigation and request results, written atomically in short transactions with an internal format marker. Originals and disposable pixel caches stay outside the database. A failed write preserves the prior durable state. Entry records are the only stored copy of a stack (catalog format 3). Only the current catalog and payload shapes are supported; unsupported formats fail explicitly without rewriting data. Every history entry is retained: undo and redo navigate without adding inverse rows, and Restore copies a snapshot into a new action. Versions are named references to entries and lineage follows the stored undo-parent column; see [versions and lineage](versions-and-lineage.md). Reopen recovers the same IDs, current snapshot and navigation state. Backups need a consistent SQLite snapshot, not a copy of a live file. Unknown payloads and missing providers are retained and reported, never dropped.

## Rendering and limits

Orient once to upright pixels and give every buffer explicit color meaning (8-bit sRGB for the supported JPEG subset). Exact buffers on synthetic fixtures prove correctness; JPEG re-encoding is not an oracle. Decode once through a signature-validated cache, share immutable pixels, compile a recipe into at most one raster pass and answer point queries from the compiled geometry. A resample (the crop module's non-zero-angle case) is a stage boundary: it separates the compiled recipe into segments, each an exact raster pass, so at most two full frames — one segment's output feeding the next resample's input — exist at once, each within the 512 MiB frame limit. Point queries evaluate through a resample recursively and never allocate a frame. Preview work is one active plus one replaceable pending job, tagged with a generation.

Limits: 128 MiB encoded source, 64 MP, 16384 px per side, 512 MiB per evaluated frame, preview uploads up to 4096 px per side, 100 history rows per page, 8 live clients, 256 buffered events, 1 MiB per request line. Decoder limits are not process or GPU memory limits. The rules and review checklist are in [performance rules](../engineering/performance-rules.md).

## Agent contract

One typed service backs the desktop and external JSON sessions. While the GUI is open it owns the catalog and accepts authenticated same-user loopback clients; headless ownership is allowed when it is absent. `version.create`, `version.delete`, `version.list` and `history.lineage` expose named states and the undo-parent chain. The owner holds each registered client's session and reports a session revision with every session-returning response. Commands carry schemas, units, ranges, defaults and structured errors. Mutations require an expected revision and a request ID with a documented deduplication scope. Reconnect reads fresh state rather than replaying. One client's disconnect does not cancel another's jobs. Diagnostics never go to protocol stdout. Preview and session state are not history: an external commit updates current state while a selected historical snapshot stays selected, and M4 drafts stay intact and marked conflicted until explicitly resolved. MCP later adapts the same registry.

## Modules and extension path

The pixel, transform and crop tools are linked modules registered by `ModuleRegistry::builtin()`; each declares its current effect identities, payloads and history actions. The crop module adds a `number` parameter kind and the `crop-frame` canvas interaction to the same descriptor shape, and updates its one crop layer in place through `ActionPlan::Update` rather than always appending. The host generates `edit.<action>` API methods and the desktop generates controls from the same descriptors, so a module capability cannot exist without an API. Unknown or unavailable effects stay in every snapshot and fail rendering explicitly. Linked built-ins with lazy resources are enough for M4. External loading comes later around a selected use case with measured costs; see [modules](modules-and-api.md).
