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

The probe is its own Cargo workspace with its own lockfile, outside the editor's: the editor's build and quick tier do not build it. `cargo xtask check-repository` reads its sources and manifest with the repository's other rules. The tests that need the pinned models and the private corpus are `#[ignore]`d; the ignored suite last passed in full on the M4 (release profile, ONNX Runtime CPU and Core ML) on 2026-10-11, including the two scoring tests below.

Backends: `ort-cpu`, `ort-coreml-all`, `ort-coreml-gpu`, `ort-coreml-ane`, `ort-coreml-cpu`, `tract-cpu`, `tract-metal`, and the fixed-shape Core ML backends `ort-coreml-all-fixed`, `ort-coreml-gpu-fixed`, `ort-coreml-ane-fixed` (and `ort-coreml-cpu-fixed`). Core ML compiles each model on first load and caches it under the system temporary directory (`luxforge-removal-probe-coreml`); macOS keeps its own device-specialized copy in `~/Library/Caches/<process name>/com.apple.e5rt.e5bundlecache`, and the measured cold loads clear both. A cache compiled before a macOS update fails at load with `Unable to load model … Compile the model with Xcode`; delete the model's directory under `luxforge-removal-probe-coreml` and it compiles again. tract has no such cache: four of the five pinned models have a dynamic input dimension (`batch_size`, `num_points` or `batch`), so `runtime::Session` concretizes shapes lazily — the first `run` for a given combination of input shapes builds and optimizes a `TypedModel` plan and caches it; later runs with the same shapes reuse it. That first build is folded into "first run", not "load", in every table below.

The Core ML provider is configured to take only nodes whose input shapes are static, so on a model that declares a free dimension (`batch_size` and `num_points` in both EfficientViT-SAM models, `num_points` in the SAM 2.1 decoder, `batch` in LaMa) it runs little or nothing. A `-fixed` backend loads a copy of the model with every free input dimension fixed at 1 instead. ONNX Runtime writes that copy itself, once per model file, into the same cache directory: a CPU session with free-dimension overrides at the basic optimization level saves the graph it optimized, whose shape computations are then constants. A run with any other input shape (EfficientViT's batched stroke prompts, a box prompt's two points) goes to a CPU session of the original model, opened the first time one arrives. Setting the overrides on the Core ML session directly lets the provider take the graph, but the compiled partition then asks at run time for a `Shape` node's output that nothing feeds (`Feature …_Shape_output_0 is required but not specified`) on both EfficientViT models; `LUXFORGE_REMOVAL_ORT_LOG=warning` prints how many nodes the provider takes, and `verbose` why it refuses each other one.

```sh
probes/object-removal/target/release/removal-probe parity --out target/evidence/task-004/parity.json
```

### Removing from the command line

`removal-probe remove` runs the whole chain on one point or one stroke and writes into `--out` (default: a new directory under the system temporary directory, which it names):

```sh
C=/Users/will/projects/lightwell/private/object-removal/corpus
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

With `--case`, `score` regenerates the pasted image and region from the manifest and scores against the clean original. With `--input` and `--region` it scores any removal, such as `remove`'s `region.png` and `result.png`; without `--truth` only the outside check runs and the quality fields are `skipped`. Measured on the M4 on 2026-10-11 with the pinned LaMa on ONNX Runtime's CPU (these are scores of the existing fill, not a claim about it):

| Removal | Region PSNR / SSIM | Edge ring PSNR / SSIM | Changed outside |
| --- | --- | --- | --- |
| Pasted object, `sky-clouds-blue` (4032 × 2670, 140,087 px region, 12,224 px ring) | 24.29 dB / 0.944 | 36.46 dB / 0.956 | 0 |
| Pasted object, `bridge-deck` (4024 × 6048, 310,666 px region, 18,336 px ring) | 21.87 dB / 0.802 | 30.08 dB / 0.893 | 0 |
| The boy on the bridge, `remove --point 1690,3550` (685,151 px region, 41,999 px ring) | skipped, no truth | skipped, no truth | 0 |

## Contract

Status: a standalone prototype, delivered and measured on the M4 on 2026-09-28 and 29 at the owner's request, before semantic masks. The product integration is the [AI editing design](../../docs/design/ai-editing.md)'s Remove, whose repair stage, frozen patches and consent rules are unchanged here. The evidence behind every choice is the [object removal research](../../docs/research/object-removal/README.md).

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

**Proposal for the owner:** EfficientViT-SAM L0 as the segmenter, on ONNX Runtime with Core ML's GPU setting over fixed-shape copies (`ort-coreml-gpu-fixed`), and LaMa on ONNX Runtime's CPU provider.

- **Runtime.** ONNX Runtime, because tract loads only one of the five pinned models. The Core ML provider takes only nodes with static input shapes, so it runs EfficientViT-SAM only over a copy with every free dimension fixed at 1. ONNX Runtime writes that copy itself, with no Python and no re-export. LaMa stays on the CPU: on Core ML it runs in 188 partitions, is no faster (2.2 s against 1.2 s in paired runs) and holds a 9 GB footprint against 1.5 GB.
- **Segmenter.** EfficientViT-SAM encodes in about 110 ms (130–160 ms at 60 MP) and decodes a hover point in about 5 ms. It loads in about 1.2 s once compiled, compiles in about 7 s the first time, and stays under a 1 GB footprint. SAM 2.1 Small on Core ML decodes as fast and encodes in 150–180 ms. Its Hiera encoder, though, splits into 27 Core ML partitions at rank-6 reshapes. As a result it takes about 11 s to load even when cached and 30–90 s to compile, and holds a 3.5 GB footprint; on the CPU it encodes in over 500 ms.
- **Accuracy.** SAM 2.1 is the more accurate of the two on small objects. At its 0.6 floor it found the kayaks and the two walkers that EfficientViT-SAM missed, because EfficientViT-SAM answers a point on a kayak in dark or uniform water with the water itself. The owner weighs that recall against SAM 2.1's load time and memory.

Budgets, from the pointer to the drawn overlay in a hidden lab window, and from `bench` and `remove`:

- **Hover, 16–33 ms: the median is met and the tail sits at the edge.** On Core ML both families draw the overlay at a p50 of 20 ms and a p95 of 33–35 ms. On the CPU the p95 is 45–46 ms, a miss.
- **Analysis, under about 300 ms after the image opens: met on Core ML once the models are loaded.** EfficientViT-SAM encodes in 110–160 ms and SAM 2.1 in 150–180 ms, while SAM 2.1 on the CPU misses (520–950 ms). The first image of a session also waits for the load, about 1.2 s for EfficientViT-SAM and 10–20 s for SAM 2.1, unless the models load before the tool is picked.
- **Fill, a few seconds: met.** LaMa on the CPU fills in 1.7–3.0 s at 20–60 MP. The region adds up to 0.5 s, and LaMa loads once in 2–3 s. Every fill left the pixels outside its region byte-identical.

**The open problem is the fill's quality on large objects.** The pinned LaMa takes a fixed 512² input, and the crop is three times the region's longer side. The fill is therefore enlarged 9× for the boy on the bridge, 10–13× for a flower or the water, 20× for a tall pole and 30× for a 60 MP quadrant. Grain re-synthesis restores the noise but not the structure: the bridge deck under the boy is rebuilt visibly softer than the deck around it, and the road's fill is a blurry smear. The fill's time hardly depends on the object, because LaMa always runs at 512². The quantized weights are not the cause. Carve's full-precision Big-LaMa export scores within 0.1 dB of the OpenCV export on eight pasted-object ground truths, and gives the same smeared deck on the bridge. What remains open is quality at a larger effective input: a fill that is not enlarged as far, either a second LaMa pass over the region at a finer scale or an export at 1024 or 2048, as in the research's measurement 7.

**Stroke parameters (proposal).** SAM 2.1 starts from a predicted-IoU floor of 0.6. At that floor two of its nine corpus strokes went from nothing to the right several objects, and the other seven did not change. At 0.65 the kayaks were still missed. EfficientViT-SAM keeps every default: its misses are the decoder's own answers, and no floor changed a decision. One question is a matter of intent rather than a threshold: whether a stroke along one bend of a road, covering 15 % of it, removes the whole road, as both families decide now.

#### What it does not do

No generative model, no RAW input (the corpus is developed to JPEG first), no proposal detector, no editor integration, no download code. A generative tier waits for the owner's weights-licence decision; a detector-driven proposal pass follows if the hover measurements show that decoding on every pointer move is not enough.

### Decisions

| Question | Status |
| --- | --- |
| Removal before semantic masks | Owner, 2026-09-28: prototype object removal first |
| Prototype outside the editor | Owner, 2026-09-28: a standalone probe |
| Models in the prototype | Proposal: Apache-2.0 weights only, as above |
| Segmenter | Proposal from the [M4 measurements](#summary-of-the-m4-measurements): EfficientViT-SAM L0; SAM 2.1 Small is more accurate on small objects but loads about ten times slower and holds about four times the memory. The owner chooses |
| Runtime | Proposal from the [M4 measurements](#summary-of-the-m4-measurements): ONNX Runtime, the segmenter on Core ML's GPU setting over fixed-shape copies, LaMa on the CPU; tract loads one pinned model of five. The owner chooses |
| Shadow handling | Proposal: the stroke footprint and an optional contact band join the region; a learned effect region waits for a licence-clean model |
| AI Remove scope, repair stage, weights licences, training-data provenance | Open, as recorded in the [research](../../docs/research/object-removal/README.md#nine-owner-decisions-and-twelve-m4-measurements-gate-the-choice) and [Corrections](../../docs/design/corrections.md) |

## Measurements

Measured 2026-09-28 and 29 on the owner's Apple M4 Pro (`Mac16,8`, 14 cores, 48 GB), macOS 26.5.2 (25F84), `rustc` 1.94.0, release build of `1a36ba72` (the SAM 2.1 stroke default came later and changes no timing), `ort` 2.0.0-rc.13 linking ONNX Runtime 1.28.0, Core ML in MLProgram format.

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

A later paired set ran the five contenders back to back on the bridge, five rounds with the order reversed each round, at load 13 to 30. EfficientViT-SAM encoded at a p50 of 326 ms on ort-cpu and 120 ms on `ort-coreml-gpu-fixed`, and decoded at 21.9 and 8.2 ms. SAM 2.1 encoded at 955 ms on ort-cpu, 240 ms on `ort-coreml-gpu-fixed` and 155 ms on `ort-coreml-all-fixed`, and decoded at 23.8, 7.4 and 5.7 ms. Core ML's times move much less with the host's load than the CPU provider's.

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

`remove` on ort-cpu with both families. Candidates are listed with their predicted IoU. EfficientViT-SAM is shown at its default floor of 0.7, and SAM 2.1 at 0.7 and at 0.6, its default since these runs. At 0.6 EfficientViT-SAM's decisions did not change.

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

## Backend parity

One run, 2026-09-28, on the owner's Apple M4 Pro (`Mac16,8`), macOS 26.5.2, `rustc` 1.94.0, `ort` 2.0.0-rc.13, `tract-onnx`/`tract-metal`/`tract-core` 0.23.8. This is a correctness and load/run-shape table from `removal-probe parity`, not a timing claim: other sessions can share this host, and each cell is a single measurement (load once; run once as "first"; run three more times and report the median as "warm"). The M4 timings the design asks for are in [Measurements](#measurements).

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
