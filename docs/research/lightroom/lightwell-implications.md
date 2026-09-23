# Engineering implications and research gaps

[Knowledge base index](README.md)

**Status: proposals and unexecuted experiments.** This chapter translates the evidence into engineering questions. It does not accept a new product decision, implement an effect, or expand S0/M1. Existing [module/API design](../../design/modules-and-api.md), [history specification](../../specs/edit-history.md), [source recovery](../../specs/source-recovery.md) and [product decisions](../../decisions.md) remain authoritative.

## Architectural lessons worth evaluating

| Evidence | Proposed consequence for Lightwell | Boundary |
| --- | --- | --- |
| [Recipes and auxiliary data](storage-and-history.md) differ from caches | Classify storage as original, durable recipe, durable auxiliary asset or disposable cache; recovery must preserve the first three | Original bytes remain protected under Lightwell's existing policy |
| A process version affects interpretation | Include enough algorithm/profile identity in recipe state to detect incompatibility | v0 format protection, not a public compatibility framework |
| [SDK tracking](sdk-and-interoperability.md) separates interactive redraw from history commit | Model drag drafts separately from committed commands; cancel stale work and publish the final state once | Existing core history/transaction ownership remains the authority |
| [Preview paths](previews-and-performance.md) optimize different jobs | Give thumbnail, Fit, 100% and export work explicit quality/resolution purposes; key reuse on relevant source, recipe and color state | Measure before building many caches or complex schedulers |
| [Scale-sensitive detail](detail-and-local-contrast.md) and AI support differ at proxy size | Inspect noise/sharpening at appropriate resolution; validate approximate previews against final output | Never present a proxy measurement as full-resolution quality evidence |
| Masks and AI have dependencies | Track auxiliary assets and invalidation; expose pending/recompute/failure states to both UI and agents | Future work; no mask/AI runtime added to M1 |
| Rendered pixels and editable state are different artifacts | Export should capture an explicit committed revision and immutable job inputs | Already consistent with accepted history/export intent |
| XMP syntax does not specify the processor | Separate metadata import from recipe interpretation and rendered equivalence | The [preset importer](../../design/presets.md#mapping) transfers values and reports the rest; it claims no rendered equivalence |

All recommendations above are **P**, not claims about hidden Lightroom internals. Lightroom demonstrates useful behaviors; it is not proof that Lightwell needs the same architecture.

## Budget full-resolution work before optimizing kernels

**C.** A hypothetical 60-million-pixel RGBA image with four 32-bit floating-point channels needs `60,000,000 × 4 × 4 = 960,000,000` bytes (about **916 MiB**) for one uncompressed buffer. Three such buffers approach 2.7 GiB before source data, pyramids, masks, GPU staging or model tensors. This is a sizing example, not a measurement or assertion of Lightroom's buffer format.

**P.** Bound jobs and live buffers; prefer reuse and cancellation over letting obsolete edits finish. A tiled/local operator may require overlap around each tile; a global statistic or AI stage may require a broader dependency. Do not assume every effect is a pointwise shader. Profile the actual chosen implementation before proposing binary module splits or a generalized render graph.

## Experiments that could resolve the remaining unknowns

These are a research backlog, **not scheduled implementation tasks**. Use disposable catalogs and copied fixtures. Record results as observations of the tested build, not universal algorithm claims.

| Question | Controlled experiment | What would count as evidence |
| --- | --- | --- |
| Do metadata operations change source bytes? | Hash copies of JPEG/DNG/RAW before editing, saving metadata and changing capture time; inventory companions | Separate pixel-array differences, file-byte differences and new/modified sidecars |
| Does action order change identical final settings? | Apply A then B and B then A; compare all exported settings, process/profile state and lossless outputs | Equal state/output supports order independence for that case only; mismatches require checking hidden state and AI invalidation |
| How adaptive are tone controls? | Place identical ramps/patches in images with different surroundings; compare rendered patch values | Context-dependent output rules out a purely fixed per-channel curve, without revealing the full algorithm |
| Which scales do detail tools affect? | Use frequency sweeps, textured patches, isolated edges and controlled noise at several image sizes | Measure contrast by spatial frequency, halos and noise amplification; repeat at 100% and Fit |
| What does interactive quality trade off? | Capture slider drag and settled output with correlated timestamps/state; compare with export | Distinguish event latency, approximate redraw, settled redraw and commit time |
| What causes loading latency? | Separate cold process/cache, warm cache, embedded preview, standard preview, Smart Preview and original availability | Timings include decode, first visible image and final-quality readiness, with memory and cache state |
| Does GPU choice alter correctness? | Repeat lossless exports under supported CPU/GPU settings with fixed profiles and settings | Numeric/image comparisons use explicit tolerances and inspect edge, clipping and color differences |
| Can edits recover without caches? | Back up catalog, auxiliary data, originals and sidecars; restore a copy after removing only disposable previews | Restored current states, masks and virtual copies plus matching outputs; a thumbnail alone is insufficient |
| What are RAW/AI constraints? | Test recorded Nikon Z6 Bayer and Fujifilm X100VI X-Trans modes and representative JPEG/DNG inputs | Tool eligibility, decode correctness, memory, source preservation and native M4 timings are separately recorded |
| What fails offline or with missing data? | Test unavailable originals, missing auxiliary edit files and offline AI actions on copies | Clear recoverable status and preservation of settings; record which operations actually need network access |

Use Lightwell's [rendered-evidence requirements](../../engineering/development.md): provenance and state must accompany UI captures. Native M4 timing is hardware evidence; VM functional checks are not native GPU benchmarks. Existing open-source library benchmarking should precede any custom RAW decoder proposal.

## Research leads before any attempt at matching Adobe output

- Obtain an official SDK package and compare its schema/signatures with the mirrored references.
- Read the follow-up Fast Local Laplacian literature for approximation and scheduling ideas; do not assume adoption in current Lightroom.
- Build a licensed fixture set with ramps, edges, textures, color targets and supported camera recording modes.
- Decide whether the goal is a useful perceptual control or compatibility with an Adobe rendering. The latter requires a separately scoped calibration effort and may remain unattainable from public evidence.

Exact tone curves, local-contrast kernels, Dehaze estimation, current model weights, processing order and internal color representations remain open. They are not suitable acceptance criteria until specified independently for Lightwell or established by reproducible evidence.
