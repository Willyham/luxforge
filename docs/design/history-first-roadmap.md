# Milestone contracts

The history-first sequence proves non-destructive state and history before adding tools of increasing complexity. Each milestone starts from the previous working result and ends with a native M4 demonstration plus proportionate automated correctness and recovery checks. Numerical performance budgets stay proposals until measured and accepted.

| Milestone | Deliverable | Done means |
| --- | --- | --- |
| M1 — history foundation (implemented) | Non-destructive edit layers, a test pixel edit, durable history, undo/redo, restore, historical previews, catalog save and reopen, UI and live API | Original → two pixel changes → preview → undo/redo → restore → new change → reopen passes through UI and API with original bytes unchanged |
| M2 — transforms (implemented) | Rotate left/right, mirror horizontal and flip vertical through the existing layer and history system | Asymmetric fixture and pixel-edit compositions, undo/redo, preview and reopen pass through UI and API |
| M3 — tool modules (implemented) | A small tool interface declaring actions, schemas, controls and processing; the pixel and transform tools implemented through it | Generic UI and API discover and invoke the modules; current recipes render correctly and survive reopen |
| M4 — crop module (implemented) | Lightroom-style crop and straighten delivered as a tool module | Direct manipulation, numeric controls, API parity, history and reopen, composition-preserving geometry at several zoom and DPI settings |

## M1 and M2

The contract for sources, layers, snapshots, history transactions, read-only preview, the live API and the exact transform mappings is in [layers, history and transforms](../specs/edit-history.md). The proof operation replaces one pixel with an explicit 8-bit sRGB value at integer input-stage coordinates; two writes to the same location prove ordering, and exact lossless buffers prove correctness. Source-resolution inspection at 100% makes the changed pixel observable.

## M3 — tool modules

A tool module supplies a stable identity and effect format, action input and result schemas, parameter units, defaults and ranges, declarative control descriptions, availability and state queries, feature validation and processing. The host supplies catalog access, atomic changes and history, rendering jobs and notifications. Modules cannot write catalog tables or keep a private undo stack. Controls describe meaning; the GUI chooses presentation, and UI events and API requests call the same declared handler. The pixel and transform handlers use the same module interface and current payload contracts. Linked modules are enough; independently loaded binaries remain later work. Full contract: [modules](modules-and-api.md).

## M4 — crop module

Recreate the familiar Lightroom crop interaction with an original implementation: free edge and corner handles, composition move, thirds overlay, aspect lock and swap, Free/Original/1:1/3:2/4:3/16:9/custom ratios, numeric straightening within ±45° and a drag-to-straighten guide, Apply, Cancel and reset. Option/Alt scales uniformly about the fixed center. Every gesture evaluates against its starting snapshot so sweeping away and back never cumulatively shrinks the crop. Define the crop layer's normalized representation, output rounding, sample centers, filter and color domain before implementing it. API parameters fully reproduce a crop without gestures. One Apply produces one history entry; Cancel and pointer motion produce none. A live agent commit preserves an active draft and marks it conflicted. Full contract: [crop, export and conflicts](../specs/single-image.md).

## After M4

JPEG export, metadata handling, the current sRGB output contract, Locate, the library, tonal tools, RAW and masks are delivered. MCP conformance and complete editor acceptance remain outstanding; external module loading remains a later requirement. Current scope and verification gaps are in [feature status](../features.md) and the [roadmap](../plan.md).
