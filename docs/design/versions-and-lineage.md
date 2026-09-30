# Versions, lineage and the history graph

Status: implemented. This note records what the history graph is, the named versions and lineage queries built on it, and the catalog storage decision those features settled. The owner chose the name "version" for the Lightroom-style saved state.

## The graph that already existed

Every history entry stores its complete immutable stack and an `undo_parent`. Undo and redo move the current pointer; a new edit after undo appends with the undone-to entry as its parent; nothing is ever deleted. That is a commit graph: an entry is a commit, its stack is the tree, `undo_parent` is the parent pointer, the asset's current entry is HEAD and the per-asset `sequence` is the reflog. Restore is a forward-moving revert that copies an old stack into a new entry.

Lightroom truncates later history when you edit from an earlier state, and its snapshots exist to survive that truncation. Luxforge never truncates, so every state a Lightroom snapshot could bring back is already reachable through `history.list` and `history.restore`. What was missing was a way to name one entry among hundreds, and a way to see the chain rather than the chronological list.

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

## Storage: catalog format 12

Entry JSON is the authoritative stored recipe snapshot. Beside it each entry's row fields have their own columns — `sequence`, `action_id`, `label`, `actor`, `timestamp_ms`, `undo_parent_id` and `restore_target_id` — written from the same entry by the one insert every commit and every import takes, so a history page and a lineage walk read columns only. Both are immutable. The `versions` table holds named references to entries. The catalog also holds the [preset library](presets.md#library), the [mask](masking.md) table and content-addressed stroke store, and the catalog's identity with the [derived-artifact](module-capabilities.md#derived-artifacts) tables, whose per-entry references keep every artifact a version or branch reaches alive. In the request table it holds each asset request's whole answer, so a retried `mask.*` command answers with the identities its first attempt minted. Each asset's source kind tag (`jpeg` or `raw`) has a column of its own beside its interpretation, so a `catalog.list` page reads columns only and decodes no interpretation. History inserts name their columns explicitly.

The current catalog format is 12, the catalog of developed picks ([catalog](catalog.md#storage)). Beside history it adds these tables:

| Table | Holds |
| --- | --- |
| `volumes` | The volumes picks and photographs live on: identity, mount point, label, whether removable, the platform's identifier and when last seen |
| `picks` | One row per picked file: path, signature (length, modification time in nanoseconds, device and inode when known), volume, actor, request and time |
| `indexed_folders` | The folders on disk the person added for events |
| `catalog_folders` | Catalog folders (`folder-…`): name, optional parent, creation time, and the event span and key a Develop made them from |
| `capture` | One row per asset: the sortable capture instant, the camera-local text and day, the offset, position and place, make, model, body serial and lens, exposure time, f-number, ISO, bias and focal lengths, and the stored size and orientation |
| `collections`, `collection_members` | Collections, smart collections (with their stored view query) and groups (`collection-…`), and plain collections' members |
| `library_changes`, `library_change_rows` | The journal of library changes, and each item's value before and after |

Each asset gains its catalog folder, the folder on disk it was developed from, its volume, file name, develop time, removal time, its original's availability (`available`, `offline`, `missing` or `changed`) with the time it was checked, and the moment it was developed from. An asset's `row_id` is an `INTEGER PRIMARY KEY`: it is stable for the catalog's life, it is the 8-byte key a browse view holds, and `capture` and `collection_members` key on it. Its `id` stays the key every other table and every answer uses.

The schema holds the invariants the lanes rely on:

- Every asset is in exactly one catalog folder, on one recorded volume. A folder that holds a photograph or a subfolder cannot be deleted.
- Folder and collection names are unique among their siblings, ignoring case; the top level counts as one parent.
- Only a group holds collections, only a plain collection has members, each once, and only a smart collection has a query. A collection keeps its kind.
- The journal is append-only: its rows are never updated or deleted, a change is undone at most once and an undo redone at most once, and each refers only to earlier changes.

The single-file import puts a new photograph into the top-level catalog folder named after its folder on disk, and records its volume and what it knows of its capture, until `pick.develop` replaces it.

Every earlier format is refused by name and left as it is:

- A format 11 catalog has none of the catalog's tables.
- A format 10 catalog has no kind column.
- A format 9 catalog stores only the mutation result of a request.
- A format 7 catalog keeps the row fields only inside the entry JSON.

The catalog's index, `<catalog stem>.index/index.sqlite`, is a separate database with a format of its own (`INDEX_FORMAT`, 2). It is opened on first use. It is a cache, so where the catalog refuses, the index is discarded and recreated, never touching the catalog: a mismatched format, a file SQLite cannot read and another catalog's index are all discarded.

An empty, unmarked database is initialized with the current schema. Existing catalogs must use the current format marker. Unsupported or nonempty unmarked catalogs are refused without rewriting their data, with an error directing the user to a new catalog path. Only current shapes are supported during pre-release development.

## Sessions live with the owner

The same change moved client sessions into the catalog owner, keyed by a registered client id. Previously the desktop threaded a copy of its session through every task and wrote the copy back on completion, so a slow read-only refresh could overwrite a newer zoom or selection. Now only the owner mutates sessions, every session-returning response carries a session revision, and the desktop adopts a response only when its revision is at least the one it holds. Pan goes through `view.set` like zoom, with one request in flight and only the newest pending position.

A session's history selection is kept per asset, in `session.preview.selections`: each asset the client previews a historical entry of maps to that `entry_id` and the `geometry_from` entry that frames it, and an asset absent from the map is at its current entry. `preview.select` changes the named asset's selection alone, and selecting an asset's current entry removes it; `preview.return-current` and a `job.adopt` return every asset. An edit, undo, redo or draft of an asset is refused only while that asset has a selection, and `render.sample`, `render.locate`, `render.transform`, `preset.capture` and a module query without an `entry_id` answer against the named asset's own selection, so a client can preview one photo's history while it edits another. A session holds at most 16 selections (`MAX_SELECTIONS`); one more is refused with `resource-limit`.

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
