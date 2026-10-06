# Lightroom import

Status: decided, planned (2026-10-06); nothing is built. The owner decided all eleven questions on 2026-10-06 ([decided](#decided)) and asked for a task plan without authorizing implementation ([task plan](../../tasks/lightroom-import.json)). How close an imported photograph *looks* to Lightroom's rendering is the separate [Lightroom alignment](lightroom-alignment.md) design; this one needs it only to raise fidelity, never to function.

## Why

Most photographers who would try Luxforge have years of work in Lightroom Classic: thousands of edited photographs, picks, collections and virtual copies. Leaving that behind is the largest cost of switching, and every competing editor that courts Lightroom users (Capture One, DxO PhotoLab, ON1, Luminar) ships a migration path for this reason. An importer that brings the library across and turns each photograph's settings into a Luxforge recipe, with an honest account of what did not carry over, removes that cost.

## What already exists

- **The settings mapping.** [Preset import](presets.md#import) already parses Lightroom Classic XMP (including a photo's own sidecar) and legacy `.lrtemplate` files, and owns one [mapping table](presets.md#mapping) from Camera Raw settings to Luxforge actions: Basic, the Tone curve, Presence, the colour mixer, the vignette and RAW white balance, with every other setting reported as neutral, unsupported or refused, and the file's text kept. The `.lrtemplate` reader (`crates/luxforge-core/src/presets/lrtemplate.rs`) reads the Lua table subset Lightroom also uses for the develop settings it stores in its catalog.
- **Places for what is imported.** The [catalog](catalog.md): developed photographs with fingerprints, catalog folders, collections and collection groups, library changes with undo, missing originals and Locate. [Versions](versions-and-lineage.md): named references to retained history entries. Crop and orientation, [lens and perspective](lens-and-perspective.md), [masks](masking.md) (linear, radial, brush, luminance and colour range) and the [RAW looks](raw-looks.md).
- **The research.** Lightroom's [storage and sidecars](../research/lightroom/storage-and-history.md) and the [levels of XMP interoperability](../research/lightroom/sdk-and-interoperability.md#xmp-interoperability-has-several-levels).

## What the importer claims

The research separates five levels of interoperability. The importer states which it supports, in the desktop and in its report:

| Level | Claimed |
| --- | --- |
| Organizational metadata: picks, rejects, collections, folders, virtual copies | Yes, as [decided](#decided) in L1 to L5 |
| Recipe syntax: settings, crop, masks and their values | Yes, setting by setting, with every setting accounted for in the report |
| Processing semantics: the same setting doing the same thing | Only as far as [Lightroom alignment](lightroom-alignment.md) has measured and calibrated it |
| Exact rendition | No. Luxforge's processing is its own |
| Application history: every history step | No; snapshots and virtual copies become versions ([L5, L6](#decided)) |

The promise to a person is: **your library and your edits come across as a faithful starting point, and you are told exactly what did not.**

## Scope

**In scope.** Lightroom Classic catalogs (`.lrcat`) at the catalog versions verified on the owner's installation, with Process Version 2012 or later; and, for people without a catalog (Lightroom cloud exports, Bridge and Camera Raw users), a folder of originals with XMP sidecars, read through the same mapping.

**Out of scope**, with no placeholders: writing to a Lightroom catalog or sidecar, two-way sync, the Lightroom cloud service, Lightroom mobile, publish services, maps, books, slideshows and print, face regions and people, edit history step by step, earlier process versions (refused, as preset import refuses them), and any file kind Luxforge does not open (HEIC, PSD, PNG, TIFF and video are reported, not imported).

## The Lightroom catalog

A Lightroom Classic catalog is a SQLite database. Its schema is undocumented but stable across recent versions and read by several open-source tools. The importer reads only:

| Data | Tables (as observed; verified per supported catalog version) |
| --- | --- |
| Catalog identity and version | `Adobe_variablesGlobal` |
| Files and where they are | `AgLibraryRootFolder` (absolute path), `AgLibraryFolder` (path from root), `AgLibraryFile` (base name, extension) |
| Photographs, picks, ratings, labels, user orientation, virtual copies | `Adobe_images` (`pick`, `rating`, `colorLabels`, `orientation`, `masterImage`, `copyName`) |
| Develop settings | `Adobe_imageDevelopSettings` (`text`: the settings as a Lua table, the same Camera Raw fields an XMP sidecar holds, unprefixed) |
| Snapshots | `Adobe_libraryImageDevelopSnapshot` |
| History steps (counted only) | `Adobe_libraryImageDevelopHistoryStep` |
| Collections, groups, smart collections | `AgLibraryCollection`, `AgLibraryCollectionImage` |
| Keywords | `AgLibraryKeyword`, `AgLibraryKeywordImage` |

Table and column names here are from public descriptions of the format and are to be confirmed against the owner's catalog before any code depends on them; the first task is that confirmation.

**Read-only, always.** The catalog file is opened read-only through SQLite's read-only mode and never written, not even its journal. A catalog whose lock file (`<name>.lrcat.lock`) is present is refused with `conflict: Lightroom Classic has this catalog open; quit it first`, because a database being written underneath a reader can read inconsistently. An unrecognised catalog version is refused by name (`unsupported-input: Lightroom catalog version N is not supported`) and nothing is imported. Originals and sidecars are only read. The catalog file's bytes, its sidecars' and the originals' are hashed before and after in the tests.

**Develop settings source.** The catalog is Lightroom's authority, so its settings are used. When a sidecar beside the original also exists and differs from the catalog, the report says so for that photograph; the sidecar is not used.

## What becomes what

### Photographs

Each Lightroom photograph in scope ([L1](#decided)) becomes a catalog photograph the way a developed pick does ([developing picks](catalog.md#developing-picks)): a bounded read streams its fingerprint and reads its RAW interpretation without developing it, then the asset is created with its Original. The Original is what a new photograph of its kind gets today, the starting look included.

- **Already in the catalog.** A file whose bytes match a photograph already in the catalog is linked to it, as Develop links it, and its Lightroom edit is added as a named version ("Lightroom") instead of replacing the current entry.
- **Missing originals.** A file Lightroom references but that is not found is listed in the report with its last known path and is not imported; the import can be run again once the drive is connected ([re-running](#re-running-and-re-mapping)). A photograph cannot be created without a verified fingerprint.
- **Unsupported kinds.** Listed with their kind and count, not imported.

### The edit

Each photograph's Lightroom settings become **one history entry**, `Imported from Lightroom`, by the `system` actor after the Original, built as a composite of each module's own actions (as [apply-preset](presets.md#composite-actions) is), so every value passes the module's own validation. Undo returns to the Original.

| Lightroom | Luxforge | Notes |
| --- | --- | --- |
| Global settings presets carry | The [preset mapping](presets.md#mapping), unchanged | Basic, Tone curve, Presence, mixer, vignette, RAW white balance |
| `CropTop`, `CropLeft`, `CropBottom`, `CropRight`, `CropAngle`, `HasCrop` | Crop's normalized rotated box | Per-photo, so presets exclude it; the conversion is frozen by a study against Lightroom exports of known crops |
| User orientation (`Adobe_images.orientation`) relative to EXIF | The orientation layer | Quarter-turns and reflections |
| `LensProfileEnable` | Lens correction on, resolved through Lensfun's own profile | Lightroom's profile name is reported; a lens Lensfun lacks is reported |
| `PerspectiveVertical`, `PerspectiveHorizontal` | Perspective, after [alignment](lightroom-alignment.md) measures the two models | Unsupported until then ("different perspective model"); Upright is unsupported |
| `CameraProfile` Adobe Color, Adobe Standard | The Standard look | |
| `CameraProfile` Camera Matching profiles (Camera Standard, Provia, Velvia…) | The Match camera look once [phase 2](raw-looks.md#phase-2-match-camera) exists; Standard until then | The intent, a camera-like rendering, carries; reported as approximate |
| Other profiles, monochrome, colour grading, grain, calibration, B&W mix, defringe, spot removal, red eye | Unsupported, reported | Spot removal maps to Clone and Heal once [Corrections](corrections.md) exists |
| Local corrections | [Masks](#masks) | Phase 2 |

The importer applies the alignment proposal's calibrated value conversions where they exist and the preset table's value transfer where they do not; each row of the report says which.

### Masks

Lightroom's masks (`MaskGroupBasedCorrections`, and in older catalogs `GradientBasedCorrections`, `CircularGradientBasedCorrections`, `PaintBasedCorrections`) are a list of masks, each with geometry and a set of local adjustments. Luxforge's masks are [targets for the delivered modules](masking.md#how-a-mask-reaches-an-effect), so each Lightroom mask becomes one Luxforge mask and its local adjustments become masked layers of the modules that have them:

| Lightroom mask component | Luxforge component | Difference reported |
| --- | --- | --- |
| Linear gradient (zero and full points) | Linear `{x0, y0, x1, y1}`, the same convention | Falloff shape: Luxforge's is `smooth` |
| Radial gradient (bounds, angle, feather, invert) | Radial, with Invert set when Lightroom's selects outside, its default | Falloff shape and feather scale, until aligned |
| Brush (dabs with size, feather, flow, density) | Brush strokes in the stroke store | Density is not delivered; a stroke whose density is not 100 is reported |
| Luminance range, colour range | The range components | Range shapes differ; reported as approximate |
| Subject, Sky, Background, People, Objects, Landscape | Recomputed through [AI editing](ai-editing.md)'s Select when it exists; unsupported until then | Lightroom stores these as rasters in `.lrcat-data` or `.acr` files whose format is not public; they are recomputed on the photograph, not imported, and the report says so |
| Intersect, subtract, invert | The mask's component algebra | |

| Local adjustment | Masked layer |
| --- | --- |
| Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Saturation | Basic |
| Texture, Clarity, Dehaze | Presence |
| Sharpness, Noise | Detail, once aligned |
| Temperature, Tint | Basic's relative pair, once the alignment study measures Lightroom's local white balance |
| Hue, Color tint, Moiré, Defringe, local Curve, Amount slider other than 100 | Unsupported, reported |

A mask some of whose adjustments are unsupported keeps the rest. A mask with no supported component is not created, and its adjustments are reported as not applied: nothing is silently omitted from a render.

### Organization

- **Catalog folders come from events** ([L4](#decided)), as they do when picks are developed ([developing picks](catalog.md#developing-picks)). The import adds the folders of the photographs it brings in to the index first, so their events are computed as Select computes them; each photograph goes into its event's catalog folder, an existing one when the event already has one, and otherwise a new one named as Develop names it ("Konstanz · Sep 2026"). Lightroom's own folder path is kept in the import record and is not a catalog folder.
- **Collections and collection sets** become collections and collection groups of the same names and nesting. Smart collections are listed in the report and not imported ([L10](#decided)).
- **Picks** carry over by being imported ([L1](#decided)); **rejects** are not imported.
- **Ratings and colour labels** become plain collections in a group named "From Lightroom" ("★★★★ and above", "Red label") ([L2](#decided)).
- **Keywords** are kept in the import record and not shown ([L3](#decided)).
- **Virtual copies and snapshots** become named versions on the photograph, each its own entry branched from the Original, named by the copy's or snapshot's name; the master's edit is the current entry ([L5](#decided)).

All of the organization is **one library change** ("Imported 1,240 photographs from Lightroom"), undone and redone by the journal as a Develop is. Each photograph's edit is its own history entry, as a batch preset's is.

## The report

Every photograph has a report, kept with it, in the shape preset import uses (`mapped`, `neutral`, `unsupported`, `refused`, each with the setting, its value and the reason), extended with masks, crop, the look and the alignment calibration used. A photograph with any `unsupported` or `refused` row is **Partial**.

The import's own summary lists counts (imported, partial, linked, missing, unsupported kinds, rejected, smart collections skipped) and the settings most often not carried, across the whole catalog, so a person sees "Colour grading on 312 photographs" and not 312 separate notes.

In Develop, a partial photograph's `Imported from Lightroom` row has a **Partial** tag whose tooltip gives the counts; right-click › Copy import report copies it in full.

## Lightroom's previews

Lightroom keeps a JPEG preview of each photograph in `<catalog> Previews.lrdata`, indexed by `previews.db`. The importer reads the largest preview of each photograph it imports and writes it as that photograph's **grid and large tiers**, fitted within each tier and upright, as a [camera preview](catalog.md#rendered-previews) is written: a row of the imported entry with origin `lightroom`. So a catalog of a thousand imported photographs is browsable in Select and the filmstrip at once, before Luxforge has rendered any of them ([L7](#decided)).

It is a fallback, under the rule every camera preview follows. Luxforge's render of the entry replaces it, as visible and look-ahead requests render photographs, and once a tier has been rendered the Lightroom row is never written again. The grid reports such a photograph `thumbnail`, not `ready`, and the tile's origin says it is Lightroom's preview. It is never shown in Develop or in Compare. A preview older than the photograph's last Lightroom edit, where the preview cache records enough to tell, is not used; the camera preview takes its place. Like every tier it lives in the index, a cache: deleting the index loses it, and the photograph is rendered instead.

The same previews, read in place and never copied, are the validation set of the [alignment](lightroom-alignment.md#validation-on-real-edits) design: Lightroom's rendering of the person's own settings, measured locally with their consent.

## Re-running and re-mapping

- **Import is idempotent.** Each imported photograph records the Lightroom catalog's identity and the photograph's id. Running the same import again imports only what is not already imported (photographs whose originals were missing, or a wider scope) and changes nothing already imported.
- **The source is kept.** Each photograph's Lightroom settings text (bounded, as preset import bounds a file) and its report are stored with it. **Re-map from Lightroom** (`lightroom.remap`) runs the current mapping on the stored text and commits a new `Imported from Lightroom` entry when the result differs, so a photograph imported today benefits from tomorrow's alignment calibration or a newly built tool without the catalog being needed again. It is offered per photograph and in batch over a selection, and never runs automatically.

## API

Every step is a command, as the desktop's are:

| Method | Does |
| --- | --- |
| `lightroom.inspect {path}` | Reads the catalog (or sidecar folder) without importing: version, lock state, counts by scope, unsupported kinds, missing originals, collections, and the mapping summary over a bounded sample. Refusals as above |
| `lightroom.import {path, scope, options, mutation}` | Starts the import as one job on the library lane; `job.read` gives progress and the report so far; `job.cancel` stops between photographs, keeping every photograph finished |
| `lightroom.report {asset_id}` | One photograph's report |
| `lightroom.imports` | The imports into this catalog, with their summaries |
| `lightroom.remap {asset_ids, mutation}` | Re-maps from the stored text, as above |

Paths are read by the worker, never by the catalog owner, as `index.add-folder` and `source.locate` take paths.

## Desktop

In Select: **File › Import from Lightroom…** opens a file chooser for a `.lrcat` or a folder. An inspection sheet shows what was found and what will be imported ([L1](#decided)), with counts and an estimate of the bytes to read; **Import** starts the job on the activity board ("Lightroom: 412 of 1,240"). When it finishes, Select shows the imported photographs and the summary opens, with Copy and a filter to the partial photographs.

## Performance and resources

- **Nothing on the owner or the interface thread.** The catalog is read, settings parsed and photographs fingerprinted on the library lane's worker. The owner only commits each photograph's prepared asset and entry, as Develop's commits are.
- **Fingerprinting dominates.** Every imported original is read once in full: 20,000 photographs of 30 MB is 600 GB, hours from an external drive. This is why the default scope is what was worked on, not everything ([L1](#decided)), why the sheet estimates the bytes, and why the import is cancellable and resumable. Measured on the owner's catalog before acceptance.
- **Bounded memory.** One photograph's settings at a time per worker, bounded as preset import bounds a file; catalog rows are streamed, never loaded whole.
- **Catalog size.** The stored settings text and reports grow the catalog; brush dabs are the largest part. Measured on the owner's catalog before choosing whether to compress them.

## Storage

A new catalog format: an import table (Lightroom catalog identity, path, time, options, summary) and a per-photograph Lightroom origin (import, Lightroom id, copy or snapshot name, settings text, report). Earlier formats are refused as every format change refuses them.

## Legal and licensing

The importer reads files the person owns, for interoperability, with Luxforge's own code; it uses no Adobe SDK, library or code and ships no Adobe profiles. "Lightroom" names the product whose files are read. No legal review has been done and none is claimed.

## Phases

1. **Library and global edits.** `lightroom.inspect` and `lightroom.import` for catalogs and sidecar folders: photographs, picks, catalog folders by event, collections, ratings and labels as collections, virtual copies and snapshots as versions, the global settings through the existing mapping, crop and orientation, lens on, the look, the report, the stored source text and `lightroom.remap`, and Lightroom's previews as the imported photographs' first tiles.
2. **Local corrections and reference.** Linear, radial, brush and range masks with their local adjustments; Camera Matching profiles to Match camera once it exists.
3. **Follow-on tools.** Spot removal to Clone and Heal, AI masks recomputed through Select, Detail and perspective once aligned, each landing with the tool it needs, and reached by existing imports through `lightroom.remap`.

## Verification and acceptance

1. **Originals and Lightroom are untouched.** The catalog file, its previews, sidecars and originals hash the same before and after every import, cancelled and failed imports included.
2. **Refusals.** A locked catalog, an unsupported catalog version, an earlier process version and a non-catalog file are each refused with their reason and nothing written.
3. **Mapping.** Every mapping row has a test from a settings table to the expected actions and report rows; every setting in a test catalog appears in its photograph's report exactly once.
4. **Previews.** A Lightroom preview becomes the imported entry's tiers with origin `lightroom`, is reported `thumbnail`, is replaced by the entry's render and never written after it, and a stale one is not used.
5. **Catalogs.** A generated catalog with the schema subset, built by a test generator, covers folders, collections, picks, rejects, ratings, virtual copies, snapshots, missing originals, unsupported kinds and duplicates; the import is idempotent, cancellation keeps finished photographs and nothing partial, and undo of the library change returns the catalog to before.
6. **The owner's catalog.** A private import of the owner's own Lightroom catalog in a background `cargo xtask develop --background` run with an isolated Luxforge catalog: the summary, the partial rate, the time and bytes read, the catalog's growth, and a rendered check of the imported photographs' Lightroom tiles in Select, their replacement by Luxforge's renders, and the photographs in Develop, with correlated state and logs. Not committed.
7. **No fidelity claim** beyond what the alignment proposal has measured.

## Decided

The owner decided every question on 2026-10-06. Eight are the recommendation; L4 and L7 differ from it.

| # | Question | Decided | Not chosen |
| --- | --- | --- | --- |
| L1 | Which Lightroom photographs enter the catalog | Those worked on: edited, picked, in a plain collection, or a virtual copy. The rest stay on disk, and their folders are added to the index so Select browses them | Everything Lightroom holds, against the catalog's "only what you develop" principle and reading every original in full; chosen folders only |
| L2 | Ratings and colour labels | Plain collections in a "From Lightroom" group, one per rating threshold and label present; the catalog's no-ratings decision stands | Kept in the import record only; reopening that decision |
| L3 | Keywords | Kept in the import record, not shown, until a retrieval decision uses them | Shown and searchable; a collection per keyword; not kept |
| L4 | Folders | Catalog folders by event, as Develop makes them | Mirroring Lightroom's folder tree (the recommendation); one flat folder |
| L5 | Virtual copies and snapshots | Named versions on one photograph | Separate photographs, which would need the catalog to stop linking identical bytes to one photograph |
| L6 | History steps | Not imported; counted in the report | Each step as an entry |
| L7 | Lightroom's previews | The imported photographs' first tiles in Select and the filmstrip, until Luxforge renders them; never in Compare | Extracted and shown in Compare (the recommendation); kept until the first Luxforge edit; not read |
| L8 | Source text and re-mapping | Kept per photograph, with an explicit Re-map | Automatic re-mapping; not kept |
| L9 | Direction | One-way import; nothing written back | Two-way XMP sync |
| L10 | Smart collections | Reported, not imported | Translating the criteria Luxforge's queries can express |
| L11 | Supported Lightroom versions | The catalog versions verified on the owner's installation; others refused by name | A wider set verified on collected catalogs |

## References

- [Presets](presets.md) (import, mapping, report), [catalog](catalog.md), [versions and lineage](versions-and-lineage.md), [masking](masking.md), [lens and perspective](lens-and-perspective.md), [RAW looks](raw-looks.md), [Corrections](corrections.md), [AI editing](ai-editing.md)
- [Lightroom alignment](lightroom-alignment.md)
- Research: [storage and sidecars](../research/lightroom/storage-and-history.md), [SDK and interoperability](../research/lightroom/sdk-and-interoperability.md), [preset formats](../research/lightroom/presets.md), [darktable's Lightroom sidecar translation](../research/darktable/sources.md)
