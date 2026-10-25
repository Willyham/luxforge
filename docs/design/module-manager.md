# Module manager

Status: **planned, not authorized.** The owner decided the scope on 2026-10-25 ([decisions](../decisions.md#module-manager)); the [task plan](../../tasks/programmability/modules.json) is validated and awaits authorization. Nothing here is implemented.

The module manager lets a person decide which modules their workspace offers and where, and later install modules that other people wrote. Phase 1 works on the built-in modules: turn one off or on, and reorder the tools panel's sections and the mode strip's tools. Phase 2 loads separately authored **adjustment** modules, each a sandboxed WebAssembly component with its own WGSL program, installed from a package file. A community library to browse and publish modules is the stated direction but is not planned: there is no index yet.

Boards: [module-manager/board.html](module-manager/board.html), with notes per board in [module-manager/screens.md](module-manager/screens.md). Where the boards and this page differ, this page is the contract.

## Decided

| Question | Decision |
| --- | --- |
| What phase 1 arranges | Any built-in module can be turned off or on unless it is locked. The tools panel's sections and the mode strip's module tools can be reordered. Corrections and mask-kind contributions follow their module's switch and keep their fixed order |
| How external modules run | A WebAssembly sandbox for the module's logic and its CPU reference, plus a WGSL program that the host validates and qualifies at first use |
| The first external kind | An adjustment section: a processing module with its own controls in the tools panel. Canvas tools, mask kinds and correction kinds come later and are not planned |
| How modules are published | No index yet. A module is installed from a package file the person chooses. In developer mode a module author can also load an unpacked folder |
| Price | Free. There are no paid modules, accounts or ratings |

## Terms

- **Toolset**: where a module's interface lives. `adjustments` is the tools panel's sections. `corrections` is the Corrections band's kinds. `masking` is the New mask and Add component kinds. `tools` is the mode strip's canvas takeovers. `processing` is a module with no interface of its own, such as the RAW development.
- **Placement**: one module's entry in one toolset. The core derives placements from what the descriptor already declares: a drawn section is an `adjustments` placement, a `crop-frame` or `brush-mask` takeover canvas is a `tools` placement, a `fill-` task or object pick is a `corrections` placement, and object selection is a `masking` placement. Crop has two placements: its section and the strip's R.
- **Home**: the toolset a module declares in its new `toolset` descriptor field. Settings › Modules lists the module and its switch there.
- **Off**: the person's choice, stored outside every catalog. An off module offers no section, strip entry, kind, palette entry, letter or chord in the workspace. It stays registered, so every photo that uses it renders, samples and exports exactly as before, and its API methods stay callable. Off is not **unavailable**: unavailable means the provider is missing, and rendering refuses ([missing effects](modules-and-api.md#missing-effects)).
- **Locked**: always on. A locked module can still be moved. Locked modules are Crop, transform, straighten (every photo's geometry), Basic (it carries the RAW development's white balance and exposure), the RAW development (processing only) and the host's Mask entry and mask kinds. A descriptor declares `locked` with its reason; the host owns Mask's lock.
- **Origin**: `built-in`, `community` (installed from a package file) or `folder` (loaded from a folder in developer mode).

## Phase 1: arranging the built-in modules

### Rules

- **Presentation only.** The layout changes what a client draws and offers, never what the host accepts or how anything renders. Processing order stays each effect's declared stage and order, and the registry keeps its order. That order also decides the Original's layers, first-open actions, the GPU warm list and which provider serves Corrections, so reordering the registry itself is not allowed ([registry](modules-and-api.md#registry)).
- **One layout per person.** It is stored in `preferences.json` as `module_layout`, outside every catalog, with no history entries. Only explicit choices are stored: a module never turned off has no entry, and a toolset never reordered follows registry order. An automated launch keeps it inside its evidence directory.
- **Nothing is discarded.** An id the layout names that this run does not register stays stored and is listed as unrecognized: an uninstalled module, a module this build no longer has, or a developer module outside developer mode. A newly registered module appears at its default place, after the registry neighbour that precedes it in the stored order, until the person moves it.
- **Edits are never hidden.** A photo with a non-neutral layer from an off module draws that module's band at its place, marked **Off · edit kept**. Expanded, the band lists the values read-only and offers **Enable *Title***. The band is in the neutral grey, not the unavailable red, because the picture is whole. A neutral layer brings no band back.
- **Everything else is unchanged.** Presets, copy and paste settings, Auto tone, first-open actions and agents act on an off module exactly as before. The status bar names what changed (`Turned off Vignette · rendering unchanged`).

### Descriptor and API

- `ModuleDescriptor` gains `toolset` (required: the module's home) and `locked` (optional: `{reason}`). Every built-in declares its toolset. Homes: Crop, transform, straighten, Presets, Look, Basic, Presence, Tone curve, Detail, Colour mixer, Lens correction, Perspective and Vignette are `adjustments`. Corrections is `tools`, with the Heal and Clone kinds following it. AI editing is `corrections`, with Masks › Object following it. The RAW development is `processing`. Developer modules keep their Developer group.
- `module.list` adds, for each module, `toolset`, `locked`, `origin`, `placements: [{toolset}]` and the person's `enabled`. The desktop reads placements from the core instead of deciding them itself, so Settings, the panel and agents agree. The rule moves from the desktop into the core unchanged.
- `module.layout.read` answers `{toolsets: [{toolset, order, default}], disabled, unrecognized}` for the orderable toolsets, `adjustments` and `tools`. The strip's pointer and Mask are host entries at its start and are not part of the order.
- `module.enable {module}` and `module.disable {module}`. A locked module refuses with `invalid-params` and its reason, and an unregistered id refuses by name. Enabling a module that is on by default removes its stored entry.
- `module.order.set {toolset, order}` takes the complete order, a permutation of that toolset's registered placements, and refuses anything else by name. A move up, down, to the top or to the bottom is computed by the client and sent as the whole order. Unrecognized ids stored in that toolset keep their stored positions.
- `module.layout.reset {toolset?}` restores one toolset's order. Without `toolset` it restores every order and turns every module on. It is an ordinary write, which the desktop can undo by writing back what it read.
- Every change answers the new layout and announces one `module.layout` event, which the desktop's event sync, an open Settings sheet and agents follow. `schema.list` is unchanged by the layout. `luxforge-json`, `luxforge-ctl` and `luxforge-mcp` reach the same methods.

### Desktop

- **Tools panel.** Sections draw in the person's `adjustments` order. A secondary click on a band opens Move up (⌥↑), Move down (⌥↓), Move to top, Move to bottom, Turn off *Title* (absent when locked), Customize panel… and Copy as JSON request. The panel takes no drags: a band press still expands or collapses it. A footer after the last section names the modules that are off (`2 modules off: Perspective, Vignette · Customize…`), and is absent when every module is on.
- **Mode strip.** The pointer and Mask first, then the module tools in the person's `tools` order. A **⋯** at the strip's end, also opened by a secondary click on the strip, lists every strip tool with its check and lock or letter, then Customize tools…. Off tools stay listed there, unchecked.
- **Letters.** A letter belongs to the first module in registry order that declares it, and host letters (V, M) come first. A later module gets no letter rather than a substitute: its tooltip says whose the letter is, and the palette still enters it. Today's built-ins do not collide, and registration already refuses duplicates between modules.
- **Palette.** *Customize panel…*, *Settings · Modules*, and *Turn off Title* or *Enable Title* for every module that is not locked. An off module's own actions, resets and mode are not listed.
- **Settings › Modules.** A tab after Appearance, in every build. Modules are grouped by home (Adjustments, Corrections, Tools, Masking, Processing only), each group in the person's order, and each row has a grip, its title, an optional qualifier (`RAW photos only`), its letter, and a switch or a lock. The detail pane shows the hint, the placements, where it renders among the stages with "Panel order doesn't change this", and the API methods it adds. A row moves by dragging its grip within its group, or with ⌥↑ and ⌥↓, and Space toggles it. Each change is written at once. An **Undo** toast reverts the last change, and ⌘Z does the same while the toast shows. **Reset order** works per group and **Reset all** restores everything. Neither needs confirming, since no photo changes. Another client's change reaches an open sheet through the event sync.
- **Where it lives in code.** The layout is derived in `state/` from `module.list` and `module.layout.read`, beside `tools::derive` and the strip's derivation in `state/canvas.rs`, with no layout logic in widgets. The keymap and palette take the effective layout from the same derivation.

### Evidence

A rendered smoke scenario, `module-layout`, runs in the background with an isolated evidence directory. It reorders the panel and the strip, turns Vignette off, opens a photo with a Vignette layer, opens Settings › Modules and resets, and correlates each capture with `module.layout.read`, `module.list` and a render whose bytes are unchanged by every layout step. Core tests cover storage, merge with unregistered ids, refusals, events and the unchanged `schema.list`. Desktop state tests cover the derived panel, strip, palette and keymap, and the Off · edit kept band.

## Phase 2: external adjustment modules

### Contract

An external module is a **field-patch module** whose field table comes from its manifest and whose colour unit runs in WebAssembly. The host already does everything a field-patch module needs from a field table: the descriptor, `edit.set-<name>` and `edit.reset-<name>`, parsing, planning, no-op detection, canonical payloads, history labels, the recipe row, neutrality, presets and copy groups ([field patches](modules-and-api.md#field-patches)). So the sandbox supplies only what is genuinely the module's: its compiled coefficients and its pixel arithmetic. This keeps the trust boundary small and gives community modules the same history, preset, copy-settings, API, MCP and generated-panel behaviour as built-ins with no desktop changes.

- **Package.** A `.lfmodule` file is a zip of `luxforge-module.toml`, `module.wasm` and an optional `program.wgsl`. Size and entry limits are checked, with no paths outside the package root. The manifest declares:
  - `id`: reverse-DNS, never `luxforge.*`, and valid as an identity.
  - `title`, `hint`, `version` (semver), `authors`, `license` (SPDX), `repository` URL and the `api` version it targets.
  - `toolset = "adjustments"`, the only value in phase 2.
  - One effect: `stage` is `color` or `finish`, with `order` and `format`. It is not maskable and exists on every source kind.
  - The fields, ranges, defaults, steps, units, hints and groups, in the existing parameter and control vocabulary ([UI components](ui-components.md)). The host refuses a table it could not draw, as `Spec::new` does for built-ins.
- **Component interface.** A WIT world, `luxforge:module/adjustment@0.1`, has three exports:
  - `coefficients(values) -> result<list<u32>, string>`: the unit's words from the canonical field values.
  - `apply-row(words, y, x0, rgb: list<f32>) -> list<f32>`: the CPU reference, under the [`PointwiseColor`](../../crates/luxforge-core/src/modules/processing.rs) contract.
  - `label(values) -> option<string>`: an optional history summary.
  It has **no imports**: no WASI, clock, randomness, files or network. A module that imports anything does not load. The words are the unit's identity, so equal values give equal operations.
- **Determinism and bounds.** The engine is Wasmtime, pinned, with relaxed SIMD and threads off and NaN canonicalization on, so the reference is reproducible across machines. Each instance has a fixed memory limit and each call an epoch deadline. A call that traps, overruns or returns the wrong length fails that render explicitly, naming the module, and never yields a partial frame. Instances are created lazily per worker on first use, so an installed module that no photo uses costs no instance. Compiled code is cached under the data root, keyed by the package hash and the engine version.
- **GPU program.** `program.wgsl` declares one `Colour` entry, `fn lf_module(rgb, pos, words, block) -> vec3<f32>`, and only functions and constants whose names start with `lf_module`, under the surface's prelude ([GPU programs](../../crates/luxforge-core/src/render/gpu/program.rs)). The loader validates it with naga and gives its names a unique prefix. A program starts disabled. At first use, a bounded probe on a worker renders a fixed set of probe patches and field values through the reference and the GPU, and enables the program only within the colour class's tolerance. The result is cached per package hash and adapter. A module without a program, or whose program fails the probe, renders through the reference fallback with the program named ([GPU-first](gpu-first.md)). That is correct, but slower.
- **Where it lives.** A new crate, `luxforge-wasm`, owns Wasmtime and the component bindings, and hands the registry assembly a provider per installed module. The core names neither Wasmtime nor a GPU crate. Program text and words cross into the core as data, as built-in programs do.

### Lifecycle

- **Store.** Installed packages live under the data root, one directory per module id and version, recorded in `modules.json`: a format-marked document outside every catalog, written atomically. An evidence run keeps its own store.
- **Next launch.** The registry is fixed for a launch, so installing, updating or uninstalling a package, or loading or removing a folder, takes effect on the next launch. Settings shows **Relaunch to apply** with a Relaunch button, as Experiments does for launch flags. Live loading is later and not required.
- **API.**
  - `module.package.inspect {path}` reads and validates a package without installing it, for the review sheet.
  - `module.install {path}` copies a validated package into the store. A package with the same id replaces it at the next launch. When the new version's payload format differs from the installed one, the answer says that existing edits will be unavailable until a version that reads them is installed.
  - `module.uninstall {module}` removes the package at the next launch.
  - `module.folder.load {path}` and `module.folder.remove {module}` are developer mode only.
  - `module.status` reports each external module's load state (`loaded`, `pending-relaunch` or `failed {reason}`) and its GPU program's state.
  - Each write announces `module.installed`.
  - Installing is a person's or agent's explicit API call, with no consent grant, because phase-2 modules hold no capabilities. The desktop's review sheet is the human confirmation.
- **Failure is explicit and keeps data.**
  - A package that does not validate, or a component that does not instantiate, is registered **unavailable** with the loader's reason when its manifest is readable. Otherwise its effect id is simply unknown.
  - Either way the photos that use it keep every layer, and rendering, sampling, appending and export refuse with the existing `unavailable effect` error ([missing effects](modules-and-api.md#missing-effects)).
  - Panels show **Didn't load** or **Not installed** in red. Installing a compatible version restores rendering without touching stored data.
  - A layer of a payload format the module does not declare is refused by its format marker, as a built-in's is.
- **Off** means the same as for a built-in: the module stays loaded, so its photos render and export.
- **Usage statistics** report an external module as `external` without its id.

### Trust

Community modules are not reviewed by Luxforge, and the review sheet and Settings say so. Their safety rests on the sandbox: no imports, bounded memory and time, a validated WGSL program run under the host's bindings, and output that is only colour values. The review sheet shows the manifest's identity, licence, authors, repository, version, package hash, what it adds and where, and "Network: none · Files: none". Signing, a verified-author mark and reproducible builds wait for a library.

### Authoring

- **SDK.** `luxforge-module-sdk` is a small guest crate: the WIT bindings, typed access to field values, and a test harness that runs a module's `apply-row` natively and through Wasmtime and asserts identical buffers.
- **Packaging and checking.** `cargo xtask module-package DIR --output FILE` builds the package. `cargo xtask module-check FILE` validates the manifest and the program, runs the reference over fixed fixtures, and reports the GPU difference against the colour class's limits on the local adapter.
- **Example.** `examples/modules/split-toning` is a separate Cargo project, outside the workspace, that depends only on the SDK. It is the proof: it is built, packaged and installed with no change to host source, it appears in the tools panel, and it is reachable through `module.list`, `schema.list`, `edit.set-split-toning`, presets, copy settings and MCP.

### Evidence and measurement

- **Journey.** A rendered journey, `external-module`, starts from an empty data root:
  - Inspect and install the example, relaunch, and edit through the desktop and the API.
  - Compare the GPU picture with the reference, export, and check the original's bytes are unchanged.
  - Turn the module off and check the export is unchanged.
  - Uninstall and relaunch: the band reads Not installed and export refuses, with the layer kept.
  - Reinstall and relaunch: the export matches the first.
- **Core tests.** These cover the manifest and package refusals, the import refusal, traps and deadlines, determinism, the program's validation and probe, and an unknown format.
- **Measurement.** Once the feature works, this is measured on the M4: launch and idle RSS with no external module, with one installed and unused, and with five; first use, meaning instantiation, shader compilation and the probe; and the reference's per-frame cost at 24 and 60 MP against the native vignette's. The binary-size and build-time cost of Wasmtime is recorded too. Figures go in the [performance spec](../specs/performance.md).

## Later, not planned

- A community library: a Browse view in Settings › Modules, search, an index and its hosting, signing and updates. This needs its own decision on who lists modules.
- Other kinds: external canvas tools in the mode strip, mask kinds (host-owned today), correction kinds, spatial units, maskable external effects.
- Capabilities for external modules: settings, models and network, through the [module capabilities](module-capabilities.md) contract and its consent.
- Live install and unload without relaunching.
- A photo count per module in Settings, and reordering the Corrections and mask kinds.

## Performance rules checklist

- **Phase 1** adds no work to rendering or the owner thread's edit path. The layout is read once per launch and on its event, and the desktop derives it with the panel, bounded by the module count.
- **Phase 2** adds no thread. Calls run on the existing pools with bounded instances, memory and deadlines. Instances and program probes are created lazily, and nothing is loaded for an unused module beyond the compiled-code cache. Untrusted input is checked for size and shape before it is parsed. Costs are measured once at the end, as above, and no performance claim is made before then.

## Recorded defaults

These are recorded defaults the owner can revise. The plan runs on them.

- The words Off and Enable; the locked set above; one switch per module at its home; no drag inside the tools panel; Masking shown as one locked summary row.
- An off module's methods stay callable, and `schema.list` ignores the layout.
- The layout is per person, not per catalog or per workspace profile.
- The Off · edit kept band is read-only, with Enable as its only action.
- External effects are restricted to the colour or finish stage, not maskable, on every source kind, with one effect per module.
- Install, update and uninstall take effect on the next launch, with no rollback store.
- A failed program probe falls back to the reference rather than refusing the module.
- The names `.lfmodule`, `luxforge-module.toml`, `luxforge-wasm`, `luxforge-module-sdk` and the example's Split toning.
