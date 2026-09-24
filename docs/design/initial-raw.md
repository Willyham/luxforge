# Initial RAW support: Nikon Z6 and Fujifilm X100VI

Status: continuous RAW editing is implemented; supplied-file M4 verification passes, with broader qualification tracked separately. The owner NEF, RAF and DNG pass native development, actual JSON editing/history/reopen and background Metal rendering. The supplied DJI Air 2S DNG uses required GainMap and WarpRectilinear corrections under the [Air 2S contract](air2s-dng.md). The [task plan](../../tasks/implementation-initial-raw.json), [integration contract](raw-integration.md) and [coverage manifest](../../fixtures/raw-coverage.json) distinguish delivered behavior from remaining controlled-scene, resource and platform qualification.

Camera-specific mode and processing policy is defined in the validated embedded
[RAW camera catalog](raw-camera-profiles.md). Camera profiles select existing
capabilities; capture-specific metadata remains authoritative in each original.

## Outcome and delivery boundaries

Open the owner's original Nikon Z6 NEF and Fujifilm X100VI RAF files directly, develop their sensor data into a useful neutral rendition, edit through the same history and command service as JPEG, and preserve every original byte. A camera's embedded JPEG is not the developed result. Support is qualified by actual recording mode and evidence, not by extension or a decoder's camera list.

The owner clarified the intended workflow: continually edit RAW in the editor, as in Lightroom. Every adjustment remains editable recipe data evaluated from the RAW original. There is no WB/exposure conversion step that creates a JPEG for subsequent editing. High-precision caches and display previews are disposable evaluations, never replacement sources. Changing an earlier setting recomputes the applicable downstream recipe, including later tools and geometry, without cumulative quantization or resampling.

The implementation has two editing checkpoints and one separate output integration gate:

| Boundary | Observable result | Completion condition |
| --- | --- | --- |
| A — continuous RAW foundation | Verified RAW import, high-precision as-shot development through existing composition tools, Fit/100%, history/versions/reopen, UI/JSON parity | Both cameras in the agreed minimum modes pass rendered, numerical, recovery and resource checks; display conversion is the terminal boundary |
| B — minimum RAW development | Exposure and RAW white balance, reset and neutral picker, persisted as a source-stage tool with generated controls | Adjustments use retained sensor/high-precision data before display clipping; history, sampling and concurrent clients agree |
| Output integration | The existing planned JPEG export can consume a RAW recipe and produces the same composition | Requires the separately owned snapshot-bound JPEG export capability; no second exporter |

Both A and B are implemented. Their full qualification still includes the outstanding quality and resource checks below. RAW export remains unavailable until the output gate passes. JPEG Basic, export, Locate, MCP and library remain separate scopes; this implementation does not assume they are delivered.

Outside this implementation: camera-JPEG/Picture Control/film-simulation matching; custom camera-profile creation; additional lens-profile corrections beyond the required Air 2S DNG operations; advanced highlight reconstruction; denoise/sharpening controls; HDR, panorama, pixel-shift or burst merging; RAW video; DNG conversion/writing; TIFF/HEIF input or export; batch/library work; sidecar writing; OS-dependent RAW decoding; external module loading. Existing demosaicer artifact handling is part of quality acceptance, not a new Detail tool. If a deferred correction makes either camera unusable, return that concrete tradeoff to the owner rather than declaring the default acceptable.

## Camera and recording-mode contract

The original **Z 6**, Z 6 II and Z 6 III are distinct models. Only the first Nikon model is a target. The Fujifilm marketing spelling “X100 VI” identifies **X100VI**. Validate make/model and the container's actual encoding; an arbitrary TIFF renamed `.NEF` or JPEG renamed `.RAF` is not RAW support.

Nikon documents 12/14-bit NEF with lossless, lossy compressed and uncompressed choices. Its image-size menu also lists reduced sizes and image areas; those need their own fixtures rather than assumptions about full-size Bayer data. See [NEF recording](https://onlinemanual.nikonimglib.com/z7_z6/en/09_menu_guide_03_07.html) and [image sizes](https://onlinemanual.nikonimglib.com/z7_z6/en/09_menu_guide_03_06.html).

Fujifilm documents uncompressed, lossless-compressed and lossy-compressed RAF, and a 40.2 MP X-Trans sensor with 7728 × 5152 RAW output. Its dynamic-range settings and shutter/burst options belong in the capture manifest; do not infer bit depth solely from the model. See [recording settings](https://app.fujifilm-dsc.com/en-int/manual/x100vi/menu_shooting/image_quality_setting/) and [specifications](https://app.fujifilm-dsc.com/en-int/manual/x100vi/technical_notes/spec/).

| Target | Implemented recording modes | Additional qualification |
| --- | --- | --- |
| Nikon Z6 NEF | FX, full-size, 14-bit lossless compressed; 12-bit lossless compressed | Full-size 12/14-bit uncompressed and lossy compressed; medium/small NEF; DX, square and widescreen image areas; shutter/burst changes |
| Fujifilm X100VI RAF | Full-size uncompressed and lossless compressed, actual encoded precision recorded | Lossy compressed; electronic shutter/burst and crop modes; DR100/200/400 and DR Auto, D Range Priority; extended ISO; digital teleconverter/aspect metadata |
| Both | Normal and rotated captures; daylight and tungsten WB; low/high ISO; intact and absent embedded preview | Firmware, camera JPEG/RAW pairing, active-area margins, black/white levels, optional metadata absence and malformed metadata |

The owner supplied 14-bit lossless NEF (firmware 03.40.b0, ISO 100, EXIF8) and 14-bit uncompressed RAF (firmware 1.32, ISO 125, DR100). Four public CC0 samples cover both NEF bit depths and both RAF encodings. Each mode needs actual editor evidence as well as native decode evidence. Additional capture settings are not implicitly qualified by sharing an encoding; the coverage manifest retains those gaps.

The additional owner-supplied DJI Air 2S file identifies its sensor container as **DJI FC3411**, DNG 1.4. Its uncompressed mosaic, calibration, active/default crop and opcode lists are verified against independent references; DNG extension support does not mean support for every DNG, drone or computational mode. DJI documents JPEG/DNG still capture for the [Air 2S](https://www.dji.com/air-2s). The supplied file is the initial DJI target. Its mandatory OpcodeList3 GainMap (9) and WarpRectilinear (1) are applied in order before the camera matrix, with recorded interpolation/calibration and headroom-preserving float semantics. Unknown mandatory operations fail explicitly and the editor preserves the previous photo. Generic DNG ingestion and DNG writing remain outside scope.

Keep a machine-readable coverage manifest with four distinct outcomes: verified supported, tested unsupported with reason, untested, and outside scope. A supported model with an unqualified mode receives an explicit mode error, not silent fallback. Do not advertise all cameras accepted by the selected dependency.

## Current implementation and integration points

The source path is integrated into the existing M4/Develop editor. JPEG Basic, export, Locate and MCP remain separate capabilities.

| Component | Current responsibility |
| --- | --- |
| `lightwell-raw` | Private pinned LibRaw/librtprocess adapter; validated metadata, immutable u16 mosaic, float development and typed failures |
| Core `source.rs` / `editor.rs` | Typed prepared sources, one signature-verified cache, source interpretation persistence and RAW development identity |
| Core `api/owner.rs` | Bounded asynchronous source jobs; worker read/hash/unpack/develop, owner-only catalog transactions and client adoption |
| Core `render/linear.rs` | Retained float32 linear sRGB, exact geometry and linear crop interpolation, bounded sampling and terminal display conversion |
| Core `modules/raw.rs` | Required source-stage RAW layer, exposure/WB/neutral/reset actions, descriptor-generated controls with their field resets, the reported control values including the as-shot equivalent, and neutrality |
| App `app/` / `state/` | Source readiness, current/historical control values, stale-result rejection and displayed frame identity |
| `xtask raw-corpus`, `raw-reference`, `raw-editor` | Source integrity, independent numerical references and correlated real-editor evidence |

The workspace forbids unsafe Rust outside the private `lightwell-raw` native adapter. That crate exposes a safe ownership API and contains the locally documented FFI and callback lifetimes. Core, app and other crates retain the workspace forbid policy. No public provider ABI or generalized graph is needed.

## Decoder and development selection

The production path is **LibRaw 0.22.2 for metadata/unpacking**, followed by pinned **librtprocess RCD** for Bayer and **one-pass Markesteijn** for X-Trans. Sources are vendored and statically built as C++17 with no OpenMP, RawSpeed, DNG SDK, LCMS or optional JPEG/zlib components. No system RAW library or runtime download is needed. Exact pins, source digests and notices are in the [backend selection](../research/raw-backend-selection.md) and [adapter](../../crates/lightwell-raw/README.md).

The independent Rawler 0.8.0 experiment agrees exactly on Fuji and DJI sensor samples. Its Nikon lookup-table dithering changes about three quarters of lossless codes by one; the selected LibRaw path preserves the direct curve values. RawSpeed was inspected as an unpacker, not integrated as a second production fallback. LibRaw's integer converter and Rawler's clipping float helper are not the retained developer. The implementation applies explicit normalization/WB, established float demosaic and camera calibration itself.

These engineering choices provide one deterministic path. They do not establish controlled chart accuracy, camera-JPEG matching or accepted performance budgets. The current native cancellation limitation is explicit: RCD/Markesteijn ignore callback cancellation, so a running call is discarded when it returns rather than interrupted immediately. All manual license/native/asset audits remain deferred.

## Source, recipe and history contract

Split source identity from a disposable rendition:

- **Source identity:** asset ID, locator, exact fingerprint, file signature, source kind and decoded recording mode. Read/hash/decode the same bytes from a stable read-only handle or bounded snapshot, then recheck change evidence before publishing. Cache hits verify signatures without re-reading or hashing all bytes.
- **Immutable interpretation:** sensor/active/default image areas, CFA and phase, levels, as-shot gains, camera color calibration, orientation, metadata provenance and the current interpretation format. Unknown or non-finite mandatory values fail explicitly. Optional metadata may be absent with availability recorded.
- **Prepared data:** immutable unpacked samples and/or the chosen reusable high-precision development intermediate, with dimensions, channel layout, units, primaries/white point/transfer function and a byte budget. Never treat a native mutable decoder object as shared immutable pixels.
- **Recipe development:** exactly one required RAW-development effect at the head of every RAW recipe, included in its Original snapshot with explicit defaults. It precedes ordinary content edits and the geometry tail. The source decoder is a service; the tool module owns editable RAW settings. For JPEG there is no RAW layer.

“Original” for RAW means the pinned neutral/as-shot development, not the camera JPEG or uninterpreted mosaic. Later edits update the same development layer ID in a new immutable snapshot. Reset restores that asset's recorded defaults, preserving other layers. A setting equal to the current value is a parameter no-op without rendering. Develop changes must not move content coordinates or change crop dimensions. Invalid/multiple/missing required development layers are rejected rather than repaired silently.

Store enough interpretation/algorithm identity to reject unsupported data and invalidate caches when decoder, calibration, algorithm or settings change. Only current formats are supported. A breaking catalog/effect change bumps the internal marker and refuses incompatible catalogs unchanged; no migration or old evaluator. A missing/disabled RAW provider keeps asset and recipe/history readable but makes affected evaluation/export explicitly unavailable, including the Original entry. No invisible replacement with a JPEG.

Persist small interpretation/settings records, not decoded frames per history entry. Preserve request IDs, expected revisions, actors, atomic commits, versions, branches, undo/redo, Restore and read-only historical preview. Source loss/change never deletes or rewrites any of them.

## Pixel and color contract

The processing order is:

```text
verified sensor samples + metadata
  → active-area and black/white normalization
  → as-shot/custom sensor white balance and chosen Bayer/X-Trans demosaic
  → required camera-space optical corrections for qualified DNGs
  → camera calibration into a declared linear RGB working domain
  → scene exposure and ordered content-space editing operations
  → high-precision geometry composition / resampling
  → fixed SDR rendition / explicit gamut and output boundary
  → sRGB presentation or explicit shared JPEG export
```

WB is applied after per-site black subtraction and sensor-white normalization, before RCD or Markesteijn demosaicing. The measured pre/post difference in the [backend selection](../research/raw-backend-selection.md) rules out changing sensor WB by multiplying a fixed developed image for any committed result; a drafted value is previewed that way during its gesture only, labelled approximate, and redeveloped on release ([instant previews](instant-preview.md#a-raw-white-balance-during-a-drag)). Sensor margins change CFA phase: crop/rotate cannot be applied to a mosaic as if it were RGB. Establish the output active rectangle and apply orientation exactly once after demosaic or through a tested coordinate mapping. Sensor, active-area, upright-content, edited-image and viewport coordinates are distinct. Decoder dimensions are authoritative; nominal camera dimensions are expectations to compare, not hardcoded allocations.

The selected retained domain is unbounded float32 linear sRGB with a D65 white point, stored as three contiguous planes with no alpha. Signed components represent colors outside the sRGB display gamut; values above one retain highlight headroom. Neither is clipped before the terminal display transform, so using sRGB primaries here does not restrict the retained data to displayable sRGB colors. LibRaw’s `rgb_cam` maps WB-balanced camera RGB into this domain; applying `pre_mul` again would double-normalize the camera response. The matrix direction and normalization are checked against the pinned source and independent numerical references. Final SDR output applies exposure in linear light, the sRGB transfer function and one clamp/round to display bytes. This does not establish monitor calibration or broad ICC input support.

Keep finite negative/out-of-display-range values until the named SDR boundary. Exposure multiplies scene-linear values by `2^EV` and does not operate on an embedded JPEG or quantized rendition. Preserve sensor clipping information separately from output clipping. No promise to recover fully saturated measurements. Explicitly fix auto-brightness, highlight handling, denoise/sharpening defaults and any hidden maximum scaling so an unrelated bright pixel cannot secretly change the image's exposure.

Both A and B require high-precision RAW evaluation through the complete editing stack, with display conversion at the end. The existing byte renderer is not an intermediate RAW-editing stage. Converting clipped 16-bit RGB into float does not restore headroom. LibRaw's exposed output depth is 8/16 bits and its documented exposure helper covers only −2 to +3 EV; do not wire it to a wider UI range or call it unbounded scene-linear processing. [LibRaw processing parameters](https://www.libraw.org/docs/API-datastruct.html)

Independent numerical tests exercise negative values, highlight headroom, exposure and final composition. Real controlled highlight/shadow and color-chart qualification remains open; the current samples alone do not establish broad image quality.

The current JPEG byte path, point proof and crop behavior remain exact under their existing contracts. Share compiled geometry mappings, but evaluate RAW crop interpolation in its declared linear high-precision domain, not by round-tripping through sRGB bytes. Convert the developer pixel-proof tool's sRGB color into the working domain at its saved recipe position; explicitly test its resulting display semantics. RAW exact transforms preserve working samples exactly. If Basic has already introduced high-precision operations/resampling, reuse them and reconcile stage order in the design gate. Verify both source paths against independent stepwise references; do not reorder nonlinear operations across a resample for convenience.

No intermediate history entry owns a flattened image. A chain such as RAW WB → Exposure → pixel/content tool → rotate → crop must be re-evaluable after any earlier adjustment, undo, Restore or catalog reopen. Point sampling and any histogram/export use the same effective recipe and output transform. A future tool joins the high-precision pipeline at a declared stage rather than treating an earlier preview as its input asset.

### Camera-specific quality

- **Nikon:** verify Bayer pattern/phase, active margins, level normalization across ISO and 12/14-bit modes, saturated-channel behavior, and PDAF/stripe artifacts on pushed shadows. Reduced NEF may not be a Bayer mosaic; classify before applying the normal path. Active D-Lighting/Picture Controls do not imply automatic look reproduction.
- **Fujifilm:** use an actual X-Trans path, including correct 6×6 phase at active-area boundaries. Inspect foliage, fabric, fine diagonals, high-contrast color edges, false color and worm-like detail at 100%. Test DR and shutter variations with metadata and exposure references; apply any necessary capture normalization exactly once. Do not reproduce a film simulation or JPEG contrast curve implicitly. Treat teleconverter/aspect/default-crop metadata as an explicit framing choice.
- **Both:** consistent neutral rendering under daylight/tungsten, stable blacks, plausible saturated colors, no uninitialized borders, doubled orientation or unexplained dimensions. Freeze the default camera framing: recommend the valid photographic active area while preserving explicit sensor-to-content geometry; owner decides whether extra camera crop metadata should be honored.

Calibration provenance and licensing belong with the selected matrices/profiles. Camera JPEGs and independent converters are visual comparators with documented settings, not exact pixel oracles. A film-simulated monochrome embedded preview may legitimately switch to a color neutral development; the UI must make that transition understandable if fast embedded previews are enabled.

## Minimal controls and shared Basic integration

The source module supplies Exposure (−5 to +5 EV), As shot restore, Custom temperature (2000–12000 K), Custom tint (−100–100 Lightwell units), a neutral picker (a `picker` control in the RAW group, under Custom tint) and reset. Each of the three sliders is its own single-parameter action, so a drag drafts through the core and commits one entry on release. Exposure previews live. A drafted temperature or tint previews approximately: the prepared development holds the committed white balance, so the drafted gains are approximated on it by one camera-space matrix per pixel, the frame is labelled approximate and never analysed, and the mosaic is redeveloped only for the committed value, whose exact frame replaces the approximation on release ([instant previews](instant-preview.md#a-raw-white-balance-during-a-drag)). Every committed render, export, sample and analysis still refuses a white balance the development does not hold. These deterministic mappings are tested with independent references and the actual camera matrices; Lightwell tint is not an Adobe unit. As-shot gains are authoritative: the payload stores no Kelvin value for As shot. A custom temperature or tint change keeps the white balance in force for the other field, as Lightroom's Temp and Tint do: from As shot the camera's as-shot equivalent (below), also after a return to As shot whatever custom values the payload kept; from a neutral pick or an explicit gain that gain's equivalent; from a custom temperature and tint the stored one; and the declared 6504 K or 0 tint only when the gains in force have no equivalent in range. Resolved normalized gains, camera calibration and custom values are persisted so reopening reproduces the result. Positive tint increases the magenta correction. The implementation blends Planckian and daylight loci over 3800–4500 K and applies a signed CIE 1960 uv offset of 1e−4 per tint unit before resolving camera gains.

While As shot is selected, the temperature and tint controls show the as-shot equivalent: the temperature and tint whose gains are the camera's as-shot gains; custom gains without a stored temperature and tint (a neutral pick, an explicit gain) show their own equivalent the same way. The core computes it (`white_balance::temperature_tint_from_gains`, through `RawPayload::white_balance_controls`, which the module's `values` report on the `recipe.describe` row and which the temperature and tint actions start from), so the desktop reads no RAW payload and what the controls show is what the next change keeps. The inverse runs in `f64`: the reciprocal gains are the camera's response to the white, the inverse camera matrix gives that white's CIE 1960 uv, and since tint moves a white along the locus normal, the temperature is where the white's offset has no component along the locus tangent. That component is scanned at 100 K steps, 10 K through the 3700–4600 K blend and at the polynomial seams (2221–2223, 3999–4001, 6999–7001 K), each sign change is bisected (at most 64 halvings), and the temperature whose white lands closest wins; nothing is clamped into range. Gains no temperature in 2000–12000 K and tint within ±100 reproduce within half a tint unit (5e−5 uv) are refused with `out-of-range:` and the reason, and the controls then show, and a change keeps, 6504 K and 0. Accuracy: over the three camera matrices and three synthetic ones on a grid of the whole range, gains come back to 4.7e−13 relative in `f64` and 4.6e−7 from the `f32` gains a payload stores; where the map is one-to-one the controls come back to 5.8e−8 K and 1.8e−12 tint in `f64`, and 2.9e−3 K and 1.05e−4 tint from stored gains. The forward map is not one-to-one within 1.5 K of the seams and near 4450–4500 K below −85 tint, where the blend bends the locus more tightly (radius about 0.008 uv) than such a tint reaches, so gains there have several temperatures and the inverse returns one of them, whose gains match. The supplied files' as-shot equivalents are 4860.955 K and −49.97 (Z6), 4831.181 K and +13.56 (X100VI) and 5598.324 K and −1.55 (Air 2S), each reproducing its gains to 2e−14. One inversion costs about 13 µs in a release build and reads no pixel.

Resetting the custom temperature or tint field — a double-click on its label or rail — runs `use-as-shot-wb`, which each control declares as its field reset, rather than setting a custom 6504 K; Exposure resets to 0 EV. A development at As shot and 0 EV is neutral (`RawPayload::is_neutral`), whatever custom values a return to As shot left in the payload, so the RAW band shows no edited dot for an untouched RAW or one returned there.

The picker evaluates a bounded neighborhood in the defined pre-WB source stage, maps from edited coordinates through crop/orientation, rejects near-black/clipped/unusable samples, and stores resulting parameters. Sampling a cache miss can request preparation, but neither a point query nor a no-op check may synchronously demosaic a whole frame on the owner. Once ready, local sampling uses the same stage math as rendering.

Use generated module controls, number parameters and update-in-place transactions. Initially use the existing commit-on-release/Enter/key-up semantics; no live RAW re-development on every pointer move is required. If shared Basic drafts exist by integration, reuse their core lifecycle and conflicts rather than create RAW-specific drafts. Changing settings during a crop draft preserves the draft and follows the current conflict/reapply rules. Historical previews remain read-only.

RAW WB is sensor-based; the proposed JPEG Basic WB is relative correction of already rendered pixels. Present their units and applicability honestly. Exposure math, output transforms, numeric controls, job identity and histogram reduction should be shared where their domains match. A JPEG Basic layer is not silently moved or reinterpreted into a RAW source layer. The integration gate assigns one owner to each control/stage and prevents duplicate Exposure/WB UI. Full Tone/Color and histogram UI are separate scope. If the histogram exists, use its declared rendered-output domain; do not call it a sensor histogram.

## Bounded work, memory and cache

Move read/hash/probe/unpack/develop work off both the desktop and catalog owner, including reopen and sample cache misses. The shared source worker also handles JPEG cache misses. One preparation service owns single-flight source work; requests for the same source/interpretation reuse it. Short owner completions validate signatures, revisions and client generations before publishing a catalog transaction or selection.

One source worker admits at most eight pending tasks and retains 64 terminal results; one prepared cache holds the current source. The preview worker has one active and one replaceable pending job. Source single-flight shares identical requests, superseded desktop imports detach their interest, and full queues report a resource error. Frame ownership and the float-development gate are detailed in the integration contract. Separate client work is not silently replaced. One disconnect does not cancel another client's work. Cancellation is cooperative at bounded stages, stops stale output from presentation, and releases native allocations. LibRaw callbacks do not cover every stage, so measure worst-case cancellation and qualify that gap; add a cancellable helper boundary only if measurements require it. [LibRaw cancellation API](https://www.libraw.org/docs/API-CXX.html)

Use generation-bound identities containing source fingerprint, interpretation/decoder build, development settings/effect format, recipe snapshot, selected entry, output dimensions, color domain and quality. Include draft identity if that shared capability exists. Full-quality and reduced previews have different keys. A late load or render cannot replace a newer selection. A failed replacement preserves the last successful asset and all its edits.

RAW admission uses the [approved contract](architecture.md#rendering-and-limits). Terminal RGBA8 remains capped at 512 MiB. JPEG retains its independent 128 MiB encoded, 64 MP and 512 MiB frame limits. The [RAW resource ledger](modern-camera-resource-ledger.md) accounts for overlapping allocations and measured editor memory; per-buffer bounds do not constitute a process memory ceiling.

Approximate payload sizes below use nominal delivered dimensions and MiB = 2²⁰ bytes. Native margins, allocator overhead and workspaces are additional:

| Allocation | Z6 6048 × 4024 | X100VI 7728 × 5152 |
| --- | ---: | ---: |
| One-channel u16 mosaic | 46.4 MiB | 75.9 MiB |
| RGB u16 | 139.3 MiB | 227.8 MiB |
| RGB f32 | 278.5 MiB | 455.6 MiB |
| RGBA f32 | 371.4 MiB | 607.5 MiB |
| RGBA u8 rendition | 92.8 MiB | 151.9 MiB |

A full X100VI float RGBA frame exceeds today's per-frame bound. Even RGB float plus mosaic, decoder scratch, old/new previews, crop intermediates and GPU copies may exceed a process budget. The integration contract records phase-by-phase liveness, including replacement while the previous rendition remains visible; remaining whole-process/GPU costs require measured qualification. Release native scratch before later passes, share immutable allocations, cap retained source/development caches by bytes and account for in-flight references during eviction. Prefer one reusable high-precision intermediate; avoid holding mosaic, several float frames and several upload copies together without measured need. Tiles require algorithm-specific halos and seam tests; decoder internals are not magically tiled by a tiled host output.

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

The editor keeps the last image with a loading state until neutral development is ready; it does not display embedded camera previews. If first-preview measurements justify adding them, require an explicit “Camera preview — developing RAW” state, separate identity, safe orientation/dimensions/profile checks and read-only display. Embedded pixels never satisfy source detail, edits, sampling, histogram, acceptance or export; absent/corrupt thumbnails must not prevent an otherwise valid RAW from opening. No silent preview-only success after decode failure.

Current discoverable operations use the same command service as the desktop:

| Operation | Semantics |
| --- | --- |
| `source.inspect`, `module.list` with optional `asset_id` | Source kind, immutable interpretation, readiness and applicable provider/control descriptors |
| `catalog.import`, `source.prepare`, `job.status`, `job.cancel`, `job.adopt` | Bounded asynchronous work; client-owned jobs, newest-import adoption and explicit preparation retries |
| `edit.set-raw-exposure`, `edit.set-raw-temperature`, `edit.set-raw-tint` | Revision-checked source-layer patches; EV, custom Kelvin and custom Lightwell tint |
| `edit.set-raw-red-gain`, `edit.set-raw-blue-gain` | Explicit sensor gain patches for programmatic callers |
| `edit.pick-raw-neutral`, `edit.use-as-shot-wb`, `edit.reset-raw` | Bounded pre-WB sensor patch solver, exact captured WB restore and source-development reset |
| Existing render/sample/history/version operations | The same RAW recipe/source identity, with display conversion only at the terminal boundary |

The [user guide](../user-guide.md) documents parameters and asynchronous client usage. Unsupported model/mode or required DNG correction, corrupt input, missing calibration/provider, incompatible interpretation, changed/missing source, resource limits and cancelled/stale work produce explicit failures. MCP will inherit this registry when its separately planned adapter exists.

## Export, recovery and portability

RAW evaluation feeds the same frozen-snapshot JPEG exporter planned in [single-image](../specs/single-image.md#export-follow-up). Keep quality 90, sRGB profile, corrected geometry/orientation, metadata stripped by default, and no overwrites or source aliases. Keep metadata uses the chosen descriptive/capture/GPS whitelist; do not copy a NEF/RAF container, opaque MakerNotes, CFA tags, embedded JPEG or stale thumbnail wholesale into JPEG. Camera profile/matrix data is not the output ICC profile. Export full-resolution development, never the Fit/embedded preview.

That exporter is not implemented in this checkout. RAW export integration can only complete once the named capability is delivered. If it remains absent, report the output gate as outstanding while allowing A/B verification. Locate is also separate: protect missing/changed RAW originals now and add NEF/RAF cases to Locate acceptance when it exists, without implementing a parallel relinker. No source-side sidecars or DNG intermediates are written.

Package the decoder/calibration resources reproducibly with no system LibRaw requirement, runtime downloads, user Python installation or separate processing application. Build/package checks cover macOS arm64, Windows x64 and Linux x64 using the existing baseline. Native Windows/Linux desktop and manual dependency/asset audits remain deferred; headless or VM evidence does not replace them. Automated Mac editor checks use `smoke`, `measure`, `hardening` or `develop --background` with isolated catalogs. Foreground interaction still needs the owner's explicit request.

## Fixtures and acceptance

Keep authentic owner originals under ignored `fixtures/raw/` or `private/` and identifiable derived previews under ignored `private/` or `artifacts/`, with a local manifest; do not commit/upload them or download an entire external corpus. Obtain redistribution permission before adding authentic public fixtures. Small synthetic Bayer/X-Trans arrays, levels, matrix cases and malformed inputs can be checked in. Synthetic arrays prove stage math, not NEF/RAF container support. The absence of a permitted real-file CI sample remains a visible coverage gap, not a synthetic substitute.

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

## Remaining qualification and decisions

The task DAG separates code delivery from broad visual quality, recovery, resource and platform qualification. It must not mark missing controlled scenes, sanitizer coverage or native target evidence as passed merely because the implementation works on the supplied files.

| Outstanding item | Current boundary |
| --- | --- |
| Controlled quality | Neutral/color charts, high ISO, fabric/foliage, clipped highlights, pushed shadows, Fuji DR/shutter variants and real-container orientation/preview combinations remain unqualified |
| DJI qualification | Required GainMap/WarpRectilinear and the supplied-file editor path are implemented; additional firmware/encodings and controlled optical/color scenes remain unqualified |
| Resource budget | The 1.5 GiB target is an investigation hypothesis. Screenshot-free Fuji series peak at 1583–1647 MiB; captured 30-trial p95 is 1992 MiB, with an unexplained 2436 MiB maximum. Attribute remaining costs without claiming an accepted budget |
| Native portability | M4 Metal evidence is distinct from automated Windows/Linux builds and native desktop checks; manual audits remain deferred |
| Output/Basic | No duplicate exporter or JPEG Basic controls; integrate with those capabilities when delivered |

Neutral rendering, exact as-shot defaults, custom WB mapping and the retained working domain are explicit implementation choices documented above. They are not assertions that the owner accepted a particular visual match, arbitrary performance relaxation or a wider feature scope. New consequential tradeoffs still require concrete evidence and consultation.

## Performance-rules checklist for implementation

| Required question | Implementation / evidence contract |
| --- | --- |
| Reads/hashes/decodes | One verified preparation/cache route; no direct request-path `open_source`; consistent source bytes and signature validation |
| Full-frame allocations | Explicit typed source/development/output buffers with a phase ledger, within the [RAW admission contract](architecture.md#rendering-and-limits), plus bounded worker concurrency |
| Point/no-op work | Parameter/geometry checks stay bounded; prepared point sampling uses shared math; cache misses return preparation state; the as-shot equivalent `recipe.describe` reports is a bounded solve over the locus, about 190 scanned temperatures and 13 µs, with no source access |
| Owner-thread work | Only metadata/transactions/session completions; no RAW read/hash/unpack/develop/raster/encode |
| Desktop refreshes | One state/entry merge and preview request per committed change; source readiness never refetches whole history |
| Timers | Reuse existing gated work/event polling; progress only while work exists; no RAW idle timer |
| Photo-sized evidence | M4 real Z6/X100VI stage distributions and RSS plus before/after 24/60 MP JPEG diagnostics |
| Correctness/sharing | Independent integer/float/geometry references, unchanged JPEG goldens and allocation/cache-sharing assertions |
