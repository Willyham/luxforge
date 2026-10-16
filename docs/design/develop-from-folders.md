# Develop from catalog folders

Status: decided by the owner on 2026-10-11 and implemented. It replaced the [catalog](catalog.md)'s development set with the catalog folder, and changed the folders Develop N makes, the Catalog sources list and the [HDR merge](hdr-merge.md) actions; the catalog and HDR designs describe the result in place.

## Decisions

The owner asked on 2026-10-11 that pressing a catalog folder in Select should change what Develop holds, that parent folders be selectable as a whole ("2023" over "Japan" and "Korea"), and that Develop N organize photographs as a tree, "2023 › Foo › Nov", so a place's photographs from several days are seen and developed together. The owner chose:

- **The catalog folder is what Develop holds.** The development set is removed from the core, the API and the desktop. Develop's filmstrip is the catalog folder last chosen in Select, with its subfolders, and follows it live: a photograph developed, merged, moved or removed changes it. Pressing any folder in Select chooses it.
- **Year › Place › Day.** Develop N files each photograph under a year folder, a place folder and a day folder: "2026 › Konstanz › 12 Sep". Year and place folders are reused by name, so later picks from the same place and year join it, and a trip's days are siblings under its place.
- **Years are real catalog folders.** Develop makes "2026" as an ordinary top-level catalog folder, which can be selected, renamed and moved like any other. The Catalog sources list drops its computed year grouping and shows the folder tree.
- **One Merge action.** With no set, Pick merged and Import merged would do the same thing, so they become one action: **Merge HDR**, which makes the result in its catalog folder and stays in Select. The planned [panorama merges](panorama-merge.md) follow: **Merge panorama**.

## Behaviour

### Folders Develop makes

`pick.plan` proposes, for each event:

- **Dated events.** The folder an earlier Develop of the same event made: the event key it stores, else an overlapping stored span. The proposal is `{kind: existing, folder_id, days: true}`, so each photograph goes into a day folder inside it. Otherwise it proposes `{kind: place, name}`, where `name` is the event's place. Without a place, it uses the user-named disk folder that names the event ("Lake"), and failing that "Unknown place".
- **Undated events.** The folder of the newest photograph developed from the same folder on disk, `{kind: existing, folder_id}`. Otherwise it proposes `{kind: place, name}` with the disk folder's name.

The folder choices, in `pick.develop`'s `into` and the plan's answer:

| Choice | Where each photograph goes |
| --- | --- |
| `{kind: place, name}` | The top-level folder named after the year of its capture day ("2026"), then the folder `name` inside that, then the folder of its capture day inside that ("12 Sep"). A photograph with no capture time goes to the top-level "Undated" folder, then `name`, with no day level. |
| `{kind: existing, folder_id, days?}` | That folder. With `days: true`, the folder of its capture day inside it, and directly into the folder when it has no capture time. |
| `{kind: new, name, parent_id?}` | One new folder, as before. |

- **Found or made.** Each folder of a path is found by its name among its siblings, ignoring case as names are compared. Otherwise it is made by the batch that first puts a photograph in it, in that batch's library change, as Develop makes a folder today. A `place` choice never adds " 2": reusing the folder is the point.
- **Days.** The capture day is the camera-local day of the photograph's capture time, as read when it is developed. A file the index did not list, such as one opened directly, still gets its day from its header. A day folder is named with the day and the month in English, with no leading zero or year: "3 Sep", "12 Nov". An event that crosses a year files each photograph under its own year.
- **Spans.** A place folder Develop makes records the span and key of the events it was made from, as a new folder does now. Year and day folders record none, and a folder that is found is not changed.
- **The plan's answer.** Each event also answers `days`, its picks' distinct camera-local days in ascending order, and `undated`, how many of its picks have no capture time, so a client can show the folders it will make: "2026 › Konstanz › 12 Sep, 13 Sep". Batches still never cross an event; one batch may make several folders.
- **Opening a file.** A file opened directly is developed with `into: []`, so it takes the plan's proposal. Its parent folder's name is the place, unless an earlier photograph from that folder was developed. `EditorService::import`, the blocking helper for tests and tools, keeps its top-level folder named after the disk folder.

### Order of folders

`folder.list` keeps parents before their children. Siblings are ordered by their names:

1. Years, four digits, newest first.
2. Days ("12 Sep"), in calendar order.
3. Everything else by name, ignoring case.

The desktop lists folders in this order.

### Select's Catalog sources

Under All photographs and Recently developed, the catalog folder tree from the top level, in `folder.list`'s order. Each folder has its count, covering its subfolders, and a disclosure when it holds folders. The newest year folder is open by default, and so is the path to the folder Develop holds. Pressing any folder, a year or a place among them, views it with its subfolders and makes it Develop's folder. The computed year grouping, with its Undated and Empty groups, is gone; a folder of undated photographs or an empty folder is listed like any other.

### Develop's folder

The desktop holds one **Develop folder**, its own view state like the shown workspace, recorded in evidence with each captured frame.

- **What chooses it.**
  - Viewing a catalog folder in Select.
  - A double-click on a photograph in any catalog view: that photograph's own folder, opening on it.
  - Develop N: the deepest folder holding every photograph it developed, or the first one's own folder when they share none. It opens on the first.
  - Opening a file: its folder.
- **What keeps it.** Browsing files, events, other catalog views and Missing originals leaves it as it was. A merge of the folder makes it the folder it went into. Deleting the folder clears it.
- **The filmstrip** holds the folder's photographs with its subfolders, captured oldest first, removed photographs excepted. It is read through a client of its own, so Select's view and selection are untouched, in `browse.rows` windows of at most 1,000, with at most 100,000 photographs. Its header is the folder's path ("2026 › Konstanz") with the active photograph's place in it ("3 of 18"). It is read again after any library change that reaches the desktop, its own or another client's, keeping the open photograph's place. An open photograph that left the folder stays open, with no cell outlined.
- **`D` and the Develop tab** open Develop on the Develop folder: on the photograph already open when it is in the folder, otherwise on the folder's first photograph. A photograph opened with nothing chosen, from Missing originals' Locate for example, is shown without a filmstrip. With neither, the tab is disabled and `D` says "Choose a catalog folder to develop". An empty folder says it has no photographs.
- **Agent equivalent.** An agent reads what Develop holds as the folder's view, `browse.view` of `{kind: catalog-folder, folder_id, subfolders: true}`. There is no separate set to replace or append to.

### Merging

- **Actions.** A bracket header offers Pick all N and **Merge HDR**. The selection menu's HDR group offers Merge HDR… ("Merge the frames into one HDR photograph in the catalog"). It opens the [merge preview](hdr-merge.md#in-the-desktop) first, with Cancel and Merge; ⌥-click merges at once with the last settings. The result is made in its folder, and the person stays in Select.
- **Choosing the folder.** `merge.plan` proposes the folder as Develop would for the reference frame. `merge.create` takes `folder?`, a folder choice as above: the plan's proposal by default, resolved for the reference frame's capture day. The folders it makes are recorded as their own library change (`folder.create`, the request's actor and identity) just before the publication's, in the same transaction, so undoing the merge leaves them: the result it keeps in Removed is still in them. The preview sheet's folder control shows the plan's proposal and sends it, or `{kind: existing, folder_id}` for a folder the person picks; the desktop never sends `folder.create` first.
- **What goes.** `add_to_development_set` and `set_id` are gone.
- **Picks.** Over files, a bracket whose stack has an active result is decided for Moments without a pick. The Picked segment and Develop N count file picks only, and a result is never shown picked.

### Removed

Everything below is removed, with no compatibility path:

- the `development-set.*` methods;
- the `set-member` library item;
- the `development_sets` and `development_set_members` tables;
- the event's `development_set_revision`;
- `in_development_set` in views, rows and `stack.inspect`, and the view stamp's set revision;
- the usage statistics features for adding to and removing from the set.

The catalog format is 2, which also holds the kind-tagged merges of [HDR merges](hdr-merge.md#durable-derived-source) ([storage](versions-and-lineage.md#storage-the-catalog-format)); a catalog of any other format is refused with the existing "choose a new catalog path".

## Delegated choices

Delegated to the implementer within this contract:

- the placeholder place name;
- the defaults for which folders are open;
- how the filmstrip's client is kept;
- the window it reads;
- the wording of status sentences;
- how a deep folder path shortens in the filmstrip header.

The Develop N confirmation shows one field per event for the place's name, with the folders it will make beneath ("2026 › Konstanz › 12 Sep, 13 Sep"); typing turns an existing proposal into `place` with the typed name. **Or add to an existing folder** chooses `{kind: existing, folder_id}`, with no day folders.

## Acceptance

- **Core.** Real-owner tests prove:
  - the proposals;
  - the folders made, found and shared across events, days, years and undated picks;
  - the batch changes and their undo;
  - a file opened directly;
  - `folder.list`'s order;
  - `merge.create`'s folder choice;
  - Moments without a pick after a merge;
  - the refusal of an earlier format's catalog.
- **Desktop.** Tests against a real owner prove:
  - the folder tree in the sources;
  - pressing a year folder views every photograph under it and makes it Develop's folder;
  - `D` opens on it;
  - another client's Develop into it reaches the filmstrip;
  - Develop N opens on the common folder;
  - a double-click in All photographs opens the photograph's folder;
  - Merge HDR leaves the person in Select with the result in the plan's day folder.
- **Verification.** The rendered scenarios that cover these are updated to the new behaviour. The quick tier passes. Rendered and timing runs are left to the owner's direction of 2026-10-11, quick tier only, and are reported as not run.
