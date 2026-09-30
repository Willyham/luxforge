# Architecture

## Overview

One application service, used by the desktop UI and by external clients alike, owns asset state, recipe and history transactions and bounded work. Rendering consumes immutable snapshots. Every edit action is a tool module: the registry validates descriptors once, the host dispatches actions and resolves processing through it, and the desktop renders controls from the same descriptors.

## Workspace

A Rust 1.94 workspace. Exact versions are pinned in `Cargo.lock`. Add boundaries when there is real code to own them; crates and dynamically loaded binaries are separate choices.

| Dependency | Used for |
| --- | --- |
| Iced 0.14 on wgpu | The desktop |
| `image` | PNG |
| libjpeg-turbo through `mozjpeg` (its bundled source built with `cc`) | Reading and writing JPEG, behind the private `luxforge-jpeg` crate, the only code that names it (`cargo xtask check-repository` enforces this) |
| `moxcms` | Conservative sRGB profile recognition |
| `rfd` | Native and portal dialogs |
| Pinned, bundled LibRaw and librtprocess | RAW, behind the private `luxforge-raw` adapter |
| Bundled SQLite through `rusqlite` | The catalog |
| Rayon | The parallel raster pass |
| `roxmltree` | Lightroom XMP presets |
| `url` (ASCII hosts only, through the data-free `idna_adapter`) and `zeroize` | Module capabilities, in the core |
| rustls with the ring provider and the platform certificate verifier | The transport's TLS, in `luxforge-net` |
| `ureq`'s agent (no features, on the transport's own socket and TLS) | The transport's HTTP/1.1, in `luxforge-net` |
| `security-framework` | The macOS Keychain, in `luxforge-net` |

### Crates

A rule marked *(enforced)* is a rule `cargo xtask check-repository` applies.

- `crates/luxforge-core`: images, recipes, rendering, the SQLite catalog and history, preview scheduling and the JSON API; its files are listed [below](#the-cores-files).
- `crates/luxforge-net`: the host's network transport and secure secret store, behind the core's `Transport` and `SecretStore` traits. The desktop and `luxforge-json` build both and give them to the catalog owner through `HostConfig`. Only this crate may depend on `ureq`, and only it frames HTTP *(enforced)*. Its files are listed [below](#the-transports-files).
- `crates/luxforge-ui`: the widget library and theme tokens of the Develop and Select workspaces. It depends on Iced only, never on the core, so a widget cannot hold editing logic.
- `crates/luxforge-jpeg`: the one JPEG codec, libjpeg-turbo through `mozjpeg`, and the JPEG container around it; described [below](#the-jpeg-codec). It depends on no workspace crate, and only `luxforge-core` depends on it *(enforced)*.
- `crates/luxforge-raw`: the private RAW adapter over the pinned native LibRaw and librtprocess source, with a safe API ([its README](../../crates/luxforge-raw/README.md)) that develops a qualified RAW and, for any RAW LibRaw identifies, lists and extracts its embedded previews by positional reads without unpacking it; its `limits.rs` holds the RAW admission limits and the parallel thresholds in the [limits](#limits) table.
- `crates/luxforge-process`: the counters the operating system keeps for this process (CPU time, memory, GPU time and GPU allocations), and the mounted file systems with their names, UUIDs and whether each is removable (`mounts.rs`, which the catalog's index reads), behind a safe API.
- `crates/luxforge-app`: the desktop and the `luxforge` desktop binary; its layers are listed [below](#the-desktops-files), with the boundaries between them *(enforced)*.
- `crates/luxforge-cli`: the headless `luxforge-json` binary (`json.rs`), which serves one JSON-lines client on its standard streams, and `Paths` (`paths.rs`), where the application keeps its configuration, data and logs, which the desktop resolves once at startup through the same type. Its normal dependencies hold no GUI crate (Iced, wgpu, rfd, `luxforge-ui` or `luxforge-app`) *(enforced)*, so building the headless binary builds no window, renderer or dialog stack. Its process tests (`tests/`) drive the built binary.
- `crates/luxforge-evidence`: the evidence script's step types, serde-derived and validated, with no dependency on the editor or the core: the desktop parses a script with them and xtask builds every scenario's script from them.
- `crates/luxforge-reference`: the independent f64 references production is tested against (the sRGB transfer function and exposure, and one module per numerical study: colour, the DNG corrections, mask, mixer, Presence, range, tone, vignette, white balance), and as its own tests the studies that prove each reference's properties and freeze the committed fixture corpora. It depends on no workspace crate *(enforced)*, so a reference can never import the code it checks. Only `[dev-dependencies]`, `luxforge-testkit` and `xtask` name it.
- `crates/luxforge-testbase`: the one gate (`Gate`: shut, open, pass, reached) and the one hang-bounded wait (`wait_until`, `wait_for`) every test orders its steps by, the one nearest-rank `Distribution` every timing figure is read from, and the test support that needs no core type (the loopback test server, the proof endpoint and the fixture and scratch paths, described [below](#the-test-kit)), never compiled into a shipped binary. It depends on no workspace crate *(enforced)*, so the core's own unit tests and the widget crate's can use it and a core test build compiles the core once; `luxforge-testkit` builds on it and `xtask`'s timing tools write their one report shape through it. See [tests that do not depend on host load](../engineering/development.md#tests-that-do-not-depend-on-host-load).
- `crates/luxforge-testkit`: the core-typed helpers the tests and `xtask` share, never compiled into a shipped binary; described [below](#the-test-kit). Only `[dev-dependencies]` and `xtask` name it, and never `luxforge-core`'s.
- `xtask`: development, check, evidence, acceptance and packaging commands.

### The core's files

Paths are under `crates/luxforge-core/src`.

| Path | Holds |
| --- | --- |
| `lib.rs` | The public surface, listed by name: what the desktop, `luxforge-json`, `luxforge-net`, the test kit, xtask and the core's integration tests use through the crate root, every type a public item's signature carries so a consumer can name whatever it receives, and the modules consumers name items through (`activity`, `analysis`, `capabilities`, `catalog_types`, `colour`, `jobs`, `latest`, `mask`, `path`, `resources` and `seed`). It keeps one marked section per lane of the catalog work for that lane's exports. Every other item is `pub(crate)` or narrower, so the compiler reports what nothing uses |
| `editor.rs` | The editor service: the `EditorService` struct, opening a catalog, and the types its API speaks |
| `editor/catalog.rs` | The schema, the format marker, row mapping, and the entry, stroke and request rows |
| `editor/catalog_rows.rs` | The format-12 catalog tables' row writers and readers, which the import, the seeder and the catalog lanes share: volumes, catalog folders, an asset's catalog columns and capture row, collections and members, picks and indexed folders |
| `editor/entries.rs` | The cache of hydrated history entries and each asset's head, and the one `mutate` every write that moves a head goes through |
| `editor/history.rs` | Admission, commits, undo, redo, restore, versions, lineage and request deduplication |
| `editor/source.rs` | Source preparation and cache, import, file identity and RAW settings |
| `editor/evaluate.rs` | Preview and analysis jobs, render, sample, locate and transform |
| `editor/plan.rs` | Actions, the stage context, queries, drafts and composites |
| `editor/masks.rs` | The `mask.*` commands and mask targets |
| `editor/describe.rs` | State and recipe views |
| `editor/artifact_store.rs` | Derived artifacts |
| `editor/test_support.rs` | The helpers the service's tests share. Each concern's tests sit in its file, the artifact store's in `artifact_tests.rs` |
| `render.rs` and `render/` | Rendering, one file per concept: `entry.rs` (the one way in, `render`, and the `Render` it returns), `compiled.rs` (segments separated by stage boundaries, and `Entry`, the one dispatch over the boundary kinds), `geometry.rs` (exact geometry, a resample's mapping and read rectangle, and the byte domain's bilinear pass), `colour_runs.rs` (colour runs and their masked blend), `pipeline.rs` (the one pipeline, generic over its pixel domain), `byte.rs` and `linear.rs` (the two pixel domains, each with its rows and its driver), `spatial.rs` and `window.rs` (the spatial primitive's execution and the windowed proxy), `raster.rs` (the rendered frame), `locate.rs` (the locate and transform types), `context.rs` and `parallel.rs` (the render context's budgets and the one parallel gate) |
| `source.rs` and `source/linear.rs` | The prepared sources: `SourceImage`, and the RAW source's `LinearImage` with its view |
| `cancel.rs` | `Cancel`, the crate's one cancellation token |
| `colour.rs` | Each colour equation the renderers and tool modules share, once: the sRGB transfer function and exact output quantizer, Rec. 709 luminance and the luminance-ratio reconstruction, the Oklab conversion, 3×3 linear algebra, and the Planckian locus with the CIE 1960 `uv` projection |
| `capabilities/host.rs` and `capabilities/host/` | The capability host the catalog owner holds: the host, the settings methods, capability job cancels and `module.status` in `host.rs`, and `permissions.rs`, `resources.rs` and `tasks.rs` |
| `capabilities/` | Beside the host: settings and profiles, grants and consent, the worker's `ModuleContext`, resource installs, and endpoint parsing (`endpoint.rs`, which the module parameter vocabulary, the descriptors, settings and every transport share) |
| `capabilities/secrets.rs` and `capabilities/transport.rs` | The two contracts the host is given rather than owns: the `SecretStore` trait, with the in-memory and unavailable stores, and the `Transport` trait that sends one checked request, with the unavailable transport. The core names no TLS, HTTP or Keychain crate, which `cargo xtask check-repository` enforces |
| `capabilities/document.rs` | The one JSON document store the settings, grants and installed-resource records and the artifact manifest share |
| `atomic_file.rs` | The crate's one durable file write, which the document store and the derived-artifact store write through, and its one disk flush (`flush`), which every durable write in the crate makes and which test builds skip ([tests skip the disk flush](../engineering/development.md#tests-skip-the-disk-flush)) |
| `jobs.rs` | Every job the catalog owner runs — source preparation, analysis, capability work, export and the catalog lanes' work — as a record in one table, which also runs the capability and export lanes ([jobs](modules-and-api.md#jobs)) |
| `catalog_types/` | The [catalog](catalog.md)'s shared shapes, which every lane codes against, one file per concept: `identity.rs` (file identities and signatures, volumes, catalog folders, collections, events, moments, library changes and the owner's 16-byte view item), `header.rs` (header metadata), `disk.rs` (volumes, cards, indexed folders and the index's file and root records), `organize.rs` (thresholds, events, moments and group layouts), `browse.rs` (view queries, summaries, rows, facets and the selection), `library.rs` (picks, targets, catalog folders, collections, the journal, developing picks, availability and missing originals), `previews.rs`, `jobs.rs` (each long-running catalog job's names) and `api.rs` (every catalog method's parameters, answer, envelope, job and error codes, declared once for the lane that registers it) |
| `index/` | The index of the files Luxforge browses (lane A): its database in `<catalog stem>.index/index.sqlite`, versioned on its own and discarded rather than refused, with its revision (`database.rs`); the mounted volumes, cards and offline folders (`volumes.rs`); what is listed and skipped (`exclude.rs`); the bounded walk (`walk.rs`); one file's header read (`read.rs`); reconciliation by signature (`reconcile.rs`); the index lane itself (`lane.rs`: one coordinator thread that walks, reconciles and writes batches on its own connection, and two header workers on different files, started on first use and asleep while idle); and a folder's subfolders for `disk.folders` (`disk.rs`). The owner only schedules the lane (`api/owner/files.rs`). Watchers and mount notifications are to come |
| `organize/` | Events, days, cameras and moments, pure functions of header metadata (lane A): the event rule (`events.rs`), event names (`names.rs`, with `Gazetteer`, the place names as organizing reads them), runs, bursts and brackets (`moments.rs`), and the bundled offline gazetteer, a k-d tree over GeoNames' populated places built on first use (`gazetteer.rs`) |
| `library/` | The library (lane C): `journal.rs`, the one path every library change records through, with generic undo and redo; `items.rs`, each journal item's value read and written; `targets.rs`, the files or photographs a method's targets name; `picks.rs`; `folders.rs` (catalog folders and moving photographs), `collections.rs` (collections, smart collections, groups and members) and `tree.rs` (what both share: name rules, sibling clashes, parents-first order, a planned change); `availability.rs` (checks, refusals naming why an original is absent, observations), `locate.rs` (checking, the streamed verification and the one relocation write) and `worker.rs` (lane C's worker thread, whose jobs `api/owner/library.rs` queues and commits, in parts through `JobContext::commit`); `missing.rs` (missing originals by source folder, relinking remembered verifications) and `missing/search.rs` (the bounded walk and verification of `source.find`). Developing picks, removal and batch jobs are to come |
| `previews/` | The preview lane and cache of browsed files and developed photographs (lane B): the cache's names, validity by signature, writes and shared byte budget, and the grid and loupe states and sizes other lanes read (`cache.rs`); a file's tiers from a JPEG original or a RAW's embedded images, upright, and the seam where a RAW with no usable preview is developed for a visible or look-ahead task (`extract.rs`); the priority queue, the failures and deferrals it remembers and its worker threads (`lane.rs`); the 100% region from the embedded full-size preview or the one development kept (`region.rs`); the region worker, one thread apart from the extraction workers, that answers region jobs and writes their answers (`regions.rs`); developed photographs' rendered tiers, planned on the owner and rendered off the editor's cache, their keys and stale rows (`rendered.rs`); the render worker, one thread apart from the others that renders one photograph at a time, and the discard of other renderer generations (`renders.rs`); photographs' rows and files, rendered tiers labelled approximate as their proxy renders are and camera previews, what is served and collected, and what is forgotten when a photograph leaves the catalog (`photos.rs`); a photograph's camera preview until its first render, on an extraction worker (`camera.rs`); and the brightness fingerprint each complete grid tier keeps, with the bracket check organizing measures a run through (`bracket.rs`); a cached preview decoded for a client to draw at the size it needs, on the client's own worker (`display.rs`, the desktop's Select grid's). Its owner side — jobs, view jobs, waking clients, `preview.read`, `preview.region` with its queue and answer files, and render jobs with their queue and following commits — is `api/owner/previews.rs`, `api/owner/previews/regions.rs` and `api/owner/previews/renders.rs` |
| `browse/` | Browse views, facets, selection and events (lane D), evaluated on the catalog owner: reading a source's compact columns from the index or the catalog (`candidates.rs`), a filter as per-item predicates (`filter.rs`), evaluation, ordering and the held item list (`view.rs`), windows (`rows.rs`), facet counts (`facets.rs`), the selection and its carry-over (`select.rs`), events and `event.list` with their cache (`events.rs`), and the preview-state seam to lane B (`previews.rs`) |
| `seed.rs` | Generated catalogs and indexes written in bulk, for `cargo xtask generate-catalog` and tests, through the same row writers |
| `api/owner/catalog.rs` and `files.rs`, `previews.rs`, `library.rs`, `views.rs` | The catalog lanes on the owner: each lane's owner-side state, the messages its workers post back, its handlers and its per-client clean-up, one file per lane |

### The transport's files

`luxforge-net` holds `HttpTransport` and the macOS Keychain store (`secrets.rs`, `keychain.rs`). `transport.rs` holds the redirects, bounds and refused framing, and `transport/` the rest: `agent.rs`, one `ureq` agent per request on the transport's own resolver, socket and TLS; `connect.rs`, resolving once and connecting only to checked addresses; `address.rs`, the addresses each endpoint class may reach; and `tls.rs`, rustls with the ring provider and the platform verifier. The transport's TLS, protocol and address tests live here, against `luxforge-testbase`'s test server with its `tls` feature.

### The JPEG codec

`luxforge-jpeg` holds the bounded marker walk and frame header, the decode and encode sessions with the safety around the C library (every call under `catch_unwind`, a failed session destroyed and never called again, the writer's I/O error kept, bounded segments), which libjpeg warnings refuse a decode (the table in `warnings.rs`: missing, corrupt or guessed data and any unlisted code refuse, harmless irregularities do not), and the ICC profile's APP2 chunks read and written, numbered from 1. It exposes a safe API and its own `JpegError`, which the core maps to its error kinds in one place. The core keeps the policy: the limits it passes in, EXIF orientation, which metadata to keep, the sRGB profile check and the export's quality, sampling, progress and cancellation.

Two decodes serve previews. `Decoder::set_scale` decodes a whole frame at 1/2, 1/4 or 1/8 on each side by libjpeg's reduced inverse DCT, and `Scale::covering` picks the most reducing scale whose output still covers a target size, so the resample after it only shrinks. `RegionDecoder` decodes one rectangle at full scale, streaming its rows like `Decoder`: libjpeg crops a window of whole iMCU columns around it (`jpeg_crop_scanline`), three left and one right of the rectangle because libjpeg takes a crop's edges for the frame's in its chroma upsampling and progressive block smoothing, and skips the rows above it (`jpeg_skip_scanlines`); its rows equal the same rectangle cut from a whole decode, byte for byte. It stops after the rectangle's last row, so warnings about data after what the rectangle needs (below it, or after the last scan of a single-scan file) are not seen. Both check the frame against the caller's limits at its full size: libjpeg's row buffers are a few dozen rows of the output (for a region, of the full width, allocated before the crop), and a multi-scan frame's coefficients are held for the whole frame at up to 6 bytes a pixel. A region allocates the caller's rectangle and one window row. `mozjpeg`'s safe API does not expose the session's `cinfo` that cropping and skipping need, so the region decode runs its own libjpeg session over `mozjpeg-sys` in `session.rs`, with the same error manager and warning table, libjpeg's memory source over the caller's bytes, and a guard that destroys the session on every path.

### The desktop's files

Paths are under `crates/luxforge-app/src`.

- `app/`: the Iced application, messages, update, owner tasks, evidence, keymap and the crop driver.
- `state/`: the pure view model, with no framework types, no widget crate and no view.
- `view/`: rendering, with no core types and no owner access, including the crop and mask canvases (`view/crop_canvas.rs` and `view/mask_canvas.rs`) and the one view transform and ellipse builder both draw through (`view/canvas_view.rs`).
- The Select workspace of the [catalog](catalog.md#workspaces) is one seam across the three layers: `app/select.rs` (with `app/message/select.rs`: the switch, the owner's view tasks, the synchronous `browse.select` and the grid's layout, scroll and viewport), `state/select.rs` (what Select last read, its rows window, and every region's model) and `view/select.rs` (the Select screen, and the workspace switch both title bars draw).
- `layout.rs`: the window's framework-free layout: the bar and panel sizes, the rules between them, the Fit inset and the photo surface they leave.
- `coalesce.rs`: the one "one request in flight, newest waiting" slot, which the pointer sample, the pan, the curve samples, the event sync and the Performance sampler share.
- `crop_draft.rs`: the crop frame's geometry, whose draft is a core draft like every other gesture's.
- `mask_draft.rs` and `mask_draft/`: the mask draft, with one shape editor per drawn kind.
- Native adapters and diagnostics.

### The test kit

Test support is split by whether it names a core type. `luxforge-testbase`, which depends on no workspace crate, holds, beside the gate, the wait and the distribution, the one loopback HTTP test server (plain, or TLS with the `tls` feature only `luxforge-net`'s tests ask for; bounded requests, a responder per test); the capability proof's fake provider `ProofEndpoint` built on it, which takes the proof module's side of the exchange (`ProofProtocol`: its paths, pinned palette and grid size) from its caller and which the core's tests answer in process through their in-memory transport; and `paths` (repository fixtures and unique scratch paths). `luxforge-testkit` holds what speaks core types: `client` (the in-process JSON client of the catalog owner that the field-patch conformance suite and xtask's acceptance chapters share); `JsonProcess` (a `luxforge-json` process driven over standard input and output, for `luxforge-cli`'s process tests); `fixtures` (synthetic byte and linear sources with content fingerprints, single-layer stacks, mutation envelopes, frames and samples through the render entry point, and the one code-threshold tolerance rule against an f64 reference); and `proof_protocol`, the proof endpoint's protocol from the core's constants. `luxforge-core` names only the base: a dev-dependency on the kit, which depends on the core, would build the core a second time for every core test build. So the core's unit tests use the base, building their proof protocol in `capabilities/testing.rs`, and each of its integration binaries compiles the kit's `client` and `fixtures` in from their one source through `#[path]` modules, naming itself `luxforge_testkit` (`extern crate self as luxforge_testkit`) so those helpers, and the conformance suite xtask compiles too, read the same there as everywhere else.

### Unsafe code

Only `luxforge-raw`, `luxforge-process` and `luxforge-jpeg` override the workspace's `forbid(unsafe_code)` in their manifests. `luxforge-jpeg` denies it and allows it by name only on the functions that need it, each block beside its `SAFETY:` comment: the one that reads libjpeg's warning code through the error manager's pointer (`decode.rs`), and those of the region decode's libjpeg session over `mozjpeg-sys` (`session.rs`: creating and destroying it, reading the header and the segments it saved, starting, cropping, skipping and reading rows), which exist because `mozjpeg`'s safe API cannot crop or skip.

## Boundaries

| Boundary | Responsibility |
| --- | --- |
| Desktop shell | Layout, focus, pointer capture, mapping gestures to semantic commands; displays authoritative state and holds none |
| Core service | Asset, layer and snapshot identity, shared invariants, atomic commits, revisions, request deduplication, history navigation |
| Catalog | Read-only source references and verified fingerprints; durable layers, snapshots, history, current/redo state and request results |
| Renderer and scheduler | Evaluate an immutable ordered stack from verified originals with bounded memory, cancellation and generation identity |
| Tool modules | One descriptor each (effects, actions, parameters, controls, optional canvas pick), input parsing, state validation and no-op detection, compilation of payloads into host processing primitives; [modules](modules-and-api.md) |
| Capability host | User-level module settings and provider profiles, secrets in the OS store, consent grants, the capability worker (verified resource installs, module tasks), the only network and user-file transport; [module capabilities](module-capabilities.md) |
| JSON/IPC, later MCP | Transport to the same catalog owner plus operation discovery; no alternate persistence or edit logic |

## Sources, layers and snapshots

### Sources

Import references a supported original and creates a stable asset plus Original after a bounded source worker has prepared and verified it. A JPEG Original has an empty recipe; a RAW Original has one required source-development layer. The asset records its JPEG/RAW kind and immutable RAW interpretation metadata.

Path and root are mutable locators; a full content fingerprint verifies the bytes. Same-filesystem aliases resolve to one asset, and identical copies at different paths are not merged. Missing or changed sources keep their edits and report an explicit rendering limitation.

### Layers

An edit layer is an identified, typed operation with parameters and an input/output stage contract. A recipe is the ordered stack of layers plus the masks those layers may reference. A history entry names a semantic action and stores its complete resulting immutable recipe. Optimizations may fuse operations only where the result stays byte-identical.

Each layer's coordinates refer to its input stage, and the host places a new layer by its effect stage ([content-space edits](content-space-edits.md), [orientation layer](orientation-layer.md)):

- Pixel-stage layers are inserted before the geometry tail of quarter-turns, reflections and crop, so they address the content stage (the source after EXIF orientation) and every later geometry change carries them.
- Geometry layers go before any finish layer, with the orientation always ahead of the crop, so the crop frames the turned photograph.

### Masks

A mask is a host object beside the layers — an ordered list of components with a whole-mask amount and inversion — and each layer carries at most one optional mask reference, so every entry's existing snapshot already stores the masks and there is no second persistence path. A mask's geometry is in content-stage coordinates, so only a layer before the geometry tail may carry one. A layer naming a mask its own snapshot does not hold is refused explicitly wherever the recipe is validated or compiled, and a component's kind and payload are retained unread exactly as an unknown effect payload is. See [masking](masking.md).

## Persistence

- Local SQLite holds current state, history entries with their snapshots, a monotonic revision, redo navigation and each request's whole answer (the [current catalog format](versions-and-lineage.md#storage-catalog-format-12)). The catalog owner is its only writer, in short atomic transactions; a failed write preserves the prior durable state. Originals and disposable pixel caches stay outside the database.
- Only the current catalog and payload shapes are supported: an unsupported format fails explicitly without rewriting data, and unknown payloads and missing providers are retained and reported, never dropped.
- Entry records are the only stored copy of a stack. Each entry's history row (sequence, action, label, actor, timestamp, undo parent and restore target) has its own columns, so a history page decodes no entry.
- Every entry is retained: undo and redo navigate without inverse rows, Restore copies a snapshot into a new action, and versions are named references to entries ([versions and lineage](versions-and-lineage.md)).
- Beside the entries the catalog holds the preset library ([presets](presets.md#library)), a content-addressed store of painted paths ([masking](masking.md#stroke-storage)) and each entry's references to derived artifacts, immutable content-addressed files in a `<catalog stem>.artifacts` directory that moves with the catalog ([derived artifacts](module-capabilities.md#derived-artifacts)).
- The catalog's index and preview cache live in a `<catalog stem>.index` directory beside it: `index.sqlite`, what Luxforge read from the files it browses, and `previews/`. It is a cache with a format of its own, opened on first use and discarded and recreated when it cannot be used, never touching the catalog ([catalog](catalog.md#the-index-and-previews-cache)).
- Module settings, grants, secrets and installed resources are user-level and never part of a catalog.
- The owner keeps the last 8 entries it read, with their strokes resolved and shared between clones, and the last 16 assets' heads, each updated where a write commits: a history move, or a library change that rewrites where an asset's original is (`source.locate`, a relink `source.check` makes, and their undo and redo), announced as that change's event. Reopening starts that cache empty and recovers the same IDs, current snapshot and navigation state.
- Backups need a consistent SQLite snapshot, not a copy of a live file.

## Rendering and limits

Every frame, sample, grid and preview phase enters rendering through one function, `luxforge_core::render`, which checks the source, compiles the recipe once for its phase (exact, or the proxy phase's thin-mask sampling) and returns a `Render` that answers the frame, a pixel, a grid, the output stage and its geometry from that compilation. The editor service compiles each stack it evaluates once, on the catalog owner, into the one bound evaluation (`Evaluation`) its preview, analysis, sample, point and export plans carry, and their workers render that compilation through the same `Render` rather than compiling it again.

What a render reads besides its source and recipe — the colour scratch budget, the spatial budget and the store of prepared spatial estimates, with their high-water marks — is a `RenderContext` passed in, not process state. The editor service owns one, and the catalog owner, the preview and analysis jobs it plans and the desktop's diagnostics share it, so their renders pace each other.

### Pixel domains

Orient once to upright content and give every buffer explicit colour meaning: 8-bit sRGB for the supported JPEG subset, or signed unbounded planar float32 linear sRGB/D65 for RAW. Decode once through a signature-validated cache and share immutable pixels. Exact buffers on synthetic fixtures prove correctness; JPEG re-encoding is not an oracle.

RAW sensor data stays in an immutable u16 mosaic. White balance redevelops that mosaic, while exposure and geometry reuse its float development. Crop and orientation views share those planes without a full-frame copy. The RAW renderer samples composed geometry and one crop directly into a terminal RGBA display buffer; it does not quantize an intermediate crop.

One pipeline, generic over its pixel domain, evaluates a compiled recipe: walking a point through the segments, the colour runs and replacements over a segment's rows, the resample recursion, and a spatial entry with its tiles and estimates exist once. The two domains differ only in what a pixel is between those steps and which frames their drivers materialize:

- The byte domain is 8-bit sRGB, quantized at every run end, replacement, resample and spatial output.
- The linear domain is `f64` from the RAW exposure and approximate white balance on and `f32` through a colour segment, quantized only at the terminal boundary.

A pointwise colour operation is not a stage boundary. It joins its segment's operation list, and the rasterizing pass streams it over that segment's rows in bounded chunks, on both paths, so no full-frame float buffer ever exists. The pass runs on the shared Rayon pool from its pass kind's measured threshold (`luxforge_raw::parallel_pixels`, counted over the rows the colour runs reach; see the [limits](#limits) and [performance rules](../engineering/performance-rules.md) rule 9). Each chunk reserves its float scratch from the render context's budget before it uses it and releases it afterwards; its buffer is allocated once per Rayon split (`try_for_each_init` calls its init once per split, typically tens of times per pass) and reused for every chunk of that split. The budget's 64 MiB is a target: a chunk holds at most 1 MiB, so one chunk per pool worker stays well inside it, and a chunk that finds it taken still runs, with the high-water mark showing the overshoot.

### Stage boundaries

A recipe compiles into at most one raster pass per segment, and point queries answer from the compiled geometry. Two operations separate segments:

- **A resample** (the crop module's non-zero-angle case). Each segment is an exact raster pass, so at most two full frames — one segment's output feeding the next resample's input — exist at once, each within the applicable JPEG 512 MiB or RAW 1.5 GiB per-buffer bound.
- **A spatial operation**, at the same dimensions as the stage it receives. It reads the finished frame before it and writes the next one in square tiles anchored at the stage origin — 512 px, or 1024 px once the operation's summed halo at its stage passes 128 px, so a wide halo is not recomputed around every small tile (`luxforge_raw::spatial_tile`, the one rule a render, a point sample and a windowed proxy ask). Each tile is read as the tile grown by the operation's summed halo and clamped to the stage.

The compiled `Entry` that produces a segment's input frame is the one place the kinds are told apart. Everything the renderer asks of a boundary is one of its methods, each a single dispatch over the kinds: the stage it produces and the rectangle of that stage its frame holds, the rectangle of the stage before it that it reads, a windowed proxy's plan and cut of it, one point mapped back through it (locate) or evaluated through it, its forward map, its frame in the byte driver and in a frame-mode evaluation, its estimates, how the linear rows load it, and whether a point query evaluates it in tiles. No caller matches on the kind, so a new kind of boundary, such as the [Corrections](corrections.md) proposal's repair stage, is one more variant with one arm in each of those methods.

Spatial tiles run in batches whose concurrency is what fits beside other evaluations in the render context's 256 MiB spatial target, reserved before each batch allocates, capped by the pool's workers and never less than one tile, so a render that finds the target taken slows rather than fails. The render's cancellation token is checked between batches. When the target holds a batch to fewer tiles than the pool has workers, each tile's own passes run their independent rows on the pool as well, so an operation whose working set allows two tiles at once still uses every core without taking more memory. Every value is the same arithmetic in the same order either way, so this changes no byte.

The RAW linear path writes only its last segment's rows and pulls what lies before them, since a linear value between two boundaries is an `f64` that the next resample blends: a bounded rectangle at a time with its colour run over rows, a spatial operation's input one row at a time and a straightened crop's taps one block of output pixels at a time. For a render there, each spatial operation's output, which is `f32`, is materialized once as three f32 planes inside the 1.5 GiB RAW planar limit, and nothing is quantized before the terminal boundary. Each is built from the one before it and replaces it, so at most two exist while one is built and one afterwards.

The rectangle a resample reads is one rule, `Resample::reads`, for the colour band before a crop, a windowed proxy's cut and the RAW driver's tap blocks, and both drivers materialize a spatial operation through one helper that resolves or reuses its estimates; `cargo xtask check-repository` refuses a second copy of either.

### Geometry beyond an exact orientation

The geometry tail is the exact orientation, then the crop, whose straightening is the one resample. The `ToolModule::carry` hook ([module trait](modules-and-api.md#module-trait)) lets a transform reposition a later geometry layer through an exact quarter turn or reflection, and nothing more: it rewrites a stored payload, it does not change how the host evaluates a stage. A geometry effect that is not affine over the whole stage, such as a perspective or lens warp, is a new host primitive, not a use of the hook, and a module must not fake one through it. That primitive would have to be carried through every place that today knows only exact mappings and the resample:

- `modules/processing.rs`, the `Processing` primitives a module compiles to;
- the output stage the registry compiles (`output_stage` in `modules/registry/compile.rs`);
- the point walk (`Evaluation::entry_pixel` in `render/pipeline.rs`);
- the byte driver and locate in `render.rs`;
- the linear driver (`render/linear.rs`) and the windowed proxy (`render/window.rs`).

The places that hold these move with the `render.rs` split, so the list follows wherever it puts them.

### Point queries

Point queries evaluate through a resample recursively and never allocate a frame. A point query through a spatial segment, on either path, evaluates the tile that contains its pixel, through the same tile function the render uses: the declared exception to point queries never rasterizing.

- The catalog owner only plans such a `render.sample`, in `O(layers)`, and one point worker thread evaluates it and answers on the caller's own reply channel in the order the samples were queued, while the owner serves other calls. A sample without a spatial layer is answered by the owner itself.
- It materializes no frame: through several spatial segments, a tile's halo reads the earlier segments' tiles from one cache per query, shared by both paths, so each tile is evaluated once per query. That cache holds at most the spatial target's bytes of tiles, charged to the budget, and releases the least recently read tile past it ([modules and API](modules-and-api.md)).
- A sample grid answers its points through the query's tile cache on both paths and materializes no frame.

### Preview scheduling

Preview work runs on one persistent worker with one active and one replaceable pending job, tagged with a generation, which takes the pending job itself the moment the active one ends. The histogram analysis and the desktop's clipping overlay run on the same latest-job primitive (`luxforge_core::latest`), each on its own thread. A newer request raises the active job's superseded token and a cancel or withdrawal its abandoned token, and each job decides which of the two its work stops on. A newer request supersedes refinement and exact analysis but does not abandon interactive region work.

- **At Fit and zoomed-out views**, a job renders the recipe against a display-bounded proxy and presents it; the exact full-resolution phase and whole-image report settle after the shared quiet policy or release.
- **At 100% and above**, motion renders the visible region at half linear resolution, then refines exact visible pixels and runs whole-frame analysis after the same policy. Matching full-image or region texture slots can serve a pan.
- **While an upload is deferred** and a requested photo has no drawable current pixels, the surface may draw one coherent prior photo, marked updating, with mismatched overlays suppressed. Once a current region draws, any viewport area it does not cover remains canvas background; regions of different recipe content are never mixed. A blank-photo diagnostic counts draws with no photo pixels at all, not partial viewport coverage.

Every colour, geometry and finish layer is resolution independent, so the proxy is an exact render of the recipe at its own scale. A spatial layer's neighbourhoods scale with the stage, so a stack holding one renders an approximate proxy that the result marks as such. A pixel-stage layer makes a stack ineligible and it takes the exact path. See [instant previews](instant-preview.md).

Each preview worker holds one proxy source, released before its replacement is built, and builds it through one band intermediate per pool worker, fitted to the display bounds in the [limits](#limits). A crop fits its output to the bounds, and the proxy holds only the window of that proxy stage the crop reads: its output for a straight crop, the box its taps read plus 2 px for a straightened one, and under a spatial layer that grown by the operation's summed halo with its origin on the operation's own tile grid. So its size follows the display bounds and not the crop's tightness: 29 MiB of planes for a 1801 × 1574 crop of the X100VI in 1716 × 1576 bounds, 71 MiB under Presence measured on the 512 px grid, where Presence now runs in 1024 px tiles and its window may start up to 512 px further left and up, against 414 MiB for the whole proxy stage.

### Limits

Every buffer and queue has a limit, and exceeding one fails with `resource-limit`. A row marked *target* is a shared working-memory budget instead: it paces the work and never refuses it ([decisions](../decisions.md#rendering-memory)). The rules and review checklist are in [performance rules](../engineering/performance-rules.md). Each figure below is enforced by the named constant; where no single constant enforces it, the row says so.

RAW has its own approved admission contract, the RAW rows of the first table; JPEG keeps the evaluated-frame limit beside the colour scratch target.

**Frames and sources**

| Limit | Figure | Enforced by |
| --- | --- | --- |
| Evaluated RGBA8 frame (the JPEG path's frames, a proxy, a linear-to-byte conversion), per buffer | 512 MiB | `MAX_FRAME_BYTES`, `crates/luxforge-raw/src/limits.rs` |
| RAW encoded source | 512 MiB | `MAX_SOURCE_BYTES`, `crates/luxforge-raw/src/limits.rs` |
| RAW sensor pixels | 128 million | `MAX_PIXELS`, `crates/luxforge-raw/src/limits.rs` |
| RAW side | 16384 px | `MAX_SIDE`, `crates/luxforge-raw/src/limits.rs` |
| RAW planar RGB float allocation, per buffer | 1.5 GiB | `MAX_RGB_BYTES` (1536 MiB), `crates/luxforge-raw/src/limits.rs` |
| A retained second RAW development | 600 MiB of planes | `RETAINED_DEVELOPMENT_BYTES`, `crates/luxforge-raw/src/limits.rs` |
| LibRaw's native scratch | 512 MiB | No named constant: the literal `max_raw_memory_mb = 512` in `crates/luxforge-raw/native/adapter.cpp` |
| One embedded RAW preview extracted, LibRaw's buffer and the copy returned each (the caller's own limit goes below it) | 64 MiB | `MAX_EMBEDDED_IMAGE_BYTES`, `crates/luxforge-raw/src/limits.rs` |
| Bytes an embedded-preview handle reads from its source over its life (the caller's own budget goes below it) | 128 MiB | `MAX_EMBEDDED_READ_BUDGET`, `crates/luxforge-raw/src/limits.rs` |
| An embedded-preview handle's read cache | 8 blocks of 16 KiB | `READ_BLOCKS` and `READ_BLOCK`, `crates/luxforge-raw/src/embedded.rs` |

**Rendering**

| Limit | Figure | Enforced by |
| --- | --- | --- |
| Colour scratch, aggregate (*target*) | 64 MiB | `DEFAULT_SCRATCH_BYTES`, `crates/luxforge-core/src/render/context.rs` |
| One colour chunk's scratch | 1 MiB | `COLOR_CHUNK_SCRATCH_BYTES`, `crates/luxforge-core/src/render/colour_runs.rs` |
| Pointwise units per colour operation | 8 | `MAX_COLOR_UNITS`, `crates/luxforge-core/src/modules/processing.rs` |
| Spatial tile working sets, aggregate (*target*) | 256 MiB | `SPATIAL_BUDGET_BYTES`, `crates/luxforge-core/src/modules/spatial.rs` |
| Spatial units per spatial operation | 4 | `MAX_SPATIAL_UNITS`, `crates/luxforge-core/src/modules/spatial.rs` |
| Summed halo per spatial operation | 512 px | `MAX_SPATIAL_HALO`, `crates/luxforge-core/src/modules/spatial.rs` |
| Spatial tile side | 512 px up to a 128 px summed halo, 1024 px past it | `SPATIAL_TILE`, `SPATIAL_WIDE_TILE` and `SPATIAL_WIDE_HALO`, read through `spatial_tile`, `crates/luxforge-raw/src/limits.rs` |
| Global estimate | 4 KiB each | `MAX_GLOBAL_BYTES`, `crates/luxforge-core/src/modules/spatial.rs` |
| Cached estimates | 8 | `ESTIMATE_STORE_ENTRIES`, `crates/luxforge-core/src/modules/spatial.rs` |
| One point query's held spatial tiles | The spatial target's bytes (85 tiles of 3 MiB at 512 px, 21 of 12 MiB at 1024 px), never fewer than 16 | `SPATIAL_BUDGET_BYTES` over the largest tile's planes, floored at `POINT_TILES_FLOOR`, `crates/luxforge-core/src/render/spatial.rs` |
| Parallel threshold: a segment's geometry | 0.5 MP | `PARALLEL_TRANSFORM_PIXELS`, read through `parallel_pixels`, `crates/luxforge-raw/src/limits.rs` |
| Parallel threshold: one or two colour units | 0.1 MP | `PARALLEL_COLOUR_PIXELS`, as above |
| Parallel threshold: three or more colour units, or a mask | 25,000 pixels | `PARALLEL_HEAVY_COLOUR_PIXELS`, as above |
| Parallel threshold: a resample | 0.1 MP of output | `PARALLEL_RESAMPLE_PIXELS`, as above |
| Parallel threshold: a spatial operation's tiles | 0.25 MP of stage | `PARALLEL_SPATIAL_PIXELS`, as above |
| Parallel threshold: a proxy's box downscale | 0.5 MP of source read | `PARALLEL_PROXY_PIXELS`, as above |
| Parallel threshold: passes that are not rendering passes | 1 MP | `PARALLEL_PIXELS`, `crates/luxforge-raw/src/limits.rs` |

**Preview**

| Limit | Figure | Enforced by |
| --- | --- | --- |
| Proxy sources per preview worker | 1, released before its replacement is built | No constant: the worker's one `ProxyCache` (`crates/luxforge-core/src/proxy.rs`), held in `crates/luxforge-core/src/preview/queue.rs` |
| A proxy build's band intermediate | About 1 MiB per pool worker | `BAND_BYTES`, `crates/luxforge-core/src/proxy.rs` |
| Display bounds | 4096 px per side and 8 megapixels | `ProxyBounds::MAX_SIDE` and `ProxyBounds::MAX_PIXELS`, `crates/luxforge-core/src/proxy.rs` |
| A proxy without a crop | 64 MiB for a JPEG, 96 MiB of RAW planes | No single constant: follows from the display bounds and the fit rule, `ProxyPlan::fit` in `crates/luxforge-core/src/proxy.rs` |
| Finished results waiting for their consumer, per latest-job worker | 2 | `WAITING_RESULTS`, `crates/luxforge-core/src/latest.rs` |
| A 100% region's pixels | 32 MiB of RGBA8 | `MAX_REGION_BYTES`, `crates/luxforge-core/src/previews/region.rs` |
| Bytes a 100% region reads from a RAW to list its previews and extract one | 64 MiB | `REGION_READ_BUDGET`, as above |
| The embedded JPEG a 100% region extracts, LibRaw's buffer and the copy each | 48 MiB | `REGION_PREVIEW_BYTES`, as above |
| A turned region's decode strip | 16 rows of the stored rectangle | `REGION_STRIP_ROWS`, as above |
| RAW developments for the 100% region and the developed tiers, process-wide | 1 running, 4 callers waiting; the preview lane's three threads are its only callers, so none is refused (a compile-time assertion in `lane.rs`) | The one slot `DEVELOPMENTS` and `MAX_DEVELOPMENT_WAITERS`, as above |
| Developed frames kept | 1, upright RGBA8 within the evaluated-frame limit, released before another is developed | The one slot `DEVELOPMENTS`, as above |
| A developed preview's long edge | 2560 px | `MAX_DEVELOPED_PREVIEW_SIDE`, as above |
| A rendered tier of a developed photograph, long edge | 512 px grid, 2048 px large | `PHOTO_GRID_SIDE` and `PHOTO_LARGE_SIDE`, `crates/luxforge-core/src/catalog_types/previews.rs` |
| Originals one rendered-preview render prepares | 1, for every tier it makes, held by the render alone and never adopted by the editor | No constant: `render` in `crates/luxforge-core/src/previews/rendered.rs` prepares once and is synchronous on its worker |
| Render worker threads | 1, started on a photograph's first render request, blocked on its channel while idle, one render at a time, so one preparation at a time | `RenderWorker`, `crates/luxforge-core/src/previews/renders.rs` |
| Renders waiting for the render worker, each one photograph at one entry with the tiers wanted of it | 2,000: ten screens of the smallest grid cells ahead of the worker | `RENDER_QUEUE_CAPACITY`, as above |
| Rows of other renderer generations one discard page removes | 256, in one short write transaction, on a thread that ends when none is left | `DISCARD_PAGE`, `crates/luxforge-core/src/previews/photos.rs` |
| Photographs whose preview rows one transaction deletes when they leave the catalog | 1,000, so one library change's 50,000 take 50 short transactions on the owner; their files are removed on a thread that ends when it has removed them | `FORGET_PAGE` and `remove_files`, as above |

**Catalog and API**

| Limit | Figure | Enforced by |
| --- | --- | --- |
| History rows per page | 100 | `MAX_HISTORY_PAGE`, `crates/luxforge-core/src/editor/history.rs` |
| Assets per `catalog.list` page | 500, 100 when the request names no `limit`; a page reads its rows' own columns and decodes no source interpretation | `MAX_ASSET_PAGE` and `DEFAULT_ASSET_PAGE`, `crates/luxforge-core/src/editor/catalog.rs` |
| Assets one client previews the history of at once | 16 | `MAX_SELECTIONS`, `crates/luxforge-core/src/preview.rs` |
| Hydrated entries the owner caches | 8 | `CACHED_ENTRIES`, `crates/luxforge-core/src/editor/entries.rs` |
| Asset heads the owner caches | 16 | `CACHED_HEADS`, `crates/luxforge-core/src/editor/entries.rs` |
| Live clients | 8 | `MAX_CLIENTS`, `crates/luxforge-core/src/api/transport.rs` |
| Request line | 1 MiB | `MAX_REQUEST_BYTES`, `crates/luxforge-core/src/api/transport.rs` |
| Point samples waiting behind the one being evaluated | 9 (the live clients and the desktop's one in flight) | `POINT_QUEUE_CAPACITY` (`MAX_CLIENTS + 1`), `crates/luxforge-core/src/api/owner/point.rs` |
| Buffered events | 256 | `EVENT_CAPACITY`, `crates/luxforge-core/src/api/owner.rs` |
| Items one library change covers, and targets one library method names (provisional, to be set by measuring the owner transaction) | 50,000 | `MAX_LIBRARY_BATCH`, `crates/luxforge-core/src/catalog_types/library.rs` |
| Library changes per `library.journal` page, and picks per `pick.list` page | 500 (100 when the request names no `limit`), and 5,000 (1,000) | `MAX_JOURNAL_PAGE` and `MAX_PICK_PAGE`, `crates/luxforge-core/src/catalog_types/library.rs` |
| The library lane (lane C's worker: availability checks and Locate) | One running and four queued jobs | One thread (`crates/luxforge-core/src/library/worker.rs`) and `LANE_QUEUE`, `crates/luxforge-core/src/jobs.rs` |
| A verification's read buffer (Locate, and a check confirming a moved original) | 1 MiB, one file at a time | `CHUNK`, `crates/luxforge-core/src/library/locate.rs` |
| A `source.find` walk | 500,000 files, 100,000 folders, 64 levels | `MAX_SEARCH_FILES`, `MAX_SEARCH_FOLDERS` and `MAX_SEARCH_DEPTH`, `crates/luxforge-core/src/library/missing/search.rs` |
| Verified files `source.relink` may commit, remembered by the owner | 100,000, oldest forgotten first | `MAX_REMEMBERED_FILES` (twice `MAX_LIBRARY_BATCH`), `crates/luxforge-core/src/library/missing.rs` |
| Activity entries | 64 active and 16 recent | `MAX_ACTIVE` and `MAX_RECENT`, `crates/luxforge-core/src/activity.rs` |
| Items one browse view's source may hold; the owner keeps 16 bytes per item of each client's view | 1,000,000 | `MAX_VIEW_ITEMS`, `crates/luxforge-core/src/catalog_types/browse.rs` |
| Rows per `browse.rows` window | 1000 | `MAX_VIEW_ROWS`, `crates/luxforge-core/src/catalog_types/browse.rs` |
| Items one `browse.select` names | 50,000 | `MAX_LIBRARY_BATCH`, `crates/luxforge-core/src/catalog_types/library.rs` |
| Event computations the owner caches, each its events' summaries and file rows | 2, keyed by index revision and event thresholds; each bounded by the index | `CACHED`, `crates/luxforge-core/src/browse/events.rs` |
| A histogram `Report`, before protocol encoding | 16 KiB | `REPORT_BOUND_BYTES`, `crates/luxforge-core/src/analysis.rs` |
| View rows the desktop's Select workspace holds | 16 blocks of 200, the blocks nearest the screen | `ROW_BLOCKS_KEPT` and `ROW_BLOCK`, `crates/luxforge-app/src/state/select.rs` |
| Decoded grid previews the desktop's Select workspace holds | 192 MiB of RGBA8 and 4,096 handles, the least recently wanted out first and never a cell on screen | `DECODED_BUDGET_BYTES` and `MAX_HANDLES`, `crates/luxforge-app/src/app/select_previews.rs` |
| The Select grid's preview reads and decodes | 64 `preview.read`s to one owner task, one task at a time; 512 decodes a plan on one worker, 8 decoded previews waiting for the update loop, each at most 1024 px a side | `READ_BATCH`, `MAX_PLAN`, `DECODED_WAITING` and `MAX_SIDE`, as above |

**The index lane** (`crates/luxforge-core/src/index/`, `crates/luxforge-core/src/api/owner/files.rs`)

| Limit | Figure | Enforced by |
| --- | --- | --- |
| Supported files one listing takes | 500,000, then `resource-limit` | `MAX_INDEX_FILES`, `walk.rs` |
| Folders one listing visits | 100,000, then `resource-limit` | `MAX_WALK_FOLDERS`, `walk.rs` |
| How deep below its root a listing goes | 64 folders; deeper ones are not entered | `MAX_WALK_DEPTH`, `walk.rs` |
| Header workers | 2, each reading one file at a time | `HEADER_WORKERS`, `lane.rs` |
| Header reads queued or being read | 32 | `HEADERS_IN_FLIGHT`, `lane.rs` |
| Writes one batch transaction holds, and how long a batch collects them | 512; 500 ms | `INDEX_BATCH` and `BATCH_INTERVAL`, `lane.rs` |
| Listings waiting behind the running one | 16, then `resource-limit` | `MAX_WAITING`, `api/owner/files.rs` |
| Subfolders `disk.folders` answers, and the directory entries it reads | 2,000 from 50,000, then `truncated` | `MAX_DISK_FOLDERS` and `MAX_DISK_ENTRIES`, `disk.rs` |
| Camera bodies `card.list` names per card | 16 | `MAX_CARD_CAMERAS`, `api/owner/files/volumes.rs` |

**Header reads** (a photo file's capture metadata for indexing, `crates/luxforge-core/src/export/metadata/header/`). A read past its budget is not an error: it returns the fields read so far and says it was capped.

| Limit | Figure | Enforced by |
| --- | --- | --- |
| The first read of a file | 64 KiB, or the whole file when shorter | `HEADER_HEAD_BYTES`, `mod.rs` |
| Every later read | The 4 KiB-aligned chunks around a range the container names, each read once and kept | `CHUNK_BYTES`, `mod.rs` |
| One file's header read | 256 KiB and 16 reads, head included (the authentic corpus needs at most 84 KiB and 5) | `MAX_HEADER_BYTES` and `MAX_HEADER_READS`, `mod.rs` |
| Entries of one IFD; one text value | 256 (a larger table is not read); 256 bytes | `MAX_IFD_ENTRIES` and `MAX_TEXT_BYTES`, `tiff.rs` |
| IFDs one file's walk visits; IFD chain and SubIFDs followed | 24; 8 each | `MAX_IFDS` and `MAX_CHAIN`, `container.rs` |
| JPEG segments before the frame header; bytes scanned for one marker | 64; 1 KiB | `MAX_JPEG_SEGMENTS` and `MAX_MARKER_SCAN`, `container.rs` |
| ISO-BMFF boxes per level; RAF directory records | 64; 64 | `MAX_BOXES` and `MAX_RAF_RECORDS`, `container.rs` |
| A located thumbnail | A JPEG of at most 512 KiB, or uncompressed RGB of at most 1024 px a side | `MAX_THUMBNAIL_JPEG_BYTES` and `MAX_THUMBNAIL_SIDE`, `mod.rs` |

**Catalog previews** (the preview lane of browsed files and developed photographs)

| Limit | Figure | Enforced by |
| --- | --- | --- |
| Preview tasks waiting, each one file's tier or a developed photograph's camera preview | 20,000: the design's 10,000 files in view, two tiers each; a view's camera previews take the room its files leave | `PREVIEW_QUEUE_CAPACITY`, `crates/luxforge-core/src/previews/lane.rs` |
| Preview worker threads | 2, started on the first task, each blocked on its own channel while idle | `PREVIEW_WORKERS`, `crates/luxforge-core/src/previews/lane.rs` |
| Failures remembered, in memory, per file's tier and signature | 20,000, the oldest forgotten first | `REMEMBERED_FAILURES`, `crates/luxforge-core/src/previews/lane.rs` |
| Deferred developments remembered, in memory, per file's tier and signature | 20,000, the oldest forgotten first | `REMEMBERED_FAILURES`, a second instance, as above |
| Region worker threads | 1, started on the first `preview.region`, blocked on its channel while idle, one region at a time | `RegionWorker`, `crates/luxforge-core/src/previews/regions.rs` |
| Regions waiting for the region worker | 9, one per client (a client's next region cancels its last): the live clients and the desktop | `REGION_QUEUE_CAPACITY` (`MAX_CLIENTS + 1`), `crates/luxforge-core/src/api/owner/previews/regions.rs` |
| Region answer files | One per client, valid until its next region, removed when it disconnects; the directory cleared when the region worker starts | `Regions::answers`, as above, and `regions.rs` |
| Loupe and large tiers on disk, together (grid tiers are kept), a large tier rendered or a camera preview | 4 GiB, least recently used out, never the one just written | `SHARED_PREVIEW_BUDGET_BYTES`, `crates/luxforge-core/src/catalog_types/previews.rs`, a parameter of the lane |
| A tier's long edge | 512 px for a grid tier, 2560 px for a loupe tier, never enlarged | `FILE_GRID_SIDE`, `crates/luxforge-core/src/previews/mod.rs`, and `LOUPE_MAX_SIDE`, `crates/luxforge-core/src/catalog_types/previews.rs` |
| A JPEG original's EXIF thumbnail read | 64 KiB | `MAX_THUMBNAIL_BYTES`, `crates/luxforge-core/src/previews/extract.rs` |
| A cached preview a client decodes to draw (`decode_preview`) | 16 MiB of JPEG, decoded within the JPEG original's limits | `MAX_PREVIEW_FILE_BYTES`, `crates/luxforge-core/src/previews/display.rs` |
| A grid tier's brightness fingerprint, in the index and in a view's bracket probe while it groups | 581 bytes a file; the probe at most the view's files | `FINGERPRINT_BYTES`, `crates/luxforge-core/src/previews/bracket.rs` |
| One task's decoded frame | The JPEG original's limits (64 MP), checked at the frame's full size, decoded at the DCT scale that covers the tier | `JPEG_LIMITS`, `crates/luxforge-core/src/source.rs`, with `Scale::covering` |
| One task's reads of a RAW, and its one extraction | 128 MiB of reads, one image of 64 MiB | `MAX_EMBEDDED_READ_BUDGET` and `MAX_EMBEDDED_IMAGE_BYTES`, `crates/luxforge-raw/src/limits.rs` |

**Masks** (the delivered mask data model)

| Limit | Figure | Enforced by |
| --- | --- | --- |
| Masks per recipe | 16 | `MASKS_PER_RECIPE`, `crates/luxforge-core/src/model.rs` |
| Components per mask | 32 | `COMPONENTS_PER_MASK`, `crates/luxforge-core/src/model.rs` |
| Serialized mask bytes per recipe | 256 KiB | `MASK_BYTES_PER_RECIPE`, `crates/luxforge-core/src/model.rs` |

**Export**

| Limit | Figure | Enforced by |
| --- | --- | --- |
| The export lane | One running and four queued jobs | One thread per lane (`Lane::Export`) and `LANE_QUEUE`, `crates/luxforge-core/src/jobs.rs` |
| Retained finished export jobs | 32 | `FINISHED_RECORDS`, `crates/luxforge-core/src/jobs.rs` |
| One export's pixels | One exact frame inside the evaluated-frame limit, its encoded bytes streamed to a temporary file | `MAX_FRAME_BYTES`, `crates/luxforge-raw/src/limits.rs` |
| Names read when suggesting a destination | 64 | `MAX_PROBES` in `suggest`, `crates/luxforge-core/src/export/publish.rs` |

**Module capabilities**

| Limit | Figure | Enforced by |
| --- | --- | --- |
| Derived artifact | 256 MiB each | `MAX_ARTIFACT_BYTES`, `crates/luxforge-core/src/artifacts/mod.rs` |
| Derived artifacts per layer | 16 | `MAX_LAYER_ARTIFACTS`, `crates/luxforge-core/src/artifacts/mod.rs` |
| Prepared-artifact cache | 256 MiB | `PREPARED_ARTIFACT_BYTES`, `crates/luxforge-core/src/artifacts/mod.rs` |
| Capability worker lanes | Two, each of one running and four queued jobs | One thread per lane (`Lane::Transfer`, `Lane::Module`) and `LANE_QUEUE`, `crates/luxforge-core/src/jobs.rs` |
| Retained finished capability jobs | 32 | `FINISHED_RECORDS`, `crates/luxforge-core/src/jobs.rs` |
| Settings file | 1 MiB | `MAX_SETTINGS_BYTES`, `crates/luxforge-core/src/capabilities/settings.rs` |
| Grants file | 2 MiB and 1024 grant records | `MAX_GRANTS_BYTES` and `MAX_GRANT_RECORDS`, `crates/luxforge-core/src/capabilities/grants.rs` |
| Adapter request and response | 256 MiB each | `MAX_ADAPTER_BYTES`, `crates/luxforge-core/src/capabilities/descriptor.rs` |
| Resource quota | 16 GiB | `DEFAULT_RESOURCE_QUOTA_BYTES`, `crates/luxforge-core/src/capabilities/resources.rs` |

**RAW memory.** The 1.5 GiB value is per buffer, not a process RSS limit; LibRaw's native 512 MiB scratch ceiling, retained mosaics, concurrent previews, GPU/display allocations and editor liveness still require separate accounting. All 100 selected models have authentic adapter evidence; representative editor memory is recorded in the [resource ledger](modern-camera-resource-ledger.md).

The editor keeps at most two finished developments of the open RAW: its current one and, when its planes fit 600 MiB, the most recently used development at another white balance, so switching between two entries redevelops neither. The source worker's memory gate lets a redevelopment start beside that one retained development and waits for every other ([second development](raw-integration.md#second-development)). With both held, sampled steady-state process RSS is 2290 / 2352 MiB p50 / p95 on the X100VI (468 MiB of planes, 458 MiB above one development) and 1527 / 1559 MiB on the Z6 (264 MiB above), recorded in the [performance spec](../specs/performance.md#second-development).

## Histogram and analysis

`luxforge_core::analysis::reduce` is the standalone exact RGB histogram reducer: three 256-bin per-channel counts plus the output endpoint counters over an already-rendered byte raster, needing no host, job or module change. It reads the raster in place (no copy, no full-size mask). It reduces serially below the one-megapixel threshold the passes that are not rendering passes share (`luxforge_raw::PARALLEL_PIXELS`) and on the shared Rayon pool above it, using bounded, worker-local bins merged by addition, so its own allocation stays a fixed handful of kilobytes per worker rather than growing with the image.

`analysis::clip_class` is the one shadow/highlight/both predicate the counters and every later clipping overlay share. A `Report` is bounded to 16 KiB before protocol encoding (see the [Basic and histogram histogram and clipping contract](basic-and-histogram.md#histogram-and-clipping-contract)).

## Agent contract

One typed service backs the desktop and external JSON sessions. While the GUI is open it owns the catalog and accepts authenticated same-user loopback clients; headless ownership is allowed when it is absent. MCP later adapts the same registry.

- **Sessions.** The owner holds each registered client's session and reports a session revision with every session-returning response. Reconnect reads fresh state rather than replaying. One client's disconnect does not cancel another's jobs. A session's history selection is kept per asset, so previewing one photo's history neither pauses edits to another nor answers another's questions ([sessions](versions-and-lineage.md#sessions-live-with-the-owner)).
- **Commands.** Commands carry schemas, units, ranges, defaults and structured errors. Every method in `schema.list`, the host's own and the generated `edit.*`, `query.*`, `mask.*` and `task.*` methods alike, lists typed `parameters` in the one parameter vocabulary module descriptors use: identity kinds for the host's own objects (asset, entry, draft, job, preset, mask, component), integer and number ranges, enumerations, bounded strings, `text` for a file's contents or a path, and one `json` kind for a structured field whose notes give its shape. A host method checks each field against its declared kind where the request is parsed, so a schema generated from the registry, such as an MCP tool's, needs no hand-written copy. Mutations require an expected revision and a request ID with a documented deduplication scope. `version.create`, `version.delete`, `version.list` and `history.lineage` expose named states and the undo-parent chain.
- **Authority.** A client registers with edit authority, or with permission authority when it is the desktop's own client or `luxforge-json --permission-authority`. Only permission authority may grant module consent, so a live-session client can never grant itself access to files, networks or photos.
- **Jobs and activity.** Every job, of every kind, is read and cancelled through one job API, `job.read` and `job.cancel`. Long-running work (preparing an original, developing a RAW, rendering a preview, measuring a histogram, exporting a JPEG, reading or collecting derived artifacts) is published to the owner's activity board, which `activity.list` reads, and `resources.read` reports the process's CPU, memory and GPU counters; neither is history or an event ([performance panel](performance-panel.md)). Diagnostics never go to protocol stdout.
- **Change notification.** `events.since` reads the bounded event log. Each event names the method and request that made the change and, when it has them, the asset and the revision it left; a library change (a pick, a catalog folder or collection change, a Locate, an undo or redo of one) is one event naming its journal sequence as `library_sequence`, however many items it covered, and names an asset, with no revision, only when it moved exactly that one photograph's original; a batch the index lane committed is one event naming the index revision it left as `index_revision`, under the request that started the listing; so no batch overruns the log. `events.wait {after, timeout_ms?, asset_id?}` is its long poll. The handler answers at once when the log already has something for the cursor (an event past `after`, one naming `asset_id` when given, or a gap) or the timeout is 0; otherwise the owner parks the call's reply channel, the way it parks `wait_source`, and answers with `events.since`'s shape when a message that recorded events makes the log answer it, or with no events when the deadline passes. The owner never blocks on a wait: with none held it sleeps on a plain receive, and with some it sleeps on its channel's receive timeout set to the soonest deadline, so no timer thread or polling loop exists, and it answers every due wait before reading its next message. `timeout_ms` is at most 30 s, so a held wait is short-lived state. A client holds at most one: a second answers the earlier at once with what the log has for it, and a disconnect drops it. Both transports (the loopback listener and the stdio command) serve one request at a time per connection, so a connection blocked in a wait carries no other request; a client that wants to keep working while it waits opens a second connection.
- **Preview and session state are not history.** An external commit updates current state while a selected historical snapshot stays selected, and M4 drafts stay intact and marked conflicted until explicitly resolved.

## Modules and extension path

The nine built-in modules — presets, pixel, RAW, Basic, presence, mixer, transform, crop and vignette, in the order `ModuleRegistry::builtin()` registers them (see [modules and API](modules-and-api.md#registry)) — are linked modules; each declares its current effect identities, payloads and history actions. The crop module adds a `number` parameter kind and the `crop-frame` canvas interaction to the same descriptor shape, and updates its one crop layer in place through `ActionPlan::Update` rather than always appending; the transform module composes its four actions into one orientation layer ahead of the crop the same way, carrying every geometry layer after it, the crop today, through the transform in the same entry by asking each module's `carry` hook.

The host generates `edit.<action>` API methods and the desktop generates controls from the same descriptors, so a module capability cannot exist without an API. Unknown or unavailable effects stay in every snapshot and fail rendering explicitly. Linked built-ins with lazy resources are enough for M4. External loading comes later around a selected use case with measured costs; see [modules](modules-and-api.md).
