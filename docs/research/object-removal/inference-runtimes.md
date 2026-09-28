# Local neural-network inference runtimes for a Rust desktop editor (state as of 2026-09-27)

Scope: runtimes that Luxforge (GPL-3.0-or-later, Rust, iced 0.14 + wgpu 27, Apple M4 first, Windows/Linux portable) could use for (1) a SAM-style encoder once per image plus a prompt decoder on every hover, (2) feed-forward inpainting (LaMa, which needs FFT ops) and possibly diffusion, and (3) semantic segmentation (sky, person, salient subject, depth). Versions and dates below come from GitHub release/tag APIs and crates.io unless a source says otherwise. Where I estimate, I say so.

Project context checked in this worktree:
- `docs/design/masking-workspace.md` line 76 frames the choice: tract keeps "the toolchain rule and the licence rule" with a smaller operator set, while ONNX Runtime is broader and native. The capability contract rejects `local-runtime` today.
- `docs/decisions.md` line 16 sets the toolchain rule: all development tooling is Rust (`cargo xtask`), with no second toolchain.
- Luxforge already statically builds a native C++ dependency, LibRaw, from pinned source (`docs/research/raw-backend-selection.md` line 3), so a pinned native library is precedented. A downloaded prebuilt binary is not.
- `Cargo.lock` pins `wgpu 27.0.1`, `iced 0.14.0`, `metal 0.32.0` and `objc2 0.6.4`.

## ort (pykeio/ort, Rust bindings to ONNX Runtime): version, execution providers, linking, size, licence and gaps

### Takeaway
ort is the broadest option and the most proven in shipping apps. The current release is `2.0.0-rc.13` (2026-07-28), which wraps ONNX Runtime 1.28; `main` moved to ORT 1.30.0 on 2026-09-16. Prebuilt static binaries include CoreML on every macOS arm64 build and DirectML on every Windows build, with optional WebGPU and CUDA/TensorRT builds. XNNPACK, OpenVINO, QNN, MIGraphX and oneDNN need a source build of ONNX Runtime. There are three main risks for this project:
- The CoreML EP has no DFT/STFT kernels, so LaMa's FFTs fall back to the CPU.
- There is a recent CoreML FP16 correctness bug.
- The ort maintainer has said he can no longer test on macOS.

### Cited Findings
**Versions and maintenance**
- ort `2.0.0-rc.13` was published 2026-07-28, `rc.12` on 2026-03-05, `rc.11` on 2026-01-07 and `rc.10` on 2025-06-05. There is still no stable 2.0.0. crates.io lists `ort` 2.0.0-rc.13 as MIT OR Apache-2.0, with about 19.8 M total and 7.5 M recent downloads — [GitHub releases API: pykeio/ort](https://github.com/pykeio/ort/releases); [crates.io ort](https://crates.io/crates/ort)
- The rc.11 notes said the next big release should finally be 2.0.0. rc.12 and rc.13 followed instead — [ort v2.0.0-rc.11](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.11)
- rc.13 skips ahead four ONNX Runtime versions to 1.28 — [ort v2.0.0-rc.13](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.13)
- On `main`, `ort-sys`'s `dist.tsv` points at ONNX Runtime 1.30.0 (commit "update ONNX Runtime to v1.30.0", 2026-09-16). This is not yet released on crates.io — [pykeio/ort PR #620](https://github.com/pykeio/ort/pull/620); [dist.tsv](https://github.com/pykeio/ort/blob/main/ort-sys/build/download/dist.tsv)
- ONNX Runtime itself released 1.30.0 on 2026-09-10, 1.29.0 on 2026-08-12 and 1.28.0 on 2026-07-25. It keeps roughly a monthly cadence — [microsoft/onnxruntime releases](https://github.com/microsoft/onnxruntime/releases)

**Execution providers and prebuilt binaries**
- ort's EP table lists CUDA, TensorRT and TensorRT-RTX (Windows x64 and Linux x64 binaries), DirectML (Windows binaries), CoreML (macOS arm64 and iOS arm64 binaries), and WebGPU (binaries for Windows x64, Linux x64 and macOS arm64). These EPs are "supported but no binaries", meaning they need a source build: MIGraphX, OpenVINO, oneDNN, XNNPACK, QNN (Windows/Linux/Android arm64), CANN, TVM, ACL, Vitis AI, RKNPU and Azure — [ort docs/core/ep.tsx](https://github.com/pykeio/ort/blob/main/docs/core/ep.tsx); [ort execution providers docs](https://ort.pyke.io/perf/execution-providers)
- All Windows builds ship with the DirectML EP and all macOS builds ship with the CoreML EP. CUDA and TensorRT always ship together. Enabling two EPs that never share a build (for example `cuda` + `webgpu`) is a compile error — [ort prebuilt binaries docs](https://github.com/pykeio/ort/blob/main/docs/content/misc/prebuilt-binaries.mdx)
- rc.13 moved EP structs behind Cargo features. It also throws a **link-time error** when `download-binaries` cannot satisfy the requested EP combination. Before this, ort silently fell back to CPU; a new `lax-feature-matching` feature restores the fallback — [ort v2.0.0-rc.13](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.13)
- rc.13 ships CUDA 13 binaries only, because ONNX Runtime deprecated CUDA 12. ort targets CUDA ≥ 13.2 and cuDNN ≥ 9.23, which must be on `PATH` — [ort v2.0.0-rc.13](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.13); [ort EP docs](https://ort.pyke.io/perf/execution-providers)
- The ort docs call the WebGPU EP "experimental" and warn it may give incorrect results or crash. Its binaries cover Windows (DX12/11), macOS and Linux (Vulkan/OpenGL) — [ort EP docs](https://ort.pyke.io/perf/execution-providers)
- The ORT WebGPU EP gained DFT support in ORT 1.29 (2026-08-12) and GridSample in 1.26 — [ORT v1.29.0](https://github.com/microsoft/onnxruntime/releases/tag/v1.29.0); [ORT v1.26.0](https://github.com/microsoft/onnxruntime/releases/tag/v1.26.0)
- rc.12 added "multiversioning" (ORT API 1.17–1.24 behind `api-*` features). It also added automatic device selection (`SessionBuilder::with_auto_device`), which prefers an NPU on ORT ≥ 1.22 — [ort v2.0.0-rc.12](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.12)
- rc.10 added:
  - a Compiler API, which persists EP-compiled graphs such as CoreML networks and TensorRT engines ahead of time, so later loads skip compilation;
  - a new CoreML registration API with compute units (`CPUOnly`, `CPUAndNeuralEngine`, …);
  - statically linked binaries by default;
  - KleidiAI-accelerated ARM64 CPU kernels.

  — [ort v2.0.0-rc.10](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.10)

**Linking and prebuilt binaries**
- By default ort downloads statically linked ONNX Runtime builds from pyke's CDN (`cdn.pyke.io`). ONNX Runtime "takes over an hour to compile" from source — [ort prebuilt binaries docs](https://github.com/pykeio/ort/blob/main/docs/content/misc/prebuilt-binaries.mdx)
- The docs recommend static linking where the EPs support it. Among dynamic options they recommend the `load-dynamic` feature, which loads the library at runtime through `ort::init_from(path)` or `ORT_DYLIB_PATH` and handles missing binaries gracefully — [ort linking docs](https://ort.pyke.io/setup/linking)
- x86-64 prebuilt binaries require x86-64-v3 (Haswell-era AVX2 or newer). Linux binaries are built with Clang and depend on `libc++` — [ort prebuilt binaries docs](https://github.com/pykeio/ort/blob/main/docs/content/misc/prebuilt-binaries.mdx)
- From rc.12 the binaries carry GitHub build attestations. `dist.tsv` records each archive's SHA-256, which `ort-sys` checks. The maintainer is the only person with backend access to the CDN — [ort prebuilt binaries docs](https://github.com/pykeio/ort/blob/main/docs/content/misc/prebuilt-binaries.mdx)
- rc.11 dropped Intel macOS and raised the macOS target to 13.4. The maintainer wrote that he can no longer debug macOS and users should expect "little to no macOS support in general from now on" — [ort v2.0.0-rc.11](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.11)

**Binary size**
- Compressed (LZMA2) sizes of pyke's ORT 1.30 static archives, measured by HTTP HEAD on 2026-09-27:

  | Archive | Size |
  | --- | --- |
  | `aarch64-apple-darwin+coreml` | 8.9 MB |
  | `aarch64-apple-darwin+coreml,webgpu` | 11.7 MB |
  | `x86_64-unknown-linux-gnu` (CPU) | 9.9 MB |
  | `x86_64-unknown-linux-gnu+webgpu` | 14.1 MB |
  | `x86_64-pc-windows-msvc+directml` | 30.3 MB |
  | `x86_64-pc-windows-msvc+webgpu` | 55.7 MB |
  | `x86_64-unknown-linux-gnu+cuda13,tensorrt,nvrtx` | 48.1 MB |
  | `x86_64-pc-windows-msvc+cuda13,tensorrt,nvrtx,directml` | 66.8 MB |

  — [dist.tsv URLs](https://github.com/pykeio/ort/blob/main/ort-sys/build/download/dist.tsv)
- Microsoft's official ORT 1.30.0 release archives (compressed) are:
  - `onnxruntime-osx-arm64` 40.4 MB, `linux-x64` 10.7 MB, `win-x64` 78.8 MB;
  - `linux-x64-gpu_cuda13` 224.7 MB and `win-x64-gpu_cuda13` 280.7 MB.

  — [ORT v1.30.0 assets](https://github.com/microsoft/onnxruntime/releases/tag/v1.30.0)
- ORT 1.28 made cuDNN and cuFFT optional at runtime for the CUDA EP and stopped linking `nvrtc`, "significantly" reducing the CUDA redistributable footprint — [ORT v1.28.0](https://github.com/microsoft/onnxruntime/releases/tag/v1.28.0)
- Building ORT from source requires C++20 from 1.25 onward (MSVC 19.29+, GCC 10+, Clang 10+) — [ORT v1.25.0](https://github.com/microsoft/onnxruntime/releases/tag/v1.25.0)

**CoreML EP**
- The MLProgram format needs macOS 12+. The default is NeuralNetwork (macOS 10.15+). The options are:
  - `MLComputeUnits` (`CPUOnly`, `CPUAndNeuralEngine`, `CPUAndGPU`, `ALL`);
  - `RequireStaticInputShapes`, `EnableOnSubgraphs`;
  - `ModelCacheDirectory` (a compiled-model disk cache);
  - `AllowLowPrecisionAccumulationOnGPU`, `SpecializationStrategy`;
  - `ProfileComputePlan` (logs which hardware each op is dispatched to).

  — [ORT CoreML EP docs](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html)
- The CoreML EP's MLProgram operator list has about 45 ops: Conv, ConvTranspose (constant weights; no output_shape/padding variants), GridSample (4D, partial), Group/Instance/LayerNormalization, Gelu, Erf, MatMul, Resize (limited permutations), Softmax, Slice (constant starts/ends), Transpose and so on. It has **no DFT, STFT or other FFT op**, so those nodes fall back to the CPU EP and split the graph — [ORT CoreML EP docs, supported operators (gh-pages source)](https://github.com/microsoft/onnxruntime/blob/gh-pages/docs/execution-providers/CoreML-ExecutionProvider.md)
- Open ORT issue #32569 (2026-09-12): with ORT 1.30.0 on macOS 15 ARM64, `ModelFormat=MLProgram` plus `RequireStaticInputShapes=1` returns stable but **wrong** FP16 outputs (e.g. 6.33 expected vs 5.52). The same model passes with the CPU EP, with legacy NeuralNetwork, and with the same settings on macOS 26. An ORT maintainer replied that numerical tolerance under partitioning is "subjective" — [onnxruntime #32569](https://github.com/microsoft/onnxruntime/issues/32569)
- Open ORT issue #31975 (2026-08-11) proposes exposing the CoreML EP through the new plugin-EP factory (`OrtEpFactory`). This is part of ORT's ongoing move to plugin EPs (a CUDA plugin EP arrived in 1.25; WebGPU plugin EP 0.4.0 on 2026-09-22) — [onnxruntime #31975](https://github.com/microsoft/onnxruntime/issues/31975); [ORT v1.25.0](https://github.com/microsoft/onnxruntime/releases/tag/v1.25.0); [ORT releases](https://github.com/microsoft/onnxruntime/releases)
- ORT 1.28 fixed "CoreML-enabled static builds on macOS" — [ORT v1.28.0](https://github.com/microsoft/onnxruntime/releases/tag/v1.28.0)
- ort issue #559 (2026-03): `SessionBuilder` hung on an M4 Pro with `load-dynamic` + `coreml`. The maintainer attributed it to a `load-dynamic` init bug fixed in a later commit — [pykeio/ort #559](https://github.com/pykeio/ort/issues/559)
- A 2023 ORT issue asked why dynamic shapes were not supported with the CoreML EP. Today dynamic inputs are allowed unless `RequireStaticInputShapes=1` — [onnxruntime #14212](https://github.com/microsoft/onnxruntime/issues/14212); [ORT CoreML EP docs](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html)

**Alternative backends inside ort**
- From rc.10, ort has an "alternative backend" API: `ort::set_api(ort_tract::api())` swaps ONNX Runtime for tract (or candle) behind the same ort API. The crates are `ort-tract` 0.4.1+0.23 and `ort-candle` 0.4.1+0.11.0 (2026-08-18) — [ort v2.0.0-rc.10](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.10); [crates.io ort-tract](https://crates.io/crates/ort-tract)

### Inferences
- LaMa exported with ONNX `DFT` ops would run on ORT CoreML as many CoreML partitions separated by CPU FFT nodes. The mlx-lama-swift port counts 108 FFC convolutions in Big-LaMa. Each boundary costs a CPU↔CoreML transfer and a precision cast, so ORT+CoreML may not beat ORT CPU for LaMa. This must be measured, not assumed. A LaMa export that avoids `DFT` (Carve's "FourierUnitJIT" variant, see below) may partition differently; its op histogram must be inspected.
- ort's `download-binaries` default fetches an attested binary built by one maintainer from a single CDN. That conflicts with Luxforge's practice of building native code from pinned source. Pinning the `dist.tsv` hash and verifying the attestation in `xtask` is the lighter alternative; building ORT from source (CMake, Python, C++20, over an hour) is the heavier one and would break the "no second toolchain" rule.
- For an M4-first project, the maintainer's statement about macOS support and the macOS-15-only CoreML FP16 bug mean the Mac path cannot rely on upstream testing. Luxforge's own exact-output tests per model and EP setting would be the safety net.
- The ort API with `ort-tract` behind it lets one call site try both runtimes. It is a cheap way to A/B the two in the measurement harness. It does not give tract's Metal runtime, because ort-tract uses tract's default runtime; that is unverified.

### Gaps
- I found no published measurement of the final linked size ort adds to a Rust binary; the archive sizes above are compressed static libraries. It must be measured (`cargo bloat` or binary diff) with the CoreML build.
- I could not confirm whether the CoreML EP keeps outputs in any device memory, or whether ort's `IoBinding` supports CoreML device tensors. The docs only show CUDA/DirectML-style device allocators.
- I did not find how ONNX Runtime's QNN or OpenVINO EPs behave via ort on Windows-on-Arm or Intel NPUs; neither has prebuilt ort binaries.

## tract (sonos): ONNX/NNEF coverage, Apple Silicon performance, Metal, and whether SAM or LaMa run

### Takeaway
tract is far more capable than its "CPU-only, small op set" reputation suggests. As of 0.23.8 (2026-09-21) it has:
- first-class Metal and CUDA runtimes with per-node CPU fallback;
- an alpha wgpu backend;
- ONNX `DFT` and `STFT` import with GPU FFT kernels;
- Stable Diffusion 1.5/XL/3 examples.

Its FFT and Metal-convolution paths are, however, only weeks old: an irfft correctness fix landed on 2026-09-21 and Metal implicit-GEMM convolution on 2026-09-08. I found no published SAM or LaMa result on tract.

### Cited Findings
**Releases and API**
- Latest releases: v0.23.8 (2026-09-21), v0.23.7 (2026-09-08), v0.23.6 (2026-09-02) and v0.23.5 (2026-08-19), with 0.22.4 and 0.21.18 maintenance releases on 2026-09-07. Crates are MIT OR Apache-2.0; `tract-onnx` has about 3.1 M downloads — [GitHub releases API: sonos/tract](https://github.com/sonos/tract/releases); [crates.io tract-onnx](https://crates.io/crates/tract-onnx)
- The README describes tract as loading ONNX and NNEF and running on embedded ARM CPUs, NVIDIA and Apple GPUs, and WebAssembly. Sonos uses it in production for wake-word and streaming ASR, and it "also runs LLM, text-to-image, and classical CV models". The MSRV badge is rustc ≥ 1.91 — [sonos/tract README](https://github.com/sonos/tract)
- The recommended deployment path converts ONNX to NNEF/tract-OPL once, then ships only `tract-core` + `tract-nnef` (no protobuf) to keep the runtime small — [sonos/tract README](https://github.com/sonos/tract)
- Runtimes: CPU (`tract-linalg`, hand-rolled SIMD for x86, ARMv6/7/8 and SVE), Apple Metal (`tract-metal`), NVIDIA CUDA (`tract-cuda`) and WASM — [sonos/tract README](https://github.com/sonos/tract)
- The 0.23 migration notes say "GPU is first-class": CUDA and Metal runtimes with f16 conv and cuDNN, automatic per-node CPU fallback when the GPU rejects a shape, and virtual `gpu` / `gpu-or-cpu` runtime names — [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md)

**Recent GPU changes**
- 0.23.8 added [ALPHA] **`tract-wgpu`**, a WebGPU backend with kernels for:
  - element-wise and binary ops;
  - matmul;
  - convolution, pooling and transposed convolution;
  - reduction, softmax and resize.

  The workspace pins `wgpu = 30.0.1`. The crate is `0.23.8-pre` and not on crates.io — [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md); [tract Cargo.toml](https://github.com/sonos/tract/blob/main/Cargo.toml); [tract wgpu/Cargo.toml](https://github.com/sonos/tract/blob/main/wgpu/Cargo.toml)
- 0.23.7 (2026-09-08) moved Metal convolution to a port of MLX's tiled implicit-GEMM for NHWC f16/f32 single-group 2D convs. The old direct kernel reached about 20 GFLOP/s on one M1 Pro layer — [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md)
- 0.23.5 added Metal SDPA (ported MLX kernels), Metal pooling and Metal Resize — [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md)
- An earlier 0.23.x entry added "native STFT / FFT on GPU" via a backend-agnostic `GpuStft` for CUDA and Metal. The repo contains `metal/src/kernels/fft.metal` and `cuda/src/kernels/cu/fft.cu` — [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md); [tract metal kernels](https://github.com/sonos/tract/tree/main/metal/src/kernels)

**FFT (needed by LaMa)**
- tract-onnx registers ONNX `DFT` (opset 17 and later forms, with `inverse`/`onesided`) and `STFT`. The ONNX op directory also has GridSample, Einsum, LayerNorm, GroupNorm, InstanceNorm, Attention/MultiHeadAttention, ConvTranspose and others — [tract onnx/src/ops/fft.rs](https://github.com/sonos/tract/blob/main/onnx/src/ops/fft.rs); [tract onnx/src/ops](https://github.com/sonos/tract/tree/main/onnx/src/ops)
- 0.23.8 fixed `DFT` with `inverse` and `onesided` both set, which is an irfft. The flags had been handled independently, returning the wrong number of bins — [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md)

**Performance evidence and examples**
- The Stable Diffusion 1.5 example runs text encoder, UNet and VAE on tract's CUDA backend: about 8 s for one 512² image at 20 steps on "an NVIDIA GPU (32GB VRAM)". SDXL and SD3 examples also exist — [tract examples/stable-diffusion](https://github.com/sonos/tract/tree/main/examples/stable-diffusion)
- A community `tract-coreml` draft (a Core ML / ANE bridge via MLProgram) was closed in 2026-05 without merging. The maintainer was interested but cautious about NPUs and high-level NN APIs. The contributor found that ORT+CoreML EP did not beat ORT CPU on a recurrent audio decoder and that "NPU are for 2D CNN" — [sonos/tract #2215](https://github.com/sonos/tract/pull/2215)

### Inferences
- tract is a credible pure-Rust candidate for all three workloads on the Mac GPU (Metal). It also keeps the licence rule and needs no second toolchain. Its Metal backend is new and optimised for LLM/ASR workloads first, so ViT-style SAM encoders and LaMa's FFC/FFT blocks are unproven on it.
- LaMa on tract depends on the irfft path fixed only on 2026-09-21. Any adoption must pin ≥ 0.23.8 and include exact-output tests against a PyTorch reference.
- `tract-wgpu` targets wgpu 30, while Luxforge uses wgpu 27 through iced 0.14. Sharing Luxforge's wgpu `Device` with tract would need a wgpu upgrade in lockstep with iced. Otherwise tract would run its own Metal or wgpu device, with a CPU hop for results.
- tract has no Neural Engine path, so ANE-class efficiency is available only through Core ML (ORT's CoreML EP or direct Core ML).

### Gaps
- I found no published latency for SAM/MobileSAM/EfficientSAM or LaMa on tract, CPU or Metal, and no tract-vs-ORT benchmark on Apple Silicon. This is the central measurement Luxforge needs.
- I did not verify tract's handling of dynamic input sizes for LaMa (512–2048 px). Symbolic shapes are documented in `doc/symbolic-shapes.md` but were not examined.

## candle (Hugging Face): Metal/CUDA backends, model coverage, M-series performance and maturity

### Takeaway
candle 0.11.0 (2026-06-26) has Metal and CUDA backends and native Rust implementations of SAM (ViT-B and a MobileSAM TinyViT), Depth Anything v2, DINOv2, SegFormer, Stable Diffusion 1.5/2.1/XL/3, FLUX and Würstchen. It has **no FFT op**, so LaMa would need hand-written FFT kernels. Its Metal backend had documented 2–11× GEMM gaps against MLX and PyTorch MPS (issue closed January 2026) and an open SAM Metal bug. It suits "port a model in Rust, load safetensors", not "run arbitrary ONNX".

### Cited Findings
**Releases and model coverage**
- Recent tags are 0.11.0, 0.10.2, 0.10.1 and 0.10.0; the last commit was 2026-09-25. `candle-core` 0.11.0 was published 2026-06-26 (MIT OR Apache-2.0, about 8.7 M downloads) — [GitHub tags API: huggingface/candle](https://github.com/huggingface/candle/tags); [crates.io candle-core](https://crates.io/crates/candle-core)
- `candle-transformers/src/models` includes `segment_anything`, `depth_anything_v2`, `dinov2`, `segformer`, `efficientvit`, `stable_diffusion`, `mmdit` (SD3), `flux`, `wuerstchen`, `z_image` and many LLMs — [candle-transformers models](https://github.com/huggingface/candle/tree/main/candle-transformers/src/models)
- The segment-anything example supports a `--use-tiny` TinyViT/MobileSAM backbone with point and box prompts — [candle segment-anything example](https://github.com/huggingface/candle/tree/main/candle-examples/examples/segment-anything)

**Missing ops and bugs**
- The candle repository tree has no file matching `fft` or `dft`. I checked the git tree on 2026-09-27. An `fft` issue search returns only audio-model items — [huggingface/candle tree](https://github.com/huggingface/candle)
- Issue #3307 (open, 2026-01-16): the SAM example fails on Metal with `--generate-masks`, raising an invalid-matmul-arguments error in `mlx_gemm` — [candle #3307](https://github.com/huggingface/candle/issues/3307)
- Issue #3966 (open, 2026-09-08) adds SAM 2 image prediction, so SAM 2 is not yet merged — [candle #3966](https://github.com/huggingface/candle/issues/3966)

**Metal performance**
- Issue #3302 (closed 2026-01-14) reported candle's Metal GEMM 2–11× slower than PyTorch MPS and MLX, caused by a hard-coded (32,32,16) tile. One example is a 4D attention matmul: 2.20 ms on MLX against 24.65 ms on candle — [candle #3302](https://github.com/huggingface/candle/issues/3302)
- Issue #1780 (open since 2024) reports that Metal→CPU `Tensor::to_device` is slow for model output — [candle #1780](https://github.com/huggingface/candle/issues/1780)

**Related crates**
- `candle-onnx` 0.11.0 exists; ort's `ort-candle` backend wraps candle behind the ort API — [crates.io candle-onnx](https://crates.io/crates/candle-onnx); [crates.io ort-candle](https://crates.io/crates/ort-candle)

### Inferences
- For SAM, depth and segmentation, candle offers a pure-Rust Metal path with safetensors weights and no ONNX, but Luxforge would own the model code. For LaMa it adds the extra work of writing an FFT. Given the Metal GEMM history and the SAM Metal bug, candle is a weaker Mac candidate than tract or ORT CoreML unless measurement says otherwise.

### Gaps
- I found no published SAM encoder or decoder latency for candle Metal on M-series. I could not confirm whether the #3302 fix shipped in 0.11.0, and did not verify `candle-onnx` operator coverage.

## burn (tracel-ai): wgpu/Metal/CUDA backends, ONNX import and coverage

### Takeaway
burn 0.21.0 (2026-05-07; 0.22.0-pre.4 on 2026-09-22) runs on CubeCL backends: wgpu (Metal/Vulkan/DX12), CUDA and ROCm, plus a new `burn-flex` CPU backend. It gained rfft/irfft, complex FFT and STFT in 0.21. ONNX import now lives in the separate `burn-onnx` repo and **generates Rust code at build time** from an ONNX file; its table marks DFT, STFT, GridSample, Attention, Resize and the norms as supported. burn depends on wgpu 30, which does not match Luxforge's wgpu 27.

### Cited Findings
**Releases and backends**
- Releases: v0.21.0 (2026-05-07); v0.22.0-pre.1 through pre.4 (2026-07-29 → 2026-09-22) — [GitHub releases API: tracel-ai/burn](https://github.com/tracel-ai/burn/releases)
- burn 0.21 added rfft and irfft with fusion, STFT/ISTFT, complex-to-complex FFT, and the `burn-flex` CPU backend — [burn v0.21.0 release notes](https://github.com/tracel-ai/burn/releases/tag/v0.21.0)
- `burn-wgpu` and `burn-cubecl` 0.22.0-pre.4 depend on `cubecl` 0.11.0-pre.4. `cubecl-wgpu` requires `wgpu ^30.0.0` and `objc2-metal ^0.3` — [crates.io burn-wgpu](https://crates.io/crates/burn-wgpu); [crates.io cubecl-wgpu](https://crates.io/crates/cubecl-wgpu)

**ONNX import**
- `tracel-ai/burn-onnx` (pushed 2026-09-27) converts ONNX models into native, backend-agnostic Burn code. Its table lists DFT, STFT (non-power-of-two frames computed as an f64 matmul), GridSample, Attention, Resize, LayerNorm, GroupNorm, Einsum and ConvTranspose as supported; AffineGrid is among the unsupported — [burn-onnx SUPPORTED-ONNX-OPS.md](https://github.com/tracel-ai/burn-onnx/blob/main/SUPPORTED-ONNX-OPS.md); [tracel-ai/burn-onnx](https://github.com/tracel-ai/burn-onnx)

### Inferences
- burn's build-time code generation means each model version becomes compiled Rust, and weights are loaded separately. That fits "hash-verified downloaded resources" for the weights, but a new model *architecture* would need an app release. That is a real constraint for a downloaded-models design.
- The wgpu backend is portable across all three OSes, but the version mismatch blocks device sharing with iced today. burn's ANE access is none.

### Gaps
- I found no burn benchmarks for SAM or LaMa on Apple Silicon, and no evidence of a shipped SAM or LaMa desktop app on burn.

## Other options: wonnx, MLX/mlx-rs, Core ML via objc2, MPSGraph, Windows ML/DirectML, stable-diffusion.cpp/ggml, IREE, ExecuTorch, rten, and OS segmentation APIs

### Takeaway
Most of these are either dead (wonnx), single-platform (MLX, Core ML, MPSGraph, Windows ML, Apple Vision), CPU-only (rten), or thinly bound to Rust (IREE, ExecuTorch, stable-diffusion.cpp). The useful ones are:
- **direct Core ML through `objc2-core-ml`**, as a Mac-only fast path for ANE and GPU;
- **Windows ML**, as the forward path replacing DirectML on Windows 11 24H2+;
- **stable-diffusion.cpp**, if diffusion is ever in scope.

### Cited Findings
**wonnx and rten**
- wonnx (WebGPU ONNX runtime in Rust) is **archived**; its last push was 2024-07-21 and the last crate release 0.5.1 in 2023-09 — [webonnx/wonnx](https://github.com/webonnx/wonnx); [crates.io wonnx](https://crates.io/crates/wonnx)
- rten 0.26.0 (2026-08-29, MIT OR Apache-2.0) is a pure-Rust ONNX engine. It is CPU-only, with AVX2, AVX-512, NEON and WASM SIMD, and upconverts f16 weights to f32 at load. The repo has FFT op sources (`src/ops/fft.rs`) — [robertknight/rten](https://github.com/robertknight/rten); [crates.io rten](https://crates.io/crates/rten)

**MLX**
- mlx-rs 0.32.0 (2026-09-12) is an *unofficial* binding (oxiglade/mlx-rs, MIT/Apache) that follows MLX's version numbers, with a `metal` feature; MSRV 1.88. MLX itself is MIT, with about 28.6 k stars — [oxiglade/mlx-rs](https://github.com/oxiglade/mlx-rs); [ml-explore/mlx](https://github.com/ml-explore/mlx); [crates.io mlx-rs](https://crates.io/crates/mlx-rs)
- mlx-examples includes a SAM port that converts `facebook/sam-vit-base` safetensors — [mlx-examples segment_anything](https://github.com/ml-explore/mlx-examples/tree/main/segment_anything)
- mlx-lama-swift ports LaMa and MI-GAN to MLX-Swift using MLX's native FFT. It reports about 162 ms for a 1024×680 image (conv2d route; hardware not stated). It warns that **fp16 "collapses the FFC → garbage"**, so LaMa runs in bf16. Licences: port MIT, LaMa Apache-2.0, MI-GAN MIT — [xocialize/mlx-lama-swift](https://github.com/xocialize/mlx-lama-swift)

**Core ML and MPSGraph from Rust**
- `objc2-core-ml` 0.3.2 and `objc2-metal-performance-shaders-graph` 0.3.2 (2025-10-04, Zlib OR Apache-2.0 OR MIT) provide generated Rust bindings to Core ML and MPSGraph — [crates.io objc2-core-ml](https://crates.io/crates/objc2-core-ml); [crates.io objc2-metal-performance-shaders-graph](https://crates.io/crates/objc2-metal-performance-shaders-graph); [docs.rs MLMultiArray](https://docs.rs/objc2-core-ml/latest/objc2_core_ml/struct.MLMultiArray.html)
- Core ML's `MLMultiArray init(pixelBuffer:shape:)` creates an IOSurface-backed float16 array (`OneComponent16Half`) that "can reduce inference latency by avoiding the buffer copy" — [Apple docs: init(pixelBuffer:shape:)](https://developer.apple.com/documentation/coreml/mlmultiarray/init(pixelbuffer:shape:))

**Windows ML and DirectML**
- DirectML is "in maintenance mode": supported and shipping with Windows, but with no new features planned. New development moved to **Windows ML** on Windows 11 24H2 (build 26100) and later — [microsoft/DirectML README](https://github.com/microsoft/DirectML); [DirectML sustained-engineering note](https://github.com/MicrosoftDocs/windows-ai-docs/blob/docs/docs/directml/includes/directml-sustained-engineering-note.md)
- Windows ML exposes ONNX Runtime APIs and downloads vendor EPs (NPU/GPU) dynamically via `ExecutionProviderCatalog` on Windows 11 24H2+ — [Windows ML supported EPs](https://learn.microsoft.com/en-us/windows/ai/new-windows-ml/supported-execution-providers); [Windows Developer Blog, 2025-05-19](https://blogs.windows.com/windowsdeveloper/2025/05/19/introducing-windows-ml-the-future-of-machine-learning-development-on-windows/)
- Per Microsoft's DirectML docs, the `Microsoft.ML.OnnxRuntime.DirectML` package stopped at 1.24.4. This is from a search summary of the doc and was not re-read — [windows-ai-docs dml.md](https://github.com/MicrosoftDocs/windows-ai-docs/blob/docs/docs/directml/dml.md)

**Diffusion, IREE and ExecuTorch**
- stable-diffusion.cpp (MIT, pushed 2026-09-27, about 7.4 k stars) is pure C/C++ on ggml with CUDA, Vulkan and Metal backends and safetensors/GGUF weights. It supports SD1.x/SDXL, FLUX.1, FLUX.2 (dev/klein), FLUX.1-Kontext and Qwen Image Edit. The Rust binding `diffusion-rs` 0.1.20 (MIT, 2026-06-16) has little uptake (about 900 recent downloads) — [leejet/stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp); [crates.io diffusion-rs](https://crates.io/crates/diffusion-rs)
- IREE (Apache-2.0) is active, but its Rust bindings are thin: `eerie` 0.5.0 (2026-06-25, about 585 recent downloads) and a stale `iree-sys` 0.1.0 (2023) — [iree-org/iree](https://github.com/iree-org/iree); [crates.io eerie](https://crates.io/crates/eerie)
- ExecuTorch is active, and its Rust crates `executorch`/`executorch-sys` 0.12.1 (Apache-2.0, 2026-09-02) have about 3 k recent downloads — [pytorch/executorch](https://github.com/pytorch/executorch); [crates.io executorch](https://crates.io/crates/executorch)

**Apple Vision (built into macOS)**
- `VNGeneratePersonInstanceMaskRequest` gives per-person masks with no model download, on macOS 14 / iOS 17 — [Apple docs](https://developer.apple.com/documentation/vision/vngeneratepersoninstancemaskrequest)
- Subject lifting (`VNGenerateForegroundInstanceMaskRequest`) was introduced at WWDC23 — [WWDC23 notes](https://wwdcnotes.com/documentation/wwdc23-10176-lift-subjects-from-images-in-your-app/)
- One search snippet claimed the foreground request works on macOS 11+. That conflicts with its WWDC23 (macOS 14) introduction; treat macOS 14 as correct unless the docs say otherwise.

### Inferences
- Direct Core ML via objc2 is the only way from Rust to get deliberate ANE placement, `.mlpackage` models tuned with coremltools (palettization, fp16 typing), and IOSurface zero-copy. Its costs are Mac-only code, a second model format per model, and conversion through Python coremltools as an offline tool (outside the app's build, so arguably not a "second toolchain" in the app).
- The Apple Vision APIs could serve "person" and "salient subject" masks on macOS with no download. They are not portable, and the model is OS-versioned, which conflicts with reproducible recipes: the same recipe could render differently after a macOS update.
- On Windows, ORT with DirectML (pyke's default build) works on Windows 10 and 11 but sits on a maintenance-mode API. Windows ML is the future path but needs 24H2 and Windows App SDK packaging; ort has no documented Windows ML integration.

### Gaps
- I did not verify ExecuTorch's Core ML/MPS delegates from Rust, IREE's Metal backend maturity, or whether MLX's Linux/CUDA support is usable from mlx-rs.
- I found no latency figures for MLX SAM on M-series.

## How comparable open-source apps do it, and published latencies (SAM variants, LaMa, diffusion) on Apple Silicon

### Takeaway
darktable, a GPL-3 C app, is the closest precedent:
- ONNX Runtime with CoreML on macOS, DirectML on Windows, and CUDA/MIGraphX on Linux, with a CPU fallback ladder;
- models downloaded as zip archives from a GitHub release;
- AI off by default.

In Rust, `usls` (MIT) wraps ort for SAM, SAM2, MobileSAM, EdgeSAM and SAM-HQ. A 2026 inpainting decision in another project chose LaMa ONNX via Windows ML. Published Apple Silicon numbers exist for Core ML (RepViT-SAM, MobileSAM, EdgeSAM, Stable Diffusion) and MLX (LaMa), but **none for SAM or LaMa via ORT's CoreML EP, tract Metal or candle Metal**.

### Cited Findings
**darktable (GPL-3, C)**
- ONNX Runtime through a self-contained `src/ai/` library. Providers:

  | Platform | Order |
  | --- | --- |
  | macOS | CoreML → CPU |
  | Windows | DirectML → CPU |
  | Linux | CUDA → MIGraphX → ROCm → CPU |

  Missing providers are handled gracefully, and one binary works with CPU-only and GPU ORT builds. AI features are **disabled by default** so ORT is not loaded unless enabled — [darktable dev-doc/AI.md](https://github.com/darktable-org/darktable/blob/master/dev-doc/AI.md)
- Models (SAM, SegNext, RawNIND denoise, upscale) are listed in a bundled `ai_models.json` and fetched as `.dtmodel` zip archives from `darktable-ai` GitHub releases. The SAM2 decoder needs *Basic* graph optimisation to avoid shape-inference failures — [darktable dev-doc/AI.md](https://github.com/darktable-org/darktable/blob/master/dev-doc/AI.md)
- `backend_onnx.c`:
  - sets CoreML `ModelFormat` (MLProgram or NeuralNetwork), `MLComputeUnits` (CPUOnly or ALL) and a persistent `ModelCacheDirectory`;
  - links ORT at compile time on Windows and macOS, but lazy-loads it on Linux;
  - on session failure falls back to provider + BASIC, then CPU at all/basic/disabled optimisation;
  - auto-converts fp32↔fp16 at the boundary.

  — [darktable src/ai/backend_onnx.c](https://github.com/darktable-org/darktable/blob/master/src/ai/backend_onnx.c)

**Other precedents**
- usls (MIT, Rust, pushed 2026-07-30) wraps ort and lists SAM, SAM-HQ, MobileSAM, EdgeSAM, SAM2 and FastSAM, with CPU, CUDA, TensorRT, CoreML, OpenVINO and DirectML EPs. Its published benchmarks are YOLO on CUDA/TensorRT, not Apple — [jamjamjon/usls](https://github.com/jamjamjon/usls)
- SysAdminDoc/Images, decided 2026-05-17, uses Windows ML (`Microsoft.Windows.AI.MachineLearning` 2.1.74; NPU, then GPU, then DirectML/CPU) with SHA-256-pinned LaMa ONNX models:
  - `opencv/inpainting_lama` 2025jan (93 MB);
  - `Carve/LaMa-ONNX` fp32 at 512² as fallback.

  It explicitly rejected diffusion as "too large, too slow, and too generative". It publishes no latency figures — [SysAdminDoc/Images inpaint-runtime-decision.md](https://github.com/SysAdminDoc/Images/blob/main/docs/inpaint-runtime-decision.md)

**LaMa exports and ports**
- `Carve/LaMa-ONNX` (Apache-2.0) has a fixed 512×512 input in two variants:
  - `lama_fp32.onnx` (recommended; opset 17; torch.onnx.export) uses a custom "FourierUnitJIT";
  - `lama.onnx` (opset 18; dynamo export) uses custom irfftn logic and "works slowly".

  Dynamic shapes would need FFT padding fixes — [Carve/LaMa-ONNX](https://huggingface.co/Carve/LaMa-ONNX)
- Exporting `aten::fft_rfftn` to ONNX opset 17 historically failed — [onnx/onnx #4843](https://github.com/onnx/onnx/issues/4843)
- CoreMLaMa converts Big-LaMa to `LaMa.mlpackage`; the FFT became convertible after a coremltools update. It runs well on the macOS GPU, but reports of fp16/Neural Engine runs failed and there are no successful iOS reports — [mallman/CoreMLaMa](https://github.com/mallman/CoreMLaMa)

**Published latencies**
- SAM paper: with a precomputed embedding, the prompt encoder and mask decoder run "in a web browser, on CPU, in ∼50ms" — [Segment Anything, arXiv 2304.02643](https://arxiv.org/pdf/2304.02643)
- MobileSAM on a single GPU: image encoder 8 ms (SAM ViT-H 452 ms) and mask decoder 4 ms (3.876 M params). It takes about 3 s on "our own Mac i5 CPU". Apache-2.0 — [ChaoningZhang/MobileSAM](https://github.com/ChaoningZhang/MobileSAM)
- RepViT-SAM paper, measured with Core ML Tools at 1024×1024 on a MacBook M1 Pro:

  | Model | Latency |
  | --- | --- |
  | RepViT-SAM | 44.8 ms |
  | MobileSAM | 482.2 ms |
  | ViT-B-SAM | 6249.5 ms |

  On iPhone 12, RepViT-SAM ran in 48.9 ms — [RepViT-SAM, arXiv 2312.05760](https://arxiv.org/html/2312.05760)
- EdgeSAM: 38.7 FPS on iPhone 14 (Core ML) and 164.3 FPS on an RTX 2080 Ti, reported as 40× faster than SAM and 14× faster than MobileSAM on edge devices. It exports Core ML encoder/decoder packages and ONNX. Licence: NTU S-Lab License 1.0 — [chongzhou96/EdgeSAM](https://github.com/chongzhou96/EdgeSAM)
- Apple's `coreml-sam2` models are fp16 conversions of SAM 2 (Apache-2.0). The model card gives no latency — [apple/coreml-sam2-tiny](https://huggingface.co/apple/coreml-sam2-tiny)
- LaMa Core ML on iPhone 13 Pro: about 2 s (a blog post, secondary) — [MLBoy, Medium](https://rockyshikoku.medium.com/woooooooo-34fb311b2a9b)
- An old IOPaint issue reports 5–6 min for a 1920×1000 photo on an M1 mini in CPU mode — [Sanster/IOPaint #54](https://github.com/Sanster/lama-cleaner/issues/54)
- An IOPaint discussion claims Core ML on M2 GPU is much faster than PyTorch MPS, and "sub-second results on 45 MP images". That figure is unverified and implausible at face value, so treat it with caution — [IOPaint discussion #314](https://github.com/Sanster/lama-cleaner/discussions/314)
- Apple ml-stable-diffusion (Core ML):
  - SDXL 1024² takes 37 s on a MacBook Pro M2 Max and 20 s on an M2 Ultra (CPU_AND_GPU);
  - SD 2.1 512² takes 7.0 s on an iPad Pro M2 (CPU_AND_NE; 6-bit weights on mobile);
  - `.mlmodelc` caches the first-load compile.

  — [apple/ml-stable-diffusion](https://github.com/apple/ml-stable-diffusion)

### Inferences
- The hover budget (5–20 ms per decoder call) is plausible on CPU for SAM-family decoders: the decoder is about 3.9 M params, and it took about 50 ms in 2023 browser WASM. Native NEON or KleidiAI CPU on an M4 should be several times faster; this is an estimate to measure. The *encoder* choice matters more.
- The RepViT-SAM table shows that architecture decides ANE fitness as much as parameter count. MobileSAM's TinyViT was about 10× slower than RepViT-SAM under Core ML on M1 Pro. Pick encoders by measured Core ML placement, not by FLOPs.
- Only darktable (GPL-3, ORT + CoreML/DirectML) and usls (ort + SAM family) are directly comparable desktop precedents. Neither publishes Mac latencies.

### Gaps
- I found no published numbers for:
  - SAM, MobileSAM or EfficientSAM through ORT CoreML EP vs ORT CPU on M1–M4;
  - LaMa via ORT on Apple Silicon at 512/1024/2048;
  - candle Metal or MLX SAM on M-series;
  - tract anything vision-shaped.
- EdgeSAM's NTU S-Lab licence text was not read. It is reportedly restrictive, so check it before use.

## Model format, security and precision (ONNX vs safetensors vs .mlpackage vs GGUF; fp16/int8; ANE eligibility)

### Takeaway
**Formats:**
- ONNX (protobuf graph plus weights) is the lingua franca for ort, tract, burn-onnx and rten.
- safetensors (weights only) suits candle, MLX and stable-diffusion.cpp, where the graph is code.
- `.mlpackage` is Core ML only.
- GGUF is ggml only.

All four avoid pickle. For the Neural Engine, fp16 is effectively required: coremltools says fp32-typed models are barred from the ANE. But LaMa's FFC is fp16-fragile (bf16 or fp32 needed), so LaMa will likely run on the GPU, not the ANE.

### Cited Findings
**Precision and the ANE**
- coremltools says that when a model is typed float32, "only the Neural Engine is barred" as a compute unit. `mlprogram` conversion defaults to float16 compute precision, and mixed precision can keep chosen ops in fp32 — [coremltools Typed Execution](https://apple.github.io/coremltools/docs-guides/source/typed-execution.html); [coremltools convert API](https://apple.github.io/coremltools/source/coremltools.converters.convert.html)
- A model card (secondary source) reports that through ORT's CoreML EP, fp32 ONNX graphs never reached the ANE while the fp16 version placed most ops on it. Its example is 12.23 ms fp16 vs 35.57 ms fp32 for a vision encoder — [Arraasz/granite-embedding-small-english-r2-ane](https://huggingface.co/Arraasz/granite-embedding-small-english-r2-ane)
- LaMa in fp16 "collapses the FFC → garbage" in the MLX port, which uses bf16 — [xocialize/mlx-lama-swift](https://github.com/xocialize/mlx-lama-swift)
- CoreMLaMa reports failures with fp16 on the Neural Engine — [mallman/CoreMLaMa](https://github.com/mallman/CoreMLaMa)
- Apple's SAM 2 Core ML models are fp16 — [apple/coreml-sam2-tiny](https://huggingface.co/apple/coreml-sam2-tiny)

**Formats and loaders**
- darktable auto-converts fp32↔fp16 at model inputs and outputs when a model expects half precision — [darktable backend_onnx.c](https://github.com/darktable-org/darktable/blob/master/src/ai/backend_onnx.c)
- rten upconverts f16 weights to f32 at load. tract has f16 conv on its GPU runtimes — [robertknight/rten](https://github.com/robertknight/rten); [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md)
- tract-OPL/NNEF models embed a `tract_nnef_ser_version`. tract does not enforce it, so apps must check it themselves. Models serialised by 0.x.y should load in 0.x.z where z ≥ y — [sonos/tract README](https://github.com/sonos/tract)
- stable-diffusion.cpp reads `.safetensors` and `.gguf` and can convert between them — [leejet/stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp)
- `MLMultiArray` from a pixel buffer must be float16 (`OneComponent16Half`) — [Apple docs](https://developer.apple.com/documentation/coreml/mlmultiarray/init(pixelbuffer:shape:))

### Inferences
- On security, none of these loaders runs embedded code the way pickle does. ONNX adds an attack surface through protobuf parsing and custom-op domains. Luxforge should pin models by SHA-256 (already planned) and reject unknown op domains. Prefer the tract NNEF path, which removes protobuf, or ORT with custom ops disabled. Also check the tract NNEF serialisation version against the pinned tract version.
- On precision, plan to store fp32 ONNX masters and derive fp16 variants per runtime:
  - SAM-family encoders in fp16 are ANE candidates;
  - LaMa stays fp32 (or bf16 on MLX) on GPU or CPU;
  - int8 is worth testing for the decoder's CPU path only after quality checks (mask IoU vs fp32).
- The recipe must record model hash *and* runtime/EP/precision, because an ANE fp16 mask can differ from a CPU fp32 mask. This follows Pillar 6 and the "stored selection" semantics in `masking-workspace.md`.

### Gaps
- I did not fetch the safetensors or ONNX external-data docs in this session. The claim that pickle is unsafe and safetensors is not rests on general knowledge and should be cited from the [safetensors docs](https://huggingface.co/docs/safetensors/index) before it goes into a design doc.
- I did not find a published quality study of int8 SAM decoders or LaMa.

## Integration with wgpu: can outputs stay on the GPU, or is a CPU round trip acceptable?

### Takeaway
Keeping inference outputs inside Luxforge's wgpu device is not practical today:
- ORT/CoreML returns host tensors;
- tract-wgpu and burn use wgpu 30, while Luxforge is on wgpu 27 via iced 0.14;
- Core ML zero-copy needs IOSurface plumbing through objc2.

A CPU round trip is acceptable. A 1024² f32 mask is 4 MiB and a SAM low-res mask is 256 KiB, a trivial copy on unified memory. The real costs are Core ML and GPU launch latency, and first-load compilation.

### Cited Findings
- Luxforge pins `wgpu 27.0.1` and `iced 0.14.0` (worktree `Cargo.lock`).
- `tract` pins `wgpu 30.0.1` and `cubecl-wgpu` requires `wgpu ^30.0.0` — [tract Cargo.toml](https://github.com/sonos/tract/blob/main/Cargo.toml); [crates.io cubecl-wgpu](https://crates.io/crates/cubecl-wgpu)
- M4 Pro unified memory bandwidth is 273 GB/s, M4 Max up to 546 GB/s, and the M4 Neural Engine is advertised at 38 TOPS — [Apple Newsroom, 2024-10](https://www.apple.com/newsroom/2024/10/apple-introduces-m4-pro-and-m4-max/)
- Core ML can take IOSurface-backed fp16 `MLMultiArray`s, avoiding a buffer copy — [Apple docs](https://developer.apple.com/documentation/coreml/mlmultiarray/init(pixelbuffer:shape:))
- ort rc.10 added manual tensor device copies (`Tensor::to`, shown with CUDA). ORT 1.30 "rejected foreign GPU handles in built-in data transfers" for WebGPU — [ort v2.0.0-rc.10](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.10); [ORT v1.30.0](https://github.com/microsoft/onnxruntime/releases/tag/v1.30.0)
- candle issue #1780 reports slow Metal→CPU output transfer — [candle #1780](https://github.com/huggingface/candle/issues/1780)
- Luxforge memory notes that each iced hop costs a frame (about 8 ms at 120 Hz), so the hover path's cost is dominated by scheduling, not copies (project memory `iced-hop-costs-a-frame.md`).

### Inferences
- The estimated copy cost is small. 4 MiB at ≥100 GB/s effective is about 40 µs, and a 256 KiB mask is about 3 µs. This is arithmetic, not a measurement. The hover decoder's end-to-end latency will be set by inference (CPU or accelerator dispatch) plus the UI hop, so a synchronous hand-off from the inference worker to the owner is what matters.
- A later zero-copy path on macOS would be possible by rendering or uploading the image into an IOSurface shared by a Metal texture (wgpu-hal) and a Core ML input. That needs objc2 Core ML directly, not ORT. It is not worth doing before measurements show the copy matters.
- If Luxforge later upgrades to an iced built on wgpu ≥ 30, tract-wgpu (alpha) or burn could share the renderer's device. That makes a wgpu-native inference path a medium-term option, not a v1 one.

### Gaps
- I found no measured Core ML or ORT-CoreML dispatch overhead for tiny models (the SAM decoder) on M4. It could be larger than CPU execution, and must be measured. The tract-coreml discussion hints at it for small recurrent models ([sonos/tract #2215](https://github.com/sonos/tract/pull/2215)).

## Licensing and GPL-3.0-or-later compatibility (runtimes and transitive native dependencies)

### Takeaway
**Compatible with GPL-3.0-or-later:**
- ONNX Runtime (MIT), ort, tract, candle, burn, rten, mlx-rs and objc2 (all MIT and/or Apache-2.0);
- MLX, ggml and stable-diffusion.cpp (MIT);
- IREE and ExecuTorch (Apache-2.0).

**Proprietary pieces that should not be bundled:**
- DirectML's redistributable package, under Microsoft's proprietary licence. It also ships in Windows 10 1903+ as a system component, which supports relying on the OS copy.
- CUDA and cuDNN, under NVIDIA's EULA. Load the user's installed copies at runtime instead of bundling them, as darktable does.

Core ML, Metal and Vision are OS system libraries.

### Cited Findings
**Runtime licences**
- Crate licences on crates.io:

  | Licence | Crates |
  | --- | --- |
  | MIT OR Apache-2.0 | ort, tract-*, candle-*, burn-*, rten, mlx-rs, mlx-sys |
  | Zlib OR Apache-2.0 OR MIT | objc2-core-ml, objc2-metal-performance-shaders-graph |
  | MIT | diffusion-rs |
  | Apache-2.0 | executorch |

  — [crates.io API queries on 2026-09-27](https://crates.io/crates/ort)
- Repo licences: MLX MIT, stable-diffusion.cpp MIT, ggml MIT, IREE Apache-2.0 — [GitHub repos API](https://github.com/ml-explore/mlx)
- tract's README says files in `onnx/protos` and `tensorflow/protos` are copied from those projects and not covered by tract's licence statement — [sonos/tract README](https://github.com/sonos/tract)
- Apache-2.0 is compatible with GPLv3 (not GPLv2) — [FSF license list](https://www.gnu.org/licenses/license-list.html#apache2)
- GPLv3 §1 excludes "System Libraries" of a "Major Component" of the OS from Corresponding Source — [GPLv3 text](https://www.gnu.org/licenses/gpl-3.0.html)

**Proprietary native dependencies**
- The DirectML redistributable (`Microsoft.AI.DirectML` 1.15.4, 2024-10-28) is under "MICROSOFT SOFTWARE LICENSE TERMS – MICROSOFT DIRECTX MACHINE LEARNING". It permits distribution inside your applications but prohibits reverse engineering and standalone distribution. It is not open source — [NuGet license](https://www.nuget.org/packages/Microsoft.AI.DirectML/1.15.4/license)
- DirectML also ships as a Windows system component since Windows 10 1903 — [microsoft/DirectML README](https://github.com/microsoft/DirectML)
- The CUDA EULA:
  - lists redistributables (cudart, cuFFT, cuBLAS, NPP, nvJPEG, nvrtc and others) that may ship only inside an application with material additional functionality;
  - forbids use that would make the SDK subject to an open-source licence;
  - explicitly allows developing OSI-licensed applications.

  — [NVIDIA CUDA EULA](https://docs.nvidia.com/cuda/eula/index.html)
- darktable (GPL-3) ships ORT with CoreML (macOS) and DirectML (Windows) linked at compile time. On Linux it lazy-loads ORT and the user's CUDA/ROCm — [darktable backend_onnx.c](https://github.com/darktable-org/darktable/blob/master/src/ai/backend_onnx.c)

**Model licences**
| Model | Licence |
| --- | --- |
| LaMa (Carve ONNX, opencv) | Apache-2.0 |
| MobileSAM | Apache-2.0 |
| Apple coreml-sam2 | Apache-2.0 |
| MI-GAN | MIT |
| EdgeSAM | NTU S-Lab 1.0 (needs review) |

— [Carve/LaMa-ONNX](https://huggingface.co/Carve/LaMa-ONNX); [MobileSAM](https://github.com/ChaoningZhang/MobileSAM); [apple/coreml-sam2-tiny](https://huggingface.co/apple/coreml-sam2-tiny); [mlx-lama-swift](https://github.com/xocialize/mlx-lama-swift); [EdgeSAM](https://github.com/chongzhou96/EdgeSAM)

### Inferences
- This is not legal advice, and AGENTS.md says licence reviews are deferred. On the Mac-first path, ort+CoreML and tract carry no proprietary redistribution. On Windows, pyke's DirectML build links ORT's DirectML EP. Whether it loads the OS `DirectML.dll` or needs the redistributable DLL next to the app decides whether a proprietary binary ships. Prefer the OS copy or WebGPU/CPU.
- CUDA/TensorRT should stay "bring your own", loaded at runtime (ort `load-dynamic` or the CUDA plugin EP) and never packaged.

### Gaps
- I did not confirm which `DirectML.dll` pyke's Windows build loads (system or side-by-side), nor the FSF's view on the DirectML redistributable specifically.

## Risks: maintenance, OS version floors, notarization and codesigning, app size

### Takeaway
The main risks:
- ort is a single-maintainer, still-RC binding whose maintainer no longer tests macOS;
- ORT CoreML has had correctness regressions;
- tract's GPU/FFT paths are weeks old;
- candle's Metal path has open SAM bugs;
- wonnx is archived;
- DirectML is in maintenance.

On macOS, static linking (the pyke default, tract, candle, burn) avoids signing a separate dylib. `load-dynamic` or MLX adds dylibs that must be signed with the team ID under the hardened runtime. OS floors differ by path:

| Path | Floor |
| --- | --- |
| pyke ORT builds | macOS 13.4 |
| CoreML MLProgram | macOS 12 |
| Vision person masks | macOS 14 |
| Windows ML | Windows 11 24H2 |

### Cited Findings
**Maintenance**
- ort: rc.11 raised macOS to 13.4, dropped Intel Macs and warned of little macOS support. The CDN is single-maintainer; rc.13 was the third RC after "the next release should be 2.0.0" — [ort v2.0.0-rc.11](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.11); [ort prebuilt binaries docs](https://github.com/pykeio/ort/blob/main/docs/content/misc/prebuilt-binaries.mdx)
- ORT CoreML FP16 wrong-output bug on macOS 15 (open, 2026-09-12) — [onnxruntime #32569](https://github.com/microsoft/onnxruntime/issues/32569)
- tract irfft correctness fix (2026-09-21), Metal implicit-GEMM conv (2026-09-08) and tract-wgpu alpha (2026-09-21) — [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md)
- candle SAM-on-Metal error open since 2026-01 — [candle #3307](https://github.com/huggingface/candle/issues/3307)
- wonnx archived — [webonnx/wonnx](https://github.com/webonnx/wonnx)
- DirectML maintenance mode — [microsoft/DirectML](https://github.com/microsoft/DirectML)

**macOS signing**
- Notarization requires the hardened runtime. Library validation then allows only code signed by the same team or by Apple, unless the Disable Library Validation entitlement is used. Third-party-signed dylibs fail with Team ID mismatches — [Apple Developer Forums: Resolving Library Loading Problems](https://developer.apple.com/forums/thread/706437); [Apple Developer Forums: signing 3rd-party dylibs with hardened runtime](https://developer.apple.com/forums/thread/679044); [Eclectic Light: the hardened runtime](https://eclecticlight.co/2021/01/07/notarization-the-hardened-runtime/)
- pyke's builds are statically linked; with CUDA and TensorRT, the EP libraries are still separate DLLs — [ort v2.0.0-rc.10](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.10)

**First-load cost and size**
- Core ML compiles on first load. Apple's SD notes that `.mlmodelc` caches the compiled asset, ORT exposes `ModelCacheDirectory`, and ort's Compiler API persists EP-compiled graphs — [apple/ml-stable-diffusion](https://github.com/apple/ml-stable-diffusion); [ORT CoreML EP docs](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html); [ort v2.0.0-rc.10](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.10)
- MIGraphX has a "multi-minute first-run model compile" that darktable surfaces to users — [darktable backend_onnx.c](https://github.com/darktable-org/darktable/blob/master/src/ai/backend_onnx.c)
- Size indicators (compressed static archives): ORT+CoreML 8.9 MB (macOS arm64), ORT+DirectML 30.3 MB (Windows x64). CUDA builds are 48–67 MB before any CUDA libraries — [dist.tsv](https://github.com/pykeio/ort/blob/main/ort-sys/build/download/dist.tsv)
- tract's NNEF-only runtime deliberately excludes protobuf and ONNX loaders to stay small — [sonos/tract README](https://github.com/sonos/tract)

### Inferences
- The on-disk app cost for a Mac build is probably about 15–30 MB for ORT+CoreML statically linked, against a few MB for tract with NNEF only. Both are estimates to measure. Models dominate either way: LaMa ONNX is about 93 MB (opencv variant), SAM-class encoders are tens to hundreds of MB, and diffusion is GBs. This supports the on-demand model design.
- First-use latency (CoreML compile/specialisation, ANE planning) needs its own UX state and a persistent cache directory under Luxforge's managed storage. The cache must be keyed by model hash, runtime version and OS build, because Core ML results differ across macOS versions (#32569).

### Gaps
- No measured first-load compile times for SAM or LaMa under ORT CoreML on M4, and no measured final binary sizes, were found.

## What should Luxforge measure, and what is the provisional recommendation?

### Takeaway
No single runtime is proven for all three workloads on the M4. Measure **ort (CPU and CoreML EP) against tract (CPU and Metal)** on the actual models, behind one small inference-port trait, with exact-output checks. Prefer tract if it meets the hover and encoder budgets, because it keeps the toolchain and licence rules and builds from source. Adopt ort+CoreML (static, hash-pinned binary) if tract misses them. Treat direct Core ML via objc2 as a later Mac-only accelerator for ANE-friendly encoders, not the portable baseline. Defer diffusion; if it is ever needed, evaluate stable-diffusion.cpp or Core ML separately.

### Cited Findings
- ort can front tract with one line (`ort::set_api(ort_tract::api())`), which makes an A/B harness cheap — [ort v2.0.0-rc.10](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.10)
- ORT CoreML exposes `ProfileComputePlan` to log per-op hardware placement. tract's `dump --profile` reports Metal accelerator time per node — [ORT CoreML EP docs](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html); [tract CHANGELOG](https://github.com/sonos/tract/blob/main/CHANGELOG.md)
- darktable's working pattern:
  - provider fallback ladder;
  - graph optimisation per model (SAM2 decoder needs BASIC);
  - persistent CoreML cache;
  - AI off by default.

  — [darktable dev-doc/AI.md](https://github.com/darktable-org/darktable/blob/master/dev-doc/AI.md)
- Luxforge's performance rules require photo-sized inputs and recorded measurements (AGENTS.md pillar 3). Its memory notes say M4 timings are skewed by other sessions, so runs must be back to back and reversed (project memory `shared-host-timing.md`).

### Inferences
**Measurement matrix**, run on the M4 first and repeated on Windows and Linux for portability only.

*Models* (fp32 ONNX masters, SHA-256 pinned):
- one SAM-family encoder and decoder pair from each of:
  - MobileSAM (Apache-2.0);
  - SAM 2.1-tiny;
  - a RepViT- or EfficientViT-SAM class model;
  - EdgeSAM only after a licence check;
- LaMa `Carve lama_fp32` at 512² and `opencv inpainting_lama 2025jan`;
- MI-GAN 512 as a lighter inpainter;
- Depth Anything v2 small;
- one sky/person/salient segmenter (e.g. SegFormer or a BiRefNet/U²-Net class model).

*Runtimes and settings:*
- ORT CPU;
- ORT CoreML in MLProgram mode with each of `ALL`, `CPUAndGPU` and `CPUAndNeuralEngine`, in fp32 and fp16, with static shapes on and off and the cache on;
- ORT WebGPU (Mac, experimental);
- tract CPU, tract Metal (`gpu-or-cpu`), and tract with NNEF-converted models;
- optionally candle Metal for SAM, and a Core ML `.mlpackage` via objc2 for one encoder, as the ceiling.

*Metrics:*
- cold load, and warm load with the compile cache;
- encoder p50/p95 at 1024²;
- decoder p50/p95/p99 at hover rate while the editor renders, with inference on the module lane and a synchronous hand-off;
- LaMa at 512, 1024 and 2048 (tiled or native);
- peak RSS / footprint per model;
- per-op placement (CoreML partitions, number of CPU fallbacks, DFT nodes);
- exact-output deltas against a PyTorch reference (max abs error, mask IoU, inpaint PSNR/LPIPS on a fixed set);
- determinism across runs and EPs;
- linked binary size delta;
- clean build time with and without a C++ toolchain;
- codesign/notarize check of the bundle.

**Decision rules to propose to the owner** (proposal, not a decision):
1. If tract meets the decoder budget (under about 10 ms p95 on CPU) and gets the encoder under about 300 ms on Metal, with exact outputs, choose tract.
2. If ORT CoreML is several times faster on encoders or LaMa, choose ort+CoreML. Pin the pyke archive hash plus attestation in `xtask`, link statically, and add per-OS provider fallbacks: DirectML or WebGPU on Windows, CPU with optional user CUDA on Linux.
3. LaMa on any Core ML path must keep fp32 (or bf16) FFC. Check whether DFT nodes force CPU partitions, and consider an FFT-free export or GPU-only placement.

The 300 ms threshold is a UX guess, not a spec. The owner should set the actual budgets.

**Portability:** Windows (DirectML is in maintenance, Windows ML needs 24H2) and Linux (no GPU EP in pyke's default Linux build except WebGPU/CUDA) mean the portable baseline is CPU. Tract's CPU path and ORT's CPU path should both be compared there too.

### Gaps
- The recommendation depends entirely on measurements that do not yet exist publicly for these models on M4. Every performance threshold above is a proposal pending owner-set budgets.
