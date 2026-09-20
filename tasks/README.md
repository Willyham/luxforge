# Task plans

Each JSON file is an independent plan. IDs start at `TASK-001` inside every file, dependencies point only at earlier tasks in the same file, and execution waves are derived from those dependencies. Milestone order is expressed in the [roadmap](../docs/plan.md) by named outcome, never by cross-file task references.

| Plan | Purpose |
| --- | --- |
| [S0 follow-ups](implementation-s0.json) | Hosted CI refresh, Windows/Linux packaging and desktop checks, Linux VM route, license audit and expiring advisory exceptions |
| [Basic adjustments and histogram](implementation-basic-histogram.json) | Proposed JPEG exposure/histogram slice, then core tone, white balance and color controls; integration alongside separately owned M3/M4 work |
| [Develop workspace](implementation-develop-workspace.json) | The accepted workspace shell: widget crate, layered desktop, generated panels, canvas modes, palette and evidence over the current modules |
| [Editor follow-ups](implementation-editor-followups.json) | Export, color and metadata, Locate, MCP and full-editor verification |
| [Later extensions](implementation-extensions.json) | External-loader measurements and proof planning after a selected use case |
| [Product decisions](product-decisions.json) | Open product questions |

Editor follow-ups and later work wait until the owner asks for them.

The Basic/histogram plan is planning only; its [design](../docs/design/basic-and-histogram.md) records proposals and integration dependencies. Ready tasks identify runnable preparation after implementation is authorized, not approval to begin all proposed work.

## Conventions

- A plan holds its own behavior, context, acceptance and tests. Link Markdown specifications or code for context and describe required capabilities by name. Never link another task plan or mention its IDs.
- Keep IDs stable during routine updates. Statuses describe actual work; completing a task needs the evidence its acceptance asks for. A planning edit never completes implementation.
- Keep context concise and current. No planning-change logs.
- Create or update plans for substantial coordinated work, or when asked. Research, reviews, diagnostics, documentation maintenance and small contained changes do not need a plan.
- Completed plans are deleted, not archived. Their outcome lives in the specs and feature status.

## Validation

`cargo xtask check` validates every JSON file in this directory against `tools/task-plan.schema.json`: unique plan IDs, contiguous ordered local IDs, dependency status and order, derived waves, and that file links do not point into another task plan. The advisory audit reads the S0 plan to confirm its two advisory follow-up tasks remain open.
