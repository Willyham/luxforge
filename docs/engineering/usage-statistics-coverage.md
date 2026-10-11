# Usage statistics coverage

Every operation Luxforge ships, mapped to the usage event it records or to the reason it records none. The events, their fields and the consent and delivery rules are the [design's](../design/usage-statistics.md); the wire shape is the [wire format](../design/usage-statistics-wire.md). Nothing here is recorded while collection is off: every producer first reads the collector's gate, one atomic load, and stops there.

A test in the core (`usage_every_registered_command_is_in_the_coverage_inventory`) reads the [registered commands](#registered-commands) table and fails when a command the method table, a module, a mask command or a task registers is missing from it, or when the table names a command that no longer exists. A new command is added here, with its event or its exemption, in the change that registers it.

## Rules

- **Authoritative boundaries.** A command's event is recorded by the core where the command's outcome stands: the editor service when a recipe transaction commits, the catalog owner when a host method's answer stands, the job table when a job reaches its terminal outcome. Nothing reads the event log, a click handler or a request's arguments to decide.
- **Once per operation.** A retried mutation (the same `request_id`) is answered from the owner's request table or the catalog's request log and records nothing. A no-op, a refusal and a call parked for a pixel read or deferred for the index lane record nothing until their final answer. A job a retry answers from its first attempt records no outcome.
- **Commits.** `tool.commit` is one event per built-in tool group whose layers a committed transaction changed (a layer added, removed, reordered or with another payload, format, mask or artifacts; a layer's identity alone is not a change), in the registry's tool order, with the photograph's image ID. History navigation, a Restore, a no-op and a first-open entry by the `system` actor record none.
- **Continuous gestures.** A drag, a drawn mask or a painted correction is one draft: its ticks (`draft.set`) record nothing, `draft.commit` records its commits once, and a cancelled draft records nothing.
- **Automatic work.** The histogram analysis, maintenance, restoring the remembered workspace at launch and a desktop storing its remembered panels, brush, window or Performance state record nothing.
- **Images.** The keyed image ID is computed when a photograph is adopted as a client's open photograph (`job.adopt`), from the catalog fingerprint the adoption already holds, and kept beside the photograph; a commit to a photograph no client has open computes it from the fingerprint its state holds. The session remembers the last fingerprint and the ID it gave (one entry, in memory, ended with the session, so a new key never reuses it), so one open's preparation outcome, adoption and the desktop's GUI-local producers cost one HMAC between them. An `interaction` carries the image only when it acts on a photograph a client has open. A multi-photograph export, a scan and an add carry none.
- **Generations.** A job and an Auto tone solve capture the consent generation when they start and record their outcome only in that generation, so work begun before an opt-in or across a revocation records nothing.
- **Statistics never count themselves.** `usage.*` commands and their reads record nothing.

## Producers

| Event | Recorded by | When |
| --- | --- | --- |
| `session.start` | the statistics supervisor | when a session begins (not part of this inventory's producers) |
| `app.presented` | the statistics supervisor, from the desktop's visibility reports | each change of visibility after the first report, which is the window's state at launch; off macOS the desktop has no native minimize or hide facts, so it stays at that state |
| `app.idle` | the statistics supervisor, from the desktop's deliberate interactions (key, mouse button, wheel and touch presses, trackpad pinches, evidence script steps) | `idle` when a presented window has had no interaction for one minute, noticed at the worker's own deadline; `active` at the next interaction. A host that reports no visibility records neither |
| `workspace.enter` | desktop | a deliberate switch between Select and Develop; the launch's first workspace is not one |
| `image.open` | core, `job.adopt` | every adoption, so a reopen records it again |
| `interaction` | core (commands) and desktop (GUI-local gestures) | the tables below |
| `tool.commit` | core, the editor service's commit | one per changed built-in group per committed transaction |
| `images.added` | core, the `develop-picks` job's terminal outcome | the photographs its committed batches created, when there is one, whether it completed, was cancelled or failed; linked, relinked, failed and skipped files and a retry's answer add nothing |
| `operation.end` | core: the `prepare`, `export`, `batch-export` and `index-refresh` jobs; the Auto tone solve | the terminal outcome and duration from the start; an export's `count` is the files written, only when it completes |
| `preview.settled` | desktop | the open photograph's committed picture is first on screen at rest: the GPU's picture at rest drawn in tiles (`gpu`), timed as the status bar's "GPU render" figure, or the reference renderer's whole frame when it is the picture (`reference`), timed as its render; a drag's frames, a zoom's hand-over and a frame under the GPU's picture are not. A photograph the GPU draws whole rather than in tiles, such as a small JPEG, records none (open) |
| `process.sample` | the statistics worker | not part of this inventory |

## GUI-local operations

Recorded by the desktop when the gesture completes, because no command records them: they are presentation state, or their command cannot tell a gesture's completion from its ticks or from automatic restoration. The button, the shortcut and the palette entry raise the same message, which records once.

| Operation | Inputs | Event |
| --- | --- | --- |
| Switch to Select or Develop | the workspace switcher; opening a photograph or a development set from Select | `workspace.enter` |
| Fit, 100%, a zoom stop, a typed zoom | title bar, `F`, `1`, `⌘+`/`⌘−`, palette | `interaction` `zoom`, with the image |
| Pinch zoom, scroll and Space pan | trackpad, scroll, Space drag | none: no completion signal exists, and a timer would wake an idle desktop (open) |
| State or tools panel shown or hidden; Select's sources panel or both panels; the filmstrip collapsed | panel buttons, shortcuts, palette, Performance's open | `interaction` `panel-toggle` |
| Develop's information overlay, Select's Info panel | title bar, `I`, palette, Select's panel button | `interaction` `information-panel` |
| Clipping overlay or one clipping triangle | the Clipping button, `J`, a clipping triangle | `interaction` `clipping-overlay` |
| Select's loupe opened or closed | opening or closing the loupe | `interaction` `loupe-toggle` |
| Select's focus check, compare the moment | `Z`, `C` in the loupe | `interaction` `focus-check`, `compare-moment` |
| Previous or next photograph, frame or moment | Develop's filmstrip arrows, the loupe's arrows | `interaction` `navigate-photo` |
| A Select view change: source, filter, sort, grouping, pick filter | sources panel, menus, search | `interaction` `browse-view` |
| A history row previewed; a version's row selected | History and Versions | `interaction` `history-preview` or `version-select`, with the image; choosing the current row returns to current and records nothing |
| The Original hold released | holding `\` | `interaction` `compare-original`, with the image |
| The Before/After comparison entered or left | tapping `\`, palette, Escape | `interaction` `compare-slider`, with the image; the divider's moves have no completion signal and record nothing |
| Copy settings; Paste from previous's capture of the previous photograph's settings | `⌘C` and its sheet; `⌥⌘V` | `interaction` `settings-copy` or `settings-paste-previous`, with the image; the paste itself is `edit.apply-settings`'s `settings-paste` |
| Brush stroke, gradient or radial drawn and released | Masks panel and canvas | the core's `draft.commit` or mask command (below) |
| A correction placed, moved, resized or given another source; a row's setting, reorder or delete; `/`, `⌫`, `⌥↑`/`⌥↓`, the band's reset | Corrections mode's canvas, band, row menu and keys | the core's `draft.commit` or Corrections command (below): one `tool.commit` `corrections` |
| Corrections mode entered or left; a correction selected; a source set or cleared; the New correction settings; pins and outlines hidden (`H`) | Corrections mode | none: desktop view state, as mask selection and the remembered brush are; what is done with them is the commit |
| Slider, curve or wheel drag released | Develop's panels | the core's `draft.commit` (below): one `tool.commit` per changed group |
| Settings sheet change | Settings | the core's `preferences.set` or `flags.set` (below) |
| Theme chosen, imported, exported or deleted | Settings › Appearance | `preferences.set`, `theme.import`, `theme.export`, `theme.delete` (below) |

Shortcuts and palette entries for a command (`⌘Z`, the palette's Undo, a module action's chord) raise exactly the message the command's button raises, so the command's boundary records it once and the desktop records nothing of its own.

## Asynchronous outcomes

A job records its terminal outcome once, through the job table's one finishing path: `completed` when ready, `cancelled` when a client cancelled it, its last client left it or a newer request superseded it, `refused` for a validation, conflict, consent, authority, readiness, resource-limit, preparation or availability error, and `failed` otherwise. A job's acceptance and its outcome are two events for an export and a Develop (`interaction`, then `operation.end` or `images.added`); a refused request is neither. A cancelled or failed Develop keeps the photographs its committed batches created and reports them as added: the count accumulates on the owner as each batch commits, and its user-facing error is unchanged. An Auto tone solve records `completed`, `cancelled` (superseded or disconnected) or `failed`.

## Registered commands

| Command | Event | Emission or exemption reason |
| --- | --- | --- |
| `activity.list` | Exempt (read) | Lists long-running work; a poll. |
| `analysis.request` | Exempt (internal) | The histogram analysis every frame asks for; automatic, not a person's operation. |
| `artifact.collect` | Exempt (internal) | Catalog maintenance. |
| `artifact.inspect` | Exempt (read) | Reads an artifact. |
| `artifact.status` | Exempt (read) | Reads an artifact. |
| `asset.move` | `interaction` photo-move | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `asset.remove` | `interaction` photo-remove | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `asset.restore` | `interaction` photo-restore | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `asset.send-back` | `interaction` photo-send-back | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `asset.state` | Exempt (read) | Reads a photograph's state. |
| `batch.apply-settings` | `interaction` `preset-apply`/`settings-paste`; `tool.commit` per photograph and changed group | The interaction once, when accepted; each photograph the job commits then records its own commits with its image. |
| `batch.export` | `interaction` `export-batch`; `operation.end` export | The interaction when accepted; the job's outcome with `count` = files written when it completes, and no image. |
| `browse.facets` | Exempt (read) | Reads a view's facets. |
| `browse.rows` | Exempt (read) | Reads a page of a view; a scroll. |
| `browse.select` | Exempt (internal) | The grid's selection: a click, not a completed operation; what is done with the selection is recorded by that operation. |
| `browse.view` | Exempt (internal) | Reads a view's summary, which every refresh and scroll asks again; the desktop records `browse-view` when a person chooses another source, filter, sort or grouping. |
| `card.list` | Exempt (read) | Lists cards. |
| `catalog.empty-removed` | `interaction` removed-empty | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `catalog.info` | Exempt (read) | Reads catalog counts. |
| `collection.add` | `interaction` collection-add | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `collection.create` | `interaction` collection-create | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `collection.create-smart` | `interaction` collection-smart-create | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `collection.delete` | `interaction` collection-delete | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `collection.list` | Exempt (read) | Lists collections. |
| `collection.move` | `interaction` collection-move | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `collection.remove` | `interaction` collection-remove | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `collection.rename` | `interaction` collection-rename | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `collection.update-smart` | `interaction` collection-smart-update | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `disk.folders` | Exempt (read) | Lists folders on disk. |
| `draft.begin` | Exempt (internal) | Starts a drag's draft; the gesture records once, at `draft.commit`. |
| `draft.cancel` | Exempt (internal) | A cancelled drag records nothing. |
| `draft.commit` | `tool.commit` × changed groups; a mask command's `interaction` | The completion of a drag or drawn gesture: one commit per changed group however many ticks it had, and for a mask command (a brush stroke, a gradient's handles) its mask interaction. A retry is answered from the request table and records nothing. |
| `draft.read` | Exempt (internal) | Reads the draft. |
| `draft.reapply` | Exempt (internal) | Re-applies the draft after a conflict; part of the gesture. |
| `draft.set` | Exempt (internal) | One drag tick; ticks record nothing. |
| `edit.accept-correction-patch` | `tool.commit` corrections | The frozen patch's one entry, when the owner's accept commits it, with the image; a refused or cancelled accept and the unreferenced artifact it leaves record nothing. |
| `edit.add-correction` | `tool.commit` corrections | One commit when the add commits, with the image. An add without a source is parked while its search is answered off the owner and records once, at its replay's commit; a painted stroke's `draft.commit` records the same event, and a retry records nothing. |
| `edit.apply-settings` | `tool.commit` × changed groups, then `interaction` `preset-apply` or `settings-paste` | One commit per built-in group the settings set changed, then the interaction (by the request's origin: a preset, or a paste), with the image. A no-op or a retry records nothing. |
| `edit.auto-tone` | `tool.commit` basic | Applying Auto tone's solve: one commit per changed group (Basic). The solve itself is `query.auto-tone`. |
| `edit.crop` | `tool.commit` crop | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.crop-fit` | `tool.commit` crop | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.crop-reset` | `tool.commit` crop | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.delete-correction` | `tool.commit` corrections | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.pick-raw-neutral` | `tool.commit` basic | The RAW neutral pick's commit to the RAW development, which Basic presents. |
| `edit.reorder-correction` | `tool.commit` corrections | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-basic` | `tool.commit` basic | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-corrections` | `tool.commit` corrections | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-curve` | `tool.commit` curve | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-detail` | `tool.commit` detail | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-lens-profile` | `tool.commit` lens | One commit when the lens group changed. |
| `edit.reset-look` | `tool.commit` look | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-mixer` | `tool.commit` mixer | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-perspective` | `tool.commit` perspective | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-presence` | `tool.commit` presence | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.reset-vignette` | `tool.commit` vignette | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.select-lens-profile` | `tool.commit` lens | One commit when the lens group changed. |
| `edit.set-basic` | `tool.commit` basic | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-curve` | `tool.commit` curve | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-detail` | `tool.commit` detail | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-look` | `tool.commit` look | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-mixer` | `tool.commit` mixer | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-perspective` | `tool.commit` perspective | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-presence` | `tool.commit` presence | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-raw` | `tool.commit` basic (RAW white balance) | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-raw-blue-gain` | `tool.commit` basic (RAW white balance) | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-raw-red-gain` | `tool.commit` basic (RAW white balance) | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.set-vignette` | `tool.commit` vignette | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.transform` | `tool.commit` crop | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. |
| `edit.update-correction` | `tool.commit` corrections | One commit per built-in group the committed transaction changed, with the image; a no-op, a retry or an automatic first-open entry records nothing. A moved pin, a moved source, a resize, a kind or a setting and `/`'s next source are each this command or its `draft.commit`. |
| `event.list` | Exempt (read) | Lists the catalog's events (photo groups). |
| `events.since` | Exempt (read) | Reads the event log; never copied into statistics. |
| `events.wait` | Exempt (read) | Holds a read of the event log. |
| `export.jpeg` | `interaction` `export`; `operation.end` export | The interaction when accepted (image when the photograph is open); the job's terminal outcome with `count` 1 when written and the open photograph's image. A refusal records neither; a cancelled or failed export carries no count. |
| `export.plan` | Exempt (read) | Plans an export (output stage, suggested destination). |
| `flags.list` | Exempt (read) | Lists feature flags. |
| `flags.set` | `interaction` preferences-change | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `folder.create` | `interaction` folder-create | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `folder.delete` | `interaction` folder-delete | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `folder.list` | Exempt (read) | Lists catalog folders. |
| `folder.merge` | `interaction` folder-merge | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `folder.move` | `interaction` folder-move | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `folder.rename` | `interaction` folder-rename | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `history.inspect` | Exempt (read) | Reads one history entry. |
| `history.lineage` | Exempt (read) | Reads history lineage. |
| `history.list` | Exempt (read) | Reads a history page. |
| `history.redo` | `interaction` history-redo | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. Carries the image of the open photograph it acted on. |
| `history.restore` | `interaction` history-restore | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. Carries the image of the open photograph it acted on. A Restore is history's, so it records no tool commit. |
| `history.undo` | `interaction` history-undo | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. Carries the image of the open photograph it acted on. |
| `index.add-folder` | `interaction` indexed-folder-add | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `index.folders` | Exempt (read) | Lists indexed folders. |
| `index.refresh` | `operation.end` catalog-scan | The listing job's terminal outcome and duration; no image or count. A request joining a listing already running records no second one. |
| `index.remove-folder` | `interaction` indexed-folder-remove | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `job.adopt` | `image.open` | Each adoption of a prepared photograph as the client's open photograph; its image ID is computed here, once, from the fingerprint the adoption holds. |
| `job.cancel` | Exempt (internal) | A cancellation is recorded as the job's own `operation.end` outcome `cancelled`. |
| `job.read` | Exempt (read) | Reads a job; a poll. |
| `job.read-candidate` | Exempt (read) | Reads a chunk of a task candidate's pixels. |
| `job.release-candidates` | Exempt (internal) | Storage housekeeping for a task's candidates, as the budget, a cancel and closing the catalog also release them; accepting one is `edit.accept-correction-patch`. |
| `job.wait` | Exempt (read) | Holds a read of a job. |
| `library.inspect` | Exempt (read) | Reads one library change. |
| `library.journal` | Exempt (read) | Reads the library journal. |
| `library.redo` | `interaction` library-redo | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `library.undo` | `interaction` library-undo | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `mask.add-colour-range` | `interaction` mask-component-add (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.add-colour-range-sample` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.add-linear` | `interaction` mask-component-add (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.add-luminance-range` | `interaction` mask-component-add (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.add-radial` | `interaction` mask-component-add (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.add-stroke` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.create-colour-range` | `interaction` mask-create (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.create-linear` | `interaction` mask-create (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.create-luminance-range` | `interaction` mask-create (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.create-radial` | `interaction` mask-create (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.delete` | `interaction` mask-delete (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.delete-colour-range-sample` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.delete-component` | `interaction` mask-component-delete (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.delete-stroke` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.duplicate` | `interaction` mask-duplicate (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.list` | Exempt (read) | Lists masks. |
| `mask.rename` | `interaction` mask-rename (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.rename-component` | `interaction` mask-rename (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.reorder` | `interaction` mask-reorder (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.reorder-component` | `interaction` mask-reorder (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.sample-input` | Exempt (read) | A pixel query for a mask's input; a read. |
| `mask.set-amount` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.set-colour-range` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.set-component-invert` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.set-component-mode` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.set-invert` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.set-linear` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.set-luminance-range` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `mask.set-radial` | `interaction` mask-edit (+ `tool.commit` if layers changed) | On a committed change, with the image; recorded from the committed action, so the method and a drawn gesture's `draft.commit` record the same event. |
| `module.list` | Exempt (read) | Discovery. |
| `module.permission.deny` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.permission.grant` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.permission.list` | Exempt (read) | Lists grants. |
| `module.permission.revoke` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.profile.create` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.profile.remove` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.resource.install` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.resource.list` | Exempt (read) | Lists resources. |
| `module.resource.remove` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.settings.clear-secret` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.settings.read` | Exempt (read) | Reads module settings. |
| `module.settings.reset` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.settings.set` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.settings.set-secret` | Exempt (privacy) | Module capability administration: names a module, a profile, a resource or a secret, and no shipped adjustment tool has capability settings; no registry value. |
| `module.status` | Exempt (read) | Reads module status. |
| `pick.develop` | `interaction` `develop-picks`; `images.added` | The interaction when accepted (opening a file is a Develop of it); the job's count of newly created photographs, when there is one, however it ended. Linked and relinked files, failures and a retry's answer add nothing. |
| `pick.list` | Exempt (read) | Lists picks. |
| `pick.plan` | Exempt (read) | Plans a Develop of picks; its `pick.develop` is the operation. |
| `pick.set` | `interaction` pick-set | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `preferences.read` | Exempt (read) | Reads preferences. |
| `preferences.set` | `interaction` preferences-change | Only a deliberate preference the event log announces (Settings); the remembered workspace, brush, window, export folder and Performance state a desktop stores as it goes are not. |
| `preset.capture` | Exempt (read) | Captures settings for a preset form; the save is `preset.create`/`preset.update`. |
| `preset.create` | `interaction` preset-create | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `preset.delete` | `interaction` preset-delete | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `preset.export` | `interaction` `preset-export` | When it answers: the desktop writes the file it returns. |
| `preset.groups` | Exempt (read) | Lists preset groups. |
| `preset.import` | `interaction` preset-import | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `preset.inspect` | Exempt (read) | Reads a preset file before import. |
| `preset.list` | Exempt (read) | Lists presets. |
| `preset.read` | Exempt (read) | Reads a preset. |
| `preset.update` | `interaction` preset-update | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `preview.compare` | Exempt (internal) | The Before/After comparison's session state, also sent per drag tick of its divider; the desktop records `compare-slider` when the comparison is entered and left (the divider's moves have no completion signal). |
| `preview.read` | Exempt (read) | Reads a catalog preview tier; a redraw. |
| `preview.region` | Exempt (read) | Reads a 100% region; a redraw. |
| `preview.return-current` | Exempt (internal) | Ends a history preview or an Original hold; the desktop records the operation that began it. |
| `preview.select` | Exempt (internal) | A per-client selection that is also the Original hold's and a comparison's mechanism; the desktop records `history-preview`, `version-select` and `compare-original` at the gesture. |
| `query.auto-tone` | `operation.end` auto-tone | The solve's terminal outcome (completed; cancelled when superseded or disconnected; failed) and duration, with the open photograph's image; its commit is `edit.auto-tone`. |
| `query.correction-sources` | Exempt (read) | Ranks automatic sources without writing; the add or update that uses one is the commit. |
| `query.corrections` | Exempt (read) | Reads the Corrections layer as stored (pins, outlines and rows). |
| `query.lens-profiles` | Exempt (read) | Lists lens profiles. |
| `query.neutral-sample` | Exempt (read) | A pixel query (the neutral picker); its commit is `edit.set-basic`/`edit.set-raw`. |
| `query.sample-curve` | Exempt (read) | A pixel query for the curve picker. |
| `query.sample-look` | Exempt (read) | A pixel query for the Look. |
| `recipe.describe` | Exempt (read) | Reads the recipe. |
| `render.locate` | Exempt (read) | A point query. |
| `render.sample` | Exempt (read) | A point query (the hover readout). |
| `render.transform` | Exempt (read) | A point query. |
| `resources.read` | Exempt (read) | Reads resource figures. |
| `schema.list` | Exempt (read) | Discovery. |
| `session.state` | Exempt (read) | Reads the session. |
| `source.check` | Exempt (read) | Checks originals' availability; the result is a status, not an operation. |
| `source.find` | `interaction` `originals-find` | When accepted; the search's rows are its own result. |
| `source.inspect` | Exempt (read) | Reads capture metadata. |
| `source.locate` | `interaction` locate-original | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `source.missing` | Exempt (read) | Lists missing originals. |
| `source.prepare` | `operation.end` source-preparation | The job's terminal outcome (completed, cancelled, refused, failed) and duration from queueing, with the image when it prepared one; recorded only in the generation it was queued in. |
| `source.relink` | `interaction` originals-relink | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `theme.delete` | `interaction` theme-delete | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `theme.export` | `interaction` `theme-export` | When it answers: the desktop writes the file it returns. |
| `theme.import` | `interaction` theme-import | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. |
| `theme.inspect` | Exempt (read) | Reads a theme file before import. |
| `theme.list` | Exempt (read) | Lists themes. |
| `theme.read` | Exempt (read) | Reads a theme. |
| `usage.prompt.claim` | Exempt (privacy) | Statistics consent; statistics commands never record. |
| `usage.read` | Exempt (read) | The statistics' own read; statistics commands and their reads never record. |
| `usage.set` | Exempt (privacy) | Statistics consent; statistics commands never record. |
| `version.create` | `interaction` version-create | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. Carries the image of the open photograph it acted on. |
| `version.delete` | `interaction` version-delete | When it changed something (announced in the event log): never for a no-op, a refusal or a retry. Carries the image of the open photograph it acted on. |
| `version.list` | Exempt (read) | Lists versions. |
| `view.set` | Exempt (internal) | Zoom and pan session state, also sent per pinch and scroll tick; the desktop records `zoom` at a discrete zoom (Fit, 100%, a stop, a typed value). Pan and pinch have no completion signal and are not recorded (open). |
| `volume.list` | Exempt (read) | Lists volumes. |
| `workspace.set` | Exempt (internal) | Panels, overlays and mode; also how the desktop restores the remembered workspace at launch, which is excluded. The desktop records `panel-toggle`, `information-panel` and `clipping-overlay` at the toggle. |
