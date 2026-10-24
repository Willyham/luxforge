# AI tools in the Develop workspace

Status: **decided, not authorized.** The owner settled the four product questions on 2026-10-24 ([Decisions](#decisions)); everything else here is a recorded default the owner can revise. Nothing below is built unless it says so. Its plan is [tasks/editing/ai-tools-workspace.json](../../tasks/editing/ai-tools-workspace.json).

This design turns the AI editing that is delivered behind two experiments into a feature a person finds, sets up and uses without Settings. It owns what the person sees and does: the **Remove** and **Object** kinds of the Corrections band, **Object** in the Masks panel's New mask menu, the setup of every model a tool needs, and how Settings › Models lists them. What a selection, a fill, a patch, the picker and the analysis *are* stays [AI editing](ai-editing.md)'s; the band, its pins and its keys stay the [Corrections workspace](corrections-workspace.md)'s; consent, resources and jobs stay [module capabilities](module-capabilities.md)'.

Board: [ai-tools-workspace/board.html](ai-tools-workspace/board.html): the Object tool ready, selecting and the loose-drag pins, the Object group through every setup state, the combined setup sheet, mask Object picking with the stored selection's row, and Settings › Models.

## What changes

| Today (delivered) | Proposed |
| --- | --- |
| Remove appears only with the `ai.generative` experiment; hover picking only with `ai.analysis` too, and a press still paints | No experiment: Remove and Object are listed wherever the run can run their fill, and mask Object wherever it can run the segmenter |
| One Remove kind that paints, with a hover highlight that does nothing | Two kinds. **Remove** paints exactly what to fill, with no hover. **Object** points: hover outlines, a click selects, a loose drag finds |
| The segmenter installs only through `module.resource.install` from an API client | Each tool sets itself up on first use, from its own group, through one sheet for everything it lacks |
| A setup sheet per resource download | A sheet per **setup**: a module-declared group of resources a tool needs, installed under one answer |
| `query.pick-object` and `task.select-object` reach no gesture | Object removal fills a set of picked objects; New mask › Object makes a stored `model` selection with Recompute |
| Settings › Models lists resources | Lists each setup's parts, captioned with the tools that use them |

## Principles

- **A tool says what it needs where it is used.** Choosing Remove, Object or New mask › Object without its models shows that tool's setup in its own group, in the feature's words, with the one download that makes it work. Nobody has to visit Settings to start.
- **One answer, one download, one bar.** A setup that needs several resources asks once, grants exactly each resource's download scope, and shows one progress bar over the parts.
- **Painting and pointing are separate tools.** Remove's press always paints the area to fill; Object's press always picks. Neither guesses which the person meant.
- **Every gesture is a request any client can send.** The Object tool's selection is the `task.select-object` artifacts an agent would publish; its fill is `task.fill-generative` naming them.
- **Honest about what is not available.** A tool whose download is not published yet, or whose model this machine cannot run, says so in its group and offers no button that would fail.

## The Corrections band

The kind control has four segments: **Heal** | **Clone** | **Remove** | **Object**, with one line under it naming the chosen kind (`Paint over what to remove`, `Point at an object to remove it`). Remove and Object are listed when the run offers a fill tier whose runtime this host has; a host with no runtime for any tier (Windows today, which compiles no generative runtime) lists neither, since nothing the person does could make them work. A machine below the fill model's minimum lists them and their groups say why they cannot run. A segment whose setup has not run carries a small download glyph.

Under Remove and Object the band shows **Size** and the kind's group; the lower half of the group (Fill, the prompt, Generate, Regenerate, the candidate strip and Accept) is the delivered Remove group, shared by both kinds. Switching between Remove and Object clears the selection, as starting a new stroke does today; candidates already held stay in the strip.

### Remove

Exactly the delivered workflow ([AI editing](ai-editing.md#the-delivered-workflow)) without the hover: a press paints the selection with the Corrections brush, `⇧`-click draws a straight stroke, a new stroke replaces the selection. The hover highlight moves to Object.

### Object

There is no brush in hand. Over the photograph the cursor is a crosshair, and the object under it is washed white inside a white outline over a dark under-stroke: the delivered hover, through `query.pick-object {point}` with one request in flight and the latest position asked next.

- **Entering.** Choosing Object with its setup installed loads the segmenter (`module.runtime.load` for the encoder and the decoder) and runs `task.analyse` for the photograph on screen at once, rather than on the first refused pick, so hover is ready when the pointer arrives. Until the analysis answers, a chip at the top of the canvas and the group read "Finding objects…"; Remove can paint meanwhile. Opening another photograph analyses it the same way.
- **Click** (the pointer moves less than 3 pt): with an object outlined, sends `task.select-object {asset_id, point}` and makes the published artifact the whole selection. `⇧`-click adds the object to the selection; `⌥`-click on a selected object takes it out (a hit test against the held pick previews, view state, no request). A click with nothing outlined does nothing but say `Nothing to select here` in the status bar.
- **Loose drag** (3 pt or more): paints a dashed stroke at the band's Size. On release the desktop asks `query.pick-object {stroke, radius, previews: all}` and acts on the picker's decision. **One**: selects it as a click would, `⇧` adding. **Several**: a numbered pin on each candidate's bounds; clicking a pin toggles it, Return or **Use N** sends `task.select-object` with the stroke and each chosen `candidate`, Escape cancels. **Nothing**: the painted footprint stays tinted under `Nothing found here · Remove the painted area ↵ · Cancel`; Return switches the kind to Remove with that stroke as its selection.
- **The selection** is up to 8 objects (a recorded default; the ninth is refused in the status bar). Each is tinted as Remove's selection is and numbered in the order picked; the tint is drawn from the pick's held preview at once and needs no read of the artifact. The group lists them as rows (`1 Object · ⌥-click to remove`) and offers **Paint shadow**, which arms the brush for one stroke sent as the fill's effect region (`effect-points`, `effect-radius`), drawn in the same tint with a dashed edge.
- **Generate** sends `task.fill-generative` with `object-artifacts` in place of a painted selection ([API](#api)). Everything after (progress, Cancel, the candidate draft in the photograph, Regenerate, Replace, Accept, the lock) is the delivered workflow.
- **Escape** cancels a running generation, then dismisses pins or the Nothing notice, then clears the selection, then leaves the mode.

The status bar names the gesture as the Corrections band does: `Corrections · Object · click to select the person · drag loosely to find an object · ⇧ adds · ⌥ takes away`. The object is named only when the picker names a class; today it says `the object`.

## Object in the Masks panel

The New mask and Add component menus gain a **Select** group whose first entry is **Object…**, listed whenever the run declares `task.select-object`. The later Select classes (Subject, Sky, People) join the same group when their models are qualified.

Choosing it enters a **picking sub-state of Mask mode**, as the range picker is: the open mask, its layer and the adjustment sections stay as they are, the cursor is a crosshair and the hover is Object's. A click or a loose drag picks as in Corrections (One at once, Several through pins), then publishes the artifact with `task.select-object` and commits one entry: `mask.create-model` for New mask, `mask.add-model` in the Add row's mode for Add component, labelled `Add object (model)`. The sub-state stays armed for further picks into the same mask until Escape or Done; the draft bar reads `New mask · Object · click an object · drag loosely to find one · Cancel esc`.

A `model` component's row (built here; Select reuses it) shows its class, how it was picked (`picked from a point`, `picked from a stroke`), when, and the model's part name and version. When `mask.list` reports it stale, the row says why in one line (`an object was removed after this was picked`) with **Recompute** and **Keep**. Recompute sends `task.select-object` with the component's stored prompt and `mask.set-model` as one entry; Keep sends nothing and hides the note until the reasons change. To make that possible an `object` component stores the prompt it was picked from ([API](#api)).

## Setting up

### Setups

A module declares **setups**: the resources one tool needs, grouped into parts named in the feature's words. A tool asks for its setup; the host works out what is missing.

| Setup | For | Parts (resources) | Download when nothing is installed |
| --- | --- | --- | --- |
| `remove` | Corrections › Remove | Generative fill (`generative-klein-4b`) | 2.5 GB |
| `object` | Corrections › Object | Object finder (`segmenter-l0-encoder`, `segmenter-l0-decoder`); Generative fill (`generative-klein-4b`) | 2.7 GB, or 133 MB with the fill installed |
| `mask-object` | Masks › Object | Object finder | 133 MB |
| `replace` | Replace, offered once `remove`'s part is installed | Replace (`generative-klein-text-encoder`) | 2.3 GB |

A developer run declares the same setups over its fake resources. A setup's machine minimum is the largest of its parts'. The setup's **introduction** (eyebrow, title, lead, steps or hero, promise) is declared on the setup, replacing the introduction on each download capability, because one sheet now introduces one tool however many files it fetches.

### The group's lines

Each tool's group shows its setup's state where Set up would be, as the delivered Remove setup does ([board 3](ai-tools-workspace/board.html)):

| State | Line | Action |
| --- | --- | --- |
| Status not read | Checking AI editing… | none |
| Nothing missing, not yet analysed (Object) | Finding objects… | none |
| Missing | The tool's sentence ("Point at an object and Luxforge removes it, filling the space to match the photo. It runs on this computer, so your photos stay on it."), then `One-time download · 2.7 GB` with only the missing bytes counted | **Set up…** |
| Partly downloaded | `Download paused at 1.1 of 2.7 GB` | **Resume** |
| Installing | `Setting up Object…`, one bar over the setup's missing bytes, then each part's state (`Object finder · Ready ✓`, `Generative fill · 34%`) | **Cancel**, which cancels every install job of the setup |
| Failed | `Setup didn't finish: …` naming the first failure, with each part's state | **Try again** (installs only what is still missing, resuming kept bytes) |
| Unavailable | `Object removal needs generative fill, which isn't available to download yet. It comes with a Luxforge update.` | none |
| Machine too small | `Object removal needs a computer with 16 GB of memory. This Mac has 8 GB.` | none |

Where a tool's fill cannot be set up but the object finder can, Object's group adds `You can still pick objects for masks in Mask mode.` New mask › Object without its setup opens the sheet directly rather than entering the sub-state.

### The sheet

Set up opens the delivered setup sheet ([AI editing](ai-editing.md#the-delivered-workflow), [board 4](ai-tools-workspace/board.html)), asked for the setup: the introduction's eyebrow, title, lead and three steps; then BEFORE IT CAN START with the three tiles summed over the missing parts (the download size and `Two parts, once, then offline`; every origin and the licences, `Free · Apache-2.0`; the setup's memory minimum against this machine's); then an **includes** list naming each part with its purpose and size, an installed part shown as `installed ✓` and not counted; then the delivered notes (a use restriction, a past decline, `You can remove it at any time in Settings › Models.`). **Download 2.7 GB ↵** grants every missing part's exact `download-artifact` scope and asks the host to install the setup; **Not now** denies each scope, as Don't allow does. The sheet closes on Download and the group shows the progress.

The Object setup's words: AI EDITING, **Remove an object by pointing at it**, "Hold the pointer over something you don't want, click, and Luxforge fills the space to match the rest of the photo. It runs entirely on this computer, so your photos never leave it."; steps **Point at it** (hover), **Click to select** (`⇧` adds, `⌥` takes away), **Generate and accept** (`Esc` cancels, `⌘Z` undoes). The mask Object setup: **Select an object by pointing at it** over the hover and selected pictures, 133 MB, no memory tile minimum. Remove and Replace keep their delivered words. The step pictures the desktop draws gain two kinds, `Hover` and `Selected`, beside the four it draws now.

### Not yet published

The removal conditioning, one file of `generative-klein-4b`, is published by the owner as a project release asset that does not exist yet ([AI editing](ai-editing.md#decisions), decision 17). Until it does, the module declares that file **pending publication**: the resource lists as `unavailable` with the reason `unpublished`, a setup with an unavailable missing part is `unavailable`, and no Set up is offered for it. A resource whose pending file is already staged at its pinned length (the delivered `generative_kit.py stage` route) is installable as today, so testers keep working. Publishing the asset changes the declaration to its pinned URL, and nothing else.

### Settings › Models

The tab lists models grouped by setup part rather than by resource: **Object finder** (the encoder and decoder as one row, one size, one Remove that removes both), **Generative fill**, **Replace**, each captioned with the tools that use it (`Corrections › Object, Masks › Object`). A part's row keeps the delivered contents: state, size, trade-off, use restriction, memory needed and while loaded. A part that is not installed offers **Download…**, which opens its tool's sheet for the smallest setup that holds it. The tab is listed in every run that serves a model; no experiment hides any row.

## Flags

`ai.generative` and `ai.analysis` are removed. The fill task, the analysis tasks, the picker and `task.select-object` declare no flag and their resources name none; nothing refuses for want of either. A stored value for either is kept and listed as unrecognized, as the flags rules require. `ai.generative-gpu-sharing` stays an experiment, off by default, listed always (it loses `requires`). On a CPU-only host (Linux today) Object is offered at the CPU provider's hover latency, which the prototype measured at a p95 of 45 ms against the 33 ms budget; the miss is published, not hidden.

## API

Proposed additions, each generated into `schema.list`; the methods not named here are delivered and unchanged.

| Method or field | Does |
| --- | --- |
| Module descriptor `setups: [{id, title, introduction, parts: [{name, purpose, resources}]}]` | At most 8 setups a module, 4 parts and 5 resources a setup, each resource one the module declares; validated with the descriptor and refused by name |
| `module.status` `setups: [{id, state: installed\|missing\|installing\|failed\|unavailable\|cannot-run, missing_bytes, staged_bytes, minimum_memory, fits, parts: [{name, state, bytes, staged_bytes, resources}], jobs, error?, reason?}]` | Derived from the resource states, the install memo and the transfer lane; opens nothing |
| `module.setup.install {module_id, setup, mutation}` | Queues one install job per missing resource, in declared order, on the transfer lane, after checking the lane has room for all of them (`resource-limit` otherwise, nothing queued); answers `{setup, jobs: [{resource_id, job_id}]}`, empty when nothing is missing. A missing grant fails `consent-required` with `data.consent: {module_id, setup, grants: [{capability, scope}], disclosure, denied}`, the disclosure summed over the missing parts with the setup's introduction and parts; the client grants each scope with `module.permission.grant` and asks again. An unavailable or too-large part fails `not-ready` naming it |
| Resource state `unavailable` with `reason: unpublished` | A declared file with no published URL yet and no staged copy |
| `task.fill-generative` `object-artifacts?: [artifact]` | 1 to 8 `mask-alpha` artifacts from `task.select-object`, instead of or beside `selection-points`. Each must be of this photograph's current source and repair prefix (`conflict` with `data.reason: "stale-selection"` naming it otherwise) and still published (`validation: unknown artifact`). The region joins each artifact's removal region (below) with any painted footprint by maximum. The candidates' provenance records each object's artifact and prompt |
| `model` payload `prompt?` | For class `object`: `{point}` or `{stroke, radius, candidate?}` in content coordinates, as `task.select-object` took it; at most 1,024 points. Recompute sends it back |

**The removal region of an object artifact** is AI editing's removal region over the artifact's alpha in place of the decoder's prior: the core where the bilinear alpha at a stage pixel is at least one half, a grayscale disc dilation of half an artifact texel plus 4% of the core's shorter side (at least 2 px, the dilation and feather capped as the region's rules cap them), and a soft outer edge one artifact texel wide; computed only over the artifact's non-zero window, with an `f64` reference and exact-buffer tests.

Parity: every gesture above sends a request `Copy as JSON request` shows (the ⋯ menu on the Object group copies the picks, the selects, Generate and Accept; a mask component's menu copies its pick, select and commit), and the acceptance drives the same journeys through `luxforge-json`.

## Performance rules checklist

- **Threads.** Hover stays on the query worker, analysis and `task.select-object` on the module lane, installs on the transfer lane; the desktop decodes pick previews off the update loop as now. Nothing new runs on the owner or interface thread.
- **Queues.** One pick in flight from the desktop, latest wins. Selects run one at a time from the desktop and further clicks wait in a bounded local queue of the selection limit (8); `⌥` removals send nothing. A setup checks the transfer lane's room before queueing.
- **Memory.** The segmenter holds 448 MiB resident beside the generative 5 GiB inside the 16 GiB host budget; idle unload and the memory signal apply. Up to 8 object artifacts of at most 16 MiB each are read by the fill job in bands on the module lane; the region is computed over each artifact's window only.
- **Cancellation.** Leaving Object or opening another photograph drops queued selects and pins; a running select finishes and its artifact is unreferenced (collected by retention). Setup Cancel cancels each job.
- **Measured.** Hover latency, analysis ready after entering Object, select-object time and the combined resident set, on the M4 with photo-sized inputs, once the feature is complete.

## Acceptance

- **Core, exact.** Setup declaration validation, status derivation for every state, install ordering, lane-room refusal, the combined consent, the unpublished state and its staged exception; the object removal region against its reference; a fill from object artifacts byte-identical outside its coverage; stale and unknown artifact refusals; the stored prompt and Recompute through `luxforge-json`.
- **Rendered.** Background-only smoke scenarios over the fake runtime with correlated state, logs and captures: `object-remove` (the sheet, one answer granting every scope, the aggregated bar, Finding objects, hover, click, `⇧`-click, `⌥`-click, a loose drag with pins, the Nothing hand-off to Remove, Paint shadow, Generate, Accept, Escape order); `mask-object` (the menu entry, the sheet, the sub-state, create and add, the stored row, a removal that stales it, Recompute and Keep); `pick`, `generative` and `settings` updated for the removed flags; `ai-setup` (unavailable, too small, failed and resumed).
- **Native, real models.** On the M4 with the pinned segmenter and klein: the Object journey on corpus photographs (the bridge, the kayaks, the road), hover and analysis latencies against their budgets, the set-up download over the network once the asset is published, the combined residency; recorded with the host's load.

## Decisions

Taken by the owner on 2026-10-24 and recorded in [decisions](../decisions.md#ai-tools-in-the-develop-workspace):

1. **Object detection is a separate tool, for removal and for masks**: Corrections › Object removes picked objects; Masks › New mask › Object makes a stored selection; one picker and one download serve both. Remove paints.
2. **Object removal ships on the Generative tier**; the Fast tier joins the shared Fill control of Remove and Object when its quality study passes.
3. **No experiment flag**: `ai.generative` and `ai.analysis` are removed and the tools are listed by default; downloads stay opt-in. Until the conditioning asset is published, setups that need it say so and offer nothing.
4. **Setup per tool, one sheet**: each tool asks for exactly what it is missing, the first time it is chosen, under one consent with the combined size; Replace stays an add-on.

Recorded defaults the owner can revise: the kind label **Object** and its line; a crosshair, not a brush, in Object; click replaces, `⇧` adds, `⌥` takes away; at most 8 objects a selection; the selection cleared when switching kinds; the Nothing hand-off to Remove; Paint shadow as one effect stroke; analysis on entering Object; the download glyph on an unset kind; Remove and Object hidden on a host with no runtime for any fill and shown with the reason on a machine too small; Object offered at CPU hover latency on a CPU-only host; mask Object's sub-state staying armed until Escape; the picture kinds `Hover` and `Selected`; the setup limits (8 setups, 4 parts, 5 resources); Settings › Models grouped by part.

Not decided here: the box gesture (`⌘`-drag) and labelled negative points, which the picker accepts and the desktop does not offer; a learned shadow region in place of Paint shadow; naming the picked object's class.
