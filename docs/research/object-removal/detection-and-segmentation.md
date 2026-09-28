# Detection and interactive segmentation for an object-removal tool (state as of 2026-09-27)

Scope: the "find the object(s)" half of object removal in Luxforge (Rust, iced/wgpu, GPL-3.0-or-later, Apple M4 first, 24–60 MP photos). Inpainting is out of scope. Licence notes use this key: **OK** = Apache-2.0/MIT/BSD/GPL-3.0 (usable by a GPL-3.0-or-later app); **AGPL** = combinable under GPL-3.0 §13 but the AGPL terms (network clause) stay with that part; **CUSTOM** = extra use restrictions, needs legal review; **NC** = non-commercial, incompatible with GPL-3.0 (GPL forbids further restrictions and requires allowing commercial use).

## 1. Segment Anything family: encoder cost, decoder cost, resolution, quality versus SAM-H, code and weight licences

### Takeaway
The best speed/quality/licence trade-off for Luxforge is in the Apache-2.0 group: **SAM 2.1 tiny/small** (reference quality; heavy Hiera encoder that does not map onto the Neural Engine), **EfficientViT-SAM L0/XL** (best measured accuracy per unit of compute; box-prompt quality at or above SAM-H), and **EdgeTAM / EfficientTAM / RepViT-SAM** (mobile-grade encoders with 5-click quality near SAM-H). EdgeSAM (fast, Core ML ready) is **non-commercial** (S-Lab License) and SAM 3 / SAM 3.1 are under the **custom SAM License** with gated weights, so both are problematic for a GPL-3.0 app; SAM 3 is also about 10× heavier than SAM 2.1-B+ and not better at point prompting.

### Cited Findings

**Original SAM (Apr 2023; superseded by SAM 2/2.1 for quality and speed)**
- Image encoder input is 1024×1024 (rescale long side, pad the short side); output is a 16× downscaled 64×64 embedding; "Given an image embedding, the prompt encoder and mask decoder predict a mask from a prompt in ∼50ms in a web browser"; the model emits three masks (whole, part, subpart) ranked by a small IoU-prediction head — [SAM paper (ar5iv)](https://ar5iv.labs.arxiv.org/html/2304.02643)
- Parameters/FLOPs (calflops, one box prompt): SAM-H 641.09 M / 5490 GFLOPs; SAM-L 312.34 M / 2640 G; SAM-B 93.74 M / 746.4 G — [Efficient SAM survey, Table 3](https://arxiv.org/abs/2410.04960)
- Encoder vs decoder split for ViT-H on GPU: encoder 611 M params, 452 ms; mask decoder 3.876 M params, 4 ms — [MobileSAM README](https://github.com/ChaoningZhang/MobileSAM)
- Checkpoint sizes: ViT-H 2.56 GB, ViT-B 375 MB (safetensors) — [HF facebook/sam-vit-huge](https://huggingface.co/facebook/sam-vit-huge), [HF facebook/sam-vit-base](https://huggingface.co/facebook/sam-vit-base)
- Licence: code Apache-2.0 — [segment-anything repo](https://github.com/facebookresearch/segment-anything); HF weights tagged apache-2.0 — [HF facebook/sam-vit-huge](https://huggingface.co/facebook/sam-vit-huge). **OK.**

**SAM 2 / SAM 2.1 (Jul/Sep 2024; current Apache-2.0 reference)**
- SAM 2.1 checkpoints: tiny 38.9 M params, small 46 M, base+ 80.8 M, large 224.4 M; video FPS on A100 (torch 2.5.1, CUDA 12.4) 91.2 / 84.8 / 64.1 / 39.5; SA-V test J&F 76.5 / 76.6 / 78.2 / 79.5; "The SAM 2 model checkpoints, SAM 2 demo code … and SAM 2 training code are licensed under Apache 2.0"; SAM 2.1 released 2024-09-29 — [facebookresearch/sam2 README](https://github.com/facebookresearch/sam2)
- Checkpoint sizes on disk: tiny 156 MB, small 184 MB, base+ 324 MB, large 898 MB; HF licence apache-2.0, not gated — [HF sam2.1-hiera-tiny](https://huggingface.co/facebook/sam2.1-hiera-tiny), [small](https://huggingface.co/facebook/sam2.1-hiera-small), [base-plus](https://huggingface.co/facebook/sam2.1-hiera-base-plus), [large](https://huggingface.co/facebook/sam2.1-hiera-large)
- Image (SA-23) quality, 1-click (5-click) mIoU, FPS on A100: SAM ViT-B 55.9 (80.9) @ 76.7 FPS; SAM ViT-H 58.1 (81.3) @ 21.7 FPS; HQ-SAM ViT-H 59.1 (79.8) @ 21.4; SAM 2 Hiera-B+ (SAM 2 data mix) 61.9 (83.5) @ 130.1 FPS; SAM 2 Hiera-L 63.6 (83.5) @ 61.4 FPS. The paper states SAM 2 is "more accurate and 6× faster" than SAM on images — [SAM 2 paper, Tables 5 and 15](https://arxiv.org/abs/2408.00714)
- SAM 2 image predictor recommends `multimask_output=True` for ambiguous prompts: "For ambiguous input prompts (such as a single click), this will often produce better masks than a single prediction"; `mask_input` is "A low resolution mask input to the model, typically coming from a previous prediction iteration" of size 256×256 — [sam2_image_predictor.py](https://github.com/facebookresearch/sam2/blob/main/sam2/sam2_image_predictor.py)
- Unified benchmark, COCO box-prompt mIoU (GT box): SAM2-B+ 75.7 vs SAM-H 77.4; center-point mIoU 54.3 vs 53.6 — [survey, Table 6](https://arxiv.org/abs/2410.04960)

**SAM 3 (Nov 2025) and SAM 3.1 (27 Mar 2026)**
- SAM 3 adds promptable concept segmentation (noun phrases, image exemplars) plus points/boxes/masks; ~850 M parameters: ~450 M vision encoder, ~300 M text encoder, ~100 M detector and tracker; Perception Encoder backbone with windowed attention (1008 px image split into 3×3 windows of 336 px); input "always a fixed square (usually 1008 × 1008)"; "On an H200 GPU, SAM 3 runs in 30 ms for a single image with 100+ detected objects" — [SAM 3 paper](https://arxiv.org/abs/2511.16719)
- Interactive (PVS) image quality on SA-37: SAM 3 1/3/5-click mIoU 66.1 / 81.3 / 85.1 vs SAM 2.1-L 66.4 / 80.3 / 84.3 — [SAM 3 paper (arXiv HTML)](https://arxiv.org/html/2511.16719)
- Repo states 848 M parameters; licence "SAM License"; checkpoints require requesting access on Hugging Face; SAM 3.1 Object Multiplex released 2026-03-27 — [facebookresearch/sam3](https://github.com/facebookresearch/sam3)
- HF: sam3.pt 3.45 GB, licence "other", gated (manual approval); sam3.1_multiplex.pt 3.50 GB, also gated — [HF facebook/sam3](https://huggingface.co/facebook/sam3), [HF facebook/sam3.1](https://huggingface.co/facebook/sam3.1)
- SAM 3.1 gives a ~7× speed-up at 128 objects on one H100 for multi-object video tracking (search-result summary of the Meta blog, not fetched in full) — [Meta AI blog](https://ai.meta.com/blog/segment-anything-model-3/)
- SAM License terms: non-exclusive, worldwide, royalty-free licence to use, reproduce, distribute and make derivative works; redistributors must "provide a copy of this Agreement"; acceptable-use prohibitions (military/warfare, nuclear, espionage, weapons) and trade-control compliance; termination on breach, after which you must delete the materials. Commercial use is permitted — [sam3 LICENSE](https://github.com/facebookresearch/sam3/blob/main/LICENSE). **CUSTOM.**
- An MLX port exists (~3.5 GB, text and box prompts, M1–M4, macOS 13+); its card is tagged Apache-2.0 even though the weights derive from SAM 3 — [HF mlx-community/sam3-image](https://huggingface.co/mlx-community/sam3-image)
- samexporter exports SAM 3 (ViT-H) to ONNX as image encoder + text encoder + decoder at opset 18 — [samexporter](https://github.com/vietanhdev/samexporter)

**EfficientSAM3 (Nov 2025 paper; stage-3 models 2026-06-11)**
- Distils SAM 3 into small students: EV-M 89.2 M, RV-M 92.7 M, TV-M 95.3 M total parameters (EfficientViT 22.2 M, RepViT 25.6 M or TinyViT 28.3 M vision encoders plus MobileCLIP text encoders); code Apache-2.0; ONNX/TensorRT export from the community; Core ML "pending" — [efficientsam3 repo](https://github.com/SimonZeng7108/efficientsam3), [arXiv 2511.15833](https://arxiv.org/abs/2511.15833)
- Fine-tuned checkpoints are 468–493 MB each; HF card tagged apache-2.0, not gated — [HF Simon7108528/EfficientSAM3](https://huggingface.co/Simon7108528/EfficientSAM3)

**MobileSAM (Jun 2023)**
- TinyViT encoder: 5 M params, 8 ms vs SAM-H 452 ms (GPU); same 3.876 M decoder, 4 ms; whole pipeline 9.66 M params, 12 ms; ~3 s per image on a Mac i5 CPU; ONNX export supported; Apache-2.0 — [MobileSAM README](https://github.com/ChaoningZhang/MobileSAM)
- Weights 40.7 MB — [HF dhkim2810/MobileSAM](https://huggingface.co/dhkim2810/MobileSAM)
- Quality: COCO GT-box mIoU 73.4 (SAM-H 77.4); centre-point 48.6 (SAM-H 53.6); COCO AP with ViTDet boxes 38.7 (SAM-H 46.5) — [survey, Tables 6–7](https://arxiv.org/abs/2410.04960)
- **OK.** Used by the GPL-3.0 Krita segmentation plugin via vision.cpp — [krita-ai-tools](https://github.com/Acly/krita-ai-tools)

**MobileSAMv2 (Dec 2023)** — see Q2 for its segment-everything speed-up. Its repo folder vendors Ultralytics 8.0.120, whose header reads "Ultralytics YOLO 🚀, AGPL-3.0 license" — [MobileSAM repo, MobileSAMv2/ultralytics](https://github.com/ChaoningZhang/MobileSAM/tree/master/MobileSAMv2/ultralytics). **AGPL** for the detector part.

**EdgeSAM (Dec 2023)**
- 9.6 M params; 38.7 FPS on iPhone 14; 164.3 FPS on a 2080 Ti; "40-fold" (paper abstract: 37-fold) faster than SAM; COCO mAP 42.1 vs SAM 46.1 vs MobileSAM 39.4; 1024×1024 input, 1×256×64×64 embedding; ONNX and Core ML exports for encoder and decoder — [EdgeSAM README](https://github.com/chongzhou96/EdgeSAM), [EdgeSAM paper](https://arxiv.org/abs/2312.06660)
- Distils the prompt encoder and mask decoder as well, with box and point prompts in the loop — [EdgeSAM paper](https://arxiv.org/abs/2312.06660)
- Files: encoder ONNX 22.1 MB, decoder ONNX 15.9 MB, Core ML packages 10.3 MB + 9.2 MB (zipped) — [HF chongzhou/EdgeSAM](https://huggingface.co/chongzhou/EdgeSAM)
- Licence: NTU S-Lab License 1.0; for "redistribution and/or use for commercial purpose … please contact the contributor(s)"; non-commercial by default — [EdgeSAM LICENSE](https://github.com/chongzhou96/EdgeSAM/blob/master/LICENSE). **NC.**

**EfficientViT-SAM (Feb 2024)**
- L0 512 px: 34.8 M params, 35 G MACs, COCO mAP 45.7, LVIS 41.8, 762 img/s; L1 47.7 M, 638 img/s; L2 61.3 M, 538 img/s; XL0 1024 px: 117.0 M, 185 G MACs, 47.5 / 43.9, 278 img/s; XL1 203.3 M, 322 G MACs, 47.8 / 44.4, 182 img/s (A100, TensorRT fp16, batch 16; mAP with ViTDet boxes as prompts). ONNX and TensorRT export procedures documented — [EfficientViT-SAM README](https://github.com/mit-han-lab/efficientvit/blob/master/applications/efficientvit_sam/README.md)
- Unified benchmark: XL1 has the highest box-prompt mIoU of all variants (COCO GT box 79.9 vs SAM-H 77.4) and COCO AP 47.8 vs SAM-H 46.5; L0 box mIoU 78.5 — [survey, Tables 6–7](https://arxiv.org/abs/2410.04960)
- Files: L0 encoder ONNX 123.1 MB + decoder 16.5 MB; XL1 encoder ONNX 797.5 MB; PyTorch L0 139 MB, XL0 468 MB, XL1 814 MB; HF licence apache-2.0 — [HF mit-han-lab/efficientvit-sam](https://huggingface.co/mit-han-lab/efficientvit-sam); code Apache-2.0 — [efficientvit repo](https://github.com/mit-han-lab/efficientvit). **OK.**

**EfficientSAM (Meta, Dec 2023)**
- Ti 10.22 M / S 26.41 M params — [survey, Table 3](https://arxiv.org/abs/2410.04960); official split ONNX (Ti encoder 24.8 MB + decoder 16.6 MB; S encoder 89.6 MB) and TorchScript, Apache-2.0 — [HF yunyangx/EfficientSAM](https://huggingface.co/yunyangx/EfficientSAM), [EfficientSAM repo](https://github.com/yformer/EfficientSAM)
- COCO GT-box mIoU Ti 74.7, S 76.1; COCO AP with ViTDet Ti 42.1, S 44.5 — [survey](https://arxiv.org/abs/2410.04960). **OK.**

**RepViT-SAM (Dec 2023)**
- Image encoder 44.8 ms and mask decoder 11.8 ms on a MacBook M1 Pro (Core ML Tools, 1024×1024); 48.9 ms encoder on iPhone 12; MobileSAM 482.2 ms and ViT-B-SAM 6249.5 ms encoders on the same Mac — [RepViT-SAM paper, Table 1](https://arxiv.org/abs/2312.05760)
- 27.22 M params; COCO GT-box mIoU 75.1; COCO AP (ViTDet) 43.3 — [survey](https://arxiv.org/abs/2410.04960); RepViT code Apache-2.0 — [THU-MIG/RepViT](https://github.com/THU-MIG/RepViT). **OK.**

**TinySAM (Dec 2023)**
- 42.0 GFLOPs; COCO AP 42.3 (Q-TinySAM 41.4, 20.3 GFLOPs) vs MobileSAM 41.0; LVIS AP 38.6; "hierarchical segmenting everything" strategy; Apache-2.0 — [TinySAM README](https://github.com/xinghaochen/TinySAM). **OK.**

**SAM-HQ / HQ-SAM (NeurIPS 2023) and HQ-SAM 2 (Nov 2024)**
- Adds a learnable High-Quality Output Token to SAM's decoder, fusing early and late ViT features; trained on 44K fine-grained masks (4 h on 8 GPUs); variants vit_b/l/h, Light HQ-SAM (vit_tiny), HQ-SAM 2 on SAM 2; ONNX export supported; Apache-2.0 — [sam-hq README](https://github.com/SysCV/sam-hq)
- ViT-L on the four fine-grained sets (DIS, COIFT, HRSOD, ThinObject): SAM mIoU 79.5 / boundary mBIoU 71.1 → HQ-SAM 89.1 / 81.8; 5.1 M trainable params (<0.5 % overhead); 5.0 → 4.8 FPS — [HQ-SAM paper (arXiv HTML)](https://arxiv.org/html/2306.01567)
- Light HQ-SAM: 45.0 COCO AP at 41.2 FPS vs MobileSAM 44.3 AP at 44.8 FPS — [sam-hq README](https://github.com/SysCV/sam-hq)
- Trade-off on generic data: HQ-SAM ViT-H SA-23 5-click mIoU 79.8 vs SAM ViT-H 81.3; HQ-SAM ViT-B 72.1 vs SAM ViT-B 80.9 — [SAM 2 paper, Table 15](https://arxiv.org/abs/2408.00714)
- Weights: sam_hq_vit_tiny 42.5 MB, vit_b 379 MB, vit_l 1.25 GB, vit_h 2.57 GB, sam2.1_hq_hiera_large 899 MB (HQ-SAM 2 is released for Hiera-L only in this repo); HF apache-2.0 — [HF lkeab/hq-sam](https://huggingface.co/lkeab/hq-sam). **OK.**

**SAM 2 on-device variants**
- **EdgeTAM** (Meta, Jan 2025): RepViT-M1 encoder, 1024×1024, 2D Spatial Perceiver for memory. Image SA-23 All 1-click (5-click) 55.5 (81.7) vs SAM 2.1 61.9 (83.5), at **40.4 FPS vs 1.3 FPS on iPhone 15 Pro Max** (Core ML, CPU+NPU); 16 FPS for video — [EdgeTAM paper](https://arxiv.org/abs/2501.07256). Code and checkpoints Apache-2.0; Core ML export gives image encoder, prompt encoder and mask decoder — [EdgeTAM repo](https://github.com/facebookresearch/EdgeTAM); checkpoint 56.1 MB — [HF facebook/EdgeTAM](https://huggingface.co/facebook/EdgeTAM). **OK.**
- **EfficientTAM** (Nov 2024): plain ViT-Ti/S encoders; 18 M (Ti) and 34 M (S) params; SA-23 All 1-click (5-click) Ti 58.2 (82.6), S 60.7 (83.0) vs SAM 2 61.9 (83.6) and SAM ViT-H 58.1 (81.3); iPhone 15 Pro Max per-video-frame latency Ti/2 261.4 ms, Ti 840.5 ms, S/2 450 ms, S 1010.8 ms; a 512×512 S variant runs 80.6 ms on iPhone vs 1010.8 ms at 1024 (SA-V test 74.5 → 71.5) — [EfficientTAM paper](https://arxiv.org/abs/2411.18933). Apache-2.0 — [EfficientTAM repo](https://github.com/yformer/EfficientTAM); checkpoints Ti 71.6 MB, S 136.4 MB — [HF yunyangx/efficient-track-anything](https://huggingface.co/yunyangx/efficient-track-anything). **OK.**

**Other variants in the unified benchmark**
- NanoSAM (NVIDIA): fastest (27.9 img/s on a 3090; 345 ms on Jetson Nano) but lowest quality (COCO GT-box mIoU 69.7) — [survey](https://arxiv.org/abs/2410.04960); Apache-2.0 — [nanosam repo](https://github.com/NVIDIA-AI-IOT/nanosam)
- SlimSAM-77: 9.85 M params, COCO GT-box mIoU 74.4 — [survey](https://arxiv.org/abs/2410.04960); Apache-2.0 — [SlimSAM repo](https://github.com/czg1225/SlimSAM); ONNX fp16 encoder 12.2 MB + decoder 8.6 MB — [HF Xenova/slimsam-77-uniform](https://huggingface.co/Xenova/slimsam-77-uniform)
- FastSAM (YOLOv8-seg based): COCO GT-box mIoU 60.5, LVIS 50.4 — [survey](https://arxiv.org/abs/2410.04960); AGPL-3.0 — [FastSAM repo](https://github.com/CASIA-IVA-Lab/FastSAM)

**Summary table (quality from the unified survey unless noted; GPU latency is per-image-per-box on an RTX 3090 from survey Table 4)**

| Model | Params | Input | 3090 latency | CPU latency | COCO GT-box mIoU | COCO AP (ViTDet boxes) | Code / weights licence |
|---|---|---|---|---|---|---|---|
| SAM-H | 641 M | 1024 | 461 ms | 9470 ms | 77.4 | 46.5 | Apache / Apache |
| SAM2-B+ | 80.8 M | 1024 | 85 ms | 1221 ms | 75.7 | 44.8 | Apache / Apache |
| MobileSAM | 10.1 M | 1024 | 30 ms | 424 ms | 73.4 | 38.7 | Apache / Apache (HF tag MIT) |
| EdgeSAM | 9.6 M | 1024 | 24 ms | 259 ms | 75.9 | 42.1 | S-Lab NC / S-Lab NC |
| EfficientSAM-Ti | 10.2 M | 1024 | 40 ms | 1159 ms | 74.7 | 42.1 | Apache / Apache |
| RepViT-SAM | 27.2 M | 1024 | 44 ms | 1013 ms | 75.1 | 43.3 | Apache / Apache |
| EfficientViT-SAM-L0 | 34.8 M | 512 | 16 ms | 194 ms | 78.5 | 45.7 | Apache / Apache |
| EfficientViT-SAM-XL1 | 203 M | 1024 | 52 ms | 1334 ms | 79.9 | 47.8 | Apache / Apache |
| TinySAM | 10.1 M | 1024 | 29 ms | 422 ms | 73.7 | 42.3 | Apache / Apache |
| NanoSAM | — | 1024 | 20 ms | — | 69.7 | 35.9 | Apache / Apache |

Sources: latency, mIoU and AP from [survey Tables 4, 6, 7](https://arxiv.org/abs/2410.04960); licences from the repos linked above. The survey's CPU model is not named in its Table 4.

### Inferences
- For Luxforge, **SAM 2.1-tiny/small** (Apache, ~39–46 M params, 156–184 MB) is the safest quality baseline and what darktable ships, while **EfficientViT-SAM-L0 or XL0** gives equal or better box-prompt quality at lower encoder cost. A mobile-class encoder (EdgeTAM, RepViT-SAM, EfficientTAM-Ti/2 or EfficientViT-L0) is the lever for fast per-image preparation.
- SAM 3 is a detector-plus-tracker built for concept prompts. Its point-prompt quality equals SAM 2.1-L, and its ~450 M-parameter vision encoder at 1008 px will be much slower than SAM 2.1-tiny on an M4 (estimate; no Apple measurement found). It is only worth considering for text prompts ("people", "sign", "trash can"), and its licence and gating are obstacles.
- EfficientSAM3's Apache-2.0 tag is questionable for weights distilled from and fine-tuned with SAM 3 components and data. Treat it as possibly carrying SAM License obligations until clarified (inference; no statement found either way).
- EdgeSAM would otherwise be a top pick (~30 ms on M4, see Q7), but it is NC. EdgeTAM (Apache, same RepViT-class idea, Core ML export, 56 MB) is the licence-clean substitute.

### Gaps
- No official parameter counts or M-series latencies found for SAM 2.1-tiny/small encoders alone. Apple's Core ML model cards list no benchmark table.
- SA-23 numbers for EfficientViT-SAM, RepViT-SAM and EdgeTAM are not all on the same protocol, so cross-paper comparisons are approximate.
- SegNext, the second darktable model, is covered in Q3.

## 2. Encoder-once, decoder-per-prompt: decoder speed, hover-to-highlight designs, and segment-everything cost

### Takeaway
Hover highlighting is feasible on an M4 by running the prompt decoder on pointer move. Measured Core ML decoder times on an M4 are 5.6–9.1 ms for the MobileSAM/EdgeSAM/EfficientViT decoders (SAM-1-style decoders), against a 16–50 ms budget, with one encoder pass per image (tens of ms for mobile encoders, hundreds of ms or more for SAM 2.1). The two production patterns are (1) the decoder runs on every pointer move over a cached embedding (Meta's SAM demo) and (2) precompute all objects and hit-test (Photoshop Object Finder, Lightroom People). A segment-everything pass costs about 1–2 s on a desktop GPU for grid-based AMG, or about 0.1–0.2 s with object-aware or detector prompts.

### Cited Findings
- SAM architecture: "Given an image embedding, the prompt encoder and mask decoder predict a mask from a prompt in ∼50ms in a web browser" — [SAM paper](https://ar5iv.labs.arxiv.org/html/2304.02643)
- Meta's SAM demo precomputes the image embedding (exported as .npy), runs the quantised (QUInt8) ONNX decoder in the browser with onnxruntime-web, multithreading and SIMD, and updates the mask on mouse move: "Move your cursor around to see the mask prediction update in real time" — [segment-anything demo README](https://github.com/facebookresearch/segment-anything/blob/main/demo/README.md)
- SAM ViT-H: encoder ~450 ms, decoder ~4 ms for one point; segment-everything decoder cost ~400 ms (16×16 grid), ~1600 ms (32×32), ~6400 ms (64×64) on top of the encoder — [MobileSAMv2 paper, Fig. 1](https://arxiv.org/abs/2312.09579)
- Measured decoder times on an **Apple M4 Mac mini** (Core ML, compute_units=ALL, FLOAT32, median of 8 runs after 3 warm-ups, one image and one centre-point prompt): EdgeSAM encoder 30.3 ms + decoder 9.1 ms; MobileSAM encoder 56.3 ms + decoder 5.6 ms; EfficientViT-SAM-L0 (512²) encoder 25.5 ms + decoder 6.2 ms. With tinygrad Metal JIT the decoders run 11.4–12.4 ms — [onnxsim PR #1904 results (merged 2026-09)](https://github.com/onnxsim/onnxsim/pull/1904)
- SAM 1-style decoder on an M1 Pro (Core ML): 11.8 ms — [RepViT-SAM paper](https://arxiv.org/abs/2312.05760)
- SAM 2's decoder takes the image embedding plus two high-resolution feature maps (32×256×256 and 64×128×128) and returns 3 masks at 1024×1024, 3 IoU scores and 3 low-res 256×256 masks — [darktable-ai SAM 2.1 small model README](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-sam21-small)
- SAM 2.1-L on an H100: single-prompt segmentation (encoder + decoder) p50 98 ms eager fp32 → 19–20 ms with AOTInductor fp16 and GPU preprocessing; AMG (32×32 grid, 1024 prompts) p50 741–864 ms eager → 112–136 ms optimised. Batching all 1024 AMG prompts raised memory from 4.35 GB to ~34 GB; fp16 AMG lost accuracy (mask-count "fail count" 306/1000) because "the number of masks generated is very sensitive to small numerical changes" — [PyTorch blog, SAM 2 acceleration](https://pytorch.org/blog/accelerating-generative-ai-segment-anything-2/)
- Segment-everything latency (ms) with 16²/32²/64² grids: SAM-H 824/2269/8103; SAM2-B+ 442/1610/6356; MobileSAM 347/1313/5022; EdgeSAM 404/1553/5877; EfficientViT-SAM-L0 258/1023/3938; EfficientSAM-S 1100/2290/6210; RepViT-SAM 512/1890/7437. Non-grid strategies: FastSAM 106, MobileSAMv2 173, TinySAM (hierarchical) 776, Q-TinySAM 1318 — [survey, Table 5](https://arxiv.org/abs/2410.04960)
- MobileSAMv2 replaces grid prompts with YOLOv8 object-aware box prompts (max 320): prompt stage 47 ms + decoder 50 ms = 97 ms vs 1616 ms (32×32 grid) and 6464 ms (64×64). Mask AR@1000 59.3 vs 59.2 for the best 64×64 multimask grid; with 256 prompts 58.5 vs 34.6 for a grid of the same size. Box prompts give "a significant performance boost at single mask mode" because they reduce ambiguity — [MobileSAMv2 paper, Tables 2–3](https://arxiv.org/abs/2312.09579)
- SAM 2 AMG defaults: points_per_side 32, points_per_batch 64, pred_iou_thresh 0.8, stability_score_thresh 0.95, box_nms_thresh 0.7, crop_n_layers 0, multimask_output True, optional `use_m2m` one-step refinement — [sam2 automatic_mask_generator.py](https://github.com/facebookresearch/sam2/blob/main/sam2/automatic_mask_generator.py)
- **Photoshop Object Finder**: as soon as the Object Selection Tool is chosen, the spinning Refresh icon shows "Photoshop is analyzing the image looking for objects". Objects are pre-detected, highlighted on hover and selected with a click; "Show All Objects" (key N) overlays them all in blue; Object Subtract removes detected objects inside an Alt-drag region; rectangle and lasso modes select the object inside a rough outline — [Photoshop Essentials](https://www.photoshopessentials.com/basics/using-the-object-selection-tool-and-object-finder-in-photoshop-2022/)
- **Lightroom Classic Distraction Removal: People** (June 2025 release, per search listing of [Adobe's June 2025 what's-new page](https://helpx.adobe.com/lightroom-classic/help/whats-new/2025-4.html)): "Lightroom will scan the image for people and highlight them with a red mask overlay", with per-person mask pins you can delete to keep people — [Digital Camera World](https://www.digitalcameraworld.com/photography/photo-editing/cheat-sheet-lightroom-classics-distraction-removal-tools-at-a-glance); the Adobe help says Lightroom detects all distracting people and you choose which to remove (search-result snippet; direct fetch returned 403) — [Adobe help](https://helpx.adobe.com/lightroom-classic/help/distraction-removal-people.html)
- **darktable 5.6** (June 2026) AI object mask: "The first click on a new image triggers the encoder pass – 1 to 10 seconds depending on hardware. Once it's done, the model is ready to answer click prompts interactively"; click adds, Shift-click subtracts; one connected object per mask; no hover mode described — [darktable blog](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/)
- SAM2 Studio (Hugging Face's Core ML SAM 2.1 demo app) supports foreground/background points and boxes; no hover mode is documented — [sam2-studio](https://github.com/huggingface/sam2-studio)

### Inferences
- **Hover with decoder-per-move** fits the budget on an M4 for SAM-1-style decoders (5.6–9.1 ms measured). With latest-wins coalescing of pointer events, 60 Hz hover is realistic. SAM 2.1's decoder does more work (high-res skip features, three 1024² outputs); using `low_res_masks` (256²) for the hover overlay and upsampling only on click should keep it inside ~16–30 ms (estimate; unmeasured on Apple).
- A single hover point is ambiguous (whole/part/subpart). Show the mask with the highest predicted IoU, or bias toward the largest stable mask, since for removal the whole object is usually what the user wants (inference from SAM's multimask design and SAM 2's stability fallback, Q3).
- **Precompute-and-hit-test** (the Photoshop approach) gives O(1) hover (label-map lookup) and a stable "objects" layer that also serves the brush flow in (a). Its cost is a segment-everything pass (MobileSAMv2-style object-aware prompts ≈0.1–0.2 s on a desktop GPU vs 1–2 s for a 32×32 grid). On an M4 a 1024-prompt grid at ~6–9 ms per decoder call would take roughly 6–9 s if unbatched (estimate), so use sparse or object-aware prompts, or a detector.
- Recommended hybrid (inference): on tool activation run the encoder once (background, cancellable) and a cheap proposal pass (detector boxes or a sparse grid). Hover hit-tests the proposals and falls back to a live decoder call where none exists. A click refines with the decoder at full quality.

### Gaps
- No published hover-latency measurement for SAM 2.1 decoders on Apple Silicon.
- Photoshop's and Lightroom's detection models are proprietary and undocumented. Only the UX behaviour is public.

## 3. Turning a painted brush stroke into prompts, and deciding one object or several

### Takeaway
No SAM-family model accepts a scribble natively. Converting every stroke pixel into a click performs badly, and a stroke used as a SAM mask prompt is unreliable. The robust designs are (i) a **box from the stroke bounds** plus a few positive points on the stroke, or (ii) **candidate proposals** (segment-everything or detector boxes) that are **filtered by overlap with the stroke**; the second maps directly onto the "one found → remove; several → highlight and click" UX, which Lightroom and Photoshop both ship. SegNext (MIT) is the one permissive model that natively takes scribbles, boxes and masks as dense prompt maps.

### Cited Findings
- Lightroom Classic Remove tool "Detect objects": you "roughly brush over the object you want to remove" and it detects "an object within the roughly brushed area to help you make a precise selection"; it "will also identify the shadow and reflections (if any) around the object and include it in the selection"; available only with Content-Aware Remove and Generative Remove (search-result snippets; direct fetch returned 403) — [Adobe Lightroom Classic help](https://helpx.adobe.com/lightroom-classic/help/remove-tool.html)
- Photoshop Object Selection rectangle/lasso modes select the detected object inside a rough outline; Object Subtract removes detected objects inside an Alt-drag — [Photoshop Essentials](https://www.photoshopessentials.com/basics/using-the-object-selection-tool-and-object-finder-in-photoshop-2022/)
- ScribblePrompt benchmark: for SAM baselines "we consider each scribbled pixel as a click", and for MedSAM "we fit a bounding box to the positive scribbles". Dice on manual scribbles (medical): SAM ViT-b 0.40, SAM ViT-h 0.56, MedSAM (box from scribble) 0.70, ScribblePrompt-UNet 0.84, ScribblePrompt-SAM 0.87. CPU latency per prediction: ScribblePrompt-UNet 0.27 s vs SAM ViT-b 13.59 s — [ScribblePrompt paper](https://arxiv.org/abs/2312.07381); repo Apache-2.0 — [ScribblePrompt](https://github.com/halleewong/ScribblePrompt)
- SAM `mask_input` is meant for a previous iteration's 256×256 logits — [sam2_image_predictor.py](https://github.com/facebookresearch/sam2/blob/main/sam2/sam2_image_predictor.py). When a rough label mask was fed as the only prompt, users reported output masks "mostly repeating the input mask" or slightly worse — [segment-anything issue #169](https://github.com/facebookresearch/segment-anything/issues/169), [issue #242](https://github.com/facebookresearch/segment-anything/issues/242)
- A cascade (box → coarse mask → box + coarse mask as prompts) is used in Prompt-Segment-Anything — [RockeyCoss/Prompt-Segment-Anything](https://github.com/RockeyCoss/Prompt-Segment-Anything)
- Users report bad results with many point prompts in SAM — [segment-anything issue #95](https://github.com/facebookresearch/segment-anything/issues/95), [issue #268](https://github.com/facebookresearch/segment-anything/issues/268)
- Box vs point quality (COCO mIoU): SAM-H centre point 53.6, 3 random points 67.5, GT box 77.4; SAM2-B+ 54.3 / 68.5 / 75.7; EfficientViT-SAM-XL1 54.3 / 70.2 / 79.9 — [survey, Table 6](https://arxiv.org/abs/2410.04960)
- SAM 2 decoder, "dynamic multimask via stability": "When outputting a single mask, if the stability score from the current single-mask output … falls below a threshold, we instead select from multi-mask outputs … the mask with the highest predicted IoU score". Defaults: stability delta 0.05, threshold 0.98. There are 4 mask tokens (1 single + 3 multi), an IoU head, and an optional object-score head (`pred_obj_scores`) — [sam2 mask_decoder.py](https://github.com/facebookresearch/sam2/blob/main/sam2/modeling/sam/mask_decoder.py)
- darktable's SAM 2.1 integration: "Multi-mask output: 3 candidate masks per prompt, select by highest IoU score"; low-res masks are fed back as `mask_input` for iterative refinement — [darktable-ai SAM 2.1 README](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-sam21-small)
- **SegNext** (CVPR 2024) encodes five prompt types (clicks, boxes, polygons, scribbles, masks) as a three-channel dense map; "A scribble is represented by a set of clicks"; the image is encoded once and prompts are fused by light modules — [SegNext paper](https://arxiv.org/abs/2404.00741). On HQSeg-44K, 5-click mIoU is 91.75 (SA×2 + HQ fine-tune) vs SAM ViT-B 86.16 and HQ-SAM 89.85. Its benchmark latency (A6000 + Xeon 6226R) is 17.6 s vs SAM ViT-B 7.0 s — [SegNext paper, Table 1](https://arxiv.org/abs/2404.00741). Code MIT — [SegNext repo](https://github.com/uncbiag/SegNext)
- darktable ships SegNext ViT-B SAx2-HQ as ONNX: encoder ~339 MB (1×768×64×64 features), decoder ~103 MB taking points plus a 1024² previous mask. darktable lists it as MIT, trained on COCO, LVIS and HQSeg-44K — [darktable-ai SegNext README](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-segnext-b2hq)
- SegNext's ViT-B backbone is MAE-pretrained — [darktable-ai SegNext README](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-segnext-b2hq); MAE's repository licence is "Attribution-NonCommercial 4.0 International" — [facebookresearch/mae](https://github.com/facebookresearch/mae)
- MobileSAMv2 finds box prompts much less ambiguous than points, and single-mask mode suffices with boxes — [MobileSAMv2 paper](https://arxiv.org/abs/2312.09579)
- Inpaint Anything (click → SAM → mask → LaMa) produces several SAM masks and lets the user choose (`--mask_idx` 0/1/2; "If the object is not segmented out well, you can try other masks") — [Inpaint-Anything](https://github.com/geekyutao/Inpaint-Anything)
- Adobe's SimpSON (CVPR 2023): a one-click distractor segmentation network plus a Click Proposal Network that mines objects similar to the clicked one, checked by a Proposal Verification Module, so one click selects a group of small distractors — [SimpSON paper](https://arxiv.org/abs/2305.17624)
- The Krita segmentation plugin (GPL-3.0) offers "Select Segment from Point" and "Select Segment from Box", plus a slower "Precise" mode (BiRefNet) that extracts "all foreground objects in the area, rather than one specific object" — [krita-ai-tools](https://github.com/Acly/krita-ai-tools)

### Inferences
- **Recommended stroke→prompt pipeline** (inference, built on the cited evidence):
  1. Rasterise the stroke to a coverage mask S (at encoder resolution) and take its bounding box B, padded by a few percent.
  2. **Candidate generation**: (a) the SAM decoder with box B, multimask on; (b) the decoder with 3–5 positive points sampled along the stroke's medial path (farthest-point sampling, not every pixel), multimask on; (c) proposals from a cached segment-everything or detector pass (Q2/Q5) whose masks intersect S.
  3. **Score each candidate mask M** with predicted IoU, stability score (reject below ~0.9–0.95, cf. SAM 2 defaults), *stroke coverage* |M∩S|/|S| and *object coverage* |M∩S|/|M| (how much of the object the stroke touched). Deduplicate with mask-IoU NMS (~0.7, as in AMG).
  4. **Decision rule**: if one candidate explains most of the stroke (coverage ≳0.6–0.8) with high predicted IoU and stability, remove it. If the stroke is covered by several disjoint candidates, each with meaningful object coverage, highlight them and ask. If the only candidates are "parts", prefer the parent mask (the largest of the multimask outputs that stays inside the padded box). The thresholds are starting points to tune on a Luxforge fixture set, not published values.
- Part-versus-whole ambiguity is structural in SAM (three granularity outputs). For removal, bias toward the larger mask, then dilate. Shadows and reflections are *not* covered by SAM masks; Lightroom explicitly adds them, so a shadow/reflection extension step (or a model trained for it) is a separate need.
- SegNext is the cleanest native scribble model, but its decoder is heavy (~103 MB ONNX; ~2.5× SAM ViT-B's benchmark latency) and its MAE-pretrained backbone may carry CC BY-NC provenance (MAE weights). darktable treats it as MIT; Luxforge should get its own licence review before shipping it.

### Gaps
- No published, quantitative study of stroke→SAM prompt conversion on natural photos was found. The medical ScribblePrompt result is the closest.
- SimpSON code and weights availability and licence were not confirmed.
- Adobe's "Detect objects" internals (model, on-device vs cloud) are undisclosed.

## 4. Resolution: 1024-px masks for 6000–9500-px photos, and what a removal mask needs

### Takeaway
Every SAM-class model predicts at 1024² (256² low-res logits; 64² embedding), so a 60 MP frame is downscaled about 9.3×. A raw upsampled mask is accurate to only ~6–37 source pixels. Workable refinements are guided or joint-bilateral upsampling (darktable), DenseCRF, a second pass on a zoomed crop around the object, and HQ-SAM-style decoders or dedicated high-res refiners (CascadePSP, BiRefNet; both MIT). Removal masks are dilated anyway, so completeness (no missed pieces, shadows or reflections) matters more than sub-pixel edges; heavy matting-grade refinement is not needed for removal.

### Cited Findings
- SAM rescales the long side to 1024 and pads; the embedding is 16× downscaled (64×64); mask prompts are input at 4× lower resolution (256×256) — [SAM paper](https://ar5iv.labs.arxiv.org/html/2304.02643)
- darktable's SAM 2.1 decoder emits 1024×1024 masks that are resized to the image size at runtime — [darktable-ai SAM 2.1 README](https://github.com/darktable-org/darktable-ai/tree/main/models/mask-object-sam21-small)
- darktable's pipeline: the decoder gives low-res masks (~256×256), then "Joint Bilateral Upsampling (JBU)" uses "the original image … as a guide to upsample the mask to full image resolution, snapping edges to high-contrast boundaries". Optional DenseCRF refines boundaries by colour and position, and the raster is vectorised with a Potrace-based `ras2vect` (source files `src/common/ai/segmentation.c`, `src/develop/masks/object.c`, `src/common/densecrf.cc`) — [DeepWiki summary of darktable source](https://deepwiki.com/darktable-org/darktable/6.2-neural-restore-and-ai-object-masking)
- darktable UI: results are "converted on the fly to a regular vector path", a Smoothing control (0–1.3) sets anchor density, "Refine boundary" "runs a second neural pass focused on the edge", and vector masks "can't represent per-pixel detail" for hair, fur and foliage — [darktable blog](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/)
- SAM's automatic generator also runs on "overlapping zoomed-in image crops" — [SAM paper](https://ar5iv.labs.arxiv.org/html/2304.02643); SAM 2 AMG exposes `crop_n_layers` ("mask prediction will be run again on crops of the image … each layer has 2**i_layer number of image crops") — [sam2 AMG](https://github.com/facebookresearch/sam2/blob/main/sam2/automatic_mask_generator.py)
- HQ-SAM raises boundary mBIoU from 71.1 to 81.8 (ViT-L, four fine-grained datasets) for <0.5 % extra params — [HQ-SAM paper](https://arxiv.org/html/2306.01567)
- SAM2Refiner (ICCV 2025) adds a localisation module (splits the image into four sub-images with global-local cross-attention) and a multi-scale cascaded mask-refinement module for high-resolution masks on SAM 2 — [SAM2Refiner paper](https://arxiv.org/abs/2502.09660)
- CascadePSP: class-agnostic "high-resolution segmentation refinement" of any input mask, with a fast global-only mode; pip package `segmentation-refinement`; MIT — [CascadePSP](https://github.com/hkchengrex/CascadePSP)
- BiRefNet: 1024² standard, BiRefNet_HR trained at 2048², dynamic 256–2304; 17 FPS on an RTX 4090 in FP16 at 1024² (3.45 GB); A100 69.4 ms FP16; ONNX available but ~75–90 % slower than PyTorch; code MIT — [BiRefNet](https://github.com/ZhengPeng7/BiRefNet). BiRefNet-Lite at 1024²: 85 ms on an RTX 4070 (Vulkan F16), 4505 ms on a Ryzen 5 5600X CPU — [vision.cpp](https://github.com/Acly/vision.cpp)
- ZIM (Naver, zero-shot matting) is CC BY-NC 4.0 — [naver-ai/ZIM LICENSE](https://github.com/naver-ai/ZIM). **NC.**
- Inpaint Anything dilates SAM masks before LaMa, `--dilate_kernel_size` 15 for Remove Anything and 50 for Fill Anything — [Inpaint-Anything](https://github.com/geekyutao/Inpaint-Anything)
- Lightroom's Detect objects includes shadows and reflections in the selection (search snippet) — [Adobe help](https://helpx.adobe.com/lightroom-classic/help/remove-tool.html)
- EfficientTAM at 512² instead of 1024² cuts iPhone latency from 1010.8 to 80.6 ms but drops SA-V J&F 74.5 → 71.5 — [EfficientTAM paper, Table 5](https://arxiv.org/abs/2411.18933)

### Inferences
- Arithmetic: a 9504×6336 (60 MP) image scaled to 1024 has one embedding cell ≈148 px and one low-res mask pixel ≈37 px; a 6000×4000 (24 MP) image has ≈94 px and ≈23 px. The 1024² output mask is ≈6–9 source px per pixel.
- Proposed Luxforge removal-mask pipeline (inference): (1) global pass on the 1024-px proxy to find the object; (2) if the object's bounding box is small relative to the frame (e.g. <25 % of the long side), **re-encode a padded crop around it at 1024²** and re-run the decoder with the transformed prompt/box, which is the zoom-in principle behind SAM's crop layers; (3) upsample the logits with a guided/joint-bilateral filter against the full-resolution luminance (darktable's approach), threshold, fill holes and drop small islands; (4) **dilate** by a resolution-relative radius (Inpaint Anything's 15 px was set for ~1–2 MP images, so a 24–60 MP frame needs a proportionally larger radius) and feather.
- CascadePSP or BiRefNet would add 100 ms–seconds per object and are only justified where precise edges survive into the result (for example "keep the subject, remove the background clutter"). For hole filling they are optional.
- Shadows and reflections need either a dedicated model or a heuristic extension. SAM masks alone will leave shadow "ghosts" after inpainting (inference from Lightroom's explicit feature).

### Gaps
- No quantitative study found comparing guided-filter upsampling against crop-and-rerun for SAM masks on 24–60 MP images.
- darktable's "Refine boundary" second pass is not described in detail (model and cost).

## 5. Object detectors and entity/"everything" segmenters as alternatives or complements (detector + SAM)

### Takeaway
A closed-set detector cannot find arbitrary distractors, but it is an excellent **proposal source and "people/vehicle/sign" finder** for the multi-object flow. The Apache-2.0 options are RF-DETR (N/S/M/L and all -Seg variants), D-FINE, DEIM (v1), RT-DETR and YOLOX. YOLOv9 and YOLO-World are GPL-3.0 (compatible with Luxforge). Ultralytics YOLOv8/11/12/26, YOLOE, FastSAM and MobileSAMv2's detector are AGPL-3.0. **DEIMv2 switched to a non-commercial licence in August 2026**, and CropFormer/EntitySeg and ZIM are CC BY-NC. Open-vocabulary detectors (Grounding DINO, OWLv2, Florence-2, YOLO-World) plus SAM (Grounded-SAM) make a strong detector→box→SAM pipeline for "find the people/cars/poles in this stroke", at 170–700 MB and roughly 10–100× the latency of a closed-set detector.

### Cited Findings

**Closed-set real-time detectors / instance segmenters**
- RF-DETR detection (T4, TensorRT FP16, batch 1): Nano 30.5 M, 48.4 AP, 2.3 ms, 384²; Small 32.1 M, 53.0 AP, 3.5 ms; Medium 33.7 M, 54.7 AP, 4.4 ms; Large 33.9 M, 56.5 AP, 6.8 ms (all Apache-2.0); XL 126.4 M, 58.6 AP, 11.5 ms and 2XL 126.9 M, 60.1 AP, 17.2 ms under **PML 1.0**. RF-DETR-Seg (all Apache-2.0): Nano 40.3 mask AP @ 3.4 ms (312²) up to 2XL 49.9 AP @ 21.8 ms (768²). "Plus components, including the `rfdetr_plus` extension and RF-DETR-XL / RF-DETR-2XL detection models, are licensed under PML 1.0" — [roboflow/rf-detr](https://github.com/roboflow/rf-detr). RF-DETR builds on DINOv2, which is Apache-2.0 — [facebookresearch/dinov2](https://github.com/facebookresearch/dinov2)
- DEIM (D-FINE based), T4 latency: N 43.0 AP / 4 M / 2.12 ms; S 49.0 / 10 M / 3.49 ms; M 52.7 / 19 M / 5.62 ms; L 54.7 / 31 M / 8.07 ms; X 56.5 / 62 M / 12.89 ms; Apache-2.0 — [DEIM repo](https://github.com/ShihuaHuang95/DEIM)
- **DEIMv2** ("Real Time Object Detection Meets DINOv3"): LICENSE.md, updated 2026-08-24, grants rights "solely for Non-Commercial Purposes" and "Commercial Use … is not permitted", covering code and model weights — [DEIMv2 LICENSE](https://github.com/Intellindust-AI-Lab/DEIMv2/blob/main/LICENSE.md). DINOv3 weights use the custom DINOv3 License (redistribution only under that Agreement) — [DINOv3 LICENSE](https://github.com/facebookresearch/dinov3/blob/main/LICENSE.md). **NC / CUSTOM.**
- Licences from GitHub metadata: D-FINE Apache-2.0 — [Peterande/D-FINE](https://github.com/Peterande/D-FINE); RT-DETR Apache-2.0 — [lyuwenyu/RT-DETR](https://github.com/lyuwenyu/RT-DETR); YOLOX Apache-2.0 — [YOLOX](https://github.com/Megvii-BaseDetection/YOLOX); YOLOv9 **GPL-3.0** — [WongKinYiu/yolov9](https://github.com/WongKinYiu/yolov9); Ultralytics **AGPL-3.0** — [ultralytics](https://github.com/ultralytics/ultralytics); Mask2Former MIT — [Mask2Former](https://github.com/facebookresearch/Mask2Former); OneFormer MIT — [OneFormer](https://github.com/SHI-Labs/OneFormer)

**Open-vocabulary detectors**
- YOLO-World: 35.4 AP on LVIS at 52.0 FPS on V100 (no TensorRT); in the same comparison Grounding DINO-T (Swin-T, 172 M) runs 1.5 FPS for 27.4 AP and GLIP-T 0.12 FPS (LVIS evaluation with the full 1203-class vocabulary) — [YOLO-World paper](https://arxiv.org/abs/2401.17270). YOLO-World code **GPL-3.0** — [AILab-CVC/YOLO-World](https://github.com/AILab-CVC/YOLO-World)
- YOLOE (ICCV 2025): text, visual or prompt-free modes (prompt-free uses a built-in 4,585-name vocabulary). T4 TensorRT / iPhone 12 Core ML FPS: YOLOE-v8-S 305.8 / 64.3 (LVIS 27.9 AP), v8-L 102.5 / 27.2 (35.9 AP), 11-L 130.5 / 35.1 (35.2 AP), vs YOLO-Worldv2-L 80.0 / 22.1 (35.5 AP). Prompt-free YOLOE-v8-L gets 27.2 AP at 25.3 FPS (T4, PyTorch) vs GenerateU 0.40–0.48 FPS — [YOLOE paper](https://arxiv.org/abs/2503.07465); **AGPL-3.0** — [THU-MIG/yoloe](https://github.com/THU-MIG/yoloe)
- Grounding DINO: Apache-2.0 — [GroundingDINO](https://github.com/IDEA-Research/GroundingDINO); grounding-dino-tiny weights 689 MB, apache-2.0 — [HF](https://huggingface.co/IDEA-Research/grounding-dino-tiny)
- OWLv2 base: 620 MB, apache-2.0 — [HF google/owlv2-base-patch16-ensemble](https://huggingface.co/google/owlv2-base-patch16-ensemble); code in scenic, Apache-2.0 — [google-research/scenic](https://github.com/google-research/scenic)
- Florence-2 base: 463 MB, MIT — [HF microsoft/Florence-2-base](https://huggingface.co/microsoft/Florence-2-base)
- Recognize Anything (image tagging): Apache-2.0 — [recognize-anything](https://github.com/xinyu1205/recognize-anything)

**Detector + SAM**
- Grounded-SAM and Grounded-SAM-2: Apache-2.0 — [Grounded-Segment-Anything](https://github.com/IDEA-Research/Grounded-Segment-Anything), [Grounded-SAM-2](https://github.com/IDEA-Research/Grounded-SAM-2)
- Box-prompted SAM variants on COCO: with YOLOv8 boxes SAM-H 43.8 AP, EfficientViT-SAM-XL1 44.7, EfficientViT-SAM-L0 42.7, SAM2-B+ 42.4; with Grounding DINO boxes 46.9 / 48.2 / 46.0 / 45.1 — [survey, Table 8](https://arxiv.org/abs/2410.04960). Grounded HQ-SAM reaches 49.6 mean AP on SegInW vs Grounded SAM 48.7 — [sam-hq README](https://github.com/SysCV/sam-hq)
- MobileSAMv2 is itself detector + SAM: object-aware YOLOv8 boxes (≤320) replace the grid for segment-everything at 97 ms vs 1616 ms — [MobileSAMv2 paper](https://arxiv.org/abs/2312.09579)
- SAM 3 is a unified open-vocabulary detector plus segmenter ("detect, segment, and track objects using text or visual prompts"), 30 ms on H200 for 100+ objects — [SAM 3 repo](https://github.com/facebookresearch/sam3), [SAM 3 paper](https://arxiv.org/abs/2511.16719). SAM License, gated. **CUSTOM.**

**Entity / everything segmenters**
- CropFormer / Entity Segmentation (qqlu/Entity): licence "Attribution-NonCommercial 4.0 International" — [qqlu/Entity LICENSE](https://github.com/qqlu/Entity). **NC.**
- FastSAM (YOLOv8-seg, segment-everything in 106 ms): AGPL-3.0, weak prompt quality (COCO GT-box mIoU 60.5) — [survey](https://arxiv.org/abs/2410.04960), [FastSAM](https://github.com/CASIA-IVA-Lab/FastSAM)

**Licence compatibility references**
- GPL-3.0 §13 permits combining a GPL-3.0 work with AGPL-3.0 code into one combined work, with the AGPL part keeping its network-use terms — [GNU GPL-3.0 text](https://www.gnu.org/licenses/gpl-3.0.html); the FSF lists Apache-2.0 as GPLv3-compatible and CC BY-NC as non-free — [GNU licence list](https://www.gnu.org/licenses/license-list.html)

### Inferences
- **"Find the distracting objects in this stroke"**: an open-vocabulary detector is overkill for (a), because the stroke already localises the object. A **class-agnostic proposal source** (sparse SAM AMG, or a closed-set Apache detector for common distractors such as person, car, bicycle, sign, pole and bin) plus stroke-overlap filtering is cheaper and licence-clean. Suggested Apache stack: **RF-DETR-Seg-Nano/Small** (instance masks at 312–384², ~3–4 ms on T4) for "people/vehicles" and a Lightroom-People-style multi-select, plus **SAM 2.1 / EfficientViT-SAM** for everything else.
- Grounded-SAM-style text prompting ("remove all people") is a later feature. Florence-2 (MIT) or OWLv2 (Apache-2.0) are licence-clean but 460–690 MB and slow (Grounding DINO-T 1.5 FPS on V100 with a 1203-class vocabulary; much faster with a handful of classes, not measured here).
- Avoid DEIMv2, EdgeSAM, CropFormer, ZIM and the RF-DETR XL/2XL weights on licence grounds. Treat Ultralytics-based models (YOLOv8/11/12/26, YOLOE, FastSAM, MobileSAMv2) as AGPL: legally combinable with GPL-3.0 but best avoided in a desktop core.

### Gaps
- No Apple-Silicon latencies found for RF-DETR, D-FINE, OWLv2, Florence-2 or Grounding DINO.
- CropFormer/EntitySeg cost on high-res images was not measured, since the licence excludes it.
- Whether RF-DETR-Seg masks at ~312–768 px are good enough for removal without SAM refinement is untested.

## 6. Portable exports (ONNX, Core ML, others) and known operator/export problems

### Takeaway
Encoder/decoder-split ONNX exports exist for almost every Apache candidate (SAM, SAM 2/2.1, MobileSAM, EfficientSAM, EfficientViT-SAM, SlimSAM, EdgeSAM, SAM 3) and Core ML exists for SAM 2.1 (Apple, FP16), EdgeSAM and EdgeTAM. The main pitfalls are that SAM 2's Hiera encoder does not compile for the Neural Engine (it falls back to GPU, with a ~16 s first-load penalty if compute units are left on ALL), that FP16/ANE precision can degrade masks (EdgeSAM IoU 0.86), that static shapes are needed, and that a few ops were missing in converters.

### Cited Findings
- Apple Core ML SAM 2.1 (FP16, Apache-2.0): three packages per size. Image-encoder weights: tiny 67.1 MB, small 81.3 MB, base+ 152.9 MB, large 444.4 MB; mask decoder 10.2 MB; prompt encoder 2.1 MB — [HF apple/coreml-sam2.1-tiny](https://huggingface.co/apple/coreml-sam2.1-tiny), [small](https://huggingface.co/apple/coreml-sam2.1-small), [baseplus](https://huggingface.co/apple/coreml-sam2.1-baseplus), [large](https://huggingface.co/apple/coreml-sam2.1-large); "converted in float16 precision" using a fork of the SAM 2 repo, for use with SAM2 Studio — [HF apple/coreml-sam2.1-tiny](https://huggingface.co/apple/coreml-sam2.1-tiny)
- WobblePic: SAM2's "Hiera backbone at that resolution does not fit what the NE will accept", so the encoder is loaded with `ComputeUnit.CPU_AND_GPU` and runs in about 310 ms. Left on `.all`, Core ML "attempts the NE compilation, fails, and falls back — burning roughly 16 seconds on first load" — [WobblePic blog](https://wobblepic.com/blog/apple-neural-engine-vs-gpu-cpu/)
- M4 Core ML: default precision on EdgeSAM (`CPU_AND_NE`) took 11.7 ms for both stages but gave 0.86 thresholded mask IoU vs ONNX Runtime; FLOAT32 was used for parity. The Core ML translator needed new `Resize`, `Not` and `DepthToSpace` lowerings for these SAM graphs — [onnxsim M4 results](https://github.com/onnxsim/onnxsim/pull/1904)
- darktable ships SAM 2.1 tiny/small/base+ and SegNext as **statically shaped ONNX** (opset 20, FP32 in the SAM README), split into encoder and decoder. The SAM 2 export bakes the high-res feature convolutions (conv_s0, conv_s1) into the encoder so the decoder receives pre-projected features. darktable runs them with ONNX Runtime providers CUDA, ROCm/MIGraphX, DirectML, OpenVINO and CoreML, with a CPU fallback — [darktable-ai model READMEs](https://github.com/darktable-org/darktable-ai/tree/main/models), [darktable blog](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/). darktable-ai requires "Model weights must be released under a license compatible with GPL-3.0" — [darktable-ai](https://github.com/darktable-org/darktable-ai)
- samexporter (MIT) exports SAM ViT-B/L/H, MobileSAM, SAM 2/2.1 (T/S/B+/L), SAM 3 (image encoder, text encoder, decoder; opset 18) and EfficientSAM-Ti/S. It warns that "dynamic quantization is not guaranteed to accelerate convolution-heavy encoders"; local test: EfficientSAM-Ti median 966 ms to encode an 1800×1200 image and 42 ms per box decode (hardware unstated) — [samexporter](https://github.com/vietanhdev/samexporter)
- MobileSAM ONNX export recommends onnx 1.12.0 / onnxruntime 1.13.1 — [MobileSAM README](https://github.com/ChaoningZhang/MobileSAM)
- EdgeSAM ships ONNX and Core ML for both halves — [HF chongzhou/EdgeSAM](https://huggingface.co/chongzhou/EdgeSAM); EdgeTAM has a Core ML export (image encoder, prompt encoder, mask decoder) — [EdgeTAM repo](https://github.com/facebookresearch/EdgeTAM); EfficientViT-SAM has official ONNX for all sizes — [HF mit-han-lab/efficientvit-sam](https://huggingface.co/mit-han-lab/efficientvit-sam); EfficientSAM has official split ONNX and TorchScript — [HF yunyangx/EfficientSAM](https://huggingface.co/yunyangx/EfficientSAM)
- vision.cpp (MIT, ggml C++) runs MobileSAM, BiRefNet, Depth-Anything V2, MI-GAN and Real-ESRGAN on CPU and Vulkan (no Metal backend documented). MobileSAM 1024²: 669 ms CPU F32 (Ryzen 5 5600X), 19 ms Vulkan F16 (RTX 4070) — [vision.cpp](https://github.com/Acly/vision.cpp)
- PyTorch AOTInductor could export SAM 2's encoder for all tasks but mask prediction only for AMG and single-prompt modes "due to varying prompts" — [PyTorch blog](https://pytorch.org/blog/accelerating-generative-ai-segment-anything-2/)
- A Unity user reports SAM2 encoder errors under GPU inference (search-result summary) — [Unity Discussions](https://discussions.unity.com/t/sam2-encoder-has-error-occurred-with-gpu-inference/1499931)

### Inferences
- For a Rust app: ONNX Runtime (via the `ort` crate) with the CoreML EP on macOS and DirectML/CUDA/OpenVINO elsewhere mirrors darktable's proven set-up. Core ML directly (Apple's `.mlpackage`) is the fastest path on the M4 but macOS-only. wgpu-native inference (e.g. Burn/candle-style) would need its own port (inference; not evaluated here).
- Pin compute units explicitly per model: `CPU_AND_GPU` for Hiera-based SAM 2.1 encoders, and ANE only for convolutional/mobile encoders after an FP16 parity check against a reference mask (the EdgeSAM 0.86 IoU warning).
- Use static shapes (1024² encoder input; fixed maximum point count with padding labels −1, as darktable's SegNext decoder does) for Core ML/ANE and ORT graph optimisations.

### Gaps
- Apple's Core ML SAM 2.1 cards publish no latency. Whether the Apple FP16 encoders run on the ANE for any size was not confirmed (WobblePic's model size and chip are unstated).
- EfficientSAM3 Core ML export is pending. No Core ML SAM 3 export was found.

## 7. Published latency measurements on Apple Silicon and consumer hardware

### Takeaway
Direct M4 evidence exists only for mobile SAM variants (M4 Mac mini, Core ML FP32): EdgeSAM 30 ms + 9 ms, MobileSAM 56 ms + 6 ms, EfficientViT-SAM-L0 (512²) 26 ms + 6 ms (encoder + decoder). SAM 2-class encoders on Macs are reported at ~310 ms (GPU, chip unstated) up to 1–10 s (darktable, "depending on hardware"). Luxforge must measure SAM 2.1-tiny/small and EdgeTAM on the M4 MacBook Pro itself before choosing.

### Cited Findings
- **M4 Mac mini**, Core ML ALL/FLOAT32, medians of 8 runs: EdgeSAM enc 30.3 ms / dec 9.1 ms; MobileSAM enc 56.3 / dec 5.6; EfficientViT-SAM-L0 (512²) enc 25.5 / dec 6.2; tinygrad Metal JIT encoders 34.5 / 147.2 / 53.2 ms; eager ONNX-on-Metal was much slower (e.g. EdgeSAM encoder 327 ms, decoders ~187–190 ms). One image and one prompt: a parity spot check, not a quality evaluation — [onnxsim PR #1904 / RESULTS_m4_sam_hybrid.md](https://github.com/onnxsim/onnxsim/pull/1904)
- **MacBook M1 Pro**, Core ML Tools, 1024²: RepViT-SAM encoder 44.8 ms, MobileSAM encoder 482.2 ms, ViT-B-SAM encoder 6249.5 ms, mask decoder 11.8 ms. On an iPhone 12 MobileSAM and ViT-B ran out of memory — [RepViT-SAM paper](https://arxiv.org/abs/2312.05760)
- SAM 2 encoder on a Mac via Core ML CPU+GPU: ~310 ms; Intel Mac: ~26 s with Core ML and ~3 s with ONNX Runtime CPU — [WobblePic blog](https://wobblepic.com/blog/apple-neural-engine-vs-gpu-cpu/)
- coreml-sam2.1-small on a MacBook Pro M3: "the inference time was around 4 seconds" (what is included is unspecified) — [mikeesto/sam2-coreml-python](https://github.com/mikeesto/sam2-coreml-python)
- darktable 5.6 (SAM 2.1 small by default): encoder "1 to 10 seconds depending on hardware", then interactive clicks — [darktable blog](https://www.darktable.org/2026/06/meet-darktable-5.6-ai-tools/)
- iPhone (Apple silicon, Core ML): EdgeSAM 38.7 FPS on iPhone 14 — [EdgeSAM](https://github.com/chongzhou96/EdgeSAM); EdgeTAM 40.4 FPS vs SAM 2/2.1 1.3 FPS on iPhone 15 Pro Max for the image SA task — [EdgeTAM paper](https://arxiv.org/abs/2501.07256); EfficientTAM-Ti/2 261 ms and S 1011 ms per video frame (image encoder + memory attention) on iPhone 15 Pro Max — [EfficientTAM paper](https://arxiv.org/abs/2411.18933); YOLOE-v8-S 64.3 FPS and YOLO-Worldv2-S 48.9 FPS on iPhone 12 — [YOLOE paper](https://arxiv.org/abs/2503.07465)
- Consumer GPUs and CPU (per image, per box prompt, survey Table 4): 2080 Ti — SAM-H 711 ms, SAM2-B+ 110 ms, MobileSAM 38 ms, EdgeSAM 32 ms, EfficientViT-SAM-L0 23 ms, XL1 82 ms, RepViT-SAM 69 ms, TinySAM 39 ms; 3090 — SAM-H 461, SAM2-B+ 85, EfficientViT-SAM-L0 16 ms; CPU — SAM-H 9470 ms, SAM2-B+ 1221 ms, EfficientViT-SAM-L0 194 ms, EdgeSAM 259 ms, MobileSAM 424 ms — [survey](https://arxiv.org/abs/2410.04960)
- RTX 4070 (Vulkan F16): MobileSAM 19 ms; Ryzen 5 5600X CPU F32: 669 ms — [vision.cpp](https://github.com/Acly/vision.cpp)
- A100: EfficientViT-SAM-L0 762 img/s, XL1 182 img/s (TensorRT fp16, batch 16) — [EfficientViT-SAM README](https://github.com/mit-han-lab/efficientvit/blob/master/applications/efficientvit_sam/README.md); SAM 2 Hiera-B+ 130.1 FPS vs SAM ViT-H 21.7 FPS on SA-23 images (batch 10) — [SAM 2 paper](https://arxiv.org/abs/2408.00714)
- H100 (SAM 2.1-L): single prompt 19–20 ms optimised; AMG 112–136 ms optimised; cold start with exported models ~0.66–0.9 s vs ~8–10 s with warm torch.compile — [PyTorch blog](https://pytorch.org/blog/accelerating-generative-ai-segment-anything-2/)
- H200: SAM 3 30 ms per image with 100+ objects — [SAM 3 paper](https://arxiv.org/abs/2511.16719)

### Inferences
- Projected M4 MacBook Pro budget (estimate from the measurements above): mobile encoder (EdgeTAM/RepViT/EfficientViT-L0) ~25–60 ms, SAM 2.1-tiny/small encoder ~0.2–0.5 s on GPU, decoder per prompt ~5–15 ms. Per-image preparation is therefore far below the user's "seconds" tolerance with any candidate, and hover stays inside 16–50 ms if the decoder runs on GPU/ANE with static shapes. These ranges are unverified until measured with Luxforge's `measure` tooling.
- A practical tiered design (inference): encode with a fast Apache encoder on tool activation for hover/proposals, and optionally run SAM 2.1-small (or a crop re-encode) in the background for the final removal mask, consistent with Luxforge's "fast, bounded, cancel stale work" rule.

### Gaps
- No first-party M4 (or M-series) measurements for SAM 2.1-tiny/small, EdgeTAM, EfficientTAM or EfficientViT-SAM-XL encoders on Core ML or ONNX Runtime CoreML EP.
- No Apple-Silicon numbers for SAM 3 / SAM 3.1 or EfficientSAM3.
- The survey does not name its CPU model. The WobblePic post does not state the Mac chip or SAM 2 size.
