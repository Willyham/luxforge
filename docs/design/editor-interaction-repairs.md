# Editor interaction repairs

Status: implemented and verified by focused regressions, the quick tier and native background mask, catalog and Nikon comparison checks. Measurements and their scope are in [performance](../specs/performance.md#catalog-edit-comparison).

## Behavior and acceptance

- Clip radial outlines before mapping and subdivision so an off-image arc cannot exhaust the bounded geometry budget and remove visible arcs. Visible handles remain individually drawable and hittable at Fit, percentage zoom and transformed geometry.
- H in Mask mode toggles mask handles through `workspace.set`; the setting is independent of O and coverage visibility. Text capture, modifiers and key repeat retain the existing keymap rules. Hidden resting handles cannot be grabbed; placement and an already-held gesture can still complete.
- Adjustment gestures temporarily suppress mask coverage, preserving its stored presentation. Release or cancellation restores the selected mask's coverage. Geometry and brush gestures keep their normal coverage.
- A comparison reuses the current GPU After picture when a shared prepared source and conservative GPU headroom allow that picture to cover the comparison view completely. RAW white-balance redevelopment and historical selection retain the exact reference fallback. Unsupported views retain the exact reference fallback. Before and After keep correlated source, recipe, geometry and view identities; exit restores the retained edited picture. No new photo-sized cache or memory budget is introduced. Measure the owner's Nikon edit using an isolated copy of the current catalog when available.
- Entering Develop from an open catalog folder uses that folder's current grid as the development set. Browsing an event or disk folder preserves that catalog-folder choice; selecting a catalog view without a folder clears it and falls back to the loaded set. Without a catalog folder or a previous set, the Develop tab is disabled and D explains that a catalog folder or grid photographs must be chosen. Explicit grid development remains available. Current active photograph is preserved if it belongs to the set; otherwise open its first photograph. Empty folders explain why no set can be opened.
- Broad roots, homes and user-parent folders open for navigation without a scan, as accepted by the owner; the shared API refuses recursive scans of these containers.

## Constraints and delegated choices

Use the shared command service, bounded workers and background-only verification. Originals and edit history are preserved. Routine code structure, clipping numerical details and readiness predicates belong to the implementer. Do not add migrations or unrelated tools.

## Performance review

The review below covers source access, allocations, owner/UI work, refreshes, caches and scoped measurements. Mask geometry and view toggles must not render or upload photograph pixels. Folder set reads use bounded browse.rows windows. No idle timer is added.

## Performance-rules review

Original reads and RAW preparation still use the verified source service. No full-frame allocation
or image-sized cache is added: After shares existing raster allocations and retains the existing
GPU picture/source handles under the photo and GPU-preview budgets. Point queries and validation
render nothing. The owner performs only registry/session changes and bounded browse queries;
source preparation, photograph rendering and coverage remain on workers. H requests only
workspace.set; overlay suppression clears coverage while the ordinary adjustment drafts render
through their existing path. Folder set loading uses at most 100,000 identities/names and reads
1,000 rows per request, releasing its independent session on every result. No timer, poll or
persistent subscription is added. Reference and GPU effect equations are unchanged; native
comparison and mask captures check presentation with correlated state. Scoped Nikon comparison
measurements are recorded in the performance specification after verification; the general
24 MP editor-performance matrix does not measure this shortcut or catalog navigation.
