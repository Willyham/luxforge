# Lightwell user guide

What works today: opening a JPEG, exact transforms and a Lightroom-style crop and straighten tool in the Develop workspace, all delivered as tool modules with generated controls, persistent history and the JSON API, verified on macOS. Export, Locate and MCP are planned; see [feature status](features.md).

Basic exposure, tone, white balance and color controls, plus a histogram and clipping inspector, have a [proposed design](design/basic-and-histogram.md). They are not available yet.

## Start the editor

After [developer setup](engineering/development.md), start an optimized build with a catalog and an optional JPEG:

```sh
cargo xtask develop --catalog /path/to/catalog.sqlite --open /path/to/photo.jpg
```

Omit `--catalog` to use the platform configuration directory. `--data-root DIRECTORY` isolates config, cache and log paths. `cargo xtask develop --debug` is an unoptimized build for debugging and is unsuitable for timing.

`--developer` lists the test modules (pixel proof) under a Developer section of the tools panel; without it the workspace shows only photo-editing modules, and the JSON API lists every module either way. `--disable-module lightwell.crop` (or another built-in id) registers that module as unavailable, which keeps its stored layers readable and reports them instead of rendering without them.

For agent-driven API or rendered checks on macOS, add `--background` to keep the editor from taking desktop focus. Use a separate test catalog or `--evidence-dir NEW_DIR`; background evidence runs capture the editor and exit automatically. Smoke and diagnostic harnesses use background launches by default on macOS. Launch normally for keyboard, mouse and native-dialog interaction.

Open references an existing supported sRGB or greyscale JPEG without copying or modifying it. Cmd+O on macOS and Ctrl+O elsewhere opens the picker. EXIF orientation is applied once before any edit. The catalog stores stable identities, the source fingerprint, ordered operations and history. It is not a backup of the original photo.

Choose Copy beside the status text at the bottom of the window to copy the complete message, including any error, to the clipboard. The message stays visible after copying. The rest of the status bar reports how many clients the live API has, what the renderer is doing or how long the displayed frame took, and the current zoom with what it means on this display.

## The workspace

The window has five regions: a title bar with the file name, Open, the view control, Compare, Undo, Redo and two panel toggles; the state panel on the left (versions, history, recipe); the photograph in the middle with a floating mode strip under it; the tools panel on the right; and a status bar. Cmd+Option+[ and Cmd+Option+] hide and show the side panels, and the canvas takes whatever remains. Panel visibility, the canvas mode and the thirds overlay are session state, reported by `session.state` and settable through `workspace.set` like zoom.

Cmd+K opens the command palette: type to filter every module action, reset and canvas mode plus Fit, 100%, Undo, Redo, Return to current, Restore, Pointer, Thirds and the panel toggles, then Enter or click runs the entry through the same path the control uses. Right-click any generated control and choose Copy as JSON request to put the exact `edit.<action>` request for its current values, with the current expected revision, on the clipboard; the crop draft's Apply offers its `edit.crop` request the same way.

## Edit and inspect

The tools panel is generated from the registered tool modules, one collapsible section per module in registry order. A header shows the module title, an accent dot when a layer of that module is in the current recipe, the module's hint while collapsed and its reset action when it declares one; an unavailable module shows its reason and cannot expand. Number fields are sliders: drag the thumb, or click the value to type; a drag changes nothing until release, key-up or Enter, which runs the control's action once. Invalid text stays editable with the message naming the declared range and commits nothing; double-click a label to reset one field. Enumerations are segmented controls or chips, colours are three fields, groups carry their own reset, and actions are buttons enabled only when the current state is editable and every value they need is valid. Tab and Shift+Tab move between fields. A control kind this build cannot draw is shown as an explicit unsupported-control message rather than hidden. During a historical preview every control stays visible and disabled, and the section names why.

Pixel proof lives under Developer with `--developer`. It accepts integer x/y coordinates and RGB values from 0 to 255. Coordinates are content coordinates: the photograph after EXIF orientation, with a top-left origin, x right and y down, regardless of any crop or rotation applied afterwards. Clicking the photo at Fit or any zoom fills X and Y with the content pixel shown under the pointer without committing anything, so a click in the corner of a cropped view names the pixel of the photograph that is drawn there. Changing the crop later never moves an edit; it only changes what is visible. Apply pixel creates one layer and one attributed history action when the resulting pixel changes. Invalid, out-of-bounds and same-value requests add nothing.

Exact transforms are Rotate left, Rotate right, Mirror horizontal and Flip vertical. Quarter-turns swap dimensions. Repeated transforms fold into one orientation layer while it is the last layer of the stack, so four Rotate right actions leave one neutral layer and four history actions to undo through; a transform made after a crop starts a new orientation layer so the crop is carried with it. Pixel edits always sit before the transforms and crop, so they move with the image.

Fit shows the whole image, centred with a margin of canvas around it. Type a percentage from 10 to 1600 in the field beside it and press Enter, or choose 100%; neither Fit nor 100% is selected while a typed percentage is in force. On a high-DPI display, 100% maps one source pixel to one physical framebuffer pixel. Both scroll axes pan content larger than the viewport.

Hold Compare in the title bar, or hold `\`, to see the Original entry for as long as the key or the button is down; releasing returns to whatever was selected before, without touching history or anything else in the session. Compare is refused while a crop draft is open, and the status bar says so.

A strip floats under the photograph with the pointer, one entry per registered module that declares a canvas mode, and the Thirds overlay. Thirds draws two guides each way over the fitted photograph; at a percentage zoom, and while a crop draft is open, the overlay is left to the crop rectangle's own guides.

Cards appear over the top of the canvas when something needs saying: a draft that was changed elsewhere, a preview that is stale because a stored layer's module is unavailable, an original that cannot be found, or a rendering limit. They name the cause and offer only the actions the core allows; none of them blocks the rest of the screen.

### Keyboard

Letters act only when no text field has focus. `F` fits, `1` is 100%, `O` toggles the thirds overlay, `V` returns to the pointer, and each module's declared letter (`R` for crop and straighten) enters its canvas mode. `\` holds Compare. Cmd+Option+[ and Cmd+Option+] show and hide the two side panels. Cmd+O / Ctrl+O opens a file, Cmd+Z / Ctrl+Z and Shift+Cmd+Z / Shift+Ctrl+Z undo and redo, and Tab and Shift+Tab move between fields.

### Crop and straighten

The crop editor appears in the tool panel as soon as a registered module declares a crop frame; the ratios, the angle range and the action it commits all come from that module's descriptor.

Choose Crop to open a draft. The surface then shows the crop layer's own input stage, which is the stack rendered up to but not including that layer, so layers after an existing crop are not drawn while you adjust it. Everything outside the crop rectangle is dimmed, and the rectangle carries a thirds overlay, a border and eight handles.

- Drag a corner handle to move two edges, or a side handle to move one with the opposite edge fixed. Drag inside the rectangle to move the composition; it slides along a boundary instead of stopping. The rectangle never leaves the photograph, so a crop never has an empty corner, and it never shrinks below one pixel on either axis.
- Hold Option (Alt on Windows and Linux) while dragging any handle to apply one scale factor about the fixed centre, keeping the current width to height even with no ratio locked. The centre does not move.
- Choose a ratio preset to keep the centre and fit the largest rectangle of that ratio inside the current one. Custom takes the two extents typed beside it. Lock ratio pins whatever the rectangle currently is, Unlock ratio releases it, and Swap inverts a locked ratio the same way.
- Type an angle from −45 to 45 degrees and press Enter, or use the −0.5° and +0.5° buttons. Every angle is measured against the last handle, move or ratio change, so sweeping the angle away and back returns exactly the rectangle you had. Larger rotations are the quarter-turn transforms.
- Turn Straighten guide on and drag a line along something that should be level; releasing rotates by the angle that makes that line horizontal or vertical, whichever is nearer.
- Hold Space and drag to pan at a percentage zoom. Fit, 100% and a typed percentage all work while drafting, and 100% still shows one input pixel per physical pixel.

The panel prints the input stage, the rectangle in whole box pixels, the resulting output size and the exact values the draft would commit.

Apply, or press Enter, commits one action and one new history entry, adjusting the existing crop layer in place and keeping its identity or appending one when there is none. Cancel, or press Escape, discards the draft and changes nothing. Enter and Escape act only when no field has just consumed the key. Reset crop is the module's own control: it commits the neutral crop through history and ends the draft. No pointer movement commits anything, and the rotation shown while drafting is a display filter — the committed render is what counts.

Selecting a historical state pauses the draft rather than discarding it: the historical preview is shown and Return to current resumes drafting. If anything else changes the photograph while a draft is open — another client, or your own Undo, Redo or Restore — the draft is kept and marked "Changed elsewhere". Apply is refused until you choose Discard, which drops the draft, or Reapply, which re-reads the current stack, rebases the draft onto it keeping the angle and the composition as far as it fits, and lets you apply normally.

## History

History lists Original and every committed action newest first, with its sequence, a label and the actor. The label comes from the action's declared summary ("Crop 16:9", "Rotate right") or its title, and is stored with the entry. The filled accent marker is current and an accent outline is the previewed entry; selecting the current row keeps you on the live state, and selecting another row previews that immutable snapshot without changing current state or revision, the status bar says which entry is shown, and Return to current and Restore appear under the list. Restore appends a new action containing the selected recipe. Editing is disabled during a historical preview.

The recipe block lists the displayed entry's layers in processing order with the module title and the module's own summary of each layer, from `recipe.describe`; a layer whose module is unavailable shows the reason instead. Each row also carries the parameter `values` that layer represents, when its module reports them, so a client can show the settings of the entry on screen.

Versions are chips naming saved states. Choose + to reveal the name field and Save; each chip carries its entry number, selecting one previews it, and Restore brings it back as a new action. Right-click a chip to delete the name; the entry stays in history. History rows marked branch were undone and replaced by later edits; they remain available for preview, restore and versions.

Undo and Redo navigate saved states without appending rows. Cmd+Z / Ctrl+Z and Shift+Cmd+Z / Shift+Ctrl+Z invoke the same service as the buttons. A new edit clears shortcut redo while every older entry remains available for preview or Restore. Layers, history, navigation state and stable IDs survive reopening the catalog.

Missing or changed sources keep their catalog data and report why rendering is unavailable. Only the current catalog format (3, which stores each entry's label) and operation formats are supported during pre-release development. Unsupported formats fail explicitly; Lightwell never silently resets or drops them. If a catalog format is rejected, start with a new path using `--catalog /path/to/new-catalog.sqlite` and import the originals again.

## JSON automation

The headless owner reads one JSON request per line and writes one response per line. Diagnostics stay off stdout:

```sh
printf '%s\n' \
  '{"id":"schema","method":"schema.list","params":{}}' \
  '{"id":"import","method":"catalog.import","params":{"path":"/path/to/photo.jpg"}}' \
  | target/release/lightwell-json --catalog /path/to/catalog.sqlite
```

A request has `id`, `method` and `params`. A success carries the matching `id`, an event `sequence` and `result`; a failure carries a structured `error`. Start with `schema.list` for the authoritative method list and `catalog.list` for the referenced assets. Edit actions are generated from the registered tool modules: `module.list` returns every module with its effects, actions, parameter descriptors (kind, range, unit, default), semantic controls, hint, reset action, canvas title and shortcut, summary templates and developer flag, and each action `<id>` is callable as `edit.<id>` with its parameters as top-level fields beside `asset_id` and `mutation`. `recipe.describe` lists an entry's layers with each module's summary; `workspace.set` and `session.state` carry the per-client panels, canvas mode and thirds overlay beside the view; every history entry carries its rendered `label`. Today that is `edit.set-pixel` (`x`, `y`, `rgb`), `edit.transform` (`transform`) and the crop module's three actions. Parameters are checked against the descriptors before the module sees them, so every client gets the same structured validation error. A number parameter may declare a `step` and a display `precision` for the control that drives it; they are hints, and a request is never rounded to them. An action listed with `patch: true` takes whichever of its fields you name: the fields you send are validated, nothing declared is filled in, the module merges them over what it already stores, the entry records the fields as sent, and a patch that changes nothing writes no entry. `version.create`, `version.list`, `version.delete` and `history.lineage` cover named states and the undo-parent chain. `render.sample` reads one rendered pixel and `render.locate` maps a rendered pixel back to the content pixel it shows. Mutations require `asset_id` and a `mutation` object:

```json
{"id":"rotate","method":"edit.transform","params":{"asset_id":"asset-…","mutation":{"expected_revision":0,"request_id":"rotate-1","actor":"my-client"},"transform":"rotate-right"}}
```

`edit.crop-fit` (`aspect`, optional `aspect-width`/`aspect-height` with `aspect: "custom"`, `angle`, optional `center-x`/`center-y`) fits the largest rectangle of a ratio about a center without computing the box geometry by hand:

```json
{"id":"crop-169","method":"edit.crop-fit","params":{"asset_id":"asset-…","mutation":{"expected_revision":3,"request_id":"crop-1","actor":"my-client"},"aspect":"16:9"}}
```

`edit.crop` (`angle`, `x`, `y`, `width`, `height`) sets the straightening angle and rectangle exactly as persisted, normalized to the rotated box:

```json
{"id":"crop-exact","method":"edit.crop","params":{"asset_id":"asset-…","mutation":{"expected_revision":4,"request_id":"crop-2","actor":"my-client"},"angle":0,"x":0.1,"y":0.1,"width":0.8,"height":0.6}}
```

Both actions reject a rectangle that would need an empty corner with a structured `validation` error naming the offending corner, its mapped input coordinates and how far outside the input stage it lands. Every crop request updates the stack's one crop layer in place, keeping its layer ID, or appends one when the stack has none; a request equal to the saved payload is a no-op. `edit.crop-reset` takes no parameters, returns an existing crop layer to the neutral payload (`angle 0, x 0, y 0, width 1, height 1`) and is a no-op without one.

A gesture that changes a setting over and over — a slider drag, a held arrow key — is a **draft**, so it costs one history entry instead of one per step. `draft.begin {asset_id, action}` opens this client's one draft and answers `{draft_id, action, asset_id, base_revision, draft_revision, fields, conflicted}`; `draft.set {draft_id, fields}` validates and merges the named fields against the action's parameter descriptors and counts up `draft_revision`; `draft.read {draft_id}` reads it back; `draft.cancel {draft_id}` ends it and commits nothing; `draft.commit {draft_id, mutation}` runs the action with the accumulated fields and ends the draft, returning the ordinary mutation result, so a gesture that returned to its start is a `no-op` with no entry. Only the commit changes the catalog; everything else is session state, reported by `session.state` and gone when the client disconnects. A draft is refused while this client already holds one, while a historical entry is previewed, and for an unknown action. Nothing is committed until the commit, and `render.sample {asset_id, x, y, draft_id}` evaluates the draft's settings so a readout during a gesture matches what committing would produce.

```json
{"id":"begin","method":"draft.begin","params":{"asset_id":"asset-…","action":"crop"}}
{"id":"drag","method":"draft.set","params":{"draft_id":"draft-…","fields":{"angle":2.5}}}
{"id":"release","method":"draft.commit","params":{"draft_id":"draft-…","mutation":{"expected_revision":5,"request_id":"straighten-1","actor":"my-client"}}}
```

`mutation.expected_revision` must be the draft's `base_revision`. If any client commits while the draft is open, the draft is kept and marked `conflicted: true`; committing it is then refused with a `conflict`. `draft.cancel` discards the gesture, or `draft.reapply {draft_id}` rebases it on the current revision and keeps only the fields this client set, so an unrelated field another client changed is retained by the commit that follows.

While the desktop owns a catalog it creates `CATALOG.live-session.json` beside it, recording a `127.0.0.1` address and a token. That session accepts the same newline-delimited requests with the token as the request's top-level `token`. The file is owner-readable on Unix and removed on orderly shutdown. Up to eight clients are accepted; requests are limited to 1 MiB and retained events to 256. Use `events.since`, and refresh with `asset.state` if it reports a gap.

A catalog has one owner. Starting the headless command against a catalog open in the GUI fails instead of creating competing state. Request IDs deduplicate retries; reusing one with different input fails.
