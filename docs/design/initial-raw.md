# Initial RAW support: Nikon Z6 and Fujifilm X100VI

Status: implementation proposal, not delivered behavior or authorization to implement. The camera targets and continuous non-destructive RAW editing model are accepted. Decoder, rendering, control and delivery choices below remain proposals. The [task plan](../../tasks/implementation-initial-raw.json) describes the work. Ready tasks mean runnable preparation once implementation is requested. No real RAW files have been decoded or benchmarked for this plan.

## Outcome and delivery boundaries

Open the owner's original Nikon Z6 NEF and Fujifilm X100VI RAF files directly, develop their sensor data into a useful neutral rendition, edit through the same history and command service as JPEG, and preserve every original byte. A camera's embedded JPEG is not the developed result. Support is qualified by actual recording mode and evidence, not by extension or a decoder's camera list.

The owner clarified the intended workflow: continually edit RAW in the editor, as in Lightroom. Every adjustment remains editable recipe data evaluated from the RAW original. There is no WB/exposure conversion step that creates a JPEG for subsequent editing. High-precision caches and display previews are disposable evaluations, never replacement sources. Changing an earlier setting recomputes the applicable downstream recipe, including later tools and geometry, without cumulative quantization or resampling.

Recommended delivery has two independently demonstrated slices and one output integration gate:

| Boundary | Observable result | Completion condition |
| --- | --- | --- |
| A — continuous RAW foundation | Verified RAW import, high-precision as-shot development through existing composition tools, Fit/100%, history/versions/reopen, UI/JSON parity | Both cameras in the agreed minimum modes pass rendered, numerical, recovery and resource checks; display conversion is the terminal boundary |
| B — minimum RAW development | Exposure and RAW white balance, reset and neutral picker, persisted as a source-stage tool with generated controls | Adjustments use retained sensor/high-precision data before display clipping; history, sampling and concurrent clients agree |
| Output integration | The existing planned JPEG export can consume a RAW recipe and produces the same composition | Requires the separately owned snapshot-bound JPEG export capability; no second exporter |

Recommend A followed by B as the initial RAW milestone. A is an intermediate integration checkpoint; the intended outcome includes ongoing RAW adjustments in B. Do not call A complete minimum RAW editing, or claim RAW export before the output gate passes. The owner has selected JPEG Basic Slice A then B as the next implementation priority; this RAW planning does not change that decision. RAW's placement among subsequent export, Locate, MCP and library work remains to be selected. The plan does not authorize those separate features.

Proposed exclusions: camera-JPEG/Picture Control/film-simulation matching; custom camera-profile creation; automatic lens corrections; advanced highlight reconstruction; denoise/sharpening controls; HDR, panorama, pixel-shift or burst merging; RAW video; DNG conversion/writing; TIFF/HEIF input or export; batch/library work; sidecar writing; OS-dependent RAW decoding; external module loading. Existing demosaicer artifact handling is part of quality acceptance, not a new Detail tool. If a deferred correction makes either camera unusable, return that concrete tradeoff to the owner rather than declaring the default acceptable.

## Camera and recording-mode contract

The original **Z 6**, Z 6 II and Z 6 III are distinct models. Only the first is a target. The Fujifilm marketing spelling “X100 VI” identifies **X100VI**. Validate make/model and the container's actual encoding; an arbitrary TIFF renamed `.NEF` or JPEG renamed `.RAF` is not RAW support.

Nikon documents 12/14-bit NEF with lossless, lossy compressed and uncompressed choices. Its image-size menu also lists reduced sizes and image areas; those need their own fixtures rather than assumptions about full-size Bayer data. See [NEF recording](https://onlinemanual.nikonimglib.com/z7_z6/en/09_menu_guide_03_07.html) and [image sizes](https://onlinemanual.nikonimglib.com/z7_z6/en/09_menu_guide_03_06.html).

Fujifilm documents uncompressed, lossless-compressed and lossy-compressed RAF, and a 40.2 MP X-Trans sensor with 7728 × 5152 RAW output. Its dynamic-range settings and shutter/burst options belong in the capture manifest; do not infer bit depth solely from the model. See [recording settings](https://app.fujifilm-dsc.com/en-int/manual/x100vi/menu_shooting/image_quality_setting/) and [specifications](https://app.fujifilm-dsc.com/en-int/manual/x100vi/technical_notes/spec/).

| Target | Proposed minimum qualification | Survey and explicitly classify |
| --- | --- | --- |
| Nikon Z6 NEF | FX, full-size, 14-bit lossless compressed; 12-bit lossless compressed | Full-size 12/14-bit uncompressed and lossy compressed; medium/small NEF; DX, square and widescreen image areas; shutter/burst changes |
| Fujifilm X100VI RAF | Full-size uncompressed and lossless compressed, actual encoded precision recorded | Lossy compressed; electronic shutter/burst and crop modes; DR100/200/400 and DR Auto, D Range Priority; extended ISO; digital teleconverter/aspect metadata |
| Both | Normal and rotated captures; daylight and tungsten WB; low/high ISO; intact and absent embedded preview | Firmware, camera JPEG/RAW pairing, active-area margins, black/white levels, optional metadata absence and malformed metadata |

The minimum is a proposal until the owner's actual modes are known. Every agreed required mode needs at least one authentic decodable file; unsupported or unavailable samples cannot count as passing coverage. Additional modes ship only after passing the same checks. Prefer broadening to modes the owner uses over collecting every possible cross-product. Use pairwise combinations for secondary settings and targeted combinations where precision/compression changes.

Keep a machine-readable coverage manifest with four distinct outcomes: verified supported, tested unsupported with reason, untested, and outside scope. A supported model with an unqualified mode receives an explicit mode error, not silent fallback. Do not advertise all cameras accepted by the selected dependency.

## Current implementation and integration points

The delivered baseline is M4 plus the Develop workspace, content-space edits and the orientation layer. The Basic design has decided product choices but is not implemented; implementation must inspect whichever shared capabilities are delivered when RAW work begins.

| Current code | What RAW changes |
| --- | --- |
| `crates/lightwell-core/src/lib.rs`: `open_source`, `SourceImage`, `read_bounded` | JPEG-only decoding and `Arc<[u8]>` RGBA need an explicit source-kind/precision boundary; keep the current JPEG path exact |
| `editor.rs`: `import`, `verified_source`, `CachedSource` | The single `RefCell` source cache and owner-thread cache miss must become a bounded shared preparation service without weakening fingerprint verification |
| `api/owner.rs`: `owner_loop`; `api/methods.rs` | Owner dispatch is serial. Import/preparation must return job state promptly and complete through an owner transaction, not hold the owner during RAW work |
| `render.rs`, `modules/processing.rs` | Exact geometry, point replacement and linear-light bilinear resampling currently consume byte sRGB. Add high-precision RAW evaluation through the complete stack and a terminal display/output boundary, while preserving JPEG byte behavior |
| `modules/descriptor.rs`, `modules/registry.rs` | Number controls and update-in-place action plans already exist. Add source-stage ordering and asset applicability through these shared contracts |
| `preview.rs`, app `app/`, `state/`, `view/` | Existing generation and replacement semantics need source-readiness and processing identities; keep business state out of widgets |
| `xtask/src/editor_acceptance.rs`, `editor_performance.rs`, `smoke.rs`, `fixtures.rs` | Extend existing evidence tooling for authentic RAW modes and stage measurements, without a separate viewer |

The workspace forbids unsafe Rust. A native decoder cannot be bolted into core with unchecked FFI. Evaluate a safe maintained wrapper; otherwise isolate the smallest private native-adapter crate with an explicit local unsafe policy, safe ownership API, documented invariants and tests. Keep the rest of the workspace's forbid policy. No public provider ABI or generalized graph is needed.

## Decoder and development selection

Recommend **LibRaw as the first unpacking/metadata candidate**, with a pinned source build behind a narrow adapter. LibRaw's 0.22 camera list names both models; its download page currently offers 0.22.2. Camera-list inclusion depends on build features and does not qualify modes. The implementation task must freeze an exact revision/archive digest and configuration after experiments. [Camera list](https://www.libraw.org/supported-cameras), [downloads](https://www.libraw.org/download)

| Candidate | Why investigate | Selection risk |
| --- | --- | --- |
| LibRaw | Established decoding and processing metadata; first candidate for both targets | Native build and FFI; basic conversion quality; memory/cancellation coverage; exact compile features |
| Rawler / DNGLab | Rust integration and a useful independent unpacking comparator; upstream records X100VI support | Verify exact modes, metadata and X-Trans development separately; API is explicitly unstable |
| RawSpeed | Established alternative for unpacking and an independent result/performance reference | Not a complete developer: it does not demosaic or do color/WB correction, so assembly cost matters |

Sources: [Rawler project](https://github.com/dnglab/dnglab), [X100VI release history](https://github.com/dnglab/dnglab/releases), [RawSpeed responsibilities](https://github.com/darktable-org/rawspeed). Use official repository sources, pinned at experiment time. A Rust implementation is not automatically faster or safer end-to-end, and a fast unpacker is not a finished RAW renderer.

LibRaw explicitly positions its built-in conversion as basic and excludes production-quality rendering from its focus. Treat `dcraw_process()` as a comparison/prototype, not the automatic product choice. Benchmark complete pipelines as well as unpacking. If its quality or retained precision fails the contract, evaluate a narrowly extracted, pinned established Bayer/X-Trans implementation from [darktable](../research/darktable/raw-and-denoise.md) or [RawTherapee](https://github.com/RawTherapee/RawTherapee), accounting for dependencies and maintenance. Do not start a custom NEF/RAF decoder or custom demosaicer merely to avoid this gate. [LibRaw scope](https://www.libraw.org/about)

Prototype LibRaw and at least one viable independent unpacker on the same files; inspect the remaining candidate and record why it was or was not built. Compare decoded samples/geometry, metadata, development quality, full-resolution latency, peak memory, cancellation latency, build size and clean macOS/Windows/Linux build feasibility. Choose one production path. Do not ship multiple automatic fallbacks that change appearance depending on the host.

The gate also decides linkage, wrapper, compiler/runtime requirements, optional codecs and threading. Prefer no OpenMP oversubscription alongside Rayon; measure bounded native threading if required. Record upstream notices and exact native components in the existing inventory/dependency policy. Manual license/native/asset audits remain deferred; a dependency experiment is not an audit completion.

## Source, recipe and history contract

Split source identity from a disposable rendition:

- **Source identity:** asset ID, locator, exact fingerprint, file signature, source kind and decoded recording mode. Read/hash/decode the same bytes from a stable read-only handle or bounded snapshot, then recheck change evidence before publishing. Cache hits verify signatures without re-reading or hashing all bytes.
- **Immutable interpretation:** sensor/active/default image areas, CFA and phase, levels, as-shot gains, camera color calibration, orientation, metadata provenance and the current interpretation format. Unknown or non-finite mandatory values fail explicitly. Optional metadata may be absent with availability recorded.
- **Prepared data:** immutable unpacked samples and/or the chosen reusable high-precision development intermediate, with dimensions, channel layout, units, primaries/white point/transfer function and a byte budget. Never treat a native mutable decoder object as shared immutable pixels.
- **Recipe development:** propose exactly one required RAW-development effect at the head of every RAW recipe, included in its Original snapshot with explicit defaults. It precedes ordinary content edits and the geometry tail. The source decoder is a service; the tool module owns editable RAW settings. For JPEG there is no RAW layer.

“Original” for RAW means the pinned neutral/as-shot development, not the camera JPEG or uninterpreted mosaic. Later edits update the same development layer ID in a new immutable snapshot. Reset restores that asset's recorded defaults, preserving other layers. A setting equal to the current value is a parameter no-op without rendering. Develop changes must not move content coordinates or change crop dimensions. Invalid/multiple/missing required development layers are rejected rather than repaired silently.

Store enough interpretation/algorithm identity to reject unsupported data and invalidate caches when decoder, calibration, algorithm or settings change. Only current formats are supported. A breaking catalog/effect change bumps the internal marker and refuses incompatible catalogs unchanged; no migration or old evaluator. A missing/disabled RAW provider keeps asset and recipe/history readable but makes affected evaluation/export explicitly unavailable, including the Original entry. No invisible replacement with a JPEG.

Persist small interpretation/settings records, not decoded frames per history entry. Preserve request IDs, expected revisions, actors, atomic commits, versions, branches, undo/redo, Restore and read-only historical preview. Source loss/change never deletes or rewrites any of them.

## Pixel and color contract

The proposed processing order is:

```text
verified sensor samples + metadata
  → active-area and black/white normalization
  → as-shot/custom sensor white balance and chosen Bayer/X-Trans demosaic
  → camera calibration into a declared linear RGB working domain
  → scene exposure and ordered content-space editing operations
  → high-precision geometry composition / resampling
  → fixed SDR rendition / explicit gamut and output boundary
  → sRGB presentation or explicit shared JPEG export
```

WB placement relative to demosaic is an algorithm decision to freeze and test; retain the chosen upstream method's requirements. Sensor margins change CFA phase: crop/rotate cannot be applied to a mosaic as if it were RGB. Establish the output active rectangle and apply orientation exactly once after demosaic or through a tested coordinate mapping. Sensor, active-area, upright-content, edited-image and viewport coordinates are distinct. Decoder dimensions are authoritative; nominal camera dimensions are expectations to compare, not hardcoded allocations.

Recommend float32 linear RGB with explicit primaries and white point for retained development values (evaluate linear Rec.2020/D65 first), using bounded storage/tiles where necessary and no stored opaque alpha channel. Freeze camera-matrix direction, normalization, chromatic adaptation, exposure reference, output transform and clipping/rounding equations before production implementation. “Linear” alone does not define color. A wider working gamut does not establish monitor calibration or broad ICC input support.

Keep finite negative/out-of-display-range values until the named SDR boundary. Exposure multiplies scene-linear values by `2^EV` and does not operate on an embedded JPEG or quantized rendition. Preserve sensor clipping information separately from output clipping. No promise to recover fully saturated measurements. Explicitly fix auto-brightness, highlight handling, denoise/sharpening defaults and any hidden maximum scaling so an unrelated bright pixel cannot secretly change the image's exposure.

Both A and B require high-precision RAW evaluation through the complete editing stack, with display conversion at the end. The existing byte renderer is not an intermediate RAW-editing stage. Converting clipped 16-bit RGB into float does not restore headroom. LibRaw's exposed output depth is 8/16 bits and its documented exposure helper covers only −2 to +3 EV; do not wire it to a wider UI range or call it unbounded scene-linear processing. [LibRaw processing parameters](https://www.libraw.org/docs/API-datastruct.html)

The numerical experiment must demonstrate highlight/shadow latitude before approving the production backend. If the proposed path fails, choose an established higher-precision development path and present the resulting cost/scope tradeoff. The accepted continuous RAW workflow cannot be replaced by JPEG-like correction or an intermediate export.

The current JPEG byte path, point proof and crop behavior remain exact under their existing contracts. Share compiled geometry mappings, but evaluate RAW crop interpolation in its declared linear high-precision domain, not by round-tripping through sRGB bytes. Convert the developer pixel-proof tool's sRGB color into the working domain at its saved recipe position; explicitly test its resulting display semantics. RAW exact transforms preserve working samples exactly. If Basic has already introduced high-precision operations/resampling, reuse them and reconcile stage order in the design gate. Verify both source paths against independent stepwise references; do not reorder nonlinear operations across a resample for convenience.

No intermediate history entry owns a flattened image. A chain such as RAW WB → Exposure → pixel/content tool → rotate → crop must be re-evaluable after any earlier adjustment, undo, Restore or catalog reopen. Point sampling and any histogram/export use the same effective recipe and output transform. A future tool joins the high-precision pipeline at a declared stage rather than treating an earlier preview as its input asset.

### Camera-specific quality

- **Nikon:** verify Bayer pattern/phase, active margins, level normalization across ISO and 12/14-bit modes, saturated-channel behavior, and PDAF/stripe artifacts on pushed shadows. Reduced NEF may not be a Bayer mosaic; classify before applying the normal path. Active D-Lighting/Picture Controls do not imply automatic look reproduction.
- **Fujifilm:** use an actual X-Trans path, including correct 6×6 phase at active-area boundaries. Inspect foliage, fabric, fine diagonals, high-contrast color edges, false color and worm-like detail at 100%. Test DR and shutter variations with metadata and exposure references; apply any necessary capture normalization exactly once. Do not reproduce a film simulation or JPEG contrast curve implicitly. Treat teleconverter/aspect/default-crop metadata as an explicit framing choice.
- **Both:** consistent neutral rendering under daylight/tungsten, stable blacks, plausible saturated colors, no uninitialized borders, doubled orientation or unexplained dimensions. Freeze the default camera framing: recommend the valid photographic active area while preserving explicit sensor-to-content geometry; owner decides whether extra camera crop metadata should be honored.

Calibration provenance and licensing belong with the selected matrices/profiles. Camera JPEGs and independent converters are visual comparators with documented settings, not exact pixel oracles. A film-simulated monochrome embedded preview may legitimately switch to a color neutral development; the UI must make that transition understandable if fast embedded previews are enabled.

## Minimal controls and shared Basic integration

For B propose Exposure (−5 to +5 EV, 0.01 step), WB As shot/Custom, custom Temperature (2000–12000 K) and Tint (−100–100, Lightwell units), a neutral picker, and reset. These ranges and mappings require numerical proof and owner selection; they are not Adobe equivalents. As-shot gains are authoritative. If their inverse temperature is ambiguous, show As shot rather than fabricate a Kelvin value. Store resolved normalized gains plus any required calibration identity and user control values so reopening reproduces the result.

The picker evaluates a bounded neighborhood in the defined pre-WB source stage, maps from edited coordinates through crop/orientation, rejects near-black/clipped/unusable samples, and stores resulting parameters. Sampling a cache miss can request preparation, but neither a point query nor a no-op check may synchronously demosaic a whole frame on the owner. Once ready, local sampling uses the same stage math as rendering.

Use generated module controls, number parameters and update-in-place transactions. Initially use the existing commit-on-release/Enter/key-up semantics; no live RAW re-development on every pointer move is required. If shared Basic drafts exist by integration, reuse their core lifecycle and conflicts rather than create RAW-specific drafts. Changing settings during a crop draft preserves the draft and follows the current conflict/reapply rules. Historical previews remain read-only.

RAW WB is sensor-based; the proposed JPEG Basic WB is relative correction of already rendered pixels. Present their units and applicability honestly. Exposure math, output transforms, numeric controls, job identity and histogram reduction should be shared where their domains match. A JPEG Basic layer is not silently moved or reinterpreted into a RAW source layer. The integration gate assigns one owner to each control/stage and prevents duplicate Exposure/WB UI. Full Tone/Color and histogram UI are separate scope. If the histogram exists, use its declared rendered-output domain; do not call it a sensor histogram.

## Bounded work, memory and cache

Move read/hash/probe/unpack/develop work off both the desktop and catalog owner, including reopen and sample cache misses. The existing allowance for a JPEG cache-miss decode on the owner is not suitable for this milestone. One preparation service owns single-flight source work; requests for the same source/interpretation reuse it. Short owner completions validate signatures, revisions and client generations before publishing a catalog transaction or selection.

Propose one active source/development job plus one replaceable pending interactive job per catalog owner, with a shared memory reservation budget covering other render/export workers. Separate client requests have explicit waiting/superseded/busy outcomes; do not replace another client's committed export or create a pending queue per slider event. One disconnect does not cancel another client's work. Cancellation is cooperative at bounded stages, stops stale output from presentation, and releases native allocations. LibRaw callbacks do not cover every stage, so measure worst-case cancellation and qualify that gap; add a cancellable helper boundary only if measurements require it. [LibRaw cancellation API](https://www.libraw.org/docs/API-CXX.html)

Use generation-bound identities containing source fingerprint, interpretation/decoder build, development settings/effect format, recipe snapshot, selected entry, output dimensions, color domain and quality. Include draft identity if that shared capability exists. Full-quality and reduced previews have different keys. A late load or render cannot replace a newer selection. A failed replacement preserves the last successful asset and all its edits.

Current hard limits remain the starting contract: encoded input 128 MiB, 64 MP, 16384 px per side, 512 MiB per evaluated frame. Measure actual sensor allocation dimensions and the largest owner files. Do not raise a JPEG limit globally to accommodate a RAW decoder. If modes exceed a bound, propose an explicit RAW-specific limit with a complete allocation ledger before accepting them.

Approximate payload sizes below use nominal delivered dimensions and MiB = 2²⁰ bytes. Native margins, allocator overhead and workspaces are additional:

| Allocation | Z6 6048 × 4024 | X100VI 7728 × 5152 |
| --- | ---: | ---: |
| One-channel u16 mosaic | 46.4 MiB | 75.9 MiB |
| RGB u16 | 139.3 MiB | 227.8 MiB |
| RGB f32 | 278.5 MiB | 455.6 MiB |
| RGBA f32 | 371.4 MiB | 607.5 MiB |
| RGBA u8 rendition | 92.8 MiB | 151.9 MiB |

A full X100VI float RGBA frame exceeds today's per-frame bound. Even RGB float plus mosaic, decoder scratch, old/new previews, crop intermediates and GPU copies may exceed a process budget. The design gate needs phase-by-phase liveness and byte reservations, including replacement when an old image remains visible. Release native scratch before later passes, share immutable allocations, cap retained source/development caches by bytes and account for in-flight references during eviction. Prefer one reusable high-precision intermediate; avoid holding mosaic, several float frames and several upload copies together without measured need. Tiles require algorithm-specific halos and seam tests; decoder internals are not magically tiled by a tiled host output.

The backend's own raw-buffer limit is only one bound. Also cap metadata/IFD traversal, file-offset arithmetic, thumbnails, row strides, native scratch, decoder instances, output frames, job/result tables and protocol payloads. Preflight checked dimensions and reservations before allocation; report structured resource errors. Source-sized buffers never enter SQLite or JSON. Keep protocol stdout clean.

### Measurement and provisional budgets

Measure on the owner's M4 with release builds and recorded OS, RAM, GPU/backend, display scale, storage, commit, native build flags and cache state. At least 30 samples for distributions; report failures and all tails. Distinguish process cold, decoder cold, source-cache hit and filesystem warm/cold. Do not claim a cold OS cache if it was not controlled.

| Metric | Proposed investigation target, not an accepted promise |
| --- | --- |
| Loading acknowledgement / unrelated owner request | p95 ≤ 100 ms while RAW work runs |
| Full neutral development ready, local SSD warm filesystem | p95 ≤ 3 s Z6, ≤ 5 s X100VI |
| Warm exposure update when intermediate can be reused | p95 ≤ 250 ms to rendered preview; report WB separately if it re-demosaics |
| RAW edit working set | Investigate ≤ 1.5 GiB combined process CPU RSS including helpers; GPU allocations reported separately |
| Idle and existing JPEG interactions | Preserve existing measured behavior and investigate the existing <1% CPU target |

Record read/hash, identify, unpack, normalize, demosaic, color/output, geometry, resize, upload and observed-frame stages; decoder-only speed is not responsiveness. Measure repeated NEF/RAF/JPEG replacements, 100%, edits, undo/redo, history preview and cancellation, plus the existing generated 24/60 MP JPEG regressions. No CI timing gates. If a target is missed, expose the measured tradeoff and obtain a scope/budget decision rather than silently relaxing it.

## UI, API and error contract

Open picker and programmatic import accept qualified NEF/RAF alongside current JPEG. Extension filters aid selection but byte/metadata validation decides support. Loading/preparation states distinguish queued, reading, decoding/developing, ready, superseded, cancelled and failed. Exact progress percentages are optional; fabricated percentages are not.

Recommend skipping embedded camera previews in A for a simpler truthful transition: keep the last image with a loading state until neutral development is ready. If first-preview measurements justify adding them, require an explicit “Camera preview — developing RAW” state, separate identity, safe orientation/dimensions/profile checks and read-only display. Embedded pixels never satisfy source detail, edits, sampling, histogram, acceptance or export; absent/corrupt thumbnails must not prevent an otherwise valid RAW from opening. No silent preview-only success after decode failure.

The following are required discoverable capabilities, **not command names that exist today**:

| Capability | Semantics |
| --- | --- |
| Input support and source inspection | Qualified camera/mode, source kind, dimensions, sensor layout/precision, interpretation and provider availability; absent metadata identified |
| Import/prepare/readiness/cancel | Bounded asynchronous state with job ID, caller/selection identity, stable structured errors and explicit cancellation scope |
| Inspect/set/reset RAW development | Current or explicit historical state; revision/request-ID mutation; atomic field patch; source applicability and parameter ranges in schemas |
| Neutral sample | Explicit stage and coordinates; bounded sampling or preparation-needed result; same solver as UI |
| Render/sample/history/export integration | Frozen recipe/source identity and declared color/quality; never an embedded-preview fallback |

Implement through the existing command service and descriptor registry; independent JSON clients need no GUI or gesture simulation. Add contracts for unsupported model/mode, corrupt/truncated data, absent mandatory calibration, missing provider, unsupported interpretation, source changed/missing, resource exhaustion, cancellation and stale result. `verified_source` currently collapses decode failures into source-unavailable: retain source-protection semantics while exposing actionable RAW cause information. MCP inherits the registry whenever its separately planned adapter exists.

## Export, recovery and portability

RAW evaluation feeds the same frozen-snapshot JPEG exporter planned in [single-image](../specs/single-image.md#export-follow-up). Keep quality 90, sRGB profile, corrected geometry/orientation, metadata stripped by default, and no overwrites or source aliases. Keep metadata uses the chosen descriptive/capture/GPS whitelist; do not copy a NEF/RAF container, opaque MakerNotes, CFA tags, embedded JPEG or stale thumbnail wholesale into JPEG. Camera profile/matrix data is not the output ICC profile. Export full-resolution development, never the Fit/embedded preview.

That exporter is not implemented in this checkout. RAW export integration can only complete once the named capability is delivered. If it remains absent, report the output gate as outstanding while allowing A/B verification. Locate is also separate: protect missing/changed RAW originals now and add NEF/RAF cases to Locate acceptance when it exists, without implementing a parallel relinker. No source-side sidecars or DNG intermediates are written.

Package the decoder/calibration resources reproducibly with no system LibRaw requirement, runtime downloads, user Python installation or separate processing application. Build/package checks cover macOS arm64, Windows x64 and Linux x64 using the existing baseline. Native Windows/Linux desktop and manual dependency/asset audits remain deferred; headless or VM evidence does not replace them. Automated Mac editor checks use `smoke`, `measure`, `hardening` or `develop --background` with isolated catalogs. Foreground interaction still needs the owner's explicit request.

## Fixtures and acceptance

Keep authentic owner originals and identifiable derived previews under ignored `private/` with a local manifest; do not commit/upload them or download an entire external corpus. Obtain redistribution permission before adding authentic public fixtures. Small synthetic Bayer/X-Trans arrays, levels, matrix cases and malformed inputs can be checked in. Synthetic arrays prove stage math, not NEF/RAF container support. The absence of a permitted real-file CI sample remains a visible coverage gap, not a synthetic substitute.

The manifest records SHA-256, provenance/permission, firmware, compression, actual precision, dimensions/active region/CFA, image area, shutter/burst/DR/ISO/WB, preview presence, expected supported/error result and the reference method. Unknown values stay unknown until inspected. Representative real scenes cover neutral/color patches, skin, daylight/tungsten/mixed light, saturated colors, smooth gradients, foliage/fabric/fine diagonals, low light, high ISO, strong highlights and deep shadows. A small pairwise corpus is preferable to hundreds of unclassified images.

| Proof | Required acceptance |
| --- | --- |
| Unpack and metadata | Exact integer samples/levels/geometry where independent results describe the same domain; account for margins/linearization explicitly. Every required mode is exercised |
| Development math | Separate f64/synthetic references for normalization, exposure, WB/matrices/output; finite values and retained highlight latitude. Freeze tolerances before goldens; initial pointwise proposal is `1e-6 + 1e-6 * abs(reference)` and ≤1 output code where rounding allows |
| Demosaic | Pinned algorithm references and real 100% patches for Bayer/X-Trans; tiled/whole or serial/parallel equivalence under frozen tolerances. Do not require two different algorithms to match byte-for-byte |
| Color/default look | Controlled neutral/patch and real-scene comparison with recorded transforms; freeze chart tolerances and visual rejection cases before declaring success. A camera JPEG is not the neutral reference |
| Geometry | Exact lossless comparisons on a frozen developed raster for orientation/flip/quarter-turn; independent crop reference; content-coordinate pick, all orientation mappings, margins and non-zero-angle crop |
| State and parity | UI and independent JSON agree on recipes, identities, pixels and errors for edit/reset/undo/redo/Restore/version/reopen/history preview, failed replacement and stale concurrent requests |
| Recovery | Hash every source before/after; source mutation during preparation, missing files, same-name wrong files, read-only sources, persistence failure, cancellation, shutdown and provider loss preserve state |
| Bounds | Malformed offsets/dimensions/CFA/levels/metadata/previews; native allocation failure; repeated replacement; job/result/cache caps; no owner frame work, stale presentation or leaks |
| Rendered evidence | Background M4 editor captures plus state, logs, source hashes, entry/revision/generation, processing identity and backend; Fit is complemented by real full-resolution/100% evidence |
| Output | When export exists, independent JPEG inspection of pixels, orientation, dimensions, profile, both metadata choices, destination races/aliases and cancellation |

Update the real support table and current user guide only for outcomes actually demonstrated. No expected output becomes a golden merely because the implementation produced it.

## Implementation sequence and decision gates

1. Establish the owner mode/fixture manifest and baseline integration constraints independently.
2. Benchmark unpackers and development quality; freeze numerical references, memory/cancellation design, Basic ownership and camera framing. Present a concrete decoder/default-rendering recommendation and measured limits for owner selection.
3. Implement the pinned adapter and shared source contracts; build bounded preparation, interpretation persistence and neutral development; integrate rendering and import/UI/API, then qualify A.
4. Implement sensor-based WB/picker and scene exposure with the shared module/control path; qualify B against history, current JPEG and mixed stacks.
5. Complete packaging/host measurements and, when its prerequisite exists, RAW JPEG-export integration; publish exact supported modes and outstanding gates.

The task DAG has separate numerical, scheduling, native build, UI/API, correctness and performance outcomes. Dependencies refer to outputs, not permission to have several agents edit shared files at once. Shared contracts must land before consumers; algorithm/reference preparation and portable build verification can proceed independently where their inputs permit.

## Open decisions

| Decision | Recommendation / evidence needed |
| --- | --- |
| Delivery order and priority | Continuous RAW editing is accepted; JPEG Basic A then B retains its selected next priority. Within RAW recommend foundation A then minimum controls B, with output integration following the shared exporter. Camera-look matching requires a separate design |
| Required modes and firmware | Begin with the minimum matrix above, then prioritize actual owner files; no guessed qualification |
| Backend and demosaic | LibRaw first for unpacking; choose development separately after measured quality/precision results; one pinned production path |
| Default rendering/framing | Neutral as-shot, explicit SDR output and valid active area; no automatic film simulation or lens correction; decide camera crop metadata behavior on examples |
| WB/exposure ranges and working domain | Proposed controls and linear RGB storage above; freeze algorithms and tolerances before production, with an owner decision if capability/range must shrink |
| Performance and preview policy | No embedded preview initially; provisional targets require measurements; byte ledger may justify a bounded RAW-specific budget |
| Basic/export coordination | Reuse delivered shared capabilities and record one control owner; do not assume proposed Basic or exporter already exists |

Unanswered decisions stay proposals. The preparation tasks can assemble the evidence, but downstream production work cannot treat a pending consequential scope/default decision as accepted.

## Performance-rules checklist for implementation

| Required question | Planned answer to verify |
| --- | --- |
| Reads/hashes/decodes | One verified preparation/cache route; no direct request-path `open_source`; consistent source bytes and signature validation |
| Full-frame allocations | Explicit typed source/development/output buffers with a phase ledger, 512 MiB frame limit or approved narrower tiling, and shared byte reservations |
| Point/no-op work | Parameter/geometry checks stay bounded; prepared point sampling uses shared math; cache misses return preparation state |
| Owner-thread work | Only metadata/transactions/session completions; no RAW read/hash/unpack/develop/raster/encode |
| Desktop refreshes | One state/entry merge and preview request per committed change; source readiness never refetches whole history |
| Timers | Reuse existing gated work/event polling; progress only while work exists; no RAW idle timer |
| Photo-sized evidence | M4 real Z6/X100VI stage distributions and RSS plus before/after 24/60 MP JPEG diagnostics |
| Correctness/sharing | Independent integer/float/geometry references, unchanged JPEG goldens and allocation/cache-sharing assertions |
