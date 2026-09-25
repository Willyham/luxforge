# Optional AI and derived images

[Knowledge base index](README.md) · Source baseline: 5.6.1. Availability depends on the installed build and model assets.

## Build support, model availability and execution are different questions

**S.** `USE_AI` is an optional build switch whose source default is off. The presence of code in this release therefore does not prove that every packaged binary exposes these features. Runtime configuration, inference-provider availability and separately installed models also matter. [Build feature switches](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/DefineOptions.cmake#L26) [AI architecture guide](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/AI.md#L1)

**S.** The ONNX backend has a CoreML provider path on macOS, including compute-unit configuration, alongside other platform/provider paths and CPU execution. This is a separate system from the ordinary OpenCL pixelpipe. “CoreML enabled” does not establish that every model node ran on the Neural Engine, nor that darktable's normal image operations are native Metal kernels. [ONNX/CoreML backend](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/ai/backend_onnx.c#L1714)

**U.** No model was downloaded, no AI-enabled binary was tested, and no M4 latency, memory or quality result was measured. Model weights/configuration have their own identity and terms; the application commit does not pin every externally supplied model artifact.

## Object masks: cache the image encoding, refine the selection

**D.** The developer task contract separates an image encoder from a prompt-driven decoder. An sRGB rendering is resized/padded and normalized for the chosen model; the encoder produces reusable embeddings. Foreground/background clicks and previous mask feedback then drive cheaper repeated decoder evaluations. Candidate selection and resizing turn model logits into a usable mask. [AI task contracts](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/AI_Tasks.md#L1)

**S.** The segmentation code explicitly distinguishes resetting previous-mask feedback from discarding the image encoding. This matters for performance and invalidation: another click can reuse the representation, whereas an image/context change may require a new encoding. The mask UI also records distortion-related state for its cache. [Object segmentation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/ai/segmentation.c#L1138) [AI mask persistence](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/masks/object.c#L94)

**S.** Finalizing the current object-mask tool vectorizes the inferred raster into ordinary path forms, assembles groups and represents holes with difference operations. The resulting paths become part of the normal mask/history machinery. There is also a separate option to write a raster-mask PNG into a configured folder. Do not describe every AI mask as an opaque neural blob embedded in XMP, or every segmentation cache as the durable edit. [AI mask finalization](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/masks/object.c#L1093) [Mask host](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/develop/masks/masks.c#L30)

**C.** This is a useful distinction: inference can author an editable geometric artifact. Subsequent rendering can use that artifact without conceptually re-running the model on every slider change. Re-running inference after upstream edits is a different operation from rendering the accepted mask; exact UX behavior still needs runtime verification.

## RAW denoise has sensor-specific input contracts

**S/D.** The implementation and developer contract distinguish Bayer and linear RAW paths. They share a restoration feature but consume different data and create different DNG representations. [Bayer AI preparation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/ai/restore_raw_bayer.c#L291) [Linear RAW AI preparation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/ai/restore_raw_linear.c#L246) [AI task contracts](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/AI_Tasks.md#L1)

| Path | Data sent to inference | Result and important limitation |
| --- | --- | --- |
| Bayer | Black/range/white-balance-normalized CFA packed into four half-resolution planes: R, G1, G2, B | Model output is full-resolution RGB; postprocessing reverses normalization and re-mosaics to a Bayer DNG |
| Linear RAW | A minimal sensor-aware pipeline first creates demosaicked camera RGB, then transforms/normalizes it for the model | Output is a three-channel floating-point LinearRaw DNG; it is already demosaicked |

**S.** The linear path reuses darktable's RAW preparation/highlight/demosaic stages with temperature handling controlled for this purpose. Matrix, white-balance and exposure normalization are applied around inference, with gain matching to account for the model's expected distribution. Omitting these transforms while calling the same neural network would not reproduce the application result. [Linear RAW AI preparation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/ai/restore_raw_linear.c#L246)

**D.** At this baseline, the X-Trans loader routes to the linear variant rather than proving a dedicated X-Trans model exists. **C.** This is particularly relevant to the Fujifilm X100VI: “supports RAW denoise” alone is not enough to specify whether mosaiced sensor data survives in the derived file. [AI task contracts](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/AI_Tasks.md#L1)

## RGB denoise and upscale are derived-image workflows

**S/D.** RGB restoration prepares a rendered RGB image, applies the transfer-function/color conversions expected by the model, runs inference and creates a new image file. The integration imports the result, associates it with the source and transfers relevant metadata such as user tags. RAW denoise similarly creates/imports DNG outputs. These are not simply ordinary scalar IOP parameters whose effect is always recomputed from the original inside every display pipe. [RGB AI preparation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/ai/restore_rgb.c#L79) [Neural restore integration](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/libs/neural_restore.c#L998) [AI task contracts](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/AI_Tasks.md#L1)

**S.** Model preparation and output handling are part of the algorithm. A model trained for nonlinear sRGB should not be silently fed unbounded camera RGB; doing so changes the statistical and color meaning of its input. The actual preparation branches must accompany any standalone model benchmark. [RGB AI preparation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/ai/restore_rgb.c#L79) [Linear RAW AI preparation](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/src/common/ai/restore_raw_linear.c#L246)

**D.** The AI architecture describes tiled execution with overlap/padding and memory-aware sizing. Some output paths stream results rather than requiring the whole enlarged image in one extra buffer. **C.** Tiling can bound working memory, but model activations, overlap, session compilation and final file creation still cost memory/time. An advertised tile size alone is not an end-to-end memory guarantee. [AI architecture guide](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/AI.md#L1) [AI task contracts](https://github.com/darktable-org/darktable/blob/03179f8e080aa9cedebfe14b098b7ba88940a292/dev-doc/AI_Tasks.md#L1)

## Recovery and provenance implications

**P.** A Luxforge design should distinguish an editable recipe, an accepted generated mask, a model artifact, a disposable embedding cache and a newly generated source image. Record model/configuration identity where reproducibility requires it, and preserve generated files when later edits reference them. Define cancellation and partial-file cleanup before exposing an asynchronous job.

AI tools remain future work under the existing roadmap. No model, provider or storage design is selected by this research. A full dependency/model license review remains deferred under the owner's existing instruction.
