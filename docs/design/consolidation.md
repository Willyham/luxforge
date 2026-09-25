# Consolidation

Status: **authorized by the owner on 2026-09-24. Waves 1 and 2 are complete; the remaining work is planned as concrete tasks in the [task plan](../../tasks/consolidation.json), each of which deletes the copies it replaces and states its proof. The source-kind controls wait on the owner's answers to their design's open questions.** When the last task lands, delete this document and the plan; the outcome lives in the specs they changed.

## Why

Features were built in parallel by separate agents, and each built the shared mechanism it needed again instead of extending the existing one. Most cross-cutting concepts now exist two or more times, and the copies have started to disagree:

- a preset applied to a masked photo plans against a stack that the direct action would have filtered;
- Restore admits a recipe with fewer checks than every other commit;
- the mask gesture sends `draft.set` through a runtime hop that the slider gesture was built to avoid;
- the RAW white-balance locus carries a mistyped coefficient that the Basic copy does not.

The fix is consolidation, not rewrite. Each concept keeps one implementation that every feature extends, and the copies are deleted.

The owner's decisions for this work are in [decisions](../decisions.md#architecture-review).

## Constraints

- **No product change** except the decided ones and the defects the tasks name.
- **Renders stay byte-identical** unless a task says otherwise and proves its bound. A rendering change keeps a sample equal to the rendered byte, on both paths.
- **Current shapes only.** API, catalog and payload shapes may change. There are no compatibility shims, and a changed table bumps the catalog format.
- **UI/API parity is kept.** Where a task can, it makes parity hold by construction rather than by a test.
- **No old path left behind.** A task deletes the copies it replaces in the same change. Two mechanisms for one concept is the failure this work removes.
- **Rules still apply.** The [performance rules](../engineering/performance-rules.md) apply to every change under `crates/`, which answers the checklist once, for the finished change. Verification follows [when to verify](../engineering/development.md#when-to-verify): targeted tests while working, `quick` at hand-off, and `rendered` and `timing` once per wave on the integrated branch.

## One path per concept

| Concept | Copies today | One replacement | Wave |
| --- | --- | --- | --- |
| Committing a change | Hand-built write transactions for import, commit, navigate, restore, versions and no-ops; Restore admits with fewer checks | `mutate(plan)` and one `admit(recipe)` | 1 (Restore), 2 |
| Planning an action | `apply_action`, a RAW special case, `draft_recipe`, preset steps, mask commands | One plan path over a lazy `StageContext`, so a draft equals its commit by construction | 2, 3 |
| Method dispatch and parameters | The `METHODS` table, three string-match layers and the capability host's own match; parameters declared as strings, serde structs and a hand-copied schema test | One table with service and owner handlers; parameters declared once | 2 |
| Jobs | Source, analysis and capability jobs with three ID schemes and three status vocabularies | One job status, then one job table | 2 |
| Latest-job workers | Preview, analysis and clipping-overlay queues, each spawning a thread per job | One persistent worker primitive with supersede and abandon tokens | 5 |
| Desktop drafts | The slider draft, loose mask-gesture fields and the local crop draft | One core-draft state machine and one gesture field | 1 (hop), 4 |
| Pixel evaluators | Byte `Evaluation` and RAW `LinearEvaluation` | One pipeline generic over its pixel domain | 5 |
| Field-patch modules | Basic, colour mixer, Presence and vignette, about 85% the same code | One declarative field-patch module: a field table plus `compile` | 3 |
| A module's one layer | Eight lookups, some ignoring mask targets | A declared single-layer effect and one host lookup | 1 (presets), 3 |
| Module list and neutrality | The registry's list and the desktop's own; desktop payload parsing for neutrality | One `builtin_modules()`; the core reports a neutral layer | 3 |
| Source-kind controls | RAW's and Basic's own Exposure and white balance | One control set that behaves per source kind, applicability declared ([design](source-controls.md)) | 3 |
| Recipe-bound external data | Strokes carried on the recipe; artifacts through a global weak index and pins | Both carried on the recipe | 2 |
| Parameter vocabulary | Module parameters and capability settings; four schema emitters | One vocabulary and one schema emitter | 3 |
| Colour math | sRGB transfer, Planckian locus, 3×3 inverse and Oklab products copied across modules and renderers | One colour module | 3 |
| Smoke scenarios and test support | Launch, frame and pixel helpers per scenario; test helpers per test binary | A scenario library and table; test-support and reference crates | 6 |

## Waves

1. **Fix and trim.** The defects, the cheap hot-path fixes and pure deletions: the event emitted by a deduplicated retry, Restore's admission, presets on masked photos, the mask gesture's runtime hop and its Discard race, the Presence estimate key, bounded RAW spatial frames, render frame copies, the RAW locus coefficient, test hooks out of the public API, the capability trims that only delete, the transport moved onto a pinned client, obsolete xtask commands, and the documentation run-logs.
2. **Core front doors.** One mutation path, one plan path with a lazy `StageContext` (removing the RAW special case and the dead sampler), table-driven dispatch with parameters declared once, typed preparation needs, a cache of immutable entries with shared strokes, history rows without snapshots with a narrow desktop refresh, artifacts carried on the recipe, one job status. Then `editor.rs` is split around a bound evaluation context.
3. **Module contract.** The declarative field-patch module, a narrower plan result whose layer identity, format and mask the host owns, declared single-layer effects, `builtin_modules()` with core-reported neutrality, declared source kinds with one Exposure and white-balance control set, the colour module, descriptor builders and one schema emitter, mask commands through the shared plan path, and the capability settings on the parameter vocabulary.
4. **Desktop.** One core-draft driver and gesture field, `app/mod.rs` split with nested messages, a presenter for the displayed frame, overlays on the photo surface, a silent idle poll, per-message waste removed, crop on the core draft lifecycle.
5. **Rendering and RAW.** The latest-job worker primitive, one compile per job, one render entry point and a render context in place of globals, band-parallel RAW development, row evaluation on the RAW path and then one pipeline for both domains.
6. **Harness.** A smoke scenario library and table, one conformance suite for field-patch modules, a serde-derived evidence script, test-support and reference crates, and one timing distribution type.

Waves 3 to 5 depend on wave 2 and can overlap each other. Wave 6 is independent of all of them.
