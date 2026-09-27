# Preset file formats

[Knowledge base index](README.md) · Evidence checked 2026-09-23.

This chapter describes how Lightroom Classic and Camera Raw store develop presets, so an importer can read them without guessing. It is a format reference, not a claim about how Adobe renders the settings. Luxforge's importer contract is in the [presets design](../../design/presets.md).

Besides the usual labels, **F** marks a structural fact observed in a real preset file (S49–S51). Where a fact rests only on the ExifTool tag reference (S52), that is said.

## Adobe's namespace page is out of date

**D.** The namespace URI is `http://ns.adobe.com/camera-raw-settings/1.0/`, with preferred prefix `crs` (S53). **U.** Adobe's published namespace page still lists only the pre-2012 field set: `Exposure`, `Contrast`, `Brightness`, `Shadows`, the legacy `ToneCurve` and the lens `VignetteAmount`. It has none of `ProcessVersion`, `PresetType`, `UUID`, the `*2012` tone fields, `Texture`, `Dehaze`, `PostCropVignette*` or the HSL adjustments, though all of them have shipped for years. The current field inventory therefore comes from the ExifTool tag reference (S52), checked against real files. It is not an Adobe specification.

## XMP develop presets

Lightroom Classic 7.3 and later store presets as `.xmp` files.

**F.** The file is `x:xmpmeta` → `rdf:RDF` → one `rdf:Description`. Every scalar setting is an attribute on that element, including the newer fields (`crs:ProcessVersion="11.0"`, `crs:Texture="30"`, `crs:HueAdjustmentRed="0"`). Localized strings are child elements holding `rdf:Alt` → `rdf:li xml:lang="x-default"`: `crs:Name`, `crs:ShortName`, `crs:SortName`, `crs:Group` and `crs:Description` (S49; ExifTool types all five as lang-alt, S52). `crs:Cluster` is a plain attribute. Tone curves (`ToneCurvePV2012` and its `Red`, `Green` and `Blue` siblings) are child elements holding an `rdf:Seq` of `"x, y"` text items on a 0–255 scale.

**F.** A develop preset can contain a nested `crs:Look` element: an `rdf:Description` with `Name`, `Amount`, `UUID`, `Supports*` and `Stubbed="true"`. This is a reference to the profile that was active when the preset was saved, for example `Adobe Color`. It does not make the file a profile.

**Preset metadata.** These fields are not settings. `PresetType` is `Normal` for a develop preset (F) and `Look` for a creative profile (reported consistently by several sources; no profile file was examined byte by byte). `UUID` is 32 uppercase hex digits (F). `Version` is the writing engine's version (F). `SupportsAmount`, `SupportsColor`, `SupportsMonochrome`, `SupportsHighDynamicRange`, `SupportsNormalDynamicRange`, `SupportsSceneReferred` and `SupportsOutputReferred` are compatibility flags (S52). The remaining metadata fields are `CameraModelRestriction`, `Copyright`, `ContactInfo`, `HasSettings` and `RequiresRGBTables`. The last one marks a profile that carries an RGB lookup table.

## Value encoding

| Kind | XMP | `.lrtemplate` |
| --- | --- | --- |
| Numbers | Decimal text such as `"0.5"`, `"30"` or `"-76"`. No leading `+` appeared in the files examined, but whether Lightroom ever writes one is **U**, so accept both | Bare Lua numbers such as `0.5` or `-15` (F) |
| Booleans | `"True"` or `"False"` for most fields. Some on/off fields are the integers `"0"` and `"1"` instead, for example `AutoLateralCA` and `LensProfileEnable` (F) | `true` and `false` (F) |
| Choices | Quoted text, for example `WhiteBalance="Custom"` (F) | Quoted Lua string (F) |
| Curves | `rdf:Seq` of `"x, y"` items (F) | Flat interleaved array `{ 0, 0, 32, 22, …, 255, 255 }` (F) |

**Types (S52).** `Exposure2012` and `Dehaze` are reals. `Contrast2012`, `Highlights2012`, `Shadows2012`, `Whites2012`, `Blacks2012`, `Texture`, `Clarity2012`, `Vibrance`, `Saturation`, the 24 HSL adjustments, the post-crop vignette fields, `Temperature`, `Tint`, `IncrementalTemperature` and `IncrementalTint` are integers.

**Choice values (S52).** `WhiteBalance` takes `As Shot`, `Auto`, `Cloudy`, `Custom`, `Daylight`, `Flash`, `Fluorescent`, `Shade` or `Tungsten` (Adobe's page agrees, S53). `PostCropVignetteStyle` is 1 Highlight Priority, 2 Color Priority or 3 Paint Overlay. `PerspectiveUpright` is 0 Off, 1 Auto, 2 Full, 3 Level, 4 Vertical or 5 Guided.

## Field inventory for Luxforge's controls

| Lightroom setting | Range | Evidence |
| --- | --- | --- |
| `Exposure2012` | −5..+5 EV | Provisional: the public help gives no bound ([slider audit](slider-parity.md)) |
| `Contrast2012`, `Whites2012`, `Blacks2012`, `Texture`, `Clarity2012`, `Dehaze`, `Vibrance` | −100..+100 | Provisional |
| `Highlights2012`, `Shadows2012`, `Saturation` | −100..+100 | D (S17, S16) |
| `HueAdjustment*`, `SaturationAdjustment*`, `LuminanceAdjustment*` for the eight ranges Red, Orange, Yellow, Green, Aqua, Blue, Purple and Magenta | −100..+100 | Provisional. The names and families match the Color Mixer (F, S44) |
| `PostCropVignetteAmount` and `Roundness` / `Midpoint` and `Feather` | −100..+100 / 0..100, defaults 0, 0, 50 and 50 | Provisional. The fields are distinct from the lens-correction `VignetteAmount` and `VignetteMidpoint` |
| `IncrementalTemperature`, `IncrementalTint` | −100..+100 | Relative white balance for rendered files. The Temperature bound is D (S16); Tint is provisional |
| `Temperature`, `Tint` | 2,000..50,000 K, −150..+150 | D (S53). These apply to RAW files only, and the scale differs from Luxforge's RAW controls ([slider audit](slider-parity.md)) |

**U.** No source describes what Lightroom does to `Temperature` and `Tint` when a preset saved from a RAW photo is applied to a JPEG. Adobe says a preset tied to a RAW-only profile does not work on a JPEG (S54), and it calls presets whose settings cannot all be applied *partially compatible* (S54, S55). Do not guess a conversion between Kelvin and incremental values.

## RAW Temperature/Tint conversion

The [source-kind controls](../../decisions.md#source-kind-controls) decision converts a preset's
RAW `Temperature`/`Tint` pair (`crs:Temperature`, `crs:Tint`) to Luxforge's own RAW Temperature
and Tint through the illuminant chromaticity both pairs name, refusing the pair together when the
result falls outside Luxforge's 2,000–12,000 K / ±100 tint range. This section records the
confirmed constants; the implementation is
[`crates/luxforge-core/src/modules/raw/lightroom_white_balance.rs`](../../../crates/luxforge-core/src/modules/raw/lightroom_white_balance.rs).

**D.** Lightroom's pair is Adobe's `dng_temperature` class, whose source is openly published in
the Adobe DNG SDK. Checked against `dng_temperature.cpp`
(`$Id: //mondo/dng_sdk_1_4/dng_sdk/source/dng_temperature.cpp#1 $`,
`$DateTime: 2012/05/30 13:28:51 $`, Copyright 2006 Adobe Systems Incorporated), read from the open
mirror <https://raw.githubusercontent.com/aizvorski/dng_sdk/master/source/dng_temperature.cpp>
(also mirrored at
<https://android.googlesource.com/platform/external/dng_sdk/+/master/source/dng_temperature.cpp>).
The accompanying `LICENSE` in that mirror is Adobe's own DNG SDK agreement: a royalty-free grant
to use, reproduce, modify and distribute the Software, conditioned on keeping the copyright
notice; that grant and notice are carried into Luxforge's source comment, with no claim that a
formal license audit was done (AGENTS.md, "Manual license reviews are deferred").

- **The table.** `kTempTable` is 31 rows of `(r, u, v, t)`: `r` is reciprocal megakelvin
  (`1.0e6 / kelvin`), `u` and `v` are the isotemperature line's point on the Planckian locus in
  **CIE 1960 `uv`**, and `t = dv/du` is that line's slope. The comment in the SDK credits the
  table to Wyszecki & Stiles, *Color Science*, second edition, page 228. Rows run `r = 0, 10, 20,
  …, 100` (step 10) then `r = 125, 150, …, 600` (step 25); `r = 600` is `1,666.67` K, `r = 0` is an
  infinitely hot blackbody.
- **Forward map (`Get_xy_coord`).** For a temperature, `r = 1.0e6 / kelvin` locates the bracketing
  pair of table rows by ascending scan (the last row is used past the table's end, an
  extrapolation Luxforge's converter never reaches because it only receives temperatures inside
  Lightroom's own declared 2,000–50,000 K). The two rows' `(u, v)` and unit slope vectors
  `(1, t)/‖(1, t)‖` are linearly interpolated by `r`'s fractional position between them (a mired,
  not Kelvin, interpolation), giving the temperature's own point on the locus and its
  isotemperature direction. Tint is added along that direction: `offset = tint / kTintScale`,
  `(u, v) += (offset · unit_slope)`.
- **`kTintScale = -3000.0`** (S64: matches the community-reported "3000 × Duv", with Adobe's sign
  making positive Tint move toward `−v`, opposite Luxforge's own positive-tint-is-magenta,
  +`v`-ward convention). The scale is a plain division in `uv` space, not a percentage or a
  camera-relative unit, so Lightroom's ±150 Tint is exactly ±0.05 `uv` (`150 / 3000`).
- **What the SDK does not state.** Neither `dng_temperature.cpp` nor its header declares the
  2,000–50,000 K / ±150 range; that is Lightroom's UI and namespace-schema restriction ([field
  inventory](#field-inventory-for-luxforges-controls)), not a limit the table or the interpolation
  enforce. The table's own `r` domain (`0..600`) covers roughly `1,667` K upward without a fixed
  upper bound.
- **Accuracy check.** [`lightroom_white_balance.rs`](../../../crates/luxforge-core/src/modules/raw/lightroom_white_balance.rs)'s
  tests check the Rust transcription two ways: against a second, independently-shaped
  reimplementation of the same interpolation (differently ordered table walk, `1/r` kept instead
  of `r`), agreeing to `1e-9` in `xy` over a grid spanning Lightroom's declared domain; and against
  CIE Illuminant A's published chromaticity (`x = 0.4476, y = 0.4074`, the same Wyszecki & Stiles
  source), which the table reproduces at its nominal `2,856` K, tint 0 to within `2e-4` in `x` and
  `y`. D65 (`x = 0.3127, y = 0.3290`) is checked only loosely (`0.01`): D65 is not on the
  Planckian locus the table approximates, so Adobe's `6,504` K, tint 0 is expected to land *near*
  D65, not on it, and this is not a claim that the two coincide.

**Luxforge's own solve.** Luxforge's inverse locus search
([`temperature_tint_from_uv`](../../../crates/luxforge-core/src/modules/raw/white_balance.rs),
factored out of the gains inverse `temperature_tint_from_gains` without changing that function's
results) takes the `uv` the DNG table names and finds the temperature and tint on *Luxforge's own*
blackbody/daylight-blend locus that reach the same `uv`, refusing with `out-of-range: ...` when
none does within its stated tolerance — in particular whenever the answer would sit outside
2,000–12,000 K or ±100 tint. This is a **value conversion**: Luxforge's own locus is a different
numerical approximation (a Planckian/daylight blend over 3,800–4,500 K, not Adobe's Robertson
table) from Lightroom's, and the result is turned into sensor gains through LibRaw's camera
matrix, never Adobe's DNG colour pipeline, so an imported preset changes the assumed illuminant,
not an attempt to reproduce Lightroom's rendering. Sample conversions (frozen in the crate's
tests, `matches_the_independent_reference_over_lightrooms_declared_domain` and
`luxforge_answer_reproduces_the_lightroom_white_through_a_camera_matrix`):

| Lightroom `Temperature`/`Tint` | Luxforge `Temperature`/`Tint` |
| --- | --- |
| 5500 K, +10 | 5501.872 K, −0.5716 |
| 3200 K, 0 | 3208.192 K, +0.3160 |
| 7500 K, −20 | 7520.251 K, −99.3120 |

The tint numbers do not track Lightroom's own sign or magnitude one-for-one: Lightroom's Tint is
an offset along *its* isotemperature line at *its* temperature, in units of `uv/3000`; Luxforge's
is an offset along a different locus's normal at a (slightly different) solved temperature, in
units of `1e-4` `uv`. A pair carries the same illuminant, not the same two numbers.

A pair whose result falls outside Luxforge's ranges is refused together, for example
`lightroom_to_luxforge(50_000.0, 0.0)`, which asks for a temperature Luxforge's locus does not
reach.

## Process versions

**D.** Adobe names the process versions 2003, 2010, 2012, 5 and 6, and never upgrades a photo's process version silently (S07). **F** establishes these `ProcessVersion` strings: `6.7` is Process 2012 (a template using the `*2012` fields) and `11.0` is Process 5 (S49). Community sources give `5.0` for 2003, `5.7` for 2010, `10.0` for Process 4 and `15.4` for Process 6; those four are unverified.

**F.** A preset from an earlier process version uses `Exposure` (−4..+4), `Contrast` (−50..+100), `Brightness` (0..150), `Shadows` (0..100), `FillLight`, `HighlightRecovery`, `Clarity` and the legacy `ToneCurve`, with no `ProcessVersion` key (S50). Their domains differ from the `*2012` fields: legacy `Shadows` is an unsigned lift and `Shadows2012` is signed. Copying one onto the other is wrong even before the algorithm changes. Under Process 2012 and later these fields do not take part in rendering.

## Legacy `.lrtemplate` presets

Lightroom Classic 7.2 and earlier store presets as a Lua table assignment (F, S50, S51):

```lua
s = {
	id = "41A00AE2-F71F-40DA-968B-91CE82FF0A48",
	internalName = "night_factory_2",
	title = "night_factory_2",
	type = "Develop",
	value = {
		settings = {
			Clarity2012 = 40,
			Contrast2012 = 30,
			ProcessVersion = "6.7",
			ToneCurvePV2012 = { 0, 0, 255, 255, },
			WhiteBalance = "Custom",
		},
		uuid = "83C3EAE3-1E07-4D0C-84E5-ECAA759F976F",
	},
	version = 0,
}
```

- `id` is optional and newer. `internalName`, `title`, `type`, `value.settings`, `value.uuid` and `version` are always present. GUIDs are dashed, unlike the XMP `UUID`.
- Settings are flat `Name = value` pairs and use the same names as XMP. Trailing commas are normal.
- Lightroom resource files use `ZSTR "$$$/Key=Default text"` for localized strings. Neither develop template examined used one for its title, so whether develop templates ever do is **U**. An importer should accept them anyway.
- Older templates carry panel switches such as `EnableColorAdjustments`, `EnableDetail`, `EnableSplitToning` and `EnableCalibration`. A `false` switch turns that panel off, so the panel's values are not in effect. The switch names follow the Lightroom SDK's develop settings; the complete list is **U**.
- **U.** No Adobe source describes how Lightroom 7.3 converted templates to XMP. The shared field names suggest a direct re-serialization.

## What a preset can contain

- **Crop: never.** Several independent sources, and an open Adobe feature request to add it, agree that develop presets cannot include crop (S56). `Crop*` and `HasCrop` appear in photo sidecars, which describe one photo's state.
- **Masks: yes.** The preset dialog has a Masking section, so `MaskGroupBasedCorrections` and the older correction containers can arrive inside an ordinary preset (S57; the Lightroom mobile help lists Masking as a preset category that is excluded by default, S58).
- **Profiles and looks** are separate `.xmp` files marked `PresetType="Look"`. Camera profiles (`.dcp`) are binary TIFF-structured files and not XMP at all.
- **DNG presets** are ordinary DNG photographs whose embedded XMP carries the settings. Lightroom mobile turns one into a preset by opening it and choosing Create Preset (S58). Nothing in the file marks it as a preset.
- **Photo sidecars** use the same `crs` attributes beside the `exif`, `tiff` and `dc` namespaces. They add per-photo fields such as `RawFileName`, `AlreadyApplied` and the crop fields, and have no `Name` or `PresetType`.

Lightroom Classic keeps user presets in `~/Library/Application Support/Adobe/CameraRaw/Settings` on macOS and `%APPDATA%\Adobe\CameraRaw\Settings` on Windows (S54).

## Applying a preset in Lightroom

- **D.** The preset dialog chooses which settings to include (S55). Applying a preset overwrites the settings it contains and leaves every other one unchanged. This is consistent across sources and is how Luxforge's field patches already behave.
- **D.** Presets that cannot be fully applied appear faded and italic in the Develop presets panel. The Show Partially Compatible Develop Presets preference controls this (S55).
- **D.** The Amount slider works only for presets saved with Support Amount Slider (S55). Community experts describe it as scaling each included setting between its neutral value and the preset's value, so 50% of +0.5 EV is +0.25 EV and 0% writes the neutral values. It is not a blend with the photo's previous values. Its upper bound (100% or 200%) is **U**.
- **U.** No source quotes the history-step text for a preset applied in Develop. Luxforge's `Preset: <name>` label is its own choice.
- **D.** Imported presets land in the User Presets group (S55).
