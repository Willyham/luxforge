# RAW and computational processing

[Knowledge base index](README.md) · Evidence checked 2026-09-20.

## Decoding is only part of RAW development

**C.** A RAW workflow must interpret sensor samples, black/white levels, channel layout, color calibration and white balance, and produce a useful rendered image. Container decompression alone does not specify demosaicing, highlight reconstruction, color appearance or detail processing. An embedded JPEG can be displayed without proving that any RAW development has occurred.

**D.** Adobe documents that camera-generated previews can initially look different from its subsequent rendition. Camera Matching profiles approximate the camera's rendering intent. [S15: Color FAQ](https://helpx.adobe.com/lightroom-classic/desktop/technical-support/technical-issues/miscellaneous-issues/color-faq.html) **P.** For Luxforge's Nikon Z6 and Fujifilm X100VI goals, keep decoder support, exact recording-mode support, baseline color quality and preview latency as separate acceptance questions; follow the [existing decoder research](../technical-options.md#image-processing-and-raw).

## Raw Details and Super Resolution

**D.** Raw Details targets Bayer and X-Trans mosaic input, improving reconstructed detail/color edges. Super Resolution doubles each dimension, quadrupling pixel count, and also supports rendered formats such as JPEG/TIFF. These are different operations despite sharing the Enhance family. Eligibility differs by format; a Smart Preview is not interchangeable with original mosaic data for Raw Details. [S25: Enhance image quality](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/enhance-details.html)

**H.** Eric Chan explains Super Resolution as a convolutional model trained on paired low-/high-resolution patches. Enhance Details performs learned reconstruction of mosaic sensor data into RGB. Their joint application to appropriate RAW input was designed to improve both quality and performance. This is a published family-level explanation; architecture, weights and contemporary model versions are not supplied. [S26: Super Resolution](https://blog.adobe.com/en/publish/2021/03/10/from-the-acr-team-super-resolution)

**C.** More pixels are not new camera measurements. Evaluate inferred structure on fine text, repeating patterns, foliage and fabric as well as subjective sharpness. Four times the pixels also means roughly four times the storage for a same-format output buffer before temporary working memory is considered.

## AI Denoise

**H.** The 2023 engineer's account says Denoise jointly demosaics and denoises RAW data with a deep convolutional network. Training used noisy/clean image-patch pairs, simulated noise, augmentation and dark frames for pattern noise. Raw Details is incorporated into that workflow. At launch, it created a derived DNG and carried over existing adjustments. [S24: Denoise Demystified](https://blog.adobe.com/en/publish/2023/04/18/denoise-demystified)

**D.** Later documentation integrates Denoise/Raw Details/Super Resolution into the Detail panel and describes broader Denoise input support and background batch execution. Avoid carrying the original Bayer/X-Trans-only limitation forward as a timeless statement. [S25: Enhance image quality](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/enhance-details.html) [S36: June 2025 feature summary](https://helpx.adobe.com/lightroom-classic/desktop/help/whats-new/2025-4.html)

**U.** Do not infer that the Denoise Amount slider is simple alpha blending, that inference runs for every tiny slider movement, or that all current formats use the same joint sensor-stage network. The sources do not specify these details.

**C.** “Non-destructive” can include expensive computed image assets in addition to scalar parameters. Reproducible rendering may require those assets, their identity and their creating model version. Re-running a future model is not guaranteed to reproduce an old result. That is why [auxiliary edit storage](storage-and-history.md) matters.

## AI processing is not one execution model

**D.** Generative Remove requires an internet connection; Adobe explicitly distinguishes offline Remove/Heal/Clone. [S30: Remove tool](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/remove-tool.html) The existence of one network-backed feature does not establish that all AI editing uploads photographs.

**H.** Adobe's reflection-removal engineering account describes separating a transmitted scene from reflected content. It works from unadjusted uncropped input and cannot reconstruct fully clipped observations reliably. Classic's June 2025 release introduced reflection removal. [S38: Removing window reflections](https://blog.adobe.com/en/publish/2024/12/12/removing-window-reflections-adobe-camera-raw) [S36: June 2025 feature summary](https://helpx.adobe.com/lightroom-classic/desktop/help/whats-new/2025-4.html)

**D.** Lens Blur produces or uses depth information to choose focus versus blur. [Geometry/masks chapter](geometry-masks-and-retouching.md#depth-and-lens-blur) explains its different data dependency.

## Derived images, HDR and rendered DNG

**D.** HDR merge aligns bracketed frames, can suppress moving-object ghosts, and creates a DNG result. A subsequent edit of that result is different from independently editing its source bracket frames. [S33: HDR photo merge](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/hdr-photo-merge.html)

**D.** Classic 15.5 adds Render to DNG: the current edit is baked into a new non-raw DNG. Its presence is a reminder that the `.dng` extension alone does not mean “untouched mosaic sensor data.” [S35: Lightroom Classic release notes](https://helpx.adobe.com/lightroom-classic/desktop/introduction-to-lightroom-classic/release-notes.html)

**P.** Represent source-preserving edit recipes, derived computational sources and flattened exports distinctly in Luxforge. Do not automatically include AI, panorama or HDR merge in current milestones. Their data, quality and API contracts require their own scope.
