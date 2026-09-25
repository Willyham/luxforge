# Versions, lineage and the history graph

Status: implemented. This note records what the history graph is, the named versions and lineage queries built on it, and the catalog storage decision those features settled. The owner chose the name "version" for the Lightroom-style saved state.

## The graph that already existed

Every history entry stores its complete immutable stack and an `undo_parent`. Undo and redo move the current pointer; a new edit after undo appends with the undone-to entry as its parent; nothing is ever deleted. That is a commit graph: an entry is a commit, its stack is the tree, `undo_parent` is the parent pointer, the asset's current entry is HEAD and the per-asset `sequence` is the reflog. Restore is a forward-moving revert that copies an old stack into a new entry.

Lightroom truncates later history when you edit from an earlier state, and its snapshots exist to survive that truncation. Lightwell never truncates, so every state a Lightroom snapshot could bring back is already reachable through `history.list` and `history.restore`. What was missing was a way to name one entry among hundreds, and a way to see the chain rather than the chronological list.

## Vocabulary

- **Snapshot** keeps its existing internal meaning: the immutable stack one action produced. It is not a user feature.
- **Version** is the user-facing name for a named reference to one retained entry. It is a tag, not a branch.
- **Lineage** is the undo-parent chain from an entry back towards Original.

Branches in the Lightroom sense, virtual copies with their own current pointer, are not part of this work. They would need a second head per asset and are left for a later library decision.

## Versions

A version is a row in `versions` with the asset, a name, the entry it names, the actor and a creation time. Names are unique per asset ignoring case, trimmed, one to sixty-four printable characters. Creating a version does not touch the recipe, revision or history; it emits an event so other clients refresh their lists. Creating the same name on the same entry is a no-op; on a different entry it is a conflict. Deleting a version removes only the name and is a no-op when absent, so retries are safe without request identifiers. The named entry stays in history and can still be previewed and restored after its version is deleted.

Restoring a version is the existing `history.restore` on the version's entry. There is no separate restore path to keep the operation set small.

## Lineage

`history.lineage` walks `undo_parent` from an entry (default current) newest first, returning entry id, sequence, action and parent per step, at most one hundred steps per call with `next_entry_id` to continue. It reads the `undo_parent_id` column rather than parsing entry JSON. The desktop reads it when an asset opens, after its own undo, redo and restore, and when another client changed the asset; after its own commit it adds the new entry to the loaded lineage itself, because that entry's undo parent is the entry that was current, which the lineage already holds. It marks loaded entries that are not on the current lineage as branches; when the chain was truncated it marks nothing at or below the oldest returned step, because it cannot know.

## History rows

`history.list` answers pages of **rows**, newest first: each entry's `id`, `sequence`, `action_id`, `label`, `actor`, `timestamp_ms`, `undo_parent` and `restore_target`, and no stack or parameters. The rows are read from the entry's own columns without decoding its JSON, so a page costs the same whatever the stacks hold, masks included. `history.inspect` answers one whole entry with its complete stack. The desktop's history panel reads only rows: an open or a change made elsewhere reads the newest page, and its own commit, undo, redo or restore merges the current entry's row into the loaded page instead.

## Storage: catalog format 10

Entry JSON is the authoritative stored recipe snapshot. Beside it each entry's row fields have their own columns — `sequence`, `action_id`, `label`, `actor`, `timestamp_ms`, `undo_parent_id` and `restore_target_id` — written from the same entry by the one insert every commit and every import takes, so a history page and a lineage walk read columns only. Both are immutable. The `versions` table holds named references to entries. The current catalog format is 10. It holds what format 7 merged — the [preset library](presets.md#library), the [mask](masking.md) table and content-addressed stroke store, and the catalog's identity with the [derived-artifact](module-capabilities.md#derived-artifacts) tables, whose per-entry references keep every artifact a version or branch reaches alive — plus the row columns and, in the request table, each asset request's whole answer, so a retried `mask.*` command answers with the identities its first attempt minted. A format 7 catalog keeps the row fields only inside the entry JSON, so it is refused by name rather than read by decoding every entry, and a format 9 catalog stores only the mutation result of a request, so it is refused by name as well. History inserts name their columns explicitly.

An empty, unmarked database is initialized with the current schema. Existing catalogs must use the current format marker. Unsupported or nonempty unmarked catalogs are refused without rewriting their data, with an error directing the user to a new catalog path. Only current shapes are supported during pre-release development.

## Sessions live with the owner

The same change moved client sessions into the catalog owner, keyed by a registered client id. Previously the desktop threaded a copy of its session through every task and wrote the copy back on completion, so a slow read-only refresh could overwrite a newer zoom or selection. Now only the owner mutates sessions, every session-returning response carries a session revision, and the desktop adopts a response only when its revision is at least the one it holds. Pan goes through `view.set` like zoom, with one request in flight and only the newest pending position.

## API

| Method | Effect |
| --- | --- |
| `version.create` | Name an entry (default current); no-op when the name already names it |
| `version.delete` | Remove a name; the entry remains |
| `version.list` | Versions in creation order with their entry sequence |
| `history.lineage` | Undo-parent chain from an entry, paged |
| `history.list` | History rows newest first, paged, without stacks; `history.inspect` reads one whole entry |

The one method table in the core carries each method's declared parameters, notes and handler; the schema and the parser are generated from the same declaration, a method mutates exactly when it carries a mutation envelope, and a generated test sends every listed method its declared fields and one undeclared one. `version.create {asset_id, name, mutation, entry_id?}` and `version.delete {asset_id, name, mutation}` take the `{request_id, actor}` envelope, and a version records its `actor`.

## Acceptance

- Empty catalogs initialize with the current format. Unsupported formats and nonempty unmarked catalogs are refused without changing their bytes.
- Current-format imports and edits reopen with stable entries, versions, undo/redo and unchanged source bytes.
- Versions survive reopen, reject empty, oversized and control-character names, treat case-insensitive duplicates as conflicts, restore through the ordinary restore path and keep their entry after deletion.
- Lineage skips abandoned branches, pages with a continuation id and rejects unknown assets.
- An independent JSON client creates a version, undoes, lists versions and reads a one-step lineage in one session.
- History rows carry no stack, equal their entries' row fields before and after reopen, page without gaps and decode no entry. A format 7 catalog is refused by name and left as it was.
- Desktop: stale session responses are not adopted, pan coalesces to one in-flight request, and a refresh replaces or merges history and marks branches. A commit's refresh reads no page, lineage or versions and merges its row and lineage; undo, redo and restore read the lineage; an answer overtaken by a newer selection or revision is dropped; a test counts the owner calls of each. `cargo xtask check` passes.
