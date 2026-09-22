# Modern camera resource ledger

Status: design and measurement ledger for the proposed modern-camera expansion.
This document does not change an admission, decoder, frame, or process limit.
The 128 MP, 1.5 GiB float, and 512 MiB encoded figures below are planning
proposals supplied by the expansion work; they are not a release claim.

## Scope and accounting rules

The current application limits remain those in
[architecture](architecture.md): 128 MiB encoded source, 64 MP, 16384 pixels
per side, 512 MiB per evaluated frame, and 64 MiB aggregate float scratch.
The standalone raw probe has separate diagnostic checks and must not be used
as evidence that the editor admits a camera mode. A decoder limit is not a
process or GPU limit.

The table uses binary MiB (bytes / 2^20), with one full pixel per sensor
sample. A u16 mosaic is `2 bytes/pixel`; planar RGB float is `3 * 4`
bytes/pixel; the terminal RGBA display buffer is 8-bit `4 bytes/pixel`.

| Work item | 24 MP | 40 MP | 128 MP | Liveness implication |
| --- | ---: | ---: | ---: | --- |
| Encoded source at the proposed 512 MiB ceiling | 512 MiB | 512 MiB | 512 MiB | The current application ceiling is 128 MiB; this row is a proposal only. |
| Immutable u16 mosaic | 45.8 MiB | 76.3 MiB | 244.1 MiB | Retained by the source while WB or a source redevelopment is possible. |
| One planar RGB float frame | 274.7 MiB | 457.8 MiB | 1,464.8 MiB | A 128 MP frame is below a 1.5 GiB single-frame proposal, but above the current 512 MiB frame limit. |
| One terminal RGBA8 display/upload frame | 91.6 MiB | 152.6 MiB | 488.3 MiB | The terminal buffer is 8-bit; GPU/shared-memory copies still need accounting. |
| Two planar RGB float frames | 549.3 MiB | 915.5 MiB | 2,929.7 MiB | Active plus pending preview, or input plus output at a resample boundary. |
| Mosaic plus two planar RGB frames | 595.1 MiB | 991.8 MiB | 3,173.8 MiB | Does not include native decoder scratch, allocator retention, or GPU/shared-memory copies. |

The 1.5 GiB proposal therefore covers one 128 MP planar RGB frame only. It
does not cover the source mosaic, a second visible/pending frame, native
demosaicer scratch, or a display upload. A 512 MiB encoded proposal is also
independent of the current 128 MiB admission limit and cannot be adopted by
raising one constant: source bytes, decompressed mosaic, decoder allocations,
float planes, and shared-memory display resources overlap in time.

## Phase liveness

The source worker reads encoded bytes and keeps them alive through signature
validation and decode. After the immutable u16 mosaic is established, the
encoded buffer should be released; the in-memory source cache retains the
mosaic and a developed float result only while references require them. The
serialized recipe/source metadata does not contain the mosaic. A white-balance
change redevelops from the retained mosaic. If the old preview remains visible
while a new preview is being computed, both float results are live. The
documented preview queue is one active job plus one replaceable pending job, so
a pending result must be accounted for even when it will soon be superseded.

At a pixel-stage geometry resample boundary, the architecture permits two full
frames: the prior segment's input and the next segment's output. Ordinary crop
and orientation views share the developed float planes. The display path then
samples into a terminal RGBA buffer. On Apple unified memory, a GPU
texture is still a physical memory cost even when it is not reported as a
separate process allocation; a capture or readback can add another live copy.
The source and float memory gate must therefore wait for actual references,
not just queue state, before admitting another large source.

Native RCD and Markesteijn workspaces are additional to these formulas. The
standalone probe recorded historical peak RSS of approximately 425 MiB for
24 MP Z6 RCD and 707 MiB for 40.2 MP X100VI Markesteijn in
[the probe notes](../../probes/raw/README.md) (those are decoder-stage trials,
not current editor limits). Scratch generally scales with implementation and
working tiles rather than only final pixel count, so a 128 MP value cannot be
extrapolated as a release guarantee. The adapter sets LibRaw's
`max_raw_memory_mb = 512`; that is a LibRaw internal allocation ceiling.
It can reject a large source before Lightwell's own u16/float accounting is
reached, and it does not make a 512 MiB process budget true.

## Authentic baseline measurements

The available current Sony samples were run through the repository's native
`qualify_profiles` develop qualifier. Each sample ran in a fresh process with
source preservation, decode, as-shot development, and perturbed-WB
development. The command shape was:

```text
/usr/bin/time -l target/release/examples/qualify_profiles \
  /private/tmp/sony-ledger-manifest-2418.json /private/tmp/sony2418-qualifier-escalated.json
/usr/bin/time -l target/release/examples/qualify_profiles \
  /private/tmp/sony-ledger-manifest-1313.json /private/tmp/sony1313-qualifier-escalated.json
```

| Sample | Authentic file SHA-256 | Raw dimensions | Pixels | Result | `/usr/bin/time -l` |
| --- | --- | ---: | ---: | --- | --- |
| Sony ILCE-7M3, id 2418 | `ece80551abf64949dbe826985a80f1d9b265401ee4ea6b989e1829549616fdcd` | 6048 × 4024 | 24.34 MP | native develop passed, 1.88 s | 593,395,712 bytes (565.9 MiB) |
| Sony ILCE-7RM2, id 1313 | `fdf3e8deea4ca31c6ee905fe5226efab398ce630ec96e9013850b488ce0e6289` | 8000 × 5320 | 42.56 MP | native develop passed, 3.29 s | 1,030,914,048 bytes (983.1 MiB) |

These are adapter functional measurements, not controlled colour or complete
editor/UI qualification. The `xtask raw-editor` harness validates mode identities and DNG requirements
against the camera catalog. The
figures include native decode/develop and both WB passes, but not an editor
window, preview replacement, display upload, or reopen path.

## Background editor measurements

The M4 Pro native Metal editor passed three edit/history/reopen trials for ten
representative modern models and the three owner originals (39 trials, 78
background launches). The first process performs exposure, red/blue WB,
temperature/tint, neutral picking, rotation/crop/undo, historical preview and
zoom changes. The second process reopens the committed state and matches the
captured photo pixels. Sources remained unchanged.

The following are sampled first-process peak RSS, with roughly 50 ms sampling,
release binary `82b2f010b105760f8fe156bf3bd74b42e9c9c7069b43ac168af9dff3d559dd03`.
Each row has three trials; filesystem cache was not purged. Captures and GPU
resources contribute to process RSS, and GPU memory is not isolated. These are
observations rather than a hard process ceiling or a timing baseline.

| Source | Median peak MiB | Maximum peak MiB |
| --- | ---: | ---: |
| OM System OM-3 | 1268.1 | 1288.2 |
| Sony A7 III | 1426.6 | 1438.5 |
| Canon EOS 5D IV Dual Pixel RAW (primary) | 1558.8 | 1560.9 |
| Fujifilm X-T5 | 1977.7 | 1983.0 |
| DJI Mavic Pro FC220 | 934.2 | 934.9 |
| Owner Fujifilm X100VI | 1945.3 | 2010.2 |

The local correlated state, events and captures are under
`artifacts/modern-camera-full-final/raw-editor/run/`. These photo-sized results
show why the decoder's per-buffer bound cannot be treated as an editor process
budget; even the current 40 MP path reaches approximately 2 GiB in this journey.

The same full verification ran the standard 24/60 MP JPEG performance checks.
The 24 MP slider-to-presented-frame p95 was 83.21 ms (30 samples), settled
histogram p95 106.3 ms (two samples), and scratch high-water mark 13.46 MiB.
Sampled peak RSS medians were 503.4 MiB at 24 MP and 981.2 MiB at 60 MP (five
samples each). Idle CPU was 1.68% of one core over 30 seconds, missing the
provisional <1% target. These default sample counts are functional observations,
not a performance baseline or a RAW memory guarantee. The full summary is
`artifacts/modern-camera-full-final/summary.md`.

## Modes held at the current limits

These are measured encoded sizes and native sensor dimensions from the selected
CC0 files. Planar bytes are `sensor pixels × 12`; none has been admitted by the
application. A valid matrix and successful standalone unpack do not bypass its
resource limits.

| Selected model | Sensor MP | Encoded MiB | One planar RGB MiB |
| --- | ---: | ---: | ---: |
| Nikon D850 | 45.75 | 93.6 | 523.6 |
| Nikon Z 7 | 45.75 | 85.0 | 523.6 |
| Nikon Z 8 | 45.71 | 58.0 | 523.1 |
| Nikon Z 9 | 45.71 | 54.9 | 523.1 |
| Nikon Z7 II | 45.75 | 81.3 | 523.6 |
| Sony ILCE-7RM4 (A7R IV) | 61.21 | 117.5 | 700.5 |
| Sony ILCE-7RM5 (A7R V) | 61.21 | 128.5 | 700.5 |
| Sony ILCE-1 (A1) | 50.16 | 108.0 | 574.0 |
| Sony ILCE-7CR (A7C R) | 61.21 | 126.4 | 700.5 |
| Canon EOS R5 | 46.65 | 38.8 | 533.9 |
| Canon EOS R5 Mark II | 47.91 | 51.9 | 548.3 |
| Fujifilm GFX100 II | 103.37 | 198.4 | 1182.9 |
| Fujifilm GFX100S | 103.37 | 200.4 | 1182.9 |
| Fujifilm GFX50S II | 52.61 | 103.5 | 602.1 |
| Q2 | 47.44 | 83.2 | 543.0 |
| SL2 | 47.44 | 84.8 | 543.0 |

## Colour and serialization constraints

The public as-shot RGB camera matrix is metadata supplied by the decoder and
is applied after camera-space WB; it is not a brand-specific allocation or
memory shortcut. It may be used for another brand only when that camera's
profile supplies and validates its own matrix, white level, CFA, black sites,
and mode metadata. A matrix from a different camera must never be substituted
because its dimensions happen to match.

In-memory source state retains one shared immutable mosaic and explicit
profile metadata; serialized recipes retain metadata and source identity. Arc sharing and single-flight requests avoid duplicate
copies, but serialized recipes do not prove that old/new previews or GPU
uploads have been released. Memory admission must be based on live buffer
references and measured native/display phases.

## Practical recommendation

Keep the current 64 MP, 128 MiB encoded, 512 MiB frame, and 64 MiB scratch
limits until full editor measurements exist. For a first modern-camera subset,
prefer 24–40 MP single-frame Bayer modes that fit one planar RGB frame under
512 MiB, release native scratch before display conversion, and enforce one
active plus one pending preview with byte-accounted eviction. Measure complete 40 MP editor workflows before making process-memory claims;
128 MP modes remain resource-gated experiments. A 128 MP release path
needs tiling or another bounded representation plus measured RCD/Markesteijn,
display/GPU, and WB replacement liveness; accepting it by increasing the
encoded or float constant alone would undercount the simultaneous peak.
