# OS-native object segmentation and removal APIs, and prior art in photo editors (as of 27 September 2026)

Scope note: this covers what macOS, Windows and Linux provide natively, a code-level read of OpenStill, and how Adobe and other editors (commercial and open source) implement object removal and AI masking, including UX. Model-level research (LaMa, SAM, diffusion inpainting and so on) is only touched where an editor or OS uses it. "Documented" means stated in a vendor or project primary source; "inference" is marked as such.

OpenStill links are permalinks to commit `99f96974912fd0700f2fa957023ff4636301f7c8` (main HEAD, 2026-09-27), abbreviated below as `OS@99f9`.

## 1. OpenStill: how it segments objects, how it removes them, and which frameworks, models and licenses it uses

### Takeaway
OpenStill (MIT, Swift, macOS 13+, created 2026-09-25) segments objects with Apple Vision only (click-to-select via `VNGenerateForegroundInstanceMaskRequest`, plus person, face-landmark and saliency-free masks), and removes objects with a separate, optional Python worker that runs the OpenCV ONNX export of LaMa on CPU at a fixed 512×512 over a padded crop. It has no hover highlighting, no auto-detection of distractions, no shadow handling, no variations and no non-Apple segmentation path. Removal bakes the fully rendered image into a new base raster, so earlier edits stop being live parameters.

### Cited Findings
**Project facts**
- Repository metadata: description "An open source alternative to Adobe Lightroom.", license MIT, language Swift, created 2026-09-25T12:43:40Z, 1 star at the time of reading — [GitHub API: haon-v2/OpenStill](https://github.com/haon-v2/OpenStill).
- Contributors by commit count: `claude` 134, `haon-v2` 27, `github-actions[bot]` 22. The 100 most recent commits span 2026-09-26 to 2026-09-27 — [GitHub contributors](https://github.com/haon-v2/OpenStill/graphs/contributors).
- `Package.swift` targets `.macOS(.v13)`, uses Swift tools 6.0, and has one package dependency (Sparkle). It vendors LibRaw and links lensfun and lcms2. Neither Core ML nor any Windows or Linux target appears — [Package.swift L1–22](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Package.swift#L1-L22).
- The README requires "macOS 13+ to run and the macOS 26 SDK with Swift 6+ to build". Optional AI "uses a separate Python runtime" — [README L307](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/README.md#L307).
- THIRD_PARTY_NOTICES: "OpenStill source code is MIT licensed. AI weights are downloaded separately and retain their own licenses." It also says "Apple Vision supplies foreground-instance segmentation and horizon detection through public operating-system APIs… These are separate from Apple Intelligence / Photos Clean Up." — [THIRD_PARTY_NOTICES.md L3, L23](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/THIRD_PARTY_NOTICES.md).

**Segmentation and auto-masking (all Apple Vision except sky and depth)**
- Click-to-select object: `VisionEditor.objectMask(_:at:)` guards on macOS 14 and runs `VNGenerateForegroundInstanceMaskRequest` on the whole image. It reads the UInt8 `instanceMask` label under the clicked point (0 means background, which raises an error), then calls `generateScaledMaskForImage(forInstances:[label], from:handler)` to get a full-resolution soft mask — [VisionEditor.swift L14–31](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/VisionEditor.swift#L14-L31).
- On macOS 13 the error reads "AI object selection requires macOS 14 or later. Brush, linear, and radial masks are available." When no instance is under the point it reads "No distinct foreground object was found at that point. Click inside a clear subject or use a brush mask." — [VisionEditor.swift L37–39](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/VisionEditor.swift#L37-L39).
- The click handler downsizes the image to at most 1600 px on the long edge before running Vision, then stores the result as an `AdjustmentMask(kind:"object")` with `feather = 0.1` and commits one history step titled "· AI object mask" — [MaskEditingController.swift L159–183](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStill/MaskEditingController.swift#L159-L183).
- "Select with AI…" masks run on an image capped at 2048 px (`aiBaseImage(... maximum: 2048)`) — [AIMaskController.swift L8–15](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStill/AIMaskController.swift#L8-L15).
- Mask kinds are subject, background, people, person, face, eyes, eyebrows, lips, skin, sky and depth — [AIMasks.swift L8–25](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/AIMasks.swift#L8-L25). Each is implemented as follows:
  - Subject: the union of all foreground instances, via `generateScaledMaskForImage(forInstances: observation.allInstances, …)`. Background is the inverted subject mask (`CIColorInvert`) — [AIMasks.swift L62–73](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/AIMasks.swift#L62-L73).
  - People: `VNGeneratePersonSegmentationRequest` with `qualityLevel = .accurate` and an 8-bit output. A mask covering 0.2% of the image or less counts as "nothing found" — [AIMasks.swift L78–87](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/AIMasks.swift#L78-L87).
  - Person N: `VNGeneratePersonInstanceMaskRequest` on macOS 14+. The code comment says "up to four" people. Instances are ordered left to right by the horizontal centroid of a 64×32 thumbnail mask — [AIMasks.swift L88–120](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/AIMasks.swift#L88-L120).
  - Face, eyes, eyebrows and lips: `VNDetectFaceLandmarksRequest` polygons, filled or stroked into a CGContext and then Gaussian-blurred. The face is the convex hull of the jaw, the brows and the top of the face box — [AIMasks.swift L126–163](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/AIMasks.swift#L126-L163).
  - Skin: the people mask multiplied by a YCbCr skin-chroma `CIColorKernel` — [AIMasks.swift L177–197](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/AIMasks.swift#L177-L197).
  - Depth: iPhone Portrait disparity or depth auxiliary data where present. Otherwise the Depth Anything V2 Small worker, or a "stand-in" that treats the subject as near and blurs it — [AIMasks.swift L202–237](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/AIMasks.swift#L202-L237).
- The README states the scope: "AI object: click inside a distinct foreground subject. Uses Apple's on-device Vision instance segmentation on macOS 14+; it is not arbitrary text-prompt/background-object selection." It also says "Everything except sky runs with Apple Vision on this Mac" and "Sky: the local AI tools' U2-Net model, as a mask only" — [README L387–393](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/README.md#L387-L393).
- Other Vision uses: `VNDetectHorizonRequest` for auto-straighten ([VisionEditor.swift L6–13](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/VisionEditor.swift#L6-L13)), and Vision feature prints for duplicate detection and face grouping ([README L115, L159](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/README.md#L115)).

**Removal (the "Erase AI" tool)**
- The UX has two steps. First, "create a brush, linear, radial, or AI object mask over the unwanted area". Then, "Remove selected area. LaMa processes a padded region around the mask. Cover the object including its edges." — [README L453](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/README.md#L453). The panel help text says "Choose Masking to select an area, then return here to remove it. AI fills it using the surrounding photograph." — [EditorPanel.swift L273–276](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStill/EditorPanel.swift#L273-L276). In the Lightroom-style layout, Remove shows "Remove with AI: select the area with Masking, then:" above the heal/clone controls — [EditorPanel.swift L712–713](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStill/EditorPanel.swift#L712-L713).
- Model manifest: `erase` downloads `inpainting_lama_2025jan.onnx` (92,591,623 bytes) from `huggingface.co/opencv/inpainting_lama`, pinned by revision and SHA-256. Sky uses `skyseg.onnx` (176 MB). There are also SCUNet (denoise), Real-ESRGAN x4v3 (detail) and Depth Anything V2 Small (99 MB) — [Resources/AI/models.json](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Resources/AI/models.json).
- Model licenses: LaMa (OpenCV ONNX distribution) is Apache 2.0. The sky model ("Sky Segmentation and Post-processing", xiongzhu666) is MIT. SCUNet and Depth Anything V2 are Apache 2.0, and Real-ESRGAN is BSD-3 — [THIRD_PARTY_NOTICES.md L5–11](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/THIRD_PARTY_NOTICES.md).
- Runtime: `engine.py setup` creates a venv (Python 3.10–3.12) and pip-installs `onnxruntime==1.23.2`, `numpy==2.2.6` and `Pillow==12.0.0` as binary wheels. Inference uses `providers=['CPUExecutionProvider']` with `intra_op_num_threads = 4`, so there is no Core ML or GPU execution provider — [engine.py L9–41](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Resources/AI/engine.py#L9-L41).
- Process model: the app spawns `python3 engine.py <tool> --input … --output … --mask …` as a `Process`, streams stdout lines as status, and cancels via `terminate()` — [LocalAI.swift L26–68](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStill/LocalAI.swift#L26-L68).
- Erase algorithm (the float path):
  1. Take the mask bounding box and pad it by `max(48, 0.4 × max(bbox w, h))`.
  2. Crop that region and Lanczos-resize it to exactly 512×512 in BGR order. The mask goes to 512×512 with nearest-neighbour resampling and is binarized (>0).
  3. Run LaMa and resize the output back to the crop size.
  4. Composite `rgb*(1-mask) + restored*mask`, so unmasked pixels are exact. Out-of-range (HDR) residuals are kept outside the model's [0,1] input.
  — [float_bridge.py L126–140, L100–106](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Resources/AI/float_bridge.py#L126-L140). The legacy 8-bit path is the same algorithm ([engine.py L75–90](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Resources/AI/engine.py#L75-L90)).
- Pipeline integration: `runAI("erase")` renders the full-resolution photo with all current edits through `ModernRenderer.render` into an `.osfloat` file, rasterizes the Erase tool's mask (`adjustmentMask.coverage`) and runs the worker — [EditingController.swift L331–361](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStill/EditingController.swift#L331-L361). On success it creates a fresh `PhotoEdits()` whose `baseAsset` is the AI output and whose `aiBackgroundAsset` is the pre-AI render, and saves the mask as the tool's blend mask — [EditingController.swift L417–423](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStill/EditingController.swift#L417-L423). The renderer blends `aiBackgroundAsset` and the AI output with `CIBlendWithMask` using that mask — [PhotoEdits.swift L188, L206–209](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/PhotoEdits.swift#L206-L209).
- The README documents this: "Edits made before running an AI tool become its input; the latest AI result's blend mask remains editable… Erase cannot remove a new object merely by expanding the result's blend mask: select the new region and run removal again." — [README L399](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/README.md#L399).
- Classic retouch is separate: Heal and Clone strokes are stored as data (source, destination, points, radius, feather, opacity). Heal is a frequency-separation-style kernel `s - blur(s) + blur(target)` over a Gaussian of 0.7× the brush radius — [Retouch.swift L4–76](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/Retouch.swift#L4-L76).
- On Apple Clean Up, the README says: "Apple's public Apple Intelligence developer APIs do not expose the Photos Clean Up removal model. OpenStill uses Apple Vision for object masks/horizon detection and LaMa for local removal" — [README L459](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/README.md#L459).
- QA evidence is thin. LaMa was smoke-tested on a 256×192 image in "roughly 3.1 s", and the notes say "These are smoke timings, not full-resolution benchmarks." The QA file also says "Not yet verified by hand on a Mac: Vision masks on real portraits and groups…", and lists "physically accurate relighting, arbitrary background-object segmentation, and Apple Photos Clean Up integration are not provided" — [QA.md L28, L55, L185](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/QA.md).
- The canvas's only `mouseMoved` handler updates the brush-outline position. No hover segmentation exists — [PhotoCanvas.swift L50–53](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStill/PhotoCanvas.swift#L50-L53).

### Inferences
- OpenStill's Vision recipe is reusable for Luxforge's macOS "native" path: run the foreground-instance request once per image, look up the label under the cursor for O(1) hit-testing, then request a scaled soft mask only for the chosen instance. Because the label map is computed once, hover-to-highlight could be instant (a byte lookup plus drawing the low-res instance outline), although OpenStill does not do this.
- Its weaknesses are useful warnings:
  - Only "distinct foreground" objects are selectable, so background clutter such as poles, signs and distant people often is not an instance.
  - LaMa always runs at 512×512 regardless of region size, so large regions are upscaled and soft.
  - Execution is CPU-only through a user-installed Python.
  - Shadows are ignored.
  - Baking the full render into `baseAsset` conflicts with Luxforge's rule that edits are ordered, re-renderable layers. Luxforge would want removal to be a layer whose output is cached and whose inputs (mask, seed, model id and version) are recorded.
- The project is two days old, mostly Claude-authored and has one star. Treat it as a design sketch, not proven practice.

### Gaps
- No OpenStill measurements of Vision latency or quality on full-size photos, or of LaMa at full resolution. Its own QA says these were not verified.
- It was not checked whether OpenStill ever calls `VNGenerateObjectnessBasedSaliencyImageRequest` or the attention request. A grep of the core sources found no use.

## 2. Apple (macOS): Vision requests, Swift-only APIs, macOS 27 iterative segmentation, Clean Up, Core Image, and callability from Rust

### Takeaway
macOS has strong native segmentation but no public object-removal API:
- **Class-agnostic foreground instances** (macOS 14+, Objective-C, callable from Rust through `objc2-vision`).
- **Person segmentation and person instances** (macOS 12 and 14+).
- **Saliency** (64×64 or 68×68 heat maps).
- **A SAM-like `GenerateIterativeSegmentationRequest`** (macOS 27 "Golden Gate", released 14 September 2026). It takes point, box or scribble seeds plus include and exclude points. It is Swift-only, needs a one-time asset download, and needs a Swift shim from Rust.

Photos Clean Up, Image Playground and Apple Intelligence editing expose no inpainting or removal API, and Core Image has no inpainting filter. Inpainting on macOS therefore has to be Luxforge's own model, run through Core ML or ONNX Runtime's Core ML provider.

### Cited Findings
**Foreground instance segmentation (subject lifting)**
- `VNGenerateForegroundInstanceMaskRequest`: "A request that generates an instance mask of noticable objects to separate from the background". Availability is macOS 14.0 / iOS 17.0 — [Apple docs JSON](https://developer.apple.com/documentation/vision/vngenerateforegroundinstancemaskrequest). Its declaration exists in both Swift and Objective-C (the `occ` language variant) — [same](https://developer.apple.com/documentation/vision/vngenerateforegroundinstancemaskrequest).
- `VNInstanceMaskObservation` has these members (macOS 14):
  - `instanceMask` ("The resulting mask that represents all instances") and `allInstances`.
  - `generateMask(forInstances:)`: "Creates a low-resolution mask from the instances you specify".
  - `generateScaledMaskForImage(forInstances:from:)`: "Creates a high-resolution mask…".
  - `generateMaskedImage(ofInstances:from:croppedToInstancesExtent:)`: "Creates a high-resolution image…".
  — [VNInstanceMaskObservation docs](https://developer.apple.com/documentation/vision/vninstancemaskobservation)
- WWDC23 session 10176 said:
  - The request "produces a soft segmentation mask at the same resolution" as the input.
  - The instance mask maps pixels to instance indices, with 0 reserved for the background and foreground instances labelled from 1. "The ordering of these IDs is not guaranteed."
  - It is "class agnostic… Any foreground object, regardless of its semantic class".
  - It is "a resource intensive task and best deferred to a background thread".
  - Vision runs in-process and "isn't as limited in image resolution as VisionKit", which works out of process.
  — [WWDC23: Lift subjects from images in your app](https://developer.apple.com/videos/play/wwdc2023/10176/)
- In the same session, Apple contrasts saliency ("coarse, region-based… fairly low resolution") with person segmentation, person instance segmentation and subject lifting, which "works on any object type" — [WWDC23 10176](https://developer.apple.com/videos/play/wwdc2023/10176/).

**People, saliency, human boxes and animals**
- `VNGeneratePersonInstanceMaskRequest` ("a mask of individual people"): macOS 14.0 — [Apple docs](https://developer.apple.com/documentation/vision/vngeneratepersoninstancemaskrequest).
- `VNGeneratePersonSegmentationRequest` ("a matte image for a person"): macOS 12.0, output as `VNPixelBufferObservation`, with `qualityLevel` fast, balanced or accurate — [Apple docs](https://developer.apple.com/documentation/vision/vngeneratepersonsegmentationrequest).
- Reported quality-level costs: `.fast` about 10 ms at 256×144, `.balanced` about 100 ms at 960×540, `.accurate` about 1 s. "The frame size for the accurate level is 64x compared to the fast", and `.accurate` is for still images — [Kodeco (secondary, based on WWDC21)](https://www.kodeco.com/29650263-person-segmentation-in-the-vision-framework/page/2). Kodeco also calls `.accurate` "full resolution", which conflicts with "64×" (about 2048×1152), so treat both as approximate.
- Saliency (`VNGenerateAttentionBasedSaliencyImageRequest`, `VNGenerateObjectnessBasedSaliencyImageRequest`) has been available since macOS 10.15. "The heat map is a CVPixelBuffer in a one-component floating-point pixel format. Its dimensions are 64 x 64 when fetched in real time, or 68 x 68 when requested in its deferred form." `salientObjects` gives bounding boxes — [VNSaliencyImageObservation docs](https://developer.apple.com/documentation/vision/vnsaliencyimageobservation). Attention returns at most 1 box and objectness at most 3 — [Kamil Tustanowski (secondary)](https://medium.com/@kamil.tustanowski/saliency-detection-using-the-vision-framework-d53a38e4ccaa).
- `VNDetectHumanRectanglesRequest` has been available since macOS 10.15. `upperBodyOnly` (macOS 12) defaults to true — [Apple docs](https://developer.apple.com/documentation/vision/vndetecthumanrectanglesrequest/upperbodyonly).
- `VNRecognizeAnimalsRequest` (macOS 10.15) recognises only `cat` and `dog` (`VNAnimalIdentifier`) — [Apple docs](https://developer.apple.com/documentation/vision/vnanimalidentifier).
- `VNRequest.setComputeDevice(_:for:)` (macOS 14) pins a compute stage to a specific `MLComputeDevice` (CPU, GPU or Neural Engine) — [Apple docs](https://developer.apple.com/documentation/vision/vnrequest/setcomputedevice(_:for:)).

**Swift-only Vision API (macOS 15) and later additions**
- In the macOS 15 Swift API, `GenerateForegroundInstanceMaskRequest` (a `struct`) and `GeneratePersonInstanceMaskRequest` exist only in Swift (the docs list only the `swift` language) — [Apple docs](https://developer.apple.com/documentation/vision/generateforegroundinstancemaskrequest).
- macOS 26 added `DetectLensSmudgeRequest` (A14/M1 or later) and document recognition, but no new segmentation — [Apple docs](https://developer.apple.com/documentation/vision/detectlenssmudgerequest).
- The Vision docs index groups "Image segmentation and subject lifting" as: GenerateForegroundInstanceMaskRequest, GeneratePersonInstanceMaskRequest, GeneratePersonSegmentationRequest, GenerateIterativeSegmentationRequest, and the sample "Segmenting objects using taps, scribbles or rectangles" — [Vision docs](https://developer.apple.com/documentation/vision).

**macOS 27 iterative segmentation (new)**
- `GenerateIterativeSegmentationRequest` is on macOS / iOS 27.0: "A request that generates a segmentation mask from points, a rectangle, or a scribble."
  - "Initialize with a seed point, a seed rectangle, or a seed scribble buffer… Then add points to iteratively refine… maximum of 13 points when seeded with a point or scribble, or 11 points when seeded with a box."
  - It is declared as `final class` and documented only in Swift.
  - Methods: `addIncludedPoint(_:)`, `addExcludedPoint(_:)`, `qualityLevel` (fast / balanced / accurate: "Higher quality levels produce a smoother, higher-resolution mask").
  - The seed scribble is a `CVReadOnlyPixelBuffer`.
  - `Result = PixelBufferObservation?`: "a gray mask image. It can be nil if there is nothing to segment".
  - It conforms to `DownloadableAssetsRequest`.
  — [Apple docs](https://developer.apple.com/documentation/vision/generateiterativesegmentationrequest)
- `DownloadableAssetsRequest` (macOS 27): "Inspect assetStatus… and call downloadAssets() to initiate the download when they are not [ready]" — [Apple docs](https://developer.apple.com/documentation/vision/downloadableassetsrequest).
- WWDC26 session 237, "What's new in image understanding", introduced tap, box, lasso and scribble segmentation with include and exclude refinement. Coordinates are normalized with the origin at the lower left, lasso strokes should be "at least 1% of total image width", and an initial model download is required before first use. It gave no latency or resolution figures — [WWDC26 session 237](https://developer.apple.com/videos/play/wwdc2026/237/). The sample "needs to run on a physical device" — [Apple sample](https://developer.apple.com/documentation/vision/segmenting-objects-using-taps-scribbles-or-rectangles).
- macOS 27 "Golden Gate" was released to the public on 14 September 2026 — [MacRumors](https://www.macrumors.com/2026/09/10/macos-27-golden-gate-release-date/); [9to5Mac](https://9to5mac.com/2026/09/09/apple-confirms-macos-27-golden-gate-launch-date-september-14/).

**VisionKit**
- `ImageAnalysisOverlayView` (AppKit) exposes `subjects`, `highlightedSubjects` (settable) and `subject(at:) async`, which "Returns the subject at the given point… nil, if no subject resides at point". The docs list macOS 13.0 availability, although these subject APIs were introduced at WWDC23 alongside macOS 14 — [Apple docs](https://developer.apple.com/documentation/visionkit/imageanalysisoverlayview/subject(at:)); [WWDC23 10176](https://developer.apple.com/videos/play/wwdc2023/10176/).
- VisionKit analysis is out-of-process and size-limited — [WWDC23 10176](https://developer.apple.com/videos/play/wwdc2023/10176/).

**Clean Up, Image Playground and Core Image**
- Clean Up in Photos requires a Mac with M1 or later running macOS Sequoia 15.1+, and is unavailable in mainland China — [Apple Support 121429](https://support.apple.com/en-us/121429).
- No public Clean Up API was found in Apple's Vision, Image Playground or developer pages, and OpenStill's author reached the same conclusion — [OpenStill README L459](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/README.md#L459).
- Image Playground (macOS 15.1+) generates images "based on descriptive information" from text, concepts and an optional source image, through a system sheet or view controller. It does not accept masks — [Apple docs](https://developer.apple.com/documentation/imageplayground). Its programmatic `ImageCreator` (macOS 15.4) is deprecated in macOS 27 with "Use ImagePlaygroundViewController or imagePlaygroundSheet" — [Apple docs](https://developer.apple.com/documentation/imageplayground/imagecreator).
- Core Image: a scan of the Core Image documentation index found no inpainting or object-removal filter. This is inference from the documentation, not an Apple statement. Core Image does offer `CIBlendWithMask`, which Apple recommends for compositing Vision masks — [WWDC23 10176](https://developer.apple.com/videos/play/wwdc2023/10176/).

**Calling these from Rust**
- `objc2-vision` 0.3.2 has Cargo features for `VNGenerateForegroundInstanceMaskRequest`, `VNGeneratePersonInstanceMaskRequest`, `VNGeneratePersonSegmentationRequest`, `VNGenerateAttentionBasedSaliencyImageRequest` and `VNGenerateObjectnessBasedSaliencyImageRequest` — [docs.rs objc2-vision Cargo.toml](https://docs.rs/crate/objc2-vision/latest/source/Cargo.toml.orig).
- `objc2-core-ml` and `objc2-core-image` 0.3.2 also exist, but no `objc2-vision-kit` crate was found on crates.io — [crates.io](https://crates.io/crates/objc2-vision).

### Inferences
- These combinations are candidates for a macOS "native segmentation" tier:

  | macOS | Capability |
  | --- | --- |
  | 14+ | Instance map for hover and click, from Objective-C `VNGenerateForegroundInstanceMaskRequest` via objc2 |
  | 14+ | Person instances, via objc2 |
  | 10.15+ | Objectness saliency for "suggested distractions", coarse only |
  | 27+ | `GenerateIterativeSegmentationRequest` for brush and scribble seeds and point refinement |

  The 27+ request exactly matches the "paint a brush, find the object" UX, but it needs a small Swift static library or dylib with a C ABI, Rust FFI, and handling of the model-download state.
- The foreground-instance request only returns "noticeable" foreground objects. Small background distractions (litter, signs, wires) will often be missing from the instance map. The brush-seeded iterative request, or a portable SAM-class model, is needed for those.
- The user's M4 MacBook Pro runs macOS 27 now, so the Swift-only path is testable on the owner's machine. Earlier macOS versions need the fallback.
- Because no Apple API performs removal, Luxforge ships its own inpainting on every platform. The only "native" choice on macOS is the accelerator: Core ML with ANE or GPU, or ONNX Runtime's Core ML execution provider, which darktable uses (see section 6).

### Gaps
- No Apple-published output resolution, latency or ANE usage for `VNGenerateForegroundInstanceMaskRequest`'s `instanceMask` beyond "low-resolution", and none for the iterative request. Both need measurement on the M4.
- WWDC23 10176 did not state a maximum instance count. The "up to four people" limit for person instances comes from OpenStill's code comment, not from Apple docs.
- Whether macOS 27's Photos Clean Up gained cloud or "High Quality" modes is reported only by a secondary how-to ([Gadget Hacks](https://apple.gadgethacks.com/how-to/how-to-use-clean-up-in-ios-27-photos-step-by-step-guide/)), which says iOS 27 adds "Auto, Fast, and High Quality" modes. This is unverified against Apple sources and does not affect the API situation.
- The `@nonobjc` compute-device API suggests part of the device selection may be Swift-only. The Objective-C equivalent was not checked.

## 3. Windows: Windows AI imaging APIs (ImageObjectExtractor, ImageObjectRemover and others), Windows ML, requirements, and Rust access

### Takeaway
The Windows App SDK ships three relevant imaging APIs:
- `ImageObjectExtractor`: SAM-like include points, exclude points and rectangles, returning a binary Gray8 mask.
- `ImageObjectRemover`: image plus Gray8 mask to inpainted image.
- `ImageForegroundExtractor`.

All of them currently run only on Copilot+ PCs (on the NPU), need a packaged app with the `systemAIModels` capability, and are delivered through Windows Update. A Rust app can reach them only through WinRT bindings generated from the Windows App SDK metadata plus package identity, which is awkward but documented for identity. Windows ML, which is ONNX Runtime with system-managed NPU and GPU providers, is the realistic native accelerator for Luxforge's own models.

### Cited Findings
**Object Erase (ImageObjectRemover)**
- "The model takes both an image and a greyscale mask indicating the object to be removed, erases the masked area from the image, and replaces the erased area with the image background." The mask "must be in Gray8 format with each pixel of the area to be removed set to 255 and all other pixels set to 0" — [Microsoft Learn: Image Object Erase](https://learn.microsoft.com/en-us/windows/ai/apis/image-object-erase).
- The flow is `ImageObjectRemover.GetReadyState()`, then `EnsureReadyAsync()`, then `CreateAsync()`, then `RemoveFromSoftwareBitmap(imageBitmap, maskBitmap)`, which is synchronous and returns a `SoftwareBitmap` — [same](https://learn.microsoft.com/en-us/windows/ai/apis/image-object-erase).
- The app "must be packaged as an MSIX package with the `systemAIModels` capability", and `MaxVersionTested` must be at least `10.0.26226.0` to avoid "Not declared by app" errors — [same](https://learn.microsoft.com/en-us/windows/ai/apis/image-object-erase).

**Object Extractor (ImageObjectExtractor)**
- Hints can be "Coordinates for points that belong to what you're identifying", "Coordinates for points that don't belong", and "A coordinate rectangle that encloses what you're identifying".
  - Guidance: avoid multiple rectangles, and avoid exclude points without include points.
  - "Don't specify more than the supported maximum of 32 coordinates (1 for a point, 2 for a rectangle)".
  - "The returned mask is in greyscale-8 format with the pixels of the mask… having a value of 255 (all others having a value of 0)", which means binary, not soft.
  - Usage: `ImageObjectExtractor.CreateWithSoftwareBitmapAsync(bitmap)` once per image, then `GetSoftwareBitmapObjectMask(new ImageObjectExtractorHint(includeRects, includePoints, excludePoints))`.
  — [Microsoft Learn: Image Object Extractor](https://learn.microsoft.com/en-us/windows/ai/apis/image-object-extractor)

**Hardware, versions and delivery**
- "The AI Imaging APIs (Image Super Resolution, Image Description, Image Segmentation, Image Foreground Extraction, and Object Erase) currently require a Copilot+ PC with an NPU." Microsoft recommends C2PA Content Credentials for modified images — [Microsoft Learn: AI Imaging overview](https://learn.microsoft.com/en-us/windows/ai/apis/imaging).
- In the supported-hardware table, Image Segmentation and Object Erase are "✅ Available" on NPU (Copilot+) and "❌ Not supported" on GPU and CPU (only Phi Silica, Speech and VSR have expanded). Object Erase shipped in Windows App SDK 1.8.0 (1.8.250907003), while "All other APIs" date from 1.7.1 (1.7.250401001) — [Microsoft Learn: What are Windows AI APIs?](https://learn.microsoft.com/en-us/windows/ai/apis/) (page dated 2026-07-15).
- Branch on `GetReadyState`:
  - `Ready`
  - `NotReady` (download through Windows Update; show consent first)
  - `DisabledByUser`
  - `NotSupportedOnCurrentSystem`: "Do not call EnsureReadyAsync… fall back to an alternative implementation"
  — [Microsoft Learn: Get started](https://learn.microsoft.com/en-us/windows/ai/apis/get-started)
- The manual setup documents Windows 11 Insider build 26120.3073+ as the development baseline — [same](https://learn.microsoft.com/en-us/windows/ai/apis/get-started).
- The Windows Photos app's "Generative erase" (formerly Spot fix) reached Arm64 and Windows 10 in February 2024. The consumer feature therefore predates, and is not limited like, the Copilot+-only developer API — [Windows Insider Blog, 2024-02-22](https://blogs.windows.com/windows-insider/2024/02/22/windows-photos-gets-generative-erase-and-recent-ai-editing-features-now-available-on-arm64-devices-and-windows-10/).

**Windows ML**
- Windows ML is "powered by ONNX Runtime" with "execution providers that Windows installs and keeps up to date via Windows Update", and has "Same ONNX APIs — no changes to your existing ONNX Runtime code".
  - "Support for CPU and GPU (via DirectML) is available on all supported Windows versions. Hardware-optimized execution providers for NPUs and specific GPU hardware require Windows 11 version 24H2 (build 26100) or greater."
  - It runs on x64 and ARM64 with "Any PC configuration".
  — [Microsoft Learn: What is Windows ML?](https://learn.microsoft.com/en-us/windows/ai/new-windows-ml/overview) (dated 2026-09-01)

**Rust access**
- Microsoft's Rust guide (2026-09-24) says "Package identity… allows your application to access specific Windows APIs (like Notifications, Security, AI APIs, etc)". It shows `winapp init`, `winapp run .\target\debug` (a loose-layout package, no signing, for debugging) and `winapp pack` to MSIX, using the `windows` crate. Its SDK-setup step says "Rust uses its own `windows` crate, not the C++ SDK headers" — [Microsoft Learn: Using winapp CLI with Rust](https://learn.microsoft.com/en-us/windows/apps/dev-tools/winapp-cli/guides/rust).
- `windows-rs` "generates Rust bindings from Windows metadata", and `windows-bindgen` 0.100.0 (2026-09-03) is the generator for metadata the prebuilt crates do not cover. The experimental `windows-app-rs` crate for the Windows App SDK is archived — [microsoft/windows-rs](https://github.com/microsoft/windows-rs); [microsoft/windows-app-rs](https://github.com/microsoft/windows-app-rs); [crates.io windows-bindgen](https://crates.io/crates/windows-bindgen).

### Inferences
- `ImageObjectExtractor`'s hint model (include points, exclude points, rectangle, 32-coordinate cap) is effectively a SAM-style prompt API, and `CreateWithSoftwareBitmapAsync` appears to encode the image once. Repeated hint queries should therefore be cheap enough for click refinement, but its binary mask would need feathering. Hover latency is unknown.
- Given the hard requirements (Copilot+ NPU only, MSIX or loose-layout package identity, the Windows App SDK runtime, and C# or C++ samples only), a Windows native tier for Luxforge is optional polish at most. It suits a later "native provider" behind the same interface. Windows ML with Luxforge's own ONNX models (via the `ort` crate or the Windows ML ORT copy) is the practical Windows accelerator.
- The Windows AI APIs are WinRT types inside the Windows App SDK framework package, so Rust bindings would come from the SDK's `.winmd` via `windows-bindgen`, and the app must bootstrap or declare the Windows App SDK runtime dependency. No Microsoft sample does this from Rust for the AI APIs. This is inferred from the archived windows-app-rs and the guide.

### Gaps
- No independent quality or latency evaluation of `ImageObjectRemover` or `ImageObjectExtractor` was found, and neither is mask resolution. The model behind the Photos app's Generative erase and its relationship to the API are undocumented.
- It was not verified whether the Windows AI imaging APIs work with a "package with external location" (sparse package) for an unpackaged Win32 or Rust app. The docs require MSIX or `systemAIModels`, and the winapp guide shows loose-layout identity.
- It is unknown whether `ort` can target the Windows ML system ORT copy directly.

## 4. Linux: is there any native equivalent?

### Takeaway
No. There is no OS-level segmentation or inpainting service on Linux. Linux editors bundle or download their own models and use ONNX Runtime (or PyTorch) with CUDA, ROCm, OpenVINO or CPU backends.

### Cited Findings
- darktable 5.6's AI subsystem lists its acceleration providers as macOS: CoreML; NVIDIA: CUDA; AMD: ROCm; Windows: DirectML; Intel: OpenVINO; CPU fallback. "Nothing leaves your machine… Built without `USE_AI`, the whole subsystem disappears", and models are "Downloaded on-demand" — [darktable: Meet the new AI tools in darktable 5.6](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/).
- Intel's GIMP 3 plugins use OpenVINO for semantic segmentation and SD 1.5 / SDXL inpainting on CPU, GPU or NPU — [intel/openvino-ai-plugins-gimp](https://github.com/intel/openvino-ai-plugins-gimp).
- IOPaint runs self-hosted on "CPU & GPU & Apple Silicon" through PyTorch — [Sanster/IOPaint README](https://github.com/Sanster/IOPaint).

### Inferences
- The portable fallback (for example ONNX Runtime through `ort` with CUDA, ROCm, OpenVINO or CPU providers) is the only Linux path, and it is also the fallback on older macOS and non-Copilot+ Windows.

### Gaps
- No primary source says that no Linux desktop segmentation or inpainting API exists. The absence is inferred from Linux editors all shipping their own models and from finding no freedesktop, GNOME or KDE service.

## 5. Adobe Lightroom, Lightroom Classic and Photoshop: Generative Remove, Detect objects, Distraction Removal, AI masks, Object Selection hover and the Remove tool

### Takeaway
Adobe's pattern is "rough brush, then AI refines to the object including its shadow and reflection (Detect objects), then remove". Removal is either on-device Content-Aware Remove or cloud Firefly Generative Remove, which offers 3 variations and a Generate button for 3 more.

One-click Distraction Removal auto-detects people (with per-person pins you delete to keep someone), reflections (with an Amount slider and a quality level) and dust. Photoshop adds hover-to-highlight object finding and "Find distractions" (general, people, wires and cables). As of August 2026, Photoshop also offers an on-device generative Remove model.

### Cited Findings
**Lightroom Classic Remove tool and Generative Remove**
- "Once you identify and highlight an object using the brush mask, Adobe Firefly automatically removes the object and the shadow around it, and generates a fill that blends with the rest of the frame." Also: "Generative credits will not be deducted… To use Generative Remove, an internet connection is required. However, the Remove, Clone, and Heal tools can function offline." — [Adobe HelpX: Lightroom Classic Remove tool](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/remove-tool.html) (updated 27 October 2025).
- The flow is Develop, Remove, then "Use Generative AI", with the mode set to Remove:
  1. Brush over the object. "If Detect objects is selected, Lightroom Classic will also identify the shadow and reflections (if any) around the object and include it in the selection."
  2. Refine with Add, Subtract and Size.
  3. Choose Remove. "Lightroom Classic will use Adobe Firefly to generate three different variations. You can select a variation or click Generate to create three new variations."
  4. Cycle variations with arrows. A per-variation menu offers Delete Variation and Report Variation.
  - Tool Overlay options control mask overlay on hover, and "Visualize Spots" shows a high-contrast overlay.
  - "press Cmd (Mac) or Ctrl (Windows) while releasing the mouse after brushing" to skip the adjustments.
  — [same](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/remove-tool.html)
- Detect objects: "Detect Objects lets you detect an object within the roughly brushed area… When you brush over an object, Lightroom Classic automatically selects the object and any shadows or reflections around it". It works only with Content-Aware Remove and Generative Remove, not Heal or Clone. "Content-Aware Remove is used whenever the Use Generative AI checkbox is unchecked."
  - Refinement uses Add and Subtract, Size and Overlay color.
  - A selection's mode can be switched later, but "if the current selection already has Detect objects applied, you can only toggle between Generative Remove and Remove."
  — [same](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/remove-tool.html)
- The FAQ says Generative Remove is on all Lightroom surfaces and Adobe Camera Raw. It "works on both raw and non-raw photos… fully non-destructive", uses the Firefly model behind Photoshop's Generative Fill and Expand, and "requires a stable internet connection."
  - Tool guidance: Remove (Content-Aware) for "smaller, whole objects", Heal for dust or blemishes, Clone for copying, and Generative Remove for "large objects or distractions".
  — [Adobe HelpX: Generative Remove FAQ](https://helpx.adobe.com/lightroom/desktop/using/generative-remove-faq.html) (June 2024)
- Search-engine summaries attribute to that FAQ that "Generative Remove outputs are a maximum of 2048px x 2048px", advice to "include shadows", and a quality toggle between reduced and higher resolution. This could not be verified on the rendered page because those tab sections did not load — [search result for Adobe FAQ](https://helpx.adobe.com/lightroom-cc/using/generative-remove-faq.html). Users separately report low-resolution fills — [Lightroom Queen forum](https://www.lightroomqueen.com/community/threads/happy-with-ai-generative-remove-but-just-discovered-it-is-low-res-ways-to-fix.52400/).

**Distraction Removal**
- People: Remove, then Distraction Removal, then People. "Lightroom Classic will automatically detect people in the background and create a mask overlay (red by default) with individual pins over them. To exclude someone from the removal, select the pin over them and then select Delete."
  - "Option/Alt key + click on a pin to delete the selection", and "Delete multiple pins by drawing a bounding rectangle using the Option/Alt key."
  - Remove produces Firefly variations, with Generate for three more.
  - Settings copy, paste, sync and work in presets.
  - "Not available on WinARM machines."
  — [Adobe HelpX: Remove distracting people](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/distraction-removal-people.html) (updated 15 May 2026)
- Reflections: one-click Apply, then "automatically detect and remove the reflections". The Amount slider runs from +100 (reflection removed) to 0 (original) to −100 ("only the reflection"). Quality can be Preview, Standard or Best ("Best – This is the full resolution"). An error string "Some AI models failed to download" suggests local models — [Adobe HelpX: Remove reflections](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/distraction-removal-reflections.html) (updated 16 April 2026).
- The Distraction Removal panel has Reflections, People and Dust. Wires and cables removal is in Photoshop, not Lightroom or Camera Raw — [Digital Camera World (secondary)](https://www.digitalcameraworld.com/photography/photo-editing/cheat-sheet-lightroom-classics-distraction-removal-tools-at-a-glance).

**Lightroom Classic AI masks**
- The mask types are:
  - Subject, Sky and Background (one click).
  - Landscape: "Sky, Snow, Architecture, Vegetation, Water, Natural Ground, Artificial Ground, and Mountains", selectable as one or separate masks.
  - Objects: "Brush Select: Roughly brush over the object" or "Rectangle Select: Make a box".
  - People: auto-detects everyone. You pick a person and then parts "like Facial Hair, Clothes, and more".
- Masks support Add, Subtract and Intersect.
- "Update AI Masks" recomputes when needed, and AI masks can be batch-applied (recomputed per photo) since Classic 11.4 (June 2022).
— [Adobe HelpX: Lightroom Classic Masking](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/masking.html) (updated 7 August 2026)

**Photoshop**
- Remove tool:
  - Set the brush slightly larger than the object and "Draw a loop around the area".
  - Contextual Task Bar: "Find distractions" (General distractions, People, Wires and cables), "Add to", "Subtract from", "Remove after each stroke", "Create new layer".
  - Mode: "Auto (May use generative AI)", "Generative AI is on" (Cloud or Device), or "Generative AI is off".
  - "The on-device model for the Remove tool runs locally on your device after it has been downloaded."
  — [Adobe HelpX: Remove objects in Photoshop](https://helpx.adobe.com/photoshop/desktop/repair-retouch/remove-objects-fill-space/remove-unwanted-objects-and-distractions.html) (updated 18 August 2026)
- On-device generative Remove minimums (Windows table): NVIDIA RTX 30-series or later with 14 GB+ VRAM; Intel Arc with 12 GB+ VRAM; Intel Arrow Lake or Lunar Lake integrated graphics with 32 GB RAM; AMD RDNA2 or newer with 23 GB VRAM (discrete) or 64 GB RAM (integrated) — [Adobe HelpX: Remove tool hardware requirements](https://helpx.adobe.com/photoshop/desktop/repair-retouch/remove-objects-fill-space/remove-tool-minimum-and-recommended-hardware-requirements.html).
- Find distractions: Wires and cables means "wait while Photoshop automatically removes the wires". People and General distractions mean "wait while Photoshop detects…", then "Review and refine the detected areas by brushing on-screen" and commit with the checkmark — [Adobe HelpX: Remove wires, people, and distractions](https://helpx.adobe.com/photoshop/desktop/repair-retouch/remove-objects-fill-space/remove-wires-people-distractions.html). Detected distractions show a pink overlay — [Photoshop Essentials / search summary (secondary)](https://www.photoshopessentials.com/photo-editing/remove-wires-and-cables-from-photos-instantly-in-photoshop-2025/).
- Object Selection tool: "Hover over the image to automatically highlight the object or region you want to select." Object finder can be disabled to "manually define a region using either Rectangle or Lasso". Add and Subtract also work "by hovering over or drawing around them" — [Adobe HelpX: Use the Object Selection tool](https://helpx.adobe.com/photoshop/desktop/make-selections/get-started-selections/select-objects-with-object-selection-tool.html) (February 2026).
- Object Finder shows a spinning refresh icon while it analyses the image — [Photoshop Essentials (secondary)](https://www.photoshopessentials.com/basics/using-the-object-selection-tool-and-object-finder-in-photoshop-2022/).
- Select people: "Select Entire person or their specific attributes like hair, eyes, and clothes… You can also select details directly on the canvas when hovering over with a mouse, or from a labelled list" — [Adobe HelpX: Object Selection tool (people)](https://helpx.adobe.com/photoshop/using/tool-techniques/object-selection-tool.html).

### Inferences
- The Lightroom Classic "Detect objects" pattern is the closest precedent for Luxforge's brush-to-object UX. The user brushes loosely, the system snaps to object plus shadow plus reflection, the user refines with Add and Subtract brushes, and only then removes. Shadow inclusion is part of detection, not left to the inpainter.
- Adobe keeps non-generative on-device Remove as the offline default and puts generative, cloud or GPU-heavy removal behind an explicit switch. This mirrors a "native or portable fallback" tiering, and Photoshop's Cloud versus Device choice is a direct precedent for an explicit provider choice recorded per edit.
- Distraction Removal's per-person pins are a good pattern for "several candidates found, let the user deselect some".

### Gaps
- Adobe does not document whether "Detect objects" segmentation runs on device or in the cloud. The fact that Content-Aware Remove works offline suggests on-device, but this is unverified.
- The Lightroom desktop and mobile variants of Detect objects and the "Objects" mask hover behaviour were not separately read (HelpX blocks non-browser fetches).
- The Photoshop hardware-requirement table for macOS on-device Remove was not captured. Only the Windows table text rendered.

## 6. Other editors: Apple Photos, Google, Samsung, Capture One, Luminar Neo, ON1, DxO, Pixelmator Pro, Affinity, Photopea, darktable, GIMP, Krita, IOPaint and digiKam

### Takeaway
Consumer tools (Apple Photos, Google Photos, Samsung) combine auto-highlighted distraction suggestions with tap, circle or brush. Samsung alone exposes explicit "erase shadows" and "erase reflections" actions.

Pro raw editors split in three ways:
- **Strong AI masking but classical removal:** DxO PhotoLab 9, Capture One.
- **Cloud generative removal:** Luminar GenErase, Photopea, Affinity via Canva AI.
- **Local or cloud choice:** ON1.

Among open-source editors:
- darktable 5.6 (June 2026) ships interactive SAM 2.1 or SegNext object masks via ONNX Runtime with per-platform providers, off by default, with GPL-compatible models documented in a GPL-3.0 model repo. It has no AI inpainting yet.
- IOPaint (Apache-2.0) is the reference open implementation of "SAM click, then LaMa or MI-GAN erase".
- GIMP 3 and Krita rely on plugins with Stable Diffusion inpainting.

### Cited Findings
**Apple Photos Clean Up (macOS)**
- Click Edit, then Tools, then Clean Up. "After you click Clean Up, some items may be highlighted automatically so you can quickly click to remove them." You can also drag a Size slider and "click, brush, or circle what you want to remove". "If you brush over a person's face, the face may become blurred with a pixelated effect." — [Apple Support: Remove distractions on Mac](https://support.apple.com/guide/photos/remove-distractions-and-imperfections-pht5c38b77c5/mac).
- Shadows and reflections: secondary sources report that Clean Up "detects and removes an object's shadow or reflection automatically in many cases", with imperfect results. Apple's page does not address it — [search summary of how-to articles, e.g. Yahoo Tech](https://tech.yahoo.com/ai/apple-intelligence/articles/apples-clean-tool-remove-unwanted-140106031.html).

**Google Photos Magic Eraser**
- A suggestion chip appears when distractions are detected. "After tapping a Magic Eraser suggestion, you can also use the circle or brush to erase more distractions". "Camouflage" recolours instead of removing — [Google Pixel Camera Help](https://support.google.com/pixelcamera/answer/9940184?hl=en).
- Magic Eraser launched on Pixel 6's Tensor chip as on-device ML. Magic Editor (generative) needs the image in the cloud — [Android Police (secondary)](https://www.androidpolice.com/i-found-a-way-save-magic-eraser-from-google-ai-slop-using-settings/).

**Samsung Object Eraser**
- "Tap objects in the image to erase", then Erase. Separate Shadow Erase and Light Reflection Erase "automatically distinguishes between shadows that need to be erased and shadows that need to be left behind" — [Samsung CA support](https://www.samsung.com/ca/support/mobile-devices/how-to-use-the-galaxy-s23-object-eraser/).
- Automatic shadow and reflection erasing arrived with the Galaxy S22 — [Android Central](https://www.androidcentral.com/samsung-object-eraser-can-now-automatically-erase-shadows-and-reflections).

**Capture One**
- 16.3.0 added AI Masking (Subject, Background, AI Select, AI Eraser). "AI Eraser" here is a mask-refinement eraser, the counterpart of the Magic Eraser for masks. 16.4.0 moved AI masking to the GPU when capable, and 16.5.0 added People masking — [Capture One support: AI Masking](https://support.captureone.com/hc/en-us/articles/14055231933853-AI-Masking); [Capture One support: Magic Eraser](https://support.captureone.com/hc/en-us/articles/5363647129757-Magic-Eraser).
- 16.6 (May 2025) added local AI face retouching (Blemish Removal, Even Skin and others): "all the retouching is done locally" — [AlexOnRAW](https://alexonraw.com/capture-one-ai-retouching-all-what-you-need-to-know/).

**Luminar Neo GenErase**
- Cloud only: "all edits are performed on the cloud", and Skylum states it does not store images — [Skylum Knowledge Hub: GenErase](https://support.skylum.com/catalog-tools/generative-tools/generase); [iPhotography (secondary)](https://www.iphotography.com/blog/how-to-use-luminar-neos-generase/).
- A secondary source says GenErase uses a Stable Diffusion model — [Toolify (secondary, low confidence)](https://www.toolify.ai/ai-news/discover-the-powerful-gen-erase-tool-in-luminar-neo-1240180).

**ON1 Photo RAW**
- The Generative Eraser (Photo RAW 2025) works by "painting over an object you want to remove, press Generate. If the result isn't what you hoped for, simply hit Retry to generate new options". A Stability.ai cloud option requires a Stability account — [ON1 blog, 11 November 2024](https://www.on1.com/blog/free-update-for-on1-photo-raw-2025-available-now-with-improved-generative-eraser/).
- ON1 describes a local mode and "a new non-generative AI model designed for systems with lower-end GPUs… runs directly on your computer" — [ON1 (search summary of on1.com pages)](https://www.on1.com/blog/how-generative-ai-is-revolutionizing-photography-new-tools-in-on1-photo-raw-2025/).
- Perfect Eraser is the older, faster tool for small areas — [ON1 features](https://www.on1.com/products/photo-raw/features/).

**DxO PhotoLab 9**
- AI Mask selection is hover-and-click: "move the mouse pointer over the image and it will highlight different areas with a red overlay – you just click to confirm". Clicking a subject and drawing a bounding box are also supported, and auto categories include sky, people, clothes, background, hair and vehicles — [Life after Photoshop](https://lifeafterphotoshop.com/dxo-photolab-9-ai-masking-tools-explained-with-examples/).
- "PhotoLab has no automatic (AI) object removal", only repair and clone — [DPReview forum (secondary)](https://www.dpreview.com/forums/threads/object-removal-in-dxo-pl9.4830271/).

**Pixelmator Pro**
- ML Select Subject and background removal arrived in 2.3 (November 2021). Masking was "reengineered from the ground up" in 3.6 (May 2024) — [Wikipedia: Pixelmator Pro](https://en.wikipedia.org/wiki/Pixelmator_Pro); [MacRumors](https://www.macrumors.com/2024/05/23/pixelmator-ai-background-removal-tool/).

**Affinity by Canva**
- Relaunched free on 30 October 2025. Canva AI Studio inside Affinity (Generative Fill and object removal, Select Subject, Remove Background) requires a Canva premium account and is optional — [Canva newsroom](https://www.canva.com/newsroom/news/all-new-affinity/); [Affinity Help: Generative Fill](https://www.affinity.studio/help/tools-tools-generative-fill/).

**Photopea**
- "Magic Replace" (May 2023) is prompt-based generative fill with premium credits for heavy use — [AI Trace summary](https://www.aitrace.org/company/photopea/practice/e5ab6701-fd7f-4711-93aa-f5f2a67b447b).
- Users report "Remove" often inserts random objects — [photopea/photopea issue #6079](https://github.com/photopea/photopea/issues/6079).

**darktable 5.6 (June 2026)**
- AI object mask interaction and cost:
  - Click to segment, plain click adds positive points, Shift-click adds negative points, and right-click commits the mask to a vector path, editable like drawn masks.
  - "First click on a new image triggers encoder pass: 1 to 10 seconds depending on hardware". The encoder result is cached per image, so refinement clicks are interactive.
  - "Refine boundary" is an optional second pass costing 1–2 s.
  - Smoothing ranges from 0 to 1.3.
- Providers are CoreML, CUDA, ROCm, DirectML, OpenVINO and CPU.
- The subsystem is optional at build time (`USE_AI`) and "Off by Default". Models are not bundled and are downloaded on demand. "Nothing leaves your machine."
— [darktable blog](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/); [darktable 5.6.0 release](https://www.darktable.org/2026/06/darktable-5.6.0-released/)
- `darktable-org/darktable-ai` (GPL-3.0) packages ONNX models:
  - `mask-object-sam21-{tiny,small,base-plus}` and `mask-object-segnext-b2hq`.
  - Denoise (NIND, NAFNet), raw denoise, upscale (BSRGAN, RealPLKSR), and an OpenCLIP embedding.
  - Selection criteria: a "GPL-3.0-compatible license" for weights, documented training-data provenance and licence, public training code, and "We do not include models designed for generating, manipulating, or synthesizing human likenesses".
  - Purposes listed include "object removal (inpainting)", but no inpainting model is packaged.
  — [darktable-ai README](https://github.com/darktable-org/darktable-ai)

**GIMP 3**
- There is no built-in AI removal. Intel's `openvino-ai-plugins-gimp` (Apache-2.0, "Dedicated for GIMP 3") offers Semantic Segmentation and SD 1.5 / SDXL inpainting, and notes "Stable Diffusion's data model is governed by the Creative ML Open Rail M license, which is not an open source license." — [intel/openvino-ai-plugins-gimp](https://github.com/intel/openvino-ai-plugins-gimp).

**Krita AI Diffusion**
- GPL-3.0, about 10.6k stars. It uses a ComfyUI backend, and "Inpainting: Use selections for generative fill, expand, to add or remove objects". AI segmentation for selections comes from a separate optional plugin — [Acly/krita-ai-diffusion](https://github.com/Acly/krita-ai-diffusion).

**IOPaint (formerly lama-cleaner)**
- Apache-2.0, about 23k stars, last push 29 April 2025.
- Erase models: "LaMa, MAT, MIGAN" (recommended; MI-GAN is "Minimal in size"), plus LDM, ZITS, FcF and Manga. SD, SDXL, BrushNet and PowerPaint diffusion inpainting are used for replacement.
- Plugins include interactive Segment Anything (`--enable-interactive-seg`, SAM2 tiny/small/base/large and SAM-HQ) and RemoveBG.
- The same author ships "OptiClean", a macOS and iOS object-erase app.
— [Sanster/IOPaint](https://github.com/Sanster/IOPaint); [IOPaint models](https://www.iopaint.com/models); [IOPaint Interactive Seg](https://www.iopaint.com/plugins/interactive_seg)

**digiKam**
- The image editor has a Healing Clone tool and a legacy CImg-based inpainting tool (reported as affected by CImg regressions), with G'MIC-Qt as the route to more advanced filters. No AI removal was found — [digiKam manual: Enhancement tools](https://docs.digikam.org/en/image_editor/enhancement_tools.html).

### Inferences
- The raw-editor peers closest to Luxforge (darktable, DxO, Capture One) all invested in AI masking before AI removal. darktable's design choices are directly reusable for Luxforge's portable tier: ONNX interface contracts per task, off by default, models downloaded on demand, a GPL-compatible model policy, a cached encoder with an interactive decoder, and masks committed as editable data.
- darktable-ai's licence policy would exclude common SD-based inpainting (CreativeML OpenRAIL-M) and non-commercial weights. For a GPL-3.0-or-later Luxforge, LaMa, MI-GAN and MAT-class models (licences to be checked by the model researcher) and SAM 2.x (Apache-2.0) fit better.
- Only Samsung and Adobe (Detect objects) treat shadows and reflections as first-class selection targets. Everyone else relies on the user brushing them, or on the fill model being trained for it.

### Gaps
- Capture One desktop: no evidence found of AI object removal, generative or otherwise, beyond Heal and Clone layers. A Capture One Mobile "AI Erase" was seen only in a search title ([AlexOnRAW](https://alexonraw.com/capture-one-mobile-2-7-ai-masks-auto-rotate-and-auto-keystone/i6_5_l/)) and was not verified.
- Pixelmator Pro's current Remove or Repair tool (ML-based?) and its status after Apple's acquisition were not verified.
- Apple Photos Clean Up's model and its shadow handling are not documented by Apple.
- ON1's local model architecture and hardware requirements were not found.

## 7. UX learnings: instant hover highlighting, part versus whole ambiguity, offering candidates or variations, and shadow handling

### Takeaway
Instant hover requires precomputation: either a per-image instance label map (Vision foreground instances, a detector) or a cached image embedding with a millisecond-scale prompt decoder (the SAM family; darktable caches the encoder output).

Ambiguity is resolved in three ways:
- Multiple nested candidate masks (SAM's whole, part and subpart, ranked by predicted IoU).
- Add and exclude points or Add and Subtract brushes (Vision 27, the Windows extractor, darktable, Adobe).
- Separate part pickers (Adobe People parts).

Variations for generative fills come as 3 per batch with a "Generate" or "Retry" for more (Adobe, ON1). Shadows are best handled at selection time: Adobe's Detect objects auto-includes shadow and reflection, and Samsung has explicit shadow and reflection erase. Generic inpainters do not remove effects they are not masked on.

### Cited Findings
**Hover and latency**
- SAM: "Given an image embedding, the prompt encoder and mask decoder predict a mask from a prompt in ∼50ms in a web browser". The image encoder runs once per image — [Kirillov et al., Segment Anything (ar5iv)](https://ar5iv.labs.arxiv.org/html/2304.02643).
- darktable's first click costs 1–10 s (the encoder). After that, clicks are interactive because the encoder result is cached per image — [darktable blog](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/).
- Photoshop's Object Selection highlights on hover. DxO PhotoLab 9 highlights hovered areas with a red overlay and clicks confirm — [Adobe HelpX](https://helpx.adobe.com/photoshop/desktop/make-selections/get-started-selections/select-objects-with-object-selection-tool.html); [Life after Photoshop](https://lifeafterphotoshop.com/dxo-photolab-9-ai-masking-tools-explained-with-examples/).
- Vision's instance mask is a label image computed once, which OpenStill queries at a point in O(1) — [OpenStill VisionEditor.swift L20–26](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/Sources/OpenStillCore/VisionEditor.swift#L20-L26).

**Ambiguity and refinement**
- SAM: "we design it to predict multiple masks for a single prompt… three masks… (whole, part, and subpart)… the model predicts a confidence score (i.e., estimated IoU) for each mask" — [Segment Anything](https://ar5iv.labs.arxiv.org/html/2304.02643).
- Refinement points by platform:
  - Vision 27 accepts included and excluded points, up to 13 or 11 in total — [Apple docs](https://developer.apple.com/documentation/vision/generateiterativesegmentationrequest).
  - The Windows extractor accepts include and exclude points and a rectangle, up to 32 coordinates — [Microsoft Learn](https://learn.microsoft.com/en-us/windows/ai/apis/image-object-extractor).
  - darktable uses click for positive and Shift-click for negative — [darktable](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/).
- Adobe offers Add and Subtract brushes after Detect objects, and in Photoshop both work by hovering — [Adobe HelpX LrC Remove](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/remove-tool.html); [Adobe HelpX Object Selection](https://helpx.adobe.com/photoshop/desktop/make-selections/get-started-selections/select-objects-with-object-selection-tool.html).
- Part pickers: Photoshop "Select people" offers Entire person or hair, eyes and clothes from the canvas or a labelled list. Lightroom Classic's People mask offers body parts, and its Objects mask offers Brush Select or Rectangle Select — [Adobe HelpX](https://helpx.adobe.com/photoshop/using/tool-techniques/object-selection-tool.html); [Adobe HelpX Masking](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/masking.html).

**Multiple detections and candidates**
- Lightroom Distraction Removal (People) puts a pin on each detected person and supports Alt-click or Alt-rectangle to deselect — [Adobe HelpX](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/distraction-removal-people.html).
- Photoshop Find distractions produces reviewable detected areas that you then brush to refine — [Adobe HelpX](https://helpx.adobe.com/photoshop/desktop/repair-retouch/remove-objects-fill-space/remove-wires-people-distractions.html).
- Apple Clean Up auto-highlights items to click — [Apple Support](https://support.apple.com/guide/photos/remove-distractions-and-imperfections-pht5c38b77c5/mac).
- Google shows a suggestion chip — [Google Help](https://support.google.com/pixelcamera/answer/9940184?hl=en).

**Variations**
- Lightroom Classic generates three variations with arrows to cycle, "Generate" for three new ones, and Delete or Report per variation — [Adobe HelpX](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/remove-tool.html).
- ON1 has "Retry to generate new options" — [ON1](https://www.on1.com/blog/free-update-for-on1-photo-raw-2025-available-now-with-improved-generative-eraser/).

**Shadows and reflections**
- Lightroom Classic: with Detect objects, "Lightroom Classic will also identify the shadow and reflections (if any) around the object and include it in the selection". Firefly "removes the object and the shadow around it" — [Adobe HelpX](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/remove-tool.html).
- Samsung has explicit Shadow Erase and Reflection Erase that decide which shadows to keep — [Samsung](https://www.samsung.com/ca/support/mobile-devices/how-to-use-the-galaxy-s23-object-eraser/).
- OpenStill tells users to "Cover the object including its edges" and does nothing for shadows — [OpenStill README L453](https://github.com/haon-v2/OpenStill/blob/99f96974912fd0700f2fa957023ff4636301f7c8/README.md#L453).
- Research: diffusion editors "struggle with occlusions, shadows, and reflections". Fine-tuning on counterfactual before-and-after pairs makes models "not only remove objects but also their effects on the scene" — [ObjectDrop (Winter et al., 2024)](https://arxiv.org/abs/2403.18818).

### Inferences
- The hover path should use the best cheap signal available, and the hover state stays a UI-only preview, never an edit:

  | Condition | Hover source |
  | --- | --- |
  | macOS 14+ | Precomputed Vision foreground-instance label map, a per-pixel lookup |
  | Portable, or objects Vision misses | Cached SAM-class image embedding, where each hover runs only the decoder (tens of ms on GPU or ANE; to be measured) |

  Hover requests should be throttled and stale ones dropped, in line with Luxforge's "cancel stale work" rule.
- For the brush gesture, treat the stroke as a scribble or seed plus sampled include points:
  - Vision 27 accepts a scribble buffer directly.
  - SAM accepts points or a box derived from the stroke.
  - The Windows extractor accepts points and a rectangle.

  When the result is ambiguous (several instances intersect the stroke, or the whole, part and subpart candidates differ materially), show the candidates highlighted and let a click choose. This matches the owner's "remove one automatically, or highlight several for the user to click" UX.
- Shadows need an explicit step, because the inpainter only fills what is masked. Two options:
  - Expand the selection to include shadow and reflection, like Adobe's Detect objects. This could use a shadow-detection or instance-shadow model, or a heuristic search below and beside the object. It needs model research.
  - Use a removal model trained on counterfactual data (ObjectDrop-style) that removes effects outside a tight mask.

  Either way, the selected shadow region should be visible and editable (Add and Subtract) before removal.
- Record the provider (for example Vision rev1, Windows extractor or SAM 2.1-small), the seeds and the model version with each removal layer. Native and portable segmenters produce different masks, and Luxforge's pillars require reproducible edits and explicit failure when a provider is missing.

### Gaps
- No measured hover latency for Vision instance lookup on an M4, for SAM 2.1 decoder latency under Core ML or ONNX Runtime on an M4, or for the Windows extractor.
- No primary documentation of how Apple Photos or Google choose which "distractions" to auto-highlight. Adobe does not describe its detection model either.
- No published shadow-detection approach from Adobe or Samsung. Their selection-time shadow inclusion is documented only as behaviour.
