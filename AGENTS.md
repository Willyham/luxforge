# Working on Lightwell

Lightwell is an open-source, non-destructive desktop photo editor for macOS, Windows and Linux, built around a small, fast core that people, programs and agents operate equally. Read this file before changing anything. It is the source of truth for what we are building and how we work.

## Pillars

Every change is measured against these.

1. **Originals are sacred.** Source files are never modified. Edits are data: ordered layers in a recipe, with immutable history. Never silently discard an incompatible catalog, recipe or edit; fail explicitly and keep the data.
2. **Everything is programmable.** Every operation a person can perform has a discoverable, schema-described programmatic equivalent through the same command service. A GUI gesture is never the only interface. UI and API parity is verified, not assumed.
3. **Fast, bounded and honest.** Responsiveness, bounded memory and image correctness are architectural requirements, not later tuning. Measure on photo-sized inputs before claiming a performance result. The rules that keep this true are in [performance rules](docs/engineering/performance-rules.md).
4. **Small core, deliberate extension points.** The core owns recipe transactions, history and undo, shared invariants and bounded services. Tool modules own their validation, controls and processing through those APIs. Prefer lazy, optional modules and measure before splitting the core into loadable binaries. External module loading is required later, not now.
5. **Open source, first on the owner's Mac.** GPL-3.0-or-later project code and open-source dependencies. Target the owner's M4 MacBook Pro first; keep Windows and Linux portable, and distinguish VM or headless functional checks from native GPU evidence.
6. **Prove it.** Claims about behavior come with evidence: exact-buffer tests, correlated state, logs and captures for UI, and recorded measurements with their scope. A skipped check is not a pass.
7. **Beautiful defaults, familiar feel.** A small, focused workspace with sensible defaults. Lightroom Library and Develop are a familiarity reference, not a feature checklist or a rendering target.

## Current state

S0 (JPEG viewing) is accepted; the evidence harness now drives the editor itself. M1 (persistent history and live JSON API), M2 (exact rotate, flip and mirror) and M3 (tool modules with generated controls and API) are implemented and verified on the M4 Mac. M4 (Lightroom-style crop as a module with drafts, conflicts and API parity) is implemented and verified on the M4 Mac, as are content-space edits (pixel-stage layers placed before the geometry tail) and the single orientation layer for exact transforms. The Develop workspace shell (widget crate, layered desktop, generated panels, canvas modes, palette) is implemented and verified on the M4 Mac over those modules. Next: export, Locate, MCP and full-editor verification. Everything is v0: no public compatibility framework, release planning, cloud, accounts, marketplace or generalized node graph. See the [roadmap](docs/plan.md) and [feature status](docs/features.md).

## How we work

- **Read first.** For any milestone or tracked task, read the relevant spec and its plan in [tasks](tasks/README.md). Product context lives in [decisions](docs/decisions.md).
- **Plan proportionately.** Substantial features, milestones and other coordinated multi-step work start with a Markdown design (behavior, scope, constraints, acceptance, open decisions) and a validated JSON task plan. Research, reviews, diagnostics, documentation maintenance and small contained changes do not need a task plan unless asked or already tracked.
- **Stay in scope.** Do what was asked. A planning request does not authorize implementing the plan. Do not prebuild future features or placeholder controls.
- **Consult the owner on consequential product tradeoffs.** Record recommendations as proposals until decided. Never turn an unanswered question into an accepted decision.
- **Verify proportionately.** Run `cargo xtask check` before handing off. UI or image changes need a real rendered check with correlated state and logs. Changes under `crates/` answer the performance-rules checklist.
- **Keep automated app launches in the background.** On macOS, use `cargo xtask smoke`, `hardening`, `measure` or `probe`, which launch without activating the desktop. For ad hoc API/render checks use `cargo xtask develop --background` with an isolated catalog or evidence directory. Do not launch the GUI binary directly or activate it through UI automation for routine tests. Foreground interaction checks require the owner's explicit request.
- **Keep docs current, not historical.** When behavior or scope changes, update the spec, [feature status](docs/features.md) and [user guide](docs/user-guide.md). Document current behavior and outstanding work only: no change logs, run logs or planning history. Record a lesson only when something was tried and did not work. Never document a proposed command as if it exists.

## Engineering rules

- Framework widgets hold no authoritative editing or catalog logic. Business rules live in the UI-independent core.
- Every user-facing operation extends the command registry and its schema. Add the API with the feature.
- Decode, render, import and export never block the UI thread or the catalog owner thread. Bound queues and memory; cancel stale work.
- Back image behavior with exact fixtures. Test source preservation and recovery, not just successful rendering.
- Pin dependencies. Manual license, native and asset reviews are deferred by the owner; keep notices and never claim an audit is complete.
- **Current shapes only.** Lightwell is pre-release and breaking changes are expected. Support only the latest catalog, recipe, API and module shapes. Do not maintain migrations, compatibility shims, old-version fixtures or historical parity checks. Internal format markers reject unsupported data explicitly without rewriting or discarding it; use a new catalog when necessary. Current UI/API correctness and exact image tests still apply. Missing or disabled providers report affected edits; they never silently omit an effect from a render or export.

## Map

| Need | Read |
| --- | --- |
| Build, run, smoke, package, CI, evidence rules | [docs/engineering/development.md](docs/engineering/development.md) |
| Performance rules and review checklist | [docs/engineering/performance-rules.md](docs/engineering/performance-rules.md) |
| Architecture and crate layout | [docs/design/architecture.md](docs/design/architecture.md) |
| Milestone contracts M1 to M4 | [docs/design/history-first-roadmap.md](docs/design/history-first-roadmap.md) |
| Develop screen layout, tool array, visual language and desktop architecture | [docs/design/develop-workspace.md](docs/design/develop-workspace.md) |
| History graph, named versions, catalog format 2 | [docs/design/versions-and-lineage.md](docs/design/versions-and-lineage.md) |
| Specs: history, crop and export, recovery, performance | [docs/specs](docs/specs) |
| Task plans and conventions | [tasks/README.md](tasks/README.md) |
| Reference research: stack options, Lightroom, darktable | [docs/research](docs/research) |
