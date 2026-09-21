# Feature status

| Capability | Status | Notes |
| --- | --- | --- |
| Repository checks, fixtures, smoke evidence, packaging | Implemented | macOS test launches run in background bundles without desktop activation; smoke and hardening use the editor; [development](engineering/development.md) |
| JPEG open, automatic EXIF orientation, Fit, failed-replacement retention | Implemented (S0) | 8-bit sRGB and greyscale subset; broad ICC conversion and display calibration are not established |
| Windows/Linux automated builds and packages | Hosted baseline verified; refresh open | Native desktop checks deferred |
| License and dependency policy | Enforced | Manual license, native and asset review deferred; two expiring advisory exceptions ([dependencies](engineering/dependencies.md)) |
| Referenced assets in a SQLite catalog | Implemented (M1) | Stable IDs, fingerprints, explicit missing or changed-source errors |
| Copyable status and error messages | Implemented | Copy in the desktop status bar copies the full text; JSON failures expose code and message |
| Ordered non-destructive edit layers | Implemented (M1) | Stable layer IDs, immutable complete recipe snapshots |
| Test pixel-change tool | Implemented (M1) | Integer x/y in the content stage and 8-bit sRGB; placed before the geometry tail so crop changes never move it; exact lossless-buffer proof |
| Persistent history, undo/redo, append-only Restore | Implemented (M1) | Every committed action; all branches retained |
| History list, inspect, select and preview in UI and API | Implemented (M1) | Read-only preview and Return to current |
| Save and reopen layers, history and navigation | Implemented (M1) | Atomic writes in the current catalog format; unsupported formats are refused |
| Fit, numeric zoom, 100% source detail and pan | Implemented (M1) | Physical-pixel 100%; per-client session state |
| Live JSON API and single-owner IPC | Implemented (M1) | Same service and history while the GUI is open |
| Rotate left/right, mirror horizontal, flip vertical | Implemented (M2) | Exact integer mappings composed into one orientation layer updated in place; four rotations leave one neutral layer ([design](design/orientation-layer.md)) |
| Named versions and lineage view | Implemented | A version names a retained entry; lineage query and branch markers; catalog format 3 ([design](design/versions-and-lineage.md)) |
| Cached source and compiled one-pass rendering | Implemented | Point queries never rasterize; [performance rules](engineering/performance-rules.md) |
| Content-space edits and `render.locate` | Implemented | Pixel-stage layers precede quarter-turns, reflections and crop; the canvas pick and the query map a rendered pixel to its content pixel ([design](design/content-space-edits.md)) |
| Declarative tool-module interface | Implemented (M3) | Descriptor-validated registry, generated `edit.<action>` methods and `module.list`; [modules](design/modules-and-api.md) |
| Pixel and transform tools as modules | Implemented (M3) | Registered effects and actions use the current payload shapes |
| Generic module controls and canvas pick in the desktop | Implemented (M3) | Controls rendered from descriptors; pointer pick fills coordinates without committing (pixel proof, `--developer`) |
| Lightroom-style crop and straighten module | Implemented (M4) | Normalized rotated-box payload, exact at 0°, linear-light bilinear resample otherwise, in-place layer updates; `edit.crop`, `edit.crop-fit`, `edit.crop-reset`; [crop contract](specs/single-image.md) |
| Draft conflicts with live agent commits | Implemented (M4) | Explicit Discard or Reapply; a stale draft keeps its composition and re-validates against the current stack |
| Resample stage boundary in the host pipeline | Implemented (M4) | Exact layers before and after compose as one raster pass each; at most two full frames exist at once, each within the 512 MiB frame limit |
| Number parameters and crop-frame canvas in descriptors | Implemented (M4) | `number` parameter kind and the `crop-frame` canvas interaction, generated into desktop controls and the JSON API alongside `point-pick` |
| Develop workspace shell | Implemented | Title bar, collapsible state and tools panels, canvas with the floating mode strip, draft bar and notices, status bar; dark theme; verified on the M4 Mac by the `workspace` and `unavailable` smoke scenarios; [design](design/develop-workspace.md) |
| Widget library and layered desktop | Implemented | `lightwell-ui` (Iced only, no core dependency); `app/`, `state/` and `view/` layers with the boundary enforced by `cargo xtask check-repository`; [architecture](design/develop-workspace.md#architecture) |
| Generated tools panel: sliders, chips, colour fields, groups, resets | Implemented | Every control from `module.list`; slider release commits once and a drag sends nothing; unsupported kinds named; Developer section behind `--developer` |
| History labels, recipe rows, module hints and resets | Implemented | `summary` templates rendered into stored entry labels (catalog format 3), `recipe.describe`, `hint`, module and group `reset`, canvas `title` and `shortcut`, `developer` flag; all in `module.list` |
| Per-client workspace state | Implemented | Panels, canvas mode and thirds through `workspace.set`, reported by `session.state` |
| Compare with the original, command palette, Copy as JSON request | Implemented | Hold `\` or Compare; Cmd+K runs every listed action and host command; a control's context menu copies its exact request |
| JPEG export with color and metadata verification | Editor follow-up | Quality 90, no overwrite, Keep metadata option |
| Manual Locate | Editor follow-up | [source recovery](specs/source-recovery.md) |
| MCP adapter | Editor follow-up | Same operation registry; the JSON API is not MCP |
| Complete editor packaging and portability verification | Editor follow-up | Native Windows/Linux remains deferred |
| External module loading | Required later | Selected use case and measured activation costs first |
| Multi-image library, filters, tagging, collections | Later | Owner workflow decisions first |
| RGB histogram, output clipping indicators/overlays | Proposed plan | [Basic and histogram](design/basic-and-histogram.md); no implementation yet |
| Exposure, white balance, tone, vibrance and saturation | Proposed plan | JPEG-first slices in [Basic and histogram](design/basic-and-histogram.md); product/numerical choices remain open |
| PNG input | Later | Separate from the proposed JPEG Basic controls |
| Nikon Z6 and Fujifilm X100VI RAW | Proposed plan | [Continuous RAW editing](design/initial-raw.md): actual-mode qualification, decoder/development measurements, high-precision recipes and neutral exposure/WB; no RAW implementation or camera qualification yet |
| Texture, clarity, dehaze, masks, clone/heal | Later | Full API required whenever introduced |
| Bitmap layers, blend modes, layer reordering | Not selected | |
| Sidecars, sync, managed-copy import, folder relinking | Later decisions | |
| Marketplace, cloud or accounts, Map/Book/Print/Web modules | Excluded | |

Unverified so far on every milestone: native Windows/Linux desktop behavior, native screen-reader exposure of the custom controls, minimum-OS execution and calibrated display color.
