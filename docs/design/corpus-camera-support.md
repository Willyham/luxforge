# Support for the retained sample corpus

## Outcome and scope

The owner authorized opening and continuously editing every recording mode for
which the retained [sample corpus](sample-corpus.md) has an original. The input
contract is the 341 CC0 files in `fixtures/sample-corpus.json`, including the
available Pixel and Galaxy DNG captures, alongside existing owner-camera
regressions. The 40 selected devices without licensed files are acquisition
gaps; this work does not guess profiles for them or add unsampled HEIC/ProRAW.

Admit exact observed camera identities and encoding selectors through the
validated embedded camera catalog. Implement shared format capabilities where
the originals require them. A successful native decoder experiment is not a
production support claim. Qualification includes source preservation, exact
unpacked samples, authoritative crop/calibration and finite as-shot and changed
white-balance development, followed by actual editor/API/history/reopen checks.

## Constraints

- Originals remain immutable, including any input that cannot be decoded.
- No catch-all camera, guessed geometry, copied sample-specific calibration or
  mode-name processing branch. Matrices, WB, black/white levels, crop and
  correction coefficients come from each source or attributed camera data.
- Preserve existing verified mosaic/development hashes. Newly admitted modes
  need a separately recorded LibRaw-only or independent JPEG XL unpack
  reference before baselines are accepted; RawSpeed routing additionally needs exact equality.
- Keep the accepted 512 MiB encoded, 128 MP sensor and 1.5 GiB RGB-buffer bounds.
  Catalog capacity may grow as a bounded metadata collection when actual
  distinct identities require it; this does not enlarge image allocations.
- Discovery uses the test-only uncatalogued native path. Production retains
  identify-time admission, bounded reads, required-correction validation and
  failure recovery. Preserve the last photo if Open fails.
- Functional camera support does not establish controlled photographic colour,
  every unsampled compression/size/capture option or native Windows/Linux GPU
  qualification. Record any genuinely unreadable original with its specific
  decoder limitation; do not turn a refusal into a support pass.

## Work and acceptance

1. Discover identity, geometry, encoding, CFA, calibration, crop and mandatory
   DNG operations for every candidate from verified R2/local originals. Record
   the native unpack hash independently of the new production admission.
2. Add evidence-backed profiles and the shared capabilities the samples need;
   cover changed format behavior with exact and malformed-input tests.
3. Run the production qualifier over the whole retained corpus, require frozen
   source/mosaic/metadata/development references and review every failure.
4. Exercise new processing families and recording layouts through background
   editor rendering, API edits, history and reopen. Keep coverage explicit for
   sources not given a complete editor journey.
5. Finish quick/rendered verification and proportionate photo-sized measurement
   after implementation. Update the camera catalog documentation, corpus
   expectations, feature status and user guide with demonstrated scope.

The grayscale behavior and format-only Sony memory exception are accepted
owner decisions. Missing licensed sources, unsampled modes and controlled
photographic qualification remain open.

## Implemented interpretation

The embedded catalog has 259 exact identities and 316 recording modes. The
341 retained files cover 258 identities and 315 modes; the owner-only Air 2S
fixture supplies the remaining profile/mode. Every retained source passes the
strict frozen M4 repeat, including all pre-existing references. The independent
unpack references for the 209 added files are committed in
[`corpus-camera-evidence.json`](../../fixtures/corpus-camera-evidence.json).
The Galaxy S22 JPEG XL buffer matches libjxl 0.11.2 exactly; the other added
buffers have separately recorded LibRaw-only references.

Shared additions include exact alternate stored frames and per-mode processing,
source-authoritative multi-strip/tile integer DNG geometry, source reference
matrices, pre-black vignette gain and pre-demosaic CFA GainMap. Source matrices,
calibration, neutral/white coordinates and AnalogBalance are read from the
original. ForwardMatrix is validated and recorded but does not select a DCP
rendering; illuminant interpolation and controlled colour matching remain open.
Missing GFX100RF/X-E5 matrices use each model's own attributed RawSpeed data.

Linear RGB sources retain three interleaved u16 samples per pixel and normalize
directly to float planes, without demosaic. Monochrome sources retain one
sample and develop identical R/G/B planes. The owner's grayscale choice disables
the global source white-balance group and picker, including keyboard entry;
colour-changing source white-balance and picker API actions refuse with
`validation` and no history mutation. An As shot reset is a no-op on a
monochrome source, so Reset Basic can still reset exposure/tone. Creative adjustments retain their existing independent semantics.

Sony A7 V Compressed HQ uses a staged upstream ARW6 backport on pristine LibRaw
0.22.2. The owner approved a separate 1 GiB working-space budget only for that
decoder. Other native decoders retain 512 MiB; encoded files remain capped at
512 MiB, sensors at 128 MP, retained u16 data at 512 MiB and each developed RGB
buffer at 1.5 GiB. The Sony decode/two-development native M4 trial takes 2.24 s
and peaks at 1,233,829,888 bytes RSS (about 1.15 GiB): total process memory
includes allocations outside decoder working space. This is one functional
trial, not a latency distribution or total-process ceiling.

JPEG XL uses pinned Rust jxl-oxide with a 512 MiB allocation tracker, decoding on
the global Rayon pool (its `rayon` feature, named explicitly in the adapter) and
adding no pool of its own. A source must be one 16-bit RGB DNG segment with exactly one
nonanimated frame, matching dimensions, orientation and no extra channels.
Header/stream feeds and output rows check cancellation. The library's final
`render_frame` call has no cancellation hook; cancellation waits for that
bounded decode interval. It is called from the existing source worker, never the
owner or UI thread.

## Performance review

- All production original reads, hashes and decodes remain behind the existing
  verified source worker/cache. Discovery is an ignored developer-only path.
- Retained source integers and sparse repair patches are immutable `Arc` data;
  clone tests prove sharing. At most 65,536 repair patches exist. New segmented
  DNG repairs exclude masked padding; existing strict-path references preserve
  their established full-frame interpretation.
- Direct RGB/monochrome development allocates only the bounded output planes;
  CFA development reads the retained mosaic through per-site tables, and only a
  development whose sensor stage rewrites the normalized values (DNG stage-one
  vignette or stage-two gain maps, or sparse repairs) uses its bounded single
  float mosaic; both share the native jobs. JPEG XL tracked scratch and Sony's
  upstream working-space estimate have their separate limits above. These are
  allocation limits, not an RSS bound.
- Neutral sampling evaluates a fixed 13×13 source patch, including the same
  source-stage gains as development; queries, validation and no-op checks
  allocate no frame. Exact tests cover signed headroom, channel order, grayscale,
  correction stages, malformed containers and unchanged-source hashes.
- No frame work moves to the owner or desktop. Existing preview, state/history,
  upload, development cache and queue policies remain in force. Controls derive
  monochrome availability from authoritative source metadata. No timer, poll,
  subscription or image cache is added.
- The complete strict repeat checks 341 files in 43 serial chunks in 670.067 s,
  including hashing and two developments; it claims no decoder speedup.
  Generic JPEG before/after measurements cover ordinary editor overhead only,
  not the cost or colour quality of the new RAW modes.

## Native editor evidence

Eleven background Metal editor journeys pass on the M4 Mac: Galaxy S22 JPEG
XL (7783), Galaxy S23 Ultra linear RGB (6758), Pixel 7 Pro (6168), Pixel 8 Pro
(7751), Sony A7 V HQ (8846), Leica SL stage-one repair/warp (1951), converted
X100S X-Trans DNG (2644), GFX100RF (8090), Pentax K-3 III Monochrome
(7081), Leica M Monochrom Typ 246 (1087) and Ricoh GR II sensor vignette
(1955). They correlate source hashes, metadata, rendered pixels, API edits,
history, geometry and exact reopened output. The monochrome journeys also
check disabled controls and unchanged pixels/history on five refused requests.
These are representative complete editor journeys; all 341 files have adapter
regression, not 341 complete native editor journeys.

Final quick verification passes. The rendered integration tier passes all 43
components, including complete headless checks and native background scenarios.
No timing target is inferred from these functional checks.

## Ordinary editor overhead measurement

Release `editor-performance` runs on the owner's M4 Pro use the same generated
24 MP source for pre-change commit `f99feaf56bec4b47f8dfff728498932d6f20c3e3`
and the implemented checkout, warm filesystem
cache and ten samples per recipe. The source and compared full/proxy output
hashes match exactly. These CPU core request-to-render diagnostics exclude
desktop scheduling, Metal upload/presentation and RAW decoding. Host load was
not recorded, so tail differences are provisional; no speedup or general
latency-budget verdict is claimed.

| Metric, p50 / p95 ms | Before, 24 MP | After, 24 MP | After, 60 MP |
| --- | ---: | ---: | ---: |
| One orientation render | 6.74 / 9.93 | 6.35 / 9.34 | 17.34 / 22.19 |
| Full Basic on crop stack | 60.78 / 62.02 | 61.04 / 63.87 | 137.62 / 144.48 |
| Fit proxy, full Basic | 36.20 / 37.70 | 35.84 / 41.61 | 37.00 / 38.99 |
| Display-bounded proxy construction | 11.49 / 15.51 | 11.57 / 12.00 | 18.80 / 19.87 |

Raw reports, source/build identities and the scoped comparison are retained in
ignored `artifacts/corpus-support/performance-*`. The admission-only catalog
validation rejects unimplemented monochrome/linear sensor-correction combinations
without changing generated data or image processing. No new image cache,
retained-frame allocation or worker is introduced for the ordinary JPEG path.
