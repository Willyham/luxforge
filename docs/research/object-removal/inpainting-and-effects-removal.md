# Inpainting and object-effect removal models for Luxforge's generative remove (state as of 2026-09-27)

Scope: local models that fill a removed object with plausible background and also remove its shadow, reflection, contact shading and colour cast. The target is Luxforge (GPL-3.0-or-later, Rust, M4 MacBook Pro first, 24–60 MP linear-float photos, background job, immutable patch composited only inside the mask). "Estimate" marks numbers I derived rather than read in a source. Licence facts come from each repo's LICENSE file or model card, fetched on 2026-09-27; where the GitHub API reported no licence file, I say so.

## 1. Non-diffusion (feed-forward) inpainters: quality, speed, resolution, failure modes, licences

### Takeaway
Big-LaMa is still the standard permissive baseline. Code and weights are Apache-2.0, it has 51M parameters, it is trained at 256 px yet stays coherent at about 1.5–2K, and it is good at periodic texture. MI-GAN (MIT code and weights, 5.95M parameters) is the smaller and faster permissive option. None of the feed-forward models deliberately removes shadows or reflections. They are deterministic, give no variations, and blur on large masks. MAT and FcF are unusable here because of non-commercial terms. CoordFill and RETHINED show fast 2K–4K inference, but their weight or code licences are unclear.

### Cited Findings
**LaMa / Big-LaMa (WACV 2022)**
- Fast Fourier convolutions give an image-wide receptive field. All LaMa models were trained on 256×256 crops taken from about 512×512 images. Big LaMa-Fourier has 18 FFC residual blocks and **51M parameters**, was trained on **4.5M Places-Challenge images**, with batch 120, on 8 V100s for about 240 h. LaMa-Fourier has 27M parameters, LaMa-Regular 45M and Big LaMa-Regular 74M — [LaMa paper](https://arxiv.org/pdf/2109.07161)
- The FFC models "preserve much more quality and consistency at the 1536×1536 resolution" than regular-convolution models and MADF (85M parameters). They are applied fully convolutionally in one pass, not patch-wise — [LaMa paper](https://arxiv.org/pdf/2109.07161)
- Strength: repetitive structures such as windows and chain-link fences, where FFCs "generate these types of structures significantly better". Stated weakness: LaMa "usually struggles when a strong perspective distortion gets involved" — [LaMa paper](https://arxiv.org/pdf/2109.07161)
- Code is Apache-2.0 (the GitHub API reports Apache-2.0 for advimman/lama) — [LaMa repo](https://github.com/advimman/lama). A hosted big-lama weights card is labelled Apache-2.0 — [smartywu/big-lama](https://huggingface.co/smartywu/big-lama), as cited in Luxforge's existing [corrections design](../../design/corrections.md)
- High-resolution add-on: "Feature Refinement to Improve High Resolution Image Inpainting" optimises LaMa's intermediate feature maps at inference with a multiscale consistency loss. The smallest pyramid level is about the training resolution and guides structure at higher resolutions. It needs no retraining and claims state-of-the-art high-resolution inpainting — [arXiv 2206.13644](https://arxiv.org/abs/2206.13644); code at [geomagical/lama-with-refiner](https://github.com/geomagical/lama-with-refiner/tree/refinement)
- Core ML port: CoreMLaMa (Apache-2.0) "runs well on macOS, on the GPU". There are reports of failures on iOS with fp16 on the Neural Engine — [CoreMLaMa](https://github.com/mallman/CoreMLaMa). One blog reports about 2 s on an iPhone 13 Pro with a model of about 200 MB (secondary source, seen only in a search snippet) — [MLBoy blog](https://rockyshikoku.medium.com/woooooooo-34fb311b2a9b)
- IOPaint recommends LaMa and MAT among its erase models and calls MI-GAN "minimal in size, requiring the least amount of resources". Its other erase models are LDM, ZITS, FcF and Manga — [IOPaint models](https://www.iopaint.com/models)

**MI-GAN (Picsart, ICCV 2023)**
- Code is MIT, and the weights have a separate MIT LICENSE-WEIGHTS file ("Copyright (c) 2024 Picsart AI Research"). Models exist for 256 and 512 (Places2) and 256 (FFHQ). The architecture is fully convolutional, but "current inference implementation has fixed-resolution operations". An ONNX pipeline export is provided — [MI-GAN repo](https://github.com/Picsart-AI-Research/MI-GAN), [LICENSE-WEIGHTS](https://raw.githubusercontent.com/Picsart-AI-Research/MI-GAN/main/LICENSE-WEIGHTS)
- 5.95M parameters. At 2048×2048: 3,905 ms (desktop GPU table), 1,200.8 ms on an iPad Pro M2 and 2,300.8 ms on an iPhone XS. LPIPS is 0.031 at 1024² and 2048² on the RETHINED benchmark — [RETHINED, WACV 2025](https://arxiv.org/html/2503.14757)

**MAT (CVPR 2022)**
- The LICENSE file is **CC BY-NC 4.0**, and the README says code and models are "for research purposes only". It is built on StyleGAN2-ADA, trained at 512 on Places365 and CelebA-HQ, supports "pluralistic" (multiple) outputs, and needs sizes that are multiples of 512 — [MAT repo](https://github.com/fenglinglwb/MAT), [LICENSE](https://raw.githubusercontent.com/fenglinglwb/MAT/main/LICENSE)

**FcF (Fourier Coarse-to-Fine, SHI Labs)**
- The code is Apache-2.0 "except for the third-party components". It bundles **stylegan2-ada-pytorch under the NVIDIA Source Code License**, which limits use to "research or evaluation purposes only". Models are 256/512 and were trained on 25M Places2 images — [FcF LICENSE](https://raw.githubusercontent.com/SHI-Labs/FcF-Inpainting/main/LICENSE), [FcF repo](https://github.com/SHI-Labs/FcF-Inpainting)

**ZITS (CVPR 2022) / ZITS++**
- Both are Apache-2.0. They need a separate wireframe/line detector (LSM-HAWP) and edge maps, and run as a two-stage transformer-structure model plus a texture model. Weights are 256/512, with HR results shown up to 1K. ZITS++ weights are on OneDrive — [ZITS](https://github.com/DQiaole/ZITS_inpainting), [ZITS++](https://github.com/ewrfcas/ZITS-PlusPlus)

**CoordFill (AAAI 2023)**
- "Real-time performance on the 2048×2048 images using a single GTX 2080 Ti GPU and can handle 4096×4096 images" — [arXiv 2303.08524](https://arxiv.org/abs/2303.08524). The repo is BSD-3-Clause. Weights are on Google Drive with no separate weight licence stated — [CoordFill repo](https://github.com/NiFangBaAGe/CoordFill)
- 101 ms at 2048² on GPU, 90.3 ms on an iPad Pro M2, LPIPS 0.038–0.039 — [RETHINED](https://arxiv.org/html/2503.14757)

**HiFill / Contextual Residual Aggregation (CVPR 2020)**
- The network predicts only a low-resolution fill. High-frequency residuals for the hole are built by weighted aggregation of residuals from context patches, then added to the upsampled fill. Trained at 512, it inpaints up to 8K and runs in real time at 2K on a GTX 1080 Ti — [arXiv 2005.09704](https://arxiv.org/abs/2005.09704)

**RETHINED (WACV 2025 oral, newer feed-forward)**
- A lightweight CNN restores structure, then a resolution-agnostic patch-replacement step adds texture. It has 4.3M parameters, runs in 34 ms at 2048² and 39 ms at 4096² on GPU, and in 17.6 ms at 2048² on an iPad Pro M2. It publishes DF8K-Inpainting (2,850 images at 2K–8K) — [RETHINED](https://arxiv.org/html/2503.14757). The GitHub API reports no licence file for [CrisalixSA/rethined](https://github.com/CrisalixSA/rethined); only the arXiv paper is CC BY 4.0.

**Known feed-forward failure modes (literature)**
- Removal-specific diffusion papers describe classical inpainters as leaving "residual artifacts" and failing "to remove associated effects such as shadows and reflections" — [ObjectClear](https://arxiv.org/html/2505.22636)
- Frameworks built on LaMa inherit its 256-px training. The refinement paper blames the inability to generate "globally coherent structures at resolutions higher than their training set" on a receptive field that stays fixed as resolution grows — [arXiv 2206.13644](https://arxiv.org/abs/2206.13644)

### Inferences
- Baseline shortlist: **Big-LaMa** (the maturity leader, with Apache code and weights, Core ML and ONNX ports, and a well-understood behaviour envelope) and **MI-GAN** (MIT code and weights, about 10× fewer parameters, 512 max). Both are sub-second to low-second on an M4 GPU at 512–1024 (estimate, from 51M and 6M parameters and the iPad Pro M2 figures). No M4-specific LaMa benchmark was found.
- A feed-forward baseline covers blemishes, sensor dust, wires, small objects and texture fills well. It will leave shadows and reflections unless they are painted into the mask, and it gives no variations.
- MAT and FcF are excluded because MAT is CC BY-NC and FcF bundles StyleGAN2-ADA code under an NVIDIA non-commercial licence. ZITS/ZITS++ add a line-detector dependency for modest gains, and their structure-prior advantage is superseded by diffusion models.
- CoordFill and RETHINED are interesting as fast, high-resolution fill engines. CoordFill's weight terms are unclear, and RETHINED has no code licence, so both need owner or author clarification before use.

### Gaps
- No published M4 (or any Apple M-series Mac) timing for Big-LaMa or MI-GAN in PyTorch MPS, Core ML or ONNX Runtime CoreML EP. This needs local measurement.
- I could not verify MI-GAN's own paper numbers (FID versus LaMa on Places2 512) from a primary source. The CVF PDF returned 403 and I did not rely on a search-snippet claim of "FID 11.83 vs LaMa 22.00".
- The big-lama weight licence rests on a third-party Hugging Face mirror. The original weights are distributed from the authors' storage without a separate weight licence. Confirm with the upstream repo or the authors.

## 2. Diffusion and flow-based removal: capability, cost, M4 feasibility

### Takeaway
2025–2026 removal models trained on real paired data remove objects together with shadows and reflections far better than generic inpainting. ObjectClear (CVPR 2026) is the reference, with distilled one-step descendants (TurboClear, FlashClear) and OSOR (ECCV 2026, one-step, SDXL or FLUX Fill). The best of them are non-commercial. ObjectClear and its derivatives use the S-Lab licence, and everything built on FLUX.1-dev/Fill/Kontext uses the FLUX non-commercial licence. The credible local paths on an M4 with 16–32 GB are SDXL-inpainting-class removers distilled to 1–4 steps (RORem-4S, OSOR-SDXL, TurboClear), at an estimated 2–10 s per 512–1024 crop, and FLUX.2 klein 4B (Apache-2.0), at about 17–30 s for a 4-step 1024 image on M-series Max chips. Large general editors (Qwen-Image-Edit 20B, Step1X-Edit, FLUX.1 12B) are minutes per image on Apple Silicon, or they do not fit.

### Cited Findings
**Specialised removal models (2024–2026)**

| Model (venue/date) | Base | Steps / time reported | Resolution | Effects (shadow/reflection) | Code licence | Weights licence | Source |
|---|---|---|---|---|---|---|---|
| **ObjectClear** (CVPR 2026) | SDXL-Inpainting 0.1 + CLIP ViT-L/14 | 20 steps, 1.63 s/image on A100 (OmniPaint 9.42 s) | 512 | Yes: predicts the object-effect mask from attention, no effect annotation needed | S-Lab License 1.0 (non-commercial) | same, and SDXL OpenRAIL++-M | [repo](https://github.com/zjx0101/ObjectClear), [paper](https://arxiv.org/html/2505.22636) |
| **TurboClear** (arXiv 2608.01288, Aug 2026) | ObjectClear/SDXL, one-step distillation (RDM + learnable spatial fusion) | 1 step, 0.0412 s on A800 vs ObjectClear 2.290 s and OmniPaint 20.34 s. 1.589 TFLOPs vs 63.63 and 1,058 | 512 (also 960×540) | Yes (inherits ObjectClear) | Apache-2.0 | "Third-party components and pretrained models retain their respective licenses" (ObjectClear-derived) | [paper](https://arxiv.org/html/2608.01288), [repo](https://github.com/GuoCalix/TurboClear) |
| **FlashClear** (arXiv 2605.09003, May 2026) | ObjectClear, adversarial few-step distillation and caching | "up to 8.26× and 122× speedup over ObjectClear and OmniPaint" | — | Yes (inherits) | not verified | ObjectClear-derived | [arXiv](https://arxiv.org/abs/2605.09003) |
| **OSOR** (ECCV 2026) | LoRA on SDXL-Inpainting (rank 256) or FLUX.1 Fill dev (rank 64), with an alpha head | 1 step, 0.42 s (SDXL) and 0.80 s (FLUX) on A100 | up to 1024² | Yes: an alpha head predicts soft effect regions beyond the user mask | MIT | OSOR-SDXL: **CreativeML OpenRAIL++-M**. OSOR-FLUX: **FLUX.1-dev Non-Commercial**. CORNE data (287,012 pairs): Apache-2.0 | [paper](https://arxiv.org/html/2606.28094), [repo](https://github.com/Zhouqm-Git/osor) |
| **RORem** (CVPR 2025) | SDXL-Inpainting, fine-tuned; LCM LoRA distilled | 4 steps "less than 1 second" | 512 (the mixed variant also 1024) | Not explicitly; trained on 200K human-verified real pairs | Apache-2.0 | HF tag apache-2.0 (SDXL-derived, so the OpenRAIL++-M base terms still matter) | [paper](https://arxiv.org/abs/2501.00740), [repo](https://github.com/leeruibin/RORem), [HF](https://huggingface.co/LetsThink/RORem) |
| **OmniEraser** (arXiv 2501.07397) | FLUX.1-dev LoRA (also a ControlNet variant using alimama FLUX inpainting ControlNet) | 28 steps. 0.104 s per step (from OmniPaint's comparison) | 1024 | Yes (Video4Removal data) | No licence file detected | HF tag apache-2.0, but "fine-tuned from FLUX.1-dev", so FLUX NC governs the base | [paper](https://arxiv.org/html/2501.07397v3), [repo](https://github.com/PRIS-CV/Omnieraser), [HF](https://huggingface.co/theSure/Omnieraser) |
| **OmniPaint** (ICCV 2025) | FLUX.1-dev, trained at 1024² | 28 steps. 0.179 s per step | 1024 | Partly (insertion and removal) | No licence file detected | FLUX.1-dev NC | [arXiv](https://arxiv.org/abs/2503.08677), [supp.](https://openaccess.thecvf.com/content/ICCV2025/supplemental/Yu_OmniPaint_Mastering_Object-Oriented_ICCV_2025_supplemental.pdf), [repo](https://github.com/yeates/OmniPaint) |
| **GeoRemover** (NeurIPS 2025 spotlight) | FLUX.1-Fill-dev + Video-Depth-Anything; removes the object in depth, then renders depth→RGB | two FLUX passes | — | Yes: effects follow from the changed geometry | No licence file detected | FLUX.1 Fill dev NC | [arXiv](https://arxiv.org/abs/2509.18538), [repo](https://github.com/buxiangzhiren/GeoRemover) |
| **PredErase** (arXiv 2609.00956, Sep 2026) | Frozen **FLUX.2 klein 4B** + frozen **I-JEPA ViT-H/14**, training-free | 5.3–6.3 s/image vs 3.0 s native FLUX.2 on A100 40 GB | 768 longest side | Yes: explicit "contact band" mask expansion | MIT | klein 4B Apache-2.0, but **I-JEPA is CC BY-NC 4.0** | [paper](https://arxiv.org/html/2609.00956), [repo](https://github.com/xiuwk0820/PredErase), [I-JEPA LICENSE](https://github.com/facebookresearch/ijepa) |
| **SmartEraser** (CVPR 2025) | SD 1.5-inpainting; "masked-region guidance" keeps the object in the input | — | — | Weak: the authors recommend mixing in about 30K RORD pairs "to compensate for object shadows gaps" | MIT | SD 1.5 base (CreativeML OpenRAIL-M); weights licence not stated | [repo](https://github.com/longtaojiang/SmartEraser) |
| **Attentive Eraser** (AAAI 2025 oral) | Training-free self-attention redirection on SDXL or SD2.1 | example: 50 steps at 1024 | 1024 | Not addressed | Apache-2.0 | base model terms (OpenRAIL++) | [repo](https://github.com/Anonym0u3/AttentiveEraser) |
| **CLIPAway** (NeurIPS 2024) | SD1.5-inpainting + IP-Adapter + AlphaCLIP | — | 512 | Not addressed. Mask downscaling to the latent "can cause some inconsistencies" | MIT | base terms | [repo](https://github.com/YigitEkin/CLIPAway) |
| **PowerPaint** v1/v2/v2-1 | v1 SD1.5; v2 on BrushNet | guidance ≥10 recommended for removal | 512 | Not addressed | MIT | v2-1 HF tag apache-2.0 (the SD1.5 base is still OpenRAIL-M) | [repo](https://github.com/open-mmlab/PowerPaint), [HF](https://huggingface.co/JunhaoZhuang/PowerPaint-v2-1) |
| **BrushNet** | SD1.5 (512) / SDXL (1024) | — | 512/1024 | Not addressed. The SDXL checkpoint is "only trained for a small step number" | Apache-2.0 except third-party parts | not stated | [repo](https://github.com/TencentARC/BrushNet) |
| **Erase Diffusion / EraDiff** (CVPR 2025) | chain-rectifying optimisation + self-rectifying attention | — | — | Not stated | No code found | — | [arXiv](https://arxiv.org/abs/2503.07026) |
| **YOEO "You Only Erase Once"** (CVPR 2026) | few-step distilled erasure diffusion, trained on unpaired real images | few-step | — | Focus on "no unexpected content" | not verified | — | [arXiv](https://arxiv.org/abs/2603.27599) |
| **PixelHacker** (2025) | diffusion with latent category guidance, 14M image-mask pairs | — | 512 (FID 8.59, LPIPS 0.2026 on Places2 40–50% masks) | general inpainting | Apache-2.0 | not verified | [repo](https://github.com/hustvl/PixelHacker), [arXiv](https://arxiv.org/abs/2504.20438) |

- RORem: with more high-quality pairs, the "removal success rate … escalated from 7.6% to 76.2%", and it improves success over previous methods "by more than 18%" — [RORem](https://arxiv.org/abs/2501.00740). The authors note that adding content-irrelevant prompts and CFG "further enhances removal performance" — [RORem repo](https://github.com/leeruibin/RORem)
- The runtime of the same model differs across papers. ObjectClear reports 1.63 s on A100 at 512 with 20 steps ([ObjectClear](https://arxiv.org/html/2505.22636)). TurboClear measures 2.29 s on A800 ([TurboClear](https://arxiv.org/html/2608.01288)). OSOR reports 6.44 s on its CORNE-Val setting ([OSOR](https://arxiv.org/html/2606.28094)).

**Base inpainting and editing models**
- **SDXL-Inpainting 0.1**: CreativeML **OpenRAIL++-M**. Trained at 1024² for 40K steps from SDXL base. About 3B parameters (per the model card). "When the strength parameter is set to 1 … the quality of the image is degraded". 15–30 steps recommended — [HF card](https://huggingface.co/diffusers/stable-diffusion-xl-1.0-inpainting-0.1)
- **FLUX.1 Fill [dev]**: 12B parameters, 50 steps, guidance 30. Known issues: "slight-color shifts in areas that are not filled in" and "lines at the edges of the filled-area" with complex textures. Licence: FLUX.1 [dev] Non-Commercial — [HF card](https://huggingface.co/black-forest-labs/FLUX.1-Fill-dev)
- **FLUX.1 Kontext [dev]**: 12B, instruction editor, FLUX.1 [dev] Non-Commercial licence — [HF card](https://huggingface.co/black-forest-labs/FLUX.1-Kontext-dev). In Replicate's anecdotal removal test, **Kontext [pro]** "left the two towers in place" — [Replicate, 2025-09-23](https://replicate.com/blog/compare-image-editing-models)
- **FLUX.2 [klein] 4B** (released 2026-01-15): **Apache-2.0**, 4B, step-distilled to 4 steps, about 13 GB VRAM, generation plus single- and multi-reference editing. The 9B variant is under the FLUX non-commercial licence and uses an 8B Qwen3 text embedder — [HF card](https://huggingface.co/black-forest-labs/FLUX.2-klein-4B), [BFL blog](https://bfl.ai/blog/flux2-klein-towards-interactive-visual-intelligence). Diffusers gained a `Flux2KleinInpaintPipeline` (mask input) after [issue #13005](https://github.com/huggingface/diffusers/issues/13005)
- **Qwen-Image-Edit-2511** (Dec 2025): **Apache-2.0**, 20B, 40 steps with true-CFG 4.0, multi-image input, **no mask input** — [HF card](https://huggingface.co/Qwen/Qwen-Image-Edit-2511). A 4-step Lightning LoRA is Apache-2.0 (≈10× fewer steps) — [lightx2v](https://huggingface.co/lightx2v/Qwen-Image-Edit-2511-Lightning). Community object-remover LoRAs exist (e.g. [QIE-2511-Object-Remover-v2](https://huggingface.co/prithivMLmods/QIE-2511-Object-Remover-v2)); I did not check their licences or data provenance. In Replicate's test, Qwen Image Edit and SeedEdit 3.0 were the "winners" at removal, at 2.9 s on hosted GPUs — [Replicate](https://replicate.com/blog/compare-image-editing-models)
- **Qwen-Image 2.1** (released 2026-09-20): a unified generate-and-edit model with a 7B DiT, which accepts masks and annotations for local edits. It has moved to the **Qwen Research License** (non-commercial) — [HF](https://huggingface.co/Qwen/Qwen-Image-2.1), [qwen-image-2.1-mlx](https://github.com/The-Focus-AI/qwen-image-2.1-mlx). **Supersedes** the Apache-licensed 2511 line in capability but not in licence.
- **Step1X-Edit**: Apache-2.0. On an H800 at 1024 with 28 steps: 22 s and **49.8 GB** peak; FP8 34 GB / 25 s; FP8 + CPU offload 18 GB / 51 s. v1.2 released 2025-11-26, and a lightweight v2.0 on 2026-04-29 claims <2 s — [repo](https://github.com/stepfun-ai/Step1X-Edit)
- **HiDream-E1**: MIT. Runs at 768², 28 steps, with Llama-3.1-8B-Instruct and T5-XXL encoders. EmuEdit "remove" score 5.99 — [HF README](https://huggingface.co/HiDream-ai/HiDream-E1-Full/blob/main/README.md)
- **LongCat-Image-Edit**: Apache-2.0, 6B, supports object add and remove — [HF](https://huggingface.co/meituan-longcat/LongCat-Image-Edit)
- **Z-Image-Edit** (Alibaba Tongyi): the code is Apache-2.0, but the Edit weights are still "to be released" — [Z-Image repo](https://github.com/Tongyi-MAI/Z-Image)

**Apple Silicon run reports (mostly secondary; chip noted)**
- FLUX.2 klein 4B, mflux 0.17.5, **M1 Max 64 GB**: 1024², 4 steps, 21.4 s inference / 31.7 s wall. 512², 4 steps, 12.0 s / 23.7 s. Reference-image editing about 52 s. Disk about 16 GB — [lilting.ch](https://lilting.ch/en/articles/flux2-klein-4b-mflux-iris-m1-max)
- FLUX.2 klein 4B 4-bit, **M4 Max**: about 17 s for 4-step 1024² including cold load (search snippet from the model card) — [Runpod/FLUX.2-klein-4B-mflux-4bit](https://huggingface.co/Runpod/FLUX.2-klein-4B-mflux-4bit)
- Qwen-Image-Edit-2509 Q8, mflux, **M1 Max 64 GB**: 4-step Lightning 2 min 28 s. 20 steps 10 min 44 s. On MPS, "BF16 is 2x slower than FP16" on M1–M3, because native BF16 "starts with M4" — [lilting.ch](https://lilting.ch/en/articles/comfyui-qwen-mps-bf16-slowdown)
- Qwen-Image 2.1, mflux 8-bit, **M4 Max 40-core / 64 GB**: 6.0 s per step at 1024 (about 130 s for 20 steps, about 30 GB). 3.1 s per step at 768 (25 GB). 2.2 s per step at 640 (22 GB). stable-diffusion.cpp: 12.5 s per step at 1024 with about 11 GB — [qwen-image-2.1-mlx](https://github.com/The-Focus-AI/qwen-image-2.1-mlx)
- **M4 Pro 24 GB**: FLUX 1024², 20 steps, about 50 s in Draw Things. SDXL 1024², 25 steps, about 20–40 s. MLX about 20% slower than Draw Things (blog) — [heyuan110](https://www.heyuan110.com/posts/ai/2026-02-15-mac-mini-local-image-generation/)
- Runtimes: stable-diffusion.cpp is MIT and ships **Rust bindings**. It uses GGUF quantisation and supports Metal, SD1.x/2.x/XL/3.x, FLUX.2 dev/klein, Qwen-Image, inpainting, LoRA and LCM — [sd.cpp](https://github.com/leejet/stable-diffusion.cpp). mflux (MLX) supports FLUX.1 Fill/Kontext, FLUX.2, Qwen Image and Z-Image — [mflux](https://github.com/filipstrand/mflux)

### Inferences
- M4 feasibility at a 512–1024 crop (**estimates**, scaled from the reports above; an M4 Pro/Max is assumed, and a base M4 would be about 2× slower):
  - SDXL-class one-step removers (TurboClear, OSOR-SDXL): about 1–2 s UNet plus about 1–3 s VAE encode/decode at 1024. **About 2–6 s per candidate**. Memory about 6–9 GB fp16, which fits 16 GB.
  - RORem-4S (4-step LCM): about **4–10 s**. ObjectClear (20 steps at 512): about 10–20 s.
  - FLUX.2 klein 4B (4 steps, quantised): about **15–30 s at 1024 and about 10–20 s at 768**, which fits 16–32 GB when quantised.
  - FLUX.1 Fill/Kontext 12B at 28–50 steps: about 70–125 s at 1024 on an M4 Pro. Outside budget, and non-commercial anyway.
  - Qwen-Image-Edit 20B, even at 4 steps: well over 60 s, and Q8 alone is about 20 GB of weights before the VL text encoder. Not credible on 16–32 GB within 30 s.
- Removal-specific models use a fixed prompt (OmniEraser uses "There is nothing here"; RORem uses content-irrelevant prompts), so the prompt embedding can be computed once and shipped. That drops the T5/Qwen3/CLIP text encoders from runtime memory. This is an engineering inference to test.
- Quality ranking implied by published tables: ObjectClear-family ≈ OSOR > OmniPaint > RORem ≈ OmniEraser > PowerPaint/SDXL-INP on effect-aware benchmarks. Generalist instruction editors (Qwen-Image-Edit, SeedEdit) do well anecdotally, but they regenerate the whole frame without a mask and are too heavy locally.
- Superseded work: SD1.5-era removers (PowerPaint v1, CLIPAway, SmartEraser, Attentive Eraser) are superseded by paired-data, effect-aware models. FLUX.1 Fill/Kontext is superseded by FLUX.2 (dev is non-commercial, klein 4B is Apache). Qwen-Image-Edit-2509 is superseded by 2511, and the 2.1 successor changed licence.

### Gaps
- There is no published M4 measurement of any SDXL inpainting or removal model at 1–4 steps. The estimates above must be measured.
- I did not confirm whether TurboClear's and FlashClear's released weights carry ObjectClear's S-Lab non-commercial terms. They are derived from ObjectClear, so this is likely but unverified.
- OSOR's claimed quality is on its own CORNE-Val, AnimeEraseBench and TextEraseBench. I found no independent OBER/RORD comparison against TurboClear.
- No FLUX.2 klein removal LoRA or model trained on effect-aware paired data was found.
- The licences and training data of the community Qwen object-remover LoRAs were not checked.

## 3. Shadows, reflections and other object effects: implicit vs explicit handling

### Takeaway
The evidence favours models that learn object-to-effect association from real counterfactual pairs (ObjectDrop, OmniEraser, ObjectClear, OSOR) over "detect the shadow, then dilate the mask, then inpaint". In ObjectClear's tables, baselines given ground-truth object-plus-effect masks still trail ObjectClear given only the object mask. Explicit masks help generic inpainters on some benchmarks but invite hallucination in larger holes. Instance shadow detectors (LISA, SSIS, SSISv2, FastInstShadow) exist, but the best are non-commercial, and none handle reflections. A pragmatic design is an implicit, effect-aware model plus a user-editable (or predicted) effect mask as a control.

### Cited Findings
**Counterfactual data**
- ObjectDrop (Google, ECCV 2024) photographed **2,500 scene pairs** with and without one object, fixed camera and lighting, plus 100 test pairs. It fine-tuned an SDXL-like internal inpainting model for 50K steps. Users preferred it for removal 64.1% of the time over Emu Edit and 86.5% over MGIE. The authors note that synthesising shadows and reflections is "inherently more complex" than removing them — [ObjectDrop arXiv](https://arxiv.org/html/2403.18818v1), [project](https://objectdrop.github.io/). I found no public release of code, weights or data.
- OmniEraser builds **Video4Removal** (>100,000 pairs) from fixed-camera video frames, with object masks that deliberately *exclude* effect regions so the model learns object-effect association. It feeds object and background latents as guidance and uses 28 steps on FLUX.1-dev at 1024. Its limitation: it "may alter elements unrelated to the target object, such as shadows from other objects or background color", because it avoids latent blending — [OmniEraser](https://arxiv.org/html/2501.07397v3). The dataset release was still "under active preparation" — [repo](https://github.com/PRIS-CV/Omnieraser)
- ObjectClear's **OBER** dataset mixes camera-captured and simulated data with object masks, object-effect masks and RGBA foregrounds: 37,994 training pairs, a 163-pair test set and OBER-Wild (302 internet images). It is gated and non-commercial — [ObjectClear repo](https://github.com/zjx0101/ObjectClear), [paper](https://arxiv.org/html/2505.22636). Adaptive Target-Aware Attention predicts the object-effect mask from cross-attention at inference, then Attention-Guided Fusion keeps the background — [ObjectClear](https://arxiv.org/html/2505.22636)
- OSOR's SAVP pipeline mines effect-aware supervision (difference heatmaps) from instruction triplets into **CORNE** (280K verified pairs). An alpha head predicts soft maps for "cast shadows, reflected appearances, and other environmental interactions" beyond the user mask — [OSOR](https://arxiv.org/html/2606.28094)
- RORD (BMVC 2022) extracts real pairs from video, but "its annotations include shadow regions", so users must manually specify effects when using such masks — [OmniEraser discussion of RORD](https://arxiv.org/html/2501.07397v3), [RORD poster](https://bmvc2022.mpi-inf.mpg.de/0542_poster.pdf)
- SmartEraser's copy-paste synthetic data (Syn4Removal, >1M triplets) lacks realistic shadows. The authors recommend mixing in RORD data "to compensate for object shadows gaps" — [SmartEraser](https://github.com/longtaojiang/SmartEraser)
- GeoRemover argues that "strictly mask-aligned training fail[s] to remove these casual effects which are not explicitly masked", while "loosely mask-aligned strategies … lack controllability and may unintentionally over-erase other objects". Its fix is to remove the object in depth and re-render RGB — [GeoRemover](https://github.com/buxiangzhiren/GeoRemover)

**Implicit vs explicit: numbers**
- ObjectClear Table 1, as extracted from the HTML (PSNR/LPIPS). On **OBER-Test**, giving baselines the object-plus-effect mask helps: RORem 24.51→**27.23**, PowerPaint 22.76→26.20, SDXL-INP 22.42→24.07. ObjectClear with only the object mask scores **33.04 / 0.0342**, and OmniPaint 29.06 / 0.0521. On **RORD-Val**, the extracted numbers show effect masks *lowering* baseline PSNR (RORem 22.49→21.61, PowerPaint 21.46→19.87). ObjectClear scores 26.24 / 0.1157 — [ObjectClear](https://arxiv.org/html/2505.22636)
- With ground-truth object-effect masks used for background blending (Table 7): SDXL-INP 21.67, PowerPaint 22.51, RORem 25.24, ObjectClear 26.24 PSNR. The paper says "even when using only the object mask, ObjectClear surpass[es] methods that rely on both object and effect masks". Larger holes make generic inpainters "generate new objects within the masked regions" — [ObjectClear](https://arxiv.org/html/2505.22636)
- In the user study, ObjectClear was preferred over PowerPaint, RORem, OmniEraser and OmniPaint under both mask conditions — [ObjectClear](https://arxiv.org/html/2505.22636)
- MetaShadow (authors associated with Adobe Research) frames "holistic" diffusion removal as handling shadows "in an implicit manner, forfeiting any controllability on the objects' effects". It proposes object-centred shadow detection, removal and synthesis — [MetaShadow](https://arxiv.org/abs/2412.02635). I found no code or weights; the paper is CC BY-NC-SA 4.0.
- PredErase is an explicit, heuristic example. It takes the floor-contact segment from the lowest rows of the object mask and grows a one-sided band upward (σ = max(6, 0.5·h_obj), δx = max(8, 0.35·w_obj), plus 4-px dilation) as the editable region. It targets "upright, ground-contacted objects", with no light-source estimation — [PredErase](https://arxiv.org/html/2609.00956)

**Instance shadow detection (explicit route)**
- LISA / "Instance Shadow Detection" (CVPR 2020) introduced SOBA: 3,623 shadow-object pairs in 1,000 photos — [DeepAI summary](https://deepai.org/publication/instance-shadow-detection)
- SOBA-testing SOAP: LISA 23.5, SSIS 30.2, SSISv2 35.3, **FastInstShadow 38.7** (mask AP 53.8). FastInstShadow runs at 17.9 fps on an RTX 3090 vs SSISv2 at 6.5 fps. It does not address reflections — [FastInstShadow](https://arxiv.org/html/2503.07517)
- SSIS/SSISv2 are built on AdelaiDet, "for non-commercial purposes"; commercial use requires contacting the authors — [SSIS repo](https://github.com/stevewongv/SSIS). The GitHub API returned "Not Found" for the FastInstShadow repo URL given in the paper (`wlotkr/FastInstShadow`). The only licence seen was the arXiv paper's CC BY 4.0, which covers the paper, not the code or weights — [FastInstShadow](https://arxiv.org/html/2503.07517)

### Inferences
- For Luxforge, an **explicit shadow detector is a weak primary strategy**. Candidates are licence-blocked (SSIS) or unclear (FastInstShadow), cover shadows only (not reflections, contact darkening or colour bleed), and enlarging the hole makes generic inpainters hallucinate more. It remains useful as a **suggestion**: pre-select the shadow as an editable "effect" region the user can accept or trim, which fits Luxforge's manual brush/add/subtract first interaction.
- The best quality comes from **effect-aware models given only the object mask** (ObjectClear, OSOR, TurboClear). Their predicted effect or alpha mask should be exposed and stored with the patch for transparency and for compositing: the patch then legitimately extends beyond the user's object mask. That affects Luxforge's "composited only inside the mask" rule, because the composite mask would become user mask ∪ accepted predicted effect mask. **This is an owner decision.**
- With a feed-forward baseline (LaMa), shadows need explicit user painting. A cheap PredErase-style contact band or a shadow-detector suggestion could be offered as mask assistance, not as the removal model itself.
- Reflections (water, glass, glossy floors) are covered only by the learned, paired-data models (Video4Removal, OBER, CORNE). No detector route was found.

### Gaps
- No public ObjectDrop code, weights or data found. Google's model is internal.
- There is no licence-clean (Apache/MIT) effect-aware removal model with licence-clean weights. OSOR-SDXL (OpenRAIL++-M) is the closest.
- I could not verify from the PDF whether ObjectClear's RORD-Val "object-effect" columns use RORD's shadow-inclusive masks. That would explain why effect masks hurt baselines there.
- MetaShadow and SAM-based shadow adapters: release status and licences not established.

## 4. High-resolution strategy for 24–60 MP photos; grain, colour/linear light and seams

### Takeaway
Everyone crops around the mask, runs the model at its native size (512–2048), and composites back only inside the mask. IOPaint's defaults are a 128-px margin, an 800-px trigger and a 1280-px resize limit. Adobe's Generative Remove returns patches of at most 2048 px, and users report softness and poor grain matching for large or noisy areas. For 24–60 MP, the model output should supply low-frequency structure. High frequencies (texture, noise, grain) should come from the image itself via residual transfer, noise re-synthesis or SR, with multiband blending across a feathered boundary. The model must see an sRGB-like, display-referred encoding, not scene-linear float.

### Cited Findings
**How existing tools do it**
- IOPaint `HDStrategy`: ORIGINAL ("Use original image size"), RESIZE (downscale the longer side to `hd_strategy_resize_limit`, default **1280**) and CROP (default strategy). CROP crops each mask box plus `hd_strategy_crop_margin` (default **128 px**) when the longer image side exceeds `hd_strategy_crop_trigger_size` (default **800**). It applies to "erase models" only. Diffusion defaults are `sd_mask_blur` 11, `sd_keep_unmasked_area` True, `sd_match_histograms` False, `sd_seed` 42 and `sd_steps` 50 — [IOPaint schema.py](https://raw.githubusercontent.com/Sanster/IOPaint/main/iopaint/schema.py)
- IOPaint's implementation: the crop box is centred on the mask box, and the width is box + 2×margin, shifted to stay inside the image. Results are pasted back into the crop rectangle. RESIZE restores original pixels wherever `mask < 127`. Inputs are padded to a multiple of 8. For SD models the result is blended `result*mask + image*(1-mask)` — [IOPaint base.py](https://github.com/Sanster/IOPaint/blob/main/iopaint/model/base.py)
- Diffusers inpainting pipelines expose `padding_mask_crop`: crop an expanded area around the mask, inpaint, then resize back. This is noted for the FLUX.2 klein inpaint pipeline — [diffusers issue #13005](https://github.com/huggingface/diffusers/issues/13005)
- Lightroom Generative Remove is powered by the Firefly Image 1 model in the cloud and offers "a few variations" — [Adobe blog, 2024-05-21](https://blog.adobe.com/en/publish/2024/05/21/lightroom-introduces-power-adobe-firefly-with-generative-remove). An Adobe employee (Rikk Flohr) "Confirmed at 2048 px", and Victoria Bampton reports Adobe confirmed outputs "are a maximum of 2048px x 2048px" (2024-07-08). One expert hypothesises a 1024 generation upscaled server-side to 2048 (unconfirmed) — [Lightroom Queen forum](https://www.lightroomqueen.com/community/threads/resolution-with-generative-remove.50557/)
- Photoshop's newer Firefly Fill & Expand model doubled output from 1024² to 2048² — [Fstoppers](https://fstoppers.com/photoshop/photoshop-generative-fill-update-firefly-fill-and-expand-gets-real-improvements-900201) (secondary)
- User reports of Lightroom Generative Remove: an inability "to match the noise and grain in the replacement patch", with Denoise suggested first — [Lightroom Queen](https://www.lightroomqueen.com/community/threads/happy-with-ai-generative-remove-but-just-discovered-it-is-low-res-ways-to-fix.52400/). It "often doesn't do a good job of matching tone and noise/grain for relatively uniform areas", showing a regular pattern at 200% (LrC 14.4, 2025-07-21) — [Adobe community](https://community.adobe.com/t5/lightroom-classic-discussions/more-noise-after-using-generative-ai-remove-on-a-raw-file-with-denoise-14-4/td-p/15422951)

**Resolution-transfer techniques**
- HiFill/CRA: predict at low resolution, then add high-frequency residuals aggregated from context patches using attention weights. This gives 8K inpainting from a 512-trained network — [arXiv 2005.09704](https://arxiv.org/abs/2005.09704)
- LaMa multiscale feature refinement uses coarse-to-fine inference-time optimisation — [arXiv 2206.13644](https://arxiv.org/abs/2206.13644)
- Patch-Adapter (ICCV 2025) adds a dual-context adapter at reduced resolution for global coherence, then a reference-patch adapter for full-resolution patch-level inpainting on top of SDXL-inpainting, reaching **4K+** — [arXiv 2510.13419](https://arxiv.org/abs/2510.13419)
- CoordFill and RETHINED are resolution-agnostic feed-forward designs at 2K–8K (see question 1) — [CoordFill](https://arxiv.org/abs/2303.08524), [RETHINED](https://arxiv.org/html/2503.14757)
- Blending references: the Laplacian-pyramid "multiresolution spline" blends each frequency band over a transition zone matched to that band — [Burt & Adelson 1983](https://dl.acm.org/doi/10.1145/245.247). Poisson/gradient-domain seamless cloning — [Pérez et al. 2003](https://www.cs.jhu.edu/~misha/Fall07/Papers/Perez03.pdf)
- Noise re-synthesis precedent (AV1): denoise, estimate grain parameters from (input − denoised) over flat blocks, model the grain as a 2D autoregressive process driven by Gaussian noise (lag 0–3) with intensity-dependent scaling, and synthesise it back — [Norkin & Birkbeck, DCC 2018](https://norkin.org/pdf/DCC_2018_AV1_film_grain.pdf)

**Colour, VAE and linear-light issues**
- FLUX.1 Fill: "slight-color shifts in areas that are not filled in"; "lines at the edges of the filled-area" with complex textures — [HF card](https://huggingface.co/black-forest-labs/FLUX.1-Fill-dev)
- SD-Inpaint downscales the mask to latent resolution, which "can cause some inconsistencies and result in artifacts"; the authors recommend dilating masks — [CLIPAway](https://github.com/YigitEkin/CLIPAway)
- ASUKA (2026) targets diffusion and rectified-flow inpainting's "unwanted object insertion" and "color inconsistency". It uses reconstruction priors plus a VAE decoder reformulated as local harmonisation — [arXiv 2601.15368](https://arxiv.org/abs/2601.15368)
- Linear HDR input to LDR-trained latent models: logarithmic encoding maps HDR into "a distribution that is naturally aligned with the latent space", so LDR-trained VAEs can be reused. "HDR … can be handled effectively without redesigning generative models, provided that the representation is chosen to align with their learned priors" — [arXiv 2604.11788](https://arxiv.org/abs/2604.11788)
- Luxforge-specific context: repairs are proposed after source development and before global colour, in content coordinates, as immutable patches — [corrections design](../../design/corrections.md)

### Inferences
Proposed pipeline for 24–60 MP (**inference; to validate by measurement**):
1. **Crop:** take the mask bbox (the object mask ∪ any effect mask) plus a context margin proportional to the mask. IOPaint's fixed 128 px is too small at 60 MP; about 0.5–1× the bbox size, clamped to the image, is a starting point. If the crop exceeds the model's native size (512 for ObjectClear/RORem/LaMa-optimal, 1024 for SDXL/FLUX-class), downscale it to that size. Very large masks need either a global pass (RESIZE) or tiling with overlap.
2. **Encode for the model:** convert the scene-linear float crop to a display-referred, sRGB-encoded [0,1] image with a fixed, recorded, invertible transform: exposure normalisation for the crop plus the sRGB OETF, or a log curve for highlight-heavy crops. Run the model, then invert the same transform. Keep original out-of-range highlights outside the mask, and clamp or fit the generated values.
3. **Resolution transfer:** upsample the model output to crop resolution and use it **only as low/mid frequencies**. Rebuild the high-frequency band from context: CRA-style residual transfer, patch-based texture synthesis, or an SR model on the patch. Then **re-synthesise noise**: estimate luminance- and colour-noise statistics (and grain, per level) from flat regions in the ring around the mask on the same raw-developed prefix, then add matched synthetic noise (AV1-style AR model) to the filled area. This targets the grain mismatch users report in Lightroom.
4. **Colour/tone match:** match low-frequency mean or histogram in the boundary ring, as in IOPaint's `sd_match_histograms`, to cancel VAE colour drift.
5. **Composite:** blend with a Laplacian pyramid inside a feathered mask. Pixels outside the (object ∪ accepted effect) mask stay bit-identical, which Luxforge requires and IOPaint and diffusers already do.
- Adobe's 2048-px cap plus reported softness and grain mismatch set a beatable bar. A 60 MP frame has about 9,500 px on the long side, so a person-sized removal can exceed 2048 px, and step 3 is where local quality is won or lost.
- Tiled diffusion over a whole high-resolution crop is costly on an M4. Prefer "one low-res generative pass + deterministic detail transfer" over "diffusion at full resolution".

### Gaps
- There is no public documentation of how Adobe composites, upsamples or grain-matches Generative Remove patches, or of any linear-light handling. Only forum statements were found.
- I found no paper that evaluates generative removal on RAW or scene-linear data, or noise re-synthesis specifically for inpainting patches. Luxforge would be the first to measure it.
- Patch-Adapter's code and licence and its runtime for 4K were not verified.

## 5. Determinism, seeds and the cost of multiple candidates

### Takeaway
Feed-forward inpainters (LaMa, MI-GAN, CoordFill) are deterministic and produce exactly one answer. "Variations" need input perturbation (mask dilation, crop context), or pluralistic models such as MAT (non-commercial). Diffusion and flow models give variations through seeds. Reproducing a result exactly across hardware or library versions is not guaranteed even with the same seed, so the immutable stored patch, not a regeneration recipe, must be the source of truth. Three candidates cost about 3× the denoising work. For one-step models (TurboClear, OSOR) that is negligible next to fixed costs such as VAE and encoders.

### Cited Findings
- Diffusers recommends a CPU `torch.Generator` for reproducibility. The GPU uses a different RNG, and `randn_tensor()` creates noise on the CPU and moves it. The guide warns: "You can try to limit randomness, but it is not *guaranteed* even with an identical seed". Deterministic algorithms may be slower — [diffusers reproducibility](https://huggingface.co/docs/diffusers/using-diffusers/reusing_seeds)
- IOPaint's diffusion default is `sd_seed` 42, where -1 means random — [IOPaint schema](https://raw.githubusercontent.com/Sanster/IOPaint/main/iopaint/schema.py)
- MAT supports pluralistic generation with "high fidelity and diversity" (CC BY-NC) — [MAT](https://github.com/fenglinglwb/MAT)
- Lightroom Generative Remove gives "a few variations to choose from" — [Adobe blog](https://blog.adobe.com/en/publish/2024/05/21/lightroom-introduces-power-adobe-firefly-with-generative-remove)
- Per-candidate costs (hosted GPU): TurboClear 0.041 s (A800, 512). OSOR 0.42 s (SDXL) or 0.80 s (FLUX) on A100 at up to 1024. ObjectClear 1.63–2.29 s. OmniPaint 9.4–20.3 s — [TurboClear](https://arxiv.org/html/2608.01288), [OSOR](https://arxiv.org/html/2606.28094), [ObjectClear](https://arxiv.org/html/2505.22636)
- On Apple Silicon, the FLUX.2 klein 4B wall time (31.7 s) is much larger than its inference time (21.4 s) at 1024 on an M1 Max, because loading dominates — [lilting.ch](https://lilting.ch/en/articles/flux2-klein-4b-mflux-iris-m1-max)
- MPS precision pitfalls: FP16 attention NaNs since macOS 14.5 in PyTorch MPS, and BF16 emulated on M1–M3 — [lilting.ch](https://lilting.ch/en/articles/comfyui-qwen-mps-bf16-slowdown)

### Inferences
- Record provenance per candidate: model ID, weight hashes, runtime and version, precision, seed, steps, sampler, prompt-embedding hash, crop geometry and encoding transform. This makes a result auditable, but Luxforge should not promise bit-exact regeneration across machines. It already stores the chosen patch as an immutable artifact, which is the right design.
- Cost of 3 candidates (**estimate**, M4 Pro): SDXL one-step at 1024 is about 2–6 s for one and about 4–10 s for three (shared VAE encode, text/image-encoder and context features; three UNet passes and three decodes). A batch of 3 raises peak memory. Sequential runs are safer at 16 GB. RORem-4S: about 8–25 s for three. FLUX.2 klein 4B: about 45–75 s for three at 1024, but about 25–45 s at 768. LaMa: variations are only from mask or context jitter, at under 1 s each.
- Keep the model resident between candidates, and keep a warm process for the session, because cold load dominates on Apple Silicon.

### Gaps
- No measurement of cross-run or cross-machine determinism for MLX, Core ML or sd.cpp on the M4 was found.
- No source states how many variations Lightroom generates per click. "A few" is Adobe's wording.

## 6. Evaluation: benchmarks, metrics and who leads in 2026

### Takeaway
There is no neutral leaderboard. Each paper evaluates on its own mix of RORD-Val (344 real pairs), RemovalBench (70 pairs), OBER-Test (163) and OBER-Wild (302), MULAN, DEFACTO-Val and newer sets (CORNE-Val, AnimeEraseBench and TextEraseBench), using PSNR/PSNR-BG, LPIPS, FID/CMMD, CLIP/DINO distances, ReMOVE/ReMOVE+ and small user studies. On these, ObjectClear (CVPR 2026) and its distilled descendants lead academic effect-aware removal. OSOR (ECCV 2026) claims to beat ObjectClear on its own benchmark. OmniPaint and OmniEraser follow. Metrics correlate only moderately with people: ReMOVE agreed with human preference 74.7% of the time. Luxforge should build its own paired, RAW, rights-cleared test set.

### Cited Findings
- **RORD-Val**: 344 images from the RORD validation set with unique scenes and objects. **RemovalBench**: 70 pairs — [OmniEraser](https://arxiv.org/html/2501.07397v3), [RemovalBench on HF](https://huggingface.co/datasets/BaiLing/RemovalBench)
- **OBER**: a 163-pair test set with object and object-effect masks, plus OBER-Wild with 302 images, released under a non-commercial licence — [ObjectClear repo](https://github.com/zjx0101/ObjectClear)
- **ReMOVE** (CVPRW 2024) is reference-free. It computes the cosine similarity between mean ViT (segmentation-pretrained) patch embeddings inside vs outside the mask, at 1024². It agreed with users 74.7% of the time vs LPIPS 71.9%, and distinguishes removal from *replacement* — [ReMOVE](https://arxiv.org/html/2409.00707v1), [code](https://github.com/chandrasekaraditya/ReMOVE). ObjectClear's **ReMOVE+** compares the output's object-effect region with the input background — [ObjectClear](https://github.com/zjx0101/ObjectClear). My inference: plain ReMOVE splits features only by the object mask ([ReMOVE](https://arxiv.org/html/2409.00707v1)), so it cannot see a shadow left outside that mask. A search snippet also said ReMOVE "lacks the necessary sensitivity" for multi-object erasure, but I could not confirm which 2026 paper says so. Treat that as unverified.
- OmniPaint proposes a reference-free **CFD** metric for context consistency and object hallucination — [OmniPaint](https://arxiv.org/abs/2503.08677)
- Headline numbers:
  - **OmniEraser**. RemovalBench: FID 39.52, CMMD 0.208, LPIPS 0.133, PSNR 21.11. RORD-Val: FID 43.71, LPIPS 0.166, PSNR 22.13. Beats Attentive Eraser — [OmniEraser](https://arxiv.org/html/2501.07397v3)
  - **ObjectClear**. OBER-Test: PSNR 33.04, PSNR-BG 35.62, LPIPS 0.0342. RORD-Val: PSNR 26.24, LPIPS 0.1157. Beats SDXL-INP, PowerPaint, BrushNet, DesignEdit, CLIPAway, FreeCompose, Attentive Eraser, RORem, OmniEraser, GeoRemover and OmniPaint. MULAN: PSNR 24.89, LPIPS 0.1586 — [ObjectClear](https://arxiv.org/html/2505.22636)
  - **TurboClear vs ObjectClear, as measured by TurboClear**. OBER-Test: LPIPS 0.0286 vs 0.0380, PSNR 34.93 vs 32.06. RORD-Val at 960×540: LPIPS 0.0627 vs 0.0717, PSNR 28.29 vs 27.35 — [TurboClear](https://arxiv.org/html/2608.01288). Note that ObjectClear's self-reported OBER numbers (33.04 / 0.0342) differ from TurboClear's reproduction.
  - **OSOR-FLUX vs ObjectClear on CORNE-Val**: FID 12.52 vs 22.38, LPIPS 0.046 vs 0.098, PSNR 32.19 vs 29.01, 0.80 s vs 6.44 s — [OSOR](https://arxiv.org/html/2606.28094)
  - **PredErase (training-free)** on RemovalBench: CMMD 0.108, PSNR 24.36, FID 52.69 (vs OmniEraser 39.52). On RORD-Val it has the best LPIPS/PSNR in its table. "Supervised removers maintain stronger full-image appearance scores" — [PredErase](https://arxiv.org/html/2609.00956)
  - **ObjectDrop** user study: 64.1% vs Emu Edit and 86.5% vs MGIE — [ObjectDrop](https://arxiv.org/html/2403.18818v1)
- Generalist editors: in an anecdotal Replicate test (2025-09-23), SeedEdit 3.0 and Qwen Image Edit did best, Nano Banana removed the object but changed the background hills, and FLUX.1 Kontext [pro] failed — [Replicate](https://replicate.com/blog/compare-image-editing-models). HiDream-E1 scores 5.99 on EmuEdit "remove" — [HiDream-E1](https://huggingface.co/HiDream-ai/HiDream-E1-Full/blob/main/README.md)
- Video removal benchmarks are emerging (PROVE, ACM MM 2026, with RC metrics claiming "substantially stronger alignment with human judgments") but are out of scope for stills — [PROVE](https://arxiv.org/abs/2605.14534)

### Inferences
- Status as of September 2026: **ObjectClear-family** (ObjectClear, TurboClear, FlashClear) and **OSOR** are the academic leaders for object-plus-effect removal from an object mask. **OmniPaint** and **OmniEraser** are the FLUX-based runners-up. **RORem** is the strongest permissively tagged SDXL remover without explicit effect modelling. PredErase shows training-free effect removal on Apache FLUX.2 klein is viable but trails supervised models.
- A Luxforge evaluation set should mirror ObjectDrop and RORD. Tripod-captured with/without-object pairs on the owner's own cameras (Nikon Z6, X100VI, DJI Air 2S), shot in RAW, give rights-cleared ground truth. They also test the linear-light pipeline, grain matching at 24–60 MP, shadows (hard sun, soft overcast), reflections (water, glass, floors) and structured backgrounds. Score with masked or ring PSNR/LPIPS against ground truth, an effect-region PSNR (difference mask minus object mask, as OSOR and ObjectClear do), ReMOVE/ReMOVE+ for unpaired shots, pixels changed outside the mask (must be 0), and a blinded owner preference test.

### Gaps
- No independent third-party benchmark compares ObjectClear, OSOR, TurboClear, LaMa, FLUX.2 klein and commercial tools (Adobe, Apple Clean Up, Google Magic Eraser) on the same data.
- The RemovalBench and RORD licences were not verified. OBER is non-commercial.
- No benchmark tests 24–60 MP inputs, RAW data or grain consistency.

## 7. Licensing: code vs weights, data provenance, compatibility with a GPL-3.0-or-later app

### Takeaway
Only a few candidates are fully permissive in both code and weights: **LaMa** (Apache/Apache, Places data caveat), **MI-GAN** (MIT/MIT, Places data caveat), **FLUX.2 klein 4B** (Apache weights, with removal ability still to be built), **Qwen-Image-Edit-2511**, **Step1X-Edit** and **LongCat-Image-Edit** (Apache, but heavy). SD1.5 and SDXL derivatives (RORem, PowerPaint, OSOR-SDXL, BrushNet, SmartEraser, Attentive Eraser) inherit OpenRAIL-M or OpenRAIL++-M use restrictions whatever their repo licence says. ObjectClear and its derivatives are S-Lab non-commercial. Everything on FLUX.1 dev/Fill/Kontext, FLUX.2 dev or klein 9B, Qwen-Image 2.1, MAT, FcF (NVIDIA NC code), SSIS and I-JEPA is non-commercial or research-only.

### Cited Findings
| Model | Code licence | Weights licence | Flag | Source |
|---|---|---|---|---|
| Big-LaMa | Apache-2.0 | Apache-2.0 (HF mirror) | Places data terms | [repo](https://github.com/advimman/lama), [HF](https://huggingface.co/smartywu/big-lama) |
| MI-GAN | MIT | MIT (LICENSE-WEIGHTS) | Places2/FFHQ data | [repo](https://github.com/Picsart-AI-Research/MI-GAN) |
| MAT | CC BY-NC 4.0 | "research purposes only" | **NC** | [LICENSE](https://raw.githubusercontent.com/fenglinglwb/MAT/main/LICENSE) |
| FcF | Apache-2.0 + NVIDIA Source Code License parts | not separate | **NC code component** ("research or evaluation purposes only") | [LICENSE](https://raw.githubusercontent.com/SHI-Labs/FcF-Inpainting/main/LICENSE) |
| ZITS / ZITS++ | Apache-2.0 | not separately stated | HR-Flickr data under Flickr terms | [ZITS](https://github.com/DQiaole/ZITS_inpainting), [ZITS++](https://github.com/ewrfcas/ZITS-PlusPlus) |
| CoordFill | BSD-3-Clause | not stated (Google Drive) | clarify weights | [repo](https://github.com/NiFangBaAGe/CoordFill) |
| RETHINED | none detected | not stated | clarify | [repo](https://github.com/CrisalixSA/rethined) |
| SDXL-Inpainting 0.1 | — | CreativeML OpenRAIL++-M | use-based restrictions | [HF](https://huggingface.co/diffusers/stable-diffusion-xl-1.0-inpainting-0.1) |
| RORem | Apache-2.0 | HF tag apache-2.0 (SDXL-inpainting derivative) | OpenRAIL++-M base terms likely still apply | [repo](https://github.com/leeruibin/RORem), [HF](https://huggingface.co/LetsThink/RORem) |
| OSOR | MIT | SDXL variant OpenRAIL++-M; FLUX variant FLUX.1-dev NC | CORNE data Apache-2.0 | [repo](https://github.com/Zhouqm-Git/osor) |
| ObjectClear | S-Lab License 1.0 | same | **NC**; OBER NC | [LICENSE](https://raw.githubusercontent.com/zjx0101/ObjectClear/main/LICENSE) |
| TurboClear | Apache-2.0 | third-party terms retained (ObjectClear-derived) | likely **NC** | [repo](https://github.com/GuoCalix/TurboClear) |
| OmniEraser | none detected | HF tag apache-2.0, but a FLUX.1-dev LoRA | **NC base** | [HF](https://huggingface.co/theSure/Omnieraser) |
| OmniPaint, GeoRemover | none detected | FLUX.1-dev / Fill-dev based | **NC base** | [OmniPaint](https://github.com/yeates/OmniPaint), [GeoRemover](https://github.com/buxiangzhiren/GeoRemover) |
| PredErase | MIT | FLUX.2 klein 4B Apache + I-JEPA CC BY-NC 4.0 | **NC component** | [repo](https://github.com/xiuwk0820/PredErase), [I-JEPA](https://github.com/facebookresearch/ijepa) |
| PowerPaint | MIT | v2-1 HF tag apache-2.0 on an SD1.5 base | OpenRAIL-M base | [repo](https://github.com/open-mmlab/PowerPaint), [HF](https://huggingface.co/JunhaoZhuang/PowerPaint-v2-1) |
| BrushNet | Apache-2.0 except third-party | not stated | SD base terms | [repo](https://github.com/TencentARC/BrushNet) |
| CLIPAway / SmartEraser | MIT / MIT | not stated | SD1.5 base terms | [CLIPAway](https://github.com/YigitEkin/CLIPAway), [SmartEraser](https://github.com/longtaojiang/SmartEraser) |
| Attentive Eraser | Apache-2.0 | uses SDXL/SD2.1 | OpenRAIL++ | [repo](https://github.com/Anonym0u3/AttentiveEraser) |
| FLUX.1 Fill / Kontext [dev] | flux repo Apache-2.0 | FLUX.1 [dev] Non-Commercial | **NC** | [licence](https://huggingface.co/black-forest-labs/FLUX.1-dev/blob/main/LICENSE.md) |
| FLUX.2 klein 4B / 9B | flux2 repo Apache-2.0 | 4B **Apache-2.0**; 9B FLUX NCL | 9B **NC** | [4B](https://huggingface.co/black-forest-labs/FLUX.2-klein-4B), [blog](https://bfl.ai/blog/flux2-klein-towards-interactive-visual-intelligence) |
| Qwen-Image-Edit-2511 (+ Lightning LoRA) | Apache-2.0 | Apache-2.0 | heavy (20B) | [HF](https://huggingface.co/Qwen/Qwen-Image-Edit-2511), [Lightning](https://huggingface.co/lightx2v/Qwen-Image-Edit-2511-Lightning) |
| Qwen-Image 2.1 | — | Qwen Research License | **NC/research** | [HF](https://huggingface.co/Qwen/Qwen-Image-2.1) |
| Step1X-Edit | Apache-2.0 | Apache-2.0 | heavy | [repo](https://github.com/stepfun-ai/Step1X-Edit) |
| HiDream-E1 | MIT | MIT | uses the Llama-3.1-8B encoder | [HF](https://huggingface.co/HiDream-ai/HiDream-E1-Full/blob/main/README.md) |
| LongCat-Image-Edit | Apache-2.0 | Apache-2.0 | 6B | [HF](https://huggingface.co/meituan-longcat/LongCat-Image-Edit) |
| SSIS / SSISv2 | AdelaiDet non-commercial | same | **NC** | [repo](https://github.com/stevewongv/SSIS) |

- What the non-permissive terms restrict:
  - **FLUX.1 [dev] NC**: the model and Derivatives (fine-tunes, LoRAs) may be used or distributed only "for Non-Commercial Purposes". Outputs "for any purpose (including for commercial purposes)" except training competing models. Content filtering or output review is required. BFL can terminate at any time — [FLUX.1 dev licence](https://huggingface.co/black-forest-labs/FLUX.1-dev/blob/main/LICENSE.md)
  - **OpenRAIL(++)-M**: use-based restrictions (Attachment A) that every downstream licence of the model or its derivatives must carry — [HF OpenRAIL blog](https://huggingface.co/blog/open_rail), [RAIL FAQ](https://www.licenses.ai/faq-2)
  - **S-Lab 1.0**: redistribution and use "for non-commercial purpose"; commercial use requires contacting the authors — [ObjectClear LICENSE](https://raw.githubusercontent.com/zjx0101/ObjectClear/main/LICENSE)
  - **NVIDIA Source Code License (StyleGAN2-ADA)**: "non-commercially … means for research or evaluation purposes only" — [FcF LICENSE](https://raw.githubusercontent.com/SHI-Labs/FcF-Inpainting/main/LICENSE)
  - **Qwen Research License**: research and evaluation only; commercial use needs a separate licence — [qwen-image-2.1-mlx](https://github.com/The-Focus-AI/qwen-image-2.1-mlx), [explainer](https://locallyuncensored.com/blog/qwen-image-2-1-explained.html)
- Data provenance:
  - Places365 images: "only for non-commercial research and educational purposes", and "You will NOT distribute the above images". This is the training set of LaMa, MI-GAN, MAT, FcF and ZITS — [Places2 terms](http://places2.csail.mit.edu/download-private.html)
  - LAION-5B, used for SD 1.5 and so the ancestor of SD-based removers, contained at least 1,008 verified CSAM items, which led to Re-LAION — [Stanford FSI](https://cyber.fsi.stanford.edu/news/investigation-finds-ai-image-generation-models-trained-child-abuse)
  - SmartEraser does not redistribute Syn4Removal because of source-image copyright (OpenImages v7, SAM, COCONut) — [SmartEraser](https://github.com/longtaojiang/SmartEraser)
  - OBER is gated and non-commercial — [ObjectClear](https://github.com/zjx0101/ObjectClear)
  - CORNE is Apache-2.0 but was mined from instruction triplets of unstated origin — [OSOR repo](https://github.com/Zhouqm-Git/osor)
- The FSF says Apache-2.0 is "compatible with version 3 of the GNU GPL" but not with GPLv2. Expat (MIT) and Modified BSD are "lax, permissive … compatible with the GNU GPL". It classes CC BY-NC as nonfree — [GNU licence list](https://www.gnu.org/licenses/license-list.html)

### Inferences
- A GPL-3.0-or-later app can **download** Apache-2.0 or MIT weights (LaMa, MI-GAN, FLUX.2 klein 4B, Qwen-Image-Edit-2511) at the user's request without licence friction.
- OpenRAIL++-M weights (RORem, OSOR-SDXL, SDXL-inpainting) are not OSI or FSF licences. Luxforge could still fetch them on explicit user action as a separately licensed download, with the use restrictions shown. Whether that meets Luxforge's "open-source dependencies" pillar is **an owner decision, not a licence incompatibility with the GPL code itself** (inference; not legal advice).
- Non-commercial weights (FLUX.1-dev family, ObjectClear family, Qwen-Image 2.1, MAT) should be treated as **evaluation references or "user-run local service" providers only**, never a bundled or default download.
- The Places2 "non-commercial research" data terms are a provenance concern for every classical inpainter, LaMa included. The weight licences themselves are permissive, but the owner should record this caveat when choosing LaMa or MI-GAN.
- "HF tag apache-2.0" on a LoRA or fine-tune of a restricted base (OmniEraser, RORem, PowerPaint v2-1) does not remove the base's terms. Evaluate the whole weight chain.

### Gaps
- The Stability AI Community License was not examined. None of the shortlisted removal models use SD3/3.5, so it does not currently bind a candidate.
- HiDream-E1 relies on Llama-3.1-8B-Instruct, whose own licence terms I did not verify here.
- The weight licences of CoordFill, the ZITS weights, the SmartEraser weights and the BrushNet checkpoints are unstated.
- The big-lama licence is known only via a mirror card.

## 8. Synthesis: candidates for (1) a fast permissive baseline and (2) a higher-quality generative option

### Takeaway
(1) **Baseline: Big-LaMa** (Apache/Apache, 51M, about 200 MB, deterministic, Core ML/ONNX-ready), with MI-GAN (MIT/MIT, 6M) as the lighter fallback. Wrap either in a crop, detail-transfer and noise re-synthesis pipeline, and have the user paint shadows. (2) **Generative, effect-aware:** no option is at once top quality, permissive and M4-fast. The realistic local choices are **OSOR-SDXL** or **RORem-4S** (SDXL-inpainting derivatives: fast, but OpenRAIL++-M), or a **FLUX.2 klein 4B (Apache)** removal path that would need fine-tuning on permissive effect-aware pairs. **ObjectClear/TurboClear** is the quality reference to benchmark against but cannot ship (S-Lab NC).

### Cited Findings
- Big-LaMa: 51M parameters, 256-px training, coherent at 1536², Apache-2.0 code — [LaMa](https://arxiv.org/pdf/2109.07161), [repo](https://github.com/advimman/lama). Core ML port runs on the macOS GPU — [CoreMLaMa](https://github.com/mallman/CoreMLaMa)
- MI-GAN: 5.95M parameters, 1.2 s at 2048² on an iPad Pro M2, MIT weights — [RETHINED](https://arxiv.org/html/2503.14757), [MI-GAN](https://github.com/Picsart-AI-Research/MI-GAN)
- OSOR-SDXL: one step, 0.42 s on A100 at up to 1024, effect-aware alpha head, MIT code, OpenRAIL++-M weights, ECCV 2026 — [OSOR](https://arxiv.org/html/2606.28094), [repo](https://github.com/Zhouqm-Git/osor)
- RORem: 4 steps, <1 s on GPU, 76.2% success rate, Apache code and weights tag, SDXL-inpainting based — [RORem](https://arxiv.org/abs/2501.00740), [HF](https://huggingface.co/LetsThink/RORem)
- FLUX.2 klein 4B: Apache-2.0, 4 steps, about 13 GB VRAM, about 17 s at 1024 on an M4 Max (4-bit, including load), and a diffusers inpaint pipeline exists — [HF](https://huggingface.co/black-forest-labs/FLUX.2-klein-4B), [Runpod card](https://huggingface.co/Runpod/FLUX.2-klein-4B-mflux-4bit), [diffusers #13005](https://github.com/huggingface/diffusers/issues/13005). Training-free effect removal on klein works (PredErase) but depends on an NC I-JEPA component — [PredErase](https://arxiv.org/html/2609.00956)
- ObjectClear / TurboClear: the quality leaders; TurboClear is one step at 0.04 s on A800; non-commercial — [TurboClear](https://arxiv.org/html/2608.01288), [ObjectClear LICENSE](https://raw.githubusercontent.com/zjx0101/ObjectClear/main/LICENSE)
- CORNE (287,012 effect-aware pairs) is Apache-2.0, a candidate fine-tuning corpus, provenance caveats aside — [OSOR repo](https://github.com/Zhouqm-Git/osor)

### Inferences
- Recommended qualification order for Luxforge's existing plan (inference):
  1. Big-LaMa (+ MI-GAN) as the shipped-quality floor and the pipeline test bed for crop, encode, detail and noise, and compositing.
  2. OSOR-SDXL and RORem-4S on M4 via Core ML, MLX or sd.cpp, measuring quality on shadows and reflections, runtime, memory and determinism. This pends an owner decision on accepting OpenRAIL++-M downloads.
  3. FLUX.2 klein 4B inpaint, zero-shot with a fixed "empty scene" prompt, plus an optional explicit contact band, as the Apache-only generative path. If promising, consider a removal LoRA trained on CORNE and owner-captured pairs.
  4. ObjectClear/TurboClear, OmniEraser and a remote provider as **reference ceilings only**.
- Replace the existing proposal's "PowerPaint" and "SDXL-inpainting" evaluation candidates with **OSOR-SDXL / RORem** (same SDXL licence class, much better removal) and add **FLUX.2 klein 4B**. PowerPaint (SD1.5, no effect handling) is superseded.

### Gaps
- None of the generative candidates has an M4 measurement. The M4 timings above are estimates.
- Whether OSOR-SDXL's one-step output quality holds at 512–1024 crops of real 24–60 MP photos with high-ISO grain is unknown.
- No licence-clean, effect-aware, M4-fast model exists as of September 2026. Creating one would be a training project with its own data-provenance questions.
