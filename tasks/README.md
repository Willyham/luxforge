# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

## Post-consolidation work

Authorized on 2026-09-26 and designed in [after the consolidation](../docs/design/post-consolidation.md), which holds the constraints, the six programme waves and the order across groups. There is one plan per group. Each task names its programme wave first in its context: 1 fix, 2 finish the consolidation, 3 speed, 4 structure, 5 tests, and 6 roadmap groundwork, which stays blocked until its milestone starts.

| Plan | Group |
| --- | --- |
| [Core service](core-service.json) | The editor service and its API: retries, panic containment, events, one job table, method envelopes, one bound evaluation, one preparation path, error data and the public surface; MCP, library and export groundwork |
| [Module contract](module-contract.json) | Declared source kinds, the shared Exposure and White balance controls, field patches over the parameter vocabulary, the descriptor snapshot, `ToolModule` and descriptor trims, developer-only test modules; the geometry carry hook |
| [Rendering](rendering.json) | Render contexts in tests, one limits module, the pixel-domain pipeline, byte-identical render and proxy speedups, the drafted exact phase, viewport tiles, larger tiles for a large halo, opaque frames and `render.rs`'s structure; stage-boundary methods for Corrections |
| [RAW](raw.json) | The development executor and normalization, the GainMap, the source cache policy, the develop boundary, container parsing and tooling, and the RAW qualification milestone: controlled quality, the foundation and journey checkpoints, failure hardening, packaging, whole-editor measurement and RAW export |
| [Masking](masking.json) | The occupancy cap and decimation, path limits, the kind table, paint latency, stroke storage, typed mask commands and one kind-conformance suite; generic path primitives for Corrections |
| [Capabilities](capabilities.json) | Pinned-URL resource installs, grant eviction, artifact collection, the transport on `ureq`'s agent and in its own crate, activation deferred, the trimmed desktop surface |
| [Desktop](desktop.json) | The crop on the one draft driver, one start refusal, synchronous draft rounds, the crop's proxy stage, presentation and seam-owned state, evidence outcomes and live-agent evidence, layering, the widget-only board and the headless binary's crate; surface ids for the library |
| [Harness](harness.json) | `verify`'s fixtures and honest summary, tests that cannot fail, documentation drift, the one-path repository check, load-independent tests, one sRGB reference, one launch path and distribution for timing tools, scenarios as plans plus pixel claims, `editor-acceptance`'s scope and test binaries |

## Other plans

| Plan | Purpose |
| --- | --- |
| [Corrections](corrections.json) | Proposed offline Clone/Heal and optional provider-agnostic AI Remove, with a qualified local-model path and explicit owner decisions |
| [Dependency advisories](dependency-advisories.json) | Remove or re-review the two expiring advisory exceptions the dependency audit enforces |
| [Product decisions](product-decisions.json) | Open product questions |

The Corrections plan is a planning proposal. Its AI implementation builds on the implemented [module capabilities](../docs/design/module-capabilities.md) and depends on owner acceptance of the scope and consequential product choices in the [Corrections design](../docs/design/corrections.md).

RAW editing is implemented with initial native M4 verification. Its [design](../docs/design/initial-raw.md) records the owner's continuous RAW editing requirement, the pinned processing path, and outstanding controlled quality, resource and platform qualification, which the RAW plan carries. The supplied FC3411 DNG has required gain/warp corrections and continuous editor support; its [contract](../docs/design/air2s-dng.md) records the qualified encoding, numerical interpretation and limits. RAW JPEG-export integration requires the shared exporter on the [roadmap](../docs/plan.md) and does not block the RAW editing checkpoint.

## Conventions

- A plan holds its own behavior, context, acceptance and tests. Link Markdown specifications or code for context and describe required capabilities by name. Never link another task plan or mention its IDs.
- Keep IDs stable during routine updates. Statuses describe actual work; completing a task needs the evidence its acceptance asks for. A planning edit never completes implementation.
- Keep context concise and current. No planning-change logs.
- A task's `test_strategy.commands` are its acceptance checks, run once when the task is complete, not after each step. Put timing commands in the task that completes the feature, or in a task whose subject is performance, so a plan measures once.
- Create or update plans for substantial coordinated work, or when asked. Research, reviews, diagnostics, documentation maintenance and small contained changes do not need a plan.
- Completed plans are deleted, not archived. Their outcome lives in the specs and feature status.

## Validation

`cargo xtask check` validates every JSON file in this directory against `tools/task-plan.schema.json`: unique plan IDs, contiguous ordered local IDs, dependency status and order, derived waves, and that file links do not point into another task plan. The advisory audit reads the dependency advisories plan to confirm each exception's task remains open.
