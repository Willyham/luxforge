# Native modules, parameter metadata and automation

[Knowledge base index](README.md) · Source baseline: 5.6.1.

## Image operations are native modules with a host contract

**S.** The CMake rules build IOPs as native module libraries, and the loader discovers installed modules and loads their callbacks. This is real runtime module loading, not just grouping source files in folders. It does not establish a stable third-party ABI, isolation from crashes, hot-unloading safety or permission sandboxing. [IOP build rules](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/CMakeLists.txt#L39) [Native module discovery](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/module.c#L27) [IOP loader](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/imageop.c#L305)

**S.** The IOP API separates several responsibilities:

| Contract area | Representative responsibility |
| --- | --- |
| Persistent parameters | Describes the user's settings and parameter version |
| Prepared per-pipe state | `commit_params` converts settings into runtime data for that pipe |
| CPU/GPU processing | `process`, optional OpenCL path and tiling-related hooks |
| Geometry | ROI negotiation and forward/backward point transforms |
| Lifecycle | Initialize/clean up global, module, GUI and pipe-specific resources |
| Editing interface | GUI controls and change callbacks |
| Introspection | Parameter metadata and lookup generated from declarations |

Optional callbacks and host fallback behavior matter; a module having one `process` function does not imply it supports all execution modes. [IOP callback contract](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/iop_api.h#L316) [Developer module guide](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/IOP_Module_API.md#L1)

**C.** Prepared state can contain LUTs, coefficients or runtime resources derived from compact user parameters. Keeping those separate helps explain how a small recipe can drive a much larger render without serializing every scratch allocation.

## Introspection is useful, but is not the whole command architecture

**S/D.** Parameter declarations and annotations provide type/range/default/description information used by generated introspection machinery. Parameter versions and legacy conversion callbacks allow the host to interpret retained histories across module changes. These are useful precedents for discoverability and explicit migration. [Introspection guide](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/introspection.md#L1) [IOP callback contract](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/iop/iop_api.h#L316) [IOP loader](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/imageop.c#L305)

**U.** The developer guide makes broad claims about programmatic access. The inspected Lua bindings establish concrete operations and an action dispatcher, but this research does not prove that every parameter, mask point, modal GUI operation and module lifecycle transition is available through a complete, headless, revision-aware API. That would require a coverage inventory and runtime tests.

**P.** Luxforge's requirement is stronger than “has plugins and scripting”: every operation must share the common command layer, expose schemas/state and work through the accepted live-agent interface. A generated parameter table alone does not provide transaction boundaries, cancellation, conflict detection, undo semantics or error recovery.

## Lua and D-Bus: inspect the actual exposed surface

**S.** Lua image bindings include applying sidecars/styles and image/history/duplicate-related operations. The GUI action binding calls `dt_action_process` with action, instance, element, effect and movement information. The binding's GUI context matters; it should not be assumed to work as a universal batch-edit API without an attached interface. [Lua image operations](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/lua/image.c#L96) [Lua GUI actions](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/lua/gui.c#L109)

**S.** D-Bus introspection exposes operations including Open, Quit and, when built with Lua, Lua execution. The implementation handles Lua asynchronously and exposes a few configuration/location properties. This is a concrete remote-control entry point, but its existence does not prove native availability or identical behavior on macOS, Windows and Linux, and it is not itself an MCP schema. [D-Bus interface](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/dbus.c#L30)

**U.** No live process was connected to, no script was executed and no broad automation-parity claim is made. This review does not establish the absence of additional integrations elsewhere in the ecosystem.

## CLI rendering reuses the application machinery

**S.** `darktable-cli` accepts an image or folder, an optional XMP recipe, an output destination and controls for size, quality, style and ICC behavior. Its source sets up an in-memory library and suppresses sidecar writing in the default standalone path; explicit options/configuration can alter behavior. The export path uses the image-processing machinery rather than an unrelated toy renderer. [Command-line rendering](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/cli/main.c#L75) [Export implementation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/imageio/imageio.c#L991)

A **source-derived, unexecuted upstream syntax sketch** is:

```text
darktable-cli INPUT_IMAGE [XMP_FILE] OUTPUT_DESTINATION [OPTIONS]
```

This is not a Luxforge command or a verified command for an installed darktable binary. Before a future benchmark, inspect that binary's help, isolate its configuration/catalog, choose a new output destination and record styles/presets/profiles. The source help explicitly marks `--bpp` unsupported; its appearance in argument help is not proof of working bit-depth selection. [Command-line rendering](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/cli/main.c#L75)

## Importing Lightroom metadata does not run Adobe algorithms

**S.** `src/develop/lightroom.c` translates selected Lightroom XMP fields into darktable module parameters. It carries assumptions about module parameter versions and maps only a subset of Adobe settings. This is a migration implementation to inspect, not an Adobe Camera Raw engine or a proof of identical rendered output. [Lightroom sidecar translation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/lightroom.c#L50)

**C.** Matching field names, reading XMP and reproducing pixels are three different compatibility goals. Similar contrast/exposure names cannot bridge different processing orders, tone curves, color handling and local operators by themselves.

## Implications for Luxforge's module boundary

**P.** Study per-pipe prepared state, explicit ROI/color requirements and host-owned blending/invalidation. Preserve Luxforge's small core, transaction ownership and requirement to retain unknown/missing-provider edits. Measure before splitting basic functions into separately loaded binaries. External loading remains later required work under the existing plan; this research does not implement a plugin runtime or select darktable's C ABI.

Source reuse would require a scoped dependency/asset review and preservation of applicable notices. The project has chosen GPL-3.0-or-later, but that selection is not an audit of darktable's entire dependency/model ecosystem.
