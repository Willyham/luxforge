# Storage, history and sidecars

[Knowledge base index](README.md) · Source baseline: 5.6.1.

## Originals, database and portable recipes

**D.** darktable documents opening input images read-only, with editing data stored in its database and optional XMP companions. A duplicate is another interpretation of the same original, with its own sidecar/version, rather than another sensor file. Naming preserves the original extension: `photo.NEF.xmp`, then a numbered variant such as `photo_01.NEF.xmp`. After import, the database takes precedence over external XMP changes unless the explicit reload/conflict workflow is used. [Sidecar workflow](https://docs.darktable.org/usermanual/5.6/en/overview/sidecar-files/sidecar/)

**S.** Database initialization creates a library database and a separate shared-data database, with locking around ownership. The library records images, history and associations; shared data includes styles, presets and tags. Treating these as one generic cache would lose durable user state. [Catalog schema](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/database.c#L3330) [Shared data schema](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/database.c#L3138) [Database ownership](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/database.c#L4377)

| Durable object | Representative implementation data | Meaning |
| --- | --- | --- |
| Image | `images.id`, `film_id`, `filename`, `version`, `history_end` | Catalog identity, source reference and chosen edit endpoint |
| History entry | `imgid`, `num`, `operation`, `module`, `op_params`, `enabled` | Numbered module state with module parameter version |
| Instance/blending | `multi_priority`, name, blend parameters/version | Distinguishes repeated modules and their local application |
| Mask history | form ID/type, version, point blob, source blob | Editable mask state associated with history |
| Processing order | `module_order.version`, `iop_list` | Interpretation of module execution order |
| History hashes | basic, automatic, current, mipmap hashes | Tracks state differences and preview relationships |

This is a reading of schema creation code, not a proposal to write SQL into a live catalog. Upgrade code and historical schema branches coexist; an installation's final schema is determined by its migrations. [Catalog schema](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/database.c#L3330)

## History is a sequence of state assignments

**S.** `dt_dev_pop_history_items_ext()` first resets modules to defaults, then walks entries up to `history_end`, assigning stored parameters, enabled state, blending and instance/order information. Later assignments replace earlier settings for the same module. It then resynchronizes processing order and selects the appropriate mask forms. [History reconstruction](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/develop.c#L1614)

**C.** Suppose the history reads `exposure +1`, `crop A`, `exposure +2`. Rendering the final state does not necessarily apply +1 and then +2 for +3 stops. It evaluates the exposure instance at +2, combined with crop A and the rest of the current pipeline. A second *exposure instance* would be a different case: both instances can be active in their assigned positions.

**S.** New edits may append a history item or update the current item's parameter data, depending on module/instance, focus and explicit new-item conditions. The code also has time/target-based undo coalescing. Adding edits after selecting an earlier state removes later history entries with special handling for mandatory/default modules. Persistent history is therefore not an immutable log of every pointer movement or every abandoned branch. [History updates](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/develop.c#L1176)

**D.** The history panel offers navigation and compression of redundant intermediate entries. This is distinct from rearranging the processing modules. [History-stack behavior](https://docs.darktable.org/usermanual/5.6/en/module-reference/utility-modules/darkroom/history-stack/) **P.** Luxforge's accepted [append-Restore/retain-all policy](../../specs/edit-history.md) is stronger and remains its own contract.

## Parameters, versions and masks survive reopening

**S.** A history item persists a module's serialized parameter struct plus its module version, blending data and instance identity. `dt_dev_write_history_ext()` writes entries, `history_end`, the module order list and current history hash; the outer function uses a database transaction. These persisted values are not cached pixels. [History persistence](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/develop.c#L1769)

**S.** XMP serialization carries darktable-specific history, module versions, parameter and blend blobs, instance names/priorities, order and mask history. The sidecar writer compares serialized content and can avoid an unnecessary rewrite. This is considerably richer than a handful of slider names and still requires a processor that understands those versions and blobs. [XMP implementation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/exif.cc#L5128) [Sidecar writer](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/exif.cc#L6062)

**C.** A schema marker, algorithm version and application version answer different questions. The fact that a parameter struct can be decoded does not prove that a changed algorithm will render it identically. A lossless migration of a source image also does not imply a lossless migration of its edit recipe.

## Backup and conflict boundaries

**D.** XMP can reconstruct much image-specific work, but the manual explicitly excludes custom sorting sequence from sidecars. It is not a complete substitute for application database/settings backup. [Sidecar workflow](https://docs.darktable.org/usermanual/5.6/en/overview/sidecar-files/sidecar/)

**P.** A practical recovery test should inventory originals, both databases, XMP, custom profiles/presets, external masks/overlays and any derived AI files separately. Disposable thumbnails and pixelpipe buffers should be recoverable by rendering; a model-generated DNG or an external raster mask may be part of the actual edit input. See [AI](ai-and-derived-images.md).

**C.** Automatic sidecar writing is not a simultaneous multi-writer protocol. Updating a sidecar behind an open application can create a disagreement with its in-memory/database state. This is directly relevant to Luxforge's existing single-owner IPC design: agent edits should use the running owner's command layer, not external SQL or file rewriting.

**U.** This research did not perform a crash-recovery test or prove atomicity across database and filesystem writes as one transaction. Nor does read-only image processing remove explicit user operations that move/delete files. Preserve original hashes in actual correctness fixtures.
