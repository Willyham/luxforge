# Modern camera and drone support

## Scope

Expand the data-driven RAW path to a practical set of 100 widely used modern
camera/drone models across brands, including the three existing models. This is
a coverage selection, not a measured sales ranking. The owner selected broad
usage across brands rather than newest-first coverage and requested Luna
subagents. Maintain exact make/model identities and recording-mode evidence.

## Behavior and constraints

Profiles select bounded processing capabilities; no model-name processing
branches. Source-dependent calibration remains authoritative in each file.
LibRaw's camera list alone is not Lightwell qualification: its published list
assumes optional features which this build may not enable. Unknown modes,
unsupported mandatory corrections and incompatible metadata fail explicitly.
Do not invent full-sensor dimensions from advertised megapixels.

Use a selected subset of CC0 authentic files from raw.pixls.us, with URL,
SHA-256, license and observed decoder metadata recorded. Downloads and rendered
photos stay ignored, bounded and local. Do not mirror the whole corpus. Validate
source preservation, mosaic/float correctness, calibrated dimensions and crop,
then representative background editor/history/reopen workflows. Separate each
mode's demonstrated evidence from outstanding controlled color/scene coverage.

Default resource bounds remain in force until an evidence-backed RAW-specific
change is agreed. High-resolution models, container compression, missing crop
metadata, per-green black levels and DNG correction layouts may require shared
capabilities before a profile can be enabled. Never inflate the support count
with guessed entries or bypass required corrections to admit a model.

## Current evidence

The selection contains 100 distinct model identities. Of these, 84 models
(87 configured recording modes including the original modes) fit the current
resource bounds. The authentic adapter run covers one selected recording mode
per enabled model: 83 CC0 public sources and the existing owner Air 2S source.
Each qualification checks source hashes before/after, full mosaic retention,
usable calibration, and finite as-shot and perturbed-white-balance development. Per-source metadata and hashes
are in [the evidence manifest](../../fixtures/modern-camera-evidence.json);
[the selection](../../fixtures/modern-camera-selection.json) records source URLs,
licenses and the 16 pending models. The float hashes record this build's output;
they are not independent color-reference ground truth.

Pending models are Nikon D850, Z7, Z7 II, Z8 and Z9; Sony A1, A7R IV, A7R V
and A7CR; Canon R5 and R5 II; Fujifilm GFX100S, GFX100 II and GFX50S II; and
Leica Q2 and SL2. Their actual encoded or planar float sizes exceed the current
limits. The [resource ledger](modern-camera-resource-ledger.md) describes the
proposed RAW-only increase and live allocations. These models are not enabled.
A demonstrated mode does not qualify every recording setting or, for drones,
every camera module. The Mavic 3 Pro Cine fixture covers its 4032×3024 module.

The native M4 Pro editor passed 39 edit/history/reopen trials across ten
representative modern models and the three original owner fixtures. The checks
cover temperature/tint, individual channel WB, neutral picking, exposure,
rotation/crop/undo, historical previews and exact reopened displayed pixels.
Full verification also passed 19 general rendered scenarios and the independent
RAW numerical reference. The resource ledger records sampled process memory
and its limits. Controlled
color/detail, other recording modes, and native Windows/Linux package
qualification remain separate work.

## Reproducing adapter qualification

Download the selected public sources with the bounded
`probes/raw/collect_camera_metadata.py` tool using the raw.pixls.us repository
index and selected numeric sample IDs. It verifies the repository's CC0
license and SHA-256 and refuses to reuse an output directory. The separate
native probe can inspect larger files, but does not establish application
support. Keep images and generated results in an ignored evidence directory.

Create a qualifier manifest containing `{"samples": [{"id": "sample-id",
"path": "/absolute/source/path", "sha256": "expected-source-sha256"}]}` for
only the enabled selection entries. The owner fixture needs local access; it
is not downloadable from the public repository. Run:

```sh
cargo run --release --locked -p lightwell-raw --example qualify_profiles -- \
  /path/to/manifest.json /path/to/new-results.json
```

The qualifier runs sources sequentially, refuses an existing output file, and
returns failure if any source changes, metadata is unsupported, or development
fails. Its maximum source read is the application's 128 MiB bound. Public
fixture redistributions and source photographs are not committed.

OM-3 calibration comes from the exact model entry in pinned RawSpeed camera
data, with its attribution and data license in the RAW adapter's notices.
Unpacking with LibRaw's identity fallback is insufficient: the adapter rejects
missing or singular XYZ-to-camera calibration before publishing the source.

## Work and acceptance

Luna agents research disjoint model groups and prepare evidence-backed candidate
records. Root owns shared decoder/profile changes, selection integration and
verification. A profile can be marked enabled only with its required processing
implemented and authentic decode/development evidence. Track concrete blocked
modes and missing fixtures; distinguish implementation from broad image-quality
qualification.

The finished catalog has 100 distinct selected model identities, source-grounded
mode rules and capability choices, reproducible qualification evidence, current
support documentation, and passing quick/rendered/timing checks. Full verification
is required before claiming the broad support checkpoint. Existing three-camera
metadata, corrections, source preservation and history remain regressions.

## Open decisions

- Exact model set follows the agreed broadly used modern mix and available
  decoder evidence; selection must not hide popular high-resolution models simply
  because they exceed the current per-frame allocation bound.
- Any increased RAW-specific allocation budget requires an explicit proposal and
  owner decision with a measured allocation ledger. JPEG limits stay independent.

## Performance checklist

- Source reads, hashes, unpacking and development stay on the existing verified
  source worker. Cache hits, retained mosaics and source exposure/WB reuse keep
  the existing source-signature path.
- Sparse sensor repairs add at most 65,536 index/value pairs, shared with the
  source. Their sorted coordinate list is also bounded. List membership uses
  binary search; constant-marker scans are linear in the bounded sensor size.
  No new full RGB copy is introduced. Ordered DNG warps reuse one active-area
  float plane after native demosaic scratch is released, as in the original
  Air 2S path. Existing 128 MiB encoded, 64 MP sensor and 512 MiB planar limits
  still apply; aggregate editor RSS is measured separately.
- The neutral picker checks at most 65,536 sparse replacements and samples its
  fixed patch with binary-search lookup. It does not develop or rasterize a
  frame. No decoding, color conversion or hashing moves onto the catalog owner.
- The only desktop admission change is the set of RAW filename extensions in
  Open. Preview, history, upload and cached source messages retain their existing
  triggers. No timers, polls or subscriptions are added.
- Native and full editor measurements are scoped in the resource ledger and
  verification evidence. These changes make no speedup or process-memory-bound
  claim; loaded-host timings are not a baseline.
- Exact tests compare stage-ordered gains/warps to separate reference stages,
  sparse correction to an undamaged mosaic, and physical green-site calibration
  to an independently calculated result. Existing original-source hashes,
  independent Air 2S numerical samples and editor history/reopen checks remain
  required. Crop/orientation continue sharing the developed float planes.
