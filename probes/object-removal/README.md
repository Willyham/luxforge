# Object removal probe

The AI harness: a development prototype, not Luxforge's editor, holding segmentation, stroke resolution, mask refinement and fill for object removal ([AI editing design](../../docs/design/ai-editing.md)), measured on the owner's M4 before anything enters `crates/`. Later AI qualification tasks add their commands here. It reads JPEG or PNG files, never writes to an input, and writes results as new files.

## Models

The probe never downloads. Fetch the pinned files once into the checkout's ignored `private/models/object-removal/` (a linked worktree finds the main checkout's), or point `--models` or `LUXFORGE_REMOVAL_MODELS` elsewhere. Every command that loads a model first checks its size and SHA-256 against the pins in `src/models.rs` and **refuses, by file name, any file that differs**, before a runtime sees it: `refusing model l0_decoder.onnx (…): 1234 bytes, expected 16470362`. `removal-probe models` checks them all, lists each file that is refused and fails if there is one.

| File | Bytes | SHA-256 | Source |
| --- | ---: | --- | --- |
| `l0_encoder.onnx` | 123,112,917 | `7a37ebd1…` | `https://huggingface.co/mit-han-lab/efficientvit-sam/resolve/main/onnx/l0_encoder.onnx` |
| `l0_decoder.onnx` | 16,470,362 | `053e72c5…` | `https://huggingface.co/mit-han-lab/efficientvit-sam/resolve/main/onnx/l0_decoder.onnx` |
| `mask-object-sam21-small.dtmodel` | 169,639,526 | `06a629ac…` | `https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/mask-object-sam21-small.dtmodel`, unzipped in place to `mask-object-sam21-small/` |
| `inpainting_lama_2025jan.onnx` | 92,591,623 | `7df918ac…` | `https://huggingface.co/opencv/inpainting_lama/resolve/main/inpainting_lama_2025jan.onnx` |
| `lama_fp32.onnx` (optional, `--fill-model lama-fp32`) | 208,044,816 | `1faef530…` | `https://huggingface.co/Carve/LaMa-ONNX/resolve/main/lama_fp32.onnx` |

Building `ort` fetches pyke's prebuilt ONNX Runtime static library, hash-checked by `ort-sys`.

The generative candidates' weights go in the ignored `private/models/generative/` (`--gen-models` or `LUXFORGE_GENERATIVE_MODELS` elsewhere), pinned the same way and refused the same way; `removal-probe models --generative` checks the ones present. Every URL is pinned to a Hugging Face commit; every file's size and SHA-256 matched the publisher's LFS record when it was fetched.

| File | Bytes | SHA-256 | Source | Licence |
| --- | ---: | --- | --- | --- |
| `moebius-unet.onnx` | 906,698,976 | `e3f90f52…` | `simonw/Moebius-ONNX@5bf1ef5d` `unet.onnx` (an fp32, static 512² export of hustvl/Moebius `ft_places2`) | Apache-2.0 (export); Moebius MIT on its card, Apache-2.0 in its repository |
| `moebius-vae-encoder.onnx` | 136,757,093 | `b8b81d41…` | same, `vae_encoder.onnx` | as above |
| `moebius-vae-decoder.onnx` | 198,078,671 | `d90ef0b7…` | same, `vae_decoder.onnx` | as above |
| `flux-2-klein-4b-Q4_0.gguf` | 2,460,378,560 | `d1023499…` | `leejet/FLUX.2-klein-4B-GGUF@3b1f5a9d` | Apache-2.0 |
| `flux-2-klein-4b-Q8_0.gguf` | 4,300,629,440 | `0bba6951…` | same | Apache-2.0 |
| `flux2-klein-4b-vae.safetensors` | 168,120,878 | `ca70d220…` | `black-forest-labs/FLUX.2-klein-4B@e7b7dc27` `vae/diffusion_pytorch_model.safetensors` | Apache-2.0 |
| `klein-object-remove-lora-comfy.safetensors` | 76,038,936 | `dc197de6…` | `fal/flux-2-klein-4B-object-remove-lora@0e3f5879`, the ComfyUI-converted file | Apache-2.0 |
| `Qwen3-4B-Q4_K_M.gguf` (text encoder, Replace only) | 2,497,281,312 | `f6f85177…` | `unsloth/Qwen3-4B-GGUF@22c9fc8a` | Apache-2.0 |
| `Qwen3-4B-Q8_0.gguf` (text encoder, Replace only) | 4,280,405,792 | `eed55523…` | same | Apache-2.0 |
| `qwen3-tokenizer.json` (with the text encoder) | 11,422,654 | `aeb13307…` | `black-forest-labs/FLUX.2-klein-4B@e7b7dc27` `tokenizer/tokenizer.json` | Apache-2.0 |

## Corpus

`corpus.json` is the checked-in manifest of the images the harness is measured on. `removal-probe corpus` lists it and what is present; it never downloads.

- **Private.** The owner's photographs live in the ignored `private/object-removal/corpus/` (`--corpus` or `LUXFORGE_REMOVAL_CORPUS` elsewhere) and are never committed. The manifest names each by file, size, source and use, and records nothing of its content (no digest, no thumbnail). The RAW files were developed to JPEG by macOS Core Image (`sips -s format jpeg -s formatOptions 95`), outside Luxforge's pipeline; the probe treats them as ordinary developed images.
- **Public.** Twelve CC0 photographs from Wikimedia Commons, three each of people, skies, water and small objects, each listed with its source page and URL, author, licence, size and SHA-256. They are fetched by hand into the ignored `private/object-removal/public/` (`--public` or `LUXFORGE_REMOVAL_PUBLIC` elsewhere): `removal-probe corpus --curl` prints the `curl` commands for the files that are absent, and `removal-probe corpus --verify` checks the present ones and refuses, by name, any whose size or digest differs. The licence was read from each file's Commons page when it was listed.
- **Ground truth.** A case is a clean image with a synthetic object and shadow (`commands::fill::paste_object`) composited at a fixed position, and the region that covers every changed pixel, dilated. Nothing in it is random, so a case is identical on every run and a removal is scored against the clean original. The manifest holds 16 cases, one per public image and four on the private photographs; a case's centre is a fraction of the image and its size a fraction of the shorter side. `removal-probe paste --case ID --out DIR` writes the case's `pasted.png`, `region.png` and `case.json`.

| Private file | Size | Source | Use |
| --- | --- | --- | --- |
| `z6-bridge.jpg` | 6048 × 4024, EXIF-rotated | `fixtures/raw/nikon_z6.NEF` | A boy on a bamboo bridge, people and poles behind: one object on structured texture; the known removal |
| `x100vi-sunflowers.jpg` | 7728 × 5152 | `fixtures/raw/fujifilm_x100vi.RAF` | Sunflowers on a blurred background |
| `air2s-kayaks.jpg` | 5464 × 3640 | `fixtures/raw/mavic_air_2s.DNG` | Many small kayaks on water: several objects under one stroke |
| `sapa-road.jpg` | 3389 × 4236 | `fixtures/jpg/will-sapa-drone.jpg` | A road through terraces: a long thin object |
| `quadrants-24mp.jpg`, `quadrants-60mp.jpg` | 6000 × 4000, 10000 × 6000 | `fixtures/generated/` | Timing at 24 and 60 MP, and colour order |

## Build and run

```sh
cargo build --release --manifest-path probes/object-removal/Cargo.toml
probes/object-removal/target/release/removal-probe models
probes/object-removal/target/release/removal-probe inspect /path/to/model.onnx --backend ort-coreml-gpu
cargo test --manifest-path probes/object-removal/Cargo.toml
cargo test --manifest-path probes/object-removal/Cargo.toml -- --ignored   # needs the models and corpus
```

The probe is its own Cargo workspace with its own lockfile, outside the editor's: the editor's build and quick tier do not build it. Its `diffusion/` sub-crate builds stable-diffusion.cpp and ggml from the editor's vendored, pinned sources in [`crates/luxforge-diffusion`](../../crates/luxforge-diffusion) with the `cc` crate (no CMake, no Python), through that crate's `native/build.rs` and `src/native.rs`; [`THIRD_PARTY.md`](../../crates/luxforge-diffusion/THIRD_PARTY.md) records the pins, the source selection, the licences and the two local changes. Metal compiles its kernels at the first generation on a machine (about 16 s) and caches them. `cargo xtask check-repository` reads its sources and manifest with the repository's other rules. The tests that need the pinned models and the private corpus are `#[ignore]`d; the ignored suite passed in full on the M4 (release profile, ONNX Runtime CPU and Core ML), including the two scoring tests below.

Backends: `ort-cpu`, `ort-coreml-all`, `ort-coreml-gpu`, `ort-coreml-ane`, `ort-coreml-cpu`, `tract-cpu`, `tract-metal`, and the fixed-shape Core ML backends `ort-coreml-all-fixed`, `ort-coreml-gpu-fixed`, `ort-coreml-ane-fixed` (and `ort-coreml-cpu-fixed`). Core ML compiles each model on first load and caches it under the system temporary directory (`luxforge-removal-probe-coreml`); macOS keeps its own device-specialized copy in `~/Library/Caches/<process name>/com.apple.e5rt.e5bundlecache`, and the measured cold loads clear both. A cache compiled before a macOS update fails at load with `Unable to load model … Compile the model with Xcode`; delete the model's directory under `luxforge-removal-probe-coreml` and it compiles again. tract has no such cache: four of the five pinned models have a dynamic input dimension (`batch_size`, `num_points` or `batch`), so `runtime::Session` concretizes shapes lazily — the first `run` for a given combination of input shapes builds and optimizes a `TypedModel` plan and caches it; later runs with the same shapes reuse it. That first build is folded into "first run", not "load", in every table below.

The Core ML provider is configured to take only nodes whose input shapes are static, so on a model that declares a free dimension (`batch_size` and `num_points` in both EfficientViT-SAM models, `num_points` in the SAM 2.1 decoder, `batch` in LaMa) it runs little or nothing. A `-fixed` backend loads a copy of the model with every free input dimension fixed at 1 instead. ONNX Runtime writes that copy itself, once per model file, into the same cache directory: a CPU session with free-dimension overrides at the basic optimization level saves the graph it optimized, whose shape computations are then constants. A run with any other input shape (EfficientViT's batched stroke prompts, a box prompt's two points) goes to a CPU session of the original model, opened the first time one arrives. Setting the overrides on the Core ML session directly lets the provider take the graph, but the compiled partition then asks at run time for a `Shape` node's output that nothing feeds (`Feature …_Shape_output_0 is required but not specified`) on both EfficientViT models; `LUXFORGE_REMOVAL_ORT_LOG=warning` prints how many nodes the provider takes, and `verbose` why it refuses each other one.

```sh
probes/object-removal/target/release/removal-probe parity --out target/evidence/task-004/parity.json
```

### Removing from the command line

`removal-probe remove` runs the whole chain on one point or one stroke and writes into `--out` (default: a new directory under the system temporary directory, which it names):

```sh
C=private/object-removal/corpus
probes/object-removal/target/release/removal-probe remove --image $C/z6-bridge.jpg --point 1690,3550 --out /tmp/bridge
probes/object-removal/target/release/removal-probe remove --image $C/air2s-kayaks.jpg --family sam21-small \
  --stroke "3177,2442 3255,2427 3320,2487" --radius 12 --pick 1,2 --out /tmp/kayaks
```

- A point is decoded alone and picked as hover picks; a stroke becomes its prompts, decoded in one batch and decided. **One** object is removed with the stroke's footprint (`--no-stroke-region` leaves it out; `--contact-band` adds the band). **Several** writes the overlay with numbered outlines and removes nothing unless `--pick N[,M...]` chooses; the chosen objects' regions are joined without the footprint, which also crosses the objects left out. **Nothing** removes the stroke's footprint, dilated as every region is.
- `decision.json` holds the inputs, the stroke parameters, every decoded candidate (`decoded`), the scored survivors (`candidates`), the decision and chosen indices, the region's bounds and area, the fill's report, the count of changed pixels outside the region (always 0, or the command fails), and each stage's time. `overlay.png` shows the candidates around the stroke or point, `region.png` the full-resolution region, `result.png` the result, and `compare.png` before (region in red) and after, cropped around the region.
- `--param NAME=VALUE` sets a `stroke::Params` field for tuning, over the family's starting values (`stroke::Params::for_family`: SAM 2.1's predicted-IoU floor is 0.6, EfficientViT-SAM's 0.7). The fill is LaMa on `--fill-backend` (default `ort-cpu`; `ort-coreml-gpu` runs none of it and `ort-coreml-gpu-fixed` most of it, neither faster, as [measured](#fill)).

### The lab

`removal-lab IMAGE` opens the image in a window (the owner launches it; automated checks never do). Hover highlights the object under the pointer once the image is analysed, and a click removes it. B toggles the brush, or hold Shift; `[` and `]` size it. A stroke resolves as `remove` does: one object is removed, several get numbered pins to click (or click the objects) and Enter to remove the chosen ones together, Esc to cancel, and none removes the painted area. A removal keeps the selection on screen until it finishes; Esc cancels it and a failure leaves the image unchanged. ⌘Z undoes, ⌘S saves `<name>-lab-NNN.png` into `--out` (default: `removal-lab` under the system temporary directory), never overwriting anything.

Encoding, hover decodes, strokes, fills and saves each run off the window's thread; LaMa loads once per session on the fill worker. The hover worker holds the segmenter and the embedding and keeps only the latest pointer position. The display copy is at most 4096 px on its long side, and each overlay is sampled from the logits over the object's bounds only, at six samples per logit cell and under 500,000 pixels, so iced uploads it within the frame.

Everything is logged as JSON lines to stdout and to `lab-<unix ms>.jsonl` in `--out`: each decision with its scored candidates and stage times, each removal with its region and fill stages, and each hover result with its decode time and `pointer_to_result_ms` (the pointer event to the result applied, where the window makes the overlay's handle). The window also logs `hover_drawn` with `pointer_to_draw_ms`, to the draw call of the first frame that contains the overlay; presenting that frame is not included.

`--script FILE` runs the same lab without a window: it makes the calls the window makes for pointer and key events, applies results as they arrive, writes the same log and a `script_summary` of the hover latencies, and saves the result. The step language is documented in `src/lab/script.rs`; `scripts/bridge.lab` hovers, sweeps, clicks, undoes and brushes on the bridge:

```sh
probes/object-removal/target/release/removal-lab $C/z6-bridge.jpg --script probes/object-removal/scripts/bridge.lab --out /tmp/lab
```

`--script FILE --hidden-window` sends the same steps to an invisible window as its own messages instead, so the window's update, image handles and canvas draw run and `hover_drawn` is logged; the pointer time is taken when the script sends the move. Launch it from a copy of the binary inside a background-only bundle (`Contents/Info.plist` with `LSBackgroundOnly`, as `xtask`'s background launches do), so it never activates or takes focus.

### Adding a command

A command is one file in `src/commands/` with a `USAGE` text and `run(Args)`, a `pub mod` line and one row in the `commands::ALL` table; `removal-probe` dispatches and prints its usage from the table. The shared parts a command reuses: `models::directory` and `Model::verified` for the pinned models, `corpus::{Manifest, Dirs}` for corpus images and cases, `runtime::open` for a backend session, and `score::score` for the metrics.

### Scoring a removal

`removal-probe score` reports, for a removal against its ground truth:

- PSNR and SSIM inside the **region** (pixels with non-zero coverage) and in its **edge ring**, against the clean truth. The ring is the region's own pixels within `--ring` pixels (default 8, a square neighbourhood) of a pixel outside it, where a seam shows; the image border is not an edge. Outside the region the result is the input, so the outside is checked for change rather than scored.
- The **changed pixels outside the region**: pixels with coverage 0 whose value differs between result and input. It must be 0 and the command fails otherwise.
- **LPIPS**: `skipped`, with the reason in the report. No LPIPS weights are pinned; the backbone and linear heads, their size and SHA-256, their licence and any restriction on the person's use of their results have yet to be recorded, so the check does not run. A skipped check is not a pass.

PSNR is `10 log10(255² / MSE)` over every channel of the counted pixels, reported as the string `"infinity"` when the two are identical. SSIM is Wang et al.'s (2004): an 11×11 Gaussian window with σ 1.5, K1 0.01, K2 0.03, L 255 and population covariance, per channel with the image edge replicated, averaged over the three channels and then over the counted pixels; windows straddle the region's boundary and read both sides. The unit tests pin both on synthetic buffers: PSNR of a known error, SSIM of identical and flat images (the luminance term, exactly), SSIM against `scripts/ssim_golden.py`, an independent per-pixel NumPy implementation of the definition, and the ring's geometry, including at the image border and for soft or holed regions.

```sh
C=probes/object-removal/target/release/removal-probe
$C paste --case sky-clouds-blue --out /tmp/case                          # pasted.png, region.png, case.json
$C fill --image /tmp/case/pasted.png --mask /tmp/case/region.png --out /tmp/case/fill
$C score --case sky-clouds-blue --result /tmp/case/fill/result.png       # scored against the clean image
$C score --input IMG --region region.png --result result.png [--truth CLEAN.png]
```

With `--case`, `score` regenerates the pasted image and region from the manifest and scores against the clean original. With `--input` and `--region` it scores any removal, such as `remove`'s `region.png` and `result.png`; without `--truth` only the outside check runs and the quality fields are `skipped`. Measured on the M4 with the pinned LaMa on ONNX Runtime's CPU (these are scores of the existing fill, not a claim about it):

| Removal | Region PSNR / SSIM | Edge ring PSNR / SSIM | Changed outside |
| --- | --- | --- | --- |
| Pasted object, `sky-clouds-blue` (4032 × 2670, 140,087 px region, 12,224 px ring) | 24.29 dB / 0.944 | 36.46 dB / 0.956 | 0 |
| Pasted object, `bridge-deck` (4024 × 6048, 310,666 px region, 18,336 px ring) | 21.87 dB / 0.802 | 30.08 dB / 0.893 | 0 |
| The boy on the bridge, `remove --point 1690,3550` (685,151 px region, 41,999 px ring) | skipped, no truth | skipped, no truth | 0 |

## Contract

Status: a standalone prototype, delivered and measured on the M4. The product integration is the [AI editing design](../../docs/design/ai-editing.md)'s Remove, which owns the repair stage, frozen patches and consent rules. The evidence behind every choice is the [object removal research](../../docs/research/object-removal/README.md).

### What a person does

The Remove tool finds the object for the person instead of asking them to trace it.

- **Brush.** The person paints roughly over what they want gone. The tool looks for objects under the stroke. When one object explains the stroke, it is removed. When several distinct objects are under it, each is outlined with a numbered pin and the person clicks the ones to remove, then confirms; the chosen objects are removed together, without the stroke's footprint, which also crosses the objects left out. When none is found, the painted area itself is removed, as a plain brush would.
- **Hover.** With the tool selected, the object under the pointer is highlighted as the pointer moves, and a click removes it. Hover appears once the image has been analysed; until then the tool shows that analysis is running and the brush still works.
- **Shadows and reflections.** A segmenter never includes an object's shadow or reflection. The stroke's own footprint joins the removal region, so painting over a shadow removes it with the object. A suggested contact band below the object is an option. A learned effect-aware model can later propose this region instead, and the person accepts it before anything is removed.
- **Removal is a job.** Filling takes seconds; the selection stays visible and cancellable while it runs, and the result replaces the region only when it succeeds.

The research names Lightroom's Detect objects and People pins, and Photoshop's Object Finder hover, as the familiarity references; neither is a rendering target.

### Pipeline

```text
image ──encode once──▶ embedding ─┬─ hover point ──decode──▶ pick_hover ──▶ overlay
                                  └─ stroke ──prompts──▶ decode_many ──▶ decide ──▶ One / Several / Nothing
chosen candidate ──removal_region (upsample, ∪ stroke, ∪ band, dilate)──▶ region
image + region ──fill (crop, model, upscale, grain, blend, composite)──▶ result
```

Every stage is a function of its inputs with no hidden state, so each is testable on its own and the same code serves the command line, the lab window and, later, the editor.

#### Segmentation

A segmenter is an image encoder run once per image and a prompt decoder run per prompt. Prompts are positive points, negative points, a box, and an optional previous result. A decoder returns one mask (EfficientViT-SAM) or up to three (SAM 2.1) as **logits**: a low-resolution grid, 256² for both pinned models, where a value above 0 is inside the object, plus the decoder's predicted IoU. Logits carry the map from source pixels to cells, so every later stage samples them in source coordinates whichever model produced them.

| Model | Encoder input | Decoder | Logit frame |
| --- | --- | --- | --- |
| EfficientViT-SAM L0 | Long side resized to 512 with antialiased bilinear, sizes rounded half up; ImageNet mean and standard deviation; zero-padded bottom and right to 512² | Points (labels 1 and 0), then box corners (2 and 3), in the long-side-1024 frame; one mask per prompt, the best of SAM's four mask tokens by predicted IoU; prompts with the same number of points batch in one call | 256² over the padded 1024² frame: `cells_per_px = (w′ / 4w, h′ / 4h)`, where `w′ × h′` is the image's long-side-1024 size; within 0.1% of `256 / long side` |
| SAM 2.1 Small (darktable-ai export) | Squashed to exactly 1024² with antialiased bilinear; ImageNet mean and standard deviation | Box corners (2 and 3), then points (1 and 0), in the squashed 1024² frame; the decoder appends SAM 2's padding point itself; `mask_input` (a previous 256² logit grid) and `has_mask_input`; one prompt per call; SAM 2's three multimask outputs, none of them its single-mask output, clamped to ±32 | 256² over the squashed frame: `cells_per_px = (256 / width, 256 / height)` |

Both decoders add half a pixel to each prompt coordinate, so a source position `p` is passed as `p · scale − 0.5`. darktable 5.6's own driver for the SAM 2.1 export letterboxes the image as SAM 1 does (long side 1024, zero padding) and appends a second padding point; the probe follows SAM 2's `SAM2Transforms` and darktable-ai's own demo, which squash. The sources are cited in `src/segment/frame.rs`.

#### From a stroke to a decision

The stroke becomes prompts: its bounding box, padded by the brush radius and clamped to the image, and one single-point prompt for each of `sample_points` points spread along it by farthest-point sampling. The polyline is sampled about once per source pixel, at most 4,096 times; sampling starts from the sample nearest the samples' centroid, and ties go to the earlier sample, so a stroke always gives the same prompts. A single-point stroke gives the box and one point. All are decoded against the cached embedding in one batch.

Each candidate is scored on predicted IoU, **stability** (the IoU between the mask thresholded at logit +1 and at −1 over the cells inside the image, as SAM's automatic mask generator computes it), **stroke coverage** (the share of the stroke inside the candidate) and **object coverage** (the share of the candidate under the stroke). Coverage and area share one working raster: each logit cell is split into sub-cells of at most about 4 source pixels, a sub-cell is inside the candidate when its bilinearly interpolated logit is above 0, and inside the stroke when its centre lies within the brush radius, or half a sub-cell's diagonal for a thinner brush. Areas are in source pixels.

A candidate the stroke does not touch is dropped, and so is one below the stability floor, the object-coverage floor or the IoU floor. The exception is a **whole**: a candidate holding at least 90 % of a smaller candidate that clears every floor is kept down to `min_whole_iou`, because a prompt on a part makes SAM 2.1 score the part above the whole. The rest are deduplicated by mask IoU, largest first, keeping the larger of two near-duplicates, because for removal the whole object is nearly always the intent. A survivor with at least 90 % of its area inside a larger survivor is a **part** of it, and a part yields to its whole.

- **One** when a surviving candidate covers at least `one_object_stroke_coverage` of the stroke. An explaining part yields to an explaining whole, even when the stroke lies entirely within the part; among the rest, the best object coverage wins. The object-coverage floor keeps out a whole the stroke barely touches, such as the table under a cup.
- **Several** when no survivor explains the stroke and two or more are disjoint. Parts are set aside, and the rest are taken by stroke coverage, each sharing less than 10 % of the smaller one's area with those already taken, and listed in that order. Overlap is measured against the smaller mask rather than as IoU, so a small part inside a whole never counts as a separate object.
- **Nothing** otherwise: no survivor, or survivors that neither explain the stroke alone nor include two disjoint objects.

Every tie falls to a fixed order (area, predicted IoU, stability, then prompt and candidate index), so the same stroke and results always give the same decision. Starting values are in `stroke::Params`, per family through `Params::for_family`. None is a published constant, and the prototype tunes them on its corpus: SAM 2.1 scores small objects lower than EfficientViT-SAM does, so its predicted-IoU floor is 0.6 against 0.7.

A hover point is decoded alone. The pick is the candidate with the best predicted IoU, unless a larger candidate holds at least 90 % of it and reaches `min_whole_iou` and `min_stability`: then the largest such whole. Taking the largest candidate within 0.05 of the best predicted IoU picked a part: for a click at a boy's waist, SAM 2.1 scored his clothes 0.80 and the whole boy 0.61.

#### The removal region

The chosen candidate's logits are upsampled to source resolution with a guided filter against the image's luminance, so its edge follows the photograph rather than the 256² grid.

- **Prior.** A hard sigmoid of the bilinearly sampled logit, `clamp(0.5 + logit / 4, 0, 1)`. It has the logistic sigmoid's slope at 0, so near the edge it is the decoder's own probability, but it saturates beyond ±2, so confident pixels are exactly 0 or 255; a logistic prior leaves 1 or 254 across the whole window.
- **Window.** The bounds of the cells whose prior is non-zero, padded by a cell plus the filter's reach and by at least three cells, and clamped to the image. For a decisive decoder that is the positive cells' bounds padded by a few cells, and no coverage is ever cut at the window's edge. Nothing outside the window is allocated, so a 1200 × 1800 px object on a 60 MP frame works over about 5 % of it.
- **Guided filter.** He et al.'s filter against Rec. 709 luma of the sRGB-encoded image, with `guided_eps` 10⁻³, box means from running sums, and by default a radius of one logit cell in source pixels. From a radius of 8, the coefficients are computed on a grid subsampled to keep the radius at 4 samples and bilinearly upsampled before `q = a · I + b` is taken at full resolution (He and Sun's fast guided filter), so the buffers stay small at 60 MP.

For removal, the soft coverage becomes an **object core**: full coverage wherever the decoder places the object inside (its bilinearly sampled logit is above 0) or the guided filter gives it at least half coverage, and 0 elsewhere. A removal region needs the object's extent, not its probability. Kept soft, faint coverage beyond the object widened the fill's crop several times over on the sunflowers, and a small kayak whose guided peak stayed at 78 of 255 survived its own removal.

The stroke's footprint (every pixel whose centre lies within the brush radius) and the optional contact band are joined to the core by pointwise maximum, and the union is trimmed to its non-zero bounds. The region is then dilated by a flat disc, because a removal mask must cover the object's edge halo: anti-aliasing, blur and colour bleeding into the surroundings. The dilation is grayscale and computed chord by chord from per-row tables of maxima over power-of-two runs, at most O(N·r) over the window. Its radius scales with what it covers: half a logit cell, the decoder mask's positional uncertainty, plus 4 % of the core's shorter side, at least 2 px. A kayak 40 px wide at 20 MP grows by about 12 px, a person 700 px wide at 24 MP by about 40 px. A radius from the frame alone, 60 px at 24 MP, swallowed the swimmers beside a kayak. The dilation and the feather together never exceed `round(15 · √(pixels / 1.5 MP))`, Inpaint Anything's 15 px on 1–2 MP images scaled with the image's linear size: about 60 px at 24 MP and 95 px at 60 MP. Last, the region gets a soft outer edge one logit cell wide, where coverage falls from the region's to 0. A quarter of a cell left a visible seam where the enlarged fill met the bridge's deck.

A stroke that finds no object removes its footprint as painted, feathered over 15 % of the brush radius and not dilated, because the person painted exactly what they want gone. A 60 MP frame is about 9.3 times the encoder's 1024 px, so one logit cell spans about 37 source pixels; the guided filter, the dilation and the feather all work at that scale.

The **contact band** is a crude heuristic after PredErase, with no light direction. The object's contact columns are those with coverage of at least 128 in its lowest tenth of rows. The band is full on the object's bottom edge over those columns, reaches up `max(4, 0.2 h)`, down `max(4, 0.1 h)` and `max(8, 0.35 w)` sideways beyond them, where `w` and `h` are the object's width and height, and falls to 0 with a smoothstep in each direction.

#### Fill

The fill crops the region's bounds plus context of `max(min_margin, margin × longer side)` on every side, squared to the grown longer side and slid inside the image. Where the image is smaller than the square, it is padded past the edge by reflection, so resizing to the model's fixed 512² never distorts geometry. The margin is 1.0, so the region spans about a third of the model's input; at OpenStill's 0.4 it spans over half, and the pinned LaMa leaves dark blobs in flat areas. The model's hole covers every model pixel that any region pixel contributes to, so no known model pixel carries the object. The pinned export zeroes the hole and keeps the known pixels itself; it takes RGB scaled to 0–1 and returns 0–255 codes, and the result is resized back.

A 512 px model output enlarged several times is smooth where the photograph is grainy. When the output was enlarged, grain is re-synthesised. Its level, spatial correlation and correlation between channels are estimated in the flattest parts of a ring around the region, with a robust high-pass estimate at two tap spacings. Deterministic Gaussian grain from a fixed seed then makes up the fine texture the upscaled fill lacks.

The blend corrects the fill's colour and tone at the region's edge. It measures the difference from the input in the band just outside the region where the enlarged fill is partly regenerated background, and pyramid push-pull extends that difference inward. A Laplacian pyramid then carries detail across the edge. The input under the region enters neither step. Finally **every pixel whose region coverage is 0 is copied from the input**, so the output is byte-identical outside the region whatever the blend did, and a pixel with full coverage is wholly replaced. The Corrections design already requires that; the prototype proves it by test.

### The prototype

The prototype is `probes/object-removal`, its own Cargo workspace outside the editor's, so its dependencies, lockfile and build are its own and nothing in `crates/` changes. It reads JPEG or PNG files, never writes to an input, and writes results as new files. It has no recipe, history, catalog or API parity; those belong to the editor integration.

| Part | What it is |
| --- | --- |
| Runtime port | `runtime::Session` over ONNX Runtime (`ort` 2.0.0-rc.13: CPU, and Core ML at each compute-unit setting, directly or over a copy of the model with its free input dimensions fixed at 1) and tract 0.23.8 (CPU and Metal), so one model runs unchanged on every backend compared |
| Models | Pinned by size and SHA-256 in `models.rs`, fetched by hand into the ignored `private/models/object-removal/`, never by the probe |
| `removal-probe` | Commands: `models`, `inspect`, `segment`, `fill`, `remove`, `bench`, `parity` |
| `removal-lab` | A small iced window: open an image, hover to highlight, click or brush to remove, choose among several, undo, save a copy |

| Model | File | Licence (code / weights) | Source |
| --- | --- | --- | --- |
| EfficientViT-SAM L0 | `l0_encoder.onnx`, `l0_decoder.onnx` | Apache-2.0 / Apache-2.0 | [mit-han-lab/efficientvit-sam](https://huggingface.co/mit-han-lab/efficientvit-sam) |
| SAM 2.1 Small | `mask-object-sam21-small.dtmodel` (encoder and decoder ONNX) | Apache-2.0 / Apache-2.0; SA-1B training data is research-only | [darktable-ai release-5.6.0](https://github.com/darktable-org/darktable-ai/releases/tag/release-5.6.0) |
| LaMa | `inpainting_lama_2025jan.onnx`, weights quantized in blocks | Apache-2.0 / Apache-2.0; Places2 training data is non-commercial | [opencv/inpainting_lama](https://huggingface.co/opencv/inpainting_lama) |
| Big-LaMa, full precision (optional) | `lama_fp32.onnx` | Apache-2.0 / Apache-2.0; Places2 training data is non-commercial | [Carve/LaMa-ONNX](https://huggingface.co/Carve/LaMa-ONNX) |

#### What the prototype shows

The measurements below hold the evidence for each of these.

- The brush, hover and click flows work on the corpus through the `remove` command and the lab, with every decision logged. The lab's own window has been driven only hidden, from a script; interactive use is the owner's check.
- Every fill leaves the pixels outside its region byte-identical to the input, by test and on every corpus run.
- A synthetic ground truth scores the fill: an object pasted onto a real background, removed, and compared inside the region with the background it covered.
- Segmenter parity: the same prompt's mask on each backend against ONNX Runtime's CPU result, as mask IoU, per model and backend.
- On the M4, for each model and backend: cold and warm load, encoder time, decoder p50, p95 and p99 at hover rate, pointer-to-overlay time in the lab, fill time per stage at 20 to 60 MP, and peak memory. These are the research's measurements 1 to 3, 5, 7 and 8.

#### Summary of the M4 measurements

The probe's [measurements](#measurements) were taken on the owner's M4 Pro while other sessions shared it (load average about 24 on 14 cores). They are upper bounds, most of all for ONNX Runtime's CPU provider.

**Chosen** ([AI editing](../../docs/design/ai-editing.md#decisions)): EfficientViT-SAM L0 as the default segmenter, on ONNX Runtime with Core ML's GPU setting over fixed-shape copies (`ort-coreml-gpu-fixed`), and LaMa on ONNX Runtime's CPU provider.

- **Runtime.** ONNX Runtime, because tract loads only one of the five pinned models. The Core ML provider takes only nodes with static input shapes, so it runs EfficientViT-SAM only over a copy with every free dimension fixed at 1. ONNX Runtime writes that copy itself, with no Python and no re-export. LaMa stays on the CPU: on Core ML it runs in 188 partitions, is no faster (2.2 s against 1.2 s in paired runs) and holds a 9 GB footprint against 1.5 GB.
- **Segmenter.** EfficientViT-SAM encodes in about 110 ms (130–160 ms at 60 MP) and decodes a hover point in about 5 ms. It loads in about 1.2 s once compiled, compiles in about 7 s the first time, and stays under a 1 GB footprint. SAM 2.1 Small on Core ML decodes as fast and encodes in 150–180 ms. Its Hiera encoder, though, splits into 27 Core ML partitions at rank-6 reshapes. As a result it takes about 11 s to load even when cached and 30–90 s to compile, and holds a 3.5 GB footprint; on the CPU it encodes in over 500 ms.
- **Accuracy.** SAM 2.1 is the more accurate of the two on small objects. At its 0.6 floor it found the kayaks and the two walkers that EfficientViT-SAM missed, because EfficientViT-SAM answers a point on a kayak in dark or uniform water with the water itself. SAM 2.1 Small is offered as the alternative, presented as that trade-off against its load time and memory.

Budgets, from the pointer to the drawn overlay in a hidden lab window, and from `bench` and `remove`:

- **Hover, 16–33 ms: the median is met and the tail sits at the edge.** On Core ML both families draw the overlay at a p50 of 20 ms and a p95 of 33–35 ms. On the CPU the p95 is 45–46 ms, a miss.
- **Analysis, under about 300 ms after the image opens: met on Core ML once the models are loaded.** EfficientViT-SAM encodes in 110–160 ms and SAM 2.1 in 150–180 ms, while SAM 2.1 on the CPU misses (520–950 ms). The first image of a session also waits for the load, about 1.2 s for EfficientViT-SAM and 10–20 s for SAM 2.1, unless the models load before the tool is picked.
- **Fill, a few seconds: met.** LaMa on the CPU fills in 1.7–3.0 s at 20–60 MP. The region adds up to 0.5 s, and LaMa loads once in 2–3 s. Every fill left the pixels outside its region byte-identical.

**The open problem is the fill's quality on large objects.** The pinned LaMa takes a fixed 512² input, and the crop is three times the region's longer side. The fill is therefore enlarged 9× for the boy on the bridge, 10–13× for a flower or the water, 20× for a tall pole and 30× for a 60 MP quadrant. Grain re-synthesis restores the noise but not the structure: the bridge deck under the boy is rebuilt visibly softer than the deck around it, and the road's fill is a blurry smear. The fill's time hardly depends on the object, because LaMa always runs at 512². The quantized weights are not the cause. Carve's full-precision Big-LaMa export scores within 0.1 dB of the OpenCV export on eight pasted-object ground truths, and gives the same smeared deck on the bridge. What remains open is quality at a larger effective input: a fill that is not enlarged as far, either a second LaMa pass over the region at a finer scale or an export at 1024 or 2048, as in the research's measurement 7.

**Stroke parameters (proposal).** SAM 2.1 starts from a predicted-IoU floor of 0.6. At that floor two of its nine corpus strokes went from nothing to the right several objects, and the other seven did not change. At 0.65 the kayaks were still missed. EfficientViT-SAM keeps every default: its misses are the decoder's own answers, and no floor changed a decision. One question is a matter of intent rather than a threshold: whether a stroke along one bend of a road, covering 15 % of it, removes the whole road, as both families decide now.

#### What it does not do

No RAW input (the corpus is developed to JPEG first), no proposal detector, no editor integration, no download code. The generative candidates run only through `generate`, measured under [Generative fill](#generative-fill); a detector-driven proposal pass follows if the hover measurements show that decoding on every pointer move is not enough.

### Decisions

| Question | Status |
| --- | --- |
| Removal before semantic masks | Decided: prototype object removal first |
| Prototype outside the editor | Decided: a standalone probe |
| Models in the prototype | Proposal: Apache-2.0 weights only, as above |
| Segmenter | Decided ([AI editing](../../docs/design/ai-editing.md#decisions)), from the [M4 measurements](#summary-of-the-m4-measurements): EfficientViT-SAM L0 by default; SAM 2.1 Small, more accurate on small objects but about ten times slower to load and with about four times the memory, as the alternative |
| Runtime | Decided ([AI editing](../../docs/design/ai-editing.md#decisions)), from the [M4 measurements](#summary-of-the-m4-measurements): ONNX Runtime, the segmenter on Core ML's GPU setting over fixed-shape copies, LaMa on the CPU; tract loads one pinned model of five |
| Shadow handling | Decided ([AI editing](../../docs/design/ai-editing.md#decisions)): the effect region is a visible, editable part of the selection, suggested by a heuristic; here the stroke footprint and an optional contact band join the region, and a learned effect region comes later |
| AI Remove scope, repair stage, weights licences, training-data provenance | Decided in the [AI editing design](../../docs/design/ai-editing.md#decisions) and [Corrections](../../docs/design/corrections.md) |

## Measurements

Measured on the owner's Apple M4 Pro (`Mac16,8`, 14 cores, 48 GB), macOS 26.5.2 (25F84), `rustc` 1.94.0, release build, `ort` 2.0.0-rc.13 linking ONNX Runtime 1.28.0, Core ML in MLProgram format.

**The host was shared throughout.** Other sessions were compiling and testing; the one-minute load average before each run was 7 to 68, median about 24, on 14 cores. Every figure here is a loaded-host figure. ONNX Runtime's CPU provider spreads each run over every core and suffers most: at low load EfficientViT-SAM encodes in about 160 ms and SAM 2.1 in about 520 ms on the CPU, against the medians below. Each figure is the median over five processes, with the range in brackets where shown. The compared configurations were interleaved, and their order reversed every other round. Memory is `/usr/bin/time -l`'s peak resident set and peak memory footprint for the whole process; the footprint counts the GPU allocations Core ML makes, which the resident set may not. The driver (`campaign.py`), the summaries (`analyze.py`), every raw record and log, and the Core ML diagnostics are in `target/evidence/task-006/`, which is not committed.

### Segmentation

`removal-probe bench` on `z6-bridge.jpg` (24 MP) and `quadrants-60mp.jpg` (60 MP), five processes per image. Each process loads both models, encodes once cold and five times warm, then decodes one point at each of 200 fixed pseudo-random positions. Cold load is five further processes on the bridge, each run after deleting both Core ML caches. Warm load is every process with the caches in place. The CPU provider has no compile cache, and its model files stay in the page cache, so it has only a warm load. Decode percentiles are each process's own, and the table gives their median over the ten processes. The encode includes preprocessing (the antialiased resize and normalization), which takes 6–11 ms at 24 MP and 15–24 ms at 60 MP. "Runs on" comes from `bench --parity`, one process per image, which compares the embedding and all 200 hover masks with ort-cpu's. Output identical to ort-cpu's means ONNX Runtime ran the graph on its CPU provider.

| Family | Backend | Runs on | Cold load s | Warm load s | Encode p50 ms, 24 / 60 MP | Decode p50 / p95 / p99 ms | Peak RSS / footprint MiB, 24 MP |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| evit-l0 | ort-cpu | CPU | – | 0.1 (0.1–0.3) | 301 / 242 | 12.9 / 18.6 / 24.7 | 694 / 677 |
| evit-l0 | ort-coreml-all | CPU (identical output) | 1.0 (0.8–4.3) | 0.2 (0.2–1.0) | 175 / 324 | 20.2 / 25.2 / 30.2 | 724 / 702 |
| evit-l0 | ort-coreml-gpu | CPU (identical output) | 0.8 (0.8–1.5) | 0.2 (0.2–0.9) | 180 / 334 | 22.5 / 28.5 / 34.6 | 725 / 704 |
| evit-l0 | ort-coreml-ane | CPU (identical output) | 0.8 (0.8–4.4) | 0.3 (0.2–1.2) | 188 / 300 | 12.8 / 25.8 / 32.3 | 724 / 703 |
| evit-l0 | ort-coreml-all-fixed | Core ML (mask IoU ≥ 0.998) | 7.2 (5.5–18.7) | 1.5 (1.1–2.4) | 109 / 126 | 6.4 / 14.4 / 26.0 | 722 / 859 |
| evit-l0 | ort-coreml-gpu-fixed | Core ML (mask IoU ≥ 0.998) | 6.8 (4.2–9.5) | 1.2 (1.1–2.3) | 108 / 156 | 5.1 / 12.7 / 25.6 | 722 / 860 |
| evit-l0 | ort-coreml-ane-fixed | Core ML (mask IoU ≥ 0.998) | 5.0 (3.8–10.7) | 1.6 (1.0–2.2) | 177 / 355 | 13.0 / 35.2 / 41.1 | 781 / 578 |
| sam21-small | ort-cpu | CPU | – | 0.1 (0.1–0.3) | 546 / 522 | 12.1 / 22.2 / 24.9 | 2,259 / 2,243 |
| sam21-small | ort-coreml-all | Core ML (mask IoU 1.000) | 53.4 (49.6–81.6) | 10.7 (8.9–19.3) | 162 / 167 | 15.9 / 21.1 / 34.5 | 1,946 / 3,588 |
| sam21-small | ort-coreml-gpu | Core ML (mask IoU 1.000) | 52.2 (34.4–88.7) | 13.5 (9.0–30.0) | 242 / 233 | 22.1 / 44.1 / 86.4 | 1,941 / 3,544 |
| sam21-small | ort-coreml-ane | Core ML (mask IoU 1.000) | 47.5 (34.3–56.9) | 13.9 (8.3–26.7) | 368 / 661 | 31.5 / 38.9 / 42.7 | 4,307 / 1,618 |
| sam21-small | ort-coreml-all-fixed | Core ML (mask IoU 1.000) | 65.0 (51.8–67.2) | 10.8 (9.5–27.4) | 153 / 172 | 5.8 / 8.8 / 14.8 | 1,948 / 3,576 |
| sam21-small | ort-coreml-gpu-fixed | Core ML (mask IoU 1.000) | 47.6 (39.4–58.4) | 11.3 (9.3–25.2) | 182 / 170 | 7.4 / 13.3 / 18.5 | 1,942 / 3,572 |
| sam21-small | ort-coreml-ane-fixed | Core ML (mask IoU 1.000) | 32.9 (32.4–49.8) | 10.5 (9.2–21.1) | 438 / 607 | 13.5 / 35.6 / 40.6 | 4,282 / 1,622 |

A paired set ran the five contenders back to back on the bridge, five rounds with the order reversed each round, at load 13 to 30. EfficientViT-SAM encoded at a p50 of 326 ms on ort-cpu and 120 ms on `ort-coreml-gpu-fixed`, and decoded at 21.9 and 8.2 ms. SAM 2.1 encoded at 955 ms on ort-cpu, 240 ms on `ort-coreml-gpu-fixed` and 155 ms on `ort-coreml-all-fixed`, and decoded at 23.8, 7.4 and 5.7 ms. Core ML's times move much less with the host's load than the CPU provider's.

- **Without fixed shapes, Core ML runs none of EfficientViT-SAM.** The output is bit-identical to ort-cpu's. The provider takes 46 of the encoder's 493 nodes and 98 of the decoder's 422, none of which change a value, and refuses the rest because an input has a free dimension. Those rows are CPU timings plus the provider's overhead.
- **The fixed backends run it on Core ML.** Its hover masks match ort-cpu's (mask IoU at least 0.998 at 24 MP and 0.9999 at 60 MP over 200 points), and its embedding is within 5 × 10⁻⁵. It encodes in about 110 ms at 24 MP and 130–160 ms at 60 MP, and decodes in about 5 ms. The first load writes the fixed copies and compiles them in about 7 s. Later loads take about 1.2 s, most of it the decoder, against 0.1 s on the CPU.
- **SAM 2.1's encoder runs on Core ML in 27 partitions.** The provider refuses 52 `Reshape` and 26 `Transpose` nodes in the Hiera trunk's windowed attention, whose shapes have rank 6; ONNX Runtime's Core ML provider takes rank 5 at most. Its first compile takes 32 to 89 s. Even with both caches in place, a load takes 8 to 30 s, with medians of 10.5 to 13.9 s across the settings, most of it the encoder. Its fixed decoder has 303 of 312 nodes on Core ML. SAM 2.1 on Core ML has a peak footprint of about 3.5 GB, against 2.2 GB on the CPU.
- **The Neural Engine setting is the slowest Core ML setting** for both families. Every row matches ort-cpu to fp32 rounding (at most 5 × 10⁻⁵), so the Neural Engine, which computes in fp16, runs little or none of these fp32 programs; Core ML's own CPU path probably runs the rest.

### Core ML for EfficientViT-SAM

Each attempt used the Core ML provider in MLProgram format unless stated, with ONNX Runtime's verbose partition log (`LUXFORGE_REMOVAL_ORT_LOG=verbose`) in `target/evidence/task-006/coreml-evit/`:

| Attempt | Encoder | Decoder |
| --- | --- | --- |
| As pinned, static shapes required | Runs; 46 of 493 nodes on Core ML; output identical to the CPU | Runs; 98 of 422; identical |
| Free-dimension overrides (`batch_size`, `num_points` at 1) on the Core ML session, at the basic or the full optimization level | Takes 437 of 440, then fails at run time: `Feature _image_encoder_neck_input_ops_2_op_list_1_Shape_output_0 is required but not specified` | Takes 278 of 289, then fails: `Feature _Shape_1_output_0 is required but not specified` |
| The same overrides in the NeuralNetwork format | Runs, 429 of 440 in 6 partitions, about 100 ms, but the embedding differs by up to 0.08 (fp16) | Fails: `Unable to compute the prediction using a neural network model` |
| Dynamic shapes allowed (`RequireStaticInputShapes` off) | Fails to build: `…Slice_1 has unbounded dimension`, and a rank-0 `squeeze` | Not tried |
| **ONNX Runtime writes a fixed copy on the CPU, and Core ML loads the copy** (the `-fixed` backends) | Runs; 437 of 440 in 2 partitions (the rest are 3 bicubic `Resize` nodes); within 2–5 × 10⁻⁵ | Runs; 270 of 281 in 3 partitions; masks within 1.2 × 10⁻⁵, IoU 1.0 |

The failing and the working attempt fix the same dimensions; they differ only in where the overrides apply. Within one session the provider compiles partitions that expect a `Shape` node's output as an input nothing feeds. Saved and reloaded, the shape computations are constants. Why ONNX Runtime 1.28 does this within one session was not established. No Python and no re-export is involved.

LaMa, fixed the same way, has 2,518 of its 3,365 nodes on Core ML, in 188 partitions. Without fixing, the provider takes 2,736 of 8,441 nodes and the output is identical to the CPU's. It refuses LaMa's blocked `DequantizeLinear` (299 nodes), `Einsum` (216), some `Conv` (77), `Gather` (72), `Neg` (36) and `ConvTranspose` (3).

### Hover end to end

`removal-lab --script scripts/bridge.lab` on the bridge, five measured processes per row after one process that fills the caches. The script moves the pointer 182 times at 8 ms intervals, clicks, undoes and strokes. Without a window (`--script`), the runner applies results as they arrive. In a hidden window (`--script --hidden-window`), a copy of the binary runs inside an `LSBackgroundOnly` bundle, and the window's update, image handles and canvas draw run. The front application, checked every second with `lsappinfo`, was never the lab. Percentiles pool every hover result applied in the five processes; the other pointer moves were overtaken before an answer arrived. "Pointer to draw" ends at the draw call of the first frame with the overlay, before the frame is presented.

| Family | Backend | Window | Results | Pointer to result p50 / p95 / p99 ms | Pointer to draw p50 / p95 / p99 ms | Decode p50 ms |
| --- | --- | --- | ---: | --- | --- | ---: |
| evit-l0 | ort-coreml-gpu-fixed | none | 642 | 14.2 / 27.1 / 58.3 | – | 8.5 |
| sam21-small | ort-coreml-gpu-fixed | none | 530 | 15.6 / 34.8 / 60.2 | – | 11.0 |
| evit-l0 | ort-cpu | none | 420 | 19.8 / 37.7 / 61.1 | – | 13.7 |
| sam21-small | ort-cpu | none | 353 | 27.9 / 42.8 / 52.3 | – | 22.8 |
| evit-l0 | ort-coreml-gpu-fixed | hidden | 792 | 20.5 / 34.5 / 51.0 | 20.3 / 33.0 / 50.5 | 6.2 |
| sam21-small | ort-coreml-gpu-fixed | hidden | 799 | 20.6 / 34.9 / 49.2 | 20.3 / 34.9 / 49.4 | 6.4 |
| evit-l0 | ort-cpu | hidden | 527 | 25.0 / 44.6 / 54.7 | 25.1 / 45.4 / 54.8 | 10.2 |
| sam21-small | ort-cpu | hidden | 438 | 30.4 / 46.3 / 57.3 | 30.5 / 46.4 / 57.5 | 13.8 |

In the lab the segmenter loads while the image opens. The segmenter was ready after about 1.2 s (1.1–3.9 s) for EfficientViT-SAM on Core ML and 10–20 s for SAM 2.1 on Core ML, against 0.1–0.4 s on the CPU. The first encode after a load took about 200 ms (156–879) for EfficientViT-SAM on Core ML and about 360 ms (318–1,466) for SAM 2.1, which is slower than the warm encodes above.

### Fill

`removal-probe remove` with EfficientViT-SAM on ort-cpu choosing the object, then LaMa on ort-cpu or on `ort-coreml-gpu-fixed`, five processes per row. The quadrants have no small objects, so a stroke picks the small and medium regions there. With `--param min_object_coverage=0.5` the quadrant under the stroke is refused. The short stroke still found a box-shaped object the size of the stroke, and the medium one found nothing and removed its footprint. The region column is the refined region (upsampling, core, dilation and feather). The rest of the fill is the crop and resample, upscale, grain, blend and composite together. Every fill left every pixel outside its region unchanged.

| Image | Object | Region px | Crop side px, enlargement | Region ms | LaMa ms, CPU | Rest of fill ms, CPU | Fill ms, CPU | Footprint MiB, CPU | LaMa ms, Core ML fixed | Fill ms, Core ML fixed | Footprint MiB, Core ML fixed |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Bridge, 24 MP | flag | 35,393 | 1,011, 2.0× | 3 | 1,665 (1,182–2,829) | 36 | 1,694 (1,201–2,873) | 1,174 | 3,014 (1,825–4,111) | 3,054 (1,848–4,180) | 8,757 |
| Bridge, 24 MP | boy | 578,443 | 4,572, 8.9× | 29 | 1,821 (1,194–2,467) | 173 | 1,997 (1,329–2,692) | 1,518 | 1,601 (1,320–3,158) | 1,753 (1,448–3,483) | 8,974 |
| Bridge, 24 MP | tall pole | 408,246 | 6,048, 19.7× | 38 | 1,463 (1,211–2,110) | 299 | 1,764 (1,406–2,613) | 1,658 | 1,648 (1,264–3,800) | 1,962 (1,483–4,307) | 9,117 |
| Sunflowers, 40 MP | sign | 302,982 | 2,226, 4.3× | 12 | 1,799 (1,197–2,129) | 98 | 1,892 (1,262–2,316) | 1,426 | 1,746 (1,536–1,925) | 1,821 (1,620–2,014) | 9,017 |
| Sunflowers, 40 MP | middle flower | 2,098,908 | 5,214, 10.2× | 154 | 1,833 (1,231–2,133) | 358 | 2,195 (1,466–2,866) | 1,824 | 2,641 (1,350–4,400) | 2,993 (1,568–4,742) | 9,443 |
| Sunflowers, 40 MP | right flower | 3,018,996 | 6,651, 13.0× | 269 | 1,662 (1,387–2,259) | 608 | 2,256 (1,979–3,258) | 2,150 | 1,962 (1,260–2,476) | 2,352 (1,799–3,168) | 9,724 |
| Kayaks, 20 MP | boat | 8,508 | 486, 0.9× | 1 | 1,937 (1,407–2,477) | 18 | 1,957 (1,417–2,502) | 1,133 | 1,791 (1,379–2,562) | 1,803 (1,394–2,582) | 8,714 |
| Kayaks, 20 MP | rock | 550,492 | 4,629, 9.0× | 42 | 2,017 (1,611–2,279) | 253 | 2,305 (1,809–2,702) | 1,505 | 1,485 (1,343–2,354) | 1,640 (1,491–2,631) | 9,012 |
| Kayaks, 20 MP | water | 1,673,254 | 5,464, 11.7× | 106 | 1,952 (1,221–2,295) | 415 | 2,446 (1,494–2,713) | 1,776 | 1,572 (1,433–2,035) | 1,888 (1,778–2,538) | 9,285 |
| Quadrants, 60 MP | short stroke | 38,955 | 1,200, 2.3× | 3 | 1,846 (1,430–2,439) | 64 | 2,053 (1,463–2,521) | 1,612 | 2,599 (1,356–4,021) | 2,637 (1,380–4,075) | 9,203 |
| Quadrants, 60 MP | long stroke | 430,688 | 4,632, 9.0× | 6 | 1,973 (1,165–2,040) | 341 | 2,338 (1,245–2,579) | 1,805 | 1,981 (1,339–3,608) | 2,084 (1,420–3,859) | 9,266 |
| Quadrants, 60 MP | a quadrant | 15,398,117 | 10,000, 29.8× | 464 | 1,957 (1,246–2,106) | 961 | 2,994 (1,854–3,959) | 3,455 | 1,614 (1,366–3,436) | 2,305 (1,978–4,971) | 11,021 |

- The LaMa run is 1.5–2.0 s whatever the object on this loaded host. In the paired set on the boy it was 1,231 ms (1,194–1,383) on the CPU, which matches the 1.1 s of the unloaded parity run. A fill takes 1.7–3.0 s. The region adds up to 0.5 s, for the 15 MP quadrant. Everything else grows with the region and stays under 1 s.
- **Core ML is no faster for LaMa and holds six times the memory.** In the paired set LaMa ran in 2,204 ms (1,494–3,134) on `ort-coreml-gpu-fixed` against 1,231 ms on the CPU, with a peak footprint of 9.0 GB against 1.5 GB. Without fixing (`ort-coreml-gpu`, ten runs on the boy and the quadrant), LaMa ran in 1,551 ms (1,233–2,344), with the CPU's footprint, since none of it runs on Core ML.
- **LaMa load:** on the CPU, 1.8–8.1 s, median 3.0 s under load and 1.8 s in the paired set. On `ort-coreml-gpu-fixed`, 2.6 s warm and 31–39 s cold (median 35 s). On `ort-coreml-gpu`, 8.4 s warm and 18–51 s cold, although it runs none of the model.
- **Enlargement.** The pinned LaMa takes a fixed 512² input, and the crop is three times the region's longer side, so that side sets the enlargement: 8.9× for the boy, 9–13× for the other medium and large objects, 19.7× for the pole and 29.8× for a 60 MP quadrant. Grain is re-synthesized, but structure is not. The bridge deck under the boy is rebuilt visibly softer than the deck around it.

### Decisions on the corpus

`remove` on ort-cpu with both families. Candidates are listed with their predicted IoU. EfficientViT-SAM is shown at its default floor of 0.7, and SAM 2.1 at 0.7 and at its default of 0.6. At 0.6 EfficientViT-SAM's decisions did not change.

| Image | Stroke or point | EfficientViT-SAM L0 | SAM 2.1 Small |
| --- | --- | --- | --- |
| Bridge | Point on the boy | One: the boy (0.74). Right; the bag he carries is not in the mask and stays, floating | One: the whole boy (0.61), taken as the whole over his clothes (0.81). Right; the bag stays |
| Bridge | Stroke over the boy, radius 80 | One: the boy. Right | One: the whole boy. Right |
| Bridge | Stroke across the two walkers behind him, radius 30 | Nothing: it finds one walker (0.76), who covers 31 % of the stroke, so only the painted band goes and their heads and legs stay. Wrong | At 0.7 nothing. At 0.6, several: both walkers (0.61, and the woman as a whole at 0.53), and `--pick 1,2` removes both; the bag between them stays. Right at 0.6 |
| Sunflowers | Zig-zag over the right flower, radius 100 | One: the flower and its leaf (2.6 MP of mask). Right; a yellow smudge where the fill meets the middle flower's petals | One: the flower and its leaf (2.4 MP). Right |
| Kayaks | Stroke through three kayaks on teal water, radius 12 | Nothing: every point answers with the water (2.2 to 3.6 MP). Wrong | At 0.7 and 0.65 nothing. At 0.6, several: two of the three (0.70, 0.65), and `--pick 1,2` removes both cleanly |
| Kayaks | Stroke through three kayaks in the lagoon, radius 12 | Several: two of the three (0.73, 0.72) | Several: two of the three (0.81, 0.79) |
| Kayaks | Stroke across two nearby kayaks, radius 12 | One: the box prompt's mask spans both kayaks (0.70), so both are removed as one object | Several: both kayaks. Right |
| Kayaks | Short stroke along one orange kayak, radius 10 | One. Right | Nothing: the kayak (0.74) covers only 62 % of a brush wider than the kayak, so only the painted area goes. Wrong |
| Kayaks | Short stroke on a yellow kayak, radius 10 | Nothing: the points answer with the water. Wrong | One. Right |
| Road | Stroke along one bend, radius 35 | One: the whole road across the frame, of which the stroke covers 15 % | The same |

EfficientViT-SAM's misses are its decoder's answers, since every point on a kayak in dark or uniform water returns the water, and no floor changes them. SAM 2.1 finds every small object here but scores them lower, which is why its floor is 0.6. Its tighter masks can leave a brush wider than a thin object short of the 70 % that "one" needs. The road is a question of intent rather than a threshold: removing the road the stroke explains is the Lightroom-like reading, and the fill over 3389 px is a blurry smear at 6.6 source pixels per model pixel.

## Generative fill

The qualification of a local generative fill: whether a model built within the one-toolchain rule fills photo regions well on the owner's M4 Pro, and at what time and memory cost. Measured on the same Apple M4 Pro (`Mac16,8`, 14 cores, 48 GB; Metal reports a 40.2 GB recommended working set and no tensor units), macOS 26.5.2, `rustc` 1.94.0, release builds. **The host was shared**: the one-minute load average before each run was 4 to 84, recorded per run below. The klein runs are bound by the GPU, so load moved them little (a 512 px candidate took 32 s at a load of 5 and 34 to 35 s at 30 to 80); Moebius on the CPU suffered most. Every figure is a single process; memory is `/usr/bin/time -l`'s peak footprint for the whole process, which includes the decoded photograph. The raw JSON, logs, crops and sheets are in `target/evidence/task-005/`, which is not committed. The tables were assembled from that JSON by a one-off Python script that is not part of the probe and never ships.

```sh
P=probes/object-removal/target/release/removal-probe
E=probes/object-removal/target/evidence/task-005
# Record the removal prompt's conditioning once, with the text encoder loaded.
$P generate --candidate klein-q4-lora --encoder q8 --record-condition $E/conditions/lora-q8enc.bin --case sky-clouds-blue --out $E/explore
# Prompt-free removal over every case, three candidates in sequence each, then the contact sheets.
$P generate --candidate klein-q4-lora --condition $E/conditions/lora-q8enc.bin --case all --seeds 3 --size 512 --out $E/campaign
$P generate --candidate lama --case all --out $E/campaign
$P sheet --dir $E/campaign --columns lama-512,klein-q4-lora-512
# Replace with a prompt, composited over the region's own edge.
$P generate --candidate klein-q4 --encoder q8 --reference outlined --no-blend --prompt "Replace the highlighted object with a red hot air balloon" --case sky-clouds-blue --out $E/replace
```

### The build route

stable-diffusion.cpp (MIT) and its ggml (MIT) are vendored as pinned source subsets in [`crates/luxforge-diffusion`](../../crates/luxforge-diffusion), where the editor's generative runtime builds them, and compiled by its `native/build.rs`, which the probe's `diffusion/build.rs` includes, with the `cc` crate, as `crates/luxforge-raw` compiles LibRaw: Metal with its kernels embedded, the CPU backend and Accelerate, `-mcpu=apple-m1` so the library runs on every M-series Mac. No CMake, no Python, no second build system and no network access; the 139 MB of generated tokenizer tables are left out (a text encoder takes its `tokenizer.json`). **The rule holds.** Two local changes are staged by exact-anchor replacement: one lets a recorded conditioning stand in for the text encoder, the other lets ggml's Metal backend run a graph in slices so a generation can share the GPU ([THIRD_PARTY.md](../../crates/luxforge-diffusion/THIRD_PARTY.md)). Its six static libraries total 19.5 MB. Metal compiles the kernels at the first generation on a machine (16.4 s) and caches them (15 ms after).

Moebius is a UNet with a learned 20-token embedding in place of a text encoder, which stable-diffusion.cpp does not load, so it runs over the probe's existing ONNX Runtime from simonw's fp32, static 512² export, with the reference pipeline's DDIM schedule (20 steps at strength 0.99, so 19 run, each twice for guidance 2.0) reimplemented in `generative.rs`. MLX was not tried: stable-diffusion.cpp met the rule, and an MLX route would be macOS-only.

### What was run

- **klein 4B** (Q4_0 and Q8_0 GGUF, bf16 VAE), four flow steps, CFG 1, strength 1, through stable-diffusion.cpp's masked path: the crop goes in with the region mid-grey, sampling starts from noise, and the known latents are restored after every step. Four ways of conditioning it were compared on three cases (bridge deck, pub, blue sky): **no reference** leaves a flat, pale rectangle with a hatched texture, and **the grey-holed crop as the reference** makes the model copy the grey; both are dropped. **The crop with the region outlined in red as the reference**, the input fal's LoRA was trained on, works with or without the LoRA, and the LoRA is the better of the two on 16 cases (below).
- **fal's removal LoRA** (Apache-2.0, trained on klein *base*; applied here to the distilled 4-step klein at multiplier 1.0) with its prompt "Remove the highlighted object from the scene".
- **Prompt-free**: every removal run took that prompt's conditioning recorded once from Qwen3-4B Q8_0 (`--record-condition`) and supplied from a file (`--condition`), with no text encoder loaded.
- **Moebius** on `ort-cpu`, `ort-coreml-gpu-fixed` and `ort-coreml-all-fixed` (the same score on the one case run on all three; the last is fastest and is the one in the tables).
- **LaMa** at 512, the Fast fill, as the baseline.
- OSOR-SDXL, RORem, ObjectClear, TurboClear and a non-commercial reference ceiling were not run.

Every candidate ran inside the Fast fill's pipeline: the same crop (the region's longer side tripled, squared, resampled to the model's size), the model, upscaling to the crop, grain re-synthesis, the pyramid blend (seam correction and Laplacian detail blend) and the composite. **Every pixel outside every region was unchanged in every run** (0 changed pixels, checked by `generate` on each candidate). **The pyramid blend helps a removal and spoils a Replace.** Without it (`--no-blend`, the region's own feathered edge), klein Q4 + LoRA scores 19.60 dB / 0.573 in the region and 24.32 dB / 0.649 in the edge ring over the 16 cases, against 20.16 / 0.578 and 26.60 / 0.670 with it; LaMa loses less (25.31 against 25.91 dB in the ring). For Replace, the seam correction carries the difference between the original and the new object's surroundings into the region, which shows as a lighter box around the balloon and a faint halo around the boat and the dog; without the blend both sit seamlessly (`replace/sheets/`).

### Quality against LaMa on the 16 pasted cases

One candidate (seed 0) per case at 512, scored against the clean image: PSNR (dB) / SSIM inside the region.

| Case | LaMa | klein Q4 + LoRA | klein Q8 + LoRA | klein Q4, outlined, no LoRA | Moebius |
| --- | ---: | ---: | ---: | ---: | ---: |
| bridge-deck | 21.87 / 0.802 | 22.55 / 0.822 | 22.66 / 0.816 | 22.37 / 0.820 | 20.87 / 0.795 |
| sunflowers-bokeh | 19.99 / 0.805 | 17.83 / 0.789 | 17.91 / 0.790 | 17.00 / 0.768 | 14.59 / 0.739 |
| kayaks-water | 17.82 / 0.171 | 17.54 / 0.209 | 17.43 / 0.211 | 17.55 / 0.211 | 17.71 / 0.180 |
| road-terraces | 14.50 / 0.240 | 14.66 / 0.259 | 14.67 / 0.261 | 14.72 / 0.260 | 12.94 / 0.234 |
| people-utrecht-pub | 15.00 / 0.541 | 15.41 / 0.552 | 15.57 / 0.553 | 15.23 / 0.538 | 13.37 / 0.443 |
| people-olinda-craftsman | 17.62 / 0.415 | 19.32 / 0.470 | 19.21 / 0.467 | 18.41 / 0.458 | 17.48 / 0.422 |
| people-juan-griego-beach | 24.77 / 0.515 | 24.73 / 0.529 | 24.79 / 0.529 | 25.03 / 0.529 | 24.44 / 0.518 |
| sky-clouds-blue | 24.29 / 0.944 | 21.44 / 0.929 | 22.09 / 0.932 | 19.44 / 0.914 | 22.99 / 0.926 |
| sky-evening-2024 | 21.89 / 0.791 | 21.49 / 0.791 | 21.70 / 0.789 | 20.75 / 0.789 | 21.27 / 0.781 |
| sky-clouds-atmosphere | 30.98 / 0.707 | 30.49 / 0.710 | 30.73 / 0.709 | 30.59 / 0.703 | 30.90 / 0.720 |
| water-minnewanka | 18.96 / 0.531 | 23.20 / 0.610 | 23.32 / 0.614 | 22.93 / 0.606 | 19.03 / 0.550 |
| water-badukskoe | 14.71 / 0.118 | 15.00 / 0.145 | 14.92 / 0.146 | 15.05 / 0.145 | 13.42 / 0.122 |
| water-rhine-eltville | 19.69 / 0.695 | 17.65 / 0.692 | 17.39 / 0.692 | 15.85 / 0.675 | 14.96 / 0.641 |
| object-green-heron | 32.82 / 0.949 | 32.66 / 0.937 | 32.49 / 0.936 | 33.07 / 0.934 | 29.13 / 0.945 |
| object-razorbill | 18.87 / 0.737 | 18.05 / 0.724 | 18.20 / 0.725 | 18.45 / 0.720 | 17.89 / 0.724 |
| object-bicycle-bk | 10.72 / 0.065 | 10.61 / 0.073 | 10.75 / 0.074 | 10.53 / 0.074 | 10.18 / 0.065 |

| Candidate | Mean region dB / SSIM | Mean edge ring dB / SSIM | Region dB above LaMa | Region SSIM above LaMa |
| --- | ---: | ---: | ---: | ---: |
| LaMa 512 | 20.28 / 0.564 | 25.91 / 0.643 | — | — |
| klein Q4 + LoRA | 20.16 / 0.578 | 26.60 / 0.670 | 6 of 16 | 11 of 16 |
| klein Q8 + LoRA | 20.24 / 0.578 | 26.50 / 0.670 | 8 of 16 | 10 of 16 |
| klein Q4, outlined, no LoRA | 19.81 / 0.572 | 26.39 / 0.667 | 8 of 16 | 8 of 16 |
| Moebius | 18.82 / 0.550 | 24.07 / 0.615 | 1 of 16 | 6 of 16 |

PSNR against the one true background understates a generative fill, which invents a plausible background rather than the one that was there; the sheets (`target/evidence/task-005/campaign/sheets/CASE.png`: input, truth, LaMa, Moebius, klein outlined, klein Q4 + LoRA, klein Q8 + LoRA) show the difference. **klein rebuilds structure that LaMa smears**: the road through the terraces continues across the region (LaMa leaves a green smear, Moebius a broken stub); the hidden sunflower's petals and centre are completed; the bicycle's rear wheel and kickstand are redrawn; the pub's second walker gets a head and shoulders and the window lettering continues; the moored boat on Minnewanka is rebuilt (+4.2 dB over LaMa). **Where it is weaker**: on open sky it paints a soft, smooth cloud that is blurrier than the clouds around it (blue sky −2.9 dB, Rhine sunset −2.0 dB), and it scores 2.2 dB under LaMa on the sunflowers, where it completes the hidden flower in a different pose from the truth. **Moebius** is below LaMa on 15 of 16 cases by PSNR, smears structure (the road, the bicycle, the pub window) and is 20 times slower; on the CPU it takes 2 to 3.5 minutes a candidate. Q8 is indistinguishable from Q4 (mean +0.08 dB, equal SSIM).

### Time, memory and download

`generate --seeds 3` on the blue-sky case, three candidates in sequence in one process. "First" includes the lazy weight load (stable-diffusion.cpp maps the file at open and loads weights at the first generation; the operating system's file cache was warm, and a cold read from disk was not measured).

| Candidate | Size | Open ms | First candidate ms | Warm candidates ms | Three in sequence s | Step ms (median) | Peak footprint GB | Load avg before / after |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | --- |
| LaMa (ort-cpu) | 512 | 3,852 | 2,063 | 2,157, 2,087 | 7.1 | — | 0.94 | 15 / 21 |
| Moebius (Core ML all, fixed) | 512 | 14,947 | 37,763 | 27,093, 34,351 | 99.7 | 1,831 (two UNet runs) | 5.18 | 51 / 43 |
| Moebius (ort-cpu) | 512 | 13,005 | 135,251 | 210,237, 169,017 | 515.5 | 8,284 | 6.21 | 43 / 36 |
| klein Q4 + LoRA | 512 | 174 | 39,315 | 35,208, 34,287 | 110.6 | 7,069 | 4.18 | 36 / 57 |
| klein Q4 + LoRA | 768 | 83 | 76,204 | 72,678, 74,894 | 225.4 | 14,775 | 4.19 | 84 / 36 |
| klein Q4 + LoRA | 1024 | 51 | 138,618 | 139,044, 138,144 | 418.1 | 27,899 | 4.25 | 12 / 36 |
| klein Q4 + LoRA, reference encoded at 512 | 1024 | 51 | 80,712 | 79,715, 81,793 | 242.7 | 15,272 | 4.13 | 5 / 6 |
| klein Q8 + LoRA | 512 | 91 | 38,411 | 34,697, 36,055 | 112.1 | 7,168 | 6.65 | 57 / 84 |
| klein Q8 + LoRA | 768 | 150 | 81,515 | 73,754, 68,632 | 224.8 | 14,693 | 6.68 | 36 / 12 |
| klein Q8 + LoRA | 1024 | 172 | 143,156 | 134,804, 181,843 | 461.5 | 28,575 | 6.68 | 36 / 31 |
| klein Q4 + LoRA, Qwen3-4B Q4_K_M encoder loaded | 512 | 597 | 36,128 | 32,570, 33,969 | 103.1 | 6,969 | 6.63 | 31 / 15 |

- **klein's step time grows with its token count**: the image, the reference image and 512 text tokens are all in every step, about 2.5 to 3.2 ms per token per step. Four steps are four fifths of a candidate; the rest is the VAE (encoding the crop and the reference, decoding the result). Encoding the reference at half the working size (`--reference-size 512` at 1024; the shim tells the runtime to encode it at its own size, since by default it is resized to the output's area) cuts a 1024 px candidate from 127 to 139 s to 79 to 84 s, but the fill no longer lines up with its surroundings: on four cases the edge ring fell by 2.0 to 10.0 dB (the boat on Minnewanka from 24.9 to 20.0 dB in the region). A 256 px reference at 512 does the same (34 s to 23 to 27 s; the boat from 23.2 to 19.8 dB), so the reference stays at the working size. Working at 1024 rather than 512 sharpens the fill: on the same four cases SSIM rose on three (the boat from 0.610 to 0.699, the pub from 0.552 to 0.571, the bicycle from 0.073 to 0.130) for four times the time.
- **Memory** does not grow with size: stable-diffusion.cpp reserves its compute buffers up front (2 GiB for the transformer, 1 GiB for the VAE). Without a text encoder klein Q4 peaks at 4.1 to 4.25 GB, Q8 at 6.7 GB. With the text encoder for Replace, Q4 peaks at 6.6 GB with Qwen3-4B Q4_K_M and 7.9 to 8.4 GB with Q8_0.
- **Downloads**: klein Q4 with its VAE, the LoRA and the shipped conditioning 2.72 GB (Q8 4.56 GB); the optional text encoder 4.29 GB at Q8_0 with its tokenizer, 2.51 GB at Q4_K_M; Moebius 1.24 GB; LaMa 93 MB.

### Quiet-host remeasurement

Measured on a quiet host, with no agent running and the one-minute load average 0.6 to 3.8 before and after every run, on the same M4 Pro, release build and blue-sky case: `generate --candidate klein-q4-lora --condition … --seeds 3 --size N`, at 512, 768 and 1024 and then in reverse order, each run a process of its own measured by `/usr/bin/time -l`. The evidence is in the main checkout's ignored `private/object-removal/evidence/task-005-quiet/`.

| Size | Per candidate ms (two runs) | Three in sequence s (two runs, whole process) | Peak footprint GB | Load before / after |
| --- | --- | ---: | ---: | --- |
| 512 | 32,542, 30,950, 30,952; 32,372, 30,947, 30,950 | 115.4 (first run, with a 14.4 s first load and Metal compile), 99.5 | 4.14 to 4.17 | 3.8 / 1.8; 1.3 / 0.6 |
| 768 | 68,311, 67,527, 67,064; 67,869, 66,461, 66,658 | 208.2, 206.3 | 4.15 | 1.8 / 1.5; 2.2 / 1.3 |
| 1024 | 123,024, 121,575, 121,302; 123,514, 121,450, 121,505 | 371.8, 371.2 | 4.21 to 4.22 | 1.5 / 0.8; 0.8 / 2.2 |

The two runs at each size agree within 1.5%, and the same seeds gave the same scores in both, so the candidates are reproducible on this machine. A quiet host is about 10% faster than the loaded runs above (1024: 122 s against 138 s), as expected of a run bound by the GPU: these are the costs on the M4 Pro, and the generative budget is set from them ([AI editing](../../docs/design/ai-editing.md#decisions)). A 512 px candidate takes 31 s.

### The shipped empty-scene embedding

The conditioning is the text encoder's output for a fixed prompt, a 512 × 7680 tensor (15.7 MB as f32). Recorded once and supplied from a file with no text encoder loaded, it **reproduces the prompted removal byte for byte**: on three cases at seed 0 the candidates are identical files to those of the run that encoded the prompt with Qwen3-4B Q8_0, and the same holds for the plain klein run against its own Q4_K_M encoding. Without the encoder the process peaks 2.4 GB lower (4.1 against 6.5 GB) and loads in 50 to 170 ms. The tolerance for an embedding computed by a different encoder build (a different quantisation, or a later runtime) is set by what a person could tell apart: per seed, at most 0.5 dB region PSNR and 0.005 SSIM from the prompted candidate, about a seventh of the seed-to-seed spread (3.3 dB between seeds 0 and 1 on the blue sky), so the difference is smaller than a Regenerate. The Q4_K_M encoder's embedding against the Q8_0 one met it: 21.22, 24.71 and 24.96 dB against 21.44, 24.75 and 24.92 dB, SSIM within 0.001.

### Replace with a prompt

klein Q4 with the crop outlined as the reference and Qwen3-4B Q8_0 encoding "Replace the highlighted object with …" (`target/evidence/task-005/replace/CASE/`). The model output places the asked-for object in a coherent scene: a red hot-air balloon among the clouds, a rowing boat on the lake, a dog on the bridge deck, an olive tree in the pub window. It also redraws what the object overlapped: the walker behind the pub's olive tree loses his head to its branches. With the pyramid blend the composite leaves a lighter box around the balloon and a faint halo around the boat and the dog; without it the balloon and the boat sit seamlessly. A Replace takes the time of a removal at its size (34 to 37 s at 512, 133 s at 1024) and peaks at 7.9 GB at 512 and 8.1 GB at 1024 with the Q8_0 encoder loaded.

### Go or no-go

| Model | Runtime and build route | Verdict | Minimum machine memory |
| --- | --- | --- | --- |
| **FLUX.2 klein 4B Q4_0 + fal's removal LoRA**, outlined reference, prompt-free | stable-diffusion.cpp `master-956-1b0ba10` with ggml `d25b121c`, Metal, vendored and built with `cc` | **Go on quality**: it rebuilds structure no other candidate does, at LaMa's level or better on the scores. Time: 35 s per candidate at 512, 74 s at 768, 139 s at 1024 (three in sequence 1.8, 3.8 and 7.0 minutes); the budget is the measured time at 1024 px, one candidate at a time ([AI editing](../../docs/design/ai-editing.md#decisions)) | Measured peak 4.25 GB without the text encoder, 8.1 to 8.4 GB with Replace's Q8_0 encoder. Accepted: 16 GB for removal, 24 GB for Replace ([AI editing](../../docs/design/ai-editing.md#decisions)); not yet measured on a smaller machine |
| FLUX.2 klein 4B Q8_0 + LoRA | as above | No-go: no measurable gain over Q4 for 1.8 GB more download and 2.4 GB more memory | — |
| FLUX.2 klein 4B without a reference (masked path only) | as above | No-go: a flat, hatched rectangle | — |
| Moebius (`ft_places2`) | ONNX Runtime, Core ML | No-go: below LaMa on 15 of 16 cases, no structure, 25 to 35 s a candidate | — |

The resource drafts for the model that passed, its files and its optional text encoder, are in [`resources/generative.json`](resources/generative.json).

### The editor's generative fill

The editor runs klein through `crates/luxforge-diffusion` ([AI editing](../../docs/design/ai-editing.md#the-delivered-workflow)). `generate --dump-input` writes the 1024 px crop and hole the harness gave the model (`-input.png`, `-input-hole.png`), which that crate's ignored native test feeds to the editor's generator and holds to the harness's candidate byte for byte. [`scripts/generative_kit.py`](scripts/generative_kit.py) drives the editor's own `luxforge-json`, never the GUI, over the cases in [`resources/generative-kit.json`](resources/generative-kit.json): the boy on the bridge, a real object, and two pasted corpus cases, each a brush stroke as the editor takes it. It needs Python 3 with Pillow and NumPy (`stage` needs Python 3 alone), and it never downloads.

```sh
K=probes/object-removal/scripts/generative_kit.py
P=probes/object-removal/target/release/removal-probe
J=target/release/luxforge-json
E=private/object-removal/evidence/task-013/editor   # in the main checkout, ignored
python3 $K prepare --cases probes/object-removal/resources/generative-kit.json --probe $P \
  --corpus private/object-removal/corpus --public private/object-removal/public --out $E
python3 $K stage --json $J --models private/models/generative --root $E/root
python3 $K editor --json $J --root $E/root --cases probes/object-removal/resources/generative-kit.json --out $E
# One candidate a case, or a Replace and then a removal in one process (its own --out):
python3 $K editor ... --once [--only ID,ID]
python3 $K editor ... --only water-minnewanka --prompt "a wooden rowing boat" --out OTHER
python3 $K lama --probe $P --out $E
python3 $K sheets --out $E [--lightroom DIR]
```

`prepare` writes each case's upright input JPEG (quality 95), its stroke and the stroke's footprint as a mask. `stage` puts klein's pinned files from the local models directory into the data root's install staging, under the pins `module.list` declares, so the ordinary `module.resource.install` job hashes each file at its pinned length and fetches nothing; this is how the generator resource installs before the owner publishes its conditioning release asset. It first checks each local file against its pinned length and SHA-256 and stops on one that differs, clones it (`cp -c`, no extra disk on APFS), and skips a resource already installed. `--resources DIR` stages into a resources directory itself instead of a data root's, listing the pins from a temporary root so nothing else is written there: for the desktop, its own resources directory, after which Download in Settings › Models (with the **Generative fill** experiment on) installs from the staged files and fetches none of them. It is the developer and testing route until the release asset is published ([user guide](../../docs/user-guide.md)):

```sh
python3 $K stage --json $J --models ~/projects/luxforge/private/models/generative \
  --resources "$HOME/Library/Application Support/Luxforge/modules/resources"
```

`editor` runs one `luxforge-json` process per case under `/usr/bin/time -l`: it installs under the download grant, turns `ai.generative` on, opens the photograph, sends Generate (seed 0) and Regenerate (the answer's `next_seed`), or Generate alone with `--once`, or with `--prompt` a Replace and then a prompt-free removal from the same seed, then accepts and exports each candidate in turn (a Replace as `ID-replace-seedN.jpg`), undoing between them, and writes `editor.json` with each request's time and step progress, the runtime slots held after each request, the process's wall time, maximum resident set and peak footprint, and `uptime` before and after. `lama` fills the same footprint with the Fast fill (`fill --mask`), and `sheets` builds the blinded sheets.

Measured on the M4 Pro (`Mac16,8`, 48 GB, macOS 26.5.2), release build, **on a shared host** (one-minute load average 20 to 56, recorded per run in `editor.json`), at 1024 px, seed 0 then Regenerate's seed 1, each case one process, with the selection's feather inside the painted footprint (below); the evidence is in the main checkout's ignored `private/object-removal/evidence/task-013/editor/` (`klein/` the exports, `lama/`, `sheets/`, `editor.json`):

| Case | Region | Generate s | Regenerate s | Steps 1 to 4 reported at s (Generate) | Peak footprint GB | Load before / after |
| --- | --- | ---: | ---: | --- | ---: | --- |
| bridge-boy (the process's first generation, after the install) | 661 × 1821 | 153.2 | 140.7 | 43.4, 73.5, 104.7, 137.6 | 4.32 | 46.7 / 20.0 |
| water-minnewanka | 745 × 359 | 142.4 | 139.6 | 44.6, 72.3, 100.9, 129.6 | 4.10 | 22.0 / 35.7 |
| people-utrecht-pub | 778 × 372 | 143.6 | 138.6 | 46.1, 74.2, 102.0, 129.4 | 4.00 | 35.7 / 51.9 |

Each step takes 28 to 31 s; before the first, the job reads the photograph and encodes the crop and the reference (about 15 s), and after the last it decodes and finishes the candidate (13 to 16 s). The runtime held `run-klein` at its declared 5 GiB, and the process's peak footprint stayed under it. Installing the generator from staged files took 18.1 s (hashing 2.72 GB, the format checks and the Metal warm-up) and the encoder 2.0 s. The same Generate and Regenerate sent to the background desktop through `luxforge-ctl` on the Minnewanka case (seeds 10 and 11, `private/object-removal/evidence/task-013/desktop/`) took 169 and 146 s at a load of 28 to 36, with the desktop's resident set peaking at 4.0 GB; the accepted candidate exported as the JSON runs' did.

What the images show: klein rebuilds what the region hid where the Fast fill smears it (the bridge's deck and rails, the moored boat's roof and mast, a walker in the pub window, whose lettering it garbles). **One failure**: on the bridge, Regenerate's seed 1 redrew the boy, softer, instead of removing him, so Regenerate is the remedy rather than a variation. With the feather inside the footprint, both pasted cases (all four candidates, and the desktop's) showed a faint red line along the region's top edge, about 18 px inside it, where the pasted object's own top edge was.

**The selection feathers outside the painted footprint.** The model's output was clean along the line; a selection with a correction's brush profile, whose 15% feather lies *inside* the painted radius, was not: the footprint's outer 27 px (of a 179 px radius) held partial coverage, and the kit's stroke, 12 px wider than the paste's region, put the object's top edge 18 px inside the brush's edge, where the coverage `c` was 0.74 to 0.79. The repair stage's composite `in + c · (patch − in)` keeps `1 − c` of the input there: the measured trace, +16 codes of red on rows the model left clean, matches that blend in linear light, and it fades to nothing within about 6 px as the feather's easing reaches 1, so it reads as a line. The pyramid blend's `(1 − r)` term takes detail from the same band, but the composite alone accounts for the trace. The narrow reproduction, a flat answer over a synthetic red block 12 px inside a brush's edge, composited 25 codes of red with the inside feather (`an_object_the_brush_paints_over_never_shows_through_the_candidate` in `luxforge-core`, with the blend and without it). The prototype does not show it, because its footprint region keeps full coverage over the footprint and feathers *outside* it (`refine::footprint_region`): the harness's composite of the same pub output with the case's own region is clean on the box's edge rows (`harness-pub/`), and the Fast fill's `fill --mask` fills a hard mask. The editor's selection does the same: full coverage over the footprint as painted, falling to zero over 15% of the radius past it, for a removal and a Replace alike, and every pixel outside the coverage is still the input's own. The regions are larger by that margin (Minnewanka 799 × 413 against 745 × 359).

Measured with the feather outside the footprint, release build, at 1024 px, seed 0, one candidate a case in a process of its own (`--once`), a load of 11 before and 3.4 after; evidence in `private/object-removal/evidence/task-013-fixes/line/` and the crops in `…/task-013-fixes/look/`:

| Case | Region | Generate s | Steps 1 to 4 reported at s | Peak footprint GB | Red excess on the box's edge rows, feather inside / outside |
| --- | --- | ---: | --- | ---: | --- |
| water-minnewanka | 799 × 413 | 132.4 | 39.6, 66.5, 93.4, 120.1 | 4.04 | +13.8 / +1.3 codes |
| people-utrecht-pub | 834 × 428 | 132.4 | 39.5, 66.4, 93.4, 120.0 | 4.05 | +24.3 / +0.3 codes |

The red excess is the mean of `R − (G + B) / 2` over the eight rows at the pasted box's top edge less that of the rows 12 to 30 px below it, over the box's width. At three times their size (`look/CASE-line-before-after.png`) the line is gone from both and neither shows a seam at the region's new edge; the moored boat is rebuilt on Minnewanka and the second walker in the pub window.

**Replace on the real models.** Measured in one `luxforge-json` process on the same build, with both resources installed from staged local files: a Replace with "a wooden rowing boat" on Minnewanka at 1024 px, seed 0, then a prompt-free removal from the same seed (`--prompt`; `…/task-013-fixes/replace/`). The Replace took 129.2 s (steps at 39.8, 65.3, 91.2 and 116.8 s) and the removal after it 132.4 s; the process ran 262.8 s with a peak footprint of 8.98 GB and a maximum resident set of 9.02 GB, at a load of 4.1 before and 2.9 after. After the Replace the runtime held `run-klein-replace` (9 GiB declared); after the removal it held `run-klein` (5 GiB) beside it, 14 GiB of the 16 GiB budget, and the removal's export is pixel-identical to the one made in a process of its own. The model put a varnished wooden rowing boat with oars, a reflection and a mooring line on the water where the box was, composited over the region's own feathered edge with no box or halo; its stern runs into the region's left end and fades out over the feather there (`look/water-minnewanka-replace-context.jpg`).

### Blinded comparison

The owner's preference test of the local generative fill against the Fast fill and Lightroom's Generative Remove, on the same photographs and the same painted area.

1. **Inputs.** Run `prepare`, `editor` and `lama` as above. Each case's `inputs/ID.jpg` is the photograph every method starts from, and `inputs/ID-mask.png` the area to fill.
2. **Lightroom.** Import each `inputs/ID.jpg` into Lightroom Classic, open Remove with **Use Generative AI** on, paint over the area `ID-mask.png` shows (overlaying it as a guide is fine), take the first variation Lightroom offers, and export it full size as `ID.jpg` into a directory of its own, with no other edit. Record the Lightroom version and the date.
3. **Sheets.** `sheets --out $E --lightroom DIR` writes `sheets/ID.png`: the input cropped around the area with its outline in red, then each result's same crop under a letter, in a random order drawn for each case from `--seed` (default 2026), and `sheets/ballot.csv`. The key, which letter is which method, is `key.json` beside `sheets/`; do not open it until the ballot is filled.
4. **Judging.** For each sheet, at 100% on the same display, record in `ballot.csv` the best and the worst letter and a short note on what decided it (a seam, a smear, invented structure, a texture or colour mismatch). Judge whether the area looks like the rest of the photograph, not whether it matches what was there.
5. **Unblinding.** Read `key.json` and tally, per method, the count of bests and worsts.

Limits: the editor's results are its JPEG exports (quality 90) and the Fast fill's PNG, so compression could tell them apart at very high zoom; three cases decide nothing on their own, and more are added by listing them in the cases file. Each method fills its own way around the area: the editor fills the whole stroke and feathers over 15% of its radius past the edge, so its fill reaches a little beyond `ID-mask.png`; LaMa fills the mask exactly; and Lightroom chooses its own region from the brush.

## Backend parity

One run on the owner's Apple M4 Pro (`Mac16,8`), macOS 26.5.2, `rustc` 1.94.0, `ort` 2.0.0-rc.13, `tract-onnx`/`tract-metal`/`tract-core` 0.23.8. This is a correctness and load/run-shape table from `removal-probe parity`, not a timing claim: other sessions can share this host, and each cell is a single measurement (load once; run once as "first"; run three more times and report the median as "warm"). The M4 timings the design asks for are in [Measurements](#measurements).

Every pinned model ran a fixed, seeded, deterministic input per model: encoders got pseudo-random values in the ImageNet-normalized range `[-2.5, 2.5]`; decoders got the matching encoder's own **ort-cpu** output plus a single positive centre point (a second point was never needed: every pinned decoder accepts `num_points = 1`); LaMa got a smooth gradient plus seeded noise in `[0, 1]` and a centred square hole. Full inputs and the exact numbers below are in `src/commands/parity.rs` and `target/evidence/task-004/parity.json`; that JSON and the run's console transcript (`parity.log`) are the evidence for this section.

| Model | Backend | Status | Load ms | First-run ms | Warm ms (median×3) | vs ort-cpu |
| --- | --- | --- | ---: | ---: | ---: | --- |
| evit-l0-encoder | ort-cpu | ok | 57.7 | 167.2 | 172.5 | reference |
| evit-l0-encoder | ort-coreml-all | ok | 170.4 | 171.6 | 173.8 | mean 0, max 0 |
| evit-l0-encoder | ort-coreml-gpu | ok | 78.4 | 173.0 | 174.0 | mean 0, max 0 |
| evit-l0-encoder | ort-coreml-ane | ok | 129.4 | 171.8 | 172.3 | mean 0, max 0 |
| evit-l0-encoder | tract-cpu | **error** | - | - | - | see below |
| evit-l0-encoder | tract-metal | **error** | - | - | - | see below |
| evit-l0-decoder | ort-cpu | ok | 37.2 | 10.3 | 10.7 | reference |
| evit-l0-decoder | ort-coreml-all | ok | 285.2 | 14.2 | 11.2 | masks mean 0, max 0, IoU 1.0000 |
| evit-l0-decoder | ort-coreml-gpu | ok | 118.2 | 13.5 | 12.8 | masks mean 0, max 0, IoU 1.0000 |
| evit-l0-decoder | ort-coreml-ane | ok | 267.0 | 14.4 | 12.9 | masks mean 0, max 0, IoU 1.0000 |
| evit-l0-decoder | tract-cpu | **error** | - | - | - | see below |
| evit-l0-decoder | tract-metal | **error** | - | - | - | see below |
| sam21-small-encoder | ort-cpu | ok | 67.0 | 569.6 | 536.7 | reference |
| sam21-small-encoder | ort-coreml-all | ok | 43535.0 | 380.0 | 173.1 | max abs diff ≤ 1.2e-5 |
| sam21-small-encoder | ort-coreml-gpu | ok | 9526.2 | 352.2 | 151.9 | max abs diff ≤ 1.2e-5 |
| sam21-small-encoder | ort-coreml-ane | ok | 29227.2 | 596.8 | 305.2 | max abs diff ≤ 1.1e-5 |
| sam21-small-encoder | tract-cpu | ok | 197.6 | 1434.3 | 1175.9 | max abs diff ≤ 1.3e-5 |
| sam21-small-encoder | tract-metal | ok | 23.2 | 634.9 | 555.4 | max abs diff ≤ 1.1e-5 |
| sam21-small-decoder | ort-cpu | ok | 22.7 | 10.5 | 10.4 | reference |
| sam21-small-decoder | ort-coreml-all | ok | 560.9 | 34.4 | 15.5 | masks max 4e-6, IoU 1.0000 |
| sam21-small-decoder | ort-coreml-gpu | ok | 282.0 | 26.6 | 13.2 | masks max 4e-6, IoU 1.0000 |
| sam21-small-decoder | ort-coreml-ane | ok | 324.8 | 19.4 | 15.4 | masks max 5e-6, IoU 1.0000 |
| sam21-small-decoder | tract-cpu | **error** | - | - | - | see below |
| sam21-small-decoder | tract-metal | **error** | - | - | - | see below |
| lama | ort-cpu | ok | 1790.6 | 1117.4 | 1133.7 | reference |
| lama | ort-coreml-all | ok | 21756.6 | 1907.4 | 1225.6 | mean 0, max 0 |
| lama | ort-coreml-gpu | ok | 12272.6 | 1262.4 | 1346.5 | mean 0, max 0 |
| lama | ort-coreml-ane | ok | 11062.9 | 1170.1 | 1143.5 | mean 0, max 0 |
| lama | tract-cpu | **error** | - | - | - | see below |
| lama | tract-metal | **error** | - | - | - | see below |

Where a run succeeded, every backend agreed with ort-cpu: every decoder's thresholded-mask IoU against ort-cpu was 1.0000, and tract's one success (`sam21-small-encoder`) differed by at most 1.3×10⁻⁵. A difference of **exactly 0** on a Core ML row means Core ML computed none of that graph's values. That is the case for both EfficientViT models and for LaMa, whose free dimensions (`batch_size`, `num_points`, `batch`) keep the provider, configured for static input shapes, off every node that carries a value. The SAM 2.1 encoder is fully static, which is where its difference of about 10⁻⁵ and its long first compile come from. The decoder declares `num_points`, so Core ML took only 138 of its 462 nodes. The fixed-shape backends put all five models on Core ML. In one run with the same inputs (`target/evidence/task-006/parity-fixed.json`):

| Model | Backend | Load ms | First-run ms | Warm ms (median×3) | Nodes on Core ML | vs ort-cpu |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| evit-l0-encoder | ort-coreml-gpu-fixed | 229.1 | 122.5 | 90.4 | 437 of 440 | max abs diff 2.3e-5 |
| evit-l0-decoder | ort-coreml-gpu-fixed | 2728.5 | 28.3 | 4.2 | 270 of 281 | masks max 1.2e-5, IoU 1.0000 |
| sam21-small-encoder | ort-coreml-gpu-fixed | 36376.7 | 323.7 | 145.9 | 525 of 603 | max abs diff 1.2e-5 |
| sam21-small-decoder | ort-coreml-gpu-fixed | 2823.7 | 34.3 | 6.9 | 303 of 312 | masks max 1.4e-5, IoU 1.0000 |
| lama | ort-coreml-gpu-fixed | 4213.1 | 1983.3 | 1099.5 | 2,518 of 3,365 | max abs diff 4.0e-4 |

The `-all-fixed` and `-ane-fixed` rows agree with ort-cpu within the same bounds (LaMa within 5.5 × 10⁻⁴). The ort-cpu reference ran the encoders in 152.8 and 482.9 ms, the decoders in 9.8 and 11.6 ms, and LaMa in 1,142.9 ms.

The default LaMa is the OpenCV export, whose weights are quantized in blocks (`DequantizeLinear` with block size). Carve's full-precision Big-LaMa export (`--fill-model lama-fp32`) takes the same inputs, scaling and outputs, and fills no better. Over eight synthetic objects pasted onto `sapa-road.jpg` (regions 297 × 211 to 1381 × 1003 px, enlarged 1.7× to 8.1×; `fill --paste --paste-at X,Y --paste-size W,H`, default parameters, ort-cpu, one run each), it scored:
- inside the region, 19.13 dB mean PSNR against the covered background, against 19.20 dB for the quantized export, better in 3 of 8 cases;
- in the edge band, 22.79 dB against 22.75 dB.

Its model time was the same (about 1.15 s) and its load was slower (2.0 s against 1.7 s). Removing the boy from `z6-bridge.jpg` with each gave results that differ by at most 35 codes and on average 3. At full size they are the same blocky, smeared deck with the boy's outline visible. The softness on large objects comes from enlarging a 512² fill, not from the quantized weights.

### Failures

Both tract backends fail identically for three of the five models, always at `into_typed()` — before either backend-specific optimization runs, which is why `tract-cpu` and `tract-metal` always fail or succeed together here:

- **`evit-l0-encoder`, `evit-l0-decoder`, `sam21-small-decoder`** (tract-cpu, tract-metal): tract's ONNX frontend refuses to unify a genuinely dynamic dimension (`batch_size` or `num_points`, both concretized in `runtime::Session::run`'s own input tensors) against a literal `1` it derives for the same axis elsewhere in the graph — for example `evit-l0-decoder`: `Failed analyse for node #131 "/Add" Add: ... Unifying shapes batch_size,num_points,2 and 1,1,2: Impossible to unify Sym(batch_size) with Val(1)`. This is a broadcast a numpy/ONNX runtime executes without issue (an axis of size 1 always broadcasts), but tract's inference-time unifier for this HIR-level `Add` treats it as a hard equality constraint instead. Two things were tried and did not change the result: setting `tract_onnx::Onnx { ignore_value_info: true, .. }` (in case the `1` came from a stale embedded shape hint rather than a graph constant), and running `InferenceModel::analyse(true)` (obstinate, multi-pass) before `into_typed()` in case the symbol substitution simply hadn't reached that node yet. Neither changed the error, so this looks like a real limitation of tract 0.23.8's ONNX-import shape inference for dynamic dimensions that meet a broadcast, not a quirk of these two exports or of this probe's shape handling — `sam21-small-encoder`, the one pinned model with no dynamic input dimension at all, is the only one of the four segmenter models that loads on tract.
- **`lama`** (tract-cpu, tract-metal): fails earlier, while building the graph rather than during shape inference — `Building node model.generator.model.34.weight_quantized_node (DequantizeLinear): DequantizeLinear: blocked quantization is not supported`. The pinned LaMa export quantizes its weights with a blocked/grouped scheme that tract 0.23.8's `DequantizeLinear` does not implement; this is unrelated to the shape issue above and would presumably reproduce on any dynamic or fixed-shape model using the same quantization.

Console noise, not a row failure: after the run above printed its table and wrote the JSON, ONNX Runtime's Core ML provider logged a few `E5RT Tensor descriptor rank doesn't match MPSGraphTensor` / `Espresso exception: "Invalid blob shape"` lines to stderr. Every Core ML row already completed and reported `ok`; these appear to be provider-internal validation notices at process teardown, not a run failure, but are recorded here since they were observed.
