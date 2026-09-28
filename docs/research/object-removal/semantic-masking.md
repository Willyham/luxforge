# Semantic auto-masking models for Luxforge, and what they can share with object removal

Scope: open models for Subject, Sky, Background, People (and body or face parts), Objects and Landscape masks. For each one: quality, speed, native resolution, edge quality, and CODE and WEIGHTS licences stated separately. Then the architecture for sharing models, runtime, cached image embeddings and the object picker with the SAM-style object-removal tool. Current as of 27 September 2026. The labels mean:

- **Estimate**: my own calculation, not a measurement.
- **Inference**: a conclusion drawn from the cited facts.
- **Not verified**: a claim I could not confirm from a primary source in this pass.

Target context: GPL-3.0-or-later Rust app. Weights would be a user-initiated download ("resource"), inference runs locally, and the output is an 8-bit alpha artifact whose long side is capped at 4096 px.

## Q1. Subject, salient object and background: which model is best at 1024–2048 px, and how fast is it?

### Takeaway
Only the BiRefNet family combines top-tier quality with MIT-licensed code *and* weights. BiRefNet_HR at 2048² is the best openly licensed option: DIS-VD maxFβ .925 against .908 for BiRefNet at 1024². BEN2's base model (MIT) scores slightly higher in its own paper's table, but part of its training data is proprietary. RMBG-1.4 and RMBG-2.0 are non-commercial (CC BY-NC) and should be excluded. A caveat applies to every DIS-family model, whatever its weights licence: they are trained on DIS5K, whose terms are non-commercial. No primary source gives speeds on Apple Silicon.

### Cited findings

**BiRefNet (family)**
- Code and weights are both MIT: "MIT license applies to the code and weights in this repository." — [BiRefNet GitHub](https://github.com/ZhengPeng7/BiRefNet)
- Variants:
  - general BiRefNet
  - BiRefNet_lite (Swin-v1-Tiny backbone)
  - BiRefNet_HR (trained at 2048×2048)
  - BiRefNet_dynamic (dynamic resolution 256–2304)
  - BiRefNet-matting and BiRefNet_HR-matting (trimap-free matting, the HR one at 2048)
  - BiRefNet_lite-2K (2560×1440 input)

  The original paper models use a Swin-v1-Large backbone. — [BiRefNet GitHub](https://github.com/ZhengPeng7/BiRefNet)
- Speed at 1024×1024 on one A100: FP32 86.8 ms (~11.5 FPS), FP16 69.4 ms (~14.4 FPS). On an RTX 4090: 17 FPS in FP16 using 3.45 GB. ONNX export is supported (opset 22 recommended); third-party TensorRT builds are ~26% faster than ONNX. — [BiRefNet GitHub](https://github.com/ZhengPeng7/BiRefNet)
- Training data for the general models: DIS5K, DUTS, HRSOD, UHRSD, P3M-10k and custom human-segmentation data. — [BiRefNet GitHub](https://github.com/ZhengPeng7/BiRefNet)
- Repo-reported scores for the general model: DIS5K-VD S=0.911, wF=0.875. BiRefNet-matting on P3M-500-NP: S=0.979. — [BiRefNet GitHub](https://github.com/ZhengPeng7/BiRefNet)
- BiRefNet_HR at 2048² against BiRefNet at 1024² on DIS-VD:

  | Metric | BiRefNet_HR (2048²) | BiRefNet (1024²) |
  |---|---|---|
  | maxFm | .925 | .908 |
  | wFmeasure | .894 | .877 |
  | MAE | .026 | .034 |
  | Smeasure | .927 | .912 |

  Licence is MIT. The card gives no timing or memory figures at 2048. — [BiRefNet_HR model card](https://huggingface.co/ZhengPeng7/BiRefNet_HR)
- A community write-up says BiRefNet-HR "appears to do much better with hair" than other models. This is anecdotal. — [ComfyUI-RMBG / Medium summary via search](https://github.com/1038lab/ComfyUI-RMBG)

**Comparative table on DIS5K validation (DIS-VD), from the BEN paper**

| Method | Fβmax | Fβω | Eφm | Sm | MAE |
|---|---|---|---|---|---|
| BEN base + refiner | 0.919 | 0.896 | 0.959 | 0.917 | 0.027 |
| DiffDIS | 0.918 | 0.888 | 0.948 | 0.904 | 0.029 |
| MVANet | 0.913 | 0.856 | 0.938 | 0.905 | 0.036 |
| BiRefNet | 0.897 | 0.863 | 0.937 | 0.905 | 0.036 |
| InSPyReNet | 0.889 | 0.834 | 0.914 | 0.900 | 0.042 |

Source: [BEN paper, arXiv 2501.06230](https://arxiv.org/html/2501.06230). The paper gives no speed or resolution figures.

Note the conflict: the BEN paper lists BiRefNet at Fβmax 0.897 and Sm 0.905, while BiRefNet's own card lists the 1024 general model at maxFm .908 and Sm .912 ([BiRefNet_HR card](https://huggingface.co/ZhengPeng7/BiRefNet_HR)). The two sources probably used different checkpoints (DIS-only paper model versus the general model). They are not directly comparable.

**Other subject and background models**
- **BEN2**:
  - Licence: base model MIT; a separate commercial "full" model is available by contacting the vendor.
  - Size and formats: 94.6M parameters; Safetensors, PyTorch and ONNX.
  - Method: a Confidence Guided Matting (CGM) refiner reprocesses the pixels where the base model has low confidence. Claimed strengths are hair matting and 4K images.
  - Training data: DIS5k plus a proprietary 22K-image segmentation set.

  — [BEN2 model card](https://huggingface.co/PramaLLC/BEN2)
- **RMBG-2.0 (BRIA)**:
  - Weights: CC BY-NC 4.0; commercial use needs a separate agreement with BRIA. Hugging Face login is required (gated).
  - Architecture: BiRefNet ("developed on the BiRefNet architecture enhanced with our proprietary dataset").
  - Training data: more than 15,000 manually labelled, "fully licensed" images.
  - Input: 1024×1024.

  — [RMBG-2.0 card](https://huggingface.co/briaai/RMBG-2.0). rembg also notes that RMBG-2.0 "requires a paid commercial agreement". — [rembg README](https://github.com/danielgatis/rembg)
- **RMBG-1.4 (BRIA)**: licence "bria-rmbg-1.4", a Creative Commons licence for non-commercial use (commercial use is paid). Based on IS-Net, trained on more than 12,000 images, input 1024×1024. — [RMBG-1.4 card](https://huggingface.co/briaai/RMBG-1.4)
- **IS-Net / DIS (isnet-general-use)**: code Apache-2.0; weights under the same Apache-2.0. — [DIS GitHub](https://github.com/xuebinqin/DIS)
- **DIS5K dataset terms**. DIS5K is the training set for BiRefNet, BEN2, MVANet, IS-Net, the InSPyReNet extended model, HR-SegRefiner and the HQSeg-44K-based models. Its terms say:
  - "available for non-commercial use in research or educational purpose"
  - "commercial use of this dataset is prohibited even after copying, editing, processing or any operations of this database"
  - distribution "as it is, or copy, edit, or process" is prohibited.

  Images were collected from Flickr. — [DIS5K Terms of Use PDF](https://raw.githubusercontent.com/xuebinqin/DIS/main/DIS5K-Dataset-Terms-of-Use.pdf)
- **MVANet**: code and weights MIT; trained on DIS5K. — [MVANet GitHub](https://github.com/qianyu-dlut/MVANet)
- **InSPyReNet**:
  - Licence: MIT.
  - Training data: DUTS, HRSOD and UHRSD; an extended version adds DIS5K.
  - Resolution: base 384 px, with "pyramid blending" of a low-resolution and a high-resolution pass for high-resolution inputs. It ships as the `transparent-background` package.

  — [InSPyReNet GitHub](https://github.com/plemeri/InSPyReNet)
- **rembg**: the library is MIT, and "model weights carry their own licenses, independent of rembg's MIT license". Its catalogue covers:
  - u2net, u2netp, u2net_human_seg, u2net_cloth_seg and silueta
  - isnet-general-use and isnet-anime
  - the birefnet variants: general, general-lite, portrait, dis, hrsod, cod and massive
  - bria-rmbg
  - sam (encoder and decoder)

  — [rembg README](https://github.com/danielgatis/rembg)
- **Apple VNGenerateForegroundInstanceMaskRequest**:
  - Availability: iOS 17+, macOS 14+, tvOS 17+, visionOS 1+.
  - Output: class-agnostic instance masks of "noticeable" foreground objects (people, pets, buildings, food, and so on), with each instance getting an ID and 0 reserved for background. Masks match the source resolution, and scaled masks can be generated.
  - Cost: Apple calls it "resource intensive" and says to run it off the main thread.

  — [Apple docs](https://developer.apple.com/documentation/vision/vngenerateforegroundinstancemaskrequest); [WWDC23 10176 notes](https://wwdcnotes.com/documentation/wwdc23-10176-lift-subjects-from-images-in-your-app/)

**Speed evidence on consumer hardware** (none is from an M4)
- vision.cpp (GGUF, CPU and Vulkan):
  - BiRefNet Lite at 1024×1024: 4505 ms on CPU in f32 (Ryzen 5 5600X); 85 ms on an RTX 4070 via Vulkan in f16.
  - MobileSAM at 1024²: 669 ms on CPU, 19 ms on GPU.

  — [vision.cpp README](https://github.com/Acly/vision.cpp)
- A community Core ML port of BiRefNet Lite 1024 says "CPU-only execution is recommended" for both builds. That suggests GPU and Neural Engine execution had problems after conversion. — [birefnet-lite-coreml](https://github.com/miracle2k/birefnet-lite-coreml)
- A Core ML (INT8-quantised) port of RMBG-2.0 exists for Apple devices. — [VincentGOURBIN/RMBG-2-CoreML](https://huggingface.co/VincentGOURBIN/RMBG-2-CoreML)
- One project, FramePilot, picked "BiRefNet 2048²" for its matte worker. It then recorded "BiRefNet training-data licence (MO-11, blocks shipping)" as an unresolved release blocker. This shows another project treating the DIS-type training data as a shipping risk. — [FramePilot PR #124](https://github.com/rojan-labs/FramePilot/pull/124)

### Inferences
- **Best choice at 1024–2048 px under a permissive-weights policy.** BiRefNet_HR (2048²) is the quality pick. BiRefNet general or BiRefNet_dynamic at 1024 is the default. BiRefNet_lite or lite-2K suits a fast preview. MVANet and BEN2-base (both MIT) are the alternatives, but neither has an HR variant or a model zoo as broad as BiRefNet's. RMBG-1.4 and RMBG-2.0 are excluded (non-commercial weights), as are MatAnyone and ZIM (see Q3).
- **Edges.** BiRefNet-matting and BEN2's CGM refiner give a soft alpha on hair. The plain segmentation models give near-binary masks that need feathering or a matting pass (see Q7).
- **M4 speed (estimate, unmeasured).** Scaling the RTX 4090 figure (17 FPS FP16 at 1024²) by the much lower FP16 throughput of an M4-class laptop GPU puts full BiRefNet at roughly 1–3 s per 1024² image on a base M4 GPU, and roughly 4× that at 2048² for BiRefNet_HR. BiRefNet_lite should be several times faster. If Core ML conversion forces CPU-only execution (as one port reports), expect the CPU order of magnitude instead (vision.cpp: 4.5 s for lite on a desktop CPU). Benchmark this before pinning a model.
- **Apple's Vision request** is a useful quality reference on macOS but cannot be the Subject provider:
  - it is macOS-only, which breaks the Windows and Linux parity pillar;
  - the model is closed and changes with OS updates, so the stored artifact is not reproducible "on the same source and model";
  - it cannot be version-pinned by hash.

  It could serve as a comparison oracle during evaluation.
- **Training-data risk.** Every strong open subject model trains on DIS5K, DUTS, HRSOD or P3M-type academic sets with non-commercial terms. An MIT weights licence therefore does not clear data provenance. This is the same unresolved question FramePilot flagged, and it needs an owner decision (see Q8).

### Gaps
- No primary-source latency or memory figures for any subject model on Apple M-series (Core ML, MPS or ONNX Runtime with the Core ML EP). This must be measured.
- No independent benchmark of BiRefNet vs BEN2 vs MVANet vs RMBG-2.0 on HRSOD/UHRSD at 2048+, and none evaluating Apple's foreground request on DIS5K.
- BEN2 base's exact DIS numbers (as opposed to BEN "base + refiner" in the paper), and whether the MIT base on Hugging Face equals the paper model.
- The U²-Net licence was not re-verified from the repo in this pass (rembg lists it).
- BiRefNet parameter counts and peak memory at 2048 and 4096 are not published on the cards read.

## Q2. Sky: dedicated sky models vs ADE20K semantic segmentation vs open vocabulary, and edge refinement at trees and hair

### Takeaway
No permissively licensed, dedicated sky model was found. SkyAR is CC BY-NC-SA. The strongest openly licensed route is the sky class of a closed-set semantic or panoptic model (EoMT with a DINOv2 backbone, OneFormer or Mask2Former; all MIT code). Its low-resolution probability is then upsampled with a confidence-weighted guided filter, which is Google's published approach. SAM 3 can take "sky" as a text prompt, but as a semantic segmenter it clearly trails closed-set models (Cityscapes 65.2 against 84.2 mIoU). It is also 848M parameters under the custom SAM Licence.

### Cited findings

**Dedicated sky models**
- **SkyAR**: code and weights under CC BY-NC-SA 4.0. The model is coord_resnet50 at 384×384, trained on Google's CVPRW20-SkyOpt dataset. — [SkyAR GitHub](https://github.com/jiupinjia/SkyAR)
- **Google "Sky Optimization"** (arXiv 2006.10172) shows the method a sky mask should copy:
  - Inference: segmentation at 256×256.
  - Upsampling: a *weighted* guided filter that takes per-pixel confidence and solves a weighted least-squares system, with a downsampling factor s=64 at inference. Its large spatial support lets correctly detected sky "propagate" into missed regions "in between leaves of trees".
  - Training labels: built from ADE20K by manual sky / not-sky / undetermined annotation, then density-estimation inpainting, then guided-filter refinement ("ADE20K+DE+GF"). Tree areas were put in the "undetermined" region because "the sky can be seen through tree branches".
  - Results: mIoU₀.₅ 0.935, boundary loss 0.0465, MCR₀.₅ 0.0134. Users preferred these masks over raw ADE20K 82.6% of the time.
  - Mobile latency: segmentation ~50 ms, guided-filter upsampling ~190 ms.

  — [Sky Optimization paper](https://arxiv.org/html/2006.10172)

**ADE20K and COCO-Stuff coverage**
- In the ADE20K 150-class benchmark, "sky" is class 3 and covers 8.78% of all pixels, the third most frequent class. The list has no snow, cloud or fog class. — [ADE20K objectInfo150.csv](https://github.com/CSAILVision/sceneparsing/blob/master/objectInfo150.csv)
- COCO-Stuff's 91 stuff classes include sky-other, clouds, fog, snow, sea, river, water-other, tree, grass, bush, plant-other, mountain, hill, rock, sand, dirt, gravel, ground-other, building-other, house, skyscraper, road and pavement. — [COCO-Stuff labels](https://github.com/nightrome/cocostuff/blob/master/labels.md)
- COCO-Stuff annotations and code are CC BY 4.0; the COCO images fall under Flickr terms. — [COCO-Stuff GitHub](https://github.com/nightrome/cocostuff)
- ADE20K images are for "non-commercial research and educational purposes" only. MIT CSAIL does not own the image copyright; the annotations and software are BSD-3. — [ADE20K GitHub README (via search)](https://github.com/CSAILVision/ADE20K)

**Closed-set semantic and panoptic models**
- **EoMT** (CVPR 2025): code MIT. Results from the DINOv2 model zoo (H100 unless noted):

  | Task | Model | Input | Score | FPS |
  |---|---|---|---|---|
  | ADE20K semantic | EoMT-L | 512 | 58.4 mIoU | 92 |
  | ADE20K panoptic | EoMT-L | 640 | 50.6 PQ | 128 |
  | ADE20K panoptic | EoMT-g | 640 | 51.3 PQ | 55 |
  | ADE20K panoptic | EoMT-L | 1280 | 51.7 PQ | 30 |
  | ADE20K panoptic | EoMT-g | 1280 | 52.8 PQ | 12 |
  | COCO panoptic | EoMT-S | 640 | 46.7 PQ | 330 |
  | COCO panoptic | EoMT-B | 640 | 51.6 PQ | 261 |
  | COCO panoptic | EoMT-L | 640 | 56.0 PQ | 128 |
  | Cityscapes | EoMT-L | 1024 | 84.2 mIoU | 25 |

  The ADE20K panoptic models were pre-trained on COCO. — [EoMT DINOv2 model zoo](https://github.com/tue-mps/eomt/blob/master/model_zoo/dinov2.md); [EoMT repo](https://github.com/tue-mps/eomt)
- A DINOv3 family of EoMT also exists; the Hugging Face docs say EoMT-L reaches up to 59.5 mIoU on ADE20K. — [HF EoMT-DINOv3 docs](https://huggingface.co/docs/transformers/main/en/model_doc/eomt_dinov3)
- **Backbone licences**:
  - DINOv2 code and weights are Apache-2.0. — [DINOv2 GitHub](https://github.com/facebookresearch/dinov2)
  - DINOv3 is under a custom "DINOv3 License". It allows commercial use, but redistribution must be under the same agreement, and "Built with DINOv3" must be displayed prominently. — [DINOv3 license (search summary of Meta page)](https://ai.meta.com/resources/models-and-libraries/dinov3-license/); [license-change request issue](https://github.com/facebookresearch/dinov3/issues/31)
- **OneFormer**: code MIT. ADE20K semantic mIoU (single-scale): Swin-L 57.0–57.4, DiNAT-L 58.1–58.3 at 640–1280 px. Panoptic PQ: Swin-L 51.4 (219M params), DiNAT-L 51.5 (223M), both at 1280. No FPS is given. — [OneFormer GitHub](https://github.com/SHI-Labs/OneFormer)
- **Mask2Former**: 57.7 mIoU on ADE20K, 57.8 PQ on COCO panoptic. — [arXiv 2112.01527](https://arxiv.org/abs/2112.01527). Code is MIT with Apache-2.0 portions (Deformable-DETR), and the repo was archived on 1 January 2025. — [Mask2Former GitHub](https://github.com/facebookresearch/Mask2Former)
- **SegFormer** (NVIDIA) is under the NVIDIA Source Code License: "may be used or intended for use non-commercially … 'non-commercially' means for research or evaluation purposes only." This covers derivative works, so its ADE20K weights are unsuitable. — [SegFormer LICENSE](https://github.com/NVlabs/SegFormer/blob/master/LICENSE)

**Open-vocabulary models**
- **SAM 3** as a semantic segmenter: ADE-847 13.8 mIoU, PC-59 60.8, Cityscapes 65.2. It runs in 30 ms per image on an H200 GPU (100+ objects). It is limited to simple noun phrases and "struggles to generalize to out-of-domain terms". — [SAM 3 paper](https://arxiv.org/html/2511.16719v1)
- SAM 3 handles "stuff" (for example sky, grass) as well as things. — [PyImageSearch SAM 3 overview (secondary)](https://pyimagesearch.com/2026/01/26/sam-3-concept-based-visual-understanding-and-segmentation/)
- CoCo-SAM3 (2026) identifies two failure modes when SAM 3 is used for open-vocabulary *semantic* segmentation:
  - inter-class conflict: masks from separate prompts "lack a unified and inter-class comparable evidence scale", giving overlapping coverage;
  - intra-class drift across synonyms.

  — [CoCo-SAM3 arXiv 2604.19648](https://arxiv.org/abs/2604.19648)
- **CLIPSeg**: code MIT, but "the MIT license does not apply to these weights". CLIP ViT-B/16 backbone, 352×352 output. Earlier weights gave "square-like predictions". — [CLIPSeg GitHub](https://github.com/timojl/clipseg)
- **Grounded-SAM-2**: Apache-2.0 plus component licences. Grounding DINO (open weights) is local, but the stronger detectors (Grounding DINO 1.5/1.6, DINO-X) are API-only. The repo says nothing about stuff classes such as sky. — [Grounded-SAM-2 GitHub](https://github.com/IDEA-Research/Grounded-SAM-2)

### Inferences
- **Recommended Sky pipeline.** Run a closed-set semantic or panoptic model once at ~512–1024 px and keep its softmax probability for sky. That gives a soft sky probability plus a confidence estimate (max probability or entropy). Then refine at artifact resolution (up to 4096) with a confidence-weighted guided filter, guided by the content-stage luminance or RGB. Treat the tree/vegetation class boundary as an "undetermined" band, as the Google paper does. This pipeline is exactly what the Sky Optimization paper evaluated, and it keeps the heavy model at low resolution.
- **Which model.** EoMT with a DINOv2 backbone (MIT code, Apache-2.0 backbone) is the best fit: it is fast (EoMT-L ADE20K 512 at 92 FPS on H100) and the latest architecture. OneFormer and Mask2Former are fine but older and slower.
  - A COCO-Stuff or COCO-panoptic-trained model has cleaner training-data terms (CC BY 4.0 annotations) than an ADE20K-trained one (non-commercial images).
  - COCO-Stuff also separates clouds and fog from sky, which matters for sky edits. ADE20K folds clouds into sky.
- **SAM 3 "sky" vs a dedicated semantic sky.**
  - Quality (inference from Cityscapes 65.2 vs 84.2 and the conflict issue CoCo-SAM3 describes): expect SAM 3 text prompts to be less reliable on stuff classes than a closed-set model trained on those classes. The comparison is not like for like: SAM 3's figure is zero-shot, while EoMT's 84.2 is supervised on Cityscapes itself. It still shows the size of the gap a specialist closes on its own taxonomy.
  - Cost: 848M parameters against ~300M for EoMT-L (estimate).
  - Licence: custom, not OSI.

  Its advantage is arbitrary nouns ("lake", "the red car"), not sky.
- CLIPSeg's 352 px output and unlicensed weights make it unsuitable as a provider.

### Gaps
- No per-class sky IoU for EoMT, OneFormer or Mask2Former on ADE20K was found; only mean IoU. Sky IoU is typically very high, but I have no primary source for it.
- No dedicated sky model with permissive weights was found. SkyFinder-trained models from academic papers were not located as downloadable permissive weights.
- No SAM 3 per-class results for "sky" were found.
- The exact ADE20K Terms of Use text was obtained only via a search summary, not the primary page.
- DINOv3-EoMT per-model numbers were not retrieved.

## Q3. People and parts: instance segmentation, human parsing, face parsing and hair matting

### Takeaway
For person instances, RF-DETR-Seg and RTMDet-Ins are the Apache-2.0 options; Ultralytics YOLO-seg is AGPL-3.0, which also covers the trained models. For body parts and face, MediaPipe's multiclass selfie segmenter (Apache-2.0) is the only clearly permissive model. It gives six classes (background, hair, body-skin, face-skin, clothes, others) at 256 or 512 px and is "not pixel perfect". Sapiens v1 is CC BY-NC, and Sapiens2 has a custom licence with use restrictions. For hair, the permissive matting models are BiRefNet-matting (MIT), MODNet (Apache-2.0) and ViTMatte (MIT, but it needs a trimap and was trained on Adobe Composition-1k). MatAnyone, MatAnyone2 and ZIM are non-commercial.

### Cited findings

**Person instance segmentation**
- **Ultralytics YOLO11 and later**: "All Ultralytics YOLO trained models fall under the AGPL-3.0 License by default… covering the training code and the models produced". The Enterprise licence bypasses this. — [Ultralytics license / HF card (via search)](https://www.ultralytics.com/license)
- **RTMDet-Ins** (MMDetection, Apache-2.0), 640 px, TensorRT FP16 latency:

  | Model | Mask AP | Params | Latency |
  |---|---|---|---|
  | tiny | 35.4 | 5.6M | 1.70 ms |
  | s | 38.7 | 10.18M | 1.93 ms |
  | m | 42.1 | 27.58M | 2.69 ms |
  | l | 43.7 | 57.37M | 3.68 ms |
  | x | 44.6 | 102.7M | 5.31 ms |

  — [MMDetection RTMDet configs](https://github.com/open-mmlab/mmdetection/tree/main/configs/rtmdet)
- **RF-DETR-Seg**: all segmentation variants are Apache-2.0; only the XL/2XL *detection* models and the "plus" package are under PML 1.0. DINOv2 backbone. COCO mask AP50:95 on a T4 with TensorRT FP16:

  | Model | Input | Mask AP | Latency |
  |---|---|---|---|
  | Nano | 312 | 40.3 | 3.4 ms |
  | Small | 384 | 43.1 | 4.4 ms |
  | Medium | 432 | 45.3 | 5.9 ms |
  | Large | 504 | 47.1 | 8.8 ms |
  | XL | 624 | 48.8 | 13.5 ms |
  | 2XL | 768 | 49.9 | 21.8 ms |

  — [RF-DETR GitHub](https://github.com/roboflow/rf-detr)

**MediaPipe image segmenters**

| Model | Input | Classes | CPU latency | GPU latency |
|---|---|---|---|---|
| SelfieSegmenter | 256×256 (landscape 144×256) | background, person | ~33–34 ms | ~33–35 ms |
| HairSegmenter | 512×512 | background, hair | 57.9 ms | 52.1 ms |
| SelfieMulticlass | 256×256 | background, hair, body-skin, face-skin, clothes, accessories | 217.76 ms | 71.24 ms |

— [MediaPipe Image Segmenter docs](https://developers.google.com/edge/mediapipe/solutions/vision/image_segmenter)

- The multiclass model card says:
  - Licence: "LICENSED UNDER Apache License, Version 2.0". It is a Vision Transformer with 256 and 512 variants.
  - Aspect ratio: "the input does not need to be resized to match the image size".
  - Limitations: "may not provide pixel perfect masks"; surveillance and identity recognition are out of scope.
  - Tuning: the model is tuned for selfie, video-conferencing and AR use.

  — [MediaPipe Multiclass Segmentation model card (PDF)](https://storage.googleapis.com/mediapipe-assets/Model%20Card%20Multiclass%20Segmentation.pdf)

**Human parsing and face parsing**
- **SCHP (Self-Correction Human Parsing)**: code MIT; the weights licence is not stated. Three checkpoints:
  - LIP: 20 classes (hat, hair, face, arms, legs, clothes and so on), 59.36 mIoU
  - ATR: 18 classes, 82.29 mIoU
  - Pascal-Person-Part: 7 classes, 71.46 mIoU

  — [SCHP GitHub](https://github.com/GoGoDuck912/Self-Correction-Human-Parsing)
- **face-parsing.PyTorch (BiSeNet)**: MIT; 19 CelebAMask-HQ classes (skin, brows, eyes, nose, lips, ears, hair, hat, glasses, neck, cloth and so on). The CelebAMask-HQ dataset and its own models are non-commercial. — [search summary citing zllrunning/face-parsing.PyTorch and the LiteRT port](https://github.com/zllrunning/face-parsing.PyTorch); [LiteRT BiSeNet port](https://huggingface.co/litert-community/BiSeNet-Face-Parsing-LiteRT)
- **Sapiens v1**: licence is CC BY-NC 4.0 ("for NonCommercial purposes only"). — [Sapiens LICENSE](https://github.com/facebookresearch/sapiens/blob/main/LICENSE). Models range from 0.3B to 2B parameters, were pre-trained on 300M human images at 1024 px, and cover body-part segmentation, pose, depth and normals. — [Sapiens GitHub](https://github.com/facebookresearch/sapiens)
- **Sapiens2** (released 24 April 2026; matting added 15 May 2026):
  - Tasks: 29 body parts, 308 keypoints, normals, pointmaps and human matting.
  - Sizes and resolution: 0.1B to 5B parameters, at 1024×768 with a 4096×3072 variant.

  — [Sapiens2 GitHub](https://github.com/facebookresearch/sapiens2)

  Its licence is the custom "Sapiens2 License". It does not explicitly permit commercial use and prohibits surveillance, biometric processing, deepfakes and identifying individuals. — [Sapiens2 LICENSE.md](https://github.com/facebookresearch/sapiens2/blob/main/LICENSE.md)

**Matting for hair**
- **MODNet**: "The code, models, and demos in this repository … are released under the Apache License 2.0." Trimap-free portrait matting, typically at 512 px. — [MODNet GitHub](https://github.com/ZHKKKe/MODNet)
- **ViTMatte**: MIT. It needs a trimap and is trained on Composition-1k (Adobe Deep Image Matting) and Distinctions-646. Composition-1k SAD: 21.46 (S) and 20.33 (B). — [ViTMatte GitHub](https://github.com/hustvl/ViTMatte)
- **MatAnyone and MatAnyone2**: NTU S-Lab License 1.0, non-commercial. — [MatAnyone GitHub](https://github.com/pq-yang/MatAnyone); [MatAnyone2 GitHub](https://github.com/pq-yang/MatAnyone2)
- **ZIM (NAVER, ICCV 2025)**: CC BY-NC 4.0. It reuses SAM's point and box prompt interface with ViT-B or ViT-L, and is trained on SA1B-Matte (derived from SA-1B). — [ZIM GitHub](https://github.com/naver-ai/ZIM)
- **BiRefNet-matting** (MIT): P3M-500-NP S=0.979. — [BiRefNet GitHub](https://github.com/ZhengPeng7/BiRefNet)

### Inferences
- **People.** Use an Apache-2.0 instance model: RF-DETR-Seg-M or L, or RTMDet-Ins-m or l. Avoid YOLO (AGPL on weights). Instance masks from these models are low-resolution (the input is ≤768 px) and coarse at hair. A "People" mask should therefore be:
  1. the instance box or mask from the detector, then
  2. refined by a high-resolution foreground or matting model inside the person's box: BiRefNet-portrait or BiRefNet-matting on the crop, or a SAM decoder prompted with the box.

  This crop-then-refine step also scales to 60 MP photos, because each person is processed at the model's native resolution.
- **Body and face parts.** MediaPipe multiclass is the only clearly permissive option for hair, face-skin, body-skin and clothes. Its selfie tuning and 256/512 inputs make it a candidate for portrait close-ups, run on a face or person crop rather than the full frame (inference). Fine face parts (eyes, lips, brows, teeth) need a CelebAMask-HQ-trained BiSeNet. Its code is MIT but its training data is non-commercial, so it carries the same data-provenance question as DIS5K. Sapiens and Sapiens2 are out under an open-licence policy.
- **Hair.** In permissive order, the options are:
  1. BiRefNet-matting / HR-matting (MIT, trimap-free)
  2. MODNet (Apache-2.0, portrait-only, lower quality)
  3. ViTMatte (MIT, with a trimap generated from the segmentation mask by erode/dilate, but Composition-1k training data is Adobe's)

### Gaps
- The Composition-1k (Adobe DIM) dataset licence text was not fetched in this pass; it is commonly described as research-only (not verified).
- No licence statement for the SCHP weights or the LIP/ATR datasets was found.
- No quality numbers for MediaPipe multiclass at 512 on a photographic benchmark.
- RF-DETR-Seg and RTMDet mask boundary quality (for example boundary AP) was not found.
- The Ultralytics licence was obtained from a search summary rather than the licence page itself.

## Q4. Landscape categories: which panoptic or semantic models cover sky, water, vegetation, mountain, building, road and ground, and is one model enough?

### Takeaway
A single ADE20K- or COCO-panoptic-trained model (EoMT, OneFormer or Mask2Former) covers Sky, Landscape (water, sea, river, lake, tree, grass, plant, mountain, hill, rock, sand, earth, building, house, skyscraper, road, sidewalk) and a "person" class in one pass, at 12–330 FPS on an H100 depending on size. It is not enough for Subject, which is salient and class-agnostic, for high-quality hair or edge detail, or for arbitrary Objects. Realistically the set is one panoptic model, one salient-object model and the interactive SAM decoder.

### Cited findings
- **ADE20K-150 landscape and architecture classes** (with pixel share): building 10.72%, sky 8.78%, tree 4.80%, road 3.98%, grass 1.83%, sidewalk 1.66%, person 1.60%, earth/ground 1.51%, mountain 1.09%, plant 1.04%, water 0.74%, house 0.60%, sea 0.53%, field 0.44%, rock 0.30%, sand 0.18%, skyscraper 0.18%, path 0.18%, river 0.15%, bridge, flower, hill 0.13%, palm, tower, land, fountain, waterfall, animal, lake 0.04%. There is no snow, cloud or fog class. — [ADE20K objectInfo150.csv](https://github.com/CSAILVision/sceneparsing/blob/master/objectInfo150.csv)
- **COCO-Stuff** adds snow, clouds, fog, gravel, dirt, bush and ground-other, and its annotations are CC BY 4.0. — [COCO-Stuff labels](https://github.com/nightrome/cocostuff/blob/master/labels.md); [COCO-Stuff GitHub](https://github.com/nightrome/cocostuff)
- **Mapillary Vistas** is CC BY-NC-SA (street-level, 66/124 classes). — [Mapillary Vistas](https://www.mapillary.com/dataset/vistas). **Cityscapes** is non-commercial, with a clause on safeguarding against dataset recovery from trained models. — [search summary citing Cityscapes terms](https://arxiv.org/pdf/2111.02374)
- EoMT and OneFormer speed and quality figures are the ones in Q2. EoMT's 1280 px ADE20K panoptic models run at 30 FPS (L) and 12 FPS (g) on an H100. — [EoMT DINOv2 model zoo](https://github.com/tue-mps/eomt/blob/master/model_zoo/dinov2.md)
- As a baseline, the Google sky paper's mask quality came from *refining ADE20K labels*: raw ADE20K sky boundaries around trees were judged worse in 82.6% of cases. — [Sky Optimization](https://arxiv.org/html/2006.10172)

### Inferences
- **One panoptic pass can produce many masks.** A single panoptic pass can publish one label map, or per-class soft probabilities at ~1024 px, from which Sky, Water, Vegetation (tree+grass+plant+flower+palm+field), Mountains (mountain+hill+rock), Architecture (building+house+skyscraper+tower+bridge), Ground (road+sidewalk+earth+path+sand+land+field) and People (person) masks derive *without re-running inference*. Each class mask then gets the guided-filter refinement at artifact resolution. This fits the proposal's "one artifact per selection" by storing one multi-class analysis artifact and deriving per-class alpha artifacts from it (inference).
- **Training set.** Prefer COCO-panoptic or COCO-Stuff-trained weights where available: cleaner annotation licence, plus snow, cloud and fog classes. Use ADE20K-trained weights where their finer landscape taxonomy (sea, lake, river, waterfall, field) matters, accepting the non-commercial image caveat (see Q8).
- **Why one model is not enough:**
  - "Background" as the inverse of Subject needs a saliency or foreground model;
  - panoptic "person" masks are coarse at hair;
  - "Objects" in the object-removal sense are arbitrary and user-picked, which needs SAM-style prompting.

### Gaps
- No EoMT, OneFormer or Mask2Former checkpoint trained on COCO-Stuff-164k (semantic, 171 classes) with permissive weights was confirmed in this pass.
- No per-class IoU for landscape classes.
- No Apple Silicon timings for any panoptic model.

## Q5. Depth: Depth Anything V2 and V3, Depth Pro and Marigold (licences and use cases)

### Takeaway
The permissive depth options are:
- Depth Anything V2 Small (24.8M, Apache-2.0)
- Depth Anything 3 Small and Base (0.08B and 0.12B, Apache-2.0)
- DA3-MONO-LARGE and DA3-METRIC-LARGE (0.35B, Apache-2.0)

DA V2 Base, Large and Giant and DA3 Large, Giant and Nested are CC BY-NC 4.0. Depth Pro's weights are under Apple's research-only AMLR licence. Marigold's weights are under RAIL++-M, which has use restrictions and is not OSI. Depth supports depth-range masks and gives object removal a foreground/background cue.

### Cited findings
- **Depth Anything V2**: code Apache-2.0. Small is 24.8M (Apache-2.0); Base is 97.5M, Large 335.3M and Giant 1.3B (all CC-BY-NC-4.0). Default input is 518. — [Depth-Anything-V2 GitHub](https://github.com/DepthAnything/Depth-Anything-V2)
- An open issue (13 April 2026) asks whether DA V2 Small's training data is cleared for commercial use; there is no maintainer response. — [DA-V2 issue #320](https://github.com/DepthAnything/Depth-Anything-V2/issues/320)
- Apple's Core ML DA V2 Small is Apache-2.0: 518×396 input, 49.8 MB in FP16. Neural Engine latency is 24.58 ms on an M3 Max and 32.80 ms on an M1 Max. — [apple/coreml-depth-anything-v2-small](https://huggingface.co/apple/coreml-depth-anything-v2-small)
- **Depth Anything 3**: code Apache-2.0.

  | Model | Params | Weights licence |
  |---|---|---|
  | DA3-SMALL | 0.08B | Apache-2.0 |
  | DA3-BASE | 0.12B | Apache-2.0 |
  | DA3-LARGE-1.1 | 0.35B | CC BY-NC 4.0 |
  | DA3-GIANT-1.1 | 1.15B | CC BY-NC 4.0 |
  | DA3NESTED-GIANT-LARGE-1.1 | 1.40B | CC BY-NC 4.0 |
  | DA3METRIC-LARGE | 0.35B | Apache-2.0 |
  | DA3MONO-LARGE | 0.35B | Apache-2.0 |

  The repo says DA3MONO-LARGE "directly predicts depth" rather than disparity. — [Depth-Anything-3 GitHub](https://github.com/ByteDance-Seed/Depth-Anything-3)
- **Depth Pro**:
  - Code licence: a permissive, Apple sample-code-style licence ("use, reproduce, modify and redistribute… with or without modifications"; no patent grant). — [ml-depth-pro LICENSE](https://github.com/apple/ml-depth-pro/blob/main/LICENSE)
  - Weights licence: Apple AMLR on Hugging Face. — [apple/DepthPro card](https://huggingface.co/apple/DepthPro). AMLR is "exclusively for Research Purposes", where Research Purposes "does not include any commercial exploitation, product development or use in any commercial product". — [AMLR licence (search summary)](https://github.com/apple/ml-mobileclip/blob/main/LICENSE_MODELS)
  - Performance: a 2.25 MP metric depth map plus focal length in 0.3 s on a standard GPU. — [apple/DepthPro card](https://huggingface.co/apple/DepthPro)
- **Marigold**: code Apache-2.0; all checkpoints (depth v1-0 and v1-1, LCM, normals, IID) are under the "RAIL++-M License". It derives from Stable Diffusion 2 (CreativeML OpenRAIL++-M), processes at 768 px, and uses 1 denoising step for depth. — [Marigold GitHub](https://github.com/prs-eth/Marigold)
- vision.cpp runs Depth-Anything Small at 518×714 in 11 ms on an RTX 4070 (Vulkan, f16). — [vision.cpp](https://github.com/Acly/vision.cpp)

### Inferences
- **Choice.** DA3-MONO-LARGE (Apache-2.0, 0.35B) is the highest-capacity permissive monocular model found. DA V2 Small or DA3 Small is the fast option, and has an Apple-published Core ML build for V2 Small at ~25 ms on the Neural Engine. Depth Pro and Marigold should be excluded under an open-licence policy.
- **Use cases:**
  - A depth-range mask component: a range component over a depth artifact, reusing the range kind's UI.
  - Separating foreground from background for Subject when saliency is ambiguous.
  - Depth-aware object removal: detecting that a clicked object occludes the background, and constraining fill sources to similar depth.
- **Resolution.** Depth models run at ~518–768 px. The depth artifact should be guided-upsampled like the other masks, and depth discontinuities are unreliable at hair and foliage.

### Gaps
- No quantitative comparison of DA3-MONO-LARGE vs DA V2 Small edge sharpness on photographic data.
- No Apple Silicon timings for DA3.
- MoGe-2 and other permissive depth alternatives were not researched.

## Q6. A unified approach: can one SAM-style backbone serve object removal and semantic masks, and what do darktable and other open editors do?

### Takeaway
Only SAM 3 truly unifies text-concept masks ("sky", "person", "lake") and click-to-select on one cached image encoding: its detector and tracker share one backbone at 1008 px. But it is 848M parameters, gated, and under the custom SAM Licence (not OSI; field-of-use and trade-control terms). It is also weaker than closed-set models on semantic segmentation, and has no "salient subject" concept. SAM 2.1 (Apache-2.0) and SegNext (MIT) serve object picking only.

darktable 5.6, the closest open precedent, ships ONNX Runtime with SAM 2.1 or SegNext for clicked object masks, off by default, from a curated catalogue that requires GPL-compatible weights. It has no automatic sky or subject masks. Krita's GPL plugin shares one runtime (vision.cpp) across MobileSAM, BiRefNet, MI-GAN inpainting and Depth Anything.

### Cited findings

**darktable 5.6** (June 2026; the first release with AI)
- Models come from a separate repo, darktable-ai, and "Binary weights are not bundled inside darktable itself, and models whose provenance isn't clear don't get included". The features are "optional and off by default … Nothing is downloaded until a feature you enable needs it". — [darktable blog: AI tools in 5.6](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/)
- The object mask tool "uses SAM2.1 or SegNext" and supports positive and negative click points with iterative refinement. The encoder runs once per image and is cached, and the lightweight decoder runs per click, with optional DenseCRF edge refinement. Masks become **vector paths** in the mask manager and can "optionally be exported as a PNG for use with the external raster masks module when finer edge detail is needed". No automatic sky or subject masks are mentioned. — [darktable 5.6.0 release notes](https://www.darktable.org/2026/06/darktable-5.6.0-released/)
- Runtime is ONNX Runtime. The bundled build is CPU-only on Linux, DirectML on Windows and Core ML on macOS; CUDA, MIGraphX, OpenVINO and DirectML providers are optional. Models are `.dtmodel` packages (ONNX plus `config.json`). — [darktable manual: how AI works](https://docs.darktable.org/usermanual/development/en/special-topics/ai/how-ai-works/)
- darktable-ai model policy:
  - "Model weights must be released under a license compatible with GPL-3.0 (e.g. Apache-2.0, MIT, BSD, GPL-3.0)"
  - models must meet the MOF "Open Source AI, Open Weights, or Open Model" classes
  - training datasets must have disclosed licences and documented provenance
  - excluded: "Models trained on undisclosed or scraped personal data without consent"
  - a paper and documented conversion scripts are required

  Its catalogue: SAM 2.1 tiny, small and base+; SegNext b2hq; NAFNet and NIND denoise; RawNIND raw denoise; BSRGAN and RealPLKSR upscale; OpenCLIP RN101-YFCC15M embeddings. — [darktable-ai GitHub](https://github.com/darktable-org/darktable-ai)
- darktable-ai's **SAM 2.1 small** card:
  - Licences: model Apache-2.0; training data SA-V (CC BY 4.0) and SA-1B under "a custom Meta research-only license" with commercial restrictions and a data-retention limit.
  - Encoder: input [1,3,1024,1024]; outputs high-resolution features at 256×256 and 128×128 plus a 64×64 embedding.
  - Decoder: returns 3 candidate masks with IoU scores, output at 1024×1024. High-resolution features stay cached between refinement clicks.

  — [darktable-ai mask-object-sam21-small](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-sam21-small)
- darktable-ai's **SegNext b2hq** card:
  - Licences: code MIT, weights MIT. Training data COCO and LVIS (CC BY 4.0) plus HQSeg-44K (mixed licences).
  - Model: ViT-B (MAE) at 1024×1024. The encoder is ~339 MB and outputs 768×64×64 features; the decoder is ~103 MB and takes points plus the previous mask.
  - Classification: MOF Class I. Paper: CVPR 2024.

  — [darktable-ai mask-object-segnext-b2hq](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-segnext-b2hq)

**Other open editors**
- **Krita Vision Tools** (GPL-3.0) offers click-to-select and box-to-select (SAM/MobileSAM), a "Precise mode", a Background Removal filter (BiRefNet), and MI-GAN "Smart Patch" inpainting. It runs on vision.cpp. — [krita-vision-tools](https://github.com/Acly/krita-vision-tools)
- **vision.cpp** is MIT, GGML-based, with GGUF models on CPU and Vulkan backends; its docs do not mention Metal. Supported models: MobileSAM (Apache-2.0), BiRefNet (MIT), Depth-Anything V2 (Apache or CC-BY-NC by size), MI-GAN (MIT) and Real-ESRGAN (BSD-3). — [vision.cpp](https://github.com/Acly/vision.cpp)
- **GIMP 3**: Intel's OpenVINO AI plugins add semantic segmentation, super-resolution and Stable Diffusion, and are inspired by GIMP-ML. — [openvino-ai-plugins-gimp](https://github.com/intel/openvino-ai-plugins-gimp)

**SAM 2.1**
- Code and checkpoints are Apache-2.0.

  | Checkpoint | Params | FPS (repo's table) |
  |---|---|---|
  | Tiny | 38.9M | 91.2 |
  | Small | 46M | 84.8 |
  | Base+ | 80.8M | 64.1 |
  | Large | 224.4M | 39.5 |

  — [SAM 2 GitHub](https://github.com/facebookresearch/sam2)
- "In image segmentation, our model is more accurate and 6x faster than" SAM. — [SAM 2 paper abstract](https://arxiv.org/abs/2408.00714)
- Apple publishes Core ML SAM 2.1 builds (tiny, small, large) in FP16 under Apache-2.0, for its "SAM2 Studio" macOS demo. — [apple/coreml-sam2.1-small](https://huggingface.co/apple/coreml-sam2.1-small)

**SAM 3**
- 848M parameters; a detector and a tracker share one vision encoder; text and exemplar prompts. SA-Co/Gold cgF1 54.1; LVIS AP 48.5; "75–80% of human performance" on SA-Co. Checkpoints are gated on Hugging Face. SAM 3.1 was released on 27 March 2026. — [SAM 3 GitHub](https://github.com/facebookresearch/sam3)
- Hugging Face's `Sam3TrackerModel` does SAM 2-style Promptable Visual Segmentation (points, boxes, masks) from the `facebook/sam3` checkpoint:
  - it accepts precomputed `image_embeddings` so the encoder runs once;
  - its prompt encoder `image_size` defaults to 1008 with patch 14.
  - The same page also runs automatic "mask-generation" with SAM 3.

  — [HF Sam3Tracker docs](https://huggingface.co/docs/transformers/main/en/model_doc/sam3_tracker)
- The SAM 3 abstract says: "an image-level detector and a memory-based video tracker that share a single backbone… improves previous SAM capabilities on visual segmentation tasks". — [HF Sam3Tracker docs quoting the paper](https://huggingface.co/docs/transformers/main/en/model_doc/sam3_tracker)
- A GitHub issue reports SAM 3 video inference at 5–6 FPS against 30+ FPS for SAM 2 on an H200. — [sam3 issue #425](https://github.com/facebookresearch/sam3/issues/425)
- **SAM Licence** (updated 19 November 2025): a broad grant including commercial use and derivatives. Conditions:
  - include a copy of the agreement with redistributions;
  - prohibited uses: military, nuclear, espionage, weapons, and ITAR or trade-controlled uses;
  - trade-control eligibility for the user;
  - no reverse engineering;
  - indemnify Meta for distributions.

  It is not OSI-approved. — [sam3 LICENSE](https://github.com/facebookresearch/sam3/blob/main/LICENSE)
- An MLX port, `mlx-community/sam3-image`, is ~3.4 GB and tagged "apache-2.0" on Hugging Face. That tag conflicts with the upstream SAM Licence of the weights it converts. — [mlx-community/sam3-image](https://huggingface.co/mlx-community/sam3-image)
- **EfficientSAM3**:
  - Code Apache-2.0; weights refer to the parent licences.
  - Variants: EV-M, RV-M and TV-M, 89–95M parameters each.
  - Text encoder: a distilled MobileCLIP text encoder (42.5M).
  - "SAM3-LiteText" variants keep SAM 3's 463M vision encoder.

  — [EfficientSAM3 GitHub](https://github.com/SimonZeng7108/efficientsam3)

  MobileCLIP model weights are under Apple's AMLR licence: "Research Purposes does not include any commercial exploitation, product development or use in any commercial product". — [ml-mobileclip LICENSE_MODELS](https://github.com/apple/ml-mobileclip/blob/main/LICENSE_MODELS)

### Inferences

**Can one backbone serve both features?**
- **With SAM 2.1 or SegNext: no.** They have no semantic or text head, so their cached embedding serves object removal and an "Objects" (click) mask kind, but not Sky, Landscape or Subject. The shared parts are the runtime, the per-image embedding cache, the picker UI and the SAM decoder used to *refine* semantic masks (a box or points from a semantic mask used as a SAM prompt).
- **With SAM 3: yes in principle.** One 1008² encoding serves text concepts ("sky", "water", "person", "tree") and clicks, and the mask-generation pipeline can propose every object for an object-removal "pick from overlay" UI.
- **SAM 3 trade-offs against specialised models:**
  1. Semantic accuracy is well below closed-set models (Cityscapes 65.2 vs 84.2 mIoU; ADE-847 13.8).
  2. There is no saliency concept for "Subject".
  3. Its masks are at the decoder's low resolution and still need edge refinement.
  4. At 848M parameters (~3.4 GB) against ~47M for SAM 2.1 Small plus ~300M for EoMT-L (estimate), it is heavier than the pair it would replace.
  5. The licence is custom. It fails a darktable-style "GPL-compatible weights" policy, and EfficientSAM3 inherits both the SAM Licence and the research-only MobileCLIP licence.
- **Recommendation (inference):**
  - pin SAM 2.1 (Apache-2.0) or SegNext (MIT) for object picking, shared by object removal and an Objects mask kind;
  - pin one closed-set panoptic model (EoMT/DINOv2, MIT and Apache) for Sky, Landscape and People;
  - pin one salient-object model (BiRefNet, MIT) for Subject and Background;
  - keep SAM 3 as an optional, clearly-labelled custom-licence resource if the owner accepts the SAM Licence.

**Architecture for sharing (inference, fitted to the module-capabilities contract)**
1. **One inference runtime service** used by both the masks host and the object-removal module. ONNX Runtime with the Core ML EP (darktable's choice) is proven with SAM 2.1, SegNext, BiRefNet and others. vision.cpp or GGML is the precedent for a single C/C++ runtime covering segmentation, background removal and inpainting in a GPL editor. `tract` would need operator-coverage testing on Swin and Hiera attention models.
2. **Per-image analysis cache**, keyed by (asset source fingerprint, content-stage fingerprint, model id and version). It stores:
   - the SAM encoder output (a derived estimate from the tensor shapes in the darktable card: SAM 2.1 ≈ 32×256² + 64×128² + 256×64² floats ≈ 16 MiB in FP32 or 8 MiB in FP16; SegNext 768×64² ≈ 12 MiB in FP32);
   - semantic logits or probabilities at ~1024 px;
   - a depth map.

   Mask tasks and object removal both read this cache. It is a bounded cache of derived, recomputable data, distinct from the immutable per-selection alpha artifacts that recipes reference.
3. **One picker UI.** Hover highlights the candidate object from a decoder run at the cursor, click adds, Alt-click subtracts, and a box selects. It emits the same prompt data (points, labels, box) for both "New mask › Object" and "Remove object". darktable's positive/negative point UI and Krita's point/box tools are the precedent.
4. **Semantic masks can seed SAM prompts.** For example, when the user wants a single tree or person from a class mask, sample points or a box from the semantic region and decode with SAM. This is another reuse of the cached embedding.
5. **Vectorising, as darktable does,** loses edge detail; darktable itself offers PNG raster export for that reason. Luxforge's planned 8-bit raster artifact is the better representation for semantic masks.

### Gaps
- No measured Apple Silicon latency for SAM 2.1, SegNext or SAM 3 encoders was found; one search snippet reported ~4 s total for Core ML SAM 2.1 small on an M3, but its source was unclear, so it is not used.
- No primary benchmark directly compares SAM 3 text-prompted "sky" to a closed-set sky class.
- SAM 3's mask decoder output resolution and edge quality were not found.
- darktable's DenseCRF parameters and its vectorisation method were not examined in source.
- RawTherapee, ART and digiKam were not surveyed; no evidence of semantic masking in them was found.

## Q7. High-resolution output: getting a 4096 px mask from 1024 px model output

### Takeaway
The proven, cheap method is confidence-weighted guided upsampling of the low-resolution soft mask using the full-resolution image as guide. Google's sky pipeline does this: 256² inference, then a weighted guided filter, ~190 ms on mobile. For subject edges and hair, running a native-HR model is better: BiRefNet_HR at 2048, BiRefNet_dynamic up to 2304, lite-2K at 2560×1440, or a crop-and-refine pass per object. Trimap-free matting (BiRefNet-matting, BEN2's refiner) or trimap-based ViTMatte can follow. The permissive class-agnostic refiners are CascadePSP (MIT) and SegRefiner (Apache-2.0 code), both trained on DIS5K/ThinObject/BIG-type data.

### Cited findings
- **Weighted guided filter upsampling:** 256² inference, confidence-weighted least squares, and large spatial support to recover sky between leaves. Upsampling costs ~190 ms on a mobile GPU. — [Sky Optimization](https://arxiv.org/html/2006.10172)
- **Native high-resolution models:**
  - BiRefNet_HR trained at 2048² beats 1024 BiRefNet on every DIS-VD metric (MAE .026 vs .034). — [BiRefNet_HR card](https://huggingface.co/ZhengPeng7/BiRefNet_HR)
  - BiRefNet_dynamic covers 256–2304, and BiRefNet_lite-2K takes 2560×1440. — [BiRefNet GitHub](https://github.com/ZhengPeng7/BiRefNet)
  - InSPyReNet blends a low-resolution and a high-resolution pyramid for high-resolution inputs. — [InSPyReNet GitHub](https://github.com/plemeri/InSPyReNet)
  - Sapiens2 has a 4096×3072 variant under a restrictive licence. — [Sapiens2 GitHub](https://github.com/facebookresearch/sapiens2)
- **CascadePSP**: MIT. "Class-agnostic and very high-resolution segmentation via global and local refinement"; installed via `pip install segmentation-refinement`; trained on the BIG dataset and relabelled PASCAL VOC. — [CascadePSP GitHub](https://github.com/hkchengrex/CascadePSP)
- **SegRefiner** (NeurIPS 2023): code Apache-2.0, weights licence unspecified. It refines by discrete diffusion; the HR variant is patch-based, trained on ThinObject-5K and DIS-5K. — [SegRefiner GitHub](https://github.com/MengyuWang826/SegRefiner)
- **HQ-SAM**: code Apache-2.0. It adds a "High-Quality Output Token" to SAM's decoder with minimal extra parameters, trained on HQSeg-44K. HQ-SAM 2 was released in November 2024, and Light HQ-SAM runs at 41.2 FPS. — [sam-hq GitHub](https://github.com/SysCV/sam-hq)
- **SegNext b2hq** is fine-tuned on HQSeg-44K for high-quality masks. — [darktable-ai SegNext card](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-segnext-b2hq)
- **darktable** adds optional DenseCRF edge refinement to SAM output. — [darktable 5.6.0 release](https://www.darktable.org/2026/06/darktable-5.6.0-released/)
- **SAM 2.1's decoder output is 1024×1024** whatever the source size. — [darktable-ai SAM 2.1 card](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-sam21-small)
- **BEN2's CGM refiner** reprocesses low-confidence pixels and claims 4K processing. — [BEN2 card](https://huggingface.co/PramaLLC/BEN2)

### Inferences
- **Two-stage pipeline for every model mask kind:**
  1. Model inference at native resolution (512–2048), keeping *soft* probabilities, not thresholds.
  2. A deterministic, non-learned, edge-aware upsample to the artifact size (≤4096 long side): a confidence-weighted guided filter or fast guided filter over content-stage luminance/RGB, with ~s=16–64 subsampling for speed.

  Stage 2 is pure Rust, deterministic and exact-testable, which helps the proposal's "bytes identical across two runs" acceptance.
- **Optional learned refinement** for Subject and People edges: crop around the object's bounding box and rerun the HR subject model or a matting model on the crop, so a small subject in a 60 MP frame gets the model's full native resolution. This is cheaper than tiling the whole frame and keeps peak memory bounded.
- **Tiling a full 4096 frame** through a salient-object model is risky: saliency is global, so tiles lose context and may flip foreground and background. Tiling suits semantic or depth models, with overlap, better than saliency models (inference).

### Gaps
- No published benchmark of CascadePSP vs SegRefiner vs guided filter on photographic 24–60 MP images.
- No measured cost of a weighted guided filter at 4096² on an M4. The mobile figure (190 ms) is for smaller images (resolution not stated in the summary).

## Q8. Licence matrix: CODE vs WEIGHTS, training-data caveats, and whether a GPL-3.0-or-later app can offer each as a user-initiated download

### Takeaway
Clean candidates, where code and weights are both permissive:
- BiRefNet (all variants), MVANet, BEN2 base, InSPyReNet and IS-Net
- SAM 2.1, SegNext and HQ-SAM code
- EoMT with DINOv2, OneFormer and Mask2Former code
- RF-DETR-Seg and RTMDet-Ins
- MediaPipe multiclass/selfie/hair
- MODNet, ViTMatte and BiRefNet-matting
- DA V2 Small, DA3 Small, Base, Mono-Large and Metric-Large
- CascadePSP and SegRefiner code

Nearly all of them are *trained* on academic datasets with non-commercial terms (DIS5K, ADE20K, SA-1B, CelebAMask-HQ, Composition-1k). darktable's GPL policy accepts that when the data is disclosed: it ships SAM 2.1, which was trained on research-only SA-1B.

Must be flagged:
- **Non-commercial (exclude):** RMBG-1.4 and 2.0, SkyAR, SegFormer, Sapiens v1, MatAnyone and MatAnyone2, ZIM, DA V2 B/L/G, DA3 L/G/Nested, Depth Pro weights, MobileCLIP (so EfficientSAM3's text path).
- **Custom terms (owner decision):** SAM 3 (SAM Licence), DINOv3-based EoMT (DINOv3 Licence), Sapiens2, Marigold (RAIL++-M).
- **Copyleft:** YOLO (AGPL-3.0 on weights).

### Cited findings (licence matrix)

| Model | Code licence | Weights licence | Training-data caveat | GPL app user download? |
|---|---|---|---|---|
| BiRefNet (general, lite, HR, dynamic, matting, lite-2K) | MIT | MIT ([repo](https://github.com/ZhengPeng7/BiRefNet)) | DIS5K (NC, no redistribution of processed data), DUTS, HRSOD, UHRSD, P3M-10k ([DIS5K terms](https://raw.githubusercontent.com/xuebinqin/DIS/main/DIS5K-Dataset-Terms-of-Use.pdf)) | Yes on the licence; data-provenance flag |
| BEN2 base | MIT ([card](https://huggingface.co/PramaLLC/BEN2)) | MIT (base); commercial "full" model separate | DIS5K plus a proprietary 22K set | Yes; data flag (proprietary set undisclosed) |
| MVANet | MIT | MIT ([repo](https://github.com/qianyu-dlut/MVANet)) | DIS5K | Yes; data flag |
| InSPyReNet | MIT | MIT per repo badge ([repo](https://github.com/plemeri/InSPyReNet)) | DUTS, HRSOD, UHRSD (+DIS5K) | Yes; data flag |
| IS-Net (isnet-general-use) | Apache-2.0 | Apache-2.0 ([repo](https://github.com/xuebinqin/DIS)) | DIS5K | Yes; data flag |
| RMBG-1.4 | — | bria-rmbg-1.4, non-commercial CC ([card](https://huggingface.co/briaai/RMBG-1.4)) | proprietary licensed images | **No (NC)** |
| RMBG-2.0 | on GitHub (licence not stated on card) | CC BY-NC 4.0, gated ([card](https://huggingface.co/briaai/RMBG-2.0)) | proprietary licensed images | **No (NC)** |
| Apple VNGenerateForegroundInstanceMaskRequest | OS API | OS API ([Apple docs](https://developer.apple.com/documentation/vision/vngenerateforegroundinstancemaskrequest)) | undisclosed | macOS-only system provider; not a download |
| SkyAR | CC BY-NC-SA 4.0 | CC BY-NC-SA 4.0 ([repo](https://github.com/jiupinjia/SkyAR)) | CVPRW20-SkyOpt | **No (NC)** |
| SegFormer (NVIDIA) | NVIDIA Source Code License (NC) | same ([LICENSE](https://github.com/NVlabs/SegFormer/blob/master/LICENSE)) | ADE20K etc. | **No (NC)** |
| EoMT (DINOv2 backbone) | MIT ([repo](https://github.com/tue-mps/eomt)) | backbone Apache-2.0 ([DINOv2](https://github.com/facebookresearch/dinov2)); fine-tuned weights licence not separately stated | ADE20K (NC images), COCO | Probably yes; confirm weights licence; data flag for ADE20K |
| EoMT (DINOv3 backbone) | MIT | DINOv3 License, custom ([Meta](https://ai.meta.com/resources/models-and-libraries/dinov3-license/)) | as above | **Custom: owner decision** ("Built with DINOv3" display, same-agreement redistribution) |
| OneFormer | MIT ([repo](https://github.com/SHI-Labs/OneFormer)) | not stated on README | ADE20K, COCO, Cityscapes | Needs weights-licence check |
| Mask2Former | MIT (+Apache-2.0 parts), archived 2025 ([repo](https://github.com/facebookresearch/Mask2Former)) | not stated on README | ADE20K, COCO | Needs weights-licence check |
| SAM 2.1 | Apache-2.0 | Apache-2.0 ([repo](https://github.com/facebookresearch/sam2)) | SA-1B (custom research-only), SA-V (CC BY 4.0) ([darktable-ai card](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-sam21-small)) | Yes (darktable ships it) |
| SegNext (b2hq) | MIT | MIT ([darktable-ai card](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-segnext-b2hq)) | COCO and LVIS (CC BY 4.0), HQSeg-44K (mixed) | Yes (darktable ships it) |
| HQ-SAM / HQ-SAM 2 | Apache-2.0 ([repo](https://github.com/SysCV/sam-hq)) | not stated in fetched text | HQSeg-44K (includes DIS5K-type data) | Likely; confirm weights licence |
| MobileSAM | — | Apache-2.0 (per [vision.cpp](https://github.com/Acly/vision.cpp)) | SA-1B distillation | Yes |
| SAM 3 / 3.1 | SAM License | SAM License, gated ([LICENSE](https://github.com/facebookresearch/sam3/blob/main/LICENSE)) | SA-Co (Meta) | **Custom: owner decision** (not OSI; military/ITAR/trade-control terms) |
| EfficientSAM3 | Apache-2.0 | inherits SAM Licence; MobileCLIP text encoder under Apple AMLR research-only ([repo](https://github.com/SimonZeng7108/efficientsam3); [MobileCLIP](https://github.com/apple/ml-mobileclip/blob/main/LICENSE_MODELS)) | — | **No (research-only component)** |
| CLIPSeg | MIT | not MIT (unspecified) ([repo](https://github.com/timojl/clipseg)) | PhraseCut etc. | **No (weights unlicensed)** |
| Grounded-SAM-2 | Apache-2.0 + components ([repo](https://github.com/IDEA-Research/Grounded-SAM-2)) | Grounding DINO open; stronger detectors API-only | — | Local Grounding DINO path only; remote APIs conflict with local-only inference |
| YOLO11/26 seg (Ultralytics) | AGPL-3.0 | AGPL-3.0 on trained models ([Ultralytics](https://www.ultralytics.com/license)) | COCO | GPL-compatible copyleft, but AGPL obligations on weights; flag |
| RTMDet-Ins | Apache-2.0 | Apache-2.0 (MMDetection) ([configs](https://github.com/open-mmlab/mmdetection/tree/main/configs/rtmdet)) | COCO (Flickr images) | Yes |
| RF-DETR-Seg (N–2XL) | Apache-2.0 | Apache-2.0 ([repo](https://github.com/roboflow/rf-detr)) | COCO; DINOv2 | Yes |
| MediaPipe selfie, hair, multiclass | Apache-2.0 | Apache-2.0 (model card) ([PDF](https://storage.googleapis.com/mediapipe-assets/Model%20Card%20Multiclass%20Segmentation.pdf)) | Google internal | Yes |
| SCHP | MIT ([repo](https://github.com/GoGoDuck912/Self-Correction-Human-Parsing)) | not stated | LIP, ATR, PPP | Needs check |
| face-parsing BiSeNet | MIT | MIT (per search summary) ([repo](https://github.com/zllrunning/face-parsing.PyTorch)) | CelebAMask-HQ (NC) | Licence yes; data flag |
| Sapiens v1 | CC BY-NC 4.0 | CC BY-NC 4.0 ([LICENSE](https://github.com/facebookresearch/sapiens/blob/main/LICENSE)) | Humans-300M | **No (NC)** |
| Sapiens2 | Sapiens2 License | Sapiens2 License ([LICENSE.md](https://github.com/facebookresearch/sapiens2/blob/main/LICENSE.md)) | — | **Custom, restrictive** |
| MODNet | Apache-2.0 | Apache-2.0 ([repo](https://github.com/ZHKKKe/MODNet)) | portrait matting data | Yes |
| ViTMatte | MIT | MIT ([repo](https://github.com/hustvl/ViTMatte)) | Composition-1k (Adobe), Distinctions-646 | Licence yes; data flag |
| MatAnyone / MatAnyone2 | S-Lab 1.0 (NC) | S-Lab 1.0 (NC) ([repo](https://github.com/pq-yang/MatAnyone)) | — | **No (NC)** |
| ZIM | CC BY-NC 4.0 | CC BY-NC 4.0 ([repo](https://github.com/naver-ai/ZIM)) | SA1B-Matte | **No (NC)** |
| Depth Anything V2 Small | Apache-2.0 | Apache-2.0 ([repo](https://github.com/DepthAnything/Depth-Anything-V2)) | open question ([#320](https://github.com/DepthAnything/Depth-Anything-V2/issues/320)) | Yes; data flag |
| Depth Anything V2 B/L/G | Apache-2.0 | CC BY-NC 4.0 | — | **No (NC)** |
| Depth Anything 3 S/B, Mono-L, Metric-L | Apache-2.0 | Apache-2.0 ([repo](https://github.com/ByteDance-Seed/Depth-Anything-3)) | — | Yes |
| Depth Anything 3 L/G/Nested | Apache-2.0 | CC BY-NC 4.0 | — | **No (NC)** |
| Depth Pro | Apple permissive code licence ([LICENSE](https://github.com/apple/ml-depth-pro/blob/main/LICENSE)) | Apple AMLR, research-only ([card](https://huggingface.co/apple/DepthPro)) | — | **No (research-only)** |
| Marigold | Apache-2.0 | RAIL++-M ([repo](https://github.com/prs-eth/Marigold)) | SD2 base (OpenRAIL++-M) | **Custom use-restricted: owner decision** |
| CascadePSP | MIT | MIT ([repo](https://github.com/hkchengrex/CascadePSP)) | BIG, relabelled VOC | Yes |
| SegRefiner | Apache-2.0 | not stated ([repo](https://github.com/MengyuWang826/SegRefiner)) | LVIS, ThinObject-5K, DIS-5K | Needs check; data flag |
| vision.cpp runtime | MIT ([repo](https://github.com/Acly/vision.cpp)) | n/a | n/a | Yes |

- darktable's precedent: weights must be "compatible with GPL-3.0 (e.g. Apache-2.0, MIT, BSD, GPL-3.0)" and meet a MOF openness class, and training data must have disclosed licences and provenance. Models "trained on undisclosed or scraped personal data without consent" are excluded. — [darktable-ai](https://github.com/darktable-org/darktable-ai)
- darktable-ai's own SAM 2.1 card discloses SA-1B's "custom Meta research-only license" and still ships the model. — [darktable-ai SAM 2.1 card](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-sam21-small)

### Inferences
- **A user-initiated download of weights** under Apache-2.0, MIT or BSD is compatible with a GPL-3.0-or-later app. This follows the darktable precedent, and matches Luxforge's resource descriptor (licence, SHA-256, provenance).
- **Non-commercial weights would impose a restriction on users that GPL-3.0 itself forbids adding to the program.** Whether separately downloaded weights count as part of the "program" is a legal question, not settled here. Excluding them, as darktable does, is the conservative path.
- **Custom-licence weights** (SAM Licence, DINOv3, Sapiens2, RAIL++-M) are consequential product trade-offs. Per the project rules they should be recorded as proposals for the owner, not decided.
- **The most pragmatic policy:** permissive weights required, training data disclosed in the resource descriptor (including non-commercial academic datasets), no gated or custom-licence models by default. This admits BiRefNet, SAM 2.1, SegNext, EoMT (DINOv2), RF-DETR-Seg, MediaPipe, DA3 Small/Base/Mono-Large and MODNet/ViTMatte, which covers every requested category.

### Gaps
- The weights licence for the OneFormer, Mask2Former, EoMT (DINOv2) and HQ-SAM fine-tuned checkpoints was not confirmed from model cards; the READMEs state only code licences.
- The exact Composition-1k, CelebAMask-HQ and LIP dataset terms were not fetched.
- Whether a GPL-3.0 program may *offer* (not bundle) NC or custom-licence weights is a legal interpretation I cannot resolve from sources.
- The Ultralytics, DINOv3 and AMLR licence details came partly from search summaries of the primary pages rather than full-text reads.
