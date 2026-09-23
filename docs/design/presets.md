# Presets

Status: design accepted for implementation on the defaults recorded under [decisions](#decisions-taken-on-defaults); the owner reviews them afterwards. The [task plan](../../tasks/presets.json) tracks the work.

A preset is a named, reusable set of adjustment settings. Applying one changes only the settings it holds, as one history entry, through the same command service every client uses. Lightwell keeps its own presets in the catalog and imports Lightroom Classic presets by converting the settings whose meaning it can carry and reporting every other one.

## Scope

In scope:

- **Settings sets and composite actions.** A settings set names field-patch actions and the fields each one sets. The host applies the steps of a composite action in order and commits the result once.
- **The presets module.** `lightwell.presets` declares `apply-preset`, which applies a settings set as one entry labelled `Preset: <name>`.
- **A preset library in the catalog.** List, read, create, capture from a photo, update, delete, export and import, all through `preset.*` methods.
- **Import.** Lightroom Classic XMP develop presets, legacy `.lrtemplate` presets and Lightwell's own preset document, with a per-setting report. A dry run returns the same report without saving anything.
- **A desktop Presets section.** The grouped library, apply on click, a create form, a file import and a delete command.

Not in scope, with no placeholder controls: an Amount slider, a hover preview, writing Lightroom XMP, DNG presets, Lightroom profiles, RAW Kelvin and tint conversion, a Copy/Paste Settings command, applying to several photos, and reading Lightroom's settings folders automatically. [Later](#later) lists each of these with what it needs.

## Settings sets

A settings set is a JSON object whose keys are field-patch action identities and whose values are non-empty objects of that action's fields:

```json
{
  "set-basic": {"exposure": 0.35, "contrast": 12, "temperature": 0, "tint": 0},
  "set-mixer": {"blue-saturation": -20},
  "set-vignette": {"amount": -18, "midpoint": 50, "roundness": 0, "feather": 50}
}
```

**Presettable actions.** Any action a registered module declares with `patch: true` can be named. Today that is `set-basic`, `set-presence`, `set-mixer` and `set-vignette`, plus `set-controls` in developer mode. A field patch updates one module's single layer, which is exactly a portable setting. Everything else is excluded: RAW source development (`set-raw-*`, `pick-raw-neutral`, `use-as-shot-wb`) is per-capture interpretation with no field patch, transforms and crop are per-photo geometry, and the pixel proof is a test tool. Lightroom Classic excludes crop from develop presets for the same reason ([preset formats](../research/lightroom/presets.md#what-a-preset-can-contain)).

**Bounds.** At most 16 actions and 64 fields per action. Each value is checked against the named action's own parameter descriptors when the set is stored and again when it is applied.

**Apply semantics.** The fields a set names overwrite the current values, and every field it does not name keeps its value. A named field at its neutral value resets that field. This follows Lightroom's rule that a preset changes only the settings it contains. Steps run in the set's key order, which is alphabetical because a JSON object carries no order. Basic, Presence, the mixer and the vignette each update their own module's one layer at a stage and order none of the others shares, so for them the order of the steps cannot change the result. Two effects of one stage and order would land in step order.

## Composite actions

`ActionPlan` gains one variant:

```text
Compose(Vec<ActionInput>)   apply these field-patch actions, in order, as this one action
```

A module returns `Compose` when its action applies other modules' settings. The host runs every step against the stack the steps before it produced:

1. The step's action must be registered, declared with `patch: true` and provided by an available module. Otherwise the result is `validation: unknown action X`, `validation: X is not a field-patch action` or `incompatible: unavailable module M`.
2. The host runs the step's fields through the action's generic check, which for a patch validates only the fields sent, and then through its module's `parse`.
3. The step's module plans against the intermediate stack, using the same `StageContext` construction a single action uses. A step whose plan is itself `Compose` is refused with `validation: composite actions do not nest`.
4. `Commit` inserts at `insertion_index_for`, `Update` replaces in place and `NoOp` changes nothing.

At most 16 steps are allowed. Planning costs O(steps × layers) and rasterizes nothing: every presettable module plans by comparing payloads.

When the final stack equals the starting stack, the action is a no-op with no entry. Otherwise the host validates, compiles and commits the final snapshot once. That snapshot carries one entry, one revision, one request result and one event. The entry stores the composite action's identity, its label and its parameters as the module parsed them, never the individual steps. Undo returns to the stack from before the preset, and restore and redo work as they do for any other entry.

A draft of a composite action resolves the same way. `EditorService::draft_recipe` and a commit share one helper, so a drafted preview is exactly what the commit would produce. No desktop gesture drafts a preset yet.

## The presets module

`lightwell.presets` is a linked built-in registered first, before the pixel module. It declares no effects, so it never owns a layer, and it writes nothing itself.

| Field | Value |
| --- | --- |
| `id`, `title`, `hint` | `lightwell.presets`, `Presets`, `Saved and imported settings` |
| `collapsed` | `true`, so Basic still leads the panel |
| `actions` | `apply-preset`, titled `Apply preset`, `patch: false` |
| `controls` | One `presets {action: "apply-preset"}` control |

`apply-preset` takes three parameters:

| Parameter | Kind | Required | Meaning |
| --- | --- | --- | --- |
| `settings` | `settings` | yes | The settings set to apply |
| `name` | `string {max_length: 128}` | yes | The history label and the provenance of the entry |
| `preset-id` | `string {max_length: 96}` | no | The library preset the settings came from. This is provenance only; the host does not look it up. The name is hyphenated because module parameter names are hyphenated words; host methods keep `preset_id` beside `asset_id` |

`parse` checks that `name` is non-empty after trimming (the generic check has already refused control characters), then stores `{settings, name, preset-id?}` as sent. `plan` returns `Compose` with one step per settings key, in key order. `label` returns `Preset: <name>`. Like every action, `apply-preset` is refused with `incompatible: unavailable module lightwell.presets` when its own module is registered as unavailable; a module with no effects has nothing the commit-time compile could refuse, so the host checks the requested module's availability before planning any action.

The request carries the settings rather than an ID for three reasons. The entry, request deduplication and Copy as JSON request each describe exactly what was applied. A later edit or deletion of the library preset cannot change what an entry means. And the module needs no access to the catalog. A client reads the library preset (`preset.list` or `preset.read`) and sends its `settings`, `name` and `id` as `preset-id`.

### Descriptor additions

- **`string {max_length}` parameter kind.** Implemented as the [UI components](ui-components.md#parameter-kinds-and-hints) design specifies it: UTF-8 text of at most `max_length` characters, with `max_length` at most 256 and no control characters. The `text` control remains the second slice, because nothing here needs one.
- **`settings` parameter kind.** The generic check validates only the shape: an object of 1 to 16 keys, each a valid action identity, each value a non-empty object of at most 64 keys. The host checks each step against its action when the set is applied or stored.
- **`presets {action}` control.** The host renders its preset library here. Choosing a preset submits `action` once with that preset's `settings`, `name` and `preset-id`. Registration requires the action to be declared by the same module, not as a field patch, with a required `settings` parameter of kind `settings` and a required `name` of kind `string`, neither with a default, an optional `preset-id` of kind `string` and nothing else. A module declares at most one such control. A client that cannot render it shows the explicit unsupported-control message.

## Library

Presets live in the catalog, so they share its single owner, atomic writes and backup. The catalog format becomes **5** and adds one table:

```sql
CREATE TABLE presets (
   id TEXT PRIMARY KEY,                -- preset-<uuid>
   name TEXT NOT NULL COLLATE NOCASE,
   group_name TEXT NOT NULL COLLATE NOCASE,
   record_json TEXT NOT NULL,          -- settings, origin, report, actor, timestamps
   source_text TEXT,                   -- the imported file's text, kept verbatim
   UNIQUE(group_name, name)
);
```

A format 4 catalog is refused by name, as every earlier format change has been; choose a new catalog path. The desktop's default catalog lives in the configuration directory, so in practice the library is per installation.

**Record.** `{id, name, group, settings, origin, report, actor, created_ms, updated_ms, unavailable}`:

- `origin` is `{kind: "lightwell"}`, `{kind: "lightroom-xmp", file_name?, uuid?, process_version?, preset_type?}` or `{kind: "lightroom-template", file_name?, uuid?}`.
- `report` is the import report, or `null` for a preset created in Lightwell.
- `unavailable` lists the actions of `settings` that are unknown or unavailable in this registry. It is computed when the record is read and never stored.

The imported file's text is kept in `source_text`, bounded by the request limit. Unsupported settings are therefore never lost: a later importer can map them again. It is returned only by `preset.read`.

**Names.** Name and group are trimmed and non-empty, with no control characters. A name has at most 128 characters and a group at most 64. The default group is `User presets` for a preset created in Lightwell and `Imported` for an import that names no group. A (group, name) pair is unique ignoring case. A duplicate is a `conflict`, and nothing is renamed automatically. The library holds at most 1,000 presets; one more is a `resource-limit` error.

### Methods

Every method is a host method listed by `schema.list`. Create, update and import take `actor`, which the record keeps as its last writer; the four mutating methods emit an event named after the method unless they are a no-op. None takes a `mutation` envelope: the library has no revision, and uniqueness makes a retried create fail visibly rather than duplicate a preset. Names are compared ignoring case across all of Unicode, not only ASCII.

| Method | Mutates | Parameters | Returns |
| --- | --- | --- | --- |
| `preset.list` | no | none | `{presets: [record without source_text, report reduced to counts]}` sorted by group, then name, ignoring case |
| `preset.read` | no | `preset_id` | `{preset: record with the full report and source_text}` |
| `preset.create` | yes | `name`, `settings`, `actor`; optional `group` | `{preset}` |
| `preset.capture` | no | `asset_id`, `fields`; optional `entry_id` (default: the session's selection) | `{settings}` read from that entry's stack |
| `preset.update` | yes | `preset_id`, `actor`; optional `name`, `group`, `settings` | `{preset}`, or `outcome: no-op` when nothing changes |
| `preset.delete` | yes | `preset_id` | `{deleted: true}`, or `outcome: no-op` when the preset is absent |
| `preset.export` | no | `preset_id` | `{file_name, content}`, a Lightwell preset document |
| `preset.inspect` | no | `content`; optional `file_name` | `{preset, report}` as an import would create them; nothing is stored |
| `preset.import` | yes | `content`, `actor`; optional `file_name`, `name`, `group` | `{preset, report}` |

`preset.create`, `preset.update` and `preset.import` validate every action and field of the set against the registry, without a stack. An empty set is refused. Applying a preset is `edit.apply-preset`; the library has no second apply path.

**Capture.** `fields` maps presettable actions to either an array of their parameter names or `true` for all of them. For each action the host finds the layers of its module's effects in the entry's stack. With no layer, each field takes its parameter's declared default. With one layer, each field takes its value from the module's `values` for that layer, and a missing value takes the default. With two or more layers the result is `validation: ambiguous`. Capture reads payloads only: it opens no source, renders nothing and works on JPEG and RAW assets alike. The desktop captures and then creates.

## Import

`preset.inspect` and `preset.import` take the file's text, never a path, so the owner thread does no file I/O. The desktop reads the chosen file on a worker, refuses anything over 1 MiB and sends its text in process. A JSON-lines client is bounded first by the transport's 1 MiB request line, which the escaped text must fit inside. `preset.inspect` parses and maps only: the library's name rules and duplicate check apply at import, after the request's overrides. Parsing a document within the request limit is text work, not frame work, and its measured worst case is recorded in [performance](../specs/performance.md).

**Detection.** The content is read after an optional UTF-8 byte-order mark and leading whitespace:

| Content | Format |
| --- | --- |
| A JSON object with `"format": "lightwell.preset"` | Lightwell preset document |
| XML containing an `x:xmpmeta` or `rdf:RDF` element | Lightroom XMP |
| Text starting `s = {` | Lightroom `.lrtemplate` |
| Anything else | `unsupported-input: not a Lightwell, Lightroom XMP or .lrtemplate preset` |

**Lightwell preset document.** `{"format": "lightwell.preset", "version": 1, "name", "group"?, "settings"}`, with no other keys. Any other version is refused explicitly. `preset.export` writes this document with the file name `<name>.lwpreset`. Its report maps every field one to one.

The [preset file formats](../research/lightroom/presets.md) chapter of the Lightroom knowledge base is the evidence for everything below.

**Lightroom XMP.** The document is parsed with `roxmltree` under the `http://ns.adobe.com/camera-raw-settings/1.0/` namespace, whatever prefix the file uses. `roxmltree` is already in the lockfile through the Iced text stack and is pinned at 0.20.0. Settings come from the attributes of the `rdf:Description` elements that carry Camera Raw fields, and from their child elements:

- `rdf:Alt` takes its `x-default` item.
- `rdf:Seq` takes its items in order.
- A nested `rdf:Description`, such as `crs:Look` or a mask container, is one structured setting whose report value summarizes it.
- Attributes in other namespaces, such as a sidecar's `exif:` and `tiff:` fields, are not settings.

A file with no Camera Raw fields is refused. A file whose `crs:PresetType` is present and is not `Normal` is a profile or a look, and is refused with `unsupported-input: Lightroom profiles are not presets`. A nested `crs:Look` inside a develop preset is only a reference to the profile that was active when it was saved, and is reported as a setting. A photo's XMP sidecar holds the same settings and imports the same way; its crop and other per-photo settings appear in the report as unsupported.

**Legacy `.lrtemplate`.** The file is a Lua table assignment, `s = { … }`. A small bounded parser reads exactly the subset Lightroom writes:

- tables with `key = value` and positional entries, including trailing commas
- double-quoted strings with escapes, and long strings
- numbers, `true` and `false`
- `ZSTR "…"` localized strings, keeping the default text after `=` in a `$$$/Key=Text` string

Nesting deeper than 16 levels or more than 100,000 values is a `resource-limit` error. Anything outside the subset is `unsupported-input`, with the byte offset. The name comes from `title`, then `internalName`, and the settings come from `value.settings`. A flat curve array `{x1, y1, x2, y2, …}` is read as that curve's points. A template whose `type` is not `Develop` is refused.

**Values.** Numbers are accepted with or without a leading `+`. Booleans are accepted as `True` and `False` in any case, as `0` and `1`, and as Lua `true` and `false`.

**Bounds and duplicates.** The XML parser compares every attribute of an element with every other one, so its cost grows with the square of an element's attribute count. A linear scan before parsing therefore refuses the following with `resource-limit`:

- nesting deeper than 64 levels;
- more than 2,000,000 attribute comparisons summed over the elements, which is about 2,000 attributes on one element; Lightroom writes a few hundred;
- more than 128 namespace declarations;
- more than 200,000 nodes.

The measured worst case within these bounds is in [performance](../specs/performance.md#preset-import-parse). A setting written twice, whether in two descriptions or as two keys of a template, is `unsupported-input` rather than one copy silently winning. An XML error names its line and column, and a template error names its byte offset. A Lightwell document of another version is `unsupported-input`.

**Name and group.** The name comes from `crs:Name`, then the template's `title`, then the file name without its extension, then the literal `Imported preset`. The group comes from `crs:Group`, then the request's `group`, then `Imported`. The request's `name` and `group` override both, which is how a client resolves a duplicate.

### Mapping

The importer owns one table of the Lightroom settings it recognizes. Each row says what happens to the setting. A test checks that every mapped target is a registered presettable action and parameter whose range covers the Lightroom range the row claims.

A mapped value is a **value transfer**: the same number on a control with the same name, range and direction. It is not a claim that Lightwell renders what Lightroom renders. The [slider audit](../research/lightroom/slider-parity.md) records why equal values do not mean equal pixels. The report and the user guide say so.

| Lightroom setting | Lightwell target | Rule |
| --- | --- | --- |
| `Exposure2012` | `set-basic.exposure` | Value transfer, −5..+5 EV |
| `Contrast2012`, `Highlights2012`, `Shadows2012`, `Whites2012`, `Blacks2012` | `set-basic.contrast`, `highlights`, `shadows`, `whites`, `blacks` | Value transfer, −100..+100 |
| `Vibrance`, `Saturation` | `set-basic.vibrance`, `saturation` | Value transfer, −100..+100 |
| `IncrementalTemperature`, `IncrementalTint` | `set-basic.temperature`, `tint` | Value transfer, −100..+100: both are relative white balance for rendered images |
| `Texture`, `Clarity2012`, `Dehaze` | `set-presence.texture`, `clarity`, `dehaze` | Value transfer, −100..+100 |
| `HueAdjustment<Range>`, `SaturationAdjustment<Range>`, `LuminanceAdjustment<Range>` for Red, Orange, Yellow, Green, Aqua, Blue, Purple and Magenta | `set-mixer.<range>-hue`, `-saturation`, `-luminance` | Value transfer, −100..+100 |
| `PostCropVignetteAmount`, `PostCropVignetteMidpoint`, `PostCropVignetteRoundness`, `PostCropVignetteFeather` | `set-vignette.amount`, `midpoint`, `roundness`, `feather` | Value transfer |
| `Temperature`, `Tint` | none | **Refused.** These are RAW-only Kelvin and tint values, on a scale that is not Lightwell's RAW tint unit, and the owner kept Lightwell's validated RAW range. Carrying them needs a calibrated conversion |
| `WhiteBalance` | none | Neutral when the preset carries `IncrementalTemperature` or `IncrementalTint` and no `Temperature` or `Tint`, because the incremental values then carry the white balance. Otherwise **refused**: `As Shot`, `Auto` and the named modes set RAW white balance, which no field patch can do |
| `CameraProfile` and a nested `Look` | none | Neutral when they name Lightroom's default profile (`Adobe Standard` or `Adobe Color`), because Lightwell keeps its own neutral rendering. Any other profile is unsupported, because Lightwell has no profiles |
| Earlier process-version fields: `Exposure`, `Contrast`, `Brightness`, `Shadows`, `FillLight`, `HighlightRecovery`, `Clarity`, `ToneCurve`, `ToneCurveName` and the `Auto*` switches of those versions | none | Neutral in a Process 2012 or later preset, because Lightroom does not render them there. In an earlier-process preset they are **refused**, because their meaning and domains differ from the 2012 fields |
| Tone curves, parametric curve, colour grading and split toning, sharpening, noise reduction, grain, lens and chromatic-aberration corrections, lens vignetting, defringe, transform and upright, calibration, black-and-white conversion and mix, Auto Tone, masks and local corrections, spot removal, red eye, crop | none | **Unsupported**: Lightwell has no such tool. Neutral when the setting is at its neutral value, or when it only qualifies an amount that is itself neutral: a sharpening radius when `Sharpness` is 0, a grain size when `GrainAmount` is 0, a hue when its saturation is 0, a mix when `ConvertToGrayscale` is false, a crop rectangle when `HasCrop` is false, and an identity curve |
| `PostCropVignetteStyle`, `PostCropVignetteHighlightContrast` | none | Lightwell draws one vignette style. Neutral when the vignette amount is 0 or the setting is at its default (style 1, contrast 0), otherwise unsupported |
| Panel switches: `Enable*` in templates | none | Never settings themselves. When one is `false`, the settings of that panel are not in effect in the preset. Mapped settings of that panel are **refused** (`disabled in the preset`), and unsupported ones are neutral |
| Preset metadata: `PresetType`, `UUID`, `Cluster`, `Supports*`, `Version`, `ProcessVersion`, `HasSettings`, `RequiresRGBTables`, `CameraModelRestriction`, `Copyright`, `ContactInfo`, `Name`, `ShortName`, `SortName`, `Group`, `Description`, and sidecar bookkeeping such as `RawFileName` and `AlreadyApplied` | none | Recorded in `origin` where useful and never reported as settings |
| Anything else | none | Unsupported: `not recognised` |

A value is parsed as Lightroom writes it (`0.5`, `+0.50`, `-12`, `True`). A value that does not parse, or lies outside the target's hard range, is **refused** with its reason. It is never clamped.

**Process version.** A preset is *modern* when its `ProcessVersion` is 6.7 (Process 2012) or later, or when it has no `ProcessVersion` and does not use the earlier-process tone fields as its tone controls. A preset with an earlier `ProcessVersion`, or with none and only earlier-process tone fields, is *legacy*. In a legacy preset every mapped setting is refused, because Lightwell's controls follow the 2012 names and not what that process version renders.

### Report

```json
{
  "format": "lightroom-xmp",
  "process_version": "15.4",
  "mapped": [{"setting": "Exposure2012", "value": "+0.35", "action": "set-basic", "field": "exposure", "applied": 0.35}],
  "neutral": [{"setting": "GrainAmount", "value": "0"}],
  "unsupported": [{"setting": "Sharpness", "value": "40", "reason": "Lightwell has no sharpening"}],
  "refused": [{"setting": "Temperature", "value": "5500", "reason": "RAW Kelvin white balance has no calibrated conversion"}]
}
```

- `mapped`: the value transfers and is in the preset's settings.
- `neutral`: the setting is unsupported but at its neutral value, so nothing is lost.
- `unsupported`: the effect is not reproduced because Lightwell has no such tool.
- `refused`: Lightwell has a related control, but this value cannot be carried. It is out of range, belongs to another process version or has no calibrated conversion.

A qualifying rule applies only when the preset holds the amount it depends on. A hue whose saturation the preset does not hold is reported on its own value, because the photo's saturation, not the preset's, would decide whether it has an effect.

`value` is the text as written, and a structured value (a curve or a sequence) is its items joined with `; `, up to 256 characters. A preset is **partial** when `unsupported` or `refused` is non-empty. An import that maps nothing is refused with `unsupported-input`, a message giving the four counts and nothing stored; `preset.inspect` returns the full report for such a file.

## Desktop

The Presets section is generated from the `presets` control and is the first section of the tools panel, collapsed until opened:

- **Library.** Group headings in the order `preset.list` returns them, each followed by one row per preset. A partial preset shows a `Partial` badge whose tooltip gives the report's four counts, and a preset with unavailable actions shows why it cannot apply. Rows are disabled while the editor is busy, while a draft is open and during a historical preview. The whole section, Import and the form included, is disabled while no photo is open, like every other section.
- **Apply.** Clicking a row submits `edit.apply-preset` once with that preset's `settings`, `name` and `preset-id`. The ordinary completion path follows: `asset.state`, one preview job and a history merge.
- **Create.** A `+` button opens a form with the name, the group (default `User presets`) and one checkbox per group of presettable controls, labelled `Module · Group` and taken from the descriptors, all checked except white balance. Create calls `preset.capture` for the displayed entry with the checked groups' parameters, then `preset.create`.
- **Import.** An Import button opens the native file dialog, filtered to `.xmp`, `.lrtemplate` and `.lwpreset`. The file is read in the dialog's task, refused over 1 MiB or when it is not UTF-8, and sent to `preset.import`. The status bar reports the result, for example `Imported "Soft film": 18 mapped, 2 unsupported, 1 refused`; the neutral count is in the badge's tooltip. A duplicate name, or a file that maps nothing, appears as the error it is.
- **Row menu.** Right-clicking a row offers Export…, which writes the `preset.export` document through a native save dialog; Copy import report, for an imported preset, which copies the full report as JSON from `preset.read`; and Delete, which calls `preset.delete`.
- **Palette.** The command palette lists `Apply preset: <name>` for each library preset that can apply in this build.

The library is listed at startup, after each of the desktop's own preset calls, after a photo opens and when the event sync sees a `preset.*` event from another client, which reads the library and not the asset. An agent's new preset therefore appears without a restart. Only a click or a palette entry applies a preset, and only that path requests `asset.state` and a preview job.

## Verification

- **Core unit and integration tests.** They cover the kinds' generic checks, registry validation of the `presets` control and composite plans, and one entry per apply with the exact stack and label. They also cover no-op, partial fields keeping untouched values, undo and restore, unknown, non-patch, unavailable and nested steps, duplicate names, bounds and format 4 refusal.
- **Importer tests.** They run on checked-in fixture presets: an XMP with attributes and child elements, a nested default `crs:Look`, a non-`crs` prefix, a profile, an earlier process version, an out-of-range value, a sidecar with crop, a `.lrtemplate` with `ZSTR`, flat curve arrays and a `false` panel switch, and a Lightwell document. Each asserts the exact settings and report.
- **JSON CLI parity.** A test imports, lists, applies, undoes, captures, creates, exports, re-imports and deletes through `lightwell-json`. It asserts that the resulting stacks and pixels equal the same edits made with `edit.set-*`.
- **Rendered check.** A `presets` smoke scenario imports a fixture preset, applies it from the section, captures the frame with the correlated history and recipe, and checks the rendered pixels against the equivalent `edit.set-*` stack.
- **Performance.** No render or source access on any preset path except the apply's ordinary preview. Import parse time for a 1 MiB document is measured on the M4.

## Decisions taken on defaults

The owner asked for this work to proceed without blocking. These are proposals the owner can revise:

1. Presets are catalog data (format 5), not files in a settings folder. Sharing goes through export and import of the `.lwpreset` document.
2. Only field-patch actions are presettable, so RAW source settings, transforms and crop are excluded.
3. `apply-preset` carries the settings, not a library reference.
4. Imports are value transfers for the controls Lightwell has. Lightroom's RAW Kelvin and tint are refused until a calibrated conversion exists. Nothing is clamped.
5. The Presets section is the first tools-panel section, collapsed, in the "what can I do" panel. Lightroom Classic puts presets on the left instead.
6. The create form leaves white balance unchecked by default, because white balance is usually per photo.

## Later

| Item | Needs |
| --- | --- |
| Amount slider | A per-field scaling rule from each module; Lightroom scales only presets that declare `SupportsAmount` |
| Hover preview | A draft of `apply-preset` rendered at proxy size, within the slider latency budget |
| RAW white balance import | A calibrated conversion from Lightroom's Kelvin and tint to Lightwell's RAW controls, and a presettable RAW action |
| Copy/Paste Settings | Capture into a transient set and apply it with `apply-preset`; no core change |
| DNG presets and profiles | Reading an embedded XMP packet from binary content; a profile system |
| Writing Lightroom XMP | An exporter for the mapped fields only, with the same value-transfer caveat |
