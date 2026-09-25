# SDK and interoperability

[Knowledge base index](README.md) · Evidence checked 2026-09-20.

## Public extension surface

**D.** Adobe offers Lua plugins for Lightroom Classic. The official portal describes metadata extensions, custom dialogs/menu items, export/publish integrations and web engines. This establishes an application extension surface, not access to the proprietary pixel engine's source. [S34: Lightroom Classic developer portal](https://developer.adobe.com/lightroom-classic)

The detailed references below are **Adobe-authored SDK documentation on a third-party mirror**. The official download console did not yield its guide through the research tool. Use these as architectural evidence; verify against a pinned official SDK before implementing an integration.

## Develop control and interactive tracking

**D, mirrored SDK.** `LrDevelopController` names controls such as Exposure, Contrast, Texture, Clarity and Dehaze, with `getValue`/`setValue` operations. `setValue` requires the Develop module to be active. Crucially, `startTracking` requests faster lower-quality redraw and suppresses individual history states; `stopTracking` ends tracking and produces one state for the tracked changes. This is direct documentation of an interaction-quality/history-coalescing mechanism. [S41: LrDevelopController reference](https://lrc.mcor.dev/modules/LrDevelopController.html)

**C.** It does not reveal the reduced resolution, omitted stages, shader choices or CPU/GPU scheduling. It also does not mean every possible plugin action is available as a headless operation.

## Photo state and catalog transactions

**D, mirrored SDK.** `LrPhoto:getDevelopSettings()` returns current settings from an asynchronous-task context. Its reference explicitly warns that the develop-settings table is experimental and can change. A readable parameter table is therefore not a stable cross-renderer image-processing specification. [S42: LrPhoto reference](https://lrc.mcor.dev/modules/LrPhoto.html)

**D, mirrored SDK.** `LrCatalog:withWriteAccessDo` supplies controlled write access, uses an action name for Undo/Redo, and rolls database changes back if the callback fails. The documented API also supports timeouts/queued access; success of an individual mutation is not equivalent to a transaction already being committed. [S43: LrCatalog reference](https://lrc.mcor.dev/modules/LrCatalog.html)

**P.** The useful lesson for Luxforge is to route UI and agent mutations through the same transaction/history service, with typed settings and observable completion. Luxforge's accepted goal is broader: every introduced operation must be programmable, including edit modules and lifecycle actions. The SDK is a comparison, not the target contract.

## XMP interoperability has several levels

**D.** XMP transports metadata and processing settings between Adobe applications when written from the catalog. [S02: Metadata and XMP](https://helpx.adobe.com/lightroom-classic/desktop/organize-photos-in-lightroom-classic/metadata-basics-actions.html) **C.** Distinguish:

| Level | What must be preserved |
| --- | --- |
| Organizational metadata | Ratings, keywords and supported descriptive fields |
| Recipe syntax | Parameter names, values, masks and references |
| Processing semantics | Algorithm, process version, profiles, defaults and order |
| Exact rendition | All source/auxiliary data plus equivalent numerical processing |
| Application history | Versions, copies, collections and historical states |

Parsing a numeric Clarity value proves only syntax-level access. A different processor with a slider called Clarity need not have the same output. Equal names/ranges are not a calibration. Sidecar settings are not executable implementations, and recent heavy edits can require an `.acr` companion. [Storage chapter](storage-and-history.md).

**P.** A future Luxforge importer should report exactly which levels it supports. Useful options include importing metadata only, retaining unsupported recipe data, or linking an Adobe-rendered reference. Any conversion of effects needs explicit scope and validation. Luxforge's [preset importer](../../design/presets.md#import) works at the recipe-syntax level: it transfers values for the controls Luxforge has and reports the rest by name, without claiming processing semantics or an exact rendition. No catalog or sidecar migration is built.

## Extension boundaries still unknown

**U.** The researched API surface does not establish support for injecting an arbitrary new non-destructive GPU kernel into Develop. Export post-processing, external raster editing, preset assignment, controller integration and an in-engine processing plugin are different capabilities. Do not infer one from another or claim a complete SDK capability audit.

**U.** The internal application framework, current code split between Lua/native components and ABI/module packaging were not established from these sources. No language-based explanation of Lightroom's latency is justified by this research.
