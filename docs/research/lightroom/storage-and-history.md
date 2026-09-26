# Catalog, recipes, history and sidecars

[Knowledge base index](README.md) · Evidence checked 2026-09-20.

## The persistent model

**D.** Import creates a catalog record linking to a photo, with processing settings and organizational metadata. Editing changes that record; rendering interprets its settings. SQLite's own project identifies Lightroom as using SQLite for its application file format. This confirms the database family, not table layouts, journal mode, locking policy or a supported SQL API. [S01: Catalog basics](https://helpx.adobe.com/lightroom-classic/desktop/manage-catalogs-and-files/lightroom-catalog-basics.html) [S40: SQLite users](https://www.sqlite.org/famous.html)

**C.** A useful decomposition is `asset identity + source location + current recipe + historical states + auxiliary assets`. This is a reasoning model, not Adobe's published schema. The current recipe answers “what should this photo look like?” History answers “how did I reach this state?” Rendering does not require repeatedly degrading a JPEG once per recorded slider gesture.

## Files have different responsibilities

| File or resource | Documented role | Consequence |
| --- | --- | --- |
| Original NEF/RAF/JPEG/etc. | Source image; catalog links to it | A catalog backup alone cannot recover missing originals |
| `.lrcat` | Catalog database | Preserve as durable application data |
| `.lrcat-data` | Additional edit data, including AI-related information, introduced in Classic 11 | Treat as durable, not a thumbnail cache |
| `.xmp` | Portable metadata/settings when written | A transport for settings, not a self-contained rendering engine |
| `.acr` sidecar | Heavy edit data with sidecar saving in Classic 15.0+ | Keep accompanying files together |
| `Previews.lrdata` | Browsing previews | Rebuildable when source/rendering dependencies exist |
| `Smart Previews.lrdata` | Editable proxy images | Useful offline; lower-resolution sources, not original backups |

The auxiliary-file distinctions come from the catalog and sidecar documentation; the preview files are detailed in the [performance chapter](previews-and-performance.md). Adobe warns that missing `.lrcat-data` can lose AI edits and require manual recomputation. Its FAQ says a nonempty auxiliary file is included in the zipped catalog backup. [S04: Catalog FAQ](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/workflow-issues/catalog-issues/catalog-faq-lightroom.html) [S03: XMP and ACR sidecars](https://helpx.adobe.com/lightroom-classic/desktop/organize-photos-in-lightroom-classic/create-xmp-acr-files.html)

## Non-destructive pixels versus unchanged bytes

**D.** Develop edits are stored automatically in the catalog. Saving metadata additionally writes sidecars for proprietary RAW formats and can embed metadata in JPEG/TIFF/PSD/DNG containers. Thus “source pixels were not overwritten by this adjustment” is a narrower promise than “the source file hash never changes.” Classic also has an explicit option to write capture-time changes into proprietary RAW files. [S02: Metadata and XMP](https://helpx.adobe.com/lightroom-classic/desktop/organize-photos-in-lightroom-classic/metadata-basics-actions.html) [S11: Create and manage catalogs](https://helpx.adobe.com/lightroom-classic/desktop/manage-catalogs-and-files/create-catalogs.html)

**P.** Luxforge's existing original-preservation requirement should be tested at the byte level. Adobe's metadata behavior is a comparison point, not authorization to modify Luxforge originals.

**D.** Starting in Classic 15.0, sidecar saving places heavy masks/AI edits in `.acr` while keeping `.xmp` smaller. Ordinary edits can remain XMP-only. Lightroom manages companion sidecars when it moves, renames or deletes the image. This is conditional on the sidecar workflow; it does not mean every photo always gets two files. [S03: XMP and ACR sidecars](https://helpx.adobe.com/lightroom-classic/desktop/organize-photos-in-lightroom-classic/create-xmp-acr-files.html)

## State, history, snapshots, copies and presets

| Concept | Meaning |
| --- | --- |
| Current Develop state | The settings currently used to render the selected photo |
| History state | A recorded point that can be selected again |
| Snapshot | A named saved state, useful independently of the current history selection |
| Virtual copy | Another catalog-based interpretation of the same source |
| Preset | Reusable settings that can be applied to images |
| Exported copy | A physical output image |

**D.** Adobe documents chronological history states, selectable prior states and snapshots of complete settings. Clearing the History list leaves current image settings intact. That is direct evidence that history and active recipe are distinct concepts. [S07: Develop module options](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/develop-module-options.html) Virtual copies store alternate settings in the catalog, becoming physical images on export or external-edit copy creation. [S06: Manage photos and virtual copies](https://helpx.adobe.com/lightroom-classic/desktop/manage-catalogs-and-files/photos.html)

**D.** Reset, individual-control reset and Undo are distinct operations. Adobe also documents an Option/Alt history action that clears later steps. Do not infer an immutable event ledger or retention of all branches from the word “history.” [S08: Develop module tools](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/develop-module-tools.html) Luxforge already has a stronger accepted retention contract: [append Restore and retain all historical states](../../specs/edit-history.md).

## Backup and recovery boundaries

**D.** Adobe's backup guide requires photo/output backups separately from catalog backup. Its general “catalog file only” wording should be read alongside the more specific auxiliary-file rules in the FAQ and its own restore instructions. Moving a catalog does not move its originals. [S05: Catalog backups](https://helpx.adobe.com/lightroom-classic/desktop/manage-catalogs-and-files/back-catalog.html) [S04: Catalog FAQ](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/workflow-issues/catalog-issues/catalog-faq-lightroom.html)

**C.** Recovery should distinguish an absent source, absent proxy, stale preview, absent profile and absent computed edit asset. They have different effects: “can show a thumbnail,” “can edit,” and “can reproduce full-quality output” are separate capabilities. Copying XMP alone has not established recovery of virtual copies, collections or the complete history. No exhaustive current sidecar round-trip test was run here.

**U.** The exact serialization of history, AI blobs, file identity and recipe deduplication is unverified. No private catalog was opened or modified during this research.
