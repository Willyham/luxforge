# Lens and perspective correction

Status: functional implementation verified on the owner's M4 Mac. The [task plan](../../tasks/lens-and-perspective.json) carries the work. The owner decided three product questions on 2026-09-30 and four on 2026-10-01 about the Lens section and first open ([decisions](#decisions)); the plan runs on two further recorded defaults, each a proposal the owner can revise. The modules and shared geometry pass quick checks and sequential native rendered integration (37 scenarios); the section's states and first open pass their targeted tests and the native `lens-perspective` scenario. First open on the authentic Air 2S DNG and Z 6 NEF, quiet-host performance measurement, the full tier and photographic qualification remain incomplete and are tracked below.

The planned sections are drawn on the [lens and perspective board](develop-workspace/lens-and-perspective.png) (a Perspective drag over a selected profile) and in their states on the [planned module panels](develop-workspace/planned-module-panels.png), with renders of [Lens correction](develop-workspace/modules/lens-correction.png) and [Perspective](develop-workspace/modules/perspective.png). Camera, lens and profile names on them are illustrative, not claims about the pinned database. The boards are design references, not evidence of anything built.

## Scope

| Choice | Rationale |
| --- | --- |
| Optional profile distortion correction for rectilinear lenses, plus manual horizontal and vertical perspective, on JPEG and supported RAW | A useful geometry tool with a small, testable mathematical surface. Perspective works without a profile. |
| Lensfun v0.3.4 data, converted offline into a bundled index; a Rust evaluator for its `poly3`, `poly5` and `ptlens` distortion models | Community calibration without a native library, a second raster engine or a network prerequisite. The subset is qualified against the pinned upstream source; this is not full Lensfun support. |
| A new RAW whose distortion is known to be uncorrected gets its detected profile once, as an ordinary undoable entry after its Original; a JPEG's detected profile is offered with an Apply that states its assumption; a search lists only compatible profiles | A RAW mosaic is never dewarped in camera, so its detected profile is the expected correction, and Undo or a reset removes it. Camera JPEGs may already be corrected, and matching metadata is not evidence of an uncorrected lens, so a JPEG is never corrected without a person or client choosing it. |
| Manual two-axis keystone, neutral at zero; straighten and rotation stay in the existing tools | No automatic Upright, line detection, guided quadrilaterals, arbitrary homographies, fisheye conversion, anamorphic correction or free scaling. |
| A fixed canvas: each warp keeps its input dimensions and zooms just enough to cover them; combined zoom at most 4× | Opaque frames and the existing no-empty-corners crop contract stay valid. Edge field of view is sacrificed deliberately; the zoom is reported in UI and API. |
| Distortion only | Lateral CA, optical vignetting, defringing, manual coefficients, profile authoring and external profile imports are later scope. Required embedded DNG corrections stay mandatory and separate. |

## Decisions

Decided by the owner on 2026-09-30:

- **RAW mosaics without a luminance warp are `known-unapplied`.** A NEF, a RAF and any DNG whose applied opcodes include no distortion-role warp (including the Air 2S DNG, whose WarpRectilinear corrects lateral CA only) report distortion `known-unapplied`, provenance `raw-mosaic`, and a profile needs no acknowledgement. A JPEG stays `unknown` and needs `assume-uncorrected`.
- **Perspective is not presettable.** Its patch action declares `preset: false`, which `ModuleRegistry::patch_action` honours. Lightroom's `Perspective*` and `PerspectiveUpright` settings stay unsupported with the reason "different perspective model".
- **Strong minification is refused.** A combined map whose local minification exceeds 1.8× is refused with `unsupported-input` ("strong minification: bilinear would alias"). The limit is documented, not hidden.

Decided by the owner on 2026-10-01:

- **No list of lenses.** The section never lists profiles except as search results, and never lists an incompatible one. It shows the applied profile with Change, the detected profile with Apply, or a warning with a search and a Report missing lens link.
- **Auto-apply on first open, as an entry.** When an import creates an asset whose source's distortion is known to be uncorrected (a RAW mosaic or a DNG without a distortion warp) and an eligible profile is detected, the core commits `select-lens-profile` as an ordinary history entry by the `system` actor right after the Original. Undo removes it and the Original stays uncorrected. A reset keeps a neutral layer, so it is never applied again; importing the same file again or reopening never applies it. A JPEG, whose in-camera correction is unknown, is not corrected: its detected profile is offered with Apply, which sends `assume-uncorrected` and says so. This lives in the core, so API and agent clients get the same behaviour.
- **Curated product names.** A small core table maps DJI camera codes (normalized EXIF make and model) to product names, so a fixed-lens drone reads "DJI Air 2S (FC3411)". It is used for display and the issue report, never for matching.
- **Report missing lens.** When no profile matches, or a search finds no compatible profile, the section offers a link to a prefilled GitHub issue built in the core.

Recorded defaults the plan runs on (proposals the owner can revise):

- **The index is a separate resource file.** The CC BY-SA 3.0-derived index ships as a bundled data file beside the binary, not compiled in, and is parsed once per process off the owner and UI threads. The manual license review stays deferred.
- **No X100VI alias.** The X100VI stays `camera-not-in-database` although the X100V record exists.

## Upstream baseline

The baseline is Lensfun tag `v0.3.4`, a lightweight tag at commit `101c745e847a5de4a1e569a94368ce2027198598`. Its database is version 1: 55 XML files, 4,249,579 bytes, 49 mounts, 948 cameras and 1,305 lenses (1,275 rectilinear). Version 1 has one calibration set per lens, with a lens-level crop factor, aspect ratio and (unused) centre. Lensfun's library is LGPL-3.0 and its database CC BY-SA 3.0 ([README](https://github.com/lensfun/lensfun/blob/v0.3.4/README.md), lines 27–55). There is no automatic database update, and Adobe LCP or other proprietary profile data is never imported.

Behavior is copied from these v0.3.4 sources, not from the current online manual, which documents the version 2 API:

| Behavior | Source at v0.3.4 |
| --- | --- |
| Normalization, aspect and crop correction | [`libs/lensfun/modifier.cpp`](https://github.com/lensfun/lensfun/blob/v0.3.4/libs/lensfun/modifier.cpp) lines 148–253 |
| Callback order (scale before distortion) | [`libs/lensfun/mod-coord.cpp`](https://github.com/lensfun/lensfun/blob/v0.3.4/libs/lensfun/mod-coord.cpp) lines 23–92, 209–225 |
| Distortion models and inversion | `mod-coord.cpp` lines 426–654; [`include/lensfun/lensfun.h.in`](https://github.com/lensfun/lensfun/blob/v0.3.4/include/lensfun/lensfun.h.in) lines 470–497 |
| Focal interpolation | [`libs/lensfun/lens.cpp`](https://github.com/lensfun/lensfun/blob/v0.3.4/libs/lensfun/lens.cpp) lines 841–943; Hermite in [`libs/lensfun/auxfun.cpp`](https://github.com/lensfun/lensfun/blob/v0.3.4/libs/lensfun/auxfun.cpp) lines 441–462 |
| String comparison | `auxfun.cpp` lines 335–380 |
| Crop compatibility and focal match strength | `lens.cpp` lines 1328–1366 |
| Database parsing, versions, defaults | [`libs/lensfun/database.cpp`](https://github.com/lensfun/lensfun/blob/v0.3.4/libs/lensfun/database.cpp) lines 193–258, 343–350, 678–687 |

Deliberate differences from Lensfun: a focal length outside the calibrated range is refused rather than clamped to the nearest calibration; records with mixed models, duplicate focal entries or a `<center>` are excluded rather than silently partly used; and inversion uses a bracketed solver rather than six unbracketed Newton steps.

## The bundled index

`cargo xtask lensfun-import --source DIR --output crates/luxforge-core/data/lensfun` reads a local checkout of the tag. It verifies every input file's git blob SHA-1 against a table in `xtask/src/lensfun_import.rs` and refuses any difference. It writes:

- `index.json`: canonical JSON (sorted keys and arrays, shortest round-trip floats) holding the mounts (name, `<compat>` list, `fixed`), the cameras (maker, model, mount, crop factor; `<variant>` ignored) and the admitted lens records.
- `provenance.json`: tag, commit, per-file blob SHA-1, SHA-256 and bytes, `index_sha256`, and every excluded record with its reason.
- `LICENSE-CC-BY-SA-3.0.txt` and `ATTRIBUTION.md`, stating the source, the license and that the index is a modified, filtered conversion.
- `crates/luxforge-core/src/modules/lens/pinned.rs`: the constants `RELEASE = "lensfun-0.3.4"`, `COMMIT` and `INDEX_SHA256`, so the binary knows exactly which index it accepts.

A lens record is admitted when its type is rectilinear, it has at least one distortion entry, every distortion entry uses one model (`poly3`, `poly5` or `ptlens`), no two entries share a focal length (byte-identical duplicates collapse to one), it has no `<center>`, and it has at most 256 distortion entries. Otherwise it is excluded with `unsupported-projection`, `no-distortion`, `mixed-distortion-models`, `duplicate-focal-calibration`, `centre-offset` or `too-many-calibrations` (the v0.3.4 `FinePix 2800 ZOOM & compatibles (Standard)` has 301). Only the whole-import caps fail the import: 16 MiB of input XML and 10,000 records. XML attributes map as `k1`/`a` → term 0, `k2`/`b` → term 1, `c` → term 2. A record without `<aspect-ratio>` has aspect 1.5; `a:b` is `a/b`. A mount is `fixed` when its name begins with a lowercase ASCII letter, Lensfun's naming convention for fixed-lens compacts (`djiFC3411`, `fujix100v2`).

Each admitted record is keyed `lf1-` followed by the first 16 hex digits of the SHA-256 of its canonical record JSON. Two records with the same maker and model (84 such pairs in v0.3.4, such as the two `Sony FE 24-70mm f/4 ZA OSS` calibrations at crop 1.534 and 1.0) are distinct keys. Aliases are only a record's untranslated `<model>` elements; `lang` translations are display text.

**Location and loading.** `xtask package` copies the four data files into the package: `Contents/Resources/lensfun/` in the macOS app, `lensfun/` beside the executable elsewhere, with the notices inventory listing them as CC BY-SA 3.0 data. The core resolves the index at `<executable>/../Resources/lensfun/index.json`, then `<executable directory>/lensfun/index.json`, then the checkout copy `concat!(env!("CARGO_MANIFEST_DIR"), "/data/lensfun/index.json")`, which serves tests, `cargo xtask develop` and background smoke and verify bundles. The first readable path wins. When the registry assembles the Lens module, one task on the shared Rayon pool reads and parses it into an immutable `LensIndex` held in a process-wide `OnceLock<Result<Arc<LensIndex>, Error>>`, checking its SHA-256 against `INDEX_SHA256`. Nothing parses on the owner or UI thread, and nothing parses per request. After it completes, lookups are synchronous over the immutable index; their cost remains to be measured. A lens query or selection answers `not-ready` with `data.missing: ["lens-profile-index"]` while the parse is running and when no path is readable, and `unsupported-input` when the file is malformed or its hash differs. The index is never read to evaluate a stored edit, so a missing or different index prevents new selections and nothing else.

## Source optics and the correction ledger

**Optical identity.** `CaptureMetadata` (`export/metadata/mod.rs`) already parses IFD0 Make and Model and Exif FocalLength, FocalLengthIn35mmFilm, LensMake and LensModel from the verified bytes on the source worker, for JPEG, NEF and DNG TIFF headers and the RAF embedded JPEG. It gains read-only accessors `make()`, `model()`, `lens_make()`, `lens_model()`, `focal_length_mm() -> Option<f64>` and `focal_length_35mm() -> Option<u16>`. `StageQuestions::optics()` answers a `SourceOptics` for the prepared source, lazily like `sample_before`, and `preparation-required` when the source is not prepared. LibRaw's normalized `RawMetadata.make`/`model` are never used for matching. The crop factor comes only from the matched Lensfun camera record. Nothing new is persisted.

**Ledger.** `SourceOptics` carries a correction ledger with three entries, `distortion`, `lateral_ca` and `shading`, each a status (`applied`, `known-unapplied` or `unknown`) and a provenance string (`dng-opcode:list3:WarpRectilinear:planes=R,B`, `dng-opcode:list3:GainMap`, `dng-opcode:list3:FixVignetteRadial`, `raw-mosaic`, `jpeg-exif-none`), plus the source interpretation marker (the DNG `interpretation`, `raw-mosaic:<mode>` for other RAW, or `jpeg`). The ledger is **derived, never persisted**: adding a field to `DngCorrectionMetadata` or `RawMetadata`, both `deny_unknown_fields`, would change the stored interpretation JSON that `core/source.rs` compares, and every existing DNG asset would fail with "original source interpretation changed".

The derivation is `luxforge_raw::optical_ledger(&RawMetadata) -> OpticalLedger` for RAW and a constant for JPEG, pure over persisted data plus the build-time camera catalog, so admission computes it without preparing the source:

- **Declared roles.** A camera record's `dng` block in `crates/luxforge-raw/data/cameras.json` may declare `optics`, the role of each required opcode, and `crates/luxforge-raw/build.rs` emits it beside `interpretation`. The FC3411 record declares `{"GainMap": "shading", "WarpRectilinear": "lateral-ca"}`. `RawMode` resolves its record, so the persisted `mode` finds the declaration.
- **Per-plane check at preparation.** When a DNG is prepared, the typed `Warp` is classified per plane, with the reference plane being plane 1 when there are three: reference plane not identity → `distortion`; reference plane identity and the others not → `lateral-ca`. The result must equal the declared role, or preparation fails with `UnsupportedMode("DNG optical role")`. The supplied FC3411 green plane is exactly `[1, 0, 0, 0, 0, 0]`, so its warp is lateral CA only.
- **Statuses.** For a WarpRectilinear in `applied` with role `distortion`, or with no declared role (a generic stage-ordered DNG), distortion is `applied`: an undeclared warp is conservatively treated as a distortion correction. Role `lateral-ca` sets `lateral_ca: applied`. GainMap and FixVignetteRadial set `shading: applied`. Otherwise every RAW source has distortion `known-unapplied` (`raw-mosaic`): a mosaic is not dewarped in camera, and neither LibRaw nor RawSpeed adds a warp. A JPEG has distortion `unknown` (`jpeg-exif-none`); an absent tag is never evidence of an uncorrected lens.

The [Air 2S contract](air2s-dng.md) stays authoritative: GainMap and WarpRectilinear run once, in recorded order, in camera-linear space before the matrix, source crop and EXIF orientation. Unknown mandatory opcodes still fail preparation, disabling the lens layer never disables a required correction, and WB redevelopment executes the required corrections once from sensor data.

**Eligibility.** One function enforces it: `validate_source_recipe` (`editor/source.rs`). It refuses an enabled lens payload when distortion is `applied` (`incompatible`, reason `embedded-distortion-applied`), when distortion is `unknown` and the payload does not carry `acknowledged: "assume-uncorrected"`, when the payload carries an acknowledgement while distortion is `applied`, when the payload's recorded interpretation marker or distortion status differs from the current derivation, and when the lens layer's full-resolution input stage `{long, short}` differs from the payload's `resolved_long`/`resolved_short`. It already runs on every draft and admission path; `EditorService::evaluation` also calls it for saved and framed entries, so export, reopen and Restore re-check. A neutral lens layer (`{"profile": null}`) is always legal.

## Profile resolution

`modules/lens/resolve.rs` resolves an optical identity and the lens input stage against the index. Strings compare as Lensfun's `_lf_strcmp` does: Unicode lowercase, whitespace runs collapsed to one space, trimmed. Nothing is chosen by a fuzzy match; the only automatic selection is a new RAW's eligible detected profile ([first open](#modules-api-and-history)).

- **Camera.** Exact normalized EXIF Make and Model against a camera record's maker and model, else `camera-not-in-database`. Several matching records give the union of their candidates.
- **Lens, fixed mount.** When the camera's mount is `fixed`, the candidates are the records whose mounts include it, whatever LensModel says (the FC3411 has none).
- **Lens, interchangeable mount.** Exact normalized EXIF LensModel against any untranslated model of a record whose mounts include the camera's mount or one of its `<compat>` mounts.
- **Coverage.** Camera crop ≥ 0.96 × lens crop, else `calibration-sensor-smaller`.
- **Crop mode.** When FocalLength and FocalLengthIn35mmFilm are both present, `|FL35 / FL ÷ camera_crop − 1| ≤ 0.05`, else `crop-mode-mismatch` (a Z 6 in DX mode reports crop 1.5 against a 1.0 record).
- **Aspect.** In sensor orientation, both ≥ 1, `|image_aspect / lens_aspect − 1| ≤ 0.03`, else `aspect-mismatch`.
- **Focal.** A single-calibration record accepts `|f / f_cal − 1| ≤ 0.01` and uses that calibration (EXIF says 8.38 mm for the 8.4 mm FC3411 calibration). A multi-calibration record accepts `f ∈ [f_min/1.01, f_max·1.01]` and clamps to `[f_min, f_max]`. Anything else is `focal-out-of-range` with `data: {focal, min, max}`; there is no extrapolation. A missing focal length may be supplied by the caller and is recorded as `source: "override"`.
- **Interpolation.** Reproduce `lens.cpp` lines 870–943 in `f64`. An exact focal match returns that calibration. Otherwise take up to two neighbours on each side (`spline[1]` nearest above, `spline[0]` next above, `spline[2]` nearest below, `spline[3]` next below), `t = (f − f₁)/(f₂ − f₁)`, and `term_i = H(y₀f₀, y₁f₁, y₂f₂, y₃f₃; t) / f` with the Hermite of `auxfun.cpp` lines 441–462, where a missing neighbour is `FLT_MAX` and its tangent falls back to the one-sided difference. Linear interpolation of the coefficients differs by up to 6.2 px at the corner of the NIKKOR Z 24-70mm f/4 S at 42 mm. The interpolated terms are frozen in the payload.

**Reasons.** A reason is either the photo's or the profile's. The photo's (`camera-not-in-database`, `focal-missing`, `crop-mode-mismatch`, `unsupported-stage`, `embedded-distortion-applied`, `assume-uncorrected-required`, and the status-only `lens-not-in-database`) hold for every profile alike. The profile's (`incompatible-mount`, `calibration-sensor-smaller`, `aspect-mismatch`, `focal-out-of-range`) mean it does not fit this camera, sensor, frame or focal length. Where several camera records match, a profile reports the reasons of the record it fits best.

**Detection.** The detected profile is the first eligible identity match (`match: "lens-model"` or `"camera-mount"`, ordered by model then key); failing that, the first held back only by the photo's reasons or its focal length, so the panel can say what is missing. Identity matches that fit only with profile reasons are not detected.

**Search.** Empty text lists nothing. Text lists the profiles whose maker or any untranslated model contains it and that fit the camera apart from the photo's reasons: identity matches first, then by model and key, 50 a page. A profile with a profile reason is never listed, and a camera the database does not hold lists nothing. `edit.select-lens-profile` still resolves any key and refuses one that does not fit with its reasons.

## Geometry

**Coordinates.** A stage of `W×H` pixels has continuous edge coordinates `x ∈ [0, W]`, pixel `i` centred at `i + 0.5`, x right and y down, the convention `Resample` already uses. A resample maps each output pixel centre back to a continuous input coordinate.

**Lens normalization.** Lensfun measures pixel centres: `x_lf = x − 0.5` and its centre is `(W−1)/2`. With `Wm = W−1`, `Hm = H−1`, `m = min(Wm, Hm)` and `a = max(Wm, Hm)/m`:

- `cc = √(ar_lens² + 1) / √(a² + 1) · crop_lens / crop_camera`, `NS = 2·cc/m`;
- normalized `x_n = (x − W/2)·NS`, `y_n = (y − H/2)·NS`.

The optical centre is the lens stage's own centre, after EXIF orientation, the RAW default crop and the orientation layer. The map is radially symmetric with min-side normalization, so orientation needs no conjugation, and the lens `carry` returns `Ok(None)`.

To keep proxies equal to the scaled full-resolution map, resolution freezes the dimensionless `unit_scale = NS_full · min(W, H)_full / 2` from the full-resolution lens input stage and records `resolved_long` and `resolved_short`. Compiling on any stage `W_s × H_s` uses `NS_s = 2·unit_scale / min(W_s, H_s)` with the centre `(W_s/2, H_s/2)`. At full resolution this is exactly the Lensfun normalization; a proxy differs only by per-axis rounding, as a crop does.

**Lens models.** Output (corrected) to input (distorted) is the direct polynomial `r_d = f(r_u)`:

- `poly3`: `f(r) = r(1 − k1 + k1 r²)`;
- `poly5`: `f(r) = r(1 + k1 r² + k2 r⁴)`;
- `ptlens`: `f(r) = r(a r³ + b r² + c r + d)`, `d = 1 − a − b − c`.

With cover scale `s`, an output position `p` (normalized) reads `(p/s)·g(ρ)` with `ρ = |p|/s` and `g(t) = f(t)/t`; at `ρ = 0`, `g = f′(0)`. Input to output (a mask outline, a pick) inverts `f` with a bracketed solver on `[0, R_mono]`: Newton safeguarded by bisection, starting at `r_d` clamped into the bracket, stopping when `|f(r_u) − r_d|` is at most `1e-4` input pixels or after 32 iterations (`MapError::Unconverged`). `R_mono = min(first positive root of f′, 4·ρ_max)` is computed at compile, and a point with `f(R_mono) < r_d` is `MapError::Outside`. Visible points converge in two to four iterations; unbracketed Newton diverges beyond the fold radius of wide lenses.

**Perspective.** Centre `c = (W/2, H/2)` and unit `U = max(W, H)/2`. `h = Horizontal/400` and `v = Vertical/400`, with Horizontal and Vertical integers in `[−100, 100]`, so `n = (h, v)` has components in `[−0.25, 0.25]`. Content to output: `u = (q − c)/U`, `w = u / (1 + n·u)`, `p = c + U·s_P·w`. Output to content: `w = (p − c)/(U·s_P)`, `u = w / (1 − n·w)`, `q = c + U·u`. Zero is exact identity; a positive Horizontal compresses the right side and a positive Vertical the bottom. It is a keystone control, not a camera-angle estimate. Over the output rectangle the denominator stays at or above 0.5; the pole lies at `|u| ≥ 2.83`, which only positions outside the content rectangle can reach.

**Carry.** A quarter turn or reflection with centred linear part `R` carries Perspective exactly as `(Horizontal, Vertical)′ = R·(Horizontal, Vertical)`, mirror first, then turns, with RotateRight `[[0, −1], [1, 0]]`. For `(40, −25)`: one turn gives `(25, 40)`, two `(−40, 25)`, three `(−25, −40)`, a mirror `(−40, −25)`, a mirror and one turn `(25, −40)`. The carry rewrites the payload only, as the architecture's rule for the hook requires. The cover scale is invariant.

## Coverage and sampling limits

Each warp computes its own centred cover scale from its input stage and payload alone, so adding Perspective never changes the Lens stage or its prefix preview. Both have closed forms, so coverage costs a few arithmetic operations in `compile` on any thread; there is no certificate, search, cache or preparation job.

- **Lens.** With `ρ_min = min(W, H)/2·NS` and `ρ_max = hypot(W, H)/2·NS`, `s_L = max(1, max over t ∈ T of g(t))`, `T = [f⁻¹(ρ_min), min(ρ_max, f⁻¹(ρ_max))]`. The maximum is at an end of `T` or a critical point of `g` inside it: none for `poly3`, `t² = −k1/(2k2)` for `poly5`, the roots of `3a t² + 2b t + c` for `ptlens`. This follows from `f(ρ/s) ≤ ρ ⟺ s ≥ ρ/f⁻¹(ρ)` along every boundary ray, given `f` monotone. Across all 5,661 single-model rectilinear v0.3.4 calibrations it equals dense sampling to `1.3e-9`, and Lensfun's own eight-point autoscale underestimates 185 of them.
- **Perspective.** `s_P = 1 + |h|·W/max(W, H) + |v|·H/max(W, H)`, exact because a homography with a positive denominator maps the rectangle to a convex quadrilateral. It is 1.4167 at both extremes on 3:2 and 1.50 on 1:1.
- **Applied.** Each scale is applied as `s·(1 + 1e-12)`. A barrel profile that touches the edge at `s = 1` is covered.
- **Refusals.** `f′ ≤ 0` anywhere on `[0, ρ_max]` (a fold), `f(ρ_max) ≤ 0` or non-finite terms refuse with `validation`. A combined `s_L·s_P` above 4 refuses with `validation`.
- **Minification.** Each stage reports `local_scale_max`, the largest input-pixels-per-output-pixel singular value of its output-to-input Jacobian over the output rectangle. For Lens it is `max over t ∈ [0, ρ_max/s] of max(f′(t), g(t)) / s`, at ends of the interval or roots of `f″` and `g′`. For Perspective the bound `(D_min + |w|_max·|n|) / (D_min² · s_P)` holds, with `D = 1 − n·w` minimal and `|w|` maximal over the output rectangle, both at corners; it is tight at the extremes. The crop's rotation has scale 1. A chain whose product of stage maxima exceeds `MAX_LOCAL_MINIFICATION = 1.8` refuses with `unsupported-input`, "strong minification: bilinear would alias". That admits every Perspective setting (at most 1.5) and all but three calibrations alone. 16 calibrations exceed 1.25 on their own (Sigma 17-50 at 17 mm reaches 1.732).

An identity calibration (65 in v0.3.4 are all zero) and neutral Perspective compile to `ExactGeometry::identity`, keeping the exact byte path and the shared buffer.

## Host mapping primitive

The host owns the primitive, stage composition, admission and execution; modules own payload validation and call the host's step constructors. It lands in two steps.

**Step 1: the affine refactor, no behavior change.** `Resample { map: Mapping, output_width, output_height }` replaces `inverse: [f64; 6]` (`Resample` becomes `Clone`, not `Copy`), with `Mapping::Affine([f64; 6])` its only variant. `Mapping::input_at(x, y)` is the one output-to-input function. `render::GeometryMap` is the content-to-output chain of a whole stack (exact steps and resamples), with `to_content`, `to_output` and `local_scale_at`, and every current reader of `StageTransform.forward`/`.inverse` calls it instead of applying coefficients: `render/locate.rs`, `render/compiled.rs`, `render/entry.rs`, `editor/evaluate.rs`, `api/methods.rs`, `analysis/mask_overlay.rs`, `preview/coverage.rs`, `modules/registry/compile.rs` and `modules/crop/module.rs` in the core; `mask_draft.rs`, `app/tasks.rs`, `app/masks.rs`, `app/message/mask.rs`, `view/mask_canvas.rs`, `app/thumbnails.rs` and `crop_draft.rs` in the desktop, whose `ContentMap` wraps the core `GeometryMap`; and `mask_interactions_smoke.rs` and `mask_brush_smoke.rs` in xtask. `render.transform` answers the new shape below with `mapping.kind: "affine"`.

**Step 2: warps.** `modules/processing.rs` adds `Processing::Warp(WarpStep)` and `Mapping::Warp(Arc<WarpChain>)`; `render/map.rs` holds:

```rust
pub struct WarpChain { steps: Vec<WarpStep> /* ≤ 3, output→input order */ }
pub enum WarpStep { Affine([f64; 6]), Projective(Projective), Radial(Radial) }
pub struct Projective { h: f64, v: f64, centre: (f64, f64), unit: f64, cover: f64, local_scale_max: f64 }
pub struct Radial { model: RadialModel, terms: [f64; 3], centre: (f64, f64), px_per_unit: f64,
                    cover: f64, monotone_radius: f64, local_scale_max: f64 }
pub enum RadialModel { Poly3, Poly5, PtLens }
pub enum MapError { Outside, Unconverged }
impl WarpStep {
    pub fn radial(model: RadialModel, terms: [f64; 3], unit_scale: f64, stage: Stage) -> Result<Self, Error>;
    pub fn projective(horizontal: i64, vertical: i64, stage: Stage) -> Result<Self, Error>;
}
```

Each step is evaluated by one scalar `f64` function with a fixed operation order and no `mul_add`, shared by rows, points, `reads`, `locate`, overlays and the desktop, so sample and render agree bit for bit.

**Fusion and canonical order.** The canonical stack is `[source] [pixel, colour, spatial…] [orientation?] [lens?] [perspective?] [crop?] [finish…]`. In `ModuleRegistry::compile`:

- The first `Processing::Warp` opens a segment whose entry is `Entry::Resample` with a one-step chain, reading the frame of the segment before it (content, colour and orientation).
- A later `Processing::Warp`, or the crop's `Processing::Resample`, whose current segment's entry is a warp chain with no work after it (no colour, point replacement or spatial work, exact geometry the identity), is composed into that chain instead of opening a segment. The fused entry is one resample: Lens, Perspective and a straightened crop interpolate once.
- An exact step after the chain (an integer crop) stays the segment's exact geometry after the resample, as today.
- A crop with no warp before it compiles exactly as today.

Without this rule the linear path refuses a second resample (`MAX_RESAMPLES`) and the byte path quantizes two or three times. Compile also refuses, rewriting nothing, with `validation` "lens and perspective layers must follow the orientation and precede the crop, with nothing else between (layer `<id>`)": a warp followed by a non-geometry layer before the crop or the end, a warp before the orientation layer, Perspective before Lens, or a crop before a warp. Stacks without the new effects keep today's rules. The chain enforces the 4× cover cap and the 1.8 minification cap as it fuses.

**Reads.** `Resample::reads` in `render/geometry.rs` stays the only read-rectangle rule, and keeps the one tap floor `check-repository`'s `one-read-rectangle` rule looks for. It asks `Mapping::bounds(window)` for a continuous bounding box, carried from output to input through the chain: four mapped corners for an affine or projective step (exact by convexity); for a radial step on an axis-aligned rectangle, the corners, each vertical edge's crossing of the horizontal axis and vertical-edge points at the critical radii of `g`, symmetrically for y, since along an edge the other coordinate is monotone whenever `f′ > 0` and `g > 0`. Composition through bounding boxes is conservative. The existing `floor(u − ½)` tap rule, the `+1` tap, `TAP_MARGIN` and the stage clamp then apply unchanged. The corner-only rule underestimates radial reads by up to 33 px; the candidate-point rule never underestimated in 3,000 random windows per profile.

**Execution.** The byte `resample_frame` and the linear `load_resampled` evaluate `Mapping::input_at` with the existing linear-light bilinear kernel, four taps with replicated edge indices. JPEG decodes taps to linear light and quantizes once at the geometry boundary; RAW keeps signed and highlight values to the terminal boundary. Tap blocks reuse `TAP_BLOCK_PIXELS` (16,384 pixels, 384 KiB); a 64×16 output block at 2.6× reads about 7,800 pixels. The byte path adds no scratch. `luxforge_raw::RenderPass::Warp` counts a chain containing a warp step, with a provisional serial-to-pool threshold equal to `Resample`'s until measured. No frame-sized displacement map or extra RGB intermediate exists; at most two live segment frames under the existing 512 MiB RGBA and 1.5 GiB RAW planar limits. `ScratchBudget` stays a target, not a limit.

## Crop, orientation and compare

Changing Lens or Perspective keeps the crop's stored rectangle, angle and ID on its fixed pre-crop canvas; only the content under it changes. Crop validation is unchanged because the canvas keeps the crop's input dimensions. Both cover scales are computed before the crop, so a crop cannot admit a stronger warp. Crop reset restores the covered corrected canvas; Lens and Perspective resets affect only their module. Sweeps derive every candidate from the same draft base, so returning to the start restores identical output. Crop's input-stage preview includes the warps. A quarter turn or reflection carries Perspective by conjugation and the crop as today, in one transaction; Lens is invariant.

Compare with `keep_geometry` splices the framing entry's whole geometry tail, including Lens and Perspective. The uncropped Original keeps mandatory source corrections and no optional warp. The finish vignette evaluates final output coordinates.

## Masks, overlays and picking

Masks keep their normalized content positions and height-based sizes; applying or removing a warp never rewrites mask or stroke coordinates. Pixel replacements, range-mask sampling and neutral picking address the same content stage.

`render.transform` answers:

```json
{"entry_id", "snapshot_id", "source_fingerprint", "draft"?,
 "content": {"width", "height"}, "output": {"width", "height"},
 "mapping": {"kind": "affine", "forward": [6], "inverse": [6]}
          | {"kind": "warp", "steps": [...content→output...], "domain": {"width", "height"},
             "cover": {"lens", "perspective", "combined"}, "local_scale_max"},
 "mapping_sha256"}
```

The identity fields mirror `PixelSample`. A draft map request refuses `conflict` if its base revision changed; Reapply selects a new base explicitly. The desktop acquires one descriptor per displayed entry or draft revision, evaluates it locally with the core `GeometryMap` and never calls the owner per pointer move; crop, pick, overlay and stroke never mix `mapping_sha256` values. `render.locate` maps a rendered centre to the content pixel containing it (`floor`), and `render.sample` still evaluates the whole filter footprint. An output point outside the domain, or an inversion that is `Outside` or `Unconverged`, is refused explicitly and never clamped. RAW neutral picking performs the recipe inverse first, then the existing content-to-sensor query; the DNG warp is not applied again.

Overlays: the core coverage overlay's cells call `GeometryMap::to_content` at each cell centre (output to content is the direct polynomial, no inversion), and the coverage cache key (`preview/coverage.rs`) hashes `mapping_sha256` instead of the affine forward coefficients, so a Lens-only change misses the cache. Desktop outlines clip every figure polyline to the content rectangle before mapping, then subdivide each edge, straight lines included, until the mapped midpoint lies within 0.25 physical pixels of the chord, at depth at most 10 and at most 1,024 segments per figure. At the cap the coarse polyline is drawn and evidence records `approximate`. Handles outside the domain are hidden; the brush cursor ring is the same subdivided ellipse. Pointer positions map to content with `to_content` before `path.rs` quantization, and `ContentMap::tolerance` uses `GeometryMap::local_scale_at(pointer)` instead of the scale at the output origin. A geometry commit during an active gesture conflicts its draft; Discard or Reapply refreshes the map, and a brush held between strokes refreshes on the displayed entry change.

## Modules, API and history

**Lens correction** is `LensModule` in `modules/lens/{mod.rs, payload.rs, resolve.rs, answer.rs, products.rs, report.rs, index.rs, pinned.rs}`: effect `luxforge.lens.distortion`, format 1, stage geometry, order 2, single, not maskable, for JPEG and RAW. Neutral is `{"profile": null}`. A resolved payload (about 700 bytes; the cap is 16 KiB) is:

```json
{"profile": {
  "key": "lf1-…", "interpretation": "lensfun-v1-distortion-edge-1",
  "database": {"release": "lensfun-0.3.4", "commit": "101c745e847a5de4a1e569a94368ce2027198598",
               "index_sha256": "…", "record_sha256": "…"},
  "camera": {"maker": "Nikon Corporation", "model": "Nikon Z 6", "crop_factor": 1.0, "fixed_mount": false},
  "lens": {"maker": "Nikon", "model": "NIKKOR Z 24-70mm f/4 S", "crop_factor": 1.0, "aspect_ratio": 1.5},
  "focal": {"mm": 35.0, "source": "exif"},
  "model": "ptlens", "terms": [0.019, -0.056, 0.063],
  "normalization": {"unit_scale": 0.99856, "resolved_long": 6048, "resolved_short": 4024},
  "optics": {"source_interpretation": "raw-mosaic:NikonZ6Lossless14", "distortion": "known-unapplied",
             "acknowledged": null}}}
```

`focal.source` is `exif` or `override`; `acknowledged` is `null` or `assume-uncorrected`, a per-selection choice never remembered per camera or lens. Floats are shortest round-trip; hashes are lowercase SHA-256 hex. Evaluation reads only the payload: reopening, history and export never rematch the index.

**Perspective** is a field patch (`modules/perspective.rs`): effect `luxforge.perspective`, format 1, stage geometry, order 4, single, not maskable, payload `{"horizontal": 0, "vertical": 0}`, both `ParameterKind::Integer {min: -100, max: 100}`, drawn as Number controls with step 1. The field-patch framework (`modules/field_patch.rs`) gains a Geometry shape: `Shape::of(EffectStage::Geometry)` returns `Shape::Geometry`, whose neutral is `ExactGeometry::identity` of the received stage, whose compile returns `Processing::Warp`, and whose module can implement a new `FieldPatch::carry` hook that `FieldPatchModule::carry` forwards. `Spec::presettable(false)` sets the derived patch action's `preset: false`.

**Presets.** `ActionDescriptor` gains `preset: bool`, default `true`. `ModuleRegistry::patch_action` refuses an action with `preset: false` ("`<id>` is not presettable"), so capture, create, apply and import all refuse it through their existing path, and the desktop's `presettable_groups` skips it. In `presets/mapping.rs` every `Perspective*` and `PerspectiveUpright` row stays unsupported with the reason "different perspective model", and the [presets design](presets.md) lists them apart from the "no such tool" row.

**Commands.**

- `query.lens-profiles {asset_id, text?: string ≤ 64, page?: integer 0..99, focal?: number 0.5..2000, assume-uncorrected?: boolean = false}` returns `{status, rows, page, pages, total}`. `rows` are the [search](#profile-resolution) for `text`, at most 50 a page, each `{key, title, subtitle, focal_range, crop_factor, model, match, eligible, reasons, parameters?}`; empty text answers no rows. It plans synchronously on the owner from the loaded index and the prepared optics, like `crop`, with no job. `status` carries:
  - `state`: `applied` (the layer holds a profile), `already-corrected` (the source's distortion is `applied`), `camera-not-found`, `detected` or `lens-not-found`, in that precedence;
  - `camera` (the matched record's maker and model, or `null`), `camera_name` (the curated [product name](#modules-api-and-history) with its code, else the record's name, else EXIF make and model), `product`, `fixed_mount`, `exif {make, model, lens_make, lens_model}`, `lens_model`, `focal_mm`, `focal_source`, `crop_factor`, the three ledger entries `distortion`, `lateral_ca` and `shading`, `reasons` (the detected profile's, or why none was detected, plus `assume-uncorrected-required` when distortion is `unknown` and the query did not acknowledge it, or `embedded-distortion-applied`), and `detected` (the detected row, or `null`);
  - the [query-choice vocabulary](ui-components.md#control-kinds): `summary` ("In camera: distortion not corrected · lateral CA corrected · shading corrected"), `visible_shared` (`["focal"]` when the focal length is missing, overridden or out of the detected profile's range), `search` (false when no profile could ever be selected: camera not found, already corrected, crop mode or a stage under 2 px), `current` (the applied profile as a card), `suggestion` (the detected profile as a card with Apply, only when nothing is applied), `notice` and `report`.

  A card reads for a fixed lens "`<camera_name>` · built-in lens" over "`<record model>` · `<f>` mm", and for an interchangeable lens "`<lens model>`" over "`<camera_name>` · `<f>` mm". When distortion is `unknown` (a JPEG), every row and the suggestion carry `parameters: {"assume-uncorrected": true}` and the suggestion's note says the camera may already have corrected distortion and that applying assumes it did not; the action still refuses a request without the acknowledgement. When the source already corrects distortion, rows an agent searches for are ineligible with `embedded-distortion-applied`. Notices: the photo's blocking condition for a detected profile (no focal length, focal out of range, crop mode, too small), "This photo's source already corrects lens distortion, so a profile would correct it twice.", "`<camera_name>` is not in the lens database.", "No profile matches `<lens>`." (or the built-in lens, an unnamed lens, or a matched profile that does not fit the sensor or frame), and for a search with text and no compatible profile "No compatible profile matches “`<text>`”.".
- `edit.select-lens-profile {asset_id, profile: string ≤ 32, focal?: number 0.5..2000, assume-uncorrected?: boolean = false}` resolves, freezes and commits one action, non-patch and not presettable. `edit.reset-lens-profile {asset_id}` writes `{"profile": null}`, keeping the layer ID.

**First open.** `ToolModule::first_open` is the host's extension point for an action a module commits when an import creates an asset. The host asks each available module that applies to the photo, in registry order, right after the new asset's Original is written, against that stack, and commits what it proposes through the ordinary action path by the `system` actor (request `first-open:<asset>:<module>`), one entry each. Lens proposes `select-lens-profile {profile: <detected key>}` only when the asset has no lens layer, its distortion is `known-unapplied` and the detected profile is eligible as it stands, so no focal override or acknowledgement is ever implied. Planning reads metadata through the prepared source's optics and the loaded index; it never reads pixels. The source worker calls `ToolModule::await_first_open` after decoding a file no asset names, which for Lens waits for the one background index parse, so the catalog owner never waits; an index that failed to load is reported, not skipped. The import's `job.read` lists `first_open: [{module_id, action_id, entry_id, label}]` for a committed entry, or `{module_id, action_id?, error}` for a refusal, and its `result` and `job.adopt` are the asset's state after those entries. A file imported again, a reopen and an asset with history are never asked again.

**Report missing lens.** `status.report` is `{label: "Report missing lens", url}`, offered for `camera-not-found`, `lens-not-found` and a search with no compatible profile when the photo names its camera. The URL is `https://github.com/Willyham/luxforge/issues/new` (`report::NEW_ISSUE_URL`, the one place the repository is named) with a percent-encoded title ("Lens profile request: `<camera_name>` · `<lens>`") and a body table: camera name, EXIF make and model, EXIF lens make and model, focal length and 35 mm equivalent, the database crop factor when known, the source kind (RAW, DNG or JPEG) with its interpretation marker, the ledger's distortion status and provenance, the reasons, the Lensfun release and commit and the Luxforge version. It names no file, path, serial number or location. Each field is one table cell of at most 64 characters with controls dropped and pipes escaped, cut further until the URL is at most 6,000 characters. Opening the page publishes nothing; the person reviews and submits it on GitHub.

**Product names.** `modules/lens/products.rs` holds the curated DJI table, keyed by normalized EXIF make and model: FC3411 Air 2S, FC6310 Phantom 4 Pro, FC3170 Mavic Air 2, FC7203 Mavic Mini, FC7303 Mini 2, FC3582 Mini 3 Pro, FC8482 Mini 4 Pro, L1D-20c Mavic 2 Pro, FC2204 Mavic 2 Zoom, L2D-20c Mavic 3, FC2103 Mavic Air, FC220 Mavic Pro, FC300X Phantom 3 Professional and FC200 Phantom 2 Vision, each "DJI `<product>`"; the L1D-20c and L2D-20c are also listed under the Hasselblad make their photographs carry. Matching never reads it.
- `edit.set-perspective {asset_id, horizontal?, vertical?}` and `edit.reset-perspective` come from the field-patch framework, with its draft lifecycle and conformance.
- `recipe.describe` reports each stage's cover scale, the combined scale and the local minification; `module.list` and `schema.list` describe every parameter, control and error.

**Errors** use existing kinds:

- `validation`: the canonical-order message above; "lens and perspective need a `<x.xx>`× zoom, more than the 4× limit"; "focal length `<f>` mm is outside the profile's calibrated `<a>`–`<b>` mm" (`data.reason: focal-out-of-range`); "unknown lens profile `<key>`".
- `incompatible`: "this photo's source already corrects lens distortion (`<provenance>`); a profile would correct it twice" (`data.reason: embedded-distortion-applied`); "the profile was admitted against source optics `<old>`; the source now reports `<new>`"; "assume-uncorrected is required: this photo's distortion correction status is unknown"; a changed full-resolution lens stage.
- `unsupported-input`: "profile `<key>` is not supported: `<reason>`" with the import and resolution reasons above; "strong minification: bilinear would alias"; a malformed or wrongly hashed index.
- `not-ready` while or when the index is unavailable; `preparation-required` when optics need the prepared source; `render` for `Unconverged`.

**History labels.** Selection: "Lens profile `<lens model>` at `<f>` mm"; a fixed-mount record: "Lens profile `<record model>`". Reset: "Reset Lens correction". Perspective uses the framework's labels. One selection or gesture is one entry; a same-value action or a reset at neutral is a no-op. Invalid, unavailable or duplicate-correction payloads stay readable in history and fail evaluation; nothing commits partially.

**Query-choice control.** Profile selection needs a search and a few states no static control can hold. `Control::QueryChoice(QueryChoiceControl {label, query, text, page, action, key, shared})` binds the module's own query (with a `String` text parameter and an `Integer` page parameter) to its own non-patch action (with a `String` key parameter that receives the chosen key); `shared` names parameters declared with equal kinds by both, rendered as inputs and sent to both. Lens shares only `focal`. Registration checks every binding, at most one per module. The answer's rows and its optional status vocabulary (cards, notice, report link, whether a search is offered, per-choice parameters) are the generic contract in [UI components](ui-components.md#control-kinds), validated by the host before any client sees them. The desktop asks with empty text when the section is shown, which answers the status alone, and re-queries on each text change with no debounce, one active request and one newest pending request; it drops answers for stale text and keeps Retry for a failed answer. Matching, eligibility, notices and the report are the module's.

**Panel.** `linked_modules` places `LensModule` ("Lens correction", hint "Profile distortion correction") and `PerspectiveModule` ("Perspective", hint "Keystone correction") after the colour mixer, both collapsed, as the [workspace](develop-workspace.md) places them. Lens draws exactly one of these, from its answer:

- **Applied:** the `current` card (for the Air 2S DNG, "DJI Air 2S (FC3411) · built-in lens" over "FC3411 & compatibles · 8.38 mm"; for a Z 6 lens, "NIKKOR Z 24-70mm f/4 S" over "Nikon Z 6 · 35 mm") with Change, which opens the search under it and closes again as Close; closing drops the typed text. A choice from the search closes it.
- **Detected, not applied:** the `suggestion` card with Apply and Change. On a JPEG its note says the camera may already have corrected distortion and that applying assumes it did not, and Apply sends `assume-uncorrected`. After a reset the detected profile is offered the same way.
- **No profile found:** the warning triangle with "No profile matches `<lens>`.", the search (empty text lists nothing) and Report missing lens.
- **Nothing can fit:** the warning naming the camera ("FUJIFILM X100VI is not in the lens database.") and Report missing lens, with no search, since no row could ever be compatible. A source that already corrects distortion says so instead, with neither.

A search with text that finds no compatible profile shows the same warning style ("No compatible profile matches “`<text>`”.") with Report missing lens. The focal field appears only when `visible_shared` names it and a choice can be made (beside the suggestion, or with the search). Report missing lens opens the page in the default browser off the update loop through the platform opener (`open`, `xdg-open`, or `rundll32 url.dll,FileProtocolHandler`), after checking it is a single https token; tests and evidence runs record the page instead. The caption under the section is the `summary`. The section also shows the module reset. Perspective shows its two Number controls and the reset. No disabled CA, vignetting or Upright controls.

## Persistence and export

Export freezes the committed entry, source interpretation and resolved payloads with the existing `Evaluation`; it never rematches a profile, exports a draft or uses a proxy. `export.plan` reads compiled dimensions without rendering, and the fixed canvas means Lens and Perspective never change output dimensions. JPEG export shares the exact kernel and the existing job lane, destination and alias protection, atomic publication, metadata normalization and cleanup. Duplicate DNG correction, a changed interpretation and unsupported mappings fail before publication. Tests compare the lossless pre-encode render with the exact render; JPEG bytes are not an oracle.

## Performance

The [performance rules](../engineering/performance-rules.md) apply. The index parses once on the shared pool; queries and selection use bounded owner lookups over the immutable index; their cost is measured after integration. Cover scales, fold checks and read rectangles are closed-form arithmetic in compile. Pixels run on the shared pool with cancellation per row or tap block, one active and one replaceable pending preview, half-detail motion, exact refinement, 120 ms quiet settlement and the export lane's one running and four waiting. Region output equals the same slice of a full render; proxies use the frozen `unit_scale`. A warp point costs a bounded mapping plus four taps and stays on the owner; only a spatial prefix sends a point to the point worker. No-op detection compares payloads. No private pools, idle polls or timers.

### Performance review

- Original reads, hashes and decode stay in the signature-verified [source worker](../../crates/luxforge-core/src/editor/source.rs). [Optical identity](../../crates/luxforge-core/src/source/optics.rs) is extracted during preparation; profile queries and compilation borrow it.
- The host retains the existing bounded byte/linear output allocations and one fused warp output; sources and identity buffers are shared. [Tap scratch](../../crates/luxforge-core/src/render/linear.rs) is bounded to 16K pixels. The [index](../../crates/luxforge-core/src/modules/lens/index.rs) is limited to 16 MiB, [frozen payloads](../../crates/luxforge-core/src/modules/lens/payload.rs) to 16 KiB and [outline paths](../../crates/luxforge-app/src/view/warped_path.rs) to 1,024 segments.
- [Point picks](../../crates/luxforge-core/src/render/locate.rs) use the compiled mapping and four taps, with the existing bounded spatial-tile exception for a spatial prefix. [Validation and same-value actions](../../crates/luxforge-core/src/modules/lens/mod.rs) compare metadata and payloads without rasterizing.
- The owner handles [profile matching](../../crates/luxforge-core/src/modules/lens/resolve.rs), bounded payload validation, [geometry compilation](../../crates/luxforge-core/src/modules/registry/compile.rs) and catalog transactions. Render, source preparation, export and index parsing remain on workers. A search runs the fit arithmetic over every text match (a few comparisons per matching camera record) and allocates response strings only for the visible page; an empty-text answer scans only the identity matches. The [report](../../crates/luxforge-core/src/modules/lens/report.rs) is a string of at most 6,000 bytes built from bounded metadata.
- A new import's [first open](../../crates/luxforge-core/src/editor/plan.rs) runs on the owner after the Original is written: per module one metadata-only plan over the prepared source's optics and the loaded index, and at most one ordinary commit, with no pixel read. The only wait is the source worker's, once per process, for the index parse an import began before it finished.
- Selection/reset use the normal mutation refresh and one preview request. [Query-choice answers](../../crates/luxforge-app/src/app/query_choice.rs) update only their current list identity; they add no history reads or image uploads. Opening a report runs the platform opener once inside a task, off the update loop. [Mask mapping](../../crates/luxforge-app/src/app/masks.rs) is read once per gesture and evaluated locally.
- Query-choice adds one active request and one replaceable pending request. No production timer, polling loop or subscription is added; existing preview settlement and resource retirement remain gated as before.
- No new raster cache or cache budget is added. The existing [coverage reuse keys](../../crates/luxforge-core/src/preview/coverage.rs) include source identity, the relevant recipe and mapping identity, mask dependencies, grid dimensions and window. `mask_overlay_cache_misses_when_only_lens_changes` proves that a Lens-only edit invalidates the cached grid. Coverage retains the existing bounded overlay grids; hit rate, rebuild cost and retained bytes remain part of TASK-015, with no cache performance claim before measurement.
- The final native 24/60 MP and authentic RAW distributions, including the warp threshold, remain pending a quiet host. `editor-performance --lens-only` compares neutral Lens against the selected profile with identical Perspective and crop, so it measures the added lens cost rather than a different output size. No speed or memory-budget result is claimed before these runs.
- The independent [reference geometry study](../../crates/luxforge-reference/tests/studies/geometry.rs), dense read-bound and eight-orientation fixtures, [warp tests](../../crates/luxforge-core/src/render/warp_tests.rs) for single bilinear and signed-linear evaluation, full/region/point equality and neutral buffer-sharing checks provide the exactness evidence.

## Verification and qualification

Tolerances: coordinates within `1e-9` px of the `f64` reference; JPEG bytes within 1 code per channel, identity and integer cases exact; RAW `1e-6 + 1e-6·|ref|`; region against full, sample against render and serial against pool bit-exact.

The frozen fixture `fixtures/geometry/lens-perspective.json` is generated by `crates/luxforge-reference/tests/studies/geometry.rs` from `crates/luxforge-reference/src/geometry.rs`, which imports no product crate. It holds the NIKKOR Z 24-70mm f/4 S at 24, 26, 30, 42, 60 and 70 mm (the Hermite terms at 26 mm are `a = 0.026132, b = −0.093154, c = 0.065238`, at 42 mm `0.022247, −0.064513, 0.084468`); the FC3411 at 8.4 mm; the X100V at 23 mm; the Canon PowerShot G12 `poly5` at 6.1 mm; the Olympus M.Zuiko 14-42 `poly3` at 14 mm; the Sony E 10-18 at 10 mm; the Sigma 17-50 at 17 mm; the Tokina 11-16 at 11 mm; one identity calibration; Perspective extremes on 3:2, 4:3, 16:9, 1:1 and 2:3; and the eight orientations.

Photographic qualification is per camera, lens and focal range, separate from RAW decode support, and is recorded as a matrix in this section and in [feature status](../features.md). Photographs available in the private RAW manifest: `nikon-z6` (NIKKOR Z 24-200mm f/4-6.3 VR at 200 mm, an exact v0.3.4 calibration), `dji-air2s` (FC3411 at 8.38 mm, snapping to 8.4 mm, distortion `known-unapplied`, lateral CA `applied`) and `fujifilm-x100vi` (the expected `camera-not-in-database` refusal). Required and not yet supplied: photographs with straight architectural lines near the frame edges from the Z 6 with the 24-200 at 24 mm and with the NIKKOR Z 24-70mm f/4 S at 24 and 70 mm, an Air 2S DNG with straight lines, a Z 6 camera JPEG for the `unknown` path, and a keystoned building for Perspective. The owner adds them to the private manifest. A focal is qualified when, on at least three marked straight edges, the corrected maximum deviation from a fitted line is at most 25% of the uncorrected deviation and at most 3 px on a 6,048-px long side. `cargo xtask lens-qualification` maps owner-marked edge points (content coordinates, at least five per edge) through the stored map headlessly and reports both deviations. A missing photograph leaves that row untested, never passed. Timing uses two generated workloads, a 24 MP JPEG with a Z 6 and NIKKOR Z 24-70mm f/4 S identity and a 60 MP JPEG with a Sony ILCE-7RM4 and FE 24-70mm f/4 ZA OSS identity, so a profile resolves on photo-sized inputs.

## Qualification matrix

The current authentic manifest run resolved and exported the Nikon and DJI selections through the JSON API and preserved all supplied source hashes. Numerical and generated-grid tests prove the mapping contract. Photographic lens accuracy remains unqualified until marked straight edges are supplied; a resolving database record alone is not photographic support.

| Camera and lens | Focal | Functional result | Photographic result |
| --- | --- | --- | --- |
| Nikon Z 6 · NIKKOR Z 24-200mm f/4-6.3 VR | 200 mm | Profile selected and exact JPEG export ready | Untested: supplied photograph has no marked straight edges |
| Nikon Z 6 · NIKKOR Z 24-200mm f/4-6.3 VR | 24 mm | Untested | Untested: required photograph missing |
| Nikon Z 6 · NIKKOR Z 24-70mm f/4 S | 24 and 70 mm | Generated-grid and independent numerical checks only | Untested: required photographs missing |
| DJI Air 2S · FC3411 | 8.38 mm, snapped to 8.4 | Profile selected, exact JPEG export ready and neutral picking verified; embedded lateral CA and shading retained | Untested: straight-line photograph and marked edges missing |
| Fujifilm X100VI · fixed lens | 23 mm | Expected `camera-not-in-database` refusal; no alias to X100V | Untested: no admitted camera record |
| Nikon Z 6 camera JPEG | Supplied EXIF focal | Generated JPEG acknowledgement and selection path verified | Untested: authentic camera JPEG missing |
| Perspective · keystoned building | Not applicable | Generated grid, mapping and orientation checks only | Untested: required photograph missing |

## Cross-plan rules

- **Registry order.** `linked_modules` is Crop, transform, straighten · Presets · (Pixel) · (RAW) · Basic · Tone curve · Detail · Presence · Colour mixer · Lens correction · Perspective · Vignette · (Controls). Each plan inserts relative to modules already registered; whichever lands second adjusts the array length, the doc comment and the order test in `registry/tests.rs`.
- **Compile context.** Detail threads `CompileStage { stage, full, scale }` through `ToolModule::compile`; Lens needs none, because `unit_scale` is frozen. Whichever lands second adapts the other's `compile` signatures mechanically. With the context present, Lens compile may also check the full-resolution stage.
- **Field-patch framework.** Tone curve adds curve values, Detail the Restoration shape, Lens the Geometry shape, `FieldPatch::carry` and `preset: false`. They land one at a time, each extending the framework tests.
- **Queries.** Detail routes pixel-reading queries through the point worker; Lens warp points stay on the owner.
- **Shared generated files.** The descriptor snapshot `fixtures/modules/builtin-descriptors.json`, conformance `KNOWN`, smoke `SCENARIOS`, the fixture `TABLE` in `xtask/src/fixtures.rs` and `luxforge-reference`'s `lib.rs` and `tests/studies/main.rs` are regenerated or re-added after each rebase, never hand-merged.
- **Pass kinds.** Detail uses `RenderPass::Spatial` until measured; Lens adds `RenderPass::Warp`, provisionally at `Resample`'s threshold until measured.
- **Preview.** Detail's settled-Fit and prefix cache and Lens's mapping identity on overlays both edit `app/preview.rs` and `ProxyApproximation`; they land one at a time.
- **Landing order.** Tone curve lands any time. The Lens numerics, index import, optics ledger, affine refactor and query-choice control land any time. Detail's host contracts (Restoration stage, compile context, window planner, Fit settlement) land before the Lens warp-chain and render work, which rebase on them.
- **Measurement.** Timing in all three plans runs only after feature work is complete, one plan at a time on a quiet host, never beside another plan's builds.

## Open questions

- Does the owner shoot the Z 6 in DX crop or in-camera aspect modes, and do those files carry FocalLengthIn35mmFilm? This decides whether the crop-mode check refuses or passes them.
- The index parse happens once at startup on the pool; whether a few milliseconds there fit the startup budget is measured, not assumed, because no startup budget for it is fixed.

A database camera identity can have records with different crop factors when EXIF lacks FocalLengthIn35mmFilm. The resolver retains the reviewed union of matching records; refusing such an ambiguous identity is an open proposal, not an accepted policy.
