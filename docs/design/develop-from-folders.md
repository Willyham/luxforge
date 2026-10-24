# Develop from catalog folders

Status: implemented. Develop holds the catalog folder chosen in Select. This note covers the folders Develop N makes, the Catalog sources list and the [HDR merge](hdr-merge.md) actions; the [catalog](catalog.md) and HDR designs describe them in place.

## Decisions

Pressing a catalog folder in Select changes what Develop holds, parent folders are selectable as a whole ("2023" over "Japan" and "Korea"), and Develop N organizes photographs as a tree, "2023 › Foo › Nov", so a place's photographs from several days are seen and developed together:

- **The catalog folder is what Develop holds.** Develop's filmstrip is the catalog folder last chosen in Select, with its subfolders, and follows it live: a photograph developed, merged, moved or removed changes it. Pressing any folder in Select chooses it.
- **Year › Place › Day.** Develop N files each photograph under a year folder, a place folder and a day folder: "2026 › Konstanz › 12 Sep". Year and place folders are reused by name, so later picks from the same place and year join it, and a trip's days are siblings under its place.
- **Years are real catalog folders.** Develop makes "2026" as an ordinary top-level catalog folder, which can be selected, renamed and moved like any other. The Catalog sources list shows the folder tree.
- **One Merge action.** **Merge HDR** makes the result in its catalog folder and stays in Select. [Panorama merges](panorama-merge.md) follow: **Merge panorama**.

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
| `{kind: new, name, parent_id?}` | One new folder. |

- **Found or made.** Each folder of a path is found by its name among its siblings, ignoring case as names are compared. Otherwise it is made by the batch that first puts a photograph in it, in that batch's library change. A `place` choice never adds " 2": reusing the folder is the point.
- **Days.** The capture day is the camera-local day of the photograph's capture time, as read when it is developed. A file the index did not list, such as one opened directly, still gets its day from its header. A day folder is named with the day and the month in English, with no leading zero or year: "3 Sep", "12 Nov". An event that crosses a year files each photograph under its own year.
- **Spans.** A place folder Develop makes records the span and key of the events it was made from, as a `new` folder does. Year and day folders record none, and a folder that is found is not changed.
- **The plan's answer.** Each event also answers `days`, its picks' distinct camera-local days in ascending order, and `undated`, how many of its picks have no capture time, so a client can show the folders it will make: "2026 › Konstanz › 12 Sep, 13 Sep". Batches still never cross an event; one batch may make several folders.
- **Opening a file.** A file opened directly is developed with `into: []`, so it takes the plan's proposal. Its parent folder's name is the place, unless an earlier photograph from that folder was developed. `EditorService::import`, the blocking helper for tests and tools, names its top-level folder after the disk folder.

### Order of folders

`folder.list` keeps parents before their children. Siblings are ordered by their names:

1. Years, four digits, newest first.
2. Days ("12 Sep"), in calendar order.
3. Everything else by name, ignoring case.

The desktop lists folders in this order.

### Select's Catalog sources

Under All photographs and Recently developed, the catalog folder tree from the top level, in `folder.list`'s order. Each folder has its count, covering its subfolders, and a disclosure when it holds folders. The newest year folder is open by default, and so is the path to the folder Develop holds. Pressing any folder, a year or a place among them, views it with its subfolders and makes it Develop's folder. A folder of undated photographs or an empty folder is listed like any other.

### Develop's folder

The desktop holds one **Develop folder**, its own view state like the shown workspace, recorded in evidence with each captured frame.

- **What chooses it.**
  - Viewing a catalog folder in Select.
  - A double-click on a photograph in any catalog view: that photograph's own folder, opening on it.
  - Develop N: the deepest folder holding every folder its choices named, or the first of them when they share none. It opens on the first photograph it developed. A choice names its place folder ("2026 › Kawaguchiko"), its existing folder or its new folder, never the day folder inside it, so photographs an earlier Develop put in the same place are in the filmstrip with the new ones. A linked or relinked photograph names its own folder.
  - Opening a file: the folder its Develop's choice named, its place folder rather than its day folder.
- **What keeps it.** Browsing files, events, other catalog views and Missing originals leaves it as it was. A merge of the folder makes it the folder it went into. Deleting the folder clears it.
- **Across launches.** Where Develop was left is remembered as the `develop_last` preference, `{catalog_id, folder_id, asset_id}`: the Develop folder and the photograph last open in Develop, either `null`, with the identity `catalog.info` answers for their catalog ([preferences](preferences.md)). Each change of either is stored; closing a photograph or moving between two keeps the last one, and a deletion that leaves neither stores `null`. Every launch reads the catalog's identity and, when the preference is this catalog's, where Develop can open again, in one task off the update loop:
  - **The folder is still listed** (`folder.list`), a missing-original folder included: it is read for the filmstrip on a client of its own, as `D` reads it, and becomes the Develop folder. Select views it too, sending the `browse.view` that pressing it sends. Develop opens on the remembered photograph while the strip holds it, and otherwise on the strip's first, the rule `D` follows; a photograph removed or moved out is not in the strip. An empty folder is held, and nothing opens.
  - **The folder is gone, the photograph is not**: the photograph is found in All photographs on a client of its own (`browse.view`, `browse.select` of it, `browse.rows` of its row) and opens with its own folder in the filmstrip, as a double-click in a catalog view opens it. Select keeps no view.
  - **Neither survives**, or the preference is another catalog's: the launch is the one it would be with nothing remembered, saying nothing, and the preference is kept.

  Only a launch that opens no file lands there; a file opened at launch takes its own folder. It lands only while nothing was viewed, chosen or opened meanwhile, so it never opens a second photograph over one already open. Develop opens through the open path every open shares (`source.prepare`, then `job.adopt`), so a photograph whose original is missing opens as any move to it does. A catalog with no photographs still starts in Select. Landing writes nothing back: the preference already says where it landed.
- **The filmstrip** holds the folder's photographs with its subfolders, captured oldest first, removed photographs excepted. It is read through a client of its own, so Select's view and selection are untouched, in `browse.rows` windows of at most 1,000, with at most 100,000 photographs. Its header is the folder's path ("2026 › Konstanz") with the active photograph's place in it ("3 of 18"). It is read again after any library change that reaches the desktop, its own or another client's, keeping the open photograph's place. An open photograph that left the folder stays open, with no cell outlined.
- **`D` and the Develop tab** open Develop on the Develop folder: on the photograph already open when it is in the folder, otherwise on the folder's first photograph. A photograph opened with nothing chosen, from Missing originals' Locate for example, is shown without a filmstrip. With neither, the tab is disabled and `D` says "Choose a catalog folder to develop". An empty folder says it has no photographs.
- **Agent equivalent.** An agent reads what Develop holds as the folder's view, `browse.view` of `{kind: catalog-folder, folder_id, subfolders: true}`. There is no separate set to replace or append to. `pick.develop`'s report names each photograph's `folder_id`, the folder it is in, and `chosen_folder_id`, the folder its choice named: the place or existing folder over the day folder it was filed in, else `folder_id`.

### Merging

- **Actions.** A bracket header offers Pick all N and **Merge HDR**. The selection menu's HDR group offers Merge HDR… ("Merge the frames into one HDR photograph in the catalog"). It opens the [merge preview](hdr-merge.md#in-the-desktop) first, with Cancel and Merge; ⌥-click merges at once with the last settings. The result is made in its folder, and the person stays in Select.
- **Choosing the folder.** `merge.plan` proposes the folder as Develop would for the reference frame. `merge.create` takes `folder?`, a folder choice as above: the plan's proposal by default, resolved for the reference frame's capture day. The folders it makes are recorded as their own library change (`folder.create`, the request's actor and identity) just before the publication's, in the same transaction, so undoing the merge leaves them: the result it keeps in Removed is still in them. The preview sheet's folder control shows the plan's proposal and sends it, or `{kind: existing, folder_id}` for a folder the person picks; the desktop never sends `folder.create` first.
- **Picks.** Over files, a bracket whose stack has an active result is decided for Moments without a pick. The Picked segment and Develop N count file picks only, and a result is never shown picked: it carries the "In the catalog" badge, and `P` on it picks nothing and says so over the grid ([HDR merges](hdr-merge.md#in-the-desktop)).

### Catalog format

The catalog format is 2, which holds the kind-tagged merges of [HDR merges](hdr-merge.md#durable-derived-source) ([storage](versions-and-lineage.md#storage-the-catalog-format)); a catalog of any other format is refused with "choose a new catalog path".

## Delegated choices

Delegated to the implementer within this contract:

- the placeholder place name;
- the defaults for which folders are open;
- how the filmstrip's client is kept;
- the window it reads;
- the wording of status sentences;
- how a deep folder path shortens in the filmstrip header.

Recorded defaults for [picking up where Develop was left](#develops-folder), which the owner can revise:

- One folder and photograph are remembered, for the catalog last used, keyed by its `catalog_id`.
- Only a launch with no file to open lands there, and only while nothing was viewed, chosen or opened meanwhile.
- It lands on the remembered photograph in the remembered folder, else on the folder's first; with the folder gone, on the photograph in its own folder; else nowhere.
- Landing in a remembered folder also views it in Select; landing in the photograph's own folder leaves Select with no view, as a double-click does.
- An empty remembered folder is held and opens nothing.

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
  - the refusal of a catalog of another format.
- **Desktop.** Tests against a real owner prove:
  - the folder tree in the sources;
  - pressing a year folder views every photograph under it and makes it Develop's folder;
  - `D` opens on it;
  - another client's Develop into it reaches the filmstrip;
  - Develop N opens on the common folder of the folders its choices named, so a later Develop into the same place holds the place with every photograph in it;
  - a double-click in All photographs opens the photograph's folder;
  - Merge HDR leaves the person in Select with the result in the plan's day folder;
  - the folder viewed and the photograph open are stored as `develop_last` with the catalog's identity; a launch lands in Develop on the remembered photograph with its folder in the filmstrip, writing nothing back; it opens the folder's first when the photograph left the folder, and the photograph with its own folder when the folder is gone; and with nothing left, or another catalog's preference, it is the launch it would be with nothing remembered (`cargo test -p luxforge-app develop_last`).
- **Verification.** The rendered scenarios cover these. The quick tier passes. Its verification is the quick tier only; rendered and timing runs are reported as not run.
