# Layers, history and exact transforms

Status: implemented (M1 and M2) and verified on the M4 Mac. This is the foundation every later tool uses.

## State model

A referenced source is read-only and identified independently of its locator. An edit layer has a stable layer ID, effect type and format, and parameters at a position in an ordered recipe. Each immutable recipe snapshot holds the complete stack needed to reconstruct the image. A history entry records the action that produced a snapshot: stable asset, entry, snapshot and layer IDs; per-asset sequence; action and provider identity with validated parameters; actor, timestamp and request ID; base and result revision; the complete resulting stack; undo parent; restore target when applicable. Sequence orders entries; timestamps are display information.

JPEG import creates Original with an empty stack. RAW import creates Original with its one required source-development layer and captured as-shot defaults; every later snapshot retains that layer at index zero. Later adjustment of a layer creates a new snapshot that retains the layer's identity; older stacks never change. A transaction may change several parameters while producing one action entry.

No full bitmap per history entry, no private widget undo stack, no module-owned database writes. A snapshot is evaluated from original pixels and its ordered payloads, never by replaying an evolving command log.

## Commit, undo, redo and restore

A real image change atomically persists its snapshot and entry, the current pointer, a monotonic revision, redo state and the retry result, after validating all shared and effect invariants. Rejected actions, failed writes, no-op changes, pointer motion, Cancel and browsing add no row.

Undo moves to the current entry's undo parent; redo follows the persisted redo path. Both change the concurrency revision and save navigation atomically without adding rows. Restore copies any retained snapshot, including Original, into a new Restore action whose undo parent is the previously current entry, keeping all later actions. The copied stack is admitted by the same checks as every commit, so restoring one this build cannot evaluate, such as a stack with an unavailable provider or a lost stroke or artifact, fails explicitly and writes nothing, while undo, redo and browsing still reach it. Restoring an equivalent current state is a no-op. A new edit or restore clears shortcut redo while every state stays in the chronological log.

Example: Original → A → B → Restore A → C keeps A and B. Undo C returns to Restore A; undo again returns to B. Undoing back to A and making D starts a new path from A while B, Restore A and C remain browsable. Reopening restores the complete log and navigation state.

## Read-only preview

The history browser lists Original and bounded pages of attributed actions, marking the current committed state separately from the selected entry, with explicit Previewing, Return to current and Restore controls. A page is made of rows — identity, sequence, action, label, actor, timestamp, undo parent and restore target — read from the catalog's columns without decoding any stack; the whole entry with its stack is inspected one at a time. The desktop reads the newest page when an asset opens or changed elsewhere and merges the current entry's row after its own commit, undo, redo or restore. Selecting or rendering an entry changes no committed stack, revision, log or redo state. Selecting the current entry is Return to current, in the UI and through `preview.select`: the latest state is never a historical preview. Return to current shows the latest committed snapshot even after an external edit during preview. A headless snapshot render does not change GUI selection unless the caller invokes session selection. Every preview result identifies its source, snapshot, entry and render generation; rapid selection supersedes obsolete work. Editing while previewing requires Return to current or Restore first. Export later captures the committed stack; exporting a historical stack requires restoring it.

## Pixel proof

The pixel editor exposes x/y and RGB controls, Apply and optional pointer picking, backed by one semantic command. Coordinates are integers in the content stage, the source after EXIF orientation, x right and y down, whatever transforms or crop follow; a coordinate inside the photograph but outside the current crop is accepted and simply not visible; values are 8-bit sRGB; invalid input fails explicitly. Replacing a pixel with its current value is a reported no-op. Exact lossless buffers on synthetic fixtures are the correctness oracle; JPEG re-encoding is not. Two writes to the same location prove ordering: the later wins and undo exposes the earlier.

## Exact transforms

Coordinates use a top-left origin, x right, y down. Every layer addresses its input stage; pixel-stage layers are placed before the geometry tail, so their input stage is the content stage.

| Operation | Input to output mapping | Output size |
| --- | --- | --- |
| Rotate right | `(x, y) → (h - 1 - y, x)` | `h × w` |
| Rotate left | `(x, y) → (y, w - 1 - x)` | `h × w` |
| Mirror horizontal | `(x, y) → (w - 1 - x, y)` | `w × h` |
| Flip vertical | `(x, y) → (x, h - 1 - y)` | `w × h` |

Mappings are integer-exact with no interpolation, accumulated raster edits or irreversible writes. Four matching quarter-turns and two matching reflections are identities. Pixel edits sit before the geometry tail and move with the image under every transform; the renderer still evaluates any layer order exactly, and tests cover identities, non-commuting combinations, EXIF-mirrored sources and every interleaving with pixel edits.

The four actions keep their durable history identities (`rotate-left`, `rotate-right`, `mirror-horizontal`, `flip-vertical`) but share one **orientation** layer: effect `lightwell.geometry.orientation`, format 1, geometry stage, payload `{"mirror": bool, "turns": 0..3}`, meaning mirror horizontally when `mirror` is true, then rotate clockwise by `turns` quarter-turns. Each of the eight exact orientations has exactly one payload, and `{"mirror": false, "turns": 0}` is the neutral identity. Applying an action to `(m, k)` gives Rotate right `(m, k + 1 mod 4)`, Rotate left `(m, k + 3 mod 4)`, Mirror horizontal `(not m, 4 − k mod 4)` and Flip vertical `(not m, 2 − k mod 4)`. The orientation layer sits ahead of the crop: the crop effect declares a later geometry order, so the host places a new orientation layer before the crop and any finish layer. When the layer just before that position is an orientation layer the action updates it in place, keeping its identity, so four rotations leave one neutral layer and four history entries; otherwise it commits a new orientation layer there. A transform made after a crop also carries the crop through itself in the same entry, so the crop frames the same content in the turned stage and the output is the previous output turned or reflected ([crop contract](single-image.md#geometry-contract)); an orientation layer stored after the crop is folded ahead of it by the next transform and left neutral. A transform is never a no-op. The retired per-action transform effect is refused as an unavailable effect, never rewritten. Design: [orientation layer](../design/orientation-layer.md).

## Versions and lineage

A version names one retained entry per asset, unique ignoring case, without changing the recipe, revision or log. Creating, listing and deleting versions are programmable and emit events; restoring a version is the ordinary restore of its entry, and deleting a version never removes history. Lineage walks undo parents from any entry newest first in bounded pages, and the desktop marks loaded entries off the current lineage as branches. Client sessions are held by the catalog owner per registered client and carry a revision; a client adopts only responses at least as new as the session it holds. Design and storage decision: [versions and lineage](../design/versions-and-lineage.md).

## Catalog and live API

Catalog reopen preserves Original, all layers and snapshots, entry identities, current state, redo navigation and request deduplication within the supported format. Only current shapes are supported. An unsupported catalog format fails explicitly and directs the user to a new catalog path; unsupported recipe or effect formats also fail explicitly. No existing data is rewritten or silently discarded. Drafts are session-only.

The desktop footer offers Copy message for its complete status or error text, including when an import fails before an asset is open. Copying leaves the displayed message and editing state intact. JSON clients receive failures directly as structured `error.code` and `error.message` fields.

One application service and one catalog owner serve UI and JSON/IPC clients. Discoverable schemas cover asset, layer and state queries, every module action as a generated `edit.<action>` method with its parameter descriptors, module descriptors, history list (rows without stacks), inspect (one whole entry), render, select, return-to-current, restore, undo and redo, viewport state, pixel sampling and jobs. Expected revision and request ID guard mutations; a retry returns the original result without emitting a second event, and a reused ID with a different payload fails. Every mutation, Restore included, takes one write path, so a request another write committed while it was being planned is a `conflict`, never a catalog error. Draft conflicts come from the crop module: Restore, undo and redo do not silently discard an active draft, and agent commits preserve drafts and mark them conflicted.

## Acceptance

1. Import → pixel A → pixel B: exactly one layer and action per real change, ordered values, unchanged original bytes, same-pixel overrides and rejected no-op or invalid requests.
2. Select Original and every entry through UI and API, comparing complete stacks and exact buffers, with committed state and revision unchanged.
3. Undo, redo, restore, edit again and reopen: every state reconstructable with stable IDs, coherent navigation and incrementing revisions.
4. An independent live client while the GUI is open: stale revisions, duplicate and conflicting request IDs, event gaps, reconnect and client/job isolation.
5. Interrupted writes and imports, malformed or incompatible catalogs, locked storage, changed or missing originals and unsupported payloads all preserve the last valid state.
6. Rapid browsing of a long history within bounded queries, jobs and memory, with native M4 pixels correlated to entry labels and render generation. A history page decodes no entry, and a desktop commit reads back only `asset.state`, the session, the displayed entry's recipe rows and masks and one preview job, with undo, redo and restore adding the lineage.

History tests create catalogs and recipes with the current implementation, covering crop drafts, geometry snapshots and conflicts alongside pixel and transform edits.
