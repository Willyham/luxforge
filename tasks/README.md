# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

## Planning for execution

A plan handed off for implementation is executable work. Resolve material product questions while creating or revising it; the task list must not send the implementer back to ask what to build.

- Reuse existing context, decisions and authorization. Ask only questions whose answers materially change the product or scope, bundle them where possible, and resolve them before handing off the affected implementation. Routine numerical, layout and engineering choices belong to the implementer within the recorded contract; state that delegation instead of adding a review task.
- Give every task a concrete output, sufficient context and observable acceptance. Do not add tasks to ask the owner, obtain implementation approval, confirm scope or freeze a contract through another decision round. Record decisions in the design and [decisions](../docs/decisions.md), not as future implementation work. A genuine discovery task produces an artifact or capability consumed downstream, with a reason it must come first.
- Maximise work that can finish autonomously. Build a useful working feature first where possible. Put owner review, Lightroom/reference matching, calibration, extensive qualification and other refinement after that delivery unless their output is genuinely needed to build it correctly. Ordinary correctness, source-preservation and rendered integration checks stay with the code they prove; moving perfection later does not make an unrun check a pass.
- Dependencies are required outputs, not ceremonial phase gates. Independent work can run together; late review or refinement has no dependency back into the working-feature handoff. Name any unavoidable prerequisite and its concrete effect on the consuming task.
- Identify external inputs while planning. Split data/tooling preparation and independent implementation from the part that needs those inputs; do not make a whole feature wait for a manual export, private data or final approval. Use honest measured/unmeasured status where inputs are absent. If a final consequential action needs approval, prepare its concrete reviewable result first.

A planning request still does not authorize executing the implementation or unrelated external actions. Preserve actual authorization boundaries, settle needed authorization during planning rather than adding an approval task, and distinguish an unfinished proposal from an implementation-ready plan. When revising existing plans, preserve IDs, truthful statuses and completed work; apply these conventions to the remaining work without silently changing scope or prior decisions.

## Layout

Plans live in one folder per area. A plan moves only when its area changes; its status lives in its tasks, not in its location. Plans that are validated and have tasks ready to implement are listed in the roadmap's [ready to implement](../docs/plan.md#ready-to-implement) section, with the minimum model and the re-review every task needs before it starts.

### Rendering (`rendering/`)

The renderer, its GPU and CPU paths, memory and efficiency.

| Plan | Purpose |
| --- | --- |
| [Efficiency](rendering/efficiency.json) | Reference-renderer, preparation and desktop CPU/memory reductions: hardware SHA-256 and build features, source preparation, the 16-bit quantizer, spatial tile buffers and batches, Detail and Presence kernels, RAW row reads, the float mosaic through a checked-in librtprocess patch, owner and painting copies, the WAL catalog and desktop derivation, measured once at the end; the `dist` profile is blocked until other plans' outstanding timing runs are recorded |
| [GPU-first rendering](rendering/gpu-first.json) | The GPU as the renderer of record for the picture, the histogram, samples and export, with correctness a declared tolerance against a whole-frame CPU reference: every zoom on the GPU, the picture at rest and the histogram on the GPU, per-frame estimates, samples and export through GPU tiles, portability and warm-up, the retirement of the CPU production paths, and one qualification and measurement at the end |
| [GPU memory accounting](rendering/gpu-memory.json) | Measure and bound both GPU devices, including resources outside the 1 GiB aggregate photo slots and separate 2 GiB preview and tile-worker budgets, with resources.read and Performance attribution |

### RAW (`raw/`)

RAW development and looks.

| Plan | Purpose |
| --- | --- |
| [RAW looks](raw/raw-looks.json) | New RAW photos start from a Luxforge look: the frozen look units and the corpus-chosen Standard look in the Original with its preference and section (phase 1, complete and qualified on the M4), then Match camera fitted to the embedded preview off the owner (phase 2), measured once at the end |

### Library (`library/`)

Selection, catalog sources and library operations.

| Plan | Purpose |
| --- | --- |
| [Panorama merges and the merge preview](library/panorama-merge.json) ([design](../docs/design/panorama-merge.md)) | Implemented: the merge preview that opens first for HDR and panoramas, panorama stitching as a second merge kind over the HDR boundary (kind-tagged provenance and commands, a shared merge lane with band-read spools), derived sources up to 65,000 pixels a side and 512 MP through paged preparation, rendering and export, merge-only RAW and JPEG frame development, rotation-model registration, projection, photometric fit, seams, blending and Boundary Warp, the job and commands, Select's panorama menu, sheet and stack, and the background `panorama-merge` journey; the `hdr-merge` journey through the preview, the `derived-paged` GPU corpus entry's remaining cells, natural-panorama quality and photo-sized measurement remain |

### Editing tools (`editing/`)

One plan per tool module.

| Plan | Purpose |
| --- | --- |
| [Colour grading](editing/colour-grading.json) | Implemented mixer extension, native M4 full-tier component coverage and 30-sample wheel/scalar and paired HSL/grade measurements at 24/60 MP and Nikon Z6 RAW ([scope and misses](../docs/specs/performance.md#colour-grading-qualified-and-measured-on-the-m4)); a Lightroom round remains for the delivered `grade-align` tooling |
| [AI editing](editing/ai-editing.json) | Remove, Select, generative fill and Replace, and optional sky replacement on local, user-downloaded models, generative fill first: the inference port and runtime, the model manager, the analysis cache and picker, the model-selection mask kind, the fill tiers, the remote-provider shape, portability, qualification and documentation |
| [AI tools in the Develop workspace](editing/ai-tools-workspace.json) ([design](../docs/design/ai-tools-workspace.md)) | Implemented: Remove paints and a separate Object tool picks, in Corrections and in the Masks panel, on the Generative tier, with object picking behind `ai.analysis`; module setups with one sheet and one consent per tool. The M4 network setup is done (TASK-006). Native qualification on the M4 has run (TASK-007, in progress): hover, selection and residency within budget; open are the missed analysis-ready budgets, a direct drawn-overlay measurement, removal of the corpus road and kayaks, small-object picking and fills, and two desktop defects (the status line on choosing Object, the hover outline over a candidate draft) |
| [Auto tone](editing/auto-tone.json) ([design](../docs/design/auto-tone.md)) | Implemented and qualified on the M4; the owner's Lightroom fitting round remains: deterministic Auto for Lightroom's eight Basic fields from a bounded sample of the stage Basic receives, the action, query and Tone-header button first, Auto in presets and the Lightroom importer, then a fit to Lightroom's Auto on the owner's photos last |
| [Grain](editing/grain.json) ([design](../docs/design/grain.md)) | Decided, not authorized: synthetic film grain (Amount, Size in full-resolution output pixels, Roughness) as a finish-stage module after the vignette, a pure function of position with an exact reference, the study first, then the module, GPU program, preset mapping, desktop section and smoke scenario, M4 qualification and measurement, and a Lightroom grain round folded into its responses last |

### Lightroom (`lightroom/`)

Import from and alignment with Lightroom Classic.

| Plan | Purpose |
| --- | --- |
| [Lightroom alignment](lightroom/lightroom-alignment.json) | Decided; TASK-001 to TASK-005 (the generator, measurement and fitting, round 1, the calibrated mapping rows, the photographs and round 2) complete; TASK-006 (Basic, Exposure and white balance on a measured scene scale) and TASK-007 (Clarity, Dehaze, the mixer's HSL sliders, the vignette's Amount and Midpoint) complete, reviewed, confirmed by round 3 and within the GPU's limits on every corpus cell on the M4 Pro; TASK-008 onward await authorization; the mixer's hue travel (level C) an open owner question: the measurement rig over deterministic reference-rendered study outputs, two Lightroom rounds by the owner, calibrated conversions for the importers with RAW preset white balance in its round, each control's response realigned by editing area with its CPU reference and GPU program, targeted behaviour changes past the threshold, and validation on the owner's real edits |
| [Basic tone that adapts to the picture](lightroom/basic-local-tone.json) ([design](../docs/design/basic-local-tone.md)) | Decided; TASK-001 to TASK-005 complete on the CPU reference, TASK-006 open: a level C change of the alignment plan's TASK-006, with Highlights, Shadows and Whites acting on a RAW photograph's scene values relative to its development's scene white and Blacks, Contrast, Vibrance and Saturation after the Look; the reference, the scene white, the registry's rendered tail, the module at format 4 with Auto tone and the refit are done; confirmed by round 3; M4 qualification and the owner's review remain |
| [Lightroom import](lightroom/lightroom-import.json) | Decided, not authorized: a read-only import of Lightroom Classic catalogs and sidecar folders, the photographs worked on with their organization, settings, report, re-mapping and Lightroom's first grid tiles only, with no import undo (phase 1), masks (phase 2), and mappings that land with later capabilities or alignment measurements (phase 3) |

### Interface (`interface/`)

| Plan | Purpose |
| --- | --- |

### Programmability (`programmability/`)

Programmatic access to the operation registry beyond the JSON API itself.

| Plan | Purpose |
| --- | --- |
| [MCP adapter](programmability/mcp.json) ([design](../docs/design/mcp.md)) | Implemented and verified on Linux: `luxforge-mcp`, a dual-era stdio MCP server with compact and generated typed tool profiles, the registry markers and client identity it needs, inline pictures, headless mode, packaging, Settings › Agents and the background `mcp` journey; the journeys' M4 runs and real-client qualification remain |

### Project (`project/`)

Owner decisions, release preparation, host services and repository upkeep.

| Plan | Purpose |
| --- | --- |
| [Release audit follow-up](project/v0-1-audit-followup.json) ([design](../docs/design/v0.1-audit-followup.md)) | Durable retries, source-integrity recovery, shared resources/permissions, publication outcomes, ingress bounds and DNS cancellation implemented; native rendering/platform/corpus qualification remains ready |
| [Dependency advisories](project/dependency-advisories.json) | Remove or re-review the two expiring advisory exceptions the dependency audit enforces |
| [Product decisions](project/product-decisions.json) | Open product questions |
| [v0.1 release preparation](project/release-v0-1.json) ([design](../docs/design/release-v0.1.md)) | Version/identity, the macOS bundle and icon, and packet verification (complete); the source archive (built; Linux rebuild and distribution review open), the Linux tarball's desktop integration and ELF import check (built; hosted run open) and installation/help docs (in progress); distribution advisory review, signing and final native/corpus/install qualification; macOS and Linux together; paid Apple membership active, signing setup reported complete with profile `luxforge-notarization`, identity/profile validation and native Linux qualification outstanding, an owner-pushed tag publishing a pre-release automatically |
| [Automatic updates](project/auto-update.json) ([design](../docs/design/auto-update.md)) | Implemented: the update feed and signed per-release records, the core `update.*` commands and preferences, a launch past an unsupported catalog, the macOS disk image, the Linux payload updater, Sparkle with a custom user driver, the desktop offer and Settings group, the move into Applications and feed publication from the release workflow; qualification on real signed candidates and the documentation remain |

The Efficiency plan follows the [decisions](../docs/decisions.md#cpu-and-memory-efficiency). Every feature task is done and measured ([performance](../docs/specs/performance.md#cpu-and-memory-efficiency-measured-on-the-m4)); each change paid for itself where measured, and the workloads not measured are listed there. Its one open task is the `dist` profile, blocked until the other plans' outstanding timing runs are recorded, so every plan measures in `release`.

The GPU-first plan follows the [decisions](../docs/decisions.md#gpu-first-rendering) and the accepted defaults in its [design](../docs/design/gpu-first.md). The retirement of the CPU production paths is complete, the display-sized CPU proxy remaining only for a session without a usable GPU. Its portability task remains open: Linux no-adapter checks, release acceptance and packaging pass, while the retained lavapipe journeys await a passing hosted result; the final qualification and throughput tasks follow it. GPU accounting is a prerequisite of any total-memory claim. Its tolerances and behaviour defaults are accepted; softer budget-reduced motion at 100% is qualified, reported and not gated.

The AI editing plan follows the [decisions](../docs/decisions.md#ai-editing): quality-first model choice with no licence or provenance gate, ONNX Runtime, stale patches kept rendering, budgets raised for quality, consent remembered per provider, sky replacement in scope. It builds on the object removal research, the standalone prototype measured on the M4 (`probes/object-removal`), the implemented module capabilities and the delivered mask model. Generative fill comes first: the decisions, the harness, the inference foundation, the generative qualification, the real models, the generative workflow over the developer fake and the local provider are complete, and the generative workflow needs only the foundation and the delivered Corrections candidate seam; the Fast tier, analysis and Select follow. All AI work takes reference inputs, locks the generating photo while another stays editable, and qualifies combined renderer and inference memory.

The RAW looks plan follows the [decisions](../docs/decisions.md#raw-looks), including its design's accepted defaults. Phase 1 (the Standard look) is complete with the owner-approved numbers frozen: the look scenario passes on the supplied files, and verify quick and the rendered tier with the owner manifest pass. Match camera remains planned. Its fused look program is registered and device-qualified. The corpus release gate covers Standard at amount 100 implicitly in RAW Originals; Neutral, other amounts and Camera fits are not separately qualified on photographs by that gate.

The Lightroom import plan follows the [decisions](../docs/decisions.md#lightroom-import). Implementation is not authorized; its first task, confirming the catalog format against a copy of the owner's catalog, needs no code.

The Lightroom alignment plan follows the [decisions](../docs/decisions.md#lightroom-alignment). TASK-001 to TASK-007 are complete, with the generator's fixes, the measurement of photographs and the sliders realigned (A9 to A11): Basic, Presence, the mixer and the vignette are realigned and within the GPU's limits on every corpus cell. TASK-008 onward await authorization. Its rounds need the owner to import and export in Lightroom; its response changes raise each module's format marker, so recipes of a module's earlier scale are refused.

## Conventions

- A plan holds its own behavior, context, acceptance and tests. Link Markdown specifications or code for context and describe required capabilities by name. Never link another task plan or mention its IDs.
- Keep IDs stable during routine updates. Statuses describe actual work; completing a task needs the evidence its acceptance asks for. A planning edit never completes implementation.
- Keep context concise and current. No planning-change logs.
- A task's `test_strategy.commands` are its acceptance checks, run once when the task is complete, not after each step. Put timing commands in the task that completes the feature, or in a task whose subject is performance, so a plan measures once. When several agents' work is integrated, a task's updates to the shared documents (feature status, the user guide, the roadmap, the development guide and this README) are the integrator's, made once per feature from the implementers' hand-offs, even when the task's acceptance names them ([agent quickstart](../docs/engineering/agent-quickstart.md#docs)).
- Create or update plans for substantial coordinated work, or when asked. Research, reviews, diagnostics, documentation maintenance and small contained changes do not need a plan.
- A new plan goes in its area folder. When a validated plan gains a `ready` task, or its last ready task starts or finishes, update the roadmap's [ready to implement](../docs/plan.md#ready-to-implement) list in the same change, or at integration when an integrator merges the work.
- Completed plans are deleted, not archived. Their outcome lives in the specs and feature status.

## Validation

`cargo xtask check` validates every JSON file in this directory and its area folders against `tools/task-plan.schema.json`: unique plan IDs, contiguous ordered local IDs, dependency status and order, derived waves, and that file links stay in the repository (no absolute path, no `..` out of it) and do not point into another task plan. The advisory audit reads the dependency advisories plan to confirm each exception's task remains open.
