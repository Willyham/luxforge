# Technical options and evidence

Research context behind the stack choices. Rust and Iced are selected; the alternatives are recorded so the reasoning survives. No comparative benchmarks have been run. Companion research: [Lightroom Classic](lightroom/README.md) and [darktable](darktable/README.md).

## Desktop interface

"Native" means three different things: an OS executable, OS-provided widgets and direct GPU rendering. A native executable with custom GPU-rendered controls satisfies the product direction without three widget implementations. [Iced](https://github.com/iced-rs/iced) (Rust, MIT, wgpu) was chosen because it keeps the whole application in Rust and shares the GPU abstraction with the image engine; an isolated probe in `probes/s0` showed both Iced and egui rendering correctly on M4 Metal, with no performance winner. That is an architectural judgment, not evidence that Iced beats Qt, Slint, GPUI or a WebView.

Recorded alternatives: [egui/eframe](https://github.com/emilk/egui) (immediate mode, used in the probe), [Slint](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backends_and_renderers/) (declarative; its royalty-free terms differ from the GPL option), [Qt Quick](https://doc.qt.io/qt-6/qtquick-visualcanvas-scenegraph.html) behind a narrow C++ bridge (the serious fallback if Rust UI cannot deliver the required UX), [GPUI](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md) (credible but pre-1.0 and tied to Zed), [Tauri](https://v2.tauri.app/start/) (WebView; GPU ownership of the photo surface unproven) and [Flutter](https://docs.flutter.dev/platform-integration/desktop) (an extra language and interop boundary). If essential accessibility or display-color behavior cannot be delivered without maintaining a framework fork, reconsider. UI toolkit dependencies stay outside the domain model.

## Rendering

[wgpu](https://wgpu.rs/) provides Metal, Vulkan and DirectX 12 backends for presentation and processing. Backend support does not imply every GPU supports every format or compute feature: probe capabilities and budget resources. The GPU is the renderer of record ([GPU-first](../design/gpu-first.md)); the whole-frame CPU reference renderer is the correctness oracle, with shared coordinate and color definitions and a declared tolerance between the two. Tiling and pyramids become essential as neighborhood operations grow, and selective invalidation, memory accounting and cancellation matter before exotic kernels.

## Linux VM testing

[Try Omarchy](https://github.com/omacom/try-omarchy) documents Apple Silicon support with a QEMU/virgl/ANGLE path that translates guest OpenGL through Metal, and [UTM](https://docs.getutm.app/settings-qemu/devices/display/) documents experimental VirGL acceleration. Either is a plausible ARM64 Linux functional-test route, not evidence that the wgpu feature set or native Linux performance works. Record the guest adapter and whether rendering is software or accelerated; use a native Linux GPU machine for renderer validation. No VM has been installed or tested.

## Image processing and RAW

| Component | Role | Limits |
| --- | --- | --- |
| [image-rs](https://github.com/image-rs/image) | JPEG decode and encode behind an adapter; PNG later | Not color management or RAW development |
| [Little CMS](https://littlecms.com/color-engine/) | ICC transformations and CPU color reference (MIT) | Does not obtain the monitor profile or define the OS surface color contract |
| [LibRaw](https://www.libraw.org/about) | RAW identify, unpacking, sensor metadata and embedded previews | LGPL-2.1/CDDL-1.0; production rendering is explicitly out of its scope |
| [RawSpeed](https://github.com/darktable-org/rawspeed) | Sensor-mosaic unpacking for the recording modes where it is exact and faster ([evaluation](rawspeed-evaluation.md)) | Supplies sensor data only; qualify coverage, integration and license |
| [Rawler/DNGLab](https://github.com/dnglab/dnglab) | Rust RAW decoder to investigate if camera support or integration justifies it | LGPL-2.1; supported extensions do not establish rendering quality |
| [libvips](https://www.libvips.org/) | Possible future thumbnail and batch helper | A second image system needs a measured workload first |

LibRaw's [camera list](https://www.libraw.org/supported-cameras) includes the original Nikon Z6 and the Fujifilm X100VI, qualified by build features; its HE/HE* exclusions name other Nikon models, not the owner's Z6. Actual recording modes still need fixtures, and an embedded JPEG is not proof of RAW development.

A new RAW library is an option if existing options fail. Escalation: measure I/O, unpack, demosaic, color and preview upload independently on real Z6 and X100VI files; compare LibRaw, RawSpeed and Rawler at equivalent output stages; fix scheduling or contribute a targeted patch; consider a fork or a narrow new decoder only for a reproducible gap with a latency target and correctness corpus. Age or language alone is not evidence of poor performance. Full RAW support also needs black and white levels, mosaic interpretation including X-Trans demosaic, camera-to-working-space conversion, white balance, highlight handling and a pleasing default rendition; lens corrections and noise reduction are explicit decisions. JPEG temperature is a creative adjustment of rendered pixels, not RAW white balance; familiar slider names do not imply Adobe-identical algorithms.

## Catalog and storage

[SQLite](https://www.sqlite.org/whentouse.html) is the catalog store with indexed metadata, recipes and revisions; originals and disposable previews live outside it. Use short transactions and one application-level writer. [WAL mode](https://www.sqlite.org/wal.html) needs a local filesystem, so a live catalog inside a network share or cloud-sync folder is not a supported configuration. Use the [backup API](https://www.sqlite.org/backup.html) for consistent snapshots. Million-photo workloads need their own query and I/O measurements.

## Agent and extension interfaces

The command API is the application boundary; MCP is an adapter over it. Operations are JSON-schema described with state queries, bounded previews, structured errors, revision checks and jobs; the UI calls the same service directly. The [MCP tools specification](https://modelcontextprotocol.io/specification/2026-07-28/server/tools) and [stdio transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio) fit a local client-launched process; prefer the [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) after verifying its released protocol support. Never introduce a hosted service to serve a desktop app.

External modules: presets and workflow automation are candidate first use cases, not the limit of the API. Capability-limited WebAssembly through [Wasmtime](https://docs.wasmtime.dev/security.html) is an isolation option if the use case warrants it, but the host still designs the API and resource limits, and it is not the assumed path for every image kernel. Third-party native binaries or shaders need their own trust and failure-isolation design.

## Reuse versus a new application

[darktable](https://www.darktable.org/about/) and [RawTherapee](https://www.rawtherapee.com/) already combine catalogs, non-destructive processing and floating-point pipelines under GPLv3. Forking would reach sophisticated RAW output sooner at the cost of inheriting a large architecture and still needing a coherent command interface. The recommendation is a new small application on established libraries, with an explicit owner decision if early RAW quality warrants reusing a larger engine. Do not copy GPL implementation into a differently licensed core.

## License

Project code is GPL-3.0-or-later with an open-source extension direction; see [dependencies](../engineering/dependencies.md). A process or WebAssembly boundary is not automatically a licensing exception. Native SDK prerequisites are distinct from redistributed components. Review exact native libraries, decoder packs and assets before distribution; that manual review is currently deferred.
