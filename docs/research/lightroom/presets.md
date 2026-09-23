# Preset file formats

[Knowledge base index](README.md) · Evidence checked 2026-09-23.

This chapter describes how Lightroom Classic and Camera Raw store develop presets, so an importer can read them without guessing. It is a format reference, not a claim about how Adobe renders the settings. Lightwell's importer contract is in the [presets design](../../design/presets.md).

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

## Field inventory for Lightwell's controls

| Lightroom setting | Range | Evidence |
| --- | --- | --- |
| `Exposure2012` | −5..+5 EV | Provisional: the public help gives no bound ([slider audit](slider-parity.md)) |
| `Contrast2012`, `Whites2012`, `Blacks2012`, `Texture`, `Clarity2012`, `Dehaze`, `Vibrance` | −100..+100 | Provisional |
| `Highlights2012`, `Shadows2012`, `Saturation` | −100..+100 | D (S17, S16) |
| `HueAdjustment*`, `SaturationAdjustment*`, `LuminanceAdjustment*` for the eight ranges Red, Orange, Yellow, Green, Aqua, Blue, Purple and Magenta | −100..+100 | Provisional. The names and families match the Color Mixer (F, S44) |
| `PostCropVignetteAmount` and `Roundness` / `Midpoint` and `Feather` | −100..+100 / 0..100, defaults 0, 0, 50 and 50 | Provisional. The fields are distinct from the lens-correction `VignetteAmount` and `VignetteMidpoint` |
| `IncrementalTemperature`, `IncrementalTint` | −100..+100 | Relative white balance for rendered files. The Temperature bound is D (S16); Tint is provisional |
| `Temperature`, `Tint` | 2,000..50,000 K, −150..+150 | D (S53). These apply to RAW files only, and the scale differs from Lightwell's RAW controls ([slider audit](slider-parity.md)) |

**U.** No source describes what Lightroom does to `Temperature` and `Tint` when a preset saved from a RAW photo is applied to a JPEG. Adobe says a preset tied to a RAW-only profile does not work on a JPEG (S54), and it calls presets whose settings cannot all be applied *partially compatible* (S54, S55). Do not guess a conversion between Kelvin and incremental values.

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

- **D.** The preset dialog chooses which settings to include (S55). Applying a preset overwrites the settings it contains and leaves every other one unchanged. This is consistent across sources and is how Lightwell's field patches already behave.
- **D.** Presets that cannot be fully applied appear faded and italic in the Develop presets panel. The Show Partially Compatible Develop Presets preference controls this (S55).
- **D.** The Amount slider works only for presets saved with Support Amount Slider (S55). Community experts describe it as scaling each included setting between its neutral value and the preset's value, so 50% of +0.5 EV is +0.25 EV and 0% writes the neutral values. It is not a blend with the photo's previous values. Its upper bound (100% or 200%) is **U**.
- **U.** No source quotes the history-step text for a preset applied in Develop. Lightwell's `Preset: <name>` label is its own choice.
- **D.** Imported presets land in the User Presets group (S55).
