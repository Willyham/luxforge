# After the consolidation

Status: **authorized by the owner on 2026-09-26**, who took the recommendations of a whole-codebase review of `main` at `4f8c3e1`. The work is planned as one task plan per group, listed below. When every task in those plans is complete, or has moved into the plan of the milestone it prepares, delete this document; the outcome lives in the specs the tasks change.

## Why

The [consolidation](../decisions.md#architecture-review) gave each shared mechanism one implementation: one write path, one planning path over a lazy `StageContext`, one method table with parameters declared once, one render entry point with an injected render context, one latest-job worker, one draft state machine and refusal rule, one frame presenter, an idle desktop that is asleep, artifacts carried on the recipe, mask commands on the action path and one home for colour maths. What remains has a different shape:

- **Second halves the consolidation named and did not reach.** One job status but three job tables; the crop on the core draft but with its own messages, refusals and angle control; parallel thresholds, fixture lists and test helpers still restated.
- **Two decisions recorded but not landed in substance:** a module's applicability to a source kind is still decided by its name, and the transport still owns an HTTP client of the old size.
- **Machinery added without a measurement**, such as the desktop's per-region derivation keys.
- **Byte-identical speedups** on the preview, RAW-development and gesture paths nobody has profiled since the refactor.
- **Tests at 46% of the tree**, some duplicated at the same layer and a few that cannot fail.
- **Defects**, each verified against the code: a retried `draft.commit` is refused; a panic on the source worker leaves its waiters blocked; brush strokes commit past the occupancy cap and the next adjustment is refused; `verify` counts skipped components as passed and cannot regenerate a missing fixture in an existing checkout; any live client can make the owner read a file path; and smaller ones listed in the plans.

The owner's decisions for this work are in [decisions](../decisions.md#post-consolidation-review).

## Constraints

- **No product change** beyond the decided ones and the defects the tasks name.
- **Renders stay byte-identical** unless a task says otherwise and proves its bound; a sample stays equal to the rendered byte on both pixel domains.
- **Measured, not assumed.** A speedup records its before and after on photo-sized inputs under [performance rule 13](../engineering/performance-rules.md), back to back and in reversed order on the shared host, in [performance](../specs/performance.md) with its scope. A figure the review estimated is a hypothesis until then.
- **Current shapes only.** API, catalog, payload and evidence shapes may change without shims; a changed table bumps the catalog format.
- **UI/API parity is kept**, by construction where a task can.
- **No old path left behind.** A task deletes what it replaces in the same change.
- **One path stays one path.** Once the repository's one-path check exists, each task that finishes a concept adds the rule that keeps it single, so a later change that brings a second path back fails `cargo xtask check-repository`.
- **A deleted test names its keeper.** A commit that deletes a test names, for each of its assertions, the kept test that proves it; a test that cannot fail is deleted or rewritten, never kept as coverage.
- **Rules still apply.** Every change under `crates/` answers the [performance checklist](../engineering/performance-rules.md#review-checklist) once, for the finished change. Verification follows [when to verify](../engineering/development.md#when-to-verify): targeted tests while working, `quick` at hand-off, and `rendered` and `timing` once per wave on the integration branch.

## Plans

| Group | Plan | Holds |
| --- | --- | --- |
| Core service and API | [core service](../../tasks/core-service.json) | Retry routing, panic containment, events that name their subject, one job table and API, declared method envelopes, one bound evaluation, one preparation path, error data, an explicit public surface, core test duplicates; MCP, library and export groundwork |
| Module contract | [module contract](../../tasks/module-contract.json) | Declared source kinds, the shared Exposure and White balance controls, one presettable-action rule, field patches over the parameter vocabulary with a spec builder and a descriptor snapshot, `ToolModule` and descriptor trims, developer-only test modules, module test duplicates; the geometry carry hook |
| Rendering | [rendering](../../tasks/rendering.json) | A render context per test, one limits module, a trimmed pixel-domain pipeline, the byte-identical render and proxy speedups, the drafted exact phase, viewport tiles, larger tiles for a large halo, opaque frames, `render.rs` split, rendering test duplicates; stage-boundary methods for Corrections |
| RAW | [RAW](../../tasks/raw.json) | The development executor and normalization, the GainMap, the source cache policy, the develop boundary, container parsing and tooling, and the RAW qualification carried over from the initial RAW plan |
| Masking | [masking](../../tasks/masking.json) | The occupancy cap at stroke time and a radius-relative decimation, path limits, the kind table, paint latency, stroke storage, typed mask commands, kind helpers, the studies' move and one kind-conformance suite; generic path primitives for Corrections |
| Module capabilities | [capabilities](../../tasks/capabilities.json) | Resource installs from the pinned URL only, grant eviction, artifact collection, the transport on `ureq`'s agent and in its own crate, activation deferred, the desktop surface trimmed, capability test duplicates |
| Desktop | [desktop](../../tasks/desktop.json) | The crop angle reset, a loud log cap, Undo refused during a draft, the widgets' own slider path, the crop on the one draft driver with a declared stepper, one refusal, view-model rules once, synchronous draft rounds, the armed brush, the crop proxy stage, the region keys, presentation and seam-owned state, evidence outcomes, layering, controls without a mirror, a widget-only board, shared widget forwarding, the headless binary's own crate, live-agent evidence, desktop test duplicates; surface ids for the library |
| Harness | [harness](../../tasks/harness.json) | `verify`'s fixtures and honest summary, tests that cannot fail, documentation drift, the one-path repository check, test rules and one gate, one sRGB reference, the timing tools on the launch envelope, one distribution, `raw-editor` as a scenario, scenarios as plans plus pixel claims, `architecture.md`'s structure, `editor-acceptance`'s scope, test binaries, harness clean-ups |

The [Corrections](../../tasks/corrections.json), [dependency advisories](../../tasks/dependency-advisories.json) and [product decisions](../../tasks/product-decisions.json) plans are unchanged in purpose and sit beside these.

## Waves

The waves are program phases, each an outcome across the groups. Inside a plan, execution waves come from its tasks' dependencies; each task names its program wave first in its context. Later waves overlap earlier ones wherever they touch different files.

1. **Fix.** The verified defects and their regression tests: retried commits answered, panics contained, events that name their subject, the occupancy cap at stroke time, path limits that agree, resource installs from the pinned URL only, grant eviction, the crop angle reset, a loud log cap, Undo refused during a draft, `verify`'s fixtures and its incomplete status, tests that cannot fail, documentation drift, the retired RAW scratch semaphore and a render context per test.
2. **Finish the consolidation.** Every concept that still has two paths gets one, with its repository check: one job table and API; declared method envelopes and retry routes; one bound evaluation; one preparation path; errors that carry data; declared source kinds; one presettable-action rule; field patches over the parameter vocabulary; one limits module; a trimmed pixel-domain pipeline; artifact collection; the transport on `ureq`'s agent; the crop on the one draft driver with a declared angle stepper; one start refusal; the widgets' own slider path in evidence; view-model rules computed once; a mask kind table ready for the next kinds; the test rules, one gate and one sRGB reference; the timing tools on the launch envelope, one distribution and `raw-editor` as a scenario.
3. **Speed.** The byte-identical speedups, each measured: spatial tiles written in their final layout, frames zeroed lazily, the resample's quantizer, per-pass parallel thresholds, row reads for points and estimates, the proxy build, the drafted exact phase, a viewport-tile design, lazy vignette tables, the RAW development executor and normalization, the GainMap, the source cache policy, paint pacing, stroke storage, synchronous draft rounds, the armed brush as view state and the crop's proxy stage.
4. **Structure.** Simplifications that follow once the paths are single: an explicit core surface, the `ToolModule` and descriptor trims, developer-only test modules, opaque frames, `render.rs` split, the RAW develop boundary, container parsing, public API and tooling, typed mask commands and kind helpers, the capability trims and the transport's own crate, the region keys, presentation and seam-owned desktop state, evidence outcomes, true layering, controls without a mirror, a widget-only board, shared widget forwarding, the headless binary's crate, scenarios as plans plus pixel claims and `architecture.md`'s structure.
5. **Tests.** Each group's duplicate tests go, each paired with its keeper; the mask studies move to the reference crate; `editor-acceptance` keeps what `cargo test` cannot prove at the same layer; fewer test binaries; live-agent evidence; harness clean-ups.
6. **Roadmap groundwork.** Blocked until its milestone starts, then done first: `events.wait` and typed host parameters before the MCP adapter; per-photo selection, a paged catalog and relocation before the library and Locate; an export job lane on the bound evaluation before JPEG export; stage-boundary methods and generic path primitives before Corrections; a geometry carry hook before a new geometry module; surface ids before a second photo surface.

## Order across groups

Plans never reference each other's tasks. These outcomes land in this order:

- **The one-path repository check** (harness) before the wave-2 tasks in other groups add their rules to it; a task that lands first adds its rule beside the existing layering rules, and the check's table absorbs it.
- **Events that name their subject** (core service) before **live-agent evidence** (desktop), which renders another client's change arriving.
- **A render context per test** (rendering) before **the test rules and one gate** (harness), which state the rule it follows.
- **Declared source kinds** (module contract) remove the desktop's name checks in the same change, before **view-model rules computed once** (desktop) finishes the rest.
- **The drafted exact phase** (rendering) is measured before **paint latency** (masking) paces its offers, so each figure is attributed.
- **Evidence outcomes** (desktop) before **scenarios as plans plus pixel claims** (harness), since both sides of the evidence format change.
- **One job table** (core service) before the capability desktop trim reads jobs through it (capabilities).
- **The other groups' wave-2 changes in the core** before **an explicit core surface** (core service), which touches every module.
- **The transport on `ureq`'s agent** before **the transport in its own crate** (both capabilities).

## Acceptance

- Every task in the eight group plans is complete, or blocked on its named milestone or owner decision.
- Each fixed defect has a test that fails before the fix.
- Each speedup has its before and after recorded with scope in [performance](../specs/performance.md), with renders byte-identical.
- The `rendered` and `timing` tiers pass once per wave on the integrated branch, and `cargo xtask check-repository` enforces the one-path rules the tasks added.
- The [architecture](architecture.md), [performance rules](../engineering/performance-rules.md), [feature status](../features.md) and [user guide](../user-guide.md) describe the result.

## Out of scope

Deferring the module capabilities framework (not adopted); replacing LibRaw's unpack with RawSpeed (a separate proposal awaiting the owner); where Detail's sharpening and noise reduction run (open, in [product decisions](../../tasks/product-decisions.json)); the open questions in [source controls](source-controls.md); and new features, including export, Locate, MCP and Corrections, whose groundwork is listed above but whose delivery belongs to their own milestones.
