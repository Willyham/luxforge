# Extended camera sample corpus

## Scope

The owner selected Cloudflare R2 for an independently retained sample corpus:
approximately 250 dedicated RAW cameras, including older bodies still in use,
and the last five flagship generations of the main iPhone, Google Pixel and
Samsung Galaxy S families. Add more devices on demand. This is a selection
target, not a measured 99% active-device coverage claim.

The reviewed [targets](../../fixtures/sample-corpus-targets.json), dated
2026-10-02, contain 250 camera identities and 46 phone variants: the released
iPhone 14–18 families (including iPhone Air), Pixel 7–11, and Galaxy S22–S26.
The selection retains every existing camera profile and adds common older
bodies and compacts; it is curated rather than a global popularity ranking.

The [manifest](../../fixtures/sample-corpus.json) pins 341 CC0 files totalling
13,080,184,249 bytes (12.18 GiB), all with qualified source, unpacked-integer,
metadata and development references. Licensed files cover 249 camera
targets and seven phone variants. The owner-only Air 2S original is excluded
from the mirror; 39 selected phones, including every selected iPhone, lack a
licensed upstream sample. These are acquisition gaps, not passing tests.

All retained files are admitted by the catalog and pass strict M4 adapter
regression ([camera support](corpus-camera-support.md)). This covers the sampled
phone RGB/JPEG XL DNG layouts, not unsampled HEIC/ProRAW or every phone setting.
Missing licensed files remain explicit coverage gaps.

## Storage and provenance

Use a private `luxforge-samples` R2 Standard bucket. Store immutable originals
under a content-addressed SHA-256 key and pin source identity, source URL,
license, byte count and qualification expectations in a checked-in manifest.
Never publish owner originals licensed for local testing only. A later
manifest revision can add samples without replacing old source objects.
After all referenced objects are verified, `publish-index` also archives the
manifest under `manifests/<manifest-sha256>.json`, so the bucket retains the
selection and provenance independently of the upstream site and local checkout.

Local credentials live in an ignored mode-0600 `.env`, with a credential-free
`.env.example`. The publishing credential is scoped to this bucket; CI uses
a separate read-only credential. Nothing in an ordinary pull-request test
needs cloud credentials or downloads photographs.
The publisher expires on 2026-11-01 and the reader on 2027-10-02. Both tokens
allow objects in this bucket only, without bucket administration. Rotate the
local publisher when adding more data, and replace the GitHub reader secrets
before expiry. The bucket has no public access. The qualified manifest is archived
as `manifests/0ed8c42f9fce8f67347a2e4368b65d278b8d2025c1e4085e436844458256f3ae.json`.

## Local and CI operation

Provide explicit commands to inspect coverage, mirror selected upstream files,
upload verified objects, sync selected groups, run authentic regression tests
and remove only tool-owned downloaded cache files. Hash every source before
use, after processing and on download. A corrupt cache is an error and a
partial download never becomes a complete cached object. Network fetches are
streamed and bounded; processing is serial within each small chunk.

Group by camera manufacturer and phones, with deterministic numeric shards.
The runner downloads one chunk, tests it, writes a durable report, and can
remove its own downloaded cache afterwards even after a test failure. Cleanup
must preserve pre-existing cache files, owner originals and reports.
Defaults are eight inputs and at most 1 GiB of encoded photographs per chunk,
with serial processing and one transfer worker (up to four explicitly).
Each input is capped at 512 MiB. Use separate cache directories for concurrent
invocations. `clean` explicitly removes selected, hash-verified tool-cache
files, including files preserved by an earlier invocation's `--cleanup`.

Qualified samples compare recorded mosaic and development evidence; candidate
samples report unsupported or unqualified rather than count as passed. Newly
accepted baselines are an explicit reviewed manifest change, never a test's
automatic response to changed output.
Portable checks freeze the integer mosaic, recording mode, sensor/crop geometry,
correction metadata and colour matrices, plus output dimensions, finiteness and
min/max ranges for as-shot and perturbed white balance. Metadata floats allow
`1e-6` absolute/relative error; output ranges allow `1e-5`. On the reference M4,
`--strict-development` additionally compares both development buffers by hash.
Monochrome sources must reject changed white balance and repeat the same
unity-gain grayscale development for their second reference. The field
`mosaic_sha256` hashes canonical little-endian u16 samples: one channel for CFA
or monochrome, interleaved RGB for linear sources. Candidate outcomes are `unqualified-decoded` or
`unqualified-refused`; neither is a qualification pass. `--require-complete`
fails while devices lack files or selected files remain candidates.

A weekly, manually dispatchable Linux CI job runs authentic adapter regression
checks with bounded concurrency, disk use and a timeout. It retains small
reports, not original photographs. It is headless functional evidence, not
native GPU, photographic colour or M4 performance qualification. The complete strict M4 repeat checks all 341 files in 43 chunks in 670.067
seconds, with zero integrity, metadata or development-reference failures. This
is elapsed offline regression time including source hashing and development,
not an isolated decoder benchmark or a prediction of hosted Linux cost.
Authenticated R2 readback verifies the retained originals independently of the
upstream site; mirror integrity does not establish photographic colour quality.

The [workflow](../../.github/workflows/sample-corpus.yml) schedules Mondays at
04:23 UTC, with four deterministic shards, at most two simultaneous jobs and a
30-minute timeout per job. It downloads each source once across the four shards,
uses two transfer workers per job, deletes downloaded test files, caches only
build dependencies and retains reports for 14 days. The full current run and qualified-only selection each read
12.18 GiB per week, because every retained source is now qualified. AWS CLI,
Rust and Actions are pinned. GitHub has the bucket-only reader secrets and
account variable. The scheduled workflow is on the default branch. Its recorded hosted run
37304142552 at `6a53f7d3` fails all four qualifier shards (12, 8, 7 and 8
failures); no successful complete hosted run is recorded, so current-main
Linux timing and compatibility evidence remain outstanding. No pull-request event receives R2 access.

## Acceptance

- The R2 mirror retains byte-identical licensed originals independently of the
  upstream repository, and authenticated reads are exercised.
- The manifest distinguishes selected devices, available samples, qualified
  samples, unsupported encodings and missing phone/model coverage.
- Extended tests refuse missing required inputs and changed frozen outputs.
- Selective sync, offline testing, deterministic shards and safe cleanup are
  exercised without changing originals.
- Ordinary checks stay offline; scheduled CI has read-only access and bounded
  resource use, with measured scope and explicit remaining gaps.
