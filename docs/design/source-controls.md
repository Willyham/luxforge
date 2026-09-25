# Source-kind controls: one Exposure and one White balance

Status: **proposal; implementation follows the owner's review of the open questions.** This is TASK-028 of the [consolidation](consolidation.md); TASK-010 implements it in wave 3, after wave 2's one plan path. The owner's decision is in [decisions](../decisions.md#architecture-review): a JPEG and a RAW photo show one Exposure control and one White balance set, each control behaves as its source requires, and a module's applicability to a source kind is declared rather than named by the desktop. The [proposals](#proposals-with-recorded-defaults) at the end are the owner's to decide.

## Outcome and scope

Every photo shows the same Basic section: White balance (Temperature, Tint, Neutral picker, As shot) and Tone with Exposure first, in the same order and with the same labels. On a RAW photo, Temperature and Tint set the source development's white balance in Kelvin and Luxforge tint units against the camera matrix. On a JPEG they set Basic's relative correction. The RAW section goes, and so do Basic's duplicate controls on RAW. Every change a control makes has the API the control sends.

In scope: descriptors, the RAW module's actions and payload, the refusals that keep one path, presets, history labels, the desktop's applicability checks and the evidence. Out of scope, with no placeholders: Auto and named white-balance modes, a control for RAW's explicit gains (they stay API-only), and any change to the frozen equations of [Basic white balance](basic-white-balance.md) or the [RAW locus](initial-raw.md#minimal-controls-and-shared-basic-integration).

## Today

| | JPEG | RAW |
| --- | --- | --- |
| Exposure | Basic `set-basic.exposure`, colour stage, maskable | RAW `set-raw-exposure.ev`, multiplied into each source pixel through `LinearSettings`, **and** Basic's, so exposure can act twice |
| White balance | Basic `set-basic.temperature`, `tint`: relative ±100, a Bradford von Kries correction of the rendered image, 2e−4 CIE 1960 uv per tint unit | RAW `set-raw-temperature.kelvin` (2000–12000 K), `set-raw-tint.tint` (±100, 1e−4 uv per unit), `use-as-shot-wb`, `reset-raw` and the sensor pick, **and** Basic's relative pair |
| Neutral picker | Basic's `neutral-sample` query through `sample-apply`, shortcut `W` | RAW's `pick-raw-neutral` through `point-pick`, shortcut `N`, and Basic's `W` |
| Applicability | | Named, not declared: `module.list` filters `luxforge.raw` by identity, and so do the desktop's section list, palette, mode shortcuts and pick gate; the JPEG recipe check names the RAW effect |

The two white balances are different operations and neither can stand in for the other. RAW's sets sensor gains before the nonlinear demosaic, which developed planes cannot undo; JPEG's corrects rendered pixels, which name no illuminant. Their scales stay distinct and nothing converts between them.

## The shape

Recommended, in four parts:

1. **Applicability is declared on effects.** `EffectDescriptor` gains `sources`, the source kinds a layer of the effect may exist on, named by the `kind` tags `asset.state` already reports (`jpeg`, `raw`); the default is every kind. The RAW effect declares `["raw"]`. A module applies to a photo when any of its effects does, or when it declares none. The host refuses an action of a module that does not apply (`validation: RAW does not apply to a JPEG photo`), admission refuses a layer on a kind its effect does not list, `module.list {asset_id}` filters by it, and the desktop's sections, palette, mode strip and pick gate read it. Every name check goes; the RAW-only invariants (one source layer at index zero, calibration equal to the original's) stay with the host's RAW source.
2. **Exposure is Basic's on every kind.** The source development stops carrying exposure: `RawPayload.exposure_ev`, `set-raw-exposure` and `LinearSettings.exposure_ev` go. On RAW, Basic's exposure multiplies the developed scene-linear planes, which the RAW path keeps unclipped to the terminal boundary ([pixel contract](initial-raw.md#pixel-and-color-contract)), before tone. It is the same operation at a later position, with only pixel-stage layers in between. [Proposal 1](#proposals-with-recorded-defaults) records the decision's wording as the alternative.
3. **White balance is one control set declared by Basic, with a RAW variant.** Basic's White balance group declares Temperature, Tint, Neutral picker and As shot once. Each of them, and the group's reset, carries a `variants` entry for `raw` naming the control the RAW module provides in that place: `set-raw.temperature` (K), `set-raw.tint`, the sensor pick and `set-raw {white-balance: as-shot}`. A variant applies on the global target of a photo of its kind; a mask target always uses the base control. The RAW module declares no controls, so it draws no section.
4. **One path per field, by a declared rule.** A Basic field whose control has a variant for kind K is refused on the global target of a K photo, naming the variant: `validation: on a RAW photo, Temperature is the source development's: set-raw temperature (K)`. Admission refuses a global Basic layer holding such a field on a K photo. So on RAW the global white balance lives only in the source layer, a masked white balance only in masked Basic layers, and exposure only in Basic layers.

### Alternatives weighed

| Shape | For | Against |
| --- | --- | --- |
| (a) Basic owns all three fields and routes them to source development on RAW, either by its plan updating the RAW layer or by compile handing the host development gains | One action for the whole section | Temperature then has two disjoint domains under one name, so the generic check, drafts and `schema.list` need the asset and the target, and a stored preset value means Kelvin or relative by its magnitude alone. Basic would read and write `RawPayload`. The compile form puts source state in a maskable colour-stage layer that sits after pixel layers |
| (b) **Recommended**: per-kind control variants over honest actions | Every parameter has one range and one unit, so `schema.list` stays static and presets unambiguous. Controls and requests stay generated from descriptors, so parity holds by construction. RAW's exact development and its approximate draft are untouched, and masks need no new rule | Basic's descriptor names the RAW module's actions, as data validated at registration |
| (c) One parameter whose range and unit vary by source kind (`temperature`: −100..100 on JPEG, 2000..12000 K on RAW) | Closest to Lightroom's single Temp slider | The domain also depends on the target (a mask on RAW is relative), which a per-kind declaration cannot say. It has the same ambiguity as (a) for presets, for history read out of context and for clients. [Masking](masking.md) refused the same shape for component kinds, because the parameter vocabulary cannot say which parameters hold for which kind |
| Exposure kept in the source development, as the decision words it | Exposure stays ahead of anything a later source stage adds, such as a camera profile's look | One Exposure API then needs routing or kind-specific preset fields. A preset's `set-basic.exposure` would be refused or doubled on RAW, and a preset captured on RAW would lose its exposure on a JPEG. It also keeps `LinearSettings` exposure and a second exposure draft path |
| Keep the RAW section and hide Basic's duplicates on RAW | Smallest change | The controls are not the same controls in the same place, which is what the owner decided |
| Variants declared by the source module (`replaces: set-basic.temperature`) | A later source module plugs in without editing Basic | There is no second source module. One table in Basic reads as each control's whole behaviour per kind |

## Semantics

### Exposure

There is one Exposure, `set-basic.exposure`: −5..+5 EV, `2^EV` in linear light, on every kind and every target, and it acts once. On RAW it moves from the source read into the colour run, so it now follows pixel-stage layers; later retouching then does not depend on exposure. The source read multiplies in f64 and Basic's unit applies an f32 gain, so the move is byte-identical only where `2^EV` is exact. [Verification](#verification) measures the bound before the change lands.

### White balance on a RAW photo

- Temperature (2000–12000 K) and Tint (±100 Luxforge units) set a custom white balance through `set-raw`, a field patch. The field not sent keeps the white balance in force, exactly as the two actions do today ([initial RAW](initial-raw.md#minimal-controls-and-shared-basic-integration)).
- As shot is `set-raw {white-balance: as-shot}`, the camera's as-shot gains. It now returns the payload to the Original's development and drops the custom values a return used to keep. Nothing reads them, since the next change starts from the as-shot equivalent. `is_neutral` becomes "equal to the Original's development", and `reset-raw` and `use-as-shot-wb` go.
- A double-click on Temperature or Tint and the White balance group's reset run As shot.
- The controls show the white balance in force. Under As shot and after a pick, that is the equivalent temperature and tint the core solves (`RawPayload::white_balance_controls`), as today.
- The Neutral picker is the sensor pick, unchanged: a 13×13 pre-white-balance patch that sets custom gains.
- A Temperature or Tint drag drafts `set-raw`. The preview approximates on the developed planes and redevelops on release, as described in [instant previews](instant-preview.md#a-raw-white-balance-during-a-drag). That path compares the RAW layer's gains with the developed ones and does not depend on which action changed them.

### White balance on a JPEG

- Temperature and Tint are Basic's relative ±100 correction, unchanged.
- As shot means the file's own rendering: Temperature 0 and Tint 0, which is the White balance group's reset. It recovers no camera white balance and shows no Kelvin. The As shot button is new on JPEG. It needs the action-control rule relaxed from exactly one preset field to at least one, since a group reset already holds several.
- A double-click on either field returns that field to 0. The picker is the 5×5 Bradford solve and drafts are exact, as today.

### Masks on either kind

A masked White balance is Basic's relative correction, and a masked Exposure is Basic's, applied on top of the global development. Lightroom's local Temp and Tint are also relative on RAW. The RAW effect is not maskable, and nothing here creates a masked Kelvin. On RAW, the masked picker still reads Basic's input stage as rendered bytes, as it does today.

### Picker and shortcut

There is one Neutral picker control and one shortcut, `W`. On the global target of a RAW photo, the control and `W` enter the RAW module's sensor-pick mode; on a JPEG, and on a mask of either kind, they enter Basic's. RAW's `N` goes, and the mode strip lists the pick modes the resolved controls name. `query.neutral-sample` keeps answering on RAW, because Basic's picker on a mask uses it.

## Descriptor, API and history

A control (number, action or picker) and a group reset may carry `variants`:

```json
{"kind": "number", "action": "set-basic", "parameter": "temperature", "label": "Temperature",
 "rail": "temperature",
 "variants": [{"source": "raw", "module": "luxforge.raw",
               "control": {"kind": "number", "action": "set-raw", "parameter": "temperature",
                           "label": "Temperature", "rail": "temperature",
                           "reset": {"action": "set-raw", "preset": {"white-balance": "as-shot"}}}}]}
```

Registration checks, once the registry is complete, that each variant names another registered module that applies to the variant's kind, and a control of the base's shape over that module's own actions. It allows at most one variant per kind. One core function resolves a control for a photo and a target, and the desktop calls it. `module.list` returns the declarations unresolved, and `asset.state` gives the kind. `schema.list` lists each method's `sources`, and each superseded parameter's `superseded: [{source: "raw", by: "set-raw.temperature"}]`, so an agent learns the refusal without trying it.

The RAW module's actions:

| Before | After |
| --- | --- |
| `set-raw-exposure {ev}` | Removed; `set-basic {exposure}` on every kind |
| `set-raw-temperature {kelvin}`, `set-raw-tint {tint}` | `set-raw {temperature?, tint?, white-balance?}`, `patch: true` |
| `use-as-shot-wb`, `reset-raw` | `set-raw {white-balance: "as-shot"}` |
| `pick-raw-neutral {x, y}` | Unchanged, `point-pick`, with no shortcut of its own |
| `set-raw-red-gain`, `set-raw-blue-gain` | Unchanged, API only |

`white-balance` is `as-shot` or `custom`. `temperature` and `tint` imply `custom`, which may be sent beside them. `as-shot` beside either of them is refused (`validation: As shot takes no temperature or tint`), and so is `custom` alone. `values` reports `white-balance`, `temperature` and `tint`, the last two as the equivalent under As shot.

History labels use the same words for the same control on both kinds: `Temperature 5500 K` or `Temperature +20`, `Tint +12`, `Exposure +0.50 EV`, and `Reset White balance` for As shot and the group reset. RAW's `label` follows the field-patch rules. A patch of exactly one group's non-default fields reads as the group, so either kind's pick is recorded as `White balance` rather than `Basic (2 fields)` or `Pick neutral patch`. `recipe.describe` reads the RAW row as `As shot` or `Temperature 5500 K · Tint +12`.

Resets and the edited dot: on RAW, the Basic module reset also returns the development to As shot ([proposal 7](#proposals-with-recorded-defaults)). It is a `Compose` of `set-basic` at its defaults and each variant group reset, recorded as one entry labelled `Reset Basic`. The section's dot reads the neutrality of every layer its resolved controls edit, so on RAW a custom white balance lights it too. `StageContext` carries the photo's source kind, so plans and the refusal read it instead of inspecting layer zero.

## Presets

Native presets carry each kind's white balance separately, and exposure once:

- `set-raw` is a field patch, so it is presettable. A capture reads a new `ToolModule::settings`, which defaults to `values`. The RAW module narrows it to `{white-balance: as-shot}` under As shot, so a preset made at As shot applies each photo's own camera white balance rather than this camera's equivalent Kelvin. A custom or picked white balance captures `{temperature, tint}`.
- The create form's `Basic · White balance` checkbox captures the resolved controls' fields: `set-raw` on RAW, `set-basic.temperature` and `tint` on JPEG. It stays unchecked by default.
- Apply skips a step whose module does not apply to the photo, and a field superseded on its global target. The result lists them under `skipped`, and the status bar says, for example, "1 setting does not apply to a RAW photo". A preset with nothing applicable is a no-op. Today any refused step refuses the whole preset. A skip is not a refusal, because a preset may legitimately carry both kinds' white balance, as Lightroom's may.
- No preset converts between Kelvin and relative, in either direction.

Lightroom import, the rows of the [mapping](presets.md#mapping) that change:

| Lightroom setting | Target | Rule |
| --- | --- | --- |
| `Exposure2012` | `set-basic.exposure` | Value transfer, now applying to RAW as well |
| `IncrementalTemperature`, `IncrementalTint` | `set-basic.temperature`, `tint` | Value transfer; skipped on RAW at apply |
| `Temperature`, `Tint` | `set-raw.temperature`, `tint` | [Proposal 5](#proposals-with-recorded-defaults): converted together through the illuminant chromaticity they name, and refused together when either result is outside 2000–12000 K or ±100. Refused, as today, until the owner adopts it |
| `WhiteBalance` | `As Shot`: `set-raw.white-balance: as-shot`, and `set-basic` Temperature and Tint 0 when the preset holds no incremental value. `Custom`: neutral when the values it names are mapped, otherwise refused with them | `Auto` and named modes stay refused |

The conversion is camera-independent. Lightroom's pair defines a white through the DNG SDK's `dng_temperature`: a correlated colour temperature on the Planckian locus and a perpendicular offset, reportedly −3000 tint units per unit of CIE 1960 uv. The constants must be confirmed from the SDK source and recorded in the [Lightroom preset research](../research/lightroom/presets.md) before implementation. Luxforge then solves its own locus for the same uv with the inverse it already has. This converts values; it does not match renderings, because Luxforge turns that white into gains through LibRaw's camera matrix, not Adobe's profile.

## Existing data

Current shapes only; nothing is migrated.

- **RAW effect format 1 → 2.** The payload loses `exposure_ev`, and As shot is canonical. A RAW recipe written before is refused explicitly (`incompatible: unsupported effect format 1`), with its history and rows readable and unchanged. JPEG assets in the same catalog keep working. RAW photos edited before the change need a new catalog.
- **API.** Removed: `edit.set-raw-exposure`, `edit.set-raw-temperature`, `edit.set-raw-tint`, `edit.use-as-shot-wb` and `edit.reset-raw`, which now return `validation: unknown action`. Added: `edit.set-raw`. Descriptors gain `sources` and `variants`, and the RAW descriptor loses its controls and its shortcut.
- **Unchanged:** catalog format 9, because no table changes; recipe format 2; the Basic effect format 1, because its payload keeps its meaning and no format-2 RAW recipe can hold a superseded field; and preset document version 1, because the `set-basic` fields keep their meaning and `set-raw` is new.

## Verification

Exact tests, in `luxforge-core` and `luxforge-app`:

- **Descriptors.** Basic's variants validate. A variant naming an unknown action, a module that does not apply to its kind, a control of another shape, or a second variant for one kind is refused. Every other descriptor is unchanged, compared before and after, as TASK-010 requires.
- **Applicability.** Every RAW action is refused on a JPEG by the declared rule. `set-basic` Temperature or Tint on a RAW global target is refused naming `set-raw`, and is accepted on a RAW mask. Exposure is accepted everywhere. Admission refuses a RAW recipe whose global Basic layer holds a superseded field, and a JPEG recipe holding a RAW layer, without naming either module.
- **`set-raw`.** The current RAW plan tests are ported: the other field kept in force from As shot, from a pick and from a custom pair, and the 6504 K and 0 fallback. Also tested: As shot equal to the Original's payload, the refused `white-balance` combinations, no-ops, labels, `values` and `settings`.
- **Exposure move, measured before it lands** with today's code, on the three supplied RAW files: a development at `ev` against one at 0 EV plus Basic `exposure: ev`, at proxy and full size, for EV −2.37, −0.5, +0.01, +1 and +3.3. Record the largest code difference and the share of pixels more than one code apart in [performance](../specs/performance.md). Integer EV should be byte-identical, since both sides multiply by an exact power of two. This measurement is the evidence for proposal 1.
- **Drafts.** The approximate white-balance tests move to `set-raw`, and an Exposure drag on RAW renders exactly with no redevelopment.
- **Presets.** The importer rows above, skip reporting, and capture under As shot, custom and a pick. The JSON CLI parity test gains a RAW source.
- **Parity.** A desktop state test derives the Basic section for a JPEG, a RAW global target and a RAW mask target. It checks that labels and order are identical, that units and ranges differ only where a variant declares them, and that each control's request equals the core's resolution. A test fails if desktop code outside tests names `luxforge.raw`.

Rendered evidence:

- `basic-panel` (JPEG, `rendered` tier) adds As shot and asserts the White balance group's four controls.
- `raw-panel` (a supplied RAW, `full` tier with `--manifest`) shows the Basic section in place of the RAW section. A Temperature drag is drafted approximate and committed exact, with the tint kept. Double-clicks on Temperature and Tint land on As shot, labelled `Reset White balance`, and on Exposure at 0 EV. The section's dot follows the development, and `W` enters the sensor pick. Its crop part is unchanged.
- `timing` runs an Exposure drag on RAW before and after, because exposure moves from the source read into the colour run. The finished change answers the [performance-rules](../engineering/performance-rules.md) checklist.

Documentation changes with the implementation: [initial RAW](initial-raw.md), [modules and API](modules-and-api.md), [presets](presets.md), [instant previews](instant-preview.md) for the action names, [feature status](../features.md), the [user guide](../user-guide.md) and the scenario rows in [development](../engineering/development.md).

## Proposals with recorded defaults

Recommendations for the owner, recorded as proposals until decided. Implementation waits for the owner's review.

| Question | Recommended default | Alternatives |
| --- | --- | --- |
| 1. Where does Exposure live on RAW? | Basic's colour stage on every kind; the source development carries none | The decision's wording: source-development exposure, with `set-basic.exposure` routed to it on RAW, or kind-specific preset fields |
| 2. Masked white balance on RAW | Relative (Basic's ±100), as Lightroom's local Temp and Tint are | Kelvin through a matrix approximation against the global development, which would move whenever the global white balance moves |
| 3. JPEG scale and range | Keep the frozen relative ±100 Temperature and Tint, with As shot at 0 and 0. Basic's tint unit (2e−4 uv) stays twice RAW's (1e−4) | Re-freeze Basic's tint to RAW's unit; a Kelvin-like readout for JPEG, which has no illuminant to name |
| 4. RAW ranges | Keep 2000–12000 K and ±100 Luxforge tint, validated against the camera matrices | Lightroom's 2000–50000 K and ±150, which needs a new study, because some matrices then give invalid whites or gains past the limit ([slider audit](../research/lightroom/slider-parity.md#preset-implication)) |
| 5. Lightroom `Temperature` and `Tint` import | Convert both through the chromaticity they name, refusing out-of-range pairs; a value conversion, not a rendering match | Keep refusing until a per-camera calibration exists, which is the current preset default |
| 6. Presets across kinds | Store each kind's white balance separately; apply skips and reports what does not apply | Refuse a preset that holds a setting for another kind |
| 7. Reset Basic on a RAW photo | Also returns the development to As shot, since the section shows its white balance | Reset Basic's layer only, leaving As shot as a separate reset |
