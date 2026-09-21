# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

| Plan | Purpose |
| --- | --- |
| [S0 follow-ups](implementation-s0.json) | Hosted CI refresh, Windows/Linux packaging and desktop checks, Linux VM route, license audit and expiring advisory exceptions |
| [Editor follow-ups](implementation-editor-followups.json) | Export, color and metadata, Locate, MCP and full-editor verification |
| [Initial RAW editing](implementation-initial-raw.json) | Continuous high-precision RAW recipes for the original Nikon Z6, Fujifilm X100VI and supplied DJI Air 2S DNG: mode qualification, decoder/development evidence, bounded preparation, neutral rendering, exposure/WB, history and UI/API parity |
| [UI components](implementation-ui-components.json) | The closed control vocabulary for modules: new parameter and control kinds, pure widgets, desktop gesture rules, a controls proof module and rendered evidence, per the accepted [design](../docs/design/ui-components.md); not started |
| [Later extensions](implementation-extensions.json) | External-loader measurements and proof planning after a selected use case |
| [Product decisions](product-decisions.json) | Open product questions |

Editor follow-ups and later work wait until the owner asks for them.

RAW editing is implemented with initial native M4 verification. Its [design](../docs/design/initial-raw.md) records the owner's continuous RAW editing requirement, the pinned processing path, and outstanding controlled quality, resource and platform qualification. The supplied FC3411 DNG has required gain/warp corrections and continuous editor support; its [contract](../docs/design/air2s-dng.md) records the qualified encoding, numerical interpretation and limits. RAW JPEG-export integration requires the separately delivered shared exporter and does not block the RAW editing checkpoint.

## Conventions

- A plan holds its own behavior, context, acceptance and tests. Link Markdown specifications or code for context and describe required capabilities by name. Never link another task plan or mention its IDs.
- Keep IDs stable during routine updates. Statuses describe actual work; completing a task needs the evidence its acceptance asks for. A planning edit never completes implementation.
- Keep context concise and current. No planning-change logs.
- Create or update plans for substantial coordinated work, or when asked. Research, reviews, diagnostics, documentation maintenance and small contained changes do not need a plan.
- Completed plans are deleted, not archived. Their outcome lives in the specs and feature status.

## Validation

`cargo xtask check` validates every JSON file in this directory against `tools/task-plan.schema.json`: unique plan IDs, contiguous ordered local IDs, dependency status and order, derived waves, and that file links do not point into another task plan. The advisory audit reads the S0 plan to confirm its two advisory follow-up tasks remain open.
